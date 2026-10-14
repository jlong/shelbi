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
//! - **Selection modes.** On a Shelbi-owned press (the program isn't
//!   reporting, or Shift is held), a double-click selects the word under the
//!   pointer and a triple-click its line, at once; dragging on extends word by
//!   word or line by line ([`MouseGesture::handle_clicks`] takes the click
//!   count, which the viewer's platform supplies). Alt on a Shelbi-owned drag
//!   selects a block (a rectangle) instead of a stream: Alt+drag when the
//!   program isn't reporting, Shift+Alt+drag when it is.
//! - **Motion.** Pointer motion with no button held reaches the program only
//!   under any-event tracking (`ESC[?1003h`), and not while Shift is held;
//!   drags reach it only under button-event tracking (the encoder checks the
//!   modes, see [`input::encode_mouse`]).
//! - **Wheel.** Shelbi's scrollback unless the program is reporting and Shift
//!   is not held ([`input::mouse_owner`]); then one wheel report per call. On
//!   the alternate screen, where there is no scrollback, the wheel of a
//!   program that doesn't report the mouse sends arrow keys instead
//!   (alternate-scroll mode, `ESC[?1007h`, on unless the program turns it
//!   off; xterm's rule), so the wheel scrolls `less` and `man`.
//! - **Middle and right buttons.** Forwarded when the program owns the mouse,
//!   otherwise ignored.
//!
//! Scrollback is normal-screen only; [`TerminalView`] enforces that.
//! Selection works on both screens.

use crate::input::{self, Modifiers, MouseAction, MouseButton, MouseEvent, MouseOwner};
use crate::selection::SelectionMode;
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
    /// dropped on the press). `block` makes a drag select a rectangle.
    Pressed { col: u16, row: u16, forward_click: bool, block: bool },
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
        self.handle_clicks(view, ev, placement, 1)
    }

    /// [`MouseGesture::handle`] with the platform's click count for a press
    /// (2 for the second press of a double-click, 3 for a triple). A viewer
    /// that can't count clicks calls [`MouseGesture::handle`].
    pub fn handle_clicks(
        &mut self,
        view: &mut TerminalView,
        ev: &MouseEvent,
        placement: &Placement,
        clicks: u8,
    ) -> MouseOutcome {
        let modes = view.emulator().mouse_modes();
        if ev.action == MouseAction::Move {
            return match input::mouse_owner(modes.reporting, ev.mods.shift) {
                MouseOwner::Program => forward(view, ev, placement),
                MouseOwner::Shelbi => MouseOutcome::Ignored,
            };
        }
        match ev.button {
            MouseButton::Left => self.left(view, ev, placement, clicks),
            MouseButton::WheelUp | MouseButton::WheelDown => match input::mouse_owner(modes.reporting, ev.mods.shift) {
                MouseOwner::Shelbi if view.emulator().alternate_scroll() && !modes.reporting => {
                    let key = if ev.button == MouseButton::WheelUp { input::Key::Up } else { input::Key::Down };
                    let encoding = view.emulator().key_encoding();
                    let bytes = input::encode_key(key, Modifiers::NONE, encoding).repeat(self.wheel_lines);
                    MouseOutcome::Forward(bytes)
                }
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

    fn left(&mut self, view: &mut TerminalView, ev: &MouseEvent, placement: &Placement, clicks: u8) -> MouseOutcome {
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
                let shelbi_owned = !reporting || ev.mods.shift;
                if shelbi_owned && clicks >= 2 {
                    // A double- or triple-click selects its word or line now;
                    // a drag from here extends it by words or lines.
                    let mode = if clicks == 2 { SelectionMode::Word } else { SelectionMode::Line };
                    if let Some(p) = view.viewer_point_to_grid(placement, col, row) {
                        view.begin_selection_mode(p, mode);
                    }
                    self.drag = DragState::Selecting;
                    return MouseOutcome::Handled;
                }
                self.drag = DragState::Pressed {
                    col,
                    row,
                    forward_click: !shelbi_owned,
                    block: shelbi_owned && ev.mods.alt,
                };
                MouseOutcome::Handled
            }
            MouseAction::Drag => match self.drag {
                DragState::Program => forward(view, ev, placement),
                DragState::Pressed { col: c0, row: r0, block, .. } => {
                    // First motion: this gesture is a drag, so a Shelbi
                    // selection anchored at the press point.
                    let mode = if block { SelectionMode::Block } else { SelectionMode::Cell };
                    if let Some(p) = view.viewer_point_to_grid(placement, c0, r0) {
                        view.begin_selection_mode(p, mode);
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
            MouseAction::Move => MouseOutcome::Ignored,
            MouseAction::Release => match std::mem::replace(&mut self.drag, DragState::Idle) {
                DragState::Program => forward(view, ev, placement),
                DragState::Selecting => finish_selection(view),
                DragState::Pressed { col: c0, row: r0, forward_click, .. } => {
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
        // Button-event tracking, so the program wants the drag's motion too.
        let mut v = view_with(b"\x1b[?1002h\x1b[?1006h");
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

    #[test]
    fn under_click_only_reporting_an_alt_drag_forwards_press_and_release_only() {
        let mut v = view_with(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        let alt = Modifiers::ALT;
        assert!(matches!(g.handle(&mut v, &left(MouseAction::Press, 0, 0, alt), &p), MouseOutcome::Forward(_)));
        assert_eq!(g.handle(&mut v, &left(MouseAction::Drag, 3, 0, alt), &p), MouseOutcome::Ignored);
        assert!(matches!(g.handle(&mut v, &left(MouseAction::Release, 3, 0, alt), &p), MouseOutcome::Forward(_)));
    }

    #[test]
    fn double_click_selects_and_copies_a_word() {
        let mut v = view_with(b"cargo test src/main.rs:42 ok");
        let mut g = MouseGesture::default();
        let p = placement();
        let press = left(MouseAction::Press, 13, 0, Modifiers::NONE);
        g.handle_clicks(&mut v, &press, &p, 1);
        g.handle_clicks(&mut v, &left(MouseAction::Release, 13, 0, Modifiers::NONE), &p, 1);
        assert_eq!(g.handle_clicks(&mut v, &press, &p, 2), MouseOutcome::Handled);
        assert_eq!(v.selection_text().as_deref(), Some("src/main.rs"), "`:` ends the word");
        assert_eq!(
            g.handle_clicks(&mut v, &left(MouseAction::Release, 13, 0, Modifiers::NONE), &p, 2),
            MouseOutcome::Copy("src/main.rs".into())
        );
    }

    #[test]
    fn double_click_drag_extends_by_words() {
        let mut v = view_with(b"one two three four");
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle_clicks(&mut v, &left(MouseAction::Press, 5, 0, Modifiers::NONE), &p, 2);
        g.handle(&mut v, &left(MouseAction::Drag, 9, 0, Modifiers::NONE), &p);
        assert_eq!(v.selection_text().as_deref(), Some("two three"));
        // Back across the anchor: the anchor word stays selected.
        g.handle(&mut v, &left(MouseAction::Drag, 1, 0, Modifiers::NONE), &p);
        assert_eq!(v.selection_text().as_deref(), Some("one two"));
    }

    #[test]
    fn triple_click_selects_the_line() {
        let mut v = view_with(b"first line\r\nsecond line");
        let mut g = MouseGesture::default();
        let p = placement();
        g.handle_clicks(&mut v, &left(MouseAction::Press, 3, 1, Modifiers::NONE), &p, 3);
        assert_eq!(v.selection_text().as_deref(), Some("second line"));
    }

    #[test]
    fn a_reporting_program_gets_double_clicks_unless_shift_is_held() {
        let mut v = view_with(b"word here");
        v.feed(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        let press = left(MouseAction::Press, 1, 0, Modifiers::NONE);
        assert_eq!(g.handle_clicks(&mut v, &press, &p, 2), MouseOutcome::Handled);
        assert!(matches!(
            g.handle_clicks(&mut v, &left(MouseAction::Release, 1, 0, Modifiers::NONE), &p, 2),
            MouseOutcome::Forward(_)
        ));
        assert!(v.selection().is_none());
        let shift = left(MouseAction::Press, 1, 0, Modifiers::SHIFT);
        g.handle_clicks(&mut v, &shift, &p, 2);
        assert_eq!(v.selection_text().as_deref(), Some("word"));
    }

    #[test]
    fn alt_drag_selects_a_block_when_nobody_reports() {
        let mut v = view_with(b"abcdef\r\nghijkl\r\nmnopqr");
        let mut g = MouseGesture::default();
        let p = placement();
        let alt = Modifiers::ALT;
        g.handle(&mut v, &left(MouseAction::Press, 1, 0, alt), &p);
        g.handle(&mut v, &left(MouseAction::Drag, 3, 2, alt), &p);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 3, 2, alt), &p),
            MouseOutcome::Copy("bcd\nhij\nnop".into())
        );
    }

    #[test]
    fn shift_alt_drag_selects_a_block_while_reporting() {
        let mut v = view_with(b"abcdef\r\nghijkl");
        v.feed(REPORTING);
        let mut g = MouseGesture::default();
        let p = placement();
        let mods = Modifiers { shift: true, alt: true, ..Modifiers::NONE };
        assert_eq!(g.handle(&mut v, &left(MouseAction::Press, 4, 1, mods), &p), MouseOutcome::Handled);
        g.handle(&mut v, &left(MouseAction::Drag, 2, 0, mods), &p);
        assert_eq!(v.selection_text().as_deref(), Some("cde\nijk"));
    }

    #[test]
    fn motion_reaches_only_an_any_event_program_and_never_with_shift() {
        let mut g = MouseGesture::default();
        let p = placement();
        let moved = |mods| ev(MouseAction::Move, MouseButton::Left, 2, 1, mods);
        let mut v = view_with(REPORTING);
        assert_eq!(g.handle(&mut v, &moved(Modifiers::NONE), &p), MouseOutcome::Ignored);
        v.feed(b"\x1b[?1003h");
        assert_eq!(
            g.handle(&mut v, &moved(Modifiers::NONE), &p),
            MouseOutcome::Forward(b"\x1b[<35;3;2M".to_vec())
        );
        assert_eq!(g.handle(&mut v, &moved(Modifiers::SHIFT), &p), MouseOutcome::Ignored);
        let mut quiet = view_with(b"");
        assert_eq!(g.handle(&mut quiet, &moved(Modifiers::NONE), &p), MouseOutcome::Ignored);
    }

    #[test]
    fn on_the_alternate_screen_the_wheel_sends_arrow_keys() {
        let mut v = view_with(b"\x1b[?1049h");
        let mut g = MouseGesture::new(3);
        let p = placement();
        let wheel = |b| ev(MouseAction::Press, b, 0, 0, Modifiers::NONE);
        assert_eq!(g.handle(&mut v, &wheel(MouseButton::WheelUp), &p), MouseOutcome::Forward(b"\x1b[A\x1b[A\x1b[A".to_vec()));
        // Application cursor mode (as `less` sets it) changes the arrows.
        v.feed(b"\x1b[?1h");
        assert_eq!(g.handle(&mut v, &wheel(MouseButton::WheelDown), &p), MouseOutcome::Forward(b"\x1bOB\x1bOB\x1bOB".to_vec()));
        // The program can turn it off.
        v.feed(b"\x1b[?1007l");
        assert_eq!(g.handle(&mut v, &wheel(MouseButton::WheelDown), &p), MouseOutcome::Handled);
    }

    /// What vim 9 (`vim -u NONE -N -c 'set mouse=a'`, `TERM=xterm-256color`)
    /// writes at startup, its terminal-mode sequences in order, captured from
    /// a real session: alternate screen, modifyOtherKeys, DECCKM, bracketed
    /// paste, focus events, then SGR + click reporting and button-event
    /// tracking.
    const VIM_STARTUP: &[u8] =
        b"\x1b[?1049h\x1b[>4;2m\x1b[?1h\x1b[?2004h\x1b[?1004h\x1b[?12h\x1b[?12l\x1b[?25l\x1b[?1006;1000h\x1b[?1002h\x1b[>c";

    /// Claude Code's full-screen renderer with `mouseTracking: "full"` (its
    /// default): click, button-event, any-event, and SGR, on the alternate
    /// screen.
    const CLAUDE_CODE_FULLSCREEN: &[u8] = b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h";

    #[test]
    fn vim_gets_sgr_clicks_wheel_and_alt_drags_while_a_plain_drag_selects() {
        let mut v = view_with(VIM_STARTUP);
        let modes = v.emulator().mouse_modes();
        assert!(modes.reporting && modes.drag && modes.sgr && !modes.motion, "{modes:?}");
        let mut g = MouseGesture::new(1);
        let p = placement();
        let none = Modifiers::NONE;
        // A click: SGR press + release.
        g.handle(&mut v, &left(MouseAction::Press, 3, 1, none), &p);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 3, 1, none), &p),
            MouseOutcome::Forward(b"\x1b[<0;4;2M\x1b[<0;4;2m".to_vec())
        );
        // The wheel scrolls vim, not Shelbi (and not as arrow keys).
        assert_eq!(
            g.handle(&mut v, &ev(MouseAction::Press, MouseButton::WheelDown, 3, 1, none), &p),
            MouseOutcome::Forward(b"\x1b[<65;4;2M".to_vec())
        );
        // Alt+drag is vim's visual selection: press, motion, release.
        let alt = Modifiers::ALT;
        assert!(matches!(g.handle(&mut v, &left(MouseAction::Press, 1, 0, alt), &p), MouseOutcome::Forward(_)));
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Drag, 5, 0, alt), &p),
            MouseOutcome::Forward(b"\x1b[<40;6;1M".to_vec())
        );
        assert!(matches!(g.handle(&mut v, &left(MouseAction::Release, 5, 0, alt), &p), MouseOutcome::Forward(_)));
        // Plain motion isn't reported (vim asked for button-event only).
        assert_eq!(g.handle(&mut v, &ev(MouseAction::Move, MouseButton::Left, 2, 2, none), &p), MouseOutcome::Ignored);
        // A plain drag is still a Shelbi selection of what vim shows.
        v.feed(b"\x1b[Hvim text here");
        g.handle(&mut v, &left(MouseAction::Press, 0, 0, none), &p);
        g.handle(&mut v, &left(MouseAction::Drag, 2, 0, none), &p);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 2, 0, none), &p),
            MouseOutcome::Copy("vim".into())
        );
    }

    #[test]
    fn claude_code_fullscreen_gets_motion_and_the_wheel_but_shift_wheel_sends_nothing() {
        let mut v = view_with(CLAUDE_CODE_FULLSCREEN);
        let modes = v.emulator().mouse_modes();
        assert!(modes.reporting && modes.drag && modes.motion && modes.sgr, "{modes:?}");
        let mut g = MouseGesture::new(1);
        let p = placement();
        let none = Modifiers::NONE;
        assert_eq!(
            g.handle(&mut v, &ev(MouseAction::Move, MouseButton::Left, 7, 3, none), &p),
            MouseOutcome::Forward(b"\x1b[<35;8;4M".to_vec())
        );
        assert_eq!(
            g.handle(&mut v, &ev(MouseAction::Press, MouseButton::WheelUp, 7, 3, none), &p),
            MouseOutcome::Forward(b"\x1b[<64;8;4M".to_vec())
        );
        // Shift claims the wheel for Shelbi, but the alternate screen has no
        // scrollback, and a reporting program gets no arrow keys from it.
        assert_eq!(
            g.handle(&mut v, &ev(MouseAction::Press, MouseButton::WheelUp, 7, 3, Modifiers::SHIFT), &p),
            MouseOutcome::Handled
        );
        g.handle(&mut v, &left(MouseAction::Press, 7, 3, none), &p);
        assert_eq!(
            g.handle(&mut v, &left(MouseAction::Release, 7, 3, none), &p),
            MouseOutcome::Forward(b"\x1b[<0;8;4M\x1b[<0;8;4m".to_vec())
        );
    }
}
