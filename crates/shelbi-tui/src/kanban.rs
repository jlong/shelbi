//! Kanban Tasks view — 5 columns (backlog / todo / in_progress / review /
//! done), rendered into the dashboard's right pane via the same hidden-pane
//! swap mechanism the other built-in views use. State is read from / written
//! to the task markdown files via `shelbi_state`; no separate cache.
//!
//! The pane is meant to outlive any one TUI process (the parent shell wraps
//! it in a `while true` loop) — so we deliberately don't bind a quit key.
//! Switching away is the palette's job.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame,
};
use shelbi_core::Column;
use shelbi_state::TaskFile;

/// State for the Kanban TUI. Selection is `(column, row)` — `row` is the
/// index inside the currently focused column.
pub struct KanbanApp {
    pub project_name: String,
    pub tasks: Vec<TaskFile>,
    pub selected_column: usize,
    pub selected_row: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    /// When `Some`, a modal task detail popover is open for the given task id.
    /// Selection underneath stays put so closing the popover returns the
    /// cursor to the same card.
    pub popover: Option<TaskPopover>,
    /// Screen-space rects for each rendered card cell, written each frame
    /// by the renderer and read by the mouse-click handler to map a click
    /// back to a (column, row) pair.
    pub card_hits: Vec<CardHit>,
    /// Declared worker names from project.yaml, in YAML order. Reloaded
    /// on every refresh so an edited project file shows up without a
    /// restart. Empty when the project YAML is missing.
    pub workers: Vec<String>,
    /// Active filter — `None` means "All workers".
    /// [`WorkerFilter::Unassigned`] keeps only cards with no `assigned_to`.
    /// Mirrored to `state.json::worker_filter` so the chip survives a
    /// respawn or project switch.
    pub worker_filter: Option<WorkerFilter>,
    /// When `Some`, the worker filter dropdown is open; carries the
    /// in-flight cursor position so navigation can move through the
    /// options list. Selection only commits on Enter/Space.
    pub worker_dropdown: Option<WorkerDropdown>,
    /// Screen-space rect of the filter chip in the title row, captured
    /// each frame by the renderer so a click on the chip can open the
    /// dropdown without keyboard.
    pub filter_chip_hit: Option<Rect>,
    /// Hit-test entries for the open dropdown's option rows. Empty when
    /// the dropdown is closed.
    pub dropdown_hits: Vec<DropdownHit>,
}

/// Worker filter applied to the visible cards. Separate from the
/// dropdown's cursor (which can hover the same options without
/// committing) so the active filter and the in-flight selection can't
/// silently desync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerFilter {
    /// Show only tasks assigned to this worker name.
    Worker(String),
    /// Show only tasks with no `assigned_to`. Distinct from "All" so the
    /// user can isolate the orchestrator-untouched backlog.
    Unassigned,
}

impl WorkerFilter {
    /// Wire format stored in `state.json::worker_filter`. `Unassigned`
    /// gets a sentinel string rather than its own enum variant on disk
    /// so the schema stays a plain `Option<String>` — no migration
    /// needed when older code reads the field.
    pub const UNASSIGNED_SENTINEL: &'static str = "__unassigned__";

    fn from_disk(s: &str) -> WorkerFilter {
        if s == Self::UNASSIGNED_SENTINEL {
            WorkerFilter::Unassigned
        } else {
            WorkerFilter::Worker(s.to_string())
        }
    }

    fn to_disk(&self) -> String {
        match self {
            WorkerFilter::Worker(w) => w.clone(),
            WorkerFilter::Unassigned => Self::UNASSIGNED_SENTINEL.to_string(),
        }
    }

    /// Predicate against a task's `assigned_to` field.
    pub fn matches(&self, assigned_to: Option<&str>) -> bool {
        match self {
            WorkerFilter::Worker(w) => assigned_to == Some(w.as_str()),
            WorkerFilter::Unassigned => assigned_to.is_none(),
        }
    }

    /// Short label used in the chip and the dropdown row.
    pub fn label(&self) -> String {
        match self {
            WorkerFilter::Worker(w) => w.clone(),
            WorkerFilter::Unassigned => "Unassigned".to_string(),
        }
    }
}

/// One entry rendered in the open dropdown — `None` is the "All
/// workers" reset row, `Some(filter)` is a concrete filter the user can
/// commit to. Order matches the rendered options list, so the dropdown
/// cursor and this slice can share an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropdownOption {
    pub filter: Option<WorkerFilter>,
    /// How many tasks currently match this option — shown in the
    /// dropdown row as a hint. Computed at render time from the
    /// in-memory tasks slice.
    pub count: usize,
}

/// State carried while the worker filter dropdown is open. Lives only
/// for the lifetime of the popover — selecting an option closes it and
/// drops this back to `None`.
#[derive(Debug, Clone)]
pub struct WorkerDropdown {
    /// Cursor row inside the options list; never out of range because
    /// [`KanbanApp::open_worker_dropdown`] seeds it from the active
    /// filter and the nav methods clamp.
    pub cursor: usize,
}

/// One option row's hit-test rect. Captured each frame by the dropdown
/// renderer so a click can map a screen coordinate back to an option
/// index in the rendered options list.
#[derive(Clone, Copy, Debug)]
pub struct DropdownHit {
    pub area: Rect,
    pub option_idx: usize,
}

/// One rendered card's screen-space rectangle and its column/row index in
/// the kanban model. Recorded each frame so click handling and keyboard
/// selection share the same source of truth for which card sits where.
#[derive(Clone, Copy, Debug)]
pub struct CardHit {
    pub area: Rect,
    pub col_idx: usize,
    pub row_idx: usize,
}

/// State for the open task detail popover. We key by task id (not column/row
/// indices) so a background refresh that reorders cards doesn't swap the
/// popover's contents under the user.
pub struct TaskPopover {
    pub task_id: String,
    pub scroll: u16,
}

impl KanbanApp {
    pub fn new(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            tasks: Vec::new(),
            selected_column: 0,
            selected_row: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            popover: None,
            card_hits: Vec::new(),
            workers: Vec::new(),
            worker_filter: None,
            worker_dropdown: None,
            filter_chip_hit: None,
            dropdown_hits: Vec::new(),
        }
    }

    /// Hit-test a screen coordinate against the most recently rendered cards.
    /// Returns `(col_idx, row_idx)` for the card under the point, or `None`
    /// if the click missed every card (including clicks on column headers,
    /// the footer, or empty space below the last card).
    pub fn card_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        self.card_hits.iter().find_map(|hit| {
            let r = hit.area;
            let in_x = x >= r.x && x < r.x.saturating_add(r.width);
            let in_y = y >= r.y && y < r.y.saturating_add(r.height);
            (in_x && in_y).then_some((hit.col_idx, hit.row_idx))
        })
    }

    /// Move selection to the given card and open its popover. Used by the
    /// click handler so it routes through the same path as ENTER/SPACE.
    pub fn open_popover_at(&mut self, col_idx: usize, row_idx: usize) {
        self.selected_column = col_idx;
        self.selected_row = row_idx;
        self.open_popover();
    }

    pub fn popover_is_open(&self) -> bool {
        self.popover.is_some()
    }

    /// Look up the task currently shown in the popover, if any. Returns
    /// `None` if the popover is closed OR if the task has since vanished
    /// from disk (e.g. deleted by another process between refreshes).
    pub fn popover_task(&self) -> Option<&TaskFile> {
        let id = &self.popover.as_ref()?.task_id;
        self.tasks.iter().find(|tf| &tf.task.id == id)
    }

    /// Open the popover for the currently selected card. No-op if the
    /// focused column is empty.
    pub fn open_popover(&mut self) {
        let Some(tf) = self.selected_task() else {
            return;
        };
        self.popover = Some(TaskPopover {
            task_id: tf.task.id.clone(),
            scroll: 0,
        });
    }

    pub fn close_popover(&mut self) {
        self.popover = None;
    }

    pub fn popover_scroll_up(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_sub(1);
        }
    }

    pub fn popover_scroll_down(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_add(1);
        }
    }

    pub fn popover_scroll_page_up(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_sub(10);
        }
    }

    pub fn popover_scroll_page_down(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = p.scroll.saturating_add(10);
        }
    }

    pub fn popover_scroll_home(&mut self) {
        if let Some(p) = self.popover.as_mut() {
            p.scroll = 0;
        }
    }

    pub fn column(&self, idx: usize) -> Column {
        Column::ALL[idx.min(Column::ALL.len() - 1)]
    }

    pub fn column_tasks(&self, col_idx: usize) -> Vec<&TaskFile> {
        let col = self.column(col_idx);
        let mut tasks: Vec<&TaskFile> = self
            .tasks
            .iter()
            .filter(|tf| tf.task.column == col)
            .filter(|tf| self.task_matches_filter(tf))
            .collect();
        // Done shows most-recently-completed first: `updated_at` is rewritten
        // on the move-into-done write, so it's a stable proxy for completion
        // time. Other columns keep their priority order (the natural order of
        // `self.tasks`). Stable sort → equal timestamps preserve that order,
        // so identical-state polls don't reshuffle / flicker.
        if col == Column::Done {
            tasks.sort_by(|a, b| b.task.updated_at.cmp(&a.task.updated_at));
        }
        tasks
    }

    /// True when `tf` passes the active worker filter. With no filter
    /// active everything passes; otherwise the task's `assigned_to`
    /// must match the filter's predicate.
    fn task_matches_filter(&self, tf: &TaskFile) -> bool {
        match &self.worker_filter {
            None => true,
            Some(f) => f.matches(tf.task.assigned_to.as_deref()),
        }
    }

    /// Snapshot of every task's column, for the blocked derivation. Built
    /// each render call from the in-memory tasks slice — cheap and avoids
    /// a stale cache.
    fn task_columns(&self) -> HashMap<String, Column> {
        self.tasks
            .iter()
            .map(|tf| (tf.task.id.clone(), tf.task.column))
            .collect()
    }

    pub fn selected_task(&self) -> Option<&TaskFile> {
        let col_tasks = self.column_tasks(self.selected_column);
        col_tasks.get(self.selected_row).copied()
    }

    pub fn refresh(&mut self) {
        // Project YAML may be missing on a fresh project — surface an
        // empty worker list rather than failing the refresh; the
        // dropdown will degrade to just "All" / "Unassigned" until the
        // project file appears.
        self.workers = shelbi_state::load_project(&self.project_name)
            .map(|p| p.workers.into_iter().map(|w| w.name).collect())
            .unwrap_or_default();
        // Filter is purely view state — a missing / unreadable
        // state.json falls back to "All" silently. Reload every tick so
        // a CLI or palette edit shows up without a respawn.
        self.worker_filter = shelbi_state::read_state(&self.project_name)
            .ok()
            .and_then(|s| s.worker_filter)
            .map(|s| WorkerFilter::from_disk(&s));
        match shelbi_state::list_tasks(&self.project_name) {
            Ok(tasks) => {
                self.tasks = tasks;
                self.last_refresh = Instant::now();
                self.clamp_selection();
            }
            Err(e) => {
                self.status_line = format!("refresh failed: {e}");
            }
        }
    }

    /// Options shown in the worker dropdown — `All`, then each worker
    /// in YAML order, then `Unassigned` if any task lacks an
    /// `assigned_to`. Counts are computed against the unfiltered task
    /// list so a worker with zero matching cards still appears
    /// (otherwise the user couldn't pick it to clear a previously-set
    /// filter on another worker).
    pub fn dropdown_options(&self) -> Vec<DropdownOption> {
        let mut opts: Vec<DropdownOption> = Vec::with_capacity(self.workers.len() + 2);
        opts.push(DropdownOption {
            filter: None,
            count: self.tasks.len(),
        });
        for w in &self.workers {
            let count = self
                .tasks
                .iter()
                .filter(|tf| tf.task.assigned_to.as_deref() == Some(w.as_str()))
                .count();
            opts.push(DropdownOption {
                filter: Some(WorkerFilter::Worker(w.clone())),
                count,
            });
        }
        let unassigned_count = self
            .tasks
            .iter()
            .filter(|tf| tf.task.assigned_to.is_none())
            .count();
        if unassigned_count > 0 {
            opts.push(DropdownOption {
                filter: Some(WorkerFilter::Unassigned),
                count: unassigned_count,
            });
        }
        opts
    }

    /// Index of the option that matches the active filter, used to seed
    /// the dropdown cursor when it opens. Falls back to the "All" row
    /// (idx 0) if the active filter no longer appears in the options —
    /// e.g. a worker was removed from project.yaml while a filter on it
    /// was still persisted.
    fn current_filter_idx(&self, opts: &[DropdownOption]) -> usize {
        opts.iter()
            .position(|o| o.filter == self.worker_filter)
            .unwrap_or(0)
    }

    pub fn worker_dropdown_is_open(&self) -> bool {
        self.worker_dropdown.is_some()
    }

    pub fn open_worker_dropdown(&mut self) {
        let opts = self.dropdown_options();
        let cursor = self.current_filter_idx(&opts);
        self.worker_dropdown = Some(WorkerDropdown { cursor });
    }

    pub fn close_worker_dropdown(&mut self) {
        self.worker_dropdown = None;
    }

    pub fn toggle_worker_dropdown(&mut self) {
        if self.worker_dropdown_is_open() {
            self.close_worker_dropdown();
        } else {
            self.open_worker_dropdown();
        }
    }

    pub fn dropdown_nav_up(&mut self) {
        let n = self.dropdown_options().len();
        if n == 0 {
            return;
        }
        if let Some(d) = self.worker_dropdown.as_mut() {
            d.cursor = if d.cursor == 0 { n - 1 } else { d.cursor - 1 };
        }
    }

    pub fn dropdown_nav_down(&mut self) {
        let n = self.dropdown_options().len();
        if n == 0 {
            return;
        }
        if let Some(d) = self.worker_dropdown.as_mut() {
            d.cursor = (d.cursor + 1) % n;
        }
    }

    /// Commit the cursor's option as the active filter and close the
    /// dropdown. Persists to `state.json` so the chip survives a
    /// respawn.
    pub fn dropdown_select(&mut self) {
        let opts = self.dropdown_options();
        let Some(d) = self.worker_dropdown.as_ref() else {
            return;
        };
        let Some(opt) = opts.get(d.cursor) else {
            self.close_worker_dropdown();
            return;
        };
        self.apply_filter(opt.filter.clone());
        self.close_worker_dropdown();
    }

    /// Persist `filter` as the new active worker filter and update the
    /// in-memory state. Best-effort on the disk write — view state
    /// shouldn't block the UI on a transient FS error.
    fn apply_filter(&mut self, filter: Option<WorkerFilter>) {
        self.worker_filter = filter.clone();
        let disk = filter.as_ref().map(|f| f.to_disk());
        if let Err(e) = shelbi_state::set_worker_filter(&self.project_name, disk.as_deref()) {
            self.status_line = format!("filter persist failed: {e}");
        }
        // The selection may now point past the end of a column that
        // just shrank; clamp before the next render reads it.
        self.clamp_selection();
        self.status_line = match &self.worker_filter {
            None => "filter: all workers".to_string(),
            Some(f) => format!("filter: {}", f.label()),
        };
    }

    /// One-shot reset bound to the dropdown's `c` key — clears the
    /// filter without needing to navigate to the "All" row.
    pub fn dropdown_clear(&mut self) {
        self.apply_filter(None);
        self.close_worker_dropdown();
    }

    /// Map a screen coordinate to an option index in the open
    /// dropdown, or `None` if the click missed every row. Used by the
    /// mouse handler to route a left-click to the same path as the
    /// keyboard `Enter`.
    pub fn dropdown_option_at(&self, x: u16, y: u16) -> Option<usize> {
        self.dropdown_hits.iter().find_map(|hit| {
            let r = hit.area;
            let in_x = x >= r.x && x < r.x.saturating_add(r.width);
            let in_y = y >= r.y && y < r.y.saturating_add(r.height);
            (in_x && in_y).then_some(hit.option_idx)
        })
    }

    /// True when the screen coordinate falls inside the most-recently
    /// rendered filter chip. The chip lives in the title row; a click
    /// on it opens / closes the dropdown.
    pub fn filter_chip_at(&self, x: u16, y: u16) -> bool {
        match self.filter_chip_hit {
            Some(r) => {
                let in_x = x >= r.x && x < r.x.saturating_add(r.width);
                let in_y = y >= r.y && y < r.y.saturating_add(r.height);
                in_x && in_y
            }
            None => false,
        }
    }

    pub fn maybe_refresh(&mut self) {
        if self.last_refresh.elapsed() >= Duration::from_millis(750) {
            self.refresh();
        }
    }

    /// Make sure `selected_row` is in range for the current column. Pulls
    /// the cursor up to the last row if the column shrunk; leaves it at 0
    /// if the column is empty.
    pub fn clamp_selection(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if n == 0 {
            self.selected_row = 0;
        } else if self.selected_row >= n {
            self.selected_row = n - 1;
        }
    }

    pub fn nav_left(&mut self) {
        if self.selected_column == 0 {
            self.selected_column = Column::ALL.len() - 1;
        } else {
            self.selected_column -= 1;
        }
        self.clamp_selection();
    }

    pub fn nav_right(&mut self) {
        self.selected_column = (self.selected_column + 1) % Column::ALL.len();
        self.clamp_selection();
    }

    pub fn nav_up(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if n == 0 {
            return;
        }
        self.selected_row = if self.selected_row == 0 {
            n - 1
        } else {
            self.selected_row - 1
        };
    }

    pub fn nav_down(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if n == 0 {
            return;
        }
        self.selected_row = (self.selected_row + 1) % n;
    }

    /// Shove the selected card one column to the left, wrapping at backlog.
    /// Keeps focus on the moved card in its new column.
    pub fn move_card_left(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let new_col_idx = if self.selected_column == 0 {
            Column::ALL.len() - 1
        } else {
            self.selected_column - 1
        };
        self.move_card(&id, new_col_idx);
    }

    pub fn move_card_right(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let new_col_idx = (self.selected_column + 1) % Column::ALL.len();
        self.move_card(&id, new_col_idx);
    }

    fn move_card(&mut self, id: &str, new_col_idx: usize) {
        let new_col = self.column(new_col_idx);
        match shelbi_state::move_task(&self.project_name, id, new_col) {
            Ok(Some((from, to, workflow))) => {
                if let Err(e) =
                    shelbi_state::append_task_event(id, &workflow, from, to, "user:tui")
                {
                    tracing::warn!(task = %id, error = %e, "append_task_event failed");
                }
            }
            Ok(None) => {}
            Err(e) => {
                self.status_line = format!("move failed: {e}");
                return;
            }
        }
        self.status_line = format!("{id} → {new_col}");
        self.refresh();
        // Follow the card.
        self.selected_column = new_col_idx;
        if let Some(row) = self
            .column_tasks(new_col_idx)
            .iter()
            .position(|tf| tf.task.id == id)
        {
            self.selected_row = row;
        }
    }

    /// Bump selection up one slot inside its column (lower priority number).
    pub fn reorder_up(&mut self) {
        if self.selected_row == 0 {
            return;
        }
        self.reorder(self.selected_row - 1);
    }

    pub fn reorder_down(&mut self) {
        let n = self.column_tasks(self.selected_column).len();
        if self.selected_row + 1 >= n {
            return;
        }
        self.reorder(self.selected_row + 1);
    }

    fn reorder(&mut self, new_pos: usize) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        if let Err(e) = shelbi_state::set_task_priority(&self.project_name, &id, new_pos as u32) {
            self.status_line = format!("reorder failed: {e}");
            return;
        }
        self.refresh();
        self.selected_row = new_pos;
    }
}

// ---------------------------------------------------------------------------
// Rendering

pub fn render_full(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Min(1),    // columns
            Constraint::Length(2), // footer
        ])
        .split(area);

    let mut hits: Vec<CardHit> = Vec::new();
    render_title(f, app, outer[0]);
    render_columns(f, app, &mut hits, outer[1]);
    render_footer(f, app, outer[2]);
    app.card_hits = hits;

    // Dropdown sits above the columns but below the popover so a card
    // detail dialog can still open over the dropdown without rendering
    // chrome bleeding through.
    if app.worker_dropdown_is_open() {
        render_worker_dropdown(f, app, area);
    } else {
        // Stale hits from a previous open would route clicks to a row
        // that's no longer on screen — clear them every frame we render
        // without the dropdown.
        app.dropdown_hits.clear();
    }

    if app.popover_is_open() {
        render_popover(f, app, area);
    }
}

fn render_title(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    // Title row: project meta on the left, worker filter chip pinned
    // to the right. We compute the chip text first so the left split
    // knows exactly how many columns the chip will consume — the chip
    // never wraps and never truncates.
    let total = app.tasks.len();
    let chip_text = filter_chip_text(app);
    let chip_w = chip_text.chars().count() as u16;

    let (left_area, chip_area) = if area.width > chip_w {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(chip_w)])
            .split(area);
        (chunks[0], Some(chunks[1]))
    } else {
        // Title bar is too narrow to fit the chip — drop the chip and
        // give all the space to the left. The dropdown is still
        // reachable by hotkey; the chip is just a hint.
        (area, None)
    };

    let left = Line::from(vec![
        Span::styled("Tasks · ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            app.project_name.clone(),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {total} total"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(left), left_area);

    if let Some(chip_area) = chip_area {
        let chip_style = if app.worker_filter.is_some() {
            // Active filter — cyan to match the popover border + the
            // sidebar's project header, so the eye notices the board is
            // narrowed.
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(chip_text, chip_style))),
            chip_area,
        );
        app.filter_chip_hit = Some(chip_area);
    } else {
        app.filter_chip_hit = None;
    }
}

/// Single-line label shown in the right edge of the title bar. Leading
/// space keeps the chip from sitting flush against the rightmost
/// column. ▾ glyph hints that it's a dropdown affordance.
fn filter_chip_text(app: &KanbanApp) -> String {
    let label = match &app.worker_filter {
        None => "All".to_string(),
        Some(f) => f.label(),
    };
    format!(" Worker: {label} ▾")
}

fn render_columns(f: &mut Frame, app: &KanbanApp, hits: &mut Vec<CardHit>, area: Rect) {
    // Equal-width columns. With 5 columns at 20% each you get good
    // proportions down to ~50 cols wide; below that things squeeze, but
    // ratatui will still render — just less readable.
    let slots = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(vec![Constraint::Ratio(1, 5); 5])
        .split(area);

    let columns = app.task_columns();
    for (i, slot) in slots.iter().enumerate() {
        render_column(f, app, i, *slot, &columns, hits);
    }
}

fn render_column(
    f: &mut Frame,
    app: &KanbanApp,
    col_idx: usize,
    area: Rect,
    columns: &HashMap<String, Column>,
    hits: &mut Vec<CardHit>,
) {
    let column = app.column(col_idx);
    let tasks = app.column_tasks(col_idx);
    let focused = col_idx == app.selected_column;

    let header_style = if focused {
        Style::default()
            .fg(column_color(column))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(column_color(column))
    };
    let title_line = Line::from(vec![
        Span::styled(column_label(column), header_style),
        Span::styled(
            format!(" ({})", tasks.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    // Manual title/list split — keeps hit-test geometry unambiguous (no
    // dependence on Block.inner behavior with title + Borders::NONE).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let header_area = chunks[0];
    let list_area = chunks[1];

    f.render_widget(Paragraph::new(title_line), header_area);

    // Reserve a 2-char right gutter so adjacent cells don't visually
    // collide; List doesn't clip Line spans on its own.
    let max_text = area.width.saturating_sub(2) as usize;
    let mut items: Vec<ListItem> = Vec::with_capacity(tasks.len());
    for (row, tf) in tasks.iter().enumerate() {
        let blocked = tf.task.is_blocked(columns);
        // 2-char prefix when blocked so the badge can't visually melt into
        // the title text on narrow columns. Lock glyph stays in the same
        // slot whether or not other rows show one.
        let badge_prefix = if blocked { "🔒 " } else { "" };
        let title_text = truncate(&format!("{badge_prefix}{}", tf.task.title), max_text);
        let title_style = if blocked {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let mut lines = vec![Line::from(Span::styled(title_text, title_style))];
        let id_w = tf.task.id.chars().count();
        let worker_w = tf
            .task
            .assigned_to
            .as_deref()
            .map(|w| 3 + w.chars().count())
            .unwrap_or(0);
        let meta_line = if id_w + worker_w <= max_text {
            let mut spans = vec![Span::styled(
                tf.task.id.clone(),
                Style::default().fg(Color::DarkGray),
            )];
            if let Some(w) = &tf.task.assigned_to {
                spans.push(Span::raw("  "));
                spans.push(Span::styled(
                    format!("@{w}"),
                    Style::default().fg(Color::Magenta),
                ));
            }
            Line::from(spans)
        } else {
            Line::from(Span::styled(
                truncate(&tf.task.id, max_text),
                Style::default().fg(Color::DarkGray),
            ))
        };
        lines.push(meta_line);
        lines.push(Line::raw(""));

        let mut item = ListItem::new(lines);
        if focused && row == app.selected_row {
            item = item.style(
                Style::default()
                    .bg(Color::Blue)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
        }
        items.push(item);
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        ))));
    }

    let mut state = ListState::default();
    if focused && !tasks.is_empty() {
        state.select(Some(app.selected_row));
    }
    let list = List::new(items);
    f.render_stateful_widget(list, list_area, &mut state);

    // Each card item renders 3 lines (title, meta, blank). Clip the last
    // card if it overflows the visible list area so clicks far below it
    // don't count. Scroll offsets aren't tracked here — a long column that
    // scrolls past its visible area will mis-attribute clicks for any row
    // off-screen; tolerable until columns regularly exceed visible height.
    const ROWS_PER_CARD: u16 = 3;
    let list_bottom = list_area.y.saturating_add(list_area.height);
    for (row, _) in tasks.iter().enumerate() {
        let card_top = list_area.y.saturating_add(row as u16 * ROWS_PER_CARD);
        if card_top >= list_bottom {
            break;
        }
        let card_height = (list_bottom - card_top).min(ROWS_PER_CARD);
        hits.push(CardHit {
            area: Rect {
                x: list_area.x,
                y: card_top,
                width: list_area.width,
                height: card_height,
            },
            col_idx,
            row_idx: row,
        });
    }
}

fn render_footer(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let keys = Line::from(Span::styled(
        "  h/l col   j/k row   enter/␣ open   H/L move col   K/J reorder   f filter   r refresh",
        Style::default().fg(Color::DarkGray),
    ));
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

/// Render the worker filter dropdown as a small popover anchored under
/// the filter chip. The popover paints over the column headers + cards
/// (`Clear` strips whatever was underneath), but does NOT suppress the
/// title-row chip itself — keeping the chip visible while the dropdown
/// is open is the visual link between the two.
fn render_worker_dropdown(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    let opts = app.dropdown_options();
    if opts.is_empty() {
        // Defensive: an empty options list means there's nothing to
        // pick. Close it so a stale dropdown doesn't linger.
        app.close_worker_dropdown();
        return;
    }
    let cursor = app.worker_dropdown.as_ref().map(|d| d.cursor).unwrap_or(0);

    // Width = widest "Worker name (count)" row + 4 chars of chrome
    // (border + arrow + padding). Cap so the popover never spans more
    // than ~⅓ of the screen — narrow lists shouldn't sprawl.
    let max_label_w = opts
        .iter()
        .map(|o| dropdown_row_text(o).chars().count())
        .max()
        .unwrap_or(10);
    let desired_w = (max_label_w + 4).max(20) as u16;
    let popover_w = desired_w.min(area.width).min(area.width / 3 + 8);
    // Anchor right edge to the chip's right edge so the dropdown
    // visually drops out of the chip. Fall back to right-aligned in
    // the full area if the chip rect isn't recorded (narrow term).
    let popover_x = match app.filter_chip_hit {
        Some(chip) => {
            let right = chip.x.saturating_add(chip.width);
            right.saturating_sub(popover_w).max(area.x)
        }
        None => area.x.saturating_add(area.width).saturating_sub(popover_w),
    };
    // 2 lines of chrome (top + bottom borders), 1 footer hint, then
    // one line per option. Cap at the available terminal height so the
    // popover can't render off-screen.
    let popover_h = (opts.len() as u16 + 3).min(area.height.saturating_sub(1));
    // Place directly under the title row (area.y + 1) so the chip
    // remains visible above it.
    let popover_y = area.y.saturating_add(1).min(area.y + area.height.saturating_sub(popover_h));

    let popover_area = Rect {
        x: popover_x,
        y: popover_y,
        width: popover_w,
        height: popover_h,
    };

    f.render_widget(Clear, popover_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(vec![Span::styled(
            " Filter by worker ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )]));
    let inner = block.inner(popover_area);
    f.render_widget(block, popover_area);

    // Split inner: option list on top, single-line hint footer on bottom.
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let list_area = chunks[0];
    let hint_area = chunks[1];

    let mut hits: Vec<DropdownHit> = Vec::with_capacity(opts.len());
    let mut items: Vec<ListItem> = Vec::with_capacity(opts.len());
    for (idx, opt) in opts.iter().enumerate() {
        let active = app.worker_filter == opt.filter;
        let label = dropdown_row_text(opt);
        // Active filter gets a leading bullet so the user can tell at
        // a glance which row is currently applied. The selected row
        // (cursor) gets a Blue bg via List highlight_style below.
        let prefix = if active { "● " } else { "  " };
        let label_style = if active {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let line = Line::from(vec![
            Span::styled(prefix, Style::default().fg(Color::Cyan)),
            Span::styled(label, label_style),
        ]);
        items.push(ListItem::new(line));
        // Hit-test rect is one terminal row tall, full inner width.
        // List indents items by 0; the cursor highlight bg is applied
        // by the List widget so we don't need to model it here.
        let row_y = list_area.y.saturating_add(idx as u16);
        if row_y < list_area.y.saturating_add(list_area.height) {
            hits.push(DropdownHit {
                area: Rect {
                    x: list_area.x,
                    y: row_y,
                    width: list_area.width,
                    height: 1,
                },
                option_idx: idx,
            });
        }
    }

    let mut state = ListState::default();
    state.select(Some(cursor.min(opts.len().saturating_sub(1))));
    let list = List::new(items).highlight_style(
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    );
    f.render_stateful_widget(list, list_area, &mut state);

    let hint = Line::from(Span::styled(
        " ↑↓ nav · ↵ select · c clear · esc close",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(hint), hint_area);

    app.dropdown_hits = hits;
}

/// Single-row text for an option: `<label> (<count>)`. Used both for
/// layout (computing popover width) and for rendering, so the two stay
/// byte-identical.
fn dropdown_row_text(opt: &DropdownOption) -> String {
    let label = match &opt.filter {
        None => "All".to_string(),
        Some(f) => f.label(),
    };
    format!("{} ({})", label, opt.count)
}

fn column_label(c: Column) -> &'static str {
    match c {
        Column::Backlog => "BACKLOG",
        Column::Todo => "TO DO",
        Column::InProgress => "IN PROGRESS",
        Column::Review => "REVIEW",
        Column::Done => "DONE",
    }
}

fn column_color(c: Column) -> Color {
    match c {
        Column::Backlog => Color::Gray,
        Column::Todo => Color::Blue,
        Column::InProgress => Color::Yellow,
        Column::Review => Color::Magenta,
        Column::Done => Color::Green,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

// ---------------------------------------------------------------------------
// Task detail popover

fn render_popover(f: &mut Frame, app: &KanbanApp, area: Rect) {
    let popover_area = centered_rect(80, 80, area);

    // Clear underneath so the kanban columns don't bleed through.
    f.render_widget(Clear, popover_area);

    let columns = app.task_columns();
    let (header_lines, body_text, title) = match app.popover_task() {
        Some(tf) => (
            popover_header(tf, &columns),
            tf.body.clone(),
            tf.task.title.clone(),
        ),
        None => (
            Vec::new(),
            "(task no longer exists — press esc to close)".to_string(),
            "Missing task".to_string(),
        ),
    };

    let title_text = truncate(&title, popover_area.width.saturating_sub(4) as usize);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(vec![
            Span::styled(" ", Style::default()),
            Span::styled(
                title_text,
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(popover_area);
    f.render_widget(block, popover_area);

    // Layout inside the border: header (fixed height) → separator → body
    // (scrollable) → footer (hint line).
    let header_height = header_lines.len() as u16;
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(header_height),
            Constraint::Length(1), // separator
            Constraint::Min(1),    // body
            Constraint::Length(1), // hint
        ])
        .split(inner);

    if !header_lines.is_empty() {
        f.render_widget(Paragraph::new(header_lines), chunks[0]);
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "─".repeat(chunks[1].width as usize),
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[1],
    );

    let scroll = app.popover.as_ref().map(|p| p.scroll).unwrap_or(0);
    let body = Paragraph::new(body_text)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));
    f.render_widget(body, chunks[2]);

    let hint = Line::from(Span::styled(
        "  esc/enter/␣  close      j/k or ↑/↓  scroll      g  top",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(hint), chunks[3]);
}

fn popover_header(tf: &TaskFile, columns: &HashMap<String, Column>) -> Vec<Line<'static>> {
    let task = &tf.task;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(meta_row("id", &task.id));
    let col_label = column_label(task.column);
    let col_span = Span::styled(col_label.to_string(), Style::default().fg(column_color(task.column)));
    let mut col_line = vec![meta_label("column"), col_span];
    col_line.push(Span::raw("   "));
    col_line.push(meta_label("priority"));
    col_line.push(Span::raw(format!("{}", task.priority)));
    if let Some(w) = &task.assigned_to {
        col_line.push(Span::raw("   "));
        col_line.push(meta_label("worker"));
        col_line.push(Span::styled(
            format!("@{w}"),
            Style::default().fg(Color::Magenta),
        ));
    }
    lines.push(Line::from(col_line));

    if let Some(branch) = &task.branch {
        lines.push(meta_row("branch", branch));
    }

    if !task.depends_on.is_empty() {
        let blocked = task.is_blocked(columns);
        let label = if blocked { "blocked by" } else { "depends on" };
        let mut spans: Vec<Span<'static>> = vec![meta_label(label)];
        for (i, dep) in task.depends_on.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(", "));
            }
            let dep_col = columns.get(dep).copied();
            let label_text = match dep_col {
                Some(c) => format!("{dep} [{}]", c.as_str()),
                None => format!("{dep} [missing]"),
            };
            let color = match dep_col {
                Some(c) => column_color(c),
                None => Color::Red,
            };
            spans.push(Span::styled(label_text, Style::default().fg(color)));
        }
        lines.push(Line::from(spans));
    } else {
        // Only mention readiness when a user might wonder — i.e. never for
        // tasks with no deps. Skipping this keeps the header compact.
    }

    let created = task.created_at.format("%Y-%m-%d %H:%M UTC").to_string();
    let updated = task.updated_at.format("%Y-%m-%d %H:%M UTC").to_string();
    lines.push(Line::from(vec![
        meta_label("created"),
        Span::raw(created),
        Span::raw("   "),
        meta_label("updated"),
        Span::raw(updated),
    ]));

    lines
}

fn meta_label(label: &str) -> Span<'static> {
    Span::styled(
        format!("{label}: "),
        Style::default().fg(Color::DarkGray),
    )
}

fn meta_row(label: &str, value: &str) -> Line<'static> {
    Line::from(vec![meta_label(label), Span::raw(value.to_string())])
}

/// Return a `Rect` centered in `area` whose width and height are the given
/// percentages of the outer rect. Used as the bounds for the modal popover.
fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit(area: Rect, col_idx: usize, row_idx: usize) -> CardHit {
        CardHit { area, col_idx, row_idx }
    }

    fn task_file(id: &str, column: Column, priority: u32, updated: &str) -> TaskFile {
        task_file_for(id, column, priority, updated, None)
    }

    fn task_file_for(
        id: &str,
        column: Column,
        priority: u32,
        updated: &str,
        assigned_to: Option<&str>,
    ) -> TaskFile {
        let updated_at = chrono::DateTime::parse_from_rfc3339(updated)
            .unwrap()
            .with_timezone(&chrono::Utc);
        TaskFile {
            task: shelbi_core::Task {
                id: id.to_string(),
                title: id.to_string(),
                column,
                priority,
                assigned_to: assigned_to.map(|s| s.to_string()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: updated_at,
                updated_at,
                params: std::collections::BTreeMap::new(),
            },
            body: String::new(),
        }
    }

    const DONE_IDX: usize = 4;
    const BACKLOG_IDX: usize = 0;

    #[test]
    fn done_column_orders_newest_completed_first() {
        let mut app = KanbanApp::new("demo");
        // Insert in priority order; updated_at is intentionally out of order.
        app.tasks = vec![
            task_file("old", Column::Done, 0, "2026-06-20T10:00:00Z"),
            task_file("newest", Column::Done, 1, "2026-06-22T10:00:00Z"),
            task_file("middle", Column::Done, 2, "2026-06-21T10:00:00Z"),
        ];
        let ids: Vec<&str> = app
            .column_tasks(DONE_IDX)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["newest", "middle", "old"]);
    }

    #[test]
    fn non_done_columns_keep_priority_order() {
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![
            task_file("a", Column::Backlog, 0, "2026-06-20T10:00:00Z"),
            task_file("b", Column::Backlog, 1, "2026-06-22T10:00:00Z"),
            task_file("c", Column::Backlog, 2, "2026-06-21T10:00:00Z"),
        ];
        let ids: Vec<&str> = app
            .column_tasks(BACKLOG_IDX)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn done_sort_is_stable_for_equal_timestamps() {
        let mut app = KanbanApp::new("demo");
        // Equal updated_at → stable sort preserves the priority order, so a
        // re-poll of the same state never reshuffles (no flicker).
        app.tasks = vec![
            task_file("first", Column::Done, 0, "2026-06-20T10:00:00Z"),
            task_file("second", Column::Done, 1, "2026-06-20T10:00:00Z"),
            task_file("third", Column::Done, 2, "2026-06-20T10:00:00Z"),
        ];
        let ids: Vec<&str> = app
            .column_tasks(DONE_IDX)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(ids, vec!["first", "second", "third"]);
    }

    #[test]
    fn card_at_returns_first_matching_hit() {
        let mut app = KanbanApp::new("demo");
        app.card_hits = vec![
            hit(Rect { x: 0, y: 2, width: 20, height: 3 }, 0, 0),
            hit(Rect { x: 0, y: 5, width: 20, height: 3 }, 0, 1),
            hit(Rect { x: 20, y: 2, width: 20, height: 3 }, 1, 0),
        ];
        assert_eq!(app.card_at(5, 2), Some((0, 0)));
        assert_eq!(app.card_at(5, 4), Some((0, 0)));   // last row of card
        assert_eq!(app.card_at(5, 5), Some((0, 1)));   // first row of next card
        assert_eq!(app.card_at(25, 3), Some((1, 0)));  // adjacent column
    }

    #[test]
    fn card_at_misses_outside_any_rect() {
        let mut app = KanbanApp::new("demo");
        app.card_hits = vec![hit(Rect { x: 0, y: 2, width: 20, height: 3 }, 0, 0)];
        // Above the card.
        assert_eq!(app.card_at(5, 1), None);
        // Below the card.
        assert_eq!(app.card_at(5, 5), None);
        // Right of the card.
        assert_eq!(app.card_at(20, 3), None);
        // Empty hit list.
        app.card_hits.clear();
        assert_eq!(app.card_at(5, 3), None);
    }

    #[test]
    fn open_popover_at_moves_selection_and_opens() {
        let mut app = KanbanApp::new("demo");
        // No tasks → open_popover is a no-op even when called via the click
        // path, so the popover stays closed. Selection still moves.
        app.open_popover_at(2, 4);
        assert_eq!(app.selected_column, 2);
        assert_eq!(app.selected_row, 4);
        assert!(!app.popover_is_open());
    }

    /// Driving `move_card` should both persist the column change and append
    /// a `task=...` line to `~/.shelbi/events.log` so the orchestrator's
    /// live event tail picks up board nudges from the TUI.
    #[test]
    fn move_card_appends_user_tui_event() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-move-card-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        use chrono::Utc;
        let now = Utc::now();
        let task = shelbi_core::Task {
            id: "fix-login".into(),
            title: "fix login".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        };
        shelbi_state::save_task("demo", &task, "").unwrap();

        let mut app = KanbanApp::new("demo");
        app.refresh();
        // Place the cursor on `fix-login` in the todo column (index 1).
        app.selected_column = 1;
        app.selected_row = 0;
        app.move_card_right();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1, "log: {log:?}");
        // Workflow-aware shape (`Plans/workflows.md` §10): the line carries
        // `workflow=`, `reason=`, and trailing `from_category=` /
        // `to_category=` annotations.
        assert!(lines[0].contains(" task=fix-login "), "line: {}", lines[0]);
        assert!(lines[0].contains(" workflow=default "), "line: {}", lines[0]);
        assert!(
            lines[0].contains(" todo -> in_progress "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].contains(" reason=user:tui "), "line: {}", lines[0]);
        assert!(lines[0].ends_with(" to_category=active"), "line: {}", lines[0]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---- worker filter ---------------------------------------------------

    /// `column_tasks` applies both the column filter and the active
    /// worker filter — a worker filter shrinks every column at once,
    /// not just the focused one.
    #[test]
    fn column_tasks_applies_worker_filter() {
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![
            task_file_for("a", Column::Todo, 0, "2026-06-20T10:00:00Z", Some("alpha")),
            task_file_for("b", Column::Todo, 1, "2026-06-20T10:00:00Z", Some("bravo")),
            task_file_for("c", Column::InProgress, 0, "2026-06-20T10:00:00Z", Some("alpha")),
            task_file_for("d", Column::Todo, 2, "2026-06-20T10:00:00Z", None),
        ];
        // No filter — all tasks pass through their column filter.
        assert_eq!(app.column_tasks(1).len(), 3);
        assert_eq!(app.column_tasks(2).len(), 1);

        app.worker_filter = Some(WorkerFilter::Worker("alpha".into()));
        let todo: Vec<&str> = app.column_tasks(1).iter().map(|t| t.task.id.as_str()).collect();
        assert_eq!(todo, vec!["a"]);
        let wip: Vec<&str> = app.column_tasks(2).iter().map(|t| t.task.id.as_str()).collect();
        assert_eq!(wip, vec!["c"]);

        app.worker_filter = Some(WorkerFilter::Unassigned);
        let todo: Vec<&str> = app.column_tasks(1).iter().map(|t| t.task.id.as_str()).collect();
        assert_eq!(todo, vec!["d"], "Unassigned filter keeps only `assigned_to: None`");
    }

    /// `dropdown_options` builds `All` + each worker + (optional)
    /// `Unassigned`, in that order. Counts reflect the unfiltered task
    /// list so workers with zero matching cards still appear (the user
    /// has to be able to pick them to clear an unrelated filter).
    #[test]
    fn dropdown_options_lists_all_then_workers_then_unassigned() {
        let mut app = KanbanApp::new("demo");
        app.workers = vec!["alpha".into(), "bravo".into(), "charlie".into()];
        app.tasks = vec![
            task_file_for("a", Column::Todo, 0, "2026-06-20T10:00:00Z", Some("alpha")),
            task_file_for("b", Column::Todo, 1, "2026-06-20T10:00:00Z", Some("alpha")),
            task_file_for("c", Column::Todo, 2, "2026-06-20T10:00:00Z", Some("bravo")),
            task_file_for("d", Column::Todo, 3, "2026-06-20T10:00:00Z", None),
        ];
        let opts = app.dropdown_options();
        // All / alpha(2) / bravo(1) / charlie(0) / Unassigned(1)
        assert_eq!(opts.len(), 5);
        assert!(opts[0].filter.is_none());
        assert_eq!(opts[0].count, 4);
        assert_eq!(opts[1].filter, Some(WorkerFilter::Worker("alpha".into())));
        assert_eq!(opts[1].count, 2);
        assert_eq!(opts[3].filter, Some(WorkerFilter::Worker("charlie".into())));
        assert_eq!(opts[3].count, 0, "zero-count workers still appear");
        assert_eq!(opts[4].filter, Some(WorkerFilter::Unassigned));
        assert_eq!(opts[4].count, 1);
    }

    /// No tasks lack `assigned_to` → the Unassigned row is suppressed
    /// so the dropdown isn't padded with a useless option.
    #[test]
    fn dropdown_options_omits_unassigned_when_zero() {
        let mut app = KanbanApp::new("demo");
        app.workers = vec!["alpha".into()];
        app.tasks = vec![task_file_for(
            "a",
            Column::Todo,
            0,
            "2026-06-20T10:00:00Z",
            Some("alpha"),
        )];
        let opts = app.dropdown_options();
        assert!(
            !opts.iter().any(|o| o.filter == Some(WorkerFilter::Unassigned)),
            "no unassigned tasks → no Unassigned row, got {:?}",
            opts.iter().map(|o| &o.filter).collect::<Vec<_>>()
        );
    }

    /// Opening the dropdown seeds the cursor on the row matching the
    /// active filter — opening then immediately hitting Enter must be
    /// a no-op (idempotent). Up / Down wrap at either end.
    #[test]
    fn dropdown_open_seeds_cursor_on_active_filter_and_nav_wraps() {
        let mut app = KanbanApp::new("demo");
        app.workers = vec!["alpha".into(), "bravo".into()];
        app.tasks = vec![
            task_file_for("a", Column::Todo, 0, "2026-06-20T10:00:00Z", Some("alpha")),
            task_file_for("b", Column::Todo, 1, "2026-06-20T10:00:00Z", Some("bravo")),
        ];
        app.worker_filter = Some(WorkerFilter::Worker("bravo".into()));
        app.open_worker_dropdown();
        // Options are [All, alpha, bravo] → bravo is index 2.
        assert_eq!(app.worker_dropdown.as_ref().unwrap().cursor, 2);

        app.dropdown_nav_down();
        assert_eq!(app.worker_dropdown.as_ref().unwrap().cursor, 0, "wraps to top");
        app.dropdown_nav_up();
        assert_eq!(app.worker_dropdown.as_ref().unwrap().cursor, 2, "wraps to bottom");
    }

    /// An active filter that no longer matches any option (e.g. a
    /// worker was removed from project.yaml) seeds the cursor on `All`
    /// rather than panicking on an out-of-range index. The active
    /// filter itself isn't auto-cleared — only the cursor lands at 0.
    #[test]
    fn dropdown_open_falls_back_to_all_when_filter_missing_from_options() {
        let mut app = KanbanApp::new("demo");
        app.workers = vec!["alpha".into()];
        app.worker_filter = Some(WorkerFilter::Worker("removed".into()));
        app.open_worker_dropdown();
        assert_eq!(app.worker_dropdown.as_ref().unwrap().cursor, 0);
    }

    /// `apply_filter` (via `dropdown_select`) updates the in-memory
    /// state, persists to `state.json`, and a fresh app picks it up on
    /// refresh. The disk format stores the sentinel for Unassigned so
    /// the schema stays a plain `Option<String>`.
    #[test]
    fn apply_filter_persists_to_state_json_and_refresh_restores() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-worker-filter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // Project needs to exist so refresh() can populate workers.
        let project = shelbi_core::Project {
            name: "demo".into(),
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            git: shelbi_core::GitConfig::default(),
            machines: vec![shelbi_core::Machine {
                name: "hub".into(),
                kind: shelbi_core::MachineKind::Local,
                work_dir: "/tmp/demo".into(),
                host: None,
            }],
            orchestrator: shelbi_core::OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: {
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "claude".into(),
                    shelbi_core::AgentRunnerSpec {
                        command: "claude".into(),
                        flags: vec![],
                    },
                );
                m
            },
            editor: None,
            github_url: None,
            workers: vec![
                shelbi_core::WorkerSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                },
                shelbi_core::WorkerSpec {
                    name: "bravo".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                },
            ],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        };
        shelbi_state::save_project(&project).unwrap();

        let mut app = KanbanApp::new("demo");
        app.refresh();
        assert!(app.worker_filter.is_none(), "fresh state.json → no filter");
        assert_eq!(app.workers, vec!["alpha".to_string(), "bravo".to_string()]);

        // Open, navigate to the bravo row (All=0, alpha=1, bravo=2),
        // commit. The dropdown closes itself.
        app.open_worker_dropdown();
        app.dropdown_nav_down();
        app.dropdown_nav_down();
        app.dropdown_select();
        assert!(!app.worker_dropdown_is_open());
        assert_eq!(app.worker_filter, Some(WorkerFilter::Worker("bravo".into())));

        // A fresh app rehydrates the same filter from disk.
        let mut app2 = KanbanApp::new("demo");
        app2.refresh();
        assert_eq!(app2.worker_filter, Some(WorkerFilter::Worker("bravo".into())));

        // Unassigned round-trips through the sentinel.
        app2.open_worker_dropdown();
        // Options: All / alpha / bravo / Unassigned? — we need an
        // unassigned task to surface it. Seed one and refresh.
        let now = chrono::Utc::now();
        shelbi_state::save_task(
            "demo",
            &shelbi_core::Task {
                id: "orphan".into(),
                title: "orphan".into(),
                column: Column::Backlog,
                priority: 0,
                assigned_to: None,
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                params: Default::default(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
            },
            "",
        )
        .unwrap();
        app2.refresh();
        app2.open_worker_dropdown();
        let opts = app2.dropdown_options();
        let unassigned_idx = opts
            .iter()
            .position(|o| o.filter == Some(WorkerFilter::Unassigned))
            .expect("Unassigned row should now appear");
        if let Some(d) = app2.worker_dropdown.as_mut() {
            d.cursor = unassigned_idx;
        }
        app2.dropdown_select();
        assert_eq!(app2.worker_filter, Some(WorkerFilter::Unassigned));
        let on_disk = shelbi_state::read_state("demo").unwrap();
        assert_eq!(
            on_disk.worker_filter.as_deref(),
            Some(WorkerFilter::UNASSIGNED_SENTINEL),
            "Unassigned must serialize to its sentinel string"
        );

        // `dropdown_clear` resets to None and writes that through to
        // disk so a subsequent refresh sees the cleared filter.
        app2.open_worker_dropdown();
        app2.dropdown_clear();
        assert_eq!(app2.worker_filter, None);
        let on_disk = shelbi_state::read_state("demo").unwrap();
        assert!(on_disk.worker_filter.is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Selecting the `All` row clears the filter even when it was
    /// previously set to a specific worker — the dropdown commit path
    /// has to accept `None` as a valid choice.
    #[test]
    fn dropdown_select_all_clears_filter() {
        let mut app = KanbanApp::new("demo");
        app.workers = vec!["alpha".into()];
        app.worker_filter = Some(WorkerFilter::Worker("alpha".into()));
        app.open_worker_dropdown();
        // Cursor seeds on alpha (idx 1). Walk back to All.
        app.dropdown_nav_up();
        assert_eq!(app.worker_dropdown.as_ref().unwrap().cursor, 0);
        // No disk write here because no SHELBI_HOME is set — the
        // persist error lands in status_line but the in-memory state
        // still updates.
        app.dropdown_select();
        assert!(app.worker_filter.is_none());
        assert!(!app.worker_dropdown_is_open());
    }

    /// Toggling the dropdown closes it when open, opens when closed —
    /// matches the chord-style "press the same key to dismiss" UX.
    #[test]
    fn toggle_worker_dropdown_is_self_inverse() {
        let mut app = KanbanApp::new("demo");
        assert!(!app.worker_dropdown_is_open());
        app.toggle_worker_dropdown();
        assert!(app.worker_dropdown_is_open());
        app.toggle_worker_dropdown();
        assert!(!app.worker_dropdown_is_open());
    }

    /// Rendering the kanban with the dropdown open must show the
    /// chip in the title row, paint every worker option (plus All), and
    /// populate `dropdown_hits` so click routing has rects to test
    /// against.
    #[test]
    fn rendering_dropdown_paints_options_and_chip() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        app.workers = vec!["alpha".into(), "bravo".into()];
        app.tasks = vec![
            task_file_for("a", Column::Todo, 0, "2026-06-20T10:00:00Z", Some("alpha")),
            task_file_for("b", Column::Todo, 1, "2026-06-20T10:00:00Z", Some("bravo")),
        ];
        app.open_worker_dropdown();

        // Wide enough to hold the chip on the right and the dropdown.
        let backend = TestBackend::new(80, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let buf = term.backend().buffer().clone();
        let rendered: Vec<String> = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect();
        let joined = rendered.join("\n");

        assert!(joined.contains("Worker: All ▾"), "chip missing in:\n{joined}");
        assert!(joined.contains("Filter by worker"), "dropdown title missing");
        assert!(joined.contains("All (2)"), "All row missing in:\n{joined}");
        assert!(joined.contains("alpha (1)"), "alpha row missing in:\n{joined}");
        assert!(joined.contains("bravo (1)"), "bravo row missing in:\n{joined}");

        assert!(app.filter_chip_hit.is_some(), "chip rect not recorded");
        assert_eq!(
            app.dropdown_hits.len(),
            3,
            "expected 3 dropdown hits (All + 2 workers), got {}",
            app.dropdown_hits.len()
        );
    }

    /// `dropdown_option_at` and `filter_chip_at` are pure hit-tests
    /// against the most recently rendered geometry — no rendering
    /// required for the assertion.
    #[test]
    fn hit_tests_route_to_recorded_rects() {
        let mut app = KanbanApp::new("demo");
        app.filter_chip_hit = Some(Rect {
            x: 50,
            y: 0,
            width: 18,
            height: 1,
        });
        app.dropdown_hits = vec![
            DropdownHit {
                area: Rect {
                    x: 50,
                    y: 1,
                    width: 18,
                    height: 1,
                },
                option_idx: 0,
            },
            DropdownHit {
                area: Rect {
                    x: 50,
                    y: 2,
                    width: 18,
                    height: 1,
                },
                option_idx: 1,
            },
        ];
        assert!(app.filter_chip_at(55, 0));
        assert!(!app.filter_chip_at(40, 0));
        assert!(!app.filter_chip_at(55, 1), "chip lives on y=0 only");
        assert_eq!(app.dropdown_option_at(55, 1), Some(0));
        assert_eq!(app.dropdown_option_at(55, 2), Some(1));
        assert_eq!(app.dropdown_option_at(10, 1), None, "miss outside chip x range");
    }
}
