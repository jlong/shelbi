//! Where a [`Doc`]'s rows land: each row's shaped lines, gutter and block
//! chrome at a width, and the geometry the editor asks of them (where a
//! caret goes, what a point hits, the visual line above or below).
//!
//! Offsets here are display offsets of a row unless named `src`; positions
//! are relative to the editor element's origin.

use gpui::{
    font, point, px, size, App, Bounds, Font, FontStyle, FontWeight, Hsla, Pixels, Point, SharedString,
    StrikethroughStyle, TextRun, UnderlineStyle, Window, WrappedLine,
};
use shelbi_app::markdown_edit::{Doc, Gap, Marker, Row, RowKind, Style};

use crate::elements::terminal::metrics::PRIMARY_FAMILY;
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
}

/// How a row's text is set: its font, size, line height and color.
struct Face {
    family: &'static str,
    size: f32,
    line_height: f32,
    weight: FontWeight,
    color: Hsla,
}

fn face(field: Field, row: &Row, theme: &Theme) -> Face {
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
        RowKind::Fence { .. } | RowKind::Raw => Face {
            family: PRIMARY_FAMILY,
            size: 12.0,
            line_height: 18.0,
            weight: FontWeight::NORMAL,
            color: theme.muted,
        },
        _ => ui(Type::Base, theme.foreground),
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
/// underlined for an input method's composition.
fn runs(row: &Row, f: &Face, marked: Option<std::ops::Range<usize>>, theme: &Theme) -> Vec<TextRun> {
    let len = row.display.len();
    let mut cuts = vec![0, len];
    for (r, _) in &row.styles {
        cuts.extend([r.start, r.end]);
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

/// Lay `doc` out at `width`. `marked` is the composition, in source
/// offsets.
pub fn layout(
    doc: &Doc,
    field: Field,
    width: Pixels,
    marked: Option<std::ops::Range<usize>>,
    theme: &Theme,
    window: &mut Window,
    _cx: &mut App,
) -> DocLayout {
    let mut rows = Vec::with_capacity(doc.rows.len());
    let mut y = px(0.0);
    let mut blocks: Vec<Bounds<Pixels>> = Vec::new();
    let mut bars = Vec::new();
    let mut rules = Vec::new();
    // Each level's running number in an ordered list (`None` for bullets).
    let mut numbers: Vec<Option<u64>> = Vec::new();
    for (ri, row) in doc.rows.iter().enumerate() {
        let f = face(field, row, theme);
        let lh = px(f.line_height);
        if ri > 0 {
            y += px(gap(row.gap));
        }
        let new_block = row.block.is_some() && (ri == 0 || doc.rows[ri - 1].block != row.block);
        let ends_block = row.block.is_some() && doc.rows.get(ri + 1).is_none_or(|n| n.block != row.block);
        if new_block {
            y += px(CODE_PAD);
        }
        let level = px(row.level as f32 * INDENT);
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
                    size: size(px(INDENT), lh),
                };
                (level + px(INDENT), Some((g, gb)))
            }
            RowKind::Quote => (px(QUOTE_INSET), None),
            _ if row.block.is_some() => (px(CODE_INSET), None),
            _ => {
                if row.level == 0 && !matches!(row.kind, RowKind::Empty) {
                    numbers.clear();
                } else {
                    numbers.truncate(row.level);
                }
                (level, None)
            }
        };
        let text_w = (width - left - if row.block.is_some() { px(CODE_INSET) } else { px(0.0) }).max(px(20.0));
        let marked_here = marked
            .as_ref()
            .filter(|m| row.lines.start <= m.start && m.start <= row.lines.end)
            .map(|m| row.display_of(m.start)..row.display_of(m.end.min(row.lines.end)));
        let text: SharedString = row.display.clone().into();
        let shaped = window
            .text_system()
            .shape_text(text, px(f.size), &runs(row, &f, marked_here, theme), Some(text_w), None)
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
            let top = if new_block { y - px(CODE_PAD) } else { y };
            let bottom = y + height + if ends_block { px(CODE_PAD) } else { px(0.0) };
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
        });
        y += height;
        if ends_block {
            y += px(CODE_PAD);
        }
    }
    DocLayout {
        rows,
        height: y,
        blocks,
        bars,
        rules,
    }
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
        let row = &doc.rows[v.row];
        let d = self.offset_at_x(&v, pos.x, &row.display);
        row.source_of(d)
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
