use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::Backend, Terminal};
use shelbi_state::keymap::{GlobalAction, KanbanAction, Keymaps, PopoverAction};

use crate::kanban::{self, KanbanApp};

pub fn tasks_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut KanbanApp,
    km: &Keymaps,
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
                    match handle_kanban_key(app, k, km) {
                        Outcome::Quit => return Ok(()),
                        Outcome::Continue | Outcome::OpenPalette => {}
                    }
                }
                Event::Mouse(m) => handle_kanban_mouse(app, m),
                _ => {}
            }
        }
    }
}

/// What the kanban handler signals back to its event loop. Mirrors the
/// sidebar handler's [`crate::handlers::sidebar::Outcome`]: `Quit` ends
/// the loop, `OpenPalette` is currently a no-op (tmux intercepts Ctrl+P
/// at the multiplexer level before it reaches the TUI) but is surfaced
/// as its own variant so a future move of the palette into the TUI can
/// drop in without re-plumbing the dispatch site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    OpenPalette,
}

pub fn handle_kanban_key(app: &mut KanbanApp, key: KeyEvent, km: &Keymaps) -> Outcome {
    // Modal stacking: the task popover swallows input — board nav keys
    // would otherwise move the cursor underneath while the user is
    // reading. The popover does NOT consult `km.global`; global chords
    // (Ctrl+P open palette, Alt+Z zen toggle) are swallowed under the
    // modal too. Close the popover first to use them again.
    if app.popover_is_open() {
        handle_popover_key(app, key, km);
        return Outcome::Continue;
    }

    // Filter dropdowns are also modal — same precedence reason as the
    // popover. Sits below the popover so a card detail open over the
    // dropdown still routes input to the card view.
    if app.workspace_dropdown_is_open() {
        handle_workspace_dropdown_key(app, key.code);
        return Outcome::Continue;
    }
    if app.workflow_dropdown_is_open() {
        handle_workflow_dropdown_key(app, key.code);
        return Outcome::Continue;
    }

    // Global chords fire next so a remapped Ctrl+C / Alt+Z can't be
    // shadowed by a kanban binding sharing the same chord.
    if let Some(global) = km.global.dispatch(key) {
        return match global {
            GlobalAction::Quit => Outcome::Quit,
            // Zen toggle is sidebar-owned (the kanban view doesn't carry
            // the zen-state machinery), so it's a no-op here.
            GlobalAction::ZenToggle => Outcome::Continue,
            GlobalAction::OpenPalette => Outcome::OpenPalette,
        };
    }

    match km.kanban.dispatch(key) {
        Some(KanbanAction::NavLeft) => app.nav_left(),
        Some(KanbanAction::NavRight) => app.nav_right(),
        Some(KanbanAction::NavUp) => app.nav_up(),
        Some(KanbanAction::NavDown) => app.nav_down(),
        Some(KanbanAction::MoveCardLeft) => app.move_card_left(),
        Some(KanbanAction::MoveCardRight) => app.move_card_right(),
        Some(KanbanAction::ReorderUp) => app.reorder_up(),
        Some(KanbanAction::ReorderDown) => app.reorder_down(),
        Some(KanbanAction::OpenPopover) => app.open_popover(),
        Some(KanbanAction::Refresh) => app.refresh(),
        Some(KanbanAction::CycleWorkflowFilter) => app.cycle_workflow_filter(),
        None => {
            // The dropdown toggles live outside the action enum for now —
            // dropdown-open is a transient UI mode, not a stand-alone
            // action a user would meaningfully rebind. A future task can
            // promote them; until then, route `f` / `w` directly so
            // parity with the pre-refactor handler holds.
            match key.code {
                KeyCode::Char('f') => app.toggle_workspace_dropdown(),
                KeyCode::Char('w') => app.toggle_workflow_dropdown(),
                _ => {}
            }
        }
    }
    Outcome::Continue
}

/// Left-click on a card opens its popover — same path as ENTER/SPACE on the
/// keyboard. Clicks outside any card are a no-op. With the popover open we
/// ignore clicks entirely; the popover has its own dismiss keys. With only
/// the workspace filter dropdown open, a click on an option commits it; a
/// click anywhere outside the dropdown closes it (drop-to-dismiss pattern
/// that matches what users expect from native dropdowns).
pub fn handle_kanban_mouse(app: &mut KanbanApp, mouse: MouseEvent) {
    if app.popover_is_open() {
        return;
    }
    if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
        if app.workspace_dropdown_is_open() {
            if let Some(idx) = app.dropdown_option_at(mouse.column, mouse.row) {
                if let Some(d) = app.workspace_dropdown.as_mut() {
                    d.cursor = idx;
                }
                app.dropdown_select();
            } else if app.filter_chip_at(mouse.column, mouse.row) {
                // Click on the chip while open → close. Mirrors a
                // native dropdown's "click the trigger again to dismiss"
                // behavior.
                app.close_workspace_dropdown();
            } else {
                // Click outside the dropdown and outside the chip →
                // dismiss without changing the filter.
                app.close_workspace_dropdown();
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
            app.open_workspace_dropdown();
            return;
        }
        if let Some((col, row)) = app.card_at(mouse.column, mouse.row) {
            app.open_popover_at(col, row);
        }
    }
}

pub fn handle_popover_key(app: &mut KanbanApp, key: KeyEvent, km: &Keymaps) {
    // The popover is a modal — `km.global` is intentionally NOT consulted
    // so a Ctrl+P or Alt+Z from inside the popover does not leak through
    // to the global handlers. Close the popover first to use them.
    match km.popover.dispatch(key) {
        Some(PopoverAction::Close) => app.close_popover(),
        Some(PopoverAction::ScrollUp) => app.popover_scroll_up(),
        Some(PopoverAction::ScrollDown) => app.popover_scroll_down(),
        Some(PopoverAction::PageUp) => app.popover_scroll_page_up(),
        Some(PopoverAction::PageDown) => app.popover_scroll_page_down(),
        Some(PopoverAction::ScrollHome) => app.popover_scroll_home(),
        None => {}
    }
}

/// Keys consumed while the workspace filter dropdown is open. Enter /
/// Space commit the cursor's option; Esc dismisses without changing
/// the filter; `c` clears the filter back to "All" without needing to
/// navigate. `f` toggles the dropdown so the same key opens and closes
/// it.
pub fn handle_workspace_dropdown_key(app: &mut KanbanApp, code: KeyCode) {
    match code {
        KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('f') => app.close_workspace_dropdown(),
        KeyCode::Up | KeyCode::Char('k') => app.dropdown_nav_up(),
        KeyCode::Down | KeyCode::Char('j') => app.dropdown_nav_down(),
        KeyCode::Enter | KeyCode::Char(' ') => app.dropdown_select(),
        KeyCode::Char('c') => app.dropdown_clear(),
        _ => {}
    }
}

/// Sibling of [`handle_workspace_dropdown_key`] — same shape, `w` toggles
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kanban::TaskPopover;
    use crate::test_support::ENV_LOCK;
    use crossterm::event::KeyModifiers;
    use shelbi_state::keymap::load_keymaps;

    /// Load a default `Keymaps` from a temp `$SHELBI_HOME` so a stray
    /// real `~/.shelbi/keys.yml` can't pollute the test. Caller holds
    /// `ENV_LOCK` because we mutate the process env.
    fn fresh_keymaps() -> Keymaps {
        let home = std::env::temp_dir().join(format!(
            "shelbi-kanban-handler-test-{}-{}",
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

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn key_with(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn default_kanban_chords_dispatch_to_expected_methods() {
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();
        let mut app = KanbanApp::new("demo");

        // Navigation
        handle_kanban_key(&mut app, key(KeyCode::Left), &km);
        handle_kanban_key(&mut app, key(KeyCode::Char('h')), &km);
        handle_kanban_key(&mut app, key(KeyCode::Right), &km);
        handle_kanban_key(&mut app, key(KeyCode::Char('l')), &km);
        handle_kanban_key(&mut app, key(KeyCode::Up), &km);
        handle_kanban_key(&mut app, key(KeyCode::Char('k')), &km);
        handle_kanban_key(&mut app, key(KeyCode::Down), &km);
        handle_kanban_key(&mut app, key(KeyCode::Char('j')), &km);

        // Open popover (Enter and Space)
        handle_kanban_key(&mut app, key(KeyCode::Enter), &km);
        assert!(
            !app.popover_is_open(),
            "open_popover on an empty board is a no-op (no card selected)"
        );

        // Refresh — exercises the dispatch path without asserting an
        // observable side effect (refresh just reloads tasks).
        handle_kanban_key(&mut app, key(KeyCode::Char('r')), &km);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn reorder_up_fires_on_both_shift_up_and_capital_k() {
        // The dual binding the task notes called out: `K` and `Shift+Up`
        // must both reach `reorder_up`. We can't observe the call
        // directly without state, so we check that the dispatcher
        // resolves the chord — both must dispatch via `km.kanban`.
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();

        assert_eq!(
            km.kanban
                .dispatch(key_with(KeyCode::Char('K'), KeyModifiers::SHIFT)),
            Some(KanbanAction::ReorderUp)
        );
        assert_eq!(
            km.kanban
                .dispatch(key_with(KeyCode::Up, KeyModifiers::SHIFT)),
            Some(KanbanAction::ReorderUp)
        );
        assert_eq!(
            km.kanban
                .dispatch(key_with(KeyCode::Char('J'), KeyModifiers::SHIFT)),
            Some(KanbanAction::ReorderDown)
        );
        assert_eq!(
            km.kanban
                .dispatch(key_with(KeyCode::Down, KeyModifiers::SHIFT)),
            Some(KanbanAction::ReorderDown)
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn popover_swallows_global_chords() {
        // Acceptance criterion: Ctrl+P while popover is open does NOT
        // open the palette. The popover dispatcher uses km.popover only.
        // We can't open the palette from in-process (tmux owns Ctrl+P
        // globally), but the contract under test is "no global side
        // effect" — we verify the popover dispatcher only consults
        // km.popover by checking that a global chord (Alt+Z) does not
        // close the popover, while a bound popover chord (Esc) does.
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();

        let mut app = KanbanApp::new("demo");
        app.popover = Some(TaskPopover {
            task_id: "task-1".into(),
            scroll: 0,
        });
        assert!(app.popover_is_open());

        // Alt+Z (a `km.global` chord) must NOT close the popover.
        handle_popover_key(
            &mut app,
            key_with(KeyCode::Char('z'), KeyModifiers::ALT),
            &km,
        );
        assert!(app.popover_is_open(), "Alt+Z must not close the popover");

        // Ctrl+P likewise.
        handle_popover_key(
            &mut app,
            key_with(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &km,
        );
        assert!(app.popover_is_open(), "Ctrl+P must not close the popover");

        // Esc — a `km.popover` chord — closes.
        handle_popover_key(&mut app, key(KeyCode::Esc), &km);
        assert!(!app.popover_is_open(), "Esc closes the popover");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn quit_global_chord_signals_outcome_quit() {
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();
        let mut app = KanbanApp::new("demo");

        let out = handle_kanban_key(
            &mut app,
            key_with(KeyCode::Char('c'), KeyModifiers::CONTROL),
            &km,
        );
        assert_eq!(out, Outcome::Quit);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn open_palette_global_chord_signals_outcome_open_palette() {
        // Ctrl+P is intercepted by tmux in production, but the dispatcher
        // still surfaces it as its own variant so a future move of the
        // palette inside the TUI doesn't need to re-plumb the handler.
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();
        let mut app = KanbanApp::new("demo");

        let out = handle_kanban_key(
            &mut app,
            key_with(KeyCode::Char('p'), KeyModifiers::CONTROL),
            &km,
        );
        assert_eq!(out, Outcome::OpenPalette);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn unbound_chord_falls_through_without_panicking() {
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();
        let mut app = KanbanApp::new("demo");

        let out = handle_kanban_key(&mut app, key(KeyCode::Char('x')), &km);
        assert_eq!(out, Outcome::Continue);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn default_tab_chord_dispatches_to_cycle_workflow_filter() {
        // Acceptance criterion: a keybinding cycles through the
        // available workflows + an "All" state, going through the
        // KanbanAction enum (not the ad-hoc match the dropdown uses).
        // Tab is the default chord; this pins the route from the
        // dispatcher to the action so a future rebinding only changes
        // the chord, not the wiring.
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();
        assert_eq!(
            km.kanban.dispatch(key(KeyCode::Tab)),
            Some(KanbanAction::CycleWorkflowFilter)
        );

        // And the handler routes that action to the cycle method —
        // observe the side effect on `workflow_filter` via the
        // KanbanApp's pre-seeded default workflow.
        let mut app = KanbanApp::new("demo");
        assert!(app.workflow_filter.is_none(), "fresh app starts at All");
        handle_kanban_key(&mut app, key(KeyCode::Tab), &km);
        assert_eq!(
            app.workflow_filter.as_deref(),
            Some("default"),
            "Tab advances filter past All into the first loaded workflow"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn project_override_move_card_left_to_alt_h() {
        // Acceptance criterion: a project override of move_card_left
        // to `alt-h` makes Alt+H fire MoveCardLeft and bare `H` no
        // longer.
        let _g = ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-kanban-handler-override-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        std::fs::write(
            home.join("keys.yml"),
            "defaults:\n  kanban:\n    move_card_left: alt-h\n",
        )
        .unwrap();

        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "{diags:?}");

        assert_eq!(
            km.kanban
                .dispatch(key_with(KeyCode::Char('h'), KeyModifiers::ALT)),
            Some(KanbanAction::MoveCardLeft)
        );
        // Bare `H` is no longer the binding.
        assert_eq!(
            km.kanban
                .dispatch(key_with(KeyCode::Char('H'), KeyModifiers::SHIFT)),
            None
        );

        std::env::remove_var("SHELBI_HOME");
    }
}
