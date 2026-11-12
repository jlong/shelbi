//! The leading column with a review or a workspace open
//! (`dt-review-replaces-sidebar`, `dt-workspace-sidebar`): the panel takes the
//! sidebar's column at the sidebar's width, the push between them runs both
//! ways (and turns around mid-way), going from one panel to the other passes
//! through the sidebar, and leaving gives the sidebar back as it was (scroll,
//! collapsed state and keys) with its selection on the main area's row
//! (`dt-back-syncs-sidebar`). Toggle sidebar slides the column out and back
//! in (`dt-sidebar-toggle-animation`).
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
use shelbi_app::workspace_panel::WsRow;

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
            w.review_ops = Some(o);
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
        self.update(|w, window, cx| {
            w.select_row(SidebarTarget::Review("t-1".into()), cx);
            w.open_review(info(), window, cx);
        });
        self.h.draw();
    }

    /// Select the workspace's sidebar row, then show it (what Enter on it
    /// does: `FocusWorkspace` shows its session view; the tap only records
    /// commands, so the view is shown directly).
    fn open_workspace(&mut self) {
        self.update(|w, window, cx| {
            w.select_row(SidebarTarget::Workspace("alpha-1".into()), cx);
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

    /// The sidebar's selected row.
    fn selected(&self) -> Option<SidebarTarget> {
        self.read(|w, cx| w.selected(cx))
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
    assert!(c.drawn("tab-bar-sidebar"), "the tab bar keeps the toggle");
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
    // Machines, the view it opened over, has no row: none is selected.
    assert_eq!(c.selected(), None, "no stale review row");
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Sidebar);
    let window_focused = c.h.cx.update(|window, cx| c.h.window.read(cx).focus_handle.is_focused(window));
    assert!(window_focused, "the sidebar's keys");
    assert_eq!(c.h.take_calls(), vec![HostCall::ShowView(View::Machines)], "back to the view it opened over");

    // The sidebar's keys work again: Up, with nothing selected, takes the
    // bottom row.
    c.h.key("up");
    let last = c.read(|w, _| w.selectable_count()) - 1;
    assert_eq!(c.selected(), c.read(|w, _| w.target_at(last)));
}

#[gpui::test]
fn the_sidebar_takes_no_keys_while_the_panel_stands_in(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_review();
    c.advance(SLIDE);
    let before = c.selected();
    // Ctrl+H (focus the sidebar) focuses the panel in its column instead.
    c.update(|w, window, cx| w.focus_sidebar(window, cx));
    let review = c.review();
    let panel_focused = c.h.cx.update(|window, cx| review.read(cx).focus_handle(cx).is_focused(window));
    assert!(panel_focused);
    c.h.key("tab");
    assert_eq!(c.selected(), before, "the hidden sidebar didn't move");
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
    assert_eq!(dragged, 30);
    assert_eq!(c.width("review-panel"), sidebar_px(30), "the panel follows the drag");

    // The sidebar comes back at the dragged width: one column, one width.
    c.back();
    c.advance(SLIDE);
    assert_eq!(c.width("sidebar"), sidebar_px(30));
}

#[gpui::test]
fn a_collapsed_sidebar_lends_its_column_and_stays_collapsed(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| {
        w.collapsed = true;
        w.client.set_sidebar_width(40);
    });
    assert!(!c.drawn("leading-column") && c.drawn("tab-bar-sidebar"));

    c.open_review();
    assert!(c.drawn("column-review") && !c.drawn("column-sidebar"), "never the hidden sidebar");
    assert!(c.drawn("tab-bar-sidebar"), "the tab bar keeps the toggle");
    c.advance(SLIDE);
    assert_eq!(c.width("review-panel"), sidebar_px(DEFAULT_SIDEBAR_WIDTH), "the default width");

    // Dragging the collapsed column's divider resizes the panel.
    let divider = c.bounds("sidebar-resize").expect("a divider beside the panel").center();
    c.h.cx.simulate_mouse_down(divider, MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_move(point(gpui::px(300.0), divider.y), MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_up(point(gpui::px(300.0), divider.y), MouseButton::Left, Modifiers::none());
    c.h.draw();
    assert_eq!(c.width("review-panel"), sidebar_px(30));

    // Back: the panel leaves and the column with it; still collapsed.
    c.back();
    assert!(c.drawn("column-review"));
    c.advance(SLIDE);
    assert!(!c.drawn("leading-column") && c.drawn("tab-bar-sidebar"));
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
    assert_eq!(main.origin.y, gpui::px(crate::views::tab_bar::TAB_BAR_H), "right under the tab bar");
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
    c.open_workspace();
    c.advance(SLIDE);
    assert!(c.workspace_panel_focused(), "the panel has the keys");

    // Arrow keys move the panel's selection, not the hidden sidebar's.
    c.h.key("down");
    assert_eq!(c.selected(), Some(SidebarTarget::Workspace("alpha-1".into())));

    // Esc is Back: the reverse push, then the sidebar as it was.
    c.h.key("escape");
    c.h.draw();
    assert!(c.drawn("column-sidebar") && c.drawn("column-workspace"), "both while it leaves");
    c.advance(SLIDE / 2);
    assert!(c.left("column-workspace") > c.left("leading-column"), "the panel leaves right");
    assert!(c.left("column-sidebar") < c.left("leading-column"), "the sidebar returns from the left");
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-workspace"));
    assert_eq!(c.selected(), None, "Machines, back in the main area, has no row");
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
    let before = c.selected();
    c.update(|w, window, cx| w.focus_sidebar(window, cx));
    assert!(c.workspace_panel_focused());
    c.h.key("up");
    assert_eq!(c.selected(), before, "the hidden sidebar didn't move");
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
    assert!(!c.drawn("leading-column") && c.drawn("tab-bar-sidebar"));
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
    assert!(c.drawn("review-switch-chat") && c.drawn("review-approve") && !c.drawn("review-loading"));
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

#[gpui::test]
fn a_panel_keeps_the_sidebars_chrome_and_takes_the_numbered_chords(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_review();
    c.advance(SLIDE);
    assert!(c.drawn("review-panel") && !c.drawn("column-sidebar"));
    // The tab bar's toolbar stays over it, and the Navigation component's
    // footer under it, the Zen pill at the 24pt every state draws.
    for selector in ["tab-bar-search", "tab-bar-sidebar", "sidebar-settings", "sidebar-zen"] {
        assert!(c.drawn(selector), "{selector} is drawn around the panel");
    }
    assert_eq!(c.bounds("sidebar-zen").unwrap().size.height, gpui::px(24.0));
    c.h.take_calls();

    // ⌃2 is the panel's View Diff, not the hidden sidebar's second row.
    c.h.key("ctrl-2");
    let active = c.review().read_with(&c.h.cx, |r, _| r.panel().unwrap().active());
    assert_eq!(active, shelbi_app::review::ActiveView::Diff);
    assert!(c.h.take_calls().is_empty(), "no sidebar row opened");

    // The same for the workspace panel: ⌃3 is Edit in <editor>.
    c.back();
    c.advance(SLIDE);
    c.open_workspace();
    c.advance(SLIDE);
    assert!(c.drawn("workspace-panel") && c.drawn("sidebar-zen"));
    c.h.take_calls();
    c.h.key("ctrl-3");
    let active = c.workspace().read_with(&c.h.cx, |w, _| w.panel().active());
    assert_eq!(active, crate::views::workspace::ContentView::Editor);
    assert!(c.h.take_calls().is_empty(), "no sidebar row opened");
}

// --- The sidebar's selection follows the main area (`dt-back-syncs-sidebar`):
// its selected row is the row for what the main area shows, however the
// main area got there.

impl Column {
    /// A window on the orchestrator chat (its sessions faked).
    fn open_on_orchestrator(cx: &mut TestAppContext) -> Column {
        let (ws_deps, _, _) = fake_workspace_deps();
        Self::open_with(cx, move |w| {
            w.chat_deps = Some(ws_deps.sessions.clone());
            w.client.set_view(View::Session("orch".into()));
        })
    }

    /// The row the selection marker is drawn in, by its selector.
    fn marked_in(&mut self, row: &'static str) -> bool {
        let marker = self.bounds("sidebar-selection").expect("a row is selected");
        self.bounds(row).unwrap_or_else(|| panic!("{row} is drawn")).contains(&marker.origin)
    }
}

fn orchestrator() -> Option<SidebarTarget> {
    Some(SidebarTarget::View(View::Session("orch".into())))
}

#[gpui::test]
fn back_from_a_review_selects_the_orchestrator_it_shows(cx: &mut TestAppContext) {
    let mut c = Column::open_on_orchestrator(cx);
    assert_eq!(c.selected(), orchestrator());
    c.open_review();
    assert_eq!(c.selected(), Some(SidebarTarget::Review("t-1".into())), "the review's row while it shows");
    c.advance(SLIDE);

    c.back();
    assert_eq!(c.selected(), orchestrator(), "the main area's row, not the review's");
    // The sidebar coming back has its chip on the row already: no slide.
    assert!(c.drawn("column-sidebar"), "mid-pop");
    assert!(!c.drawn("sliding-chip"), "placed, not slid");
    c.advance(SLIDE);
    assert!(c.drawn("sidebar") && !c.drawn("column-review"));
    assert!(c.marked_in("sidebar-nav-Orchestrator"));
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Sidebar, "keys back on the sidebar");
}

#[gpui::test]
fn a_decision_close_or_the_row_leaving_selects_the_main_areas_row(cx: &mut TestAppContext) {
    let mut c = Column::open_on_orchestrator(cx);

    // A decision leaves the panel the way Back does, once it lands.
    c.open_review();
    c.advance(SLIDE);
    c.update(|w, window, cx| w.leave_review(window, cx));
    c.advance(SLIDE);
    assert_eq!(c.selected(), orchestrator(), "after a decision");

    // Close.
    c.open_review();
    c.advance(SLIDE);
    let review = c.review();
    c.h.cx.update(|_, cx| review.update(cx, |r, cx| r.apply(PanelEffect::Close, cx)));
    c.h.cx.run_until_parked();
    c.advance(SLIDE);
    assert_eq!(c.selected(), orchestrator(), "after a close");

    // The task left review elsewhere: its row goes, the panel with it.
    c.open_review();
    c.advance(SLIDE);
    c.update(|w, window, cx| {
        w.model.reviews.clear();
        w.update_review_row(window, cx);
    });
    c.advance(SLIDE);
    assert!(c.drawn("sidebar"));
    assert_eq!(c.selected(), orchestrator(), "after the row left");
    assert!(c.marked_in("sidebar-nav-Orchestrator"));
}

#[gpui::test]
fn back_from_a_workspace_selects_the_view_it_returns_to(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| w.client.set_view(View::Issues));
    assert_eq!(c.selected(), Some(SidebarTarget::View(View::Issues)));
    c.open_workspace();
    assert_eq!(c.selected(), Some(SidebarTarget::Workspace("alpha-1".into())));
    c.advance(SLIDE);
    c.h.key("escape");
    c.h.draw();
    assert_eq!(c.selected(), Some(SidebarTarget::View(View::Issues)), "the workspace's row let go");
    assert!(!c.drawn("sliding-chip"), "placed, not slid");
    c.advance(SLIDE);
    assert!(c.marked_in("sidebar-nav-Issues"));
}

#[gpui::test]
fn navigation_that_skips_the_sidebar_moves_its_selection(cx: &mut TestAppContext) {
    let (ws_deps, _, _) = fake_workspace_deps();
    let mut c = Column::open_with(cx, move |w| {
        w.chat_deps = Some(ws_deps.sessions.clone());
        w.client.set_view(View::Issues);
    });
    c.h.focus_window();
    assert!(c.marked_in("sidebar-nav-Issues"));

    // A palette command, chord, menu item or notice lands as a `ShowView`:
    // the chip slides to the new view's row, on its spring.
    c.update(|w, window, cx| w.perform(HostCall::ShowView(View::Session("orch".into())), window, cx));
    c.h.draw();
    assert_eq!(c.selected(), orchestrator());
    assert!(c.drawn("sliding-chip"), "a user-driven change slides");
    c.advance(Duration::from_millis(600));
    assert!(c.marked_in("sidebar-nav-Orchestrator"));

    // The keys move the selection without opening anything; showing a view
    // (even the one already shown) takes it back to that view's row.
    c.update(|w, window, cx| {
        w.client.focus_sidebar();
        w.focus_handle.focus(window, cx);
    });
    c.h.key("down");
    assert_eq!(c.selected(), Some(SidebarTarget::View(View::Issues)), "the keys' row");
    c.update(|w, window, cx| w.perform(HostCall::ShowView(View::Session("orch".into())), window, cx));
    assert_eq!(c.selected(), orchestrator(), "the main area's row again");

    // A view changed under the sidebar (a notice for a workspace, ⌘2).
    c.update(|w, window, cx| w.perform(HostCall::ShowView(View::Session("alpha-1".into())), window, cx));
    assert_eq!(c.selected(), Some(SidebarTarget::Workspace("alpha-1".into())));
}

#[gpui::test]
fn a_view_without_a_row_selects_none(cx: &mut TestAppContext) {
    let mut c = Column::open_on_orchestrator(cx);
    assert!(c.marked_in("sidebar-nav-Orchestrator"));
    // Machines has no sidebar row (its entity isn't built here: it reads
    // the machine state).
    c.update(|w, _, cx| {
        w.client.set_view(View::Machines);
        cx.notify();
    });
    c.advance(Duration::from_millis(600));
    assert_eq!(c.selected(), None);
    assert!(!c.drawn("sidebar-selection"), "no stale row selected");
    assert!(!c.drawn("sliding-chip"));

    // Down, with nothing selected, starts at the top.
    c.update(|w, window, cx| {
        w.client.focus_sidebar();
        w.focus_handle.focus(window, cx);
    });
    c.h.key("down");
    assert_eq!(c.selected(), orchestrator());
}

/// The task pane drawn over the main area (the panels' More,
/// `dt-more-opens-detail`), and the board entity floating.
fn detail_over(c: &Column) -> bool {
    c.read(|w, cx| w.detail_over.is_some() && w.issues.as_ref().is_some_and(|v| v.read(cx).is_floating()))
}

#[gpui::test]
fn more_opens_the_task_detail_over_the_workspace_and_esc_comes_back(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_workspace();
    c.advance(SLIDE);
    c.h.take_calls();
    let ws = c.workspace();
    let shown = c.read(|w, cx| w.shown(cx));
    let attached = c.ws_sessions.attached();

    c.h.cx.update(|_, cx| cx.set_reduce_motion(true));
    c.h.click_selector("ws-more");
    c.h.draw();
    assert!(detail_over(&c), "More opens the pane over the screen");
    assert!(c.drawn("issues-over"));
    let main = c.bounds("workspace-view").unwrap();
    let over = c.bounds("issues-over").unwrap();
    assert_eq!(over.origin.x, main.origin.x, "the pane's layer is the main area, not the panel");
    // The same floating pane as on the board: 8pt in from the main area's
    // top, right and bottom.
    let pane = c.bounds("task-pane").unwrap();
    assert_eq!(pane.right(), over.right() - gpui::px(8.0));
    assert_eq!(pane.top(), over.top() + gpui::px(8.0));
    assert_eq!(pane.bottom(), over.bottom() - gpui::px(8.0));
    assert!(pane.left() > main.left(), "the workspace shows beside it");
    assert!(!c.drawn("backdrop"), "no scrim over the workspace");
    assert!(c.drawn("workspace-panel") && c.drawn("workspace-view"), "the workspace stays under it");
    assert_eq!(c.read(|w, cx| w.shown(cx)), shown, "no switch to Issues");
    assert!(
        !c.h.take_calls().contains(&HostCall::ShowView(View::Issues)),
        "the board isn't routed to"
    );
    let selected = c.read(|_, cx| ws.read(cx).panel_selected_row());
    assert_eq!(selected, Some(WsRow::More));

    // Esc closes it: the same screen, the panel's keys and row, the agent
    // still attached.
    c.h.key("escape");
    c.h.draw();
    assert!(!detail_over(&c) && !c.drawn("issues-over"));
    assert_eq!(c.read(|w, cx| w.shown(cx)), shown);
    assert!(c.workspace_panel_focused(), "the keys are back on the panel");
    assert_eq!(c.read(|_, cx| ws.read(cx).panel_selected_row()), selected);
    assert_eq!(c.ws_sessions.attached(), attached, "the agent pane is untouched");
    assert!(c.read(|w, _| w.workspace_return.clone()).is_some(), "Back still returns where it did");

    // Enter on the More row opens it again.
    c.h.key("enter");
    c.h.draw();
    assert!(detail_over(&c), "Enter on More opens it too");
}

#[gpui::test]
fn more_opens_the_task_pane_over_the_review_and_close_hands_it_back(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.h.cx.update(|_, cx| cx.set_reduce_motion(true));
    c.open_review();
    c.advance(SLIDE);
    let review = c.review();

    c.h.click_selector("review-more");
    c.h.draw();
    assert!(detail_over(&c));
    assert!(c.drawn("review-panel"), "the review stays under it");
    assert!(c.read(|w, _| matches!(w.route, Route::Review)));

    // A click beside the pane isn't the pane's: it isn't modal, so it stays
    // up, and the review under the click keeps working.
    let over = c.bounds("issues-over").unwrap();
    let at = over.origin + point(gpui::px(8.0), gpui::px(80.0));
    c.h.cx.simulate_click(at, Modifiers::none());
    c.h.cx.run_until_parked();
    c.h.draw();
    assert!(detail_over(&c) && c.drawn("task-pane"), "a click beside the pane leaves it up");

    // Its Close hands the keys back to the review.
    c.h.click_selector("pane-close");
    c.h.cx.run_until_parked();
    c.h.draw();
    assert!(!detail_over(&c) && !c.drawn("issues-over"));
    assert!(c.read(|w, _| matches!(w.route, Route::Review)));
    let panel_focused = c.h.cx.update(|window, cx| review.read(cx).contains_focus(window, cx));
    assert!(panel_focused, "the keys are back in the review");
}

/// Drag the task pane's left edge `dx` from where it's pressed.
fn drag_pane_edge(c: &mut Column, dx: f32) {
    let edge = c.bounds("task-pane-resize").expect("the pane's edge").center();
    let to = point(edge.x + gpui::px(dx), edge.y);
    c.h.cx.simulate_mouse_down(edge, MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_move(to, MouseButton::Left, Modifiers::none());
    c.h.cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
    c.h.cx.run_until_parked();
    c.h.draw();
}

#[gpui::test]
fn the_task_pane_resizes_over_the_review_and_the_workspace_and_the_window_keeps_it(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| w.window_state.task_pane_width = 700.0);
    c.h.cx.update(|_, cx| cx.set_reduce_motion(true));
    c.open_review();
    c.advance(SLIDE);
    c.h.click_selector("review-more");
    c.h.draw();
    assert!(detail_over(&c));
    assert_eq!(c.width("task-pane"), 700.0, "the window's saved width");

    // Over the review: wider, and the window keeps it to save.
    let right = c.bounds("task-pane").unwrap().right();
    drag_pane_edge(&mut c, -100.0);
    assert_eq!(c.width("task-pane"), 800.0);
    assert_eq!(c.bounds("task-pane").unwrap().right(), right, "the right edge stays put");
    assert_eq!(c.read(|w, _| w.window_state.task_pane_width), 800.0);
    assert!(detail_over(&c) && c.drawn("review-panel"), "the pane and the review stay up");
    c.h.click_selector("pane-close");
    c.h.cx.run_until_parked();
    c.h.draw();
    c.back();
    c.advance(SLIDE);

    // Over the workspace: the same width, and its edge drags the same way.
    c.open_workspace();
    c.advance(SLIDE);
    c.h.click_selector("ws-more");
    c.h.draw();
    assert!(detail_over(&c));
    assert_eq!(c.width("task-pane"), 800.0, "the width outlives the pane");
    drag_pane_edge(&mut c, 60.0);
    assert_eq!(c.width("task-pane"), 740.0);
    assert_eq!(c.read(|w, _| w.window_state.task_pane_width), 740.0);
    assert!(c.drawn("workspace-view"), "the workspace stays under it");
}

#[gpui::test]
fn the_detail_over_a_screen_goes_when_the_main_area_moves_on(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.open_workspace();
    c.advance(SLIDE);
    c.h.click_selector("ws-more");
    assert!(detail_over(&c));
    c.update(|w, window, cx| w.perform(HostCall::ShowView(View::Machines), window, cx));
    c.h.draw();
    assert!(!detail_over(&c) && !c.drawn("issues-over"));
    let open = c.read(|w, cx| w.issues.as_ref().is_some_and(|v| v.read(cx).overlay_open()));
    assert!(!open, "the detail closed with the screen it opened over");
}

#[gpui::test]
fn activity_still_opens_the_detail_on_the_board(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.update(|w, window, cx| w.open_task("t-1", window, cx));
    assert!(!detail_over(&c), "not floating: the board shows it");
    let open = c.read(|w, cx| w.issues.as_ref().is_some_and(|v| v.read(cx).overlay_open()));
    assert!(open);
    assert!(c.h.take_calls().contains(&HostCall::ShowView(View::Issues)), "routed to the board");
}

// ---------------------------------------------------------------------------
// The sidebar's hide and show (`dt-sidebar-toggle-animation`).
// ---------------------------------------------------------------------------

fn held(c: &mut Column) -> bool {
    c.h.cx.update(|window, cx| crate::elements::terminal::ResizeHold::holds(window, cx))
}

fn close_px(a: f32, b: f32) -> bool {
    (a - b).abs() < 0.5
}

/// Where the tab bar's Toggle sidebar button sits, the sidebar shown or
/// not: the toolbar's second 32pt button, centered on the traffic lights.
fn toggle_origin() -> (f32, f32) {
    (crate::views::tab_bar::TOOLBAR_X + 32.0 + 4.0, (crate::views::tab_bar::TAB_BAR_H - 32.0) / 2.0)
}

#[gpui::test]
fn the_toggle_slides_the_sidebar_out_at_its_full_width_and_back(cx: &mut TestAppContext) {
    use crate::views::sidebar_reveal::REVEAL;
    let mut c = Column::open(cx);
    let full = c.width("leading-column");
    let content = c.width("sidebar");
    assert!(!held(&mut c));
    let at = c.bounds("tab-bar-sidebar").expect("the tab bar's toggle").origin;
    assert_eq!((at.x.as_f32(), at.y.as_f32()), toggle_origin());

    // Hide: the first frame is already a step out.
    c.h.click_selector("tab-bar-sidebar");
    c.advance(Duration::ZERO);
    let first = c.width("leading-column");
    assert!(first < full && first > 0.0, "a step in: {first}");
    assert!(c.read(|w, _| w.collapsed && w.window_state.sidebar_collapsed), "the state saves at once");
    assert_ne!(c.read(|w, _| w.client.focus()), Focus::Sidebar, "the keys go to the main area");

    // Part way: the column narrower, the sidebar still at its full width,
    // slid left so its right edge rides the column's; the main area follows.
    c.advance(Duration::from_millis(30));
    let col = c.width("leading-column");
    assert!(col < first && col > 0.0, "{col}");
    assert_eq!(c.width("sidebar"), content, "clipped, never laid out narrower");
    assert!(close_px(c.left("sidebar") + content, col), "its right edge on the column's");
    assert!(held(&mut c), "the terminals hold their resizes");
    // The toggle stays put in the tab bar.
    let at = c.bounds("tab-bar-sidebar").expect("drawn while the sidebar leaves").origin;
    assert_eq!((at.x.as_f32(), at.y.as_f32()), toggle_origin());

    // Settled: no column, the button where it rests, the hold released.
    c.advance(REVEAL);
    assert!(!c.drawn("leading-column") && !c.drawn("sidebar"));
    let at = c.bounds("tab-bar-sidebar").expect("the toggle stays reachable").origin;
    assert_eq!((at.x.as_f32(), at.y.as_f32()), toggle_origin());
    assert!(!held(&mut c));

    // Show: it slides back in from the left.
    c.h.click_selector("tab-bar-sidebar");
    c.advance(Duration::from_millis(30));
    let col = c.width("leading-column");
    assert!(col > 0.0 && col < full, "{col}");
    assert_eq!(c.width("sidebar"), content);
    assert!(c.left("sidebar") < 0.0);
    assert!(!c.read(|w, _| w.collapsed || w.window_state.sidebar_collapsed));
    assert!(held(&mut c));
    c.advance(REVEAL);
    assert_eq!(c.width("leading-column"), full);
    assert_eq!(c.left("sidebar"), 0.0);
    assert!(!held(&mut c));
}

#[gpui::test]
fn a_second_toggle_mid_way_turns_the_sidebar_around(cx: &mut TestAppContext) {
    use crate::views::sidebar_reveal::REVEAL;
    let mut c = Column::open(cx);
    let full = c.width("leading-column");
    c.update(|w, _, cx| w.toggle_sidebar(cx));
    c.advance(Duration::ZERO);
    c.advance(Duration::from_millis(25));
    let at = c.width("leading-column");
    assert!(at > 0.0 && at < full);

    // Back before it finished: the same frame draws it where it stood.
    c.update(|w, _, cx| w.toggle_sidebar(cx));
    c.advance(Duration::ZERO);
    assert!(close_px(c.width("leading-column"), at), "no jump from {at}");
    c.advance(Duration::from_millis(16));
    assert!(c.width("leading-column") > at, "heading back in");
    c.advance(REVEAL);
    assert_eq!(c.width("leading-column"), full);
    assert!(!c.read(|w, _| w.collapsed));
    assert!(!held(&mut c));
}

#[gpui::test]
fn reduce_motion_toggles_the_sidebar_at_once(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.reduce_motion.set(true);
    c.h.click_selector("tab-bar-sidebar");
    c.advance(Duration::ZERO);
    assert!(!c.drawn("leading-column") && c.drawn("tab-bar-sidebar"));
    assert!(!held(&mut c), "nothing moves, nothing is held");
    c.h.click_selector("tab-bar-sidebar");
    c.advance(Duration::ZERO);
    assert_eq!(c.width("sidebar"), c.width("leading-column"));
    assert_eq!(c.left("sidebar"), 0.0);
    assert!(c.drawn("tab-bar-sidebar"), "the tab bar keeps the toggle");
}

#[gpui::test]
fn a_window_that_opens_collapsed_does_not_animate(cx: &mut TestAppContext) {
    let mut c = Column::open_with(cx, |w| w.collapsed = true);
    assert!(!c.drawn("leading-column") && c.drawn("tab-bar-sidebar"));
    let at = c.bounds("tab-bar-sidebar").unwrap().origin;
    assert_eq!((at.x.as_f32(), at.y.as_f32()), toggle_origin());
    assert!(!held(&mut c));
    assert!(!c.read(|w, _| w.reveal.clone()).is_animating());
}

#[gpui::test]
fn showing_the_sidebar_over_a_collapsed_panel_column_widens_it(cx: &mut TestAppContext) {
    use crate::views::sidebar_reveal::REVEAL;
    let mut c = Column::open_with(cx, |w| {
        w.collapsed = true;
        w.client.set_sidebar_width(40);
    });
    c.open_review();
    c.advance(SLIDE);
    assert_eq!(c.width("leading-column"), sidebar_px(DEFAULT_SIDEBAR_WIDTH));

    // Shown: the panel keeps the column, which widens to the user's width;
    // no titlebar row opens in the main area.
    c.update(|w, _, cx| w.toggle_sidebar(cx));
    c.advance(Duration::ZERO);
    c.advance(Duration::from_millis(30));
    let mid = c.width("leading-column");
    assert!(mid > sidebar_px(DEFAULT_SIDEBAR_WIDTH) && mid < sidebar_px(40), "{mid}");
    assert!(c.drawn("column-review") && !c.drawn("column-sidebar"));
    assert!(c.drawn("tab-bar-sidebar"), "the tab bar keeps the toggle");
    c.advance(REVEAL);
    assert_eq!(c.width("leading-column"), sidebar_px(40));
    assert_eq!(c.width("review-panel"), sidebar_px(40));

    // And hidden again: back to the panel's width, the column kept.
    c.update(|w, _, cx| w.toggle_sidebar(cx));
    c.advance(Duration::ZERO);
    c.advance(REVEAL);
    assert_eq!(c.width("leading-column"), sidebar_px(DEFAULT_SIDEBAR_WIDTH));
    assert!(c.read(|w, _| w.collapsed));
}

/// The board's task pane shows the agent working its task
/// (`dt-task-pane-session`) only while the board is on screen, and over the
/// Workspace view already showing that agent it doesn't attach it twice.
#[gpui::test]
fn the_task_panes_session_follows_the_screen_and_never_doubles_the_workspaces(cx: &mut TestAppContext) {
    use crate::views::board_state::tests::{issue, read};
    // t-1 isn't in review here: the developer working it is its session.
    let mut c = Column::open_with(cx, |w| w.model.reviews.clear());
    c.h.cx.update(|_, cx| cx.set_reduce_motion(true));
    let agent = |c: &Column| c.ws_sessions.live().into_iter().filter(|n| n.ends_with("/ws/alpha-1")).count();
    c.update(|w, window, cx| {
        w.perform(HostCall::ShowView(View::Issues), window, cx);
        let issues = w.ensure_issues(window, cx);
        // The harness made the board before the fakes were in: host its
        // pane on them now.
        let deps = (w.workspace_deps.as_ref().expect("fake sessions").sessions)();
        let handle = window.window_handle();
        issues.update(cx, |v, cx| {
            v.host_sessions(handle, deps, None, cx);
            v.stop_workers();
            let mut t = issue("t-1", "in-progress");
            t.task.assigned_to = Some("alpha-1".into());
            v.state_for_test().apply_reads(Some(read(vec![t])), Some(None));
            v.open_task("t-1", cx);
        });
        w.update_task_rows(cx);
        w.sync_issues_shown(cx);
    });
    c.h.draw();
    c.h.draw();
    assert!(c.drawn("pane-session"), "the pane shows the agent");
    assert_eq!(agent(&c), 1);

    // The main area moves on: the pane's attach goes, the agent stays up.
    c.update(|w, window, cx| w.perform(HostCall::ShowView(View::Machines), window, cx));
    c.h.draw();
    assert_eq!(agent(&c), 0);

    // The workspace's own screen, then its More: one attach, the screen's.
    c.open_workspace();
    c.advance(SLIDE);
    assert_eq!(agent(&c), 1);
    c.h.click_selector("ws-more");
    c.h.draw();
    assert!(detail_over(&c) && c.drawn("task-pane"));
    assert!(!c.drawn("pane-session"), "the screen under the pane already shows it");
    assert_eq!(agent(&c), 1);
}

// --- a decision on the review -------------------------------------------------

/// Ask for review `t-1`, as a sidebar row, the board's card and the palette's
/// entry all do (`LoadReview`), with the window on `view`, its daemon answering
/// each decision with `answer`.
fn deciding_over(cx: &mut TestAppContext, view: View, answer: Result<(), &'static str>) -> Column {
    let (ws_deps, _, _) = fake_workspace_deps();
    let mut c = Column::open_with(cx, move |w| {
        w.chat_deps = Some(ws_deps.sessions.clone());
        w.client.set_view(view);
        // The last item in review.
        w.model.reviews.retain(|r| r.task_id == "t-1");
        w.review_mutation = Some(Arc::new(move |_| answer.map_err(str::to_string)));
    });
    c.update(|w, window, cx| w.perform(HostCall::LoadReview("t-1".into()), window, cx));
    c.h.draw();
    c.advance(SLIDE);
    assert!(c.drawn("review-panel"), "the review opened");
    c
}

impl Column {
    /// Send `call` to the window without letting its background work (the
    /// daemon's reply) run: the merge is still in flight afterwards.
    fn send_decision(&mut self, call: HostCall) {
        self.on_window(|w, win, cx| w.perform(call, win, cx));
    }

    /// Run `f` on the window without letting background work run.
    fn on_window(&mut self, f: impl FnOnce(&mut ShelbiWindow, &mut Window, &mut Context<ShelbiWindow>)) {
        let window = self.h.window.clone();
        self.h.cx.update(|win, cx| window.update(cx, |w, cx| f(w, win, cx)));
    }

    /// The daemon answers.
    fn reply(&mut self) {
        self.h.cx.run_until_parked();
        self.h.draw();
    }

    /// The sessions closes the window has asked of the daemon, by task.
    fn closes(&self) -> Vec<String> {
        let ops = self.ops.ops.lock().unwrap();
        ops.iter()
            .filter(|(_, op)| matches!(op, shelbi_app::ReviewSessionOp::Close))
            .map(|(task, _)| task.clone())
            .collect()
    }

    fn status(&self) -> Option<String> {
        self.read(|w, _| w.status.as_ref().map(|s| s.to_string()))
    }

    /// The window is off the review: the sidebar in the column, with the keys.
    /// Reads no timers, so a decision in flight stays in flight.
    fn assert_off_review(&mut self) {
        assert!(self.read(|w, _| matches!(w.route, Route::View)), "the review was left");
        // The reverse push starts on this frame and has run by the next.
        self.draw_only();
        self.advance_only(SLIDE);
        assert!(self.h.cx.debug_bounds("sidebar").is_some(), "the sidebar is back");
        assert!(self.h.cx.debug_bounds("column-review").is_none(), "the panel has gone");
        assert_eq!(self.read(|w, _| w.client.focus()), Focus::Sidebar, "keys on the sidebar");
    }

    /// The view the window returned to, which the tap records in place of
    /// building it.
    fn returned_to(&self) -> Option<View> {
        self.h.tap.borrow().iter().rev().find_map(|call| match call {
            HostCall::ShowView(view) => Some(view.clone()),
            _ => None,
        })
    }

    fn decisions(&self) -> ReviewDecisions {
        self.read(|w, _| w.decisions.clone())
    }

    fn panel_status(&self) -> Option<String> {
        let review = self.review();
        review.read_with(&self.h.cx, |r, _| r.panel().and_then(|p| p.status_line().map(str::to_string)))
    }

    fn panel_merging(&self) -> bool {
        let review = self.review();
        review.read_with(&self.h.cx, |r, _| r.panel().is_some_and(|p| p.is_merging()))
    }
}

fn approve() -> HostCall {
    HostCall::ApproveReview("t-1".into())
}

fn reject() -> HostCall {
    HostCall::RejectReview("t-1".into(), "flaky".into())
}

#[gpui::test]
fn a_decision_leaves_the_review_at_once_and_lands_later(cx: &mut TestAppContext) {
    for (view, call, working, done) in [
        (View::Issues, approve(), "Merging t-1…", "Approved t-1."),
        (View::Issues, reject(), "Rejecting t-1…", "Rejected t-1."),
        (View::Machines, approve(), "Merging t-1…", "Approved t-1."),
        (View::Machines, reject(), "Rejecting t-1…", "Rejected t-1."),
    ] {
        let mut c = deciding_over(cx, view.clone(), Ok(()));
        // The daemon hasn't answered: nothing below waits on it.
        c.send_decision(call);
        c.assert_off_review();
        assert_eq!(c.returned_to(), Some(view));
        assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_some(), "a notice carries the progress");
        let notice = c.decisions().notices()[0].clone();
        assert_eq!(notice.decision.working("t-1"), working);
        assert!(c.closes().is_empty(), "the sessions stay until it lands");

        c.reply();
        assert_eq!(c.status().as_deref(), Some(done));
        assert_eq!(c.closes(), ["t-1"], "the review's sessions closed once");
        assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_none(), "the notice gave way to the flash");
        assert!(c.decisions().notices().is_empty());
    }
}

#[gpui::test]
fn approving_the_last_review_returns_to_the_orchestrator_it_was_opened_over(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Session("orch".into()), Ok(()));
    // The real return, not the tap's: it builds the chat.
    c.update(|w, _, _| w.tap = None);
    c.send_decision(approve());
    assert_eq!(c.read(|w, _| w.client.view().clone()), View::Session("orch".into()));
    assert!(c.read(|w, _| matches!(w.route, Route::View)), "left before the reply");
    assert_eq!(c.selected(), orchestrator(), "the selection on the main area's row");
    let chat_shown = c.read(|w, cx| w.chat.as_ref().is_some_and(|chat| chat.read(cx).is_shown()));
    assert!(chat_shown, "the chat is back in the main area");
    c.reply();
    assert_eq!(c.status().as_deref(), Some("Approved t-1."));
}

#[gpui::test]
fn gy_in_the_review_opens_reject_with_the_comments_as_its_reason(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    let review = c.review();
    review.update(&mut c.h.cx, |_, cx| {
        cx.emit(ReviewEvent::RejectWith { id: "t-1".into(), reason: "[thread c1] a.rs:1\njlong: why?".into() });
    });
    c.h.cx.run_until_parked();
    let reason = c.read(|w, _| match &w.overlay {
        Some(ActiveOverlay::RejectReason { task_id, prompt, .. }) if task_id == "t-1" => Some(prompt.reason()),
        _ => None,
    });
    assert_eq!(reason.as_deref(), Some("[thread c1] a.rs:1\njlong: why?"));
    // A later read doesn't write over it, and only this task's prompt takes one.
    c.update(|w, _, _| {
        let overlay = w.overlay.as_mut().unwrap();
        assert!(!overlay.fill_reject_reason("other", "elsewhere"));
        assert!(overlay.fill_reject_reason("t-1", "late"));
    });
    let reason = c.read(|w, _| match &w.overlay {
        Some(ActiveOverlay::RejectReason { prompt, .. }) => prompt.reason(),
        _ => String::new(),
    });
    assert_eq!(reason, "[thread c1] a.rs:1\njlong: why?");
}

#[gpui::test]
fn rejecting_the_last_review_returns_to_the_orchestrator(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Session("orch".into()), Ok(()));
    c.update(|w, _, _| w.tap = None);
    c.send_decision(reject());
    assert_eq!(c.read(|w, _| w.client.view().clone()), View::Session("orch".into()));
    assert!(c.read(|w, _| matches!(w.route, Route::View)), "left before the reply");
    c.reply();
    assert_eq!(c.status().as_deref(), Some("Rejected t-1."));
}

#[gpui::test]
fn the_board_refreshing_mid_merge_changes_nothing_off_the_review(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    c.send_decision(approve());

    // A refresh with the row, then one without it: the window has left already.
    c.on_window(|w, win, cx| w.update_review_row(win, cx));
    c.on_window(|w, win, cx| {
        w.model.reviews.clear();
        w.update_review_row(win, cx)
    });
    c.assert_off_review();
    assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_some(), "the reply says when it landed");

    c.reply();
    assert_eq!(c.status().as_deref(), Some("Approved t-1."));
    assert_eq!(c.closes(), ["t-1"]);
}

#[gpui::test]
fn the_row_says_a_decision_is_running_and_the_review_takes_no_second(cx: &mut TestAppContext) {
    let calls = Arc::new(std::sync::Mutex::new(0));
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    let counted = calls.clone();
    c.update(|w, _, _| {
        w.review_mutation = Some(Arc::new(move |_| {
            *counted.lock().unwrap() += 1;
            Ok(())
        }))
    });
    c.send_decision(approve());
    c.assert_off_review();
    assert!(c.h.cx.debug_bounds("sidebar-review-deciding-t-1").is_some(), "the row says so");

    c.send_decision(reject());
    c.send_decision(approve());
    c.send_decision(HostCall::AskRejectReason("t-1".into()));
    assert!(c.read(|w, _| w.overlay.is_none()), "no reason asked for");
    assert_eq!(c.status().as_deref(), Some("t-1 already has a decision running."));

    c.reply();
    assert_eq!(*calls.lock().unwrap(), 1, "one decision reached the daemon");
    assert_eq!(c.closes(), ["t-1"]);
    assert!(c.h.cx.debug_bounds("sidebar-review-deciding-t-1").is_none(), "the note has gone");
}

#[gpui::test]
fn a_failure_landing_later_offers_to_reopen_the_review(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Err("the merge was refused"));
    c.send_decision(approve());
    c.assert_off_review();

    c.reply();
    c.assert_off_review();
    assert_eq!(c.closes(), Vec::<String>::new(), "the sessions stay: it's still loaded on its slot");
    let notice = c.decisions().notices()[0].clone();
    assert_eq!(notice.failure.as_deref(), Some("the merge was refused"));
    assert_eq!(
        notice.decision.failed_on(&notice.task, "the merge was refused"),
        "Couldn't approve t-1: the merge was refused"
    );
    assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_some(), "in the banner");
    assert!(c.h.cx.debug_bounds("sidebar-review-deciding-t-1").is_none(), "the row is itself again");

    // A move elsewhere keeps it: it isn't the plain banner.
    c.on_window(|w, win, cx| w.perform(HostCall::ShowView(View::Machines), win, cx));
    c.draw_only();
    assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_some());

    // The action: the review as it was, its panel saying why, ready to try again.
    c.h.click_selector("reopen-review-t-1");
    c.h.cx.run_until_parked();
    c.advance(SLIDE);
    assert!(c.read(|w, _| matches!(w.route, Route::Review)));
    assert!(c.drawn("review-panel"));
    assert!(!c.panel_merging(), "the panel takes input");
    assert_eq!(c.panel_status().as_deref(), Some("approve failed: the merge was refused"));
    assert!(c.decisions().notices().is_empty(), "the panel has it now");
    assert!(c.drawn("review-approve"), "the decision can be made again");
}

#[gpui::test]
fn a_failure_can_be_dismissed(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Err("the merge was refused"));
    c.send_decision(reject());
    c.reply();
    assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_some());
    c.h.click_selector("dismiss-decision-t-1");
    c.h.cx.run_until_parked();
    c.draw_only();
    assert!(c.h.cx.debug_bounds("decision-notice-t-1").is_none());
    assert!(c.decisions().notices().is_empty());
    assert!(c.read(|w, _| matches!(w.route, Route::View)), "it doesn't open anything");
}

#[gpui::test]
fn a_decision_from_elsewhere_leaves_the_view_alone(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Err("the merge was refused"));
    c.back();
    let taps = c.h.tap.borrow().len();
    // The palette's entry: the window isn't on the review.
    c.send_decision(approve());
    assert_eq!(c.h.tap.borrow().len(), taps, "nothing to leave");
    c.reply();
    assert!(c.h.cx.debug_bounds("reopen-review-t-1").is_some());
}

/// The review opened again while the decision runs.
fn reopened_mid_merge(c: &mut Column) {
    c.send_decision(approve());
    c.on_window(|w, win, cx| w.perform(HostCall::LoadReview("t-1".into()), win, cx));
    // Its resolve runs here; the decision's reply is what it must not run.
    c.on_window(|w, win, cx| w.open_review(info(), win, cx));
    c.advance_only(SLIDE);
    c.draw_only();
    assert!(c.h.cx.debug_bounds("review-panel").is_some(), "open again");
    assert!(c.panel_merging(), "it can't be decided on twice");
    assert!(c.h.cx.debug_bounds("review-merging").is_some());
}

#[gpui::test]
fn a_review_opened_mid_merge_never_holds_the_window(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    reopened_mid_merge(&mut c);
    let review = c.review();
    c.h.cx.update(|window, cx| review.update(cx, |r, cx| r.focus_panel(window, cx)));
    c.h.cx.simulate_keystrokes("escape");
    assert!(c.read(|w, _| matches!(w.route, Route::View)), "Esc leaves as ever");
    assert_eq!(c.returned_to(), Some(View::Issues));
}

#[gpui::test]
fn the_board_refreshing_on_a_review_opened_mid_merge_lands_the_decision(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    reopened_mid_merge(&mut c);

    // Refreshes that still have the row change nothing.
    c.on_window(|w, win, cx| w.update_review_row(win, cx));
    assert!(c.read(|w, _| matches!(w.route, Route::Review)), "still merging");

    // The move has landed: the window doesn't wait for the reply.
    c.on_window(|w, win, cx| {
        w.model.reviews.clear();
        w.update_review_row(win, cx)
    });
    assert!(c.read(|w, _| matches!(w.route, Route::View)), "left at once");
    assert_eq!(c.status().as_deref(), Some("Approved t-1."));
    assert!(c.decisions().notices().is_empty());

    // The reply then only confirms it.
    c.reply();
    assert_eq!(c.closes(), ["t-1"], "closed once");
    assert_eq!(c.returned_to(), Some(View::Issues));
}

#[gpui::test]
fn the_review_closing_on_a_review_opened_mid_merge_lands_the_decision(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    reopened_mid_merge(&mut c);
    c.on_window(|w, win, cx| {
        let event = shelbi_state::LayoutEvent::ReviewClosed {
            workspace: "review-1".into(),
            task: "t-1".into(),
        };
        w.apply_layout(event, win, cx)
    });
    assert!(c.read(|w, _| matches!(w.route, Route::View)), "left at once");
    assert_eq!(c.status().as_deref(), Some("Approved t-1."));
    c.reply();
    assert_eq!(c.closes(), ["t-1"], "closed once");
}

#[gpui::test]
fn a_failure_on_a_review_opened_mid_merge_stays_on_it(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Err("the merge was refused"));
    reopened_mid_merge(&mut c);
    c.reply();
    assert!(c.read(|w, _| matches!(w.route, Route::Review)), "still on the review");
    assert!(!c.panel_merging(), "the panel takes input again");
    assert_eq!(c.panel_status().as_deref(), Some("approve failed: the merge was refused"));
    assert!(c.decisions().notices().is_empty(), "the panel says so; no banner too");
    assert!(c.closes().is_empty(), "the sessions stay");
}

#[gpui::test]
fn a_decision_landing_with_another_review_up_closes_its_own_sessions(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Issues, Ok(()));
    c.send_decision(approve());
    let mut other = info();
    other.task_id = "t-2".into();
    c.on_window(|w, win, cx| w.open_review(other, win, cx));
    c.reply();
    assert!(c.read(|w, _| matches!(w.route, Route::Review)), "the other review stays up");
    assert_eq!(c.read(|w, cx| w.shown_review(cx)).as_deref(), Some("t-2"));
    assert_eq!(c.closes(), ["t-1"], "not the one on the screen");
    assert_eq!(c.status().as_deref(), Some("Approved t-1."));
}

#[gpui::test]
fn a_decision_never_returns_to_the_reviews_own_slot(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Session("review-1".into()), Ok(()));
    c.send_decision(approve());
    c.assert_off_review();
    assert_eq!(c.returned_to(), Some(View::Session("orch".into())));
}

#[gpui::test]
fn back_returns_to_the_slots_page_a_review_was_opened_over(cx: &mut TestAppContext) {
    let mut c = deciding_over(cx, View::Session("review-1".into()), Ok(()));
    c.back();
    assert_eq!(c.returned_to(), Some(View::Session("review-1".into())), "the review is still loaded");
}

/// A review up with Shelbi's own diff holding the keyboard.
fn builtin_diff_up(c: &mut Column) -> Entity<crate::views::diff_view::DiffView> {
    let mut builtin = info();
    builtin.apps.builtin_diff = true;
    c.update(|w, window, cx| {
        w.select_row(SidebarTarget::Review("t-1".into()), cx);
        w.open_review(builtin, window, cx);
    });
    c.advance(Duration::from_secs(1));
    let review = c.review();
    c.h.cx.update(|_, cx| review.update(cx, |r, cx| r.apply(PanelEffect::ShowDiff, cx)));
    c.h.cx.run_until_parked();
    c.h.draw();
    let diff = c.h.cx.update(|_, cx| review.read(cx).diff_on_show().cloned()).expect("the diff is up");
    let start = Instant::now();
    loop {
        c.h.cx.update(|_, cx| diff.update(cx, |d, cx| d.poll_now(cx)));
        c.h.cx.run_until_parked();
        if !c.h.cx.update(|_, cx| diff.read(cx).state().loading()) || start.elapsed() > Duration::from_secs(10) {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    c.h.draw();
    diff
}

#[gpui::test]
fn a_view_chord_and_back_keeps_the_keyboard_in_the_diff(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let diff = builtin_diff_up(&mut c);

    // To the reviewer's terminal with a chord, and back with another.
    c.h.cx.simulate_keystrokes("ctrl-1");
    c.advance(Duration::from_secs(1));
    c.h.cx.run_until_parked();
    c.h.draw();
    let review = c.review();
    let on_chat = c.h.cx.update(|_, cx| review.read(cx).diff_on_show().is_none());
    assert!(on_chat);
    let in_reviewer = c.h.cx.update(|window, cx| review.read(cx).contains_focus(window, cx));
    assert!(in_reviewer, "the keyboard follows to the reviewer's pane");
    c.h.cx.simulate_keystrokes("ctrl-2");
    c.advance(Duration::from_secs(1));
    c.h.cx.run_until_parked();
    c.h.draw();
    let in_diff = c.h.cx.update(|window, cx| diff.read(cx).contains_focus(window));
    assert!(in_diff, "the keyboard came back to the diff");
    c.h.cx.simulate_keystrokes("?");
    c.h.cx.run_until_parked();
    assert_eq!(c.h.cx.update(|_, cx| diff.read(cx).state().mode().label()), "HELP");
}

#[gpui::test]
fn the_diffs_keys_beat_the_windows_chords_and_the_rest_get_through(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    let diff = builtin_diff_up(&mut c);
    let in_diff = c.h.cx.update(|window, cx| diff.read(cx).contains_focus(window));
    assert!(in_diff, "asking for the diff hands it the keyboard");

    // Ctrl-p is the diff's file palette here, not the window's command palette.
    c.h.cx.simulate_keystrokes("ctrl-p");
    c.h.cx.run_until_parked();
    assert_eq!(c.h.cx.update(|_, cx| diff.read(cx).state().mode().label()), "PALETTE");
    assert!(c.read(|w, _| w.overlay.is_none()));
    c.h.cx.simulate_keystrokes("escape");
    c.h.cx.run_until_parked();
    assert_eq!(c.h.cx.update(|_, cx| diff.read(cx).state().mode().label()), "NORMAL");

    // `?` is its help.
    c.h.cx.simulate_keystrokes("?");
    c.h.cx.run_until_parked();
    assert_eq!(c.h.cx.update(|_, cx| diff.read(cx).state().mode().label()), "HELP");
    c.h.cx.simulate_keystrokes("q");
    c.h.cx.run_until_parked();

    // A rebound key follows `keys.yaml` at once.
    let km = shelbi_state::keymap::validate_keymaps_yaml("defaults:\n  diff:\n    cursor_down: e\n", None).0;
    c.update(|w, _, cx| w.adopt_keymaps(km, cx));
    let top = c.h.cx.update(|_, cx| diff.read(cx).state().cursor());
    c.h.cx.simulate_keystrokes("e");
    c.h.cx.run_until_parked();
    assert_eq!(c.h.cx.update(|_, cx| diff.read(cx).state().cursor()), top + 1);

    // The reserved palette chord still reaches the window from inside the diff.
    c.h.cx.simulate_keystrokes("ctrl-space");
    c.h.cx.run_until_parked();
    assert!(c.read(|w, _| w.overlay.is_some()), "the window's palette");
}

// --- a click lands the keys on the content (`nav-click-focuses-content`) ---

impl Column {
    /// Let commands run for real (the harness's tap only records them), so a
    /// click opens what it names.
    fn live(&mut self) {
        self.h.window.update(&mut self.h.cx, |w, _| w.tap = None);
    }

    /// Let the push settle and the content attach.
    fn settle(&mut self) {
        for _ in 0..3 {
            self.advance(SLIDE);
        }
    }

    fn review_content_has_keys(&mut self) -> bool {
        let review = self.review();
        self.h.cx.update(|window, cx| review.read(cx).content_has_keys(window, cx))
    }

    fn workspace_content_has_keys(&mut self) -> bool {
        let ws = self.workspace();
        self.h.cx.update(|window, cx| ws.read(cx).content_has_keys(window, cx))
    }

    fn review_panel_focused(&mut self) -> bool {
        let review = self.review();
        self.h.cx.update(|window, cx| review.read(cx).focus_handle(cx).is_focused(window))
    }
}

#[gpui::test]
fn a_click_on_a_review_row_hands_the_keys_to_its_terminal(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.live();
    c.h.click_selector("sidebar-review-t-1");
    c.settle();
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Main);
    assert!(c.review_content_has_keys(), "the review's terminal has the keys, not its panel");
    assert!(!c.review_panel_focused());
}

#[gpui::test]
fn enter_on_a_review_row_still_leaves_the_keys_on_the_panel(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.live();
    c.update(|w, window, cx| {
        w.select_row(SidebarTarget::Review("t-1".into()), cx);
        w.activate_selection(window, cx);
    });
    c.settle();
    assert!(c.review_panel_focused(), "the keyboard moves on through the panel");
    assert!(!c.review_content_has_keys());
}

#[gpui::test]
fn a_click_on_a_workspace_row_hands_the_keys_to_its_terminal(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.live();
    c.h.click_selector("sidebar-ws-alpha-1");
    c.settle();
    assert_eq!(c.read(|w, _| w.client.focus()), Focus::Main);
    assert!(c.workspace_content_has_keys(), "the agent's terminal has the keys, not the panel");
    assert!(!c.workspace_panel_focused());
}

#[gpui::test]
fn a_click_on_a_review_panel_view_hands_the_keys_to_it_even_the_one_shown(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.live();
    c.open_review();
    c.settle();
    assert!(c.review_panel_focused(), "opened from the keyboard: the panel has the keys");
    // The view already shown: its terminal takes the keys.
    c.h.click_selector("review-switch-chat");
    c.settle();
    assert!(c.review_content_has_keys(), "Chat, already shown, has the keys");
    // Another view: its terminal, once attached.
    c.h.click_selector("review-switch-editor");
    c.settle();
    assert!(c.review_content_has_keys(), "the editor's terminal has the keys");
    assert!(!c.review_panel_focused());
}

#[gpui::test]
fn a_click_on_a_workspace_panel_view_hands_the_keys_to_it(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.live();
    c.open_workspace();
    c.settle();
    assert!(c.workspace_panel_focused());
    c.h.click_selector("ws-switch-agent");
    c.settle();
    assert!(c.workspace_content_has_keys(), "the agent's terminal has the keys");
    c.h.click_selector("ws-switch-editor");
    c.settle();
    assert!(c.workspace_content_has_keys(), "the editor's terminal has the keys");
}

#[gpui::test]
fn a_dialog_opened_before_the_content_attaches_keeps_the_keys(cx: &mut TestAppContext) {
    let mut c = Column::open(cx);
    c.live();
    c.h.click_selector("sidebar-ws-alpha-1");
    // Mid-push, before the terminal attaches: the palette opens over it.
    c.update(|w, window, cx| w.open_palette(window, cx));
    c.settle();
    assert!(c.read(|w, _| w.overlay.is_some()), "the palette is still up");
    let window_focused = c.h.cx.update(|window, cx| c.h.window.read(cx).focus_handle.is_focused(window));
    assert!(window_focused, "the palette's keys stay with it");
    assert!(!c.workspace_content_has_keys(), "the attach didn't take them");
}
