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
//!   with the **review status** (`Ready for review`) in the accent cyan on the
//!   button's own row, two columns to its right,
//! - **task info**: the task title (bold white) and the first few lines of the
//!   task description (markdown stripped, wrapped, at most three lines, ending
//!   in `...` when clipped), followed by a cyan **`More`** link / `m` key that
//!   opens the full task-description popover — see [`PanelEffect::ShowDescription`],
//! - the review worktree's folder name (left-truncated to fit; click to reveal
//!   it in the OS file manager),
//! - a **view-switcher** action group — *Chat with Reviewer* (default) /
//!   *View Diff* / *Edit in <editor>* / *Open Browser* (the last only when the
//!   workflow declares a review URL), the active content view highlighted,
//!   *View Diff* opening the OS-configured diff tool over the review branch's
//!   changes in the right-column content pane, and
//! - an **Approve** / **Reject** action row (no brackets, green / red in the
//!   design colors), Reject opening a type-the-reason popover rather than an
//!   in-pane modal.
//!
//! The state machine here is pure and side-effect free: [`ReviewPanel`]
//! methods return a [`PanelEffect`] describing what the host loop should do
//! (swap the content pane, open a browser, open the description popover, move
//! the task), so the rendering and input logic are unit-testable with a
//! `TestBackend` without touching the filesystem or the task board.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::theme::{ACCENT_CYAN, ACTION_RED, FG_SECONDARY, PALETTE_FG, PALETTE_GREEN, SELECTION_BG};

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

/// One selectable panel row — the keyboard-navigable targets (the back button,
/// the worktree folder, the view switches, and the Approve / Reject actions).
/// The task-info block (title / description / `More`) is **not** a nav row: it
/// renders as its own block above the folder and is reached by click or the
/// `m` key, so the nav cycle stays stable regardless of how the description
/// wraps.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PanelRow {
    /// The square back button at the top — switches focus back to the
    /// dashboard window.
    Back,
    /// The worktree folder name — click to reveal in the file manager.
    Folder,
    /// A middle-pane view switch.
    Switch(SwitchItem),
    Approve,
    Reject,
}

/// What the host loop should do after a panel interaction. The panel never
/// performs side effects itself so its logic stays unit-testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PanelEffect {
    /// Nothing to do (e.g. moved the selection).
    None,
    /// Switch the active window back to the dashboard, leaving the review
    /// interface loaded. The back button navigates focus only — it does not
    /// tear the interface down.
    FocusDashboard,
    /// Swap the reviewer-chat pane into the content slot.
    ShowChat,
    /// Open the OS-configured diff tool over the review branch's changes in the
    /// content slot.
    ShowDiff,
    /// Swap the editor pane into the content slot.
    ShowVim,
    /// Open the configured review URL in the system browser.
    OpenBrowser,
    /// Reveal the review worktree folder in the OS file manager.
    RevealFolder,
    /// Open the full task-description popover (the board's task-detail popover)
    /// over the main area. The host loop owns the overlay; this just asks for
    /// it. Emitted by the `More` link / `m` key, only when the task has a body.
    ShowDescription,
    /// Accept: move the task out of review via the normal accept transition,
    /// tear down the interface, and quit the panel.
    Approve,
    /// Open the reject-reason overlay. The host loop opens it, and on submit
    /// performs the review-reject with the typed reason.
    RejectPrompt,
}

/// The review panel's full state. Built once from the task's config (worktree
/// path, resolved editor name, whether a review URL exists, the task title and
/// body) and then driven by key/mouse events.
pub struct ReviewPanel {
    /// Absolute path of the review worktree — shown truncated, revealed on
    /// click.
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …) for the
    /// "Edit in <name>" switch label.
    pub editor_name: String,
    /// Whether the workflow declares a review URL — gates the Browser entry.
    pub has_review_url: bool,
    /// The reviewed task's title — shown bold white in the task-info block.
    pub title: String,
    /// The reviewed task's markdown body — the first few lines preview the
    /// task-info block; the whole thing opens in the description popover. An
    /// empty body hides the preview and the `More` link.
    pub description: String,
    /// Which middle-pane view is currently shown (drives the highlight).
    pub active_view: ActiveView,
    /// Selected panel row (index into [`rows`](Self::rows)).
    pub selected: usize,
    pub should_quit: bool,
    pub status_line: String,
    /// Set while the gated review→done merge runs on a background thread.
    pub merging: bool,
    /// Animation frame for the "merging…" spinner.
    pub spinner: usize,
    /// Screen rects of the selectable rows, written each frame and read by the
    /// mouse handler to map a click back to a row — one per [`rows`](Self::rows)
    /// entry, carrying that entry's index. Replaces the old line-math click map
    /// now that the task-info block makes the panel's vertical layout dynamic.
    hit_targets: Vec<(Rect, usize)>,
    /// Screen rect of the `More` link, when rendered (task has a body). Checked
    /// before [`hit_targets`](Self::hit_targets) so a click on it opens the
    /// description popover.
    more_hit: Option<Rect>,
}

impl ReviewPanel {
    pub fn new(
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        has_review_url: bool,
        title: impl Into<String>,
        description: impl Into<String>,
    ) -> Self {
        let mut panel = Self {
            worktree: worktree.into(),
            editor_name: editor_name.into(),
            has_review_url,
            title: title.into(),
            description: description.into(),
            active_view: ActiveView::Chat,
            selected: 0,
            should_quit: false,
            status_line: String::new(),
            merging: false,
            spinner: 0,
            hit_targets: Vec::new(),
            more_hit: None,
        };
        // Focus the Chat switch initially — it's the default middle-pane view
        // (the mockup highlights it). `rows()` is stable (the task-info block is
        // not a nav row), so this index holds for the panel's lifetime.
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Chat)))
            .unwrap_or(0);
        panel
    }

    /// The keyboard-navigable rows, top to bottom: back button, worktree
    /// folder, the view switches, then Approve / Reject. Stable regardless of
    /// the description's wrapped height (the task-info block is drawn outside
    /// this list), so [`selected`](Self::selected) never drifts between frames.
    fn rows(&self) -> Vec<PanelRow> {
        let mut rows = vec![
            PanelRow::Back,
            PanelRow::Folder,
            PanelRow::Switch(SwitchItem::Chat),
            PanelRow::Switch(SwitchItem::Diff),
            PanelRow::Switch(SwitchItem::Vim),
        ];
        if self.has_review_url {
            rows.push(PanelRow::Switch(SwitchItem::Browser));
        }
        rows.push(PanelRow::Approve);
        rows.push(PanelRow::Reject);
        rows
    }

    /// Start index and count of the contiguous `Switch` run in [`rows`]. The
    /// switches render as a full-width nav block; everything else is a plain
    /// one-line row, so this span is all the renderer and the click map need to
    /// agree on where the nav block sits.
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

    pub fn nav_up(&mut self) {
        self.step(-1);
    }

    pub fn nav_down(&mut self) {
        self.step(1);
    }

    fn step(&mut self, delta: i32) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        let idx = self.selected.min(n - 1);
        self.selected = if delta < 0 {
            if idx == 0 {
                n - 1
            } else {
                idx - 1
            }
        } else {
            (idx + 1) % n
        };
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
            // Decline Approve / Reject while the gated merge is already in
            // flight — the host loop runs it off-thread and sets `merging`, so a
            // replayed or double press (Enter *or* click) is dropped, never
            // queued behind the running merge.
            PanelRow::Approve if self.merging => PanelEffect::None,
            PanelRow::Approve => PanelEffect::Approve,
            PanelRow::Reject if self.merging => PanelEffect::None,
            PanelRow::Reject => PanelEffect::RejectPrompt,
        }
    }

    /// Ask to open the task-description popover (the `More` link / `m` key).
    /// Returns [`PanelEffect::None`] when the task has no body — a bodyless task
    /// shows just its title and no `More`, so there is nothing to open.
    pub fn request_description(&self) -> PanelEffect {
        if self.description.trim().is_empty() {
            PanelEffect::None
        } else {
            PanelEffect::ShowDescription
        }
    }

    /// Map a click at (`column`, `row`) to an effect. The `More` link is checked
    /// first, then the selectable rows' recorded rects. Returns `None` when the
    /// click misses everything.
    pub fn click(&mut self, column: u16, row: u16) -> PanelEffect {
        if let Some(r) = self.more_hit {
            if in_rect(r, column, row) {
                return self.request_description();
            }
        }
        let rows = self.rows();
        // `hit_targets` is small (≤ 8); cloning sidesteps the borrow conflict
        // between iterating it and mutating `self` in `activate_row`.
        for (rect, idx) in self.hit_targets.clone() {
            if in_rect(rect, column, row) {
                if let Some(r) = rows.get(idx) {
                    self.selected = idx;
                    return self.activate_row(r.clone());
                }
            }
        }
        PanelEffect::None
    }

    /// Handle a q / Esc quit request. While a gated merge is in flight, quitting
    /// would tear the review window down and exit the process with the merge
    /// worker's `gh` children still mid-flight, orphaning a half-finished merge.
    /// So while `merging` the quit is ignored and a short note explains why.
    pub fn request_quit(&mut self) {
        if self.merging {
            self.status_line = "merge in progress, wait for it to finish".into();
        } else {
            self.should_quit = true;
        }
    }
}

/// Display column width of `s`, honoring wide glyphs.
fn display_width(s: &str) -> usize {
    UnicodeWidthStr::width(s)
}

/// Whether (`x`, `y`) falls inside `r`.
fn in_rect(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0
        && r.height > 0
        && x >= r.x
        && x < r.x.saturating_add(r.width)
        && y >= r.y
        && y < r.y.saturating_add(r.height)
}

/// Left-truncate `path` to at most `width` columns, prefixing `...` when it's
/// clipped, so the tail (the folder name the reviewer cares about) always stays
/// visible: `/a/b/c/.shelbi/wt/review` → `...ct/.shelbi/wt/review`.
pub fn truncate_left(path: &str, width: usize) -> String {
    let chars: Vec<char> = path.chars().collect();
    if chars.len() <= width {
        return path.to_string();
    }
    if width <= 3 {
        return chars[chars.len().saturating_sub(width)..].iter().collect();
    }
    let keep = width - 3;
    let tail: String = chars[chars.len() - keep..].iter().collect();
    format!("...{tail}")
}

/// Right-truncate `s` to at most `width` columns, appending `…` when clipped.
/// Used for the task title, which the design keeps to one line.
fn truncate_right(s: &str, width: usize) -> String {
    if display_width(s) <= width {
        return s.to_string();
    }
    if width == 0 {
        return String::new();
    }
    // Reserve one column for the ellipsis.
    let budget = width.saturating_sub(1);
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > budget {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// The folder emoji + space that prefixes the worktree row. Named so the budget
/// math and the rendered line can never drift.
const FOLDER_PREFIX: &str = "📁 ";

/// Compose the worktree folder row — `"📁 <path>"` — fitting the whole line
/// within `width` columns. The prefix's display width is reserved *before*
/// left-truncating the path so the composed line never overruns the row.
fn folder_row_text(worktree: &str, width: usize) -> String {
    let budget = width.saturating_sub(display_width(FOLDER_PREFIX)).max(1);
    let label = truncate_left(worktree, budget);
    format!("{FOLDER_PREFIX}{label}")
}

/// Outer left padding (in columns) for the header block, task info, worktree,
/// and actions — 2 cols per the Figma. The nav switches keep the sidebar's
/// narrower 1-col inset instead (see [`switch_nav_line`]).
const PAD: u16 = 2;

/// The square back button occupies three rendered lines: a lower-half-block
/// bleed row, the button (arrow) row, and an upper-half-block bleed row.
const BACK_BLOCK_H: u16 = 3;
/// The button (arrow) + status sit on the middle line of the three-line block.
/// Only the render tests anchor to it; the renderer builds the three lines in
/// order, so it carries no runtime use.
#[cfg(test)]
const BACK_BUTTON_LINE: u16 = 1;
/// Column width of the back button. A cell is ~twice as tall as it is wide and
/// the half-block bleed makes the button ~2 cells tall, so a handful of columns
/// reads as roughly square; the arrow is centered within it.
const BACK_BTN_WIDTH: usize = 5;

/// Most lines the task description preview may claim.
const MAX_DESC_LINES: usize = 3;

/// Max rows the bottom status/error line may claim.
const STATUS_MAX_H: u16 = 6;

/// 1-col horizontal inset the bottom status line renders into.
const STATUS_INDENT: ratatui::layout::Margin = ratatui::layout::Margin {
    horizontal: 1,
    vertical: 0,
};

/// Carve the bottom rows out of `area` for the status line when one is set.
fn reserve_status_area(app: &ReviewPanel, area: Rect) -> (Rect, Option<Rect>) {
    if app.status_line.is_empty() || area.height < 2 {
        return (area, None);
    }
    let inner_w = area.width.saturating_sub(2).max(1) as usize;
    let needed = wrapped_line_count(&app.status_line, inner_w) as u16;
    let cap = STATUS_MAX_H.min(area.height - 1);
    let h = needed.clamp(1, cap);
    let body = Rect {
        height: area.height - h,
        ..area
    };
    let status = Rect {
        y: area.y + area.height - h,
        height: h,
        ..area
    };
    (body, Some(status))
}

/// Render the panel's status line — a non-empty `status_line` (an Approve /
/// merge failure, an opener error) painted red and word-wrapped.
fn render_status_line(f: &mut Frame, app: &ReviewPanel, area: Rect) {
    if area.width == 0 || area.height == 0 || app.status_line.is_empty() {
        return;
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            app.status_line.clone(),
            Style::default().fg(Color::Red),
        )))
        .wrap(ratatui::widgets::Wrap { trim: true }),
        area.inner(STATUS_INDENT),
    );
}

/// Estimate how many rows `text` occupies when word-wrapped to `width` columns,
/// matching ratatui's `Wrap { trim: true }` closely enough to size the status
/// area.
fn wrapped_line_count(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut lines = 1usize;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let w = display_width(word);
        if w > width {
            if col > 0 {
                lines += 1;
            }
            lines += (w - 1) / width;
            col = w % width;
            if col == 0 {
                col = width;
            }
            continue;
        }
        let need = if col == 0 { w } else { col + 1 + w };
        if need > width {
            lines += 1;
            col = w;
        } else {
            col = need;
        }
    }
    lines.max(1)
}

pub fn render_full(f: &mut Frame, app: &mut ReviewPanel, area: Rect) {
    // A non-empty `status_line` claims the bottom rows as a red, wrapped
    // warning. Carve it off first so the body's top-anchored layout is
    // unaffected and a too-small early return still leaves the warning painted.
    let (area, status_area) = reserve_status_area(app, area);
    if let Some(status_area) = status_area {
        render_status_line(f, app, status_area);
    }

    // Reset the click map; each rendered region re-populates it.
    app.hit_targets.clear();
    app.more_hit = None;
    if area.width == 0 || area.height == 0 {
        return;
    }

    // A cursor walking down the panel, clamped to the available height. Each
    // region renders only if it fits, so a very short panel degrades gracefully.
    let bottom = area.y + area.height;
    let mut y = area.y;

    // 1. The square back button block + the review status beside it.
    let back_h = BACK_BLOCK_H.min(area.height);
    render_back_button(
        f,
        app,
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: back_h,
        },
    );
    // The whole button column (all three lines) activates Back.
    let btn_w = (BACK_BTN_WIDTH as u16).min(area.width.saturating_sub(PAD)).max(1);
    push_hit(
        app,
        Rect {
            x: area.x + PAD,
            y,
            width: btn_w,
            height: back_h,
        },
        0,
    );
    y += back_h;

    // blank
    y = advance_blank(y, bottom);

    // 2. Task info (title + description preview + More).
    if y < bottom {
        let used = render_task_info(
            f,
            app,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: bottom - y,
            },
        );
        y += used;
    }

    // blank
    y = advance_blank(y, bottom);

    // 3. Worktree folder row.
    let folder_idx = app
        .rows()
        .iter()
        .position(|r| matches!(r, PanelRow::Folder))
        .unwrap_or(0);
    if y < bottom {
        let line = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        render_folder(f, app, line, app.selected == folder_idx);
        push_hit(
            app,
            Rect {
                x: area.x + PAD,
                y,
                width: area.width.saturating_sub(PAD),
                height: 1,
            },
            folder_idx,
        );
        y += 1;
    }

    // blank
    y = advance_blank(y, bottom);

    // 4. The view-switcher nav block.
    let (sstart, scount) = app.switch_span();
    let nav_h = (crate::sidebar::nav_lines(scount) as u16).min(bottom.saturating_sub(y));
    if y < bottom && nav_h > 0 {
        render_switch_nav(
            f,
            app,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: nav_h,
            },
            sstart,
            scount,
        );
        y += nav_h;
    }

    // blank
    y = advance_blank(y, bottom);

    // 5. The Approve / Reject action row (one line).
    if y < bottom {
        render_actions(
            f,
            app,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// Advance the layout cursor one blank line, never past `bottom`.
fn advance_blank(y: u16, bottom: u16) -> u16 {
    (y + 1).min(bottom)
}

/// Record a selectable row's screen rect + its [`rows`](ReviewPanel::rows)
/// index for the click map.
fn push_hit(app: &mut ReviewPanel, rect: Rect, idx: usize) {
    app.hit_targets.push((rect, idx));
}

/// Render the square back button block and the review status beside it. The
/// bleed rows carry the button's fill colour in their *foreground* so the
/// single arrow row reads as a square (the sidebar nav's half-block trick). The
/// status (`Ready for review`) sits on the button's own row, two columns to its
/// right, in the accent cyan.
fn render_back_button(f: &mut Frame, app: &ReviewPanel, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let selected = app.selected == 0; // Back is always rows[0].
    let bg = SELECTION_BG;
    let avail = area.width.saturating_sub(PAD) as usize;
    let btn_w = BACK_BTN_WIDTH.min(avail.max(1)).max(1);
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
    let pad = " ".repeat(PAD as usize);
    let mut lines = vec![Line::from(vec![
        Span::raw(pad.clone()),
        Span::styled(crate::sidebar::BLEED_ABOVE.repeat(btn_w), bleed_style),
    ])];
    // The button row carries the status label two columns to the right of the
    // button.
    lines.push(Line::from(vec![
        Span::raw(pad.clone()),
        Span::styled(btn_text, arrow_style),
        Span::raw("  "),
        Span::styled(
            "Ready for review",
            Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw(pad),
        Span::styled(crate::sidebar::BLEED_BELOW.repeat(btn_w), bleed_style),
    ]));
    f.render_widget(Paragraph::new(lines), area);
}

/// Render the task-info block (title, description preview, `More`) into `area`,
/// returning how many lines it used so the caller can advance past it. Records
/// [`ReviewPanel::more_hit`] when the `More` link is drawn.
fn render_task_info(f: &mut Frame, app: &mut ReviewPanel, area: Rect) -> u16 {
    if area.width == 0 || area.height == 0 {
        return 0;
    }
    let inner_x = area.x + PAD;
    let width = area.width.saturating_sub(PAD) as usize;
    if width == 0 {
        return 0;
    }
    let mut lines: Vec<Line> = Vec::new();
    let mut used: u16 = 0;

    // Title (bold white, one line, right-truncated with …).
    if !app.title.is_empty() && used < area.height {
        lines.push(Line::from(Span::styled(
            truncate_right(&app.title, width),
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        )));
        used += 1;
    }

    // Description preview (#c6c6c6, up to MAX_DESC_LINES, ending in `...` when
    // clipped). A bodyless task shows no preview and no More.
    let desc_lines = if app.description.trim().is_empty() {
        Vec::new()
    } else {
        description_preview(&app.description, width, MAX_DESC_LINES)
    };
    for dl in &desc_lines {
        if used >= area.height {
            break;
        }
        lines.push(Line::from(Span::styled(
            dl.clone(),
            Style::default().fg(PALETTE_FG),
        )));
        used += 1;
    }

    // More link (cyan), only when a description was shown.
    if !desc_lines.is_empty() && used < area.height {
        lines.push(Line::from(Span::styled(
            "More",
            Style::default().fg(ACCENT_CYAN),
        )));
        app.more_hit = Some(Rect {
            x: inner_x,
            y: area.y + used,
            width: 4, // "More"
            height: 1,
        });
        used += 1;
    }

    f.render_widget(
        Paragraph::new(lines),
        Rect {
            x: inner_x,
            y: area.y,
            width: width as u16,
            height: used,
        },
    );
    used
}

/// Render the worktree folder row: `📁 <path>`, left-truncated. `#bababa` when
/// unselected, white/bold when selected.
fn render_folder(f: &mut Frame, app: &ReviewPanel, area: Rect, selected: bool) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let width = area.width.saturating_sub(PAD) as usize;
    let style = if selected {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(FG_SECONDARY)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            folder_row_text(&app.worktree, width),
            style,
        ))),
        Rect {
            x: area.x + PAD,
            y: area.y,
            width: width as u16,
            height: 1,
        },
    );
}

/// Render the Chat / Diff / Edit / Browser switches as a full-width nav block,
/// mirroring the main sidebar's nav: a separator line between (and bracketing)
/// each item, the selected item's fill spanning edge to edge with its adjacent
/// separators carrying the half-block bleed. Records each item's screen rect.
fn render_switch_nav(
    f: &mut Frame,
    app: &mut ReviewPanel,
    area: Rect,
    sstart: usize,
    scount: usize,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = app.rows();
    let selected =
        (app.selected >= sstart && app.selected < sstart + scount).then(|| app.selected - sstart);
    let width = area.width as usize;
    let bleed = SELECTION_BG;

    let mut lines: Vec<Line> = Vec::with_capacity(crate::sidebar::nav_lines(scount));
    // The item line for switch `p` sits at nav-area offset `2p + 1`.
    for p in 0..=scount {
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
            let item_y = area.y + (2 * p as u16 + 1);
            if item_y < area.y + area.height {
                push_hit(
                    app,
                    Rect {
                        x: area.x,
                        y: item_y,
                        width: area.width,
                        height: 1,
                    },
                    sstart + p,
                );
            }
        }
    }
    f.render_widget(Paragraph::new(lines), area);
}

/// One switch nav row. Selected rows fill edge to edge with the selection
/// background and render white/bold; the active middle-pane view is cyan/bold
/// when unselected; other rows are `#bababa`. The leading space keeps the label
/// aligned with the sidebar nav's 1-col inset.
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
            Style::default().fg(ACCENT_CYAN).add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(text, Style::default().fg(FG_SECONDARY)))
    }
}

/// Render the Approve / Reject action row: `✅ Approve` (green) and `❌ Reject`
/// (red) on one line, no brackets, with up to [`ACTIONS_GAP`] columns between
/// them (squeezed down to 1 col at the minimum width so nothing overlaps). The
/// selected action reads as pressed (reverse-video its tint). While a merge is
/// in flight the row becomes a busy spinner instead. Records
/// [`ReviewPanel::hit_targets`] for each word.
fn render_actions(f: &mut Frame, app: &mut ReviewPanel, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let x0 = area.x + PAD;
    let width = area.width.saturating_sub(PAD);
    if width == 0 {
        return;
    }

    if app.merging {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("{} merging PR…", spinner_frame(app.spinner)),
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
            ))),
            Rect {
                x: x0,
                y: area.y,
                width,
                height: 1,
            },
        );
        return;
    }

    let rows = app.rows();
    let approve_idx = rows.iter().position(|r| matches!(r, PanelRow::Approve));
    let reject_idx = rows.iter().position(|r| matches!(r, PanelRow::Reject));
    let approve_sel = approve_idx == Some(app.selected);
    let reject_sel = reject_idx == Some(app.selected);

    let approve_label = "✅ Approve";
    let reject_label = "❌ Reject";
    let aw = display_width(approve_label) as u16;
    let rw = display_width(reject_label) as u16;

    // Fit the two labels + a gap within the available width, shrinking the gap
    // (never below 1) before anything clips.
    const ACTIONS_GAP: u16 = 4;
    let slack = width.saturating_sub(aw + rw);
    let gap = ACTIONS_GAP.min(slack).max(1);

    let approve_style = action_style(PALETTE_GREEN, approve_sel);
    let reject_style = action_style(ACTION_RED, reject_sel);

    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(approve_label, approve_style),
            Span::raw(" ".repeat(gap as usize)),
            Span::styled(reject_label, reject_style),
        ])),
        Rect {
            x: x0,
            y: area.y,
            width,
            height: 1,
        },
    );

    // Record hit rects for each word (clamped to the row width).
    if let Some(idx) = approve_idx {
        push_hit(
            app,
            Rect {
                x: x0,
                y: area.y,
                width: aw.min(width),
                height: 1,
            },
            idx,
        );
    }
    if let Some(idx) = reject_idx {
        let rx = x0 + aw + gap;
        if rx < x0 + width {
            push_hit(
                app,
                Rect {
                    x: rx,
                    y: area.y,
                    width: rw.min(x0 + width - rx),
                    height: 1,
                },
                idx,
            );
        }
    }
}

/// The style for an action word: flat tint when idle, reverse-video (pressed)
/// when it is the selected row.
fn action_style(tint: Color, selected: bool) -> Style {
    if selected {
        Style::default()
            .fg(tint)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(tint)
    }
}

/// One frame of the braille "merging…" spinner, indexed by the panel's
/// `spinner` tick.
fn spinner_frame(tick: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[tick % FRAMES.len()]
}

// ---------------------------------------------------------------------------
// Description preview (markdown stripped, wrapped, clipped) — pure, unit-tested.

/// Build the task-description preview: strip block markdown (headings, list
/// markers, blockquotes, inline code backticks), flatten to a single
/// whitespace-separated stream, greedily wrap to `width` columns, and keep at
/// most `max_lines` lines — appending `...` to the last kept line when the body
/// runs past the preview. Returns an empty vec for an empty body or zero width.
fn description_preview(body: &str, width: usize, max_lines: usize) -> Vec<String> {
    if width == 0 || max_lines == 0 {
        return Vec::new();
    }
    let mut words: Vec<String> = Vec::new();
    for raw in body.lines() {
        for w in strip_block_markers(raw).split_whitespace() {
            words.push(w.to_string());
        }
    }
    if words.is_empty() {
        return Vec::new();
    }

    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut idx = 0;
    while idx < words.len() {
        let w = &words[idx];
        let need = if cur.is_empty() {
            display_width(w)
        } else {
            display_width(&cur) + 1 + display_width(w)
        };
        if need <= width {
            if !cur.is_empty() {
                cur.push(' ');
            }
            cur.push_str(w);
            idx += 1;
        } else if cur.is_empty() {
            // A single word wider than the row: hard-truncate it onto this line.
            cur = truncate_to_width(w, width);
            idx += 1;
        } else {
            lines.push(std::mem::take(&mut cur));
            if lines.len() == max_lines {
                break;
            }
        }
    }
    if !cur.is_empty() && lines.len() < max_lines {
        lines.push(std::mem::take(&mut cur));
    }

    // Anything left over means the preview was clipped — mark the last line.
    if idx < words.len() {
        if let Some(last) = lines.last_mut() {
            append_ellipsis(last, width);
        }
    }
    lines
}

/// Strip the leading block-level markdown markers from one source line and drop
/// inline-code backticks. Blockquote `>` markers are removed; list bullets
/// (`-`/`*`/`+`/`N.`/`N)`) are removed but their text is kept; an ATX heading
/// line (`## Summary`) is dropped *whole* — its text is a section label, not
/// prose, so the preview flows the body beneath it (matching the Figma, whose
/// preview starts on the Summary paragraph, not the word "Summary"). Emphasis
/// markers (`*`, `_`) are left alone so identifiers like `snake_case` survive.
fn strip_block_markers(line: &str) -> String {
    let mut t = line.trim();
    while let Some(rest) = t.strip_prefix('>') {
        t = rest.trim_start();
    }
    if t.starts_with('#') {
        return String::new();
    }
    t = strip_list_marker(t);
    t.replace('`', "")
}

/// Strip a single leading list marker (`- `, `* `, `+ `, or an ordered
/// `N.`/`N)` followed by a space) from `s`.
fn strip_list_marker(s: &str) -> &str {
    let t = s.trim_start();
    for m in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(m) {
            return rest.trim_start();
        }
    }
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    if i > 0 && i < bytes.len() && (bytes[i] == b'.' || bytes[i] == b')') {
        if let Some(rest) = t[i + 1..].strip_prefix(' ') {
            return rest.trim_start();
        }
    }
    t
}

/// Truncate `s` to at most `width` columns (no ellipsis) — used when a single
/// word is wider than the whole row.
fn truncate_to_width(s: &str, width: usize) -> String {
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = UnicodeWidthChar::width(c).unwrap_or(0);
        if w + cw > width {
            break;
        }
        out.push(c);
        w += cw;
    }
    out
}

/// Trim `line` as needed and append `...` so the result fits within `width`.
fn append_ellipsis(line: &mut String, width: usize) {
    const ELL: &str = "...";
    if width < ELL.len() {
        return;
    }
    while display_width(line) + ELL.len() > width {
        if line.pop().is_none() {
            break;
        }
    }
    while line.ends_with(' ') {
        line.pop();
    }
    line.push_str(ELL);
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
pub(crate) fn spawn_opener(program: &str, args: &[String]) -> std::result::Result<(), String> {
    std::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("{program} failed: {e}"))
}

// ---------------------------------------------------------------------------

/// Resolve the concrete review URL to open for `task_id` (with `$PORT` /
/// `$SLOT` substituted), or `None` when none is configured.
pub(crate) fn review_url(project_name: &str, task_id: &str) -> Option<String> {
    let project = shelbi_state::load_project(project_name).ok()?;
    let store = shelbi_state::issue_store_for_project(&project).ok()?;
    let tf = store.get(task_id).ok().flatten()?;
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

    const BODY: &str = "## Summary\n\nWarm the application cache during startup so the first request can reuse the same data as subsequent requests.\n\n## Acceptance criteria\n\n- Initialize the cache once before accepting traffic.";

    fn panel(has_url: bool) -> ReviewPanel {
        ReviewPanel::new(
            "/Users/j/proj/.shelbi/wt/review",
            "Vim".to_string(),
            has_url,
            "Cold-start cache",
            BODY,
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
        assert_eq!(truncate_left("/a/b/c/.shelbi/wt/review", 12), "...wt/review");
        assert_eq!(truncate_left("/a/b/c/.shelbi/wt/review", 12).chars().count(), 12);
        assert_eq!(truncate_left("short", 20), "short");
        assert!(truncate_left("/very/long/path/here", 12).starts_with("..."));
        assert!(truncate_left("/very/long/path/here", 12).ends_with("path/here"));
    }

    #[test]
    fn folder_row_reserves_the_emoji_prefix_before_truncating() {
        let worktree = "/Users/jlong/Workspaces/32pixels/ContextStore/.shelbi/wt/review";
        for width in [16_usize, 20, 24, 30, 40] {
            let line = folder_row_text(worktree, width);
            assert!(
                display_width(&line) <= width,
                "line {line:?} (w={}) overruns row width {width}",
                display_width(&line),
            );
            assert!(
                line.ends_with("wt/review"),
                "line {line:?} lost the trailing segment at width {width}",
            );
            assert!(line.starts_with(FOLDER_PREFIX));
        }
        let tight = folder_row_text(worktree, 18);
        assert!(
            tight.contains("..."),
            "expected front elision at a narrow width, got {tight:?}",
        );
    }

    #[test]
    fn renders_header_task_info_switcher_and_actions() {
        let mut app = panel(true);
        let out = render(&mut app, 44, 24);
        assert!(out.contains("Ready for review"), "status header: {out}");
        assert!(out.contains("Cold-start cache"), "task title: {out}");
        assert!(out.contains("Warm the application cache"), "description preview: {out}");
        assert!(out.contains("More"), "More link: {out}");
        assert!(out.contains("wt/review"), "worktree folder shown: {out}");
        assert!(out.contains("Chat with Reviewer"), "chat switch: {out}");
        assert!(out.contains("View Diff"), "diff switch: {out}");
        assert!(out.contains("Edit in Vim"), "editor switch reflects name: {out}");
        assert!(out.contains("Open Browser"), "browser switch when url set: {out}");
        assert!(out.contains("Approve"), "approve button: {out}");
        assert!(out.contains("Reject"), "reject button: {out}");
        // The actions row carries no brackets.
        assert!(!out.contains("[ "), "no bracketed buttons: {out}");
    }

    /// The status sits on the back button's own row (to its right), not below
    /// it — the top row reads `[←]  Ready for review`.
    #[test]
    fn status_renders_on_the_back_button_row() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        assert!(
            rows[BACK_BUTTON_LINE as usize].contains("Ready for review"),
            "status must share the button row: {:?}",
            rows[BACK_BUTTON_LINE as usize]
        );
        assert!(
            rows[BACK_BUTTON_LINE as usize].contains('\u{2190}'),
            "back-arrow glyph on the button row: {:?}",
            rows[BACK_BUTTON_LINE as usize]
        );
    }

    /// The status header renders in the accent cyan.
    #[test]
    fn status_header_is_accent_cyan() {
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let y = BACK_BUTTON_LINE;
        let x = (0..buf.area.width)
            .find(|&x| buf[(x, y)].symbol() == "R" && buf[(x, y)].fg == ACCENT_CYAN)
            .expect("the 'Ready for review' R should be accent cyan");
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD), "status is bold");
    }

    /// The back button's fill reads as a square: the arrow row carries the
    /// selection background across the button's cells (starting at the 2-col
    /// pad), and the bleed rows carry that same colour in their foreground.
    #[test]
    fn back_button_fill_reads_as_a_square() {
        let mut term = Terminal::new(TestBackend::new(30, 24)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let x = PAD; // button starts at the 2-col pad
        assert_eq!(
            buf[(x, BACK_BUTTON_LINE)].bg,
            SELECTION_BG,
            "arrow row carries the button fill"
        );
        assert_eq!(
            buf[(x, BACK_BUTTON_LINE - 1)].fg,
            SELECTION_BG,
            "bleed above carries the fill colour so it reads square"
        );
        assert_eq!(
            buf[(x, BACK_BUTTON_LINE + 1)].fg,
            SELECTION_BG,
            "bleed below carries the fill colour so it reads square"
        );
    }

    /// Activating the back button (Enter or click) focuses the dashboard.
    #[test]
    fn activating_back_button_focuses_the_dashboard() {
        let mut app = panel(true);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Back)).unwrap();
        assert_eq!(idx, 0, "back button leads the panel");
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::FocusDashboard);
        // A click on the button block maps to the same effect.
        let _ = render(&mut app, 30, 24);
        assert_eq!(app.click(PAD, BACK_BUTTON_LINE), PanelEffect::FocusDashboard);
    }

    #[test]
    fn failed_approve_status_line_renders_red_below_the_body() {
        let mut app = panel(true);
        app.status_line =
            "approve failed: PR #9 is not mergeable: the merge commit cannot be cleanly created"
                .to_string();
        let mut term = Terminal::new(TestBackend::new(40, 28)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect();
        let out = rows.join("\n");
        assert!(out.contains("Ready for review"), "header stays: {out}");
        for frag in ["approve", "mergeable", "cleanly", "created"] {
            assert!(out.contains(frag), "status fragment {frag:?} missing:\n{out}");
        }
        let y = rows.iter().position(|r| r.contains("cleanly")).expect("status row present");
        let red = (0..buf.area.width).any(|x| {
            let c = &buf[(x, y as u16)];
            c.fg == Color::Red && !c.symbol().trim().is_empty()
        });
        assert!(red, "status row should be red:\n{out}");
    }

    #[test]
    fn cleared_status_line_leaves_no_warning() {
        let mut app = panel(true);
        assert!(app.status_line.is_empty());
        let out = render(&mut app, 40, 24);
        assert!(out.contains("Ready for review"));
        assert!(!out.contains("failed"), "no stale warning: {out}");
    }

    #[test]
    fn wrapped_line_count_matches_greedy_word_wrap() {
        assert_eq!(wrapped_line_count("", 10), 1);
        assert_eq!(wrapped_line_count("short", 10), 1);
        assert_eq!(wrapped_line_count("hello world", 8), 2);
        assert_eq!(wrapped_line_count("abcdefghij", 4), 3);
    }

    // -- task info / description preview ------------------------------------

    #[test]
    fn description_preview_strips_markdown_wraps_and_clips_to_three_lines() {
        let out = description_preview(BODY, 40, 3);
        assert_eq!(out.len(), 3, "preview is at most three lines, got {out:?}");
        // The leading `## Summary` heading is stripped — the preview starts on
        // the Summary prose.
        assert!(out[0].starts_with("Warm the application cache"), "stripped heading: {out:?}");
        assert!(!out.join(" ").contains('#'), "no heading markers survive: {out:?}");
        // Clipped, so the last line ends in an ellipsis.
        assert!(out[2].ends_with("..."), "clipped preview ends in ...: {out:?}");
        for line in &out {
            assert!(display_width(line) <= 40, "line {line:?} fits the width");
        }
    }

    #[test]
    fn description_preview_of_a_short_body_is_not_ellipsized() {
        let out = description_preview("A tiny note.", 40, 3);
        assert_eq!(out, vec!["A tiny note.".to_string()]);
    }

    #[test]
    fn description_preview_strips_list_markers() {
        let out = description_preview("- first item\n- second item", 40, 3);
        assert_eq!(out, vec!["first item second item".to_string()]);
    }

    /// A task with an empty body shows just the title — no preview, no More.
    #[test]
    fn empty_body_shows_title_without_more() {
        let mut app = ReviewPanel::new("/wt", "Vim", true, "Just a title", "");
        let out = render(&mut app, 40, 24);
        assert!(out.contains("Just a title"), "title still shows: {out}");
        assert!(!out.contains("More"), "no More link without a body: {out}");
        // request_description is inert for a bodyless task.
        assert_eq!(app.request_description(), PanelEffect::None);
    }

    /// The More link / `m` key opens the description popover when the task has a
    /// body.
    #[test]
    fn more_requests_the_description_popover() {
        let mut app = panel(true);
        assert_eq!(app.request_description(), PanelEffect::ShowDescription);
        // A click on the rendered More link routes to the same effect.
        let rows = render_lines(&mut app, 44, 24);
        let more_y = row_y(&rows, "More") as u16;
        let more_x = rows[more_y as usize].find("More").unwrap() as u16;
        assert_eq!(app.click(more_x, more_y), PanelEffect::ShowDescription);
    }

    #[test]
    fn description_body_is_cyan_more_link() {
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let more_y = row_y(&rows, "More") as u16;
        let more_x = rows[more_y as usize].find("More").unwrap() as u16;
        assert_eq!(buf[(more_x, more_y)].fg, ACCENT_CYAN, "More is accent cyan");
    }

    // -- switches ----------------------------------------------------------

    #[test]
    fn selected_switch_renders_full_width_half_block_bleed() {
        let width = 30u16;
        let mut app = panel(true);
        // Default selection is Chat; step to the Edit switch.
        app.nav_down();
        app.nav_down();
        let rows = render_lines(&mut app, width, 24);
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
        let browser_y = row_y(&rows, "Open Browser");
        assert!(
            rows[browser_y + 1].trim().is_empty(),
            "the line below the unselected Browser switch stays blank, got: {:?}",
            rows[browser_y + 1]
        );
    }

    #[test]
    fn switch_separators_keep_labels_from_shifting_across_selection() {
        let mut chat = panel(true);
        let chat_rows = render_lines(&mut chat, 30, 24);
        let mut moved = panel(true);
        moved.nav_down();
        let moved_rows = render_lines(&mut moved, 30, 24);
        for label in ["Chat with Reviewer", "View Diff", "Edit in Vim", "Open Browser"] {
            assert_eq!(
                row_y(&chat_rows, label),
                row_y(&moved_rows, label),
                "'{label}' must not move when the selection changes"
            );
        }
        assert_eq!(
            row_y(&chat_rows, "View Diff") - row_y(&chat_rows, "Chat with Reviewer"),
            2,
            "one separator line always sits between Chat and View Diff"
        );
    }

    #[test]
    fn selected_switch_fill_spans_full_width() {
        let width = 30u16;
        let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
        let mut app = panel(true); // Chat focused by default
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let chat_y = row_y(&rows, "Chat with Reviewer") as u16;
        assert_eq!(buf[(0, chat_y)].bg, SELECTION_BG, "left edge carries the fill");
        for x in (width - 4)..width {
            assert_eq!(buf[(x, chat_y)].bg, SELECTION_BG, "right edge padding carries the fill, col {x}");
        }
        assert_eq!(buf[(0, chat_y)].symbol(), " ", "column 0 is the indent gutter");
    }

    #[test]
    fn browser_entry_hidden_without_review_url() {
        let mut app = panel(false);
        let out = render(&mut app, 30, 24);
        assert!(!out.contains("Open Browser"), "no browser action when no url: {out}");
        assert!(out.contains("Chat with Reviewer") && out.contains("Approve"));
    }

    #[test]
    fn editor_label_tracks_resolved_editor_name() {
        let mut app = ReviewPanel::new("/wt", "Helix".to_string(), false, "T", "b");
        let out = render(&mut app, 30, 24);
        assert!(out.contains("Edit in Helix"), "label uses resolved editor: {out}");
    }

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
        assert_eq!(app.active_view, ActiveView::Chat);
        select_switch(&mut app, SwitchItem::Vim);
        assert_eq!(app.activate(), PanelEffect::ShowVim);
        assert_eq!(app.active_view, ActiveView::Vim);
        select_switch(&mut app, SwitchItem::Chat);
        assert_eq!(app.activate(), PanelEffect::ShowChat);
        assert_eq!(app.active_view, ActiveView::Chat);
    }

    #[test]
    fn view_diff_renders_directly_below_chat() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        let chat_y = row_y(&rows, "Chat with Reviewer");
        let diff_y = row_y(&rows, "View Diff");
        assert_eq!(diff_y - chat_y, 2, "View Diff sits immediately below Chat");
        assert!(diff_y < row_y(&rows, "Edit in Vim"), "View Diff precedes Edit");
    }

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
            vec![SwitchItem::Chat, SwitchItem::Diff, SwitchItem::Vim, SwitchItem::Browser]
        );
    }

    #[test]
    fn activating_view_diff_returns_show_diff_and_marks_active() {
        let mut app = panel(true);
        select_switch(&mut app, SwitchItem::Diff);
        assert_eq!(app.activate(), PanelEffect::ShowDiff);
        assert_eq!(app.active_view, ActiveView::Diff);
    }

    #[test]
    fn clicking_view_diff_dispatches_show_diff() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        let diff_line = row_y(&rows, "View Diff") as u16;
        let effect = app.click(4, diff_line);
        assert_eq!(effect, PanelEffect::ShowDiff);
        assert_eq!(app.active_view, ActiveView::Diff);
    }

    #[test]
    fn browser_activation_returns_open_browser_effect() {
        let mut app = panel(true);
        select_switch(&mut app, SwitchItem::Browser);
        assert_eq!(app.activate(), PanelEffect::OpenBrowser);
    }

    // -- folder / actions --------------------------------------------------

    #[test]
    fn folder_row_activation_reveals_the_worktree() {
        let mut app = panel(false);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Folder)).unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::RevealFolder);
    }

    #[test]
    fn approve_row_activation_returns_approve_effect() {
        let mut app = panel(false);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Approve)).unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::Approve);
    }

    #[test]
    fn reject_row_activation_requests_the_reason_popover() {
        let mut app = panel(false);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Reject)).unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::RejectPrompt);
    }

    /// Approve and Reject render on one line, with Reject to the right of
    /// Approve, and clicking each dispatches its effect.
    #[test]
    fn approve_and_reject_share_one_row_and_are_separately_clickable() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        let ay = row_y(&rows, "Approve");
        let ry = row_y(&rows, "Reject");
        assert_eq!(ay, ry, "Approve and Reject share one row");
        let line = &rows[ay];
        assert!(
            line.find("Approve").unwrap() < line.find("Reject").unwrap(),
            "Approve sits left of Reject: {line:?}"
        );
        let ax = line.find("Approve").unwrap() as u16;
        let rx = line.find("Reject").unwrap() as u16;
        assert_eq!(app.click(ax, ay as u16), PanelEffect::Approve);
        assert_eq!(app.click(rx, ry as u16), PanelEffect::RejectPrompt);
    }

    /// Selected Approve / Reject read as pressed (reverse-video their tint), and
    /// carry the design colours (#5acd25 / #e04f52).
    #[test]
    fn selected_action_renders_reverse_video_tint() {
        for (tint, needle, item) in [
            (PALETTE_GREEN, "Approve", PanelRow::Approve),
            (ACTION_RED, "Reject", PanelRow::Reject),
        ] {
            let width = 44u16;
            let mut app = panel(true);
            app.selected = app
                .rows()
                .iter()
                .position(|r| std::mem::discriminant(r) == std::mem::discriminant(&item))
                .unwrap();
            let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
            term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
            let buf = term.backend().buffer().clone();
            let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
            let y = row_y(&rows, needle) as u16;
            let col = rows[y as usize].find(needle).unwrap() as u16;
            let cell = &buf[(col, y)];
            assert!(
                cell.modifier.contains(Modifier::REVERSED),
                "{needle}: selected action must be reverse-video, got {:?}",
                cell.modifier
            );
            assert_eq!(cell.fg, tint, "{needle}: reverse fg carries the action tint");
        }
    }

    #[test]
    fn unselected_action_is_flat_tinted_without_reverse() {
        // Select something else (the folder) so neither action is selected.
        let mut app = panel(true);
        app.selected = app.rows().iter().position(|r| matches!(r, PanelRow::Folder)).unwrap();
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        for (tint, needle) in [(PALETTE_GREEN, "Approve"), (ACTION_RED, "Reject")] {
            let y = row_y(&rows, needle) as u16;
            let col = rows[y as usize].find(needle).unwrap() as u16;
            let cell = &buf[(col, y)];
            assert!(!cell.modifier.contains(Modifier::REVERSED), "{needle}: flat when unselected");
            assert_eq!(cell.fg, tint, "{needle}: carries the design tint");
        }
    }

    /// At the minimum sidebar width (24 cols) the actions still fit on one row
    /// with nothing overlapping, and the rest of the panel renders.
    #[test]
    fn renders_at_minimum_width() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 24, 30);
        let out = rows.join("\n");
        // At 24 cols the status shares the row with the back button, so the
        // 16-char "Ready for review" clips — but it still renders (truncated),
        // which is the AC's "text truncates, nothing overlaps".
        assert!(out.contains("Ready"), "status (possibly clipped) still renders: {out}");
        assert!(out.contains("Cold-start cache"), "title: {out}");
        assert!(out.contains("Chat with Reviewer"), "nav: {out}");
        // Approve and Reject both fit, on one row, no overlap.
        let ay = row_y(&rows, "Approve");
        assert_eq!(ay, row_y(&rows, "Reject"), "actions share a row even at 24 cols");
        let line = &rows[ay];
        // Both labels render on the one row, Approve before Reject. The gap
        // between them is squeezed but never below one column (see
        // `render_actions`), so the two never overlap — and rendering into a
        // 24-wide buffer can't overflow. One separating space proves the gap.
        assert!(
            line.contains("Approve ") && line.find("Approve").unwrap() < line.find("Reject").unwrap(),
            "Approve sits left of Reject with a gap at 24 cols: {line:?}"
        );
    }

    // -- merging -----------------------------------------------------------

    #[test]
    fn second_approve_while_merging_is_ignored() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Approve)).unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::Approve);
        app.merging = true;
        assert_eq!(app.activate(), PanelEffect::None, "Enter is dropped while merging");
        let approve_y = row_y(&rows, "Approve") as u16;
        let approve_x = rows[approve_y as usize].find("Approve").unwrap() as u16;
        // Re-render so the click map reflects the merging (busy) row; the
        // Approve word is gone, so a click there hits nothing.
        let _ = render(&mut app, 44, 24);
        assert_eq!(app.click(approve_x, approve_y), PanelEffect::None, "click dropped while merging");
    }

    #[test]
    fn reject_while_merging_is_ignored() {
        let mut app = panel(true);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Reject)).unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::RejectPrompt);
        app.merging = true;
        assert_eq!(app.activate(), PanelEffect::None, "Enter is dropped while merging");
    }

    #[test]
    fn quit_is_ignored_while_merging() {
        let mut app = panel(true);
        app.merging = true;
        app.request_quit();
        assert!(!app.should_quit, "q / Esc does not quit while merging");
        assert!(!app.status_line.is_empty(), "the status line explains why");
        app.merging = false;
        app.request_quit();
        assert!(app.should_quit, "q / Esc quits once the merge is done");
    }

    #[test]
    fn merging_replaces_actions_with_a_busy_indicator() {
        let mut app = panel(true);
        let idle = render(&mut app, 44, 24);
        assert!(idle.contains("Approve"), "idle panel shows the Approve action: {idle}");
        app.merging = true;
        let busy = render(&mut app, 44, 24);
        assert!(busy.contains("merging PR"), "busy indicator shown while merging: {busy}");
        assert!(!busy.contains("Approve"), "the Approve label is gone while merging: {busy}");
        assert!(busy.contains(spinner_frame(app.spinner)), "spinner frame rendered: {busy}");
    }

    #[test]
    fn spinner_frame_cycles_and_wraps() {
        assert_eq!(spinner_frame(0), '⠋');
        assert_ne!(spinner_frame(0), spinner_frame(1), "consecutive ticks differ");
        assert_eq!(spinner_frame(0), spinner_frame(10), "wraps after 10 frames");
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
    }
}
