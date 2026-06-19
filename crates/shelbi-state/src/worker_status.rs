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

use shelbi_core::Result;

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

/// Extract the trailing `shelbi:<state>` marker from a pane title and map
/// it to a [`WorkerState`]. Returns `None` if the marker is missing or
/// unrecognized — the pane is either pre-hook-emit or running something
/// other than a shelbi-deployed worker.
pub fn parse_pane_title_state(title: &str) -> Option<WorkerState> {
    let idx = title.rfind("shelbi:")?;
    let tail = &title[idx + "shelbi:".len()..];
    let marker = tail.split(|c: char| c.is_whitespace()).next()?;
    // Trim trailing control chars (BEL, ST) some terminals leave behind.
    let marker = marker.trim_end_matches(|c: char| c.is_control() || c == '\u{0007}');
    match marker {
        "working" => Some(WorkerState::Working),
        "idle" => Some(WorkerState::AwaitingInput),
        "blocked" => Some(WorkerState::Blocked),
        _ => None,
    }
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
    let path = events_log_path()?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let ts = Utc::now().to_rfc3339();
    let prev_str = prev.map(|s| s.as_str()).unwrap_or("none");
    writeln!(f, "{ts} worker={worker} {prev_str} -> {new}")?;
    Ok(())
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
}
