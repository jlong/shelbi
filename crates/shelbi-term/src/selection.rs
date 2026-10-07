//! Text selection and copy.
//!
//! Selection is Shelbi's own, driven by Shift+drag (and plain drag when the
//! program has not asked for the mouse — see [`crate::input`]). It exists only
//! on the **normal** screen; [`crate::view::TerminalView`] enforces that. This
//! module is the toolkit-neutral model: a start/end region in grid
//! coordinates, text extraction from the emulator grid, and the OSC 52 copy
//! encoder.
//!
//! Copy uses OSC 52 ([`osc52_copy`]) so it works over SSH and through an outer
//! tmux or Screen. The actual clipboard write — OSC 52 to the outer terminal,
//! or a native clipboard crate when running locally — stays the UI's job; this
//! crate only produces the escape sequence.
//!
//! ## Coordinates
//!
//! A [`Point`] is in `alacritty_terminal` grid-line coordinates: `line` 0 is
//! the top of the visible screen, negative lines are scrollback history, and
//! `line` up to `rows - 1` is the bottom of the screen. This is independent of
//! the current scroll offset, so a selection stays anchored to its text as the
//! view scrolls. A UI converts a viewer cell to a [`Point`] with
//! [`crate::view::TerminalView::viewer_point_to_grid`].

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};

/// A cell position in grid-line coordinates (see the module docs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Point {
    /// Grid line: `0` top of screen, negative into history.
    pub line: i32,
    /// Column.
    pub col: u16,
}

impl Point {
    /// A new point.
    pub fn new(line: i32, col: u16) -> Self {
        Self { line, col }
    }
}

impl PartialOrd for Point {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Point {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.line.cmp(&other.line).then(self.col.cmp(&other.col))
    }
}

/// An in-progress or settled text selection: an anchor (where the drag began)
/// and a focus (where it is now). A single click with no drag leaves both
/// equal, which selects one cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    anchor: Point,
    focus: Point,
}

impl Selection {
    /// Begin a selection at `anchor`.
    pub fn start(anchor: Point) -> Self {
        Self { anchor, focus: anchor }
    }

    /// Extend the selection to `focus` (a drag).
    pub fn update(&mut self, focus: Point) {
        self.focus = focus;
    }

    /// The selection range normalized so start precedes end.
    pub fn range(&self) -> (Point, Point) {
        if self.anchor <= self.focus {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Whether the selection covers nothing wider than a single cell.
    pub fn is_empty(&self) -> bool {
        self.anchor == self.focus
    }

    /// Whether the cell at grid `line`/`col` falls inside the (stream)
    /// selection. Mirrors [`extract_stream`]'s geometry so the on-screen
    /// highlight matches exactly what a copy would yield: the start line is
    /// selected from `start.col` on, the end line up to `end.col`, and every
    /// line in between is selected whole.
    pub fn contains(&self, line: i32, col: u16) -> bool {
        let (start, end) = self.range();
        if line < start.line || line > end.line {
            return false;
        }
        let after_start = line > start.line || col >= start.col;
        let before_end = line < end.line || col <= end.col;
        after_start && before_end
    }

    /// Extract the selected text from `grid` as a stream selection (the whole
    /// run from start to end, not a rectangle). Lines that the emulator marked
    /// as wrapped are joined without a newline, matching `capture-pane -J`;
    /// other line breaks become `\n`, with trailing whitespace trimmed.
    pub fn extract_text(&self, grid: &Grid<Cell>) -> String {
        let (start, end) = self.range();
        extract_stream(grid, start, end)
    }
}

/// Extract a stream selection from `start` to `end` (inclusive) out of `grid`.
pub(crate) fn extract_stream(grid: &Grid<Cell>, start: Point, end: Point) -> String {
    let cols = grid.columns() as u16;
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32 - 1;

    let start_line = start.line.clamp(top, bottom);
    let end_line = end.line.clamp(top, bottom);
    if start_line > end_line {
        return String::new();
    }

    let mut out = String::new();
    for line in start_line..=end_line {
        let first_col = if line == start_line { start.col } else { 0 };
        let last_col = if line == end_line { end.col } else { cols.saturating_sub(1) };
        let (text, wrapped) = row_text(grid, line, first_col, last_col, cols);
        out.push_str(&text);
        if line != end_line && !wrapped {
            out.push('\n');
        }
    }
    out
}

/// The text of one grid row between `first_col` and `last_col` (inclusive),
/// right-trimmed, plus whether the row wraps into the next one.
pub(crate) fn row_text(
    grid: &Grid<Cell>,
    line: i32,
    first_col: u16,
    last_col: u16,
    cols: u16,
) -> (String, bool) {
    let row = &grid[Line(line)];
    let last_col = last_col.min(cols.saturating_sub(1));
    let wrapped = cols > 0
        && row[Column((cols - 1) as usize)].flags.contains(Flags::WRAPLINE);

    let mut text = String::new();
    for c in first_col..=last_col {
        let cell = &row[Column(c as usize)];
        // Skip the blank spacer cells that follow (or lead) a wide character;
        // the wide character itself already carries the glyph.
        if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
            continue;
        }
        text.push(cell.c);
    }
    // A wrapped row is a continuation, so its trailing cells are real; only
    // trim unwrapped rows (true end-of-line padding).
    if !wrapped {
        let trimmed = text.trim_end_matches(' ');
        text.truncate(trimmed.len());
    }
    (text, wrapped)
}

/// Encode `text` as an OSC 52 clipboard-set sequence (`ESC ] 52 ; c ; <base64>
/// ESC \`). Any embedded sequence terminator in the payload is harmless because
/// the content is base64-encoded first.
pub fn osc52_copy(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 16);
    out.extend_from_slice(b"\x1b]52;c;");
    base64_encode_into(text.as_bytes(), &mut out);
    out.extend_from_slice(b"\x1b\\");
    out
}

/// Standard base64 (RFC 4648) with padding, appended to `out`.
fn base64_encode_into(input: &[u8], out: &mut Vec<u8>) {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize]);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize]);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 0x3f) as usize]);
        } else {
            out.push(b'=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 0x3f) as usize]);
        } else {
            out.push(b'=');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;

    #[test]
    fn range_normalizes_regardless_of_drag_direction() {
        let mut s = Selection::start(Point::new(5, 10));
        s.update(Point::new(2, 3));
        assert_eq!(s.range(), (Point::new(2, 3), Point::new(5, 10)));
    }

    #[test]
    fn single_click_is_empty() {
        assert!(Selection::start(Point::new(0, 0)).is_empty());
    }

    #[test]
    fn contains_matches_stream_geometry() {
        // A selection from (0,2) to (2,3): partial first and last lines, whole
        // middle line.
        let mut s = Selection::start(Point::new(0, 2));
        s.update(Point::new(2, 3));
        // First line: only from column 2 on.
        assert!(!s.contains(0, 1));
        assert!(s.contains(0, 2));
        assert!(s.contains(0, 99));
        // Middle line: every column.
        assert!(s.contains(1, 0));
        assert!(s.contains(1, 99));
        // Last line: only up to column 3.
        assert!(s.contains(2, 3));
        assert!(!s.contains(2, 4));
        // Outside the line range.
        assert!(!s.contains(-1, 2));
        assert!(!s.contains(3, 0));
    }

    #[test]
    fn contains_is_direction_independent() {
        // A drag the other way selects the same cells.
        let mut s = Selection::start(Point::new(2, 3));
        s.update(Point::new(0, 2));
        assert!(s.contains(0, 2));
        assert!(s.contains(1, 50));
        assert!(s.contains(2, 3));
        assert!(!s.contains(2, 4));
    }

    #[test]
    fn single_cell_selection_contains_only_itself() {
        let s = Selection::start(Point::new(1, 5));
        assert!(s.contains(1, 5));
        assert!(!s.contains(1, 4));
        assert!(!s.contains(1, 6));
        assert!(!s.contains(0, 5));
    }

    #[test]
    fn extract_single_line() {
        let term = mock_term("hello world");
        let mut s = Selection::start(Point::new(0, 0));
        s.update(Point::new(0, 4));
        assert_eq!(s.extract_text(term.grid()), "hello");
    }

    #[test]
    fn extract_multi_line_joins_with_newline() {
        // `mock_term` treats `\r\n` as a hard line break (no WRAPLINE).
        let term = mock_term("line one\r\nline two");
        let mut s = Selection::start(Point::new(0, 0));
        s.update(Point::new(1, 7));
        assert_eq!(s.extract_text(term.grid()), "line one\nline two");
    }

    #[test]
    fn wrapped_rows_join_without_a_newline() {
        // `mock_term` treats a plain `\n` as a wrapped continuation: row 0
        // "abcd" carries WRAPLINE into row 1 "ef" in this 4-column terminal.
        let term = mock_term("abcd\nef");
        let mut s = Selection::start(Point::new(0, 0));
        s.update(Point::new(1, 1));
        assert_eq!(s.extract_text(term.grid()), "abcdef");
    }

    #[test]
    fn base64_matches_known_vectors() {
        let mut v = Vec::new();
        base64_encode_into(b"", &mut v);
        assert_eq!(v, b"");
        v.clear();
        base64_encode_into(b"f", &mut v);
        assert_eq!(&v, b"Zg==");
        v.clear();
        base64_encode_into(b"fo", &mut v);
        assert_eq!(&v, b"Zm8=");
        v.clear();
        base64_encode_into(b"foo", &mut v);
        assert_eq!(&v, b"Zm9v");
        v.clear();
        base64_encode_into(b"foobar", &mut v);
        assert_eq!(&v, b"Zm9vYmFy");
    }

    #[test]
    fn osc52_wraps_base64_payload() {
        let seq = osc52_copy("foo");
        assert_eq!(seq, b"\x1b]52;c;Zm9v\x1b\\");
    }
}
