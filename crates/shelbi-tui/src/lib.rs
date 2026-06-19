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
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

mod app;
mod kanban;
mod poller;
mod review;
mod sidebar;

pub use app::{App, View};
pub use kanban::KanbanApp;
pub use poller::WorkerPoller;
pub use review::ReviewApp;

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
    let mut term = setup_sidebar_terminal().context("setting up terminal")?;
    let mut app = App::new_sidebar(project_name);
    app.refresh().ok();

    // Background poll loop: per-worker `tmux display-message` every
    // `worker_poll_interval_secs`, parses the `shelbi:<state>` marker,
    // persists transitions to `~/.shelbi/workers/<name>/status.yaml`
    // and `~/.shelbi/events.log`. The handle's Drop joins the thread,
    // so it shuts down when this function returns regardless of which
    // exit path we took.
    let _poller = WorkerPoller::start(project_name);

    let result = sidebar_loop(&mut term, &mut app);

    restore_sidebar_terminal(&mut term).context("restoring terminal")?;
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

    let result = tasks_loop(&mut term, &mut app);

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

    let result = review_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

/// Same as `setup_terminal` but also enables mouse capture so the sidebar
/// can react to clicks. Tmux only forwards these to the pane when its
/// `mouse` option is on — `ensure_dashboard` sets it on shelbi sessions.
fn setup_sidebar_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_sidebar_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

fn sidebar_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    while !app.should_quit {
        app.maybe_refresh().ok();

        term.draw(|f| sidebar::render_full(f, app, f.area()))?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    handle_key(app, k.code, k.modifiers);
                }
                Event::Mouse(m) => handle_mouse(app, m),
                _ => {}
            }
        }
    }
    Ok(())
}

fn tasks_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut KanbanApp,
) -> Result<()> {
    loop {
        app.maybe_refresh();
        term.draw(|f| kanban::render_full(f, app, f.area()))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Ctrl+C exits — the parent shell loop will respawn us.
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c'))
                {
                    return Ok(());
                }
                handle_kanban_key(app, k.code, k.modifiers);
            }
        }
    }
}

fn review_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut ReviewApp,
) -> Result<()> {
    loop {
        app.maybe_refresh();
        term.draw(|f| review::render_full(f, app, f.area()))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                // Ctrl+C exits — the parent shell loop will respawn us.
                if k.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(k.code, KeyCode::Char('c'))
                {
                    return Ok(());
                }
                handle_review_key(app, k.code);
            }
        }
    }
}

fn handle_review_key(app: &mut ReviewApp, code: KeyCode) {
    match code {
        KeyCode::Up | KeyCode::Char('k') => app.nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.nav_down(),
        KeyCode::Char('K') => app.scroll_body_up(),
        KeyCode::Char('J') => app.scroll_body_down(),
        KeyCode::PageUp | KeyCode::Char('u') => app.scroll_body_page_up(),
        KeyCode::PageDown | KeyCode::Char('d') => app.scroll_body_page_down(),
        KeyCode::Char('g') | KeyCode::Home => app.scroll_body_home(),
        KeyCode::Enter | KeyCode::Char(' ') => app.activate_selection(),
        KeyCode::Char('r') => app.refresh(),
        _ => {}
    }
}

fn handle_kanban_key(app: &mut KanbanApp, code: KeyCode, mods: KeyModifiers) {
    // When the task popover is open it swallows input — board nav keys
    // would otherwise move the cursor underneath while the user is reading.
    if app.popover_is_open() {
        handle_popover_key(app, code);
        return;
    }

    let shift = mods.contains(KeyModifiers::SHIFT);
    match code {
        KeyCode::Left | KeyCode::Char('h') => app.nav_left(),
        KeyCode::Right | KeyCode::Char('l') => app.nav_right(),
        KeyCode::Up | KeyCode::Char('k') => {
            if shift {
                app.reorder_up()
            } else {
                app.nav_up()
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            if shift {
                app.reorder_down()
            } else {
                app.nav_down()
            }
        }
        KeyCode::Enter | KeyCode::Char(' ') => app.open_popover(),
        // Shifted hjkl: caps-letter form, since shift+h/l won't carry the
        // SHIFT modifier on most terminals — the keycode arrives as the
        // uppercase char directly.
        KeyCode::Char('H') => app.move_card_left(),
        KeyCode::Char('L') => app.move_card_right(),
        KeyCode::Char('K') => app.reorder_up(),
        KeyCode::Char('J') => app.reorder_down(),
        KeyCode::Char('r') => app.refresh(),
        _ => {}
    }
}

fn handle_popover_key(app: &mut KanbanApp, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Enter | KeyCode::Char(' ') | KeyCode::Char('q') => {
            app.close_popover();
        }
        KeyCode::Up | KeyCode::Char('k') => app.popover_scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.popover_scroll_down(),
        KeyCode::PageUp | KeyCode::Char('u') => app.popover_scroll_page_up(),
        KeyCode::PageDown | KeyCode::Char('d') => app.popover_scroll_page_down(),
        KeyCode::Char('g') | KeyCode::Home => app.popover_scroll_home(),
        _ => {}
    }
}

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.should_quit = true;
        return;
    }
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Up | KeyCode::Char('k') => app.nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.nav_down(),
        KeyCode::Enter => app.activate_selection(),
        KeyCode::Char('r') => {
            app.refresh().ok();
        }
        _ => {}
    }
}

/// Left-click on a sidebar row selects and activates it (same as
/// nav-then-Enter). Scroll wheel walks the selection up/down without
/// activating, so a user can preview which row is highlighted.
fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            if let Some(idx) = app.row_at(mouse.column, mouse.row) {
                app.sidebar_index = idx;
                app.activate_selection();
            }
        }
        MouseEventKind::ScrollDown => app.nav_down(),
        MouseEventKind::ScrollUp => app.nav_up(),
        _ => {}
    }
}
