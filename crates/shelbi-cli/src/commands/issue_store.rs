//! `shelbi issue-store <subcommand>` — operations on the issue-tracker backend
//! itself, as opposed to the individual issues on it (`shelbi issue` /
//! `shelbi task`).
//!
//! Today the only subcommand is `migrate`, which copies a project's whole board
//! from one backend to another (`file_system` ⇄ `github`), matched on the
//! stable `shelbi:id/<slug>` anchor so a re-run never duplicates a card. The
//! source is whichever backend the migration is *not* going to — the two live
//! backends are file_system and github, so `--to github` reads the on-disk
//! board and `--to file_system` reads GitHub issues. Both backends are built
//! from the project's `issue_tracker` config; the github side needs a
//! `github.repo` selector, which can sit staged in the config even while the
//! active backend is still file_system (see [`IssueTrackerConfig`]).

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use clap::{Subcommand, ValueEnum};
use shelbi_core::{Issue, IssueTrackerBackend, IssueTrackerConfig};
use shelbi_state::{
    apply_issue_migration, IssueMigrationPlan, IssueStore, MigrationControls, PartialMigration,
};

use super::require_project;

/// Default spacing between successive creates on a `--to github` run. GitHub's
/// secondary limit for content creation is 80/min and 500/hr; a create every
/// 7.5s is ~8/min and 480/hr, safely under both, so a bulk run stays out of the
/// limit rather than sprinting into it and recovering via backoff. `0` disables.
const DEFAULT_PACE_SECS: f64 = 7.5;

/// How many queued cards the default (non-`--dry-run`) plan preview lists before
/// collapsing the rest into a count — enough to sanity-check the plan without
/// the 588-line wall the full dump used to print up front.
const PLAN_PREVIEW: usize = 10;

#[derive(Debug, Subcommand)]
pub enum IssueStoreCmd {
    /// Migrate the project's issues from one backend to another, matched on the
    /// stable shelbi id so a re-run never duplicates an already-migrated card.
    Migrate {
        /// Which backend to migrate *into*. The source is the other live
        /// backend (`--to github` reads the on-disk board; `--to file_system`
        /// reads GitHub issues).
        #[arg(long = "to", value_enum)]
        to: MigrateTarget,
        /// Preview the plan without writing anything to the target.
        #[arg(long)]
        dry_run: bool,
        /// Seconds to wait between successive creates on a `--to github` run, to
        /// stay under GitHub's secondary content-creation limit (80/min, 500/hr).
        /// `0` disables pacing. Ignored for a `--to file_system` target.
        #[arg(long, default_value_t = DEFAULT_PACE_SECS)]
        pace_secs: f64,
    },
}

/// The target backend for `issue-store migrate`. Only the two live backends are
/// selectable; jira/linear are stubs with no store to write into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MigrateTarget {
    /// GitHub issues in the configured `github.repo`.
    #[value(name = "github")]
    Github,
    /// The markdown-on-disk board under `~/.shelbi/projects/<name>/tasks/`.
    #[value(name = "file_system", alias = "file-system")]
    FileSystem,
}

impl MigrateTarget {
    fn backend(self) -> IssueTrackerBackend {
        match self {
            MigrateTarget::Github => IssueTrackerBackend::Github,
            MigrateTarget::FileSystem => IssueTrackerBackend::FileSystem,
        }
    }

    /// The backend a migration *into* this target reads from. Only file_system
    /// and github are live, and migration runs strictly between the two.
    fn source_backend(self) -> IssueTrackerBackend {
        match self {
            MigrateTarget::Github => IssueTrackerBackend::FileSystem,
            MigrateTarget::FileSystem => IssueTrackerBackend::Github,
        }
    }
}

pub fn run(project_opt: Option<String>, cmd: IssueStoreCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        IssueStoreCmd::Migrate {
            to,
            dry_run,
            pace_secs,
        } => migrate(&project, to, dry_run, pace_secs),
    }
}

fn migrate(project: &str, to: MigrateTarget, dry_run: bool, pace_secs: f64) -> Result<()> {
    // A file_system-only project need not have a `project.yaml` on disk (the
    // board is created straight through `FileSystemStore`), so a missing config
    // defaults rather than erroring. A `--to github` run then fails on the
    // absent `github.repo` selector — the informative error, not "project not
    // found".
    let cfg = shelbi_state::load_project(project)
        .map(|p| p.issue_tracker)
        .unwrap_or_default();

    let source = resolve_backend(project, &cfg, to.source_backend())?;
    let target = resolve_backend(project, &cfg, to.backend())?;

    let plan = shelbi_state::plan_issue_migration(source.as_ref(), target.as_ref())
        .map_err(|e| anyhow!(e))?;

    let target_name = to.backend();
    let source_name = to.source_backend();
    report_plan_counts(&plan, source_name, target_name);

    if plan.is_empty() {
        println!("Nothing to migrate — {target_name} already holds every issue.");
        return Ok(());
    }

    if dry_run {
        // The full per-card plan is a --dry-run affordance only: on a 588-card
        // board the up-front dump was a wall of text the operator then waited
        // behind in silence. A real run summarizes instead (see below).
        report_plan_full(&plan);
        println!(
            "\nDry run — no changes written. Re-run without --dry-run to migrate {} issue(s).",
            plan.to_migrate.len()
        );
        return Ok(());
    }

    // A real run: show a short preview, not the whole board.
    report_plan_preview(&plan);

    // Only gate the write path on a matching daemon: planning is read-only, and
    // a dry run must never be blocked by an upgrade-in-progress.
    super::hub_version::ensure_daemon_matches_for_mutation()?;

    // Pacing only helps against a rate-limited remote; the on-disk board has no
    // limit, so a file_system target never sleeps.
    let pace = if target_name == IssueTrackerBackend::Github {
        Duration::from_secs_f64(pace_secs.max(0.0))
    } else {
        Duration::ZERO
    };
    let total = plan.to_migrate.len();
    if !pace.is_zero() {
        let est_min = (pace.as_secs_f64() * total as f64 / 60.0).ceil() as u64;
        println!(
            "\nPacing at {:.1}s between creates to stay under GitHub's secondary limit \
             (~{est_min} min for {total} issues). Ctrl-C is safe — re-run resumes where it stops.",
            pace.as_secs_f64()
        );
    }
    println!();

    // Stream a line per creation as it lands, with a running count and a live
    // rate/ETA, so a slow run never looks wedged.
    let start = Instant::now();
    let mut on_created = |i: usize, total: usize, issue: &Issue| {
        let elapsed = start.elapsed().as_secs_f64();
        let per_min = if elapsed > 0.0 {
            i as f64 / elapsed * 60.0
        } else {
            0.0
        };
        let eta = if per_min > 0.0 {
            fmt_duration_secs(((total - i) as f64 / per_min * 60.0) as u64)
        } else {
            "—".to_string()
        };
        println!(
            "[{i}/{total}] ✓ migrated {} → {target_name}  ({per_min:.1}/min, ETA {eta})",
            issue.id
        );
    };
    let sleep = |d: Duration| std::thread::sleep(d);
    let mut controls = MigrationControls {
        on_created: &mut on_created,
        pace,
        sleep: &sleep,
    };

    match apply_issue_migration(target.as_ref(), &plan, &mut controls) {
        Ok(created) => {
            println!(
                "\nMigrated {} issue(s) to {target_name} ({} already present, skipped).",
                created.len(),
                plan.already_present.len()
            );
            Ok(())
        }
        Err(PartialMigration { created, error }) => {
            // The created cards are durable on the target and were already
            // streamed above; report where it stopped, the error, and the
            // explicit resume step rather than discarding the progress.
            let done = created.len();
            let remaining = total - done;
            eprintln!("\nMigration stopped after creating {done} of {total} issue(s).");
            eprintln!("Error: {error}");
            eprintln!(
                "\nThose {done} issue(s) are already on {target_name} and durable; {remaining} \
                 remain. Re-run the same command to resume — migration matches on id, so it \
                 skips what exists and creates only the rest."
            );
            Err(anyhow!(
                "issue-store migrate aborted after {done}/{total} issue(s): {error}"
            ))
        }
    }
}

/// The one-line queued/skipped header a plan represents, before any write.
fn report_plan_counts(
    plan: &IssueMigrationPlan,
    source: IssueTrackerBackend,
    target: IssueTrackerBackend,
) {
    println!(
        "Migrating {} → {target}: {} to create, {} already present.",
        source,
        plan.to_migrate.len(),
        plan.already_present.len()
    );
}

/// A short preview of the queued cards for a real run — the first [`PLAN_PREVIEW`]
/// plus a count of the rest, so the operator can sanity-check without the full
/// board scrolling past.
fn report_plan_preview(plan: &IssueMigrationPlan) {
    for tf in plan.to_migrate.iter().take(PLAN_PREVIEW) {
        println!("  + {} [{}]", tf.task.id, tf.task.column.as_str());
    }
    let extra = plan.to_migrate.len().saturating_sub(PLAN_PREVIEW);
    if extra > 0 {
        println!("  … and {extra} more (use --dry-run to see the full plan).");
    }
}

/// The complete per-card plan — every queued and every already-migrated id. Only
/// printed under `--dry-run`.
fn report_plan_full(plan: &IssueMigrationPlan) {
    for tf in &plan.to_migrate {
        println!("  + {} [{}]", tf.task.id, tf.task.column.as_str());
    }
    for id in &plan.already_present {
        println!("  = {id} (already migrated)");
    }
}

/// Render a whole-second duration as a compact `Hh Mm Ss` / `Mm Ss` / `Ss`
/// string for the progress ETA.
fn fmt_duration_secs(total: u64) -> String {
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 {
        format!("{h}h {m}m")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

/// Build a live store for `backend` using the project's connection facts,
/// regardless of which backend is currently *active* in the config. Reuses the
/// same validation `resolve_issue_store` runs — a `github` target with no
/// `github.repo` selector fails here with a field-named error.
fn resolve_backend(
    project: &str,
    cfg: &IssueTrackerConfig,
    backend: IssueTrackerBackend,
) -> Result<Box<dyn IssueStore>> {
    if !matches!(
        backend,
        IssueTrackerBackend::FileSystem | IssueTrackerBackend::Github
    ) {
        bail!("issue-store migrate supports only `file_system` and `github` (got `{backend}`)");
    }
    let mut resolved = cfg.clone();
    resolved.backend = backend;
    shelbi_state::resolve_issue_store(project, &resolved).map_err(|e| anyhow!(e))
}
