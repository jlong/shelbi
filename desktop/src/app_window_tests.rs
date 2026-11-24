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

use gpui::{point, Modifiers, MouseButton, Pixels, Point, TestAppContext, VisualTestContext, WindowAppearance};
use shelbi_app::theme::ThemePreference;

use super::smoke::fixture_model;
use super::*;
use crate::feeds::tests::{canned_hub, Started};
use crate::projects::ProjectEntry;
use crate::views::tab_tearoff::{self, FloatingGhost, CARD_H, CARD_W};
use crate::views::workspace::tests::{fake_deps as fake_workspace_deps, FakeSessions};

pub(super) struct Fixture {
    hub: FeedHub,
    /// The feeds started, and which still run.
    feeds: Started,
    sessions: Arc<FakeSessions>,
    pub(super) deps: WorkspaceDeps,
}

fn entry(name: &str, label: &str) -> ProjectEntry {
    ProjectEntry {
        name: name.into(),
        label: label.into(),
    }
}

/// Two projects in the catalog: alpha (the smoke fixture's sidebar, with its
/// workspace `alpha-1`) and beta (a bare skeleton).
pub(super) fn fixture(cx: &mut TestAppContext) -> Fixture {
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
pub(super) fn open(cx: &mut TestAppContext, f: &Fixture, project: &str, slot: usize) -> WindowHandle<ShelbiWindow> {
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

pub(super) fn update<R>(
    cx: &mut TestAppContext,
    handle: WindowHandle<ShelbiWindow>,
    f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>) -> R,
) -> R {
    let r = cx.update(|cx| handle.update(cx, f).unwrap());
    cx.run_until_parked();
    r
}

pub(super) fn read<R>(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, f: impl FnOnce(&ShelbiWindow, &App) -> R) -> R {
    cx.update(|cx| handle.read_with(cx, f).unwrap())
}

/// Draw one frame of `handle`, then let foreground work run (the frames it
/// asks for included).
pub(super) fn draw(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) {
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

pub(super) fn windows(cx: &mut TestAppContext) -> Vec<(String, usize)> {
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
    // alpha quit too, elsewhere: its tab goes, the window stays on beta.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("alpha"), window, cx));
    assert_eq!(windows(cx), [("beta".to_string(), 0)]);
    assert_eq!(read(cx, h, |w, _| w.tabs.order().to_vec()), ["beta"]);
    // None left (no tab, and a test window finds none open): the chooser,
    // window still open.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("beta"), window, cx));
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
    // No titlebar row: its top strip moves the window, clear of the rows.
    let strip = vcx.debug_bounds("chooser-drag").unwrap();
    assert_eq!((strip.top(), strip.size.height), (gpui::px(0.0), gpui::px(52.0)));
    assert!(vcx.debug_bounds("chooser-alpha").unwrap().top() >= strip.bottom());
    assert_eq!(
        crate::elements::window_drag::test_log::drag_from(&mut vcx, strip.center()),
        [crate::elements::window_drag::test_log::Call::Move]
    );
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
fn a_quit_finishing_after_a_switch_leaves_the_window_on_the_chosen_project(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    // The switch lands while the quit's completion looks for the next project.
    update(cx, h, |w, window, cx| {
        w.after_quit_project(ProjectKey::local("alpha"), window, cx);
        w.switch_to("beta", window, cx);
    });
    assert_eq!(windows(cx), [("beta".to_string(), 0)]);
    assert!(!read(cx, h, |w, _| w.chooser), "the quit doesn't move it off beta");
    // It lands before the quit finishes.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("alpha"), window, cx));
    assert_eq!(windows(cx), [("beta".to_string(), 0)]);
    assert!(!read(cx, h, |w, _| w.chooser));
    // Another machine's project of the same name isn't this one.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::remote("devbox", "beta"), window, cx));
    assert!(!read(cx, h, |w, _| w.chooser));
    // Back on the quit project by the time it finishes: it still has to go,
    // to the tab opened meanwhile.
    update(cx, h, |w, window, cx| {
        w.after_quit_project(ProjectKey::local("beta"), window, cx);
        w.switch_to("alpha", window, cx);
        w.switch_to("beta", window, cx);
    });
    assert_eq!(windows(cx), [("alpha".to_string(), 0)], "beta is gone: its neighbor");
    assert_eq!(read(cx, h, |w, _| w.tabs.order().to_vec()), ["alpha"]);
    // That one quit too, with nothing left: the chooser.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("alpha"), window, cx));
    assert!(read(cx, h, |w, _| w.chooser));
}

#[gpui::test]
fn closing_the_shown_tab_selects_its_right_neighbor_else_its_left(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    update(cx, h, |w, window, cx| {
        w.switch_to("beta", window, cx);
        w.switch_to("gamma", window, cx);
        w.switch_to("beta", window, cx);
    });
    let tabs = |cx: &mut TestAppContext| read(cx, h, |w, _| w.tabs.order().to_vec());
    assert_eq!(tabs(cx), ["alpha", "beta", "gamma"], "in the order they were opened");
    // The middle tab: its right neighbor.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("beta"), window, cx));
    assert_eq!(windows(cx), [("gamma".to_string(), 0)]);
    assert_eq!(tabs(cx), ["alpha", "gamma"]);
    // The last tab: its left one.
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("gamma"), window, cx));
    assert_eq!(windows(cx), [("alpha".to_string(), 0)]);
    assert_eq!(tabs(cx), ["alpha"]);
    // A tab the window doesn't show goes without moving it; another
    // machine's project of that name has none here.
    update(cx, h, |w, window, cx| {
        w.switch_to("beta", window, cx);
        w.after_quit_project(ProjectKey::remote("devbox", "alpha"), window, cx);
    });
    assert_eq!(tabs(cx), ["alpha", "beta"]);
    update(cx, h, |w, window, cx| w.after_quit_project(ProjectKey::local("alpha"), window, cx));
    assert_eq!(windows(cx), [("beta".to_string(), 0)]);
    assert_eq!(tabs(cx), ["beta"]);
    // Saved with the window's state (its slot's, on the debounced save).
    assert_eq!(read(cx, h, |w, _| w.window_state.tabs.clone()), [ProjectKey::local("beta")]);
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    let saved = cx.update(|cx| cx.global::<Shell>().session.slots[0].state.tabs.clone());
    assert_eq!(saved, [ProjectKey::local("beta")]);
}

#[gpui::test]
fn a_switch_closes_the_dialogs_opened_over_the_old_project(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    update(cx, h, |w, window, cx| w.open_connect(window, cx));
    assert!(read(cx, h, |w, _| w.connect.is_some()));
    update(cx, h, |w, window, cx| w.switch_to("beta", window, cx));
    assert!(read(cx, h, |w, _| !w.modal_open()), "Connect closed");

    update(cx, h, |w, window, cx| w.open_feedback(window, cx));
    assert!(read(cx, h, |w, _| w.feedback.is_some()));
    update(cx, h, |w, window, cx| w.switch_to("alpha", window, cx));
    assert!(read(cx, h, |w, _| !w.modal_open()), "Contact Us closed");

    update(cx, h, |w, window, cx| w.open_new_task(window, cx));
    assert!(read(cx, h, |w, _| w.new_task.is_some()));
    // The chooser is a switch away from the project too.
    update(cx, h, |w, window, cx| w.show_chooser(window, cx));
    assert!(read(cx, h, |w, _| !w.modal_open()), "New task closed");
}

#[gpui::test]
fn a_notice_for_the_shown_project_leaves_its_dialog_alone(cx: &mut TestAppContext) {
    let f = fixture(cx);
    let h = open(cx, &f, "alpha", 0);
    update(cx, h, |w, window, cx| w.open_new_task(window, cx));
    cx.update(|cx| open_notice_tag("shelbi/alpha/orch", cx));
    cx.run_until_parked();
    assert_eq!(windows(cx), [("alpha".to_string(), 0)]);
    assert!(read(cx, h, |w, _| w.new_task.is_some()), "nothing switched");
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

// --- tab-tearoff: a tab torn off into its own window, or dragged between
// windows ---------------------------------------------------------------

/// Open a window on `project` in `slot` at `frame`, with `tabs` (Reduce
/// Motion on, so the tabs and the ghost land at once), and draw it so its
/// bar has measured itself.
pub(super) fn open_at(
    cx: &mut TestAppContext,
    f: &Fixture,
    project: &str,
    slot: usize,
    frame: Frame,
    tabs: &[&str],
) -> WindowHandle<ShelbiWindow> {
    let deps = f.deps.clone();
    let state = WindowState {
        frame: Some(frame),
        tabs: tabs.iter().map(|t| crate::project_tabs::tab_key(t)).collect(),
        ..WindowState::default()
    };
    let handle = cx.update(|cx| {
        open_window(project, slot, state, None, cx);
        let handle = cx.global::<Shell>().windows.last().unwrap().handle;
        handle
            .update(cx, |w, window, cx| {
                w.chat_deps = Some(deps.sessions.clone());
                w.workspace_deps = Some(deps);
                w.reduce_motion = Rc::new(|| true);
                w.start_feed(window, cx);
            })
            .unwrap();
        handle
    });
    draw(cx, handle);
    draw(cx, handle);
    handle
}

pub(super) fn frame(x: f32, y: f32) -> Frame {
    Frame { x, y, width: 900.0, height: 700.0 }
}

/// A catalog with `open` recorded open, and windows closing as the app's do.
pub(super) fn tearoff_fixture(cx: &mut TestAppContext, open: &[&str]) -> Fixture {
    let f = fixture(cx);
    cx.update(|cx| {
        cx.global::<Catalog>().set_open(open);
        cx.on_window_closed(|cx, id| window_closed(id, cx)).detach();
    });
    f
}

pub(super) fn tabs_of(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) -> Vec<String> {
    read(cx, handle, |w, _| w.tabs.order().to_vec())
}

pub(super) fn shown(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) -> String {
    read(cx, handle, |w, _| w.project.clone())
}

pub(super) fn vcx(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) -> VisualTestContext {
    VisualTestContext::from_window(*handle, cx)
}

pub(super) fn bounds_in(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, selector: &'static str) -> Option<gpui::Bounds<Pixels>> {
    draw(cx, handle);
    vcx(cx, handle).debug_bounds(selector)
}

/// Press on `selector` in `handle` and drag past gpui's threshold: the drag
/// begins. Where it began (the point its ghost is held by).
pub(super) fn press_and_start(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, selector: &'static str) -> Point<Pixels> {
    let start = bounds_in(cx, handle, selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
    let began = start + point(px(8.0), px(0.0));
    let mut v = vcx(cx, handle);
    v.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
    v.simulate_mouse_move(began, Some(MouseButton::Left), Modifiers::none());
    draw(cx, handle);
    began
}

/// Move the held pointer to window point `to` of `handle` (the drag's own
/// window: it follows the drag past its edges).
pub(super) fn move_to(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, to: Point<Pixels>) {
    vcx(cx, handle).simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
    cx.run_until_parked();
    draw(cx, handle);
}

pub(super) fn release_at(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, at: Point<Pixels>) {
    vcx(cx, handle).simulate_mouse_up(at, MouseButton::Left, Modifiers::none());
    cx.run_until_parked();
}

/// The open floating ghost windows.
fn floats(cx: &mut TestAppContext) -> Vec<WindowHandle<FloatingGhost>> {
    cx.update(|cx| {
        cx.windows()
            .into_iter()
            .filter_map(|w| w.downcast::<FloatingGhost>())
            .collect()
    })
}

fn over(cx: &mut TestAppContext) -> Option<Option<WindowId>> {
    cx.update(|cx| tab_tearoff::session(cx).map(|s| s.over))
}

#[gpui::test]
fn a_tab_dragged_200_from_the_bar_becomes_a_card_and_tears_off_into_its_own_window(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta"]);
    let beta = bounds_in(cx, a, "tab-beta").unwrap();
    let grab = press_and_start(cx, a, "tab-beta") - beta.origin;

    // Along the bar, and within 200 under it: the tab ghost, in the window.
    move_to(cx, a, point(px(450.0), px(52.0 + 199.0)));
    assert_eq!(over(cx), Some(Some(a.window_id())));
    let ghost = bounds_in(cx, a, "tab-drag-ghost").expect("the tab ghost");
    assert_eq!(ghost.size.height, px(52.0));
    assert!(floats(cx).is_empty(), "no floating ghost over its own bar");

    // Past 200: the card, 300 × 200, drawn by the floating ghost window.
    let away = point(px(450.0), px(52.0 + 260.0));
    move_to(cx, a, away);
    assert_eq!(over(cx), Some(None), "torn off");
    assert!(bounds_in(cx, a, "tab-drag-ghost").is_none(), "the window's own ghost steps aside");
    let float = *floats(cx).first().expect("a floating ghost window opened");
    let mut fv = VisualTestContext::from_window(*float, cx);
    fv.update(|window, cx| window.draw(cx).clear(cx));
    let card = fv.debug_bounds("tab-drag-card").expect("the card");
    assert_eq!(card.size, gpui::size(px(CARD_W), px(CARD_H)));

    // Back within 200: the tab ghost again.
    move_to(cx, a, point(px(450.0), px(120.0)));
    assert_eq!(over(cx), Some(Some(a.window_id())));
    assert!(bounds_in(cx, a, "tab-drag-ghost").is_some());

    // Dropped torn off: a new window holding just beta; alpha keeps its
    // window.
    move_to(cx, a, away);
    release_at(cx, a, away);
    let list = windows(cx);
    assert_eq!(list.len(), 2, "{list:?}");
    assert!(floats(cx).is_empty(), "the floating ghost closed");
    assert!(over(cx).is_none(), "the drag is over");
    let b = cx.update(|cx| cx.global::<Shell>().windows[1].handle);
    assert_eq!(tabs_of(cx, a), ["alpha"]);
    assert_eq!(shown(cx, a), "alpha");
    assert_eq!(tabs_of(cx, b), ["beta"]);
    assert_eq!(shown(cx, b), "beta");
    // Its tab under the pointer, where the pointer held it.
    let (fa, fb) = (frame_of(cx, a), frame_of(cx, b));
    let first = read(cx, a, |w, _| w.tab_reorder.layout.borrow().first_x);
    assert_eq!(
        (fb.x, fb.y),
        (fa.x + away.x.as_f32() - first - grab.x.as_f32(), fa.y + away.y.as_f32() - grab.y.as_f32())
    );
    assert_eq!((fb.width, fb.height), (900.0, 700.0), "the size of the window it came from");

    // A refresh doesn't bring beta's tab back to the first window.
    update(cx, a, |w, _, cx| w.sync_tabs(cx));
    assert_eq!(tabs_of(cx, a), ["alpha"]);
    update(cx, b, |w, _, cx| w.sync_tabs(cx));
    assert_eq!(tabs_of(cx, b), ["beta"]);
}

#[gpui::test]
fn a_relaunch_restores_the_torn_off_arrangement(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta", "gamma"]);
    press_and_start(cx, a, "tab-gamma");
    let away = point(px(400.0), px(600.0));
    move_to(cx, a, away);
    release_at(cx, a, away);
    // The tabs' save waits a beat.
    cx.executor().advance_clock(std::time::Duration::from_secs(1));
    cx.run_until_parked();
    let session = cx.update(|cx| cx.global::<Shell>().session.clone());
    let open: Vec<(Option<ProjectKey>, Vec<String>)> = session
        .slots
        .iter()
        .filter(|s| s.open)
        .map(|s| (s.project.clone(), s.state.tabs.iter().map(ToString::to_string).collect()))
        .collect();
    assert_eq!(
        open,
        [
            (Some(ProjectKey::local("alpha")), vec!["alpha".to_string(), "beta".to_string()]),
            (Some(ProjectKey::local("gamma")), vec!["gamma".to_string()]),
        ]
    );
    // Quit (the windows close with the app) and relaunch from the saved
    // slots: the same two windows, the same tabs.
    let plan = window_state::launch_plan(Some(&session), None, None, &|_| true, &|_| WindowState::default());
    let reopened: Vec<(String, Vec<String>)> = plan
        .iter()
        .map(|w| (w.project.name.clone(), w.state.tabs.iter().map(ToString::to_string).collect()))
        .collect();
    assert_eq!(
        reopened,
        [
            ("alpha".to_string(), vec!["alpha".to_string(), "beta".to_string()]),
            ("gamma".to_string(), vec!["gamma".to_string()]),
        ]
    );
}

#[gpui::test]
fn a_tab_dragged_onto_another_windows_bar_drops_into_its_gap_with_its_view_and_sessions(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    leave_alpha_on_its_workspace(cx, a);
    assert!(live_attaches(&f, "alpha") > 0, "alpha's workspace session is attached");

    // Over the other window's bar (its gamma tab's left half, in this
    // window's coordinates): the tab ghost, and that bar opens its gap.
    let gamma = bounds_in(cx, b, "tab-gamma").unwrap();
    let rest = gamma.left();
    press_and_start(cx, a, "tab-alpha");
    let to = point(px(1000.0) + gamma.left() + px(10.0), gamma.center().y);
    move_to(cx, a, to);
    assert_eq!(over(cx), Some(Some(b.window_id())));
    assert!(read(cx, b, |w, _| w.tab_reorder.hosting()));
    let slid = bounds_in(cx, b, "tab-gamma").unwrap().left();
    assert!(slid > rest, "gamma steps aside for the gap: {slid:?} > {rest:?}");
    assert!(!floats(cx).is_empty(), "the tab ghost floats over the other window");

    release_at(cx, a, to);
    draw(cx, b);
    draw(cx, b);
    assert_eq!(tabs_of(cx, b), ["alpha", "gamma"], "in the gap, before gamma");
    assert_eq!(shown(cx, b), "alpha");
    assert!(!read(cx, b, |w, _| w.tab_reorder.hosting()));
    assert_eq!(tabs_of(cx, a), ["beta"], "it left the first window");
    assert_eq!(shown(cx, a), "beta", "which moved to its neighbor");
    // The project kept where it was left, and its session: the workspace,
    // attached in its new window.
    assert_eq!(read(cx, b, |w, _| w.client.view().clone()), View::from_view_id("alpha-1"));
    assert!(live_attaches(&f, "alpha") > 0, "alpha's session is attached again");
    assert_eq!(windows(cx).len(), 2);
}

#[gpui::test]
fn moving_a_windows_last_tab_away_closes_the_empty_window(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha"]);
    let b = open_at(cx, &f, "beta", 1, frame(1000.0, 0.0), &["beta"]);
    let beta = bounds_in(cx, b, "tab-beta").unwrap();
    press_and_start(cx, a, "tab-alpha");
    // Past beta's tab: the gap after it.
    let to = point(px(1000.0) + beta.right() - px(5.0), beta.center().y);
    move_to(cx, a, to);
    release_at(cx, a, to);
    assert_eq!(windows(cx), [("alpha".to_string(), 1)], "one window, the one it went to");
    assert_eq!(tabs_of(cx, b), ["beta", "alpha"]);
    let slots = cx.update(|cx| cx.global::<Shell>().session.slots.clone());
    assert!(!slots[0].open, "the emptied window doesn't come back next launch");
}

#[gpui::test]
fn tearing_off_a_windows_only_tab_moves_the_window(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha"]);
    let a = open_at(cx, &f, "alpha", 0, frame(100.0, 50.0), &["alpha"]);
    let tab = bounds_in(cx, a, "tab-alpha").unwrap();
    let grab = press_and_start(cx, a, "tab-alpha") - tab.origin;
    crate::platform::screen::test_log::take();
    let away = point(px(500.0), px(600.0));
    move_to(cx, a, away);
    assert_eq!(over(cx), Some(None), "the card");
    release_at(cx, a, away);
    assert_eq!(windows(cx), [("alpha".to_string(), 0)], "no new window");
    assert_eq!(tabs_of(cx, a), ["alpha"]);
    // The window goes where the tab was dropped (screen space: its origin
    // plus the pointer), its tab under the pointer.
    let to = crate::platform::screen::test_log::take();
    let fa = frame_of(cx, a);
    let want = point(
        fa.x + away.x.as_f32() - tab.origin.x.as_f32() - grab.x.as_f32(),
        fa.y + away.y.as_f32() - tab.origin.y.as_f32() - grab.y.as_f32(),
    );
    assert_eq!(to, [want]);
}

#[gpui::test]
fn esc_ends_a_tab_drag_where_it_began(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    let gamma = bounds_in(cx, b, "tab-gamma").unwrap();
    press_and_start(cx, a, "tab-beta");
    let to = point(px(1000.0) + gamma.left() + px(10.0), gamma.center().y);
    move_to(cx, a, to);
    assert!(read(cx, b, |w, _| w.tab_reorder.hosting()));
    vcx(cx, a).simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(over(cx).is_none(), "no drag");
    assert!(!cx.update(|cx| cx.has_active_drag()));
    assert!(!read(cx, b, |w, _| w.tab_reorder.hosting()), "the other bar's gap closes");
    release_at(cx, a, to);
    assert_eq!(tabs_of(cx, a), ["alpha", "beta"]);
    assert_eq!(tabs_of(cx, b), ["gamma"]);
    assert_eq!(windows(cx).len(), 2);
    assert!(floats(cx).is_empty());
}

fn gap_open(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>) -> bool {
    read(cx, handle, |w, _| w.tab_reorder.gap_open())
}

#[gpui::test]
fn the_gap_follows_the_tab_between_windows_back_and_forth(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    let alpha = bounds_in(cx, a, "tab-alpha").unwrap();
    let beta = bounds_in(cx, a, "tab-beta").unwrap();
    let gamma = bounds_in(cx, b, "tab-gamma").unwrap();
    let plus_b = bounds_in(cx, b, "tab-plus").unwrap();
    press_and_start(cx, a, "tab-alpha");
    // Window points of `a` (at the screen's origin): its own bar, b's bar
    // before gamma, and torn off below both.
    let home = point(beta.right() - px(5.0), beta.center().y);
    let on_b = point(px(1000.0) + gamma.left() + px(10.0), gamma.center().y);
    let away = point(px(450.0), px(600.0));
    let only_a = |cx: &mut TestAppContext| (gap_open(cx, a), gap_open(cx, b)) == (true, false);
    let only_b = |cx: &mut TestAppContext| (gap_open(cx, a), gap_open(cx, b)) == (false, true);

    move_to(cx, a, home);
    assert!(only_a(cx), "its own bar");
    for round in 0..3 {
        move_to(cx, a, on_b);
        assert!(only_b(cx), "round {round}: a's closes as b's opens");
        draw(cx, b);
        assert!(bounds_in(cx, b, "tab-gamma").unwrap().left() > gamma.left(), "gamma steps aside");
        assert!(bounds_in(cx, b, "tab-plus").unwrap().left() > plus_b.left(), "and so does the +");
        // The tab gone from a's bar: beta closes up over its slot.
        assert_eq!(bounds_in(cx, a, "tab-beta").unwrap().left(), alpha.left(), "round {round}");
        move_to(cx, a, home);
        assert!(only_a(cx), "round {round}: back home");
        assert_eq!(bounds_in(cx, b, "tab-gamma").unwrap().left(), gamma.left(), "b's gap closed");
        move_to(cx, a, away);
        assert_eq!((gap_open(cx, a), gap_open(cx, b)), (false, false), "round {round}: torn off, none");
        move_to(cx, a, on_b);
        assert!(only_b(cx));
        move_to(cx, a, away);
        assert_eq!((gap_open(cx, a), gap_open(cx, b)), (false, false));
    }
    // Straight from b's bar to a's in one move, and back.
    move_to(cx, a, on_b);
    move_to(cx, a, home);
    assert!(only_a(cx));
    move_to(cx, a, on_b);
    assert!(only_b(cx));
    // Dropped: it lands in the gap that showed, before gamma.
    let gap = cx.update(|cx| tab_tearoff::session(cx).and_then(|s| s.gap));
    assert_eq!(gap, Some(0));
    release_at(cx, a, on_b);
    draw(cx, b);
    assert_eq!(tabs_of(cx, b), ["alpha", "gamma"]);
    assert_eq!(tabs_of(cx, a), ["beta"]);
    assert!(!gap_open(cx, a) && !gap_open(cx, b), "the drag is over");
}

#[gpui::test]
fn a_tab_dropped_past_another_bars_last_tab_lands_in_the_gap_at_the_end(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    let plus = bounds_in(cx, b, "tab-plus").unwrap();
    press_and_start(cx, a, "tab-alpha");
    // Over b's `+`: the gap at the end shows (the + steps aside), and the
    // drop lands there.
    let to = point(px(1000.0) + plus.center().x, plus.center().y);
    move_to(cx, a, to);
    assert!(gap_open(cx, b));
    assert_eq!(cx.update(|cx| tab_tearoff::session(cx).and_then(|s| s.gap)), Some(1));
    assert!(bounds_in(cx, b, "tab-plus").unwrap().left() > plus.left());
    release_at(cx, a, to);
    assert_eq!(tabs_of(cx, b), ["gamma", "alpha"]);
}

#[gpui::test]
fn over_overlapping_windows_the_front_ones_bar_takes_the_gap(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    // b over a's lower part, in front.
    let b = open_at(cx, &f, "gamma", 1, frame(300.0, 200.0), &["gamma"]);
    update(cx, b, |_, window, _| window.activate_window());
    let gamma = bounds_in(cx, b, "tab-gamma").unwrap();
    press_and_start(cx, a, "tab-alpha");
    // On b's bar, inside a too and within 200 of a's bar: b's, in front.
    let on_b = point(px(300.0) + gamma.left() + px(10.0), px(200.0) + gamma.center().y);
    let on_a = point(px(150.0), px(26.0));
    for _ in 0..2 {
        move_to(cx, a, on_b);
        assert_eq!(over(cx), Some(Some(b.window_id())));
        assert_eq!((gap_open(cx, a), gap_open(cx, b)), (false, true));
        move_to(cx, a, on_a);
        assert_eq!(over(cx), Some(Some(a.window_id())));
        assert_eq!((gap_open(cx, a), gap_open(cx, b)), (true, false));
    }
    release_at(cx, a, on_a);
}

#[gpui::test]
fn a_bar_left_hosting_by_a_missed_move_closes_its_gap_on_its_next_frame(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    let gamma = bounds_in(cx, b, "tab-gamma").unwrap();
    press_and_start(cx, a, "tab-alpha");
    move_to(cx, a, point(px(1000.0) + gamma.left() + px(10.0), gamma.center().y));
    assert!(gap_open(cx, b));
    // The session moves on without telling b (as a move that never reached
    // it would): b's next frame follows the session anyway.
    cx.update(|cx| {
        if let Some(s) = tab_tearoff::session_mut(cx) {
            s.over = None;
            s.gap = None;
        }
    });
    draw(cx, b);
    assert!(!gap_open(cx, b));
    release_at(cx, a, point(px(450.0), px(600.0)));
}

/// Right-click `selector` in `handle`: a tab's context menu opens.
fn right_click_in(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, selector: &'static str) {
    let at = bounds_in(cx, handle, selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
    let mut v = vcx(cx, handle);
    v.simulate_mouse_down(at, MouseButton::Right, Modifiers::none());
    v.simulate_mouse_up(at, MouseButton::Right, Modifiers::none());
    cx.run_until_parked();
    draw(cx, handle);
}

fn click_in(cx: &mut TestAppContext, handle: WindowHandle<ShelbiWindow>, selector: &'static str) {
    let at = bounds_in(cx, handle, selector).unwrap_or_else(|| panic!("{selector} is drawn")).center();
    vcx(cx, handle).simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    draw(cx, handle);
}

/// Wayland: no window knows where it or any other is. A tab dragged over
/// where another window's bar would be is just away from its own bar, so it
/// tears off; the card is drawn in its own window (no floating ghost, which
/// couldn't be placed), and the new window opens where the compositor puts
/// it.
#[gpui::test]
fn without_global_positions_a_drag_keeps_to_its_own_window(cx: &mut TestAppContext) {
    crate::platform::screen::test_global_positions(false);
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);
    let gamma = bounds_in(cx, b, "tab-gamma").unwrap();
    press_and_start(cx, a, "tab-beta");
    // Where gamma's bar is in a shared screen space, and far from a's own.
    let to = point(px(1300.0) + gamma.left(), gamma.center().y);
    move_to(cx, a, to);
    assert_eq!(over(cx), Some(None), "torn off: the other bar isn't found");
    assert!(!read(cx, b, |w, _| w.tab_reorder.hosting()), "b opens no gap");
    assert!(floats(cx).is_empty(), "no floating ghost");
    assert!(bounds_in(cx, a, "tab-drag-card").is_some(), "a draws the card itself");
    release_at(cx, a, to);
    assert_eq!(windows(cx).len(), 3, "a new window");
    assert_eq!(tabs_of(cx, a), ["alpha"]);
    assert_eq!(tabs_of(cx, b), ["gamma"], "the other window is untouched");
    let c = cx.update(|cx| cx.global::<Shell>().windows[2].handle);
    assert_eq!(tabs_of(cx, c), ["beta"]);
    crate::platform::screen::test_global_positions(true);
}

/// The tab menu's Move to Window lists the other windows and New Window,
/// and moves the tab as a drag between windows would: by the pointer or the
/// keys.
#[gpui::test]
fn move_to_window_moves_a_tab_into_another_window_or_a_new_one(cx: &mut TestAppContext) {
    let f = tearoff_fixture(cx, &["alpha", "beta", "gamma", "delta"]);
    let a = open_at(cx, &f, "alpha", 0, frame(0.0, 0.0), &["alpha", "beta", "delta"]);
    let b = open_at(cx, &f, "gamma", 1, frame(1000.0, 0.0), &["gamma"]);

    // Hovering the row opens the submenu: gamma's window, then New Window.
    right_click_in(cx, a, "tab-beta");
    assert!(bounds_in(cx, a, "tab-move-menu").is_none(), "closed until asked");
    click_in(cx, a, "tab-context-move");
    let row = bounds_in(cx, a, "tab-context-move").unwrap();
    let sub = bounds_in(cx, a, "tab-move-menu").expect("the submenu");
    assert!(sub.left() > row.right(), "beside the menu");
    assert_eq!(bounds_in(cx, a, "tab-move-0").unwrap().top(), row.top(), "its first row level with Move to Window");
    assert!(bounds_in(cx, a, "tab-move-1").is_some(), "New Window");
    assert!(bounds_in(cx, a, "tab-move-2").is_none());
    click_in(cx, a, "tab-move-0");
    assert_eq!(tabs_of(cx, b), ["gamma", "beta"], "last in the other window");
    assert_eq!(shown(cx, b), "beta", "which switches to it");
    assert_eq!(tabs_of(cx, a), ["alpha", "delta"]);
    assert!(bounds_in(cx, a, "tab-context-menu").is_none(), "the menu closed");

    // New Window, by the keys: ↑ lands on Move to Window (the last row), →
    // opens the submenu on its first row, ↓ steps to New Window.
    right_click_in(cx, a, "tab-delta");
    let mut v = vcx(cx, a);
    v.simulate_keystrokes("up right down enter");
    cx.run_until_parked();
    assert_eq!(windows(cx).len(), 3, "a new window");
    let c = cx.update(|cx| cx.global::<Shell>().windows[2].handle);
    assert_eq!(tabs_of(cx, c), ["delta"]);
    assert_eq!(tabs_of(cx, a), ["alpha"]);

    // A window's only tab: no New Window, but the other windows still.
    right_click_in(cx, a, "tab-alpha");
    click_in(cx, a, "tab-context-move");
    assert!(bounds_in(cx, a, "tab-move-1").is_some(), "the two other windows");
    assert!(bounds_in(cx, a, "tab-move-2").is_none(), "and no New Window");
    // The click empties (and closes) the window it lands in: no redraw.
    let at = bounds_in(cx, a, "tab-move-1").unwrap().center();
    vcx(cx, a).simulate_click(at, Modifiers::none());
    cx.run_until_parked();
    assert_eq!(windows(cx).len(), 2, "the emptied window closed");
    assert_eq!(tabs_of(cx, c), ["delta", "alpha"]);
}
