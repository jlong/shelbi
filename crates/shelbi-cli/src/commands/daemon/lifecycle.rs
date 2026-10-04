//! On-demand daemon lifecycle for `shelbi daemon restart|status`, plus the
//! upgrade step that retires a leftover launchd/systemd unit.
//!
//! The daemon is started on demand and exits when no project is open (see
//! `docs/removing-tmux/phase3-daemon.md`). There is no supervisor to install,
//! so this module does not write units: it only *removes* ones left by older
//! builds, and drives restart/status directly off the single-instance lock, the
//! socket, and the PID record (all in `shelbi-state`).
//!
//! [`retire_supervisor_units`] is called on daemon startup by [`super::serve`]
//! so an existing install self-heals: an installed unit's `KeepAlive` (launchd)
//! or `Restart=always` (systemd) loop would otherwise fight the on-demand
//! daemon, respawning the old binary after a restart and keeping a daemon alive
//! with no project open.

use std::fs;
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use shelbi_state::RootSource;

/// launchd `Label` / plist basename for the retired supervisor, and its
/// pre-rename predecessor. Both are booted out and deleted so an upgrade from
/// any prior build leaves nothing respawning the old daemon.
#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "dev.shelbi.daemon";
#[cfg(target_os = "macos")]
const LEGACY_SERVICE_LABEL: &str = "co.32pixels.shelbi";
/// systemd user unit name for the retired supervisor, and its pre-rename
/// predecessor.
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_NAME: &str = "dev.shelbi.daemon.service";
#[cfg(target_os = "linux")]
const LEGACY_SYSTEMD_SERVICE_NAME: &str = "shelbi.service";

/// How long [`restart`] waits for the freshly started daemon to answer before
/// reporting success.
const RESTART_START_DEADLINE: Duration = Duration::from_secs(10);

// --------------------------------------------------------------------------
// Retire the supervisor units (upgrade step, run on daemon startup)
// --------------------------------------------------------------------------

/// Env var that forces [`retire_supervisor_units`] to skip, regardless of the
/// resolved root. A belt-and-braces the test harnesses set so a daemon spawned
/// with a temp `SHELBI_HOME` but the developer's real `$HOME` (where
/// `dirs::home_dir()` still resolves the live plist) can never touch it — on top
/// of the default-root gate in [`should_retire_units`].
const NO_RETIRE_ENV: &str = "SHELBI_NO_RETIRE_UNITS";

/// Stop and remove any installed launchd/systemd supervisor unit, returning the
/// unit files removed (for disclosure). Idempotent: a host with no unit returns
/// an empty vec and makes no changes. Best-effort throughout — a hiccup must
/// never take the daemon down — so it swallows the expected "not loaded" noise
/// from `launchctl bootout` / `systemctl disable`.
///
/// Hermetic: a no-op unless the daemon is running against the default installed
/// shelbi root (see [`should_retire_units`]). Retiring the old unit is a
/// one-time upgrade step for a *real* install, so a daemon under `--root` /
/// `$SHELBI_ROOT` / `$SHELBI_HOME` (every `cargo test` daemon) makes no
/// `launchctl`/`systemctl` call and deletes no file — it can't uninstall the
/// developer's live hub daemon out from under a test.
pub(super) fn retire_supervisor_units() -> Vec<PathBuf> {
    if !should_retire_units() {
        return Vec::new();
    }
    #[cfg(target_os = "macos")]
    {
        retire_units_with(
            &RealLaunchd {
                uid: current_uid(),
                home: dirs::home_dir(),
            },
            &[SERVICE_LABEL, LEGACY_SERVICE_LABEL],
        )
    }
    #[cfg(target_os = "linux")]
    {
        retire_units_with(
            &RealSystemd {
                home: dirs::home_dir(),
            },
            &[SYSTEMD_SERVICE_NAME, LEGACY_SYSTEMD_SERVICE_NAME],
        )
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

/// Whether this daemon may touch the real user's launchd/systemd state.
///
/// True only when (a) the opt-out env var [`NO_RETIRE_ENV`] is unset, AND (b)
/// the resolved shelbi root is the *default* installed one — the compile-time
/// install bake ([`RootSource::CompileTime`]) or the `~/.shelbi` fallback
/// ([`RootSource::HomeFallback`]). Any explicit `--root` / `$SHELBI_ROOT` /
/// `$SHELBI_HOME` (every test, and every non-default install) resolves to one of
/// the other sources and makes this a hermetic no-op. This is the gate that
/// stops a `cargo test` daemon — which points `SHELBI_HOME` at a temp dir but
/// inherits the developer's real `$HOME` — from deleting the live
/// `~/Library/LaunchAgents/dev.shelbi.daemon.plist`.
fn should_retire_units() -> bool {
    if std::env::var_os(NO_RETIRE_ENV).is_some() {
        return false;
    }
    matches!(
        shelbi_state::resolve_root(),
        Ok((_, RootSource::CompileTime | RootSource::HomeFallback))
    )
}

/// The side-effecting operations [`retire_units_with`] performs, behind a seam
/// so a test runs the full retire logic against a temp dir and a recording fake
/// instead of the developer's real launchd/systemd state.
#[cfg(any(target_os = "macos", target_os = "linux"))]
trait RetireHost {
    /// Stop and unregister the service named `label` (`launchctl bootout` /
    /// `systemctl disable --now`). Best-effort; the "not loaded" error is a
    /// no-op the real impls silence.
    fn deactivate(&self, label: &str);
    /// Absolute path to `label`'s unit file, or `None` when no home resolves.
    fn unit_path(&self, label: &str) -> Option<PathBuf>;
    /// Hook run once after all removals (systemd `daemon-reload`; launchd a
    /// no-op). Receives the files actually removed so it can skip when empty.
    fn after_removals(&self, _removed: &[PathBuf]) {}
}

/// Deactivate and delete each labelled unit, returning the files actually
/// removed. Idempotent: a label with no live service and no file on disk
/// contributes nothing and makes no net change.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn retire_units_with<H: RetireHost>(host: &H, labels: &[&str]) -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for label in labels {
        // bootout / disable stops the service. Non-zero / "No such process" is
        // the expected no-op case; the real impls silence it.
        host.deactivate(label);
        if let Some(path) = host.unit_path(label) {
            if path.exists() && fs::remove_file(&path).is_ok() {
                removed.push(path);
            }
        }
    }
    host.after_removals(&removed);
    removed
}

#[cfg(target_os = "macos")]
struct RealLaunchd {
    uid: u32,
    home: Option<PathBuf>,
}

#[cfg(target_os = "macos")]
impl RetireHost for RealLaunchd {
    fn deactivate(&self, label: &str) {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{}/{label}", self.uid)])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    fn unit_path(&self, label: &str) -> Option<PathBuf> {
        self.home
            .as_ref()
            .map(|h| h.join("Library/LaunchAgents").join(format!("{label}.plist")))
    }
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    // SAFETY: getuid() is a thread-safe POSIX call with no inputs.
    unsafe { libc::getuid() }
}

#[cfg(target_os = "linux")]
struct RealSystemd {
    home: Option<PathBuf>,
}

#[cfg(target_os = "linux")]
impl RetireHost for RealSystemd {
    fn deactivate(&self, unit: &str) {
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", unit])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    fn unit_path(&self, unit: &str) -> Option<PathBuf> {
        self.home
            .as_ref()
            .map(|h| h.join(".config/systemd/user").join(unit))
    }
    fn after_removals(&self, removed: &[PathBuf]) {
        if !removed.is_empty() {
            let _ = Command::new("systemctl")
                .args(["--user", "daemon-reload"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }
}

// --------------------------------------------------------------------------
// restart / status
// --------------------------------------------------------------------------

/// Stop the running daemon and start a fresh one on the current binary, with no
/// supervisor. Retires any leftover unit first so its respawn loop can't bring
/// the old binary back between the stop and the start.
pub(super) fn restart() -> Result<()> {
    let removed = retire_supervisor_units();
    for path in &removed {
        println!("✓ retired leftover supervisor unit {}", path.display());
    }

    let stopped = shelbi_state::stop_daemon().context("stopping the running hub daemon")?;
    if stopped {
        println!("✓ stopped the running hub daemon");
    }

    shelbi_state::ensure_daemon_running().context("starting the hub daemon")?;
    // ensure_daemon_running already waits for the socket; this re-check keeps
    // the success line honest under a slow host.
    let deadline = Instant::now() + RESTART_START_DEADLINE;
    while Instant::now() < deadline {
        if !matches!(
            shelbi_state::probe_daemon_hello(),
            shelbi_state::DaemonProbe::NotRunning
        ) {
            println!("✓ hub daemon restarted on the current binary");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "restarted the hub daemon but it did not come back within {}s — \
         check `shelbi daemon status`",
        RESTART_START_DEADLINE.as_secs()
    )
}

/// Report daemon status from the single-instance lock, the socket, and the PID
/// record — no supervisor to query.
pub(super) fn status() -> Result<()> {
    let lock_held = shelbi_state::daemon_lock_held();
    let probe = shelbi_state::probe_daemon_hello();
    let listening = !matches!(probe, shelbi_state::DaemonProbe::NotRunning);

    if !lock_held && !listening {
        println!("shelbi daemon: not running");
        println!("  (started on demand when you open a project)");
        return Ok(());
    }

    println!("shelbi daemon: running");
    println!("  lock held:  {}", if lock_held { "yes" } else { "no" });
    println!("  listening:  {}", if listening { "yes" } else { "no" });

    match &probe {
        shelbi_state::DaemonProbe::Hello(hello) => {
            println!("  version:    {}", hello.version);
            println!("  protocol:   {}", hello.protocol);
        }
        shelbi_state::DaemonProbe::NoHello => {
            println!("  version:    (older daemon, predates the version handshake)");
        }
        shelbi_state::DaemonProbe::NotRunning => {}
    }

    if let Ok(Some(record)) = shelbi_state::read_daemon_pid_record() {
        println!("  pid:        {}", record.pid);
        if let Some(v) = record.version {
            println!("  pid record: {v}");
        }
    }

    if let Ok(open) = shelbi_state::list_open_projects() {
        if open.is_empty() {
            println!("  open:       (no open projects — will exit when idle)");
        } else {
            println!("  open:       {}", open.join(", "));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};

    /// A unique temp dir, created and returned.
    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-retire-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    // ---- seam: retire_units_with against a recording fake host ----
    //
    // These exercise the full retire logic (deactivate → remove → reload) against
    // a temp dir and a fake that records invocations, so the behavior stays
    // covered without ever touching the real launchd/systemd state.

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    struct FakeHost {
        dir: PathBuf,
        deactivated: std::cell::RefCell<Vec<String>>,
        reloads: std::cell::RefCell<usize>,
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl FakeHost {
        fn new(dir: PathBuf) -> Self {
            Self {
                dir,
                deactivated: std::cell::RefCell::new(Vec::new()),
                reloads: std::cell::RefCell::new(0),
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    impl RetireHost for FakeHost {
        fn deactivate(&self, label: &str) {
            self.deactivated.borrow_mut().push(label.to_string());
        }
        fn unit_path(&self, label: &str) -> Option<PathBuf> {
            Some(self.dir.join(format!("{label}.plist")))
        }
        fn after_removals(&self, removed: &[PathBuf]) {
            if !removed.is_empty() {
                *self.reloads.borrow_mut() += 1;
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn retire_units_with_deactivates_and_removes_present_files() {
        let dir = tmp_dir("present");
        let a = dir.join("a.plist");
        std::fs::write(&a, b"x").unwrap();
        // "b" intentionally has no file on disk: deactivate still runs for it,
        // but nothing is removed.
        let host = FakeHost::new(dir.clone());
        let removed = retire_units_with(&host, &["a", "b"]);

        assert_eq!(removed, vec![a.clone()]);
        assert!(!a.exists(), "a present unit file must be removed");
        assert_eq!(
            *host.deactivated.borrow(),
            vec!["a".to_string(), "b".to_string()],
            "every label is deactivated, present on disk or not"
        );
        assert_eq!(
            *host.reloads.borrow(),
            1,
            "after_removals runs once because something was removed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn retire_units_with_is_a_noop_when_absent() {
        let dir = tmp_dir("absent");
        let host = FakeHost::new(dir.clone());
        let removed = retire_units_with(&host, &["a", "b"]);

        assert!(removed.is_empty(), "no unit files present, so none removed");
        assert_eq!(
            *host.deactivated.borrow(),
            vec!["a".to_string(), "b".to_string()],
            "deactivate is still attempted (idempotent \"not loaded\" no-op)"
        );
        assert_eq!(
            *host.reloads.borrow(),
            0,
            "the post-removal reload is skipped when nothing was removed"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    // ---- gate: should_retire_units ----
    //
    // The hermetic gate. These assert only the boolean and never call
    // retire_supervisor_units(), so they can run on the developer's machine
    // without any risk of touching the live supervisor unit.

    #[test]
    fn gate_skips_under_explicit_shelbi_root() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let guard = EnvGuard::new(&["SHELBI_ROOT", "SHELBI_HOME", "SHELBI_NO_RETIRE_UNITS"]);
        guard.remove("SHELBI_HOME");
        guard.remove("SHELBI_NO_RETIRE_UNITS");
        guard.set("SHELBI_ROOT", "/tmp/not-the-default-root");
        assert!(
            !should_retire_units(),
            "an explicit $SHELBI_ROOT must gate retire off"
        );
    }

    #[test]
    fn gate_skips_under_env_home() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let guard = EnvGuard::new(&["SHELBI_ROOT", "SHELBI_HOME", "SHELBI_NO_RETIRE_UNITS"]);
        guard.remove("SHELBI_ROOT");
        guard.remove("SHELBI_NO_RETIRE_UNITS");
        guard.set("SHELBI_HOME", "/tmp/legacy-home");
        assert!(
            !should_retire_units(),
            "a $SHELBI_HOME (set by every daemon test) must gate retire off"
        );
    }

    #[test]
    fn gate_skips_on_opt_out_env() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let guard = EnvGuard::new(&["SHELBI_ROOT", "SHELBI_HOME", "SHELBI_NO_RETIRE_UNITS"]);
        // Clear the root overrides so the default-root branch would otherwise say
        // "retire"; the opt-out must still force the skip.
        guard.remove("SHELBI_ROOT");
        guard.remove("SHELBI_HOME");
        guard.set("SHELBI_NO_RETIRE_UNITS", "1");
        assert!(
            !should_retire_units(),
            "SHELBI_NO_RETIRE_UNITS must force the skip even on the default root"
        );
    }

    #[test]
    fn gate_allows_default_installed_root() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let guard = EnvGuard::new(&["SHELBI_ROOT", "SHELBI_HOME", "SHELBI_NO_RETIRE_UNITS"]);
        guard.remove("SHELBI_ROOT");
        guard.remove("SHELBI_HOME");
        guard.remove("SHELBI_NO_RETIRE_UNITS");
        // The default installed root (the compile-time install bake or the
        // ~/.shelbi fallback) is the one real upgrade path that should retire.
        assert!(
            should_retire_units(),
            "the default installed root must allow the one-time retire"
        );
    }

    // ---- regression: a temp-root daemon with the real $HOME is hermetic ----

    #[test]
    fn retire_is_hermetic_under_temp_root_with_real_home_layout() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let guard = EnvGuard::new(&[
            "HOME",
            "SHELBI_ROOT",
            "SHELBI_HOME",
            "SHELBI_NO_RETIRE_UNITS",
        ]);

        // A fake $HOME standing in for the developer's real one, holding a
        // sentinel supervisor unit at the exact real-world path that
        // retire_supervisor_units() would delete if the gate failed.
        let fake_home = tmp_dir("fake-home");
        #[cfg(target_os = "macos")]
        let unit = fake_home
            .join("Library/LaunchAgents")
            .join(format!("{SERVICE_LABEL}.plist"));
        #[cfg(target_os = "linux")]
        let unit = fake_home
            .join(".config/systemd/user")
            .join(SYSTEMD_SERVICE_NAME);
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        {
            std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
            std::fs::write(&unit, b"sentinel").unwrap();
        }

        // The daemon points SHELBI_ROOT at a throwaway dir (as every test does)
        // yet still inherits the real $HOME — exactly the control_socket.rs leak.
        let temp_root = tmp_dir("temp-root");
        guard.set("HOME", &fake_home);
        guard.set("SHELBI_ROOT", &temp_root);
        guard.remove("SHELBI_HOME");
        guard.remove("SHELBI_NO_RETIRE_UNITS");

        let removed = retire_supervisor_units();
        assert!(
            removed.is_empty(),
            "a temp-root daemon must retire nothing (made no launchctl/systemctl call)"
        );
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        assert!(
            unit.exists(),
            "the real-path supervisor unit must be left untouched"
        );

        std::fs::remove_dir_all(&fake_home).ok();
        std::fs::remove_dir_all(&temp_root).ok();
    }
}
