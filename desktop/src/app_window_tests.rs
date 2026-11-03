//! The window model (`dt-switch-project-in-window`): a project switch
//! retargets the current window in place, a switch back restores where the
//! project was left with nothing left behind (no second feed, no stale
//! session attach), New Window opens a second window that switches on its
//! own, a notification goes to the window showing its project, quitting the
//! shown project keeps the window, and a launch reopens the windows that
//! were open.
//!
//! Windows open through the app's own `open_window` (so [`Shell`] tracks
//! them) as inert test windows: commands that reach the executor are recorded
//! by each window's tap, the feeds are canned ([`crate::feeds::tests`]), and
//! the chat and workspace sessions are fakes that report which attaches are
//! still connected.

use std::sync::Arc;

use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
use shelbi_app::theme::ThemePreference;

use super::smoke::fixture_model;
use super::*;
use crate::feeds::tests::{canned_hub, Started};
use crate::projects::ProjectEntry;
use crate::views::workspace::tests::{fake_deps as fake_workspace_deps, FakeSessions};

struct Fixture {
    hub: FeedHub,
    /// The feeds started, and which still run.
    feeds: Started,
    sessions: Arc<FakeSessions>,
    deps: WorkspaceDeps,
}

fn entry(name: &str, label: &str) -> ProjectEntry {
    ProjectEntry {
        name: name.into(),
        label: label.into(),
    }
}

/// Two projects in the catalog: alpha (the smoke fixture's sidebar, with its
/// workspace `alpha-1`) and beta (a bare skeleton).
fn fixture(cx: &mut TestAppContext) -> Fixture {
    let (hub, feeds) = canned_hub();
    let (deps, sessions, _ops) = fake_workspace_deps();
    let mut alpha = fixture_model();
    alpha.project_label = "Alpha".into();
    let mut beta = SidebarModel::skeleton("Beta");
    beta.board_loading = false;
    cx.update(|cx| {
        cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
        gpui_kit::init(cx);
        cx.set_global(Shell {
            inert_windows: true,
            ..Shell::default()
        });
        cx.set_global(hub.clone());
        cx.set_global(Catalog::with(
            vec![entry("alpha", "Alpha"), entry("beta", "Beta"), entry("gamma", "Gamma")],
            vec![alpha, beta, SidebarModel::skeleton("Gamma")],
        ));
    });
    Fixture {
        hub,
        feeds,
        sessions,
        deps,
    }
}

/// Open a window on `project` in `slot`, with fake sessions and a running
/// feed.
fn open(cx: &mut TestAppContext, f: &Fixture, project: &str, slot: usize) -> WindowHandle<ShelbiWindow> {
    let deps = f.deps.clone();
    cx.update(|cx| {
        open_window(project, slot, WindowState::default(), None, cx);
        let handle = cx.global::<Shell>().windows.last().unwrap().handle;
        handle
            .update(cx, |w, window, cx| {
                w.chat_deps = Some(deps.sessions.clone());
                w.workspace_deps = Some(deps);
                w.start_feed(window, cx);
            })
            .unwrap();
        handle
    })
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

/// Draw one frame of `handle`, then let foreground work run (the frames it
/// asks for included).
fn draw(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) {
    draw_only(cx, handle);
    cx.run_until_parked();
}

/// Draw one frame of `handle` and nothing else: what the user sees first.
fn draw_only(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) {
    cx.update(|cx| {
        cx.update_window(*handle, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap()
    });
}

fn windows(cx: &mut TestAppContext) -> Vec<(String, usize)> {
    cx.update(|cx| {
        cx.global::<Shell>()
            .windows
            .iter()
            .map(|w| (w.handle.read(cx).unwrap().project.clone(), w.slot))
            .collect()
    })
}

fn taken(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) -> Vec<HostCall> {
    cx.update(|cx| {
        let tap = handle.read(cx).unwrap().tap.clone().unwrap();
        let calls = std::mem::take(&mut *tap.borrow_mut());
        calls
    })
}

/// Live session attaches, as `<project>` / anything-else counts.
fn live_attaches(f: &Fixture, project: &str) -> usize {
    f.sessions.live().iter().filter(|n| n.contains(project)).count()
}

/// Leave alpha on its workspace, the workspace row selected: the state a
/// switch away must save.
fn leave_alpha_on_its_workspace(cx: &mut TestAppContext, h: WindowHandle<ShelbiWindow>) {
    update(cx, h, |w, _, _| {
        w.adopt_view_state(ProjectViewState {
            view: Some("alpha-1".into()),
            selected: Some("workspace:alpha-1".into()),
            workspace_return: Some("orch".into()),
            ..Default::default()
        })
    });
    draw(cx, h);
    draw(cx, h);
}

#[gpui::test]
fn a_switch_retargets_this_window_with_no_new_window(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    let before = cx.update(|cx| cx.windows().len());
    update(cx, h, |w, window, cx| w.perform(HostCall::SwitchProject("beta".into()), window, cx));
    assert_eq!(cx.update(|cx| cx.windows().len()), before, "no window opened");
    assert_eq!(windows(cx), [("beta".to_string(), 0)]);
    // The first frame paints beta's prefetched skeleton, in the same window.
    assert_eq!(read(cx, h, |w, _| w.model.project_label.clone()), "Beta");
    assert_eq!(f.hub.subscribers(), [(ProjectKey::local("beta"), 1)], "alpha's feed is gone");
    // The window's slot now names beta.
    let slot = cx.update(|cx| cx.global::<Shell>().session.slots[0].clone());
    assert_eq!(slot.project, Some(ProjectKey::local("beta")));
    assert!(slot.open);
    // Switching to the project already shown does nothing.
    update(cx, h, |w, window, cx| w.perform(HostCall::SwitchProject("beta".into()), window, cx));
    assert_eq!(f.hub.spawned(), 2);
}

#[gpui::test]
fn the_switch_paints_the_sidebar_then_attaches_after_that_frame(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    draw(cx, h);
    // The switch's update ends with the window's next frame drawn (gpui draws
    // a dirtied window as the update finishes).
    cx.update(|cx| h.update(cx, |w, window, cx| w.switch_to("beta", window, cx)).unwrap());
    read(cx, h, |w, _| {
        assert!(
            w.switch_hold.as_ref().is_some_and(|hold| hold.rendered),
            "a frame has been drawn"
        );
        // It painted beta's sidebar and nothing more: the chat (beta was
        // never left, so it opens there) isn't built or attached yet.
        assert_eq!(w.model.project_label, "Beta");
        assert_eq!(w.client.view(), &View::Session("orch".into()));
        assert!(w.chat.is_none(), "the first frame only paints the sidebar");
    });
    assert_eq!(live_attaches(&f, "beta"), 0);
    // The frame after builds the chat and attaches it.
    draw(cx, h);
    assert!(read(cx, h, |w, _| w.chat.is_some()), "the next frame builds the chat");
    assert_eq!(live_attaches(&f, "beta"), 1, "and attaches it");
}

#[gpui::test]
fn a_to_b_to_a_restores_alpha_with_nothing_left_behind(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    leave_alpha_on_its_workspace(cx, h);
    assert_eq!(live_attaches(&f, "alpha"), 1, "alpha's workspace agent is attached");
    let workspace_row = read(cx, h, |w, cx| w.selected(cx));

    // A → B: alpha's views and sessions go, beta's chat comes. (Scrolled
    // just before: a test sidebar is too short to stay scrolled once drawn.)
    update(cx, h, |w, window, cx| {
        w.sidebar_scroll.set_offset(gpui::point(px(0.0), px(-40.0)));
        w.switch_to("beta", window, cx)
    });
    draw(cx, h);
    draw(cx, h);
    assert_eq!(live_attaches(&f, "alpha"), 0, "alpha's agent detached");
    assert_eq!(live_attaches(&f, "beta"), 1);
    assert!(read(cx, h, |w, _| w.workspace.is_none()));
    // Beta's sidebar has the row for beta's view selected, not alpha's row.
    read(cx, h, |w, cx| {
        assert_eq!(w.client.view(), &View::Session("orch".into()));
        assert_eq!(w.selected(cx), Some(SidebarTarget::View(View::Session("orch".into()))));
    });

    // B → A: alpha comes back as it was left, its sidebar scroll before
    // its first frame (which a test sidebar, too short to scroll, would undo).
    let scroll = cx.update(|cx| {
        h.update(cx, |w, window, cx| {
            w.switch_to("alpha", window, cx);
            w.sidebar_scroll.offset().y
        })
        .unwrap()
    });
    assert_eq!(scroll, px(-40.0), "the sidebar's scroll is back");
    cx.run_until_parked();
    draw(cx, h);
    draw(cx, h);
    read(cx, h, |w, cx| {
        assert_eq!(w.client.view(), &View::Session("alpha-1".into()), "its view");
        assert_eq!(w.selected(cx), workspace_row, "its selection");
        assert_eq!(w.workspace_return, Some(View::Session("orch".into())), "its panel's Back");
        assert_eq!(w.column.target(), Occupant::Workspace, "its panel");
        assert!(!w.column.is_animating(), "put straight back, no push");
    });
    // One feed (alpha's), one subscriber; alpha's first feed and beta's have
    // stopped.
    assert_eq!(f.hub.subscribers(), [(ProjectKey::local("alpha"), 1)]);
    assert_eq!(f.hub.spawned(), 3);
    assert_eq!(f.feeds.running(), 1);
    // One live attach: alpha's agent. Beta's chat is closed, and alpha's
    // first attach was not kept alongside the new one.
    assert_eq!(f.sessions.live().len(), 1, "live: {:?}", f.sessions.live());
    assert_eq!(live_attaches(&f, "alpha"), 1);
}

#[gpui::test]
fn a_restored_workspace_the_project_no_longer_has_gives_way_to_the_chat(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    update(cx, h, |w, _, _| {
        w.adopt_view_state(ProjectViewState {
            view: Some("removed".into()),
            workspace_return: Some("tasks".into()),
            ..Default::default()
        })
    });
    draw(cx, h);
    draw(cx, h);
    read(cx, h, |w, _| {
        assert_eq!(w.client.view(), &View::Session("orch".into()));
        assert_eq!(w.workspace_return, None);
        assert!(w.workspace.is_none());
    });
}

#[gpui::test]
fn new_window_opens_a_second_window_that_switches_on_its_own(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let a = open(cx, &f, "alpha", 0);
    update(cx, a, |w, window, cx| w.perform(HostCall::NewWindow, window, cx));
    assert_eq!(windows(cx), [("alpha".to_string(), 0), ("alpha".to_string(), 1)]);
    assert_eq!(cx.update(|cx| cx.windows().len()), 2);
    let b = cx.update(|cx| cx.global::<Shell>().windows[1].handle);
    // Its frame cascades off the first's.
    let (pa, pb) = cx.update(|cx| {
        let mut at = |h: WindowHandle<ShelbiWindow>| {
            h.update(cx, |_, window, _| window.window_bounds().get_bounds().origin)
                .unwrap()
        };
        (at(a), at(b))
    });
    assert_eq!(pb.x - pa.x, px(CASCADE));
    // It switches without moving the first.
    update(cx, b, |w, window, cx| w.switch_to("beta", window, cx));
    assert_eq!(windows(cx), [("alpha".to_string(), 0), ("beta".to_string(), 1)]);
    let slots = cx.update(|cx| cx.global::<Shell>().session.slots.clone());
    assert_eq!(slots[0].project, Some(ProjectKey::local("alpha")));
    assert_eq!(slots[1].project, Some(ProjectKey::local("beta")));
    // ⌘⇧N is on the command, and the palette offers it.
    assert!(read(cx, a, |w, _| w.command_model().multi_window));
    assert_eq!(
        CommandKind::from_id("action:new-window"),
        Some(CommandKind::NewWindow)
    );
}

#[gpui::test]
fn two_windows_on_one_project_share_its_feed_and_one_posts_notices(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let a = open(cx, &f, "alpha", 0);
    let b = open(cx, &f, "alpha", 1);
    assert_eq!(f.hub.subscribers(), [(ProjectKey::local("alpha"), 2)]);
    assert_eq!(f.hub.spawned(), 1);
    // Asked as a feed event asks it: from inside the window's own update.
    assert!(update(cx, a, |w, _, cx| w.posts_notices(cx)));
    assert!(!update(cx, b, |w, _, cx| w.posts_notices(cx)), "the second stays quiet");
    update(cx, a, |w, window, cx| w.switch_to("beta", window, cx));
    assert!(update(cx, b, |w, _, cx| w.posts_notices(cx)), "now it is alpha's only window");
}

#[gpui::test]
fn a_notice_goes_to_the_window_showing_its_project(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let a = open(cx, &f, "alpha", 0);
    let b = open(cx, &f, "beta", 1);
    taken(cx, a);
    taken(cx, b);
    cx.update(|cx| open_notice_tag("shelbi/beta/orch", cx));
    cx.run_until_parked();
    assert_eq!(taken(cx, b), [HostCall::ShowView(View::Session("orch".into()))]);
    assert_eq!(taken(cx, a), []);
    assert_eq!(windows(cx), [("alpha".to_string(), 0), ("beta".to_string(), 1)]);
}

#[gpui::test]
fn a_notice_for_a_project_no_window_shows_switches_the_frontmost(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let a = open(cx, &f, "alpha", 0);
    let b = open(cx, &f, "beta", 1);
    update(cx, a, |_, window, _| window.activate_window());
    taken(cx, a);
    taken(cx, b);
    cx.update(|cx| open_notice_tag("shelbi/gamma/ws/g-1", cx));
    cx.run_until_parked();
    assert_eq!(windows(cx), [("gamma".to_string(), 0), ("beta".to_string(), 1)]);
    assert_eq!(
        taken(cx, a),
        [HostCall::ShowView(View::Session("g-1".into()))],
        "and opens the item there"
    );
    // The notice's target wins over the view gamma was left on.
    assert!(read(cx, a, |w, _| w.switch_hold.is_none()));
}

#[gpui::test]
fn quitting_the_shown_project_keeps_the_window(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    // Another project is open: the window moves to it.
    update(cx, h, |w, window, cx| w.leave_quit_project(Some("beta".into()), window, cx));
    assert_eq!(windows(cx), [("beta".to_string(), 0)]);
    // None left (a test window finds none): the chooser, window still open.
    update(cx, h, |w, window, cx| w.after_quit_project("beta".into(), window, cx));
    assert!(read(cx, h, |w, _| w.chooser));
    assert_eq!(cx.update(|cx| cx.windows().len()), 1, "the window stays");
    assert!(f.hub.subscribers().is_empty(), "the quit project's feed is dropped");
    assert_eq!(
        cx.update(|cx| cx.global::<Shell>().session.slots[0].project.clone()),
        None
    );
    let mut vcx = VisualTestContext::from_window(h.into(), cx);
    vcx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(vcx.debug_bounds("chooser").is_some(), "the chooser is drawn");
    assert!(vcx.debug_bounds("chooser-alpha").is_some());
    assert!(vcx.debug_bounds("chooser-add").is_some());
    // Its Add Project… row asks for the wizard.
    taken(cx, h);
    let add = vcx.debug_bounds("chooser-add").unwrap();
    vcx.simulate_click(add.center(), gpui::Modifiers::none());
    assert_eq!(taken(cx, h), [HostCall::OpenAddProject]);
    // Its keys pick a project (the tap records the switch); a menu offers
    // nothing that needs a project.
    update(cx, h, |w, window, cx| {
        assert!(w.chooser_key(Key::Down, window, cx));
        assert!(w.chooser_key(Key::Enter, window, cx));
    });
    assert_eq!(taken(cx, h), [HostCall::SwitchProject("beta".into())]);
    let model = read(cx, h, |w, _| w.command_model());
    assert_eq!(model.project, None);
    assert!(!model.multi_window);
    // Picking one leaves the chooser.
    update(cx, h, |w, window, cx| w.switch_to("alpha", window, cx));
    assert!(!read(cx, h, |w, _| w.chooser));
    assert_eq!(windows(cx), [("alpha".to_string(), 0)]);
}

#[gpui::test]
fn a_launch_reopens_the_open_windows_and_a_close_forgets_one(cx: &mut TestAppContext) {
    let _f = fixture(cx);
    let plan = vec![
        LaunchWindow {
            slot: 0,
            project: ProjectKey::local("alpha"),
            state: WindowState::default(),
        },
        LaunchWindow {
            slot: 2,
            project: ProjectKey::local("beta"),
            state: WindowState {
                sidebar_width: 33,
                task_pane_width: 820.0,
                ..WindowState::default()
            },
        },
    ];
    cx.update(|cx| {
        cx.on_window_closed(|cx, id| window_closed(id, cx)).detach();
        open_launch_windows(plan, cx);
    });
    assert_eq!(windows(cx), [("alpha".to_string(), 0), ("beta".to_string(), 2)]);
    let beta = cx.update(|cx| cx.global::<Shell>().windows[1].handle);
    assert_eq!(read(cx, beta, |w, _| w.client.sidebar_width()), 33, "its slot's sidebar");
    assert_eq!(read(cx, beta, |w, _| w.window_state.task_pane_width), 820.0, "and its task pane");
    // Closing one of two: its slot isn't reopened next launch.
    update(cx, beta, |_, window, _| window.remove_window());
    assert_eq!(windows(cx), [("alpha".to_string(), 0)]);
    let slots = cx.update(|cx| cx.global::<Shell>().session.slots.clone());
    assert!(slots[0].open);
    assert!(!slots[2].open);
    assert_eq!(slots[2].state.task_pane_width, 820.0, "a closed slot keeps its pane width");
    // Closing the last (the app quits): it stays open, to come back.
    let alpha = cx.update(|cx| cx.global::<Shell>().windows[0].handle);
    update(cx, alpha, |_, window, _| window.remove_window());
    assert!(cx.update(|cx| cx.global::<Shell>().session.slots[0].open));
}

#[gpui::test]
fn add_project_opens_the_wizard_window_once(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    let before = cx.update(|cx| cx.windows().len());
    for _ in 0..2 {
        update(cx, h, |w, window, cx| w.perform(HostCall::OpenAddProject, window, cx));
        cx.run_until_parked();
    }
    assert_eq!(cx.update(|cx| cx.windows().len()), before + 1, "one wizard, raised the second time");
    let wizard = cx
        .update(|cx| cx.windows().into_iter().find_map(|w| w.downcast::<crate::views::add_project::AddProjectView>()))
        .expect("the wizard window");
    let screen = cx.update(|cx| wizard.read(cx).unwrap().wizard().screen());
    assert_eq!(screen, shelbi_app::add_project_wizard::Screen::Folder);
    // Cancelling closes it and writes nothing; the project window stays.
    wizard
        .update(cx, |_, window, cx| window.dispatch_action(Box::new(CloseWindow), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| cx.windows().len()), before);
    assert_eq!(windows(cx), [("alpha".to_string(), 0)]);
}

/// The Add Project wizard's Okay from a window on alpha that was left in
/// Settings, with view state saved for the new project's id from an earlier
/// project: the same window lands on gamma's Orchestrator, Settings closed,
/// nothing of the old state restored, and no window opened.
#[gpui::test]
fn okay_from_a_project_window_lands_on_the_new_projects_orchestrator(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    leave_alpha_on_its_workspace(cx, h);
    update(cx, h, |w, window, cx| w.open_settings(None, window, cx));
    assert!(read(cx, h, |w, _| matches!(w.route, Route::Settings(_))));
    cx.update(|cx| {
        cx.global::<Catalog>().remember_view(
            &ProjectKey::local("gamma"),
            ProjectViewState {
                view: Some("tasks".into()),
                ..Default::default()
            },
        )
    });
    let before = cx.update(|cx| cx.windows().len());

    let opened = cx.update(|cx| open_added_project("gamma", Some(h), cx));
    cx.run_until_parked();
    assert_eq!(opened, Ok(()));
    draw(cx, h);
    draw(cx, h);
    assert_eq!(cx.update(|cx| cx.windows().len()), before, "no window opened");
    assert_eq!(windows(cx), [("gamma".to_string(), 0)]);
    read(cx, h, |w, _| {
        assert!(matches!(w.route, Route::View), "Settings stayed with alpha");
        assert_eq!(w.client.view(), &View::Session("orch".into()), "the Orchestrator, not gamma's old board");
        assert_eq!(w.model.project_label, "Gamma");
    });
    // alpha keeps where it was left.
    let alpha = cx.update(|cx| cx.global::<Catalog>().view_state(&ProjectKey::local("alpha")));
    assert_eq!(alpha.view.as_deref(), Some("alpha-1"));

    // Okay again (a retried open): the window that shows it comes up, still
    // one window.
    let again = cx.update(|cx| open_added_project("gamma", Some(h), cx));
    cx.run_until_parked();
    assert_eq!(again, Ok(()));
    assert_eq!(windows(cx), [("gamma".to_string(), 0)]);
}

/// The first launch's Okay: no window to switch, so one opens on the new
/// project's Orchestrator; Okay again reuses it.
#[gpui::test]
fn okay_on_the_first_launch_opens_one_window(cx: &mut TestAppContext) {
    let _f = fixture(cx);
    assert!(windows(cx).is_empty());
    assert_eq!(cx.update(|cx| open_added_project("gamma", None, cx)), Ok(()));
    cx.run_until_parked();
    assert_eq!(windows(cx), [("gamma".to_string(), 0)]);
    let h = cx.update(|cx| cx.global::<Shell>().windows[0].handle);
    draw(cx, h);
    draw(cx, h);
    assert_eq!(read(cx, h, |w, _| w.client.view().clone()), View::Session("orch".into()));
    assert_eq!(cx.update(|cx| open_added_project("gamma", None, cx)), Ok(()));
    cx.run_until_parked();
    assert_eq!(windows(cx), [("gamma".to_string(), 0)], "no second window");
}

/// A project the catalog doesn't list yet can't open: an error for the
/// wizard to show, and the window stays where it was.
#[gpui::test]
fn okay_for_a_project_the_catalog_lacks_is_an_error(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    let opened = cx.update(|cx| open_added_project("delta", Some(h), cx));
    assert!(opened.is_err());
    cx.run_until_parked();
    assert_eq!(windows(cx), [("alpha".to_string(), 0)]);
}

/// The primary display's visible area, as a saved frame.
fn visible_frame(cx: &mut TestAppContext) -> Frame {
    cx.update(|cx| bounds_frame(cx.primary_display().unwrap().visible_bounds()))
}

fn frame_of(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) -> Frame {
    cx.update(|cx| {
        handle
            .update(cx, |_, window, _| bounds_frame(window.window_bounds().get_bounds()))
            .unwrap()
    })
}

fn launch_with_frame(frame: Frame) -> Vec<LaunchWindow> {
    vec![LaunchWindow {
        slot: 0,
        project: ProjectKey::local("alpha"),
        state: WindowState {
            frame: Some(frame),
            ..WindowState::default()
        },
    }]
}

#[gpui::test]
fn a_window_saved_filling_the_screen_reopens_filling_it(cx: &mut TestAppContext) {
    let _f = fixture(cx);
    // The display's whole visible area, as a window zoomed or dragged to fill
    // it saves it (x is -0.0 as AppKit reports a window at the left edge).
    let full = Frame {
        x: -0.0,
        ..visible_frame(cx)
    };
    cx.update(|cx| {
        cx.on_window_closed(|cx, id| window_closed(id, cx)).detach();
        open_launch_windows(launch_with_frame(full), cx);
    });
    let h = cx.update(|cx| cx.global::<Shell>().windows[0].handle);
    assert_eq!(frame_of(cx, h), full, "it opens at exactly its saved frame");
    // Relaunch from what this launch saved, twice: the full frame stays.
    for launch in 1..=2 {
        let session = cx.update(|cx| cx.global::<Shell>().session.clone());
        assert_eq!(session.slots[0].state.frame, Some(full), "launch {launch} saved the full frame");
        let h = h_of(cx);
        update(cx, h, |_, window, _| window.remove_window());
        let plan = window_state::launch_plan(Some(&session), None, None, &|_| true, &|_| WindowState::default());
        cx.update(|cx| open_launch_windows(plan, cx));
        assert_eq!({ let h = h_of(cx); frame_of(cx, h) }, full, "relaunch {launch} opens it full");
    }
}

fn h_of(cx: &mut TestAppContext) -> WindowHandle<ShelbiWindow> {
    cx.update(|cx| cx.global::<Shell>().windows[0].handle)
}

#[gpui::test]
fn a_window_opened_narrower_than_asked_is_given_its_size(cx: &mut TestAppContext) {
    let _f = fixture(cx);
    let full = visible_frame(cx);
    cx.update(|cx| open_launch_windows(launch_with_frame(full), cx));
    let h = h_of(cx);
    // What Stage Manager does as the window first orders front: a narrower
    // window (gpui has already put the origin back).
    VisualTestContext::from_window(h.into(), cx)
        .simulate_resize(gpui::size(px(full.width * 0.87), px(full.height)));
    assert_ne!(frame_of(cx, h), full);
    update(cx, h, |_, window, _| hold_size(window, frame_bounds(full)));
    assert_eq!(frame_of(cx, h), full, "the window gets back its asked-for size");
}

#[gpui::test]
fn a_saved_frame_that_no_longer_fits_opens_on_the_display(cx: &mut TestAppContext) {
    let _f = fixture(cx);
    let area = visible_frame(cx);
    cx.update(|cx| cx.on_window_closed(|cx, id| window_closed(id, cx)).detach());
    for saved in [
        // On a display to the right that's gone.
        Frame { x: area.x + area.width + 200.0, y: area.y + 100.0, width: 900.0, height: 700.0 },
        // Bigger than this display (a larger one, or a higher resolution).
        Frame { x: area.x - 50.0, y: area.y, width: area.width + 400.0, height: area.height + 300.0 },
    ] {
        cx.update(|cx| open_launch_windows(launch_with_frame(saved), cx));
        let at = { let h = h_of(cx); frame_of(cx, h) };
        assert!(
            at.x >= area.x
                && at.y >= area.y
                && at.x + at.width <= area.x + area.width
                && at.y + at.height <= area.y + area.height,
            "{saved:?} opened at {at:?}, off the visible area {area:?}"
        );
        assert_eq!(at, saved.fit_within(area));
        let h = h_of(cx);
        update(cx, h, |_, window, _| window.remove_window());
    }
}

/// Attach display `id` beside the primary one (the test platform's displays
/// all share one size; each has its own UUID), as it would be saved.
fn attach_display(cx: &mut TestAppContext, id: u64) -> DisplayRef {
    cx.simulate_display_added(gpui::DisplayId::new(id), None);
    cx.update(|cx| display_ref(&*cx.find_display(gpui::DisplayId::new(id)).unwrap()))
}

fn primary_id(cx: &mut TestAppContext) -> Option<gpui::DisplayId> {
    cx.update(|cx| cx.primary_display().map(|d| d.id()))
}

#[gpui::test]
fn a_frame_saved_on_an_attached_display_opens_there(cx: &mut TestAppContext) {
    let external = attach_display(cx, 2);
    assert_ne!(Some(gpui::DisplayId::new(2)), primary_id(cx));
    let area = visible_frame(cx);
    let saved = Frame { x: 300.0, y: area.y + 80.0, width: 1000.0, height: 700.0 };
    let mut state = WindowState {
        frame: Some(saved),
        display: Some(external),
        ..WindowState::default()
    };
    let (bounds, display) = cx.update(|cx| placement(&state, None, cx));
    assert_eq!(display, Some(gpui::DisplayId::new(2)), "it opens on the display it was saved on");
    assert_eq!(bounds_frame(bounds), saved, "at exactly its saved frame");

    // That display is gone: the primary one, fully on screen.
    cx.simulate_display_removed(gpui::DisplayId::new(2));
    let off = Frame { x: area.x + area.width + 200.0, y: area.y + 100.0, width: 900.0, height: 700.0 };
    state.frame = Some(off);
    let (bounds, display) = cx.update(|cx| placement(&state, None, cx));
    assert_eq!(display, primary_id(cx));
    assert_eq!(bounds_frame(bounds), off.fit_within(area));

    // Another display attached under the gone one's id isn't it.
    attach_display(cx, 2);
    let (_, display) = cx.update(|cx| placement(&state, None, cx));
    assert_eq!(display, primary_id(cx), "a reused id doesn't match a saved UUID");
}

#[gpui::test]
fn a_frame_saved_before_displays_opens_on_the_primary_unchanged(cx: &mut TestAppContext) {
    attach_display(cx, 2);
    // A session file from before this change: no display, a frame filling
    // the primary's visible area (the full-width restore holds).
    let full = Frame { x: -0.0, ..visible_frame(cx) };
    let state = WindowState {
        frame: Some(full),
        ..WindowState::default()
    };
    let (bounds, display) = cx.update(|cx| placement(&state, None, cx));
    assert_eq!(display, primary_id(cx));
    assert_eq!(bounds_frame(bounds), full);
}

#[gpui::test]
fn a_window_saves_the_display_its_frame_is_on(cx: &mut TestAppContext) {
    let _f = fixture(cx);
    attach_display(cx, 2);
    let area = visible_frame(cx);
    cx.update(|cx| open_launch_windows(launch_with_frame(area), cx));
    let h = h_of(cx);
    let primary = cx.update(|cx| display_ref(&*cx.primary_display().unwrap()));
    assert!(primary.uuid.is_some(), "the UUID is saved, not only the id");
    // The user resizes it: the frame is saved with the display it's on (what
    // the window's bounds and display observers do; an inert test window
    // has none).
    VisualTestContext::from_window(h.into(), cx).simulate_resize(gpui::size(px(1000.0), px(700.0)));
    update(cx, h, |w, window, cx| w.record_frame(window, cx));
    let state = read(cx, h, |w, _| w.window_state.clone());
    assert_eq!(state.frame.map(|f| (f.width, f.height)), Some((1000.0, 700.0)));
    assert_eq!(state.display, Some(primary));
}
