//! macOS GUI-login-session loss detection, recovery, and warning for the hub
//! daemon.
//!
//! A daemon started in one macOS login session keeps running as an orphan when
//! the user logs out and back in (or the window server restarts after a crash
//! or update). Every process it then spawns inherits a dead per-user bootstrap
//! context: DNS stops resolving, user lookups fail, and the launchd `ssh-agent`
//! has moved so the stale `SSH_AUTH_SOCK` no longer works. The daemon keeps
//! starting broken agents and nothing notices.
//!
//! This module runs a background monitor inside the daemon that:
//!
//! 1. **Detects** the loss, using the debounced decision in
//!    [`shelbi_core::session_health`] (decisive signal: `launchctl managername`
//!    no longer reports `Aqua`; DNS + `SSH_AUTH_SOCK` are corroborating only, so
//!    a brief network outage is never mistaken for a session loss).
//! 2. **Stops spawning** new sessions ([`shelbi_orchestrator::session_guard`])
//!    and writes a `daemon session-lost reason=…` event + a durable marker.
//! 3. **Recovers** by re-execing the daemon inside the live GUI session via
//!    `launchctl asuser <uid> <self> daemon run --takeover`. The detached session
//!    processes survive a daemon re-exec, so running work is not killed. This
//!    daemon then stands down, holding the bind lock through its drain, and the
//!    successor waits for that lock (see `serve::run_takeover`) instead of losing
//!    to it. If the re-exec can't be launched (or has already been tried once),
//!    it falls back to the warning.
//! 4. **Warns** via the marker, which the TUI banner, `shelbi status`, and
//!    `shelbi doctor` read.
//!
//! ## One owner, across daemon generations
//!
//! [`monitor::SessionLostState`] is the only writer of the marker, the spawn gate
//! and the lifecycle events, and moves them together. The marker carries the
//! lifecycle across a re-exec, so a daemon that finds one settles it instead of
//! trusting it:
//!
//! ```text
//!              declared, warn-only
//!   healthy ───────────────────────► not-attempted
//!      │                                  │ re-exec mode, still lost
//!      │ declared, re-exec                ▼
//!      └────────────────────────────► attempted ──────► gave-up
//!                                         launch failed, successor could not
//!                                         serve, or successor still lost
//!
//!   any marker ──► healthy, on a healthy reading (marker cleared, gate open)
//! ```
//!
//! ## Platform gating and tests
//!
//! A lost *GUI* login session is a macOS-only concern, so the monitor only runs
//! on macOS, against the default installed root ([`imp::session_health_enabled`],
//! mirroring [`super::lifecycle`]'s `should_retire_units` gate), and never under
//! an explicit `--root`/`$SHELBI_ROOT`/`$SHELBI_HOME`. So an ordinary `cargo test`
//! or integration daemon never runs it, never shells `launchctl`, and never blocks
//! its own spawns. That gate is at run time so every OS builds the same code.
//!
//! The decision core ([`monitor`]) is unit tested with injected probe + recovery
//! fakes. The handoff itself takes real processes, so
//! `tests/session_lost_takeover.rs` sets [`imp::TEST_PROBE_FILE_ENV`], which turns
//! the monitor on under a temp root, reads the probe from a file, and launches the
//! successor directly rather than through `launchctl`.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Spawn the session-health monitor. A no-op on non-macOS, when disabled, or under
/// a non-default root (tests). `sock` is the hub socket, used to wake the accept
/// loop when recovery re-execs and this daemon must stand down.
pub(super) fn spawn_session_health_monitor(stop: Arc<AtomicBool>, sock: PathBuf) {
    imp::spawn(stop, sock);
}

/// A `--takeover` successor could not come up. Closes out the attempt that
/// launched it, since no daemon of this generation will. A marker that has moved
/// on (another daemon won the lock and already settled it) is left alone.
pub(super) fn takeover_failed() {
    let mut state = monitor::SessionLostState::load();
    if state.recovery() == Some(shelbi_state::RecoveryState::Attempted) {
        state.apply(monitor::SessionLostEvent::RecoveryFailed {
            detail: "successor-could-not-serve",
        });
    }
}

/// The platform-agnostic detection + recovery decision core. The OS-specific bits
/// (how we probe, how we re-exec) arrive through the [`SessionProbe`] and
/// [`SessionRecovery`] seams.
mod monitor {
    use shelbi_core::session_health::{
        classify, HealthTransition, SessionHealthTracker, SessionProbe, TickVerdict,
    };
    use shelbi_orchestrator::session_guard;
    use shelbi_state::{RecoveryState, SessionLostRecord};

    /// What to do when the session is declared lost.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) enum RecoveryMode {
        /// Re-exec the daemon into the live GUI session (the default).
        Reexec,
        /// Only warn — leave recovery to the operator (`shelbi quit` + reopen).
        WarnOnly,
    }

    /// The recovery action seam: re-exec the daemon inside the current GUI login
    /// session. Behind a trait so unit tests never invoke the real `launchctl`.
    pub(crate) trait SessionRecovery {
        /// Launch a fresh daemon inside the live GUI session. `Ok(())` means the
        /// re-exec was started (the caller then shuts this orphaned daemon down
        /// so the new one can take the socket); `Err` carries a short reason.
        fn reexec_in_gui_session(&self) -> std::result::Result<(), String>;
    }

    /// Something that moves the lost-session lifecycle.
    #[derive(Debug, Clone, Copy)]
    pub(crate) enum SessionLostEvent<'a> {
        /// Loss declared on a healthy session.
        Detected { reason: &'a str },
        /// About to launch a re-exec.
        RecoveryStarting,
        /// The re-exec launched.
        RecoveryLaunched,
        /// The re-exec could not be launched, or its successor could not serve.
        RecoveryFailed { detail: &'a str },
        /// A daemon that found an attempt on the marker confirmed it is still lost.
        StillLost { reason: &'a str },
        /// A healthy reading.
        Recovered,
    }

    /// The single owner of the lost-session lifecycle: the durable marker, the
    /// spawn gate and the events only change here, together.
    ///
    /// Memory is the truth for this daemon. A marker write that fails is retried
    /// by [`Self::retry_sync`], so a full disk delays the banner rather than
    /// forgetting the state.
    pub(crate) struct SessionLostState {
        record: Option<SessionLostRecord>,
        /// `record` has not reached disk yet.
        unsynced: bool,
    }

    impl SessionLostState {
        /// Adopt the marker a predecessor left, closing the gate to match it.
        pub(crate) fn load() -> Self {
            let record = shelbi_state::read_session_lost();
            if record.is_some() {
                session_guard::block_spawning();
            }
            Self {
                record,
                unsynced: false,
            }
        }

        /// Where recovery stands, or `None` when the session is healthy.
        pub(crate) fn recovery(&self) -> Option<RecoveryState> {
            self.record.as_ref().map(|r| r.recovery)
        }

        pub(crate) fn is_lost(&self) -> bool {
            self.record.is_some()
        }

        /// Apply `event`. Returns whether the marker on disk reflects the result,
        /// which a caller about to do something the marker must guard (a re-exec)
        /// has to check.
        pub(crate) fn apply(&mut self, event: SessionLostEvent<'_>) -> bool {
            match event {
                SessionLostEvent::Detected { reason } => {
                    Self::log("session-lost", reason);
                    session_guard::block_spawning();
                    self.record = Some(SessionLostRecord::now(reason, RecoveryState::NotAttempted));
                    self.sync()
                }
                SessionLostEvent::RecoveryStarting => self.set_recovery(RecoveryState::Attempted),
                SessionLostEvent::RecoveryLaunched => {
                    Self::log("session-recovery-attempting", &self.reason());
                    !self.unsynced
                }
                SessionLostEvent::RecoveryFailed { detail } => {
                    Self::log("session-recovery-failed", detail);
                    self.set_recovery(RecoveryState::GaveUp)
                }
                SessionLostEvent::StillLost { reason } => {
                    Self::log("session-recovery-gave-up", reason);
                    self.set_recovery(RecoveryState::GaveUp)
                }
                SessionLostEvent::Recovered => self.recover(),
            }
        }

        /// Clear the marker and open the gate. A clear that fails changes nothing,
        /// so the banner and the gate can't disagree: the next healthy reading
        /// tries again.
        fn recover(&mut self) -> bool {
            if shelbi_state::clear_session_lost().is_err() {
                return false;
            }
            self.record = None;
            self.unsynced = false;
            session_guard::allow_spawning();
            Self::log("session-recovered", "manager=aqua");
            true
        }

        /// Retry a marker write that failed earlier.
        pub(crate) fn retry_sync(&mut self) {
            if self.unsynced {
                self.sync();
            }
        }

        fn set_recovery(&mut self, recovery: RecoveryState) -> bool {
            if let Some(record) = self.record.as_mut() {
                record.recovery = recovery;
            }
            self.sync()
        }

        fn sync(&mut self) -> bool {
            let written = self
                .record
                .as_ref()
                .is_none_or(|r| shelbi_state::write_session_lost(r).is_ok());
            self.unsynced = !written;
            written
        }

        fn reason(&self) -> String {
            self.record.as_ref().map(|r| r.reason.clone()).unwrap_or_default()
        }

        /// Best-effort: an event that can't be appended must not change the state.
        fn log(action: &str, detail: &str) {
            let _ = shelbi_state::append_daemon_event(action, detail);
        }
    }

    /// Retire a marker no monitor in this daemon will ever settle (the monitor is
    /// disabled), so its banner can't stay up forever.
    pub(crate) fn retire_unowned_marker() {
        if shelbi_state::read_session_lost().is_some()
            && shelbi_state::clear_session_lost().is_ok()
        {
            let _ = shelbi_state::append_daemon_event("session-lost-marker-retired", "no-monitor");
        }
    }

    /// The monitor's core, generic over the probe + recovery seams so tests
    /// drive [`SessionHealthMonitor::tick`] with fakes.
    pub(crate) struct SessionHealthMonitor<P: SessionProbe, R: SessionRecovery> {
        probe: P,
        recovery: R,
        tracker: SessionHealthTracker,
        state: SessionLostState,
        mode: RecoveryMode,
        /// Called after a re-exec is launched, to shut this orphaned daemon down
        /// so the new one can bind the socket.
        shutdown: Box<dyn Fn() + Send>,
    }

    impl<P: SessionProbe, R: SessionRecovery> SessionHealthMonitor<P, R> {
        /// Build the monitor for a daemon that is starting up. Reconciles with any
        /// marker a predecessor left: the gate stays closed until this daemon has
        /// a verdict of its own, and the first healthy reading clears the marker.
        pub(crate) fn start(
            probe: P,
            recovery: R,
            threshold: u32,
            mode: RecoveryMode,
            shutdown: Box<dyn Fn() + Send>,
        ) -> Self {
            let state = SessionLostState::load();
            let mut tracker = SessionHealthTracker::new(threshold);
            if state.is_lost() {
                tracker.seed_declared_lost();
            }
            Self {
                probe,
                recovery,
                tracker,
                state,
                mode,
                shutdown,
            }
        }

        /// Whether a marker is awaiting a verdict. The spawn gate is closed until
        /// it gets one, so the loop probes straight away rather than a full
        /// interval later.
        pub(crate) fn is_lost(&self) -> bool {
            self.state.is_lost()
        }

        /// Run one probe → classify → debounce → act cycle.
        pub(crate) fn tick(&mut self) {
            let verdict = classify(&self.probe.read());
            let healthy = verdict == TickVerdict::Healthy;
            if let HealthTransition::BecameLost { reason } = self.tracker.observe(verdict) {
                self.on_lost(&reason);
            }
            // Keyed off the state rather than the tracker's one-shot `Recovered`,
            // so a clear that failed is retried on the next healthy reading.
            if healthy && self.state.is_lost() {
                self.state.apply(SessionLostEvent::Recovered);
            }
            self.state.retry_sync();
        }

        /// Loss is declared (or, for a daemon that found a marker, confirmed).
        /// What that means depends on how far recovery has got.
        fn on_lost(&mut self, reason: &str) {
            if !self.state.is_lost() {
                self.state.apply(SessionLostEvent::Detected { reason });
            }
            match (self.state.recovery(), self.mode) {
                (Some(RecoveryState::NotAttempted), RecoveryMode::Reexec) => {
                    self.attempt_recovery();
                }
                // The attempt already happened and we're what it produced (or it
                // never produced anything). Re-execing again would loop.
                (Some(RecoveryState::Attempted), _) => {
                    self.state.apply(SessionLostEvent::StillLost { reason });
                }
                _ => {}
            }
        }

        fn attempt_recovery(&mut self) {
            // The durable attempt is what stops a successor re-execing again, so
            // a re-exec never runs without it.
            if !self.state.apply(SessionLostEvent::RecoveryStarting) {
                self.state.apply(SessionLostEvent::RecoveryFailed {
                    detail: "marker-write-failed",
                });
                return;
            }
            match self.recovery.reexec_in_gui_session() {
                Ok(()) => {
                    self.state.apply(SessionLostEvent::RecoveryLaunched);
                    // The successor is waiting on our bind lock.
                    (self.shutdown)();
                }
                Err(detail) => {
                    self.state
                        .apply(SessionLostEvent::RecoveryFailed { detail: &detail });
                }
            }
        }
    }
}

// --------------------------------------------------------------------------
// Production spawn + env/config plumbing
// --------------------------------------------------------------------------

/// Real plumbing: env-driven config, the `launchctl asuser` recovery, the hermetic
/// enablement gate, and the background spawn loop. The decision logic it drives
/// lives in the [`monitor`] module.
mod imp {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use shelbi_core::session_health::{
        RealSessionProbe, SessionProbe, SessionReading, DEFAULT_LOST_THRESHOLD,
    };

    use super::monitor::{self, RecoveryMode, SessionHealthMonitor, SessionRecovery};

    /// Default interval between session-health checks.
    const CHECK_INTERVAL: Duration = Duration::from_secs(30);
    /// Env override (ms) for the check interval, so tests drive the loop fast.
    const CHECK_INTERVAL_ENV: &str = "SHELBI_SESSION_HEALTH_INTERVAL_MS";
    /// Env override for the consecutive-lost threshold.
    const THRESHOLD_ENV: &str = "SHELBI_SESSION_HEALTH_THRESHOLD";
    /// Set to disable the monitor entirely (detection + recovery + warn).
    const DISABLE_ENV: &str = "SHELBI_DISABLE_SESSION_HEALTH";
    /// `warn` (don't auto-re-exec, just warn) vs the default `reexec`.
    const RECOVERY_MODE_ENV: &str = "SHELBI_SESSION_RECOVERY";
    /// Hostname resolved for the corroborating DNS signal; overridable.
    const DNS_HOST_ENV: &str = "SHELBI_SESSION_HEALTH_DNS_HOST";
    const DEFAULT_DNS_HOST: &str = "github.com";
    /// Test fixture: a file whose contents stand in for the probe (`lost` reads
    /// lost, anything else healthy). Setting it runs the monitor under any root
    /// and on any OS, and launches the successor without `launchctl`, so an
    /// integration test can drive a real re-exec between real daemons.
    pub(super) const TEST_PROBE_FILE_ENV: &str = "SHELBI_SESSION_HEALTH_TEST_PROBE_FILE";
    /// How long a launched successor is watched for an early exit. It waits on
    /// our bind lock for as long as we run, so exiting inside this window can only
    /// mean it never started: bad launchctl arguments, a binary that was replaced.
    const LAUNCH_SETTLE: Duration = Duration::from_millis(500);

    /// Where the session verdict comes from.
    enum LiveProbe {
        Real(RealSessionProbe),
        Fixture(PathBuf),
    }

    impl SessionProbe for LiveProbe {
        fn read(&self) -> SessionReading {
            match self {
                Self::Real(probe) => probe.read(),
                Self::Fixture(path) => match std::fs::read_to_string(path) {
                    Ok(text) if text.trim() == "lost" => SessionReading {
                        manager_aqua: false,
                        dns_ok: false,
                        ssh_auth_sock_ok: false,
                    },
                    _ => SessionReading::healthy(),
                },
            }
        }
    }

    /// The real recovery: `launchctl asuser <uid> <self-exe> daemon run --takeover`,
    /// which runs the command in the user's GUI (`Aqua`) bootstrap domain — the
    /// live login session — rather than this orphaned one.
    pub(super) struct RealSessionRecovery {
        /// The GUI domain to launch into; `None` launches directly (test fixture).
        pub(super) uid: Option<u32>,
    }

    impl RealSessionRecovery {
        pub(super) fn successor_command(&self, exe: &Path) -> Command {
            let mut cmd = match self.uid {
                Some(uid) => {
                    let mut cmd = Command::new("launchctl");
                    cmd.arg("asuser").arg(uid.to_string()).arg(exe);
                    cmd
                }
                None => Command::new(exe),
            };
            cmd.args(["daemon", "run", "--takeover"]);
            cmd
        }
    }

    impl SessionRecovery for RealSessionRecovery {
        fn reexec_in_gui_session(&self) -> std::result::Result<(), String> {
            let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
            launch_detached(self.successor_command(&exe), LAUNCH_SETTLE)
        }
    }

    /// Spawn `cmd` detached in its own session (it has to outlive this daemon) and
    /// watch it for `settle`. An exit in that window fails the launch.
    pub(super) fn launch_detached(
        mut cmd: Command,
        settle: Duration,
    ) -> std::result::Result<(), String> {
        use std::os::unix::process::CommandExt;
        cmd.stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        // SAFETY: `setsid` is async-signal-safe and touches no shared state; the
        // closure runs in the forked child before exec. A failure only means it
        // is already a group leader, which is fine.
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
        let deadline = Instant::now() + settle;
        while Instant::now() < deadline {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!("successor exited at launch: {status}"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }

    fn duration_from_env_ms(key: &str, default: Duration) -> Duration {
        match std::env::var(key).ok().and_then(|v| v.parse::<u64>().ok()) {
            Some(ms) if ms > 0 => Duration::from_millis(ms),
            _ => default,
        }
    }

    fn threshold_from_env() -> u32 {
        std::env::var(THRESHOLD_ENV)
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|&n| n > 0)
            .unwrap_or(DEFAULT_LOST_THRESHOLD)
    }

    fn recovery_mode_from_env() -> RecoveryMode {
        match std::env::var(RECOVERY_MODE_ENV).ok().as_deref() {
            Some("warn") | Some("warn-only") | Some("off") => RecoveryMode::WarnOnly,
            _ => RecoveryMode::Reexec,
        }
    }

    fn dns_host() -> String {
        std::env::var(DNS_HOST_ENV)
            .ok()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_DNS_HOST.to_string())
    }

    fn ssh_auth_sock() -> Option<PathBuf> {
        std::env::var_os("SSH_AUTH_SOCK")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }

    fn test_probe_file() -> Option<PathBuf> {
        std::env::var_os(TEST_PROBE_FILE_ENV)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
    }

    /// Whether the daemon may run the session-health monitor (and, on loss,
    /// shell `launchctl`). True only on macOS when the opt-out env is unset AND
    /// the daemon is running against the *default* installed root — so a
    /// `--root`/`$SHELBI_ROOT`/`$SHELBI_HOME` daemon (every test) is a hermetic
    /// no-op, exactly as `super::lifecycle::should_retire_units` gates the
    /// launchd-retire step. [`TEST_PROBE_FILE_ENV`] opts a test daemon back in.
    pub(super) fn session_health_enabled() -> bool {
        if std::env::var_os(DISABLE_ENV).is_some() {
            return false;
        }
        if test_probe_file().is_some() {
            return true;
        }
        cfg!(target_os = "macos")
            && matches!(
                shelbi_state::resolve_root(),
                Ok((
                    _,
                    shelbi_state::RootSource::CompileTime | shelbi_state::RootSource::HomeFallback
                ))
            )
    }

    /// Spawn the monitor loop. See [`super::spawn_session_health_monitor`].
    pub(super) fn spawn(stop: Arc<AtomicBool>, sock: PathBuf) {
        if !session_health_enabled() {
            monitor::retire_unowned_marker();
            return;
        }
        let interval = duration_from_env_ms(CHECK_INTERVAL_ENV, CHECK_INTERVAL);
        let fixture = test_probe_file();
        // SAFETY: getuid() is a thread-safe POSIX call with no inputs.
        let uid = unsafe { libc::getuid() };
        let recovery = RealSessionRecovery {
            uid: fixture.is_none().then_some(uid),
        };
        let probe = match fixture {
            Some(path) => LiveProbe::Fixture(path),
            None => LiveProbe::Real(RealSessionProbe::new(dns_host(), ssh_auth_sock())),
        };
        let shutdown: Box<dyn Fn() + Send> = {
            let stop = stop.clone();
            Box::new(move || {
                stop.store(true, Ordering::SeqCst);
                let _ = std::os::unix::net::UnixStream::connect(&sock);
            })
        };
        // Synchronous, so a predecessor's marker has closed the spawn gate before
        // the daemon's pollers start.
        let mut monitor = SessionHealthMonitor::start(
            probe,
            recovery,
            threshold_from_env(),
            recovery_mode_from_env(),
            shutdown,
        );

        std::thread::Builder::new()
            .name("shelbi-session-health".into())
            .spawn(move || {
                if monitor.is_lost() {
                    monitor.tick();
                }
                // Recovery may have flipped the stop flag.
                while sleep_unless_stopped(&stop, interval) {
                    monitor.tick();
                }
            })
            .ok();
    }

    /// Sleep `total` in short slices, returning `false` early if `stop` is set.
    fn sleep_unless_stopped(stop: &Arc<AtomicBool>, total: Duration) -> bool {
        let slice = Duration::from_millis(250)
            .min(total)
            .max(Duration::from_millis(10));
        let mut waited = Duration::ZERO;
        while waited < total {
            if stop.load(Ordering::SeqCst) {
                return false;
            }
            std::thread::sleep(slice);
            waited += slice;
        }
        !stop.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::imp::{launch_detached, RealSessionRecovery};
    use super::monitor::{
        retire_unowned_marker, RecoveryMode, SessionHealthMonitor, SessionRecovery,
    };
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use shelbi_core::session_health::{classify, SessionProbe, SessionReading, TickVerdict};
    use shelbi_orchestrator::session_guard;
    use shelbi_state::{RecoveryState, SessionLostRecord};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// A probe returning a scripted sequence of readings (last one repeats).
    struct ScriptProbe {
        readings: Vec<SessionReading>,
        idx: std::cell::Cell<usize>,
    }
    impl ScriptProbe {
        fn new(readings: Vec<SessionReading>) -> Self {
            Self {
                readings,
                idx: std::cell::Cell::new(0),
            }
        }
    }
    impl SessionProbe for ScriptProbe {
        fn read(&self) -> SessionReading {
            let i = self.idx.get().min(self.readings.len() - 1);
            self.idx.set(self.idx.get() + 1);
            self.readings[i].clone()
        }
    }

    /// Records whether recovery was invoked and what to return.
    struct FakeRecovery {
        calls: Arc<AtomicUsize>,
        result: std::result::Result<(), String>,
    }
    impl SessionRecovery for FakeRecovery {
        fn reexec_in_gui_session(&self) -> std::result::Result<(), String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    /// Isolate `SHELBI_HOME` so marker IO stays in a throwaway dir, holding the
    /// shared env lock. Also resets the process-global spawn gate on the way in
    /// and out so these tests don't leak into (or inherit from) others.
    struct MonitorHome {
        _guard: EnvGuard,
        _lock: std::sync::MutexGuard<'static, ()>,
        home: PathBuf,
    }
    impl MonitorHome {
        fn new(tag: &str) -> Self {
            let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let guard = EnvGuard::new(&["SHELBI_HOME"]);
            let home = std::env::temp_dir().join(format!(
                "shelbi-session-health-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
            guard.set("SHELBI_HOME", &home);
            session_guard::allow_spawning();
            Self {
                _guard: guard,
                _lock: lock,
                home,
            }
        }

        /// A marker path nothing can write to, replace, or remove.
        fn wedge_marker(&self) -> PathBuf {
            let path = shelbi_state::session_lost_path().unwrap();
            std::fs::create_dir_all(path.join("wedged")).unwrap();
            path
        }

        fn events(&self, action: &str) -> usize {
            let log = std::fs::read_to_string(self.home.join("events.log")).unwrap_or_default();
            log.lines()
                .filter(|l| l.contains(&format!(" daemon {action} ")))
                .count()
        }
    }
    impl Drop for MonitorHome {
        fn drop(&mut self) {
            session_guard::allow_spawning();
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn lost() -> SessionReading {
        SessionReading {
            manager_aqua: false,
            dns_ok: false,
            ssh_auth_sock_ok: false,
        }
    }

    fn marker(recovery: RecoveryState) {
        shelbi_state::write_session_lost(&SessionLostRecord::now(
            "launchctl-managername-not-aqua",
            recovery,
        ))
        .unwrap();
    }

    fn recovery_on_disk() -> Option<RecoveryState> {
        shelbi_state::read_session_lost().map(|r| r.recovery)
    }

    /// A monitor over scripted readings, as a daemon starting now would build it.
    /// Returns it with its re-exec and shutdown counters.
    fn start_monitor(
        readings: Vec<SessionReading>,
        threshold: u32,
        mode: RecoveryMode,
        result: std::result::Result<(), String>,
    ) -> (
        SessionHealthMonitor<ScriptProbe, FakeRecovery>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let shut = Arc::new(AtomicUsize::new(0));
        let shut2 = shut.clone();
        let monitor = SessionHealthMonitor::start(
            ScriptProbe::new(readings),
            FakeRecovery {
                calls: calls.clone(),
                result,
            },
            threshold,
            mode,
            Box::new(move || {
                shut2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        (monitor, calls, shut)
    }

    #[test]
    fn healthy_session_writes_no_marker_and_never_recovers() {
        let _home = MonitorHome::new("healthy");
        let (mut monitor, calls, _) =
            start_monitor(vec![SessionReading::healthy()], 2, RecoveryMode::Reexec, Ok(()));
        for _ in 0..5 {
            monitor.tick();
        }
        assert!(!shelbi_state::session_lost_active(), "no marker on a healthy session");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "recovery never invoked");
        assert!(!session_guard::spawning_blocked());
    }

    #[test]
    fn declared_loss_blocks_spawning_attempts_recovery_and_shuts_down() {
        let home = MonitorHome::new("reexec");
        let (mut monitor, calls, shut) =
            start_monitor(vec![lost()], 2, RecoveryMode::Reexec, Ok(()));
        // Below threshold: nothing yet.
        monitor.tick();
        assert!(!shelbi_state::session_lost_active());
        // Crosses threshold: marker + gate + recovery + shutdown.
        monitor.tick();
        assert!(session_guard::spawning_blocked());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "recovery invoked once");
        assert_eq!(shut.load(Ordering::SeqCst), 1, "shutdown invoked after re-exec");
        assert_eq!(recovery_on_disk(), Some(RecoveryState::Attempted));
        assert_eq!(home.events("session-lost"), 1);
        assert_eq!(home.events("session-recovery-attempting"), 1);
    }

    #[test]
    fn warn_only_mode_never_re_execs() {
        let _home = MonitorHome::new("warn");
        let (mut monitor, calls, _) =
            start_monitor(vec![lost()], 1, RecoveryMode::WarnOnly, Ok(()));
        monitor.tick();
        assert!(session_guard::spawning_blocked());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "warn-only never re-execs");
        assert_eq!(recovery_on_disk(), Some(RecoveryState::NotAttempted));
    }

    #[test]
    fn failed_recovery_records_gave_up_and_keeps_warning() {
        let home = MonitorHome::new("failed");
        let (mut monitor, calls, shut) = start_monitor(
            vec![lost()],
            1,
            RecoveryMode::Reexec,
            Err("launchctl asuser spawn failed: nope".into()),
        );
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(shut.load(Ordering::SeqCst), 0, "this daemon keeps serving");
        assert_eq!(recovery_on_disk(), Some(RecoveryState::GaveUp), "failed re-exec → gave up");
        assert_eq!(home.events("session-recovery-failed"), 1);
        assert!(session_guard::spawning_blocked());
    }

    #[test]
    fn a_successor_still_lost_escalates_the_attempt_to_gave_up() {
        let home = MonitorHome::new("escalate");
        // What the predecessor left behind on its way out.
        marker(RecoveryState::Attempted);
        let detected_at = shelbi_state::read_session_lost().unwrap().detected_at;

        let (mut monitor, calls, shut) =
            start_monitor(vec![lost()], 2, RecoveryMode::Reexec, Ok(()));
        assert!(monitor.is_lost(), "the loop must probe straight away");
        assert!(session_guard::spawning_blocked(), "gate closed from the start");

        monitor.tick();
        assert_eq!(recovery_on_disk(), Some(RecoveryState::Attempted), "still debouncing");
        monitor.tick();
        assert_eq!(recovery_on_disk(), Some(RecoveryState::GaveUp));
        monitor.tick();

        assert_eq!(calls.load(Ordering::SeqCst), 0, "must not re-exec again");
        assert_eq!(shut.load(Ordering::SeqCst), 0, "keeps serving, warn-only");
        assert_eq!(home.events("session-recovery-gave-up"), 1, "escalates once");
        assert_eq!(home.events("session-lost"), 0, "not a new loss");
        assert!(session_guard::spawning_blocked());
        assert_eq!(
            shelbi_state::read_session_lost().unwrap().detected_at,
            detected_at,
            "still the same loss"
        );
    }

    #[test]
    fn a_gave_up_marker_stays_gave_up_and_never_re_execs() {
        let home = MonitorHome::new("gaveup");
        marker(RecoveryState::GaveUp);
        let (mut monitor, calls, _) = start_monitor(vec![lost()], 1, RecoveryMode::Reexec, Ok(()));
        monitor.tick();
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(recovery_on_disk(), Some(RecoveryState::GaveUp));
        assert_eq!(home.events("session-recovery-gave-up"), 0, "nothing new to report");
        assert!(session_guard::spawning_blocked());
    }

    #[test]
    fn a_warn_only_marker_still_gets_its_recovery_attempt_in_reexec_mode() {
        let _home = MonitorHome::new("upgrade");
        marker(RecoveryState::NotAttempted);
        let (mut monitor, calls, shut) =
            start_monitor(vec![lost()], 1, RecoveryMode::Reexec, Ok(()));
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 1, "nothing tried yet, so try");
        assert_eq!(shut.load(Ordering::SeqCst), 1);
        assert_eq!(recovery_on_disk(), Some(RecoveryState::Attempted));
    }

    #[test]
    fn a_warn_only_marker_stays_put_in_warn_only_mode() {
        let _home = MonitorHome::new("stayswarn");
        marker(RecoveryState::NotAttempted);
        let (mut monitor, calls, _) =
            start_monitor(vec![lost()], 1, RecoveryMode::WarnOnly, Ok(()));
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(recovery_on_disk(), Some(RecoveryState::NotAttempted));
    }

    #[test]
    fn every_marker_is_cleared_by_a_healthy_first_reading() {
        for recovery in [
            RecoveryState::NotAttempted,
            RecoveryState::Attempted,
            RecoveryState::GaveUp,
        ] {
            let home = MonitorHome::new("recovered");
            marker(recovery);
            let (mut monitor, calls, _) = start_monitor(
                vec![SessionReading::healthy()],
                2,
                RecoveryMode::Reexec,
                Ok(()),
            );
            assert!(session_guard::spawning_blocked(), "closed until a verdict");
            monitor.tick();
            assert!(!shelbi_state::session_lost_active(), "marker cleared on recovery");
            assert!(!session_guard::spawning_blocked(), "gate reopened");
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert_eq!(home.events("session-recovered"), 1);
        }
    }

    #[test]
    fn an_attempt_it_cannot_record_is_never_launched() {
        let home = MonitorHome::new("unrecorded");
        let wedged = home.wedge_marker();
        let (mut monitor, calls, shut) =
            start_monitor(vec![lost()], 1, RecoveryMode::Reexec, Ok(()));
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "a successor could loop without the marker");
        assert_eq!(shut.load(Ordering::SeqCst), 0);
        assert!(session_guard::spawning_blocked(), "the gate doesn't depend on the disk");
        assert_eq!(home.events("session-recovery-failed"), 1);

        // Once the disk is back the warning lands, without another attempt.
        std::fs::remove_dir_all(&wedged).unwrap();
        monitor.tick();
        assert_eq!(recovery_on_disk(), Some(RecoveryState::GaveUp));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_clear_that_fails_keeps_marker_and_gate_in_step_until_it_lands() {
        let home = MonitorHome::new("clearfail");
        marker(RecoveryState::Attempted);
        let (mut monitor, _, _) = start_monitor(
            vec![SessionReading::healthy()],
            2,
            RecoveryMode::Reexec,
            Ok(()),
        );
        std::fs::remove_file(shelbi_state::session_lost_path().unwrap()).unwrap();
        let wedged = home.wedge_marker();

        monitor.tick();
        assert!(session_guard::spawning_blocked(), "still reports lost, so still gated");
        assert_eq!(home.events("session-recovered"), 0);

        std::fs::remove_dir_all(&wedged).unwrap();
        monitor.tick();
        assert!(!session_guard::spawning_blocked());
        assert_eq!(home.events("session-recovered"), 1);
    }

    #[test]
    fn a_failed_takeover_gives_up_an_attempt_and_leaves_anything_else() {
        let home = MonitorHome::new("takeover");
        marker(RecoveryState::Attempted);
        super::takeover_failed();
        assert_eq!(recovery_on_disk(), Some(RecoveryState::GaveUp));
        assert_eq!(home.events("session-recovery-failed"), 1);

        // Another daemon got there first and settled it: nothing to resurrect.
        shelbi_state::clear_session_lost().unwrap();
        super::takeover_failed();
        assert_eq!(recovery_on_disk(), None);
    }

    #[test]
    fn a_marker_no_monitor_will_settle_is_retired() {
        let home = MonitorHome::new("retire");
        marker(RecoveryState::NotAttempted);
        retire_unowned_marker();
        assert_eq!(recovery_on_disk(), None);
        assert_eq!(home.events("session-lost-marker-retired"), 1);
        // Nothing left to retire, nothing to report.
        retire_unowned_marker();
        assert_eq!(home.events("session-lost-marker-retired"), 1);
    }

    #[test]
    fn the_successor_launches_in_the_gui_domain_as_a_takeover() {
        let argv = |uid| {
            let cmd = RealSessionRecovery { uid }.successor_command(Path::new("/x/shelbi"));
            std::iter::once(cmd.get_program())
                .chain(cmd.get_args())
                .map(|a| a.to_string_lossy().into_owned())
                .collect::<Vec<_>>()
                .join(" ")
        };
        assert_eq!(
            argv(Some(501)),
            "launchctl asuser 501 /x/shelbi daemon run --takeover"
        );
        assert_eq!(argv(None), "/x/shelbi daemon run --takeover");
    }

    #[test]
    fn a_successor_that_dies_at_launch_fails_the_launch() {
        let mut dies = Command::new("sh");
        dies.args(["-c", "exit 3"]);
        let err = launch_detached(dies, Duration::from_secs(10)).unwrap_err();
        assert!(err.contains("exited at launch"), "{err}");

        let mut waits = Command::new("sleep");
        waits.arg("1");
        launch_detached(waits, Duration::from_millis(100)).expect("still running is a launch");

        let err = launch_detached(Command::new("/nonexistent/shelbi"), Duration::ZERO).unwrap_err();
        assert!(err.contains("spawn failed"), "{err}");
    }

    #[test]
    fn classify_is_healthy_when_only_dns_is_down() {
        // A sanity cross-check that a network blip (dns down, still Aqua) never
        // drives the monitor to declare loss.
        let _home = MonitorHome::new("blip");
        let reading = SessionReading {
            manager_aqua: true,
            dns_ok: false,
            ssh_auth_sock_ok: true,
        };
        assert_eq!(classify(&reading), TickVerdict::Healthy);
        let (mut monitor, _, _) = start_monitor(vec![reading], 1, RecoveryMode::Reexec, Ok(()));
        monitor.tick();
        assert!(!shelbi_state::session_lost_active());
    }
}
