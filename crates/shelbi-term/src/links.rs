//! Clickable URLs and file paths in terminal output.
//!
//! [`find_links`] scans one line of text for URLs (`https://...`, any
//! `scheme://`) and file paths with an optional `:line` or `:line:col`
//! suffix (`src/main.rs:42:5`, `./build.sh`, `~/notes.md`, `/etc/hosts`).
//! [`link_at`] runs it over the logical line under a grid point (a URL that
//! wrapped across rows is one link) and returns the link's cells, so a viewer
//! can underline it and open it. [`path_candidates`] turns a detected path
//! into the files it may name, for the viewer to check.
//!
//! The rules lean toward fewer false links over catching every path, since an
//! underline under ordinary prose is noise:
//!
//! - A **URL** is a scheme (`[a-z][a-z0-9+.-]*`) followed by `://` and the
//!   characters a URL may hold. Trailing sentence punctuation (`.,:;!?`) and a
//!   closing bracket with no opener inside the URL are not part of it, so
//!   `(see https://x.dev/a).` links `https://x.dev/a`.
//! - A **path** is a run of path characters (letters, digits, and
//!   `/ . _ - ~ + @ # % =`) between delimiters (whitespace, quotes, brackets,
//!   `,` `;` `|` `│`). It counts when it starts with `/`, `./`, `../`, or
//!   `~/`; or contains a `/` and ends in a file name with an extension
//!   (`src/lib.rs`); or is any file name with an extension followed by a line
//!   number (`lib.rs:42`). So `and/or`, `e.g.`, and `v1.2.3` are not links,
//!   and neither is a bare `README.md` (no line number).

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};

use crate::selection::Point;

/// The furthest a logical line is followed across wrapped rows in each
/// direction when looking for a link (a long URL wraps a few rows; this
/// bounds the work on a pathological single-line flood).
const MAX_WRAPPED_ROWS: i32 = 32;

/// What a link points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// A URL, as written.
    Url(String),
    /// A file path, as written (relative paths resolve against the session's
    /// working directory; see [`path_candidates`]), with the line and column
    /// it named, if any.
    Path {
        /// The path text.
        path: String,
        /// The 1-based line number after a `:`.
        line: Option<u32>,
        /// The 1-based column after a second `:`.
        column: Option<u32>,
    },
}

/// A link found in a line of text: its target and the character range
/// (`start..end`, character indices, not bytes) it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextLink {
    /// What it points at.
    pub target: LinkTarget,
    /// First character.
    pub start: usize,
    /// One past the last character.
    pub end: usize,
}

/// A link in the grid: its target and the inclusive range of cells it covers
/// (which may span wrapped rows), in grid-line coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    /// What it points at.
    pub target: LinkTarget,
    /// The first cell.
    pub start: Point,
    /// The last cell (for a wide character, its first cell).
    pub end: Point,
}

impl Link {
    /// The columns this link covers on grid `line`, as an inclusive
    /// `(first, last)` pair in a grid `cols` wide, or `None` when the link
    /// isn't on that line. For underlining.
    pub fn row_span(&self, line: i32, cols: u16) -> Option<(u16, u16)> {
        if line < self.start.line || line > self.end.line || cols == 0 {
            return None;
        }
        let first = if line == self.start.line { self.start.col } else { 0 };
        let last = if line == self.end.line { self.end.col } else { cols - 1 };
        (first <= last).then_some((first, last.min(cols - 1)))
    }
}

/// Every URL and path in `text`, left to right. See the module docs for the
/// rules.
pub fn find_links(text: &str) -> Vec<TextLink> {
    let chars: Vec<char> = text.chars().collect();
    let mut links = find_urls(&chars);
    let mut paths = find_paths(&chars, &links);
    links.append(&mut paths);
    links.sort_by_key(|l| l.start);
    links
}

/// The link under grid point `at`, looking across the wrapped rows of its
/// logical line. `None` when there is none, or `at` is outside the grid.
pub fn link_at(grid: &Grid<Cell>, at: Point) -> Option<Link> {
    let cols = grid.columns();
    let top = -(grid.history_size() as i32);
    let bottom = grid.screen_lines() as i32 - 1;
    if at.line < top || at.line > bottom || at.col as usize >= cols || cols == 0 {
        return None;
    }
    let wraps = |line: i32| grid[Line(line)][Column(cols - 1)].flags.contains(Flags::WRAPLINE);

    // The logical line: back up while the row above wraps into this one,
    // then run down while rows wrap.
    let mut first = at.line;
    while first > top && at.line - first < MAX_WRAPPED_ROWS && wraps(first - 1) {
        first -= 1;
    }
    let mut last = at.line;
    while last < bottom && last - at.line < MAX_WRAPPED_ROWS && wraps(last) {
        last += 1;
    }

    // The text, one char per cell (wide-character spacers skipped), and the
    // cell each char came from.
    let mut text = String::new();
    let mut cells = Vec::new();
    for line in first..=last {
        let row = &grid[Line(line)];
        for col in 0..cols {
            let cell = &row[Column(col)];
            if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                continue;
            }
            text.push(if cell.c == '\0' { ' ' } else { cell.c });
            cells.push(Point::new(line, col as u16));
        }
    }

    // The char under `at` (a wide character's spacer maps to the character).
    let index = cells.iter().rposition(|p| *p <= at)?;
    find_links(&text)
        .into_iter()
        .find(|l| l.start <= index && index < l.end)
        .map(|l| Link {
            target: l.target,
            start: cells[l.start],
            end: cells[l.end - 1],
        })
}

/// The files a detected path may name, most likely first: `~/` expands to
/// `home`, an absolute path is itself, and a relative one resolves against
/// `cwd`. A `git diff` side prefix (`a/src/x.rs`, `b/src/x.rs`) also tries
/// the path without it. The viewer opens the first that exists.
pub fn path_candidates(path: &str, cwd: Option<&std::path::Path>, home: Option<&std::path::Path>) -> Vec<std::path::PathBuf> {
    use std::path::PathBuf;
    if let Some(rest) = path.strip_prefix("~/") {
        return home.map(|h| vec![h.join(rest)]).unwrap_or_default();
    }
    if path.starts_with('/') {
        return vec![PathBuf::from(path)];
    }
    let Some(cwd) = cwd else {
        return Vec::new();
    };
    let mut out = vec![cwd.join(path)];
    if let Some(rest) = path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")) {
        out.push(cwd.join(rest));
    }
    out
}

// ---- URLs ----

fn is_scheme_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-')
}

/// Characters a URL may hold (RFC 3986's, plus non-ASCII for IRIs), minus
/// the ones that delimit it in prose.
fn is_url_char(c: char) -> bool {
    !c.is_whitespace()
        && !c.is_control()
        && !matches!(c, '<' | '>' | '"' | '\'' | '`' | '{' | '}' | '|' | '\\' | '^' | '│')
}

fn find_urls(chars: &[char]) -> Vec<TextLink> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= chars.len() {
        if !(chars[i] == ':' && chars.get(i + 1) == Some(&'/') && chars.get(i + 2) == Some(&'/')) {
            i += 1;
            continue;
        }
        // The scheme runs back from the colon; it must start with a letter
        // that isn't itself the tail of a word (`xhttps://` is not a URL
        // starting at `h`, but the scheme `xhttps` is fine).
        let mut start = i;
        while start > 0 && is_scheme_char(chars[start - 1]) {
            start -= 1;
        }
        while start < i && !chars[start].is_ascii_alphabetic() {
            start += 1;
        }
        let scheme_len = i - start;
        let mut end = i + 3;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        end = trim_url_end(chars, start, end);
        if scheme_len == 0 || scheme_len > 32 || end <= i + 3 {
            i += 3;
            continue;
        }
        out.push(TextLink {
            target: LinkTarget::Url(chars[start..end].iter().collect()),
            start,
            end,
        });
        i = end;
    }
    out
}

/// Drop trailing sentence punctuation and unbalanced closing brackets.
fn trim_url_end(chars: &[char], start: usize, mut end: usize) -> usize {
    loop {
        let Some(&last) = chars.get(end.wrapping_sub(1)).filter(|_| end > start) else {
            return end;
        };
        let unbalanced = |open: char, close: char| {
            let body = &chars[start..end];
            body.iter().filter(|&&c| c == close).count() > body.iter().filter(|&&c| c == open).count()
        };
        let drop = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '*' => true,
            ')' => unbalanced('(', ')'),
            ']' => unbalanced('[', ']'),
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= 1;
    }
}

// ---- paths ----

fn is_path_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '/' | '.' | '_' | '-' | '~' | '+' | '@' | '#' | '%' | '=' | ':')
}

fn find_paths(chars: &[char], urls: &[TextLink]) -> Vec<TextLink> {
    let in_url = |i: usize| urls.iter().any(|u| u.start <= i && i < u.end);
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !is_path_char(chars[i]) || in_url(i) {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && is_path_char(chars[i]) && !in_url(i) {
            i += 1;
        }
        // Trailing punctuation ends the sentence, not the path.
        let mut end = i;
        while end > start && matches!(chars[end - 1], '.' | ':' | ',') {
            end -= 1;
        }
        let token: String = chars[start..end].iter().collect();
        if let Some(target) = parse_path(&token) {
            out.push(TextLink { target, start, end });
        }
    }
    out
}

/// `path`, `path:line`, or `path:line:col`, if it reads as a path (see the
/// module docs).
fn parse_path(token: &str) -> Option<LinkTarget> {
    let mut parts = token.split(':');
    let path = parts.next()?;
    let line = match parts.next() {
        None => None,
        Some(n) => Some(parse_number(n)?),
    };
    let column = match parts.next() {
        None => None,
        Some(n) => Some(parse_number(n)?),
    };
    if parts.next().is_some() || path.is_empty() || !path.chars().any(char::is_alphanumeric) {
        return None;
    }
    let rooted = path.starts_with('/')
        || path.starts_with("./")
        || path.starts_with("../")
        || path.starts_with("~/");
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let has_extension = has_extension(file_name);
    let counts = rooted || (path.contains('/') && has_extension) || (has_extension && line.is_some());
    // `//` alone or a URL-ish `//host` is not a path.
    if !counts || path.starts_with("//") {
        return None;
    }
    Some(LinkTarget::Path { path: path.to_string(), line, column })
}

fn parse_number(s: &str) -> Option<u32> {
    (!s.is_empty() && s.len() <= 9 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok())?
}

/// `name.ext` with a non-empty name and an extension that starts with a
/// letter (`lib.rs`, `index.d.ts`, `clip.mp4`; not `v1.2`, `e.g`, or `.env`).
fn has_extension(file_name: &str) -> bool {
    let Some((name, ext)) = file_name.rsplit_once('.') else {
        return false;
    };
    !name.is_empty()
        && name.chars().any(char::is_alphanumeric)
        && !ext.is_empty()
        && ext.len() <= 12
        && ext.starts_with(|c: char| c.is_ascii_alphabetic())
        && ext.chars().all(|c| c.is_ascii_alphanumeric())
        // `e.g` and `i.e`: a one-letter name and a one-letter extension.
        && !(name.len() == 1 && ext.len() == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::view::TerminalView;
    use crate::Size;

    fn targets(text: &str) -> Vec<(String, LinkTarget)> {
        let chars: Vec<char> = text.chars().collect();
        find_links(text)
            .into_iter()
            .map(|l| (chars[l.start..l.end].iter().collect(), l.target))
            .collect()
    }

    fn url(s: &str) -> LinkTarget {
        LinkTarget::Url(s.into())
    }

    fn path(p: &str, line: Option<u32>, column: Option<u32>) -> LinkTarget {
        LinkTarget::Path { path: p.into(), line, column }
    }

    #[test]
    fn finds_urls_with_any_scheme() {
        assert_eq!(
            targets("see https://example.com/a?b=1#c and ftp://host/x"),
            vec![
                ("https://example.com/a?b=1#c".into(), url("https://example.com/a?b=1#c")),
                ("ftp://host/x".into(), url("ftp://host/x")),
            ]
        );
        assert_eq!(targets("git+ssh://git@github.com/o/r.git")[0].1, url("git+ssh://git@github.com/o/r.git"));
    }

    #[test]
    fn url_trailing_punctuation_and_brackets_are_trimmed() {
        assert_eq!(targets("(see https://x.dev/a).")[0].1, url("https://x.dev/a"));
        assert_eq!(targets("Done: https://x.dev/a, then")[0].1, url("https://x.dev/a"));
        // A balanced paren inside the URL stays (a Wikipedia link).
        assert_eq!(
            targets("https://en.wikipedia.org/wiki/Rust_(language)!")[0].1,
            url("https://en.wikipedia.org/wiki/Rust_(language)")
        );
        assert_eq!(targets("<https://x.dev/a>")[0].1, url("https://x.dev/a"));
        assert_eq!(targets("\"https://x.dev/q\"")[0].1, url("https://x.dev/q"));
    }

    #[test]
    fn a_bare_scheme_is_not_a_url() {
        assert!(targets("https:// and ://nothing").is_empty());
    }

    #[test]
    fn finds_paths_with_line_and_column() {
        assert_eq!(
            targets("  --> src/main.rs:42:5"),
            vec![("src/main.rs:42:5".into(), path("src/main.rs", Some(42), Some(5)))]
        );
        assert_eq!(targets("at lib.rs:7:")[0].1, path("lib.rs", Some(7), None));
        assert_eq!(targets("error in ./build.sh")[0].1, path("./build.sh", None, None));
        assert_eq!(targets("cat ~/notes.md")[0].1, path("~/notes.md", None, None));
        assert_eq!(targets("edit /etc/hosts.")[0].1, path("/etc/hosts", None, None));
        assert_eq!(targets("(../x/y)")[0].1, path("../x/y", None, None));
        assert_eq!(
            targets("Modified crates/shelbi-term/src/links.rs, desktop/README.md"),
            vec![
                ("crates/shelbi-term/src/links.rs".into(), path("crates/shelbi-term/src/links.rs", None, None)),
                ("desktop/README.md".into(), path("desktop/README.md", None, None)),
            ]
        );
    }

    #[test]
    fn prose_is_not_a_path() {
        for text in [
            "and/or",
            "e.g. this",
            "version v1.2.3",
            "README.md",
            "ratio 3/4",
            "foo:bar",
            "time 12:30",
            "a/b:c",
            "//comment",
            "the end.",
            "x.y",
        ] {
            assert!(targets(text).is_empty(), "{text:?} gave {:?}", targets(text));
        }
    }

    #[test]
    fn urls_take_precedence_over_paths() {
        assert_eq!(targets("https://x.dev/src/main.rs:4"), vec![("https://x.dev/src/main.rs:4".into(), url("https://x.dev/src/main.rs:4"))]);
    }

    #[test]
    fn link_ranges_are_character_indices() {
        // Wide characters before the link count as one character each.
        let l = &find_links("漢字 src/a.rs:1")[0];
        assert_eq!((l.start, l.end), (3, 13));
    }

    #[test]
    fn link_at_maps_cells_and_follows_wrapped_rows() {
        let mut v = TerminalView::new(Size::new(20, 5));
        // A 29-character URL from column 3 of a 20-column grid wraps onto the
        // second row.
        v.feed(b"go https://example.com/a/b/c/d/e now");
        let link = v.link_at(Point::new(1, 3)).expect("a link on the wrapped row");
        assert_eq!(link.target, url("https://example.com/a/b/c/d/e"));
        assert_eq!((link.start, link.end), (Point::new(0, 3), Point::new(1, 11)));
        assert_eq!(link.row_span(0, 20), Some((3, 19)));
        assert_eq!(link.row_span(1, 20), Some((0, 11)));
        assert_eq!(link.row_span(2, 20), None);
        // The same link from its first row; nothing on "go" or "now".
        assert_eq!(v.link_at(Point::new(0, 5)), Some(link));
        assert_eq!(v.link_at(Point::new(0, 0)), None);
        assert_eq!(v.link_at(Point::new(1, 15)), None);
    }

    #[test]
    fn link_at_handles_wide_characters() {
        let mut v = TerminalView::new(Size::new(30, 3));
        v.feed("漢字 src/a.rs:9".as_bytes());
        // 漢 and 字 take two cells each, so the path starts at column 5.
        let link = v.link_at(Point::new(0, 6)).expect("path");
        assert_eq!(link.target, path("src/a.rs", Some(9), None));
        assert_eq!((link.start.col, link.end.col), (5, 14));
    }

    #[test]
    fn path_candidates_resolve_home_absolute_relative_and_diff_prefixes() {
        use std::path::{Path, PathBuf};
        let cwd = Path::new("/w");
        let home = Path::new("/h");
        assert_eq!(path_candidates("~/n.md", Some(cwd), Some(home)), vec![PathBuf::from("/h/n.md")]);
        assert_eq!(path_candidates("/etc/hosts", Some(cwd), Some(home)), vec![PathBuf::from("/etc/hosts")]);
        assert_eq!(path_candidates("src/a.rs", Some(cwd), None), vec![PathBuf::from("/w/src/a.rs")]);
        assert_eq!(
            path_candidates("b/src/a.rs", Some(cwd), None),
            vec![PathBuf::from("/w/b/src/a.rs"), PathBuf::from("/w/src/a.rs")]
        );
        assert!(path_candidates("src/a.rs", None, None).is_empty());
    }
}
