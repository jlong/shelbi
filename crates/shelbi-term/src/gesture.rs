//! The mouse gesture state machine both clients share.
//!
//! [`input::mouse_owner`] is the per-event policy; this is the per-*gesture*
//! layer above it, which a selection needs (a press can't be routed until the
//! pointer moves or the button comes up). The TUI and the desktop app map their
//! native mouse events onto [`MouseEvent`] in **viewer** cell coordinates and
//! hand them to one [`MouseGesture`] per pane:
//!
//! - **Left button.** A plain press is buffered. The first motion turns the
//!   gesture into a Shelbi selection anchored at the press (even when the
//!   program has mouse reporting on: dragging to select text is what a user
//!   means far more often than a program-side drag); a release with no motion
//!   is a click, forwarded to the program when it is reporting. Shift keeps the
//!   whole gesture Shelbi's (a Shift+click forwards nothing). Alt, while the
//!   program is reporting, hands the whole gesture to the program, press
//!   through release: the escape hatch for programs that need drags. A finished
//!   drag that spans text yields [`MouseOutcome::Copy`].
//! - **Wheel.** Shelbi's scrollback unless the program is reporting and Shift
//!   is not held ([`input::mouse_owner`]); then one wheel report per call.
//! - **Middle and right buttons.** Forwarded when the program owns the mouse,
//!   otherwise ignored.
//!
//! Scrollback and selection are normal-screen only; [`TerminalView`] enforces
//! that, so on the alternate screen a Shelbi-owned wheel or drag is a no-op.

use crate::input::{self, Modifiers, MouseAction, MouseButton, MouseEvent, MouseOwner};
use crate::view::TerminalView;
use crate::viewport::Placement;

/// What a mouse event resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MouseOutcome {
    /// Bytes to forward to the program (it is reporting the mouse).
    Forward(Vec<u8>),
    /// Shelbi handled it locally (scrollback / selection); repaint.
    Handled,
    /// A selection drag finished over this text; copy it to the clipboard.
    Copy(String),
    /// Nothing to do.
    Ignored,
}

/// Where a left-button gesture is in its lifecycle. Ownership is decided once
/// per gesture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DragState {
    /// No left button is down.
    Idle,
    /// Left is down at this viewer cell but has not moved yet. The first motion
    /// makes it a selection anchored here; a release with no motion is a click.
    /// `forward_click` records what that click does: forward a press+release to
    /// the program (it is reporting and Shift didn't reclaim the gesture), or,
    /// when clear, nothing (a bare Shelbi click; the old highlight was already
    /// dropped on the press).
    Pressed { col: u16, row: u16, forward_click: bool },
    /// A Shelbi selection drag is underway.
    Selecting,
    /// The gesture belongs to the program (the Alt escape hatch).
    Program,
}

/// One pane's mouse gesture state. See the module docs for the rules.
#[derive(Debug, Clone)]
pub struct MouseGesture {
    drag: DragState,
    /// Lines of scrollback one Shelbi-owned wheel event scrolls.
    wheel_lines: usize,
}

impl Default for MouseGesture {
    fn default() -> Self {
        Self::new(3)
    }
}

impl MouseGesture {
    /// A gesture tracker whose Shelbi-owned wheel event scrolls `wheel_lines`
    /// lines. A terminal UI gets one coarse event per notch (3 lines reads
    /// right); a window that already turns pixel deltas into whole lines
    /// passes 1 and calls once per line.
    pub fn new(wheel_lines: usize) -> Self {
        Self { drag: DragState::Idle, wheel_lines }
    }

    /// Whether a left-button selection drag is underway (a viewer may want to
    /// keep receiving motion outside its bounds while it is).
    pub fn is_selecting(&self) -> bool {
        matches!(self.drag, DragState::Selecting)
    }

    /// Handle one mouse event over `view`, whose session grid is placed in the
    /// viewer by `placement` (the same placement the viewer paints with, so a
    /// click maps to the cell under it, and a click in letterbox margin maps to
    /// nothing).
    pub fn handle(&mut self, view: &mut TerminalView, ev: &MouseEvent, placement: &Placement) -> MouseOutcome {
        let modes = view.emulator().mouse_modes();
        match ev.button {
            MouseButton::Left => self.left(view, ev, placement),
            MouseButton::WheelUp | MouseButton::WheelDown => match input::mouse_owner(modes.reporting, ev.mods.shift) {
                MouseOwner::Shelbi => {
                    if ev.button == MouseButton::WheelUp {
                        view.scroll_up(self.wheel_lines);
                    } else {
                        view.scroll_down(self.wheel_lines);
                    }
                    MouseOutcome::Handled
                }
                MouseOwner::Program => forward(view, ev, placement),
            },
            MouseButton::Middle | MouseButton::Right => match input::mouse_owner(modes.reporting, ev.mods.shift) {
                MouseOwner::Shelbi => MouseOutcome::Ignored,
                MouseOwner::Program => forward(view, ev, placement),
            },
        }
    }

    fn left(&mut self, view: &mut TerminalView, ev: &MouseEvent, placement: &Placement) -> MouseOutcome {
        let reporting = view.emulator().mouse_modes().reporting;
        let (col, row) = (ev.col, ev.row);
        match ev.action {
            MouseAction::Press => {
                // A new press ends any lingering highlight (the "next click"
                // rule) and defers the selection-vs-click decision to the first
                // motion. Alt while reporting is the one exception: it hands the
                // whole gesture to the program up front.
                view.clear_selection();
                if reporting && !ev.mods.shift && ev.mods.alt {
                    self.drag = DragState::Program;
                    return forward(view, ev, placement);
                }
                let forward_click = reporting && !ev.mods.shift;
                self.drag = DragState::Pressed { col, row, forward_click };
                MouseOutcome::Handled
            }
            MouseAction::Drag => match self.drag {
                DragState::Program => forward(view, ev, placement),
                DragState::Pressed { col: c0, row: r0, .. } => {
                    // First motion: this gesture is a drag, so a Shelbi
                    // selection anchored at the press point.
                    if let Some(p) = view.viewer_point_to_grid(placement, c0, r0) {
                        view.begin_selection(p);
                    }
                    if let Some(p) = view.viewer_point_to_grid(placement, col, row) {
                        view.update_selection(p);
                    }
                    self.drag = DragState::Selecting;
                    MouseOutcome::Handled
                }
                DragState::Selecting => {
                    if let Some(p) = view.viewer_point_to_grid(placement, col, row) {
                        view.update_selection(p);
                    }
                    MouseOutcome::Handled
                }
                DragState::Idle => MouseOutcome::Handled,
            },
            MouseAction::Release => match std::mem::replace(&mut self.drag, DragState::Idle) {
                DragState::Program => forward(view, ev, placement),
                DragState::Selecting => finish_selection(view),
                DragState::Pressed { col: c0, row: r0, forward_click } => {
                    if forward_click {
                        // A plain click while the program is reporting: now that
                        // it never became a drag, forward the buffered press and
                        // this release as one clean click.
                        forward_click_pair(view, (c0, r0), (col, row), placement)
                    } else {
                        MouseOutcome::Handled
                    }
                }
                DragState::Idle => MouseOutcome::Ignored,
            },
        }
    }
}

/// Encode an event for the program, if a session cell lies under it.
fn forward(view: &TerminalView, ev: &MouseEvent, placement: &Placement) -> MouseOutcome {
    match input::encode_mouse(ev, view.emulator().mouse_modes(), placement) {
        Some(bytes) => MouseOutcome::Forward(bytes),
        None => MouseOutcome::Ignored,
    }
}

/// Resolve a finished selection drag: copy it when it actually spans text,
/// otherwise drop the stray highlight of a drag that never left its anchor.
fn finish_selection(view: &mut TerminalView) -> MouseOutcome {
    let dragged = view.selection().is_some_and(|s| !s.is_empty());
    match view.selection_text() {
        Some(text) if dragged && !text.is_empty() => MouseOutcome::Copy(text),
        _ => {
            view.clear_selection();
            MouseOutcome::Handled
        }
    }
}

/// Forward a buffered click (press at `down`, release at `up`) to the program
/// as a press immediately followed by a release.
fn forward_click_pair(view: &TerminalView, down: (u16, u16), up: (u16, u16), placement: &Placement) -> MouseOutcome {
    let modes = view.emulator().mouse_modes();
    let ev = |action, (col, row): (u16, u16)| MouseEvent {
        action,
        button: MouseButton::Left,
        col,
        row,
        mods: Modifiers::default(),
    };
    let mut bytes = Vec::new();
    if let Some(b) = input::encode_mouse(&ev(MouseAction::Press, down), modes, placement) {
        bytes.extend_from_slice(&b);
    }
    if let Some(b) = input::encode_mouse(&ev(MouseAction::Release, up), modes, placement) {
        bytes.extend_from_slice(&b);
    }
    if bytes.is_empty() {
        MouseOutcome::Ignored
    } else {
        MouseOutcome::Forward(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::viewport::fit;
    use crate::Size;

    const SIZE: Size = Size { cols: 20, rows: 5 };

    fn view_with(bytes: &[u8]) -> TerminalView {
        let mut v = TerminalView::new(SIZE);
        v.feed(bytes);
        v
    }

    fn placement() -> Placement {
        fit(SIZE, SIZE)
    }

    fn ev(action: MouseAction, button: MouseButton, col: u16, row: u16, mods: Modifiers) -> MouseEvent {
        MouseEvent { action, button, col, row, mods }
    }

    fn left(action: MouseAction, col: u16, row: u16, mods: Modifiers) -> MouseEvent {
        ev(action, MouseButton::Left, col, row, mods)
    }

    const REPORTING: &[u8] = b"\x1b[?1000h\x1b[?1006h";

    #[test]
    fn plain_drag_selects_and_copies_without_reporting() {
        let mut v = view_with(b"hello world");
        let mut g = MouseGesture::default();
        let p = placement();
        assert_eq!(g.handle(&mut v, &left(MouseAction::Press, 0, 0, Modifiers::NONE), &p), MouseOutcome::Handled);
        assert_eq!(g.handle(&mut v, &left(MouseAction::Drag, 4, 0, Modifiers::NONE), &p), MouseOutcome::Handled);
        assert!(g.is_selecting());
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 4, 0, Modifiers::NONE), &p),
            MouseOutcome::Copy("hello".into())
        );
        assert!(!g.is_selecting());
    }

    #[test]
    fn plain_drag_selects_even_when_the_program_reports() {
        let mut v = view_with(b"hello world");
        v.feed(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle(&mut v, &left(MouseAction::Press, 6, 0, Modifiers::NONE), &p);
        g.handle(&mut v, &left(MouseAction::Drag, 10, 0, Modifiers::NONE), &p);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 10, 0, Modifiers::NONE), &p),
            MouseOutcome::Copy("world".into())
        );
    }

    #[test]
    fn plain_click_reaches_a_reporting_program_as_press_then_release() {
        let mut v = view_with(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        assert_eq!(g.handle(&mut v, &left(MouseAction::Press, 2, 1, Modifiers::NONE), &p), MouseOutcome::Handled);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 2, 1, Modifiers::NONE), &p),
            MouseOutcome::Forward(b"\x1b[<0;3;2M\x1b[<0;3;2m".to_vec())
        );
    }

    #[test]
    fn plain_click_without_reporting_is_shelbis_and_quiet() {
        let mut v = view_with(b"x");
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle(&mut v, &left(MouseAction::Press, 2, 1, Modifiers::NONE), &p);
        assert_eq!(g.handle(&mut v, &left(MouseAction::Release, 2, 1, Modifiers::NONE), &p), MouseOutcome::Handled);
    }

    #[test]
    fn shift_click_never_reaches_the_program() {
        let mut v = view_with(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle(&mut v, &left(MouseAction::Press, 2, 1, Modifiers::SHIFT), &p);
        assert_eq!(g.handle(&mut v, &left(MouseAction::Release, 2, 1, Modifiers::SHIFT), &p), MouseOutcome::Handled);
    }

    #[test]
    fn shift_drag_selects_while_reporting() {
        let mut v = view_with(b"hello world");
        v.feed(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle(&mut v, &left(MouseAction::Press, 0, 0, Modifiers::SHIFT), &p);
        g.handle(&mut v, &left(MouseAction::Drag, 4, 0, Modifiers::SHIFT), &p);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 4, 0, Modifiers::SHIFT), &p),
            MouseOutcome::Copy("hello".into())
        );
    }

    #[test]
    fn alt_drag_hands_the_whole_gesture_to_a_reporting_program() {
        let mut v = view_with(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        let alt = Modifiers::ALT;
        // Alt adds 8 to the button code.
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Press, 0, 0, alt), &p),
            MouseOutcome::Forward(b"\x1b[<8;1;1M".to_vec())
        );
        // Motion adds 32.
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Drag, 3, 0, alt), &p),
            MouseOutcome::Forward(b"\x1b[<40;4;1M".to_vec())
        );
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 3, 0, alt), &p),
            MouseOutcome::Forward(b"\x1b[<8;4;1m".to_vec())
        );
        assert!(v.selection().is_none());
    }

    #[test]
    fn a_drag_that_never_leaves_its_anchor_copies_nothing() {
        let mut v = view_with(b"hello");
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle(&mut v, &left(MouseAction::Press, 1, 0, Modifiers::NONE), &p);
        g.handle(&mut v, &left(MouseAction::Drag, 1, 0, Modifiers::NONE), &p);
        assert_eq!(g.handle(&mut v, &left(MouseAction::Release, 1, 0, Modifiers::NONE), &p), MouseOutcome::Handled);
        assert!(v.selection().is_none(), "the stray highlight is dropped");
    }

    #[test]
    fn a_new_press_clears_the_previous_selection() {
        let mut v = view_with(b"hello");
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle(&mut v, &left(MouseAction::Press, 0, 0, Modifiers::NONE), &p);
        g.handle(&mut v, &left(MouseAction::Drag, 3, 0, Modifiers::NONE), &p);
        g.handle(&mut v, &left(MouseAction::Release, 3, 0, Modifiers::NONE), &p);
        assert!(v.selection().is_some());
        g.handle(&mut v, &left(MouseAction::Press, 0, 2, Modifiers::NONE), &p);
        assert!(v.selection().is_none());
    }

    fn history_view() -> TerminalView {
        let mut v = TerminalView::new(SIZE);
        for i in 0..40 {
            v.feed(format!("line {i}\r\n").as_bytes());
        }
        v
    }

    #[test]
    fn wheel_scrolls_scrollback_when_nobody_reports() {
        let mut v = history_view();
        let mut g = MouseGesture::new(1);
        let p = placement();
        let up = ev(MouseAction::Press, MouseButton::WheelUp, 0, 0, Modifiers::NONE);
        assert_eq!(g.handle(&mut v, &up, &p), MouseOutcome::Handled);
        assert_eq!(g.handle(&mut v, &up, &p), MouseOutcome::Handled);
        assert_eq!(v.scroll_offset(), 2);
        let down = ev(MouseAction::Press, MouseButton::WheelDown, 0, 0, Modifiers::NONE);
        g.handle(&mut v, &down, &p);
        assert_eq!(v.scroll_offset(), 1);
    }

    #[test]
    fn wheel_goes_to_a_reporting_program_but_shift_wheel_is_shelbis() {
        let mut v = history_view();
        v.feed(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        let up = ev(MouseAction::Press, MouseButton::WheelUp, 4, 2, Modifiers::NONE);
        assert_eq!(g.handle(&mut v, &up, &p), MouseOutcome::Forward(b"\x1b[<64;5;3M".to_vec()));
        assert_eq!(v.scroll_offset(), 0);
        let shift_up = ev(MouseAction::Press, MouseButton::WheelUp, 4, 2, Modifiers::SHIFT);
        assert_eq!(g.handle(&mut v, &shift_up, &p), MouseOutcome::Handled);
        assert_eq!(v.scroll_offset(), 3);
    }

    #[test]
    fn right_button_reaches_only_a_reporting_program() {
        let mut v = view_with(b"");
        let mut g = MouseGesture::default();
        let p = placement();
        let right = ev(MouseAction::Press, MouseButton::Right, 0, 0, Modifiers::NONE);
        assert_eq!(g.handle(&mut v, &right, &p), MouseOutcome::Ignored);
        v.feed(REPORTING);
        assert_eq!(g.handle(&mut v, &right, &p), MouseOutcome::Forward(b"\x1b[<2;1;1M".to_vec()));
    }

    #[test]
    fn clicks_in_the_letterbox_margin_forward_nothing() {
        let mut v = view_with(REPORTING);
        let mut g = MouseGesture::default();
        // A 20x5 session centered in a 40x5 viewer: 10 columns of margin.
        let p = fit(SIZE, Size::new(40, 5));
        g.handle(&mut v, &left(MouseAction::Press, 2, 0, Modifiers::NONE), &p);
        assert_eq!(g.handle(&mut v, &left(MouseAction::Release, 2, 0, Modifiers::NONE), &p), MouseOutcome::Ignored);
    }
}
