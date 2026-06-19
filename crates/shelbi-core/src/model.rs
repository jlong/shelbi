use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Workspace / Session

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub name: String,
    #[serde(default)]
    pub projects: Vec<SessionProject>,
    #[serde(default)]
    pub startup: Vec<serde_yaml::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionProject {
    pub name: String,
    #[serde(default)]
    pub machines: Vec<String>,
}

// ---------------------------------------------------------------------------
// Project

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub repo: String,
    #[serde(default = "default_branch")]
    pub default_branch: String,
    pub machines: Vec<Machine>,
    pub orchestrator: OrchestratorSpec,
    pub agent_runners: std::collections::BTreeMap<String, AgentRunnerSpec>,
    #[serde(default)]
    pub editor: Option<String>,
    /// Optional GitHub repo URL (e.g. `git@github.com:owner/repo.git`)
    /// recorded by the project-setup wizard. Informational for now — the
    /// merge `--pr` flow still resolves the remote via local git config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_url: Option<String>,
    /// Fixed pool of worker agents available to this project. Each owns a
    /// stable worktree on its machine; the orchestrator routes tasks to
    /// workers by name. See [`WorkerSpec`].
    #[serde(default)]
    pub workers: Vec<WorkerSpec>,
    /// How often the orchestrator polls each worker pane for state changes.
    #[serde(default = "default_worker_poll_interval_secs")]
    pub worker_poll_interval_secs: u64,
    /// Permissions posture rendered into the worker settings template
    /// (see [`Project::worker_settings_template`]). The default `auto`
    /// is mapped to claude's `acceptEdits` at render time.
    #[serde(default = "default_worker_permissions_mode")]
    pub worker_permissions_mode: String,
    /// Optional override for the path to the per-project worker settings
    /// template. When `None`, the default at
    /// `~/.shelbi/projects/<name>/worker-settings.json` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_settings_template: Option<PathBuf>,
}

fn default_branch() -> String {
    "main".to_string()
}

fn default_worker_poll_interval_secs() -> u64 {
    5
}

fn default_worker_permissions_mode() -> String {
    "auto".to_string()
}

impl Project {
    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }

    pub fn runner(&self, name: &str) -> Option<&AgentRunnerSpec> {
        self.agent_runners.get(name)
    }

    pub fn worker(&self, name: &str) -> Option<&WorkerSpec> {
        self.workers.iter().find(|w| w.name == name)
    }

    /// Cross-check workers reference declared machines and runners.
    pub fn validate_workers(&self) -> crate::Result<()> {
        for w in &self.workers {
            if self.machine(&w.machine).is_none() {
                return Err(crate::Error::UnknownMachine(w.machine.clone()));
            }
            if self.runner(&w.runner).is_none() {
                return Err(crate::Error::UnknownRunner(w.runner.clone()));
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Worker (declared agent in project YAML)

/// A worker is a long-lived slot on a machine: one stable worktree, one
/// runner. Workers pick up tasks from the board and switch branches between
/// assignments (with cleared context). The worktree path is derived as
/// `<machine.work_dir>/.shelbi/wt/<worker-name>` — not configurable yet.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSpec {
    pub name: String,
    pub machine: String,
    pub runner: String,
}

// ---------------------------------------------------------------------------
// Machine / Host

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Machine {
    pub name: String,
    pub kind: MachineKind,
    pub work_dir: PathBuf,
    /// SSH hostname, required when `kind = ssh`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MachineKind {
    Local,
    Ssh,
}

impl Machine {
    /// Effective host abstraction for shelling out tmux/git/etc.
    pub fn host(&self) -> Host {
        match (&self.kind, &self.host) {
            (MachineKind::Local, _) => Host::Local,
            (MachineKind::Ssh, Some(h)) => Host::Ssh { host: h.clone() },
            (MachineKind::Ssh, None) => Host::Ssh {
                host: self.name.clone(),
            },
        }
    }
}

/// Where a command runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Host {
    Local,
    Ssh { host: String },
}

impl Host {
    pub fn is_local(&self) -> bool {
        matches!(self, Host::Local)
    }
}

// ---------------------------------------------------------------------------
// Agent runner / orchestrator runner

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRunnerSpec {
    /// Executable to invoke (e.g. "claude", "codex").
    pub command: String,
    /// Extra flags to append to every invocation.
    #[serde(default)]
    pub flags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrchestratorSpec {
    /// Name of an agent runner declared in `agent_runners`.
    pub runner: String,
}

// ---------------------------------------------------------------------------
// Worker / Agent state

/// Persistent state for a single worker agent.
///
/// Serialized as YAML frontmatter on disk. The markdown body lives separately.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    pub project: String,
    pub machine: String,
    pub runner: String,
    pub branch: String,
    pub worktree: PathBuf,
    pub status: Status,
    pub created: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    pub tmux: TmuxAddr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Queued,
    Running,
    Waiting,
    Done,
    Error,
    Archived,
}

impl Status {
    pub fn glyph(self) -> &'static str {
        match self {
            Status::Queued => "○",
            Status::Running => "●",
            Status::Waiting => "◐",
            Status::Done => "✓",
            Status::Error => "✗",
            Status::Archived => "·",
        }
    }
}

/// A tmux address — `session:window` (we keep pane implicit; one pane per worker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TmuxAddr {
    pub session: String,
    pub window: String,
}

impl TmuxAddr {
    pub fn target(&self) -> String {
        format!("{}:{}", self.session, self.window)
    }
}

// ---------------------------------------------------------------------------
// Tasks (Kanban board)

/// Where on the board a task lives.
///
/// Lifecycle:
///
/// - `Backlog`: orchestrator-created, awaiting user triage.
/// - `Todo`: user-curated, ready for a worker to pick up.
/// - `InProgress`: assigned and active on a worker.
/// - `Review`: worker reports done; user inspects via the review dir.
/// - `Done`: accepted by user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Column {
    Backlog,
    Todo,
    InProgress,
    Review,
    Done,
}

impl Column {
    pub const ALL: [Column; 5] = [
        Column::Backlog,
        Column::Todo,
        Column::InProgress,
        Column::Review,
        Column::Done,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Column::Backlog => "backlog",
            Column::Todo => "todo",
            Column::InProgress => "in_progress",
            Column::Review => "review",
            Column::Done => "done",
        }
    }
}

impl std::fmt::Display for Column {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Column {
    type Err = crate::Error;
    fn from_str(s: &str) -> crate::Result<Self> {
        // Accept both the canonical form and a few friendly aliases users
        // are likely to type at the CLI ("in-progress", "wip", "todo").
        match s.trim().to_ascii_lowercase().as_str() {
            "backlog" => Ok(Column::Backlog),
            "todo" | "to_do" | "to-do" => Ok(Column::Todo),
            "in_progress" | "in-progress" | "wip" => Ok(Column::InProgress),
            "review" | "ready_for_review" | "ready-for-review" => Ok(Column::Review),
            "done" | "complete" | "completed" => Ok(Column::Done),
            other => Err(crate::Error::Other(format!("unknown column: {other}"))),
        }
    }
}

/// One Kanban card. Position within a column is given by `priority`
/// (0 = top); the storage layer keeps these contiguous within each column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub column: Column,
    pub priority: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assigned_to: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// Other task ids this task depends on. A task is **blocked** (see
    /// [`Task::is_blocked`]) when any of these are not yet in
    /// [`Column::Done`]. Stored as a list rather than a reverse `blocks`
    /// field so cycle detection and dep editing only touch one file.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Task {
    /// True iff any id in `depends_on` is not yet `Done` in `columns`.
    /// IDs missing from `columns` are treated as not-done (which matches
    /// the project-wide validation that rejects unknown ids at save time
    /// — if a dep id is unknown here, the safer answer is still blocked).
    pub fn is_blocked(&self, columns: &std::collections::HashMap<String, Column>) -> bool {
        self.depends_on
            .iter()
            .any(|id| columns.get(id).copied() != Some(Column::Done))
    }
}

/// Same character set as agent ids (kebab/snake alphanumeric). Aliased so
/// call sites read clearly at the task layer.
pub fn validate_task_id(s: &str) -> crate::Result<()> {
    validate_agent_id(s)
}

// ---------------------------------------------------------------------------
// Agent id validation

/// Validate an agent id: kebab-case alphanumerics, hyphen-separated.
pub fn validate_agent_id(s: &str) -> crate::Result<()> {
    if s.is_empty() {
        return Err(crate::Error::InvalidAgentId(s.to_string()));
    }
    let ok = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    let starts_ok = s
        .chars()
        .next()
        .map(|c| c.is_ascii_alphanumeric())
        .unwrap_or(false);
    if !ok || !starts_ok {
        return Err(crate::Error::InvalidAgentId(s.to_string()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_id_validation() {
        assert!(validate_agent_id("fix-login-bug").is_ok());
        assert!(validate_agent_id("fix_login_bug").is_ok());
        assert!(validate_agent_id("abc123").is_ok());
        assert!(validate_agent_id("").is_err());
        assert!(validate_agent_id("-leading-hyphen").is_err());
        assert!(validate_agent_id("has spaces").is_err());
        assert!(validate_agent_id("slash/in/id").is_err());
    }

    #[test]
    fn status_glyphs_unique() {
        let glyphs = [
            Status::Queued.glyph(),
            Status::Running.glyph(),
            Status::Waiting.glyph(),
            Status::Done.glyph(),
            Status::Error.glyph(),
            Status::Archived.glyph(),
        ];
        let unique: std::collections::HashSet<_> = glyphs.iter().collect();
        assert_eq!(unique.len(), glyphs.len());
    }

    #[test]
    fn tmux_target_format() {
        let addr = TmuxAddr {
            session: "shelbi-daily".to_string(),
            window: "w-fix-login".to_string(),
        };
        assert_eq!(addr.target(), "shelbi-daily:w-fix-login");
    }

    #[test]
    fn column_serde_roundtrip() {
        for c in Column::ALL {
            let y = serde_yaml::to_string(&c).unwrap();
            let back: Column = serde_yaml::from_str(&y).unwrap();
            assert_eq!(c, back);
        }
        // Wire format is the snake_case form.
        assert_eq!(serde_yaml::to_string(&Column::InProgress).unwrap().trim(), "in_progress");
    }

    #[test]
    fn column_from_str_friendly_aliases() {
        use std::str::FromStr;
        assert_eq!(Column::from_str("backlog").unwrap(), Column::Backlog);
        assert_eq!(Column::from_str("to-do").unwrap(), Column::Todo);
        assert_eq!(Column::from_str("WIP").unwrap(), Column::InProgress);
        assert_eq!(Column::from_str("in-progress").unwrap(), Column::InProgress);
        assert_eq!(Column::from_str("ready-for-review").unwrap(), Column::Review);
        assert_eq!(Column::from_str("complete").unwrap(), Column::Done);
        assert!(Column::from_str("garbage").is_err());
    }

    #[test]
    fn task_depends_on_defaults_to_empty_and_omits_in_serialization() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.depends_on.is_empty());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("depends_on"));
    }

    #[test]
    fn task_is_blocked_when_any_dep_not_done() {
        let now = chrono::Utc::now();
        let task = Task {
            id: "a".into(),
            title: "A".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            branch: None,
            depends_on: vec!["b".into(), "c".into()],
            created_at: now,
            updated_at: now,
        };
        let mut cols = std::collections::HashMap::new();
        cols.insert("b".to_string(), Column::Done);
        cols.insert("c".to_string(), Column::InProgress);
        assert!(task.is_blocked(&cols));

        cols.insert("c".to_string(), Column::Done);
        assert!(!task.is_blocked(&cols));

        // Unknown dep id is treated as not-done.
        cols.remove("c");
        assert!(task.is_blocked(&cols));
    }

    #[test]
    fn task_id_uses_same_rules_as_agent_id() {
        assert!(validate_task_id("fix-login").is_ok());
        assert!(validate_task_id("with spaces").is_err());
    }

    #[test]
    fn project_yaml_omits_new_worker_keys_and_uses_defaults() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.worker_poll_interval_secs, 5);
        assert_eq!(p.worker_permissions_mode, "auto");
        assert!(p.worker_settings_template.is_none());
    }

    #[test]
    fn project_yaml_round_trips_explicit_worker_keys() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
worker_poll_interval_secs: 12
worker_permissions_mode: acceptEdits
worker_settings_template: /etc/shelbi/p.json
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.worker_poll_interval_secs, 12);
        assert_eq!(p.worker_permissions_mode, "acceptEdits");
        assert_eq!(
            p.worker_settings_template.as_deref(),
            Some(std::path::Path::new("/etc/shelbi/p.json"))
        );
    }

    #[test]
    fn workers_validate_against_machines_and_runners() {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert("claude".to_string(), AgentRunnerSpec { command: "claude".into(), flags: vec![] });
        let project = Project {
            name: "p".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp".into(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: vec![
                WorkerSpec { name: "alice".into(), machine: "hub".into(), runner: "claude".into() },
            ],
            worker_poll_interval_secs: default_worker_poll_interval_secs(),
            worker_permissions_mode: default_worker_permissions_mode(),
            worker_settings_template: None,
        };
        assert!(project.validate_workers().is_ok());

        let mut bad = project.clone();
        bad.workers.push(WorkerSpec { name: "bob".into(), machine: "ghost".into(), runner: "claude".into() });
        assert!(matches!(bad.validate_workers(), Err(crate::Error::UnknownMachine(_))));

        let mut bad2 = project.clone();
        bad2.workers.push(WorkerSpec { name: "bob".into(), machine: "hub".into(), runner: "ghost".into() });
        assert!(matches!(bad2.validate_workers(), Err(crate::Error::UnknownRunner(_))));
    }
}
