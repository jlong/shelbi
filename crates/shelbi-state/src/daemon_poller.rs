//! Daemon-poller dev setting and the per-project poller lock (Phase 3,
//! `rt-daemon-poller`).
//!
//! The workspace poller can run in one of two places: the sidebar process (the
//! default) or `shelbi daemon` (one poller per open project). A hidden dev
//! setting selects which, and a per-project lock guarantees that exactly one
//! poller ever runs for a project — so a stale sidebar that outlives the switch
//! can't poll beside the daemon that has taken over
//! (`docs/removing-tmux/phase3-daemon.md`, "Per-project pollers").
//!
//! Dev-only for now: the default stays the sidebar poller until
//! `rt-daemon-layout-split` lands, because the poller still makes tmux layout
//! calls that belong in a client, not a hub-global daemon.

use std::os::unix::io::AsRawFd;
use std::path::PathBuf;

use shelbi_core::Result;

use crate::project_dir;

/// Environment variable that turns the daemon poller on. Truthy values (`1`,
/// `true`, `yes`, `on`, case-insensitive) enable it; anything else — including
/// absence — leaves the sidebar poller as the default.
pub const DAEMON_POLLER_ENV: &str = "SHELBI_DAEMON_POLLER";

/// Whether the daemon-poller dev setting is on. When true the daemon runs one
/// poller per open project and the sidebar runs none; when false (the default)
/// the sidebar runs the poller and the daemon's poller manager starts nothing.
pub fn daemon_poller_enabled() -> bool {
    matches!(
        std::env::var(DAEMON_POLLER_ENV)
            .ok()
            .as_deref()
            .map(str::trim)
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("1" | "true" | "yes" | "on")
    )
}

/// The per-project poller lock file, `<project_dir>/poller.lock`. The holder of
/// an exclusive `flock` on it is the one poller allowed to run for the project.
pub fn poller_lock_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("poller.lock"))
}

/// An acquired per-project poller lock. Holds the lock file open for as long as
/// the value lives; the exclusive `flock` is released when it is dropped (or the
/// process exits), so a crashed poller never wedges the next one out.
pub struct PollerLock {
    _file: std::fs::File,
    project: String,
}

impl PollerLock {
    /// The project this lock guards.
    pub fn project(&self) -> &str {
        &self.project
    }
}

/// Try to take the per-project poller lock without blocking.
///
/// Returns `Ok(Some(lock))` when this caller is now the sole poller for
/// `project`, `Ok(None)` when another poller (a daemon, or a stale sidebar)
/// already holds it, and `Err` only when the lock file's parent can't be
/// created or opened. Modeled on the daemon's own single-instance bind lock:
/// an exclusive, non-blocking `flock` whose refusal (`EWOULDBLOCK`) is the
/// "someone else is polling" signal.
pub fn acquire_poller_lock(project: &str) -> Result<Option<PollerLock>> {
    let path = poller_lock_path(project)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(shelbi_core::Error::Io)?;
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(shelbi_core::Error::Io)?;
    // SAFETY: `flock` on a valid fd we own; no memory is dereferenced.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        Ok(Some(PollerLock {
            _file: file,
            project: project.to_string(),
        }))
    } else {
        // Any failure to acquire (EWOULDBLOCK and friends) means a live holder.
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use std::path::Path;

    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
        home: PathBuf,
    }
    impl HomeGuard {
        fn new(tag: &str) -> Self {
            let lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-poller-lock-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(home.join("projects")).unwrap();
            // A minimal project state dir so `project_dir` validates and resolves.
            std::fs::create_dir_all(home.join("projects").join("p")).unwrap();
            let prev = std::env::var_os("SHELBI_HOME");
            std::env::set_var("SHELBI_HOME", &home);
            Self {
                _lock: lock,
                prev,
                home,
            }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    struct EnvGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
    }
    impl EnvGuard {
        fn set(value: Option<&str>) -> Self {
            let lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let prev = std::env::var_os(DAEMON_POLLER_ENV);
            match value {
                Some(v) => std::env::set_var(DAEMON_POLLER_ENV, v),
                None => std::env::remove_var(DAEMON_POLLER_ENV),
            }
            Self { _lock: lock, prev }
        }
    }
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var(DAEMON_POLLER_ENV, v),
                None => std::env::remove_var(DAEMON_POLLER_ENV),
            }
        }
    }

    #[test]
    fn setting_defaults_off_and_reads_truthy_values() {
        let _g = EnvGuard::set(None);
        assert!(!daemon_poller_enabled(), "absent → off");
        for on in ["1", "true", "TRUE", "yes", "On", " on "] {
            std::env::set_var(DAEMON_POLLER_ENV, on);
            assert!(daemon_poller_enabled(), "`{on}` → on");
        }
        for off in ["0", "false", "no", "", "nope"] {
            std::env::set_var(DAEMON_POLLER_ENV, off);
            assert!(!daemon_poller_enabled(), "`{off}` → off");
        }
    }

    #[test]
    fn lock_path_is_under_the_project_dir() {
        let _g = HomeGuard::new("path");
        let path = poller_lock_path("p").unwrap();
        assert!(path.ends_with(Path::new("p/poller.lock")), "{path:?}");
    }

    #[test]
    fn a_second_acquire_is_blocked_until_the_first_is_dropped() {
        let _g = HomeGuard::new("excl");
        let first = acquire_poller_lock("p").unwrap();
        assert!(first.is_some(), "first poller takes the lock");
        assert_eq!(first.as_ref().unwrap().project(), "p");

        // A second poller for the same project is refused while the first holds it.
        let second = acquire_poller_lock("p").unwrap();
        assert!(second.is_none(), "a second poller can't run beside the first");

        // Dropping the first releases the lock; the next poller may take it.
        drop(first);
        let third = acquire_poller_lock("p").unwrap();
        assert!(third.is_some(), "the lock is reclaimable once released");
    }
}
