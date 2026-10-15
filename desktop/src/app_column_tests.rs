//! The leading column with a review open (`dt-review-replaces-sidebar`): the
//! review panel takes the sidebar's column at the sidebar's width, the push
//! between them runs both ways (and turns around mid-way), and leaving gives
//! the sidebar back as it was: selection, scroll, collapsed state and keys.
//!
//! The window is the smoke test's headless one ([`Harness`]) with fake review
//! sessions and daemon ops, an injected clock for the transition and an
//! injected Reduce Motion setting.

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
use crate::views::review::tests::{fake_deps, info, row};

struct Column {
    h: Harness,
    now: Rc<Cell<Instant>>,
    reduce_motion: Rc<Cell<bool>>,
}

impl Column {
    fn open(cx: &mut TestAppContext) -> Column {
        Self::open_with(cx, |_| {})
    }

    fn open_with(cx: &mut TestAppContext, setup: impl FnOnce(&mut ShelbiWindow)) -> Column {
        let mut h = Harness::open(cx);
        let now = Rc::new(Cell::new(Instant::now()));
        let reduce_motion = Rc::new(Cell::new(false));
        let (deps, _sessions, ops) = fake_deps();
        let (n, r) = (now.clone(), reduce_motion.clone());
        h.window.update(&mut h.cx, |w, _| {
            w.clock = Rc::new(move || n.get());
            w.reduce_motion = Rc::new(move || r.get());
            w.review_ops = ops;
            w.review_deps = Some(deps);
            w.model.reviews.push(row(ReviewState::Serving));
            setup(w);
        });
        let mut c = Column {
            h,
            now,
            reduce_motion,
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

    fn ordinal_of(&self, target: &SidebarTarget) -> usize {
        self.read(|w, _| {
            (0..w.selectable_count())
                .find(|&i| w.target_at(i).as_ref() == Some(target))
                .expect("the sidebar lists it")
        })
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

    // The first frame: both, the sidebar in place, the panel just right of
    // the column (clipped away), the session already beside the column.
    assert!(c.drawn("column-sidebar") && c.drawn("column-review"));
    assert_eq!(c.width("leading-column"), column_w, "the column keeps its width");
    let column_x = c.left("leading-column");
    assert_eq!(c.left("column-sidebar"), column_x);
    assert_eq!(c.left("column-review"), column_x + column_w);
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
