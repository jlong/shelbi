use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use shelbi_core::{Agent, Column, Status};
use shelbi_palette::{Decoration, DecorationColor};
use shelbi_state::{
    keymap::Keymaps, load_worker_status, read_state, TaskFile, WorkerState, ZenModeState,
    ZenToggleChord,
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
    /// A declared worker (from project YAML) — switch tmux to its pane
    /// (local: window in the project session; remote: a proxy window in
    /// the project session that ssh-attaches to the worker's remote session).
    Worker(String),
    /// A legacy `shelbi spawn` agent — switch tmux to its window. Workers
    /// from the modern task-board flow surface as [`View::Worker`] instead.
    Agent(String),
    /// A task in the review queue — trigger the review checkout flow and
    /// focus the review pane.
    ReviewTask(String),
}

/// Sidebar view of a declared worker — the bits we need to render and
/// activate it. Built fresh each refresh from the project YAML + the
/// in-progress task column.
#[derive(Debug, Clone)]
pub struct WorkerOverview {
    pub name: String,
    pub machine: String,
    pub is_remote: bool,
    /// `Some(task_id)` if this worker is currently assigned an in_progress
    /// task — drives the busy/idle indicator.
    pub current_task: Option<String>,
    /// Single-char state glyph derived from the worker's status file and
    /// the task board state.
    pub badge: WorkerBadge,
}

/// Per-worker state glyph shown in the sidebar. Derived each refresh from
/// the task board (review-ready / idle) and from
/// `~/.shelbi/workers/<name>/status.yaml` (working / awaiting-input /
/// awaiting-permission), which the [`crate::WorkerPoller`] writes from the
/// worker pane's `shelbi:<state>` title marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerBadge {
    /// ⏵ — claude is actively running a turn.
    Working,
    /// 💬 — claude finished a turn and is sitting at the prompt.
    AwaitingInput,
    /// ⚠ — claude is showing a permission dialog.
    AwaitingPermission,
    /// ✓ — the worker's task has been moved to the review column.
    ReviewReady,
    /// · — no in-flight task assigned.
    Idle,
}

impl WorkerBadge {
    /// Single-char glyph — paired with one trailing space in the renderer
    /// so the badge column stays narrow on small terminals.
    pub fn glyph(self) -> &'static str {
        match self {
            WorkerBadge::Working => "⏵",
            WorkerBadge::AwaitingInput => "💬",
            WorkerBadge::AwaitingPermission => "⚠",
            WorkerBadge::ReviewReady => "✓",
            WorkerBadge::Idle => "·",
        }
    }

    /// Color the glyph paints in. Shared by the sidebar render path and
    /// the palette so a worker row gets the same tint in both surfaces.
    pub fn decoration_color(self) -> DecorationColor {
        match self {
            WorkerBadge::Working => DecorationColor::Green,
            WorkerBadge::AwaitingInput => DecorationColor::Yellow,
            WorkerBadge::AwaitingPermission => DecorationColor::Red,
            WorkerBadge::ReviewReady => DecorationColor::Cyan,
            WorkerBadge::Idle => DecorationColor::DarkGray,
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
    pub workers: Vec<WorkerOverview>,
    pub review_queue: Vec<TaskFile>,
    pub sidebar_index: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    pub should_quit: bool,
    /// Latest Zen Mode state read from `state.json`. Drives the green pill
    /// in the lower-left status block and the Alt+Z toggle direction.
    pub zen_mode: ZenModeState,
    /// Chord that toggles Zen Mode — picked by the first-run probe, loaded
    /// from `~/.shelbi/config.yaml::keymap.zen_toggle`. Defaults to Alt+Z
    /// before the probe runs (matches the fresh-install spec).
    pub zen_toggle_chord: ZenToggleChord,
    /// Merged keymaps for every TUI mode — populated once at startup by
    /// `run_sidebar` calling [`shelbi_state::keymap::load_keymaps`]. The
    /// sidebar handler dispatches `KeyEvent`s through `keymaps.global` then
    /// `keymaps.sidebar`. Default is empty bindings; callers that exercise
    /// the handler (entry points, tests) replace this before dispatching.
    pub keymaps: Keymaps,
    /// Screen-space rect occupied by the rendered row list — written each
    /// frame by the sidebar renderer and read by the mouse-click handler to
    /// map a click coordinate back to a row index.
    pub list_area: Rect,
}

impl App {
    pub fn new_sidebar(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            agents: Vec::new(),
            workers: Vec::new(),
            review_queue: Vec::new(),
            sidebar_index: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            should_quit: false,
            zen_mode: ZenModeState::Off,
            zen_toggle_chord: ZenToggleChord::AltZ,
            keymaps: Keymaps::default(),
            list_area: Rect::default(),
        }
    }

    /// Borrow the keymaps populated at startup by `run_sidebar`. The
    /// sidebar handler reads this once per loop entry rather than
    /// re-parsing `keys.yml` per keystroke.
    pub fn keymaps(&self) -> &Keymaps {
        &self.keymaps
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
    /// declared workers under an `— agents —` separator, then the review
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
        if !self.workers.is_empty() {
            rows.push(Row::Blank);
            rows.push(Row::Section {
                label: "Agents".into(),
            });
            for w in &self.workers {
                rows.push(Row::Worker {
                    name: w.name.clone(),
                    badge: w.badge,
                    view: View::Worker(w.name.clone()),
                });
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
                    worker: tf.task.assigned_to.clone(),
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
        self.workers =
            load_workers(&self.project_name, &self.review_queue).unwrap_or_default();
        // A missing state.json is normal (fresh project): default to Off so
        // the pill stays hidden rather than flashing then disappearing.
        self.zen_mode = read_state(&self.project_name)
            .map(|s| s.zen_mode)
            .unwrap_or(ZenModeState::Off);
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
    /// window (orchestrator → dashboard's right pane; agent → its window).
    pub fn activate_selection(&mut self) {
        if let Some(row) = self.rows().get(self.sidebar_index).cloned() {
            if let Some(view) = row.view().cloned() {
                self.activate_view(&view);
            }
        }
    }

    pub fn activate_view(&mut self, view: &View) {
        match view {
            View::Builtin(name) => match shelbi_orchestrator::show_view(&self.project_name, name) {
                Ok(()) => self.status_line = format!("▶ {name}"),
                Err(e) => self.status_line = format!("show view `{name}` failed: {e}"),
            },
            View::Worker(name) => {
                match shelbi_orchestrator::focus_worker(&self.project_name, name) {
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
                         (remote workers need `tmux attach -t shelbi-w-{id}` for now)"
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
    /// A declared worker, with its current state badge.
    Worker {
        name: String,
        badge: WorkerBadge,
        view: View,
    },
    /// A task sitting in the review column — title + the worker who
    /// finished it.
    Review {
        title: String,
        worker: Option<String>,
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
        !matches!(self, Row::Section { .. } | Row::Blank)
    }

    pub fn view(&self) -> Option<&View> {
        match self {
            Row::Nav { view, .. }
            | Row::Worker { view, .. }
            | Row::Review { view, .. }
            | Row::LegacyAgent { view, .. } => Some(view),
            Row::Section { .. } | Row::Blank => None,
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
            Row::Worker { badge, .. } => Some(badge.decoration()),
            Row::Review { .. } => Some(Decoration {
                glyph: "✓".into(),
                color: DecorationColor::Cyan,
            }),
            Row::LegacyAgent { status, .. } => Some(status_decoration(*status)),
            Row::Section { .. } | Row::Blank => None,
        }
    }
}

/// Build the sidebar's view of declared workers from the project YAML, the
/// in-progress task column, and the review queue (passed in so we don't
/// scan tasks twice per refresh). One disk read per worker for the
/// `status.yaml` lookup. Returns an empty vec if the project YAML or task
/// dir is missing.
fn load_workers(project: &str, review_queue: &[TaskFile]) -> Result<Vec<WorkerOverview>> {
    let p = match shelbi_state::load_project(project) {
        Ok(p) => p,
        Err(_) => return Ok(Vec::new()),
    };
    let in_progress =
        shelbi_state::list_column(project, Column::InProgress).unwrap_or_default();
    let mut out = Vec::with_capacity(p.workers.len());
    for worker in &p.workers {
        let machine = match p.machine(&worker.machine) {
            Some(m) => m,
            None => continue, // mis-configured worker, skip silently
        };
        let is_remote = !machine.host().is_local();
        let current_task = in_progress
            .iter()
            .find(|tf| tf.task.assigned_to.as_deref() == Some(worker.name.as_str()))
            .map(|tf| tf.task.id.clone());
        let has_review = review_queue
            .iter()
            .any(|tf| tf.task.assigned_to.as_deref() == Some(worker.name.as_str()));
        let badge = derive_worker_badge(&worker.name, current_task.is_some(), has_review);
        out.push(WorkerOverview {
            name: worker.name.clone(),
            machine: worker.machine.clone(),
            is_remote,
            current_task,
            badge,
        });
    }
    Ok(out)
}

/// Pick the badge for a worker given the task-board signals + an on-disk
/// state read. Review-ready wins over claude state — once a task is sent
/// for review the worker is conceptually done with it even if claude is
/// still mid-turn. Idle wins when there's no in-progress task at all, so
/// a stale `status.yaml` from a previous run doesn't show "working" for a
/// worker that has nothing to do.
fn derive_worker_badge(
    worker_name: &str,
    has_in_progress: bool,
    has_review: bool,
) -> WorkerBadge {
    if has_review {
        return WorkerBadge::ReviewReady;
    }
    if !has_in_progress {
        return WorkerBadge::Idle;
    }
    match load_worker_status(worker_name).ok().flatten() {
        Some(s) => match s.state {
            WorkerState::Working => WorkerBadge::Working,
            WorkerState::AwaitingInput => WorkerBadge::AwaitingInput,
            WorkerState::Blocked => WorkerBadge::AwaitingPermission,
        },
        // Task assigned but the poller hasn't observed a marker yet. Show
        // working as the best guess — it'll firm up within one poll tick.
        None => WorkerBadge::Working,
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
        AgentRunnerSpec, Column, Machine, MachineKind, OrchestratorSpec, Project, Task, WorkerSpec,
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
            workers: vec![
                WorkerSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                },
                WorkerSpec {
                    name: "delta".into(),
                    machine: "devbox".into(),
                    runner: "claude".into(),
                },
            ],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn load_workers_surfaces_local_and_remote_with_in_progress_task() {
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

        let workers = load_workers("demo", &[]).unwrap();
        assert_eq!(workers.len(), 2);

        let alpha = &workers[0];
        assert_eq!(alpha.name, "alpha");
        assert_eq!(alpha.machine, "hub");
        assert!(!alpha.is_remote);
        assert!(alpha.current_task.is_none());

        let delta = &workers[1];
        assert_eq!(delta.name, "delta");
        assert_eq!(delta.machine, "devbox");
        assert!(delta.is_remote, "ssh-machine workers must report is_remote=true");
        assert_eq!(delta.current_task.as_deref(), Some("fix-thing"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn rows_include_workers_with_idle_and_working_badges() {
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

        // 3 nav + 1 blank spacer + 1 `agents` section header + 2 workers = 7 rows.
        let rows = app.rows();
        assert_eq!(rows.len(), 7);
        assert!(matches!(&rows[3], Row::Blank));
        assert!(matches!(&rows[4], Row::Section { label } if label == "Agents"));

        // alpha (busy, no status file yet) — default to Working.
        assert_eq!(find_worker_badge(&rows, "alpha").unwrap(), WorkerBadge::Working);
        // delta (idle remote) — Idle.
        assert_eq!(find_worker_badge(&rows, "delta").unwrap(), WorkerBadge::Idle);

        std::env::remove_var("SHELBI_HOME");
    }

    fn find_worker_badge(rows: &[Row], name: &str) -> Option<WorkerBadge> {
        rows.iter().find_map(|r| match r {
            Row::Worker { name: n, badge, .. } if n == name => Some(*badge),
            _ => None,
        })
    }

    #[test]
    fn worker_badge_reflects_status_yaml_when_task_in_progress() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        for (id, worker, state) in [
            ("t-work", "alpha", WorkerState::Working),
            ("t-wait", "delta", WorkerState::AwaitingInput),
        ] {
            shelbi_state::save_task(
                "demo",
                &Task {
                    id: id.into(),
                    title: id.into(),
                    column: Column::InProgress,
                    priority: 0,
                    assigned_to: Some(worker.into()),
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
            shelbi_state::save_worker_status(&shelbi_state::WorkerStatus {
                worker: worker.into(),
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

        assert_eq!(find_worker_badge(&rows, "alpha").unwrap(), WorkerBadge::Working);
        assert_eq!(
            find_worker_badge(&rows, "delta").unwrap(),
            WorkerBadge::AwaitingInput
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_badge_shows_awaiting_permission_when_blocked() {
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
        shelbi_state::save_worker_status(&shelbi_state::WorkerStatus {
            worker: "alpha".into(),
            current_task: Some("t".into()),
            state: WorkerState::Blocked,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        assert_eq!(
            find_worker_badge(&rows, "alpha").unwrap(),
            WorkerBadge::AwaitingPermission
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_badge_shows_review_ready_when_task_in_review_column() {
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
        // once the task moves to review, the worker is conceptually done.
        shelbi_state::save_worker_status(&shelbi_state::WorkerStatus {
            worker: "alpha".into(),
            current_task: None,
            state: WorkerState::Working,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        assert_eq!(
            find_worker_badge(&rows, "alpha").unwrap(),
            WorkerBadge::ReviewReady
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_badge_idle_overrides_stale_status_yaml_when_no_task() {
        // status.yaml says working but no in-progress task is assigned —
        // probably a leftover from a finished task. Show idle so the user
        // isn't misled.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project();
        shelbi_state::save_project(&project).unwrap();

        let now = Utc::now();
        shelbi_state::save_worker_status(&shelbi_state::WorkerStatus {
            worker: "alpha".into(),
            current_task: None,
            state: WorkerState::Working,
            last_transition: now,
            last_seen: now,
        })
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();
        let rows = app.rows();
        assert_eq!(find_worker_badge(&rows, "alpha").unwrap(), WorkerBadge::Idle);

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
            Row::Review { title, worker, .. } if title == "Fix login" && worker.as_deref() == Some("delta")
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
        // should skip the blank spacer (idx 3) and the `agents` section
        // header (idx 4) and land on the first worker row (idx 5).
        app.sidebar_index = 2;
        app.nav_down();
        let rows = app.rows();
        assert!(rows[app.sidebar_index].is_selectable());
        assert!(
            matches!(&rows[app.sidebar_index], Row::Worker { .. }),
            "nav_down past a section header must land on a worker, got {:?}",
            app.sidebar_index
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_badge_glyphs_are_single_chars() {
        // Sidebar column must stay narrow for small terminals. Each badge
        // glyph is one (possibly multi-byte) Unicode char — never a
        // multi-char string.
        for b in [
            WorkerBadge::Working,
            WorkerBadge::AwaitingInput,
            WorkerBadge::AwaitingPermission,
            WorkerBadge::ReviewReady,
            WorkerBadge::Idle,
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

        let worker = Row::Worker {
            name: "alpha".into(),
            badge: WorkerBadge::AwaitingPermission,
            view: View::Worker("alpha".into()),
        };
        let d = worker.decoration().unwrap();
        assert_eq!(d.glyph, WorkerBadge::AwaitingPermission.glyph());
        assert_eq!(d.color, DecorationColor::Red);

        let review = Row::Review {
            title: "Fix login".into(),
            worker: Some("delta".into()),
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

        assert!(Row::Section { label: "Agents".into() }.decoration().is_none());
        assert!(Row::Blank.decoration().is_none());
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
