//! The native remote window (remote-window P6, the default since P10): a
//! host on this exact version opens the full main window for the far
//! project, and every view reads it, and commands it, over the host's relay
//! link. Open in Terminal View opens the far TUI beside it; a host that
//! changed while the window was away closes its panes and offers Reopen.
//!
//! The far host is [`crate::remote_fixture::FarHost`]: a fixture project read
//! by the far side's real query handler across an in-process relay, a fake far
//! daemon on the relay's `control.sock`, and a fake far hub. The window opens
//! through the app's own [`open_remote_window`] (an inert test window: no
//! bootstrap); its tap is then taken off so commands run for real, against
//! the far host. Feeds are canned ([`crate::feeds::tests`]): a live feed's
//! threads would wake gpui's test scheduler off its thread. The far `Changed`
//! reaching a feed is `refresh::tests`'s; here the board's own reads show it.

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
use shelbi_app::launch::FakeRemote;
use shelbi_app::remote_window::{self, RemoteOpen};
use shelbi_app::session::RetryPolicy;
use shelbi_app::theme::ThemePreference;
use shelbi_core::Column;
use shelbi_proto::control::MutationKind;

use super::*;
use crate::feeds::tests::canned_hub;
use crate::remote_fixture::{far_column, FarHost, HOST, PROJECT, USER};
use crate::views::session_view::SessionDeps;
use crate::views::settings_nav::SettingsPage;

fn setup(cx: &mut TestAppContext) {
    // Activity reads on this thread (its reader thread would wake the test
    // scheduler off it).
    crate::views::activity_reader::tests::INLINE.with(|i| i.set(true));
    let (hub, _feeds) = canned_hub();
    cx.update(|cx| {
        cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        gpui_kit::init(cx);
        cx.set_global(Shell {
            inert_windows: true,
            ..Shell::default()
        });
        cx.set_global(hub);
    });
}

/// Open `far`'s project as Connect does: the listing decides (natively:
/// same version), then the window opens over the host's link. Its commands
/// then run for real.
fn open(cx: &mut TestAppContext, far: &FarHost) -> WindowHandle<ShelbiWindow> {
    let RemoteOpen::Native(listing) = remote_window::choose(HOST, || far.remote.listing(HOST)) else {
        panic!("a host on this exact version opens natively");
    };
    let remote = RemoteWindow::new(far.session(), listing, far.link.clone(), far.remote.clone());
    let handle = cx
        .update(|cx| open_remote_window(remote, cx))
        .expect("the remote window opens");
    update(cx, handle, |w, window, cx| {
        w.tap = None;
        // The orchestrator's attach gives up at once when the far host has
        // no session (rather than waiting out the relaunch deadline).
        let project = w.project_handle.clone();
        w.chat_deps = Some(Rc::new(move || {
            let mut deps = SessionDeps::live(&project);
            deps.retry = RetryPolicy {
                deadline: Duration::ZERO,
                backoff: Duration::from_millis(1),
            };
            deps
        }));
        w.start_feed(window, cx);
    });
    handle
}

fn update<R>(
    cx: &mut TestAppContext,
    handle: WindowHandle<ShelbiWindow>,
    f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>) -> R,
) -> R {
    let r = cx.update(|cx| handle.update(cx, f).unwrap());
    cx.run_until_parked();
    r
}

fn read<R>(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, f: impl FnOnce(&ShelbiWindow, &App) -> R) -> R {
    cx.update(|cx| handle.read_with(cx, f).unwrap())
}

fn draw(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) {
    cx.update(|cx| {
        cx.update_window(*handle, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap()
    });
    cx.run_until_parked();
}

fn run(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, kind: CommandKind) {
    update(cx, handle, |w, window, cx| w.run(kind, window, cx));
}

/// Let the views' workers (threads answering over the relay) land: step the
/// views' poll timers until `done` holds.
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

/// Long enough for anything still in flight to land (for a thing that must
/// not happen).
fn linger(cx: &mut TestAppContext) {
    for _ in 0..20 {
        cx.executor().advance_clock(Duration::from_millis(100));
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Where `id` sits on the window's board, once it has read it.
fn column_of(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, id: &str) -> Option<Column> {
    read(cx, handle, |w, cx| {
        let issues = w.issues.as_ref()?;
        issues
            .read(cx)
            .board_input()
            .open
            .iter()
            .find(|i| i.task.id == id)
            .map(|i| i.task.column.clone())
    })
}

#[gpui::test]
fn the_remote_window_names_its_host_and_switches_among_its_projects(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("names");
    let handle = open(cx, &far);
    draw(cx, handle);

    let (title, remote, key) = read(cx, handle, |w, _| (w.window_title(), w.is_remote(), w.project_handle.key().clone()));
    assert_eq!(title, format!("{USER}@{HOST} · {PROJECT}"));
    assert!(remote);
    assert_eq!(key, ProjectKey::remote(HOST, PROJECT));
    // The sidebar names who and where, with the link's dot.
    let mut vcx = VisualTestContext::from_window(handle.into(), cx);
    assert!(vcx.debug_bounds("sidebar-remote-host").is_some());
    assert!(vcx.debug_bounds("sidebar-remote-dot").is_some());

    // A local project of the same name is another key: a local open doesn't
    // find this window.
    assert!(cx.update(|cx| windows_showing(PROJECT, None, cx)).is_empty());
    assert_eq!(cx.update(|cx| windows_showing_key(&key, None, cx)), vec![handle]);
    // Opening it again brings this window up rather than a second one.
    let again = RemoteWindow::new(far.session(), far.listing(), far.link.clone(), far.remote.clone());
    assert_eq!(cx.update(|cx| open_remote_window(again, cx)), Some(handle));

    // The switcher and the palette list the far host's projects only.
    let (switcher, model) = read(cx, handle, |w, cx| (w.switcher_projects(cx), w.command_model()));
    assert_eq!(switcher.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), [PROJECT]);
    assert_eq!(model.remote_host.as_deref(), Some(HOST));
}

#[gpui::test]
fn a_clicked_notice_opens_in_its_remote_window_and_keeps_the_project(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("notice");
    let handle = open(cx, &far);
    update(cx, handle, |w, _, _| {
        w.tap = Some(Rc::default());
    });
    let key = read(cx, handle, |w, _| w.project_handle.key().clone());
    // The tag the window's own notifier writes.
    let tag = format!("shelbi/{key}/orch");
    assert_eq!(tag, format!("shelbi/{HOST}:{PROJECT}/orch"));

    cx.update(|cx| open_notice_tag(&tag, cx));
    cx.run_until_parked();

    let shows = |cx: &mut TestAppContext| {
        read(cx, handle, |w, _| {
            (
                w.project.clone(),
                w.project_handle.key().clone(),
                w.remote.as_ref().unwrap().session.project.clone(),
            )
        })
    };
    assert_eq!(shows(cx), (PROJECT.to_string(), key.clone(), PROJECT.to_string()), "no bogus switch");
    assert_eq!(
        read(cx, handle, |w, _| w.tap.as_ref().unwrap().borrow().clone()),
        [HostCall::ShowView(View::Session("orch".into()))],
        "and the item opens there"
    );

    // Another far project no window shows has nowhere to go: nothing moves.
    cx.update(|cx| open_notice_tag(&format!("shelbi/{HOST}:elsewhere/orch"), cx));
    cx.run_until_parked();
    assert_eq!(shows(cx), (PROJECT.to_string(), key.clone(), PROJECT.to_string()));
}

#[gpui::test]
fn a_local_notice_never_lands_in_a_remote_window(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("localnotice");
    let handle = open(cx, &far);
    let key = read(cx, handle, |w, _| w.project_handle.key().clone());
    update(cx, handle, |_, window, _| window.activate_window());

    cx.update(|cx| open_notice_tag("shelbi/gamma/orch", cx));
    cx.run_until_parked();

    assert_eq!(read(cx, handle, |w, _| w.project_handle.key().clone()), key);
    assert_eq!(
        cx.update(|cx| windows_showing("gamma", None, cx)).len(),
        1,
        "a local window opens on it instead"
    );
}

#[gpui::test]
fn a_notice_switching_the_window_under_the_quit_prompt_keeps_it_there(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("quitswitch");
    let handle = open(cx, &far);
    update(cx, handle, |w, window, cx| w.confirm_quit_project(window, cx));
    assert!(cx.has_pending_prompt());

    // Moved on (a shown chat, the view a close would mark), then the answer.
    update(cx, handle, |w, window, cx| w.switch_to("other", window, cx));
    draw(cx, handle);
    assert!(read(cx, handle, |w, _| w.chat.is_some()), "the new project's chat");
    cx.simulate_prompt_answer("Close Project");
    settle(cx, "the far daemon to quit the project", |_| !far.asked.lock().unwrap().quits.is_empty());
    linger(cx);

    assert_eq!(far.asked.lock().unwrap().quits, [PROJECT], "the project the prompt named");
    let (project, chooser, closing) = read(cx, handle, |w, cx| {
        let closing = w.chat.as_ref().is_some_and(|c| c.read(cx).closing_deliberately());
        (w.project.clone(), w.chooser, closing)
    });
    assert_eq!(project, "other", "the window stays on the project on screen");
    assert!(!chooser);
    assert!(!closing, "other's orchestrator isn't closing");
}

#[gpui::test]
fn quitting_a_remote_windows_project_goes_to_the_chooser(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("quitfar");
    // Open on this machine, so never for a window on another one.
    std::fs::write(shelbi_state::projects_dir().unwrap().join("here.yaml"), b"").unwrap();
    shelbi_state::set_project_open("here", true).unwrap();
    let handle = open(cx, &far);
    update(cx, handle, |w, window, cx| w.confirm_quit_project(window, cx));
    cx.simulate_prompt_answer("Close Project");
    settle(cx, "the window to leave the project", |cx| read(cx, handle, |w, _| w.chooser));
    assert_eq!(far.asked.lock().unwrap().quits, [PROJECT]);
    assert_eq!(read(cx, handle, |w, _| w.remote.as_ref().unwrap().session.project.clone()), PROJECT);
}

#[gpui::test]
fn a_switch_during_the_zen_check_drops_the_toggle(cx: &mut TestAppContext) {
    use shelbi_proto::control::StateWrite;
    setup(cx);
    let far = FarHost::new("zenswitch");
    shelbi_state::mark_zen_intro_seen().unwrap();
    let handle = open(cx, &far);
    update(cx, handle, |w, window, cx| {
        w.toggle_zen(window, cx);
        w.switch_to("other", window, cx);
    });
    linger(cx);
    assert!(
        !far.writes.lock().unwrap().iter().any(|w| matches!(w, StateWrite::ToggleZen { .. })),
        "the toggle was for the project the check read"
    );
    assert!(read(cx, handle, |w, _| w.overlay.is_none()));
}

#[gpui::test]
fn the_remote_window_opens_every_view_over_the_relay(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("views");
    let handle = open(cx, &far);

    run(cx, handle, CommandKind::ShowView(View::Issues));
    settle(cx, "the far board", |cx| column_of(cx, handle, "t-1").is_some());
    assert_eq!(column_of(cx, handle, "t-2"), Some(Column::from_status_id("backlog")));
    draw(cx, handle);

    run(cx, handle, CommandKind::ShowView(View::Activity));
    settle(cx, "the far activity", |cx| {
        read(cx, handle, |w, cx| {
            w.activity
                .as_ref()
                .is_some_and(|a| a.read(cx).state().is_loaded() && a.read(cx).state().loaded_events() > 0)
        })
    });
    draw(cx, handle);

    run(cx, handle, CommandKind::ShowView(View::Machines));
    settle(cx, "the far machines", |cx| {
        read(cx, handle, |w, cx| w.machines.as_ref().is_some_and(|m| m.read(cx).state().is_loaded()))
    });
    draw(cx, handle);

    run(cx, handle, CommandKind::ShowView(View::Changelog));
    assert!(read(cx, handle, |w, _| w.changelog.is_some()));
    draw(cx, handle);

    // The orchestrator attaches over the link: the far host has no session,
    // so it says so (rather than reaching this machine's sessions).
    run(cx, handle, CommandKind::ShowView(View::Session("orch".into())));
    settle(cx, "the orchestrator's attach", |cx| {
        read(cx, handle, |w, cx| w.chat.as_ref().is_some_and(|c| c.read(cx).notice().is_some()))
    });
    draw(cx, handle);

    // A workspace opens its page (idle: nothing runs on the far host).
    run(cx, handle, CommandKind::FocusWorkspace { workspace: "alpha".into() });
    assert!(read(cx, handle, |w, _| w.workspace.is_some()));
    draw(cx, handle);

    // Settings, every page, reading the far project.
    run(cx, handle, CommandKind::OpenSettings);
    assert!(read(cx, handle, |w, _| matches!(w.route, Route::Settings(_))));
    for page in [
        SettingsPage::General,
        SettingsPage::Keybindings,
        SettingsPage::Terminal,
        SettingsPage::Workspaces,
        SettingsPage::Workflows,
        SettingsPage::Agents,
        SettingsPage::Orchestrator,
    ] {
        update(cx, handle, |w, window, cx| w.open_settings(Some(page), window, cx));
        draw(cx, handle);
    }
    // Settings edits: the workflow editor opens on the far project.
    update(cx, handle, |w, window, cx| w.edit_workflow("task".into(), window, cx));
    let (edited, banner) = read(cx, handle, |w, _| (w.edited_workflows.clone(), w.banner.clone()));
    assert_eq!(edited, ["task"], "a remote project's workflow opens its editor");
    assert_eq!(banner, None);
}

#[gpui::test]
fn a_moved_card_lands_on_the_far_board_and_a_far_change_reaches_the_window(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("move");
    let handle = open(cx, &far);
    run(cx, handle, CommandKind::ShowView(View::Issues));
    settle(cx, "the far board", |cx| column_of(cx, handle, "t-1").is_some());

    // This window moves t-1: the far daemon takes it (over the relay's
    // control service) and its board has it.
    run(
        cx,
        handle,
        CommandKind::MoveIssue {
            id: "t-1".into(),
            to_status: "review".into(),
        },
    );
    settle(cx, "the far daemon to take the move", |_| !far.taken.lock().unwrap().is_empty());
    let taken = far.taken.lock().unwrap().clone();
    assert!(
        matches!(&taken[..], [(id, MutationKind::Move { to, .. })] if id == "t-1" && to == "review"),
        "{taken:?}"
    );
    assert_eq!(far_column("t-1"), Some(Column::from_status_id("review")), "the far board has it");
    settle(cx, "the window to show the move", |cx| {
        column_of(cx, handle, "t-1") == Some(Column::from_status_id("review"))
    });

    // Another client on the far host moves t-2: the window follows.
    far.far_move("t-2", "todo");
    settle(cx, "the far change to reach the window", |cx| {
        column_of(cx, handle, "t-2") == Some(Column::from_status_id("todo"))
    });
}

#[gpui::test]
fn what_a_remote_window_cant_do_yet_is_refused_with_the_reason(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("refuse");
    let handle = open(cx, &far);
    for kind in [
        CommandKind::QuitShelbi,
        CommandKind::AddProject,
        CommandKind::NewWindow,
    ] {
        update(cx, handle, |w, _, _| w.banner = None);
        run(cx, handle, kind.clone());
        let (banner, overlay) = read(cx, handle, |w, _| (w.banner.clone(), w.overlay.is_some()));
        let reason = remote_window::command_unavailable(&kind, HOST).unwrap();
        assert_eq!(banner.as_deref(), Some(reason.as_str()), "{kind:?}");
        assert!(!overlay, "{kind:?} opened nothing");
    }
    // No window opened beside it (New Window, the Add Project wizard).
    assert_eq!(cx.update(|cx| shelbi_windows(cx)).len(), 1);

    // The palette lists them with the reason; the menus draw them disabled.
    let entry = read(cx, handle, |w, _| {
        w.palette_entries()
            .into_iter()
            .find(|e| e.id == "action:quit-shelbi")
            .unwrap()
    });
    assert_eq!(
        entry.subtitle,
        remote_window::command_unavailable(&CommandKind::QuitShelbi, HOST)
    );
}

#[gpui::test]
fn open_in_terminal_view_opens_the_far_tui_beside_the_native_window(cx: &mut TestAppContext) {
    use crate::views::remote_session::tests::TERMINAL_OPENED;
    TERMINAL_OPENED.with(|o| o.borrow_mut().clear());
    setup(cx);
    let far = FarHost::new("terminal-view");
    let handle = open(cx, &far);

    // The palette lists it in a remote window, as available.
    let entry = read(cx, handle, |w, _| {
        w.palette_entries()
            .into_iter()
            .find(|e| e.id == "action:open-terminal-view")
    });
    let entry = entry.expect("a remote window offers Open in Terminal View");
    assert_eq!(entry.label, "Open in Terminal View");
    assert!(read(cx, handle, |w, _| w.command_model().unavailable(&CommandKind::OpenTerminalView)).is_none());

    run(cx, handle, CommandKind::OpenTerminalView);
    let opened = TERMINAL_OPENED.with(|o| o.borrow().clone());
    assert_eq!(opened, vec![far.session()], "the far TUI opens for this window's project");
    // The native window stays, as it was.
    assert!(read(cx, handle, |w, _| w.remote_changed.is_none() && w.is_remote()));
    assert_eq!(cx.update(|cx| shelbi_windows(cx)).len(), 1);
}

#[gpui::test]
fn a_host_that_changed_closes_the_panes_and_reopen_decides_again(cx: &mut TestAppContext) {
    use crate::views::remote_session::tests::OPENED;
    OPENED.with(|o| o.borrow_mut().clear());
    setup(cx);
    let far = FarHost::new("changed");
    let handle = open(cx, &far);
    run(cx, handle, CommandKind::ShowView(View::Issues));
    draw(cx, handle);
    assert!(read(cx, handle, |w, _| w.issues.is_some()));

    // The far Shelbi was upgraded while the link was down: the window's own
    // reconnect check refuses it, and never opens a channel to it.
    far.remote.set_far_version(HOST, "0.0.2", Some(shelbi_app::launch::NativeCapability::current()));
    let down = HostLink::new(HOST, |_| panic!("no channel opens to a host that changed"));
    let refused = RemoteWindow::keep_up(&down, far.remote.as_ref(), &far.session());
    let Err(Some(RemoteError::NoLongerNative { message, .. })) = refused else {
        panic!("expected the changed host to be refused, got {refused:?}");
    };
    assert!(message.contains("is now 0.0.2"), "{message}");

    // The window shows that with Reopen in place of its shell, and its panes
    // and views close.
    update(cx, handle, |w, _, cx| w.host_changed(message.clone(), cx));
    draw(cx, handle);
    let (changed, open) = read(cx, handle, |w, _| {
        (
            w.remote_changed.clone(),
            w.chat.is_some() || w.issues.is_some() || w.workspace.is_some() || w.review.is_some(),
        )
    });
    assert_eq!(changed.as_deref(), Some(message.as_str()));
    assert!(!open, "the panes are closed");

    // Reopen closes the window and opens the project again, deciding afresh.
    update(cx, handle, |w, window, cx| w.reopen_remote(window, cx));
    let opened = OPENED.with(|o| o.borrow().clone());
    assert_eq!(opened, vec![far.session()]);
}

#[gpui::test]
fn a_refused_sign_in_stops_asking_and_the_bar_signs_in_again(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("signin");
    let handle = open(cx, &far);
    draw(cx, handle);

    // The link dropped and the master went with it. The reconnect's sign-in
    // dialog got Cancel: from here on nothing may put it back up unasked.
    let refused = RemoteError::AuthFailed {
        host: HOST.into(),
        reason: "Permission denied (publickey,password).".into(),
        asked: true,
    };
    let saying_no = FakeRemote::new().failing_connect(refused.clone());
    let down = HostLink::new(HOST, |_| panic!("no channel opens without a sign-in"))
        .with_pause(Duration::ZERO);
    let session = far.session();
    for _ in 0..3 {
        let kept = RemoteWindow::keep_up(&down, &saying_no, &session);
        assert_eq!(kept, Err(Some(refused.clone())), "the window answers this one");
    }
    let asks = AtomicU32::new(0);
    let answered = down.connect(&|| {
        asks.fetch_add(1, Ordering::SeqCst);
        Ok(())
    });
    assert_eq!(answered.err(), Some(refused.clone()));
    assert_eq!(asks.load(Ordering::SeqCst), 0, "the held answer runs no check, so no dialog");

    // The window shows it with Sign In, in plain words.
    update(cx, handle, |w, _, cx| {
        w.remote_bar = Some(RemoteBar::of(&refused));
        cx.notify();
    });
    draw(cx, handle);
    let bar = read(cx, handle, |w, _| w.remote_bar.clone()).expect("the bar");
    assert_eq!(bar.label(), shelbi_app::launch::remote::SIGN_IN_LABEL);
    assert!(bar.line().contains(&format!("ssh-copy-id {HOST}")), "{}", bar.line());
    let mut vcx = VisualTestContext::from_window(handle.into(), cx);
    assert!(vcx.debug_bounds("remote-sign-in").is_some());
    assert!(vcx.debug_bounds("remote-stopped").is_none());

    // Sign In: the link comes up, and the bar goes.
    update(cx, handle, |w, _, cx| w.remote_bar_action(cx));
    settle(cx, "the bar to clear", |cx| read(cx, handle, |w, _| w.remote_bar.is_none()));
}

#[test]
fn a_stopped_far_daemon_keeps_its_start_bar() {
    let bar = RemoteBar::of(&RemoteError::DaemonStopped { host: "devbox".into() });
    assert_eq!(bar.id(), "remote-stopped");
    assert_eq!(bar.label(), shelbi_app::launch::remote::START_DAEMON_LABEL);
    assert_eq!(bar.line().as_ref(), "Shelbi isn't running on devbox.");
    // An unreachable host, or a refusal nobody was asked for, just keeps trying.
    assert!(RemoteWindow::refusal(RemoteError::Unreachable { host: "devbox".into(), reason: "x".into() }).is_none());
    let unasked = RemoteError::AuthFailed { host: "devbox".into(), reason: "x".into(), asked: false };
    assert!(RemoteWindow::refusal(unasked).is_none());
}

#[gpui::test]
fn zen_and_the_error_log_are_written_by_the_far_daemon(cx: &mut TestAppContext) {
    use shelbi_proto::control::StateWrite;
    setup(cx);
    let far = FarHost::new("writes");
    // The intro is the viewer's own (this machine's), already seen.
    shelbi_state::mark_zen_intro_seen().unwrap();
    shelbi_state::append_error(PROJECT, "the far board didn't refresh", Some("poller")).unwrap();
    let handle = open(cx, &far);
    let wrote = |what: fn(&StateWrite) -> bool| far.writes.lock().unwrap().iter().any(what);

    run(cx, handle, CommandKind::ToggleZen);
    settle(cx, "the far daemon to flip Zen", |_| wrote(|w| matches!(w, StateWrite::ToggleZen { .. })));
    assert_eq!(read(cx, handle, |w, _| w.banner.clone()), None, "nothing was refused");
    assert_eq!(
        shelbi_state::read_state(PROJECT).unwrap().zen_mode,
        shelbi_state::ZenModeState::On
    );

    run(cx, handle, CommandKind::OpenErrorLog);
    settle(cx, "the far daemon to mark the log read", |_| wrote(|w| *w == StateWrite::MarkErrorsRead));
    assert_eq!(shelbi_state::unread_error_count(PROJECT).unwrap(), 0);

    update(cx, handle, |w, window, cx| {
        w.apply_overlay_event(
            OverlayEvent::ClearErrors {
                project: PROJECT.into(),
            },
            window,
            cx,
        )
    });
    settle(cx, "the far daemon to clear the log", |_| wrote(|w| *w == StateWrite::ClearErrors));
    assert_eq!(read(cx, handle, |w, _| w.banner.clone()), None, "the clear wasn't refused");
    assert!(shelbi_state::read_errors(PROJECT).unwrap().is_empty());
}


#[gpui::test]
fn a_workflow_saved_in_a_remote_window_writes_the_far_file_and_a_far_edit_is_refused(cx: &mut TestAppContext) {
    use shelbi_app::settings::workflows::{commit, Edit};
    setup(cx);
    let far = FarHost::new("save");
    let statuses = shelbi_state::workflows_dir(PROJECT).unwrap().join("statuses.yaml");
    let text = std::fs::read_to_string(&statuses).unwrap();
    std::fs::write(&statuses, format!("# The far team's catalog.\n{text}")).unwrap();
    let handle = open(cx, &far);
    let project = read(cx, handle, |w, _| w.project_handle.clone());

    // The page's save, as it runs it: through the window's handle, so the
    // far daemon writes the far file, with its comment kept.
    let config = project.source.workflow_config().unwrap();
    let rename = || Edit::RenameStatus {
        id: "todo".into(),
        name: "Up next".into(),
    };
    let done = commit(&project, &config, rename());
    assert_eq!(done.error, None);
    let saved = std::fs::read_to_string(&statuses).unwrap();
    assert!(saved.starts_with("# The far team's catalog.\n"), "{saved}");
    assert!(saved.contains("name: Up next"), "{saved}");
    assert_eq!(saved.lines().count(), text.lines().count() + 1, "only the one line changed");

    // Someone on the far host edits it meanwhile: a save from the model the
    // window read before that is refused, naming the host, and nothing is
    // written.
    let stale = done.config.expect("the model as saved, read back from the far side");
    std::fs::write(&statuses, format!("{saved}# edited on {HOST}\n")).unwrap();
    let refused = commit(
        &project,
        &stale,
        Edit::RenameStatus {
            id: "todo".into(),
            name: "Later".into(),
        },
    );
    let error = refused.error.expect("a stale save is refused");
    assert!(error.starts_with(&format!("Changed on {HOST} since it was loaded")), "{error}");
    assert!(std::fs::read_to_string(&statuses).unwrap().ends_with(&format!("# edited on {HOST}\n")));
    assert!(
        refused.config.is_some_and(|c| c.status("todo").unwrap().name == "Up next"),
        "the page reloads the far files"
    );

    // The workflow editor: its save writes the far workflow file, and one
    // made over a far edit says so, offering the reload.
    use shelbi_app::settings::workflow_editor::{run_save, Change, EditorState, Field};
    let task_yaml = shelbi_state::workflows_dir(PROJECT).unwrap().join("task.yaml");
    let mut editor = EditorState::new(project.source.workflow_config().unwrap(), "task");
    editor.change(Change::Description("Far work.".into()));
    let job = editor.begin_save(Vec::new()).expect("a save");
    editor.saved(run_save(job, &project));
    assert_eq!(editor.error_at(&Field::Save), None);
    let task = std::fs::read_to_string(&task_yaml).unwrap();
    assert!(task.contains("description: Far work."), "{task}");

    std::fs::write(&task_yaml, format!("{task}# edited on {HOST}\n")).unwrap();
    editor.change(Change::Description("Nearer work.".into()));
    let job = editor.begin_save(Vec::new()).expect("a save");
    editor.saved(run_save(job, &project));
    let error = editor.error_at(&Field::Save).expect("refused").to_string();
    assert!(error.starts_with(&format!("Changed on {HOST} since it was loaded")), "{error}");
    assert!(error.ends_with("Discard to reload them."), "{error}");
    assert!(editor.discard(), "Discard reads the far files again");
    assert!(std::fs::read_to_string(&task_yaml).unwrap().contains("description: Far work."));
}

#[gpui::test]
fn edit_in_a_remote_window_opens_the_far_editor_in_a_far_session(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("edit");
    let handle = open(cx, &far);
    crate::views::far_editor::tests::SHOWN.with(|s| s.borrow_mut().clear());

    run(
        cx,
        handle,
        CommandKind::OpenEditor {
            target: EditTarget::Workflows,
        },
    );
    settle(cx, "the far editor session", |_| {
        crate::views::far_editor::tests::SHOWN.with(|s| !s.borrow().is_empty())
    });
    let (session, label) = crate::views::far_editor::tests::SHOWN.with(|s| s.borrow()[0].clone());
    assert_eq!(label, "workflows");
    assert!(session.starts_with(&format!("{PROJECT}/edit/")), "{session}");
    let asked = far.asked.lock().unwrap();
    let (name, cwd, argv) = &asked.editors[0];
    assert_eq!(name, &session, "the window attaches to the session the far daemon started");
    let dir = shelbi_state::workflows_dir(PROJECT).unwrap();
    assert_eq!(cwd, &dir, "the far $EDITOR opens in the far directory");
    assert_eq!(argv[4], dir.to_string_lossy(), "on the far path: {argv:?}");
    assert_eq!(read(cx, handle, |w, _| w.banner.clone()), None);
}

#[gpui::test]
fn a_file_dropped_on_a_far_terminal_lands_on_the_far_machine(cx: &mut TestAppContext) {
    setup(cx);
    let far = FarHost::new("drop");
    let handle = open(cx, &far);
    let link = read(cx, handle, |w, _| w.project_handle.link.clone());
    let local = std::env::temp_dir().join(format!("rwf-drop-{}.png", std::process::id()));
    std::fs::write(&local, b"\x89PNG not really").unwrap();

    let pasted = shelbi_app::far::upload(&link, std::slice::from_ref(&local)).unwrap();
    assert_eq!(pasted.len(), 1);
    assert!(pasted[0].starts_with(far.drops()), "a far temp path: {pasted:?}");
    assert_eq!(pasted[0].file_name(), local.file_name());
    assert_eq!(std::fs::read(&pasted[0]).unwrap(), b"\x89PNG not really", "the far file holds the drop");

    // A folder isn't sent.
    let folder = shelbi_app::far::upload(&link, &[std::env::temp_dir()]).unwrap_err();
    assert!(folder.contains("is a folder") && folder.contains(HOST), "{folder}");
    let _ = std::fs::remove_file(&local);
}

#[gpui::test]
fn a_remote_windows_machine_form_checks_and_saves_on_the_far_hub(cx: &mut TestAppContext) {
    use shelbi_app::far::Request;
    use shelbi_app::settings::machine_check::{ReadinessIntent, ReadinessRequest, ReadinessTarget};
    use shelbi_app::settings::machine_form::SaveRequest;
    setup(cx);
    let far = FarHost::new("machine");
    let handle = open(cx, &far);
    let link = read(cx, handle, |w, _| w.project_handle.link.clone());
    let engine = crate::views::machine_form::Engine::far(&link);

    let request = ReadinessRequest {
        run: 1,
        intent: ReadinessIntent::Test,
        target: ReadinessTarget {
            project: PROJECT.into(),
            machine: None,
            name: "Build".into(),
            host: "build".into(),
            work_dir: "/srv/far".into(),
            record_as: None,
        },
    };
    let outcome = (engine.readiness)(&request, Default::default());
    assert!(outcome.is_ready(), "the far hub's answer: {outcome:?}");

    let fields = shelbi_core::pool::MachineFields {
        name: "Build".into(),
        kind: shelbi_core::MachineKind::Ssh,
        host: Some("build".into()),
        work_dir: "/srv/far".into(),
        tags: Vec::new(),
    };
    let change = (engine.save)(PROJECT, &SaveRequest::Add(fields.clone())).unwrap();
    assert_eq!(change.machine.name, "Build");
    let asked = far.asked.lock().unwrap().requests.clone();
    assert_eq!(asked, [Request::CheckMachine(request), Request::AddMachine(fields)]);
}
