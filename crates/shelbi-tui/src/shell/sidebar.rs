//! The shell's sidebar: a renderer over shelbi-app's [`SidebarModel`], brought
//! to visual parity with the former tmux-runtime sidebar (`crate::sidebar` +
//! the old `crate::app` renderer it was ported from).
//!
//! The model logic lives in [`shelbi_app::view::SidebarModel`] (built off the
//! UI thread by [`super::sidebar_model`]); this module only lays those rows out
//! and maps a selection / click to what the main area should show. Laid out
//! top to bottom to match John's Figma "Navigation" design: a header (project
//! title + a square Search button that opens the command palette), a full-width
//! nav block with the half-block selection bleed, the machine-grouped
//! workspace pool with per-state badges, the two review sections rendered as
//! two-line entries (dim `⎇ branch` line, truncated with `…`), and the footer
//! (the one-time first-run hint or a status line; a red daemon/CLI version
//! warning, naming the fix, only while the two disagree; the toolbar,
//! mirroring the desktop's: Settings, Send Feedback, and the unread-errors
//! badge while there are any, in its bottom-left corner, on the bottom row;
//! and, only while Zen is on, the `ZEN MODE ON` band under it, which lifts the
//! toolbar one row while it shows). Matching versions don't show in the
//! sidebar, and the board's freshness shows on the Issues board. The old `^P
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

    /// The text shown beside the toolbar while the pointer is over the
    /// button: only the unread-errors badge has any (its count). Settings and
    /// Send Feedback just light their hover fill.
    fn tooltip(self, unread_errors: usize) -> Option<String> {
        match self {
            FooterButton::Settings | FooterButton::Feedback => None,
            FooterButton::Errors => Some(format!(
                "{unread_errors} unread error{}",
                if unread_errors == 1 { "" } else { "s" }
            )),
        }
    }
}

/// Per-frame footer/header input that isn't board state: the resolved
/// palette-open chord the Search button's tooltip names, and which button the
/// pointer is over. Owned (no borrow of the shell state) so the draw closure
/// can disjointly borrow the rest of the state.
pub struct SidebarChrome {
    /// The palette-open chord resolved for the host platform (e.g. `⌃P` on
    /// macOS, `Ctrl+P` elsewhere), or `<unbound>` when no chord is bound.
    palette: String,
    /// The footer button under the pointer, lit with its hover fill (the
    /// unread-errors badge also shows its count beside the toolbar).
    hover: Option<FooterButton>,
    /// The pointer is over the header's Search button: its square lifts and
    /// its tooltip shows.
    search_hover: bool,
    /// The pointer is over the header's project name: its cells lift to the
    /// buttons' hover fill (a click there opens the palette too).
    name_hover: bool,
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
            search_hover: false,
            name_hover: false,
        }
    }

    /// The footer button the pointer is over (see [`SidebarView::footer_button_at`]).
    pub fn with_hover(self, hover: Option<FooterButton>) -> Self {
        SidebarChrome { hover, ..self }
    }

    /// Whether the pointer is over the header's Search button (see
    /// [`SidebarView::search_button_hit`]).
    pub fn with_search_hover(self, search_hover: bool) -> Self {
        SidebarChrome { search_hover, ..self }
    }

    /// Whether the pointer is over the header's project name (see
    /// [`SidebarView::project_name_hit`]).
    pub fn with_name_hover(self, name_hover: bool) -> Self {
        SidebarChrome { name_hover, ..self }
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
    /// the toolbar's three rows (its half-row margin above, the toolbar, its
    /// half-row margin below), and, only while Zen is on, the zen band under
    /// them. Off or paused, no row is held for the band: the toolbar's margin
    /// is the sidebar's bottom row, and it drops back there when Zen goes off.
    fn footer_height(&self) -> u16 {
        self.status_row_height() + self.warning_lines().len() as u16 + 3 + u16::from(self.zen_on())
    }

    /// Whether the `ZEN MODE ON` band shows (and takes the bottom row).
    fn zen_on(&self) -> bool {
        matches!(self.zen_mode, ZenModeState::On)
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

    /// Where the hovered button's `width`-column tooltip goes, its row with a
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

    /// The header's Search button glyph (`🔍`, two cells wide), or the word
    /// when emoji can't draw.
    fn search_face(&self) -> &'static str {
        if self.emoji {
            "🔍"
        } else {
            "Search"
        }
    }

    /// The header's Search button: a square like the footer's (`▄` cap,
    /// glyph row, `▀` cap) across header rows 0–2, so its glyph shares the
    /// title's row, its right edge [`HEADER_INSET`] columns in from the
    /// sidebar's right as the title is from its left. `None` on a header too
    /// short for all three rows, or a sidebar too narrow to leave the title a
    /// column beside it.
    fn search_button_rect(&self, header: Rect) -> Option<Rect> {
        let width = panel::square_button_width(self.search_face()) as u16;
        let x = header.right().checked_sub(HEADER_INSET + width)?;
        (header.height >= 3 && x > header.x + HEADER_INSET).then_some(Rect {
            x,
            y: header.y,
            width,
            height: 3,
        })
    }

    /// The row the header's title is laid out in: row 1 (a blank row above
    /// it), or row 0 on a header clamped to a single row, from
    /// [`HEADER_INSET`] columns in to a column short of the Search button
    /// (or [`HEADER_INSET`] columns short of the sidebar's right without
    /// one).
    fn title_rect(header: Rect, search: Option<Rect>) -> Rect {
        let title_y = header.y + if header.height >= 2 { 1 } else { 0 };
        let title_x = (header.x + HEADER_INSET).min(header.right());
        let title_end = search.map_or(header.right().saturating_sub(HEADER_INSET), |b| b.x - 1);
        Rect {
            x: title_x,
            y: title_y,
            width: title_end.saturating_sub(title_x),
            height: 1,
        }
    }

    /// The project name as drawn in `title`: the label, cut with `…` to the
    /// title's width.
    fn project_name_text(&self, title: Rect) -> String {
        truncate_ellipsis(&self.project_label, title.width as usize)
    }

    /// The cells the project name is drawn in: the title row's leading
    /// columns, as many as the (possibly `…`-cut) name covers. `None` on an
    /// empty header or name.
    fn project_name_rect(&self, header: Rect, search: Option<Rect>) -> Option<Rect> {
        if header.width == 0 || header.height == 0 {
            return None;
        }
        let title = Self::title_rect(header, search);
        let drawn = UnicodeWidthStr::width(self.project_name_text(title).as_str()) as u16;
        let width = drawn.min(title.width);
        (width > 0).then_some(Rect { width, ..title })
    }

    /// Split the sidebar `area` into (header, search, nav, rest-list, footer)
    /// rects — the shared geometry the renderer and the click map both derive
    /// from. `search` is the Search button's square, `None` when the header
    /// can't hold it (see [`Self::search_button_rect`]);
    /// `rest` is `None` when the body is too short to hold anything past the nav
    /// block.
    fn geometry(&self, area: Rect) -> SidebarGeometry {
        let outer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(self.footer_height())])
            .split(area);
        let body_region = outer[0];
        let footer = self.footer_rows(outer[1]);
        let header_h = HEADER_H.min(body_region.height);
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(header_h), Constraint::Min(0)])
            .split(body_region);
        let header = split[0];
        let body = split[1];
        let search = self.search_button_rect(header);
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
    /// by a short sidebar keeps the band (while Zen is on) and the toolbar and
    /// drops what sits above them first.
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
        let zen = if self.zen_on() { take(&mut left, 1) } else { None };
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

    /// Whether `(x, y)` lands on the header's Search button (any of the three
    /// cell rows its square spans) — the shell maps a click here to opening
    /// the command palette, the same as pressing the palette chord.
    pub fn search_button_hit(&self, area: Rect, x: u16, y: u16) -> bool {
        if area.width == 0 || area.height == 0 || !contains(area, x, y) {
            return false;
        }
        self.geometry(area)
            .search
            .is_some_and(|s| contains(s, x, y))
    }

    /// Whether `(x, y)` lands on the header's project name: exactly the cells
    /// its text is drawn in. The shell maps a click here to opening the
    /// command palette, as it does a click on the Search button.
    pub fn project_name_hit(&self, area: Rect, x: u16, y: u16) -> bool {
        if area.width == 0 || area.height == 0 || !contains(area, x, y) {
            return false;
        }
        let geo = self.geometry(area);
        self.project_name_rect(geo.header, geo.search)
            .is_some_and(|r| contains(r, x, y))
    }

    /// The footer button at `(x, y)`, if any: its columns across the three
    /// cell rows its square spans (the `▄` margin, the toolbar row, the `▀`
    /// margin), like the Search button's target. The shell opens Settings, runs
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
        // Figma carry a fill — the buttons, the selection blocks, and the zen
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

    /// Render the header block (Figma `Project`): the project title in bold
    /// accent cyan on row 1, [`HEADER_INSET`] columns in so it lines up with
    /// the nav glyphs, and the Search button's square at the row's right end,
    /// its glyph on the title's row. Under the pointer the square lifts, as
    /// the footer's do, and the row below it names the button and its chord
    /// (the desktop's `Search (⌃P)` tooltip), right-aligned under the square.
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
        // a header clamped to a single row. It stops a column short of the
        // Search button.
        let title_rect = Self::title_rect(header, search);
        Paragraph::new(Line::from(Span::styled(
            self.project_name_text(title_rect),
            Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
        )))
        .render(title_rect, buf);
        // Under the pointer the name's cells lift to the buttons' hover fill:
        // a click there opens the palette, as the Search button does.
        if chrome.name_hover {
            if let Some(name) = self.project_name_rect(header, search) {
                buf.set_style(name, Style::default().bg(palette().button_hover));
            }
        }

        let Some(button) = search else {
            return;
        };
        let fill = if chrome.search_hover {
            palette().button_hover
        } else {
            palette().search
        };
        panel::draw_square_button(
            buf,
            button.x,
            [Some(button.y), Some(button.y + 1), Some(button.y + 2)],
            button.width,
            self.search_face(),
            fill,
            Style::default().fg(palette().gray),
        );
        // The tooltip row: the header's last row, when it has one past the
        // square.
        let tip_y = button.y + 3;
        if chrome.search_hover && tip_y < header.bottom() {
            let tip = search_tooltip(&chrome.palette);
            let room = button.right().saturating_sub(header.x + LIST_INDENT.horizontal) as usize;
            let tip = truncate_ellipsis(&tip, room);
            let w = UnicodeWidthStr::width(tip.as_str()) as u16;
            buf.set_stringn(
                button.right() - w,
                tip_y,
                &tip,
                w as usize,
                Style::default().fg(palette().gray),
            );
        }
    }

    /// Render the leading `Row::Nav` rows as an inset block: a separator line
    /// between (and bracketing) each item, the selected item's fill spanning the
    /// inset width (one column in from each edge), its
    /// adjacent separators carrying the half-block bleed. The label keeps a
    /// further 1-col inner pad inside that inset.
    fn render_nav(&self, buf: &mut Buffer, area: Rect, sel_row: Option<usize>, focused: bool) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // The nav item bounds — fill, half-block bleed, and label — are inset one
        // column from each side, as the rest of the list's rows are, so the
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
    /// square lifts, and for the unread-errors badge its count shows beside
    /// the toolbar, or, when the squares leave too little room for the whole
    /// count, floats on the row above them (over whatever sits there, as the
    /// desktop's tooltip does).
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
            // Right of the buttons: the hovered badge's count (the desktop's
            // tooltip), left-aligned. Nothing sits there otherwise.
            if let Some(name) = hover.and_then(|button| button.tooltip(self.unread_errors)) {
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

    /// Zen row: a full-width green band carrying a bold `ZEN MODE ON`. The
    /// geometry only gives it a row while Zen is on; off or paused there is no
    /// row, and the toolbar sits on the sidebar's bottom edge instead.
    fn render_zen_row(&self, buf: &mut Buffer, area: Rect) {
        if area.width == 0 || area.height == 0 {
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
    /// The toolbar's half-row margin below (`▀`): the spacer above the band
    /// while Zen is on, else the sidebar's bottom row.
    below: Option<Rect>,
    /// The `ZEN MODE ON` band, only while Zen is on.
    zen: Option<Rect>,
}

/// Columns between two toolbar buttons (the desktop's 4pt gap, rounded up so
/// two squares never touch).
const BUTTON_GAP: u16 = 1;

/// Height of the header block (Figma `Project`, 88pt = four 22pt rows): the
/// Search button's square across rows 0–2 (`▄` cap, glyph, `▀` cap), the
/// project title on row 1 beside its glyph, and a blank row 3 (where the
/// button's tooltip shows under the pointer). The nav block's own leading
/// separator sits directly below that, so the nav labels land on rows 5, 7
/// and 9, as in the Figma.
const HEADER_H: u16 = 4;

/// Columns between the sidebar's edges and the header's contents (Figma's
/// 19.2pt): the title on the left, lined up with the nav glyphs, and the
/// Search button on the right.
const HEADER_INSET: u16 = 2;

/// 1-col horizontal padding shared by the nav block, the footer's toolbar,
/// and the rest-of-list rows. The nav section's full-width fill
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

/// The Search button's tooltip: its name and the palette chord it shares
/// (`Search (⌃P)`), just the name while the palette is unbound.
fn search_tooltip(chord: &str) -> String {
    if chord.is_empty() || chord == crate::keymap::UNBOUND {
        "Search".to_string()
    } else {
        format!("Search ({chord})")
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
    /// U+2580 below) are inset one column from each edge, with the gutter
    /// columns left on the default (transparent)
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

    /// The Search button's columns on a `w`-wide sidebar: a square five
    /// columns wide (the footer's), its right edge two columns in.
    fn search_cols(w: u16) -> std::ops::Range<u16> {
        w - 7..w - 2
    }

    fn header_buf(model: &SidebarModel, chrome: &SidebarChrome, w: u16, h: u16) -> Buffer {
        let view = SidebarView::build(model);
        let mut buf = Buffer::empty(Rect::new(0, 0, w, h));
        let area = buf.area;
        view.render(&mut buf, area, 1, true, chrome); // Issues selected
        buf
    }

    /// The header is Figma's `Project` frame: four rows, the bold cyan title
    /// two columns in on row 1, and the Search button's square (`▄` / `🔍`
    /// on the search fill / `▀`) two columns in from the right across rows
    /// 0–2, its glyph on the title's row. Nothing else: no `Search` label and
    /// no chord.
    #[test]
    fn header_matches_the_figma_project_frame() {
        let mut model = empty_model();
        model.project_label = "My project".into();
        let buf = header_buf(&model, &default_chrome(), 40, 20);
        let rows: Vec<String> = (0..20).map(|y| buf_row(&buf, y)).collect();
        let joined = rows.join("\n");

        assert_eq!(buf[(1, 0)].bg, Color::Reset, "the sidebar paints no background (transparent)");
        assert_eq!(row_y(&rows, "My project"), 1, "the title is on row 1:\n{joined}");
        assert_eq!(rows[1].find('M'), Some(2), "the title is two columns in, over the nav glyphs:\n{joined}");
        for x in 2..12u16 {
            assert_eq!(buf[(x, 1)].fg, palette().cyan, "the title is the accent cyan, col {x}");
            assert!(buf[(x, 1)].modifier.contains(Modifier::BOLD), "the title is bold, col {x}");
        }

        let cols = search_cols(40);
        assert_square(&buf, cols.clone(), 1, palette().search, "the Search button");
        assert_eq!(buf[(cols.start + 2, 1)].symbol(), "🔍", "the magnifier is centered in the square");
        assert_eq!(buf[(cols.start + 2, 1)].fg, palette().gray, "the magnifier is #bababa");
        for y in 0..3u16 {
            for x in (cols.start - 1..cols.start).chain(cols.end..40) {
                assert_eq!(buf[(x, y)].bg, Color::Reset, "outside the square stays clear, ({x}, {y})");
                assert!(!["▄", "▀"].contains(&buf[(x, y)].symbol()), "no cap outside the square, ({x}, {y})");
            }
        }
        assert!(rows[0].trim_start_matches(['▄', ' ']).is_empty(), "row 0 holds only the cap:\n{joined}");
        assert!(rows[3].trim().is_empty(), "row 3 is the header's blank last row:\n{joined}");
        assert!(!joined.contains("Search"), "the button carries no label:\n{joined}");
        assert!(!joined.contains("⌃P") && !joined.contains("Ctrl+P"), "no chord at rest:\n{joined}");
    }

    /// The nav sits directly below the four-row header, its labels on rows 5,
    /// 7 and 9 as in the Figma, each two columns of glyph after a column of
    /// inset, gray at rest.
    #[test]
    fn nav_labels_land_on_the_figma_rows() {
        let rows = render_rows(&empty_model(), 99, 40, 20);
        let joined = rows.join("\n");
        for (label, y) in [("Orchestrator", 5), ("Issues", 7), ("Changelog", 9)] {
            assert_eq!(row_y(&rows, label), y, "{label} on row {y}:\n{joined}");
        }
        let buf = header_buf(&empty_model(), &default_chrome(), 40, 20);
        assert_eq!(buf[(2, 5)].symbol(), "💬", "the glyph is two columns in");
        assert_eq!(buf[(5, 5)].symbol(), "O", "the label follows a column after the glyph");
        assert_eq!(buf[(5, 9)].fg, palette().gray, "an unselected nav label is gray");
    }

    /// A click anywhere on the square, caps included, maps to the Search
    /// button (opens the palette); the title, the blank row under it, the
    /// columns beside it, and the nav below do not.
    #[test]
    fn search_button_hit_covers_its_square() {
        let view = SidebarView::build(&empty_model());
        let area = Rect { x: 0, y: 0, width: 40, height: 20 };
        let cols = search_cols(40);
        for y in 0..3u16 {
            for x in cols.clone() {
                assert!(view.search_button_hit(area, x, y), "({x}, {y}) is on the square");
            }
            assert!(!view.search_button_hit(area, cols.start - 1, y), "the column before the square, row {y}");
            assert!(!view.search_button_hit(area, cols.end, y), "the column after the square, row {y}");
        }
        assert!(!view.search_button_hit(area, 3, 1), "the title is not the button");
        for x in cols {
            assert!(!view.search_button_hit(area, x, 3), "the row under the square, col {x}");
        }
        assert_eq!(view.hit(area, 6, 5), Some(0), "the first nav item is on row 5 and selects index 0");
        assert_eq!(view.hit(area, 6, 4), None, "the nav's leading separator is not selectable");
    }

    /// Under the pointer the square lifts to `button_hover` and the row under
    /// it names the button and its chord, right-aligned to the square (the
    /// desktop's `Search (⌃P)` tooltip); with the palette unbound it's just
    /// the name.
    #[test]
    fn hovered_search_button_lifts_and_names_its_chord() {
        let want_chord = match DisplayStyle::detect() {
            DisplayStyle::Mac => "⌃P",
            DisplayStyle::Linux => "Ctrl+P",
        };
        let cols = search_cols(40);
        let buf = header_buf(&empty_model(), &default_chrome().with_search_hover(true), 40, 20);
        assert_square(&buf, cols.clone(), 1, palette().button_hover, "the lit Search button");
        assert_ne!(palette().button_hover, palette().search, "the lift shows");
        let tip = format!("Search ({want_chord})");
        assert_eq!(buf_row(&buf, 3).trim(), tip, "the tooltip on the row under the square");
        let last = (0..40u16).rev().find(|&x| !buf[(x, 3)].symbol().trim().is_empty()).unwrap();
        assert_eq!(last, cols.end - 1, "the tooltip ends under the square's right edge");
        assert_eq!(buf[(last, 3)].fg, palette().gray, "the tooltip is gray");

        let unbound = header_buf(&empty_model(), &chrome(None).with_search_hover(true), 40, 20);
        assert_eq!(buf_row(&unbound, 3).trim(), "Search", "unbound: just the name");
    }

    /// A long project name ends in `…` a column short of the square at every
    /// width the sidebar allows; a terminal without emoji gets the word.
    #[test]
    fn long_titles_stop_short_of_the_search_button() {
        let mut model = empty_model();
        model.project_label = "an-extremely-long-project-name-that-never-fits-even-a-wide-sidebar".into();
        for w in [24u16, 39, 60] {
            let buf = header_buf(&model, &default_chrome(), w, 20);
            let cols = search_cols(w);
            let row = buf_row(&buf, 1);
            assert!(row.contains('…'), "{w} cols: the title is cut short: {row:?}");
            assert_eq!(buf[(cols.start - 1, 1)].symbol(), " ", "{w} cols: a column between the title and the square");
            assert_eq!(buf[(cols.start - 2, 1)].symbol(), "…", "{w} cols: the title runs up to that column");
            assert_square(&buf, cols, 1, palette().search, &format!("{w} cols"));
        }
        let view = SidebarView::build(&model).with_emoji(false);
        let mut buf = Buffer::empty(Rect::new(0, 0, 40, 20));
        let area = buf.area;
        view.render(&mut buf, area, 1, true, &default_chrome());
        assert!(buf_row(&buf, 1).contains(" Search "), "the word in place of the emoji: {:?}", buf_row(&buf, 1));
        assert!(view.search_button_hit(area, 30, 1), "the wider square is still the button");
    }

    /// The project name's target is exactly the cells its text is drawn in
    /// (a short name and one cut with `…`), at narrow and wide sidebars and
    /// one too narrow for the Search button: nothing beside it or above or
    /// below it, never the Search button's cells, and never a row.
    #[test]
    fn project_name_hit_covers_exactly_the_drawn_name() {
        for label in ["demo", "an-extremely-long-project-name-that-never-fits-even-a-wide-sidebar"] {
            let mut model = empty_model();
            model.project_label = label.into();
            for w in [6u16, 24, 28, 39, 60] {
                let buf = header_buf(&model, &default_chrome(), w, 20);
                let area = buf.area;
                let view = SidebarView::build(&model);
                // The name's cells: the bold run from column 2 on row 1.
                let drawn: Vec<u16> = (0..w)
                    .filter(|&x| buf[(x, 1)].modifier.contains(Modifier::BOLD))
                    .collect();
                assert_eq!(drawn.first(), Some(&2), "{label:?} at {w} cols starts two columns in");
                let end = drawn.last().unwrap() + 1;
                assert_eq!(drawn, (2..end).collect::<Vec<_>>(), "{label:?} at {w} cols is one run");
                if label == "demo" && w >= 24 {
                    assert_eq!(end, 6, "the short name at {w} cols");
                } else {
                    assert_eq!(buf[(end - 1, 1)].symbol(), "…", "the cut name at {w} cols");
                }
                for y in 0..area.height {
                    for x in 0..w {
                        let on_name = y == 1 && (2..end).contains(&x);
                        assert_eq!(
                            view.project_name_hit(area, x, y),
                            on_name,
                            "{label:?} at {w} cols: ({x}, {y})"
                        );
                        if on_name {
                            assert!(!view.search_button_hit(area, x, y), "({x}, {y}) is not the Search button");
                            assert_eq!(view.hit(area, x, y), None, "({x}, {y}) is not a row");
                        }
                    }
                }
                // Off the sidebar entirely.
                assert!(!view.project_name_hit(area, 2, 21), "below the area");
            }
        }
        // Offset the sidebar: the target moves with it.
        let view = SidebarView::build(&empty_model());
        let area = Rect::new(10, 3, 30, 20);
        assert!(view.project_name_hit(area, 12, 4));
        assert!(!view.project_name_hit(area, 2, 1), "the unshifted cell misses");
    }

    /// Under the pointer the name's cells lift to the buttons' hover fill,
    /// and only those: the column after it, the Search button and the rest
    /// of the row keep theirs.
    #[test]
    fn hovered_project_name_lifts() {
        let rest = header_buf(&empty_model(), &default_chrome(), 40, 20);
        let lit = header_buf(&empty_model(), &default_chrome().with_name_hover(true), 40, 20);
        for x in 0..40u16 {
            for y in 0..20u16 {
                if y == 1 && (2..6).contains(&x) {
                    assert_eq!(lit[(x, y)].bg, palette().button_hover, "the name is lit at ({x}, {y})");
                    assert_eq!(lit[(x, y)].symbol(), rest[(x, y)].symbol());
                    assert_eq!(lit[(x, y)].fg, palette().cyan, "the name keeps its cyan");
                } else {
                    assert_eq!(lit[(x, y)], rest[(x, y)], "unchanged at ({x}, {y})");
                }
            }
        }
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
    /// never show in the footer, at any width: the footer is the bare toolbar,
    /// the rows the line used to take given back to the list.
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
                assert_eq!(view.footer_height(), 3, "the toolbar and its margins only");
                assert_eq!(row_y(&rows, "⚙"), TOOLBAR_Y as usize, "the toolbar keeps its row at {w} cols");
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
        assert_eq!(view.footer_height(), 5, "two warning rows over the toolbar");
        let buf = footer_buf(&view, None, 36, 20);
        let toolbar_y = TOOLBAR_Y;
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
        assert_eq!(SidebarView::build(&model).footer_height(), 3);
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

    /// Off or paused, no row is held for the band: the toolbar's `▀` caps are
    /// the sidebar's bottom row, with nothing blank beneath them. On, the band
    /// takes the bottom row and the toolbar sits one row up, right above it.
    #[test]
    fn toolbar_takes_the_bottom_row_unless_zen_is_on() {
        for zen in [ZenModeState::Off, ZenModeState::Paused] {
            let mut model = empty_model();
            model.zen_mode = zen;
            let view = SidebarView::build(&model);
            assert_eq!(view.footer_height(), 3, "{zen:?}: no row for the band");
            let rows = render_rows_with(&model, 0, true, &default_chrome(), 24, 16);
            let joined = rows.join("\n");
            assert_eq!(row_y(&rows, "⚙"), rows.len() - 2, "{zen:?}: the toolbar is on the bottom:\n{joined}");
            let last = rows[rows.len() - 1].trim_end();
            assert_eq!(last, " ▀▀▀▀▀ ▀▀▀▀▀", "{zen:?}: the caps are the last row:\n{joined}");
        }
        let mut model = empty_model();
        model.zen_mode = ZenModeState::On;
        assert_eq!(SidebarView::build(&model).footer_height(), 4, "On: the band's row too");
        let rows = render_rows_with(&model, 0, true, &default_chrome(), 24, 16);
        assert_eq!(row_y(&rows, "⚙"), rows.len() - 3);
        assert_eq!(rows[rows.len() - 2].trim_end(), " ▀▀▀▀▀ ▀▀▀▀▀");
        assert_eq!(row_y(&rows, "ZEN MODE ON"), rows.len() - 1);
    }

    /// Toggling Zen moves the toolbar, and the version warning above it, by
    /// the band's one row: down when it goes off, up when it comes on.
    #[test]
    fn footer_shifts_one_row_across_zen_toggle() {
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
                row_y(&on_rows, text) + 1,
                "{text:?} drops one row when Zen goes off"
            );
        }
    }

    // --- footer toolbar -------------------------------------------------------

    /// The toolbar's row in a 20-row sidebar with Zen off: the row above its
    /// `▀` caps, which are the bottom row.
    const TOOLBAR_Y: u16 = 20 - 2;

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
    /// desktop's order, their `▀` caps on the bottom row (Zen off); the title
    /// row has no gear any more. Each is a square button at rest, the back
    /// button's: [`panel::BACK_BTN_WIDTH`] wide on the selection fill, its
    /// glyph gray and centered (the double-width `📣` too).
    #[test]
    fn footer_toolbar_shows_settings_then_feedback_bottom_left() {
        let mut model = empty_model();
        model.project_label = "My project".into();
        let view = SidebarView::build(&model);
        let buf = footer_buf(&view, None, 40, 20);
        let toolbar_y = TOOLBAR_Y;
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
        let (bx, fx, fy) = (panel::PAD, 1u16, TOOLBAR_Y);
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

    /// Under the pointer a button's square lifts to the lighter
    /// `button_hover` across its own columns only; its glyph stays
    /// gray. The other button stays at rest.
    #[test]
    fn hovered_button_lifts_its_square() {
        let view = SidebarView::build(&empty_model());
        let y = TOOLBAR_Y;
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

    /// Hovering Settings or Send Feedback only lights the square: no name and
    /// no daemon/CLI versions, beside the toolbar or floating above it. The
    /// unread-errors badge still shows its count beside the toolbar.
    #[test]
    fn hover_shows_text_only_for_the_unread_errors_badge() {
        let mut model = empty_model();
        model.daemon_version_line = Some("daemon 0.11.0 · cli 0.11.0".into());
        model.unread_errors = 1;
        let view = SidebarView::build(&model);
        let y = TOOLBAR_Y;
        let rest = footer_buf(&view, None, 60, 20);
        assert_eq!(buf_row(&rest, y).trim_end(), "   ⚙     📣    !", "nothing beside the buttons at rest");
        for w in [27, 60] {
            let at_rest = footer_buf(&view, None, w, 20);
            for hover in [FooterButton::Settings, FooterButton::Feedback] {
                let buf = footer_buf(&view, Some(hover), w, 20);
                let joined: String = (0..20).map(|y| buf_row(&buf, y)).collect::<Vec<_>>().join("\n");
                for text in ["Settings", "Feedback", "daemon", "cli", "0.11.0"] {
                    assert!(!joined.contains(text), "{hover:?} at {w} cols shows {text:?}:\n{joined}");
                }
                for row in 0..20 {
                    assert_eq!(buf_row(&buf, row), buf_row(&at_rest, row), "{hover:?} at {w} cols, row {row}: the text is the rest's");
                }
            }
        }
        let buf = footer_buf(&view, Some(FooterButton::Errors), 60, 20);
        let row = buf_row(&buf, y);
        assert_eq!(row.trim_end(), "   ⚙     📣    !    1 unread error", "two columns past the squares");
        assert!(buf_row(&buf, y - 2).trim().is_empty(), "nothing floats when it fits beside");
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
        let (w, y) = (27, TOOLBAR_Y);
        let buf = footer_buf(&view, Some(FooterButton::Errors), w, 20);
        let above = buf_row(&buf, y - 2);
        assert_eq!(above.trim_end(), " 2 unread errors", "whole, at the inset, over the status: {above:?}");
        assert_eq!(buf[(1, y - 2)].fg, palette().gray);
        assert!(buf_row(&buf, y).trim_end().ends_with('!'), "nothing beside: {:?}", buf_row(&buf, y));
        assert_square(&buf, 13..18, y, palette().error_button_hover, "the caps stay whole");
        // At rest the status is back.
        let rest = footer_buf(&view, None, w, 20);
        assert!(buf_row(&rest, y - 2).contains("3 open"), "{:?}", buf_row(&rest, y - 2));

        // A count longer than the whole row is cut short there.
        let mut model = empty_model();
        model.unread_errors = 1000;
        let buf = footer_buf(&SidebarView::build(&model), Some(FooterButton::Errors), 19, 20);
        assert_eq!(buf_row(&buf, y - 2).trim_end(), " 1000 unread erro…");
    }

    /// The unread-errors badge follows Send Feedback while there are unread
    /// errors (the desktop's order): the same square, red, with a bold `!`, a
    /// shade lighter under the pointer.
    #[test]
    fn unread_errors_badge_follows_feedback_only_with_unread_errors() {
        let model = empty_model();
        let buf = footer_buf(&SidebarView::build(&model), None, 40, 20);
        let y = TOOLBAR_Y;
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
    /// toolbar and the `▀` rows, wherever Zen puts the toolbar (the bottom
    /// row off, one row up on); the gaps, the rows past the square, and the
    /// band miss.
    #[test]
    fn footer_button_hits_cover_the_square() {
        let mut model = empty_model();
        model.unread_errors = 2;
        let area = Rect::new(0, 0, 40, 20);
        for (zen, y) in [
            (ZenModeState::Off, TOOLBAR_Y),
            (ZenModeState::Paused, TOOLBAR_Y),
            (ZenModeState::On, TOOLBAR_Y - 1),
        ] {
            model.zen_mode = zen;
            let view = SidebarView::build(&model);
            for row in [y - 1, y, y + 1] {
                for x in 1..6 {
                    assert_eq!(view.footer_button_at(area, x, row), Some(FooterButton::Settings), "{zen:?}");
                }
                for x in 7..12 {
                    assert_eq!(view.footer_button_at(area, x, row), Some(FooterButton::Feedback), "{zen:?}");
                }
                for x in 13..18 {
                    assert_eq!(view.footer_button_at(area, x, row), Some(FooterButton::Errors), "{zen:?}");
                }
                for x in [0, 6, 12, 18, 30] {
                    assert_eq!(view.footer_button_at(area, x, row), None, "{zen:?}: col {x} row {row} misses");
                }
            }
            assert_eq!(view.footer_button_at(area, 3, y - 2), None, "{zen:?}: above the square");
            // An offset sidebar (the shell's content rect) hits in its own frame.
            let shifted = Rect::new(0, 3, 40, 20);
            assert_eq!(view.footer_button_at(shifted, 3, y + 3), Some(FooterButton::Settings), "{zen:?}");
        }
        assert_eq!(SidebarView::build(&model).footer_button_at(area, 3, 19), None, "the zen band");
    }

    /// Without emoji (the Linux console, a non-UTF-8 locale) the buttons are
    /// words, each square widened to fit its word with a column either side,
    /// and hit-test over the squares.
    #[test]
    fn without_emoji_the_buttons_are_words() {
        let view = SidebarView::build(&empty_model()).with_emoji(false);
        let buf = footer_buf(&view, None, 40, 20);
        let y = TOOLBAR_Y;
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
        let y = TOOLBAR_Y;
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
    /// reviews in each state, the Search button at rest and lit, the Zen band, the unread-errors
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
            let hovered = header_buf(&model, &default_chrome().with_search_hover(true), 40, 48);
            crate::theme::assert_light_readable("shell-sidebar-search-hover", &hovered);
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



