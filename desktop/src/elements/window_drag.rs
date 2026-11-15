//! Window drag regions: the empty parts of the main window's titlebar row
//! that move the window.
//!
//! The project window draws its own titlebar (the tab bar, or an editor's
//! titlebar row) under a transparent native one, so it opens with gpui's
//! `app_owns_titlebar_drag`: AppKit no longer moves the window from a press
//! anywhere in its titlebar strip, which would take a press on a tab before
//! the tab's own drag could start. Moving the window is the app's job then,
//! and only where the row is empty: each such spot is a [`region`].
//!
//! A region works the way gpui-component's `TitleBar` does: a left press
//! arms it, the first move while the button is still down hands the drag to
//! the system (`Window::start_window_move`, which AppKit runs from the
//! current event), and a double click is the system's titlebar double-click
//! action (zoom, minimize or nothing, per the user's setting). A press and
//! release without moving does nothing.
//!
//! Regions sit beside interactive elements, never under them: gpui hovers
//! every hitbox under the pointer, so a region under a tab would arm on the
//! tab's press too.

use std::cell::Cell;

use gpui::{canvas, div, prelude::*, DispatchPhase, Div, ElementId, MouseButton, MouseMoveEvent, Stateful, Window};

thread_local! {
    /// A press in a region is waiting for its first move. There is one
    /// pointer, so one flag serves every region in every window.
    static ARMED: Cell<bool> = const { Cell::new(false) };
}

/// An empty spot of a titlebar row that moves the window. The caller sizes
/// it (a fixed width, or `flex_1` for the rest of the row) and gives it no
/// children.
pub fn region(id: impl Into<ElementId>) -> Stateful<Div> {
    div()
        .id(id)
        // Capture phase: a press anywhere but here disarms it before the
        // pressed region (if any) arms itself.
        .on_mouse_down_out(|_, _, _| ARMED.set(false))
        .on_mouse_down(MouseButton::Left, |_, _, _| ARMED.set(true))
        .on_mouse_up(MouseButton::Left, |_, _, _| ARMED.set(false))
        .on_click(|event, window, _| {
            if event.click_count() == 2 {
                double_click(window);
            }
        })
        // The first move after the press, wherever it lands: a quick flick
        // can leave the region before its first move event, which a hovered
        // element's own move listener would miss.
        .child(
            canvas(
                |_, _, _| {},
                |_, _, window, _| {
                    window.on_mouse_event(|event: &MouseMoveEvent, phase, window, _| {
                        // Any move disarms; one with the button still down
                        // (it wasn't released in between) moves the window.
                        if phase == DispatchPhase::Capture
                            && ARMED.replace(false)
                            && event.pressed_button == Some(MouseButton::Left)
                        {
                            start_move(window);
                        }
                    })
                },
            )
            .absolute()
            .size_full(),
        )
}

/// Hand the press to the system's window move.
fn start_move(window: &Window) {
    #[cfg(not(test))]
    window.start_window_move();
    #[cfg(test)]
    {
        // gpui's test window doesn't implement a window move.
        let _ = window;
        test_log::push(test_log::Call::Move);
    }
}

/// The system's titlebar double-click action.
fn double_click(window: &Window) {
    window.titlebar_double_click();
    #[cfg(test)]
    test_log::push(test_log::Call::DoubleClick);
}

/// What the regions asked of the window, for tests.
#[cfg(test)]
pub mod test_log {
    use std::cell::RefCell;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Call {
        Move,
        DoubleClick,
    }

    thread_local! {
        static CALLS: RefCell<Vec<Call>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn push(call: Call) {
        CALLS.with_borrow_mut(|c| c.push(call));
    }

    /// The calls since the last `take`, and clear them (and any armed
    /// press, so a test starts clean).
    pub fn take() -> Vec<Call> {
        super::ARMED.set(false);
        CALLS.with_borrow_mut(std::mem::take)
    }

    use gpui::{point, px, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, Point, VisualTestContext};

    /// Press at `at`, move 8pt with the button held, release: what the
    /// regions asked of the window.
    pub fn drag_from(cx: &mut VisualTestContext, at: Point<Pixels>) -> Vec<Call> {
        take();
        cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::none());
        let to = at + point(px(8.0), px(0.0));
        cx.simulate_mouse_move(to, Some(MouseButton::Left), Modifiers::none());
        cx.simulate_mouse_up(to, MouseButton::Left, Modifiers::none());
        cx.run_until_parked();
        take()
    }

    /// A press and release at `at` without moving, `clicks` its click count
    /// (2 is the second click of a double click).
    pub fn click_at(cx: &mut VisualTestContext, at: Point<Pixels>, clicks: usize) -> Vec<Call> {
        take();
        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::none(),
            click_count: clicks,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: at,
            modifiers: Modifiers::none(),
            click_count: clicks,
        });
        cx.run_until_parked();
        take()
    }
}

#[cfg(test)]
mod tests {
    use gpui::{div, px, Context, Modifiers, MouseButton, TestAppContext, VisualTestContext, Window};

    use super::test_log::{self, Call};
    use super::*;

    /// A titlebar row: a button, then a region for the rest.
    struct Row {
        clicks: usize,
    }

    impl Render for Row {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            div()
                .flex()
                .flex_row()
                .size_full()
                .child(
                    div()
                        .id("button")
                        .debug_selector(|| "button".into())
                        .w(px(40.0))
                        .h(px(52.0))
                        .on_click(cx.listener(|this, _, _, _| this.clicks += 1)),
                )
                .child(region("drag").debug_selector(|| "drag".into()).flex_1().h(px(52.0)))
        }
    }

    fn row(cx: &mut TestAppContext) -> (gpui::Entity<Row>, VisualTestContext) {
        let window = cx.add_window(|_, _| Row { clicks: 0 });
        let entity = window.root(cx).unwrap();
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        vcx.update(|window, cx| window.draw(cx).clear(cx));
        (entity, vcx)
    }

    #[gpui::test]
    fn a_press_and_move_in_a_region_moves_the_window(cx: &mut TestAppContext) {
        let (_, mut vcx) = row(cx);
        let at = vcx.debug_bounds("drag").unwrap().center();
        assert_eq!(test_log::drag_from(&mut vcx, at), [Call::Move], "once per press");
    }

    #[gpui::test]
    fn a_flick_whose_first_move_leaves_the_region_still_moves_the_window(cx: &mut TestAppContext) {
        let (row, mut vcx) = row(cx);
        let drag = vcx.debug_bounds("drag").unwrap().center();
        let button = vcx.debug_bounds("button").unwrap().center();
        test_log::take();
        vcx.simulate_mouse_down(drag, MouseButton::Left, Modifiers::none());
        vcx.simulate_mouse_move(button, Some(MouseButton::Left), Modifiers::none());
        vcx.simulate_mouse_up(button, MouseButton::Left, Modifiers::none());
        assert_eq!(test_log::take(), [Call::Move]);
        assert_eq!(vcx.read_entity(&row, |r, _| r.clicks), 0, "the button wasn't pressed");
    }

    #[gpui::test]
    fn a_click_in_a_region_does_nothing_and_a_double_click_is_the_titlebars(cx: &mut TestAppContext) {
        let (_, mut vcx) = row(cx);
        let at = vcx.debug_bounds("drag").unwrap().center();
        assert_eq!(test_log::click_at(&mut vcx, at, 1), []);
        assert_eq!(test_log::click_at(&mut vcx, at, 2), [Call::DoubleClick]);
    }

    #[gpui::test]
    fn a_press_beside_a_region_never_moves_the_window(cx: &mut TestAppContext) {
        let (row, mut vcx) = row(cx);
        let button = vcx.debug_bounds("button").unwrap().center();
        let drag = vcx.debug_bounds("drag").unwrap().center();
        // Pressed on the button and dragged into the region: not a move.
        test_log::take();
        vcx.simulate_mouse_down(button, MouseButton::Left, Modifiers::none());
        vcx.simulate_mouse_move(drag, Some(MouseButton::Left), Modifiers::none());
        vcx.simulate_mouse_up(drag, MouseButton::Left, Modifiers::none());
        assert_eq!(test_log::take(), []);
        // A press in the region, released without moving, then a move
        // with no button: not a move either.
        assert_eq!(test_log::click_at(&mut vcx, drag, 1), []);
        vcx.simulate_mouse_move(drag + gpui::point(px(4.0), px(0.0)), None, Modifiers::none());
        assert_eq!(test_log::take(), []);
        // The button still clicks.
        assert_eq!(test_log::click_at(&mut vcx, button, 1), []);
        assert_eq!(vcx.read_entity(&row, |r, _| r.clicks), 1);
    }
}
