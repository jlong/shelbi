//! Fenced code blocks read as rendered code: their lines highlighted in the
//! language the opening fence names, and the fence lines themselves hidden
//! while the caret is elsewhere.
//!
//! The fences are source like any other syntax the editor hides, so they come
//! back to be edited: [`Doc::hidden_rows`] hides a block's fence rows unless
//! the selection touches the block (from its opening fence's first byte to its
//! closing fence's last), and a view without the keys (a pane being read)
//! hides them always. The caret still reaches them: a move into the block
//! (an arrow, a click on one of its lines) puts the selection in it, and the
//! fences show. A block with no code lines (two fences, nothing between)
//! keeps its fences, or it would draw as nothing and couldn't be clicked.
//!
//! [`CodeHighlights`] highlights each block whole, the grammar's state carried
//! down its lines, through [`crate::diff::highlight`]'s grammars and classes,
//! and keeps each block's result by its language and text: typing in one
//! block re-reads that block alone, and a body with no fenced language never
//! loads a grammar.

use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

use super::blocks::{Doc, Row, RowKind};
use crate::diff::highlight::{highlight_code, SyntaxSpan};

/// The language an opening fence's info string names (its first word):
/// `yaml` for "```yaml", `rust` for "~~~ rust title=x". `None` for a row
/// that isn't an opening fence, or a fence with no info string.
pub fn fence_language(row: &Row) -> Option<&str> {
    if row.kind != (RowKind::Fence { open: true }) {
        return None;
    }
    let rest = row.display.trim_start();
    let fence = rest.chars().next()?;
    let info = rest.trim_start_matches(fence).trim();
    let word = info.split(|c: char| c.is_whitespace() || c == '{').next()?;
    (!word.is_empty()).then_some(word)
}

/// A code block's rows: the first and last row index (inclusive).
fn block_rows(doc: &Doc) -> Vec<(usize, RangeInclusive<usize>)> {
    let mut out: Vec<(usize, RangeInclusive<usize>)> = Vec::new();
    for (i, row) in doc.rows.iter().enumerate() {
        let Some(block) = row.block.filter(|_| row.is_code()) else {
            continue;
        };
        match out.last_mut() {
            Some((b, rows)) if *b == block => *rows = *rows.start()..=i,
            _ => out.push((block, i..=i)),
        }
    }
    out
}

impl Doc {
    /// The language a fenced block names (see [`fence_language`]).
    pub fn code_language(&self, block: usize) -> Option<&str> {
        self.rows
            .iter()
            .find(|r| r.block == Some(block) && r.kind == (RowKind::Fence { open: true }))
            .and_then(fence_language)
    }

    /// Which rows a view leaves out: each fenced block's fence rows, unless
    /// `selection` (`None` for a view without the keys) touches the block,
    /// and each table's delimiter line while it draws as a grid
    /// ([`Doc::grid_rows`]). One entry per row.
    pub fn hidden_rows(&self, selection: Option<Range<usize>>) -> Vec<bool> {
        let mut hidden = vec![false; self.rows.len()];
        self.hide_table_delimiters(&self.grid_rows(selection.clone()), &mut hidden);
        for (_, rows) in block_rows(self) {
            let span = &self.rows[*rows.start()..=*rows.end()];
            let fenced = span.iter().any(|r| matches!(r.kind, RowKind::Fence { .. }));
            let has_code = span.iter().any(|r| r.kind == RowKind::Code);
            if !fenced || !has_code {
                continue;
            }
            let start = span[0].lines.start;
            let end = span[span.len() - 1].lines.end;
            let touched = selection
                .as_ref()
                .is_some_and(|s| s.start <= end && start <= s.end);
            if touched {
                continue;
            }
            for (i, r) in span.iter().enumerate() {
                if matches!(r.kind, RowKind::Fence { .. }) {
                    hidden[rows.start() + i] = true;
                }
            }
        }
        hidden
    }
}

/// A block's highlights, by its code lines.
type Lines = Arc<Vec<Vec<SyntaxSpan>>>;

/// The highlights of a doc's fenced code, a block at a time, each block's
/// kept until its language or text changes.
#[derive(Debug, Default)]
pub struct CodeHighlights {
    /// By (language, the block's code lines joined with `\n`); `None` for a
    /// language no grammar knows.
    cache: HashMap<(String, String), Option<Lines>>,
}

impl CodeHighlights {
    /// The spans of each of `doc`'s rows: a fenced code line in a language a
    /// grammar knows gets its classes; every other row (prose, fences,
    /// indented code, an unknown language's lines) none. Blocks that left the
    /// doc are forgotten.
    pub fn rows(&mut self, doc: &Doc) -> Vec<Vec<SyntaxSpan>> {
        let mut out = vec![Vec::new(); doc.rows.len()];
        let mut used = HashMap::new();
        for (block, rows) in block_rows(doc) {
            let Some(lang) = doc.code_language(block) else {
                continue;
            };
            let code: Vec<usize> = rows.filter(|&i| doc.rows[i].kind == RowKind::Code).collect();
            if code.is_empty() {
                continue;
            }
            let text = code
                .iter()
                .map(|&i| doc.rows[i].display.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let key = (lang.to_ascii_lowercase(), text);
            let lines = match self.cache.remove(&key) {
                Some(hit) => hit,
                None => highlight_code(&key.0, code.iter().map(|&i| doc.rows[i].display.as_str())).map(Arc::new),
            };
            if let Some(lines) = &lines {
                for (&i, spans) in code.iter().zip(lines.iter()) {
                    out[i] = spans.clone();
                }
            }
            used.insert(key, lines);
        }
        self.cache = used;
        out
    }

    /// How many blocks it holds (for tests).
    pub fn cached(&self) -> usize {
        self.cache.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::highlight::SyntaxClass;

    const YAML: &str = "Intro.\n\n```yaml\nopencode-plugin:\n  runs-on: ubuntu-latest # ci\n```\n\nAfter.";

    fn kinds(doc: &Doc, hidden: &[bool]) -> Vec<String> {
        doc.rows
            .iter()
            .zip(hidden)
            .filter(|(_, h)| !**h)
            .map(|(r, _)| r.display.clone())
            .collect()
    }

    #[test]
    fn fence_language_is_the_info_strings_first_word() {
        let lang = |src: &str| Doc::parse(src).rows.first().and_then(fence_language).map(str::to_string);
        assert_eq!(lang("```yaml\nx\n```").as_deref(), Some("yaml"));
        assert_eq!(lang("```yml\nx\n```").as_deref(), Some("yml"));
        assert_eq!(lang("~~~ rust title=x\nx\n~~~").as_deref(), Some("rust"));
        assert_eq!(lang("  ````toml\nx\n````").as_deref(), Some("toml"));
        assert_eq!(lang("```rust{.numbered}\nx\n```").as_deref(), Some("rust"));
        assert_eq!(lang("```\nx\n```"), None);
        assert_eq!(lang("```   \nx\n```"), None);
        // Only an opening fence names one.
        let doc = Doc::parse("```sh\nx\n```");
        assert_eq!(fence_language(&doc.rows[2]), None);
        assert_eq!(fence_language(&doc.rows[1]), None);
        assert_eq!(doc.code_language(doc.rows[1].block.unwrap()), Some("sh"));
    }

    #[test]
    fn fences_hide_unless_the_selection_is_in_the_block() {
        let doc = Doc::parse(YAML);
        let shown = |sel: Option<Range<usize>>| kinds(&doc, &doc.hidden_rows(sel));
        let code = ["Intro.", "opencode-plugin:", "  runs-on: ubuntu-latest # ci", "After."];
        // Read (no keys), or the caret in the text around it: no fences.
        assert_eq!(shown(None), code);
        assert_eq!(shown(Some(0..0)), code);
        let after = YAML.find("After").unwrap();
        assert_eq!(shown(Some(after..after)), code);
        // Just past "Intro.": the blank line separates it from the block.
        assert_eq!(shown(Some(6..6)), code);
        let all = ["Intro.", "```yaml", "opencode-plugin:", "  runs-on: ubuntu-latest # ci", "```", "After."];
        // At the opening fence's start, in a code line, at the closing
        // fence's end: the fences show.
        let open = YAML.find("```yaml").unwrap();
        assert_eq!(shown(Some(open..open)), all);
        let key = YAML.find("runs-on").unwrap();
        assert_eq!(shown(Some(key..key)), all);
        let close = YAML.rfind("```").unwrap() + 3;
        assert_eq!(shown(Some(close..close)), all);
        // A selection running into it from outside does too.
        assert_eq!(shown(Some(0..key)), all);
    }

    #[test]
    fn indented_code_and_empty_fenced_blocks_keep_their_rows() {
        let doc = Doc::parse("para\n\n    indented\n\n```\n```\n\n```\n");
        assert!(doc.hidden_rows(None).iter().all(|h| !h));
        // An unclosed fence with code hides its opening line.
        let doc = Doc::parse("```rust\nfn x() {}");
        assert_eq!(doc.hidden_rows(None), [true, false]);
    }

    #[test]
    fn hiding_and_showing_never_touches_the_source() {
        let mut e = super::super::Editor::markdown(YAML);
        for at in [0, YAML.find("runs").unwrap(), YAML.len()] {
            e.move_to(at, false);
            let _ = e.doc().hidden_rows(Some(e.selection()));
            assert_eq!(e.text(), YAML);
        }
    }

    #[test]
    fn known_languages_highlight_and_unknown_ones_stay_plain() {
        let mut h = CodeHighlights::default();
        let doc = Doc::parse(YAML);
        let rows = h.rows(&doc);
        let line = doc.rows.iter().position(|r| r.display.contains("runs-on")).unwrap();
        let classes: Vec<SyntaxClass> = rows[line].iter().map(|s| s.class).collect();
        assert!(classes.contains(&SyntaxClass::Tag), "{classes:?}");
        assert!(classes.contains(&SyntaxClass::Comment), "{classes:?}");
        // Fences and prose carry none.
        assert!(rows[0].is_empty() && rows[1].is_empty());
        for lang in ["rust", "sh", "json", "toml"] {
            let src = format!("```{lang}\nfn x = \"s\" # 1\n```");
            let doc = Doc::parse(&src);
            assert!(!h.rows(&doc)[1].is_empty(), "{lang}");
        }
        for src in ["```nosuchlang\nfn x = \"s\"\n```", "```\nfn x = \"s\"\n```", "    fn x = \"s\"\n"] {
            let doc = Doc::parse(src);
            assert!(h.rows(&doc).iter().all(Vec::is_empty), "{src:?}");
        }
    }

    #[test]
    fn a_blocks_highlights_are_kept_until_it_changes() {
        let mut h = CodeHighlights::default();
        let src = "```rust\nlet a = 1;\n```\n\n```yaml\nk: v\n```";
        let first = h.rows(&Doc::parse(src));
        assert_eq!(h.cached(), 2);
        // Same doc: the same spans, from the cache.
        assert_eq!(h.rows(&Doc::parse(src)), first);
        // Edit the yaml block: the rust block keeps its entry, the old yaml
        // text's is dropped.
        let edited = src.replace("k: v", "k: w");
        let second = h.rows(&Doc::parse(&edited));
        assert_eq!(h.cached(), 2);
        assert_eq!(second[1], first[1]);
        // A block removed is forgotten.
        h.rows(&Doc::parse("```rust\nlet a = 1;\n```"));
        assert_eq!(h.cached(), 1);
    }
}
