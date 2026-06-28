use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::Backend, Terminal};

use crate::kanban::{self, KanbanApp};

pub fn tasks_loop<B: Backend>(term: &mut Terminal<B>, app: &mut KanbanApp) -> Result<()> {
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

pub fn handle_kanban_key(app: &mut KanbanApp, code: KeyCode, mods: KeyModifiers) {
    // When the task popover is open it swallows input — board nav keys
    // would otherwise move the cursor underneath while the user is reading.
    if app.popover_is_open() {
        handle_popover_key(app, code);
        return;
    }

    // Filter dropdowns are also modal — same precedence reason as the
    // popover. Sits below the popover so a card detail open over the
    // dropdown still routes input to the card view.
    if app.worker_dropdown_is_open() {
        handle_worker_dropdown_key(app, code);
        return;
    }
    if app.workflow_dropdown_is_open() {
        handle_workflow_dropdown_key(app, code);
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
        KeyCode::Char('f') => app.toggle_worker_dropdown(),
        KeyCode::Char('w') => app.toggle_workflow_dropdown(),
        KeyCode::Char('r') => app.refresh(),
        _ => {}
    }
}

/// Left-click on a card opens its popover — same path as ENTER/SPACE on the
/// keyboard. Clicks outside any card are a no-op. With the popover open we
/// ignore clicks entirely; the popover has its own dismiss keys. With only
/// the worker filter dropdown open, a click on an option commits it; a
/// click anywhere outside the dropdown closes it (drop-to-dismiss pattern
/// that matches what users expect from native dropdowns).
pub fn handle_kanban_mouse(app: &mut KanbanApp, mouse: MouseEvent) {
    if app.popover_is_open() {
        return;
    }
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        if app.worker_dropdown_is_open() {
            if let Some(idx) = app.dropdown_option_at(mouse.column, mouse.row) {
                if let Some(d) = app.worker_dropdown.as_mut() {
                    d.cursor = idx;
                }
                app.dropdown_select();
            } else if app.filter_chip_at(mouse.column, mouse.row) {
                // Click on the chip while open → close. Mirrors a
                // native dropdown's "click the trigger again to dismiss"
                // behavior.
                app.close_worker_dropdown();
            } else {
                // Click outside the dropdown and outside the chip →
                // dismiss without changing the filter.
                app.close_worker_dropdown();
            }
            return;
        }
        if app.workflow_dropdown_is_open() {
            if let Some(idx) = app.workflow_dropdown_option_at(mouse.column, mouse.row) {
                if let Some(d) = app.workflow_dropdown.as_mut() {
                    d.cursor = idx;
                }
                app.workflow_dropdown_select();
            } else {
                // Click on the chip → close. Click outside → close.
                // Both paths collapse to the same dismiss behaviour;
                // the chip branch exists only so the chip's click
                // routes here instead of bubbling to a kanban card.
                app.close_workflow_dropdown();
            }
            return;
        }
        if app.workflow_chip_at(mouse.column, mouse.row) {
            app.open_workflow_dropdown();
            return;
        }
        if app.filter_chip_at(mouse.column, mouse.row) {
            app.open_worker_dropdown();
            return;
        }
        if let Some((col, row)) = app.card_at(mouse.column, mouse.row) {
            app.open_popover_at(col, row);
        }
    }
}

pub fn handle_popover_key(app: &mut KanbanApp, code: KeyCode) {
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

/// Keys consumed while the worker filter dropdown is open. Enter /
/// Space commit the cursor's option; Esc dismisses without changing
/// the filter; `c` clears the filter back to "All" without needing to
/// navigate. `f` toggles the dropdown so the same key opens and closes
/// it.
pub fn handle_worker_dropdown_key(app: &mut KanbanApp, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => app.close_worker_dropdown(),
        KeyCode::Up | KeyCode::Char('k') => app.dropdown_nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.dropdown_nav_down(),
        KeyCode::Enter | KeyCode::Char(' ') => app.dropdown_select(),
        KeyCode::Char('c') => app.dropdown_clear(),
        _ => {}
    }
}

/// Sibling of [`handle_worker_dropdown_key`] — same shape, `w` toggles
/// the workflow dropdown so the same key opens and closes it.
pub fn handle_workflow_dropdown_key(app: &mut KanbanApp, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('w') => app.close_workflow_dropdown(),
        KeyCode::Up | KeyCode::Char('k') => app.workflow_dropdown_nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.workflow_dropdown_nav_down(),
        KeyCode::Enter | KeyCode::Char(' ') => app.workflow_dropdown_select(),
        KeyCode::Char('c') => app.workflow_dropdown_clear(),
        _ => {}
    }
}
