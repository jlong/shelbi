use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use shelbi_core::{Agent, Column, Status};
use shelbi_palette::{Decoration, DecorationColor};
use shelbi_state::{
    keymap::{DisplayStyle, Keymaps},
    load_workspace_status, read_state, sidebar_collapsed_machines, toggle_sidebar_machine_collapsed,
    TaskFile, WorkspaceState, ZenModeState, ZenToggleChord,
};

/// What's currently highlighted in the sidebar — drives selection logic
/// only; the right pane (orchestrator / agent) is a real tmux pane, not
/// rendered by this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// One of the built-in views hosted as a hidden tmux pane: swap it
    /// into the dashboard's right slot. Sidebar nav uses `"orch"` and
    /// `"tasks"`; the orchestrator can still serve `"review"` /
    /// `"machines"` for callers that hold onto pane ids directly.
    Builtin(&'static str),
    /// A declared workspace (from project YAML) — switch tmux to its pane
    /// (local: window in the project session; remote: a proxy window in
    /// the project session that ssh-attaches to the workspace's remote session).
    Workspace(String),
    /// A legacy `shelbi spawn` agent — switch tmux to its window. Workspaces
    /// from the modern task-board flow surface as [`View::Workspace`] instead.
    Agent(String),
    /// A task in the review queue — trigger the review checkout flow and
    /// focus the review pane.
    ReviewTask(String),
}

/// Sidebar view of a declared workspace — the bits we need to render and
/// activate it. Built fresh each refresh from the project YAML + the
/// in-progress task column.
#[derive(Debug, Clone)]
pub struct WorkspaceOverview {
    pub name: String,
    pub machine: String,
    pub is_remote: bool,
    /// `Some(task_id)` if this workspace is currently assigned an in_progress
    /// task — drives the busy/idle indicator.
    pub current_task: Option<String>,
    /// Single-char state glyph derived from the workspace's status file and
    /// the task board state.
    pub badge: WorkspaceBadge,
    /// Name of the agent loaded into this workspace's pane. Resolved from
    /// the assigned task's frontmatter `agent:` (falling back to the
    /// project's default task agent) — same lookup `shelbi workspace list`
    /// uses for its AGENT column. `None` when the workspace is idle (no
    /// in-progress task assigned), in which case the sidebar renders the
    /// "idle" placeholder instead.
    pub agent: Option<String>,
}

/// Per-workspace state glyph shown in the sidebar. Derived each refresh from
/// the task board (review-ready / idle) and from
/// `~/.shelbi/workspaces/<name>/status.yaml` (working / awaiting-input /
/// awaiting-permission), which the [`crate::WorkspacePoller`] writes from the
/// workspace pane's `shelbi:<state>` title marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceBadge {
    /// ⏵ — claude is actively running a turn.
    Working,
    /// 💬 — claude finished a turn and is sitting at the prompt.
    AwaitingInput,
    /// ⚠ — claude is showing a permission dialog.
    AwaitingPermission,
    /// ✓ — the workspace's task has been moved to the review column.
    ReviewReady,
    /// · — no in-flight task assigned.
    Idle,
}

impl WorkspaceBadge {
    /// Single-char glyph — paired with one trailing space in the renderer
    /// so the badge column stays narrow on small terminals.
    pub fn glyph(self) -> &'static str {
        match self {
            WorkspaceBadge::Working => "⏵",
            WorkspaceBadge::AwaitingInput => "💬",
            WorkspaceBadge::AwaitingPermission => "⚠",
            WorkspaceBadge::ReviewReady => "✓",
            WorkspaceBadge::Idle => "·",
        }
    }

    /// Color the glyph paints in. Shared by the sidebar render path and
    /// the palette so a workspace row gets the same tint in both surfaces.
    pub fn decoration_color(self) -> DecorationColor {
        match self {
            WorkspaceBadge::Working => DecorationColor::Green,
            WorkspaceBadge::AwaitingInput => DecorationColor::Yellow,
            WorkspaceBadge::AwaitingPermission => DecorationColor::Red,
            WorkspaceBadge::ReviewReady => DecorationColor::Cyan,
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

/// Color the legacy-agent status glyph paints in. Pairs with
/// [`shelbi_core::Status::glyph`] to form a [`Decoration`]; sidebar and
/// palette both consume this so a `Running` agent renders green in both.
pub fn status_decoration_color(s: shelbi_core::Status) -> DecorationColor {
    use shelbi_core::Status::*;
    match s {
        Running => DecorationColor::Green,
        Waiting => DecorationColor::Yellow,
        Queued => DecorationColor::Blue,
        Done => DecorationColor::Cyan,
        Error => DecorationColor::Red,
        Archived => DecorationColor::DarkGray,
    }
}

pub fn status_decoration(s: shelbi_core::Status) -> Decoration {
    Decoration {
        glyph: s.glyph().to_string(),
        color: status_decoration_color(s),
    }
}

pub struct App {
    pub project_name: String,
    pub agents: Vec<Agent>,
    pub workspaces: Vec<WorkspaceOverview>,
    pub review_queue: Vec<TaskFile>,
    pub sidebar_index: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    pub should_quit: bool,
    /// Latest Zen Mode state read from `state.json`. Drives the green pill
    /// in the lower-left status block and the Alt+Z toggle direction.
    pub zen_mode: ZenModeState,
    /// Chord that toggles Zen Mode — resolved from
    /// `keys.yml::defaults.global.zen_toggle` via
    /// [`Keymaps::zen_toggle_chord`], falling back to the legacy
    /// `~/.shelbi/config.yaml::keymap.zen_toggle` for chords the
    /// four-value preset enum can't represent. Defaults to Alt+Z before
    /// any of those layers have been read.
    ///
    /// [`Keymaps::zen_toggle_chord`]: shelbi_state::keymap::Keymaps::zen_toggle_chord
    pub zen_toggle_chord: ZenToggleChord,
    /// Merged keymaps for every TUI mode — populated once at startup by
    /// `run_sidebar` calling [`shelbi_state::keymap::load_keymaps`]. The
    /// sidebar handler dispatches `KeyEvent`s through `keymaps.global` then
    /// `keymaps.sidebar`. Default is empty bindings; callers that exercise
    /// the handler (entry points, tests) replace this before dispatching.
    pub keymaps: Keymaps,
    /// Platform convention for rendering chord hints — detected once here
    /// at construction so per-frame footer rendering never re-probes the
    /// host OS. Read via [`App::display_style`].
    pub display_style: DisplayStyle,
    /// Screen-space rect occupied by the rendered row list — written each
    /// frame by the sidebar renderer and read by the mouse-click handler to
    /// map a click coordinate back to a row index.
    pub list_area: Rect,
    /// Names of machines the user has collapsed in the Workspaces tree.
    /// Mirrored to `~/.shelbi/state.json::sidebar.collapsed_machines` so
    /// the choice survives a sidebar respawn and follows the user across
    /// projects that share a machine name. Loaded once at refresh; the
    /// toggle path mutates this set and writes to disk in one shot.
    /// Stale entries (machine names not declared in the current project)
    /// are silently ignored at row-build time — no error, no warning.
    pub collapsed_machines: BTreeSet<String>,
}

impl App {
    pub fn new_sidebar(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            agents: Vec::new(),
            workspaces: Vec::new(),
            review_queue: Vec::new(),
            sidebar_index: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            should_quit: false,
            zen_mode: ZenModeState::Off,
            zen_toggle_chord: ZenToggleChord::AltZ,
            keymaps: Keymaps::default(),
            display_style: DisplayStyle::detect(),
            list_area: Rect::default(),
            collapsed_machines: BTreeSet::new(),
        }
    }

    /// Borrow the keymaps populated at startup by `run_sidebar`. The
    /// sidebar handler reads this once per loop entry rather than
    /// re-parsing `keys.yml` per keystroke.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// The cached host-platform chord-display convention. Detected once at
    /// construction; help rendering reads this rather than calling
    /// [`DisplayStyle::detect`] per frame.
    pub fn display_style(&self) -> DisplayStyle {
        self.display_style
    }

    /// Translate a click in terminal coordinates into a row index, if the
    /// point lands inside the rendered list and on a valid row.
    pub fn row_at(&self, column: u16, row: u16) -> Option<usize> {
        let area = self.list_area;
        if area.width == 0 || area.height == 0 {
            return None;
        }
        if column < area.x || column >= area.x.saturating_add(area.width) {
            return None;
        }
        if row < area.y || row >= area.y.saturating_add(area.height) {
            return None;
        }
        let idx = (row - area.y) as usize;
        let rows = self.rows();
        rows.get(idx).filter(|r| r.is_selectable()).map(|_| idx)
    }

    /// Sidebar rows: a fixed 3-item nav (Chat / Tasks / Activity), then
    /// declared workspaces under an `— agents —` separator, then the review
    /// queue under `— Ready for Review —`, then any legacy `shelbi spawn`
    /// agents under `— spawned —`. Each section header and its rows are
    /// dropped together when that group is empty — Review is intentionally
    /// not a destination view, only an inline live list. The Ctrl+P
    /// palette mirrors this same set of rows for fuzzy access.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![
            Row::Nav {
                icon: "💬",
                label: "Chat",
                view: View::Builtin("orch"),
            },
            Row::Nav {
                icon: "📋",
                label: "Tasks",
                view: View::Builtin("tasks"),
            },
            Row::Nav {
                icon: "⚡",
                label: "Activity",
                view: View::Builtin("activity"),
            },
        ];
        // Every list section header gets exactly one blank line above it,
        // regardless of position, so all section breaks render as the same
        // uniform gap.
        if !self.workspaces.is_empty() {
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "Workspaces".into(),
            });
            // Group workspaces by their machine when the project declares
            // more than one machine — header per machine in declaration
            // order, each workspace indented underneath. A single-machine
            // project collapses to a flat list (no group header, no
            // indent) so the sidebar stays compact when grouping carries
            // no information.
            let machines: Vec<&str> = {
                let mut seen: Vec<&str> = Vec::new();
                for w in &self.workspaces {
                    if !seen.iter().any(|m| *m == w.machine) {
                        seen.push(&w.machine);
                    }
                }
                seen
            };
            let grouped = machines.len() > 1;
            if grouped {
                for machine in &machines {
                    let machine_str = *machine;
                    let on_machine: Vec<&WorkspaceOverview> = self
                        .workspaces
                        .iter()
                        .filter(|w| w.machine == machine_str)
                        .collect();
                    let total = on_machine.len();
                    // "Active" = the workspace is currently running a task
                    // (`current_task.is_some()`). Review-ready workspaces
                    // have no in-progress task assigned, so they don't
                    // count — matches the wireframe's "agent loaded vs
                    // idle" reading of the workspace row.
                    let active = on_machine
                        .iter()
                        .filter(|w| w.current_task.is_some())
                        .count();
                    let collapsed = self.collapsed_machines.contains(machine_str);
                    rows.push(Row::MachineGroup {
                        name: machine_str.to_string(),
                        collapsed,
                        total,
                        active,
                    });
                    if collapsed {
                        // Hide the workspace rows beneath. The header row
                        // carries the count suffix so the user still sees
                        // overall capacity at a glance.
                        continue;
                    }
                    for w in on_machine {
                        rows.push(Row::Workspace {
                            name: w.name.clone(),
                            badge: w.badge,
                            agent: w.agent.clone(),
                            indent: true,
                            view: View::Workspace(w.name.clone()),
                        });
                    }
                }
            } else {
                for w in &self.workspaces {
                    rows.push(Row::Workspace {
                        name: w.name.clone(),
                        badge: w.badge,
                        agent: w.agent.clone(),
                        indent: false,
                        view: View::Workspace(w.name.clone()),
                    });
                }
            }
        }
        if !self.review_queue.is_empty() {
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "Ready for Review".into(),
            });
            for tf in &self.review_queue {
                rows.push(Row::Review {
                    title: tf.task.title.clone(),
                    workspace: tf.task.assigned_to.clone(),
                    view: View::ReviewTask(tf.task.id.clone()),
                });
            }
        }
        if !self.agents.is_empty() {
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "spawned".into(),
            });
            for a in &self.agents {
                rows.push(Row::LegacyAgent {
                    id: a.id.clone(),
                    machine: a.machine.clone(),
                    status: a.status,
                    view: View::Agent(a.id.clone()),
                });
            }
        }
        rows
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.agents = load_agents(&self.project_name).unwrap_or_default();
        self.review_queue =
            shelbi_state::list_column(&self.project_name, Column::Review).unwrap_or_default();
        self.workspaces =
            load_workspaces(&self.project_name, &self.review_queue).unwrap_or_default();
        // A missing state.json is normal (fresh project): default to Off so
        // the pill stays hidden rather than flashing then disappearing.
        self.zen_mode = read_state(&self.project_name)
            .map(|s| s.zen_mode)
            .unwrap_or(ZenModeState::Off);
        // Refresh the cached collapse set from disk. Missing
        // `~/.shelbi/state.json` is normal on a fresh install — default
        // to empty (every machine expanded) without surfacing an error.
        self.collapsed_machines = sidebar_collapsed_machines().unwrap_or_default();
        self.last_refresh = Instant::now();
        Ok(())
    }

    pub fn maybe_refresh(&mut self) -> Result<()> {
        if self.last_refresh.elapsed() >= Duration::from_millis(750) {
            self.refresh()?;
        }
        Ok(())
    }

    pub fn nav_up(&mut self) {
        self.step_selection(-1);
    }

    pub fn nav_down(&mut self) {
        self.step_selection(1);
    }

    /// Walk the selection by `delta` (±1), skipping non-selectable rows
    /// (section headers) and wrapping at either end. Caps at one full
    /// cycle if no selectable row exists.
    fn step_selection(&mut self, delta: i32) {
        let rows = self.rows();
        let n = rows.len();
        if n == 0 {
            return;
        }
        let mut idx = self.sidebar_index.min(n - 1);
        for _ in 0..n {
            idx = if delta < 0 {
                if idx == 0 {
                    n - 1
                } else {
                    idx - 1
                }
            } else {
                (idx + 1) % n
            };
            if rows[idx].is_selectable() {
                self.sidebar_index = idx;
                return;
            }
        }
    }

    /// Act on the currently highlighted row: tmux-select the matching
    /// window (orchestrator → dashboard's right pane; agent → its window)
    /// or, when the row is a [`Row::MachineGroup`], toggle the machine's
    /// collapse state. Space and Enter both route here, so a user can
    /// fold/unfold a machine without leaving the keyboard.
    pub fn activate_selection(&mut self) {
        let Some(row) = self.rows().get(self.sidebar_index).cloned() else {
            return;
        };
        if let Row::MachineGroup { name, .. } = &row {
            self.toggle_machine_collapsed(name);
            return;
        }
        if let Some(view) = row.view().cloned() {
            self.activate_view(&view);
        }
    }

    /// Flip the collapse state for `machine` in the sidebar tree. Writes
    /// the new state to `~/.shelbi/state.json` and mirrors it into the
    /// in-memory cache so the next render reflects the toggle without an
    /// extra refresh tick. A disk failure surfaces in the status line —
    /// the cache is not updated in that case so the row keeps reading
    /// what's on disk.
    pub fn toggle_machine_collapsed(&mut self, machine: &str) {
        match toggle_sidebar_machine_collapsed(machine) {
            Ok(true) => {
                self.collapsed_machines.insert(machine.to_string());
            }
            Ok(false) => {
                self.collapsed_machines.remove(machine);
            }
            Err(e) => {
                self.status_line = format!("collapse `{machine}` failed: {e}");
            }
        }
    }

    pub fn activate_view(&mut self, view: &View) {
        match view {
            View::Builtin(name) => match shelbi_orchestrator::show_view(&self.project_name, name) {
                Ok(()) => self.status_line = format!("▶ {name}"),
                Err(e) => self.status_line = format!("show view `{name}` failed: {e}"),
            },
            View::Workspace(name) => {
                match shelbi_orchestrator::focus_workspace(&self.project_name, name) {
                    Ok(()) => self.status_line = format!("▶ {name}"),
                    Err(e) => self.status_line = format!("focus `{name}` failed: {e}"),
                }
            }
            View::Agent(id) => {
                let target = format!("shelbi-{}:{}", self.project_name, id);
                let out = run_tmux(["select-window", "-t", &target]);
                if !out {
                    self.status_line = format!(
                        "couldn't switch to `{id}` — window not in this session \
                         (remote workspaces need `tmux attach -t shelbi-w-{id}` for now)"
                    );
                } else {
                    self.status_line = format!("▶ {id}");
                }
            }
            View::ReviewTask(id) => match self.start_review(id) {
                Ok(focus_target) => {
                    let _ = run_tmux(["select-window", "-t", &focus_target]);
                    self.status_line = format!("▶ reviewing {id}");
                }
                Err(e) => self.status_line = format!("review `{id}` failed: {e}"),
            },
        }
    }

    fn start_review(&self, id: &str) -> Result<String> {
        shelbi_orchestrator::review::start_review_by_id(&self.project_name, id)
            .map_err(|e| anyhow::anyhow!(e))
    }

    /// Flip `state.json::zen_mode` between On and Off via the shared
    /// [`shelbi_state::toggle_zen_mode`] path — same read/write/log
    /// dance as the palette's "Toggle Zen Mode" entry and the CLI's
    /// `shelbi zen on|off`, just with `reason=user:hotkey` so the
    /// activity feed can tell the chord apart from the palette
    /// (`user:palette`) and the CLI (`user:cli`). Paused collapses to
    /// On because this hotkey is intentionally a two-state hop.
    pub fn toggle_zen_mode(&mut self) {
        match shelbi_state::toggle_zen_mode(&self.project_name, "user:hotkey") {
            Ok(target) => {
                self.zen_mode = target;
                let action = match target {
                    ZenModeState::On => "on",
                    ZenModeState::Off => "off",
                    ZenModeState::Paused => "pause",
                };
                self.status_line = format!("zen {action}");
            }
            Err(e) => {
                self.status_line = format!("zen toggle failed: {e}");
            }
        }
    }
}

/// One rendered line in the sidebar. Section headers are inert dividers;
/// everything else activates a view on Enter / click. Kept as an enum so
/// the renderer pattern-matches the row kind without an `Option<View>`
/// dance and tests can target one shape unambiguously.
#[derive(Clone)]
pub enum Row {
    /// Top-level destination (Chat / Tasks).
    Nav {
        icon: &'static str,
        label: &'static str,
        view: View,
    },
    /// `— label —` separator. Not selectable.
    Section { label: String },
    /// Vertical spacing between sections. Renders as an empty line and
    /// can't be selected — purely for visual rhythm.
    Blank,
    /// Machine group header inside the Workspaces section. Renders as
    /// `▾ <machine>` when expanded and `▸ <machine>   (<total>, <active>
    /// active)` when collapsed. Only emitted when the project declares
    /// more than one machine; single-machine projects skip the header
    /// entirely and have nothing to collapse. Selectable so Space/Enter
    /// on the focused row can toggle the collapse state via
    /// [`App::toggle_machine_collapsed`].
    MachineGroup {
        name: String,
        /// Whether the user has folded this machine — workspace rows
        /// under it are then suppressed from the row list. Loaded from
        /// `~/.shelbi/state.json::sidebar.collapsed_machines` via
        /// [`App::collapsed_machines`].
        collapsed: bool,
        /// Total workspaces declared on this machine. Surfaced as the
        /// first number in the `(total, active)` suffix when collapsed.
        total: usize,
        /// Workspaces currently running an in-progress task — matches
        /// the "Developer"-vs-"idle" reading of the right column on the
        /// expanded rows. Second number in the `(total, active)` suffix.
        active: usize,
    },
    /// A declared workspace, with its current state badge.
    Workspace {
        name: String,
        badge: WorkspaceBadge,
        /// Name of the agent loaded into this workspace's pane (lowercase
        /// directory name as it lives on disk; the renderer title-cases it
        /// for display). `None` means idle and the renderer surfaces the
        /// "idle" placeholder in the agent column.
        agent: Option<String>,
        /// Whether this row sits under a [`Row::MachineGroup`] header and
        /// therefore needs a leading indent. `false` in single-machine
        /// projects where the section is rendered as a flat list.
        indent: bool,
        view: View,
    },
    /// A task sitting in the review column — title + the workspace who
    /// finished it.
    Review {
        title: String,
        workspace: Option<String>,
        view: View,
    },
    /// Legacy `shelbi spawn` agent row — pre-task-board flow.
    LegacyAgent {
        id: String,
        machine: String,
        status: Status,
        view: View,
    },
}

impl Row {
    pub fn is_selectable(&self) -> bool {
        // Machine group rows are selectable now — focusing one and
        // pressing Space/Enter toggles the collapse state. Section
        // headers and blank spacers stay inert (no useful action).
        !matches!(self, Row::Section { .. } | Row::Blank)
    }

    pub fn view(&self) -> Option<&View> {
        match self {
            Row::Nav { view, .. }
            | Row::Workspace { view, .. }
            | Row::Review { view, .. }
            | Row::LegacyAgent { view, .. } => Some(view),
            Row::Section { .. } | Row::Blank | Row::MachineGroup { .. } => None,
        }
    }

    /// The icon glyph + color the row paints in. Single source of truth
    /// for both the sidebar renderer and the palette so the two surfaces
    /// can't drift on what destination shows what mark. Section headers
    /// and blank spacers have no decoration.
    pub fn decoration(&self) -> Option<Decoration> {
        match self {
            Row::Nav { icon, .. } => Some(Decoration {
                glyph: (*icon).to_string(),
                color: DecorationColor::Default,
            }),
            Row::Workspace { badge, .. } => Some(badge.decoration()),
            Row::Review { .. } => Some(Decoration {
                glyph: "✓".into(),
                color: DecorationColor::Cyan,
            }),
            Row::LegacyAgent { status, .. } => Some(status_decoration(*status)),
            Row::Section { .. } | Row::Blank | Row::MachineGroup { .. } => None,
        }
    }
}

/// Build the sidebar's view of declared workspaces from the project YAML, the
/// in-progress task column, and the review queue (passed in so we don't
/// scan tasks twice per refresh). One disk read per workspace for the
/// `status.yaml` lookup. Returns an empty vec if the project YAML or task
/// dir is missing.
fn load_workspaces(project: &str, review_queue: &[TaskFile]) -> Result<Vec<WorkspaceOverview>> {
    let p = match shelbi_state::load_project(project) {
        Ok(p) => p,
        Err(_) => return Ok(Vec::new()),
    };
    let in_progress =
        shelbi_state::list_column(project, Column::InProgress).unwrap_or_default();
    let mut out = Vec::with_capacity(p.workspaces.len());
    for workspace in &p.workspaces {
        let machine = match p.machine(&workspace.machine) {
            Some(m) => m,
            None => continue, // mis-configured workspace, skip silently
        };
        let is_remote = !machine.host().is_local();
        let assigned_task = in_progress
            .iter()
            .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()));
        let current_task = assigned_task.map(|tf| tf.task.id.clone());
        // Mirror `shelbi workspace list`'s AGENT column: take the task's
        // frontmatter `agent:` when present (matching the lookup the
        // task-start path uses to load agent instructions/skills), and
        // fall back to the project's default task agent. Idle workspaces
        // get `None` — the renderer surfaces "idle" in that case rather
        // than the default agent name.
        let agent = assigned_task.map(|tf| {
            tf.task
                .params
                .get("agent")
                .cloned()
                .unwrap_or_else(|| DEFAULT_TASK_AGENT.to_string())
        });
        let has_review = review_queue
            .iter()
            .any(|tf| tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()));
        let badge = derive_workspace_badge(&workspace.name, current_task.is_some(), has_review);
        out.push(WorkspaceOverview {
            name: workspace.name.clone(),
            machine: workspace.machine.clone(),
            is_remote,
            current_task,
            badge,
            agent,
        });
    }
    Ok(out)
}

/// Default agent surfaced when a task has no explicit `agent:` in its
/// frontmatter — matches `shelbi workspace list`'s `DEFAULT_TASK_AGENT`
/// so the sidebar and the CLI never disagree about what's loaded.
const DEFAULT_TASK_AGENT: &str = "developer";

/// Pick the badge for a workspace given the task-board signals + an on-disk
/// state read. Review-ready wins over claude state — once a task is sent
/// for review the workspace is conceptually done with it even if claude is
/// still mid-turn. Idle wins when there's no in-progress task at all, so
/// a stale `status.yaml` from a previous run doesn't show "working" for a
/// workspace that has nothing to do.
fn derive_workspace_badge(
    workspace_name: &str,
    has_in_progress: bool,
    has_review: bool,
) -> WorkspaceBadge {
    if has_review {
        return WorkspaceBadge::ReviewReady;
    }
    if !has_in_progress {
        return WorkspaceBadge::Idle;
    }
    match load_workspace_status(workspace_name).ok().flatten() {
        Some(s) => match s.state {
            WorkspaceState::Working => WorkspaceBadge::Working,
            WorkspaceState::AwaitingInput => WorkspaceBadge::AwaitingInput,
            WorkspaceState::Blocked => WorkspaceBadge::AwaitingPermission,
        },
        // Task assigned but the poller hasn't observed a marker yet. Show
        // working as the best guess — it'll firm up within one poll tick.
        None => WorkspaceBadge::Working,
    }
}

fn load_agents(project: &str) -> Result<Vec<Agent>> {
    let dir = match shelbi_state::agents_dir(project) {
        Ok(d) => d,
        Err(_) => return Ok(Vec::new()),
    };
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".log.md") || !name.ends_with(".md") {
            continue;
        }
        let id = name.trim_end_matches(".md");
        if let Ok(file) = shelbi_state::load_agent(project, id) {
            out.push(file.agent);
        }
    }
    out.sort_by_key(|a| status_order(a.status));
    Ok(out)
}

fn status_order(s: Status) -> u8 {
    match s {
        Status::Running => 0,
        Status::Waiting => 1,
        Status::Queued => 2,
        Status::Done => 3,
        Status::Error => 4,
        Status::Archived => 5,
    }
}

/// Run `tmux ARGS`. Returns true on success.
fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shelbi_core::{
        AgentRunnerSpec, Column, Machine, MachineKind, OrchestratorSpec, Project, Task, WorkspaceSpec,
    };
    use std::collections::BTreeMap;

    use crate::test_support::ENV_LOCK as TEST_LOCK;

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-tui-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn fixture_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        Project {
            name: "demo".into(),
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/demo".into(),
                    host: None,
                },
                Machine {
                    name: "devbox".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/demo".into(),
                    host: Some("devbox.local".into()),
                },
            ],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    role: Default::default(),
                },
                WorkspaceSpec {
                    name: "delta".into(),
                    machine: "devbox".into(),
                    runner: "claude".into(),
                    role: Default::default(),
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn load_workspaces_surfaces_local_and_remote_with_in_progress_task() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        let assigned = Task {
            id: "fix-thing".into(),
            title: "Fix the thing".into(),
            column: Column::InProgress,
            priority: 0,
            assigned_to: Some("delta".into()),
            workflow: None,
            branch: Some("shelbi/fix-thing".into()),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        };
        shelbi_state::save_task("demo", &assigned, "# task").unwrap();

        let workspaces = load_workspaces("demo", &[]).unwrap();
        assert_eq!(workspaces.len(), 2);

        let alpha = &workspaces[0];
        assert_eq!(alpha.name, "alpha");
        assert_eq!(alpha.machine, "hub");
        assert!(!alpha.is_remote);
        assert!(alpha.current_task.is_none());

        let delta = &workspaces[1];
        assert_eq!(delta.name, "delta");
        assert_eq!(delta.machine, "devbox");
        assert!(delta.is_remote, "ssh-machine workspaces must report is_remote=true");
        assert_eq!(delta.current_task.as_deref(), Some("fix-thing"));
        // Default agent — the task carries no `agent:` in params, so the
        // sidebar surfaces the project's default task agent verbatim.
        assert_eq!(delta.agent.as_deref(), Some("developer"));
        assert!(alpha.agent.is_none(), "idle workspaces must not carry an agent name");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn rows_include_workspaces_with_idle_and_working_badges() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "wip".into(),
                title: "wip".into(),
                column: Column::InProgress,
                priority: 0,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();

        // 3 nav + 1 blank spacer + 1 `Workspaces` section header + 2
        // machine-group headers (one per declared machine, in declaration
        // order) + 2 workspaces = 9 rows.
        let rows = app.rows();
        assert_eq!(rows.len(), 9);
        assert!(matches!(&rows[3], Row::Blank));
        assert!(matches!(&rows[4], Row::Section { label } if label == "Workspaces"));
        // Machine group headers appear in `project.yaml` declaration
        // order (hub before devbox) — not workspace order, so a project
        // that flips the workspace list still renders machines top-down
        // in the order they were declared.
        assert!(matches!(&rows[5], Row::MachineGroup { name, .. } if name == "hub"));
        assert!(matches!(&rows[7], Row::MachineGroup { name, .. } if name == "devbox"));

        // alpha (busy, no status file yet) — default to Working.
        assert_eq!(find_workspace_badge(&rows, "alpha").unwrap(), WorkspaceBadge::Working);
        // delta (idle remote) — Idle.
        assert_eq!(find_workspace_badge(&rows, "delta").unwrap(), WorkspaceBadge::Idle);

        std::env::remove_var("SHELBI_HOME");
    }

    fn find_workspace_badge(rows: &[Row], name: &str) -> Option<WorkspaceBadge> {
        rows.iter().find_map(|r| match r {
            Row::Workspace { name: n, badge, .. } if n == name => Some(*badge),
            _ => None,
        })
    }

    fn find_workspace_row<'a>(rows: &'a [Row], name: &str) -> Option<&'a Row> {
        rows.iter().find(|r| match r {
            Row::Workspace { name: n, .. } => n == name,
            _ => false,
        })
    }

    /// Single-machine fixture — the second machine and its workspace are
    /// stripped from [`fixture_project`] so the grouped-vs-flat rendering
    /// path tests can exercise the collapsed-header branch.
    fn single_machine_fixture() -> Project {
        let mut p = fixture_project();
        p.machines.retain(|m| m.name == "hub");
        p.workspaces.retain(|w| w.machine == "hub");
        p
    }

    #[test]
    fn single_machine_project_renders_workspaces_as_flat_list_without_group_header() {
        // When only one machine is declared, the `▾ <machine>` group
        // header is suppressed and rows are emitted at the section root
        // without indent — grouping carries no information so the sidebar
        // stays compact. Adding a second machine flips the layout into
        // the grouped form (covered by [`rows_include_workspaces_with_idle_and_working_badges`]).
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = single_machine_fixture();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();

        let rows = app.rows();
        assert!(
            !rows.iter().any(|r| matches!(r, Row::MachineGroup { .. })),
            "single-machine project must not emit any machine group headers"
        );
        // 3 nav + 1 blank + 1 Workspaces section + 1 workspace = 6 rows.
        assert_eq!(rows.len(), 6);
        let alpha = find_workspace_row(&rows, "alpha").expect("alpha must render at the section root");
        assert!(
            matches!(alpha, Row::Workspace { indent: false, .. }),
            "flat-list rows render without indent"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn machine_group_carries_collapse_state_and_counts() {
        // Multi-machine project, one assigned task → hub has 1 active /
        // 1 total (alpha), devbox has 0 active / 1 total (delta idle).
        // Both machines render expanded by default; the row carries the
        // counts even when expanded so the renderer can decide whether
        // to surface them.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "wip".into(),
                title: "wip".into(),
                column: Column::InProgress,
                priority: 0,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        let hub = rows
            .iter()
            .find(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .expect("hub group must render");
        let devbox = rows
            .iter()
            .find(|r| matches!(r, Row::MachineGroup { name, .. } if name == "devbox"))
            .expect("devbox group must render");
        assert!(matches!(
            hub,
            Row::MachineGroup { collapsed: false, total: 1, active: 1, .. }
        ));
        assert!(matches!(
            devbox,
            Row::MachineGroup { collapsed: false, total: 1, active: 0, .. }
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn toggle_machine_collapse_hides_workspace_rows_and_persists_to_state() {
        // Activate on a focused machine row toggles its collapse state.
        // While collapsed, workspace rows under it are dropped from
        // `App::rows`; expanding re-emits them. The choice persists to
        // `~/.shelbi/state.json::sidebar.collapsed_machines`, so a
        // fresh `App` reads it back on first refresh — that's the
        // `shelbi reload` survival path the spec calls out.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        // Pre-condition: hub is expanded, alpha is rendered under it.
        assert!(find_workspace_row(&app.rows(), "alpha").is_some());

        // Toggle hub → collapsed. The header carries the new flag and
        // the workspace row beneath it disappears.
        app.toggle_machine_collapsed("hub");
        let rows = app.rows();
        let hub = rows
            .iter()
            .find(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .expect("hub header still renders when collapsed");
        assert!(matches!(hub, Row::MachineGroup { collapsed: true, .. }));
        assert!(
            find_workspace_row(&rows, "alpha").is_none(),
            "collapsed hub must hide its workspace rows"
        );
        // devbox is unaffected — collapsing one machine doesn't fold
        // the other.
        assert!(
            find_workspace_row(&rows, "delta").is_some(),
            "devbox stays expanded"
        );

        // Persisted to ~/.shelbi/state.json — a brand-new App reads it
        // back and starts up with hub already collapsed.
        let persisted = shelbi_state::sidebar_collapsed_machines().unwrap();
        assert!(persisted.contains("hub"));
        let mut app2 = App::new_sidebar("demo");
        app2.refresh().unwrap();
        assert!(app2.collapsed_machines.contains("hub"));
        assert!(find_workspace_row(&app2.rows(), "alpha").is_none());

        // Toggle hub again → expanded.
        app.toggle_machine_collapsed("hub");
        assert!(find_workspace_row(&app.rows(), "alpha").is_some());
        assert!(!shelbi_state::sidebar_collapsed_machines().unwrap().contains("hub"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn collapsed_machine_count_reflects_active_vs_idle_workspaces() {
        // 3 workspaces on hub: 2 with an assigned in-progress task
        // (active), 1 idle. Collapsing hub surfaces "(3, 2 active)" via
        // the row's total/active fields — that's the count the
        // renderer hangs off the header.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut project = fixture_project();
        // Replace the original fixture (alpha on hub, delta on devbox)
        // with three workspaces on hub plus one on devbox. The
        // grouped-layout branch keys off the set of machines that have
        // at least one workspace, so we need devbox to carry a row too
        // for the `▾ hub` header to be emitted at all.
        project.workspaces = vec![
            shelbi_core::WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: Default::default(),
            },
            shelbi_core::WorkspaceSpec {
                name: "bravo".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: Default::default(),
            },
            shelbi_core::WorkspaceSpec {
                name: "charlie".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: Default::default(),
            },
            shelbi_core::WorkspaceSpec {
                name: "delta".into(),
                machine: "devbox".into(),
                runner: "claude".into(),
                role: Default::default(),
            },
        ];
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        for (id, ws) in [("t-a", "alpha"), ("t-b", "bravo")] {
            shelbi_state::save_task(
                "demo",
                &Task {
                    id: id.into(),
                    title: id.into(),
                    column: Column::InProgress,
                    priority: 0,
                    assigned_to: Some(ws.into()),
                    workflow: None,
                    branch: None,
                    depends_on: Vec::new(),
                    prefers_machine: None,
                    zen: None,
                    created_at: now,
                    updated_at: now,
                    params: std::collections::BTreeMap::new(),
                },
                "",
            )
            .unwrap();
        }

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        app.toggle_machine_collapsed("hub");
        let rows = app.rows();
        let hub = rows
            .iter()
            .find(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .expect("hub header must render even when collapsed");
        assert!(matches!(
            hub,
            Row::MachineGroup { collapsed: true, total: 3, active: 2, .. }
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn collapsed_state_for_unknown_machine_is_silently_ignored() {
        // ~/.shelbi/state.json names a machine that the current project
        // doesn't declare. The sidebar must load and render without
        // error, and the unknown entry stays on disk untouched so
        // re-adding the machine later restores the prior state.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Pre-populate state.json with a stale machine name.
        shelbi_state::toggle_sidebar_machine_collapsed("ghost").unwrap();

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        // The stale name is loaded into the cache (so re-adding the
        // machine restores the collapse state) but doesn't produce a
        // MachineGroup row — there's no machine called `ghost` in this
        // project. Hub and devbox render normally.
        assert!(app.collapsed_machines.contains("ghost"));
        let rows = app.rows();
        assert!(!rows.iter().any(
            |r| matches!(r, Row::MachineGroup { name, .. } if name == "ghost")
        ));
        // Real machines are still rendered expanded — `ghost` doesn't
        // leak its collapse state to anyone else.
        let hub = rows
            .iter()
            .find(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .expect("hub header must render");
        assert!(matches!(hub, Row::MachineGroup { collapsed: false, .. }));

        // And the on-disk entry survives the render pass — no silent
        // pruning.
        assert!(shelbi_state::sidebar_collapsed_machines()
            .unwrap()
            .contains("ghost"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn activate_on_machine_row_toggles_collapse_instead_of_view() {
        // Activate on a `MachineGroup` row routes to
        // `toggle_machine_collapsed`, not `activate_view` — Space and
        // Enter both flow through `activate_selection`, so this is the
        // behavior the keymap depends on.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let hub_idx = app
            .rows()
            .iter()
            .position(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .unwrap();
        app.sidebar_index = hub_idx;
        app.activate_selection();
        assert!(app.collapsed_machines.contains("hub"));
        // No status_line change relating to view focus — activation on
        // a MachineGroup never reaches `activate_view`, so it can't
        // accidentally try to focus a workspace pane.
        assert!(!app.status_line.starts_with("▶ "));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn multi_machine_project_groups_workspaces_under_machine_headers_with_indent() {
        // Two machines, two workspaces — each workspace sits underneath a
        // `Row::MachineGroup` for its host and carries the `indent: true`
        // flag so the renderer can shift it right by the leading-space
        // amount the wireframe specifies.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();

        let hub_idx = rows
            .iter()
            .position(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .expect("hub group header must render in a multi-machine project");
        assert!(matches!(
            &rows[hub_idx + 1],
            Row::Workspace { name, indent: true, .. } if name == "alpha"
        ));
        let devbox_idx = rows
            .iter()
            .position(|r| matches!(r, Row::MachineGroup { name, .. } if name == "devbox"))
            .expect("devbox group header must render in a multi-machine project");
        assert!(devbox_idx > hub_idx, "machine headers must follow project declaration order");
        assert!(matches!(
            &rows[devbox_idx + 1],
            Row::Workspace { name, indent: true, .. } if name == "delta"
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_row_carries_agent_from_task_params_with_developer_default() {
        // The sidebar's per-row agent name mirrors `shelbi workspace list`
        // — `agent:` from the task's frontmatter wins, falling back to
        // `developer` when the task didn't pin one. Idle workspaces (no
        // assigned in-progress task) collapse to `agent: None` so the
        // renderer surfaces the "idle" placeholder rather than the
        // default agent.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        // alpha → explicit `agent: qa` frontmatter
        let mut params = std::collections::BTreeMap::new();
        params.insert("agent".into(), "qa".into());
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "explicit".into(),
                title: "explicit".into(),
                column: Column::InProgress,
                priority: 0,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params,
            },
            "",
        )
        .unwrap();
        // delta → no `agent:` in frontmatter, so the default applies
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "default".into(),
                title: "default".into(),
                column: Column::InProgress,
                priority: 0,
                assigned_to: Some("delta".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();

        let workspaces = load_workspaces("demo", &[]).unwrap();
        let alpha = workspaces.iter().find(|w| w.name == "alpha").unwrap();
        let delta = workspaces.iter().find(|w| w.name == "delta").unwrap();
        assert_eq!(alpha.agent.as_deref(), Some("qa"));
        assert_eq!(delta.agent.as_deref(), Some("developer"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn idle_workspace_renders_no_agent_active_renders_resolved_agent() {
        // Acceptance criterion: idle workspaces drop their agent column,
        // active workspaces carry it. The renderer keys off `agent` being
        // `Some` / `None` — this test pins the data shape so a future
        // refactor can't silently render a placeholder agent on idle rows.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "active".into(),
                title: "active".into(),
                column: Column::InProgress,
                priority: 0,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();

        let alpha = find_workspace_row(&rows, "alpha").unwrap();
        assert!(matches!(
            alpha,
            Row::Workspace { agent: Some(a), .. } if a == "developer"
        ));
        let delta = find_workspace_row(&rows, "delta").unwrap();
        assert!(matches!(delta, Row::Workspace { agent: None, .. }));

        // Active row's badge is not Idle; idle row's badge is.
        assert_ne!(find_workspace_badge(&rows, "alpha").unwrap(), WorkspaceBadge::Idle);
        assert_eq!(find_workspace_badge(&rows, "delta").unwrap(), WorkspaceBadge::Idle);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_badge_reflects_status_yaml_when_task_in_progress() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        for (id, workspace, state) in [
            ("t-work", "alpha", WorkspaceState::Working),
            ("t-wait", "delta", WorkspaceState::AwaitingInput),
        ] {
            shelbi_state::save_task(
                "demo",
                &Task {
                    id: id.into(),
                    title: id.into(),
                    column: Column::InProgress,
                    priority: 0,
                    assigned_to: Some(workspace.into()),
                    workflow: None,
                    branch: None,
                    depends_on: Vec::new(),
                    prefers_machine: None,
                    zen: None,
                    created_at: now,
                    updated_at: now,
                    params: std::collections::BTreeMap::new(),
                },
                "",
            )
            .unwrap();
            shelbi_state::save_workspace_status(&shelbi_state::WorkspaceStatus {
                workspace: workspace.into(),
                current_task: Some(id.into()),
                state,
                last_transition: now,
                last_seen: now,
            })
            .unwrap();
        }

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();

        assert_eq!(find_workspace_badge(&rows, "alpha").unwrap(), WorkspaceBadge::Working);
        assert_eq!(
            find_workspace_badge(&rows, "delta").unwrap(),
            WorkspaceBadge::AwaitingInput
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_badge_shows_awaiting_permission_when_blocked() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "t".into(),
                title: "t".into(),
                column: Column::InProgress,
                priority: 0,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();
        shelbi_state::save_workspace_status(&shelbi_state::WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: Some("t".into()),
            state: WorkspaceState::Blocked,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        assert_eq!(
            find_workspace_badge(&rows, "alpha").unwrap(),
            WorkspaceBadge::AwaitingPermission
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_badge_shows_review_ready_when_task_in_review_column() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "ready".into(),
                title: "ready".into(),
                column: Column::Review,
                priority: 0,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();
        // Even an active "working" status.yaml shouldn't beat review-ready —
        // once the task moves to review, the workspace is conceptually done.
        shelbi_state::save_workspace_status(&shelbi_state::WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: None,
            state: WorkspaceState::Working,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        assert_eq!(
            find_workspace_badge(&rows, "alpha").unwrap(),
            WorkspaceBadge::ReviewReady
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_badge_idle_overrides_stale_status_yaml_when_no_task() {
        // status.yaml says working but no in-progress task is assigned —
        // probably a leftover from a finished task. Show idle so the user
        // isn't misled.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_workspace_status(&shelbi_state::WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: None,
            state: WorkspaceState::Working,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        assert_eq!(find_workspace_badge(&rows, "alpha").unwrap(), WorkspaceBadge::Idle);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn nav_is_chat_tasks_activity_no_review_destination() {
        // The sidebar nav stays at three items — Review is surfaced
        // inline as a live list below, never as a destination.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let names: Vec<&'static str> = app
            .rows()
            .into_iter()
            .filter_map(|r| match r {
                Row::Nav {
                    view: View::Builtin(n),
                    ..
                } => Some(n),
                _ => None,
            })
            .collect();
        assert_eq!(names, vec!["orch", "tasks", "activity"]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn review_section_hides_when_queue_empty_and_appears_when_populated() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        assert!(
            !app.rows().iter().any(|r| matches!(r, Row::Section { label } if label == "Ready for Review")),
            "empty review queue must not render the section header"
        );

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "ready".into(),
                title: "Fix login".into(),
                column: Column::Review,
                priority: 0,
                assigned_to: Some("delta".into()),
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            "",
        )
        .unwrap();
        app.refresh().unwrap();
        let rows = app.rows();
        let section_idx = rows
            .iter()
            .position(|r| matches!(r, Row::Section { label } if label == "Ready for Review"))
            .expect("populated review queue must render the section header");
        assert!(matches!(
            &rows[section_idx + 1],
            Row::Review { title, workspace, .. } if title == "Fix login" && workspace.as_deref() == Some("delta")
        ));
        // Exactly one blank row separates the agents list from the Ready
        // for Review header — every section header gets the same uniform
        // single-blank gap.
        assert!(section_idx >= 1);
        assert!(matches!(&rows[section_idx - 1], Row::Blank));
        assert!(!matches!(&rows[section_idx - 2], Row::Blank));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn nav_down_skips_section_headers() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        // Start on the last nav item (Activity, idx 2). Next nav_down
        // skips the blank spacer (idx 3) and the `Workspaces` section
        // header (idx 4) — both inert — and lands on the first machine
        // group header (idx 5), which is now selectable so Space/Enter
        // can toggle the machine's collapse state.
        app.sidebar_index = 2;
        app.nav_down();
        let rows = app.rows();
        assert!(rows[app.sidebar_index].is_selectable());
        assert!(
            matches!(&rows[app.sidebar_index], Row::MachineGroup { name, .. } if name == "hub"),
            "nav_down past a section header must land on a machine group, got {:?}",
            app.sidebar_index
        );
        // One more nav_down lands on the first workspace under hub.
        app.nav_down();
        assert!(
            matches!(&app.rows()[app.sidebar_index], Row::Workspace { name, .. } if name == "alpha"),
            "nav_down past a machine group must land on its first workspace",
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_badge_glyphs_are_single_chars() {
        // Sidebar column must stay narrow for small terminals. Each badge
        // glyph is one (possibly multi-byte) Unicode char — never a
        // multi-char string.
        for b in [
            WorkspaceBadge::Working,
            WorkspaceBadge::AwaitingInput,
            WorkspaceBadge::AwaitingPermission,
            WorkspaceBadge::ReviewReady,
            WorkspaceBadge::Idle,
        ] {
            assert_eq!(
                b.glyph().chars().count(),
                1,
                "{b:?} glyph {:?} must be a single char",
                b.glyph()
            );
        }
    }

    #[test]
    fn row_decoration_centralizes_icon_and_status_for_every_kind() {
        // `Row::decoration` is the single source of truth for the icon +
        // status tint each sidebar row paints — the palette consumes the
        // exact same value via `entry_from_row`, so anything that drifts
        // here drifts in both surfaces at once (good) instead of leaving
        // them silently misaligned (bad).
        use shelbi_palette::DecorationColor;

        let nav = Row::Nav {
            icon: "💬",
            label: "Chat",
            view: View::Builtin("orch"),
        };
        let d = nav.decoration().unwrap();
        assert_eq!(d.glyph, "💬");
        assert_eq!(d.color, DecorationColor::Default);

        let workspace = Row::Workspace {
            name: "alpha".into(),
            badge: WorkspaceBadge::AwaitingPermission,
            agent: Some("developer".into()),
            indent: false,
            view: View::Workspace("alpha".into()),
        };
        let d = workspace.decoration().unwrap();
        assert_eq!(d.glyph, WorkspaceBadge::AwaitingPermission.glyph());
        assert_eq!(d.color, DecorationColor::Red);

        let review = Row::Review {
            title: "Fix login".into(),
            workspace: Some("delta".into()),
            view: View::ReviewTask("ready".into()),
        };
        let d = review.decoration().unwrap();
        assert_eq!(d.glyph, "✓");
        assert_eq!(d.color, DecorationColor::Cyan);

        let agent = Row::LegacyAgent {
            id: "spawn-1".into(),
            machine: "hub".into(),
            status: shelbi_core::Status::Running,
            view: View::Agent("spawn-1".into()),
        };
        let d = agent.decoration().unwrap();
        assert_eq!(d.glyph, shelbi_core::Status::Running.glyph());
        assert_eq!(d.color, DecorationColor::Green);

        assert!(Row::Section { label: "Workspaces".into() }.decoration().is_none());
        assert!(Row::Blank.decoration().is_none());
        assert!(
            Row::MachineGroup {
                name: "hub".into(),
                collapsed: false,
                total: 0,
                active: 0,
            }
            .decoration()
            .is_none(),
            "machine group headers are dividers, not decorated rows"
        );
    }

    #[test]
    fn toggle_zen_mode_flips_and_writes_state_and_event() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        assert_eq!(app.zen_mode, shelbi_state::ZenModeState::Off);

        app.toggle_zen_mode();
        assert_eq!(app.zen_mode, shelbi_state::ZenModeState::On);
        // Persisted to state.json — a re-read sees the toggle.
        let state = shelbi_state::read_state("demo").unwrap();
        assert_eq!(state.zen_mode, shelbi_state::ZenModeState::On);
        // Toggle again returns to Off — Paused is not in the binary path.
        app.toggle_zen_mode();
        assert_eq!(app.zen_mode, shelbi_state::ZenModeState::Off);

        // Events log got both transitions in the canonical mode=zen shape
        // tagged with `user:hotkey` so the orchestrator's tail and the
        // activity feed can both pick them up.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let on_line = log
            .lines()
            .find(|l| l.contains("mode=zen off -> on reason=user:hotkey"));
        let off_line = log
            .lines()
            .find(|l| l.contains("mode=zen on -> off reason=user:hotkey"));
        assert!(on_line.is_some(), "missing zen-on event in: {log}");
        assert!(off_line.is_some(), "missing zen-off event in: {log}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn toggle_zen_mode_from_paused_goes_to_on() {
        // Paused isn't reachable via the hotkey, but state.json may already
        // be Paused from a CLI invocation; toggling there should mean
        // "give me the on path", matching the spec's binary-toggle wording.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();
        let paused = shelbi_state::State {
            zen_mode: shelbi_state::ZenModeState::Paused,
            zen_last_crashed_at: None,
            ..shelbi_state::State::default()
        };
        shelbi_state::write_state("demo", &paused).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        assert_eq!(app.zen_mode, shelbi_state::ZenModeState::Paused);

        app.toggle_zen_mode();
        assert_eq!(app.zen_mode, shelbi_state::ZenModeState::On);

        std::env::remove_var("SHELBI_HOME");
    }
}
