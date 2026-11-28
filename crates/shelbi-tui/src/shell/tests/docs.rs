//! The Documentation through the shell's real event path: its nav row after Issues,
//! Enter swapping the tree into the sidebar's column and Esc swapping the nav
//! back, the tree's and the document's keys, a hub change refreshing what
//! shows, and `e` handing the document to the editor. The hub is the test
//! docs of `docs_tests` (no daemon, no real home).

use std::sync::{Arc, Mutex};

use super::*;
use crate::shell::docs::tests::{ask_of, FakeDocs};
use shelbi_app::docs::DocsRequest;

/// The shipped nav over the test docs, the sidebar focused on Orchestrator.
fn docs_state() -> (ShellState, Arc<Mutex<FakeDocs>>) {
    let mut st = smoke_state();
    st.effect_tap = None;
    st.client.set_sidebar_width(shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH);
    let docs = Arc::new(Mutex::new(FakeDocs::sample()));
    let ask = ask_of(&docs);
    st.docs_ports = Arc::new(move |_: &str| {
        let (store, _) = refusing_docs_ports();
        (store, ask.clone())
    });
    st.client.focus_sidebar();
    (st, docs)
}

/// Fold in the Documentation's hub work until it's all in.
fn settle_docs(st: &mut ShellState) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        st.poll_docs();
        if !st.docs.as_ref().is_some_and(|w| w.busy()) {
            return;
        }
        assert!(Instant::now() < deadline, "the docs' work never finished");
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn draw(st: &mut ShellState) -> String {
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(120, 32)).unwrap();
    draw_frame(&mut term, st).unwrap();
    screen_text(&term)
}

/// Select the Documentation row with the keyboard and press Enter.
fn enter_docs(st: &mut ShellState) {
    let count = st.sidebar_view().unwrap().selectable_count();
    let at = (0..count)
        .find(|&i| st.sidebar_view().unwrap().target_at(i) == Some(RowTarget::Native(View::Docs)))
        .expect("a Documentation row");
    st.client.select(at, count);
    press(st, KeyCode::Enter);
    settle_docs(st);
}

#[test]
fn the_docs_row_sits_after_issues_and_enter_swaps_in_the_tree() {
    let (mut st, _) = docs_state();
    let nav: Vec<String> = st.sidebar_model.as_ref().unwrap().nav.iter().map(|n| n.label.clone()).collect();
    assert_eq!(nav, ["Orchestrator", "Issues", "Documentation", "Changelog"]);
    let screen = draw(&mut st);
    assert!(screen.contains("📖"), "the Documentation row's glyph:\n{screen}");

    enter_docs(&mut st);
    assert!(matches!(st.main_view, MainView::Native(View::Docs)));
    assert_eq!(st.client.view(), &View::Docs);
    assert!(st.docs_tree_shown());
    assert!(!st.focus_is_main(), "the tree has the keys");
    let screen = draw(&mut st);
    assert!(screen.contains("DOCS") && screen.contains("▸ Plans") && screen.contains("Shelbi docs"), "{screen}");
    assert!(!screen.contains("Orchestrator"), "the nav is swapped out:\n{screen}");
    assert!(screen.contains("● Synced"));

    // Esc: the nav again, on the Documentation row, with the document area kept.
    press(&mut st, KeyCode::Esc);
    assert!(!st.docs_tree_shown());
    assert!(matches!(st.main_view, MainView::Native(View::Docs)));
    let at = st.client.sidebar_selection();
    assert_eq!(st.sidebar_view().unwrap().target_at(at), Some(RowTarget::Native(View::Docs)));
    let screen = draw(&mut st);
    assert!(screen.contains("Orchestrator") && !screen.contains("DOCS"), "{screen}");
    // And Enter on it brings the tree back.
    press(&mut st, KeyCode::Enter);
    assert!(st.docs_tree_shown());
}

#[test]
fn the_tree_keys_reach_the_docs_and_enter_opens_the_document_in_the_main_area() {
    let (mut st, docs) = docs_state();
    enter_docs(&mut st);
    type_key(&mut st, 'l'); // open Plans
    type_key(&mut st, 'j'); // Archive
    type_key(&mut st, 'j'); // Awareness v2
    type_key(&mut st, 'j'); // OpenCode integration
    press(&mut st, KeyCode::Enter);
    settle_docs(&mut st);
    assert!(st.focus_is_main(), "the document has the keys");
    let screen = draw(&mut st);
    assert!(screen.contains("Plans › OpenCode integration"), "{screen}");
    assert!(screen.contains("OpenCode is a terminal coding agent."), "{screen}");
    // Esc from the document goes back to the tree.
    press(&mut st, KeyCode::Esc);
    assert!(!st.focus_is_main() && st.docs_tree_shown());

    // n, typed, Enter: a document through the hub, selected and shown.
    type_key(&mut st, 'n');
    for c in "roadmap".chars() {
        type_key(&mut st, c);
    }
    press(&mut st, KeyCode::Enter);
    settle_docs(&mut st);
    settle_docs(&mut st);
    assert!(docs.lock().unwrap().docs.contains_key("Plans/roadmap.md"));
    assert_eq!(st.docs.as_ref().unwrap().doc_path(), Some("Plans/roadmap.md"));
}

#[test]
fn a_hub_change_refreshes_the_tree_and_the_open_document() {
    let (mut st, docs) = docs_state();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut workers = recording_workers_factory(Arc::new(Mutex::new(Vec::new())))("proj");
    workers.change_rx = rx;
    st.workers = Some(workers);
    enter_docs(&mut st);
    st.docs.as_mut().unwrap().tree.select_path("README.md");
    press(&mut st, KeyCode::Enter);
    settle_docs(&mut st);
    docs.lock().unwrap().docs.insert("README.md".into(), "# Shelbi docs\n\nAn agent was here.\n".into());
    tx.send(shelbi_app::changes::ChangeWake::Docs { paths: vec!["README.md".into()] }).unwrap();
    let (board_changed, _) = st.drain_change_wakes();
    assert!(!board_changed, "docs change isn't a board refresh");
    settle_docs(&mut st);
    let screen = draw(&mut st);
    assert!(screen.contains("An agent was here."), "{screen}");
}

#[test]
fn e_queues_the_editor_and_its_save_goes_through_the_hub() {
    let (mut st, docs) = docs_state();
    enter_docs(&mut st);
    st.docs.as_mut().unwrap().tree.select_path("README.md");
    type_key(&mut st, 'e');
    settle_docs(&mut st);
    // What the run loop hands to $EDITOR.
    let edit = st.docs.as_mut().unwrap().take_edit().expect("an edit for the editor");
    std::fs::write(&edit.file, "# Shelbi docs\n\nEdited in vi.\n").unwrap();
    st.docs_edit_returned(edit, Ok(()));
    settle_docs(&mut st);
    assert!(docs.lock().unwrap().asked.iter().any(|r| matches!(
        r,
        DocsRequest::Write { path, content, .. } if path == "README.md" && content.contains("Edited in vi.")
    )));
    let screen = draw(&mut st);
    assert!(screen.contains("Saved README.md"), "{screen}");
}

#[test]
fn a_project_switch_leaves_the_docs_behind() {
    let (mut st, _) = docs_state();
    enter_docs(&mut st);
    assert!(st.docs.is_some());
    st.switch_project("other");
    assert!(st.docs.is_none());
}
