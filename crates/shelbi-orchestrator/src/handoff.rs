//! Orchestrator handoff — ask the live orchestrator pane to write its
//! in-flight state to `agents/orchestrator/handoff.md` before we kill
//! and respawn it (on `shelbi reload`) or tear it down (on
//! `shelbi quit`).
//!
//! The next orchestrator instance ingests the file via the
//! `deploy_agent_context` splice path (see
//! [`crate::workspace::deploy_agent_context`]) and deletes it after
//! reading — handoff is one-shot, not persistent state.
//!
//! Mechanism: we type a request into the orchestrator pane's tmux
//! `send-keys` input (the pane is running claude) and poll the
//! filesystem for the handoff file to appear. A 30s timeout caps the
//! wait — a missing handoff degrades to a cold start on the next
//! orchestrator, not a stuck reload/quit.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use shelbi_core::{Error, Result};

/// Hard cap on how long we'll block a reload/quit waiting for the
/// orchestrator to write its handoff. The orchestrator's response time
/// is dominated by claude's request roundtrip — usually a few seconds —
/// so 30s is generous without being so long that a wedged agent blocks
/// shutdown indefinitely. A missing handoff is degraded but not fatal,
/// so we'd rather time out and proceed than hang.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(30);

/// How often we re-check disk for the handoff file. Cheap enough at
/// 250ms that the worst-case extra latency over a fast write is well
/// under a second.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Outcome of [`request_orchestrator_handoff`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HandoffOutcome {
    /// Orchestrator wrote the file within the timeout. Caller should
    /// proceed; the next launch will splice and delete it.
    Written {
        /// Absolute path of the file the orchestrator just wrote.
        path: PathBuf,
    },
    /// The orchestrator pane isn't running — either it crashed earlier
    /// or it was never bootstrapped. Caller should skip the handoff
    /// step and proceed; the next launch reads whatever (possibly
    /// stale) handoff is already on disk or starts cold.
    PaneNotAlive,
    /// We asked but didn't see the file within [`HANDOFF_TIMEOUT`].
    /// Caller should proceed; the next orchestrator starts cold.
    Timeout,
    /// We couldn't send the request (e.g. tmux send-keys errored
    /// because the pane id disappeared between the alive check and
    /// the send). Caller should proceed; same degradation as Timeout.
    SendFailed { reason: String },
}

/// Ask the live orchestrator pane to write
/// `agents/orchestrator/handoff.md`, then poll for the file to appear
/// up to [`HANDOFF_TIMEOUT`].
///
/// Idempotent and best-effort: any stale handoff file from a previous
/// run is removed before the request goes out so we don't false-
/// positive on a leftover. The session-env lookup is the same
/// `SHELBI_PANE_orch` that `ensure_dashboard` pins at bootstrap, so
/// missing-env reads `PaneNotAlive` rather than guessing the pane by
/// position.
///
/// The caller (reload, quit_project, quit_shelbi) is responsible for
/// proceeding with the rest of its teardown regardless of which
/// outcome variant comes back — every variant of [`HandoffOutcome`] is
/// "okay to proceed" semantics, distinguished only for logging.
pub fn request_orchestrator_handoff(project_name: &str) -> Result<HandoffOutcome> {
    let session = format!("shelbi-{project_name}");
    if !local_session_exists(&session)? {
        return Ok(HandoffOutcome::PaneNotAlive);
    }
    let Some(pane_id) = read_orch_pane_id(&session)? else {
        return Ok(HandoffOutcome::PaneNotAlive);
    };
    if !pane_alive(&pane_id)? {
        return Ok(HandoffOutcome::PaneNotAlive);
    }

    let handoff_path = shelbi_state::orchestrator_handoff_path(project_name)?;
    // Sweep any stale handoff so the poll below only ever sees a fresh
    // write from the request we're about to send. Best-effort — a
    // missing file is fine; a permissions error is rare and the caller
    // would degrade to a stale-handoff ingest, which is the documented
    // fallback anyway.
    let _ = std::fs::remove_file(&handoff_path);

    let request = handoff_request_message();
    if let Err(reason) = send_to_pane(&pane_id, &request) {
        return Ok(HandoffOutcome::SendFailed { reason });
    }

    let deadline = Instant::now() + HANDOFF_TIMEOUT;
    while Instant::now() < deadline {
        if handoff_path.exists() {
            return Ok(HandoffOutcome::Written { path: handoff_path });
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    Ok(HandoffOutcome::Timeout)
}

/// Text we type into the orchestrator's input. Phrased so the agent
/// knows exactly what to do without re-reading its instructions —
/// the prompt parts that the orchestrator's bundled `instructions.md`
/// `## Handoff` section also references are mirrored here so a user
/// who customized that section still gets a sensible request.
fn handoff_request_message() -> String {
    format!(
        "[shelbi handoff request] The orchestrator pane is about to be \
         restarted. Per the `## Handoff` section of your instructions, \
         write `{rel}` (relative to your workdir) covering in-flight \
         decisions, what you're watching for, recent context the next \
         instance should know, and anything the user asked but you \
         haven't fully answered. Free-form prose, markdown. Don't do \
         anything else — once the file lands, this pane will be torn \
         down.",
        rel = shelbi_state::ORCHESTRATOR_HANDOFF_REL,
    )
}

/// `tmux has-session -t <name>` on the local server.
fn local_session_exists(session: &str) -> Result<bool> {
    let out = std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .output()
        .map_err(Error::Io)?;
    Ok(out.status.success())
}

/// Read `SHELBI_PANE_orch` from the session's tmux environment. Returns
/// `None` when the var is unset (older session before
/// `ensure_dashboard` pinned it) or empty.
fn read_orch_pane_id(session: &str) -> Result<Option<String>> {
    let out = std::process::Command::new("tmux")
        .args(["show-environment", "-t", session, "SHELBI_PANE_orch"])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(None);
    }
    let line = String::from_utf8_lossy(&out.stdout);
    let line = line.trim();
    if line.starts_with('-') {
        return Ok(None);
    }
    let Some((_, value)) = line.split_once('=') else {
        return Ok(None);
    };
    if value.is_empty() {
        Ok(None)
    } else {
        Ok(Some(value.to_string()))
    }
}

/// `tmux list-panes -a -F #{pane_id}` — true when the given pane id
/// shows up in the live pane list. Catches the case where the
/// orchestrator pane crashed (or was manually killed) after
/// `SHELBI_PANE_orch` was set but before we asked.
fn pane_alive(pane_id: &str) -> Result<bool> {
    let out = std::process::Command::new("tmux")
        .args(["list-panes", "-a", "-F", "#{pane_id}"])
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|l| l.trim() == pane_id))
}

/// Stage `text` through tmux's paste-buffer + paste it to the pane,
/// then send `Enter` to submit. Mirrors `shelbi_tmux::send_line`'s
/// multi-line path so the message lands as one atomic paste — the
/// claude UI's heuristic paste-detection bundles it into one user
/// turn instead of splitting on intra-message Enters.
fn send_to_pane(pane_id: &str, text: &str) -> std::result::Result<(), String> {
    const BUFFER: &str = "shelbi-handoff";
    // load-buffer reads from stdin so embedded whitespace and shell
    // metacharacters don't get re-parsed by argv joining.
    let mut child = std::process::Command::new("tmux")
        .args(["load-buffer", "-b", BUFFER, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("tmux load-buffer spawn: {e}"))?;
    {
        use std::io::Write;
        let stdin = child
            .stdin
            .as_mut()
            .ok_or_else(|| "tmux load-buffer: failed to open stdin".to_string())?;
        stdin
            .write_all(text.as_bytes())
            .map_err(|e| format!("tmux load-buffer write: {e}"))?;
    }
    let load = child
        .wait_with_output()
        .map_err(|e| format!("tmux load-buffer wait: {e}"))?;
    if !load.status.success() {
        return Err(format!(
            "tmux load-buffer failed: {}",
            String::from_utf8_lossy(&load.stderr).trim()
        ));
    }

    let paste = std::process::Command::new("tmux")
        .args(["paste-buffer", "-p", "-d", "-b", BUFFER, "-t", pane_id])
        .output()
        .map_err(|e| format!("tmux paste-buffer: {e}"))?;
    if !paste.status.success() {
        return Err(format!(
            "tmux paste-buffer failed: {}",
            String::from_utf8_lossy(&paste.stderr).trim()
        ));
    }
    let enter = std::process::Command::new("tmux")
        .args(["send-keys", "-t", pane_id, "Enter"])
        .output()
        .map_err(|e| format!("tmux send-keys Enter: {e}"))?;
    if !enter.status.success() {
        return Err(format!(
            "tmux send-keys Enter failed: {}",
            String::from_utf8_lossy(&enter.stderr).trim()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handoff_request_message_names_the_relative_path() {
        // Acceptance criterion: the request must point the orchestrator
        // at `agents/orchestrator/handoff.md` so a user who customized
        // their instructions still gets a sensible ask.
        let msg = handoff_request_message();
        assert!(
            msg.contains("agents/orchestrator/handoff.md"),
            "request missing handoff path: {msg}"
        );
        // And mentions the section name so the agent knows which prompt
        // policy to follow if there's any ambiguity.
        assert!(msg.contains("Handoff"), "request missing section: {msg}");
    }

    #[test]
    fn handoff_outcome_variants_distinguish_proceed_reasons() {
        // No semantic assertions here — the variants are an enum we
        // only ever match on for logging. This test just locks in
        // PartialEq so the call sites can match cleanly.
        let written = HandoffOutcome::Written {
            path: PathBuf::from("/tmp/handoff.md"),
        };
        assert_eq!(written, written.clone());
        assert_ne!(written, HandoffOutcome::Timeout);
        assert_ne!(HandoffOutcome::PaneNotAlive, HandoffOutcome::Timeout);
    }
}
