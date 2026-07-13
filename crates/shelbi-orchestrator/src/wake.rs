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

use shelbi_core::{Project, StatusCategory};

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
    if !should_submit(state, pending, baseline.is_actively_busy()) {
        return;
    }

    // Re-check idleness at the shared submit guard. A user can start a turn
    // between the baseline capture and delivery; in that race we defer rather
    // than type into the active turn's queue.
    let may_submit = || !PaneBaseline::capture(&host, &addr, profile).is_actively_busy();
    match crate::submit::send_verified_guarded(&host, &addr, WAKE_PROMPT, &baseline, may_submit) {
        Ok(SubmitStatus::Submitted { .. }) => state.woken_through = pending.through,
        Ok(status) => tracing::warn!(
            project = %project.name,
            ?status,
            "Codex event wake was not submitted; retrying when idle",
        ),
        Err(error) => tracing::warn!(
            project = %project.name,
            %error,
            "Codex event wake delivery failed; retrying when idle",
        ),
    }
    // Every other result remains pending. The next supervisor tick retries a
    // failed delivery, or waits for an active turn to become idle.
}

fn should_submit(state: &CodexWakeState, pending: PendingWake, active: bool) -> bool {
    !active && pending.through > state.woken_through
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
        let zen_actionable = fields.get("zen").is_some_and(|zen| *zen == "on");
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
        if board_in_flight || zen_actionable || capacity_actionable {
            return Some(WakePriority::Heartbeat);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let line = "t project=demo heartbeat zen_eligible=0 idle_workspaces=2";
        assert_eq!(line_priority(line, "demo", false), None);
        assert_eq!(
            line_priority(line, "demo", true),
            Some(WakePriority::Heartbeat)
        );
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
    fn idle_codex_with_pending_event_is_eligible_for_wake() {
        let state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        assert!(should_submit(&state, pending, false));
    }

    #[test]
    fn active_codex_turn_defers_pending_wake() {
        let state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        assert!(!should_submit(&state, pending, true));
    }

    #[test]
    fn failed_submit_stays_retryable() {
        let state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        // No successful result advanced the watermark, so the next tick retries.
        assert!(should_submit(&state, pending, false));
    }

    #[test]
    fn successful_wake_position_deduplicates_until_cursor_drain() {
        let mut state = CodexWakeState::default();
        let pending = PendingWake { through: 42 };
        state.woken_through = pending.through;
        assert!(!should_submit(&state, pending, false));
    }
}
