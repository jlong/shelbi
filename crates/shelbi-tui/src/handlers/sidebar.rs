use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{
    self, Event, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{backend::Backend, Terminal};
use shelbi_state::keymap::{GlobalAction, Keymaps, SidebarAction};

use crate::app::App;
use crate::sidebar;

/// What the sidebar handler signals back to its event loop. `Quit` ends
/// the loop; `OpenPalette` is currently a no-op for the sidebar (the
/// palette is hosted as a tmux popup driven by the orchestrator) but is
/// surfaced as its own variant so the global Ctrl+P binding stays
/// addressable from a single dispatch site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    OpenPalette,
}

pub fn sidebar_loop<B: Backend>(term: &mut Terminal<B>, app: &mut App) -> Result<()> {
    // Snapshot the merged keymaps once — `keys.yaml` is parsed at startup
    // (in `run_sidebar`) and must not be re-read per keystroke.
    let keymaps = app.keymaps().clone();
    while !app.should_quit {
        app.maybe_refresh().ok();
        // Drain any in-flight background review load so the spinner/outcome
        // shows up on the next frame without the UI thread ever blocking.
        app.poll_review_load();

        draw_sidebar_self_healing(term, app)?;

        if event::poll(Duration::from_millis(200))? {
            match event::read()? {
                Event::Key(k) => {
                    if k.kind != KeyEventKind::Press {
                        continue;
                    }
                    match handle_sidebar_key(app, k, &keymaps) {
                        Outcome::Quit => app.should_quit = true,
                        Outcome::Continue | Outcome::OpenPalette => {}
                    }
                }
                Event::Mouse(m) => handle_mouse(app, m),
                _ => {}
            }
        }
    }
    Ok(())
}

/// Draw one sidebar frame, self-healing instead of dying on a render fault.
///
/// The sidebar is the persistent left pane, and — unlike the tasks/activity
/// panes — a render-pass panic that unwound out of `run_sidebar` would drop
/// the whole process, leaving the pane blank with no crash event for the
/// supervisor to catch. So the draw is wrapped in [`catch_unwind`]: on a
/// caught panic we log it, ask [`App::recover_render_panic`] to count the
/// recovery, and force a full repaint on the next frame (the panicked frame
/// may have left ratatui's back buffer half-updated). Genuine backend I/O
/// errors still propagate — those aren't render faults.
///
/// A separate invariant check emits an observable signal when tmux hands the
/// pane a degenerate (zero/one-column) size, so a "sidebar vanished" report
/// is diagnosable from `tui.log` even though the pane repaints itself once a
/// usable size returns.
fn draw_sidebar_self_healing<B: Backend>(term: &mut Terminal<B>, app: &mut App) -> Result<()> {
    if let Ok(size) = term.size() {
        if let Some(signal) = app.note_sidebar_area(size.width, size.height, Instant::now()) {
            tracing::warn!("{signal}");
        }
    }
    guarded_draw(term, app, |f, app| {
        sidebar::render_full(f, app, f.area());
    })
}

/// The panic-guarded core of [`draw_sidebar_self_healing`], parameterized on
/// the render pass so a test can force it to panic and assert the self-heal
/// (count the recovery, repaint, keep drawing) without a live terminal.
/// Production passes [`sidebar::render_full`].
fn guarded_draw<B, R>(term: &mut Terminal<B>, app: &mut App, mut render: R) -> Result<()>
where
    B: Backend,
    R: FnMut(&mut ratatui::Frame, &mut App),
{
    // Discard the `CompletedFrame` inside the closure — it borrows `term`, so
    // it can't escape the `catch_unwind` boundary.
    let drew = catch_unwind(AssertUnwindSafe(|| {
        term.draw(|f| render(f, app)).map(|_| ())
    }));
    match drew {
        // Clean frame, or a real backend write error (propagate the latter —
        // it's a dead terminal, not a recoverable render fault).
        Ok(res) => res.map_err(Into::into),
        Err(payload) => {
            let signal = app.recover_render_panic(&panic_message(payload.as_ref()));
            tracing::error!("{signal}");
            // Repaint from scratch next frame — the aborted draw may have left
            // the back buffer inconsistent with what's on the pane.
            let _ = term.clear();
            Ok(())
        }
    }
}

/// Best-effort human string for a caught panic payload. `panic!`/`assert!`
/// carry a `&str` or `String`; anything else is opaque.
fn panic_message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic payload".to_string()
    }
}

/// Dispatch a key press through the merged keymaps. Global chords (Quit,
/// Zen toggle, palette) win first — a per-mode rebind can't shadow them.
/// Anything not bound returns [`Outcome::Continue`] so unfamiliar chords
/// fall through silently rather than triggering a default action.
pub fn handle_sidebar_key(app: &mut App, key: KeyEvent, km: &Keymaps) -> Outcome {
    // The review-load confirm now runs as a modal tmux popup that blocks the
    // sidebar loop while it's open (see `App::open_review_load_prompt`), so
    // there's no in-TUI overlay left to swallow keys for here.
    if let Some(global) = km.global.dispatch(key) {
        return match global {
            GlobalAction::Quit => Outcome::Quit,
            GlobalAction::ZenToggle => {
                app.toggle_zen_mode();
                Outcome::Continue
            }
            GlobalAction::OpenPalette => Outcome::OpenPalette,
        };
    }
    match km.sidebar.dispatch(key) {
        Some(SidebarAction::Quit) => Outcome::Quit,
        Some(SidebarAction::NavUp) => {
            app.nav_up();
            Outcome::Continue
        }
        Some(SidebarAction::NavDown) => {
            app.nav_down();
            Outcome::Continue
        }
        Some(SidebarAction::Activate) => {
            app.activate_selection();
            Outcome::Continue
        }
        Some(SidebarAction::Refresh) => {
            app.refresh().ok();
            Outcome::Continue
        }
        None => Outcome::Continue,
    }
}

/// Left-click on a sidebar row selects and activates it (same as
/// nav-then-Enter). Scroll wheel walks the selection up/down without
/// activating, so a user can preview which row is highlighted.
pub fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    // No in-TUI review-load overlay to guard against anymore: the confirm is a
    // blocking tmux popup, so the sidebar loop isn't processing mouse events
    // while it's up.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use crossterm::event::{KeyCode, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_state::keymap::load_keymaps;

    /// Forcing the failure condition from the task: a panic in the sidebar
    /// render pass must be caught and healed, not unwound out of the loop
    /// (which would drop the process and leave the pane blank). The recovery
    /// is counted and the next clean draw succeeds against the same terminal.
    #[test]
    fn guarded_draw_recovers_from_a_render_panic() {
        // Silence the panic hook for this thread's forced panic so the test
        // log stays clean; restore it afterwards.
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {}));

        let mut term = Terminal::new(TestBackend::new(24, 12)).unwrap();
        let mut app = App::new_sidebar("demo");

        let healed = guarded_draw(&mut term, &mut app, |_f, _app| panic!("render boom"));
        std::panic::set_hook(prev);

        assert!(healed.is_ok(), "a render panic must be caught, not propagated");
        assert_eq!(app.render_panics, 1, "the recovery is counted");

        // The pane keeps drawing: a subsequent clean frame renders fine.
        let ok = guarded_draw(&mut term, &mut app, |f, app| {
            sidebar::render_full(f, app, f.area());
        });
        assert!(ok.is_ok(), "the loop draws normally after recovery");
    }

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-handlers-sidebar-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// Default keymaps with `SHELBI_HOME` pointed at a fresh empty dir so
    /// the loader never reads the developer's real `~/.shelbi/keys.yaml`.
    /// Returns the keymaps plus the temp home so the caller can write a
    /// `keys.yaml` into it for override tests.
    fn defaults_with_home() -> (Keymaps, std::path::PathBuf) {
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let (km, _) = load_keymaps(None);
        (km, home)
    }

    /// Parity-table: every chord that used to fire in the inline
    /// `match KeyCode` body still produces the same outcome under the
    /// keymap dispatcher. Zen toggle is asserted separately because it
    /// needs a real project on disk to write `state.json`.
    #[test]
    fn parity_table_chords_route_to_expected_outcomes() {
        let _g = ENV_LOCK.lock().unwrap();
        let (km, _home) = defaults_with_home();
        let mut app = App::new_sidebar("demo");

        // Ctrl+C → global Quit.
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('c'), KeyModifiers::CONTROL), &km),
            Outcome::Quit
        );

        // `q` → sidebar Quit.
        let mut app = App::new_sidebar("demo");
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('q'), KeyModifiers::NONE), &km),
            Outcome::Quit
        );

        // Up / k → NavUp (sidebar); both arrow and letter route there.
        let mut app = App::new_sidebar("demo");
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Up, KeyModifiers::NONE), &km),
            Outcome::Continue
        );
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('k'), KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        // Down / j → NavDown.
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Down, KeyModifiers::NONE), &km),
            Outcome::Continue
        );
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('j'), KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        // Enter → Activate.
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Enter, KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        // Space → Activate. Same action as Enter — used to toggle a
        // focused machine row's collapse state without leaving the
        // keyboard.
        assert_eq!(
            km.sidebar
                .dispatch(ev(KeyCode::Char(' '), KeyModifiers::NONE)),
            Some(SidebarAction::Activate)
        );
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char(' '), KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        // r → Refresh. A missing project YAML is fine — `app.refresh()`
        // swallows the error and returns `()`; the handler returns
        // Continue.
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('r'), KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        // Ctrl+P (global OpenPalette) — currently a no-op for the sidebar
        // loop but the dispatcher still surfaces it as its own variant.
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('p'), KeyModifiers::CONTROL), &km),
            Outcome::OpenPalette
        );

        // Unknown chord → Continue (no-op).
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('x'), KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// User override in `keys.yaml` (`defaults.sidebar.nav_up: w`) takes
    /// effect: `w` now fires NavUp and the old defaults (`k`, `Up`) no
    /// longer match anything in the sidebar mode.
    #[test]
    fn keys_yml_override_redirects_nav_up_and_unbinds_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  sidebar:\n    nav_up: w\n",
        )
        .unwrap();
        let (km, _) = load_keymaps(None);

        let mut app = App::new_sidebar("demo");
        assert_eq!(
            km.sidebar
                .dispatch(ev(KeyCode::Char('w'), KeyModifiers::NONE)),
            Some(SidebarAction::NavUp)
        );
        // `w` flows through the dispatcher into NavUp (Outcome::Continue,
        // not OpenPalette — sidebar.nav_up wins because global doesn't
        // match `w`).
        assert_eq!(
            handle_sidebar_key(&mut app, ev(KeyCode::Char('w'), KeyModifiers::NONE), &km),
            Outcome::Continue
        );
        // Old defaults no longer fire NavUp — dispatcher returns None for
        // `k` / Up under the sidebar mode.
        assert_eq!(
            km.sidebar
                .dispatch(ev(KeyCode::Char('k'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km.sidebar.dispatch(ev(KeyCode::Up, KeyModifiers::NONE)),
            None
        );

        std::env::remove_var("SHELBI_HOME");
    }
}
