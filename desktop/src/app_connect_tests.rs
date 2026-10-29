//! Connect to Remote Session over a project window (`launch-desktop`): the
//! palette's and the menu's Connect to Remote Session… both run the
//! registry's command, which opens the one dialog over the window; it is
//! modal (⌘W cancels it, keys stay in it) and a connection closes it.
//!
//! The window is the smoke test's headless one ([`Harness`]); the dialog runs
//! against the foundation's `FakeRemote`, with no recent list.

use gpui::TestAppContext;
use shelbi_app::launch::connect::Stage;
use shelbi_app::launch::{FakeRemote, RecentRemotes};

use super::smoke::Harness;
use super::*;
use crate::menus::tree::{AppCommand, MenuAction};
use crate::views::connect_remote::test_services;

fn open(cx: &mut TestAppContext) -> Harness {
    let mut h = Harness::open(cx);
    let remote = FakeRemote::new().with_host("devbox", "jlong", &[("shelbi", "~/Workspaces/shelbi")]);
    h.cx.update(|_, cx| cx.set_global(test_services(remote, RecentRemotes::none())));
    h.focus_window();
    h
}

fn dialog(h: &Harness) -> Option<Entity<ConnectDialog>> {
    h.window.read_with(&h.cx, |w, _| w.connect_dialog())
}

/// The menu's Connect to Remote Session… ran the registry's command: run it.
fn run_connect(h: &mut Harness) -> Entity<ConnectDialog> {
    h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::Command("action:connect-remote".into()),
        generation: 0,
    });
    h.cx.run_until_parked();
    let calls = h.take_calls();
    assert_eq!(calls, [HostCall::ConnectRemote]);
    h.perform(calls);
    dialog(h).expect("the Connect dialog is open over the window")
}

#[gpui::test]
fn the_menu_opens_the_dialog_once_and_cmd_w_cancels_it(cx: &mut TestAppContext) {
    let mut h = open(cx);
    let first = run_connect(&mut h);
    // Asking again while it's up keeps the one dialog.
    h.perform(vec![HostCall::ConnectRemote]);
    assert_eq!(dialog(&h).map(|d| d.entity_id()), Some(first.entity_id()));
    h.cx.dispatch_action(menus::MenuInvoke {
        target: MenuAction::App(AppCommand::CloseWindow),
        generation: 0,
    });
    h.cx.run_until_parked();
    assert!(dialog(&h).is_none(), "⌘W cancels the dialog, not the window");
}

#[gpui::test]
fn a_connection_from_the_window_closes_the_dialog(cx: &mut TestAppContext) {
    let mut h = open(cx);
    let d = run_connect(&mut h);
    h.cx.update(|window, cx| d.update(cx, |d, cx| d.type_host("devbox", window, cx)));
    h.key("enter");
    assert_eq!(d.read_with(&h.cx, |d, _| d.flow().stage()), Stage::Choose);
    h.key("enter");
    assert!(dialog(&h).is_none(), "connected, so closed");
}
