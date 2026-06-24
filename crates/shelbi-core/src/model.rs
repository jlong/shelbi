use std::path::PathBuf;
use std::time::Duration;

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
    /// `~/.shelbi/projects/<name>/worker-settings.json.template` is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker_settings_template: Option<PathBuf>,
    /// Zen Mode configuration: which checks to run, how long to wait on
    /// CI, and which paths require human review even in Zen Mode.
    #[serde(default)]
    pub zen: ZenConfig,
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

    pub fn is_ssh(&self) -> bool {
        matches!(self, Host::Ssh { .. })
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
    /// Optional hint to the orchestrator: prefer assigning this task to a
    /// worker on the named machine. Persisted only; enforcement (or
    /// override) is the orchestrator's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefers_machine: Option<String>,
    /// Per-task overrides for Zen Mode: opt-in/out of auto-promote and
    /// adjust the check set against the project default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zen: Option<TaskZenConfig>,
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
// Zen Mode

/// Project-level Zen Mode configuration. Stored under the `zen:` key in
/// the project YAML. Defaults are tuned for a small repo with a sane CI
/// pipeline — `checks.local` empty, 15-minute CI timeout, no extra
/// danger paths beyond the built-in list.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZenConfig {
    /// Local checks run before promotion (e.g. `cargo test`, `npm test`).
    #[serde(default)]
    pub checks: ZenChecks,
    /// How long Zen Mode will wait for CI to report a verdict before
    /// timing out the promotion. Serialized as a number of seconds.
    #[serde(default = "default_ci_timeout", with = "duration_secs")]
    pub ci_timeout: Duration,
    /// Glob patterns considered too sensitive to auto-promote. By default
    /// this *extends* the built-in list (see [`Project::builtin_danger_paths`]);
    /// projects can opt into a full override via the `override` variant.
    #[serde(default)]
    pub danger_paths: ZenDangerPaths,
}

impl Default for ZenConfig {
    fn default() -> Self {
        Self {
            checks: ZenChecks::default(),
            ci_timeout: default_ci_timeout(),
            danger_paths: ZenDangerPaths::default(),
        }
    }
}

/// Default CI wait: 15 minutes.
fn default_ci_timeout() -> Duration {
    Duration::from_secs(15 * 60)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ZenChecks {
    /// Shell commands run locally before the worker hands off to CI.
    /// Each entry is a single command line, executed in the worktree
    /// root.
    #[serde(default)]
    pub local: Vec<String>,
}

/// How the project's `danger_paths` list interacts with the built-in
/// list. `Extend` (default) keeps the built-in patterns and adds the
/// user's; `Override` replaces them entirely. The wire format is a map
/// with a single `extend:` or `override:` key — we hand-roll it via
/// [`ZenDangerPathsRepr`] because serde_yaml's externally-tagged
/// default uses YAML tags (`!extend`), which are awkward to write in a
/// hand-edited project file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "ZenDangerPathsRepr", into = "ZenDangerPathsRepr")]
pub enum ZenDangerPaths {
    Extend(Vec<String>),
    Override(Vec<String>),
}

impl Default for ZenDangerPaths {
    fn default() -> Self {
        ZenDangerPaths::Extend(Vec::new())
    }
}

/// Wire form for [`ZenDangerPaths`]: exactly one of `extend` / `override`
/// is set. Both being set, or neither, is a deserialization error.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ZenDangerPathsRepr {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    extend: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "override")]
    override_: Option<Vec<String>>,
}

impl TryFrom<ZenDangerPathsRepr> for ZenDangerPaths {
    type Error = &'static str;
    fn try_from(r: ZenDangerPathsRepr) -> Result<Self, Self::Error> {
        match (r.extend, r.override_) {
            (Some(_), Some(_)) => {
                Err("zen.danger_paths: set either `extend:` or `override:`, not both")
            }
            (Some(v), None) => Ok(ZenDangerPaths::Extend(v)),
            (None, Some(v)) => Ok(ZenDangerPaths::Override(v)),
            (None, None) => Ok(ZenDangerPaths::default()),
        }
    }
}

impl From<ZenDangerPaths> for ZenDangerPathsRepr {
    fn from(p: ZenDangerPaths) -> Self {
        match p {
            ZenDangerPaths::Extend(v) => Self { extend: Some(v), override_: None },
            ZenDangerPaths::Override(v) => Self { extend: None, override_: Some(v) },
        }
    }
}

/// Built-in danger paths: always part of the resolved list when the
/// project uses the `extend` variant (the default). Patterns are glob
/// strings; matching is the caller's job.
pub const BUILTIN_DANGER_PATHS: &[&str] = &[
    ".github/workflows/**",
    "scripts/install.sh",
    "*.yaml",
    "*.yml",
    "LICENSE",
    "package-lock.json",
    "Cargo.lock",
];

/// Per-task Zen Mode overrides. Lives under `zen:` in the task
/// frontmatter. Every field is optional so a task can adjust just one
/// dimension.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskZenConfig {
    /// Explicit opt-in/out of Zen Mode promotion for this task. `None`
    /// means "follow project default"; `Some(false)` is the canonical
    /// way to keep a sensitive task on the manual-review path even when
    /// the project is otherwise in Zen Mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Checks to run *in addition to* the project's `zen.checks.local`.
    /// Ignored when `checks_only` is set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks_additional: Vec<String>,
    /// Checks to run *instead of* the project's `zen.checks.local`.
    /// Takes precedence over `checks_additional`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub checks_only: Vec<String>,
}

/// Resolve the effective check list for a task: `checks_only` replaces
/// the project list, `checks_additional` extends it, and absent both the
/// project's `zen.checks.local` is returned verbatim.
pub fn checks_for_task(project: &Project, task: &Task) -> Vec<String> {
    let project_local = &project.zen.checks.local;
    match task.zen.as_ref() {
        Some(z) if !z.checks_only.is_empty() => z.checks_only.clone(),
        Some(z) if !z.checks_additional.is_empty() => {
            let mut out = project_local.clone();
            out.extend(z.checks_additional.iter().cloned());
            out
        }
        _ => project_local.clone(),
    }
}

/// Resolve the effective danger-path list for a project. `Extend`
/// returns the built-in list with project additions appended; `Override`
/// returns the project list verbatim. Duplicates are preserved in
/// order — callers that care can dedupe.
pub fn danger_paths_for_project(project: &Project) -> Vec<String> {
    match &project.zen.danger_paths {
        ZenDangerPaths::Extend(extra) => {
            let mut out: Vec<String> =
                BUILTIN_DANGER_PATHS.iter().map(|s| s.to_string()).collect();
            out.extend(extra.iter().cloned());
            out
        }
        ZenDangerPaths::Override(custom) => custom.clone(),
    }
}

/// Serde adapter that stores a `Duration` as an integer number of
/// seconds in YAML/JSON. Keeps the project YAML readable while letting
/// the in-memory type stay a `Duration`.
mod duration_secs {
    use std::time::Duration;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u64(d.as_secs())
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let secs = u64::deserialize(d)?;
        Ok(Duration::from_secs(secs))
    }
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
            prefers_machine: None,
            zen: None,
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
    fn task_prefers_machine_round_trips() {
        let now = chrono::Utc::now();
        let task = Task {
            id: "linux-probe".into(),
            title: "Tune the readiness probe".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            branch: None,
            depends_on: vec![],
            prefers_machine: Some("devbox".into()),
            zen: None,
            created_at: now,
            updated_at: now,
        };
        let y = serde_yaml::to_string(&task).unwrap();
        assert!(y.contains("prefers_machine: devbox"));
        let back: Task = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.prefers_machine.as_deref(), Some("devbox"));
        // YAML representation is stable across a second round trip.
        let y2 = serde_yaml::to_string(&back).unwrap();
        assert_eq!(y, y2);
    }

    #[test]
    fn task_prefers_machine_defaults_to_none_and_omits_in_serialization() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.prefers_machine.is_none());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("prefers_machine"));
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
            zen: ZenConfig::default(),
        };
        assert!(project.validate_workers().is_ok());

        let mut bad = project.clone();
        bad.workers.push(WorkerSpec { name: "bob".into(), machine: "ghost".into(), runner: "claude".into() });
        assert!(matches!(bad.validate_workers(), Err(crate::Error::UnknownMachine(_))));

        let mut bad2 = project.clone();
        bad2.workers.push(WorkerSpec { name: "bob".into(), machine: "hub".into(), runner: "ghost".into() });
        assert!(matches!(bad2.validate_workers(), Err(crate::Error::UnknownRunner(_))));
    }

    // ---- Zen Mode ----------------------------------------------------------

    fn project_with_zen(zen: ZenConfig) -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        Project {
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
            workers: vec![],
            worker_poll_interval_secs: default_worker_poll_interval_secs(),
            worker_permissions_mode: default_worker_permissions_mode(),
            worker_settings_template: None,
            zen,
        }
    }

    fn task_with_zen(zen: Option<TaskZenConfig>) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "t".into(),
            title: "T".into(),
            column: Column::Todo,
            priority: 0,
            assigned_to: None,
            branch: None,
            depends_on: vec![],
            prefers_machine: None,
            zen,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn zen_config_defaults_match_spec() {
        let z = ZenConfig::default();
        assert!(z.checks.local.is_empty());
        assert_eq!(z.ci_timeout, Duration::from_secs(15 * 60));
        assert_eq!(z.danger_paths, ZenDangerPaths::Extend(vec![]));
    }

    #[test]
    fn project_yaml_omits_zen_and_uses_defaults() {
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
        assert_eq!(p.zen, ZenConfig::default());
    }

    #[test]
    fn project_yaml_parses_zen_block() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
zen:
  checks:
    local:
      - cargo test
      - cargo clippy
  ci_timeout: 600
  danger_paths:
    extend:
      - migrations/**
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(p.zen.checks.local, vec!["cargo test", "cargo clippy"]);
        assert_eq!(p.zen.ci_timeout, Duration::from_secs(600));
        assert_eq!(
            p.zen.danger_paths,
            ZenDangerPaths::Extend(vec!["migrations/**".into()])
        );
    }

    #[test]
    fn project_yaml_parses_override_danger_paths() {
        let yaml = r#"
name: p
repo: r
machines:
  - { name: hub, kind: local, work_dir: /tmp }
orchestrator: { runner: claude }
agent_runners:
  claude: { command: claude, flags: [] }
zen:
  danger_paths:
    override:
      - "**/*"
"#;
        let p: Project = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(
            p.zen.danger_paths,
            ZenDangerPaths::Override(vec!["**/*".into()])
        );
    }

    #[test]
    fn zen_config_yaml_round_trip() {
        let cfg = ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ci_timeout: Duration::from_secs(900),
            danger_paths: ZenDangerPaths::Extend(vec!["docs/**".into()]),
        };
        let y = serde_yaml::to_string(&cfg).unwrap();
        let back: ZenConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);
        // ci_timeout serializes as a plain integer (no struct form).
        assert!(y.contains("ci_timeout: 900"));
    }

    #[test]
    fn task_frontmatter_parses_zen_overrides() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
zen:
  enabled: false
  checks_only:
    - cargo test --doc
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        let z = t.zen.expect("zen block present");
        assert_eq!(z.enabled, Some(false));
        assert_eq!(z.checks_only, vec!["cargo test --doc"]);
        assert!(z.checks_additional.is_empty());
    }

    #[test]
    fn task_zen_round_trips_and_defaults_to_none() {
        let yaml = r#"
id: a
title: A
column: todo
priority: 0
created_at: 2026-06-19T00:00:00Z
updated_at: 2026-06-19T00:00:00Z
"#;
        let t: Task = serde_yaml::from_str(yaml).unwrap();
        assert!(t.zen.is_none());
        let back = serde_yaml::to_string(&t).unwrap();
        assert!(!back.contains("zen"));
    }

    #[test]
    fn task_zen_config_round_trip() {
        let cfg = TaskZenConfig {
            enabled: Some(true),
            checks_additional: vec!["npm test".into()],
            checks_only: vec![],
        };
        let y = serde_yaml::to_string(&cfg).unwrap();
        let back: TaskZenConfig = serde_yaml::from_str(&y).unwrap();
        assert_eq!(cfg, back);
        // Empty lists omitted on the wire.
        assert!(!y.contains("checks_only"));
    }

    #[test]
    fn checks_for_task_falls_back_to_project_when_no_overrides() {
        let p = project_with_zen(ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ..Default::default()
        });
        let t = task_with_zen(None);
        assert_eq!(checks_for_task(&p, &t), vec!["cargo test"]);
    }

    #[test]
    fn checks_for_task_extends_with_additional() {
        let p = project_with_zen(ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ..Default::default()
        });
        let t = task_with_zen(Some(TaskZenConfig {
            checks_additional: vec!["cargo clippy".into()],
            ..Default::default()
        }));
        assert_eq!(checks_for_task(&p, &t), vec!["cargo test", "cargo clippy"]);
    }

    #[test]
    fn checks_for_task_only_replaces_project_checks() {
        let p = project_with_zen(ZenConfig {
            checks: ZenChecks { local: vec!["cargo test".into()] },
            ..Default::default()
        });
        let t = task_with_zen(Some(TaskZenConfig {
            checks_only: vec!["cargo test --doc".into()],
            // `checks_only` wins even when both are set.
            checks_additional: vec!["never-run".into()],
            ..Default::default()
        }));
        assert_eq!(checks_for_task(&p, &t), vec!["cargo test --doc"]);
    }

    #[test]
    fn danger_paths_extend_appends_to_builtins() {
        let p = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Extend(vec!["secrets/**".into()]),
            ..Default::default()
        });
        let paths = danger_paths_for_project(&p);
        for builtin in BUILTIN_DANGER_PATHS {
            assert!(paths.iter().any(|p| p == builtin), "missing builtin {builtin}");
        }
        assert!(paths.iter().any(|p| p == "secrets/**"));
    }

    #[test]
    fn danger_paths_override_drops_builtins() {
        let p = project_with_zen(ZenConfig {
            danger_paths: ZenDangerPaths::Override(vec!["only/this".into()]),
            ..Default::default()
        });
        let paths = danger_paths_for_project(&p);
        assert_eq!(paths, vec!["only/this".to_string()]);
    }

    #[test]
    fn danger_paths_default_returns_just_builtins() {
        let p = project_with_zen(ZenConfig::default());
        let paths = danger_paths_for_project(&p);
        assert_eq!(paths.len(), BUILTIN_DANGER_PATHS.len());
        for (got, want) in paths.iter().zip(BUILTIN_DANGER_PATHS.iter()) {
            assert_eq!(got, want);
        }
    }
}
