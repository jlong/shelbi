//! shelbi's two top-level entry points:
//!
//! - `run_main(project)` — set up the project's tmux session with the
//!   dashboard layout (sidebar + orchestrator) and `exec tmux attach`.
//!   This is what `shelbi` (no subcommand) invokes.
//! - `run_sidebar(project)` — the minimal ratatui process that lives in
//!   the dashboard's left pane: agent list, status footer, Ctrl+Space
//!   palette.
//!   Selecting an agent switches the tmux window. This is what
//!   `shelbi __sidebar PROJECT` invokes.

use std::io;

use anyhow::{Context, Result};
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

mod activity;
mod app;
mod handlers;
mod kanban;
mod keymap;
mod poller;
mod review;
mod sidebar;
mod zen_probe;

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
            AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind,
            OrchestratorSpec, Project, ZenConfig,
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
            },
        );
        let project = Project {
            name: project_name.into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.clone(),
                host: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: Vec::new(),
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        repo
    }
}

pub use activity::ActivityApp;
pub use app::{App, Row, View, WorkerBadge, WorkerOverview};
pub use kanban::KanbanApp;
pub use poller::WorkerPoller;
pub use review::ReviewApp;
pub use sidebar::decoration_to_color;

/// Set up the project's tmux session and attach to it. If we're already
/// inside a tmux client, use `switch-client` instead of `attach` (tmux
/// refuses to nest, modern tmux supports switching).
pub fn run_main(project_name: &str) -> Result<()> {
    // Bump the recently-used timestamp before bootstrapping the session
    // so the picker's recency sort reflects this launch even if the
    // tmux exec below replaces the process before normal shutdown.
    // Best-effort — a missing/unwritable ~/.shelbi/shelbi.yaml should
    // not block launching.
    let _ = shelbi_state::touch_project_launched(project_name);

    shelbi_orchestrator::ensure_dashboard(project_name)
        .with_context(|| format!("setting up dashboard for `{project_name}`"))?;

    let session = format!("shelbi-{project_name}");
    let inside_tmux = std::env::var("TMUX").is_ok();

    let args: &[&str] = if inside_tmux {
        &["switch-client", "-t"]
    } else {
        &["attach", "-t"]
    };

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("tmux")
            .args(args)
            .arg(&session)
            .exec();
        Err(err.into())
    }
    #[cfg(not(unix))]
    {
        let status = std::process::Command::new("tmux")
            .args(args)
            .arg(&session)
            .status()?;
        if !status.success() {
            anyhow::bail!("tmux exited with {status}");
        }
        Ok(())
    }
}

/// Run the minimal ratatui sidebar in the current pane.
pub fn run_sidebar(project_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = App::new_sidebar(project_name);
    app.refresh().ok();

    // Load merged keymaps once — embedded builtins, then
    // `~/.shelbi/keys.yml::defaults`, then `projects.<project_name>`.
    // Diagnostics print to stderr; bad config never blocks launch.
    let (keymaps, diags) = shelbi_state::keymap::load_keymaps(Some(project_name));
    for d in &diags {
        match d {
            shelbi_state::keymap::KeymapDiagnostic::Error { message, location, .. } => {
                if let Some(loc) = location {
                    eprintln!("shelbi: keys.yml error: {message} (at {loc})");
                } else {
                    eprintln!("shelbi: keys.yml error: {message}");
                }
            }
            shelbi_state::keymap::KeymapDiagnostic::Warning { message, location, .. } => {
                if let Some(loc) = location {
                    eprintln!("shelbi: keys.yml warning: {message} (at {loc})");
                } else {
                    eprintln!("shelbi: keys.yml warning: {message}");
                }
            }
        }
    }
    app.keymaps = keymaps;

    // First-run probe: on fresh installs (no ~/.shelbi/config.yaml),
    // verify Alt+Z is delivered and let the user pick a fallback if not.
    // Best-effort: an error here defaults to Alt+Z so the sidebar still
    // launches with a working binding on cooperative terminals.
    app.zen_toggle_chord = zen_probe::ensure_zen_keymap(&mut term)
        .unwrap_or(shelbi_state::ZenToggleChord::AltZ);

    // Background poll loop: per-worker `tmux display-message` every
    // `worker_poll_interval_secs`, parses the `shelbi:<state>` marker,
    // persists transitions to `~/.shelbi/workers/<name>/status.yaml`
    // and `~/.shelbi/events.log`. The handle's Drop joins the thread,
    // so it shuts down when this function returns regardless of which
    // exit path we took.
    let _poller = WorkerPoller::start(project_name);

    let result = handlers::sidebar::sidebar_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Run the Kanban tasks view in the current pane. Meant to be hosted in
/// the project's hidden stash session and swapped into the dashboard via
/// the palette. Parent shell wraps invocation in `while true; do …; done`
/// so an accidental crash respawns instead of leaving an empty pane.
pub fn run_tasks(project_name: &str) -> Result<()> {
    // Load `keys.yml` before the alt-screen swap so parse / collision
    // diagnostics land on the terminal the user can still see. Bad
    // config never blocks launch — affected actions fall back to
    // built-in defaults. Formatting mirrors `run_sidebar` so both
    // entry points present diagnostics the same way.
    let (keymaps, diags) = shelbi_state::keymap::load_keymaps(Some(project_name));
    for d in &diags {
        match d {
            shelbi_state::keymap::KeymapDiagnostic::Error { message, location, .. } => {
                if let Some(loc) = location {
                    eprintln!("shelbi: keys.yml error: {message} (at {loc})");
                } else {
                    eprintln!("shelbi: keys.yml error: {message}");
                }
            }
            shelbi_state::keymap::KeymapDiagnostic::Warning { message, location, .. } => {
                if let Some(loc) = location {
                    eprintln!("shelbi: keys.yml warning: {message} (at {loc})");
                } else {
                    eprintln!("shelbi: keys.yml warning: {message}");
                }
            }
        }
    }

    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = KanbanApp::new(project_name);
    app.refresh();

    let result = handlers::kanban::tasks_loop(&mut term, &mut app, &keymaps);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Run the review-queue ratatui view in the current pane. Hosted in the
/// hidden stash session and swapped in by the palette / sidebar — same
/// lifecycle as `run_tasks`.
pub fn run_review(project_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = ReviewApp::new(project_name);
    app.refresh();

    let result = handlers::review::review_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Run the activity-feed ratatui view in the current pane. Hosted in
/// the hidden stash session and swapped in by the palette / sidebar —
/// same lifecycle as `run_tasks` and `run_review`.
pub fn run_activity(project_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = ActivityApp::new(project_name);
    app.refresh();

    let result = handlers::activity::activity_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

/// Enter raw mode + alt screen + mouse capture. Tmux only forwards mouse
/// events to the pane when its `mouse` option is on — `ensure_dashboard`
/// sets it on shelbi sessions, so callers don't need to plumb anything.
/// Views that don't care about mouse just ignore the events.
fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}
