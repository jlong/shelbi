use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::layout::Rect;
use shelbi_core::{Agent, Column, Status};
use shelbi_state::TaskFile;

/// What's currently highlighted in the sidebar — drives selection logic
/// only; the right pane (orchestrator / agent) is a real tmux pane, not
/// rendered by this process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    /// One of the built-in views hosted as a hidden tmux pane: swap it
    /// into the dashboard's right slot.
    Builtin(&'static str), // "orch" | "tasks" | "review" | "machines"
    /// A specific worker agent — switch tmux to its window.
    Agent(String),
    /// A task in the review queue — trigger the review checkout flow and
    /// focus the review pane.
    ReviewTask(String),
}

pub struct App {
    pub project_name: String,
    pub agents: Vec<Agent>,
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
        Self {
            project_name: project_name.into(),
            agents: Vec::new(),
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

    /// Sidebar rows: built-in views, review queue (one per task awaiting
    /// review), then any spawned worker agents (legacy / shelbi spawn).
    pub fn rows(&self) -> Vec<Row> {
        let mut rows = vec![
            Row {
                label: "orchestrator".into(),
                view: View::Builtin("orch"),
                badge: None,
                status: None,
            },
            Row {
                label: "tasks".into(),
                view: View::Builtin("tasks"),
                badge: None,
                status: None,
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
            },
            Row {
                label: "machines".into(),
                view: View::Builtin("machines"),
                badge: None,
                status: None,
            },
        ];
        for tf in &self.review_queue {
            rows.push(Row {
                label: tf.task.title.clone(),
                view: View::ReviewTask(tf.task.id.clone()),
                badge: tf.task.assigned_to.clone(),
                status: None,
            });
        }
        for a in &self.agents {
            rows.push(Row {
                label: a.id.clone(),
                view: View::Agent(a.id.clone()),
                badge: Some(a.machine.clone()),
                status: Some(a.status),
            });
        }
        rows
    }

    pub fn refresh(&mut self) -> Result<()> {
        self.agents = load_agents(&self.project_name).unwrap_or_default();
        self.review_queue =
            shelbi_state::list_column(&self.project_name, Column::Review).unwrap_or_default();
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
    pub status: Option<Status>,
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
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
