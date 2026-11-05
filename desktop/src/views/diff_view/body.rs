//! The diff's rows, painted: a custom element, as the terminal's grid is. Only
//! the rows in view are shaped, so the cost of a frame doesn't depend on the
//! size of the diff.
//!
//! - **Prepaint** measures the cell from the terminal font settings (the diff
//!   follows ⌘+ and ⌘−, as the terminals do), tells the state how many rows fit,
//!   then lays out each row in view into fills and shaped lines.
//! - **Paint** draws the fills back to front, then the text, clipped to its
//!   column so a long line never runs over the gutter or the other side of a
//!   split.
//!
//! A row's anatomy, in cells:
//!
//! ```text
//! unified  [blame ]  old  new │ + text
//! split    [blame ]  old - text              │  new + text
//! ```

use std::ops::Range;

use gpui::{
    fill, point, px, relative, size, App, Bounds, ContentMask, Element, ElementId, Entity, GlobalElementId, Hsla,
    InspectorElementId, IntoElement, LayoutId, Pixels, Point, ShapedLine, SharedString, Style, TextAlign, TextRun,
    Window,
};
use shelbi_app::diff::git::BlameLine;
use shelbi_app::diff::highlight::SyntaxSpan;
use shelbi_app::diff::{DiffLine, DiffState, FileDiff, FileStatus, LineKind, Row, ThreadPart};

use super::text::{age, digits, stretches, Shown};
use super::DiffView;
use crate::elements::terminal::metrics::CellMetrics;
use crate::elements::terminal::TerminalSettings;
use crate::theme::diff::{syntax_color, DiffColors};
use crate::theme::Theme;

/// The blame gutter's width, in cells (`abc1234 Author name  3d`).
const BLAME_CELLS: usize = 26;
/// The cursor's bar at the row's leading edge.
const BAR_W: f32 = 2.0;
/// The space a line's height gets beyond the terminal's cell, for air.
const ROW_LEADING: f32 = 2.0;

/// A row's height at `metrics`.
pub fn row_height(metrics: &CellMetrics) -> Pixels {
    metrics.height + px(ROW_LEADING)
}

/// What the view remembers of the last frame, to read a click and a wheel.
#[derive(Debug, Clone, Copy, Default)]
pub struct BodyGeometry {
    pub bounds: Bounds<Pixels>,
    pub row_h: Pixels,
    pub cell_w: Pixels,
    /// How far a long line can scroll sideways, in this frame's rows.
    pub max_scroll_x: Pixels,
}

impl BodyGeometry {
    /// The row under `position`, from the top row `top`.
    pub fn row_at(&self, position: Point<Pixels>, top: usize) -> Option<usize> {
        if self.row_h <= px(0.0) || !self.bounds.contains(&position) {
            return None;
        }
        Some(top + (f32::from(position.y - self.bounds.origin.y) / f32::from(self.row_h)) as usize)
    }

    /// Whether `position` is over a header's fold chevron.
    pub fn on_chevron(&self, position: Point<Pixels>) -> bool {
        position.x - self.bounds.origin.x < self.cell_w * 3.0
    }
}

/// The element. Cheap to build each render; everything is read in prepaint, so
/// the row count it reports moves the view in the same frame.
pub struct DiffBody {
    view: Entity<DiffView>,
}

impl DiffBody {
    pub fn new(view: Entity<DiffView>) -> Self {
        DiffBody { view }
    }
}

/// One frame's drawing.
#[derive(Default)]
pub struct Frame {
    fills: Vec<(Bounds<Pixels>, Hsla)>,
    texts: Vec<Text>,
    row_h: Pixels,
}

struct Text {
    line: ShapedLine,
    origin: Point<Pixels>,
    clip: Bounds<Pixels>,
}

impl IntoElement for DiffBody {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for DiffBody {
    type RequestLayoutState = ();
    type PrepaintState = Frame;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Frame {
        self.view.update(cx, |view, cx| {
            let settings = TerminalSettings::current(cx);
            let metrics = CellMetrics::cached(window, &settings.font_family, settings.font_size);
            let theme = *cx.global::<Theme>();
            let row_h = row_height(&metrics);
            let rows = ((f32::from(bounds.size.height) / f32::from(row_h)) as usize).max(1);
            let focused = view.focus_handle.is_focused(window) && !view.list_focus();
            view.session.state_mut().set_viewport(rows);
            let mut painter = Painter {
                state: view.session.state(),
                theme: &theme,
                colors: DiffColors::of(&theme),
                metrics: &metrics,
                bounds,
                row_h,
                scroll_x: view.scroll_x,
                number_cells: number_cells(view.session.state()),
                blame_cells: if view.session.state().blame_on() { BLAME_CELLS } else { 0 },
                focused,
                now: chrono::Utc::now().timestamp(),
                frame: Frame { row_h, ..Frame::default() },
                widest: px(0.0),
                text_w: px(0.0),
                window,
            };
            painter.scroll_to_col();
            painter.rows();
            painter.scrollbar();
            let (frame, widest, text_w, scroll_x) = (painter.frame, painter.widest, painter.text_w, painter.scroll_x);
            let max_scroll_x = (widest - text_w + metrics.width * 2.0).max(px(0.0));
            view.scroll_x = scroll_x;
            view.geometry = BodyGeometry { bounds, row_h, cell_w: metrics.width, max_scroll_x };
            frame
        })
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _state: &mut (),
        frame: &mut Frame,
        window: &mut Window,
        cx: &mut App,
    ) {
        for (at, color) in &frame.fills {
            window.paint_quad(fill(*at, *color));
        }
        for text in &frame.texts {
            window.with_content_mask(Some(ContentMask { bounds: text.clip }), |window| {
                let _ = text.line.paint(text.origin, frame.row_h, TextAlign::Left, None, window, cx);
            });
        }
    }
}

/// The widest line number in the changeset, in cells.
fn number_cells(state: &DiffState) -> usize {
    let max = state
        .changeset()
        .files
        .iter()
        .flat_map(|f| &f.hunks)
        .map(|h| (h.old_start + h.old_count).max(h.new_start + h.new_count))
        .max()
        .unwrap_or(0);
    digits(max)
}

/// A file header's words: the path, or where it came from and went.
pub fn file_title(file: &FileDiff) -> String {
    match (&file.old_path, file.status) {
        (Some(old), FileStatus::Renamed) => format!("{old} → {}", file.path),
        _ => file.path.clone(),
    }
}

/// A thread row's words, without its leading bar.
/// The rows' layout, into a [`Frame`].
struct Painter<'a> {
    state: &'a DiffState,
    theme: &'a Theme,
    colors: DiffColors,
    metrics: &'a CellMetrics,
    bounds: Bounds<Pixels>,
    row_h: Pixels,
    scroll_x: Pixels,
    number_cells: usize,
    blame_cells: usize,
    focused: bool,
    now: i64,
    frame: Frame,
    /// The widest text shaped this frame, and the room for it.
    widest: Pixels,
    text_w: Pixels,
    window: &'a mut Window,
}

/// One side of a line.
struct Side<'a> {
    line: &'a DiffLine,
    syntax: &'a [SyntaxSpan],
    file: u32,
    hunk: u32,
    index: u32,
    /// The cursor's column is on this side.
    has_caret: bool,
}

impl<'a> Painter<'a> {
    fn cells(&self, n: usize) -> Pixels {
        self.metrics.width * n as f32
    }

    fn rows(&mut self) {
        let top = self.state.top();
        let cursor = self.state.cursor();
        let selection = self.state.selection();
        let rows: Vec<Row> = self.state.visible_rows().to_vec();
        for (offset, row) in rows.iter().enumerate() {
            let at = top + offset;
            let y = self.bounds.origin.y + self.row_h * offset as f32;
            self.row(row, y);
            if selection.is_some_and(|(start, end)| (start..=end).contains(&at)) {
                self.fill_row(y, self.colors.selection);
            }
            if at == cursor {
                self.cursor(y);
            }
        }
    }

    fn fill_row(&mut self, y: Pixels, color: Hsla) {
        let at = Bounds::new(point(self.bounds.origin.x, y), size(self.bounds.size.width, self.row_h));
        self.frame.fills.push((at, color));
    }

    fn fill(&mut self, x: Pixels, y: Pixels, w: Pixels, color: Hsla) {
        self.frame.fills.push((Bounds::new(point(x, y), size(w, self.row_h)), color));
    }

    fn cursor(&mut self, y: Pixels) {
        if !self.focused {
            let bar = self.colors.cursor_bar_idle;
            self.fill(self.bounds.origin.x, y, px(BAR_W), bar);
            return;
        }
        self.fill_row(y, self.colors.cursor);
        let bar = self.colors.cursor_bar;
        self.fill(self.bounds.origin.x, y, px(BAR_W), bar);
    }

    /// Text in `color`, from `x`, clipped to `clip`.
    fn plain(&mut self, text: impl Into<SharedString>, color: Hsla, x: Pixels, y: Pixels, clip: Bounds<Pixels>) -> Pixels {
        let text = text.into();
        let run = self.run(text.len(), color);
        let line = self.window.text_system().shape_line(text, self.metrics.font_size, &[run], None);
        let width = line.width;
        self.frame.texts.push(Text { line, origin: point(x, y), clip });
        width
    }

    fn run(&self, len: usize, color: Hsla) -> TextRun {
        TextRun {
            len,
            font: self.metrics.font.clone(),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }
    }

    /// The whole row's width as a clip.
    fn row_clip(&self, y: Pixels) -> Bounds<Pixels> {
        Bounds::new(point(self.bounds.origin.x, y), size(self.bounds.size.width, self.row_h))
    }

    fn row(&mut self, row: &Row, y: Pixels) {
        match *row {
            Row::File { file, folded } => self.file_row(file, folded, y),
            Row::Hunk { file, hunk, folded } => self.hunk_row(file, hunk, folded, y),
            Row::Line { file, hunk, line } => self.unified_row(file, hunk, line, y),
            Row::Split { file, hunk, old, new } => self.split_row(file, hunk, old, new, y),
            Row::Thread { thread, part, .. } => self.thread_row(thread, part, y),
            Row::Notice { file, kind } => {
                let Some(f) = self.state.file(file) else { return };
                let text = kind.text(f);
                let clip = self.row_clip(y);
                let x = self.bounds.origin.x + self.cells(self.gutter_cells() + 1);
                self.plain(text, self.colors.muted, x, y, clip);
            }
        }
    }

    fn file_row(&mut self, file: u32, folded: bool, y: Pixels) {
        let Some(f) = self.state.file(file) else { return };
        self.fill_row(y, self.colors.file_header);
        let clip = self.row_clip(y);
        let left = self.bounds.origin.x + self.metrics.width;
        let chevron = if folded { "▸" } else { "▾" };
        self.plain(chevron, self.colors.muted, left, y, clip);
        // Right to left: the viewed check, then the counts. The path gets
        // what's left, so a narrow pane cuts it rather than overprint them.
        let mut right = self.bounds.origin.x + self.bounds.size.width - self.metrics.width;
        if self.state.is_viewed(f) {
            right -= self.metrics.width;
            self.plain("✓", self.colors.viewed, right, y, clip);
            right -= self.metrics.width;
        }
        if f.binary {
            let word = "binary";
            right -= self.cells(word.len());
            self.plain(word, self.colors.muted, right, y, clip);
        } else {
            if f.deletions > 0 {
                let text = format!("−{}", f.deletions);
                right -= self.cells(text.chars().count());
                self.plain(text, self.colors.del, right, y, clip);
                right -= self.metrics.width;
            }
            if f.additions > 0 {
                let text = format!("+{}", f.additions);
                right -= self.cells(text.chars().count());
                self.plain(text, self.colors.add, right, y, clip);
            }
        }
        let title_x = left + self.cells(2);
        let room = (right - self.metrics.width - title_x).max(px(0.0));
        let title = format!("{} {}", f.status.letter(), file_title(f));
        let title_clip = Bounds::new(point(title_x, y), size(room, self.row_h));
        self.plain(title, self.colors.loud, title_x, y, title_clip);
    }

    fn hunk_row(&mut self, file: u32, hunk: u32, folded: bool, y: Pixels) {
        let Some(h) = self.state.file(file).and_then(|f| f.hunks.get(hunk as usize)) else { return };
        self.fill_row(y, self.colors.hunk_header);
        let clip = self.row_clip(y);
        let left = self.bounds.origin.x + self.metrics.width;
        let mut text = format!("{} {}", if folded { "▸" } else { "▾" }, h.header());
        if folded {
            text.push_str(&format!("  ({} lines)", h.lines.len()));
        }
        self.plain(text, self.colors.muted, left, y, clip);
    }

    fn thread_row(&mut self, thread: u32, part: ThreadPart, y: Pixels) {
        let Some(text) = self.state.thread_row_text(thread, part) else { return };
        let clip = self.row_clip(y);
        let x = self.bounds.origin.x + self.cells(self.gutter_cells());
        let bar = self.colors.thread;
        self.fill(x, y, px(2.0), bar);
        self.plain(text, self.colors.text, x + self.metrics.width, y, clip);
    }

    /// The cells before a unified line's text: blame, both numbers, the rule.
    fn gutter_cells(&self) -> usize {
        self.blame_cells + 1 + self.number_cells * 2 + 2
    }

    fn side(&self, file: u32, hunk: u32, index: u32, has_caret: bool) -> Option<Side<'a>> {
        let state = self.state;
        let line = state.line(file, hunk, index)?;
        let syntax = state.highlights(file).map_or(&[][..], |h| h.line(hunk as usize, index as usize));
        Some(Side { line, syntax, file, hunk, index, has_caret })
    }

    fn is_cursor(&self, y: Pixels) -> bool {
        let offset = f32::from(y - self.bounds.origin.y) / f32::from(self.row_h);
        self.state.top() + offset.round() as usize == self.state.cursor()
    }

    fn unified_row(&mut self, file: u32, hunk: u32, line: u32, y: Pixels) {
        let Some(side) = self.side(file, hunk, line, true) else { return };
        let (kind, old, new) = (side.line.kind, side.line.old, side.line.new);
        let number_cells = self.number_cells;
        let left = self.bounds.origin.x;
        let (line_bg, gutter_bg) = self.tints(kind);
        if let Some(bg) = line_bg {
            self.fill_row(y, bg);
        }
        let gutter = self.gutter_cells();
        if let Some(bg) = gutter_bg {
            let w = self.cells(gutter);
            self.fill(left, y, w, bg);
        }
        self.blame(file, new, y);
        let clip = self.row_clip(y);
        let numbers_x = left + self.cells(self.blame_cells + 1);
        self.number(old, numbers_x, number_cells, y, clip);
        self.number(new, numbers_x + self.cells(number_cells + 1), number_cells, y, clip);
        let rule_x = left + self.cells(gutter - 1) + self.metrics.width * 0.5;
        let rule = self.colors.rule;
        self.frame.fills.push((Bounds::new(point(rule_x, y), size(px(1.0), self.row_h)), rule));
        self.sign(kind, left + self.cells(gutter), y, clip);
        let text_x = left + self.cells(gutter + 2);
        let right = left + self.bounds.size.width;
        self.code(&side, text_x, right, y);
    }

    fn split_row(&mut self, file: u32, hunk: u32, old: Option<u32>, new: Option<u32>, y: Pixels) {
        let left = self.bounds.origin.x;
        let blame = self.cells(self.blame_cells);
        let middle = left + blame + (self.bounds.size.width - blame) * 0.5;
        let right = left + self.bounds.size.width;
        let clip = self.row_clip(y);
        let new_number = new.and_then(|n| self.state.line(file, hunk, n)).and_then(|l| l.new);
        self.blame(file, new_number, y);
        let number_cells = self.number_cells;
        // A row's cursor column is on the side the state reads: new, else old.
        let caret_old = new.is_none();
        for (index, from, to, is_old) in [(old, left + blame, middle, true), (new, middle, right, false)] {
            let Some(index) = index else {
                continue;
            };
            let Some(side) = self.side(file, hunk, index, if is_old { caret_old } else { true }) else { continue };
            let (kind, number) = (side.line.kind, if is_old { side.line.old } else { side.line.new });
            let (line_bg, gutter_bg) = self.tints(kind);
            if let Some(bg) = line_bg {
                self.fill(from, y, to - from, bg);
            }
            if let Some(bg) = gutter_bg {
                let w = self.cells(number_cells + 2);
                self.fill(from, y, w, bg);
            }
            self.number(number, from + self.metrics.width, number_cells, y, clip);
            self.sign(kind, from + self.cells(number_cells + 2), y, clip);
            self.code(&side, from + self.cells(number_cells + 4), to, y);
        }
        let rule = self.colors.rule;
        self.frame.fills.push((Bounds::new(point(middle, y), size(px(1.0), self.row_h)), rule));
    }

    fn tints(&self, kind: LineKind) -> (Option<Hsla>, Option<Hsla>) {
        match kind {
            LineKind::Context => (None, None),
            LineKind::Added => (Some(self.colors.add_line), Some(self.colors.add_gutter)),
            LineKind::Deleted => (Some(self.colors.del_line), Some(self.colors.del_gutter)),
        }
    }

    fn number(&mut self, number: Option<u32>, x: Pixels, cells: usize, y: Pixels, clip: Bounds<Pixels>) {
        let Some(number) = number else { return };
        let text = number.to_string();
        let pad = cells.saturating_sub(text.len());
        self.plain(text, self.colors.muted, x + self.cells(pad), y, clip);
    }

    fn sign(&mut self, kind: LineKind, x: Pixels, y: Pixels, clip: Bounds<Pixels>) {
        match kind {
            LineKind::Context => {}
            LineKind::Added => {
                self.plain("+", self.colors.add, x, y, clip);
            }
            LineKind::Deleted => {
                self.plain("-", self.colors.del, x, y, clip);
            }
        }
    }

    fn blame(&mut self, file: u32, line: Option<u32>, y: Pixels) {
        if self.blame_cells == 0 {
            return;
        }
        let Some(line) = line else { return };
        let Some(path) = self.state.file(file).map(|f| f.path.as_str()) else { return };
        let Some(blame) = self.state.blame_for(path) else { return };
        let Ok(at) = blame.binary_search_by_key(&line, |b: &BlameLine| b.line) else { return };
        let text = blame_text(&blame[at], self.now);
        let clip = self.row_clip(y);
        self.plain(text, self.colors.muted, self.bounds.origin.x + self.metrics.width, y, clip);
    }

    /// A line's code from `x`, up to `right`: its word and search marks, then
    /// its syntax-colored text, moved by the sideways scroll.
    fn code(&mut self, side: &Side<'a>, x: Pixels, right: Pixels, y: Pixels) {
        let shown = Shown::of(&side.line.text);
        let clip = Bounds::new(point(x, y), size((right - x).max(px(0.0)), self.row_h));
        self.text_w = self.text_w.max(clip.size.width);
        let runs = self.code_runs(&shown, side);
        let text: SharedString = shown.text.clone().into();
        let line = self.window.text_system().shape_line(text, self.metrics.font_size, &runs, None);
        self.widest = self.widest.max(line.width);
        let origin = x - self.scroll_x;
        let mark = |this: &mut Self, range: Range<usize>, color: Hsla| {
            if range.is_empty() {
                return;
            }
            let start = (origin + line.x_for_index(range.start)).max(x);
            let end = (origin + line.x_for_index(range.end)).min(right);
            if end > start {
                this.frame.fills.push((Bounds::new(point(start, y), size(end - start, this.row_h)), color));
            }
        };
        let word = match side.line.kind {
            LineKind::Added => Some(self.colors.add_word),
            LineKind::Deleted => Some(self.colors.del_word),
            LineKind::Context => None,
        };
        if let Some(word) = word {
            for range in &side.line.intraline {
                mark(self, shown.range(range), word);
            }
        }
        let on_cursor = self.is_cursor(y);
        let col = self.state.col();
        for m in self.state.matches_on(side.file, side.hunk, side.index).to_vec() {
            let current = on_cursor && side.has_caret && m.start as usize == col;
            let color = if current { self.colors.search_current } else { self.colors.search };
            mark(self, shown.range(&(m.start..m.end)), color);
        }
        if on_cursor && side.has_caret && col > 0 && self.focused {
            let at = shown.at(col);
            let caret = origin + line.x_for_index(at);
            if caret >= x && caret < right {
                let bar = self.colors.cursor_bar;
                self.frame.fills.push((Bounds::new(point(caret, y), size(px(2.0), self.row_h)), bar));
            }
        }
        self.frame.texts.push(Text { line, origin: point(origin, y), clip });
    }

    fn code_runs(&self, shown: &Shown, side: &Side<'a>) -> Vec<TextRun> {
        let spans = side
            .syntax
            .iter()
            .map(|span| (shown.range(&span.range), syntax_color(self.theme, span.class)));
        stretches(shown.text.len(), spans)
            .into_iter()
            .map(|(len, color)| self.run(len, color.unwrap_or(self.colors.text)))
            .collect()
    }

    /// Bring the cursor's column into view when a find or a search moved it.
    fn scroll_to_col(&mut self) {
        let col = self.state.col();
        if col == 0 {
            return;
        }
        let Some(row) = self.state.table().rows.get(self.state.cursor()) else { return };
        let (file, hunk, index, split) = match *row {
            Row::Line { file, hunk, line } => (file, hunk, line, false),
            Row::Split { file, hunk, old, new } => match new.or(old) {
                Some(index) => (file, hunk, index, true),
                None => return,
            },
            _ => return,
        };
        let Some(line) = self.state.line(file, hunk, index) else { return };
        let shown = Shown::of(&line.text);
        let chars = shown.text[..shown.at(col)].chars().count();
        let x = self.cells(chars);
        let room = if split {
            (self.bounds.size.width - self.cells(self.blame_cells)) * 0.5 - self.cells(self.number_cells + 4)
        } else {
            self.bounds.size.width - self.cells(self.gutter_cells() + 2)
        };
        if x < self.scroll_x {
            self.scroll_x = (x - self.cells(4)).max(px(0.0));
        } else if x >= self.scroll_x + room - self.cells(2) {
            self.scroll_x = (x - room + self.cells(8)).max(px(0.0));
        }
    }

    /// Where the view is in the rows, at the right edge. Only an indication:
    /// the wheel and the keys do the scrolling.
    fn scrollbar(&mut self) {
        let total = self.state.table().len();
        let shown = self.state.visible_rows().len();
        if total <= shown || total == 0 {
            return;
        }
        let height = self.bounds.size.height;
        let thumb = (height * (shown as f32 / total as f32)).max(px(24.0));
        let y = self.bounds.origin.y + (height - thumb) * (self.state.top() as f32 / (total - shown) as f32);
        let x = self.bounds.origin.x + self.bounds.size.width - px(5.0);
        self.frame.fills.push((Bounds::new(point(x, y), size(px(3.0), thumb)), self.colors.scroll_thumb));
    }
}

/// A blame gutter's words.
pub fn blame_text(blame: &BlameLine, now: i64) -> String {
    if blame.uncommitted {
        return "not committed yet".to_string();
    }
    let author: String = blame.author.chars().take(12).collect();
    format!("{:<7} {:<12} {}", blame.short, author, age(blame.time, now))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_blame_gutter_fits_its_cells() {
        let line = |uncommitted| BlameLine {
            line: 1,
            short: "abc1234".into(),
            author: "A very long author name".into(),
            time: 0,
            summary: String::new(),
            uncommitted,
        };
        let text = blame_text(&line(false), 86_400 * 3);
        assert_eq!(text, "abc1234 A very long  3d");
        assert!(text.chars().count() < BLAME_CELLS);
        assert_eq!(blame_text(&line(true), 0), "not committed yet");
    }

    #[test]
    fn a_rename_shows_both_paths() {
        let cs = shelbi_app::diff::parse::parse_patch(
            "diff --git a/old.rs b/new.rs\nsimilarity index 100%\nrename from old.rs\nrename to new.rs\n",
        );
        assert_eq!(file_title(&cs[0]), "old.rs → new.rs");
    }

    #[test]
    fn a_click_finds_its_row() {
        let geometry = BodyGeometry {
            bounds: Bounds::new(point(px(100.0), px(50.0)), size(px(400.0), px(200.0))),
            row_h: px(20.0),
            cell_w: px(8.0),
            max_scroll_x: px(0.0),
        };
        assert_eq!(geometry.row_at(point(px(120.0), px(50.0)), 0), Some(0));
        assert_eq!(geometry.row_at(point(px(120.0), px(95.0)), 10), Some(12));
        assert_eq!(geometry.row_at(point(px(20.0), px(95.0)), 10), None, "outside");
        assert!(geometry.on_chevron(point(px(110.0), px(60.0))));
        assert!(!geometry.on_chevron(point(px(200.0), px(60.0))));
    }
}
