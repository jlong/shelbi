//! Machine-readable orchestrator transport primitives.
//!
//! These commands expose the append-only hub event stream with durable
//! cursors. They intentionally do not dispatch work or mutate board state.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use clap::Subcommand;
use serde::Serialize;

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum OrchestratorCmd {
    /// Read project-scoped events since a cursor.
    Events {
        #[command(subcommand)]
        cmd: OrchestratorEventsCmd,
    },
}

#[derive(Debug, Subcommand)]
pub enum OrchestratorEventsCmd {
    /// Drain all currently pending project events since the cursor.
    Drain {
        /// Explicit cursor override. Omit to resume from — and persist back
        /// to — the durable cursor in the project config dir. Use `0` to
        /// replay the whole log from the start.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Wait up to `--timeout` for the next non-empty project event batch.
    Next {
        /// Durable cursor returned by a prior drain. Use `0` for the first read.
        #[arg(long, default_value = "0")]
        cursor: String,
        /// Maximum wait, e.g. `10s`, `2m`, `1h`.
        #[arg(long)]
        timeout: String,
    },
}

pub fn run(project_opt: Option<String>, cmd: OrchestratorCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        OrchestratorCmd::Events { cmd } => match cmd {
            OrchestratorEventsCmd::Drain { cursor } => {
                let cursor_override = cursor.as_deref().map(parse_cursor).transpose()?;
                let response = drain_persisted(&project, cursor_override)?;
                print_response(&response)
            }
            OrchestratorEventsCmd::Next { cursor, timeout } => {
                let timeout = super::events::parse_duration(&timeout)?;
                let response = wait_next(&project, parse_cursor(&cursor)?, timeout)?;
                print_response(&response)
            }
        },
    }
}

fn print_response(response: &DrainResponse) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(response)?);
    Ok(())
}

fn wait_next(project: &str, mut cursor: u64, timeout: Duration) -> Result<DrainResponse> {
    let deadline = Instant::now() + timeout;
    loop {
        let response = drain_once(project, cursor)?;
        cursor = response.cursor_offset;
        if !response.events.is_empty() || Instant::now() >= deadline {
            return Ok(response);
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        thread::sleep(remaining.min(Duration::from_millis(250)));
    }
}

fn drain_once(project: &str, cursor: u64) -> Result<DrainResponse> {
    let path = shelbi_state::events_log_path().map_err(|e| anyhow!(e))?;
    let scope = ProjectScope::load(project)?;

    let mut file = match fs::File::open(&path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DrainResponse::empty(project, 0));
        }
        Err(e) => return Err(anyhow::Error::new(e).context("opening events.log")),
    };

    let len = file.metadata().context("stat events.log")?.len();
    let start = if cursor > len { 0 } else { cursor };
    file.seek(SeekFrom::Start(start))
        .context("seek events.log")?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut buf).context("read events.log")?;

    let complete_len = match buf.iter().rposition(|b| *b == b'\n') {
        Some(idx) => idx + 1,
        None => 0,
    };
    let next_cursor = start + complete_len as u64;
    let text = String::from_utf8_lossy(&buf[..complete_len]);
    let mut events = Vec::new();
    let mut line_offset = start;

    for line_with_nl in text.split_inclusive('\n') {
        let line_len = line_with_nl.len() as u64;
        let line = line_with_nl.trim_end_matches(['\r', '\n']);
        let line_cursor = line_offset + line_len;
        if !line.is_empty() {
            if let Some(event) = normalize_line(&scope, line_offset, line_cursor, line) {
                events.push(event);
            }
        }
        line_offset = line_cursor;
    }

    Ok(DrainResponse {
        project: project.to_string(),
        cursor: next_cursor.to_string(),
        cursor_offset: next_cursor,
        events,
    })
}

/// Drain with durable-cursor persistence anchored in the project config
/// dir (`~/.shelbi/projects/<name>/event-cursor`), independent of the
/// caller's shell cwd.
///
/// * `cursor_override = None` — resume from the persisted cursor.
/// * `cursor_override = Some(n)` — start at `n` (an explicit replay).
///
/// Either way the new cursor is written back only after `drain_once`
/// succeeds, so a failed drain never clobbers the persisted position.
fn drain_persisted(project: &str, cursor_override: Option<u64>) -> Result<DrainResponse> {
    let start = match cursor_override {
        Some(cursor) => cursor,
        None => read_persisted_cursor(project)?,
    };
    let response = drain_once(project, start)?;
    write_persisted_cursor(project, response.cursor_offset)?;
    Ok(response)
}

/// Durable cursor path: a fixed location in the project config dir, NOT
/// under `.claude/` — Shelbi state is runner-agnostic (the Codex runner
/// drains the same stream) and `.claude/` is deployed agent config, not
/// durable state.
fn cursor_path(project: &str) -> Result<PathBuf> {
    Ok(shelbi_state::project_dir(project)
        .map_err(|e| anyhow!(e))?
        .join("event-cursor"))
}

fn read_persisted_cursor(project: &str) -> Result<u64> {
    let path = cursor_path(project)?;
    match fs::read_to_string(&path) {
        // A missing or malformed cursor file resumes from the log start,
        // matching the old shell `cat ... || echo 0` behavior.
        Ok(text) => Ok(text.trim().parse().unwrap_or(0)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(anyhow::Error::new(e).context("reading event cursor")),
    }
}

fn write_persisted_cursor(project: &str, cursor: u64) -> Result<()> {
    let path = cursor_path(project)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("creating project dir for event cursor")?;
    }
    // Write-then-rename so a torn write never leaves a partial cursor a
    // later drain would misparse to 0 and replay the whole log.
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, cursor.to_string()).context("writing event cursor")?;
    fs::rename(&tmp, &path).context("persisting event cursor")?;
    Ok(())
}

fn parse_cursor(cursor: &str) -> Result<u64> {
    cursor
        .trim()
        .parse()
        .map_err(|_| anyhow!("cursor `{cursor}` is not a Shelbi event cursor"))
}

#[derive(Debug)]
struct ProjectScope {
    project: String,
    task_ids: HashSet<String>,
}

impl ProjectScope {
    fn load(project: &str) -> Result<Self> {
        let task_ids = shelbi_state::list_tasks(project)
            .map_err(|e| anyhow!(e))?
            .into_iter()
            .map(|tf| tf.task.id)
            .collect();
        Ok(Self {
            project: project.to_string(),
            task_ids,
        })
    }
}

#[derive(Debug, Serialize)]
struct DrainResponse {
    project: String,
    cursor: String,
    #[serde(skip)]
    cursor_offset: u64,
    events: Vec<NormalizedEvent>,
}

impl DrainResponse {
    fn empty(project: &str, cursor: u64) -> Self {
        Self {
            project: project.to_string(),
            cursor: cursor.to_string(),
            cursor_offset: cursor,
            events: Vec::new(),
        }
    }
}

#[derive(Debug, Serialize)]
struct NormalizedEvent {
    cursor: String,
    offset: u64,
    timestamp: Option<String>,
    kind: String,
    raw: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    task: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    metadata: BTreeMap<String, String>,
}

fn normalize_line(
    scope: &ProjectScope,
    offset: u64,
    cursor: u64,
    line: &str,
) -> Option<NormalizedEvent> {
    let parsed = ParsedLine::parse(line);
    if !line_belongs_to_project(scope, &parsed) {
        return None;
    }

    let kind = event_kind(&parsed);
    Some(NormalizedEvent {
        cursor: cursor.to_string(),
        offset,
        timestamp: parsed.timestamp,
        kind,
        raw: line.to_string(),
        task: parsed.fields.get("task").cloned(),
        workspace: parsed.fields.get("workspace").cloned(),
        workflow: parsed.fields.get("workflow").cloned(),
        from: parsed.from,
        to: parsed.to,
        reason: parsed.fields.get("reason").cloned(),
        metadata: parsed.fields,
    })
}

fn line_belongs_to_project(scope: &ProjectScope, parsed: &ParsedLine) -> bool {
    if let Some(project) = parsed.fields.get("project") {
        return project == &scope.project;
    }

    // Compatibility for pre-project-scoped task/message lines. New task
    // transition events include `project=`, but old logs are still useful.
    // Workspace-only legacy lines are intentionally excluded because
    // workspace names are unique only within a project.
    parsed
        .fields
        .get("task")
        .is_some_and(|task| scope.task_ids.contains(task))
}

fn event_kind(parsed: &ParsedLine) -> String {
    if parsed.fields.contains_key("heartbeat") {
        return "heartbeat".into();
    }
    if parsed.fields.get("mode").is_some_and(|mode| mode == "zen") && parsed.from.is_some() {
        return "zen_mode_transition".into();
    }
    if parsed.fields.contains_key("task") && parsed.from.is_some() {
        return "task_transition".into();
    }
    if parsed.fields.contains_key("workspace") && parsed.from.is_some() {
        return "workspace_transition".into();
    }
    if parsed.fields.contains_key("message") {
        return "message".into();
    }
    if parsed.fields.contains_key("question") {
        return "clarification".into();
    }
    if parsed
        .fields
        .get("pane_alive")
        .is_some_and(|alive| alive == "false")
        || parsed
            .fields
            .get("server_alive")
            .is_some_and(|alive| alive == "false")
    {
        return "pane_death".into();
    }
    if parsed.fields.contains_key("pane_alive") || parsed.fields.contains_key("server_alive") {
        return "pane_lifecycle".into();
    }
    if parsed.fields.contains_key("supervision") {
        return "supervision".into();
    }
    if parsed.fields.contains_key("dispatch") {
        return "dispatch".into();
    }
    if parsed.fields.contains_key("rebase") {
        return "rebase".into();
    }
    "event".into()
}

#[derive(Debug)]
struct ParsedLine {
    timestamp: Option<String>,
    fields: BTreeMap<String, String>,
    from: Option<String>,
    to: Option<String>,
}

impl ParsedLine {
    fn parse(line: &str) -> Self {
        let mut parts = line.split_whitespace().collect::<Vec<_>>();
        let timestamp = parts.first().map(|s| (*s).to_string());
        if !parts.is_empty() {
            parts.remove(0);
        }

        let mut fields = BTreeMap::new();
        for part in &parts {
            if let Some((k, v)) = part.split_once('=') {
                fields.insert(k.to_string(), v.to_string());
            } else if *part == "heartbeat" {
                fields.insert("heartbeat".into(), "true".into());
            } else if *part == "dispatch" || *part == "rebase" || *part == "zen-dryrun" {
                fields.insert((*part).to_string(), "true".into());
            }
        }

        let mut from = None;
        let mut to = None;
        if let Some(idx) = parts.iter().position(|p| *p == "->") {
            if idx > 0 {
                from = Some(parts[idx - 1].to_string());
            }
            if idx + 1 < parts.len() {
                to = Some(parts[idx + 1].to_string());
            }
        }

        Self {
            timestamp,
            fields,
            from,
            to,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;
    use chrono::Utc;
    use shelbi_core::{Column, Task};
    use shelbi_state::{
        append_external_event, append_heartbeat_event, append_task_event, append_workspace_event,
        list_tasks, save_task, WorkspaceState,
    };
    use tempfile::TempDir;

    struct TestHome {
        _tmp: TempDir,
    }

    impl Drop for TestHome {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_ROOT");
        }
    }

    fn setup_home() -> (std::sync::MutexGuard<'static, ()>, TestHome) {
        let guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::remove_var("SHELBI_HOME");
        std::env::set_var("SHELBI_ROOT", tmp.path());
        (guard, TestHome { _tmp: tmp })
    }

    fn save_demo_task(project: &str, id: &str) {
        save_demo_task_in_column(project, id, Column::todo());
    }

    fn save_demo_task_in_column(project: &str, id: &str, column: Column) {
        let now = Utc::now();
        let task = Task {
            id: id.into(),
            title: id.into(),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        };
        save_task(project, &task, "body").unwrap();
    }

    fn event<'a>(response: &'a DrainResponse, kind: &str, id: &str) -> &'a NormalizedEvent {
        response
            .events
            .iter()
            .find(|event| {
                event.kind == kind
                    && (event.task.as_deref() == Some(id) || event.workspace.as_deref() == Some(id))
            })
            .unwrap_or_else(|| panic!("missing {kind} event for {id}: {:?}", response.events))
    }

    #[test]
    fn drain_filters_by_project_and_returns_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "owned");
        save_demo_task("other", "foreign");
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();
        append_workspace_event("other", "alpha", None, WorkspaceState::Working).unwrap();
        append_task_event(
            "demo",
            "owned",
            "default",
            Column::todo(),
            Column::done(),
            "test",
        )
        .unwrap();
        append_task_event(
            "other",
            "foreign",
            "default",
            Column::todo(),
            Column::done(),
            "test",
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.project, "demo");
        assert!(response.cursor_offset > 0);
        assert_eq!(response.events.len(), 2);
        assert_eq!(response.events[0].workspace.as_deref(), Some("alpha"));
        assert_eq!(response.events[1].task.as_deref(), Some("owned"));
        assert_eq!(response.events[1].kind, "task_transition");
    }

    #[test]
    fn drain_starts_after_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();
        let first = drain_once("demo", 0).unwrap();
        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();

        let second = drain_once("demo", first.cursor_offset).unwrap();

        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].task.as_deref(), Some("second"));
    }

    #[test]
    fn next_timeout_returns_updated_cursor_for_unrelated_events() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("other", "alpha", None, WorkspaceState::Working).unwrap();

        let response = wait_next("demo", 0, Duration::from_millis(1)).unwrap();

        assert!(response.events.is_empty());
        assert!(response.cursor_offset > 0);
    }

    #[test]
    fn stale_cursor_after_rotation_restarts_at_current_log_start() {
        let (_guard, _tmp) = setup_home();
        append_workspace_event("demo", "alpha", None, WorkspaceState::Working).unwrap();

        let response = drain_once("demo", 999_999).unwrap();

        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].workspace.as_deref(), Some("alpha"));
    }

    #[test]
    fn drain_presents_ready_task_and_idle_workspace_as_distinct_facts() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-task");
        append_task_event(
            "demo",
            "ready-task",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();
        append_workspace_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.events.len(), 2);
        let task = &response.events[0];
        assert_eq!(task.kind, "task_transition");
        assert_eq!(task.task.as_deref(), Some("ready-task"));
        assert_eq!(task.from.as_deref(), Some("backlog"));
        assert_eq!(task.to.as_deref(), Some("todo"));
        assert_eq!(
            task.metadata.get("to_category").map(String::as_str),
            Some("ready")
        );
        assert_eq!(task.reason.as_deref(), Some("user:move"));

        let workspace = &response.events[1];
        assert_eq!(workspace.kind, "workspace_transition");
        assert_eq!(workspace.workspace.as_deref(), Some("alpha"));
        assert_eq!(workspace.from.as_deref(), Some("working"));
        assert_eq!(workspace.to.as_deref(), Some("awaiting_input"));
    }

    #[test]
    fn polling_turn_boundary_delivers_ready_task_without_scheduling_it() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-task");
        append_task_event(
            "demo",
            "ready-task",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();

        // Polling-only runners see the fact when they drain at the next turn
        // boundary. This is a fact batch, not an implicit dispatch.
        let response = drain_once("demo", 0).unwrap();
        let task = event(&response, "task_transition", "ready-task");

        assert_eq!(task.from.as_deref(), Some("backlog"));
        assert_eq!(task.to.as_deref(), Some("todo"));
        assert_eq!(
            task.metadata.get("to_category").map(String::as_str),
            Some("ready")
        );
        let stored = list_tasks("demo").unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].task.column, Column::todo());
        assert_eq!(
            stored[0].task.assigned_to, None,
            "event delivery must not assign or start work"
        );
    }

    #[test]
    fn drain_presents_active_to_handoff_and_idle_workspace_as_facts() {
        let (_guard, _tmp) = setup_home();
        save_demo_task_in_column("demo", "handoff-task", Column::review());
        append_task_event(
            "demo",
            "handoff-task",
            "default",
            Column::in_progress(),
            Column::review(),
            "workspace:ready",
        )
        .unwrap();
        append_workspace_event(
            "demo",
            "alpha",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();
        let handoff = event(&response, "task_transition", "handoff-task");
        assert_eq!(handoff.from.as_deref(), Some("in_progress"));
        assert_eq!(handoff.to.as_deref(), Some("review"));
        assert_eq!(
            handoff.metadata.get("from_category").map(String::as_str),
            Some("active")
        );
        assert_eq!(
            handoff.metadata.get("to_category").map(String::as_str),
            Some("handoff")
        );

        let workspace = event(&response, "workspace_transition", "alpha");
        assert_eq!(workspace.from.as_deref(), Some("working"));
        assert_eq!(workspace.to.as_deref(), Some("awaiting_input"));
    }

    #[test]
    fn idle_workspace_event_arrives_while_ready_task_remains_orchestrator_choice() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-next");
        append_workspace_event(
            "demo",
            "dev-1",
            Some(WorkspaceState::Working),
            WorkspaceState::AwaitingInput,
        )
        .unwrap();

        let response = drain_once("demo", 0).unwrap();
        let workspace = event(&response, "workspace_transition", "dev-1");
        assert_eq!(workspace.to.as_deref(), Some("awaiting_input"));

        let stored = list_tasks("demo").unwrap();
        assert_eq!(stored[0].task.id, "ready-next");
        assert_eq!(stored[0].task.column, Column::todo());
        assert_eq!(
            stored[0].task.assigned_to, None,
            "idle-workspace delivery must not consume the ready task"
        );
    }

    #[test]
    fn heartbeat_backstop_surfaces_missed_immediate_event_for_bounded_sweep() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "ready-after-miss");
        append_task_event(
            "demo",
            "ready-after-miss",
            "default",
            Column::backlog(),
            Column::todo(),
            "user:move",
        )
        .unwrap();
        let missed = drain_once("demo", 0).unwrap();

        append_heartbeat_event("demo", 1, 1, None).unwrap();

        let response = wait_next("demo", missed.cursor_offset, Duration::from_millis(250)).unwrap();
        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].kind, "heartbeat");
        assert_eq!(
            response.events[0]
                .metadata
                .get("zen_eligible")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            response.events[0]
                .metadata
                .get("idle_workspaces")
                .map(String::as_str),
            Some("1")
        );
    }

    #[test]
    fn drain_labels_heartbeat_and_pane_death_without_scheduling() {
        let (_guard, _tmp) = setup_home();
        append_heartbeat_event("demo", 1, 1, None).unwrap();
        append_external_event("project=demo workspace=alpha pane_alive=false reason=signal:SIGHUP")
            .unwrap();
        append_external_event("project=demo workspace=review server_alive=false reason=exit:1")
            .unwrap();

        let response = drain_once("demo", 0).unwrap();

        let kinds = response
            .events
            .iter()
            .map(|event| event.kind.as_str())
            .collect::<Vec<_>>();
        assert_eq!(kinds, vec!["heartbeat", "pane_death", "pane_death"]);
        assert_eq!(
            response.events[0]
                .metadata
                .get("zen_eligible")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            response.events[0]
                .metadata
                .get("idle_workspaces")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(response.events[1].workspace.as_deref(), Some("alpha"));
        assert_eq!(response.events[2].workspace.as_deref(), Some("review"));
    }

    #[test]
    fn cursor_path_lives_in_project_config_dir_not_claude() {
        let (_guard, _tmp) = setup_home();
        let path = cursor_path("demo").unwrap();
        assert_eq!(path.file_name().and_then(|s| s.to_str()), Some("event-cursor"));
        assert_eq!(
            path.parent(),
            Some(shelbi_state::project_dir("demo").unwrap().as_path())
        );
        assert!(
            !path.components().any(|c| c.as_os_str() == ".claude"),
            "cursor must not live under .claude/: {path:?}"
        );
    }

    #[test]
    fn drain_persisted_resumes_from_persisted_cursor() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();

        // No override: reads 0 (no file yet), drains, and persists the new cursor.
        let first = drain_persisted("demo", None).unwrap();
        assert_eq!(first.events.len(), 1);
        assert_eq!(first.events[0].task.as_deref(), Some("first"));
        let stored = fs::read_to_string(cursor_path("demo").unwrap()).unwrap();
        assert_eq!(stored.trim(), first.cursor_offset.to_string());

        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();

        // No override again: resumes from the persisted cursor, so the first
        // event is not replayed.
        let second = drain_persisted("demo", None).unwrap();
        assert_eq!(second.events.len(), 1);
        assert_eq!(second.events[0].task.as_deref(), Some("second"));
    }

    #[test]
    fn drain_persisted_explicit_cursor_overrides_and_advances_persisted() {
        let (_guard, _tmp) = setup_home();
        save_demo_task("demo", "first");
        append_task_event(
            "demo",
            "first",
            "default",
            Column::todo(),
            Column::done(),
            "one",
        )
        .unwrap();
        // Consume the first event and persist a non-zero cursor.
        let first = drain_persisted("demo", None).unwrap();
        assert_eq!(first.events.len(), 1);

        save_demo_task("demo", "second");
        append_task_event(
            "demo",
            "second",
            "default",
            Column::todo(),
            Column::done(),
            "two",
        )
        .unwrap();

        // Explicit `--cursor 0` replays the whole log despite the persisted cursor.
        let replay = drain_persisted("demo", Some(0)).unwrap();
        assert_eq!(replay.events.len(), 2);

        // A successful override drain advances the persisted cursor too.
        let stored = fs::read_to_string(cursor_path("demo").unwrap()).unwrap();
        assert_eq!(stored.trim(), replay.cursor_offset.to_string());
    }

    #[test]
    fn read_persisted_cursor_defaults_to_zero_when_absent() {
        let (_guard, _tmp) = setup_home();
        assert_eq!(read_persisted_cursor("demo").unwrap(), 0);
    }

    #[test]
    fn pane_death_fact_is_delivered_without_supervision_restart_action() {
        let (_guard, _tmp) = setup_home();
        append_external_event("project=demo workspace=alpha pane_alive=false reason=exit:1")
            .unwrap();

        let response = drain_once("demo", 0).unwrap();

        assert_eq!(response.events.len(), 1);
        assert_eq!(response.events[0].kind, "pane_death");
        assert_eq!(response.events[0].workspace.as_deref(), Some("alpha"));
        assert_eq!(response.events[0].reason.as_deref(), Some("exit:1"));
        assert!(
            !response.events[0].metadata.contains_key("supervision"),
            "pane death delivery must not be conflated with auto-restart"
        );
    }
}
