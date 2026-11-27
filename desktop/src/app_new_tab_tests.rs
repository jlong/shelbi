//! The New tab (`new-tab-launch`): the tab bar's `+` turns into a "New tab"
//! (`new-tab-plus-morph`: its `+` into an `×`, its label sliding out, the
//! slot widening; at once under Reduce Motion) whose content is the launch
//! screen ([`LaunchView::in_tab`], the launch window's own view). While it is
//! open there is no other `+`, so a window has one. What is picked there
//! becomes that tab: a project opens in its place (the last tab, a plain `+`
//! back after it), one with a tab already (here or in another window) comes
//! up there and the New tab goes, the wizard's new project lands in it, and a
//! remote session opens its own window. It closes (its `×`, ⌘W) turning back
//! into the `+`, is never saved, and stays pinned in the `+` slot: it never
//! drags, reorders or tears off, and no tab lands past it.
//!
//! Windows are the window tests' inert ones ([`super::window_tests`]); the
//! launch screen reads a canned model ([`TestDeps`]) and starts a fake
//! daemon, so nothing runs or reaches the real Shelbi home.

use std::cell::Cell;
use std::sync::Arc;
use std::time::Instant;

use gpui::{point, px, Modifiers, TestAppContext};
use shelbi_app::launch::{FakeDaemon, LaunchModel};

use super::window_tests::{
    bounds_in, draw, fixture, open, open_at, press_and_start, read, release_at, shown, tabs_of, tearoff_fixture,
    update, vcx, windows, Fixture,
};
use super::*;
use crate::views::launch::tests::{project, session, Home};
use crate::theme::motion;
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

/// Run `h`'s clock by hand from now on (its tab bar's turns follow it):
/// the clock, to set.
fn hand_clock(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>) -> Rc<Cell<Instant>> {
    let now = Rc::new(Cell::new(Instant::now()));
    let clock = now.clone();
    update(cx, h, move |w, _, _| {
        w.clock = Rc::new(move || clock.get());
        w.reduce_motion = Rc::new(|| false);
    });
    now
}

fn width(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>, selector: &'static str) -> f32 {
    bounds_in(cx, h, selector).unwrap_or_else(|| panic!("{selector} is drawn")).size.width.as_f32()
}

/// The New tab's width, fully open, from its label's as it painted.
fn open_width(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>) -> f32 {
    let label = read(cx, h, |w, _| w.tab_reorder.layout.borrow().plus_label_w).expect("the label painted");
    tab_bar::plus_slot_w(1.0, label)
}

/// Widths as laid out, within a fraction of a point.
#[track_caller]
fn assert_near(got: f32, want: f32, why: &str) {
    assert!((got - want).abs() < 0.5, "{got} vs {want}: {why}");
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
    // The `+` became it, in its slot past beta: no `+` is left to start
    // another, and its `×` is where the `+` was.
    assert!(drawn(cx, h, "tab-new-tab") && drawn(cx, h, "tab-close-new-tab"));
    assert!(!drawn(cx, h, "tab-plus"), "no separate +");
    let (beta, new_tab) = (bounds_in(cx, h, "tab-beta").unwrap(), bounds_in(cx, h, "tab-new-tab").unwrap());
    assert!(new_tab.left() > beta.right(), "at the bar's end");
    let close = bounds_in(cx, h, "tab-close-new-tab").unwrap();
    assert_eq!(close.center().x - new_tab.left(), px(26.0), "centered where the + was");

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

    // Switch Project… (or a click on its body) selects it: never a second
    // New tab.
    update(cx, h, |w, window, cx| w.open_new_tab(window, cx));
    click(cx, h, "tab-new-tab-label");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", NEW_TAB]);
    assert!(on_new_tab(cx, h));
}

#[gpui::test]
fn the_plus_turns_into_the_new_tab_and_back(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = open(cx, &f, "alpha", 0);
    update(cx, h, |w, window, cx| w.switch_to("beta", window, cx));
    let now = hand_clock(cx, h);
    assert_near(width(cx, h, "tab-plus"), 52.0, "the 52pt square");
    let beta = bounds_in(cx, h, "tab-beta").unwrap();
    let t0 = now.get();
    click(cx, h, "tab-plus");
    assert!(on_new_tab(cx, h), "the launch screen shows at once");
    assert!(drawn(cx, h, "launch"));
    // Still the square as the click lands, then widening on the curve.
    assert_near(width(cx, h, "tab-new-tab"), 52.0, "");
    let full = open_width(cx, h);
    assert!(full > 52.0 + 40.0, "{full}: room for the label");
    now.set(t0 + motion::TAB_MORPH / 4);
    let quarter = width(cx, h, "tab-new-tab");
    let eased = 52.0 + (full - 52.0) * motion::ease_out_expo(0.25);
    assert!((quarter - eased).abs() < 0.5, "{quarter} vs {eased}");
    assert!(read(cx, h, |w, _| w.plus_morph.animating(true, t0 + motion::TAB_MORPH / 4, false)));
    // The tabs before it never move.
    assert_eq!(bounds_in(cx, h, "tab-beta").unwrap(), beta);
    now.set(t0 + motion::TAB_MORPH);
    assert_near(width(cx, h, "tab-new-tab"), full, "");
    assert_eq!(bounds_in(cx, h, "tab-beta").unwrap(), beta);

    // Its `×`: back the same way, to a lone `+`.
    let t1 = now.get();
    click(cx, h, "tab-close-new-tab");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta"]);
    assert!(!drawn(cx, h, "tab-new-tab"));
    assert_near(width(cx, h, "tab-plus"), full, "turning back from the New tab");
    now.set(t1 + motion::TAB_MORPH / 2);
    let half = width(cx, h, "tab-plus");
    assert!(half > 52.0 && half < full, "{half}");
    now.set(t1 + motion::TAB_MORPH);
    assert_near(width(cx, h, "tab-plus"), 52.0, "");
    assert!(!drawn(cx, h, "tab-new-tab-label"), "the label is gone under the +");
    assert_eq!(bounds_in(cx, h, "tab-beta").unwrap(), beta);
}

#[gpui::test]
fn under_reduce_motion_the_plus_turns_at_once(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = open(cx, &f, "alpha", 0);
    let _now = hand_clock(cx, h);
    update(cx, h, |w, _, _| w.reduce_motion = Rc::new(|| true));
    click(cx, h, "tab-plus");
    let full = open_width(cx, h);
    assert_near(width(cx, h, "tab-new-tab"), full, "open as the click lands");
    click(cx, h, "tab-close-new-tab");
    assert_near(width(cx, h, "tab-plus"), 52.0, "closed as the click lands");
    // ⌘W too.
    click(cx, h, "tab-plus");
    update(cx, h, |w, window, cx| w.close_window(window, cx));
    assert_near(width(cx, h, "tab-plus"), 52.0, "");
    assert_eq!(windows(cx).len(), 1);
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
    let tab = read(cx, h, |w, cx| w.tab_props(cx).into_iter().find(|t| t.name == NEW_TAB).unwrap());
    assert!(!tab.selected, "unselected, like any tab");
    assert!(drawn(cx, h, "tab-new-tab") && !drawn(cx, h, "tab-plus"), "still in the + slot");
    // Its tab (a click on its label, ⌘3) brings the launch screen back.
    click(cx, h, "tab-new-tab-label");
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
    let now = hand_clock(cx, h);
    click(cx, h, "tab-plus");
    now.set(now.get() + motion::TAB_MORPH);
    let full = open_width(cx, h);
    assert_near(width(cx, h, "tab-new-tab"), full, "");
    // gamma, the launch screen's third row.
    click(cx, h, "launch-project-2");
    assert_eq!(daemon.starts(), 1, "the daemon is started first, as from the launch window");
    assert_eq!(shown(cx, h), "gamma");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", "gamma"], "in the New tab's place, the last");
    assert!(!on_new_tab(cx, h));
    assert_eq!(windows(cx).len(), 1, "in this window");
    assert_eq!(read(cx, h, |w, _| w.window_state.tabs.clone()), keys(&["alpha", "beta", "gamma"]));
    // A plain `+` past it, just there: no turning back.
    assert!(drawn(cx, h, "tab-gamma"));
    assert_near(width(cx, h, "tab-plus"), 52.0, "");
    assert!(!drawn(cx, h, "tab-new-tab-label"));
    assert!(bounds_in(cx, h, "tab-plus").unwrap().left() > bounds_in(cx, h, "tab-gamma").unwrap().right());
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
    update(cx, h, |w, window, cx| w.project_added("gamma", window, cx));
    assert_eq!(shown(cx, h), "gamma");
    assert_eq!(tabs_of(cx, h), ["alpha", "beta", "gamma"], "in the New tab's place");
    assert!(drawn(cx, h, "tab-plus"));
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
fn the_new_tab_never_drags_or_tears_off(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta"]);
    launch_deps(cx);
    let a = open_at(cx, &f, "alpha", 0, super::window_tests::frame(0.0, 0.0), &["alpha", "beta"]);
    update(cx, a, |w, window, cx| w.open_new_tab(window, cx));
    draw(cx, a);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta", NEW_TAB]);
    // Pressed and pulled onto alpha: nothing moves.
    let alpha = bounds_in(cx, a, "tab-alpha").unwrap();
    press_and_start(cx, a, "tab-new-tab-label");
    assert!(read(cx, a, |w, _| !w.tab_reorder.ghost_alive()), "no drag began");
    let to = point(alpha.left() + px(4.0), alpha.center().y);
    super::window_tests::move_to(cx, a, to);
    release_at(cx, a, to);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta", NEW_TAB]);
    // Pulled far off the bar: no window of its own.
    press_and_start(cx, a, "tab-new-tab-label");
    let away = point(px(450.0), px(52.0 + 300.0));
    super::window_tests::move_to(cx, a, away);
    release_at(cx, a, away);
    assert_eq!(cx.update(|cx| cx.global::<Shell>().windows.len()), 1);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta", NEW_TAB]);
}

#[gpui::test]
fn a_tab_dragged_to_the_end_lands_before_the_new_tab(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta"]);
    launch_deps(cx);
    let a = open_at(cx, &f, "alpha", 0, super::window_tests::frame(0.0, 0.0), &["alpha", "beta"]);
    update(cx, a, |w, window, cx| w.open_new_tab(window, cx));
    draw(cx, a);
    let new_tab = bounds_in(cx, a, "tab-new-tab").unwrap();
    // Over the New tab is no gap; past it, the end, which is before it.
    press_and_start(cx, a, "tab-alpha");
    let over = point(new_tab.center().x, new_tab.center().y);
    super::window_tests::move_to(cx, a, over);
    release_at(cx, a, over);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta", NEW_TAB], "a drop over it moves nothing");
    let past = point(new_tab.right() + px(40.0), new_tab.center().y);
    press_and_start(cx, a, "tab-alpha");
    super::window_tests::move_to(cx, a, past);
    release_at(cx, a, past);
    assert_eq!(tabs_of(cx, a), ["beta", "alpha", NEW_TAB]);
}

#[gpui::test]
fn switch_project_from_the_palette_gives_the_new_tab_the_keys(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = open(cx, &f, "alpha", 0);
    draw(cx, h);
    // The real dispatch, not the tap: Switch Project… runs its effect.
    update(cx, h, |w, window, cx| {
        w.tap = None;
        w.open_palette(window, cx);
    });
    draw(cx, h);
    // Picked as a person does: typed into the palette, then Return.
    let mut v = vcx(cx, h);
    v.simulate_input("switch project");
    cx.run_until_parked();
    draw(cx, h);
    let top = read(cx, h, |w, _| match &w.overlay {
        Some(ActiveOverlay::Palette { palette, .. }) => {
            palette.results().get(palette.selected()).map(|(e, _)| e.id.clone())
        }
        _ => None,
    });
    assert_eq!(top.as_deref(), Some("action:switch-project"), "Switch Project… is the pick");
    let mut v = vcx(cx, h);
    v.simulate_keystrokes("enter");
    cx.run_until_parked();
    draw(cx, h);
    assert!(on_new_tab(cx, h), "Switch Project… opened the New tab");
    let launch = read(cx, h, |w, _| w.new_tab.clone()).unwrap();
    let mut v = vcx(cx, h);
    v.simulate_keystrokes("down down");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| launch.read(cx).selected()), 2, "↓ walks the rows, as after the +");
}

#[gpui::test]
fn keys_left_on_the_window_go_back_to_the_new_tab(cx: &mut TestAppContext) {
    let f = setup(cx);
    let h = with_new_tab(cx, &f);
    let launch = read(cx, h, |w, _| w.new_tab.clone()).unwrap();
    // What a palette close does: the keys back on the window itself.
    update(cx, h, |w, window, cx| w.focus_handle.focus(window, cx));
    draw(cx, h);
    let mut v = vcx(cx, h);
    v.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| launch.read(cx).selected()), 1, "the launch screen has them again");
    // The palette over the New tab keeps them while it's up.
    update(cx, h, |w, window, cx| w.open_palette(window, cx));
    draw(cx, h);
    let window_has_keys = cx.update(|cx| h.update(cx, |w, window, _| w.focus_handle.is_focused(window)).unwrap());
    assert!(window_has_keys, "the palette's keys stay with it");
    let mut v = vcx(cx, h);
    v.simulate_keystrokes("down");
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| launch.read(cx).selected()), 1, "the launch screen got none of them");
    assert!(read(cx, h, |w, _| w.overlay.is_some()));
}
