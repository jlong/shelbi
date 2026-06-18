//! ratatui dashboard for shelbi.
//!
//! Phase 4a: two-pane layout (sidebar nav + content view) with keyboard
//! navigation and live state-file polling.
//! Phase 4b: ⌘K palette overlay, chat input bound to the orchestrator
//! pane, agent-view live tail.

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
mod palette;
mod sidebar;
mod view;

pub use app::{App, View};

/// Open the TUI for the given session name.
pub fn run(session_name: &str) -> Result<()> {
    let mut term = setup_terminal().context("setting up terminal")?;
    let mut app = App::new(session_name);
    let mut pal = palette::PaletteState::new();
    app.refresh().ok();

    let result = main_loop(&mut term, &mut app, &mut pal);

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
    pal: &mut palette::PaletteState,
) -> Result<()> {
    while !app.should_quit {
        app.maybe_refresh().ok();

        let pal_entries = if pal.open {
            palette::entries(app)
        } else {
            Vec::new()
        };
        let pal_results = if pal.open {
            shelbi_palette::search(&pal_entries, &pal.query)
        } else {
            Vec::new()
        };

        term.draw(|f| {
            let area = f.area();
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(24), Constraint::Min(40)])
                .split(area);
            sidebar::render(f, app, chunks[0]);
            view::render(f, app, chunks[1]);
            palette::render(f, pal, &pal_results);
        })?;

        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                if pal.open {
                    handle_palette_key(app, pal, &pal_results, k.code, k.modifiers);
                } else {
                    handle_key(app, pal, k.code, k.modifiers);
                }
            }
        }
    }
    Ok(())
}

fn handle_palette_key(
    app: &mut App,
    pal: &mut palette::PaletteState,
    results: &[(shelbi_palette::Entry, u16)],
    code: KeyCode,
    mods: KeyModifiers,
) {
    if mods.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('c') | KeyCode::Char('k') => {
                pal.close();
                return;
            }
            _ => {}
        }
    }
    match code {
        KeyCode::Esc => pal.close(),
        KeyCode::Up => {
            if pal.selected > 0 {
                pal.selected -= 1;
            }
        }
        KeyCode::Down => {
            if pal.selected + 1 < results.len() {
                pal.selected += 1;
            }
        }
        KeyCode::Enter => {
            if let Some((entry, _)) = results.get(pal.selected) {
                let keep_open = palette::activate(app, entry);
                if !keep_open {
                    pal.close();
                }
            }
        }
        KeyCode::Backspace => pal.backspace(),
        KeyCode::Char(c) => pal.type_char(c),
        _ => {}
    }
}

fn handle_key(
    app: &mut App,
    pal: &mut palette::PaletteState,
    code: KeyCode,
    mods: KeyModifiers,
) {
    // Global shortcuts.
    if mods.contains(KeyModifiers::CONTROL) {
        match code {
            KeyCode::Char('c') => {
                app.should_quit = true;
                return;
            }
            KeyCode::Char('k') => {
                pal.toggle();
                return;
            }
            _ => {}
        }
    }

    // Chat view: capture text input for the orchestrator.
    if matches!(app.view, View::Chat) {
        match code {
            KeyCode::Enter => {
                app.send_chat();
                return;
            }
            KeyCode::Backspace => {
                app.chat_input.pop();
                return;
            }
            KeyCode::Char(c) => {
                // Reserve digits + jk for navigation only when the buffer is empty.
                if app.chat_input.is_empty() && matches!(c, '1'..='4' | 'j' | 'k' | 'q' | 'r') {
                    // fall through to navigation
                } else {
                    app.chat_input.push(c);
                    return;
                }
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
