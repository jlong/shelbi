//! Issues-board semantics shared by every client.
//!
//! What a board shows and what it allows is decided here, once: which columns
//! render (from `workflows/statuses.yaml`, narrowed by a workflow filter), which
//! column a card sits in (its status resolved against its own workflow), the
//! order cards sort in, which column a card can move to next (the statuses its
//! workflow declares), what a card's meta line carries (the resolved branch),
//! the workspace / workflow filter options, and how the open board and the
//! on-demand terminal history merge. The TUI's kanban and the desktop board both
//! call these; neither decides any of it locally.
//!
//! The pure half (everything above) takes already-read data. The IO half
//! ([`read_open_board`], [`read_closed_page`], [`load_more_closed`]) is the
//! board read both clients run on a background refresher: the open board comes
//! from the daemon-owned board index (`read_board_report`, never a live backend
//! sweep), and the terminal `done`/`canceled` history from the done-history
//! cache on its own refresher, so a slow history fetch never delays the open
//! board's paint.

use std::collections::{BTreeMap, HashSet};

use shelbi_core::{
    default_project_statuses, default_workflow, Issue, ProjectStatuses, StatusCategory, Workflow,
    DEFAULT_WORKFLOW_NAME,
};
use shelbi_state::{IssueFile, KanbanColumnOverride};

use crate::view::ColumnDef;

// ---------------------------------------------------------------------------
// Columns
// ---------------------------------------------------------------------------

/// Build the column list from `statuses.yaml`. With no `workflow` every
/// declared status is a column, in declared order. With one, the list narrows
/// to the statuses that workflow declares, still in `statuses.yaml` order (the
/// loader rejects workflows that try to reorder relative to the catalogue).
pub fn columns_from(statuses: &ProjectStatuses, workflow: Option<&Workflow>) -> Vec<ColumnDef> {
    let allowed: Option<HashSet<&str>> =
        workflow.map(|wf| wf.statuses.iter().map(|s| s.id.as_str()).collect());
    statuses
        .statuses
        .iter()
        .filter(|st| {
            allowed
                .as_ref()
                .is_none_or(|set| set.contains(st.id.as_str()))
        })
        .map(|st| ColumnDef {
            status_id: st.id.clone(),
            status_name: st.name.clone(),
            category: st.category,
        })
        .collect()
}

/// The column header label: the stock status names in the board's capitals
/// (`In Progress` and the legacy `InProgress` both read `IN PROGRESS`), any
/// other name upper-cased.
pub fn column_label(status_name: &str) -> String {
    match status_name {
        "Backlog" => "BACKLOG".to_string(),
        "Todo" => "TO DO".to_string(),
        "In Progress" | "InProgress" => "IN PROGRESS".to_string(),
        "Review" => "REVIEW".to_string(),
        "Done" => "DONE".to_string(),
        other => other.to_uppercase(),
    }
}

/// Resolve the workflow status *id* `task` lives in. A task's position is
/// itself a status id, so this is mostly an identity; the fallbacks only matter
/// when the stored id isn't one the workflow declares (a workflow that renamed
/// a status):
///
/// 1. **Id match**: the workflow declares the stored id.
/// 2. **Category match**: the first workflow status whose category equals the
///    stored position's category (a workflow that renamed `in-progress` to
///    `design`, both `Active`).
/// 3. **Canonical**: the stored id unchanged. The card won't bucket cleanly,
///    but nothing crashes.
pub fn resolve_task_status(task: &Issue, workflow: &Workflow) -> String {
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

/// The workflow-aware lookups a board needs about a task: which workflow owns
/// it and which column it resolves to. Borrowed over the loaded workflows and
/// the project's default workflow name.
#[derive(Debug, Clone, Copy)]
pub struct BoardRules<'a> {
    pub workflows: &'a [Workflow],
    pub default_workflow_name: &'a str,
}

impl<'a> BoardRules<'a> {
    /// The task's workflow name: its own `workflow:`, else the project default.
    pub fn workflow_name<'t>(&self, task: &'t Issue) -> &'t str
    where
        'a: 't,
    {
        task.workflow.as_deref().unwrap_or(self.default_workflow_name)
    }

    /// The loaded workflow that owns `task`, if it is loaded.
    pub fn workflow_for(&self, task: &Issue) -> Option<&'a Workflow> {
        let name = self.workflow_name(task);
        self.workflows.iter().find(|w| w.name == name)
    }

    /// The status id `task` buckets under: resolved against its own workflow,
    /// then the `default` workflow, then the stored id as a last resort.
    pub fn resolved_status_id(&self, task: &Issue) -> String {
        if let Some(wf) = self.workflow_for(task) {
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
}

/// Index of the next/previous column a card in `current_status_id` can move
/// to under `workflow`. Columns whose status the workflow doesn't declare are
/// skipped. With `wrap` the step wraps within the workflow's eligible columns
/// (the board's `H`/`L`); without it the step clamps at the ends (the task
/// detail's move, where wrapping `done` back to `backlog` would be illegible).
///
/// `None` when the workflow has fewer than two eligible columns, when
/// `current_status_id` isn't a visible column (refuse to invent a move), or
/// when a non-wrapping step is already at the end.
pub fn adjacent_column(
    columns: &[ColumnDef],
    current_status_id: &str,
    workflow: &Workflow,
    forward: bool,
    wrap: bool,
) -> Option<usize> {
    let eligible: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| workflow.status(&c.status_id).is_some())
        .map(|(i, _)| i)
        .collect();
    if eligible.len() <= 1 {
        return None;
    }
    let current_col_idx = columns
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

/// Sort one column's cards in place. Done columns show the most recently
/// completed first (`updated_at` is rewritten on the move into done, so it is a
/// stable proxy for completion time); every other column keeps the board's
/// priority order (the slice's incoming order). Stable, so equal timestamps
/// keep their order and identical polls never reshuffle.
pub fn sort_column_cards(category: StatusCategory, cards: &mut [&IssueFile]) {
    if category == StatusCategory::Done {
        cards.sort_by_key(|tf| std::cmp::Reverse(tf.task.updated_at));
    }
}

/// Whether cards should carry their workflow-name badge: only when more than
/// one workflow is loaded and no workflow filter narrows the board to one, i.e.
/// when a column can actually mix workflows.
pub fn show_card_workflow_label(workflow_count: usize, workflow_filtered: bool) -> bool {
    workflow_count > 1 && !workflow_filtered
}

/// The branch a card's meta line shows: the task's own `branch` once the
/// orchestrator has cut it, otherwise the workflow's `git.base_branch` template
/// when it carries a `{{var}}` placeholder that resolves cleanly from the
/// task's params. A fixed `base_branch: main` or a half-substituted template
/// resolves to `None`: neither is per-task information.
pub fn card_branch(task: &Issue, workflows: &[Workflow], workflow_name: &str) -> Option<String> {
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

/// Merge the open board with the terminal history: the open board first (in its
/// priority order), then the first closed page, then any user-paged extras,
/// deduped by id with the open board winning. The board index deliberately
/// omits terminal history, so this is how the done column fills; a
/// `file_system` board, whose open read already returns everything, is
/// unaffected by the dedup.
pub fn merge_board(
    open: &[IssueFile],
    closed_first: &[IssueFile],
    closed_more: &[IssueFile],
) -> Vec<IssueFile> {
    let mut tasks = open.to_vec();
    let mut seen: HashSet<String> = tasks.iter().map(|tf| tf.task.id.clone()).collect();
    for tf in closed_first.iter().chain(closed_more.iter()) {
        if seen.insert(tf.task.id.clone()) {
            tasks.push(tf.clone());
        }
    }
    tasks
}

/// A pull-request link recorded on the issue, if any: a param whose value is an
/// `http(s)` URL to a pull request (`…/pull/<n>`). Issues carry no PR record of
/// their own and a live `gh` lookup would spend GitHub budget, so the board only
/// surfaces a link someone (an agent, a workflow action) wrote onto the issue.
pub fn pr_link(task: &Issue) -> Option<String> {
    task.string_params().into_values().find(|v| {
        (v.starts_with("https://") || v.starts_with("http://"))
            && v.contains("/pull/")
            && !v.contains(char::is_whitespace)
    })
}

// ---------------------------------------------------------------------------
// Column collapse
// ---------------------------------------------------------------------------

/// Resolved collapse state for one column. `Auto` (no user override) collapses
/// an empty column and expands a non-empty one; the explicit states win
/// regardless of count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ColumnExpansion {
    Auto,
    Collapsed,
    Expanded,
}

impl ColumnExpansion {
    /// The expansion a persisted override (or its absence) means.
    pub fn from_override(o: Option<KanbanColumnOverride>) -> Self {
        match o {
            Some(KanbanColumnOverride::Collapsed) => ColumnExpansion::Collapsed,
            Some(KanbanColumnOverride::Expanded) => ColumnExpansion::Expanded,
            None => ColumnExpansion::Auto,
        }
    }

    /// True when the column renders as its narrow collapsed strip.
    pub fn is_collapsed(self, task_count: usize) -> bool {
        match self {
            ColumnExpansion::Collapsed => true,
            ColumnExpansion::Expanded => false,
            ColumnExpansion::Auto => task_count == 0,
        }
    }

    /// The override a toggle writes: the opposite of what renders now, so a
    /// click never silently slides a column back to `Auto`.
    pub fn toggled(self, task_count: usize) -> KanbanColumnOverride {
        if self.is_collapsed(task_count) {
            KanbanColumnOverride::Expanded
        } else {
            KanbanColumnOverride::Collapsed
        }
    }
}

/// The workflow a column override is scoped to: the active workflow filter, or
/// the project default with no filter, so a `review` column shared by two
/// workflows keeps an independent override per workflow view.
pub fn override_workflow<'a>(workflow_filter: Option<&'a str>, default_workflow: &'a str) -> &'a str {
    workflow_filter.unwrap_or(default_workflow)
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// The workspace filter on the visible cards. `None` (no filter) is "All
/// workspaces".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceFilter {
    /// Only cards assigned to this workspace.
    Workspace(String),
    /// Only cards with no `assigned_to`.
    Unassigned,
}

impl WorkspaceFilter {
    /// The `state.json::workspace_filter` sentinel for [`Self::Unassigned`],
    /// so the on-disk schema stays a plain `Option<String>`.
    pub const UNASSIGNED_SENTINEL: &'static str = "__unassigned__";

    /// Parse the persisted `state.json::workspace_filter` value.
    pub fn from_disk(s: &str) -> WorkspaceFilter {
        if s == Self::UNASSIGNED_SENTINEL {
            WorkspaceFilter::Unassigned
        } else {
            WorkspaceFilter::Workspace(s.to_string())
        }
    }

    /// The persisted form [`set_workspace_filter`](shelbi_state::set_workspace_filter) takes.
    pub fn to_disk(&self) -> String {
        match self {
            WorkspaceFilter::Workspace(w) => w.clone(),
            WorkspaceFilter::Unassigned => Self::UNASSIGNED_SENTINEL.to_string(),
        }
    }

    /// Whether a card with this `assigned_to` passes.
    pub fn matches(&self, assigned_to: Option<&str>) -> bool {
        match self {
            WorkspaceFilter::Workspace(w) => assigned_to == Some(w.as_str()),
            WorkspaceFilter::Unassigned => assigned_to.is_none(),
        }
    }

    /// The chip / dropdown label.
    pub fn label(&self) -> String {
        match self {
            WorkspaceFilter::Workspace(w) => w.clone(),
            WorkspaceFilter::Unassigned => "Unassigned".to_string(),
        }
    }
}

/// One workspace-filter option with how many cards it matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceOption {
    /// `None` is the "All workspaces" reset row.
    pub filter: Option<WorkspaceFilter>,
    pub count: usize,
}

/// The workspace dropdown's options: All, each declared workspace in YAML
/// order (even with zero cards, so a filter can always be picked), then
/// Unassigned when any card lacks an assignee. Counts are over the unfiltered
/// board.
pub fn workspace_options(tasks: &[IssueFile], workspaces: &[String]) -> Vec<WorkspaceOption> {
    let mut opts = Vec::with_capacity(workspaces.len() + 2);
    opts.push(WorkspaceOption {
        filter: None,
        count: tasks.len(),
    });
    for w in workspaces {
        let count = tasks
            .iter()
            .filter(|tf| tf.task.assigned_to.as_deref() == Some(w.as_str()))
            .count();
        opts.push(WorkspaceOption {
            filter: Some(WorkspaceFilter::Workspace(w.clone())),
            count,
        });
    }
    let unassigned = tasks
        .iter()
        .filter(|tf| tf.task.assigned_to.is_none())
        .count();
    if unassigned > 0 {
        opts.push(WorkspaceOption {
            filter: Some(WorkspaceFilter::Unassigned),
            count: unassigned,
        });
    }
    opts
}

/// One workflow-filter option with how many cards belong to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowOption {
    /// `None` is the "All workflows" reset row.
    pub filter: Option<String>,
    pub count: usize,
}

/// The workflow dropdown's options: All, then each loaded workflow (even with
/// zero cards). Counts are over the unfiltered board.
pub fn workflow_options(tasks: &[IssueFile], rules: BoardRules<'_>) -> Vec<WorkflowOption> {
    let mut opts = Vec::with_capacity(rules.workflows.len() + 1);
    opts.push(WorkflowOption {
        filter: None,
        count: tasks.len(),
    });
    for w in rules.workflows {
        let count = tasks
            .iter()
            .filter(|tf| rules.workflow_name(&tf.task) == w.name)
            .count();
        opts.push(WorkflowOption {
            filter: Some(w.name.clone()),
            count,
        });
    }
    opts
}

/// The workflow filter after one cycle step (the board's `tab`): All → first
/// workflow → … → last → All. A filter naming a workflow that no longer exists
/// wraps back to All so the unfiltered view is always reachable. `None` input
/// with no workflows stays `None`.
pub fn next_workflow_filter(workflows: &[Workflow], current: Option<&str>) -> Option<String> {
    if workflows.is_empty() {
        return None;
    }
    match current {
        None => Some(workflows[0].name.clone()),
        Some(current) => match workflows.iter().position(|w| w.name == current) {
            Some(i) if i + 1 < workflows.len() => Some(workflows[i + 1].name.clone()),
            _ => None,
        },
    }
}

// ---------------------------------------------------------------------------
// Board reads (IO; run these off the UI thread)
// ---------------------------------------------------------------------------

/// The project-level fields a board read carries.
#[derive(Debug, Clone)]
pub struct BoardProject {
    pub default_workflow_name: String,
    pub display_name: Option<String>,
    pub workspaces: Vec<String>,
}

/// How the open-board read went.
#[derive(Debug, Clone)]
pub enum BoardOutcome {
    /// `issue_store_for` or `read_board_report` failed; a ready-to-show message.
    Failed(String),
    /// No index published yet: paint the column chrome with a loading state
    /// until the daemon fills it.
    Cold { banner: Option<String> },
    /// A warm or stale read of the **open** board. Terminal history is read
    /// separately ([`read_closed_page`]).
    Warm {
        banner: Option<String>,
        base_tasks: Vec<IssueFile>,
    },
}

/// Everything one open-board refresh reads, produced off the UI thread by
/// [`read_open_board`] and folded in on it.
#[derive(Debug, Clone)]
pub struct BoardRead {
    /// `None` when the project YAML couldn't be read.
    pub project: Option<BoardProject>,
    /// The project state (persisted workspace filter, column overrides).
    pub state_snapshot: Option<shelbi_state::State>,
    /// The loaded workflows, or the load error for the status line.
    pub workflows: Result<Vec<Workflow>, String>,
    /// `statuses.yaml`, or the load error for the status line.
    pub project_statuses: Result<ProjectStatuses, String>,
    pub board: BoardOutcome,
}

/// Read the open board for `project`: the project YAML, its state, workflows,
/// `statuses.yaml`, and the open issues from the daemon-owned board index
/// (`read_board_report`, which serves this process's snapshot cache when no
/// index or daemon exists). Never a live backend sweep, so it costs no GitHub
/// budget; on a cold project it returns [`BoardOutcome::Cold`] without
/// blocking.
pub fn read_open_board(project: &str) -> BoardRead {
    let project_cfg = shelbi_state::load_project(project)
        .ok()
        .map(|p| BoardProject {
            default_workflow_name: p.default_workflow_name().to_string(),
            display_name: p.display_name.clone().or_else(|| p.label.clone()),
            workspaces: p.workspaces.into_iter().map(|w| w.name).collect(),
        });
    let state_snapshot = shelbi_state::read_state(project).ok();
    let workflows =
        shelbi_state::list_workflows(project).map_err(|e| format!("workflow load failed: {e}"));
    let project_statuses = shelbi_state::load_project_statuses(project)
        .map_err(|e| format!("statuses.yaml load failed: {e}"));
    let board = match shelbi_state::issue_store_for(project) {
        Err(e) => BoardOutcome::Failed(format!("refresh failed: {e}")),
        Ok(_store) => match shelbi_state::read_board_report(project) {
            Err(e) => BoardOutcome::Failed(format!("refresh failed: {e}")),
            Ok(report) => {
                let banner = report.freshness.banner();
                match report.state {
                    shelbi_state::BoardState::Cold => BoardOutcome::Cold { banner },
                    state => BoardOutcome::Warm {
                        banner,
                        base_tasks: state.into_issues(),
                    },
                }
            }
        },
    };
    BoardRead {
        project: project_cfg,
        state_snapshot,
        workflows,
        project_statuses,
        board,
    }
}

/// One page of terminal `done`/`canceled` history.
#[derive(Debug, Clone)]
pub struct ClosedPage {
    pub issues: Vec<IssueFile>,
    /// Cursor for the next page; `None` when this is the whole history.
    pub next_cursor: Option<String>,
}

/// Read the first page of terminal history, on its own refresher. Served from
/// the ten-minute done-history cache (so at most one live fetch per TTL), and
/// skipped for a cold board. `None` when the store can't be built or the read
/// fails; the caller keeps its last-known lanes.
pub fn read_closed_page(project: &str) -> Option<ClosedPage> {
    let store = shelbi_state::issue_store_for(project).ok()?;
    match shelbi_state::read_board_report(project) {
        Ok(report) if report.state.is_cold() => None,
        Ok(_) => store.closed_page(None).ok().map(|page| ClosedPage {
            issues: page.issues,
            next_cursor: page.next_cursor,
        }),
        Err(_) => None,
    }
}

/// Fetch the page after `cursor`: the done column's explicit "load more". A
/// live, uncached fetch, so only ever user-driven.
pub fn load_more_closed(project: &str, cursor: &str) -> Result<ClosedPage, String> {
    let store =
        shelbi_state::issue_store_for(project).map_err(|e| format!("load more failed: {e}"))?;
    store
        .closed_page(Some(cursor))
        .map(|page| ClosedPage {
            issues: page.issues,
            next_cursor: page.next_cursor,
        })
        .map_err(|e| format!("load more failed: {e}"))
}

// ---------------------------------------------------------------------------
// The board input a client accumulates
// ---------------------------------------------------------------------------

/// What a client knows about the board, folded from the open-board and
/// closed-page refreshers as they land (each independently, so the open board
/// paints without waiting on history). [`crate::IssuesModel::build`] renders it.
#[derive(Debug, Clone)]
pub struct BoardInput {
    pub project_statuses: ProjectStatuses,
    pub workflows: Vec<Workflow>,
    pub default_workflow_name: String,
    pub display_name: Option<String>,
    pub workspaces: Vec<String>,
    /// The persisted workspace filter (`state.json`), reloaded every read so a
    /// change from another client shows up.
    pub workspace_filter: Option<WorkspaceFilter>,
    /// Persisted column collapse overrides, keyed by
    /// [`shelbi_state::kanban_column_override_key`].
    pub column_overrides: BTreeMap<String, KanbanColumnOverride>,
    /// The open board as last read.
    pub open: Vec<IssueFile>,
    /// The first terminal-history page as last read.
    pub closed_first: Vec<IssueFile>,
    /// History pages beyond the first, from explicit "load more".
    pub closed_more: Vec<IssueFile>,
    pub closed_next_cursor: Option<String>,
    /// Whether a closed page has landed yet (the done column is still filling
    /// until it has).
    pub closed_loaded: bool,
    /// True until the first warm read (a cold index, or nothing read yet).
    pub loading: bool,
    /// The board freshness banner (`board 12s`, `board 4m stale …`).
    pub banner: Option<String>,
    /// The last read error, if the latest read failed.
    pub error: Option<String>,
}

impl Default for BoardInput {
    fn default() -> Self {
        BoardInput {
            project_statuses: default_project_statuses(),
            workflows: vec![default_workflow()],
            default_workflow_name: DEFAULT_WORKFLOW_NAME.to_string(),
            display_name: None,
            workspaces: Vec::new(),
            workspace_filter: None,
            column_overrides: BTreeMap::new(),
            open: Vec::new(),
            closed_first: Vec::new(),
            closed_more: Vec::new(),
            closed_next_cursor: None,
            closed_loaded: false,
            loading: true,
            banner: None,
            error: None,
        }
    }
}

impl BoardInput {
    /// Fold an open-board read. A failed workflow or `statuses.yaml` load
    /// degrades to the canonical defaults (and reports in `error`), so the
    /// board still paints. A failed board read keeps the last-known cards.
    pub fn apply_open(&mut self, read: BoardRead) {
        let mut errors = Vec::new();
        match read.project {
            Some(p) => {
                self.default_workflow_name = p.default_workflow_name;
                self.display_name = p.display_name;
                self.workspaces = p.workspaces;
            }
            None => {
                self.default_workflow_name = DEFAULT_WORKFLOW_NAME.to_string();
                self.display_name = None;
                self.workspaces = Vec::new();
            }
        }
        self.workspace_filter = read
            .state_snapshot
            .as_ref()
            .and_then(|s| s.workspace_filter.as_deref())
            .map(WorkspaceFilter::from_disk);
        self.column_overrides = read
            .state_snapshot
            .map(|s| s.kanban_column_overrides)
            .unwrap_or_default();
        self.workflows = match read.workflows {
            Ok(wfs) => wfs,
            Err(msg) => {
                errors.push(msg);
                vec![default_workflow()]
            }
        };
        self.project_statuses = match read.project_statuses {
            Ok(ps) => ps,
            Err(msg) => {
                errors.push(msg);
                default_project_statuses()
            }
        };
        match read.board {
            BoardOutcome::Failed(msg) => {
                self.banner = None;
                errors.push(msg);
            }
            BoardOutcome::Cold { banner } => {
                self.banner = banner;
                self.loading = true;
            }
            BoardOutcome::Warm { banner, base_tasks } => {
                self.banner = banner;
                self.loading = false;
                self.open = base_tasks;
            }
        }
        self.error = (!errors.is_empty()).then(|| errors.join("; "));
    }

    /// Fold a closed-page read. `None` (a transient failure) keeps the
    /// last-known lanes rather than blanking the done column.
    pub fn apply_closed(&mut self, closed: Option<ClosedPage>) {
        if let Some(page) = closed {
            self.closed_first = page.issues;
            self.closed_next_cursor = page.next_cursor;
            self.closed_loaded = true;
        }
    }

    /// Append a "load more" page, keeping only ids not already on the board.
    pub fn apply_more(&mut self, page: ClosedPage) {
        self.closed_next_cursor = page.next_cursor;
        let mut seen: HashSet<String> = self
            .open
            .iter()
            .chain(self.closed_first.iter())
            .chain(self.closed_more.iter())
            .map(|tf| tf.task.id.clone())
            .collect();
        for tf in page.issues {
            if seen.insert(tf.task.id.clone()) {
                self.closed_more.push(tf);
            }
        }
    }

    /// The merged board (open first, then terminal history).
    pub fn tasks(&self) -> Vec<IssueFile> {
        merge_board(&self.open, &self.closed_first, &self.closed_more)
    }

    pub fn rules(&self) -> BoardRules<'_> {
        BoardRules {
            workflows: &self.workflows,
            default_workflow_name: &self.default_workflow_name,
        }
    }

    /// The visible columns under a workflow filter. A filter naming a workflow
    /// that no longer exists falls back to every column so the board never goes
    /// blank and the filter stays clearable.
    pub fn columns(&self, workflow_filter: Option<&str>) -> Vec<ColumnDef> {
        let wf = workflow_filter.and_then(|name| self.workflows.iter().find(|w| w.name == name));
        columns_from(&self.project_statuses, wf)
    }

    /// The collapse state of `status_id`'s column under a workflow filter.
    pub fn expansion(&self, workflow_filter: Option<&str>, status_id: &str) -> ColumnExpansion {
        let workflow = override_workflow(workflow_filter, &self.default_workflow_name);
        let key = shelbi_state::kanban_column_override_key(workflow, status_id);
        ColumnExpansion::from_override(self.column_overrides.get(&key).copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{Column, ProjectStatus, WorkflowStatus};

    fn issue(id: &str, status: &str) -> IssueFile {
        let now = chrono::DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        IssueFile {
            task: Issue {
                id: id.to_string(),
                title: id.to_string(),
                column: Column::from_status_id(status),
                priority: 0,
                assigned_to: None,
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: now,
                updated_at: now,
                params: BTreeMap::new(),
            },
            body: String::new(),
            tracker_assignees: Vec::new(),
        }
    }

    fn workflow(name: &str, statuses: &[(&str, StatusCategory)]) -> Workflow {
        let mut wf = default_workflow();
        wf.name = name.to_string();
        wf.statuses = statuses
            .iter()
            .map(|(id, cat)| {
                let mut st: WorkflowStatus = default_workflow().statuses[0].clone();
                st.id = id.to_string();
                st.name = id.to_string();
                st.category = *cat;
                st
            })
            .collect();
        wf
    }

    fn statuses(ids: &[(&str, StatusCategory)]) -> ProjectStatuses {
        ProjectStatuses {
            statuses: ids
                .iter()
                .map(|(id, cat)| ProjectStatus {
                    id: id.to_string(),
                    name: id.to_string(),
                    category: *cat,
                })
                .collect(),
        }
    }

    #[test]
    fn columns_follow_statuses_yaml_and_narrow_to_a_workflow() {
        use StatusCategory::*;
        let ps = statuses(&[
            ("backlog", Backlog),
            ("todo", Ready),
            ("in-progress", Active),
            ("review", Handoff),
            ("done", Done),
        ]);
        let all = columns_from(&ps, None);
        assert_eq!(
            all.iter().map(|c| c.status_id.as_str()).collect::<Vec<_>>(),
            ["backlog", "todo", "in-progress", "review", "done"]
        );
        // A workflow without review narrows the board but keeps the catalogue order.
        let sub = workflow("subtask", &[("done", Done), ("backlog", Backlog), ("in-progress", Active)]);
        let narrowed = columns_from(&ps, Some(&sub));
        assert_eq!(
            narrowed.iter().map(|c| c.status_id.as_str()).collect::<Vec<_>>(),
            ["backlog", "in-progress", "done"]
        );
    }

    #[test]
    fn adjacent_column_skips_undeclared_statuses_and_wraps_or_clamps() {
        use StatusCategory::*;
        let ps = statuses(&[
            ("backlog", Backlog),
            ("todo", Ready),
            ("in-progress", Active),
            ("review", Handoff),
            ("done", Done),
        ]);
        let cols = columns_from(&ps, None);
        // No `todo` or `review` in this workflow: moves hop over those columns.
        let wf = workflow("subtask", &[("backlog", Backlog), ("in-progress", Active), ("done", Done)]);
        assert_eq!(adjacent_column(&cols, "backlog", &wf, true, true), Some(2));
        assert_eq!(adjacent_column(&cols, "in-progress", &wf, true, true), Some(4));
        // Wrap from done back to backlog on the board; clamp in the detail.
        assert_eq!(adjacent_column(&cols, "done", &wf, true, true), Some(0));
        assert_eq!(adjacent_column(&cols, "done", &wf, true, false), None);
        assert_eq!(adjacent_column(&cols, "backlog", &wf, false, false), None);
        assert_eq!(adjacent_column(&cols, "backlog", &wf, false, true), Some(4));
        // An off-board status never invents a move.
        assert_eq!(adjacent_column(&cols, "nowhere", &wf, true, true), None);
    }

    #[test]
    fn resolve_task_status_prefers_id_then_category() {
        use StatusCategory::*;
        let design = workflow("design", &[("backlog", Backlog), ("design", Active), ("qa", Handoff)]);
        assert_eq!(resolve_task_status(&issue("a", "backlog").task, &design), "backlog");
        assert_eq!(resolve_task_status(&issue("b", "in-progress").task, &design), "design");
        assert_eq!(resolve_task_status(&issue("c", "review").task, &design), "qa");
    }

    #[test]
    fn done_sorts_newest_first_and_other_columns_keep_priority_order() {
        let mut a = issue("a", "done");
        let mut b = issue("b", "done");
        a.task.updated_at -= chrono::Duration::hours(1);
        b.task.updated_at += chrono::Duration::hours(1);
        let mut cards = vec![&a, &b];
        sort_column_cards(StatusCategory::Done, &mut cards);
        assert_eq!(cards[0].task.id, "b");
        let mut cards = vec![&a, &b];
        sort_column_cards(StatusCategory::Ready, &mut cards);
        assert_eq!(cards[0].task.id, "a", "non-done columns keep incoming order");
    }

    #[test]
    fn merge_board_dedups_with_the_open_board_winning() {
        let mut open_copy = issue("x", "in-progress");
        open_copy.task.title = "open".into();
        let mut closed_copy = issue("x", "done");
        closed_copy.task.title = "closed".into();
        let merged = merge_board(&[open_copy], &[closed_copy, issue("y", "done")], &[issue("y", "done")]);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].task.title, "open");
        assert_eq!(merged[1].task.id, "y");
    }

    #[test]
    fn workspace_filter_round_trips_and_options_count_the_unfiltered_board() {
        assert_eq!(
            WorkspaceFilter::from_disk(&WorkspaceFilter::Unassigned.to_disk()),
            WorkspaceFilter::Unassigned
        );
        let mut assigned = issue("a", "todo");
        assigned.task.assigned_to = Some("alpha".into());
        let tasks = vec![assigned, issue("b", "todo")];
        let opts = workspace_options(&tasks, &["alpha".into(), "bravo".into()]);
        assert_eq!(opts.len(), 4);
        assert_eq!(opts[0], WorkspaceOption { filter: None, count: 2 });
        assert_eq!(opts[1].count, 1);
        assert_eq!(opts[2].count, 0, "a workspace with no cards is still offered");
        assert_eq!(opts[3].filter, Some(WorkspaceFilter::Unassigned));
    }

    #[test]
    fn workflow_filter_cycles_through_every_workflow_back_to_all() {
        let wfs = vec![workflow("a", &[]), workflow("b", &[])];
        assert_eq!(next_workflow_filter(&wfs, None).as_deref(), Some("a"));
        assert_eq!(next_workflow_filter(&wfs, Some("a")).as_deref(), Some("b"));
        assert_eq!(next_workflow_filter(&wfs, Some("b")), None);
        assert_eq!(next_workflow_filter(&wfs, Some("gone")), None);
        assert_eq!(next_workflow_filter(&[], None), None);
    }

    #[test]
    fn pr_link_finds_a_pull_request_url_param() {
        let parse = |extra: &str| {
            let text = format!(
                "---\nid: a\ntitle: A\ncolumn: review\npriority: 0\n{extra}\
                 created_at: 2026-10-01T10:00:00Z\nupdated_at: 2026-10-01T10:00:00Z\n---\nbody\n"
            );
            shelbi_state::parse_task_file(&text).unwrap().task
        };
        assert_eq!(pr_link(&parse("")), None);
        assert_eq!(
            pr_link(&parse("note: see https://example.com/docs\n")),
            None,
            "a non-PR URL is not a PR link"
        );
        assert_eq!(
            pr_link(&parse("pr: https://github.com/jlong/shelbi/pull/42\n")).as_deref(),
            Some("https://github.com/jlong/shelbi/pull/42")
        );
    }

    #[test]
    fn expansion_auto_collapses_empty_and_toggle_writes_the_opposite() {
        assert!(ColumnExpansion::Auto.is_collapsed(0));
        assert!(!ColumnExpansion::Auto.is_collapsed(3));
        assert_eq!(ColumnExpansion::Auto.toggled(0), KanbanColumnOverride::Expanded);
        assert_eq!(ColumnExpansion::Auto.toggled(3), KanbanColumnOverride::Collapsed);
        assert!(!ColumnExpansion::Expanded.is_collapsed(0));
    }

    #[test]
    fn closed_page_fills_without_touching_the_open_board_and_failures_keep_lanes() {
        let mut input = BoardInput {
            open: vec![issue("o", "todo")],
            ..BoardInput::default()
        };
        input.apply_closed(Some(ClosedPage {
            issues: vec![issue("d", "done")],
            next_cursor: Some("c1".into()),
        }));
        assert!(input.closed_loaded);
        assert_eq!(input.tasks().len(), 2);
        input.apply_closed(None);
        assert_eq!(input.tasks().len(), 2, "a failed closed read keeps the last lanes");
        input.apply_more(ClosedPage {
            issues: vec![issue("d", "done"), issue("e", "done")],
            next_cursor: None,
        });
        assert_eq!(input.tasks().len(), 3, "load more dedups against the board");
        assert!(input.closed_next_cursor.is_none());
    }
}
