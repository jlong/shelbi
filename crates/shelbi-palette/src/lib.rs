//! Command palette (Ctrl+Space) — fuzzy matcher backing the TUI's palette overlay.
//!
//! Pure data + matching. Rendering lives in `shelbi-tui`.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization, Pattern},
    Matcher,
};

/// A single thing the palette can find — a view, an agent, or an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub id: String,
    pub label: String,
    pub kind: EntryKind,
    pub subtitle: Option<String>,
    /// Optional right-aligned hotkey hint (e.g. `⌥Z`). Surfaces the
    /// hotkey-equivalent for entries that can also be reached without
    /// the palette, so users learn the chord by spotting it in the row.
    pub shortcut: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    View,
    Agent,
    Action,
}

impl EntryKind {
    pub fn icon(self) -> &'static str {
        match self {
            EntryKind::View => "◉",
            EntryKind::Agent => "▶",
            EntryKind::Action => "⚡",
        }
    }
}

/// Score an entry's `label` against `pattern`. Higher = better. None = no match.
pub fn score(matcher: &mut Matcher, pattern: &str, label: &str) -> Option<u16> {
    if pattern.is_empty() {
        return Some(0);
    }
    let needle = Pattern::parse(pattern, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let haystack = nucleo_matcher::Utf32Str::new(label, &mut buf);
    let scored = needle.atoms.iter().try_fold(0u16, |acc, atom| {
        atom.score(haystack, matcher).map(|s| acc.saturating_add(s))
    });
    scored
}

/// Filter + sort `entries` against `query`. Best match first.
pub fn search(entries: &[Entry], query: &str) -> Vec<(Entry, u16)> {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let mut hits: Vec<(Entry, u16)> = entries
        .iter()
        .filter_map(|e| score(&mut matcher, query, &e.label).map(|s| (e.clone(), s)))
        .collect();
    hits.sort_by_key(|h| std::cmp::Reverse(h.1));
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Entry> {
        vec![
            Entry {
                id: "view:chat".into(),
                label: "Chat".into(),
                kind: EntryKind::View,
                subtitle: None,
                shortcut: None,
            },
            Entry {
                id: "agent:fix-login-bug".into(),
                label: "fix-login-bug".into(),
                kind: EntryKind::Agent,
                subtitle: Some("m2 · running".into()),
                shortcut: None,
            },
            Entry {
                id: "action:new-task".into(),
                label: "New task".into(),
                kind: EntryKind::Action,
                subtitle: None,
                shortcut: None,
            },
        ]
    }

    #[test]
    fn empty_query_returns_all() {
        let hits = search(&sample(), "");
        assert_eq!(hits.len(), 3);
    }

    #[test]
    fn fuzzy_matches_subseq() {
        let hits = search(&sample(), "flbg");
        assert!(!hits.is_empty());
        assert_eq!(hits[0].0.id, "agent:fix-login-bug");
    }

    #[test]
    fn no_match_returns_empty() {
        let hits = search(&sample(), "qqqqqqq");
        assert!(hits.is_empty());
    }
}
