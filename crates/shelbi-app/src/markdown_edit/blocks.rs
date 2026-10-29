//! The rows a Markdown source lays out as while it is edited in place.
//!
//! The source is never rewritten to lay it out: a [`Doc`] reads it into
//! [`Row`]s (a paragraph, a heading, one list item, one line of code, ...),
//! each holding the text it shows (its *display*: the source with its
//! syntax hidden, soft line breaks as spaces) and how display offsets map
//! back to source bytes ([`Chunk`]s). Edits are made to the source; the doc
//! is read again after each one. So whatever the subset understands is
//! drawn formatted, and anything it doesn't (a table, HTML, a footnote)
//! shows as the source it is, never dropped.
//!
//! Blank lines separate rows and take no caret. A blank line with blank
//! lines (or the document's edge) on both sides is an empty paragraph the
//! caret can sit in: that's what Enter opens to start a new block, and what
//! an empty document is.

use std::ops::Range;

use super::inline::{self, Mark, Style};

/// What a row is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowKind {
    /// An empty paragraph (a blank line between blank lines).
    Empty,
    Paragraph,
    Heading(u8),
    Item(Item),
    Quote,
    /// A code fence's line (` ``` ` / `~~~`), shown as written; `open` for
    /// the opening one.
    Fence {
        open: bool,
    },
    /// A line of code (fenced or indented).
    Code,
    /// A thematic break (`---`).
    Rule,
    /// Source the editor doesn't render (a table row, HTML), as written.
    Raw,
}

/// A list item's marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Marker {
    Bullet(char),
    Ordered { number: u64, delim: char },
}

/// A list item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub marker: Marker,
    /// `Some(checked)` for a task-list item.
    pub task: Option<bool>,
    /// The source offset of the task box's mark (the ` ` or `x` in `[ ]`).
    pub toggle: Option<usize>,
    /// The marker's indentation, in source bytes.
    pub indent: usize,
    /// Where the marker and the space after it end (before a task box): the
    /// indentation a child item needs.
    pub marker_end: usize,
}

/// Room above a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gap {
    /// The first row, a line of the same code block, an empty row after
    /// another.
    None,
    /// Between the items of a plain list, or quote lines.
    Tight,
    /// Between the rows of a list holding tasks.
    List,
    /// Under a heading, or a list starting under text.
    Heading,
    /// Between blocks.
    Block,
}

/// A visible stretch of a row: `len` display bytes from `display`, the
/// same bytes of the source from `source`. Between two chunks lies hidden
/// syntax; `snap_right` says where a caret at their shared display offset
/// goes: to this chunk's start (past a line's prefix, so typing lands in
/// the text) or, otherwise, the previous chunk's end (before a closing
/// `**`, so typing at the end of bold text stays bold).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chunk {
    pub display: usize,
    pub source: usize,
    pub len: usize,
    pub snap_right: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub kind: RowKind,
    /// List nesting (0 at the margin).
    pub level: usize,
    /// From the start of the row's first line to the end of its last
    /// (without the `\n`).
    pub lines: Range<usize>,
    /// Where its text starts, past any prefix (`## `, `- [ ] `, `> `).
    pub content_start: usize,
    /// What it shows.
    pub display: String,
    pub chunks: Vec<Chunk>,
    /// Display ranges and their style, disjoint, in order.
    pub styles: Vec<(Range<usize>, Style)>,
    /// The inline syntax in it, in source offsets.
    pub marks: Vec<Mark>,
    pub gap: Gap,
    /// The code block (or raw block) the row belongs to, for drawing their
    /// background as one.
    pub block: Option<usize>,
}

impl Row {
    /// The display offset a source offset shows at. One inside hidden
    /// syntax shows where the syntax would be.
    pub fn display_of(&self, src: usize) -> usize {
        let mut last = 0;
        for c in &self.chunks {
            if src < c.source {
                return c.display;
            }
            if src <= c.source + c.len {
                return c.display + (src - c.source);
            }
            last = c.display + c.len;
        }
        last
    }

    /// The source offset a caret at display offset `d` stands for.
    pub fn source_of(&self, d: usize) -> usize {
        let d = d.min(self.display.len());
        let Some(mut i) = self.chunks.iter().position(|c| c.display + c.len >= d) else {
            return self
                .chunks
                .last()
                .map_or(self.content_start, |c| c.source + c.len);
        };
        loop {
            let c = self.chunks[i];
            let at_end = d == c.display + c.len;
            match self.chunks.get(i + 1) {
                Some(next) if at_end && next.display == d && next.snap_right => i += 1,
                _ => return c.source + (d - c.display),
            }
        }
    }

    /// The canonical caret for `src`: the source offset its display offset
    /// stands for.
    pub fn snap(&self, src: usize) -> usize {
        self.source_of(self.display_of(src))
    }

    /// The style at display offset `d` (of the character after it).
    pub fn style_at(&self, d: usize) -> Style {
        self.styles
            .iter()
            .find(|(r, _)| r.start <= d && d < r.end)
            .map_or(Style::default(), |(_, s)| *s)
    }

    /// The source byte ranges of the row's visible text.
    pub fn visible(&self) -> impl Iterator<Item = Range<usize>> + '_ {
        self.chunks
            .iter()
            .filter(|c| c.len > 0)
            .map(|c| c.source..c.source + c.len)
    }

    pub fn is_code(&self) -> bool {
        matches!(self.kind, RowKind::Code | RowKind::Fence { .. })
    }

    /// Whether the row's display is text the inline subset formatted (not
    /// code, raw source, a rule or an empty line).
    pub fn is_text(&self) -> bool {
        matches!(
            self.kind,
            RowKind::Paragraph | RowKind::Heading(_) | RowKind::Item(_) | RowKind::Quote
        )
    }

    pub fn task(&self) -> Option<bool> {
        match &self.kind {
            RowKind::Item(item) => item.task,
            _ => None,
        }
    }
}

/// A source laid out as rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Doc {
    pub rows: Vec<Row>,
}

impl Doc {
    /// `src` as Markdown.
    pub fn parse(src: &str) -> Doc {
        Doc { rows: build(src) }
    }

    /// `src` as one plain paragraph (a title): nothing is syntax.
    pub fn plain(src: &str) -> Doc {
        let display = src.replace('\n', " ");
        Doc {
            rows: vec![Row {
                kind: RowKind::Paragraph,
                level: 0,
                lines: 0..src.len(),
                content_start: 0,
                chunks: vec![Chunk {
                    display: 0,
                    source: 0,
                    len: src.len(),
                    snap_right: false,
                }],
                display,
                styles: Vec::new(),
                marks: Vec::new(),
                gap: Gap::None,
                block: None,
            }],
        }
    }

    /// The row a source offset belongs to: the last one starting at or
    /// before it.
    pub fn row_of(&self, src: usize) -> usize {
        self.rows
            .iter()
            .rposition(|r| r.lines.start <= src)
            .unwrap_or(0)
    }

    /// The row and display offset `src` shows at.
    pub fn locate(&self, src: usize) -> (usize, usize) {
        let row = self.row_of(src);
        (row, self.rows[row].display_of(src))
    }

    /// The canonical caret for `src`.
    pub fn snap(&self, src: usize) -> usize {
        let row = self.row_of(src);
        self.rows[row].snap(src)
    }
}

// ---- reading the source ----------------------------------------------------

/// One line of a row: where it starts, where its content starts, where it
/// ends; `hidden` for a setext heading's underline.
#[derive(Debug, Clone)]
struct Part {
    start: usize,
    content: usize,
    end: usize,
    hidden: bool,
}

#[derive(Debug)]
struct Building {
    kind: RowKind,
    level: usize,
    parts: Vec<Part>,
    block: Option<usize>,
}

enum Entry {
    Row(Building),
    Blank { start: usize, end: usize },
}

/// The lines of `src` with their start offsets. A final `\n` ends the last
/// line rather than starting an empty one.
fn lines(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = 0;
    for line in src.split('\n') {
        out.push((start, line));
        start += line.len() + 1;
    }
    if src.ends_with('\n') && out.len() > 1 {
        out.pop();
    }
    out
}

/// Leading whitespace: (columns, bytes). A tab runs to the next multiple of
/// four.
fn indent(line: &str) -> (usize, usize) {
    let mut cols = 0;
    let mut bytes = 0;
    for b in line.bytes() {
        match b {
            b' ' => cols += 1,
            b'\t' => cols += 4 - cols % 4,
            _ => break,
        }
        bytes += 1;
    }
    (cols, bytes)
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// An opening fence: its character, length, and the offset of its info
/// string.
fn fence_open(line: &str) -> Option<(u8, usize)> {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return None;
    }
    let rest = &line.as_bytes()[bytes..];
    let ch = *rest.first()?;
    if ch != b'`' && ch != b'~' {
        return None;
    }
    let n = rest.iter().take_while(|&&b| b == ch).count();
    if n < 3 {
        return None;
    }
    // A backtick fence's info string can't hold a backtick.
    if ch == b'`' && line[bytes + n..].contains('`') {
        return None;
    }
    Some((ch, n))
}

fn is_fence_close(line: &str, ch: u8, len: usize) -> bool {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return false;
    }
    let rest = &line.as_bytes()[bytes..];
    let n = rest.iter().take_while(|&&b| b == ch).count();
    n >= len && rest[n..].iter().all(|b| b.is_ascii_whitespace())
}

/// `## Heading`: its level and the offset its text starts at.
fn atx(line: &str) -> Option<(u8, usize)> {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return None;
    }
    let rest = &line[bytes..];
    let n = rest.bytes().take_while(|&b| b == b'#').count();
    if !(1..=6).contains(&n) {
        return None;
    }
    let after = &rest[n..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let spaces = after.len() - after.trim_start_matches([' ', '\t']).len();
    Some((n as u8, bytes + n + spaces))
}

fn is_rule(line: &str) -> bool {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return false;
    }
    let rest = line[bytes..].trim_end();
    let Some(ch) = rest.chars().next() else {
        return false;
    };
    matches!(ch, '-' | '*' | '_')
        && rest.chars().all(|c| c == ch || c == ' ' || c == '\t')
        && rest.chars().filter(|&c| c == ch).count() >= 3
}

/// A setext underline (`===` or `---` under a paragraph line): its level.
fn setext(line: &str) -> Option<u8> {
    let (cols, bytes) = indent(line);
    if cols > 3 {
        return None;
    }
    let rest = line[bytes..].trim_end();
    if !rest.is_empty() && rest.bytes().all(|b| b == b'=') {
        Some(1)
    } else if !rest.is_empty() && rest.bytes().all(|b| b == b'-') {
        Some(2)
    } else {
        None
    }
}

struct ItemLine {
    indent_cols: usize,
    indent_bytes: usize,
    marker: Marker,
    /// Where the item's text starts (after the task box, if any).
    content: usize,
    /// Where the marker and its spaces end.
    marker_end: usize,
    /// Columns from the line start to where a child item can start.
    content_col: usize,
    task: Option<(bool, usize)>,
}

/// `- item`, `1. item`, `- [ ] task`.
fn list_item(line: &str) -> Option<ItemLine> {
    let (indent_cols, indent_bytes) = indent(line);
    let rest = &line[indent_bytes..];
    let bytes = rest.as_bytes();
    let (marker, width) = match bytes.first()? {
        c @ (b'-' | b'*' | b'+') => (Marker::Bullet(*c as char), 1),
        b'0'..=b'9' => {
            let digits = bytes.iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 {
                return None;
            }
            let delim = *bytes.get(digits)?;
            if delim != b'.' && delim != b')' {
                return None;
            }
            let number = rest[..digits].parse().ok()?;
            (
                Marker::Ordered {
                    number,
                    delim: delim as char,
                },
                digits + 1,
            )
        }
        _ => return None,
    };
    let after = &rest[width..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let spaces = (after.len() - after.trim_start_matches([' ', '\t']).len()).min(4);
    let spaces = if after.trim().is_empty() {
        after.len()
    } else {
        spaces
    };
    let mut content = indent_bytes + width + spaces;
    let marker_end = content;
    let content_col = indent_cols + width + spaces.max(1);
    let mut task = None;
    let text = &line[content..];
    if text.len() >= 3 && text.as_bytes()[0] == b'[' && text.as_bytes()[2] == b']' {
        let mark = text.as_bytes()[1];
        let follows = &text[3..];
        if matches!(mark, b' ' | b'x' | b'X')
            && (follows.is_empty() || follows.starts_with([' ', '\t']))
        {
            task = Some((mark != b' ', content + 1));
            content += 3 + usize::from(!follows.is_empty());
        }
    }
    Some(ItemLine {
        indent_cols,
        indent_bytes,
        marker,
        content,
        marker_end,
        content_col,
        task,
    })
}

/// `> quote`: where its text starts.
fn quote(line: &str) -> Option<usize> {
    let (cols, bytes) = indent(line);
    if cols > 3 || !line[bytes..].starts_with('>') {
        return None;
    }
    let after = &line[bytes + 1..];
    Some(bytes + 1 + usize::from(after.starts_with([' ', '\t'])))
}

fn is_table(line: &str) -> bool {
    let (cols, bytes) = indent(line);
    cols <= 3 && line[bytes..].starts_with('|')
}

fn is_html_start(line: &str) -> bool {
    let (cols, bytes) = indent(line);
    let rest = &line[bytes..];
    cols <= 3
        && rest.starts_with('<')
        && rest[1..]
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '/' || c == '!' || c == '?')
}

fn part(start: usize, content: usize, line: &str) -> Part {
    Part {
        start,
        content,
        end: start + line.len(),
        hidden: false,
    }
}

fn row(kind: RowKind, level: usize, p: Part) -> Entry {
    Entry::Row(Building {
        kind,
        level,
        parts: vec![p],
        block: None,
    })
}

/// Lay `src` out as rows.
fn build(src: &str) -> Vec<Row> {
    let mut entries: Vec<Entry> = Vec::new();
    // An open fence: its character, length and block.
    let mut fence: Option<(u8, usize, usize)> = None;
    let mut html = false;
    let mut list: Vec<usize> = Vec::new();
    let mut blocks = 0usize;
    let mut next_block = || {
        blocks += 1;
        blocks
    };
    for (start, line) in lines(src) {
        let whole = |content: usize| part(start, start + content, line);
        if let Some((ch, len, block)) = fence {
            let closing = is_fence_close(line, ch, len);
            let kind = if closing {
                RowKind::Fence { open: false }
            } else {
                RowKind::Code
            };
            let mut e = row(kind, 0, whole(0));
            if let Entry::Row(b) = &mut e {
                b.block = Some(block);
            }
            entries.push(e);
            if closing {
                fence = None;
            }
            continue;
        }
        if is_blank(line) {
            entries.push(Entry::Blank {
                start,
                end: start + line.len(),
            });
            html = false;
            continue;
        }
        let (last_kind, last_block) = match entries.last() {
            Some(Entry::Row(b)) => (Some(b.kind.clone()), b.block),
            _ => (None, None),
        };
        if html {
            let block = last_block;
            let mut e = row(RowKind::Raw, 0, whole(0));
            if let Entry::Row(b) = &mut e {
                b.block = block;
            }
            entries.push(e);
            continue;
        }
        let joinable = matches!(
            last_kind,
            Some(RowKind::Paragraph | RowKind::Item(_) | RowKind::Quote)
        );
        let (cols, ind_bytes) = indent(line);
        if cols >= 4 && !joinable && list.is_empty() {
            // Indented code: one block with the code lines right above.
            let block = match last_kind {
                Some(RowKind::Code) if last_block.is_some() => last_block,
                _ => Some(next_block()),
            };
            let mut e = row(RowKind::Code, 0, whole(0));
            if let Entry::Row(b) = &mut e {
                b.block = block;
            }
            entries.push(e);
            continue;
        }
        if let Some((ch, len)) = fence_open(line) {
            let block = next_block();
            fence = Some((ch, len, block));
            let mut e = row(RowKind::Fence { open: true }, 0, whole(0));
            if let Entry::Row(b) = &mut e {
                b.block = Some(block);
            }
            entries.push(e);
            list.clear();
            continue;
        }
        if let Some((level, content)) = atx(line) {
            entries.push(row(RowKind::Heading(level), 0, whole(content)));
            list.clear();
            continue;
        }
        if let Some(level) = setext(line) {
            if let Some(b) = last_row(&mut entries).filter(|b| b.kind == RowKind::Paragraph) {
                b.kind = RowKind::Heading(level);
                let mut p = whole(line.len());
                p.hidden = true;
                b.parts.push(p);
                continue;
            }
        }
        if is_rule(line) {
            entries.push(row(RowKind::Rule, 0, whole(line.len())));
            continue;
        }
        if let Some(item) = list_item(line) {
            while list.last().is_some_and(|&c| c > item.indent_cols) {
                list.pop();
            }
            let level = list.len();
            list.push(item.content_col);
            let kind = RowKind::Item(Item {
                marker: item.marker,
                task: item.task.map(|(checked, _)| checked),
                toggle: item.task.map(|(_, at)| start + at),
                indent: item.indent_bytes,
                marker_end: start + item.marker_end,
            });
            entries.push(row(kind, level, whole(item.content)));
            continue;
        }
        if let Some(content) = quote(line) {
            match last_row(&mut entries) {
                Some(b) if b.kind == RowKind::Quote && !line[content..].trim().is_empty() => {
                    b.parts.push(whole(content));
                }
                _ => entries.push(row(RowKind::Quote, 0, whole(content))),
            }
            continue;
        }
        if is_table(line) {
            let block = match last_kind {
                Some(RowKind::Raw) => last_block,
                _ => Some(next_block()),
            };
            let mut e = row(RowKind::Raw, 0, whole(0));
            if let Entry::Row(b) = &mut e {
                b.block = block;
            }
            entries.push(e);
            continue;
        }
        if is_html_start(line) && !joinable {
            html = true;
            let mut e = row(RowKind::Raw, 0, whole(0));
            if let Entry::Row(b) = &mut e {
                b.block = Some(next_block());
            }
            entries.push(e);
            continue;
        }
        // Text: a lazy continuation of the row above, or a new paragraph
        // (inside the list when indented under an item).
        if joinable {
            if let Some(b) = last_row(&mut entries) {
                b.parts.push(whole(ind_bytes));
            }
            continue;
        }
        let level = list.iter().filter(|&&c| c <= cols).count();
        if level == 0 {
            list.clear();
        }
        entries.push(row(RowKind::Paragraph, level, whole(ind_bytes)));
    }

    // A blank line between blank lines (or the edge) is an empty paragraph.
    let n = entries.len();
    let blank = |i: usize| matches!(entries.get(i), Some(Entry::Blank { .. }));
    let empties: Vec<bool> = (0..n)
        .map(|i| blank(i) && (i == 0 || blank(i - 1)) && (i + 1 == n || blank(i + 1)))
        .collect();

    let mut rows: Vec<Row> = Vec::new();
    let mut blanks = 0usize;
    for (i, e) in entries.into_iter().enumerate() {
        let b = match e {
            Entry::Blank { start, end } if empties[i] => Building {
                kind: RowKind::Empty,
                level: 0,
                parts: vec![Part {
                    start,
                    content: start,
                    end,
                    hidden: false,
                }],
                block: None,
            },
            Entry::Blank { .. } => {
                blanks += 1;
                continue;
            }
            Entry::Row(b) => b,
        };
        let gap = match rows.last() {
            None => Gap::None,
            // Under a heading the frames leave its own gap, blank line or
            // not (an empty paragraph opened there keeps a block's).
            Some(prev) if blanks > 0 => match prev.kind {
                RowKind::Heading(_) if b.kind != RowKind::Empty => Gap::Heading,
                _ => Gap::Block,
            },
            Some(prev) => gap_between(prev, &b),
        };
        blanks = 0;
        rows.push(finish(src, b, gap));
    }
    if rows.is_empty() {
        // Only blank lines: one empty paragraph at the start.
        rows.push(finish(
            src,
            Building {
                kind: RowKind::Empty,
                level: 0,
                parts: vec![Part {
                    start: 0,
                    content: 0,
                    end: 0,
                    hidden: false,
                }],
                block: None,
            },
            Gap::None,
        ));
    }
    rows
}

fn last_row(entries: &mut [Entry]) -> Option<&mut Building> {
    match entries.last_mut() {
        Some(Entry::Row(b)) => Some(b),
        _ => None,
    }
}

fn gap_between(prev: &Row, next: &Building) -> Gap {
    match (&prev.kind, &next.kind) {
        (RowKind::Empty, RowKind::Empty) => Gap::None,
        (RowKind::Heading(_), _) => Gap::Heading,
        (RowKind::Item(a), RowKind::Item(b)) => {
            if a.task.is_some() || b.task.is_some() {
                Gap::List
            } else {
                Gap::Tight
            }
        }
        _ if prev.block.is_some() && prev.block == next.block => Gap::None,
        (RowKind::Quote, RowKind::Quote) => Gap::Tight,
        (_, RowKind::Item(_)) => Gap::Heading,
        _ => Gap::Block,
    }
}

/// Read a row's text: its display, chunks, styles and marks.
fn finish(src: &str, b: Building, gap: Gap) -> Row {
    let first = &b.parts[0];
    let lines = first.start..b.parts.last().map_or(first.end, |p| p.end);
    let content_start = first.content;
    let mut row = Row {
        kind: b.kind,
        level: b.level,
        lines,
        content_start,
        display: String::new(),
        chunks: Vec::new(),
        styles: Vec::new(),
        marks: Vec::new(),
        gap,
        block: b.block,
    };
    match row.kind {
        RowKind::Code | RowKind::Fence { .. } | RowKind::Raw => {
            row.display = src[first.content..first.end].to_string();
            row.chunks.push(Chunk {
                display: 0,
                source: first.content,
                len: first.end - first.content,
                snap_right: false,
            });
            if row.kind == RowKind::Raw {
                row.styles.push((
                    0..row.display.len(),
                    Style {
                        raw: true,
                        ..Style::default()
                    },
                ));
            }
            return row;
        }
        RowKind::Rule | RowKind::Empty => {
            let at = if row.kind == RowKind::Rule {
                first.end
            } else {
                first.start
            };
            row.content_start = at;
            row.chunks.push(Chunk {
                display: 0,
                source: at,
                len: 0,
                snap_right: false,
            });
            return row;
        }
        _ => {}
    }

    // The content: each shown line's text after its prefix, `\n` between.
    // `pieces` maps it back: (content offset, source offset, length).
    let shown: Vec<&Part> = b.parts.iter().filter(|p| !p.hidden).collect();
    let mut content = String::new();
    let mut pieces: Vec<(usize, usize, usize)> = Vec::new();
    for (i, p) in shown.iter().enumerate() {
        let at = content.len();
        content.push_str(&src[p.content..p.end]);
        let mut len = p.end - p.content;
        if i + 1 < shown.len() {
            content.push('\n');
            len += 1;
        }
        pieces.push((at, p.content, len));
    }
    let parsed = inline::parse(&content);
    let hidden_before = |c: usize| -> usize {
        parsed
            .hidden
            .iter()
            .map(|r| r.end.min(c).saturating_sub(r.start))
            .sum()
    };
    let to_source = |c: usize| -> usize {
        pieces
            .iter()
            .rev()
            .find(|(at, _, _)| *at <= c)
            .map_or(content_start, |(at, s, _)| s + (c - at))
    };

    // Visible ranges of the content: what the hidden ranges leave.
    let mut visible: Vec<Range<usize>> = Vec::new();
    let mut at = 0;
    for h in &parsed.hidden {
        if h.start > at {
            visible.push(at..h.start);
        }
        at = h.end;
    }
    if at < content.len() {
        visible.push(at..content.len());
    }
    for (pi, &(pc, ps, plen)) in pieces.iter().enumerate() {
        row.chunks.push(Chunk {
            display: pc - hidden_before(pc),
            source: ps,
            len: 0,
            snap_right: pi > 0,
        });
        for v in &visible {
            let a = v.start.max(pc);
            let z = v.end.min(pc + plen);
            if a < z {
                row.chunks.push(Chunk {
                    display: a - hidden_before(a),
                    source: ps + (a - pc),
                    len: z - a,
                    snap_right: false,
                });
            }
        }
    }
    let mut display = String::with_capacity(content.len());
    for v in &visible {
        display.push_str(&content[v.clone()]);
    }
    // Soft breaks show as spaces, hard breaks as new lines.
    let breaks: Vec<usize> = parsed
        .breaks
        .iter()
        .map(|&c| c - hidden_before(c))
        .collect();
    row.display = display
        .char_indices()
        .map(|(i, ch)| {
            if ch == '\n' && !breaks.contains(&i) {
                ' '
            } else {
                ch
            }
        })
        .collect();
    row.styles = parsed
        .flat_spans()
        .into_iter()
        .map(|(r, s)| {
            (
                r.start - hidden_before(r.start)..r.end - hidden_before(r.end),
                s,
            )
        })
        .filter(|(r, _)| !r.is_empty())
        .collect();
    row.marks = parsed
        .marks
        .iter()
        .map(|m| Mark {
            kind: m.kind,
            open: to_source(m.open.start)..to_source(m.open.end),
            close: to_source(m.close.start)..to_source(m.close.end),
            url: m.url.clone(),
        })
        .collect();
    row
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<(String, String)> {
        Doc::parse(src)
            .rows
            .iter()
            .map(|r| {
                let k = match &r.kind {
                    RowKind::Empty => "empty".to_string(),
                    RowKind::Paragraph => format!("p{}", r.level),
                    RowKind::Heading(n) => format!("h{n}"),
                    RowKind::Item(i) => match i.task {
                        Some(true) => format!("done{}", r.level),
                        Some(false) => format!("task{}", r.level),
                        None => format!("item{}", r.level),
                    },
                    RowKind::Quote => "quote".into(),
                    RowKind::Fence { open } => format!("fence{}", if *open { "+" } else { "-" }),
                    RowKind::Code => "code".into(),
                    RowKind::Rule => "rule".into(),
                    RowKind::Raw => "raw".into(),
                };
                (k, r.display.clone())
            })
            .collect()
    }

    fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
        v.iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect()
    }

    #[test]
    fn the_feature_template_lays_out_as_blocks() {
        let src = "# Board keys\n\nMove between columns with **the TUI's** keys.\n\n## Wireframes\n\n```\n| A | B |\n```\n\n## Technical Details\n\n- Keys come from `keys.yaml`\n- See [the plan](https://example.com/plan)\n\n## Acceptance Criteria\n- [ ] h/l move\n- [x] j/k move\n";
        assert_eq!(
            kinds(src),
            pairs(&[
                ("h1", "Board keys"),
                ("p0", "Move between columns with the TUI's keys."),
                ("h2", "Wireframes"),
                ("fence+", "```"),
                ("code", "| A | B |"),
                ("fence-", "```"),
                ("h2", "Technical Details"),
                ("item0", "Keys come from keys.yaml"),
                ("item0", "See the plan"),
                ("h2", "Acceptance Criteria"),
                ("task0", "h/l move"),
                ("done0", "j/k move"),
            ])
        );
    }

    #[test]
    fn soft_wrapped_lines_join_into_one_paragraph() {
        let src = "one line\nwraps here\n  and here\n\nnext";
        assert_eq!(
            kinds(src),
            pairs(&[("p0", "one line wraps here and here"), ("p0", "next")])
        );
        let doc = Doc::parse(src);
        // The second line's indent is a prefix: the caret after the space
        // lands past it.
        let r = &doc.rows[0];
        let d = "one line wraps here ".len();
        assert_eq!(&src[r.source_of(d)..r.source_of(d) + 3], "and");
    }

    #[test]
    fn nested_lists_and_continuations_keep_their_level() {
        let src = "- a\n  - b\n    - c\n  more of b\n- d\n\n  para in d\n\nout";
        assert_eq!(
            kinds(src),
            pairs(&[
                ("item0", "a"),
                ("item1", "b"),
                ("item2", "c more of b"),
                ("item0", "d"),
                ("p1", "para in d"),
                ("p0", "out")
            ])
        );
        let ordered = "1. one\n2) two\n10. ten";
        assert_eq!(
            kinds(ordered),
            pairs(&[("item0", "one"), ("item0", "two"), ("item0", "ten")])
        );
    }

    #[test]
    fn unsupported_blocks_stay_visible_as_source() {
        let src = "| a | b |\n|---|---|\n| 1 | 2 |\n\n<details>\n<summary>x</summary>\n</details>\n\n> quoted **text**\n> more";
        assert_eq!(
            kinds(src),
            pairs(&[
                ("raw", "| a | b |"),
                ("raw", "|---|---|"),
                ("raw", "| 1 | 2 |"),
                ("raw", "<details>"),
                ("raw", "<summary>x</summary>"),
                ("raw", "</details>"),
                ("quote", "quoted text more"),
            ])
        );
    }

    #[test]
    fn rules_setext_headings_and_indented_code() {
        let src = "Title\n=====\n\ntext\n\n---\n\n    code line\n    more\n";
        assert_eq!(
            kinds(src),
            pairs(&[
                ("h1", "Title"),
                ("p0", "text"),
                ("rule", ""),
                ("code", "    code line"),
                ("code", "    more")
            ])
        );
    }

    #[test]
    fn blank_lines_are_gaps_unless_isolated() {
        assert_eq!(kinds(""), pairs(&[("empty", "")]));
        assert_eq!(kinds("a\n\nb"), pairs(&[("p0", "a"), ("p0", "b")]));
        assert_eq!(kinds("a\n\n\nb"), pairs(&[("p0", "a"), ("p0", "b")]));
        assert_eq!(
            kinds("a\n\n\n\nb"),
            pairs(&[("p0", "a"), ("empty", ""), ("p0", "b")])
        );
        // The line a new paragraph opens on at the end of a body.
        assert_eq!(kinds("a\n\n\n"), pairs(&[("p0", "a"), ("empty", "")]));
        let doc = Doc::parse("a\n\n\n");
        assert_eq!(doc.rows[1].content_start, 3);
        assert_eq!(doc.rows[1].gap, Gap::Block);
    }

    #[test]
    fn gaps_follow_the_frames() {
        let doc = Doc::parse("## Head\n- [ ] a\n- [ ] b\n\ntext\n- x\n- y");
        let gaps: Vec<Gap> = doc.rows.iter().map(|r| r.gap).collect();
        assert_eq!(
            gaps,
            [
                Gap::None,
                Gap::Heading,
                Gap::List,
                Gap::Block,
                Gap::Heading,
                Gap::Tight
            ]
        );
    }

    #[test]
    fn carets_snap_out_of_syntax() {
        let src = "- [ ] **bold** end";
        let doc = Doc::parse(src);
        let r = &doc.rows[0];
        assert_eq!(r.display, "bold end");
        // At the start: past the task box, before the opening `**`.
        assert_eq!(r.source_of(0), 6);
        // After "bold": still inside the bold (before the closing `**`).
        assert_eq!(r.source_of(4), 12);
        assert_eq!(r.display_of(14), 4);
        assert_eq!(r.snap(0), 6);
        assert_eq!(r.snap(13), 12);
        assert_eq!(r.task(), Some(false));
        let RowKind::Item(item) = &r.kind else {
            panic!()
        };
        assert_eq!(&src[item.toggle.unwrap()..item.toggle.unwrap() + 1], " ");
    }

    #[test]
    fn marks_are_in_source_offsets() {
        let src = "> a *b*\n> **c\n> d**";
        let doc = Doc::parse(src);
        let r = &doc.rows[0];
        assert_eq!(r.display, "a b c d");
        let bold = r
            .marks
            .iter()
            .find(|m| m.kind == inline::MarkKind::Bold)
            .unwrap();
        assert_eq!(&src[bold.open.clone()], "**");
        assert_eq!(&src[bold.content()], "c\n> d");
        let italic_style = r.style_at(2);
        assert!(italic_style.italic);
    }

    #[test]
    fn hard_breaks_show_as_new_lines() {
        let doc = Doc::parse("one\\\ntwo  \nthree");
        assert_eq!(doc.rows[0].display, "one\ntwo  \nthree");
    }
}
