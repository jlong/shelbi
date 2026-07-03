use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyEvent, KeyEventKind};
use ratatui::{backend::Backend, Terminal};
use shelbi_state::keymap::{GlobalAction, Keymaps, ReviewAction};

use crate::review::{self, ReviewApp};

/// What the review handler signals back to its event loop. Mirrors the
/// sidebar / kanban handlers' `Outcome`: `Quit` ends the loop (the
/// parent shell loop respawns the pane); `OpenPalette` is currently a
/// no-op (tmux intercepts Ctrl+P at the multiplexer level before it
/// reaches this TUI) but is surfaced as its own variant so a future
/// in-TUI palette can drop in without re-plumbing the dispatch site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Continue,
    Quit,
    OpenPalette,
}

pub fn review_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut ReviewApp,
    km: &Keymaps,
) -> Result<()> {
    loop {
        app.maybe_refresh();
        term.draw(|f| review::render_full(f, app, f.area()))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match handle_review_key(app, k, km) {
                    Outcome::Quit => return Ok(()),
                    Outcome::Continue | Outcome::OpenPalette => {}
                }
            }
        }
    }
}

/// Dispatch a key press through the merged keymaps. Global chords (Quit,
/// Zen toggle, palette) win first — a per-mode rebind can't shadow them.
/// Anything not bound returns [`Outcome::Continue`] so unfamiliar chords
/// fall through silently rather than triggering a default action.
pub fn handle_review_key(app: &mut ReviewApp, key: KeyEvent, km: &Keymaps) -> Outcome {
    if let Some(global) = km.global.dispatch(key) {
        return match global {
            GlobalAction::Quit => Outcome::Quit,
            // Zen toggle is sidebar-owned (the review view doesn't carry
            // the zen-state machinery), so it's a no-op here.
            GlobalAction::ZenToggle => Outcome::Continue,
            GlobalAction::OpenPalette => Outcome::OpenPalette,
        };
    }
    match km.review.dispatch(key) {
        Some(ReviewAction::NavUp) => app.nav_up(),
        Some(ReviewAction::NavDown) => app.nav_down(),
        Some(ReviewAction::ScrollBodyUp) => app.scroll_body_up(),
        Some(ReviewAction::ScrollBodyDown) => app.scroll_body_down(),
        Some(ReviewAction::PageBodyUp) => app.scroll_body_page_up(),
        Some(ReviewAction::PageBodyDown) => app.scroll_body_page_down(),
        Some(ReviewAction::ScrollBodyHome) => app.scroll_body_home(),
        Some(ReviewAction::Activate) => app.activate_selection(),
        Some(ReviewAction::Refresh) => app.refresh(),
        None => {}
    }
    Outcome::Continue
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ENV_LOCK;
    use crossterm::event::{KeyCode, KeyModifiers};
    use shelbi_state::keymap::load_keymaps;

    /// Load a default `Keymaps` from a temp `$SHELBI_HOME` so a stray
    /// real `~/.shelbi/keys.yaml` can't pollute the test. Caller holds
    /// `ENV_LOCK` because we mutate the process env.
    fn fresh_keymaps() -> Keymaps {
        let home = std::env::temp_dir().join(format!(
            "shelbi-review-handler-test-{}-{}",
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

    fn ev(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    /// Every chord in the pre-keymaps parity table still dispatches to
    /// the same `ReviewAction` under default keymaps. Asserts on the
    /// dispatch result rather than ReviewApp state because most actions
    /// (nav, scroll) are no-ops on an empty queue.
    #[test]
    fn default_keymaps_dispatch_matches_parity_table() {
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();

        let cases: &[(KeyCode, KeyModifiers, ReviewAction)] = &[
            (KeyCode::Up, KeyModifiers::NONE, ReviewAction::NavUp),
            (KeyCode::Char('k'), KeyModifiers::NONE, ReviewAction::NavUp),
            (KeyCode::Down, KeyModifiers::NONE, ReviewAction::NavDown),
            (KeyCode::Char('j'), KeyModifiers::NONE, ReviewAction::NavDown),
            (KeyCode::Char('K'), KeyModifiers::SHIFT, ReviewAction::ScrollBodyUp),
            (KeyCode::Char('J'), KeyModifiers::SHIFT, ReviewAction::ScrollBodyDown),
            (KeyCode::PageUp, KeyModifiers::NONE, ReviewAction::PageBodyUp),
            (KeyCode::Char('u'), KeyModifiers::NONE, ReviewAction::PageBodyUp),
            (KeyCode::PageDown, KeyModifiers::NONE, ReviewAction::PageBodyDown),
            (KeyCode::Char('d'), KeyModifiers::NONE, ReviewAction::PageBodyDown),
            (KeyCode::Char('g'), KeyModifiers::NONE, ReviewAction::ScrollBodyHome),
            (KeyCode::Home, KeyModifiers::NONE, ReviewAction::ScrollBodyHome),
            (KeyCode::Enter, KeyModifiers::NONE, ReviewAction::Activate),
            (KeyCode::Char(' '), KeyModifiers::NONE, ReviewAction::Activate),
            (KeyCode::Char('r'), KeyModifiers::NONE, ReviewAction::Refresh),
        ];
        for (code, mods, want) in cases {
            assert_eq!(
                km.review.dispatch(ev(*code, *mods)),
                Some(*want),
                "chord {code:?}+{mods:?} should dispatch to {want:?}"
            );
        }

        std::env::remove_var("SHELBI_HOME");
    }

    /// Ctrl+C → `Outcome::Quit`; Ctrl+P → `Outcome::OpenPalette`; an
    /// unbound chord → `Outcome::Continue`. These are the only global
    /// chords the review handler surfaces — Zen toggle (Alt+Z) is
    /// sidebar-owned and collapses to `Continue` here.
    #[test]
    fn global_chords_route_to_expected_outcomes() {
        let _g = ENV_LOCK.lock().unwrap();
        let km = fresh_keymaps();
        let mut app = ReviewApp::new("demo");

        assert_eq!(
            handle_review_key(&mut app, ev(KeyCode::Char('c'), KeyModifiers::CONTROL), &km),
            Outcome::Quit
        );
        assert_eq!(
            handle_review_key(&mut app, ev(KeyCode::Char('p'), KeyModifiers::CONTROL), &km),
            Outcome::OpenPalette
        );
        // Alt+Z is bound to GlobalAction::ZenToggle but the review view
        // has no zen machinery, so we swallow it as Continue.
        assert_eq!(
            handle_review_key(&mut app, ev(KeyCode::Char('z'), KeyModifiers::ALT), &km),
            Outcome::Continue
        );
        // Unbound chord.
        assert_eq!(
            handle_review_key(&mut app, ev(KeyCode::Char('x'), KeyModifiers::NONE), &km),
            Outcome::Continue
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// `defaults.review.activate: o` should rebind Activate to `o` and
    /// leave Enter/Space unbound for the review mode (the merge replaces
    /// — it does not union).
    #[test]
    fn user_override_rebinds_activate_and_unbinds_defaults() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-review-handler-override-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::write(
            home.join("keys.yaml"),
            "defaults:\n  review:\n    activate: o\n",
        )
        .unwrap();
        let (km, diags) = load_keymaps(None);
        assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");

        // `o` now fires Activate.
        assert_eq!(
            km.review.dispatch(ev(KeyCode::Char('o'), KeyModifiers::NONE)),
            Some(ReviewAction::Activate)
        );
        // Enter and Space are no longer bound to Activate (or anything
        // else in this mode).
        assert_eq!(km.review.dispatch(ev(KeyCode::Enter, KeyModifiers::NONE)), None);
        assert_eq!(km.review.dispatch(ev(KeyCode::Char(' '), KeyModifiers::NONE)), None);
        // Untouched actions keep their built-ins.
        assert_eq!(
            km.review.dispatch(ev(KeyCode::Char('r'), KeyModifiers::NONE)),
            Some(ReviewAction::Refresh)
        );

        std::env::remove_var("SHELBI_HOME");
    }
}
