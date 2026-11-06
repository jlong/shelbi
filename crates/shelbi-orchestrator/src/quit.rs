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
//! 1. **Snapshot the hosts to sweep.** [`HostSweep`] records each remote machine
//!    of the project(s) being quit and which session names on it they own. It
//!    has to come first: Quit Shelbi finds its projects through the open flag,
//!    which step 3 clears.
//! 2. **Enter the quitting phase.** [`crate::cancel::begin_quit`] cancels the
//!    project's in-flight daemon jobs (poll/launch threads) and makes any job
//!    registered from here on start out cancelled, so a supervisor can't relaunch
//!    what the later steps are ending.
//! 3. **Mark the project(s) closed.** Every session process runs a watchdog that
//!    restarts a crashed daemon *only while its project is open*
//!    (`shelbi_session::daemon_watchdog`). Clearing the open flag before any
//!    session ends is what makes "quit Shelbi … and nothing restarts it" hold:
//!    a session shutting down sees `open == false` and stays out of the way.
//! 4. **Drain the quit barrier.** Wait for the cancelled jobs to acknowledge, so
//!    "quit" means "no job for this project is still running." This precedes the
//!    kills so a launch that passed its last check just before step 2 has
//!    finished spawning by the time step 5 lists what's live.
//! 5. **End the sessions.** Best-effort kill of the matching session processes
//!    on the local hub and the snapshotted remote machines.
//!
//! While a composition runs it holds a [`QuitInFlight`] guard, taken *before*
//! step 1. The daemon's idle-exit monitor treats a quit in flight as activity
//! ([`quit_in_flight`]): marking the last open project closed would otherwise
//! look like "nothing open" while the quit is still waiting on the
//! orchestrator's handoff, and the daemon would exit mid-quit, orphaning the
//! very sessions step 5 was about to end.
//!
//! Session termination is behind the [`Sessions`] seam so the composition —
//! the ordering, the per-host and per-project scoping, the barrier wait — is unit-testable
//! without standing up real session processes. [`BackendSessions`] is the
//! production implementation over [`crate::session_backend::backend`].

use std::sync::atomic::{AtomicUsize, Ordering};

use shelbi_core::{Host, Project};

use crate::session_backend::{backend, KillReason};
use crate::session_process_backend::{session_name, SessionProcessBackend};

/// The session-termination seam the quit compositions drive. Hiding IO behind a
/// trait keeps the ordering/scoping logic testable without real sessions.
pub trait Sessions {
    /// The logical names of every live session on `host`. Names follow the
    /// backend scheme: `<project>/orch`, `<project>/ws/<ws>`, and `w-<ws>/ws/agent`
    /// for an agent on a remote machine. An unreachable host yields none.
    fn live_names(&self, host: &Host) -> Vec<String>;
    /// Best-effort end the session named `name` on `host` (kill its child's
    /// process group). An already-gone session is fine.
    fn kill(&self, host: &Host, name: &str);
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

/// One remote machine a quit reaches, and which sessions on it the quit owns.
#[derive(Debug, Clone, PartialEq, Eq)]
struct HostSweep {
    host: Host,
    /// The quitting projects that have this machine: a session named under one
    /// of them is theirs.
    projects: Vec<String>,
    /// The names of those projects' workspace agents on this machine. Remote
    /// agents are named `w-<ws>/ws/agent`, with no project in the name
    /// ([`crate::workspace::workspace_target`]), so the name alone can't place
    /// them.
    agents: Vec<String>,
}

impl HostSweep {
    /// Whether the session `name` on this machine belongs to the quit. A machine
    /// can serve projects (or a hub of its own) that aren't being quit.
    fn owns(&self, name: &str) -> bool {
        self.projects.iter().any(|p| belongs_to_project(name, p))
            || self.agents.iter().any(|agent| agent == name)
    }

    /// Add `project`'s claim on the machine `host`, keeping one entry per host.
    fn merge(sweeps: &mut Vec<HostSweep>, host: Host, project: &str, agents: Vec<String>) {
        let sweep = match sweeps.iter().position(|s| s.host == host) {
            Some(at) => &mut sweeps[at],
            None => {
                sweeps.push(HostSweep {
                    host,
                    projects: Vec::new(),
                    agents: Vec::new(),
                });
                sweeps.last_mut().expect("just pushed")
            }
        };
        sweep.projects.push(project.to_string());
        sweep.agents.extend(agents);
    }
}

/// The remote machines of `projects` and what a quit of them owns there.
fn remote_sweeps(projects: &[String]) -> Vec<HostSweep> {
    let mut sweeps = Vec::new();
    for name in projects {
        match shelbi_state::load_project(name) {
            Ok(project) => add_project_sweeps(&mut sweeps, &project),
            Err(e) => tracing::warn!(
                project = %name,
                error = %e,
                "can't read the project's machines; a quit will not reach its remote sessions",
            ),
        }
    }
    sweeps
}

fn add_project_sweeps(sweeps: &mut Vec<HostSweep>, project: &Project) {
    for machine in &project.machines {
        let host = machine.host();
        if !host.is_ssh() {
            continue;
        }
        let agents = project
            .workspaces
            .iter()
            .filter(|ws| ws.machine == machine.name)
            .filter_map(|ws| crate::workspace::workspace_target(project, ws).ok())
            .map(|target| session_name(&target))
            .collect();
        HostSweep::merge(sweeps, host, &project.name, agents);
    }
}

/// End the sessions a quit owns: the local ones `owned_locally` picks, then each
/// remote machine's own. Returns the names asked to end, sorted and deduplicated
/// (one name can live on two machines).
fn end_sessions(
    sessions: &dyn Sessions,
    owned_locally: impl Fn(&str) -> bool,
    remote: &[HostSweep],
) -> Vec<String> {
    let mut ended = Vec::new();
    let mut sweep = |host: &Host, owned: &dyn Fn(&str) -> bool| {
        for name in sessions.live_names(host) {
            if owned(&name) {
                sessions.kill(host, &name);
                ended.push(name);
            }
        }
    };
    sweep(&Host::Local, &owned_locally);
    for machine in remote {
        sweep(&machine.host, &|name| machine.owns(name));
    }
    ended.sort();
    ended.dedup();
    ended
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
/// the tmux quit does), then drains the quit barrier and ends its sessions.
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

    // 1. The machines to sweep.
    let remote = remote_sweeps(&[project.to_string()]);

    // 2. Nothing new starts for this project from here on.
    let quitting = crate::cancel::begin_quit(project);

    // 3. Mark closed (watchdog contract above).
    let _ = shelbi_state::set_project_open(project, false);
    let _ = shelbi_state::append_project_event(project, "closed", "user:quit-project");

    // Handoff (production only) while the orchestrator session is still live.
    before_end_sessions();

    // 4. Drain the barrier for this project's in-flight daemon jobs.
    let drained = quitting.wait();

    // 5. End only this project's sessions.
    let ended = end_sessions(sessions, |name| belongs_to_project(name, project), &remote);

    QuitProjectOutcome { ended, drained }
}

/// Quit Shelbi (production). See [`quit_shelbi_with`]. Does **not** stop the
/// daemon itself — the caller (the daemon's control handler) stops after the
/// client is acknowledged, so the shutdown doesn't race the reply.
pub fn quit_shelbi() -> QuitShelbiOutcome {
    quit_shelbi_with(&BackendSessions)
}

/// Quit Shelbi against an injected [`Sessions`]: mark every open project closed
/// (so no watchdog resurrects the daemon), drain each project's quit barrier,
/// and end every local session plus the open projects' sessions on their remote
/// machines. Other sessions on those machines aren't ours to end: a machine can
/// run a hub of its own.
pub fn quit_shelbi_with(sessions: &dyn Sessions) -> QuitShelbiOutcome {
    let _in_flight = QuitInFlight::enter();

    // 1. The machines to sweep, while the open set still names them.
    let open = shelbi_state::list_open_projects().unwrap_or_default();
    let remote = remote_sweeps(&open);

    // 2. Nothing new starts for any of these projects from here on.
    let quitting: Vec<_> = open.iter().map(|p| crate::cancel::begin_quit(p)).collect();

    // 3. Close every open project.
    for project in &open {
        let _ = shelbi_state::set_project_open(project, false);
        let _ = shelbi_state::append_project_event(project, "closed", "user:quit-shelbi");
    }

    // 4. Drain every closed project's barrier.
    let mut drained = true;
    for phase in &quitting {
        drained &= phase.wait();
    }

    // 5. End the sessions.
    let ended = end_sessions(sessions, |_| true, &remote);

    QuitShelbiOutcome {
        closed: open,
        ended,
        drained,
    }
}

/// The production [`Sessions`]: enumerates and kills session processes through
/// the session backend.
pub struct BackendSessions;

impl Sessions for BackendSessions {
    fn live_names(&self, host: &Host) -> Vec<String> {
        backend().live_pane_ids(host).unwrap_or_else(|e| {
            tracing::warn!(?host, error = %e, "quit can't list this host's sessions; they keep running");
            Vec::new()
        })
    }

    fn kill(&self, host: &Host, name: &str) {
        // A reopen resumes these slots; the reason tells any supervisor pass that
        // still gets a tick before the poller stops that this is not a crash.
        backend().record_kill_reason(host, name, KillReason::Quit);
        // An interactive shell ignores the default SIGTERM; everything else ends on it.
        let signal = crate::workspace_session::is_user_shell_session(name)
            .then_some(crate::workspace_session::SIGHUP);
        if let Err(e) = SessionProcessBackend.signal_by_name(host, name, signal) {
            tracing::warn!(?host, session = name, error = %e, "quit couldn't end this session");
        }
    }
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

    fn ssh(host: &str) -> Host {
        Host::Ssh { host: host.into() }
    }

    /// A [`Sessions`] fake over a fixed set of live names per host, recording
    /// which hosts were asked and every kill.
    struct FakeSessions {
        live: Vec<(Host, String)>,
        swept: Mutex<Vec<Host>>,
        killed: Mutex<Vec<(Host, String)>>,
    }
    impl FakeSessions {
        /// Live sessions on the local hub.
        fn new(live: &[&str]) -> Self {
            Self {
                live: live.iter().map(|s| (Host::Local, s.to_string())).collect(),
                swept: Mutex::new(Vec::new()),
                killed: Mutex::new(Vec::new()),
            }
        }
        /// Add live sessions on a remote machine.
        fn on(mut self, host: &str, live: &[&str]) -> Self {
            self.live.extend(live.iter().map(|s| (ssh(host), s.to_string())));
            self
        }
        fn swept(&self) -> Vec<Host> {
            self.swept.lock().unwrap().clone()
        }
        fn killed(&self) -> Vec<String> {
            self.killed.lock().unwrap().iter().map(|(_, n)| n.clone()).collect()
        }
        fn killed_on(&self, host: &Host) -> Vec<String> {
            let killed = self.killed.lock().unwrap();
            killed.iter().filter(|(h, _)| h == host).map(|(_, n)| n.clone()).collect()
        }
    }
    impl Sessions for FakeSessions {
        fn live_names(&self, host: &Host) -> Vec<String> {
            self.swept.lock().unwrap().push(host.clone());
            self.live.iter().filter(|(h, _)| h == host).map(|(_, n)| n.clone()).collect()
        }
        fn kill(&self, host: &Host, name: &str) {
            self.killed.lock().unwrap().push((host.clone(), name.to_string()));
        }
    }

    /// Give a [`Home`] project an SSH machine `host` with the workspaces `ws`
    /// on it (alongside its local machine).
    fn give_ssh_machine(project: &str, host: &str, ws: &[&str]) {
        let mut p = shelbi_state::load_project(project).unwrap();
        p.machines.push(shelbi_core::Machine {
            name: host.into(),
            kind: shelbi_core::MachineKind::Ssh,
            work_dir: "/srv/work".into(),
            host: Some(host.into()),
            tags: Vec::new(),
            forward: None,
        });
        for name in ws {
            let mut spec = p.workspaces[0].clone();
            spec.name = (*name).into();
            spec.machine = host.into();
            p.workspaces.push(spec);
        }
        shelbi_state::save_project(&p).unwrap();
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
        fn live_names(&self, host: &Host) -> Vec<String> {
            if host.is_local() { self.live.clone() } else { Vec::new() }
        }
        fn kill(&self, _host: &Host, _name: &str) {
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

    #[test]
    fn quit_project_sweeps_its_ssh_machine_and_kills_its_sessions_there() {
        // The bug: the sweep read its hosts from the open set after the project
        // left it, so the quitting project's machines were never asked. And its
        // agents there are named `w-<ws>/ws/agent`, which no project prefix
        // matches, so listing the host alone wouldn't have reached them.
        let home = Home::new("qp-remote", &["alpha", "beta"]);
        give_ssh_machine("alpha", "devbox", &["delta"]);
        give_ssh_machine("beta", "buildbox", &["echo"]);
        shelbi_state::set_project_open("alpha", true).unwrap();
        shelbi_state::set_project_open("beta", true).unwrap();

        let sessions = FakeSessions::new(&["alpha/orch", "beta/orch"])
            .on(
                "devbox",
                &["alpha/ws/delta/shell", "w-delta/ws/agent", "gamma/orch", "w-zulu/ws/agent"],
            )
            .on("buildbox", &["w-echo/ws/agent"]);

        let out = quit_project_with("alpha", &sessions);

        assert_eq!(
            sessions.swept(),
            vec![Host::Local, ssh("devbox")],
            "the project's own machine is swept, and only that one",
        );
        assert_eq!(
            sessions.killed_on(&ssh("devbox")),
            vec!["alpha/ws/delta/shell", "w-delta/ws/agent"],
            "this project's sessions end; a stranger's on the same machine don't",
        );
        assert_eq!(sessions.killed_on(&Host::Local), vec!["alpha/orch"]);
        assert_eq!(out.ended, vec!["alpha/orch", "alpha/ws/delta/shell", "w-delta/ws/agent"]);
        assert!(shelbi_state::is_project_open("beta").unwrap());
        drop(home);
    }

    #[test]
    fn quit_project_only_kills_an_agents_name_on_the_machine_it_runs_on() {
        // alpha's `delta` agent is on devbox. A `w-delta/ws/agent` on alpha's
        // other machine is another project's workspace of the same name.
        let home = Home::new("qp-same-name", &["alpha"]);
        give_ssh_machine("alpha", "devbox", &["delta"]);
        give_ssh_machine("alpha", "buildbox", &[]);
        shelbi_state::set_project_open("alpha", true).unwrap();

        let sessions = FakeSessions::new(&[])
            .on("devbox", &["w-delta/ws/agent"])
            .on("buildbox", &["w-delta/ws/agent"]);
        quit_project_with("alpha", &sessions);

        assert_eq!(sessions.swept(), vec![Host::Local, ssh("devbox"), ssh("buildbox")]);
        assert_eq!(sessions.killed_on(&ssh("devbox")), vec!["w-delta/ws/agent"]);
        assert!(sessions.killed_on(&ssh("buildbox")).is_empty());
        drop(home);
    }

    #[test]
    fn quit_shelbi_kills_remote_sessions_for_every_open_project() {
        // Quit Shelbi closed everything first, so its sweep found no hosts at all.
        let home = Home::new("qs-remote", &["one", "two", "closed"]);
        give_ssh_machine("one", "devbox", &["delta"]);
        give_ssh_machine("two", "devbox", &["echo"]);
        give_ssh_machine("two", "buildbox", &["foxtrot"]);
        give_ssh_machine("closed", "oldbox", &["golf"]);
        shelbi_state::set_project_open("one", true).unwrap();
        shelbi_state::set_project_open("two", true).unwrap();

        let sessions = FakeSessions::new(&["one/orch", "unrelated/orch"])
            .on(
                "devbox",
                &["one/ws/delta/shell", "w-delta/ws/agent", "w-echo/ws/agent", "theirs/orch"],
            )
            .on("buildbox", &["w-foxtrot/ws/agent"])
            .on("oldbox", &["w-golf/ws/agent"]);

        let out = quit_shelbi_with(&sessions);

        assert_eq!(
            sessions.swept(),
            vec![Host::Local, ssh("devbox"), ssh("buildbox")],
            "every open project's machines are swept, each once",
        );
        assert_eq!(
            sessions.killed_on(&ssh("devbox")),
            vec!["one/ws/delta/shell", "w-delta/ws/agent", "w-echo/ws/agent"],
            "a session on the machine that no open project owns is left alone",
        );
        assert_eq!(sessions.killed_on(&ssh("buildbox")), vec!["w-foxtrot/ws/agent"]);
        assert_eq!(
            sessions.killed_on(&Host::Local),
            vec!["one/orch", "unrelated/orch"],
            "every session on the hub still ends",
        );
        assert_eq!(out.closed, vec!["one", "two"]);
        assert!(shelbi_state::list_open_projects().unwrap().is_empty());
        drop(home);
    }

    /// A [`Sessions`] fake standing in for a supervisor that sees a session die
    /// and relaunches, at each of the moments a quit could lose that race.
    struct RelaunchRacer {
        project: String,
        registered_cancelled: Mutex<Vec<bool>>,
    }
    impl RelaunchRacer {
        fn race(&self) {
            let launch = crate::cancel::register(
                &self.project,
                Some("dev"),
                crate::cancel::JobKind::Launch,
            );
            self.registered_cancelled.lock().unwrap().push(launch.is_cancelled());
        }
    }
    impl Sessions for RelaunchRacer {
        fn live_names(&self, _host: &Host) -> Vec<String> {
            self.race();
            vec![format!("{}/ws/dev", self.project)]
        }
        fn kill(&self, _host: &Host, _name: &str) {
            self.race();
        }
    }

    #[test]
    fn a_relaunch_during_a_quit_is_cancelled_immediately_and_a_reopen_is_not() {
        let home = Home::new("qp-relaunch", &["raced"]);
        shelbi_state::set_project_open("raced", true).unwrap();
        let sessions = RelaunchRacer {
            project: "raced".into(),
            registered_cancelled: Mutex::new(Vec::new()),
        };

        let out = quit_project_inner("raced", &sessions, || sessions.race());

        assert_eq!(
            *sessions.registered_cancelled.lock().unwrap(),
            vec![true, true, true],
            "a launch registered during the handoff, the listing or the kill starts cancelled",
        );
        assert!(out.drained);

        // The phase ends with the quit, so reopening the project can launch.
        shelbi_state::set_project_open("raced", true).unwrap();
        let launch = crate::cancel::register("raced", Some("dev"), crate::cancel::JobKind::Launch);
        assert!(launch.is_current());
        drop(launch);
        drop(home);
    }

    /// Records whether the project's jobs had drained when its sessions ended.
    struct LiveJobsAtKill {
        project: String,
        at_kill: Mutex<Vec<usize>>,
    }
    impl Sessions for LiveJobsAtKill {
        fn live_names(&self, host: &Host) -> Vec<String> {
            if host.is_local() { vec![format!("{}/orch", self.project)] } else { Vec::new() }
        }
        fn kill(&self, _host: &Host, _name: &str) {
            self.at_kill
                .lock()
                .unwrap()
                .push(crate::cancel::live_job_count(&self.project));
        }
    }

    #[test]
    fn a_quit_drains_jobs_before_ending_sessions() {
        // A launch already past its last check when the quit began has to finish
        // spawning before the kill lists what's live, or its session survives.
        let home = Home::new("qp-order", &["ordered"]);
        shelbi_state::set_project_open("ordered", true).unwrap();
        let guard = crate::cancel::register("ordered", None, crate::cancel::JobKind::Launch);
        let cancelled = guard.cancel_flag();
        let worker = std::thread::spawn(move || {
            while !cancelled.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(2));
            }
            drop(guard);
        });

        let sessions = LiveJobsAtKill {
            project: "ordered".into(),
            at_kill: Mutex::new(Vec::new()),
        };
        let out = quit_project_with("ordered", &sessions);
        worker.join().unwrap();

        assert!(out.drained);
        assert_eq!(*sessions.at_kill.lock().unwrap(), vec![0]);
        drop(home);
    }
}
