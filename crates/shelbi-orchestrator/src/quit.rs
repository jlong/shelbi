//! Quit composition for the single-process (session-backend) runtime —
//! removing-tmux Phase 4f.
//!
//! The plan gives the new TUI three quit actions:
//!
//! - **Close the UI** — agents keep running. That is purely a client decision
//!   (drop the shell's session connections and exit); nothing here is involved,
//!   because a session process outlives every client (see `shelbi-session`).
//! - **Quit project** — end that project's sessions and mark it closed, going
//!   through the daemon's quit barrier ([`crate::cancel`]).
//! - **Quit Shelbi** — end all sessions and stop the daemon.
//!
//! This module owns the **project/shelbi** compositions. They run inside the
//! daemon (reached over the control socket), because that is where the quit
//! barrier's job registry lives and where "mark closed, then exit when idle"
//! is already wired (`rt-daemon-lifecycle`). The ordering is load-bearing and
//! identical for both actions:
//!
//! 1. **Mark the project(s) closed first.** Every session process runs a
//!    watchdog that restarts a crashed daemon *only while its project is open*
//!    (`shelbi_session::daemon_watchdog`). Clearing the open flag before any
//!    session ends is what makes "quit Shelbi … and nothing restarts it" hold:
//!    a session shutting down sees `open == false` and stays out of the way.
//! 2. **End the sessions.** Best-effort kill of the matching session processes
//!    across the local hub and any remote machines.
//! 3. **Drain the quit barrier.** [`crate::cancel::quit_project`] trips the
//!    project's in-flight daemon jobs (poll/launch threads) and we wait for
//!    them to acknowledge, so "closed" means "no job for this project is still
//!    running."
//!
//! While a composition runs it holds a [`QuitInFlight`] guard, taken *before*
//! step 1. The daemon's idle-exit monitor treats a quit in flight as activity
//! ([`quit_in_flight`]): marking the last open project closed would otherwise
//! look like "nothing open" while the quit is still waiting on the
//! orchestrator's handoff, and the daemon would exit mid-quit, orphaning the
//! very sessions step 2 was about to end.
//!
//! Session termination is behind the [`Sessions`] seam so the composition —
//! the ordering, the per-project scoping, the barrier wait — is unit-testable
//! without standing up real session processes. [`BackendSessions`] is the
//! production implementation over [`crate::session_backend::backend`].

use std::sync::atomic::{AtomicUsize, Ordering};

use shelbi_core::{Host, MachineKind};

use crate::session_backend::{backend, Backend};

/// The session-termination seam the quit compositions drive. Hiding IO behind a
/// trait keeps the ordering/scoping logic testable without real sessions.
pub trait Sessions {
    /// The logical names of every live session Shelbi owns, across the local
    /// hub and the given project's (or all open projects') remote machines.
    /// Names follow the backend scheme: `<project>/orch`, `<project>/ws/<ws>`.
    fn live_names(&self) -> Vec<String>;
    /// Best-effort end the session named `name` (kill its child's process
    /// group). An already-gone session is fine.
    fn kill(&self, name: &str);
}

/// Quit compositions currently running in this process.
static QUITS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);

/// Marks a quit composition as running for as long as it is held. Drop-based,
/// so a panicking quit still releases it and can't pin the daemon alive.
#[must_use = "the quit only counts as in flight while the guard is held"]
pub struct QuitInFlight(());

impl QuitInFlight {
    /// Count a quit as in flight until the returned guard drops.
    pub fn enter() -> Self {
        QUITS_IN_FLIGHT.fetch_add(1, Ordering::SeqCst);
        Self(())
    }
}

impl Drop for QuitInFlight {
    fn drop(&mut self) {
        QUITS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Whether any quit composition is running in this process. The guard is
/// taken before the open flag is cleared and released only after the sessions
/// are ended and the barrier drained, so a caller that reads the open-project
/// set *first* and this *second* never sees "nothing open, no quit running"
/// while a quit is still between those two points.
pub fn quit_in_flight() -> bool {
    QUITS_IN_FLIGHT.load(Ordering::SeqCst) > 0
}

/// Whether a session `name` belongs to `project` under the backend's naming
/// scheme (`<project>/…`). The trailing slash keeps `alpha` from matching
/// `alpha-2`'s sessions.
pub fn belongs_to_project(name: &str, project: &str) -> bool {
    name == project || name.starts_with(&format!("{project}/"))
}

/// Outcome of a [`quit_project`] / [`quit_project_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuitProjectOutcome {
    /// The session names we asked to end (scoped to the project).
    pub ended: Vec<String>,
    /// Whether the quit barrier drained within its bound (`false` = a wedged
    /// job was left for the OS to reap at process exit).
    pub drained: bool,
}

/// Outcome of a [`quit_shelbi`] / [`quit_shelbi_with`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuitShelbiOutcome {
    /// The projects that were open and are now marked closed.
    pub closed: Vec<String>,
    /// Every session name we asked to end.
    pub ended: Vec<String>,
    /// Whether every closed project's barrier drained within its bound.
    pub drained: bool,
}

/// Quit one project (production). Marks it closed, asks the orchestrator to
/// write its handoff file (so the next open resumes with context, exactly as
/// the tmux quit does), then ends its sessions and drains the quit barrier.
pub fn quit_project(project: &str) -> QuitProjectOutcome {
    quit_project_inner(project, &BackendSessions, || {
        // Best-effort: a missing/timed-out handoff is degraded (next open is
        // cold), never fatal to the quit.
        let _ = crate::handoff::request_orchestrator_handoff(project);
    })
}

/// Quit one project against an injected [`Sessions`], with no handoff — the
/// unit-test entry point. Exercises the same ordering as production.
pub fn quit_project_with(project: &str, sessions: &dyn Sessions) -> QuitProjectOutcome {
    quit_project_inner(project, sessions, || {})
}

/// The shared composition. `before_end_sessions` runs after the project is
/// marked closed but before its sessions end — the window in which the live
/// orchestrator can still be asked for a handoff.
fn quit_project_inner(
    project: &str,
    sessions: &dyn Sessions,
    before_end_sessions: impl FnOnce(),
) -> QuitProjectOutcome {
    // Held across every step: closing the last open project must not let the
    // daemon idle-exit before the handoff and session teardown below finish.
    let _in_flight = QuitInFlight::enter();

    // 1. Mark closed first (watchdog contract above).
    let _ = shelbi_state::set_project_open(project, false);
    let _ = shelbi_state::append_project_event(project, "closed", "user:quit-project");

    // 2. Handoff (production only) while the orchestrator session is still live.
    before_end_sessions();

    // 3. End only this project's sessions.
    let mut ended = Vec::new();
    for name in sessions.live_names() {
        if belongs_to_project(&name, project) {
            sessions.kill(&name);
            ended.push(name);
        }
    }

    // 4. Drain the barrier for this project's in-flight daemon jobs.
    let drained = crate::cancel::quit_project(project).wait();

    QuitProjectOutcome { ended, drained }
}

/// Quit Shelbi (production). See [`quit_shelbi_with`]. Does **not** stop the
/// daemon itself — the caller (the daemon's control handler) stops after the
/// client is acknowledged, so the shutdown doesn't race the reply.
pub fn quit_shelbi() -> QuitShelbiOutcome {
    quit_shelbi_with(&BackendSessions)
}

/// Quit Shelbi against an injected [`Sessions`]: mark every open project closed
/// (so no watchdog resurrects the daemon), end all sessions, and drain each
/// project's quit barrier.
pub fn quit_shelbi_with(sessions: &dyn Sessions) -> QuitShelbiOutcome {
    let _in_flight = QuitInFlight::enter();

    // 1. Close every open project FIRST.
    let open = shelbi_state::list_open_projects().unwrap_or_default();
    for project in &open {
        let _ = shelbi_state::set_project_open(project, false);
        let _ = shelbi_state::append_project_event(project, "closed", "user:quit-shelbi");
    }

    // 2. End every live session.
    let ended: Vec<String> = sessions.live_names();
    for name in &ended {
        sessions.kill(name);
    }

    // 3. Drain every closed project's barrier.
    let mut drained = true;
    for project in &open {
        drained &= crate::cancel::quit_project(project).wait();
    }

    QuitShelbiOutcome {
        closed: open,
        ended,
        drained,
    }
}

/// The production [`Sessions`]: enumerates and kills session processes through
/// the active [`Backend`] (the session backend when the dev flag is on; with
/// tmux the backend's `live_pane_ids`/`kill_window` operate on tmux and this is
/// effectively unused, since the tmux runtime keeps its own teardown).
pub struct BackendSessions;

impl Sessions for BackendSessions {
    fn live_names(&self) -> Vec<String> {
        let backend = backend();
        let mut names = live_on_host(&backend, &Host::Local);
        // Remote machines across every open project — a closed project should
        // have no live sessions, but we still sweep the machines of whatever is
        // open so a remote orchestrator/workspace is reached.
        for host in remote_hosts_of_open_projects() {
            names.extend(live_on_host(&backend, &host));
        }
        names.sort();
        names.dedup();
        names
    }

    fn kill(&self, name: &str) {
        let backend = backend();
        // Local first; if it wasn't a local session the remote sweep handles it.
        if backend.kill_window(&Host::Local, name).is_ok() {
            // best-effort; a non-matching local kill is harmless.
        }
        for host in remote_hosts_of_open_projects() {
            let _ = backend.kill_window(&host, name);
        }
    }
}

/// Live session names on one host (best-effort; an unreachable host yields none).
fn live_on_host(backend: &Backend, host: &Host) -> Vec<String> {
    backend.live_pane_ids(host).unwrap_or_default()
}

/// The distinct SSH hosts of every currently-open project.
fn remote_hosts_of_open_projects() -> Vec<Host> {
    let mut hosts: Vec<Host> = Vec::new();
    let open = shelbi_state::list_open_projects().unwrap_or_default();
    for project in &open {
        let Ok(p) = shelbi_state::load_project(project) else {
            continue;
        };
        for machine in &p.machines {
            if matches!(machine.kind, MachineKind::Local) {
                continue;
            }
            let host = machine.host();
            if host.is_ssh() && !hosts.contains(&host) {
                hosts.push(host);
            }
        }
    }
    hosts
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[test]
    fn belongs_to_project_matches_scheme_and_disambiguates_prefixes() {
        assert!(belongs_to_project("alpha/orch", "alpha"));
        assert!(belongs_to_project("alpha/ws/dev", "alpha"));
        assert!(belongs_to_project("alpha", "alpha"));
        // A different project whose name shares a prefix must not match.
        assert!(!belongs_to_project("alpha-2/orch", "alpha"));
        assert!(!belongs_to_project("beta/orch", "alpha"));
    }

    /// A [`Sessions`] fake recording every kill, over a fixed live-name set.
    struct FakeSessions {
        live: Vec<String>,
        killed: Mutex<Vec<String>>,
    }
    impl FakeSessions {
        fn new(live: &[&str]) -> Self {
            Self {
                live: live.iter().map(|s| s.to_string()).collect(),
                killed: Mutex::new(Vec::new()),
            }
        }
        fn killed(&self) -> Vec<String> {
            self.killed.lock().unwrap().clone()
        }
    }
    impl Sessions for FakeSessions {
        fn live_names(&self) -> Vec<String> {
            self.live.clone()
        }
        fn kill(&self, name: &str) {
            self.killed.lock().unwrap().push(name.to_string());
        }
    }

    /// An isolated `SHELBI_HOME` with a couple of filesystem projects so
    /// `set_project_open` / `list_open_projects` operate on real state.
    struct Home {
        path: std::path::PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }
    impl Home {
        fn new(tag: &str, projects: &[&str]) -> Self {
            let guard = crate::test_lock::acquire();
            let path = std::env::temp_dir().join(format!(
                "shb-quit-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            for p in projects {
                std::fs::create_dir_all(path.join(format!("projects/{p}/tasks"))).unwrap();
                std::fs::write(
                    path.join(format!("projects/{p}.yaml")),
                    format!(
                        "name: {p}\nrepo: /tmp/{p}\ndefault_branch: main\n\
                         orchestrator:\n  runner: claude\n\
                         agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                         machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{p}\n\
                         workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n",
                    ),
                )
                .unwrap();
            }
            std::env::set_var("SHELBI_HOME", &path);
            Self { path, _guard: guard }
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn quit_project_ends_only_that_projects_sessions_and_marks_it_closed() {
        // AC3: quit project ends only that project's sessions and marks it
        // closed. Two projects are open; quitting `alpha` must leave `beta`
        // open and never touch `beta`'s sessions.
        let home = Home::new("qp-scope", &["alpha", "beta"]);
        shelbi_state::set_project_open("alpha", true).unwrap();
        shelbi_state::set_project_open("beta", true).unwrap();

        let sessions = FakeSessions::new(&[
            "alpha/orch",
            "alpha/ws/dev",
            "beta/orch",
            "alpha-2/orch", // a prefix-sharing OTHER project
        ]);

        let out = quit_project_with("alpha", &sessions);

        // Only alpha's sessions were ended.
        assert_eq!(sessions.killed(), vec!["alpha/orch", "alpha/ws/dev"]);
        assert_eq!(out.ended, vec!["alpha/orch", "alpha/ws/dev"]);
        assert!(out.drained, "no jobs registered → barrier drains at once");
        // alpha is closed; beta is untouched.
        assert!(!shelbi_state::is_project_open("alpha").unwrap());
        assert!(shelbi_state::is_project_open("beta").unwrap());
        drop(home);
    }

    #[test]
    fn quit_project_waits_for_the_quit_barrier_to_drain() {
        // AC3: "waits for its jobs to cancel". Register a long-lived poll job
        // for the project that acknowledges cancellation only when its flag
        // trips; quit_project must trip it and return once it has drained.
        let home = Home::new("qp-barrier", &["gamma"]);
        shelbi_state::set_project_open("gamma", true).unwrap();

        let guard = crate::cancel::register("gamma", None, crate::cancel::JobKind::Poll);
        let cancelled = guard.cancel_flag();
        let done = Arc::new(AtomicBool::new(false));
        let worker = {
            let done = done.clone();
            std::thread::spawn(move || {
                // Hold the guard (job "running") until cancelled, then drop it
                // to acknowledge — exactly what a real poll thread does.
                while !cancelled.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                drop(guard);
                done.store(true, Ordering::SeqCst);
            })
        };

        let out = quit_project_with("gamma", &FakeSessions::new(&[]));
        assert!(out.drained, "the barrier drained after the job acknowledged");
        worker.join().unwrap();
        assert!(done.load(Ordering::SeqCst), "the cancelled job ran to completion");
        drop(home);
    }

    /// A [`Sessions`] fake that records whether a quit was in flight at the
    /// moment each session was ended.
    struct InFlightAtKill {
        live: Vec<String>,
        in_flight_at_kill: Mutex<Vec<bool>>,
    }
    impl Sessions for InFlightAtKill {
        fn live_names(&self) -> Vec<String> {
            self.live.clone()
        }
        fn kill(&self, _name: &str) {
            self.in_flight_at_kill.lock().unwrap().push(quit_in_flight());
        }
    }

    #[test]
    fn quit_project_stays_in_flight_through_handoff_and_session_teardown() {
        // The daemon's idle exit defers while `quit_in_flight()` is true. It
        // must already be true by the time the last open project reads closed
        // (the handoff wait, here standing in for a handoff that times out),
        // and still true while the orchestrator session is ended, so idle exit
        // can only fire after the session is gone.
        let home = Home::new("qp-inflight", &["solo"]);
        shelbi_state::set_project_open("solo", true).unwrap();
        let sessions = InFlightAtKill {
            live: vec!["solo/orch".into()],
            in_flight_at_kill: Mutex::new(Vec::new()),
        };

        let seen_during_handoff = Mutex::new(None);
        let out = quit_project_inner("solo", &sessions, || {
            // A timed-out handoff: the wait elapses with nothing written.
            std::thread::sleep(Duration::from_millis(20));
            *seen_during_handoff.lock().unwrap() = Some((
                shelbi_state::list_open_projects().unwrap(),
                quit_in_flight(),
            ));
        });

        let (open, in_flight) = seen_during_handoff.into_inner().unwrap().unwrap();
        assert!(open.is_empty(), "the project reads closed during the handoff");
        assert!(in_flight, "a quit is in flight while the handoff is pending");
        assert_eq!(
            *sessions.in_flight_at_kill.lock().unwrap(),
            vec![true],
            "the orchestrator session is ended while the quit is still in flight",
        );
        assert_eq!(out.ended, vec!["solo/orch"]);
        drop(home);
    }

    #[test]
    fn quit_shelbi_closes_every_project_before_ending_sessions() {
        // AC4: quit Shelbi ends all sessions and closes every project. The
        // close-before-end ordering is what stops a watchdog resurrecting the
        // daemon, so assert the projects are closed and all sessions ended.
        let home = Home::new("qs-all", &["one", "two"]);
        shelbi_state::set_project_open("one", true).unwrap();
        shelbi_state::set_project_open("two", true).unwrap();

        let sessions = FakeSessions::new(&["one/orch", "two/orch", "two/ws/dev"]);
        let out = quit_shelbi_with(&sessions);

        assert_eq!(out.closed, vec!["one", "two"]);
        assert_eq!(out.ended.len(), 3);
        assert_eq!(sessions.killed().len(), 3, "every live session is ended");
        assert!(out.drained);
        assert!(shelbi_state::list_open_projects().unwrap().is_empty());
        assert!(!shelbi_state::is_project_open("one").unwrap());
        assert!(!shelbi_state::is_project_open("two").unwrap());
        drop(home);
    }
}
