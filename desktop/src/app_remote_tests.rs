//! The native remote window (remote-window P6): with the dev flag on and a
//! host on this exact version, Connect opens the full main window for the far
//! project, and every view reads it, and commands it, over the host's relay
//! link.
//!
//! The far host is [`crate::remote_fixture::FarHost`]: a fixture project read
//! by the far side's real query handler across an in-process relay, a fake far
//! daemon on the relay's `control.sock`, and a fake far hub. The window opens
//! through the app's own [`open_remote_window`] (an inert test window: no
//! bootstrap); its tap is then taken off so commands run for real, against
//! the far host. Feeds are canned ([`crate::feeds::tests`]): a live feed's
//! threads would wake gpui's test scheduler off its thread. The far `Changed`
//! reaching a feed is `refresh::tests`'s; here the board's own reads show it.

use std::time::Duration;

use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
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

/// Open `far`'s project as Connect does with the flag on: the listing
/// decides (natively: same version), then the window opens over the host's
/// link. Its commands then run for real.
fn open(cx: &mut TestAppContext, far: &FarHost) -> WindowHandle<ShelbiWindow> {
    let RemoteOpen::Native(listing) = remote_window::choose(true, HOST, || far.remote.listing(HOST)) else {
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
    ] {
        update(cx, handle, |w, window, cx| w.open_settings(Some(page), window, cx));
        draw(cx, handle);
    }
    // Read-only: an editor doesn't open, saying why.
    update(cx, handle, |w, window, cx| w.edit_workflow("task".into(), window, cx));
    let (edited, banner) = read(cx, handle, |w, _| (w.edited_workflows.clone(), w.banner.clone()));
    assert!(edited.is_empty(), "no editor opens on a remote project's workflow");
    assert_eq!(
        banner.as_deref(),
        Some(remote_window::settings_read_only(HOST).as_str())
    );
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
        CommandKind::ToggleZen,
        CommandKind::QuitShelbi,
        CommandKind::AddProject,
        CommandKind::NewWindow,
        CommandKind::OpenEditor {
            target: EditTarget::ZenMode,
        },
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
            .find(|e| e.id == "action:toggle-zen")
            .unwrap()
    });
    assert_eq!(
        entry.subtitle,
        remote_window::command_unavailable(&CommandKind::ToggleZen, HOST)
    );
}

