//! View models: plain data for each UI surface, built from the existing
//! `shelbi-state` readers.
//!
//! Every type here is plain, `Clone`, `Send` data — no toolkit types, no
//! IO. A renderer (the TUI today, the desktop app later) draws these; the
//! [`refresh`](crate::refresh) worker is what calls the readers and feeds
//! their output into these builders off the UI thread.
//!
//! The builders are pure functions over already-read data (an
//! `[IssueFile]` board slice, a `[Machine]` list, `[(ErrorLogEntry, bool)]`
//! rows). That is deliberate: it mirrors how the TUI derives its sidebar,
//! kanban, and activity views by filtering one freshly-read board, and it
//! keeps the mapping unit-testable without a `$SHELBI_HOME` or a `gh`.

use std::collections::{BTreeSet, HashMap};

use shelbi_core::{Column, Machine, MachineKind, StatusCategory, WorkspaceSpec};
use shelbi_palette::{Decoration, DecorationColor};
use shelbi_state::{ErrorLogEntry, IssueFile, ZenModeState};

use crate::board::{self, BoardInput, BoardRules, WorkspaceFilter};
use crate::nav::View;

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

/// The left navigation list.
///
/// Everything a renderer needs to draw the sidebar at parity with the former
/// tmux-runtime sidebar: the fixed nav, the machine-grouped workspace pool
/// (each row carrying its state badge), the review sections (split by
/// lifecycle state), and the footer chrome (zen state, version segment, status
/// line, unread-errors count). The board-derived parts are pure functions over
/// an already-read board slice; the disk-derived parts (badges, review
/// serving-markers, version probe) are filled by the host that owns IO.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarModel {
    pub project_label: String,
    pub nav: Vec<NavItem>,
    pub workspaces: Vec<WorkspaceRow>,
    /// Tasks sitting in the review column. The renderer partitions these into
    /// the "Ready for Review" (`Serving` / `Loading`) and "Queued for Review"
    /// (`Pending`) sections by each row's [`ReviewRow::state`].
    pub reviews: Vec<ReviewRow>,
    /// The project config load error (present-but-broken config), surfaced
    /// inline under the Workspaces header instead of silently dropping the
    /// section. `None` for a healthy or genuinely-absent config.
    pub config_error: Option<String>,
    /// True on a cold process whose board hasn't loaded yet — drives the dim
    /// "Loading…" placeholder so the chrome paints instantly.
    pub board_loading: bool,
    /// Machines the user has collapsed in the Workspaces tree (their workspace
    /// rows are hidden, the group header shows a count suffix).
    pub collapsed_machines: BTreeSet<String>,
    /// Footer freshness banner (shares the version row): `None` for a local or
    /// not-yet-probed board.
    pub board_banner: Option<String>,
    /// Preformatted daemon/CLI version footer segment. `None` until probed.
    pub daemon_version_line: Option<String>,
    /// True when the probed daemon version differs from the running binary —
    /// the renderer paints the version segment red instead of dim.
    pub daemon_version_mismatch: bool,
    /// Launch-time sidebar status line (the first-run hint or a startup-warning
    /// count). Empty when there is nothing to surface.
    pub status_line: String,
    /// Latest Zen Mode state — drives the footer's green band (On) vs the
    /// hotkey hint (Off / Paused).
    pub zen_mode: ZenModeState,
    pub unread_errors: usize,
    /// Set when the hub daemon has recorded that it lost its macOS GUI login
    /// session (the user logged out/in while it kept running). Carries the
    /// user-facing banner text; the shell paints it as a persistent top banner
    /// in every view. `None` on a healthy session.
    pub session_lost: Option<String>,
}

/// A fixed nav builtin (Chat / Issues / Activity).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavItem {
    pub label: String,
    pub view: View,
}

/// A dev workspace row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRow {
    pub name: String,
    /// The machine this workspace lives on — used to group rows under a
    /// `▾ <machine>` header when the project declares more than one.
    pub machine: String,
    /// Whether `machine` is a remote (SSH) host.
    pub is_remote: bool,
    /// The task id this workspace is currently working, if any.
    pub current_task: Option<String>,
    /// The agent name (from the task's `agent:` frontmatter), if known.
    pub agent: Option<String>,
    /// Per-workspace state glyph shown in the badge column.
    pub badge: WorkspaceBadge,
}

/// A review-column task row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRow {
    pub task_id: String,
    pub title: String,
    /// Branch name shown dim on the entry's second line.
    pub branch: String,
    /// `machine:port` URL badge — `Some` only for a `Serving` task.
    pub location: Option<String>,
    /// The `review`-tagged workspace this task is loaded onto, when assigned.
    pub workspace: Option<String>,
    /// Which lifecycle state drives the row's section and glyph.
    pub state: ReviewState,
}

/// Per-workspace state glyph shown in the sidebar's badge column. The plain
/// data twin of the former tmux-runtime `WorkspaceBadge`; the glyph + colour
/// are the single source both the sidebar renderer and the palette consume so
/// the two surfaces can't drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceBadge {
    /// ⏵ — the agent is actively running a turn.
    Working,
    /// ? — finished a turn and sitting at the prompt.
    AwaitingInput,
    /// ⚠ — a permission dialog is up.
    AwaitingPermission,
    /// ⏸ — the runner stalled on a usage/session limit.
    Paused,
    /// · — no in-flight task assigned.
    Idle,
    /// … — state not read yet: the row comes from the project's config (a
    /// cold project switch paints its workspace rows before the first board /
    /// status read lands), and the real badge replaces it within ~one refresh.
    Loading,
}

impl WorkspaceBadge {
    /// Single-char glyph — paired with one trailing space in the renderer.
    pub fn glyph(self) -> &'static str {
        match self {
            WorkspaceBadge::Working => "⏵",
            WorkspaceBadge::AwaitingInput => "?",
            WorkspaceBadge::AwaitingPermission => "⚠",
            WorkspaceBadge::Paused => "⏸",
            WorkspaceBadge::Idle => "·",
            WorkspaceBadge::Loading => "…",
        }
    }

    /// Colour the glyph paints in.
    pub fn decoration_color(self) -> DecorationColor {
        match self {
            WorkspaceBadge::Working => DecorationColor::Green,
            WorkspaceBadge::AwaitingInput => DecorationColor::Yellow,
            WorkspaceBadge::AwaitingPermission => DecorationColor::Red,
            WorkspaceBadge::Paused => DecorationColor::Yellow,
            WorkspaceBadge::Idle | WorkspaceBadge::Loading => DecorationColor::DarkGray,
        }
    }

    pub fn decoration(self) -> Decoration {
        Decoration {
            glyph: self.glyph().to_string(),
            color: self.decoration_color(),
        }
    }
}

/// Which of the three review lifecycle states a [`ReviewRow`] is in. `Serving`
/// and `Loading` both read "Ready for Review" (the slot is already assigned —
/// serving shows a ✓, loading a ▶ with no ✓ yet); `Pending` reads "Queued for
/// Review" (no slot yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewState {
    /// The review slot's dev server is confirmed up. Ready for Review, ✓, with
    /// the `machine:port` location.
    Serving,
    /// Assigned to a review slot but the server isn't confirmed serving yet.
    /// Ready for Review, ▶, no ✓ and no location yet.
    Loading,
    /// In Review but not yet assigned to any review slot. Queued for Review, ·.
    Pending,
}

impl ReviewState {
    /// Glyph + colour for the entry's badge — the single source shared with the
    /// palette so the ✓ is gated on serving, never on section membership.
    pub fn decoration(self) -> Decoration {
        match self {
            ReviewState::Serving => Decoration {
                glyph: "✓".into(),
                color: DecorationColor::Cyan,
            },
            ReviewState::Loading => Decoration {
                glyph: "▶".into(),
                // Muted `#7a7a7a`, not yellow: a still-starting review is quiet
                // chrome, not an alert. A loading glyph only shows while the
                // review session is actually coming up.
                color: DecorationColor::Muted,
            },
            ReviewState::Pending => Decoration {
                glyph: "·".into(),
                color: DecorationColor::DarkGray,
            },
        }
    }
}

impl SidebarModel {
    /// Build the sidebar from one already-read board slice, mirroring the
    /// TUI's approach of filtering the single board rather than making a
    /// second round trip.
    ///
    /// `workspace_names` is the project's declared workspace pool; each
    /// row's `current_task` / `agent` is derived from any board task
    /// assigned to that workspace and sitting in an active (non-terminal,
    /// non-review) column.
    pub fn from_board(
        project_label: impl Into<String>,
        board: &[IssueFile],
        workspace_names: &[String],
        zen_on: bool,
        unread_errors: usize,
    ) -> SidebarModel {
        let review = Column::review();
        let reviews = board
            .iter()
            .filter(|f| f.task.column == review)
            .map(|f| ReviewRow {
                task_id: f.task.id.clone(),
                title: f.task.title.clone(),
                // Structural builder: no project/orchestrator context, so the
                // branch falls back to the task's recorded branch (or the
                // `user/<id>` default) and every row reads Queued/Pending with
                // no serving location. A host with IO overrides these.
                branch: f
                    .task
                    .branch
                    .clone()
                    .unwrap_or_else(|| format!("user/{}", f.task.id)),
                location: None,
                workspace: None,
                state: ReviewState::Pending,
            })
            .collect();

        let workspaces = workspace_names
            .iter()
            .map(|name| {
                let task = board.iter().find(|f| {
                    f.task.assigned_to.as_deref() == Some(name.as_str())
                        && f.task.column.category() == StatusCategory::Active
                });
                let current_task = task.map(|f| f.task.id.clone());
                // Without a `status.yaml` read the badge is a structural guess:
                // a workspace with an active task reads Working, otherwise Idle.
                let badge = if current_task.is_some() {
                    WorkspaceBadge::Working
                } else {
                    WorkspaceBadge::Idle
                };
                WorkspaceRow {
                    name: name.clone(),
                    machine: String::new(),
                    is_remote: false,
                    current_task,
                    agent: task.and_then(|f| f.task.param_str("agent").map(str::to_string)),
                    badge,
                }
            })
            .collect();

        SidebarModel {
            project_label: project_label.into(),
            nav: vec![
                NavItem {
                    // The orchestrator chat nav item; see the sidebar builder.
                    label: "Orchestrator".into(),
                    view: View::Session("orch".into()),
                },
                NavItem {
                    label: "Issues".into(),
                    view: View::Issues,
                },
                NavItem {
                    label: "Activity".into(),
                    view: View::Activity,
                },
            ],
            workspaces,
            reviews,
            config_error: None,
            board_loading: false,
            collapsed_machines: BTreeSet::new(),
            board_banner: None,
            daemon_version_line: None,
            daemon_version_mismatch: false,
            status_line: String::new(),
            zen_mode: if zen_on {
                ZenModeState::On
            } else {
                ZenModeState::Off
            },
            unread_errors,
            // The structural builder is fed pre-read board data and does no disk
            // IO of its own; the richer disk builder reads the hub-global marker
            // and sets this (`sidebar_model::read_sidebar_model`).
            session_lost: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Issues board
// ---------------------------------------------------------------------------

/// The kanban board, ready to draw: the visible columns (each with its cards
/// in board order), plus the board-level chrome. Built by
/// [`IssuesModel::build`] from a [`BoardInput`] and the active
/// [`BoardFilters`]; every decision in it (which columns, which column a card
/// sits in, card order, what a card shows) comes from [`crate::board`], so the
/// TUI and the desktop board agree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IssuesModel {
    pub columns: Vec<IssueColumn>,
    /// The board label: the project's `display_name`, else its slug.
    pub title: String,
    /// True while no warm read has landed (a cold index): paint the columns
    /// with a loading state instead of empty lanes that look real.
    pub loading: bool,
    /// The board freshness banner, when the index lags or is cached.
    pub banner: Option<String>,
    /// The last read's error, for the status line.
    pub error: Option<String>,
    /// True until the first terminal-history page lands: the done column is
    /// still filling (the open board never waits on it).
    pub done_loading: bool,
    /// More terminal history is available behind "load more".
    pub has_more_done: bool,
    /// Whether cards carry their workflow-name badge (more than one workflow
    /// loaded and no workflow filter).
    pub show_workflow_badges: bool,
    /// The active filters' chip labels.
    pub workflow_filter_label: String,
    pub workspace_filter_label: String,
}

/// A column definition: one status from `workflows/statuses.yaml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColumnDef {
    pub status_id: String,
    pub status_name: String,
    pub category: StatusCategory,
}

/// A rendered column with its cards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueColumn {
    pub status_id: String,
    pub status_name: String,
    /// The header label (`IN PROGRESS`), see [`crate::board::column_label`].
    pub label: String,
    pub category: StatusCategory,
    /// Renders as the narrow collapsed strip (empty, or the user collapsed it).
    pub collapsed: bool,
    pub cards: Vec<IssueCard>,
}

/// A card on the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCard {
    pub id: String,
    pub title: String,
    /// True when a dependency is not yet done (renders the lock badge).
    pub blocked: bool,
    /// The meta-line branch: the task's own branch, or a resolved
    /// `base_branch` template ([`crate::board::card_branch`]).
    pub branch: Option<String>,
    /// The task's own `workflow:` field, as written.
    pub workflow: Option<String>,
    /// The workflow the card belongs to (its own, else the project default).
    pub workflow_name: String,
    /// The workspace the card is assigned to (`assigned_to`, no `@`).
    pub assignee: Option<String>,
    pub priority: u32,
    /// Dependency badges, in `depends_on` order.
    pub depends_on: Vec<DepBadge>,
    /// The column the card buckets under (its status resolved against its
    /// workflow).
    pub status_id: String,
    /// The status as stored (`Issue::column`) and `updated_at` as RFC3339:
    /// the state this card was read at, which a mutation carries so the daemon
    /// refuses it if another client changed the issue first.
    pub stored_status: String,
    pub updated_at: String,
}

/// One `depends_on` entry and where that dependency stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepBadge {
    pub id: String,
    /// The dependency's status id, or `None` when it isn't on the board.
    pub status: Option<String>,
    pub done: bool,
}

/// The view-only filters a client applies over the board. The workflow
/// filter is per-session; the workspace filter is persisted in `state.json`
/// (the client reads it back through [`BoardInput::workspace_filter`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardFilters {
    pub workflow: Option<String>,
    pub workspace: Option<WorkspaceFilter>,
}

impl IssueCard {
    /// The expected state a mutation of this card carries.
    pub fn expected(&self) -> shelbi_proto::control::ExpectedState {
        shelbi_proto::control::ExpectedState {
            status: self.stored_status.clone(),
            updated_at: self.updated_at.clone(),
        }
    }
}

impl IssuesModel {
    /// Render a board: every visible column (from `statuses.yaml`, narrowed by
    /// the workflow filter) with the cards that bucket under it, filtered by
    /// workspace and workflow, in board order (priority, or newest-first in a
    /// done column).
    pub fn build(input: &BoardInput, filters: &BoardFilters) -> IssuesModel {
        let rules = input.rules();
        let tasks = input.tasks();
        let by_id: HashMap<String, Column> = tasks
            .iter()
            .map(|f| (f.task.id.clone(), f.task.column.clone()))
            .collect();
        let workflow_filter = filters.workflow.as_deref();
        let resolved: Vec<String> = tasks
            .iter()
            .map(|tf| rules.resolved_status_id(&tf.task))
            .collect();
        let columns = input
            .columns(workflow_filter)
            .into_iter()
            .map(|def| {
                let mut members: Vec<&IssueFile> = tasks
                    .iter()
                    .zip(&resolved)
                    .filter(|(tf, status)| {
                        status.as_str() == def.status_id
                            && filters
                                .workspace
                                .as_ref()
                                .is_none_or(|f| f.matches(tf.task.assigned_to.as_deref()))
                            && workflow_filter.is_none_or(|w| rules.workflow_name(&tf.task) == w)
                    })
                    .map(|(tf, _)| tf)
                    .collect();
                board::sort_column_cards(def.category, &mut members);
                let cards: Vec<IssueCard> = members
                    .into_iter()
                    .map(|tf| card_for(tf, &by_id, &rules, &def.status_id))
                    .collect();
                let collapsed = input
                    .expansion(workflow_filter, &def.status_id)
                    .is_collapsed(cards.len());
                IssueColumn {
                    label: board::column_label(&def.status_name),
                    status_id: def.status_id,
                    status_name: def.status_name,
                    category: def.category,
                    collapsed,
                    cards,
                }
            })
            .collect();
        IssuesModel {
            columns,
            title: input.display_name.clone().unwrap_or_default(),
            loading: input.loading,
            banner: input.banner.clone(),
            error: input.error.clone(),
            done_loading: !input.closed_loaded,
            has_more_done: input.closed_next_cursor.is_some(),
            show_workflow_badges: board::show_card_workflow_label(
                input.workflows.len(),
                filters.workflow.is_some(),
            ),
            workflow_filter_label: filters
                .workflow
                .clone()
                .unwrap_or_else(|| "All workflows".to_string()),
            workspace_filter_label: filters
                .workspace
                .as_ref()
                .map(WorkspaceFilter::label)
                .unwrap_or_else(|| "All workspaces".to_string()),
        }
    }

    /// The card with `id`, wherever it sits.
    pub fn card(&self, id: &str) -> Option<&IssueCard> {
        self.columns
            .iter()
            .flat_map(|c| c.cards.iter())
            .find(|c| c.id == id)
    }

    /// `(column, row)` of the card with `id`.
    pub fn position(&self, id: &str) -> Option<(usize, usize)> {
        self.columns.iter().enumerate().find_map(|(ci, col)| {
            col.cards
                .iter()
                .position(|c| c.id == id)
                .map(|ri| (ci, ri))
        })
    }
}

/// The task detail: the issue's frontmatter fields and its markdown body, as
/// the board's detail view (the TUI's popover, the desktop's task detail)
/// shows them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDetailModel {
    pub id: String,
    pub title: String,
    /// The stored status id and its display label (`IN PROGRESS`).
    pub status_id: String,
    pub status_label: String,
    pub category: StatusCategory,
    pub priority: u32,
    pub assignee: Option<String>,
    pub branch: Option<String>,
    pub workflow_name: String,
    pub depends_on: Vec<DepBadge>,
    pub blocked: bool,
    /// A recorded pull-request link ([`crate::board::pr_link`]).
    pub pr_link: Option<String>,
    /// `YYYY-MM-DD HH:MM UTC`.
    pub created: String,
    pub updated: String,
    /// The markdown body.
    pub body: String,
}

impl TaskDetailModel {
    /// The detail of issue `id` on `input`'s board, or `None` when it is no
    /// longer there.
    pub fn build(input: &BoardInput, id: &str) -> Option<TaskDetailModel> {
        let tasks = input.tasks();
        let tf = tasks.iter().find(|tf| tf.task.id == id)?;
        let by_id: HashMap<String, Column> = tasks
            .iter()
            .map(|f| (f.task.id.clone(), f.task.column.clone()))
            .collect();
        let rules = input.rules();
        let status_id = rules.resolved_status_id(&tf.task);
        let card = card_for(tf, &by_id, &rules, &status_id);
        let status = input
            .project_statuses
            .statuses
            .iter()
            .find(|st| st.id == status_id);
        let task = &tf.task;
        Some(TaskDetailModel {
            id: task.id.clone(),
            title: task.title.clone(),
            status_label: board::column_label(
                status.map(|st| st.name.as_str()).unwrap_or(task.column.default_status_name()),
            ),
            category: status.map(|st| st.category).unwrap_or(task.column.category()),
            status_id,
            priority: task.priority,
            assignee: card.assignee,
            branch: card.branch,
            workflow_name: card.workflow_name,
            depends_on: card.depends_on,
            blocked: card.blocked,
            pr_link: board::pr_link(task),
            created: task.created_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            updated: task.updated_at.format("%Y-%m-%d %H:%M UTC").to_string(),
            body: tf.body.clone(),
        })
    }
}

fn card_for(
    tf: &IssueFile,
    by_id: &HashMap<String, Column>,
    rules: &BoardRules<'_>,
    status_id: &str,
) -> IssueCard {
    let task = &tf.task;
    let workflow_name = rules.workflow_name(task).to_string();
    IssueCard {
        id: task.id.clone(),
        title: task.title.clone(),
        blocked: task.is_blocked(by_id),
        branch: board::card_branch(task, rules.workflows, &workflow_name),
        workflow: task.workflow.clone(),
        workflow_name,
        assignee: task.assigned_to.clone(),
        priority: task.priority,
        depends_on: task
            .depends_on
            .iter()
            .map(|dep| {
                let status = by_id.get(dep);
                DepBadge {
                    id: dep.clone(),
                    status: status.map(|c| c.as_str().to_string()),
                    done: status.is_some_and(|c| c.category() == StatusCategory::Done),
                }
            })
            .collect(),
        status_id: status_id.to_string(),
        stored_status: task.column.as_str().to_string(),
        updated_at: task.updated_at.to_rfc3339(),
    }
}

// ---------------------------------------------------------------------------
// Activity feed
// ---------------------------------------------------------------------------

/// The activity feed: a reverse-chronological list of events.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ActivityModel {
    pub items: Vec<ActivityItem>,
}

/// One feed row. The host reads and parses `events.log`; this is the plain
/// shape the feed renders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityItem {
    /// RFC3339 timestamp string (as written in the log).
    pub timestamp: String,
    /// The acting agent/identity, if the line names one.
    pub agent: Option<String>,
    /// A short human summary of what happened.
    pub summary: String,
    pub kind: ActivityKind,
}

/// Coarse classification used by the feed's pill filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityKind {
    /// An issue status transition.
    Issue,
    /// A workspace state change.
    Workspace,
    /// A Zen-mode dry-run / promotion event.
    Zen,
    /// Anything else (system lines, ssh, merges…).
    Other,
}

/// The feed's pill filter (All / Zen / Workspaces). `All` shows everything
/// except heartbeats (which never reach the model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ActivityFilter {
    pub zen_only: bool,
    pub workspaces_only: bool,
}

impl ActivityModel {
    /// Wrap a pre-parsed, newest-first item list.
    pub fn from_items(items: Vec<ActivityItem>) -> ActivityModel {
        ActivityModel { items }
    }

    /// The items passing `filter`. `zen_only` keeps Zen events;
    /// `workspaces_only` keeps workspace events; both unset keeps all.
    pub fn filtered(&self, filter: ActivityFilter) -> Vec<&ActivityItem> {
        self.items
            .iter()
            .filter(|it| match (filter.zen_only, filter.workspaces_only) {
                (false, false) => true,
                (true, false) => it.kind == ActivityKind::Zen,
                (false, true) => it.kind == ActivityKind::Workspace,
                // Both pills active: either category.
                (true, true) => {
                    matches!(it.kind, ActivityKind::Zen | ActivityKind::Workspace)
                }
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------
// Machines
// ---------------------------------------------------------------------------

/// The machines list (a new in-process view).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachinesModel {
    pub machines: Vec<MachineRow>,
}

/// A machine row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub name: String,
    pub kind: MachineKind,
    /// SSH host, when this is a remote machine.
    pub host: Option<String>,
    pub is_local: bool,
    pub tags: Vec<String>,
    /// How many of the project's workspaces live on this machine.
    pub workspace_count: usize,
    /// How many of those are currently active (host-supplied; 0 when
    /// unknown).
    pub active_count: usize,
}

impl MachinesModel {
    /// Build from the project's declared machines and workspaces.
    /// `active_by_machine` maps a machine name to its count of currently
    /// active workspaces (the host derives this from workspace statuses);
    /// a machine absent from the map reports zero active.
    pub fn from_project(
        machines: &[Machine],
        workspaces: &[WorkspaceSpec],
        active_by_machine: &HashMap<String, usize>,
    ) -> MachinesModel {
        let machines = machines
            .iter()
            .map(|m| {
                let workspace_count = workspaces.iter().filter(|w| w.machine == m.name).count();
                MachineRow {
                    name: m.name.clone(),
                    kind: m.kind,
                    host: m.host.clone(),
                    is_local: m.kind == MachineKind::Local,
                    tags: m.tags.clone(),
                    workspace_count,
                    active_count: active_by_machine.get(&m.name).copied().unwrap_or(0),
                }
            })
            .collect();
        MachinesModel { machines }
    }
}

// ---------------------------------------------------------------------------
// Review panel
// ---------------------------------------------------------------------------

/// The review interface's left panel for one ready-for-review task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewPanelModel {
    pub task_id: String,
    pub title: String,
    pub worktree: String,
    pub status_label: String,
    pub editor_name: String,
    pub has_review_url: bool,
    /// The view-switcher nav items, in order.
    pub switch_items: Vec<ReviewSwitch>,
}

/// A review view-switcher entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewSwitch {
    /// Chat with the reviewer agent (default).
    Chat,
    /// The diff view.
    Diff,
    /// Open the worktree in the user's editor.
    Editor,
    /// Open the PR / review URL in a browser (only when one exists).
    Browser,
}

impl ReviewPanelModel {
    /// Build the panel for a ready-for-review task. `editor_name` is the
    /// resolved editor display name; the Browser switch is included only
    /// when `has_review_url`.
    pub fn new(
        task_id: impl Into<String>,
        title: impl Into<String>,
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        has_review_url: bool,
    ) -> ReviewPanelModel {
        let mut switch_items = vec![ReviewSwitch::Chat, ReviewSwitch::Diff, ReviewSwitch::Editor];
        if has_review_url {
            switch_items.push(ReviewSwitch::Browser);
        }
        ReviewPanelModel {
            task_id: task_id.into(),
            title: title.into(),
            worktree: worktree.into(),
            status_label: "Ready for review".to_string(),
            editor_name: editor_name.into(),
            has_review_url,
            switch_items,
        }
    }
}

// ---------------------------------------------------------------------------
// Error log
// ---------------------------------------------------------------------------

/// The per-project error log.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ErrorLogModel {
    pub entries: Vec<ErrorRow>,
    /// The count of entries that are unread (relative to the read marker).
    pub unread: usize,
}

/// One error-log row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorRow {
    pub timestamp: String,
    pub message: String,
    pub source: Option<String>,
    pub unread: bool,
}

impl ErrorLogModel {
    /// Build from [`shelbi_state::read_errors_with_unread`] output — a
    /// list of `(entry, is_unread)`, newest last on disk.
    pub fn from_entries(rows: &[(ErrorLogEntry, bool)]) -> ErrorLogModel {
        let entries: Vec<ErrorRow> = rows
            .iter()
            .map(|(e, unread)| ErrorRow {
                timestamp: e.ts.clone(),
                message: e.message.clone(),
                source: e.source.clone(),
                unread: *unread,
            })
            .collect();
        let unread = entries.iter().filter(|r| r.unread).count();
        ErrorLogModel { entries, unread }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::Issue;
    use std::collections::BTreeMap;

    fn issue(id: &str, column: &str) -> IssueFile {
        issue_full(id, column, &[], None)
    }

    fn issue_full(id: &str, column: &str, depends_on: &[&str], assigned: Option<&str>) -> IssueFile {
        let now = chrono::Utc::now();
        let task = Issue {
            id: id.to_string(),
            title: format!("Task {id}"),
            column: Column::from_status_id(column),
            priority: 0,
            assigned_to: assigned.map(str::to_string),
            workflow: None,
            branch: None,
            depends_on: depends_on.iter().map(|s| s.to_string()).collect(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        };
        IssueFile {
            task,
            body: String::new(),
            tracker_assignees: Vec::new(),
        }
    }

    #[test]
    fn sidebar_splits_reviews_and_maps_workspaces() {
        let board = vec![
            issue_full("T-1", "in-progress", &[], Some("alpha-1")),
            issue("T-2", "review"),
            issue("T-3", "todo"),
        ];
        let model = SidebarModel::from_board(
            "Alpha",
            &board,
            &["alpha-1".to_string(), "alpha-2".to_string()],
            true,
            4,
        );
        assert_eq!(model.project_label, "Alpha");
        assert_eq!(model.zen_mode, ZenModeState::On);
        assert_eq!(model.unread_errors, 4);
        // Machines moved to the command palette; the nav is Chat / Issues /
        // Activity only, matching main's sidebar.
        assert_eq!(model.nav.len(), 3);
        assert_eq!(model.nav[1].view, View::Issues);
        assert_eq!(model.nav[2].view, View::Activity);
        assert!(
            !model.nav.iter().any(|n| n.view == View::Machines),
            "Machines is no longer a sidebar nav entry"
        );

        // One review task.
        assert_eq!(model.reviews.len(), 1);
        assert_eq!(model.reviews[0].task_id, "T-2");

        // alpha-1 is working T-1; alpha-2 is idle.
        let a1 = model.workspaces.iter().find(|w| w.name == "alpha-1").unwrap();
        assert_eq!(a1.current_task.as_deref(), Some("T-1"));
        let a2 = model.workspaces.iter().find(|w| w.name == "alpha-2").unwrap();
        assert_eq!(a2.current_task, None);
    }

    #[test]
    fn issues_board_buckets_by_status_and_flags_blocked() {
        let board = vec![
            issue("T-1", "todo"),
            issue_full("T-2", "todo", &["T-3"], None), // dep not done -> blocked
            issue("T-3", "in-progress"),
            issue_full("T-4", "todo", &["T-5"], None), // dep done -> not blocked
            issue("T-5", "done"),
        ];
        let input = board_input(board);
        let model = IssuesModel::build(&input, &BoardFilters::default());
        // The canonical statuses.yaml columns, in declared order.
        let ids: Vec<&str> = model.columns.iter().map(|c| c.status_id.as_str()).collect();
        assert_eq!(ids, ["backlog", "todo", "in-progress", "review", "done", "canceled"]);
        let todo = &model.columns[1];
        assert_eq!(todo.label, "TO DO");
        assert_eq!(todo.cards.len(), 3);
        let t2 = todo.cards.iter().find(|c| c.id == "T-2").unwrap();
        assert!(t2.blocked, "T-2 depends on an unfinished task");
        assert_eq!(t2.depends_on[0].status.as_deref(), Some("in-progress"));
        assert!(!t2.depends_on[0].done);
        let t4 = todo.cards.iter().find(|c| c.id == "T-4").unwrap();
        assert!(!t4.blocked, "T-4's dependency is done");
        assert!(t4.depends_on[0].done);
        assert_eq!(model.columns[4].category, StatusCategory::Done);
        // Empty columns auto-collapse; non-empty ones expand.
        assert!(model.columns[0].collapsed, "empty backlog collapses");
        assert!(!todo.collapsed);
    }

    fn board_input(open: Vec<IssueFile>) -> BoardInput {
        BoardInput {
            open,
            loading: false,
            closed_loaded: true,
            ..BoardInput::default()
        }
    }

    #[test]
    fn issues_board_filters_by_workspace_and_carries_the_read_state() {
        let mut board = vec![
            issue_full("T-1", "todo", &[], Some("alpha")),
            issue("T-2", "todo"),
        ];
        board[0].task.branch = Some("jlong/t-1".into());
        let input = board_input(board);
        let filters = BoardFilters {
            workspace: Some(WorkspaceFilter::Workspace("alpha".into())),
            ..BoardFilters::default()
        };
        let model = IssuesModel::build(&input, &filters);
        let todo = &model.columns[1];
        assert_eq!(todo.cards.len(), 1);
        let card = &todo.cards[0];
        assert_eq!(card.assignee.as_deref(), Some("alpha"));
        assert_eq!(card.branch.as_deref(), Some("jlong/t-1"));
        assert_eq!(card.workflow_name, "default");
        assert_eq!(model.workspace_filter_label, "alpha");
        // The expected state is the stored status + revision the card was read at.
        let expected = card.expected();
        assert_eq!(expected.status, "todo");
        assert_eq!(expected.updated_at, input.open[0].task.updated_at.to_rfc3339());
        assert_eq!(model.position("T-1"), Some((1, 0)));
    }

    #[test]
    fn issues_board_done_column_fills_from_the_closed_page_newest_first() {
        let mut input = board_input(vec![issue("T-1", "todo")]);
        input.closed_loaded = false;
        let model = IssuesModel::build(&input, &BoardFilters::default());
        assert!(model.done_loading, "done is still filling until a closed page lands");
        assert_eq!(model.columns[1].cards.len(), 1, "the open board never waits on history");

        let mut older = issue("D-1", "done");
        older.task.updated_at -= chrono::Duration::hours(2);
        let newer = issue("D-2", "done");
        input.apply_closed(Some(board::ClosedPage {
            issues: vec![older, newer],
            next_cursor: Some("next".into()),
        }));
        let model = IssuesModel::build(&input, &BoardFilters::default());
        assert!(!model.done_loading);
        assert!(model.has_more_done);
        let done: Vec<&str> = model.columns[4].cards.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(done, ["D-2", "D-1"]);
    }

    #[test]
    fn task_detail_carries_frontmatter_fields_and_body() {
        let mut t = issue_full("T-1", "in-progress", &["T-2"], Some("alpha"));
        t.task.branch = Some("jlong/t-1".into());
        t.body = "## Goal\n\n- [ ] ship it\n".into();
        let input = board_input(vec![t, issue("T-2", "todo")]);
        let d = TaskDetailModel::build(&input, "T-1").unwrap();
        assert_eq!(d.status_id, "in-progress");
        assert_eq!(d.status_label, "IN PROGRESS");
        assert_eq!(d.assignee.as_deref(), Some("alpha"));
        assert_eq!(d.branch.as_deref(), Some("jlong/t-1"));
        assert_eq!(d.workflow_name, "default");
        assert!(d.blocked);
        assert_eq!(d.depends_on[0].status.as_deref(), Some("todo"));
        assert!(d.body.contains("- [ ] ship it"));
        assert!(TaskDetailModel::build(&input, "gone").is_none());
    }

    #[test]
    fn activity_filters_by_pill() {
        let items = vec![
            ActivityItem {
                timestamp: "2026-10-03T10:00:00Z".into(),
                agent: Some("developer".into()),
                summary: "T-1 in-progress -> review".into(),
                kind: ActivityKind::Issue,
            },
            ActivityItem {
                timestamp: "2026-10-03T10:01:00Z".into(),
                agent: None,
                summary: "alpha-1 working".into(),
                kind: ActivityKind::Workspace,
            },
            ActivityItem {
                timestamp: "2026-10-03T10:02:00Z".into(),
                agent: None,
                summary: "zen promoted T-9".into(),
                kind: ActivityKind::Zen,
            },
        ];
        let model = ActivityModel::from_items(items);
        assert_eq!(model.filtered(ActivityFilter::default()).len(), 3);
        assert_eq!(
            model
                .filtered(ActivityFilter {
                    zen_only: true,
                    workspaces_only: false
                })
                .len(),
            1
        );
        let both = model.filtered(ActivityFilter {
            zen_only: true,
            workspaces_only: true,
        });
        assert_eq!(both.len(), 2);
    }

    #[test]
    fn machines_count_workspaces_and_active() {
        let machines = vec![
            Machine {
                name: "local".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp".into(),
                host: None,
                tags: vec!["review".into()],
                forward: None,
            },
            Machine {
                name: "gpu".into(),
                kind: MachineKind::Ssh,
                work_dir: "/home/me".into(),
                host: Some("gpu.local".into()),
                tags: vec![],
                forward: None,
            },
        ];
        let workspaces = vec![
            WorkspaceSpec {
                name: "local-1".into(),
                machine: "local".into(),
                tags: vec![],
                slot: None,
            },
            WorkspaceSpec {
                name: "local-2".into(),
                machine: "local".into(),
                tags: vec![],
                slot: None,
            },
            WorkspaceSpec {
                name: "gpu-1".into(),
                machine: "gpu".into(),
                tags: vec![],
                slot: None,
            },
        ];
        let mut active = HashMap::new();
        active.insert("local".to_string(), 1usize);
        let model = MachinesModel::from_project(&machines, &workspaces, &active);
        let local = model.machines.iter().find(|m| m.name == "local").unwrap();
        assert!(local.is_local);
        assert_eq!(local.workspace_count, 2);
        assert_eq!(local.active_count, 1);
        assert_eq!(local.tags, vec!["review".to_string()]);
        let gpu = model.machines.iter().find(|m| m.name == "gpu").unwrap();
        assert!(!gpu.is_local);
        assert_eq!(gpu.host.as_deref(), Some("gpu.local"));
        assert_eq!(gpu.workspace_count, 1);
        assert_eq!(gpu.active_count, 0);
    }

    #[test]
    fn review_panel_includes_browser_only_with_url() {
        let no_url = ReviewPanelModel::new("T-1", "Fix", "/wt/alpha", "nvim", false);
        assert_eq!(no_url.status_label, "Ready for review");
        assert!(!no_url.switch_items.contains(&ReviewSwitch::Browser));
        assert_eq!(no_url.switch_items.len(), 3);

        let with_url = ReviewPanelModel::new("T-1", "Fix", "/wt/alpha", "nvim", true);
        assert!(with_url.switch_items.contains(&ReviewSwitch::Browser));
        assert_eq!(with_url.switch_items.len(), 4);
    }

    #[test]
    fn error_log_counts_unread() {
        let rows = vec![
            (
                ErrorLogEntry {
                    ts: "2026-10-03T10:00:00Z".into(),
                    message: "boom".into(),
                    source: Some("kanban".into()),
                },
                true,
            ),
            (
                ErrorLogEntry {
                    ts: "2026-10-03T09:00:00Z".into(),
                    message: "older".into(),
                    source: None,
                },
                false,
            ),
        ];
        let model = ErrorLogModel::from_entries(&rows);
        assert_eq!(model.entries.len(), 2);
        assert_eq!(model.unread, 1);
        assert_eq!(model.entries[0].source.as_deref(), Some("kanban"));
    }

    #[test]
    fn serving_review_badge_is_cyan_check() {
        // A live, URL-less review resolves to Serving (see
        // `shelbi_orchestrator::workspace::review_slot_serving`), and Serving
        // must read as a cyan ✓ — the "ready for review" mark.
        let dec = ReviewState::Serving.decoration();
        assert_eq!(dec.glyph, "✓");
        assert_eq!(dec.color, DecorationColor::Cyan);
    }

    #[test]
    fn loading_review_badge_is_muted_not_yellow() {
        // A review whose declared server isn't up yet stays Loading, and its ▶
        // must render in the design's muted tint (`#7a7a7a` via
        // `DecorationColor::Muted`), never the old alert-yellow.
        let dec = ReviewState::Loading.decoration();
        assert_eq!(dec.glyph, "▶");
        assert_eq!(dec.color, DecorationColor::Muted);
        assert_ne!(dec.color, DecorationColor::Yellow);
    }

    #[test]
    fn pending_review_badge_is_muted_dot() {
        let dec = ReviewState::Pending.decoration();
        assert_eq!(dec.glyph, "·");
        assert_eq!(dec.color, DecorationColor::DarkGray);
    }
}
