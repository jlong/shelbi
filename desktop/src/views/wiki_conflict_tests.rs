//! The Wiki's conflict inspector (`wiki-conflicts`): a held document shows
//! a notice over the footer and a bar over the document; Resolve opens the
//! inspector, whose per-hunk choices (or Use all) Apply sends to the hub,
//! and the document reads the result.

use gpui::TestAppContext;
use shelbi_app::wiki::{Resolution, WikiRequest};
use shelbi_app::wiki_conflict::Side;
use shelbi_app::wiki_sub::WikiWake;

use super::tests::{FakeHub, T};

const DOC: &str = "Plans/awareness-v2.md";
const OURS: &str = "# Awareness v2\n\n## Alpha\n\nAlpha ours.\n\n## Beta\n\nBeta ours.\n";
const THEIRS: &str = "# Awareness v2\n\n## Alpha\n\nAlpha theirs.\n\n## Beta\n\nBeta theirs.\n";

fn held(cx: &mut TestAppContext) -> T {
    let mut hub = FakeHub::sample();
    hub.hold(DOC, OURS, THEIRS);
    T::open(cx, hub)
}

impl T {
    fn resolves(&mut self) -> Vec<Resolution> {
        self.asked()
            .into_iter()
            .filter_map(|r| match r {
                WikiRequest::Resolve { path, resolution, .. } => {
                    assert_eq!(path, DOC);
                    Some(resolution)
                }
                _ => None,
            })
            .collect()
    }

    fn held_text(&self, path: &str) -> String {
        self.hub.lock().unwrap().entries[path].2.clone()
    }

    fn editor_text(&mut self) -> String {
        let editor = self.read(|v| v.doc_editor()).expect("the editor shows");
        editor.read_with(&self.cx, |e, _| e.text().to_string())
    }
}

#[gpui::test]
fn a_held_document_shows_the_notice_and_the_bar(cx: &mut TestAppContext) {
    let mut t = held(cx);
    assert!(t.drawn("wiki-conflict-notice"));
    t.update(|v, window, cx| v.open_doc(DOC, window, cx));
    assert!(t.drawn("wiki-doc-held"));
    t.update(|v, window, cx| v.open_doc("README.md", window, cx));
    assert!(!t.drawn("wiki-doc-held"), "only the held document has the bar");
    assert!(t.drawn("wiki-conflict-notice"));
}

#[gpui::test]
fn per_hunk_choices_apply_and_the_document_reads_the_result(cx: &mut TestAppContext) {
    let mut t = held(cx);
    t.update(|v, window, cx| v.open_doc(DOC, window, cx));
    t.click("wiki-doc-held-resolve");
    assert_eq!(t.read(|v| v.inspecting().map(str::to_string)).as_deref(), Some(DOC));
    assert!(t.drawn("wiki-conflict"));
    assert!(!t.drawn("wiki-doc-body"), "the inspector stands in for the document");
    assert_eq!(t.read(|v| v.inspector().map(|i| i.hunks().len())), Some(2));
    assert!(t.drawn("wiki-conflict-hunk-0") && t.drawn("wiki-conflict-hunk-1"));

    // Apply waits for every hunk.
    t.asked();
    t.click("wiki-conflict-apply");
    assert!(t.resolves().is_empty());
    t.click("wiki-conflict-0-theirs");
    t.click("wiki-conflict-1-ours");
    assert_eq!(t.read(|v| v.inspector().unwrap().progress()), "2 of 2 chosen");
    t.click("wiki-conflict-apply");
    let sent = t.resolves();
    assert!(
        matches!(&sent[..], [Resolution::Hunks { choices, .. }] if choices == &[Side::Theirs, Side::Ours]),
        "{sent:?}"
    );
    let merged = "# Awareness v2\n\n## Alpha\n\nAlpha theirs.\n\n## Beta\n\nBeta ours.\n";
    assert_eq!(t.held_text(DOC), merged);
    // Closed, the notice gone, the document showing the result.
    assert_eq!(t.read(|v| v.inspecting().map(str::to_string)), None);
    assert!(!t.drawn("wiki-conflict-notice"));
    assert!(!t.drawn("wiki-doc-held"));
    assert_eq!(t.editor_text(), merged);
}

#[gpui::test]
fn use_all_theirs_and_cancel(cx: &mut TestAppContext) {
    let mut t = held(cx);
    // The footer's Resolve opens the first held document.
    t.click("wiki-conflict-notice-resolve");
    assert_eq!(t.read(|v| v.inspecting().map(str::to_string)).as_deref(), Some(DOC));
    t.click("wiki-conflict-cancel");
    assert_eq!(t.read(|v| v.inspecting().map(str::to_string)), None);
    assert!(t.resolves().is_empty(), "Cancel resolves nothing");

    t.click("wiki-conflict-notice-resolve");
    // A second click on a chosen side undoes it.
    t.click("wiki-conflict-0-ours");
    assert_eq!(t.read(|v| v.inspector().unwrap().choice(0)), Some(Side::Ours));
    t.click("wiki-conflict-0-ours");
    assert_eq!(t.read(|v| v.inspector().unwrap().choice(0)), None);
    t.click("wiki-conflict-all-theirs");
    t.click("wiki-conflict-apply");
    assert_eq!(t.held_text(DOC), THEIRS);
}

#[gpui::test]
fn a_refused_apply_says_why_and_a_change_reads_the_sides_again(cx: &mut TestAppContext) {
    let mut t = held(cx);
    t.click("wiki-conflict-notice-resolve");
    // Ours changed on the hub since the sides were read.
    t.hub.lock().unwrap().entries.get_mut(DOC).unwrap().2 = "# Awareness v2\n\nNew.\n".into();
    t.click("wiki-conflict-all-ours");
    t.click("wiki-conflict-apply");
    assert!(t.read(|v| v.conflict_error().map(str::to_string)).unwrap().contains("changed since"));
    assert!(t.drawn("wiki-conflict-error"));
    assert_eq!(t.read(|v| v.inspecting().map(str::to_string)).as_deref(), Some(DOC), "still open");

    let wake = WikiWake::Changed {
        paths: vec![DOC.into()],
        source: "local".into(),
    };
    t.update(|v, window, cx| v.woke(vec![wake], window, cx));
    let ours = t.read(|v| v.inspector().unwrap().conflict().ours.clone()).unwrap();
    assert!(ours.contains("New."), "{ours}");

    // Resolved elsewhere (the CLI): the next status closes the inspector.
    t.hub.lock().unwrap().conflicts.clear();
    t.hub.lock().unwrap().status.as_mut().unwrap().conflicts.clear();
    t.update(|v, window, cx| v.woke(vec![WikiWake::Status("synced".into())], window, cx));
    assert_eq!(t.read(|v| v.inspecting().map(str::to_string)), None);
}
