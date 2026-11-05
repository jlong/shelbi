//! Shelbi's own diff in the shell: `diff_tool: builtin` shows it in a review and
//! a workspace with no session behind it, an external tool keeps its session,
//! and while the diff has the keyboard its keys win over the shell's. Sessions
//! here are a test's own, so no test reads a real worktree or the daemon.

use super::*;
use crate::shell::diff_view;
use shelbi_app::diff::comments::Author;
use shelbi_app::diff::{DiffOptions, DiffSession};
use shelbi_state::keymap::validate_keymaps_yaml;
use shelbi_state::AppTools;

/// A session that fails to open: the view on top of it behaves all the same.
fn fake_opener() -> diff_view::DiffOpener {
    Arc::new(|| {
        let options = DiffOptions { author: Author::human("j"), watch: None, wake: Arc::new(|| {}) };
        DiffSession::open(|| Err("no worktree in this test".to_string()), options)
    })
}

fn builtin() -> AppTools {
    AppTools { builtin_diff: true, ..AppTools::default() }
}

/// A shell with review `t-1` open, its keymaps the defaults.
fn with_review(apps: AppTools) -> ShellState {
    let mut st = test_state();
    st.keymaps = validate_keymaps_yaml("", None).0;
    st.palette_chord = st.keymaps.global.first_chord_for(GlobalAction::OpenPalette).copied();
    st.build_review_interface(ReviewOpenParams {
        task_id: "t-1".into(),
        slot: "review-1".into(),
        worktree: "/wt".into(),
        editor_name: "Vim".into(),
        apps,
        has_review_url: false,
        task: None,
    });
    st.review.as_mut().unwrap().set_diff_opener(fake_opener());
    st
}

/// A shell with workspace `w-1` open.
fn with_workspace(apps: AppTools) -> ShellState {
    let mut st = test_state();
    st.keymaps = validate_keymaps_yaml("", None).0;
    st.palette_chord = st.keymaps.global.first_chord_for(GlobalAction::OpenPalette).copied();
    st.build_workspace_interface(WorkspaceOpenParams {
        workspace: "w-1".into(),
        remote_address: None,
        worktree: "/wt".into(),
        editor_name: "Vim".into(),
        apps,
        task: None,
    });
    st.workspace.as_mut().unwrap().set_diff_opener(fake_opener());
    st
}

fn press(st: &mut ShellState, code: KeyCode, mods: KeyModifiers) {
    st.handle_key(KeyEvent::new(code, mods));
}

/// What the main area shows, drawn through the interface.
fn main_text(frame_of: impl FnOnce(&mut ratatui::Frame, Rect, Rect)) -> String {
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(100, 24)).unwrap();
    term.draw(|f| {
        let area = f.area();
        let panel = Rect::new(0, 0, 28, area.height);
        let main = Rect::new(28, 0, area.width - 28, area.height);
        frame_of(f, panel, main);
    })
    .unwrap();
    let buf = term.backend().buffer().clone();
    (0..buf.area.height)
        .map(|y| (28..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn a_review_shows_the_built_in_diff_without_asking_the_daemon() {
    let mut st = with_review(builtin());
    st.apply_review_action(ReviewAction::ShowContent(Some(shelbi_app::ReviewRole::Diff)));
    let r = st.review.as_mut().unwrap();
    assert!(r.content_focused(), "selecting it puts the keyboard on it");
    let text = main_text(|f, panel, main| {
        r.render(f, panel, main, true, true);
    });
    // Its frame and status line, not "Loading diff…" over a session.
    assert!(text.contains("NORMAL · unified"), "{text}");
}

#[test]
fn a_review_with_an_external_tool_keeps_its_session() {
    let mut st = with_review(AppTools::default());
    let r = st.review.as_mut().unwrap();
    // The daemon's half answers here, so no test reaches a real one.
    r.set_diff_ensure(Arc::new(|| Ok(())));
    r.show_diff();
    assert!(!r.diff_claims(&KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)));
    let text = main_text(|f, panel, main| {
        r.render(f, panel, main, true, true);
    });
    assert!(!text.contains("NORMAL · unified"), "{text}");
}

#[test]
fn a_workspace_shows_the_built_in_diff_and_goes_back_to_the_agent() {
    let mut st = with_workspace(builtin());
    st.apply_workspace_action(WorkspaceAction::ShowContent(Some(shelbi_app::ReviewRole::Diff)));
    assert!(st.workspace_rx.is_none(), "no session to ensure");
    let w = st.workspace.as_mut().unwrap();
    assert!(w.content_focused());
    let text = main_text(|f, panel, main| {
        w.render(f, panel, main, true, true);
    });
    assert!(text.contains("NORMAL · unified"), "{text}");

    st.apply_workspace_action(WorkspaceAction::ShowContent(None));
    let w = st.workspace.as_mut().unwrap();
    let text = main_text(|f, panel, main| {
        w.render(f, panel, main, true, true);
    });
    assert!(!text.contains("NORMAL · unified"), "{text}");
}

#[test]
fn a_workspace_with_an_external_tool_doesnt_take_the_built_in_path() {
    // Short of the session ensure itself, which would reach a daemon.
    let st = with_workspace(AppTools::default());
    assert!(!st.workspace.as_ref().unwrap().builtin_diff());
    let st = with_workspace(AppTools { diff: true, ..AppTools::default() });
    assert!(!st.workspace.as_ref().unwrap().builtin_diff());
}

#[test]
fn the_diffs_keys_win_while_it_has_the_keyboard() {
    let mut st = with_review(builtin());
    // Before it's on show, Ctrl-p is Shelbi's palette.
    press(&mut st, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert!(matches!(st.overlay, Some(ActiveOverlay::Palette(_))));
    st.overlay = None;

    st.apply_review_action(ReviewAction::ShowContent(Some(shelbi_app::ReviewRole::Diff)));
    // Now it's the diff's file palette, and the shell's stays closed.
    press(&mut st, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert!(st.overlay.is_none(), "the diff took it");
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);

    // Shelbi's palette stays on Ctrl-Space.
    press(&mut st, KeyCode::Char(' '), KeyModifiers::CONTROL);
    assert!(matches!(st.overlay, Some(ActiveOverlay::Palette(_))));
    st.overlay = None;

    // Esc with nothing held gives the keyboard back, and Ctrl-p is the shell's
    // again.
    press(&mut st, KeyCode::Esc, KeyModifiers::NONE);
    assert!(!st.review.as_ref().unwrap().content_focused());
    press(&mut st, KeyCode::Char('p'), KeyModifiers::CONTROL);
    assert!(matches!(st.overlay, Some(ActiveOverlay::Palette(_))));
    st.overlay = None;

    // Ctrl-l comes back to the diff, Ctrl-h leaves it: neither is the diff's.
    press(&mut st, KeyCode::Char('l'), KeyModifiers::CONTROL);
    assert!(st.review.as_ref().unwrap().content_focused());
    press(&mut st, KeyCode::Char('h'), KeyModifiers::CONTROL);
    assert!(!st.review.as_ref().unwrap().content_focused());
    // Tab from the panel comes back too, but from the diff it's the hunk filter.
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    assert!(st.review.as_ref().unwrap().content_focused());
    press(&mut st, KeyCode::Tab, KeyModifiers::NONE);
    assert!(st.review.as_ref().unwrap().content_focused(), "Tab stays in the diff");
}

#[test]
fn a_rebound_key_follows_settings_into_an_open_diff() {
    let mut st = with_workspace(builtin());
    st.apply_workspace_action(WorkspaceAction::ShowContent(Some(shelbi_app::ReviewRole::Diff)));
    let chord = KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE);
    assert!(!st.diff_claims(&chord), "`e` isn't bound");
    let keymaps = validate_keymaps_yaml("defaults:\n  diff:\n    cursor_down: e\n", None).0;
    st.workspace
        .as_mut()
        .unwrap()
        .set_diff_setup(diff_view::DiffSetup { keymaps, style: DisplayStyle::Linux });
    assert!(st.diff_claims(&chord), "it follows");
}

#[test]
fn ctrl_g_queues_the_editor_at_the_line() {
    let mut st = with_review(builtin());
    st.apply_review_action(ReviewAction::OpenFile { path: PathBuf::from("/wt/a.rs"), line: Some(12) });
    assert_eq!(st.pending_editor.as_deref(), Some(std::path::Path::new("/wt/a.rs")));
    assert_eq!(st.pending_editor_line, Some(12));
}
