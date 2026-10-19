//! Client-side on-demand daemon lifecycle.
//!
//! The hub daemon is started on demand and exits when no project is open (the
//! launchd/systemd units are retired). This module is the client half: the
//! shared `ensure_daemon_running` that any client calls before it needs the
//! daemon, a root-scoped `stop_daemon` used by `shelbi daemon stop`/`restart`, and the
//! single-instance lock helpers shared with the daemon's own bind lock.
//!
//! See `docs/removing-tmux/phase3-daemon.md` (Lifecycle).

use std::collections::BTreeMap;
use std::os::unix::io::AsRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::RwLock;
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

/// How a client launches `shelbi daemon` when none is running.
///
/// The default (no launcher installed, see [`set_daemon_launcher`]) re-runs the
/// current executable, which is right for the `shelbi` binary itself and the
/// TUI it hosts. A client that is **not** the `shelbi` binary (the desktop app)
/// installs a launcher naming the CLI it discovered, so every on-demand start in
/// that process, including the ones buried in shared code like
/// `shelbi_app::execute_mutation`, runs `<cli> daemon` instead of re-running
/// the client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonLauncher {
    /// The `shelbi` executable to run as `<exe> daemon`.
    pub exe: PathBuf,
    /// The daemon's complete environment (applied after `env_clear`), or
    /// `None` to inherit the caller's.
    pub env: Option<BTreeMap<String, String>>,
    /// Where the daemon's stderr goes (truncated on each start), so a daemon
    /// that dies during startup leaves its reason behind. `None` discards it.
    pub stderr_log: Option<PathBuf>,
}

/// The process-wide launcher [`ensure_daemon_running`] uses; `None` = re-run
/// the current executable.
static LAUNCHER: RwLock<Option<DaemonLauncher>> = RwLock::new(None);

/// Install the launcher every later [`ensure_daemon_running`] in this process
/// uses (replacing any earlier one).
pub fn set_daemon_launcher(launcher: DaemonLauncher) {
    *LAUNCHER.write().unwrap_or_else(|p| p.into_inner()) = Some(launcher);
}

/// Why [`ensure_daemon_running_with`] could not bring a daemon up.
#[derive(Debug)]
pub enum DaemonStartError {
    /// The daemon executable could not be spawned at all.
    Spawn(std::io::Error),
    /// The daemon we spawned exited before any daemon answered the socket.
    /// `status` is its exit status as text (`exit status: 1`).
    Exited { status: String },
    /// Nothing answered the socket within the deadline.
    Timeout { after: Duration },
}

impl std::fmt::Display for DaemonStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonStartError::Spawn(e) => write!(f, "could not run `shelbi daemon`: {e}"),
            DaemonStartError::Exited { status } => {
                write!(f, "`shelbi daemon` exited during startup ({status})")
            }
            DaemonStartError::Timeout { after } => write!(
                f,
                "hub daemon did not start within {}s (check `shelbi daemon status`)",
                after.as_secs()
            ),
        }
    }
}

impl std::error::Error for DaemonStartError {}

/// Ensure a hub daemon is running, starting one on demand if not.
///
/// 1. Return immediately if the socket already answers.
/// 2. Otherwise, if the single-instance lock is free, spawn `shelbi daemon`
///    detached (through the installed [`DaemonLauncher`], if any).
/// 3. Wait for the socket to answer, up to [`START_DEADLINE`].
///
/// Safe when several clients race: the daemon takes the single-instance lock
/// for its whole lifetime at startup, so if two clients both spawn a daemon,
/// exactly one wins the bind and the losers exit without touching the live
/// socket. Every caller then observes the one surviving daemon.
pub fn ensure_daemon_running() -> Result<()> {
    let installed = LAUNCHER.read().unwrap_or_else(|p| p.into_inner()).clone();
    let launcher = match installed {
        Some(l) => l,
        None => DaemonLauncher {
            exe: std::env::current_exe().map_err(Error::Io)?,
            env: None,
            stderr_log: None,
        },
    };
    ensure_daemon_running_with(&launcher, START_DEADLINE).map_err(|e| Error::Other(e.to_string()))
}

/// [`ensure_daemon_running`] with an explicit launcher and deadline, returning
/// a typed error so a client can render each failure (the desktop app's
/// "daemon failed to start" screen reads `launcher.stderr_log` on
/// [`DaemonStartError::Exited`] / [`DaemonStartError::Timeout`]).
///
/// A spawned daemon that exits while no daemon holds the lock or answers the
/// socket fails fast with [`DaemonStartError::Exited`] instead of waiting out
/// the deadline. (A daemon that loses a start race also exits, but then the
/// winner holds the lock, so the wait continues.)
pub fn ensure_daemon_running_with(
    launcher: &DaemonLauncher,
    deadline: Duration,
) -> std::result::Result<(), DaemonStartError> {
    if socket_answers() {
        return Ok(());
    }
    // No daemon answering. If the lock is free, no daemon is starting either —
    // spawn one. If it's held, a daemon is mid-startup; just wait for it. A
    // redundant spawn is harmless (the daemon's bind lock dedups), so the lock
    // check is an optimization, not a correctness gate.
    let child = if daemon_lock_held() {
        None
    } else {
        Some(spawn_daemon_detached(launcher).map_err(DaemonStartError::Spawn)?)
    };
    wait_for_socket(child, deadline)
}

/// Launch `<exe> daemon` as a detached background process in its own session,
/// so it outlives the launching client and a launching `ssh`/terminal does not
/// hang on its stdio.
fn spawn_daemon_detached(launcher: &DaemonLauncher) -> std::io::Result<Child> {
    let mut cmd = Command::new(&launcher.exe);
    cmd.arg("daemon").stdin(Stdio::null()).stdout(Stdio::null());
    match &launcher.stderr_log {
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            cmd.stderr(std::fs::File::create(path)?);
        }
        None => {
            cmd.stderr(Stdio::null());
        }
    }
    if let Some(env) = &launcher.env {
        cmd.env_clear().envs(env);
    }
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
    cmd.spawn()
}

/// Block until the hub socket answers or `deadline` elapses. `child` is the
/// daemon this call spawned, if any: its early exit ends the wait (see
/// [`ensure_daemon_running_with`]), and it is reaped either way so a
/// long-lived client never collects zombies.
fn wait_for_socket(
    mut child: Option<Child>,
    deadline: Duration,
) -> std::result::Result<(), DaemonStartError> {
    let start = Instant::now();
    let result = loop {
        if socket_answers() {
            break Ok(());
        }
        if let Some(c) = child.as_mut() {
            if let Ok(Some(status)) = c.try_wait() {
                child = None;
                if !daemon_lock_held() && !socket_answers() {
                    break Err(DaemonStartError::Exited {
                        status: status.to_string(),
                    });
                }
            }
        }
        if start.elapsed() >= deadline {
            break Err(DaemonStartError::Timeout { after: deadline });
        }
        std::thread::sleep(POLL_SLICE);
    };
    if let Some(mut c) = child {
        // The daemon runs for as long as projects are open; reap it off-thread
        // whenever it eventually exits.
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    }
    result
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

    /// A launcher running `sh -c <script> daemon` stand-in: `exe` is a script
    /// written to a temp dir.
    fn script_launcher(tag: &str, body: &str) -> (DaemonLauncher, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shb-dl-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("shelbi");
        std::fs::write(&exe, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let log = dir.join("daemon.stderr");
        (
            DaemonLauncher {
                exe,
                env: None,
                stderr_log: Some(log.clone()),
            },
            log,
        )
    }

    #[test]
    fn a_daemon_that_dies_at_startup_fails_fast_with_its_stderr_kept() {
        let _l = LOCK.lock().unwrap();
        let _g = SockGuard::new("dies");
        let (launcher, log) = script_launcher("dies", "echo \"boom: bad config\" >&2; exit 3");
        let started = Instant::now();
        let err = ensure_daemon_running_with(&launcher, Duration::from_secs(20)).unwrap_err();
        assert!(
            matches!(&err, DaemonStartError::Exited { status } if status.contains('3')),
            "got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "must not wait out the deadline"
        );
        assert!(std::fs::read_to_string(&log)
            .unwrap()
            .contains("boom: bad config"));
    }

    #[test]
    fn the_installed_launcher_runs_its_exe_with_the_daemon_arg() {
        let _l = LOCK.lock().unwrap();
        let _g = SockGuard::new("launch");
        let sock = hub_socket_path().unwrap();
        // The stand-in "daemon" records its argv, then binds the socket by way
        // of the test (it can't bind a Unix socket from sh); the test waits for
        // the record and binds on its behalf, while the script stays alive.
        let dir = std::env::temp_dir().join(format!("shb-dl-launch-{}", std::process::id()));
        let argv = dir.join("argv");
        let stop = dir.join("stop");
        let _ = std::fs::remove_file(&argv);
        let _ = std::fs::remove_file(&stop);
        let (launcher, _log) = script_launcher(
            "launch",
            &format!(
                "echo \"$@\" > {}; while [ ! -f {} ]; do sleep 0.05; done",
                argv.display(),
                stop.display()
            ),
        );
        set_daemon_launcher(launcher);
        let binder = std::thread::spawn(move || {
            let start = Instant::now();
            while !argv.exists() && start.elapsed() < Duration::from_secs(10) {
                std::thread::sleep(Duration::from_millis(20));
            }
            let recorded = std::fs::read_to_string(&argv).unwrap_or_default();
            (UnixListener::bind(&sock).unwrap(), recorded)
        });
        let result = ensure_daemon_running();
        let (_listener, recorded) = binder.join().unwrap();
        *LAUNCHER.write().unwrap() = None;
        std::fs::write(&stop, "").unwrap();
        result.unwrap();
        assert_eq!(recorded.trim(), "daemon");
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
