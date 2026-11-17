//! The New tab (`new-tab-launch`): the tab bar's `+` adds a "New tab" whose
//! content is the launch screen ([`LaunchView::in_tab`], the launch window's
//! own view), a second `+` selects it, and what is picked there becomes
//! that tab: a project opens in its place, one with a tab already (here or
//! in another window) comes up there and the New tab goes, the wizard's
//! new project lands in it, and a remote session opens its own window. It
//! closes (its `×`, ⌘W), is never saved, and drags and tears off like a
//! project's tab.
//!
//! Windows are the window tests' inert ones ([`super::window_tests`]); the
//! launch screen reads a canned model ([`TestDeps`]) and starts a fake
//! daemon, so nothing runs or reaches the real Shelbi home.

use std::sync::Arc;

use gpui::{point, px, Modifiers, TestAppContext};
use shelbi_app::launch::{FakeDaemon, LaunchModel};
use shelbi_app::overlay::palette::ProjectIndicator;

use super::window_tests::{
    bounds_in, draw, fixture, open, open_at, press_and_start, read, release_at, shown, tabs_of, tearoff_fixture,
    update, vcx, windows, Fixture,
};
use super::*;
use crate::views::launch::tests::{project, session, Home};
use crate::views::launch::{Deps, Host, TestDeps};

/// The launch screen's rows (alpha, beta, gamma) and a daemon that only
/// counts its starts, for every launch view a test opens.
fn launch_deps(cx: &mut TestAppContext) -> Arc<FakeDaemon> {
    let daemon = Arc::new(FakeDaemon::new());
    let deps = Deps {
        load: Arc::new(|recents| LaunchModel {
            projects: vec![project("alpha"), project("beta"), project("gamma")],
            remotes: recents.load(),
        }),
        daemon: Rc::new({
            let daemon = daemon.clone();
            move |_| daemon.clone()
        }),
        // A New tab sends its picks to its window instead.
        open: Rc::new(|slug, _| panic!("the launch window's open ran for {slug}")),
        add: Rc::new(|_| panic!("the launch window's Add project ran")),
        remote: Rc::new(|_, _| panic!("the launch window's remote open ran")),
    };
    cx.update(|cx| cx.set_global(TestDeps(deps)));
    daemon
}

fn setup(cx: &mut TestAppContext) -> Fixture {
    let f = fixture(cx);
    launch_deps(cx);
    f
}

/// Tab ids as the keys a window saves.
fn keys(ids: &[&str]) -> Vec<ProjectKey> {
    ids.iter().map(|id| crate::project_tabs::tab_key(id)).collect()
}

fn on_new_tab(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>) -> bool {
    read(cx, h, |w, _| w.on_new_tab())
}

fn click(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>, selector: &'static str) {
    let at = bounds_in(cx, h, selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
    vcx(cx, h).simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    draw(cx, h);
}

fn drawn(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>, selector: &'static str) -> bool {
    bounds_in(cx, h, selector).is_some()
}

/// The window's tabs after `beta` and the `+`: alpha, beta, New tab.
fn with_new_tab(cx: &mut TestAppContext, f: &Fixture) -> WindowHandle<ShelbiWindow> {
    let h = open(cx, f, "alpha", 0);
    update(cx, h, |w, window, cx| w.switch_to("beta", window, cx));
    draw(cx, h);
    click(cx, h, "tab-plus");
    h
}

fn chose(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>, choice: NewTabChoice) {
    cx.update(|cx| {
        let weak = h.entity(cx).unwrap().downgrade();
        new_tab_chose(choice, *h, weak, cx);
    });
    cx.run_until_parked();
    // Unless it closed.
    if cx.update(|cx| h.read(cx).is_ok()) {
        draw(cx, h);
    }
}

#[gpui::test]
fn plus_opens_a_new_tab_showing_the_launch_screen(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", NEW_TAB], "after the last tab");
    assert!(on_new_tab(cx, h), "and selected");
    let tab = read(cx, h, |w, cx| w.tab_props(cx).into_iter().find(|t| t.name == NEW_TAB).unwrap());
    assert_eq!((tab.label.as_ref(), tab.selected, tab.working), ("New tab", true, false));
    assert!(drawn(cx, h, "tab-new-tab"));
    assert_eq!(tab_bar::indicator(&tab.name, tab.working), ProjectIndicator::Unloaded, "a gray ring");

    // The launch screen fills the window under the bar: no sidebar.
    for screen in ["new-tab", "launch", "launch-welcome", "launch-projects", "launch-add-project", "launch-connect-remote"] {
        assert!(drawn(cx, h, screen), "{screen}");
    }
    assert!(!drawn(cx, h, "sidebar"));
    let bar = bounds_in(cx, h, "tab-bar").unwrap();
    assert_eq!(bounds_in(cx, h, "launch").unwrap().top(), bar.bottom());
    let launch = read(cx, h, |w, _| w.new_tab.clone()).unwrap();
    assert_eq!(cx.update(|cx| launch.read(cx).host()), Host::Tab, "the launch window's own view");
    assert!(read(cx, h, |w, _| w.command_model().project.is_none()), "no project");

    // A second `+` selects it: never a second New tab.
    click(cx, h, "tab-plus");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", NEW_TAB]);
    assert!(on_new_tab(cx, h));
}

#[gpui::test]
fn the_new_tab_takes_the_launch_screens_keys(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    let launch = read(cx, h, |w, _| w.new_tab.clone()).unwrap();
    let mut v = vcx(cx, h);
    v.simulate_keystrokes("down down");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| launch.read(cx).selected()), 2, "↓ walks the rows");
    let mut v = vcx(cx, h);
    v.simulate_keystrokes("up");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| launch.read(cx).selected()), 1);
}

#[gpui::test]
fn another_tab_picked_keeps_the_new_tab_to_come_back_to(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    update(cx, h, |w, window, cx| w.switch_to("alpha", window, cx));
    draw(cx, h);
    assert_eq!(shown(cx, h), "alpha");
    assert!(!on_new_tab(cx, h));
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", NEW_TAB], "it stays");
    assert!(drawn(cx, h, "sidebar") && !drawn(cx, h, "launch"));
    // Its tab (a click, ⌘3) brings the launch screen back.
    click(cx, h, "tab-new-tab");
    assert!(on_new_tab(cx, h));
    assert!(drawn(cx, h, "launch"));
    update(cx, h, |w, window, cx| w.switch_to("alpha", window, cx));
    update(cx, h, |w, window, cx| w.select_tab(3, window, cx));
    assert!(on_new_tab(cx, h));
}

#[gpui::test]
fn a_project_picked_on_the_launch_screen_opens_in_the_tabs_place(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let daemon = launch_deps(cx);
    let _home = Home::new("new-tab-pick", &["alpha", "beta", "gamma"]);
    let h = open(cx, &f, "alpha", 0);
    update(cx, h, |w, window, cx| w.switch_to("beta", window, cx));
    click(cx, h, "tab-plus");
    // The New tab between alpha and beta.
    update(cx, h, |w, _, cx| {
        w.tabs.move_to(NEW_TAB, 1);
        w.tabs_changed(cx);
    });
    // gamma, the launch screen's third row.
    click(cx, h, "launch-project-2");
    assert_eq!(daemon.starts(), 1, "the daemon is started first, as from the launch window");
    assert_eq!(shown(cx, h), "gamma");
    assert_eq!(tabs_of(cx, h), ["alpha", "gamma", "beta"], "in the New tab's place");
    assert!(!on_new_tab(cx, h));
    assert_eq!(windows(cx).len(), 1, "in this window");
    assert_eq!(read(cx, h, |w, _| w.window_state.tabs.clone()), keys(&["alpha", "gamma", "beta"]));
}

#[gpui::test]
fn a_project_with_a_tab_here_is_selected_and_the_new_tab_goes(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    update(cx, h, |w, window, cx| w.switch_to("alpha", window, cx));
    update(cx, h, |w, window, cx| w.open_new_tab(window, cx));
    chose(cx, h, NewTabChoice::Project("beta".into()));
    assert_eq!(shown(cx, h), "beta");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta"]);
    assert!(!on_new_tab(cx, h));
}

#[gpui::test]
fn a_project_in_another_window_comes_up_there_and_the_new_tab_goes(cx: &mut TestAppContext) {
    // Windows close as the app's do (the shell forgets them).
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    launch_deps(cx);
    let a = open(cx, &f, "alpha", 0);
    update(cx, a, |w, window, cx| w.switch_to("beta", window, cx));
    update(cx, a, |w, window, cx| w.switch_to("alpha", window, cx));
    let b = open(cx, &f, "gamma", 1);
    update(cx, b, |w, window, cx| w.open_new_tab(window, cx));
    chose(cx, b, NewTabChoice::Project("beta".into()));
    assert_eq!(shown(cx, a), "beta", "the window with its tab comes up on it");
    assert_eq!(tabs_of(cx, a), ["alpha", "beta"]);
    assert_eq!(tabs_of(cx, b), ["gamma"], "the New tab went");
    assert_eq!(shown(cx, b), "gamma", "its neighbor shows");

    // A window holding only the New tab closes: what was picked is up
    // elsewhere.
    update(cx, b, |w, window, cx| w.open_new_tab(window, cx));
    update(cx, b, |w, _, cx| {
        w.tabs.remove("gamma");
        w.tabs_changed(cx);
    });
    chose(cx, b, NewTabChoice::Project("alpha".into()));
    assert_eq!(shown(cx, a), "alpha");
    let open: Vec<String> = windows(cx).into_iter().map(|(p, _)| p).collect();
    assert_eq!(open, ["alpha"], "{open:?}");
}

#[gpui::test]
fn the_wizards_new_project_opens_in_the_new_tab(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    update(cx, h, |w, _, cx| {
        w.tabs.move_to(NEW_TAB, 0);
        w.tabs_changed(cx);
    });
    update(cx, h, |w, window, cx| w.project_added("gamma", window, cx));
    assert_eq!(shown(cx, h), "gamma");
    assert_eq!(tabs_of(cx, h), ["gamma", "alpha", "beta"], "in the New tab's place");
}

#[gpui::test]
fn a_remote_session_that_opens_elsewhere_closes_the_new_tab(cx: &mut TestAppContext) {
    // No host link (the session is only recorded, as the terminal window
    // would open it): the New tab it was picked on goes. One that opens
    // natively takes the New tab's place (`app_mixed_tabs_tests`).
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    crate::views::remote_session::tests::OPENED.with(|o| o.borrow_mut().clear());
    chose(cx, h, NewTabChoice::Remote(session("web")));
    let opened = crate::views::remote_session::tests::OPENED.with(|o| o.borrow().clone());
    assert_eq!(opened, [session("web")], "as from the launch window");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta"], "the New tab went");
    assert_eq!(shown(cx, h), "beta");
}

#[gpui::test]
fn the_new_tab_closes_with_its_x_or_command_w(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    // Its `×`, hovered: no prompt, nothing quits; its left neighbor shows.
    let tab = bounds_in(cx, h, "tab-new-tab").unwrap();
    vcx(cx, h).simulate_mouse_move(tab.center(), None, Modifiers::none());
    draw(cx, h);
    click(cx, h, "tab-close-new-tab");
    assert!(!cx.has_pending_prompt(), "nothing to confirm");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta"]);
    assert_eq!(shown(cx, h), "beta");

    // ⌘W on it closes the tab, not the window.
    update(cx, h, |w, window, cx| w.open_new_tab(window, cx));
    update(cx, h, |w, window, cx| w.close_window(window, cx));
    assert_eq!(tabs_of(cx, h), ["alpha", "beta"]);
    assert_eq!(windows(cx).len(), 1);

    // The only tab: the chooser, as when the last project closes.
    update(cx, h, |w, window, cx| w.open_new_tab(window, cx));
    update(cx, h, |w, _, cx| {
        w.tabs = ProjectTabs::new([NEW_TAB.to_string()]);
        w.tabs_changed(cx);
    });
    update(cx, h, |w, window, cx| w.sidebar_event(SidebarEvent::CloseTab(NEW_TAB.into()), window, cx));
    assert!(read(cx, h, |w, _| w.chooser && w.new_tab.is_none()));
    assert!(drawn(cx, h, "chooser"));
}

#[gpui::test]
fn the_new_tab_is_never_saved(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    assert_eq!(read(cx, h, |w, _| w.window_state.tabs.clone()), keys(&["alpha", "beta"]));
    // Nor does a refresh drop it (the open set never names it).
    update(cx, h, |w, _, cx| {
        cx.global::<Catalog>().set_open(&["alpha", "beta"]);
        w.sync_tabs(cx);
    });
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", NEW_TAB]);
    // Close All Other Projects counts projects only.
    let others = read(cx, h, |w, _| w.command_model().open_others.len());
    assert_eq!(others, 0, "no project shows");
    update(cx, h, |w, window, cx| w.switch_to("alpha", window, cx));
    let others: Vec<String> =
        read(cx, h, |w, _| w.command_model().open_others.into_iter().map(|p| p.slug).collect());
    assert_eq!(others, ["beta"]);
}

#[gpui::test]
fn a_torn_off_new_tab_becomes_a_window_showing_the_launch_screen(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta"]);
    launch_deps(cx);
    let a = open_at(cx, &f, "alpha", 0, super::window_tests::frame(0.0, 0.0), &["alpha", "beta"]);
    update(cx, a, |w, window, cx| w.open_new_tab(window, cx));
    draw(cx, a);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta", NEW_TAB]);
    press_and_start(cx, a, "tab-new-tab");
    let away = point(px(450.0), px(52.0 + 300.0));
    super::window_tests::move_to(cx, a, away);
    release_at(cx, a, away);
    let handles: Vec<WindowHandle<ShelbiWindow>> =
        cx.update(|cx| cx.global::<Shell>().windows.iter().map(|w| w.handle).collect());
    assert_eq!(handles.len(), 2);
    let b = handles[1];
    assert_eq!(tabs_of(cx, a), ["alpha", "beta"], "it left");
    assert_eq!(shown(cx, a), "beta", "its neighbor shows");
    assert_eq!(tabs_of(cx, b), [NEW_TAB]);
    assert!(on_new_tab(cx, b), "the new window shows the picker");
    assert!(drawn(cx, b, "launch") && drawn(cx, b, "tab-new-tab"));

    // Picked there, a project opens in that window, in that tab.
    chose(cx, b, NewTabChoice::Project("gamma".into()));
    assert_eq!(shown(cx, b), "gamma");
    assert_eq!(tabs_of(cx, b), ["gamma"]);
}

#[gpui::test]
fn a_new_tab_reorders_like_any_tab(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta"]);
    launch_deps(cx);
    let a = open_at(cx, &f, "alpha", 0, super::window_tests::frame(0.0, 0.0), &["alpha", "beta"]);
    update(cx, a, |w, window, cx| w.open_new_tab(window, cx));
    draw(cx, a);
    let alpha = bounds_in(cx, a, "tab-alpha").unwrap();
    press_and_start(cx, a, "tab-new-tab");
    let to = point(alpha.left() + px(4.0), alpha.center().y);
    super::window_tests::move_to(cx, a, to);
    release_at(cx, a, to);
    assert_eq!(tabs_of(cx, a), [NEW_TAB, "alpha", "beta"]);
    assert!(on_new_tab(cx, a));
}
