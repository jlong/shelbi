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
//! (the one-time first-run hint or a status line; a red daemon/CLI version
//! warning, naming the fix, only while the two disagree; the toolbar,
//! mirroring the desktop's: Settings, Send Feedback, and the unread-errors
//! badge while there are any, in its bottom-left corner; and the `ZEN MODE ON`
//! band shown only while Zen is on). The versions otherwise wait in the
//! Settings button's hover name, and the board's freshness shows on the Issues
//! board. The old `^P
//! palette q quit` / `^G Zen mode` footer hint lines are gone; those actions
//! stay reachable through the palette and their existing keys.
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
use shelbi_app::view::{ReviewState, SidebarModel, SidebarRow, WorkspaceBadge};
use shelbi_state::keymap::{DisplayStyle, GlobalAction, Keymaps};
use shelbi_state::ZenModeState;

use super::session::SessionRef;
use crate::keymap::format_chord_or_unbound;
use crate::panel;
use crate::sidebar::{decoration_to_color, nav_lines};
use crate::theme::palette;
use shelbi_palette::DecorationColor;
use unicode_width::UnicodeWidthStr;

/// Exact one-time orientation copy shown in the sidebar footer after the first
/// project scaffold. One constant so persistence and wrapping paths can't drift
/// from the product wording (ported from the former `crate::FIRST_RUN_HINT`).
pub(crate) const FIRST_RUN_HINT: &str = "Ctrl+P palette · type E to edit settings";

/// Rows the footer status reserves for the orchestrator supervisor's failure.
const ORCH_STATUS_ROWS: u16 = 3;

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

/// A button in the footer's toolbar, left to right in the desktop's order
/// (Figma Navigation's footer Toolbar): Settings, Send Feedback, then the
/// unread-errors badge while there are any.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FooterButton {
    Settings,
    Feedback,
    Errors,
}

impl FooterButton {
    /// The glyph centered in the button's square (the `📣` is two cells
    /// wide), or the word when emoji can't draw.
    fn face(self, emoji: bool) -> &'static str {
        match (self, emoji) {
            (FooterButton::Settings, true) => "⚙",
            (FooterButton::Feedback, true) => "📣",
            (FooterButton::Settings, false) => "Settings",
            (FooterButton::Feedback, false) => "Feedback",
            (FooterButton::Errors, _) => "!",
        }
    }

    /// The desktop's tooltip for the button, shown beside the toolbar while
    /// the pointer is over it.
    /// The button's name beside the toolbar under the pointer. Settings
    /// carries the daemon/CLI versions as well, as the desktop's tooltip does:
    /// the footer itself only shows them on a mismatch.
    fn tooltip(self, unread_errors: usize, versions: Option<&str>) -> String {
        match self {
            FooterButton::Settings => match versions {
                Some(line) => format!("Settings · {line}"),
                None => "Settings".to_string(),
            },
            FooterButton::Feedback => "Send Feedback".to_string(),
            FooterButton::Errors => format!(
                "{unread_errors} unread error{}",
                if unread_errors == 1 { "" } else { "s" }
            ),
        }
    }
}

/// Per-frame footer/header input that isn't board state: the resolved
/// palette-open chord shown in the search box's `⌘K`-style hint. Owned (no
/// borrow of the shell state) so the draw closure can disjointly borrow the
/// rest of the state.
pub struct SidebarChrome {
    /// The palette-open chord resolved for the host platform (e.g. `⌃P` on
    /// macOS, `Ctrl+P` elsewhere), or `<unbound>` when no chord is bound.
    palette: String,
    /// The footer button under the pointer, lit with its hover fill and
    /// named beside the toolbar (the desktop's tooltip).
    hover: Option<FooterButton>,
}

impl SidebarChrome {
    /// Build the chrome from the resolved keymaps + platform convention.
    pub fn from_keymaps(keymaps: &Keymaps, display_style: DisplayStyle) -> Self {
        SidebarChrome {
            palette: format_chord_or_unbound(
                keymaps.global.first_chord_for(GlobalAction::OpenPalette),
                display_style,
            ),
            hover: None,
        }
    }

    /// The footer button the pointer is over (see [`SidebarView::footer_button_at`]).
    pub fn with_hover(self, hover: Option<FooterButton>) -> Self {
        SidebarChrome { hover, ..self }
    }

    /// The resolved palette-open chord (e.g. `⌃P`, `Ctrl+Space`), for callers
    /// that show the chord outside the sidebar — the idle-workspace placeholder.
    pub fn palette(&self) -> &str {
        &self.palette
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
        right_label: String,
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
    /// The daemon/CLI versions, for the Settings button's hover name.
    daemon_version_line: Option<String>,
    /// The version line while the daemon and the CLI disagree (it names the
    /// fix), painted red above the toolbar. The board's freshness shows on
    /// the Issues board, not here.
    version_warning: Option<String>,
    status_line: String,
    /// The status row carries the orchestrator supervisor's failure (painted
    /// red) rather than the launch-time status line.
    status_is_orchestrator_failure: bool,
    zen_mode: ZenModeState,
    unread_errors: usize,
    /// Emoji draw on this terminal (`Caps::emoji`); else the footer's buttons
    /// fall back to words.
    emoji: bool,
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
            daemon_version_line: model.daemon_version_line.clone(),
            version_warning: model
                .daemon_version_line
                .clone()
                .filter(|_| model.daemon_version_mismatch),
            // A dead orchestrator the supervisor couldn't bring back outranks
            // the launch-time hint: it is why the Orchestrator view won't load.
            status_line: model.footer_status(),
            status_is_orchestrator_failure: model.orchestrator_status.is_some(),
            zen_mode: model.zen_mode,
            unread_errors: model.unread_errors,
            emoji: true,
        }
    }

    /// Lay the footer's buttons out for a terminal that can (or can't) draw
    /// emoji.
    pub fn with_emoji(self, emoji: bool) -> Self {
        SidebarView { emoji, ..self }
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

    /// Footer height: the status row's height (0 when empty, 2 for the
    /// first-run hint, [`ORCH_STATUS_ROWS`] for an orchestrator supervisor
    /// failure, else 1), the version warning's rows on a daemon/CLI mismatch,
    /// and the four fixed rows: the toolbar's half-row margin above, the
    /// toolbar, its half-row margin below (the spacer above the band), and the
    /// zen band. The band paints only while Zen is on, but its row is always
    /// reserved so the toolbar never shifts when Zen toggles.
    fn footer_height(&self) -> u16 {
        self.status_row_height() + self.warning_lines().len() as u16 + 4
    }

    /// The version warning's lines: the mismatch, then the fix it names on a
    /// line of its own (`daemon 0.10.0 ≠ cli 0.11.0` / `shelbi daemon
    /// restart`), so neither is cut short on a sidebar too narrow for both.
    fn warning_lines(&self) -> Vec<&str> {
        self.version_warning
            .as_deref()
            .map(|line| line.split(" — ").collect())
            .unwrap_or_default()
    }

    fn status_row_height(&self) -> u16 {
        if self.status_line.is_empty() {
            0
        } else if self.status_is_orchestrator_failure {
            // The supervisor's reason is a sentence (it names the failed
            // session and why), so give it room to wrap.
            ORCH_STATUS_ROWS
        } else if self.status_line == FIRST_RUN_HINT {
            2
        } else {
            1
        }
    }

    /// The toolbar's buttons, left to right.
    fn footer_buttons(&self) -> Vec<FooterButton> {
        let mut buttons = vec![FooterButton::Settings, FooterButton::Feedback];
        if self.unread_errors > 0 {
            buttons.push(FooterButton::Errors);
        }
        buttons
    }

    /// Each toolbar button's columns on the toolbar `row`: one column in from
    /// the left edge, a column between buttons. A button that would reach the
    /// row's last column is left off.
    fn button_spans(&self, row: Rect) -> Vec<(FooterButton, Rect)> {
        let mut out = Vec::new();
        let mut x = row.x + LIST_INDENT.horizontal;
        let limit = row.right().saturating_sub(LIST_INDENT.horizontal);
        for button in self.footer_buttons() {
            let w = panel::square_button_width(button.face(self.emoji)) as u16;
            if x + w > limit {
                break;
            }
            out.push((button, Rect { x, width: w, ..row }));
            x += w + BUTTON_GAP;
        }
        out
    }

    /// Where the hovered button's `width`-column name goes, its row with a
    /// column of inset each side: beside the buttons, two columns past the
    /// last, when it fits whole there; otherwise the full width of the row
    /// above the squares' `▄` caps when the sidebar has one (cut short with
    /// `…` only when even that is too narrow); else what is left beside them.
    fn tooltip_rect(&self, area: Rect, rows: &FooterRows, toolbar: Rect, width: usize) -> Option<Rect> {
        let inset = LIST_INDENT.horizontal;
        let start = self.buttons_end(toolbar) + 2 - inset;
        let beside = (start + inset < toolbar.right().saturating_sub(inset)).then(|| Rect {
            x: start,
            width: toolbar.right() - start,
            ..toolbar
        });
        if beside.is_some_and(|r| r.width as usize >= width + 2 * inset as usize) {
            return beside;
        }
        let floating = rows
            .above
            .filter(|above| above.y > area.y)
            .map(|above| Rect { y: above.y - 1, ..toolbar });
        floating.or(beside)
    }

    /// Where the buttons end on a toolbar row starting at `x` (the column just
    /// past the last one).
    fn buttons_end(&self, row: Rect) -> u16 {
        self.button_spans(row)
            .last()
            .map_or(row.x + LIST_INDENT.horizontal, |(_, r)| r.right())
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
        let footer = self.footer_rows(outer[1]);
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
        // Header rhythm: a blank row (0), the project title (1), a blank row
        // (2), then the 2-row search box drawn across the three cell rows 3–5
        // (`▄` / label / `▀`). The box needs all three rows present (height ≥ 6).
        let search = (header.height >= 6).then(|| {
            Rect {
                y: header.y + 3,
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

    /// Lay the footer's rows out bottom-up in `footer`, so a footer squeezed
    /// by a short sidebar keeps the band and the toolbar and drops what sits
    /// above them first.
    fn footer_rows(&self, footer: Rect) -> FooterRows {
        // Peel one `h`-row rect off the bottom of what's left, if it fits.
        fn take(left: &mut Rect, h: u16) -> Option<Rect> {
            (h > 0 && left.height >= h).then(|| {
                left.height -= h;
                Rect {
                    y: left.bottom(),
                    height: h,
                    ..*left
                }
            })
        }
        let mut left = footer;
        let zen = take(&mut left, 1);
        let below = take(&mut left, 1);
        let toolbar = take(&mut left, 1);
        let above = take(&mut left, 1);
        let warning_h = (self.warning_lines().len() as u16).min(left.height);
        let warning = take(&mut left, warning_h);
        let status_h = self.status_row_height().min(left.height);
        let status = take(&mut left, status_h);
        FooterRows {
            status,
            warning,
            above,
            toolbar,
            below,
            zen,
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

    /// The footer button at `(x, y)`, if any: its columns across the three
    /// cell rows its square spans (the `▄` margin, the toolbar row, the `▀`
    /// margin), like the search box's target. The shell opens Settings, runs
    /// Send Feedback, or opens the error log.
    pub fn footer_button_at(&self, area: Rect, x: u16, y: u16) -> Option<FooterButton> {
        if area.width == 0 || area.height == 0 || !contains(area, x, y) {
            return None;
        }
        let rows = self.geometry(area).footer;
        let toolbar = rows.toolbar?;
        let top = rows.above.map_or(toolbar.y, |r| r.y);
        let bottom = rows.below.map_or(toolbar.bottom(), |r| r.bottom());
        if y < top || y >= bottom {
            return None;
        }
        self.button_spans(toolbar)
            .into_iter()
            .find(|(_, r)| x >= r.x && x < r.right())
            .map(|(b, _)| b)
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
        // The sidebar paints no full-area background: unfilled cells keep the
        // terminal's default background (`Color::Reset`) so a semi-transparent
        // terminal shows through. Only the components that are filled in the
        // Figma carry a fill — the search box, the selection blocks, and the zen
        // band. The half-block bleed rows draw their fill as the glyph's
        // *foreground* over that default background, so they blend with whatever
        // sits behind the sidebar rather than a painted black.
        let geo = self.geometry(area);

        self.render_header(buf, geo.header, geo.search, chrome);

        let sel_row = self.selected_row(selection);
        self.render_nav(buf, geo.nav, sel_row, focused);
        if let Some(rest) = geo.rest {
            self.render_rest(buf, rest, sel_row, focused);
        }
        self.render_footer(buf, area, &geo.footer, chrome.hover);
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
        // The title sits on row 1 (a blank row above it); fall back to row 0 on
        // a header clamped to a single row. Settings lives in the footer's
        // toolbar, so the title has the row to itself.
        let title_rect = Rect {
            y: header.y + if header.height >= 2 { 1 } else { 0 },
            height: 1,
            ..header
        }
        .inner(LIST_INDENT);
        Paragraph::new(Line::from(Span::styled(
            truncate_ellipsis(&self.project_label, title_rect.width as usize),
            Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
        )))
        .render(title_rect, buf);

        if let Some(search) = search {
            render_search_box(buf, search, &chrome.palette);
        }
    }

    /// Render the leading `Row::Nav` rows as an inset block: a separator line
    /// between (and bracketing) each item, the selected item's fill spanning the
    /// inset width (one column in from each edge, matching the search box), its
    /// adjacent separators carrying the half-block bleed. The label keeps a
    /// further 1-col inner pad inside that inset.
    fn render_nav(&self, buf: &mut Buffer, area: Rect, sel_row: Option<usize>, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // The nav item bounds — fill, half-block bleed, and label — are inset one
        // column from each side, exactly like the search box above, so the
        // selection block reads as the same width. The gutter columns stay on
        // the terminal's default (transparent) background.
        let inner = area.inner(LIST_INDENT);
        if inner.width == 0 {
            return;
        }
        let nav_n = self.nav_n();
        let width = inner.width as usize;
        let selected = sel_row.filter(|&r| r < nav_n);

        let content: Vec<Line> = (0..nav_n)
            .filter_map(|p| {
                if let Some(Row::Nav { glyph, label, .. }) = self.rows.get(p) {
                    Some(nav_item_line(glyph, label, selected == Some(p), focused, width))
                } else {
                    None
                }
            })
            .collect();
        let lines = crate::sidebar::bleed_block_lines(content, selected, width, palette().selection);
        Paragraph::new(lines).render(inner, buf);
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
        let list = List::new(items).highlight_style(Style::default().bg(palette().selection));
        StatefulWidget::render(list, area, buf, &mut state);
    }

    /// Paint the footer: the status row, the version warning on a daemon/CLI
    /// mismatch, the toolbar (Settings, Send Feedback, the unread-errors
    /// badge), and the zen band. `hover` is the button under the pointer: its
    /// square lifts and its name shows beside the toolbar, or, when the
    /// squares leave too little room for the whole name, floats on the row
    /// above them (over whatever sits there, as the desktop's tooltip does).
    fn render_footer(&self, buf: &mut Buffer, area: Rect, rows: &FooterRows, hover: Option<FooterButton>) {
        if let Some(status) = rows.status {
            let style = if self.status_is_orchestrator_failure {
                Style::default().fg(palette().ansi_red)
            } else if self.status_line == FIRST_RUN_HINT {
                Style::default().fg(palette().ansi_dark_gray)
            } else {
                Style::default().fg(palette().ansi_yellow)
            };
            Paragraph::new(Line::from(Span::styled(self.status_line.clone(), style)))
                .wrap(Wrap { trim: true })
                .render(status.inner(LIST_INDENT), buf);
        }

        // The mismatch in red, a line per part, inside the list inset; a line
        // too long for a narrow sidebar ends in `…`.
        if let Some(rows) = rows.warning {
            let rows = rows.inner(LIST_INDENT);
            let style = Style::default().fg(palette().ansi_red);
            for (i, part) in self.warning_lines().into_iter().enumerate().take(rows.height as usize) {
                let row = Rect {
                    y: rows.y + i as u16,
                    height: 1,
                    ..rows
                };
                Paragraph::new(Span::styled(truncate_ellipsis(part, row.width as usize), style))
                    .render(row, buf);
            }
        }

        if let Some(toolbar) = rows.toolbar {
            let hover = hover.filter(|h| self.footer_buttons().contains(h));
            for (button, rect) in self.button_spans(toolbar) {
                render_footer_button(
                    buf,
                    button,
                    button.face(self.emoji),
                    rect,
                    rows.above,
                    rows.below,
                    hover == Some(button),
                );
            }
            // Right of the buttons: the hovered button's name (the desktop's
            // tooltip), left-aligned. Nothing sits there otherwise.
            if let Some(button) = hover {
                let name = button.tooltip(self.unread_errors, self.daemon_version_line.as_deref());
                let info = self.tooltip_rect(area, rows, toolbar, UnicodeWidthStr::width(name.as_str()));
                if let Some(info) = info {
                    for x in info.left()..info.right() {
                        buf[(x, info.y)].reset();
                    }
                    Paragraph::new(Span::styled(
                        truncate_ellipsis(&name, info.inner(LIST_INDENT).width as usize),
                        Style::default().fg(palette().gray),
                    ))
                    .render(info.inner(LIST_INDENT), buf);
                }
            }
        }

        if let Some(zen) = rows.zen {
            self.render_zen_row(buf, zen);
        }
    }

    /// Zen row: a full-width green band carrying a bold `ZEN MODE ON` only while
    /// Zen is on. Off or paused paints nothing — the row stays blank so the
    /// toolbar above keeps its place.
    fn render_zen_row(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        if !matches!(self.zen_mode, ZenModeState::On) {
            return;
        }
        let style = Style::default()
            .bg(palette().zen)
            .fg(palette().on_color)
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
    footer: FooterRows,
}

/// The footer's rows (see [`SidebarView::footer_rows`]), top to bottom; each
/// `None` when a short sidebar squeezed it out (or, for `warning`, when the
/// daemon and the CLI agree).
#[derive(Debug, Default)]
struct FooterRows {
    status: Option<Rect>,
    /// The version warning's rows (a daemon/CLI mismatch only).
    warning: Option<Rect>,
    /// The toolbar's half-row margin above: the buttons' `▄` row.
    above: Option<Rect>,
    toolbar: Option<Rect>,
    /// The toolbar's half-row margin below (`▀`), and the spacer above the band.
    below: Option<Rect>,
    zen: Option<Rect>,
}

/// Columns between two toolbar buttons (the desktop's 4pt gap, rounded up so
/// two squares never touch).
const BUTTON_GAP: u16 = 1;

/// Height of the header block: a blank row (0), the project title (row 1), a
/// blank row (2), and the 2-row search box drawn across three cell rows with
/// the half-block bleed (rows 3–5: `▄` top, label, `▀` bottom). The nav block's
/// own leading separator sits directly below that.
const HEADER_H: u16 = 6;

/// 1-col horizontal padding shared by the title, the search box, the nav
/// labels, and the rest-of-list rows. The nav section's full-width fill
/// deliberately bypasses it while its label text re-applies it.
const LIST_INDENT: Margin = Margin {
    horizontal: 1,
    vertical: 0,
};

/// Build the ordered row list from the model. The layout itself (which rows,
/// in which order, with which labels) is [`SidebarModel::rows`], shared with the
/// desktop app; this only attaches the TUI's glyphs and routes.
fn build_rows(model: &SidebarModel) -> Vec<Row> {
    model
        .rows()
        .into_iter()
        .map(|row| match row {
            SidebarRow::Nav { label, view } => Row::Nav {
                glyph: nav_glyph(&label),
                label,
                target: target_from_view(&view),
            },
            SidebarRow::Section(label) => Row::Section(label),
            SidebarRow::ConfigError(message) => Row::ConfigError(message),
            SidebarRow::Blank => Row::Blank,
            SidebarRow::Loading => Row::Loading,
            SidebarRow::MachineGroup {
                name,
                collapsed,
                total,
                active,
            } => Row::MachineGroup {
                target: RowTarget::Machine(name.clone()),
                name,
                collapsed,
                total,
                active,
            },
            SidebarRow::Workspace {
                name,
                badge,
                right_label,
                indent,
            } => Row::Workspace {
                target: RowTarget::Session(SessionRef::Workspace(name.clone())),
                name,
                badge,
                right_label,
                indent,
            },
            SidebarRow::Review {
                task_id,
                title,
                branch,
                location,
                state,
            } => Row::Review {
                title,
                branch,
                location,
                state,
                target: RowTarget::Review(task_id),
            },
        })
        .collect()
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
        let fg = if focused { palette().white } else { palette().gray };
        let mut style = Style::default().fg(fg).bg(palette().selection);
        if focused {
            style = style.add_modifier(Modifier::BOLD);
        }
        Line::from(Span::styled(format!("{text}{}", " ".repeat(pad)), style))
    } else {
        Line::from(Span::styled(text, Style::default().fg(palette().gray)))
    }
}

fn render_row(row: &Row, selected: bool, focused: bool, width: usize) -> ListItem<'static> {
    match row {
        Row::Nav { .. } => ListItem::new(Line::raw("")), // nav never reaches the rest-list
        Row::Section(label) => ListItem::new(Line::from(Span::styled(
            format!("— {label} —"),
            Style::default().fg(palette().muted),
        ))),
        Row::ConfigError(message) => {
            let style = Style::default().fg(palette().ansi_red);
            let lines: Vec<Line> = config_error_lines(message, width)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, style)))
                .collect();
            ListItem::new(lines)
        }
        Row::Blank => ListItem::new(Line::raw("")),
        Row::Loading => ListItem::new(Line::from(Span::styled(
            "  Loading…",
            Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
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
                let spans = right_align(left, right_label, Style::default().fg(palette().muted), width);
                ListItem::new(Line::from(spans))
            } else {
                ListItem::new(Line::from(left))
            }
        }
        Row::Workspace {
            name,
            badge,
            right_label,
            indent,
            ..
        } => {
            let leading = if *indent { "  " } else { "" };
            let left = vec![
                Span::raw(leading),
                Span::styled(
                    format!("{} ", badge.glyph()),
                    Style::default().fg(badge_color(badge.decoration_color())),
                ),
                Span::styled(name.clone(), name_style(selected, focused)),
            ];
            // A skeleton row (state not read yet) carries an empty label rather
            // than a misleading "idle"; the `…` badge carries it.
            let spans =
                right_align(left, right_label.clone(), Style::default().fg(palette().muted), width);
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
            // Serving reads as a bold cyan ✓ (matching the review panel's bold
            // cyan "Ready for review" header); the queued `·` and loading `▶`
            // keep their weight so only the ready mark pops.
            let mut badge_style = Style::default().fg(badge_color(dec.color));
            if matches!(state, ReviewState::Serving) {
                badge_style = badge_style.add_modifier(Modifier::BOLD);
            }
            let badge = Span::styled(format!("{} ", dec.glyph), badge_style);
            // Badge is one glyph plus a trailing space; keep the title within the
            // remaining width so a long title clips with `…` rather than
            // overflowing. A right-aligned location then drops before the title.
            let title_text = truncate_ellipsis(title, width.saturating_sub(2));
            let title_span = Span::styled(title_text, name_style(selected, focused));
            let line1 = match location {
                Some(loc) => right_align(
                    vec![badge, title_span],
                    loc.clone(),
                    Style::default().fg(palette().muted),
                    width,
                ),
                None => vec![badge, title_span],
            };
            // The `⎇ branch` line is muted in every state; the selection fill
            // (not a text change) marks a selected review row.
            let branch_style = Style::default().fg(palette().muted);
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

/// Workspace / review / machine-name text style — always the normal
/// `color/gray` (#bababa). Unlike the nav label, these rows keep their text
/// colour when selected; only the `color/selection` row fill (from the list's
/// highlight style) marks the selection. Kept as a fn so every list row routes
/// its name through one place.
fn name_style(_selected: bool, _focused: bool) -> Style {
    Style::default().fg(palette().gray)
}

/// Colour for a row badge glyph (the workspace `⏵`/`·`, the review `✓`/`·`),
/// mapping the shared [`DecorationColor`] to the sidebar's exact Figma tokens:
/// the busy green, the muted bullet, the ready cyan, the normal gray. The
/// remaining decorations (yellow / red state badges) fall back to the shared
/// [`decoration_to_color`] mapping.
fn badge_color(c: DecorationColor) -> Color {
    match c {
        DecorationColor::Green => palette().green,
        DecorationColor::DarkGray => palette().muted,
        DecorationColor::Cyan => palette().cyan,
        DecorationColor::Gray => palette().gray,
        other => decoration_to_color(other),
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
/// terminal's default (transparent) background, so the eye reads the fill as
/// half a cell tall on those rows. The chord is dropped when there isn't room.
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
    // the terminal's default (transparent) background, exactly like the nav
    // selection bleed (and the settings' text fields, which share the caps).
    let (above, below) = crate::textbox::caps(width, palette().search);
    if label_y > top {
        Paragraph::new(above).render(Rect { y: top, height: 1, ..area }, buf);
    }
    if bottom < area.bottom() {
        Paragraph::new(below).render(Rect { y: bottom, height: 1, ..area }, buf);
    }

    // Label row, on the solid search fill. Measure in *display* columns, not
    // chars: the `🔍` is one `char` but two cells wide, so a char count would
    // undercount the label by a column and push the right-aligned chord one cell
    // past where the padding math intends — clipping the trailing inset and
    // leaving the chord flush against the box's right edge.
    let label = " 🔍 Search";
    let label_w = UnicodeWidthStr::width(label);
    let chord_w = UnicodeWidthStr::width(chord);

    // Both the label and the shortcut are the normal `color/gray` (#bababa), not
    // dimmed — the Figma search box reads them at the same weight.
    let mut spans = vec![Span::styled(
        label.to_string(),
        Style::default().fg(palette().gray).bg(palette().search),
    )];
    // Append the chord hint only when the label, the chord, and a one-column
    // right inset all fit. The chord sits one column in from the box's right
    // edge, mirroring the label's one-column inset on the left; that final
    // column is a blank fill cell (RIGHT_INSET wide).
    const RIGHT_INSET: usize = 1;
    if chord_w > 0 && label_w + chord_w + RIGHT_INSET <= width {
        let pad = width - label_w - chord_w - RIGHT_INSET;
        spans.push(Span::styled(" ".repeat(pad), Style::default().bg(palette().search)));
        spans.push(Span::styled(chord.to_string(), Style::default().fg(palette().gray).bg(palette().search)));
        spans.push(Span::styled(
            " ".repeat(RIGHT_INSET),
            Style::default().bg(palette().search),
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
        buf[(x, label_y)].set_bg(palette().search);
    }
}

/// Draw one toolbar button over `rect` (its toolbar-row cells) as a square,
/// the back button's ([`panel::draw_square_button`]): the glyph gray on the
/// selection fill, `▄` in the row above and `▀` in the row below, the fill
/// lifting to the lighter `button_hover` under the pointer, as the other
/// half-block buttons lift. The unread-errors badge is the same square in red, a bold `!` on the
/// reddish-gray `error_button`, a shade lighter under the pointer.
fn render_footer_button(
    buf: &mut Buffer,
    button: FooterButton,
    face: &str,
    rect: Rect,
    above: Option<Rect>,
    below: Option<Rect>,
    hovered: bool,
) {
    let p = palette();
    let (fill, style) = match (button, hovered) {
        (FooterButton::Errors, lit) => (
            if lit { p.error_button_hover } else { p.error_button },
            Style::default().fg(p.on_color).add_modifier(Modifier::BOLD),
        ),
        (_, lit) => (
            if lit { p.button_hover } else { p.selection },
            panel::square_glyph_style(false),
        ),
    };
    panel::draw_square_button(
        buf,
        rect.x,
        [above.map(|r| r.y), Some(rect.y), below.map(|r| r.y)],
        rect.width,
        face,
        fill,
        style,
    );
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

pub(crate) fn nav_glyph(label: &str) -> &'static str {
    // Shared with the palette's nav decorations (and the desktop app).
    shelbi_app::command_model::nav_glyph(label)
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
    use crate::sidebar::{BLEED_ABOVE, BLEED_BELOW};
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_app::view::{NavItem, ReviewRow, WorkspaceRow};
    use shelbi_state::keymap::KeyChord;

    fn nav() -> Vec<NavItem> {
        vec![
            NavItem {
                label: "Orchestrator".into(),
                view: View::Session("orch".into()),
            },
            NavItem {
                label: "Issues".into(),
                view: View::Issues,
            },
            NavItem {
                label: "Changelog".into(),
                view: View::Changelog,
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
            session_lost: None,
            orchestrator_status: None,
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
            number: None,
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
        let changelog = render_rows(&model, 2, 24, 20); // Changelog

        for label in ["Orchestrator", "Issues", "Changelog"] {
            assert_eq!(
                row_y(&chat, label),
                row_y(&changelog, label),
                "'{label}' must not move when the selection changes"
            );
        }
        assert_eq!(
            row_y(&chat, "Issues") - row_y(&chat, "Orchestrator"),
            2,
            "one separator line always sits between adjacent nav items"
        );
    }

    /// The selected nav item's fill and its half-block bleed (U+2584 above,
    /// U+2580 below) are inset one column from each edge — the same width as the
    /// search box — with the gutter columns left on the default (transparent)
    /// background. The selected label is white + bold; an unselected label is
    /// plain #bababa with no fill.
    #[test]
    fn selected_nav_item_renders_inset_half_block_bleed() {
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

        let issues_y = row_y(&rows, "Issues") as u16;
        // Gutters stay on the default (transparent) background; the inset region
        // carries the selection fill. (Columns 2–3 hold the double-width icon
        // continuation, which the TestBackend leaves unflushed, so sample
        // single-width cells.)
        assert_eq!(buf[(0, issues_y)].bg, Color::Reset, "left gutter stays transparent, not the fill");
        assert_eq!(buf[(width - 1, issues_y)].bg, Color::Reset, "right gutter stays transparent, not the fill");
        assert_eq!(buf[(1, issues_y)].bg, palette().selection, "the inset starts one column in with the selection fill");
        for x in (width - 4)..(width - 1) {
            assert_eq!(buf[(x, issues_y)].bg, palette().selection, "right-edge padding carries the fill, col {x}");
        }

        // Bleed rows above and below span only the inset region.
        let above = issues_y - 1;
        let below = issues_y + 1;
        for x in 1..(width - 1) {
            assert_eq!(buf[(x, above)].symbol(), BLEED_ABOVE, "bleed above is U+2584, col {x}");
            assert_eq!(buf[(x, above)].fg, palette().selection, "bleed above carries the fill as fg, col {x}");
            assert_eq!(buf[(x, below)].symbol(), BLEED_BELOW, "bleed below is U+2580, col {x}");
        }
        assert_eq!(buf[(0, above)].symbol(), " ", "the bleed gutter stays blank");
        assert_eq!(buf[(width - 1, above)].symbol(), " ", "the bleed right gutter stays blank");
        // Unselected items keep plain blank separators.
        let chat_y = row_y(&rows, "Orchestrator");
        assert!(
            rows[chat_y - 1].trim().is_empty(),
            "the line above unselected Chat stays blank, got: {:?}",
            rows[chat_y - 1]
        );

        // Selected label: white + bold. Unselected label: #bababa, no fill.
        let issues_x = rows[issues_y as usize].find('I').unwrap() as u16;
        assert_eq!(buf[(issues_x, issues_y)].fg, palette().white, "selected nav label is white");
        assert!(
            buf[(issues_x, issues_y)].modifier.contains(Modifier::BOLD),
            "selected nav label is bold"
        );
        let chat_x = rows[chat_y].find('O').unwrap() as u16;
        assert_eq!(buf[(chat_x, chat_y as u16)].fg, palette().gray, "unselected nav label is #bababa");
        assert_eq!(buf[(chat_x, chat_y as u16)].bg, Color::Reset, "unselected nav label has no fill");
        assert!(
            !buf[(chat_x, chat_y as u16)].modifier.contains(Modifier::BOLD),
            "unselected nav label is not bold"
        );
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
        let rows = render_rows(&model, 0, 28, 24);
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
        let rows = render_rows(&model, 0, 44, 28);
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

    /// The serving ✓ renders bold cyan (matching the review panel's bold-cyan
    /// "Ready for review" header) and the loading ▶ renders in the design's
    /// muted `#7a7a7a`, never alert-yellow. Inspects the drawn cell styles, not
    /// just the glyphs, so a regression in either tint is caught.
    #[test]
    fn review_badges_render_bold_cyan_check_and_muted_loading() {
        use ratatui::style::{Color, Modifier};

        let mut model = empty_model();
        model.reviews = vec![
            review("palette", "Palette fuzzy-match fix", "shelbi/pf", Some("hub:3000"), ReviewState::Serving),
            review("nav", "Homepage nav fix", "shelbi/nav", None, ReviewState::Loading),
        ];

        let view = SidebarView::build(&model);
        let backend = TestBackend::new(44, 26);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();

        let cell = |glyph: &str| {
            (0..buf.area.height)
                .flat_map(|y| (0..buf.area.width).map(move |x| (x, y)))
                .map(|(x, y)| &buf[(x, y)])
                .find(|c| c.symbol() == glyph)
                .unwrap_or_else(|| panic!("no {glyph:?} cell drawn"))
        };

        let check = cell("✓");
        assert_eq!(
            check.fg,
            palette().cyan,
            "serving ✓ is the Figma cyan accent",
        );
        assert!(
            check.modifier.contains(Modifier::BOLD),
            "serving ✓ is bold",
        );

        let play = cell("▶");
        assert_eq!(
            play.fg,
            palette().muted,
            "loading ▶ renders in the muted #7a7a7a tint",
        );
        assert_eq!(play.fg, Color::Rgb(122, 122, 122), "muted is #7a7a7a");
        assert_ne!(play.fg, Color::Yellow, "loading ▶ is never alert-yellow");
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
        let backend = TestBackend::new(30, 22);
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
        assert_eq!(buf[(glyph_x, alpha_y as u16)].fg, palette().green, "busy glyph is the #5acd25 green");
        // The workspace name and its right-aligned agent are #bababa / #7a7a7a.
        let name_x = rows[alpha_y].find("alpha").unwrap() as u16;
        assert_eq!(buf[(name_x, alpha_y as u16)].fg, palette().gray, "workspace name is #bababa");
        let dev_x = rows[alpha_y].find("Developer").unwrap() as u16;
        assert_eq!(buf[(dev_x, alpha_y as u16)].fg, palette().muted, "agent label is muted #7a7a7a");

        let charlie_y = row_y(&rows, "charlie");
        assert!(rows[charlie_y].contains('·'), "idle row carries the · glyph, got: {:?}", rows[charlie_y]);
        assert!(rows[charlie_y].contains("idle"), "idle row shows the idle placeholder, got: {:?}", rows[charlie_y]);
        let bullet_x = rows[charlie_y].find('·').unwrap() as u16;
        assert_eq!(buf[(bullet_x, charlie_y as u16)].fg, palette().muted, "idle bullet is muted #7a7a7a");
    }

    /// The selected workspace row fills with `color/selection` (#3f3f3f), inset
    /// one column from the left edge; the gutter stays on the default
    /// (transparent) background. The row's text colour is unchanged (only the
    /// fill marks it).
    #[test]
    fn selected_workspace_row_fills_inset_selection() {
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("charlie", "hub", None, None, WorkspaceBadge::Idle),
        ];
        let view = SidebarView::build(&model);
        let width = 30u16;
        let backend = TestBackend::new(width, 22);
        let mut term = Terminal::new(backend).unwrap();
        // 3 nav items precede the workspaces; ordinal 3 is the first workspace.
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 3, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term);

        let alpha_y = row_y(&rows, "alpha") as u16;
        assert_eq!(buf[(0, alpha_y)].bg, Color::Reset, "the left gutter stays transparent");
        assert_eq!(buf[(1, alpha_y)].bg, palette().selection, "the fill starts one column in");
        // The fill runs to roughly one column before the divider column.
        assert_eq!(buf[(width - 2, alpha_y)].bg, palette().selection, "the fill extends to the right inset");
        // Text colour is unchanged under the fill.
        let name_x = rows[alpha_y as usize].find("alpha").unwrap() as u16;
        assert_eq!(buf[(name_x, alpha_y)].fg, palette().gray, "selected workspace name keeps #bababa");

        // The unselected row below has no fill.
        let charlie_y = row_y(&rows, "charlie") as u16;
        assert_eq!(buf[(1, charlie_y)].bg, Color::Reset, "an unselected row has no fill");
    }

    // --- header / search ----------------------------------------------------

    /// The header renders a blank row, the project name in the accent cyan on
    /// row 1, a blank row, then the filled search box (rows 3–5) carrying
    /// `🔍 Search` and the resolved palette chord. The area paints no background;
    /// unfilled cells keep the default (transparent) background.
    #[test]
    fn header_renders_project_title_and_search_box() {
        let mut model = empty_model();
        model.project_label = "My project".into();
        let view = SidebarView::build(&model);
        let backend = TestBackend::new(40, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 1, true, &default_chrome()) // Issues selected
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term);
        let joined = rows.join("\n");

        // A blank row sits above the title; the title is on row 1, accent cyan.
        assert!(rows[0].trim().is_empty(), "a blank row sits above the title, got: {:?}", rows[0]);
        assert_eq!(buf[(1, 0)].bg, Color::Reset, "the sidebar paints no background (transparent)");
        let title_y = row_y(&rows, "My project");
        assert_eq!(title_y, 1, "project title is on the second row, got:\n{joined}");
        let title_x = rows[title_y].find('M').unwrap() as u16;
        assert_eq!(buf[(title_x, title_y as u16)].fg, palette().cyan, "title is the accent cyan #00a6b2");
        assert!(rows[2].trim().is_empty(), "a blank row sits below the title, got: {:?}", rows[2]);

        // Search box with the magnifier, label, and the mac/linux palette chord.
        let search_y = row_y(&rows, "Search");
        assert_eq!(search_y, 4, "the search label row is row 4, got:\n{joined}");
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
        assert_eq!(buf[(1, sy)].bg, palette().search, "search box fills the leading cell");
        for x in 5..39u16 {
            assert_eq!(buf[(x, sy)].bg, palette().search, "search box fills its row, col {x}");
        }
        // The label and the shortcut are both the normal #bababa (not dimmed).
        // (Column math, not `str::find`, since the double-width 🔍 makes byte
        // offsets diverge from columns.) The label " 🔍 Search" renders from the
        // box's inset left edge (x=1): space (1), 🔍 (2–3), space (4), S (5).
        assert_eq!(buf[(5, sy)].fg, palette().gray, "search label 'S' is #bababa");
        // The shortcut is right-aligned; scan in from the right for its last
        // glyph (past the one trailing space of inner padding).
        let chord_cell_x = (6..40u16)
            .rev()
            .find(|&x| buf[(x, sy)].symbol().trim() != "")
            .expect("the shortcut renders a glyph");
        assert_eq!(buf[(chord_cell_x, sy)].fg, palette().gray, "search shortcut is #bababa, not dimmed");

        // The box is 2 rows tall, drawn with the half-block bleed: a `▄` row of
        // lower-half blocks above the label and a `▀` row of upper-half blocks
        // below it, both carrying the search fill as their *foreground* on the
        // default (transparent) background (the half shows the fill; the cell
        // background stays the terminal default).
        let top_y = sy - 1;
        let bottom_y = sy + 1;
        for x in 1..39u16 {
            assert_eq!(buf[(x, top_y)].symbol(), "▄", "top bleed row is lower-half blocks, col {x}");
            assert_eq!(buf[(x, top_y)].fg, palette().search, "top bleed carries the fill as fg, col {x}");
            assert_eq!(buf[(x, top_y)].bg, Color::Reset, "top bleed keeps the default background, col {x}");
            assert_eq!(buf[(x, bottom_y)].symbol(), "▀", "bottom bleed row is upper-half blocks, col {x}");
            assert_eq!(buf[(x, bottom_y)].fg, palette().search, "bottom bleed carries the fill as fg, col {x}");
        }
        // A blank half-row margin sits above the box (below the title).
        assert!(rows[(top_y - 1) as usize].trim().is_empty(), "blank margin above the box, got: {:?}", rows[(top_y - 1) as usize]);
    }

    /// The palette-chord hint sits one column in from the search box's right
    /// edge, mirroring the label's one-column inset on the left — it is never
    /// flush against the edge. (A char-count of the double-width `🔍` used to
    /// mis-size the padding by a column and push the chord's last glyph onto the
    /// box's final column.)
    #[test]
    fn search_chord_is_inset_one_column_from_the_box_right_edge() {
        let view = SidebarView::build(&empty_model());
        let mut term = Terminal::new(TestBackend::new(40, 20)).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 1, true, &default_chrome())
        })
        .unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term);
        let sy = row_y(&rows, "Search") as u16;

        // The chord still renders (mac `⌃P` / linux `Ctrl+P`).
        let want_chord = match DisplayStyle::detect() {
            DisplayStyle::Mac => "⌃P",
            DisplayStyle::Linux => "Ctrl+P",
        };
        assert!(rows[sy as usize].contains(want_chord), "chord renders: {:?}", rows[sy as usize]);

        // The box fills cols 1..=38 on a 40-wide sidebar (a one-column gutter on
        // each side); col 38 is its right edge. The chord's last glyph must land
        // on col 37 — one column in — and col 38 must be a blank fill cell.
        let box_right = 38u16;
        let last_glyph_x = (0..buf.area.width)
            .rev()
            .find(|&x| !buf[(x, sy)].symbol().trim().is_empty())
            .expect("the chord renders a glyph on the search row");
        assert_eq!(
            last_glyph_x,
            box_right - 1,
            "the chord's last glyph is one column in from the box's right edge (col {}), got col {last_glyph_x}",
            box_right - 1
        );
        assert_eq!(
            buf[(box_right, sy)].symbol().trim(),
            "",
            "the box's right-edge column is a blank inset"
        );
        assert_eq!(
            buf[(box_right, sy)].bg,
            palette().search,
            "the one-column right inset is part of the box fill"
        );
    }

    /// A click anywhere in the 2-row box — including the `▄` / `▀` half-block
    /// rows — maps to the search box (opens the palette); the title above and
    /// the first nav row below do not.
    #[test]
    fn search_box_hit_covers_all_three_cell_rows() {
        let view = SidebarView::build(&empty_model());
        let area = Rect { x: 0, y: 0, width: 40, height: 20 };
        // Rows 3, 4, 5 are the box (`▄`, label, `▀`); row 0 is blank, row 1 the
        // title, row 2 blank, row 6 the nav's leading separator.
        for y in 3..=5u16 {
            assert!(view.search_box_hit(area, 3, y), "click on box row {y} hits the search box");
        }
        assert!(!view.search_box_hit(area, 3, 1), "title row is not the search box");
        assert!(!view.search_box_hit(area, 3, 2), "the margin above the box is not the search box");
        assert!(!view.search_box_hit(area, 3, 6), "the nav separator below the box is not the search box");
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
        // Header is 6 rows (0 blank, 1 title, 2 blank, 3–5 box); the nav's
        // leading separator is row 6, so the first nav item ("Orchestrator") sits on
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

    /// A supervisor give-up on the orchestrator takes the footer status row,
    /// in red, over the launch-time hint, wrapping instead of clipping.
    #[test]
    fn orchestrator_supervisor_failure_paints_red_on_the_status_row() {
        let mut model = empty_model();
        model.status_line = FIRST_RUN_HINT.to_string();
        model.orchestrator_status = Some("supervisor gave up: spawn refused".into());
        let view = SidebarView::build(&model);
        let mut term = Terminal::new(TestBackend::new(40, 16)).unwrap();
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &default_chrome())
        })
        .unwrap();
        let rows = dump(&term);
        let joined = rows.join("\n");
        assert!(joined.contains("Orchestrator: supervisor gave up:"), "got:\n{joined}");
        assert!(joined.contains("spawn refused"), "the reason wraps, not clips:\n{joined}");
        assert!(!joined.contains("Ctrl+P palette"), "the failure outranks the hint:\n{joined}");
        let y = row_y(&rows, "Orchestrator: supervisor") as u16;
        let x = rows[y as usize].find("Orchestrator: supervisor").unwrap() as u16;
        let buf = term.backend().buffer().clone();
        assert_eq!(buf[(x, y)].fg, palette().ansi_red);

        // Healthy orchestrator: no status, the hint comes back.
        model.orchestrator_status = None;
        let joined = render_rows_with(&model, 0, true, &default_chrome(), 40, 16).join("\n");
        assert!(!joined.contains("supervisor gave up"), "got:\n{joined}");
    }

    /// Matching versions and the board's freshness (warm, stale or cached)
    /// never show in the footer, at any width: the footer is the bare toolbar
    /// over the band, the rows the line used to take given back to the list.
    #[test]
    fn footer_shows_neither_versions_nor_the_board_banner() {
        for banner in ["board 12s", "board 4m stale · quota resets 16:03", "board 4m cached · no daemon"] {
            let mut model = empty_model();
            model.daemon_version_line = Some("daemon 0.11.0 · cli 0.11.0".into());
            model.board_banner = Some(banner.into());
            for w in [20, 24, 28, 36, 44, 60, 120] {
                let rows = render_rows_with(&model, 0, true, &default_chrome(), w, 20);
                let joined = rows.join("\n");
                assert!(!joined.contains("daemon"), "no versions at {w} cols:\n{joined}");
                assert!(!joined.contains("board"), "no board banner ({banner}) at {w} cols:\n{joined}");
                let view = SidebarView::build(&model);
                assert_eq!(view.footer_height(), 4, "toolbar, its margins and the band only");
                assert_eq!(row_y(&rows, "⚙"), 20 - 3, "the toolbar keeps its row at {w} cols");
            }
        }
    }

    /// A daemon/CLI mismatch still shows, in red above the toolbar, with the
    /// fix on a line of its own so a narrow sidebar cuts neither short.
    #[test]
    fn version_mismatch_warns_in_red_and_names_the_fix() {
        let mut model = empty_model();
        model.daemon_version_line = Some("daemon 0.10.0 ≠ cli 0.11.0 — shelbi daemon restart".into());
        model.daemon_version_mismatch = true;
        model.board_banner = Some("board 4m stale".into());
        let view = SidebarView::build(&model);
        assert_eq!(view.footer_height(), 6, "two warning rows over the toolbar");
        let buf = footer_buf(&view, None, 36, 20);
        let toolbar_y = 20 - 3;
        let (mismatch, fix) = (buf_row(&buf, toolbar_y - 3), buf_row(&buf, toolbar_y - 2));
        assert_eq!(mismatch.trim_end(), " daemon 0.10.0 ≠ cli 0.11.0");
        assert_eq!(fix.trim_end(), " shelbi daemon restart");
        assert_eq!(buf[(1, toolbar_y - 3)].fg, palette().ansi_red);
        assert_eq!(buf[(1, toolbar_y - 2)].fg, palette().ansi_red);
        assert!(buf_row(&buf, toolbar_y).starts_with("   ⚙"), "the toolbar sits below");
        let joined: String = (0..20).map(|y| buf_row(&buf, y)).collect::<Vec<_>>().join("\n");
        assert!(!joined.contains("board"), "still no board banner:\n{joined}");

        // Too narrow for the mismatch: it ends in `…` rather than wrapping
        // into the toolbar.
        let buf = footer_buf(&view, None, 20, 20);
        assert_eq!(buf_row(&buf, toolbar_y - 3).trim_end(), " daemon 0.10.0 ≠ c…");
        assert_eq!(buf_row(&buf, toolbar_y - 2).trim_end(), " shelbi daemon res…");

        // Fixed (the daemon restarted on the CLI's version): the warning goes.
        model.daemon_version_line = Some("daemon 0.11.0 · cli 0.11.0".into());
        model.daemon_version_mismatch = false;
        assert_eq!(SidebarView::build(&model).footer_height(), 4);
    }

    #[test]
    fn zen_row_renders_full_width_band_when_on() {
        let mut model = empty_model();
        model.zen_mode = ZenModeState::On;
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 24, 16);
        let joined = rows.join("\n");
        assert!(joined.contains("ZEN MODE ON"), "expected ZEN MODE ON in:\n{joined}");
        assert!(!joined.contains("┌─") && !joined.contains("└─"), "no border chrome, got:\n{joined}");
        // The band is the bottom row, full width, with one spacer row between
        // it and the toolbar above, holding only the squares' `▀` caps.
        let toolbar_y = row_y(&rows, "⚙");
        let zen_y = row_y(&rows, "ZEN MODE ON");
        assert_eq!(zen_y, toolbar_y + 2, "one spacer row between the toolbar and zen, got:\n{joined}");
        assert_eq!(zen_y, rows.len() - 1, "the zen band is the bottom row, got:\n{joined}");
        assert_eq!(
            rows[zen_y - 1].trim_end(),
            " ▀▀▀▀▀ ▀▀▀▀▀",
            "only the caps sit above the band, got: {:?}",
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
    fn footer_stays_put_across_zen_toggle() {
        let mut off = empty_model();
        off.daemon_version_line = Some("daemon 0.9.0 ≠ cli 0.11.0 — shelbi daemon restart".into());
        off.daemon_version_mismatch = true;
        off.zen_mode = ZenModeState::Off;
        let off_rows = render_rows_with(&off, 0, true, &default_chrome(), 30, 16);
        let mut on = off.clone();
        on.zen_mode = ZenModeState::On;
        let on_rows = render_rows_with(&on, 0, true, &default_chrome(), 30, 16);
        for text in ["daemon 0.9.0", "⚙"] {
            assert_eq!(
                row_y(&off_rows, text),
                row_y(&on_rows, text),
                "{text:?} must not move when toggling Zen"
            );
        }
    }

    // --- footer toolbar -------------------------------------------------------

    /// Render `view` at `w`×`h` with `hover`, returning the buffer.
    fn footer_buf(view: &SidebarView, hover: Option<FooterButton>, w: u16, h: u16) -> Buffer {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        let chrome = default_chrome().with_hover(hover);
        term.draw(|f| {
            let area = f.area();
            view.render(f.buffer_mut(), area, 0, true, &chrome)
        })
        .unwrap();
        term.backend().buffer().clone()
    }

    /// One buffer row as text, a double-width glyph's continuation cell
    /// skipped (it holds no symbol of its own).
    fn buf_row(buf: &Buffer, y: u16) -> String {
        let mut out = String::new();
        let mut x = 0;
        while x < buf.area.width {
            let sym = buf[(x, y)].symbol();
            out.push_str(sym);
            x += UnicodeWidthStr::width(sym).max(1) as u16;
        }
        out
    }

    /// Assert the square button over `cols` on toolbar row `y` of `buf`: `▄`
    /// above and `▀` below in `fill` as their foreground on the default
    /// background, the toolbar row solid `fill`.
    fn assert_square(buf: &Buffer, cols: std::ops::Range<u16>, y: u16, fill: Color, what: &str) {
        for x in cols {
            assert_eq!(buf[(x, y - 1)].symbol(), BLEED_ABOVE, "{what}: cap above, col {x}");
            assert_eq!(buf[(x, y - 1)].fg, fill, "{what}: cap above carries the fill, col {x}");
            assert_eq!(buf[(x, y - 1)].bg, Color::Reset, "{what}: the cap keeps the default bg");
            assert_eq!(buf[(x, y + 1)].symbol(), BLEED_BELOW, "{what}: cap below, col {x}");
            assert_eq!(buf[(x, y + 1)].fg, fill, "{what}: cap below carries the fill, col {x}");
            assert_eq!(buf[(x, y + 1)].bg, Color::Reset);
            assert_eq!(buf[(x, y)].bg, fill, "{what}: solid fill, col {x}");
        }
    }

    /// Settings then Send Feedback sit in the footer's bottom-left corner, the
    /// desktop's order, on the row above the spacer and the zen band; the title
    /// row has no gear any more. Each is a square button at rest, the back
    /// button's: [`panel::BACK_BTN_WIDTH`] wide on the selection fill, its
    /// glyph gray and centered (the double-width `📣` too).
    #[test]
    fn footer_toolbar_shows_settings_then_feedback_bottom_left() {
        let mut model = empty_model();
        model.project_label = "My project".into();
        let view = SidebarView::build(&model);
        let buf = footer_buf(&view, None, 40, 20);
        let toolbar_y = 20 - 3;
        let row = buf_row(&buf, toolbar_y);
        assert!(row.starts_with("   ⚙     📣  "), "Settings then Feedback, bottom-left: {row:?}");
        assert_eq!(buf[(3, toolbar_y)].symbol(), "⚙");
        assert_eq!(buf[(9, toolbar_y)].symbol(), "📣");
        assert_eq!(panel::BACK_BTN_WIDTH, 5);
        assert_square(&buf, 1..6, toolbar_y, palette().selection, "Settings");
        // The `📣`'s continuation cell (col 10) is never flushed by the
        // TestBackend; the emoji covers it on a real terminal.
        assert_square(&buf, 7..10, toolbar_y, palette().selection, "Feedback");
        assert_square(&buf, 11..12, toolbar_y, palette().selection, "Feedback");
        for x in [3, 9] {
            assert_eq!(buf[(x, toolbar_y)].fg, palette().ansi_gray, "the back button's glyph gray");
            assert!(!buf[(x, toolbar_y)].modifier.contains(Modifier::BOLD));
        }
        for x in [0, 6, 12, 13] {
            assert_eq!(buf[(x, toolbar_y)].bg, Color::Reset, "a gap, col {x}");
            assert!(buf[(x, toolbar_y - 1)].symbol().trim().is_empty(), "no cap over col {x}");
            assert!(buf[(x, toolbar_y + 1)].symbol().trim().is_empty(), "no cap under col {x}");
        }
        let title = buf_row(&buf, 1);
        assert!(!title.contains('⚙'), "the gear left the title row: {title:?}");
        assert!(title.contains("My project"));
    }

    /// The footer's squares are the back button's own: drawn by the same
    /// helper, a workspace panel's `←` and the sidebar's `⚙` come out cell for
    /// cell alike apart from the glyph.
    #[test]
    fn footer_squares_match_the_back_button() {
        let mut term = Terminal::new(TestBackend::new(12, 3)).unwrap();
        term.draw(|f| panel::render_back_button(f, Rect::new(0, 0, 12, 3), false, "", Style::default()))
            .unwrap();
        let back = term.backend().buffer().clone();
        let footer = footer_buf(&SidebarView::build(&empty_model()), None, 40, 20);
        let (bx, fx, fy) = (panel::PAD, 1u16, 20 - 3);
        for i in 0..panel::BACK_BTN_WIDTH as u16 {
            for dy in 0..3u16 {
                let (b, f) = (&back[(bx + i, dy)], &footer[(fx + i, fy - 1 + dy)]);
                assert_eq!((b.fg, b.bg, b.modifier), (f.fg, f.bg, f.modifier), "col {i} row {dy}");
                if !(dy == 1 && i == 2) {
                    assert_eq!(b.symbol(), f.symbol(), "col {i} row {dy}");
                }
            }
        }
        assert_eq!(back[(bx + 2, 1)].symbol(), "\u{2190}");
        assert_eq!(footer[(fx + 2, fy)].symbol(), "⚙");
    }

    /// The title has the whole row now that the gear is gone: a long project
    /// name only truncates at the row's inset.
    #[test]
    fn title_uses_the_whole_row_without_the_gear() {
        let mut model = empty_model();
        model.project_label = "abcdefghijklmnopqrstuvwxyz".into();
        let rows = render_rows(&model, 0, 28, 20);
        assert_eq!(rows[1].trim_end(), " abcdefghijklmnopqrstuvwxyz");
    }

    /// Under the pointer a button's square lifts to the lighter
    /// `button_hover` across its own columns only; its glyph stays
    /// gray. The other button stays at rest.
    #[test]
    fn hovered_button_lifts_its_square() {
        let view = SidebarView::build(&empty_model());
        let y = 20 - 3;
        for (button, cols, glyph_x) in [(FooterButton::Settings, 1..6u16, 3), (FooterButton::Feedback, 7..12u16, 9)] {
            let buf = footer_buf(&view, Some(button), 40, 20);
            for x in cols.clone().filter(|&x| x != 10) {
                assert_square(&buf, x..x + 1, y, palette().button_hover, &format!("{button:?}"));
            }
            assert_eq!(buf[(glyph_x, y)].fg, palette().ansi_gray, "{button:?}'s glyph");
            let other = if button == FooterButton::Settings { 7..10 } else { 1..6 };
            assert_square(&buf, other, y, palette().selection, "the other button");
        }
        assert_ne!(palette().button_hover, palette().selection, "the lift shows");
    }

    /// The hovered button's name (the desktop's tooltip) shows beside the
    /// toolbar; Settings's carries the daemon/CLI versions, as the desktop's
    /// does, which the footer no longer shows at rest.
    #[test]
    fn hover_names_the_button_beside_the_toolbar() {
        let mut model = empty_model();
        model.daemon_version_line = Some("daemon 0.11.0 · cli 0.11.0".into());
        model.unread_errors = 1;
        let view = SidebarView::build(&model);
        let y = 20 - 3;
        for (hover, want) in [
            (FooterButton::Settings, "Settings · daemon 0.11.0 · cli 0.11.0"),
            (FooterButton::Feedback, "Send Feedback"),
            (FooterButton::Errors, "1 unread error"),
        ] {
            let buf = footer_buf(&view, Some(hover), 60, 20);
            let row = buf_row(&buf, y);
            assert!(row.contains(want), "{hover:?} is named: {row:?}");
            assert!(row.starts_with("   ⚙     📣    !  "), "two columns past the squares: {row:?}");
            assert!(buf_row(&buf, y - 2).trim().is_empty(), "nothing floats when it fits beside");
        }
        let row = buf_row(&footer_buf(&view, None, 60, 20), y);
        assert_eq!(row.trim_end(), "   ⚙     📣    !", "nothing beside the buttons at rest: {row:?}");
        // Before the first probe Settings is just named.
        let row = buf_row(&footer_buf(&SidebarView::build(&empty_model()), Some(FooterButton::Settings), 60, 20), y);
        assert!(row.trim_end().ends_with("Settings"), "{row:?}");
    }

    /// At the default sidebar width the squares leave too little room beside
    /// them for some names; those float, whole, on the row above the squares'
    /// caps, over whatever sits there. A name that fits stays beside.
    #[test]
    fn a_name_too_long_for_beside_the_squares_floats_above_them() {
        let mut model = empty_model();
        model.unread_errors = 2;
        model.status_line = "3 open · 1 review".into();
        let view = SidebarView::build(&model);
        let (w, y) = (27, 20 - 3);
        let buf = footer_buf(&view, Some(FooterButton::Errors), w, 20);
        let above = buf_row(&buf, y - 2);
        assert_eq!(above.trim_end(), " 2 unread errors", "whole, at the inset, over the status: {above:?}");
        assert_eq!(buf[(1, y - 2)].fg, palette().gray);
        assert!(buf_row(&buf, y).trim_end().ends_with('!'), "nothing beside: {:?}", buf_row(&buf, y));
        assert_square(&buf, 13..18, y, palette().error_button_hover, "the caps stay whole");
        // At rest the status is back.
        let rest = footer_buf(&view, None, w, 20);
        assert!(buf_row(&rest, y - 2).contains("3 open"), "{:?}", buf_row(&rest, y - 2));

        // Two buttons leave room for "Settings" beside them.
        let view = SidebarView::build(&empty_model());
        let buf = footer_buf(&view, Some(FooterButton::Settings), w, 20);
        assert!(buf_row(&buf, y).trim_end().ends_with("📣   Settings"), "{:?}", buf_row(&buf, y));
        // A name longer than the whole row is cut short there.
        let mut model = empty_model();
        model.daemon_version_line = Some("daemon 0.11.0 · cli 0.11.0".into());
        let buf = footer_buf(&SidebarView::build(&model), Some(FooterButton::Settings), w, 20);
        let above = buf_row(&buf, y - 2);
        assert!(above.starts_with(" Settings · daemon") && above.trim_end().ends_with('…'), "{above:?}");
    }

    /// The unread-errors badge follows Send Feedback while there are unread
    /// errors (the desktop's order): the same square, red, with a bold `!`, a
    /// shade lighter under the pointer.
    #[test]
    fn unread_errors_badge_follows_feedback_only_with_unread_errors() {
        let model = empty_model();
        let buf = footer_buf(&SidebarView::build(&model), None, 40, 20);
        let y = 20 - 3;
        assert!(!buf_row(&buf, y).contains('!'), "no badge without unread errors");
        assert_eq!(buf[(13, y)].bg, Color::Reset, "no square past Feedback");

        let mut model = empty_model();
        model.unread_errors = 3;
        let view = SidebarView::build(&model);
        let buf = footer_buf(&view, None, 40, 20);
        assert_eq!(buf[(15, y)].symbol(), "!", "the badge sits after Feedback, centered");
        assert!(buf[(15, y)].modifier.contains(Modifier::BOLD));
        assert_eq!(buf[(15, y)].fg, palette().on_color);
        assert_square(&buf, 13..18, y, palette().error_button, "the badge");
        assert_eq!(buf[(18, y)].bg, Color::Reset);
        let hovered = footer_buf(&view, Some(FooterButton::Errors), 40, 20);
        assert_square(&hovered, 13..18, y, palette().error_button_hover, "the lit badge");
        assert_square(&hovered, 1..6, y, palette().selection, "Settings at rest");
        // A stale hover on a badge that's gone draws nothing.
        let buf = footer_buf(&SidebarView::build(&empty_model()), Some(FooterButton::Errors), 40, 20);
        assert!(!buf_row(&buf, y).contains("unread"), "no tooltip for a missing badge");
        assert_square(&buf, 1..6, y, palette().selection, "Settings stays at rest");
        assert_square(&buf, 7..10, y, palette().selection, "Feedback stays at rest");
    }

    /// Each button's target spans its square: its columns across the `▄`, the
    /// toolbar and the `▀` rows; the gaps, the rows past the square, and the
    /// band miss.
    #[test]
    fn footer_button_hits_cover_the_square() {
        let mut model = empty_model();
        model.unread_errors = 2;
        let view = SidebarView::build(&model);
        let area = Rect::new(0, 0, 40, 20);
        let y = 20 - 3;
        for row in [y - 1, y, y + 1] {
            for x in 1..6 {
                assert_eq!(view.footer_button_at(area, x, row), Some(FooterButton::Settings));
            }
            for x in 7..12 {
                assert_eq!(view.footer_button_at(area, x, row), Some(FooterButton::Feedback));
            }
            for x in 13..18 {
                assert_eq!(view.footer_button_at(area, x, row), Some(FooterButton::Errors));
            }
            for x in [0, 6, 12, 18, 30] {
                assert_eq!(view.footer_button_at(area, x, row), None, "col {x} row {row} misses");
            }
        }
        assert_eq!(view.footer_button_at(area, 3, y - 2), None, "above the square");
        assert_eq!(view.footer_button_at(area, 3, y + 2), None, "the zen band");
        // An offset sidebar (the shell's content rect) hits in its own frame.
        let shifted = Rect::new(0, 3, 40, 20);
        assert_eq!(view.footer_button_at(shifted, 3, y + 3), Some(FooterButton::Settings));
    }

    /// Without emoji (the Linux console, a non-UTF-8 locale) the buttons are
    /// words, each square widened to fit its word with a column either side,
    /// and hit-test over the squares.
    #[test]
    fn without_emoji_the_buttons_are_words() {
        let view = SidebarView::build(&empty_model()).with_emoji(false);
        let buf = footer_buf(&view, None, 40, 20);
        let y = 20 - 3;
        let row = buf_row(&buf, y);
        assert!(row.starts_with("  Settings   Feedback  "), "{row:?}");
        assert!(!row.contains('⚙') && !row.contains('📣'));
        assert_square(&buf, 1..11, y, palette().selection, "Settings");
        assert_square(&buf, 12..22, y, palette().selection, "Feedback");
        let area = Rect::new(0, 0, 40, 20);
        assert_eq!(view.footer_button_at(area, 10, y), Some(FooterButton::Settings));
        assert_eq!(view.footer_button_at(area, 11, y), None, "the gap");
        assert_eq!(view.footer_button_at(area, 12, y), Some(FooterButton::Feedback));
    }

    /// A sidebar too narrow for every square leaves off the ones that don't
    /// fit whole (never a clipped square), down to none at all.
    #[test]
    fn narrow_sidebars_drop_whole_squares() {
        let mut model = empty_model();
        model.unread_errors = 1;
        let view = SidebarView::build(&model);
        let y = 20 - 3;
        for (w, last) in [
            (19u16, Some(FooterButton::Errors)),
            (18, Some(FooterButton::Feedback)),
            (13, Some(FooterButton::Feedback)),
            (12, Some(FooterButton::Settings)),
            (7, Some(FooterButton::Settings)),
            (6, None),
        ] {
            let buf = footer_buf(&view, None, w, 20);
            let area = Rect::new(0, 0, w, 20);
            let shown: Vec<_> = (0..w).filter_map(|x| view.footer_button_at(area, x, y)).collect();
            assert_eq!(shown.last().copied(), last, "w={w}");
            // The last column is always blank: nothing reaches the edge.
            assert_eq!(buf[(w - 1, y)].bg, Color::Reset, "w={w}");
            assert!(buf[(w - 1, y - 1)].symbol().trim().is_empty(), "w={w}");
        }
    }

    /// The first-run hint, the version warning, the toolbar and the band keep
    /// their order at every height down to a sidebar too short for them,
    /// and nothing panics.
    #[test]
    fn footer_keeps_its_order_at_every_height() {
        let mut model = empty_model();
        model.status_line = FIRST_RUN_HINT.into();
        model.daemon_version_line = Some("daemon 0.11.0 ≠ cli 0.12.0 — shelbi daemon restart".into());
        model.daemon_version_mismatch = true;
        model.zen_mode = ZenModeState::On;
        model.unread_errors = 1;
        for h in 1..40u16 {
            let rows = render_rows(&model, 0, 30, h);
            let find = |needle: &str| rows.iter().position(|r| r.contains(needle));
            let (hint, version, toolbar, zen) =
                (find("Ctrl+P palette"), find("daemon 0.11"), find("⚙"), find("ZEN MODE ON"));
            if h >= 25 {
                for (name, at) in [("hint", hint), ("version", version), ("toolbar", toolbar), ("zen", zen)] {
                    assert!(at.is_some(), "h={h}: the {name} shows:\n{}", rows.join("\n"));
                }
            }
            if let Some(z) = zen {
                assert_eq!(z, rows.len() - 1, "h={h}: the band is the bottom row");
            }
            if let (Some(t), Some(z)) = (toolbar, zen) {
                assert_eq!(t + 2, z, "h={h}: one spacer between the toolbar and the band");
            }
            if let (Some(v), Some(t)) = (version, toolbar) {
                assert!(v < t, "h={h}: the version warning above the toolbar");
            }
            if let (Some(hi), Some(v)) = (hint, version) {
                assert!(hi < v, "h={h}: the hint above the version");
            }
        }
    }

    /// Every sidebar element (nav selection, workspaces in each badge state,
    /// reviews in each state, the search box, the Zen band, the unread-errors
    /// button, the status line and the version warning) is readable on a light
    /// terminal.
    #[test]
    fn renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        let mut model = empty_model();
        model.workspaces = vec![
            ws("alpha", "hub", Some("t-1"), Some("developer"), WorkspaceBadge::Working),
            ws("bravo", "hub", Some("t-2"), Some("qa"), WorkspaceBadge::AwaitingInput),
            ws("charlie", "hub", Some("t-3"), Some("developer"), WorkspaceBadge::AwaitingPermission),
            ws("delta", "devbox", Some("t-4"), Some("developer"), WorkspaceBadge::Paused),
            ws("echo", "devbox", None, None, WorkspaceBadge::Idle),
        ];
        model.reviews = vec![
            review("palette", "Palette fuzzy-match fix", "shelbi/pf", Some("hub:3000"), ReviewState::Serving),
            review("nav", "Homepage nav fix", "shelbi/nav", None, ReviewState::Loading),
            review("onboarding", "Rework onboarding", "shelbi/ob", None, ReviewState::Pending),
        ];
        model.zen_mode = ZenModeState::On;
        model.unread_errors = 2;
        model.status_line = "3 open · 1 review".into();
        model.daemon_version_line = Some("daemon 0.10.0 ≠ cli 0.11.0 — shelbi daemon restart".into());
        model.daemon_version_mismatch = true;
        let view = SidebarView::build(&model);
        crate::theme::with_appearance(Appearance::Light, || {
            let mut term = Terminal::new(TestBackend::new(40, 48)).unwrap();
            term.draw(|f| {
                let area = f.area();
                view.render(f.buffer_mut(), area, 1, true, &default_chrome())
            })
            .unwrap();
            crate::theme::assert_light_readable("shell-sidebar", term.backend().buffer());
            for hover in [FooterButton::Settings, FooterButton::Feedback, FooterButton::Errors] {
                let buf = footer_buf(&view, Some(hover), 40, 48);
                crate::theme::assert_light_readable("shell-sidebar-footer-hover", &buf);
            }
        });
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
        assert_eq!(v.target_at(2), Some(RowTarget::Native(View::Changelog)));
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
        let chat_y = row_y(&rows, "Orchestrator") as u16;
        assert_eq!(v.hit(area, 2, chat_y), Some(0), "Chat → ordinal 0");
        assert_eq!(v.hit(area, 2, chat_y - 1), None, "the separator above Chat is inert");
        let changelog_y = row_y(&rows, "Changelog") as u16;
        assert_eq!(v.hit(area, 2, changelog_y), Some(2), "Changelog → ordinal 2");

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



