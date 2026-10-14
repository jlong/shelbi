//! The error-log viewer's state machine and row layout.
//!
//! Lists the project's recorded errors newest first, marks the unread ones
//! with a `●`, and (the caller's contract) marks the whole log read on open so
//! the sidebar's unread button clears once the viewer closes. `↑`/`↓` (or
//! `k`/`j`, or the wheel) scroll, `c` clears the log, Esc/`q` close. Reading,
//! marking and clearing the on-disk log stay in [`shelbi_state`]; a
//! [`ErrorLogOutcome::Clear`] asks the caller to do that IO.

use chrono::{DateTime, Local};
use shelbi_state::keymap::{Key, KeyChord};
use shelbi_state::ErrorLogEntry;

/// The column an entry's message starts at (and its wrapped continuation lines
/// align to): the `● ` marker (2), `YYYY-MM-DD HH:MM` (16), two spaces.
pub const MESSAGE_INDENT: usize = 20;

/// The hint line under the log.
pub const HINT: &str = "↑/↓ scroll · c clear · esc close";

/// The line shown when the log is empty.
pub const EMPTY: &str = "No errors logged.";

/// What a key did. `Clear` asks the caller to wipe the on-disk log, then call
/// [`Viewer::clear`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorLogOutcome {
    Continue,
    Close,
    Clear,
}

/// The viewer: the entries (newest first) with their unread-when-opened flag,
/// and the scroll offset in rendered lines.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    /// `(entry, was_unread_when_opened)`, newest first.
    pub rows: Vec<(ErrorLogEntry, bool)>,
    scroll: usize,
}

impl Viewer {
    pub fn new(rows: Vec<(ErrorLogEntry, bool)>) -> Self {
        Self { rows, scroll: 0 }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Clamp the offset so it never scrolls past the last line: `total`
    /// rendered lines in a viewport `visible` lines tall.
    pub fn clamp(&mut self, total: usize, visible: usize) {
        self.scroll = self.scroll.min(total.saturating_sub(visible));
    }

    /// Set the offset (a client that also scrolls natively, by the wheel,
    /// syncs it before applying a key).
    pub fn set_scroll(&mut self, lines: usize) {
        self.scroll = lines;
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    /// Empty the view after the on-disk log is cleared.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.scroll = 0;
    }

    /// Feed one key. Pure except for the scroll; the caller does the clear IO.
    pub fn handle_key(&mut self, chord: KeyChord) -> ErrorLogOutcome {
        match chord.code {
            Key::Esc | Key::Char('q') => return ErrorLogOutcome::Close,
            Key::Up | Key::Char('k') => self.scroll_up(1),
            Key::Down | Key::Char('j') => self.scroll_down(1),
            Key::PageUp => self.scroll_up(10),
            Key::PageDown => self.scroll_down(10),
            Key::Home => self.scroll_top(),
            Key::Char('c') => return ErrorLogOutcome::Clear,
            _ => {}
        }
        ErrorLogOutcome::Continue
    }

    /// The whole log as plain text, one entry per line (`<ts>  <message>`),
    /// newest first: what "copy" puts on the clipboard.
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|(e, _)| format!("{}  {}", format_ts(e), e.message))
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// An entry's stored RFC3339 timestamp as local `YYYY-MM-DD HH:MM`, or the
/// raw string when it doesn't parse.
pub fn format_ts(entry: &ErrorLogEntry) -> String {
    match DateTime::parse_from_rfc3339(&entry.ts) {
        Ok(dt) => dt.with_timezone(&Local).format("%Y-%m-%d %H:%M").to_string(),
        Err(_) => entry.ts.clone(),
    }
}

/// Word-wrap `text` to `width` columns, hard-splitting a word longer than the
/// width. Always at least one (possibly empty) line.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            current = chunk;
            continue;
        }
        let sep = usize::from(!current.is_empty());
        if current.chars().count() + sep + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            if sep == 1 {
                current.push(' ');
            }
            current.push_str(word);
        }
    }
    lines.push(current);
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, key};

    fn entry(ts: &str, message: &str) -> ErrorLogEntry {
        ErrorLogEntry {
            ts: ts.into(),
            message: message.into(),
            source: None,
        }
    }

    #[test]
    fn keys_scroll_close_and_request_clear() {
        let mut v = Viewer::new(vec![(entry("2026-09-26T14:02:00Z", "boom"), true)]);
        assert_eq!(v.handle_key(key(Key::Down)), ErrorLogOutcome::Continue);
        assert_eq!(v.scroll(), 1);
        assert_eq!(v.handle_key(ch('k')), ErrorLogOutcome::Continue);
        assert_eq!(v.scroll(), 0);
        v.handle_key(key(Key::PageDown));
        assert_eq!(v.scroll(), 10);
        v.handle_key(key(Key::Home));
        assert_eq!(v.scroll(), 0);
        assert_eq!(v.handle_key(ch('c')), ErrorLogOutcome::Clear);
        assert_eq!(v.rows.len(), 1, "the caller clears, not the viewer");
        v.clear();
        assert!(v.rows.is_empty());
        assert_eq!(v.handle_key(key(Key::Esc)), ErrorLogOutcome::Close);
        assert_eq!(v.handle_key(ch('q')), ErrorLogOutcome::Close);
    }

    #[test]
    fn scroll_clamps_to_the_last_page() {
        let mut v = Viewer::new(Vec::new());
        v.scroll_down(100);
        v.clamp(30, 10);
        assert_eq!(v.scroll(), 20);
        v.clamp(5, 10);
        assert_eq!(v.scroll(), 0);
    }

    #[test]
    fn wrap_hard_splits_a_word_longer_than_the_width() {
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap("", 7), vec![""]);
    }

    #[test]
    fn text_is_one_line_per_entry_newest_first() {
        let v = Viewer::new(vec![
            (entry("not a date", "second"), false),
            (entry("also not", "first"), true),
        ]);
        assert_eq!(v.text(), "not a date  second\nalso not  first");
    }
}
