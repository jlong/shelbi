use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use shelbi_core::{Agent, Column, Host, Status};
use shelbi_state::{load_worker_status, TaskFile, WorkerState};

/// What's currently highlighted in the sidebar — drives selection logic
/// only; the right pane (orchestrator / agent) is a real tmux pane, not
/// rendered by this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// One of the built-in views hosted as a hidden tmux pane: swap it
    /// into the dashboard's right slot.
    Builtin(&'static str), // "orch" | "tasks" | "review" | "machines"
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
}

pub struct App {
    pub project_name: String,
    /// User-chosen name for the assistant (from `~/.shelbi/shelbi.yaml`).
    /// Surfaces in the sidebar as the label on the orchestrator row;
    /// falls back to [`shelbi_state::DEFAULT_ASSISTANT_NAME`] when the
    /// onboarding wizard hasn't run yet.
    pub assistant_name: String,
    pub agents: Vec<Agent>,
    pub workers: Vec<WorkerOverview>,
    pub review_queue: Vec<TaskFile>,
    pub sidebar_index: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    pub should_quit: bool,
    /// Screen-space rect occupied by the rendered row list — written each
    /// frame by the sidebar renderer and read by the mouse-click handler to
    /// map a click coordinate back to a row index.
    pub list_area: Rect,
}

impl App {
    pub fn new_sidebar(project_name: impl Into<String>) -> Self {
        let assistant_name = shelbi_state::load_shelbi_config()
            .map(|c| c.assistant_name().to_string())
            .unwrap_or_else(|_| shelbi_state::DEFAULT_ASSISTANT_NAME.to_string());
        Self {
            project_name: project_name.into(),
            assistant_name,
            agents: Vec::new(),
            workers: Vec::new(),
            review_queue: Vec::new(),
            sidebar_index: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            should_quit: false,
            list_area: Rect::default(),
        }
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
        if idx < self.rows().len() {
            Some(idx)
        } else {
            None
        }
    }

    /// Sidebar rows: built-in views, declared workers (with machine badge),
    /// review queue (one per task awaiting review), then any legacy
    /// `shelbi spawn` agents.
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![
            Row {
                label: self.assistant_name.clone(),
                view: View::Builtin("orch"),
                badge: None,
                status: None,
                worker_badge: None,
            },
            Row {
                label: "tasks".into(),
                view: View::Builtin("tasks"),
                badge: None,
                status: None,
                worker_badge: None,
            },
            Row {
                label: "review".into(),
                view: View::Builtin("review"),
                badge: if self.review_queue.is_empty() {
                    None
                } else {
                    Some(format!("{}", self.review_queue.len()))
                },
                status: None,
                worker_badge: None,
            },
            Row {
                label: "machines".into(),
                view: View::Builtin("machines"),
                badge: None,
                status: None,
                worker_badge: None,
            },
        ];
        for w in &self.workers {
            rows.push(Row {
                label: w.name.clone(),
                view: View::Worker(w.name.clone()),
                badge: Some(w.machine.clone()),
                status: None,
                worker_badge: Some(w.badge),
            });
        }
        for tf in &self.review_queue {
            rows.push(Row {
                label: tf.task.title.clone(),
                view: View::ReviewTask(tf.task.id.clone()),
                badge: tf.task.assigned_to.clone(),
                status: None,
                worker_badge: None,
            });
        }
        for a in &self.agents {
            rows.push(Row {
                label: a.id.clone(),
                view: View::Agent(a.id.clone()),
                badge: Some(a.machine.clone()),
                status: Some(a.status),
                worker_badge: None,
            });
        }
        rows
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.agents = load_agents(&self.project_name).unwrap_or_default();
        self.review_queue =
            shelbi_state::list_column(&self.project_name, Column::Review).unwrap_or_default();
        self.workers =
            load_workers(&self.project_name, &self.review_queue).unwrap_or_default();
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
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        self.sidebar_index = if self.sidebar_index == 0 {
            n - 1
        } else {
            self.sidebar_index - 1
        };
    }

    pub fn nav_down(&mut self) {
        let n = self.rows().len();
        if n == 0 {
            return;
        }
        self.sidebar_index = (self.sidebar_index + 1) % n;
    }

    /// Act on the currently highlighted row: tmux-select the matching
    /// window (orchestrator → dashboard's right pane; agent → its window).
    pub fn activate_selection(&mut self) {
        if let Some(row) = self.rows().get(self.sidebar_index).cloned() {
            self.activate_view(&row.view);
        }
    }

    pub fn activate_view(&mut self, view: &View) {
        match view {
            View::Builtin(name) => match shelbi_orchestrator::show_view(&self.project_name, name) {
                Ok(()) => self.status_line = format!("▶ {name}"),
                Err(e) => self.status_line = format!("show view `{name}` failed: {e}"),
            },
            View::Worker(name) => match self.focus_worker(name) {
                Ok(()) => self.status_line = format!("▶ {name}"),
                Err(e) => self.status_line = format!("focus `{name}` failed: {e}"),
            },
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

    /// Switch the dashboard window to the worker's pane.
    ///
    /// Local workers live in a window named after the worker inside the
    /// project session (placed there by `shelbi task start`). Remote workers
    /// live in their own tmux session on the remote machine — we surface
    /// them by maintaining a *proxy window* in the project session, named
    /// after the worker, whose command is `ssh -t <host> tmux attach -t
    /// shelbi-w-<worker>`. The proxy is created lazily on first selection
    /// and re-used on subsequent selections; closing it (e.g. detaching
    /// from the remote tmux) lets the next selection spawn a fresh one.
    fn focus_worker(&self, name: &str) -> Result<()> {
        let project = shelbi_state::load_project(&self.project_name)
            .map_err(|e| anyhow::anyhow!("load project: {e}"))?;
        let worker = project.worker(name).ok_or_else(|| {
            anyhow::anyhow!("worker `{name}` not declared in project YAML")
        })?;
        let machine = project.machine(&worker.machine).ok_or_else(|| {
            anyhow::anyhow!("worker `{name}` references unknown machine `{}`", worker.machine)
        })?;

        let project_session = format!("shelbi-{}", self.project_name);
        let target = format!("{project_session}:{}", worker.name);

        // Window already in the project session — local worker window OR a
        // remote proxy window we created earlier. Just switch to it.
        if run_tmux(["select-window", "-t", &target]) {
            return Ok(());
        }

        match machine.host() {
            Host::Local => Err(anyhow::anyhow!(
                "worker has no live pane — assign a task with \
                 `shelbi task start <task> --worker {name}`"
            )),
            Host::Ssh { host } => {
                let remote_session = format!("shelbi-w-{}", worker.name);
                let cmd = format!(
                    "ssh -t {host} tmux attach -t {remote_session}",
                    host = shelbi_agent::shell_escape(&host),
                    remote_session = shelbi_agent::shell_escape(&remote_session),
                );
                let ok = run_tmux([
                    "new-window",
                    "-t",
                    &format!("{project_session}:"),
                    "-n",
                    &worker.name,
                    "sh",
                    "-c",
                    &cmd,
                ]);
                if !ok {
                    return Err(anyhow::anyhow!(
                        "couldn't open proxy window for remote worker `{name}` on `{host}`"
                    ));
                }
                let _ = run_tmux(["select-window", "-t", &target]);
                Ok(())
            }
        }
    }

    fn start_review(&self, id: &str) -> Result<String> {
        let project = shelbi_state::load_project(&self.project_name)?;
        let tf = shelbi_state::load_task(&self.project_name, id)?;
        let machine =
            shelbi_orchestrator::review::resolve_review_machine(&project, &tf.task, None)?;
        let addr = shelbi_orchestrator::review::start_review(
            shelbi_orchestrator::review::ReviewSpec {
                project: &project,
                machine,
                task: &tf.task,
                task_body: &tf.body,
            },
        )?;
        Ok(addr.target())
    }
}

#[derive(Clone)]
pub struct Row {
    pub label: String,
    pub view: View,
    pub badge: Option<String>,
    /// Glyph for legacy `shelbi spawn` agent rows. Workers use
    /// [`Self::worker_badge`] instead.
    pub status: Option<Status>,
    /// Glyph for declared-worker rows — derived from the worker's
    /// `status.yaml` plus the task board. `None` on built-ins, review
    /// tasks, and legacy agents.
    pub worker_badge: Option<WorkerBadge>,
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
    use std::sync::Mutex;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

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
            branch: Some("shelbi/fix-thing".into()),
            depends_on: Vec::new(),
            created_at: now,
            updated_at: now,
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
                branch: None,
                depends_on: Vec::new(),
                created_at: now,
                updated_at: now,
            },
            "",
        )
        .unwrap();

        let mut app = App::new_sidebar("demo");
        app.refresh().unwrap();

        // 4 built-ins + 2 workers + 0 review + 0 legacy agents.
        let rows = app.rows();
        assert_eq!(rows.len(), 6);

        // alpha (busy, no status file yet) — default to Working.
        let alpha = rows.iter().find(|r| matches!(&r.view, View::Worker(n) if n == "alpha")).unwrap();
        assert_eq!(alpha.badge.as_deref(), Some("hub"));
        assert_eq!(alpha.worker_badge, Some(WorkerBadge::Working));
        assert!(alpha.status.is_none());

        // delta (idle remote) — Idle.
        let delta = rows.iter().find(|r| matches!(&r.view, View::Worker(n) if n == "delta")).unwrap();
        assert_eq!(delta.badge.as_deref(), Some("devbox"));
        assert_eq!(delta.worker_badge, Some(WorkerBadge::Idle));

        std::env::remove_var("SHELBI_HOME");
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
                    branch: None,
                    depends_on: Vec::new(),
                    created_at: now,
                    updated_at: now,
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

        let alpha = rows.iter().find(|r| matches!(&r.view, View::Worker(n) if n == "alpha")).unwrap();
        assert_eq!(alpha.worker_badge, Some(WorkerBadge::Working));

        let delta = rows.iter().find(|r| matches!(&r.view, View::Worker(n) if n == "delta")).unwrap();
        assert_eq!(delta.worker_badge, Some(WorkerBadge::AwaitingInput));

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
                branch: None,
                depends_on: Vec::new(),
                created_at: now,
                updated_at: now,
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
        let alpha = app
            .rows()
            .into_iter()
            .find(|r| matches!(&r.view, View::Worker(n) if n == "alpha"))
            .unwrap();
        assert_eq!(alpha.worker_badge, Some(WorkerBadge::AwaitingPermission));

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
                branch: None,
                depends_on: Vec::new(),
                created_at: now,
                updated_at: now,
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
        let alpha = app
            .rows()
            .into_iter()
            .find(|r| matches!(&r.view, View::Worker(n) if n == "alpha"))
            .unwrap();
        assert_eq!(alpha.worker_badge, Some(WorkerBadge::ReviewReady));

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
        let alpha = app
            .rows()
            .into_iter()
            .find(|r| matches!(&r.view, View::Worker(n) if n == "alpha"))
            .unwrap();
        assert_eq!(alpha.worker_badge, Some(WorkerBadge::Idle));

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
}
