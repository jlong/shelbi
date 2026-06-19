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
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

mod app;
mod kanban;
mod sidebar;

pub use app::{App, View};
pub use kanban::KanbanApp;

/// Set up the project's tmux session and attach to it. If we're already
/// inside a tmux client, use `switch-client` instead of `attach` (tmux
/// refuses to nest, modern tmux supports switching).
pub fn run_main(project_name: &str) -> Result<()> {
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

    let result = sidebar_loop(&mut term, &mut app);

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

    let result = tasks_loop(&mut term, &mut app);

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

fn sidebar_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    while !app.should_quit {
        app.maybe_refresh().ok();

        term.draw(|f| sidebar::render_full(f, app, f.area()))?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                handle_key(app, k.code, k.modifiers);
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

fn handle_kanban_key(app: &mut KanbanApp, code: KeyCode, mods: KeyModifiers) {
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
