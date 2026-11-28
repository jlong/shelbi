//! Inline-markdown rendering for the note bodies shown in the review and
//! kanban panes.
//!
//! Issue and review notes are stored as plain markdown strings. The inline
//! constructs we surface visually are backtick `code`, painted with a
//! subtle background so it reads as code against the surrounding prose, and
//! `**bold**`, drawn bold with its asterisks dropped.
//!
//! We deliberately do **not** lean on `Paragraph`'s built-in `Wrap` for
//! this. ratatui word-wraps *after* the spans are styled, and a
//! background-styled span that lands at the wrap boundary paints its
//! background across the trailing padding to the paragraph's edge — so an
//! inline-code highlight sitting at the right edge of a wrapped line bleeds
//! past the pane's content area into the margin/border. To keep the
//! highlight clamped to the glyphs it covers we word-wrap here to a known
//! `width` and hand the caller pre-wrapped `Line`s (rendered *without*
//! `.wrap()`), so a code span never crosses the boundary and its background
//! only ever covers characters inside `width`.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use unicode_width::UnicodeWidthStr;

use crate::theme::palette;

/// Inline `code` spans: the theme's `code_fg` on `code_bg`, a tint just off
/// the default background (in dark, xterm 236 under 252: a light gray that
/// stays legible on the tint without reading as a hard white box).
fn code_style() -> Style {
    Style::default().fg(palette().code_fg).bg(palette().code_bg)
}

/// Render a markdown note `body` into pre-wrapped lines for a pane of the
/// given inner `width` (in cells). Backtick spans become highlighted inline
/// code; everything else is plain text. Wrapping happens here so the code
/// highlight never overflows the pane's right edge.
///
/// A `width` of 0 disables wrapping (each source line becomes one output
/// line) — callers only hit this during a zero-area draw.
pub fn render_note(body: &str, width: usize) -> Vec<Line<'static>> {
    let mut out = Vec::new();
    // `split('\n')` keeps blank lines (and the empty tail of a trailing
    // newline) so the note's vertical spacing is preserved.
    for src in body.split('\n') {
        let runs = parse_runs(src);
        wrap_runs(runs, width, &mut out);
    }
    out
}

/// How a slice of text is marked up: inline code, bold, or both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct Mark {
    code: bool,
    bold: bool,
}

impl Mark {
    fn style(self) -> Style {
        let style = if self.code { code_style() } else { Style::default() };
        if self.bold {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }
}

/// One styled slice of a word; its `mark` picks the style.
struct Seg {
    text: String,
    mark: Mark,
}

/// A token in a source line: a run of whitespace (carrying its own code
/// flag so an inline-code highlight stays continuous across internal
/// spaces), or a word (a maximal run of non-whitespace, which may span a
/// code/plain boundary).
enum Tok {
    Space(Seg),
    Word(Vec<Seg>),
}

/// Split a source line into `(text, mark)` runs: inline code on backtick
/// pairs, then bold on `**` pairs in the text between ([`bold_runs`]). An
/// unclosed trailing backtick, or an unpaired `**`, is rendered literally
/// so no text is lost.
fn parse_runs(line: &str) -> Vec<(String, Mark)> {
    let pieces: Vec<&str> = line.split('`').collect();
    let last = pieces.len().saturating_sub(1);
    let mut code_runs: Vec<(String, bool)> = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        let is_code_slot = i % 2 == 1;
        if is_code_slot && i != last {
            // A closing backtick followed this piece — real inline code.
            code_runs.push((piece.to_string(), true));
        } else if is_code_slot {
            // Opening backtick with no partner: keep the backtick as text.
            code_runs.push((format!("`{piece}"), false));
        } else {
            code_runs.push((piece.to_string(), false));
        }
    }
    let mut runs: Vec<(String, Mark)> = Vec::new();
    for (text, code) in code_runs {
        if code {
            runs.push((text, Mark { code, bold: false }));
        } else {
            runs.extend(bold_runs(&text));
        }
    }
    runs.retain(|(text, _)| !text.is_empty());
    runs
}

/// Split plain text on `**` pairs, CommonMark-style but simplified: an
/// opening `**` is followed by a non-space, its closing one comes after a
/// non-space, and a `**` with no partner (`2 ** 3`) is text.
fn bold_runs(text: &str) -> Vec<(String, Mark)> {
    let marks: Vec<usize> = text.match_indices("**").map(|(i, _)| i).collect();
    let opens = |i: usize| text[i + 2..].chars().next().is_some_and(|c| !c.is_whitespace());
    let closes = |i: usize| text[..i].chars().next_back().is_some_and(|c| !c.is_whitespace());
    let mut runs = Vec::new();
    let mut from = 0;
    let mut k = 0;
    while k < marks.len() {
        let open = marks[k];
        let close = (open >= from && opens(open))
            .then(|| marks[k + 1..].iter().position(|&c| c >= open + 3 && closes(c)))
            .flatten()
            .map(|p| k + 1 + p);
        match close {
            Some(c) => {
                runs.push((text[from..open].to_string(), Mark::default()));
                let bold = Mark { code: false, bold: true };
                runs.push((text[open + 2..marks[c]].to_string(), bold));
                from = marks[c] + 2;
                k = c + 1;
            }
            None => k += 1,
        }
    }
    runs.push((text[from..].to_string(), Mark::default()));
    runs
}

/// Break `runs` into whitespace / word tokens, merging adjacent code and
/// plain slices with no space between them into a single word so they wrap
/// as one unit (e.g. project-level `` `default_branch` ``).
fn tokenize(runs: Vec<(String, Mark)>) -> Vec<Tok> {
    let mut toks: Vec<Tok> = Vec::new();
    let mut word: Vec<Seg> = Vec::new();
    for (text, mark) in runs {
        for (is_space, chunk) in split_ws(&text) {
            if is_space {
                if !word.is_empty() {
                    toks.push(Tok::Word(std::mem::take(&mut word)));
                }
                toks.push(Tok::Space(Seg { text: chunk, mark }));
            } else {
                word.push(Seg { text: chunk, mark });
            }
        }
    }
    if !word.is_empty() {
        toks.push(Tok::Word(word));
    }
    toks
}

/// Split `s` into maximal whitespace / non-whitespace runs, each tagged
/// with whether it is whitespace.
fn split_ws(s: &str) -> Vec<(bool, String)> {
    let mut runs: Vec<(bool, String)> = Vec::new();
    let mut cur = String::new();
    let mut cur_space: Option<bool> = None;
    for ch in s.chars() {
        let sp = ch == ' ' || ch == '\t';
        match cur_space {
            Some(b) if b == sp => cur.push(ch),
            Some(b) => {
                runs.push((b, std::mem::take(&mut cur)));
                cur.push(ch);
                cur_space = Some(sp);
            }
            None => {
                cur.push(ch);
                cur_space = Some(sp);
            }
        }
    }
    if let Some(b) = cur_space {
        runs.push((b, cur));
    }
    runs
}

fn cell_width(s: &str) -> usize {
    s.chars().count()
}

/// Greedy word-wrap of one source line's tokens into `out`, always emitting
/// at least one line (blank lines are preserved).
fn wrap_runs(runs: Vec<(String, Mark)>, width: usize, out: &mut Vec<Line<'static>>) {
    let toks = tokenize(runs);
    let start_len = out.len();

    if width == 0 {
        out.push(toks_to_line(&toks));
        return;
    }

    let mut cur: Vec<Span<'static>> = Vec::new();
    let mut cur_w = 0usize;
    // Separator whitespace is held back until we know a word follows on the
    // same line, so trailing space is trimmed at a wrap and never painted.
    // Each segment keeps its code flag so a highlight spanning an internal
    // space stays continuous.
    let mut pending: Vec<Seg> = Vec::new();

    for tok in toks {
        match tok {
            Tok::Space(seg) => {
                if cur.is_empty() {
                    // Leading indent on a fresh line — keep it verbatim.
                    cur_w += cell_width(&seg.text);
                    push_seg(&mut cur, seg);
                } else {
                    pending.push(seg);
                }
            }
            Tok::Word(segs) => {
                let ww: usize = segs.iter().map(|s| cell_width(&s.text)).sum();
                let sep_w: usize = pending.iter().map(|s| cell_width(&s.text)).sum();
                if cur_w > 0 && cur_w + sep_w + ww > width {
                    out.push(Line::from(std::mem::take(&mut cur)));
                    cur_w = 0;
                    pending.clear();
                } else {
                    for seg in pending.drain(..) {
                        cur_w += cell_width(&seg.text);
                        push_seg(&mut cur, seg);
                    }
                }

                if ww <= width {
                    for seg in segs {
                        push_seg(&mut cur, seg);
                    }
                    cur_w += ww;
                } else {
                    // Word wider than the whole pane: hard-split it so the
                    // code highlight can't run past the right edge.
                    hard_split(segs, width, &mut cur, &mut cur_w, out);
                }
            }
        }
    }

    if !cur.is_empty() {
        out.push(Line::from(cur));
    } else if out.len() == start_len {
        // Blank source line — keep a blank output line for spacing.
        out.push(Line::from(""));
    }
}

/// Append a word wider than `width` a line at a time, coalescing same-style
/// runs into spans so the code highlight stays glyph-tight on every row.
fn hard_split(
    segs: Vec<Seg>,
    width: usize,
    cur: &mut Vec<Span<'static>>,
    cur_w: &mut usize,
    out: &mut Vec<Line<'static>>,
) {
    let chars: Vec<(char, Mark)> = segs
        .iter()
        .flat_map(|s| s.text.chars().map(move |c| (c, s.mark)))
        .collect();

    let mut i = 0;
    while i < chars.len() {
        if *cur_w >= width {
            out.push(Line::from(std::mem::take(cur)));
            *cur_w = 0;
        }
        let avail = width - *cur_w;
        let end = (i + avail).min(chars.len());
        let mut j = i;
        while j < end {
            let mark = chars[j].1;
            let mut text = String::new();
            while j < end && chars[j].1 == mark {
                text.push(chars[j].0);
                j += 1;
            }
            cur.push(Span::styled(text, mark.style()));
        }
        *cur_w += end - i;
        i = end;
    }
}

fn push_seg(cur: &mut Vec<Span<'static>>, seg: Seg) {
    if seg.text.is_empty() {
        return;
    }
    cur.push(Span::styled(seg.text, seg.mark.style()));
}

/// Flatten tokens onto a single unwrapped line (the `width == 0` fallback).
fn toks_to_line(toks: &[Tok]) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    for tok in toks {
        match tok {
            Tok::Space(seg) => spans.push(Span::styled(seg.text.clone(), seg.mark.style())),
            Tok::Word(segs) => {
                for seg in segs {
                    if seg.text.is_empty() {
                        continue;
                    }
                    spans.push(Span::styled(seg.text.clone(), seg.mark.style()));
                }
            }
        }
    }
    Line::from(spans)
}

// ---- whole documents (the Wiki) ---------------------------------------------

/// Render a whole Markdown document (a wiki page) into pre-wrapped lines for
/// a pane `width` cells wide: headings, lists and task lists, quotes, code
/// blocks and rules as blocks, and inside them the inline styles. The rows
/// come from the shared Markdown model the desktop's document editor lays
/// out ([`shelbi_app::markdown_edit::Doc`]), so both clients read a document
/// the same way. A table draws as a box-drawn grid, its cells wrapping in
/// their columns ([`draw_table`]); what the model doesn't render (HTML) shows
/// as written, muted. A leading frontmatter block is left out.
pub fn render_document(src: &str, width: usize) -> Vec<Line<'static>> {
    use shelbi_app::markdown_edit::{Doc, Gap, Marker, RowKind, TablePart};
    let width = width.max(8);
    let body = strip_frontmatter(src);
    let doc = Doc::parse(body);
    let p = palette();
    let mut out: Vec<Line<'static>> = Vec::new();
    for (ri, row) in doc.rows.iter().enumerate() {
        let spaced = matches!(row.gap, Gap::Block | Gap::Heading | Gap::List);
        if spaced && !out.is_empty() && !matches!(row.kind, RowKind::Fence { .. }) {
            out.push(Line::default());
        }
        let segs = || styled_segments(&row.display, &row.styles);
        match &row.kind {
            RowKind::Empty => out.push(Line::default()),
            RowKind::Paragraph => wrap_segments(segs(), width, "", "", Style::default(), &mut out),
            RowKind::Heading(level) => {
                let mut base = Style::default().add_modifier(Modifier::BOLD);
                if *level <= 2 {
                    base = base.fg(p.ansi_cyan);
                }
                let start = out.len();
                wrap_segments(segs(), width, "", "", base, &mut out);
                if *level == 1 {
                    let w = out[start..].iter().map(Line::width).max().unwrap_or(0).max(1);
                    out.push(Line::from(Span::styled("─".repeat(w.min(width)), Style::default().fg(p.ansi_dark_gray))));
                }
            }
            RowKind::Item(item) => {
                let indent = "  ".repeat(row.level);
                let marker = match (&item.task, &item.marker) {
                    (Some(true), _) => "☑ ".to_string(),
                    (Some(false), _) => "☐ ".to_string(),
                    (None, Marker::Bullet(_)) => "• ".to_string(),
                    (None, Marker::Ordered { number, delim }) => format!("{number}{delim} "),
                };
                let first = format!("{indent}{marker}");
                let rest = " ".repeat(UnicodeWidthStr::width(first.as_str()));
                let base = if item.task == Some(true) {
                    Style::default().fg(p.ansi_gray).add_modifier(Modifier::CROSSED_OUT)
                } else {
                    Style::default()
                };
                wrap_segments(segs(), width, &first, &rest, base, &mut out);
            }
            RowKind::Quote => {
                let base = Style::default().fg(p.ansi_gray).add_modifier(Modifier::ITALIC);
                wrap_segments(segs(), width, "│ ", "│ ", base, &mut out);
            }
            // The fences themselves don't show; the code between them does.
            RowKind::Fence { .. } => {}
            RowKind::Code => {
                let text: String = format!("  {}", row.display.replace('\t', "    "));
                let shown = crate::panel::truncate_right(&text, width);
                let pad = width.saturating_sub(UnicodeWidthStr::width(shown.as_str()));
                out.push(Line::from(Span::styled(format!("{shown}{}", " ".repeat(pad)), code_style())));
            }
            RowKind::Rule => out.push(Line::from(Span::styled("─".repeat(width), Style::default().fg(p.ansi_dark_gray)))),
            // A table draws whole at its header line.
            RowKind::Table(t) if t.part == TablePart::Header => {
                let lines: Vec<&shelbi_app::markdown_edit::Row> =
                    doc.rows[ri..].iter().take_while(|r| r.block == row.block && r.table().is_some()).collect();
                let shown: Vec<&shelbi_app::markdown_edit::TableRow> =
                    lines.iter().filter_map(|r| r.table()).filter(|t| t.part != TablePart::Delimiter).collect();
                if !draw_table(&shown, width, &mut out) {
                    // Too narrow for its columns: the source, as written.
                    let base = Style::default().fg(p.ansi_gray);
                    for r in lines {
                        wrap_segments(vec![(r.display.clone(), Style::default())], width, "", "", base, &mut out);
                    }
                }
            }
            RowKind::Table(_) => {}
            RowKind::Raw => {
                let base = Style::default().fg(p.ansi_gray);
                wrap_segments(vec![(row.display.clone(), Style::default())], width, "", "", base, &mut out);
            }
        }
    }
    out
}

/// The widest a word holds a table column open in the TUI (a longer one
/// is cut at the column's edge).
const MAX_WORD: usize = 24;

/// Draw a table's lines (`rows`: its header, then its body) as a box-drawn
/// grid `width` cells wide: the header bold, a rule under every row, each
/// column sized to its content ([`shelbi_app::markdown_edit::column_widths`],
/// as the desktop sizes them) and aligned as its delimiter says, a long cell
/// wrapping inside its column. `false` (nothing drawn) when the columns
/// can't fit three cells each.
fn draw_table(rows: &[&shelbi_app::markdown_edit::TableRow], width: usize, out: &mut Vec<Line<'static>>) -> bool {
    use shelbi_app::markdown_edit::{column_widths, Align};
    let Some(first) = rows.first() else {
        return true;
    };
    let n = first.aligns.len().max(1);
    // Each column's border and a space either side.
    let chrome = 3 * n + 1;
    if width < chrome + 3 * n {
        return false;
    }
    let room = width - chrome;
    let mut want = vec![1.0f32; n];
    let mut least = vec![1.0f32; n];
    for t in rows {
        for (j, c) in t.cells.iter().enumerate().take(n) {
            for line in c.display.split('\n') {
                want[j] = want[j].max(UnicodeWidthStr::width(line) as f32);
                for word in line.split_whitespace() {
                    least[j] = least[j].max(UnicodeWidthStr::width(word).min(MAX_WORD) as f32);
                }
            }
        }
    }
    let mut cols: Vec<usize> = column_widths(&want, &least, room as f32)
        .into_iter()
        .map(|w| (w.floor() as usize).max(1))
        .collect();
    // The words don't fit: take from the widest columns, three cells at the
    // least.
    while cols.iter().sum::<usize>() > room {
        let j = (0..n).max_by_key(|&j| cols[j]).unwrap_or(0);
        if cols[j] <= 3 {
            return false;
        }
        cols[j] -= 1;
    }
    let rule = Style::default().fg(palette().ansi_dark_gray);
    let border = |l: char, m: char, r: char| {
        let mut s = String::new();
        s.push(l);
        for (j, w) in cols.iter().enumerate() {
            if j > 0 {
                s.push(m);
            }
            s.push_str(&"─".repeat(w + 2));
        }
        s.push(r);
        Line::from(Span::styled(s, rule))
    };
    out.push(border('┌', '┬', '┐'));
    for (ri, t) in rows.iter().enumerate() {
        let base = if ri == 0 { Style::default().add_modifier(Modifier::BOLD) } else { Style::default() };
        let cells: Vec<Vec<Line<'static>>> = (0..n)
            .map(|j| {
                let mut lines = Vec::new();
                if let Some(c) = t.cells.get(j) {
                    wrap_segments(styled_segments(&c.display, &c.styles), cols[j], "", "", base, &mut lines);
                }
                lines
            })
            .collect();
        let height = cells.iter().map(Vec::len).max().unwrap_or(0).max(1);
        for k in 0..height {
            let mut spans = vec![Span::styled("│", rule)];
            for (j, w) in cols.iter().enumerate() {
                let align = first.aligns.get(j).copied().unwrap_or_default();
                spans.push(Span::raw(" "));
                let (mut text, used) = fit(cells[j].get(k), *w);
                let pad = w - used;
                let (before, after) = match align {
                    Align::Right => (pad, 0),
                    Align::Center => (pad / 2, pad - pad / 2),
                    Align::None | Align::Left => (0, pad),
                };
                if before > 0 {
                    spans.push(Span::raw(" ".repeat(before)));
                }
                spans.append(&mut text);
                spans.push(Span::raw(" ".repeat(after + 1)));
                spans.push(Span::styled("│", rule));
            }
            out.push(Line::from(spans));
        }
        if ri + 1 < rows.len() {
            out.push(border('├', '┼', '┤'));
        }
    }
    out.push(border('└', '┴', '┘'));
    true
}

/// `line`'s spans cut to `width` cells, and the cells they take.
fn fit(line: Option<&Line<'static>>, width: usize) -> (Vec<Span<'static>>, usize) {
    use unicode_width::UnicodeWidthChar;
    let mut out = Vec::new();
    let mut used = 0;
    for span in line.map_or(&[][..], |l| &l.spans[..]) {
        let mut text = String::new();
        for ch in span.content.chars() {
            let w = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + w > width {
                break;
            }
            used += w;
            text.push(ch);
        }
        if !text.is_empty() {
            out.push(Span::styled(text, span.style));
        }
    }
    (out, used)
}

/// `src` without a leading `---` frontmatter block (YAML), which a reader
/// doesn't need and the block model would take for a rule and a heading.
pub(crate) fn strip_frontmatter(src: &str) -> &str {
    let Some(rest) = src.strip_prefix("---\n").or_else(|| src.strip_prefix("---\r\n")) else {
        return src;
    };
    let mut at = 0;
    for line in rest.split_inclusive('\n') {
        at += line.len();
        if line.trim_end() == "---" {
            return rest[at..].trim_start_matches(['\n', '\r']);
        }
    }
    src
}

/// A row's display text cut into `(text, style)` runs by its inline styles.
fn styled_segments(display: &str, styles: &[(std::ops::Range<usize>, shelbi_app::markdown_edit::Style)]) -> Vec<(String, Style)> {
    let p = palette();
    let mut out = Vec::new();
    let mut at = 0;
    for (range, s) in styles {
        let (start, end) = (range.start.min(display.len()), range.end.min(display.len()));
        if start > at {
            out.push((display[at..start].to_string(), Style::default()));
        }
        if end > start {
            let mut style = if s.code { code_style() } else { Style::default() };
            if s.bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if s.italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if s.strike {
                style = style.add_modifier(Modifier::CROSSED_OUT);
            }
            if s.link {
                style = style.fg(p.ansi_cyan).add_modifier(Modifier::UNDERLINED);
            }
            if s.raw {
                style = style.fg(p.ansi_gray);
            }
            out.push((display[start..end].to_string(), style));
        }
        at = at.max(end);
    }
    if at < display.len() {
        out.push((display[at..].to_string(), Style::default()));
    }
    out
}

/// Word-wrap styled `segs` to `width`, the first line led by `first`, the
/// rest by `rest`, each segment's style over `base`. A `\n` in them (a hard
/// break, a `<br>`) starts a new line.
fn wrap_segments(segs: Vec<(String, Style)>, width: usize, first: &str, rest: &str, base: Style, out: &mut Vec<Line<'static>>) {
    let mut lines: Vec<Vec<(String, Style)>> = vec![Vec::new()];
    for (text, style) in segs {
        for (k, part) in text.split('\n').enumerate() {
            if k > 0 {
                lines.push(Vec::new());
            }
            if !part.is_empty() {
                lines.last_mut().expect("starts with a line").push((part.to_string(), style));
            }
        }
    }
    for (k, line) in lines.into_iter().enumerate() {
        wrap_line(line, width, if k == 0 { first } else { rest }, rest, base, out);
    }
}

/// [`wrap_segments`] for one line of text (no `\n` in it).
fn wrap_line(segs: Vec<(String, Style)>, width: usize, first: &str, rest: &str, base: Style, out: &mut Vec<Line<'static>>) {
    // Words (runs of non-space, possibly across segments) and the spaces
    // between them, each a list of styled pieces.
    let mut words: Vec<(bool, Vec<(String, Style)>)> = Vec::new();
    for (text, style) in segs {
        let style = base.patch(style);
        let mut piece = String::new();
        let mut space = None;
        for ch in text.chars() {
            let is_space = ch == ' ';
            if space != Some(is_space) && !piece.is_empty() {
                push_piece(&mut words, space.unwrap_or(false), std::mem::take(&mut piece), style);
            }
            space = Some(is_space);
            piece.push(ch);
        }
        if !piece.is_empty() {
            push_piece(&mut words, space.unwrap_or(false), piece, style);
        }
    }
    let lead = |n: usize| if n == 0 { first } else { rest };
    let mut line: Vec<Span<'static>> = vec![Span::raw(first.to_string())];
    let mut col = UnicodeWidthStr::width(first);
    let mut count = 0;
    let mut has_text = false;
    for (space, pieces) in words {
        let w: usize = pieces.iter().map(|(t, _)| UnicodeWidthStr::width(t.as_str())).sum();
        if space {
            // A space at a break is dropped.
            if has_text && col + w < width {
                col += w;
                line.extend(pieces.into_iter().map(|(t, s)| Span::styled(t, s)));
            }
            continue;
        }
        if has_text && col + w > width {
            // Trailing spaces at the break go too.
            while line.len() > 1 && line.last().is_some_and(|s| !s.content.is_empty() && s.content.trim().is_empty()) {
                line.pop();
            }
            out.push(Line::from(std::mem::take(&mut line)));
            count += 1;
            line.push(Span::raw(lead(count).to_string()));
            col = UnicodeWidthStr::width(lead(count));
        }
        for (t, s) in pieces {
            col += UnicodeWidthStr::width(t.as_str());
            line.push(Span::styled(t, s));
        }
        has_text = true;
    }
    out.push(Line::from(line));
}

fn push_piece(words: &mut Vec<(bool, Vec<(String, Style)>)>, space: bool, text: String, style: Style) {
    match words.last_mut() {
        Some((was_space, pieces)) if *was_space == space => pieces.push((text, style)),
        _ => words.push((space, vec![(text, style)])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a line back to its plain text for content assertions.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Every span's visible width, summed — must never exceed the pane.
    fn line_width(line: &Line<'_>) -> usize {
        line.spans.iter().map(|s| cell_width(&s.content)).sum()
    }

    /// True if any span in the line carries the inline-code background.
    fn has_code_bg(line: &Line<'_>) -> bool {
        line.spans.iter().any(|s| s.style.bg == Some(palette().code_bg))
    }

    #[test]
    fn plain_text_has_no_code_style() {
        let lines = render_note("just some prose", 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "just some prose");
        assert!(!has_code_bg(&lines[0]));
    }

    #[test]
    fn inline_code_gets_highlight() {
        let lines = render_note("run `cargo test` now", 40);
        assert_eq!(line_text(&lines[0]), "run cargo test now");
        assert!(has_code_bg(&lines[0]));
        // Only the code word carries the background.
        let coded: String = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.bg == Some(palette().code_bg))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(coded, "cargo test");
    }

    #[test]
    fn no_line_exceeds_width() {
        let note = "shelbi's rebase/probe `ignore` the workflow's base_branch \
                    and fall back to the project-level `default_branch`.";
        for width in [12usize, 18, 24, 30, 40] {
            let lines = render_note(note, width);
            for line in &lines {
                assert!(
                    line_width(line) <= width,
                    "line {:?} width {} exceeds {}",
                    line_text(line),
                    line_width(line),
                    width,
                );
            }
        }
    }

    #[test]
    fn code_span_at_wrap_boundary_stays_in_bounds() {
        // A width tuned so `ignore` lands at the right edge of a wrapped
        // line — the original overflow repro.
        let note = "the probe will ignore the `default_branch` here";
        for width in 10..=48 {
            let lines = render_note(note, width);
            for line in &lines {
                assert!(
                    line_width(line) <= width,
                    "width {width}: line {:?} = {} cells",
                    line_text(line),
                    line_width(line),
                );
                // A code span must never be the trailing padding past the
                // content: its glyphs are always within the measured width.
                if has_code_bg(line) {
                    assert!(line_width(line) <= width);
                }
            }
        }
    }

    #[test]
    fn word_longer_than_width_is_hard_split() {
        let lines = render_note("`supercalifragilisticexpialidocious`", 10);
        assert!(lines.len() > 1, "long code word should wrap across lines");
        for line in &lines {
            assert!(line_width(line) <= 10);
            assert!(has_code_bg(line), "each fragment keeps the code tint");
        }
        let joined: String = lines.iter().map(line_text).collect();
        assert_eq!(joined, "supercalifragilisticexpialidocious");
    }

    #[test]
    fn blank_lines_are_preserved() {
        let lines = render_note("first\n\nsecond", 20);
        assert_eq!(lines.len(), 3);
        assert_eq!(line_text(&lines[0]), "first");
        assert_eq!(line_text(&lines[1]), "");
        assert_eq!(line_text(&lines[2]), "second");
    }

    #[test]
    fn double_asterisks_draw_bold_without_their_markers() {
        let lines = render_note("The **Changelog** row and `**raw**` code", 80);
        assert_eq!(line_text(&lines[0]), "The Changelog row and **raw** code");
        let bold: Vec<&str> = lines[0]
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, ["Changelog"]);
        // Bold that spans words wraps like any text, staying bold.
        let lines = render_note("**one two three**", 8);
        assert!(lines.iter().all(|l| l
            .spans
            .iter()
            .filter(|s| !s.content.trim().is_empty())
            .all(|s| s.style.add_modifier.contains(Modifier::BOLD))));
    }

    #[test]
    fn an_unpaired_double_asterisk_is_literal() {
        let lines = render_note("2 ** 3 and **bold** and ** left", 80);
        assert_eq!(line_text(&lines[0]), "2 ** 3 and bold and ** left");
        let lines = render_note("an **open one", 80);
        assert_eq!(line_text(&lines[0]), "an **open one");
    }

    #[test]
    fn unclosed_backtick_is_literal() {
        let lines = render_note("a stray ` backtick", 40);
        assert_eq!(line_text(&lines[0]), "a stray ` backtick");
        assert!(!has_code_bg(&lines[0]));
    }

    #[test]
    fn leading_indent_is_kept() {
        let lines = render_note("    indented `code` line", 40);
        assert!(line_text(&lines[0]).starts_with("    indented"));
    }

    #[test]
    fn zero_width_does_not_panic() {
        let lines = render_note("some `code` text", 0);
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "some code text");
    }

    #[test]
    fn code_spans_render_readable_in_light() {
        use ratatui::widgets::{Paragraph, Widget};
        crate::theme::with_appearance(shelbi_app::theme::Appearance::Light, || {
            let lines = render_note("run `cargo test` and **then** `cargo clippy`", 40);
            let area = ratatui::layout::Rect::new(0, 0, 40, lines.len() as u16);
            let mut buf = ratatui::buffer::Buffer::empty(area);
            Paragraph::new(lines).render(area, &mut buf);
            crate::theme::assert_light_readable("markdown", &buf);
        });
    }

    // ---- whole documents -------------------------------------------------

    fn doc_text(src: &str, width: usize) -> Vec<String> {
        render_document(src, width).iter().map(line_text).collect()
    }

    #[test]
    fn a_document_draws_its_blocks() {
        let src = "---\ntags: [a]\n---\n# Plan\n\nSome **bold** text.\n\n## Goals\n\n- [ ] ship it\n- [x] plan it\n- a point\n\n1. first\n\n> quoted\n\n```rust\nfn main() {}\n```\n\n---\n";
        let lines = doc_text(src, 30);
        assert_eq!(lines[0], "Plan");
        assert_eq!(lines[1], "────");
        assert!(lines.contains(&"Some bold text.".to_string()), "{lines:?}");
        assert!(lines.contains(&"Goals".to_string()));
        assert!(lines.contains(&"☐ ship it".to_string()));
        assert!(lines.contains(&"☑ plan it".to_string()));
        assert!(lines.contains(&"• a point".to_string()));
        assert!(lines.contains(&"1. first".to_string()));
        assert!(lines.contains(&"│ quoted".to_string()));
        assert!(lines.iter().any(|l| l.starts_with("  fn main() {}")), "{lines:?}");
        assert!(!lines.iter().any(|l| l.contains("```") || l.contains("tags")), "{lines:?}");
        assert_eq!(lines.last().unwrap(), &"─".repeat(30));
        // Bold stays bold, its asterisks gone.
        let rendered = render_document("Some **bold** text.", 30);
        let bold = rendered[0].spans.iter().find(|s| s.content == "bold").unwrap();
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn document_lines_wrap_under_their_marker() {
        let lines = doc_text("- one two three four five six", 14);
        assert_eq!(lines, ["• one two", "  three four", "  five six"]);
        for l in render_document("A long paragraph that has to wrap somewhere", 12) {
            assert!(l.width() <= 12, "{:?}", line_text(&l));
        }
    }

    #[test]
    fn br_tags_break_lines_and_space_blocks() {
        // A line of only `<br />` is a blank line; one inside a paragraph
        // breaks it, the next line under the item's marker.
        let lines = doc_text("Tagline here.\n\n<br />\n\nNext\n\n- one<br/>two", 30);
        assert_eq!(lines, ["Tagline here.", "", "", "", "Next", "", "• one", "  two"]);
        assert!(!doc_text("a<br>b\\\nc", 30).iter().any(|l| l.contains('\n')));
    }

    #[test]
    fn a_table_draws_as_a_grid_with_wrapped_aligned_cells() {
        let src = "Intro.\n\n| Element | Shelbi | Since |\n| :--- | :---: | ---: |\n| **Use case** | Shipping code changes, large and small, with multiple coding agents | `v1` |\n| Tool | Tabs<br>one each | 9 |\n\nAfter.";
        let lines = doc_text(src, 50);
        let grid: Vec<&String> = lines.iter().filter(|l| l.starts_with(['┌', '│', '├', '└'])).collect();
        assert!(grid.first().unwrap().starts_with('┌'), "{lines:#?}");
        assert!(grid.last().unwrap().starts_with('└'), "{lines:#?}");
        // Every line of the grid is as wide as the others, inside the pane.
        let w = UnicodeWidthStr::width(grid[0].as_str());
        assert!(w <= 50, "{lines:#?}");
        for l in &grid {
            assert_eq!(UnicodeWidthStr::width(l.as_str()), w, "{l:?} in {lines:#?}");
        }
        // The header, the cells' Markdown drawn (no `**`, no `<br>`), the
        // long cell wrapped in its column, the right column right-aligned.
        assert!(grid[1].contains("Element") && grid[1].contains("Shelbi"), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.contains("**") || l.contains("<br>") || l.contains("---")), "{lines:#?}");
        let use_case = grid.iter().position(|l| l.contains("Use case")).unwrap();
        assert!(grid[use_case + 1].starts_with('│'), "wrapped: {lines:#?}");
        assert!(grid[use_case].contains(" v1 │"), "{lines:#?}");
        assert!(grid.iter().any(|l| l.contains("one each")), "{lines:#?}");
        // Row rules between the rows.
        assert_eq!(grid.iter().filter(|l| l.starts_with('├')).count(), 2, "{lines:#?}");
        // The header is bold.
        let rendered = render_document(src, 50);
        let header = rendered.iter().find(|l| line_text(l).contains("Element")).unwrap();
        assert!(header
            .spans
            .iter()
            .any(|s| s.content.contains("Element") && s.style.add_modifier.contains(Modifier::BOLD)));
    }

    #[test]
    fn a_table_too_narrow_for_its_columns_shows_its_source() {
        let src = "| a | b | c | d | e |\n|---|---|---|---|---|\n| 1 | 2 | 3 | 4 | 5 |";
        let lines = doc_text(src, 20);
        assert!(lines[0].starts_with("| a | b |"), "{lines:#?}");
        assert!(lines.iter().any(|l| l.contains("|---|")), "{lines:#?}");
        assert!(!lines.iter().any(|l| l.starts_with('┌')), "{lines:#?}");
    }

    #[test]
    fn frontmatter_is_left_out_only_when_closed() {
        assert_eq!(strip_frontmatter("---\na: 1\n---\n\n# T\n"), "# T\n");
        assert_eq!(strip_frontmatter("---\nnot closed\n"), "---\nnot closed\n");
        assert_eq!(strip_frontmatter("# T\n"), "# T\n");
    }
}
