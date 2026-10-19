//! The Issues board's scroll axes: the board row scrolls across its columns
//! (x only), each column scrolls its cards (y only), and one gesture moves one
//! of them, never both.
//!
//! gpui hands every scroll event to every scroll container under the pointer
//! (a column and the board both see it), and by default a container that
//! scrolls on one axis takes the *other* axis's delta when its own is zero: a
//! vertical wheel over a column the column can't take (at its end, or too
//! short to scroll) slid the whole board sideways, and a sideways swipe
//! scrolled the column. [`board_row`] sets `restrict_scroll_to_axis`, so the
//! board takes only x; with it, gpui also locks each trackpad gesture
//! (precise deltas) to its dominant axis, tracked per [`ScrollHandle`], so a
//! vertical swipe with a little sideways drift doesn't move the board.
//!
//! A column is a gpui `list` (`board_list.rs`, `dt-board-perf`: only the
//! cards in view are built), which only ever takes y but locks no gesture, so
//! [`with_board_wheel`] does it for the columns: a trackpad gesture locked
//! sideways scrolls the board and never reaches a column, and a vertical
//! drift in it moves nothing. [`column_list`] steps a wheel mouse's lines at
//! the window's line height, as the board's scroll containers do (a list
//! steps 20px a line).
//!
//! Shift+wheel is the mouse's horizontal scroll. macOS already reports it as
//! an x delta for a wheel mouse; where the delta still arrives vertical (a
//! trackpad with shift held, or a platform that doesn't convert),
//! [`with_board_wheel`] turns it into board-horizontal scroll before any
//! column sees it.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    canvas, div, point, prelude::*, AnyElement, DispatchPhase, Div, HitboxBehavior, List,
    ListSizingBehavior, ListState, OngoingScroll, Pixels, ScrollHandle, ScrollWheelEvent, Stateful,
};

/// The board's horizontal scroll: its handle, and the trackpad gesture the
/// board's wheel layer is tracking (kept across frames).
#[derive(Clone, Default)]
pub(super) struct BoardScroll {
    pub handle: ScrollHandle,
    gesture: Rc<Cell<OngoingScroll>>,
}

/// The board row: scrolls on x only, tracked by `scroll`.
pub(super) fn board_row(row: Stateful<Div>, scroll: &BoardScroll) -> Stateful<Div> {
    row.overflow_x_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(&scroll.handle)
}

/// A column's card list over `state`, filling a flex column (the returned
/// wrapper takes the list's place in the layout). It scrolls on y only.
pub(super) fn column_list(list: List, state: &ListState, board: &BoardScroll) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(list.with_sizing_behavior(ListSizingBehavior::Auto).flex_1().min_h_0())
        .child(column_wheel(state.clone(), Some(board.handle.clone())))
}

/// A list over `state` filling a flex column on its own (the Activity feed):
/// [`column_list`] with no board beside it, a wheel mouse's lines stepped at
/// the window's line height as a scroll container steps them.
pub(super) fn wheel_list(list: List, state: &ListState) -> Div {
    div()
        .relative()
        .flex()
        .flex_col()
        .flex_1()
        .min_h_0()
        .child(list.with_sizing_behavior(ListSizingBehavior::Auto).flex_1().min_h_0())
        .child(column_wheel(state.clone(), None))
}

/// An invisible layer over a column's list that takes a wheel mouse's
/// vertical lines (in the capture phase, before the list's own handler) and
/// scrolls the list by them at the window's line height, the step the
/// board's scroll containers take. Trackpad deltas and shift+wheel pass by.
fn column_wheel(list: ListState, board: Option<ScrollHandle>) -> AnyElement {
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            let view = window.current_view();
            window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture
                    || !hitbox.should_handle_scroll(window)
                    || e.delta.precise()
                    || e.modifiers.shift
                {
                    return;
                }
                let delta = e.delta.pixel_delta(window.line_height());
                if delta.y == Pixels::ZERO {
                    // A horizontal wheel: the board's, as before.
                    return;
                }
                cx.stop_propagation();
                list.scroll_by(-delta.y);
                // The board took a wheel's x beside a column's y.
                if let Some(board) = board.as_ref().filter(|_| delta.x != Pixels::ZERO) {
                    let offset = board.offset();
                    board.set_offset(point(clamp_x(offset.x + delta.x, board.max_offset().x), offset.y));
                }
                cx.notify(view);
            });
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// Wrap `board` (a [`board_row`] tracked by `scroll`) in the board's wheel
/// layer: a shift+wheel that arrives vertical scrolls it horizontally, and a
/// trackpad gesture locked sideways scrolls only it. The wrapper takes the
/// board's place in the layout (`flex_1`, shrinkable on both axes).
pub(super) fn with_board_wheel(board: impl IntoElement, scroll: &BoardScroll) -> Div {
    div()
        .relative()
        .flex()
        .flex_1()
        .min_h_0()
        .min_w_0()
        .child(board)
        .child(BoardWheel(scroll.clone()).into_element())
}

/// An invisible layer over the board that catches wheel events in the
/// capture phase (before the columns' and the board's own bubble-phase
/// scroll handlers): shift+vertical wheel scrolls the board on x, and so
/// does each step of a trackpad gesture locked to x, which no column sees.
struct BoardWheel(BoardScroll);

impl BoardWheel {
    fn into_element(self) -> AnyElement {
        let BoardScroll { handle, gesture } = self.0;
        canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |_, hitbox, window, _| {
                let view = window.current_view();
                window.on_mouse_event(move |e: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
                        return;
                    }
                    let delta = e.delta.pixel_delta(window.line_height());
                    let dx = match shift_wheel_dx(e.modifiers.shift, delta) {
                        Some(dx) => dx,
                        None if e.delta.precise() => {
                            // Every step of the gesture goes through the
                            // lock, the ones let by too, so it locks where
                            // the board's own does.
                            let mut locked = delta;
                            let mut g = gesture.get();
                            g.filter(&mut locked, e.touch_phase);
                            gesture.set(g);
                            match sideways_dx(locked) {
                                Some(dx) => dx,
                                None => return,
                            }
                        }
                        None => return,
                    };
                    cx.stop_propagation();
                    let offset = handle.offset();
                    let x = clamp_x(offset.x + dx, handle.max_offset().x);
                    if x != offset.x {
                        handle.set_offset(point(x, offset.y));
                        cx.notify(view);
                    }
                });
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .into_any_element()
    }
}

/// The board's x delta for a shift+wheel event whose delta is vertical (its
/// dominant axis is y), or `None` when the event isn't one: no shift, or the
/// platform already turned it horizontal.
fn shift_wheel_dx(shift: bool, delta: gpui::Point<Pixels>) -> Option<Pixels> {
    (shift && delta.y.abs() > delta.x.abs()).then_some(delta.y)
}

/// The board's x delta for a trackpad step after the gesture lock, when the
/// lock left it only an x (the gesture is sideways); `None` otherwise.
fn sideways_dx(locked: gpui::Point<Pixels>) -> Option<Pixels> {
    (locked.y == Pixels::ZERO && locked.x != Pixels::ZERO).then_some(locked.x)
}

/// A board x offset kept within its scroll range (`-max..=0`; gpui offsets
/// grow negative as content scrolls toward its end).
fn clamp_x(x: Pixels, max: Pixels) -> Pixels {
    x.min(Pixels::ZERO).max(-max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        px, size, Bounds, Context, Modifiers, Point, ScrollDelta, TestAppContext, TouchPhase,
        VisualTestContext, Window,
    };

    const COL_W: f32 = 200.0;
    const ROW_H: f32 = 40.0;
    /// Column 0 is long (scrolls), column 1 is short (fits), the rest are long.
    const ROWS: [usize; 5] = [30, 2, 30, 30, 30];

    /// A board laid out the way `IssuesView::render_board` lays it out: a row
    /// of fixed-width columns, each a header over a list of cards.
    struct Board {
        board: BoardScroll,
        columns: Vec<ListState>,
    }

    impl Render for Board {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let columns = ROWS.iter().enumerate().map(|(ci, _)| {
                let cards = gpui::list(self.columns[ci].clone(), |_, _, _| {
                    div().flex_none().h(px(ROW_H)).into_any_element()
                })
                // The cards' bottom room once scrolled to the end.
                .pb_3();
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .flex_none()
                    .w(px(COL_W))
                    .min_h_0()
                    .child(div().flex_none().h(px(24.0)))
                    .child(
                        column_list(cards, &self.columns[ci], &self.board)
                            // The focus glow's room past the column.
                            .mx(px(-6.0))
                            .mt(px(-6.0)),
                    )
            });
            let board = board_row(
                div()
                    .id("board-columns")
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .gap_3()
                    .pt_3()
                    .px_3(),
                &self.board,
            )
            .children(columns);
            div()
                .flex()
                .flex_col()
                .size_full()
                .child(with_board_wheel(board, &self.board))
        }
    }

    struct Harness {
        view: gpui::Entity<Board>,
        cx: VisualTestContext,
    }

    impl Harness {
        fn open(cx: &mut TestAppContext) -> Harness {
            // 600x400: about three columns wide, so the board scrolls on x.
            let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(600.0), px(400.0)));
            let window = cx.update(|cx| {
                cx.open_window(
                    gpui::WindowOptions {
                        window_bounds: Some(gpui::WindowBounds::Windowed(bounds)),
                        ..Default::default()
                    },
                    |_, cx| {
                        cx.new(|_| Board {
                            board: BoardScroll::default(),
                            // Measured up front, so the tests know each
                            // column's scroll range from the first frame.
                            columns: ROWS
                                .iter()
                                .map(|&n| ListState::new(n, gpui::ListAlignment::Top, px(200.0)).measure_all())
                                .collect(),
                        })
                    },
                )
                .unwrap()
            });
            let mut cx = VisualTestContext::from_window(window.into(), cx);
            let view = window.root(&mut cx).unwrap();
            let mut h = Harness { view, cx };
            h.draw();
            h
        }

        fn draw(&mut self) {
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
            self.cx.run_until_parked();
        }

        fn board(&mut self) -> Point<Pixels> {
            self.view.read_with(&self.cx, |v, _| v.board.handle.offset())
        }

        fn column(&mut self, ci: usize) -> Point<Pixels> {
            self.view.read_with(&self.cx, |v, _| v.columns[ci].scroll_px_offset_for_scrollbar())
        }

        fn max_column(&mut self, ci: usize) -> Point<Pixels> {
            self.view.read_with(&self.cx, |v, _| v.columns[ci].max_offset_for_scrollbar())
        }

        /// A point over column `ci`'s cards (columns 0-2 are on screen at x 0).
        fn over(&self, ci: usize) -> Point<Pixels> {
            point(px(12.0 + ci as f32 * (COL_W + 12.0) + COL_W / 2.0), px(200.0))
        }

        fn scroll(&mut self, at: Point<Pixels>, delta: ScrollDelta, mods: Modifiers, phase: TouchPhase) {
            self.cx.simulate_mouse_move(at, None, mods);
            self.cx.simulate_event(ScrollWheelEvent {
                position: at,
                delta,
                modifiers: mods,
                touch_phase: phase,
            });
            self.draw();
        }

        /// A trackpad (precise) delta in one gesture phase.
        fn swipe(&mut self, at: Point<Pixels>, x: f32, y: f32, phase: TouchPhase) {
            let d = ScrollDelta::Pixels(point(px(x), px(y)));
            self.scroll(at, d, Modifiers::default(), phase);
        }

        /// A wheel mouse (line) delta.
        fn wheel(&mut self, at: Point<Pixels>, x: f32, y: f32, mods: Modifiers) {
            self.scroll(at, ScrollDelta::Lines(point(x, y)), mods, TouchPhase::Moved);
        }
    }

    fn shift() -> Modifiers {
        Modifiers {
            shift: true,
            ..Default::default()
        }
    }

    #[gpui::test]
    fn the_fixture_scrolls_on_both_axes(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let max = h.view.read_with(&h.cx, |v, _| v.board.handle.max_offset());
        assert!(max.x > px(0.0), "the board overflows on x: {max:?}");
        assert!(h.max_column(0).y > px(0.0), "column 0 overflows");
        assert_eq!(h.max_column(1).y, px(0.0), "column 1 fits");
    }

    #[gpui::test]
    fn vertical_wheel_over_a_scrollable_column_scrolls_only_it(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(0);
        h.wheel(at, 0.0, -3.0, Modifiers::default());
        assert!(h.column(0).y < px(0.0), "column 0 scrolled down");
        assert_eq!(h.board(), Point::default());
        assert_eq!(h.column(2), Point::default());

        h.swipe(at, 0.0, -30.0, TouchPhase::Started);
        h.swipe(at, 0.0, -30.0, TouchPhase::Moved);
        h.swipe(at, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(h.board(), Point::default());
    }

    #[gpui::test]
    fn vertical_scroll_over_a_short_column_moves_nothing(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(1);
        h.wheel(at, 0.0, -5.0, Modifiers::default());
        h.wheel(at, 0.0, 5.0, Modifiers::default());
        h.swipe(at, 0.0, -40.0, TouchPhase::Started);
        h.swipe(at, 0.0, -40.0, TouchPhase::Moved);
        assert_eq!(h.board(), Point::default());
        assert_eq!(h.column(1), Point::default());
    }

    #[gpui::test]
    fn vertical_scroll_at_a_columns_end_moves_nothing(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(0);
        // At the top, scrolling further up.
        h.wheel(at, 0.0, 5.0, Modifiers::default());
        assert_eq!(h.column(0), Point::default());
        assert_eq!(h.board(), Point::default());
        // Down to the bottom and past it (the list's range is its cards'
        // and the bottom room under them).
        let max = h.max_column(0).y;
        for _ in 0..20 {
            h.wheel(at, 0.0, -10.0, Modifiers::default());
        }
        let bottom = h.column(0).y;
        assert!(bottom <= -max, "column 0 sits at its bottom: {bottom:?} vs {max:?}");
        h.wheel(at, 0.0, -10.0, Modifiers::default());
        assert_eq!(h.column(0).y, bottom, "no further");
        h.swipe(at, 0.0, -40.0, TouchPhase::Started);
        h.swipe(at, 0.0, -40.0, TouchPhase::Moved);
        assert_eq!(h.column(0).y, bottom);
        assert_eq!(h.board(), Point::default());
    }

    #[gpui::test]
    fn a_wheel_line_steps_a_column_by_the_line_height(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(0);
        let line = h.cx.update(|window, _| window.line_height());
        h.wheel(at, 0.0, -1.0, Modifiers::default());
        assert_eq!(h.column(0).y, -line);
        h.wheel(at, 0.0, 1.0, Modifiers::default());
        assert_eq!(h.column(0).y, px(0.0));
    }

    #[gpui::test]
    fn horizontal_swipe_scrolls_only_the_board(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(0);
        h.swipe(at, -30.0, 0.0, TouchPhase::Started);
        h.swipe(at, -30.0, 0.0, TouchPhase::Moved);
        h.swipe(at, 0.0, 0.0, TouchPhase::Ended);
        assert_eq!(h.board().x, px(-60.0));
        assert_eq!(h.board().y, px(0.0));
        assert_eq!(h.column(0), Point::default(), "no column scrolled");
        // A wheel mouse's horizontal wheel (or tilt) too.
        h.wheel(at, -1.0, 0.0, Modifiers::default());
        assert!(h.board().x < px(-60.0));
        assert_eq!(h.column(0), Point::default());
    }

    #[gpui::test]
    fn shift_wheel_scrolls_the_board_not_the_column(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(0);
        // As a wheel delta arrives unconverted: vertical, shift held.
        h.wheel(at, 0.0, -1.0, shift());
        let after_one = h.board().x;
        assert!(after_one < px(0.0), "shift+wheel down scrolls the board on");
        assert_eq!(h.column(0), Point::default(), "no column scrolled");
        // As macOS converts it: horizontal, shift still held.
        h.wheel(at, -1.0, 0.0, shift());
        assert!(h.board().x < after_one);
        assert_eq!(h.column(0), Point::default());
        // Back to the start, and no further.
        for _ in 0..50 {
            h.wheel(at, 0.0, 3.0, shift());
        }
        assert_eq!(h.board(), Point::default());
        assert_eq!(h.column(0), Point::default());
    }

    #[gpui::test]
    fn sideways_drift_in_a_vertical_swipe_never_moves_the_board(cx: &mut TestAppContext) {
        for at_end in [false, true] {
            let mut h = Harness::open(cx);
            let at = h.over(0);
            if at_end {
                for _ in 0..20 {
                    h.wheel(at, 0.0, -10.0, Modifiers::default());
                }
            }
            let before = h.column(0);
            h.swipe(at, -1.0, -12.0, TouchPhase::Started);
            for x in [-2.0, -3.0, -4.0, -2.0, -5.0, -1.0] {
                h.swipe(at, x, -14.0, TouchPhase::Moved);
            }
            h.swipe(at, 0.0, 0.0, TouchPhase::Ended);
            assert_eq!(h.board(), Point::default(), "at_end={at_end}");
            if !at_end {
                assert!(h.column(0).y < before.y, "the column took the vertical part");
            }
            assert_eq!(h.column(0).x, px(0.0));
        }
    }

    #[gpui::test]
    fn vertical_drift_in_a_horizontal_swipe_never_scrolls_a_column(cx: &mut TestAppContext) {
        let mut h = Harness::open(cx);
        let at = h.over(0);
        h.swipe(at, -12.0, -1.0, TouchPhase::Started);
        for y in [-2.0, -3.0, -4.0, -2.0] {
            h.swipe(at, -14.0, y, TouchPhase::Moved);
        }
        assert!(h.board().x < px(0.0));
        assert_eq!(h.column(0), Point::default());
    }

    #[test]
    fn shift_wheel_dx_only_turns_a_vertical_shift_delta() {
        let d = |x: f32, y: f32| point(px(x), px(y));
        assert_eq!(shift_wheel_dx(true, d(0.0, -20.0)), Some(px(-20.0)));
        assert_eq!(shift_wheel_dx(true, d(-1.0, -20.0)), Some(px(-20.0)));
        assert_eq!(shift_wheel_dx(true, d(-20.0, 0.0)), None, "already horizontal");
        assert_eq!(shift_wheel_dx(false, d(0.0, -20.0)), None, "no shift");
        assert_eq!(clamp_x(px(10.0), px(100.0)), px(0.0));
        assert_eq!(clamp_x(px(-150.0), px(100.0)), px(-100.0));
        assert_eq!(clamp_x(px(-40.0), px(100.0)), px(-40.0));
        assert_eq!(sideways_dx(d(-12.0, 0.0)), Some(px(-12.0)));
        assert_eq!(sideways_dx(d(0.0, -12.0)), None, "locked vertical");
        assert_eq!(sideways_dx(d(-12.0, -3.0)), None, "unlocked");
        assert_eq!(sideways_dx(d(0.0, 0.0)), None);
    }
}
