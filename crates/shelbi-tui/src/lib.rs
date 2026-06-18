//! ratatui dashboard for shelbi.
//!
//! Phase 4a: two-pane layout (sidebar nav + content view) with keyboard
//! navigation and live state-file polling. ⌘K palette + chat input land in
//! Phase 4b.

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    Terminal,
};

mod app;
mod sidebar;
mod view;

pub use app::{App, View};

/// Open the TUI for the given session name.
pub fn run(session_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = App::new(session_name);
    app.refresh().ok(); // best-effort first load

    let result = main_loop(&mut term, &mut app);

    restore_terminal(&mut term).context("restoring terminal")?;
    result
}

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
    execute!(term.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
    term.show_cursor()?;
    Ok(())
}

fn main_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    while !app.should_quit {
        app.maybe_refresh().ok();

        term.draw(|f| {
            let area = f.area();
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(24), Constraint::Min(40)])
                .split(area);
            sidebar::render(f, app, chunks[0]);
            view::render(f, app, chunks[1]);
        })?;

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

fn handle_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    if mods.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('c') | KeyCode::Char('q') => {
                app.should_quit = true;
                return;
            }
            KeyCode::Char('k') => {
                app.status_line = "(palette lands in Phase 4b)".into();
                return;
            }
            _ => {}
        }
    }
    match code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Char('1') => app.nav_jump(0),
        KeyCode::Char('2') => app.nav_jump(1),
        KeyCode::Char('3') => app.nav_jump(2),
        KeyCode::Char('4') => app.nav_jump(3),
        KeyCode::Up | KeyCode::Char('k') => app.nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.nav_down(),
        KeyCode::Enter => app.nav_activate(),
        KeyCode::Char('r') => {
            app.refresh().ok();
        }
        _ => {}
    }
}
