//! A runner's own report of its pane's state, sent over the hub's
//! `runner-state` verb.
//!
//! Claude Code and Codex announce their state with `shelbi:<state>` pane-title
//! markers, which the poller reads off the pane. A runner with a structured
//! channel of its own (today OpenCode, through Shelbi's plugin) reports it to
//! the hub instead: the daemon writes the latest report to
//! `~/.shelbi/projects/<project>/workspaces/<ws>/runner-state.json`, and the
//! poller prefers it over the title marker for that workspace. A report can
//! say why (`reason`, such as `permission` or `provider.free-limit`) and until
//! when (`until`, a scheduled retry), which a title marker can't.
//!
//! A dispatch clears the record before it launches a pane, and reaping a slot
//! clears it with `status.yaml`, so a report from an earlier pane never
//! speaks for the next one. A report also names its task; the poller ignores
//! one for a task the slot no longer serves.

use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::Result;

use crate::{atomic_write, ensure_dir, workspaces_dir, WorkspaceState};

/// File name of the record, beside the workspace's `status.yaml`.
pub const RUNNER_STATE_FILE: &str = "runner-state.json";

/// Longest `reason` the hub keeps. Reasons are short tokens; anything longer
/// is cut, never rejected, so a chatty runner still lands its state.
pub const MAX_RUNNER_REASON_CHARS: usize = 64;

/// The states a runner reports. The vocabulary is the title markers' plus
/// `paused`, which a title marker never carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerState {
    /// Running a turn.
    Working,
    /// The turn ended and the runner waits at its prompt.
    Idle,
    /// The turn ended, but background work the runner started (a subagent)
    /// will wake it without the user.
    Background,
    /// Waiting on a person: a permission ask or a question.
    Blocked,
    /// Stopped on a provider limit, or waiting out a scheduled retry.
    Paused,
}

impl RunnerState {
    /// Parse the wire token (`working`, `idle`, `background`, `blocked`,
    /// `paused`).
    pub fn parse(s: &str) -> Option<RunnerState> {
        match s {
            "working" => Some(RunnerState::Working),
            "idle" => Some(RunnerState::Idle),
            "background" => Some(RunnerState::Background),
            "blocked" => Some(RunnerState::Blocked),
            "paused" => Some(RunnerState::Paused),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            RunnerState::Working => "working",
            RunnerState::Idle => "idle",
            RunnerState::Background => "background",
            RunnerState::Blocked => "blocked",
            RunnerState::Paused => "paused",
        }
    }

    /// The workspace state this report stands for, read the way the matching
    /// title marker is ([`crate::PaneMarker::workspace_state`]).
    pub fn workspace_state(self) -> WorkspaceState {
        match self {
            RunnerState::Working => WorkspaceState::Working,
            RunnerState::Idle => WorkspaceState::AwaitingInput,
            RunnerState::Background => WorkspaceState::Waiting,
            RunnerState::Blocked => WorkspaceState::Blocked,
            RunnerState::Paused => WorkspaceState::Paused,
        }
    }
}

/// The latest `runner-state` report for one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunnerStateReport {
    pub state: RunnerState,
    /// Why, as a short token: `permission` / `question` for `blocked`, the
    /// provider's error type (`provider.free-limit`, `provider.rate-limit`)
    /// for `paused`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// When a `paused` runner expects to carry on by itself (a scheduled
    /// retry). Absent when it waits on something else.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// The task the runner was serving when it reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// When the hub received the report.
    pub at: DateTime<Utc>,
}

impl RunnerStateReport {
    /// Whether this report speaks for a slot serving `task`. A report with no
    /// task does; one naming another task is left over from it.
    pub fn is_for_task(&self, task: Option<&str>) -> bool {
        match (self.task.as_deref(), task) {
            (None, _) => true,
            (Some(reported), Some(current)) => reported == current,
            (Some(_), None) => false,
        }
    }
}

/// Fold a runner's `reason` into the short token the hub keeps: ASCII letters,
/// digits and `.:_-` only (anything else becomes `-`), at most
/// [`MAX_RUNNER_REASON_CHARS`]. `None` when nothing is left.
pub fn sanitize_runner_reason(reason: &str) -> Option<String> {
    let token: String = reason
        .trim()
        .chars()
        .take(MAX_RUNNER_REASON_CHARS)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | ':' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let token = token.trim_matches('-').to_string();
    (!token.is_empty()).then_some(token)
}

/// `~/.shelbi/projects/<project>/workspaces/<ws>/runner-state.json`.
pub fn runner_state_path(project: &str, workspace: &str) -> Result<PathBuf> {
    crate::ensure_flat_path_component("workspace", workspace)?;
    Ok(workspaces_dir(project)?.join(workspace).join(RUNNER_STATE_FILE))
}

/// Record `report` as `workspace`'s latest, replacing the one before.
pub fn write_runner_state(project: &str, workspace: &str, report: &RunnerStateReport) -> Result<()> {
    let path = runner_state_path(project, workspace)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    let mut body = serde_json::to_vec(report)
        .map_err(|e| shelbi_core::Error::Other(format!("encoding runner state: {e}")))?;
    body.push(b'\n');
    atomic_write(&path, &body)
}

/// `workspace`'s latest report. A missing or unreadable record reads as
/// `None`: the poller then reads the pane the way it always has.
pub fn read_runner_state(project: &str, workspace: &str) -> Option<RunnerStateReport> {
    let path = runner_state_path(project, workspace).ok()?;
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Remove `workspace`'s record. Idempotent: a missing file is not an error.
pub fn clear_runner_state(project: &str, workspace: &str) -> Result<()> {
    let path = runner_state_path(project, workspace)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-runner-state-test-{}-{}",
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
    fn states_parse_and_map_like_their_title_markers() {
        for (token, state, ws) in [
            ("working", RunnerState::Working, WorkspaceState::Working),
            ("idle", RunnerState::Idle, WorkspaceState::AwaitingInput),
            ("background", RunnerState::Background, WorkspaceState::Waiting),
            ("blocked", RunnerState::Blocked, WorkspaceState::Blocked),
            ("paused", RunnerState::Paused, WorkspaceState::Paused),
        ] {
            assert_eq!(RunnerState::parse(token), Some(state));
            assert_eq!(state.as_str(), token);
            assert_eq!(state.workspace_state(), ws);
        }
        assert_eq!(RunnerState::parse("review"), None);
        assert_eq!(RunnerState::parse("Working"), None);
    }

    #[test]
    fn reasons_fold_to_short_tokens() {
        assert_eq!(sanitize_runner_reason("provider.free-limit").as_deref(), Some("provider.free-limit"));
        assert_eq!(sanitize_runner_reason(" permission ").as_deref(), Some("permission"));
        assert_eq!(sanitize_runner_reason("a b\nc=d").as_deref(), Some("a-b-c-d"));
        assert_eq!(sanitize_runner_reason("  "), None);
        assert_eq!(sanitize_runner_reason("\u{1b}[31m"), Some("31m".to_string()));
        let long = "x".repeat(200);
        assert_eq!(sanitize_runner_reason(&long).unwrap().len(), MAX_RUNNER_REASON_CHARS);
    }

    #[test]
    fn a_report_speaks_only_for_its_own_task() {
        let report = |task: Option<&str>| RunnerStateReport {
            state: RunnerState::Working,
            reason: None,
            until: None,
            task: task.map(str::to_string),
            at: Utc::now(),
        };
        assert!(report(None).is_for_task(Some("t-1")));
        assert!(report(None).is_for_task(None));
        assert!(report(Some("t-1")).is_for_task(Some("t-1")));
        assert!(!report(Some("t-1")).is_for_task(Some("t-2")));
        assert!(!report(Some("t-1")).is_for_task(None));
    }

    #[test]
    fn record_round_trips_and_clears() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        assert_eq!(read_runner_state("demo", "alpha"), None);
        let until = "2026-11-24T12:00:00Z".parse().unwrap();
        let report = RunnerStateReport {
            state: RunnerState::Paused,
            reason: Some("provider.rate-limit".into()),
            until: Some(until),
            task: Some("t-1".into()),
            at: Utc::now(),
        };
        write_runner_state("demo", "alpha", &report).unwrap();
        assert_eq!(read_runner_state("demo", "alpha"), Some(report));
        assert_eq!(read_runner_state("demo", "bravo"), None);
        let path = runner_state_path("demo", "alpha").unwrap();
        assert!(path.ends_with("workspaces/alpha/runner-state.json"));
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(body.contains(r#""state":"paused""#), "{body}");

        // A torn or foreign file reads as no report, never an error.
        std::fs::write(&path, "{not json").unwrap();
        assert_eq!(read_runner_state("demo", "alpha"), None);

        clear_runner_state("demo", "alpha").unwrap();
        clear_runner_state("demo", "alpha").unwrap();
        assert!(!path.exists());
        assert!(runner_state_path("demo", "../x").is_err());

        std::env::remove_var("SHELBI_HOME");
    }
}
