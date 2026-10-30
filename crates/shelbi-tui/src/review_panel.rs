//! The **review panel** of the review interface — a dedicated ratatui view
//! hosted in the **left** column of the review window, with the review content
//! (agent / server / editor) filling the column on its right. It renders, for
//! the Ready-for-review task it was launched on:
//!
//! - a **square back button** at the very top (a back-arrow glyph, no label)
//!   that switches focus back to the dashboard window without tearing the
//!   still-loaded review down — see [`PanelEffect::Back`]. With the
//!   **review status** (`Ready for review`) in the accent cyan on the button's
//!   own row, two columns to its right,
//! - **task info**: the task title (bold white) and the first few lines of the
//!   task description (markdown stripped, wrapped, at most three lines, ending
//!   in `...` when clipped), followed by a cyan **`More`** link / `m` key that
//!   opens the full task-description popover — see [`PanelEffect::ShowDescription`],
//! - the review worktree's folder name (left-truncated to fit; click to reveal
//!   it in the OS file manager),
//! - a **view-switcher** action group — *Reviewer* (default) /
//!   *View Diff* / *Edit in <editor>* / *Open Browser* (the last only when the
//!   workflow declares a review URL), the active content view highlighted, and
//! - an **Approve** / **Reject** action row (no brackets, green / red in the
//!   design colors), Reject opening a type-the-reason popover.
//!
//! The header, task-info, worktree and nav-block rendering are the shared
//! [`crate::panel`] primitives, so this panel and the workspace panel
//! ([`crate::workspace_panel`]) draw the sidebar column identically.
//!
//! The state machine here is pure and side-effect free: [`ReviewPanel`]
//! methods return a [`PanelEffect`] describing what the host loop should do, so
//! the rendering and input logic are unit-testable with a `TestBackend`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::panel::{
    advance_blank, display_width, in_rect, render_back_button, render_folder, render_nav_block,
    render_status_line, render_task_info, reserve_status_area, NavEntry, BACK_BLOCK_H,
    BACK_BTN_WIDTH, PAD,
};
use crate::theme::palette;

// The panel's rows, effects and content views are the shared review model's
// (`shelbi_app::review`), so the TUI and the desktop navigate the panel by one
// table. `SwitchItem` keeps its local name for the view-switcher entries.
pub use shelbi_app::review::{ActiveView, PanelEffect, PanelRow};
pub use shelbi_app::view::ReviewSwitch as SwitchItem;

/// The review panel's full state.
pub struct ReviewPanel {
    /// Absolute path of the review worktree — shown truncated, revealed on
    /// click.
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …).
    pub editor_name: String,
    /// Which of Edit and View Diff open an app instead of a pane: those
    /// launch it ([`PanelEffect::OpenApp`]) and leave the view as it is.
    pub apps: shelbi_state::AppTools,
    /// Whether the workflow declares a review URL — gates the Browser entry.
    pub has_review_url: bool,
    /// The reviewed task's title — shown bold white in the task-info block.
    pub title: String,
    /// The reviewed task's markdown body.
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
    /// mouse handler to map a click back to a row.
    hit_targets: Vec<(Rect, usize)>,
    /// Screen rect of the `More` link, when rendered (task has a body).
    more_hit: Option<Rect>,
    /// The slot's server, live from the task's sidebar row: its sub-state and
    /// `machine:port`. `None` until the shell has a row for the task. The
    /// desktop's Review view shows the same (`render_server`).
    pub server: Option<(shelbi_app::view::ReviewState, Option<String>)>,
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
            apps: shelbi_state::AppTools::default(),
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
            server: None,
        };
        // Focus the Chat switch initially — it's the default middle-pane view.
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Chat)))
            .unwrap_or(0);
        panel
    }

    /// The keyboard-navigable rows, top to bottom. Stable regardless of the
    /// description's wrapped height (the task-info block is drawn outside this
    /// list).
    fn rows(&self) -> Vec<PanelRow> {
        let mut switches = vec![SwitchItem::Chat, SwitchItem::Diff, SwitchItem::Editor];
        if self.has_review_url {
            switches.push(SwitchItem::Browser);
        }
        // `More` sits right under the back button, in reading order with the
        // task-info block it belongs to, and only when there's a body to open.
        shelbi_app::review::panel_rows(!self.description.trim().is_empty(), true, &switches)
    }

    /// Start index and count of the contiguous `Switch` run in [`rows`].
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
        self.selected = shelbi_app::review::step_selection(self.selected, self.rows().len(), delta);
    }

    /// Activate the selected row (Enter / Space).
    pub fn activate(&mut self) -> PanelEffect {
        let rows = self.rows();
        let Some(row) = rows.get(self.selected) else {
            return PanelEffect::None;
        };
        self.activate_row(*row)
    }

    /// Activate `row` through the shared table, which also declines Approve /
    /// Reject while the gated merge is already in flight — a replayed or
    /// double press (Enter *or* click) is dropped.
    fn activate_row(&mut self, row: PanelRow) -> PanelEffect {
        let has_body = !self.description.trim().is_empty();
        let (effect, view) = shelbi_app::review::activate_row(row, has_body, self.merging, self.apps);
        if let Some(view) = view {
            self.active_view = view;
        }
        effect
    }

    /// Ask to open the task-description popover (the `More` link / `m` key).
    /// Returns [`PanelEffect::None`] when the task has no body.
    pub fn request_description(&self) -> PanelEffect {
        shelbi_app::review::description_effect(!self.description.trim().is_empty())
    }

    /// Map a click at (`column`, `row`) to an effect.
    pub fn click(&mut self, column: u16, row: u16) -> PanelEffect {
        if let Some(r) = self.more_hit {
            if in_rect(r, column, row) {
                // Move keyboard focus to `More` too, so a click and the keyboard
                // agree on what's selected.
                if let Some(idx) = self.rows().iter().position(|r| matches!(r, PanelRow::More)) {
                    self.selected = idx;
                }
                return self.request_description();
            }
        }
        let rows = self.rows();
        for (rect, idx) in self.hit_targets.clone() {
            if in_rect(rect, column, row) {
                if let Some(r) = rows.get(idx) {
                    self.selected = idx;
                    return self.activate_row(*r);
                }
            }
        }
        PanelEffect::None
    }

    /// Handle a q / Esc quit request. While a gated merge is in flight the quit
    /// is ignored and a short note explains why.
    pub fn request_quit(&mut self) {
        if self.merging {
            self.status_line = "merge in progress, wait for it to finish".into();
        } else {
            self.should_quit = true;
        }
    }
}

/// Record a selectable row's screen rect + its [`rows`](ReviewPanel::rows)
/// index for the click map.
fn push_hit(app: &mut ReviewPanel, rect: Rect, idx: usize) {
    app.hit_targets.push((rect, idx));
}

pub fn render_full(f: &mut Frame, app: &mut ReviewPanel, area: Rect) {
    // A non-empty `status_line` claims the bottom rows as a red, wrapped
    // warning. Carve it off first.
    let (area, status_area) = reserve_status_area(&app.status_line, area);
    if let Some(status_area) = status_area {
        render_status_line(f, &app.status_line, status_area);
    }

    // Reset the click map; each rendered region re-populates it.
    app.hit_targets.clear();
    app.more_hit = None;
    if area.width == 0 || area.height == 0 {
        return;
    }

    let bottom = area.y + area.height;
    let mut y = area.y;

    // 1. The square back button block + the review status beside it.
    let back_h = BACK_BLOCK_H.min(area.height);
    render_back_button(
        f,
        Rect {
            x: area.x,
            y,
            width: area.width,
            height: back_h,
        },
        app.selected == 0, // Back is always rows[0].
        "Ready for review",
        Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
    );
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
        let more_selected = app
            .rows()
            .get(app.selected)
            .is_some_and(|r| matches!(r, PanelRow::More));
        let (used, more_hit) = render_task_info(
            f,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: bottom - y,
            },
            &app.title,
            &app.description,
            more_selected,
        );
        app.more_hit = more_hit;
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
        render_folder(
            f,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            },
            &app.worktree,
            app.selected == folder_idx,
        );
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

    // 3b. The slot's server: its sub-state and location, live.
    if let Some((state, location)) = &app.server {
        if y < bottom {
            y = advance_blank(y, bottom);
        }
        if y < bottom {
            let line = Rect {
                x: area.x,
                y,
                width: area.width,
                height: 1,
            };
            render_server_line(f, *state, location.as_deref(), line);
            y += 1;
        }
    }

    // blank
    y = advance_blank(y, bottom);

    // 4. The view-switcher nav block.
    let (sstart, scount) = app.switch_span();
    let nav_h = (crate::sidebar::nav_lines(scount) as u16).min(bottom.saturating_sub(y));
    if y < bottom && nav_h > 0 {
        let rows = app.rows();
        let entries: Vec<NavEntry> = (sstart..sstart + scount)
            .filter_map(|i| match rows.get(i) {
                Some(PanelRow::Switch(item)) => Some(switch_entry(app, *item)),
                _ => None,
            })
            .collect();
        let selected = (app.selected >= sstart && app.selected < sstart + scount)
            .then(|| app.selected - sstart);
        let rects = render_nav_block(
            f,
            Rect {
                x: area.x,
                y,
                width: area.width,
                height: nav_h,
            },
            &entries,
            selected,
        );
        for (p, rect) in rects.into_iter().enumerate() {
            if rect.y < area.y + area.height {
                push_hit(app, rect, sstart + p);
            }
        }
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

/// The server line: the sub-state's glyph and name (`serving`, `loading`,
/// `pending-load`) in its color, then the location.
fn render_server_line(
    f: &mut Frame,
    state: shelbi_app::view::ReviewState,
    location: Option<&str>,
    area: Rect,
) {
    let dec = state.decoration();
    let mut spans = vec![Span::styled(
        format!("{} {}", dec.glyph, shelbi_app::review::sub_state_label(state)),
        Style::default().fg(crate::sidebar::decoration_to_color(dec.color)),
    )];
    if let Some(location) = location {
        spans.push(Span::styled(
            format!("  {location}"),
            Style::default().fg(palette().ansi_dark_gray),
        ));
    }
    let x = area.x + PAD;
    f.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x,
            y: area.y,
            width: area.width.saturating_sub(PAD),
            height: 1,
        },
    );
}

/// Build the nav entry (glyph, label, active) for one review switch.
fn switch_entry(app: &ReviewPanel, item: SwitchItem) -> NavEntry {
    // The labels are the shared model's, so both clients word them alike.
    let (glyph, active) = match item {
        SwitchItem::Chat => ("🤓", app.active_view == ActiveView::Chat),
        SwitchItem::Diff => ("🔀", app.active_view == ActiveView::Diff),
        SwitchItem::Editor => ("✍️", app.active_view == ActiveView::Editor),
        SwitchItem::Browser => ("🌐", false),
    };
    NavEntry {
        glyph,
        label: item.label(&app.editor_name),
        active,
    }
}

/// Render the Approve / Reject action row: `✅ Approve` (green) and `❌ Reject`
/// (red) on one line, no brackets. While a merge is in flight the row becomes a
/// busy spinner instead.
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
                Style::default().fg(palette().ansi_yellow).add_modifier(Modifier::BOLD),
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

    const ACTIONS_GAP: u16 = 4;
    let slack = width.saturating_sub(aw + rw);
    let gap = ACTIONS_GAP.min(slack).max(1);

    let approve_style = action_style(palette().green, approve_sel);
    let reject_style = action_style(palette().red, reject_sel);

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

/// One frame of the braille "merging…" spinner.
fn spinner_frame(tick: usize) -> char {
    const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
    FRAMES[tick % FRAMES.len()]
}

// ---------------------------------------------------------------------------

/// Resolve the concrete review URL to open for `task_id` (with `$PORT` /
/// `$SLOT` substituted), or `None` when none is configured.
pub(crate) fn review_url(project_name: &str, task_id: &str) -> Option<String> {
    shelbi_orchestrator::review_session::review_url(project_name, task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::BACK_BUTTON_LINE;
    use crate::theme::palette;
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
    fn the_server_line_follows_the_sidebar_row() {
        use shelbi_app::view::ReviewState;
        let mut app = panel(true);
        let out = render(&mut app, 44, 30);
        assert!(!out.contains("serving"), "no row yet, no server line: {out}");
        app.server = Some((ReviewState::Serving, Some("hub:4101".into())));
        let rows = render_lines(&mut app, 44, 30);
        let y = row_y(&rows, "serving");
        assert!(rows[y].contains("hub:4101"), "location beside the state: {}", rows[y]);
        assert!(y > row_y(&rows, "wt/review"), "under the folder row");
        assert!(y < row_y(&rows, "Reviewer"), "above the switcher");
        app.server = Some((ReviewState::Loading, None));
        assert!(render(&mut app, 44, 30).contains("loading"));
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
        assert!(out.contains("Reviewer"), "chat switch: {out}");
        assert!(out.contains("View Diff"), "diff switch: {out}");
        assert!(out.contains("Edit in Vim"), "editor switch reflects name: {out}");
        assert!(out.contains("Open Browser"), "browser switch when url set: {out}");
        assert!(out.contains("Approve"), "approve button: {out}");
        assert!(out.contains("Reject"), "reject button: {out}");
        assert!(!out.contains("[ "), "no bracketed buttons: {out}");
    }

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

    #[test]
    fn status_header_is_accent_cyan() {
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let y = BACK_BUTTON_LINE;
        let x = (0..buf.area.width)
            .find(|&x| buf[(x, y)].symbol() == "R" && buf[(x, y)].fg == palette().cyan)
            .expect("the 'Ready for review' R should be accent cyan");
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD), "status is bold");
    }

    #[test]
    fn back_button_fill_reads_as_a_square() {
        let mut term = Terminal::new(TestBackend::new(30, 24)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let x = PAD; // button starts at the 2-col pad
        assert_eq!(buf[(x, BACK_BUTTON_LINE)].bg, palette().selection, "arrow row carries the button fill");
        assert_eq!(
            buf[(x, BACK_BUTTON_LINE - 1)].fg,
            palette().selection,
            "bleed above carries the fill colour so it reads square"
        );
        assert_eq!(
            buf[(x, BACK_BUTTON_LINE + 1)].fg,
            palette().selection,
            "bleed below carries the fill colour so it reads square"
        );
    }

    #[test]
    fn activating_back_button_focuses_the_dashboard() {
        let mut app = panel(true);
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Back)).unwrap();
        assert_eq!(idx, 0, "back button leads the panel");
        app.selected = idx;
        assert_eq!(app.activate(), PanelEffect::Back);
        let _ = render(&mut app, 30, 24);
        assert_eq!(app.click(PAD, BACK_BUTTON_LINE), PanelEffect::Back);
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

    /// The panel (status header, task info, More, the worktree line, the
    /// switches, Approve / Reject plain and reverse-video, the red status
    /// line) is readable on a light terminal in each selection.
    #[test]
    fn renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            for (name, row) in [
                ("review-panel-approve", PanelRow::Approve),
                ("review-panel-reject", PanelRow::Reject),
                ("review-panel-more", PanelRow::More),
                ("review-panel-folder", PanelRow::Folder),
            ] {
                let mut app = panel(true);
                app.status_line = "approve failed: PR #9 is not mergeable".to_string();
                app.selected = app
                    .rows()
                    .iter()
                    .position(|r| std::mem::discriminant(r) == std::mem::discriminant(&row))
                    .unwrap();
                let mut term = Terminal::new(TestBackend::new(44, 30)).unwrap();
                term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
                crate::theme::assert_light_readable(name, term.backend().buffer());
            }
        });
    }

    #[test]
    fn cleared_status_line_leaves_no_warning() {
        let mut app = panel(true);
        assert!(app.status_line.is_empty());
        let out = render(&mut app, 40, 24);
        assert!(out.contains("Ready for review"));
        assert!(!out.contains("failed"), "no stale warning: {out}");
    }

    // -- switches ----------------------------------------------------------

    #[test]
    fn selected_switch_renders_full_width_half_block_bleed() {
        let width = 30u16;
        let mut app = panel(true);
        app.nav_down();
        app.nav_down();
        let rows = render_lines(&mut app, width, 24);
        let edit_y = row_y(&rows, "Edit in Vim");
        // The bleed is inset one column from each side (matching the main
        // sidebar): a gutter space, U+2584/U+2580 over the inner width, a gutter.
        let inner = width as usize - 2;
        assert_eq!(
            rows[edit_y - 1],
            format!(" {} ", crate::sidebar::BLEED_ABOVE.repeat(inner)),
            "line above the selected switch is inset U+2584"
        );
        assert_eq!(
            rows[edit_y + 1],
            format!(" {} ", crate::sidebar::BLEED_BELOW.repeat(inner)),
            "line below the selected switch is inset U+2580"
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
        for label in ["Reviewer", "View Diff", "Edit in Vim", "Open Browser"] {
            assert_eq!(
                row_y(&chat_rows, label),
                row_y(&moved_rows, label),
                "'{label}' must not move when the selection changes"
            );
        }
        assert_eq!(
            row_y(&chat_rows, "View Diff") - row_y(&chat_rows, "Reviewer"),
            2,
            "one separator line always sits between Chat and View Diff"
        );
    }

    #[test]
    fn selected_switch_fill_is_inset_one_column() {
        let width = 30u16;
        let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
        let mut app = panel(true); // Chat focused by default
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let chat_y = row_y(&rows, "Reviewer") as u16;
        // The selection fill is inset one column from each edge, exactly like the
        // main sidebar: gutter columns transparent, the inner region filled.
        assert_eq!(buf[(0, chat_y)].bg, Color::Reset, "left gutter is transparent");
        assert_eq!(buf[(width - 1, chat_y)].bg, Color::Reset, "right gutter is transparent");
        assert_eq!(buf[(1, chat_y)].bg, palette().selection, "the fill starts one column in");
        for x in (width - 4)..(width - 1) {
            assert_eq!(buf[(x, chat_y)].bg, palette().selection, "right-edge padding carries the fill, col {x}");
        }
        assert_eq!(buf[(0, chat_y)].symbol(), " ", "column 0 is the indent gutter");
    }

    #[test]
    fn browser_entry_hidden_without_review_url() {
        let mut app = panel(false);
        let out = render(&mut app, 30, 24);
        assert!(!out.contains("Open Browser"), "no browser action when no url: {out}");
        assert!(out.contains("Reviewer") && out.contains("Approve"));
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
    fn an_app_editor_or_diff_launches_and_keeps_the_view() {
        let mut app = panel(true);
        app.apps = shelbi_state::AppTools { editor: true, diff: true };
        select_switch(&mut app, SwitchItem::Editor);
        assert_eq!(app.activate(), PanelEffect::OpenApp(shelbi_app::ReviewRole::Editor));
        select_switch(&mut app, SwitchItem::Diff);
        assert_eq!(app.activate(), PanelEffect::OpenApp(shelbi_app::ReviewRole::Diff));
        assert_eq!(app.active_view, ActiveView::Chat, "the chat stays shown");
    }

    #[test]
    fn activating_chat_and_vim_returns_switch_effects_and_marks_active() {
        let mut app = panel(true);
        assert_eq!(app.active_view, ActiveView::Chat);
        select_switch(&mut app, SwitchItem::Editor);
        assert_eq!(app.activate(), PanelEffect::ShowEditor);
        assert_eq!(app.active_view, ActiveView::Editor);
        select_switch(&mut app, SwitchItem::Chat);
        assert_eq!(app.activate(), PanelEffect::ShowChat);
        assert_eq!(app.active_view, ActiveView::Chat);
    }

    #[test]
    fn view_diff_renders_directly_below_chat() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        let chat_y = row_y(&rows, "Reviewer");
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
            vec![SwitchItem::Chat, SwitchItem::Diff, SwitchItem::Editor, SwitchItem::Browser]
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

    // -- task info / More --------------------------------------------------

    #[test]
    fn empty_body_shows_title_without_more() {
        let mut app = ReviewPanel::new("/wt", "Vim", true, "Just a title", "");
        let out = render(&mut app, 40, 24);
        assert!(out.contains("Just a title"), "title still shows: {out}");
        assert!(!out.contains("More"), "no More link without a body: {out}");
        assert_eq!(app.request_description(), PanelEffect::None);
    }

    #[test]
    fn more_requests_the_description_popover() {
        let mut app = panel(true);
        assert_eq!(app.request_description(), PanelEffect::ShowDescription);
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
        assert_eq!(buf[(more_x, more_y)].fg, palette().cyan, "More is accent cyan");
    }

    #[test]
    fn more_is_keyboard_reachable_and_activates_the_popover() {
        let mut app = panel(true);
        // Nav from the back button lands on More before the folder / switches.
        app.selected = app.rows().iter().position(|r| matches!(r, PanelRow::Back)).unwrap();
        app.nav_down();
        assert!(
            matches!(app.rows().get(app.selected), Some(PanelRow::More)),
            "↓ from Back reaches More first"
        );
        assert_eq!(app.activate(), PanelEffect::ShowDescription, "Enter on More opens the popover");
        // A full cycle (j/k or Tab drive the same step) returns to the start.
        let start = app.selected;
        let n = app.rows().len();
        for _ in 0..n {
            app.nav_down();
        }
        assert_eq!(app.selected, start, "More stays in the nav cycle");
    }

    #[test]
    fn a_bodyless_panel_has_no_more_row() {
        let app = ReviewPanel::new("/wt", "Vim", true, "T", "");
        assert!(
            !app.rows().iter().any(|r| matches!(r, PanelRow::More)),
            "no More row without a body"
        );
    }

    #[test]
    fn focused_more_carries_the_selection_fill() {
        let mut app = panel(true);
        app.selected = app.rows().iter().position(|r| matches!(r, PanelRow::More)).unwrap();
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let more_y = row_y(&rows, "More") as u16;
        let more_x = rows[more_y as usize].find("More").unwrap() as u16;
        let cell = &buf[(more_x, more_y)];
        assert_eq!(cell.bg, palette().selection, "focused More carries the selection fill");
        assert_eq!(cell.fg, Color::White, "focused More brightens to white");
        assert!(cell.modifier.contains(Modifier::BOLD), "focused More is bold");
    }

    #[test]
    fn switch_icons_start_at_col_two() {
        // Every left-anchored icon shares col 2 (the back button, the folder 📁,
        // and each nav switch's icon). Here we check the first switch's glyph.
        let mut app = panel(true);
        let rows = render_lines(&mut app, 44, 24);
        let chat_y = row_y(&rows, "Reviewer");
        let col = rows[chat_y].chars().take_while(|c| *c == ' ').count();
        assert_eq!(col, 2, "the switch icon starts at col 2, got row {:?}", rows[chat_y]);
    }

    #[test]
    fn panel_text_cells_keep_the_default_background() {
        // The panel paints no opaque background: a plain text cell (the task
        // title) sits on the terminal default so a transparent terminal shows
        // through.
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        let mut app = panel(true);
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let title_y = row_y(&rows, "Cold-start cache") as u16;
        let title_x = rows[title_y as usize].find('C').unwrap() as u16;
        assert_eq!(buf[(title_x, title_y)].bg, Color::Reset, "task title cell is transparent");
        // A blank gutter column is transparent too.
        assert_eq!(buf[(0, title_y)].bg, Color::Reset, "left gutter is transparent");
    }

    #[test]
    fn task_info_and_folder_keep_a_two_column_right_gutter() {
        // Title, wrapped description, and the worktree path all stay two columns
        // clear of the right border (matching the left gutter): no text glyph
        // lands in the two right-most columns, and the folder path never clips at
        // the border.
        let width = 40u16;
        let mut app = ReviewPanel::new(
            "/Users/jlong/Workspaces/32pixels/ContextStore/.shelbi/wt/review",
            "Vim".to_string(),
            true,
            "A very long task title that would otherwise run right up against the sidebar border",
            BODY,
        );
        let mut term = Terminal::new(TestBackend::new(width, 28)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();

        // The title must actually reach near the border (otherwise the gutter is
        // trivially satisfied by short text) and ellipsize within the reduced
        // width.
        let title_y = row_y(&rows, "A very long task title");
        assert!(rows[title_y].contains('…'), "the long title ellipsizes: {:?}", rows[title_y]);
        // The description was clipped, so its last visible line ends in `...`.
        let more_y = row_y(&rows, "More");
        assert!(
            rows[more_y - 1].trim_end().ends_with("..."),
            "the clipped description ends in ...: {:?}",
            rows[more_y - 1]
        );
        let folder_y = row_y(&rows, "wt/review");
        assert!(rows[folder_y].contains("..."), "the path is left-truncated: {:?}", rows[folder_y]);

        // Every text-content row (title, each description line, the folder path)
        // leaves the two right-most columns blank.
        let mut text_rows = vec![title_y, folder_y];
        text_rows.extend((title_y + 1)..more_y);
        for y in text_rows {
            for x in (width - 2)..width {
                assert_eq!(
                    buf[(x, y as u16)].symbol(),
                    " ",
                    "col {x} of row {y} ({:?}) must be blank — two-column right gutter",
                    rows[y]
                );
            }
        }
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

    #[test]
    fn selected_action_renders_reverse_video_tint() {
        for (tint, needle, item) in [
            (palette().green, "Approve", PanelRow::Approve),
            (palette().red, "Reject", PanelRow::Reject),
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
        let mut app = panel(true);
        app.selected = app.rows().iter().position(|r| matches!(r, PanelRow::Folder)).unwrap();
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        for (tint, needle) in [(palette().green, "Approve"), (palette().red, "Reject")] {
            let y = row_y(&rows, needle) as u16;
            let col = rows[y as usize].find(needle).unwrap() as u16;
            let cell = &buf[(col, y)];
            assert!(!cell.modifier.contains(Modifier::REVERSED), "{needle}: flat when unselected");
            assert_eq!(cell.fg, tint, "{needle}: carries the design tint");
        }
    }

    #[test]
    fn renders_at_minimum_width() {
        let mut app = panel(true);
        let rows = render_lines(&mut app, 24, 30);
        let out = rows.join("\n");
        assert!(out.contains("Ready"), "status (possibly clipped) still renders: {out}");
        assert!(out.contains("Cold-start cache"), "title: {out}");
        assert!(out.contains("Reviewer"), "nav: {out}");
        let ay = row_y(&rows, "Approve");
        assert_eq!(ay, row_y(&rows, "Reject"), "actions share a row even at 24 cols");
        let line = &rows[ay];
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
}
