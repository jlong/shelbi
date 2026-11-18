//! `shelbi issue-store <subcommand>` — operations on the issue-tracker backend
//! itself, as opposed to the individual issues on it (`shelbi issue` /
//! `shelbi task`).
//!
//! Today the only subcommand is `migrate`, which copies a project's whole board
//! from one backend to another (`file_system`, `github`, `linear`, in any
//! direction), matched on the stable shelbi id so a re-run never duplicates a
//! card. It **previews by default**; `--apply` writes.
//!
//! The source is `--from`, else the project's configured backend when that
//! isn't the target, else its staged connection block's backend, else
//! `file_system`. Each side's store is built from the project's
//! `issue_tracker` config, and a project keeps at most one external
//! connection block (see [`IssueTrackerConfig::validate`]), so the side the
//! config doesn't describe comes from flags: `--github-repo`, or `--linear-team`
//! (plus `--linear-project` / `--linear-label` / `--linear-assignee`). A
//! `file_system` project can also keep the target's block staged.
//!
//! A Linear target whose workflow states lack a source status gets the card in
//! the team's first status of the same category; the plan lists each remap.
//! Comments are copied with every card the run creates.

use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Result};
use clap::{Args, Subcommand, ValueEnum};
use shelbi_core::{
    Column, GithubConnection, Issue, IssueTrackerBackend, IssueTrackerConfig, LinearConnection,
    StatusCategory,
};
use shelbi_state::{
    apply_issue_migration, IssueMigrationPlan, IssueStore, MigrationControls, PartialMigration,
};

use super::require_project;

/// Default spacing between successive writes on a `--to github` run. GitHub's
/// secondary limit for content creation is 80/min and 500/hr; a write every
/// 7.5s is ~8/min and 480/hr, safely under both, so a bulk run stays out of the
/// limit rather than sprinting into it and recovering via backoff.
const DEFAULT_GITHUB_PACE_SECS: f64 = 7.5;

/// Default spacing between successive writes on a `--to linear` run: 3s is
/// 1,200 writes an hour, inside Linear's hourly request budget for an API key
/// with room left for the board's own reads.
const DEFAULT_LINEAR_PACE_SECS: f64 = 3.0;

/// How many queued cards the default plan preview lists before collapsing the
/// rest into a count — enough to sanity-check the plan without the 588-line
/// wall the full dump used to print up front.
const PLAN_PREVIEW: usize = 10;

#[derive(Debug, Subcommand)]
pub enum IssueStoreCmd {
    /// Migrate the project's issues from one backend to another, matched on the
    /// stable shelbi id so a re-run never duplicates an already-migrated card.
    /// Previews the plan unless `--apply` is given.
    Migrate(MigrateArgs),
}

/// `shelbi issue-store migrate` arguments.
#[derive(Debug, Args)]
pub struct MigrateArgs {
    /// Which backend to migrate *into*.
    #[arg(long = "to", value_enum)]
    pub to: MigrateTarget,
    /// Which backend to migrate *from*. Default: the project's configured
    /// backend when it isn't the target, else its staged connection block's
    /// backend, else `file_system`.
    #[arg(long = "from", value_enum)]
    pub from: Option<MigrateTarget>,
    /// Write to the target. Without it the command only prints the plan.
    #[arg(long, conflicts_with = "dry_run")]
    pub apply: bool,
    /// Preview the plan without writing anything (the default; kept so
    /// existing scripts keep working).
    #[arg(long)]
    pub dry_run: bool,
    /// Seconds to wait between successive writes, to stay under the target's
    /// write limits. Default: 7.5 for github (its secondary limit of 80/min,
    /// 500/hr), 3 for linear, none for file_system. `0` disables pacing.
    #[arg(long)]
    pub pace_secs: Option<f64>,
    /// The GitHub `owner/repo` for the github side, when the project's config
    /// doesn't name it.
    #[arg(long, value_name = "OWNER/REPO")]
    pub github_repo: Option<String>,
    /// The Linear team (key, name or id) for the linear side, when the
    /// project's config doesn't name it.
    #[arg(long, value_name = "TEAM")]
    pub linear_team: Option<String>,
    /// With `--linear-team`: the Linear project filter.
    #[arg(long, value_name = "PROJECT", requires = "linear_team")]
    pub linear_project: Option<String>,
    /// With `--linear-team`: the Linear label filter.
    #[arg(long, value_name = "LABEL", requires = "linear_team")]
    pub linear_label: Option<String>,
    /// With `--linear-team`: the Linear assignee filter (`me`, an email, or a
    /// user id).
    #[arg(long, value_name = "WHO", requires = "linear_team")]
    pub linear_assignee: Option<String>,
}

/// A backend `issue-store migrate` reads from or writes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum MigrateTarget {
    /// GitHub issues in the `github.repo` (or `--github-repo`).
    #[value(name = "github")]
    Github,
    /// Linear issues in the `linear.team` (or `--linear-team`), narrowed by
    /// its filters.
    #[value(name = "linear")]
    Linear,
    /// The markdown-on-disk board under `~/.shelbi/projects/<name>/tasks/`.
    #[value(name = "file_system", alias = "file-system")]
    FileSystem,
}

impl MigrateTarget {
    fn backend(self) -> IssueTrackerBackend {
        match self {
            MigrateTarget::Github => IssueTrackerBackend::Github,
            MigrateTarget::Linear => IssueTrackerBackend::Linear,
            MigrateTarget::FileSystem => IssueTrackerBackend::FileSystem,
        }
    }
}

pub fn run(project_opt: Option<String>, cmd: IssueStoreCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        IssueStoreCmd::Migrate(args) => migrate(&project, &args),
    }
}

/// The source backend for a migration into `to` (see the module docs).
fn source_backend(
    cfg: &IssueTrackerConfig,
    to: IssueTrackerBackend,
    from: Option<IssueTrackerBackend>,
) -> Result<IssueTrackerBackend> {
    let staged = cfg.connection_blocks().first().map(|b| match *b {
        "github" => IssueTrackerBackend::Github,
        "jira" => IssueTrackerBackend::Jira,
        _ => IssueTrackerBackend::Linear,
    });
    let source = from
        .or_else(|| Some(cfg.backend).filter(|b| *b != to))
        .or_else(|| staged.filter(|b| *b != to))
        .or_else(|| Some(IssueTrackerBackend::FileSystem).filter(|b| *b != to));
    match source {
        Some(s) if s == to => bail!("--from and --to are both `{to}`; pick two different backends"),
        Some(s) => Ok(s),
        None => bail!("nothing to migrate from: pass --from (file_system, github or linear)"),
    }
}

/// The pace for a run into `target`: `--pace-secs`, else the target's default.
fn pace_for(target: IssueTrackerBackend, pace_secs: Option<f64>) -> Duration {
    let default = match target {
        IssueTrackerBackend::Github => DEFAULT_GITHUB_PACE_SECS,
        IssueTrackerBackend::Linear => DEFAULT_LINEAR_PACE_SECS,
        _ => 0.0,
    };
    Duration::from_secs_f64(pace_secs.unwrap_or(default).max(0.0))
}

fn migrate(project: &str, args: &MigrateArgs) -> Result<()> {
    // A file_system-only project need not have a `project.yaml` on disk (the
    // board is created straight through `FileSystemStore`), so a missing config
    // defaults rather than erroring. A `--to github` run then fails on the
    // absent `github.repo` selector — the informative error, not "project not
    // found".
    let cfg = shelbi_state::load_project(project)
        .map(|p| p.issue_tracker)
        .unwrap_or_default();

    let target_name = args.to.backend();
    let source_name = source_backend(&cfg, target_name, args.from.map(MigrateTarget::backend))?;
    let source = resolve_backend(project, &cfg, source_name, args)?;
    let target = resolve_backend(project, &cfg, target_name, args)?;

    // A Linear target has its own statuses (the team's workflow states): map
    // each source status the team lacks onto one of the same category.
    let targets = match &target {
        Side::Linear(store) => Some(store.status_columns().map_err(|e| anyhow!(e))?),
        Side::Other(_) => None,
    };
    let catalog = shelbi_state::load_project_statuses(project).ok();
    let source_category = |col: &Column| -> StatusCategory {
        catalog
            .as_ref()
            .and_then(|c| c.get(col.as_str()))
            .map(|s| s.category)
            .unwrap_or_else(|| col.category())
    };
    let plan = shelbi_state::plan_issue_migration_mapped(
        source.store(),
        target.store(),
        targets.as_deref(),
        &source_category,
    )
    .map_err(|e| anyhow!(e))?;

    report_plan_counts(&plan, source_name, target_name);
    report_remaps(&plan);

    if plan.is_empty() {
        println!("Nothing to migrate — {target_name} already holds every issue.");
        return Ok(());
    }

    if !args.apply {
        // The full per-card plan is the preview's job: on a 588-card board an
        // up-front dump on a real run was a wall of text the operator then
        // waited behind in silence. A real run summarizes instead (see below).
        report_plan_full(&plan);
        println!(
            "\nDry run — no changes written. Re-run with --apply to migrate {} issue(s) \
             (and their comments).",
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
    // limit, so a file_system target never sleeps by default.
    let pace = pace_for(target_name, args.pace_secs);
    let total = plan.to_migrate.len();
    if !pace.is_zero() {
        let est_min = (pace.as_secs_f64() * total as f64 / 60.0).ceil() as u64;
        println!(
            "\nPacing at {:.1}s between writes to stay under {target_name}'s write limits \
             (~{est_min} min for {total} issues, plus their comments). Ctrl-C is safe — re-run \
             resumes where it stops.",
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
        comments_from: Some(source.store()),
        comments_copied: 0,
    };

    let result = apply_issue_migration(target.store(), &plan, &mut controls);
    let comments = controls.comments_copied;
    match result {
        Ok(created) => {
            println!(
                "\nMigrated {} issue(s) and {comments} comment(s) to {target_name} ({} already \
                 present, skipped).",
                created.len(),
                plan.already_present.len()
            );
            if cfg.backend != target_name {
                println!("{}", switch_hint(target_name));
            }
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

/// How to point the project at the board it was just migrated to.
fn switch_hint(target: IssueTrackerBackend) -> String {
    let how = match target {
        IssueTrackerBackend::Linear => {
            "run `shelbi tracker linear connect --team <TEAM> --force` (the old board still has \
             its issues, so the switch needs --force)"
        }
        IssueTrackerBackend::Github => {
            "set `issue_tracker.backend: github` with its `github:` block, or connect it in Settings"
        }
        _ => "set `issue_tracker.backend: file_system`",
    };
    format!("The project still reads its old board; to switch it, {how}.")
}

/// The cards written in another status than their source's.
fn report_remaps(plan: &IssueMigrationPlan) {
    if plan.remapped.is_empty() {
        return;
    }
    println!(
        "{} card(s) go into a different status (the target has no status of the same id; \
         mapped by category):",
        plan.remapped.len()
    );
    for r in &plan.remapped {
        println!("  ~ {}: {} → {}", r.id, r.from.as_str(), r.to.as_str());
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

/// One side of a migration: a Linear store (whose statuses the plan maps
/// onto), or any other backend's store.
enum Side {
    Linear(Box<shelbi_state::LinearStore>),
    Other(Box<dyn IssueStore>),
}

impl Side {
    fn store(&self) -> &dyn IssueStore {
        match self {
            Side::Linear(s) => s.as_ref(),
            Side::Other(s) => s.as_ref(),
        }
    }
}

/// Build a live store for `backend` from the project's connection facts (or
/// the `--github-repo` / `--linear-*` flags), regardless of which backend is
/// currently *active* in the config. Reuses the same validation
/// `resolve_issue_store` runs — a `github` side with no repo, or a `linear`
/// side with no team, fails here with a field-named error.
fn resolve_backend(
    project: &str,
    cfg: &IssueTrackerConfig,
    backend: IssueTrackerBackend,
    args: &MigrateArgs,
) -> Result<Side> {
    // Only this side's connection block, so the one-tracker rule holds.
    let mut resolved = IssueTrackerConfig {
        backend,
        refresh_secs: cfg.refresh_secs,
        budget: cfg.budget.clone(),
        ..Default::default()
    };
    match backend {
        IssueTrackerBackend::FileSystem => {}
        IssueTrackerBackend::Github => {
            resolved.github = match &args.github_repo {
                Some(repo) => Some(GithubConnection { repo: repo.clone() }),
                None => cfg.github.clone(),
            };
        }
        IssueTrackerBackend::Linear => {
            resolved.linear = match &args.linear_team {
                Some(team) => Some(LinearConnection {
                    team: team.clone(),
                    project: args.linear_project.clone(),
                    label: args.linear_label.clone(),
                    assignee: args.linear_assignee.clone(),
                }),
                None => cfg.linear.clone(),
            };
            resolved.validate().map_err(|e| {
                anyhow!("{e}; pass --linear-team when the project's config has no `linear:` block")
            })?;
            let conn = resolved.linear.clone().unwrap_or_default();
            return Ok(Side::Linear(Box::new(shelbi_state::LinearStore::new(project, conn))));
        }
        other => bail!("issue-store migrate supports `file_system`, `github` and `linear` (got `{other}`)"),
    }
    shelbi_state::resolve_issue_store(project, &resolved)
        .map(Side::Other)
        .map_err(|e| anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;
    use shelbi_state::linear_client::FakeLinear;

    fn args(to: MigrateTarget) -> MigrateArgs {
        MigrateArgs {
            to,
            from: None,
            apply: false,
            dry_run: false,
            pace_secs: None,
            github_repo: None,
            linear_team: None,
            linear_project: None,
            linear_label: None,
            linear_assignee: None,
        }
    }

    #[test]
    fn the_source_defaults_to_the_backend_that_isnt_the_target() {
        use IssueTrackerBackend::*;
        let staged_github = IssueTrackerConfig {
            github: Some(GithubConnection { repo: "o/r".into() }),
            ..Default::default()
        };
        // The two original directions keep their sources.
        assert_eq!(source_backend(&staged_github, Github, None).unwrap(), FileSystem);
        assert_eq!(source_backend(&staged_github, FileSystem, None).unwrap(), Github);
        // A GitHub project migrating to Linear reads GitHub; a Linear one
        // migrating home reads Linear.
        let github = IssueTrackerConfig { backend: Github, ..staged_github.clone() };
        assert_eq!(source_backend(&github, Linear, None).unwrap(), Github);
        let linear = IssueTrackerConfig {
            backend: Linear,
            linear: Some(LinearConnection::for_team("ENG")),
            ..Default::default()
        };
        assert_eq!(source_backend(&linear, FileSystem, None).unwrap(), Linear);
        assert_eq!(source_backend(&linear, Linear, None).unwrap(), FileSystem);
        // --from wins; the same backend on both sides is refused.
        assert_eq!(source_backend(&linear, Github, Some(FileSystem)).unwrap(), FileSystem);
        assert!(source_backend(&linear, Linear, Some(Linear)).is_err());
        // Pacing defaults per target.
        assert_eq!(pace_for(Github, None), Duration::from_secs_f64(7.5));
        assert_eq!(pace_for(Linear, None), Duration::from_secs(3));
        assert_eq!(pace_for(FileSystem, None), Duration::ZERO);
        assert_eq!(pace_for(Linear, Some(0.0)), Duration::ZERO);
    }

    #[test]
    fn a_migration_to_linear_previews_by_default_and_needs_a_team() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-migrate-linear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(home.join("projects/p")).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::linear_store::clear_linear_caches_for_test();
        let fs = shelbi_state::FileSystemStore::new("p");
        fs.add(shelbi_state::NewIssue::new("a", "A", Column::review(), "body")).unwrap();
        let fake = FakeLinear::new();
        let _guard = fake.install();

        // No `linear:` block and no --linear-team: a field-named error.
        let err = migrate("p", &args(MigrateTarget::Linear)).unwrap_err().to_string();
        assert!(err.contains("issue_tracker.linear.team") && err.contains("--linear-team"), "{err}");

        // With the team: the plan is read (states mapped), nothing written.
        let mut a = args(MigrateTarget::Linear);
        a.linear_team = Some("ENG".into());
        migrate("p", &a).unwrap();
        assert!(!fake.calls_for("TeamStates").is_empty(), "the team's statuses were read");
        assert!(fake.calls_for("IssueCreate").is_empty(), "a preview never writes");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
