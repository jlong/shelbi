//! The hub-owned board index (`<project_dir>/board-index.json`).
//!
//! Phase 1 of `Plans/github-issue-caching-and-rate-limits.md` §5 makes the
//! `shelbi daemon` the single board reader per hub: one refresh loop per open
//! project reads the board through the store and publishes it here, and every
//! *list* consumer (sidebar, Issues board, pollers, `events next`, `zen scan`,
//! `issue list`) reads this file instead of hitting GitHub on its own cadence.
//!
//! This module owns the file's **format and IO** — the type, the atomic write,
//! the read, and the change-count diff the daemon uses to decide whether a
//! refresh is worth an events.log line. The refresh *loop* and the
//! `refresh-board` hub message live in the daemon (`shelbi-cli`); the consumer
//! read-through lands in a later slice (`gh-cache-p1-consumers-read-index`).
//!
//! ## Relationship to `board-snapshot.json`
//!
//! [`crate::issue_cache`] already persists a per-process `board-snapshot.json`
//! (a bare `Vec<IssueFile>`) so a cold process paints its last-known board
//! without a blocking sweep. The board index is the hub-wide successor: it
//! wraps the same board data in a freshness/quota envelope (`fetched_at`,
//! `stale`, and the token's `remaining`/`reset`) so a consumer can render
//! "stale, last refreshed HH:MM" and a governor can read the budget without a
//! second call. The two coexist during the migration; consumers move to the
//! index in a follow-up.
//!
//! ## Why the board is `Vec<IssueFile>` and not per-field entries
//!
//! The plan's index carries `number`, `id`, `title`, `labels`, `state`,
//! `updated_at` and the fenced metadata block. In Phase 1 the daemon still
//! reads **through the store** ([`crate::issue_store::IssueStore::list_open`]),
//! which yields [`IssueFile`]s — id, title, column (the status/state), priority
//! and the metadata block all ride along, losslessly. GitHub's `number` and raw
//! `labels` are not surfaced by the store trait yet; Phase 2's GraphQL reader
//! adds them. Storing the `IssueFile` list keeps this slice honest (it persists
//! exactly what the store produced) and lossless (nothing a consumer needs is
//! dropped), and matches the shape every list consumer already understands.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};

use shelbi_core::Result;

use crate::IssueFile;

/// Basename of the hub-owned board index, under the project's state dir
/// (`<shelbi-root>/projects/<project>/`). JSON so a torn write is a clean parse
/// failure (→ treated as absent), not silently-valid garbage — the same
/// discipline [`crate::issue_cache`] uses for `board-snapshot.json`.
pub const BOARD_INDEX_FILE: &str = "board-index.json";

/// The hub-owned, on-disk board index for one project.
///
/// Written by the daemon's refresh loop and read by every list consumer. The
/// `board` is the open-issue set exactly as the store produced it; the envelope
/// fields describe *this* read's freshness and the token budget behind it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BoardIndex {
    /// The open board as read through the store: each [`IssueFile`] carries id,
    /// title, column (status/state), priority and the fenced metadata block.
    /// See the module docs for why this is a `Vec<IssueFile>` rather than
    /// per-field index entries in Phase 1.
    pub board: Vec<IssueFile>,
    /// RFC3339 timestamp of the read that produced `board`. Advances every tick
    /// (the daemon rewrites the file each refresh), so a consumer reads it — or
    /// the file mtime — as the freshness signal.
    pub fetched_at: String,
    /// True when this index is known to lag the backend — a refresh failed and
    /// the previous board was carried forward. A fresh successful read clears
    /// it. Consumers render a "stale" banner on this; destructive poller
    /// actions must refuse to act on a `stale` index (Phase 3 wires that rule).
    #[serde(default)]
    pub stale: bool,
    /// Remaining API budget on the token behind the last read, when known.
    /// `None` on the Phase 1 REST path (the rate-limit headers aren't parsed
    /// until the Phase 3 governor); populated once the reader surfaces them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining: Option<u64>,
    /// Unix epoch seconds at which the token's budget resets, when known. Same
    /// `None`-until-Phase-3 story as [`remaining`](Self::remaining).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset: Option<i64>,
}

impl BoardIndex {
    /// Build a fresh index from a board just read from the backend:
    /// `fetched_at` is now, `stale` is false, and the budget fields are unset.
    /// The convenience shape for a read that surfaces no budget or timestamp of
    /// its own; the GraphQL board path uses [`BoardIndex::fresh_at`] to carry the
    /// budget and a pre-read watermark.
    pub fn fresh(board: Vec<IssueFile>) -> Self {
        Self::fresh_at(board, Utc::now().to_rfc3339(), None, None)
    }

    /// Build a fresh (non-stale) index with an explicit `fetched_at` and the
    /// token budget the read observed.
    ///
    /// `fetched_at` is captured by the daemon **before** it issues the read, so
    /// it is a safe lower bound on the backend state this index reflects: the
    /// next incremental tick uses it as the `since` watermark, and an update that
    /// lands while this read is in flight (`updated_at >= fetched_at`) is caught
    /// on that next tick rather than skipped. `remaining` / `reset` are the
    /// GraphQL `rateLimit` numbers, `None` when the backend didn't surface them.
    pub fn fresh_at(
        board: Vec<IssueFile>,
        fetched_at: String,
        remaining: Option<u64>,
        reset: Option<i64>,
    ) -> Self {
        Self {
            board,
            fetched_at,
            stale: false,
            remaining,
            reset,
        }
    }
}

/// The on-disk board-index path for `project`, under its state dir. Errors only
/// when the shelbi root can't be resolved or the name is invalid — the same
/// validation [`crate::project_dir`] applies to every per-project state path.
pub fn board_index_path(project: &str) -> Result<PathBuf> {
    Ok(crate::project_dir(project)?.join(BOARD_INDEX_FILE))
}

/// Persist `index` to `project`'s board-index file atomically (temp file +
/// rename via [`crate::atomic_write`]), so a daemon killed mid-write leaves
/// either the old index or the new one, never a truncated file.
pub fn write_board_index(project: &str, index: &BoardIndex) -> Result<()> {
    let path = board_index_path(project)?;
    let bytes = serde_json::to_vec_pretty(index)
        .map_err(|e| shelbi_core::Error::Other(format!("serializing board index: {e}")))?;
    crate::atomic_write(&path, &bytes)
}

/// Read and parse `project`'s board index. A missing file, an unreadable file,
/// or a torn/corrupt one (truncated mid-write, invalid JSON) all resolve to
/// `None` — the daemon treats that as "no prior index" (everything counts as
/// changed) and a consumer treats it as a cache miss, never an error.
pub fn read_board_index(project: &str) -> Option<BoardIndex> {
    let path = board_index_path(project).ok()?;
    read_board_index_at(&path)
}

/// [`read_board_index`] against an explicit path — the daemon reads by project
/// name; tests read a temp path directly.
pub fn read_board_index_at(path: &Path) -> Option<BoardIndex> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// How many issues differ between an old and a new board — the `changed=<n>`
/// the daemon reports (and its zero/non-zero decides whether a tick is worth an
/// events.log line at all).
///
/// An issue counts as changed when it is added, removed, or its stored form
/// differs (title, column, priority, metadata block, …). Keyed by task id;
/// order-only churn does **not** count, since the store returns a canonical
/// column-then-priority order and a pure reorder still moves an issue's
/// `priority`, which the per-issue comparison already catches. A `None` old
/// board (first tick, or a torn prior file) makes every issue count as new.
pub fn board_diff_count(old: Option<&[IssueFile]>, new: &[IssueFile]) -> usize {
    use std::collections::HashMap;
    let Some(old) = old else {
        return new.len();
    };
    let old_by_id: HashMap<&str, &IssueFile> =
        old.iter().map(|f| (f.task.id.as_str(), f)).collect();
    let new_by_id: HashMap<&str, &IssueFile> =
        new.iter().map(|f| (f.task.id.as_str(), f)).collect();

    let mut changed = 0usize;
    // Added or content-changed.
    for f in new {
        match old_by_id.get(f.task.id.as_str()) {
            None => changed += 1,
            Some(prev) => {
                if !issue_files_eq(prev, f) {
                    changed += 1;
                }
            }
        }
    }
    // Removed (present in old, gone from new).
    for f in old {
        if !new_by_id.contains_key(f.task.id.as_str()) {
            changed += 1;
        }
    }
    changed
}

/// Structural equality of two [`IssueFile`]s for change detection. Compares the
/// serialized JSON so every field the index persists — typed [`Issue`] fields
/// and the body — participates without hand-maintaining a field list that would
/// silently rot as `Issue` grows. A serialize failure (practically
/// unreachable) conservatively reports "changed" so a real edit is never missed.
///
/// [`Issue`]: shelbi_core::Issue
fn issue_files_eq(a: &IssueFile, b: &IssueFile) -> bool {
    match (serde_json::to_vec(a), serde_json::to_vec(b)) {
        (Ok(ja), Ok(jb)) => ja == jb,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::Issue;

    fn issue(id: &str, column: &str, priority: u32) -> IssueFile {
        let task: Issue = serde_yaml::from_str(&format!(
            "id: {id}\ntitle: {id}\ncolumn: {column}\npriority: {priority}\n\
             created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n"
        ))
        .expect("issue fixture parses");
        IssueFile {
            task,
            body: String::new(),
        }
    }

    fn temp_index_path(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "shelbi-board-index-{}-{}.json",
            std::process::id(),
            tag
        ))
    }

    #[test]
    fn write_then_read_round_trips_through_disk() {
        // The atomic write + read round-trip: what the daemon publishes is
        // exactly what a consumer reads back.
        let path = temp_index_path("round-trip");
        let _ = std::fs::remove_file(&path);
        let index = BoardIndex::fresh(vec![issue("a", "todo", 0), issue("b", "review", 1)]);
        let bytes = serde_json::to_vec_pretty(&index).unwrap();
        crate::atomic_write(&path, &bytes).unwrap();

        let back = read_board_index_at(&path).expect("index round-trips");
        assert_eq!(back.board.len(), 2);
        assert_eq!(back.board[0].task.id, "a");
        assert_eq!(back.board[1].task.column.as_str(), "review");
        assert_eq!(back.fetched_at, index.fetched_at);
        assert!(!back.stale);
        assert_eq!(back.remaining, None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_torn_index_reads_as_absent_not_an_error() {
        // A file truncated mid-write (invalid JSON) must read as `None` — a
        // recoverable "no prior index", never a parse error surfaced upward.
        let path = temp_index_path("torn");
        std::fs::write(&path, b"{\"board\": [ {\"task\": {\"id\": \"a\", tru").unwrap();
        assert!(read_board_index_at(&path).is_none());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn fresh_index_is_not_stale_and_has_a_timestamp() {
        let index = BoardIndex::fresh(vec![issue("a", "todo", 0)]);
        assert!(!index.stale);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&index.fetched_at).is_ok(),
            "fetched_at is RFC3339: {}",
            index.fetched_at
        );
    }

    #[test]
    fn diff_counts_a_first_tick_as_all_new() {
        let new = vec![issue("a", "todo", 0), issue("b", "todo", 1)];
        assert_eq!(board_diff_count(None, &new), 2, "no prior index ⇒ all new");
    }

    #[test]
    fn diff_is_zero_for_an_identical_board() {
        let board = vec![issue("a", "todo", 0), issue("b", "review", 0)];
        assert_eq!(
            board_diff_count(Some(&board.clone()), &board),
            0,
            "a quiet tick reports no change"
        );
    }

    #[test]
    fn diff_counts_added_removed_and_edited() {
        let old = vec![issue("a", "todo", 0), issue("b", "review", 0)];
        // `a` moved column (edited), `b` removed, `c` added.
        let new = vec![issue("a", "in_progress", 0), issue("c", "todo", 0)];
        // a edited (1) + c added (1) + b removed (1) = 3.
        assert_eq!(board_diff_count(Some(&old), &new), 3);
    }

    #[test]
    fn diff_catches_a_move_that_only_changes_priority() {
        // A reprioritize keeps the same ids and columns but changes priority;
        // the per-issue comparison must catch it so the sidebar's ordering
        // update earns its `board refreshed` line.
        let old = vec![issue("a", "todo", 0), issue("b", "todo", 1)];
        let new = vec![issue("a", "todo", 1), issue("b", "todo", 0)];
        assert_eq!(board_diff_count(Some(&old), &new), 2);
    }
}
