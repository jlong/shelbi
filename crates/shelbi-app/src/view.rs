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
        }
    }

    /// Colour the glyph paints in.
    pub fn decoration_color(self) -> DecorationColor {
        match self {
            WorkspaceBadge::Working => DecorationColor::Green,
            WorkspaceBadge::AwaitingInput => DecorationColor::Yellow,
            WorkspaceBadge::AwaitingPermission => DecorationColor::Red,
            WorkspaceBadge::Paused => DecorationColor::Yellow,
            WorkspaceBadge::Idle => DecorationColor::DarkGray,
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
                    label: "Chat".into(),
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
        }
    }
}

// ---------------------------------------------------------------------------
// Issues board
// ---------------------------------------------------------------------------

/// The kanban board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuesModel {
    pub columns: Vec<IssueColumn>,
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
    pub category: StatusCategory,
    pub cards: Vec<IssueCard>,
}

/// A card on the board.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueCard {
    pub id: String,
    pub title: String,
    /// True when a dependency is not yet done (renders the 🔒 prefix).
    pub blocked: bool,
    pub branch: Option<String>,
    pub workflow: Option<String>,
}

impl IssuesModel {
    /// Bucket a board slice into the given columns by status id. A card's
    /// `blocked` flag is computed against the whole board, matching the
    /// kanban's lock glyph.
    pub fn from_board(columns: &[ColumnDef], board: &[IssueFile]) -> IssuesModel {
        // id -> column, so dependency status can be checked for `is_blocked`.
        let by_id: HashMap<String, Column> = board
            .iter()
            .map(|f| (f.task.id.clone(), f.task.column.clone()))
            .collect();

        let columns = columns
            .iter()
            .map(|def| {
                let cards = board
                    .iter()
                    .filter(|f| f.task.column.as_str() == def.status_id)
                    .map(|f| IssueCard {
                        id: f.task.id.clone(),
                        title: f.task.title.clone(),
                        blocked: f.task.is_blocked(&by_id),
                        branch: f.task.branch.clone(),
                        workflow: f.task.workflow.clone(),
                    })
                    .collect();
                IssueColumn {
                    status_id: def.status_id.clone(),
                    status_name: def.status_name.clone(),
                    category: def.category,
                    cards,
                }
            })
            .collect();

        IssuesModel { columns }
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
        let cols = vec![
            ColumnDef {
                status_id: "todo".into(),
                status_name: "To do".into(),
                category: StatusCategory::Ready,
            },
            ColumnDef {
                status_id: "in-progress".into(),
                status_name: "In progress".into(),
                category: StatusCategory::Active,
            },
            ColumnDef {
                status_id: "done".into(),
                status_name: "Done".into(),
                category: StatusCategory::Done,
            },
        ];
        let model = IssuesModel::from_board(&cols, &board);
        assert_eq!(model.columns.len(), 3);
        let todo = &model.columns[0];
        assert_eq!(todo.cards.len(), 3);
        let t2 = todo.cards.iter().find(|c| c.id == "T-2").unwrap();
        assert!(t2.blocked, "T-2 depends on an unfinished task");
        let t4 = todo.cards.iter().find(|c| c.id == "T-4").unwrap();
        assert!(!t4.blocked, "T-4's dependency is done");
        assert_eq!(model.columns[2].category, StatusCategory::Done);
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
