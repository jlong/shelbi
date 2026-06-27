use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;

use crate::kanban;
use crate::KanbanApp;

pub(crate) fn tasks_loop<B: ratatui::backend::Backend>(
    term: &mut Terminal<B>,
    app: &mut KanbanApp,
) -> Result<()> {
    loop {
        app.maybe_refresh();
        term.draw(|f| kanban::render_full(f, app, f.area()))?;
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
                    handle_kanban_key(app, k.code, k.modifiers);
                }
                Event::Mouse(m) => handle_kanban_mouse(app, m),
                _ => {}
            }
        }
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

/// Left-click on a card opens its popover — same path as ENTER/SPACE on the
/// keyboard. Clicks outside any card are a no-op. With the popover open we
/// ignore clicks entirely; the popover has its own dismiss keys.
fn handle_kanban_mouse(app: &mut KanbanApp, mouse: MouseEvent) {
    if app.popover_is_open() {
        return;
    }
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        if let Some((col, row)) = app.card_at(mouse.column, mouse.row) {
            app.open_popover_at(col, row);
        }
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
