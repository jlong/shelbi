// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-search/src/lib.rs), MIT in Shelbi.

//! Full-text search across ContextStore documents.
//!
//! v1 ships **live mode only**: walks the space tree with the
//! `ignore` crate's `WalkBuilder`, content-matches each `.md` file
//! line by line. Results are buffered, sorted by relative path, and
//! returned in deterministic order. Tantivy-backed indexed search is
//! reserved for a follow-up — the [`search`] entry point will gain
//! an indexed mode behind the same surface later.
//!
//! Replaces the search logic in `SearchCommand.swift` and
//! `MCPServer.swift::executeSearch` from the Swift CLI.
//!
//! Spec lives in `Engineering/rust-core/cstore-search.md` on the
//! `docs/planning` branch.

#![deny(missing_docs)]

use std::path::{Path, PathBuf};

/// Errors surfaced by `cstore-search`.
#[derive(Debug, thiserror::Error)]
pub enum SearchError {
    /// Underlying I/O or walker failure.
    #[error("io: {0}")]
    Io(String),

    /// User-supplied regex failed to compile.
    #[error("invalid regex: {0}")]
    InvalidRegex(String),

    /// `folder` scope was provided but doesn't exist under the
    /// space root.
    #[error("not found: {0}")]
    NotFound(String),
}

impl From<SearchError> for crate::domain::CoreError {
    fn from(err: SearchError) -> Self {
        match err {
            SearchError::Io(msg) => crate::domain::CoreError::Io(msg),
            SearchError::InvalidRegex(msg) => crate::domain::CoreError::InvalidInput(msg),
            SearchError::NotFound(msg) => crate::domain::CoreError::NotFound(msg),
        }
    }
}

/// Result alias defaulting to [`SearchError`].
pub type Result<T> = std::result::Result<T, SearchError>;

/// How [`search`] interprets `Query.pattern`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryMode {
    /// Case-insensitive substring match.
    Substring,
    /// Case-sensitive substring match.
    SubstringExact,
    /// Regex (Rust `regex` crate syntax). Compiled once before the
    /// walk starts — invalid patterns surface as
    /// [`SearchError::InvalidRegex`] before any I/O happens.
    Regex,
}

/// A search query.
#[derive(Debug, Clone)]
pub struct Query {
    /// What to look for (interpreted per [`QueryMode`]).
    pub pattern: String,
    /// How to interpret [`Query::pattern`].
    pub mode: QueryMode,
    /// Limit search to a subfolder, relative to the space root.
    /// `None` = whole space.
    pub folder: Option<PathBuf>,
    /// Cap on matches per document. `0` = unlimited. Default: `0`.
    pub max_per_doc: usize,
    /// Cap on total matches across the whole search. `0` =
    /// unlimited. Default: `1000`.
    pub max_total: usize,
}

impl Default for Query {
    fn default() -> Self {
        Self {
            pattern: String::new(),
            mode: QueryMode::Substring,
            folder: None,
            max_per_doc: 0,
            max_total: 1000,
        }
    }
}

/// One match in one document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    /// Absolute path to the matched document.
    pub document: PathBuf,
    /// Path relative to the space root.
    pub relative_path: PathBuf,
    /// 0-based line index within the document.
    pub line_index: usize,
    /// The full text of the line containing the match.
    pub line_text: String,
    /// Byte offsets of the match within `line_text` as `(start, end)`.
    /// Always aligned to UTF-8 character boundaries.
    pub byte_range: (usize, usize),
}

/// Search a space. Returns matches in document-traversal order
/// (deterministic: sorted alphabetically by relative path).
pub fn search(space_root: &Path, query: &Query) -> Result<Vec<Match>> {
    let matcher = build_matcher(query)?;
    let scope_root = resolve_scope(space_root, query.folder.as_deref())?;
    let mut results = Vec::new();
    let total_cap = if query.max_total == 0 {
        usize::MAX
    } else {
        query.max_total
    };

    for entry in walk(&scope_root) {
        let path = entry.path().to_path_buf();
        if !is_searchable_md(&path) {
            continue;
        }
        let body = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            // Skip unreadable / non-UTF-8 files rather than abort.
            Err(_) => continue,
        };
        let relative = path.strip_prefix(space_root).unwrap_or(&path).to_path_buf();
        let per_doc_cap = if query.max_per_doc == 0 {
            usize::MAX
        } else {
            query.max_per_doc
        };
        let mut per_doc = 0usize;
        for (line_index, line) in body.lines().enumerate() {
            if per_doc >= per_doc_cap {
                break;
            }
            if results.len() >= total_cap {
                break;
            }
            for (start, end) in matcher.find_in(line) {
                results.push(Match {
                    document: path.clone(),
                    relative_path: relative.clone(),
                    line_index,
                    line_text: line.to_string(),
                    byte_range: (start, end),
                });
                per_doc += 1;
                if per_doc >= per_doc_cap || results.len() >= total_cap {
                    break;
                }
            }
        }
        if results.len() >= total_cap {
            break;
        }
    }

    results.sort_by(|a, b| {
        a.relative_path
            .cmp(&b.relative_path)
            .then(a.line_index.cmp(&b.line_index))
            .then(a.byte_range.0.cmp(&b.byte_range.0))
    });
    Ok(results)
}

/// Maximum file size we'll content-match. Files larger than this
/// are skipped per the sub-plan's sanity bound.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;

fn is_searchable_md(path: &Path) -> bool {
    if path.extension().and_then(|s| s.to_str()) != Some("md") {
        return false;
    }
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && m.len() <= MAX_FILE_BYTES,
        Err(_) => false,
    }
}

fn walk(root: &Path) -> impl Iterator<Item = ignore::DirEntry> {
    ignore::WalkBuilder::new(root)
        .hidden(true)
        .git_ignore(false)
        .git_exclude(false)
        .git_global(false)
        .add_custom_ignore_filename(".csignore")
        .build()
        .filter_map(|res| res.ok())
}

fn resolve_scope(space_root: &Path, folder: Option<&Path>) -> Result<PathBuf> {
    match folder {
        None => Ok(space_root.to_path_buf()),
        Some(rel) => {
            let scoped = space_root.join(rel);
            if !scoped.exists() {
                return Err(SearchError::NotFound(rel.display().to_string()));
            }
            Ok(scoped)
        }
    }
}

/// Compiled query, ready to apply per line.
enum Matcher {
    Substring { needle: String },
    SubstringExact { needle: String },
    Regex(regex::Regex),
}

impl Matcher {
    fn find_in<'a>(&'a self, line: &'a str) -> Box<dyn Iterator<Item = (usize, usize)> + 'a> {
        match self {
            Matcher::SubstringExact { needle } => {
                if needle.is_empty() {
                    return Box::new(std::iter::empty());
                }
                let hits = ByteSubstringIter {
                    haystack: line,
                    needle,
                    case_insensitive: false,
                    pos: 0,
                };
                Box::new(hits)
            }
            Matcher::Substring { needle } => {
                if needle.is_empty() {
                    return Box::new(std::iter::empty());
                }
                let hits = ByteSubstringIter {
                    haystack: line,
                    needle,
                    case_insensitive: true,
                    pos: 0,
                };
                Box::new(hits)
            }
            Matcher::Regex(rx) => Box::new(rx.find_iter(line).map(|m| (m.start(), m.end()))),
        }
    }
}

/// Iterator over byte ranges of `needle` inside `haystack`. Honors
/// UTF-8 character boundaries by only yielding offsets that the
/// underlying `str::find` produces.
struct ByteSubstringIter<'a> {
    haystack: &'a str,
    needle: &'a str,
    case_insensitive: bool,
    pos: usize,
}

impl Iterator for ByteSubstringIter<'_> {
    type Item = (usize, usize);

    fn next(&mut self) -> Option<Self::Item> {
        if self.pos > self.haystack.len() {
            return None;
        }
        let tail = &self.haystack[self.pos..];
        let found = if self.case_insensitive {
            find_case_insensitive(tail, self.needle)
        } else {
            tail.find(self.needle)
        }?;
        let start = self.pos + found;
        let end = start + self.needle.len();
        // Advance by at least one byte to make progress; clamp to a
        // char boundary so the next `tail` slice is valid UTF-8.
        let mut next_pos = start + 1;
        while next_pos <= self.haystack.len() && !self.haystack.is_char_boundary(next_pos) {
            next_pos += 1;
        }
        self.pos = next_pos;
        Some((start, end))
    }
}

/// ASCII-aware case-insensitive substring search. For pure-ASCII
/// inputs (the overwhelming case for markdown source) this matches
/// the Swift implementation's behaviour and the byte offsets land
/// on the haystack's actual character boundaries. Non-ASCII chars
/// in either operand fall back to byte-exact comparison for those
/// code points.
fn find_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    if needle.is_empty() {
        return Some(0);
    }
    let hb = haystack.as_bytes();
    let nb = needle.as_bytes();
    if nb.len() > hb.len() {
        return None;
    }
    for start in 0..=hb.len() - nb.len() {
        let mut matched = true;
        for (i, &nbyte) in nb.iter().enumerate() {
            if !hb[start + i].eq_ignore_ascii_case(&nbyte) {
                matched = false;
                break;
            }
        }
        if matched && haystack.is_char_boundary(start) {
            return Some(start);
        }
    }
    None
}

fn build_matcher(query: &Query) -> Result<Matcher> {
    Ok(match query.mode {
        QueryMode::Substring => Matcher::Substring {
            needle: query.pattern.clone(),
        },
        QueryMode::SubstringExact => Matcher::SubstringExact {
            needle: query.pattern.clone(),
        },
        QueryMode::Regex => Matcher::Regex(
            regex::Regex::new(&query.pattern)
                .map_err(|e| SearchError::InvalidRegex(e.to_string()))?,
        ),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn fixture(files: &[(&str, &str)]) -> TempDir {
        let dir = TempDir::new().expect("tempdir");
        for (rel, body) in files {
            let path = dir.path().join(rel);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).expect("mkdir");
            }
            fs::write(&path, body).expect("write");
        }
        dir
    }

    fn substring(pattern: &str) -> Query {
        Query {
            pattern: pattern.to_string(),
            mode: QueryMode::Substring,
            ..Query::default()
        }
    }

    #[test]
    fn substring_matches_case_insensitive() {
        let dir = fixture(&[("note.md", "FOO bar\nbaz Foo\nqux")]);
        let hits = search(dir.path(), &substring("foo")).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].line_index, 0);
        assert_eq!(hits[0].byte_range, (0, 3));
        assert_eq!(hits[1].line_index, 1);
        assert_eq!(hits[1].byte_range, (4, 7));
    }

    #[test]
    fn substring_exact_is_case_sensitive() {
        let dir = fixture(&[("note.md", "Foo\nFOO\nfoo")]);
        let mut q = substring("foo");
        q.mode = QueryMode::SubstringExact;
        let hits = search(dir.path(), &q).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].line_index, 2);
    }

    #[test]
    fn regex_mode_compiles_pattern() {
        let dir = fixture(&[("a.md", "ticket PIX-1 and PIX-23 plus PIXIE\n")]);
        let mut q = substring(r"PIX-\d+");
        q.mode = QueryMode::Regex;
        let hits = search(dir.path(), &q).unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(
            &hits[0].line_text[hits[0].byte_range.0..hits[0].byte_range.1],
            "PIX-1"
        );
        assert_eq!(
            &hits[1].line_text[hits[1].byte_range.0..hits[1].byte_range.1],
            "PIX-23"
        );
    }

    #[test]
    fn regex_mode_surfaces_invalid_pattern() {
        let dir = fixture(&[("a.md", "anything")]);
        let mut q = substring("[unclosed");
        q.mode = QueryMode::Regex;
        let err = search(dir.path(), &q).unwrap_err();
        assert!(matches!(err, SearchError::InvalidRegex(_)));
    }

    #[test]
    fn results_are_sorted_by_relative_path() {
        let dir = fixture(&[
            ("zeta.md", "match here\n"),
            ("alpha.md", "match too\n"),
            ("nested/mid.md", "match also\n"),
        ]);
        let hits = search(dir.path(), &substring("match")).unwrap();
        let paths: Vec<_> = hits.iter().map(|m| m.relative_path.clone()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "results not sorted by relative_path");
    }

    #[test]
    fn non_md_files_are_skipped() {
        let dir = fixture(&[("kept.md", "needle\n"), ("skipped.txt", "needle\n")]);
        let hits = search(dir.path(), &substring("needle")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relative_path, PathBuf::from("kept.md"));
    }

    #[test]
    fn folder_scope_restricts_search() {
        let dir = fixture(&[("outside.md", "needle\n"), ("eng/inside.md", "needle\n")]);
        let mut q = substring("needle");
        q.folder = Some(PathBuf::from("eng"));
        let hits = search(dir.path(), &q).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relative_path, PathBuf::from("eng/inside.md"));
    }

    #[test]
    fn folder_scope_missing_returns_not_found() {
        let dir = fixture(&[("a.md", "x")]);
        let mut q = substring("x");
        q.folder = Some(PathBuf::from("nope"));
        let err = search(dir.path(), &q).unwrap_err();
        assert!(matches!(err, SearchError::NotFound(_)));
    }

    #[test]
    fn csignore_skips_ignored_files() {
        let dir = fixture(&[
            (".csignore", "secret.md\n"),
            ("secret.md", "needle inside ignored\n"),
            ("public.md", "needle inside public\n"),
        ]);
        let hits = search(dir.path(), &substring("needle")).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].relative_path, PathBuf::from("public.md"));
    }

    #[test]
    fn utf8_boundary_respected_in_match_offsets() {
        // 'ö' is two bytes; ensure byte_range starts on a char
        // boundary even when the pattern starts after a multi-byte
        // char.
        let dir = fixture(&[("u.md", "über foo bar\n")]);
        let hits = search(dir.path(), &substring("foo")).unwrap();
        assert_eq!(hits.len(), 1);
        let m = &hits[0];
        assert!(
            m.line_text.is_char_boundary(m.byte_range.0)
                && m.line_text.is_char_boundary(m.byte_range.1),
            "byte_range {:?} not on UTF-8 boundaries in {:?}",
            m.byte_range,
            m.line_text
        );
        assert_eq!(&m.line_text[m.byte_range.0..m.byte_range.1], "foo");
    }
}
