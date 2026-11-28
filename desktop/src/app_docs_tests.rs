//! The Documentation in the window (`docs-desktop-tree`): its nav row sits after
//! Issues with the open book; showing it pushes its tree into the leading
//! column in the sidebar's place (the push Settings and the task panels
//! use) with the document area beside it, and ‹ or Esc in the tree pops back
//! to the view it was opened over, the sidebar's keys again. Its open
//! folders and document are remembered with the project's view state.
//!
//! The window is the smoke test's headless one ([`Harness`]) with an
//! injected clock; the Documentation asks a fake hub ([`DocsPorts`]).

use std::cell::Cell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::smoke::Harness;
use super::*;
use crate::views::column_swap::SLIDE;
use crate::views::sidebar_reveal::REVEAL;
use crate::views::docs::tests::{ask_of, FakeHub};
use crate::views::docs::DocsPorts;

struct W {
    h: Harness,
    now: Rc<Cell<Instant>>,
    hub: Arc<Mutex<FakeHub>>,
}

impl W {
    fn open(cx: &mut TestAppContext) -> W {
        cx.update(|cx| cx.set_global(TerminalSettings::default()));
        let hub = Arc::new(Mutex::new(FakeHub::sample()));
        let ports = DocsPorts {
            ask: ask_of(&hub),
            store: shelbi_app::settings::store::Disk::store("alpha"),
        };
        cx.update(|cx| cx.set_global(ports));
        let mut h = Harness::open(cx);
        let now = Rc::new(Cell::new(Instant::now()));
        let n = now.clone();
        h.window.update(&mut h.cx, |w, _| {
            w.clock = Rc::new(move || n.get());
            w.reduce_motion = Rc::new(|| false);
            w.client.set_view(View::Issues);
            // The shipped nav (the smoke fixture's predates the Documentation row).
            w.model.nav = shelbi_app::view::default_nav_items();
        });
        h.focus_window();
        h.take_calls();
        W { h, now, hub }
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>) -> R) -> R {
        let window = self.h.window.clone();
        let r = self.h.cx.update(|win, cx| window.update(cx, |w, cx| f(w, win, cx)));
        self.h.cx.run_until_parked();
        r
    }

    fn read<R>(&self, f: impl FnOnce(&ShelbiWindow, &App) -> R) -> R {
        self.h.window.read_with(&self.h.cx, f)
    }

    fn settle(&mut self) {
        self.h.draw();
        self.now.set(self.now.get() + SLIDE.max(REVEAL) + Duration::from_millis(1));
        self.h.draw();
    }

    /// Show the Documentation as its nav row does (the tap records commands, so the
    /// view is shown directly).
    fn show_docs(&mut self) {
        self.update(|w, window, cx| w.perform(HostCall::ShowView(View::Docs), window, cx));
        self.settle();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.h.cx.debug_bounds(selector).is_some()
    }
}

#[gpui::test]
fn the_docs_row_sits_after_issues_with_the_open_book(cx: &mut TestAppContext) {
    let mut w = W::open(cx);
    w.settle();
    let issues = w.h.cx.debug_bounds("sidebar-nav-Issues").expect("Issues' row");
    let docs = w.h.cx.debug_bounds("sidebar-nav-Documentation").expect("the Documentation's row");
    let changelog = w.h.cx.debug_bounds("sidebar-nav-Changelog").expect("the Changelog's row");
    assert!(issues.bottom() <= docs.top() && docs.bottom() <= changelog.top());
    assert_eq!(crate::views::sidebar::nav_icon(&View::Docs), crate::assets::icons::BOOK_OPEN);
    // A click on it runs Show Documentation, as the other rows run theirs.
    w.h.click_selector("sidebar-nav-Documentation");
    assert!(w.h.take_calls().contains(&HostCall::ShowView(View::Docs)));
}

#[gpui::test]
fn showing_the_docs_pushes_its_tree_and_back_pops_it(cx: &mut TestAppContext) {
    for leave in ["back", "escape"] {
        let mut w = W::open(cx);
        w.settle();
        let column_w = w.h.cx.debug_bounds("leading-column").expect("the column").size.width;
        w.show_docs();
        assert_eq!(w.read(|w, _| w.column.target()), Occupant::Docs, "{leave}");
        assert!(w.drawn("column-docs") && w.drawn("docs-sidebar"), "{leave}");
        assert!(!w.drawn("sidebar"), "{leave}: the app sidebar is out of the column");
        assert_eq!(w.h.cx.debug_bounds("leading-column").unwrap().size.width, column_w);
        assert!(w.drawn("docs-main") && w.drawn("docs-row-Plans"), "{leave}");
        // The tree has the keys.
        let focus = w.read(|w, cx| w.docs.as_ref().unwrap().read(cx).focus_handle().clone());
        assert!(w.h.cx.update(|window, _| focus.is_focused(window)), "{leave}");
        w.h.key("down");
        let selected = w.read(|w, cx| w.docs.as_ref().unwrap().read(cx).tree().selected_path().map(str::to_string));
        assert_eq!(selected.as_deref(), Some("Plans/Archive"), "{leave}");

        if leave == "back" {
            w.h.click_selector("docs-back");
        } else {
            w.h.key("escape");
        }
        w.settle();
        assert_eq!(w.h.take_calls(), vec![HostCall::ShowView(View::Issues)], "{leave}");
        assert!(w.drawn("sidebar") && !w.drawn("column-docs"), "{leave}: the app sidebar is back");
        assert_eq!(w.read(|w, _| w.client.focus()), Focus::Sidebar, "{leave}");
    }
}

#[gpui::test]
fn leaving_the_docs_stops_its_reads_and_its_state_is_remembered(cx: &mut TestAppContext) {
    let mut w = W::open(cx);
    w.show_docs();
    w.update(|w, window, cx| {
        let docs = w.docs.clone().unwrap();
        docs.update(cx, |v, cx| v.reveal("Plans/Archive/old.md", window, cx));
    });
    let (_, state) = w.read(|w, cx| w.view_state_to_save(cx)).expect("a view state");
    assert_eq!(state.view.as_deref(), Some("docs"));
    assert_eq!(state.docs_document.as_deref(), Some("Plans/Archive/old.md"));
    assert!(state.docs_expanded.contains(&"Plans/Archive".to_string()), "{:?}", state.docs_expanded);
    w.update(|w, window, cx| w.perform(HostCall::ShowView(View::Issues), window, cx));
    assert!(!w.read(|w, cx| w.docs.as_ref().unwrap().read(cx).is_shown()));

    // A Documentation built from a saved state opens where it was left.
    w.update(|w, _, _| {
        w.docs = None;
        w.docs_saved = (vec!["Plans".into(), "Plans/Archive".into()], Some("Plans/Archive/old.md".into()));
    });
    w.show_docs();
    let open = w.read(|w, cx| w.docs.as_ref().unwrap().read(cx).open_path().map(str::to_string));
    assert_eq!(open.as_deref(), Some("Plans/Archive/old.md"));
    assert!(w.drawn("docs-breadcrumb"));
    assert!(w.hub.lock().unwrap().entries.contains_key("Plans/Archive/old.md"));
}

#[gpui::test]
fn the_empty_state_sets_the_docs_up_in_one_click(cx: &mut TestAppContext) {
    use crate::views::workflows_state::tests::Home;
    let _home = Home::with_project("docs-setup", "alpha");
    let mut w = W::open(cx);
    {
        let mut hub = w.hub.lock().unwrap();
        hub.status = Some(shelbi_app::docs::DocsStatus::not_set_up());
    }
    w.show_docs();
    for _ in 0..3 {
        w.h.cx.run_until_parked();
        w.settle();
    }
    assert!(w.drawn("docs-setup") && w.drawn("docs-setup-branch") && w.drawn("docs-setup-folder"));
    assert!(shelbi_state::load_project("alpha").unwrap().docs.is_none());
    // The hub sets it up as Init's answer says.
    {
        let hub = w.hub.clone();
        let ask: shelbi_app::settings::docs::Ask = Arc::new(move |req: shelbi_app::docs::DocsRequest| {
            let mut h = hub.lock().unwrap();
            if matches!(req, shelbi_app::docs::DocsRequest::Init) {
                let mut s = FakeHub::sample().status.unwrap();
                s.set_up = true;
                h.status = Some(s);
            }
            h.answer_for_test(req)
        });
        w.update(|w, _, cx| {
            w.docs.as_ref().unwrap().update(cx, |v, _| v.set_ask_for_test(ask));
        });
    }
    w.h.click_selector("docs-setup-branch");
    for _ in 0..3 {
        w.h.cx.run_until_parked();
        w.settle();
    }
    let block = shelbi_state::load_project("alpha").unwrap().docs.expect("the docs: block");
    assert_eq!(
        (block.source, block.branch.as_deref()),
        (shelbi_core::DocsSource::Branch, Some("docs/planning"))
    );
    assert!(w.hub.lock().unwrap().asked.contains(&shelbi_app::docs::DocsRequest::Init));
    assert!(!w.drawn("docs-setup"), "the tree takes over once it's set up");
    assert!(w.drawn("docs-row-Plans"));
}

#[gpui::test]
fn a_docs_link_runs_open_doc(cx: &mut TestAppContext) {
    let mut w = W::open(cx);
    w.settle();
    w.update(|_, window, cx| {
        crate::links::open("docs:Plans/awareness-v2.md", window, cx);
    });
    assert_eq!(w.h.take_calls(), [HostCall::OpenDoc("Plans/awareness-v2.md".into())]);
    // A task body written before the rename links with `wiki:`: the same.
    w.update(|_, window, cx| {
        crate::links::open("wiki:Plans/awareness-v2.md", window, cx);
    });
    assert_eq!(w.h.take_calls(), [HostCall::OpenDoc("Plans/awareness-v2.md".into())]);
}

#[gpui::test]
fn open_doc_shows_the_docs_at_the_document(cx: &mut TestAppContext) {
    let mut w = W::open(cx);
    w.settle();
    // From another view: the Documentation view is built, then its tree read, and the
    // document opens once it is.
    w.update(|w, window, cx| w.perform(HostCall::OpenDoc("Plans/Archive/old.md".into()), window, cx));
    for _ in 0..3 {
        w.h.cx.run_until_parked();
        w.settle();
    }
    assert_eq!(w.read(|w, _| w.client.view().clone()), View::Docs);
    let (open, selected) = w.read(|w, cx| {
        let v = w.docs.as_ref().unwrap().read(cx);
        (v.open_path().map(str::to_string), v.tree().selected_path().map(str::to_string))
    });
    assert_eq!(open.as_deref(), Some("Plans/Archive/old.md"));
    assert_eq!(selected.as_deref(), Some("Plans/Archive/old.md"), "its folder opens to show it");
    assert!(w.drawn("docs-doc-body"));
    // Again while it shows: straight there.
    w.update(|w, window, cx| w.perform(HostCall::OpenDoc("README.md".into()), window, cx));
    w.settle();
    let open = w.read(|w, cx| w.docs.as_ref().unwrap().read(cx).open_path().map(str::to_string));
    assert_eq!(open.as_deref(), Some("README.md"));
}

#[gpui::test]
fn the_palette_finds_docs_documents_by_title_and_text(cx: &mut TestAppContext) {
    use shelbi_app::overlay::palette_search::SearchFilter;
    let mut w = W::open(cx);
    w.settle();
    w.h.focus_window();
    w.h.key("ctrl-space");
    w.h.draw();
    // A title.
    w.h.key("o p e n c o d e");
    w.h.draw();
    let docs = w.h.palette(|p| p.chips().iter().find(|c| c.filter == SearchFilter::Docs).map(|c| c.count));
    assert_eq!(docs, Some(1));
    assert!(w.drawn("palette-heading-Documentation"));
    assert!(w.drawn("palette-docs-Plans/opencode-integration.md"));
    w.h.click_selector("palette-docs-Plans/opencode-integration.md");
    assert!(!w.h.overlay_open());
    assert_eq!(w.h.take_calls(), [HostCall::OpenDoc("Plans/opencode-integration.md".into())]);

    // The text: a line no title holds.
    w.hub.lock().unwrap().entries.get_mut("README.md").unwrap().2 = "# Read me\n\nThe merge flow, step by step.\n".into();
    w.h.key("ctrl-space");
    w.h.draw();
    w.h.key("s t e p space b y");
    w.h.draw();
    let detail = w.h.palette(|p| {
        p.rows()
            .iter()
            .find_map(|r| match r.item() {
                Some(shelbi_app::overlay::palette_search::ResultItem::Docs(h)) => Some((h.path.clone(), h.detail.clone())),
                _ => None,
            })
    });
    assert_eq!(detail, Some(("README.md".to_string(), "Documentation · The merge flow, step by step.".to_string())));
}
