//! The session lifetime lock.
//!
//! A session holds an exclusive `flock` on `lock` for its whole life. Liveness
//! is therefore a cheap, race-free question for any other process: try to take
//! the same lock *shared*, non-blocking — if it succeeds, nobody holds it
//! exclusively, so the session is **dead** and its directory is stale state to
//! reap.
//!
//! This is a thin, Unix-only wrapper today. Windows gets a byte-range lock or a
//! named mutex behind the same `SessionLock` / [`is_held`] seam when the session
//! backend is ported there; the rest of the session code only sees these two
//! entry points.

use std::fs::{File, OpenOptions};
use std::os::unix::io::AsRawFd;
use std::path::Path;

use anyhow::{Context, Result};

/// An acquired, exclusive lock on a session's `lock` file. Dropping it (which
/// happens when the session process exits, by any path) releases the lock, and
/// the session then reads as dead.
#[derive(Debug)]
pub struct SessionLock {
    // Held open for the lifetime of the lock; closing the fd releases the flock.
    _file: File,
}

impl SessionLock {
    /// Take the exclusive lock, creating the file if needed. Fails if another
    /// live session already holds it (which would mean an id collision).
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating session dir {}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(path)
            .with_context(|| format!("opening lock file {}", path.display()))?;
        // SAFETY: flock on a valid open fd. LOCK_NB so a collision is an error,
        // not a hang.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(std::io::Error::last_os_error())
                .with_context(|| format!("another live session holds {}", path.display()));
        }
        Ok(Self { _file: file })
    }
}

/// Whether some process currently holds the lock at `path` — i.e. the session
/// is alive. A missing file, or a file nobody has locked, both read as **not
/// held** (dead).
pub fn is_held(path: &Path) -> bool {
    let file = match OpenOptions::new().read(true).write(true).open(path) {
        Ok(f) => f,
        // No lock file at all ⇒ never started or already reaped ⇒ not held.
        Err(_) => return false,
    };
    // Try to take it *shared*, non-blocking. A live session holds it exclusive,
    // so a shared request is denied exactly while the session lives. Probing
    // shared (not exclusive) is what keeps concurrent probers from mistaking
    // each other for the session: shared locks coexist, so a TUI refresh, the
    // poller and a `shelbi session ls` probing the same dead session at once all
    // read it as dead. With an exclusive probe, whichever prober lost the race
    // saw the winner's momentary lock as "held", and a dead session read as
    // alive (`phantom-working-dead-session`).
    let fd = file.as_raw_fd();
    let got = unsafe { libc::flock(fd, libc::LOCK_SH | libc::LOCK_NB) } == 0;
    if got {
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
        false
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn held_while_lock_is_alive_then_dead_after_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");

        assert!(!is_held(&path), "no file yet ⇒ not held");

        let lock = SessionLock::acquire(&path).expect("acquire");
        assert!(is_held(&path), "held while the lock lives");

        drop(lock);
        assert!(!is_held(&path), "released after drop ⇒ dead");
    }

    #[test]
    fn a_concurrent_probe_never_makes_a_dead_session_read_alive() {
        // Another prober mid-probe holds the same shared lock `is_held` takes.
        // That must not read as the session's exclusive lifetime lock.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        std::fs::write(&path, b"").unwrap();
        let other = OpenOptions::new().read(true).write(true).open(&path).unwrap();
        let rc = unsafe { libc::flock(other.as_raw_fd(), libc::LOCK_SH | libc::LOCK_NB) };
        assert_eq!(rc, 0, "a concurrent prober takes the shared lock");
        assert!(!is_held(&path), "a prober's lock is not a live session");
    }

    #[test]
    fn second_acquire_fails_while_first_is_held() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("lock");
        let _first = SessionLock::acquire(&path).expect("first acquire");
        assert!(
            SessionLock::acquire(&path).is_err(),
            "a second acquire must fail while the first is held"
        );
    }
}
