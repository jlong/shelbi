//! The **workspace panel** of the workspace interface — the sidebar-column view
//! shown when the user opens a dev workspace that has a running task (the
//! workspace-sidebar task). It replaces the nav sidebar the same way the review
//! panel does, and renders, for the in-progress task on that workspace:
//!
//! - a **square back button** with the task **status** beside it (`IN PROGRESS`
//!   bold yellow while working),
//! - **task info**: the title (bold white), a few wrapped lines of the body
//!   preview, and a cyan **`More`** link that opens the full task-description
//!   popover,
//! - the workspace worktree's **folder** row (left-truncated, revealed on
//!   click), and
//! - a **view-switcher** nav block — the workspace's **agent** session (default,
//!   labelled with the agent's display name) / **View Diff** / **Edit in
//!   <editor>** — the active view highlighted.
//!
//! No Approve / Reject (those belong to review). The header, task-info, worktree
//! and nav-block rendering are the shared [`crate::panel`] primitives, so this
//! panel and the review panel draw the sidebar column identically.
//!
//! The state machine is pure and side-effect free: [`WorkspacePanel`] methods
//! return a [`WsEffect`] the host loop carries out, so rendering and input are
//! unit-testable with a `TestBackend`.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    Frame,
};

use crate::panel::{
    advance_blank, in_rect, render_back_button, render_folder, render_nav_block,
    render_status_line, render_task_info, reserve_status_area, NavEntry, BACK_BLOCK_H,
    BACK_BTN_WIDTH, PAD,
};

// The rows, their activation and the effects are the shared panel
// navigation (`shelbi_app::workspace_panel`), so the TUI and the desktop
// navigate the workspace panel by one state machine. The TUI asks for no
// user-shell row (the desktop's), so `WsEffect::ToggleShell` never comes
// out of it.
use shelbi_app::workspace_panel::{
    activate_row, description_effect, panel_rows, WsPanelShape, WsRow as PanelRow,
    WsSwitch as SwitchItem,
};
/// Which content view the workspace interface is currently showing. Drives the
/// nav highlight.
pub use shelbi_app::workspace_panel::WsSwitch as WsView;
/// What the host loop should do after a panel interaction.
pub use shelbi_app::workspace_panel::WsEffect;

/// The status label + colour shown beside the back button, derived from the
/// task's column/state by the host (`IN PROGRESS` yellow, `READY FOR REVIEW`
/// cyan, …).
#[derive(Debug, Clone)]
pub struct WsStatus {
    pub text: String,
    pub tone: StatusTone,
}

/// A status's colour, as a theme token: resolved when the panel draws, so a
/// theme change (Settings ▸ General) repaints an open panel's status too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Yellow,
    Cyan,
    Green,
    Muted,
}

impl StatusTone {
    pub fn color(self) -> Color {
        let p = crate::theme::palette();
        match self {
            StatusTone::Yellow => p.yellow,
            StatusTone::Cyan => p.cyan,
            StatusTone::Green => p.green,
            StatusTone::Muted => p.muted,
        }
    }
}

/// The workspace panel's full state.
pub struct WorkspacePanel {
    /// Absolute path of the workspace worktree — shown truncated, revealed on
    /// click.
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …).
    pub editor_name: String,
    /// Display name of the workspace's agent (`Developer`, …) — the Agent
    /// switch's label.
    pub agent_name: String,
    /// The task's title (bold white).
    pub title: String,
    /// The task's markdown body (preview + popover). Empty hides the preview and
    /// `More`.
    pub description: String,
    /// The status shown beside the back button.
    pub status: WsStatus,
    /// Which content view is shown (drives the highlight).
    pub active_view: WsView,
    /// Selected panel row (index into [`rows`](Self::rows)).
    pub selected: usize,
    /// A message on the panel's status line (effect failures, notes).
    pub status_line: String,
    hit_targets: Vec<(Rect, usize)>,
    more_hit: Option<Rect>,
}

impl WorkspacePanel {
    pub fn new(
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        agent_name: impl Into<String>,
        title: impl Into<String>,
        description: impl Into<String>,
        status: WsStatus,
    ) -> Self {
        let mut panel = Self {
            worktree: worktree.into(),
            editor_name: editor_name.into(),
            agent_name: agent_name.into(),
            title: title.into(),
            description: description.into(),
            status,
            active_view: WsView::Agent,
            selected: 0,
            status_line: String::new(),
            hit_targets: Vec::new(),
            more_hit: None,
        };
        // Focus the Agent switch initially — it's the default content view.
        panel.selected = panel
            .rows()
            .iter()
            .position(|r| matches!(r, PanelRow::Switch(SwitchItem::Agent)))
            .unwrap_or(0);
        panel
    }

    /// The keyboard-navigable rows, top to bottom: Back, `More` (only with a
    /// body, in reading order with the task-info block it belongs to), the
    /// folder, then the view switch.
    fn rows(&self) -> Vec<PanelRow> {
        panel_rows(&WsPanelShape {
            has_body: !self.description.trim().is_empty(),
            folder: true,
            switches: vec![SwitchItem::Agent, SwitchItem::Diff, SwitchItem::Editor],
            shell: false,
        })
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
    pub fn activate(&mut self) -> WsEffect {
        let rows = self.rows();
        let Some(row) = rows.get(self.selected) else {
            return WsEffect::None;
        };
        self.activate_row(*row)
    }

    fn activate_row(&mut self, row: PanelRow) -> WsEffect {
        let (effect, view) = activate_row(row, !self.description.trim().is_empty());
        if let Some(view) = view {
            self.active_view = view;
        }
        effect
    }

    /// Ask to open the task-description popover (the `More` link / `m` key).
    pub fn request_description(&self) -> WsEffect {
        description_effect(!self.description.trim().is_empty())
    }

    /// Map a click at (`column`, `row`) to an effect.
    pub fn click(&mut self, column: u16, row: u16) -> WsEffect {
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
        WsEffect::None
    }
}

/// Record a selectable row's screen rect + its [`rows`](WorkspacePanel::rows)
/// index for the click map.
fn push_hit(app: &mut WorkspacePanel, rect: Rect, idx: usize) {
    app.hit_targets.push((rect, idx));
}

pub fn render_full(f: &mut Frame, app: &mut WorkspacePanel, area: Rect) {
    let (area, status_area) = reserve_status_area(&app.status_line, area);
    if let Some(status_area) = status_area {
        render_status_line(f, &app.status_line, status_area);
    }

    app.hit_targets.clear();
    app.more_hit = None;
    if area.width == 0 || area.height == 0 {
        return;
    }

    let bottom = area.y + area.height;
    let mut y = area.y;

    // 1. The square back button block + the status beside it.
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
        &app.status.text,
        Style::default().fg(app.status.tone.color()).add_modifier(Modifier::BOLD),
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
    }
}

/// Build the nav entry (glyph, label, active) for one workspace switch.
fn switch_entry(app: &WorkspacePanel, item: SwitchItem) -> NavEntry {
    let (glyph, label, active) = match item {
        SwitchItem::Agent => (
            "💬",
            app.agent_name.clone(),
            app.active_view == WsView::Agent,
        ),
        SwitchItem::Diff => (
            "🔀",
            "View Diff".to_string(),
            app.active_view == WsView::Diff,
        ),
        SwitchItem::Editor => (
            "✍️",
            format!("Edit in {}", app.editor_name),
            app.active_view == WsView::Editor,
        ),
    };
    NavEntry {
        glyph,
        label,
        active,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::panel::BACK_BUTTON_LINE;
    use crate::theme::palette;
    use ratatui::{backend::TestBackend, Terminal};

    const BODY: &str = "## Summary\n\nWarm the application cache during startup so the first request can reuse the same data as subsequent requests.\n\n## Acceptance criteria\n\n- Initialize the cache once before accepting traffic.";

    fn in_progress() -> WsStatus {
        WsStatus {
            text: "IN PROGRESS".into(),
            tone: StatusTone::Yellow,
        }
    }

    fn panel() -> WorkspacePanel {
        WorkspacePanel::new(
            "/Users/j/my-project/.shelbi/wt/alpha",
            "Vim".to_string(),
            "Developer".to_string(),
            "Cold-start cache",
            BODY,
            in_progress(),
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

    fn render(app: &mut WorkspacePanel, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render_full(f, app, f.area())).unwrap();
        dump(&term)
    }

    fn render_lines(app: &mut WorkspacePanel, w: u16, h: u16) -> Vec<String> {
        render(app, w, h).split('\n').map(str::to_string).collect()
    }

    fn row_y(rows: &[String], needle: &str) -> usize {
        rows.iter()
            .position(|r| r.contains(needle))
            .unwrap_or_else(|| panic!("expected row containing {needle:?} in:\n{}", rows.join("\n")))
    }

    /// The panel in each status color (in progress, ready for review, done,
    /// a custom status) and with the back button, More, and a switch
    /// selected, plus a status line, is readable on a light terminal.
    #[test]
    fn renders_readable_in_light() {
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            let statuses = [
                ("in-progress", "IN PROGRESS", StatusTone::Yellow),
                ("review", "READY FOR REVIEW", StatusTone::Cyan),
                ("done", "DONE", StatusTone::Green),
                ("custom", "BLOCKED", StatusTone::Muted),
            ];
            for (i, (name, text, tone)) in statuses.into_iter().enumerate() {
                let mut app = WorkspacePanel::new(
                    "/Users/j/my-project/.shelbi/wt/alpha",
                    "Vim".to_string(),
                    "Developer".to_string(),
                    "Cold-start cache",
                    BODY,
                    WsStatus {
                        text: text.into(),
                        tone,
                    },
                );
                app.status_line = "agent restarted".into();
                let rows = app.rows();
                app.selected = [
                    PanelRow::Back,
                    PanelRow::More,
                    PanelRow::Folder,
                    PanelRow::Switch(SwitchItem::Diff),
                ]
                .iter()
                .filter_map(|want| rows.iter().position(|r| r == want))
                .nth(i)
                .unwrap_or(0);
                let mut term = Terminal::new(TestBackend::new(44, 28)).unwrap();
                term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
                crate::theme::assert_light_readable(
                    &format!("workspace-panel-{name}"),
                    term.backend().buffer(),
                );
            }
        });
    }

    #[test]
    fn renders_status_task_info_worktree_and_switches() {
        let mut app = panel();
        let out = render(&mut app, 44, 24);
        assert!(out.contains("IN PROGRESS"), "status header: {out}");
        assert!(out.contains("Cold-start cache"), "task title: {out}");
        assert!(out.contains("Warm the application cache"), "description preview: {out}");
        assert!(out.contains("More"), "More link: {out}");
        assert!(out.contains("wt/alpha"), "worktree folder shown: {out}");
        assert!(out.contains("Developer"), "agent switch uses the display name: {out}");
        assert!(out.contains("View Diff"), "diff switch: {out}");
        assert!(out.contains("Edit in Vim"), "editor switch reflects name: {out}");
        // No review actions on a workspace panel.
        assert!(!out.contains("Approve"), "no Approve on a workspace panel: {out}");
        assert!(!out.contains("Reject"), "no Reject on a workspace panel: {out}");
        assert!(!out.contains("Browser"), "no Browser switch: {out}");
    }

    #[test]
    fn status_in_progress_is_bold_yellow_on_the_button_row() {
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        let mut app = panel();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let y = BACK_BUTTON_LINE;
        let x = (0..buf.area.width)
            .find(|&x| buf[(x, y)].symbol() == "I" && buf[(x, y)].fg == palette().yellow)
            .expect("the 'IN PROGRESS' I should be the status yellow");
        assert!(buf[(x, y)].modifier.contains(Modifier::BOLD), "status is bold");
        // The back-arrow glyph shares the row.
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        assert!(rows[y as usize].contains('\u{2190}'), "back arrow on the status row");
    }

    #[test]
    fn a_non_in_progress_status_uses_its_own_colour() {
        let mut app = WorkspacePanel::new(
            "/wt",
            "Vim",
            "Developer",
            "T",
            "b",
            WsStatus {
                text: "READY FOR REVIEW".into(),
                tone: StatusTone::Cyan,
            },
        );
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let y = BACK_BUTTON_LINE;
        let found = (0..buf.area.width)
            .any(|x| buf[(x, y)].symbol() == "R" && buf[(x, y)].fg == palette().cyan);
        assert!(found, "the status header carries its configured colour");
    }

    #[test]
    fn agent_switch_is_selected_and_inset_by_default() {
        let width = 30u16;
        let mut term = Terminal::new(TestBackend::new(width, 24)).unwrap();
        let mut app = panel();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let agent_y = row_y(&rows, "Developer") as u16;
        // The selection fill is inset one column from each edge (matching the
        // main sidebar); the gutters stay transparent.
        assert_eq!(buf[(0, agent_y)].bg, Color::Reset, "left gutter is transparent");
        assert_eq!(buf[(width - 1, agent_y)].bg, Color::Reset, "right gutter is transparent");
        assert_eq!(buf[(1, agent_y)].bg, palette().selection, "the fill starts one column in");
        for x in (width - 4)..(width - 1) {
            assert_eq!(buf[(x, agent_y)].bg, palette().selection, "right-edge padding carries the fill, col {x}");
        }
    }

    #[test]
    fn switch_icons_start_at_col_two() {
        let mut app = panel();
        let rows = render_lines(&mut app, 44, 24);
        let agent_y = row_y(&rows, "Developer");
        let col = rows[agent_y].chars().take_while(|c| *c == ' ').count();
        assert_eq!(col, 2, "the switch icon starts at col 2, got row {:?}", rows[agent_y]);
    }

    #[test]
    fn more_is_keyboard_reachable_and_activates_the_popover() {
        let mut app = panel();
        app.selected = app.rows().iter().position(|r| matches!(r, PanelRow::Back)).unwrap();
        app.nav_down();
        assert!(
            matches!(app.rows().get(app.selected), Some(PanelRow::More)),
            "↓ from Back reaches More first"
        );
        assert_eq!(app.activate(), WsEffect::ShowDescription, "Enter on More opens the popover");
    }

    #[test]
    fn focused_more_carries_the_selection_fill() {
        let mut app = panel();
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
    fn panel_text_cells_keep_the_default_background() {
        let mut term = Terminal::new(TestBackend::new(44, 24)).unwrap();
        let mut app = panel();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();
        let title_y = row_y(&rows, "Cold-start cache") as u16;
        let title_x = rows[title_y as usize].find('C').unwrap() as u16;
        assert_eq!(buf[(title_x, title_y)].bg, Color::Reset, "task title cell is transparent");
        assert_eq!(buf[(0, title_y)].bg, Color::Reset, "left gutter is transparent");
    }

    #[test]
    fn task_info_and_folder_keep_a_two_column_right_gutter() {
        // The workspace panel shares the review panel's task-info block, so it
        // gets the same two-column right gutter: title, wrapped description, and
        // the worktree path all stay clear of the right border.
        let width = 40u16;
        let mut app = WorkspacePanel::new(
            "/Users/jlong/Workspaces/32pixels/ContextStore/.shelbi/wt/alpha",
            "Vim".to_string(),
            "Developer".to_string(),
            "A very long task title that would otherwise run right up against the sidebar border",
            BODY,
            in_progress(),
        );
        let mut term = Terminal::new(TestBackend::new(width, 28)).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let rows = dump(&term).split('\n').map(str::to_string).collect::<Vec<_>>();

        let title_y = row_y(&rows, "A very long task title");
        assert!(rows[title_y].contains('…'), "the long title ellipsizes: {:?}", rows[title_y]);
        let more_y = row_y(&rows, "More");
        assert!(
            rows[more_y - 1].trim_end().ends_with("..."),
            "the clipped description ends in ...: {:?}",
            rows[more_y - 1]
        );
        let folder_y = row_y(&rows, "wt/alpha");
        assert!(rows[folder_y].contains("..."), "the path is left-truncated: {:?}", rows[folder_y]);

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

    #[test]
    fn diff_renders_directly_below_the_agent_switch() {
        let mut app = panel();
        let rows = render_lines(&mut app, 44, 24);
        let agent_y = row_y(&rows, "Developer");
        let diff_y = row_y(&rows, "View Diff");
        assert_eq!(diff_y - agent_y, 2, "View Diff sits immediately below the agent switch");
        assert!(diff_y < row_y(&rows, "Edit in Vim"), "View Diff precedes Edit");
    }

    #[test]
    fn back_is_the_first_row_and_activates_back() {
        let mut app = panel();
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Back)).unwrap();
        assert_eq!(idx, 0);
        app.selected = idx;
        assert_eq!(app.activate(), WsEffect::Back);
        let _ = render(&mut app, 30, 24);
        assert_eq!(app.click(PAD, BACK_BUTTON_LINE), WsEffect::Back);
    }

    #[test]
    fn folder_activation_reveals_the_worktree() {
        let mut app = panel();
        let idx = app.rows().iter().position(|r| matches!(r, PanelRow::Folder)).unwrap();
        app.selected = idx;
        assert_eq!(app.activate(), WsEffect::RevealFolder);
    }

    #[test]
    fn switches_return_their_effects_and_mark_active() {
        let mut app = panel();
        assert_eq!(app.active_view, WsView::Agent);
        for (item, effect, view) in [
            (SwitchItem::Diff, WsEffect::ShowDiff, WsView::Diff),
            (SwitchItem::Editor, WsEffect::ShowEditor, WsView::Editor),
            (SwitchItem::Agent, WsEffect::ShowAgent, WsView::Agent),
        ] {
            let idx = app
                .rows()
                .iter()
                .position(|r| matches!(r, PanelRow::Switch(i) if *i == item))
                .unwrap();
            app.selected = idx;
            assert_eq!(app.activate(), effect);
            assert_eq!(app.active_view, view);
        }
    }

    #[test]
    fn clicking_view_diff_dispatches_show_diff() {
        let mut app = panel();
        let rows = render_lines(&mut app, 44, 24);
        let diff_line = row_y(&rows, "View Diff") as u16;
        assert_eq!(app.click(4, diff_line), WsEffect::ShowDiff);
        assert_eq!(app.active_view, WsView::Diff);
    }

    #[test]
    fn more_opens_the_description_popover_and_is_inert_without_a_body() {
        let mut app = panel();
        assert_eq!(app.request_description(), WsEffect::ShowDescription);
        let rows = render_lines(&mut app, 44, 24);
        let more_y = row_y(&rows, "More") as u16;
        let more_x = rows[more_y as usize].find("More").unwrap() as u16;
        assert_eq!(app.click(more_x, more_y), WsEffect::ShowDescription);

        let mut bodyless =
            WorkspacePanel::new("/wt", "Vim", "Developer", "Just a title", "", in_progress());
        let out = render(&mut bodyless, 40, 24);
        assert!(out.contains("Just a title"));
        assert!(!out.contains("More"), "no More without a body: {out}");
        assert_eq!(bodyless.request_description(), WsEffect::None);
    }

    #[test]
    fn nav_cycles_through_all_rows() {
        let mut app = panel();
        let n = app.rows().len();
        let start = app.selected;
        for _ in 0..n {
            app.nav_down();
        }
        assert_eq!(app.selected, start, "a full cycle returns to the start");
    }

    #[test]
    fn renders_at_minimum_width() {
        let mut app = panel();
        let out = render(&mut app, 24, 30);
        assert!(out.contains("IN PROGRESS") || out.contains("IN PROGRES"), "status renders: {out}");
        assert!(out.contains("Cold-start cache"), "title: {out}");
        assert!(out.contains("Developer"), "agent switch: {out}");
    }
}
