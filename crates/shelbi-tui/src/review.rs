//! Review queue view — a master/detail ratatui pane hosted in the
//! project's hidden stash window and swapped into the dashboard via the
//! palette. Replaces the previous `printf '\033c'; shelbi list | grep …`
//! loop with a real interactive list.
//!
//! Left column: the tasks currently in the [`Column::Review`] column,
//! sorted by priority. Right column: full detail of whatever is
//! highlighted — id, branch, workspace, timestamps, and the task body the
//! orchestrator wrote when it routed the work.
//!
//! Pressing Enter on a task loads it onto a review workspace, the same flow
//! the sidebar uses. As with the Kanban pane the parent shell wraps this in a
//! `while true` loop so we deliberately don't bind a quit key — switching
//! away is the palette's job.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_core::{Column, Project, Task};
use shelbi_state::keymap::{DisplayStyle, Keymaps, ReviewAction};
use shelbi_state::TaskFile;

use crate::keymap::format_chord_or_unbound;

pub struct ReviewApp {
    pub project_name: String,
    pub queue: Vec<TaskFile>,
    /// Project config, reloaded each refresh — needed to resolve which review
    /// workspace a task is loaded on and compute its `machine:port` URL. `None`
    /// until the first successful load (or if the project can't be read).
    pub project: Option<Project>,
    pub selected: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    /// Vertical scroll offset for the detail panel's body (Markdown).
    pub body_scroll: u16,
    /// Merged keymaps, assigned by `run_review` at startup from the same
    /// load that surfaces `keys.yaml` diagnostics. The footer reads this to
    /// render hints in the user's configured chords. Defaults to empty —
    /// isolated render tests that don't exercise the footer leave it so.
    pub keymaps: Keymaps,
    /// Cached host-platform chord-display convention.
    pub display_style: DisplayStyle,
}

impl ReviewApp {
    pub fn new(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            queue: Vec::new(),
            project: None,
            selected: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            body_scroll: 0,
            keymaps: Keymaps::default(),
            display_style: DisplayStyle::detect(),
        }
    }

    /// Borrow the keymaps the footer renders hints from. Populated by
    /// `run_review`; empty by default.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// Cached host-platform chord-display convention.
    pub fn display_style(&self) -> DisplayStyle {
        self.display_style
    }

    pub fn refresh(&mut self) {
        // Reload project config so `machine:port` URLs track workspace/role
        // edits. A missing project just means no URL column — not fatal.
        self.project = shelbi_state::load_project(&self.project_name).ok();
        match shelbi_state::list_column(&self.project_name, Column::Review) {
            Ok(tasks) => {
                let prev_id = self.selected_task().map(|tf| tf.task.id.clone());
                self.queue = tasks;
                // Keep the cursor on the same task across refreshes when
                // possible — falling back to clamping if it was removed.
                if let Some(id) = prev_id {
                    if let Some(idx) = self.queue.iter().position(|tf| tf.task.id == id) {
                        self.selected = idx;
                    } else {
                        self.clamp_selection();
                        self.body_scroll = 0;
                    }
                } else {
                    self.clamp_selection();
                }
                self.last_refresh = Instant::now();
            }
            Err(e) => {
                self.status_line = format!("refresh failed: {e}");
            }
        }
    }

    pub fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_millis(750) {
            self.refresh();
        }
    }

    pub fn selected_task(&self) -> Option<&TaskFile> {
        self.queue.get(self.selected)
    }

    /// The `machine:port` URL a task is loaded on, when it's assigned to a
    /// review workspace (spec §16 — same deterministic port the sidebar
    /// badge shows). `None` for a queued task, a task on a dev workspace, or
    /// when the project config isn't available.
    pub fn location_for(&self, task: &Task) -> Option<String> {
        let project = self.project.as_ref()?;
        let name = task.assigned_to.as_deref()?;
        let ws = project.workspace(name).filter(|w| w.is_review())?;
        shelbi_orchestrator::workspace::review_workspace_port(project, ws)
            .map(|port| format!("{}:{port}", ws.machine))
    }

    fn clamp_selection(&mut self) {
        if self.queue.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.queue.len() {
            self.selected = self.queue.len() - 1;
        }
    }

    pub fn nav_up(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.selected = if self.selected == 0 {
            self.queue.len() - 1
        } else {
            self.selected - 1
        };
        self.body_scroll = 0;
    }

    pub fn nav_down(&mut self) {
        if self.queue.is_empty() {
            return;
        }
        self.selected = (self.selected + 1) % self.queue.len();
        self.body_scroll = 0;
    }

    pub fn scroll_body_up(&mut self) {
        self.body_scroll = self.body_scroll.saturating_sub(1);
    }

    pub fn scroll_body_down(&mut self) {
        self.body_scroll = self.body_scroll.saturating_add(1);
    }

    pub fn scroll_body_page_up(&mut self) {
        self.body_scroll = self.body_scroll.saturating_sub(10);
    }

    pub fn scroll_body_page_down(&mut self) {
        self.body_scroll = self.body_scroll.saturating_add(10);
    }

    pub fn scroll_body_home(&mut self) {
        self.body_scroll = 0;
    }

    /// Load the highlighted task onto a review workspace. Same path the
    /// sidebar's `ReviewTask` view uses — move the branch onto a free review
    /// workspace's worktree and launch its review agent.
    pub fn activate_selection(&mut self) {
        let Some(tf) = self.selected_task() else {
            self.status_line = "queue empty — nothing to review".into();
            return;
        };
        let id = tf.task.id.clone();
        match self.start_review(&id) {
            Ok(focus_target) => {
                let _ = run_tmux(["select-window", "-t", &focus_target]);
                self.status_line = format!("▶ reviewing {id}");
            }
            Err(e) => self.status_line = format!("review `{id}` failed: {e}"),
        }
    }

    fn start_review(&self, id: &str) -> Result<String> {
        // Shared with the sidebar's `ReviewTask` view and the palette:
        // load the branch onto a review workspace and return its pane target.
        // A queued task surfaces as an error carrying the queue position.
        shelbi_orchestrator::review::start_review_by_id(&self.project_name, id)
            .map_err(|e| anyhow::anyhow!(e))
    }
}

fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Rendering

pub fn render_full(f: &mut Frame, app: &ReviewApp, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // body
            Constraint::Length(2), // footer
        ])
        .split(area);

    render_title(f, app, outer[0]);
    render_body(f, app, outer[1]);
    render_footer(f, app, outer[2]);
}

fn render_title(f: &mut Frame, app: &ReviewApp, area: Rect) {
    let count = app.queue.len();
    let line = Line::from(vec![
        Span::styled("Review · ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.project_name.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {count} waiting"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

fn render_body(f: &mut Frame, app: &ReviewApp, area: Rect) {
    // 32-col list on the left, detail fills the rest. Below ~60 cols the
    // detail squeezes a bit but still readable.
    let split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(1)])
        .split(area);

    render_list(f, app, split[0]);
    render_detail(f, app, split[1]);
}

fn render_list(f: &mut Frame, app: &ReviewApp, area: Rect) {
    let mut items: Vec<ListItem> = Vec::with_capacity(app.queue.len());
    for tf in &app.queue {
        let mut spans = vec![Span::styled(
            tf.task.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if let Some(w) = &tf.task.assigned_to {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                format!("@{w}"),
                Style::default().fg(Color::Magenta),
            ));
        }
        // Loaded tasks show the review workspace's serving URL so the queue
        // reads which slot + port each is on at a glance.
        if let Some(loc) = app.location_for(&tf.task) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(loc, Style::default().fg(Color::Cyan)));
        }
        items.push(ListItem::new(Line::from(spans)));
    }
    if items.is_empty() {
        items.push(ListItem::new(Span::styled(
            "(queue is empty)",
            Style::default().fg(Color::DarkGray),
        )));
    }

    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(Color::DarkGray));
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(crate::theme::SELECTION_BG)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default();
    if !app.queue.is_empty() {
        state.select(Some(app.selected));
    }
    f.render_stateful_widget(list, area, &mut state);
}

fn render_detail(f: &mut Frame, app: &ReviewApp, area: Rect) {
    let Some(tf) = app.selected_task() else {
        let empty = Paragraph::new(Line::from(Span::styled(
            "  no tasks waiting for review",
            Style::default().fg(Color::DarkGray),
        )));
        f.render_widget(empty, area);
        return;
    };

    let header = detail_header(tf, app.location_for(&tf.task).as_deref());
    let header_h = header.len() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_h),
            Constraint::Length(1), // separator
            Constraint::Min(1),    // body
        ])
        .split(area);

    f.render_widget(Paragraph::new(header), chunks[0]);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(chunks[1].width.saturating_sub(2) as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[1],
    );

    // Pre-wrap here (rather than `Paragraph`'s `Wrap`) so inline-code
    // highlights stay clamped to the content width and can't bleed past
    // the pane's right edge at a wrap boundary.
    let lines = crate::markdown::render_note(&tf.body, chunks[2].width as usize);
    let body = Paragraph::new(lines).scroll((app.body_scroll, 0));
    f.render_widget(body, chunks[2]);
}

fn detail_header(tf: &TaskFile, location: Option<&str>) -> Vec<Line<'static>> {
    let task = &tf.task;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(Line::from(Span::styled(
        task.title.clone(),
        Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
    )));
    lines.push(meta_row("id", &task.id));
    if let Some(w) = &task.assigned_to {
        lines.push(Line::from(vec![
            meta_label("workspace"),
            Span::styled(format!("@{w}"), Style::default().fg(Color::Magenta)),
        ]));
    }
    // The serving URL (`machine:port`) when the task is loaded on a review
    // workspace — the address a human opens to look at the running change.
    if let Some(loc) = location {
        lines.push(Line::from(vec![
            meta_label("url"),
            Span::styled(loc.to_string(), Style::default().fg(Color::Cyan)),
        ]));
    }
    if let Some(branch) = &task.branch {
        lines.push(meta_row("branch", branch));
    }
    let updated = task.updated_at.format("%Y-%m-%d %H:%M UTC").to_string();
    lines.push(Line::from(vec![meta_label("updated"), Span::raw(updated)]));
    lines
}

fn meta_label(label: &str) -> Span<'static> {
    Span::styled(
        format!("  {label}: "),
        Style::default().fg(Color::DarkGray),
    )
}

fn meta_row(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![meta_label(label), Span::raw(value.to_string())])
}

fn render_footer(f: &mut Frame, app: &ReviewApp, area: Rect) {
    // Hints are sourced from the merged keymaps and rendered in the host
    // platform's convention; rebinding any of these review actions updates
    // the row. Multi-bound actions show their first chord only.
    let km = app.keymaps();
    let style = app.display_style();
    let fc = |c| format_chord_or_unbound(c, style);
    let text = format!(
        "  {}/{} select   {} review   {}/{} scroll body   {} top   {} refresh",
        fc(km.review.first_chord_for(ReviewAction::NavDown)),
        fc(km.review.first_chord_for(ReviewAction::NavUp)),
        fc(km.review.first_chord_for(ReviewAction::Activate)),
        fc(km.review.first_chord_for(ReviewAction::ScrollBodyDown)),
        fc(km.review.first_chord_for(ReviewAction::ScrollBodyUp)),
        fc(km.review.first_chord_for(ReviewAction::ScrollBodyHome)),
        fc(km.review.first_chord_for(ReviewAction::Refresh)),
    );
    let keys = Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)));
    let status = if app.status_line.is_empty() {
        Line::raw("")
    } else {
        Line::from(Span::styled(
            format!("  {}", app.status_line),
            Style::default().fg(Color::Yellow),
        ))
    };
    f.render_widget(Paragraph::new(vec![keys, status]), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    use crate::test_support::{provision_hub_repo_for_project, ENV_LOCK};

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-review-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn save_review_task(project: &str, id: &str, title: &str, assigned_to: Option<&str>) {
        let now = chrono::Utc::now();
        let task = shelbi_core::Task {
            id: id.into(),
            title: title.into(),
            column: Column::Review,
            priority: 0,
            assigned_to: assigned_to.map(str::to_string),
            workflow: None,
            branch: Some(format!("shelbi/{id}")),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        };
        shelbi_state::save_task(project, &task, "# body").unwrap();
    }

    fn dump(term: &Terminal<TestBackend>) -> String {
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The `__review` queue surfaces which review workspace + URL each loaded
    /// task is on (spec §8/§16): a task assigned to a `role: review` workspace
    /// shows its deterministic `machine:port` in both the list and the detail
    /// header; a task not on a review slot shows neither.
    #[test]
    fn review_view_surfaces_workspace_and_url_for_loaded_task() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        provision_hub_repo_for_project(&home, "demo");
        // Add a review workspace so a loaded task resolves to a port.
        let mut project = shelbi_state::load_project("demo").unwrap();
        project.workspaces.push(shelbi_core::WorkspaceSpec {
            name: "review-1".into(),
            machine: "hub".into(),
            runner: "claude".into(),
            role: shelbi_core::model::WorkspaceRole::Review,
        });
        shelbi_state::save_project(&project).unwrap();

        save_review_task("demo", "loaded", "Palette fix", Some("review-1"));
        save_review_task("demo", "waiting", "Onboarding copy", None);

        let mut app = ReviewApp::new("demo");
        app.refresh();

        // location_for resolves the deterministic port for the loaded task,
        // and nothing for the queued one.
        let loaded = app.queue.iter().find(|tf| tf.task.id == "loaded").unwrap();
        let waiting = app.queue.iter().find(|tf| tf.task.id == "waiting").unwrap();
        assert_eq!(app.location_for(&loaded.task).as_deref(), Some("hub:3000"));
        assert!(app.location_for(&waiting.task).is_none());

        // The URL shows up in the rendered list. Select the loaded task so
        // the detail header carries the `url:` row too.
        app.selected = app.queue.iter().position(|tf| tf.task.id == "loaded").unwrap();
        let backend = TestBackend::new(90, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &app, f.area())).unwrap();
        let screen = dump(&term);
        assert!(screen.contains("hub:3000"), "URL must render, got:\n{screen}");
        assert!(screen.contains("@review-1"), "workspace must render, got:\n{screen}");
        assert!(screen.contains("url:"), "detail header carries a url row, got:\n{screen}");

        std::env::remove_var("SHELBI_HOME");
    }
}
