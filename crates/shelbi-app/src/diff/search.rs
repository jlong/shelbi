//! Search over every file's lines. Smart case, as in Skim: a query with no
//! capital ignores case, one with a capital matches exactly. Only diff
//! content is searched (added, deleted and context lines).

use regex::RegexBuilder;

use super::model::Changeset;

/// One match: a byte range of a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchMatch {
    pub file: u32,
    pub hunk: u32,
    pub line: u32,
    pub start: u32,
    pub end: u32,
}

/// Every match of `query`, in reading order.
pub fn search(changeset: &Changeset, query: &str) -> Vec<SearchMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let case_insensitive = !query.chars().any(char::is_uppercase);
    let Ok(re) = RegexBuilder::new(&regex::escape(query)).case_insensitive(case_insensitive).build() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (f, file) in changeset.files.iter().enumerate() {
        for (h, hunk) in file.hunks.iter().enumerate() {
            for (l, line) in hunk.lines.iter().enumerate() {
                for m in re.find_iter(&line.text) {
                    out.push(SearchMatch {
                        file: f as u32,
                        hunk: h as u32,
                        line: l as u32,
                        start: m.start() as u32,
                        end: m.end() as u32,
                    });
                }
            }
        }
    }
    out
}

/// The matches on one line, from a list in reading order.
pub fn matches_on(matches: &[SearchMatch], file: u32, hunk: u32, line: u32) -> &[SearchMatch] {
    let key = (file, hunk, line);
    let start = matches.partition_point(|m| (m.file, m.hunk, m.line) < key);
    let end = start + matches[start..].partition_point(|m| (m.file, m.hunk, m.line) == key);
    &matches[start..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_patch;

    fn changeset() -> Changeset {
        Changeset {
            files: parse_patch(
                "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,2 +1,2 @@\n ctx Foo foo\n-removed foo\n+added FOO\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-nothing\n+foo again\n",
            ),
            ..Changeset::default()
        }
    }

    #[test]
    fn lowercase_ignores_case_across_every_file() {
        let m = search(&changeset(), "foo");
        assert_eq!(m.len(), 5);
        assert_eq!((m[0].file, m[0].line, m[0].start, m[0].end), (0, 0, 4, 7));
        assert_eq!(m[4].file, 1);
    }

    #[test]
    fn a_capital_makes_it_exact() {
        let m = search(&changeset(), "Foo");
        assert_eq!(m.len(), 1);
        assert_eq!((m[0].file, m[0].line), (0, 0));
        assert_eq!(search(&changeset(), "FOO").len(), 1);
    }

    #[test]
    fn the_query_is_text_not_a_pattern() {
        assert!(search(&changeset(), "f.o").is_empty());
        assert!(search(&changeset(), "").is_empty());
    }

    #[test]
    fn matches_on_finds_a_lines_matches() {
        let c = changeset();
        let all = search(&c, "foo");
        assert_eq!(matches_on(&all, 0, 0, 0).len(), 2);
        assert_eq!(matches_on(&all, 0, 0, 1).len(), 1);
        assert!(matches_on(&all, 1, 0, 0).is_empty());
    }
}
