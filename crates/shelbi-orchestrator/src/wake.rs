//! Autonomous wake scheduling for polling-only Codex orchestrators.
//!
//! The durable event cursor remains the delivery ledger. This module only
//! notices that actionable project events exist beyond that cursor and asks an
//! idle Codex pane to drain them. It never advances or embeds a second parsed
//! batch, so the orchestrator's normal `events drain` turn applies each event
//! exactly once.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom};

use shelbi_core::{Error, Project, StatusCategory};

use crate::submit::{PaneBaseline, SubmitProfile, SubmitStatus};

const WAKE_PROMPT: &str = "[shelbi board wake] Project events are pending. Run `shelbi orchestrator events drain` now, apply every returned fact through your normal reaction rules in priority order, then continue autonomous scheduling. Do not rely on the background tail as the event batch; the durable cursor is authoritative.";

#[derive(Debug, Default)]
pub struct CodexWakeState {
    /// Highest log position for which a wake prompt was successfully
    /// submitted. It suppresses duplicate turns until the durable drain cursor
    /// catches up; newly appended events beyond it can still raise a later wake.
    woken_through: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum WakePriority {
    Heartbeat,
    WorkspaceFree,
    Ready,
    Handoff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingWake {
    through: u64,
}

/// One supervisor tick. Claude retains its asynchronous Monitor path and
/// custom runners retain their existing contracts; only a configured Codex
/// orchestrator is eligible for pane injection.
pub fn maybe_wake_codex(project: &Project, state: &mut CodexWakeState) {
    let Some(runner) = project.runner(&project.orchestrator.runner) else {
        return;
    };
    if !shelbi_agent::is_codex_runner(&runner.command) {
        return;
    }

    let pending = match scan_pending(project) {
        Ok(Some(pending)) => pending,
        Ok(None) => return,
        Err(error) => {
            tracing::debug!(project = %project.name, %error, "Codex wake scan failed");
            return;
        }
    };
    if pending.through < state.woken_through {
        // events.log rotated beneath the durable cursor; positions now refer
        // to the fresh file and the old in-memory watermark is meaningless.
        state.woken_through = 0;
    }
    if pending.through <= state.woken_through {
        return;
    }

    let Ok(Some((host, addr))) = crate::orchestrator_pane_addr(&project.name) else {
        return;
    };
    let profile = SubmitProfile::CodexUi;
    let baseline = PaneBaseline::capture(&host, &addr, profile);
    if !should_submit(state, pending, baseline.is_codex_wake_ready()) {
        return;
    }

    // Re-check the live composer at the shared submit guard immediately before
    // text delivery. A user can start typing or submit a turn after the
    // baseline capture; either race revokes delivery without touching tmux.
    let may_deliver = || crate::submit::codex_wake_ready(&host, &addr);
    // Once our text is parked, the composer is no longer empty. Preserve the
    // verifier's one dropped-Enter retry, but require a fresh capture showing
    // exactly our prompt so appended user text can never be submitted with it.
    let may_retry_enter = || crate::submit::codex_wake_retry_ready(&host, &addr, WAKE_PROMPT);
    match crate::submit::send_verified_guarded_with_guards(
        &host,
        &addr,
        WAKE_PROMPT,
        &baseline,
        may_deliver,
        may_retry_enter,
    ) {
        Ok(SubmitStatus::Submitted { .. }) => state.woken_through = pending.through,
        Ok(status) => tracing::warn!(
            project = %project.name,
            ?status,
            "Codex event wake was not submitted; retrying when idle",
        ),
        Err(error) => {
            let error = wake_delivery_error_summary(&error);
            tracing::warn!(
                project = %project.name,
                target_kind = addr.target_kind(),
                target = %addr.target(),
                %error,
                "Codex event wake delivery failed; retrying when idle",
            );
        }
    }
    // Every other result remains pending. The next supervisor tick retries a
    // failed delivery, or waits for an active turn to become idle.
}

/// A wake-safe error summary. `shelbi_ssh::run_capture` includes the complete
/// argv in `Error::Command`, and the local literal-send argv contains the wake
/// prompt. Keep tmux's status and stderr while deliberately omitting that
/// command field; the caller logs the target kind/value separately.
fn wake_delivery_error_summary(error: &Error) -> String {
    match error {
        Error::Command { status, stderr, .. } => {
            format!("status={status}; stderr={}", stderr.trim())
        }
        other => other.to_string(),
    }
}

fn should_submit(state: &CodexWakeState, pending: PendingWake, pane_ready: bool) -> bool {
    pane_ready && pending.through > state.woken_through
}

fn scan_pending(project: &Project) -> shelbi_core::Result<Option<PendingWake>> {
    let cursor_path = shelbi_state::project_dir(&project.name)?.join("event-cursor");
    let cursor = match fs::read_to_string(cursor_path) {
        Ok(text) => text.trim().parse().unwrap_or(0),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => 0,
        Err(e) => return Err(e.into()),
    };
    let path = shelbi_state::events_log_path()?;
    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let len = file.metadata()?.len();
    let start = if cursor > len { 0 } else { cursor };
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start) as usize);
    file.read_to_end(&mut bytes)?;
    let complete_len = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if complete_len == 0 {
        return Ok(None);
    }
    let tasks = shelbi_state::list_tasks(&project.name)?;
    let board_in_flight = tasks.iter().any(|task| {
        matches!(
            task.task.column.category(),
            StatusCategory::Ready | StatusCategory::Active | StatusCategory::Handoff
        )
    });
    let text = String::from_utf8_lossy(&bytes[..complete_len]);
    Ok(scan_text(&text, start, &project.name, board_in_flight))
}

fn scan_text(text: &str, start: u64, project: &str, board_in_flight: bool) -> Option<PendingWake> {
    let mut actionable_through = None;
    let mut priority: Option<WakePriority> = None;
    let mut offset = start;
    for line in text.split_inclusive('\n') {
        offset += line.len() as u64;
        if let Some(line_priority) = line_priority(
            line.trim_end_matches(['\r', '\n']),
            project,
            board_in_flight,
        ) {
            actionable_through = Some(offset);
            priority = Some(priority.map_or(line_priority, |old| old.max(line_priority)));
        }
    }
    // Computing the maximum is intentional even though the wake prompt carries
    // no parsed event data: it proves the burst contains an actionable class,
    // while the prompt tells Codex to drain and apply the whole durable batch
    // in priority order. No lower-priority line can mask a handoff reaction.
    priority.map(|_highest_priority| PendingWake {
        through: actionable_through.expect("priority requires an actionable line"),
    })
}

fn line_priority(
    line: &str,
    project: &str,
    board_in_flight: bool,
) -> Option<WakePriority> {
    let fields = line
        .split_whitespace()
        .filter_map(|part| part.split_once('='))
        .collect::<HashMap<_, _>>();
    // Wake scheduling is stricter than durable drain's legacy compatibility:
    // only explicitly scoped modern records may inject a pane prompt. A
    // project-less historical task id can collide across projects and must
    // never become a cross-project wake-up.
    if fields.get("project").copied() != Some(project) {
        return None;
    }

    match fields.get("to_category").copied() {
        Some("handoff") => return Some(WakePriority::Handoff),
        Some("ready") => return Some(WakePriority::Ready),
        _ => {}
    }
    let words = line.split_whitespace().collect::<Vec<_>>();
    if fields.contains_key("workspace")
        && words
            .windows(2)
            .any(|window| window == ["->", "awaiting_input"] || window == ["->", "idle"])
    {
        return Some(WakePriority::WorkspaceFree);
    }
    if words.contains(&"heartbeat") {
        let capacity_actionable = fields
            .get("zen_eligible")
            .and_then(|n| n.parse::<usize>().ok())
            .unwrap_or(0)
            > 0
            && fields
                .get("idle_workspaces")
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0)
                > 0;
        if board_in_flight || capacity_actionable {
            return Some(WakePriority::Heartbeat);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delivery_diagnostic_keeps_tmux_context_but_redacts_prompt_argv() {
        let secret = "private wake prompt body";
        let error = Error::Command {
            cmd: format!("tmux send-keys -t =%99 -l -- {secret}"),
            status: "exit status: 1".into(),
            stderr: "can't find pane: %99\n".into(),
        };
        let summary = wake_delivery_error_summary(&error);
        assert!(summary.contains("exit status: 1"));
        assert!(summary.contains("can't find pane: %99"));
        assert!(!summary.contains(secret));
        assert!(!summary.contains("send-keys"));
    }

    #[test]
    fn scopes_events_and_coalesces_to_highest_priority() {
        assert_eq!(
            line_priority(
                "t project=demo heartbeat zen_eligible=1 idle_workspaces=1",
                "demo",
                false,
            ),
            Some(WakePriority::Heartbeat)
        );
        assert_eq!(
            line_priority(
                "t project=demo task=ours x -> review to_category=handoff",
                "demo",
                false,
            ),
            Some(WakePriority::Handoff)
        );
        assert_eq!(
            line_priority(
                "t project=other task=ours x -> review to_category=handoff",
                "demo",
                true,
            ),
            None
        );
        assert_eq!(
            line_priority(
                "t task=ours x -> review to_category=handoff",
                "demo",
                true,
            ),
            None,
            "project-less legacy lines must not inject a cross-project wake"
        );
    }

    #[test]
    fn quiet_heartbeat_is_not_actionable_but_inflight_heartbeat_is() {
        let line = "t project=demo heartbeat zen=on zen_eligible=0 idle_workspaces=9";
        assert_eq!(line_priority(line, "demo", false), None);
        assert_eq!(
            line_priority(line, "demo", true),
            Some(WakePriority::Heartbeat)
        );
    }

    #[test]
    fn heartbeat_requires_both_eligible_work_and_idle_capacity() {
        assert_eq!(
            line_priority(
                "t project=demo heartbeat zen=on zen_eligible=2 idle_workspaces=1",
                "demo",
                false,
            ),
            Some(WakePriority::Heartbeat)
        );
        for line in [
            "t project=demo heartbeat zen=on zen_eligible=2 idle_workspaces=0",
            "t project=demo heartbeat zen=on zen_eligible=0 idle_workspaces=9",
            "t project=demo heartbeat zen=on zen_eligible=nope idle_workspaces=9",
            "t project=demo heartbeat zen=on",
        ] {
            assert_eq!(line_priority(line, "demo", false), None, "line: {line}");
        }
    }

    #[test]
    fn transition_wakes_remain_actionable() {
        assert_eq!(
            line_priority(
                "t project=demo task=ours backlog -> queued to_category=ready",
                "demo",
                false,
            ),
            Some(WakePriority::Ready)
        );
        assert_eq!(
            line_priority(
                "t project=demo task=ours active -> review to_category=handoff",
                "demo",
                false,
            ),
            Some(WakePriority::Handoff)
        );
        for state in ["awaiting_input", "idle"] {
            let line = format!("t project=demo workspace=alpha working -> {state}");
            assert_eq!(
                line_priority(&line, "demo", false),
                Some(WakePriority::WorkspaceFree),
                "state: {state}"
            );
        }
    }

    #[test]
    fn foreign_lines_do_not_advance_the_coalescing_watermark() {
        let owned = "t project=demo task=ours x -> todo to_category=ready\n";
        let foreign = "t project=other task=theirs x -> review to_category=handoff\n";
        let pending = scan_text(&format!("{owned}{foreign}"), 0, "demo", false).unwrap();
        assert_eq!(pending.through, owned.len() as u64);
    }

    #[test]
    fn bursty_owned_events_coalesce_to_one_latest_wake_position() {
        let first = "t project=demo heartbeat zen=on zen_eligible=1 idle_workspaces=1\n";
        let second = "t project=demo task=ours x -> review to_category=handoff\n";
        let pending = scan_text(&format!("{first}{second}"), 7, "demo", false).unwrap();
        assert_eq!(pending.through, 7 + first.len() as u64 + second.len() as u64);
    }

    #[test]
    fn quiet_heartbeat_after_actionable_event_does_not_extend_wake_position() {
        let actionable = "t project=demo task=ours x -> todo to_category=ready\n";
        let quiet = "t project=demo heartbeat zen=on zen_eligible=0 idle_workspaces=9\n";
        let pending = scan_text(&format!("{actionable}{quiet}"), 11, "demo", false).unwrap();
        assert_eq!(pending.through, 11 + actionable.len() as u64);
    }

    #[test]
    fn idle_codex_with_pending_event_is_eligible_for_wake() {
        let state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        assert!(should_submit(&state, pending, true));
    }

    #[test]
    fn active_codex_turn_defers_pending_wake() {
        let state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        assert!(!should_submit(&state, pending, false));
    }

    #[test]
    fn failed_submit_stays_retryable() {
        let state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        // No successful result advanced the watermark, so the next tick retries.
        assert!(should_submit(&state, pending, true));
    }

    #[test]
    fn successful_wake_position_deduplicates_until_cursor_drain() {
        let mut state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        state.woken_through = pending.through;
        assert!(!should_submit(&state, pending, true));
    }

    #[test]
    fn actionable_event_starts_a_turn_in_the_stable_orchestrator_pane() {
        use shelbi_core::{
            AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind,
            OrchestratorSpec, ZenConfig,
        };
        use std::collections::BTreeMap;
        use std::ffi::OsString;
        use std::path::PathBuf;
        use std::process::Command;
        use std::time::{Duration, Instant};

        let _test_lock = crate::test_lock::acquire();
        match Command::new("tmux").arg("-V").output() {
            Ok(output) => assert!(
                output.status.success(),
                "tmux -V failed: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => panic!("could not probe tmux: {error}"),
        }
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");

        struct RootEnvGuard {
            root: Option<OsString>,
            home: Option<OsString>,
        }
        impl RootEnvGuard {
            fn install(home: &std::path::Path) -> Self {
                let guard = Self {
                    root: std::env::var_os("SHELBI_ROOT"),
                    home: std::env::var_os("SHELBI_HOME"),
                };
                std::env::remove_var("SHELBI_ROOT");
                std::env::set_var("SHELBI_HOME", home);
                guard
            }
        }
        impl Drop for RootEnvGuard {
            fn drop(&mut self) {
                match &self.root {
                    Some(value) => std::env::set_var("SHELBI_ROOT", value),
                    None => std::env::remove_var("SHELBI_ROOT"),
                }
                match &self.home {
                    Some(value) => std::env::set_var("SHELBI_HOME", value),
                    None => std::env::remove_var("SHELBI_HOME"),
                }
            }
        }
        let _env = RootEnvGuard::install(&home);

        let project_name = format!("wake-pane-{}", std::process::id());
        let session = format!("shelbi-{project_name}");
        let session_target = format!("={session}");
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &session_target])
            .output();

        let script = tmp.path().join("fake-codex.sh");
        let receipt = tmp.path().join("wake-receipt");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
             stty -echo\n\
             printf '\\033[2J\\033[H› Explain this codebase\\n\\n  ? for shortcuts\\n'\n\
             IFS= read -r line\n\
             printf '%s\\n' \"$line\" > \"$1\"\n\
             printf '\\033[2J\\033[H• Working (1s)\\n  esc to interrupt\\n'\n\
             sleep 5\n",
        )
        .unwrap();

        let mut started = false;
        let mut start_error = String::new();
        for _ in 0..50 {
            match Command::new("tmux")
                .args([
                    "new-session",
                    "-d",
                    "-s",
                    &session,
                    "-n",
                    "dashboard",
                    "sh",
                    script.to_str().unwrap(),
                    receipt.to_str().unwrap(),
                ])
                .output()
            {
                Ok(output) if !output.status.success() => {
                    start_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
                }
                Err(error) => start_error = error.to_string(),
                Ok(_) => {}
            }
            let live = Command::new("tmux")
                .args(["has-session", "-t", &session_target])
                .output();
            started = match live {
                Ok(output) if output.status.success() => true,
                Ok(output) => {
                    if start_error.is_empty() {
                        start_error = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    }
                    false
                }
                Err(error) => {
                    start_error = error.to_string();
                    false
                }
            };
            if started {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(started, "tmux session never came up: {start_error}");

        struct SessionGuard(String);
        impl Drop for SessionGuard {
            fn drop(&mut self) {
                let _ = Command::new("tmux")
                    .args(["kill-session", "-t", &format!("={}", self.0)])
                    .output();
            }
        }
        let _session = SessionGuard(session.clone());

        let pane = Command::new("tmux")
            .args([
                "list-panes",
                "-t",
                &session_target,
                "-F",
                "#{pane_id}",
            ])
            .output()
            .unwrap();
        assert!(pane.status.success());
        let pane_id = String::from_utf8_lossy(&pane.stdout).trim().to_string();
        assert!(pane_id.starts_with('%'), "unexpected pane id: {pane_id}");
        let pinned = Command::new("tmux")
            .args([
                "set-environment",
                "-t",
                &session_target,
                "SHELBI_PANE_orch",
                &pane_id,
            ])
            .status()
            .unwrap();
        assert!(pinned.success());

        let ready_deadline = Instant::now() + Duration::from_secs(2);
        let mut screen = String::new();
        while Instant::now() < ready_deadline {
            let output = Command::new("tmux")
                .args(["capture-pane", "-p", "-t", &pane_id])
                .output()
                .unwrap();
            screen = String::from_utf8_lossy(&output.stdout).into_owned();
            if screen.contains("Explain this codebase") {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            screen.contains("Explain this codebase"),
            "fixture never became idle: {screen}"
        );

        let mut runners = BTreeMap::new();
        runners.insert(
            "codex".into(),
            AgentRunnerSpec {
                command: "codex".into(),
                flags: Vec::new(),
                prompt_injection: None,
                dialog_signatures: Vec::new(),
            },
        );
        let project = Project {
            name: project_name.clone(),
            repo: tmp.path().to_string_lossy().into_owned(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: PathBuf::from(tmp.path()),
                host: None,
                tags: Vec::new(),
                forward: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "codex".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        std::fs::write(
            shelbi_state::events_log_path().unwrap(),
            format!(
                "2026-07-13T12:00:00Z project={project_name} task=ready x -> review to_category=handoff\n"
            ),
        )
        .unwrap();

        let resolved = crate::orchestrator_pane_addr(&project_name)
            .unwrap()
            .expect("pinned orchestrator pane should resolve");
        assert!(resolved.1.is_pane_id());
        assert_eq!(resolved.1.target(), pane_id);

        let mut state = CodexWakeState::default();
        maybe_wake_codex(&project, &mut state);
        assert!(state.woken_through > 0, "wake was not verified as submitted");
        assert_eq!(std::fs::read_to_string(&receipt).unwrap().trim(), WAKE_PROMPT);

        let woken_through = state.woken_through;
        maybe_wake_codex(&project, &mut state);
        assert_eq!(
            state.woken_through, woken_through,
            "the same durable position should not schedule a duplicate wake"
        );
    }
}
