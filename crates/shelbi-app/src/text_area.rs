//! A multi-line text buffer with a caret, toolkit-free: the reject reason's
//! editor ([`overlay::review_reject`](crate::overlay::review_reject)) and the
//! TUI's in-place Settings editors (the shared agent instructions, a new
//! agent's name) type into it.
//!
//! The text is kept exactly as given: [`TextArea::from_text`] splits on `\n`
//! only, so [`TextArea::text`] gives back the same string (a trailing newline,
//! tabs and `\r` included). Only pasted text has its line endings normalized
//! ([`TextArea::insert_str`]), since a terminal pastes `\r` for a line break.

use shelbi_state::keymap::{Key, KeyChord, Mods};

/// A minimal multi-line buffer with a caret. Lines are `Vec<char>` so the
/// caret column is a plain index; there is always at least one line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextArea {
    pub(crate) lines: Vec<Vec<char>>,
    pub(crate) row: usize,
    pub(crate) col: usize,
}

impl Default for TextArea {
    fn default() -> Self {
        Self {
            lines: vec![Vec::new()],
            row: 0,
            col: 0,
        }
    }
}

impl TextArea {
    /// A buffer holding `text`, the caret at its start.
    pub fn from_text(text: &str) -> Self {
        TextArea {
            lines: text.split('\n').map(|l| l.chars().collect()).collect(),
            row: 0,
            col: 0,
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

    pub fn move_up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.lines[self.row].len());
        } else {
            self.col = 0;
        }
    }

    pub fn move_down(&mut self) {
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

    /// The caret `n` lines up or down, stopping at the first or last line.
    pub fn move_lines(&mut self, down: bool, n: usize) {
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

    /// The lines soft-wrapped at `width` columns (one per character), each
    /// as `(line, start column)`, and the caret's wrapped `(row, column)`.
    /// An empty line is one empty wrapped line; a caret at a full row's end
    /// sits at the start of the next.
    pub fn wrapped(&self, width: usize) -> (Vec<(usize, usize)>, (usize, usize)) {
        let width = width.max(1);
        let mut rows = Vec::new();
        let mut caret = (0, 0);
        for (i, line) in self.lines.iter().enumerate() {
            let chunks = (line.len() / width + 1).max(1);
            for chunk in 0..chunks {
                let start = chunk * width;
                if chunk > 0 && start > line.len() {
                    break;
                }
                if i == self.row && self.col >= start && (self.col < start + width || chunk + 1 == chunks) {
                    caret = (rows.len(), self.col - start);
                }
                rows.push((i, start));
            }
        }
        (rows, caret)
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

    #[test]
    fn wrapping_tracks_the_caret() {
        let mut t = TextArea::from_text("abcdef\n\nxy");
        assert_eq!(t.wrapped(4).0, [(0, 0), (0, 4), (1, 0), (2, 0)]);
        t.move_to(0, 5);
        assert_eq!(t.wrapped(4).1, (1, 1));
        t.move_to(0, 4);
        assert_eq!(t.wrapped(4).1, (1, 0));
        let full = TextArea::from_text("abcd");
        // A full row: the caret at its end sits on the next (empty) row.
        assert_eq!(full.wrapped(4).0, [(0, 0), (0, 4)]);
        t.move_to(2, 2);
        assert_eq!(t.wrapped(4).1, (3, 2));
    }
}
