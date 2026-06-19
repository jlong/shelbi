use std::time::{Duration, Instant};

use anyhow::Result;
use shelbi_core::{Agent, Status};

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
}

pub struct App {
    pub project_name: String,
    pub agents: Vec<Agent>,
    pub sidebar_index: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    pub should_quit: bool,
}

impl App {
    pub fn new_sidebar(project_name: impl Into<String>) -> Self {
        Self {
            project_name: project_name.into(),
            agents: Vec::new(),
            sidebar_index: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            should_quit: false,
        }
    }

    /// Sidebar rows: built-in views first, then a separator, then each
    /// active worker.
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
                badge: None,
                status: None,
            },
            Row {
                label: "machines".into(),
                view: View::Builtin("machines"),
                badge: None,
                status: None,
            },
        ];
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
        }
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
