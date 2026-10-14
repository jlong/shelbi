//! The orchestrator supervisor's user-facing status
//! (`orch-relaunch-noop-after-session-death`).
//!
//! The daemon poller relaunches a crashed orchestrator, and when a relaunch
//! fails (or the crash-loop cap trips) the reason must reach the user, not just
//! `events.log`. The supervisor runs in the daemon and the sidebar in the TUI,
//! so the reason crosses processes as a one-line file here: the poller writes it
//! on `restart-failed` / `gave-up`, and clears it once the orchestrator is seen
//! alive again (or a bring-up succeeds). The TUI's sidebar refresher reads it
//! and paints it on the orchestrator's status row.

use std::fs;
use std::path::PathBuf;

use shelbi_core::Result;

/// `<project_dir>/supervision-status/orch`.
fn orchestrator_status_path(project: &str) -> Result<PathBuf> {
    Ok(crate::project_dir(project)?
        .join("supervision-status")
        .join("orch"))
}

/// Record the orchestrator supervisor's current status line (e.g.
/// `supervisor gave up: <reason>`). Newlines are folded to spaces so the file
/// always holds a single line.
pub fn write_orchestrator_supervision_status(project: &str, status: &str) -> Result<()> {
    let path = orchestrator_status_path(project)?;
    if let Some(parent) = path.parent() {
        crate::ensure_dir(parent)?;
    }
    let line: String = status
        .chars()
        .map(|c| if c == '\n' || c == '\r' { ' ' } else { c })
        .collect();
    crate::atomic_write(&path, line.trim().as_bytes())
}

/// Clear the orchestrator supervisor's status. An absent file is fine.
pub fn clear_orchestrator_supervision_status(project: &str) -> Result<()> {
    let path = orchestrator_status_path(project)?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

/// The orchestrator supervisor's current status line, or `None` when the
/// orchestrator is healthy (no file, an empty file, or an unreadable one).
pub fn read_orchestrator_supervision_status(project: &str) -> Option<String> {
    let text = fs::read_to_string(orchestrator_status_path(project).ok()?).ok()?;
    let line = text.trim();
    (!line.is_empty()).then(|| line.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::test_lock::LOCK as TEST_LOCK;

    struct HomeGuard(#[allow(dead_code)] std::sync::MutexGuard<'static, ()>);
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
        }
    }

    fn fresh_home() -> HomeGuard {
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-supervision-status-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        HomeGuard(guard)
    }

    #[test]
    fn status_round_trips_and_clears() {
        let _home = fresh_home();
        assert_eq!(read_orchestrator_supervision_status("p"), None);
        write_orchestrator_supervision_status("p", "supervisor gave up: spawn failed\nboom")
            .unwrap();
        assert_eq!(
            read_orchestrator_supervision_status("p").as_deref(),
            Some("supervisor gave up: spawn failed boom"),
        );
        // Scoped per project.
        assert_eq!(read_orchestrator_supervision_status("other"), None);
        clear_orchestrator_supervision_status("p").unwrap();
        assert_eq!(read_orchestrator_supervision_status("p"), None);
        // Clearing an absent status is not an error.
        clear_orchestrator_supervision_status("p").unwrap();
    }
}
