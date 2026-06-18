use std::time::{Duration, Instant};

use anyhow::Result;
use shelbi_core::{Agent, Session, Status};

/// What the right pane is showing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Chat,
    Tasks,
    Review,
    Machines,
    /// A specific worker agent picked from the sidebar list.
    Agent(String),
}

impl View {
    pub fn title(&self) -> String {
        match self {
            View::Chat => "Chat — orchestrator".into(),
            View::Tasks => "Tasks".into(),
            View::Review => "Review".into(),
            View::Machines => "Machines".into(),
            View::Agent(id) => format!("agent: {id}"),
        }
    }
}

/// One row in the left-side nav.
#[derive(Debug, Clone)]
pub struct NavRow {
    pub label: String,
    pub icon: &'static str,
    pub view: View,
    /// Optional badge (e.g. count for Review).
    pub badge: Option<String>,
}

pub struct App {
    pub session_name: String,
    pub session: Option<Session>,
    pub project_name: Option<String>,

    pub agents: Vec<Agent>,
    pub view: View,
    pub sidebar_index: usize,
    pub last_refresh: Instant,
    pub status_line: String,
    pub should_quit: bool,
}

impl App {
    pub fn new(session_name: impl Into<String>) -> Self {
        Self {
            session_name: session_name.into(),
            session: None,
            project_name: None,
            agents: Vec::new(),
            view: View::Chat,
            sidebar_index: 0,
            last_refresh: Instant::now() - Duration::from_secs(60),
            status_line: String::new(),
            should_quit: false,
        }
    }

    /// Rebuild the nav rows from current session state.
    pub fn nav(&self) -> Vec<NavRow> {
        let review_count = self
            .agents
            .iter()
            .filter(|a| matches!(a.status, Status::Done | Status::Waiting))
            .count();
        let mut rows = vec![
            NavRow {
                label: "Chat".into(),
                icon: "💬",
                view: View::Chat,
                badge: None,
            },
            NavRow {
                label: "Tasks".into(),
                icon: "📋",
                view: View::Tasks,
                badge: None,
            },
            NavRow {
                label: "Review".into(),
                icon: "🔍",
                view: View::Review,
                badge: (review_count > 0).then(|| review_count.to_string()),
            },
            NavRow {
                label: "Machines".into(),
                icon: "🖥 ",
                view: View::Machines,
                badge: None,
            },
        ];
        for a in &self.agents {
            rows.push(NavRow {
                label: format!("{} {}", a.status.glyph(), a.id),
                icon: " ",
                view: View::Agent(a.id.clone()),
                badge: Some(a.machine.clone()),
            });
        }
        rows
    }

    pub fn refresh(&mut self) -> Result<()> {
        // Load session (cached on first success).
        if self.session.is_none() {
            self.session = shelbi_state::load_session(&self.session_name).ok();
        }
        // Pick a project for "current": first project in the session, else
        // first thing under ~/.shelbi/projects.
        if self.project_name.is_none() {
            if let Some(s) = &self.session {
                if let Some(sp) = s.projects.first() {
                    self.project_name = Some(sp.name.clone());
                }
            }
        }
        // Reload agents from disk.
        if let Some(p) = &self.project_name {
            self.agents = load_agents(p).unwrap_or_default();
        }
        self.last_refresh = Instant::now();
        Ok(())
    }

    pub fn maybe_refresh(&mut self) -> Result<()> {
        if self.last_refresh.elapsed() >= Duration::from_millis(500) {
            self.refresh()?;
        }
        Ok(())
    }

    /// Move the sidebar selection up (wraps).
    pub fn nav_up(&mut self) {
        let n = self.nav().len();
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
        let n = self.nav().len();
        if n == 0 {
            return;
        }
        self.sidebar_index = (self.sidebar_index + 1) % n;
    }

    /// Activate the highlighted sidebar row.
    pub fn nav_activate(&mut self) {
        if let Some(row) = self.nav().get(self.sidebar_index).cloned() {
            self.view = row.view;
        }
    }

    /// Jump directly to nav index N (1-based from user, 0-based here).
    pub fn nav_jump(&mut self, n: usize) {
        let nav = self.nav();
        if n < nav.len() {
            self.sidebar_index = n;
            self.view = nav[n].view.clone();
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nav_jump_changes_view() {
        let mut a = App::new("default");
        a.nav_jump(1);
        assert_eq!(a.view, View::Tasks);
        a.nav_jump(3);
        assert_eq!(a.view, View::Machines);
    }

    #[test]
    fn nav_wraps() {
        let mut a = App::new("default");
        let n = a.nav().len();
        for _ in 0..n + 2 {
            a.nav_down();
        }
        assert!(a.sidebar_index < n);
    }
}
