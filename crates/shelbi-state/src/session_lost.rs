//! The durable "the daemon lost its macOS login session" marker.
//!
//! Detection runs inside the hub daemon (see the daemon's `session_health`
//! module), but the *warning* has to reach every attached TUI, `shelbi status`,
//! and `shelbi doctor` — none of which share the daemon's process. So the daemon
//! records the condition as a small JSON file under the shelbi root and the
//! surfaces read it back. Clearing the file (on recovery, or a clean reopen)
//! retracts the warning everywhere at once.
//!
//! The file also carries the recovery bookkeeping: whether the daemon has
//! already tried to re-exec itself into the live GUI session. A fresh daemon
//! reads it at startup so an attempt that didn't take can't turn into a re-exec
//! loop.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use shelbi_core::Result;

/// The user-facing banner text. One canonical string so the TUI banner,
/// `shelbi status`, and `shelbi doctor` all say the same thing.
pub const SESSION_LOST_BANNER: &str =
    "Shelbi lost its macOS login session (you logged out/in). \
     Run `shelbi quit` and reopen to restore DNS, SSH and user lookups.";

/// How far the daemon has gotten trying to recover from a lost session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryState {
    /// Loss detected; no recovery attempted (warn-only mode, or not yet tried).
    NotAttempted,
    /// A re-exec into the GUI session was launched. A fresh daemon that finds
    /// this and is *still* lost escalates to [`RecoveryState::GaveUp`] rather
    /// than re-execing again.
    Attempted,
    /// Recovery was tried and did not restore a healthy session (or a second
    /// detection found the attempt hadn't taken). Warn-only from here.
    GaveUp,
}

/// The on-disk record describing a lost macOS login session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionLostRecord {
    /// Machine-readable reason tokens (e.g.
    /// `launchctl-managername-not-aqua,dns-unresolved`).
    pub reason: String,
    /// When the loss was first detected (RFC 3339).
    pub detected_at: String,
    /// How far recovery has progressed.
    pub recovery: RecoveryState,
}

impl SessionLostRecord {
    /// A freshly-detected record stamped `now`, with the given recovery state.
    pub fn now(reason: impl Into<String>, recovery: RecoveryState) -> Self {
        Self {
            reason: reason.into(),
            detected_at: chrono::Utc::now().to_rfc3339(),
            recovery,
        }
    }
}

/// Path to the marker file under the shelbi root.
pub fn session_lost_path() -> Result<PathBuf> {
    Ok(crate::root()?.join("daemon-session-lost.json"))
}

/// Write (or replace) the marker. Atomic, so a reader never sees a torn file.
pub fn write_session_lost(record: &SessionLostRecord) -> Result<()> {
    let path = session_lost_path()?;
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| shelbi_core::Error::Other(format!("serialize session-lost marker: {e}")))?;
    crate::atomic_write(&path, &bytes)
}

/// Read the marker, or `None` when it is absent or unreadable. Read-only and
/// resilient: any IO/parse error is treated as "no marker" so a surface never
/// fails to render over a transient read.
pub fn read_session_lost() -> Option<SessionLostRecord> {
    let path = session_lost_path().ok()?;
    let bytes = std::fs::read(&path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Whether a session-lost marker is currently present.
pub fn session_lost_active() -> bool {
    read_session_lost().is_some()
}

/// Remove the marker (retract the warning). A no-op when it is already absent.
pub fn clear_session_lost() -> Result<()> {
    let path = session_lost_path()?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Other(format!(
            "removing session-lost marker {}: {e}",
            path.display()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK;
    use std::path::PathBuf;

    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<std::ffi::OsString>,
        home: PathBuf,
    }
    impl HomeGuard {
        fn new(tag: &str) -> Self {
            let lock = LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-session-lost-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
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

    #[test]
    fn absent_marker_reads_as_none() {
        let _g = HomeGuard::new("absent");
        assert!(read_session_lost().is_none());
        assert!(!session_lost_active());
    }

    #[test]
    fn write_read_roundtrip_and_clear() {
        let _g = HomeGuard::new("roundtrip");
        let rec = SessionLostRecord::now("launchctl-managername-not-aqua", RecoveryState::Attempted);
        write_session_lost(&rec).unwrap();
        assert!(session_lost_active());
        let back = read_session_lost().unwrap();
        assert_eq!(back.reason, "launchctl-managername-not-aqua");
        assert_eq!(back.recovery, RecoveryState::Attempted);
        assert!(!back.detected_at.is_empty());

        clear_session_lost().unwrap();
        assert!(read_session_lost().is_none());
        // Clearing an already-absent marker is a no-op.
        clear_session_lost().unwrap();
    }

    #[test]
    fn write_replaces_the_prior_record() {
        let _g = HomeGuard::new("replace");
        write_session_lost(&SessionLostRecord::now("a", RecoveryState::NotAttempted)).unwrap();
        write_session_lost(&SessionLostRecord::now("b", RecoveryState::GaveUp)).unwrap();
        let back = read_session_lost().unwrap();
        assert_eq!(back.reason, "b");
        assert_eq!(back.recovery, RecoveryState::GaveUp);
    }
}
