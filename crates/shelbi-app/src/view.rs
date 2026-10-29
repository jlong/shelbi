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

use shelbi_core::{Column, Machine, MachineKind, Reachability, StatusCategory, WorkspaceSpec};
use shelbi_palette::{Decoration, DecorationColor};
use shelbi_state::machine_state::MachineRecord;
use shelbi_state::{
    ErrorLogEntry, IssueFile, TrackerOwner, WorkspaceState, WorkspaceStatus, ZenModeState,
};

use crate::board::{self, BoardInput, BoardRules, WorkspaceFilter};
use crate::board_search::{BoardSearch, SearchStatus};
use crate::nav::View;
use crate::theme::Tone;

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
    /// Board freshness banner: `None` for a local or not-yet-probed board. The
    /// desktop footer shows its warnings; the TUI sidebar doesn't show it (its
    /// Issues board does).
    pub board_banner: Option<String>,
    /// Preformatted daemon/CLI version line. `None` until probed. Both clients
    /// show it with the Settings button (tooltip / hover name) and, on a
    /// mismatch, in red in the footer.
    pub daemon_version_line: Option<String>,
    /// True when the probed daemon version differs from the running binary:
    /// the footer shows the version line, red, naming the fix.
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
    /// The daemon supervisor's status for a dead orchestrator it could not
    /// bring back (e.g. `supervisor gave up: <reason>`), painted on the
    /// sidebar's status row. `None` while the orchestrator is healthy.
    pub orchestrator_status: Option<String>,
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
    /// The issue's number on its GitHub-backed tracker (`#401`), when it has
    /// one. `None` on the file-system backend, or before the daemon's board
    /// index has recorded it.
    pub number: Option<i64>,
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

impl WorkspaceRow {
    /// The right-aligned label on the workspace's row: the agent's name in
    /// title case, `idle` with no task, or nothing for a skeleton row whose
    /// state hasn't been read yet (its `…` badge carries that).
    pub fn right_label(&self) -> String {
        match &self.agent {
            Some(a) => title_case(a),
            None if self.badge == WorkspaceBadge::Loading => String::new(),
            None => "idle".to_string(),
        }
    }
}

impl WorkspaceBadge {
    /// The state in words, for a header or tooltip beside the glyph.
    pub fn label(self) -> &'static str {
        match self {
            WorkspaceBadge::Working => "Working",
            WorkspaceBadge::AwaitingInput => "Awaiting input",
            WorkspaceBadge::AwaitingPermission => "Needs permission",
            WorkspaceBadge::Paused => "Paused",
            WorkspaceBadge::Idle => "Idle",
            WorkspaceBadge::Loading => "Loading",
        }
    }
}

// ---------------------------------------------------------------------------
// Workspace view
// ---------------------------------------------------------------------------

/// What a workspace view resolved about its workspace's task, read off the UI
/// thread (`shelbi_orchestrator::workspace_session::workspace_details`). The
/// sidebar row carries the live state and the task id; this carries what the
/// row doesn't: the title, the branch, the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorkspaceDetailsInfo {
    /// The in-progress task's id and title, when the workspace has one.
    pub task: Option<(String, String)>,
    /// The task's markdown body (the description the header's More opens).
    pub body: String,
    /// The branch: the task's, else the local worktree's.
    pub branch: Option<String>,
    /// The worktree path on the workspace's machine.
    pub worktree: String,
    /// The resolved editor's display name (`Vim`, ...).
    pub editor_name: String,
}

/// The header above a workspace's agent: which task it is working (id and
/// title), on which branch and machine, and its live state. Built from the
/// sidebar's [`WorkspaceRow`] (live: the state and the task id move with every
/// refresh) and the resolved [`WorkspaceDetailsInfo`] (the title and branch,
/// read once per task).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceHeader {
    pub name: String,
    /// The task id, when the workspace is working one.
    pub task_id: Option<String>,
    /// The task's title, once resolved for *that* task (a title read for the
    /// previous task is never shown under a new id).
    pub title: Option<String>,
    pub branch: Option<String>,
    pub machine: String,
    pub is_remote: bool,
    /// The agent's display name (`Developer`), when the row knows it.
    pub agent: Option<String>,
    /// The live state: the sidebar's badge, so the two never disagree.
    pub state: WorkspaceBadge,
}

impl WorkspaceHeader {
    /// The header for `row`, filling the title (and the task's branch) from
    /// `details` only when they describe the task the row says is running.
    pub fn build(row: &WorkspaceRow, details: Option<&WorkspaceDetailsInfo>) -> Self {
        let task_details = details.and_then(|d| d.task.as_ref());
        let same_task = matches!(
            (row.current_task.as_deref(), task_details),
            (Some(id), Some((resolved, _))) if id == resolved
        );
        let title = if same_task {
            task_details.map(|(_, title)| title.clone())
        } else {
            None
        };
        // A branch resolved for a different task (or for the worktree while
        // idle) is stale once the row moves on.
        let details_current = same_task || (row.current_task.is_none() && task_details.is_none());
        let branch = details
            .filter(|_| details_current)
            .and_then(|d| d.branch.clone());
        WorkspaceHeader {
            name: row.name.clone(),
            task_id: row.current_task.clone(),
            title,
            branch,
            machine: row.machine.clone(),
            is_remote: row.is_remote,
            agent: row.agent.as_deref().map(title_case),
            state: row.badge,
        }
    }

    /// Whether `details` is out of date for this header: the row names a task
    /// the details don't describe (or the reverse), so the view re-reads them.
    pub fn needs_details(row: &WorkspaceRow, details: Option<&WorkspaceDetailsInfo>) -> bool {
        let resolved = details.and_then(|d| d.task.as_ref()).map(|(id, _)| id.as_str());
        details.is_none() || row.current_task.as_deref() != resolved
    }

    /// The task line: `id  title`, the id alone until the title resolves, or
    /// `None` while idle.
    pub fn task_line(&self) -> Option<String> {
        let id = self.task_id.as_ref()?;
        Some(match &self.title {
            Some(title) if !title.is_empty() => format!("{id}  {title}"),
            _ => id.clone(),
        })
    }

    /// The location line: `machine · branch`, with the branch dropped when
    /// unknown and `(remote)` marking a machine reached over SSH.
    pub fn location_line(&self) -> String {
        let mut machine = self.machine.clone();
        if self.is_remote {
            machine.push_str(" (remote)");
        }
        match &self.branch {
            Some(branch) if !branch.is_empty() => format!("{machine} · {branch}"),
            _ => machine,
        }
    }

    /// The state badge as the sidebar draws it, plus its words.
    pub fn state_decoration(&self) -> Decoration {
        self.state.decoration()
    }
}

/// The fixed nav builtins, in sidebar order: the orchestrator chat, the issues
/// board, the activity feed. Machines is reachable from the command palette,
/// not the sidebar nav.
pub fn default_nav_items() -> Vec<NavItem> {
    vec![
        NavItem {
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
    ]
}

/// Uppercase the first character of a lowercase identifier (`developer` →
/// `Developer`). Empty input is preserved.
pub fn title_case(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// One laid-out sidebar row: the flat, ordered list every renderer draws, so
/// the TUI and the desktop app show the same items in the same order with the
/// same labels. Built by [`SidebarModel::rows`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarRow {
    /// A fixed nav builtin.
    Nav { label: String, view: View },
    /// A section header (`Workspaces`, `Ready for Review`, `Queued for Review`).
    Section(String),
    /// A present-but-broken project config, shown under the Workspaces header.
    ConfigError(String),
    /// Vertical breathing room between sections.
    Blank,
    /// The dim "Loading…" placeholder of a cold board.
    Loading,
    /// A machine group header (only when the project spans several machines).
    MachineGroup {
        name: String,
        collapsed: bool,
        total: usize,
        active: usize,
    },
    /// A dev workspace.
    Workspace {
        name: String,
        badge: WorkspaceBadge,
        /// The right-aligned label ([`WorkspaceRow::right_label`]).
        right_label: String,
        /// Indented under a machine group header.
        indent: bool,
    },
    /// A review-column task (a two-line entry: title, then `⎇ branch`).
    Review {
        task_id: String,
        title: String,
        branch: String,
        location: Option<String>,
        state: ReviewState,
    },
}

/// What a selectable [`SidebarRow`] opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarTarget {
    /// A nav builtin's view.
    View(View),
    /// A dev workspace, by name.
    Workspace(String),
    /// A review-column task, by id.
    Review(String),
    /// A machine group header: toggles its collapse state.
    Machine(String),
}

impl SidebarRow {
    /// Whether keyboard selection stops on this row. Headers, blanks, the
    /// loading placeholder and the config error are skipped.
    pub fn is_selectable(&self) -> bool {
        self.target().is_some()
    }

    /// What opening this row does, or `None` for a non-selectable row.
    pub fn target(&self) -> Option<SidebarTarget> {
        match self {
            SidebarRow::Nav { view, .. } => Some(SidebarTarget::View(view.clone())),
            SidebarRow::MachineGroup { name, .. } => Some(SidebarTarget::Machine(name.clone())),
            SidebarRow::Workspace { name, .. } => Some(SidebarTarget::Workspace(name.clone())),
            SidebarRow::Review { task_id, .. } => Some(SidebarTarget::Review(task_id.clone())),
            SidebarRow::Section(_)
            | SidebarRow::ConfigError(_)
            | SidebarRow::Blank
            | SidebarRow::Loading => None,
        }
    }
}

impl SidebarModel {
    /// A zero-IO sidebar for `project`: the fixed nav, the raw project name,
    /// and the board-loading flag. Painted the instant a cold project switch
    /// lands, before any read for the target project has finished, so the
    /// chrome shows the target project immediately and data cascades in.
    pub fn skeleton(project: &str) -> SidebarModel {
        SidebarModel {
            project_label: project.to_string(),
            nav: default_nav_items(),
            workspaces: Vec::new(),
            reviews: Vec::new(),
            config_error: None,
            board_loading: true,
            collapsed_machines: BTreeSet::new(),
            board_banner: None,
            daemon_version_line: None,
            daemon_version_mismatch: false,
            status_line: String::new(),
            zen_mode: ZenModeState::Off,
            unread_errors: 0,
            session_lost: None,
            orchestrator_status: None,
        }
    }

    /// The footer's status row text: the orchestrator supervisor's failure
    /// (`Orchestrator: <status>`, painted red) takes the row while there is one,
    /// else [`SidebarModel::status_line`]. Empty means no status row.
    pub fn footer_status(&self) -> String {
        match &self.orchestrator_status {
            Some(status) => format!("Orchestrator: {status}"),
            None => self.status_line.clone(),
        }
    }

    /// Lay the model out as the ordered row list: the fixed nav, then the
    /// machine-grouped workspace pool (or the config-error / loading
    /// placeholder), then the Ready / Queued review sections. Each section
    /// header and its rows drop together when empty.
    pub fn rows(&self) -> Vec<SidebarRow> {
        let mut rows: Vec<SidebarRow> = self
            .nav
            .iter()
            .map(|nav| SidebarRow::Nav {
                label: nav.label.clone(),
                view: nav.view.clone(),
            })
            .collect();

        // Cold board, nothing loaded yet: a dim "Loading…" placeholder.
        if self.board_loading {
            rows.push(SidebarRow::Blank);
            rows.push(SidebarRow::Loading);
            return rows;
        }

        if let Some(message) = &self.config_error {
            rows.push(SidebarRow::Blank);
            rows.push(SidebarRow::Section("Workspaces".into()));
            rows.push(SidebarRow::ConfigError(message.clone()));
        } else if !self.workspaces.is_empty() {
            rows.push(SidebarRow::Blank);
            rows.push(SidebarRow::Section("Workspaces".into()));
            // Group by machine when the project declares more than one; a
            // single-machine project collapses to a flat list.
            let mut machines: Vec<&str> = Vec::new();
            for w in &self.workspaces {
                if !machines.contains(&w.machine.as_str()) {
                    machines.push(&w.machine);
                }
            }
            let workspace_row = |w: &WorkspaceRow, indent: bool| SidebarRow::Workspace {
                name: w.name.clone(),
                badge: w.badge,
                right_label: w.right_label(),
                indent,
            };
            if machines.len() > 1 {
                for machine in machines {
                    let on_machine: Vec<&WorkspaceRow> = self
                        .workspaces
                        .iter()
                        .filter(|w| w.machine == machine)
                        .collect();
                    let collapsed = self.collapsed_machines.contains(machine);
                    rows.push(SidebarRow::MachineGroup {
                        name: machine.to_string(),
                        collapsed,
                        total: on_machine.len(),
                        active: on_machine
                            .iter()
                            .filter(|w| w.current_task.is_some())
                            .count(),
                    });
                    if !collapsed {
                        rows.extend(on_machine.into_iter().map(|w| workspace_row(w, true)));
                    }
                }
            } else {
                rows.extend(self.workspaces.iter().map(|w| workspace_row(w, false)));
            }
        }

        let review_row = |r: &ReviewRow| SidebarRow::Review {
            task_id: r.task_id.clone(),
            title: r.title.clone(),
            branch: r.branch.clone(),
            location: r.location.clone(),
            state: r.state,
        };
        // Ready for Review: Serving / Loading rows (already on a slot).
        let ready: Vec<&ReviewRow> = self
            .reviews
            .iter()
            .filter(|r| r.state != ReviewState::Pending)
            .collect();
        if !ready.is_empty() {
            rows.push(SidebarRow::Blank);
            rows.push(SidebarRow::Section("Ready for Review".into()));
            rows.extend(ready.into_iter().map(review_row));
        }
        // Queued for Review: Pending rows (waiting for a free slot).
        let queued: Vec<&ReviewRow> = self
            .reviews
            .iter()
            .filter(|r| r.state == ReviewState::Pending)
            .collect();
        if !queued.is_empty() {
            rows.push(SidebarRow::Blank);
            rows.push(SidebarRow::Section("Queued for Review".into()));
            rows.extend(queued.into_iter().map(review_row));
        }

        rows
    }

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
                number: None,
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
            nav: default_nav_items(),
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
            orchestrator_status: None,
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
    /// The done column offers "load more": more terminal history, or, while
    /// a remote search's results show, more of them.
    pub has_more_done: bool,
    /// The text search's state for the search box (searching, the match
    /// count, the local-results notice), or `None` with no query.
    pub search: Option<SearchStatus>,
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
    /// The body's first prose paragraph as plain text, the card's excerpt
    /// ([`crate::board::card_summary`]); empty when the body has none.
    pub summary: String,
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
    /// Whose the issue is on a remote tracker, or `None` on a filesystem board
    /// (no ownership). A card that isn't the user's is marked, and isn't offered
    /// to start ([`IssueCard::startable`]).
    pub tracker_owner: Option<TrackerOwner>,
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
/// (the client reads it back through [`BoardInput::workspace_filter`]). The
/// text search is per-session too.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardFilters {
    pub workflow: Option<String>,
    pub workspace: Option<WorkspaceFilter>,
    /// The board's text search ([`board::card_matches_search`]): blank
    /// shows every card.
    pub search: String,
}

impl IssueCard {
    /// Whether the board offers to start this card: always on a filesystem
    /// board, and on a remote tracker only when it's assigned to the user
    /// (starting someone else's takes the explicit `issue start --take`).
    pub fn startable(&self) -> bool {
        start_refusal(&self.id, self.tracker_owner.as_ref()).is_none()
    }

    /// Why the board won't start this card, for the status line, or `None`
    /// when it will ([`IssueCard::startable`]).
    pub fn start_refusal(&self) -> Option<String> {
        start_refusal(&self.id, self.tracker_owner.as_ref())
    }

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
        Self::build_searched(input, filters, &BoardSearch::local(&filters.search))
    }

    /// [`IssuesModel::build`] under a [`BoardSearch`]: which cards show is the
    /// search's call ([`BoardSearch::shows`]) rather than a local filter of
    /// `filters.search`. While the tracker's results show, "load more" pages
    /// them and the done column isn't waiting on history.
    pub fn build_searched(
        input: &BoardInput,
        filters: &BoardFilters,
        search: &BoardSearch,
    ) -> IssuesModel {
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
                    .map(|tf| card_for(tf, &by_id, &rules, &def.status_id, &input.ownership))
                    .filter(|c| search.shows(&c.title, &c.id, c.branch.as_deref()))
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
            done_loading: !input.closed_loaded && !search.showing_results(),
            has_more_done: match search.status() {
                Some(status) if search.showing_results() => status.has_more,
                _ => input.closed_next_cursor.is_some(),
            },
            search: search.status(),
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
    /// The status's name as the workflow spells it (`In Progress`).
    pub status_name: String,
    pub category: StatusCategory,
    pub priority: u32,
    pub assignee: Option<String>,
    /// Whose the issue is on a remote tracker ([`IssueCard::tracker_owner`]).
    pub tracker_owner: Option<TrackerOwner>,
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

/// Why a board won't start issue `id` with this tracker owner: it is someone
/// else's, or unassigned, on a remote tracker (starting it takes the explicit
/// `issue start --take` claim). `None` when it's the user's, or the board has
/// no ownership (filesystem). Shared by both clients' Start affordances.
pub fn start_refusal(id: &str, owner: Option<&TrackerOwner>) -> Option<String> {
    let owner = owner.filter(|o| !o.is_mine())?;
    let whose = match owner {
        TrackerOwner::Unassigned => "unassigned on the tracker".to_string(),
        TrackerOwner::Unknown(_) => "not known to be yours (your tracker login is unresolved)"
            .to_string(),
        other => format!("assigned to {}", other.label()),
    };
    Some(format!(
        "{id} is {whose}; run `shelbi issue start {id} --take` to claim and start it"
    ))
}

impl TaskDetailModel {
    /// Whether the detail offers Start ([`IssueCard::startable`]).
    pub fn startable(&self) -> bool {
        start_refusal(&self.id, self.tracker_owner.as_ref()).is_none()
    }

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
        let card = card_for(tf, &by_id, &rules, &status_id, &input.ownership);
        let status = input
            .project_statuses
            .statuses
            .iter()
            .find(|st| st.id == status_id);
        let task = &tf.task;
        let status_name = status
            .map(|st| st.name.clone())
            .unwrap_or_else(|| task.column.default_status_name().to_string());
        Some(TaskDetailModel {
            id: task.id.clone(),
            title: task.title.clone(),
            status_label: board::column_label(&status_name),
            status_name,
            category: status.map(|st| st.category).unwrap_or(task.column.category()),
            status_id,
            priority: task.priority,
            assignee: card.assignee,
            tracker_owner: card.tracker_owner,
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
    ownership: &shelbi_state::OwnershipGate,
) -> IssueCard {
    let task = &tf.task;
    let workflow_name = rules.workflow_name(task).to_string();
    IssueCard {
        id: task.id.clone(),
        title: task.title.clone(),
        summary: board::card_summary(&tf.body),
        blocked: task.is_blocked(by_id),
        branch: board::card_branch(task, rules.workflows, &workflow_name),
        workflow: task.workflow.clone(),
        workflow_name,
        assignee: task.assigned_to.clone(),
        tracker_owner: ownership.owner(&tf.tracker_assignees),
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
// Machines
// ---------------------------------------------------------------------------

/// The machines view: every machine the project declares, each with every
/// workspace that lives on it (review slots included). Shared by the TUI's
/// machines view and the desktop's; `shelbi_orchestrator::machines_model`
/// reads the inputs and [`MachinesModel::build`] maps them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MachinesModel {
    /// The project's display name (or label), for the view's title.
    pub display_name: Option<String>,
    pub machines: Vec<MachineRow>,
}

/// A machine and the workspaces on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineRow {
    pub name: String,
    pub kind: MachineKind,
    /// SSH host, when this is a remote machine.
    pub host: Option<String>,
    pub is_local: bool,
    /// Capability tags declared on the machine.
    pub tags: Vec<String>,
    /// The resolved remote `shelbi` binary (path + version) once `shelbi
    /// machine setup` has run for it. `None` for a local machine or a remote
    /// not set up yet.
    pub remote: Option<MachineRecord>,
    /// Live reachability, folded in from a [`ReachabilityProber`]
    /// (`fold_reachability`). A local machine is the hub itself: always
    /// [`Reachability::Reachable`] and never probed. A remote reads
    /// [`Reachability::Unknown`] ("checking…") until its first probe lands.
    ///
    /// [`ReachabilityProber`]: crate::reachability::ReachabilityProber
    pub reachability: Reachability,
    pub workspaces: Vec<MachineWorkspace>,
}

/// A workspace under its machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineWorkspace {
    pub name: String,
    /// Whether this is a `review`-tagged slot.
    pub is_review_slot: bool,
    /// The hub poller's last observed state (`status.yaml`), or `None` when
    /// the workspace has none yet.
    pub state: Option<WorkspaceState>,
    /// The task the observed status names, if any.
    pub current_task: Option<String>,
    /// What the workspace is assigned, as `shelbi workspace list` reads it from
    /// the board.
    pub assignment: SlotAssignment,
    /// The agent `shelbi workspace list` shows: the in-progress task's
    /// `agent:` (default `developer`), `review` for a serving review slot.
    /// `None` when idle.
    pub agent: Option<String>,
    /// The agent CLI that task's agent runs on (`Claude`, `Codex`, or a
    /// generic runner's command name), resolved the way dispatch resolves
    /// it. `None` when idle.
    pub runner: Option<String>,
    /// The assigned task's title, when the board has it.
    pub task_title: Option<String>,
    /// The assigned task's branch.
    pub branch: Option<String>,
    /// For a review slot holding a review task: its server state and URL.
    pub review: Option<ReviewSlot>,
}

impl MachineWorkspace {
    /// The assignment as `<status>: <task title>` (`in_progress: Add project
    /// switching`, `review: Desktop application`), or `idle`. Falls back to
    /// the ids ([`SlotAssignment::cell`]) when the board has no title, or
    /// when a diverged board assigns several in-progress tasks.
    pub fn task_cell(&self) -> String {
        let title = self.task_title.as_deref();
        match (&self.assignment, title) {
            (SlotAssignment::InProgress(ids), Some(title)) if ids.len() == 1 => {
                format!("in_progress: {title}")
            }
            (SlotAssignment::Review(_), Some(title)) => format!("review: {title}"),
            (assignment, _) => assignment.cell(),
        }
    }
}

/// A workspace's assignment, matching `shelbi workspace list`'s STATE cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotAssignment {
    /// Nothing assigned.
    Idle,
    /// In-progress task(s) assigned to it (normally one; more means the board
    /// diverged, and the cell shows them all).
    InProgress(Vec<String>),
    /// A review slot serving (or loading) a review-column task.
    Review(String),
}

impl SlotAssignment {
    /// The STATE cell text `shelbi workspace list` prints for this assignment:
    /// `in_progress: <ids>`, `review: <id>`, or `idle`.
    pub fn cell(&self) -> String {
        match self {
            SlotAssignment::Idle => "idle".to_string(),
            SlotAssignment::InProgress(ids) => format!("in_progress: {}", ids.join(", ")),
            SlotAssignment::Review(id) => format!("review: {id}"),
        }
    }

    /// The task the slot holds (the first one, for a diverged board).
    pub fn task(&self) -> Option<&str> {
        match self {
            SlotAssignment::Idle => None,
            SlotAssignment::InProgress(ids) => ids.first().map(String::as_str),
            SlotAssignment::Review(id) => Some(id),
        }
    }
}

/// A review slot's server: whether it is up and where.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSlot {
    pub task_id: String,
    /// [`ReviewState::Serving`] once the server is confirmed up, else
    /// [`ReviewState::Loading`].
    pub state: ReviewState,
    /// The review URL (the workflow's `review:` template with `$PORT`/`$SLOT`
    /// substituted), when the workflow declares one.
    pub url: Option<String>,
}

impl ReviewSlot {
    /// `serving` or `starting`.
    pub fn label(&self) -> &'static str {
        match self.state {
            ReviewState::Serving => "serving",
            _ => "starting",
        }
    }
}

/// Everything [`MachinesModel::build`] maps: plain data the host has already
/// read (the project's declarations, the machine records, the workspace
/// statuses, the board's in-progress tasks, and the resolved review slots).
#[derive(Debug, Clone, Default)]
pub struct MachinesInput {
    pub display_name: Option<String>,
    pub machines: Vec<Machine>,
    /// The project's workspaces, each with whether it is a review slot.
    pub workspaces: Vec<(WorkspaceSpec, bool)>,
    pub records: HashMap<String, MachineRecord>,
    pub statuses: HashMap<String, WorkspaceStatus>,
    /// The board's in-progress tasks.
    pub in_progress: Vec<IssueFile>,
    /// Review slot name -> the review task it holds.
    pub review_slots: HashMap<String, ReviewSlot>,
    /// Task id -> the task's branch (as the orchestrator names it).
    pub branches: HashMap<String, String>,
    /// Task id -> title, for tasks in neither `in_progress` (review tasks).
    pub titles: HashMap<String, String>,
    /// Task id -> the runner its agent launches on (`Claude`, `Codex`, ...).
    pub runners: HashMap<String, String>,
}

/// The default agent `shelbi workspace list` shows for a task with no
/// `agent:`.
pub const DEFAULT_TASK_AGENT: &str = "developer";
/// The agent `shelbi workspace list` shows for a review slot.
pub const REVIEW_SLOT_AGENT: &str = "review";

impl MachinesModel {
    /// Map the read inputs. Machines keep their declared order and each
    /// lists its workspaces in declared order.
    pub fn build(input: &MachinesInput) -> MachinesModel {
        let machines = input
            .machines
            .iter()
            .map(|m| {
                let is_local = m.kind == MachineKind::Local;
                let workspaces = input
                    .workspaces
                    .iter()
                    .filter(|(w, _)| w.machine == m.name)
                    .map(|(w, review)| machine_workspace(input, w, *review))
                    .collect();
                MachineRow {
                    name: m.name.clone(),
                    kind: m.kind,
                    host: m.host.clone(),
                    is_local,
                    tags: m.tags.clone(),
                    remote: if is_local {
                        None
                    } else {
                        let host = m.host.as_deref().unwrap_or(&m.name);
                        input
                            .records
                            .get(&m.name)
                            .filter(|r| r.is_for_host(host))
                            .cloned()
                    },
                    reachability: if is_local {
                        Reachability::Reachable
                    } else {
                        Reachability::Unknown
                    },
                    workspaces,
                }
            })
            .collect();
        MachinesModel {
            display_name: input.display_name.clone(),
            machines,
        }
    }

    /// The `(machine, ssh host)` pairs a reachability prober should probe:
    /// the remotes only (a local machine is never probed). An SSH machine with
    /// no explicit host is reached by its name, as `Machine::host()` does.
    pub fn probe_targets(&self) -> Vec<(String, String)> {
        self.machines
            .iter()
            .filter(|m| !m.is_local)
            .map(|m| {
                let host = m.host.clone().unwrap_or_else(|| m.name.clone());
                (m.name.clone(), host)
            })
            .collect()
    }

    /// Overlay a prober's latest results onto the remotes. A remote with no
    /// result yet keeps [`Reachability::Unknown`].
    pub fn fold_reachability(&mut self, results: &HashMap<String, Reachability>) {
        for m in &mut self.machines {
            if m.is_local {
                continue;
            }
            if let Some(r) = results.get(&m.name) {
                m.reachability = r.clone();
            }
        }
    }

    /// Every workspace, in render order, as `(machine index, workspace index)`.
    pub fn workspace_positions(&self) -> Vec<(usize, usize)> {
        self.machines
            .iter()
            .enumerate()
            .flat_map(|(mi, m)| (0..m.workspaces.len()).map(move |wi| (mi, wi)))
            .collect()
    }

    /// Total workspaces across every machine.
    pub fn workspace_count(&self) -> usize {
        self.machines.iter().map(|m| m.workspaces.len()).sum()
    }
}

fn machine_workspace(input: &MachinesInput, w: &WorkspaceSpec, is_review_slot: bool) -> MachineWorkspace {
    let status = input.statuses.get(&w.name);
    let mine: Vec<&IssueFile> = input
        .in_progress
        .iter()
        .filter(|tf| tf.task.assigned_to.as_deref() == Some(w.name.as_str()))
        .collect();
    let review = input.review_slots.get(&w.name).cloned();
    // `shelbi workspace list` precedence: an in-progress assignment, else a
    // review slot's task, else idle.
    let (assignment, agent) = if let Some(first) = mine.first() {
        let agent = first
            .task
            .param_str("agent")
            .unwrap_or(DEFAULT_TASK_AGENT)
            .to_string();
        (
            SlotAssignment::InProgress(mine.iter().map(|tf| tf.task.id.clone()).collect()),
            Some(agent),
        )
    } else if let Some(r) = &review {
        (
            SlotAssignment::Review(r.task_id.clone()),
            Some(REVIEW_SLOT_AGENT.to_string()),
        )
    } else {
        (SlotAssignment::Idle, None)
    };
    let task = assignment.task().map(str::to_string);
    let task_title = task.as_deref().and_then(|id| {
        mine.iter()
            .find(|tf| tf.task.id == id)
            .map(|tf| tf.task.title.clone())
            .or_else(|| input.titles.get(id).cloned())
    });
    let branch = task.as_deref().and_then(|id| {
        input.branches.get(id).cloned().or_else(|| {
            mine.iter()
                .find(|tf| tf.task.id == id)
                .and_then(|tf| tf.task.branch.clone())
        })
    });
    let runner = task.as_deref().and_then(|id| input.runners.get(id).cloned());
    MachineWorkspace {
        name: w.name.clone(),
        is_review_slot,
        state: status.map(|s| s.state),
        current_task: status.and_then(|s| s.current_task.clone()),
        assignment,
        agent,
        runner,
        task_title,
        branch,
        review: review.filter(|_| mine.is_empty()),
    }
}

/// The reachability badge text and tone the machines views show next to a
/// remote's `ssh <host>` label. `error_max` bounds the unreachable reason.
pub fn reachability_label(r: &Reachability, error_max: usize) -> (String, Tone) {
    match r {
        Reachability::Reachable => ("● reachable".to_string(), Tone::Green),
        Reachability::Unknown => ("○ checking…".to_string(), Tone::DarkGray),
        Reachability::Unreachable { error } => (
            format!("● unreachable: {}", truncate_chars(error, error_max)),
            Tone::Red,
        ),
    }
}

/// A remote's binary line: `<path> v<version>` (flagged when incompatible), or
/// the prompt to set it up.
pub fn remote_binary_label(remote: Option<&MachineRecord>) -> (String, Tone) {
    match remote {
        Some(rec) if rec.compatible => (format!("{} v{}", rec.path, rec.version), Tone::Green),
        Some(rec) => (
            format!("{} v{} (incompatible)", rec.path, rec.version),
            Tone::Red,
        ),
        None => (
            "not set up — run `shelbi machine setup`".to_string(),
            Tone::Yellow,
        ),
    }
}

/// The short badge for an observed workspace state, and its tone.
pub fn workspace_state_badge(state: Option<WorkspaceState>) -> (&'static str, Tone) {
    match state {
        Some(WorkspaceState::Working) => ("working", Tone::Green),
        Some(WorkspaceState::AwaitingInput) => ("awaiting", Tone::Yellow),
        Some(WorkspaceState::Blocked) => ("blocked", Tone::Red),
        Some(WorkspaceState::Paused) => ("paused", Tone::Magenta),
        Some(WorkspaceState::Serving) => ("serving", Tone::Cyan),
        None => ("idle", Tone::DarkGray),
    }
}

/// Truncate `s` to `max` chars, marking an elision with `…`.
pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let keep = max.saturating_sub(1);
        let mut out: String = s.chars().take(keep).collect();
        out.push('…');
        out
    }
}

// ---------------------------------------------------------------------------
// Review panel
// ---------------------------------------------------------------------------

// The review panel's model lives with its state machine in [`crate::review`];
// re-exported here so `view::ReviewPanelModel` keeps naming it.
pub use crate::review::{ReviewPanelModel, ReviewSwitch};

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

    fn ws_row(name: &str, machine: &str, task: Option<&str>, badge: WorkspaceBadge) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            machine: machine.into(),
            is_remote: false,
            current_task: task.map(str::to_string),
            agent: task.map(|_| "developer".to_string()),
            badge,
        }
    }

    fn review_row(id: &str, state: ReviewState) -> ReviewRow {
        ReviewRow {
            task_id: id.into(),
            title: format!("Task {id}"),
            branch: format!("jlong/{id}"),
            location: None,
            workspace: None,
            state,
            number: None,
        }
    }

    #[test]
    fn rows_lay_out_nav_workspaces_then_ready_and_queued_reviews() {
        let mut model = SidebarModel::skeleton("demo");
        model.board_loading = false;
        model.workspaces = vec![
            ws_row("alpha", "hub", Some("T-1"), WorkspaceBadge::Working),
            ws_row("bravo", "hub", None, WorkspaceBadge::Idle),
        ];
        model.reviews = vec![
            review_row("T-2", ReviewState::Pending),
            review_row("T-3", ReviewState::Serving),
        ];
        let rows = model.rows();
        let labels: Vec<String> = rows
            .iter()
            .map(|r| match r {
                SidebarRow::Nav { label, .. } => format!("nav:{label}"),
                SidebarRow::Section(s) => format!("section:{s}"),
                SidebarRow::Blank => "blank".into(),
                SidebarRow::Workspace {
                    name, right_label, indent, ..
                } => format!("ws:{name}:{right_label}:{indent}"),
                SidebarRow::Review { task_id, .. } => format!("review:{task_id}"),
                other => format!("{other:?}"),
            })
            .collect();
        assert_eq!(
            labels,
            [
                "nav:Orchestrator",
                "nav:Issues",
                "nav:Activity",
                "blank",
                "section:Workspaces",
                // Single machine: a flat, unindented list.
                "ws:alpha:Developer:false",
                "ws:bravo:idle:false",
                "blank",
                "section:Ready for Review",
                "review:T-3",
                "blank",
                "section:Queued for Review",
                "review:T-2",
            ]
        );
        let targets: Vec<SidebarTarget> = rows.iter().filter_map(SidebarRow::target).collect();
        assert_eq!(targets[0], SidebarTarget::View(View::Session("orch".into())));
        assert_eq!(targets[3], SidebarTarget::Workspace("alpha".into()));
        assert_eq!(targets[5], SidebarTarget::Review("T-3".into()));
        assert_eq!(targets.len(), 7, "headers and blanks are not selectable");
    }

    #[test]
    fn rows_group_by_machine_and_hide_collapsed_groups() {
        let mut model = SidebarModel::skeleton("demo");
        model.board_loading = false;
        model.workspaces = vec![
            ws_row("alpha", "hub", Some("T-1"), WorkspaceBadge::Working),
            ws_row("delta", "devbox", None, WorkspaceBadge::Idle),
            ws_row("echo", "devbox", Some("T-4"), WorkspaceBadge::AwaitingInput),
        ];
        model.collapsed_machines.insert("devbox".into());
        let rows = model.rows();
        assert!(rows.contains(&SidebarRow::MachineGroup {
            name: "hub".into(),
            collapsed: false,
            total: 1,
            active: 1,
        }));
        assert!(rows.contains(&SidebarRow::MachineGroup {
            name: "devbox".into(),
            collapsed: true,
            total: 2,
            active: 1,
        }));
        let workspaces: Vec<(&str, bool)> = rows
            .iter()
            .filter_map(|r| match r {
                SidebarRow::Workspace { name, indent, .. } => Some((name.as_str(), *indent)),
                _ => None,
            })
            .collect();
        assert_eq!(workspaces, [("alpha", true)], "a collapsed group hides its rows");
    }

    #[test]
    fn footer_status_names_the_orchestrator_failure_over_the_status_line() {
        let mut model = SidebarModel::skeleton("demo");
        assert_eq!(model.footer_status(), "", "no status, no row");
        model.status_line = "hint".into();
        assert_eq!(model.footer_status(), "hint");
        model.orchestrator_status = Some("supervisor gave up: spawn refused".into());
        assert_eq!(model.footer_status(), "Orchestrator: supervisor gave up: spawn refused");
    }

    #[test]
    fn skeleton_and_config_error_rows() {
        let skeleton = SidebarModel::skeleton("demo");
        assert_eq!(skeleton.project_label, "demo");
        let rows = skeleton.rows();
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[4], SidebarRow::Loading);

        let mut broken = SidebarModel::skeleton("demo");
        broken.board_loading = false;
        broken.config_error = Some("demo.yaml: bad".into());
        assert_eq!(
            broken.rows()[3..],
            [
                SidebarRow::Blank,
                SidebarRow::Section("Workspaces".into()),
                SidebarRow::ConfigError("demo.yaml: bad".into()),
            ]
        );
    }

    #[test]
    fn workspace_right_label_reads_agent_idle_or_nothing() {
        let mut row = ws_row("alpha", "hub", Some("T-1"), WorkspaceBadge::Working);
        assert_eq!(row.right_label(), "Developer");
        row.agent = None;
        row.badge = WorkspaceBadge::Idle;
        assert_eq!(row.right_label(), "idle");
        row.badge = WorkspaceBadge::Loading;
        assert_eq!(row.right_label(), "");
        assert_eq!(title_case(""), "");
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

    #[test]
    fn remote_board_cards_carry_ownership_and_only_the_users_start() {
        let mut board = vec![issue("mine", "todo"), issue("theirs", "todo"), issue("nobody", "todo")];
        board[0].tracker_assignees = vec!["octocat".into()];
        board[1].tracker_assignees = vec!["alice".into()];
        let mut input = board_input(board);
        input.ownership = shelbi_state::OwnershipGate::RequireAssignee("octocat".into());
        let model = IssuesModel::build(&input, &BoardFilters::default());
        let card = |id| model.card(id).unwrap();
        assert_eq!(card("mine").tracker_owner, Some(TrackerOwner::Me));
        assert!(card("mine").startable());
        assert_eq!(
            card("theirs").tracker_owner,
            Some(TrackerOwner::Other(vec!["alice".into()]))
        );
        assert!(!card("theirs").startable());
        let why = card("theirs").start_refusal().unwrap();
        assert!(why.contains("assigned to @alice"), "{why}");
        assert!(why.contains("shelbi issue start theirs --take"), "{why}");
        assert_eq!(card("nobody").tracker_owner, Some(TrackerOwner::Unassigned));
        assert!(!card("nobody").startable());
        // The detail agrees with the card.
        let detail = TaskDetailModel::build(&input, "theirs").unwrap();
        assert_eq!(detail.tracker_owner, card("theirs").tracker_owner);
        assert!(!detail.startable());
    }

    #[test]
    fn filesystem_board_cards_have_no_ownership_and_all_start() {
        let mut board = vec![issue("a", "todo")];
        board[0].tracker_assignees = vec!["alice".into()];
        let model = IssuesModel::build(&board_input(board), &BoardFilters::default());
        let card = model.card("a").unwrap();
        assert_eq!(card.tracker_owner, None);
        assert!(card.startable());
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
    fn issues_board_search_narrows_cards_on_top_of_the_other_filters() {
        let mut board = vec![
            issue_full("T-1", "todo", &[], Some("alpha")),
            issue_full("T-2", "todo", &[], Some("beta")),
            issue_full("T-3", "todo", &[], Some("alpha")),
        ];
        board[0].task.title = "Restyle the command palette".into();
        board[1].task.title = "Palette keyboard shortcuts".into();
        board[2].task.branch = Some("jlong/palette-search".into());
        let input = board_input(board);
        let ids = |filters: &BoardFilters| -> Vec<String> {
            let model = IssuesModel::build(&input, filters);
            model.columns[1].cards.iter().map(|c| c.id.clone()).collect()
        };
        let mut filters = BoardFilters {
            search: "palette".into(),
            ..BoardFilters::default()
        };
        // Title (either case) and branch both match.
        assert_eq!(ids(&filters), ["T-1", "T-2", "T-3"]);
        filters.search = "t-2".into();
        assert_eq!(ids(&filters), ["T-2"]);
        // Combined with the workspace filter: only alpha's palette cards.
        filters.search = "PALETTE".into();
        filters.workspace = Some(WorkspaceFilter::Workspace("alpha".into()));
        assert_eq!(ids(&filters), ["T-1", "T-3"]);
        filters.search = "keyboard".into();
        assert!(ids(&filters).is_empty());
        filters.search.clear();
        assert_eq!(ids(&filters), ["T-1", "T-3"]);
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

    fn machine(name: &str, kind: MachineKind, host: Option<&str>) -> Machine {
        Machine {
            name: name.into(),
            kind,
            work_dir: "/tmp".into(),
            host: host.map(str::to_string),
            tags: vec![],
            forward: None,
        }
    }

    fn ws_spec(name: &str, machine: &str) -> WorkspaceSpec {
        WorkspaceSpec {
            name: name.into(),
            machine: machine.into(),
            tags: vec![],
            slot: None,
        }
    }

    fn status(ws: &str, state: WorkspaceState, task: Option<&str>) -> WorkspaceStatus {
        WorkspaceStatus {
            workspace: ws.into(),
            current_task: task.map(str::to_string),
            state,
            last_transition: chrono::Utc::now(),
            last_seen: chrono::Utc::now(),
        }
    }

    /// A local machine with a dev slot (working a task whose `agent:` is set)
    /// and a review slot serving a review task; a remote with one idle slot.
    fn machines_input() -> MachinesInput {
        let mut working = issue_full("t1", "in-progress", &[], Some("alpha"));
        working
            .task
            .params
            .insert("agent".into(), "qa".into());
        let mut records = HashMap::new();
        records.insert(
            "gpu".to_string(),
            MachineRecord {
                path: "/usr/local/bin/shelbi".into(),
                version: "0.10.0".into(),
                compatible: true,
                source: "path".into(),
                checked_at: chrono::Utc::now(),
                host: None,
            },
        );
        let mut statuses = HashMap::new();
        statuses.insert(
            "alpha".to_string(),
            status("alpha", WorkspaceState::AwaitingInput, Some("t1")),
        );
        let mut review_slots = HashMap::new();
        review_slots.insert(
            "review".to_string(),
            ReviewSlot {
                task_id: "r9".into(),
                state: ReviewState::Serving,
                url: Some("http://localhost:3001".into()),
            },
        );
        let mut branches = HashMap::new();
        branches.insert("t1".to_string(), "jlong/t1".to_string());
        branches.insert("r9".to_string(), "jlong/r9".to_string());
        let mut titles = HashMap::new();
        titles.insert("r9".to_string(), "Review me".to_string());
        let mut runners = HashMap::new();
        runners.insert("t1".to_string(), "Codex".to_string());
        runners.insert("r9".to_string(), "Claude".to_string());
        MachinesInput {
            display_name: Some("Demo".into()),
            machines: vec![
                machine("local", MachineKind::Local, None),
                machine("gpu", MachineKind::Ssh, Some("gpu.lan")),
                machine("box", MachineKind::Ssh, None),
            ],
            workspaces: vec![
                (ws_spec("alpha", "local"), false),
                (ws_spec("review", "local"), true),
                (ws_spec("gpu-1", "gpu"), false),
            ],
            records,
            statuses,
            in_progress: vec![working],
            review_slots,
            branches,
            titles,
            runners,
        }
    }

    #[test]
    fn machines_group_every_workspace_under_its_machine() {
        let model = MachinesModel::build(&machines_input());
        let names: Vec<(&str, Vec<&str>)> = model
            .machines
            .iter()
            .map(|m| {
                (
                    m.name.as_str(),
                    m.workspaces.iter().map(|w| w.name.as_str()).collect(),
                )
            })
            .collect();
        // Declared order, review slots included, a machine with none kept.
        assert_eq!(
            names,
            vec![
                ("local", vec!["alpha", "review"]),
                ("gpu", vec!["gpu-1"]),
                ("box", vec![]),
            ]
        );
        assert_eq!(model.display_name.as_deref(), Some("Demo"));
        assert_eq!(model.workspace_count(), 3);
        assert_eq!(model.workspace_positions(), vec![(0, 0), (0, 1), (1, 0)]);
        let local = &model.machines[0];
        assert!(local.is_local);
        assert_eq!(local.reachability, Reachability::Reachable, "the hub is never probed");
        assert!(local.remote.is_none());
        let gpu = &model.machines[1];
        assert_eq!(gpu.reachability, Reachability::Unknown, "checking until probed");
        assert_eq!(gpu.remote.as_ref().map(|r| r.version.as_str()), Some("0.10.0"));
        assert!(model.machines[2].remote.is_none(), "not set up yet");
    }

    #[test]
    fn machines_rows_carry_workspace_list_state_agent_task_branch_and_review() {
        let model = MachinesModel::build(&machines_input());
        let alpha = &model.machines[0].workspaces[0];
        assert_eq!(alpha.assignment.cell(), "in_progress: t1");
        assert_eq!(alpha.agent.as_deref(), Some("qa"), "the task's agent: wins");
        assert_eq!(alpha.task_title.as_deref(), Some("Task t1"));
        assert_eq!(alpha.branch.as_deref(), Some("jlong/t1"));
        assert_eq!(alpha.state, Some(WorkspaceState::AwaitingInput));
        assert!(alpha.review.is_none());
        assert_eq!(alpha.runner.as_deref(), Some("Codex"));
        assert_eq!(alpha.task_cell(), "in_progress: Task t1");

        let review = &model.machines[0].workspaces[1];
        assert!(review.is_review_slot);
        assert_eq!(review.assignment.cell(), "review: r9");
        assert_eq!(review.agent.as_deref(), Some(REVIEW_SLOT_AGENT));
        assert_eq!(review.task_title.as_deref(), Some("Review me"));
        assert_eq!(review.branch.as_deref(), Some("jlong/r9"));
        assert_eq!(review.runner.as_deref(), Some("Claude"));
        assert_eq!(review.task_cell(), "review: Review me");
        let slot = review.review.as_ref().expect("a review slot");
        assert_eq!(slot.label(), "serving");
        assert_eq!(slot.url.as_deref(), Some("http://localhost:3001"));

        let idle = &model.machines[1].workspaces[0];
        assert_eq!(idle.assignment, SlotAssignment::Idle);
        assert_eq!(idle.assignment.cell(), "idle");
        assert!(idle.agent.is_none() && idle.branch.is_none() && idle.state.is_none());
        assert!(idle.runner.is_none());
        assert_eq!(idle.task_cell(), "idle");
    }

    #[test]
    fn machines_default_agent_and_diverged_assignments_match_workspace_list() {
        let mut input = machines_input();
        input.in_progress[0].task.params.clear();
        input
            .in_progress
            .push(issue_full("t2", "in-progress", &[], Some("alpha")));
        let model = MachinesModel::build(&input);
        let alpha = &model.machines[0].workspaces[0];
        assert_eq!(alpha.agent.as_deref(), Some(DEFAULT_TASK_AGENT));
        assert_eq!(alpha.assignment.cell(), "in_progress: t1, t2");
        assert_eq!(alpha.task_cell(), "in_progress: t1, t2", "no one title for two tasks");
    }

    #[test]
    fn machines_probe_remotes_only_and_fold_their_results() {
        let mut model = MachinesModel::build(&machines_input());
        assert_eq!(
            model.probe_targets(),
            vec![
                ("gpu".to_string(), "gpu.lan".to_string()),
                // An ssh machine with no host is reached by its name.
                ("box".to_string(), "box".to_string()),
            ]
        );
        let mut results = HashMap::new();
        results.insert(
            "gpu".to_string(),
            Reachability::Unreachable {
                error: "ssh: connect to host gpu.lan port 22: Operation timed out".into(),
            },
        );
        // A stray result for the local machine is ignored.
        results.insert(
            "local".to_string(),
            Reachability::Unreachable { error: "x".into() },
        );
        model.fold_reachability(&results);
        assert_eq!(model.machines[0].reachability, Reachability::Reachable);
        assert!(matches!(
            model.machines[1].reachability,
            Reachability::Unreachable { .. }
        ));
        assert_eq!(model.machines[2].reachability, Reachability::Unknown);
    }

    #[test]
    fn machines_labels_read_like_the_tui() {
        assert_eq!(
            reachability_label(&Reachability::Unknown, 40),
            ("○ checking…".to_string(), Tone::DarkGray)
        );
        assert_eq!(
            reachability_label(&Reachability::Reachable, 40),
            ("● reachable".to_string(), Tone::Green)
        );
        let (text, tone) = reachability_label(
            &Reachability::Unreachable {
                error: "a very long ssh failure reason".into(),
            },
            10,
        );
        assert_eq!(text, "● unreachable: a very lo…");
        assert_eq!(tone, Tone::Red);
        assert_eq!(
            remote_binary_label(None).0,
            "not set up — run `shelbi machine setup`"
        );
        assert_eq!(workspace_state_badge(None), ("idle", Tone::DarkGray));
        assert_eq!(
            workspace_state_badge(Some(WorkspaceState::Serving)),
            ("serving", Tone::Cyan)
        );
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

    fn header_row(task: Option<&str>, badge: WorkspaceBadge, remote: bool) -> WorkspaceRow {
        WorkspaceRow {
            name: "alpha".into(),
            machine: if remote { "gpu-box" } else { "hub" }.into(),
            is_remote: remote,
            current_task: task.map(str::to_string),
            agent: task.map(|_| "developer".to_string()),
            badge,
        }
    }

    fn details(task: Option<(&str, &str)>, branch: Option<&str>) -> WorkspaceDetailsInfo {
        WorkspaceDetailsInfo {
            task: task.map(|(id, title)| (id.to_string(), title.to_string())),
            branch: branch.map(str::to_string),
            ..WorkspaceDetailsInfo::default()
        }
    }

    #[test]
    fn a_working_workspace_header_shows_task_branch_machine_and_state() {
        let row = header_row(Some("cold-start"), WorkspaceBadge::Working, false);
        let d = details(Some(("cold-start", "Cold-start cache")), Some("jlong/cold-start"));
        let h = WorkspaceHeader::build(&row, Some(&d));
        assert_eq!(h.task_line().as_deref(), Some("cold-start  Cold-start cache"));
        assert_eq!(h.location_line(), "hub · jlong/cold-start");
        assert_eq!(h.agent.as_deref(), Some("Developer"));
        assert_eq!(h.state, WorkspaceBadge::Working);
        assert_eq!(h.state.label(), "Working");
        assert_eq!(h.state_decoration().glyph, "⏵");
        assert!(!WorkspaceHeader::needs_details(&row, Some(&d)));
    }

    #[test]
    fn the_header_shows_the_id_alone_until_the_title_resolves() {
        let row = header_row(Some("cold-start"), WorkspaceBadge::AwaitingInput, false);
        let h = WorkspaceHeader::build(&row, None);
        assert_eq!(h.task_line().as_deref(), Some("cold-start"));
        assert_eq!(h.location_line(), "hub");
        assert_eq!(h.state.label(), "Awaiting input");
        assert!(WorkspaceHeader::needs_details(&row, None));
    }

    #[test]
    fn details_for_a_previous_task_never_label_a_new_one() {
        // The row moved on to a new task; the details still describe the old.
        let row = header_row(Some("next-task"), WorkspaceBadge::Working, false);
        let stale = details(Some(("cold-start", "Cold-start cache")), Some("jlong/cold-start"));
        let h = WorkspaceHeader::build(&row, Some(&stale));
        assert_eq!(h.task_line().as_deref(), Some("next-task"));
        assert_eq!(h.branch, None, "the old task's branch is not shown");
        assert!(WorkspaceHeader::needs_details(&row, Some(&stale)));

        // A handoff idled the row: the old task's title and branch go too.
        let idle = header_row(None, WorkspaceBadge::Idle, false);
        let h = WorkspaceHeader::build(&idle, Some(&stale));
        assert_eq!(h.task_line(), None);
        assert_eq!(h.branch, None);
        assert_eq!(h.state.label(), "Idle");
        assert!(WorkspaceHeader::needs_details(&idle, Some(&stale)));
    }

    #[test]
    fn an_idle_workspace_header_shows_its_worktree_branch() {
        let row = header_row(None, WorkspaceBadge::Idle, false);
        let d = details(None, Some("main"));
        let h = WorkspaceHeader::build(&row, Some(&d));
        assert_eq!(h.task_line(), None);
        assert_eq!(h.agent, None);
        assert_eq!(h.location_line(), "hub · main");
        assert!(!WorkspaceHeader::needs_details(&row, Some(&d)));
    }

    #[test]
    fn a_remote_workspace_header_names_its_machine() {
        let row = header_row(Some("t-9"), WorkspaceBadge::AwaitingPermission, true);
        let d = details(Some(("t-9", "Ship it")), Some("jlong/t-9"));
        let h = WorkspaceHeader::build(&row, Some(&d));
        assert_eq!(h.location_line(), "gpu-box (remote) · jlong/t-9");
        assert_eq!(h.state.label(), "Needs permission");
        assert_eq!(h.state_decoration().color, DecorationColor::Red);
    }
}
