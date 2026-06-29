use std::time::Duration;

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::Backend, Terminal};
use shelbi_state::keymap::{ActivityAction, GlobalAction, Keymaps};

use crate::activity::{self, ActivityApp};

pub fn activity_loop<B: Backend>(term: &mut Terminal<B>, app: &mut ActivityApp) -> Result<()> {
    // Snapshot the keymaps once up-front: the loader does file IO + chord
    // parsing, and we'd otherwise re-borrow `app.keymaps()` on every tick
    // (and run into a double-borrow against the `&mut app` the handler
    // wants). A `Keymaps` is plain HashMaps; cloning it is cheap relative
    // to a single key dispatch.
    let keymaps = app.keymaps().clone();
    while !app.should_quit {
        app.maybe_refresh();
        term.draw(|f| activity::render_full(f, app, f.area()))?;
        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    handle_activity_key(app, k, &keymaps);
                }
                Event::Mouse(m) => handle_activity_mouse(app, m),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Dispatch one key press against the configured keymaps. Global chords
/// (Ctrl+C / the Zen-toggle chord / Ctrl+P) win over activity chords so
/// a user can't accidentally shadow a quit binding with a local nav key.
pub fn handle_activity_key(app: &mut ActivityApp, key: KeyEvent, km: &Keymaps) {
    if let Some(global) = km.global.dispatch(key) {
        dispatch_global(app, global);
        return;
    }
    match km.activity.dispatch(key) {
        Some(ActivityAction::ScrollUp) => app.scroll_up(),
        Some(ActivityAction::ScrollDown) => app.scroll_down(),
        Some(ActivityAction::PageUp) => app.scroll_page_up(),
        Some(ActivityAction::PageDown) => app.scroll_page_down(),
        Some(ActivityAction::ScrollHome) => app.scroll_home(),
        Some(ActivityAction::ScrollEnd) => app.scroll_end(),
        Some(ActivityAction::Refresh) => app.refresh(),
        Some(ActivityAction::ResetFilter) => app.reset_filter(),
        Some(ActivityAction::ToggleZenFilter) => app.toggle_zen_filter(),
        Some(ActivityAction::ToggleWorkspacesFilter) => app.toggle_workspaces_filter(),
        None => {}
    }
}

fn dispatch_global(app: &mut ActivityApp, action: GlobalAction) {
    match action {
        GlobalAction::Quit => app.should_quit = true,
        GlobalAction::ZenToggle => app.toggle_zen_mode(),
        // Ctrl+P opens the palette via a tmux key binding that fires a
        // popup before the key reaches this process. Consuming the action
        // here is defensive — if a user remaps the chord, the activity
        // view at least doesn't fall through to the activity map (which
        // could shadow it with another binding).
        GlobalAction::OpenPalette => {}
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

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use shelbi_state::keymap::load_keymaps;

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn ev_plain(code: KeyCode) -> KeyEvent {
        ev(code, KeyModifiers::NONE)
    }

    fn default_app_and_keymaps() -> (ActivityApp, Keymaps) {
        let app = ActivityApp::new("demo");
        // Match what ActivityApp::new loads, but with a deterministic
        // call site so the tests don't depend on any keys.yml that
        // happens to sit under the caller's $SHELBI_HOME.
        let (km, _diags) = load_keymaps(None);
        (app, km)
    }

    #[test]
    fn default_keymap_routes_scroll_up_for_k_and_up() {
        let (mut app, km) = default_app_and_keymaps();
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('k')), &km);
        assert_eq!(app.scroll, 1, "k should scroll up by one line");

        let mut app = ActivityApp::new("demo");
        handle_activity_key(&mut app, ev_plain(KeyCode::Up), &km);
        assert_eq!(app.scroll, 1, "Up arrow should also scroll up");
    }

    #[test]
    fn default_keymap_routes_scroll_down_for_j_and_down() {
        let (mut app, km) = default_app_and_keymaps();
        app.scroll = 5;
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('j')), &km);
        assert_eq!(app.scroll, 4);

        let mut app = ActivityApp::new("demo");
        app.scroll = 5;
        handle_activity_key(&mut app, ev_plain(KeyCode::Down), &km);
        assert_eq!(app.scroll, 4);
    }

    #[test]
    fn default_keymap_routes_page_up_for_u_and_pageup() {
        let (mut app, km) = default_app_and_keymaps();
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('u')), &km);
        // page step is `viewport_h.max(1)`, viewport_h is 0 until render runs.
        assert_eq!(app.scroll, 1);

        let mut app = ActivityApp::new("demo");
        handle_activity_key(&mut app, ev_plain(KeyCode::PageUp), &km);
        assert_eq!(app.scroll, 1);
    }

    #[test]
    fn default_keymap_routes_page_down_for_d_and_pagedown() {
        let (mut app, km) = default_app_and_keymaps();
        app.scroll = 10;
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('d')), &km);
        assert_eq!(app.scroll, 9);

        let mut app = ActivityApp::new("demo");
        app.scroll = 10;
        handle_activity_key(&mut app, ev_plain(KeyCode::PageDown), &km);
        assert_eq!(app.scroll, 9);
    }

    #[test]
    fn default_keymap_routes_scroll_home_for_g_and_home() {
        let (mut app, km) = default_app_and_keymaps();
        app.scroll = 42;
        app.auto_scroll = false;
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('g')), &km);
        assert_eq!(app.scroll, 0);
        assert!(app.auto_scroll);

        let mut app = ActivityApp::new("demo");
        app.scroll = 7;
        handle_activity_key(&mut app, ev_plain(KeyCode::Home), &km);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn default_keymap_routes_scroll_end_for_shift_g_and_end() {
        // `scroll_end` clamps to `total_lines - 1` which is 0 in a fresh
        // app — but it also flips `auto_scroll` off, which is the witness
        // we use here without poking at private layout state.
        let (mut app, km) = default_app_and_keymaps();
        assert!(app.auto_scroll);
        handle_activity_key(&mut app, ev(KeyCode::Char('G'), KeyModifiers::SHIFT), &km);
        assert!(!app.auto_scroll, "Shift+G must drive scroll_end");

        let mut app = ActivityApp::new("demo");
        handle_activity_key(&mut app, ev_plain(KeyCode::End), &km);
        assert!(!app.auto_scroll, "End must drive scroll_end");
    }

    #[test]
    fn default_keymap_r_invokes_refresh() {
        let (mut app, km) = default_app_and_keymaps();
        let stale = app.last_refresh;
        std::thread::sleep(Duration::from_millis(2));
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('r')), &km);
        assert!(app.last_refresh > stale, "r should refresh");
    }

    #[test]
    fn default_keymap_a_resets_filter() {
        let (mut app, km) = default_app_and_keymaps();
        app.filter.zen = true;
        app.filter.workspaces = true;
        app.scroll = 9;
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('a')), &km);
        assert!(app.filter.is_all(), "a should clear both pills");
        assert_eq!(app.scroll, 0, "filter reset snaps scroll back to top");
    }

    #[test]
    fn default_keymap_z_toggles_zen_filter_not_zen_mode() {
        let (mut app, km) = default_app_and_keymaps();
        assert!(!app.filter.zen);
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('z')), &km);
        assert!(app.filter.zen, "plain z toggles the Zen filter pill");
        // Plain `z` must NOT touch Zen Mode (status line would say so).
        assert!(
            !app.status_line.starts_with("zen"),
            "plain z should not trigger global Zen Mode toggle: {:?}",
            app.status_line
        );
    }

    /// Acceptance criterion: Alt+Z drives the global Zen Mode toggle from
    /// the activity view, not the activity-local filter pill. Together
    /// with the plain-`z` test this proves the two chords route to
    /// distinct actions even though they share the keyname.
    #[test]
    fn alt_z_drives_global_zen_toggle_not_activity_filter() {
        use crate::test_support::ENV_LOCK;
        let _g = ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-altz-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        // Provision a minimal project so toggle_zen_mode can read/write
        // its state.json successfully — otherwise the dispatch still
        // routes correctly but status_line carries the error path.
        crate::test_support::provision_hub_repo_for_project(&home, "demo");

        let (mut app, km) = default_app_and_keymaps();
        handle_activity_key(&mut app, ev(KeyCode::Char('z'), KeyModifiers::ALT), &km);
        assert!(
            !app.filter.zen,
            "Alt+Z must NOT toggle the activity Zen filter"
        );
        assert_eq!(
            app.status_line, "zen on",
            "Alt+Z must drive the global Zen Mode toggle (status_line set)"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn default_keymap_w_toggles_workspaces_filter() {
        let (mut app, km) = default_app_and_keymaps();
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('w')), &km);
        assert!(app.filter.workspaces, "w should flip the workspaces pill on");
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('w')), &km);
        assert!(!app.filter.workspaces, "w should flip the workspaces pill off");
    }

    #[test]
    fn global_quit_chord_signals_loop_exit() {
        let (mut app, km) = default_app_and_keymaps();
        assert!(!app.should_quit);
        handle_activity_key(&mut app, ev(KeyCode::Char('c'), KeyModifiers::CONTROL), &km);
        assert!(app.should_quit, "Ctrl+C must set should_quit");
    }

    #[test]
    fn unbound_chord_is_silently_ignored() {
        let (mut app, km) = default_app_and_keymaps();
        let before = app.scroll;
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('x')), &km);
        assert_eq!(app.scroll, before, "x is not bound; app must be untouched");
        assert!(app.status_line.is_empty());
    }

    /// User-keys.yml override moves `toggle_zen_filter` from `z` to
    /// `Shift+Z`: the acceptance criterion's worked example. Verifies the
    /// dispatcher actually reads `Keymaps` rather than re-hardcoding the
    /// chord at the call site.
    #[test]
    fn override_routes_shift_z_to_toggle_zen_filter() {
        use shelbi_state::keymap::ModeKeymap;
        use shelbi_state::keymap::{ActivityAction, KeyChord};
        use std::collections::HashMap;

        let mut bindings: HashMap<KeyChord, ActivityAction> = HashMap::new();
        let mut by_action: HashMap<ActivityAction, Vec<KeyChord>> = HashMap::new();
        let shift_z = KeyChord::parse("shift-z").unwrap();
        bindings.insert(shift_z, ActivityAction::ToggleZenFilter);
        by_action.insert(ActivityAction::ToggleZenFilter, vec![shift_z]);
        let km = Keymaps {
            activity: ModeKeymap {
                bindings,
                by_action,
            },
            ..Keymaps::default()
        };

        // Plain `z` no longer toggles the filter — it's no-op under this
        // synthetic keymap (default activity bindings replaced wholesale).
        let mut app = ActivityApp::new("demo");
        handle_activity_key(&mut app, ev_plain(KeyCode::Char('z')), &km);
        assert!(
            !app.filter.zen,
            "plain z must not toggle the Zen filter when remapped to shift-z"
        );

        // Shift+Z fires the action.
        handle_activity_key(&mut app, ev(KeyCode::Char('Z'), KeyModifiers::SHIFT), &km);
        assert!(
            app.filter.zen,
            "shift-z override must drive the Zen filter toggle"
        );
    }
}
