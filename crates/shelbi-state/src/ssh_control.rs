//! Hub-side SSH ControlMaster bookkeeping plus the daemon PID file.
//!
//! The hub maintains a long-lived outbound SSH ControlMaster per remote
//! host so each follow-up `ssh <host> ...` reuses a single TCP+TLS+auth
//! channel. We park those masters under `$SHELBI_HOME/ssh/` (default
//! `~/.shelbi/ssh/`) — distinct from `~/.ssh/` so the cleanup walk here
//! can't accidentally touch the user's hand-rolled CMs — and probe each
//! one at hub startup, removing any whose master process has died.
//!
//! The PID file at `$SHELBI_HOME/shelbi.pid` is the hub daemon's
//! aliveness beacon. The cleanup pass refuses to touch the directory
//! when the PID file points at a still-running shelbi process other
//! than us — that's another live daemon's CMs and they belong to it.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use shelbi_core::Result;

use crate::shelbi_home;

/// Directory holding the per-host SSH ControlMaster sockets:
/// `$SHELBI_HOME/ssh/`. Each file inside is named `<user>@<host>` per
/// the `%r@%h` template handed to OpenSSH's `ControlPath` option.
///
/// Lives under SHELBI_HOME (not `~/.ssh/`) so the startup cleanup can
/// scan and prune without risking the user's own ControlMasters.
pub fn ssh_control_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("ssh"))
}

/// Ensure [`ssh_control_dir`] exists with `0700` permissions. Idempotent —
/// creates the directory on first call and re-applies perms if they've
/// drifted.
///
/// Called from [`shelbi-ssh`](../../../shelbi-ssh/index.html) before every
/// outbound SSH invocation and from `ensure_root_subdirs` so a fresh
/// install ships with the directory in place. Without this, the first
/// hub→remote `ssh -o ControlPath=…` fails with
/// `unix_listener: cannot bind to path …: No such file or directory`
/// because OpenSSH won't create the ControlPath's parent for us.
pub fn ensure_ssh_control_dir() -> Result<()> {
    let dir = ssh_control_dir()?;
    fs::create_dir_all(&dir)?;
    // 0700: the sockets are only meaningful to the local user; opening
    // the directory up would let another local user hijack the master.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// String passed to OpenSSH's `-o ControlPath=…`. Includes the `%r@%h`
/// tokens that ssh expands per-connection. The directory matches
/// [`ssh_control_dir`] so this module's cleanup logic and ssh's
/// runtime agree on the path.
pub fn ssh_control_path_template() -> Result<String> {
    let dir = ssh_control_dir()?;
    Ok(format!("{}/%r@%h", dir.display()))
}

/// Remote-side landing path for the hub socket's reverse forward.
/// `$SHELBI_REMOTE_HUB_SOCK` wins so remote workers (Phase 5) can be
/// pointed at the same value via env without rebuilding the binary.
/// Default `/tmp/shelbi-hub.sock`.
pub fn remote_hub_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SHELBI_REMOTE_HUB_SOCK") {
        return PathBuf::from(p);
    }
    PathBuf::from("/tmp/shelbi-hub.sock")
}

/// Daemon PID file: `$SHELBI_HOME/shelbi.pid`. Used by the cleanup
/// pass to detect an already-running shelbi daemon whose CMs should
/// be left alone.
pub fn daemon_pid_file_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("shelbi.pid"))
}

/// Read the PID stored in `$SHELBI_HOME/shelbi.pid`. Returns `Ok(None)`
/// when the file is missing OR contains an unparseable value — a torn
/// or empty file from a crashed write is treated the same as "no
/// previous daemon recorded itself."
pub fn read_daemon_pid() -> Result<Option<libc::pid_t>> {
    let path = daemon_pid_file_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    Ok(text.trim().parse::<libc::pid_t>().ok())
}

/// Atomically write our PID to `$SHELBI_HOME/shelbi.pid`.
pub fn write_daemon_pid(pid: libc::pid_t) -> Result<()> {
    let path = daemon_pid_file_path()?;
    crate::atomic_write(&path, format!("{pid}\n").as_bytes())
}

/// Best-effort unlink of the daemon PID file. Missing file is not an
/// error — clean shutdown after a never-fully-started daemon must not
/// surface a spurious failure.
pub fn remove_daemon_pid_file() -> Result<()> {
    let path = daemon_pid_file_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

/// Does a process with `pid` currently exist? Implemented via
/// `kill(pid, 0)` — POSIX's standard "no-op signal, but still does the
/// permission/existence check." A `0` return means yes, the process is
/// alive (or at least known to the kernel). `ESRCH` means no such
/// process; `EPERM` means it exists but we can't signal it (treated as
/// alive — different user, but still a real process).
pub fn is_process_alive(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) is signal-safe and only reads kernel state.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Outcome of [`cleanup_stale_control_masters`] — surfaced so the
/// daemon can log a one-liner instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmCleanupOutcome {
    /// Another live shelbi process owns the CMs; we left them alone.
    SkippedAnotherDaemon { pid: libc::pid_t },
    /// Scanned the dir; `removed` is the count of orphaned socket
    /// files we unlinked, `kept` the count of sockets still answering
    /// a `connect()` (live masters).
    Scanned { removed: usize, kept: usize },
}

/// Walk `$SHELBI_HOME/ssh/` and remove socket files whose
/// ControlMaster is no longer alive (a `connect()` against the socket
/// fails with ENOENT, ECONNREFUSED, or similar).
///
/// `self_pid` is the calling process's PID — used to decide whether
/// the PID-file owner is "us" (cleanup proceeds) or another live
/// shelbi process (cleanup skipped). Pass `std::process::id() as i32`.
///
/// Live sockets (connect succeeded) are left in place. Non-socket
/// files in the directory are skipped silently — a stray text file
/// shouldn't keep the daemon from starting.
pub fn cleanup_stale_control_masters(self_pid: libc::pid_t) -> Result<CmCleanupOutcome> {
    if let Some(prev) = read_daemon_pid()? {
        if prev != self_pid && is_process_alive(prev) {
            return Ok(CmCleanupOutcome::SkippedAnotherDaemon { pid: prev });
        }
    }

    let dir = ssh_control_dir()?;
    if !dir.exists() {
        return Ok(CmCleanupOutcome::Scanned { removed: 0, kept: 0 });
    }

    let mut removed = 0;
    let mut kept = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        // Skip subdirectories outright. Skip regular files that aren't
        // sockets — a stray text file (e.g. an editor swap) shouldn't
        // get unlinked just for being in the dir.
        if ft.is_dir() {
            continue;
        }
        if !is_socket(&path) {
            continue;
        }
        if is_socket_alive(&path) {
            kept += 1;
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Vanished between stat and unlink — fine.
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "shelbi: failed to remove stale ControlMaster socket"
                );
            }
        }
    }
    Ok(CmCleanupOutcome::Scanned { removed, kept })
}

/// Best-effort: is this path a Unix-domain socket? Falls back to
/// `false` on any stat error so the cleanup loop treats "we don't
/// know" as "leave it alone."
fn is_socket(path: &std::path::Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
}

/// Probe the socket by opening a connection: a live ControlMaster has
/// a listener and `connect()` succeeds; an orphaned socket file
/// (master died, file leaked) gives ENOENT/ECONNREFUSED. Any other
/// error (permission denied, etc.) is conservatively treated as
/// "alive" so we don't unlink something we can't even probe.
fn is_socket_alive(path: &std::path::Path) -> bool {
    match UnixStream::connect(path) {
        Ok(_) => true,
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ),
    }
}

/// Hint at the canonical bind argument for re-establishing the
/// reverse forward when shelling out. Returned as `OsString` so
/// callers can hand it straight to `Command::arg`.
///
/// Layout: `<remote_sock>:<local_hub_sock>` — exactly what ssh's `-R`
/// flag expects.
pub fn reverse_forward_spec() -> Result<OsString> {
    let remote = remote_hub_socket_path();
    let local = crate::hub_socket_path()?;
    let mut s = OsString::new();
    s.push(remote);
    s.push(":");
    s.push(local);
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::os::unix::net::UnixListener;
    use std::sync::MutexGuard;

    /// Tolerate a poisoned mutex — a previous test panicked while
    /// holding the lock and we still want the rest of the suite to
    /// run instead of cascade-failing on PoisonError.
    fn lock_test() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Short-path temp dir. macOS caps unix-socket paths at ~104 bytes
    /// (SUN_LEN), and `std::env::temp_dir()` lands under
    /// `/var/folders/.../T/` which leaves no room for our `ssh/` +
    /// `user@host` suffix. `/tmp/` plus a brief slug keeps us well
    /// under the limit on every supported platform.
    fn fresh_home() -> PathBuf {
        let p = PathBuf::from(format!(
            "/tmp/shls-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn control_dir_lives_under_shelbi_home() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert_eq!(ssh_control_dir().unwrap(), home.join("ssh"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn ensure_ssh_control_dir_creates_missing_dir_with_0700() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = ssh_control_dir().unwrap();
        // Precondition: fresh home, no ssh/ subdir yet — mirrors the
        // fresh-install case that motivated this helper.
        assert!(!dir.exists(), "test setup: ssh/ dir should not exist yet");

        ensure_ssh_control_dir().unwrap();

        assert!(dir.is_dir(), "ssh/ dir should have been created");
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ssh/ dir should be 0700, got {mode:o}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn ensure_ssh_control_dir_is_idempotent_and_reasserts_perms() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = ssh_control_dir().unwrap();

        // First call creates. Second call is a no-op on already-correct
        // state. Both must succeed — daemon startup and every SSH
        // invocation call this, so a spurious failure would wedge us.
        ensure_ssh_control_dir().unwrap();
        ensure_ssh_control_dir().unwrap();
        assert!(dir.is_dir());

        // Drift perms open (0755 — what create_dir_all defaults to) and
        // confirm the helper snaps them back to 0700. Guards against a
        // future refactor that removes the set_permissions call.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_ssh_control_dir().unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ssh/ dir perms should have been reset to 0700");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn control_path_template_carries_user_at_host_tokens() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let tpl = ssh_control_path_template().unwrap();
        assert!(tpl.ends_with("/ssh/%r@%h"), "got: {tpl}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn remote_hub_socket_defaults_to_tmp_and_env_overrides() {
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        assert_eq!(remote_hub_socket_path(), PathBuf::from("/tmp/shelbi-hub.sock"));
        std::env::set_var("SHELBI_REMOTE_HUB_SOCK", "/run/foo.sock");
        assert_eq!(remote_hub_socket_path(), PathBuf::from("/run/foo.sock"));
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
    }

    #[test]
    fn pid_file_round_trips_and_remove_is_idempotent() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert_eq!(read_daemon_pid().unwrap(), None);
        write_daemon_pid(12345).unwrap();
        assert_eq!(read_daemon_pid().unwrap(), Some(12345));
        remove_daemon_pid_file().unwrap();
        assert_eq!(read_daemon_pid().unwrap(), None);
        // Second remove on the now-missing file is still Ok — a clean
        // shutdown after a never-fully-started daemon would otherwise
        // leak a spurious error.
        remove_daemon_pid_file().unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn read_daemon_pid_returns_none_for_garbage_contents() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        fs::write(home.join("shelbi.pid"), "not-a-pid\n").unwrap();
        // Garbage parses to None — treated as "no recorded daemon",
        // not a hard error. The torn-write recovery path needs this.
        assert_eq!(read_daemon_pid().unwrap(), None);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn is_process_alive_recognizes_self_and_pid_one() {
        // Self is always alive — kill(getpid(), 0) succeeds.
        assert!(is_process_alive(std::process::id() as libc::pid_t));
        // pid 1 (init) is always present on a running system. On
        // macOS launchd / on Linux systemd. EPERM here is treated as
        // alive by design.
        assert!(is_process_alive(1));
        // Sentinel: 0 and negatives never refer to a real process.
        assert!(!is_process_alive(0));
        assert!(!is_process_alive(-1));
    }

    #[test]
    fn cleanup_removes_orphan_keeps_live_and_skips_when_another_daemon() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let ssh_dir = ssh_control_dir().unwrap();
        fs::create_dir_all(&ssh_dir).unwrap();

        // Orphan: socket file with no listener (simulated by a regular
        // file). We need an actual socket node, not a plain file, so the
        // is_socket() check passes. Bind+drop a listener: the path now
        // refers to a closed socket file — a connect() returns
        // ECONNREFUSED.
        let orphan = ssh_dir.join("user@orphan");
        {
            let _l = UnixListener::bind(&orphan).unwrap();
        }
        // After drop the listener is gone but the file remains. Sanity:
        // confirm probe sees it as dead.
        assert!(orphan.exists());
        assert!(!is_socket_alive(&orphan));

        // Live: bind a listener and keep it. The handle stays in scope
        // for the duration of the test.
        let live = ssh_dir.join("user@live");
        let _live_listener = UnixListener::bind(&live).unwrap();
        assert!(is_socket_alive(&live));

        // Non-socket files in the directory are left alone.
        let stray = ssh_dir.join("README");
        fs::write(&stray, "not a socket").unwrap();

        // No PID file → cleanup proceeds.
        let outcome = cleanup_stale_control_masters(std::process::id() as libc::pid_t).unwrap();
        assert_eq!(outcome, CmCleanupOutcome::Scanned { removed: 1, kept: 1 });
        assert!(!orphan.exists());
        assert!(live.exists());
        assert!(stray.exists());

        // Re-stage the orphan and pin the directory with another live
        // daemon PID (pid 1 — always present). Cleanup must short-circuit.
        let orphan2 = ssh_dir.join("user@orphan2");
        {
            let _l = UnixListener::bind(&orphan2).unwrap();
        }
        write_daemon_pid(1).unwrap();
        let outcome = cleanup_stale_control_masters(std::process::id() as libc::pid_t).unwrap();
        assert_eq!(outcome, CmCleanupOutcome::SkippedAnotherDaemon { pid: 1 });
        // Orphan still present — we didn't touch the dir.
        assert!(orphan2.exists());

        // Stale PID for a process that no longer exists: cleanup
        // proceeds. PID 0 is never a real process, so passing it as the
        // recorded PID forces is_process_alive() to false; the existing
        // orphan is then unlinked.
        write_daemon_pid(0).unwrap();
        let outcome = cleanup_stale_control_masters(std::process::id() as libc::pid_t).unwrap();
        // orphan2 should be removed; live still alive.
        assert!(matches!(outcome, CmCleanupOutcome::Scanned { removed: 1, kept: 1 }));
        assert!(!orphan2.exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn reverse_forward_spec_joins_remote_and_local_with_colon() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        std::env::remove_var("SHELBI_HUB_SOCK");
        let spec = reverse_forward_spec().unwrap();
        let s = spec.to_string_lossy().into_owned();
        assert!(s.starts_with("/tmp/shelbi-hub.sock:"), "got: {s}");
        assert!(s.ends_with("/hub.sock"), "got: {s}");
        std::env::remove_var("SHELBI_HOME");
    }
}
