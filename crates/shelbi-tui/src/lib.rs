//! shelbi's top-level entry point:
//!
//! - `run_main(project)` — bring up the project's orchestrator session and
//!   run the single-process ratatui shell that owns the whole screen and
//!   shows worker sessions through shelbi-term / shelbi-client. This is what
//!   `shelbi` (no subcommand) invokes.

use anyhow::Result;

mod activity;
mod app;
mod error_report;
mod handlers;
mod kanban;
mod keymap;
mod machines;
mod reachability;
mod layout_sub;
mod markdown;
pub mod overlay;
mod panel;
mod review_panel;
mod shell;
mod sidebar;
mod workspace_panel;
pub mod theme;

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Serializes any test that mutates the process-global `SHELBI_HOME`
    /// env var. Tests across modules share one binary (and thus one env),
    /// so they must all lock the *same* mutex or they race each other.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Provision a real git repo + project YAML at `<home>/projects/<name>.yaml`
    /// pointing the hub machine at the repo. The kanban TUI's
    /// `move_card` now runs the depends_on-aware branch cut via
    /// `shelbi_orchestrator::lifecycle` when a task lands in
    /// `in_progress`; that hook needs a loadable project YAML and a
    /// real git repo at the hub workdir. Tests reach for this helper to
    /// produce both.
    ///
    /// Caller must hold `ENV_LOCK` and have already pointed
    /// `SHELBI_HOME` at `home`. Returns the repo path so the test can
    /// drive further git operations against it.
    pub fn provision_hub_repo_for_project(home: &Path, project_name: &str) -> PathBuf {
        use shelbi_core::{
            AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
            Project, ZenConfig,
        };
        use std::collections::BTreeMap;
        use std::process::Command;

        let repo = home.join(format!("{project_name}-repo"));
        std::fs::create_dir_all(&repo).unwrap();

        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed in {}", repo.display());
        };
        run(&["init", "-q", "-b", "main", "."]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);

        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        let project = Project { session: Default::default(),
            name: project_name.into(),
            label: None,
            display_name: None,
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.clone(),
                host: None,
                tags: Vec::new(),
                forward: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            github_reconcile_interval_secs: 900,
            workspace_permissions_mode: Some("auto".into()),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            issue_tracker: Default::default(),
            disk: shelbi_core::DiskConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
            review: shelbi_core::ReviewConfig::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        repo
    }
}

pub use activity::ActivityApp;
pub use app::{App, Row, View, WorkspaceBadge, WorkspaceOverview};
pub use kanban::KanbanApp;
pub use machines::MachinesApp;
pub use sidebar::decoration_to_color;
// The poller moved out of this crate into `shelbi-orchestrator` (Phase 3,
// `rt-daemon-poller`) so it can run either here (the sidebar) or in
// `shelbi daemon`. Re-exported at the old path so existing callers don't churn.
pub use shelbi_orchestrator::poller::WorkspacePoller;

/// Bring up the project's orchestrator session and run the single-process
/// ratatui shell.
pub fn run_main(project_name: &str) -> Result<()> {
    // Cutover gate (`rt-cutover-migration`): refuse to open a project whose
    // legacy `shelbi-<p>` / `_shelbi-<p>` tmux session is still running from the
    // previous runtime — opening beside it would run two pollers at once and
    // could start a second agent in the hub worktree. A no-op when tmux is not
    // installed / no such session exists. Checked first, before we touch the
    // daemon or bootstrap the orchestrator session.
    shelbi_orchestrator::migration::ensure_project_openable(project_name)?;

    // Bump the recently-used timestamp before bootstrapping the session so the
    // picker's recency sort reflects this launch. Best-effort — a
    // missing/unwritable ~/.shelbi/shelbi.yaml should not block launching.
    let _ = shelbi_state::touch_project_launched(project_name);

    // Cutover migration pass (`rt-cutover-migration`): now that the open gate
    // proved the local tmux session gone, record each workspace's migration
    // state so dispatch knows which worktrees are proven idle. Local workspaces
    // migrate; a remote stays pending until the hub reaches its machine and
    // confirms (killing a surviving `shelbi-w-<ws>` only with the user's
    // agreement). Best-effort — a pending workspace doesn't block opening;
    // dispatch to it is what's refused. Runs here, before the shell takes over
    // the screen, because its consent prompt needs a plain-terminal `[y/N]`; it
    // probes tmux/SSH directly and needs neither the daemon nor the dashboard.
    run_open_migration_pass(project_name);

    // Starting the on-demand hub daemon and bootstrapping the orchestrator
    // dashboard session used to run synchronously here — the daemon socket wait
    // and the cold orchestrator launch were the bulk of the "shelbi draws
    // nothing for seconds" headless startup block. Both now run off the shell's
    // UI thread (`shell::run` -> `ShellState::spawn_startup`) so the first frame
    // draws immediately; the orchestrator session attaches when the bootstrap
    // completes (`rt-tui-headless-startup-block`).
    shell::run(project_name)
}

/// Run the cutover migration pass for `project_name` at open, prompting on
/// stderr for consent before killing any surviving remote `shelbi-w-<ws>`
/// session. Runs before the alt-screen is entered (from `run_main`), so a
/// `[y/N]` prompt is safe. Best-effort: a probe or state-write failure is
/// logged and leaves the affected workspace pending, which is the safe
/// direction (dispatch to a pending workspace is refused, not silently run).
fn run_open_migration_pass(project_name: &str) {
    use std::io::{IsTerminal, Write};

    let project = match shelbi_state::load_project(project_name) {
        Ok(p) => p,
        Err(e) => {
            tracing::debug!(project = %project_name, error = %e, "migration pass: load_project failed");
            return;
        }
    };

    // Consent prompt for a surviving remote session. A non-interactive stdin
    // declines (leaves the workspace pending) rather than killing silently.
    let mut consent = |kill: &shelbi_orchestrator::migration::MigrationKill<'_>| -> bool {
        if !std::io::stdin().is_terminal() {
            eprintln!(
                "shelbi: workspace `{}` on machine `{}` still has a tmux session \
                 `{}` from the previous runtime; not killing it (no terminal to \
                 confirm). It stays pending — rerun `shelbi {}` in a terminal to \
                 migrate it.",
                kill.workspace, kill.machine, kill.session, kill.project
            );
            return false;
        }
        eprint!(
            "shelbi: workspace `{}` on machine `{}` still has a tmux session `{}` \
             from the previous runtime. Kill it so the new runtime can take over \
             this workspace? [y/N] ",
            kill.workspace, kill.machine, kill.session
        );
        let _ = std::io::stderr().flush();
        let mut input = String::new();
        if std::io::stdin().read_line(&mut input).is_err() {
            return false;
        }
        matches!(input.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    };

    match shelbi_orchestrator::migration::run_migration_pass(&project, &mut consent) {
        Ok(report) => {
            let pending: Vec<&str> = report.pending().map(|w| w.workspace.as_str()).collect();
            if !pending.is_empty() {
                eprintln!(
                    "shelbi: {} workspace(s) still pending migration: {}. Dispatch to \
                     them is paused until their tmux session is confirmed gone; the \
                     rest of `{}` works normally.",
                    pending.len(),
                    pending.join(", "),
                    project_name
                );
            }
        }
        Err(e) => {
            tracing::debug!(project = %project_name, error = %e, "migration pass failed");
        }
    }
}
