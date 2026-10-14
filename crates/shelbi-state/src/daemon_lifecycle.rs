//! Client-side on-demand daemon lifecycle.
//!
//! The hub daemon is started on demand and exits when no project is open (the
//! launchd/systemd units are retired). This module is the client half: the
//! shared `ensure_daemon_running` that any client calls before it needs the
//! daemon, a root-scoped `stop_daemon` used by `shelbi daemon stop`/`restart`, and the
//! single-instance lock helpers shared with the daemon's own bind lock.
//!
//! See `docs/removing-tmux/phase3-daemon.md` (Lifecycle).

use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{hub_socket_path, probe_daemon_hello, DaemonProbe};
use shelbi_core::{Error, Result};

/// How long [`ensure_daemon_running`] waits for a freshly spawned daemon to
/// bind its socket before giving up. Startup is sub-second in practice; this is
/// generous headroom for a loaded host.
const START_DEADLINE: Duration = Duration::from_secs(10);

/// How long [`stop_daemon`] waits for a signalled daemon to release its lock
/// and exit. The daemon's graceful drain is a few seconds (`SHUTDOWN_DRAIN_
/// TIMEOUT`); this leaves margin before falling back to a stronger signal.
const STOP_DEADLINE: Duration = Duration::from_secs(8);

/// Poll granularity for the two deadlines above.
const POLL_SLICE: Duration = Duration::from_millis(100);

/// The daemon's single-instance lock file (`<hub.sock>.lock`). The daemon holds
/// an exclusive `flock` on it for its whole lifetime; a client uses
/// [`daemon_lock_held`] to tell whether a daemon is running or starting.
pub fn hub_lock_path() -> Result<PathBuf> {
    let sock = hub_socket_path()?;
    let mut os = sock.into_os_string();
    os.push(".lock");
    Ok(PathBuf::from(os))
}

/// Whether the daemon's single-instance lock is currently held — i.e. a daemon
/// is running or mid-startup.
///
/// Opens the lock file and attempts a non-blocking exclusive `flock`. Success
/// means no one holds it (we immediately release), so the answer is `false`;
/// `EWOULDBLOCK` means a daemon holds it, so the answer is `true`. Any other
/// error (the file can't be opened) conservatively reads as "not held" so a
/// client still tries to start a daemon rather than wrongly assuming one is up.
pub fn daemon_lock_held() -> bool {
    let Ok(path) = hub_lock_path() else {
        return false;
    };
    let file = match std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    // SAFETY: flock on a valid fd we own; no memory is dereferenced.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        // We acquired it, so no daemon holds it. Release before returning so we
        // don't block the daemon we may be about to spawn.
        // SAFETY: same fd, still open and owned.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        false
    } else {
        // Any failure to acquire (EWOULDBLOCK and friends) means a live holder.
        true
    }
}

/// Whether the hub socket currently answers — a daemon is listening.
fn socket_answers() -> bool {
    !matches!(probe_daemon_hello(), DaemonProbe::NotRunning)
}

/// Ensure a hub daemon is running, starting one on demand if not.
///
/// 1. Return immediately if the socket already answers.
/// 2. Otherwise, if the single-instance lock is free, spawn `shelbi daemon`
///    detached.
/// 3. Wait for the socket to answer, up to [`START_DEADLINE`].
///
/// Safe when several clients race: the daemon takes the single-instance lock
/// for its whole lifetime at startup, so if two clients both spawn a daemon,
/// exactly one wins the bind and the losers exit without touching the live
/// socket. Every caller then observes the one surviving daemon.
pub fn ensure_daemon_running() -> Result<()> {
    if socket_answers() {
        return Ok(());
    }
    // No daemon answering. If the lock is free, no daemon is starting either —
    // spawn one. If it's held, a daemon is mid-startup; just wait for it. A
    // redundant spawn is harmless (the daemon's bind lock dedups), so the lock
    // check is an optimization, not a correctness gate.
    if !daemon_lock_held() {
        spawn_daemon_detached()?;
    }
    wait_for_socket(START_DEADLINE)
}

/// Launch `shelbi daemon` as a detached background process in its own session,
/// so it outlives the launching client and a launching `ssh`/terminal does not
/// hang on its stdio.
fn spawn_daemon_detached() -> Result<()> {
    let exe = std::env::current_exe().map_err(Error::Io)?;
    let mut cmd = Command::new(exe);
    cmd.arg("daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach into a new session so a quitting client's process-group kill (and
    // the controlling terminal going away) never reaches the daemon.
    // SAFETY: `setsid` is async-signal-safe and touches no shared state; the
    // closure runs in the forked child before exec.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                // ESRCH/EPERM only when we're already a group leader, which a
                // freshly forked child is not; ignore so the spawn still
                // succeeds rather than failing the whole start.
            }
            Ok(())
        });
    }
    cmd.spawn().map_err(Error::Io)?;
    Ok(())
}

/// Block until the hub socket answers or `deadline` elapses.
fn wait_for_socket(deadline: Duration) -> Result<()> {
    let start = Instant::now();
    loop {
        if socket_answers() {
            return Ok(());
        }
        if start.elapsed() >= deadline {
            return Err(Error::Other(format!(
                "hub daemon did not start within {}s (check `shelbi daemon status`)",
                deadline.as_secs()
            )));
        }
        std::thread::sleep(POLL_SLICE);
    }
}

/// What [`stop_daemon`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopOutcome {
    /// No daemon is running under this root (its lock is free and its socket
    /// is silent). Nothing was signalled.
    NotRunning,
    /// This root's recorded daemon `pid` was signalled and has exited.
    Stopped { pid: libc::pid_t },
    /// A daemon appears to be up, but this root's `shelbi.pid` doesn't identify
    /// a process that is safe to signal. Nothing was signalled; `reason` says
    /// why and what to fix.
    Refused { reason: String },
}

/// The pure decision [`stop_daemon`] makes before it sends any signal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum StopDecision {
    Nothing,
    Signal(libc::pid_t),
    Refuse(String),
}

/// Decide which pid (if any) a stop may signal.
///
/// * `up` — this root's daemon lock is held or its hub socket answers.
/// * `record` — this root's `shelbi.pid`: the recorded pid and whether it is
///   provably that same process still ([`crate::recorded_daemon_is_same_process`]).
/// * `answering` — the pid the hub socket's hello reported, when it did.
///
/// The only pid that is ever signalled is the one recorded under **this** root,
/// and only when nothing contradicts it: a recycled/stale record or a socket
/// answered by a different pid (the socket env and the root disagree, e.g. a
/// sandbox `SHELBI_ROOT` with the pane's real `SHELBI_HUB_SOCK` still set) is
/// refused rather than guessed at.
fn decide_stop(
    up: bool,
    record: Option<(libc::pid_t, bool)>,
    answering: Option<u32>,
    pid_file: &std::path::Path,
) -> StopDecision {
    if !up {
        return StopDecision::Nothing;
    }
    let Some((pid, same_process)) = record else {
        return StopDecision::Refuse(format!(
            "a hub daemon is up, but {} records no pid to signal; \
             if it is still starting, retry in a moment",
            pid_file.display()
        ));
    };
    if !same_process {
        return StopDecision::Refuse(format!(
            "{} records pid {pid}, which is no longer that daemon (stale or recycled pid); \
             refusing to signal it",
            pid_file.display()
        ));
    }
    if let Some(answering) = answering {
        if answering as libc::pid_t != pid {
            return StopDecision::Refuse(format!(
                "the hub socket is answered by pid {answering}, but {} records pid {pid}; \
                 the socket env (SHELBI_HUB_SOCK) and the root disagree, refusing to signal either",
                pid_file.display()
            ));
        }
    }
    StopDecision::Signal(pid)
}

/// Stop this root's hub daemon and wait for it to exit. Used by `shelbi daemon
/// stop` and `shelbi daemon restart`.
///
/// Root-scoped by construction: the only pid it ever signals is the one in
/// **this root's** `shelbi.pid` (`$SHELBI_ROOT`/`$SHELBI_HOME`/`--root`), and
/// only after [`decide_stop`] finds nothing contradicting it — the record must
/// still name the same process (start-time identity), and when the hub socket
/// answers, it must answer as that same pid. So a stop under a sandbox root can
/// never reach the live hub's daemon, even with the pane's real hub-socket env
/// leaking in. (Stopping a daemon is not fatal for in-flight work in any case:
/// session hosts run in their own sessions, outlive the daemon, and a restarted
/// daemon finds them on disk.)
///
/// Sends SIGTERM (the daemon drains and exits on it), then waits up to
/// [`STOP_DEADLINE`] for the process to go away or its single-instance lock to
/// release. A SIGKILL fallback fires only if that same process is still there.
pub fn stop_daemon() -> Result<StopOutcome> {
    let up = daemon_lock_held() || socket_answers();
    if !up {
        // Nothing under this root: the fast path a sandbox stop takes, which
        // never even reads a pid.
        return Ok(StopOutcome::NotRunning);
    }
    let record = crate::read_daemon_pid_record()?;
    let answering = match probe_daemon_hello() {
        DaemonProbe::Hello(hello) => hello.pid,
        _ => None,
    };
    let pid_file = crate::daemon_pid_file_path()?;
    let pid = match decide_stop(
        up,
        record
            .as_ref()
            .map(|r| (r.pid, crate::recorded_daemon_is_same_process(r))),
        answering,
        &pid_file,
    ) {
        StopDecision::Nothing => return Ok(StopOutcome::NotRunning),
        StopDecision::Refuse(reason) => return Ok(StopOutcome::Refused { reason }),
        StopDecision::Signal(pid) => pid,
    };
    let record = record.expect("decide_stop signals only a recorded pid");
    let still_there = || crate::recorded_daemon_is_same_process(&record);

    // SAFETY: kill with a real signal to the pid this root recorded, verified
    // above to still be that process; only affects that process.
    unsafe { libc::kill(pid, libc::SIGTERM) };

    let start = Instant::now();
    while still_there() && daemon_lock_held() {
        if start.elapsed() >= STOP_DEADLINE {
            // The drain wedged. Escalate once so a restart isn't blocked
            // forever by a stuck daemon — re-checking identity first.
            if still_there() {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
            break;
        }
        std::thread::sleep(POLL_SLICE);
    }
    // Give a SIGKILLed daemon a final moment to let the lock die with it.
    let kill_deadline = Instant::now() + Duration::from_secs(2);
    while still_there() && daemon_lock_held() && Instant::now() < kill_deadline {
        std::thread::sleep(POLL_SLICE);
    }
    Ok(StopOutcome::Stopped { pid })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use std::os::unix::net::UnixListener;

    /// Point `SHELBI_HUB_SOCK` at a short, unique path for a test, restoring it
    /// on drop. macOS caps Unix-socket paths at ~104 bytes, so the path stays
    /// under `/tmp`. Callers hold [`LOCK`] for the guard's lifetime so the
    /// process-wide env var isn't mutated under a parallel test.
    struct SockGuard {
        prev: Option<std::ffi::OsString>,
        path: PathBuf,
    }
    impl SockGuard {
        fn new(tag: &str) -> Self {
            let prev = std::env::var_os("SHELBI_HUB_SOCK");
            let path = PathBuf::from(format!("/tmp/shb-dl-{tag}-{}.sock", std::process::id()));
            let _ = std::fs::remove_file(&path);
            let mut lock = path.clone().into_os_string();
            lock.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(lock));
            std::env::set_var("SHELBI_HUB_SOCK", &path);
            Self { prev, path }
        }
    }
    impl Drop for SockGuard {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
            let mut lock = self.path.clone().into_os_string();
            lock.push(".lock");
            let _ = std::fs::remove_file(PathBuf::from(lock));
            match &self.prev {
                Some(v) => std::env::set_var("SHELBI_HUB_SOCK", v),
                None => std::env::remove_var("SHELBI_HUB_SOCK"),
            }
        }
    }

    #[test]
    fn lock_path_is_the_socket_plus_lock_suffix() {
        let _l = LOCK.lock().unwrap();
        let _g = SockGuard::new("lockpath");
        let sock = hub_socket_path().unwrap();
        let lock = hub_lock_path().unwrap();
        assert_eq!(lock, PathBuf::from(format!("{}.lock", sock.display())));
    }

    #[test]
    fn daemon_lock_held_tracks_an_exclusive_flock() {
        let _l = LOCK.lock().unwrap();
        let _g = SockGuard::new("held");
        // Nobody holds it yet.
        assert!(!daemon_lock_held(), "fresh lock must read as free");

        // Simulate a daemon holding the lock for its lifetime.
        let path = hub_lock_path().unwrap();
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        assert_eq!(rc, 0, "test must acquire the lock");
        assert!(daemon_lock_held(), "a held lock must read as held");

        // Releasing it flips the answer back.
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        assert!(!daemon_lock_held(), "released lock must read as free");
    }

    #[test]
    fn stop_daemon_is_a_noop_when_nothing_is_running() {
        let _l = LOCK.lock().unwrap();
        let _g = SockGuard::new("stopnoop");
        assert_eq!(
            stop_daemon().unwrap(),
            StopOutcome::NotRunning,
            "no daemon → nothing stopped"
        );
    }

    fn pid_file() -> PathBuf {
        PathBuf::from("/tmp/sandbox/shelbi.pid")
    }

    #[test]
    fn decide_stop_signals_nothing_when_this_root_has_no_daemon() {
        // A sandbox root with a free lock and a silent socket never even looks
        // at a pid — whatever another root's daemon is doing.
        assert_eq!(decide_stop(false, None, None, &pid_file()), StopDecision::Nothing);
        assert_eq!(
            decide_stop(false, Some((4242, true)), Some(4242), &pid_file()),
            StopDecision::Nothing
        );
    }

    #[test]
    fn decide_stop_signals_only_this_roots_verified_pid() {
        assert_eq!(
            decide_stop(true, Some((4242, true)), Some(4242), &pid_file()),
            StopDecision::Signal(4242)
        );
        // An older daemon whose hello carries no pid: the verified record alone.
        assert_eq!(
            decide_stop(true, Some((4242, true)), None, &pid_file()),
            StopDecision::Signal(4242)
        );
    }

    #[test]
    fn decide_stop_refuses_when_the_socket_and_the_root_disagree() {
        // Sandbox root (its record names its own daemon) but the pane's real
        // SHELBI_HUB_SOCK still points at the live hub, which answers as a
        // different pid: signal neither.
        let d = decide_stop(true, Some((4242, true)), Some(1111), &pid_file());
        let StopDecision::Refuse(reason) = d else {
            panic!("expected a refusal, got {d:?}");
        };
        assert!(reason.contains("1111") && reason.contains("4242"), "{reason}");
        assert!(reason.contains("SHELBI_HUB_SOCK"), "{reason}");
    }

    #[test]
    fn decide_stop_refuses_a_stale_or_recycled_record() {
        let d = decide_stop(true, Some((4242, false)), None, &pid_file());
        assert!(matches!(d, StopDecision::Refuse(ref r) if r.contains("recycled")), "{d:?}");
        let d = decide_stop(true, None, Some(4242), &pid_file());
        assert!(matches!(d, StopDecision::Refuse(ref r) if r.contains("no pid")), "{d:?}");
    }

    #[test]
    fn socket_answers_reflects_a_listener() {
        let _l = LOCK.lock().unwrap();
        let _g = SockGuard::new("answers");
        assert!(!socket_answers(), "no listener yet");
        let sock = hub_socket_path().unwrap();
        let _listener = UnixListener::bind(&sock).unwrap();
        assert!(socket_answers(), "a bound listener answers the probe");
    }
}
