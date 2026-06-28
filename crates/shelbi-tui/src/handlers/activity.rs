use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::Backend, Terminal};

use crate::activity::{self, ActivityApp};

pub fn activity_loop<B: Backend>(term: &mut Terminal<B>, app: &mut ActivityApp) -> Result<()> {
    loop {
        app.maybe_refresh();
        term.draw(|f| activity::render_full(f, app, f.area()))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    // Ctrl+C exits — the parent shell loop will respawn us.
                    if k.modifiers.contains(KeyModifiers::CONTROL)
                        && matches!(k.code, KeyCode::Char('c'))
                    {
                        return Ok(());
                    }
                    handle_activity_key(app, k.code);
                }
                Event::Mouse(m) => handle_activity_mouse(app, m),
                _ => {}
            }
        }
    }
}

pub fn handle_activity_key(app: &mut ActivityApp, code: KeyCode) {
    match code {
        KeyCode::Up | KeyCode::Char('k') => app.scroll_up(),
        KeyCode::Down | KeyCode::Char('j') => app.scroll_down(),
        KeyCode::PageUp | KeyCode::Char('u') => app.scroll_page_up(),
        KeyCode::PageDown | KeyCode::Char('d') => app.scroll_page_down(),
        KeyCode::Char('g') | KeyCode::Home => app.scroll_home(),
        KeyCode::Char('G') | KeyCode::End => app.scroll_end(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Char('a') => app.reset_filter(),
        KeyCode::Char('z') => app.toggle_zen_filter(),
        KeyCode::Char('w') => app.toggle_workers_filter(),
        _ => {}
    }
}

/// Mouse-wheel scrolls the feed; left-click on the pill row toggles
/// the matching filter. Scroll-up walks toward older events (positive
/// scroll offset since newest sits at the top).
pub fn handle_activity_mouse(app: &mut ActivityApp, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.scroll_up(),
        MouseEventKind::ScrollDown => app.scroll_down(),
        MouseEventKind::Down(MouseButton::Left) => {
            app.click_pill(mouse.column, mouse.row);
        }
        _ => {}
    }
}
