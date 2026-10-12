//! Client-side on-demand daemon lifecycle.
//!
//! The hub daemon is started on demand and exits when no project is open (the
//! launchd/systemd units are retired). This module is the client half: the
//! shared `ensure_daemon_running` that any client calls before it needs the
//! daemon, a `stop_daemon` used by `shelbi daemon restart`, and the
//! single-instance lock helpers shared with the daemon's own bind lock.
//!
//! See `docs/removing-tmux/phase3-daemon.md` (Lifecycle).

use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{
    hub_socket_path, is_process_alive, probe_daemon_hello, read_daemon_pid, DaemonProbe,
};
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

/// Stop a running hub daemon and wait for it to exit, returning whether one was
/// signalled. Used by `shelbi daemon restart`, which then starts a fresh daemon
/// on the current binary.
///
/// Sends SIGTERM to the PID in the daemon PID record (the daemon drains and
/// exits on it), then waits up to [`STOP_DEADLINE`] for the single-instance
/// lock to release — the authoritative "it's gone" signal, since the lock dies
/// with the holder. A SIGKILL fallback fires if the drain wedges past the
/// deadline. Returns `Ok(false)` when no daemon was running.
pub fn stop_daemon() -> Result<bool> {
    if !daemon_lock_held() && !socket_answers() {
        return Ok(false);
    }
    let Some(pid) = read_daemon_pid()? else {
        // Lock held but no PID on file: an older or mid-startup daemon we can't
        // signal by PID. Report nothing stopped; the caller's start path still
        // converges on one daemon via the bind lock.
        return Ok(false);
    };
    if !is_process_alive(pid) {
        return Ok(false);
    }
    // SAFETY: kill with a real signal to a pid we read; only affects that process.
    unsafe { libc::kill(pid, libc::SIGTERM) };

    let start = Instant::now();
    while daemon_lock_held() {
        if start.elapsed() >= STOP_DEADLINE {
            // The drain wedged. Escalate once so a restart isn't blocked
            // forever by a stuck daemon.
            unsafe { libc::kill(pid, libc::SIGKILL) };
            break;
        }
        std::thread::sleep(POLL_SLICE);
    }
    // Give a SIGKILLed daemon a final moment to let the lock die with it.
    let kill_deadline = Instant::now() + Duration::from_secs(2);
    while daemon_lock_held() && Instant::now() < kill_deadline {
        std::thread::sleep(POLL_SLICE);
    }
    Ok(true)
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
        assert!(!stop_daemon().unwrap(), "no daemon → nothing stopped");
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
