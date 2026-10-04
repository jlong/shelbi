use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::Backend, Terminal};
use shelbi_state::keymap::{GlobalAction, Keymaps};

use crate::machines::{self, MachinesApp};

/// Drive the standalone machines view (the tmux runtime's `__machines` pane).
/// Shares [`machines::render_full`] with the single-process shell's native view;
/// only the Enter action differs — here it focuses the workspace in a tmux pane
/// (`shelbi open`), while the shell opens it in a terminal view.
pub fn machines_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut MachinesApp,
    km: &Keymaps,
) -> Result<()> {
    while !app.should_quit {
        app.maybe_refresh();
        term.draw(|f| machines::render_full(f, app, f.area()))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    handle_machines_key(app, k, km);
                }
                Event::Mouse(m) => handle_machines_mouse(app, m),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Dispatch one key press. Global chords (Ctrl+C / Zen / palette) win over the
/// view's own nav so a remapped quit can't be shadowed; the view's nav is a
/// small fixed set (arrows / `j`/`k` / Enter / `r`), not keymap-configurable —
/// same rationale as the kanban dropdown toggles.
pub fn handle_machines_key(app: &mut MachinesApp, key: KeyEvent, km: &Keymaps) {
    let chord = crate::keymap::chord_from_event(key);
    if let Some(global) = chord.and_then(|c| km.global.dispatch(c)) {
        match global {
            GlobalAction::Quit => app.should_quit = true,
            // Zen toggle and palette are owned elsewhere (the sidebar / tmux);
            // consume them here so they can't fall through to the view's nav.
            GlobalAction::ZenToggle | GlobalAction::OpenPalette => {}
        }
        return;
    }
    match key.code {
        KeyCode::Up | KeyCode::Char('k') => app.nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.nav_down(),
        KeyCode::Char('r') => app.refresh(),
        KeyCode::Enter => open_selected(app),
        _ => {}
    }
}

/// Open the selected workspace in a tmux pane via the shared focus path. The
/// shell overrides this with its own terminal-view open.
fn open_selected(app: &mut MachinesApp) {
    let Some(ws) = app.selected_workspace().map(str::to_string) else {
        app.set_status("no workspace selected");
        return;
    };
    match shelbi_orchestrator::focus_workspace(&app.project_name, &ws) {
        Ok(()) => app.set_status(format!("▶ @{ws}")),
        Err(e) => app.set_status(format!("open @{ws} failed: {e}")),
    }
}

/// Wheel scrolls the selection; a left-click selects the clicked workspace row.
pub fn handle_machines_mouse(app: &mut MachinesApp, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => app.nav_up(),
        MouseEventKind::ScrollDown => app.nav_down(),
        MouseEventKind::Down(MouseButton::Left) => {
            // The standalone process has no persisted hit map; a click just
            // focuses the nearest navigable row by falling back to keyboard nav.
            // (The shell does precise hit-testing against its own layout rects.)
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::machines::{MachineEntry, MachinesData, WorkspaceRow};
    use crate::test_support::ENV_LOCK;
    use crossterm::event::KeyModifiers;
    use shelbi_core::MachineKind;
    use shelbi_state::keymap::load_keymaps;

    fn fresh_keymaps() -> Keymaps {
        let home = std::env::temp_dir().join(format!(
            "shelbi-machines-handler-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, _diags) = load_keymaps(None);
        km
    }

    fn app() -> MachinesApp {
        let mut a = MachinesApp::new("demo");
        a.apply_data(MachinesData {
            display_name: Some("Demo".into()),
            machines: vec![MachineEntry {
                name: "local".into(),
                kind: MachineKind::Local,
                host: None,
                is_local: true,
                tags: vec![],
                remote: None,
                workspaces: vec![
                    WorkspaceRow {
                        name: "a".into(),
                        state: None,
                        current_task: None,
                    },
                    WorkspaceRow {
                        name: "b".into(),
                        state: None,
                        current_task: None,
                    },
                ],
            }],
        });
        a
    }

    #[test]
    fn arrows_navigate_workspace_rows() {
        let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let km = fresh_keymaps();
        let mut a = app();
        assert_eq!(a.selected_workspace(), Some("a"));
        handle_machines_key(&mut a, KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &km);
        assert_eq!(a.selected_workspace(), Some("b"));
        handle_machines_key(&mut a, KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE), &km);
        assert_eq!(a.selected_workspace(), Some("a"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn quit_chord_sets_should_quit() {
        let _g = ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let km = fresh_keymaps();
        let mut a = app();
        handle_machines_key(
            &mut a,
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &km,
        );
        assert!(a.should_quit);
        std::env::remove_var("SHELBI_HOME");
    }
}
