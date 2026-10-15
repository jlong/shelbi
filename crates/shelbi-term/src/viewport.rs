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
//!   Columns anchor at the left edge. Rows anchor at the bottom of the
//!   session's content ([`fit_showing`]): the window ends at the lowest row
//!   that holds anything (or the cursor), and never hides a shown cursor, so
//!   a program that draws to the bottom of the screen (an agent's input
//!   prompt and status line) stays in view, while a fresh shell whose prompt
//!   sits at the top still shows the top. [`fit`] is the plain top-left
//!   anchor, for callers with no screen to look at.
//!
//! Anchoring at the top hid the bottom rows of a session sized by a taller
//! client: they are live rows, not scrollback, so no amount of scrolling
//! reached them (`dt-terminal-bottom-clipped`).
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
    /// First session index shown. 0 when letterboxing; when clipping, where
    /// the window starts (0 for [`fit`], the content anchor for
    /// [`fit_showing`]).
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

    /// Like [`Axis::fit`], but a clipped window starts at `start` (clamped so
    /// it stays inside the session).
    fn fit_from(session: u16, viewer: u16, start: u16) -> Axis {
        let mut axis = Axis::fit(session, viewer);
        if axis.len < session {
            axis.src_start = start.min(session - axis.len);
        }
        axis
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

    /// Map a session index on this axis to the viewer index showing it, or
    /// `None` if it is outside a clipped window.
    fn unmap(&self, session_index: u16) -> Option<u16> {
        let rel = session_index.checked_sub(self.src_start)?;
        (rel < self.len).then_some(self.pad_before + rel)
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

    /// Translate a session cell `(col, row)` to the viewer cell showing it, or
    /// `None` if it is outside a clipped window. The inverse of
    /// [`Placement::viewer_to_session`]; what a renderer paints through.
    pub fn session_to_viewer(&self, col: u16, row: u16) -> Option<(u16, u16)> {
        Some((self.cols.unmap(col)?, self.rows.unmap(row)?))
    }
}

/// Compute how `session` content is placed inside a `viewer`, a clipped
/// window anchored at the session's top-left. Viewers with a screen to look
/// at use [`fit_showing`] (via `TerminalView::placement`) instead.
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

/// Compute how `session` content is placed inside a `viewer`, keeping the
/// session's content in view when the viewer has fewer rows: the window ends
/// at `content_bottom` (the lowest live-screen row holding anything) and
/// starts no lower than `cursor` (when the cursor is shown), so neither a
/// prompt at the bottom of the screen nor the cursor is cut off. Columns still
/// clip from the left; letterboxing is as [`fit`].
pub fn fit_showing(session: Size, viewer: Size, content_bottom: u16, cursor: Option<u16>) -> Placement {
    let session = session.non_zero();
    let viewer = viewer.non_zero();
    let mut start = content_bottom.saturating_add(1).saturating_sub(viewer.rows);
    if let Some(cursor) = cursor {
        start = start.min(cursor);
    }
    Placement {
        session,
        viewer,
        cols: Axis::fit(session.cols, viewer.cols),
        rows: Axis::fit_from(session.rows, viewer.rows, start),
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

    #[test]
    fn a_taller_session_shows_its_bottom_rows() {
        // dt-terminal-bottom-clipped: a 60-row session (sized by a taller
        // client) in a 40-row viewer, with an agent's prompt and status line
        // on its last rows. The window ends at the content's last row.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 59, Some(57));
        assert!(p.is_clipped());
        assert_eq!(p.rows.src_start, 20);
        assert_eq!(p.rows.len, 40);
        assert_eq!(p.viewer_to_session(0, 39), Some((0, 59)), "the last viewer row shows the last session row");
        assert_eq!(p.viewer_to_session(0, 0), Some((0, 20)));
        assert_eq!(p.session_to_viewer(0, 59), Some((0, 39)));
        assert_eq!(p.session_to_viewer(0, 19), None, "above the window");
    }

    #[test]
    fn a_short_screen_of_content_keeps_the_top() {
        // A fresh shell: the prompt on row 0, nothing below. Showing the
        // (blank) bottom would hide the only thing on screen.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 0, Some(0));
        assert_eq!(p.rows.src_start, 0);
        // Content that fits the window from the top keeps the top too.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 39, Some(39));
        assert_eq!(p.rows.src_start, 0);
        // One row more and the window follows it down.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 40, Some(40));
        assert_eq!(p.rows.src_start, 1);
    }

    #[test]
    fn the_cursor_stays_in_view() {
        // A full-screen program with its cursor near the top: the window
        // starts at the cursor rather than cutting it off.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 59, Some(5));
        assert_eq!(p.rows.src_start, 5);
        // A hidden cursor doesn't pull the window up.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 59, None);
        assert_eq!(p.rows.src_start, 20);
    }

    #[test]
    fn the_anchor_is_clamped_inside_the_session() {
        // A content row past the session (a stale anchor) still leaves a
        // full window inside it.
        let p = fit_showing(Size::new(80, 60), Size::new(80, 40), 500, None);
        assert_eq!(p.rows.src_start, 20);
        assert_eq!(p.viewer_to_session(0, 39), Some((0, 59)));
    }

    #[test]
    fn showing_letterboxes_like_fit() {
        // The viewer is taller: nothing clips, the anchor is irrelevant.
        let session = Size::new(100, 20);
        let viewer = Size::new(80, 30);
        assert_eq!(fit_showing(session, viewer, 19, Some(19)), fit(session, viewer));
        assert_eq!(fit_showing(session, viewer, 0, None).session_to_viewer(0, 0), Some((0, 5)));
    }

    #[test]
    fn session_to_viewer_inverts_viewer_to_session() {
        for p in [
            fit(Size::new(80, 24), Size::new(100, 30)),
            fit(Size::new(120, 40), Size::new(80, 24)),
            fit_showing(Size::new(120, 40), Size::new(80, 24), 39, Some(30)),
        ] {
            for row in 0..p.viewer.rows {
                for col in 0..p.viewer.cols {
                    if let Some((c, r)) = p.viewer_to_session(col, row) {
                        assert_eq!(p.session_to_viewer(c, r), Some((col, row)));
                    }
                }
            }
        }
    }
}
