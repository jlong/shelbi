//! The task pane's width (`dt-task-pane-resize`): the pixel math
//! ([`PaneWidth`], tested without a window) for the resize handle on the
//! pane's left edge.
//!
//! The width is kept as the user chose it, [`DEFAULT_W`] until they drag.
//! A drag moves the edge as far as the pointer moves from where it pressed
//! (so a press never jumps the edge to the pointer), at least [`MIN_W`] and at most what leaves [`BOARD_KEEP`] of the main area
//! beside it. A main area too narrow for the chosen width draws the pane
//! narrower (the layout shrinks it to fit) without forgetting the width, so
//! it comes back when the window grows. Each window saves its own width
//! (`WindowState::task_pane_width`).
//!
//! The width shown also picks the details' layout ([`PaneLayout`],
//! `pane-responsive-props`): under [`SPLIT_W`] the properties stack under
//! the title, as at the default width; at it or past it they move to a
//! [`SIDE_W`] column beside the title and body, the way Linear's issue view
//! has them. A drag across it switches the layout live.

/// The pane's width until the user drags it: the Figma frame's 640pt.
pub const DEFAULT_W: f32 = 640.0;
/// The narrowest a drag makes the pane: the 108pt property labels, their
/// values, and the content's insets.
pub const MIN_W: f32 = 400.0;
/// The main area a drag leaves beside the pane, so part of the board (or
/// the screen under it) stays in view.
pub const BOARD_KEEP: f32 = 240.0;

/// The narrowest pane that puts its properties in a column beside the title
/// and body. The title and body keep 580pt there (a little under the 600pt
/// they have at the default width), so the body never reads narrower for
/// the split.
pub const SPLIT_W: f32 = 860.0;
/// The properties' column in the split layout, its left divider included.
pub const SIDE_W: f32 = 280.0;
/// A property's label column in the split layout (the stacked one keeps
/// the frame's 108pt): room for `Blocked by` with the values beside it.
pub const SIDE_LABEL_W: f32 = 84.0;

/// Where the pane's details put its properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneLayout {
    /// Under the title, over a divider and the body: under [`SPLIT_W`].
    Stacked,
    /// In a [`SIDE_W`] column beside the title and body: at [`SPLIT_W`] and
    /// past it.
    Split,
}

impl PaneLayout {
    /// The layout of a pane `shown` wide (the width it draws at, not the one
    /// chosen: a narrow window that shrinks the pane stacks it too).
    pub fn for_width(shown: f32) -> Self {
        if shown >= SPLIT_W {
            PaneLayout::Split
        } else {
            PaneLayout::Stacked
        }
    }
}

/// The details' scroll offset after the layout changed under it (gpui's,
/// so negative as it scrolls down, at most `max` down): a reader in the body
/// (its top scrolled out of sight) keeps the body's line where it was on
/// screen, though the body moved from `was_top` to `now_top` in the scroll's
/// content; one who can still see the body's top (or what's over it) keeps
/// the offset, so the properties don't scroll away from them.
pub fn keep_body_line(offset: f32, max: f32, was_top: f32, now_top: f32) -> f32 {
    if !offset.is_finite() || !max.is_finite() || !was_top.is_finite() || !now_top.is_finite() {
        return offset;
    }
    if was_top + offset >= 0.0 {
        return offset;
    }
    (offset + was_top - now_top).clamp(-max.max(0.0), 0.0)
}

/// The pane's chosen width.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PaneWidth {
    wanted: f32,
}

impl Default for PaneWidth {
    fn default() -> Self {
        Self { wanted: DEFAULT_W }
    }
}

impl PaneWidth {
    /// A saved width; junk (not finite, under the minimum) is the default.
    pub fn new(saved: f32) -> Self {
        if saved.is_finite() && saved >= MIN_W {
            Self { wanted: saved.round() }
        } else {
            Self::default()
        }
    }

    /// The width the user chose, as saved. The pane asks for it; a main area
    /// without room shrinks it for display only.
    pub fn wanted(&self) -> f32 {
        self.wanted
    }

    /// The width the pane shows in a layer `room` wide (the main area inside
    /// the pane's insets): the chosen one, or all the room when it's less.
    pub fn shown(&self, room: f32) -> f32 {
        self.wanted.min(room.max(0.0))
    }

    /// Follow a drag of the left edge that started on a pane `from` wide:
    /// the pointer has moved `dx` since the press (left is negative, so it
    /// widens), in a layer `room` wide. Whether the width changed.
    pub fn drag(&mut self, from: f32, dx: f32, room: f32) -> bool {
        if !from.is_finite() || !dx.is_finite() || !room.is_finite() {
            return false;
        }
        let width = clamp((from - dx).round(), room);
        let changed = width != self.wanted;
        self.wanted = width;
        changed
    }
}

/// `width` within a layer `room` wide: at least [`MIN_W`], and at most what
/// leaves [`BOARD_KEEP`] beside it (never under the minimum).
fn clamp(width: f32, room: f32) -> f32 {
    let max = (room - BOARD_KEEP).max(MIN_W);
    width.clamp(MIN_W, max)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The main area of the 1440pt frame beside its 280pt sidebar, inside
    /// the pane's 8pt insets.
    const ROOM: f32 = 1144.0;

    #[test]
    fn the_default_is_the_frames_640() {
        assert_eq!(PaneWidth::default().wanted(), DEFAULT_W);
        assert_eq!(DEFAULT_W, 640.0);
    }

    #[test]
    fn dragging_left_widens_and_right_narrows() {
        let mut w = PaneWidth::default();
        assert!(w.drag(640.0, -160.0, ROOM));
        assert_eq!(w.wanted(), 800.0);
        assert!(w.drag(640.0, 140.0, ROOM));
        assert_eq!(w.wanted(), 500.0);
        assert!(!w.drag(640.0, 140.0, ROOM), "the same place changes nothing");
        assert!(w.drag(640.0, 0.4, ROOM), "back where it pressed, whole points");
        assert_eq!(w.wanted(), 640.0);
    }

    #[test]
    fn a_drag_keeps_the_minimum_and_leaves_some_board() {
        let mut w = PaneWidth::default();
        w.drag(640.0, 630.0, ROOM);
        assert_eq!(w.wanted(), MIN_W, "past the minimum, the minimum");
        w.drag(640.0, 700.0, ROOM);
        assert_eq!(w.wanted(), MIN_W, "past the right edge too");
        w.drag(640.0, -1000.0, ROOM);
        assert_eq!(w.wanted(), ROOM - BOARD_KEEP, "past the maximum, the maximum");
    }

    #[test]
    fn a_main_area_too_narrow_for_the_board_keeps_the_minimum() {
        let mut w = PaneWidth::default();
        w.drag(500.0, -100.0, 500.0);
        assert_eq!(w.wanted(), MIN_W);
    }

    #[test]
    fn a_narrow_layer_shows_less_without_forgetting() {
        let w = PaneWidth::new(760.0);
        assert_eq!(w.shown(ROOM), 760.0);
        assert_eq!(w.shown(584.0), 584.0);
        assert_eq!(w.wanted(), 760.0);
        assert_eq!(w.shown(-4.0), 0.0);
    }

    #[test]
    fn a_saved_width_comes_back_and_junk_is_the_default() {
        assert_eq!(PaneWidth::new(812.0).wanted(), 812.0);
        assert_eq!(PaneWidth::new(812.4).wanted(), 812.0, "whole points");
        assert_eq!(PaneWidth::new(f32::NAN), PaneWidth::default());
        assert_eq!(PaneWidth::new(f32::INFINITY), PaneWidth::default());
        assert_eq!(PaneWidth::new(12.0), PaneWidth::default());
    }

    #[test]
    fn the_properties_go_beside_the_body_from_the_breakpoint_on() {
        assert_eq!(PaneLayout::for_width(DEFAULT_W), PaneLayout::Stacked, "the default stays one column");
        assert_eq!(PaneLayout::for_width(MIN_W), PaneLayout::Stacked);
        assert_eq!(PaneLayout::for_width(SPLIT_W - 1.0), PaneLayout::Stacked);
        assert_eq!(PaneLayout::for_width(SPLIT_W), PaneLayout::Split, "at the breakpoint");
        assert_eq!(PaneLayout::for_width(1200.0), PaneLayout::Split);
        // A chosen width past it, in a window that shows less, stacks.
        assert_eq!(PaneLayout::for_width(PaneWidth::new(900.0).shown(584.0)), PaneLayout::Stacked);
        const { assert!(SPLIT_W - SIDE_W >= 560.0, "the body keeps most of the default's room") };
        const { assert!(SIDE_W - SIDE_LABEL_W - 2.0 * 20.0 >= 140.0, "the side's values have room") };
    }

    #[test]
    fn a_reader_in_the_body_keeps_their_line_across_a_switch() {
        // 300pt into a body that starts at 260: the body moves up 200pt as
        // the properties leave it, so the scroll follows it up.
        assert_eq!(keep_body_line(-300.0, 2000.0, 260.0, 60.0), -100.0);
        // And back down as they return.
        assert_eq!(keep_body_line(-100.0, 2000.0, 60.0, 260.0), -300.0);
        // Within what the new layout can scroll.
        assert_eq!(keep_body_line(-100.0, 150.0, 60.0, 400.0), -150.0);
        assert_eq!(keep_body_line(-300.0, 2000.0, 260.0, 290.0), -330.0);
    }

    #[test]
    fn a_reader_over_the_body_keeps_the_offset() {
        assert_eq!(keep_body_line(0.0, 2000.0, 260.0, 60.0), 0.0, "at the top, the top");
        assert_eq!(keep_body_line(0.0, 2000.0, 60.0, 260.0), 0.0, "the properties come back in view");
        assert_eq!(keep_body_line(-40.0, 2000.0, 60.0, 260.0), -40.0, "the body's top still showed");
        assert_eq!(keep_body_line(-60.0, 2000.0, 60.0, 260.0), -60.0, "right at it");
        assert!(keep_body_line(f32::NAN, 2000.0, 60.0, 260.0).is_nan());
        assert_eq!(keep_body_line(-300.0, 2000.0, f32::NAN, 260.0), -300.0);
    }

    #[test]
    fn junk_pointer_input_leaves_the_width() {
        let mut w = PaneWidth::new(700.0);
        assert!(!w.drag(700.0, f32::NAN, ROOM));
        assert!(!w.drag(f32::INFINITY, 10.0, ROOM));
        assert!(!w.drag(700.0, 10.0, f32::NAN));
        assert_eq!(w.wanted(), 700.0);
    }
}
