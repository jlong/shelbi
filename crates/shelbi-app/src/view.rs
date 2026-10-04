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

use std::collections::HashMap;

use shelbi_core::{Column, Machine, MachineKind, StatusCategory, WorkspaceSpec};
use shelbi_state::{ErrorLogEntry, IssueFile};

use crate::nav::View;

// ---------------------------------------------------------------------------
// Sidebar
// ---------------------------------------------------------------------------

/// The left navigation list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarModel {
    pub project_label: String,
    pub nav: Vec<NavItem>,
    pub workspaces: Vec<WorkspaceRow>,
    /// Tasks sitting in the review column.
    pub reviews: Vec<ReviewRow>,
    pub zen_on: bool,
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
    /// The task id this workspace is currently working, if any.
    pub current_task: Option<String>,
    /// The agent name (from the task's `agent:` frontmatter), if known.
    pub agent: Option<String>,
}

/// A review-column task row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewRow {
    pub task_id: String,
    pub title: String,
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
            })
            .collect();

        let workspaces = workspace_names
            .iter()
            .map(|name| {
                let task = board.iter().find(|f| {
                    f.task.assigned_to.as_deref() == Some(name.as_str())
                        && f.task.column.category() == StatusCategory::Active
                });
                WorkspaceRow {
                    name: name.clone(),
                    current_task: task.map(|f| f.task.id.clone()),
                    agent: task.and_then(|f| f.task.param_str("agent").map(str::to_string)),
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
                NavItem {
                    label: "Machines".into(),
                    view: View::Machines,
                },
            ],
            workspaces,
            reviews,
            zen_on,
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
        assert!(model.zen_on);
        assert_eq!(model.unread_errors, 4);
        assert_eq!(model.nav.len(), 4);
        assert_eq!(model.nav[1].view, View::Issues);
        assert_eq!(model.nav[3].view, View::Machines);

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
}
