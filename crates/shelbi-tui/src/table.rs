//! The settings screens' tables, drawn as real tables: a header row over a
//! rule, a `│` between every cell, and the lit row as a half-block pill.
//!
//! ```text
//!    Workspace   │ Machine  │ Tags    │ Slot
//!   ─────────────┼──────────┼─────────┼────────
//!    alpha       │ This Mac │ No tags │ 0 auto
//!  ▐ bravo       │ gpu      │ cuda    │ 1      ▌   <- selected or hovered
//!   ~/wt/bravo   │ SSH      │         │        ▌
//! ```
//!
//! A lit row (the selection, or the row under the pointer) takes a fill
//! across its whole width, capped by `▐` and `▌` whose foreground is the
//! fill over the terminal's default background, so the bar reads as a
//! rounded pill: the sidebar's half-block selection, turned on its side.
//! The selection's fill is [`Theme::selection`]; the pointer's is the
//! lighter [`Theme::row_hover`], and the selection wins on a row that is
//! both. The rules are [`Theme::table_rule`].
//!
//! Every line is the table's width plus [`BLEED`] columns each side: the cap
//! and a column of fill. A page draws table lines [`BLEED`] columns out into
//! its gutter, so the cells' text lines up with the page's other text.
//!
//! Columns size to their content (the header included) between a minimum
//! and a maximum; a `fill` column takes what's left of the width. When the
//! width can't hold every column at its minimum, the columns with the
//! highest drop rank go first; then the widest shrink, their text cut with
//! `…`. Nothing wraps.
//!
//! The pages lay their rows out as lines, so the table hands back lines,
//! each with the row it belongs to (for the page's click targets) and the
//! line as drawn under the pointer (the page picks it at draw time, when it
//! knows where each line landed).
//!
//! [`Theme::selection`]: shelbi_app::theme::Theme::selection
//! [`Theme::row_hover`]: shelbi_app::theme::Theme::row_hover
//! [`Theme::table_rule`]: shelbi_app::theme::Theme::table_rule

use ratatui::{
    style::{Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

use crate::panel;
use crate::theme::palette;

/// Columns a line reaches past the table's width on each side: the cap,
/// then a column of fill.
pub(crate) const BLEED: usize = 2;

/// The right cap's glyph and the left's: half blocks toward the row.
const CAP_LEFT: &str = "▐";
const CAP_RIGHT: &str = "▌";

/// Columns between two cells: a space, the rule, a space.
const GAP: usize = 3;

/// One column: its header, how wide it may be, and when it goes.
#[derive(Debug, Clone)]
pub(crate) struct Column {
    title: String,
    title_style: Option<Style>,
    min: usize,
    max: usize,
    fill: bool,
    drop: u8,
}

impl Column {
    /// A column headed `title`, sized to its content up to 40 columns and
    /// never dropped.
    pub(crate) fn new(title: impl Into<String>) -> Column {
        let title = title.into();
        let min = panel::display_width(&title).clamp(1, 8);
        Column { title, title_style: None, min, max: 40, fill: false, drop: 0 }
    }

    /// The narrowest it shrinks to before a column is dropped.
    pub(crate) fn min(mut self, min: usize) -> Column {
        self.min = min.max(1);
        self.max = self.max.max(self.min);
        self
    }

    /// The widest its content makes it.
    pub(crate) fn max(mut self, max: usize) -> Column {
        self.max = max.max(self.min);
        self
    }

    /// It takes the width the other columns leave.
    pub(crate) fn fill(mut self) -> Column {
        self.fill = true;
        self
    }

    /// It goes when the width can't hold every column: the highest rank
    /// first (0, the default, never goes).
    pub(crate) fn drop(mut self, rank: u8) -> Column {
        self.drop = rank;
        self
    }

    /// The header drawn in `style` rather than the muted heading.
    pub(crate) fn title_style(mut self, style: Style) -> Column {
        self.title_style = Some(style);
        self
    }
}

/// One cell: its lines of styled text, each cut to the column.
#[derive(Debug, Clone, Default)]
pub(crate) struct Cell {
    lines: Vec<Vec<Span<'static>>>,
    /// Lines (by index) cut from the left, keeping their end (a path).
    keep_end: Vec<usize>,
}

impl Cell {
    /// A cell with nothing in it.
    pub(crate) fn empty() -> Cell {
        Cell::default()
    }

    /// One line of `text` in `style`.
    pub(crate) fn text(text: impl Into<String>, style: Style) -> Cell {
        Cell::spans(vec![Span::styled(text.into(), style)])
    }

    /// One line of `text` that, cut, keeps its end (`…/wt/alpha`).
    pub(crate) fn end(text: impl Into<String>, style: Style) -> Cell {
        Cell { keep_end: vec![0], ..Cell::text(text, style) }
    }

    /// One line of styled spans.
    pub(crate) fn spans(spans: Vec<Span<'static>>) -> Cell {
        Cell { lines: vec![spans], keep_end: Vec::new() }
    }

    /// Another line of `text` in `style` under what's there.
    pub(crate) fn and(self, text: impl Into<String>, style: Style) -> Cell {
        self.and_spans(vec![Span::styled(text.into(), style)])
    }

    /// Another line of spans under what's there.
    pub(crate) fn and_spans(mut self, spans: Vec<Span<'static>>) -> Cell {
        self.lines.push(spans);
        self
    }

    /// Another line of `text` that, cut, keeps its end (`…/wt/alpha`).
    pub(crate) fn and_end(mut self, text: impl Into<String>, style: Style) -> Cell {
        self.keep_end.push(self.lines.len());
        self.and(text, style)
    }

    fn width(&self) -> usize {
        self.lines.iter().map(|l| spans_width(l)).max().unwrap_or(0)
    }
}

/// One drawn line of a table.
#[derive(Debug, Clone)]
pub(crate) struct TableLine {
    pub line: Line<'static>,
    /// The body row it's a line of (`None`: the header, its rule, or the
    /// empty table's note).
    pub row: Option<usize>,
    /// The line as drawn while the pointer is on its row; `None` on the
    /// selected row (the selection wins) and on lines that aren't a row's.
    pub hover: Option<Line<'static>>,
}

/// How a row is lit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lit {
    None,
    Hover,
    /// Selected; `true` while its list has the keyboard.
    Selected(bool),
}

/// A table: its columns, its rows, and which one is selected.
#[derive(Debug, Clone, Default)]
pub(crate) struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Cell>>,
    selected: Option<usize>,
    focused: bool,
    empty: Option<String>,
}

impl Table {
    pub(crate) fn new(columns: Vec<Column>) -> Table {
        Table { columns, ..Table::default() }
    }

    /// Add a row: a cell per column (missing ones are empty).
    pub(crate) fn push(&mut self, cells: Vec<Cell>) {
        self.rows.push(cells);
    }

    /// Row `row` is selected; `focused` while its list has the keyboard.
    pub(crate) fn selected(mut self, row: Option<usize>, focused: bool) -> Table {
        self.selected = row;
        self.focused = focused;
        self
    }

    /// What a table without rows says, centered under its header.
    pub(crate) fn empty(mut self, text: impl Into<String>) -> Table {
        self.empty = Some(text.into());
        self
    }

    /// Each column's width at `width` (the table's, without the bleed), or
    /// `None` for a column dropped for room.
    pub(crate) fn layout(&self, width: usize) -> Vec<Option<usize>> {
        let natural: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .map(|(c, col)| {
                let content = self.rows.iter().filter_map(|r| r.get(c)).map(Cell::width).max().unwrap_or(0);
                content.max(panel::display_width(&col.title)).clamp(col.min, col.max)
            })
            .collect();
        let mut shown: Vec<usize> = (0..self.columns.len()).collect();
        let need = |shown: &[usize]| -> usize {
            shown.iter().map(|&c| self.columns[c].min).sum::<usize>() + GAP * shown.len().saturating_sub(1)
        };
        while need(&shown) > width {
            // The highest rank goes first; of equals, the rightmost.
            let Some(at) = shown
                .iter()
                .enumerate()
                .filter(|(_, &c)| self.columns[c].drop > 0)
                .max_by_key(|(i, &c)| (self.columns[c].drop, *i))
                .map(|(i, _)| i)
            else {
                break;
            };
            shown.remove(at);
        }
        let mut widths: Vec<usize> = shown.iter().map(|&c| natural[c]).collect();
        let gaps = GAP * shown.len().saturating_sub(1);
        let total = |w: &[usize]| w.iter().sum::<usize>() + gaps;
        // Too wide: a fill column gives first (it's the one that stretches),
        // then the column with the most room above its minimum, a column at
        // a time; past every minimum, the widest does.
        while total(&widths) > width {
            let slack = |i: usize| widths[i] - self.columns[shown[i]].min;
            let over_min = (0..widths.len())
                .filter(|&i| widths[i] > self.columns[shown[i]].min)
                .max_by_key(|&i| (self.columns[shown[i]].fill, slack(i), i));
            let widest = (0..widths.len()).filter(|&i| widths[i] > 1).max_by_key(|&i| (widths[i], i));
            match over_min.or(widest) {
                Some(i) => widths[i] -= 1,
                None => break,
            }
        }
        // Room to spare: the fill columns share it (the last column takes
        // it when none fills), so the table spans the width.
        let spare = width.saturating_sub(total(&widths));
        if spare > 0 && !widths.is_empty() {
            let fills: Vec<usize> = (0..widths.len()).filter(|&i| self.columns[shown[i]].fill).collect();
            let takers = if fills.is_empty() { vec![widths.len() - 1] } else { fills };
            let each = spare / takers.len();
            for (n, &i) in takers.iter().enumerate() {
                widths[i] += each + usize::from(n < spare % takers.len());
            }
        }
        let mut out = vec![None; self.columns.len()];
        for (i, &c) in shown.iter().enumerate() {
            out[c] = Some(widths[i]);
        }
        out
    }

    /// Where column `column`'s text starts at `width`, from the table's
    /// left edge (without the bleed); `None` when it's dropped.
    pub(crate) fn column_x(&self, width: usize, column: usize) -> Option<usize> {
        let layout = self.layout(width);
        layout[column]?;
        Some(layout[..column].iter().flatten().map(|w| w + GAP).sum())
    }

    /// The table's lines at `width` (each `width + 2 * BLEED` wide): the
    /// header, its rule, then each row's lines (or the empty note).
    pub(crate) fn lines(&self, width: usize) -> Vec<TableLine> {
        let layout = self.layout(width);
        let shown: Vec<(usize, usize)> =
            layout.iter().enumerate().filter_map(|(c, w)| w.map(|w| (c, w))).collect();
        let mut out = Vec::new();
        if shown.is_empty() {
            return out;
        }
        out.push(TableLine { line: self.header(&shown), row: None, hover: None });
        out.push(TableLine { line: rule(&shown), row: None, hover: None });
        if self.rows.is_empty() {
            if let Some(text) = &self.empty {
                let text = panel::truncate_right(text, width);
                let left = width.saturating_sub(panel::display_width(&text)) / 2;
                out.push(TableLine {
                    line: Line::from(vec![
                        Span::raw(" ".repeat(BLEED + left)),
                        Span::styled(text, Style::default().fg(palette().muted)),
                    ]),
                    row: None,
                    hover: None,
                });
            }
            return out;
        }
        for (r, cells) in self.rows.iter().enumerate() {
            let height = cells.iter().map(|c| c.lines.len()).max().unwrap_or(0).max(1);
            let lit = match self.selected {
                Some(s) if s == r => Lit::Selected(self.focused),
                _ => Lit::None,
            };
            for n in 0..height {
                let hover = (lit == Lit::None).then(|| self.row_line(cells, n, &shown, Lit::Hover));
                out.push(TableLine { line: self.row_line(cells, n, &shown, lit), row: Some(r), hover });
            }
        }
        out
    }

    fn header(&self, shown: &[(usize, usize)]) -> Line<'static> {
        let heading = Style::default().fg(palette().muted).add_modifier(Modifier::BOLD);
        let mut spans = vec![Span::raw(" ".repeat(BLEED))];
        for (i, &(c, w)) in shown.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" "));
                spans.push(Span::styled("│", rule_style()));
                spans.push(Span::raw(" "));
            }
            let col = &self.columns[c];
            let style = col.title_style.unwrap_or(heading);
            let title = fit(&[Span::styled(col.title.clone(), style)], w);
            let used = spans_width(&title);
            spans.extend(title);
            spans.push(Span::raw(" ".repeat(w - used)));
        }
        Line::from(spans)
    }

    /// Line `n` of a row's cells, lit as `lit`.
    fn row_line(&self, cells: &[Cell], n: usize, shown: &[(usize, usize)], lit: Lit) -> Line<'static> {
        let fill = match lit {
            Lit::None => None,
            Lit::Hover => Some(palette().row_hover),
            Lit::Selected(_) => Some(palette().selection),
        };
        let on = |s: Style| match fill {
            Some(bg) if s.bg.is_none() => s.bg(bg),
            _ => s,
        };
        let blank = on(Style::default());
        let cap = |glyph: &'static str| match fill {
            Some(bg) => Span::styled(glyph, Style::default().fg(bg)),
            None => Span::raw(" "),
        };
        let mut spans = vec![cap(CAP_LEFT), Span::styled(" ", blank)];
        for (i, &(c, w)) in shown.iter().enumerate() {
            if i > 0 {
                spans.push(Span::styled(" ", blank));
                spans.push(Span::styled("│", on(rule_style())));
                spans.push(Span::styled(" ", blank));
            }
            let cell = cells.get(c);
            let line = cell.and_then(|cell| cell.lines.get(n)).cloned().unwrap_or_default();
            let keep_end = cell.is_some_and(|cell| cell.keep_end.contains(&n));
            let line: Vec<Span<'static>> = line
                .into_iter()
                .map(|s| {
                    let style = match lit {
                        Lit::Selected(focused) => selected(s.style, focused && i == 0 && n == 0),
                        _ => s.style,
                    };
                    Span::styled(s.content, on(style))
                })
                .collect();
            let fitted = if keep_end { fit_end(&line, w) } else { fit(&line, w) };
            let used = spans_width(&fitted);
            spans.extend(fitted);
            if used < w {
                spans.push(Span::styled(" ".repeat(w - used), blank));
            }
        }
        spans.push(Span::styled(" ", blank));
        spans.push(cap(CAP_RIGHT));
        Line::from(spans)
    }
}

/// The rule under the header: `─` across each column and its padding, `┼`
/// where a `│` falls.
fn rule(shown: &[(usize, usize)]) -> Line<'static> {
    let mut text = String::new();
    for (i, &(_, w)) in shown.iter().enumerate() {
        if i > 0 {
            text.push('┼');
        }
        text.push_str(&"─".repeat(w + 2));
    }
    Line::from(vec![Span::raw(" "), Span::styled(text, rule_style())])
}

fn rule_style() -> Style {
    Style::default().fg(palette().table_rule)
}

/// A span's style on the selection: muted text lifted to gray (muted
/// doesn't read on the fill), and the row's first text loud and bold while
/// its list has the keyboard. A span with its own fill (a badge, a picked
/// chord) keeps it.
fn selected(style: Style, loud: bool) -> Style {
    if style.bg.is_some() {
        return style;
    }
    let quiet = [palette().muted, palette().ansi_dark_gray];
    let style = match style.fg {
        Some(fg) if quiet.contains(&fg) => style.fg(palette().gray),
        _ => style,
    };
    if loud {
        let plain = [None, Some(palette().foreground), Some(palette().gray), Some(palette().white)];
        let style = if plain.contains(&style.fg) { style.fg(palette().white) } else { style };
        style.add_modifier(Modifier::BOLD)
    } else {
        style
    }
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(|s| panel::display_width(&s.content)).sum()
}

/// `spans` cut to `width` columns, `…` ending a cut line.
fn fit(spans: &[Span<'static>], width: usize) -> Vec<Span<'static>> {
    if spans_width(spans) <= width {
        return spans.to_vec();
    }
    if width == 0 {
        return Vec::new();
    }
    // Room for the ellipsis, in the style of the span it cuts.
    let budget = width - 1;
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let mut text = String::new();
        let mut cut = false;
        for ch in s.content.chars() {
            let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
            if used + cw > budget {
                cut = true;
                break;
            }
            text.push(ch);
            used += cw;
        }
        if cut {
            text.push('…');
            out.push(Span::styled(text, s.style));
            return out;
        }
        out.push(Span::styled(text, s.style));
    }
    out
}

/// `spans` cut to `width` from the left, `…` starting a cut line, in the
/// first span's style.
fn fit_end(spans: &[Span<'static>], width: usize) -> Vec<Span<'static>> {
    if spans_width(spans) <= width {
        return spans.to_vec();
    }
    if width == 0 {
        return Vec::new();
    }
    let text: String = spans.iter().map(|s| s.content.as_ref()).collect();
    let style = spans.first().map(|s| s.style).unwrap_or_default();
    let mut tail = Vec::new();
    let mut used = 0;
    for ch in text.chars().rev() {
        let cw = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + cw > width - 1 {
            break;
        }
        tail.push(ch);
        used += cw;
    }
    let tail: String = tail.into_iter().rev().collect();
    vec![Span::styled(format!("…{tail}"), style)]
}

/// The fill a drawn table line shows at column `x` (for tests): its span's
/// background there.
#[cfg(test)]
pub(crate) fn bg_at(line: &Line<'_>, x: usize) -> Option<ratatui::style::Color> {
    let mut at = 0;
    for s in &line.spans {
        let w = panel::display_width(&s.content);
        if x < at + w {
            return s.style.bg;
        }
        at += w;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()
    }

    fn sample() -> Table {
        let mut t = Table::new(vec![
            Column::new("Name").fill(),
            Column::new("Machine").drop(1),
            Column::new("State").drop(2),
        ]);
        for (name, machine, state) in [("alpha", "hub", "idle"), ("bravo", "hub", "working"), ("charlie", "gpu", "idle")] {
            t.push(vec![
                Cell::text(name, Style::default().fg(palette().white)),
                Cell::text(machine, Style::default().fg(palette().foreground)),
                Cell::text(state, Style::default().fg(palette().muted)),
            ]);
        }
        t
    }

    #[test]
    fn a_header_over_a_rule_and_a_rule_between_every_cell() {
        let lines = sample().lines(30);
        let shown: Vec<String> = lines.iter().map(|l| text(&l.line)).collect();
        assert_eq!(shown[0], "  Name       │ Machine │ State  ");
        assert_eq!(shown[1], " ────────────┼─────────┼─────────");
        assert_eq!(shown[2], "  alpha      │ hub     │ idle     ");
        // Every line is the width and the bleed each side (the header's and
        // the rule's trailing cap columns stay unpainted).
        for l in &lines[2..] {
            assert_eq!(panel::display_width(&text(&l.line)), 30 + 2 * BLEED);
        }
        assert!(lines[2..].iter().all(|l| l.row.is_some()) && lines[..2].iter().all(|l| l.row.is_none()));
        // The rules are the palette's quiet table rule; the header muted.
        let bar = lines[2].line.spans.iter().find(|s| s.content == "│").unwrap();
        assert_eq!(bar.style.fg, Some(palette().table_rule));
        assert_eq!(lines[0].line.spans[1].style.fg, Some(palette().muted));
    }

    #[test]
    fn the_selected_row_is_a_pill_with_half_block_caps() {
        let lines = sample().selected(Some(1), true).lines(30);
        let row = &lines[3].line;
        let s = text(row);
        assert!(s.starts_with("▐ bravo"), "{s:?}");
        assert!(s.ends_with("working ▌"), "{s:?}");
        // The caps carry the fill as their foreground over the default
        // background; everything between is on the fill.
        let first = &row.spans[0];
        assert_eq!((first.style.fg, first.style.bg), (Some(palette().selection), None));
        let last = row.spans.last().unwrap();
        assert_eq!((last.style.fg, last.style.bg), (Some(palette().selection), None));
        for x in 1..30 + 2 * BLEED - 1 {
            assert_eq!(bg_at(row, x), Some(palette().selection), "column {x} is on the fill");
        }
        // Focused: the name is loud; muted text lifts to gray on the fill.
        let name = row.spans.iter().find(|s| s.content == "bravo").unwrap();
        assert!(name.style.add_modifier.contains(Modifier::BOLD));
        let state = row.spans.iter().find(|s| s.content == "working").unwrap();
        assert_eq!(state.style.fg, Some(palette().gray));
        // The selection has no hover look; the others do, on the hover fill.
        assert!(lines[3].hover.is_none());
        let hover = lines[2].hover.as_ref().unwrap();
        assert!(text(hover).starts_with("▐ alpha"));
        assert_eq!(hover.spans[0].style.fg, Some(palette().row_hover));
        assert_eq!(bg_at(hover, 5), Some(palette().row_hover));
        // Unlit rows have blank caps and no fill.
        assert_eq!(bg_at(&lines[2].line, 5), None);
        assert!(text(&lines[2].line).starts_with("  alpha"));
    }

    #[test]
    fn a_tall_cell_makes_every_line_of_its_row_lit() {
        let mut t = Table::new(vec![Column::new("Workspace").fill(), Column::new("Slot")]);
        t.push(vec![Cell::text("alpha", Style::default()).and("~/wt/alpha", Style::default()), Cell::text("0", Style::default())]);
        t.push(vec![Cell::text("bravo", Style::default()), Cell::text("1", Style::default())]);
        let lines = t.selected(Some(0), false).lines(24);
        assert_eq!(lines.len(), 2 + 3);
        assert_eq!((lines[2].row, lines[3].row, lines[4].row), (Some(0), Some(0), Some(1)));
        for l in &lines[2..4] {
            let s = text(&l.line);
            assert!(s.starts_with('▐') && s.ends_with('▌'), "{s:?}");
        }
        assert!(text(&lines[3].line).contains("~/wt/alpha"));
        // The second line still draws the rule in the empty Slot cell.
        assert!(text(&lines[3].line).contains('│'));
    }

    #[test]
    fn narrow_widths_cut_with_an_ellipsis_then_drop_columns() {
        let mut t = Table::new(vec![
            Column::new("Name").min(4).fill(),
            Column::new("Machine").min(4).drop(1),
            Column::new("State").min(4).drop(2),
        ]);
        t.push(vec![
            Cell::text("a-very-long-workspace-name", Style::default()),
            Cell::text("gpu-box-in-the-closet", Style::default()),
            Cell::text("working", Style::default()),
        ]);
        // Room for all three at their minimums: cut, not dropped.
        let layout = t.layout(20);
        assert!(layout.iter().all(Option::is_some), "{layout:?}");
        let row = text(&t.lines(20)[2].line);
        assert!(row.contains('…'), "{row:?}");
        assert_eq!(panel::display_width(&row), 20 + 2 * BLEED);
        // Less: State (rank 2) goes first, then Machine; the fill column
        // gives its width first.
        assert_eq!(t.layout(12), vec![Some(4), Some(5), None]);
        assert_eq!(t.layout(6), vec![Some(6), None, None]);
        // A header cut too.
        assert!(text(&t.lines(6)[0].line).contains("Name"));
    }

    #[test]
    fn spare_width_goes_to_the_fill_column() {
        let t = sample();
        let layout = t.layout(60);
        assert_eq!(layout[1], Some(7));
        assert_eq!(layout[2], Some(7));
        assert_eq!(layout[0], Some(60 - 7 - 7 - 2 * GAP));
        assert_eq!(t.column_x(60, 2), Some(60 - 7));
        assert_eq!(t.column_x(6, 2), None);
    }

    #[test]
    fn an_empty_table_centers_its_note_under_the_header() {
        let t = Table::new(vec![Column::new("Workspace").fill(), Column::new("Slot")]).empty("No workspaces yet.");
        let lines = t.lines(40);
        assert_eq!(lines.len(), 3);
        let note = text(&lines[2].line);
        assert_eq!(note.trim(), "No workspaces yet.");
        let left = note.len() - note.trim_start().len() - BLEED;
        assert_eq!(left, (40 - "No workspaces yet.".len()) / 2);
        assert_eq!(lines[2].line.spans[1].style.fg, Some(palette().muted));
    }

    #[test]
    fn a_badge_keeps_its_own_fill_on_the_selection() {
        let badge = Style::default().fg(palette().workflow_badge_fg).bg(palette().workflow_badge_bg);
        let mut t = Table::new(vec![Column::new("Workflow").fill()]);
        t.push(vec![Cell::spans(vec![Span::raw("task "), Span::styled(" Default ", badge)])]);
        let lines = t.selected(Some(0), true).lines(30);
        let b = lines[2].line.spans.iter().find(|s| s.content == " Default ").unwrap();
        assert_eq!(b.style.bg, Some(palette().workflow_badge_bg));
    }

    #[test]
    fn tiny_widths_never_panic() {
        let t = sample().selected(Some(0), true).empty("No rows");
        for w in 0..12 {
            for l in t.lines(w) {
                assert!(panel::display_width(&text(&l.line)) <= w.max(1) + 2 * BLEED, "{w}: {:?}", text(&l.line));
            }
        }
        let none = Table::new(Vec::new());
        assert!(none.lines(10).is_empty());
    }
}
