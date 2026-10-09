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
//!    `launchctl asuser <uid> <self> daemon`. The detached session processes
//!    survive a daemon re-exec, so running work is not killed; the fresh daemon
//!    takes over the socket and spawns future work with a healthy context. If
//!    the re-exec can't be launched (or has already been tried once), it falls
//!    back to the warning.
//! 4. **Warns** via the marker, which the TUI banner, `shelbi status`, and
//!    `shelbi doctor` read.
//!
//! ## Platform gating
//!
//! A lost *GUI* login session is a macOS-only concern, so the production
//! plumbing (env config, the real `launchctl asuser` recovery, the spawn loop)
//! lives in the [`imp`] submodule under `#[cfg(target_os = "macos")]`; on every
//! other OS [`spawn_session_health_monitor`] is a no-op. The platform-agnostic
//! decision core ([`monitor`]) is additionally compiled under `cfg(test)` so the
//! unit tests exercise it on every OS, with injected probe + recovery fakes —
//! no real `launchctl`, `HOME`, or `~/Library/LaunchAgents` is ever touched.
//!
//! ## Keeping tests (and CI) off the real `launchctl`
//!
//! The production monitor is only *spawned* against the default installed root
//! ([`imp::session_health_enabled`], mirroring [`super::lifecycle`]'s
//! `should_retire_units` gate) and never under an explicit
//! `--root`/`$SHELBI_ROOT`/`$SHELBI_HOME` — so every `cargo test` / integration
//! daemon (which points `SHELBI_HOME` at a temp dir) never runs it, never shells
//! `launchctl`, and never blocks its own spawns.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

/// Spawn the macOS session-health monitor. A no-op on non-macOS, when disabled,
/// or under a non-default root (tests). `sock` is the hub socket, used to wake
/// the accept loop when recovery re-execs and this daemon must stand down.
pub(super) fn spawn_session_health_monitor(stop: Arc<AtomicBool>, sock: PathBuf) {
    #[cfg(not(target_os = "macos"))]
    {
        // Linux / other: a lost *GUI* login session is not a concern here.
        let _ = (stop, sock);
    }
    #[cfg(target_os = "macos")]
    {
        imp::spawn(stop, sock);
    }
}

/// The platform-agnostic detection + recovery decision core.
///
/// Compiled on macOS (where the real daemon drives it) and under `cfg(test)`
/// (where the unit tests drive it with fakes on every OS). Everything here is
/// OS-independent: the OS-specific bits (how we probe, how we re-exec) arrive
/// through the [`SessionProbe`] and [`SessionRecovery`] seams.
#[cfg(any(target_os = "macos", test))]
mod monitor {
    use shelbi_core::session_health::{
        classify, HealthTransition, SessionHealthTracker, SessionProbe,
    };
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

    /// The monitor's core, generic over the probe + recovery seams so tests
    /// drive [`SessionHealthMonitor::tick`] with fakes.
    pub(crate) struct SessionHealthMonitor<P: SessionProbe, R: SessionRecovery> {
        probe: P,
        recovery: R,
        tracker: SessionHealthTracker,
        mode: RecoveryMode,
        /// Called after a re-exec is launched, to shut this orphaned daemon down
        /// so the new one can bind the socket.
        shutdown: Box<dyn Fn() + Send>,
    }

    impl<P: SessionProbe, R: SessionRecovery> SessionHealthMonitor<P, R> {
        pub(crate) fn new(
            probe: P,
            recovery: R,
            tracker: SessionHealthTracker,
            mode: RecoveryMode,
            shutdown: Box<dyn Fn() + Send>,
        ) -> Self {
            Self {
                probe,
                recovery,
                tracker,
                mode,
                shutdown,
            }
        }

        /// Run one probe → classify → debounce → act cycle.
        pub(crate) fn tick(&mut self) {
            let reading = self.probe.read();
            match self.tracker.observe(classify(&reading)) {
                HealthTransition::BecameLost { reason } => self.on_lost(reason),
                HealthTransition::Recovered => self.on_recovered(),
                HealthTransition::None => {}
            }
        }

        fn on_lost(&mut self, reason: String) {
            let _ = shelbi_state::append_daemon_event("session-lost", &reason);
            shelbi_orchestrator::session_guard::block_spawning();

            // Loop guard: if a predecessor daemon (or this one) already tried to
            // recover, don't re-exec again — escalate to GaveUp and keep warning.
            let already_tried = matches!(
                shelbi_state::read_session_lost().map(|r| r.recovery),
                Some(RecoveryState::Attempted) | Some(RecoveryState::GaveUp)
            );
            if already_tried {
                let _ = shelbi_state::write_session_lost(&SessionLostRecord::now(
                    reason.clone(),
                    RecoveryState::GaveUp,
                ));
                let _ = shelbi_state::append_daemon_event("session-recovery-gave-up", &reason);
                return;
            }

            match self.mode {
                RecoveryMode::WarnOnly => {
                    let _ = shelbi_state::write_session_lost(&SessionLostRecord::now(
                        reason,
                        RecoveryState::NotAttempted,
                    ));
                }
                RecoveryMode::Reexec => {
                    // Record the attempt BEFORE launching, so a crash mid-re-exec
                    // still leaves a marker that prevents a loop on the next boot.
                    let _ = shelbi_state::write_session_lost(&SessionLostRecord::now(
                        reason.clone(),
                        RecoveryState::Attempted,
                    ));
                    match self.recovery.reexec_in_gui_session() {
                        Ok(()) => {
                            let _ = shelbi_state::append_daemon_event(
                                "session-recovery-attempting",
                                &reason,
                            );
                            // Hand the socket to the fresh daemon.
                            (self.shutdown)();
                        }
                        Err(e) => {
                            let _ = shelbi_state::write_session_lost(&SessionLostRecord::now(
                                reason,
                                RecoveryState::GaveUp,
                            ));
                            let _ = shelbi_state::append_daemon_event("session-recovery-failed", &e);
                        }
                    }
                }
            }
        }

        fn on_recovered(&mut self) {
            shelbi_orchestrator::session_guard::allow_spawning();
            let _ = shelbi_state::clear_session_lost();
            let _ = shelbi_state::append_daemon_event("session-recovered", "manager=aqua");
        }
    }
}

// --------------------------------------------------------------------------
// macOS production spawn + env/config plumbing
// --------------------------------------------------------------------------

/// Real macOS plumbing: env-driven config, the `launchctl asuser` recovery, the
/// hermetic enablement gate, and the background spawn loop. Gated to macOS; the
/// decision logic it drives lives in the cross-platform [`monitor`] module.
#[cfg(target_os = "macos")]
mod imp {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use shelbi_core::session_health::{RealSessionProbe, SessionHealthTracker, DEFAULT_LOST_THRESHOLD};

    use super::monitor::{RecoveryMode, SessionHealthMonitor, SessionRecovery};

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

    /// The real recovery: `launchctl asuser <uid> <self-exe> daemon`, which runs
    /// the command in the user's GUI (`Aqua`) bootstrap domain — the live login
    /// session — rather than this orphaned one. Spawned detached; we don't wait.
    pub(super) struct RealSessionRecovery {
        uid: u32,
    }

    impl SessionRecovery for RealSessionRecovery {
        fn reexec_in_gui_session(&self) -> std::result::Result<(), String> {
            let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
            std::process::Command::new("launchctl")
                .arg("asuser")
                .arg(self.uid.to_string())
                .arg(&exe)
                .arg("daemon")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .map(|_child| ())
                .map_err(|e| format!("launchctl asuser spawn failed: {e}"))
        }
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

    /// Whether the daemon may run the session-health monitor (and, on loss,
    /// shell `launchctl`). True only when the opt-out env is unset AND the daemon
    /// is running against the *default* installed root — so a
    /// `--root`/`$SHELBI_ROOT`/`$SHELBI_HOME` daemon (every test) is a hermetic
    /// no-op, exactly as `super::lifecycle::should_retire_units` gates the
    /// launchd-retire step.
    pub(super) fn session_health_enabled() -> bool {
        if std::env::var_os(DISABLE_ENV).is_some() {
            return false;
        }
        matches!(
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
            return;
        }
        let interval = duration_from_env_ms(CHECK_INTERVAL_ENV, CHECK_INTERVAL);
        let threshold = threshold_from_env();
        let mode = recovery_mode_from_env();
        // SAFETY: getuid() is a thread-safe POSIX call with no inputs.
        let uid = unsafe { libc::getuid() };

        // Startup reconcile: a marker already on disk means a predecessor
        // declared the session lost. Block spawning until we prove ourselves
        // healthy, and seed the tracker so the first healthy reading clears the
        // marker (and a still-lost reading isn't re-announced or re-recovered).
        let mut tracker = SessionHealthTracker::new(threshold);
        if shelbi_state::session_lost_active() {
            shelbi_orchestrator::session_guard::block_spawning();
            tracker.seed_declared_lost();
        }

        let probe = RealSessionProbe::new(dns_host(), ssh_auth_sock());
        let recovery = RealSessionRecovery { uid };
        let shutdown: Box<dyn Fn() + Send> = {
            let stop = stop.clone();
            let sock = sock.clone();
            Box::new(move || {
                stop.store(true, Ordering::SeqCst);
                let _ = std::os::unix::net::UnixStream::connect(&sock);
            })
        };
        let mut monitor = SessionHealthMonitor::new(probe, recovery, tracker, mode, shutdown);

        std::thread::Builder::new()
            .name("shelbi-session-health".into())
            .spawn(move || loop {
                if !sleep_unless_stopped(&stop, interval) {
                    return;
                }
                monitor.tick();
                // Recovery may have flipped the stop flag.
                if stop.load(Ordering::SeqCst) {
                    return;
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
    use super::monitor::{RecoveryMode, SessionHealthMonitor, SessionRecovery};
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use shelbi_core::session_health::{
        classify, SessionHealthTracker, SessionProbe, SessionReading, TickVerdict,
    };
    use shelbi_state::{RecoveryState, SessionLostRecord};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
            shelbi_orchestrator::session_guard::allow_spawning();
            Self {
                _guard: guard,
                _lock: lock,
                home,
            }
        }
    }
    impl Drop for MonitorHome {
        fn drop(&mut self) {
            shelbi_orchestrator::session_guard::allow_spawning();
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

    fn no_shutdown() -> Box<dyn Fn() + Send> {
        Box::new(|| {})
    }

    #[test]
    fn healthy_session_writes_no_marker_and_never_recovers() {
        let _home = MonitorHome::new("healthy");
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = ScriptProbe::new(vec![SessionReading::healthy()]);
        let recovery = FakeRecovery {
            calls: calls.clone(),
            result: Ok(()),
        };
        let mut monitor = SessionHealthMonitor::new(
            probe,
            recovery,
            SessionHealthTracker::new(2),
            RecoveryMode::Reexec,
            no_shutdown(),
        );
        for _ in 0..5 {
            monitor.tick();
        }
        assert!(!shelbi_state::session_lost_active(), "no marker on a healthy session");
        assert_eq!(calls.load(Ordering::SeqCst), 0, "recovery never invoked");
        assert!(!shelbi_orchestrator::session_guard::spawning_blocked());
    }

    #[test]
    fn declared_loss_blocks_spawning_attempts_recovery_and_shuts_down() {
        let _home = MonitorHome::new("reexec");
        let calls = Arc::new(AtomicUsize::new(0));
        let shut = Arc::new(AtomicUsize::new(0));
        let shut2 = shut.clone();
        let probe = ScriptProbe::new(vec![lost()]);
        let recovery = FakeRecovery {
            calls: calls.clone(),
            result: Ok(()),
        };
        let mut monitor = SessionHealthMonitor::new(
            probe,
            recovery,
            SessionHealthTracker::new(2),
            RecoveryMode::Reexec,
            Box::new(move || {
                shut2.fetch_add(1, Ordering::SeqCst);
            }),
        );
        // Below threshold: nothing yet.
        monitor.tick();
        assert!(!shelbi_state::session_lost_active());
        // Crosses threshold: marker + gate + recovery + shutdown.
        monitor.tick();
        assert!(shelbi_orchestrator::session_guard::spawning_blocked());
        assert_eq!(calls.load(Ordering::SeqCst), 1, "recovery invoked once");
        assert_eq!(shut.load(Ordering::SeqCst), 1, "shutdown invoked after re-exec");
        let rec = shelbi_state::read_session_lost().unwrap();
        assert_eq!(rec.recovery, RecoveryState::Attempted);
    }

    #[test]
    fn warn_only_mode_never_re_execs() {
        let _home = MonitorHome::new("warn");
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = ScriptProbe::new(vec![lost()]);
        let recovery = FakeRecovery {
            calls: calls.clone(),
            result: Ok(()),
        };
        let mut monitor = SessionHealthMonitor::new(
            probe,
            recovery,
            SessionHealthTracker::new(1),
            RecoveryMode::WarnOnly,
            no_shutdown(),
        );
        monitor.tick();
        assert!(shelbi_orchestrator::session_guard::spawning_blocked());
        assert_eq!(calls.load(Ordering::SeqCst), 0, "warn-only never re-execs");
        let rec = shelbi_state::read_session_lost().unwrap();
        assert_eq!(rec.recovery, RecoveryState::NotAttempted);
    }

    #[test]
    fn failed_recovery_records_gave_up_and_keeps_warning() {
        let _home = MonitorHome::new("failed");
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = ScriptProbe::new(vec![lost()]);
        let recovery = FakeRecovery {
            calls: calls.clone(),
            result: Err("launchctl asuser spawn failed: nope".into()),
        };
        let mut monitor = SessionHealthMonitor::new(
            probe,
            recovery,
            SessionHealthTracker::new(1),
            RecoveryMode::Reexec,
            no_shutdown(),
        );
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        let rec = shelbi_state::read_session_lost().unwrap();
        assert_eq!(rec.recovery, RecoveryState::GaveUp, "failed re-exec → gave up");
        assert!(shelbi_orchestrator::session_guard::spawning_blocked());
    }

    #[test]
    fn a_pre_existing_attempt_marker_is_not_re_execed() {
        let _home = MonitorHome::new("loopguard");
        // A predecessor already attempted recovery.
        shelbi_state::write_session_lost(&SessionLostRecord::now(
            "launchctl-managername-not-aqua",
            RecoveryState::Attempted,
        ))
        .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let probe = ScriptProbe::new(vec![lost()]);
        let recovery = FakeRecovery {
            calls: calls.clone(),
            result: Ok(()),
        };
        // Fresh tracker (new daemon). First loss crosses a threshold of 1.
        let mut monitor = SessionHealthMonitor::new(
            probe,
            recovery,
            SessionHealthTracker::new(1),
            RecoveryMode::Reexec,
            no_shutdown(),
        );
        monitor.tick();
        assert_eq!(calls.load(Ordering::SeqCst), 0, "must not re-exec again");
        let rec = shelbi_state::read_session_lost().unwrap();
        assert_eq!(rec.recovery, RecoveryState::GaveUp);
    }

    #[test]
    fn recovered_daemon_clears_marker_and_reopens_the_gate() {
        let _home = MonitorHome::new("recovered");
        // Startup state: marker present, so the new daemon seeds declared-lost
        // and blocks spawning.
        shelbi_state::write_session_lost(&SessionLostRecord::now(
            "launchctl-managername-not-aqua",
            RecoveryState::Attempted,
        ))
        .unwrap();
        shelbi_orchestrator::session_guard::block_spawning();
        let mut tracker = SessionHealthTracker::new(2);
        tracker.seed_declared_lost();

        let probe = ScriptProbe::new(vec![SessionReading::healthy()]);
        let recovery = FakeRecovery {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Ok(()),
        };
        let mut monitor = SessionHealthMonitor::new(
            probe,
            recovery,
            tracker,
            RecoveryMode::Reexec,
            no_shutdown(),
        );
        monitor.tick();
        assert!(!shelbi_state::session_lost_active(), "marker cleared on recovery");
        assert!(!shelbi_orchestrator::session_guard::spawning_blocked(), "gate reopened");
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
        let probe = ScriptProbe::new(vec![reading]);
        let mut monitor = SessionHealthMonitor::new(
            probe,
            FakeRecovery {
                calls: Arc::new(AtomicUsize::new(0)),
                result: Ok(()),
            },
            SessionHealthTracker::new(1),
            RecoveryMode::Reexec,
            no_shutdown(),
        );
        monitor.tick();
        assert!(!shelbi_state::session_lost_active());
    }
}
