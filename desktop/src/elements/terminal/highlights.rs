//! Where the overlays go: which viewer cells the selection, the search
//! matches, and a hovered link cover in the rows on screen.
//!
//! The element paints these as quads over the cell backgrounds (selection and
//! matches) or under the glyphs (a link's underline). The geometry itself is
//! `shelbi_term`'s ([`Selection::row_span`], [`Link::row_span`]), the same the
//! TUI highlights with and the copy extracts; this maps grid lines to viewer
//! rows through `top` (the grid line on the first row shown: the clipped
//! window's start less the scroll offset, `TerminalView::first_shown_line`)
//! and clips to the columns shown. Plain data, unit-tested without a window.

use shelbi_term::links::Link;
use shelbi_term::search::Match;
use shelbi_term::selection::Selection;
use shelbi_term::Size;

/// A run of cells on one viewer row: `len` cells from `col`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Viewer row.
    pub row: u16,
    /// First column.
    pub col: u16,
    /// Width in cells.
    pub len: u16,
}

/// The grid line shown on viewer `row` when the first row shows line `top`.
fn line_of(row: u16, top: i32) -> i32 {
    top + row as i32
}

/// The cells `sel` covers among the `shown` rows and columns, the first
/// showing grid line `top`. One span per row it touches.
pub fn selection_spans(sel: &Selection, top: i32, shown: Size) -> Vec<Span> {
    (0..shown.rows)
        .filter_map(|row| {
            let (first, last) = sel.row_span(line_of(row, top), shown.cols)?;
            Some(Span { row, col: first, len: last - first + 1 })
        })
        .collect()
}

/// The cells a hovered link covers (for its underline).
pub fn link_spans(link: &Link, top: i32, shown: Size) -> Vec<Span> {
    (0..shown.rows)
        .filter_map(|row| {
            let (first, last) = link.row_span(line_of(row, top), shown.cols)?;
            Some(Span { row, col: first, len: last - first + 1 })
        })
        .collect()
}

/// The search matches on screen, each with whether it is the current one.
pub fn match_spans(matches: &[Match], current: Option<Match>, top: i32, shown: Size) -> Vec<(Span, bool)> {
    matches
        .iter()
        .filter_map(|m| {
            let row = m.line - top;
            if row < 0 || row >= shown.rows as i32 || m.start_col >= shown.cols {
                return None;
            }
            let end = m.end_col.min(shown.cols);
            let span = Span { row: row as u16, col: m.start_col, len: end - m.start_col };
            Some((span, Some(*m) == current))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_term::links::LinkTarget;
    use shelbi_term::selection::{Point, SelectionMode};

    const SHOWN: Size = Size { cols: 10, rows: 4 };

    fn span(row: u16, col: u16, len: u16) -> Span {
        Span { row, col, len }
    }

    #[test]
    fn a_stream_selection_covers_partial_ends_and_whole_middles() {
        let mut sel = Selection::start(Point::new(0, 3));
        sel.update(Point::new(2, 1));
        assert_eq!(
            selection_spans(&sel, 0, SHOWN),
            vec![span(0, 3, 7), span(1, 0, 10), span(2, 0, 2)]
        );
    }

    #[test]
    fn a_backward_selection_paints_the_same_cells() {
        let mut sel = Selection::start(Point::new(2, 1));
        sel.update(Point::new(0, 3));
        assert_eq!(
            selection_spans(&sel, 0, SHOWN),
            vec![span(0, 3, 7), span(1, 0, 10), span(2, 0, 2)]
        );
    }

    #[test]
    fn a_block_selection_is_the_same_columns_on_each_row() {
        let mut sel = Selection::with_mode(Point::new(3, 6), SelectionMode::Block);
        sel.update(Point::new(1, 2));
        assert_eq!(
            selection_spans(&sel, 0, SHOWN),
            vec![span(1, 2, 5), span(2, 2, 5), span(3, 2, 5)]
        );
    }

    #[test]
    fn scrolling_moves_a_history_selection_onto_the_screen() {
        // Grid lines -5..=-4 are history; scrolled up 5, they are rows 0..=1.
        let mut sel = Selection::start(Point::new(-5, 0));
        sel.update(Point::new(-4, 4));
        assert!(selection_spans(&sel, 0, SHOWN).is_empty(), "off screen at the bottom");
        assert_eq!(selection_spans(&sel, -5, SHOWN), vec![span(0, 0, 10), span(1, 0, 5)]);
    }

    #[test]
    fn spans_clip_to_the_shown_columns() {
        // The viewer shows 10 columns of a wider session (clipped).
        let mut sel = Selection::start(Point::new(0, 8));
        sel.update(Point::new(0, 30));
        assert_eq!(selection_spans(&sel, 0, SHOWN), vec![span(0, 8, 2)]);
        let mut beyond = Selection::start(Point::new(0, 12));
        beyond.update(Point::new(0, 20));
        assert!(selection_spans(&beyond, 0, SHOWN).is_empty());
    }

    #[test]
    fn a_wrapped_link_is_underlined_on_both_rows() {
        let link = Link {
            target: LinkTarget::Url("https://x".into()),
            start: Point::new(0, 6),
            end: Point::new(1, 2),
        };
        assert_eq!(link_spans(&link, 0, SHOWN), vec![span(0, 6, 4), span(1, 0, 3)]);
    }

    #[test]
    fn matches_on_screen_with_the_current_one_marked() {
        let m = |line, start_col, end_col| Match { line, start_col, end_col };
        let matches = [m(-1, 0, 3), m(0, 2, 5), m(3, 8, 12)];
        assert_eq!(
            match_spans(&matches, Some(m(0, 2, 5)), 0, SHOWN),
            vec![(span(0, 2, 3), true), (span(3, 8, 2), false)]
        );
        // Scrolled up one: the history match shows on row 0.
        assert_eq!(match_spans(&matches, None, -1, SHOWN)[0], (span(0, 0, 3), false));
    }

    #[test]
    fn a_window_clipped_to_the_bottom_shifts_every_overlay_up() {
        // A taller session's window starting at grid line 3
        // (dt-terminal-bottom-clipped): line 3 is viewer row 0.
        let mut sel = Selection::start(Point::new(3, 1));
        sel.update(Point::new(4, 2));
        assert_eq!(selection_spans(&sel, 3, SHOWN), vec![span(0, 1, 9), span(1, 0, 3)]);
        let link = Link {
            target: LinkTarget::Url("https://x".into()),
            start: Point::new(5, 0),
            end: Point::new(5, 4),
        };
        assert_eq!(link_spans(&link, 3, SHOWN), vec![span(2, 0, 5)]);
        let m = |line, start_col, end_col| Match { line, start_col, end_col };
        assert_eq!(
            match_spans(&[m(2, 0, 3), m(6, 1, 2)], None, 3, SHOWN),
            vec![(span(3, 1, 1), false)],
            "line 2 is above the window"
        );
    }
}
