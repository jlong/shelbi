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
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::{IssueTrackerConfig, Result};

use crate::issue_store::BoardState;
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
    /// The backend's native issue `number` for each open issue, keyed by shelbi
    /// id. This is the id→number map the single-issue fetch path resolves
    /// through (`Plans/github-issue-caching-and-rate-limits.md` §3): a `get(id)`
    /// on a remote backend looks the number up here and fetches that one issue in
    /// a single request, falling back to a label search only for an id the open
    /// index does not carry (a done task, or one added since the last tick).
    /// Empty on the `file_system` backend (no per-issue number) and on an index
    /// written by a pre-Phase-2 daemon, both of which force the search fallback.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub numbers: std::collections::BTreeMap<String, i64>,
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
        Self::fresh_at(board, Vec::new(), Utc::now().to_rfc3339(), None, None)
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
        numbers: Vec<(String, i64)>,
        fetched_at: String,
        remaining: Option<u64>,
        reset: Option<i64>,
    ) -> Self {
        Self {
            board,
            numbers: numbers.into_iter().collect(),
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

/// The shared read every **list** consumer uses instead of sweeping the backend
/// itself: the board the daemon published, tagged with its freshness. This is
/// the consumer-side half of `Plans/github-issue-caching-and-rate-limits.md` §5
/// — the sidebar, Issues board, workspace pollers, `zen scan`, the orchestrator
/// drain and `issue list` / `status` / `workspace list` all route their board
/// reads through here.
///
/// For a **remote** project the daemon is the single board reader per hub, so
/// this returns the persisted `board-index.json` and **never touches the
/// backend** — a consumer running inside a `shelbi __sidebar` / `__tasks` pane
/// spawns no `gh api` of its own. The index's `stale` flag and the age of its
/// `fetched_at` (against the project's refresh cadence) decide
/// [`BoardState::Warm`] vs [`BoardState::Stale`]; a missing or torn file is
/// [`BoardState::Cold`] (the daemon publishes one within a tick or two of the
/// hub opening).
///
/// For a **local** (`file_system`) project there is no daemon and no index — the
/// board is a cheap, authoritative directory read, so this returns
/// [`BoardState::Warm`] straight from disk, exactly as before.
pub fn read_board(project: &str) -> Result<BoardState> {
    let cfg = crate::load_project(project)?.issue_tracker;
    read_board_with_cfg(project, &cfg)
}

/// [`read_board`] for a caller that already holds the resolved
/// [`IssueTrackerConfig`] (a poll or render path with a `&Project` in hand),
/// saving the per-read project-YAML load.
pub fn read_board_with_cfg(project: &str, cfg: &IssueTrackerConfig) -> Result<BoardState> {
    if !cfg.backend.is_remote() {
        // Local board: a directory scan is cheap and always authoritative, and
        // there is no daemon-owned index to read. Serve it warm.
        let store = crate::issue_store::build_store(project, cfg)?;
        return Ok(BoardState::Warm(store.list_open()?));
    }
    Ok(board_state_from_index(project, cfg.refresh_interval_secs()))
}

/// Map the on-disk index to a [`BoardState`] from its `stale` flag and the age
/// of its `fetched_at` relative to `interval_secs` (the project's refresh
/// cadence). A missing or torn file is [`BoardState::Cold`]; an index the daemon
/// flagged stale, or one older than [`index_stale_threshold`] (the daemon has
/// missed several ticks — stopped, wedged, or rate-limited), is
/// [`BoardState::Stale`]; anything fresher is [`BoardState::Warm`].
fn board_state_from_index(project: &str, interval_secs: u64) -> BoardState {
    let Some(idx) = read_board_index(project) else {
        return BoardState::Cold;
    };
    if idx.stale || fetched_at_is_stale(&idx.fetched_at, interval_secs) {
        BoardState::Stale(idx.board)
    } else {
        BoardState::Warm(idx.board)
    }
}

/// How old a published index may get before it reads as [`BoardState::Stale`]:
/// three refresh intervals, with a 90s floor. The daemon rewrites the index
/// every `interval_secs` (so `fetched_at`/mtime advance even on a quiet board),
/// which means a *running* daemon keeps the index comfortably inside this
/// window; crossing it means several ticks have been missed and the board can
/// no longer be trusted as current. One or two missed ticks (network jitter,
/// a slow sweep) stay Warm so a healthy hub never flickers to a stale banner.
fn index_stale_threshold(interval_secs: u64) -> Duration {
    Duration::from_secs(interval_secs.saturating_mul(3).max(90))
}

/// Whether an index `fetched_at` timestamp is old enough — or unparseable
/// enough — to be treated as stale. An RFC3339 timestamp older than
/// [`index_stale_threshold`] is stale; a timestamp we can't parse is not
/// something we can vouch for as fresh, so it is stale too.
fn fetched_at_is_stale(fetched_at: &str, interval_secs: u64) -> bool {
    let Ok(ts) = DateTime::parse_from_rfc3339(fetched_at) else {
        return true;
    };
    let age = Utc::now().signed_duration_since(ts.with_timezone(&Utc));
    age.to_std()
        .is_ok_and(|a| a >= index_stale_threshold(interval_secs))
}

/// Splice a single freshly-mutated issue into the published index in place — the
/// write-through half of §5. After a mutation the writer holds a current copy of
/// the one issue it touched; patching it into `board-index.json` (with the same
/// atomic temp-file-and-rename [`write_board_index`] uses) lets the sidebar and
/// Issues board reflect the change on their next paint, before the next daemon
/// tick reconciles it.
///
/// The entry is matched by task id: replaced in place when present (a moved /
/// edited card), appended otherwise (a freshly-added card). The index's
/// freshness envelope (`fetched_at`, `stale`, budget) is preserved untouched —
/// only the daemon advances that. Relative ordering of a patched entry is left
/// as-is (the daemon's next tick restores the canonical column-then-priority
/// order); consumers that care order by column and priority themselves.
///
/// A no-op when no index has been published yet (nothing to patch — the first
/// tick will include the issue). Only meaningful for a remote project; a
/// `file_system` project has no index and never calls this.
pub fn patch_board_index_issue(project: &str, issue: &IssueFile) -> Result<()> {
    let Some(mut idx) = read_board_index(project) else {
        return Ok(());
    };
    match idx.board.iter_mut().find(|f| f.task.id == issue.task.id) {
        Some(existing) => *existing = issue.clone(),
        None => idx.board.push(issue.clone()),
    }
    write_board_index(project, &idx)
}

/// [`patch_board_index_issue`] that also records the issue's backend `number` in
/// the id→number map, in one atomic rewrite — the single-issue fetch's
/// write-through (§3 "reads also refresh that issue's entry in the index"). Used
/// when the writer holds both the freshly-read issue and its number, so a later
/// `get` in another process resolves the number without a search. `None` for a
/// backend with no per-issue number leaves the map untouched.
pub fn patch_board_index_issue_with_number(
    project: &str,
    issue: &IssueFile,
    number: Option<i64>,
) -> Result<()> {
    let Some(mut idx) = read_board_index(project) else {
        return Ok(());
    };
    match idx.board.iter_mut().find(|f| f.task.id == issue.task.id) {
        Some(existing) => *existing = issue.clone(),
        None => idx.board.push(issue.clone()),
    }
    if let Some(number) = number {
        idx.numbers.insert(issue.task.id.clone(), number);
    }
    write_board_index(project, &idx)
}

/// Drop an issue from the published index — the write-through for a mutation
/// that takes a card off the *open* board (a cancel, or a move into a terminal
/// `done`/`canceled` column, which the open index deliberately omits). Preserves
/// the freshness envelope like [`patch_board_index_issue`]. A no-op when no
/// index exists or the id isn't in it.
pub fn remove_board_index_issue(project: &str, id: &str) -> Result<()> {
    let Some(mut idx) = read_board_index(project) else {
        return Ok(());
    };
    let before = idx.board.len();
    idx.board.retain(|f| f.task.id != id);
    let had_number = idx.numbers.remove(id).is_some();
    if idx.board.len() == before && !had_number {
        return Ok(());
    }
    write_board_index(project, &idx)
}

/// Record a remote backend's native issue `number` for `id` in the published
/// index's id→number map, so a later single-issue `get(id)` resolves the number
/// locally and fetches that one issue in a single request instead of a label
/// search. Used by the write path the moment it learns a number: a create
/// returns the new issue's number (so an immediate `get` after `add` resolves
/// without the eventually-consistent search), and a fresh single-issue fetch
/// re-confirms it.
///
/// The freshness envelope (`fetched_at`, `stale`, budget) is left untouched —
/// only the daemon advances that. A no-op when no index has been published yet
/// (the first tick will carry the number) or when the map already maps `id` to
/// the same number.
pub fn record_board_index_number(project: &str, id: &str, number: i64) -> Result<()> {
    let Some(mut idx) = read_board_index(project) else {
        return Ok(());
    };
    if idx.numbers.get(id) == Some(&number) {
        return Ok(());
    }
    idx.numbers.insert(id.to_string(), number);
    write_board_index(project, &idx)
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

    // --- consumer read helper + write-through -------------------------------

    use shelbi_core::{GithubConnection, IssueTrackerBackend, IssueTrackerConfig};

    /// An isolated `SHELBI_HOME` so a test's board-index writes land in a temp
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
                "shelbi-board-index-home-{tag}-{}-{}",
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

    /// A `github` tracker config — a remote backend, so `read_board_with_cfg`
    /// must read the published index file rather than sweep the backend.
    fn github_cfg() -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection {
                repo: "owner/repo".into(),
            }),
            ..Default::default()
        }
    }

    /// Build an index with an explicit `fetched_at` age and `stale` flag, so the
    /// Warm/Stale mapping can be exercised without waiting real time.
    fn index_aged(board: Vec<IssueFile>, secs_ago: i64, stale: bool) -> BoardIndex {
        BoardIndex {
            board,
            numbers: std::collections::BTreeMap::new(),
            fetched_at: (Utc::now() - chrono::Duration::seconds(secs_ago)).to_rfc3339(),
            stale,
            remaining: None,
            reset: None,
        }
    }

    #[test]
    fn read_board_serves_a_fresh_remote_index_as_warm_without_touching_the_backend() {
        // The core §5 guarantee: for a remote project `read_board` reads the
        // published file — it never builds a store, so it can issue no `gh` list.
        // A sentinel id that exists on no backend proves the file was the source.
        let _iso = IsolatedHome::new("warm");
        let idx = index_aged(vec![issue("sentinel-only-in-index", "todo", 0)], 0, false);
        write_board_index("proj", &idx).unwrap();

        match read_board_with_cfg("proj", &github_cfg()).unwrap() {
            BoardState::Warm(board) => {
                assert_eq!(board.len(), 1);
                assert_eq!(board[0].task.id, "sentinel-only-in-index");
            }
            other => panic!("expected Warm from a fresh index, got {other:?}"),
        }
    }

    #[test]
    fn read_board_reports_an_aged_index_as_stale() {
        // No `stale` flag, but `fetched_at` is older than the stale threshold
        // (3 × the 30s default interval, floored at 90s): a lagging daemon.
        let _iso = IsolatedHome::new("aged");
        let idx = index_aged(vec![issue("a", "review", 0)], 600, false);
        write_board_index("proj", &idx).unwrap();

        match read_board_with_cfg("proj", &github_cfg()).unwrap() {
            BoardState::Stale(board) => assert_eq!(board.len(), 1),
            other => panic!("expected Stale from an aged index, got {other:?}"),
        }
    }

    #[test]
    fn read_board_reports_a_flagged_index_as_stale_even_when_fresh() {
        // The daemon can flag an index stale (a failed refresh carrying the
        // previous board forward); that wins regardless of `fetched_at` age.
        let _iso = IsolatedHome::new("flagged");
        let idx = index_aged(vec![issue("a", "todo", 0)], 0, true);
        write_board_index("proj", &idx).unwrap();

        assert!(matches!(
            read_board_with_cfg("proj", &github_cfg()).unwrap(),
            BoardState::Stale(_)
        ));
    }

    #[test]
    fn read_board_is_cold_when_no_index_has_been_published() {
        // A remote project on a hub whose daemon hasn't written the file yet:
        // Cold, and still no backend touch (no `gh` on PATH is required).
        let _iso = IsolatedHome::new("cold");
        assert!(matches!(
            read_board_with_cfg("proj", &github_cfg()).unwrap(),
            BoardState::Cold
        ));
    }

    #[test]
    fn patch_replaces_in_place_and_appends_and_preserves_freshness() {
        let _iso = IsolatedHome::new("patch");
        let original = index_aged(vec![issue("a", "todo", 0), issue("b", "todo", 1)], 0, false);
        write_board_index("proj", &original).unwrap();

        // Replace `a` in place (moved to review) — same id, new column.
        patch_board_index_issue("proj", &issue("a", "review", 0)).unwrap();
        // Append a brand-new card.
        patch_board_index_issue("proj", &issue("c", "todo", 2)).unwrap();

        let back = read_board_index("proj").unwrap();
        assert_eq!(back.board.len(), 3, "one replaced in place, one appended");
        let a = back.board.iter().find(|f| f.task.id == "a").unwrap();
        assert_eq!(a.task.column.as_str(), "review", "the move was written through");
        assert!(back.board.iter().any(|f| f.task.id == "c"));
        // The freshness envelope is the daemon's to advance, not the patch's.
        assert_eq!(back.fetched_at, original.fetched_at);
    }

    #[test]
    fn remove_drops_a_card_and_is_a_noop_for_an_unknown_id() {
        let _iso = IsolatedHome::new("remove");
        write_board_index(
            "proj",
            &index_aged(vec![issue("a", "todo", 0), issue("b", "todo", 1)], 0, false),
        )
        .unwrap();

        remove_board_index_issue("proj", "a").unwrap();
        let back = read_board_index("proj").unwrap();
        assert_eq!(back.board.len(), 1);
        assert_eq!(back.board[0].task.id, "b");

        // Removing an id that isn't there leaves the file unchanged.
        remove_board_index_issue("proj", "ghost").unwrap();
        assert_eq!(read_board_index("proj").unwrap().board.len(), 1);
    }

    #[test]
    fn patch_with_number_and_record_number_maintain_the_id_to_number_map() {
        let _iso = IsolatedHome::new("numbers");
        let mut idx = index_aged(vec![issue("a", "todo", 0)], 0, false);
        idx.numbers.insert("a".into(), 1);
        write_board_index("proj", &idx).unwrap();

        // A single-issue fetch write-through carries the number in.
        patch_board_index_issue_with_number("proj", &issue("b", "review", 0), Some(2)).unwrap();
        // A create records a number without touching the board.
        record_board_index_number("proj", "c", 3).unwrap();

        let back = read_board_index("proj").unwrap();
        assert_eq!(back.numbers.get("a"), Some(&1), "existing mapping preserved");
        assert_eq!(back.numbers.get("b"), Some(&2), "patched issue's number recorded");
        assert_eq!(back.numbers.get("c"), Some(&3), "create-recorded number present");

        // Removing an issue drops its number too.
        remove_board_index_issue("proj", "b").unwrap();
        assert_eq!(read_board_index("proj").unwrap().numbers.get("b"), None);
    }

    #[test]
    fn fresh_at_carries_the_numbers_and_survives_a_round_trip() {
        let idx = BoardIndex::fresh_at(
            vec![issue("a", "todo", 0)],
            vec![("a".to_string(), 7)],
            Utc::now().to_rfc3339(),
            Some(4999),
            None,
        );
        assert_eq!(idx.numbers.get("a"), Some(&7));
        // The map round-trips through the on-disk (de)serialization.
        let bytes = serde_json::to_vec(&idx).unwrap();
        let back: BoardIndex = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(back.numbers.get("a"), Some(&7));
    }

    #[test]
    fn patch_and_remove_are_noops_when_no_index_exists() {
        // Before the daemon's first tick there is no file to patch; the write
        // still landed on the backend, and the first tick will include it.
        let _iso = IsolatedHome::new("noindex");
        patch_board_index_issue("proj", &issue("a", "todo", 0)).unwrap();
        remove_board_index_issue("proj", "a").unwrap();
        assert!(read_board_index("proj").is_none());
    }
}
