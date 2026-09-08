//! The on-demand done/canceled history page (`<project_dir>/done-history.json`).
//!
//! Phase 2 §4 of `Plans/github-issue-caching-and-rate-limits.md`: the done and
//! canceled columns are history — hundreds of closed issues that only the Issues
//! board ever shows — so they must never ride the refresh tick. This module owns
//! the **format and IO** of the first-page cache the terminal columns are served
//! from: the type, the atomic write, the read, freshness against a long TTL, and
//! the write-through patch a merge/cancel applies.
//!
//! ## Why a separate file from `board-index.json`
//!
//! The daemon-owned [`crate::board_index`] is the *open* board, rewritten every
//! tick. Closed history is the opposite: nearly static, expensive to sweep, and
//! read only by the Issues board / `issue list --status done|canceled` / the Zen
//! done-history judgment. Caching it here — separately, with its own `fetched_at`
//! and a ten-minute TTL — keeps the frequently-rewritten open index lean and lets
//! a cold CLI one-shot serve the terminal columns straight from disk without a
//! backend request, as long as the page is under ten minutes old. A merge or
//! cancel done through shelbi patches the just-completed issue in immediately
//! (write-through), so the operator's own completion shows at the top of the
//! column before the TTL expires.
//!
//! This is a single **page** (the first 50, newest-closed first — see
//! [`crate::issue_store::ClosedPage`]); "load more" pages fetched interactively
//! are live and uncached. The page is exactly what the store produced, so it is
//! stored as a `Vec<IssueFile>` for the same reasons [`crate::board_index`]
//! documents.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::Result;

use crate::issue_store::{is_terminal_column, ClosedPage};
use crate::IssueFile;

/// Basename of the on-demand closed-history page, under the project's state dir
/// (`<shelbi-root>/projects/<project>/`). JSON so a torn write is a clean parse
/// failure (→ treated as absent), the same discipline the board index uses.
pub const DONE_HISTORY_FILE: &str = "done-history.json";

/// How long a cached done-history page is served before a fresh backend fetch is
/// due — ten minutes (`Plans/github-issue-caching-and-rate-limits.md` §4). Far
/// longer than the open board's cadence: the terminal history barely changes,
/// only the Issues board reads it, and a closed sweep is expensive. A merge or
/// cancel through shelbi patches the page immediately (write-through), so an
/// operator's own completion never waits out this window.
pub const DONE_HISTORY_TTL: Duration = Duration::from_secs(600);

/// The on-disk first-page cache of the terminal `done`/`canceled` history.
///
/// Written whenever the first page is fetched from the backend, read by every
/// terminal-column consumer, and patched in place by a merge/cancel write-through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoneHistory {
    /// The first page of closed issues, newest-closed (`updatedAt`) first.
    pub issues: Vec<IssueFile>,
    /// Cursor for the next page, or `None` when the first page is the whole
    /// history. Carried so a consumer knows whether to offer "load more".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// RFC3339 timestamp of the read that produced this page. The freshness
    /// signal: a page younger than [`DONE_HISTORY_TTL`] is served without a
    /// backend request.
    pub fetched_at: String,
    /// Remaining API budget on the token behind the read, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// Unix epoch seconds at which the token's budget resets, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<i64>,
}

impl DoneHistory {
    /// Wrap a freshly-fetched [`ClosedPage`] with a `fetched_at` of now.
    pub fn from_page(page: &ClosedPage) -> Self {
        Self {
            issues: page.issues.clone(),
            next_cursor: page.next_cursor.clone(),
            fetched_at: Utc::now().to_rfc3339(),
            remaining: page.remaining,
            reset: page.reset,
        }
    }

    /// Reconstruct the [`ClosedPage`] a consumer serves from this cached page.
    pub fn to_page(&self) -> ClosedPage {
        ClosedPage {
            issues: self.issues.clone(),
            next_cursor: self.next_cursor.clone(),
            remaining: self.remaining,
            reset: self.reset,
        }
    }

    /// Whether this page is still fresh — its `fetched_at` is within
    /// [`DONE_HISTORY_TTL`]. An unparseable timestamp is treated as stale (we
    /// can't vouch for its age), so a corrupt value forces a re-fetch rather
    /// than serving indefinitely.
    pub fn is_fresh(&self) -> bool {
        let Ok(ts) = DateTime::parse_from_rfc3339(&self.fetched_at) else {
            return false;
        };
        let age = Utc::now().signed_duration_since(ts.with_timezone(&Utc));
        age.to_std().is_ok_and(|a| a < DONE_HISTORY_TTL)
    }
}

/// The on-disk done-history path for `project`, under its state dir. Errors only
/// when the shelbi root can't be resolved or the name is invalid.
pub fn done_history_path(project: &str) -> Result<PathBuf> {
    Ok(crate::project_dir(project)?.join(DONE_HISTORY_FILE))
}

/// Persist `history` for `project` atomically (temp file + rename), so a process
/// killed mid-write leaves either the old page or the new one, never a truncated
/// file.
pub fn write_done_history(project: &str, history: &DoneHistory) -> Result<()> {
    let path = done_history_path(project)?;
    let bytes = serde_json::to_vec_pretty(history)
        .map_err(|e| shelbi_core::Error::Other(format!("serializing done history: {e}")))?;
    crate::atomic_write(&path, &bytes)
}

/// Read and parse `project`'s done-history page. A missing, unreadable, or
/// torn/corrupt file all resolve to `None` — a cache miss the caller recovers
/// from with a live fetch, never an error.
pub fn read_done_history(project: &str) -> Option<DoneHistory> {
    read_done_history_at(&done_history_path(project).ok()?)
}

/// [`read_done_history`] against an explicit path — consumers read by project
/// name; tests read a temp path directly.
pub fn read_done_history_at(path: &Path) -> Option<DoneHistory> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Write-through a freshly-completed issue to the top of the cached page — the
/// §4 write-through so an operator's own merge/cancel shows up immediately
/// instead of waiting out the ten-minute TTL.
///
/// The issue is placed **first** (newest-closed), replacing any existing entry
/// with the same id (a re-completion), so it renders at the top of the column.
/// The page's `fetched_at` and cursor are preserved — only the daemon-cadence
/// fetch advances freshness; this is a targeted splice. A no-op when no page has
/// been cached yet (nothing to patch — the first fetch will include it).
pub fn patch_done_history_issue(project: &str, issue: &IssueFile) -> Result<()> {
    let Some(mut history) = read_done_history(project) else {
        return Ok(());
    };
    history.issues.retain(|f| f.task.id != issue.task.id);
    history.issues.insert(0, issue.clone());
    write_done_history(project, &history)
}

/// Drop an issue from the cached page — the write-through for a card that left
/// the terminal history (a reopen, or a delete). A no-op when no page is cached
/// or the id isn't in it.
pub fn remove_done_history_issue(project: &str, id: &str) -> Result<()> {
    let Some(mut history) = read_done_history(project) else {
        return Ok(());
    };
    let before = history.issues.len();
    history.issues.retain(|f| f.task.id != id);
    if history.issues.len() == before {
        return Ok(());
    }
    write_done_history(project, &history)
}

/// The terminal cards from a cached page filtered to one `status` column — what
/// `list_in_status(done)` / `list_in_status(canceled)` serve. Relative order
/// (newest-closed first) is preserved.
pub fn page_in_status(page: &ClosedPage, status: &shelbi_core::Column) -> Vec<IssueFile> {
    page.issues
        .iter()
        .filter(|f| &f.task.column == status && is_terminal_column(&f.task.column))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{Column, Issue};

    fn issue(id: &str, column: &str) -> IssueFile {
        let task: Issue = serde_yaml::from_str(&format!(
            "id: {id}\ntitle: {id}\ncolumn: {column}\npriority: 0\n\
             created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n"
        ))
        .expect("issue fixture parses");
        IssueFile {
            task,
            body: String::new(),
        }
    }

    fn temp_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "shelbi-done-history-{}-{}.json",
            std::process::id(),
            tag
        ))
    }

    fn page(issues: Vec<IssueFile>, next: Option<&str>) -> ClosedPage {
        ClosedPage {
            issues,
            next_cursor: next.map(str::to_string),
            remaining: None,
            reset: None,
        }
    }

    #[test]
    fn write_then_read_round_trips_through_disk() {
        let path = temp_path("round-trip");
        let _ = std::fs::remove_file(&path);
        let history = DoneHistory::from_page(&page(
            vec![issue("a", "done"), issue("b", "canceled")],
            Some("cursor-2"),
        ));
        let bytes = serde_json::to_vec_pretty(&history).unwrap();
        crate::atomic_write(&path, &bytes).unwrap();

        let back = read_done_history_at(&path).expect("round-trips");
        assert_eq!(back.issues.len(), 2);
        assert_eq!(back.next_cursor.as_deref(), Some("cursor-2"));
        assert_eq!(back.fetched_at, history.fetched_at);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_torn_page_reads_as_absent_not_an_error() {
        let path = temp_path("torn");
        std::fs::write(&path, b"{\"issues\": [ {\"task\": {\"id\": \"a\", tru").unwrap();
        assert!(read_done_history_at(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_fresh_page_is_fresh_and_an_old_one_is_not() {
        let fresh = DoneHistory::from_page(&page(vec![issue("a", "done")], None));
        assert!(fresh.is_fresh(), "a just-fetched page is fresh");

        let mut old = fresh.clone();
        old.fetched_at = (Utc::now() - chrono::Duration::seconds(601)).to_rfc3339();
        assert!(!old.is_fresh(), "a page older than the 10-minute TTL is stale");

        let mut garbage = fresh;
        garbage.fetched_at = "not-a-timestamp".into();
        assert!(!garbage.is_fresh(), "an unparseable timestamp is stale");
    }

    #[test]
    fn page_in_status_filters_to_one_terminal_column() {
        let p = page(
            vec![issue("d1", "done"), issue("c1", "canceled"), issue("d2", "done")],
            None,
        );
        let done = page_in_status(&p, &Column::done());
        assert_eq!(done.len(), 2);
        assert!(done.iter().all(|f| f.task.column == Column::done()));
        assert_eq!(page_in_status(&p, &Column::canceled()).len(), 1);
    }

    // ----- write-through --------------------------------------------------

    /// An isolated `SHELBI_HOME` so a test's done-history writes land in a temp
    /// dir, never the developer's real `~/.shelbi`. Holds the crate-wide test
    /// lock because `set_var` is process-global.
    struct IsolatedHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        home: PathBuf,
    }
    impl IsolatedHome {
        fn new(tag: &str) -> Self {
            let lock = crate::test_lock::LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-done-history-home-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
            let prev = std::env::var("SHELBI_HOME").ok();
            std::env::set_var("SHELBI_HOME", &home);
            Self {
                _lock: lock,
                prev,
                home,
            }
        }
    }
    impl Drop for IsolatedHome {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn patch_places_a_completion_at_the_top_and_dedupes() {
        let _iso = IsolatedHome::new("patch");
        let original = DoneHistory::from_page(&page(
            vec![issue("old", "done"), issue("older", "done")],
            None,
        ));
        write_done_history("proj", &original).unwrap();

        // A just-merged task lands at the top.
        patch_done_history_issue("proj", &issue("fresh", "done")).unwrap();
        let back = read_done_history("proj").unwrap();
        assert_eq!(back.issues[0].task.id, "fresh", "the merge shows at the top");
        assert_eq!(back.issues.len(), 3);
        // fetched_at is preserved (only the cadence fetch advances freshness).
        assert_eq!(back.fetched_at, original.fetched_at);

        // Re-completing an already-present card moves it to the top, no dup.
        patch_done_history_issue("proj", &issue("older", "canceled")).unwrap();
        let back = read_done_history("proj").unwrap();
        assert_eq!(back.issues[0].task.id, "older");
        assert_eq!(back.issues[0].task.column.as_str(), "canceled");
        assert_eq!(back.issues.len(), 3, "no duplicate entry");
    }

    #[test]
    fn remove_drops_a_reopened_card_and_is_a_noop_for_unknown() {
        let _iso = IsolatedHome::new("remove");
        write_done_history(
            "proj",
            &DoneHistory::from_page(&page(vec![issue("a", "done"), issue("b", "done")], None)),
        )
        .unwrap();

        remove_done_history_issue("proj", "a").unwrap();
        let back = read_done_history("proj").unwrap();
        assert_eq!(back.issues.len(), 1);
        assert_eq!(back.issues[0].task.id, "b");

        remove_done_history_issue("proj", "ghost").unwrap();
        assert_eq!(read_done_history("proj").unwrap().issues.len(), 1);
    }

    #[test]
    fn patch_and_remove_are_noops_when_no_page_is_cached() {
        let _iso = IsolatedHome::new("nopage");
        patch_done_history_issue("proj", &issue("a", "done")).unwrap();
        remove_done_history_issue("proj", "a").unwrap();
        assert!(read_done_history("proj").is_none());
    }
}
