//! One window, tabs from more than one machine (`mixed-host-tabs`): a
//! remote project opens as a tab beside this machine's, each tab reads and
//! attaches on its own machine, and the host travels with its tab.
//!
//! The local projects are the window tests' catalog fixture
//! ([`super::window_tests`]); the far one is [`FarHost`]'s, reached as the app
//! reaches a real host ([`super::remote_tests::reach`]: its connector decides,
//! over its in-process link). Windows are inert: a tab click is recorded by
//! the tap and then performed, as the tab tests do.

use std::time::Duration;

use gpui::{point, px, TestAppContext};

use super::remote_tests::reach;
use super::window_tests::{
    draw, frame, open_at, press_and_start, read, release_at, move_to, shown, tabs_of, tearoff_fixture, update,
    vcx, windows, Fixture,
};
use super::*;
use crate::remote_fixture::{FarHost, HOST, PROJECT, USER};

/// The far project's tab id.
const FAR: &str = "devbox:far";

fn far_key() -> ProjectKey {
    ProjectKey::remote(HOST, PROJECT)
}

/// `far`'s project opened natively into window `into`, as a pick there does
/// once the host's listing decided.
fn open_far_in(cx: &mut TestAppContext, far: &FarHost, into: WindowHandle<ShelbiWindow>) {
    let remote = RemoteWindow::new(far.session(), far.listing(), far.link.clone(), far.remote.clone());
    cx.update(|cx| open_remote_in(remote, Some(into.into()), cx));
    cx.run_until_parked();
    draw(cx, into);
    draw(cx, into);
}

/// A window on alpha (the local fixture) with the far project as a second
/// tab, selected.
fn mixed(cx: &mut TestAppContext, tag: &str) -> (Fixture, FarHost, WindowHandle<ShelbiWindow>) {
    let f = tearoff_fixture(cx, &["alpha"]);
    let far = FarHost::new(tag);
    reach(cx, &far);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha"]);
    open_far_in(cx, &far, a);
    (f, far, a)
}

/// The tab calls the tap took (a click, ⌘N), performed.
fn perform_taken(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>) -> Vec<HostCall> {
    let calls = read(cx, h, |w, _| w.tap.as_ref().unwrap().borrow_mut().drain(..).collect::<Vec<_>>());
    for call in calls.clone() {
        update(cx, h, |w, window, cx| w.perform(call, window, cx));
    }
    calls
}

/// Step the far relay's threads until `done` holds.
fn settle(cx: &mut TestAppContext, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    for _ in 0..1000 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timed out waiting for {what}");
}

#[gpui::test]
fn a_remote_session_picked_on_the_new_tab_opens_as_a_tab_in_this_window(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha"]);
    let far = FarHost::new("mix-plus");
    reach(cx, &far);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha"]);
    update(cx, a, |w, window, cx| w.open_new_tab(window, cx));
    assert_eq!(tabs_of(cx, a), ["alpha", NEW_TAB]);

    // + → Connect to a remote session → the far project: its listing
    // decides (the same Shelbi), and it takes the New tab's place.
    cx.update(|cx| {
        let weak = a.update(cx, |_, _, cx| cx.weak_entity()).unwrap();
        new_tab_chose(NewTabChoice::Remote(far.session()), a.into(), weak, cx);
    });
    cx.run_until_parked();
    assert_eq!(tabs_of(cx, a), ["alpha", FAR], "in the New tab's place");
    assert_eq!(windows(cx).len(), 1, "no new window");
    assert!(read(cx, a, |w, _| w.shows_key(&far_key()) && w.is_remote()));
    assert_eq!(read(cx, a, |w, _| w.window_title()), format!("{USER}@{HOST} · {PROJECT}"));

    // The remote tab names its host; the local one doesn't.
    draw(cx, a);
    let mut v = vcx(cx, a);
    assert!(v.debug_bounds("tab-host-devbox:far").is_some());
    assert!(v.debug_bounds("tab-host-alpha").is_none());
    let props = read(cx, a, |w, cx| w.tab_props(cx));
    let hosts: Vec<_> = props.iter().map(|t| (t.name.as_str(), t.host.as_deref())).collect();
    assert_eq!(hosts, [("alpha", None), (FAR, Some(HOST))]);
    assert_eq!(tab_bar::tab_title(&props[1]).as_ref(), format!("{HOST} · {PROJECT}"));
}

#[gpui::test]
fn the_launch_window_adds_a_remote_project_to_the_frontmost_window(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha"]);
    let far = FarHost::new("mix-launch");
    reach(cx, &far);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha"]);
    // As the launch window (or a recent remote session) opens it.
    cx.update(|cx| crate::views::remote_session::open_remote_session(far.session(), cx));
    cx.run_until_parked();
    assert_eq!(windows(cx).len(), 1);
    assert_eq!(tabs_of(cx, a), ["alpha", FAR]);
    assert!(read(cx, a, |w, _| w.shows_key(&far_key())));

    // Opening it again selects its tab rather than adding another.
    update(cx, a, |w, window, cx| w.switch_to("alpha", window, cx));
    cx.update(|cx| crate::views::remote_session::open_remote_session(far.session(), cx));
    cx.run_until_parked();
    assert_eq!(tabs_of(cx, a), ["alpha", FAR]);
    assert!(read(cx, a, |w, _| w.shows_key(&far_key())));
}

#[gpui::test]
fn local_and_remote_tabs_switch_by_click_and_command_n_each_on_its_own_machine(cx: &mut TestAppContext) {
    let (_f, far, a) = mixed(cx, "mix-switch");
    update(cx, a, |w, _, cx| {
        w.hosts.get_mut(HOST).unwrap().bar = Some(RemoteBar::Stopped("Shelbi isn't running on devbox.".into()));
        cx.notify();
    });

    // ⌘1: the local tab, read here. The host stays open behind it, its
    // link up: a background tab never tears its link down.
    update(cx, a, |w, window, cx| w.select_tab(1, window, cx));
    assert_eq!(perform_taken(cx, a), [HostCall::SwitchProject("alpha".into())]);
    let (key, remote, title, bar) =
        read(cx, a, |w, _| (w.project_handle.key().clone(), w.is_remote(), w.window_title(), w.remote_bar()));
    assert_eq!(key, ProjectKey::local("alpha"));
    assert!(!remote);
    assert_eq!(title, "Alpha", "the title follows the tab");
    assert!(bar.is_none(), "the host's bar is its own tabs'");
    assert!(read(cx, a, |w, _| w.hosts.contains_key(HOST)));
    assert!(far.link.is_up());
    draw(cx, a);
    assert!(vcx(cx, a).debug_bounds("remote-stopped-bar").is_none());

    // A click on the remote tab: back on the far project, with its bar.
    vcx(cx, a).simulate_click(
        vcx(cx, a).debug_bounds("tab-devbox:far").unwrap().center(),
        gpui::Modifiers::none(),
    );
    cx.run_until_parked();
    assert_eq!(perform_taken(cx, a), [HostCall::SwitchProject(FAR.into())]);
    assert!(read(cx, a, |w, _| w.shows_key(&far_key()) && w.is_remote()));
    assert_eq!(read(cx, a, |w, _| w.window_title()), format!("{USER}@{HOST} · {PROJECT}"));
    assert!(read(cx, a, |w, _| w.remote_bar()).is_some());

    // The host changes: only its tab shows Reopen; the local one goes on.
    update(cx, a, |w, _, cx| w.host_changed(HOST, "Shelbi on devbox changed.".into(), cx));
    draw(cx, a);
    assert!(vcx(cx, a).debug_bounds("remote-reopen").is_some());
    update(cx, a, |w, window, cx| w.select_tab(1, window, cx));
    perform_taken(cx, a);
    // The switch's first frame paints the sidebar, the next its main view.
    draw(cx, a);
    draw(cx, a);
    assert!(vcx(cx, a).debug_bounds("remote-reopen").is_none(), "alpha's tab is untouched");
    assert_eq!(shown(cx, a), "alpha");
    // Back where alpha was left (a test window opens on Machines), its
    // shell up.
    assert_eq!(read(cx, a, |w, _| w.client.view().clone()), View::Machines);
    assert!(read(cx, a, |w, _| w.machines.is_some() && !w.is_remote() && w.remote_changed().is_none()));
}

#[gpui::test]
fn a_local_and_a_remote_project_of_one_name_are_two_tabs(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha"]);
    let far = FarHost::new("mix-same");
    reach(cx, &far);
    // A project here named like the far one.
    let a = open_at(cx, &f, PROJECT, 0, frame(0.0, 0.0), &[PROJECT]);
    open_far_in(cx, &far, a);
    assert_eq!(tabs_of(cx, a), [PROJECT, FAR]);
    assert!(read(cx, a, |w, _| w.shows_key(&far_key())));
    update(cx, a, |w, window, cx| w.switch_to(PROJECT, window, cx));
    assert!(read(cx, a, |w, _| w.shows_key(&ProjectKey::local(PROJECT)) && !w.is_remote()));
    // A window with the local tab isn't the remote one's: opening the far
    // project again finds its own tab.
    open_far_in(cx, &far, a);
    assert_eq!(tabs_of(cx, a), [PROJECT, FAR]);
    assert!(read(cx, a, |w, _| w.shows_key(&far_key())));
}

#[gpui::test]
fn closing_the_remote_tab_leaves_the_local_ones_and_lets_its_host_go(cx: &mut TestAppContext) {
    let (_f, far, a) = mixed(cx, "mix-close");
    update(cx, a, |w, window, cx| w.confirm_close_project(FAR.into(), window, cx));
    cx.simulate_prompt_answer("Close Project");
    settle(cx, "the far daemon to quit the project", |_| !far.asked.lock().unwrap().quits.is_empty());
    settle(cx, "the window to move on", |cx| shown(cx, a) == "alpha");
    assert_eq!(far.asked.lock().unwrap().quits, [PROJECT], "quit on its own machine");
    assert_eq!(tabs_of(cx, a), ["alpha"]);
    assert!(read(cx, a, |w, _| !w.is_remote() && w.hosts.is_empty()), "the host went with its last tab");
}

#[gpui::test]
fn a_restored_remote_tab_opens_its_host_in_its_tab_and_never_holds_up_the_window(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha"]);
    let far = FarHost::new("mix-restore");
    // The saved window: alpha, then the far project, which it showed.
    let state = WindowState {
        tabs: vec![ProjectKey::local("alpha"), far_key()],
        ..WindowState::default()
    };
    // The host can't be reached (nothing reaches it yet).
    let a = cx
        .update(|cx| open_window_on(&far_key(), None, 0, state, None, cx))
        .expect("the window opens");
    let deps = f.deps.clone();
    update(cx, a, |w, _, _| {
        w.chat_deps = Some(deps.sessions.clone());
        w.workspace_deps = Some(deps);
    });
    cx.run_until_parked();
    draw(cx, a);
    assert_eq!(tabs_of(cx, a), ["alpha", FAR], "the saved order");
    assert!(read(cx, a, |w, _| w.on_pending_tab() && w.shows(FAR)));
    let mut v = vcx(cx, a);
    assert!(v.debug_bounds("remote-retry").is_some(), "why, with Try Again, in its tab");
    assert!(v.debug_bounds("tab-alpha").is_some(), "the other tabs stay a click away");
    // It is still the project the window is on, for the next launch.
    let slot = cx.update(|cx| cx.global::<Shell>().session.slots[0].clone());
    assert_eq!(slot.project, Some(far_key()));
    assert_eq!(slot.state.tabs, [ProjectKey::local("alpha"), far_key()]);

    // The local tab works meanwhile.
    update(cx, a, |w, window, cx| w.switch_to("alpha", window, cx));
    draw(cx, a);
    assert!(read(cx, a, |w, _| w.shows_key(&ProjectKey::local("alpha")) && w.chat.is_some()));

    // The host answers now: back on its tab, it opens there.
    reach(cx, &far);
    update(cx, a, |w, window, cx| w.switch_to(FAR, window, cx));
    cx.run_until_parked();
    assert!(read(cx, a, |w, _| w.shows_key(&far_key()) && w.is_remote()));
    assert_eq!(read(cx, a, |w, _| w.window_title()), format!("{USER}@{HOST} · {PROJECT}"));
}

#[gpui::test]
fn a_remote_tab_dragged_to_another_window_stays_connected_to_its_host(cx: &mut TestAppContext) {
    let (f, far, a) = mixed(cx, "mix-move");
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    let gamma = super::window_tests::bounds_in(cx, b, "tab-gamma").unwrap();
    press_and_start(cx, a, "tab-devbox:far");
    let to = point(px(1000.0) + gamma.right() - px(5.0), gamma.center().y);
    move_to(cx, a, to);
    release_at(cx, a, to);
    draw(cx, b);
    assert_eq!(tabs_of(cx, b), ["gamma", FAR]);
    assert!(read(cx, b, |w, _| w.shows_key(&far_key()) && w.is_remote()), "over its host's link");
    assert!(read(cx, b, |w, _| w.hosts.get(HOST).and_then(|h| h.remote.as_ref()).is_some_and(|r| Arc::ptr_eq(&r.link, &far.link))));
    assert_eq!(tabs_of(cx, a), ["alpha"]);
    assert!(read(cx, a, |w, _| w.hosts.is_empty()), "the first window let the host go");
    assert!(far.link.is_up());
}

#[gpui::test]
fn a_remote_tab_torn_off_opens_its_window_over_its_host(cx: &mut TestAppContext) {
    let (_f, _far, a) = mixed(cx, "mix-tear");
    press_and_start(cx, a, "tab-devbox:far");
    let away = point(px(450.0), px(52.0 + 260.0));
    move_to(cx, a, away);
    release_at(cx, a, away);
    assert_eq!(windows(cx).len(), 2);
    let b = cx.update(|cx| cx.global::<Shell>().windows[1].handle);
    assert_eq!(tabs_of(cx, b), [FAR]);
    assert!(read(cx, b, |w, _| w.shows_key(&far_key()) && w.is_remote()));
    assert_eq!(tabs_of(cx, a), ["alpha"]);
}
