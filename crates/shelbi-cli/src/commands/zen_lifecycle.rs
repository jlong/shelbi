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
    append_project_event, maybe_emit_claude_md_migration_hint, reset_claude_md_migration_hint,
    zen_check_crash_recovery, zen_clear_crash, zen_heartbeat, OrchestratorCrash, ZenCrashRecovery,
};

/// Lines of the dead pane's output to snapshot into a crash record. Enough
/// to catch a panic backtrace / final stderr without bloating the file.
const CRASH_OUTPUT_TAIL_LINES: usize = 200;

/// How far back a `config-upgrade` events.log line still counts as "ran this
/// boot" for the crash record's self-heal flag. Generous — a boot's
/// self-heal fires seconds before the pane launches, and this only decorates
/// a post-mortem, so an over-long window is harmless.
const SELF_HEAL_LOOKBACK_SECS: i64 = 900;

/// `shelbi __zen-orch-start <project>` — runs once at the top of the
/// orchestrator pane's wrapper script. Checks `state.json` for a
/// recent heartbeat with no graceful-exit clear; if found AND Zen was
/// on, force it off, emit a `zen=off reason=crash-recovery` line to
/// `events.log`, and print a single warning to stderr so the user sees
/// it the moment the pane respawns.
///
/// Also resets the per-session "CLAUDE.md migration hint already
/// fired" latch and (best-effort) re-fires the hint if a legacy
/// `~/.shelbi/projects/<project>/CLAUDE.md` still exists. The reset
/// happens unconditionally so a fresh orchestrator session always
/// re-checks; the hint itself is a one-shot stderr nudge that v2 will
/// drop entirely.
pub fn orch_start(project: &str) -> Result<()> {
    // This internal hook can disable Zen and also updates its per-session
    // migration latch. Refuse the whole mutation bundle before touching disk
    // when the long-lived daemon belongs to another Shelbi build.
    shelbi_state::ensure_daemon_matches_for_mutation().map_err(|e| anyhow!(e))?;
    // Best-effort: a write failure here doesn't change Zen-recovery
    // semantics, and the migration hint is purely advisory. We don't
    // want a state.json hiccup to kill the orchestrator pane on start.
    let _ = reset_claude_md_migration_hint(project);
    let _ = maybe_emit_claude_md_migration_hint(project);

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
        // Surface, don't swallow: a persistent heartbeat write failure
        // means the crash-recovery signal on disk is stale, and the
        // previous debug-level line was invisible in the field. The
        // hook still exits 0 so a one-off hiccup can't kill the
        // orchestrator pane.
        tracing::warn!(project, "zen heartbeat failed (will retry): {e}");
    }
    Ok(())
}

/// `shelbi __zen-orch-exit <project>` — clear `zen_last_crashed_at` so
/// the next orchestrator start doesn't misread this graceful exit as
/// a crash. Idempotent.
pub fn orch_exit(project: &str) -> Result<()> {
    zen_clear_crash(project).map_err(|e| anyhow!(e))
}

/// `shelbi __orch-record-exit <project> <reason> [pane]` — the crash-record
/// capture the orchestrator pane wrapper runs on every exit path: once after
/// the agent returns (`exit:<code>`) and from its `SIGHUP` trap
/// (`signal:SIGHUP`). Writes a persisted crash record — exit code/signal, a
/// tail of the dead pane's output, and whether a config self-heal ran that
/// boot — plus an `events.log` pointer to it, but ONLY for a genuine crash.
///
/// A clean agent exit (`exit:0`) and every graceful teardown produce nothing.
/// The discriminator is `zen_last_crashed_at`: every graceful path (quit,
/// quit-project, reload, host teardown) clears it *before* killing the pane,
/// while a crash leaves the heartbeat's timestamp set — so a still-set marker
/// is the crash signal. This mirrors the Zen crash-recovery contract exactly,
/// so the record and the `zen=off` auto-disable can never disagree.
///
/// Best-effort by construction: the caller is a dying pane, so a failure to
/// capture must never block its teardown or the supervisor's respawn. The
/// whole body is swallowed to `Ok(())` and the hook always exits 0.
pub fn orch_record_exit(project: &str, reason: &str, pane: Option<&str>) -> Result<()> {
    if let Err(e) = record_if_crash(project, reason, pane) {
        // Surface at warn so a persistent capture failure is visible in the
        // logs, but never propagate: the pane is closing either way.
        tracing::warn!(project, reason, "orchestrator crash-record capture failed: {e}");
    }
    Ok(())
}

/// The fallible core of [`orch_record_exit`]. Returns early (no record) for a
/// clean exit or a graceful teardown; otherwise persists the crash record and
/// emits its `events.log` pointer.
fn record_if_crash(project: &str, reason: &str, pane: Option<&str>) -> Result<()> {
    // A clean agent exit or a deliberate SIGTERM shutdown is never a crash,
    // regardless of the marker (belt-and-suspenders against a stale heartbeat).
    if reason == "exit:0" || reason == "signal:SIGTERM" {
        return Ok(());
    }
    // Graceful teardowns clear the heartbeat before the kill; a cleared marker
    // means "expected exit", so there is nothing to record.
    let state = shelbi_state::read_state(project).map_err(|e| anyhow!(e))?;
    if state.zen_last_crashed_at.is_none() {
        return Ok(());
    }

    let output_tail = pane.map(capture_pane_tail).unwrap_or_default();
    let self_heal_ran = recent_config_self_heal(project);
    let crash = OrchestratorCrash {
        reason: reason.to_string(),
        output_tail,
        self_heal_ran,
    };
    let path = shelbi_state::record_orchestrator_crash(project, &crash).map_err(|e| anyhow!(e))?;

    // Point the log at the record. Path tokens under `~/.shelbi/` are
    // space-free (validated project name), so this stays a single parseable
    // `record=` token. Best-effort — a missing daemon falls back to a file
    // append, and even a total failure leaves the on-disk record + state
    // pointer intact.
    let _ = shelbi_state::emit_event_body(&format!(
        "project={project} orchestrator crash exit={reason} record={}",
        path.display()
    ));
    Ok(())
}

/// Snapshot the last [`CRASH_OUTPUT_TAIL_LINES`] lines of the orchestrator
/// pane's output. Runs on the hub (the orchestrator pane is always local), so
/// it shells out to `tmux capture-pane` directly. Best-effort: a gone pane, a
/// wedged tmux, or a non-UTF-8 blob all degrade to an empty tail rather than
/// failing the record.
fn capture_pane_tail(pane: &str) -> String {
    let start = format!("-{CRASH_OUTPUT_TAIL_LINES}");
    let out = std::process::Command::new("tmux")
        .args(["capture-pane", "-p", "-t", pane, "-S", &start])
        .output();
    match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).into_owned(),
        _ => String::new(),
    }
}

/// Did a config self-heal / validate-and-upgrade pass run recently enough to
/// count as "this boot"? Scans the tail of `events.log` for a
/// `project=<project> config-upgrade …` line inside [`SELF_HEAL_LOOKBACK_SECS`].
/// Best-effort → `false` on any I/O error or a missing log.
fn recent_config_self_heal(project: &str) -> bool {
    let Ok(path) = shelbi_state::events_log_path() else {
        return false;
    };
    let Ok(text) = read_events_tail(&path, EVENTS_TAIL_BYTES) else {
        return false;
    };
    let needle = format!(" project={project} config-upgrade");
    let cutoff = chrono::Utc::now() - chrono::Duration::seconds(SELF_HEAL_LOOKBACK_SECS);
    for line in text.lines().rev() {
        if !line.contains(&needle) {
            continue;
        }
        if let Some(token) = line.split_whitespace().next() {
            if let Ok(ts) = chrono::DateTime::parse_from_rfc3339(token) {
                if ts.with_timezone(&chrono::Utc) >= cutoff {
                    return true;
                }
            }
        }
    }
    false
}

/// Tail-read at most `max_bytes` of `path` as lossy UTF-8, seeking rather than
/// slurping the (unbounded) events log. Mirrors `status::read_tail`.
fn read_events_tail(path: &std::path::Path, max_bytes: u64) -> std::io::Result<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path)?;
    let len = f.metadata()?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        f.seek(SeekFrom::Start(start))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// Bytes of `events.log` tail scanned for a recent self-heal disclosure.
const EVENTS_TAIL_BYTES: u64 = 64 * 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use std::path::PathBuf;

    /// Env vars every test in this module isolates: `SHELBI_HOME` points the
    /// state + events.log at a throwaway dir, and `SHELBI_HUB_SOCK` MUST be
    /// cleared — inherited from the worker pane it would route
    /// `emit_event_body` to the real hub daemon instead of the test's
    /// degraded file append, so the assertion would read an empty test log.
    const ISOLATED_ENV: &[&str] = &["SHELBI_HOME", "SHELBI_HUB_SOCK"];

    fn setup_home(tag: &str) -> (EnvGuard, PathBuf) {
        let guard = EnvGuard::new(ISOLATED_ENV);
        let p = std::env::temp_dir().join(format!(
            "shelbi-orch-record-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        std::env::set_var("SHELBI_HOME", &p);
        std::env::remove_var("SHELBI_HUB_SOCK");
        (guard, p)
    }

    /// Arm a recent crash marker so `record_if_crash` reads a genuine crash.
    fn set_recent_crash_marker(project: &str) {
        shelbi_state::zen_heartbeat(project).unwrap();
        assert!(shelbi_state::read_state(project)
            .unwrap()
            .zen_last_crashed_at
            .is_some());
    }

    /// A non-zero agent exit with the crash marker still set produces a
    /// persisted record + a state pointer + an events.log pointer line.
    #[test]
    fn unexpected_exit_writes_a_crash_record() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_env, home) = setup_home("unexpected");

        set_recent_crash_marker("demo");
        // No pane id — deterministic, no tmux dependency; the record still
        // captures reason + boot context.
        orch_record_exit("demo", "exit:139", None).unwrap();

        let record = shelbi_state::read_state("demo")
            .unwrap()
            .orchestrator_last_crash_record
            .expect("a crash must leave a record pointer");
        let body = std::fs::read_to_string(&record).unwrap();
        assert!(body.contains("reason: exit:139"), "body: {body}");

        // events.log carries the pointer line.
        let log =
            std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap_or_default();
        assert!(
            log.contains("orchestrator crash exit=exit:139")
                && log.contains(&format!("record={record}")),
            "events.log missing crash pointer: {log}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A SIGHUP teardown that cleared the crash marker first (the graceful
    /// quit / reload path) produces NO record.
    #[test]
    fn graceful_teardown_writes_no_record() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_env, home) = setup_home("graceful-sighup");

        // Marker cleared == graceful teardown already ran zen_clear_crash.
        shelbi_state::zen_clear_crash("demo").unwrap();
        orch_record_exit("demo", "signal:SIGHUP", None).unwrap();

        assert!(
            shelbi_state::read_state("demo")
                .unwrap()
                .orchestrator_last_crash_record
                .is_none(),
            "graceful teardown must not write a crash record"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A clean agent exit (`exit:0`) is never a crash even while the heartbeat
    /// marker is still set (the exit hook clears it a step later).
    #[test]
    fn clean_exit_writes_no_record_even_with_marker_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_env, home) = setup_home("clean-exit");

        set_recent_crash_marker("demo");
        orch_record_exit("demo", "exit:0", None).unwrap();

        assert!(
            shelbi_state::read_state("demo")
                .unwrap()
                .orchestrator_last_crash_record
                .is_none(),
            "a clean exit must not write a crash record"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// SIGTERM is treated as a graceful shutdown regardless of the marker, per
    /// the acceptance criterion.
    #[test]
    fn sigterm_writes_no_record_even_with_marker_set() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (_env, home) = setup_home("sigterm");

        set_recent_crash_marker("demo");
        orch_record_exit("demo", "signal:SIGTERM", None).unwrap();

        assert!(
            shelbi_state::read_state("demo")
                .unwrap()
                .orchestrator_last_crash_record
                .is_none(),
            "SIGTERM is a graceful shutdown and must not write a record"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
