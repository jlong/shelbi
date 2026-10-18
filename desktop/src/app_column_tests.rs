//! The leading column with a review or a workspace open
//! (`dt-review-replaces-sidebar`, `dt-workspace-sidebar`): the panel takes the
//! sidebar's column at the sidebar's width, the push between them runs both
//! ways (and turns around mid-way), going from one panel to the other passes
//! through the sidebar, and leaving gives the sidebar back as it was:
//! selection, scroll, collapsed state and keys.
//!
//! The window is the smoke test's headless one ([`Harness`]) with fake review
//! and workspace sessions and daemon ops, an injected clock for the
//! transition and an injected Reduce Motion setting.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui::{point, Bounds, Focusable, Modifiers, Pixels, TestAppContext};
use shelbi_app::nav::DEFAULT_SIDEBAR_WIDTH;
use shelbi_app::review::PanelEffect;
use shelbi_app::view::{ReviewState, WorkspaceBadge, WorkspaceRow};

use super::smoke::Harness;
use super::*;
use crate::views::column_swap::SLIDE;
use std::sync::Arc;

use shelbi_app::review::ReviewTarget;

use crate::views::review::tests::{fake_deps, info, row, FakeOps, FakeSessions};
use crate::views::workspace::tests::{fake_deps as fake_workspace_deps, FakeSessions as FakeWorkspaceSessions};

struct Column {
    h: Harness,
    now: Rc<Cell<Instant>>,
    reduce_motion: Rc<Cell<bool>>,
    sessions: Arc<FakeSessions>,
    ws_sessions: Arc<FakeWorkspaceSessions>,
    ops: Arc<FakeOps>,
}

impl Column {
    fn open(cx: &mut TestAppContext) -> Column {
        Self::open_with(cx, |_| {})
    }

    fn open_with(cx: &mut TestAppContext, setup: impl FnOnce(&mut ShelbiWindow)) -> Column {
        let mut h = Harness::open(cx);
        let now = Rc::new(Cell::new(Instant::now()));
        let reduce_motion = Rc::new(Cell::new(false));
        let (deps, sessions, ops) = fake_deps();
        let (ws_deps, ws_sessions, _ws_ops) = fake_workspace_deps();
        let (n, r, o) = (now.clone(), reduce_motion.clone(), ops.clone());
        h.window.update(&mut h.cx, |w, _| {
            w.clock = Rc::new(move || n.get());
            w.reduce_motion = Rc::new(move || r.get());
            w.review_ops = o;
            w.review_deps = Some(deps);
            w.workspace_deps = Some(ws_deps);
            w.model.reviews.push(row(ReviewState::Serving));
            // The fixture's workspace works the task the fake details describe.
            w.model.workspaces[0].current_task = Some("t-1".into());
            setup(w);
        });
        let mut c = Column {
            h,
            now,
            reduce_motion,
            sessions,
            ws_sessions,
            ops,
        };
        c.h.draw();
        c
    }

    fn update<R>(&mut self, f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>) -> R) -> R {
        let window = self.h.window.clone();
        let r = self.h.cx.update(|win, cx| window.update(cx, |w, cx| f(w, win, cx)));
        self.h.cx.run_until_parked();
        r
    }

    fn read<R>(&self, f: impl FnOnce(&ShelbiWindow, &App) -> R) -> R {
        self.h.window.read_with(&self.h.cx, f)
    }

    /// Select the review's sidebar row, then open the review (what Enter on
    /// it does once the load flow resolves it as serving).
    fn open_review(&mut self) {
        let ordinal = self.ordinal_of(&SidebarTarget::Review("t-1".into()));
        self.update(|w, window, cx| {
            let count = w.selectable_count();
            w.client.select(ordinal, count);
            w.open_review(info(), window, cx);
        });
        self.h.draw();
    }

    /// Select the workspace's sidebar row, then show it (what Enter on it
    /// does: `FocusWorkspace` shows its session view; the tap only records
    /// commands, so the view is shown directly).
    fn open_workspace(&mut self) {
        let ordinal = self.ordinal_of(&SidebarTarget::Workspace("alpha-1".into()));
        self.update(|w, window, cx| {
            let count = w.selectable_count();
            w.client.select(ordinal, count);
            w.client.focus_main();
            w.perform(HostCall::ShowView(View::Session("alpha-1".into())), window, cx);
        });
        self.h.draw();
    }

    fn workspace(&self) -> Entity<WorkspaceView> {
        self.read(|w, _| w.workspace.clone().expect("a workspace was shown"))
    }

    fn workspace_panel_focused(&mut self) -> bool {
        let ws = self.workspace();
        self.h.cx.update(|window, cx| ws.read(cx).focus_handle(cx).is_focused(window))
    }

    fn ordinal_of(&self, target: &SidebarTarget) -> usize {
        self.read(|w, _| {
            (0..w.selectable_count())
                .find(|&i| w.target_at(i).as_ref() == Some(target))
                .expect("the sidebar lists it")
        })
    }

    /// Ask for review `t-1` as a click or Enter on its row does, and draw
    /// the next frame, without letting background work (the load flow's
    /// resolve) run: the frame the user sees before the daemon answers.
    fn ask_for_review(&mut self) {
        let window = self.h.window.clone();
        self.h.cx.update(|win, cx| {
            window.update(cx, |w, cx| w.perform(HostCall::LoadReview("t-1".into()), win, cx))
        });
        self.draw_only();
    }

    /// Draw a frame without running background work.
    fn draw_only(&mut self) {
        self.h.cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    /// Move the clock on and draw, background work still held.
    fn advance_only(&mut self, by: Duration) {
        self.now.set(self.now.get() + by);
        self.draw_only();
    }

    /// Move the transition's clock on and draw.
    fn advance(&mut self, by: Duration) {
        self.now.set(self.now.get() + by);
        self.h.draw();
    }

    fn bounds(&mut self, selector: &'static str) -> Option<Bounds<Pixels>> {
        self.h.cx.debug_bounds(selector)
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.bounds(selector).is_some()
    }

    fn review(&self) -> Entity<ReviewView> {
        self.read(|w, _| w.review.clone().expect("a review was opened"))
    }

    fn back(&mut self) {
        let review = self.review();
        self.h.cx.update(|_, cx| review.update(cx, |r, cx| r.apply(PanelEffect::Back, cx)));
        self.h.cx.run_until_parked();
        self.h.draw();
    }

    fn width(&mut self, selector: &'static str) -> f32 {
        self.bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).size.width.as_f32()
    }

    fn left(&mut self, selector: &'static str) -> f32 {
        self.bounds(selector).unwrap_or_else(|| panic!("{selector} is drawn")).origin.x.as_f32()
    }
}

fn sidebar_px(cols: u16) -> f32 {
    f32::from(cols) * sidebar::COLUMN_PX
}

#[gpui::test]
fn without_a_review_the_column_is_the_sidebar(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    assert!(c.drawn("column-sidebar") && c.drawn("sidebar"));
    assert!(!c.drawn("column-review") && !c.drawn("review-panel"));
    assert_eq!(c.width("leading-column"), sidebar_px(DEFAULT_SIDEBAR_WIDTH));
    assert!(c.drawn("sidebar-resize"));
    assert!(!c.drawn("expand-sidebar"));
}

#[gpui::test]
fn a_review_takes_the_sidebars_column_with_a_push(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let column_w = c.width("leading-column");
    c.open_review();

    // The first frame: both, already one frame's step into the push (the
    // sidebar a little left, the panel coming in from just right of the
    // column), the session already beside the column.
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"));
    assert_eq!(c.width("leading-column"), column_w, "the column keeps its width");
    let column_x = c.left("leading-column");
    assert!(c.left("column-sidebar") < column_x, "it moves on the first frame");
    let first = c.left("column-review");
    assert!(first > column_x + column_w / 2.0 && first < column_x + column_w);
    let session_x = c.left("review-view");
    assert!(session_x >= column_x + column_w, "the session sits right of the column");

    // Mid-way: the sidebar leaves left, the panel comes in from the right,
    // and nothing else moves.
    c.advance(SLIDE / 2);
    assert!(c.left("column-sidebar") < column_x);
    let panel_x = c.left("column-review");
    assert!(panel_x > column_x && panel_x < column_x + column_w);
    assert_eq!(c.width("leading-column"), column_w);
    assert_eq!(c.left("review-view"), session_x, "the session column never moves");

    // Done: two columns, the review panel then the session; no sidebar.
    c.advance(SLIDE);
    assert!(!c.drawn("column-sidebar") && !c.drawn("sidebar"));
    assert!(c.drawn("review-panel") && c.drawn("review-content"));
    assert_eq!(c.left("column-review"), column_x);
    assert_eq!(c.width("review-panel"), column_w, "the panel fills the sidebar's width");
    assert_eq!(c.left("review-view"), session_x);
    let review_x = c.left("review-panel");
    assert!(review_x < c.left("review-content"));
}

#[gpui::test]
fn focus_moves_to_the_panel_and_back_to_the_sidebar_row(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let selected = c.ordinal_of(&SidebarTarget::Review("t-1".into()));
    c.open_review();
    c.advance(SLIDE);
    let review = c.review();
    let panel_focused = c.h.cx.update(|window, cx| review.read(cx).focus_handle(cx).is_focused(window));
    assert!(panel_focused, "the panel has the keys");

    // Esc in the panel is Back: the reverse push, then the sidebar as it was.
    c.h.key("escape");
    c.h.draw();
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"), "both while it leaves");
    c.advance(SLIDE / 2);
    assert!(c.left("column-review") > c.left("leading-column"), "the panel leaves right");
    assert!(c.left("column-sidebar") < c.left("leading-column"), "the sidebar returns from the left");
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-review"));
    assert_eq!(c.read(|w, _| w.client.sidebar_selection()), selected, "selection kept");
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Sidebar);
    let window_focused = c.h.cx.update(|window, cx| c.h.window.read(cx).focus_handle.is_focused(window));
    assert!(window_focused, "the sidebar's keys");
    assert_eq!(c.h.take_calls(), vec![HostCall::ShowView(View::Machines)], "back to the view it opened over");

    // The sidebar's keys work again: Up moves its selection.
    c.h.key("up");
    assert_eq!(c.read(|w, _| w.client.sidebar_selection()), selected - 1);
}

#[gpui::test]
fn the_sidebar_takes_no_keys_while_the_panel_stands_in(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_review();
    c.advance(SLIDE);
    let before = c.read(|w, _| w.client.sidebar_selection());
    // Ctrl+H (focus the sidebar) focuses the panel in its column instead.
    c.update(|w, window, cx| w.focus_sidebar(window, cx));
    let review = c.review();
    let panel_focused = c.h.cx.update(|window, cx| review.read(cx).focus_handle(cx).is_focused(window));
    assert!(panel_focused);
    c.h.key("tab");
    assert_eq!(c.read(|w, _| w.client.sidebar_selection()), before, "the hidden sidebar didn't move");
}

#[gpui::test]
fn close_and_the_task_leaving_review_also_push_back(cx: &mut TestAppContext) {
    // Close (`q`).
    let mut c = Column::open(cx);
    c.open_review();
    c.advance(SLIDE);
    let review = c.review();
    c.h.cx.update(|_, cx| review.update(cx, |r, cx| r.apply(PanelEffect::Close, cx)));
    c.h.cx.run_until_parked();
    c.h.draw();
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"), "the reverse push");
    assert!(c.drawn("review-panel"), "the closed panel still draws while it leaves");
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("review-panel"));
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Sidebar);

    // The task left review elsewhere (its row is gone from a fresh model).
    c.open_review();
    c.advance(SLIDE);
    assert!(!c.drawn("sidebar"));
    c.update(|w, window, cx| {
        w.model.reviews.clear();
        w.update_review_row(window, cx);
    });
    c.h.draw();
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"));
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-review"));
}

#[gpui::test]
fn back_mid_push_turns_around_from_where_it_stands(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let column_x = c.left("leading-column");
    c.open_review();
    c.advance(Duration::from_millis(60));
    let panel_x = c.left("column-review");
    assert!(panel_x > column_x);

    // Back before the panel arrived: it heads out again from where it is.
    c.back();
    let turned = c.left("column-review");
    assert!((turned - panel_x).abs() < 0.5, "no jump: {panel_x} then {turned}");
    c.advance(Duration::from_millis(30));
    assert!(c.left("column-review") > turned, "now leaving right");
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-review"));

    // And re-entering mid-leave turns around the other way.
    c.open_review();
    c.advance(SLIDE);
    c.back();
    c.advance(Duration::from_millis(50));
    c.update(|w, window, cx| w.open_review(info(), window, cx));
    c.h.draw();
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"));
    c.advance(SLIDE);
    assert!(!c.drawn("sidebar") && c.drawn("review-panel"));
}

#[gpui::test]
fn reduce_motion_crossfades_without_a_slide(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.reduce_motion.set(true);
    let column_x = c.left("leading-column");
    c.open_review();
    c.advance(Duration::from_millis(40));
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"));
    assert_eq!(c.left("column-sidebar"), column_x, "no slide");
    assert_eq!(c.left("column-review"), column_x, "no slide");
    c.advance(crate::views::column_swap::FADE);
    assert!(!c.drawn("column-sidebar"));
}

#[gpui::test]
fn the_panel_uses_the_users_width_and_the_divider_resizes_it(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| w.client.set_sidebar_width(40));
    assert_eq!(c.width("leading-column"), sidebar_px(40));
    c.open_review();
    c.advance(SLIDE);
    assert_eq!(c.width("leading-column"), sidebar_px(40), "the user's width");
    assert_eq!(c.width("review-panel"), sidebar_px(40));

    // Drag the divider beside the panel.
    let divider = c.bounds("sidebar-resize").expect("the divider beside the panel").center();
    c.h.cx.simulate_mouse_down(divider, MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_move(point(gpui::px(300.0), divider.y), MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_up(point(gpui::px(300.0), divider.y), MouseButton::Left, Modifiers::none());
    c.h.draw();
    let dragged = c.read(|w, _| w.client.sidebar_width());
    assert_eq!(dragged, 33);
    assert_eq!(c.width("review-panel"), sidebar_px(33), "the panel follows the drag");

    // The sidebar comes back at the dragged width: one column, one width.
    c.back();
    c.advance(SLIDE);
    assert_eq!(c.width("sidebar"), sidebar_px(33));
}

#[gpui::test]
fn a_collapsed_sidebar_lends_its_column_and_stays_collapsed(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| {
        w.collapsed = true;
        w.client.set_sidebar_width(40);
    });
    assert!(!c.drawn("leading-column") && c.drawn("expand-sidebar"));

    c.open_review();
    assert!(c.drawn("column-review") && !c.drawn("column-sidebar"), "never the hidden sidebar");
    assert!(!c.drawn("expand-sidebar"), "the column is up");
    c.advance(SLIDE);
    assert_eq!(c.width("review-panel"), sidebar_px(DEFAULT_SIDEBAR_WIDTH), "the default width");

    // Dragging the collapsed column's divider resizes the panel.
    let divider = c.bounds("sidebar-resize").expect("a divider beside the panel").center();
    c.h.cx.simulate_mouse_down(divider, MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_move(point(gpui::px(300.0), divider.y), MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_up(point(gpui::px(300.0), divider.y), MouseButton::Left, Modifiers::none());
    c.h.draw();
    assert_eq!(c.width("review-panel"), sidebar_px(33));

    // Back: the panel leaves and the column with it; still collapsed.
    c.back();
    assert!(c.drawn("column-review"));
    c.advance(SLIDE);
    assert!(!c.drawn("leading-column") && c.drawn("expand-sidebar"));
    assert!(c.read(|w, _| w.collapsed));
    assert_ne!(c.read(|w, _| w.client.focus()), Focus::Sidebar, "a collapsed sidebar takes no keys");

    // The next review starts at the default width again.
    c.open_review();
    c.advance(SLIDE);
    assert_eq!(c.width("review-panel"), sidebar_px(DEFAULT_SIDEBAR_WIDTH));
}

#[gpui::test]
fn the_sidebars_scroll_survives_the_review(cx: &mut TestAppContext) {
    // Enough workspaces to scroll.
    let mut c = Column::open_with(cx, |w| {
        w.model.workspaces = (0..80)
            .map(|i| WorkspaceRow {
                name: format!("ws-{i}"),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                agent: None,
                badge: WorkspaceBadge::Idle,
            })
            .collect();
    });
    let scrolled = point(gpui::px(0.0), gpui::px(-200.0));
    c.read(|w, _| w.sidebar_scroll.set_offset(scrolled));
    c.h.draw();
    assert_eq!(c.read(|w, _| w.sidebar_scroll.offset()), scrolled);

    c.open_review();
    c.advance(SLIDE);
    assert!(!c.drawn("sidebar"));
    c.back();
    c.advance(SLIDE);
    assert!(c.drawn("sidebar"));
    assert_eq!(c.read(|w, _| w.sidebar_scroll.offset()), scrolled, "scrolled where it was");
}

#[gpui::test]
fn a_workspace_takes_the_sidebars_column_with_a_push(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let column_w = c.width("leading-column");
    let column_x = c.left("leading-column");
    c.open_workspace();

    // The first frame: both, the panel one step in from just right of the
    // column.
    assert!(c.drawn("column-sidebar") && c.drawn("column-workspace"));
    assert!(!c.drawn("column-review"));
    let first = c.left("column-workspace");
    assert!(first > column_x + column_w / 2.0 && first < column_x + column_w);
    let session_x = c.left("workspace-view");
    assert!(session_x >= column_x + column_w, "the terminal sits right of the column");

    // Mid-way.
    c.advance(SLIDE / 2);
    assert!(c.left("column-sidebar") < column_x);
    let panel_x = c.left("column-workspace");
    assert!(panel_x > column_x && panel_x < column_x + column_w);
    assert_eq!(c.left("workspace-view"), session_x, "the terminal never moves");

    // Done: the panel in the column, the terminal beside it from the top.
    c.advance(SLIDE);
    assert!(!c.drawn("column-sidebar") && !c.drawn("sidebar"));
    assert!(c.drawn("workspace-panel"));
    assert_eq!(c.width("workspace-panel"), column_w, "the panel fills the sidebar's width");
    assert!(c.drawn("ws-switch-agent") && c.drawn("ws-shell-toggle"));
    let main = c.bounds("workspace-view").unwrap();
    let panes = c.bounds("ws-panes").unwrap();
    assert_eq!(
        panes.origin.y,
        main.origin.y + gpui::px(1.0),
        "the agent terminal starts at the top, under the frame's 1px rule"
    );
    assert_eq!(main.origin.y, gpui::px(0.0));
    assert!(!c.drawn("workspace-header"));
}

#[gpui::test]
fn an_idle_workspace_keeps_the_sidebar_and_a_task_pushes_the_panel(cx: &mut TestAppContext) {
    // No task: the Workspace idle frame (Figma 2174:851) keeps the nav
    // sidebar, and the main area shows the idle page.
    let mut c = Column::open_with(cx, |w| {
        w.model.workspaces[0].current_task = None;
        w.model.workspaces[0].badge = WorkspaceBadge::Idle;
    });
    c.open_workspace();
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-workspace"), "no push");
    assert!(c.drawn("workspace-toolbar"), "the idle page's toolbar");
    let ws = c.workspace();
    assert!(!c.read(|_, cx| ws.read(cx).is_holding()), "nothing waits for a push");

    // A dispatch gives it a task: the panel pushes in.
    c.update(|w, _, _| w.model.workspaces[0].current_task = Some("t-1".into()));
    c.update(|w, _, cx| w.update_workspace_row(cx));
    c.h.draw();
    assert!(c.drawn("column-workspace"), "the push starts");
    c.advance(SLIDE);
    assert!(c.drawn("workspace-panel") && !c.drawn("sidebar"));
    assert!(!c.drawn("workspace-toolbar"));
}

#[gpui::test]
fn esc_on_the_workspace_panel_pushes_back_to_the_sidebar_as_it_was(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| {
        let mut rows: Vec<WorkspaceRow> = (0..80)
            .map(|i| WorkspaceRow {
                name: format!("ws-{i}"),
                machine: "hub".into(),
                is_remote: false,
                current_task: None,
                agent: None,
                badge: WorkspaceBadge::Idle,
            })
            .collect();
        rows.insert(0, w.model.workspaces[0].clone());
        w.model.workspaces = rows;
    });
    let scrolled = point(gpui::px(0.0), gpui::px(-120.0));
    c.read(|w, _| w.sidebar_scroll.set_offset(scrolled));
    c.h.draw();
    let selected = c.ordinal_of(&SidebarTarget::Workspace("alpha-1".into()));
    c.open_workspace();
    c.advance(SLIDE);
    assert!(c.workspace_panel_focused(), "the panel has the keys");

    // Arrow keys move the panel's selection, not the hidden sidebar's.
    c.h.key("down");
    assert_eq!(c.read(|w, _| w.client.sidebar_selection()), selected);

    // Esc is Back: the reverse push, then the sidebar as it was.
    c.h.key("escape");
    c.h.draw();
    assert!(c.drawn("column-sidebar") && c.drawn("column-workspace"), "both while it leaves");
    c.advance(SLIDE / 2);
    assert!(c.left("column-workspace") > c.left("leading-column"), "the panel leaves right");
    assert!(c.left("column-sidebar") < c.left("leading-column"), "the sidebar returns from the left");
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-workspace"));
    assert_eq!(c.read(|w, _| w.client.sidebar_selection()), selected, "the selection is still on it");
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Sidebar);
    let window_focused = c.h.cx.update(|window, cx| c.h.window.read(cx).focus_handle.is_focused(window));
    assert!(window_focused, "the sidebar's keys");
    assert_eq!(c.read(|w, _| w.sidebar_scroll.offset()), scrolled, "scrolled where it was");
    assert_eq!(
        c.h.take_calls(),
        vec![HostCall::ShowView(View::Machines)],
        "back to the view it was opened over"
    );
}

#[gpui::test]
fn the_back_button_and_a_toggle_mid_push(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let column_x = c.left("leading-column");
    c.open_workspace();
    c.advance(Duration::from_millis(60));
    let panel_x = c.left("column-workspace");
    assert!(panel_x > column_x);

    // The back button before the panel arrived: it heads out from where it is.
    let ws = c.workspace();
    c.h.cx.update(|_, cx| ws.update(cx, |_, cx| cx.emit(crate::views::workspace::WorkspaceEvent::Back)));
    c.h.cx.run_until_parked();
    c.h.draw();
    let turned = c.left("column-workspace");
    assert!((turned - panel_x).abs() < 0.5, "no jump: {panel_x} then {turned}");
    c.advance(Duration::from_millis(30));
    assert!(c.left("column-workspace") > turned, "now leaving right");

    // Selecting it again mid-leave turns around once more.
    let leaving = c.left("column-workspace");
    c.open_workspace();
    c.advance(Duration::from_millis(10));
    assert!(c.left("column-workspace") < leaving, "coming back in");
    c.advance(SLIDE);
    assert!(!c.drawn("sidebar") && c.drawn("workspace-panel"));

    // The back button itself.
    c.h.click_selector("ws-back");
    c.h.draw();
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-workspace"));
}

#[gpui::test]
fn the_sidebar_chord_focuses_the_workspace_panel(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_workspace();
    c.advance(SLIDE);
    let before = c.read(|w, _| w.client.sidebar_selection());
    c.update(|w, window, cx| w.focus_sidebar(window, cx));
    assert!(c.workspace_panel_focused());
    c.h.key("up");
    assert_eq!(c.read(|w, _| w.client.sidebar_selection()), before, "the hidden sidebar didn't move");
}

#[gpui::test]
fn a_review_from_a_workspace_passes_through_the_sidebar_and_back(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_workspace();
    c.advance(SLIDE);

    // A review opened over the workspace: the workspace panel pops first.
    c.update(|w, window, cx| w.open_review(info(), window, cx));
    c.h.draw();
    c.advance(SLIDE / 2);
    assert!(c.drawn("column-workspace") && c.drawn("column-sidebar"));
    assert!(!c.drawn("column-review"), "the review waits for the sidebar");
    c.advance(SLIDE / 2);
    c.advance(SLIDE / 2);
    assert!(c.drawn("column-review") && c.drawn("column-sidebar"));
    assert!(!c.drawn("column-workspace"));
    c.advance(SLIDE);
    assert!(c.drawn("review-panel") && !c.drawn("column-sidebar"));

    // Back from the review returns to the workspace, through the sidebar.
    c.back();
    c.advance(SLIDE / 2);
    assert!(c.drawn("column-review") && !c.drawn("column-workspace"));
    c.advance(SLIDE * 2);
    assert!(c.drawn("workspace-panel") && !c.drawn("column-review"));
    assert_eq!(c.read(|w, _| w.workspace_return.clone()), Some(View::Machines), "its Back target kept");
}

#[gpui::test]
fn a_collapsed_sidebar_lends_its_column_to_the_workspace_panel(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| w.collapsed = true);
    assert!(!c.drawn("leading-column"));
    c.open_workspace();
    assert!(c.drawn("column-workspace") && !c.drawn("column-sidebar"));
    c.advance(SLIDE);
    assert_eq!(c.width("workspace-panel"), sidebar_px(DEFAULT_SIDEBAR_WIDTH));
    c.h.key("escape");
    c.h.draw();
    c.advance(SLIDE);
    assert!(!c.drawn("leading-column") && c.drawn("expand-sidebar"));
    assert!(c.read(|w, _| w.collapsed));
}

#[gpui::test]
fn reduce_motion_crossfades_the_workspace_panel(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.reduce_motion.set(true);
    let column_x = c.left("leading-column");
    c.open_workspace();
    c.advance(Duration::from_millis(40));
    assert!(c.drawn("column-sidebar") && c.drawn("column-workspace"));
    assert_eq!(c.left("column-workspace"), column_x, "no slide");
    c.advance(crate::views::column_swap::FADE);
    assert!(!c.drawn("column-sidebar") && c.drawn("workspace-panel"));
}

// --- dt-instant-panel-push --------------------------------------------------

#[gpui::test]
fn the_push_starts_on_the_input_before_the_load_answers(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let (column_x, column_w) = (c.left("leading-column"), c.width("leading-column"));

    // The click's very next frame: the panel is in the column and moving,
    // its title from the sidebar row and placeholders for the rest, though
    // the resolve hasn't even run.
    c.ask_for_review();
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"));
    assert!(c.left("column-sidebar") < column_x, "moving on the first frame");
    assert!(c.read(|w, cx| w.review.as_ref().unwrap().read(cx).is_pending()));
    assert!(c.drawn("review-title") && c.drawn("review-loading"));
    assert!(!c.drawn("review-approve"), "nothing to decide on yet");
    c.advance_only(Duration::from_millis(50));
    let before = c.left("column-review");
    assert!(before > column_x && before < column_x + column_w);

    // The resolve answers mid-push: the panel fills in where it stands, and
    // the content waits for the push to settle.
    c.h.cx.run_until_parked();
    c.draw_only();
    assert!(!c.read(|w, cx| w.review.as_ref().unwrap().read(cx).is_pending()));
    assert!(c.drawn("review-server") && c.drawn("review-approve") && !c.drawn("review-loading"));
    assert!((c.left("column-review") - before).abs() < 0.5, "no jump when it fills in");
    assert!(c.sessions.attached().is_empty(), "no attach in the push's frames");

    // Settled: the reviewer attaches.
    c.advance(SLIDE);
    assert!(!c.drawn("column-sidebar"));
    c.h.cx.run_until_parked();
    assert_eq!(c.sessions.attached(), vec!["alpha/ws/review-1".to_string()]);
}

#[gpui::test]
fn a_load_that_fails_mid_push_turns_back_from_where_it_stands(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    *c.ops.resolve_with.lock().unwrap() = Some(Err("`t-1` is not a review task".into()));
    c.ask_for_review();
    c.advance_only(Duration::from_millis(60));
    let at = c.left("column-review");

    // The resolve fails: the push turns around where it is, no freeze, no
    // jump, and the failure is reported.
    c.h.cx.run_until_parked();
    c.draw_only();
    assert!((c.left("column-review") - at).abs() < 0.5, "no jump: {at} then {}", c.left("column-review"));
    c.advance(Duration::from_millis(30));
    assert!(c.left("column-review") > at, "now leaving right");
    assert!(c.read(|w, _| w.banner.as_ref().is_some_and(|b| b.contains("not a review task"))));
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-review"));
    assert!(c.sessions.attached().is_empty(), "nothing attached");
    assert_eq!(c.h.take_calls(), vec![HostCall::ShowView(View::Machines)], "back where it was");
}

#[gpui::test]
fn a_slot_prompt_shows_over_the_panel_and_dismissing_it_backs_out(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    *c.ops.resolve_with.lock().unwrap() = Some(Ok(ReviewTarget::Queued {
        title: "Cold-start cache".into(),
        free_slots: vec!["review-1".into()],
        total_slots: 1,
    }));
    c.ask_for_review();
    c.h.cx.run_until_parked();
    c.draw_only();
    assert!(c.read(|w, _| matches!(w.overlay, Some(ActiveOverlay::ReviewConfirm { .. }))));
    assert!(c.drawn("column-review"), "the panel stays up behind the prompt");
    c.advance(SLIDE);
    assert!(c.drawn("review-loading") && !c.drawn("column-sidebar"));

    // Dismissed: no slot, so the panel backs out.
    c.h.key("escape");
    c.h.draw();
    assert!(c.read(|w, _| w.overlay.is_none()));
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-review"));
}

#[gpui::test]
fn the_push_clock_starts_at_the_first_frame(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let (column_x, column_w) = (c.left("leading-column"), c.width("leading-column"));
    let (window, now) = (c.h.window.clone(), c.now.clone());
    // The first frame comes 150ms after the input (a slow one, the panel
    // built in it): it still shows the push's first step, not 150ms of it,
    // which would have it nearly in.
    c.h.cx.update(|win, cx| {
        window.update(cx, |w, cx| w.perform(HostCall::LoadReview("t-1".into()), win, cx));
        now.set(now.get() + Duration::from_millis(150));
    });
    c.draw_only();
    let first = c.left("column-review");
    let step = 1.0 - crate::views::column_swap::ease_out(1.0 / 60.0 / SLIDE.as_secs_f32());
    assert!((first - (column_x + column_w * step)).abs() < 1.0, "one step in: {first}");
    c.advance_only(Duration::from_millis(16));
    assert!(c.left("column-review") < first, "and on from there");
}

#[gpui::test]
fn a_workspace_attaches_once_its_push_settles(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_workspace();
    assert!(c.drawn("column-workspace") && c.drawn("column-sidebar"));
    assert!(c.ws_sessions.attached().is_empty(), "no attach in the push's frames");
    c.advance(SLIDE / 2);
    assert!(c.ws_sessions.attached().is_empty());
    c.advance(SLIDE);
    c.h.cx.run_until_parked();
    assert_eq!(c.ws_sessions.attached(), vec!["alpha/ws/alpha-1".to_string()]);
}
