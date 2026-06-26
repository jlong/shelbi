//! Kanban Tasks view — rendered into the dashboard's right pane via the
//! same hidden-pane swap mechanism the other built-in views use. State is
//! read from / written to the task markdown files via `shelbi_state`; no
//! separate cache.
//!
//! In "All" mode the board renders the **union of every loaded workflow's
//! `(workflow, status)` pairs** as columns, in workflow-declaration order
//! (workflows themselves sorted by name via [`shelbi_state::list_workflows`]).
//! A project with only the default workflow looks identical to the old
//! 5-column flow; a project with a second workflow gets that workflow's
//! statuses appended to the right. Each task is bucketed into the column
//! for `(task.workflow_or_default(), task.column)` — see
//! [`resolve_task_status`].
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
use shelbi_core::{
    default_workflow, Column, StatusCategory, Task, Workflow, DEFAULT_WORKFLOW_NAME,
};
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
    /// Loaded workflows, in `list_workflows` order (alphabetical by name,
    /// with `default` last when present unless another workflow sorts
    /// after it lexicographically). Reloaded each [`refresh`], so an
    /// edited `workflows/<name>.yaml` shows up on the next poll without
    /// a respawn. Empty when the loader errors — the board degrades to
    /// the canonical default workflow via [`KanbanApp::workflows_or_default`].
    pub workflows: Vec<Workflow>,
    /// The All-mode column list: one entry per `(workflow, status)` pair
    /// taken from every loaded workflow, in declared order. Selection
    /// indices ([`KanbanApp::selected_column`]) refer to slots in this
    /// vec, and the renderer walks it left-to-right. Rebuilt each
    /// refresh from [`KanbanApp::workflows`].
    pub all_columns: Vec<WorkflowColumn>,
    /// Active workflow filter — `None` means "All workflows" (the union
    /// All-mode view). When `Some(name)`, [`KanbanApp::all_columns`] is
    /// narrowed to that single workflow's columns and tasks belonging
    /// to other workflows drop out of the board. Persisted to
    /// `state.json::workflow_filter` so the chip survives a respawn.
    pub workflow_filter: Option<String>,
    /// When `Some`, the workflow filter dropdown is open. Same
    /// modal-cursor shape as [`KanbanApp::worker_dropdown`] — selection
    /// commits on Enter / click.
    pub workflow_dropdown: Option<WorkflowDropdown>,
    /// Screen-space rect of the workflow filter chip in the title row,
    /// captured each frame by the renderer so a click on the chip can
    /// open the dropdown without keyboard.
    pub workflow_chip_hit: Option<Rect>,
    /// Hit-test entries for the open workflow dropdown's option rows.
    /// Empty when the dropdown is closed.
    pub workflow_dropdown_hits: Vec<DropdownHit>,
    /// Leftmost visible column index. Stays at 0 unless the full
    /// `all_columns` list can't fit at minimum width — then the renderer
    /// scrolls horizontally to keep the selected column on screen.
    /// Tracked on the app (not the renderer) so frames stay stable as
    /// selection moves.
    pub column_scroll: usize,
    /// Last-rendered horizontal scroll window: the slice of
    /// `all_columns` that was actually drawn this frame, recorded so
    /// selection-driven scrolling can ask "is the selected column
    /// currently visible?" without redoing the layout pass.
    pub visible_columns: std::ops::Range<usize>,
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

/// State carried while the workflow filter dropdown is open. Mirrors
/// [`WorkerDropdown`] — just a cursor — but split into its own struct
/// so the two dropdowns can't be confused at the call site and so a
/// future workflow-only field (e.g. a filter substring) has somewhere
/// to live.
#[derive(Debug, Clone)]
pub struct WorkflowDropdown {
    /// Cursor row inside the workflow options list; seeded from the
    /// active filter by [`KanbanApp::open_workflow_dropdown`].
    pub cursor: usize,
}

/// One entry rendered in the workflow filter dropdown. `None` is the
/// "All workflows" reset row; `Some(name)` filters the board to that
/// workflow's columns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowDropdownOption {
    pub filter: Option<String>,
    /// How many tasks currently sit in this workflow (or the whole
    /// board, for the `All` row). Computed at render time against the
    /// unfiltered tasks slice so an empty workflow can still be picked.
    pub count: usize,
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

/// One column in the All-mode kanban view — a `(workflow, status)` pair.
///
/// Each loaded workflow contributes one [`WorkflowColumn`] per status it
/// declares, in declared order. The status's [`StatusCategory`] is mirrored
/// here so:
///
/// - the renderer can colour the header using the same palette the legacy
///   5-column flow uses (Backlog grey, Ready blue, Active yellow…), and
/// - the move handler can map a target column back to the legacy
///   [`Column`] enum that `shelbi_state::move_task` still takes — see
///   [`category_to_column`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowColumn {
    /// Workflow name this column belongs to. Matches the workflow's
    /// YAML `name:` field. Used both to bucket tasks and to render the
    /// subscript label under the status name when more than one
    /// workflow is loaded.
    pub workflow: String,
    /// Status name as declared in the workflow YAML. Doubles as the
    /// header label rendered above the column.
    pub status_name: String,
    /// Semantic category the status reports — controls header colour
    /// and the legacy-column mapping used when moving cards.
    pub category: StatusCategory,
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
        // Seed `all_columns` with the canonical default workflow so a
        // brand-new app pre-`refresh` already paints a board (and
        // exercises the same code path as a refresh that found exactly
        // one default workflow on disk). Tests that drive the app
        // directly without calling `refresh` also benefit — they don't
        // have to populate `all_columns` by hand to navigate.
        let default = default_workflow();
        let all_columns = workflow_columns(std::slice::from_ref(&default));
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
            workflows: vec![default],
            all_columns,
            workflow_filter: None,
            workflow_dropdown: None,
            workflow_chip_hit: None,
            workflow_dropdown_hits: Vec::new(),
            column_scroll: 0,
            visible_columns: 0..0,
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

    /// Resolved `(workflow, status)` pair for column slot `idx`. Returns
    /// the last slot when `idx` overshoots — the same clamp-to-end the
    /// legacy `Column::ALL[idx.min(_)]` implementation had — so callers
    /// that pass a stale selection index still get a usable column.
    /// Never returns `None`: `all_columns` is seeded with the default
    /// workflow in [`Self::new`] and rebuilt to at least the canonical
    /// default on every refresh.
    pub fn column(&self, idx: usize) -> &WorkflowColumn {
        let last = self.all_columns.len().saturating_sub(1);
        &self.all_columns[idx.min(last)]
    }

    pub fn column_tasks(&self, col_idx: usize) -> Vec<&TaskFile> {
        let ac = self.column(col_idx);
        let mut tasks: Vec<&TaskFile> = self
            .tasks
            .iter()
            .filter(|tf| self.task_matches_filter(tf))
            .filter(|tf| self.task_belongs_to(&tf.task, ac))
            .collect();
        // Done shows most-recently-completed first: `updated_at` is rewritten
        // on the move-into-done write, so it's a stable proxy for completion
        // time. Other columns keep their priority order (the natural order of
        // `self.tasks`). Stable sort → equal timestamps preserve that order,
        // so identical-state polls don't reshuffle / flicker.
        if ac.category == StatusCategory::Done {
            tasks.sort_by(|a, b| b.task.updated_at.cmp(&a.task.updated_at));
        }
        tasks
    }

    /// True when `task` lives in the All-mode column `ac` — i.e. its
    /// workflow matches and its column resolves to `ac.status_name`
    /// inside that workflow. Tasks whose declared workflow is missing
    /// from `self.workflows` are treated as belonging to the default
    /// workflow, so they still show up somewhere instead of vanishing.
    fn task_belongs_to(&self, task: &Task, ac: &WorkflowColumn) -> bool {
        let task_wf_name = task.workflow_or_default();
        let resolved_wf_name = if self.workflows.iter().any(|w| w.name == task_wf_name) {
            task_wf_name
        } else {
            DEFAULT_WORKFLOW_NAME
        };
        if resolved_wf_name != ac.workflow {
            return false;
        }
        let Some(wf) = self.workflows.iter().find(|w| w.name == ac.workflow) else {
            return false;
        };
        resolve_task_status(task, wf) == ac.status_name
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
        let state_snapshot = shelbi_state::read_state(&self.project_name).ok();
        self.worker_filter = state_snapshot
            .as_ref()
            .and_then(|s| s.worker_filter.clone())
            .map(|s| WorkerFilter::from_disk(&s));
        self.workflow_filter = state_snapshot
            .as_ref()
            .and_then(|s| s.workflow_filter.clone());
        // Workflows drive the All-mode column layout. A broken
        // `workflows/<name>.yaml` surfaces as a status_line warning and
        // we fall back to the canonical default so the board still
        // paints — better than a blank screen while the user fixes the
        // YAML.
        self.workflows = match shelbi_state::list_workflows(&self.project_name) {
            Ok(wfs) => wfs,
            Err(e) => {
                self.status_line = format!("workflow load failed: {e}");
                vec![default_workflow()]
            }
        };
        self.all_columns = self.compute_all_columns();
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

    // ----- workflow filter --------------------------------------------------

    /// Build [`Self::all_columns`] honouring the active workflow
    /// filter. With no filter set this is the union across every loaded
    /// workflow (the All-mode behaviour). With a filter set, only that
    /// workflow's columns participate — if the filter targets a
    /// workflow that no longer exists in `self.workflows`, the columns
    /// fall back to the union so the board doesn't go blank and the
    /// user can still see the chip to clear it.
    fn compute_all_columns(&self) -> Vec<WorkflowColumn> {
        let filtered: Vec<&Workflow> = match &self.workflow_filter {
            Some(name) => self.workflows.iter().filter(|w| &w.name == name).collect(),
            None => self.workflows.iter().collect(),
        };
        if filtered.is_empty() {
            // Filter doesn't match any loaded workflow — degrade to the
            // unfiltered union so the user keeps a usable board.
            return workflow_columns_from_refs(&self.workflows.iter().collect::<Vec<_>>());
        }
        workflow_columns_from_refs(&filtered)
    }

    /// Options for the workflow dropdown: `All` (count = all tasks),
    /// then each loaded workflow with its task count. A workflow with
    /// zero matching tasks still appears so the user can pick it (the
    /// scaffolding behaviour the worker dropdown uses for the same
    /// reason).
    pub fn workflow_dropdown_options(&self) -> Vec<WorkflowDropdownOption> {
        let mut opts: Vec<WorkflowDropdownOption> =
            Vec::with_capacity(self.workflows.len() + 1);
        opts.push(WorkflowDropdownOption {
            filter: None,
            count: self.tasks.len(),
        });
        for w in &self.workflows {
            let count = self
                .tasks
                .iter()
                .filter(|tf| tf.task.workflow_or_default() == w.name)
                .count();
            opts.push(WorkflowDropdownOption {
                filter: Some(w.name.clone()),
                count,
            });
        }
        opts
    }

    fn current_workflow_filter_idx(&self, opts: &[WorkflowDropdownOption]) -> usize {
        opts.iter()
            .position(|o| o.filter == self.workflow_filter)
            .unwrap_or(0)
    }

    pub fn workflow_dropdown_is_open(&self) -> bool {
        self.workflow_dropdown.is_some()
    }

    pub fn open_workflow_dropdown(&mut self) {
        let opts = self.workflow_dropdown_options();
        let cursor = self.current_workflow_filter_idx(&opts);
        self.workflow_dropdown = Some(WorkflowDropdown { cursor });
    }

    pub fn close_workflow_dropdown(&mut self) {
        self.workflow_dropdown = None;
    }

    pub fn toggle_workflow_dropdown(&mut self) {
        if self.workflow_dropdown_is_open() {
            self.close_workflow_dropdown();
        } else {
            self.open_workflow_dropdown();
        }
    }

    pub fn workflow_dropdown_nav_up(&mut self) {
        let n = self.workflow_dropdown_options().len();
        if n == 0 {
            return;
        }
        if let Some(d) = self.workflow_dropdown.as_mut() {
            d.cursor = if d.cursor == 0 { n - 1 } else { d.cursor - 1 };
        }
    }

    pub fn workflow_dropdown_nav_down(&mut self) {
        let n = self.workflow_dropdown_options().len();
        if n == 0 {
            return;
        }
        if let Some(d) = self.workflow_dropdown.as_mut() {
            d.cursor = (d.cursor + 1) % n;
        }
    }

    pub fn workflow_dropdown_select(&mut self) {
        let opts = self.workflow_dropdown_options();
        let Some(d) = self.workflow_dropdown.as_ref() else {
            return;
        };
        let Some(opt) = opts.get(d.cursor) else {
            self.close_workflow_dropdown();
            return;
        };
        self.apply_workflow_filter(opt.filter.clone());
        self.close_workflow_dropdown();
    }

    pub fn workflow_dropdown_clear(&mut self) {
        self.apply_workflow_filter(None);
        self.close_workflow_dropdown();
    }

    /// Persist `filter` as the active workflow filter, rebuild
    /// `all_columns`, and clamp selection so it lands on an existing
    /// column after the layout shrinks. Mirrors [`apply_filter`] for
    /// the worker filter.
    fn apply_workflow_filter(&mut self, filter: Option<String>) {
        self.workflow_filter = filter.clone();
        if let Err(e) =
            shelbi_state::set_workflow_filter(&self.project_name, filter.as_deref())
        {
            self.status_line = format!("workflow filter persist failed: {e}");
        }
        self.all_columns = self.compute_all_columns();
        // Selection may sit past the new column count — clamp before
        // the next render reads it. Selection-driven scroll updates
        // happen on the next render pass.
        let last = self.all_columns.len().saturating_sub(1);
        if self.selected_column > last {
            self.selected_column = last;
        }
        self.column_scroll = 0;
        self.clamp_selection();
        self.status_line = match &self.workflow_filter {
            None => "workflow: all".to_string(),
            Some(w) => format!("workflow: {w}"),
        };
    }

    pub fn workflow_dropdown_option_at(&self, x: u16, y: u16) -> Option<usize> {
        self.workflow_dropdown_hits.iter().find_map(|hit| {
            let r = hit.area;
            let in_x = x >= r.x && x < r.x.saturating_add(r.width);
            let in_y = y >= r.y && y < r.y.saturating_add(r.height);
            (in_x && in_y).then_some(hit.option_idx)
        })
    }

    pub fn workflow_chip_at(&self, x: u16, y: u16) -> bool {
        match self.workflow_chip_hit {
            Some(r) => {
                let in_x = x >= r.x && x < r.x.saturating_add(r.width);
                let in_y = y >= r.y && y < r.y.saturating_add(r.height);
                in_x && in_y
            }
            None => false,
        }
    }

    /// Adjust [`Self::column_scroll`] so the selected column will be
    /// inside the next render's visible window. Called by `render_full`
    /// before the layout pass with the actual area width.
    ///
    /// Two passes — first widen leftward (scroll left if the selection
    /// fell off the left edge), then widen rightward (scroll right if
    /// the selection won't fit in the current window). Together they
    /// keep `column_scroll <= selected_column < scroll + visible_len`.
    pub fn ensure_selected_visible(&mut self, area_w: u16) {
        if self.all_columns.is_empty() {
            self.column_scroll = 0;
            return;
        }
        let sel = self.selected_column.min(self.all_columns.len() - 1);
        if self.column_scroll > sel {
            self.column_scroll = sel;
        }
        // Walk right: keep incrementing column_scroll until the
        // selected column fits inside the window the layout pass will
        // build. `compute_column_widths` is the source of truth for
        // which columns fit, so we drive the loop with it.
        let counts: Vec<usize> = (0..self.all_columns.len())
            .map(|i| self.column_tasks(i).len())
            .collect();
        loop {
            let widths =
                compute_column_widths(&self.all_columns, &counts, self.column_scroll, area_w);
            let last_visible = widths.last().map(|(i, _)| *i);
            match last_visible {
                Some(idx) if sel <= idx => break,
                Some(_) => {
                    self.column_scroll = self.column_scroll.saturating_add(1);
                    if self.column_scroll >= self.all_columns.len() {
                        self.column_scroll = self.all_columns.len() - 1;
                        break;
                    }
                }
                None => break,
            }
        }
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
        let n = self.all_columns.len().max(1);
        if self.selected_column == 0 {
            self.selected_column = n - 1;
        } else {
            self.selected_column -= 1;
        }
        self.clamp_selection();
    }

    pub fn nav_right(&mut self) {
        let n = self.all_columns.len().max(1);
        self.selected_column = (self.selected_column + 1) % n;
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

    /// Shove the selected card one column to the left **within its
    /// current workflow**, wrapping at the workflow's first status.
    /// Cards never jump between workflows here — moving a card carries
    /// its `task.workflow` along, so wrapping inside the workflow
    /// matches the semantic the user expects.
    pub fn move_card_left(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let Some(new_col_idx) = self.adjacent_column_in_same_workflow(self.selected_column, false)
        else {
            return;
        };
        self.move_card(&id, new_col_idx);
    }

    pub fn move_card_right(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let Some(new_col_idx) = self.adjacent_column_in_same_workflow(self.selected_column, true)
        else {
            return;
        };
        self.move_card(&id, new_col_idx);
    }

    /// Find the index of the slot immediately before/after `from_idx`
    /// that belongs to the same workflow, wrapping around within that
    /// workflow's contiguous slice. Returns `None` when the workflow
    /// has only one status (nowhere to move to) or `from_idx` is out
    /// of range.
    fn adjacent_column_in_same_workflow(
        &self,
        from_idx: usize,
        forward: bool,
    ) -> Option<usize> {
        let from = self.all_columns.get(from_idx)?;
        // Collect the indices of every column belonging to the same
        // workflow. `all_columns` keeps each workflow's statuses
        // contiguous (see `workflow_columns`) so this is a tight
        // range in practice, but iterating defensively keeps us
        // correct if the invariant ever slips.
        let same_wf: Vec<usize> = self
            .all_columns
            .iter()
            .enumerate()
            .filter(|(_, c)| c.workflow == from.workflow)
            .map(|(i, _)| i)
            .collect();
        if same_wf.len() <= 1 {
            return None;
        }
        let pos = same_wf.iter().position(|&i| i == from_idx)?;
        let next = if forward {
            (pos + 1) % same_wf.len()
        } else if pos == 0 {
            same_wf.len() - 1
        } else {
            pos - 1
        };
        Some(same_wf[next])
    }

    fn move_card(&mut self, id: &str, new_col_idx: usize) {
        // `shelbi_state::move_task` still takes the legacy 5-column
        // enum; map the workflow column's semantic category back to
        // the matching Column so the on-disk task file lands in the
        // right bucket. For default-workflow status names this is a
        // pure identity; for custom workflows the category is the
        // bridge (`Plans/workflows.md` §1).
        let target_col = self.column(new_col_idx).clone();
        let new_col = category_to_column(target_col.category);
        // Lifecycle hook: when a move actually transitions a task INTO
        // `in_progress`, cut its branch on the hub (depends_on aware) and
        // persist `branch:` first — see `shelbi_orchestrator::lifecycle`.
        // If the cut fails (e.g. depends_on names a branch that doesn't
        // exist locally yet) we bail without moving the card so the YAML
        // and the git refs stay consistent.
        if matches!(new_col, Column::InProgress) {
            if let Some(tf) = self
                .column_tasks(self.selected_column)
                .iter()
                .find(|tf| tf.task.id == id)
            {
                if tf.task.column != Column::InProgress {
                    match shelbi_state::load_project(&self.project_name) {
                        Ok(project) => {
                            if let Err(e) =
                                shelbi_orchestrator::lifecycle::ensure_branch_for_in_progress(
                                    &project, id,
                                )
                            {
                                self.status_line = format!("branch cut failed: {e}");
                                return;
                            }
                        }
                        Err(e) => {
                            self.status_line = format!("load project failed: {e}");
                            return;
                        }
                    }
                }
            }
        }
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
        self.status_line = format!("{id} → {}", target_col.status_name);
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
    // Selection-driven horizontal scroll: nudge the scroll offset so
    // the selected column lands inside the visible window before the
    // column-layout pass reads it. Done here (not in `nav_left/right`)
    // so the layout has the final area width to work with.
    app.ensure_selected_visible(outer[1].width);
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
    if app.workflow_dropdown_is_open() {
        render_workflow_dropdown(f, app, area);
    } else {
        app.workflow_dropdown_hits.clear();
    }

    if app.popover_is_open() {
        render_popover(f, app, area);
    }
}

fn render_title(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    // Title row: project meta on the left, filter chips pinned to the
    // right. Two chips when there are multiple workflows loaded (so a
    // workflow filter is meaningful); otherwise just the worker chip.
    // Chip widths are precomputed so the left split knows exactly how
    // many columns the chips will consume — chips never wrap.
    let total = app.tasks.len();
    let worker_text = filter_chip_text(app);
    let workflow_text = workflow_chip_text(app);
    let show_workflow_chip = app.workflows.len() > 1;
    let worker_w = worker_text.chars().count() as u16;
    let workflow_w = workflow_text.chars().count() as u16;
    let total_chip_w = if show_workflow_chip {
        worker_w + workflow_w
    } else {
        worker_w
    };

    let (left_area, workflow_area, worker_area) = if area.width > total_chip_w {
        let mut constraints = Vec::with_capacity(3);
        constraints.push(Constraint::Min(0));
        if show_workflow_chip {
            constraints.push(Constraint::Length(workflow_w));
        }
        constraints.push(Constraint::Length(worker_w));
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints(constraints)
            .split(area);
        if show_workflow_chip {
            (chunks[0], Some(chunks[1]), Some(chunks[2]))
        } else {
            (chunks[0], None, Some(chunks[1]))
        }
    } else {
        // Title bar is too narrow for the chips — drop them and give
        // all the space to the left. Dropdowns remain reachable by
        // hotkey; the chips are just a hint.
        (area, None, None)
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

    if let Some(area) = workflow_area {
        let style = if app.workflow_filter.is_some() {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(workflow_text, style))),
            area,
        );
        app.workflow_chip_hit = Some(area);
    } else {
        app.workflow_chip_hit = None;
    }

    if let Some(area) = worker_area {
        let style = if app.worker_filter.is_some() {
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(worker_text, style))),
            area,
        );
        app.filter_chip_hit = Some(area);
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

/// Workflow filter chip text — sits left of the worker chip when more
/// than one workflow is loaded. Identical shape to [`filter_chip_text`]
/// so the two chips line up visually.
fn workflow_chip_text(app: &KanbanApp) -> String {
    let label = match &app.workflow_filter {
        None => "All".to_string(),
        Some(name) => name.clone(),
    };
    format!(" Workflow: {label} ▾")
}

fn render_columns(f: &mut Frame, app: &mut KanbanApp, hits: &mut Vec<CardHit>, area: Rect) {
    // Layout pass: decide which columns are visible and how wide each
    // gets. Two refinements over the previous equal-fraction layout:
    //
    //   1. **Collapse empty** — a column with zero matching cards
    //      shrinks to a fixed minimum width so cards-bearing columns
    //      get the screen real estate. Without this an All-mode board
    //      across two workflows would slice the screen into many empty
    //      lanes the eye has to scan past.
    //
    //   2. **Horizontal scroll** — when the minimum total width
    //      overflows the available area, render only a window starting
    //      at `column_scroll` and walk forward until the next column
    //      no longer fits. Selection-driven scroll (run before this
    //      pass) keeps the selected column inside that window.
    if app.all_columns.is_empty() {
        // Nothing to render — typically a stale workflow_filter that
        // doesn't match any loaded workflow. The compute_all_columns
        // fallback should keep us out of this branch in practice.
        return;
    }
    let counts: Vec<usize> = (0..app.all_columns.len())
        .map(|i| app.column_tasks(i).len())
        .collect();
    let widths = compute_column_widths(&app.all_columns, &counts, app.column_scroll, area.width);
    let visible_start = app.column_scroll;
    let visible_end = visible_start + widths.len();

    let columns = app.task_columns();
    // The workflow subscript only adds value when the rendered slice
    // actually mixes workflows — narrowing to one workflow (via filter
    // or via a single-workflow project) means every visible column
    // already shares a workflow, so the subscript would be redundant.
    let visible_workflows: std::collections::HashSet<&str> = app
        .all_columns
        .get(visible_start..visible_end)
        .unwrap_or(&[])
        .iter()
        .map(|c| c.workflow.as_str())
        .collect();
    let show_workflow_label = visible_workflows.len() > 1;
    let mut x = area.x;
    for (i, w) in widths.iter().copied() {
        let slot = Rect {
            x,
            y: area.y,
            width: w,
            height: area.height,
        };
        let collapsed = counts.get(i).copied().unwrap_or(0) == 0;
        render_column(
            f,
            app,
            i,
            slot,
            &columns,
            hits,
            show_workflow_label,
            collapsed,
        );
        x = x.saturating_add(w);
    }

    // Tiny horizontal-scroll indicators painted in the topmost row of
    // the columns area when the board can't fit everything at minimum
    // width. Anchored to the visible window's edges, not the area, so
    // the user can tell which side they can scroll toward.
    if visible_start > 0 {
        let glyph = Span::styled(
            "◂",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        );
        let pos = Rect {
            x: area.x,
            y: area.y,
            width: 1,
            height: 1,
        };
        f.render_widget(Paragraph::new(Line::from(glyph)), pos);
    }
    if visible_end < app.all_columns.len() {
        let glyph = Span::styled(
            "▸",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        );
        let pos = Rect {
            x: area.x.saturating_add(area.width).saturating_sub(1),
            y: area.y,
            width: 1,
            height: 1,
        };
        f.render_widget(Paragraph::new(Line::from(glyph)), pos);
    }

    app.visible_columns = visible_start..visible_end;
}

/// Minimum width for a non-empty column — enough to fit the canonical
/// `IN PROGRESS (NN)` header without clipping plus the 2-char right
/// gutter cards rely on.
const NONEMPTY_MIN_W: u16 = 14;
/// Minimum width for a collapsed (empty) column — wide enough for the
/// status label's first few letters plus the `(0)` count, so the user
/// can still tell what each lane is.
const EMPTY_MIN_W: u16 = 8;

/// Lay out `[scroll, scroll + k)` of `columns` into `area_w` columns of
/// terminal cells. Returns `(col_idx, width)` pairs in render order.
///
/// Two-pass algorithm:
///
/// 1. **Fit pass** — walk forward from `scroll`, assigning each column
///    its minimum width (empty → `EMPTY_MIN_W`, non-empty →
///    `NONEMPTY_MIN_W`). Stop as soon as adding the next column would
///    push past `area_w`.
///
/// 2. **Expand pass** — divide whatever slack remains among the
///    non-empty visible columns. Empty columns stay at the minimum so
///    they don't reclaim space the user said is "uninteresting".
///
/// At least one column always renders, even if it doesn't reach its
/// minimum — better to clip than to paint nothing at all.
fn compute_column_widths(
    columns: &[WorkflowColumn],
    counts: &[usize],
    scroll: usize,
    area_w: u16,
) -> Vec<(usize, u16)> {
    let mut out: Vec<(usize, u16)> = Vec::new();
    if columns.is_empty() || area_w == 0 {
        return out;
    }
    let start = scroll.min(columns.len().saturating_sub(1));
    let mut used: u16 = 0;
    for i in start..columns.len() {
        let min_w = if counts.get(i).copied().unwrap_or(0) == 0 {
            EMPTY_MIN_W
        } else {
            NONEMPTY_MIN_W
        };
        let next_used = used.saturating_add(min_w);
        if next_used > area_w {
            if out.is_empty() {
                // Render at least one column even if it overflows —
                // empty board with `area_w < EMPTY_MIN_W` only happens
                // in tiny test terminals.
                out.push((i, area_w));
                used = area_w;
            }
            break;
        }
        out.push((i, min_w));
        used = next_used;
    }
    // Expand pass: hand slack to non-empty columns.
    let slack = area_w.saturating_sub(used);
    let non_empty_positions: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, (idx, _))| counts.get(*idx).copied().unwrap_or(0) > 0)
        .map(|(pos, _)| pos)
        .collect();
    let grow_targets = if non_empty_positions.is_empty() {
        // Nothing is non-empty in the visible window — share the slack
        // across every visible column so we don't leave a gap on the
        // right.
        (0..out.len()).collect::<Vec<_>>()
    } else {
        non_empty_positions
    };
    if !grow_targets.is_empty() && slack > 0 {
        let n = grow_targets.len() as u16;
        let bonus = slack / n;
        let remainder = (slack % n) as usize;
        for (n_idx, &pos) in grow_targets.iter().enumerate() {
            out[pos].1 = out[pos]
                .1
                .saturating_add(bonus + if n_idx < remainder { 1 } else { 0 });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn render_column(
    f: &mut Frame,
    app: &KanbanApp,
    col_idx: usize,
    area: Rect,
    columns: &HashMap<String, Column>,
    hits: &mut Vec<CardHit>,
    show_workflow_label: bool,
    collapsed: bool,
) {
    let column = app.column(col_idx).clone();
    let tasks = app.column_tasks(col_idx);
    let focused = col_idx == app.selected_column;

    let header_color = category_color(column.category);
    let header_style = if focused {
        Style::default()
            .fg(header_color)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(header_color)
    };
    // Collapsed columns truncate the canonical label hard so the
    // narrow slot can still tell the user which lane it is. Non-empty
    // columns get the full label.
    let header_text = if collapsed {
        truncate(&column_label(&column.status_name), area.width.saturating_sub(4) as usize)
    } else {
        column_label(&column.status_name)
    };
    let title_line = Line::from(vec![
        Span::styled(header_text, header_style),
        Span::styled(
            format!(" ({})", tasks.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    // Reserve a 1-line subscript under the status header when more than
    // one workflow is loaded, so the user can tell which workflow each
    // column belongs to. Skip the line entirely for single-workflow
    // projects so the layout matches the legacy 5-column view exactly.
    let header_rows = if show_workflow_label { 2 } else { 1 };

    // Manual title/list split — keeps hit-test geometry unambiguous (no
    // dependence on Block.inner behavior with title + Borders::NONE).
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(header_rows), Constraint::Min(0)])
        .split(area);
    let header_area = chunks[0];
    let list_area = chunks[1];

    if show_workflow_label {
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(header_area);
        f.render_widget(Paragraph::new(title_line), split[0]);
        let label = truncate(&column.workflow, area.width.saturating_sub(1) as usize);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                label,
                Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
            ))),
            split[1],
        );
    } else {
        f.render_widget(Paragraph::new(title_line), header_area);
    }

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
        lines.push(workflow_card_indicator(
            &tf.task,
            &app.workflows,
            show_workflow_label,
            max_text,
        ));

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

    // Each card item renders 3 lines (title, meta, workflow indicator —
    // see [`workflow_card_indicator`], which renders blank when there's
    // nothing to surface so this constant stays correct). Clip the last
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
        "  h/l col   j/k row   enter/␣ open   H/L move col   K/J reorder   w workflow   f filter   r refresh",
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

/// Workflow filter dropdown — same anchor logic as the worker one but
/// keyed off [`KanbanApp::workflow_chip_hit`] so it drops below the
/// workflow chip. Kept as a separate function from the worker dropdown
/// so each can evolve (footer hints, future per-dropdown affordances)
/// independently.
fn render_workflow_dropdown(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    let opts = app.workflow_dropdown_options();
    if opts.is_empty() {
        app.close_workflow_dropdown();
        return;
    }
    let cursor = app
        .workflow_dropdown
        .as_ref()
        .map(|d| d.cursor)
        .unwrap_or(0);

    let max_label_w = opts
        .iter()
        .map(|o| workflow_dropdown_row_text(o).chars().count())
        .max()
        .unwrap_or(10);
    let desired_w = (max_label_w + 4).max(22) as u16;
    let popover_w = desired_w.min(area.width).min(area.width / 3 + 8);
    let popover_x = match app.workflow_chip_hit {
        Some(chip) => {
            let right = chip.x.saturating_add(chip.width);
            right.saturating_sub(popover_w).max(area.x)
        }
        None => area.x.saturating_add(area.width).saturating_sub(popover_w),
    };
    let popover_h = (opts.len() as u16 + 3).min(area.height.saturating_sub(1));
    let popover_y = area
        .y
        .saturating_add(1)
        .min(area.y + area.height.saturating_sub(popover_h));

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
            " Filter by workflow ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )]));
    let inner = block.inner(popover_area);
    f.render_widget(block, popover_area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let list_area = chunks[0];
    let hint_area = chunks[1];

    let mut hits: Vec<DropdownHit> = Vec::with_capacity(opts.len());
    let mut items: Vec<ListItem> = Vec::with_capacity(opts.len());
    for (idx, opt) in opts.iter().enumerate() {
        let active = app.workflow_filter == opt.filter;
        let label = workflow_dropdown_row_text(opt);
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

    app.workflow_dropdown_hits = hits;
}

/// Sibling of [`dropdown_row_text`] for workflow options. Keeping it
/// separate avoids overloading the worker-side row formatter with a
/// trait/enum that adds nothing to the only two callers.
fn workflow_dropdown_row_text(opt: &WorkflowDropdownOption) -> String {
    let label = match &opt.filter {
        None => "All".to_string(),
        Some(name) => name.clone(),
    };
    format!("{} ({})", label, opt.count)
}

/// Header text for a workflow column. The canonical 5-status names get
/// the familiar uppercase labels (`BACKLOG`, `IN PROGRESS`, …); custom
/// status names fall through to a generic uppercase rendering so a
/// `Design` status renders as `DESIGN` without the renderer needing a
/// per-workflow lookup table.
fn column_label(status_name: &str) -> String {
    match status_name {
        "Backlog" => "BACKLOG".to_string(),
        "Todo" => "TO DO".to_string(),
        "InProgress" => "IN PROGRESS".to_string(),
        "Review" => "REVIEW".to_string(),
        "Done" => "DONE".to_string(),
        other => other.to_uppercase(),
    }
}

/// Header colour for a column. Driven by [`StatusCategory`] so a
/// renamed Review-step column (e.g. a workflow's `QA` status with
/// `category: handoff`) still gets the same magenta the canonical
/// Review column has — generic code keys off category, not name.
fn category_color(category: StatusCategory) -> Color {
    match category {
        StatusCategory::Backlog => Color::Gray,
        StatusCategory::Ready => Color::Blue,
        StatusCategory::Active => Color::Yellow,
        StatusCategory::Handoff => Color::Magenta,
        StatusCategory::Done => Color::Green,
        // Closed-without-shipping (Cancelled / Won't Fix / Duplicate).
        // Darker than Backlog so the eye reads it as terminal-inactive,
        // not waiting-for-triage.
        StatusCategory::Archived => Color::DarkGray,
    }
}

/// Per-Column header colour, retained for spots (the popover header,
/// the dep-list badges) that still take a `Column` directly. Routes
/// through [`category_color`] so the canonical 5-status palette stays
/// the single source of truth.
fn column_color(c: Column) -> Color {
    category_color(c.category())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Build the third-line workflow indicator shown at the bottom of each
/// card. Renders the task's workflow name and — when the workflow's
/// `git.base_branch` template carries a `{{var}}` placeholder that
/// resolves from this task's params — the resolved branch alongside it.
///
/// `show_workflow_name` mirrors the column-header `show_workflow_label`:
/// true when the visible board mixes more than one workflow, so naming
/// each card's workflow disambiguates which lane it belongs to. When
/// false, the workflow name is suppressed (the column header already
/// implies it) but a resolved branch is still surfaced because that's
/// per-task information the column subscript can't carry.
///
/// Returns `Line::raw("")` when there's nothing useful to say so the
/// card retains its canonical 3-row height (preserves [`CardHit`]
/// geometry and the `ROWS_PER_CARD` constant downstream).
fn workflow_card_indicator(
    task: &Task,
    workflows: &[Workflow],
    show_workflow_name: bool,
    max_text: usize,
) -> Line<'static> {
    let wf_name = task.workflow_or_default();
    let resolved_branch = workflows
        .iter()
        .find(|w| w.name == wf_name)
        .and_then(|w| w.git.as_ref())
        .and_then(|g| g.base_branch.as_deref())
        // Only interesting when the template carries a placeholder — a
        // fixed `base_branch: main` would add no per-task info.
        .filter(|tmpl| tmpl.contains("{{"))
        .and_then(|tmpl| {
            let mut missing = Vec::new();
            let resolved =
                shelbi_core::substitute_placeholders(tmpl, &task.params, &mut missing);
            // Hide on unresolved — a half-substituted branch at the
            // per-card glance scale would mislead more than help.
            missing.is_empty().then_some(resolved)
        });

    let text = match (show_workflow_name, resolved_branch) {
        (false, None) => return Line::raw(""),
        (true, None) => wf_name.to_string(),
        (true, Some(branch)) => format!("{wf_name} · {branch}"),
        (false, Some(branch)) => branch,
    };
    Line::from(Span::styled(
        truncate(&text, max_text),
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::ITALIC),
    ))
}

// ---------------------------------------------------------------------------
// All-mode column builders

/// Build the All-mode column list as the union of every loaded
/// workflow's declared `(workflow, status)` pairs. Workflow order is
/// whatever the caller passed in (`list_workflows` already sorts
/// alphabetically); inside each workflow the statuses are emitted in
/// their declared order — that order *is* the workflow's left-to-right
/// column order in the YAML, so workflow authors get exactly the layout
/// they declared.
fn workflow_columns(workflows: &[Workflow]) -> Vec<WorkflowColumn> {
    let mut out: Vec<WorkflowColumn> =
        Vec::with_capacity(workflows.iter().map(|w| w.statuses.len()).sum());
    for wf in workflows {
        for st in &wf.statuses {
            out.push(WorkflowColumn {
                workflow: wf.name.clone(),
                status_name: st.name.clone(),
                category: st.category,
            });
        }
    }
    out
}

/// Same as [`workflow_columns`] but takes references — used by the
/// filter pass, which builds a `Vec<&Workflow>` from the loaded
/// workflows without cloning. Identical semantics.
fn workflow_columns_from_refs(workflows: &[&Workflow]) -> Vec<WorkflowColumn> {
    let mut out: Vec<WorkflowColumn> =
        Vec::with_capacity(workflows.iter().map(|w| w.statuses.len()).sum());
    for wf in workflows {
        for st in &wf.statuses {
            out.push(WorkflowColumn {
                workflow: wf.name.clone(),
                status_name: st.name.clone(),
                category: st.category,
            });
        }
    }
    out
}

/// Resolve which workflow status `task` lives in. Task storage still
/// uses the legacy [`Column`] enum on disk (`Plans/workflows.md` §10
/// hasn't moved tasks to status-name yet), so we have to bridge.
///
/// Resolution order, mirroring the events log writer's behaviour:
///
/// 1. **Name match** — if the workflow declares a status whose name
///    equals `task.column.default_status_name()` (Backlog / Todo /
///    InProgress / Review / Done), use that. Covers the default
///    workflow and any custom workflow that reuses the canonical names.
/// 2. **Category match** — fall back to the first status in the
///    workflow whose category equals the task's column category.
///    Handles a custom workflow that renamed `InProgress` to `Design`
///    (both report `StatusCategory::Active`).
/// 3. **Canonical** — if the workflow declares no status that matches
///    by name or category, return the canonical name unchanged. The
///    task won't bucket cleanly, but the renderer never crashes.
fn resolve_task_status(task: &Task, workflow: &Workflow) -> String {
    let canonical = task.column.default_status_name();
    if workflow.status(canonical).is_some() {
        return canonical.to_string();
    }
    let cat = task.column.category();
    if let Some(st) = workflow.statuses.iter().find(|s| s.category == cat) {
        return st.name.clone();
    }
    canonical.to_string()
}

/// Map a [`StatusCategory`] back to the canonical [`Column`] enum the
/// task storage layer still takes. The mapping is 1:1 since each
/// category maps to exactly one default-workflow status (see the
/// table in [`Column::category`]).
fn category_to_column(category: StatusCategory) -> Column {
    match category {
        StatusCategory::Backlog => Column::Backlog,
        StatusCategory::Ready => Column::Todo,
        StatusCategory::Active => Column::InProgress,
        StatusCategory::Handoff => Column::Review,
        StatusCategory::Done => Column::Done,
        // Legacy `Column` has no Archived bucket — both terminal
        // categories collapse to `Done` on disk until task storage
        // moves off the 5-column enum (`Plans/workflows.md` §10).
        StatusCategory::Archived => Column::Done,
    }
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
    let col_label = column_label(task.column.default_status_name());
    let col_span = Span::styled(col_label, Style::default().fg(column_color(task.column)));
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
        // `move_card` now runs the lifecycle branch-cut hook
        // (`shelbi_orchestrator::lifecycle::ensure_branch_for_in_progress`)
        // whenever a card lands in `in_progress`. That hook needs both a
        // loadable project YAML and a real git repo at the hub workdir,
        // so we provision them here before the move fires.
        crate::test_support::provision_hub_repo_for_project(&home, "demo");

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

    // ---- All-mode column rendering ---------------------------------------

    /// Build a workflow inline. Keeping a builder here (rather than
    /// loading YAML from disk) lets every All-mode test stay
    /// hermetic — no `SHELBI_HOME` juggling.
    fn workflow(name: &str, statuses: &[(&str, StatusCategory)]) -> Workflow {
        Workflow {
            name: name.into(),
            description: None,
            statuses: statuses
                .iter()
                .map(|(n, c)| shelbi_core::WorkflowStatus {
                    name: (*n).into(),
                    category: *c,
                    owner: shelbi_core::Owner::Agent,
                })
                .collect(),
            initial_status: None,
            transitions: None,
            git: None,
            zen: None,
        }
    }

    /// Same as `task_file_for` but lets the test pin the task's
    /// workflow frontmatter field — All-mode bucket lookups key off
    /// `task.workflow_or_default()`, so the workflow has to be
    /// settable per task.
    fn task_in_workflow(
        id: &str,
        column: Column,
        workflow: Option<&str>,
        updated: &str,
    ) -> TaskFile {
        let updated_at = chrono::DateTime::parse_from_rfc3339(updated)
            .unwrap()
            .with_timezone(&chrono::Utc);
        TaskFile {
            task: shelbi_core::Task {
                id: id.to_string(),
                title: id.to_string(),
                column,
                priority: 0,
                assigned_to: None,
                workflow: workflow.map(|s| s.to_string()),
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

    /// With only the canonical default workflow loaded, `all_columns`
    /// reduces to exactly the 6 declared columns in order — the five
    /// legacy lanes plus the `Canceled` terminal lane.
    #[test]
    fn workflow_columns_for_default_only_matches_documented_six() {
        let cols = workflow_columns(std::slice::from_ref(&default_workflow()));
        let pairs: Vec<(&str, &str)> = cols
            .iter()
            .map(|c| (c.workflow.as_str(), c.status_name.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("default", "Backlog"),
                ("default", "Todo"),
                ("default", "InProgress"),
                ("default", "Review"),
                ("default", "Done"),
                ("default", "Canceled"),
            ]
        );
    }

    /// Two workflows → union is the concatenation in `list_workflows`
    /// order, with each workflow's statuses kept in declared order.
    /// A custom `design-review` workflow appended after `default`
    /// adds its statuses to the right of the canonical five.
    #[test]
    fn workflow_columns_unions_pairs_across_workflows_in_declared_order() {
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let wfs = vec![default_workflow(), design];
        let cols = workflow_columns(&wfs);
        let pairs: Vec<(&str, &str)> = cols
            .iter()
            .map(|c| (c.workflow.as_str(), c.status_name.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("default", "Backlog"),
                ("default", "Todo"),
                ("default", "InProgress"),
                ("default", "Review"),
                ("default", "Done"),
                ("default", "Canceled"),
                ("design-review", "Backlog"),
                ("design-review", "Design"),
                ("design-review", "QA"),
                ("design-review", "Done"),
            ]
        );
    }

    /// A task whose Column has a status-name match in its workflow
    /// resolves directly. A task whose Column has no name match falls
    /// back to the first status in the workflow with the same
    /// category (the events log's bridge rule) — so a task with
    /// `column: InProgress` and `workflow: design-review` lands in
    /// design-review's `Design` column, not its `Backlog`.
    #[test]
    fn resolve_task_status_name_match_then_category_fallback() {
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let t_backlog =
            task_in_workflow("a", Column::Backlog, Some("design-review"), "2026-06-20T10:00:00Z")
                .task;
        let t_wip =
            task_in_workflow("b", Column::InProgress, Some("design-review"), "2026-06-20T10:00:00Z")
                .task;
        let t_review =
            task_in_workflow("c", Column::Review, Some("design-review"), "2026-06-20T10:00:00Z")
                .task;
        // Name match.
        assert_eq!(resolve_task_status(&t_backlog, &design), "Backlog");
        // Category fallback — `InProgress` is not declared by name.
        assert_eq!(resolve_task_status(&t_wip, &design), "Design");
        // Category fallback — `Review` is not declared by name.
        assert_eq!(resolve_task_status(&t_review, &design), "QA");
    }

    /// `column_tasks(idx)` honours both the workflow and status fields
    /// of the `WorkflowColumn` at `idx`. A task whose `workflow`
    /// frontmatter is `default` only shows up in default's columns;
    /// a task whose workflow is `design-review` only shows up in
    /// design-review's columns. Tasks pointing at a workflow that
    /// isn't loaded fall back to the default workflow.
    #[test]
    fn column_tasks_buckets_by_workflow_and_status_pair() {
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.all_columns = workflow_columns(&app.workflows);
        app.tasks = vec![
            // Default-workflow tasks land in default's columns.
            task_in_workflow("a", Column::Todo, None, "2026-06-20T10:00:00Z"),
            task_in_workflow("b", Column::Review, Some("default"), "2026-06-20T10:00:00Z"),
            // design-review tasks land in design-review's columns.
            task_in_workflow(
                "c",
                Column::InProgress,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            task_in_workflow(
                "d",
                Column::Done,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            // Task pointing at a workflow that doesn't exist falls
            // back to default — Todo lands in default's Todo column.
            task_in_workflow("orphan", Column::Todo, Some("ghost"), "2026-06-20T10:00:00Z"),
        ];

        // default order: Backlog Todo InProgress Review Done Canceled
        // design-review order: Backlog Design QA Done
        // → all_columns indexes:
        //     0 default/Backlog       6 design-review/Backlog
        //     1 default/Todo          7 design-review/Design
        //     2 default/InProgress    8 design-review/QA
        //     3 default/Review        9 design-review/Done
        //     4 default/Done
        //     5 default/Canceled
        let ids = |idx: usize| -> Vec<&str> {
            app.column_tasks(idx)
                .iter()
                .map(|t| t.task.id.as_str())
                .collect()
        };
        assert_eq!(ids(0), Vec::<&str>::new(), "default/Backlog");
        assert_eq!(ids(1), vec!["a", "orphan"], "default/Todo");
        assert_eq!(ids(3), vec!["b"], "default/Review");
        assert_eq!(ids(7), vec!["c"], "design-review/Design (category fallback)");
        assert_eq!(ids(9), vec!["d"], "design-review/Done");
    }

    /// With two workflows loaded, nav_right after the last default
    /// column lands on the first design-review column, and wraps
    /// back to default/Backlog after the last design-review column.
    /// `move_card_right`, on the other hand, must NEVER cross
    /// workflow boundaries — a card in default/Done wraps back to
    /// default/Backlog instead of jumping into design-review.
    #[test]
    fn nav_right_crosses_workflows_but_move_card_does_not() {
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.all_columns = workflow_columns(&app.workflows);

        // From default/Canceled (idx 5), nav_right → design-review/Backlog (idx 6).
        app.selected_column = 5;
        app.nav_right();
        assert_eq!(app.selected_column, 6);
        // Continue: design-review/Backlog → Design → QA → Done → wrap to 0.
        app.nav_right();
        app.nav_right();
        app.nav_right();
        assert_eq!(app.selected_column, 9, "should be on design-review/Done");
        app.nav_right();
        assert_eq!(app.selected_column, 0, "wraps back to default/Backlog");

        // Move-card boundaries: from default/Canceled, move-right wraps
        // back to default/Backlog. Test through the helper so we
        // don't need a real task on disk.
        assert_eq!(
            app.adjacent_column_in_same_workflow(5, true),
            Some(0),
            "default/Canceled → default/Backlog (wrap inside workflow)"
        );
        assert_eq!(
            app.adjacent_column_in_same_workflow(0, false),
            Some(5),
            "default/Backlog → default/Canceled (wrap backwards inside workflow)"
        );
        // Same check on the other workflow.
        assert_eq!(
            app.adjacent_column_in_same_workflow(9, true),
            Some(6),
            "design-review/Done → design-review/Backlog (wrap inside workflow)"
        );
    }

    /// A workflow with a single status has nowhere to move-card to,
    /// so the move-card helpers return `None` and the caller noops.
    #[test]
    fn adjacent_column_returns_none_for_singleton_workflow() {
        let single = workflow("solo", &[("OnlyStatus", StatusCategory::Active)]);
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![single];
        app.all_columns = workflow_columns(&app.workflows);
        assert_eq!(app.adjacent_column_in_same_workflow(0, true), None);
        assert_eq!(app.adjacent_column_in_same_workflow(0, false), None);
    }

    /// Two workflows render side-by-side. The header for each column
    /// shows the status name; a subscript row labels the workflow
    /// underneath so the user can tell which `Backlog` is which.
    /// Single-workflow projects omit the subscript so the layout
    /// matches the legacy 5-column view exactly.
    #[test]
    fn rendering_two_workflows_shows_status_names_and_workflow_subscript() {
        use ratatui::{backend::TestBackend, Terminal};
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.all_columns = workflow_columns(&app.workflows);
        // Seed every column with one task so collapse-empty doesn't
        // truncate the header text — the test is checking that the
        // multi-workflow layout paints labels at all, not that the
        // collapse logic kicks in.
        app.tasks = vec![
            task_in_workflow("d-backlog", Column::Backlog, None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-todo", Column::Todo, None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-wip", Column::InProgress, None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-review", Column::Review, None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-done", Column::Done, None, "2026-06-20T10:00:00Z"),
            task_in_workflow(
                "dr-backlog",
                Column::Backlog,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            task_in_workflow(
                "dr-design",
                Column::InProgress,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            task_in_workflow(
                "dr-qa",
                Column::Review,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            task_in_workflow(
                "dr-done",
                Column::Done,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
        ];

        // 10 columns total (default's six + design's four) → keep the
        // terminal wide so every status header has room to render its
        // label. The unseeded `Canceled` column collapses to
        // EMPTY_MIN_W, leaving plenty of slack for the headers the
        // assertions below look for.
        let backend = TestBackend::new(160, 18);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let buf = term.backend().buffer().clone();
        let rendered: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Canonical headers render full when the layout has enough
        // room. With 9 non-empty columns (× 14) + 1 empty Canceled (8)
        // = 134 min, 160 cells leaves slack for full headers on the
        // seeded columns.
        assert!(rendered.contains("BACKLOG"), "BACKLOG header missing:\n{rendered}");
        assert!(rendered.contains("DESIGN"), "DESIGN header missing:\n{rendered}");
        assert!(rendered.contains("QA"), "QA header missing:\n{rendered}");
        // Workflow subscript labels appear under the headers when >1
        // workflow is loaded.
        assert!(
            rendered.contains("default"),
            "default workflow subscript missing:\n{rendered}"
        );
        assert!(
            rendered.contains("design-review") || rendered.contains("design-revi"),
            "design-review subscript missing:\n{rendered}"
        );
    }

    // ---- workflow filter -------------------------------------------------

    /// With `workflow_filter` set to a loaded workflow, `compute_all_columns`
    /// emits only that workflow's columns. Clearing the filter restores the
    /// All-mode union.
    #[test]
    fn workflow_filter_narrows_all_columns_to_one_workflow() {
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("Done", StatusCategory::Done),
            ],
        );
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.all_columns = app.compute_all_columns();
        assert_eq!(app.all_columns.len(), 6 + 3);

        app.workflow_filter = Some("design-review".into());
        app.all_columns = app.compute_all_columns();
        let pairs: Vec<(&str, &str)> = app
            .all_columns
            .iter()
            .map(|c| (c.workflow.as_str(), c.status_name.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("design-review", "Backlog"),
                ("design-review", "Design"),
                ("design-review", "Done"),
            ]
        );

        app.workflow_filter = None;
        app.all_columns = app.compute_all_columns();
        assert_eq!(app.all_columns.len(), 6 + 3, "clear restores All-mode");
    }

    /// A stale workflow filter (workflow no longer loaded) falls back
    /// to the unfiltered union so the board still paints. The filter
    /// itself isn't auto-cleared — the user can see the chip and
    /// dismiss it.
    #[test]
    fn workflow_filter_for_missing_workflow_falls_back_to_union() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow()];
        app.workflow_filter = Some("removed".into());
        app.all_columns = app.compute_all_columns();
        assert_eq!(app.all_columns.len(), 6);
        assert_eq!(app.workflow_filter, Some("removed".into()));
    }

    /// `workflow_dropdown_options` lists All + every loaded workflow.
    /// Counts use the unfiltered tasks list (zero-count workflows
    /// still appear so the user can pick them).
    #[test]
    fn workflow_dropdown_options_lists_all_then_each_workflow() {
        let design = workflow("design-review", &[("Backlog", StatusCategory::Backlog)]);
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.tasks = vec![
            task_in_workflow("a", Column::Todo, None, "2026-06-20T10:00:00Z"),
            task_in_workflow("b", Column::Backlog, Some("design-review"), "2026-06-20T10:00:00Z"),
        ];
        let opts = app.workflow_dropdown_options();
        assert_eq!(opts.len(), 3);
        assert!(opts[0].filter.is_none());
        assert_eq!(opts[0].count, 2);
        assert_eq!(opts[1].filter.as_deref(), Some("default"));
        assert_eq!(opts[1].count, 1);
        assert_eq!(opts[2].filter.as_deref(), Some("design-review"));
        assert_eq!(opts[2].count, 1);
    }

    /// Toggling the workflow dropdown is self-inverse, the same way
    /// the worker dropdown is — the same key opens and closes it.
    #[test]
    fn toggle_workflow_dropdown_is_self_inverse() {
        let mut app = KanbanApp::new("demo");
        assert!(!app.workflow_dropdown_is_open());
        app.toggle_workflow_dropdown();
        assert!(app.workflow_dropdown_is_open());
        app.toggle_workflow_dropdown();
        assert!(!app.workflow_dropdown_is_open());
    }

    // ---- collapse-empty + horizontal scroll ------------------------------

    /// `compute_column_widths` collapses empty columns to a fixed
    /// minimum and gives the slack to the non-empty ones. With a
    /// single non-empty column out of six, it should grow well past
    /// the others.
    #[test]
    fn compute_column_widths_collapses_empty_and_grows_non_empty() {
        let workflows = vec![default_workflow()];
        let cols = workflow_columns(&workflows);
        // 5 empty columns, 1 non-empty.
        let counts = vec![0, 0, 3, 0, 0, 0];
        let widths = compute_column_widths(&cols, &counts, 0, 100);
        assert_eq!(widths.len(), 6);
        // Empty columns sit at EMPTY_MIN_W exactly; non-empty consumes
        // the rest.
        for (i, w) in &widths {
            if counts[*i] == 0 {
                assert_eq!(*w, EMPTY_MIN_W, "empty column {i} should stay at min");
            } else {
                assert!(
                    *w > NONEMPTY_MIN_W,
                    "non-empty column {i} should grow beyond min, got {w}"
                );
            }
        }
        // Total width matches the area exactly — no gap on the right.
        let total: u16 = widths.iter().map(|(_, w)| *w).sum();
        assert_eq!(total, 100);
    }

    /// When the minimum widths overflow the available area,
    /// `compute_column_widths` drops trailing columns from the visible
    /// window so the remaining ones fit. `scroll` then walks the
    /// window forward.
    #[test]
    fn compute_column_widths_drops_overflow_and_scrolls() {
        // 10 columns × NONEMPTY_MIN_W (14) = 140 > 80. Some columns
        // must drop out.
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let wfs = vec![default_workflow(), design];
        let cols = workflow_columns(&wfs);
        let counts = vec![1; cols.len()];

        let widths = compute_column_widths(&cols, &counts, 0, 80);
        assert!(widths.len() < cols.len(), "should drop trailing cols");
        assert_eq!(widths[0].0, 0, "starts at scroll=0");

        // Scrolling reveals later columns at the cost of earlier ones.
        let widths = compute_column_widths(&cols, &counts, 4, 80);
        assert_eq!(widths[0].0, 4, "starts at scroll=4");
        assert!(
            widths.iter().map(|(_, w)| *w).sum::<u16>() <= 80,
            "respects area width"
        );
    }

    /// Full render with a workflow filter applied: only that
    /// workflow's columns paint, and the workflow chip shows the
    /// active filter. The worker chip stays in place beside it.
    #[test]
    fn rendering_workflow_filter_narrows_visible_columns() {
        use ratatui::{backend::TestBackend, Terminal};
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("Done", StatusCategory::Done),
            ],
        );
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.workflow_filter = Some("design-review".into());
        app.all_columns = app.compute_all_columns();
        app.tasks = vec![
            task_in_workflow("d1", Column::Todo, None, "2026-06-20T10:00:00Z"),
            task_in_workflow(
                "dr1",
                Column::InProgress,
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
        ];

        let backend = TestBackend::new(120, 14);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let buf = term.backend().buffer().clone();
        let rendered: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");

        // Filter chip shows the active filter; only design-review
        // columns render. The default workflow's TODO label must not
        // appear — that's the proof the filter narrowed the layout.
        assert!(
            rendered.contains("Workflow: design-review ▾"),
            "workflow chip missing or wrong:\n{rendered}"
        );
        assert!(rendered.contains("DESIGN"), "DESIGN column missing");
        // TO DO is a default-workflow column with the legacy uppercase
        // label — never paints under design-review filter. (Using
        // `IN PROGRESS` would false-positive against a generic prefix.)
        assert!(
            !rendered.contains("TO DO"),
            "default-workflow TO DO leaked into filtered view:\n{rendered}"
        );
        // dr1 should render in design-review/Design.
        assert!(rendered.contains("dr1"), "filtered card missing");
    }

    /// `ensure_selected_visible` advances `column_scroll` when the
    /// selection falls past the visible window, and pulls it back to
    /// the selection when scrolled too far right.
    #[test]
    fn ensure_selected_visible_keeps_selection_in_view() {
        let design = workflow(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow(), design];
        app.all_columns = workflow_columns(&app.workflows);
        // Every column non-empty so they all want full width.
        app.tasks = (0..app.all_columns.len())
            .map(|i| task_in_workflow(&format!("t{i}"), Column::Todo, None, "2026-06-20T10:00:00Z"))
            .collect();

        // Select a column that won't fit at scroll=0 in 60 cells.
        app.selected_column = 7;
        app.column_scroll = 0;
        app.ensure_selected_visible(60);
        assert!(app.column_scroll > 0, "should scroll right");

        // Scroll back: selection at 1 with scroll at 5 → scroll snaps
        // back to 1.
        app.selected_column = 1;
        app.column_scroll = 5;
        app.ensure_selected_visible(60);
        assert!(app.column_scroll <= 1, "should snap back left");
    }

    // ---- Per-card workflow indicator -------------------------------------

    /// Read a line's plain text — used to assert indicator content
    /// independently of the styling spans.
    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn task_with_params(
        id: &str,
        workflow: Option<&str>,
        params: &[(&str, &str)],
    ) -> shelbi_core::Task {
        shelbi_core::Task {
            id: id.into(),
            title: id.into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            workflow: workflow.map(|s| s.to_string()),
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
        }
    }

    fn workflow_with_git(name: &str, base_branch: Option<&str>) -> Workflow {
        Workflow {
            name: name.into(),
            description: None,
            statuses: vec![shelbi_core::WorkflowStatus {
                name: "Todo".into(),
                category: StatusCategory::Ready,
                owner: shelbi_core::Owner::Agent,
            }],
            initial_status: None,
            transitions: None,
            git: base_branch.map(|b| shelbi_core::GitConfig {
                base_branch: Some(b.into()),
                merge_strategy: shelbi_core::MergeStrategy::Squash,
            }),
            zen: None,
        }
    }

    /// Single-workflow project, default workflow, no git params → the
    /// card's third line stays blank, matching the legacy layout the
    /// pre-workflow TUI rendered. Avoids forcing a redundant "default"
    /// label onto every card in projects that haven't adopted workflows.
    #[test]
    fn workflow_card_indicator_blank_for_default_only() {
        let task = task_with_params("t", None, &[]);
        let workflows = vec![default_workflow()];
        let line = workflow_card_indicator(&task, &workflows, false, 40);
        assert_eq!(line_text(&line), "");
    }

    /// Multi-workflow board, default-workflow task → name surfaces so
    /// the user can distinguish "default" cards from cards belonging to
    /// other workflows on the same screen.
    #[test]
    fn workflow_card_indicator_shows_default_when_multiple_workflows() {
        let task = task_with_params("t", None, &[]);
        let workflows = vec![default_workflow(), workflow_with_git("feature-task", None)];
        let line = workflow_card_indicator(&task, &workflows, true, 40);
        assert_eq!(line_text(&line), "default");
    }

    /// Custom workflow with no `git:` block → indicator falls back to
    /// just the workflow name. Nothing to resolve.
    #[test]
    fn workflow_card_indicator_shows_workflow_name_without_git_block() {
        let task = task_with_params("t", Some("design-review"), &[]);
        let workflows = vec![
            default_workflow(),
            workflow_with_git("design-review", None),
        ];
        let line = workflow_card_indicator(&task, &workflows, true, 40);
        assert_eq!(line_text(&line), "design-review");
    }

    /// Resolved placeholder: `base_branch: feature/{{feature}}` + the
    /// task carrying `feature: auth-rewrite` renders as
    /// `feature-task · feature/auth-rewrite`. This is the headline case
    /// the task description names — workflow + resolved parameter.
    #[test]
    fn workflow_card_indicator_appends_resolved_branch() {
        let task = task_with_params(
            "t",
            Some("feature-task"),
            &[("feature", "auth-rewrite")],
        );
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = workflow_card_indicator(&task, &workflows, true, 60);
        assert_eq!(line_text(&line), "feature-task · feature/auth-rewrite");
    }

    /// When the column header already implies the workflow (single
    /// workflow visible, so `show_workflow_name=false`), the per-card
    /// indicator drops the redundant name but keeps the resolved branch
    /// — it's the only per-task signal worth carrying.
    #[test]
    fn workflow_card_indicator_omits_name_but_keeps_branch_when_filtered() {
        let task = task_with_params(
            "t",
            Some("feature-task"),
            &[("feature", "dashboard-v2")],
        );
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = workflow_card_indicator(&task, &workflows, false, 60);
        assert_eq!(line_text(&line), "feature/dashboard-v2");
    }

    /// A `base_branch` without any placeholder (e.g. `main`) carries no
    /// per-task info, so the indicator suppresses it. Falls through to
    /// the workflow-name (when `show_workflow_name`) or blank line.
    #[test]
    fn workflow_card_indicator_skips_non_templated_branch() {
        let task = task_with_params("t", Some("feature-release"), &[]);
        let workflows = vec![workflow_with_git("feature-release", Some("main"))];
        // Show name path: drop the branch, keep the workflow name.
        let with_name = workflow_card_indicator(&task, &workflows, true, 60);
        assert_eq!(line_text(&with_name), "feature-release");
        // No name path: nothing left worth showing → blank.
        let without_name = workflow_card_indicator(&task, &workflows, false, 60);
        assert_eq!(line_text(&without_name), "");
    }

    /// Missing param keeps the placeholder unresolved. Rather than
    /// surface a half-substituted `feature/{{feature}}` (which would
    /// mislead at a glance), the indicator drops the branch and shows
    /// just the workflow name (or blank when the column header already
    /// names the workflow). The popover stays responsible for surfacing
    /// the actionable error.
    #[test]
    fn workflow_card_indicator_hides_branch_when_param_missing() {
        let task = task_with_params("t", Some("feature-task"), &[]);
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let with_name = workflow_card_indicator(&task, &workflows, true, 60);
        assert_eq!(line_text(&with_name), "feature-task");
        let without_name = workflow_card_indicator(&task, &workflows, false, 60);
        assert_eq!(line_text(&without_name), "");
    }

    /// `max_text` clips long indicators with an ellipsis, matching how
    /// titles and meta lines are already truncated so narrow columns
    /// don't push spans past the card's right gutter.
    #[test]
    fn workflow_card_indicator_truncates_to_max_text() {
        let task = task_with_params(
            "t",
            Some("feature-task"),
            &[("feature", "very-long-feature-name-that-exceeds-the-cell-width")],
        );
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = workflow_card_indicator(&task, &workflows, true, 20);
        let text = line_text(&line);
        assert_eq!(text.chars().count(), 20, "got: {text:?}");
        assert!(text.ends_with('…'), "should end with ellipsis: {text:?}");
        assert!(text.starts_with("feature-task"), "kept the leading workflow name: {text:?}");
    }
}
