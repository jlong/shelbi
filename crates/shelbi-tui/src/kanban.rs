//! Kanban Tasks view — rendered into the dashboard's right pane via the
//! same hidden-pane swap mechanism the other built-in views use. State is
//! read from / written to the task markdown files via `shelbi_state`; no
//! separate cache.
//!
//! In "All" mode the board renders the **canonical column set declared in
//! `workflows/statuses.yaml`**, in that file's declaration order. Empty
//! columns render with their headers so the layout stays stable as tasks
//! move through. Each task is bucketed by its resolved status id (see
//! [`resolve_task_status`]) regardless of which workflow it belongs to,
//! so a `review` column shows tasks from every workflow whose schema
//! includes a `review` status. With a workflow filter active the visible
//! column set narrows to the statuses that workflow declares (still in
//! `statuses.yaml` order — workflows cannot reorder).
//!
//! The pane is meant to outlive any one TUI process (the parent shell wraps
//! it in a `while true` loop) — so we deliberately don't bind a quit key.
//! Switching away is the palette's job.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame,
};
use shelbi_core::{
    default_project_statuses, default_workflow, Column, ProjectStatuses, StatusCategory, Task,
    Workflow, DEFAULT_WORKFLOW_NAME,
};
use shelbi_state::keymap::{DisplayStyle, KanbanAction, Keymaps, PopoverAction};
use shelbi_state::{KanbanColumnOverride, TaskFile};

use crate::keymap::format_chord_or_unbound;

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
    /// Declared workspace names from project.yaml, in YAML order. Reloaded
    /// on every refresh so an edited project file shows up without a
    /// restart. Empty when the project YAML is missing.
    pub workspaces: Vec<String>,
    /// Active filter — `None` means "All workspaces".
    /// [`WorkspaceFilter::Unassigned`] keeps only cards with no `assigned_to`.
    /// Mirrored to `state.json::workspace_filter` so the chip survives a
    /// respawn or project switch.
    pub workspace_filter: Option<WorkspaceFilter>,
    /// When `Some`, the workspace filter dropdown is open; carries the
    /// in-flight cursor position so navigation can move through the
    /// options list. Selection only commits on Enter/Space.
    pub workspace_dropdown: Option<WorkspaceDropdown>,
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
    /// the canonical default workflow.
    pub workflows: Vec<Workflow>,
    /// Effective project default workflow name. Defaults to the historical
    /// `default`, then refreshes from project config when available.
    pub default_workflow_name: String,
    /// Project-wide canonical column catalogue, loaded from
    /// `workflows/statuses.yaml`. Source of truth for column identity
    /// (id, name, category) and ordering — workflows can no longer
    /// reorder columns or invent their own. Reloaded each [`refresh`].
    /// Falls back to [`default_project_statuses`] when the loader
    /// errors so the board still paints.
    pub project_statuses: ProjectStatuses,
    /// The active column list: one entry per status from
    /// [`KanbanApp::project_statuses`], in that file's declared order.
    /// Selection indices ([`KanbanApp::selected_column`]) refer to slots
    /// in this vec, and the renderer walks it left-to-right. When a
    /// workflow filter is active the list narrows to the statuses that
    /// workflow declares — still in `statuses.yaml` order. Rebuilt each
    /// refresh.
    pub all_columns: Vec<KanbanColumn>,
    /// Active workflow filter — `None` means "All workflows" (the
    /// all-view rendering). When `Some(name)`,
    /// [`KanbanApp::all_columns`] is narrowed to that single workflow's
    /// columns and tasks belonging to other workflows drop out of the
    /// board. **Per-session, not persisted** — a `shelbi reload` returns
    /// the user to "All". The filter is a momentary view choice, not a
    /// preference worth surviving across runs.
    pub workflow_filter: Option<String>,
    /// When `Some`, the workflow filter dropdown is open. Same
    /// modal-cursor shape as [`KanbanApp::workspace_dropdown`] — selection
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
    /// Merged keymaps, assigned by `run_tasks` at startup from the same
    /// load that surfaces `keys.yaml` diagnostics. The board + popover
    /// footers read this to render hints in the user's configured chords.
    /// Defaults to empty — isolated render tests that don't exercise the
    /// footers leave it so.
    pub keymaps: Keymaps,
    /// Cached host-platform chord-display convention.
    pub display_style: DisplayStyle,
    /// Screen-space rects for the header band of each visible column.
    /// Used by the mouse handler to toggle collapsed/expanded state on
    /// a click. Re-written each render call by [`render_columns`].
    pub header_hits: Vec<HeaderHit>,
    /// Explicit user overrides on column collapse state, keyed by
    /// (workflow_name, status_id). Columns with no entry render in
    /// `Auto` mode — empty collapses, non-empty expands. Loaded on
    /// every [`refresh`] and rewritten through
    /// [`shelbi_state::set_kanban_column_override`] whenever a user
    /// click toggles a column. The workflow scope is the task's
    /// `workflow_or_default()` so a `review` column shared by two
    /// workflows can carry independent overrides per workflow view.
    pub column_overrides: std::collections::BTreeMap<String, KanbanColumnOverride>,
}

/// One rendered column header's screen-space rectangle plus the index
/// of the column it covers. Captured each frame by the column renderer
/// so a click on either the horizontal banner of an expanded column
/// or the rotated label of a collapsed one routes to the same toggle.
#[derive(Clone, Copy, Debug)]
pub struct HeaderHit {
    pub area: Rect,
    pub col_idx: usize,
}

/// Resolved collapse state for one column on a given frame. Combines
/// the three logical states a column can be in:
///
/// - **Auto** — no user override; non-empty columns expand, empty
///   columns collapse.
/// - **Explicit collapsed** — user clicked to collapse; respected
///   regardless of count.
/// - **Explicit expanded** — user clicked to expand; respected
///   regardless of count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnExpansion {
    Auto,
    Collapsed,
    Expanded,
}

impl ColumnExpansion {
    /// True when the column should render in the rotated single-char
    /// strip instead of the full horizontal layout.
    pub fn is_collapsed(self, task_count: usize) -> bool {
        match self {
            ColumnExpansion::Collapsed => true,
            ColumnExpansion::Expanded => false,
            ColumnExpansion::Auto => task_count == 0,
        }
    }
}

/// Workspace filter applied to the visible cards. Separate from the
/// dropdown's cursor (which can hover the same options without
/// committing) so the active filter and the in-flight selection can't
/// silently desync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceFilter {
    /// Show only tasks assigned to this workspace name.
    Workspace(String),
    /// Show only tasks with no `assigned_to`. Distinct from "All" so the
    /// user can isolate the orchestrator-untouched backlog.
    Unassigned,
}

impl WorkspaceFilter {
    /// Wire format stored in `state.json::workspace_filter`. `Unassigned`
    /// gets a sentinel string rather than its own enum variant on disk
    /// so the schema stays a plain `Option<String>` — no migration
    /// needed when older code reads the field.
    pub const UNASSIGNED_SENTINEL: &'static str = "__unassigned__";

    fn from_disk(s: &str) -> WorkspaceFilter {
        if s == Self::UNASSIGNED_SENTINEL {
            WorkspaceFilter::Unassigned
        } else {
            WorkspaceFilter::Workspace(s.to_string())
        }
    }

    fn to_disk(&self) -> String {
        match self {
            WorkspaceFilter::Workspace(w) => w.clone(),
            WorkspaceFilter::Unassigned => Self::UNASSIGNED_SENTINEL.to_string(),
        }
    }

    /// Predicate against a task's `assigned_to` field.
    pub fn matches(&self, assigned_to: Option<&str>) -> bool {
        match self {
            WorkspaceFilter::Workspace(w) => assigned_to == Some(w.as_str()),
            WorkspaceFilter::Unassigned => assigned_to.is_none(),
        }
    }

    /// Short label used in the chip and the dropdown row.
    pub fn label(&self) -> String {
        match self {
            WorkspaceFilter::Workspace(w) => w.clone(),
            WorkspaceFilter::Unassigned => "Unassigned".to_string(),
        }
    }
}

/// One entry rendered in the open dropdown — `None` is the "All
/// workspaces" reset row, `Some(filter)` is a concrete filter the user can
/// commit to. Order matches the rendered options list, so the dropdown
/// cursor and this slice can share an index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropdownOption {
    pub filter: Option<WorkspaceFilter>,
    /// How many tasks currently match this option — shown in the
    /// dropdown row as a hint. Computed at render time from the
    /// in-memory tasks slice.
    pub count: usize,
}

/// State carried while the workspace filter dropdown is open. Lives only
/// for the lifetime of the popover — selecting an option closes it and
/// drops this back to `None`.
#[derive(Debug, Clone)]
pub struct WorkspaceDropdown {
    /// Cursor row inside the options list; never out of range because
    /// [`KanbanApp::open_workspace_dropdown`] seeds it from the active
    /// filter and the nav methods clamp.
    pub cursor: usize,
}

/// State carried while the workflow filter dropdown is open. Mirrors
/// [`WorkspaceDropdown`] — just a cursor — but split into its own struct
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

/// One column in the kanban view — a status from `workflows/statuses.yaml`.
///
/// Columns are project-wide (not bound to any workflow): a single
/// `review` column shows tasks from every workflow whose schema includes
/// a `review` status. The status's [`StatusCategory`] is mirrored here so
/// the renderer can colour the header using the stock palette (Backlog
/// grey, Ready blue, Active yellow…). A card moves to this column by its
/// [`status_id`](KanbanColumn::status_id) — the destination status id is
/// the move target verbatim, so any column (including `canceled`) can hold
/// a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanColumn {
    /// Stable status id ([`shelbi_core::ProjectStatus::id`]) used for
    /// bucketing tasks into this column. Decoupled from the display
    /// label so a project that renames `Review → QA` keeps every task
    /// reference pointing at the same id.
    pub status_id: String,
    /// Status display label rendered above the column. Mirrors
    /// [`shelbi_core::ProjectStatus::name`] — free to change without
    /// breaking the matching path, which keys off [`status_id`].
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
        // Seed `all_columns` with the canonical six-status default so a
        // brand-new app pre-`refresh` already paints a board (and
        // exercises the same code path as a refresh that found exactly
        // the default `statuses.yaml` on disk). Tests that drive the app
        // directly without calling `refresh` also benefit — they don't
        // have to populate `all_columns` by hand to navigate.
        let project_statuses = default_project_statuses();
        let all_columns = kanban_columns_from(&project_statuses, None);
        Self {
            project_name: project_name.into(),
            tasks: Vec::new(),
            selected_column: 0,
            selected_row: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            popover: None,
            card_hits: Vec::new(),
            workspaces: Vec::new(),
            workspace_filter: None,
            workspace_dropdown: None,
            filter_chip_hit: None,
            dropdown_hits: Vec::new(),
            workflows: vec![default_workflow()],
            default_workflow_name: DEFAULT_WORKFLOW_NAME.to_string(),
            project_statuses,
            all_columns,
            workflow_filter: None,
            workflow_dropdown: None,
            workflow_chip_hit: None,
            workflow_dropdown_hits: Vec::new(),
            column_scroll: 0,
            visible_columns: 0..0,
            keymaps: Keymaps::default(),
            display_style: DisplayStyle::detect(),
            header_hits: Vec::new(),
            column_overrides: std::collections::BTreeMap::new(),
        }
    }

    /// Borrow the keymaps the footers render hints from. Populated by
    /// `run_tasks`; empty by default.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// Cached host-platform chord-display convention.
    pub fn display_style(&self) -> DisplayStyle {
        self.display_style
    }

    /// Hit-test a screen coordinate against the most recently rendered cards.
    /// Returns `(col_idx, row_idx)` for the card under the point, or `None`
    /// if the click missed every card (including clicks on column headers,
    /// the footer, or empty space below the last card).
    pub fn card_at(&self, x: u16, y: u16) -> Option<(usize, usize)> {
        if self.header_at(x, y).is_some() {
            return None;
        }

        self.card_hits.iter().find_map(|hit| {
            let r = hit.area;
            let in_x = x >= r.x && x < r.x.saturating_add(r.width);
            let in_y = y >= r.y && y < r.y.saturating_add(r.height);
            (in_x && in_y).then_some((hit.col_idx, hit.row_idx))
        })
    }

    /// Hit-test a screen coordinate against the most recently rendered
    /// column headers. Returns the column index whose header band
    /// (horizontal banner of an expanded column or the rotated label /
    /// count strip of a collapsed one) covers the point. Used by the
    /// mouse handler to route a header click to the collapse toggle.
    pub fn header_at(&self, x: u16, y: u16) -> Option<usize> {
        self.header_hits.iter().find_map(|hit| {
            let r = hit.area;
            let in_x = x >= r.x && x < r.x.saturating_add(r.width);
            let in_y = y >= r.y && y < r.y.saturating_add(r.height);
            (in_x && in_y).then_some(hit.col_idx)
        })
    }

    /// Persistence-key scope for a column override. Combines the column's
    /// owning view (the active workflow filter, or `default` when no filter
    /// is active) with the column's `status_id` so a `review` column in two
    /// different workflows tracks its expansion state independently.
    fn override_key_for(&self, col_idx: usize) -> Option<String> {
        let col = self.all_columns.get(col_idx)?;
        let workflow = self
            .workflow_filter
            .as_deref()
            .unwrap_or(&self.default_workflow_name);
        Some(shelbi_state::kanban_column_override_key(
            workflow,
            &col.status_id,
        ))
    }

    /// Resolved collapse state for column slot `col_idx`. Explicit
    /// overrides win; an absent override resolves to `Auto` (the
    /// renderer collapses on empty / expands otherwise).
    pub fn column_expansion(&self, col_idx: usize) -> ColumnExpansion {
        match self
            .override_key_for(col_idx)
            .and_then(|k| self.column_overrides.get(&k))
        {
            Some(KanbanColumnOverride::Collapsed) => ColumnExpansion::Collapsed,
            Some(KanbanColumnOverride::Expanded) => ColumnExpansion::Expanded,
            None => ColumnExpansion::Auto,
        }
    }

    /// Whether column slot `col_idx` should render in its collapsed
    /// (rotated-label) form on the current frame. Resolves the
    /// auto-default by consulting the column's task count.
    pub fn is_column_collapsed(&self, col_idx: usize) -> bool {
        let count = self.column_tasks(col_idx).len();
        self.column_expansion(col_idx).is_collapsed(count)
    }

    /// Flip column slot `col_idx` between collapsed and expanded. The
    /// override that lands on disk is the OPPOSITE of the column's
    /// currently-rendered state — so clicking an empty column (auto →
    /// collapsed) sets the explicit-expanded override, and clicking
    /// a non-empty column (auto → expanded) sets the explicit-collapsed
    /// override. Subsequent clicks alternate between the two explicit
    /// states; the column never silently slides back to `Auto`. Best-
    /// effort on the disk write — view state shouldn't block the UI on
    /// a transient FS error.
    pub fn toggle_column(&mut self, col_idx: usize) {
        let Some(key) = self.override_key_for(col_idx) else {
            return;
        };
        let new_state = if self.is_column_collapsed(col_idx) {
            KanbanColumnOverride::Expanded
        } else {
            KanbanColumnOverride::Collapsed
        };
        self.column_overrides.insert(key.clone(), new_state);
        let col = self.all_columns[col_idx].clone();
        let workflow = self
            .workflow_filter
            .clone()
            .unwrap_or_else(|| self.default_workflow_name.clone());
        if let Err(e) = shelbi_state::set_kanban_column_override(
            &self.project_name,
            &workflow,
            &col.status_id,
            Some(new_state),
        ) {
            self.status_line = format!("column override persist failed: {e}");
        }
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
    pub fn column(&self, idx: usize) -> &KanbanColumn {
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
            tasks.sort_by_key(|tf| std::cmp::Reverse(tf.task.updated_at));
        }
        tasks
    }

    /// True when `task` lives in the column `ac` — i.e. its resolved
    /// status id (looked up against its owning workflow) equals
    /// `ac.status_id`. Tasks from different workflows can share a
    /// column when both workflows declare the same status id. Tasks
    /// whose declared workflow is missing from `self.workflows` fall
    /// back to the legacy [`Column`]-derived id so they still show up
    /// somewhere instead of vanishing.
    ///
    /// When [`Self::workflow_filter`] is set, tasks whose
    /// `workflow_or_default()` doesn't match are filtered out — even
    /// if their resolved status id matches a visible column. This is
    /// what makes the filter actually narrow the board (otherwise two
    /// workflows that both declare `review` would share that column).
    fn task_belongs_to(&self, task: &Task, ac: &KanbanColumn) -> bool {
        if let Some(name) = &self.workflow_filter {
            if self.task_workflow_name(task) != name.as_str() {
                return false;
            }
        }
        self.resolved_status_id(task) == ac.status_id
    }

    fn task_workflow_name<'a>(&'a self, task: &'a Task) -> &'a str {
        task.workflow
            .as_deref()
            .unwrap_or(&self.default_workflow_name)
    }

    /// Resolve a task to its status id. Prefers the task's own workflow,
    /// then the default workflow, then the legacy column-derived id as
    /// a last-resort fallback.
    fn resolved_status_id(&self, task: &Task) -> String {
        let wf_name = self.task_workflow_name(task);
        if let Some(wf) = self.workflows.iter().find(|w| w.name == wf_name) {
            return resolve_task_status(task, wf);
        }
        if let Some(wf) = self
            .workflows
            .iter()
            .find(|w| w.name == DEFAULT_WORKFLOW_NAME)
        {
            return resolve_task_status(task, wf);
        }
        task.column.as_str().to_string()
    }

    /// True when `tf` passes the active workspace filter. With no filter
    /// active everything passes; otherwise the task's `assigned_to`
    /// must match the filter's predicate.
    fn task_matches_filter(&self, tf: &TaskFile) -> bool {
        match &self.workspace_filter {
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
            .map(|tf| (tf.task.id.clone(), tf.task.column.clone()))
            .collect()
    }

    pub fn selected_task(&self) -> Option<&TaskFile> {
        let col_tasks = self.column_tasks(self.selected_column);
        col_tasks.get(self.selected_row).copied()
    }

    pub fn refresh(&mut self) {
        // Project YAML may be missing on a fresh project — surface an
        // empty workspace list rather than failing the refresh; the
        // dropdown will degrade to just "All" / "Unassigned" until the
        // project file appears.
        match shelbi_state::load_project(&self.project_name) {
            Ok(p) => {
                self.default_workflow_name = p.default_workflow_name().to_string();
                self.workspaces = p.workspaces.into_iter().map(|w| w.name).collect();
            }
            Err(_) => {
                self.default_workflow_name = DEFAULT_WORKFLOW_NAME.to_string();
                self.workspaces = Vec::new();
            }
        }
        // Workspace filter is persisted view state — a missing /
        // unreadable state.json falls back to "All" silently. Reload
        // every tick so a CLI or palette edit shows up without a
        // respawn. The workflow filter is intentionally NOT loaded
        // here: it's per-session by design (resets on `shelbi reload`),
        // matching how the column-scroll position is per-session.
        let state_snapshot = shelbi_state::read_state(&self.project_name).ok();
        self.workspace_filter = state_snapshot
            .as_ref()
            .and_then(|s| s.workspace_filter.clone())
            .map(|s| WorkspaceFilter::from_disk(&s));
        self.column_overrides = state_snapshot
            .as_ref()
            .map(|s| s.kanban_column_overrides.clone())
            .unwrap_or_default();
        // Workflows are still loaded — per-task overlays and the move
        // semantics need them — but they no longer drive the column
        // layout. A broken `workflows/<name>.yaml` surfaces as a
        // status_line warning and we fall back to the canonical default
        // so the board still paints.
        self.workflows = match shelbi_state::list_workflows(&self.project_name) {
            Ok(wfs) => wfs,
            Err(e) => {
                self.status_line = format!("workflow load failed: {e}");
                vec![default_workflow()]
            }
        };
        // `statuses.yaml` is the source of truth for the column layout.
        // A broken / missing file surfaces as a status_line warning and
        // degrades to the canonical six so the board still paints.
        self.project_statuses = match shelbi_state::load_project_statuses(&self.project_name) {
            Ok(ps) => ps,
            Err(e) => {
                self.status_line = format!("statuses.yaml load failed: {e}");
                default_project_statuses()
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

    /// Options shown in the workspace dropdown — `All`, then each workspace
    /// in YAML order, then `Unassigned` if any task lacks an
    /// `assigned_to`. Counts are computed against the unfiltered task
    /// list so a workspace with zero matching cards still appears
    /// (otherwise the user couldn't pick it to clear a previously-set
    /// filter on another workspace).
    pub fn dropdown_options(&self) -> Vec<DropdownOption> {
        let mut opts: Vec<DropdownOption> = Vec::with_capacity(self.workspaces.len() + 2);
        opts.push(DropdownOption {
            filter: None,
            count: self.tasks.len(),
        });
        for w in &self.workspaces {
            let count = self
                .tasks
                .iter()
                .filter(|tf| tf.task.assigned_to.as_deref() == Some(w.as_str()))
                .count();
            opts.push(DropdownOption {
                filter: Some(WorkspaceFilter::Workspace(w.clone())),
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
                filter: Some(WorkspaceFilter::Unassigned),
                count: unassigned_count,
            });
        }
        opts
    }

    /// Index of the option that matches the active filter, used to seed
    /// the dropdown cursor when it opens. Falls back to the "All" row
    /// (idx 0) if the active filter no longer appears in the options —
    /// e.g. a workspace was removed from project.yaml while a filter on it
    /// was still persisted.
    fn current_filter_idx(&self, opts: &[DropdownOption]) -> usize {
        opts.iter()
            .position(|o| o.filter == self.workspace_filter)
            .unwrap_or(0)
    }

    pub fn workspace_dropdown_is_open(&self) -> bool {
        self.workspace_dropdown.is_some()
    }

    pub fn open_workspace_dropdown(&mut self) {
        let opts = self.dropdown_options();
        let cursor = self.current_filter_idx(&opts);
        self.workspace_dropdown = Some(WorkspaceDropdown { cursor });
    }

    pub fn close_workspace_dropdown(&mut self) {
        self.workspace_dropdown = None;
    }

    pub fn toggle_workspace_dropdown(&mut self) {
        if self.workspace_dropdown_is_open() {
            self.close_workspace_dropdown();
        } else {
            self.open_workspace_dropdown();
        }
    }

    pub fn dropdown_nav_up(&mut self) {
        let n = self.dropdown_options().len();
        if n == 0 {
            return;
        }
        if let Some(d) = self.workspace_dropdown.as_mut() {
            d.cursor = if d.cursor == 0 { n - 1 } else { d.cursor - 1 };
        }
    }

    pub fn dropdown_nav_down(&mut self) {
        let n = self.dropdown_options().len();
        if n == 0 {
            return;
        }
        if let Some(d) = self.workspace_dropdown.as_mut() {
            d.cursor = (d.cursor + 1) % n;
        }
    }

    /// Commit the cursor's option as the active filter and close the
    /// dropdown. Persists to `state.json` so the chip survives a
    /// respawn.
    pub fn dropdown_select(&mut self) {
        let opts = self.dropdown_options();
        let Some(d) = self.workspace_dropdown.as_ref() else {
            return;
        };
        let Some(opt) = opts.get(d.cursor) else {
            self.close_workspace_dropdown();
            return;
        };
        self.apply_filter(opt.filter.clone());
        self.close_workspace_dropdown();
    }

    /// Persist `filter` as the new active workspace filter and update the
    /// in-memory state. Best-effort on the disk write — view state
    /// shouldn't block the UI on a transient FS error.
    fn apply_filter(&mut self, filter: Option<WorkspaceFilter>) {
        self.workspace_filter = filter.clone();
        let disk = filter.as_ref().map(|f| f.to_disk());
        if let Err(e) = shelbi_state::set_workspace_filter(&self.project_name, disk.as_deref()) {
            self.status_line = format!("filter persist failed: {e}");
        }
        // The selection may now point past the end of a column that
        // just shrank; clamp before the next render reads it.
        self.clamp_selection();
        self.status_line = match &self.workspace_filter {
            None => "filter: all workspaces".to_string(),
            Some(f) => format!("filter: {}", f.label()),
        };
    }

    /// One-shot reset bound to the dropdown's `c` key — clears the
    /// filter without needing to navigate to the "All" row.
    pub fn dropdown_clear(&mut self) {
        self.apply_filter(None);
        self.close_workspace_dropdown();
    }

    // ----- workflow filter --------------------------------------------------

    /// Build [`Self::all_columns`] from `statuses.yaml`, honouring the
    /// active workflow filter. With no filter set this is every status
    /// declared in `statuses.yaml`, in declared order. With a filter set,
    /// only the statuses that workflow declares participate — still
    /// rendered in `statuses.yaml` order (workflows cannot reorder).
    /// If the filter targets a workflow that no longer exists in
    /// `self.workflows`, the columns fall back to the full list so the
    /// board doesn't go blank and the user can still see the chip to
    /// clear it.
    fn compute_all_columns(&self) -> Vec<KanbanColumn> {
        let workflow = match &self.workflow_filter {
            Some(name) => self.workflows.iter().find(|w| &w.name == name),
            None => None,
        };
        kanban_columns_from(&self.project_statuses, workflow)
    }

    /// Options for the workflow dropdown: `All` (count = all tasks),
    /// then each loaded workflow with its task count. A workflow with
    /// zero matching tasks still appears so the user can pick it (the
    /// scaffolding behaviour the workspace dropdown uses for the same
    /// reason).
    pub fn workflow_dropdown_options(&self) -> Vec<WorkflowDropdownOption> {
        let mut opts: Vec<WorkflowDropdownOption> = Vec::with_capacity(self.workflows.len() + 1);
        opts.push(WorkflowDropdownOption {
            filter: None,
            count: self.tasks.len(),
        });
        for w in &self.workflows {
            let count = self
                .tasks
                .iter()
                .filter(|tf| self.task_workflow_name(&tf.task) == w.name)
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

    /// Cycle the workflow filter through `None → workflows[0] →
    /// workflows[1] → ... → None`. Bound to
    /// [`KanbanAction::CycleWorkflowFilter`] (default chord `tab`) so a
    /// keyboard user can narrow the board without opening the dropdown.
    /// A single-workflow project cycles between `None` and that one
    /// workflow — degenerate but harmless.
    pub fn cycle_workflow_filter(&mut self) {
        if self.workflows.is_empty() {
            return;
        }
        let next = match &self.workflow_filter {
            None => Some(self.workflows[0].name.clone()),
            Some(current) => {
                let pos = self.workflows.iter().position(|w| &w.name == current);
                match pos {
                    Some(i) if i + 1 < self.workflows.len() => {
                        Some(self.workflows[i + 1].name.clone())
                    }
                    // Cycled past the last workflow, or the active
                    // filter targets a workflow that no longer exists —
                    // wrap back to "All" so the user always reaches the
                    // unfiltered view.
                    _ => None,
                }
            }
        };
        self.apply_workflow_filter(next);
    }

    /// Set `filter` as the active workflow filter, rebuild
    /// `all_columns`, and clamp selection so it lands on an existing
    /// column after the layout shrinks. Filter state is in-memory only
    /// — it deliberately doesn't survive a `shelbi reload` (mirrors how
    /// the column-scroll position is per-session). Mirrors
    /// [`apply_filter`] for the workspace filter otherwise.
    fn apply_workflow_filter(&mut self, filter: Option<String>) {
        self.workflow_filter = filter;
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
        let collapsed: Vec<bool> = (0..self.all_columns.len())
            .map(|i| self.is_column_collapsed(i))
            .collect();
        loop {
            let widths =
                compute_column_widths(&self.all_columns, &collapsed, self.column_scroll, area_w);
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

    /// Shove the selected card one column to the left, into the
    /// previous status its **owning workflow** declares. Cards skip
    /// columns the task's workflow doesn't include, wrapping at the
    /// workflow's first eligible status. Returns silently if the
    /// task's workflow declares only one status (nowhere to move).
    pub fn move_card_left(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let Some(new_col_idx) = self.adjacent_column_for_task(&id, false, true) else {
            return;
        };
        self.move_card(&id, new_col_idx);
    }

    pub fn move_card_right(&mut self) {
        let Some(id) = self.selected_task().map(|tf| tf.task.id.clone()) else {
            return;
        };
        let Some(new_col_idx) = self.adjacent_column_for_task(&id, true, true) else {
            return;
        };
        self.move_card(&id, new_col_idx);
    }

    /// Move the popover's task one workflow-eligible column left/right.
    /// Same `move_card` path the board's `move_card_*` uses (workflow
    /// validation, branch-cut lifecycle, event line) but **clamped** at
    /// the workflow's first/last eligible column instead of wrapping —
    /// wrapping inside the modal would teleport the card from `done`
    /// back to `backlog` with no board visible to make that legible.
    /// A clamped no-op surfaces on the status line under the popover.
    pub fn popover_move_left(&mut self) {
        self.popover_move(false);
    }

    pub fn popover_move_right(&mut self) {
        self.popover_move(true);
    }

    fn popover_move(&mut self, forward: bool) {
        let Some(id) = self.popover.as_ref().map(|p| p.task_id.clone()) else {
            return;
        };
        let Some(new_col_idx) = self.adjacent_column_for_task(&id, forward, false) else {
            let dir = if forward { "right" } else { "left" };
            self.status_line = format!("{id}: no column to the {dir} in its workflow");
            return;
        };
        self.move_card(&id, new_col_idx);
    }

    /// Find the next/prev column slot that belongs to a status the
    /// task's owning workflow declares. With `wrap`, wraps within the
    /// workflow's eligible set; otherwise clamps at the ends. Returns
    /// `None` when the task isn't currently in `self.tasks` or its
    /// workflow isn't loaded — see [`Self::adjacent_column_in_workflow`]
    /// for the underlying rule.
    fn adjacent_column_for_task(&self, task_id: &str, forward: bool, wrap: bool) -> Option<usize> {
        let task = &self.tasks.iter().find(|tf| tf.task.id == task_id)?.task;
        let wf = self
            .workflows
            .iter()
            .find(|w| w.name == self.task_workflow_name(task))?;
        let current_id = self.resolved_status_id(task);
        self.adjacent_column_in_workflow(&current_id, wf, forward, wrap)
    }

    /// Index of the next/prev column the workflow can land on, relative
    /// to `current_status_id`. Skips columns whose status the workflow
    /// doesn't declare; with `wrap` it wraps within the workflow's
    /// eligible set in `statuses.yaml` order, otherwise it clamps (the
    /// popover's behavior). Returns `None` when:
    ///
    /// - the workflow declares fewer than two statuses that appear in
    ///   the current column set (nowhere to move),
    /// - `current_status_id` isn't in the current column set (an off-
    ///   list state — refuse to invent a move), or
    /// - `wrap` is off and the task already sits at the workflow's
    ///   first/last eligible column in the requested direction.
    fn adjacent_column_in_workflow(
        &self,
        current_status_id: &str,
        workflow: &Workflow,
        forward: bool,
        wrap: bool,
    ) -> Option<usize> {
        let eligible: Vec<usize> = self
            .all_columns
            .iter()
            .enumerate()
            .filter(|(_, c)| workflow.status(&c.status_id).is_some())
            .map(|(i, _)| i)
            .collect();
        if eligible.len() <= 1 {
            return None;
        }
        let current_col_idx = self
            .all_columns
            .iter()
            .position(|c| c.status_id == current_status_id)?;
        let pos = eligible.iter().position(|&i| i == current_col_idx)?;
        let next = if forward {
            match pos + 1 {
                n if n < eligible.len() => n,
                _ if wrap => 0,
                _ => return None,
            }
        } else if pos > 0 {
            pos - 1
        } else if wrap {
            eligible.len() - 1
        } else {
            return None;
        };
        Some(eligible[next])
    }

    fn move_card(&mut self, id: &str, new_col_idx: usize) {
        // A task's position IS its status id, so the destination column's
        // status id is the move target verbatim — no lossy category round
        // trip. This is what lets a card land in `canceled` / any custom
        // column, not just one of the five stock buckets.
        let target_col = self.column(new_col_idx).clone();
        let new_col = Column::from_status_id(&target_col.status_id);
        // Lifecycle hook: when a move actually transitions a task INTO
        // `in_progress`, cut its branch on the hub (depends_on aware) and
        // persist `branch:` first — see `shelbi_orchestrator::lifecycle`.
        // If the cut fails (e.g. depends_on names a branch that doesn't
        // exist locally yet) we bail without moving the card so the YAML
        // and the git refs stay consistent.
        if new_col == Column::in_progress() {
            // By-id lookup (not a selected-column scan): the popover's
            // move path keys off the popover task, which a background
            // refresh may have drifted away from the board selection.
            if let Some(tf) = self.tasks.iter().find(|tf| tf.task.id == id) {
                if tf.task.column != Column::in_progress() {
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
                if let Err(e) = shelbi_state::append_task_event(
                    &self.project_name,
                    id,
                    &workflow,
                    from,
                    to,
                    "user:tui",
                ) {
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
    if app.workspace_dropdown_is_open() {
        render_workspace_dropdown(f, app, area);
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
    // workflow filter is meaningful); otherwise just the workspace chip.
    // Chip widths are precomputed so the left split knows exactly how
    // many columns the chips will consume — chips never wrap.
    let total = app.tasks.len();
    let workspace_text = filter_chip_text(app);
    let workflow_text = workflow_chip_text(app);
    let show_workflow_chip = app.workflows.len() > 1;
    let workspace_w = workspace_text.chars().count() as u16;
    let workflow_w = workflow_text.chars().count() as u16;
    let total_chip_w = if show_workflow_chip {
        workspace_w + workflow_w
    } else {
        workspace_w
    };

    let (left_area, workflow_area, workspace_area) = if area.width > total_chip_w {
        let mut constraints = Vec::with_capacity(3);
        constraints.push(Constraint::Min(0));
        if show_workflow_chip {
            constraints.push(Constraint::Length(workflow_w));
        }
        constraints.push(Constraint::Length(workspace_w));
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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("   {total} total"),
            Style::default().fg(Color::DarkGray),
        ),
    ]);
    f.render_widget(Paragraph::new(left), left_area);

    if let Some(area) = workflow_area {
        let style = if app.workflow_filter.is_some() {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
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

    if let Some(area) = workspace_area {
        let style = if app.workspace_filter.is_some() {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(workspace_text, style))),
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
    let label = match &app.workspace_filter {
        None => "All".to_string(),
        Some(f) => f.label(),
    };
    format!(" Workspace: {label} ▾")
}

/// Workflow filter chip text — sits left of the workspace chip when more
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
    //      get the screen real estate. Without this a board with many
    //      empty lanes (a `statuses.yaml` with eight statuses but only
    //      two in active use) would slice the screen into lanes the
    //      eye has to scan past.
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
        // Clear stale header hits so clicks can't route into the
        // previous frame's now-vanished columns.
        app.header_hits.clear();
        return;
    }
    let counts: Vec<usize> = (0..app.all_columns.len())
        .map(|i| app.column_tasks(i).len())
        .collect();
    let collapsed_states: Vec<bool> = (0..app.all_columns.len())
        .map(|i| app.is_column_collapsed(i))
        .collect();
    let widths = compute_column_widths(
        &app.all_columns,
        &collapsed_states,
        app.column_scroll,
        area.width,
    );
    let visible_start = app.column_scroll;
    let visible_end = visible_start + widths.len();

    let columns = app.task_columns();
    // Per-card workflow label: only meaningful when the board can
    // actually mix workflows in a single column — i.e. multiple
    // workflows are loaded AND no filter narrows the view to one.
    let show_card_workflow_label = app.workflows.len() > 1 && app.workflow_filter.is_none();
    let mut header_hits: Vec<HeaderHit> = Vec::with_capacity(widths.len());
    let mut x = area.x;
    for (i, w) in widths.iter().copied() {
        let slot = Rect {
            x,
            y: area.y,
            width: w,
            height: area.height,
        };
        let is_collapsed = collapsed_states.get(i).copied().unwrap_or(false);
        let count = counts.get(i).copied().unwrap_or(0);
        if is_collapsed {
            render_collapsed_column(f, app, i, slot, count, &mut header_hits);
        } else {
            render_column(
                f,
                app,
                i,
                slot,
                &columns,
                hits,
                show_card_workflow_label,
                &mut header_hits,
            );
        }
        x = x.saturating_add(w);
    }
    app.header_hits = header_hits;

    // Tiny horizontal-scroll indicators painted in the topmost row of
    // the columns area when the board can't fit everything at minimum
    // width. Anchored to the visible window's edges, not the area, so
    // the user can tell which side they can scroll toward.
    if visible_start > 0 {
        let glyph = Span::styled(
            "◂",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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

/// Minimum width for an expanded column — enough to fit the canonical
/// `IN PROGRESS (NN)` header without clipping plus the 2-char right
/// gutter cards rely on.
const EXPANDED_MIN_W: u16 = 14;
/// Minimum (and only) width for a collapsed column — one cell for the
/// rotated label glyph plus one cell of breathing room either side.
const COLLAPSED_MIN_W: u16 = 3;
/// Back-compat alias retained so existing tests reference the old name
/// for the non-collapsed minimum. Same value as [`EXPANDED_MIN_W`].
#[cfg(test)]
const NONEMPTY_MIN_W: u16 = EXPANDED_MIN_W;

/// Lay out `[scroll, scroll + k)` of `columns` into `area_w` columns of
/// terminal cells. Returns `(col_idx, width)` pairs in render order.
///
/// Two-pass algorithm:
///
/// 1. **Fit pass** — walk forward from `scroll`, assigning each column
///    its minimum width (collapsed → [`COLLAPSED_MIN_W`], expanded →
///    [`EXPANDED_MIN_W`]). Stop as soon as adding the next column
///    would push past `area_w`.
///
/// 2. **Expand pass** — divide whatever slack remains among the
///    expanded visible columns. Collapsed columns stay at the minimum
///    so they don't reclaim space the user said is "uninteresting".
///
/// At least one column always renders, even if it doesn't reach its
/// minimum — better to clip than to paint nothing at all.
///
/// `collapsed` is a parallel slice to `columns`/`counts`: index `i`
/// holds the resolved collapsed-or-not state for that column (see
/// [`ColumnExpansion::is_collapsed`]).
fn compute_column_widths(
    columns: &[KanbanColumn],
    collapsed: &[bool],
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
        let min_w = if collapsed.get(i).copied().unwrap_or(false) {
            COLLAPSED_MIN_W
        } else {
            EXPANDED_MIN_W
        };
        let next_used = used.saturating_add(min_w);
        if next_used > area_w {
            if out.is_empty() {
                // Render at least one column even if it overflows —
                // empty board with `area_w < COLLAPSED_MIN_W` only
                // happens in tiny test terminals.
                out.push((i, area_w));
                used = area_w;
            }
            break;
        }
        out.push((i, min_w));
        used = next_used;
    }
    // Expand pass: hand slack to expanded columns.
    let slack = area_w.saturating_sub(used);
    let expanded_positions: Vec<usize> = out
        .iter()
        .enumerate()
        .filter(|(_, (idx, _))| !collapsed.get(*idx).copied().unwrap_or(false))
        .map(|(pos, _)| pos)
        .collect();
    let grow_targets = if expanded_positions.is_empty() {
        // Nothing is expanded in the visible window — share the slack
        // across every visible column so we don't leave a gap on the
        // right.
        (0..out.len()).collect::<Vec<_>>()
    } else {
        expanded_positions
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

/// Render a column in its collapsed (rotated-label) form. The label
/// fills one cell per character, with a blank row preserved for each
/// space in the status name; `(<count>)` sits below the label after a
/// one-row gap. The label starts on the top row of `area` so a
/// collapsed column's first glyph lines up with the header row of its
/// expanded neighbours.
///
/// The full `area` (header band + the cells below it) is registered as
/// the column's header hit-rect so clicking anywhere on the strip
/// toggles back to the expanded view — there's nothing else to click
/// in a collapsed column.
fn render_collapsed_column(
    f: &mut Frame,
    app: &KanbanApp,
    col_idx: usize,
    area: Rect,
    count: usize,
    header_hits: &mut Vec<HeaderHit>,
) {
    let column = app.column(col_idx).clone();
    let focused = col_idx == app.selected_column;
    let header_color = category_color(column.category);
    let label_style = if focused {
        Style::default()
            .fg(header_color)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default()
            .fg(header_color)
            .add_modifier(Modifier::BOLD)
    };

    // Whole column area routes clicks to the toggle — there's nothing
    // else to interact with in this strip.
    header_hits.push(HeaderHit { area, col_idx });

    if area.height == 0 || area.width == 0 {
        return;
    }

    let label = column_label(&column.status_name);
    let label_chars: Vec<char> = label.chars().collect();
    let count_text = format!("({count})");
    let count_w = count_text.chars().count() as u16;

    let label_rows = label_chars.len() as u16;
    // Anchor the label to the top row of the column so the first
    // glyph lines up with the header row of expanded columns. Rows
    // that fall past `area.height` get clipped by the per-cell guard
    // below.
    let top = area.y;
    // Horizontal center inside the column slot.
    let glyph_x = area.x.saturating_add(area.width / 2);

    for (i, ch) in label_chars.iter().enumerate() {
        let row = top.saturating_add(i as u16);
        if row >= area.y.saturating_add(area.height) {
            break;
        }
        if *ch == ' ' {
            // Spaces in the status name render as blank rows so the
            // word break is visible.
            continue;
        }
        let cell = Rect {
            x: glyph_x,
            y: row,
            width: 1,
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(ch.to_string(), label_style))),
            cell,
        );
    }

    // Count line sits one blank row below the last label character.
    let count_y = top.saturating_add(label_rows).saturating_add(1);
    let count_bottom = area.y.saturating_add(area.height);
    if count_y < count_bottom {
        // Center the `(N)` horizontally inside the column slot. When
        // the slot is narrower than the count text, anchor left so the
        // leading paren is the first thing the user sees.
        let count_x = if area.width > count_w {
            area.x.saturating_add((area.width - count_w) / 2)
        } else {
            area.x
        };
        let count_rect = Rect {
            x: count_x,
            y: count_y,
            width: area.width.saturating_sub(count_x - area.x).min(count_w),
            height: 1,
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                count_text,
                Style::default().fg(Color::DarkGray),
            ))),
            count_rect,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_column(
    f: &mut Frame,
    app: &KanbanApp,
    col_idx: usize,
    area: Rect,
    columns: &HashMap<String, Column>,
    hits: &mut Vec<CardHit>,
    show_card_workflow_label: bool,
    header_hits: &mut Vec<HeaderHit>,
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
    let header_text = column_label(&column.status_name);
    let title_line = Line::from(vec![
        Span::styled(header_text, header_style),
        Span::styled(
            format!(" ({})", tasks.len()),
            Style::default().fg(Color::DarkGray),
        ),
    ]);

    // Columns are project-wide post-`statuses.yaml` — no per-column
    // workflow subscript any more. A single header row is all we need;
    // per-card workflow names still surface via [`card_meta_line`].
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(0)])
        .split(area);
    let header_area = chunks[0];
    let list_area = chunks[1];
    f.render_widget(Paragraph::new(title_line), header_area);
    header_hits.push(HeaderHit {
        area: header_area,
        col_idx,
    });

    // Reserve a 2-char right gutter so adjacent cells don't visually
    // collide; List doesn't clip Line spans on its own.
    let max_text = area.width.saturating_sub(2) as usize;
    // A full blank row separates consecutive cards so each reads as a
    // distinct block. Rendered as its own (unstyled) list item between
    // cards — never leading the first card or trailing the last — so the
    // selection highlight lands on the card and not the gap. The
    // hit-test loop below accounts for the same spacer when walking `y`.
    let last_row = tasks.len().saturating_sub(1);
    let mut items: Vec<ListItem> = Vec::with_capacity(tasks.len() * 2);
    // Per-card rendered height (2 rows for a single-line title, 3 when the
    // title wraps). Recorded alongside the items so the hit-test loop below
    // can walk a running `y` offset instead of assuming a uniform card.
    let mut card_heights: Vec<u16> = Vec::with_capacity(tasks.len());
    for (row, tf) in tasks.iter().enumerate() {
        let blocked = tf.task.is_blocked(columns);
        // 2-char prefix when blocked so the badge can't visually melt into
        // the title text on narrow columns. Lock glyph stays in the same
        // slot whether or not other rows show one. Kept on line 1 only.
        let badge_prefix = if blocked { "🔒 " } else { "" };
        let (line1, line2) =
            wrap_title_two_lines(&format!("{badge_prefix}{}", tf.task.title), max_text);
        let title_style = if blocked {
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().add_modifier(Modifier::BOLD)
        };
        let mut lines = vec![Line::from(Span::styled(line1, title_style))];
        if let Some(line2) = line2 {
            lines.push(Line::from(Span::styled(line2, title_style)));
        }
        // Meta line: workflow badge (bg-filled) + `⎇ branch`. Always emitted
        // — even when blank — so every card carries a title+meta pair and the
        // height stays 2 (single-line title) or 3 (wrapped title) rows.
        lines.push(card_meta_line(
            &tf.task,
            &app.workflows,
            app.task_workflow_name(&tf.task),
            show_card_workflow_label,
            max_text,
        ));

        card_heights.push(lines.len() as u16);

        let mut item = ListItem::new(lines);
        if focused && row == app.selected_row {
            item = item.style(
                Style::default()
                    .bg(crate::theme::SELECTION_BG)
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            );
        }
        items.push(item);
        // One blank spacer between cards — never after the last.
        if row != last_row {
            items.push(ListItem::new(Line::from("")));
        }
    }
    if items.is_empty() {
        items.push(ListItem::new(Line::from(Span::styled(
            "(empty)",
            Style::default().fg(Color::DarkGray),
        ))));
    }

    let mut state = ListState::default();
    if focused && !tasks.is_empty() {
        // Each card is preceded by a one-row spacer item (except the
        // first), so the selected card sits at list index `row * 2`.
        state.select(Some(app.selected_row * 2));
    }
    let list = List::new(items);
    f.render_stateful_widget(list, list_area, &mut state);

    // Cards are now variable height: 2 rows for a single-line title, 3 when
    // the title wraps to two lines (see `card_heights`, filled as the items
    // were built). Walk a running `y` offset so each card's `CardHit` rect
    // matches its actual rendered footprint. Clip the last visible card if
    // it overflows the list area so clicks far below it don't count. Scroll
    // offsets aren't tracked here — a long column that scrolls past its
    // visible area will mis-attribute clicks for any row off-screen;
    // tolerable until columns regularly exceed visible height.
    let list_bottom = list_area.y.saturating_add(list_area.height);
    let mut card_top = list_area.y;
    let last_row = card_heights.len().saturating_sub(1);
    for (row, &rows) in card_heights.iter().enumerate() {
        if card_top >= list_bottom {
            break;
        }
        let card_height = (list_bottom - card_top).min(rows);
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
        card_top = card_top.saturating_add(rows);
        // Skip the blank spacer row rendered between cards (not after the
        // last one). No CardHit is recorded for it, so a click in the gap
        // misses every card — matching the un-highlighted empty row.
        if row != last_row {
            card_top = card_top.saturating_add(1);
        }
    }
}

fn render_footer(f: &mut Frame, app: &KanbanApp, area: Rect) {
    // Nav / move / reorder / open / refresh / workflow glyphs come from
    // the merged keymaps, rendered in the host platform's convention.
    // `filter` (workspace dropdown) is still ad-hoc on `f`, so it stays
    // literal. Multi-bound actions show their first chord.
    let km = app.keymaps();
    let style = app.display_style();
    let fc = |c| format_chord_or_unbound(c, style);
    let text = format!(
        "  {}/{} col   {}/{} row   {} open   {}/{} move col   {}/{} reorder   {} workflow   f filter   {} refresh",
        fc(km.kanban.first_chord_for(KanbanAction::NavLeft)),
        fc(km.kanban.first_chord_for(KanbanAction::NavRight)),
        fc(km.kanban.first_chord_for(KanbanAction::NavDown)),
        fc(km.kanban.first_chord_for(KanbanAction::NavUp)),
        fc(km.kanban.first_chord_for(KanbanAction::OpenPopover)),
        fc(km.kanban.first_chord_for(KanbanAction::MoveCardLeft)),
        fc(km.kanban.first_chord_for(KanbanAction::MoveCardRight)),
        fc(km.kanban.first_chord_for(KanbanAction::ReorderUp)),
        fc(km.kanban.first_chord_for(KanbanAction::ReorderDown)),
        fc(km.kanban.first_chord_for(KanbanAction::CycleWorkflowFilter)),
        fc(km.kanban.first_chord_for(KanbanAction::Refresh)),
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

/// Render the workspace filter dropdown as a small popover anchored under
/// the filter chip. The popover paints over the column headers + cards
/// (`Clear` strips whatever was underneath), but does NOT suppress the
/// title-row chip itself — keeping the chip visible while the dropdown
/// is open is the visual link between the two.
fn render_workspace_dropdown(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
    let opts = app.dropdown_options();
    if opts.is_empty() {
        // Defensive: an empty options list means there's nothing to
        // pick. Close it so a stale dropdown doesn't linger.
        app.close_workspace_dropdown();
        return;
    }
    let cursor = app
        .workspace_dropdown
        .as_ref()
        .map(|d| d.cursor)
        .unwrap_or(0);

    // Width = widest "Workspace name (count)" row + 4 chars of chrome
    // (border + arrow + padding). Cap so the popover never spans more
    // than ~⅓ of the screen — narrow lists shouldn't sprawl. The lower
    // bound is sized to fit the " Filter by workspace " title.
    let max_label_w = opts
        .iter()
        .map(|o| dropdown_row_text(o).chars().count())
        .max()
        .unwrap_or(10);
    let desired_w = (max_label_w + 4).max(24) as u16;
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
            " Filter by workspace ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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
        let active = app.workspace_filter == opt.filter;
        let label = dropdown_row_text(opt);
        // Active filter gets a leading bullet so the user can tell at
        // a glance which row is currently applied. The selected row
        // (cursor) gets the shared selection-gray bg via List
        // highlight_style below.
        let prefix = if active { "● " } else { "  " };
        let label_style = if active {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
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
            .bg(crate::theme::SELECTION_BG)
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

/// Workflow filter dropdown — same anchor logic as the workspace one but
/// keyed off [`KanbanApp::workflow_chip_hit`] so it drops below the
/// workflow chip. Kept as a separate function from the workspace dropdown
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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
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
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
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
            .bg(crate::theme::SELECTION_BG)
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
/// separate avoids overloading the workspace-side row formatter with a
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
        // Accept both the post-split "In Progress" (matches the spec
        // wireframe) and the pre-split "InProgress" written into legacy
        // workflow YAMLs that haven't been migrated yet.
        "In Progress" | "InProgress" => "IN PROGRESS".to_string(),
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
fn column_color(c: &Column) -> Color {
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

/// Wrap `title` to at most two lines within `max` columns, using char
/// count as the width proxy (matching [`truncate`] and the rest of the
/// card layout, which don't account for wide glyphs). Line 1 fills the
/// width, breaking on the last space that fits so a word isn't split when
/// avoidable; a first token longer than the width hard-wraps at `max`.
/// Line 2 holds the remainder, truncated with `…` when it overflows.
///
/// Returns `(line1, None)` when the title fits on one line, so the caller
/// can render a 2-row card; `(line1, Some(line2))` when it wraps (3-row
/// card).
fn wrap_title_two_lines(title: &str, max: usize) -> (String, Option<String>) {
    if max == 0 {
        return (String::new(), None);
    }
    let chars: Vec<char> = title.chars().collect();
    if chars.len() <= max {
        return (title.to_string(), None);
    }
    // Prefer the last space at or before `max` so line 1 ends on a word
    // boundary; ignore a space at index 0 (would leave line 1 empty) and
    // hard-wrap at `max` when the first token alone overflows the width.
    let break_idx = (1..=max).rev().find(|&i| chars[i] == ' ');
    let (line1_end, remainder_start) = match break_idx {
        Some(i) => (i, i + 1), // drop the breaking space
        None => (max, max),    // hard wrap mid-token
    };
    let line1: String = chars[..line1_end].iter().collect();
    let remainder: String = chars[remainder_start..].iter().collect();
    (line1, Some(truncate(&remainder, max)))
}

/// Resolve the branch to surface on a card's meta line. Prefers the
/// task's own `branch` (the working branch, once the orchestrator has cut
/// it) and otherwise falls back to the workflow's `git.base_branch`
/// template when it carries a `{{var}}` placeholder that resolves cleanly
/// from this task's params. A fixed `base_branch: main` (no placeholder)
/// or a half-substituted template resolves to `None` — neither adds
/// trustworthy per-task info at the card-glance scale.
fn card_branch(task: &Task, workflows: &[Workflow], workflow_name: &str) -> Option<String> {
    if let Some(branch) = task.branch.as_deref().filter(|b| !b.is_empty()) {
        return Some(branch.to_string());
    }
    workflows
        .iter()
        .find(|w| w.name == workflow_name)
        .and_then(|w| w.git.as_ref())
        .and_then(|g| g.base_branch.as_deref())
        .filter(|tmpl| tmpl.contains("{{"))
        .and_then(|tmpl| {
            let mut missing = Vec::new();
            let resolved =
                shelbi_core::substitute_placeholders(tmpl, &task.string_params(), &mut missing);
            missing.is_empty().then_some(resolved)
        })
}

/// Build the card's meta line: the task's workflow name rendered as a
/// small background-filled badge, followed on the same line by the branch
/// prefixed with `⎇ ` (dim, truncated to the remaining width).
///
/// `show_workflow_name` mirrors the column-header `show_workflow_label`:
/// true when the visible board mixes more than one workflow, so naming
/// each card's workflow disambiguates which lane it belongs to. When
/// false, the badge is suppressed (the column header already implies it)
/// and the meta line is just `⎇ branch`. When there's neither a badge nor
/// a branch to show, returns `Line::raw("")` — the row is still emitted so
/// the card keeps its title+meta shape, it just renders blank.
fn card_meta_line(
    task: &Task,
    workflows: &[Workflow],
    workflow_name: &str,
    show_workflow_name: bool,
    max_text: usize,
) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut used = 0usize;

    if show_workflow_name {
        // One space of padding either side so the background reads as a
        // chip rather than butting straight against the glyphs.
        let badge = truncate(&format!(" {workflow_name} "), max_text);
        used += badge.chars().count();
        spans.push(Span::styled(
            badge,
            Style::default()
                .bg(crate::theme::WORKFLOW_BADGE_BG)
                .fg(crate::theme::WORKFLOW_BADGE_FG),
        ));
    }

    if let Some(branch) = card_branch(task, workflows, workflow_name) {
        // A single space separates the badge from the branch; skip it when
        // the badge is hidden so the branch sits flush with the gutter.
        let sep = if spans.is_empty() { 0 } else { 1 };
        let remaining = max_text.saturating_sub(used + sep);
        if remaining > 0 {
            if sep == 1 {
                spans.push(Span::raw(" "));
            }
            spans.push(Span::styled(
                truncate(&format!("⎇ {branch}"), remaining),
                Style::default().fg(Color::DarkGray),
            ));
        }
    }

    if spans.is_empty() {
        return Line::raw("");
    }
    Line::from(spans)
}

// ---------------------------------------------------------------------------
// Column builder

/// Build the column list from `statuses.yaml`. When `workflow` is `None`
/// every status declared in the file is emitted (in declared order).
/// When `Some`, the list narrows to the statuses that workflow declares
/// — still in `statuses.yaml` order, since the loader rejects workflow
/// files that try to reorder relative to the project-wide catalogue.
fn kanban_columns_from(
    statuses: &ProjectStatuses,
    workflow: Option<&Workflow>,
) -> Vec<KanbanColumn> {
    let allowed: Option<HashSet<&str>> =
        workflow.map(|wf| wf.statuses.iter().map(|s| s.id.as_str()).collect());
    statuses
        .statuses
        .iter()
        .filter(|st| {
            allowed
                .as_ref()
                .map_or(true, |set| set.contains(st.id.as_str()))
        })
        .map(|st| KanbanColumn {
            status_id: st.id.clone(),
            status_name: st.name.clone(),
            category: st.category,
        })
        .collect()
}

/// Resolve the workflow status *id* `task` lives in. A task's position is
/// itself a status id ([`Column::as_str`]), so this is mostly an identity
/// — the fallbacks only matter when the task's stored id isn't one the
/// workflow declares (a workflow that renamed a status).
///
/// Resolution order, mirroring the events log writer's behaviour:
///
/// 1. **Id match** — if the workflow declares a status whose id equals the
///    task's stored position id, use it. Covers the common case.
/// 2. **Category match** — fall back to the first status in the workflow
///    whose category equals the task's position category. Handles a custom
///    workflow that renamed `in-progress` to `design` (both report
///    `StatusCategory::Active`).
/// 3. **Canonical** — if the workflow declares no status that matches by
///    id or category, return the stored id unchanged. The task won't
///    bucket cleanly, but the renderer never crashes.
fn resolve_task_status(task: &Task, workflow: &Workflow) -> String {
    let stored = task.column.as_str();
    if workflow.status(stored).is_some() {
        return stored.to_string();
    }
    let cat = task.column.category();
    if let Some(st) = workflow.statuses.iter().find(|s| s.category == cat) {
        return st.id.clone();
    }
    stored.to_string()
}

// ---------------------------------------------------------------------------
// Task detail popover

fn render_popover(f: &mut Frame, app: &mut KanbanApp, area: Rect) {
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
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
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

    // Pre-wrap so inline-code highlights stay within the popover's inner
    // width — see `markdown::render_note`.
    let lines = crate::markdown::render_note(&body_text, chunks[2].width as usize);
    // Clamp scroll at the bottom against this frame's wrapped-line count,
    // same render-time clamp the activity feed uses — the scroll methods
    // only saturate at the top and can't know the wrapped height.
    let total_lines = u16::try_from(lines.len()).unwrap_or(u16::MAX);
    let max_scroll = total_lines.saturating_sub(chunks[2].height);
    if let Some(p) = app.popover.as_mut() {
        if p.scroll > max_scroll {
            p.scroll = max_scroll;
        }
    }
    let scroll = app.popover.as_ref().map(|p| p.scroll).unwrap_or(0);
    let body = Paragraph::new(lines).scroll((scroll, 0));
    f.render_widget(body, chunks[2]);

    // First-chord-only: the popover's `close` and `scroll` actions each
    // carry several bindings (esc/enter/space/q, j/k/↑/↓), but the hint
    // shows just the first — the full list lives in `config list-actions`.
    let km = app.keymaps();
    let style = app.display_style();
    let fc = |c| format_chord_or_unbound(c, style);
    let hint_text = format!(
        "  {}  close      {}/{}  scroll      {}  top      {}/{}  move col",
        fc(km.popover.first_chord_for(PopoverAction::Close)),
        fc(km.popover.first_chord_for(PopoverAction::ScrollDown)),
        fc(km.popover.first_chord_for(PopoverAction::ScrollUp)),
        fc(km.popover.first_chord_for(PopoverAction::ScrollHome)),
        fc(km.popover.first_chord_for(PopoverAction::MoveLeft)),
        fc(km.popover.first_chord_for(PopoverAction::MoveRight)),
    );
    let hint = Line::from(Span::styled(
        hint_text,
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(Paragraph::new(hint), chunks[3]);
}

fn popover_header(tf: &TaskFile, columns: &HashMap<String, Column>) -> Vec<Line<'static>> {
    let task = &tf.task;
    let mut lines: Vec<Line<'static>> = Vec::new();

    lines.push(meta_row("id", &task.id));
    let col_label = column_label(task.column.default_status_name());
    let col_span = Span::styled(col_label, Style::default().fg(column_color(&task.column)));
    let mut col_line = vec![meta_label("column"), col_span];
    col_line.push(Span::raw("   "));
    col_line.push(meta_label("priority"));
    col_line.push(Span::raw(format!("{}", task.priority)));
    if let Some(w) = &task.assigned_to {
        col_line.push(Span::raw("   "));
        col_line.push(meta_label("workspace"));
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
            let dep_col = columns.get(dep);
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
    Span::styled(format!("{label}: "), Style::default().fg(Color::DarkGray))
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
        CardHit {
            area,
            col_idx,
            row_idx,
        }
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
            task_file("old", Column::done(), 0, "2026-06-20T10:00:00Z"),
            task_file("newest", Column::done(), 1, "2026-06-22T10:00:00Z"),
            task_file("middle", Column::done(), 2, "2026-06-21T10:00:00Z"),
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
            task_file("a", Column::backlog(), 0, "2026-06-20T10:00:00Z"),
            task_file("b", Column::backlog(), 1, "2026-06-22T10:00:00Z"),
            task_file("c", Column::backlog(), 2, "2026-06-21T10:00:00Z"),
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
            task_file("first", Column::done(), 0, "2026-06-20T10:00:00Z"),
            task_file("second", Column::done(), 1, "2026-06-20T10:00:00Z"),
            task_file("third", Column::done(), 2, "2026-06-20T10:00:00Z"),
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
            hit(
                Rect {
                    x: 0,
                    y: 2,
                    width: 20,
                    height: 3,
                },
                0,
                0,
            ),
            hit(
                Rect {
                    x: 0,
                    y: 5,
                    width: 20,
                    height: 3,
                },
                0,
                1,
            ),
            hit(
                Rect {
                    x: 20,
                    y: 2,
                    width: 20,
                    height: 3,
                },
                1,
                0,
            ),
        ];
        assert_eq!(app.card_at(5, 2), Some((0, 0)));
        assert_eq!(app.card_at(5, 4), Some((0, 0))); // last row of card
        assert_eq!(app.card_at(5, 5), Some((0, 1))); // first row of next card
        assert_eq!(app.card_at(25, 3), Some((1, 0))); // adjacent column
    }

    #[test]
    fn card_at_misses_outside_any_rect() {
        let mut app = KanbanApp::new("demo");
        app.card_hits = vec![hit(
            Rect {
                x: 0,
                y: 2,
                width: 20,
                height: 3,
            },
            0,
            0,
        )];
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
            column: Column::todo(),
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
        assert!(
            lines[0].contains(" workflow=default "),
            "line: {}",
            lines[0]
        );
        assert!(
            lines[0].contains(" todo -> in_progress "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].contains(" reason=user:tui "), "line: {}", lines[0]);
        assert!(
            lines[0].ends_with(" to_category=active"),
            "line: {}",
            lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The popover's move shortcut drives the same `move_card` path the
    /// board uses: the task file's column persists, the `task=` event
    /// line lands in `~/.shelbi/events.log`, and the popover stays open
    /// on the moved task while the board selection follows it.
    #[test]
    fn popover_move_right_persists_emits_event_and_keeps_popover_open() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-popover-move-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        // Moving into `in_progress` fires the branch-cut lifecycle hook,
        // which needs a loadable project and a git repo at the hub workdir.
        crate::test_support::provision_hub_repo_for_project(&home, "demo");

        use chrono::Utc;
        let now = Utc::now();
        let task = shelbi_core::Task {
            id: "fix-login".into(),
            title: "fix login".into(),
            column: Column::todo(),
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
        app.selected_column = 1; // todo
        app.selected_row = 0;
        app.open_popover();
        assert!(app.popover_is_open());

        app.popover_move_right();

        // Same status-move path as the board: event line emitted…
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1, "log: {log:?}");
        assert!(lines[0].contains(" task=fix-login "), "line: {}", lines[0]);
        assert!(
            lines[0].contains(" todo -> in_progress "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].contains(" reason=user:tui "), "line: {}", lines[0]);

        // …popover follows the task into its new column…
        let p = app.popover.as_ref().expect("popover stays open");
        assert_eq!(p.task_id, "fix-login");
        assert_eq!(
            app.popover_task().unwrap().task.column,
            Column::in_progress(),
            "popover shows the task in its new column"
        );
        // …and the board selection underneath follows too.
        assert_eq!(app.selected_column, 2);
        assert_eq!(app.selected_row, 0);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// At the workflow's first eligible column the popover move clamps:
    /// no move, no event, popover open, and the status line explains why
    /// (the board's `move_card_*` wraps instead — the modal must not
    /// teleport the card across the board while it's hidden).
    #[test]
    fn popover_move_left_clamps_at_first_column_with_feedback() {
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![{
            let now = chrono::Utc::now();
            TaskFile {
                task: shelbi_core::Task {
                    id: "task-1".into(),
                    title: "task-1".into(),
                    column: Column::backlog(),
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
                },
                body: String::new(),
            }
        }];
        app.popover = Some(TaskPopover {
            task_id: "task-1".into(),
            scroll: 0,
        });

        app.popover_move_left();

        assert_eq!(
            app.tasks[0].task.column,
            Column::backlog(),
            "clamped move must not change the column"
        );
        assert!(app.popover_is_open(), "popover survives a clamped no-op");
        assert!(
            app.status_line.contains("no column to the left"),
            "status line: {:?}",
            app.status_line
        );
    }

    // ---- workspace filter ---------------------------------------------------

    /// `column_tasks` applies both the column filter and the active
    /// workspace filter — a workspace filter shrinks every column at once,
    /// not just the focused one.
    #[test]
    fn column_tasks_applies_workspace_filter() {
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![
            task_file_for(
                "a",
                Column::todo(),
                0,
                "2026-06-20T10:00:00Z",
                Some("alpha"),
            ),
            task_file_for(
                "b",
                Column::todo(),
                1,
                "2026-06-20T10:00:00Z",
                Some("bravo"),
            ),
            task_file_for(
                "c",
                Column::in_progress(),
                0,
                "2026-06-20T10:00:00Z",
                Some("alpha"),
            ),
            task_file_for("d", Column::todo(), 2, "2026-06-20T10:00:00Z", None),
        ];
        // No filter — all tasks pass through their column filter.
        assert_eq!(app.column_tasks(1).len(), 3);
        assert_eq!(app.column_tasks(2).len(), 1);

        app.workspace_filter = Some(WorkspaceFilter::Workspace("alpha".into()));
        let todo: Vec<&str> = app
            .column_tasks(1)
            .iter()
            .map(|t| t.task.id.as_str())
            .collect();
        assert_eq!(todo, vec!["a"]);
        let wip: Vec<&str> = app
            .column_tasks(2)
            .iter()
            .map(|t| t.task.id.as_str())
            .collect();
        assert_eq!(wip, vec!["c"]);

        app.workspace_filter = Some(WorkspaceFilter::Unassigned);
        let todo: Vec<&str> = app
            .column_tasks(1)
            .iter()
            .map(|t| t.task.id.as_str())
            .collect();
        assert_eq!(
            todo,
            vec!["d"],
            "Unassigned filter keeps only `assigned_to: None`"
        );
    }

    /// `dropdown_options` builds `All` + each workspace + (optional)
    /// `Unassigned`, in that order. Counts reflect the unfiltered task
    /// list so workspaces with zero matching cards still appear (the user
    /// has to be able to pick them to clear an unrelated filter).
    #[test]
    fn dropdown_options_lists_all_then_workspaces_then_unassigned() {
        let mut app = KanbanApp::new("demo");
        app.workspaces = vec!["alpha".into(), "bravo".into(), "charlie".into()];
        app.tasks = vec![
            task_file_for(
                "a",
                Column::todo(),
                0,
                "2026-06-20T10:00:00Z",
                Some("alpha"),
            ),
            task_file_for(
                "b",
                Column::todo(),
                1,
                "2026-06-20T10:00:00Z",
                Some("alpha"),
            ),
            task_file_for(
                "c",
                Column::todo(),
                2,
                "2026-06-20T10:00:00Z",
                Some("bravo"),
            ),
            task_file_for("d", Column::todo(), 3, "2026-06-20T10:00:00Z", None),
        ];
        let opts = app.dropdown_options();
        // All / alpha(2) / bravo(1) / charlie(0) / Unassigned(1)
        assert_eq!(opts.len(), 5);
        assert!(opts[0].filter.is_none());
        assert_eq!(opts[0].count, 4);
        assert_eq!(
            opts[1].filter,
            Some(WorkspaceFilter::Workspace("alpha".into()))
        );
        assert_eq!(opts[1].count, 2);
        assert_eq!(
            opts[3].filter,
            Some(WorkspaceFilter::Workspace("charlie".into()))
        );
        assert_eq!(opts[3].count, 0, "zero-count workspaces still appear");
        assert_eq!(opts[4].filter, Some(WorkspaceFilter::Unassigned));
        assert_eq!(opts[4].count, 1);
    }

    /// No tasks lack `assigned_to` → the Unassigned row is suppressed
    /// so the dropdown isn't padded with a useless option.
    #[test]
    fn dropdown_options_omits_unassigned_when_zero() {
        let mut app = KanbanApp::new("demo");
        app.workspaces = vec!["alpha".into()];
        app.tasks = vec![task_file_for(
            "a",
            Column::todo(),
            0,
            "2026-06-20T10:00:00Z",
            Some("alpha"),
        )];
        let opts = app.dropdown_options();
        assert!(
            !opts
                .iter()
                .any(|o| o.filter == Some(WorkspaceFilter::Unassigned)),
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
        app.workspaces = vec!["alpha".into(), "bravo".into()];
        app.tasks = vec![
            task_file_for(
                "a",
                Column::todo(),
                0,
                "2026-06-20T10:00:00Z",
                Some("alpha"),
            ),
            task_file_for(
                "b",
                Column::todo(),
                1,
                "2026-06-20T10:00:00Z",
                Some("bravo"),
            ),
        ];
        app.workspace_filter = Some(WorkspaceFilter::Workspace("bravo".into()));
        app.open_workspace_dropdown();
        // Options are [All, alpha, bravo] → bravo is index 2.
        assert_eq!(app.workspace_dropdown.as_ref().unwrap().cursor, 2);

        app.dropdown_nav_down();
        assert_eq!(
            app.workspace_dropdown.as_ref().unwrap().cursor,
            0,
            "wraps to top"
        );
        app.dropdown_nav_up();
        assert_eq!(
            app.workspace_dropdown.as_ref().unwrap().cursor,
            2,
            "wraps to bottom"
        );
    }

    /// An active filter that no longer matches any option (e.g. a
    /// workspace was removed from project.yaml) seeds the cursor on `All`
    /// rather than panicking on an out-of-range index. The active
    /// filter itself isn't auto-cleared — only the cursor lands at 0.
    #[test]
    fn dropdown_open_falls_back_to_all_when_filter_missing_from_options() {
        let mut app = KanbanApp::new("demo");
        app.workspaces = vec!["alpha".into()];
        app.workspace_filter = Some(WorkspaceFilter::Workspace("removed".into()));
        app.open_workspace_dropdown();
        assert_eq!(app.workspace_dropdown.as_ref().unwrap().cursor, 0);
    }

    /// `apply_filter` (via `dropdown_select`) updates the in-memory
    /// state, persists to `state.json`, and a fresh app picks it up on
    /// refresh. The disk format stores the sentinel for Unassigned so
    /// the schema stays a plain `Option<String>`.
    #[test]
    fn apply_filter_persists_to_state_json_and_refresh_restores() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-workspace-filter-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // Project needs to exist so refresh() can populate workspaces.
        let project = shelbi_core::Project {
            name: "demo".into(),
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            git: shelbi_core::GitConfig::default(),
            machines: vec![shelbi_core::Machine {
                name: "hub".into(),
                kind: shelbi_core::MachineKind::Local,
                work_dir: "/tmp/demo".into(),
                host: None,
                tags: Vec::new(),
                forward: None,
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
                        prompt_injection: None,
                        dialog_signatures: vec![],
                    },
                );
                m
            },
            editor: None,
            github_url: None,
            workspaces: vec![
                shelbi_core::WorkspaceSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                shelbi_core::WorkspaceSpec {
                    name: "bravo".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
        };
        shelbi_state::save_project(&project).unwrap();

        let mut app = KanbanApp::new("demo");
        app.refresh();
        assert!(
            app.workspace_filter.is_none(),
            "fresh state.json → no filter"
        );
        assert_eq!(
            app.workspaces,
            vec!["alpha".to_string(), "bravo".to_string()]
        );

        // Open, navigate to the bravo row (All=0, alpha=1, bravo=2),
        // commit. The dropdown closes itself.
        app.open_workspace_dropdown();
        app.dropdown_nav_down();
        app.dropdown_nav_down();
        app.dropdown_select();
        assert!(!app.workspace_dropdown_is_open());
        assert_eq!(
            app.workspace_filter,
            Some(WorkspaceFilter::Workspace("bravo".into()))
        );

        // A fresh app rehydrates the same filter from disk.
        let mut app2 = KanbanApp::new("demo");
        app2.refresh();
        assert_eq!(
            app2.workspace_filter,
            Some(WorkspaceFilter::Workspace("bravo".into()))
        );

        // Unassigned round-trips through the sentinel.
        app2.open_workspace_dropdown();
        // Options: All / alpha / bravo / Unassigned? — we need an
        // unassigned task to surface it. Seed one and refresh.
        let now = chrono::Utc::now();
        shelbi_state::save_task(
            "demo",
            &shelbi_core::Task {
                id: "orphan".into(),
                title: "orphan".into(),
                column: Column::backlog(),
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
        app2.open_workspace_dropdown();
        let opts = app2.dropdown_options();
        let unassigned_idx = opts
            .iter()
            .position(|o| o.filter == Some(WorkspaceFilter::Unassigned))
            .expect("Unassigned row should now appear");
        if let Some(d) = app2.workspace_dropdown.as_mut() {
            d.cursor = unassigned_idx;
        }
        app2.dropdown_select();
        assert_eq!(app2.workspace_filter, Some(WorkspaceFilter::Unassigned));
        let on_disk = shelbi_state::read_state("demo").unwrap();
        assert_eq!(
            on_disk.workspace_filter.as_deref(),
            Some(WorkspaceFilter::UNASSIGNED_SENTINEL),
            "Unassigned must serialize to its sentinel string"
        );

        // `dropdown_clear` resets to None and writes that through to
        // disk so a subsequent refresh sees the cleared filter.
        app2.open_workspace_dropdown();
        app2.dropdown_clear();
        assert_eq!(app2.workspace_filter, None);
        let on_disk = shelbi_state::read_state("demo").unwrap();
        assert!(on_disk.workspace_filter.is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Selecting the `All` row clears the filter even when it was
    /// previously set to a specific workspace — the dropdown commit path
    /// has to accept `None` as a valid choice.
    #[test]
    fn dropdown_select_all_clears_filter() {
        // `dropdown_select` persists through `set_workspace_filter`, a
        // real disk write keyed off whatever `SHELBI_HOME` is set at that
        // instant. Unlocked, this test raced the persistence tests and
        // cleared the filter they had just written into *their* temp home
        // (both use project "demo"), so hold `ENV_LOCK` and write into a
        // throwaway home of our own.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-dropdown-clear-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let mut app = KanbanApp::new("demo");
        app.workspaces = vec!["alpha".into()];
        app.workspace_filter = Some(WorkspaceFilter::Workspace("alpha".into()));
        app.open_workspace_dropdown();
        // Cursor seeds on alpha (idx 1). Walk back to All.
        app.dropdown_nav_up();
        assert_eq!(app.workspace_dropdown.as_ref().unwrap().cursor, 0);
        app.dropdown_select();
        assert!(app.workspace_filter.is_none());
        assert!(!app.workspace_dropdown_is_open());

        std::env::remove_var("SHELBI_HOME");
    }

    /// Toggling the dropdown closes it when open, opens when closed —
    /// matches the chord-style "press the same key to dismiss" UX.
    #[test]
    fn toggle_workspace_dropdown_is_self_inverse() {
        let mut app = KanbanApp::new("demo");
        assert!(!app.workspace_dropdown_is_open());
        app.toggle_workspace_dropdown();
        assert!(app.workspace_dropdown_is_open());
        app.toggle_workspace_dropdown();
        assert!(!app.workspace_dropdown_is_open());
    }

    /// Rendering the kanban with the dropdown open must show the
    /// chip in the title row, paint every workspace option (plus All), and
    /// populate `dropdown_hits` so click routing has rects to test
    /// against.
    #[test]
    fn rendering_dropdown_paints_options_and_chip() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        app.workspaces = vec!["alpha".into(), "bravo".into()];
        app.tasks = vec![
            task_file_for(
                "a",
                Column::todo(),
                0,
                "2026-06-20T10:00:00Z",
                Some("alpha"),
            ),
            task_file_for(
                "b",
                Column::todo(),
                1,
                "2026-06-20T10:00:00Z",
                Some("bravo"),
            ),
        ];
        app.open_workspace_dropdown();

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

        assert!(
            joined.contains("Workspace: All ▾"),
            "chip missing in:\n{joined}"
        );
        assert!(
            joined.contains("Filter by workspace"),
            "dropdown title missing"
        );
        assert!(joined.contains("All (2)"), "All row missing in:\n{joined}");
        assert!(
            joined.contains("alpha (1)"),
            "alpha row missing in:\n{joined}"
        );
        assert!(
            joined.contains("bravo (1)"),
            "bravo row missing in:\n{joined}"
        );

        assert!(app.filter_chip_hit.is_some(), "chip rect not recorded");
        assert_eq!(
            app.dropdown_hits.len(),
            3,
            "expected 3 dropdown hits (All + 2 workspaces), got {}",
            app.dropdown_hits.len()
        );
    }

    /// Scrolling past the end of the popover body (wheel or keyboard —
    /// both only saturate at the top) must clamp at the bottom on the
    /// next render, leaving the body's last line visible instead of a
    /// blank pane. Same render-time clamp the activity feed uses.
    #[test]
    fn popover_render_clamps_scroll_at_bottom() {
        use ratatui::{backend::TestBackend, Terminal};
        let body: String = (1..=60)
            .map(|i| format!("body line {i}\n"))
            .collect();
        let mut tf = task_file("task-1", Column::todo(), 0, "2026-06-20T10:00:00Z");
        tf.body = body;
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![tf];
        app.popover = Some(TaskPopover {
            task_id: "task-1".into(),
            scroll: u16::MAX,
        });

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let clamped = app.popover.as_ref().unwrap().scroll;
        assert!(
            clamped < u16::MAX,
            "render must clamp an overshot scroll offset"
        );

        let buf = term.backend().buffer().clone();
        let joined: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            joined.contains("body line 60"),
            "at max scroll the last body line is visible, got:\n{joined}"
        );

        // A second render must not move the offset — the clamp is a
        // fixed point, not a per-frame decrement.
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        assert_eq!(app.popover.as_ref().unwrap().scroll, clamped);
    }

    /// A body shorter than the popover viewport clamps to zero — no
    /// scrolling into blank space below a short note.
    #[test]
    fn popover_render_clamps_short_body_to_zero() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut tf = task_file("task-1", Column::todo(), 0, "2026-06-20T10:00:00Z");
        tf.body = "just one line".to_string();
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![tf];
        app.popover = Some(TaskPopover {
            task_id: "task-1".into(),
            scroll: 7,
        });

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        assert_eq!(app.popover.as_ref().unwrap().scroll, 0);
    }

    /// The board footer sources every nav/move/reorder/open/refresh/
    /// workflow glyph from the merged keymaps and renders it in the
    /// host platform's convention. Pinning the Linux spelling guards
    /// the action→slot mapping (a swapped `MoveCardLeft`/`Right` would
    /// surface here) and confirms `filter` stays literal (the
    /// workspace-filter dropdown is still ad-hoc).
    #[test]
    fn board_footer_renders_chords_from_keymaps() {
        use ratatui::{backend::TestBackend, Terminal};
        use shelbi_state::keymap::KeyChord;

        let mut app = KanbanApp::new("demo");
        app.display_style = DisplayStyle::Linux;
        for (action, chord) in [
            (KanbanAction::NavLeft, "h"),
            (KanbanAction::NavRight, "l"),
            (KanbanAction::NavDown, "j"),
            (KanbanAction::NavUp, "k"),
            (KanbanAction::OpenPopover, "enter"),
            (KanbanAction::MoveCardLeft, "H"),
            (KanbanAction::MoveCardRight, "L"),
            (KanbanAction::ReorderUp, "K"),
            (KanbanAction::ReorderDown, "J"),
            (KanbanAction::Refresh, "r"),
            (KanbanAction::CycleWorkflowFilter, "tab"),
        ] {
            app.keymaps
                .kanban
                .by_action
                .insert(action, vec![KeyChord::parse(chord).unwrap()]);
        }

        let backend = TestBackend::new(160, 14);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();
        let buf = term.backend().buffer().clone();
        let joined: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n");

        assert!(
            joined.contains(
                "h/l col   j/k row   Enter open   Shift+h/Shift+l move col   \
                 Shift+k/Shift+j reorder   Tab workflow   f filter   r refresh"
            ),
            "footer mismatch in:\n{joined}"
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
        assert_eq!(
            app.dropdown_option_at(10, 1),
            None,
            "miss outside chip x range"
        );
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
                    // Tests pass a single label; collapse it onto both
                    // id and name. Real workflows split these (kebab id
                    // vs. display name), but the All-mode rendering
                    // tests don't care about the split.
                    id: (*n).into(),
                    name: (*n).into(),
                    category: *c,
                    owner: shelbi_core::Owner::Agent,
                    agent: Some("orchestrator".into()),
                    tags: Vec::new(),
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

    /// Build a workflow that declares a subset of the project-wide
    /// canonical status ids. Each id must exist in
    /// [`default_project_statuses`]; the helper copies the canonical
    /// name/category for each. Used to keep these all-view tests
    /// hermetic — no on-disk `statuses.yaml` required.
    fn workflow_using(name: &str, status_ids: &[&str]) -> Workflow {
        let ps = default_project_statuses();
        Workflow {
            name: name.into(),
            description: None,
            statuses: status_ids
                .iter()
                .map(|id| {
                    let canonical = ps
                        .get(id)
                        .expect("test status id must exist in default catalogue");
                    shelbi_core::WorkflowStatus {
                        id: canonical.id.clone(),
                        name: canonical.name.clone(),
                        category: canonical.category,
                        owner: shelbi_core::Owner::Agent,
                        agent: Some("orchestrator".into()),
                        tags: Vec::new(),
                    }
                })
                .collect(),
            initial_status: None,
            transitions: None,
            git: None,
            zen: None,
        }
    }

    /// With the canonical `statuses.yaml` loaded, `all_columns` is
    /// exactly the six declared statuses in declared order — the five
    /// legacy lanes plus the `Canceled` terminal lane. Holds regardless
    /// of which workflows are loaded, since columns come from the
    /// project-wide catalogue.
    #[test]
    fn all_columns_match_statuses_yml_declaration_order() {
        let cols = kanban_columns_from(&default_project_statuses(), None);
        let names: Vec<&str> = cols.iter().map(|c| c.status_name.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "Backlog",
                "Todo",
                "In Progress",
                "Review",
                "Done",
                "Canceled"
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
        let t_backlog = task_in_workflow(
            "a",
            Column::backlog(),
            Some("design-review"),
            "2026-06-20T10:00:00Z",
        )
        .task;
        let t_wip = task_in_workflow(
            "b",
            Column::in_progress(),
            Some("design-review"),
            "2026-06-20T10:00:00Z",
        )
        .task;
        let t_review = task_in_workflow(
            "c",
            Column::review(),
            Some("design-review"),
            "2026-06-20T10:00:00Z",
        )
        .task;
        // Name match.
        assert_eq!(resolve_task_status(&t_backlog, &design), "Backlog");
        // Category fallback — `InProgress` is not declared by name.
        assert_eq!(resolve_task_status(&t_wip, &design), "Design");
        // Category fallback — `Review` is not declared by name.
        assert_eq!(resolve_task_status(&t_review, &design), "QA");
    }

    /// All-view tasks slot into the column matching their resolved
    /// status id, regardless of workflow. Two workflows that both
    /// declare `in-progress` share that column — the all-view's whole
    /// point. Tasks whose declared workflow isn't loaded fall back to
    /// the default workflow's resolution rules so they still appear.
    #[test]
    fn column_tasks_buckets_tasks_from_all_workflows_by_status_id() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        app.tasks = vec![
            // Default-workflow tasks.
            task_in_workflow("a", Column::todo(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow(
                "b",
                Column::review(),
                Some("default"),
                "2026-06-20T10:00:00Z",
            ),
            // design-review tasks land in shared columns.
            task_in_workflow(
                "c",
                Column::in_progress(),
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            task_in_workflow(
                "d",
                Column::done(),
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            // A default-workflow task in InProgress shares the column
            // with the design-review InProgress task.
            task_in_workflow("e", Column::in_progress(), None, "2026-06-20T10:00:00Z"),
            // Task pointing at a workflow that doesn't exist falls
            // back to default — Todo lands in the project-wide Todo column.
            task_in_workflow(
                "orphan",
                Column::todo(),
                Some("ghost"),
                "2026-06-20T10:00:00Z",
            ),
        ];

        // statuses.yaml order: backlog todo in-progress review done canceled
        let ids = |idx: usize| -> Vec<&str> {
            app.column_tasks(idx)
                .iter()
                .map(|t| t.task.id.as_str())
                .collect()
        };
        assert_eq!(ids(0), Vec::<&str>::new(), "backlog empty");
        assert_eq!(ids(1), vec!["a", "orphan"], "todo");
        assert_eq!(
            ids(2),
            vec!["c", "e"],
            "in-progress shares cards across workflows"
        );
        assert_eq!(ids(3), vec!["b"], "review");
        assert_eq!(ids(4), vec!["d"], "done");
        assert_eq!(
            ids(5),
            Vec::<&str>::new(),
            "canceled stays visible but empty"
        );
    }

    /// Every status declared in `statuses.yaml` gets a column even if
    /// no current task occupies it. The acceptance criterion behind
    /// "the layout doesn't shift when the last task leaves a column."
    #[test]
    fn unused_statuses_render_as_empty_columns() {
        let mut app = KanbanApp::new("demo");
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        app.tasks = vec![task_in_workflow(
            "only",
            Column::todo(),
            None,
            "2026-06-20T10:00:00Z",
        )];
        // Six columns from statuses.yaml — five are empty.
        assert_eq!(app.all_columns.len(), 6);
        let counts: Vec<usize> = (0..app.all_columns.len())
            .map(|i| app.column_tasks(i).len())
            .collect();
        assert_eq!(counts, vec![0, 1, 0, 0, 0, 0]);
    }

    /// `move_card_*` stays inside the task's owning workflow — a card
    /// in workflow `design-review` (which declares only backlog,
    /// in-progress, done) skips the columns its workflow doesn't have.
    /// `nav_right`, in contrast, walks every visible column.
    #[test]
    fn nav_right_walks_every_column_but_move_card_skips_workflow_gaps() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();

        // nav_right walks all six columns and wraps at the end.
        app.selected_column = 5; // canceled
        app.nav_right();
        assert_eq!(
            app.selected_column, 0,
            "wraps from canceled back to backlog"
        );
        for expected in 1..=5 {
            app.nav_right();
            assert_eq!(app.selected_column, expected);
        }

        // Move-card respects the task's workflow. design-review declares
        // only backlog (0), in-progress (2), done (4) — so a card sitting
        // in `backlog` moves right to `in-progress` (skipping `todo`).
        let design_review = app
            .workflows
            .iter()
            .find(|w| w.name == "design-review")
            .unwrap();
        assert_eq!(
            app.adjacent_column_in_workflow("backlog", design_review, true, true),
            Some(2),
            "design-review skips todo on right"
        );
        assert_eq!(
            app.adjacent_column_in_workflow("done", design_review, true, true),
            Some(0),
            "wraps back to the workflow's first declared status"
        );
        assert_eq!(
            app.adjacent_column_in_workflow("backlog", design_review, false, true),
            Some(4),
            "wrap left from first → workflow's last status"
        );

        // The default workflow's adjacency walks every column (no gaps).
        let default = app.workflows.iter().find(|w| w.name == "default").unwrap();
        assert_eq!(
            app.adjacent_column_in_workflow("backlog", default, true, true),
            Some(1),
            "default → next column without skipping"
        );

        // Clamped adjacency (the popover's mode): interior moves match
        // the wrapping ones, but the workflow's first/last eligible
        // column has no adjacent in that direction.
        assert_eq!(
            app.adjacent_column_in_workflow("backlog", design_review, true, false),
            Some(2),
            "clamped interior move matches the wrapping one"
        );
        assert_eq!(
            app.adjacent_column_in_workflow("done", design_review, true, false),
            None,
            "clamped at the workflow's last eligible column"
        );
        assert_eq!(
            app.adjacent_column_in_workflow("backlog", design_review, false, false),
            None,
            "clamped at the workflow's first eligible column"
        );
    }

    /// A workflow with a single status has nowhere to move-card to,
    /// so the move-card helpers return `None` and the caller noops.
    #[test]
    fn adjacent_column_returns_none_for_singleton_workflow() {
        let solo = workflow_using("solo", &["in-progress"]);
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![solo.clone()];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        assert_eq!(
            app.adjacent_column_in_workflow("in-progress", &solo, true, true),
            None
        );
        assert_eq!(
            app.adjacent_column_in_workflow("in-progress", &solo, false, true),
            None
        );
    }

    /// Two workflows render side-by-side over the shared, project-wide
    /// column set from `statuses.yaml`. Headers show the canonical
    /// status names; per-card workflow names surface so the user can
    /// tell which workflow each card belongs to. Columns themselves no
    /// longer carry a workflow subscript — they're project-wide.
    #[test]
    fn rendering_two_workflows_uses_shared_column_set_with_card_labels() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        // Seed every non-Canceled column with at least one task so the
        // collapse-empty logic doesn't truncate their headers below the
        // length the assertions look for.
        app.tasks = vec![
            task_in_workflow("d-backlog", Column::backlog(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-todo", Column::todo(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-wip", Column::in_progress(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-review", Column::review(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow("d-done", Column::done(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow(
                "dr-wip",
                Column::in_progress(),
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
            task_in_workflow(
                "dr-done",
                Column::done(),
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
        ];

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

        // Canonical headers paint from statuses.yaml. Canceled is empty
        // (no task) so it collapses to EMPTY_MIN_W — don't assert on it.
        assert!(rendered.contains("BACKLOG"), "BACKLOG missing:\n{rendered}");
        assert!(rendered.contains("TO DO"), "TO DO missing:\n{rendered}");
        assert!(rendered.contains("IN PROGRESS"), "IN PROGRESS missing");
        assert!(rendered.contains("REVIEW"), "REVIEW header missing");
        assert!(rendered.contains("DONE"), "DONE header missing");
        // Per-card workflow names appear on each card so the user can
        // tell which workflow a shared-column card came from.
        assert!(
            rendered.contains("default"),
            "default workflow card label missing:\n{rendered}"
        );
        assert!(
            rendered.contains("design-review") || rendered.contains("design-revi"),
            "design-review card label missing:\n{rendered}"
        );
    }

    /// A full blank row separates consecutive cards in a column: the
    /// second card's hit rect starts one row below the first card's
    /// bottom (not flush against it), there's no leading gap above the
    /// first card, and a click landing on the gap row misses every card.
    #[test]
    fn cards_render_with_one_blank_row_between_them() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow()];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        // Two cards in the same (backlog) column — the gap only appears
        // between cards, so we need at least two.
        app.tasks = vec![
            task_in_workflow("t-one", Column::backlog(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow("t-two", Column::backlog(), None, "2026-06-20T10:00:00Z"),
        ];
        app.selected_column = 0;

        let backend = TestBackend::new(160, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let mut col0: Vec<&CardHit> = app
            .card_hits
            .iter()
            .filter(|h| h.col_idx == 0)
            .collect();
        col0.sort_by_key(|h| h.row_idx);
        assert_eq!(col0.len(), 2, "both backlog cards recorded a hit rect");
        let (first, second) = (col0[0], col0[1]);

        // One full blank row between the cards: the second card starts a
        // single row below the first card's bottom edge.
        assert_eq!(
            second.area.y,
            first.area.y + first.area.height + 1,
            "expected exactly one blank row between cards"
        );

        // The gap row belongs to no card.
        let gap_y = first.area.y + first.area.height;
        let x = first.area.x + 1;
        assert_eq!(
            app.card_at(x, gap_y),
            None,
            "click on the inter-card gap misses every card"
        );
        // Cards on either side of the gap still map to the right rows.
        assert_eq!(app.card_at(x, first.area.y), Some((0, 0)));
        assert_eq!(app.card_at(x, second.area.y), Some((0, 1)));
    }

    // ---- workflow filter -------------------------------------------------

    /// With `workflow_filter` set, `compute_all_columns` narrows the
    /// column set to just the statuses that workflow declares — still
    /// in `statuses.yaml` declared order (workflows cannot reorder).
    /// Clearing the filter restores the full project-wide column set.
    #[test]
    fn workflow_filter_narrows_columns_to_workflow_statuses() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        assert_eq!(app.all_columns.len(), 6, "all six project statuses");

        app.workflow_filter = Some("design-review".into());
        app.all_columns = app.compute_all_columns();
        let ids: Vec<&str> = app
            .all_columns
            .iter()
            .map(|c| c.status_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["backlog", "in-progress", "done"],
            "narrowed to the workflow's statuses, in statuses.yaml order"
        );

        app.workflow_filter = None;
        app.all_columns = app.compute_all_columns();
        assert_eq!(app.all_columns.len(), 6, "clear restores full set");
    }

    /// A stale workflow filter (workflow no longer loaded) falls back
    /// to the full statuses.yaml column set so the board still paints.
    /// The filter itself isn't auto-cleared — the user can see the
    /// chip and dismiss it.
    #[test]
    fn workflow_filter_for_missing_workflow_falls_back_to_full_set() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow()];
        app.project_statuses = default_project_statuses();
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
            task_in_workflow("a", Column::todo(), None, "2026-06-20T10:00:00Z"),
            task_in_workflow(
                "b",
                Column::backlog(),
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
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
    /// the workspace dropdown is — the same key opens and closes it.
    #[test]
    fn toggle_workflow_dropdown_is_self_inverse() {
        let mut app = KanbanApp::new("demo");
        assert!(!app.workflow_dropdown_is_open());
        app.toggle_workflow_dropdown();
        assert!(app.workflow_dropdown_is_open());
        app.toggle_workflow_dropdown();
        assert!(!app.workflow_dropdown_is_open());
    }

    /// Default filter state on a fresh app is `None` — the "All
    /// workflows" view. Acceptance criterion: cycling starts from the
    /// unfiltered state.
    #[test]
    fn workflow_filter_defaults_to_all() {
        let app = KanbanApp::new("demo");
        assert!(app.workflow_filter.is_none(), "fresh app starts at All");
    }

    /// `cycle_workflow_filter` walks `None → workflows[0] →
    /// workflows[1] → ... → None`, in the order workflows are loaded.
    /// Wrapping past the last workflow returns to `None` so the user
    /// always reaches the unfiltered view exactly once per cycle.
    #[test]
    fn cycle_workflow_filter_walks_all_then_each_workflow_then_wraps() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.all_columns = app.compute_all_columns();
        assert!(app.workflow_filter.is_none(), "starts at All");

        app.cycle_workflow_filter();
        assert_eq!(app.workflow_filter.as_deref(), Some("default"));

        app.cycle_workflow_filter();
        assert_eq!(app.workflow_filter.as_deref(), Some("design-review"));

        app.cycle_workflow_filter();
        assert!(app.workflow_filter.is_none(), "wraps to All");

        // And cycles again from All — proves the wrap is stable.
        app.cycle_workflow_filter();
        assert_eq!(app.workflow_filter.as_deref(), Some("default"));
    }

    /// `cycle_workflow_filter` from a filter that targets a workflow
    /// no longer loaded jumps back to `None` rather than getting
    /// stuck — the user can always cycle out of a stale chip.
    #[test]
    fn cycle_workflow_filter_from_stale_filter_returns_to_all() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![default_workflow()];
        app.workflow_filter = Some("removed".into());
        app.cycle_workflow_filter();
        assert!(app.workflow_filter.is_none());
    }

    /// `cycle_workflow_filter` with no workflows loaded (the loader
    /// surfaced an error and `self.workflows` is empty) is a no-op so
    /// the keybinding can't panic.
    #[test]
    fn cycle_workflow_filter_no_workflows_loaded_is_noop() {
        let mut app = KanbanApp::new("demo");
        app.workflows = Vec::new();
        app.workflow_filter = None;
        app.cycle_workflow_filter();
        assert!(app.workflow_filter.is_none());
    }

    /// With a workflow filter active, `column_tasks` hides cards whose
    /// `workflow:` frontmatter doesn't match — even when their resolved
    /// status id matches a visible column. Two workflows that both
    /// declare `backlog` would otherwise share that column; the filter
    /// is what narrows the board to one workflow's cards.
    #[test]
    fn column_tasks_hides_other_workflow_cards_when_filter_set() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            // design-review shares `backlog` with the default workflow.
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.workflow_filter = Some("design-review".into());
        app.all_columns = app.compute_all_columns();
        app.tasks = vec![
            // Default-workflow card in Backlog — design-review also has
            // a `backlog` column, so without the workflow check this
            // card would leak in.
            task_in_workflow(
                "default-card",
                Column::backlog(),
                None,
                "2026-06-20T10:00:00Z",
            ),
            // design-review card in Backlog — should be the only card
            // visible in the backlog column.
            task_in_workflow(
                "dr-card",
                Column::backlog(),
                Some("design-review"),
                "2026-06-20T10:00:00Z",
            ),
        ];

        let backlog_idx = app
            .all_columns
            .iter()
            .position(|c| c.status_id == "backlog")
            .expect("design-review declares backlog");
        let ids: Vec<&str> = app
            .column_tasks(backlog_idx)
            .iter()
            .map(|tf| tf.task.id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["dr-card"],
            "default-workflow card must be hidden by the design-review filter"
        );
    }

    /// Empty columns still render under a single-workflow filter — the
    /// Phase 2 stable-layout decision. If design-review declares
    /// `backlog, in-progress, done` and only `in-progress` has cards,
    /// the other two columns still appear as empty headers.
    #[test]
    fn empty_columns_render_under_workflow_filter() {
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.workflow_filter = Some("design-review".into());
        app.all_columns = app.compute_all_columns();
        app.tasks = vec![task_in_workflow(
            "dr",
            Column::in_progress(),
            Some("design-review"),
            "2026-06-20T10:00:00Z",
        )];

        let ids: Vec<&str> = app
            .all_columns
            .iter()
            .map(|c| c.status_id.as_str())
            .collect();
        assert_eq!(
            ids,
            vec!["backlog", "in-progress", "done"],
            "all three of the filtered workflow's statuses paint"
        );
        let counts: Vec<usize> = (0..app.all_columns.len())
            .map(|i| app.column_tasks(i).len())
            .collect();
        assert_eq!(
            counts,
            vec![0, 1, 0],
            "empty columns persist alongside the populated one"
        );
    }

    /// Filter state is in-memory only — `apply_workflow_filter` must
    /// not touch `state.json`. Acceptance criterion: filter resets on
    /// `shelbi reload` (the orchestrator-side relaunch path), so it
    /// can't be written to the cross-session state file.
    #[test]
    fn apply_workflow_filter_does_not_write_state_json() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-kanban-workflow-filter-noop-persist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("SHELBI_HOME", &tmp);
        // Project dir must exist for `read_state` not to error, but
        // the state.json itself shouldn't be created by setting the
        // filter — that's the contract under test.
        let proj_dir = tmp.join("projects").join("demo");
        std::fs::create_dir_all(&proj_dir).unwrap();

        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.cycle_workflow_filter();
        assert_eq!(app.workflow_filter.as_deref(), Some("default"));

        let state_file = proj_dir.join("state.json");
        assert!(
            !state_file.exists(),
            "workflow filter must NOT be persisted; state.json should not be created"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    // ---- collapse-empty + horizontal scroll ------------------------------

    /// `compute_column_widths` collapses empty columns to a fixed
    /// minimum and gives the slack to the non-empty ones. With a
    /// single non-empty column out of six, it should grow well past
    /// the others.
    #[test]
    fn compute_column_widths_collapses_empty_and_grows_non_empty() {
        let cols = kanban_columns_from(&default_project_statuses(), None);
        // 5 collapsed columns, 1 expanded.
        let collapsed = vec![true, true, false, true, true, true];
        let widths = compute_column_widths(&cols, &collapsed, 0, 100);
        assert_eq!(widths.len(), 6);
        for (i, w) in &widths {
            if collapsed[*i] {
                assert_eq!(
                    *w, COLLAPSED_MIN_W,
                    "collapsed column {i} should stay at min"
                );
            } else {
                assert!(
                    *w > NONEMPTY_MIN_W,
                    "expanded column {i} should grow beyond min, got {w}"
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
    /// window forward. Synthesizes a 10-status `statuses.yaml` so the
    /// minimum total (10 × 14 = 140) overflows the 80-cell area.
    #[test]
    fn compute_column_widths_drops_overflow_and_scrolls() {
        let ps = shelbi_core::ProjectStatuses {
            statuses: (0..10)
                .map(|i| shelbi_core::ProjectStatus {
                    id: format!("s{i}"),
                    name: format!("Status{i}"),
                    category: StatusCategory::Active,
                })
                .collect(),
        };
        let cols = kanban_columns_from(&ps, None);
        let collapsed = vec![false; cols.len()];

        let widths = compute_column_widths(&cols, &collapsed, 0, 80);
        assert!(widths.len() < cols.len(), "should drop trailing cols");
        assert_eq!(widths[0].0, 0, "starts at scroll=0");

        // Scrolling reveals later columns at the cost of earlier ones.
        let widths = compute_column_widths(&cols, &collapsed, 4, 80);
        assert_eq!(widths[0].0, 4, "starts at scroll=4");
        assert!(
            widths.iter().map(|(_, w)| *w).sum::<u16>() <= 80,
            "respects area width"
        );
    }

    /// Full render with a workflow filter applied: only the statuses
    /// the filtered workflow declares paint (in `statuses.yaml` order),
    /// and the workflow chip shows the active filter. The workspace
    /// chip stays in place beside it.
    #[test]
    fn rendering_workflow_filter_narrows_visible_columns() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        app.workflows = vec![
            default_workflow(),
            workflow_using("design-review", &["backlog", "in-progress", "done"]),
        ];
        app.project_statuses = default_project_statuses();
        app.workflow_filter = Some("design-review".into());
        app.all_columns = app.compute_all_columns();
        app.tasks = vec![
            // A default-workflow card in Todo — filtered out because
            // design-review doesn't declare `todo`.
            task_in_workflow("d1", Column::todo(), None, "2026-06-20T10:00:00Z"),
            // A design-review card in InProgress — should paint.
            task_in_workflow(
                "dr1",
                Column::in_progress(),
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

        // Filter chip shows the active filter; only design-review's
        // statuses render. The unselected `TO DO` and `REVIEW` columns
        // must not appear — that's the proof the filter narrowed.
        assert!(
            rendered.contains("Workflow: design-review ▾"),
            "workflow chip missing or wrong:\n{rendered}"
        );
        assert!(
            rendered.contains("IN PROGRESS"),
            "IN PROGRESS column missing"
        );
        // TO DO is a project-wide column with the legacy uppercase label —
        // never paints under the design-review filter (design-review
        // doesn't declare `todo`).
        assert!(
            !rendered.contains("TO DO"),
            "TO DO leaked into filtered view:\n{rendered}"
        );
        assert!(
            !rendered.contains("REVIEW"),
            "REVIEW leaked into filtered view:\n{rendered}"
        );
        // dr1 should render in design-review's `in-progress` column.
        assert!(rendered.contains("dr1"), "filtered card missing");
    }

    /// `ensure_selected_visible` advances `column_scroll` when the
    /// selection falls past the visible window, and pulls it back to
    /// the selection when scrolled too far right. Uses a synthesized
    /// 10-status catalogue and forces every column into the explicit
    /// `Expanded` override so the minimum width (14 × 10 = 140)
    /// overflows the 60-cell area we measure against — without the
    /// overrides each empty column would auto-collapse to its
    /// 3-cell strip and the whole row would fit.
    #[test]
    fn ensure_selected_visible_keeps_selection_in_view() {
        let ps = shelbi_core::ProjectStatuses {
            statuses: (0..10)
                .map(|i| shelbi_core::ProjectStatus {
                    id: format!("s{i}"),
                    name: format!("Status{i}"),
                    category: StatusCategory::Active,
                })
                .collect(),
        };
        let mut app = KanbanApp::new("demo");
        app.project_statuses = ps;
        app.all_columns = kanban_columns_from(&app.project_statuses, None);
        // Force every synthesized column into the explicit-expanded
        // state. Using `kanban_column_override_key` keeps this in
        // lock-step with the production override path.
        app.column_overrides = (0..app.all_columns.len())
            .map(|i| {
                (
                    shelbi_state::kanban_column_override_key("default", &format!("s{i}")),
                    KanbanColumnOverride::Expanded,
                )
            })
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
            column: Column::todo(),
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
                .map(|(k, v)| ((*k).to_string(), (*v).into()))
                .collect(),
        }
    }

    fn workflow_with_git(name: &str, base_branch: Option<&str>) -> Workflow {
        Workflow {
            name: name.into(),
            description: None,
            statuses: vec![shelbi_core::WorkflowStatus {
                id: "todo".into(),
                name: "Todo".into(),
                category: StatusCategory::Ready,
                owner: shelbi_core::Owner::Agent,
                agent: Some("orchestrator".into()),
                tags: Vec::new(),
            }],
            initial_status: None,
            transitions: None,
            git: base_branch.map(|b| shelbi_core::GitConfig {
                base_branch: Some(b.into()),
                merge_strategy: shelbi_core::MergeStrategy::Squash,
                ..Default::default()
            }),
            zen: None,
        }
    }

    /// Single-workflow project, default workflow, no branch → the meta
    /// line stays blank (badge hidden because the header already implies
    /// the sole workflow, no branch to surface). Avoids forcing a
    /// redundant "default" badge onto every card in projects that haven't
    /// adopted workflows.
    #[test]
    fn card_meta_line_blank_for_default_only() {
        let task = task_with_params("t", None, &[]);
        let workflows = vec![default_workflow()];
        let line = card_meta_line(&task, &workflows, "default", false, 40);
        assert_eq!(line_text(&line), "");
    }

    /// Multi-workflow board, default-workflow task → the workflow badge
    /// surfaces (space-padded) so the user can distinguish "default"
    /// cards from cards belonging to other workflows on the same screen.
    #[test]
    fn card_meta_line_shows_default_badge_when_multiple_workflows() {
        let task = task_with_params("t", None, &[]);
        let workflows = vec![default_workflow(), workflow_with_git("feature-task", None)];
        let line = card_meta_line(&task, &workflows, "default", true, 40);
        assert_eq!(line_text(&line), " default ");
    }

    /// Custom workflow with no `git:` block and no task branch → just the
    /// badge. Nothing to resolve.
    #[test]
    fn card_meta_line_shows_workflow_badge_without_git_block() {
        let task = task_with_params("t", Some("design-review"), &[]);
        let workflows = vec![default_workflow(), workflow_with_git("design-review", None)];
        let line = card_meta_line(&task, &workflows, "design-review", true, 40);
        assert_eq!(line_text(&line), " design-review ");
    }

    /// Resolved placeholder: `base_branch: feature/{{feature}}` + the
    /// task carrying `feature: auth-rewrite` renders as the badge plus
    /// `⎇ feature/auth-rewrite`. Headline case — workflow badge +
    /// resolved branch on one line.
    #[test]
    fn card_meta_line_appends_resolved_branch() {
        let task = task_with_params("t", Some("feature-task"), &[("feature", "auth-rewrite")]);
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = card_meta_line(&task, &workflows, "feature-task", true, 60);
        // Two spaces between badge and branch: the badge's own trailing
        // padding (bg-filled) plus a plain separator space.
        assert_eq!(line_text(&line), " feature-task  ⎇ feature/auth-rewrite");
    }

    /// When the column header already implies the workflow (single
    /// workflow visible, so `show_workflow_name=false`), the meta line
    /// drops the badge but keeps `⎇ branch` — the only per-task signal
    /// worth carrying.
    #[test]
    fn card_meta_line_omits_badge_but_keeps_branch_when_filtered() {
        let task = task_with_params("t", Some("feature-task"), &[("feature", "dashboard-v2")]);
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = card_meta_line(&task, &workflows, "feature-task", false, 60);
        assert_eq!(line_text(&line), "⎇ feature/dashboard-v2");
    }

    /// The task's own `branch` (once the orchestrator has cut it) wins
    /// over the workflow's base-branch template — it's the working branch
    /// the card is actually operating on.
    #[test]
    fn card_meta_line_prefers_task_branch_over_template() {
        let mut task = task_with_params("t", Some("feature-task"), &[("feature", "auth")]);
        task.branch = Some("shelbi/t".into());
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = card_meta_line(&task, &workflows, "feature-task", false, 60);
        assert_eq!(line_text(&line), "⎇ shelbi/t");
    }

    /// A `base_branch` without any placeholder (e.g. `main`) carries no
    /// per-task info and the task has no branch of its own, so the branch
    /// is suppressed. Falls through to the badge (when shown) or blank.
    #[test]
    fn card_meta_line_skips_non_templated_branch() {
        let task = task_with_params("t", Some("feature-release"), &[]);
        let workflows = vec![workflow_with_git("feature-release", Some("main"))];
        // Show badge path: drop the branch, keep the badge.
        let with_name = card_meta_line(&task, &workflows, "feature-release", true, 60);
        assert_eq!(line_text(&with_name), " feature-release ");
        // No badge path: nothing left worth showing → blank.
        let without_name = card_meta_line(&task, &workflows, "feature-release", false, 60);
        assert_eq!(line_text(&without_name), "");
    }

    /// Missing param keeps the placeholder unresolved. Rather than
    /// surface a half-substituted `feature/{{feature}}` (which would
    /// mislead at a glance), the meta line drops the branch and shows
    /// just the badge (or blank when the column header already names the
    /// workflow). The popover stays responsible for surfacing the
    /// actionable error.
    #[test]
    fn card_meta_line_hides_branch_when_param_missing() {
        let task = task_with_params("t", Some("feature-task"), &[]);
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let with_name = card_meta_line(&task, &workflows, "feature-task", true, 60);
        assert_eq!(line_text(&with_name), " feature-task ");
        let without_name = card_meta_line(&task, &workflows, "feature-task", false, 60);
        assert_eq!(line_text(&without_name), "");
    }

    /// `max_text` clips the meta line with an ellipsis, matching how the
    /// title is truncated so narrow columns don't push spans past the
    /// card's right gutter. The badge keeps its full width; the branch
    /// takes whatever remains.
    #[test]
    fn card_meta_line_truncates_to_max_text() {
        let task = task_with_params(
            "t",
            Some("feature-task"),
            &[(
                "feature",
                "very-long-feature-name-that-exceeds-the-cell-width",
            )],
        );
        let workflows = vec![workflow_with_git(
            "feature-task",
            Some("feature/{{feature}}"),
        )];
        let line = card_meta_line(&task, &workflows, "feature-task", true, 20);
        let text = line_text(&line);
        assert!(text.chars().count() <= 20, "overflows width: {text:?}");
        assert!(text.ends_with('…'), "should end with ellipsis: {text:?}");
        assert!(
            text.starts_with(" feature-task "),
            "kept the leading workflow badge: {text:?}"
        );
    }

    /// `wrap_title_two_lines` keeps a short title on one line (2-row
    /// card), word-wraps a long title at the last space that fits, and
    /// truncates an over-long second line with `…`. A first token wider
    /// than the column hard-wraps mid-word.
    #[test]
    fn wrap_title_two_lines_behaviour() {
        // Fits → single line.
        assert_eq!(
            wrap_title_two_lines("short title", 20),
            ("short title".to_string(), None)
        );
        // Word-wrap at the last fitting space.
        assert_eq!(
            wrap_title_two_lines("one two three four five", 12),
            ("one two".to_string(), Some("three four …".to_string()))
        );
        // First token longer than the width → hard wrap.
        assert_eq!(
            wrap_title_two_lines("supercalifragilistic done", 10),
            ("supercalif".to_string(), Some("ragilisti…".to_string()))
        );
    }

    // ---- column collapse / expand ----------------------------------------

    /// `ColumnExpansion::Auto` defers to task count: empty collapses,
    /// non-empty expands. Explicit states win regardless.
    #[test]
    fn column_expansion_auto_resolves_against_count() {
        assert!(ColumnExpansion::Auto.is_collapsed(0));
        assert!(!ColumnExpansion::Auto.is_collapsed(1));
        assert!(ColumnExpansion::Collapsed.is_collapsed(0));
        assert!(ColumnExpansion::Collapsed.is_collapsed(99));
        assert!(!ColumnExpansion::Expanded.is_collapsed(0));
        assert!(!ColumnExpansion::Expanded.is_collapsed(99));
    }

    /// A fresh KanbanApp has no overrides → every column resolves to
    /// `Auto`. Empty columns are collapsed-by-rendering; non-empty
    /// expand.
    #[test]
    fn fresh_app_columns_are_auto_and_collapse_on_empty() {
        let mut app = KanbanApp::new("demo");
        // Backlog (idx 0) is empty; Todo (idx 1) has one card.
        app.tasks = vec![task_file("a", Column::todo(), 0, "2026-06-20T10:00:00Z")];
        assert_eq!(app.column_expansion(0), ColumnExpansion::Auto);
        assert!(app.is_column_collapsed(0), "empty Backlog auto-collapses");
        assert!(!app.is_column_collapsed(1), "non-empty Todo auto-expands");
    }

    /// Clicking an empty (auto-collapsed) column promotes it to
    /// `Explicit Expanded`; clicking again promotes it to `Explicit
    /// Collapsed`. The state never silently slides back to `Auto` —
    /// each click alternates between the two explicit states.
    #[test]
    fn toggle_column_alternates_explicit_states() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-toggle-column-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let mut app = KanbanApp::new("demo");
        // Backlog (idx 0) starts empty → Auto → collapsed.
        assert!(app.is_column_collapsed(0));
        // Click → explicit expanded.
        app.toggle_column(0);
        assert_eq!(app.column_expansion(0), ColumnExpansion::Expanded);
        assert!(!app.is_column_collapsed(0));
        // Click again → explicit collapsed.
        app.toggle_column(0);
        assert_eq!(app.column_expansion(0), ColumnExpansion::Collapsed);
        assert!(app.is_column_collapsed(0));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An explicit-collapsed override on a non-empty column survives
    /// a fresh KanbanApp (i.e. a shelbi restart): writing the override
    /// through `toggle_column` then rebuilding the app and calling
    /// `refresh` rehydrates the same state.
    #[test]
    fn explicit_overrides_persist_across_app_restart() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-column-override-persist-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // First app: collapse a non-empty column explicitly.
        let mut app = KanbanApp::new("demo");
        app.tasks = vec![task_file("a", Column::todo(), 0, "2026-06-20T10:00:00Z")];
        assert!(
            !app.is_column_collapsed(1),
            "non-empty Todo expands by default"
        );
        app.toggle_column(1);
        assert_eq!(app.column_expansion(1), ColumnExpansion::Collapsed);

        // Second app: refresh hydrates the override from disk.
        let mut app2 = KanbanApp::new("demo");
        app2.refresh();
        assert_eq!(
            app2.column_expansion(1),
            ColumnExpansion::Collapsed,
            "explicit collapse must survive a fresh app",
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// All-view column overrides are scoped to the project's configured
    /// default workflow, not the literal legacy `default` workflow. When
    /// those keys diverge, a refresh must reload the just-written override
    /// instead of falling back to Auto.
    #[test]
    fn all_view_toggle_uses_project_default_workflow_key() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-default-workflow-column-override-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let project = shelbi_core::Project {
            name: "demo".into(),
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: Some("app".into()),
            config_mode: None,
            git: shelbi_core::GitConfig::default(),
            machines: vec![shelbi_core::Machine {
                name: "hub".into(),
                kind: shelbi_core::MachineKind::Local,
                work_dir: "/tmp/demo".into(),
                host: None,
                tags: Vec::new(),
                forward: None,
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
                        prompt_injection: None,
                        dialog_signatures: vec![],
                    },
                );
                m
            },
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
        };
        shelbi_state::save_project(&project).unwrap();
        shelbi_state::scaffold_project_statuses("demo").unwrap();
        let workflows_dir = shelbi_state::project_dir("demo").unwrap().join("workflows");
        std::fs::create_dir_all(&workflows_dir).unwrap();
        let app_workflow = shelbi_core::scaffold::default_workflow_yaml()
            .unwrap()
            .replacen("name: default", "name: app", 1);
        std::fs::write(workflows_dir.join("app.yaml"), app_workflow).unwrap();
        let loaded = shelbi_state::load_project("demo").unwrap();
        assert_eq!(loaded.default_workflow_name(), "app");

        let mut app = KanbanApp::new("demo");
        app.refresh();
        assert_eq!(app.default_workflow_name, "app");
        assert!(app.workflow_filter.is_none(), "precondition: All view");
        assert!(app.is_column_collapsed(0), "empty Backlog auto-collapses");

        app.toggle_column(0);
        assert_eq!(app.column_expansion(0), ColumnExpansion::Expanded);

        app.refresh();
        assert_eq!(app.default_workflow_name, "app");
        assert_eq!(
            app.column_expansion(0),
            ColumnExpansion::Expanded,
            "All-view override should reload from app:<status>, not default:<status>",
        );
        assert!(!app.is_column_collapsed(0));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// When a task lands in an explicitly-collapsed column the override
    /// stays — only the `(N)` count surfaces the change. The
    /// acceptance criterion: a non-empty + explicitly-collapsed column
    /// must not auto-expand just because a card showed up.
    #[test]
    fn explicit_collapse_persists_when_tasks_arrive() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-tui-explicit-collapse-arrival-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let mut app = KanbanApp::new("demo");
        // Empty board → Backlog auto-collapses. Promote to explicit
        // collapsed (one toggle goes auto→expanded, second goes to
        // explicit collapsed).
        app.toggle_column(0);
        app.toggle_column(0);
        assert_eq!(app.column_expansion(0), ColumnExpansion::Collapsed);

        // Task lands in the column — override stands, count updates.
        app.tasks = vec![task_file("a", Column::backlog(), 0, "2026-06-20T10:00:00Z")];
        assert!(
            app.is_column_collapsed(0),
            "explicit-collapsed must persist after a task arrives",
        );
        assert_eq!(app.column_tasks(0).len(), 1, "count surfaces the new task");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Full-frame render confirms a collapsed column paints its label
    /// one character per row, preserves the space in `IN PROGRESS` as
    /// a blank row, and writes the `(N)` count below the strip. The
    /// label's first glyph must land on the column's top row so it
    /// lines up with the header row of expanded neighbours.
    #[test]
    fn rendering_empty_column_uses_rotated_label_with_count() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        // Seed a single Backlog card so the Backlog column expands —
        // we want a NON-Backlog empty column to verify the strip.
        app.tasks = vec![task_file(
            "seed",
            Column::backlog(),
            0,
            "2026-06-20T10:00:00Z",
        )];

        let backend = TestBackend::new(120, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        let buf = term.backend().buffer().clone();
        // Find the column slot for "in-progress" — it's empty so
        // auto-collapses. Walk down the centre of its slot and confirm
        // the letters of "IN PROGRESS" paint top-down (with a blank
        // row for the space).
        let in_progress_idx = app
            .all_columns
            .iter()
            .position(|c| c.status_id == "in-progress")
            .expect("default catalogue declares in-progress");
        let header_hit = app
            .header_hits
            .iter()
            .find(|h| h.col_idx == in_progress_idx)
            .expect("collapsed in-progress column registered a header hit");
        let col_area = header_hit.area;
        let glyph_x = col_area.x + col_area.width / 2;

        let glyphs: Vec<String> = (col_area.y..col_area.y + col_area.height)
            .map(|y| buf[(glyph_x, y)].symbol().to_string())
            .collect();
        let joined: String = glyphs.join("");
        // Label is anchored to the top — the first row must hold the
        // leading glyph of the label, not a blank.
        assert_eq!(
            glyphs[0], "I",
            "collapsed label must start on the column's top row, got {glyphs:?}"
        );
        // The count `(N)` paints horizontally, so only the leading
        // `(` falls on the centre column. Expect the rotated label
        // followed by the count line's leading paren.
        let trimmed = joined.trim_end_matches(|c: char| c.is_whitespace());
        assert!(
            trimmed.starts_with("IN PROGRESS"),
            "expected leading rotated label, got centre column: {trimmed:?}"
        );
    }

    /// Collapsed and expanded columns must share the same header row:
    /// the rotated label's first glyph and the expanded column's
    /// header text both paint at `area.y` of the columns band, so the
    /// board reads as a single aligned row of headers.
    #[test]
    fn collapsed_and_expanded_columns_share_header_row() {
        use ratatui::{backend::TestBackend, Terminal};
        let mut app = KanbanApp::new("demo");
        // One Backlog task → Backlog expands. Every other status is
        // empty → auto-collapses to a strip.
        app.tasks = vec![task_file(
            "seed",
            Column::backlog(),
            0,
            "2026-06-20T10:00:00Z",
        )];

        let backend = TestBackend::new(120, 20);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| render_full(f, &mut app, f.area())).unwrap();

        // Every header hit's first row should agree — collapsed strips
        // and expanded banners both anchor at the top of their slot.
        let header_top_rows: Vec<u16> = app.header_hits.iter().map(|h| h.area.y).collect();
        assert!(
            header_top_rows.windows(2).all(|w| w[0] == w[1]),
            "header hit rects sit on different top rows: {header_top_rows:?}"
        );
        let header_y = header_top_rows[0];

        let buf = term.backend().buffer().clone();
        // Backlog column (expanded) — its first non-blank glyph on the
        // header row should be the `B` in `BACKLOG`.
        let backlog_idx = app
            .all_columns
            .iter()
            .position(|c| c.status_id == "backlog")
            .expect("default catalogue declares backlog");
        let backlog_hit = app
            .header_hits
            .iter()
            .find(|h| h.col_idx == backlog_idx)
            .expect("backlog header registered a hit");
        let backlog_row: String = (backlog_hit.area.x..backlog_hit.area.x + backlog_hit.area.width)
            .map(|x| buf[(x, header_y)].symbol().to_string())
            .collect();
        assert!(
            backlog_row.trim_start().starts_with("BACKLOG"),
            "expanded header row should hold 'BACKLOG': {backlog_row:?}"
        );

        // A collapsed column — pick `in-progress`, whose rotated label
        // starts with `I`. The first row of its strip must hold that
        // glyph (no leading blank from centering).
        let collapsed_idx = app
            .all_columns
            .iter()
            .position(|c| c.status_id == "in-progress")
            .expect("default catalogue declares in-progress");
        let collapsed_hit = app
            .header_hits
            .iter()
            .find(|h| h.col_idx == collapsed_idx)
            .expect("in-progress header registered a hit");
        let glyph_x = collapsed_hit.area.x + collapsed_hit.area.width / 2;
        assert_eq!(
            buf[(glyph_x, header_y)].symbol(),
            "I",
            "collapsed label's first glyph must sit on the shared header row",
        );
    }

    /// `header_at` resolves a click within a recorded header rect to
    /// the right column index, and misses outside it.
    #[test]
    fn header_at_resolves_clicks_to_column() {
        let mut app = KanbanApp::new("demo");
        app.header_hits = vec![
            HeaderHit {
                area: Rect {
                    x: 0,
                    y: 1,
                    width: 20,
                    height: 1,
                },
                col_idx: 0,
            },
            HeaderHit {
                area: Rect {
                    x: 20,
                    y: 1,
                    width: 20,
                    height: 1,
                },
                col_idx: 1,
            },
            // A collapsed column registers its whole vertical strip.
            HeaderHit {
                area: Rect {
                    x: 40,
                    y: 1,
                    width: 3,
                    height: 12,
                },
                col_idx: 2,
            },
        ];
        assert_eq!(app.header_at(5, 1), Some(0));
        assert_eq!(app.header_at(25, 1), Some(1));
        assert_eq!(app.header_at(41, 8), Some(2));
        assert_eq!(app.header_at(41, 14), None, "below the strip");
        assert_eq!(app.header_at(50, 1), None, "right of every header");
    }

    /// Card hit-testing treats column headers as reserved space even
    /// when a stale or overlapping card rectangle also covers the same
    /// cell. The mouse handler checks headers first, and this keeps the
    /// lower-level card path from re-handling that same click.
    #[test]
    fn card_at_ignores_points_inside_header_hits() {
        let mut app = KanbanApp::new("demo");
        app.header_hits = vec![HeaderHit {
            area: Rect {
                x: 0,
                y: 1,
                width: 20,
                height: 1,
            },
            col_idx: 0,
        }];
        app.card_hits = vec![CardHit {
            area: Rect {
                x: 0,
                y: 1,
                width: 20,
                height: 3,
            },
            col_idx: 0,
            row_idx: 0,
        }];

        assert_eq!(app.card_at(5, 1), None, "header wins overlap");
        assert_eq!(app.card_at(5, 2), Some((0, 0)), "card body still hits");
    }

    /// Per-task overlay reads from the **owning workflow**, not
    /// `statuses.yaml`. Two workflows declare the same status id; each
    /// resolves its task's branch template against the owning
    /// workflow's `git:` block, so two cards in the same column can
    /// render two different resolved branches. The acceptance criterion
    /// behind "Per-task overlays read from the owning workflow."
    #[test]
    fn card_meta_line_reads_owning_workflow_not_statuses_yml() {
        let frontend = workflow_with_git("frontend", Some("feature/fe-{{feature}}"));
        let backend = workflow_with_git("backend", Some("feature/be-{{feature}}"));
        let workflows = vec![frontend, backend];

        let fe_task = task_with_params("a", Some("frontend"), &[("feature", "login")]);
        let be_task = task_with_params("b", Some("backend"), &[("feature", "login")]);

        // Both tasks live in the same project-wide `Todo` status, but
        // each resolves against its OWN workflow's `git:` template.
        let fe_line = card_meta_line(&fe_task, &workflows, "frontend", true, 60);
        let be_line = card_meta_line(&be_task, &workflows, "backend", true, 60);
        assert_eq!(line_text(&fe_line), " frontend  ⎇ feature/fe-login");
        assert_eq!(line_text(&be_line), " backend  ⎇ feature/be-login");
    }
}
