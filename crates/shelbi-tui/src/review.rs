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
//! Pressing Enter on a task kicks off the same review checkout flow the
//! sidebar uses. As with the Kanban pane the parent shell wraps this in a
//! `while true` loop so we deliberately don't bind a quit key — switching
//! away is the palette's job.

use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use shelbi_core::Column;
use shelbi_state::keymap::{DisplayStyle, Keymaps, ReviewAction};
use shelbi_state::TaskFile;

use crate::keymap::format_chord_or_unbound;

pub struct ReviewApp {
    pub project_name: String,
    pub queue: Vec<TaskFile>,
    pub selected: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    /// Vertical scroll offset for the detail panel's body (Markdown).
    pub body_scroll: u16,
    /// Merged keymaps, assigned by `run_review` at startup from the same
    /// load that surfaces `keys.yml` diagnostics. The footer reads this to
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

    /// Trigger the review checkout flow for the highlighted task. Same
    /// path the sidebar's `ReviewTask` view uses — bring the branch into
    /// the machine's review work_dir and (re)launch the review pane.
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
        let project = shelbi_state::load_project(&self.project_name)?;
        let tf = shelbi_state::load_task(&self.project_name, id)?;
        let machine =
            shelbi_orchestrator::review::resolve_review_machine(&project, &tf.task, None)?;
        let addr = shelbi_orchestrator::review::start_review(
            shelbi_orchestrator::review::ReviewSpec {
                project: &project,
                machine,
                task: &tf.task,
                task_body: &tf.body,
            },
        )?;
        Ok(addr.target())
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
            .bg(Color::Blue)
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

    let header = detail_header(tf);
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

    let body = Paragraph::new(tf.body.clone())
        .wrap(Wrap { trim: false })
        .scroll((app.body_scroll, 0));
    f.render_widget(body, chunks[2]);
}

fn detail_header(tf: &TaskFile) -> Vec<Line<'static>> {
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
