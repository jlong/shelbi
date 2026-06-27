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
pub(crate) mod test_support {
    use std::sync::Mutex;

    /// Serializes any test that mutates the process-global `SHELBI_HOME`
    /// env var. Tests across modules share one binary (and thus one env),
    /// so they must all lock the *same* mutex or they race each other.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());
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
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = KanbanApp::new(project_name);
    app.refresh();

    let result = handlers::kanban::tasks_loop(&mut term, &mut app);

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
