//! The comment editor, as in Skim's COMMENT mode: `Enter` saves, `Ctrl-J`
//! breaks the line, and it starts inserting. `Esc` while inserting drops to
//! motions (`h j k l w b e 0 $ x dd`, `i a I A` to insert again); `Esc` there
//! cancels, so a slip of `Esc` never loses what was typed.

use shelbi_state::keymap::{Key, KeyChord, Mods};

use super::comments::Anchor;
use crate::text_area::TextArea;

/// What saving a draft does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DraftTarget {
    /// A new thread on these lines.
    New(Anchor),
    /// New text for a comment, or one of its replies.
    Edit { id: String, reply: Option<String> },
    /// A reply to a thread.
    Reply { id: String },
}

/// What a key did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftStep {
    Editing,
    Save,
    Cancel,
}

/// A comment being written.
#[derive(Debug, Clone)]
pub struct CommentDraft {
    pub target: DraftTarget,
    pub area: TextArea,
    /// Typing inserts. Otherwise keys are motions.
    pub inserting: bool,
    /// The first `d` of `dd`.
    pending_delete: bool,
}

impl CommentDraft {
    pub fn new(target: DraftTarget, text: &str) -> Self {
        let mut area = TextArea::from_text(text);
        let last = area.lines().len().saturating_sub(1);
        let end = area.lines()[last].len();
        area.move_to(last, end);
        CommentDraft { target, area, inserting: true, pending_delete: false }
    }

    /// The text to save.
    pub fn text(&self) -> String {
        self.area.text().trim_end().to_string()
    }

    /// A key.
    pub fn key(&mut self, chord: KeyChord) -> DraftStep {
        let plain = chord.mods == Mods::NONE;
        match chord.code {
            Key::Enter if plain => return DraftStep::Save,
            Key::Char('j') if chord.mods == Mods::CONTROL => {
                self.area.newline();
                return DraftStep::Editing;
            }
            _ => {}
        }
        if self.inserting {
            if chord.code == Key::Esc {
                self.inserting = false;
                return DraftStep::Editing;
            }
            self.area.edit(chord, false, 1);
            return DraftStep::Editing;
        }
        if chord.code == Key::Esc {
            return DraftStep::Cancel;
        }
        let was_pending = std::mem::take(&mut self.pending_delete);
        // A terminal sends `I` bare, a window system as Shift+i.
        let code = match chord.code {
            Key::Char(c) if chord.mods.contains(Mods::SHIFT) => Key::Char(c.to_ascii_uppercase()),
            code => code,
        };
        match code {
            Key::Char('i') => self.inserting = true,
            Key::Char('a') => {
                self.area.move_right();
                self.inserting = true;
            }
            Key::Char('I') => {
                self.area.home();
                self.inserting = true;
            }
            Key::Char('A') => {
                self.area.end();
                self.inserting = true;
            }
            Key::Char('h') | Key::Left => self.area.move_left(),
            Key::Char('l') | Key::Right => self.area.move_right(),
            Key::Char('k') | Key::Up => self.area.move_up(),
            Key::Char('j') | Key::Down => self.area.move_down(),
            Key::Char('0') | Key::Home => self.area.home(),
            Key::Char('$') | Key::End => self.area.end(),
            Key::Char('w') => self.word_forward(),
            Key::Char('b') => self.word_back(),
            Key::Char('e') => self.word_end(),
            Key::Char('x') => self.area.delete(),
            Key::Backspace => self.area.backspace(),
            Key::Char('d') if was_pending => self.delete_line(),
            Key::Char('d') => self.pending_delete = true,
            _ => {}
        }
        DraftStep::Editing
    }

    /// The whole text as chars, and the caret's offset in it.
    fn flat(&self) -> (Vec<char>, usize) {
        let (row, col) = self.area.cursor();
        let mut chars = Vec::new();
        let mut at = 0;
        for (r, line) in self.area.lines().iter().enumerate() {
            if r == row {
                at = chars.len() + col;
            }
            chars.extend_from_slice(line);
            chars.push('\n');
        }
        (chars, at)
    }

    /// Put the caret at offset `at` of [`Self::flat`].
    fn seek(&mut self, at: usize) {
        let mut rest = at;
        let lengths: Vec<usize> = self.area.lines().iter().map(Vec::len).collect();
        for (row, len) in lengths.iter().enumerate() {
            if rest <= *len {
                self.area.move_to(row, rest);
                return;
            }
            rest -= len + 1;
        }
        let last = lengths.len() - 1;
        self.area.move_to(last, lengths[last]);
    }

    fn word_forward(&mut self) {
        let (chars, mut at) = self.flat();
        let class = |c: char| word_class(c);
        if at < chars.len() {
            let start = class(chars[at]);
            while at < chars.len() && class(chars[at]) == start && start != 0 {
                at += 1;
            }
            while at < chars.len() && class(chars[at]) == 0 {
                at += 1;
            }
        }
        self.seek(at);
    }

    fn word_back(&mut self) {
        let (chars, mut at) = self.flat();
        while at > 0 && word_class(chars[at - 1]) == 0 {
            at -= 1;
        }
        if at > 0 {
            let class = word_class(chars[at - 1]);
            while at > 0 && word_class(chars[at - 1]) == class {
                at -= 1;
            }
        }
        self.seek(at);
    }

    fn word_end(&mut self) {
        let (chars, mut at) = self.flat();
        at = (at + 1).min(chars.len());
        while at < chars.len() && word_class(chars[at]) == 0 {
            at += 1;
        }
        if at < chars.len() {
            let class = word_class(chars[at]);
            while at + 1 < chars.len() && word_class(chars[at + 1]) == class {
                at += 1;
            }
        }
        self.seek(at);
    }

    fn delete_line(&mut self) {
        let (row, _) = self.area.cursor();
        let mut lines: Vec<String> = self.area.lines().iter().map(|l| l.iter().collect()).collect();
        lines.remove(row);
        self.area = TextArea::from_text(&lines.join("\n"));
        self.area.move_to(row.min(self.area.lines().len() - 1), 0);
    }
}

/// 0 for whitespace, 1 for word characters, 2 for punctuation.
fn word_class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ch(c: char) -> KeyChord {
        KeyChord::new(Key::Char(c), Mods::NONE)
    }

    fn draft(text: &str) -> CommentDraft {
        CommentDraft::new(DraftTarget::Reply { id: "c".into() }, text)
    }

    fn typed(keys: &str, d: &mut CommentDraft) {
        for c in keys.chars() {
            d.key(ch(c));
        }
    }

    const ESC: KeyChord = KeyChord::new(Key::Esc, Mods::NONE);

    #[test]
    fn it_starts_inserting_and_enter_saves() {
        let mut d = draft("");
        typed("why not?", &mut d);
        assert_eq!(d.text(), "why not?");
        assert_eq!(d.key(KeyChord::new(Key::Enter, Mods::NONE)), DraftStep::Save);
    }

    #[test]
    fn ctrl_j_breaks_the_line() {
        let mut d = draft("one");
        assert_eq!(d.key(KeyChord::new(Key::Char('j'), Mods::CONTROL)), DraftStep::Editing);
        typed("two", &mut d);
        assert_eq!(d.text(), "one\ntwo");
    }

    #[test]
    fn esc_drops_to_motions_then_cancels() {
        let mut d = draft("x");
        assert_eq!(d.key(ESC), DraftStep::Editing);
        assert!(!d.inserting);
        assert_eq!(d.key(ESC), DraftStep::Cancel);
    }

    #[test]
    fn motions_move_and_insert_keys_insert() {
        let mut d = draft("alpha beta");
        d.key(ESC);
        d.key(ch('0'));
        d.key(ch('w'));
        assert_eq!(d.area.cursor(), (0, 6));
        d.key(ch('e'));
        assert_eq!(d.area.cursor(), (0, 9));
        d.key(ch('b'));
        assert_eq!(d.area.cursor(), (0, 6));
        d.key(ch('x'));
        assert_eq!(d.text(), "alpha eta");
        d.key(ch('i'));
        d.key(ch('B'));
        assert_eq!(d.text(), "alpha Beta");
        d.key(ESC);
        d.key(KeyChord::new(Key::Char('i'), Mods::SHIFT));
        d.key(ch('>'));
        assert_eq!(d.text(), ">alpha Beta");
        d.key(ESC);
        d.key(KeyChord::new(Key::Char('a'), Mods::SHIFT));
        d.key(ch('!'));
        assert_eq!(d.text(), ">alpha Beta!");
    }

    #[test]
    fn dd_deletes_the_line() {
        let mut d = draft("one\ntwo\nthree");
        d.key(ESC);
        d.key(ch('k'));
        d.key(ch('d'));
        d.key(ch('d'));
        assert_eq!(d.text(), "one\nthree");
    }

    #[test]
    fn dollar_and_word_motions_cross_lines() {
        let mut d = draft("one\ntwo");
        d.key(ESC);
        d.key(ch('k'));
        d.key(ch('0'));
        d.key(ch('$'));
        assert_eq!(d.area.cursor(), (0, 3));
        d.key(ch('w'));
        assert_eq!(d.area.cursor(), (1, 0));
    }
}
