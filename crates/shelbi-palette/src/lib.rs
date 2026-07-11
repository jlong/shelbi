//! Command palette (Ctrl+Space) — fuzzy matcher backing the TUI's palette overlay.
//!
//! Pure data + matching. Rendering lives in `shelbi-tui`.

use nucleo_matcher::{
    pattern::{CaseMatching, Normalization},
    Matcher,
};
// Part of the `parse_pattern` / `score_pattern` API surface.
pub use nucleo_matcher::pattern::Pattern;

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
    /// Optional per-entry icon override + color. Set for entries that
    /// mirror a sidebar row (Chat/Tasks/Activity, workspaces, review-ready
    /// tasks, legacy agents) so the palette shows the same glyph and
    /// status tint the sidebar does. `None` falls back to the dim
    /// `EntryKind::icon()` used for entries without a sidebar twin.
    pub decoration: Option<Decoration>,
    /// When set, the entry is excluded from the empty-query palette list
    /// but included (fuzzy-scored) the moment the query is non-empty.
    /// Power-user shortcuts — the "Edit …" config openers — set this so
    /// they don't clutter the default list; every other entry leaves it
    /// `false` and stays visible on an empty query.
    pub hidden_until_query: bool,
}

/// Icon + color attached to an [`Entry`]. Mirrors what the sidebar
/// renders next to the matching row — the palette receives this
/// pre-computed so the two surfaces can't drift on glyph or tint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoration {
    pub glyph: String,
    pub color: DecorationColor,
}

/// Palette-side color enum. Kept ratatui-free so `shelbi-palette` stays
/// a pure-data crate; the renderer maps these to its terminal colors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DecorationColor {
    /// No explicit foreground — let the glyph render in its own color
    /// (used for emoji icons like 💬 / 📋 / ⚡).
    #[default]
    Default,
    Gray,
    DarkGray,
    Green,
    Yellow,
    Red,
    Cyan,
    Blue,
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

/// Parse `query` into a reusable fuzzy pattern. Parse once per query,
/// then score every candidate label against the result with
/// [`score_pattern`] — parsing is the expensive half of a match.
pub fn parse_pattern(query: &str) -> Pattern {
    Pattern::parse(query, CaseMatching::Smart, Normalization::Smart)
}

/// Score `label` against a pre-parsed `pattern`. Higher = better.
/// None = no match. An empty pattern matches everything at score 0.
pub fn score_pattern(matcher: &mut Matcher, pattern: &Pattern, label: &str) -> Option<u16> {
    let mut buf = Vec::new();
    let haystack = nucleo_matcher::Utf32Str::new(label, &mut buf);
    pattern
        .score(haystack, matcher)
        .map(|s| s.min(u32::from(u16::MAX)) as u16)
}

/// Score an entry's `label` against `pattern`. Higher = better. None = no match.
///
/// Convenience for one-off matches; when scoring many labels against the
/// same query, use [`parse_pattern`] + [`score_pattern`] to parse once.
pub fn score(matcher: &mut Matcher, pattern: &str, label: &str) -> Option<u16> {
    score_pattern(matcher, &parse_pattern(pattern), label)
}

/// Filter + sort `entries` against `query`. Best match first.
///
/// Entries flagged [`Entry::hidden_until_query`] are dropped while the
/// query is empty or whitespace-only, then scored normally the instant the
/// user types a non-blank character — a "hidden until searched" tier for
/// power-user shortcuts that shouldn't pad the default list.
pub fn search(entries: &[Entry], query: &str) -> Vec<(Entry, u16)> {
    let mut matcher = Matcher::new(nucleo_matcher::Config::DEFAULT);
    let pattern = parse_pattern(query);
    let query_blank = query.trim().is_empty();
    let mut hits: Vec<(Entry, u16)> = entries
        .iter()
        .filter(|e| !(query_blank && e.hidden_until_query))
        .filter_map(|e| score_pattern(&mut matcher, &pattern, &e.label).map(|s| (e.clone(), s)))
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
                decoration: None,
                hidden_until_query: false,
            },
            Entry {
                id: "agent:fix-login-bug".into(),
                label: "fix-login-bug".into(),
                kind: EntryKind::Agent,
                subtitle: Some("m2 · running".into()),
                shortcut: None,
                decoration: None,
                hidden_until_query: false,
            },
            Entry {
                id: "action:new-task".into(),
                label: "New task".into(),
                kind: EntryKind::Action,
                subtitle: None,
                shortcut: None,
                decoration: None,
                hidden_until_query: false,
            },
        ]
    }

    /// A `hidden_until_query` entry to append to [`sample`] for the
    /// hidden-tier filtering tests.
    fn hidden_entry() -> Entry {
        Entry {
            id: "edit:project".into(),
            label: "Edit Project Settings".into(),
            kind: EntryKind::Action,
            subtitle: Some("opens project.yaml".into()),
            shortcut: None,
            decoration: None,
            hidden_until_query: true,
        }
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

    #[test]
    fn hidden_until_query_excluded_on_blank_query() {
        let mut entries = sample();
        entries.push(hidden_entry());
        // Empty query: the hidden entry stays out, the three always-visible
        // ones remain — the default list is unchanged from before the flag.
        let empty = search(&entries, "");
        assert_eq!(empty.len(), 3);
        assert!(empty.iter().all(|(e, _)| e.id != "edit:project"));
        // A whitespace-only query counts as blank too.
        let ws = search(&entries, "   ");
        assert!(ws.iter().all(|(e, _)| e.id != "edit:project"));
    }

    #[test]
    fn hidden_until_query_included_on_nonempty_query() {
        let mut entries = sample();
        entries.push(hidden_entry());
        // The moment the query is non-blank, the hidden entry is scored
        // like any other and surfaces when it matches.
        let hits = search(&entries, "Edit");
        assert!(hits.iter().any(|(e, _)| e.id == "edit:project"));
        // ...and still doesn't appear for a query it doesn't match.
        let miss = search(&entries, "Chat");
        assert!(miss.iter().all(|(e, _)| e.id != "edit:project"));
    }
}
