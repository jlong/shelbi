//! The changeset as plain data: files, hunks and lines. Parsed once off the
//! UI thread ([`super::parse`]) and shared behind an `Arc`; everything a
//! client shows of it (rows, folds, the filter) is derived in [`super::rows`].

use std::ops::Range;

use serde::{Deserialize, Serialize};

/// How a file changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FileStatus {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl FileStatus {
    /// The status letter a file list shows (`A`, `M`, `D`, `R`).
    pub fn letter(self) -> char {
        match self {
            FileStatus::Added => 'A',
            FileStatus::Modified => 'M',
            FileStatus::Deleted => 'D',
            FileStatus::Renamed => 'R',
        }
    }
}

/// Which side of the diff a line number is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Old,
    New,
}

/// What a line is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineKind {
    Context,
    Added,
    Deleted,
}

/// How a line ended. The text never carries its line ending, so a change that
/// only touches line endings still has something to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Eol {
    Lf,
    Crlf,
    /// `\ No newline at end of file`.
    None,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    /// The line's number in the old file (context and deleted lines).
    pub old: Option<u32>,
    /// The line's number in the new file (context and added lines).
    pub new: Option<u32>,
    /// The text, without its line ending. Bytes that aren't UTF-8 read as
    /// U+FFFD.
    pub text: String,
    pub eol: Eol,
    /// The byte ranges of `text` that changed within the line, for a changed
    /// line paired with its counterpart. Empty when the whole line is new.
    pub intraline: Vec<Range<u32>>,
}

impl DiffLine {
    /// The line number on `side`, if the line is on that side.
    pub fn number(&self, side: Side) -> Option<u32> {
        match side {
            Side::Old => self.old,
            Side::New => self.new,
        }
    }

    /// The side a comment on this line anchors to: a deleted line's old
    /// number, otherwise its new one.
    pub fn side(&self) -> Side {
        match self.kind {
            LineKind::Deleted => Side::Old,
            _ => Side::New,
        }
    }
}

/// One `@@` block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// What follows the closing `@@` (usually the enclosing function).
    pub section: String,
    pub lines: Vec<DiffLine>,
}

impl Hunk {
    /// The `@@ -a,b +c,d @@ section` header.
    pub fn header(&self) -> String {
        let mut out = format!(
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        );
        if !self.section.is_empty() {
            out.push(' ');
            out.push_str(&self.section);
        }
        out
    }

    /// Whether the hunk has a line of `kind`.
    pub fn has(&self, kind: LineKind) -> bool {
        self.lines.iter().any(|l| l.kind == kind)
    }
}

/// One changed file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Its path: the new path, or the old one for a deleted file.
    pub path: String,
    /// The old path, for a rename.
    pub old_path: Option<String>,
    pub status: FileStatus,
    /// Git treats it as binary: there are no hunks to show.
    pub binary: bool,
    pub additions: u32,
    pub deletions: u32,
    /// The rename's similarity, in percent.
    pub similarity: Option<u8>,
    /// `(old mode, new mode)`, when the file mode changed.
    pub mode_change: Option<(String, String)>,
    /// Not tracked by git yet (a workspace's new file).
    pub untracked: bool,
    /// Why the body isn't here, when it was left out (too large to load).
    pub omitted: Option<String>,
    pub hunks: Vec<Hunk>,
    /// A hash of what changed in the file, independent of where in the file
    /// it landed: "viewed" is cleared when it differs ([`super::viewed`]).
    pub fingerprint: u64,
}

impl FileDiff {
    /// The file's name without its directory.
    pub fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }
}

/// One commit of the changeset, for "review one commit".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitInfo {
    pub sha: String,
    pub short: String,
    pub author: String,
    /// Commit time, seconds since the epoch.
    pub time: i64,
    pub subject: String,
}

/// Which changes a changeset shows (`Commits: all ▾`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommitScope {
    /// Everything the target is about: a review's branch, a workspace's
    /// branch plus its uncommitted work.
    #[default]
    All,
    /// A workspace's uncommitted work alone.
    Uncommitted,
    /// One commit.
    Commit(String),
}

/// A loaded changeset.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Changeset {
    pub scope: CommitScope,
    /// What the diff compares from: the merge-base, or a commit's parent.
    pub base: String,
    /// `HEAD` when it was read.
    pub head: String,
    pub files: Vec<FileDiff>,
    pub additions: u32,
    pub deletions: u32,
    /// The commits between the merge-base and `HEAD`, newest first.
    pub commits: Vec<CommitInfo>,
    /// New files left out of a workspace's diff because there were too many
    /// to read.
    pub omitted_untracked: usize,
}

impl Changeset {
    /// Number of lines in hunks, over every file.
    pub fn line_count(&self) -> usize {
        self.files.iter().flat_map(|f| &f.hunks).map(|h| h.lines.len()).sum()
    }

    /// The index of the file at `path`.
    pub fn file_index(&self, path: &str) -> Option<usize> {
        self.files.iter().position(|f| f.path == path)
    }
}

/// FNV-1a: a stable hash for values that are persisted. `DefaultHasher` makes
/// no promise across releases, and a viewed mark shouldn't depend on one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Fnv(u64);

impl Fnv {
    pub(crate) fn new() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    pub(crate) fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    pub(crate) fn finish(self) -> u64 {
        self.0
    }
}
