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

use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_palette::{Entry, EntryKind};
use shelbi_state::keymap::PaletteAction;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::decoration_to_color;
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

// ---------------------------------------------------------------------------
// Project indicators (Projects column status glyphs)
// ---------------------------------------------------------------------------

/// A project row's liveness indicator in the Projects column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectIndicator {
    /// The current/open project (active) — filled green disc, pulsing.
    Active,
    /// Loaded but not the current project — green ring, no fill.
    LoadedIdle,
    /// Not loaded — neutral/gray ring.
    Unloaded,
}

/// Resolve a project's indicator from its two live inputs. Not-loaded
/// dominates: a project with no live session shows the neutral ring even if a
/// stale active-category task lingers on disk, because nothing is progressing
/// it.
pub fn project_indicator(loaded: bool, active: bool) -> ProjectIndicator {
    if !loaded {
        ProjectIndicator::Unloaded
    } else if active {
        ProjectIndicator::Active
    } else {
        ProjectIndicator::LoadedIdle
    }
}

/// Leading status glyph + color for a project row, given its indicator and the
/// current pulse `phase`. The Figma uses a filled `●` for the active project, a
/// `○` ring for every other (green when loaded, gray when not).
pub fn project_status_style(indicator: ProjectIndicator, phase: f32) -> (&'static str, Color) {
    match indicator {
        ProjectIndicator::Active => ("●", theme::project_pulse_color(phase)),
        ProjectIndicator::LoadedIdle => ("○", theme::PROJECT_STATUS_GREEN),
        ProjectIndicator::Unloaded => ("○", theme::PROJECT_STATUS_NEUTRAL),
    }
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

/// Everything the shared [`render`] draws, as plain data.
pub struct PaletteView<'a> {
    pub query: &'a str,
    pub results: &'a [(Entry, u16)],
    pub selected: usize,
    /// Whether the commands column holds focus (dims its selection otherwise).
    pub commands_focused: bool,
    /// The Projects column, or `None` to render a single-column completion list
    /// spanning the whole width.
    pub projects: Option<ProjectsColumn>,
    /// The one-line footer hint.
    pub footer: &'a str,
}

/// Paint the palette into `area` (the content-sized overlay rect [`palette_rect`]
/// computes). The rect is the panel: a solid [`theme::PALETTE_BG`] fill wrapped
/// in a muted single-line border, content inset one column inside the border.
pub fn render(f: &mut Frame, area: Rect, view: &PaletteView) {
    // A filled panel wrapped in the muted single-line border (the Figma
    // "Terminal border"): the `Block` paints the PALETTE_BG fill — so every gap
    // reads as one surface — and draws the border in the same pass.
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::PALETTE_MUTED))
        .style(Style::default().bg(theme::PALETTE_BG));
    let inner = block.inner(area);
    f.render_widget(block, area);

    // One column of padding inside the border on each side. Vertically the
    // search line sits just under the top border and the footer just above the
    // bottom one (the blank rows inside the layout do the breathing).
    let content = Rect {
        x: inner.x.saturating_add(1),
        y: inner.y,
        width: inner.width.saturating_sub(2),
        height: inner.height,
    };
    if content.width == 0 || content.height == 0 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // search line
            Constraint::Length(1), // blank
            Constraint::Min(1),    // results band
            Constraint::Length(1), // blank
            Constraint::Length(1), // footer
        ])
        .split(content);

    f.render_widget(Paragraph::new(search_line(view.query)), rows[0]);

    // Results: commands column on the left, an optional Projects column on the
    // right separated by whitespace.
    let (commands_area, projects_area) = split_results(rows[2], view.projects.is_some());

    if view.results.is_empty() {
        // A query that matches no command shows one dim row rather than
        // collapsing the band to nothing, so the panel keeps a legible shape.
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "No matching commands",
                Style::default().fg(theme::PALETTE_MUTED),
            ))),
            commands_area,
        );
    } else {
        let commands_focused = projects_area.is_none() || view.commands_focused;
        let selected = view.selected.min(view.results.len().saturating_sub(1));
        let items = build_result_items(
            view.results,
            commands_area.width as usize,
            commands_focused.then_some(selected),
        );
        let mut list = List::new(items);
        if commands_focused {
            // Patch the selected row's background only; the label span already
            // carries its white/bold foreground, and leaving the foreground
            // alone keeps the description dim on the highlighted row (matching
            // the design).
            list = list.highlight_style(Style::default().bg(theme::SELECTION_BG));
        }
        // `ListState` scrolls to keep the selected row visible when the band is
        // capped shorter than the result list, so the selection never leaves
        // the panel.
        let mut s = ListState::default();
        s.select(Some(selected));
        f.render_stateful_widget(list, commands_area, &mut s);
    }

    if let (Some(area), Some(projects)) = (projects_area, view.projects.as_ref()) {
        render_projects_column(f, area, projects);
    }

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            view.footer.to_string(),
            Style::default().fg(theme::PALETTE_MUTED),
        ))),
        rows[4],
    );
}

/// Rows the results band needs: the larger of the command result count (at
/// least one, for the "No matching commands" row) and the Projects column's
/// rows — its heading, one row per project, and the trailing "Add project".
fn results_band_rows(result_count: usize, project_count: Option<usize>) -> u16 {
    let commands = result_count.max(1).min(u16::MAX as usize) as u16;
    let projects = project_count
        .map(|n| (n.min(u16::MAX as usize - 2) as u16) + 2)
        .unwrap_or(0);
    commands.max(projects)
}

/// The palette's overlay rect within `area` (the main area). Horizontally
/// centered at the prior width (70%, min 40), top-aligned a small fixed offset
/// below the top of the window (about 2 rows, or 10% of the height, whichever is
/// smaller), and tall enough for exactly its current content — so the panel
/// grows and shrinks as the list filters. Capped to the window, with a matching
/// top and bottom margin, so a longer list scrolls inside the panel instead of
/// overflowing.
pub fn palette_rect(area: Rect, result_count: usize, project_count: Option<usize>) -> Rect {
    let offset_y = (area.height / 10).min(2);
    let width = (area.width.saturating_mul(70) / 100)
        .max(PALETTE_MIN_W)
        .min(area.width);
    let desired_h = results_band_rows(result_count, project_count).saturating_add(CHROME_ROWS);
    let max_h = area.height.saturating_sub(offset_y.saturating_mul(2));
    let height = desired_h.min(max_h);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + offset_y;
    Rect::new(x, y, width, height)
}

/// The search line: a `❯` prompt, then a green block cursor and the dim
/// placeholder (empty query) or the typed query followed by the block cursor.
fn search_line(query: &str) -> Line<'static> {
    let prompt = Span::styled("❯ ", Style::default().fg(theme::PALETTE_FG));
    let cursor = Style::default().bg(theme::PALETTE_GREEN).fg(Color::White);
    if query.is_empty() {
        const PLACEHOLDER: &str = "Type a command or search";
        let mut chars = PLACEHOLDER.chars();
        let first: String = chars.by_ref().take(1).collect();
        let rest: String = chars.collect();
        Line::from(vec![
            prompt,
            Span::styled(first, cursor),
            Span::styled(rest, Style::default().fg(theme::PALETTE_MUTED)),
        ])
    } else {
        Line::from(vec![
            prompt,
            Span::styled(query.to_string(), Style::default().fg(theme::PALETTE_FG)),
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
                Color::Yellow
            } else {
                Color::DarkGray
            };
            (glyph.to_string(), color)
        }
    }
}

/// Build the command-list rows: a fixed-width icon slot, a fixed-width label
/// (bold white on the selected row, else foreground), a dim description aligned
/// to a fixed x and truncated with `…` when it won't fit, and a right-aligned
/// dim shortcut hint. Each row is padded to `row_width` so the selection bar
/// spans the whole commands column. `selected` is the highlighted index when
/// the commands column holds focus.
fn build_result_items(
    results: &[(Entry, u16)],
    row_width: usize,
    selected: Option<usize>,
) -> Vec<ListItem<'static>> {
    results
        .iter()
        .enumerate()
        .map(|(i, (e, _))| {
            let is_selected = selected == Some(i);
            let (glyph, glyph_color) = icon_for(e);

            // Icon field: a space, the glyph padded to two cells, a space.
            let glyph_w = UnicodeWidthStr::width(glyph.as_str());
            let icon_field = format!(" {glyph}{} ", " ".repeat(2usize.saturating_sub(glyph_w)));

            // Label field padded to a fixed display width.
            let label = truncate_to_width(&e.label, LABEL_W);
            let label_pad = LABEL_W.saturating_sub(UnicodeWidthStr::width(label.as_str()));
            let label_style = if is_selected {
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme::PALETTE_FG)
            };

            let mut spans = vec![
                Span::styled(icon_field, Style::default().fg(glyph_color)),
                Span::styled(
                    format!("{label}{}", " ".repeat(label_pad)),
                    label_style,
                ),
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
            spans.push(Span::styled(
                desc,
                Style::default().fg(theme::PALETTE_MUTED),
            ));
            if short_w > 0 {
                spans.push(Span::raw(" ".repeat(filler + 1)));
                spans.push(Span::styled(short, Style::default().fg(theme::PALETTE_MUTED)));
            } else if filler > 0 {
                spans.push(Span::raw(" ".repeat(filler)));
            }

            ListItem::new(Line::from(spans))
        })
        .collect()
}

/// Render the Projects column into `area`: a dim "Projects" heading, then one
/// row per project — a dim `·` bullet, the status glyph, and the name — closed
/// by a trailing "+ Add project" row. Separated from the commands by
/// whitespace, with no border line.
fn render_projects_column(f: &mut Frame, area: Rect, col: &ProjectsColumn) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Projects",
            Style::default().fg(theme::PALETTE_MUTED),
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
        theme::PROJECT_STATUS_NEUTRAL,
        "Add project",
        width,
    ));

    // Selection patches background + weight only (never a foreground color) so a
    // loaded project's green ring keeps its status color under the bar.
    let highlight = Style::default()
        .bg(theme::SELECTION_BG)
        .add_modifier(Modifier::BOLD);
    let list = List::new(items).highlight_style(highlight);
    let mut s = ListState::default();
    if let Some(sel) = col.selected {
        let max = col.rows.len(); // the Add row is index rows.len()
        s.select(Some(sel.min(max)));
    }
    f.render_stateful_widget(list, rows[1], &mut s);
}

/// One Projects-column row: `· <glyph> <label>`, padded to `width` so a focused
/// selection bar spans the whole column.
fn project_row_line(glyph: &str, glyph_color: Color, label: &str, width: usize) -> ListItem<'static> {
    let text_w = 2 // "· "
        + UnicodeWidthStr::width(glyph)
        + 1 // space
        + UnicodeWidthStr::width(label);
    let pad = width.saturating_sub(text_w);
    ListItem::new(Line::from(vec![
        Span::styled("· ", Style::default().fg(theme::PALETTE_MUTED)),
        Span::styled(glyph.to_string(), Style::default().fg(glyph_color)),
        Span::styled(format!(" {label}"), Style::default().fg(theme::PALETTE_FG)),
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

/// A switch-target project for the Projects column: its slug (for the
/// switch-project entry id), display label, and resolved status indicator.
#[derive(Debug, Clone)]
pub struct ProjectEntry {
    pub slug: String,
    pub label: String,
    pub indicator: ProjectIndicator,
}

/// Which column holds focus inside the open palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Commands,
    Projects,
}

/// What feeding one key into the in-process palette resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteStep {
    /// Keep the overlay open (filtered, moved selection, edited the query).
    Continue,
    /// Close the overlay and return focus to the agent (Escape / the palette
    /// chord again).
    Close,
    /// Close the overlay and move focus to the sidebar (Tab).
    FocusSidebar,
    /// Run this entry's command, then close the overlay.
    Activate(Entry),
}

/// The in-process command palette overlay. Holds the current query, the command
/// selection, the switch-target projects, and which column holds focus; the
/// host (the shell) supplies the entries from
/// [`shelbi_app::CommandRegistry::entries`] and the projects, and turns an
/// activated [`Entry`] into a command effect.
pub struct Palette {
    query: String,
    selected: usize,
    entries: Vec<Entry>,
    projects: Vec<ProjectEntry>,
    focus: Focus,
    project_selected: usize,
    opened_at: Instant,
}

impl Palette {
    /// Open the palette over `entries`, with `projects` listed in the Projects
    /// column (empty hides the column).
    pub fn new(entries: Vec<Entry>, projects: Vec<ProjectEntry>) -> Self {
        Self {
            query: String::new(),
            selected: 0,
            entries,
            projects,
            focus: Focus::Commands,
            project_selected: 0,
            opened_at: Instant::now(),
        }
    }

    /// Replace the entry list (a background refresh landed) without disturbing
    /// the query; the renderer re-clamps the selection.
    pub fn set_entries(&mut self, entries: Vec<Entry>) {
        self.entries = entries;
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    /// The fuzzy-filtered results for the current query.
    pub fn results(&self) -> Vec<(Entry, u16)> {
        shelbi_palette::search(&self.entries, &self.query)
    }

    /// The overlay rect for the current content within `area`, via
    /// [`palette_rect`]. Recomputed every frame so the panel resizes as the
    /// query filters the list.
    pub fn overlay_rect(&self, area: Rect) -> Rect {
        let result_count = self.results().len();
        let project_count = (!self.projects.is_empty()).then_some(self.projects.len());
        palette_rect(area, result_count, project_count)
    }

    /// The highest selectable index in the Projects column: one past the last
    /// project (the trailing "Add project" row).
    fn projects_max(&self) -> usize {
        self.projects.len()
    }

    /// Synthesize the [`Entry`] activated by Enter on the focused project row.
    /// A project row maps to its `action:switch-project:<slug>` entry; the
    /// trailing row maps to `action:add-project`. The shell resolves either id
    /// through the registry, so no new effect plumbing is needed.
    fn project_entry(&self) -> Entry {
        let (id, label) = match self.projects.get(self.project_selected) {
            Some(p) => (format!("action:switch-project:{}", p.slug), p.label.clone()),
            None => ("action:add-project".to_string(), "Add project".to_string()),
        };
        Entry {
            id,
            label,
            kind: EntryKind::Action,
            subtitle: None,
            shortcut: None,
            decoration: None,
            hidden_until_query: false,
        }
    }

    /// Feed one key press. `action` is the palette-mode binding the host
    /// resolved from its keymap (so Close/Activate/Nav/Backspace honor the
    /// user's `keys.yaml`); Tab, Space, the arrow keys, and unbound printable
    /// chars are handled by raw key code so they behave regardless of bindings.
    pub fn handle_key(&mut self, ev: KeyEvent, action: Option<PaletteAction>) -> PaletteStep {
        // Tab leaves the palette for the sidebar (plan focus model). Handled
        // ahead of the keymap so a user binding can't shadow it.
        if ev.code == KeyCode::Tab && ev.modifiers.is_empty() {
            return PaletteStep::FocusSidebar;
        }
        // Right/Left move focus into and out of the Projects column.
        if ev.code == KeyCode::Right && ev.modifiers.is_empty() {
            if self.focus == Focus::Commands && !self.projects.is_empty() {
                self.focus = Focus::Projects;
                self.project_selected = 0;
            }
            return PaletteStep::Continue;
        }
        if ev.code == KeyCode::Left && ev.modifiers.is_empty() {
            self.focus = Focus::Commands;
            return PaletteStep::Continue;
        }
        // Space always types into the query (even if a keymap binds it); this
        // also pulls focus back to the commands column.
        if ev.code == KeyCode::Char(' ')
            && (ev.modifiers.is_empty() || ev.modifiers == KeyModifiers::SHIFT)
        {
            self.type_char(' ');
            return PaletteStep::Continue;
        }

        match action {
            Some(PaletteAction::Close) => PaletteStep::Close,
            Some(PaletteAction::Activate) => {
                if self.focus == Focus::Projects {
                    return PaletteStep::Activate(self.project_entry());
                }
                match self.results().get(self.selected) {
                    Some((entry, _)) => PaletteStep::Activate(entry.clone()),
                    None => PaletteStep::Continue,
                }
            }
            Some(PaletteAction::NavUp) => {
                if self.focus == Focus::Projects {
                    self.project_selected = self.project_selected.saturating_sub(1);
                } else {
                    self.selected = self.selected.saturating_sub(1);
                }
                PaletteStep::Continue
            }
            Some(PaletteAction::NavDown) => {
                if self.focus == Focus::Projects {
                    if self.project_selected < self.projects_max() {
                        self.project_selected += 1;
                    }
                } else {
                    let count = self.results().len();
                    if self.selected + 1 < count {
                        self.selected += 1;
                    }
                }
                PaletteStep::Continue
            }
            Some(PaletteAction::Backspace) => {
                self.query.pop();
                self.selected = 0;
                self.focus = Focus::Commands;
                PaletteStep::Continue
            }
            None => {
                if let KeyCode::Char(c) = ev.code {
                    if ev.modifiers.is_empty() || ev.modifiers == KeyModifiers::SHIFT {
                        self.type_char(c);
                    }
                }
                PaletteStep::Continue
            }
        }
    }

    /// Type a character into the query, resetting the command selection and
    /// pulling focus back to the commands column (typing filters commands).
    fn type_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.focus = Focus::Commands;
    }

    /// The Projects column view, or `None` when there are no projects.
    fn projects_view(&self) -> Option<ProjectsColumn> {
        if self.projects.is_empty() {
            return None;
        }
        Some(ProjectsColumn {
            rows: self
                .projects
                .iter()
                .map(|p| ProjectRow {
                    label: p.label.clone(),
                    indicator: p.indicator,
                })
                .collect(),
            selected: (self.focus == Focus::Projects).then_some(self.project_selected),
            phase: pulse_phase(self.opened_at),
        })
    }

    /// Paint the overlay into `area`.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let results = self.results();
        let footer = if self.projects.is_empty() {
            "↑↓ navigate · Enter activate · Esc / Ctrl+P close"
        } else {
            "↑↓ navigate · → projects · Enter activate · Esc / Ctrl+P close"
        };
        let view = PaletteView {
            query: &self.query,
            results: &results,
            selected: self.selected,
            commands_focused: self.focus == Focus::Commands,
            projects: self.projects_view(),
            footer,
        };
        render(f, area, &view);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(idle_color, theme::PROJECT_STATUS_GREEN);
        // The unloaded project is a gray ring (design `#666`), distinguished
        // from the loaded ring by color, not glyph.
        let (unloaded_glyph, unloaded_color) =
            project_status_style(ProjectIndicator::Unloaded, 0.0);
        assert_eq!(unloaded_glyph, "○");
        assert_eq!(unloaded_color, theme::PROJECT_STATUS_NEUTRAL);
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
        p.selected = 1;
        for c in "zen".chars() {
            assert_eq!(p.handle_key(key(KeyCode::Char(c)), None), PaletteStep::Continue);
        }
        assert_eq!(p.query(), "zen");
        assert_eq!(p.selected(), 0);
        let hits = p.results();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].0.id, "action:toggle-zen");
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
            theme::PALETTE_MUTED,
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
        assert!(!text.contains("Issues"), "the non-matching row is filtered out: {text}");
    }

    #[test]
    fn right_focuses_projects_then_left_returns_to_commands() {
        let mut p = Palette::new(
            vec![entry("view:tasks", "Issues")],
            vec![project("website", "Website", ProjectIndicator::Active)],
        );
        assert_eq!(p.focus, Focus::Commands);
        assert_eq!(p.handle_key(key(KeyCode::Right), None), PaletteStep::Continue);
        assert_eq!(p.focus, Focus::Projects);
        assert_eq!(p.handle_key(key(KeyCode::Left), None), PaletteStep::Continue);
        assert_eq!(p.focus, Focus::Commands);
    }

    #[test]
    fn right_is_a_noop_without_projects() {
        let mut p = Palette::new(vec![entry("view:tasks", "Issues")], Vec::new());
        p.handle_key(key(KeyCode::Right), None);
        assert_eq!(p.focus, Focus::Commands);
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
        assert_eq!(p.focus, Focus::Projects);
        p.handle_key(key(KeyCode::Char('i')), None);
        assert_eq!(p.focus, Focus::Commands);
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
        assert_eq!(p.project_selected, 1);
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
        let results: Vec<(Entry, u16)> = Vec::new();
        let view = PaletteView {
            query: "",
            results: &results,
            selected: 0,
            commands_focused: col.selected.is_none(),
            projects: Some(col),
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
                        theme::PROJECT_STATUS_GREEN,
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
    fn projects_column_renders_glyphs_the_bullet_and_the_add_row() {
        // All three status glyphs plus the add row and the `·` bullets.
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
        assert!(dumped.contains('·'), "row bullets: {dumped}");
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
        assert_eq!(results_band_rows(6, None), 6);
        // An empty list still reserves one row (the "No matching commands" row).
        assert_eq!(results_band_rows(0, None), 1);
        // Projects column: heading + one row per project + the Add row (n + 2).
        assert_eq!(results_band_rows(1, Some(3)), 5);
        assert_eq!(results_band_rows(10, Some(3)), 10);
    }

    #[test]
    fn palette_rect_is_top_aligned_centered_and_content_sized() {
        let area = Rect::new(0, 0, 100, 40);
        // Six results, no projects: band 6 + 6 chrome rows = 12 tall.
        let r = palette_rect(area, 6, None);
        assert_eq!(r.height, 12, "height fits the content");
        // Width is the prior 70%/min-40 rule, horizontally centered.
        assert_eq!(r.width, 70);
        assert_eq!(r.x, 15);
        // Top-aligned a small offset below the top: min(2, 10% of height).
        assert_eq!(r.y, 2);
    }

    #[test]
    fn palette_rect_shrinks_and_grows_with_the_result_count() {
        let area = Rect::new(0, 0, 100, 40);
        let full = palette_rect(area, 8, None);
        let one = palette_rect(area, 1, None);
        let none = palette_rect(area, 0, None);
        assert!(one.height < full.height, "one result is a shorter panel");
        assert_eq!(one.height, 7, "one result: 1 band row + 6 chrome");
        assert_eq!(none.height, 7, "no matches still reserves the one row");
    }

    #[test]
    fn palette_rect_caps_tall_lists_to_the_window() {
        let area = Rect::new(0, 0, 100, 20);
        // 100 results would want 106 rows; cap to the window minus the top and
        // bottom margin (offset 2 each, from 10% of 20).
        let r = palette_rect(area, 100, None);
        assert_eq!(r.y, 2);
        assert_eq!(r.height, 16, "capped to area.height - 2*offset");
        assert!(r.y + r.height <= area.height, "stays within the window");
    }

    #[test]
    fn palette_rect_offset_is_the_smaller_of_two_rows_and_ten_percent() {
        // Short window: 10% of 15 = 1 row, smaller than 2.
        let short = palette_rect(Rect::new(0, 0, 80, 15), 3, None);
        assert_eq!(short.y, 1);
        // Tall window: the offset caps at 2 rows.
        let tall = palette_rect(Rect::new(0, 0, 80, 60), 3, None);
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
        assert!(
            text.contains("No matching commands"),
            "the empty state renders one dim row: {text}"
        );
    }

    #[test]
    fn capped_band_scrolls_to_keep_the_selection_visible() {
        use ratatui::{backend::TestBackend, Terminal};
        let results: Vec<(Entry, u16)> = (0..12)
            .map(|i| (entry(&format!("view:v{i}"), &format!("Command{i:02}")), 0u16))
            .collect();
        let view = PaletteView {
            query: "",
            results: &results,
            selected: 11,
            commands_focused: true,
            projects: None,
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

}

