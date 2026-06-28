use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{backend::Backend, Terminal};

use crate::review::{self, ReviewApp};

pub fn review_loop<B: Backend>(term: &mut Terminal<B>, app: &mut ReviewApp) -> Result<()> {
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

pub fn handle_review_key(app: &mut ReviewApp, code: KeyCode) {
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
