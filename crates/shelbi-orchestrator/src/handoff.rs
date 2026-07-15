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
//! Mechanism: for Claude, custom runners, and a one-time migration from a
//! standalone Codex pane, we type a request into the orchestrator pane and
//! poll the filesystem for the handoff file. Once Codex has a persisted
//! native thread, that thread is the handoff only while the configured runner
//! remains Codex; switching to a non-Codex runner instead runs a guided
//! migration before any pane mutation. Shelbi cannot serialize the native
//! thread through the composer, so it archives the thread marker (leaving the
//! id recoverable) and proceeds with the file-based handoff — but only after
//! confirming the durable event queue has no undelivered actionable batches.
//! If batches are still pending it refuses and names them, so no board events
//! are silently dropped. A 30s timeout caps legacy handoff waits.

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
    /// Codex continuity is owned by its persisted app-server thread. Shelbi
    /// must not paste a handoff request into the visible remote-TUI composer.
    NativeThread,
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
/// A project-matching persisted native Codex thread returns
/// [`HandoffOutcome::NativeThread`] before any tmux lookup or write. A Codex
/// pane without that state is an old standalone pane and gets the existing
/// one-time best-effort handoff so an install/reload can migrate its context.
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
    // The native thread is the handoff only for a same-runner Codex reload.
    // A Codex -> Claude/custom switch cannot use composer transport, so the
    // shared preflight instead archives the thread marker (once the durable
    // queue is drained) and returns `false`, letting the caller fall through
    // to the file-based handoff. A still-pending queue surfaces here as a hard
    // error so the caller aborts before tearing down the pane.
    if validate_configured_orchestrator_transition(project_name)? {
        return Ok(HandoffOutcome::NativeThread);
    }

    let project = shelbi_state::load_project(project_name)?;
    let _runner = project
        .runner(&project.orchestrator.runner)
        .ok_or_else(|| Error::UnknownRunner(project.orchestrator.runner.clone()))?;

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

/// Validate continuity between the live orchestrator and the runner currently
/// selected in project configuration.
///
/// Returns `true` when an active native Codex thread supplies same-runner
/// continuity, and `false` for legacy/standalone panes that use the file-based
/// handoff. Switching from a native Codex thread to a non-Codex runner is
/// migrated in place: the thread marker is archived (once the durable queue is
/// drained) so the switch proceeds, otherwise the switch is refused with the
/// pending delivery ids and the marker left intact.
pub(crate) fn validate_configured_orchestrator_transition(project_name: &str) -> Result<bool> {
    let project = shelbi_state::load_project(project_name)?;
    let runner = project
        .runner(&project.orchestrator.runner)
        .ok_or_else(|| Error::UnknownRunner(project.orchestrator.runner.clone()))?;
    validate_orchestrator_runner_transition(
        project_name,
        &project.orchestrator.runner,
        &runner.command,
    )
}

/// Validate the exact runner already selected for an imminent launch.
///
/// Callers must pass the captured runner rather than reloading configuration:
/// otherwise a concurrent config edit could validate Codex while stale
/// non-Codex argv is about to be started.
pub(crate) fn validate_orchestrator_runner_transition(
    project_name: &str,
    runner_name: &str,
    runner_command: &str,
) -> Result<bool> {
    let native_active = crate::wake::has_persisted_codex_thread(project_name)?;
    // A stale marker (`native_active: false`) is invisible to
    // `has_persisted_codex_thread` but still needs archiving on a switch, so
    // probe the file directly when the active check comes back negative.
    let thread_file_present =
        native_active || crate::wake::persisted_codex_thread_file_exists(project_name)?;
    match classify_runner_transition(thread_file_present, native_active, runner_command) {
        RunnerTransition::NativeContinuity => Ok(true),
        RunnerTransition::LegacyProceed => Ok(false),
        RunnerTransition::MigrateToLegacy => {
            migrate_native_thread_to_legacy(project_name, runner_name, runner_command)?;
            Ok(false)
        }
    }
}

/// Decision for an imminent orchestrator launch, given the persisted Codex
/// thread state and the runner argv about to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RunnerTransition {
    /// Same-runner Codex reload: the live native thread is the handoff.
    NativeContinuity,
    /// Legacy/cold path with no native thread marker to migrate.
    LegacyProceed,
    /// Native thread marker (active, or stale with `native_active: false`) with
    /// a non-Codex target: archive the marker after the durable-queue drain
    /// check, then proceed with the file-based handoff.
    MigrateToLegacy,
}

fn classify_runner_transition(
    thread_file_present: bool,
    native_active: bool,
    runner_command: &str,
) -> RunnerTransition {
    if shelbi_agent::RunnerAdapter::for_command(runner_command).is_codex() {
        // Codex target: an active thread carries same-runner continuity; an
        // inactive or missing marker is a fresh/cold start that
        // `open_owned_thread` handles without our help.
        return if native_active {
            RunnerTransition::NativeContinuity
        } else {
            RunnerTransition::LegacyProceed
        };
    }
    // Non-Codex target: any thread marker must be archived before the switch,
    // including a stale one — leaving it would let a later Codex reload resume a
    // thread the user has moved on from.
    if thread_file_present {
        RunnerTransition::MigrateToLegacy
    } else {
        RunnerTransition::LegacyProceed
    }
}

/// Guided native-to-legacy migration. Refuses only when the durable queue still
/// holds undelivered actionable batches — naming them so the user can restore
/// the Codex runner, drain them, and retry — otherwise archives the thread
/// marker so the id stays recoverable and lets the switch proceed.
fn migrate_native_thread_to_legacy(
    project_name: &str,
    runner_name: &str,
    runner_command: &str,
) -> Result<()> {
    let pending = crate::wake::pending_codex_delivery_ids(project_name)?;
    if !pending.is_empty() {
        return Err(Error::Other(format!(
            "cannot switch project `{project_name}` to orchestrator runner \
             `{runner_name}` (`{runner_command}`): the Codex event queue still holds \
             {count} undelivered batch(es) [{ids}]. Restore the Codex runner and let \
             the orchestrator drain them (each batch leaves the queue once its events \
             are applied), then retry the switch. The native thread was left intact.",
            count = pending.len(),
            ids = pending.join(", "),
        )));
    }
    if let Some(archived) = crate::wake::archive_persisted_codex_thread(project_name)? {
        tracing::info!(
            project = project_name,
            archived = %archived.display(),
            "archived native Codex thread marker for runner switch"
        );
    }
    Ok(())
}

/// Whether a persisted, active native thread supplies same-runner continuity.
/// Retained as a named predicate for the wake-module tests that assert the
/// migration boundary; the launch guards now route through
/// [`classify_runner_transition`].
#[cfg(test)]
pub(crate) fn uses_native_thread_continuity(persisted_native_thread: bool) -> bool {
    persisted_native_thread
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
    let target = shelbi_tmux::session_target(session);
    let out = std::process::Command::new("tmux")
        .args(["has-session", "-t", &target])
        .output()
        .map_err(Error::Io)?;
    Ok(out.status.success())
}

/// Read `SHELBI_PANE_orch` from the session's tmux environment. Returns
/// `None` when the var is unset (older session before
/// `ensure_dashboard` pinned it) or empty.
fn read_orch_pane_id(session: &str) -> Result<Option<String>> {
    let target = shelbi_tmux::session_target(session);
    let out = std::process::Command::new("tmux")
        .args([
            "show-environment",
            "-t",
            &target,
            "SHELBI_PANE_orch",
        ])
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
    use std::fs;

    fn project_with_runner(name: &str, command: &str) -> shelbi_core::Project {
        shelbi_core::Project {
            name: "demo".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            orchestrator: shelbi_core::OrchestratorSpec {
                runner: name.into(),
            },
            agent_runners: std::collections::BTreeMap::from([(
                name.into(),
                shelbi_core::AgentRunnerSpec {
                    command: command.into(),
                    flags: vec![],
                    prompt_injection: None,
                    dialog_signatures: vec![],
                    integration: None,
                },
            )]),
            github_url: None,
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            repo: "/tmp/demo".into(),
            machines: Vec::new(),
            editor: None,
            workspaces: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

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
        assert_ne!(HandoffOutcome::NativeThread, HandoffOutcome::Timeout);
    }

    #[test]
    fn classify_routes_each_runner_transition() {
        // Active native thread + a Codex target is same-runner continuity.
        assert_eq!(
            classify_runner_transition(true, true, "codex"),
            RunnerTransition::NativeContinuity
        );
        assert_eq!(
            classify_runner_transition(true, true, "/opt/codex/bin/codex"),
            RunnerTransition::NativeContinuity
        );
        // Active native thread + a non-Codex target migrates instead of erroring.
        assert_eq!(
            classify_runner_transition(true, true, "claude"),
            RunnerTransition::MigrateToLegacy
        );
        // A stale marker (present but inactive) still gets archived on a switch.
        assert_eq!(
            classify_runner_transition(true, false, "claude"),
            RunnerTransition::MigrateToLegacy
        );
        // No marker at all: nothing to migrate on either target.
        assert_eq!(
            classify_runner_transition(false, false, "claude"),
            RunnerTransition::LegacyProceed
        );
        // Inactive/missing marker + Codex target is a fresh cold start.
        assert_eq!(
            classify_runner_transition(false, false, "codex"),
            RunnerTransition::LegacyProceed
        );
    }

    /// Install `SHELBI_HOME` for the duration of a closure, restoring the prior
    /// value afterward. Serialized by `test_lock` because it mutates env.
    fn with_temp_home(body: impl FnOnce(&std::path::Path)) {
        let _lock = crate::test_lock::acquire();
        let previous_home = std::env::var_os("SHELBI_HOME");
        let temp = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", temp.path());
        body(temp.path());
        match previous_home {
            Some(home) => std::env::set_var("SHELBI_HOME", home),
            None => std::env::remove_var("SHELBI_HOME"),
        }
    }

    fn write_thread_marker(native_active: bool) -> PathBuf {
        let project_dir = shelbi_state::project_dir("demo").unwrap();
        fs::create_dir_all(&project_dir).unwrap();
        let state_path = project_dir.join("codex-thread.json");
        fs::write(
            &state_path,
            format!(
                r#"{{"version":1,"project":"demo","thread_id":"thread-owned","bootstrap_generation":3,"native_active":{native_active}}}"#,
            ),
        )
        .unwrap();
        state_path
    }

    fn archived_marker(project_dir: &std::path::Path) -> Option<PathBuf> {
        fs::read_dir(project_dir)
            .unwrap()
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .find(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("codex-thread.json.archived-"))
            })
    }

    #[test]
    fn active_native_switch_with_drained_queue_archives_and_proceeds() {
        with_temp_home(|_| {
            shelbi_state::save_project(&project_with_runner("codex", "codex")).unwrap();
            let state_path = write_thread_marker(true);
            let project_dir = state_path.parent().unwrap().to_path_buf();

            // Same-runner reload is still native continuity, untouched.
            assert_eq!(
                request_orchestrator_handoff("demo").unwrap(),
                HandoffOutcome::NativeThread,
            );

            // Switch to Claude: no pending queue, so it migrates and proceeds.
            shelbi_state::save_project(&project_with_runner("claude", "claude")).unwrap();
            assert!(
                !validate_configured_orchestrator_transition("demo").unwrap(),
                "a drained native thread must migrate to the file-based handoff"
            );
            assert!(!state_path.exists(), "the live marker should be archived");
            assert!(
                archived_marker(&project_dir).is_some(),
                "an archived marker must remain for recovery"
            );
            assert!(!crate::wake::has_persisted_codex_thread("demo").unwrap());

            // Idempotent: a second preflight has nothing left to archive.
            assert!(!validate_configured_orchestrator_transition("demo").unwrap());
        });
    }

    #[test]
    fn stale_inactive_marker_is_archived_immediately() {
        with_temp_home(|_| {
            shelbi_state::save_project(&project_with_runner("claude", "claude")).unwrap();
            let state_path = write_thread_marker(false);
            let project_dir = state_path.parent().unwrap().to_path_buf();

            assert!(
                !validate_configured_orchestrator_transition("demo").unwrap(),
                "an inactive marker must never block a switch"
            );
            assert!(!state_path.exists());
            assert!(archived_marker(&project_dir).is_some());
        });
    }

    #[test]
    fn pending_queue_blocks_switch_and_names_delivery_ids() {
        with_temp_home(|_| {
            shelbi_state::save_project(&project_with_runner("codex", "codex")).unwrap();
            let state_path = write_thread_marker(true);
            let before = fs::read(&state_path).unwrap();
            let message_id = crate::wake::seed_pending_codex_batch("demo", 4, 42).unwrap();

            shelbi_state::save_project(&project_with_runner("claude", "claude")).unwrap();
            let error = validate_configured_orchestrator_transition("demo")
                .expect_err("a pending queue must block the switch")
                .to_string();
            assert!(error.contains("orchestrator runner `claude`"), "{error}");
            assert!(error.contains(&message_id), "must name the delivery id: {error}");
            assert!(error.contains("undelivered"), "{error}");
            assert!(error.contains("Restore the Codex runner"), "{error}");

            // The switch is refused, so the native thread is left intact.
            assert_eq!(fs::read(&state_path).unwrap(), before);
            assert!(crate::wake::has_persisted_codex_thread("demo").unwrap());
        });
    }

    #[test]
    fn captured_non_codex_argv_is_not_authorized_by_later_config() {
        with_temp_home(|_| {
            // Marker + Codex config, but a caller already captured Claude argv.
            shelbi_state::save_project(&project_with_runner("codex", "codex")).unwrap();
            let state_path = write_thread_marker(true);
            let project_dir = state_path.parent().unwrap().to_path_buf();
            crate::wake::seed_pending_codex_batch("demo", 4, 42).unwrap();

            // The immediate launch guard validates captured argv, not a freshly
            // reloaded config value: a pending queue blocks the captured Claude
            // launch even though config still reads Codex.
            let error = validate_orchestrator_runner_transition(
                "demo",
                "captured-claude",
                "/opt/claude/bin/claude",
            )
            .expect_err("captured non-Codex argv must go through the migration guard")
            .to_string();
            assert!(error.contains("captured-claude"), "{error}");
            assert!(state_path.exists(), "a blocked switch leaves the marker intact");
            assert!(archived_marker(&project_dir).is_none());
        });
    }
}
