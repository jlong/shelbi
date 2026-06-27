use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::Terminal;

use crate::sidebar;
use crate::App;

pub(crate) fn sidebar_loop<B: ratatui::backend::Backend>(
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
                    handle_sidebar_key(app, k.code, k.modifiers);
                }
                Event::Mouse(m) => handle_mouse(app, m),
                _ => {}
            }
        }
    }
    Ok(())
}

fn handle_sidebar_key(app: &mut App, code: KeyCode, mods: KeyModifiers) {
    if mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('c')) {
        app.should_quit = true;
        return;
    }
    // Zen-toggle chord runs before any other binding so a remap to (say)
    // Ctrl+G can't be eaten by a future `g` nav key. The sidebar has no
    // modal overlays of its own — the palette is a tmux popup, which
    // pre-empts our input entirely — so there's no "modal swallow" branch
    // to check here.
    if crate::keymap::matches_zen_toggle(code, mods, app.zen_toggle_chord) {
        app.toggle_zen_mode();
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
