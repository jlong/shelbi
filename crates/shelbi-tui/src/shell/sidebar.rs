//! The shell's sidebar: a renderer over shelbi-app's [`SidebarModel`], brought
//! to visual parity with the former tmux-runtime sidebar (`crate::sidebar` +
//! the old `crate::app` renderer it was ported from).
//!
//! The model logic lives in [`shelbi_app::view::SidebarModel`] (built off the
//! UI thread by [`super::sidebar_model`]); this module only lays those rows out
//! and maps a selection / click to what the main area should show. Laid out
//! top to bottom to match John's Figma "Navigation" design: a header (project
//! title + a filled search box that opens the command palette), a full-width
//! nav block with the half-block selection bleed, the machine-grouped
//! workspace pool with per-state badges, the two review sections rendered as
//! two-line entries (dim `⎇ branch` line, truncated with `…`), and the footer
//! (daemon-version row, the `ZEN MODE ON` band shown only while Zen is on, the
//! one-time first-run hint, and the unread-errors button). The old `^P palette
//! q quit` / `^G Zen mode` footer hint lines are gone; those actions stay
//! reachable through the palette and their existing keys.
//!
//! Selection is a single flat index over the *selectable* rows (section
//! headers / blanks are skipped), exactly as [`shelbi_app::nav::ClientState`]
//! models it — the one semantic difference from the old renderer, which indexed
//! every row.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, StatefulWidget, Widget, Wrap};

use shelbi_app::nav::View;
use shelbi_app::view::{ReviewState, SidebarModel, WorkspaceBadge};
use shelbi_state::keymap::{DisplayStyle, GlobalAction, Keymaps};
use shelbi_state::ZenModeState;

use super::session::SessionRef;
use crate::keymap::format_chord_or_unbound;
use crate::sidebar::{decoration_to_color, nav_lines, BLEED_ABOVE, BLEED_BELOW};
use crate::theme::{SEARCH_BG, SELECTION_BG};

/// Exact one-time orientation copy shown in the sidebar footer after the first
/// project scaffold. One constant so persistence and wrapping paths can't drift
/// from the product wording (ported from the former `crate::FIRST_RUN_HINT`).
pub(crate) const FIRST_RUN_HINT: &str = "Ctrl+P palette · type E to edit settings";

/// The unread-errors button: a solid 5-wide × 3-tall block anchored to the
/// footer's bottom-left corner, a single bold `!` in its center cell.
const ERROR_BUTTON_W: u16 = 5;
const ERROR_BUTTON_H: u16 = 3;
/// Reddish-gray fill — muted enough to read as chrome, warm enough to signal
/// something's wrong, legible in both light and dark terminals.
const ERROR_BUTTON_BG: Color = Color::Rgb(110, 70, 70);

/// Fill for the `ZEN MODE ON` band — the Figma `color/zen` token (#007f00), a
/// deep green that reads as a steady "on" bar behind bold white text.
const ZEN_BAND_BG: Color = Color::Rgb(0, 127, 0);

/// What a selectable sidebar row routes to when opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowTarget {
    /// A live session shown in the terminal view.
    Session(SessionRef),
    /// A native view (issues / activity / machines).
    Native(View),
    /// A review-column task — opens the review interface.
    Review(String),
    /// A machine group header — toggles its collapse state.
    Machine(String),
}

/// Per-frame footer/header input that isn't board state: the resolved
/// palette-open chord shown in the search box's `⌘K`-style hint. Owned (no
/// borrow of the shell state) so the draw closure can disjointly borrow the
/// rest of the state.
pub struct SidebarChrome {
    /// The palette-open chord resolved for the host platform (e.g. `⌃P` on
    /// macOS, `Ctrl+P` elsewhere), or `<unbound>` when no chord is bound.
    palette: String,
}

impl SidebarChrome {
    /// Build the chrome from the resolved keymaps + platform convention.
    pub fn from_keymaps(keymaps: &Keymaps, display_style: DisplayStyle) -> Self {
        SidebarChrome {
            palette: format_chord_or_unbound(
                keymaps.global.first_chord_for(GlobalAction::OpenPalette),
                display_style,
            ),
        }
    }
}

/// One laid-out sidebar row.
enum Row {
    Nav {
        glyph: &'static str,
        label: String,
        target: RowTarget,
    },
    Section(String),
    ConfigError(String),
    Blank,
    Loading,
    MachineGroup {
        name: String,
        collapsed: bool,
        total: usize,
        active: usize,
        target: RowTarget,
    },
    Workspace {
        name: String,
        badge: WorkspaceBadge,
        agent: Option<String>,
        indent: bool,
        target: RowTarget,
    },
    Review {
        title: String,
        branch: String,
        location: Option<String>,
        state: ReviewState,
        target: RowTarget,
    },
}

impl Row {
    fn is_selectable(&self) -> bool {
        !matches!(self, Row::Section(_) | Row::ConfigError(_) | Row::Blank | Row::Loading)
    }

    fn target(&self) -> Option<&RowTarget> {
        match self {
            Row::Nav { target, .. }
            | Row::MachineGroup { target, .. }
            | Row::Workspace { target, .. }
            | Row::Review { target, .. } => Some(target),
            Row::Section(_) | Row::ConfigError(_) | Row::Blank | Row::Loading => None,
        }
    }
}

/// A laid-out sidebar ready to render and hit-test.
pub struct SidebarView {
    project_label: String,
    rows: Vec<Row>,
    /// Indices into `rows` that are selectable, in order.
    selectable: Vec<usize>,
    board_banner: Option<String>,
    daemon_version_line: Option<String>,
    daemon_version_mismatch: bool,
    status_line: String,
    zen_mode: ZenModeState,
    unread_errors: usize,
}

impl SidebarView {
    pub fn build(model: &SidebarModel) -> Self {
        let rows = build_rows(model);
        let selectable = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| r.is_selectable().then_some(i))
            .collect();
        Self {
            project_label: model.project_label.clone(),
            rows,
            selectable,
            board_banner: model.board_banner.clone(),
            daemon_version_line: model.daemon_version_line.clone(),
            daemon_version_mismatch: model.daemon_version_mismatch,
            status_line: model.status_line.clone(),
            zen_mode: model.zen_mode,
            unread_errors: model.unread_errors,
        }
    }

    pub fn selectable_count(&self) -> usize {
        self.selectable.len()
    }

    /// The target of the `selection`-th selectable row.
    pub fn target_at(&self, selection: usize) -> Option<RowTarget> {
        let row = *self.selectable.get(selection)?;
        self.rows[row].target().cloned()
    }

    /// The row index of the `selection`-th selectable row, if any.
    fn selected_row(&self, selection: usize) -> Option<usize> {
        self.selectable.get(selection).copied()
    }

    /// Count of leading `Row::Nav` rows (the full-width nav block).
    fn nav_n(&self) -> usize {
        self.rows
            .iter()
            .take_while(|r| matches!(r, Row::Nav { .. }))
            .count()
    }

    /// Footer height: the three fixed rows (version / blank / zen band) plus the
    /// status row's height (0 when empty, 2 for the first-run hint, else 1). The
    /// zen band paints only while Zen is on, but the row is always reserved so
    /// the version line never shifts when Zen toggles.
    fn footer_height(&self) -> u16 {
        3 + self.status_row_height()
    }

    fn status_row_height(&self) -> u16 {
        if self.status_line.is_empty() {
            0
        } else if self.status_line == FIRST_RUN_HINT {
            2
        } else {
            1
        }
    }

    /// Split the sidebar `area` into (header, search, nav, rest-list, footer)
    /// rects — the shared geometry the renderer and the click map both derive
    /// from. `search` is `None` when the header is too short to hold the box;
    /// `rest` is `None` when the body is too short to hold anything past the nav
    /// block.
    fn geometry(&self, area: Rect) -> SidebarGeometry {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(self.footer_height())])
            .split(area);
        let body_region = outer[0];
        let footer = outer[1];
        // Header holds the project title (row 0), a half-row margin (row 1),
        // the 2-row search box across rows 2–4 (`▄` / label / `▀`), and a
        // half-row margin (row 5); then the body.
        let header_h = HEADER_H.min(body_region.height);
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_h), Constraint::Min(0)])
            .split(body_region);
        let header = split[0];
        let body = split[1];
        // The box spans the three cell rows 2–4; it needs rows 2, 3 and 4
        // present (height ≥ 5). The trailing margin (row 5) is optional.
        let search = (header.height >= 5).then(|| {
            Rect {
                y: header.y + 2,
                height: 3,
                ..header
            }
            .inner(LIST_INDENT)
        });
        let nav_height = (nav_lines(self.nav_n()) as u16).min(body.height);
        let nav = Rect {
            height: nav_height,
            ..body
        };
        let rest = if body.height > nav_height {
            Some(
                Rect {
                    y: body.y + nav_height,
                    height: body.height - nav_height,
                    ..body
                }
                .inner(LIST_INDENT),
            )
        } else {
            None
        };
        SidebarGeometry {
            header,
            search,
            nav,
            rest,
            footer,
        }
    }

    /// Whether `(x, y)` lands on the search box — the shell maps a click here to
    /// opening the command palette, the same as pressing the palette chord.
    pub fn search_box_hit(&self, area: Rect, x: u16, y: u16) -> bool {
        if area.width == 0 || area.height == 0 || !contains(area, x, y) {
            return false;
        }
        self.geometry(area)
            .search
            .is_some_and(|s| contains(s, x, y))
    }

    /// Map a click at viewer `(x, y)` to the selection index of the row there,
    /// or `None` for a header / blank / footer / out-of-bounds cell. Matches
    /// [`Self::render`]'s geometry: the nav block interleaves item rows with
    /// separators, and the rest-of-list rows are variable-height (a review
    /// entry is two lines; a config error wraps).
    pub fn hit(&self, area: Rect, x: u16, y: u16) -> Option<usize> {
        if area.width == 0 || area.height == 0 || !contains(area, x, y) {
            return None;
        }
        let geo = self.geometry(area);
        let nav_n = self.nav_n();

        // Nav block: item `k` sits on line `2k + 1`, separators on the even
        // lines between. Only item lines are selectable.
        if contains(geo.nav, x, y) {
            let target = (y - geo.nav.y) as usize;
            if target % 2 == 1 {
                let idx = target / 2;
                return self
                    .rows
                    .get(idx)
                    .and_then(|r| r.is_selectable().then_some(idx))
                    .and_then(|idx| self.row_to_selection(idx));
            }
            return None;
        }

        // Rest-of-list: walk cumulative row heights from where the nav ends.
        let rest = geo.rest?;
        if !contains(rest, x, y) {
            return None;
        }
        let inner_width = rest.width as usize;
        let target = (y - rest.y) as usize;
        let mut line = 0usize;
        for (idx, r) in self.rows.iter().enumerate().skip(nav_n) {
            let h = row_height(r, inner_width);
            if target < line + h {
                return r
                    .is_selectable()
                    .then(|| self.row_to_selection(idx))
                    .flatten();
            }
            line += h;
        }
        None
    }

    /// The selectable ordinal of row `idx`, if it is selectable.
    fn row_to_selection(&self, idx: usize) -> Option<usize> {
        self.selectable.iter().position(|&i| i == idx)
    }

    /// Paint the sidebar. `selection` is the selectable-row ordinal; `focused`
    /// brightens the selected row's text (vs a dim gray when focus is in the
    /// main area) — the fill is drawn in both states so the selection stays
    /// visible.
    pub fn render(
        &self,
        buf: &mut Buffer,
        area: Rect,
        selection: usize,
        focused: bool,
        chrome: &SidebarChrome,
    ) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let geo = self.geometry(area);

        self.render_header(buf, geo.header, geo.search, chrome);

        let sel_row = self.selected_row(selection);
        self.render_nav(buf, geo.nav, sel_row, focused);
        if let Some(rest) = geo.rest {
            self.render_rest(buf, rest, sel_row, focused);
        }
        self.render_footer(buf, geo.footer);
    }

    /// Render the header block: the project title in the accent cyan on the
    /// first row, and the 2-row filled search box (with its half-block bleed)
    /// two rows below it.
    fn render_header(
        &self,
        buf: &mut Buffer,
        header: Rect,
        search: Option<Rect>,
        chrome: &SidebarChrome,
    ) {
        if header.width == 0 || header.height == 0 {
            return;
        }
        let title_rect = Rect {
            height: 1.min(header.height),
            ..header
        };
        Paragraph::new(Line::from(Span::styled(
            self.project_label.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )))
        .render(title_rect.inner(LIST_INDENT), buf);

        if let Some(search) = search {
            render_search_box(buf, search, &chrome.palette);
        }
    }

    /// Render the leading `Row::Nav` rows as a full-width block: a separator
    /// line between (and bracketing) each item, the selected item's fill
    /// spanning edge to edge, its adjacent separators carrying the half-block
    /// bleed. Text keeps the 1-col indent of the rest of the list.
    fn render_nav(&self, buf: &mut Buffer, area: Rect, sel_row: Option<usize>, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let nav_n = self.nav_n();
        let width = area.width as usize;
        let selected = sel_row.filter(|&r| r < nav_n);

        let mut lines: Vec<Line> = Vec::with_capacity(nav_lines(nav_n));
        for p in 0..=nav_n {
            let glyph = if selected == Some(p) {
                Some(BLEED_ABOVE)
            } else if p > 0 && selected == Some(p - 1) {
                Some(BLEED_BELOW)
            } else {
                None
            };
            lines.push(match glyph {
                Some(g) => Line::from(Span::styled(
                    g.repeat(width),
                    Style::default().fg(SELECTION_BG),
                )),
                None => Line::raw(""),
            });
            if let Some(Row::Nav { glyph, label, .. }) = self.rows.get(p) {
                lines.push(nav_item_line(
                    glyph,
                    label,
                    selected == Some(p),
                    focused,
                    width,
                ));
            }
        }
        Paragraph::new(lines).render(area, buf);
    }

    /// Render everything after the nav block as a normal variable-height list
    /// with a full-row fill on the selected row.
    fn render_rest(&self, buf: &mut Buffer, area: Rect, sel_row: Option<usize>, focused: bool) {
        let nav_n = self.nav_n();
        let width = area.width as usize;
        let mut items: Vec<ListItem> = Vec::with_capacity(self.rows.len().saturating_sub(nav_n));
        for (i, row) in self.rows.iter().enumerate().skip(nav_n) {
            let selected = Some(i) == sel_row && row.is_selectable();
            items.push(render_row(row, selected, focused, width));
        }
        let mut state = ListState::default();
        if let Some(r) = sel_row {
            if r >= nav_n {
                state.select(Some(r - nav_n));
            }
        }
        let list = List::new(items).highlight_style(Style::default().bg(SELECTION_BG));
        StatefulWidget::render(list, area, buf, &mut state);
    }

    fn render_footer(&self, buf: &mut Buffer, area: Rect) {
        // Vertical rhythm: [status?] version, blank, zen-band. The blank keeps
        // the version line put whether or not the band paints, so toggling Zen
        // never nudges the line above.
        let has_status = !self.status_line.is_empty();
        let mut constraints = Vec::with_capacity(if has_status { 4 } else { 3 });
        if has_status {
            constraints.push(Constraint::Length(self.status_row_height()));
        }
        constraints.extend([Constraint::Length(1); 3]);
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        let first_fixed = if has_status { 1 } else { 0 };
        let show_button = self.unread_errors > 0
            && area.width > ERROR_BUTTON_W + 1
            && rows.len() >= first_fixed + 3;
        let content_x = area.x + if show_button { ERROR_BUTTON_W + 1 } else { 1 };
        let content_w = area
            .width
            .saturating_sub(content_x - area.x)
            .saturating_sub(if show_button { 0 } else { 1 });
        let content_row = |row: Rect| Rect {
            x: content_x,
            width: content_w,
            ..row
        };

        let mut idx = 0;
        if has_status {
            let style = if self.status_line == FIRST_RUN_HINT {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Yellow)
            };
            Paragraph::new(Line::from(Span::styled(self.status_line.clone(), style)))
                .wrap(Wrap { trim: true })
                .render(
                    rows[idx].inner(Margin {
                        horizontal: 1,
                        vertical: 0,
                    }),
                    buf,
                );
            idx += 1;
        }

        self.render_version_row(buf, content_row(rows[idx]));
        idx += 1;

        // Blank spacer row between the version line and the zen band.
        idx += 1;

        let zen_area = if show_button {
            content_row(rows[idx])
        } else {
            rows[idx]
        };
        self.render_zen_row(buf, zen_area);

        if show_button {
            let button = Rect {
                x: area.x,
                y: rows[first_fixed].y,
                width: ERROR_BUTTON_W,
                height: ERROR_BUTTON_H,
            };
            render_error_button(buf, button);
        }
    }

    /// Daemon/CLI version segment: dim on match, red on mismatch; nothing until
    /// the first probe. Shares the row with the board-freshness banner.
    fn render_version_row(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let mut spans: Vec<Span> = Vec::new();
        if let Some(line) = self.daemon_version_line.clone() {
            let style = if self.daemon_version_mismatch {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            spans.push(Span::styled(line, style));
        }
        if let Some(banner) = &self.board_banner {
            let stale = banner.contains("stale") || banner.contains("no daemon");
            let color = if stale { Color::Yellow } else { Color::DarkGray };
            let prefix = if spans.is_empty() { "" } else { "  " };
            spans.push(Span::styled(
                format!("{prefix}{banner}"),
                Style::default().fg(color),
            ));
        }
        if spans.is_empty() {
            return;
        }
        Paragraph::new(Line::from(spans)).render(area, buf);
    }

    /// Zen row: a full-width green band carrying a bold `ZEN MODE ON` only while
    /// Zen is on. Off or paused paints nothing — the row stays blank so the
    /// version line above keeps its place.
    fn render_zen_row(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if !matches!(self.zen_mode, ZenModeState::On) {
            return;
        }
        let style = Style::default()
            .bg(ZEN_BAND_BG)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD);
        let width = area.width as usize;
        let label = "ZEN MODE ON";
        let label_w = label.chars().count();
        let line = if width <= label_w {
            label.chars().take(width).collect::<String>()
        } else {
            let pad = width - label_w;
            let left = 1;
            let right = pad - left;
            format!("{}{}{}", " ".repeat(left), label, " ".repeat(right))
        };
        Paragraph::new(Line::from(Span::styled(line, style))).render(area, buf);
    }
}

/// The sidebar's derived sub-rects (see [`SidebarView::geometry`]).
struct SidebarGeometry {
    header: Rect,
    search: Option<Rect>,
    nav: Rect,
    rest: Option<Rect>,
    footer: Rect,
}

/// Height of the header block: project title (row 0), a half-row margin
/// (row 1), the 2-row search box drawn across three cell rows with the
/// half-block bleed (rows 2–4: `▄` top, label, `▀` bottom), and a half-row
/// margin (row 5). The nav block's own leading separator sits below that.
const HEADER_H: u16 = 6;

/// 1-col horizontal padding shared by the title, the search box, the nav
/// labels, and the rest-of-list rows. The nav section's full-width fill
/// deliberately bypasses it while its label text re-applies it.
const LIST_INDENT: Margin = Margin {
    horizontal: 1,
    vertical: 0,
};

/// Build the ordered row list from the model, mirroring the old renderer's
/// `rows()`: a fixed nav, then the machine-grouped workspace pool (or the
/// config-error / loading placeholder), then the Ready / Queued review
/// sections. Each section header and its rows drop together when empty.
fn build_rows(model: &SidebarModel) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    for nav in &model.nav {
        rows.push(Row::Nav {
            glyph: nav_glyph(&nav.label),
            label: nav.label.clone(),
            target: target_from_view(&nav.view),
        });
    }

    // Cold board, nothing loaded yet: a dim "Loading…" placeholder.
    if model.board_loading {
        rows.push(Row::Blank);
        rows.push(Row::Loading);
        return rows;
    }

    if let Some(message) = &model.config_error {
        rows.push(Row::Blank);
        rows.push(Row::Section("Workspaces".into()));
        rows.push(Row::ConfigError(message.clone()));
    } else if !model.workspaces.is_empty() {
        rows.push(Row::Blank);
        rows.push(Row::Section("Workspaces".into()));
        // Group by machine when the project declares more than one; a
        // single-machine project collapses to a flat list.
        let machines: Vec<&str> = {
            let mut seen: Vec<&str> = Vec::new();
            for w in &model.workspaces {
                if !seen.iter().any(|m| *m == w.machine) {
                    seen.push(&w.machine);
                }
            }
            seen
        };
        let grouped = machines.len() > 1;
        if grouped {
            for machine in &machines {
                let machine_str = *machine;
                let on_machine: Vec<_> = model
                    .workspaces
                    .iter()
                    .filter(|w| w.machine == machine_str)
                    .collect();
                let total = on_machine.len();
                let active = on_machine
                    .iter()
                    .filter(|w| w.current_task.is_some())
                    .count();
                let collapsed = model.collapsed_machines.contains(machine_str);
                rows.push(Row::MachineGroup {
                    name: machine_str.to_string(),
                    collapsed,
                    total,
                    active,
                    target: RowTarget::Machine(machine_str.to_string()),
                });
                if collapsed {
                    continue;
                }
                for w in on_machine {
                    rows.push(Row::Workspace {
                        name: w.name.clone(),
                        badge: w.badge,
                        agent: w.agent.clone(),
                        indent: true,
                        target: RowTarget::Session(SessionRef::Workspace(w.name.clone())),
                    });
                }
            }
        } else {
            for w in &model.workspaces {
                rows.push(Row::Workspace {
                    name: w.name.clone(),
                    badge: w.badge,
                    agent: w.agent.clone(),
                    indent: false,
                    target: RowTarget::Session(SessionRef::Workspace(w.name.clone())),
                });
            }
        }
    }

    // Ready for Review — Serving / Loading rows (already on a slot).
    let ready: Vec<_> = model
        .reviews
        .iter()
        .filter(|r| !matches!(r.state, ReviewState::Pending))
        .collect();
    if !ready.is_empty() {
        rows.push(Row::Blank);
        rows.push(Row::Section("Ready for Review".into()));
        for r in ready {
            rows.push(Row::Review {
                title: r.title.clone(),
                branch: r.branch.clone(),
                location: r.location.clone(),
                state: r.state,
                target: RowTarget::Review(r.task_id.clone()),
            });
        }
    }

    // Queued for Review — Pending rows (waiting for a free slot).
    let queued: Vec<_> = model
        .reviews
        .iter()
        .filter(|r| matches!(r.state, ReviewState::Pending))
        .collect();
    if !queued.is_empty() {
        rows.push(Row::Blank);
        rows.push(Row::Section("Queued for Review".into()));
        for r in queued {
            rows.push(Row::Review {
                title: r.title.clone(),
                branch: r.branch.clone(),
                location: r.location.clone(),
                state: r.state,
                target: RowTarget::Review(r.task_id.clone()),
            });
        }
    }

    rows
}

/// One nav item row. Selected rows fill edge to edge with the selection
/// background (padding the label to the full width) and render white/bold when
/// focused (dim gray otherwise); unselected rows are plain gray with no fill.
fn nav_item_line(
    glyph: &str,
    label: &str,
    selected: bool,
    focused: bool,
    width: usize,
) -> Line<'static> {
    let text = format!(" {glyph} {label}");
    if selected {
        let pad = width.saturating_sub(text.chars().count());
        let fg = if focused { Color::White } else { Color::Gray };
        let mut style = Style::default().fg(fg).bg(SELECTION_BG);
        if focused {
            style = style.add_modifier(Modifier::BOLD);
        }
        Line::from(Span::styled(format!("{text}{}", " ".repeat(pad)), style))
    } else {
        Line::from(Span::styled(text, Style::default().fg(Color::Gray)))
    }
}

fn render_row(row: &Row, selected: bool, focused: bool, width: usize) -> ListItem<'static> {
    match row {
        Row::Nav { .. } => ListItem::new(Line::raw("")), // nav never reaches the rest-list
        Row::Section(label) => ListItem::new(Line::from(Span::styled(
            format!("— {label} —"),
            Style::default().fg(Color::DarkGray),
        ))),
        Row::ConfigError(message) => {
            let style = Style::default().fg(Color::Red);
            let lines: Vec<Line> = config_error_lines(message, width)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, style)))
                .collect();
            ListItem::new(lines)
        }
        Row::Blank => ListItem::new(Line::raw("")),
        Row::Loading => ListItem::new(Line::from(Span::styled(
            "  Loading…",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ))),
        Row::MachineGroup {
            name,
            collapsed,
            total,
            active,
            ..
        } => {
            let glyph = if *collapsed { "▸" } else { "▾" };
            let header_style = name_style(selected, focused);
            let left = vec![Span::styled(format!("{glyph} {name}"), header_style)];
            if *collapsed {
                let right_label = format!("({total}, {active} active)");
                let spans =
                    right_align(left, right_label, Style::default().fg(Color::DarkGray), width);
                ListItem::new(Line::from(spans))
            } else {
                ListItem::new(Line::from(left))
            }
        }
        Row::Workspace {
            name,
            badge,
            agent,
            indent,
            ..
        } => {
            let leading = if *indent { "  " } else { "" };
            let left = vec![
                Span::raw(leading),
                Span::styled(
                    format!("{} ", badge.glyph()),
                    Style::default().fg(decoration_to_color(badge.decoration_color())),
                ),
                Span::styled(name.clone(), name_style(selected, focused)),
            ];
            let right_label = match agent {
                Some(a) => title_case(a),
                None => "idle".to_string(),
            };
            let spans = right_align(left, right_label, Style::default().fg(Color::DarkGray), width);
            ListItem::new(Line::from(spans))
        }
        Row::Review {
            title,
            branch,
            location,
            state,
            ..
        } => {
            let dec = state.decoration();
            let badge = Span::styled(
                format!("{} ", dec.glyph),
                Style::default().fg(decoration_to_color(dec.color)),
            );
            // Badge is one glyph plus a trailing space; keep the title within the
            // remaining width so a long title clips with `…` rather than
            // overflowing. A right-aligned location then drops before the title.
            let title_text = truncate_ellipsis(title, width.saturating_sub(2));
            let title_span = Span::styled(title_text, name_style(selected, focused));
            let line1 = match location {
                Some(loc) => right_align(
                    vec![badge, title_span],
                    loc.clone(),
                    Style::default().fg(Color::DarkGray),
                    width,
                ),
                None => vec![badge, title_span],
            };
            let branch_style = if selected {
                let fg = if focused { Color::Gray } else { Color::DarkGray };
                Style::default().fg(fg).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::DarkGray)
            };
            // Branch line: a 2-col indent, the `⎇` glyph, then the branch
            // truncated with `…` to fit (matching the Figma review row).
            const BRANCH_INDENT: &str = "  ";
            const BRANCH_MARKER: &str = "⎇ ";
            let avail = width.saturating_sub(BRANCH_INDENT.len() + BRANCH_MARKER.chars().count());
            let branch_text = truncate_ellipsis(branch, avail);
            let line2 = Line::from(Span::styled(
                format!("{BRANCH_INDENT}{BRANCH_MARKER}{branch_text}"),
                branch_style,
            ));
            ListItem::new(vec![Line::from(line1), line2])
        }
    }
}

/// Selected-row text style: white/bold when focused, dim gray when not. The row
/// fill comes from the list's highlight style, so this sets only fg/weight.
fn name_style(selected: bool, focused: bool) -> Style {
    if selected {
        let fg = if focused { Color::White } else { Color::Gray };
        let mut s = Style::default().fg(fg);
        if focused {
            s = s.add_modifier(Modifier::BOLD);
        }
        s
    } else {
        Style::default().fg(Color::Gray)
    }
}

/// Pad `left` so a right-aligned `right` ends at `width`. The right column is
/// dropped (rather than pushing the title off-screen) if there isn't room.
fn right_align(
    left: Vec<Span<'static>>,
    right: String,
    right_style: Style,
    width: usize,
) -> Vec<Span<'static>> {
    let left_w: usize = left.iter().map(|s| s.content.chars().count()).sum();
    let right_w = right.chars().count();
    if right.is_empty() || left_w + right_w + 1 > width {
        return left;
    }
    let pad = width.saturating_sub(left_w + right_w);
    let mut out = left;
    out.push(Span::raw(" ".repeat(pad)));
    out.push(Span::styled(right, right_style));
    out
}

/// Uppercase the first character of a lowercase identifier (`developer` →
/// `Developer`). Empty input is preserved.
fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Truncate `s` to at most `max` columns (by char count), appending `…` when it
/// would overflow. `max == 0` yields an empty string; `max == 1` yields `…`.
fn truncate_ellipsis(s: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 1 {
        return "…".to_string();
    }
    let keep: String = s.chars().take(max - 1).collect();
    format!("{keep}…")
}

/// Draw the search box: a 2-row-tall filled box carrying a `🔍 Search` label on
/// the left and a right-aligned palette-chord hint (e.g. `⌃P`). The box is
/// drawn across three cell rows with the same half-block bleed the nav
/// selection bar uses — a lower-half-block row on top (fill in its bottom half,
/// a half-row of padding above the label), the label row on a solid fill, and
/// an upper-half-block row below (fill in its top half, a half-row of padding
/// below). The half-block glyphs carry the fill as their *foreground* on the
/// default background, so the eye reads the fill as half a cell tall on those
/// rows. The chord is dropped when there isn't room for it.
fn render_search_box(buf: &mut Buffer, area: Rect, chord: &str) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;

    // The box occupies rows [top, top+2]: `▄` bleed, label, `▀` bleed. When the
    // area is shorter (a clamped header) we draw what fits, label row first.
    let top = area.top();
    let label_y = (top + 1).min(area.bottom().saturating_sub(1));
    let bottom = top + 2;

    // Top / bottom half-block bleed rows: the fill as the glyph's foreground on
    // the default background, exactly like the nav selection bleed.
    let bleed_style = Style::default().fg(SEARCH_BG);
    if label_y > top {
        Paragraph::new(Line::from(Span::styled(BLEED_ABOVE.repeat(width), bleed_style)))
            .render(Rect { y: top, height: 1, ..area }, buf);
    }
    if bottom < area.bottom() {
        Paragraph::new(Line::from(Span::styled(BLEED_BELOW.repeat(width), bleed_style)))
            .render(Rect { y: bottom, height: 1, ..area }, buf);
    }

    // Label row, on the solid search fill.
    let label = " 🔍 Search";
    let label_w = label.chars().count();
    let chord_hint = if chord.is_empty() {
        String::new()
    } else {
        format!("{chord} ")
    };
    let chord_w = chord_hint.chars().count();

    let mut spans = vec![Span::styled(
        label.to_string(),
        Style::default().fg(Color::Gray).bg(SEARCH_BG),
    )];
    // Append the chord hint only when the label and it both fit.
    if chord_w > 0 && label_w + chord_w < width {
        let pad = width - label_w - chord_w;
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(SEARCH_BG)));
        spans.push(Span::styled(
            chord_hint,
            Style::default().fg(Color::DarkGray).bg(SEARCH_BG),
        ));
    }
    Paragraph::new(Line::from(spans)).render(Rect { y: label_y, height: 1, ..area }, buf);
    // Re-fill the label row's background last: a double-width glyph resets its
    // trailing continuation cell when the text is laid down, so paint over every
    // cell to keep the row a solid bar. Only the label row is re-filled — the
    // bleed rows must keep their default background so the half-block reads as
    // half a cell of fill rather than a full one. Setting bg leaves the symbols
    // and fg intact.
    for x in area.left()..area.right() {
        buf[(x, label_y)].set_bg(SEARCH_BG);
    }
}

/// Draw the unread-errors button: a solid reddish-gray 5×3 block with a bold
/// white `!` in the center cell.
fn render_error_button(buf: &mut Buffer, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let style = Style::default()
        .bg(ERROR_BUTTON_BG)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let mid_col = (area.width / 2) as usize;
    let mid_row = area.height / 2;
    let lines: Vec<Line> = (0..area.height)
        .map(|r| {
            let content: String = (0..area.width as usize)
                .map(|c| if r == mid_row && c == mid_col { '!' } else { ' ' })
                .collect();
            Line::from(Span::styled(content, style))
        })
        .collect();
    Paragraph::new(lines).render(area, buf);
}

/// Height in list lines a rest-of-list row occupies at inner `width`. Review
/// rows are two-line; a config error wraps; everything else is one line.
fn row_height(row: &Row, width: usize) -> usize {
    match row {
        Row::Review { .. } => 2,
        Row::ConfigError(message) => config_error_lines(message, width).len(),
        _ => 1,
    }
}

/// Word-wrap a config-error message into sidebar list lines. The first line
/// carries a `! ` marker; continuation lines indent two columns. Long words
/// hard-split rather than overflow.
fn config_error_lines(message: &str, width: usize) -> Vec<String> {
    let body_width = width.saturating_sub(2).max(1);
    let wrapped = wrap_words(message, body_width);
    if wrapped.is_empty() {
        return vec!["! ".to_string()];
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, line)| if i == 0 { format!("! {line}") } else { format!("  {line}") })
        .collect()
}

/// Greedy word-wrap `text` to `width` columns (by char count); a word longer
/// than `width` is hard-split.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            if !chunk.is_empty() {
                current = chunk;
            }
            continue;
        }
        let candidate = if current.is_empty() {
            word.to_string()
        } else {
            format!("{current} {word}")
        };
        if candidate.chars().count() > width {
            lines.push(std::mem::take(&mut current));
            current = word.to_string();
        } else {
            current = candidate;
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn nav_glyph(label: &str) -> &'static str {
    match label {
        "Chat" => "💬",
        "Issues" => "📋",
        "Activity" => "⚡",
        "Machines" => "🖥",
        _ => "•",
    }
}

/// Map a shelbi-app nav `View` to a shell route. The orchestrator chat is the
/// session named `"orch"`; every other `Session` view names a workspace; the
/// rest are native views.
fn target_from_view(view: &View) -> RowTarget {
    match view {
        View::Session(name) if name == "orch" => RowTarget::Session(SessionRef::Orchestrator),
        View::Session(name) => RowTarget::Session(SessionRef::Workspace(name.clone())),
        other => RowTarget::Native(other.clone()),
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

/// Claim the one-time first-run hint, else surface a keymap-diagnostic count.
/// The persisted first-run hint wins so a genuinely fresh launch displays the
/// required wording exactly; a state error fails closed (no repeat-prone hint).
/// Ported from the former `crate::sidebar_startup_status_line`.
pub(crate) fn sidebar_startup_status_line(diagnostics: usize) -> String {
    match shelbi_state::claim_first_run_hint() {
        Ok(true) => return FIRST_RUN_HINT.to_string(),
        Ok(false) => {}
        Err(error) => tracing::warn!(
            %error,
            "could not claim first-run hint; skipping non-durable onboarding copy"
        ),
    }
    if diagnostics > 0 {
        let suffix = if diagnostics == 1 { "" } else { "s" };
        format!("⚠ {diagnostics} startup warning{suffix} — see ~/.shelbi/logs/tui.log")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_app::view::{NavItem, ReviewRow, WorkspaceRow};
    use shelbi_state::keymap::KeyChord;

    fn nav() -> Vec<NavItem> {
        vec![
            NavItem {
                label: "Chat".into(),
                view: View::Session("orch".into()),
            },
            NavItem {
                label: "Issues".into(),
                view: View::Issues,
            },
            NavItem {
                label: "Activity".into(),
                view: View::Activity,
            },
        ]
    }

    fn empty_model() -> SidebarModel {
        SidebarModel {
            project_label: "demo".into(),
            nav: nav(),
            workspaces: Vec::new(),
            reviews: Vec::new(),
            config_error: None,
            board_loading: false,
            collapsed_machines: Default::default(),
            board_banner: None,
            daemon_version_line: None,
            daemon_version_mismatch: false,
            status_line: String::new(),
            zen_mode: ZenModeState::Off,
            unread_errors: 0,
        }
    }

    fn ws(name: &str, machine: &str, task: Option<&str>, agent: Option<&str>, badge: WorkspaceBadge) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            machine: machine.into(),
            is_remote: machine != "hub",
            current_task: task.map(str::to_string),
            agent: agent.map(str::to_string),
            badge,
        }
    }

    fn review(task_id: &str, title: &str, branch: &str, location: Option<&str>, state: ReviewState) -> ReviewRow {
        ReviewRow {
            task_id: task_id.into(),
            title: title.into(),
            branch: branch.into(),
            location: location.map(str::to_string),
            workspace: None,
            state,
        }
    }

    fn chrome(palette: Option<&str>) -> SidebarChrome {
        let mut keymaps = Keymaps::default();
        if let Some(c) = palette {
            keymaps
                .global
                .by_action
                .insert(GlobalAction::OpenPalette, vec![KeyChord::parse(c).unwrap()]);
        } else {
            keymaps.global.by_action.remove(&GlobalAction::OpenPalette);
        }
        SidebarChrome::from_keymaps(&keymaps, DisplayStyle::detect())
    }

    fn default_chrome() -> SidebarChrome {
        chrome(Some("ctrl-p"))
    }

    /// Flatten a rendered sidebar into per-row strings.
    fn render_rows(model: &SidebarModel, selection: usize, w: u16, h: u16) -> Vec<String> {
        render_rows_with(model, selection, true, &default_chrome(), w, h)
    }

    fn render_rows_with(
        model: &SidebarModel,
        selection: usize,
        focused: bool,
        chrome: &SidebarChrome,
        w: u16,
        h: u16,
    ) -> Vec<String> {
        let view = SidebarView::build(model);
        let backend = TestBackend::new(w, h);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, selection, focused, chrome)
        })
        .unwrap();
        dump(&term)
    }

    fn dump(term: &Terminal<TestBackend>) -> Vec<String> {
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    fn row_y(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("expected row containing {needle:?} in:\n{}", rows.join("\n")))
    }

    // --- nav ----------------------------------------------------------------

    /// A blank separator line sits between every pair of nav items (and above
    /// the first / below the last), so moving the selection never shifts where
    /// the labels land.
    #[test]
    fn nav_separators_keep_labels_from_shifting_across_selection() {
        let model = empty_model();
        let chat = render_rows(&model, 0, 24, 20); // Chat
        let activity = render_rows(&model, 2, 24, 20); // Activity

        for label in ["Chat", "Issues", "Activity"] {
            assert_eq!(
                row_y(&chat, label),
                row_y(&activity, label),
                "'{label}' must not move when the selection changes"
            );
        }
        assert_eq!(
            row_y(&chat, "Issues") - row_y(&chat, "Chat"),
            2,
            "one separator line always sits between adjacent nav items"
        );
    }

    /// The selected nav item's adjacent lines carry the full-width half-block
    /// bleed (U+2584 above, U+2580 below); the selection fill spans the column
    /// edge to edge while the label text keeps the 1-col indent.
    #[test]
    fn selected_nav_item_renders_full_width_half_block_bleed() {
        let width = 24u16;
        let model = empty_model();
        let view = SidebarView::build(&model);
        let backend = TestBackend::new(width, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 1, true, &default_chrome())
        })
        .unwrap(); // Issues selected
        let buf = term.backend().buffer().clone();
        let rows = dump(&term);

        let tasks_y = row_y(&rows, "Issues");
        assert_eq!(
            rows[tasks_y - 1],
            BLEED_ABOVE.repeat(width as usize),
            "the line above the selection is full-width U+2584"
        );
        assert_eq!(
            rows[tasks_y + 1],
            BLEED_BELOW.repeat(width as usize),
            "the line below the selection is full-width U+2580"
        );
        // Unselected items keep plain blank separators.
        let chat_y = row_y(&rows, "Chat");
        assert!(
            rows[chat_y - 1].trim().is_empty(),
            "the line above unselected Chat stays blank, got: {:?}",
            rows[chat_y - 1]
        );
        // Selection fill paints edge to edge; column 0 is the indent gutter.
        let ty = tasks_y as u16;
        assert_eq!(
            buf[(0, ty)].bg,
            SELECTION_BG,
            "left edge (indent gutter) carries the selection fill"
        );
        for x in (width - 4)..width {
            assert_eq!(buf[(x, ty)].bg, SELECTION_BG, "right edge padding carries the fill, col {x}");
        }
        assert_eq!(buf[(0, ty)].symbol(), " ", "column 0 is the indent gutter, not the label");
    }

    // --- workspaces ---------------------------------------------------------

    /// Multi-machine layout: the `Workspaces` header, a `▾ <machine>` header per
    /// host, each workspace on its own indented row with its agent (or `idle`).
    #[test]
    fn workspaces_section_renders_grouped_layout_with_agent_column() {
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("charlie", "hub", None, None, WorkspaceBadge::Idle),
            ws("delta", "devbox", Some("t-2"), Some("developer"), WorkspaceBadge::Working),
        ];
        let rows = render_rows(&model, 0, 28, 22);
        let joined = rows.join("\n");

        assert!(joined.contains("Workspaces"), "section header, got:\n{joined}");
        assert!(!joined.contains("— Agents —"), "old label gone, got:\n{joined}");
        assert!(joined.contains("▾ hub"), "hub group header, got:\n{joined}");
        assert!(joined.contains("▾ devbox"), "devbox group header, got:\n{joined}");

        let alpha_y = row_y(&rows, "alpha");
        assert!(rows[alpha_y].contains("Developer"), "active row shows title-cased agent: {:?}", rows[alpha_y]);
        let charlie_y = row_y(&rows, "charlie");
        assert!(rows[charlie_y].contains("idle"), "idle row shows the placeholder: {:?}", rows[charlie_y]);
        assert!(
            rows[alpha_y].starts_with("   "),
            "grouped rows indent under their machine header, got: {:?}",
            rows[alpha_y]
        );
    }

    /// A collapsed machine renders `▸ <name>  (<total>, <active> active)` and
    /// hides its workspace rows; an expanded machine keeps its workspaces (with
    /// the active Working badge, distinct from the collapsed `▸`).
    #[test]
    fn collapsed_machine_renders_count_suffix_and_hides_workspaces() {
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("bravo", "hub", None, None, WorkspaceBadge::Idle),
            ws("charlie", "hub", None, None, WorkspaceBadge::Idle),
            ws("delta", "devbox", None, None, WorkspaceBadge::Idle),
        ];
        model.collapsed_machines.insert("hub".into());
        let rows = render_rows(&model, 0, 40, 22);
        let joined = rows.join("\n");

        let hub_y = row_y(&rows, "▸ hub");
        assert!(rows[hub_y].contains("(3, 1 active)"), "count suffix, got: {:?}", rows[hub_y]);
        assert!(!joined.contains("alpha"), "collapsed hub hides its workspaces, got:\n{joined}");
        assert!(!joined.contains("bravo") && !joined.contains("charlie"), "all hidden, got:\n{joined}");
        assert!(joined.contains("▾ devbox"), "devbox stays expanded, got:\n{joined}");
        assert!(joined.contains("delta"), "expanded devbox surfaces its workspace, got:\n{joined}");
    }

    #[test]
    fn collapsed_machine_glyph_and_active_workspace_glyph_do_not_collide() {
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("delta", "devbox", Some("t-2"), Some("developer"), WorkspaceBadge::Working),
        ];
        model.collapsed_machines.insert("devbox".into());
        let rows = render_rows(&model, 0, 40, 22);
        let joined = rows.join("\n");

        let working_glyph = WorkspaceBadge::Working.glyph();
        assert_ne!(working_glyph, "▸", "Working badge must not reuse the collapsed glyph");
        assert!(joined.contains("▾ hub"));
        let alpha_y = row_y(&rows, "alpha");
        assert!(rows[alpha_y].contains(working_glyph), "expanded active workspace uses the badge: {:?}", rows[alpha_y]);
        let devbox_y = row_y(&rows, "▸ devbox");
        assert!(!rows[devbox_y].contains(working_glyph), "collapsed header doesn't borrow the badge: {:?}", rows[devbox_y]);
    }

    /// Single-machine layout: a flat list, no `▾ <machine>` divider, no indent.
    #[test]
    fn single_machine_skips_group_header_and_indent() {
        let mut model = empty_model();
        model.workspaces = vec![ws("alpha", "hub", Some("t-1"), Some("qa"), WorkspaceBadge::Working)];
        let rows = render_rows(&model, 0, 28, 20);
        let joined = rows.join("\n");

        assert!(joined.contains("Workspaces"));
        assert!(!joined.contains("▾ hub"), "single-machine projects skip the group header, got:\n{joined}");
        let alpha_y = row_y(&rows, "alpha");
        assert!(!rows[alpha_y].starts_with("   "), "flat rows skip the indent, got: {:?}", rows[alpha_y]);
        assert!(rows[alpha_y].contains("Qa"), "agent name title-cases verbatim, got: {:?}", rows[alpha_y]);
    }

    /// A broken project config renders a visible red error row under the
    /// Workspaces header, naming the file and the reason.
    #[test]
    fn config_error_renders_visible_row_naming_file_and_reason() {
        let mut model = empty_model();
        model.project_label = "Shelbi".into();
        model.config_error = Some(
            "project config ~/.shelbi/projects/Shelbi.yaml has an invalid id `Shelbi` \
             (only lowercase ASCII letters, digits, `-`, and `_` are allowed)"
                .into(),
        );
        let rows = render_rows(&model, 0, 40, 24);
        let joined = rows.join("\n");

        assert!(joined.contains("Workspaces"), "header stays so the error reads in context, got:\n{joined}");
        assert!(joined.contains('!'), "the error row carries a `!` marker, got:\n{joined}");
        assert!(joined.contains("invalid"), "the reason is visible, got:\n{joined}");
        assert!(joined.contains("Shelbi.yaml"), "the offending file is named, got:\n{joined}");
    }

    // --- review -------------------------------------------------------------

    /// Both review sections render as two-line entries: line 1 = badge + title
    /// (+ a right-aligned URL for a serving item), line 2 = branch. Serving uses
    /// ✓, loading uses ▶ (no ✓), queued uses ·.
    #[test]
    fn review_sections_render_two_line_entries_with_badge_url_and_branch() {
        let mut model = empty_model();
        model.reviews = vec![
            review("palette", "Palette fuzzy-match fix", "shelbi/palette-fuzzy-match-fix", Some("hub:3000"), ReviewState::Serving),
            review("nav", "Homepage nav fix", "shelbi/homepage-nav-fix", None, ReviewState::Loading),
            review("onboarding", "Rework onboarding copy", "shelbi/rework-onboarding-copy", None, ReviewState::Pending),
        ];
        let rows = render_rows(&model, 0, 44, 26);
        let joined = rows.join("\n");

        assert!(joined.contains("Ready for Review"), "Ready header, got:\n{joined}");
        assert!(joined.contains("Queued for Review"), "Queued header, got:\n{joined}");

        let ready_y = row_y(&rows, "Palette fuzzy-match fix");
        assert!(rows[ready_y].contains('✓'), "Ready row carries ✓, got: {:?}", rows[ready_y]);
        assert!(rows[ready_y].contains("hub:3000"), "Ready row line 1 carries the URL, got: {:?}", rows[ready_y]);
        assert!(
            rows[ready_y + 1].contains("shelbi/palette-fuzzy-match-fix"),
            "branch renders on the next line, got: {:?}",
            rows[ready_y + 1]
        );

        let loading_y = row_y(&rows, "Homepage nav fix");
        assert!(
            row_y(&rows, "Ready for Review") < loading_y && loading_y < row_y(&rows, "Queued for Review"),
            "loading row renders under Ready, above Queued, got:\n{joined}"
        );
        assert!(rows[loading_y].contains('▶'), "loading row carries ▶, got: {:?}", rows[loading_y]);
        assert!(!rows[loading_y].contains('✓'), "loading row has no ✓, got: {:?}", rows[loading_y]);
        assert!(!rows[loading_y].contains(':'), "loading row has no location, got: {:?}", rows[loading_y]);

        let queued_y = row_y(&rows, "Rework onboarding copy");
        assert!(rows[queued_y].contains('·'), "Queued row carries ·, got: {:?}", rows[queued_y]);
        assert!(!rows[queued_y].contains(':'), "queued row has no location, got: {:?}", rows[queued_y]);
        assert!(
            rows[queued_y + 1].contains("shelbi/rework-onboarding-copy"),
            "queued branch renders directly below, got: {:?}",
            rows[queued_y + 1]
        );

        assert!(
            row_y(&rows, "Ready for Review") < row_y(&rows, "Queued for Review"),
            "Ready section renders above Queued, got:\n{joined}"
        );
    }

    /// The branch line carries the `⎇` marker and truncates a long branch with
    /// `…` so it never overflows the sidebar width.
    #[test]
    fn review_branch_line_shows_marker_and_truncates() {
        let mut model = empty_model();
        model.reviews = vec![review(
            "wh",
            "Validate webhook payloads",
            "shelbi/validate-webhook-payloads-and-signatures",
            None,
            ReviewState::Pending,
        )];
        let rows = render_rows(&model, 0, 26, 22);
        // The title row carries "Validate webhook" (capital V); the branch line
        // is the lowercase slug below it.
        let title_y = row_y(&rows, "Validate webhook");
        let branch_line = &rows[title_y + 1];
        assert!(branch_line.contains('⎇'), "branch line carries the ⎇ marker, got: {branch_line:?}");
        assert!(branch_line.contains('…'), "a too-long branch truncates with …, got: {branch_line:?}");
        assert!(
            branch_line.contains("shelbi/validate"),
            "the branch prefix is still visible, got: {branch_line:?}"
        );
    }

    /// A flat (single-machine) layout: the busy row shows the green `⏵` glyph and
    /// a right-aligned agent name; the idle row shows the dim `·` and `idle`.
    #[test]
    fn workspace_rows_show_busy_and_idle_glyphs_with_right_label() {
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("charlie", "hub", None, None, WorkspaceBadge::Idle),
        ];
        let view = SidebarView::build(&model);
        let backend = TestBackend::new(30, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term);

        let alpha_y = row_y(&rows, "alpha");
        assert!(rows[alpha_y].contains('⏵'), "busy row carries the ⏵ glyph, got: {:?}", rows[alpha_y]);
        assert!(rows[alpha_y].contains("Developer"), "busy row right-aligns the agent, got: {:?}", rows[alpha_y]);
        let glyph_x = rows[alpha_y].find('⏵').unwrap() as u16;
        assert_eq!(buf[(glyph_x, alpha_y as u16)].fg, Color::Green, "busy glyph is green");

        let charlie_y = row_y(&rows, "charlie");
        assert!(rows[charlie_y].contains('·'), "idle row carries the · glyph, got: {:?}", rows[charlie_y]);
        assert!(rows[charlie_y].contains("idle"), "idle row shows the idle placeholder, got: {:?}", rows[charlie_y]);
    }

    // --- header / search ----------------------------------------------------

    /// The header renders the project name in the accent cyan and a filled
    /// search box carrying `🔍 Search` and the resolved palette chord.
    #[test]
    fn header_renders_project_title_and_search_box() {
        let mut model = empty_model();
        model.project_label = "My project".into();
        let view = SidebarView::build(&model);
        let backend = TestBackend::new(40, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term);
        let joined = rows.join("\n");

        // Title on the first row, in cyan bold.
        let title_y = row_y(&rows, "My project");
        assert_eq!(title_y, 0, "project title is the first row, got:\n{joined}");
        let title_x = rows[title_y].find('M').unwrap() as u16;
        assert_eq!(buf[(title_x, title_y as u16)].fg, Color::Cyan, "title is cyan");

        // Search box with the magnifier, label, and the mac/linux palette chord.
        let search_y = row_y(&rows, "Search");
        let want_chord = match DisplayStyle::detect() {
            DisplayStyle::Mac => "⌃P",
            DisplayStyle::Linux => "Ctrl+P",
        };
        assert!(rows[search_y].contains('🔍'), "search box shows the magnifier, got: {:?}", rows[search_y]);
        assert!(rows[search_y].contains(want_chord), "search box shows the palette chord {want_chord:?}, got: {:?}", rows[search_y]);
        // The label row is a solid fill. The magnifier is double-width, so the
        // single column after it is a wide-char continuation the TestBackend
        // never flushes (the emoji covers it on a real terminal); assert the
        // leading gutter cell and the whole single-width region past the emoji,
        // which proves the bar spans the full box width.
        let sy = search_y as u16;
        assert_eq!(buf[(1, sy)].bg, SEARCH_BG, "search box fills the leading cell");
        for x in 5..39u16 {
            assert_eq!(buf[(x, sy)].bg, SEARCH_BG, "search box fills its row, col {x}");
        }

        // The box is 2 rows tall, drawn with the half-block bleed: a `▄` row of
        // lower-half blocks above the label and a `▀` row of upper-half blocks
        // below it, both carrying the search fill as their *foreground* (the
        // half shows the fill; the cell background stays the terminal default).
        let top_y = sy - 1;
        let bottom_y = sy + 1;
        for x in 1..39u16 {
            assert_eq!(buf[(x, top_y)].symbol(), "▄", "top bleed row is lower-half blocks, col {x}");
            assert_eq!(buf[(x, top_y)].fg, SEARCH_BG, "top bleed carries the fill as fg, col {x}");
            assert_eq!(buf[(x, bottom_y)].symbol(), "▀", "bottom bleed row is upper-half blocks, col {x}");
            assert_eq!(buf[(x, bottom_y)].fg, SEARCH_BG, "bottom bleed carries the fill as fg, col {x}");
        }
        // A blank half-row margin sits above (below the title) and below the box.
        assert!(rows[(top_y - 1) as usize].trim().is_empty(), "blank margin above the box, got: {:?}", rows[(top_y - 1) as usize]);
        assert!(rows[(bottom_y + 1) as usize].trim().is_empty(), "blank margin below the box, got: {:?}", rows[(bottom_y + 1) as usize]);
    }

    /// A click anywhere in the 2-row box — including the `▄` / `▀` half-block
    /// rows — maps to the search box (opens the palette); the title above and
    /// the first nav row below do not.
    #[test]
    fn search_box_hit_covers_all_three_cell_rows() {
        let view = SidebarView::build(&empty_model());
        let area = Rect { x: 0, y: 0, width: 40, height: 20 };
        // Rows 2, 3, 4 are the box (`▄`, label, `▀`); row 0 is the title, row 1
        // and row 5 the margins, row 6 the nav's leading separator.
        for y in 2..=4u16 {
            assert!(view.search_box_hit(area, 3, y), "click on box row {y} hits the search box");
        }
        assert!(!view.search_box_hit(area, 3, 0), "title row is not the search box");
        assert!(!view.search_box_hit(area, 3, 1), "top margin is not the search box");
        assert!(!view.search_box_hit(area, 3, 5), "bottom margin is not the search box");
        // The gutter column (0) and the divider-adjacent last column fall
        // outside the inset box.
        assert!(!view.search_box_hit(area, 0, 3), "left gutter is outside the inset box");
    }

    /// The nav block (and everything below it) shifts down by the taller header,
    /// and a click on the first nav row still resolves to its selection index.
    #[test]
    fn taller_search_box_shifts_nav_click_targets_down() {
        let view = SidebarView::build(&empty_model());
        let area = Rect { x: 0, y: 0, width: 40, height: 20 };
        // Header is 6 rows (0 title, 1 margin, 2–4 box, 5 margin); the nav's
        // leading separator is row 6, so the first nav item ("Chat") sits on
        // row 7.
        assert_eq!(view.hit(area, 3, 7), Some(0), "first nav item is on row 7 and selects index 0");
        assert_eq!(view.hit(area, 3, 6), None, "the nav separator row is not selectable");
    }

    /// With no palette chord bound the search box falls back to the `<unbound>`
    /// marker rather than a key hint.
    #[test]
    fn search_box_shows_unbound_when_palette_unbound() {
        let rows = render_rows_with(&empty_model(), 0, true, &chrome(None), 40, 20);
        let search_y = row_y(&rows, "Search");
        assert!(
            rows[search_y].contains("<unbound>"),
            "search box shows <unbound> when no palette chord is bound, got: {:?}",
            rows[search_y]
        );
    }

    // --- footer -------------------------------------------------------------

    #[test]
    fn first_run_hint_wraps_without_clipping_at_sidebar_max_width() {
        let mut model = empty_model();
        model.status_line = FIRST_RUN_HINT.to_string();
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 40, 16);
        let joined = rows.join("\n");
        assert!(joined.contains("Ctrl+P palette · type E to edit"), "first line of the hint is missing:\n{joined}");
        assert!(joined.contains("settings"), "the final word must not be clipped:\n{joined}");
    }

    /// The version row paints the daemon/CLI segment, red on mismatch.
    #[test]
    fn version_row_renders_daemon_cli_segment() {
        let mut model = empty_model();
        model.daemon_version_line = Some("daemon 0.10.0 · cli 0.10.0".into());
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 40, 16);
        assert!(rows.join("\n").contains("daemon 0.10.0 · cli 0.10.0"));
    }

    #[test]
    fn zen_row_renders_full_width_band_when_on() {
        let mut model = empty_model();
        model.daemon_version_line = Some("daemon 0.9.0 · cli 0.9.0".into());
        model.zen_mode = ZenModeState::On;
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 24, 16);
        let joined = rows.join("\n");
        assert!(joined.contains("ZEN MODE ON"), "expected ZEN MODE ON in:\n{joined}");
        assert!(!joined.contains("┌─") && !joined.contains("└─"), "no border chrome, got:\n{joined}");
        // The band is the bottom row, full width, with one blank spacer row
        // between it and the version line above.
        let version_y = row_y(&rows, "daemon 0.9.0");
        let zen_y = row_y(&rows, "ZEN MODE ON");
        assert_eq!(zen_y, version_y + 2, "one blank row between version and zen, got:\n{joined}");
        assert_eq!(zen_y, rows.len() - 1, "the zen band is the bottom row, got:\n{joined}");
        assert!(
            rows[zen_y - 1].trim().is_empty(),
            "a blank spacer sits above the band, got: {:?}",
            rows[zen_y - 1]
        );
        assert_eq!(rows[zen_y].chars().count(), 24, "zen row spans full width, got: {:?}", rows[zen_y]);
    }

    /// Off: no band and — unlike the old sidebar — no `Zen mode` hint line.
    #[test]
    fn zen_off_shows_no_band_and_no_hint() {
        let mut model = empty_model();
        model.zen_mode = ZenModeState::Off;
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 24, 16);
        let joined = rows.join("\n");
        assert!(!joined.contains("ZEN MODE ON"), "no band when off, got:\n{joined}");
        assert!(!joined.contains("Zen mode"), "no hotkey hint line, got:\n{joined}");
    }

    #[test]
    fn paused_shows_no_band() {
        let mut model = empty_model();
        model.zen_mode = ZenModeState::Paused;
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 24, 16);
        let joined = rows.join("\n");
        assert!(!joined.contains("ZEN MODE ON"), "paused shows no band, got:\n{joined}");
        assert!(!joined.contains("Zen mode"), "paused shows no hint, got:\n{joined}");
    }

    #[test]
    fn version_line_stays_put_across_zen_toggle() {
        let mut off = empty_model();
        off.daemon_version_line = Some("daemon 0.9.0 · cli 0.9.0".into());
        off.zen_mode = ZenModeState::Off;
        let off_rows = render_rows_with(&off, 0, true, &default_chrome(), 24, 16);
        let mut on = off.clone();
        on.zen_mode = ZenModeState::On;
        let on_rows = render_rows_with(&on, 0, true, &default_chrome(), 24, 16);
        assert_eq!(
            row_y(&off_rows, "daemon 0.9.0"),
            row_y(&on_rows, "daemon 0.9.0"),
            "version line must not move when toggling Zen"
        );
    }

    /// The unread-errors button: a 5×3 reddish-gray block with a centered `!`
    /// in the footer's bottom-left, only when there are unread errors.
    #[test]
    fn unread_error_button_appears_only_with_unread_errors() {
        // No unread errors: no button fill in the bottom-left.
        let model = empty_model();
        let view = SidebarView::build(&model);
        let mut term = Terminal::new(TestBackend::new(40, 16)).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        assert_ne!(buf[(0, 15)].bg, ERROR_BUTTON_BG, "no button block without unread errors");

        // With unread errors: a 5×3 block anchored bottom-left, `!` centered.
        let mut model = empty_model();
        model.unread_errors = 3;
        let view = SidebarView::build(&model);
        let mut term = Terminal::new(TestBackend::new(40, 16)).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        // The block occupies the bottom 3 rows, leftmost 5 columns.
        let top = 16 - ERROR_BUTTON_H;
        for dy in 0..ERROR_BUTTON_H {
            for dx in 0..ERROR_BUTTON_W {
                assert_eq!(
                    buf[(dx, top + dy)].bg,
                    ERROR_BUTTON_BG,
                    "button cell ({dx},{dy}) carries the reddish-gray fill"
                );
            }
        }
        let cx = ERROR_BUTTON_W / 2;
        let cy = top + ERROR_BUTTON_H / 2;
        assert_eq!(buf[(cx, cy)].symbol(), "!", "the `!` sits in the center cell");
    }

    // --- hit-testing --------------------------------------------------------

    /// Selectable rows skip section headers and route correctly.
    #[test]
    fn selectable_rows_skip_section_headers_and_route_correctly() {
        let mut model = empty_model();
        model.workspaces = vec![ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working)];
        model.reviews = vec![review("rev-9", "Fix it", "shelbi/fix-it", None, ReviewState::Pending)];
        let v = SidebarView::build(&model);
        // 3 nav + 1 workspace (flat) + 1 review = 5 selectable rows.
        assert_eq!(v.selectable_count(), 5);
        assert_eq!(v.target_at(0), Some(RowTarget::Session(SessionRef::Orchestrator)));
        assert_eq!(v.target_at(1), Some(RowTarget::Native(View::Issues)));
        assert_eq!(v.target_at(2), Some(RowTarget::Native(View::Activity)));
        assert_eq!(v.target_at(3), Some(RowTarget::Session(SessionRef::Workspace("alpha".into()))));
        assert_eq!(v.target_at(4), Some(RowTarget::Review("rev-9".into())));
        assert_eq!(v.target_at(5), None);
    }

    /// Click hit-testing maps a click to the right selectable ordinal across
    /// the nav block (separators non-selectable), the machine group header, a
    /// grouped two-line review entry, and the branch line underneath it.
    #[test]
    fn hit_testing_maps_rows_with_two_line_entries_and_group_headers() {
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("delta", "devbox", None, None, WorkspaceBadge::Idle),
        ];
        model.reviews = vec![review("rev-9", "Fix it", "shelbi/fix-it", Some("hub:3000"), ReviewState::Serving)];
        let v = SidebarView::build(&model);
        let area = Rect::new(0, 0, 40, 30);

        // Render so we can resolve the rendered y of each label, then hit-test.
        let rows = render_rows(&model, 0, 40, 30);

        // Nav: item lines map to their ordinal; separators map to nothing.
        let chat_y = row_y(&rows, "Chat") as u16;
        assert_eq!(v.hit(area, 2, chat_y), Some(0), "Chat → ordinal 0");
        assert_eq!(v.hit(area, 2, chat_y - 1), None, "the separator above Chat is inert");
        let activity_y = row_y(&rows, "Activity") as u16;
        assert_eq!(v.hit(area, 2, activity_y), Some(2), "Activity → ordinal 2");

        // Section headers are inert; the machine group header is selectable.
        let ws_header_y = row_y(&rows, "Workspaces") as u16;
        assert_eq!(v.hit(area, 2, ws_header_y), None, "the Workspaces header is inert");
        let hub_y = row_y(&rows, "▾ hub") as u16;
        assert_eq!(v.hit(area, 2, hub_y), Some(3), "the hub group header → ordinal 3");
        let alpha_y = row_y(&rows, "alpha") as u16;
        assert_eq!(v.hit(area, 2, alpha_y), Some(4), "alpha workspace → ordinal 4");

        // The two-line review entry maps both its title line and its branch
        // line to the same selectable ordinal.
        let review_title_y = row_y(&rows, "Fix it") as u16;
        let branch_y = row_y(&rows, "shelbi/fix-it") as u16;
        let title_hit = v.hit(area, 2, review_title_y);
        assert!(title_hit.is_some(), "the review title line is selectable");
        assert_eq!(
            v.hit(area, 2, branch_y),
            title_hit,
            "the branch line maps to the same ordinal as its title"
        );

        // A click past the sidebar / below the rows is None.
        assert_eq!(v.hit(area, 99, chat_y), None, "outside the sidebar");
    }

    #[test]
    fn renders_without_panicking_on_a_small_area() {
        let model = empty_model();
        let view = SidebarView::build(&model);
        let area = Rect::new(0, 0, 10, 3);
        let mut buf = Buffer::empty(area);
        view.render(&mut buf, area, 0, true, &default_chrome()); // must not panic
    }
}



