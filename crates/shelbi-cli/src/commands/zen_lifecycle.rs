//! Internal lifecycle hooks the orchestrator pane wrapper invokes
//! around its agent launch. Not user-facing — the entries live behind
//! `__zen-orch-start`, `__zen-heartbeat`, and `__zen-orch-exit` in the
//! CLI surface and are emitted by the `sh -c` wrapper that
//! `shelbi_orchestrator::ensure_dashboard` constructs.
//!
//! The wrapper sequence is:
//!
//! ```text
//! __zen-orch-start   -> check crash recovery, maybe disable + warn
//! (heartbeat loop)   -> __zen-heartbeat every 60s while agent is alive
//! <agent runs>
//! __zen-orch-exit    -> graceful: clear heartbeat so next start is clean
//! ```
//!
//! If the pane dies mid-run (kill, SIGHUP, machine power loss), the
//! wrapper shell dies with it, the exit hook never runs, and the
//! heartbeat timestamp stays recent on disk — which is exactly the
//! signal `__zen-orch-start` reads on the next startup.

use anyhow::{anyhow, Result};

use shelbi_state::{
    append_project_event, zen_check_crash_recovery, zen_clear_crash, zen_heartbeat,
    ZenCrashRecovery,
};

/// `shelbi __zen-orch-start <project>` — runs once at the top of the
/// orchestrator pane's wrapper script. Checks `state.json` for a
/// recent heartbeat with no graceful-exit clear; if found AND Zen was
/// on, force it off, emit a `zen=off reason=crash-recovery` line to
/// `events.log`, and print a single warning to stderr so the user sees
/// it the moment the pane respawns.
pub fn orch_start(project: &str) -> Result<()> {
    match zen_check_crash_recovery(project).map_err(|e| anyhow!(e))? {
        ZenCrashRecovery::NoCrash => Ok(()),
        ZenCrashRecovery::AutoDisabled { crashed_at } => {
            // Best-effort event-log write. If it fails the in-pane stderr
            // line still carries the warning to the user.
            let _ = append_project_event(project, "zen=off", "crash-recovery");
            let pretty = crashed_at.with_timezone(&chrono::Local).format("%H:%M:%S");
            tracing::warn!(
                project,
                crashed_at = %crashed_at.to_rfc3339(),
                "zen mode auto-disabled — orchestrator pane was last alive at {pretty} \
                 and didn't exit cleanly; review before re-enabling with `shelbi zen on`"
            );
            eprintln!(
                "warning: zen mode auto-disabled after detecting an orchestrator crash \
                 (last heartbeat at {pretty}). Re-enable with `shelbi zen on` once you've \
                 reviewed any in-flight work."
            );
            Ok(())
        }
    }
}

/// `shelbi __zen-heartbeat <project>` — refresh `zen_last_crashed_at`
/// to "now" so the wrapper has a current liveness signal on disk.
/// Errors are best-effort: a one-off write failure shouldn't kill the
/// orchestrator pane.
pub fn heartbeat(project: &str) -> Result<()> {
    if let Err(e) = zen_heartbeat(project) {
        tracing::debug!(project, "zen heartbeat failed (will retry): {e}");
    }
    Ok(())
}

/// `shelbi __zen-orch-exit <project>` — clear `zen_last_crashed_at` so
/// the next orchestrator start doesn't misread this graceful exit as
/// a crash. Idempotent.
pub fn orch_exit(project: &str) -> Result<()> {
    zen_clear_crash(project).map_err(|e| anyhow!(e))
}
