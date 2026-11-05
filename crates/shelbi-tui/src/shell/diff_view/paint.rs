//! Drawing the diff view into a ratatui buffer. Pure over a [`DiffState`], so a
//! render test needs no session and no terminal. Only the rows in view are
//! looked at, whatever the size of the diff.
//!
//! Colors go through [`Colors`], which keeps them to what the terminal can
//! show: every RGB value becomes its nearest xterm-256 index when the terminal
//! doesn't advertise truecolor.

use std::ops::Range;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Clear, Widget};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use shelbi_app::diff::draft::{CommentDraft, DraftTarget};
use shelbi_app::diff::help::help_sections;
use shelbi_app::diff::highlight::{SyntaxClass, SyntaxSpan};
use shelbi_app::diff::palette::{DiffPalette, PaletteMode};
use shelbi_app::diff::pr::review_title;
use shelbi_app::diff::{
    DiffLine, DiffState, FileDiff, FileStatus, LineKind, Mode, PrPart, PrSpan, PrTone, Row, ThreadPart,
};
use shelbi_state::keymap::{format_chord, DisplayStyle, GlobalAction, Keymaps};

use crate::overlay::{centered_rect, dialog_content, dialog_frame};
use crate::theme::{diff_tints, fit_color, palette};

/// Below this width the file list starts hidden: the diff needs the room. The
/// toggle still opens it.
pub(super) const NARROW_BELOW: u16 = 70;

/// Where a tab stop falls.
const TAB: usize = 4;

/// The fewest columns a line number takes.
const MIN_DIGITS: usize = 3;

/// A comment box's most text lines at once.
const DRAFT_LINES: usize = 5;

/// What's where.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct DiffLayout {
    /// The file list's frame, when it shows.
    pub list: Option<Rect>,
    /// The commit picker's row, inside the list.
    pub commits: Option<Rect>,
    /// The file rows, inside the list.
    pub files: Rect,
    /// The diff's frame.
    pub main: Rect,
    /// The diff's rows, inside the frame.
    pub rows: Rect,
    pub status: Rect,
}

/// Split `area`: the status line along the bottom, the list and the diff above.
pub(super) fn layout(area: Rect, list_open: bool) -> DiffLayout {
    let status_h = u16::from(area.height > 0);
    let status = Rect::new(area.x, area.bottom().saturating_sub(status_h), area.width, status_h);
    let body = Rect::new(area.x, area.y, area.width, area.height.saturating_sub(status_h));
    let list_w = if list_open { (body.width / 3).clamp(18, 34).min(body.width / 2) } else { 0 };
    let main = Rect::new(body.x + list_w, body.y, body.width - list_w, body.height);
    if list_w == 0 {
        return DiffLayout { list: None, commits: None, files: Rect::default(), main, rows: dialog_content(main), status };
    }
    let list = Rect::new(body.x, body.y, list_w, body.height);
    let inner = dialog_content(list);
    let commits_h = u16::from(inner.height > 0);
    DiffLayout {
        list: Some(list),
        commits: Some(Rect::new(inner.x, inner.y, inner.width, commits_h)),
        files: Rect::new(inner.x, inner.y + commits_h, inner.width, inner.height - commits_h),
        main,
        rows: dialog_content(main),
        status,
    }
}

/// The view's colors, for this frame's theme and terminal.
pub(super) struct Colors {
    pub add_bg: Color,
    pub del_bg: Color,
    pub add_word: Color,
    pub del_word: Color,
    pub add_fg: Color,
    pub del_fg: Color,
    pub fg: Color,
    pub muted: Color,
    pub accent: Color,
    pub selection: Color,
    pub search: Color,
    pub search_fg: Color,
    pub header: Color,
    pub dialog: Color,
    pub thread: Color,
    truecolor: bool,
}

impl Colors {
    pub fn new(truecolor: bool) -> Self {
        let p = palette();
        let tints = diff_tints(truecolor);
        let fit = |color| fit_color(color, truecolor);
        Colors {
            add_bg: tints.add_bg,
            del_bg: tints.del_bg,
            add_word: tints.add_word,
            del_word: tints.del_word,
            add_fg: fit(p.ansi_green),
            del_fg: fit(p.ansi_red),
            fg: fit(p.foreground),
            muted: fit(p.ansi_dark_gray),
            accent: fit(p.cyan),
            selection: fit(p.selection),
            search: fit(p.search),
            search_fg: fit(p.on_color),
            header: fit(p.code_bg),
            dialog: fit(p.dialog),
            thread: fit(p.yellow),
            truecolor,
        }
    }

    /// The color a pull request's words read in.
    fn tone(&self, tone: PrTone) -> Color {
        match tone {
            PrTone::Normal | PrTone::Loud => self.fg,
            PrTone::Muted => self.muted,
            PrTone::Good => self.add_fg,
            PrTone::Bad => self.del_fg,
            PrTone::Pending => self.thread,
        }
    }

    /// A syntax class's color. `None` keeps the line's own.
    fn syntax(&self, class: SyntaxClass) -> Option<Color> {
        let p = palette();
        let color = match class {
            SyntaxClass::Comment => p.ansi_dark_gray,
            SyntaxClass::String => p.ansi_green,
            SyntaxClass::Number | SyntaxClass::Constant => p.ansi_magenta,
            SyntaxClass::Keyword | SyntaxClass::Tag => p.ansi_blue,
            SyntaxClass::Function | SyntaxClass::Heading => p.ansi_cyan,
            SyntaxClass::Type | SyntaxClass::Attribute => p.ansi_yellow,
            SyntaxClass::Operator | SyntaxClass::Variable | SyntaxClass::Punctuation => return None,
        };
        Some(fit_color(color, self.truecolor))
    }
}

/// Everything a frame draws from.
pub(super) struct Paint<'a> {
    pub state: &'a DiffState,
    pub keymaps: &'a Keymaps,
    pub style: DisplayStyle,
    pub colors: Colors,
    pub focused: bool,
    /// Why comments or viewed marks can't be read or saved.
    pub store_error: Option<&'a str>,
    /// What an empty changeset says.
    pub empty: &'static str,
}

/// Draw the view. Returns where the terminal's cursor belongs, when something
/// is taking text.
pub(super) fn paint(p: &Paint<'_>, buf: &mut Buffer, area: Rect, at: &DiffLayout, list_top: usize) -> Option<(u16, u16)> {
    if area.width == 0 || area.height == 0 {
        return None;
    }
    if let Some(list) = at.list {
        paint_list(p, buf, list, at, list_top);
    }
    paint_frame(p, buf, at.main);
    paint_rows(p, buf, at.rows);
    let status_cursor = paint_status(p, buf, at.status);
    let overlay_cursor = match p.state.mode() {
        Mode::Help { scroll } => {
            paint_help(p, buf, area, *scroll);
            None
        }
        Mode::PrInfo { scroll } => {
            paint_pr_info(p, buf, area, *scroll);
            None
        }
        Mode::Palette(palette) => paint_palette(p, buf, area, palette),
        Mode::Comment(draft) => paint_draft(p, buf, at.rows, draft),
        _ => None,
    };
    if !p.colors.truecolor {
        fit_area(buf, area);
    }
    overlay_cursor.or(status_cursor)
}

/// Keep a frame to what a 256-color terminal can show, shared chrome included.
fn fit_area(buf: &mut Buffer, area: Rect) {
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let cell = &mut buf[(x, y)];
            cell.fg = fit_color(cell.fg, false);
            cell.bg = fit_color(cell.bg, false);
        }
    }
}

// ---- The file list ---------------------------------------------------------

/// `+42 −3`, leaving out a side that's zero.
fn counts(file: &FileDiff) -> String {
    match (file.additions, file.deletions) {
        (0, 0) => String::new(),
        (a, 0) => format!("+{a}"),
        (0, d) => format!("−{d}"),
        (a, d) => format!("+{a} −{d}"),
    }
}

fn status_color(p: &Paint<'_>, status: FileStatus) -> Color {
    match status {
        FileStatus::Added => p.colors.add_fg,
        FileStatus::Deleted => p.colors.del_fg,
        FileStatus::Renamed => p.colors.thread,
        FileStatus::Modified => p.colors.accent,
    }
}

/// As much of the end of `path` as fits `width`: whole directories first, then
/// a cut file name.
fn path_tail(path: &str, width: usize) -> String {
    if path.width() <= width {
        return path.to_string();
    }
    let mut from = path;
    while let Some((_, rest)) = from.split_once('/') {
        from = rest;
        if from.width() <= width {
            return from.to_string();
        }
    }
    let mut out = String::new();
    let mut used = 0;
    for c in from.chars() {
        let w = c.width().unwrap_or(0);
        if used + w + 1 > width {
            out.push('…');
            return out;
        }
        out.push(c);
        used += w;
    }
    out
}

fn paint_list(p: &Paint<'_>, buf: &mut Buffer, list: Rect, at: &DiffLayout, list_top: usize) {
    let changeset = p.state.changeset();
    dialog_frame(buf, list, None, Some(&format!("Files {}", changeset.files.len())));
    if let Some(row) = at.commits.filter(|r| r.height > 0) {
        let label = format!("Commits: {} ▾", p.state.scope_label());
        buf.set_stringn(row.x, row.y, label, usize::from(row.width), Style::default().fg(p.colors.muted));
    }
    let current = p.state.current_file();
    for (y, (index, file)) in (at.files.y..at.files.bottom()).zip(changeset.files.iter().enumerate().skip(list_top)) {
        let is_current = current == Some(index);
        let viewed = p.state.is_viewed(file);
        let base = if is_current { Style::default().bg(p.colors.selection) } else { Style::default() };
        buf.set_style(Rect::new(at.files.x, y, at.files.width, 1), base);
        let width = usize::from(at.files.width);
        // A file that's both current and viewed keeps its check: the row's
        // highlight already says it's current.
        let mark = if viewed { "✓" } else if is_current { "▸" } else { " " };
        let mark_color = if viewed { p.colors.add_fg } else { p.colors.accent };
        buf.set_stringn(at.files.x, y, mark, width, base.fg(mark_color));
        if width < 4 {
            continue;
        }
        let status = file.status.letter().to_string();
        buf.set_stringn(at.files.x + 2, y, status, width - 2, base.fg(status_color(p, file.status)));
        let tally = counts(file);
        let tally_w = tally.width();
        let name_room = width.saturating_sub(4 + if tally_w > 0 { tally_w + 1 } else { 0 });
        let name = path_tail(&file.path, name_room);
        let fg = if viewed { p.colors.muted } else { p.colors.fg };
        buf.set_stringn(at.files.x + 4, y, name, name_room, base.fg(fg));
        if tally_w > 0 && width > tally_w + 4 {
            let x = at.files.right().saturating_sub(tally_w as u16);
            paint_tally(p, buf, x, y, &tally, base);
        }
    }
}

/// `+a −d`, each side in its color.
fn paint_tally(p: &Paint<'_>, buf: &mut Buffer, mut x: u16, y: u16, tally: &str, base: Style) {
    for (i, part) in tally.split(' ').enumerate() {
        if i > 0 {
            x += 1;
        }
        let color = if part.starts_with('+') { p.colors.add_fg } else { p.colors.del_fg };
        buf.set_string(x, y, part, base.fg(color));
        x += part.width() as u16;
    }
}

/// The diff's frame, titled with the file in view.
fn paint_frame(p: &Paint<'_>, buf: &mut Buffer, main: Rect) {
    let room = usize::from(main.width).saturating_sub(4);
    let changeset = p.state.changeset();
    let title = match p.state.current_file().and_then(|f| changeset.files.get(f)) {
        Some(file) => {
            let tally = counts(file);
            let path = path_tail(&file.path, room.saturating_sub(tally.width() + 2));
            format!("{path}  {tally}")
        }
        None => format!("{} files", changeset.files.len()),
    };
    dialog_frame(buf, main, None, Some(&title));
}

// ---- The rows --------------------------------------------------------------

fn digits(n: u32) -> usize {
    n.max(1).ilog10() as usize + 1
}

/// How wide a file's line numbers are, so a column doesn't shift between hunks.
fn number_width(file: &FileDiff) -> usize {
    file.hunks
        .iter()
        .map(|h| digits(h.old_start + h.old_count).max(digits(h.new_start + h.new_count)))
        .max()
        .unwrap_or(0)
        .max(MIN_DIGITS)
}

fn paint_rows(p: &Paint<'_>, buf: &mut Buffer, rows: Rect) {
    if rows.width == 0 || rows.height == 0 {
        return;
    }
    let state = p.state;
    if state.table().is_empty() {
        let text = if state.loading() {
            "Loading diff…".to_string()
        } else if let Some(e) = state.error() {
            format!("Couldn't read the diff: {e} (r retries)")
        } else {
            p.empty.to_string()
        };
        let style = Style::default().fg(p.colors.muted);
        for (i, line) in wrap(&text, usize::from(rows.width.saturating_sub(2))).into_iter().enumerate() {
            let y = rows.y + rows.height / 2 + i as u16;
            if y < rows.bottom() {
                buf.set_stringn(rows.x + 1, y, line, usize::from(rows.width.saturating_sub(2)), style);
            }
        }
        return;
    }
    let selection = state.selection();
    for (i, row) in state.visible_rows().iter().enumerate() {
        let index = state.top() + i;
        let area = Rect::new(rows.x, rows.y + i as u16, rows.width, 1);
        let at = RowAt {
            area,
            cursor: index == state.cursor(),
            selected: selection.is_some_and(|(a, b)| (a..=b).contains(&index)),
        };
        paint_row(p, buf, row, at);
    }
}

/// Where a row goes, and how it stands.
#[derive(Clone, Copy)]
struct RowAt {
    area: Rect,
    cursor: bool,
    selected: bool,
}

fn paint_row(p: &Paint<'_>, buf: &mut Buffer, row: &Row, at: RowAt) {
    let state = p.state;
    match *row {
        Row::File { file, folded } => paint_file_row(p, buf, file, folded, at),
        Row::Hunk { file, hunk, folded } => {
            let Some(h) = state.file(file).and_then(|f| f.hunks.get(hunk as usize)) else { return };
            let base = row_base(p, None, at);
            buf.set_style(at.area, base);
            let mut text = h.header();
            if folded {
                text = format!("▸ {text}  ({} lines)", h.lines.len());
            }
            buf.set_stringn(at.area.x, at.area.y, text, usize::from(at.area.width), base.fg(p.colors.accent));
        }
        Row::Line { file, hunk, line } => paint_unified(p, buf, file, hunk, line, at),
        Row::Split { file, hunk, old, new } => paint_split(p, buf, file, hunk, old, new, at),
        Row::Thread { thread, part, .. } => paint_thread(p, buf, thread, part, at),
        Row::Pr { part } => paint_pr_row(p, buf, part, at),
        Row::Notice { file, kind } => {
            let Some(f) = state.file(file) else { return };
            let base = row_base(p, None, at);
            buf.set_style(at.area, base);
            buf.set_stringn(
                at.area.x + 1,
                at.area.y,
                kind.text(f),
                usize::from(at.area.width.saturating_sub(1)),
                base.fg(p.colors.muted).add_modifier(Modifier::ITALIC),
            );
        }
    }
}

fn paint_file_row(p: &Paint<'_>, buf: &mut Buffer, file: u32, folded: bool, at: RowAt) {
    let Some(f) = p.state.file(file) else { return };
    let base = if at.cursor || at.selected {
        Style::default().bg(p.colors.selection)
    } else {
        Style::default().bg(p.colors.header)
    };
    buf.set_style(at.area, base);
    let width = usize::from(at.area.width);
    let tally = counts(f);
    let check = if p.state.is_viewed(f) { "  ✓" } else { "" };
    let name = match &f.old_path {
        Some(old) => format!("{old} → {}", f.path),
        None => f.path.clone(),
    };
    let room = width.saturating_sub(2 + tally.width() + 2 + check.width());
    let name = path_tail(&name, room);
    let text = format!("{} {name}", if folded { "▸" } else { "▾" });
    let name_w = text.width();
    let bold = base.fg(p.colors.fg).add_modifier(Modifier::BOLD);
    buf.set_stringn(at.area.x, at.area.y, text, width, bold);
    let mut x = at.area.x + name_w as u16 + 2;
    if !tally.is_empty() && usize::from(x - at.area.x) + tally.width() <= width {
        paint_tally(p, buf, x, at.area.y, &tally, base);
        x += tally.width() as u16;
    }
    if !check.is_empty() && usize::from(x - at.area.x) + check.width() <= width {
        buf.set_string(x, at.area.y, check, base.fg(p.colors.add_fg));
    }
}

/// A row's background: the cursor and a selection win over a line's tint.
fn row_base(p: &Paint<'_>, kind: Option<LineKind>, at: RowAt) -> Style {
    if at.selected || (at.cursor && matches!(kind, None | Some(LineKind::Context))) {
        return Style::default().bg(p.colors.selection);
    }
    match kind {
        Some(LineKind::Added) => Style::default().bg(p.colors.add_bg),
        Some(LineKind::Deleted) => Style::default().bg(p.colors.del_bg),
        _ => Style::default(),
    }
}

/// A number column's style. The cursor's row stands out here even on a tinted
/// line, where its text keeps the tint.
fn gutter_style(p: &Paint<'_>, base: Style, at: RowAt) -> Style {
    if at.cursor {
        Style::default().bg(p.colors.selection).fg(p.colors.fg).add_modifier(Modifier::BOLD)
    } else {
        base.fg(p.colors.muted)
    }
}

fn number(n: Option<u32>, width: usize) -> String {
    match n {
        Some(n) => format!("{n:>width$}"),
        None => " ".repeat(width),
    }
}

fn marker(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Added => "+",
        LineKind::Deleted => "-",
        LineKind::Context => " ",
    }
}

fn marker_color(p: &Paint<'_>, kind: LineKind) -> Color {
    match kind {
        LineKind::Added => p.colors.add_fg,
        LineKind::Deleted => p.colors.del_fg,
        LineKind::Context => p.colors.muted,
    }
}

/// The blame column for a line of the new side: a commit, and its author when
/// there's room. Blank until the blame is read, and for a line with no new
/// number.
fn blame_text(p: &Paint<'_>, file: &FileDiff, new: Option<u32>, author_too: bool) -> Option<String> {
    if !p.state.blame_on() {
        return None;
    }
    let width = if author_too { 16 } else { 7 };
    let blank = " ".repeat(width);
    let found = new.and_then(|n| {
        let blame = p.state.blame_for(&file.path)?;
        let at = (n as usize).checked_sub(1).and_then(|i| blame.get(i)).filter(|b| b.line == n);
        at.or_else(|| blame.iter().find(|b| b.line == n))
    });
    let Some(line) = found else { return Some(blank) };
    let short: String = if line.uncommitted { "·······".to_string() } else { line.short.chars().take(7).collect() };
    if !author_too {
        return Some(format!("{short:<7}"));
    }
    let author: String = line.author.chars().take(8).collect();
    Some(format!("{short:<7} {author:<8}"))
}

/// One line of the unified view: ` 120  121 │ +text`.
fn paint_unified(p: &Paint<'_>, buf: &mut Buffer, file: u32, hunk: u32, line: u32, at: RowAt) {
    let state = p.state;
    let (Some(f), Some(l)) = (state.file(file), state.line(file, hunk, line)) else { return };
    let base = row_base(p, Some(l.kind), at);
    buf.set_style(at.area, base);
    let digits = number_width(f);
    let mut x = at.area.x;
    let y = at.area.y;
    let right = at.area.right();
    let gutter = gutter_style(p, base, at);
    if let Some(blame) = blame_text(p, f, l.new, at.area.width >= 70) {
        let w = blame.width() as u16;
        if x + w + 1 < right {
            buf.set_string(x, y, &blame, base.fg(p.colors.muted));
            x += w + 1;
        }
    }
    let numbers = format!(" {} {} ", number(l.old, digits), number(l.new, digits));
    let width = usize::from(right.saturating_sub(x));
    buf.set_stringn(x, y, &numbers, width, gutter);
    x = x.saturating_add(numbers.width() as u16);
    if x >= right {
        return;
    }
    buf.set_string(x, y, "│", base.fg(p.colors.muted));
    x += 1;
    if x + 1 >= right {
        return;
    }
    buf.set_string(x + 1, y, marker(l.kind), base.fg(marker_color(p, l.kind)));
    x += 2;
    if x < right {
        let code = LineCode { file, hunk, line, line_ref: l, base, focus: at.cursor && p.focused };
        paint_code(p, buf, x, y, right - x, &code);
    }
}

/// One row of the split view: old on the left, new on the right.
fn paint_split(
    p: &Paint<'_>,
    buf: &mut Buffer,
    file: u32,
    hunk: u32,
    old: Option<u32>,
    new: Option<u32>,
    at: RowAt,
) {
    let state = p.state;
    let Some(f) = state.file(file) else { return };
    let digits = number_width(f);
    let left_w = at.area.width.saturating_sub(1) / 2;
    let left = Rect::new(at.area.x, at.area.y, left_w, 1);
    let right = Rect::new(left.right() + 1, at.area.y, at.area.width.saturating_sub(left_w + 1), 1);
    // A context row is one line on both sides, so only the new side takes the
    // cursor; so does a pair, whose `f` and `/` read the new line.
    let cursor_on_left = new.is_none();
    paint_side(p, buf, file, hunk, old, SplitSide::Old, left, digits, at, cursor_on_left);
    let divider = if at.cursor { Style::default().bg(p.colors.selection) } else { Style::default() };
    if left.right() < at.area.right() {
        buf.set_string(left.right(), at.area.y, "│", divider.fg(p.colors.muted));
    }
    paint_side(p, buf, file, hunk, new, SplitSide::New, right, digits, at, !cursor_on_left);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SplitSide {
    Old,
    New,
}

#[allow(clippy::too_many_arguments)]
fn paint_side(
    p: &Paint<'_>,
    buf: &mut Buffer,
    file: u32,
    hunk: u32,
    line: Option<u32>,
    side: SplitSide,
    area: Rect,
    digits: usize,
    at: RowAt,
    takes_cursor: bool,
) {
    if area.width == 0 {
        return;
    }
    let state = p.state;
    let Some(l) = line.and_then(|i| state.line(file, hunk, i)) else {
        // Nothing on this side: a quiet band, so the other side reads as the
        // one that changed.
        let style = if at.selected || at.cursor { Style::default().bg(p.colors.selection) } else { Style::default() };
        buf.set_style(area, style);
        return;
    };
    let base = row_base(p, Some(l.kind), at);
    buf.set_style(area, base);
    let mut x = area.x;
    let right = area.right();
    let f = state.file(file);
    if side == SplitSide::New {
        if let Some(blame) = f.and_then(|f| blame_text(p, f, l.new, false)) {
            let w = blame.width() as u16;
            if x + w + 1 < right {
                buf.set_string(x, area.y, &blame, base.fg(p.colors.muted));
                x += w + 1;
            }
        }
    }
    let n = if side == SplitSide::Old { l.old } else { l.new };
    let text = format!("{} ", number(n, digits));
    buf.set_stringn(x, area.y, &text, usize::from(right.saturating_sub(x)), gutter_style(p, base, at));
    x = x.saturating_add(text.width() as u16);
    if x >= right {
        return;
    }
    buf.set_string(x, area.y, marker(l.kind), base.fg(marker_color(p, l.kind)));
    x += 1;
    if x < right {
        let code = LineCode {
            file,
            hunk,
            line: line.unwrap_or(0),
            line_ref: l,
            base,
            focus: at.cursor && takes_cursor && p.focused,
        };
        paint_code(p, buf, x, area.y, right - x, &code);
    }
}

/// A line's code and how to show it.
struct LineCode<'a> {
    file: u32,
    hunk: u32,
    line: u32,
    line_ref: &'a DiffLine,
    base: Style,
    /// The character cursor is on this line.
    focus: bool,
}

/// The style of each byte range: syntax, then the changed words, then search.
struct Marks<'a> {
    syntax: &'a [SyntaxSpan],
    word: &'a [Range<u32>],
    word_bg: Color,
    matches: Vec<Range<u32>>,
}

impl Marks<'_> {
    fn style_at(&self, p: &Paint<'_>, base: Style, at: u32) -> Style {
        let mut style = base.fg(p.colors.fg);
        if let Some(span) = self.syntax.iter().find(|s| s.range.contains(&at)) {
            if let Some(color) = p.colors.syntax(span.class) {
                style = style.fg(color);
            }
        }
        if self.word.iter().any(|r| r.contains(&at)) {
            style = style.bg(self.word_bg);
        }
        if self.matches.iter().any(|r| r.contains(&at)) {
            style = style.bg(p.colors.search).fg(p.colors.search_fg);
        }
        style
    }
}

/// Cells to skip so byte `col` of `text` lands inside `width`. Only the cursor's
/// line scrolls sideways: a find or a search hit past the edge stays visible.
fn skip_for(text: &str, col: usize, width: usize) -> usize {
    let mut seen = 0;
    for (i, c) in text.char_indices() {
        if i >= col {
            break;
        }
        seen += if c == '\t' { TAB - seen % TAB } else { c.width().unwrap_or(0) };
    }
    if seen + 1 < width {
        0
    } else {
        seen + 1 - width / 2
    }
}

/// Draw a line's text in `width` cells from `x`, expanding tabs and cutting the
/// end with `…`. Walks only as far as the edge, so a huge minified line costs a
/// screenful.
fn paint_code(p: &Paint<'_>, buf: &mut Buffer, x: u16, y: u16, width: u16, code: &LineCode<'_>) {
    let state = p.state;
    let line = code.line_ref;
    let marks = Marks {
        syntax: state.highlights(code.file).map_or(&[], |h| h.line(code.hunk as usize, code.line as usize)),
        word: &line.intraline,
        word_bg: if line.kind == LineKind::Deleted { p.colors.del_word } else { p.colors.add_word },
        matches: state.matches_on(code.file, code.hunk, code.line).iter().map(|m| m.start..m.end).collect(),
    };
    let width = usize::from(width);
    let cursor_col = code.focus.then(|| state.col());
    let skip = cursor_col.map_or(0, |col| skip_for(&line.text, col, width));
    let mut seen = 0;
    let mut used = 0;
    let mut run = String::new();
    let mut run_style: Option<Style> = None;
    let mut run_start = 0;
    let mut cut = false;
    for (i, c) in line.text.char_indices() {
        let (cell, w): (String, usize) = match c {
            '\t' => {
                let n = TAB - seen % TAB;
                (" ".repeat(n), n)
            }
            c if c.is_control() => ("�".to_string(), 1),
            c => (c.to_string(), c.width().unwrap_or(0)),
        };
        if seen + w <= skip && w > 0 {
            seen += w;
            continue;
        }
        if used + w > width {
            cut = true;
            break;
        }
        let mut style = marks.style_at(p, code.base, i as u32);
        if cursor_col == Some(i) {
            style = style.add_modifier(Modifier::REVERSED);
        }
        if run_style != Some(style) {
            flush(buf, x + run_start as u16, y, &mut run, run_style);
            run_style = Some(style);
            run_start = used;
        }
        run.push_str(&cell);
        seen += w;
        used += w;
    }
    flush(buf, x + run_start as u16, y, &mut run, run_style);
    let muted = code.base.fg(p.colors.muted);
    if cut && width > 0 {
        buf.set_string(x + width as u16 - 1, y, "…", muted);
    }
    if skip > 0 && width > 0 {
        buf.set_string(x, y, "…", muted);
    }
}

fn flush(buf: &mut Buffer, x: u16, y: u16, run: &mut String, style: Option<Style>) {
    if let (false, Some(style)) = (run.is_empty(), style) {
        buf.set_string(x, y, &*run, style);
    }
    run.clear();
}

/// A part of a thread: `┃ jlong: why not reuse this?`
fn paint_thread(p: &Paint<'_>, buf: &mut Buffer, thread: u32, part: ThreadPart, at: RowAt) {
    let state = p.state;
    let base = row_base(p, None, at);
    buf.set_style(at.area, base);
    let bar = base.fg(p.colors.thread);
    buf.set_stringn(at.area.x, at.area.y, "┃ ", usize::from(at.area.width), bar);
    let x = at.area.x + 2;
    let width = usize::from(at.area.width.saturating_sub(2));
    let Some(text) = state.thread_row_text(thread, part) else { return };
    let style = match part {
        ThreadPart::Summary => base.fg(p.colors.muted),
        _ => base.fg(p.colors.fg),
    };
    // A GitHub thread says where it stands at the end of its first row, ahead of
    // the words when the row's too narrow for both.
    let tag = (part == ThreadPart::Head).then(|| state.thread_tag(thread)).flatten();
    let room = tag.map_or(width, |(tag, _)| width.saturating_sub(tag.width() + 1));
    buf.set_stringn(x, at.area.y, text, room, style);
    if let Some((tag, tone)) = tag {
        let tag_x = at.area.right().saturating_sub(tag.width() as u16);
        if tag_x >= x {
            buf.set_string(tag_x, at.area.y, tag, base.fg(p.colors.tone(tone)));
        }
    }
}

/// A row of the pull request's description block.
fn paint_pr_row(p: &Paint<'_>, buf: &mut Buffer, part: PrPart, at: RowAt) {
    let base = if at.cursor || at.selected {
        Style::default().bg(p.colors.selection)
    } else if part == PrPart::Title {
        Style::default().bg(p.colors.header)
    } else {
        Style::default()
    };
    buf.set_style(at.area, base);
    paint_spans(p, buf, at.area, &p.state.pr_row(part), base);
}

/// `spans` from the left of `area`, each in its tone, cut at the edge.
fn paint_spans(p: &Paint<'_>, buf: &mut Buffer, area: Rect, spans: &[PrSpan], base: Style) {
    let mut x = area.x;
    for span in spans {
        let room = usize::from(area.right().saturating_sub(x));
        if room == 0 {
            return;
        }
        let mut style = base.fg(p.colors.tone(span.tone));
        if span.tone == PrTone::Loud {
            style = style.add_modifier(Modifier::BOLD);
        }
        buf.set_stringn(x, area.y, &span.text, room, style);
        x = x.saturating_add(span.text.width() as u16);
    }
}

// ---- The status line -------------------------------------------------------

fn paint_status(p: &Paint<'_>, buf: &mut Buffer, status: Rect) -> Option<(u16, u16)> {
    if status.height == 0 || status.width == 0 {
        return None;
    }
    let state = p.state;
    let width = usize::from(status.width);
    if let Mode::Search { query } = state.mode() {
        let text = format!("/{query}");
        buf.set_stringn(status.x, status.y, &text, width, Style::default().fg(p.colors.fg));
        return Some((status.x + (text.width().min(width.saturating_sub(1))) as u16, status.y));
    }
    let mut parts = vec![state.status()];
    if let Some(first) = state.pending() {
        parts.push(format!("{} …", format_chord(&first, p.style)));
    }
    if let Some(count) = state.count() {
        parts.push(count.to_string());
    }
    if let Some(e) = p.store_error {
        parts.push(format!("not saved: {e}"));
    }
    if let (Some(e), false) = (state.error(), state.table().is_empty()) {
        parts.push(format!("couldn't refresh: {e}"));
    }
    let text = parts.join(" · ");
    let label = state.mode().label();
    // A revert's question has to be read: it isn't the usual quiet line.
    let asking = matches!(state.mode(), Mode::Confirm(_));
    let muted = Style::default().fg(if asking { p.colors.fg } else { p.colors.muted });
    buf.set_stringn(status.x, status.y, &text, width, muted);
    if p.focused && label.width() <= width {
        buf.set_string(status.x, status.y, label, Style::default().fg(p.colors.accent).add_modifier(Modifier::BOLD));
    }
    None
}

// ---- Overlays --------------------------------------------------------------

/// A box of `width` by `height` centered in `area`, cleared and framed, with its
/// inside.
fn dialog(p: &Paint<'_>, buf: &mut Buffer, area: Rect, width: u16, height: u16, title: &str, foot: &str) -> Rect {
    paint_box(p, buf, centered_rect(area, width, height), title, foot)
}

/// Clear `rect` and frame it as every dialog is, with `foot` in the bottom
/// border.
fn paint_box(p: &Paint<'_>, buf: &mut Buffer, rect: Rect, title: &str, foot: &str) -> Rect {
    Clear.render(rect, buf);
    let inner = dialog_frame(buf, rect, Some(p.colors.dialog), Some(title));
    if !foot.is_empty() && rect.height > 1 && rect.width > 4 {
        let style = Style::default().bg(p.colors.dialog).fg(p.colors.muted);
        buf.set_stringn(rect.x + 2, rect.bottom() - 1, format!(" {foot} "), usize::from(rect.width - 4), style);
    }
    inner
}

/// The `?` overlay: every key in effect, from the live keymap, and the two ways
/// out of the view the diff's own keys hide.
fn paint_help(p: &Paint<'_>, buf: &mut Buffer, area: Rect, scroll: usize) {
    let sections = help_sections(&p.keymaps.diff, p.state.kind(), |c| format_chord(c, p.style));
    let mut rows: Vec<(String, String)> = Vec::new();
    for section in &sections {
        if !rows.is_empty() {
            rows.push((String::new(), String::new()));
        }
        rows.push((section.title.to_string(), String::new()));
        for row in &section.rows {
            let keys = if row.keys.is_empty() { "unbound".to_string() } else { row.keys.join(", ") };
            rows.push((keys, row.label.to_string()));
        }
    }
    rows.push((String::new(), String::new()));
    rows.push(("Shelbi".to_string(), String::new()));
    let focus = p.keymaps.global.first_chord_for(GlobalAction::FocusSidebar);
    let focus = focus.map_or_else(|| "Esc".to_string(), |c| format!("Esc, {}", format_chord(c, p.style)));
    rows.push((focus, "Back to the panel".to_string()));
    rows.push((
        format_chord(&shelbi_state::keymap::KeyChord::new(
            shelbi_state::keymap::Key::Char(' '),
            shelbi_state::keymap::Mods::CONTROL,
        ), p.style),
        "Shelbi's command palette".to_string(),
    ));
    let inner = dialog(p, buf, area, 72, area.height.saturating_sub(2), "Keys", "j k scroll · any key closes");
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let key_w = rows.iter().filter(|(_, label)| !label.is_empty()).map(|(k, _)| k.width()).max().unwrap_or(0);
    let key_w = key_w.min(usize::from(inner.width) / 2);
    let max_scroll = rows.len().saturating_sub(usize::from(inner.height));
    let bg = Style::default().bg(p.colors.dialog);
    for (y, (keys, label)) in (inner.y..inner.bottom()).zip(rows.iter().skip(scroll.min(max_scroll))) {
        if label.is_empty() {
            let title = bg.fg(p.colors.fg).add_modifier(Modifier::BOLD);
            buf.set_stringn(inner.x, y, keys, usize::from(inner.width), title);
            continue;
        }
        buf.set_stringn(inner.x, y, keys, key_w, bg.fg(p.colors.accent));
        let x = inner.x + 1 + key_w as u16;
        buf.set_stringn(x, y, label, usize::from(inner.right().saturating_sub(x)), bg.fg(p.colors.fg));
    }
}

/// The `i` panel: every check and review on the pull request.
fn paint_pr_info(p: &Paint<'_>, buf: &mut Buffer, area: Rect, scroll: usize) {
    let rows = p.state.pr_info_rows();
    let height = area.height.saturating_sub(2);
    let foot = "j k scroll · r read again · i close";
    let inner = dialog(p, buf, area, 80.min(area.width.saturating_sub(2)), height, "Pull request", foot);
    if inner.height == 0 || inner.width == 0 {
        return;
    }
    let first = scroll.min(rows.len().saturating_sub(usize::from(inner.height)));
    let bg = Style::default().bg(p.colors.dialog);
    for (y, row) in (inner.y..inner.bottom()).zip(rows.iter().skip(first)) {
        paint_spans(p, buf, Rect::new(inner.x, y, inner.width, 1), row, bg);
    }
}

/// The file, command and commit palette. Returns where its cursor goes.
fn paint_palette(p: &Paint<'_>, buf: &mut Buffer, area: Rect, palette: &DiffPalette) -> Option<(u16, u16)> {
    let items = palette.items(p.state.changeset());
    let (title, prompt) = match palette.mode {
        PaletteMode::Files => ("Files", ""),
        PaletteMode::Commands => ("Commands", "> "),
        PaletteMode::Commits => ("Commits", ""),
    };
    let foot = if palette.mode == PaletteMode::Files { "> for commands" } else { "" };
    let height = (items.len() as u16).saturating_add(3).min(area.height.saturating_sub(2).max(3));
    let inner = dialog(p, buf, area, 64.min(area.width.saturating_sub(2)), height, title, foot);
    if inner.height == 0 || inner.width == 0 {
        return None;
    }
    let bg = Style::default().bg(p.colors.dialog);
    let query = format!("{prompt}{}", palette.query);
    buf.set_stringn(inner.x, inner.y, &query, usize::from(inner.width), bg.fg(p.colors.fg));
    let cursor = (inner.x + (query.width() as u16).min(inner.width.saturating_sub(1)), inner.y);
    let list = Rect::new(inner.x, inner.y + 1, inner.width, inner.height - 1);
    let first = palette.selected.saturating_sub(usize::from(list.height).saturating_sub(1));
    let width = usize::from(list.width);
    if items.is_empty() && list.height > 0 {
        buf.set_stringn(list.x, list.y, "Nothing matches", width, bg.fg(p.colors.muted));
    }
    for (y, (index, item)) in (list.y..list.bottom()).zip(items.iter().enumerate().skip(first)) {
        let row = if index == palette.selected { Style::default().bg(p.colors.selection) } else { bg };
        buf.set_style(Rect::new(list.x, y, list.width, 1), row);
        let detail_w = item.detail.width();
        let room = width.saturating_sub(if detail_w > 0 { detail_w + 1 } else { 0 });
        buf.set_stringn(list.x, y, path_tail(&item.label, room), room, row.fg(p.colors.fg));
        if detail_w > 0 && width > detail_w {
            buf.set_string(list.right() - detail_w as u16, y, &item.detail, row.fg(p.colors.muted));
        }
    }
    Some(cursor)
}

/// The comment being written, along the bottom of the rows. Returns where its
/// cursor goes.
fn paint_draft(p: &Paint<'_>, buf: &mut Buffer, rows: Rect, draft: &CommentDraft) -> Option<(u16, u16)> {
    let lines = draft.area.lines();
    let height = (lines.len().min(DRAFT_LINES) as u16 + 2).min(rows.height);
    if height < 3 || rows.width < 4 {
        return None;
    }
    let rect = Rect::new(rows.x, rows.bottom() - height, rows.width, height);
    let title = match &draft.target {
        DraftTarget::New(anchor) if anchor.start == anchor.end => format!("Comment on {}:{}", anchor.path, anchor.start),
        DraftTarget::New(anchor) => format!("Comment on {}:{}-{}", anchor.path, anchor.start, anchor.end),
        DraftTarget::Edit { .. } => "Edit comment".to_string(),
        DraftTarget::Reply { .. } => "Reply".to_string(),
        DraftTarget::GithubNew(anchor) if anchor.start == anchor.end => {
            format!("GitHub comment on {}:{}", anchor.path, anchor.start)
        }
        DraftTarget::GithubNew(anchor) => format!("GitHub comment on {}:{}-{}", anchor.path, anchor.start, anchor.end),
        DraftTarget::GithubReply { .. } => "Reply on GitHub".to_string(),
        DraftTarget::Review { event } => review_title(*event),
    };
    let reviewing = matches!(draft.target, DraftTarget::Review { .. });
    let foot = match (reviewing, draft.inserting) {
        (true, true) => "Tab verdict · Enter submit · Ctrl-J new line · Esc motions",
        (true, false) => "Tab verdict · Enter submit · i insert · Esc cancel",
        (false, true) => "Enter save · Ctrl-J new line · Esc motions",
        (false, false) => "Enter save · i insert · Esc cancel",
    };
    let inner = paint_box(p, buf, rect, &title, foot);
    let (row, col) = draft.area.cursor();
    let first = row.saturating_sub(usize::from(inner.height).saturating_sub(1));
    let width = usize::from(inner.width);
    let style = Style::default().bg(p.colors.dialog).fg(p.colors.fg);
    let mut cursor = None;
    for (y, (index, line)) in (inner.y..inner.bottom()).zip(lines.iter().enumerate().skip(first)) {
        // Cut from the left when the caret would pass the edge.
        let before: usize = line.iter().take(col).map(|c| c.width().unwrap_or(0)).sum();
        let skip = if index == row && before + 1 >= width { before + 1 - width + 1 } else { 0 };
        let mut seen = 0;
        let text: String = line
            .iter()
            .filter(|c| {
                let w = c.width().unwrap_or(0);
                seen += w;
                seen > skip
            })
            .collect();
        buf.set_stringn(inner.x, y, text, width, style);
        if index == row {
            cursor = Some((inner.x + (before - skip) as u16, y));
        }
    }
    cursor
}

/// `text` broken at spaces to fit `width`.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if !line.is_empty() && line.width() + 1 + word.width() > width {
            out.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(word);
    }
    if !line.is_empty() {
        out.push(line);
    }
    out
}
