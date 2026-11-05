//! `git diff` output into [`FileDiff`]s.
//!
//! The patch comes from [`super::git`], which pins the options that change its
//! shape (prefixes, color, external drivers), so one grammar holds:
//! `diff --git` starts a file, header lines say how it changed, `@@` starts a
//! hunk whose lengths say where it ends. Counting the hunk's lines is what
//! keeps a deleted `-- a/x` from reading as a header.

use super::intraline;
use super::model::{DiffLine, Eol, FileDiff, FileStatus, Fnv, Hunk, LineKind};

/// Parse a whole patch.
pub fn parse_patch(patch: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut file: Option<Building> = None;
    let mut hunk: Option<OpenHunk> = None;
    for raw in patch.split('\n') {
        if let Some(open) = &mut hunk {
            if open.old_left > 0 || open.new_left > 0 {
                let building = file.as_mut().expect("a hunk belongs to a file");
                open.push(raw, building);
                continue;
            }
            if let Some(rest) = raw.strip_prefix('\\') {
                // `\ No newline at end of file`, after the hunk's last line.
                if rest.contains("No newline") {
                    open.last_has_no_newline();
                }
                continue;
            }
            let building = file.as_mut().expect("a hunk belongs to a file");
            building.hunks.push(hunk.take().unwrap().hunk);
        }
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            if let Some(done) = file.take() {
                files.push(done.finish());
            }
            file = Some(Building::new(rest));
        } else if let Some(building) = &mut file {
            if let Some(open) = OpenHunk::parse(raw) {
                hunk = Some(open);
            } else {
                building.header(raw);
            }
        }
    }
    if let Some(mut building) = file {
        if let Some(open) = hunk {
            building.hunks.push(open.hunk);
        }
        files.push(building.finish());
    }
    files
}

/// A hunk being read, with how many lines of each side it has left.
struct OpenHunk {
    hunk: Hunk,
    old_left: u32,
    new_left: u32,
    old_next: u32,
    new_next: u32,
}

impl OpenHunk {
    /// `@@ -a,b +c,d @@ section`, if `line` is one.
    fn parse(line: &str) -> Option<OpenHunk> {
        let rest = line.strip_prefix("@@ -")?;
        let (ranges, section) = rest.split_once(" @@")?;
        let (old, new) = ranges.split_once(" +")?;
        let (old_start, old_count) = range(old)?;
        let (new_start, new_count) = range(new)?;
        Some(OpenHunk {
            old_left: old_count,
            new_left: new_count,
            old_next: old_start,
            new_next: new_start,
            hunk: Hunk {
                old_start,
                old_count,
                new_start,
                new_count,
                section: section.trim().to_string(),
                lines: Vec::new(),
            },
        })
    }

    /// One body line.
    fn push(&mut self, raw: &str, file: &mut Building) {
        let (kind, body) = match raw.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &raw[1..]),
            Some(b'-') => (LineKind::Deleted, &raw[1..]),
            Some(b' ') => (LineKind::Context, &raw[1..]),
            Some(b'\\') => {
                if raw.contains("No newline") {
                    self.last_has_no_newline();
                }
                return;
            }
            // `diff.suppressBlankEmpty` writes a blank context line bare.
            _ => (LineKind::Context, raw),
        };
        let (old, new) = match kind {
            LineKind::Context => (Some(self.old_next), Some(self.new_next)),
            LineKind::Added => (None, Some(self.new_next)),
            LineKind::Deleted => (Some(self.old_next), None),
        };
        if old.is_some() {
            self.old_next += 1;
            self.old_left = self.old_left.saturating_sub(1);
        }
        if new.is_some() {
            self.new_next += 1;
            self.new_left = self.new_left.saturating_sub(1);
        }
        match kind {
            LineKind::Added => file.additions += 1,
            LineKind::Deleted => file.deletions += 1,
            LineKind::Context => {}
        }
        let (text, eol) = match body.strip_suffix('\r') {
            Some(text) => (text, Eol::Crlf),
            None => (body, Eol::Lf),
        };
        self.hunk.lines.push(DiffLine {
            kind,
            old,
            new,
            text: text.to_string(),
            eol,
            intraline: Vec::new(),
        });
    }

    fn last_has_no_newline(&mut self) {
        if let Some(last) = self.hunk.lines.last_mut() {
            last.eol = Eol::None;
        }
    }
}

/// `a,b` (or `a`, whose length is 1).
fn range(text: &str) -> Option<(u32, u32)> {
    match text.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((text.parse().ok()?, 1)),
    }
}

/// A file's headers as they're read.
struct Building {
    git_header: String,
    old_path: Option<String>,
    new_path: Option<String>,
    rename: bool,
    created: bool,
    deleted: bool,
    binary: bool,
    similarity: Option<u8>,
    old_mode: Option<String>,
    new_mode: Option<String>,
    index: Option<String>,
    additions: u32,
    deletions: u32,
    hunks: Vec<Hunk>,
}

impl Building {
    fn new(git_header: &str) -> Building {
        Building {
            git_header: git_header.to_string(),
            old_path: None,
            new_path: None,
            rename: false,
            created: false,
            deleted: false,
            binary: false,
            similarity: None,
            old_mode: None,
            new_mode: None,
            index: None,
            additions: 0,
            deletions: 0,
            hunks: Vec::new(),
        }
    }

    fn header(&mut self, line: &str) {
        if let Some(rest) = line.strip_prefix("--- ") {
            self.old_path = side_path(rest).or(self.old_path.take());
        } else if let Some(rest) = line.strip_prefix("+++ ") {
            self.new_path = side_path(rest).or(self.new_path.take());
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            self.rename = true;
            self.old_path = Some(unquote(rest));
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            self.rename = true;
            self.new_path = Some(unquote(rest));
        } else if let Some(rest) = line.strip_prefix("similarity index ") {
            self.similarity = rest.trim_end_matches('%').parse().ok();
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            self.created = true;
            self.new_mode = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            self.deleted = true;
            self.old_mode = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("old mode ") {
            self.old_mode = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("new mode ") {
            self.new_mode = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("index ") {
            self.index = Some(rest.to_string());
        } else if line.starts_with("Binary files ") || line.starts_with("GIT binary patch") {
            self.binary = true;
        }
    }

    fn finish(self) -> FileDiff {
        // An empty file, a mode change and a binary file have no `---`/`+++`
        // to read the paths from, only the `diff --git` line.
        let (git_old, git_new) = header_paths(&self.git_header);
        let old = self.old_path.or(git_old);
        let new = self.new_path.or(git_new);
        let status = if self.created {
            FileStatus::Added
        } else if self.deleted {
            FileStatus::Deleted
        } else if self.rename {
            FileStatus::Renamed
        } else {
            FileStatus::Modified
        };
        let path = match status {
            FileStatus::Deleted => old.clone().or(new),
            _ => new.or(old.clone()),
        }
        .unwrap_or_default();
        let mode_change = match (&self.old_mode, &self.new_mode) {
            (Some(a), Some(b)) if !self.created && !self.deleted && a != b => Some((a.clone(), b.clone())),
            _ => None,
        };
        let mut file = FileDiff {
            old_path: if self.rename { old } else { None },
            path,
            status,
            binary: self.binary,
            additions: self.additions,
            deletions: self.deletions,
            similarity: self.similarity,
            mode_change,
            untracked: false,
            omitted: None,
            hunks: self.hunks,
            fingerprint: 0,
        };
        file.fingerprint = fingerprint(&file, self.index.as_deref());
        for hunk in &mut file.hunks {
            intraline::annotate(hunk);
        }
        file
    }
}

/// A hash of what changed: the paths, status, and every changed or context
/// line's text, but not its line number. A binary file has no lines, so its
/// `index` line (the two blob ids) stands in.
fn fingerprint(file: &FileDiff, index: Option<&str>) -> u64 {
    let mut hash = Fnv::new();
    hash.write(&[file.status.letter() as u8]);
    hash.write(file.path.as_bytes());
    if let Some(old) = &file.old_path {
        hash.write(old.as_bytes());
    }
    if let Some((a, b)) = &file.mode_change {
        hash.write(a.as_bytes());
        hash.write(b.as_bytes());
    }
    if file.binary {
        hash.write(index.unwrap_or("").as_bytes());
    }
    for line in file.hunks.iter().flat_map(|h| &h.lines) {
        let tag = match line.kind {
            LineKind::Context => b' ',
            LineKind::Added => b'+',
            LineKind::Deleted => b'-',
        };
        let eol = match line.eol {
            Eol::Lf => b'n',
            Eol::Crlf => b'r',
            Eol::None => b'0',
        };
        hash.write(&[tag, eol]);
        hash.write(line.text.as_bytes());
        hash.write(b"\n");
    }
    hash.finish()
}

/// The path on a `---` / `+++` line: none for `/dev/null`, the `a/` or `b/`
/// dropped otherwise. Git ends a name with whitespace in it with a tab.
fn side_path(text: &str) -> Option<String> {
    let text = text.strip_suffix('\t').unwrap_or(text);
    if text == "/dev/null" {
        return None;
    }
    let path = unquote(text);
    Some(strip_prefix(&path).to_string())
}

fn strip_prefix(path: &str) -> &str {
    path.strip_prefix("a/").or_else(|| path.strip_prefix("b/")).unwrap_or(path)
}

/// The two paths on a `diff --git` line. Unquoted paths can hold spaces, so
/// the split is the one whose two sides name the same path, which they do for
/// everything that reaches here (a rename has its own lines).
fn header_paths(rest: &str) -> (Option<String>, Option<String>) {
    if rest.starts_with('"') || rest.ends_with('"') {
        let (a, b) = split_quoted(rest);
        return (a.map(|p| strip_prefix(&p).to_string()), b.map(|p| strip_prefix(&p).to_string()));
    }
    let mut first = None;
    for (at, _) in rest.match_indices(" b/") {
        let (a, b) = (&rest[..at], &rest[at + 1..]);
        first.get_or_insert((a, b));
        if strip_prefix(a) == strip_prefix(b) {
            return (Some(strip_prefix(a).to_string()), Some(strip_prefix(b).to_string()));
        }
    }
    match first {
        Some((a, b)) => (Some(strip_prefix(a).to_string()), Some(strip_prefix(b).to_string())),
        None => (None, None),
    }
}

/// Two header paths, at least one quoted.
fn split_quoted(rest: &str) -> (Option<String>, Option<String>) {
    let mut end = None;
    if rest.starts_with('"') {
        let bytes = rest.as_bytes();
        let mut i = 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2,
                b'"' => {
                    end = Some(i);
                    break;
                }
                _ => i += 1,
            }
        }
        let Some(end) = end else { return (None, None) };
        return (Some(unquote(&rest[..=end])), Some(unquote(rest[end + 1..].trim_start())));
    }
    match rest.find(" \"") {
        Some(at) => (Some(unquote(&rest[..at])), Some(unquote(&rest[at + 1..]))),
        None => (None, None),
    }
}

/// A path git may have C-quoted (`"sp\303\251c\tial"`).
pub(crate) fn unquote(text: &str) -> String {
    let Some(inner) = text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) else {
        return text.to_string();
    };
    let bytes = inner.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' || i + 1 == bytes.len() {
            out.push(bytes[i]);
            i += 1;
            continue;
        }
        i += 1;
        match bytes[i] {
            b'n' => out.push(b'\n'),
            b't' => out.push(b'\t'),
            b'r' => out.push(b'\r'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            d @ b'0'..=b'3' if i + 2 < bytes.len() => {
                let value = u32::from(d - b'0') * 64
                    + u32::from(bytes[i + 1].wrapping_sub(b'0')) * 8
                    + u32::from(bytes[i + 2].wrapping_sub(b'0'));
                out.push(value as u8);
                i += 2;
            }
            other => out.push(other),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(patch: &str) -> FileDiff {
        let mut files = parse_patch(patch);
        assert_eq!(files.len(), 1, "{patch}");
        files.remove(0)
    }

    #[test]
    fn a_modified_file_numbers_both_sides() {
        let f = one("diff --git a/src/x.rs b/src/x.rs\nindex 1..2 100644\n--- a/src/x.rs\n+++ b/src/x.rs\n@@ -10,3 +10,3 @@ fn main() {\n ctx\n-old\n+new\n after\n");
        assert_eq!((f.path.as_str(), f.status, f.additions, f.deletions), ("src/x.rs", FileStatus::Modified, 1, 1));
        let h = &f.hunks[0];
        assert_eq!(h.section, "fn main() {");
        assert_eq!(h.header(), "@@ -10,3 +10,3 @@ fn main() {");
        let nums: Vec<_> = h.lines.iter().map(|l| (l.kind, l.old, l.new)).collect();
        assert_eq!(
            nums,
            [
                (LineKind::Context, Some(10), Some(10)),
                (LineKind::Deleted, Some(11), None),
                (LineKind::Added, None, Some(11)),
                (LineKind::Context, Some(12), Some(12)),
            ]
        );
    }

    #[test]
    fn a_deleted_line_that_looks_like_a_header_stays_a_line() {
        let f = one("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1 @@\n--- a/y\n-diff --git a/y b/y\n+kept\n");
        assert_eq!(f.deletions, 2);
        assert_eq!(f.hunks[0].lines[0].text, "-- a/y");
        assert_eq!(f.hunks[0].lines[1].text, "diff --git a/y b/y");
    }

    #[test]
    fn line_endings_come_off_the_text_and_are_kept_apart() {
        let f = one("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,2 +1,2 @@\n-a\r\n+a\n-b\n\\ No newline at end of file\n+b\n");
        let lines = &f.hunks[0].lines;
        assert_eq!((lines[0].text.as_str(), lines[0].eol), ("a", Eol::Crlf));
        assert_eq!((lines[1].text.as_str(), lines[1].eol), ("a", Eol::Lf));
        assert_eq!((lines[2].text.as_str(), lines[2].eol), ("b", Eol::None));
        assert_eq!((lines[3].text.as_str(), lines[3].eol), ("b", Eol::Lf));
    }

    #[test]
    fn no_newline_on_the_hunks_last_line() {
        let f = one("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a\n+b\n\\ No newline at end of file\n");
        assert_eq!(f.hunks[0].lines[1].eol, Eol::None);
    }

    #[test]
    fn renames_adds_deletes_binary_and_mode_changes() {
        let f = one("diff --git a/old name.rs b/new name.rs\nsimilarity index 91%\nrename from old name.rs\nrename to new name.rs\n");
        assert_eq!((f.status, f.path.as_str(), f.old_path.as_deref(), f.similarity), (FileStatus::Renamed, "new name.rs", Some("old name.rs"), Some(91)));

        let f = one("diff --git a/empty b/empty\nnew file mode 100644\nindex 0000000..e69de29\n");
        assert_eq!((f.status, f.path.as_str()), (FileStatus::Added, "empty"));

        let f = one("diff --git a/gone b/gone\ndeleted file mode 100644\nindex 1..0\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n");
        assert_eq!((f.status, f.path.as_str(), f.deletions), (FileStatus::Deleted, "gone", 1));

        let f = one("diff --git a/i.png b/i.png\nindex 1..2 100644\nBinary files a/i.png and b/i.png differ\n");
        assert!(f.binary && f.hunks.is_empty());

        let f = one("diff --git a/s.sh b/s.sh\nold mode 100644\nnew mode 100755\n");
        assert_eq!(f.mode_change, Some(("100644".into(), "100755".into())));
        assert_eq!(f.path, "s.sh");
    }

    #[test]
    fn quoted_and_spaced_paths() {
        assert_eq!(unquote("\"sp\\303\\251c\\tial\""), "spéc\tial");
        let f = one("diff --git \"a/sp\\303\\251c\" \"b/sp\\303\\251c\"\nnew file mode 100644\n");
        assert_eq!(f.path, "spéc");
        let f = one("diff --git a/a b/c b/a b/c\nnew file mode 100644\n");
        assert_eq!(f.path, "a b/c");
        let f = one("diff --git a/with space b/with space\n--- a/with space\t\n+++ b/with space\t\n@@ -1 +1 @@\n-a\n+b\n");
        assert_eq!(f.path, "with space");
    }

    #[test]
    fn an_empty_patch_is_no_files() {
        assert!(parse_patch("").is_empty());
    }

    #[test]
    fn the_fingerprint_ignores_where_a_change_landed() {
        let at = |line: u32, text: &str| {
            one(&format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -{line},1 +{line},1 @@\n-a\n+{text}\n")).fingerprint
        };
        assert_eq!(at(1, "b"), at(50, "b"));
        assert_ne!(at(1, "b"), at(1, "c"));
    }

    #[test]
    fn several_files_and_hunks() {
        let files = parse_patch("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-1\n+2\n@@ -9 +9 @@\n-3\n+4\ndiff --git a/b b/b\n--- a/b\n+++ b/b\n@@ -1 +1 @@\n-1\n+2\n");
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[1].path, "b");
    }
}
