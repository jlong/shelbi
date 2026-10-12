//! Cross-process "relaunch this pane now" markers
//! (`rt-auto-restart-killed-panes`).
//!
//! The daemon poller owns the restart budget for the *persistent* panes it
//! supervises (the orchestrator, the workspace agents, the review agent slot),
//! and that budget lives in-memory on the supervisor thread. Once the
//! crash-loop cap trips it latches `gave-up` and leaves the pane dead — the
//! auto-restarts are spent.
//!
//! When the user re-opens such a pane in the TUI ("navigate away and back")
//! the pane should relaunch with a *fresh* budget. The TUI client and the
//! daemon are different processes, so the client can't reach the poller's
//! in-memory state directly. Instead it drops a small marker file here; each
//! supervision pass consumes its own key on the next tick and re-arms its state
//! machine ([`crate::...`] — see `shelbi_orchestrator::supervision`'s
//! `request_relaunch`), so the dead pane relaunches immediately with the budget
//! reset.
//!
//! A pane can be governed by more than one pass (a dev workspace by both the
//! per-workspace supervisor and the stranded-dev resume pass), so each pass
//! owns a *distinct* key and the client writes every key that could apply to
//! the pane it re-opened. Consuming is therefore unambiguous — exactly one pass
//! reads each marker — and a key no pass reads simply ages out.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use shelbi_core::Result;

/// Freshness window on a relaunch-request marker. It must survive at least one
/// supervisor tick (5 s) so the pass that owns the key sees it, and be short
/// enough that a marker left by a reopen whose pass never ran (the pane was
/// never actually given up) can't resurrect a budget reset much later. The
/// supervisor tick is 5 s; 60 s covers a slow/busy daemon without lingering.
pub const RELAUNCH_REQUEST_MAX_AGE: Duration = Duration::from_secs(60);

/// `<project_dir>/supervision-relaunch/`.
fn relaunch_dir(project: &str) -> Result<PathBuf> {
    Ok(crate::project_dir(project)?.join("supervision-relaunch"))
}

/// The marker filename for `key`, a flat token. Non-flat characters (notably
/// the `/` in a composed key) are folded to `-` so the key is always a single
/// path component, and the daemon and client build it the same way.
fn relaunch_marker_path(project: &str, key: &str) -> Result<PathBuf> {
    let flat: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '-' })
        .collect();
    Ok(relaunch_dir(project)?.join(flat))
}

/// The key the per-project orchestrator supervisor
/// (`maybe_supervise_orchestrator`) consumes.
pub fn orchestrator_relaunch_key() -> String {
    "orch".to_string()
}

/// The key the per-workspace pane supervisor (`maybe_supervise_workspace`)
/// consumes for a dev workspace agent pane.
pub fn workspace_supervise_relaunch_key(workspace: &str) -> String {
    format!("ws-sup-{workspace}")
}

/// The key the stranded-dev resume pass (`maybe_resume_stranded_dev_slots`)
/// consumes for a dev workspace agent pane re-opened after a `quit`.
pub fn workspace_resume_relaunch_key(workspace: &str) -> String {
    format!("ws-resume-{workspace}")
}

/// The key the stranded-review resume pass (`maybe_resume_stranded_review_slots`)
/// consumes for a review agent slot.
pub fn review_resume_relaunch_key(workspace: &str) -> String {
    format!("review-resume-{workspace}")
}

/// Drop a relaunch request for `key` in `project`. Best-effort: a failed write
/// just means the reopen doesn't re-arm the daemon this time and the pane stays
/// dead until the next reopen, which is degraded, not broken.
pub fn request_supervision_relaunch(project: &str, key: &str) -> Result<()> {
    let path = relaunch_marker_path(project, key)?;
    if let Some(parent) = path.parent() {
        crate::ensure_dir(parent)?;
    }
    // Empty body — presence + the kernel-recorded mtime are the whole signal,
    // mirroring the expected-teardown marker.
    crate::atomic_write(&path, b"")
}

/// If a fresh (< [`RELAUNCH_REQUEST_MAX_AGE`]) relaunch request for `key`
/// exists, remove it and return `true` (the owning supervision pass should
/// re-arm its state machine). A stale marker is removed and reads `false`; an
/// absent one reads `false`. Always deleting on read keeps a marker from
/// re-arming the budget on more than one tick.
pub fn consume_supervision_relaunch(project: &str, key: &str) -> Result<bool> {
    let path = relaunch_marker_path(project, key)?;
    let mtime = match fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let fresh = SystemTime::now()
        .duration_since(mtime)
        .map(|elapsed| elapsed < RELAUNCH_REQUEST_MAX_AGE)
        // Clock skew (mtime in the future): treat as fresh — honoring an extra
        // reopen is harmless, dropping a real one is not.
        .unwrap_or(true);
    let _ = fs::remove_file(&path);
    Ok(fresh)
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
    /// Mount a throwaway `SHELBI_HOME` under the shared test lock, so the
    /// process-global env var doesn't race a concurrent home-mutating test
    /// (the crate's documented isolation guard).
    fn fresh_home() -> HomeGuard {
        let guard = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = std::env::temp_dir().join(format!(
            "shelbi-supervision-relaunch-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        HomeGuard(guard)
    }

    #[test]
    fn keys_are_distinct_per_pass() {
        assert_eq!(orchestrator_relaunch_key(), "orch");
        assert_eq!(workspace_supervise_relaunch_key("alpha"), "ws-sup-alpha");
        assert_eq!(workspace_resume_relaunch_key("alpha"), "ws-resume-alpha");
        assert_eq!(review_resume_relaunch_key("rev"), "review-resume-rev");
        // No two keys for the same workspace collide.
        let w = "alpha";
        let keys = [
            workspace_supervise_relaunch_key(w),
            workspace_resume_relaunch_key(w),
            review_resume_relaunch_key(w),
        ];
        let unique: std::collections::BTreeSet<_> = keys.iter().collect();
        assert_eq!(unique.len(), keys.len());
    }

    #[test]
    fn request_then_consume_is_true_once() {
        let _h = fresh_home();
        let p = "proj";
        let key = workspace_supervise_relaunch_key("alpha");
        assert!(!consume_supervision_relaunch(p, &key).unwrap(), "absent → false");
        request_supervision_relaunch(p, &key).unwrap();
        assert!(consume_supervision_relaunch(p, &key).unwrap(), "fresh → true");
        assert!(
            !consume_supervision_relaunch(p, &key).unwrap(),
            "consumed → false on the next read"
        );
    }

    #[test]
    fn a_key_is_independent_of_other_keys() {
        let _h = fresh_home();
        let p = "proj";
        request_supervision_relaunch(p, &orchestrator_relaunch_key()).unwrap();
        // Consuming a different key doesn't clear the orchestrator's.
        assert!(!consume_supervision_relaunch(p, "ws-sup-alpha").unwrap());
        assert!(consume_supervision_relaunch(p, &orchestrator_relaunch_key()).unwrap());
    }
}
