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

use anyhow::{anyhow, bail, Result};
use clap::{Subcommand, ValueEnum};
use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig};
use shelbi_state::{IssueMigrationPlan, IssueStore};

use super::require_project;

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
        IssueStoreCmd::Migrate { to, dry_run } => migrate(&project, to, dry_run),
    }
}

fn migrate(project: &str, to: MigrateTarget, dry_run: bool) -> Result<()> {
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
    report_plan(&plan, source_name, target_name);

    if plan.is_empty() {
        println!("Nothing to migrate — {target_name} already holds every issue.");
        return Ok(());
    }

    if dry_run {
        println!(
            "\nDry run — no changes written. Re-run without --dry-run to migrate {} issue(s).",
            plan.to_migrate.len()
        );
        return Ok(());
    }

    // Only gate the write path on a matching daemon: planning is read-only, and
    // a dry run must never be blocked by an upgrade-in-progress.
    super::hub_version::ensure_daemon_matches_for_mutation()?;

    let created = shelbi_state::apply_issue_migration(target.as_ref(), &plan)
        .map_err(|e| anyhow!(e))?;
    for issue in &created {
        println!("✓ migrated {} → {target_name}", issue.id);
    }
    println!(
        "\nMigrated {} issue(s) to {target_name} ({} already present, skipped).",
        created.len(),
        plan.already_present.len()
    );
    Ok(())
}

/// Print the queued/skipped breakdown a plan represents, before any write.
fn report_plan(
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
    for tf in &plan.to_migrate {
        println!("  + {} [{}]", tf.task.id, tf.task.column.as_str());
    }
    for id in &plan.already_present {
        println!("  = {id} (already migrated)");
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
