//! The project tabs (`project-tabs`): the tab bar across the top of the
//! window (Figma `Tabs`, 2792:33339) over the sidebar, which no longer has a
//! titlebar row or a project switcher; one tab per open project in its saved
//! order; a click switches, the hovered tab's `×` closes; the `+` tab's menu
//! lists the projects that aren't open, then Add project; ⌘1 to ⌘9 pick the
//! tabs and holding ⌘ shows their badges.
//!
//! The window is the smoke test's headless one ([`Harness`], on `alpha`),
//! with a project catalog in memory (nothing reads the disk) and Reduce
//! Motion on, so the menu lands at once. Switching itself (and the close's
//! neighbor) is `app_window_tests`'s, where windows switch for real.

use std::rc::Rc;

use gpui::{point, px, size, Bounds, Modifiers, Pixels, TestAppContext};

use super::smoke::Harness;
use super::*;
use crate::projects::ProjectEntry;

const PROJECTS: [&str; 4] = ["alpha", "beta", "gamma", "delta"];

/// The window on `alpha`, with `open` recorded open and `saved` the tab
/// order its state kept.
fn open(cx: &mut TestAppContext, open: &[&str], saved: &[&str]) -> Harness {
    let mut h = Harness::open(cx);
    let catalog = Catalog::with(
        PROJECTS
            .iter()
            .map(|name| ProjectEntry {
                name: name.to_string(),
                // A display name unlike the id, to see which a tab shows.
                label: format!("{name} app"),
            })
            .collect(),
        Vec::new(),
    );
    catalog.set_open(open);
    h.cx.update(|_, cx| cx.set_global(catalog));
    let saved: Vec<String> = saved.iter().map(|s| s.to_string()).collect();
    h.window.update(&mut h.cx, |w, cx| {
        w.reduce_motion = Rc::new(|| true);
        w.tabs = ProjectTabs::new(saved);
        w.sync_tabs(cx);
    });
    h.draw();
    h
}

fn bounds(h: &mut Harness, selector: &'static str) -> Bounds<Pixels> {
    h.draw();
    h.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
}

fn drawn(h: &mut Harness, selector: &'static str) -> bool {
    h.draw();
    h.cx.debug_bounds(selector).is_some()
}

fn order(h: &Harness) -> Vec<String> {
    h.window.read_with(&h.cx, |w, _| w.tabs.order().to_vec())
}

#[gpui::test]
fn the_tab_bar_spans_the_window_over_the_sidebar(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta"], &[]);
    let viewport = h.cx.update(|window, _| window.viewport_size());
    let bar = bounds(&mut h, "tab-bar");
    assert_eq!(bar.origin, point(px(0.0), px(0.0)));
    assert_eq!(bar.size, size(viewport.width, px(52.0)), "full width, 52pt");

    // The toolbar beside the traffic lights, centered on them: two 32pt
    // buttons 4pt apart.
    let search = bounds(&mut h, "tab-bar-search");
    let toggle = bounds(&mut h, "tab-bar-sidebar");
    assert_eq!(search.origin, point(px(tab_bar::TOOLBAR_X), px(10.0)));
    assert_eq!(search.size, size(px(32.0), px(32.0)));
    assert_eq!(toggle.size, search.size);
    assert_eq!(toggle.left(), search.right() + px(4.0));
    assert_eq!(toggle.top(), search.top());

    // The tabs start 8pt past the toolbar's own 8pt end, after a 1pt rule,
    // run the bar's height, and sit a rule apart; then the 52pt `+`.
    let alpha = bounds(&mut h, "tab-alpha");
    let beta = bounds(&mut h, "tab-beta");
    let plus = bounds(&mut h, "tab-plus");
    assert_eq!(alpha.left(), toggle.right() + px(8.0 + 8.0 + 1.0));
    assert_eq!((alpha.top(), alpha.size.height), (px(0.0), px(52.0)));
    assert_eq!(beta.left(), alpha.right() + px(1.0));
    assert_eq!(plus.left(), beta.right() + px(1.0));
    assert_eq!(plus.size, size(px(52.0), px(52.0)));

    // The sidebar starts under the bar, the nav its first row: no titlebar
    // row, no project switcher.
    let sidebar = bounds(&mut h, "sidebar");
    assert_eq!(sidebar.top(), px(52.0));
    let orchestrator = bounds(&mut h, "sidebar-nav-Orchestrator");
    assert_eq!(orchestrator.top(), sidebar.top() + px(20.0), "the frame's 20pt over the nav");
    for gone in ["sidebar-project-title", "sidebar-search", "sidebar-collapse"] {
        assert!(h.cx.debug_bounds(gone).is_none(), "{gone} is gone");
    }
}

#[gpui::test]
fn hiding_the_sidebar_keeps_the_tab_bar_in_place(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta"], &[]);
    let bar = bounds(&mut h, "tab-bar");
    let alpha = bounds(&mut h, "tab-alpha");
    h.click_selector("tab-bar-sidebar");
    assert!(h.window.read_with(&h.cx, |w, _| w.collapsed));
    assert!(!drawn(&mut h, "sidebar"));
    assert_eq!(bounds(&mut h, "tab-bar"), bar);
    assert_eq!(bounds(&mut h, "tab-alpha"), alpha, "the tabs don't move");
    h.click_selector("tab-bar-sidebar");
    assert!(drawn(&mut h, "sidebar"));
    assert_eq!(bounds(&mut h, "tab-bar"), bar);
}

#[gpui::test]
fn one_tab_per_open_project_in_the_saved_order(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta", "gamma"], &["gamma", "alpha"]);
    // The saved order kept, beta (open, without a tab) after it.
    assert_eq!(order(&h), ["gamma", "alpha", "beta"]);
    let left = |h: &mut Harness, s| bounds(h, s).left();
    assert!(left(&mut h, "tab-gamma") < left(&mut h, "tab-alpha"));
    assert!(left(&mut h, "tab-alpha") < left(&mut h, "tab-beta"));
    assert!(!drawn(&mut h, "tab-delta"), "delta isn't open");
    // Saved with the window's state.
    let saved = h.window.read_with(&h.cx, |w, _| w.window_state.tabs.clone());
    assert_eq!(saved, order(&h));
    // Each tab says its project's display name; this window's is selected.
    let tabs = h.window.read_with(&h.cx, |w, cx| w.tab_props(cx));
    let shown: Vec<(&str, &str, bool)> = tabs.iter().map(|t| (t.name.as_str(), t.label.as_ref(), t.selected)).collect();
    assert_eq!(shown, [("gamma", "gamma app", false), ("alpha", "alpha", true), ("beta", "beta app", false)]);

    // Quit elsewhere: the tab goes. This window's own stays, open or not.
    h.cx.update(|_, cx| cx.global::<Catalog>().set_open(&["beta"]));
    h.window.update(&mut h.cx, |w, cx| w.sync_tabs(cx));
    assert_eq!(order(&h), ["alpha", "beta"]);
    assert!(!drawn(&mut h, "tab-gamma"));
}

#[gpui::test]
fn the_tab_order_survives_a_restart(cx: &mut TestAppContext) {
    let saved = WindowState {
        tabs: vec!["gamma".into(), "beta".into()],
        ..Default::default()
    };
    // A window built from the saved state keeps the order, its own
    // project at the end when the order lacked it.
    cx.update(|cx| {
        cx.set_global(crate::theme::Theme::resolve(
            shelbi_state::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        ));
        gpui_kit::init(cx);
    });
    let handle = cx.update(|cx| {
        cx.open_window(Default::default(), |window, cx| {
            cx.new(|cx| ShelbiWindow::build("alpha", saved.clone(), window, cx))
        })
        .unwrap()
    });
    let (tabs, state) = cx
        .update(|cx| handle.read_with(cx, |w, _| (w.tabs.order().to_vec(), w.window_state.tabs.clone())))
        .unwrap();
    assert_eq!(tabs, ["gamma", "beta", "alpha"]);
    assert_eq!(state, tabs, "and saves it back");
}

#[gpui::test]
fn clicking_a_tab_switches_to_its_project(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta"], &[]);
    h.take_calls();
    h.click_selector("tab-beta");
    assert_eq!(h.take_calls(), [HostCall::SwitchProject("beta".into())]);
}

#[gpui::test]
fn a_hovered_tab_shows_a_close_button_that_asks_first(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta"], &[]);
    assert!(!drawn(&mut h, "tab-close-beta"), "a dot at rest");
    let beta = bounds(&mut h, "tab-beta");
    h.cx.simulate_mouse_move(beta.center(), None, Modifiers::none());
    assert!(drawn(&mut h, "tab-close-beta"), "the × while hovered");
    assert!(!drawn(&mut h, "tab-close-alpha"));
    let close = bounds(&mut h, "tab-close-beta");
    assert!(beta.contains(&close.origin) && close.size.width == px(16.0), "in the dot's 16pt slot");

    h.take_calls();
    h.click_selector("tab-close-beta");
    assert!(h.cx.has_pending_prompt(), "closing stops agents: it asks");
    assert!(h.take_calls().is_empty(), "the click doesn't also switch to it");
    h.cx.simulate_prompt_answer("Cancel");
    h.cx.run_until_parked();
    assert_eq!(order(&h), ["alpha", "beta"], "cancelled: the tab stays");

    // Off the tab, the dot is back.
    h.cx.simulate_mouse_move(point(px(700.0), px(400.0)), None, Modifiers::none());
    assert!(!drawn(&mut h, "tab-close-beta"));
}

#[gpui::test]
fn the_plus_menu_lists_the_projects_that_arent_open(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta"], &[]);
    h.take_calls();
    h.click_selector("tab-plus");
    assert_eq!(h.take_calls(), [HostCall::OpenSwitcher], "the + is Switch Project's menu");
    h.window.update(&mut h.cx, |w, cx| w.open_switcher(cx));
    let plus = bounds(&mut h, "tab-plus");
    let menu = bounds(&mut h, "tab-menu");
    assert_eq!((menu.left(), menu.top()), (plus.left(), plus.bottom() + px(4.0)), "under the +");
    for listed in ["tab-menu-project-gamma", "tab-menu-project-delta", "tab-menu-add"] {
        assert_eq!(bounds(&mut h, listed).size.height, px(26.0), "{listed}");
    }
    for tabbed in ["tab-menu-project-alpha", "tab-menu-project-beta"] {
        assert!(!drawn(&mut h, tabbed), "{tabbed} has a tab");
    }
    // Picking one opens it in its tab (the switch adds it).
    h.click_selector("tab-menu-project-delta");
    assert_eq!(h.take_calls(), [HostCall::SwitchProject("delta".into())]);

    // The keyboard: Down then Enter is the second row; up from the top
    // wraps to Add project.
    h.window.update(&mut h.cx, |w, cx| {
        w.switcher.close(Instant::now());
        w.open_switcher(cx);
    });
    h.focus_window();
    h.key("down");
    h.key("enter");
    assert_eq!(h.take_calls(), [HostCall::SwitchProject("delta".into())]);
    h.window.update(&mut h.cx, |w, cx| {
        w.switcher.close(Instant::now());
        w.open_switcher(cx);
    });
    h.key("up");
    assert_eq!(h.window.read_with(&h.cx, |w, _| w.switcher.highlight), 2, "Add project");
    h.key("escape");
    assert!(!h.window.read_with(&h.cx, |w, _| w.switcher.is_open()));
}

#[gpui::test]
fn with_every_project_open_the_plus_menu_is_add_project(cx: &mut TestAppContext) {
    let mut h = open(cx, &PROJECTS, &[]);
    assert_eq!(order(&h), ["alpha", "beta", "delta", "gamma"]);
    h.window.update(&mut h.cx, |w, cx| w.open_switcher(cx));
    assert!(drawn(&mut h, "tab-menu-add"));
    for name in ["tab-menu-project-alpha", "tab-menu-project-gamma"] {
        assert!(!drawn(&mut h, name));
    }
    h.take_calls();
    h.click_selector("tab-menu-add");
    assert_eq!(h.take_calls(), [HostCall::OpenAddProject]);
}

#[gpui::test]
fn command_digits_pick_the_tabs_and_holding_command_shows_them(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha", "beta", "gamma"], &["beta", "alpha", "gamma"]);
    h.take_calls();
    h.key("cmd-1");
    assert_eq!(h.take_calls(), [HostCall::SwitchProject("beta".into())]);
    h.key("cmd-2");
    assert!(h.take_calls().is_empty(), "alpha shows already");
    h.key("cmd-3");
    assert_eq!(h.take_calls(), [HostCall::SwitchProject("gamma".into())]);
    h.key("cmd-4");
    assert!(h.take_calls().is_empty(), "no fourth tab");

    assert!(!drawn(&mut h, "tab-hotkey-beta"), "hidden at rest");
    h.cx.simulate_modifiers_change(Modifiers::command());
    for badge in ["tab-hotkey-beta", "tab-hotkey-alpha", "tab-hotkey-gamma"] {
        assert!(drawn(&mut h, badge), "{badge} while ⌘ is held");
    }
    let beta = bounds(&mut h, "tab-beta");
    assert!(beta.contains(&bounds(&mut h, "tab-hotkey-beta").origin), "inside its tab");
    // Another modifier with it is another chord: no badges.
    h.cx.simulate_modifiers_change(Modifiers::command_shift());
    assert!(!drawn(&mut h, "tab-hotkey-beta"));
    h.cx.simulate_modifiers_change(Modifiers::none());
    assert!(!drawn(&mut h, "tab-hotkey-beta"));
}

#[gpui::test]
fn a_users_own_control_digit_binding_stays_theirs(cx: &mut TestAppContext) {
    let _ = cx;
    let chord = shelbi_state::keymap::KeyChord::parse("ctrl-1").unwrap();
    let defaults = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
    assert_eq!(window_chord(&defaults, &chord), Some(NavChord::Row(1)));
    let (custom, diags) =
        shelbi_state::keymap::validate_keymaps_yaml("defaults:\n  global:\n    zen_toggle: ctrl-1\n", None);
    assert!(diags.is_empty(), "{diags:?}");
    assert_eq!(window_chord(&custom, &chord), None, "the keys.yaml binding wins");
    // A tab chord isn't the user's to take (keys.yaml has no ⌘ digits).
    let tab = shelbi_state::keymap::KeyChord::parse("super-1").unwrap();
    assert_eq!(window_chord(&custom, &tab), Some(NavChord::Tab(1)));
}

#[gpui::test]
fn a_project_opened_elsewhere_gets_a_tab_not_a_menu_row(cx: &mut TestAppContext) {
    let mut h = open(cx, &["alpha"], &[]);
    // Opened from another window or a terminal since the tabs last synced.
    h.cx.update(|_, cx| cx.global::<Catalog>().set_open(&["alpha", "gamma"]));
    h.window.update(&mut h.cx, |w, cx| w.open_switcher(cx));
    assert_eq!(order(&h), ["alpha", "gamma"]);
    assert!(drawn(&mut h, "tab-gamma"));
    assert!(!drawn(&mut h, "tab-menu-project-gamma"));
    assert!(drawn(&mut h, "tab-menu-project-beta") && drawn(&mut h, "tab-menu-project-delta"));
}
