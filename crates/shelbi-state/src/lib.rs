//! State IO: load/save projects, sessions, and per-agent markdown files.
//!
//! Agent files use YAML frontmatter (`---` fenced) with a free-form markdown
//! body. We don't depend on `gray_matter` to keep the dep tree small;
//! splitting the file at the second `---` is good enough for our format.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use shelbi_core::{Agent, Column, Project, Result, Session, Task};

mod hub_config;
mod worker_status;

pub use hub_config::{
    hub_config_path, list_projects, load_hub_config, save_hub_config, touch_project_launched,
    HubConfig, ProjectMeta, ProjectSummary,
};
pub use worker_status::{
    append_worker_event, events_log_path, load_worker_status, parse_pane_title_marker,
    parse_pane_title_state, save_worker_status, worker_status_path, workers_dir, PaneMarker,
    WorkerState, WorkerStatus,
};

/// Default assistant name surfaced in the sidebar header and the
/// orchestrator system prompt when the user hasn't picked one yet.
pub const DEFAULT_ASSISTANT_NAME: &str = "Orchestrator";

/// Global shelbi config, persisted at `~/.shelbi/shelbi.yaml`. Created and
/// populated by the onboarding wizard; everything is `Option` so that
/// loading an older or partial file still succeeds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShelbiConfig {
    /// What the user wants to call their assistant — shown above the
    /// orchestrator pane and substituted into the orchestrator system
    /// prompt. `None` means the user hasn't been through Phase 1 of the
    /// wizard yet; callers should fall back to [`DEFAULT_ASSISTANT_NAME`]
    /// via [`ShelbiConfig::assistant_name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_name: Option<String>,
}

impl ShelbiConfig {
    /// The configured assistant name, or [`DEFAULT_ASSISTANT_NAME`] if
    /// the wizard hasn't set one yet.
    pub fn assistant_name(&self) -> &str {
        self.assistant_name
            .as_deref()
            .unwrap_or(DEFAULT_ASSISTANT_NAME)
    }
}

/// Path to the global config file: `$SHELBI_HOME/shelbi.yaml`.
pub fn shelbi_config_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("shelbi.yaml"))
}

/// Load the global config. Missing file → default (empty) config; that's
/// not an error because every consumer has a sensible fallback.
pub fn load_shelbi_config() -> Result<ShelbiConfig> {
    let path = shelbi_config_path()?;
    if !path.exists() {
        return Ok(ShelbiConfig::default());
    }
    let text = fs::read_to_string(&path)?;
    Ok(serde_yaml::from_str(&text)?)
}

/// Atomically write the global config.
pub fn save_shelbi_config(cfg: &ShelbiConfig) -> Result<()> {
    ensure_dir(&shelbi_home()?)?;
    let path = shelbi_config_path()?;
    atomic_write(&path, serde_yaml::to_string(cfg)?.as_bytes())
}

#[cfg(test)]
pub(crate) mod test_lock {
    //! Shared mutex for all tests that mutate the process-wide
    //! `SHELBI_HOME` env var. Per-module locks would race because the
    //! env var is global.
    use std::sync::Mutex;
    pub static LOCK: Mutex<()> = Mutex::new(());
}

/// Default contents of the per-project worker settings template. Lives at
/// `~/.shelbi/projects/<name>/worker-settings.json.template` after
/// `shelbi init --project <name>` runs. The `.template` suffix flags the
/// file as needing placeholder substitution before use — the
/// `{{worker_permissions_mode}}` placeholder is filled in later by the
/// worker deploy step from `Project::worker_permissions_mode`.
pub const DEFAULT_WORKER_SETTINGS_TEMPLATE: &str =
    include_str!("default_worker_settings.json.template");

/// Default shelbi home directory: `~/.shelbi`, overridable via
/// `$SHELBI_HOME` (useful for tests and sandboxed CI).
pub fn shelbi_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SHELBI_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".shelbi"))
        .ok_or_else(|| shelbi_core::Error::Other("no home directory".into()))
}

pub fn projects_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("projects"))
}

pub fn sessions_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("sessions"))
}

pub fn project_dir(project: &str) -> Result<PathBuf> {
    Ok(projects_dir()?.join(project))
}

/// Resolve the worker settings template path for a project: the override
/// in [`Project::worker_settings_template`] (with `~` expansion) if set,
/// otherwise the default at
/// `~/.shelbi/projects/<name>/worker-settings.json.template`.
///
/// As a one-shot migration, if the legacy `worker-settings.json` (no
/// `.template` suffix) exists in the project dir and the new path doesn't,
/// the legacy file is renamed in place — see [`migrate_worker_settings_template`].
pub fn worker_settings_template_path(project: &Project) -> Result<PathBuf> {
    if let Some(p) = &project.worker_settings_template {
        return Ok(expand_tilde(p));
    }
    let dir = project_dir(&project.name)?;
    migrate_worker_settings_template(&dir);
    Ok(dir.join("worker-settings.json.template"))
}

/// One-shot rename of a legacy `worker-settings.json` to the new
/// `.json.template` name. Idempotent: skips when the new file already exists
/// or the legacy file is missing. Best-effort — any IO error is swallowed
/// so a permissions hiccup doesn't break worker deploy; the caller will
/// fall back to [`DEFAULT_WORKER_SETTINGS_TEMPLATE`] just like any other
/// missing-template case.
fn migrate_worker_settings_template(project_dir: &Path) {
    let legacy = project_dir.join("worker-settings.json");
    let renamed = project_dir.join("worker-settings.json.template");
    if renamed.exists() || !legacy.exists() {
        return;
    }
    let _ = fs::rename(&legacy, &renamed);
}

/// Render the worker settings JSON for `project`: read the template file
/// resolved by [`worker_settings_template_path`] (falling back to
/// [`DEFAULT_WORKER_SETTINGS_TEMPLATE`] when the file is missing — a fresh
/// project that hasn't run `shelbi init --project` yet) and substitute
/// `{{worker_permissions_mode}}` with `project.worker_permissions_mode`.
/// The model documents `auto` as a shelbi-level alias for claude's
/// `acceptEdits`, mapped here at render time.
pub fn render_worker_settings(project: &Project) -> Result<String> {
    let path = worker_settings_template_path(project)?;
    let template = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            DEFAULT_WORKER_SETTINGS_TEMPLATE.to_string()
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let mode = match project.worker_permissions_mode.as_str() {
        "auto" => "acceptEdits",
        other => other,
    };
    Ok(template.replace("{{worker_permissions_mode}}", mode))
}

fn expand_tilde(p: &Path) -> PathBuf {
    if let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("~/")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
}

pub fn agents_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("agents"))
}

pub fn tasks_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("tasks"))
}

/// Ensure a directory exists.
pub fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Project / Session YAML

pub fn load_project(project: &str) -> Result<Project> {
    let p = projects_dir()?.join(format!("{project}.yaml"));
    let text = fs::read_to_string(&p)?;
    let p: Project = serde_yaml::from_str(&text)?;
    p.validate_workers()?;
    Ok(p)
}

pub fn save_project(p: &Project) -> Result<()> {
    ensure_dir(&projects_dir()?)?;
    let path = projects_dir()?.join(format!("{}.yaml", p.name));
    atomic_write(&path, serde_yaml::to_string(p)?.as_bytes())
}

pub fn load_session(name: &str) -> Result<Session> {
    let p = sessions_dir()?.join(format!("{name}.yaml"));
    let text = fs::read_to_string(&p)?;
    Ok(serde_yaml::from_str(&text)?)
}

pub fn save_session(s: &Session) -> Result<()> {
    ensure_dir(&sessions_dir()?)?;
    let path = sessions_dir()?.join(format!("{}.yaml", s.name));
    atomic_write(&path, serde_yaml::to_string(s)?.as_bytes())
}

// ---------------------------------------------------------------------------
// Agent markdown files

pub fn agent_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?.join(format!("{id}.md")))
}

pub fn agent_log_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?.join(format!("{id}.log.md")))
}

/// Write an agent file with YAML frontmatter + markdown body.
pub fn save_agent(project: &str, agent: &Agent, body_md: &str) -> Result<()> {
    ensure_dir(&agents_dir(project)?)?;
    let path = agent_path(project, &agent.id)?;
    write_frontmatter_file(&path, agent, body_md)
}

/// Render a `---\n<yaml>\n---\n<body>` file. Caller owns the path/dir.
fn write_frontmatter_file<T: serde::Serialize>(path: &Path, front: &T, body: &str) -> Result<()> {
    let yaml = serde_yaml::to_string(front)?;
    let mut buf = String::with_capacity(yaml.len() + body.len() + 32);
    buf.push_str("---\n");
    buf.push_str(&yaml);
    if !yaml.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str("---\n");
    buf.push_str(body);
    if !body.ends_with('\n') {
        buf.push('\n');
    }
    atomic_write(path, buf.as_bytes())
}

/// Parsed result of an agent file.
pub struct AgentFile {
    pub agent: Agent,
    pub body: String,
}

/// Read an agent file from disk and split frontmatter from body.
pub fn load_agent(project: &str, id: &str) -> Result<AgentFile> {
    let path = agent_path(project, id)?;
    let text = fs::read_to_string(&path)?;
    parse_agent_file(&text)
}

pub fn parse_agent_file(text: &str) -> Result<AgentFile> {
    let (front, body) = split_frontmatter(text)
        .ok_or_else(|| shelbi_core::Error::Other("missing frontmatter".into()))?;
    let agent: Agent = serde_yaml::from_str(front)?;
    Ok(AgentFile {
        agent,
        body: body.to_string(),
    })
}

/// Append a line to the agent's `.log.md`. Each line is timestamped.
pub fn append_log(project: &str, id: &str, line: &str) -> Result<()> {
    use std::fs::OpenOptions;
    ensure_dir(&agents_dir(project)?)?;
    let path = agent_log_path(project, id)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let ts = chrono::Utc::now().to_rfc3339();
    writeln!(f, "[{ts}] {line}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Task markdown files

pub fn task_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(tasks_dir(project)?.join(format!("{id}.md")))
}

pub struct TaskFile {
    pub task: Task,
    pub body: String,
}

pub fn save_task(project: &str, task: &Task, body_md: &str) -> Result<()> {
    ensure_dir(&tasks_dir(project)?)?;
    let path = task_path(project, &task.id)?;
    write_frontmatter_file(&path, task, body_md)
}

pub fn load_task(project: &str, id: &str) -> Result<TaskFile> {
    let path = task_path(project, id)?;
    let text = fs::read_to_string(&path)?;
    parse_task_file(&text)
}

pub fn parse_task_file(text: &str) -> Result<TaskFile> {
    let (front, body) = split_frontmatter(text)
        .ok_or_else(|| shelbi_core::Error::Other("missing frontmatter".into()))?;
    let task: Task = serde_yaml::from_str(front)?;
    Ok(TaskFile {
        task,
        body: body.to_string(),
    })
}

pub fn delete_task(project: &str, id: &str) -> Result<()> {
    let path = task_path(project, id)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Every task in the project. Order: column (in [`Column::ALL`] order) then
/// priority ASC. Files that fail to parse are skipped (and logged).
pub fn list_tasks(project: &str) -> Result<Vec<TaskFile>> {
    let dir = tasks_dir(project)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("shelbi: skipping unreadable task file {}: {e}", path.display());
                continue;
            }
        };
        match parse_task_file(&text) {
            Ok(tf) => out.push(tf),
            Err(e) => eprintln!(
                "shelbi: skipping malformed task file {}: {e}",
                path.display()
            ),
        }
    }
    out.sort_by_key(|tf| {
        let col_idx = Column::ALL.iter().position(|c| *c == tf.task.column).unwrap_or(0);
        (col_idx, tf.task.priority)
    });
    Ok(out)
}

/// Tasks in one column, sorted by priority.
pub fn list_column(project: &str, column: Column) -> Result<Vec<TaskFile>> {
    Ok(list_tasks(project)?
        .into_iter()
        .filter(|tf| tf.task.column == column)
        .collect())
}

/// Map of every task id in `project` to its current column. Used to derive
/// the [blocked](Task::is_blocked) state without reloading individual files.
pub fn task_columns(project: &str) -> Result<HashMap<String, Column>> {
    Ok(list_tasks(project)?
        .into_iter()
        .map(|tf| (tf.task.id, tf.task.column))
        .collect())
}

/// Tasks ready to start: in [`Column::Todo`], not blocked by any unfinished
/// dependency. Returned in priority order.
pub fn list_ready(project: &str) -> Result<Vec<TaskFile>> {
    let columns = task_columns(project)?;
    Ok(list_column(project, Column::Todo)?
        .into_iter()
        .filter(|tf| !tf.task.is_blocked(&columns))
        .collect())
}

/// Validate `depends_on` for a task that is about to be saved. Rejects:
///
/// 1. Self-references.
/// 2. IDs that don't exist in the project.
/// 3. Changes that would introduce a cycle.
///
/// `existing_tasks` should be the full task list before the save. The
/// candidate's own id is taken from `task.id` and excluded from existence
/// checks (allowing this fn to be used for both add and modification).
pub fn validate_depends_on(task: &Task, existing_tasks: &[TaskFile]) -> Result<()> {
    if task.depends_on.is_empty() {
        return Ok(());
    }

    // Self-reference rejection.
    if task.depends_on.iter().any(|d| d == &task.id) {
        return Err(shelbi_core::Error::DependencyCycle(format!(
            "{} → {}",
            task.id, task.id
        )));
    }

    // Existence check (every dep must already be a task, EXCEPT the candidate
    // itself — which may or may not be in `existing_tasks` depending on whether
    // this is an add or an edit).
    let known: HashSet<&str> = existing_tasks
        .iter()
        .map(|tf| tf.task.id.as_str())
        .collect();
    let missing: Vec<&str> = task
        .depends_on
        .iter()
        .filter(|d| d.as_str() != task.id && !known.contains(d.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(shelbi_core::Error::UnknownDepends(missing.join(", ")));
    }

    // Cycle detection. Build the dep graph from `existing_tasks` with the
    // candidate's depends_on substituted in (so we catch cycles introduced
    // by this change, not just pre-existing ones). DFS from the candidate.
    let mut graph: HashMap<&str, &[String]> = existing_tasks
        .iter()
        .map(|tf| (tf.task.id.as_str(), tf.task.depends_on.as_slice()))
        .collect();
    graph.insert(task.id.as_str(), task.depends_on.as_slice());

    if let Some(chain) = find_cycle(&graph, task.id.as_str()) {
        return Err(shelbi_core::Error::DependencyCycle(chain.join(" → ")));
    }
    Ok(())
}

/// DFS that returns the offending chain (e.g. `["a", "b", "c", "a"]`) if a
/// cycle is reachable from `start`, otherwise `None`. Iterative to avoid
/// blowing the stack on pathological graphs.
fn find_cycle(graph: &HashMap<&str, &[String]>, start: &str) -> Option<Vec<String>> {
    // Visiting stack with an iteration index so we can backtrack on pop.
    let mut path: Vec<&str> = vec![start];
    let mut on_path: HashSet<&str> = [start].into_iter().collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut iter_stack: Vec<usize> = vec![0];

    while let Some(&node) = path.last() {
        let i = *iter_stack.last().unwrap();
        let deps = graph.get(node).copied().unwrap_or(&[]);
        if i < deps.len() {
            let next = deps[i].as_str();
            *iter_stack.last_mut().unwrap() += 1;
            if on_path.contains(next) {
                // Found a cycle. Build the offending chain starting from
                // the first occurrence of `next` in the path.
                let pos = path.iter().position(|p| *p == next).unwrap();
                let mut chain: Vec<String> = path[pos..].iter().map(|s| s.to_string()).collect();
                chain.push(next.to_string());
                return Some(chain);
            }
            if visited.contains(next) {
                continue;
            }
            // Resolve to a stable key in the graph (so &str borrows survive).
            let key = graph
                .get_key_value(next)
                .map(|(k, _)| *k)
                .unwrap_or(next);
            path.push(key);
            on_path.insert(key);
            iter_stack.push(0);
        } else {
            on_path.remove(node);
            visited.insert(node);
            path.pop();
            iter_stack.pop();
        }
    }
    None
}

/// Move `id` to `new_column`. The task lands at the bottom (priority = N)
/// and the old column gets renumbered contiguous from 0. No-op if the
/// column is unchanged.
pub fn move_task(project: &str, id: &str, new_column: Column) -> Result<()> {
    let TaskFile { mut task, body } = load_task(project, id)?;
    if task.column == new_column {
        return Ok(());
    }
    let old_column = task.column;
    let new_priority = list_column(project, new_column)?.len() as u32;
    task.column = new_column;
    task.priority = new_priority;
    task.updated_at = chrono::Utc::now();
    save_task(project, &task, &body)?;
    renumber_column(project, old_column)?;
    Ok(())
}

/// Re-position `id` to slot `new_priority` within its current column. Other
/// tasks shift to keep the column contiguous from 0.
pub fn set_task_priority(project: &str, id: &str, new_priority: u32) -> Result<()> {
    let target = load_task(project, id)?;
    let column = target.task.column;
    let mut col = list_column(project, column)?;
    let from = col
        .iter()
        .position(|tf| tf.task.id == id)
        .ok_or_else(|| shelbi_core::Error::Other(format!("task `{id}` not in its own column?")))?;
    let to = (new_priority as usize).min(col.len().saturating_sub(1));
    if from == to {
        return Ok(());
    }
    let item = col.remove(from);
    col.insert(to, item);
    write_column_order(project, &col)
}

/// Stamp 0..N priorities onto the ordered slice and persist only the
/// tasks whose priority actually changed.
fn write_column_order(project: &str, ordered: &[TaskFile]) -> Result<()> {
    let now = chrono::Utc::now();
    for (i, tf) in ordered.iter().enumerate() {
        let want = i as u32;
        if tf.task.priority == want {
            continue;
        }
        let mut task = tf.task.clone();
        task.priority = want;
        task.updated_at = now;
        save_task(project, &task, &tf.body)?;
    }
    Ok(())
}

/// Reload `column`'s tasks, sort by current priority, and renumber 0..N.
pub fn renumber_column(project: &str, column: Column) -> Result<()> {
    let col = list_column(project, column)?;
    write_column_order(project, &col)
}

// ---------------------------------------------------------------------------
// Helpers

/// Split a string on `^---\n` … `^---\n`. Returns (frontmatter, body).
fn split_frontmatter(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("---\n").or_else(|| s.strip_prefix("---\r\n"))?;
    // Find closing `---` on its own line.
    let mut search_from = 0usize;
    while let Some(idx) = rest[search_from..].find("\n---") {
        let abs = search_from + idx + 1; // points at the line starting "---"
        let after_dashes = abs + 3;
        let after_byte = rest.as_bytes().get(after_dashes).copied();
        if matches!(after_byte, Some(b'\n') | Some(b'\r') | None) {
            let front = &rest[..abs - 1]; // strip the trailing \n before the closing dashes
            // Skip the closing line and its terminator.
            let body_start = match after_byte {
                Some(b'\r') => after_dashes + 2, // \r\n
                Some(b'\n') => after_dashes + 1,
                None => rest.len(),
                _ => after_dashes,
            };
            let body = &rest[body_start.min(rest.len())..];
            return Some((front, body));
        }
        search_from = abs + 3;
    }
    None
}

/// Atomic write: write to a temp file in the same dir, then rename.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| shelbi_core::Error::Other(format!("no parent dir for {path:?}")))?;
    ensure_dir(dir)?;
    let tmp = path.with_extension(format!(
        "tmp.{}",
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_project(name: &str, override_template: Option<PathBuf>) -> shelbi_core::Project {
        use shelbi_core::*;
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        Project {
            name: name.into(),
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
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: override_template,
        }
    }

    #[test]
    fn worker_settings_template_path_defaults_under_project_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let path = worker_settings_template_path(&p).unwrap();
        assert_eq!(
            path,
            home.join("projects/myapp/worker-settings.json.template")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_settings_template_path_renames_legacy_file_in_project_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let legacy = home.join("projects/myapp/worker-settings.json");
        ensure_dir(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"custom":true}"#).unwrap();
        let path = worker_settings_template_path(&p).unwrap();
        assert!(!legacy.exists(), "legacy file should be renamed away");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"custom":true}"#
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_settings_template_path_leaves_legacy_when_new_already_exists() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let dir = home.join("projects/myapp");
        ensure_dir(&dir).unwrap();
        let legacy = dir.join("worker-settings.json");
        let renamed = dir.join("worker-settings.json.template");
        std::fs::write(&legacy, "legacy").unwrap();
        std::fs::write(&renamed, "new").unwrap();
        let _ = worker_settings_template_path(&p).unwrap();
        // Both files survive; we never overwrite the new one.
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "legacy");
        assert_eq!(std::fs::read_to_string(&renamed).unwrap(), "new");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn worker_settings_template_path_honors_override() {
        let p = fixture_project("myapp", Some(PathBuf::from("/etc/shelbi/p.json")));
        let path = worker_settings_template_path(&p).unwrap();
        assert_eq!(path, PathBuf::from("/etc/shelbi/p.json"));
    }

    #[test]
    fn worker_settings_template_path_expands_tilde_in_override() {
        let p = fixture_project("myapp", Some(PathBuf::from("~/custom/p.json")));
        let path = worker_settings_template_path(&p).unwrap();
        let expected = dirs::home_dir().unwrap().join("custom/p.json");
        assert_eq!(path, expected);
    }

    #[test]
    fn render_worker_settings_uses_embedded_default_when_file_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No template at ~/.shelbi/projects/myapp/worker-settings.json.template yet.
        let p = fixture_project("myapp", None);
        let rendered = render_worker_settings(&p).unwrap();
        // `auto` is mapped to `acceptEdits`.
        assert!(rendered.contains("\"defaultMode\": \"acceptEdits\""));
        assert!(!rendered.contains("{{worker_permissions_mode}}"));
        let _: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn render_worker_settings_reads_project_template_when_present() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let tpl_path = worker_settings_template_path(&p).unwrap();
        ensure_dir(tpl_path.parent().unwrap()).unwrap();
        std::fs::write(
            &tpl_path,
            r#"{"permissions":{"defaultMode":"{{worker_permissions_mode}}"},"custom":true}"#,
        )
        .unwrap();
        let rendered = render_worker_settings(&p).unwrap();
        assert!(rendered.contains("\"custom\":true"));
        assert!(rendered.contains("\"defaultMode\":\"acceptEdits\""));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn render_worker_settings_passes_through_explicit_modes() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut p = fixture_project("myapp", None);
        p.worker_permissions_mode = "bypassPermissions".into();
        let rendered = render_worker_settings(&p).unwrap();
        assert!(rendered.contains("\"defaultMode\": \"bypassPermissions\""));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn default_worker_settings_template_contains_hooks_and_placeholder() {
        let s = DEFAULT_WORKER_SETTINGS_TEMPLATE;
        assert!(s.contains("{{worker_permissions_mode}}"));
        assert!(s.contains("Stop"));
        assert!(s.contains("Notification"));
        assert!(s.contains("UserPromptSubmit"));
        assert!(s.contains("PreToolUse"));
        assert!(s.contains("shelbi:idle"));
        assert!(s.contains("shelbi:blocked"));
        assert!(s.contains("shelbi:working"));
        // The rendered file is JSON after placeholder substitution.
        let rendered = s.replace("{{worker_permissions_mode}}", "acceptEdits");
        let _: serde_json::Value =
            serde_json::from_str(&rendered).expect("template renders to valid JSON");
    }

    #[test]
    fn frontmatter_split_basic() {
        let s = "---\nfoo: 1\nbar: 2\n---\nhello body\n";
        let (front, body) = split_frontmatter(s).unwrap();
        assert_eq!(front, "foo: 1\nbar: 2");
        assert_eq!(body, "hello body\n");
    }

    #[test]
    fn frontmatter_no_frontmatter_returns_none() {
        let s = "just a markdown file\n";
        assert!(split_frontmatter(s).is_none());
    }

    // ---- Storage tests ------------------------------------------------
    //
    // These exercise the on-disk task layout via $SHELBI_HOME. The env var
    // is process-wide so tests must serialize on it.

    use crate::test_lock::LOCK as TEST_LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_task(id: &str, column: Column, priority: u32) -> shelbi_core::Task {
        let now = chrono::Utc::now();
        shelbi_core::Task {
            id: id.to_string(),
            title: id.replace('-', " "),
            column,
            priority,
            assigned_to: None,
            branch: None,
            depends_on: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn task_save_load_roundtrip() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let task = make_task("fix-login", Column::Todo, 3);
        save_task("proj", &task, "# Description\n").unwrap();
        let back = load_task("proj", "fix-login").unwrap();
        assert_eq!(back.task.id, "fix-login");
        assert_eq!(back.task.column, Column::Todo);
        assert_eq!(back.task.priority, 3);
        assert!(back.body.contains("Description"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn move_task_renumbers_old_column() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            save_task("p", &make_task(id, Column::Todo, i as u32), "").unwrap();
        }
        move_task("p", "b", Column::InProgress).unwrap();

        let todo = list_column("p", Column::Todo).unwrap();
        let ids: Vec<_> = todo.iter().map(|tf| tf.task.id.as_str()).collect();
        let prios: Vec<_> = todo.iter().map(|tf| tf.task.priority).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(prios, vec![0, 1]); // renumbered

        let wip = list_column("p", Column::InProgress).unwrap();
        assert_eq!(wip.len(), 1);
        assert_eq!(wip[0].task.id, "b");
        assert_eq!(wip[0].task.priority, 0);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_priority_reorders_within_column() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
            save_task("p", &make_task(id, Column::Backlog, i as u32), "").unwrap();
        }
        // Move 'd' to slot 1 → expected order: a, d, b, c
        set_task_priority("p", "d", 1).unwrap();
        let col = list_column("p", Column::Backlog).unwrap();
        let ids: Vec<_> = col.iter().map(|tf| tf.task.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "d", "b", "c"]);
        std::env::remove_var("SHELBI_HOME");
    }

    fn make_task_with_deps(
        id: &str,
        column: Column,
        priority: u32,
        deps: &[&str],
    ) -> shelbi_core::Task {
        let mut t = make_task(id, column, priority);
        t.depends_on = deps.iter().map(|s| s.to_string()).collect();
        t
    }

    #[test]
    fn validate_depends_on_rejects_missing_id() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("b", Column::Todo, 1, &["a", "ghost"]);
        let err = validate_depends_on(&candidate, &existing).unwrap_err();
        assert!(matches!(err, shelbi_core::Error::UnknownDepends(_)));
        assert!(err.to_string().contains("ghost"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_depends_on_accepts_valid_chain() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Done, 0), "").unwrap();
        save_task("p", &make_task_with_deps("b", Column::Todo, 0, &["a"]), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("c", Column::Todo, 1, &["b"]);
        validate_depends_on(&candidate, &existing).unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_depends_on_rejects_self_reference() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("a", Column::Todo, 0, &["a"]);
        let err = validate_depends_on(&candidate, &existing).unwrap_err();
        assert!(matches!(err, shelbi_core::Error::DependencyCycle(_)));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_depends_on_detects_cycle() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Existing chain: c → b → a (c depends on b, b depends on a).
        // Closing the loop with a → c should be rejected.
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        save_task("p", &make_task_with_deps("b", Column::Todo, 1, &["a"]), "").unwrap();
        save_task("p", &make_task_with_deps("c", Column::Todo, 2, &["b"]), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("a", Column::Todo, 0, &["c"]);
        let err = validate_depends_on(&candidate, &existing).unwrap_err();
        match err {
            shelbi_core::Error::DependencyCycle(chain) => {
                // Chain should mention a, b, c.
                for id in ["a", "b", "c"] {
                    assert!(chain.contains(id), "chain={chain} missing {id}");
                }
            }
            other => panic!("expected cycle, got {other:?}"),
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_ready_filters_blocked_todos() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("done-a", Column::Done, 0), "").unwrap();
        save_task("p", &make_task("inprog-b", Column::InProgress, 0), "").unwrap();
        save_task(
            "p",
            &make_task_with_deps("free", Column::Todo, 0, &["done-a"]),
            "",
        )
        .unwrap();
        save_task(
            "p",
            &make_task_with_deps("blocked", Column::Todo, 1, &["inprog-b"]),
            "",
        )
        .unwrap();
        save_task("p", &make_task("no-deps", Column::Todo, 2), "").unwrap();
        let ready = list_ready("p").unwrap();
        let ids: Vec<_> = ready.iter().map(|tf| tf.task.id.as_str()).collect();
        assert_eq!(ids, vec!["free", "no-deps"]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_tasks_sorts_by_column_then_priority() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("z", Column::Done, 0), "").unwrap();
        save_task("p", &make_task("a", Column::Backlog, 1), "").unwrap();
        save_task("p", &make_task("b", Column::Backlog, 0), "").unwrap();
        save_task("p", &make_task("c", Column::InProgress, 0), "").unwrap();
        let all = list_tasks("p").unwrap();
        let ids: Vec<_> = all.iter().map(|tf| tf.task.id.as_str()).collect();
        // Column::ALL ordering: backlog, todo, in_progress, review, done
        assert_eq!(ids, vec!["b", "a", "c", "z"]);
        std::env::remove_var("SHELBI_HOME");
    }
}
