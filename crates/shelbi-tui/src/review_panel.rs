//! The right-hand **review panel** of the review interface — a dedicated
//! ratatui view hosted in a third tmux pane alongside the project sidebar
//! (left) and the swappable content slot (middle). It renders, for the
//! Ready-for-review task it was launched on:
//!
//! - a **header** showing review status + the review worktree's folder name
//!   (left-truncated to fit; click to reveal it in the OS file manager),
//! - a **view-switcher** action group — *Chat with Reviewer* (default) /
//!   *Edit in <editor>* / *Open Browser* (the last only when the workflow
//!   declares a review URL), the active middle-pane view highlighted, and
//! - an **Approve** / **Reject** action group, Reject opening a
//!   type-the-reason dialog.
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
    self, Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    backend::Backend,
    layout::{Margin, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

/// Which middle-pane view is currently shown. `Browser` isn't a persistent
/// view — it opens the system browser — so only `Chat` / `Vim` are ever the
/// *active* highlighted entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveView {
    Chat,
    Vim,
}

/// The three view-switcher entries. `Browser` renders only when a review URL
/// is configured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchItem {
    Chat,
    Vim,
    Browser,
}

/// One rendered line in the panel. Mirrors the sidebar's `Row` idea: section
/// headers and blanks are inert; everything else activates on Enter/click.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelRow {
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
    /// Nothing to do (e.g. moved the selection, opened/typed in the dialog).
    None,
    /// Swap the reviewer-chat pane into the middle slot.
    ShowChat,
    /// Swap the editor pane into the middle slot.
    ShowVim,
    /// Open the configured review URL in the system browser.
    OpenBrowser,
    /// Reveal the review worktree folder in the OS file manager.
    RevealFolder,
    /// Accept: move the task out of review via the normal accept transition,
    /// tear down the interface, and quit the panel.
    Approve,
    /// Reject with the typed reason: append it to the task body, bounce the
    /// task to the ready status, tear down the interface, and quit.
    Reject(String),
}

/// In-progress Reject dialog: the reviewer types the reason for bouncing the
/// task. An empty reason can't submit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RejectDialog {
    pub reason: String,
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
    /// Open Reject dialog, if any.
    pub dialog: Option<RejectDialog>,
    pub should_quit: bool,
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
            dialog: None,
            should_quit: false,
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
        let mut rows = vec![
            PanelRow::Status,
            PanelRow::Folder,
            PanelRow::Blank,
            PanelRow::Switch(SwitchItem::Chat),
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
    /// row index. Rows before and after the switch group are one line each;
    /// the switch group renders as a nav block whose item `j` sits on line
    /// `sstart + 2j + 1` with inert separators on the even lines between.
    /// Mirrors [`crate::app::App::row_at`] so drawing and clicks agree.
    fn row_at_line(&self, target: usize) -> Option<usize> {
        let rows = self.rows();
        let (sstart, scount) = self.switch_span();
        if target < sstart {
            return Some(target);
        }
        let nav_lines = crate::sidebar::nav_lines(scount);
        if target < sstart + nav_lines {
            let offset = target - sstart;
            // Odd offsets are item rows; even offsets are inert separators.
            return (offset % 2 == 1).then_some(sstart + offset / 2);
        }
        let idx = sstart + scount + (target - sstart - nav_lines);
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

    /// Activate the selected row (Enter / Space). No-op when a dialog is open
    /// — dialog keys are routed through [`ReviewPanel::dialog_key`] instead.
    pub fn activate(&mut self) -> PanelEffect {
        if self.dialog.is_some() {
            return PanelEffect::None;
        }
        let rows = self.rows();
        let Some(row) = rows.get(self.selected) else {
            return PanelEffect::None;
        };
        self.activate_row(row.clone())
    }

    fn activate_row(&mut self, row: PanelRow) -> PanelEffect {
        match row {
            PanelRow::Folder => PanelEffect::RevealFolder,
            PanelRow::Switch(SwitchItem::Chat) => {
                self.active_view = ActiveView::Chat;
                PanelEffect::ShowChat
            }
            PanelRow::Switch(SwitchItem::Vim) => {
                self.active_view = ActiveView::Vim;
                PanelEffect::ShowVim
            }
            PanelRow::Switch(SwitchItem::Browser) => PanelEffect::OpenBrowser,
            PanelRow::Approve => PanelEffect::Approve,
            PanelRow::Reject => {
                self.dialog = Some(RejectDialog::default());
                PanelEffect::None
            }
            PanelRow::Status | PanelRow::Blank | PanelRow::Section(_) => PanelEffect::None,
        }
    }

    /// Map a click at (`column`, `row`) to a row activation. Returns `None`
    /// when the click misses the list or lands on an inert row.
    pub fn click(&mut self, column: u16, row: u16) -> PanelEffect {
        if self.dialog.is_some() {
            return PanelEffect::None;
        }
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

    /// Feed one key to the open Reject dialog. Enter submits (only when the
    /// reason is non-blank), Esc cancels, Backspace deletes, printable chars
    /// append. Returns `Reject(reason)` on submit, else `None`.
    pub fn dialog_key(&mut self, key: KeyEvent) -> PanelEffect {
        let Some(dialog) = self.dialog.as_mut() else {
            return PanelEffect::None;
        };
        match key.code {
            KeyCode::Esc => {
                self.dialog = None;
                PanelEffect::None
            }
            KeyCode::Enter => {
                let reason = dialog.reason.trim().to_string();
                if reason.is_empty() {
                    // Empty reason must not submit — keep the dialog open.
                    self.status_line = "type a reason to reject (Esc to cancel)".into();
                    PanelEffect::None
                } else {
                    self.dialog = None;
                    PanelEffect::Reject(reason)
                }
            }
            KeyCode::Backspace => {
                dialog.reason.pop();
                PanelEffect::None
            }
            KeyCode::Char(c) => {
                dialog.reason.push(c);
                PanelEffect::None
            }
            _ => PanelEffect::None,
        }
    }

    /// Whether a Reject dialog is currently open.
    pub fn dialog_open(&self) -> bool {
        self.dialog.is_some()
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

pub fn render_full(f: &mut Frame, app: &mut ReviewPanel, area: Rect) {
    // The list spans the full pane width so the switch nav block's selection
    // fill and half-block bleed can paint edge to edge; the plain rows above
    // and below re-apply the 1-col indent themselves.
    app.list_area = area;
    let rows = app.rows();
    let (sstart, scount) = app.switch_span();
    let nav_h = crate::sidebar::nav_lines(scount) as u16;

    // Region above the switches (status / folder / blank), one line each.
    let a_h = (sstart as u16).min(area.height);
    let a_area = Rect {
        height: a_h,
        ..area
    };
    render_row_list(f, app, &rows[..sstart], 0, a_area.inner(LIST_INDENT));

    // The switch group itself — a full-width nav block: a separator line
    // between (and bracketing) each item, the selected item filling edge to
    // edge with its adjacent separators carrying the half-block bleed. Same
    // treatment as the main sidebar nav.
    if area.height > a_h {
        let nav_area = Rect {
            y: area.y + a_h,
            height: nav_h.min(area.height - a_h),
            ..area
        };
        render_switch_nav(f, app, nav_area, sstart, scount);

        // Region below the switches (blank, Actions header, Approve / Reject).
        let used = a_h + nav_h;
        if area.height > used {
            let rest = Rect {
                y: area.y + used,
                height: area.height - used,
                ..area
            }
            .inner(LIST_INDENT);
            let offset = sstart + scount;
            render_row_list(f, app, &rows[offset..], offset, rest);
        }
    }

    if app.dialog.is_some() {
        render_reject_dialog(f, app, area);
    }
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
/// the active middle-pane view keeps its leading `▸` marker and cyan/bold
/// tint when unselected; other rows are plain gray. The single leading space
/// keeps the label aligned with the 1-col-indented rows above and below.
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
        SwitchItem::Vim => (
            "✍️",
            format!("Edit in {}", app.editor_name),
            app.active_view == ActiveView::Vim,
        ),
        SwitchItem::Browser => ("🌐", "Open Browser".to_string(), false),
    };
    // The active middle-pane view is marked with a leading `▸`; the leading
    // space matches the 1-col indent the sidebar nav labels use.
    let marker = if active { "▸ " } else { "  " };
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

fn render_reject_dialog(f: &mut Frame, app: &ReviewPanel, area: Rect) {
    let reason = app.dialog.as_ref().map(|d| d.reason.as_str()).unwrap_or("");
    // Centered modal, capped so it stays readable on a narrow review pane
    // and never wider than the pane itself.
    let w = area
        .width
        .saturating_sub(2)
        .clamp(10, 48)
        .min(area.width);
    let h = 7u16.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let modal = Rect {
        x,
        y,
        width: w,
        height: h,
    };
    f.render_widget(Clear, modal);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Reject task ");
    let inner = block.inner(modal);
    f.render_widget(block, modal);

    let can_submit = !reason.trim().is_empty();
    let hint = if can_submit {
        "Enter submit · Esc cancel"
    } else {
        "type a reason · Esc cancel"
    };
    let body = Paragraph::new(vec![
        Line::from(Span::styled(
            "Reason for rejecting:",
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            format!("{reason}▌"),
            Style::default().fg(Color::White),
        )),
        Line::raw(""),
        Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray))),
    ])
    .wrap(Wrap { trim: false });
    f.render_widget(body, inner);
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
    // Best-effort — the pane is going away regardless.
    let _ = shelbi_orchestrator::review_ui::close_review_interface(project_name);
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
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let effect = match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                if app.dialog_open() {
                    app.dialog_key(k)
                } else {
                    match k.code {
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
                    }
                }
            }
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
        PanelEffect::ShowChat => {
            if let Err(e) = shelbi_orchestrator::review_ui::show_review_view(
                project_name,
                &app.task_id,
                shelbi_orchestrator::review_ui::ReviewMidView::Chat,
            ) {
                app.status_line = format!("show chat failed: {e}");
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
                Ok(()) => app.should_quit = true,
                Err(e) => app.status_line = format!("approve failed: {e}"),
            }
        }
        PanelEffect::Reject(reason) => {
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
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{backend::TestBackend, Terminal};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

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
        // Wide enough that the (short) worktree path isn't truncated.
        let out = render(&mut app, 44, 16);
        assert!(out.contains("Ready for review"), "header status: {out}");
        assert!(out.contains("wt/review"), "worktree folder shown: {out}");
        assert!(out.contains("Chat with Reviewer"), "chat switch: {out}");
        assert!(out.contains("Edit in Vim"), "editor switch reflects name: {out}");
        assert!(out.contains("Open Browser"), "browser switch when url set: {out}");
        assert!(out.contains("Approve"), "approve button: {out}");
        assert!(out.contains("Reject"), "reject button: {out}");
    }

    /// The leading "Actions" header above the switches is gone — the three
    /// action items stand on their own the way the main nav does. The switch
    /// group renders before any remaining section header.
    #[test]
    fn leading_actions_header_no_longer_renders_above_the_switches() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 16);
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
        // Move focus to the Edit switch (the middle of the three) so both a
        // separator-above and separator-below are asserted.
        app.nav_down();
        let rows = render_lines(&mut app, width, 16);
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
        let chat_rows = render_lines(&mut chat, 30, 16);

        let mut edit = panel(true);
        edit.nav_down(); // focus Edit
        let edit_rows = render_lines(&mut edit, 30, 16);

        for label in ["Chat with Reviewer", "Edit in Vim", "Open Browser"] {
            assert_eq!(
                row_y(&chat_rows, label),
                row_y(&edit_rows, label),
                "'{label}' must not move when the selection changes"
            );
        }
        // Adjacent switches are one separator line apart.
        assert_eq!(
            row_y(&chat_rows, "Edit in Vim") - row_y(&chat_rows, "Chat with Reviewer"),
            2,
            "one separator line always sits between adjacent switches"
        );
    }

    /// The selected switch's fill spans the pane edge to edge (column 0) while
    /// the label keeps the 1-col indent, matching the main nav's fill.
    #[test]
    fn selected_switch_fill_spans_full_width() {
        let width = 30u16;
        let mut term = Terminal::new(TestBackend::new(width, 16)).unwrap();
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
        let out = render(&mut app, 30, 16);
        assert!(!out.contains("Open Browser"), "no browser action when no url: {out}");
        // The rest of the panel still renders.
        assert!(out.contains("Chat with Reviewer") && out.contains("Approve"));
    }

    #[test]
    fn editor_label_tracks_resolved_editor_name() {
        let mut app = ReviewPanel::new("t", "/wt", "Helix".to_string(), false);
        let out = render(&mut app, 30, 16);
        assert!(out.contains("Edit in Helix"), "label uses resolved editor: {out}");
    }

    #[test]
    fn activating_chat_and_vim_returns_switch_effects_and_marks_active() {
        let mut app = panel(true);
        // Default selection is Chat; active view starts Chat.
        assert_eq!(app.active_view, ActiveView::Chat);
        // Move down to the Vim switch and activate.
        app.nav_down();
        assert_eq!(app.activate(), PanelEffect::ShowVim);
        assert_eq!(app.active_view, ActiveView::Vim);
        // Back up to Chat.
        app.nav_up();
        assert_eq!(app.activate(), PanelEffect::ShowChat);
        assert_eq!(app.active_view, ActiveView::Chat);
    }

    #[test]
    fn reject_requires_a_non_empty_reason_before_it_submits() {
        let mut app = panel(false);
        // Select and activate Reject → opens the dialog, no effect yet.
        while !matches!(app.rows().get(app.selected), Some(PanelRow::Reject)) {
            app.nav_down();
        }
        assert_eq!(app.activate(), PanelEffect::None);
        assert!(app.dialog_open(), "reject opens the reason dialog");

        // Enter on an empty reason must not submit.
        assert_eq!(app.dialog_key(key(KeyCode::Enter)), PanelEffect::None);
        assert!(app.dialog_open(), "empty reason keeps the dialog open");

        // Type a reason, then Enter submits it.
        for c in "please fix the null deref".chars() {
            assert_eq!(app.dialog_key(key(KeyCode::Char(c))), PanelEffect::None);
        }
        assert_eq!(
            app.dialog_key(key(KeyCode::Enter)),
            PanelEffect::Reject("please fix the null deref".to_string())
        );
        assert!(!app.dialog_open(), "dialog closes on submit");
    }

    #[test]
    fn reject_dialog_esc_cancels_without_submitting() {
        let mut app = panel(false);
        while !matches!(app.rows().get(app.selected), Some(PanelRow::Reject)) {
            app.nav_down();
        }
        app.activate();
        app.dialog_key(key(KeyCode::Char('x')));
        assert_eq!(app.dialog_key(key(KeyCode::Esc)), PanelEffect::None);
        assert!(!app.dialog_open(), "Esc closes the dialog");
    }

    #[test]
    fn reject_reason_backspace_edits_the_buffer() {
        let mut app = panel(false);
        while !matches!(app.rows().get(app.selected), Some(PanelRow::Reject)) {
            app.nav_down();
        }
        app.activate();
        for c in "abc".chars() {
            app.dialog_key(key(KeyCode::Char(c)));
        }
        app.dialog_key(key(KeyCode::Backspace));
        assert_eq!(
            app.dialog_key(key(KeyCode::Enter)),
            PanelEffect::Reject("ab".to_string())
        );
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

    #[test]
    fn dialog_swallows_activation_of_underlying_rows() {
        let mut app = panel(true);
        while !matches!(app.rows().get(app.selected), Some(PanelRow::Reject)) {
            app.nav_down();
        }
        app.activate(); // open dialog
        // Enter would normally activate a row, but the dialog is open, so
        // activate() is inert (dialog_key owns the keys now).
        assert_eq!(app.activate(), PanelEffect::None);
    }
}
