//! The **review panel** of the review interface — a dedicated ratatui view
//! hosted in the **left** column of the review window, with the review content
//! (agent / server / editor) filling the column on its right. It renders, for
//! the Ready-for-review task it was launched on:
//!
//! - a **square back button** at the very top (a back-arrow glyph, no label)
//!   that switches focus back to the dashboard window without tearing the
//!   still-loaded review down — see [`PanelEffect::FocusDashboard`]. It reads
//!   as a square via the same half-block bleed trick the sidebar nav uses for
//!   its selection (a lower-half-block row above, an upper-half-block below),
//! - a **header** showing review status + the review worktree's folder name
//!   (left-truncated to fit; click to reveal it in the OS file manager),
//! - a **view-switcher** action group — *Chat with Reviewer* (default) /
//!   *View Diff* / *Edit in <editor>* / *Open Browser* (the last only when the
//!   workflow declares a review URL), the active content view highlighted,
//!   *View Diff* opening the OS-configured diff tool over the review branch's
//!   changes in the right-column content pane, and
//! - an **Approve** / **Reject** action group, Reject opening a
//!   type-the-reason popover (a centered `tmux display-popup` with a bordered
//!   textbox and [ Reject ] / [ Cancel ] buttons — see
//!   [`reject_reason_popup`]), rather than an in-pane modal.
//!
//! The state machine here is pure and side-effect free: [`ReviewPanel`]
//! methods return a [`PanelEffect`] describing what the host loop should do
//! (swap a tmux pane, open a browser, move the task), so the rendering and
//! input logic are unit-testable with a `TestBackend` without touching tmux,
//! the filesystem, or the task board. [`run_review_panel`] is the real
//! executor that maps each effect onto `shelbi_orchestrator` / `shelbi_state`.

use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    backend::Backend,
    layout::{Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};

/// Which middle-pane view is currently shown. `Browser` isn't a persistent
/// view — it opens the system browser — so only `Chat` / `Diff` / `Vim` are
/// ever the *active* highlighted entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Chat,
    Diff,
    Vim,
}

/// The view-switcher entries. `Diff` sits directly below `Chat`; `Browser`
/// renders only when a review URL is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchItem {
    Chat,
    Diff,
    Vim,
    Browser,
}

/// One rendered line in the panel. Mirrors the sidebar's `Row` idea: section
/// headers and blanks are inert; everything else activates on Enter/click.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelRow {
    /// The square back button at the top of the panel — switches focus back
    /// to the dashboard window. Rendered as its own three-line block (bleed /
    /// button / bleed), not through the per-row list renderer.
    Back,
    /// Review status line (e.g. `Ready for review`). Inert.
    Status,
    /// The worktree folder name — click to reveal in the file manager.
    Folder,
    Blank,
    Section(&'static str),
    Switch(SwitchItem),
    Approve,
    Reject,
}

impl PanelRow {
    fn is_selectable(&self) -> bool {
        !matches!(self, PanelRow::Status | PanelRow::Blank | PanelRow::Section(_))
    }
}

/// What the host loop should do after a panel interaction. The panel never
/// performs side effects itself so its logic stays unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEffect {
    /// Nothing to do (e.g. moved the selection).
    None,
    /// Switch the active tmux window back to the dashboard, leaving the review
    /// interface loaded. The back button navigates focus only — it does not
    /// tear the interface down — so the reviewer can return to the still-loaded
    /// review by re-opening it from the sidebar.
    FocusDashboard,
    /// Swap the reviewer-chat pane into the middle slot.
    ShowChat,
    /// Open the OS-configured diff tool over the review branch's changes in
    /// the middle slot.
    ShowDiff,
    /// Swap the editor pane into the middle slot.
    ShowVim,
    /// Open the configured review URL in the system browser.
    OpenBrowser,
    /// Reveal the review worktree folder in the OS file manager.
    RevealFolder,
    /// Accept: move the task out of review via the normal accept transition,
    /// tear down the interface, and quit the panel.
    Approve,
    /// Open the reject-reason popover (a centered `tmux display-popup`). The
    /// host loop launches the popup, and on submit performs the review-reject
    /// with the typed reason, then tears down the interface and quits. The
    /// reason itself is collected outside this process (a `display-popup`
    /// child's output can't return here directly), so it isn't carried on the
    /// effect — see [`reject_reason_popup`].
    RejectPrompt,
}

/// The review panel's full state. Built once from the task's config
/// (worktree path, resolved editor name, whether a review URL exists) and
/// then driven by key/mouse events.
pub struct ReviewPanel {
    pub task_id: String,
    /// Absolute path of the review worktree — shown truncated in the header,
    /// revealed on click.
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …) for the
    /// "Edit in <name>" switch label.
    pub editor_name: String,
    /// Whether the workflow declares a review URL — gates the Browser entry.
    pub has_review_url: bool,
    /// Which middle-pane view is currently shown (drives the highlight).
    pub active_view: ActiveView,
    /// Selected panel row.
    pub selected: usize,
    pub should_quit: bool,
    /// Set when the reviewer Approved (accept). Distinguishes the accept exit
    /// from a plain q / Esc / Reject dismissal so only acceptance closes the
    /// review slot's window on the way out.
    pub approved: bool,
    pub status_line: String,
    /// Screen rect of the rendered row list — written each frame, read by the
    /// mouse handler to map a click to a row.
    pub list_area: Rect,
}

impl ReviewPanel {
    pub fn new(
        task_id: impl Into<String>,
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        has_review_url: bool,
    ) -> Self {
        let mut panel = Self {
            task_id: task_id.into(),
            worktree: worktree.into(),
            editor_name: editor_name.into(),
            has_review_url,
            active_view: ActiveView::Chat,
            selected: 0,
            should_quit: false,
            approved: false,
            status_line: String::new(),
            list_area: Rect::default(),
        };
        // Focus the Chat switch initially — it's the default middle-pane view
        // (the mockup highlights it), not the header folder above it.
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Chat)))
            .or_else(|| panel.rows().iter().position(PanelRow::is_selectable))
            .unwrap_or(0);
        panel
    }

    fn rows(&self) -> Vec<PanelRow> {
        // The Chat / Edit / Browser switches render as a full-width nav block
        // (separator lines + half-block selection bleed) that stands on its
        // own the way the main sidebar nav does — no leading section header.
        // The back button leads the panel (its own block above everything),
        // a blank giving it breathing room before the header.
        let mut rows = vec![
            PanelRow::Back,
            PanelRow::Blank,
            PanelRow::Status,
            PanelRow::Folder,
            PanelRow::Blank,
            PanelRow::Switch(SwitchItem::Chat),
            PanelRow::Switch(SwitchItem::Diff),
            PanelRow::Switch(SwitchItem::Vim),
        ];
        if self.has_review_url {
            rows.push(PanelRow::Switch(SwitchItem::Browser));
        }
        rows.push(PanelRow::Blank);
        rows.push(PanelRow::Section("Actions"));
        rows.push(PanelRow::Approve);
        rows.push(PanelRow::Reject);
        rows
    }

    /// Start index and count of the contiguous `Switch` run in [`rows`]. The
    /// switches render as a full-width nav block; everything else is a plain
    /// one-line list row, so this span is all the renderer and the click map
    /// need to agree on where the nav block sits.
    fn switch_span(&self) -> (usize, usize) {
        let rows = self.rows();
        let start = rows
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(_)))
            .unwrap_or(rows.len());
        let count = rows
            .iter()
            .skip(start)
            .take_while(|r| matches!(r, PanelRow::Switch(_)))
            .count();
        (start, count)
    }

    /// Map a rendered-line offset (from the top of the list area) back to a
    /// row index. The back button block leads the panel (its middle line is the
    /// only clickable one); rows before and after the switch group are one line
    /// each; the switch group renders as a nav block whose item `j` sits on line
    /// `navStart + 2j + 1` with inert separators on the even lines between.
    /// Mirrors [`crate::app::App::row_at`] so drawing and clicks agree.
    fn row_at_line(&self, target: usize) -> Option<usize> {
        // The back button block (rows[0]) occupies the first BACK_BLOCK_H
        // lines; only its middle line maps to the Back row.
        if target < BACK_BLOCK_H {
            return (target == BACK_BUTTON_LINE).then_some(0);
        }
        let rows = self.rows();
        let (sstart, scount) = self.switch_span();
        // Lines below the back block, 0-based. The plain rows above the switch
        // group are rows[1..sstart], one line each.
        let t = target - BACK_BLOCK_H;
        let above = sstart.saturating_sub(1);
        if t < above {
            return Some(1 + t);
        }
        let nav_lines = crate::sidebar::nav_lines(scount);
        if t < above + nav_lines {
            let offset = t - above;
            // Odd offsets are item rows; even offsets are inert separators.
            return (offset % 2 == 1).then_some(sstart + offset / 2);
        }
        let idx = sstart + scount + (t - above - nav_lines);
        (idx < rows.len()).then_some(idx)
    }

    pub fn nav_up(&mut self) {
        self.step(-1);
    }

    pub fn nav_down(&mut self) {
        self.step(1);
    }

    fn step(&mut self, delta: i32) {
        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            return;
        }
        let mut idx = self.selected.min(n - 1);
        for _ in 0..n {
            idx = if delta < 0 {
                if idx == 0 {
                    n - 1
                } else {
                    idx - 1
                }
            } else {
                (idx + 1) % n
            };
            if rows[idx].is_selectable() {
                self.selected = idx;
                return;
            }
        }
    }

    /// Fall the mid view back to Chat after its content pane (Vim / difftool)
    /// died in place. Mirrors `activate_row(Switch(Chat))` — sets the active
    /// view and returns [`PanelEffect::ShowChat`] — and moves the selection
    /// highlight to the Chat entry, so a poll-tick auto-recovery drives the
    /// exact same view-switch (and leaves the panel in the same visible state)
    /// a click on the Chat entry would. Kept as its own method so the recovery
    /// converges with the click path without duplicating the effect mapping.
    pub fn recover_to_chat(&mut self) -> PanelEffect {
        if let Some(idx) = self
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Chat)))
        {
            self.selected = idx;
        }
        self.activate_row(PanelRow::Switch(SwitchItem::Chat))
    }

    /// Activate the selected row (Enter / Space).
    pub fn activate(&mut self) -> PanelEffect {
        let rows = self.rows();
        let Some(row) = rows.get(self.selected) else {
            return PanelEffect::None;
        };
        self.activate_row(row.clone())
    }

    fn activate_row(&mut self, row: PanelRow) -> PanelEffect {
        match row {
            PanelRow::Back => PanelEffect::FocusDashboard,
            PanelRow::Folder => PanelEffect::RevealFolder,
            PanelRow::Switch(SwitchItem::Chat) => {
                self.active_view = ActiveView::Chat;
                PanelEffect::ShowChat
            }
            PanelRow::Switch(SwitchItem::Diff) => {
                self.active_view = ActiveView::Diff;
                PanelEffect::ShowDiff
            }
            PanelRow::Switch(SwitchItem::Vim) => {
                self.active_view = ActiveView::Vim;
                PanelEffect::ShowVim
            }
            PanelRow::Switch(SwitchItem::Browser) => PanelEffect::OpenBrowser,
            PanelRow::Approve => PanelEffect::Approve,
            PanelRow::Reject => PanelEffect::RejectPrompt,
            PanelRow::Status | PanelRow::Blank | PanelRow::Section(_) => PanelEffect::None,
        }
    }

    /// Map a click at (`column`, `row`) to a row activation. Returns `None`
    /// when the click misses the list or lands on an inert row.
    pub fn click(&mut self, column: u16, row: u16) -> PanelEffect {
        let area = self.list_area;
        if area.width == 0
            || area.height == 0
            || column < area.x
            || column >= area.x.saturating_add(area.width)
            || row < area.y
            || row >= area.y.saturating_add(area.height)
        {
            return PanelEffect::None;
        }
        // The switch group renders as a nav block (separators between/around
        // each item), so a clicked line no longer maps 1:1 to a row index.
        let Some(idx) = self.row_at_line((row - area.y) as usize) else {
            return PanelEffect::None;
        };
        let rows = self.rows();
        match rows.get(idx) {
            Some(r) if r.is_selectable() => {
                self.selected = idx;
                self.activate_row(r.clone())
            }
            _ => PanelEffect::None,
        }
    }

}

/// Left-truncate `path` to at most `width` columns, prefixing `...` when it's
/// clipped, so the tail (the folder name the reviewer cares about) always
/// stays visible: `/a/b/c/.shelbi/wt/review` → `...ct/.shelbi/wt/review`.
pub fn truncate_left(path: &str, width: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= width {
        return path.to_string();
    }
    if width <= 3 {
        // No room for the ellipsis + content — show the last `width` chars.
        return chars[chars.len().saturating_sub(width)..].iter().collect();
    }
    let keep = width - 3;
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("...{tail}")
}

/// 1-col horizontal padding shared by every non-nav row. The switch nav block
/// deliberately bypasses it (its fill and half-block bleed paint edge to edge)
/// while its label text re-applies the same indent, so labels stay aligned
/// with the rows above and below — the same split the main sidebar uses.
const LIST_INDENT: Margin = Margin {
    horizontal: 1,
    vertical: 0,
};

/// The square back button occupies three rendered lines: a lower-half-block
/// bleed row, the button (arrow) row, and an upper-half-block bleed row. The
/// two bleed rows extend the button's fill half a cell up and down so a single
/// text row reads as a square block — the same trick the sidebar nav uses for
/// its selection (see [`crate::sidebar::BLEED_ABOVE`] / `BLEED_BELOW`).
const BACK_BLOCK_H: usize = 3;
/// The button (arrow) sits on the middle line of the three-line block.
const BACK_BUTTON_LINE: usize = 1;
/// Column width of the back button. A terminal cell is ~twice as tall as it is
/// wide and the half-block bleed makes the button ~2 cells tall, so a handful
/// of columns reads as roughly square; the arrow is centered within it.
const BACK_BTN_WIDTH: usize = 5;

pub fn render_full(f: &mut Frame, app: &mut ReviewPanel, area: Rect) {
    // The list spans the full pane width so the switch nav block's selection
    // fill and half-block bleed can paint edge to edge; the plain rows above
    // and below re-apply the 1-col indent themselves.
    app.list_area = area;
    let rows = app.rows();
    let (sstart, scount) = app.switch_span();

    // 1. The square back button block leads the panel, occupying the top
    //    BACK_BLOCK_H lines.
    let back_h = (BACK_BLOCK_H as u16).min(area.height);
    render_back_button(
        f,
        app,
        Rect {
            height: back_h,
            ..area
        },
    );
    if area.height <= back_h {
        return;
    }
    // Everything else renders below the back block. `sstart` counts the Back
    // row (rows[0]); the plain rows above the switch group are rows[1..sstart].
    let below_y = area.y + back_h;
    let below_h = area.height - back_h;

    // Region above the switches (blank / status / folder / blank), one line each.
    let above_n = (sstart.saturating_sub(1)) as u16;
    let a_h = above_n.min(below_h);
    let a_area = Rect {
        y: below_y,
        height: a_h,
        ..area
    };
    render_row_list(f, app, &rows[1..sstart], 1, a_area.inner(LIST_INDENT));
    if below_h <= a_h {
        return;
    }

    // The switch group itself — a full-width nav block: a separator line
    // between (and bracketing) each item, the selected item filling edge to
    // edge with its adjacent separators carrying the half-block bleed. Same
    // treatment as the main sidebar nav.
    let nav_h = (crate::sidebar::nav_lines(scount) as u16).min(below_h - a_h);
    let nav_area = Rect {
        y: below_y + a_h,
        height: nav_h,
        ..area
    };
    render_switch_nav(f, app, nav_area, sstart, scount);

    // Region below the switches (blank, Actions header, Approve / Reject).
    let used = a_h + nav_h;
    if below_h > used {
        let rest = Rect {
            y: below_y + used,
            height: below_h - used,
            ..area
        }
        .inner(LIST_INDENT);
        let offset = sstart + scount;
        render_row_list(f, app, &rows[offset..], offset, rest);
    }
}

/// Render the square back button block: a lower-half-block bleed row, the
/// arrow row, and an upper-half-block bleed row. The bleed rows carry the
/// button's fill colour in their *foreground* so the eye reads the fill as
/// continuing half a cell past the arrow row — the same half-block trick the
/// sidebar nav uses — making the single arrow row read as a square. The arrow
/// glyph (`←`, U+2190) is the only content; there is no text label. Selecting
/// the button brightens the arrow; the fill is constant so it always reads as
/// a raised square.
fn render_back_button(f: &mut Frame, app: &ReviewPanel, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let selected = app.selected == 0; // Back is always rows[0].
    let bg = crate::theme::SELECTION_BG;
    let width = area.width as usize;
    // Keep a 1-col indent (matching LIST_INDENT) so the button sits in from the
    // pane edge like the labels below it.
    let btn_w = BACK_BTN_WIDTH.min(width.saturating_sub(1)).max(1);
    let left = (btn_w - 1) / 2;
    let right = btn_w - 1 - left;
    let btn_text = format!("{}\u{2190}{}", " ".repeat(left), " ".repeat(right));
    let arrow_style = if selected {
        Style::default()
            .fg(Color::White)
            .bg(bg)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray).bg(bg)
    };
    let bleed_style = Style::default().fg(bg);
    let lines = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(crate::sidebar::BLEED_ABOVE.repeat(btn_w), bleed_style),
        ]),
        Line::from(vec![Span::raw(" "), Span::styled(btn_text, arrow_style)]),
        Line::from(vec![
            Span::raw(" "),
            Span::styled(crate::sidebar::BLEED_BELOW.repeat(btn_w), bleed_style),
        ]),
    ];
    f.render_widget(Paragraph::new(lines), area);
}

/// Render a slice of one-line rows as an indented `List`, highlighting the row
/// at `app.selected` (a global row index; `offset` is where this slice starts
/// in the full row list) with the same full-row selection fill the sidebar's
/// rest-of-list uses.
fn render_row_list(
    f: &mut Frame,
    app: &ReviewPanel,
    rows: &[PanelRow],
    offset: usize,
    area: Rect,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width as usize;
    let mut items: Vec<ListItem> = Vec::with_capacity(rows.len());
    for (i, row) in rows.iter().enumerate() {
        let selected = offset + i == app.selected && row.is_selectable();
        items.push(render_row(app, row, selected, width));
    }
    let mut state = ListState::default();
    if app.selected >= offset && app.selected < offset + rows.len() {
        state.select(Some(app.selected - offset));
    }
    let list = List::new(items).highlight_style(Style::default().bg(crate::theme::SELECTION_BG));
    f.render_stateful_widget(list, area, &mut state);
}

/// Render the Chat / Edit / Browser switches as a full-width nav block,
/// mirroring the main sidebar's `render_nav`: a separator line between (and
/// bracketing) each item, with the selected item's fill spanning edge to edge
/// and its adjacent separators carrying the half-block bleed. Text keeps the
/// same 1-col indent as the rest of the list.
fn render_switch_nav(f: &mut Frame, app: &ReviewPanel, area: Rect, sstart: usize, scount: usize) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = app.rows();
    // Which switch (0-indexed within the group) is focused, if any.
    let selected =
        (app.selected >= sstart && app.selected < sstart + scount).then(|| app.selected - sstart);
    let width = area.width as usize;
    let bleed = crate::theme::SELECTION_BG;

    let mut lines: Vec<Line> = Vec::with_capacity(crate::sidebar::nav_lines(scount));
    for p in 0..=scount {
        // Separator `p` sits between item `p-1` (above) and item `p` (below).
        // It shows the lower half-block when the item below it is selected and
        // the upper half-block when the item above it is selected; blank
        // otherwise. Only one item is ever selected, so the cases are exclusive.
        let glyph = if selected == Some(p) {
            Some(crate::sidebar::BLEED_ABOVE)
        } else if p > 0 && selected == Some(p - 1) {
            Some(crate::sidebar::BLEED_BELOW)
        } else {
            None
        };
        lines.push(match glyph {
            Some(g) => Line::from(Span::styled(g.repeat(width), Style::default().fg(bleed))),
            None => Line::raw(""),
        });
        if let Some(PanelRow::Switch(item)) = rows.get(sstart + p) {
            lines.push(switch_nav_line(app, *item, selected == Some(p), width, bleed));
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// One switch nav row. Selected rows fill edge to edge with the selection
/// background (label padded out to the full width) and render white/bold;
/// the active middle-pane view is distinguished by its cyan/bold tint when
/// unselected; other rows are plain gray. The single leading space keeps the
/// label aligned with the 1-col-indented rows above and below.
fn switch_nav_line(
    app: &ReviewPanel,
    item: SwitchItem,
    selected: bool,
    width: usize,
    bg: Color,
) -> Line<'static> {
    let (glyph, label, active) = match item {
        SwitchItem::Chat => (
            "🤓",
            "Chat with Reviewer".to_string(),
            app.active_view == ActiveView::Chat,
        ),
        SwitchItem::Diff => (
            "🔀",
            "View Diff".to_string(),
            app.active_view == ActiveView::Diff,
        ),
        SwitchItem::Vim => (
            "✍️",
            format!("Edit in {}", app.editor_name),
            app.active_view == ActiveView::Vim,
        ),
        SwitchItem::Browser => ("🌐", "Open Browser".to_string(), false),
    };
    // Two-space placeholder keeps the glyph column aligned; the active view is
    // distinguished by cyan/bold styling and the selection background, not a
    // leading marker glyph. The leading space matches the 1-col indent the
    // sidebar nav labels use.
    let marker = "  ";
    let text = format!(" {marker}{glyph} {label}");
    if selected {
        let pad = width.saturating_sub(text.chars().count());
        Line::from(Span::styled(
            format!("{text}{}", " ".repeat(pad)),
            Style::default()
                .fg(Color::White)
                .bg(bg)
                .add_modifier(Modifier::BOLD),
        ))
    } else if active {
        Line::from(Span::styled(
            text,
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(text, Style::default().fg(Color::Gray)))
    }
}

fn render_row(app: &ReviewPanel, row: &PanelRow, selected: bool, width: usize) -> ListItem<'static> {
    match row {
        // The back button is drawn by `render_back_button` as its own block,
        // never through this per-row list renderer.
        PanelRow::Back => unreachable!("the back button renders via render_back_button"),
        PanelRow::Status => ListItem::new(Line::from(Span::styled(
            "Ready for review",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ))),
        PanelRow::Folder => {
            let label = truncate_left(&app.worktree, width.max(1));
            let style = if selected {
                Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            ListItem::new(Line::from(Span::styled(format!("📂 {label}"), style)))
        }
        PanelRow::Blank => ListItem::new(Line::raw("")),
        PanelRow::Section(label) => ListItem::new(Line::from(Span::styled(
            format!("— {label} —"),
            Style::default().fg(Color::DarkGray),
        ))),
        // Switch rows are drawn by `render_switch_nav` as a full-width nav
        // block, never through this per-row list renderer.
        PanelRow::Switch(_) => unreachable!("switch rows render via the nav block"),
        PanelRow::Approve => button_item("✅ Approve", Color::Green, selected),
        PanelRow::Reject => button_item("❌ Reject", Color::Red, selected),
    }
}

fn button_item(label: &str, tint: Color, selected: bool) -> ListItem<'static> {
    let style = if selected {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(tint)
    };
    ListItem::new(Line::from(Span::styled(format!("[ {label} ]"), style)))
}

// ---------------------------------------------------------------------------
// Platform open/reveal command builders (pure, unit-tested)

/// Host OS family for picking the file-manager / browser opener.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OsKind {
    Macos,
    Windows,
    Linux,
}

/// The OS this binary was built for. Everything else is treated as Linux
/// (xdg-open), which is the correct default for the BSDs too.
pub fn current_os() -> OsKind {
    if cfg!(target_os = "macos") {
        OsKind::Macos
    } else if cfg!(target_os = "windows") {
        OsKind::Windows
    } else {
        OsKind::Linux
    }
}

/// Argv (`program`, `args`) that reveals `path` in the OS file manager:
/// `open` on macOS, `explorer` on Windows, `xdg-open` on Linux.
pub fn reveal_command(os: OsKind, path: &str) -> (String, Vec<String>) {
    let program = match os {
        OsKind::Macos => "open",
        OsKind::Windows => "explorer",
        OsKind::Linux => "xdg-open",
    };
    (program.to_string(), vec![path.to_string()])
}

/// Argv (`program`, `args`) that opens `url` in the system browser — same
/// per-platform opener as [`reveal_command`].
pub fn open_url_command(os: OsKind, url: &str) -> (String, Vec<String>) {
    let program = match os {
        OsKind::Macos => "open",
        OsKind::Windows => "explorer",
        OsKind::Linux => "xdg-open",
    };
    (program.to_string(), vec![url.to_string()])
}

/// Run a fire-and-forget opener command, mapping a launch failure to a short
/// message the caller can surface on the status line (never a crash).
fn spawn_opener(program: &str, args: &[String]) -> std::result::Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program} failed: {e}"))
}

// ---------------------------------------------------------------------------
// Executor

/// Run the review panel in the current pane on `task_id`. Builds the panel
/// state from the project/workflow config, then drives the crossterm event
/// loop, mapping each [`PanelEffect`] onto the real orchestrator/state calls.
pub fn run_review_panel(project_name: &str, task_id: &str) -> Result<()> {
    let (worktree, has_review_url) = review_context(project_name, task_id);
    let editor_name = shelbi_state::editor_display_name(&shelbi_state::resolve_editor());

    let mut term = crate::setup_terminal_pub().context("setting up terminal")?;
    let mut app = ReviewPanel::new(task_id, worktree, editor_name, has_review_url);

    let result = review_panel_loop(&mut term, &mut app, project_name);
    crate::restore_terminal_pub(&mut term).ok();
    // Every exit path (q / Esc / Approve / Reject) tears the three-column
    // interface back down: restore the agent pane to the review window's
    // middle, drop the panel/editor panes, and return focus to the dashboard.
    // Best-effort — the pane is going away regardless. This also parks focus on
    // the dashboard, so the window-close below reaps a *non-active* window and
    // never yanks the client onto some adjacent window.
    let _ = shelbi_orchestrator::review_ui::close_review_interface(project_name);
    // On accept (only), close the review slot's whole window now that the
    // accept signal has been emitted — the slot returns to idle instead of
    // lingering with just its agent pane. Ordering is load-bearing: the
    // `approve_review_task` move event fired before we reach here, mirroring the
    // dev-workspace teardown (promote, then close). q / Esc / Reject leave the
    // window standing so the still-loaded review can be reopened.
    if app.approved {
        let _ = shelbi_orchestrator::review_ui::close_review_window(project_name, task_id);
    }
    result
}

/// Resolve the review worktree path and whether a review URL is configured
/// for `task_id`. Missing config degrades gracefully — an empty worktree and
/// no browser action rather than a failed launch.
fn review_context(project_name: &str, task_id: &str) -> (String, bool) {
    let Ok(project) = shelbi_state::load_project(project_name) else {
        return (String::new(), false);
    };
    let Ok(tf) = shelbi_state::load_task(project_name, task_id) else {
        return (String::new(), false);
    };
    let worktree = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|ws| project.workspace(ws))
        .and_then(|ws| {
            let machine = project.machine(&ws.machine)?;
            Some(shelbi_orchestrator::workspace::workspace_worktree(machine, ws))
        })
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let has_review_url = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .ok()
        .map(|wf| wf.review_url_for_status(tf.task.column.as_str()).is_some())
        .unwrap_or(false);
    (worktree, has_review_url)
}

fn review_panel_loop<B: Backend>(
    term: &mut Terminal<B>,
    app: &mut ReviewPanel,
    project_name: &str,
) -> Result<()> {
    while !app.should_quit {
        term.draw(|f| render_full(f, app, f.area()))?;
        // Recover a dead middle content pane before waiting on input, on the
        // same cadence as the event poll below. When the mid view's `exec`'d
        // process exits (Vim `:q`, difftool quit) its pane dies in place; fall
        // the view back to Chat via the same `ShowChat` effect a click on the
        // Chat entry produces, so the window never sits showing a dead pane.
        // `mid_content_pane_dead` is guarded to the active review window and is
        // idempotent (false once Chat is back / while the pane is live), so a
        // live pane is never rebuilt and the view is never double-switched.
        if app.active_view != ActiveView::Chat
            && shelbi_orchestrator::review_ui::mid_content_pane_dead(project_name, &app.task_id)
        {
            let effect = app.recover_to_chat();
            perform_effect(app, project_name, effect);
        }
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let effect = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    app.should_quit = true;
                    PanelEffect::None
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    app.nav_up();
                    PanelEffect::None
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.nav_down();
                    PanelEffect::None
                }
                KeyCode::Enter | KeyCode::Char(' ') => app.activate(),
                _ => PanelEffect::None,
            },
            Event::Mouse(m) => handle_mouse(app, m),
            _ => PanelEffect::None,
        };
        perform_effect(app, project_name, effect);
    }
    Ok(())
}

fn handle_mouse(app: &mut ReviewPanel, mouse: MouseEvent) -> PanelEffect {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => app.click(mouse.column, mouse.row),
        _ => PanelEffect::None,
    }
}

/// Map one [`PanelEffect`] onto the real world. Opener/tmux failures surface
/// on the status line (spec: "failures surface as a status-line warning, not
/// a crash").
fn perform_effect(app: &mut ReviewPanel, project_name: &str, effect: PanelEffect) {
    match effect {
        PanelEffect::None => {}
        // Back button: navigate focus to the dashboard without tearing the
        // interface down (no `should_quit`), so the still-loaded review stays
        // put and can be re-opened from the sidebar.
        PanelEffect::FocusDashboard => {
            if let Err(e) = shelbi_orchestrator::review_ui::focus_dashboard(project_name) {
                app.status_line = format!("back to dashboard failed: {e}");
            }
        }
        PanelEffect::ShowChat => {
            if let Err(e) = shelbi_orchestrator::review_ui::show_review_view(
                project_name,
                &app.task_id,
                shelbi_orchestrator::review_ui::ReviewMidView::Chat,
            ) {
                app.status_line = format!("show chat failed: {e}");
            }
        }
        PanelEffect::ShowDiff => {
            if let Err(e) = shelbi_orchestrator::review_ui::show_review_view(
                project_name,
                &app.task_id,
                shelbi_orchestrator::review_ui::ReviewMidView::Diff,
            ) {
                app.status_line = format!("view diff failed: {e}");
            }
        }
        PanelEffect::ShowVim => {
            if let Err(e) = shelbi_orchestrator::review_ui::show_review_view(
                project_name,
                &app.task_id,
                shelbi_orchestrator::review_ui::ReviewMidView::Editor,
            ) {
                app.status_line = format!("open editor failed: {e}");
            }
        }
        PanelEffect::OpenBrowser => match review_url(project_name, &app.task_id) {
            Some(url) => {
                let (prog, args) = open_url_command(current_os(), &url);
                if let Err(e) = spawn_opener(&prog, &args) {
                    app.status_line = e;
                }
            }
            None => app.status_line = "no review URL configured".into(),
        },
        PanelEffect::RevealFolder => {
            if app.worktree.is_empty() {
                app.status_line = "no review worktree to reveal".into();
            } else {
                let (prog, args) = reveal_command(current_os(), &app.worktree);
                if let Err(e) = spawn_opener(&prog, &args) {
                    app.status_line = e;
                }
            }
        }
        // Approve / Reject mutate the board, then quit — the loop's exit path
        // runs close_review_interface to restore the dashboard layout.
        PanelEffect::Approve => {
            match shelbi_orchestrator::review_ui::approve_review_task(project_name, &app.task_id) {
                Ok(()) => {
                    app.approved = true;
                    app.should_quit = true;
                }
                Err(e) => app.status_line = format!("approve failed: {e}"),
            }
        }
        // Reject opens the reason popover (a centered tmux display-popup). A
        // submitted reason drives the same review-reject transition the old
        // inline prompt did; a cancel (or a failed popup launch) leaves the
        // task untouched.
        PanelEffect::RejectPrompt => {
            if let Some(reason) = reject_reason_popup() {
                match shelbi_orchestrator::review_ui::reject_review_task(
                    project_name,
                    &app.task_id,
                    &reason,
                ) {
                    Ok(()) => app.should_quit = true,
                    Err(e) => app.status_line = format!("reject failed: {e}"),
                }
            }
        }
    }
}

/// Launch the reject-reason prompt as a centered `tmux display-popup` running
/// `shelbi __review-reject-reason`, returning the typed reason on submit or
/// `None` on cancel / launch failure.
///
/// The reason round-trips through a temp file: a `display-popup -E` child's
/// stdout goes to the popup pane, not back to us, so the popup writes the
/// reason to `--out PATH` on submit and exits 0; we read it back. The file is
/// removed before launch (so a stale one can't masquerade as a submit) and
/// after read. Mirrors the launch pattern of `App::review_load_dialog` in
/// `app.rs` — `-B` suppresses tmux's own border so only the widget's frame
/// shows. Any tmux/IO failure degrades to `None` (a no-op), never a crash.
fn reject_reason_popup() -> Option<String> {
    let bin = std::env::current_exe().ok()?;
    let bin = bin.to_string_lossy();
    // Per-process temp path — the review panel handles one reject at a time
    // (the popup blocks its loop), so the pid alone keeps concurrent review
    // panels from colliding on the round-trip file.
    let out = std::env::temp_dir().join(format!("shelbi-reject-reason-{}.txt", std::process::id()));
    let cmd = format!(
        "{} __review-reject-reason --out {}",
        shelbi_agent::shell_escape(&bin),
        shelbi_agent::shell_escape(&out.to_string_lossy()),
    );
    let _ = std::fs::remove_file(&out);
    // `-h 18` leaves room for the label, a several-line bordered text area, the
    // button row, and the hint without clipping — the reason field is now a
    // multi-line editor, so it wants the extra vertical space.
    let ok = run_tmux([
        "display-popup",
        "-B",
        "-E",
        "-w",
        "70",
        "-h",
        "18",
        cmd.as_str(),
    ]);
    let reason = ok.then(|| std::fs::read_to_string(&out).ok()).flatten();
    let _ = std::fs::remove_file(&out);
    reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
}

/// Run `tmux ARGS`, returning whether it exited successfully. Mirrors
/// `app::run_tmux`; kept local so the review panel's popup launch doesn't
/// reach across modules.
fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Resolve the concrete review URL to open for `task_id` (with `$PORT` /
/// `$SLOT` substituted), or `None` when none is configured.
fn review_url(project_name: &str, task_id: &str) -> Option<String> {
    let project = shelbi_state::load_project(project_name).ok()?;
    let tf = shelbi_state::load_task(project_name, task_id).ok()?;
    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task).ok()?;
    let template = workflow.review_url_for_status(tf.task.column.as_str())?;
    let port = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|ws| project.workspace(ws))
        .and_then(|ws| ws.slot)
        .and_then(|s| u16::try_from(s).ok());
    Some(shelbi_core::substitute_review_url(template, port))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn panel(has_url: bool) -> ReviewPanel {
        ReviewPanel::new(
            "fix-login",
            "/Users/j/proj/.shelbi/wt/review",
            "Vim".to_string(),
            has_url,
        )
    }

    fn dump(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render(app: &mut ReviewPanel, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render_full(f, app, f.area())).unwrap();
        dump(&term)
    }

    /// Render and split into per-row strings — the nav-treatment tests assert
    /// on separator / bleed geometry, which needs line-by-line access.
    fn render_lines(app: &mut ReviewPanel, w: u16, h: u16) -> Vec<String> {
        render(app, w, h).split('\n').map(str::to_string).collect()
    }

    fn row_y(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("expected row containing {needle:?} in:\n{}", rows.join("\n")))
    }

    #[test]
    fn truncate_left_prefixes_ellipsis_and_keeps_the_tail() {
        // width includes the "..." so a 12-wide budget keeps the last 9 chars.
        assert_eq!(truncate_left("/a/b/c/.shelbi/wt/review", 12), "...wt/review");
        assert_eq!(truncate_left("/a/b/c/.shelbi/wt/review", 12).chars().count(), 12);
        assert_eq!(truncate_left("short", 20), "short");
        assert!(truncate_left("/very/long/path/here", 12).starts_with("..."));
        assert!(truncate_left("/very/long/path/here", 12).ends_with("path/here"));
    }

    #[test]
    fn renders_header_switcher_and_action_buttons() {
        let mut app = panel(true);
        // Wide enough that the (short) worktree path isn't truncated; tall
        // enough that the back button + header + switches + actions all fit.
        let out = render(&mut app, 44, 20);
        assert!(out.contains("Ready for review"), "header status: {out}");
        assert!(out.contains("wt/review"), "worktree folder shown: {out}");
        assert!(out.contains("Chat with Reviewer"), "chat switch: {out}");
        assert!(out.contains("View Diff"), "diff switch: {out}");
        assert!(out.contains("Edit in Vim"), "editor switch reflects name: {out}");
        assert!(out.contains("Open Browser"), "browser switch when url set: {out}");
        assert!(out.contains("Approve"), "approve button: {out}");
        assert!(out.contains("Reject"), "reject button: {out}");
    }

    /// The back button leads the panel: a square block (bleed row above, arrow
    /// row, bleed row below) sitting above the "Ready for review" header, with
    /// only the back-arrow glyph and no text label.
    #[test]
    fn back_button_renders_a_square_arrow_at_the_top() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 30, 20);
        // Arrow on the button's middle line; bleed rows bracket it.
        assert!(
            rows[BACK_BUTTON_LINE].contains('\u{2190}'),
            "back-arrow glyph on the button line: {:?}",
            rows[BACK_BUTTON_LINE]
        );
        assert!(
            rows[BACK_BUTTON_LINE - 1].contains(crate::sidebar::BLEED_ABOVE),
            "lower-half-block bleed above the button: {:?}",
            rows[BACK_BUTTON_LINE - 1]
        );
        assert!(
            rows[BACK_BUTTON_LINE + 1].contains(crate::sidebar::BLEED_BELOW),
            "upper-half-block bleed below the button: {:?}",
            rows[BACK_BUTTON_LINE + 1]
        );
        // Glyph only — no text label on the button row.
        assert!(
            !rows[BACK_BUTTON_LINE].chars().any(|c| c.is_alphabetic()),
            "back button carries no text label: {:?}",
            rows[BACK_BUTTON_LINE]
        );
        // It sits above the header, which is the whole point of a top button.
        assert!(
            row_y(&rows, "Ready for review") > BACK_BUTTON_LINE,
            "back button must sit above the status header, rows:\n{}",
            rows.join("\n")
        );
    }

    /// The back button's fill reads as a square: the arrow line carries the
    /// selection background across the button's cells, and the bleed rows carry
    /// that same colour in their foreground so it continues half a cell up/down.
    #[test]
    fn back_button_fill_reads_as_a_square() {
        let mut term = Terminal::new(TestBackend::new(30, 20)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        // The button starts at the 1-col indent (column 1). Its fill covers the
        // arrow row (BACK_BUTTON_LINE); the bleed rows above/below paint the
        // same colour in the foreground.
        assert_eq!(
            buf[(1, BACK_BUTTON_LINE as u16)].bg,
            crate::theme::SELECTION_BG,
            "arrow row carries the button fill"
        );
        assert_eq!(
            buf[(1, (BACK_BUTTON_LINE - 1) as u16)].fg,
            crate::theme::SELECTION_BG,
            "bleed above carries the fill colour so it reads square"
        );
        assert_eq!(
            buf[(1, (BACK_BUTTON_LINE + 1) as u16)].fg,
            crate::theme::SELECTION_BG,
            "bleed below carries the fill colour so it reads square"
        );
    }

    /// Activating the back button (Enter or click) asks the host loop to switch
    /// focus back to the dashboard — it does not tear the interface down.
    #[test]
    fn activating_back_button_focuses_the_dashboard() {
        let mut app = panel(true);
        let idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Back))
            .unwrap();
        assert_eq!(idx, 0, "back button leads the panel");
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::FocusDashboard);
        // A click on the button's middle line maps to the same effect.
        let _ = render(&mut app, 30, 20); // populate list_area
        assert_eq!(app.click(1, BACK_BUTTON_LINE as u16), PanelEffect::FocusDashboard);
    }

    /// The leading "Actions" header above the switches is gone — the three
    /// action items stand on their own the way the main nav does. The switch
    /// group renders before any remaining section header.
    #[test]
    fn leading_actions_header_no_longer_renders_above_the_switches() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 20);
        // The Chat switch is the first action, sitting directly under the
        // folder row with no "Actions" divider above it.
        let chat_y = row_y(&rows, "Chat with Reviewer");
        let actions_ys: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, r)| r.contains("Actions"))
            .map(|(y, _)| y)
            .collect();
        // Only the Approve/Reject group keeps a header, and it sits below the
        // switches — nothing labelled "Actions" renders above them.
        assert!(
            actions_ys.iter().all(|&y| y > chat_y),
            "no 'Actions' header may render above the switches, got headers at {actions_ys:?}, chat at {chat_y}"
        );
    }

    /// The selected switch's adjacent lines carry the half-block bleed — a
    /// full-width row of U+2584 directly above and U+2580 directly below, both
    /// spanning the pane edge to edge — exactly like the main sidebar nav.
    #[test]
    fn selected_switch_renders_full_width_half_block_bleed() {
        let width = 30u16;
        let mut app = panel(true);
        // Move focus to the Edit switch (Chat / View Diff / Edit / Browser) so
        // both a separator-above and separator-below are asserted.
        app.nav_down();
        app.nav_down();
        let rows = render_lines(&mut app, width, 20);
        let edit_y = row_y(&rows, "Edit in Vim");
        assert_eq!(
            rows[edit_y - 1],
            crate::sidebar::BLEED_ABOVE.repeat(width as usize),
            "line above the selected switch is full-width U+2584"
        );
        assert_eq!(
            rows[edit_y + 1],
            crate::sidebar::BLEED_BELOW.repeat(width as usize),
            "line below the selected switch is full-width U+2580"
        );
        // A switch that isn't adjacent to the selection keeps a blank
        // separator — no bleed leaks onto it.
        let browser_y = row_y(&rows, "Open Browser");
        assert!(
            rows[browser_y + 1].trim().is_empty(),
            "the line below the unselected Browser switch stays blank, got: {:?}",
            rows[browser_y + 1]
        );
    }

    /// A blank separator line sits between every pair of switches (and above
    /// the first / below the last), so moving the selection never shifts where
    /// the labels land — the same rhythm as the main nav.
    #[test]
    fn switch_separators_keep_labels_from_shifting_across_selection() {
        let mut chat = panel(true); // Chat focused by default
        let chat_rows = render_lines(&mut chat, 30, 20);

        let mut moved = panel(true);
        moved.nav_down(); // focus View Diff
        let moved_rows = render_lines(&mut moved, 30, 20);

        for label in ["Chat with Reviewer", "View Diff", "Edit in Vim", "Open Browser"] {
            assert_eq!(
                row_y(&chat_rows, label),
                row_y(&moved_rows, label),
                "'{label}' must not move when the selection changes"
            );
        }
        // Adjacent switches are one separator line apart.
        assert_eq!(
            row_y(&chat_rows, "View Diff") - row_y(&chat_rows, "Chat with Reviewer"),
            2,
            "one separator line always sits between Chat and View Diff"
        );
        assert_eq!(
            row_y(&chat_rows, "Edit in Vim") - row_y(&chat_rows, "View Diff"),
            2,
            "one separator line always sits between View Diff and Edit"
        );
    }

    /// The selected switch's fill spans the pane edge to edge (column 0) while
    /// the label keeps the 1-col indent, matching the main nav's fill.
    #[test]
    fn selected_switch_fill_spans_full_width() {
        let width = 30u16;
        let mut term = Terminal::new(TestBackend::new(width, 20)).unwrap();
        let mut app = panel(true); // Chat focused by default
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term)
            .split('\n')
            .map(str::to_string)
            .collect::<Vec<_>>();
        let chat_y = row_y(&rows, "Chat with Reviewer") as u16;

        // Both the left gutter (column 0) and the trailing padding carry the
        // selection fill.
        assert_eq!(
            buf[(0, chat_y)].bg,
            crate::theme::SELECTION_BG,
            "left edge (indent gutter) must carry the selection fill"
        );
        for x in (width - 4)..width {
            assert_eq!(
                buf[(x, chat_y)].bg,
                crate::theme::SELECTION_BG,
                "right edge padding must carry the selection fill, col {x}"
            );
        }
        assert_eq!(
            buf[(0, chat_y)].symbol(),
            " ",
            "column 0 is the indent gutter, not the label"
        );
    }

    #[test]
    fn browser_entry_hidden_without_review_url() {
        let mut app = panel(false);
        let out = render(&mut app, 30, 20);
        assert!(!out.contains("Open Browser"), "no browser action when no url: {out}");
        // The rest of the panel still renders.
        assert!(out.contains("Chat with Reviewer") && out.contains("Approve"));
    }

    #[test]
    fn editor_label_tracks_resolved_editor_name() {
        let mut app = ReviewPanel::new("t", "/wt", "Helix".to_string(), false);
        let out = render(&mut app, 30, 20);
        assert!(out.contains("Edit in Helix"), "label uses resolved editor: {out}");
    }

    /// Move the selection to the switch backing `item` (used by the
    /// activation tests so they don't hard-code a step count that shifts as
    /// switches are added or removed).
    fn select_switch(app: &mut ReviewPanel, item: SwitchItem) {
        let idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(i) if *i == item))
            .unwrap();
        app.selected = idx;
    }

    #[test]
    fn activating_chat_and_vim_returns_switch_effects_and_marks_active() {
        let mut app = panel(true);
        // Default selection is Chat; active view starts Chat.
        assert_eq!(app.active_view, ActiveView::Chat);
        // Move to the Vim switch and activate.
        select_switch(&mut app, SwitchItem::Vim);
        assert_eq!(app.activate(), PanelEffect::ShowVim);
        assert_eq!(app.active_view, ActiveView::Vim);
        // Back to Chat.
        select_switch(&mut app, SwitchItem::Chat);
        assert_eq!(app.activate(), PanelEffect::ShowChat);
        assert_eq!(app.active_view, ActiveView::Chat);
    }

    /// The dead-mid-pane recovery (`recover_to_chat`) drives the exact same
    /// effect + active-view state a click on the Chat entry produces: from a
    /// Vim / Diff mid view it returns `ShowChat`, flips the active view back to
    /// Chat, and moves the selection highlight onto the Chat switch so the panel
    /// reads identically either way. This is the state-machine half of the
    /// poll-tick recovery in `review_panel_loop`.
    #[test]
    fn recover_to_chat_mirrors_the_chat_switch_click() {
        let mut app = panel(true);
        // Simulate the user having switched the mid view to the editor.
        select_switch(&mut app, SwitchItem::Vim);
        assert_eq!(app.activate(), PanelEffect::ShowVim);
        assert_eq!(app.active_view, ActiveView::Vim);

        // The editor pane died (Vim `:q`): recovery falls the view back to Chat.
        assert_eq!(app.recover_to_chat(), PanelEffect::ShowChat);
        assert_eq!(app.active_view, ActiveView::Chat);
        // Selection now sits on the Chat switch, matching the click path.
        let chat_idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Chat)))
            .unwrap();
        assert_eq!(app.selected, chat_idx, "selection moves onto the Chat entry");

        // Idempotent: recovering again while already on Chat is still a ShowChat
        // no-op switch (the loop guards on `active_view != Chat` so it never even
        // calls this again, but the method itself must stay stable).
        assert_eq!(app.recover_to_chat(), PanelEffect::ShowChat);
        assert_eq!(app.active_view, ActiveView::Chat);
    }

    /// View Diff sits directly below Chat with Reviewer in the switch group —
    /// the ordering the wireframe pins — with no other selectable row between
    /// them.
    #[test]
    fn view_diff_renders_directly_below_chat() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 16);
        let chat_y = row_y(&rows, "Chat with Reviewer");
        let diff_y = row_y(&rows, "View Diff");
        assert!(diff_y > chat_y, "View Diff must render below Chat");
        // Exactly one separator line between them — nothing else is interleaved.
        assert_eq!(
            diff_y - chat_y,
            2,
            "View Diff sits immediately below Chat with Reviewer"
        );
        // And it precedes the editor switch, preserving the rest of the order.
        assert!(
            diff_y < row_y(&rows, "Edit in Vim"),
            "View Diff comes before Edit in the switch group"
        );
    }

    /// The row order in the pure model puts the Diff switch between Chat and
    /// Vim — the same invariant the render test asserts, checked structurally so
    /// a renderer change can't mask a model regression.
    #[test]
    fn diff_switch_is_second_in_the_switch_group() {
        let app = panel(true);
        let switches: Vec<SwitchItem> = app
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                PanelRow::Switch(i) => Some(i),
                _ => None,
            })
            .collect();
        assert_eq!(
            switches,
            vec![
                SwitchItem::Chat,
                SwitchItem::Diff,
                SwitchItem::Vim,
                SwitchItem::Browser
            ]
        );
    }

    #[test]
    fn activating_view_diff_returns_show_diff_and_marks_active() {
        let mut app = panel(true);
        select_switch(&mut app, SwitchItem::Diff);
        assert_eq!(app.activate(), PanelEffect::ShowDiff);
        assert_eq!(app.active_view, ActiveView::Diff);
    }

    /// The mouse path reaches View Diff through the same nav-block click map the
    /// other switches use: a click on the rendered "View Diff" line dispatches
    /// ShowDiff and marks it the active view.
    #[test]
    fn clicking_view_diff_dispatches_show_diff() {
        let mut app = panel(true);
        // Render so `list_area` and the switch-block geometry are populated.
        let rows = render_lines(&mut app, 44, 16);
        let diff_line = row_y(&rows, "View Diff") as u16;
        // The test backend renders the list at the pane origin (0, 0).
        let effect = app.click(2, diff_line);
        assert_eq!(effect, PanelEffect::ShowDiff);
        assert_eq!(app.active_view, ActiveView::Diff);
    }

    #[test]
    fn reject_row_activation_requests_the_reason_popover() {
        // The reason is now collected in a tmux display-popup outside this
        // process; activating Reject just asks the host loop to launch it.
        let mut app = panel(false);
        while !matches!(app.rows().get(app.selected), Some(PanelRow::Reject)) {
            app.nav_down();
        }
        assert_eq!(app.activate(), PanelEffect::RejectPrompt);
    }

    #[test]
    fn folder_row_activation_reveals_the_worktree() {
        let mut app = panel(false);
        // Header folder is the first selectable row above the switcher; select
        // it explicitly and activate.
        let idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Folder))
            .unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::RevealFolder);
    }

    #[test]
    fn approve_row_activation_returns_approve_effect() {
        let mut app = panel(false);
        let idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Approve))
            .unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::Approve);
    }

    #[test]
    fn browser_activation_returns_open_browser_effect() {
        let mut app = panel(true);
        let idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Browser)))
            .unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::OpenBrowser);
    }

    #[test]
    fn reveal_and_open_commands_are_per_platform() {
        assert_eq!(
            reveal_command(OsKind::Macos, "/p"),
            ("open".to_string(), vec!["/p".to_string()])
        );
        assert_eq!(
            reveal_command(OsKind::Linux, "/p"),
            ("xdg-open".to_string(), vec!["/p".to_string()])
        );
        assert_eq!(
            reveal_command(OsKind::Windows, "C:\\p"),
            ("explorer".to_string(), vec!["C:\\p".to_string()])
        );
        assert_eq!(
            open_url_command(OsKind::Macos, "https://x"),
            ("open".to_string(), vec!["https://x".to_string()])
        );
        assert_eq!(
            open_url_command(OsKind::Linux, "https://x"),
            ("xdg-open".to_string(), vec!["https://x".to_string()])
        );
    }

}
