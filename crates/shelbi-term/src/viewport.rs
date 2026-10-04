//! Clipping and letterboxing.
//!
//! The PTY is sized to the most recently active client (plan, "The protocol" /
//! "Sizing"); every other viewer sees a session whose size differs from its
//! own. This module is the pure geometry that reconciles the two, per axis:
//!
//! - **Letterbox** when the viewer is *larger* than the session on an axis: the
//!   session content is centered and the surplus becomes blank margin
//!   (`pad_before` / `pad_after`).
//! - **Clip** when the viewer is *smaller*: only the cells that fit are shown.
//!   The visible window is anchored at the session origin (top-left), the
//!   convention terminal multiplexers use so a shell prompt and a program's
//!   top-left stay visible rather than scrolling off.
//!
//! The same [`Placement`] also translates a viewer cell back to a session cell
//! for mouse reporting ([`Placement::viewer_to_session`]): a click in the
//! letterbox margin, or outside a clipped window, maps to `None`.

use crate::Size;

/// How one axis (columns or rows) of a session maps into a viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Axis {
    /// Blank cells shown before the session content (letterbox margin). Zero
    /// when clipping.
    pub pad_before: u16,
    /// Blank cells shown after the session content. Zero when clipping.
    pub pad_after: u16,
    /// First session index shown. Always 0: a clipped window anchors at the
    /// origin.
    pub src_start: u16,
    /// Number of session cells shown (the whole axis when letterboxing, the
    /// viewer's extent when clipping).
    pub len: u16,
}

impl Axis {
    /// Fit a session axis of `session` cells into a viewer axis of `viewer`
    /// cells.
    fn fit(session: u16, viewer: u16) -> Axis {
        if viewer >= session {
            let pad = viewer - session;
            Axis { pad_before: pad / 2, pad_after: pad - pad / 2, src_start: 0, len: session }
        } else {
            Axis { pad_before: 0, pad_after: 0, src_start: 0, len: viewer }
        }
    }

    /// Whether this axis clips session content (viewer smaller than session).
    fn clipped(&self, session: u16) -> bool {
        self.len < session
    }

    /// Map a viewer index on this axis to a session index, or `None` if it
    /// falls in the margin or past the visible window.
    fn map(&self, viewer_index: u16) -> Option<u16> {
        if viewer_index < self.pad_before {
            return None;
        }
        let rel = viewer_index - self.pad_before;
        if rel >= self.len {
            return None;
        }
        Some(self.src_start + rel)
    }
}

/// The placement of a session's grid inside a viewer, per axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placement {
    /// Session size being shown.
    pub session: Size,
    /// Viewer size doing the showing.
    pub viewer: Size,
    /// Horizontal mapping.
    pub cols: Axis,
    /// Vertical mapping.
    pub rows: Axis,
}

impl Placement {
    /// Whether any session content is clipped (viewer smaller on either axis).
    pub fn is_clipped(&self) -> bool {
        self.cols.clipped(self.session.cols) || self.rows.clipped(self.session.rows)
    }

    /// Whether the viewer letterboxes the session (viewer larger on either
    /// axis, so there is blank margin).
    pub fn is_letterboxed(&self) -> bool {
        self.cols.pad_before + self.cols.pad_after > 0 || self.rows.pad_before + self.rows.pad_after > 0
    }

    /// Translate a viewer cell `(col, row)` to the session cell it covers, or
    /// `None` if it lands in the letterbox margin or outside a clipped window.
    pub fn viewer_to_session(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        Some((self.cols.map(col)?, self.rows.map(row)?))
    }
}

/// Compute how `session` content is placed inside a `viewer`.
pub fn fit(session: Size, viewer: Size) -> Placement {
    let session = session.non_zero();
    let viewer = viewer.non_zero();
    Placement {
        session,
        viewer,
        cols: Axis::fit(session.cols, viewer.cols),
        rows: Axis::fit(session.rows, viewer.rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_size_is_identity() {
        let p = fit(Size::new(80, 24), Size::new(80, 24));
        assert!(!p.is_clipped());
        assert!(!p.is_letterboxed());
        assert_eq!(p.viewer_to_session(0, 0), Some((0, 0)));
        assert_eq!(p.viewer_to_session(79, 23), Some((79, 23)));
        assert_eq!(p.viewer_to_session(80, 0), None);
    }

    #[test]
    fn larger_viewer_letterboxes_centered() {
        // Session 80x24 in a 100x30 viewer: 20 surplus cols (10/10), 6 rows (3/3).
        let p = fit(Size::new(80, 24), Size::new(100, 30));
        assert!(p.is_letterboxed());
        assert!(!p.is_clipped());
        assert_eq!(p.cols.pad_before, 10);
        assert_eq!(p.cols.pad_after, 10);
        assert_eq!(p.rows.pad_before, 3);
        assert_eq!(p.rows.pad_after, 3);
        // Left margin and top margin map to nothing.
        assert_eq!(p.viewer_to_session(9, 15), None);
        assert_eq!(p.viewer_to_session(50, 2), None);
        // First content cell sits at the margin edge.
        assert_eq!(p.viewer_to_session(10, 3), Some((0, 0)));
        assert_eq!(p.viewer_to_session(89, 26), Some((79, 23)));
        // Right / bottom margin map to nothing.
        assert_eq!(p.viewer_to_session(90, 3), None);
        assert_eq!(p.viewer_to_session(10, 27), None);
    }

    #[test]
    fn odd_surplus_biases_the_extra_cell_after() {
        // 1 surplus column: pad_before 0, pad_after 1.
        let p = fit(Size::new(80, 24), Size::new(81, 24));
        assert_eq!(p.cols.pad_before, 0);
        assert_eq!(p.cols.pad_after, 1);
        assert_eq!(p.viewer_to_session(0, 0), Some((0, 0)));
        assert_eq!(p.viewer_to_session(80, 0), None);
    }

    #[test]
    fn smaller_viewer_clips_from_the_origin() {
        // Session 120x40 in an 80x24 viewer: show the top-left 80x24.
        let p = fit(Size::new(120, 40), Size::new(80, 24));
        assert!(p.is_clipped());
        assert!(!p.is_letterboxed());
        assert_eq!(p.cols.len, 80);
        assert_eq!(p.rows.len, 24);
        assert_eq!(p.viewer_to_session(0, 0), Some((0, 0)));
        assert_eq!(p.viewer_to_session(79, 23), Some((79, 23)));
        // Nothing beyond the viewer exists.
        assert_eq!(p.viewer_to_session(80, 0), None);
    }

    #[test]
    fn mixed_axes_clip_one_letterbox_the_other() {
        // Narrower but taller viewer: clip columns, letterbox rows.
        let p = fit(Size::new(100, 20), Size::new(80, 30));
        assert!(p.is_clipped());
        assert!(p.is_letterboxed());
        assert_eq!(p.cols.len, 80); // clipped
        assert_eq!(p.cols.pad_before, 0);
        assert_eq!(p.rows.pad_before, 5); // (30-20)/2
        assert_eq!(p.viewer_to_session(79, 5), Some((79, 0)));
        assert_eq!(p.viewer_to_session(79, 4), None); // top margin
    }
}
