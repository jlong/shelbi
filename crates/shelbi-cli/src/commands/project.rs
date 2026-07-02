//! `shelbi project <subcommand>` — manage projects.
//!
//! * `add` — walk the same project-setup prompt sequence as initial
//!   onboarding without the idempotence guard.
//! * `migrate-to-in-repo` — one-way migration of a project's config
//!   from `~/.shelbi/projects/<name>.yaml` into
//!   `<repo>/.shelbi/project.yaml` + `~/.shelbi/projects/<name>/local.yaml`.

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use inquire::Confirm;

use shelbi_state::{
    append_gitignore_snippet, apply_migration_plan, gitignore_already_has_snippet,
    plan_in_repo_migration, MigrationAction, MigrationPlan,
};

#[derive(Debug, Subcommand)]
pub enum ProjectCmd {
    /// Set up a new project interactively. Walks through the same prompt
    /// sequence as initial onboarding (project name, machines, workspaces,
    /// runners) and writes `~/.shelbi/projects/<name>.yaml`. Does not
    /// launch the TUI on completion.
    Add,

    /// Migrate an existing global-mode project into in-repo mode.
    ///
    /// Splits `~/.shelbi/projects/<name>.yaml` into a committed shared
    /// half at `<repo>/.shelbi/project.yaml` and a per-machine
    /// `~/.shelbi/projects/<name>/local.yaml`; moves `workflows/`,
    /// `agents/`, and the workspace-settings template from the state
    /// dir to the repo. State (`state.json`, `tasks/`, `HANDOFF.md`,
    /// `.claude/`, `workspaces/`, `events.log`) stays under
    /// `~/.shelbi/` in both modes.
    ///
    /// Idempotent — safe to re-run on an already-migrated project
    /// (no-op) or a half-migrated one (completes the missing steps).
    /// Prints a `.gitignore` snippet at the repo root and, in
    /// non-dry-run mode, offers to auto-append it.
    ///
    /// This is a ONE-WAY migration. There is no `migrate-to-global`
    /// command; reverting is `git revert` on the migration commit
    /// (which restores `<repo>/.shelbi/` to its pre-migration state)
    /// plus manually moving `local.yaml` back to
    /// `~/.shelbi/projects/<name>.yaml`. Bail out here if you're not
    /// sure — a merged migration commit is expensive to undo.
    ///
    /// Docs: `site/content/docs/concepts/config-modes.mdx` — full
    /// on-disk layout, `.gitignore` list, and worked pick-up example.
    #[command(name = "migrate-to-in-repo")]
    MigrateToInRepo {
        /// Project to migrate. Defaults to the project resolved from
        /// `$SHELBI_PROJECT` or by walking up from the current
        /// directory against registered project work_dirs.
        #[arg(long, short = 'p', value_name = "NAME")]
        project: Option<String>,
        /// Print the plan without touching disk. Emits every write /
        /// move / delete the non-dry-run form would perform, in order,
        /// so a diff-oriented reviewer can vet the migration before it
        /// runs.
        #[arg(long)]
        dry_run: bool,
        /// Skip the interactive prompt and auto-append the `.gitignore`
        /// snippet at the repo root. Useful for scripts and headless
        /// runs where no TTY is attached. Ignored under `--dry-run`
        /// (which never writes anything).
        #[arg(long)]
        yes: bool,
    },
}

pub fn run(cli_project: Option<String>, cmd: ProjectCmd) -> Result<()> {
    match cmd {
        ProjectCmd::Add => crate::wizard::setup_one_project(),
        ProjectCmd::MigrateToInRepo {
            project,
            dry_run,
            yes,
        } => {
            let name = super::require_project(project.or(cli_project))?;
            run_migrate_to_in_repo(&name, dry_run, yes)
        }
    }
}

/// Drive the migration for `project_name`. In dry-run mode, prints the
/// plan and returns. Otherwise applies the plan and handles the
/// `.gitignore` prompt.
fn run_migrate_to_in_repo(project_name: &str, dry_run: bool, yes: bool) -> Result<()> {
    let plan = plan_in_repo_migration(project_name).map_err(|e| anyhow!(e))?;
    print_plan_header(&plan, dry_run);
    for warning in &plan.warnings {
        println!("  ⚠ {warning}");
    }

    if plan.is_noop() {
        if plan.already_in_repo {
            println!("Nothing to do — project `{project_name}` is already in in-repo mode.");
        } else {
            // Empty plan but not `already_in_repo` is a defensive
            // fallback: nothing to migrate, nothing to heal.
            println!("Nothing to do for project `{project_name}`.");
        }
        // Still surface the gitignore snippet: a user who ran migrate
        // and never wired up `.gitignore` might come back to fix that
        // alone. Dry-run mode already prints below.
        maybe_offer_gitignore(&plan, dry_run, yes)?;
        return Ok(());
    }

    print_actions(&plan.actions);

    if dry_run {
        println!("\n(dry-run — no files were written)");
        print_gitignore_snippet(&plan, true)?;
        return Ok(());
    }

    let n = apply_migration_plan(&plan).map_err(|e| anyhow!(e))?;
    println!("\n✓ applied {n} action{}", if n == 1 { "" } else { "s" });
    println!("  shared config: {}", plan.shared_yaml_path.display());
    println!("  local config:  {}", plan.local_yaml_path.display());

    maybe_offer_gitignore(&plan, false, yes)?;
    Ok(())
}

fn print_plan_header(plan: &MigrationPlan, dry_run: bool) {
    let mode = if dry_run { " (dry-run)" } else { "" };
    println!(
        "Migrating project `{}` to in-repo mode{mode}",
        plan.project_name
    );
    println!("  repo:        {}", plan.repo_root.display());
    println!("  state dir:   {}", plan.state_root.display());
    println!("  in-repo dir: {}", plan.in_repo_config_root.display());
}

fn print_actions(actions: &[MigrationAction]) {
    println!("\nPlan:");
    for action in actions {
        println!("  - {}", format_action(action));
    }
}

fn format_action(action: &MigrationAction) -> String {
    match action {
        MigrationAction::WriteSharedYaml { path, .. } => {
            format!("write shared config → {}", path.display())
        }
        MigrationAction::WriteLocalYaml { path, .. } => {
            format!("write local config  → {}", path.display())
        }
        MigrationAction::MoveConfigDir { src, dst } => {
            format!("move dir  {} → {}", src.display(), dst.display())
        }
        MigrationAction::MoveConfigFile { src, dst } => {
            format!("move file {} → {}", src.display(), dst.display())
        }
        MigrationAction::DeleteGlobalYaml { path } => {
            format!("delete    {}", path.display())
        }
    }
}

/// Decide whether to print/append the `.gitignore` snippet, and (in
/// non-dry-run mode) prompt the user for auto-append. Idempotent — a
/// `.gitignore` that already carries the snippet triggers the "already
/// present" line and no prompt.
///
/// `--yes` skips the prompt and auto-appends. Same effect if stdin
/// isn't a TTY (typical in scripts / CI) — the snippet is still printed
/// so the user has a copyable record, but we don't try to prompt.
fn maybe_offer_gitignore(plan: &MigrationPlan, dry_run: bool, yes: bool) -> Result<()> {
    let already = gitignore_already_has_snippet(&plan.gitignore_path, plan.gitignore_snippet)
        .map_err(|e| anyhow!(e))?;
    if already {
        println!(
            "\n.gitignore at {} already carries the shelbi state entries.",
            plan.gitignore_path.display()
        );
        return Ok(());
    }
    print_gitignore_snippet(plan, dry_run)?;
    if dry_run {
        return Ok(());
    }
    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
    let should_append = if yes {
        true
    } else if !is_tty {
        // No TTY and no `--yes` — err on the side of not touching the
        // repo file; the snippet was printed above so the user can add
        // it manually.
        println!(
            "\nstdin is not a TTY — skipping `.gitignore` prompt. \
             Re-run with `--yes` to auto-append."
        );
        return Ok(());
    } else {
        Confirm::new(&format!(
            "Append this snippet to {}?",
            plan.gitignore_path.display()
        ))
        .with_default(true)
        .prompt()
        .with_context(|| "confirm prompt `Append .gitignore snippet?`")?
    };
    if should_append {
        append_gitignore_snippet(&plan.gitignore_path, plan.gitignore_snippet)
            .map_err(|e| anyhow!(e))?;
        println!(
            "✓ appended shelbi state entries to {}",
            plan.gitignore_path.display()
        );
    } else {
        println!(
            "Skipped — copy the snippet above into {} manually before committing.",
            plan.gitignore_path.display()
        );
    }
    Ok(())
}

fn print_gitignore_snippet(plan: &MigrationPlan, dry_run: bool) -> Result<()> {
    let verb = if dry_run {
        "Would suggest"
    } else {
        "Suggested"
    };
    println!(
        "\n{verb} `.gitignore` snippet for {}:",
        plan.gitignore_path.display()
    );
    for line in plan.gitignore_snippet.lines() {
        println!("  {line}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    /// Minimal parent so clap has a subcommand to parse. Mirrors the
    /// shape of the real `Cli` in main.rs — we only need the
    /// `project` arm.
    #[derive(Debug, Parser)]
    #[command(name = "shelbi", no_binary_name = false)]
    struct TestCli {
        #[command(subcommand)]
        cmd: TestCmd,
    }

    #[derive(Debug, clap::Subcommand)]
    enum TestCmd {
        Project {
            #[command(subcommand)]
            cmd: ProjectCmd,
        },
    }

    /// `migrate-to-in-repo` parses with the expected defaults (no
    /// `--project`, no `--dry-run`, no `--yes`).
    #[test]
    fn migrate_subcommand_parses_with_defaults() {
        let cli = TestCli::parse_from(["shelbi", "project", "migrate-to-in-repo"]);
        match cli.cmd {
            TestCmd::Project {
                cmd:
                    ProjectCmd::MigrateToInRepo {
                        project,
                        dry_run,
                        yes,
                    },
            } => {
                assert!(project.is_none());
                assert!(!dry_run);
                assert!(!yes);
            }
            _ => panic!("expected MigrateToInRepo, got {:?}", cli.cmd),
        }
    }

    /// All three flags parse together — the shape scripts will exercise.
    #[test]
    fn migrate_subcommand_parses_with_all_flags() {
        let cli = TestCli::parse_from([
            "shelbi",
            "project",
            "migrate-to-in-repo",
            "--project",
            "sample",
            "--dry-run",
            "--yes",
        ]);
        match cli.cmd {
            TestCmd::Project {
                cmd:
                    ProjectCmd::MigrateToInRepo {
                        project,
                        dry_run,
                        yes,
                    },
            } => {
                assert_eq!(project.as_deref(), Some("sample"));
                assert!(dry_run);
                assert!(yes);
            }
            _ => panic!("expected MigrateToInRepo, got {:?}", cli.cmd),
        }
    }

    /// `format_action` produces stable, greppable prefixes for each
    /// action variant so dry-run output can be scanned in tests or by
    /// eye without ambiguity.
    #[test]
    fn format_action_prefixes_are_stable() {
        use std::path::PathBuf;
        let cases = [
            (
                MigrationAction::WriteSharedYaml {
                    path: PathBuf::from("/a"),
                    body: String::new(),
                },
                "write shared config",
            ),
            (
                MigrationAction::WriteLocalYaml {
                    path: PathBuf::from("/a"),
                    body: String::new(),
                },
                "write local config",
            ),
            (
                MigrationAction::MoveConfigDir {
                    src: PathBuf::from("/a"),
                    dst: PathBuf::from("/b"),
                },
                "move dir",
            ),
            (
                MigrationAction::MoveConfigFile {
                    src: PathBuf::from("/a"),
                    dst: PathBuf::from("/b"),
                },
                "move file",
            ),
            (
                MigrationAction::DeleteGlobalYaml {
                    path: PathBuf::from("/a"),
                },
                "delete",
            ),
        ];
        for (action, expected_prefix) in cases {
            let rendered = format_action(&action);
            assert!(
                rendered.starts_with(expected_prefix),
                "action {action:?} rendered as `{rendered}` — expected prefix `{expected_prefix}`",
            );
        }
    }
}
