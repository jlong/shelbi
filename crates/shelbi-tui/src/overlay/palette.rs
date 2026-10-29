//! The command palette — shared overlay rendering + the in-process state.
//!
//! The palette lists and runs commands fuzzy-matched from
//! [`shelbi_palette::Entry`] values. [`render`] paints a filled panel wrapped in
//! a muted single-line border (the Figma "Terminal border") matching the design:
//! a `❯` search line with a block cursor and placeholder, a two-column results
//! list (icon + bold label, a dim description, and a right-aligned shortcut
//! hint), a full-width highlight bar on the selected row, a Projects column on
//! the right separated by whitespace, and a dim footer hint line.
//!
//! [`palette_rect`] sizes the overlay to its content: top-aligned a small fixed
//! offset below the top of the window, horizontally centered at the prior width,
//! and exactly as tall as the current results need — so the panel grows and
//! shrinks on every keystroke as the list filters, capped to the window (a long
//! list then scrolls inside the panel).
//!
//! The in-process TUI overlay uses [`Palette`] — a small state machine over the
//! registry's entries. Its focus model: typing filters and runs any command,
//! `↑`/`↓` move the selection, `→` moves focus into the Projects column (and
//! `←` back), Enter activates, Tab moves focus to the sidebar, and Escape (or
//! the palette chord) returns to the agent. Project switching and add-project
//! are driven by activating the matching registry entry, so the shell's
//! existing [`run_entry`](crate) path carries them out unchanged.
//!
//! Typing also searches the project's issues and Changelog entries (the
//! shared [`shelbi_app::overlay::palette_search`]): while searching, the
//! Projects column steps aside, the filter row (All / Issues / Changelog
//! Entries / Commands with their counts, the settings' segmented control)
//! sits under the search line, and the results are grouped by kind with a
//! muted header per group.

use std::time::Instant;

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_palette::{Entry, EntryKind};
use shelbi_state::keymap::PaletteAction;

// The palette's state machine and project indicators are shared with the
// desktop app (`shelbi_app::overlay::palette`).
pub use shelbi_app::overlay::palette::{
    project_indicator, PaletteFocus as Focus, PaletteStep, ProjectEntry, ProjectIndicator,
};
use shelbi_app::overlay::palette as shared;
pub use shelbi_app::overlay::palette_search::{
    FilterChip, PaletteSearch, ResultItem, ResultKind, ResultRow, SearchCorpus, SearchFilter,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::palette;
use crate::decoration_to_color;
use crate::sidebar::{bleed_block_lines, nav_lines};
use crate::theme;

// ---------------------------------------------------------------------------
// Layout constants (display columns)
// ---------------------------------------------------------------------------

/// Display width reserved for a command row's icon slot: a leading space, the
/// glyph padded to two cells (emoji are double-width), and a trailing space.
const ICON_FIELD_W: usize = 4;
/// Display width of a command row's label column, so every description starts
/// at the same x. Mirrors the Figma's fixed 211px label column.
const LABEL_W: usize = 22;
/// Whitespace gap between the commands column and the Projects column.
const COLUMN_GAP: u16 = 2;
/// The Projects column's clamped width range.
const PROJECTS_MIN_W: u16 = 18;
const PROJECTS_MAX_W: u16 = 30;
/// Below this commands-column width, the Projects column is dropped so nothing
/// overlaps on a narrow terminal (the column collapses, per the design note).
const COMMANDS_MIN_W: u16 = 30;

/// Minimum panel width (display cols). Mirrors the prior 70%/min-40 rule so the
/// panel stays usable on a small terminal.
const PALETTE_MIN_W: u16 = 40;
/// Chrome rows framing the results band: the top and bottom border (2), the
/// search line (1), one blank row above and below the band (2), and the footer
/// (1). Added to the band height to size the whole panel.
const CHROME_ROWS: u16 = 6;
/// Rows the filter row adds while searching: the segmented control's three
/// (its `▄` cap, the chips, its `▀` cap), in place of the blank under the
/// search line plus two more.
const FILTER_ROWS: u16 = 2;

// ---------------------------------------------------------------------------
// Project indicators (Projects column status glyphs)
// ---------------------------------------------------------------------------

/// Leading status glyph + color for a project row, given its indicator and the
/// current pulse `phase`. The Figma uses a filled `●` for the active project, a
/// `○` ring for every other (green when loaded, gray when not).
pub fn project_status_style(indicator: ProjectIndicator, phase: f32) -> (&'static str, Color) {
    let color = match indicator {
        ProjectIndicator::Active => theme::project_pulse_color(phase),
        ProjectIndicator::LoadedIdle => palette().green,
        ProjectIndicator::Unloaded => palette().ansi_dark_gray,
    };
    (indicator.glyph(), color)
}

/// Current pulse phase in `[0.0, 1.0)`, derived from how long the picker has
/// been open. Feeds [`project_status_style`] so the active indicator breathes as
/// the render loop repaints.
pub fn pulse_phase(start: Instant) -> f32 {
    let period = theme::PROJECT_PULSE_PERIOD.as_secs_f32();
    (start.elapsed().as_secs_f32() / period).fract()
}

// ---------------------------------------------------------------------------
// The view the shared renderer draws
// ---------------------------------------------------------------------------

/// One row of the Projects column: its display label and resolved indicator.
#[derive(Debug, Clone)]
pub struct ProjectRow {
    pub label: String,
    pub indicator: ProjectIndicator,
}

/// The Projects column: the project rows (a trailing "Add project" row is
/// appended by the renderer), the selected index when the column is focused,
/// and the pulse phase.
#[derive(Debug, Clone)]
pub struct ProjectsColumn {
    pub rows: Vec<ProjectRow>,
    /// `Some(i)` when the column is focused (paints the selection bar); `None`
    /// leaves it unselected. `i` may be `rows.len()` (the Add row).
    pub selected: Option<usize>,
    pub phase: f32,
}

/// The filter row while searching: its chips (each with its count) and
/// whether it has the keys.
#[derive(Debug, Clone, Copy)]
pub struct FilterRowView<'a> {
    pub chips: &'a [FilterChip],
    pub focused: bool,
}

/// Everything the shared [`render`] draws, as plain data.
pub struct PaletteView<'a> {
    pub query: &'a str,
    /// The results band's rows ([`shared::Palette::rows`]).
    pub rows: &'a [ResultRow],
    /// The selected row, an index into `rows`.
    pub selected: Option<usize>,
    /// Whether the results hold focus (dims their selection otherwise).
    pub commands_focused: bool,
    /// The Projects column, or `None` to render a single-column completion list
    /// spanning the whole width.
    pub projects: Option<ProjectsColumn>,
    /// The filter row, while searching.
    pub filters: Option<FilterRowView<'a>>,
    /// What the band says when there are no rows.
    pub empty: &'a str,
    /// The one-line footer hint.
    pub footer: &'a str,
}

/// The rows for a list of commands alone (an empty query's list).
pub fn command_rows(results: &[(Entry, u16)]) -> Vec<ResultRow> {
    results
        .iter()
        .map(|(e, _)| ResultRow::Item(ResultItem::Command(e.clone())))
        .collect()
}

/// Paint the palette into `area` (the content-sized overlay rect [`palette_rect`]
/// computes). The rect is the panel: a solid `palette().search` fill wrapped
/// in a muted single-line border, content inset one column inside the border.
pub fn render(f: &mut Frame, area: Rect, view: &PaletteView) {
    // A filled panel wrapped in the dialogs' muted single-line border (the
    // Figma "Terminal border"): the fill paints every gap as one surface.
    super::dialog_frame(f.buffer_mut(), area, Some(palette().search), None);

    let Some(rows) = panel_rows(area, view.filters.is_some()) else {
        return;
    };
    if rows.band.width == 0 || rows.band.height == 0 {
        return;
    }

    f.render_widget(Paragraph::new(search_line(view.query)), rows.search);

    if let (Some(chips_area), Some(filters)) = (rows.filters, view.filters) {
        render_filters(f, chips_area, filters);
    }

    // Results: commands column on the left, an optional Projects column on the
    // right separated by whitespace.
    let (commands_area, projects_area) = split_results(rows.band, view.projects.is_some());

    if view.rows.is_empty() {
        // A query that matches nothing shows one dim row rather than
        // collapsing the band to nothing, so the panel keeps a legible shape.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                view.empty.to_string(),
                Style::default().fg(palette().muted),
            ))),
            commands_area,
        );
    } else {
        let commands_focused = (projects_area.is_none() || view.commands_focused)
            && !view.filters.is_some_and(|f| f.focused);
        let selected = view.selected.map(|s| s.min(view.rows.len() - 1));
        // Each row gets a half-row of space above and below (the sidebar
        // nav's half-block bleed): the selected row's fill bleeds into those
        // half-rows via the `▄`/`▀` glyphs so the highlight reads as a taller
        // pill. The content lines carry the solid fill on the selected row; the
        // shared `bleed_block_lines` interleaves the bleed separators.
        let shown = selected.filter(|_| commands_focused);
        let content = build_row_lines(view.rows, commands_area.width as usize, shown);
        let lines = bleed_block_lines(content, shown, commands_area.width as usize, palette().selection);
        // Scroll the band to keep the selected pill visible when the list is
        // taller than the band (the half-block pitch doubles the row count).
        let band_h = commands_area.height as usize;
        let off = command_scroll_offset(view.rows.len(), selected.unwrap_or(0), band_h);
        let visible: Vec<Line> = lines.into_iter().skip(off).take(band_h).collect();
        f.render_widget(Paragraph::new(visible), commands_area);
    }

    if let (Some(area), Some(projects)) = (projects_area, view.projects.as_ref()) {
        render_projects_column(f, area, projects);
    }

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            view.footer.to_string(),
            Style::default().fg(palette().muted),
        ))),
        rows.footer,
    );
}

/// The filter chips' labels: each filter's name and its count (`Issues 4`).
fn chip_labels(chips: &[FilterChip]) -> Vec<String> {
    chips.iter().map(|c| format!("{} {}", c.label, c.count)).collect()
}

/// The filter row: the settings' segmented control, the chosen filter's chip
/// in the selection fill (bold while the row has the keys).
fn render_filters(f: &mut Frame, area: Rect, filters: FilterRowView) {
    let labels = chip_labels(filters.chips);
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let chosen = filters.chips.iter().position(|c| c.selected).unwrap_or(0);
    let (lines, _) = crate::button::segmented_in(
        &labels,
        chosen,
        filters.focused,
        None,
        area.width as usize,
        palette().selection,
    );
    f.render_widget(Paragraph::new(lines.to_vec()), area);
}

/// Where each chip of the filter row sits within `area`, for a click:
/// `(x, width)` per chip that fits, in [`SearchFilter::ROW`] order.
fn filter_hits(area: Rect, chips: &[FilterChip]) -> Vec<(u16, u16)> {
    let labels = chip_labels(chips);
    let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
    let (_, at) = crate::button::segmented_in(&labels, 0, false, None, area.width as usize, palette().selection);
    at.into_iter().map(|(x, w)| (area.x + x, w)).collect()
}

/// The palette panel's content rows within its overlay `area`: the search
/// line, the filter row (while searching) or a blank, the results band, a
/// blank, and the footer. One source of truth so [`render`] and the mouse
/// hit-test agree on where everything sits. The content is inset one column
/// inside the border on each side (so `x == area.x + 2`), matching the
/// Figma's padding.
#[derive(Debug, Clone, Copy)]
struct PanelRows {
    search: Rect,
    filters: Option<Rect>,
    band: Rect,
    footer: Rect,
}

fn panel_rows(area: Rect, searching: bool) -> Option<PanelRows> {
    // The dialogs' content rect: inside the border, one more column of
    // padding each side. Vertically the search line sits just under the top
    // border and the footer just above the bottom one.
    let content = super::dialog_content(area);
    let under_search = if searching { 1 + FILTER_ROWS } else { 1 };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),            // search line
            Constraint::Length(under_search), // blank, or the filter row
            Constraint::Min(1),               // results band
            Constraint::Length(1),            // blank
            Constraint::Length(1),            // footer
        ])
        .split(content);
    if rows.len() < 5 {
        return None;
    }
    Some(PanelRows {
        search: rows[0],
        filters: searching.then_some(rows[1]),
        band: rows[2],
        footer: rows[4],
    })
}

/// Rows the results band needs: the larger of the result list height and the
/// Projects column's rows. The command list uses the half-block bleed pitch —
/// `nav_lines(n)` (`2n + 1`) rows, a half-row band above and below every item
/// so the selected row's fill bleeds into a taller pill — except the empty
/// "No matching commands" state, which is a single plain row. The Projects
/// column keeps its compact one-row-per-project list: a heading, one row per
/// project, and the trailing "Add project".
fn results_band_rows(result_count: usize, project_count: Option<usize>) -> u16 {
    let commands = if result_count == 0 {
        1
    } else {
        nav_lines(result_count).min(u16::MAX as usize) as u16
    };
    let projects = project_count
        .map(|n| (n.min(u16::MAX as usize - 2) as u16) + 2)
        .unwrap_or(0);
    commands.max(projects)
}

/// The vertical scroll offset (in band rows) that keeps the selected command's
/// pill — its content row at `2*sel + 1` and the half-block bleed rows at
/// `2*sel` and `2*sel + 2` — fully inside a band `band_h` rows tall. The full
/// bleed block is `nav_lines(n)` rows; when it fits, no scrolling is needed.
fn command_scroll_offset(n: usize, sel: usize, band_h: usize) -> usize {
    let total = nav_lines(n);
    if band_h == 0 || total <= band_h {
        return 0;
    }
    // Keep the selected item's bleed-below row (`2*sel + 2`) as the last visible
    // row when we must scroll; its content and bleed-above then sit just above
    // it, so the whole pill stays on screen.
    let bot = 2 * sel + 2;
    (bot + 1).saturating_sub(band_h).min(total - band_h)
}

/// The palette's overlay rect within `area` (the main area). Horizontally
/// centered at the prior width (70%, min 40), top-aligned a small fixed offset
/// below the top of the window (about 2 rows, or 10% of the height, whichever is
/// smaller), and tall enough for exactly its current content — so the panel
/// grows and shrinks as the list filters. Capped to the window, with a matching
/// top and bottom margin, so a longer list scrolls inside the panel instead of
/// overflowing.
pub fn palette_rect(area: Rect, result_count: usize, project_count: Option<usize>, searching: bool) -> Rect {
    let offset_y = (area.height / 10).min(2);
    let width = (area.width.saturating_mul(70) / 100)
        .max(PALETTE_MIN_W)
        .min(area.width);
    let chrome = if searching { CHROME_ROWS + FILTER_ROWS } else { CHROME_ROWS };
    let desired_h = results_band_rows(result_count, project_count).saturating_add(chrome);
    let max_h = area.height.saturating_sub(offset_y.saturating_mul(2));
    let height = desired_h.min(max_h);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + offset_y;
    Rect::new(x, y, width, height)
}

/// The search line: a `❯` prompt, then a green block cursor and the dim
/// placeholder (empty query) or the typed query followed by the block cursor.
fn search_line(query: &str) -> Line<'static> {
    let prompt = Span::styled("❯ ", Style::default().fg(palette().foreground));
    let cursor = Style::default().bg(palette().green).fg(palette().on_color);
    if query.is_empty() {
        const PLACEHOLDER: &str = "Type a command or search";
        let mut chars = PLACEHOLDER.chars();
        let first: String = chars.by_ref().take(1).collect();
        let rest: String = chars.collect();
        Line::from(vec![
            prompt,
            Span::styled(first, cursor),
            Span::styled(rest, Style::default().fg(palette().muted)),
        ])
    } else {
        Line::from(vec![
            prompt,
            Span::styled(query.to_string(), Style::default().fg(palette().foreground)),
            Span::styled(" ", cursor),
        ])
    }
}

/// Split the results area into the commands column and an optional Projects
/// column. The Projects column is dropped when the commands column would fall
/// below a usable width, so a narrow terminal collapses to a single column
/// rather than overlapping.
fn split_results(area: Rect, want_projects: bool) -> (Rect, Option<Rect>) {
    if !want_projects {
        return (area, None);
    }
    let proj_w = (area.width / 3).clamp(PROJECTS_MIN_W, PROJECTS_MAX_W);
    if area.width < proj_w + COLUMN_GAP + COMMANDS_MIN_W {
        return (area, None);
    }
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(COLUMN_GAP),
            Constraint::Length(proj_w),
        ])
        .split(area);
    (cols[0], Some(cols[2]))
}

/// The icon glyph + color for a command row. A decoration wins (the nav emoji);
/// otherwise the entry-kind glyph, with the action lightning tinted yellow to
/// read as the Figma's `⚡` and the quieter kind glyphs left gray.
fn icon_for(e: &Entry) -> (String, Color) {
    match &e.decoration {
        Some(d) => (d.glyph.clone(), decoration_to_color(d.color)),
        None => {
            let glyph = e.kind.icon();
            let color = if glyph == EntryKind::Action.icon() {
                palette().ansi_yellow
            } else {
                palette().ansi_dark_gray
            };
            (glyph.to_string(), color)
        }
    }
}

/// Build the results band's content lines, one per row: a command, an
/// issue, a Changelog entry or a "Show all" row, or a group's muted header.
/// `selected` is the highlighted row when the results hold focus.
fn build_row_lines(rows: &[ResultRow], row_width: usize, selected: Option<usize>) -> Vec<Line<'static>> {
    rows.iter()
        .enumerate()
        .map(|(i, row)| {
            let is_selected = selected == Some(i);
            let mut line = match row {
                ResultRow::Header(kind) => header_line(*kind, row_width),
                ResultRow::Item(ResultItem::Command(e)) => command_line(e, row_width, is_selected),
                ResultRow::Item(ResultItem::Issue(d)) => detail_line(
                    crate::kanban::category_glyph(d.category),
                    crate::kanban::category_color(d.category),
                    &d.title,
                    &d.detail(),
                    row_width,
                    is_selected,
                ),
                ResultRow::Item(ResultItem::Changelog(h)) => detail_line(
                    shelbi_app::command_model::nav_glyph("Changelog"),
                    palette().foreground,
                    &h.title,
                    &h.detail,
                    row_width,
                    is_selected,
                ),
                ResultRow::Item(ResultItem::ShowAll { kind, count }) => {
                    show_all_line(&kind.show_all_label(*count), row_width, is_selected)
                }
            };
            // Bake the solid fill into every span on the selected row so it
            // reads as one continuous bar (and the bleed half-rows match).
            if is_selected {
                for s in &mut line.spans {
                    s.style = s.style.bg(palette().selection);
                }
            }
            line
        })
        .collect()
}

/// The icon field: a space, the glyph padded to two cells, a space.
fn icon_field(glyph: &str) -> String {
    let glyph_w = UnicodeWidthStr::width(glyph);
    format!(" {glyph}{} ", " ".repeat(2usize.saturating_sub(glyph_w)))
}

/// A row's label style: bold white when selected, else the foreground.
fn label_style(selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().foreground)
    }
}

/// A group's header under All: its name, muted and bold, where a label
/// would start.
fn header_line(kind: ResultKind, width: usize) -> Line<'static> {
    let text = truncate_to_width(&format!(" {}", kind.heading()), width);
    let pad = width.saturating_sub(UnicodeWidthStr::width(text.as_str()));
    Line::from(vec![
        Span::styled(text, Style::default().fg(palette().muted).add_modifier(Modifier::BOLD)),
        Span::raw(" ".repeat(pad)),
    ])
}

/// An issue's or a Changelog entry's row: its glyph, the title (cut with `…`
/// to leave room), and the dim detail (`in progress · #1931`) at the right.
fn detail_line(glyph: &str, glyph_color: Color, title: &str, detail: &str, width: usize, selected: bool) -> Line<'static> {
    let rem = width.saturating_sub(ICON_FIELD_W);
    let detail_w = UnicodeWidthStr::width(detail).min(rem / 2);
    let detail = truncate_with_ellipsis(detail, detail_w);
    let detail_w = UnicodeWidthStr::width(detail.as_str());
    let title_room = rem.saturating_sub(detail_w + usize::from(detail_w > 0) * 2);
    let title = truncate_with_ellipsis(title, title_room);
    let filler = rem.saturating_sub(UnicodeWidthStr::width(title.as_str()) + detail_w);
    Line::from(vec![
        Span::styled(icon_field(glyph), Style::default().fg(glyph_color)),
        Span::styled(title, label_style(selected)),
        Span::raw(" ".repeat(filler)),
        Span::styled(detail, Style::default().fg(palette().muted)),
    ])
}

/// A capped group's last row: `Show all 12 issues`, in the accent, where a
/// label would start.
fn show_all_line(label: &str, width: usize, selected: bool) -> Line<'static> {
    let label = truncate_with_ellipsis(label, width.saturating_sub(ICON_FIELD_W));
    let pad = width.saturating_sub(ICON_FIELD_W + UnicodeWidthStr::width(label.as_str()));
    let style = if selected {
        label_style(true)
    } else {
        Style::default().fg(palette().ansi_cyan)
    };
    Line::from(vec![
        Span::raw(" ".repeat(ICON_FIELD_W)),
        Span::styled(label, style),
        Span::raw(" ".repeat(pad)),
    ])
}

/// The command list's content lines, as [`build_row_lines`] draws them.
#[cfg(test)]
fn build_result_lines(
    results: &[(Entry, u16)],
    row_width: usize,
    selected: Option<usize>,
) -> Vec<Line<'static>> {
    build_row_lines(&command_rows(results), row_width, selected)
}

/// One command's row: a fixed-width icon slot, a fixed-width label (bold
/// white on the selected row, else foreground), a dim description aligned to
/// a fixed x and truncated with `…` when it won't fit, and a right-aligned dim
/// shortcut hint, padded to `row_width` so the selection fill spans the whole
/// commands column ([`build_row_lines`] bakes the fill in; the shared
/// [`bleed_block_lines`] then bleeds it into the half-rows above and below).
fn command_line(e: &Entry, row_width: usize, is_selected: bool) -> Line<'static> {
    let (glyph, glyph_color) = icon_for(e);

    // Label field padded to a fixed display width.
    let label = truncate_to_width(&e.label, LABEL_W);
    let label_pad = LABEL_W.saturating_sub(UnicodeWidthStr::width(label.as_str()));

    let mut spans = vec![
        Span::styled(icon_field(&glyph), Style::default().fg(glyph_color)),
        Span::styled(format!("{label}{}", " ".repeat(label_pad)), label_style(is_selected)),
    ];

    // Remaining width after the icon slot and label column holds a gap,
    // the description, filler, and the right-aligned shortcut.
    let remaining = row_width.saturating_sub(ICON_FIELD_W + LABEL_W);
    let gap = 1usize.min(remaining);
    let rem = remaining - gap;
    let short = e.shortcut.clone().unwrap_or_default();
    let short_w = UnicodeWidthStr::width(short.as_str());
    // When a shortcut is present it sits at the right edge with one
    // space before it.
    let short_block = if short_w > 0 { short_w + 1 } else { 0 };
    let desc_avail = rem.saturating_sub(short_block);
    let desc = truncate_with_ellipsis(e.subtitle.as_deref().unwrap_or(""), desc_avail);
    let desc_w = UnicodeWidthStr::width(desc.as_str());
    let filler = rem.saturating_sub(desc_w).saturating_sub(short_block);

    if gap > 0 {
        spans.push(Span::raw(" ".repeat(gap)));
    }
    spans.push(Span::styled(desc, Style::default().fg(palette().muted)));
    if short_w > 0 {
        spans.push(Span::raw(" ".repeat(filler + 1)));
        spans.push(Span::styled(short, Style::default().fg(palette().muted)));
    } else if filler > 0 {
        spans.push(Span::raw(" ".repeat(filler)));
    }

    Line::from(spans)
}

/// Render the Projects column into `area`: a dim "Projects" heading, then one
/// row per project — the status glyph and the name, with no leading bullet —
/// closed by a trailing "+ Add project" row. Separated from the commands by
/// whitespace, with no border line.
fn render_projects_column(f: &mut Frame, area: Rect, col: &ProjectsColumn) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Projects",
            Style::default().fg(palette().muted),
        ))),
        rows[0],
    );

    let width = area.width as usize;
    let mut items: Vec<ListItem> = col
        .rows
        .iter()
        .map(|p| {
            let (glyph, glyph_color) = project_status_style(p.indicator, col.phase);
            project_row_line(glyph, glyph_color, &p.label, width)
        })
        .collect();
    items.push(project_row_line(
        "+",
        palette().ansi_dark_gray,
        "Add project",
        width,
    ));

    // Selection patches background + weight only (never a foreground color) so a
    // loaded project's green ring keeps its status color under the bar.
    let highlight = Style::default()
        .bg(palette().selection)
        .add_modifier(Modifier::BOLD);
    let list = List::new(items).highlight_style(highlight);
    let mut s = ListState::default();
    if let Some(sel) = col.selected {
        let max = col.rows.len(); // the Add row is index rows.len()
        s.select(Some(sel.min(max)));
    }
    f.render_stateful_widget(list, rows[1], &mut s);
}

/// One Projects-column row: `<glyph> <label>`, padded to `width` so a focused
/// selection bar spans the whole column. The leading status glyph keeps its
/// tint; there is no decorative bullet in front of it.
fn project_row_line(glyph: &str, glyph_color: Color, label: &str, width: usize) -> ListItem<'static> {
    let text_w = UnicodeWidthStr::width(glyph)
        + 1 // space
        + UnicodeWidthStr::width(label);
    let pad = width.saturating_sub(text_w);
    ListItem::new(Line::from(vec![
        Span::styled(glyph.to_string(), Style::default().fg(glyph_color)),
        Span::styled(format!(" {label}"), Style::default().fg(palette().foreground)),
        Span::raw(" ".repeat(pad)),
    ]))
}

/// Truncate `s` to at most `max` display columns, dropping whole characters.
fn truncate_to_width(s: &str, max: usize) -> String {
    let mut width = 0;
    let mut out = String::new();
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if width + cw > max {
            break;
        }
        width += cw;
        out.push(c);
    }
    out
}

/// Truncate `s` to `max` display columns, appending `…` when it didn't fit.
fn truncate_with_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut out = truncate_to_width(s, max.saturating_sub(1));
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// The in-process overlay state
// ---------------------------------------------------------------------------

/// The in-process command palette overlay: the shared state machine
/// ([`shared::Palette`]) plus when it opened (the active project's pulse phase).
/// The host (the shell) supplies the entries from
/// [`shelbi_app::CommandRegistry::entries`] and the projects, and turns an
/// activated [`Entry`] into a command effect.
pub struct Palette {
    inner: shared::Palette,
    opened_at: Instant,
}

impl Palette {
    /// Open the palette over `entries`, with `projects` listed in the Projects
    /// column (empty hides the column).
    pub fn new(entries: Vec<Entry>, projects: Vec<ProjectEntry>) -> Self {
        Self {
            inner: shared::Palette::new(entries, projects),
            opened_at: Instant::now(),
        }
    }

    /// Search issues and Changelog entries with `search` too.
    pub fn with_search(mut self, search: PaletteSearch) -> Self {
        self.inner = self.inner.with_search(search);
        self
    }

    /// Replace the entry list (a background refresh landed) without disturbing
    /// the query; the renderer re-clamps the selection.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.inner.set_entries(entries);
    }

    /// Search fresh issues and entries (a board or Changelog read landed).
    pub fn set_corpus(&mut self, corpus: SearchCorpus) {
        self.inner.set_corpus(corpus);
    }

    /// Take search hits that landed off-thread; whether to redraw.
    pub fn poll_search(&mut self) -> bool {
        self.inner.poll_search()
    }

    /// The shared state machine (the shell's tests read its rows).
    pub fn inner(&self) -> &shared::Palette {
        &self.inner
    }

    pub fn query(&self) -> &str {
        self.inner.query()
    }

    pub fn selected(&self) -> usize {
        self.inner.selected()
    }

    /// Focus the Projects column (the bare "Switch Project").
    pub fn focus_projects(&mut self) {
        self.inner.focus_projects();
    }

    /// Whether the Projects column holds focus.
    pub fn projects_focused(&self) -> bool {
        self.inner.focus() == shared::PaletteFocus::Projects
    }

    /// The fuzzy-filtered results for the current query.
    pub fn results(&self) -> Vec<(Entry, u16)> {
        self.inner.results()
    }

    /// The overlay rect for the current content within `area`, via
    /// [`palette_rect`]. Recomputed every frame so the panel resizes as the
    /// query filters the list.
    pub fn overlay_rect(&self, area: Rect) -> Rect {
        let row_count = self.inner.rows().len();
        let projects = self.inner.projects().len();
        let project_count = (self.inner.shows_projects()).then_some(projects);
        palette_rect(area, row_count, project_count, self.inner.searching())
    }

    /// Feed one key press. `action` is the palette-mode binding the host
    /// resolved from its keymap (so Close/Activate/Nav/Backspace honor the
    /// user's `keys.yaml`); Tab, Space, the arrow keys, and unbound printable
    /// chars are handled by raw key so they behave regardless of bindings
    /// ([`shared::Palette::handle_key`]).
    pub fn handle_key(&mut self, ev: KeyEvent, action: Option<PaletteAction>) -> PaletteStep {
        match crate::keymap::chord_from_event(ev) {
            Some(chord) => self.inner.handle_key(chord, action),
            None => PaletteStep::Continue,
        }
    }

    /// Feed a left-click at terminal cell (`col`, `row`) over the palette panel
    /// `rect` (the overlay rect [`overlay_rect`] returned). A click on a filter
    /// chip shows that filter. A click anywhere on a result's band (its label
    /// row or either half-block bleed row) selects that result and activates
    /// it; a group's header does nothing. A click outside the results (the
    /// Projects column, the search line, the footer, or the blank area below a
    /// short list) leaves the palette open unchanged.
    ///
    /// [`overlay_rect`]: Palette::overlay_rect
    pub fn handle_click(&mut self, rect: Rect, col: u16, row: u16) -> PaletteStep {
        let Some(panel) = panel_rows(rect, self.inner.searching()) else {
            return PaletteStep::Continue;
        };
        if let Some(chips_area) = panel.filters {
            // The chips sit on the segmented control's middle row.
            if row == chips_area.y + 1 {
                let chips = self.inner.chips();
                for (chip, (x, w)) in chips.iter().zip(filter_hits(chips_area, &chips)) {
                    if col >= x && col < x + w {
                        self.inner.click_filter(chip.filter);
                        return PaletteStep::Continue;
                    }
                }
            }
            if row >= chips_area.y && row < chips_area.y + chips_area.height {
                return PaletteStep::Continue;
            }
        }
        let rows = self.inner.rows();
        let n = rows.len();
        if n == 0 {
            return PaletteStep::Continue;
        }
        let (commands_area, _) = split_results(panel.band, self.inner.shows_projects());
        // Horizontal + vertical bounds of the results band.
        if col < commands_area.x
            || col >= commands_area.x.saturating_add(commands_area.width)
            || row < commands_area.y
            || row >= commands_area.y.saturating_add(commands_area.height)
        {
            return PaletteStep::Continue;
        }
        // Map the clicked row back through the scroll offset to a line in the
        // full `2n + 1`-row bleed block. Each row owns a 2-row band (its
        // bleed-above and its label row), so `line / 2` resolves the row; the
        // final trailing bleed row clamps to the last one.
        let selected = self.inner.selected_row().unwrap_or(0);
        let band_h = commands_area.height as usize;
        let off = command_scroll_offset(n, selected, band_h);
        let line = (row - commands_area.y) as usize + off;
        if line >= nav_lines(n) {
            return PaletteStep::Continue;
        }
        self.inner.click_row((line / 2).min(n - 1))
    }

    /// The Projects column view, or `None` when there are no projects.
    fn projects_view(&self) -> Option<ProjectsColumn> {
        let projects = self.inner.projects();
        if !self.inner.shows_projects() {
            return None;
        }
        Some(ProjectsColumn {
            rows: projects
                .iter()
                .map(|p| ProjectRow {
                    label: p.label.clone(),
                    indicator: p.indicator,
                })
                .collect(),
            selected: self.inner.project_selection(),
            phase: pulse_phase(self.opened_at),
        })
    }

    /// Paint the overlay into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let rows = self.inner.rows();
        let chips = self.inner.chips();
        let footer = self.inner.footer("Ctrl+P");
        let empty = if self.inner.search_pending() {
            "Searching…"
        } else if self.inner.searching() {
            self.inner.filter().empty_label()
        } else {
            SearchFilter::Commands.empty_label()
        };
        let view = PaletteView {
            query: self.inner.query(),
            rows: &rows,
            selected: self.inner.selected_row(),
            commands_focused: self.inner.focus() == Focus::Commands,
            projects: self.projects_view(),
            filters: self.inner.searching().then_some(FilterRowView {
                chips: &chips,
                focused: self.inner.filter_focused(),
            }),
            empty,
            footer: &footer,
        };
        render(f, area, &view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};
    use shelbi_palette::{Decoration, DecorationColor};

    fn entry(id: &str, label: &str) -> Entry {
        Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        }
    }

    fn entry_full(id: &str, label: &str, subtitle: &str, shortcut: Option<&str>) -> Entry {
        Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::View,
            subtitle: Some(subtitle.into()),
            shortcut: shortcut.map(Into::into),
            decoration: None,
            hidden_until_query: false,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn project(slug: &str, label: &str, indicator: ProjectIndicator) -> ProjectEntry {
        ProjectEntry {
            slug: slug.into(),
            label: label.into(),
            indicator,
        }
    }

    fn dump(buf: &ratatui::buffer::Buffer) -> String {
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `y` of the first buffer row whose text contains `needle`.
    fn find_row_with(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<u16> {
        (0..buf.area.height).find(|&y| {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            row.contains(needle)
        })
    }

    /// The (col, row) of the first cell where `needle` starts (ASCII needles).
    fn find_cell(buf: &ratatui::buffer::Buffer, needle: &str) -> Option<(u16, u16)> {
        for y in 0..buf.area.height {
            let row: String = (0..buf.area.width)
                .map(|x| buf[(x, y)].symbol().to_string())
                .collect();
            if let Some(byte_idx) = row.find(needle) {
                return Some((row[..byte_idx].chars().count() as u16, y));
            }
        }
        None
    }

    fn entry_with_glyph(id: &str, label: &str, glyph: &str) -> Entry {
        Entry {
            id: id.into(),
            label: label.into(),
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: Some(Decoration {
                glyph: glyph.into(),
                color: DecorationColor::Default,
            }),
            hidden_until_query: false,
        }
    }

    #[test]
    fn emoji_icon_fields_stay_one_slot_wide_including_the_vs16_sequence() {
        // Each action emoji — scroll / flying saucer / chequered flag / racing
        // car — must occupy exactly `ICON_FIELD_W` display columns so every
        // label starts at the same x. The racing car (`🏎️`, U+1F3CE + U+FE0F)
        // is the risky one: its base code point measures a single column and
        // only the VS16 sequence widens it to an emoji's two cells. Measuring
        // the glyph as a whole string (not per code point) keeps the slot at
        // four columns; a per-char measurement would render it three columns
        // wide and shove the label one cell left.
        for (glyph, name) in [
            ("📜", "scroll"),
            ("🛸", "flying saucer"),
            ("🏁", "chequered flag"),
            ("🏎️", "racing car"),
        ] {
            let results = vec![(entry_with_glyph("action:x", "Quit Shelbi", glyph), 0u16)];
            let lines = build_result_lines(&results, 60, None);
            let icon_field = lines[0].spans[0].content.as_ref();
            assert_eq!(
                UnicodeWidthStr::width(icon_field),
                ICON_FIELD_W,
                "{name} icon field must be {ICON_FIELD_W} cols wide, got {icon_field:?}"
            );
        }
    }

    #[test]
    fn project_indicator_resolves_the_three_states_in_precedence_order() {
        assert_eq!(project_indicator(false, false), ProjectIndicator::Unloaded);
        // Not-loaded dominates a stale active flag.
        assert_eq!(project_indicator(false, true), ProjectIndicator::Unloaded);
        assert_eq!(project_indicator(true, true), ProjectIndicator::Active);
        assert_eq!(project_indicator(true, false), ProjectIndicator::LoadedIdle);
    }

    #[test]
    fn project_status_style_paints_each_state() {
        let (active_glyph, _) = project_status_style(ProjectIndicator::Active, 0.5);
        assert_eq!(active_glyph, "●");
        let (idle_glyph, idle_color) = project_status_style(ProjectIndicator::LoadedIdle, 0.0);
        assert_eq!(idle_glyph, "○");
        assert_eq!(idle_color, palette().green);
        // The unloaded project is a gray ring (design `#666`), distinguished
        // from the loaded ring by color, not glyph.
        let (unloaded_glyph, unloaded_color) =
            project_status_style(ProjectIndicator::Unloaded, 0.0);
        assert_eq!(unloaded_glyph, "○");
        assert_eq!(unloaded_color, palette().ansi_dark_gray);
    }

    #[test]
    fn active_pulse_fill_breathes_across_the_cycle() {
        let (_, trough) = project_status_style(ProjectIndicator::Active, 0.0);
        let (_, peak) = project_status_style(ProjectIndicator::Active, 0.5);
        assert_ne!(trough, peak, "the fill must cycle between phases");
    }

    #[test]
    fn typing_filters_and_resets_selection() {
        let mut p = Palette::new(
            vec![
                entry("view:tasks", "Issues"),
                entry("action:toggle-zen", "Turn Zen Mode on"),
            ],
            Vec::new(),
        );
        assert_eq!(p.results().len(), 2);
        p.inner.set_selected(1);
        for c in "zen".chars() {
            assert_eq!(p.handle_key(key(KeyCode::Char(c)), None), PaletteStep::Continue);
        }
        assert_eq!(p.query(), "zen");
        assert_eq!(p.selected(), 0);
        let hits = p.results();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, "action:toggle-zen");
    }

    /// The palette (panel fill, border, prompt + placeholder or typed query
    /// with the green cursor, command rows with every decoration color,
    /// descriptions and shortcuts, the selected row, the Projects column in
    /// each indicator state, the Add row, the footer) is readable on a light
    /// terminal, with the commands and then the projects column focused.
    #[test]
    fn renders_readable_in_light() {
        use ratatui::{backend::TestBackend, Terminal};
        use shelbi_app::theme::Appearance;
        let decorated = |id: &str, label: &str, glyph: &str, color: DecorationColor| Entry {
            decoration: Some(Decoration {
                glyph: glyph.into(),
                color,
            }),
            ..entry_full(id, label, "Jump to it", Some("⌃G"))
        };
        let results: Vec<(Entry, u16)> = vec![
            (entry_full("view:orch", "Chat", "Talk with the Orchestrator", Some("⌃O")), 0),
            (entry_full("view:tasks", "Issues", "Queue up and manage work", None), 0),
            (decorated("workspace:alpha", "alpha", "⏵", DecorationColor::Green), 0),
            (decorated("workspace:bravo", "bravo", "?", DecorationColor::Yellow), 0),
            (decorated("workspace:charlie", "charlie", "⚠", DecorationColor::Red), 0),
            (decorated("review:a", "Palette fix", "✓", DecorationColor::Cyan), 0),
            (decorated("review:b", "Nav fix", "▶", DecorationColor::Muted), 0),
            (decorated("review:c", "Copy", "·", DecorationColor::DarkGray), 0),
            (decorated("x:gray", "Gray", "•", DecorationColor::Gray), 0),
            (decorated("x:blue", "Blue", "•", DecorationColor::Blue), 0),
        ];
        let projects = |selected: Option<usize>| ProjectsColumn {
            rows: vec![
                ProjectRow {
                    label: "Website".into(),
                    indicator: ProjectIndicator::Active,
                },
                ProjectRow {
                    label: "Shelbi".into(),
                    indicator: ProjectIndicator::LoadedIdle,
                },
                ProjectRow {
                    label: "Archive".into(),
                    indicator: ProjectIndicator::Unloaded,
                },
            ],
            selected,
            // Near the pulse's peak: its trough is meant to fade into the panel.
            phase: 0.5,
        };
        crate::theme::with_appearance(Appearance::Light, || {
            for (name, query, selected, projects_selected) in [
                ("palette-commands", "", 1, None),
                ("palette-query", "ch", 2, None),
                ("palette-projects", "", 0, Some(1)),
                ("palette-add-project", "", 0, Some(3)),
            ] {
                let rows = command_rows(&results);
                let view = PaletteView {
                    query,
                    rows: &rows,
                    selected: Some(selected),
                    commands_focused: projects_selected.is_none(),
                    projects: Some(projects(projects_selected)),
                    filters: None,
                    empty: "No matching commands",
                    footer: "↑↓ select · → projects · Ctrl+P close",
                };
                let mut term = Terminal::new(TestBackend::new(100, 18)).unwrap();
                term.draw(|f| render(f, f.area(), &view)).unwrap();
                crate::theme::assert_light_readable(name, term.backend().buffer());
            }
        });
    }

    #[test]
    fn empty_query_renders_the_bordered_panel_layout() {
        use ratatui::{backend::TestBackend, Terminal};
        let p = Palette::new(
            vec![
                entry_full(
                    "view:orch",
                    "Chat",
                    "Talk with the Orchestrator to manage Shelbi",
                    None,
                ),
                entry_full("view:tasks", "Issues", "Queue up and manage work for Shelbi", None),
            ],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        let mut term = Terminal::new(TestBackend::new(100, 14)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text = dump(&buf);
        // Search line with the prompt + placeholder.
        assert!(text.contains("❯"), "prompt: {text}");
        assert!(text.contains("Type a command or search"), "placeholder: {text}");
        // Two-column command rows: label + description (the description may
        // truncate to fit the narrower commands column, so match a prefix).
        assert!(text.contains("Chat"), "label: {text}");
        assert!(
            text.contains("Talk with the Orchestrator"),
            "description: {text}"
        );
        // Projects column heading + a row.
        assert!(text.contains("Projects"), "projects heading: {text}");
        assert!(text.contains("Website"), "project row: {text}");
        // Footer advertises the new chords, no old title.
        assert!(text.contains("→ projects"), "footer: {text}");
        assert!(text.contains("Ctrl+P close"), "footer: {text}");
        assert!(!text.contains("shelbi ·"), "no title row: {text}");
        // The muted single-line border wraps the panel: corners, edges, and the
        // border color on a corner cell.
        assert!(text.contains('┌') && text.contains('┐'), "top corners: {text}");
        assert!(text.contains('└') && text.contains('┘'), "bottom corners: {text}");
        assert!(text.contains('│') && text.contains('─'), "border edges: {text}");
        assert_eq!(
            buf[(0, 0)].fg,
            palette().muted,
            "the border renders in the muted color"
        );
        assert_eq!(buf[(0, 0)].symbol(), "┌", "top-left corner glyph");
    }

    #[test]
    fn long_description_truncates_before_the_shortcut() {
        use ratatui::{backend::TestBackend, Terminal};
        // A description far too long for the row plus a shortcut: it must be
        // cut with `…` and the shortcut must survive at the right.
        let p = Palette::new(
            vec![entry_full(
                "action:toggle-zen",
                "Turn Zen Mode off",
                "Shelbi does the human parts of your workflow and then some more text",
                Some("⌥Z"),
            )],
            Vec::new(),
        );
        let mut term = Terminal::new(TestBackend::new(70, 10)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let text = dump(&buf);
        assert!(text.contains('…'), "description should truncate with an ellipsis: {text}");
        assert!(text.contains("⌥Z"), "the shortcut must stay on the row: {text}");
        // The full description must not have fit verbatim.
        assert!(
            !text.contains("your workflow and then some more text"),
            "the overflowing tail must be cut: {text}"
        );
    }

    #[test]
    fn filtered_query_narrows_the_rendered_rows() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut p = Palette::new(
            vec![
                entry_full("view:tasks", "Issues", "Queue up and manage work for Shelbi", None),
                entry_full("view:activity", "Activity", "See what's happened recently", None),
            ],
            Vec::new(),
        );
        for c in "activ".chars() {
            p.handle_key(key(KeyCode::Char(c)), None);
        }
        let mut term = Terminal::new(TestBackend::new(80, 10)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let text = dump(&term.backend().buffer().clone());
        assert!(text.contains("activ"), "the query echoes in the search line: {text}");
        assert!(text.contains("Activity"), "the matching row renders: {text}");
        assert!(!text.contains("Queue up"), "the non-matching row is filtered out: {text}");
    }

    #[test]
    fn right_focuses_projects_then_left_returns_to_commands() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues")],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        assert_eq!(p.inner.focus(), Focus::Commands);
        assert_eq!(p.handle_key(key(KeyCode::Right), None), PaletteStep::Continue);
        assert_eq!(p.inner.focus(), Focus::Projects);
        assert_eq!(p.handle_key(key(KeyCode::Left), None), PaletteStep::Continue);
        assert_eq!(p.inner.focus(), Focus::Commands);
    }

    #[test]
    fn right_is_a_noop_without_projects() {
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        p.handle_key(key(KeyCode::Right), None);
        assert_eq!(p.inner.focus(), Focus::Commands);
    }

    #[test]
    fn enter_on_a_project_activates_its_switch_entry() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues")],
            vec![
                project("website", "Website", ProjectIndicator::Active),
                project("docs", "Docs", ProjectIndicator::LoadedIdle),
            ],
        );
        p.handle_key(key(KeyCode::Right), None); // focus projects, index 0
        p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown)); // -> Docs
        match p.handle_key(key(KeyCode::Enter), Some(PaletteAction::Activate)) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "action:switch-project:docs"),
            other => panic!("expected Activate, got {other:?}"),
        }
    }

    #[test]
    fn enter_on_the_add_row_activates_add_project() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues")],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        p.handle_key(key(KeyCode::Right), None);
        // One project, so index 1 is the trailing "Add project" row.
        p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown));
        match p.handle_key(key(KeyCode::Enter), Some(PaletteAction::Activate)) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "action:add-project"),
            other => panic!("expected Activate, got {other:?}"),
        }
    }

    #[test]
    fn typing_while_in_projects_returns_focus_to_commands() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues")],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        p.handle_key(key(KeyCode::Right), None);
        assert_eq!(p.inner.focus(), Focus::Projects);
        p.handle_key(key(KeyCode::Char('i')), None);
        assert_eq!(p.inner.focus(), Focus::Commands);
        assert_eq!(p.query(), "i");
    }

    #[test]
    fn project_nav_down_clamps_to_the_add_row() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues")],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        p.handle_key(key(KeyCode::Right), None);
        // Two selectable rows (Website=0, Add=1): extra NavDowns clamp at 1.
        for _ in 0..5 {
            p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown));
        }
        assert_eq!(p.inner.project_selection(), Some(1));
    }

    #[test]
    fn tab_moves_to_the_sidebar_and_esc_closes_to_the_agent() {
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        assert_eq!(p.handle_key(key(KeyCode::Tab), None), PaletteStep::FocusSidebar);
        assert_eq!(
            p.handle_key(key(KeyCode::Esc), Some(PaletteAction::Close)),
            PaletteStep::Close
        );
    }

    #[test]
    fn activate_returns_the_selected_entry() {
        let mut p = Palette::new(
            vec![
                entry("view:tasks", "Issues"),
                entry("view:activity", "Activity"),
            ],
            Vec::new(),
        );
        assert_eq!(
            p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown)),
            PaletteStep::Continue
        );
        assert_eq!(p.selected(), 1);
        match p.handle_key(key(KeyCode::Enter), Some(PaletteAction::Activate)) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "view:activity"),
            other => panic!("expected Activate, got {other:?}"),
        }
    }

    #[test]
    fn nav_down_clamps_to_the_result_count() {
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown));
        assert_eq!(p.selected(), 0);
    }

    #[test]
    fn space_types_even_when_unbound() {
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        assert_eq!(p.handle_key(key(KeyCode::Char(' ')), None), PaletteStep::Continue);
        assert_eq!(p.query(), " ");
    }

    #[test]
    fn backspace_pops_the_query() {
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        for c in "iss".chars() {
            p.handle_key(key(KeyCode::Char(c)), None);
        }
        assert_eq!(p.query(), "iss");
        assert_eq!(
            p.handle_key(key(KeyCode::Backspace), Some(PaletteAction::Backspace)),
            PaletteStep::Continue
        );
        assert_eq!(p.query(), "is");
    }

    fn draw_projects(col: ProjectsColumn) -> ratatui::buffer::Buffer {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let view = PaletteView {
            query: "",
            rows: &[],
            selected: None,
            commands_focused: col.selected.is_none(),
            projects: Some(col),
            filters: None,
            empty: "No matching commands",
            footer: "footer",
        };
        let mut term = Terminal::new(TestBackend::new(60, 12)).unwrap();
        term.draw(|f| render(f, f.area(), &view)).unwrap();
        term.backend().buffer().clone()
    }

    #[test]
    fn projects_column_keeps_the_loaded_glyph_green_when_its_row_is_selected() {
        let buf = draw_projects(ProjectsColumn {
            rows: vec![ProjectRow {
                label: "Website".into(),
                indicator: ProjectIndicator::LoadedIdle,
            }],
            selected: Some(0),
            phase: 0.0,
        });
        let mut found = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "○" {
                    assert_eq!(
                        cell.fg,
                        palette().green,
                        "the selected loaded-idle ring must stay green"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "the loaded project's green ring should render");
    }

    #[test]
    fn projects_column_pulses_a_filled_green_disc_for_active_work() {
        let buf = draw_projects(ProjectsColumn {
            rows: vec![ProjectRow {
                label: "Website".into(),
                indicator: ProjectIndicator::Active,
            }],
            selected: None,
            phase: 0.0,
        });
        let mut found = false;
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if cell.symbol() == "●" {
                    assert!(
                        matches!(cell.fg, Color::Rgb(0, _, 0)),
                        "the active disc must render a green pulse fill"
                    );
                    found = true;
                }
            }
        }
        assert!(found, "an active project should render the filled disc");
    }

    #[test]
    fn projects_column_renders_glyphs_without_a_bullet_plus_the_add_row() {
        // All three status glyphs plus the add row — and no leading `·` bullet
        // in front of any project row.
        let buf = draw_projects(ProjectsColumn {
            rows: vec![
                ProjectRow {
                    label: "Website".into(),
                    indicator: ProjectIndicator::Active,
                },
                ProjectRow {
                    label: "Docs".into(),
                    indicator: ProjectIndicator::LoadedIdle,
                },
                ProjectRow {
                    label: "Sandbox".into(),
                    indicator: ProjectIndicator::Unloaded,
                },
            ],
            selected: None,
            phase: 0.0,
        });
        let dumped = dump(&buf);
        assert!(dumped.contains('●'), "active disc: {dumped}");
        assert!(dumped.contains('○'), "idle/unloaded ring: {dumped}");
        assert!(!dumped.contains('·'), "no leading bullet: {dumped}");
        assert!(dumped.contains("Website"), "project row: {dumped}");
        assert!(dumped.contains("Add project"), "Add project row: {dumped}");
        assert!(dumped.contains('+'), "Add row glyph: {dumped}");
        // The first project label is never dimmed/grayed when unfocused.
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                if "Website".contains(cell.symbol()) && cell.symbol() != " " {
                    assert!(
                        !cell.modifier.contains(Modifier::DIM),
                        "first project label must not be dimmed when unfocused"
                    );
                    assert_ne!(
                        cell.fg,
                        Color::Gray,
                        "first project label must not render gray when unfocused"
                    );
                }
            }
        }
    }

    // -- Sizing: top-aligned, content-height, cap/scroll ------------------

    #[test]
    fn results_band_rows_is_the_larger_of_commands_and_projects() {
        // Commands use the half-block pitch: nav_lines(n) == 2n + 1.
        assert_eq!(results_band_rows(6, None), 13);
        // An empty list still reserves one row (the "No matching commands" row).
        assert_eq!(results_band_rows(0, None), 1);
        // Projects column: heading + one row per project + the Add row (n + 2).
        // One command (3 rows) is shorter than three projects (5 rows).
        assert_eq!(results_band_rows(1, Some(3)), 5);
        // Ten commands (21 rows) dwarf the Projects column (5 rows).
        assert_eq!(results_band_rows(10, Some(3)), 21);
    }

    #[test]
    fn palette_rect_is_top_aligned_centered_and_content_sized() {
        let area = Rect::new(0, 0, 100, 40);
        // Six results, no projects: band nav_lines(6)=13 + 6 chrome rows = 19.
        let r = palette_rect(area, 6, None, false);
        assert_eq!(r.height, 19, "height fits the content");
        // Width is the prior 70%/min-40 rule, horizontally centered.
        assert_eq!(r.width, 70);
        assert_eq!(r.x, 15);
        // Top-aligned a small offset below the top: min(2, 10% of height).
        assert_eq!(r.y, 2);
    }

    #[test]
    fn palette_rect_shrinks_and_grows_with_the_result_count() {
        let area = Rect::new(0, 0, 100, 40);
        let full = palette_rect(area, 8, None, false);
        let one = palette_rect(area, 1, None, false);
        let none = palette_rect(area, 0, None, false);
        assert!(one.height < full.height, "one result is a shorter panel");
        assert_eq!(one.height, 9, "one result: 3 band rows (2*1+1) + 6 chrome");
        assert_eq!(none.height, 7, "no matches still reserves the one row");
    }

    #[test]
    fn palette_rect_caps_tall_lists_to_the_window() {
        let area = Rect::new(0, 0, 100, 20);
        // 100 results would want 106 rows; cap to the window minus the top and
        // bottom margin (offset 2 each, from 10% of 20).
        let r = palette_rect(area, 100, None, false);
        assert_eq!(r.y, 2);
        assert_eq!(r.height, 16, "capped to area.height - 2*offset");
        assert!(r.y + r.height <= area.height, "stays within the window");
    }

    #[test]
    fn palette_rect_offset_is_the_smaller_of_two_rows_and_ten_percent() {
        // Short window: 10% of 15 = 1 row, smaller than 2.
        let short = palette_rect(Rect::new(0, 0, 80, 15), 3, None, false);
        assert_eq!(short.y, 1);
        // Tall window: the offset caps at 2 rows.
        let tall = palette_rect(Rect::new(0, 0, 80, 60), 3, None, false);
        assert_eq!(tall.y, 2);
    }

    #[test]
    fn no_match_shows_a_single_dim_row() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        for c in "zzzzz".chars() {
            p.handle_key(key(KeyCode::Char(c)), None);
        }
        assert!(p.results().is_empty(), "the query matches nothing");
        let mut term = Terminal::new(TestBackend::new(60, 10)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let text = dump(&term.backend().buffer().clone());
        assert!(text.contains("No matches"), "the empty state renders one dim row: {text}");
    }

    #[test]
    fn capped_band_scrolls_to_keep_the_selection_visible() {
        use ratatui::{backend::TestBackend, Terminal};
        let results: Vec<(Entry, u16)> = (0..12)
            .map(|i| (entry(&format!("view:v{i}"), &format!("Command{i:02}")), 0u16))
            .collect();
        let rows = command_rows(&results);
        let view = PaletteView {
            query: "",
            rows: &rows,
            selected: Some(11),
            commands_focused: true,
            projects: None,
            filters: None,
            empty: "No matching commands",
            footer: "footer",
        };
        // Height 11: 6 chrome rows leave a 5-row band for 12 items, so the band
        // must scroll. The selected last row must stay visible; the top ones
        // scroll out of view.
        let mut term = Terminal::new(TestBackend::new(60, 11)).unwrap();
        term.draw(|f| render(f, f.area(), &view)).unwrap();
        let text = dump(&term.backend().buffer().clone());
        assert!(text.contains("Command11"), "the selected row stays visible: {text}");
        assert!(!text.contains("Command00"), "the top rows scrolled out: {text}");
    }

    // -- Half-block selection bleed on the command list --------------------

    #[test]
    fn selected_command_row_renders_the_half_block_bleed() {
        use ratatui::{backend::TestBackend, Terminal};
        let p = Palette::new(
            vec![
                entry_full("view:orch", "Chat", "Talk with the Orchestrator", None),
                entry_full("view:tasks", "Issues", "Queue up and manage work", None),
            ],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        // Selection defaults to index 0 (Chat) with the commands column focused.
        let mut term = Terminal::new(TestBackend::new(100, 14)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();

        let chat_y = find_row_with(&buf, "Chat").expect("the selected label renders");
        let (website_x, _) = find_cell(&buf, "Website").expect("a Projects row renders");

        // Solid fill: the selected content row carries the selection background.
        assert!(
            (0..buf.area.width).any(|x| buf[(x, chat_y)].bg == palette().selection),
            "the selected row has the solid fill"
        );
        // Lower-half-block (`▄`) bleed row directly above the selection, drawn
        // with its foreground set to the selection background.
        let above = chat_y - 1;
        assert!(
            (0..buf.area.width).any(|x| {
                buf[(x, above)].symbol() == "▄" && buf[(x, above)].fg == palette().selection
            }),
            "a ▄ bleed row sits above the selection: {}",
            dump(&buf)
        );
        // Upper-half-block (`▀`) bleed row directly below.
        let below = chat_y + 1;
        assert!(
            (0..buf.area.width).any(|x| {
                buf[(x, below)].symbol() == "▀" && buf[(x, below)].fg == palette().selection
            }),
            "a ▀ bleed row sits below the selection: {}",
            dump(&buf)
        );

        // The Projects column is untouched: no bleed glyph reaches into or past
        // it, and no Projects row carries the command fill.
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let c = &buf[(x, y)];
                if (c.symbol() == "▄" || c.symbol() == "▀") && c.fg == palette().selection {
                    assert!(
                        x < website_x,
                        "bleed stays in the commands column (x={x}, website_x={website_x})"
                    );
                }
            }
        }
        let web_y = find_row_with(&buf, "Website").unwrap();
        for x in website_x..buf.area.width {
            assert_ne!(
                buf[(x, web_y)].bg,
                palette().selection,
                "a Projects row must carry no command selection fill"
            );
        }
    }

    #[test]
    fn moving_the_selection_leaves_no_leftover_fill_on_the_old_row() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut p = Palette::new(
            vec![
                entry_full("view:orch", "Chat", "Talk with the Orchestrator", None),
                entry_full("view:tasks", "Issues", "Queue up and manage work", None),
            ],
            Vec::new(),
        );
        // Move the selection off Chat onto Issues.
        p.handle_key(key(KeyCode::Down), Some(PaletteAction::NavDown));
        assert_eq!(p.selected(), 1);
        let mut term = Terminal::new(TestBackend::new(80, 12)).unwrap();
        term.draw(|f| p.render(f, f.area())).unwrap();
        let buf = term.backend().buffer().clone();

        let chat_y = find_row_with(&buf, "Chat").unwrap();
        let issues_y = find_row_with(&buf, "Issues").unwrap();

        // The now-unselected Chat row keeps no fill. (The row below it is the
        // selected Issues row's own bleed-above — a shared separator — so the
        // clean-separator check is the row *above* Chat, which belongs to no
        // selection now.)
        assert!(
            (0..buf.area.width).all(|x| buf[(x, chat_y)].bg != palette().selection),
            "the deselected row has no leftover fill: {}",
            dump(&buf)
        );
        assert!(
            (0..buf.area.width)
                .all(|x| buf[(x, chat_y - 1)].symbol() != "▄" && buf[(x, chat_y - 1)].symbol() != "▀"),
            "no bleed glyph lingers above the deselected row: {}",
            dump(&buf)
        );
        // The newly selected Issues row carries the fill and its bleed rows.
        assert!(
            (0..buf.area.width).any(|x| buf[(x, issues_y)].bg == palette().selection),
            "the new selection is filled: {}",
            dump(&buf)
        );
        assert!(
            (0..buf.area.width).any(|x| buf[(x, issues_y - 1)].symbol() == "▄"),
            "the new selection has a ▄ above it: {}",
            dump(&buf)
        );
    }

    // -- Searching: the filter row and grouped results ---------------------

    /// A Changelog entry for the search fixtures.
    pub(crate) fn changelog_entry(id: &str, title: &str, pr: u64) -> shelbi_app::changelog::ChangelogEntryView {
        shelbi_app::changelog::ChangelogEntryView::new(
            shelbi_state::changelog::ChangelogEntry {
                id: id.into(),
                title: title.into(),
                date: chrono::DateTime::parse_from_rfc3339("2026-10-28T14:30:00-07:00").unwrap(),
                author: "orchestrator".into(),
                issues: vec![],
                prs: vec![shelbi_state::changelog::PrRef::Number(pr)],
                urls: vec![],
                body: "The login form, rebuilt.".into(),
                path: Default::default(),
            },
            Some("jlong/shelbi"),
        )
    }

    /// The wireframe's palette: commands, two issues and an entry that match
    /// `login`, and a Projects column (which steps aside while searching).
    pub(crate) fn search_palette() -> Palette {
        use shelbi_app::overlay::palette_search::IssueDoc;
        use shelbi_core::StatusCategory;
        let issues = vec![
            IssueDoc::new("1931", "Fix login redirect loop", "", "In Progress", StatusCategory::Active),
            IssueDoc::new("1944", "Login form validation", "", "Backlog", StatusCategory::Backlog),
            IssueDoc::new("1950", "Faster board", "", "Done", StatusCategory::Done),
        ];
        let corpus = SearchCorpus::new(issues, vec![changelog_entry("2026-10-28-login", "Login form shipped", 1902)]);
        Palette::new(
            vec![
                entry_full("action:log-in", "Log in to GitHub", "Account", None),
                entry_full("view:tasks", "Issues", "Queue up and manage work", None),
            ],
            vec![project("website", "Website", ProjectIndicator::Active)],
        )
        .with_search(PaletteSearch::inline(corpus))
    }

    fn type_in(p: &mut Palette, q: &str) {
        for c in q.chars() {
            p.handle_key(key(KeyCode::Char(c)), None);
        }
    }

    fn draw(p: &Palette, w: u16, h: u16) -> (ratatui::buffer::Buffer, Rect) {
        use ratatui::{backend::TestBackend, Terminal};
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let rect = p.overlay_rect(Rect::new(0, 0, w, h));
        term.draw(|f| p.render(f, rect)).unwrap();
        (term.backend().buffer().clone(), rect)
    }

    #[test]
    fn searching_draws_the_filter_row_and_groups_the_results() {
        let mut p = search_palette();
        type_in(&mut p, "login");
        let (buf, _) = draw(&p, 100, 30);
        let text = dump(&buf);
        assert!(
            text.contains("All 4  Issues 2  Changelog Entries 1  Commands 1"),
            "the chips with their counts: {text}"
        );
        // The chosen chip is in the selection fill.
        let (x, y) = find_cell(&buf, "All 4").unwrap();
        assert_eq!(buf[(x, y)].bg, palette().selection, "the chosen chip");
        let (x, y) = find_cell(&buf, "Issues 2").unwrap();
        assert_ne!(buf[(x, y)].bg, palette().selection);
        // Groups in order, commands first, each under its header.
        let at = |needle: &str| find_row_with(&buf, needle).unwrap_or_else(|| panic!("{needle}: {text}"));
        assert!(at("Log in to GitHub") < at("Fix login redirect loop"));
        assert!(at("Fix login redirect loop") < at("Login form shipped"));
        assert!(at(" Commands  ") < at("Log in to GitHub"));
        assert!(text.contains("in progress · #1931"), "an issue's status and number: {text}");
        assert!(text.contains("Oct 28 · #1902"), "an entry's date and PR: {text}");
        assert!(!text.contains("Projects"), "the Projects column steps aside: {text}");
        assert!(text.contains("Tab filter"), "the footer names the filter keys: {text}");
    }

    #[test]
    fn tab_picks_a_filter_and_a_click_picks_a_chip() {
        let mut p = search_palette();
        type_in(&mut p, "login");
        p.handle_key(key(KeyCode::Tab), None);
        let (buf, rect) = draw(&p, 100, 30);
        let text = dump(&buf);
        let (x, y) = find_cell(&buf, "Issues 2").unwrap();
        assert_eq!(buf[(x, y)].bg, palette().selection, "Tab chose Issues: {text}");
        assert!(text.contains("Login form validation"));
        assert!(!text.contains("Log in to GitHub") && !text.contains("Login form shipped"), "{text}");

        // A click on the Changelog chip shows the entries.
        let (x, y) = find_cell(&buf, "Changelog Entries 1").unwrap();
        assert_eq!(p.handle_click(rect, x + 2, y), PaletteStep::Continue);
        assert_eq!(p.inner().filter(), SearchFilter::Changelog);
        let (buf, rect) = draw(&p, 100, 30);
        let (x, y) = find_cell(&buf, "Login form shipped").unwrap();
        match p.handle_click(rect, x, y) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "action:open-changelog-entry:2026-10-28-login"),
            other => panic!("a click on the entry opens it, got {other:?}"),
        }
    }

    #[test]
    fn a_click_on_an_issue_opens_it_and_a_header_does_nothing() {
        let mut p = search_palette();
        type_in(&mut p, "login");
        let (buf, rect) = draw(&p, 100, 30);
        let (x, y) = find_cell(&buf, "Login form validation").unwrap();
        match p.handle_click(rect, x, y) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "action:open-issue:1944"),
            other => panic!("a click on the issue opens it, got {other:?}"),
        }
        let (buf, rect) = draw(&p, 100, 30);
        let y = find_row_with(&buf, " Changelog Entries  ").unwrap();
        assert_eq!(p.handle_click(rect, rect.x + 3, y), PaletteStep::Continue);
    }

    #[test]
    fn searching_renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            let mut p = search_palette();
            type_in(&mut p, "login");
            let (buf, _) = draw(&p, 100, 30);
            crate::theme::assert_light_readable("palette-search", &buf);
            p.handle_key(key(KeyCode::Up), Some(PaletteAction::NavUp));
            let (buf, _) = draw(&p, 100, 30);
            crate::theme::assert_light_readable("palette-search-filters", &buf);
        });
    }

    #[test]
    fn clicking_a_command_label_or_its_bleed_row_activates_that_item() {
        let mut p = Palette::new(
            vec![
                entry("view:a", "Alpha"),
                entry("view:b", "Bravo"),
                entry("view:c", "Charlie"),
            ],
            Vec::new(),
        );
        let area = Rect::new(0, 0, 80, 24);
        let rect = p.overlay_rect(area);
        let rows = panel_rows(rect, false).unwrap();
        let (band, _) = split_results(rows.band, false);
        let col = band.x + 1;

        // Item 1's label row sits at band offset 2*1 + 1 = 3.
        match p.handle_click(rect, col, band.y + 3) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "view:b"),
            other => panic!("label-row click should activate Bravo, got {other:?}"),
        }
        assert_eq!(p.selected(), 1, "the clicked item becomes selected");

        // Item 2's bleed-above row (band offset 2*2 = 4) also activates it — a
        // click anywhere on the item's band resolves to that item.
        match p.handle_click(rect, col, band.y + 4) {
            PaletteStep::Activate(e) => assert_eq!(e.id, "view:c"),
            other => panic!("bleed-row click should activate Charlie, got {other:?}"),
        }

        // A click on the search line (above the band) leaves the palette open.
        assert_eq!(
            p.handle_click(rect, col, rows.search.y),
            PaletteStep::Continue,
            "a click outside the commands band is a no-op"
        );
    }

}

