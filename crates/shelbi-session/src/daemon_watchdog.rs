//! Keep the hub daemon alive while this session's project is open.
//!
//! With the launchd and systemd units retired (`rt-daemon-lifecycle`), nothing
//! brings the daemon back if it crashes while no UI is attached. But an open
//! project always has at least an orchestrator session, so every session process
//! is itself a watcher: on a jittered interval it checks whether the daemon's
//! single-instance lock is held, and if it is **free** *and* this session's
//! project is still marked open, it starts the daemon using the same on-demand
//! helper clients use (the `shelbi __ensure-daemon` seam, which calls
//! [`shelbi_state::ensure_daemon_running`]).
//!
//! Three properties make this safe and small:
//!
//! * **Races are harmless.** Several sessions may decide to start at once. The
//!   daemon's single-instance bind lock decides; the losers exit quietly. The
//!   lock check here is only an optimization to avoid spawning a helper when a
//!   daemon is plainly up.
//! * **Not when closed.** A session whose project is not open never starts the
//!   daemon. The quit/teardown paths clear the open flag *before* their sessions
//!   exit, so a session shutting down for a quit sees `open == false` and stays
//!   out of the way.
//! * **Installed binary, not this image.** The daemon is started from the
//!   installed `shelbi` on `$PATH` (or `$SHELBI_BIN`), never this session's own
//!   `current_exe`. A session is fixed at the version it started with, so an old
//!   session must not resurrect a stale daemon image — it brings up the current
//!   installed one.
//!
//! The check depends only on things that stay stable across versions: the lock
//! path, the open-project record in `state.json`, and the `shelbi __ensure-daemon`
//! entry point resolved from the installed binary.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Base interval between daemon-liveness checks. A crashed daemon is back within
/// roughly one interval (plus up to [`DEFAULT_JITTER`]). Documented in
/// `docs/removing-tmux/status/rt-session-restarts-daemon.md`.
const DEFAULT_INTERVAL: Duration = Duration::from_secs(15);

/// Maximum random delay added to each interval so that many sessions do not all
/// probe — and race to spawn — on the same tick. The effective period per
/// session is `DEFAULT_INTERVAL + rand(0..=DEFAULT_JITTER)`.
const DEFAULT_JITTER: Duration = Duration::from_secs(10);

/// Millisecond override for [`DEFAULT_INTERVAL`] (tests drive the loop fast).
const INTERVAL_ENV: &str = "SHELBI_DAEMON_WATCH_INTERVAL_MS";
/// Millisecond override for [`DEFAULT_JITTER`].
const JITTER_ENV: &str = "SHELBI_DAEMON_WATCH_JITTER_MS";

/// Start the background watchdog for a session named `name`.
///
/// The thread is detached and loops for the life of the process; the session
/// process exiting (any return from [`crate::session::run`]) tears it down. A
/// name with no resolvable project is ignored.
pub fn spawn(name: &str) {
    let project = match project_of(name) {
        Some(p) => p.to_string(),
        None => return,
    };
    let interval = env_duration(INTERVAL_ENV).unwrap_or(DEFAULT_INTERVAL);
    let jitter = env_duration(JITTER_ENV).unwrap_or(DEFAULT_JITTER);
    std::thread::Builder::new()
        .name("shelbi-daemon-watchdog".to_string())
        .spawn(move || loop {
            std::thread::sleep(interval + random_delay(jitter));
            tick(&project);
        })
        // A watchdog we failed to start is non-fatal: the session still runs,
        // it just won't be the one that restarts a crashed daemon.
        .ok();
}

/// One watchdog iteration: if the daemon lock is free and the project is open,
/// start the daemon from the installed binary.
fn tick(project: &str) {
    let lock_held = shelbi_state::daemon_lock_held();
    let project_open = shelbi_state::is_project_open(project).unwrap_or(false);
    if !should_start(lock_held, project_open) {
        return;
    }
    if let Some(exe) = installed_shelbi() {
        start_daemon(&exe);
    }
    // No installed binary resolvable → skip this tick. Best-effort: the daemon
    // simply isn't auto-restarted by this (presumably old) session rather than
    // being resurrected from a stale image.
}

/// The decision a single tick makes, factored out so it is unit-testable without
/// a daemon or a shell: start iff the lock is free **and** the project is open.
fn should_start(lock_held: bool, project_open: bool) -> bool {
    !lock_held && project_open
}

/// The project a session name belongs to: the first `/`-separated component of
/// the `<project>/orch`, `<project>/ws/<workspace>`, … scheme. `None` for an
/// empty name.
fn project_of(name: &str) -> Option<&str> {
    let project = name.split('/').next().unwrap_or(name);
    if project.is_empty() {
        None
    } else {
        Some(project)
    }
}

/// Resolve the **installed** `shelbi` binary — the one the user's shell would
/// run — never this session's own (possibly older) `current_exe`. In order:
///
/// 1. `$SHELBI_BIN`, if set and an executable file (an explicit override and the
///    test seam), then
/// 2. the first executable `shelbi` found on `$PATH`.
///
/// Returns `None` when neither resolves; [`tick`] then skips the restart.
fn installed_shelbi() -> Option<PathBuf> {
    resolve_installed_shelbi(std::env::var_os("SHELBI_BIN"), std::env::var_os("PATH"))
}

/// The pure core of [`installed_shelbi`], taking the two environment inputs
/// directly so it is testable without mutating the process-wide environment.
fn resolve_installed_shelbi(
    bin_override: Option<OsString>,
    path_var: Option<OsString>,
) -> Option<PathBuf> {
    if let Some(raw) = bin_override {
        let path = PathBuf::from(raw);
        if is_executable_file(&path) {
            return Some(path);
        }
    }
    std::env::split_paths(&path_var?)
        .map(|dir| dir.join("shelbi"))
        .find(|cand| is_executable_file(cand))
}

/// Whether `path` is a regular file with an execute bit set.
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(md) => md.is_file() && md.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Start the daemon by running `<installed-shelbi> __ensure-daemon` detached.
///
/// This reuses the exact on-demand helper clients use (the seam calls
/// [`shelbi_state::ensure_daemon_running`], whose own bind-lock dedup makes
/// concurrent starts converge on one daemon), but resolved from the installed
/// binary so the daemon that comes up is the current one. The subprocess is
/// handed an **explicit captured environment** (the login-shell environment plus
/// this home's `SHELBI_HOME`/`SHELBI_ROOT`), never the session's own child
/// environment, and is `setsid`-detached so a later process-group kill of the
/// session can't reach it mid-flight.
fn start_daemon(exe: &Path) {
    let mut cmd = Command::new(exe);
    cmd.arg("__ensure-daemon")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    cmd.env_clear();
    for (k, v) in daemon_spawn_env() {
        cmd.env(k, v);
    }
    // SAFETY: `setsid` is async-signal-safe and touches no shared state; it runs
    // in the forked child before exec. Ignore its error (a freshly forked child
    // is never already a group leader, so it does not fail in practice).
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let _ = cmd.spawn();
}

/// The explicit environment handed to the `__ensure-daemon` helper: the captured
/// interactive login-shell environment, plus Shelbi's state-root overrides so it
/// resolves the same `~/.shelbi` this session is operating under.
fn daemon_spawn_env() -> BTreeMap<String, String> {
    let mut env = shelbi_core::login_shell_env().clone();
    for key in ["SHELBI_HOME", "SHELBI_ROOT"] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env
}

/// Read a millisecond duration from `var`, or `None` if unset/unparseable.
fn env_duration(var: &str) -> Option<Duration> {
    std::env::var(var)
        .ok()?
        .parse::<u64>()
        .ok()
        .map(Duration::from_millis)
}

/// A pseudo-random delay in `0..=max`, derived from the clock so it needs no RNG
/// dependency. Good enough to spread many sessions' checks across the window.
fn random_delay(max: Duration) -> Duration {
    let span = max.as_millis();
    if span == 0 {
        return Duration::ZERO;
    }
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u128)
        .unwrap_or(0);
    Duration::from_millis((nanos % (span + 1)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_only_when_lock_free_and_project_open() {
        assert!(should_start(false, true), "free lock + open project → start");
        assert!(
            !should_start(true, true),
            "a held lock means a daemon is up → don't start"
        );
        assert!(
            !should_start(false, false),
            "a closed project must never start the daemon"
        );
        assert!(!should_start(true, false), "closed + held → don't start");
    }

    #[test]
    fn project_is_the_first_name_component() {
        assert_eq!(project_of("demo/orch"), Some("demo"));
        assert_eq!(project_of("demo/ws/alpha"), Some("demo"));
        assert_eq!(project_of("proj/review/slot-3/adversarial"), Some("proj"));
        assert_eq!(project_of("bare"), Some("bare"));
        assert_eq!(project_of(""), None);
    }

    #[test]
    fn env_duration_parses_milliseconds() {
        let var = "SHELBI_TEST_WATCH_DURATION";
        std::env::set_var(var, "250");
        assert_eq!(env_duration(var), Some(Duration::from_millis(250)));
        std::env::set_var(var, "not-a-number");
        assert_eq!(env_duration(var), None);
        std::env::remove_var(var);
        assert_eq!(env_duration(var), None);
    }

    #[test]
    fn random_delay_stays_within_bounds() {
        let max = Duration::from_millis(10);
        for _ in 0..1000 {
            assert!(random_delay(max) <= max);
        }
        assert_eq!(random_delay(Duration::ZERO), Duration::ZERO);
    }

    #[test]
    fn resolver_picks_an_installed_binary_not_this_image() {
        // The whole point of the resolver: it must return an *installed* binary
        // (here: fakes on a crafted PATH / SHELBI_BIN), never this process's own
        // image. The pure form takes the two env inputs directly so no
        // process-wide env is mutated.
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("shb-wd-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let bin_dir = dir.join("bin");
        let noexec_dir = dir.join("noexec");
        std::fs::create_dir_all(&bin_dir).unwrap();
        std::fs::create_dir_all(&noexec_dir).unwrap();

        // A non-executable `shelbi` earlier on PATH must be skipped...
        std::fs::write(noexec_dir.join("shelbi"), b"#!/bin/sh\n").unwrap();
        // ...in favour of the executable one later on PATH.
        let exe = bin_dir.join("shelbi");
        std::fs::write(&exe, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = std::env::join_paths([noexec_dir.as_path(), bin_dir.as_path()]).unwrap();
        assert_eq!(
            resolve_installed_shelbi(None, Some(path.clone())).as_deref(),
            Some(exe.as_path()),
            "PATH lookup picks the executable shelbi"
        );

        // An explicit, executable SHELBI_BIN wins outright.
        assert_eq!(
            resolve_installed_shelbi(Some(exe.clone().into_os_string()), None).as_deref(),
            Some(exe.as_path()),
            "an executable SHELBI_BIN override is honored"
        );

        // A non-executable SHELBI_BIN falls through to the PATH search.
        let noexec_bin = noexec_dir.join("shelbi").into_os_string();
        assert_eq!(
            resolve_installed_shelbi(Some(noexec_bin), Some(path)).as_deref(),
            Some(exe.as_path()),
            "a non-executable override falls back to PATH"
        );

        // Nothing resolvable → None (the watchdog then skips the restart).
        assert!(resolve_installed_shelbi(None, None).is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
