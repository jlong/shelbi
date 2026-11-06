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

use serde::{Deserialize, Serialize};

use shelbi_core::{
    default_project_statuses, default_workflow, Issue, IssuePr, ProjectStatuses, StatusCategory, Workflow,
    DEFAULT_WORKFLOW_NAME,
};
use shelbi_state::{IssueFile, KanbanColumnOverride, OwnershipGate};

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

/// What dropping a card on a column does (a drag on the board). The board
/// renders refused targets as not droppable before the drop, and only sends a
/// mutation for [`DropTarget::Move`] and [`DropTarget::Reorder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DropTarget {
    /// The card's own column: a reorder within it (priority).
    Reorder,
    /// A legal status move. `merge` when the edge runs the workflow's gated
    /// merge (the daemon runs the transition's actions, exactly as
    /// `shelbi issue move` does).
    Move { merge: bool },
    /// Not a legal drop, with the reason to show.
    Refused(String),
}

/// Whether `task` may be dropped on the column `target_status_id` (whose
/// category is `target_category`). Moves follow the card's workflow:
/// transitions are any-to-any between the statuses it declares
/// ([`Workflow::transition_allowed`]), so a column whose status the workflow
/// doesn't declare is refused. A drop on the card's own column is a reorder,
/// refused in a done column, which shows newest first rather than by priority.
pub fn drop_target(
    rules: BoardRules<'_>,
    task: &Issue,
    target_status_id: &str,
    target_category: StatusCategory,
) -> DropTarget {
    let current = rules.resolved_status_id(task);
    if current == target_status_id {
        if target_category == StatusCategory::Done {
            return DropTarget::Refused("done is ordered by completion".to_string());
        }
        return DropTarget::Reorder;
    }
    let Some(wf) = rules.workflow_for(task) else {
        return DropTarget::Refused(format!(
            "its workflow `{}` isn't loaded",
            rules.workflow_name(task)
        ));
    };
    if !wf.transition_allowed(&current, target_status_id) {
        return DropTarget::Refused(format!("not in the `{}` workflow", wf.name));
    }
    DropTarget::Move {
        merge: wf.is_merge_transition(&current, target_status_id),
    }
}

/// The absolute slot to send as a priority move (`PrioMove::Set`) so `id`
/// lands where `target_id` sits now, or at the bottom when `target_id` is
/// `None`. Slots index the card's whole stored column in the stores' canonical
/// order (priority, then id), which the store re-derives under its lock: the
/// board shows a filtered column, so a visible row index is not a slot.
/// `None` when either card is missing or they sit in different stored columns.
pub fn reorder_slot(tasks: &[IssueFile], id: &str, target_id: Option<&str>) -> Option<u32> {
    let card = tasks.iter().find(|tf| tf.task.id == id)?;
    let mut column: Vec<&IssueFile> = tasks
        .iter()
        .filter(|tf| tf.task.column == card.task.column)
        .collect();
    column.sort_by(|a, b| {
        a.task
            .priority
            .cmp(&b.task.priority)
            .then_with(|| a.task.id.cmp(&b.task.id))
    });
    let slot = match target_id {
        Some(target) => column.iter().position(|tf| tf.task.id == target)?,
        None => column.len().checked_sub(1)?,
    };
    u32::try_from(slot).ok()
}

/// The slot `id` sits at now in its stored column ([`reorder_slot`]'s order).
pub fn current_slot(tasks: &[IssueFile], id: &str) -> Option<u32> {
    reorder_slot(tasks, id, Some(id))
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

/// Whether a card matches the board's text search: every whitespace-separated
/// word of `query` appears, ignoring case, in the card's title, id or branch
/// (words may match different fields). A blank query matches every card.
pub fn card_matches_search(title: &str, id: &str, branch: Option<&str>, query: &str) -> bool {
    let haystack = format!("{title}\n{id}\n{}", branch.unwrap_or_default()).to_lowercase();
    query
        .split_whitespace()
        .all(|word| haystack.contains(&word.to_lowercase()))
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

/// The longest excerpt [`card_summary`] keeps (a card clamps it to a few
/// lines anyway).
const SUMMARY_MAX_CHARS: usize = 280;

/// A card's excerpt: the first prose paragraph of an issue's markdown body,
/// as plain text. Headings, code fences, HTML comments and rules are skipped;
/// list and quote markers, emphasis, inline code ticks and link targets are
/// dropped; the lines join with single spaces. Empty when the body has no
/// prose.
pub fn card_summary(body: &str) -> String {
    let mut words: Vec<String> = Vec::new();
    let mut in_fence = false;
    for line in body.lines() {
        let line = line.trim();
        if line.starts_with("```") || line.starts_with("~~~") {
            in_fence = !in_fence;
            if !words.is_empty() {
                break;
            }
            continue;
        }
        let skipped = line.is_empty()
            || line.starts_with('#')
            || line.starts_with("<!--")
            || line.chars().all(|c| matches!(c, '-' | '*' | '_' | ' '));
        if in_fence || skipped {
            if !words.is_empty() {
                break;
            }
            continue;
        }
        words.extend(plain_inline(strip_block_marker(line)).split_whitespace().map(str::to_string));
    }
    let text = words.join(" ");
    match text.char_indices().nth(SUMMARY_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", text[..cut].trim_end()),
        None => text,
    }
}

/// `line` without a leading list, task-list or quote marker.
fn strip_block_marker(line: &str) -> &str {
    let mut rest = line;
    while let Some(r) = rest.strip_prefix('>') {
        rest = r.trim_start();
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(r) = rest.strip_prefix(bullet) {
            rest = r;
            break;
        }
    }
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(r) = rest[digits..].strip_prefix(". ") {
            rest = r;
        }
    }
    for task in ["[ ] ", "[x] ", "[X] "] {
        if let Some(r) = rest.strip_prefix(task) {
            rest = r;
            break;
        }
    }
    rest
}

/// Inline markdown as plain text: `[text](url)` and `![alt](url)` keep their
/// text, `**`, `__` and backticks go.
fn plain_inline(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(open) = rest.find('[') {
        let (before, from) = rest.split_at(open);
        let link = from[1..].find("](").and_then(|close| {
            let target = &from[1 + close + 2..];
            target.find(')').map(|end| (&from[1..1 + close], &target[end + 1..]))
        });
        match link {
            Some((text, after)) => {
                out.push_str(before.strip_suffix('!').unwrap_or(before));
                out.push_str(text);
                rest = after;
            }
            None => {
                out.push_str(before);
                out.push('[');
                rest = &from[1..];
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace("__", "").replace('`', "")
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

/// The issue's pull request: its record, filled in by the board's read
/// (`shelbi_state::pr_status`), or for an issue with no record, a param someone
/// wrote whose value is a pull-request URL (`pr_url: https://…/pull/12`). A
/// render never asks GitHub: a live lookup would spend budget.
pub fn task_pr(task: &Issue) -> Option<IssuePr> {
    task.pr.clone().or_else(|| {
        task.string_params().into_values().find_map(|v| {
            let number = shelbi_core::pr_number_from_url(&v)?;
            Some(IssuePr::new(number, Some(v)))
        })
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardProject {
    #[serde(default)]
    pub default_workflow_name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<String>,
    /// The tracker-ownership gate cards are marked against
    /// ([`shelbi_state::ownership`]): unrestricted on a filesystem board.
    /// Decodes as [`OwnershipGate::FailClosed`] when missing, the same
    /// fallback [`read_open_board`] uses when the gate can't be read.
    #[serde(default = "crate::wire::fail_closed")]
    pub ownership: OwnershipGate,
    /// The tracker can search its issues remotely (a GitHub board), so the
    /// board's text search asks it ([`crate::board_search`]).
    #[serde(default)]
    pub remote_search: bool,
}

/// How the open-board read went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardOutcome {
    /// `issue_store_for` or `read_board_report` failed; a ready-to-show message.
    Failed(String),
    /// No index published yet: paint the column chrome with a loading state
    /// until the daemon fills it.
    Cold {
        #[serde(default)]
        banner: Option<String>,
    },
    /// A warm or stale read of the **open** board. Terminal history is read
    /// separately ([`read_closed_page`]).
    Warm {
        #[serde(default)]
        banner: Option<String>,
        #[serde(default)]
        base_tasks: Vec<IssueFile>,
    },
}

/// Everything one open-board refresh reads, produced off the UI thread by
/// [`read_open_board`] and folded in on it.
///
/// Serializable for the remote window's query service (`crate::wire` pins the
/// JSON shape). Every field decodes when missing: an absent `board` reads as
/// [`BoardOutcome::Cold`], the loading state, never as an empty board.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoardRead {
    /// `None` when the project YAML couldn't be read.
    #[serde(default)]
    pub project: Option<BoardProject>,
    /// The project state (persisted workspace filter, column overrides).
    #[serde(default)]
    pub state_snapshot: Option<shelbi_state::State>,
    /// The loaded workflows, or the load error for the status line.
    #[serde(default = "crate::wire::ok_default", with = "crate::wire::workflows")]
    pub workflows: Result<Vec<Workflow>, String>,
    /// `statuses.yaml`, or the load error for the status line.
    #[serde(default = "crate::wire::ok_default")]
    pub project_statuses: Result<ProjectStatuses, String>,
    #[serde(default = "crate::wire::cold_board")]
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
            // From the board index, no API call: a render never spends budget.
            ownership: shelbi_state::ownership::gate_from_index(project, &p.issue_tracker)
                .unwrap_or(OwnershipGate::FailClosed),
            remote_search: matches!(
                p.issue_tracker.backend,
                shelbi_core::IssueTrackerBackend::Github
            ),
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
                    state => {
                        let mut base_tasks = state.into_issues();
                        shelbi_state::pr_status::fold(project, &mut base_tasks);
                        BoardOutcome::Warm { banner, base_tasks }
                    }
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

/// One page of terminal `done`/`canceled` history. Serializable for the remote
/// window's query service.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
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
        Ok(_) => store.closed_page(None).ok().map(|page| closed_page(project, page)),
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
        .map(|page| closed_page(project, page))
        .map_err(|e| format!("load more failed: {e}"))
}

/// A page from the store, with what the hub knows of each issue's PR.
fn closed_page(project: &str, page: shelbi_state::ClosedPage) -> ClosedPage {
    let mut issues = page.issues;
    shelbi_state::pr_status::fold(project, &mut issues);
    ClosedPage {
        issues,
        next_cursor: page.next_cursor,
    }
}

// ---------------------------------------------------------------------------
// Done-history paging
// ---------------------------------------------------------------------------

/// What a client shows while a "load more" fetch runs.
pub const LOADING_MORE_STATUS: &str = "loading more history…";

/// One "load more" fetch, from [`ClosedHistory::begin_more`]. Fetch the page
/// after [`cursor`](Self::cursor), off the UI thread, and hand this back with
/// the answer to [`ClosedHistory::finish_more`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoreRequest {
    pub cursor: String,
    /// The paging run this fetch belongs to. A reset starts a new one, so an
    /// answer anchored on the old first page can't land on the new one.
    run: u64,
}

/// How far into the history the board has paged.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Depth {
    /// No closed page has landed yet: the done column is still filling.
    #[default]
    Unread,
    /// Only page 1 is loaded, so the next cursor is page 1's and a first-page
    /// refresh moves it.
    FirstPage,
    /// At least one "load more" page landed, so the next cursor is the deepest
    /// page's and a first-page refresh that keeps the same cards leaves it be.
    /// Counted by landed pages, not by cards: a page whose cards were all
    /// already on the board still moved the cursor.
    Paged,
}

/// The terminal `done`/`canceled` history a board has loaded: page 1 as last
/// read by the closed-page refresher, the pages beyond it from an explicit
/// "load more", and the one cursor that says what comes next.
///
/// The refresher re-reads page 1 on every tick while "load more" walks forward,
/// so both write the cursor, and they only agree through this state:
///
/// - A refresh with the same page-1 cards updates those cards and keeps the
///   loaded pages and the cursor. The same cards in a new order count as the
///   same: nothing crossed the page boundary.
/// - A refresh with different page-1 cards resets the pages and the cursor
///   together, and abandons a fetch in flight. Cards crossed the page boundary,
///   so keeping the old pages could leave a hole in the history (the card that
///   fell off page 1 is on none of them).
#[derive(Debug, Clone, Default)]
pub struct ClosedHistory {
    first: Vec<IssueFile>,
    more: Vec<IssueFile>,
    next_cursor: Option<String>,
    depth: Depth,
    run: u64,
    fetching: bool,
}

impl ClosedHistory {
    /// Whether a closed page has landed yet.
    pub fn landed(&self) -> bool {
        self.depth != Depth::Unread
    }

    /// Page 1 as last read.
    pub fn first(&self) -> &[IssueFile] {
        &self.first
    }

    /// The pages beyond the first.
    pub fn more(&self) -> &[IssueFile] {
        &self.more
    }

    /// Where the next page starts; `None` when the history is all loaded.
    pub fn next_cursor(&self) -> Option<&str> {
        self.next_cursor.as_deref()
    }

    pub fn has_more(&self) -> bool {
        self.next_cursor.is_some()
    }

    /// Whether a "load more" fetch is running.
    pub fn fetching(&self) -> bool {
        self.fetching
    }

    /// Fold a read of page 1. See the type's docs for what happens to the
    /// pages beyond it.
    pub fn apply_first(&mut self, page: ClosedPage) {
        if !same_cards(&self.first, &page.issues) {
            self.more.clear();
            self.fetching = false;
            self.run += 1;
            self.depth = Depth::FirstPage;
        }
        self.first = page.issues;
        if self.depth != Depth::Paged {
            self.next_cursor = page.next_cursor;
            self.depth = Depth::FirstPage;
        }
    }

    /// Start a "load more" fetch. `None` when there's nothing to fetch, or one
    /// is already running.
    pub fn begin_more(&mut self) -> Option<MoreRequest> {
        if self.fetching {
            return None;
        }
        let cursor = self.next_cursor.clone()?;
        self.fetching = true;
        Some(MoreRequest {
            cursor,
            run: self.run,
        })
    }

    /// Fold a "load more" answer, keeping only cards not already on the board
    /// (`open`, or a loaded page). A failure keeps the cursor so the next try
    /// fetches the same page, and comes back for the client to show. An answer
    /// to a fetch a reset abandoned is dropped, failure included.
    pub fn finish_more(
        &mut self,
        request: &MoreRequest,
        result: Result<ClosedPage, String>,
        open: &[IssueFile],
    ) -> Result<(), String> {
        if request.run != self.run {
            return Ok(());
        }
        self.fetching = false;
        let page = result?;
        let mut seen: HashSet<String> = open
            .iter()
            .chain(&self.first)
            .chain(&self.more)
            .map(|tf| tf.task.id.clone())
            .collect();
        self.more
            .extend(page.issues.into_iter().filter(|tf| seen.insert(tf.task.id.clone())));
        self.next_cursor = page.next_cursor;
        self.depth = Depth::Paged;
        Ok(())
    }
}

/// Whether two pages hold the same cards, in any order.
fn same_cards(a: &[IssueFile], b: &[IssueFile]) -> bool {
    let ids = |page: &[IssueFile]| -> HashSet<String> {
        page.iter().map(|tf| tf.task.id.clone()).collect()
    };
    ids(a) == ids(b)
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
    /// The terminal history: its pages, and where the next one starts.
    pub closed: ClosedHistory,
    /// True until the first warm read (a cold index, or nothing read yet).
    pub loading: bool,
    /// The board freshness banner (`board 12s`, `board 4m stale …`).
    pub banner: Option<String>,
    /// The last read error, if the latest read failed.
    pub error: Option<String>,
    /// Whose each card is on a remote tracker (see [`BoardProject::ownership`]).
    pub ownership: OwnershipGate,
    /// See [`BoardProject::remote_search`].
    pub remote_search: bool,
    /// Issues the active text search found that the board hasn't loaded
    /// ([`crate::board_search::BoardSearch::hits`], set by
    /// [`BoardInput::set_search_hits`]); merged last, so a loaded card wins.
    pub search_hits: Vec<IssueFile>,
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
            closed: ClosedHistory::default(),
            loading: true,
            banner: None,
            error: None,
            ownership: OwnershipGate::Unrestricted,
            remote_search: false,
            search_hits: Vec::new(),
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
                self.ownership = p.ownership;
                self.remote_search = p.remote_search;
            }
            None => {
                self.default_workflow_name = DEFAULT_WORKFLOW_NAME.to_string();
                self.display_name = None;
                self.workspaces = Vec::new();
                self.ownership = OwnershipGate::Unrestricted;
                self.remote_search = false;
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
            self.closed.apply_first(page);
        }
    }

    /// Fold the answer to a "load more" fetch ([`ClosedHistory::begin_more`]).
    pub fn finish_more(
        &mut self,
        request: &MoreRequest,
        result: Result<ClosedPage, String>,
    ) -> Result<(), String> {
        self.closed.finish_more(request, result, &self.open)
    }

    /// The merged board (open first, then terminal history, then any search
    /// hits the board hasn't loaded).
    pub fn tasks(&self) -> Vec<IssueFile> {
        let mut tasks = merge_board(&self.open, self.closed.first(), self.closed.more());
        if !self.search_hits.is_empty() {
            let mut seen: HashSet<String> = tasks.iter().map(|tf| tf.task.id.clone()).collect();
            tasks.extend(
                self.search_hits
                    .iter()
                    .filter(|tf| seen.insert(tf.task.id.clone()))
                    .cloned(),
            );
        }
        tasks
    }

    /// Take the issues `search` found ([`crate::board_search::BoardSearch::hits`]),
    /// after the search changed.
    pub fn set_search_hits(&mut self, search: &crate::board_search::BoardSearch) {
        self.search_hits = search.hits().to_vec();
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
                pr: None,
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
    fn search_matches_every_word_in_title_id_or_branch_ignoring_case() {
        let m = |q| card_matches_search("Restyle the command palette", "t-42", Some("jlong/desktop-palette"), q);
        assert!(m(""));
        assert!(m("   "));
        assert!(m("PALETTE"));
        assert!(m("t-42"));
        assert!(m("jlong/desktop"));
        // Words may match different fields, but every word must match.
        assert!(m("command t-42"));
        assert!(!m("command scrollback"));
        assert!(!card_matches_search("Retry failed starts", "t-7", None, "jlong"));
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
    fn task_pr_prefers_the_record_and_falls_back_on_a_pull_request_url_param() {
        let parse = |extra: &str| {
            let text = format!(
                "---\nid: a\ntitle: A\ncolumn: review\npriority: 0\n{extra}\
                 created_at: 2026-10-01T10:00:00Z\nupdated_at: 2026-10-01T10:00:00Z\n---\nbody\n"
            );
            shelbi_state::parse_task_file(&text).unwrap().task
        };
        assert_eq!(task_pr(&parse("")), None);
        assert_eq!(
            task_pr(&parse("note: see https://example.com/docs\n")),
            None,
            "a non-PR URL is not a PR link"
        );
        let url = "https://github.com/jlong/shelbi/pull/42";
        // Under the record's own key, as a URL or a bare number (hand-written,
        // or from before the record existed)...
        assert_eq!(task_pr(&parse(&format!("pr: {url}\n"))), Some(IssuePr::new(42, Some(url.into()))));
        assert_eq!(task_pr(&parse("pr: 42\n")), Some(IssuePr::new(42, None)));
        // ...under any other param...
        assert_eq!(task_pr(&parse(&format!("pr_url: {url}\n"))), Some(IssuePr::new(42, Some(url.into()))));
        // ...and the record itself. Anything unreadable is no PR, not a broken card.
        let record = task_pr(&parse(&format!("pr:\n  number: 42\n  url: {url}\n"))).unwrap();
        assert_eq!(record, IssuePr::new(42, Some(url.into())));
        assert_eq!(task_pr(&parse("pr: [1, 2]\n")), None);
        assert_eq!(task_pr(&parse("pr: not-a-pr\n")), None);
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
        assert!(input.closed.landed());
        assert_eq!(input.tasks().len(), 2);
        input.apply_closed(None);
        assert_eq!(input.tasks().len(), 2, "a failed closed read keeps the last lanes");
        let request = input.closed.begin_more().expect("a next page");
        input
            .finish_more(
                &request,
                Ok(ClosedPage {
                    issues: vec![issue("d", "done"), issue("e", "done")],
                    next_cursor: None,
                }),
            )
            .unwrap();
        assert_eq!(input.tasks().len(), 3, "load more dedups against the board");
        assert!(!input.closed.has_more());
    }

    // ---- done-history paging ------------------------------------------------

    fn page(ids: &[&str], next: Option<&str>) -> ClosedPage {
        ClosedPage {
            issues: ids.iter().map(|id| issue(id, "done")).collect(),
            next_cursor: next.map(str::to_string),
        }
    }

    /// Fetch the next page, answered with `answer`; the cursor it asked for.
    fn load_more(input: &mut BoardInput, answer: ClosedPage) -> String {
        let request = input.closed.begin_more().expect("a next page");
        input.finish_more(&request, Ok(answer)).unwrap();
        request.cursor
    }

    fn history_ids(input: &BoardInput) -> Vec<String> {
        input.tasks().into_iter().map(|tf| tf.task.id).collect()
    }

    #[test]
    fn refreshes_never_rewind_the_cursor_past_a_loaded_page() {
        let mut input = BoardInput::default();
        let first = || page(&["a"], Some("p2"));
        input.apply_closed(Some(first()));

        assert_eq!(load_more(&mut input, page(&["b"], Some("p3"))), "p2");
        input.apply_closed(Some(first()));
        assert_eq!(load_more(&mut input, page(&["c"], Some("p4"))), "p3");
        input.apply_closed(Some(first()));
        assert_eq!(load_more(&mut input, page(&["d"], None)), "p4");
        input.apply_closed(Some(first()));

        assert_eq!(history_ids(&input), ["a", "b", "c", "d"]);
        assert!(!input.closed.has_more(), "a refresh doesn't bring back the end");
    }

    #[test]
    fn a_refresh_moves_the_cursor_while_only_page_one_is_loaded() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&["a"], Some("p2"))));
        input.apply_closed(Some(page(&["a"], Some("p2-again"))));
        assert_eq!(input.closed.next_cursor(), Some("p2-again"));
    }

    #[test]
    fn an_empty_first_page_still_counts_as_landed() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&[], None)));
        assert!(input.closed.landed());
    }

    #[test]
    fn a_page_of_only_known_cards_still_moves_the_cursor_for_good() {
        let mut input = BoardInput::default();
        let first = || page(&["a"], Some("p2"));
        input.apply_closed(Some(first()));
        load_more(&mut input, page(&["a"], Some("p3")));
        assert!(input.closed.more().is_empty(), "nothing new on that page");
        input.apply_closed(Some(first()));
        assert_eq!(input.closed.next_cursor(), Some("p3"), "and page 2 isn't fetched again");
    }

    #[test]
    fn a_refresh_that_changes_page_one_resets_the_pages_and_the_cursor_together() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&["a", "b"], Some("p2"))));
        load_more(&mut input, page(&["c"], Some("p3")));

        // A card closed: "b" fell off page 1, onto no loaded page.
        input.apply_closed(Some(page(&["new", "a"], Some("p2-new"))));

        assert!(input.closed.more().is_empty());
        assert_eq!(input.closed.next_cursor(), Some("p2-new"));
        assert_eq!(load_more(&mut input, page(&["b", "c"], None)), "p2-new");
        assert_eq!(history_ids(&input), ["new", "a", "b", "c"], "no hole");
    }

    #[test]
    fn page_one_changing_only_inside_keeps_the_loaded_pages() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&["a", "b"], Some("p2"))));
        load_more(&mut input, page(&["c"], Some("p3")));

        // The same cards, reordered (one was edited), with a new title.
        let mut edited = page(&["b", "a"], Some("p2-moved"));
        edited.issues[0].task.title = "edited".into();
        input.apply_closed(Some(edited));

        assert_eq!(input.closed.more().len(), 1);
        assert_eq!(input.closed.next_cursor(), Some("p3"));
        assert_eq!(input.closed.first()[0].task.title, "edited", "the read still lands");
    }

    #[test]
    fn one_fetch_runs_at_a_time_and_a_failure_keeps_the_cursor() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&["a"], Some("p2"))));
        let request = input.closed.begin_more().expect("a next page");
        assert!(input.closed.fetching());
        assert!(input.closed.begin_more().is_none(), "one at a time");

        assert_eq!(input.finish_more(&request, Err("boom".into())), Err("boom".to_string()));
        assert!(!input.closed.fetching());
        assert_eq!(input.closed.begin_more().expect("a retry").cursor, "p2");
    }

    #[test]
    fn a_reset_abandons_the_fetch_in_flight() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&["a"], Some("p2"))));
        let stale = input.closed.begin_more().expect("a next page");

        input.apply_closed(Some(page(&["new"], Some("p2-new"))));
        assert!(!input.closed.fetching(), "the next press fetches from the new page 1");
        let current = input.closed.begin_more().expect("a next page");
        assert_eq!(current.cursor, "p2-new");

        // The abandoned answer drops, whichever way it went.
        input.finish_more(&stale, Ok(page(&["old"], Some("p3-old")))).unwrap();
        input.finish_more(&stale, Err("boom".into())).unwrap();
        assert!(input.closed.more().is_empty());
        assert_eq!(input.closed.next_cursor(), Some("p2-new"));
        assert!(input.closed.fetching(), "and the current fetch is still running");

        input.finish_more(&current, Ok(page(&["b"], None))).unwrap();
        assert_eq!(history_ids(&input), ["new", "b"]);
    }

    #[test]
    fn a_refresh_of_the_same_cards_keeps_the_fetch_in_flight() {
        let mut input = BoardInput::default();
        input.apply_closed(Some(page(&["a"], Some("p2"))));
        let request = input.closed.begin_more().expect("a next page");
        input.apply_closed(Some(page(&["a"], Some("p2"))));
        assert!(input.closed.fetching());
        input.finish_more(&request, Ok(page(&["b"], Some("p3")))).unwrap();
        assert_eq!(input.closed.next_cursor(), Some("p3"));
    }

    // ---- drag and drop ------------------------------------------------------

    /// A card on `workflow` (`None` = the project default) in `status`.
    fn card_on(id: &str, status: &str, workflow: Option<&str>) -> IssueFile {
        let mut tf = issue(id, status);
        tf.task.workflow = workflow.map(str::to_string);
        tf
    }

    #[test]
    fn drop_targets_follow_the_shipped_task_workflow() {
        use StatusCategory::*;
        let workflows = vec![shelbi_core::task_workflow(), shelbi_core::subtask_workflow()];
        let rules = BoardRules {
            workflows: &workflows,
            default_workflow_name: shelbi_core::TASK_WORKFLOW_NAME,
        };
        let card = card_on("t", "in-progress", None);
        // Any declared status is a legal move (transitions are any-to-any).
        assert_eq!(
            drop_target(rules, &card.task, "todo", Ready),
            DropTarget::Move { merge: false }
        );
        assert_eq!(
            drop_target(rules, &card.task, "review", Handoff),
            DropTarget::Move { merge: false }
        );
        // `in-progress -> done` declares no merge in `task` (review gates it).
        assert_eq!(
            drop_target(rules, &card.task, "done", Done),
            DropTarget::Move { merge: false }
        );
        // `review -> done` runs the gated merge.
        let in_review = card_on("r", "review", None);
        assert_eq!(
            drop_target(rules, &in_review.task, "done", Done),
            DropTarget::Move { merge: true }
        );
        // A column no workflow declares is refused.
        assert!(matches!(
            drop_target(rules, &card.task, "qa", Handoff),
            DropTarget::Refused(msg) if msg.contains("`task` workflow")
        ));
        // Its own column is a reorder.
        assert_eq!(
            drop_target(rules, &card.task, "in-progress", Active),
            DropTarget::Reorder
        );
    }

    #[test]
    fn drop_targets_follow_the_shipped_subtask_workflow() {
        use StatusCategory::*;
        let workflows = vec![shelbi_core::task_workflow(), shelbi_core::subtask_workflow()];
        let rules = BoardRules {
            workflows: &workflows,
            default_workflow_name: shelbi_core::TASK_WORKFLOW_NAME,
        };
        let sub = card_on("s", "in-progress", Some(shelbi_core::SUBTASK_WORKFLOW_NAME));
        // `subtask` has no review status: the REVIEW column refuses it.
        assert!(matches!(
            drop_target(rules, &sub.task, "review", Handoff),
            DropTarget::Refused(msg) if msg.contains("`subtask` workflow")
        ));
        // ...and merges straight from in-progress to done.
        assert_eq!(
            drop_target(rules, &sub.task, "done", Done),
            DropTarget::Move { merge: true }
        );
        assert_eq!(
            drop_target(rules, &sub.task, "backlog", Backlog),
            DropTarget::Move { merge: false }
        );
    }

    #[test]
    fn drop_target_refuses_an_unloaded_workflow_and_a_done_reorder() {
        use StatusCategory::*;
        let workflows = vec![shelbi_core::task_workflow()];
        let rules = BoardRules {
            workflows: &workflows,
            default_workflow_name: shelbi_core::TASK_WORKFLOW_NAME,
        };
        let ghost = card_on("g", "todo", Some("ghost"));
        assert!(matches!(
            drop_target(rules, &ghost.task, "done", Done),
            DropTarget::Refused(msg) if msg.contains("`ghost`")
        ));
        let done = card_on("d", "done", None);
        assert!(matches!(
            drop_target(rules, &done.task, "done", Done),
            DropTarget::Refused(_)
        ));
    }

    #[test]
    fn reorder_slot_indexes_the_whole_stored_column_in_priority_order() {
        let mut tasks: Vec<IssueFile> = ["a", "b", "c", "d"]
            .iter()
            .enumerate()
            .map(|(i, id)| {
                let mut tf = issue(id, "todo");
                tf.task.priority = i as u32;
                tf
            })
            .collect();
        tasks.push(issue("x", "backlog"));
        // Listed out of order: the slot comes from priority, not list order.
        tasks.reverse();
        assert_eq!(reorder_slot(&tasks, "d", Some("b")), Some(1));
        assert_eq!(reorder_slot(&tasks, "a", Some("c")), Some(2));
        assert_eq!(reorder_slot(&tasks, "a", None), Some(3), "None is the bottom");
        assert_eq!(current_slot(&tasks, "c"), Some(2));
        // A target in another stored column has no slot.
        assert_eq!(reorder_slot(&tasks, "a", Some("x")), None);
        assert_eq!(reorder_slot(&tasks, "nope", None), None);
        // Equal priorities break ties by id, like the stores.
        for tf in tasks.iter_mut() {
            tf.task.priority = 0;
        }
        assert_eq!(current_slot(&tasks, "b"), Some(1));
    }

    #[test]
    fn card_summary_is_the_first_prose_paragraph_as_plain_text() {
        assert_eq!(
            card_summary("# Desktop application\n\nBuild a **native** client\nwith `gpui`.\n\nSecond paragraph."),
            "Build a native client with gpui."
        );
        assert_eq!(
            card_summary("<!-- note -->\n## Problem\n\nSee [the docs](https://x.y/z) and ![a shot](s.png).\n"),
            "See the docs and a shot."
        );
        assert_eq!(card_summary("- [ ] first item\n- [x] second\n\nlater"), "first item second");
        assert_eq!(card_summary("> quoted\n> line"), "quoted line");
        assert_eq!(card_summary("```sh\ncargo test\n```\n\nAfter the fence."), "After the fence.");
        assert_eq!(card_summary("---\n\n1. Numbered step"), "Numbered step");
        assert_eq!(card_summary("an [unclosed bracket"), "an [unclosed bracket");
        assert_eq!(card_summary("## Only a heading\n\n"), "");
        assert_eq!(card_summary(""), "");
        let long = "word ".repeat(100);
        let summary = card_summary(&long);
        assert!(summary.ends_with('…'), "{summary}");
        assert!(summary.chars().count() <= SUMMARY_MAX_CHARS + 1);
    }
}
