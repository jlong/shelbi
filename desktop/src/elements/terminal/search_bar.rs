//! The find bar's state: the query being typed (with any IME composition in
//! it), smart case, and the "3 of 12" status. The pane owns one while the
//! bar is open and drives `shelbi_term::view::TerminalView`'s search with it;
//! this module is plain data, unit-tested without a window.
//!
//! Keys while the bar is open (handled by the pane): typing edits the query
//! and searches as you type; Enter steps to the next older match and
//! Shift+Enter to the next newer one (Cmd+G / Cmd+Shift+G too); Escape closes
//! the bar and clears the highlights.

/// The open find bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchBar {
    query: String,
    /// An IME composition in progress at the end of the query.
    marked: Option<String>,
}

/// What a key in the open bar does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarKey {
    /// Step to the next older match (Enter, Cmd+G).
    Older,
    /// Step to the next newer match (Shift+Enter, Cmd+Shift+G).
    Newer,
    /// Delete the last character (Backspace).
    Backspace,
    /// Clear the query (Cmd+Backspace).
    Clear,
    /// Close the bar (Escape).
    Close,
}

impl BarKey {
    /// The bar's meaning of a gpui key name with modifiers, if any. Keys with
    /// no meaning here are swallowed by the pane while the bar is open (they
    /// never reach the session), except app chords, which propagate.
    pub fn from_key(key: &str, shift: bool, platform: bool) -> Option<Self> {
        Some(match (key, shift, platform) {
            ("enter", false, false) | ("g", false, true) => BarKey::Older,
            ("enter", true, false) | ("g", true, true) => BarKey::Newer,
            ("backspace", _, false) => BarKey::Backspace,
            ("backspace", _, true) => BarKey::Clear,
            ("escape", _, _) => BarKey::Close,
            _ => return None,
        })
    }
}

impl SearchBar {
    /// A bar holding `seed` (the selection, when it is one line of text).
    pub fn new(seed: Option<&str>) -> Self {
        let mut bar = Self::default();
        if let Some(seed) = seed {
            bar.insert(seed);
        }
        bar
    }

    /// The query (without any composition in progress).
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The query as shown: with the composition in progress, and where the
    /// composition starts (a byte index into the text).
    pub fn display(&self) -> (String, Option<usize>) {
        match &self.marked {
            Some(m) => (format!("{}{m}", self.query), Some(self.query.len())),
            None => (self.query.clone(), None),
        }
    }

    /// Append typed or pasted text: only its first line, control characters
    /// dropped. Returns whether the query changed.
    pub fn insert(&mut self, text: &str) -> bool {
        self.marked = None;
        let line = text.lines().next().unwrap_or("");
        let clean: String = line.chars().filter(|c| !c.is_control()).collect();
        self.query.push_str(&clean);
        !clean.is_empty()
    }

    /// Set (or, with `None` or an empty string, end) the IME composition.
    pub fn set_marked(&mut self, marked: Option<&str>) {
        self.marked = marked.filter(|m| !m.is_empty()).map(str::to_string);
    }

    /// Delete the last character. Returns whether the query changed.
    pub fn backspace(&mut self) -> bool {
        self.query.pop().is_some()
    }

    /// Clear the query. Returns whether it changed.
    pub fn clear(&mut self) -> bool {
        let changed = !self.query.is_empty();
        self.query.clear();
        self.marked = None;
        changed
    }

    /// Smart case: case-insensitive unless the query has an uppercase
    /// letter.
    pub fn case_insensitive(&self) -> bool {
        !self.query.chars().any(char::is_uppercase)
    }

    /// The status beside the query: `3 of 12`, `No matches`, or nothing for
    /// an empty query. `position` is the view's `search_position()`.
    pub fn status(&self, position: Option<(usize, usize)>) -> String {
        if self.query.is_empty() {
            return String::new();
        }
        match position {
            Some((_, 0)) | None => "No matches".into(),
            Some((n, total)) => format!("{n} of {total}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_builds_the_query_and_backspace_removes() {
        let mut bar = SearchBar::new(None);
        assert!(bar.insert("ne"));
        assert!(bar.insert("é"));
        assert_eq!(bar.query(), "neé");
        assert!(bar.backspace());
        assert_eq!(bar.query(), "ne");
        assert!(bar.clear());
        assert!(!bar.backspace(), "nothing left to delete");
        assert!(!bar.clear());
    }

    #[test]
    fn pasting_keeps_the_first_line_without_control_characters() {
        let mut bar = SearchBar::new(Some("first\tline\nsecond"));
        assert_eq!(bar.query(), "firstline");
        assert!(!bar.insert("\n"), "an empty first line changes nothing");
    }

    #[test]
    fn a_composition_shows_after_the_query_until_committed() {
        let mut bar = SearchBar::new(Some("ab"));
        bar.set_marked(Some("にほ"));
        assert_eq!(bar.display(), ("abにほ".into(), Some(2)));
        assert_eq!(bar.query(), "ab", "a composition doesn't search yet");
        bar.insert("日本");
        assert_eq!(bar.display(), ("ab日本".into(), None));
        bar.set_marked(Some("x"));
        bar.set_marked(Some(""));
        assert_eq!(bar.display(), ("ab日本".into(), None), "an empty composition ends it");
    }

    #[test]
    fn smart_case() {
        assert!(SearchBar::new(Some("error")).case_insensitive());
        assert!(!SearchBar::new(Some("Error")).case_insensitive());
    }

    #[test]
    fn status_reads_the_position() {
        let bar = SearchBar::new(Some("x"));
        assert_eq!(bar.status(Some((3, 12))), "3 of 12");
        assert_eq!(bar.status(Some((0, 0))), "No matches");
        assert_eq!(SearchBar::new(None).status(Some((0, 0))), "");
    }

    #[test]
    fn bar_keys() {
        assert_eq!(BarKey::from_key("enter", false, false), Some(BarKey::Older));
        assert_eq!(BarKey::from_key("enter", true, false), Some(BarKey::Newer));
        assert_eq!(BarKey::from_key("g", false, true), Some(BarKey::Older));
        assert_eq!(BarKey::from_key("g", true, true), Some(BarKey::Newer));
        assert_eq!(BarKey::from_key("backspace", false, false), Some(BarKey::Backspace));
        assert_eq!(BarKey::from_key("backspace", false, true), Some(BarKey::Clear));
        assert_eq!(BarKey::from_key("escape", false, false), Some(BarKey::Close));
        assert_eq!(BarKey::from_key("g", false, false), None, "a plain g is text");
    }
}
