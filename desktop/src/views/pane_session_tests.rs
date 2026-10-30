//! The task pane's live session (`dt-task-pane-session`), through the board
//! with fake sessions: which session attaches, that nothing launches, that
//! switching, closing and hiding detach only this view, Open Workspace, the
//! terminal's keys, the split, and remote reconnects.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::channel::mpsc::{unbounded, UnboundedSender};
use gpui::{point, px, AppContext, Entity, Modifiers, MouseButton, Size as GpuiSize, TestAppContext, VisualTestContext};
use shelbi_app::session::{
    self as app_session, ConnectFailure, IdleInfo, ReconnectPolicy, Relauncher, RetryPolicy, SessionRef,
};
use shelbi_app::view::{ReviewRow, ReviewState, WorkspaceBadge, WorkspaceRow};
use shelbi_app::CommandKind;
use shelbi_client::SessionEvent;
use shelbi_state::IssueFile;
use shelbi_term::Size;

use super::super::board_state::tests::{issue, read};
use super::super::issues::{IssuesEvent, IssuesView};
use super::super::pane_split::{HANDLE_H, KEY_STEP, SESSION_H, SESSION_HEADER_H};
use super::super::session_view::{SessionConnector, SessionDeps, SessionPhase};
use super::MainShows;
use crate::elements::terminal::session::{Connected, SessionIo};
use crate::theme::Theme;

const PROJECT: &str = "pane-test";

/// Sessions by name: every attach succeeds unless the name is idle or
/// unreachable. Records the attaches, the bytes typed into them, and every
/// relaunch the view asks for.
#[derive(Default)]
struct Fake {
    attaches: Mutex<Vec<(String, UnboundedSender<SessionEvent>)>>,
    input: Arc<Mutex<Vec<u8>>>,
    idle: Mutex<Vec<String>>,
    unreachable: Mutex<Vec<String>>,
    relaunches: Mutex<u32>,
}

struct Io(Arc<Mutex<Vec<u8>>>);

impl SessionIo for Io {
    fn input(&self, bytes: Vec<u8>) {
        self.0.lock().unwrap().extend(bytes);
    }
    fn paste(&self, text: String, _bracketed: bool) {
        self.0.lock().unwrap().extend(text.into_bytes());
    }
    fn resize(&self, _size: Size) {}
}

impl Fake {
    fn attached(&self) -> Vec<String> {
        self.attaches.lock().unwrap().iter().map(|(n, _)| n.clone()).collect()
    }

    fn live(&self) -> Vec<String> {
        self.attaches.lock().unwrap().iter().filter(|(_, tx)| !tx.is_closed()).map(|(n, _)| n.clone()).collect()
    }

    fn typed(&self) -> String {
        String::from_utf8_lossy(&self.input.lock().unwrap()).into_owned()
    }

    fn push(&self, name: &str, bytes: &[u8]) {
        let attaches = self.attaches.lock().unwrap();
        let tx = &attaches.iter().rev().find(|(n, _)| n == name).unwrap().1;
        tx.unbounded_send(SessionEvent::Output { seq: 1, data: bytes.to_vec() }).unwrap();
    }

    fn drop_streams(&self, name: &str) {
        self.attaches.lock().unwrap().retain(|(n, _)| n != name);
    }
}

impl SessionConnector for Fake {
    fn connect(&self, project: &str, target: &SessionRef, _retry: RetryPolicy) -> Result<Connected, ConnectFailure> {
        let name = app_session::discovery_name(project, target);
        if self.unreachable.lock().unwrap().contains(&name) {
            return Err(ConnectFailure::Unreachable("`gpu` is unreachable".into()));
        }
        if self.idle.lock().unwrap().contains(&name) {
            return Err(ConnectFailure::Idle(IdleInfo {
                name: target.workspace().unwrap_or_default().to_string(),
                machine: "hub".into(),
                branch: None,
            }));
        }
        let (tx, rx) = unbounded();
        tx.unbounded_send(SessionEvent::Resync { seq: 0, replay: format!("{name} scrollback\r\n").into_bytes() })
            .unwrap();
        self.attaches.lock().unwrap().push((name.clone(), tx));
        Ok(Connected { link: Box::new(Io(self.input.clone())), events: rx, size: Size::new(80, 24), name, cwd: None })
    }
}

impl Relauncher for Fake {
    fn relaunch(&self, _target: &SessionRef, _reopen: bool) -> Result<(), String> {
        *self.relaunches.lock().unwrap() += 1;
        Ok(())
    }
}

struct Pane {
    view: Entity<IssuesView>,
    cx: VisualTestContext,
    fake: Arc<Fake>,
    events: Rc<std::cell::RefCell<Vec<IssuesEvent>>>,
}

fn ws(name: &str, task: Option<&str>) -> WorkspaceRow {
    WorkspaceRow {
        name: name.into(),
        machine: "hub".into(),
        is_remote: false,
        current_task: task.map(str::to_string),
        agent: None,
        badge: if task.is_some() { WorkspaceBadge::Working } else { WorkspaceBadge::Idle },
    }
}

/// The board: A (alpha works it), B (bravo works it), C (assigned to alpha,
/// which is busy with A), D (to do, nobody's).
fn tasks() -> Vec<IssueFile> {
    let mut a = issue("A", "in-progress");
    a.task.assigned_to = Some("alpha".into());
    let mut b = issue("B", "in-progress");
    b.task.assigned_to = Some("bravo".into());
    let mut c = issue("C", "in-progress");
    c.task.assigned_to = Some("alpha".into());
    vec![a, b, c, issue("D", "todo")]
}

fn open(cx: &mut TestAppContext) -> Pane {
    open_sized(cx, GpuiSize { width: px(1160.0), height: px(1024.0) })
}

fn open_sized(cx: &mut TestAppContext, size: GpuiSize<gpui::Pixels>) -> Pane {
    cx.update(|cx| {
        cx.set_global(Theme::resolve(shelbi_state::ThemePreference::Dark, gpui::WindowAppearance::Dark));
        gpui_kit::init(cx);
        // The pane in place at once, so a click lands where it is drawn.
        cx.set_reduce_motion(true);
    });
    let options = gpui::WindowOptions {
        window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(gpui::Point::default(), size))),
        ..Default::default()
    };
    let handle = cx.update(|cx| {
        cx.open_window(options, |_, cx| cx.new(|cx| IssuesView::new(PROJECT.into(), cx))).unwrap()
    });
    let mut vcx = VisualTestContext::from_window(handle.into(), cx);
    let view = handle.root(&mut vcx).unwrap();
    let fake = Arc::new(Fake::default());
    let events = Rc::new(std::cell::RefCell::new(Vec::new()));
    let sink = events.clone();
    let deps = SessionDeps {
        connector: fake.clone(),
        relauncher: fake.clone(),
        retry: RetryPolicy::default(),
        reconnect: ReconnectPolicy::default(),
        clock: Rc::new(Instant::now),
    };
    view.update(&mut vcx, |v, cx| {
        v.stop_workers();
        v.state.apply_reads(Some(read(tasks())), Some(None));
        v.host_sessions(handle.into(), deps, None, cx);
        v.set_task_rows(vec![ws("alpha", Some("A")), ws("bravo", Some("B"))], Vec::new(), cx);
        v.set_shown(true, MainShows::default(), cx);
        cx.subscribe(&cx.entity(), move |_, _, ev: &IssuesEvent, _| sink.borrow_mut().push(ev.clone())).detach();
    });
    let mut p = Pane { view, cx: vcx, fake, events };
    p.draw();
    p
}

impl Pane {
    fn draw(&mut self) {
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
        self.cx.run_until_parked();
    }

    fn open_task(&mut self, id: &str) {
        self.view.update(&mut self.cx, |v, cx| v.open_task(id, cx));
        self.draw();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn bounds(&mut self, selector: &'static str) -> gpui::Bounds<gpui::Pixels> {
        self.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn"))
    }

    fn click(&mut self, selector: &'static str) {
        let at = self.bounds(selector).center();
        self.cx.simulate_click(at, Modifiers::none());
        self.draw();
    }

    fn phase(&mut self) -> Option<SessionPhase> {
        self.view.read_with(&self.cx, |v, cx| v.pane_session_view().map(|s| s.read(cx).phase()))
    }

    fn pane_task(&mut self) -> Option<String> {
        self.view.read_with(&self.cx, |v, _| v.state.detail_id().map(str::to_string))
    }

    fn session_h(&mut self) -> f32 {
        self.bounds("pane-session").size.height.as_f32()
    }

    fn relaunches(&self) -> u32 {
        *self.fake.relaunches.lock().unwrap()
    }

    fn commands(&self) -> Vec<CommandKind> {
        self.events
            .borrow()
            .iter()
            .filter_map(|e| match e {
                IssuesEvent::Command(k) => Some(k.clone()),
                IssuesEvent::Focused | IssuesEvent::PaneWidth(_) => None,
            })
            .collect()
    }
}

fn name(ws: &str) -> String {
    format!("{PROJECT}/ws/{ws}")
}

#[gpui::test]
fn an_in_progress_task_shows_the_agent_working_it_at_the_frames_size(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    assert_eq!(p.fake.attached(), vec![name("alpha")]);
    assert_eq!(p.phase(), Some(SessionPhase::Live));
    // The In progress frame: the 17pt handle over the 356pt session (its
    // 44pt header included), flush with the pane's foot.
    let pane = p.bounds("task-pane");
    assert_eq!(pane.size.height.as_f32(), 1008.0);
    let handle = p.bounds("pane-split-handle");
    let session = p.bounds("pane-session");
    let header = p.bounds("pane-session-header");
    assert_eq!(handle.size.height.as_f32(), HANDLE_H);
    assert_eq!(session.size.height.as_f32(), SESSION_H);
    assert_eq!(session.bottom(), pane.bottom());
    assert_eq!(handle.bottom(), session.top());
    assert_eq!(header.size.height.as_f32(), SESSION_HEADER_H);
    assert_eq!(p.bounds("pane-scroll").size.height.as_f32(), 583.0, "the frame's details scroll");
    assert!(p.drawn("pane-session-name") && p.drawn("pane-open-workspace"));
    assert!(p.drawn("session-view"));
    assert_eq!(p.relaunches(), 0, "attaching launched nothing");
}

#[gpui::test]
fn an_assignment_or_status_alone_attaches_nothing_and_an_idle_task_launches_nothing(cx: &mut TestAppContext) {
    let mut p = open(cx);
    // C is In Progress and assigned to alpha, but alpha is working A.
    p.open_task("C");
    assert!(!p.drawn("pane-session") && !p.drawn("pane-split-handle"));
    assert_eq!(p.bounds("pane-scroll").bottom(), p.bounds("task-pane").bottom(), "the details fill the pane");
    p.open_task("D");
    assert!(!p.drawn("pane-session"));
    assert!(p.fake.attached().is_empty(), "no connect at all");
    assert_eq!(p.relaunches(), 0);
    assert!(p.commands().is_empty(), "no start or dispatch");
}

#[gpui::test]
fn switching_tasks_drops_the_old_attach_before_the_new_one(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    p.open_task("B");
    assert_eq!(p.fake.attached(), vec![name("alpha"), name("bravo")]);
    assert_eq!(p.fake.live(), vec![name("bravo")], "alpha's attach is gone");
    // Then an idle task: nothing attached, nothing of bravo's left.
    p.open_task("D");
    assert!(p.fake.live().is_empty());
    assert!(!p.drawn("session-view"));
    assert_eq!(p.relaunches(), 0);
}

#[gpui::test]
fn closing_the_pane_detaches_only_this_view(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    p.click("pane-close");
    assert_eq!(p.pane_task(), None);
    assert!(p.fake.live().is_empty(), "the pane's attach is closed");
    assert_eq!(p.relaunches(), 0, "nothing restarted");
    assert!(p.commands().is_empty(), "nothing stopped or dispatched");
}

#[gpui::test]
fn open_workspace_goes_to_the_same_workspace(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    p.click("pane-open-workspace");
    assert_eq!(p.commands(), vec![CommandKind::FocusWorkspace { workspace: "alpha".into() }]);
    assert_eq!(p.pane_task(), None, "the pane makes way");
    assert!(p.fake.live().is_empty());
}

#[gpui::test]
fn a_loaded_review_shows_its_reviewer_and_opens_the_review(cx: &mut TestAppContext) {
    let mut p = open(cx);
    let review = ReviewRow {
        task_id: "B".into(),
        title: String::new(),
        branch: String::new(),
        location: None,
        workspace: Some("review-1".into()),
        state: ReviewState::Serving,
        number: None,
    };
    p.view.update(&mut p.cx, |v, cx| v.set_task_rows(vec![ws("alpha", Some("A"))], vec![review], cx));
    p.open_task("B");
    assert_eq!(p.fake.live(), vec![name("review-1")]);
    p.click("pane-open-workspace");
    assert_eq!(p.commands(), vec![CommandKind::LoadReview { task_id: "B".into() }]);
}

#[gpui::test]
fn the_terminal_takes_its_keys_before_the_pane_and_board(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    let before = p.view.read_with(&p.cx, |v, _| (v.state.selected_column, v.state.selected_row));
    // A click in the grid gives the terminal the keys.
    let grid = p.bounds("session-view");
    p.cx.update(|window, _| window.activate_window());
    p.cx.simulate_click(grid.center(), Modifiers::none());
    p.draw();
    let focused = p.view.clone();
    assert!(p.cx.update(|window, cx| focused.read(cx).session_focused(window, cx)));
    assert!(p.events.borrow().iter().any(|e| matches!(e, IssuesEvent::Focused)));
    // A board key, a pane key and Esc all go to the agent.
    p.cx.simulate_keystrokes("j l escape");
    p.draw();
    assert_eq!(p.fake.typed(), "jl\x1b");
    assert_eq!(p.pane_task().as_deref(), Some("A"), "Esc didn't close the pane");
    let after = p.view.read_with(&p.cx, |v, _| (v.state.selected_column, v.state.selected_row));
    assert_eq!(before, after, "the board's selection didn't move");
    assert!(p.commands().is_empty(), "l didn't move the card");
}

#[gpui::test]
fn tab_past_the_properties_puts_the_keys_in_the_terminal(cx: &mut TestAppContext) {
    use crate::views::board_state::Property;
    let mut p = open(cx);
    p.open_task("A");
    let view = p.view.clone();
    p.cx.update(|window, _| window.activate_window());
    let body_focused = |p: &mut Pane| {
        p.view.read_with(&p.cx, |v, cx| v.pane_editors.as_ref().is_some_and(|e| e.body.read(cx).is_focused()))
    };
    // The pane's focus order: the title, Status, Workflow, Priority, the
    // body, then the session.
    p.cx.simulate_keystrokes("tab");
    p.draw();
    assert!(p.view.read_with(&p.cx, |v, cx| v.pane_editors.as_ref().is_some_and(|e| e.title.read(cx).is_focused())));
    for property in Property::ALL {
        p.cx.simulate_keystrokes("tab");
        p.draw();
        assert_eq!(p.view.read_with(&p.cx, |v, _| v.state.pane_focus()), Some(property));
        assert!(!p.cx.update(|window, cx| view.read(cx).session_focused(window, cx)));
    }
    p.cx.simulate_keystrokes("tab");
    p.draw();
    assert!(body_focused(&mut p));
    assert_eq!(p.view.read_with(&p.cx, |v, _| v.state.pane_focus()), None);
    // Shift+Tab goes back to Priority, and Tab to the body again.
    p.cx.simulate_keystrokes("shift-tab");
    p.draw();
    assert_eq!(p.view.read_with(&p.cx, |v, _| v.state.pane_focus()), Some(Property::Priority));
    p.cx.simulate_keystrokes("tab");
    p.draw();
    assert!(body_focused(&mut p));
    p.cx.simulate_keystrokes("tab");
    p.draw();
    assert!(p.cx.update(|window, cx| view.read(cx).session_focused(window, cx)));
    assert_eq!(p.view.read_with(&p.cx, |v, _| v.state.pane_focus()), None);
    p.cx.simulate_keystrokes("x");
    p.draw();
    assert_eq!(p.fake.typed(), "x");
}

#[gpui::test]
fn the_split_drags_and_steps_from_the_keyboard(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    let handle = p.bounds("pane-split-handle").center();
    p.cx.simulate_mouse_down(handle, MouseButton::Left, Modifiers::none());
    p.cx.simulate_mouse_move(point(handle.x, handle.y - px(100.0)), Some(MouseButton::Left), Modifiers::none());
    p.cx.simulate_mouse_up(point(handle.x, handle.y - px(100.0)), MouseButton::Left, Modifiers::none());
    p.draw();
    assert_eq!(p.session_h(), SESSION_H + 100.0);
    // The handle took the keys with the press: ↑ / ↓ step it.
    p.cx.simulate_keystrokes("down");
    p.draw();
    assert_eq!(p.session_h(), SESSION_H + 100.0 - KEY_STEP);
    // From the pane, ⌥↑ / ⌥↓.
    p.view.update(&mut p.cx, |v, _| v.request_focus());
    p.draw();
    p.cx.simulate_keystrokes("alt-up alt-up");
    p.draw();
    assert_eq!(p.session_h(), SESSION_H + 100.0 + KEY_STEP);
    // The details scroll and the terminal stay apart, and resizing never
    // made a second attach.
    assert_eq!(p.fake.attached(), vec![name("alpha")]);
}

#[gpui::test]
fn a_small_window_keeps_both_sides_usable(cx: &mut TestAppContext) {
    let mut p = open_sized(cx, GpuiSize { width: px(600.0), height: px(500.0) });
    p.open_task("A");
    let pane = p.bounds("task-pane");
    let details = p.bounds("pane-scroll");
    let session = p.bounds("pane-session");
    assert!(details.size.height.as_f32() >= super::super::pane_split::MIN_DETAILS_H - 0.5, "{details:?}");
    assert!(session.size.height.as_f32() >= super::super::pane_split::MIN_SESSION_H - 0.5, "{session:?}");
    assert_eq!(session.bottom(), pane.bottom());
}

#[gpui::test]
fn a_hidden_board_detaches_and_its_return_reattaches(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    p.view.update(&mut p.cx, |v, cx| v.set_shown(false, MainShows::default(), cx));
    p.cx.run_until_parked();
    assert!(p.fake.live().is_empty());
    assert_eq!(p.pane_task().as_deref(), Some("A"), "the pane keeps its task");
    p.view.update(&mut p.cx, |v, cx| v.set_shown(true, MainShows::default(), cx));
    p.draw();
    assert_eq!(p.fake.live(), vec![name("alpha")]);
    assert_eq!(p.relaunches(), 0);
}

#[gpui::test]
fn over_the_workspace_that_shows_it_the_pane_leaves_the_session_out(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.view.update(&mut p.cx, |v, cx| {
        v.set_shown(true, MainShows { workspace: Some("alpha".into()), review_task: None }, cx)
    });
    p.open_task("A");
    assert!(!p.drawn("pane-session"));
    assert!(p.fake.attached().is_empty());
}

#[gpui::test]
fn a_reassigned_or_deleted_task_drops_its_session(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    // alpha moves on to C: A has no session, C would.
    p.view.update(&mut p.cx, |v, cx| v.set_task_rows(vec![ws("alpha", Some("C")), ws("bravo", Some("B"))], Vec::new(), cx));
    p.draw();
    assert!(p.fake.live().is_empty());
    assert!(!p.drawn("pane-session"));
    // B goes away from the board: its pane shows Missing, no session.
    p.open_task("B");
    assert_eq!(p.fake.live(), vec![name("bravo")]);
    p.view.update(&mut p.cx, |v, cx| {
        let rest: Vec<IssueFile> = tasks().into_iter().filter(|t| t.task.id != "B").collect();
        v.state.apply_reads(Some(read(rest)), Some(None));
        cx.notify();
    });
    p.draw();
    assert!(p.drawn("pane-state"), "the missing state");
    assert!(p.fake.live().is_empty());
}

#[gpui::test]
fn a_session_that_appears_later_attaches_on_the_row_change(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.fake.idle.lock().unwrap().push(name("alpha"));
    p.open_task("A");
    assert_eq!(p.phase(), Some(SessionPhase::Idle));
    assert!(p.drawn("session-notice"));
    p.fake.idle.lock().unwrap().clear();
    let mut row = ws("alpha", Some("A"));
    row.badge = WorkspaceBadge::AwaitingInput;
    p.view.update(&mut p.cx, |v, cx| v.set_task_rows(vec![row, ws("bravo", Some("B"))], Vec::new(), cx));
    p.draw();
    assert_eq!(p.phase(), Some(SessionPhase::Live));
    assert_eq!(p.relaunches(), 0);
}

#[gpui::test]
fn a_remote_session_reconnects_after_its_link_drops(cx: &mut TestAppContext) {
    let mut p = open(cx);
    let mut row = ws("alpha", Some("A"));
    row.is_remote = true;
    row.machine = "gpu".into();
    p.view.update(&mut p.cx, |v, cx| v.set_task_rows(vec![row, ws("bravo", Some("B"))], Vec::new(), cx));
    p.open_task("A");
    assert_eq!(p.phase(), Some(SessionPhase::Live));
    p.fake.unreachable.lock().unwrap().push(name("alpha"));
    p.fake.drop_streams(&name("alpha"));
    p.draw();
    assert!(matches!(p.phase(), Some(SessionPhase::Reconnecting(_))), "{:?}", p.phase());
    assert_eq!(p.relaunches(), 0, "a dropped link isn't a death to relaunch");
}

#[gpui::test]
fn output_while_editing_the_description_keeps_the_editor_focused(cx: &mut TestAppContext) {
    let mut p = open(cx);
    p.open_task("A");
    p.cx.simulate_keystrokes("e");
    p.draw();
    let view = p.view.clone();
    let editor_focused = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            let v = view.read(cx);
            let e = v.pane_editors.as_ref().expect("the pane's editors");
            gpui::Focusable::focus_handle(e.body.read(cx), cx).is_focused(window)
        })
    };
    assert!(editor_focused(&mut p.cx));
    let scroll = p.view.read_with(&p.cx, |v, _| v.detail_scroll.offset());
    for _ in 0..5 {
        p.fake.push(&name("alpha"), b"streaming output\r\n");
        p.draw();
    }
    assert!(editor_focused(&mut p.cx), "output didn't take the keys");
    assert_eq!(p.view.read_with(&p.cx, |v, _| v.detail_scroll.offset()), scroll, "nor scroll the details");
    p.cx.simulate_keystrokes("z");
    p.draw();
    assert_eq!(p.fake.typed(), "", "typing went to the draft");
}
