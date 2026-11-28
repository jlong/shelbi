//! The Wiki's open document (`wiki-desktop-doc`): opened from the tree it
//! is the in-place editor; typing saves through the hub once it pauses (at
//! once on ⌘S); the hub's changes reload a clean document and hold for the
//! changed-elsewhere bar on a dirty one; the toolbar says who changed it
//! last and its ⋯ copies the path and lists the history.

use std::time::Duration;

use gpui::TestAppContext;
use shelbi_app::wiki::{Author, WikiCommit, WikiRequest};
use shelbi_app::wiki_doc::SAVE_AFTER;
use shelbi_app::wiki_sub::WikiWake;

use super::doc::{DocAction, DocMenu};
use super::tests::{FakeHub, T};

const DOC: &str = "Plans/awareness-v2.md";

impl T {
    /// Open `DOC` from the tree, as a click on its row does.
    fn open_doc(&mut self) {
        self.update(|v, window, cx| v.open_doc(DOC, window, cx));
        self.asked();
    }

    fn text(&mut self) -> String {
        let editor = self.read(|v| v.doc_editor()).expect("the editor shows");
        editor.read_with(&self.cx, |e, _| e.text().to_string())
    }

    /// Put the keys in the editor, the caret at the end, and type `text`.
    fn type_at_end(&mut self, text: &str) {
        let editor = self.read(|v| v.doc_editor()).expect("the editor shows");
        self.cx.update(|window, cx| editor.update(cx, |e, cx| e.focus_at(true, window, cx)));
        self.frame();
        self.cx.simulate_input(text);
        self.cx.run_until_parked();
    }

    /// Draw a frame (the input method follows the keys at the next one).
    fn frame(&mut self) {
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
    }

    /// Let the save debounce run out.
    fn pause(&mut self) {
        self.cx.executor().advance_clock(SAVE_AFTER + Duration::from_millis(50));
        self.cx.run_until_parked();
    }

    fn writes(&mut self) -> Vec<(String, String)> {
        self.asked()
            .into_iter()
            .filter_map(|r| match r {
                WikiRequest::Write { path, content, by, .. } => {
                    assert_eq!(by, Author::Person);
                    Some((path, content))
                }
                _ => None,
            })
            .collect()
    }

    fn hub_text(&self, path: &str) -> String {
        self.hub.lock().unwrap().entries[path].2.clone()
    }

    /// Someone else rewrites `DOC` on the hub, which announces it.
    fn change_elsewhere(&mut self, text: &str, source: &str) {
        self.hub.lock().unwrap().entries.get_mut(DOC).unwrap().2 = text.to_string();
        let wake = WikiWake::Changed {
            paths: vec![DOC.into()],
            source: source.into(),
        };
        self.update(|v, window, cx| v.woke(vec![wake], window, cx));
    }
}

fn commit(by: Option<Author>, author: &str, date: &str, subject: &str) -> WikiCommit {
    WikiCommit {
        sha: "0123456789abcdef".into(),
        author: author.into(),
        date: date.into(),
        subject: subject.into(),
        paths: vec![DOC.into()],
        by,
    }
}

#[gpui::test]
fn a_document_opens_in_the_editor_and_typing_saves_through_the_hub(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    assert!(t.drawn("wiki-doc-body"));
    assert!(t.drawn("wiki-breadcrumb"));
    assert_eq!(t.text(), "# Awareness v2\n\nBody of Plans/awareness-v2.md.\n");

    t.type_at_end(" More.");
    assert!(t.writes().is_empty(), "nothing saves while the typing goes on");
    t.pause();
    let want = "# Awareness v2\n\nBody of Plans/awareness-v2.md. More.\n";
    assert_eq!(t.writes(), [(DOC.to_string(), want.to_string())]);
    assert_eq!(t.hub_text(DOC), want);
    let now = chrono::Utc::now();
    assert_eq!(t.read(|v| v.updated_line(now)).as_deref(), Some("Updated by you · just now"));

    // ⌘S (Ctrl+S on Linux) saves at once.
    t.type_at_end("!");
    t.keys(&crate::keys::test_keys::text_spec("cmd-s"));
    assert_eq!(t.writes(), [(DOC.to_string(), want.replace(".\n", ".!\n"))]);
    t.pause();
    assert!(t.writes().is_empty(), "nothing new to save");
}

#[gpui::test]
fn a_new_title_relabels_the_tree(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    let editor = t.read(|v| v.doc_editor()).unwrap();
    t.cx.update(|window, cx| editor.update(cx, |e, cx| e.focus_at(false, window, cx)));
    t.frame();
    // The caret at the start is the title's first letter (the `# ` isn't
    // shown).
    t.cx.simulate_input("Live ");
    t.pause();
    assert!(t.titles().contains(&"Live Awareness v2".to_string()), "{:?}", t.titles());
}

#[gpui::test]
fn opening_another_document_saves_the_one_left(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    t.type_at_end("Draft.");
    t.update(|v, window, cx| v.open_doc("README.md", window, cx));
    let writes = t.writes();
    assert_eq!(writes.len(), 1);
    assert_eq!(writes[0].0, DOC);
    assert!(writes[0].1.ends_with("Draft.\n"));
    assert_eq!(t.text(), "# Read me\n\nBody of README.md.\n");
}

#[gpui::test]
fn a_clean_document_reloads_in_place_when_it_changes_elsewhere(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    t.change_elsewhere("# Awareness v2\n\nRewritten by an agent.\n", "agent");
    assert_eq!(t.text(), "# Awareness v2\n\nRewritten by an agent.\n");
    assert!(!t.read(|v| v.changed_elsewhere()));
    assert!(!t.drawn("wiki-changed-elsewhere"));
    let now = chrono::Utc::now();
    assert_eq!(t.read(|v| v.updated_line(now)).as_deref(), Some("Updated by an agent · just now"));
    t.pause();
    assert!(t.writes().is_empty(), "a reload isn't an edit");
}

#[gpui::test]
fn our_own_saves_echo_leaves_the_editor_alone(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    t.type_at_end("One.");
    t.pause();
    t.type_at_end(" Two.");
    // The hub announces the first save while the second is typed.
    let wake = WikiWake::Changed {
        paths: vec![DOC.into()],
        source: "local".into(),
    };
    t.update(|v, window, cx| v.woke(vec![wake], window, cx));
    assert!(t.text().ends_with("One. Two.\n"));
    assert!(!t.read(|v| v.changed_elsewhere()));
}

#[gpui::test]
fn a_dirty_document_shows_the_changed_elsewhere_bar(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    t.type_at_end("Mine.");
    t.change_elsewhere("# Awareness v2\n\nTheirs.\n", "pull");
    assert!(t.read(|v| v.changed_elsewhere()));
    assert!(t.drawn("wiki-changed-elsewhere"));
    assert!(t.text().ends_with("Mine.\n"), "ours stays until the bar is answered");
    t.pause();
    assert!(t.writes().is_empty(), "nothing saves over theirs while the bar asks");

    t.click("wiki-reload");
    assert_eq!(t.text(), "# Awareness v2\n\nTheirs.\n");
    assert!(!t.drawn("wiki-changed-elsewhere"));
    t.pause();
    assert!(t.writes().is_empty());
}

#[gpui::test]
fn keep_mine_saves_over_the_change_elsewhere(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    t.type_at_end("Mine.");
    t.change_elsewhere("# Awareness v2\n\nTheirs.\n", "agent");
    t.click("wiki-keep-mine");
    let writes = t.writes();
    assert_eq!(writes.len(), 1);
    assert!(writes[0].1.ends_with("Mine.\n"));
    assert!(t.hub_text(DOC).ends_with("Mine.\n"));
    assert!(!t.drawn("wiki-changed-elsewhere"));
}

#[gpui::test]
fn a_refused_save_says_so_and_keeps_the_text(cx: &mut TestAppContext) {
    let mut hub = FakeHub::sample();
    hub.refuse_writes = Some("held for a conflict".into());
    let mut t = T::open(cx, hub);
    t.open_doc();
    t.type_at_end("Mine.");
    t.pause();
    assert!(t.drawn("wiki-doc-updated"));
    assert!(t.text().ends_with("Mine.\n"));
    assert!(!t.hub_text(DOC).ends_with("Mine.\n"));
}

#[gpui::test]
fn the_toolbar_names_the_last_change_and_its_menu_copies_the_path_and_lists_the_history(cx: &mut TestAppContext) {
    let mut hub = FakeHub::sample();
    hub.commits.insert(
        DOC.into(),
        vec![
            commit(Some(Author::Agent), "John", "2026-11-27T12:00:00Z", "Update awareness-v2.md"),
            commit(None, "Jane Doe", "2026-11-26T12:00:00Z", "Add awareness-v2.md"),
        ],
    );
    let mut t = T::open(cx, hub);
    t.open_doc();
    t.cx.run_until_parked();
    let at = chrono::DateTime::parse_from_rfc3339("2026-11-27T12:05:00Z").unwrap().with_timezone(&chrono::Utc);
    assert_eq!(t.read(|v| v.updated_line(at)).as_deref(), Some("Updated by an agent · 5m ago"));
    assert!(t.drawn("wiki-doc-updated"));

    t.click("wiki-doc-more");
    assert_eq!(t.read(|v| v.doc_menu().cloned()), Some(DocMenu::Actions));
    assert!(t.drawn("wiki-doc-menu-open-in-editor"));
    t.click("wiki-doc-menu-copy-path");
    assert_eq!(t.cx.read_from_clipboard().and_then(|c| c.text()).as_deref(), Some(DOC));
    assert_eq!(t.read(|v| v.doc_menu().cloned()), None);

    t.update(|v, window, cx| v.doc_action(DocAction::ShowHistory, window, cx));
    let Some(DocMenu::History(Some(Ok(commits)))) = t.read(|v| v.doc_menu().cloned()) else {
        panic!("the history is read");
    };
    assert_eq!(commits.len(), 2);
    assert!(t.drawn("wiki-history"));
    assert!(t.asked().iter().any(|r| matches!(r, WikiRequest::Log { limit, .. } if *limit > 1)));
}

#[gpui::test]
fn a_remote_wiki_has_no_open_in_editor(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.update(|v, _, _| v.local = false);
    assert!(t.read(|v| v.doc_action_refusal(DocAction::OpenInEditor)).is_some());
    assert!(t.read(|v| v.doc_action_refusal(DocAction::CopyPath)).is_none());
}

#[gpui::test]
fn open_at_waits_for_the_tree_and_refuses_a_missing_path(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.update(|v, window, cx| v.open_at("Plans/Archive/old.md", window, cx));
    assert_eq!(t.selected().as_deref(), Some("Plans/Archive/old.md"), "its folder opens");
    assert_eq!(t.text(), "# Old plan\n\nBody of Plans/Archive/old.md.\n");
    t.update(|v, window, cx| v.open_at("Plans/nope.md", window, cx));
    assert_eq!(t.read(|v| v.notice().map(str::to_string)).as_deref(), Some("There's no Plans/nope.md in this wiki."));
    assert_eq!(t.read(|v| v.open_path().map(str::to_string)).as_deref(), Some("Plans/Archive/old.md"));
}

#[gpui::test]
fn esc_in_the_document_hands_the_keys_to_the_tree(cx: &mut TestAppContext) {
    let mut t = T::open(cx, FakeHub::sample());
    t.open_doc();
    t.type_at_end("x");
    t.keys("escape");
    // (Focus changes are delivered with the next frame.)
    t.frame();
    let view = t.view.clone();
    assert!(t.cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
    // Leaving the editor saved it.
    assert_eq!(t.writes().len(), 1);
}

