//! The Figma sidebar's window behavior (`dt-figma-sidebar`): the sidebar's
//! chords (⌘O ⌘I ⌘L open the nav views, ⌘1 to ⌘9 the numbered rows, even
//! from a focused terminal), the sections collapsing and expanding and the
//! project remembering it, the Zen pill, and the titlebar's Search and
//! Toggle sidebar buttons.
//!
//! The window is the smoke test's headless one ([`Harness`]): one review
//! (`T-9`, queued) and one working workspace (`alpha-1`), so `T-9` is ⌘1 and
//! `alpha-1` is ⌘2.

use gpui::TestAppContext;

use super::smoke::Harness;
use super::*;

fn calls_of(kind: CommandKind) -> Vec<HostCall> {
    let mut calls = Vec::new();
    commands::dispatch_command(&kind, &mut calls);
    calls
}

fn open(cx: &mut TestAppContext) -> Harness {
    let mut h = Harness::open(cx);
    h.focus_window();
    h.take_calls();
    h
}

#[gpui::test]
fn the_nav_chords_open_their_views(cx: &mut TestAppContext) {
    let mut h = open(cx);
    for (keystroke, view) in [
        ("cmd-o", View::Session("orch".into())),
        ("cmd-i", View::Issues),
        ("cmd-l", View::Activity),
    ] {
        h.key(keystroke);
        assert_eq!(h.take_calls(), calls_of(CommandKind::ShowView(view)), "{keystroke}");
    }
}

#[gpui::test]
fn the_numbered_chords_open_their_rows(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.key("cmd-1");
    assert_eq!(
        h.take_calls(),
        calls_of(CommandKind::LoadReview {
            task_id: "T-9".into()
        })
    );
    h.key("cmd-2");
    assert_eq!(
        h.take_calls(),
        calls_of(CommandKind::FocusWorkspace {
            workspace: "alpha-1".into()
        })
    );
    let selected = h.window.read_with(&h.cx, |w, cx| w.selected(cx));
    assert_eq!(selected, Some(SidebarTarget::Workspace("alpha-1".into())), "its row is selected");
    // Nothing is numbered 3.
    h.key("cmd-3");
    assert!(h.take_calls().is_empty());
}

#[gpui::test]
fn a_collapsed_sections_chord_still_opens_its_row(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.click_selector("sidebar-section-Workspaces");
    h.draw();
    assert!(h.cx.debug_bounds("sidebar-ws-alpha-1").is_none());
    h.key("cmd-2");
    assert_eq!(
        h.take_calls(),
        calls_of(CommandKind::FocusWorkspace {
            workspace: "alpha-1".into()
        })
    );
}

#[gpui::test]
fn the_window_takes_the_sidebar_chords_from_a_terminal(cx: &mut TestAppContext) {
    let h = open(cx);
    let keymaps = h.window.read_with(&h.cx, |w, _| w.keymaps.clone());
    for spec in ["super-o", "super-i", "super-l", "super-1", "super-9"] {
        let chord = shelbi_state::keymap::KeyChord::parse(spec).unwrap();
        assert!(takes_from_terminal(&keymaps, chord), "{spec}");
    }
    let plain = shelbi_state::keymap::KeyChord::parse("super-p").unwrap();
    assert!(!takes_from_terminal(&keymaps, plain));
}

#[gpui::test]
fn a_section_collapses_expands_and_is_remembered(cx: &mut TestAppContext) {
    let mut h = open(cx);
    assert!(h.cx.debug_bounds("sidebar-review-T-9").is_some());
    h.click_selector("sidebar-section-Queued for Review");
    h.draw();
    assert!(h.cx.debug_bounds("sidebar-review-T-9").is_none(), "collapsed");
    assert!(h.cx.debug_bounds("sidebar-section-Queued for Review").is_some(), "the header stays");

    // The project's view state carries it, and a window adopting that state
    // draws the section collapsed.
    let saved = h
        .window
        .read_with(&h.cx, |w, cx| w.view_state_to_save(cx))
        .map(|(_, s)| s)
        .expect("a project is shown");
    assert_eq!(saved.collapsed_sections, ["queued"]);

    h.click_selector("sidebar-section-Queued for Review");
    h.draw();
    assert!(h.cx.debug_bounds("sidebar-review-T-9").is_some(), "expanded again");

    h.window.update(&mut h.cx, |w, cx| {
        w.adopt_view_state(saved);
        cx.notify();
    });
    h.draw();
    assert!(h.cx.debug_bounds("sidebar-review-T-9").is_none(), "restored collapsed");
}

#[gpui::test]
fn collapsing_a_section_keeps_the_selection_on_its_row(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.key("cmd-2");
    h.take_calls();
    // Collapse the review section above the selected workspace.
    h.click_selector("sidebar-section-Queued for Review");
    let selected = h.window.read_with(&h.cx, |w, cx| w.selected(cx));
    assert_eq!(selected, Some(SidebarTarget::Workspace("alpha-1".into())));
}

#[gpui::test]
fn the_zen_pill_toggles_zen(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.click_selector("sidebar-zen");
    assert_eq!(h.take_calls(), calls_of(CommandKind::ToggleZen));
}

#[gpui::test]
fn the_titlebar_buttons_search_and_toggle_the_sidebar(cx: &mut TestAppContext) {
    let mut h = open(cx);
    h.click_selector("sidebar-search");
    assert!(h.window.read_with(&h.cx, |w, _| w.overlay.is_some()), "Search opens the palette");
    h.window.update(&mut h.cx, |w, cx| {
        w.overlay = None;
        cx.notify();
    });

    // Instant here: the slide itself is `app_column_tests`'s.
    h.window.update(&mut h.cx, |w, _| w.reduce_motion = std::rc::Rc::new(|| true));
    h.click_selector("sidebar-collapse");
    h.draw();
    assert!(h.window.read_with(&h.cx, |w, _| w.collapsed));
    assert!(h.cx.debug_bounds("sidebar").is_none(), "the column is hidden");
    // The toggle stays reachable, in the main area.
    h.click_selector("expand-sidebar");
    h.draw();
    assert!(!h.window.read_with(&h.cx, |w, _| w.collapsed));
    assert!(h.cx.debug_bounds("sidebar").is_some());
}

#[gpui::test]
fn the_go_menu_names_changelog_with_the_sidebar_chords(cx: &mut TestAppContext) {
    use crate::menus::tree::{self, MenuEntry, WindowFacts};
    let h = open(cx);
    let model = h.window.read_with(&h.cx, |w, _| w.command_model());
    let menus = tree::build(&model, &Keymaps::default(), WindowFacts::default());
    let go = menus.menus.iter().find(|m| m.title == "Go").expect("a Go menu");
    let item = |title: &str| {
        go.items
            .iter()
            .find_map(|e| match e {
                MenuEntry::Item(i) if i.title == title => Some(i.clone()),
                _ => None,
            })
            .unwrap_or_else(|| panic!("Go ▸ {title}"))
    };
    assert_eq!(item("Orchestrator").shortcut.map(|s| s.display).as_deref(), Some("⌘O"));
    assert_eq!(item("Issues").shortcut.map(|s| s.display).as_deref(), Some("⌘I"));
    assert_eq!(item("Changelog").shortcut.map(|s| s.display).as_deref(), Some("⌘L"));
}
