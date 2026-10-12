//! `shelbi quit` for the single-process (session-backend) runtime — restoring
//! the command `rt-cutover-delete` removed with the tmux-era `quit.rs`.
//!
//! With the session backend there are no panes to tear down: quitting is a
//! daemon operation. The palette already offers "Quit Project" / "Quit Shelbi"
//! through the daemon's [`ClientMsg::QuitProject`](shelbi_proto::control::ClientMsg::QuitProject)
//! / [`QuitShelbi`](shelbi_proto::control::ClientMsg::QuitShelbi) (`rt-tui-project-quit`);
//! this command is the shell sibling, a **thin client** over that same control
//! socket:
//!
//! - `shelbi quit` sends `QuitProject` for the resolved current project — the
//!   daemon ends that project's sessions (after asking its orchestrator to
//!   write a handoff), drains its quit barrier, and marks it closed. Other
//!   projects keep running.
//! - `shelbi quit --all` sends `QuitShelbi` — the daemon closes every project,
//!   ends all sessions, acks, then stops. The projects are closed before the
//!   sessions end, so no session watchdog resurrects the daemon.
//!
//! Both block on the daemon's ack via [`shelbi_client::ControlClient`]. The
//! orchestrator handoff and the close-before-end ordering live in
//! [`shelbi_orchestrator::quit`], invoked by the daemon's control handler — this
//! command does not reimplement them.
//!
//! When no daemon is running, nothing is open: the command reports that and
//! exits 0, so `shelbi quit` is a clean no-op on an idle machine (the daemon
//! idle-exits once the last project closes, so "no daemon" and "nothing open"
//! are the same state).
//!
//! The IO is behind the [`QuitOps`] seam so the routing — daemon-running gate,
//! project vs all, the ack wait — is unit-testable without a daemon or a
//! control socket.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};

use super::require_project;

/// The side effects a quit performs, behind a trait so the routing is testable
/// with a stubbed daemon.
pub(crate) trait QuitOps {
    /// Whether a hub daemon is currently running (its single-instance lock is
    /// held). When it is not, nothing is open — the daemon idle-exits once the
    /// last project closes.
    fn daemon_running(&self) -> bool;
    /// Quit one project through the control socket, blocking until the daemon
    /// acks. The daemon ends the project's sessions (after its handoff), drains
    /// its quit barrier, and marks it closed; other projects are untouched.
    fn quit_project(&self, project: &str) -> Result<()>;
    /// Quit Shelbi through the control socket, blocking until the daemon acks.
    /// The daemon closes every project, ends all sessions, then stops; nothing
    /// restarts it.
    fn quit_all(&self) -> Result<()>;
}

/// What a quit did, for reporting / assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QuitReport {
    /// No daemon was running — nothing to quit.
    NothingOpen,
    /// The named project was quit (its sessions ended, it is closed).
    Project(String),
    /// Shelbi was quit entirely (every project closed, the daemon stopped).
    All,
}

/// `shelbi quit` (`--all` for the whole hub). Resolves the current project only
/// when it is actually needed (a running daemon, not `--all`), so the
/// nothing-open path never fails on project resolution.
pub fn run(project_opt: Option<String>, all: bool) -> Result<()> {
    let report = run_with(&LiveQuit, all, || require_project(project_opt))?;
    match report {
        QuitReport::NothingOpen => {
            println!("shelbi: nothing is running — nothing to quit.");
        }
        QuitReport::Project(project) => {
            println!(
                "shelbi: quit \"{project}\" — its sessions were ended (after the orchestrator \
                 handoff) and it is now closed; other projects keep running. Worktrees and \
                 branches are left intact."
            );
        }
        QuitReport::All => {
            println!(
                "shelbi: quit Shelbi — every project was closed, all sessions ended, and the hub \
                 daemon stopped. Worktrees and branches are left intact."
            );
        }
    }
    Ok(())
}

/// The quit routing, exercised by both production and the tests. `resolve_project`
/// is deferred so it runs only on the `QuitProject` path.
pub(crate) fn run_with(
    ops: &dyn QuitOps,
    all: bool,
    resolve_project: impl FnOnce() -> Result<String>,
) -> Result<QuitReport> {
    // No daemon → nothing is open. Report and exit cleanly, for both forms.
    if !ops.daemon_running() {
        return Ok(QuitReport::NothingOpen);
    }

    if all {
        ops.quit_all()?;
        Ok(QuitReport::All)
    } else {
        let project = resolve_project()?;
        ops.quit_project(&project)?;
        Ok(QuitReport::Project(project))
    }
}

/// The production [`QuitOps`]: the daemon-running gate is the single-instance
/// lock, and each quit is a control-socket round-trip through
/// [`shelbi_client::ControlClient`].
struct LiveQuit;

impl QuitOps for LiveQuit {
    fn daemon_running(&self) -> bool {
        shelbi_state::daemon_lock_held()
    }

    fn quit_project(&self, project: &str) -> Result<()> {
        let mut client = connect()?;
        client
            .quit_project(project)
            .map_err(|e| anyhow!("quitting project `{project}`: {e}"))
    }

    fn quit_all(&self) -> Result<()> {
        let mut client = connect()?;
        client
            .quit_shelbi()
            .map_err(|e| anyhow!("quitting Shelbi: {e}"))
    }
}

/// Connect to the daemon's control socket, retrying briefly. `daemon_running`
/// gated on the lock, but a daemon mid-startup may hold the lock before the
/// control socket answers, so we retry for a short window rather than racing.
fn connect() -> Result<shelbi_client::ControlClient> {
    let sock = shelbi_state::control_socket_path().map_err(|e| anyhow!(e))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match shelbi_client::ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION) {
            Ok(c) => return Ok(c),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "could not reach the hub daemon's control socket at {}: {e}",
                        sock.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A stubbed daemon recording the quit calls the command makes.
    struct StubDaemon {
        running: bool,
        calls: Mutex<Vec<String>>,
    }
    impl StubDaemon {
        fn new(running: bool) -> Self {
            Self {
                running,
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl QuitOps for StubDaemon {
        fn daemon_running(&self) -> bool {
            self.running
        }
        fn quit_project(&self, project: &str) -> Result<()> {
            self.calls.lock().unwrap().push(format!("project:{project}"));
            Ok(())
        }
        fn quit_all(&self) -> Result<()> {
            self.calls.lock().unwrap().push("all".into());
            Ok(())
        }
    }

    #[test]
    fn quit_sends_quit_project_for_the_current_project() {
        // AC1: `shelbi quit` ends the current project's sessions and leaves
        // other projects running. The command sends exactly one QuitProject for
        // the resolved project and nothing else (no QuitShelbi, which would
        // stop the daemon and take other projects down with it).
        let daemon = StubDaemon::new(true);
        let report = run_with(&daemon, false, || Ok("alpha".to_string())).unwrap();
        assert_eq!(report, QuitReport::Project("alpha".to_string()));
        assert_eq!(daemon.calls(), vec!["project:alpha"]);
    }

    #[test]
    fn quit_all_sends_quit_shelbi_and_never_resolves_a_project() {
        // AC2: `shelbi quit --all` ends every session and stops the daemon. It
        // sends QuitShelbi and must not resolve a current project (so it works
        // from anywhere, e.g. not inside a project's work_dir).
        let daemon = StubDaemon::new(true);
        let report = run_with(&daemon, true, || {
            panic!("--all must not resolve a current project")
        })
        .unwrap();
        assert_eq!(report, QuitReport::All);
        assert_eq!(daemon.calls(), vec!["all"]);
    }

    #[test]
    fn quit_with_no_daemon_reports_nothing_open_and_does_not_connect() {
        // AC3: with no daemon running, `shelbi quit` exits 0 with a clear
        // message. It must short-circuit before resolving a project or making
        // any control-socket call.
        let daemon = StubDaemon::new(false);
        let report = run_with(&daemon, false, || {
            panic!("must not resolve a project when no daemon is running")
        })
        .unwrap();
        assert_eq!(report, QuitReport::NothingOpen);
        assert!(daemon.calls().is_empty(), "no quit call should be sent");
    }

    #[test]
    fn quit_all_with_no_daemon_also_reports_nothing_open() {
        // The nothing-open gate applies to both forms.
        let daemon = StubDaemon::new(false);
        let report = run_with(&daemon, true, || unreachable!()).unwrap();
        assert_eq!(report, QuitReport::NothingOpen);
        assert!(daemon.calls().is_empty());
    }

    #[test]
    fn quit_surfaces_a_daemon_error() {
        // A failed control-socket round-trip must surface, not be swallowed.
        struct FailDaemon;
        impl QuitOps for FailDaemon {
            fn daemon_running(&self) -> bool {
                true
            }
            fn quit_project(&self, _project: &str) -> Result<()> {
                anyhow::bail!("control socket went away")
            }
            fn quit_all(&self) -> Result<()> {
                unreachable!()
            }
        }
        let err = run_with(&FailDaemon, false, || Ok("alpha".to_string())).unwrap_err();
        assert!(err.to_string().contains("went away"), "err: {err}");
    }
}
