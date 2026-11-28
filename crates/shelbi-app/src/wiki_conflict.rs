// Copied from ContextStore (32pixelsCo/ContextStore@b89f5d8a634bc5ff6b38ecd02723c581583ead98, ContextStoreUI/src/diff.rs), MIT in Shelbi.
// Shelbi's changes: the choices live beside the hunks (so the hub can apply
// a client's choices to its own diff), a common prefix and suffix are
// trimmed before the LCS walk, and the inspector's state, the conflict's
// revision and the notices are Shelbi's own.

//! The wiki's conflict inspector, toolkit-free: a held file
//! ([`WikiConflict`]) broken into **hunks**, runs of lines where our side
//! (this machine's) and theirs (what is committed, from elsewhere) differ,
//! so each is decided on its own. Both clients render [`Inspector`]; the hub
//! applies the choices a client sends ([`Resolution::Hunks`]) to the same
//! [`diff`] of its own copy of the sides ([`resolve`]), refusing them when
//! the sides moved on since the client read them ([`revision`]).
//!
//! The grouping rule is ContextStore's (and its Swift app's): walk the line
//! operations in order, keep unchanged lines as the context before the next
//! hunk, and close a hunk at the first unchanged line after a run of
//! changes. Unchanged lines after the last hunk are the trailing context.

use serde::{Deserialize, Serialize};

use crate::wiki::{Resolution, WikiConflict, WikiStatus};

/// One side of a conflict.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Side {
    /// This machine's version: the wiki's file as it is here.
    Ours,
    /// The version committed elsewhere and pulled in.
    Theirs,
}

impl Side {
    pub fn label(self) -> &'static str {
        match self {
            Side::Ours => "Ours",
            Side::Theirs => "Theirs",
        }
    }

    /// What the side is, in a few words.
    pub fn detail(self) -> &'static str {
        match self {
            Side::Ours => "This machine's version",
            Side::Theirs => "The version from elsewhere",
        }
    }
}

/// One line operation between the two sides.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Op {
    /// On both sides.
    Equal(String),
    /// Only on theirs.
    Insert(String),
    /// Only on ours.
    Delete(String),
}

/// One run of differing lines, with the unchanged lines before it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Unchanged lines just before this hunk (since the previous one).
    pub context_before: Vec<String>,
    /// Our version of the lines (empty when only theirs added some).
    pub ours: Vec<String>,
    /// Their version (empty when only theirs removed some).
    pub theirs: Vec<String>,
}

impl Hunk {
    pub fn side(&self, side: Side) -> &[String] {
        match side {
            Side::Ours => &self.ours,
            Side::Theirs => &self.theirs,
        }
    }
}

/// A diff: the hunks, plus the unchanged lines after the last one.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Diff {
    pub hunks: Vec<Hunk>,
    pub trailing_context: Vec<String>,
}

impl Diff {
    /// Put the file back together, each hunk from the side `choices` (one
    /// per hunk, in order) names. An undecided hunk (`None`, or no entry)
    /// keeps our lines: losing local work to a missed click is the worse
    /// failure.
    pub fn merged(&self, choices: &[Option<Side>]) -> String {
        let mut lines: Vec<&str> = Vec::new();
        for (i, hunk) in self.hunks.iter().enumerate() {
            lines.extend(hunk.context_before.iter().map(String::as_str));
            let side = choices.get(i).copied().flatten().unwrap_or(Side::Ours);
            lines.extend(hunk.side(side).iter().map(String::as_str));
        }
        lines.extend(self.trailing_context.iter().map(String::as_str));
        join_lines(&lines)
    }
}

/// Diff `ours` against `theirs`, line by line.
pub fn diff(ours: &str, theirs: &str) -> Diff {
    group_into_hunks(&operations(&split_lines(ours), &split_lines(theirs)))
}

/// The diff of a held file's two sides (a deleted side is empty).
pub fn diff_of(conflict: &WikiConflict) -> Diff {
    diff(conflict.ours.as_deref().unwrap_or(""), conflict.theirs.as_deref().unwrap_or(""))
}

/// The line operations taking `old` to `new`: the common prefix and suffix
/// as they are, and a longest-common-subsequence walk between them
/// (deletions before insertions at one position, equal runs between).
fn operations(old: &[String], new: &[String]) -> Vec<Op> {
    let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (o, n) = (&old[prefix..old.len() - suffix], &new[prefix..new.len() - suffix]);
    let mut ops: Vec<Op> = old[..prefix].iter().cloned().map(Op::Equal).collect();
    let lcs = lcs_table(o, n);
    let (mut i, mut j) = (0usize, 0usize);
    while i < o.len() && j < n.len() {
        if o[i] == n[j] {
            ops.push(Op::Equal(o[i].clone()));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push(Op::Delete(o[i].clone()));
            i += 1;
        } else {
            ops.push(Op::Insert(n[j].clone()));
            j += 1;
        }
    }
    ops.extend(o[i..].iter().cloned().map(Op::Delete));
    ops.extend(n[j..].iter().cloned().map(Op::Insert));
    ops.extend(old[old.len() - suffix..].iter().cloned().map(Op::Equal));
    ops
}

/// `lcs[i][j]`: the longest common subsequence of `old[i..]` and `new[j..]`.
fn lcs_table(old: &[String], new: &[String]) -> Vec<Vec<usize>> {
    let mut table = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            table[i][j] = if old[i] == new[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    table
}

/// Group operations into hunks, closing one at the first unchanged line
/// after a run of changes.
fn group_into_hunks(ops: &[Op]) -> Diff {
    let mut hunks = Vec::new();
    let mut context: Vec<String> = Vec::new();
    let mut ours: Vec<String> = Vec::new();
    let mut theirs: Vec<String> = Vec::new();
    let mut in_change = false;
    for op in ops {
        match op {
            Op::Equal(line) => {
                if in_change {
                    hunks.push(Hunk {
                        context_before: std::mem::take(&mut context),
                        ours: std::mem::take(&mut ours),
                        theirs: std::mem::take(&mut theirs),
                    });
                    in_change = false;
                }
                context.push(line.clone());
            }
            Op::Delete(line) => {
                in_change = true;
                ours.push(line.clone());
            }
            Op::Insert(line) => {
                in_change = true;
                theirs.push(line.clone());
            }
        }
    }
    if in_change {
        hunks.push(Hunk {
            context_before: context,
            ours,
            theirs,
        });
        return Diff {
            hunks,
            trailing_context: Vec::new(),
        };
    }
    Diff {
        hunks,
        trailing_context: context,
    }
}

/// Lines, empty ones kept, terminators dropped. An empty text has none.
fn split_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let body = text.strip_suffix('\n').unwrap_or(text);
    body.split('\n').map(str::to_string).collect()
}

/// Lines joined again, ending in the newline a Markdown file ends with.
fn join_lines(lines: &[&str]) -> String {
    if lines.is_empty() {
        return String::new();
    }
    let mut out = lines.join("\n");
    out.push('\n');
    out
}

/// A fingerprint of a held file's two sides: the choices of a
/// [`Resolution::Hunks`] only mean something against the diff they were
/// made on, so the hub refuses them when its sides hash differently (a
/// newer pull, or an edit here, since the client read them).
pub fn revision(conflict: &WikiConflict) -> String {
    // FNV-1a, 64-bit: stable across builds and machines (unlike std's
    // hasher), which the far hub needs.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut eat = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    };
    for side in [&conflict.ours, &conflict.theirs] {
        match side {
            Some(text) => {
                eat(b"+");
                eat(text.as_bytes());
            }
            None => eat(b"-"),
        }
        eat(&[0]);
    }
    format!("{hash:016x}")
}

/// The content `resolution` gives a held file (`None` deletes it), or why
/// it can't be applied.
pub fn resolve(conflict: &WikiConflict, resolution: &Resolution) -> Result<Option<String>, String> {
    match resolution {
        Resolution::Ours => Ok(conflict.ours.clone()),
        Resolution::Theirs => Ok(conflict.theirs.clone()),
        Resolution::Content(content) => Ok(content.clone()),
        Resolution::Hunks { choices, revision: rev } => {
            if *rev != revision(conflict) {
                return Err(format!(
                    "`{}` changed since its conflict was read; read it again and choose again",
                    conflict.path
                ));
            }
            let diff = diff_of(conflict);
            if choices.len() != diff.hunks.len() {
                return Err(format!(
                    "`{}` has {} hunks, and {} choices came",
                    conflict.path,
                    diff.hunks.len(),
                    choices.len()
                ));
            }
            // One side throughout is that side whole, a deletion included.
            if let Some(first) = choices.first() {
                if choices.iter().all(|c| c == first) {
                    return Ok(match first {
                        Side::Ours => conflict.ours.clone(),
                        Side::Theirs => conflict.theirs.clone(),
                    });
                }
            }
            let choices: Vec<Option<Side>> = choices.iter().copied().map(Some).collect();
            Ok(Some(diff.merged(&choices)))
        }
    }
}

/// The notice both clients show over the Wiki's footer while files are
/// held: `None` when none are.
pub fn notice(status: &WikiStatus) -> Option<String> {
    match status.conflicts.len() {
        0 => None,
        1 => Some("1 document couldn't be merged".into()),
        n => Some(format!("{n} documents couldn't be merged")),
    }
}

/// The open document's bar while it is held.
pub const DOC_NOTICE: &str = "This document couldn't be merged with a change from elsewhere. It won't sync until it's resolved.";

/// How many context lines show above a hunk (and below the last one).
pub const CONTEXT_LINES: usize = 3;

/// What a client shows of a run of context lines: at most
/// [`CONTEXT_LINES`] (the last of the ones above a hunk, the first of the
/// ones after the last), and how many it leaves out.
pub fn context_shown(lines: &[String], above: bool) -> (&[String], usize) {
    if lines.len() <= CONTEXT_LINES {
        return (lines, 0);
    }
    let hidden = lines.len() - CONTEXT_LINES;
    if above {
        (&lines[hidden..], hidden)
    } else {
        (&lines[..CONTEXT_LINES], hidden)
    }
}

/// The marker for context lines left out: "… 1 line", "… 4 lines".
pub fn hidden_label(n: usize) -> String {
    match n {
        1 => "… 1 line".into(),
        n => format!("… {n} lines"),
    }
}

/// The inspector's state for one held file: its hunks, the side chosen for
/// each, and which one has the keys (the TUI's cursor).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspector {
    conflict: WikiConflict,
    diff: Diff,
    choices: Vec<Option<Side>>,
    cursor: usize,
}

impl Inspector {
    pub fn new(conflict: WikiConflict) -> Self {
        let diff = diff_of(&conflict);
        let choices = vec![None; diff.hunks.len()];
        Self {
            conflict,
            diff,
            choices,
            cursor: 0,
        }
    }

    pub fn path(&self) -> &str {
        &self.conflict.path
    }

    pub fn conflict(&self) -> &WikiConflict {
        &self.conflict
    }

    pub fn diff(&self) -> &Diff {
        &self.diff
    }

    pub fn hunks(&self) -> &[Hunk] {
        &self.diff.hunks
    }

    pub fn choice(&self, hunk: usize) -> Option<Side> {
        self.choices.get(hunk).copied().flatten()
    }

    /// Choose `side` for `hunk`; choosing the side it has undoes it.
    pub fn choose(&mut self, hunk: usize, side: Side) {
        if let Some(c) = self.choices.get_mut(hunk) {
            *c = if *c == Some(side) { None } else { Some(side) };
        }
    }

    /// Use all ours / Use all theirs.
    pub fn choose_all(&mut self, side: Side) {
        self.choices.iter_mut().for_each(|c| *c = Some(side));
    }

    /// Choose `side` for the hunk with the keys, and move to the next one
    /// still undecided (else stay).
    pub fn choose_here(&mut self, side: Side) {
        let Some(c) = self.choices.get_mut(self.cursor) else {
            return;
        };
        *c = Some(side);
        if let Some(next) = (self.cursor + 1..self.choices.len()).find(|&i| self.choices[i].is_none()) {
            self.cursor = next;
        }
    }

    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Move the keys `by` hunks, staying in range.
    pub fn move_cursor(&mut self, by: isize) {
        let last = self.choices.len().saturating_sub(1) as isize;
        self.cursor = (self.cursor as isize + by).clamp(0, last) as usize;
    }

    pub fn chosen(&self) -> usize {
        self.choices.iter().filter(|c| c.is_some()).count()
    }

    /// Every hunk has a side (or there were none to decide).
    pub fn is_complete(&self) -> bool {
        self.choices.iter().all(Option::is_some)
    }

    /// "2 of 3 chosen", for the inspector's header.
    pub fn progress(&self) -> String {
        match self.choices.len() {
            0 => "The two versions are the same".into(),
            n => format!("{} of {n} chosen", self.chosen()),
        }
    }

    /// The sides are the same text (a both-deleted file, a whitespace-only
    /// pull): nothing to choose, only to apply.
    pub fn is_empty(&self) -> bool {
        self.choices.is_empty()
    }

    /// What Apply sends, once every hunk has a side.
    pub fn resolution(&self) -> Option<Resolution> {
        if !self.is_complete() {
            return None;
        }
        if self.choices.is_empty() {
            return Some(Resolution::Ours);
        }
        Some(Resolution::Hunks {
            choices: self.choices.iter().map(|c| c.unwrap_or(Side::Ours)).collect(),
            revision: revision(&self.conflict),
        })
    }

    /// The file the choices make (undecided hunks keep ours).
    pub fn preview(&self) -> String {
        self.diff.merged(&self.choices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<String> {
        split_lines(text)
    }

    fn conflict(ours: Option<&str>, theirs: Option<&str>) -> WikiConflict {
        WikiConflict {
            path: "Plan.md".into(),
            base: Some("# Plan\n".into()),
            ours: ours.map(str::to_string),
            theirs: theirs.map(str::to_string),
            detected_at: "2026-11-27T00:00:00Z".into(),
        }
    }

    #[test]
    fn identical_text_has_no_hunks() {
        let d = diff("a\nb\nc\n", "a\nb\nc\n");
        assert!(d.hunks.is_empty());
        assert_eq!(d.trailing_context, lines("a\nb\nc\n"));
    }

    #[test]
    fn a_single_changed_line_is_one_hunk() {
        let d = diff("a\nOURS\nc\n", "a\nTHEIRS\nc\n");
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].context_before, ["a"]);
        assert_eq!(d.hunks[0].ours, ["OURS"]);
        assert_eq!(d.hunks[0].theirs, ["THEIRS"]);
        assert_eq!(d.trailing_context, ["c"]);
    }

    #[test]
    fn separated_changes_become_separate_hunks() {
        let d = diff("a\nX\nb\nY\nc\n", "a\n1\nb\n2\nc\n");
        assert_eq!(d.hunks.len(), 2);
        assert_eq!(d.hunks[0].ours, ["X"]);
        assert_eq!(d.hunks[1].ours, ["Y"]);
        assert_eq!(d.hunks[1].context_before, ["b"]);
    }

    #[test]
    fn a_pure_addition_or_deletion_has_one_empty_side() {
        let d = diff("a\nc\n", "a\nb\nc\n");
        assert!(d.hunks[0].ours.is_empty());
        assert_eq!(d.hunks[0].theirs, ["b"]);
        let d = diff("a\nb\nc\n", "a\nc\n");
        assert_eq!(d.hunks[0].ours, ["b"]);
        assert!(d.hunks[0].theirs.is_empty());
    }

    #[test]
    fn hunks_resolve_independently_and_undecided_keeps_ours() {
        let d = diff("a\nX\nb\nY\nc\n", "a\n1\nb\n2\nc\n");
        assert_eq!(d.merged(&[Some(Side::Theirs), Some(Side::Ours)]), "a\n1\nb\nY\nc\n");
        assert_eq!(d.merged(&[Some(Side::Theirs), Some(Side::Theirs)]), "a\n1\nb\n2\nc\n");
        assert_eq!(d.merged(&[None, None]), "a\nX\nb\nY\nc\n");
        assert_eq!(d.merged(&[]), "a\nX\nb\nY\nc\n");
    }

    #[test]
    fn an_empty_side_is_one_hunk_of_the_other() {
        let d = diff("", "a\nb\n");
        assert_eq!(d.hunks.len(), 1);
        assert!(d.hunks[0].ours.is_empty());
        assert_eq!(d.hunks[0].theirs, ["a", "b"]);
    }

    #[test]
    fn a_realistic_conflict_round_trips_both_ways() {
        let ours = "# Notes\n\nIntro.\n\n- one\n- two\n\nEnd.\n";
        let theirs = "# Notes\n\nIntro rewritten.\n\n- one\n- two\n- three\n\nEnd.\n";
        let d = diff(ours, theirs);
        assert_eq!(d.hunks.len(), 2, "the intro and the list changed");
        assert_eq!(d.merged(&[Some(Side::Theirs); 2]), theirs);
        assert_eq!(d.merged(&[Some(Side::Ours); 2]), ours);
    }

    #[test]
    fn the_trimmed_walk_matches_the_whole_one_on_long_files() {
        let mut ours = String::new();
        let mut theirs = String::new();
        for i in 0..400 {
            ours.push_str(&format!("line {i}\n"));
            theirs.push_str(&if i == 200 { "changed\n".to_string() } else { format!("line {i}\n") });
        }
        let d = diff(&ours, &theirs);
        assert_eq!(d.hunks.len(), 1);
        assert_eq!(d.hunks[0].ours, ["line 200"]);
        assert_eq!(d.hunks[0].context_before.len(), 200);
        assert_eq!(d.trailing_context.len(), 199);
        assert_eq!(d.merged(&[Some(Side::Theirs)]), theirs);
    }

    #[test]
    fn hunk_choices_apply_against_their_revision_only() {
        let c = conflict(Some("a\nX\nb\nY\nc\n"), Some("a\n1\nb\n2\nc\n"));
        let rev = revision(&c);
        let mixed = Resolution::Hunks {
            choices: vec![Side::Theirs, Side::Ours],
            revision: rev.clone(),
        };
        assert_eq!(resolve(&c, &mixed).unwrap().as_deref(), Some("a\n1\nb\nY\nc\n"));
        // The sides moved on: the choices are refused.
        let mut newer = c.clone();
        newer.theirs = Some("a\n1\nb\n3\nc\n".into());
        assert_ne!(revision(&newer), rev);
        assert!(resolve(&newer, &mixed).is_err());
        // The wrong number of choices is refused.
        let short = Resolution::Hunks {
            choices: vec![Side::Theirs],
            revision: rev,
        };
        assert!(resolve(&c, &short).is_err());
    }

    #[test]
    fn a_whole_side_keeps_its_deletion() {
        let c = conflict(None, Some("# Plan\n\nTheirs.\n"));
        assert_eq!(resolve(&c, &Resolution::Ours).unwrap(), None);
        assert_eq!(resolve(&c, &Resolution::Theirs).unwrap().as_deref(), Some("# Plan\n\nTheirs.\n"));
        let all_ours = Resolution::Hunks {
            choices: vec![Side::Ours],
            revision: revision(&c),
        };
        assert_eq!(resolve(&c, &all_ours).unwrap(), None, "all ours is ours whole: deleted");
    }

    #[test]
    fn the_revision_tells_deleted_from_empty() {
        assert_ne!(revision(&conflict(None, Some("a"))), revision(&conflict(Some(""), Some("a"))));
        assert_ne!(revision(&conflict(Some("ab"), Some(""))), revision(&conflict(Some("a"), Some("b"))));
    }

    #[test]
    fn the_inspector_walks_chooses_and_applies() {
        let mut ins = Inspector::new(conflict(Some("a\nX\nb\nY\nc\n"), Some("a\n1\nb\n2\nc\n")));
        assert_eq!(ins.hunks().len(), 2);
        assert_eq!(ins.progress(), "0 of 2 chosen");
        assert!(ins.resolution().is_none(), "Apply waits for every hunk");
        ins.choose_here(Side::Theirs);
        assert_eq!(ins.cursor(), 1, "on to the next undecided hunk");
        ins.choose_here(Side::Ours);
        assert_eq!(ins.progress(), "2 of 2 chosen");
        assert_eq!(ins.preview(), "a\n1\nb\nY\nc\n");
        let Some(Resolution::Hunks { choices, .. }) = ins.resolution() else {
            panic!("hunks");
        };
        assert_eq!(choices, [Side::Theirs, Side::Ours]);
        // Choosing a hunk's side again undoes it; Use all sets every one.
        ins.choose(0, Side::Theirs);
        assert_eq!(ins.choice(0), None);
        ins.choose_all(Side::Theirs);
        assert!(ins.is_complete());
        assert_eq!(ins.preview(), "a\n1\nb\n2\nc\n");
        ins.move_cursor(-5);
        assert_eq!(ins.cursor(), 0);
        ins.move_cursor(9);
        assert_eq!(ins.cursor(), 1);
    }

    #[test]
    fn same_sides_have_nothing_to_choose() {
        let ins = Inspector::new(conflict(Some("a\n"), Some("a\n")));
        assert!(ins.is_empty() && ins.is_complete());
        assert_eq!(ins.resolution(), Some(Resolution::Ours));
    }

    #[test]
    fn context_shows_the_lines_nearest_the_change() {
        let ctx = lines("1\n2\n3\n4\n5\n");
        assert_eq!(context_shown(&ctx, true), (&ctx[2..], 2));
        assert_eq!(context_shown(&ctx, false), (&ctx[..3], 2));
        assert_eq!(context_shown(&ctx[..2], true), (&ctx[..2], 0));
        assert_eq!(hidden_label(1), "… 1 line");
        assert_eq!(hidden_label(4), "… 4 lines");
    }

    #[test]
    fn the_notice_counts_held_documents() {
        let mut s = WikiStatus::not_set_up();
        assert_eq!(notice(&s), None);
        s.conflicts = vec!["a.md".into()];
        assert_eq!(notice(&s).as_deref(), Some("1 document couldn't be merged"));
        s.conflicts.push("b.md".into());
        assert_eq!(notice(&s).as_deref(), Some("2 documents couldn't be merged"));
    }
}
