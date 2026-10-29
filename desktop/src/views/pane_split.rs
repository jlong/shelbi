//! The task pane's details | session split (`dt-task-pane-session`): the
//! pixel math ([`PaneSplit`], tested without a window) for the In progress
//! frame's resize handle (Figma 2570:10583) over its live session
//! (2570:10585).
//!
//! The split is kept as the session area's height, the Figma frame's 356pt
//! (its 44pt header included) until the user drags or steps it. A pane too
//! short for it gives the session less without forgetting the height, so it
//! comes back when the window grows. Both sides keep a minimum; a pane too
//! short for both minimums shares what it has between them.

/// The resize handle's height: the frame's 17pt band (4pt over and under a
/// 4pt grip, centered, and the 1pt rule under it), all of it grab room.
pub const HANDLE_H: f32 = 17.0;
/// The grip's size and corners.
pub const GRIP_W: f32 = 32.0;
pub const GRIP_H: f32 = 4.0;
pub const GRIP_RADIUS: f32 = 16.0;
/// The session area's height at the reference size, its header included.
pub const SESSION_H: f32 = 356.0;
/// The session header's height.
pub const SESSION_HEADER_H: f32 = 44.0;
/// The shortest the session area is dragged to: its header and five
/// terminal rows.
pub const MIN_SESSION_H: f32 = SESSION_HEADER_H + 96.0;
/// The shortest the details above it are dragged to: the title and a few
/// properties.
pub const MIN_DETAILS_H: f32 = 120.0;
/// How far one step of the keyboard resize moves the split.
pub const KEY_STEP: f32 = 24.0;

/// The session area's height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneSplit {
    session_h: f32,
}

impl Default for PaneSplit {
    fn default() -> Self {
        Self { session_h: SESSION_H }
    }
}

impl PaneSplit {
    /// The height the user asked for (dragged or stepped), unclamped.
    #[cfg(test)]
    pub fn wanted(&self) -> f32 {
        self.session_h
    }

    /// The session area's height in a column `total` tall (the pane under its
    /// Header): the handle and the session fill what the details leave.
    pub fn session_height(&self, total: f32) -> f32 {
        clamp(self.session_h, total)
    }

    /// Follow a handle drag: the pointer is `y` from the column's top, in a
    /// column `total` tall. The handle centers on the pointer, as far as the
    /// minimums allow.
    pub fn drag_to(&mut self, y: f32, total: f32) {
        if !y.is_finite() || !total.is_finite() || total <= HANDLE_H {
            return;
        }
        let details = y - HANDLE_H / 2.0;
        self.session_h = clamp(total - HANDLE_H - details, total);
    }

    /// The keyboard's resize: one [`KEY_STEP`] taller (`grow`) or shorter,
    /// from the height on screen now.
    pub fn step(&mut self, grow: bool, total: f32) {
        if !total.is_finite() || total <= HANDLE_H {
            return;
        }
        let now = self.session_height(total);
        let delta = if grow { KEY_STEP } else { -KEY_STEP };
        self.session_h = clamp(now + delta, total);
    }
}

/// `session` within a column `total` tall: at least [`MIN_SESSION_H`], with
/// [`MIN_DETAILS_H`] left above the handle; a column too short for both
/// shares the room in proportion to the minimums.
fn clamp(session: f32, total: f32) -> f32 {
    let room = (total - HANDLE_H).max(0.0);
    if room < MIN_SESSION_H + MIN_DETAILS_H {
        return room * MIN_SESSION_H / (MIN_SESSION_H + MIN_DETAILS_H);
    }
    let session = if session.is_finite() { session } else { SESSION_H };
    session.clamp(MIN_SESSION_H, room - MIN_DETAILS_H)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 0.01
    }

    /// The reference pane is 1008pt; under its 52pt Header the column is 956.
    const REF: f32 = 956.0;

    #[test]
    fn the_reference_pane_matches_the_frame() {
        let s = PaneSplit::default();
        assert!(close(s.session_height(REF), 356.0));
        // The details scroll is the frame's 583pt over the 17pt handle.
        assert!(close(REF - HANDLE_H - s.session_height(REF), 583.0));
    }

    #[test]
    fn dragging_centers_the_handle_on_the_pointer_within_the_minimums() {
        let mut s = PaneSplit::default();
        // Handle centered at 400: the details get 391.5, the session the rest.
        s.drag_to(400.0, REF);
        assert!(close(s.session_height(REF), REF - 400.0 - HANDLE_H / 2.0), "{}", s.session_height(REF));
        s.drag_to(0.0, REF);
        assert!(close(s.session_height(REF), REF - HANDLE_H - MIN_DETAILS_H), "the details keep their minimum");
        s.drag_to(5000.0, REF);
        assert!(close(s.session_height(REF), MIN_SESSION_H), "the session keeps its minimum");
    }

    #[test]
    fn a_short_window_shrinks_the_session_and_gives_it_back() {
        let s = PaneSplit::default();
        // 484pt pane (a 600 x 500 window): 432 under the Header.
        let short = 432.0;
        assert!(close(s.session_height(short), short - HANDLE_H - MIN_DETAILS_H));
        assert!(close(s.wanted(), SESSION_H), "the asked-for height is kept");
        assert!(close(s.session_height(REF), SESSION_H));
    }

    #[test]
    fn a_pane_too_short_for_both_minimums_shares_its_room() {
        let s = PaneSplit::default();
        let h = s.session_height(200.0);
        let room = 200.0 - HANDLE_H;
        assert!(close(h, room * MIN_SESSION_H / (MIN_SESSION_H + MIN_DETAILS_H)));
        assert_eq!(s.session_height(0.0), 0.0, "never negative");
    }

    #[test]
    fn the_keyboard_steps_from_what_is_on_screen() {
        let mut s = PaneSplit::default();
        s.step(true, REF);
        assert!(close(s.session_height(REF), SESSION_H + KEY_STEP));
        s.step(false, REF);
        s.step(false, REF);
        assert!(close(s.session_height(REF), SESSION_H - KEY_STEP));
        // In a short pane the step starts from the clamped height, so the
        // first press visibly moves it.
        let mut s = PaneSplit::default();
        s.step(false, 432.0);
        assert!(close(s.session_height(432.0), 432.0 - HANDLE_H - MIN_DETAILS_H - KEY_STEP));
    }

    #[test]
    fn junk_input_leaves_a_sane_split() {
        let mut s = PaneSplit::default();
        s.drag_to(f32::NAN, REF);
        s.drag_to(100.0, f32::NAN);
        s.step(true, f32::INFINITY);
        assert_eq!(s, PaneSplit::default());
    }
}
