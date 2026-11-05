//! The view of a changeset as a flat list of rows: what a client scrolls.
//!
//! Folds, the hunk filter, unified or split, and comment threads all decide
//! which rows exist, so the table is rebuilt when one changes and never while
//! scrolling. A [`Row`] is a few integers: a client draws only the rows in its
//! viewport ([`RowTable::slice`]), looking each one's text up in the changeset,
//! so a large diff costs the same to scroll as a small one.

use std::collections::{HashMap, HashSet};

use super::comments::{Comment, Placement};
use super::intraline::hunk_pairs;
use super::model::{Changeset, FileDiff, Hunk, LineKind};

/// Unified, or old and new side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ViewMode {
    #[default]
    Unified,
    Split,
}

impl ViewMode {
    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Unified => "unified",
            ViewMode::Split => "split",
        }
    }
}

/// Which changed lines show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HunkFilter {
    #[default]
    All,
    Additions,
    Deletions,
}

impl HunkFilter {
    pub fn next(self) -> Self {
        match self {
            HunkFilter::All => HunkFilter::Additions,
            HunkFilter::Additions => HunkFilter::Deletions,
            HunkFilter::Deletions => HunkFilter::All,
        }
    }

    pub fn prev(self) -> Self {
        self.next().next()
    }

    pub fn label(self) -> &'static str {
        match self {
            HunkFilter::All => "all lines",
            HunkFilter::Additions => "additions",
            HunkFilter::Deletions => "deletions",
        }
    }

    /// Whether a line of `kind` shows.
    pub fn shows(self, kind: LineKind) -> bool {
        !matches!(
            (self, kind),
            (HunkFilter::Additions, LineKind::Deleted) | (HunkFilter::Deletions, LineKind::Added)
        )
    }

    /// Whether a hunk shows at all: it has a line of the filter's kind.
    fn shows_hunk(self, hunk: &Hunk) -> bool {
        match self {
            HunkFilter::All => true,
            HunkFilter::Additions => hunk.has(LineKind::Added),
            HunkFilter::Deletions => hunk.has(LineKind::Deleted),
        }
    }
}

/// What's folded. By path and hunk start rather than index, so folds survive
/// a refresh that changes the file list.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Folds {
    pub files: HashSet<String>,
    pub hunks: HashSet<(String, u32)>,
}

impl Folds {
    pub fn file_folded(&self, path: &str) -> bool {
        self.files.contains(path)
    }

    pub fn hunk_folded(&self, path: &str, hunk: &Hunk) -> bool {
        self.hunks.contains(&(path.to_string(), hunk.old_start))
    }

    /// Fold `path`'s hunk.
    pub fn set_hunk(&mut self, path: &str, hunk: &Hunk, folded: bool) {
        let key = (path.to_string(), hunk.old_start);
        if folded {
            self.hunks.insert(key);
        } else {
            self.hunks.remove(&key);
        }
    }

    pub fn set_file(&mut self, path: &str, folded: bool) {
        if folded {
            self.files.insert(path.to_string());
        } else {
            self.files.remove(path);
        }
    }

    /// Unfold everything.
    pub fn clear(&mut self) {
        self.files.clear();
        self.hunks.clear();
    }
}

/// Why a file has no lines to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NoticeKind {
    Binary,
    /// A rename with no edits.
    RenameOnly,
    /// A mode change with no edits.
    ModeOnly,
    /// An empty file was added or deleted.
    Empty,
    /// Left out: too large ([`FileDiff::omitted`]).
    Omitted,
}

impl NoticeKind {
    /// What a file's notice row says.
    pub fn text(self, file: &FileDiff) -> String {
        match self {
            NoticeKind::Binary => "Binary file".to_string(),
            NoticeKind::RenameOnly => match (&file.old_path, file.similarity) {
                (Some(old), Some(similarity)) if similarity < 100 => {
                    format!("Renamed from {old} ({similarity}% similar)")
                }
                (Some(old), _) => format!("Renamed from {old}, no changes"),
                (None, _) => "Renamed, no changes".to_string(),
            },
            NoticeKind::ModeOnly => match &file.mode_change {
                Some((old, new)) => format!("Mode changed from {old} to {new}"),
                None => "Mode changed".to_string(),
            },
            NoticeKind::Empty => "Empty file".to_string(),
            NoticeKind::Omitted => file.omitted.clone().unwrap_or_else(|| "Too large to show".to_string()),
        }
    }
}

/// Which part of a thread a row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadPart {
    /// The comment.
    Head,
    /// One reply, by index.
    Reply(u16),
    /// `▸ N replies`, collapsed.
    Summary,
}

/// One row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Row {
    /// A file's header. `folded` shows `▸` and hides everything below it.
    File { file: u32, folded: bool },
    /// A hunk's `@@` header. `folded` hides its lines.
    Hunk { file: u32, hunk: u32, folded: bool },
    /// One line, in the unified view.
    Line { file: u32, hunk: u32, line: u32 },
    /// One row of the split view: an old line, a new line, or a pair.
    Split { file: u32, hunk: u32, old: Option<u32>, new: Option<u32> },
    /// A part of a thread, by index in the [`Threads`]' list.
    Thread { file: u32, thread: u32, part: ThreadPart },
    /// Why a file has no lines.
    Notice { file: u32, kind: NoticeKind },
}

impl Row {
    /// The file this row is in.
    pub fn file(&self) -> u32 {
        match *self {
            Row::File { file, .. }
            | Row::Hunk { file, .. }
            | Row::Line { file, .. }
            | Row::Split { file, .. }
            | Row::Thread { file, .. }
            | Row::Notice { file, .. } => file,
        }
    }

    /// The hunk this row is in, when it's in one.
    pub fn hunk(&self) -> Option<u32> {
        match *self {
            Row::Hunk { hunk, .. } | Row::Line { hunk, .. } | Row::Split { hunk, .. } => Some(hunk),
            _ => None,
        }
    }
}

/// The threads to show, resolved against a changeset.
#[derive(Debug, Clone, Default)]
pub struct Threads {
    /// `(comment index in the doc, placement)`, in document order.
    pub placed: Vec<(usize, Placement)>,
    /// Threads whose replies show.
    pub expanded: HashSet<String>,
    /// file → (hunk, line) → thread indexes.
    by_line: HashMap<u32, HashMap<(u32, u32), Vec<u32>>>,
    /// file → outdated thread indexes.
    outdated: HashMap<u32, Vec<u32>>,
    /// thread index → (replies, thread expanded), for rows.
    shape: Vec<(u16, bool)>,
}

impl Threads {
    /// Resolve `comments` and their places, showing outdated threads only when
    /// `show_outdated` (a commit's own view isn't where they went out of date).
    pub fn new(
        comments: &[Comment],
        places: &HashMap<String, Placement>,
        expanded: HashSet<String>,
        show_outdated: bool,
    ) -> Threads {
        let mut out = Threads { expanded, ..Threads::default() };
        for (index, comment) in comments.iter().enumerate() {
            let Some(place) = places.get(&comment.id) else { continue };
            if place.at.is_none() && !show_outdated {
                continue;
            }
            let at = out.placed.len() as u32;
            out.placed.push((index, *place));
            out.shape.push((comment.replies.len() as u16, out.expanded.contains(&comment.id)));
            match place.at {
                Some(line) => out.by_line.entry(place.file).or_default().entry(line).or_default().push(at),
                None => out.outdated.entry(place.file).or_default().push(at),
            }
        }
        out
    }

    /// The threads after a line.
    fn at(&self, file: u32, hunk: u32, line: u32) -> &[u32] {
        self.by_line
            .get(&file)
            .and_then(|m| m.get(&(hunk, line)))
            .map_or(&[], Vec::as_slice)
    }

    fn push_rows(&self, rows: &mut Vec<Row>, file: u32, thread: u32) {
        let (replies, expanded) = self.shape[thread as usize];
        rows.push(Row::Thread { file, thread, part: ThreadPart::Head });
        if replies == 0 {
            return;
        }
        if expanded {
            for reply in 0..replies {
                rows.push(Row::Thread { file, thread, part: ThreadPart::Reply(reply) });
            }
        } else {
            rows.push(Row::Thread { file, thread, part: ThreadPart::Summary });
        }
    }
}

/// Everything rows are built from.
pub struct RowInput<'a> {
    pub changeset: &'a Changeset,
    pub mode: ViewMode,
    pub filter: HunkFilter,
    pub folds: &'a Folds,
    pub threads: &'a Threads,
}

/// The rows, and where each file's starts.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RowTable {
    pub rows: Vec<Row>,
    /// The row of each file's header, by file index.
    pub file_rows: Vec<u32>,
}

impl RowTable {
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The rows from `top`, at most `height` of them: what a client draws.
    pub fn slice(&self, top: usize, height: usize) -> &[Row] {
        let start = top.min(self.rows.len());
        let end = start.saturating_add(height).min(self.rows.len());
        &self.rows[start..end]
    }
}

/// Build the table.
pub fn build_rows(input: &RowInput<'_>) -> RowTable {
    let lines = input.changeset.line_count();
    let mut table = RowTable {
        rows: Vec::with_capacity(lines + input.changeset.files.len() * 2),
        file_rows: Vec::with_capacity(input.changeset.files.len()),
    };
    for (f, file) in input.changeset.files.iter().enumerate() {
        file_rows(input, f as u32, file, &mut table);
    }
    table
}

fn notice(file: &FileDiff) -> Option<NoticeKind> {
    if file.omitted.is_some() {
        Some(NoticeKind::Omitted)
    } else if file.binary {
        Some(NoticeKind::Binary)
    } else if !file.hunks.is_empty() {
        None
    } else if file.old_path.is_some() {
        Some(NoticeKind::RenameOnly)
    } else if file.mode_change.is_some() {
        Some(NoticeKind::ModeOnly)
    } else {
        Some(NoticeKind::Empty)
    }
}

fn file_rows(input: &RowInput<'_>, f: u32, file: &FileDiff, table: &mut RowTable) {
    let folded = input.folds.file_folded(&file.path);
    table.file_rows.push(table.rows.len() as u32);
    table.rows.push(Row::File { file: f, folded });
    if folded {
        return;
    }
    if let Some(kind) = notice(file) {
        table.rows.push(Row::Notice { file: f, kind });
    }
    if let Some(outdated) = input.threads.outdated.get(&f) {
        for thread in outdated {
            input.threads.push_rows(&mut table.rows, f, *thread);
        }
    }
    for (h, hunk) in file.hunks.iter().enumerate() {
        if !input.filter.shows_hunk(hunk) {
            continue;
        }
        let h = h as u32;
        let folded = input.folds.hunk_folded(&file.path, hunk);
        table.rows.push(Row::Hunk { file: f, hunk: h, folded });
        if folded {
            continue;
        }
        match input.mode {
            ViewMode::Unified => unified_rows(input, f, h, hunk, &mut table.rows),
            ViewMode::Split => split_rows(input, f, h, hunk, &mut table.rows),
        }
    }
}

fn unified_rows(input: &RowInput<'_>, f: u32, h: u32, hunk: &Hunk, rows: &mut Vec<Row>) {
    for (l, line) in hunk.lines.iter().enumerate() {
        if !input.filter.shows(line.kind) {
            continue;
        }
        let l = l as u32;
        rows.push(Row::Line { file: f, hunk: h, line: l });
        for thread in input.threads.at(f, h, l) {
            input.threads.push_rows(rows, f, *thread);
        }
    }
}

/// Old on the left and new on the right, a paired deleted and added line on
/// one row. Unpaired lines keep their order, each beside a blank.
fn split_rows(input: &RowInput<'_>, f: u32, h: u32, hunk: &Hunk, rows: &mut Vec<Row>) {
    let partner_of_old: HashMap<usize, usize> = hunk_pairs(hunk).into_iter().collect();
    let paired_new: HashSet<usize> = partner_of_old.values().copied().collect();
    let shows = |kind| input.filter.shows(kind);
    let emit = |rows: &mut Vec<Row>, old: Option<usize>, new: Option<usize>| {
        let old = old.filter(|o| shows(hunk.lines[*o].kind));
        let new = new.filter(|n| shows(hunk.lines[*n].kind));
        if old.is_none() && new.is_none() {
            return;
        }
        rows.push(Row::Split { file: f, hunk: h, old: old.map(|o| o as u32), new: new.map(|n| n as u32) });
        // A context row's threads anchor to either index, the same line.
        for at in [old, new].into_iter().flatten().collect::<HashSet<_>>() {
            for thread in input.threads.at(f, h, at as u32) {
                input.threads.push_rows(rows, f, *thread);
            }
        }
    };
    let lines = &hunk.lines;
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            emit(rows, Some(i), Some(i));
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Deleted {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        let add_end = i;
        // Walk both runs: unpaired added lines before a pair's added line come
        // out first, so the pair lines up and neither side reorders.
        let mut new_at = add_start;
        for old in del_start..add_start {
            match partner_of_old.get(&old) {
                Some(&partner) => {
                    while new_at < partner {
                        emit(rows, None, Some(new_at));
                        new_at += 1;
                    }
                    emit(rows, Some(old), Some(partner));
                    new_at = partner + 1;
                }
                None => emit(rows, Some(old), None),
            }
        }
        while new_at < add_end {
            if !paired_new.contains(&new_at) {
                emit(rows, None, Some(new_at));
            }
            new_at += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_patch;

    fn changeset(patch: &str) -> Changeset {
        let files = parse_patch(patch);
        Changeset { files, ..Changeset::default() }
    }

    const PATCH: &str = "diff --git a/a.rs b/a.rs\n--- a/a.rs\n+++ b/a.rs\n@@ -1,3 +1,4 @@ one\n ctx\n-let total = count + 1;\n+// a comment line\n+let total = amount + 1;\n end\n@@ -20 +20,2 @@ two\n base\n+added\ndiff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n";

    fn rows(cs: &Changeset, mode: ViewMode, filter: HunkFilter, folds: &Folds) -> RowTable {
        build_rows(&RowInput { changeset: cs, mode, filter, folds, threads: &Threads::default() })
    }

    #[test]
    fn unified_rows_walk_files_and_hunks() {
        let cs = changeset(PATCH);
        let t = rows(&cs, ViewMode::Unified, HunkFilter::All, &Folds::default());
        assert_eq!(t.rows[0], Row::File { file: 0, folded: false });
        assert_eq!(t.rows[1], Row::Hunk { file: 0, hunk: 0, folded: false });
        assert_eq!(t.rows[2], Row::Line { file: 0, hunk: 0, line: 0 });
        // Two hunks, 5 + 2 lines, then the binary file's header and notice.
        assert_eq!(t.len(), 1 + 1 + 5 + 1 + 2 + 2);
        assert_eq!(t.file_rows, [0, 10]);
        assert_eq!(t.rows[11], Row::Notice { file: 1, kind: NoticeKind::Binary });
    }

    #[test]
    fn split_rows_pair_edits_and_keep_order() {
        let cs = changeset(PATCH);
        let t = rows(&cs, ViewMode::Split, HunkFilter::All, &Folds::default());
        let hunk0: Vec<_> = t.rows.iter().filter_map(|r| match r {
            Row::Split { hunk: 0, old, new, .. } => Some((*old, *new)),
            _ => None,
        }).collect();
        assert_eq!(
            hunk0,
            [
                (Some(0), Some(0)),     // context
                (None, Some(2)),        // the new comment, unpaired, first
                (Some(1), Some(3)),     // the edit, side by side
                (Some(4), Some(4)),     // context
            ]
        );
    }

    #[test]
    fn a_large_unequal_block_pairs_in_order_and_loses_nothing() {
        let mut body = String::new();
        for i in 0..40 {
            body.push_str(&format!("-old {i}\n"));
        }
        for i in 0..50 {
            body.push_str(&format!("+new {i}\n"));
        }
        let cs = changeset(&format!("diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1,40 +1,50 @@\n{body}"));
        let t = rows(&cs, ViewMode::Split, HunkFilter::All, &Folds::default());
        let mut olds = 0;
        let mut news = 0;
        for r in &t.rows {
            if let Row::Split { old, new, .. } = r {
                olds += usize::from(old.is_some());
                news += usize::from(new.is_some());
            }
        }
        assert_eq!((olds, news), (40, 50));
    }

    #[test]
    fn the_filter_hides_the_other_side_and_hunks_without_its_kind() {
        let cs = changeset(PATCH);
        let t = rows(&cs, ViewMode::Unified, HunkFilter::Deletions, &Folds::default());
        let kinds: Vec<LineKind> = t.rows.iter().filter_map(|r| match r {
            Row::Line { file, hunk, line } => Some(cs.files[*file as usize].hunks[*hunk as usize].lines[*line as usize].kind),
            _ => None,
        }).collect();
        assert_eq!(kinds, [LineKind::Context, LineKind::Deleted, LineKind::Context]);
        // The second hunk has no deletion, so it's gone, header and all.
        assert!(!t.rows.contains(&Row::Hunk { file: 0, hunk: 1, folded: false }));
        let t = rows(&cs, ViewMode::Split, HunkFilter::Additions, &Folds::default());
        assert!(t.rows.iter().all(|r| !matches!(r, Row::Split { old: Some(1), .. })));
    }

    #[test]
    fn folds_hide_a_hunk_or_a_file_and_stop_at_the_header() {
        let cs = changeset(PATCH);
        let mut folds = Folds::default();
        folds.set_hunk("a.rs", &cs.files[0].hunks[0], true);
        let t = rows(&cs, ViewMode::Unified, HunkFilter::All, &folds);
        assert_eq!(t.rows[1], Row::Hunk { file: 0, hunk: 0, folded: true });
        assert_eq!(t.rows[2], Row::Hunk { file: 0, hunk: 1, folded: false });

        folds.set_file("a.rs", true);
        let t = rows(&cs, ViewMode::Unified, HunkFilter::All, &folds);
        assert_eq!(t.rows[0], Row::File { file: 0, folded: true });
        assert_eq!(t.rows[1], Row::File { file: 1, folded: false });
        assert_eq!(t.file_rows, [0, 1]);

        folds.clear();
        assert_eq!(rows(&cs, ViewMode::Unified, HunkFilter::All, &folds).len(), 12);
    }

    #[test]
    fn folds_survive_by_path_when_indexes_move() {
        let cs = changeset(PATCH);
        let mut folds = Folds::default();
        folds.set_file("i.png", true);
        let moved = changeset("diff --git a/i.png b/i.png\nBinary files a/i.png and b/i.png differ\n");
        let t = rows(&moved, ViewMode::Unified, HunkFilter::All, &folds);
        assert_eq!(t.rows, [Row::File { file: 0, folded: true }]);
        drop(cs);
    }

    #[test]
    fn notices_say_why_a_file_has_no_lines() {
        let cs = changeset("diff --git a/o b/n\nsimilarity index 100%\nrename from o\nrename to n\ndiff --git a/s b/s\nold mode 100644\nnew mode 100755\ndiff --git a/e b/e\nnew file mode 100644\n");
        let t = rows(&cs, ViewMode::Unified, HunkFilter::All, &Folds::default());
        let kinds: Vec<_> = t.rows.iter().filter_map(|r| match r { Row::Notice { kind, .. } => Some(*kind), _ => None }).collect();
        assert_eq!(kinds, [NoticeKind::RenameOnly, NoticeKind::ModeOnly, NoticeKind::Empty]);
    }

    #[test]
    fn threads_follow_their_line_in_both_views() {
        use crate::diff::comments::{place, Anchor, Author, CommentDoc};
        use crate::diff::model::Side;
        let cs = changeset(PATCH);
        let comment = |id: &str, line: u32, code: &str, replies: usize| Comment {
            id: id.into(),
            anchor: Anchor { path: "a.rs".into(), side: Side::New, start: line, end: line, code: vec![code.into()] },
            author: Author::human("j"),
            body: "b".into(),
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            replies: (0..replies).map(|i| crate::diff::comments::CommentReply {
                id: format!("r{i}"),
                author: Author::agent("dev"),
                body: "r".into(),
                created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            }).collect(),
            sent: false,
        };
        let doc = CommentDoc { comments: vec![comment("a", 3, "let total = amount + 1;", 2), comment("b", 9, "was rewritten", 0)] };
        let places = place(&doc, &cs);
        let threads = Threads::new(&doc.comments, &places, HashSet::new(), true);
        let t = build_rows(&RowInput { changeset: &cs, mode: ViewMode::Unified, filter: HunkFilter::All, folds: &Folds::default(), threads: &threads });
        let at = t.rows.iter().position(|r| *r == Row::Line { file: 0, hunk: 0, line: 3 }).unwrap();
        assert_eq!(t.rows[at + 1], Row::Thread { file: 0, thread: 0, part: ThreadPart::Head });
        assert_eq!(t.rows[at + 2], Row::Thread { file: 0, thread: 0, part: ThreadPart::Summary });
        // The outdated thread sits at the top of its file.
        assert_eq!(t.rows[1], Row::Thread { file: 0, thread: 1, part: ThreadPart::Head });

        let expanded: HashSet<String> = ["a".to_string()].into();
        let threads = Threads::new(&doc.comments, &places, expanded, false);
        let t = build_rows(&RowInput { changeset: &cs, mode: ViewMode::Split, filter: HunkFilter::All, folds: &Folds::default(), threads: &threads });
        let at = t.rows.iter().position(|r| matches!(r, Row::Split { new: Some(3), .. })).unwrap();
        assert_eq!(t.rows[at + 1], Row::Thread { file: 0, thread: 0, part: ThreadPart::Head });
        assert_eq!(t.rows[at + 2], Row::Thread { file: 0, thread: 0, part: ThreadPart::Reply(0) });
        assert_eq!(t.rows[at + 3], Row::Thread { file: 0, thread: 0, part: ThreadPart::Reply(1) });
        assert_eq!(t.rows[1], Row::Hunk { file: 0, hunk: 0, folded: false }, "not shown outdated");
    }

    #[test]
    fn slice_clamps_to_the_table() {
        let cs = changeset(PATCH);
        let t = rows(&cs, ViewMode::Unified, HunkFilter::All, &Folds::default());
        assert_eq!(t.slice(0, 3).len(), 3);
        assert_eq!(t.slice(10, 100).len(), 2);
        assert!(t.slice(100, 3).is_empty());
    }
}
