//! The editing operations: what typing, Enter, Backspace, the formatting
//! commands and undo do to the Markdown source, with the caret kept in the
//! text the user sees.
//!
//! Every operation is one edit of the source (one undo step), chosen so the
//! row reads back the way the user expects: Enter at the end of a list item
//! starts the next item, Backspace at the start of a heading makes it a
//! paragraph, deleting the last letter of bold text drops its `**`, and so
//! on. Text the edit doesn't touch keeps its bytes, so an unchanged body
//! never changes.

use std::ops::Range;
use std::time::Instant;

use super::blocks::{Doc, Marker, Row, RowKind};
use super::history::{History, Kind, Step};
use super::inline::{Mark, MarkKind, Style};
use crate::issue_edit::Splice;

/// A block a row can be turned into (the block formatting commands).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Paragraph,
    Heading(u8),
    Bullet,
    Ordered,
    Task,
    Quote,
    Code,
}

/// A link the link command edits: an existing one under the caret, or the
/// selection to make one of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkTarget {
    /// The link as written (an existing one), or the selection.
    pub range: Range<usize>,
    pub text: String,
    pub url: String,
    pub existing: bool,
}

/// A Markdown body (or, `plain`, a one-line title) being edited.
#[derive(Debug, Clone)]
pub struct Editor {
    text: String,
    doc: Doc,
    plain: bool,
    anchor: usize,
    head: usize,
    /// The input method's uncommitted text.
    marked: Option<Range<usize>>,
    /// Where a composition started and the text it replaced, for its undo
    /// step.
    composing: Option<(usize, String)>,
    history: History,
    /// Formatting toggled on with nothing selected, for what's typed next.
    pending: Vec<MarkKind>,
    /// Bumped on every change of the text.
    revision: u64,
}

impl Editor {
    /// A Markdown body.
    pub fn markdown(text: &str) -> Self {
        Self::new(text, false)
    }

    /// A one-line plain field (a title).
    pub fn plain(text: &str) -> Self {
        Self::new(text, true)
    }

    fn new(text: &str, plain: bool) -> Self {
        let doc = if plain {
            Doc::plain(text)
        } else {
            Doc::parse(text)
        };
        let mut e = Editor {
            text: text.to_string(),
            doc,
            plain,
            anchor: 0,
            head: 0,
            marked: None,
            composing: None,
            history: History::default(),
            pending: Vec::new(),
            revision: 0,
        };
        let start = e.doc.rows[0].source_of(0);
        e.anchor = start;
        e.head = start;
        e
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn doc(&self) -> &Doc {
        &self.doc
    }

    pub fn is_plain(&self) -> bool {
        self.plain
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn anchor(&self) -> usize {
        self.anchor
    }

    pub fn head(&self) -> usize {
        self.head
    }

    /// The selection, start before end.
    pub fn selection(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }

    pub fn has_selection(&self) -> bool {
        self.anchor != self.head
    }

    pub fn marked(&self) -> Option<Range<usize>> {
        self.marked.clone()
    }

    pub fn is_composing(&self) -> bool {
        self.marked.is_some()
    }

    /// The formatting toggled on for what's typed next.
    pub fn pending(&self) -> &[MarkKind] {
        &self.pending
    }

    /// The style at the caret (the selection's start), pending toggles
    /// included, for a toolbar's pressed states.
    pub fn style_at_caret(&self) -> Style {
        let at = self.selection().start;
        let (r, d) = self.doc.locate(at);
        let row = &self.doc.rows[r];
        // The character before the caret, unless it's at the start.
        let mut s = if self.has_selection() || d == 0 {
            row.style_at(d)
        } else {
            row.style_at(d - 1)
        };
        for k in &self.pending {
            match k {
                MarkKind::Bold => s.bold = !s.bold,
                MarkKind::Italic => s.italic = !s.italic,
                MarkKind::Strike => s.strike = !s.strike,
                MarkKind::Code => s.code = !s.code,
                _ => {}
            }
        }
        s
    }

    /// The row the caret is in.
    pub fn caret_row(&self) -> usize {
        self.doc.row_of(self.head)
    }

    // ---- selection and movement -------------------------------------------

    /// Put the selection at source offsets (a click, a drag, a vertical
    /// move the view worked out). Each end snaps out of hidden syntax.
    pub fn select(&mut self, anchor: usize, head: usize) {
        let len = self.text.len();
        self.anchor = self.doc.snap(anchor.min(len));
        self.head = self.doc.snap(head.min(len));
        self.moved();
    }

    /// Move the caret (or, `extend`, the selection's head) to `src`.
    pub fn move_to(&mut self, src: usize, extend: bool) {
        let src = self.doc.snap(src.min(self.text.len()));
        self.head = src;
        if !extend {
            self.anchor = src;
        }
        self.moved();
    }

    pub fn select_all(&mut self) {
        let first = &self.doc.rows[0];
        let last = self.doc.rows.last().expect("a doc has a row");
        self.anchor = first.source_of(0);
        self.head = last.source_of(last.display.len());
        self.moved();
    }

    fn moved(&mut self) {
        self.pending.clear();
        self.history.seal();
    }

    /// Left by a character (or `word`), across rows.
    pub fn move_left(&mut self, word: bool, extend: bool) {
        if !extend && self.has_selection() {
            let at = self.selection().start;
            self.move_to(at, false);
            return;
        }
        let target = self.left_of(self.head, word);
        self.move_to(target, extend);
    }

    /// Right by a character (or `word`), across rows.
    pub fn move_right(&mut self, word: bool, extend: bool) {
        if !extend && self.has_selection() {
            let at = self.selection().end;
            self.move_to(at, false);
            return;
        }
        let target = self.right_of(self.head, word);
        self.move_to(target, extend);
    }

    /// To the start (or end) of the whole text.
    pub fn move_to_edge(&mut self, end: bool, extend: bool) {
        let target = if end {
            let last = self.doc.rows.last().expect("a doc has a row");
            last.source_of(last.display.len())
        } else {
            self.doc.rows[0].source_of(0)
        };
        self.move_to(target, extend);
    }

    fn left_of(&self, src: usize, word: bool) -> usize {
        let (r, d) = self.doc.locate(src);
        let row = &self.doc.rows[r];
        if d > 0 {
            let nd = if word {
                word_left(&row.display, d)
            } else {
                prev_boundary(&row.display, d)
            };
            row.source_of(nd)
        } else if r > 0 {
            let prev = &self.doc.rows[r - 1];
            prev.source_of(prev.display.len())
        } else {
            src
        }
    }

    fn right_of(&self, src: usize, word: bool) -> usize {
        let (r, d) = self.doc.locate(src);
        let row = &self.doc.rows[r];
        if d < row.display.len() {
            let nd = if word {
                word_right(&row.display, d)
            } else {
                next_boundary(&row.display, d)
            };
            row.source_of(nd)
        } else if r + 1 < self.doc.rows.len() {
            self.doc.rows[r + 1].source_of(0)
        } else {
            src
        }
    }

    // ---- the one way the text changes ---------------------------------------

    /// Replace `range` with `new` as one undo step, then put the selection
    /// at `sel` (in the new text).
    fn apply(&mut self, range: Range<usize>, new: &str, sel: (usize, usize), kind: Kind) {
        let old = self.text[range.clone()].to_string();
        if old == new {
            self.anchor = sel.0;
            self.head = sel.1;
            return;
        }
        let step = Step {
            at: range.start,
            old,
            new: new.to_string(),
            before: (self.anchor, self.head),
        };
        self.text.replace_range(range, new);
        self.history.record(step, kind, Instant::now());
        self.reparse();
        self.anchor = floor_char(&self.text, sel.0.min(self.text.len()));
        self.head = floor_char(&self.text, sel.1.min(self.text.len()));
    }

    /// Several disjoint edits as one step; the selection's ends are carried
    /// through them (`a` after an insertion at it, `b` before).
    fn apply_all(
        &mut self,
        mut edits: Vec<(Range<usize>, String)>,
        sel: (usize, usize),
        kind: Kind,
    ) {
        edits.retain(|(r, s)| !(r.is_empty() && s.is_empty()));
        if edits.is_empty() {
            return;
        }
        edits.sort_by_key(|(r, _)| (r.start, r.end));
        let start = edits[0].0.start;
        let end = edits.iter().map(|(r, _)| r.end).max().unwrap_or(start);
        let mut out = String::new();
        let mut at = start;
        for (r, s) in &edits {
            out.push_str(&self.text[at..r.start.max(at)]);
            out.push_str(s);
            at = at.max(r.end);
        }
        out.push_str(&self.text[at..end]);
        let map = |p: usize, after: bool| map_through(&edits, p, after);
        let sel = (map(sel.0, true), map(sel.1, false));
        let sel = if sel.0 > sel.1 { (sel.1, sel.1) } else { sel };
        self.apply(start..end, &out, sel, kind);
    }

    fn reparse(&mut self) {
        self.doc = if self.plain {
            Doc::plain(&self.text)
        } else {
            Doc::parse(&self.text)
        };
        self.revision += 1;
    }

    // ---- typing ---------------------------------------------------------------

    /// Type or paste `text` over the selection. Pasted Markdown stays
    /// Markdown (so it comes in formatted); a title takes it as one line.
    pub fn insert(&mut self, text: &str) {
        if self.marked.is_some() {
            self.commit(None, text);
            return;
        }
        let text = self.clean(text);
        if text.is_empty() && !self.has_selection() {
            return;
        }
        let typing = !self.has_selection() && !text.contains('\n') && text.chars().count() <= 2;
        let (range, mut new, mut caret) = self.deletion(self.selection());
        let (open, close) = self.pending_markers();
        let rel = caret - range.start;
        let insert = format!("{open}{text}{close}");
        new.insert_str(rel, &insert);
        caret += open.len() + text.len();
        self.pending.clear();
        let kind = if typing && open.is_empty() {
            Kind::Typing
        } else {
            Kind::Other
        };
        self.apply(range, &new, (caret, caret), kind);
    }

    fn clean(&self, text: &str) -> String {
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if self.plain {
            text.replace('\n', " ")
        } else {
            text
        }
    }

    /// The markers the pending toggles wrap typing in.
    fn pending_markers(&self) -> (String, String) {
        let mut open = String::new();
        let mut close = String::new();
        for k in &self.pending {
            let m = marker(*k);
            open.push_str(m);
            close.insert_str(0, m);
        }
        (open, close)
    }

    /// A hard line break inside the row (Shift+Enter).
    pub fn line_break(&mut self) {
        if self.plain {
            return;
        }
        let (r, _) = self.doc.locate(self.head);
        let code = matches!(
            self.doc.rows[r].kind,
            RowKind::Code | RowKind::Fence { .. } | RowKind::Raw
        );
        self.insert(if code { "\n" } else { "\\\n" });
    }

    // ---- deleting ---------------------------------------------------------------

    /// Backspace: the selection, else what's before the caret (a
    /// character, or `word`). At the start of a row it undoes the row's
    /// formatting first (a list item, a heading), then joins it to the row
    /// above.
    pub fn backspace(&mut self, word: bool) {
        if self.marked.is_some() {
            return;
        }
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let (r, d) = self.doc.locate(self.head);
        let row = self.doc.rows[r].clone();
        if d > 0 {
            let from = row.source_of(if word {
                word_left(&row.display, d)
            } else {
                prev_boundary(&row.display, d)
            });
            let (range, new, caret) = self.deletion(from..self.head);
            self.apply(range, &new, (caret, caret), Kind::Deleting);
            return;
        }
        match &row.kind {
            RowKind::Item(_) if row.level > 0 => {
                self.outdent();
            }
            RowKind::Item(_) | RowKind::Heading(_) | RowKind::Quote => self.unformat(r),
            RowKind::Empty if r > 0 => {
                let prev = &self.doc.rows[r - 1];
                let caret = prev.source_of(prev.display.len());
                self.apply(
                    prev.lines.end..row.lines.start,
                    "",
                    (caret, caret),
                    Kind::Deleting,
                );
            }
            _ if r > 0 => {
                let prev = self.doc.rows[r - 1].clone();
                if prev.kind == RowKind::Empty {
                    // Drop the empty row above, keeping one blank line.
                    let caret = self.head - (row.lines.start - prev.lines.start);
                    self.apply(
                        prev.lines.start..row.lines.start,
                        "",
                        (caret, caret),
                        Kind::Deleting,
                    );
                } else {
                    let caret = prev.lines.end;
                    self.apply(
                        prev.lines.end..row.content_start,
                        "",
                        (caret, caret),
                        Kind::Deleting,
                    );
                    let snapped = self.doc.snap(caret);
                    self.anchor = snapped;
                    self.head = snapped;
                }
            }
            _ => {}
        }
    }

    /// Forward delete: the selection, else what's after the caret; at the
    /// end of a row the row below joins it.
    pub fn delete(&mut self, word: bool) {
        if self.marked.is_some() {
            return;
        }
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        let (r, d) = self.doc.locate(self.head);
        let row = self.doc.rows[r].clone();
        if d < row.display.len() {
            let to = row.source_of(if word {
                word_right(&row.display, d)
            } else {
                next_boundary(&row.display, d)
            });
            let (range, new, caret) = self.deletion(self.head..to);
            self.apply(range, &new, (caret, caret), Kind::Deleting);
            return;
        }
        let Some(next) = self.doc.rows.get(r + 1).cloned() else {
            return;
        };
        let caret = self.head;
        if next.kind == RowKind::Empty {
            self.apply(
                row.lines.end..next.lines.start,
                "",
                (caret, caret),
                Kind::Deleting,
            );
        } else if row.kind == RowKind::Empty {
            // An empty row takes the row below's place.
            let caret = row.lines.start;
            self.apply(
                row.lines.start..next.lines.start,
                "",
                (caret, caret),
                Kind::Deleting,
            );
            let snapped = self.doc.snap(caret);
            self.anchor = snapped;
            self.head = snapped;
        } else {
            let caret = caret.min(row.lines.end);
            self.apply(
                row.lines.end..next.content_start,
                "",
                (caret, caret),
                Kind::Deleting,
            );
        }
    }

    /// Delete the selection.
    pub fn delete_selection(&mut self) {
        if !self.has_selection() {
            return;
        }
        let (range, new, caret) = self.deletion(self.selection());
        self.apply(range, &new, (caret, caret), Kind::Other);
    }

    /// Delete from the caret back to `src` (Cmd+Backspace to the start of
    /// the visual line, which the view knows).
    pub fn delete_back_to(&mut self, src: usize) {
        if self.has_selection() {
            self.delete_selection();
            return;
        }
        if src >= self.head {
            self.backspace(false);
            return;
        }
        let (range, new, caret) = self.deletion(src..self.head);
        self.apply(range, &new, (caret, caret), Kind::Deleting);
    }

    /// What deleting the text shown in `sel` does to the source: the range
    /// replaced, its replacement, and where the caret ends up. Only the
    /// visible text goes, plus the syntax between rows it spans (so they
    /// join) and the delimiters of any mark it empties; the delimiters of
    /// marks it only cuts into stay balanced.
    fn deletion(&self, sel: Range<usize>) -> (Range<usize>, String, usize) {
        if sel.is_empty() {
            return (sel.start..sel.start, String::new(), sel.start);
        }
        if self.plain {
            return (sel.clone(), String::new(), sel.start);
        }
        let (a, b) = (sel.start, sel.end);
        let ra = self.doc.row_of(a);
        let rb = self.doc.row_of(b);
        let mut gone: Vec<Range<usize>> = Vec::new();
        for r in ra..=rb {
            let row = &self.doc.rows[r];
            for v in row.visible() {
                let (s, e) = (v.start.max(a), v.end.min(b));
                if s < e {
                    gone.push(s..e);
                }
            }
            if r < rb {
                gone.push(row.lines.end..self.doc.rows[r + 1].content_start);
            }
        }
        // Marks whose visible text all goes take their delimiters along.
        for r in ra..=rb {
            let row = &self.doc.rows[r];
            for m in &row.marks {
                let c = m.content();
                let shown: Vec<Range<usize>> = row
                    .visible()
                    .filter_map(|v| {
                        let (s, e) = (v.start.max(c.start), v.end.min(c.end));
                        (s < e).then_some(s..e)
                    })
                    .collect();
                if !shown.is_empty() && shown.iter().all(|v| covered(&gone, v)) {
                    gone.push(m.open.clone());
                    gone.push(m.close.clone());
                }
            }
        }
        gone.retain(|r| !r.is_empty());
        gone.sort_by_key(|r| r.start);
        let mut merged: Vec<Range<usize>> = Vec::new();
        for r in gone {
            match merged.last_mut() {
                Some(last) if r.start <= last.end => last.end = last.end.max(r.end),
                _ => merged.push(r),
            }
        }
        let start = merged.first().map_or(a, |r| r.start).min(a);
        let end = merged.last().map_or(b, |r| r.end).max(a);
        let mut new = String::new();
        let mut at = start;
        for r in &merged {
            new.push_str(&self.text[at..r.start]);
            at = r.end;
        }
        new.push_str(&self.text[at..end]);
        // The caret lands where `a` was, less what went before it.
        let gone_before: usize = merged
            .iter()
            .map(|r| r.end.min(a).saturating_sub(r.start))
            .sum();
        let caret = a - gone_before;
        (start..end, new, caret)
    }

    // ---- Enter ----------------------------------------------------------------

    /// Enter: a new block, as the row calls for (the next list item, a new
    /// paragraph, a new line of code), or out of an empty list item. A
    /// title has no Enter of its own (returns false; the view moves on).
    pub fn enter(&mut self) -> bool {
        if self.plain {
            return false;
        }
        if self.marked.is_some() {
            return true;
        }
        if self.has_selection() {
            self.delete_selection();
        }
        let (r, d) = self.doc.locate(self.head);
        let row = self.doc.rows[r].clone();
        let at_end = d == row.display.len();
        match &row.kind {
            RowKind::Code | RowKind::Raw => {
                let line_start = self.text[..self.head].rfind('\n').map_or(0, |i| i + 1);
                let indent: String = self.text[line_start..]
                    .chars()
                    .take_while(|c| *c == ' ' || *c == '\t')
                    .collect();
                let indent = if row.kind == RowKind::Raw {
                    String::new()
                } else {
                    indent
                };
                let at = self.head;
                let new = format!("\n{indent}");
                self.apply(at..at, &new, (at + new.len(), at + new.len()), Kind::Other);
            }
            RowKind::Fence { open: true } => {
                let closed =
                    self.doc.rows.iter().any(|o| {
                        o.block == row.block && o.kind == (RowKind::Fence { open: false })
                    });
                let at = row.lines.end;
                let new = if closed {
                    "\n".to_string()
                } else {
                    let line = &self.text[row.lines.clone()];
                    let fence: String = line
                        .trim_start()
                        .chars()
                        .take_while(|c| *c == '`' || *c == '~')
                        .collect();
                    format!("\n\n{fence}")
                };
                self.apply(at..at, &new, (at + 1, at + 1), Kind::Other);
            }
            RowKind::Fence { open: false } | RowKind::Empty | RowKind::Rule => self.open_after(r),
            RowKind::Item(item) => {
                if row.display.is_empty() {
                    if row.level > 0 {
                        self.outdent();
                    } else {
                        self.leave_list(r);
                    }
                    return true;
                }
                let indent = self.text[row.lines.start..row.lines.start + item.indent].to_string();
                let next = match &item.marker {
                    Marker::Bullet(c) => format!("{c} "),
                    Marker::Ordered { number, delim } => format!("{}{delim} ", number + 1),
                };
                let task = if item.task.is_some() { "[ ] " } else { "" };
                if at_end {
                    let at = row.lines.end;
                    let new = format!("\n{indent}{next}{task}");
                    self.apply(at..at, &new, (at + new.len(), at + new.len()), Kind::Other);
                } else {
                    let (closers, openers) = self.open_marks_at(&row, self.head);
                    let at = self.head;
                    let new = format!("{closers}\n{indent}{next}{task}{openers}");
                    self.apply(at..at, &new, (at + new.len(), at + new.len()), Kind::Other);
                }
            }
            RowKind::Paragraph | RowKind::Heading(_) | RowKind::Quote => {
                if at_end {
                    self.open_after(r);
                } else if d == 0 {
                    self.open_before(r);
                } else {
                    let (closers, openers) = self.open_marks_at(&row, self.head);
                    let quote = if row.kind == RowKind::Quote { "> " } else { "" };
                    // The spaces around the split go: a paragraph doesn't
                    // start with one, and a delimiter can't sit beside one.
                    let line = &self.text[row.lines.clone()];
                    let rel = self.head - row.lines.start;
                    let before =
                        line[..rel].len() - line[..rel].trim_end_matches([' ', '\t']).len();
                    let after =
                        line[rel..].len() - line[rel..].trim_start_matches([' ', '\t']).len();
                    let range = self.head - before..self.head + after;
                    let new = format!("{closers}\n\n{quote}{openers}");
                    let caret = range.start + new.len();
                    self.apply(range, &new, (caret, caret), Kind::Other);
                }
            }
        }
        true
    }

    /// The closing and reopening delimiters of the marks open at `at`, so a
    /// row split there keeps both halves formatted.
    fn open_marks_at(&self, row: &Row, at: usize) -> (String, String) {
        let mut open: Vec<&Mark> = row
            .marks
            .iter()
            .filter(|m| {
                matches!(
                    m.kind,
                    MarkKind::Bold | MarkKind::Italic | MarkKind::Strike | MarkKind::Code
                ) && m.content().start < at
                    && at < m.content().end
            })
            .collect();
        open.sort_by_key(|m| m.open.start);
        let openers: String = open.iter().map(|m| &self.text[m.open.clone()]).collect();
        let closers: String = open
            .iter()
            .rev()
            .map(|m| &self.text[m.close.clone()])
            .collect();
        (closers, openers)
    }

    /// Whether the line starting at `start` is blank (one past the end, the
    /// text's edge, counts as blank).
    fn line_blank_at(&self, start: usize) -> bool {
        if start >= self.text.len() {
            return true;
        }
        let end = self.text[start..]
            .find('\n')
            .map_or(self.text.len(), |i| start + i);
        self.text[start..end].trim().is_empty()
    }

    /// Whether the line ending at `end` (exclusive, before its `\n`) is
    /// blank; none at all (the text's start) counts as blank.
    fn line_blank_before(&self, end: usize) -> bool {
        let start = self.text[..end].rfind('\n').map_or(0, |i| i + 1);
        self.text[start..end].trim().is_empty()
    }

    /// Open an empty paragraph under row `r`, blank lines around it, and
    /// put the caret in it.
    fn open_after(&mut self, r: usize) {
        let p = self.doc.rows[r].lines.end;
        let suffix = if p >= self.text.len() || !self.line_blank_at(p + 1) {
            "\n"
        } else {
            ""
        };
        let new = format!("\n\n{suffix}");
        self.apply(p..p, &new, (p + 2, p + 2), Kind::Other);
    }

    /// Open an empty paragraph above row `r`; the caret stays in the row.
    fn open_before(&mut self, r: usize) {
        let q = self.doc.rows[r].lines.start;
        let prefix = if q > 0 && !self.line_blank_before(q - 1) {
            "\n"
        } else {
            ""
        };
        let new = format!("{prefix}\n\n");
        let caret = self.head + new.len();
        self.apply(q..q, &new, (caret, caret), Kind::Other);
    }

    /// Turn the empty list item `r` into an empty paragraph after the list.
    fn leave_list(&mut self, r: usize) {
        let row = self.doc.rows[r].clone();
        let prefix = if row.lines.start > 0 && !self.line_blank_before(row.lines.start - 1) {
            "\n"
        } else {
            ""
        };
        let end = row.lines.end;
        let suffix = if end >= self.text.len() || !self.line_blank_at(end + 1) {
            "\n"
        } else {
            ""
        };
        let new = format!("{prefix}{suffix}");
        let caret = row.lines.start + prefix.len();
        self.apply(row.lines.clone(), &new, (caret, caret), Kind::Other);
    }

    /// Drop row `r`'s block formatting (list marker, heading, quote) so it
    /// reads as a paragraph, kept apart from its neighbours.
    fn unformat(&mut self, r: usize) {
        let row = self.doc.rows[r].clone();
        let prefix = if row.lines.start > 0 && !self.line_blank_before(row.lines.start - 1) {
            "\n"
        } else {
            ""
        };
        let end = row.lines.end;
        let suffix = if end < self.text.len() && !self.line_blank_at(end + 1) {
            "\n"
        } else {
            ""
        };
        let new = format!("{prefix}{}{suffix}", &self.text[row.content_start..end]);
        let caret = row.lines.start + prefix.len() + self.head.saturating_sub(row.content_start);
        self.apply(row.lines.clone(), &new, (caret, caret), Kind::Other);
    }

    // ---- inline formatting --------------------------------------------------------

    /// Bold, italic, strikethrough or code over the selection: on where any
    /// of it isn't, off where all of it is. With nothing selected it applies
    /// to what's typed next (or, at the end of formatted text, steps out of
    /// it).
    pub fn toggle_mark(&mut self, kind: MarkKind) {
        if self.plain
            || !matches!(
                kind,
                MarkKind::Bold | MarkKind::Italic | MarkKind::Strike | MarkKind::Code
            )
        {
            return;
        }
        self.history.seal();
        if !self.has_selection() {
            let (r, _) = self.doc.locate(self.head);
            let head = self.head;
            let inside = self.doc.rows[r]
                .marks
                .iter()
                .find(|m| m.kind == kind && m.content().start <= head && head <= m.content().end)
                .cloned();
            match inside {
                Some(m) if head == m.content().end => {
                    // Step out: the caret goes past the closing delimiter.
                    self.anchor = m.close.end;
                    self.head = m.close.end;
                }
                Some(_) => {}
                None => {
                    if let Some(i) = self.pending.iter().position(|k| *k == kind) {
                        self.pending.remove(i);
                    } else {
                        self.pending.push(kind);
                    }
                }
            }
            return;
        }
        let sel = self.selection();
        let m = marker(kind);
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let (ra, rb) = (self.doc.row_of(sel.start), self.doc.row_of(sel.end));
        for r in ra..=rb {
            let row = &self.doc.rows[r];
            if !row.is_text() {
                continue;
            }
            let Some((va, vb)) = trimmed_visible(row, sel.clone()) else {
                continue;
            };
            let existing: Vec<&Mark> = row
                .marks
                .iter()
                .filter(|mk| mk.kind == kind && mk.content().start < vb && va < mk.content().end)
                .collect();
            let all = row
                .visible()
                .filter_map(|v| {
                    let (s, e) = (v.start.max(va), v.end.min(vb));
                    (s < e).then_some(s..e)
                })
                .all(|v| {
                    existing
                        .iter()
                        .any(|mk| mk.content().start <= v.start && v.end <= mk.content().end)
                });
            if all && !existing.is_empty() {
                for mk in existing {
                    let (cs, ce) = (mk.content().start, mk.content().end);
                    let open = &self.text[mk.open.clone()];
                    let close = &self.text[mk.close.clone()];
                    let mut new = String::new();
                    if cs < va {
                        new.push_str(&format!("{open}{}{close}", &self.text[cs..va]));
                    }
                    new.push_str(&self.text[cs.max(va)..ce.min(vb)]);
                    if vb < ce {
                        new.push_str(&format!("{open}{}{close}", &self.text[vb..ce]));
                    }
                    edits.push((mk.whole(), new));
                }
            } else {
                let start = existing
                    .iter()
                    .map(|mk| mk.content().start)
                    .fold(va, usize::min);
                let end = existing
                    .iter()
                    .map(|mk| mk.content().end)
                    .fold(vb, usize::max);
                let marker_text = if kind == MarkKind::Code && self.text[start..end].contains('`') {
                    "``"
                } else {
                    m
                };
                edits.push((start..start, marker_text.to_string()));
                for mk in &existing {
                    edits.push((mk.open.clone(), String::new()));
                    edits.push((mk.close.clone(), String::new()));
                }
                edits.push((end..end, marker_text.to_string()));
            }
        }
        let anchor_first = self.anchor <= self.head;
        let (a, b) = (sel.start, sel.end);
        self.apply_all(edits, (a, b), Kind::Other);
        let sel = self.selection();
        let (a, b) = (self.doc.snap(sel.start), self.doc.snap(sel.end));
        if anchor_first {
            self.anchor = a;
            self.head = b;
        } else {
            self.anchor = b;
            self.head = a;
        }
    }

    // ---- block formatting ------------------------------------------------------------

    /// Turn the rows the selection touches into `kind`; rows already all
    /// `kind` go back to paragraphs.
    pub fn set_block(&mut self, kind: BlockKind) {
        if self.plain {
            return;
        }
        self.history.seal();
        let sel = self.selection();
        let (ra, rb) = (self.doc.row_of(sel.start), self.doc.row_of(sel.end));
        if kind == BlockKind::Code {
            self.toggle_code_block(ra, rb);
            return;
        }
        let rows: Vec<Row> = self.doc.rows[ra..=rb]
            .iter()
            .filter(|r| r.is_text() || r.kind == RowKind::Empty)
            .cloned()
            .collect();
        if rows.is_empty() {
            return;
        }
        let already = rows.iter().all(|r| block_kind(r) == Some(kind));
        let kind = if already { BlockKind::Paragraph } else { kind };
        let mut edits: Vec<(Range<usize>, String)> = Vec::new();
        let mut number = 1u64;
        for row in &rows {
            let indent = match &row.kind {
                RowKind::Item(item) => {
                    self.text[row.lines.start..row.lines.start + item.indent].to_string()
                }
                _ => String::new(),
            };
            let checked = row.task() == Some(true);
            let prefix = match kind {
                BlockKind::Paragraph => String::new(),
                BlockKind::Heading(n) => format!("{} ", "#".repeat(n.clamp(1, 6) as usize)),
                BlockKind::Bullet => format!("{indent}- "),
                BlockKind::Ordered => {
                    let p = format!("{indent}{number}. ");
                    number += 1;
                    p
                }
                BlockKind::Task => format!("{indent}- [{}] ", if checked { "x" } else { " " }),
                BlockKind::Quote => "> ".to_string(),
                BlockKind::Code => unreachable!(),
            };
            edits.push((row.lines.start..row.content_start, prefix));
        }
        // A paragraph keeps clear of a list or quote above and of text
        // below (each would otherwise run into it).
        let first = &rows[0];
        let last = rows.last().expect("rows");
        if kind == BlockKind::Paragraph || kind == BlockKind::Quote {
            if first.lines.start > 0 && !self.line_blank_before(first.lines.start - 1) {
                edits.push((first.lines.start..first.lines.start, "\n".into()));
            }
            if last.lines.end < self.text.len() && !self.line_blank_at(last.lines.end + 1) {
                edits.push((last.lines.end..last.lines.end, "\n".into()));
            }
        }
        // Merge the inserted blank line with the first prefix edit.
        edits.sort_by_key(|(r, s)| (r.start, !s.starts_with('\n')));
        let mut merged: Vec<(Range<usize>, String)> = Vec::new();
        for (r, s) in edits {
            match merged.last_mut() {
                Some((lr, ls)) if lr.end == r.start && lr.start == lr.end => {
                    ls.push_str(&s);
                    *lr = lr.start..r.end;
                }
                _ => merged.push((r, s)),
            }
        }
        let (a, b) = (self.anchor, self.head);
        self.apply_all(merged, (a.min(b), a.max(b)), Kind::Other);
        let sel = self.selection();
        self.anchor = self.doc.snap(sel.start);
        self.head = self.doc.snap(sel.end);
    }

    /// Wrap rows `ra..=rb` in a code fence, or, in a fenced block, unwrap it.
    fn toggle_code_block(&mut self, ra: usize, rb: usize) {
        let row = &self.doc.rows[ra];
        if let Some(block) = row.block.filter(|_| row.is_code()) {
            let open = self
                .doc
                .rows
                .iter()
                .find(|r| r.block == Some(block) && r.kind == RowKind::Fence { open: true })
                .cloned();
            let close = self
                .doc
                .rows
                .iter()
                .find(|r| r.block == Some(block) && r.kind == RowKind::Fence { open: false })
                .cloned();
            let mut edits = Vec::new();
            if let Some(o) = open {
                edits.push((
                    o.lines.start..(o.lines.end + 1).min(self.text.len()),
                    String::new(),
                ));
            }
            if let Some(c) = close {
                edits.push((c.lines.start.saturating_sub(1)..c.lines.end, String::new()));
            }
            let (a, b) = (self.anchor, self.head);
            self.apply_all(edits, (a.min(b), a.max(b)), Kind::Other);
            let at = self.doc.snap(self.selection().start);
            self.anchor = at;
            self.head = at;
            return;
        }
        let first = self.doc.rows[ra].clone();
        let last = self.doc.rows[rb].clone();
        let pre = if first.lines.start > 0 && !self.line_blank_before(first.lines.start - 1) {
            "\n"
        } else {
            ""
        };
        let post = if last.lines.end < self.text.len() && !self.line_blank_at(last.lines.end + 1) {
            "\n"
        } else {
            ""
        };
        let edits = vec![
            (
                first.lines.start..first.content_start,
                format!("{pre}```\n"),
            ),
            (last.lines.end..last.lines.end, format!("\n```{post}")),
        ];
        let (a, b) = (self.anchor, self.head);
        self.apply_all(edits, (a.min(b), a.max(b)), Kind::Other);
        let sel = self.selection();
        self.anchor = self.doc.snap(sel.start);
        self.head = self.doc.snap(sel.end);
    }

    // ---- lists -------------------------------------------------------------------------

    /// Check or uncheck the task-list item in row `r`. False when it isn't
    /// one.
    pub fn toggle_task(&mut self, r: usize) -> bool {
        let Some(RowKind::Item(item)) = self.doc.rows.get(r).map(|row| row.kind.clone()) else {
            return false;
        };
        let Some(at) = item.toggle else {
            return false;
        };
        self.history.seal();
        let new = if item.task == Some(true) { " " } else { "x" };
        let sel = (self.anchor, self.head);
        self.apply(at..at + 1, new, sel, Kind::Other);
        self.history.seal();
        true
    }

    /// Toggle the task item(s) the caret is in (Cmd+Enter).
    pub fn toggle_task_at_caret(&mut self) -> bool {
        let r = self.doc.row_of(self.head);
        self.toggle_task(r)
    }

    /// Nest the list item at the caret under the item above (Tab). False
    /// when the caret isn't in a list item.
    pub fn indent(&mut self) -> bool {
        let r = self.doc.row_of(self.head);
        let row = self.doc.rows[r].clone();
        let RowKind::Item(item) = &row.kind else {
            return false;
        };
        let sibling = self.doc.rows[..r]
            .iter()
            .rev()
            .take_while(|o| matches!(o.kind, RowKind::Item(_)) || o.level > row.level)
            .find(|o| matches!(o.kind, RowKind::Item(_)) && o.level == row.level)
            .cloned();
        let Some(sibling) = sibling else {
            return true;
        };
        let RowKind::Item(sib) = &sibling.kind else {
            return true;
        };
        let target = sib.marker_end - sibling.lines.start;
        let target = target.max(sib.indent + 2);
        if target <= item.indent {
            return true;
        }
        self.shift_items(r, target as isize - item.indent as isize);
        true
    }

    /// Move the list item at the caret out a level (Shift+Tab). False at
    /// the outer level or outside a list.
    pub fn outdent(&mut self) -> bool {
        let r = self.doc.row_of(self.head);
        let row = self.doc.rows[r].clone();
        let RowKind::Item(item) = &row.kind else {
            return false;
        };
        if row.level == 0 {
            return false;
        }
        let parent = self.doc.rows[..r]
            .iter()
            .rev()
            .find(|o| matches!(o.kind, RowKind::Item(_)) && o.level < row.level)
            .cloned();
        let target = match parent.as_ref().map(|p| &p.kind) {
            Some(RowKind::Item(p)) => p.indent,
            _ => 0,
        };
        self.shift_items(r, target as isize - item.indent as isize);
        true
    }

    /// Re-indent item `r` and the rows nested under it by `delta` columns.
    fn shift_items(&mut self, r: usize, delta: isize) {
        self.history.seal();
        let level = self.doc.rows[r].level;
        let mut edits = Vec::new();
        for (i, row) in self.doc.rows.iter().enumerate().skip(r) {
            if i > r
                && (row.level <= level
                    || !matches!(row.kind, RowKind::Item(_) | RowKind::Paragraph))
            {
                break;
            }
            // Every line of the row (continuations too).
            let mut at = row.lines.start;
            while at <= row.lines.end {
                let end = self.text[at..]
                    .find('\n')
                    .map_or(self.text.len(), |n| at + n)
                    .min(row.lines.end);
                let ws =
                    self.text[at..end].len() - self.text[at..end].trim_start_matches(' ').len();
                if delta > 0 {
                    edits.push((at..at, " ".repeat(delta as usize)));
                } else {
                    edits.push((at..at + ws.min(delta.unsigned_abs()), String::new()));
                }
                at = end + 1;
            }
        }
        let (a, b) = (self.anchor, self.head);
        let sel = (a.min(b), a.max(b));
        self.apply_all(edits, sel, Kind::Other);
        let sel = self.selection();
        self.anchor = self.doc.snap(sel.start);
        self.head = self.doc.snap(sel.end);
    }

    // ---- links ------------------------------------------------------------------------------

    /// The link the link command edits: the one the caret is in, else the
    /// selection (within one row) to make one of.
    pub fn link_target(&self) -> Option<LinkTarget> {
        if self.plain {
            return None;
        }
        let sel = self.selection();
        let r = self.doc.row_of(sel.start);
        let row = &self.doc.rows[r];
        let existing = row.marks.iter().find(|m| {
            m.kind == MarkKind::Link
                && m.open.len() == 1
                && self.text[m.open.clone()] == *"["
                && m.whole().start <= sel.start
                && sel.end <= m.whole().end
        });
        if let Some(m) = existing {
            return Some(LinkTarget {
                range: m.whole(),
                text: self.text[m.content()].to_string(),
                url: m.url.clone(),
                existing: true,
            });
        }
        if sel.is_empty() || self.doc.row_of(sel.end) != r || !row.is_text() {
            return None;
        }
        Some(LinkTarget {
            range: sel.clone(),
            text: self.text[sel].to_string(),
            url: String::new(),
            existing: false,
        })
    }

    /// Make the link target a link to `url` (an existing link's destination
    /// changes; an empty `url` removes the link, keeping its text).
    pub fn set_link(&mut self, url: &str) -> bool {
        let Some(t) = self.link_target() else {
            return false;
        };
        self.history.seal();
        let url = url.trim();
        let dest = if url.contains([' ', '(', ')']) {
            format!("<{url}>")
        } else {
            url.to_string()
        };
        if t.existing {
            if url.is_empty() {
                let caret = t.range.start + t.text.len();
                self.apply(t.range.clone(), &t.text, (caret, caret), Kind::Other);
            } else {
                let new = format!("[{}]({dest})", t.text);
                let caret = t.range.start + 1 + t.text.len();
                self.apply(t.range.clone(), &new, (caret, caret), Kind::Other);
            }
        } else if !url.is_empty() {
            let new = format!("[{}]({dest})", t.text);
            let caret = t.range.start + 1 + t.text.len();
            self.apply(
                t.range.clone(),
                &new,
                (t.range.start + 1, caret),
                Kind::Other,
            );
        }
        self.history.seal();
        true
    }

    /// The URL of the link at source offset `src` (a written link, an
    /// autolink or a bare URL), to open.
    pub fn link_at(&self, src: usize) -> Option<String> {
        let r = self.doc.row_of(src);
        let row = &self.doc.rows[r];
        if let Some(m) = row
            .marks
            .iter()
            .find(|m| m.kind == MarkKind::Link && m.content().start <= src && src < m.content().end)
        {
            return Some(m.url.clone());
        }
        let d = row.display_of(src);
        row.styles
            .iter()
            .find(|(range, s)| s.link && range.start <= d && d < range.end)
            .map(|(range, _)| row.display[range.clone()].to_string())
            .filter(|u| u.starts_with("http") || u.starts_with("www."))
            .map(|u| {
                if u.starts_with("www.") {
                    format!("https://{u}")
                } else {
                    u
                }
            })
    }

    // ---- clipboard -------------------------------------------------------------------------

    /// The selection as Markdown (whole marks it covers keep their
    /// delimiters), for the clipboard.
    pub fn selected_markdown(&self) -> String {
        let sel = self.selection();
        if sel.is_empty() || self.plain {
            return self.text[sel].to_string();
        }
        let (mut a, mut b) = (sel.start, sel.end);
        let (ra, rb) = (self.doc.row_of(a), self.doc.row_of(b));
        loop {
            let mut grew = false;
            for row in &self.doc.rows[ra..=rb] {
                for m in &row.marks {
                    let c = m.content();
                    if a <= c.start
                        && c.end <= b
                        && (m.open.start < a || m.close.end > b)
                        && a <= m.open.end
                        && m.close.start <= b
                    {
                        a = a.min(m.open.start);
                        b = b.max(m.close.end);
                        grew = true;
                    }
                }
            }
            if !grew {
                break;
            }
        }
        self.text[a..b].to_string()
    }

    /// Cut: the selection as Markdown, deleted.
    pub fn cut(&mut self) -> String {
        let out = self.selected_markdown();
        self.delete_selection();
        out
    }

    // ---- undo ------------------------------------------------------------------------------

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }

    pub fn undo(&mut self) -> bool {
        if self.marked.is_some() {
            return false;
        }
        let Some(step) = self.history.undo() else {
            return false;
        };
        let end = (step.at + step.new.len()).min(self.text.len());
        self.text.replace_range(step.at..end, &step.old);
        self.reparse();
        let len = self.text.len();
        let (a, h) = step.before;
        if a <= len && h <= len {
            self.anchor = self.doc.snap(a);
            self.head = self.doc.snap(h);
        } else {
            let at = self.doc.snap(step.at + step.old.len());
            self.anchor = at;
            self.head = at;
        }
        self.pending.clear();
        true
    }

    pub fn redo(&mut self) -> bool {
        if self.marked.is_some() {
            return false;
        }
        let Some(step) = self.history.redo() else {
            return false;
        };
        let end = (step.at + step.old.len()).min(self.text.len());
        self.text.replace_range(step.at..end, &step.new);
        self.reparse();
        let at = self.doc.snap(step.at + step.new.len());
        self.anchor = at;
        self.head = at;
        self.pending.clear();
        true
    }

    // ---- a change made elsewhere -----------------------------------------------------------

    /// The text became `new` from outside (the draft store merged someone
    /// else's change in, a conflict was settled): applied as one splice,
    /// the selection and the undo history carried through it.
    pub fn replace_from_outside(&mut self, new: &str) -> bool {
        if new == self.text {
            return false;
        }
        let splice = Splice::between(&self.text, new);
        if let Some(m) = &self.marked {
            if m.start < splice.end && splice.start < m.end.max(m.start + 1) {
                self.marked = None;
                self.composing = None;
            }
        }
        self.history
            .rebase(splice.start, splice.end, splice.text.len());
        let (a, h) = (splice.map(self.anchor), splice.map(self.head));
        self.marked = self
            .marked
            .take()
            .map(|m| splice.map(m.start)..splice.map(m.end));
        self.text = new.to_string();
        self.reparse();
        self.anchor = self.doc.snap(a.min(self.text.len()));
        self.head = self.doc.snap(h.min(self.text.len()));
        true
    }

    // ---- input methods ------------------------------------------------------------------------

    /// The input method's composition: `text` replaces `range` (or the
    /// marked text, or the selection) and is marked; `selected` is the
    /// caret within it.
    pub fn set_marked(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
    ) {
        let text = self.clean(text);
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let range = clamp_range(&self.text, range);
        if self.composing.is_none() {
            self.composing = Some((range.start, self.text[range.clone()].to_string()));
        }
        self.text.replace_range(range.clone(), &text);
        self.reparse();
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        let caret = selected.clone().map_or(range.start + text.len(), |s| {
            range.start + s.end.min(text.len())
        });
        let anchor = selected.map_or(caret, |s| range.start + s.start.min(text.len()));
        self.anchor = anchor;
        self.head = caret;
        if self.marked.is_none() {
            self.finish_composition();
        }
    }

    /// The input method committed `text` over `range` (or the marked text):
    /// the composition becomes one undo step.
    pub fn commit(&mut self, range: Option<Range<usize>>, text: &str) {
        let text = self.clean(text);
        if self.composing.is_none() {
            match range {
                Some(r) => {
                    let r = clamp_range(&self.text, r);
                    let caret = r.start + text.len();
                    self.apply(r, &text, (caret, caret), Kind::Other);
                }
                None => {
                    self.insert(&text);
                }
            }
            return;
        }
        let range = range
            .or_else(|| self.marked.clone())
            .unwrap_or_else(|| self.selection());
        let range = clamp_range(&self.text, range);
        self.text.replace_range(range.clone(), &text);
        let caret = range.start + text.len();
        self.marked = None;
        self.reparse();
        self.anchor = caret;
        self.head = caret;
        self.finish_composition();
    }

    /// The input method let go of its marked text as it stands.
    pub fn unmark(&mut self) {
        if self.marked.take().is_some() {
            self.finish_composition();
        }
    }

    /// Record the finished composition as one undo step.
    fn finish_composition(&mut self) {
        let Some((at, old)) = self.composing.take() else {
            return;
        };
        let end = self.head.max(at).min(self.text.len());
        let new = self.text[at..end].to_string();
        if new != old {
            let step = Step {
                at,
                old,
                new,
                before: (at, at),
            };
            self.history.record(step, Kind::Other, Instant::now());
            self.history.seal();
        }
    }

    // ---- UTF-16 offsets (the platform's input method counts in them) ------------------------

    pub fn utf16_len(&self) -> usize {
        self.text.encode_utf16().count()
    }

    pub fn to_utf16(&self, byte: usize) -> usize {
        let byte = floor_char(&self.text, byte.min(self.text.len()));
        self.text[..byte].encode_utf16().count()
    }

    pub fn from_utf16(&self, units: usize) -> usize {
        let mut seen = 0;
        for (i, c) in self.text.char_indices() {
            if seen >= units {
                return i;
            }
            seen += c.len_utf16();
        }
        self.text.len()
    }
}

/// A mark's delimiter as the formatting commands write it.
fn marker(kind: MarkKind) -> &'static str {
    match kind {
        MarkKind::Bold => "**",
        MarkKind::Italic => "*",
        MarkKind::Strike => "~~",
        MarkKind::Code => "`",
        MarkKind::Link | MarkKind::Escape => "",
    }
}

/// The block kind a row is, as the block commands name them.
fn block_kind(row: &Row) -> Option<BlockKind> {
    Some(match &row.kind {
        RowKind::Paragraph | RowKind::Empty => BlockKind::Paragraph,
        RowKind::Heading(n) => BlockKind::Heading(*n),
        RowKind::Item(item) => match (&item.marker, item.task) {
            (_, Some(_)) => BlockKind::Task,
            (Marker::Bullet(_), None) => BlockKind::Bullet,
            (Marker::Ordered { .. }, None) => BlockKind::Ordered,
        },
        RowKind::Quote => BlockKind::Quote,
        RowKind::Code | RowKind::Fence { .. } => BlockKind::Code,
        RowKind::Rule | RowKind::Raw => return None,
    })
}

/// The part of `sel` that's visible in `row`, without surrounding spaces:
/// source offsets snapped into the text.
fn trimmed_visible(row: &Row, sel: Range<usize>) -> Option<(usize, usize)> {
    let d0 = if sel.start <= row.content_start {
        0
    } else {
        row.display_of(sel.start)
    };
    let d1 = if sel.end >= row.lines.end {
        row.display.len()
    } else {
        row.display_of(sel.end)
    };
    if d0 >= d1 {
        return None;
    }
    let shown = &row.display[d0..d1];
    let lead = shown.len() - shown.trim_start().len();
    let trail = shown.len() - shown.trim_end().len();
    let (d0, d1) = (d0 + lead, d1 - trail);
    if d0 >= d1 {
        return None;
    }
    // The start stays outside any opening delimiter, the end inside a
    // closing one: the next chunk's start, the previous chunk's end.
    let a = row
        .chunks
        .iter()
        .find(|c| c.len > 0 && c.display <= d0 && d0 < c.display + c.len)
        .map(|c| c.source + (d0 - c.display))?;
    let b = row
        .chunks
        .iter()
        .find(|c| c.len > 0 && c.display < d1 && d1 <= c.display + c.len)
        .map(|c| c.source + (d1 - c.display))?;
    Some((a, b))
}

fn covered(gone: &[Range<usize>], v: &Range<usize>) -> bool {
    let mut at = v.start;
    let mut sorted: Vec<&Range<usize>> = gone.iter().collect();
    sorted.sort_by_key(|r| r.start);
    for r in sorted {
        if r.start <= at && at < r.end {
            at = r.end;
        }
        if at >= v.end {
            return true;
        }
    }
    at >= v.end
}

/// Where offset `p` lands after `edits` (sorted, disjoint); `after` puts
/// it after an insertion made right at it.
fn map_through(edits: &[(Range<usize>, String)], p: usize, after: bool) -> usize {
    let mut shift: isize = 0;
    for (r, s) in edits {
        let grow = s.len() as isize - r.len() as isize;
        if r.end < p || (r.end == p && !r.is_empty()) || (r.is_empty() && r.start == p && after) {
            shift += grow;
        } else if r.start < p && p < r.end {
            return (r.start as isize + shift) as usize + s.len();
        }
    }
    (p as isize + shift) as usize
}

fn clamp_range(text: &str, r: Range<usize>) -> Range<usize> {
    let s = floor_char(text, r.start.min(text.len()));
    let e = floor_char(text, r.end.min(text.len())).max(s);
    s..e
}

fn floor_char(text: &str, mut i: usize) -> usize {
    while i > 0 && !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// A character that joins the one before it into one grapheme (combining
/// marks, variation selectors, emoji modifiers, tags, the zero-width
/// joiner).
fn extends(c: char) -> bool {
    matches!(c as u32,
        0x300..=0x36F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20FF
        | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F | 0x1F3FB..=0x1F3FF | 0xE0020..=0xE007F | 0x200D)
}

/// The grapheme boundary after `i` in `s` (an approximation of Unicode's
/// that covers accents and emoji sequences).
pub fn next_boundary(s: &str, i: usize) -> usize {
    let mut it = s[i..].char_indices().peekable();
    let Some((_, first)) = it.next() else {
        return s.len();
    };
    let mut end = i + first.len_utf8();
    let mut joined = first == '\u{200D}';
    while let Some(&(off, c)) = it.peek() {
        if extends(c) || joined {
            joined = c == '\u{200D}';
            end = i + off + c.len_utf8();
            it.next();
        } else {
            break;
        }
    }
    end
}

/// The grapheme boundary before `i` in `s`.
pub fn prev_boundary(s: &str, i: usize) -> usize {
    let mut at = i;
    loop {
        let Some(c) = s[..at].chars().next_back() else {
            return 0;
        };
        at -= c.len_utf8();
        let before = s[..at].chars().next_back();
        if extends(c) || before == Some('\u{200D}') {
            continue;
        }
        return at;
    }
}

#[derive(PartialEq)]
enum Class {
    Space,
    Word,
    Punct,
}

fn class(c: char) -> Class {
    if c.is_whitespace() {
        Class::Space
    } else if c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// The start of the word before `i`.
pub fn word_left(s: &str, i: usize) -> usize {
    let mut chars = s[..i].char_indices().rev().peekable();
    while chars.peek().is_some_and(|(_, c)| class(*c) == Class::Space) {
        chars.next();
    }
    let Some(&(_, c)) = chars.peek() else {
        return 0;
    };
    let k = class(c);
    let mut at = i;
    for (j, c) in chars {
        if class(c) != k {
            break;
        }
        at = j;
    }
    at.min(i)
}

/// The end of the word after `i`.
pub fn word_right(s: &str, i: usize) -> usize {
    let mut chars = s[i..].char_indices().peekable();
    while chars.peek().is_some_and(|(_, c)| class(*c) == Class::Space) {
        chars.next();
    }
    let Some(&(_, c)) = chars.peek() else {
        return s.len();
    };
    let k = class(c);
    let mut at = s.len();
    for (j, c) in chars {
        if class(c) != k {
            at = i + j;
            break;
        }
    }
    at
}
