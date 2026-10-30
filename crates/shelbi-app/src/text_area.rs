//! A multi-line text buffer with a caret, toolkit-free: the reject reason's
//! editor ([`overlay::review_reject`](crate::overlay::review_reject)) and the
//! TUI's in-place Settings editors (the shared agent instructions, a new
//! agent's name) type into it.
//!
//! The text is kept exactly as given: [`TextArea::from_text`] splits on `\n`
//! only, so [`TextArea::text`] gives back the same string (a trailing newline,
//! tabs and `\r` included). Only pasted text has its line endings normalized
//! ([`TextArea::insert_str`]), since a terminal pastes `\r` for a line break.
//!
//! Long lines soft-wrap at the box's width ([`Wrap`]): at word boundaries,
//! a word wider than the box broken where it overflows. Wrapping is only
//! how the lines are laid out ([`TextArea::wrapped`]); it never adds a
//! newline to the text. Once the host has told the buffer how it wraps
//! ([`TextArea::set_wrap`]), ↑ / ↓ and Page Up / Page Down move by
//! wrapped (visual) row and [`TextArea::place`] puts the caret where a
//! click landed on one.

use std::fmt;
use std::sync::Arc;

use shelbi_state::keymap::{Key, KeyChord, Mods};

/// How a text area soft-wraps: the room across a row and each char's
/// advance across it, in the host's unit (cells in a terminal, points in a
/// window).
#[derive(Clone)]
pub struct Wrap {
    width: f32,
    advance: Arc<dyn Fn(char) -> f32 + Send + Sync>,
}

impl Wrap {
    pub fn new(width: f32, advance: impl Fn(char) -> f32 + Send + Sync + 'static) -> Self {
        Wrap {
            width,
            advance: Arc::new(advance),
        }
    }

    /// No wrapping: a row per line, every char one cell.
    fn none() -> Self {
        Wrap::new(f32::INFINITY, |_| 1.0)
    }

    /// Rows `width` cells wide, every char one cell.
    pub fn cells(width: usize) -> Self {
        Wrap::new(width.max(1) as f32, |_| 1.0)
    }

    pub fn width(&self) -> f32 {
        self.width
    }

    pub fn advance(&self, c: char) -> f32 {
        (self.advance)(c)
    }

    /// How far across `chars` reach.
    pub fn measure(&self, chars: &[char]) -> f32 {
        chars.iter().map(|&c| self.advance(c)).sum()
    }

    /// Where `line`'s wrapped rows start (the first at 0). A row breaks
    /// after the last whitespace that keeps it in the room; a word wider
    /// than the room breaks where it overflows. Whitespace never starts a
    /// row: spaces at a break hang past the edge, at the end of the row
    /// they follow. A line whose last row leaves no room for the caret
    /// after it gets an empty row at its end, where that caret sits.
    pub fn line_starts(&self, line: &[char]) -> Vec<usize> {
        let mut starts = vec![0];
        let mut start = 0;
        let mut x = 0.0;
        // Where a row may start: just after whitespace.
        let mut brk: Option<usize> = None;
        let mut i = 0;
        while i < line.len() {
            let c = line[i];
            let w = self.advance(c);
            if c.is_whitespace() {
                x += w;
                i += 1;
                brk = Some(i);
                continue;
            }
            if x + w > self.width && i > start {
                start = match brk {
                    Some(b) if b > start => b,
                    _ => i,
                };
                starts.push(start);
                x = self.measure(&line[start..i]);
                brk = None;
                // `c` again, on the new row.
                continue;
            }
            x += w;
            i += 1;
        }
        if !line.is_empty() && x + self.advance(' ') > self.width {
            starts.push(line.len());
        }
        starts
    }
}

impl fmt::Debug for Wrap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Wrap").field("width", &self.width).finish_non_exhaustive()
    }
}

/// One wrapped (visual) row: chars `start..end` of line `line`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VisualRow {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

/// A minimal multi-line buffer with a caret. Lines are `Vec<char>` so the
/// caret column is a plain index; there is always at least one line.
#[derive(Debug, Clone)]
pub struct TextArea {
    pub(crate) lines: Vec<Vec<char>>,
    pub(crate) row: usize,
    pub(crate) col: usize,
    /// How the host lays the lines out, once it has said: ↑ / ↓ then move
    /// by wrapped row.
    wrap: Option<Wrap>,
}

impl Default for TextArea {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
            wrap: None,
        }
    }
}

/// Two buffers are equal when their text and caret are: how they wrap is
/// the host's layout.
impl PartialEq for TextArea {
    fn eq(&self, other: &Self) -> bool {
        self.lines == other.lines && self.row == other.row && self.col == other.col
    }
}

impl Eq for TextArea {}

impl TextArea {
    /// A buffer holding `text`, the caret at its start.
    pub fn from_text(text: &str) -> Self {
        TextArea {
            lines: text.split('\n').map(|l| l.chars().collect()).collect(),
            row: 0,
            col: 0,
            wrap: None,
        }
    }

    /// The lines, for rendering.
    pub fn lines(&self) -> &[Vec<char>] {
        &self.lines
    }

    /// The caret as `(row, col)`.
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.iter().collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn is_blank(&self) -> bool {
        self.text().trim().is_empty()
    }

    pub fn insert(&mut self, c: char) {
        self.lines[self.row].insert(self.col, c);
        self.col += 1;
    }

    /// Insert pasted `text` at the caret: `\r\n` and a lone `\r` (how a
    /// terminal pastes a line break) become line breaks.
    pub fn insert_str(&mut self, text: &str) {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        for c in text.chars() {
            if c == '\n' {
                self.newline();
            } else {
                self.insert(c);
            }
        }
    }

    pub fn newline(&mut self) {
        let tail = self.lines[self.row].split_off(self.col);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        if self.col > 0 {
            self.lines[self.row].remove(self.col - 1);
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.lines[self.row].len();
            self.lines[self.row].extend(cur);
        }
    }

    pub fn delete(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.lines[self.row].remove(self.col);
        } else if self.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.row + 1);
            self.lines[self.row].extend(next);
        }
    }

    pub fn move_left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.lines[self.row].len();
        }
    }

    pub fn move_right(&mut self) {
        if self.col < self.lines[self.row].len() {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }

    /// Up a row: a wrapped row once the host has said how it wraps, else a
    /// line. From the first row, to the start.
    pub fn move_up(&mut self) {
        if self.wrap.is_some() {
            return self.move_visual(false, 1);
        }
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.col = 0;
        }
    }

    /// Down a row (wrapped, as [`TextArea::move_up`]). From the last row,
    /// to the end.
    pub fn move_down(&mut self) {
        if self.wrap.is_some() {
            return self.move_visual(true, 1);
        }
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.col = self.lines[self.row].len();
        }
    }

    /// The caret to the start of its line.
    pub fn home(&mut self) {
        self.col = 0;
    }

    /// The caret to the end of its line.
    pub fn end(&mut self) {
        self.col = self.lines[self.row].len();
    }

    /// The caret `n` rows up or down (wrapped, as [`TextArea::move_up`]),
    /// stopping at the first or last.
    pub fn move_lines(&mut self, down: bool, n: usize) {
        if let Some(wrap) = self.wrap.clone() {
            let rows = self.wrapped_with(&wrap);
            let (v, _) = self.caret_row(&rows);
            let to = if down { (v + n).min(rows.len() - 1) } else { v.saturating_sub(n) };
            let x = self.caret_x(&rows, &wrap);
            return self.place_with(&rows, to, x, &wrap);
        }
        self.row = if down {
            (self.row + n).min(self.lines.len() - 1)
        } else {
            self.row.saturating_sub(n)
        };
        self.col = self.col.min(self.lines[self.row].len());
    }

    /// Put the caret at `(row, col)`, clamped into the text.
    pub fn move_to(&mut self, row: usize, col: usize) {
        self.row = row.min(self.lines.len() - 1);
        self.col = col.min(self.lines[self.row].len());
    }

    /// Apply an editing key: a character (no Control or Command held), Enter
    /// (a line break unless `single_line`), Backspace, Delete, the arrows,
    /// Home / End and Page Up / Page Down (`page` lines). Returns whether
    /// the key was one; the caller handles any other (Esc, Tab, a save
    /// chord).
    pub fn edit(&mut self, chord: KeyChord, single_line: bool, page: usize) -> bool {
        let command = chord.mods.contains(Mods::CONTROL) || chord.mods.contains(Mods::SUPER);
        match chord.code {
            Key::Char(c) if !command => self.insert(c),
            Key::Enter if !single_line && !command => self.newline(),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Left => self.move_left(),
            Key::Right => self.move_right(),
            Key::Up if !single_line => self.move_up(),
            Key::Down if !single_line => self.move_down(),
            Key::Home => self.home(),
            Key::End => self.end(),
            Key::PageUp if !single_line => self.move_lines(false, page.max(1)),
            Key::PageDown if !single_line => self.move_lines(true, page.max(1)),
            _ => return false,
        }
        true
    }

    /// Lay the lines out as `wrap` says from here on: ↑ / ↓ and Page Up /
    /// Page Down move by wrapped row. The host sets it when it learns the
    /// box's width (each draw, in a terminal); `None` moves by line again.
    pub fn set_wrap(&mut self, wrap: Option<Wrap>) {
        self.wrap = wrap;
    }

    pub fn wrap(&self) -> Option<&Wrap> {
        self.wrap.as_ref()
    }

    /// The lines soft-wrapped as `wrap` says, and the caret's place on them
    /// as `(row, column in the row)`.
    pub fn wrapped(&self, wrap: &Wrap) -> (Vec<VisualRow>, (usize, usize)) {
        let rows = self.wrapped_with(wrap);
        let caret = self.caret_row(&rows);
        (rows, caret)
    }

    /// [`TextArea::wrapped`] with the wrap set by [`TextArea::set_wrap`]: a
    /// row per line without one.
    pub fn layout(&self) -> (Vec<VisualRow>, (usize, usize)) {
        self.wrapped(self.wrap.as_ref().unwrap_or(&Wrap::none()))
    }

    fn wrapped_with(&self, wrap: &Wrap) -> Vec<VisualRow> {
        let mut rows = Vec::new();
        for (i, line) in self.lines.iter().enumerate() {
            let starts = wrap.line_starts(line);
            for (k, &start) in starts.iter().enumerate() {
                let end = starts.get(k + 1).copied().unwrap_or(line.len());
                rows.push(VisualRow { line: i, start, end });
            }
        }
        rows
    }

    /// The caret's `(row, column in the row)` on `rows`: a caret at a break
    /// sits at the start of the row after it.
    fn caret_row(&self, rows: &[VisualRow]) -> (usize, usize) {
        let mut at = (0, 0);
        for (v, r) in rows.iter().enumerate() {
            if r.line == self.row && r.start <= self.col {
                at = (v, self.col - r.start);
            }
        }
        at
    }

    fn caret_x(&self, rows: &[VisualRow], wrap: &Wrap) -> f32 {
        let (v, _) = self.caret_row(rows);
        let r = rows[v];
        wrap.measure(&self.lines[r.line][r.start..self.col])
    }

    fn move_visual(&mut self, down: bool, n: usize) {
        let Some(wrap) = self.wrap.clone() else {
            return;
        };
        let rows = self.wrapped_with(&wrap);
        let (v, _) = self.caret_row(&rows);
        if !down && v == 0 {
            self.row = 0;
            self.col = 0;
        } else if down && v + 1 >= rows.len() {
            self.row = self.lines.len() - 1;
            self.col = self.lines[self.row].len();
        } else {
            let x = self.caret_x(&rows, &wrap);
            let to = if down { v + n } else { v - n.min(v) };
            self.place_with(&rows, to.min(rows.len() - 1), x, &wrap);
        }
    }

    /// Put the caret on wrapped row `row` (of [`TextArea::wrapped`] with the
    /// wrap set by [`TextArea::set_wrap`], or one row per line without one)
    /// at the char boundary nearest `x` across it: where a click landed.
    pub fn place(&mut self, row: usize, x: f32) {
        let wrap = self.wrap.clone().unwrap_or_else(Wrap::none);
        let rows = self.wrapped_with(&wrap);
        self.place_with(&rows, row.min(rows.len() - 1), x, &wrap);
    }

    fn place_with(&mut self, rows: &[VisualRow], v: usize, x: f32, wrap: &Wrap) {
        let r = rows[v];
        // A row that wraps ends before its last boundary: that one is the
        // next row's start.
        let wraps = rows.get(v + 1).is_some_and(|n| n.line == r.line);
        let last = if wraps { r.end.saturating_sub(1).max(r.start) } else { r.end };
        let line = &self.lines[r.line];
        let (mut col, mut at, mut best) = (r.start, 0.0, f32::MAX);
        for c in r.start..=last {
            if (at - x).abs() < best {
                best = (at - x).abs();
                col = c;
            }
            if let Some(&ch) = line.get(c) {
                at += wrap.advance(ch);
            }
        }
        self.row = r.line;
        self.col = col;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(code: Key) -> KeyChord {
        KeyChord { code, mods: Mods::NONE }
    }

    #[test]
    fn text_round_trips_exactly() {
        for text in ["", "one", "a\n", "# T\n\n- x\n\tcode\r\nend\n\n"] {
            assert_eq!(TextArea::from_text(text).text(), text);
        }
    }

    #[test]
    fn a_paste_normalizes_line_breaks() {
        let mut t = TextArea::from_text("ab");
        t.end();
        t.insert_str("1\r2\r\n3");
        assert_eq!(t.text(), "ab1\n2\n3");
        assert_eq!(t.cursor(), (2, 1));
    }

    #[test]
    fn editing_keys_edit_and_others_pass_through() {
        let mut t = TextArea::default();
        for c in "hi".chars() {
            assert!(t.edit(chord(Key::Char(c)), false, 5));
        }
        assert!(t.edit(chord(Key::Enter), false, 5));
        assert!(!t.edit(chord(Key::Esc), false, 5));
        assert!(!t.edit(KeyChord { code: Key::Char('s'), mods: Mods::CONTROL }, false, 5));
        assert_eq!(t.text(), "hi\n");
        let mut name = TextArea::default();
        assert!(!name.edit(chord(Key::Enter), true, 5), "Enter is the caller's on one line");
        assert!(t.edit(chord(Key::PageUp), false, 5));
        assert_eq!(t.cursor(), (0, 0));
    }

    fn starts(t: &TextArea, wrap: &Wrap) -> Vec<(usize, usize)> {
        t.wrapped(wrap).0.iter().map(|r| (r.line, r.start)).collect()
    }

    fn row_text(t: &TextArea, r: VisualRow) -> String {
        t.lines()[r.line][r.start..r.end].iter().collect()
    }

    #[test]
    fn wrapping_tracks_the_caret() {
        let four = Wrap::cells(4);
        let mut t = TextArea::from_text("abcdef\n\nxy");
        assert_eq!(starts(&t, &four), [(0, 0), (0, 4), (1, 0), (2, 0)]);
        t.move_to(0, 5);
        assert_eq!(t.wrapped(&four).1, (1, 1));
        t.move_to(0, 4);
        assert_eq!(t.wrapped(&four).1, (1, 0));
        let full = TextArea::from_text("abcd");
        // A full row: the caret at its end sits on the next (empty) row.
        assert_eq!(starts(&full, &four), [(0, 0), (0, 4)]);
        t.move_to(2, 2);
        assert_eq!(t.wrapped(&four).1, (3, 2));
    }

    #[test]
    fn lines_wrap_at_word_boundaries() {
        let t = TextArea::from_text("Logo is bigger than figma. Buttons are wrong.");
        let (rows, _) = t.wrapped(&Wrap::cells(20));
        let shown: Vec<String> = rows.iter().map(|&r| row_text(&t, r)).collect();
        assert_eq!(shown, ["Logo is bigger than ", "figma. Buttons are ", "wrong."]);
        // Wrapping is layout only: the text keeps its one line.
        assert_eq!(t.text(), "Logo is bigger than figma. Buttons are wrong.");
        assert_eq!(t.lines().len(), 1);
    }

    #[test]
    fn a_word_wider_than_the_box_breaks_where_it_overflows() {
        let t = TextArea::from_text("ab abcdefghij cd");
        let (rows, _) = t.wrapped(&Wrap::cells(5));
        let shown: Vec<String> = rows.iter().map(|&r| row_text(&t, r)).collect();
        assert_eq!(shown, ["ab ", "abcde", "fghij ", "cd"]);
    }

    #[test]
    fn wrapping_measures_with_the_hosts_advances() {
        // A wide `W`: the 11 chars fit 14 cells, not 14 points.
        let wrap = Wrap::new(14.0, |c| if c == 'W' { 3.0 } else { 1.0 });
        assert_eq!(TextArea::from_text("WWW iii WWW").wrapped(&Wrap::cells(14)).0.len(), 1);
        let t = TextArea::from_text("WWW iii WWW");
        let (rows, _) = t.wrapped(&wrap);
        let shown: Vec<String> = rows.iter().map(|&r| row_text(&t, r)).collect();
        assert_eq!(shown, ["WWW iii ", "WWW"]);
    }

    #[test]
    fn up_and_down_move_by_wrapped_row() {
        let mut t = TextArea::from_text("aaaa bbbb cccc\nxy");
        t.set_wrap(Some(Wrap::cells(5)));
        // Rows: "aaaa " "bbbb " "cccc" "xy".
        t.move_to(0, 2);
        t.move_down();
        assert_eq!(t.cursor(), (0, 7), "same column, next wrapped row");
        assert_eq!(t.wrapped(&Wrap::cells(5)).1, (1, 2));
        t.move_down();
        assert_eq!(t.cursor(), (0, 12));
        t.move_down();
        assert_eq!(t.cursor(), (1, 2), "then the next line, column clamped");
        t.move_down();
        assert_eq!(t.cursor(), (1, 2), "the last row goes to the end");
        t.move_up();
        t.move_up();
        assert_eq!(t.cursor(), (0, 7));
        t.move_lines(false, 5);
        assert_eq!(t.cursor(), (0, 2), "Page Up stops on the first row");
        t.move_up();
        assert_eq!(t.cursor(), (0, 0), "up from the first row goes to the start");
        // A wrapped row's end is the next row's start: down from the end of
        // "aaaa " stays on "bbbb ", before its trailing space.
        t.move_to(0, 4);
        t.move_down();
        assert_eq!(t.cursor(), (0, 9));
        assert_eq!(t.text(), "aaaa bbbb cccc\nxy");
    }

    #[test]
    fn without_a_wrap_up_and_down_move_by_line() {
        let mut t = TextArea::from_text("aaaa bbbb cccc\nxy");
        t.move_to(0, 2);
        t.move_down();
        assert_eq!(t.cursor(), (1, 2));
    }

    #[test]
    fn a_click_places_the_caret_on_its_wrapped_row() {
        let mut t = TextArea::from_text("aaaa bbbb cccc\nxy");
        t.set_wrap(Some(Wrap::cells(5)));
        t.place(1, 2.0);
        assert_eq!(t.cursor(), (0, 7));
        t.place(1, 40.0);
        assert_eq!(t.cursor(), (0, 9), "past a wrapped row's end: before the break");
        t.place(2, 40.0);
        assert_eq!(t.cursor(), (0, 14), "past a line's last row: its end");
        t.place(9, 0.4);
        assert_eq!(t.cursor(), (1, 0), "below the text: the last row");
        let mut plain = TextArea::from_text("abc\ndef");
        plain.place(1, 1.6);
        assert_eq!(plain.cursor(), (1, 2), "no wrap: a row per line");
    }
}
