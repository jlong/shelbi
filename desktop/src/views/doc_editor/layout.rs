//! Where a [`Doc`]'s rows land: each row's shaped lines, gutter and block
//! chrome at a width, and the geometry the editor asks of them (where a
//! caret goes, what a point hits, the visual line above or below).
//!
//! Offsets here are display offsets of a row unless named `src`; positions
//! are relative to the editor element's origin.
//!
//! A fenced code block's lines take their syntax colors ([`Code::spans`],
//! the diff view's classes and colors, `theme::diff::syntax_color`), and its
//! fence rows are left out while the caret is elsewhere ([`Code::hidden`]):
//! a hidden row takes no room and no visual line, so the caret and the
//! pointer can't land in it, and the block's padding wraps what's shown.
//!
//! A table drawn as a grid ([`Code::grid`]) lays its lines out as rows of
//! cells ([`TableLayout`], [`CellLayout`]): each column as wide as its
//! content wants while the table fits, its cells wrapping inside their
//! column when it doesn't, and the grid scrolling sideways inside its own box
//! when even its narrowest columns are wider than the editor. A grid row is
//! one visual line as tall as the row, so a click or a vertical move lands in
//! it and the source offset it picks is in the cell under the point
//! ([`DocLayout::source_at`]).

use std::collections::HashMap;
use std::ops::RangeInclusive;

use gpui::{
    font, point, px, size, App, Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Point, SharedString,
    StrikethroughStyle, TextAlign, TextRun, UnderlineStyle, Window, WrappedLine,
};
use shelbi_app::diff::highlight::{SyntaxClass, SyntaxSpan};
use shelbi_app::markdown_edit::{column_widths, Align, Doc, Gap, Marker, Row, RowKind, Style, TablePart};

use crate::elements::terminal::metrics::PRIMARY_FAMILY;
use crate::theme::diff::syntax_color;
use crate::theme::style::{Type, UI_FAMILY};
use crate::theme::Theme;

/// A list's indent per level, and the gutter its marker (bullet, number,
/// checkbox) sits in: the frame's 14pt check and 8pt before the text.
pub const INDENT: f32 = 22.0;
/// A code block's inset around its text.
const CODE_PAD: f32 = 8.0;
const CODE_INSET: f32 = 12.0;
/// A quote's bar and the room after it.
const QUOTE_INSET: f32 = 14.0;
/// The check's edge.
pub const CHECK: f32 = 14.0;
/// A table cell's padding.
const CELL_PAD_X: f32 = 10.0;
const CELL_PAD_Y: f32 = 6.0;
/// The narrowest a table column gets, and the widest its longest word can
/// hold it open (a longer word breaks).
const MIN_COLUMN: f32 = 40.0;
const MAX_WORD: f32 = 220.0;
/// The room under a table that scrolls sideways, for its scroll bar.
const SCROLL_ROOM: f32 = 8.0;
/// A grid's corners.
pub const TABLE_RADIUS: f32 = 6.0;

/// Which field the editor is: the title's one line, or the body.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Body,
}

/// A row's marker in its gutter.
#[derive(Debug, Clone)]
pub enum Gutter {
    Bullet,
    Number(String),
    Check(bool),
}

/// How a doc's code and tables show: one entry per row in each (empty for
/// none).
#[derive(Debug, Clone, Copy, Default)]
pub struct Code<'a> {
    /// Rows left out (a block's fences, a grid's delimiter line, the caret
    /// elsewhere).
    pub hidden: &'a [bool],
    /// Each row's syntax spans, in display offsets.
    pub spans: &'a [Vec<SyntaxSpan>],
    /// Rows drawn as a table's grid rather than as their source.
    pub grid: &'a [bool],
    /// Each table's sideways scroll, by its block.
    pub scroll: Option<&'a HashMap<usize, Pixels>>,
}

impl Code<'_> {
    fn hidden(&self, row: usize) -> bool {
        self.hidden.get(row).copied().unwrap_or(false)
    }

    fn grid(&self, row: usize) -> bool {
        self.grid.get(row).copied().unwrap_or(false)
    }

    fn spans(&self, row: usize) -> &[SyntaxSpan] {
        self.spans.get(row).map_or(&[], Vec::as_slice)
    }
}

pub struct RowLayout {
    /// The top of the row's text.
    pub top: Pixels,
    /// The left edge of its text.
    pub left: Pixels,
    pub line_height: Pixels,
    /// Each `\n`-separated line of the display: where it starts, and its
    /// shaping.
    pub lines: Vec<(usize, WrappedLine)>,
    pub gutter: Option<(Gutter, Bounds<Pixels>)>,
    /// Left out: no lines, no height, no visual line.
    pub hidden: bool,
    /// A grid row's cells (its `lines` are empty, its `line_height` the
    /// row's height).
    pub cells: Vec<CellLayout>,
    /// The table ([`DocLayout::tables`]) a grid row is in.
    pub table: Option<usize>,
}

/// A table cell drawn in its grid.
pub struct CellLayout {
    /// The cell, padding and all (the table's scroll applied).
    pub cell: Bounds<Pixels>,
    /// Its text's box: the cell less its padding.
    pub text: Bounds<Pixels>,
    /// Each `\n`-separated line of the cell's display: where it starts, and
    /// its shaping (wrapped to the column).
    pub lines: Vec<(usize, WrappedLine)>,
    pub line_height: Pixels,
    pub align: TextAlign,
}

/// A table drawn as a grid.
pub struct TableLayout {
    pub block: usize,
    /// The box it shows in: the grid's width, or the editor's when the grid
    /// is wider (it scrolls inside it), and the scroll bar's room.
    pub bounds: Bounds<Pixels>,
    /// The grid's outline (the scroll applied).
    pub grid: Bounds<Pixels>,
    /// The rules between its rows and its columns.
    pub rules: Vec<Bounds<Pixels>>,
    /// How far it can scroll (its grid's width past the box's).
    pub max_scroll: Pixels,
    /// The scroll bar's thumb, for a grid wider than its box.
    pub thumb: Option<Bounds<Pixels>>,
}

pub struct DocLayout {
    pub rows: Vec<RowLayout>,
    pub height: Pixels,
    /// Code (and raw source) blocks' backgrounds.
    pub blocks: Vec<Bounds<Pixels>>,
    /// Quote bars.
    pub bars: Vec<Bounds<Pixels>>,
    /// Thematic breaks.
    pub rules: Vec<Bounds<Pixels>>,
    /// Tables drawn as grids.
    pub tables: Vec<TableLayout>,
}

/// How a row's text is set: its font, size, line height and color.
struct Face {
    family: &'static str,
    size: f32,
    line_height: f32,
    weight: FontWeight,
    color: Hsla,
}

/// `row`'s face at `scale` (the body's size over [`Type::Base`]'s): every
/// size and line height in proportion, the line heights on whole points.
/// With `body_line_height`, a body row's line height is instead its size
/// times that ratio ([`super::DocEditor::set_body_line_height`]).
fn face(field: Field, row: &Row, theme: &Theme, scale: f32, body_line_height: Option<f32>) -> Face {
    let f = face_at_base(field, row, theme);
    let size = f.size * scale;
    let line_height = match body_line_height.filter(|_| is_body(field, row)) {
        Some(ratio) => size * ratio,
        None => f.line_height * scale,
    };
    Face {
        size,
        line_height: line_height.round(),
        ..f
    }
}

/// Whether `row` is body text: a paragraph, list item or quote (and the
/// blank lines and rules between them), not a heading, code or the title.
fn is_body(field: Field, row: &Row) -> bool {
    field == Field::Body
        && matches!(
            row.kind,
            RowKind::Empty | RowKind::Paragraph | RowKind::Item(_) | RowKind::Quote | RowKind::Rule
        )
}

/// `row`'s face at the UI's base size.
fn face_at_base(field: Field, row: &Row, theme: &Theme) -> Face {
    let ui = |ty: Type, color| Face {
        family: UI_FAMILY,
        size: ty.size(),
        line_height: ty.line_height(),
        weight: ty.weight(),
        color,
    };
    if field == Field::Title {
        return ui(Type::Title, theme.text_loud);
    }
    match &row.kind {
        RowKind::Heading(1) => ui(Type::Heading, theme.text_loud),
        RowKind::Heading(_) => ui(Type::BaseSemibold, theme.text_loud),
        RowKind::Quote => ui(Type::Base, theme.muted),
        RowKind::Code => Face {
            family: PRIMARY_FAMILY,
            size: 12.0,
            line_height: 18.0,
            weight: FontWeight::NORMAL,
            color: theme.foreground,
        },
        RowKind::Fence { .. } | RowKind::Raw | RowKind::Table(_) => Face {
            family: PRIMARY_FAMILY,
            size: 12.0,
            line_height: 18.0,
            weight: FontWeight::NORMAL,
            color: theme.muted,
        },
        _ => ui(Type::Base, theme.foreground),
    }
}

/// The color a code line's token paints, or `None` for the code's own: the
/// diff view's ([`syntax_color`]), but a comment is the code's text faded
/// rather than the muted role, which falls under 3:1 on a block in the light
/// theme at low contrast.
fn code_color(theme: &Theme, class: SyntaxClass) -> Option<Hsla> {
    match class {
        SyntaxClass::Comment => Some(Hsla {
            a: theme.foreground.a * 0.66,
            ..theme.foreground
        }),
        _ => syntax_color(theme, class),
    }
}

/// The room above a row.
fn gap(g: Gap) -> f32 {
    match g {
        Gap::None => 0.0,
        Gap::Tight => 4.0,
        Gap::List | Gap::Heading => 12.0,
        Gap::Block => 20.0,
    }
}

/// The runs that set `row`'s display in `f`, `marked` (display offsets)
/// underlined for an input method's composition, `syntax` (a code line's
/// highlights) in their classes' colors.
fn runs(
    row: &Row,
    f: &Face,
    marked: Option<std::ops::Range<usize>>,
    syntax: &[SyntaxSpan],
    theme: &Theme,
) -> Vec<TextRun> {
    let len = row.display.len();
    let mut cuts = vec![0, len];
    for (r, _) in &row.styles {
        cuts.extend([r.start, r.end]);
    }
    for s in syntax {
        let (a, b) = (s.range.start as usize, s.range.end as usize);
        if b <= len && row.display.is_char_boundary(a) && row.display.is_char_boundary(b) {
            cuts.extend([a, b]);
        }
    }
    if let Some(m) = &marked {
        cuts.extend([m.start.min(len), m.end.min(len)]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    let base = Font {
        weight: f.weight,
        ..font(f.family)
    };
    let mut out = Vec::new();
    for w in cuts.windows(2) {
        let (a, b) = (w[0], w[1]);
        if a >= b {
            continue;
        }
        let s: Style = row.style_at(a);
        let mut fnt = base.clone();
        let mut color = f.color;
        let mut background = None;
        if s.bold {
            fnt.weight = FontWeight::BOLD;
        }
        if s.italic {
            fnt.style = FontStyle::Italic;
        }
        if s.code {
            fnt.family = PRIMARY_FAMILY.into();
            background = Some(theme.background);
        }
        if s.link {
            color = theme.link;
        }
        if s.raw {
            color = theme.muted;
        }
        if let Some(c) = syntax
            .iter()
            .find(|s| (s.range.start as usize) <= a && a < s.range.end as usize)
            .and_then(|s| code_color(theme, s.class))
        {
            color = c;
        }
        let in_marked = marked
            .as_ref()
            .is_some_and(|m| m.start <= a && b <= m.end && m.start < m.end);
        out.push(TextRun {
            len: b - a,
            font: fnt,
            color,
            background_color: background,
            underline: in_marked.then_some(UnderlineStyle {
                thickness: px(1.0),
                color: Some(color),
                wavy: false,
            }),
            strikethrough: s.strike.then_some(StrikethroughStyle {
                thickness: px(1.0),
                color: Some(color),
            }),
        });
    }
    if out.is_empty() {
        out.push(TextRun {
            len: 0,
            font: base,
            color: f.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        });
    }
    out
}

/// The distinct colors `row` is set in, with `syntax` its highlights.
#[cfg(test)]
pub fn run_colors(row: &Row, field: Field, syntax: &[SyntaxSpan], theme: &Theme) -> Vec<Hsla> {
    let mut out: Vec<Hsla> = Vec::new();
    for run in runs(row, &face(field, row, theme, 1.0, None), None, syntax, theme) {
        if !out.contains(&run.color) {
            out.push(run.color);
        }
    }
    out
}

/// Lay `doc` out at `width`. `marked` is the composition, in source
/// offsets; `code` the rows left out and the code lines' highlights;
/// `scale` the editor's [`super::DocEditor::scale`], which sets every text
/// size, line height, gap and inset in proportion (1 for the UI's sizes);
/// `body_line_height` the editor's body line height over its size, if set.
#[allow(clippy::too_many_arguments)]
pub fn layout(
    doc: &Doc,
    field: Field,
    width: Pixels,
    marked: Option<std::ops::Range<usize>>,
    code: Code<'_>,
    scale: f32,
    body_line_height: Option<f32>,
    theme: &Theme,
    window: &mut Window,
    _cx: &mut App,
) -> DocLayout {
    let mut rows = Vec::with_capacity(doc.rows.len());
    let mut y = px(0.0);
    let mut blocks: Vec<Bounds<Pixels>> = Vec::new();
    let mut bars = Vec::new();
    let mut rules = Vec::new();
    let mut tables: Vec<TableLayout> = Vec::new();
    // Rows a table's grid already laid out.
    let mut skip = 0;
    // Each level's running number in an ordered list (`None` for bullets).
    let mut numbers: Vec<Option<u64>> = Vec::new();
    // The room a left-out row would have had above it, for the next one
    // shown (an opening fence's gap above its block).
    let mut carried = 0.0f32;
    let mut shown_any = false;
    let room = |g: Gap| gap(g) * scale;
    // The block of the row shown before or after `ri`, past left-out rows.
    let prev_block = |ri: usize| (0..ri).rev().find(|&i| !code.hidden(i)).and_then(|i| doc.rows[i].block);
    let next_block = |ri: usize| (ri + 1..doc.rows.len()).find(|&i| !code.hidden(i)).and_then(|i| doc.rows[i].block);
    for (ri, row) in doc.rows.iter().enumerate() {
        if ri < skip {
            continue;
        }
        if code.grid(ri) {
            let last = (ri..doc.rows.len())
                .take_while(|&i| code.grid(i) && doc.rows[i].block == row.block)
                .last()
                .unwrap_or(ri);
            if shown_any {
                y += px(room(row.gap).max(carried));
            }
            carried = 0.0;
            shown_any = true;
            numbers.clear();
            let block = row.block.unwrap_or(0);
            let scroll = code.scroll.and_then(|m| m.get(&block)).copied().unwrap_or(px(0.0));
            let at = Grid {
                top: y,
                width,
                scroll,
                index: tables.len(),
                body_line_height,
            };
            let (table, laid) = lay_table(doc, ri..=last, at, scale, theme, window);
            y = table.bounds.origin.y + table.bounds.size.height;
            tables.push(table);
            rows.extend(laid);
            skip = last + 1;
            continue;
        }
        let f = face(field, row, theme, scale, body_line_height);
        let lh = px(f.line_height);
        if code.hidden(ri) {
            if shown_any {
                carried = carried.max(room(row.gap));
            }
            rows.push(RowLayout {
                top: y,
                left: px(CODE_INSET * scale),
                line_height: lh,
                lines: Vec::new(),
                gutter: None,
                hidden: true,
                cells: Vec::new(),
                table: None,
            });
            continue;
        }
        if shown_any {
            y += px(room(row.gap).max(carried));
        }
        carried = 0.0;
        shown_any = true;
        let new_block = row.block.is_some() && prev_block(ri) != row.block;
        let ends_block = row.block.is_some() && next_block(ri) != row.block;
        if new_block {
            y += px(CODE_PAD * scale);
        }
        let level = px(row.level as f32 * INDENT * scale);
        let (left, gutter) = match &row.kind {
            RowKind::Item(item) => {
                numbers.truncate(row.level + 1);
                while numbers.len() <= row.level {
                    numbers.push(None);
                }
                let g = match (&item.marker, item.task) {
                    (_, Some(checked)) => {
                        numbers[row.level] = None;
                        Gutter::Check(checked)
                    }
                    (Marker::Bullet(_), None) => {
                        numbers[row.level] = None;
                        Gutter::Bullet
                    }
                    (Marker::Ordered { number, delim }, None) => {
                        let n = numbers[row.level].map_or(*number, |p| p + 1);
                        numbers[row.level] = Some(n);
                        Gutter::Number(format!("{n}{delim}"))
                    }
                };
                let gb = Bounds {
                    origin: point(level, y),
                    size: size(px(INDENT * scale), lh),
                };
                (level + px(INDENT * scale), Some((g, gb)))
            }
            RowKind::Quote => (px(QUOTE_INSET * scale), None),
            _ if row.block.is_some() => (px(CODE_INSET * scale), None),
            _ => {
                if row.level == 0 && !matches!(row.kind, RowKind::Empty) {
                    numbers.clear();
                } else {
                    numbers.truncate(row.level);
                }
                (level, None)
            }
        };
        let text_w = (width - left - if row.block.is_some() { px(CODE_INSET * scale) } else { px(0.0) }).max(px(20.0));
        let marked_here = marked
            .as_ref()
            .filter(|m| row.lines.start <= m.start && m.start <= row.lines.end)
            .map(|m| row.display_of(m.start)..row.display_of(m.end.min(row.lines.end)));
        let text: SharedString = row.display.clone().into();
        let shaped = window
            .text_system()
            .shape_text(
                text,
                px(f.size),
                &runs(row, &f, marked_here, code.spans(ri), theme),
                Some(text_w),
                None,
            )
            .map(|v| v.into_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        let mut lines = Vec::with_capacity(shaped.len());
        let mut at = 0;
        let mut height = px(0.0);
        for line in shaped {
            let len = line.len();
            height += lh * (line.wrap_boundaries().len() + 1) as f32;
            lines.push((at, line));
            at += len + 1;
        }
        if lines.is_empty() {
            height = lh;
        }
        if row.kind == RowKind::Rule {
            rules.push(Bounds {
                origin: point(px(0.0), y + lh / 2.0),
                size: size(width, px(1.0)),
            });
        }
        if row.kind == RowKind::Quote {
            bars.push(Bounds {
                origin: point(px(0.0), y),
                size: size(px(2.0), height),
            });
        }
        if row.block.is_some() {
            let top = if new_block { y - px(CODE_PAD * scale) } else { y };
            let bottom = y + height + if ends_block { px(CODE_PAD * scale) } else { px(0.0) };
            match blocks.last_mut().filter(|_| !new_block) {
                Some(b) => b.size.height = bottom - b.origin.y,
                None => blocks.push(Bounds {
                    origin: point(px(0.0), top),
                    size: size(width, bottom - top),
                }),
            }
        }
        rows.push(RowLayout {
            top: y,
            left,
            line_height: lh,
            lines,
            gutter,
            hidden: false,
            cells: Vec::new(),
            table: None,
        });
        y += height;
        if ends_block {
            y += px(CODE_PAD * scale);
        }
    }
    DocLayout {
        rows,
        height: y,
        blocks,
        bars,
        rules,
        tables,
    }
}

/// Where a table's grid goes: its top, the editor's width, its scroll, its
/// index in [`DocLayout::tables`], and the editor's body line height (its
/// cells are body text).
struct Grid {
    top: Pixels,
    width: Pixels,
    scroll: Pixels,
    index: usize,
    body_line_height: Option<f32>,
}

/// `text`'s lines shaped in `runs`, wrapped at `wrap`: each one's start and
/// shaping.
fn shape_lines(text: &str, f: &Face, runs: &[TextRun], wrap: Option<Pixels>, window: &mut Window) -> Vec<(usize, WrappedLine)> {
    let shaped = window
        .text_system()
        .shape_text(SharedString::from(text.to_string()), px(f.size), runs, wrap, None)
        .map(|v| v.into_iter().collect::<Vec<_>>())
        .unwrap_or_default();
    let mut at = 0;
    shaped
        .into_iter()
        .map(|line| {
            let start = at;
            at += line.len() + 1;
            (start, line)
        })
        .collect()
}

/// The words of `text` (runs of non-whitespace), as byte ranges.
fn words(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        match (c.is_whitespace(), start) {
            (true, Some(s)) => {
                out.push((s, i));
                start = None;
            }
            (false, None) => start = Some(i),
            _ => {}
        }
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out
}

/// The text's alignment for a column's.
fn text_align(a: Align) -> TextAlign {
    match a {
        Align::Center => TextAlign::Center,
        Align::Right => TextAlign::Right,
        Align::None | Align::Left => TextAlign::Left,
    }
}

/// Lay a table's rows (`rows`, every one drawn as its grid) out at `at`.
fn lay_table(
    doc: &Doc,
    rows: RangeInclusive<usize>,
    at: Grid,
    scale: f32,
    theme: &Theme,
    window: &mut Window,
) -> (TableLayout, Vec<RowLayout>) {
    let first = &doc.rows[*rows.start()];
    let aligns = first.table().map(|t| t.aligns.clone()).unwrap_or_default();
    let n = aligns.len().max(1);
    let (pad_x, pad_y) = (CELL_PAD_X * scale, CELL_PAD_Y * scale);
    let rule = px(1.0);
    // A cell's face: the body's (a paragraph's), the header's semibold and
    // loud.
    let cell_face = |header: bool, cell: &Row| {
        let mut f = face(Field::Body, cell, theme, scale, at.body_line_height);
        if header {
            f.weight = Type::BaseSemibold.weight();
            f.color = theme.text_loud;
        }
        f
    };
    let lines_of = |i: usize| doc.rows[i].table().filter(|t| t.part != TablePart::Delimiter);

    // What each column wants (its widest line) and holds open (its widest
    // word).
    let mut want = vec![0.0f32; n];
    let mut least = vec![0.0f32; n];
    for i in rows.clone() {
        let Some(t) = lines_of(i) else {
            continue;
        };
        for (j, cell) in t.cells.iter().enumerate().take(n) {
            let f = cell_face(t.part == TablePart::Header, cell);
            let runs = runs(cell, &f, None, &[], theme);
            for (off, line) in shape_lines(&cell.display, &f, &runs, None, window) {
                let l = &line.unwrapped_layout;
                want[j] = want[j].max(l.width.as_f32());
                let text = &cell.display[off..off + line.len()];
                for (a, z) in words(text) {
                    let w = (l.x_for_index(z) - l.x_for_index(a)).as_f32();
                    least[j] = least[j].max(w.min(MAX_WORD * scale));
                }
            }
        }
    }
    let pad = |w: f32| (w.ceil() + 2.0 * pad_x + 1.0).max(MIN_COLUMN * scale);
    let want: Vec<f32> = want.into_iter().map(pad).collect();
    let least: Vec<f32> = least.into_iter().map(pad).collect();
    let room = at.width.as_f32() - (n + 1) as f32;
    let cols = column_widths(&want, &least, room);
    let grid_w = px(cols.iter().sum::<f32>() + (n + 1) as f32);
    let max_scroll = (grid_w - at.width).max(px(0.0));
    let scroll = at.scroll.max(px(0.0)).min(max_scroll);
    let left = -scroll;
    let mut col_x = Vec::with_capacity(n);
    let mut x = left + rule;
    for w in &cols {
        col_x.push(x);
        x += px(*w) + rule;
    }

    let mut laid = Vec::new();
    let mut rules = Vec::new();
    let mut y = at.top + rule;
    let last_shown = rows.clone().rev().find(|&i| lines_of(i).is_some());
    for i in rows.clone() {
        let Some(t) = lines_of(i) else {
            laid.push(RowLayout {
                top: y,
                left: px(0.0),
                line_height: px(0.0),
                lines: Vec::new(),
                gutter: None,
                hidden: true,
                cells: Vec::new(),
                table: None,
            });
            continue;
        };
        let header = t.part == TablePart::Header;
        let mut shaped = Vec::with_capacity(n);
        let mut text_h = px(0.0);
        for (j, cell) in t.cells.iter().enumerate().take(n) {
            let f = cell_face(header, cell);
            let lh = px(f.line_height);
            let wrap = px((cols[j] - 2.0 * pad_x).max(1.0));
            let lines = shape_lines(&cell.display, &f, &runs(cell, &f, None, &[], theme), Some(wrap), window);
            let h = lines
                .iter()
                .map(|(_, l)| lh * (l.wrap_boundaries().len() + 1) as f32)
                .fold(px(0.0), |a, b| a + b)
                .max(lh);
            text_h = text_h.max(h);
            shaped.push((lines, lh, wrap));
        }
        let row_h = text_h + px(2.0 * pad_y);
        let cells = shaped
            .into_iter()
            .enumerate()
            .map(|(j, (lines, lh, wrap))| CellLayout {
                cell: Bounds {
                    origin: point(col_x[j], y),
                    size: size(px(cols[j]), row_h),
                },
                text: Bounds {
                    origin: point(col_x[j] + px(pad_x), y + px(pad_y)),
                    size: size(wrap, text_h),
                },
                lines,
                line_height: lh,
                align: text_align(aligns.get(j).copied().unwrap_or_default()),
            })
            .collect();
        laid.push(RowLayout {
            top: y,
            left: px(0.0),
            line_height: row_h,
            lines: Vec::new(),
            gutter: None,
            hidden: false,
            cells,
            table: Some(at.index),
        });
        y += row_h;
        if Some(i) != last_shown {
            rules.push(Bounds {
                origin: point(left, y),
                size: size(grid_w, rule),
            });
        }
        y += rule;
    }
    for x in col_x.iter().skip(1) {
        rules.push(Bounds {
            origin: point(*x - rule, at.top),
            size: size(rule, y - at.top),
        });
    }
    let grid = Bounds {
        origin: point(left, at.top),
        size: size(grid_w, y - at.top),
    };
    let box_w = grid_w.min(at.width);
    let mut height = y - at.top;
    let mut thumb = None;
    if max_scroll > px(0.0) {
        let track = at.width.as_f32();
        let w = (track * track / grid_w.as_f32()).max(24.0);
        let x = (track - w) * (scroll / max_scroll);
        thumb = Some(Bounds {
            origin: point(px(x), y + px(3.0 * scale)),
            size: size(px(w), px(3.0)),
        });
        height += px(SCROLL_ROOM * scale);
    }
    let table = TableLayout {
        block: first.block.unwrap_or(0),
        bounds: Bounds {
            origin: point(px(0.0), at.top),
            size: size(box_w, height),
        },
        grid,
        rules,
        max_scroll,
        thumb,
    };
    (table, laid)
}

/// A shaped line's visual (wrapped) lines: each one's start and end index.
pub fn sub_lines(line: &WrappedLine) -> Vec<(usize, usize)> {
    let mut starts = vec![0];
    for b in line.wrap_boundaries() {
        starts.push(line.unwrapped_layout.runs[b.run_ix].glyphs[b.glyph_ix].index);
    }
    let mut out = Vec::with_capacity(starts.len());
    for (i, s) in starts.iter().enumerate() {
        let e = starts.get(i + 1).copied().unwrap_or(line.len());
        out.push((*s, e));
    }
    out
}

/// One visual line of the document.
#[derive(Debug, Clone, Copy)]
pub struct Visual {
    pub row: usize,
    pub line: usize,
    pub sub: usize,
    pub top: Pixels,
    pub height: Pixels,
    /// Display offsets (in the row) it spans.
    pub start: usize,
    pub end: usize,
}

impl DocLayout {
    /// Every visual line, top to bottom.
    pub fn visuals(&self) -> Vec<Visual> {
        let mut out = Vec::new();
        for (ri, r) in self.rows.iter().enumerate() {
            if r.hidden {
                continue;
            }
            if r.lines.is_empty() {
                out.push(Visual {
                    row: ri,
                    line: 0,
                    sub: 0,
                    top: r.top,
                    height: r.line_height,
                    start: 0,
                    end: 0,
                });
                continue;
            }
            let mut top = r.top;
            for (li, (off, line)) in r.lines.iter().enumerate() {
                for (si, (s, e)) in sub_lines(line).into_iter().enumerate() {
                    out.push(Visual {
                        row: ri,
                        line: li,
                        sub: si,
                        top,
                        height: r.line_height,
                        start: off + s,
                        end: off + e,
                    });
                    top += r.line_height;
                }
            }
        }
        out
    }

    /// The x of display offset `d` on visual line `v`.
    pub fn x_at(&self, v: &Visual, d: usize) -> Pixels {
        let r = &self.rows[v.row];
        let Some((off, line)) = r.lines.get(v.line) else {
            return r.left;
        };
        let start = v.start - off;
        let i = d.clamp(v.start, v.end) - off;
        r.left + line.unwrapped_layout.x_for_index(i) - line.unwrapped_layout.x_for_index(start)
    }

    /// The visual line display offset `d` of row `row` sits on: the one
    /// starting at it when it falls on a wrap.
    pub fn visual_of(&self, visuals: &[Visual], row: usize, d: usize) -> Visual {
        let mut found = None;
        for v in visuals.iter().filter(|v| v.row == row) {
            if v.start <= d && d <= v.end {
                found = Some(*v);
                if d < v.end {
                    break;
                }
            }
        }
        found.unwrap_or_else(|| *visuals.iter().rfind(|v| v.row == row).unwrap_or(&visuals[0]))
    }

    /// The caret's box for display offset `d` of row `row`.
    pub fn caret(&self, visuals: &[Visual], row: usize, d: usize) -> Bounds<Pixels> {
        let v = self.visual_of(visuals, row, d);
        Bounds {
            origin: point(self.x_at(&v, d), v.top),
            size: size(px(1.5), v.height),
        }
    }

    /// The display offset on visual line `v` closest to `x`.
    pub fn offset_at_x(&self, v: &Visual, x: Pixels, row_display: &str) -> usize {
        let r = &self.rows[v.row];
        let Some((off, line)) = r.lines.get(v.line) else {
            return 0;
        };
        let start = v.start - off;
        let base = line.unwrapped_layout.x_for_index(start);
        let mut i = line.unwrapped_layout.closest_index_for_x(x - r.left + base) + off;
        i = i.clamp(v.start, v.end);
        // Past the end of a wrapped line: before the space it wrapped at,
        // so the caret stays on this line.
        let last_sub = sub_lines(line).len() - 1;
        if i == v.end && v.sub < last_sub && i > v.start {
            i = shelbi_app::markdown_edit::prev_boundary(row_display, i);
        }
        while i > 0 && !row_display.is_char_boundary(i) {
            i -= 1;
        }
        i
    }

    /// The visual line at `y` (the nearest one above or below the text).
    pub fn visual_at_y(&self, visuals: &[Visual], y: Pixels) -> Visual {
        let mut best = visuals[0];
        let mut dist = f32::MAX;
        for v in visuals {
            if v.top <= y && y < v.top + v.height {
                return *v;
            }
            let d = if y < v.top {
                (v.top - y).as_f32()
            } else {
                (y - v.top - v.height).as_f32()
            };
            if d < dist {
                dist = d;
                best = *v;
            }
        }
        best
    }

    /// The source offset at `pos`.
    pub fn hit(&self, doc: &Doc, pos: Point<Pixels>) -> usize {
        let visuals = self.visuals();
        let v = self.visual_at_y(&visuals, pos.y);
        self.source_at(doc, &v, pos)
    }

    /// The source offset on visual line `v` closest to `pos`: by its x, and
    /// in a table's grid row by the cell under it and the line of the cell's
    /// text at its y.
    pub fn source_at(&self, doc: &Doc, v: &Visual, pos: Point<Pixels>) -> usize {
        if let Some(src) = self.cell_source(doc, v.row, pos) {
            return src;
        }
        let row = &doc.rows[v.row];
        row.source_of(self.offset_at_x(v, pos.x, &row.display))
    }

    /// In grid row `row`, the source offset in the cell nearest `pos`.
    fn cell_source(&self, doc: &Doc, row: usize, pos: Point<Pixels>) -> Option<usize> {
        let r = self.rows.get(row)?;
        let t = doc.rows.get(row)?.table()?;
        let off = |c: &CellLayout| {
            if pos.x < c.cell.left() {
                (c.cell.left() - pos.x).as_f32()
            } else {
                (pos.x - c.cell.right()).max(px(0.0)).as_f32()
            }
        };
        let (ci, c) = r
            .cells
            .iter()
            .enumerate()
            .min_by(|a, b| off(a.1).total_cmp(&off(b.1)))?;
        let cell = t.cells.get(ci)?;
        let subs: Vec<(usize, &WrappedLine, usize, usize, bool)> = c
            .lines
            .iter()
            .flat_map(|(at, line)| {
                let subs = sub_lines(line);
                let last = subs.len().saturating_sub(1);
                subs.into_iter()
                    .enumerate()
                    .map(move |(k, (s, e))| (*at, line, s, e, k == last))
            })
            .collect();
        if subs.is_empty() {
            return Some(cell.source_of(0));
        }
        let k = ((pos.y - c.text.top()).as_f32() / c.line_height.as_f32()).floor().max(0.0) as usize;
        let (at, line, s, e, last) = subs[k.min(subs.len() - 1)];
        let l = &line.unwrapped_layout;
        let line_w = l.x_for_index(e) - l.x_for_index(s);
        let shift = match c.align {
            TextAlign::Left => px(0.0),
            TextAlign::Center => (c.text.size.width - line_w) / 2.0,
            TextAlign::Right => c.text.size.width - line_w,
        };
        let mut i = l
            .closest_index_for_x(pos.x - c.text.left() - shift + l.x_for_index(s))
            .clamp(s, e);
        if i == e && !last && i > s {
            i = shelbi_app::markdown_edit::prev_boundary(&cell.display[at..], i);
        }
        let mut d = (at + i).min(cell.display.len());
        while d > 0 && !cell.display.is_char_boundary(d) {
            d -= 1;
        }
        Some(cell.source_of(d))
    }

    /// The row whose checkbox is at `pos`.
    pub fn check_at(&self, pos: Point<Pixels>) -> Option<usize> {
        self.rows.iter().position(
            |r| matches!(r.gutter, Some((Gutter::Check(_), b)) if check_box(b).dilate(px(4.0)).contains(&pos)),
        )
    }

    /// Selection boxes for source range `sel`.
    pub fn selection(&self, doc: &Doc, sel: std::ops::Range<usize>) -> Vec<Bounds<Pixels>> {
        if sel.is_empty() {
            return Vec::new();
        }
        let visuals = self.visuals();
        let mut out = Vec::new();
        for (ri, row) in doc.rows.iter().enumerate() {
            if sel.end < row.lines.start || sel.start > row.lines.end.max(row.content_start) {
                continue;
            }
            let d0 = if sel.start <= row.content_start {
                0
            } else {
                row.display_of(sel.start)
            };
            let past_end = sel.end > row.lines.end;
            let d1 = if past_end {
                row.display.len()
            } else {
                row.display_of(sel.end)
            };
            for v in visuals.iter().filter(|v| v.row == ri) {
                let a = d0.max(v.start);
                let b = d1.min(v.end);
                let is_last = !visuals.iter().any(|o| o.row == ri && o.top > v.top);
                if a > b || (a == b && !(past_end && is_last)) {
                    continue;
                }
                let x0 = self.x_at(v, a);
                let mut x1 = self.x_at(v, b);
                // A selection running on past the line shows its line break.
                if past_end && is_last && b == v.end {
                    x1 += px(6.0);
                }
                out.push(Bounds {
                    origin: point(x0, v.top),
                    size: size((x1 - x0).max(px(1.0)), v.height),
                });
            }
        }
        out
    }
}

/// The check's box in a gutter: 14pt at the gutter's left, centered on the
/// first line.
pub fn check_box(gutter: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            gutter.origin.x,
            gutter.origin.y + (gutter.size.height - px(CHECK)) / 2.0,
        ),
        size: size(px(CHECK), px(CHECK)),
    }
}

#[cfg(test)]
mod tests {
    use gpui::WindowAppearance;
    use shelbi_app::diff::highlight::SyntaxClass;
    use shelbi_app::theme::ThemePreference;

    use shelbi_app::markdown_edit::{Doc, RowKind};

    use super::{code_color, face, Field};
    use crate::theme::diff::contrast;
    use crate::theme::style::Type;
    use crate::theme::Theme;

    /// Documentation's body rows (paragraphs, items, quotes) sit on lines 1.4
    /// times their size at any body size; headings and code keep their
    /// scaled line heights, and the task body (no ratio) is unchanged.
    #[test]
    fn body_rows_take_the_body_line_height_and_the_rest_keep_theirs() {
        let theme = Theme::resolve_with(ThemePreference::Auto, 50, WindowAppearance::Dark);
        let doc = Doc::parse("A paragraph.\n\n- an item\n\n> a quote\n\n# Heading\n\n## Sub\n\n```\ncode\n```\n");
        let kinds = |pick: fn(&RowKind) -> bool| doc.rows.iter().filter(move |r| pick(&r.kind));
        let body = || kinds(|k| matches!(k, RowKind::Paragraph | RowKind::Item(_) | RowKind::Quote));
        let other = || kinds(|k| matches!(k, RowKind::Heading(_) | RowKind::Code | RowKind::Fence { .. }));
        assert_eq!(body().count(), 3);
        assert_eq!(other().count(), 5);
        for size in [14.0, 16.0] {
            let scale = size / Type::Base.size();
            for row in body() {
                let docs = face(Field::Body, row, &theme, scale, Some(1.4));
                assert_eq!(docs.size, size);
                assert_eq!(docs.line_height, (size * 1.4).round(), "{:?} at {size}", row.kind);
                let task = face(Field::Body, row, &theme, scale, None);
                assert_eq!(task.line_height, (Type::Base.line_height() * scale).round(), "{:?}", row.kind);
            }
            for row in other() {
                let docs = face(Field::Body, row, &theme, scale, Some(1.4));
                let task = face(Field::Body, row, &theme, scale, None);
                assert_eq!(docs.line_height, task.line_height, "{:?} at {size}", row.kind);
            }
            // The title keeps its own.
            let title = Doc::plain("Title");
            let t = |ratio| face(Field::Title, &title.rows[0], &theme, scale, ratio).line_height;
            assert_eq!(t(Some(1.4)), t(None));
        }
        // 14pt on 20 (19.6 to whole points), 16pt on 22 (22.4).
        let row = body().next().unwrap();
        assert_eq!(face(Field::Body, row, &theme, 1.0, Some(1.4)).line_height, 20.0);
        assert_eq!(face(Field::Body, row, &theme, 16.0 / 14.0, Some(1.4)).line_height, 22.0);
        // The task body is still 14 on 18.
        assert_eq!(face(Field::Body, row, &theme, 1.0, None).line_height, 18.0);
    }

    /// A code block's highlights read on its background in light and dark,
    /// at every contrast and window opacity (the editor paints from the
    /// opaque theme, so the glass never shows through the block).
    #[test]
    fn code_block_highlights_read_on_the_block_in_every_theme() {
        let classes = [
            SyntaxClass::Comment,
            SyntaxClass::String,
            SyntaxClass::Number,
            SyntaxClass::Keyword,
            SyntaxClass::Function,
            SyntaxClass::Type,
            SyntaxClass::Tag,
            SyntaxClass::Heading,
        ];
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            for level in [0, 25, 50, 75, 100] {
                for opacity in [1.0, 0.6] {
                    let theme = Theme::resolve_with(ThemePreference::Auto, level, system)
                        .with_opacity(opacity)
                        .opaque();
                    for class in classes {
                        let color = code_color(&theme, class).unwrap();
                        let ratio = contrast(color, theme.background, theme.canvas);
                        assert!(
                            ratio >= 3.0,
                            "{system:?} contrast {level} opacity {opacity}: {class:?} is only {ratio}:1"
                        );
                        if class == SyntaxClass::Comment {
                            let text = contrast(theme.foreground, theme.background, theme.canvas);
                            assert!(ratio < text * 0.8, "{system:?} {level}: a comment reads quieter than code");
                        }
                    }
                }
            }
        }
    }
}
