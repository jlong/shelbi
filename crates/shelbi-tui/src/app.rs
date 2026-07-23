use std::collections::BTreeSet;
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use shelbi_core::{Agent, Column, Status};
use shelbi_palette::{Decoration, DecorationColor};
use shelbi_state::{
    keymap::{DisplayStyle, Keymaps},
    load_workspace_status, read_state, sidebar_collapsed_machines,
    toggle_sidebar_machine_collapsed, TaskFile, WorkspaceState, ZenModeState, ZenToggleChord,
};

/// Sidebar render area floor. Below two columns / two rows the pane can't
/// fit the 1-col horizontal padding plus a content cell — it renders as an
/// empty sliver that reads as "the sidebar is gone". Used by
/// [`App::note_sidebar_area`] to emit an observable collapse signal.
const MIN_SIDEBAR_WIDTH: u16 = 2;
const MIN_SIDEBAR_HEIGHT: u16 = 2;

/// How long to suppress repeat "sidebar area collapsed" diagnostics while
/// the pane stays degenerate — the render loop ticks every 200ms, so an
/// unthrottled warning would flood `tui.log`.
const COLLAPSE_WARN_THROTTLE: Duration = Duration::from_secs(3);

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
    /// ⏸ — the runner stalled on a usage/session limit. Distinct from the
    /// working/idle/awaiting badges so a paused slot reads at a glance; the
    /// poller detects it from a pane sample and clears it when the worker
    /// resumes (see [`WorkspaceState::Paused`]).
    Paused,
    /// · — no in-flight task assigned. A dev workspace that just finished a
    /// task reads Idle immediately: on promotion the poller closes its
    /// session (spec §16), so there is no lingering completion glyph — the
    /// review sections own the "done, awaiting human" state, never the
    /// workspace row.
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
            WorkspaceBadge::Paused => "⏸",
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
    /// Human-readable label for the project, loaded from the project YAML's
    /// `display_name` each refresh. `None` for legacy projects (and until the
    /// first refresh), which then render under `project_name`. Read via
    /// [`App::display_label`].
    pub display_name: Option<String>,
    pub agents: Vec<Agent>,
    pub workspaces: Vec<WorkspaceOverview>,
    /// The project config's load error, surfaced inline in the Workspaces
    /// section instead of silently dropping the section. `Some` only when a
    /// config file for the project **exists on disk but fails to load**
    /// (invalid id, schema/workspace validation, unparseable YAML) — a
    /// genuinely missing config (fresh/half-set-up project) stays `None` so
    /// the section is omitted as before rather than flagged as broken. Built
    /// each refresh by [`App::refresh`]; carries the same actionable message
    /// [`shelbi_state::load_project`] hands the `shelbi workspace list` CLI.
    pub config_error: Option<String>,
    /// Tasks in the Review column that are **loaded on a review worktree** —
    /// the "Ready for Review" section (✓). Built each refresh by
    /// [`split_review_sections`]; each entry carries the `machine:port` URL
    /// the human can open.
    pub ready_review: Vec<ReviewEntry>,
    /// Tasks in the Review column **waiting for a free review workspace** —
    /// the "Queued for Review" section (·). No location yet (nothing serving).
    pub queued_review: Vec<ReviewEntry>,
    pub sidebar_index: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    pub should_quit: bool,
    /// Latest Zen Mode state read from `state.json`. Drives the green pill
    /// in the lower-left status block and the Alt+Z toggle direction.
    pub zen_mode: ZenModeState,
    /// Chord that toggles Zen Mode — resolved from
    /// `keys.yaml::defaults.global.zen_toggle` via
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
    /// Preformatted daemon/CLI version segment for the footer, refreshed
    /// with the rest of the sidebar state. `None` until the first refresh
    /// (the row renders blank).
    pub daemon_version_line: Option<String>,
    /// True when the probed daemon version differs from this binary —
    /// the footer paints [`App::daemon_version_line`] red instead of dim.
    pub daemon_version_mismatch: bool,
    /// An in-flight review load running on a background thread, so the git
    /// checkout / install / dispatch round-trip never blocks the UI thread.
    /// [`App::poll_review_load`] drains its channel each loop tick and swaps
    /// the outcome (or an animated spinner) into [`App::status_line`].
    review_job: Option<ReviewLoadJob>,
    /// Count of render-pass panics the sidebar loop has caught and healed
    /// from. Purely diagnostic — surfaced in the recovery log line so a
    /// repeatedly-panicking frame is distinguishable from a one-off in
    /// `tui.log`. See [`App::recover_render_panic`].
    pub render_panics: u32,
    /// Throttle for the "sidebar area collapsed" diagnostic. Holds the last
    /// instant a degenerate render size was logged so a persistently tiny
    /// pane doesn't flood `tui.log` at the 200ms render cadence; cleared
    /// once the size recovers. See [`App::note_sidebar_area`].
    last_collapse_warn: Option<Instant>,
}

/// A review load running on a worker thread. Holds the channel the thread
/// reports its outcome on plus the start instant that drives the status-line
/// spinner while the load is in flight.
struct ReviewLoadJob {
    task_id: String,
    workspace: String,
    rx: Receiver<std::result::Result<String, String>>,
    started: Instant,
}

impl App {
    pub fn new_sidebar(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            display_name: None,
            agents: Vec::new(),
            workspaces: Vec::new(),
            config_error: None,
            ready_review: Vec::new(),
            queued_review: Vec::new(),
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
            daemon_version_line: None,
            daemon_version_mismatch: false,
            review_job: None,
            render_panics: 0,
            last_collapse_warn: None,
        }
    }

    /// Probe the hub daemon and precompute the footer version segment. A match
    /// still labels both sides so the daemon and CLI versions remain explicit;
    /// a mismatch also names the fix and flips
    /// [`App::daemon_version_mismatch`] so the footer flags it.
    pub fn probe_daemon_version(&mut self) {
        let cli = env!("CARGO_PKG_VERSION");
        let (line, mismatch) = match shelbi_state::daemon_version_status() {
            shelbi_state::DaemonVersionStatus::NotRunning => {
                (format!("daemon not running · cli {cli}"), false)
            }
            shelbi_state::DaemonVersionStatus::Match { version } => {
                (format!("daemon {version} · cli {cli}"), false)
            }
            shelbi_state::DaemonVersionStatus::Mismatch { daemon } => (
                format!("daemon {daemon} ≠ cli {cli} — shelbi daemon restart"),
                true,
            ),
        };
        self.daemon_version_line = Some(line);
        self.daemon_version_mismatch = mismatch;
    }

    /// Borrow the keymaps populated at startup by `run_sidebar`. The
    /// sidebar handler reads this once per loop entry rather than
    /// re-parsing `keys.yaml` per keystroke.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
    }

    /// The label shown in the sidebar header: the project's `display_name`
    /// when set, otherwise the slug `project_name` — so a legacy project reads
    /// exactly as before. Mirrors [`shelbi_core::Project::display_label`].
    pub fn display_label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.project_name)
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
        // Map the clicked line back to a row index. Two regions:
        //
        //   1. The leading `Row::Nav` rows render as a full-width block whose
        //      items are interleaved with separator lines — item `k` sits on
        //      rendered line `2k + 1`, separators on the even lines between.
        //   2. Everything after them is a normal variable-height list (a review
        //      entry is two lines, every other row one), so we walk cumulative
        //      heights from where the nav block ends.
        //
        // (Matches the renderer's no-scroll case; the list scrolls only when it
        // overflows, which the click map doesn't model, same as before.)
        let target = (row - area.y) as usize;
        let rows = self.rows();
        let nav_n = rows
            .iter()
            .take_while(|r| matches!(r, Row::Nav { .. }))
            .count();
        let nav_lines = crate::sidebar::nav_lines(nav_n);
        if target < nav_lines {
            // Odd lines are item rows; even lines are inert separators.
            if target % 2 == 1 {
                let idx = target / 2;
                return rows.get(idx).and_then(|r| r.is_selectable().then_some(idx));
            }
            return None;
        }
        // The rest-of-list renders inside the 1-col horizontal padding on each
        // side, so its inner width — the width the config-error row wraps to —
        // is the list area minus two columns. Match the renderer's `rest.width`
        // so a wrapped error row's height agrees between click map and drawing.
        let inner_width = area.width.saturating_sub(2) as usize;
        let mut line = nav_lines;
        for (idx, r) in rows.iter().enumerate().skip(nav_n) {
            let h = row_height(r, inner_width);
            if target < line + h {
                return r.is_selectable().then_some(idx);
            }
            line += h;
        }
        None
    }

    /// Sidebar rows: a fixed 3-item nav (Chat / Tasks / Activity), then
    /// declared workspaces under an `— Workspaces —` separator, then review
    /// tasks split across `— Ready for Review —` (loaded on a review worktree)
    /// and `— Queued for Review —` (waiting for a slot), then any legacy
    /// `shelbi spawn` agents under `— spawned —`. Each section header and its rows are
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
        if let Some(message) = &self.config_error {
            // The project config exists but won't load — surface the error
            // where the workspaces would be instead of silently dropping the
            // whole section (the user has no other clue the config is broken).
            // The Workspaces header stays so the row reads as "this section
            // couldn't load", not a free-floating error.
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "Workspaces".into(),
            });
            rows.push(Row::ConfigError {
                message: message.clone(),
            });
        } else if !self.workspaces.is_empty() {
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
        // Ready for Review — tasks loaded on a review worktree (✓). Line 1
        // carries the `machine:port` URL badge; line 2 the branch.
        if !self.ready_review.is_empty() {
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "Ready for Review".into(),
            });
            for e in &self.ready_review {
                rows.push(Row::Review {
                    title: e.title.clone(),
                    branch: e.branch.clone(),
                    location: e.location.clone(),
                    ready: true,
                    view: View::ReviewTask(e.task_id.clone()),
                });
            }
        }
        // Queued for Review — Review-status tasks waiting for a free review
        // workspace (·). No location yet (nothing serving); branch on line 2.
        if !self.queued_review.is_empty() {
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "Queued for Review".into(),
            });
            for e in &self.queued_review {
                rows.push(Row::Review {
                    title: e.title.clone(),
                    branch: e.branch.clone(),
                    location: None,
                    ready: false,
                    view: View::ReviewTask(e.task_id.clone()),
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
        // The daemon can be restarted independently of this long-lived
        // sidebar process after a package upgrade. Re-probe on the normal
        // refresh cadence so a stale red mismatch clears without requiring a
        // separate `shelbi reload sidebar`.
        self.probe_daemon_version();
        // Refresh the human-readable label from the project YAML. A load
        // failure (fresh/half-set-up project) leaves the slug showing.
        self.display_name = shelbi_state::load_project(&self.project_name)
            .ok()
            .and_then(|p| p.display_name.or(p.label));
        self.agents = load_agents(&self.project_name).unwrap_or_default();
        let review =
            shelbi_state::list_column(&self.project_name, Column::review()).unwrap_or_default();
        let (ready, queued) = split_review_sections(&self.project_name, review);
        self.ready_review = ready;
        self.queued_review = queued;
        match load_workspaces(&self.project_name) {
            Ok(ws) => {
                self.workspaces = ws;
                self.config_error = None;
            }
            Err(e) => {
                // A load failure with a config file present on disk is a
                // broken config (invalid id / schema / YAML) — surface the
                // message inline. A load failure with *no* config file is a
                // fresh/half-set-up project: keep the section omitted, as
                // before, rather than crying "config error" during setup.
                self.workspaces = Vec::new();
                self.config_error =
                    project_config_present(&self.project_name).then(|| e.to_string());
            }
        }
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
            View::ReviewTask(id) => {
                // A Queued row isn't loaded on a review slot yet: ask before
                // loading (and load onto a *review* workspace, never the dev
                // pane that built it). A Ready row is already assigned to a
                // review slot, so open its review interface straight away —
                // unless that slot's window was never launched, in which case
                // `open_ready_review` lazily launches it first.
                if self.queued_review.iter().any(|e| e.task_id == *id) {
                    self.open_review_load_prompt(id);
                } else {
                    self.open_ready_review(id);
                }
            }
        }
    }

    /// Raise the "Load onto a review workspace?" confirm for a Queued row as a
    /// centered tmux `display-popup` (the same surface style as the palette
    /// popup), rather than an overlay boxed inside the sidebar rect. Resolves a
    /// free `review`-tagged slot up front so the popup can name the target (or
    /// report none free), then launches the confirm. Only a confirming exit
    /// starts the load, onto that same slot — so the dev pane is never
    /// dispatched to and cancel is a no-op. A no-op when a prior load is still
    /// running, so two clicks can't stack dispatches.
    fn open_review_load_prompt(&mut self, id: &str) {
        if self.review_job.is_some() {
            self.status_line = "a review load is already in progress…".into();
            return;
        }
        let task_title = self
            .queued_review
            .iter()
            .find(|e| e.task_id == *id)
            .map(|e| e.title.clone())
            .unwrap_or_else(|| id.to_string());
        let workspace = match shelbi_orchestrator::load::free_review_workspaces(&self.project_name) {
            Ok(free) => free.into_iter().next().map(|w| w.name),
            Err(e) => {
                self.status_line = format!("review slots query failed: {e}");
                return;
            }
        };
        // The confirm renders in a centered tmux popup whose exit code carries
        // the decision (0 = confirm, non-zero = cancel / none-free). Blocking
        // the sidebar loop here is fine: the popup is modal, exactly as the
        // palette popup blocks its launcher until it closes.
        if review_confirm_popup(&task_title, workspace.as_deref()) {
            if let Some(workspace) = workspace {
                self.start_review_load(id.to_string(), workspace);
            }
        }
    }

    /// Recover sidebar state after the render pass panicked. A panic while
    /// drawing must not blank the pane, so the loop catches the unwind and
    /// calls this to self-heal: bump the diagnostic counter and hand back a
    /// description of what was reset for the caller to log, so the next frame
    /// repaints the sidebar from scratch instead of leaving the region gone.
    /// Pure / no-IO so the self-heal path is unit-testable without a live
    /// terminal.
    ///
    /// (Before PR #465 this also dropped an in-sidebar review-load modal
    /// overlay — the surface implicated in the drag-drop / large-paste-burst
    /// repro. That confirm now renders in a centered tmux popup outside this
    /// process, so there is no in-sidebar overlay left to drop; recovery is
    /// simply repaint + count + log.)
    pub fn recover_render_panic(&mut self, panic_msg: &str) -> String {
        self.render_panics = self.render_panics.saturating_add(1);
        format!(
            "sidebar render panicked ({panic_msg}); repainting (recovery #{})",
            self.render_panics
        )
    }

    /// Note the terminal size the sidebar is about to render into. Returns a
    /// diagnostic message when the pane has collapsed below the usable floor
    /// — a zero/one-column sidebar renders blank and reads as "gone" — so the
    /// caller can log an observable signal. Rate-limited to once every few
    /// seconds (`now` is injected for testability) so a persistently tiny
    /// pane can't flood `tui.log` at the render cadence; returns `None` when
    /// the size is healthy, clearing the throttle so the next collapse logs
    /// immediately. The pane self-heals on its own once tmux restores a sane
    /// size — the next render repaints — so this is a signal, not a fix.
    pub fn note_sidebar_area(&mut self, width: u16, height: u16, now: Instant) -> Option<String> {
        let degenerate = width < MIN_SIDEBAR_WIDTH || height < MIN_SIDEBAR_HEIGHT;
        if !degenerate {
            self.last_collapse_warn = None;
            return None;
        }
        let throttled = self
            .last_collapse_warn
            .is_some_and(|t| now.duration_since(t) < COLLAPSE_WARN_THROTTLE);
        if throttled {
            return None;
        }
        self.last_collapse_warn = Some(now);
        Some(format!(
            "sidebar render area collapsed to {width}x{height}; \
             pane will repaint when tmux restores a usable size"
        ))
    }

    /// Kick off a background load of `task_id` onto the review slot
    /// `workspace`, showing a spinner and switching focus when it lands.
    /// Shared by the Queued-for-Review confirm ([`App::open_review_load_prompt`])
    /// and the Ready-but-unlaunched path ([`App::open_ready_review`]) so the
    /// two dispatch through one code path. A no-op when a prior load is still
    /// running, so two clicks can't stack dispatches. Emits a `dispatch`
    /// event so the interaction is observable in `events.log` even before the
    /// launch itself starts logging.
    fn start_review_load(&mut self, task_id: String, workspace: String) {
        if self.review_job.is_some() {
            self.status_line = "a review load is already in progress…".into();
            return;
        }
        let _ = shelbi_state::append_dispatch_event(
            &task_id,
            &workspace,
            "review-load",
            "loading branch onto review slot",
        );
        let (tx, rx) = channel();
        let project = self.project_name.clone();
        let tid = task_id.clone();
        let ws = workspace.clone();
        std::thread::spawn(move || {
            let result = shelbi_orchestrator::load::load_review_task(&project, &tid, &ws)
                .map_err(|e| e.to_string());
            let _ = tx.send(result);
        });
        self.status_line = format!("⠋ loading {task_id} onto {workspace}…");
        self.review_job = Some(ReviewLoadJob {
            task_id,
            workspace,
            rx,
            started: Instant::now(),
        });
    }

    /// Advance any in-flight review load: pull the outcome if the worker
    /// finished, otherwise refresh the spinner frame. Called once per sidebar
    /// event-loop tick so the status line stays live without ever blocking on
    /// the load.
    pub fn poll_review_load(&mut self) {
        let Some(job) = self.review_job.as_ref() else {
            return;
        };
        match job.rx.try_recv() {
            Ok(Ok(target)) => {
                let task_id = job.task_id.clone();
                self.review_job = None;
                // The review workspace's window now holds the booted review
                // agent/server pane. Build the three-column review interface
                // (sidebar | agent | panel) inside it and switch to it, so the
                // same click that started the review agent also opens the
                // review panel — the load/serve state and branch/server
                // details — instead of leaving the user on a bare agent window
                // that only grows the panel on a *second* activation. This is
                // the missing half of a Ready-for-Review click that had to
                // launch the slot's window first (e.g. the row was already
                // Ready when Shelbi restarted); the already-live path builds
                // the same interface synchronously in `open_ready_review`.
                self.open_loaded_review_interface(&task_id, &target);
            }
            Ok(Err(e)) => {
                self.status_line = format!("review load failed: {e}");
                self.review_job = None;
            }
            Err(TryRecvError::Disconnected) => {
                self.status_line = format!("review load for {} was interrupted", job.task_id);
                self.review_job = None;
            }
            Err(TryRecvError::Empty) => {
                const FRAMES: [char; 10] =
                    ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
                let idx = (job.started.elapsed().as_millis() / 100) as usize % FRAMES.len();
                self.status_line =
                    format!("{} loading {} onto {}…", FRAMES[idx], job.task_id, job.workspace);
            }
        }
    }

    /// Build (and focus) the review interface for a task whose review slot has
    /// just finished loading, falling back to a plain window switch when the
    /// embed can't be constructed. Split out of [`App::poll_review_load`] so
    /// the borrow of the in-flight job is released before the tmux round-trip.
    ///
    /// This closes the gap where clicking a Ready-for-Review row whose slot had
    /// to be launched first booted the review agent but left the review panel
    /// unopened: the click and the panel now land in one action, matching the
    /// already-live click path in [`App::open_ready_review`]. Reusing
    /// [`shelbi_orchestrator::review_ui::open_review_interface`] means the panel
    /// shows the same review sub-state and branch/server details either way.
    fn open_loaded_review_interface(&mut self, task_id: &str, target: &str) {
        use shelbi_orchestrator::review_ui::ReviewOpenOutcome;
        match shelbi_orchestrator::review_ui::open_review_interface(&self.project_name, task_id) {
            Ok(ReviewOpenOutcome::Opened(_)) => {
                self.status_line = format!("▶ reviewing {task_id}");
            }
            // Remote slot: the workspace window was already focused by
            // `open_review_interface`; surface its note rather than re-focusing.
            Ok(ReviewOpenOutcome::RemoteFallback(note)) => self.status_line = note,
            // Loading / NeedsLaunch / an error (including the unit-test env with
            // no tmux) mean the interface couldn't be embedded here. Don't kick
            // off another load — that risks a launch loop — just focus the
            // freshly loaded window so the review is at least on screen (the
            // traveling sidebar follows via the `after-select-window` hook).
            _ => {
                let _ = run_tmux(["select-window", "-t", target]);
                self.status_line = format!("▶ reviewing {task_id}");
            }
        }
    }

    /// Open the review interface for a Ready-for-review task. Delegates to
    /// [`shelbi_orchestrator::review_ui::open_review_interface`], which builds
    /// the three-column layout inside the review workspace's own window and
    /// switches to it.
    ///
    /// When the assigned review slot's window was never launched (its first
    /// use) or was reaped, `open_review_interface` returns
    /// [`ReviewOpenOutcome::NeedsLaunch`] instead of failing — the first-run
    /// regression from window-per-workspace, where the embed used to target a
    /// nonexistent window and silently no-op. Here that launches the slot in
    /// the background (checkout + boot the review agent/server, same async
    /// load the Queued confirm uses) and switches to it once it's up; the
    /// user re-activates the now-live Ready row to get the embedded interface.
    fn open_ready_review(&mut self, id: &str) {
        use shelbi_orchestrator::review_ui::ReviewOpenOutcome;
        match shelbi_orchestrator::review_ui::open_review_interface(&self.project_name, id) {
            Ok(ReviewOpenOutcome::Opened(_)) => self.status_line = format!("▶ reviewing {id}"),
            Ok(ReviewOpenOutcome::Loading) => {
                self.status_line = format!("loading review for {id}…")
            }
            Ok(ReviewOpenOutcome::RemoteFallback(note)) => self.status_line = note,
            Ok(ReviewOpenOutcome::NeedsLaunch { workspace }) => {
                self.start_review_load(id.to_string(), workspace)
            }
            Err(e) => self.status_line = format!("review `{id}` failed: {e}"),
        }
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
    /// The project config failed to load — surfaced inline under the
    /// Workspaces header instead of silently dropping the section. Carries
    /// the actionable [`shelbi_state::load_project`] message (names the file
    /// and the reason); the renderer word-wraps it across the sidebar width.
    /// Not selectable — it's a message, not a destination.
    ConfigError { message: String },
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
    /// A task sitting in the Review column, rendered as a two-line entry:
    /// line 1 = title (+ a right-aligned `machine:port` URL badge when
    /// loaded); line 2 = branch, dim. Appears in one of two sidebar
    /// sections keyed by [`Row::Review::ready`]:
    /// **Ready for Review** (`ready: true`, ✓ — loaded on a review worktree,
    /// serving) or **Queued for Review** (`ready: false`, · — waiting for a
    /// free review workspace). See spec §16.
    Review {
        title: String,
        /// Branch name shown dim on line 2.
        branch: String,
        /// `machine:port` URL the human can open — `Some` only for a Ready
        /// (loaded) task; `None` for a Queued one (nothing serving yet).
        location: Option<String>,
        /// Loaded on a review worktree (Ready, ✓) vs waiting (Queued, ·).
        ready: bool,
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
        !matches!(
            self,
            Row::Section { .. } | Row::Blank | Row::ConfigError { .. }
        )
    }

    pub fn view(&self) -> Option<&View> {
        match self {
            Row::Nav { view, .. }
            | Row::Workspace { view, .. }
            | Row::Review { view, .. }
            | Row::LegacyAgent { view, .. } => Some(view),
            Row::Section { .. }
            | Row::Blank
            | Row::MachineGroup { .. }
            | Row::ConfigError { .. } => None,
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
            // Ready → cyan ✓ (loaded, human can look); Queued → dim ·
            // (waiting for a review workspace). Single source of truth for
            // both the sidebar renderer and the palette.
            Row::Review { ready, .. } => Some(if *ready {
                Decoration {
                    glyph: "✓".into(),
                    color: DecorationColor::Cyan,
                }
            } else {
                Decoration {
                    glyph: "·".into(),
                    color: DecorationColor::DarkGray,
                }
            }),
            Row::LegacyAgent { status, .. } => Some(status_decoration(*status)),
            // The config-error row's `!` marker is painted directly by the
            // renderer (it's not a palette destination), so no shared
            // decoration here — same as the inert section/blank rows.
            Row::Section { .. }
            | Row::Blank
            | Row::MachineGroup { .. }
            | Row::ConfigError { .. } => None,
        }
    }
}

/// A rendered review-section entry: the data the sidebar needs to draw one
/// two-line review row. Built each refresh by [`split_review_sections`] from
/// the Review column + project config. `location` is `Some("machine:port")`
/// only for a Ready (loaded) task; a Queued one has none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewEntry {
    pub task_id: String,
    pub title: String,
    /// Branch shown dim on line 2.
    pub branch: String,
    /// `machine:port` URL badge (Ready only); `None` when queued.
    pub location: Option<String>,
}

/// Height in list lines a row occupies at the given inner list `width`.
/// Review rows are two-line (title + branch); a config-error row word-wraps
/// its message across `width` (so its height depends on the sidebar width);
/// everything else is one line. Used by [`App::row_at`] to map a click back
/// to a row index now that rows are variable-height, and by the renderer so
/// the click map and the drawing agree.
fn row_height(row: &Row, width: usize) -> usize {
    match row {
        Row::Review { .. } => 2,
        Row::ConfigError { message } => config_error_lines(message, width).len(),
        _ => 1,
    }
}

/// Word-wrap a config-error message into sidebar list lines at the given
/// inner `width`. The first line carries a `! ` marker; continuation lines
/// indent two columns to align under the text. Long words (paths, ids) hard-
/// split rather than overflow. Shared by [`row_height`] and the renderer so
/// the click map and the drawing never disagree on the row's height.
pub(crate) fn config_error_lines(message: &str, width: usize) -> Vec<String> {
    // Reserve two columns for the `! ` marker / continuation indent.
    let body_width = width.saturating_sub(2).max(1);
    let wrapped = wrap_words(message, body_width);
    if wrapped.is_empty() {
        return vec!["! ".to_string()];
    }
    wrapped
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            if i == 0 {
                format!("! {line}")
            } else {
                format!("  {line}")
            }
        })
        .collect()
}

/// Greedy word-wrap `text` to `width` columns (by char count). A single word
/// longer than `width` is hard-split across lines rather than allowed to
/// overflow the sidebar.
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            // Flush the in-progress line, then hard-split the long word.
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            current = chunk;
            continue;
        }
        let need = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if need > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

/// Split the Review column into the two sidebar sections (spec §16):
///
/// - **Ready** — the task's `assigned_to` names a `review`-tagged workspace,
///   so it's loaded onto a review slot. The location is the review workspace
///   holding it.
/// - **Queued** — every other Review-status task (unassigned, or still pinned
///   to the dev workspace that produced it): waiting for a free review slot.
///
/// Membership is driven by the `review` tag — the workspace-neutral marker for
/// "this slot is a review surface". If the project can't be loaded we can't
/// classify, so everything falls back to Queued (no location).
fn split_review_sections(
    project_name: &str,
    queue: Vec<TaskFile>,
) -> (Vec<ReviewEntry>, Vec<ReviewEntry>) {
    let fallback_entry = |task: &shelbi_core::Task, location: Option<String>| ReviewEntry {
        task_id: task.id.clone(),
        title: task.title.clone(),
        branch: task
            .branch
            .clone()
            .unwrap_or_else(|| format!("user/{}", task.id)),
        location,
    };

    let project = match shelbi_state::load_project(project_name) {
        Ok(p) => p,
        Err(_) => {
            let queued = queue
                .iter()
                .map(|tf| fallback_entry(&tf.task, None))
                .collect();
            return (Vec::new(), queued);
        }
    };
    let entry = |task: &shelbi_core::Task, location: Option<String>| {
        let workflow = shelbi_state::load_task_workflow(project_name, &project, task).ok();
        let branch =
            shelbi_orchestrator::branch::branch_name_for_task(&project, workflow.as_ref(), task)
                .unwrap_or_else(|_| {
                    task.branch
                        .clone()
                        .unwrap_or_else(|| format!("user/{}", task.id))
                });
        ReviewEntry {
            task_id: task.id.clone(),
            title: task.title.clone(),
            branch,
            location,
        }
    };

    let mut ready = Vec::new();
    let mut queued = Vec::new();
    for tf in &queue {
        let loaded_on = tf
            .task
            .assigned_to
            .as_deref()
            .and_then(|name| project.workspace(name))
            .filter(|w| project.effective_tags(w).contains("review"));
        match loaded_on {
            Some(ws) => {
                let location = Some(format!("{}:{}", ws.machine, ws.name));
                ready.push(entry(&tf.task, location));
            }
            None => queued.push(entry(&tf.task, None)),
        }
    }
    (ready, queued)
}

/// Build the sidebar's view of declared workspaces from the project YAML, the
/// in-progress task column, and the review column (the latter only so a review
/// slot serving a loaded task reads active rather than idle — §16). One disk
/// read per workspace for the `status.yaml` lookup. Errors when the project
/// config can't be loaded (missing, invalid id, bad schema); the caller keys
/// off that to distinguish "not set up" from "broken config".
fn load_workspaces(project: &str) -> Result<Vec<WorkspaceOverview>> {
    // Propagate a load failure (invalid id, bad schema, unparseable YAML) so
    // the caller can surface it inline in the sidebar. The caller decides
    // whether an error means "broken config" (file present) or "not set up
    // yet" (file absent) — see [`App::refresh`] / [`project_config_present`].
    let p = shelbi_state::load_project(project)?;
    let in_progress = shelbi_state::list_column(project, Column::in_progress()).unwrap_or_default();
    let mut out = Vec::with_capacity(p.workspaces.len());
    for workspace in &p.workspaces {
        // Review-tagged slots never appear under `— Workspaces —`; their
        // capacity surfaces exclusively through the Ready/Queued for Review
        // sections (spec §17). A slot that is *both* a dev and review surface
        // (extra tags) still counts as review here — the review sections own
        // it. Dev workspaces (no `review` tag) list as before, on every
        // project whether or not it declares review slots.
        if p.effective_tags(workspace).contains("review") {
            continue;
        }
        let machine = match p.machine(&workspace.machine) {
            Some(m) => m,
            None => continue, // mis-configured workspace, skip silently
        };
        let is_remote = !machine.host().is_local();
        // Only a dev workspace's own in-progress task marks it active; review
        // slots were skipped above, so no Review-column scan is needed here.
        let assigned_task = in_progress
            .iter()
            .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()));
        let current_task = assigned_task.map(|tf| tf.task.id.clone());
        // Mirror `shelbi workspace list`'s AGENT column: take the task's
        // frontmatter `agent:` when present (matching the lookup the
        // task-start path uses to load agent instructions/skills), and
        // fall back to the project's default task agent. Idle workspaces get
        // `None` — the renderer surfaces "idle" in that case rather than the
        // default agent name.
        let agent = assigned_task.map(|tf| {
            tf.task
                .param_str("agent")
                .map(str::to_string)
                .unwrap_or_else(|| DEFAULT_TASK_AGENT.to_string())
        });
        let badge = derive_workspace_badge(&workspace.name, current_task.is_some());
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

/// Whether a config file for `project` exists on disk in either supported
/// layout — the flat global `<name>.yaml` or the split in-repo
/// `<name>/local.yaml`. Deliberately does **not** validate the name: the whole
/// point is to detect a config that is *present but invalid* (e.g. a
/// capitalized stem like `Shelbi.yaml` that fails id validation), which the
/// name-guarding [`shelbi_state::has_project_registration`] would itself
/// reject. Read-only existence check, so an odd name is harmless here.
fn project_config_present(project: &str) -> bool {
    let Ok(dir) = shelbi_state::projects_dir() else {
        return false;
    };
    dir.join(format!("{project}.yaml")).is_file() || dir.join(project).join("local.yaml").is_file()
}

/// Default agent surfaced when a task has no explicit `agent:` in its
/// frontmatter — matches `shelbi workspace list`'s `DEFAULT_TASK_AGENT`
/// so the sidebar and the CLI never disagree about what's loaded.
const DEFAULT_TASK_AGENT: &str = "developer";

/// Pick the badge for a workspace given the task-board signals + an on-disk
/// state read. Idle wins when there's no in-progress task at all, so a stale
/// `status.yaml` from a previous run doesn't show "working" for a workspace
/// that has nothing to do.
///
/// A workspace never shows a "review-ready"/completion glyph (spec §16): when
/// a dev workspace's task is promoted to Review the poller closes its session,
/// so a finished slot simply reads Idle. Completion lives in the sidebar's
/// review sections, keyed to the review workspace the task is loaded on —
/// never on the dev workspace that produced it.
fn derive_workspace_badge(workspace_name: &str, has_in_progress: bool) -> WorkspaceBadge {
    if !has_in_progress {
        return WorkspaceBadge::Idle;
    }
    match load_workspace_status(workspace_name).ok().flatten() {
        Some(s) => match s.state {
            WorkspaceState::Working => WorkspaceBadge::Working,
            WorkspaceState::AwaitingInput => WorkspaceBadge::AwaitingInput,
            WorkspaceState::Blocked => WorkspaceBadge::AwaitingPermission,
            WorkspaceState::Paused => WorkspaceBadge::Paused,
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

/// Launch the review-load confirm as a centered `tmux display-popup` running
/// `shelbi __review-confirm` — the same popup surface the palette uses, so the
/// confirm reads as a full-terminal modal rather than a box inside the sidebar
/// column. Sized smaller than the palette (70%×60%) since it's a short yes/no.
///
/// Returns `true` only when the popup exits 0 (the user confirmed and a slot
/// was free); a cancel, an informational "none free" dismiss, or any tmux
/// failure all map to `false`, the safe no-op. Blocks until the popup closes,
/// which is exactly the modal behavior we want.
fn review_confirm_popup(title: &str, workspace: Option<&str>) -> bool {
    let Ok(bin) = std::env::current_exe() else {
        return false;
    };
    let bin = bin.to_string_lossy();
    let mut cmd = format!(
        "{} __review-confirm --title {}",
        shelbi_agent::shell_escape(&bin),
        shelbi_agent::shell_escape(title),
    );
    if let Some(ws) = workspace {
        cmd.push_str(&format!(" --workspace {}", shelbi_agent::shell_escape(ws)));
    }
    run_tmux(["display-popup", "-E", "-w", "60", "-h", "9", cmd.as_str()])
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
        AgentRunnerSpec, Column, Machine, MachineKind, OrchestratorSpec, Project, Task,
        WorkspaceSpec,
    };
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::io::{Read, Write};
    use std::net::Shutdown;
    use std::os::unix::net::UnixListener;

    use crate::test_support::ENV_LOCK as TEST_LOCK;

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    fn serve_probe_hellos(
        listener: UnixListener,
        hellos: Vec<shelbi_state::DaemonHello>,
    ) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            for hello in hellos {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                stream.read_to_end(&mut request).unwrap();
                assert!(
                    request.is_empty(),
                    "version probes must negotiate with EOF, got {request:?}"
                );
                stream.write_all(hello.to_line().as_bytes()).unwrap();
                let _ = stream.shutdown(Shutdown::Both);
            }
        })
    }

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

    #[test]
    fn daemon_version_refresh_changes_mismatch_to_match_without_reload() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        let _home_env = EnvVarGuard::set("SHELBI_HOME", &home);
        // macOS limits Unix-domain socket paths to roughly 104 bytes.
        let sock =
            std::path::PathBuf::from(format!("/tmp/shb-tui-refresh-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _sock_env = EnvVarGuard::set("SHELBI_HUB_SOCK", &sock);
        let cli = env!("CARGO_PKG_VERSION");
        let server = serve_probe_hellos(
            listener,
            vec![
                shelbi_state::DaemonHello::new("0.1.0"),
                shelbi_state::DaemonHello::new(cli),
            ],
        );

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        assert!(app.daemon_version_mismatch);
        let stale = app.daemon_version_line.as_deref().unwrap();
        assert!(stale.contains("daemon 0.1.0"), "got: {stale}");
        assert!(stale.contains(&format!("cli {cli}")), "got: {stale}");
        assert!(stale.contains("shelbi daemon restart"), "got: {stale}");

        // The same long-lived App observes the relaunched current daemon on
        // its next ordinary refresh; no sidebar reconstruction/reload occurs.
        app.refresh().unwrap();
        assert!(!app.daemon_version_mismatch);
        assert_eq!(
            app.daemon_version_line.as_deref(),
            Some(format!("daemon {cli} · cli {cli}").as_str())
        );

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn daemon_version_display_identifies_protocol_skew() {
        let _lock = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        let _home_env = EnvVarGuard::set("SHELBI_HOME", &home);
        let sock =
            std::path::PathBuf::from(format!("/tmp/shb-tui-protocol-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _sock_env = EnvVarGuard::set("SHELBI_HUB_SOCK", &sock);
        let mut hello = shelbi_state::DaemonHello::new(env!("CARGO_PKG_VERSION"));
        hello.protocol = shelbi_state::HUB_PROTOCOL_VERSION + 1;
        let server = serve_probe_hellos(listener, vec![hello]);

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();

        assert!(app.daemon_version_mismatch);
        let line = app.daemon_version_line.as_deref().unwrap();
        assert!(
            line.contains(&format!(
                "socket protocol {}",
                shelbi_state::HUB_PROTOCOL_VERSION + 1
            )),
            "got: {line}"
        );
        assert!(
            line.contains(&format!(
                "this CLI speaks {}",
                shelbi_state::HUB_PROTOCOL_VERSION
            )),
            "got: {line}"
        );
        assert!(line.contains("shelbi daemon restart"), "got: {line}");

        server.join().unwrap();
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&home);
    }

    fn fixture_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            name: "demo".into(),
            label: None,
            display_name: None,
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/demo".into(),
                    host: None,
                    tags: Vec::new(),
                    forward: None,
                },
                Machine {
                    name: "devbox".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/demo".into(),
                    host: Some("devbox.local".into()),
                    tags: Vec::new(),
                    forward: None,
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
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "delta".into(),
                    machine: "devbox".into(),
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
            git: shelbi_core::GitConfig {
                branch_prefix: Some("shelbi".into()),
                ..Default::default()
            },
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
            column: Column::in_progress(),
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

        let workspaces = load_workspaces("demo").unwrap();
        assert_eq!(workspaces.len(), 2);

        let alpha = &workspaces[0];
        assert_eq!(alpha.name, "alpha");
        assert_eq!(alpha.machine, "hub");
        assert!(!alpha.is_remote);
        assert!(alpha.current_task.is_none());

        let delta = &workspaces[1];
        assert_eq!(delta.name, "delta");
        assert_eq!(delta.machine, "devbox");
        assert!(
            delta.is_remote,
            "ssh-machine workspaces must report is_remote=true"
        );
        assert_eq!(delta.current_task.as_deref(), Some("fix-thing"));
        // Default agent — the task carries no `agent:` in params, so the
        // sidebar surfaces the project's default task agent verbatim.
        assert_eq!(delta.agent.as_deref(), Some("developer"));
        assert!(
            alpha.agent.is_none(),
            "idle workspaces must not carry an agent name"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// A `config_error` renders an inline error row under a still-present
    /// `Workspaces` header instead of dropping the whole section — the core
    /// of AC1. Pure `rows()` check, no disk.
    #[test]
    fn rows_surface_config_error_under_workspaces_header() {
        let mut app = App::new_sidebar("Shelbi");
        app.config_error = Some(
            "project config ~/.shelbi/projects/Shelbi.yaml has an invalid id `Shelbi`".into(),
        );
        let rows = app.rows();
        assert!(
            rows.iter()
                .any(|r| matches!(r, Row::Section { label } if label == "Workspaces")),
            "the Workspaces header must stay so the error reads in context"
        );
        let err = rows
            .iter()
            .find_map(|r| match r {
                Row::ConfigError { message } => Some(message.clone()),
                _ => None,
            })
            .expect("a ConfigError row must be present when config_error is set");
        assert!(err.contains("Shelbi.yaml"), "error names the file: {err}");
        // The error row is inert — it can't be focused or activated.
        let err_row = rows
            .iter()
            .find(|r| matches!(r, Row::ConfigError { .. }))
            .unwrap();
        assert!(!err_row.is_selectable());
        assert!(err_row.view().is_none());
    }

    /// AC1 + AC2: a config file that is **present but fails validation** (a
    /// capitalized stem is not a valid shelbi id) sets `config_error` to the
    /// same actionable message the CLI produces — naming the file and reason —
    /// rather than silently emptying the workspaces list.
    #[test]
    fn refresh_flags_present_but_invalid_config() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        let _home_env = EnvVarGuard::set("SHELBI_HOME", &home);

        // Write `Shelbi.yaml` directly — `save_project` keys the filename off
        // `p.name`, and the on-disk `name:` is ignored on load (the id is the
        // stem), so this lands a parseable config under an invalid id.
        let mut proj = fixture_project();
        proj.name = "Shelbi".into();
        shelbi_state::save_project(&proj).unwrap();

        let mut app = App::new_sidebar("Shelbi");
        app.refresh().unwrap();

        let err = app
            .config_error
            .as_deref()
            .expect("a present-but-invalid config must set config_error");
        assert!(err.contains("Shelbi.yaml"), "names the offending file: {err}");
        assert!(err.contains("invalid id"), "states the reason: {err}");
        assert!(
            app.workspaces.is_empty(),
            "no workspaces load when the config is broken"
        );
        assert!(
            app.rows()
                .iter()
                .any(|r| matches!(r, Row::ConfigError { .. })),
            "the error surfaces as a ConfigError row"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// AC3: fixing the YAML makes the workspaces section reappear on the next
    /// ordinary refresh — no TUI restart. One long-lived `App` observes the
    /// broken config, then the repaired one.
    #[test]
    fn refresh_clears_config_error_when_yaml_fixed() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        let _home_env = EnvVarGuard::set("SHELBI_HOME", &home);

        // Broken first: unparseable YAML at the project's own (valid) stem.
        let projects = shelbi_state::projects_dir().unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("demo.yaml"), "name: demo\n: this is not: valid").unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        assert!(
            app.config_error.is_some(),
            "a present but unparseable config is flagged"
        );
        assert!(app.workspaces.is_empty());

        // Repair it in place, then refresh the same App — the section returns.
        shelbi_state::save_project(&fixture_project()).unwrap();
        app.refresh().unwrap();
        assert!(
            app.config_error.is_none(),
            "fixing the YAML clears the error without restart"
        );
        assert!(
            !app.workspaces.is_empty(),
            "workspaces reappear once the config loads"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// A genuinely missing project (no config file at all) is a fresh /
    /// half-set-up project, not a broken one: `config_error` stays `None` so
    /// the section is omitted quietly rather than flagged.
    #[test]
    fn refresh_ignores_genuinely_missing_project() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        let _home_env = EnvVarGuard::set("SHELBI_HOME", &home);

        let mut app = App::new_sidebar("ghost");
        app.refresh().unwrap();
        assert!(
            app.config_error.is_none(),
            "a missing config must not masquerade as a broken one"
        );
        assert!(app.workspaces.is_empty());
        assert!(
            !app.rows()
                .iter()
                .any(|r| matches!(r, Row::ConfigError { .. })),
            "no error row for a project that simply isn't set up yet"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// AC1: a `review`-tagged slot never appears under `— Workspaces —`; only
    /// dev workspaces list there, whether or not the project declares review
    /// slots.
    #[test]
    fn load_workspaces_excludes_review_tagged_slots() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut project = fixture_project();
        project.workspaces.push(WorkspaceSpec {
            name: "rev-1".into(),
            machine: "hub".into(),
            runner: "claude".into(),
            tags: vec!["review".into()],
            slot: None,
        });
        shelbi_state::save_project(&project).unwrap();

        let workspaces = load_workspaces("demo").unwrap();
        let names: Vec<_> = workspaces.iter().map(|w| w.name.as_str()).collect();
        assert!(names.contains(&"alpha"), "dev workspace listed: {names:?}");
        assert!(names.contains(&"delta"), "dev workspace listed: {names:?}");
        assert!(
            !names.contains(&"rev-1"),
            "review slot must be absent from the Workspaces section: {names:?}"
        );

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
                column: Column::in_progress(),
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
        assert_eq!(
            find_workspace_badge(&rows, "alpha").unwrap(),
            WorkspaceBadge::Working
        );
        // delta (idle remote) — Idle.
        assert_eq!(
            find_workspace_badge(&rows, "delta").unwrap(),
            WorkspaceBadge::Idle
        );

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

    /// [`fixture_project`] plus a `role: review` workspace (`review-1`) on hub,
    /// so tests can exercise the Ready-for-Review path (a task loaded onto a
    /// review worktree). review-1 is index 0 among hub's review workspaces, so
    /// its deterministic port is the default base (3000) → URL `hub:3000`.
    fn review_fixture_project() -> Project {
        let mut p = fixture_project();
        p.workspaces.push(shelbi_core::WorkspaceSpec {
            name: "review-1".into(),
            machine: "hub".into(),
            runner: "claude".into(),
            tags: vec!["review".to_string()],
            slot: None,
        });
        p
    }

    #[test]
    fn review_tasks_split_into_ready_and_queued_sections() {
        // Spec §16: a Review-status task loaded on a review workspace lands in
        // "Ready for Review" (✓) with its `machine:workspace` location; a
        // Review-status task still pinned to a dev workspace (or unassigned)
        // lands in "Queued for Review" (·) with no location. Both are two-line
        // rows carrying the branch.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = review_fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        // Loaded: assigned to the review workspace → Ready, location hub:review-1.
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "loaded".into(),
                title: "Palette fuzzy-match fix".into(),
                column: Column::review(),
                priority: 0,
                assigned_to: Some("review-1".into()),
                workflow: None,
                branch: Some("shelbi/palette-fix".into()),
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
        // Waiting: still on a dev workspace → Queued, no URL, branch fallback.
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "waiting".into(),
                title: "Rework onboarding copy".into(),
                column: Column::review(),
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

        assert_eq!(
            app.ready_review.len(),
            1,
            "one task loaded on a review worktree"
        );
        let ready = &app.ready_review[0];
        assert_eq!(ready.task_id, "loaded");
        assert_eq!(ready.branch, "shelbi/palette-fix");
        assert_eq!(ready.location.as_deref(), Some("hub:review-1"));

        assert_eq!(
            app.queued_review.len(),
            1,
            "one task still waiting for a slot"
        );
        let queued = &app.queued_review[0];
        assert_eq!(queued.task_id, "waiting");
        assert_eq!(
            queued.branch, "shelbi/waiting",
            "branch falls back to the configured prefix"
        );
        assert!(queued.location.is_none(), "queued tasks have no URL yet");

        // Row layout: a Ready section (✓, loaded row) precedes a Queued
        // section (·, waiting row); each review row carries its branch + flag.
        let rows = app.rows();
        let ready_hdr = rows
            .iter()
            .position(|r| matches!(r, Row::Section { label } if label == "Ready for Review"))
            .expect("Ready section renders");
        let queued_hdr = rows
            .iter()
            .position(|r| matches!(r, Row::Section { label } if label == "Queued for Review"))
            .expect("Queued section renders");
        assert!(ready_hdr < queued_hdr, "Ready section sits above Queued");
        assert!(matches!(
            &rows[ready_hdr + 1],
            Row::Review { ready: true, location: Some(loc), branch, .. }
                if loc == "hub:review-1" && branch == "shelbi/palette-fix"
        ));
        assert!(matches!(
            &rows[queued_hdr + 1],
            Row::Review { ready: false, location: None, branch, .. }
                if branch == "shelbi/waiting"
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn review_slot_is_absent_from_workspaces_even_when_serving_a_loaded_task() {
        // AC1: the Workspaces section lists dev slots only. Even a review slot
        // actively serving a loaded task (its task in the Review column,
        // assigned to it) must not appear there — its capacity lives in the
        // Ready/Queued for Review sections instead. This exercises the
        // "project *with* review workspaces" branch of the criterion.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = review_fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "loaded".into(),
                title: "loaded".into(),
                column: Column::review(),
                priority: 0,
                assigned_to: Some("review-1".into()),
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

        let workspaces = load_workspaces("demo").unwrap();
        assert!(
            workspaces.iter().all(|w| w.name != "review-1"),
            "review slot must not surface under Workspaces, got: {:?}",
            workspaces.iter().map(|w| &w.name).collect::<Vec<_>>()
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn row_at_maps_clicks_across_two_line_review_rows() {
        // Review rows are two lines tall; every other row is one. `row_at`
        // must walk cumulative heights so a click on a review row's *branch*
        // line resolves to the same row index as its title line, and rows
        // below a review entry don't drift by the extra line.
        let mut app = App::new_sidebar("demo");
        app.ready_review = vec![ReviewEntry {
            task_id: "r".into(),
            title: "Ready task".into(),
            branch: "shelbi/r".into(),
            location: Some("hub:3000".into()),
        }];
        app.queued_review = vec![ReviewEntry {
            task_id: "q".into(),
            title: "Queued task".into(),
            branch: "shelbi/q".into(),
            location: None,
        }];
        // No workspaces/agents so the row layout is exactly: 3 nav, blank,
        // Ready header, Ready review (h2), blank, Queued header, Queued
        // review (h2). Anchor the list at the buffer origin so a clicked line
        // maps directly to a cumulative-height offset.
        app.list_area = Rect::new(0, 0, 40, 20);

        let rows = app.rows();
        let nav_n = rows
            .iter()
            .take_while(|r| matches!(r, Row::Nav { .. }))
            .count();
        let ready_idx = rows
            .iter()
            .position(|r| matches!(r, Row::Review { ready: true, .. }))
            .unwrap();
        let queued_idx = rows
            .iter()
            .position(|r| matches!(r, Row::Review { ready: false, .. }))
            .unwrap();

        // The nav block renders as `2n + 1` lines (item + bracketing/between
        // separators), so the rest-of-list starts below it. `line_of` walks
        // cumulative heights from there, mirroring `row_at`'s own math.
        let rest_start = crate::sidebar::nav_lines(nav_n) as u16;
        let line_of = |idx: usize| -> u16 {
            rest_start
                + rows[nav_n..idx]
                    .iter()
                    .map(|r| row_height(r, 38) as u16)
                    .sum::<u16>()
        };

        // Both lines of the two-line Ready review row map to its index.
        let ready_line = line_of(ready_idx);
        assert_eq!(app.row_at(1, ready_line), Some(ready_idx), "title line");
        assert_eq!(
            app.row_at(1, ready_line + 1),
            Some(ready_idx),
            "branch line maps to the same review row"
        );

        // The Queued review row sits below the two-line Ready row without drift.
        let queued_line = line_of(queued_idx);
        assert_eq!(
            app.row_at(1, queued_line),
            Some(queued_idx),
            "queued title line"
        );
        assert_eq!(
            app.row_at(1, queued_line + 1),
            Some(queued_idx),
            "queued branch line maps to the same review row"
        );

        // A section header line is inert.
        let ready_hdr = line_of(
            rows.iter()
                .position(|r| matches!(r, Row::Section { label } if label == "Ready for Review"))
                .unwrap(),
        );
        assert_eq!(
            app.row_at(1, ready_hdr),
            None,
            "section headers aren't clickable"
        );
        // The first nav item renders on line 1 (line 0 is its bleed separator,
        // which is inert).
        assert_eq!(app.row_at(1, 0), None, "leading separator line is inert");
        assert_eq!(app.row_at(1, 1), Some(0), "first nav item is on line 1");
        assert_eq!(app.row_at(1, 3), Some(1), "second nav item is on line 3");
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
        let alpha =
            find_workspace_row(&rows, "alpha").expect("alpha must render at the section root");
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
                column: Column::in_progress(),
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
            Row::MachineGroup {
                collapsed: false,
                total: 1,
                active: 1,
                ..
            }
        ));
        assert!(matches!(
            devbox,
            Row::MachineGroup {
                collapsed: false,
                total: 1,
                active: 0,
                ..
            }
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
        assert!(matches!(
            hub,
            Row::MachineGroup {
                collapsed: true,
                ..
            }
        ));
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
        assert!(!shelbi_state::sidebar_collapsed_machines()
            .unwrap()
            .contains("hub"));

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
            shelbi_core::WorkspaceSpec {
                name: "charlie".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            },
            shelbi_core::WorkspaceSpec {
                name: "delta".into(),
                machine: "devbox".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
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
                    column: Column::in_progress(),
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
            Row::MachineGroup {
                collapsed: true,
                total: 3,
                active: 2,
                ..
            }
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
        assert!(!rows
            .iter()
            .any(|r| matches!(r, Row::MachineGroup { name, .. } if name == "ghost")));
        // Real machines are still rendered expanded — `ghost` doesn't
        // leak its collapse state to anyone else.
        let hub = rows
            .iter()
            .find(|r| matches!(r, Row::MachineGroup { name, .. } if name == "hub"))
            .expect("hub header must render");
        assert!(matches!(
            hub,
            Row::MachineGroup {
                collapsed: false,
                ..
            }
        ));

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
        assert!(
            devbox_idx > hub_idx,
            "machine headers must follow project declaration order"
        );
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
                column: Column::in_progress(),
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
                column: Column::in_progress(),
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

        let workspaces = load_workspaces("demo").unwrap();
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
                column: Column::in_progress(),
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
        assert_ne!(
            find_workspace_badge(&rows, "alpha").unwrap(),
            WorkspaceBadge::Idle
        );
        assert_eq!(
            find_workspace_badge(&rows, "delta").unwrap(),
            WorkspaceBadge::Idle
        );

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
                    column: Column::in_progress(),
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

        assert_eq!(
            find_workspace_badge(&rows, "alpha").unwrap(),
            WorkspaceBadge::Working
        );
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
                column: Column::in_progress(),
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
    fn workspace_badge_shows_pause_when_usage_limited() {
        // A usage-limited workspace (status.yaml recorded Paused by the poller)
        // renders the ⏸ pause badge, distinct from working/idle/awaiting, so a
        // slot stalled on the clock is visible at a glance.
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
                column: Column::in_progress(),
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
            state: WorkspaceState::Paused,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        let badge = find_workspace_badge(&rows, "alpha").unwrap();
        assert_eq!(badge, WorkspaceBadge::Paused);
        assert_eq!(badge.glyph(), "⏸");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_row_shows_no_completion_check_when_task_in_review_column() {
        // Spec §16: a dev workspace never carries a review-ready/completion
        // glyph. Once its task is promoted to Review the poller closes the
        // session, so even a still-assigned Review task (before the
        // orchestrator loads it onto a review workspace) reads Idle on the
        // workspace row — completion lives in the review sections instead.
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
                column: Column::review(),
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
        // A stale "working" status.yaml must not resurrect a badge either —
        // the task is out of the in-progress column, so the row is Idle.
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
            WorkspaceBadge::Idle
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
        assert_eq!(
            find_workspace_badge(&rows, "alpha").unwrap(),
            WorkspaceBadge::Idle
        );

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
        // The fixture declares no `role: review` workspace, so a Review-status
        // task (assigned to a dev workspace) lands in the **Queued for Review**
        // section — nothing is loaded onto a review worktree, so there's no
        // Ready section. The row is two-line: title on line 1, branch on
        // line 2 (the configured-prefix fallback since the task pins none).
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        assert!(
            !app.rows()
                .iter()
                .any(|r| matches!(r, Row::Section { label } if label == "Queued for Review")),
            "empty review queue must not render the section header"
        );

        let now = Utc::now();
        shelbi_state::save_task(
            "demo",
            &Task {
                id: "ready".into(),
                title: "Fix login".into(),
                column: Column::review(),
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
            .position(|r| matches!(r, Row::Section { label } if label == "Queued for Review"))
            .expect("populated review queue must render the section header");
        assert!(matches!(
            &rows[section_idx + 1],
            Row::Review { title, branch, location, ready, .. }
                if title == "Fix login"
                    && branch == "shelbi/ready"
                    && location.is_none()
                    && !*ready
        ));
        // No Ready section — the task is queued, not loaded on a review slot.
        assert!(
            !rows
                .iter()
                .any(|r| matches!(r, Row::Section { label } if label == "Ready for Review")),
            "a task on a dev workspace is queued, never Ready"
        );
        // Exactly one blank row separates the preceding list from the Queued
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

        // Ready (loaded) → cyan ✓.
        let review = Row::Review {
            title: "Fix login".into(),
            branch: "shelbi/ready".into(),
            location: Some("hub:3000".into()),
            ready: true,
            view: View::ReviewTask("ready".into()),
        };
        let d = review.decoration().unwrap();
        assert_eq!(d.glyph, "✓");
        assert_eq!(d.color, DecorationColor::Cyan);

        // Queued (waiting) → dim ·.
        let queued = Row::Review {
            title: "Rework onboarding".into(),
            branch: "shelbi/rework".into(),
            location: None,
            ready: false,
            view: View::ReviewTask("rework".into()),
        };
        let d = queued.decoration().unwrap();
        assert_eq!(d.glyph, "·");
        assert_eq!(d.color, DecorationColor::DarkGray);

        let agent = Row::LegacyAgent {
            id: "spawn-1".into(),
            machine: "hub".into(),
            status: shelbi_core::Status::Running,
            view: View::Agent("spawn-1".into()),
        };
        let d = agent.decoration().unwrap();
        assert_eq!(d.glyph, shelbi_core::Status::Running.glyph());
        assert_eq!(d.color, DecorationColor::Green);

        assert!(Row::Section {
            label: "Workspaces".into()
        }
        .decoration()
        .is_none());
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
            .find(|l| l.contains("project=demo mode=zen off -> on reason=user:hotkey"));
        let off_line = log
            .lines()
            .find(|l| l.contains("project=demo mode=zen on -> off reason=user:hotkey"));
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

    #[test]
    fn recover_render_panic_counts_the_recovery_and_carries_the_panic_text() {
        let mut app = App::new_sidebar("demo");
        let msg = app.recover_render_panic("index out of bounds");
        assert_eq!(app.render_panics, 1);
        assert!(msg.contains("index out of bounds"), "carries the panic text: {msg}");
        assert!(msg.contains("repainting"), "names the recovery action: {msg}");
        assert!(msg.contains("recovery #1"), "carries the counter: {msg}");
    }

    #[test]
    fn recover_render_panic_bumps_the_counter_each_time() {
        let mut app = App::new_sidebar("demo");
        let first = app.recover_render_panic("boom");
        let second = app.recover_render_panic("boom again");
        assert_eq!(app.render_panics, 2, "each caught panic bumps the counter");
        assert!(first.contains("recovery #1"), "first recovery: {first}");
        assert!(second.contains("recovery #2"), "counter climbs: {second}");
    }

    #[test]
    fn note_sidebar_area_signals_only_on_a_collapsed_pane() {
        let mut app = App::new_sidebar("demo");
        let t0 = Instant::now();
        // A healthy pane is silent.
        assert!(app.note_sidebar_area(40, 24, t0).is_none());
        // A zero/one-column pane reads as "gone" — emit a diagnostic.
        let signal = app.note_sidebar_area(0, 24, t0).expect("collapse must signal");
        assert!(signal.contains("collapsed to 0x24"), "got: {signal}");
    }

    #[test]
    fn note_sidebar_area_throttles_repeat_collapse_signals() {
        let mut app = App::new_sidebar("demo");
        let t0 = Instant::now();
        // First collapse logs; a rapid repeat within the throttle window is
        // suppressed so the 200ms render cadence can't flood tui.log.
        assert!(app.note_sidebar_area(1, 24, t0).is_some());
        assert!(app
            .note_sidebar_area(1, 24, t0 + Duration::from_millis(200))
            .is_none());
        // Past the throttle window it logs again.
        assert!(app
            .note_sidebar_area(1, 24, t0 + COLLAPSE_WARN_THROTTLE + Duration::from_millis(1))
            .is_some());
        // A recovery to a healthy size clears the throttle so the next
        // collapse signals immediately.
        assert!(app
            .note_sidebar_area(40, 24, t0 + Duration::from_secs(10))
            .is_none());
        assert!(app
            .note_sidebar_area(1, 24, t0 + Duration::from_secs(10))
            .is_some());
    }

    #[test]
    fn poll_review_load_surfaces_success_and_clears_the_job() {
        let mut app = App::new_sidebar("demo");
        let (tx, rx) = channel();
        app.review_job = Some(ReviewLoadJob {
            task_id: "t-1".into(),
            workspace: "review-1".into(),
            rx,
            started: Instant::now(),
        });
        tx.send(Ok("shelbi-demo:review-1".into())).unwrap();
        app.poll_review_load();
        assert!(app.review_job.is_none(), "a finished load clears the job");
        assert!(
            app.status_line.contains("▶ reviewing t-1"),
            "got: {}",
            app.status_line
        );
    }

    #[test]
    fn poll_review_load_surfaces_failure_and_clears_the_job() {
        let mut app = App::new_sidebar("demo");
        let (tx, rx) = channel();
        app.review_job = Some(ReviewLoadJob {
            task_id: "t-1".into(),
            workspace: "review-1".into(),
            rx,
            started: Instant::now(),
        });
        tx.send(Err("every review slot is busy".into())).unwrap();
        app.poll_review_load();
        assert!(app.review_job.is_none());
        assert!(
            app.status_line.contains("review load failed")
                && app.status_line.contains("busy"),
            "got: {}",
            app.status_line
        );
    }

    #[test]
    fn start_review_load_is_a_no_op_while_a_load_is_in_flight() {
        let mut app = App::new_sidebar("demo");
        // Keep the sender alive so the pre-existing job reads as still running.
        let (_tx, rx) = channel::<std::result::Result<String, String>>();
        app.review_job = Some(ReviewLoadJob {
            task_id: "first".into(),
            workspace: "review-1".into(),
            rx,
            started: Instant::now(),
        });
        // A second kick-off (e.g. a Ready row whose window is being launched)
        // while the first load is still in flight is refused — the existing
        // job is left untouched so two activations can't stack dispatches onto
        // a review slot. Guard returns before any thread spawn or disk write.
        app.start_review_load("second".into(), "review-2".into());
        let job = app.review_job.as_ref().expect("job stays present");
        assert_eq!(job.task_id, "first", "the in-flight job is not replaced");
        assert!(
            app.status_line.contains("already in progress"),
            "got: {}",
            app.status_line
        );
    }

    #[test]
    fn poll_review_load_animates_a_spinner_while_in_flight() {
        let mut app = App::new_sidebar("demo");
        // Keep the sender alive so the channel stays connected but empty.
        let (_tx, rx) = channel::<std::result::Result<String, String>>();
        app.review_job = Some(ReviewLoadJob {
            task_id: "t-1".into(),
            workspace: "review-1".into(),
            rx,
            started: Instant::now(),
        });
        app.poll_review_load();
        // Still pending: the job survives and the status line shows progress.
        assert!(app.review_job.is_some(), "an unfinished load stays pending");
        assert!(
            app.status_line.contains("loading t-1 onto review-1"),
            "got: {}",
            app.status_line
        );
    }
}
