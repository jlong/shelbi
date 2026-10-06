//! `shelbi reload` for the single-process (session-backend) runtime —
//! removing-tmux Phase 4f.
//!
//! The tmux reload ([`super::reload`]) respawns the orchestrator pane and the
//! shelbi-owned TUI panes in place. With the session backend there are no
//! panes: the plan redefines reload as
//!
//! > restart the daemon, signal attached TUI clients to re-exec and desktop
//! > apps to prompt for relaunch, and hand off the orchestrator as today by
//! > replacing its session. Worker sessions are not touched.
//!
//! The ordering is load-bearing and lives in [`run`]:
//!
//! 1. **Handoff** — ask the still-live orchestrator to write its handoff file,
//!    exactly as the tmux path does, so the replacement starts with context.
//! 2. **Signal clients** — tell the (soon-to-be-stale, after the restart)
//!    attached clients to re-exec / prompt for relaunch. This must happen
//!    *before* the daemon restarts, because it is the running daemon that
//!    broadcasts the re-exec push to its subscribers.
//! 3. **Restart the daemon** — stop it and start a fresh one on the current
//!    binary.
//! 4. **Replace the orchestrator session** — end the `<project>/orch` session
//!    and bring a fresh one up (it ingests the handoff). **Worker sessions are
//!    left running**; `replace_orchestrator_session` touches only the
//!    orchestrator, so a worker mid-task is never interrupted by a reload.
//!
//! The IO is behind the [`ReloadOps`] seam so the ordering — and the "workers
//! untouched" contract — is unit-testable without a daemon, a control socket,
//! or real sessions.

use anyhow::Result;

/// The side effects a session-runtime reload performs, behind a trait so the
/// orchestration order is testable with a recording stub.
pub(crate) trait ReloadOps {
    /// Ask the live orchestrator to write its handoff file. Returns a short
    /// status line for the user; best-effort (never fails the reload).
    fn request_handoff(&self) -> String;
    /// Tell every attached client to re-exec (a TUI) / prompt for relaunch (the
    /// desktop app). Best-effort.
    fn signal_clients_reexec(&self);
    /// Stop the daemon and start a fresh one on the current binary.
    fn restart_daemon(&self) -> Result<()>;
    /// End the orchestrator session and bring a fresh one up (ingesting the
    /// handoff). Must NOT touch worker sessions.
    fn replace_orchestrator_session(&self) -> Result<()>;
}

/// The step a reload reached, for reporting / assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ReloadReport {
    pub handoff_status: String,
    pub signalled_clients: bool,
    pub restarted_daemon: bool,
    pub replaced_orchestrator: bool,
}

/// Run the session-runtime reload in the required order (see module docs).
pub(crate) fn run(ops: &dyn ReloadOps) -> Result<ReloadReport> {
    // 1. Handoff while the old orchestrator session is still alive.
    let handoff_status = ops.request_handoff();
    // 2. Signal clients to re-exec — the running daemon broadcasts this, so it
    //    must precede the restart.
    ops.signal_clients_reexec();
    // 3. Restart the daemon on the current binary.
    ops.restart_daemon()?;
    // 4. Replace the orchestrator session (workers untouched).
    ops.replace_orchestrator_session()?;
    Ok(ReloadReport {
        handoff_status,
        signalled_clients: true,
        restarted_daemon: true,
        replaced_orchestrator: true,
    })
}

/// The production [`ReloadOps`] for `project`.
pub(crate) struct LiveReload {
    pub project: String,
}

impl ReloadOps for LiveReload {
    fn request_handoff(&self) -> String {
        match shelbi_orchestrator::handoff::request_orchestrator_handoff(&self.project) {
            Ok(outcome) => format!("{outcome:?}"),
            Err(e) => format!("handoff request failed: {e}"),
        }
    }

    fn signal_clients_reexec(&self) {
        // Best-effort: a missing daemon or a failed connect just means no client
        // was connected to signal; the restart below replaces it anyway.
        let Ok(sock) = shelbi_state::control_socket_path() else {
            return;
        };
        if let Ok(mut client) =
            shelbi_client::ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION)
        {
            let _ = client.reload_clients();
        }
    }

    fn restart_daemon(&self) -> Result<()> {
        // Stop the running daemon and start a fresh one on the current binary.
        // (The idle monitor / watchdog would restart it anyway while a project
        // is open, but an explicit restart makes reload prompt and deterministic.)
        let _ = shelbi_state::stop_daemon();
        shelbi_state::ensure_daemon_running()
            .map_err(|e| anyhow::anyhow!("restarting the hub daemon: {e}"))
    }

    fn replace_orchestrator_session(&self) -> Result<()> {
        // End only the orchestrator session; worker sessions are left running.
        let orch = format!("{}/orch", self.project);
        let backend = shelbi_orchestrator::session_backend::backend();
        let _ = backend.kill_window(&shelbi_core::Host::Local, &orch);
        // Bring a fresh orchestrator session up; it ingests the handoff file.
        shelbi_orchestrator::ensure_dashboard(&self.project)
            .map(|_| ())
            .map_err(|e| anyhow::anyhow!("respawning the orchestrator session: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Records the order of the reload steps.
    #[derive(Default)]
    struct RecordingOps {
        order: Mutex<Vec<&'static str>>,
    }
    impl ReloadOps for RecordingOps {
        fn request_handoff(&self) -> String {
            self.order.lock().unwrap().push("handoff");
            "Written".into()
        }
        fn signal_clients_reexec(&self) {
            self.order.lock().unwrap().push("signal");
        }
        fn restart_daemon(&self) -> Result<()> {
            self.order.lock().unwrap().push("restart");
            Ok(())
        }
        fn replace_orchestrator_session(&self) -> Result<()> {
            self.order.lock().unwrap().push("replace_orch");
            Ok(())
        }
    }

    #[test]
    fn reload_runs_handoff_signal_restart_then_replace_orchestrator() {
        // AC5: reload restarts the daemon, replaces the orchestrator session
        // with a handoff, re-execs attached TUIs, and leaves worker sessions
        // running. The ordering (handoff → signal clients → restart daemon →
        // replace orchestrator) is what makes each of those hold: the handoff
        // is captured before anything restarts, the re-exec signal goes out
        // while the old daemon is still up to broadcast it, and only the
        // orchestrator session is replaced (there is no kill-workers step).
        let ops = RecordingOps::default();
        let report = run(&ops).unwrap();
        assert_eq!(
            *ops.order.lock().unwrap(),
            vec!["handoff", "signal", "restart", "replace_orch"],
            "reload steps must run in the required order"
        );
        assert_eq!(report.handoff_status, "Written");
        assert!(report.signalled_clients);
        assert!(report.restarted_daemon);
        assert!(report.replaced_orchestrator);
        // "Worker sessions are not touched": the ops surface has no
        // kill-workers step, and replace_orchestrator_session ran exactly once.
        assert_eq!(
            ops.order.lock().unwrap().iter().filter(|s| **s == "replace_orch").count(),
            1,
        );
    }

    #[test]
    fn reload_aborts_if_the_daemon_restart_fails() {
        // A failed daemon restart must surface, not silently replace the
        // orchestrator against a dead daemon.
        struct FailRestart;
        impl ReloadOps for FailRestart {
            fn request_handoff(&self) -> String {
                "Written".into()
            }
            fn signal_clients_reexec(&self) {}
            fn restart_daemon(&self) -> Result<()> {
                anyhow::bail!("daemon would not come back")
            }
            fn replace_orchestrator_session(&self) -> Result<()> {
                panic!("must not replace the orchestrator when the restart failed");
            }
        }
        let err = run(&FailRestart).unwrap_err();
        assert!(err.to_string().contains("come back"), "err: {err}");
    }
}
