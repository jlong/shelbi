//! Worker state observed from each worker's tmux pane title.
//!
//! Worker `.claude/settings.json` hooks emit `shelbi:working|idle|blocked`
//! markers via OSC pane-title escapes (see
//! `default_worker_settings.json.template`); the hub-side sidebar poll loop reads
//! the current pane title with `tmux display-message`, parses the trailing
//! marker, and writes any state transition here.
//!
//! Layout (all hub-side, under `$SHELBI_HOME` / `~/.shelbi`):
//!
//! - `workers/<name>/status.yaml` — last observed state per worker.
//! - `events.log` — append-only transition log across all workers.
//!
//! Authoritative state stays on the hub: workers themselves only emit
//! markers; they don't own these files.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::{Column, Result, DEFAULT_WORKFLOW_NAME};

use crate::{atomic_write, ensure_dir, shelbi_home};

/// The marker emitted by the worker's claude hooks. `idle` from the hook
/// wire-format maps to [`WorkerState::AwaitingInput`] — Stop fires when
/// claude finishes a turn and is waiting for the next prompt, which is
/// what we want to surface in the UI ("awaiting input"), not "no work to
/// do".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkerState {
    Working,
    AwaitingInput,
    Blocked,
}

impl WorkerState {
    pub fn as_str(self) -> &'static str {
        match self {
            WorkerState::Working => "working",
            WorkerState::AwaitingInput => "awaiting_input",
            WorkerState::Blocked => "blocked",
        }
    }
}

impl std::fmt::Display for WorkerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// All recognized `shelbi:<…>` pane-title markers. Distinct from
/// [`WorkerState`] because two markers — `idle` (mid-task pause, fires on
/// every claude turn end) and `review` (explicit completion handoff from
/// the worker prompt) — both map to the same persisted state
/// ([`WorkerState::AwaitingInput`]) but have very different downstream
/// semantics: `review` triggers a one-shot kanban move into the review
/// column, `idle` does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneMarker {
    Working,
    Idle,
    Review,
    Blocked,
}

impl PaneMarker {
    /// Persisted [`WorkerState`] for this marker. `Idle` and `Review`
    /// collapse to `AwaitingInput` — the status file just records that
    /// claude is sitting at a prompt; the review-handoff side effect
    /// happens elsewhere.
    pub fn worker_state(self) -> WorkerState {
        match self {
            PaneMarker::Working => WorkerState::Working,
            PaneMarker::Idle | PaneMarker::Review => WorkerState::AwaitingInput,
            PaneMarker::Blocked => WorkerState::Blocked,
        }
    }
}

/// Extract the trailing `shelbi:<marker>` from a pane title. Returns
/// `None` if the marker is missing or unrecognized — the pane is either
/// pre-hook-emit or running something other than a shelbi-deployed worker.
pub fn parse_pane_title_marker(title: &str) -> Option<PaneMarker> {
    let idx = title.rfind("shelbi:")?;
    let tail = &title[idx + "shelbi:".len()..];
    let marker = tail.split(|c: char| c.is_whitespace()).next()?;
    // Trim trailing control chars (BEL, ST) some terminals leave behind.
    let marker = marker.trim_end_matches(|c: char| c.is_control() || c == '\u{0007}');
    match marker {
        "working" => Some(PaneMarker::Working),
        "idle" => Some(PaneMarker::Idle),
        "review" => Some(PaneMarker::Review),
        "blocked" => Some(PaneMarker::Blocked),
        _ => None,
    }
}

/// Convenience: just the persisted state, dropping the marker
/// distinction. Callers that need to know `review` vs `idle` should use
/// [`parse_pane_title_marker`] instead.
pub fn parse_pane_title_state(title: &str) -> Option<WorkerState> {
    parse_pane_title_marker(title).map(PaneMarker::worker_state)
}

/// `~/.shelbi/workers` — root for per-worker status dirs.
pub fn workers_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("workers"))
}

/// `~/.shelbi/workers/<name>/status.yaml`.
pub fn worker_status_path(worker: &str) -> Result<PathBuf> {
    Ok(workers_dir()?.join(worker).join("status.yaml"))
}

/// `~/.shelbi/events.log`.
pub fn events_log_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("events.log"))
}

/// Last observed state for a worker — persisted to disk so a fresh hub
/// process can see the prior state without re-deriving it from the pane
/// title (which may have rolled past the marker).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerStatus {
    pub worker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_task: Option<String>,
    pub state: WorkerState,
    /// When the state most recently *changed*. Stays put across polls
    /// that observe the same state.
    pub last_transition: DateTime<Utc>,
    /// When the marker was most recently observed (any state). Bumped on
    /// every successful poll regardless of transition.
    pub last_seen: DateTime<Utc>,
}

pub fn save_worker_status(status: &WorkerStatus) -> Result<()> {
    let path = worker_status_path(&status.worker)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let yaml = serde_yaml::to_string(status)?;
    atomic_write(&path, yaml.as_bytes())
}

pub fn load_worker_status(worker: &str) -> Result<Option<WorkerStatus>> {
    let path = worker_status_path(worker)?;
    if !path.exists() {
        return Ok(None);
    }
    let text = fs::read_to_string(&path)?;
    Ok(Some(serde_yaml::from_str(&text)?))
}

/// Append `<rfc3339> worker=<name> <prev> -> <new>` to
/// `~/.shelbi/events.log`. `prev` is `None` on the first observation.
pub fn append_worker_event(
    worker: &str,
    prev: Option<WorkerState>,
    new: WorkerState,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let prev_str = prev.map(|s| s.as_str()).unwrap_or("none");
    append_event_line(&format!("{ts} worker={worker} {prev_str} -> {new}"))
}

/// Append a task transition line to `~/.shelbi/events.log` using the
/// workflow-aware line shape from `Plans/workflows.md` §10:
///
/// ```text
/// <rfc3339> task=<id> workflow=<name> <from> -> <to> reason=<short> from_category=<cat> to_category=<cat>
/// ```
///
/// Shares the file with worker events; the orchestrator distinguishes the
/// two by the `task=` vs `worker=` prefix.
///
/// `workflow` is the name from the task's frontmatter (typically
/// `task.workflow_or_default()`); passing `""` is treated as the default
/// workflow so callers that haven't yet plumbed through the lookup don't
/// emit a malformed line. `<from>` / `<to>` are the column-status ids
/// (lowercase) and the `from_category` / `to_category` annotations are
/// derived from [`Column::category`] so reaction rules can match
/// semantically without re-reading the workflow YAML.
///
/// `reason` should be a single short token (whitespace/newlines are folded
/// to underscores so the line stays parseable).
pub fn append_task_event(
    task_id: &str,
    workflow: &str,
    from: Column,
    to: Column,
    reason: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let reason = sanitize_reason(reason);
    let workflow_name = if workflow.trim().is_empty() {
        DEFAULT_WORKFLOW_NAME
    } else {
        workflow
    };
    let from_category = from.category();
    let to_category = to.category();
    append_event_line(&format!(
        "{ts} task={task_id} workflow={workflow_name} {from} -> {to} \
         reason={reason} from_category={from_category} to_category={to_category}"
    ))
}

/// Append `<rfc3339> project=<name> <action> reason=<reason>` to
/// `~/.shelbi/events.log`. Use for project-scoped lifecycle events
/// (currently just `closed` from the palette's quit-project action) that
/// aren't task or worker transitions but should still surface in the
/// activity feed.
pub fn append_project_event(project: &str, action: &str, reason: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let action = sanitize_reason(action);
    let reason = sanitize_reason(reason);
    append_event_line(&format!("{ts} project={project} {action} reason={reason}"))
}

/// Append `<rfc3339> contextstore space=<space> machine=<machine> status=<status> detail=<detail>`
/// to `~/.shelbi/events.log`. Use this to record cross-machine ContextStore
/// sync attempts run after a remote worker hands off for review, so the user
/// (and the orchestrator) can see when a worker's `cstore` writes did — or
/// did not — make it back to the hub copy.
///
/// `detail` is folded to a single token (whitespace → underscores) so the
/// line stays parseable; pass the short rsync stderr excerpt or a status label.
pub fn append_contextstore_event(
    space: &str,
    machine: &str,
    status: &str,
    detail: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let status = sanitize_reason(status);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} contextstore space={space} machine={machine} status={status} detail={detail}"
    ))
}

/// Append `<rfc3339> mode=zen <prev> -> <new> reason=<source>` to
/// `~/.shelbi/events.log`. The orchestrator's tail watches this line shape
/// to react to Zen Mode toggles without re-reading `state.json`. Sources
/// are short tokens identifying the toggle path (`user:cli`, `user:hotkey`,
/// `system:crash-recovery`); whitespace folds to underscores so the line
/// stays parseable.
pub fn append_zen_mode_event(prev: &str, new: &str, source: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let prev = sanitize_reason(prev);
    let new = sanitize_reason(new);
    let source = sanitize_reason(source);
    append_event_line(&format!("{ts} mode=zen {prev} -> {new} reason={source}"))
}

/// Append `<rfc3339> zen-dryrun task=<id> action=<action> detail=<detail>`
/// to `~/.shelbi/events.log`. Emitted by `shelbi zen dry-run` for every
/// would-have decision it makes — distinct prefix so the activity feed
/// can render dry-run rows with their own visual tag and so `grep
/// zen-dryrun` over the log isolates a preview run from real activity.
///
/// `action` and `detail` collapse whitespace to underscores so the line
/// stays a single parseable record (same convention as `append_task_event`).
pub fn append_zen_dryrun_event(task_id: &str, action: &str, detail: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let action = sanitize_reason(action);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} zen-dryrun task={task_id} action={action} detail={detail}"
    ))
}

/// Append `<rfc3339> project=<name> heartbeat` to `~/.shelbi/events.log`.
/// The hub-side poller emits this on its configured `heartbeat` cadence
/// (see `shelbi_core::HeartbeatConfig`) so the orchestrator's
/// `events tail --follow` watch has a guaranteed recurring trigger even
/// when the board is otherwise silent. Heartbeats are filtered out of
/// the human-facing activity feed by default — they're a wake-up signal,
/// not user-facing news.
pub fn append_heartbeat_event(project: &str) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    append_event_line(&format!("{ts} project={project} heartbeat"))
}

/// Append `<rfc3339> dispatch task=<id> worker=<name> status=<status> detail=<detail>`
/// to `~/.shelbi/events.log`. Use this to surface dispatch-time anomalies
/// (e.g. the initial prompt was pasted but Enter never landed) that aren't
/// state transitions but still need to show up in `shelbi events tail` so the
/// orchestrator (and the user) sees them at the moment they happen.
///
/// Detail is a single short token; whitespace folds to underscores so the
/// line stays parseable.
pub fn append_dispatch_event(
    task_id: &str,
    worker: &str,
    status: &str,
    detail: &str,
) -> Result<()> {
    let ts = Utc::now().to_rfc3339();
    let status = sanitize_reason(status);
    let detail = sanitize_reason(detail);
    append_event_line(&format!(
        "{ts} dispatch task={task_id} worker={worker} status={status} detail={detail}"
    ))
}

/// Open `events.log` with O_APPEND and write one terminated line in a
/// single `write_all` call. POSIX guarantees that writes <= PIPE_BUF
/// (4096B) under O_APPEND are atomic relative to other appenders, so
/// concurrent writes from the CLI and the poller interleave whole lines
/// rather than tearing. We must hand the kernel one finished buffer —
/// `writeln!(f, …)` would split the line into separate `write` syscalls
/// per format fragment, which the OS is free to interleave.
fn append_event_line(line: &str) -> Result<()> {
    let path = events_log_path()?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let mut buf = String::with_capacity(line.len() + 1);
    buf.push_str(line);
    buf.push('\n');
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    f.write_all(buf.as_bytes())?;
    Ok(())
}

fn sanitize_reason(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_whitespace() { '_' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-worker-status-test-{}-{}",
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
    fn parses_each_marker() {
        assert_eq!(
            parse_pane_title_state("foo shelbi:working"),
            Some(WorkerState::Working)
        );
        // `idle` from the wire format surfaces as awaiting_input — that's
        // what the user actually wants to see in the UI when claude is
        // sitting at a prompt.
        assert_eq!(
            parse_pane_title_state("shelbi:idle"),
            Some(WorkerState::AwaitingInput)
        );
        assert_eq!(
            parse_pane_title_state("claude · shelbi:blocked"),
            Some(WorkerState::Blocked)
        );
        // `review` is the explicit completion handoff. For status-file
        // purposes it collapses to AwaitingInput (claude is sitting at a
        // prompt); the kanban move side-effect is handled by the poller.
        assert_eq!(
            parse_pane_title_state("shelbi:review"),
            Some(WorkerState::AwaitingInput)
        );
    }

    #[test]
    fn marker_parser_distinguishes_idle_from_review() {
        assert_eq!(
            parse_pane_title_marker("shelbi:idle"),
            Some(PaneMarker::Idle)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:review"),
            Some(PaneMarker::Review)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:working"),
            Some(PaneMarker::Working)
        );
        assert_eq!(
            parse_pane_title_marker("shelbi:blocked"),
            Some(PaneMarker::Blocked)
        );
        assert!(parse_pane_title_marker("shelbi:bogus").is_none());
    }

    #[test]
    fn ignores_unknown_or_missing_markers() {
        assert!(parse_pane_title_state("zsh").is_none());
        assert!(parse_pane_title_state("shelbi:bogus").is_none());
        assert!(parse_pane_title_state("").is_none());
    }

    #[test]
    fn parses_last_marker_when_multiple_present() {
        // OSC re-writes append a fresh title segment; take the right-most
        // marker so a stale `shelbi:idle` earlier in the buffer doesn't
        // mask a current `shelbi:working`.
        assert_eq!(
            parse_pane_title_state("shelbi:idle  shelbi:working"),
            Some(WorkerState::Working)
        );
    }

    #[test]
    fn parses_marker_followed_by_terminator_bytes() {
        // Some terminal stacks (or our own OSC capture path) can leave a
        // BEL or stray newline trailing the marker. The parser should
        // ignore those rather than failing the marker match.
        assert_eq!(
            parse_pane_title_state("shelbi:working\u{0007}"),
            Some(WorkerState::Working)
        );
    }

    #[test]
    fn worker_state_serializes_snake_case() {
        let s = serde_yaml::to_string(&WorkerState::AwaitingInput).unwrap();
        assert!(s.trim().ends_with("awaiting_input"), "got {s:?}");
    }

    #[test]
    fn save_and_load_worker_status_roundtrip() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let now = Utc::now();
        let status = WorkerStatus {
            worker: "alpha".into(),
            current_task: Some("fix-thing".into()),
            state: WorkerState::Working,
            last_transition: now,
            last_seen: now,
        };
        save_worker_status(&status).unwrap();
        let path = worker_status_path("alpha").unwrap();
        assert!(path.exists());
        let back = load_worker_status("alpha").unwrap().unwrap();
        assert_eq!(back.worker, "alpha");
        assert_eq!(back.state, WorkerState::Working);
        assert_eq!(back.current_task.as_deref(), Some("fix-thing"));

        // Missing worker returns None, not an error — the sidebar uses
        // this to bootstrap fresh on first observation.
        assert!(load_worker_status("ghost").unwrap().is_none());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_worker_event_writes_transition_line() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_worker_event("alpha", None, WorkerState::Working).unwrap();
        append_worker_event("alpha", Some(WorkerState::Working), WorkerState::AwaitingInput)
            .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("worker=alpha"));
        assert!(lines[0].contains("none -> working"));
        assert!(lines[1].contains("working -> awaiting_input"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_task_event_round_trips() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event(
            "fix-login",
            "default",
            Column::Todo,
            Column::InProgress,
            "assigned",
        )
        .unwrap();
        append_task_event(
            "fix-login",
            "default",
            Column::InProgress,
            Column::Review,
            "worker_review",
        )
        .unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);

        // Each line must split cleanly back into its fields:
        // `<ts> task=<id> workflow=<name> <from> -> <to> reason=<r>
        //  from_category=<c> to_category=<c>` — see `Plans/workflows.md` §10.
        let parsed: Vec<Vec<&str>> = lines
            .iter()
            .map(|line| line.split(' ').collect::<Vec<&str>>())
            .collect();

        // Timestamp parses as RFC3339.
        for tokens in &parsed {
            let ts = tokens[0];
            chrono::DateTime::parse_from_rfc3339(ts)
                .unwrap_or_else(|_| panic!("not rfc3339: {ts}"));
        }
        assert_eq!(parsed[0][1], "task=fix-login");
        assert_eq!(parsed[0][2], "workflow=default");
        assert_eq!(parsed[0][3], "todo");
        assert_eq!(parsed[0][4], "->");
        assert_eq!(parsed[0][5], "in_progress");
        assert_eq!(parsed[0][6], "reason=assigned");
        assert_eq!(parsed[0][7], "from_category=ready");
        assert_eq!(parsed[0][8], "to_category=active");
        assert_eq!(parsed[1][5], "review");
        assert_eq!(parsed[1][6], "reason=worker_review");
        assert_eq!(parsed[1][7], "from_category=active");
        assert_eq!(parsed[1][8], "to_category=handoff");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_task_event_uses_default_workflow_when_blank() {
        // Callers that haven't yet plumbed through `task.workflow_or_default()`
        // can pass an empty string and still get a well-formed line — the
        // line never carries `workflow=` with no value.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event("a", "", Column::Todo, Column::Done, "assigned").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        assert!(line.contains(" workflow=default "), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn append_task_event_emits_workflow_name_verbatim() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event(
            "ship-it",
            "feature-task",
            Column::InProgress,
            Column::Review,
            "worker:review-marker",
        )
        .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let line = log.lines().next().unwrap();
        assert!(line.contains(" workflow=feature-task "), "line: {line}");
        assert!(line.ends_with(" to_category=handoff"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn heartbeat_event_writes_project_scoped_line() {
        // Shape: `<ts> project=<name> heartbeat`. No `task=`/`worker=`
        // prefix on purpose — the orchestrator's tail uses the leading
        // token after the timestamp to dispatch handlers, and the
        // `project=…` form lets a heartbeat live alongside other
        // future project-scoped events without colliding.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_heartbeat_event("myapp").unwrap();
        append_heartbeat_event("myapp").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2);
        for line in &lines {
            assert!(
                line.ends_with(" project=myapp heartbeat"),
                "line: {line}"
            );
            // Timestamp parses as RFC3339 so `--since` filtering works
            // the same way it does for every other event shape.
            let ts = line.split_whitespace().next().unwrap();
            chrono::DateTime::parse_from_rfc3339(ts)
                .unwrap_or_else(|_| panic!("not rfc3339: {ts}"));
        }

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn dispatch_event_writes_distinct_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_dispatch_event(
            "fix-login",
            "alpha",
            "enter-stalled",
            "no shelbi marker after retry",
        )
        .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        // Shape: `<ts> dispatch task=<id> worker=<name> status=<s> detail=<d>`.
        // The `dispatch` prefix lets `shelbi events tail` show it without
        // colliding with task=... or worker=... lines.
        let line = lines[0];
        assert!(line.contains(" dispatch task=fix-login "), "line: {line}");
        assert!(line.contains(" worker=alpha "), "line: {line}");
        assert!(line.contains(" status=enter-stalled "), "line: {line}");
        // Whitespace in detail folds to underscores so the line stays parseable.
        assert!(line.ends_with(" detail=no_shelbi_marker_after_retry"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_dryrun_event_writes_canonical_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_zen_dryrun_event(
            "fix-typo",
            "consider-auto-promote",
            "mechanically eligible",
        )
        .unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1);
        let line = lines[0];
        // Prefix lets the activity feed match and render dry-run rows
        // with a distinct visual tag. Detail whitespace folds to
        // underscores so the line stays parseable.
        assert!(line.contains(" zen-dryrun task=fix-typo "), "line: {line}");
        assert!(line.contains(" action=consider-auto-promote "), "line: {line}");
        assert!(line.ends_with(" detail=mechanically_eligible"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_mode_event_writes_canonical_shape() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_zen_mode_event("off", "on", "user:cli").unwrap();
        append_zen_mode_event("on", "paused", "user:hotkey").unwrap();
        append_zen_mode_event("paused", "off", "system:crash-recovery").unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 3);
        // No project tag — the orchestrator's tail is hub-global and the
        // line is project-implicit. Shape: `<ts> mode=zen <prev> -> <new>
        // reason=<source>`.
        assert!(lines[0].contains(" mode=zen off -> on reason=user:cli"));
        assert!(lines[1].contains(" mode=zen on -> paused reason=user:hotkey"));
        assert!(lines[2].contains(" mode=zen paused -> off reason=system:crash-recovery"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn task_event_sanitizes_whitespace_in_reason() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        append_task_event("a", "default", Column::Todo, Column::Done, "user moved\nit").unwrap();
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        // The reason newline must not produce a torn line — the line keeps
        // going past `reason=` into the trailing category annotations.
        assert_eq!(lines.len(), 1);
        assert!(
            lines[0].contains(" reason=user_moved_it "),
            "line: {}",
            lines[0]
        );
        assert!(lines[0].ends_with(" to_category=done"), "line: {}", lines[0]);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn concurrent_task_and_worker_appends_dont_tear() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        const N: usize = 200;
        let task_thread = std::thread::spawn(|| {
            for i in 0..N {
                append_task_event(
                    &format!("t{i:04}"),
                    "default",
                    Column::Todo,
                    Column::InProgress,
                    "assigned",
                )
                .unwrap();
            }
        });
        let worker_thread = std::thread::spawn(|| {
            for i in 0..N {
                let prev = if i == 0 {
                    None
                } else {
                    Some(WorkerState::Working)
                };
                append_worker_event("alpha", prev, WorkerState::AwaitingInput).unwrap();
            }
        });
        task_thread.join().unwrap();
        worker_thread.join().unwrap();

        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 2 * N, "expected {} lines, got {}", 2 * N, lines.len());

        let mut task_lines = 0usize;
        let mut worker_lines = 0usize;
        for line in &lines {
            // No line should mix prefixes — that would mean an interleaved
            // write tore one record across another.
            assert!(line.contains(" -> "), "malformed: {line:?}");
            let has_task = line.contains(" task=");
            let has_worker = line.contains(" worker=");
            assert!(
                has_task ^ has_worker,
                "torn or unrecognized line: {line:?}"
            );
            if has_task {
                task_lines += 1;
                assert!(line.contains("reason="), "task line missing reason: {line:?}");
            } else {
                worker_lines += 1;
            }
        }
        assert_eq!(task_lines, N);
        assert_eq!(worker_lines, N);

        std::env::remove_var("SHELBI_HOME");
    }
}
