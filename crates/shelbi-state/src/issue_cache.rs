//! Process-local board cache for **remote** [`IssueStore`] backends.
//!
//! A local `file_system` board is a directory read: every consumer could call
//! `list()` freely and the TUI was built on that assumption — the poller checks
//! board quiescence each cycle, the kanban re-lists on render, the review panel
//! and app state each read it again.
//!
//! A remote board is not free. Against `github` a single `list()` is a
//! paginated sweep of the whole repo: ~7s and ~6 API requests for a 589-issue
//! board. Called on a 5s poll cadence — shorter than one read takes — the TUI
//! never catches up: it blocks, snaps forward, and repaints out of phase (the
//! "flickering, frozen-then-jumping" sidebar), while burning thousands of API
//! calls an hour against a 5000/hr limit.
//!
//! This wraps a remote store so reads stop being a network round trip:
//!
//! * [`IssueStore::list`] keeps its authoritative contract: it serves the
//!   in-memory snapshot when warm and, on a genuinely cold process, reads live
//!   (synchronously). CLI one-shots go through here, so `shelbi issue list`
//!   never prints stale data.
//! * [`IssueStore::list_state`] is the render path: it also serves a
//!   **persisted** on-disk snapshot on a cold process — regardless of age —
//!   so a brand-new process (a fresh TUI pane, every command-palette popup)
//!   paints its last-known board in milliseconds instead of blocking on the
//!   ~7s sweep. It reports [`BoardState::Warm`] / [`BoardState::Stale`] /
//!   [`BoardState::Cold`] so the caller can render a loading indicator only
//!   when there is genuinely no data yet, never for an empty board.
//! * Every later in-memory `list()` returns the cached board **immediately**,
//!   never blocking the caller. Once the snapshot ages past [`TTL`] a single
//!   background refresh is kicked off and the (slightly stale) snapshot is
//!   served meanwhile. A cold process seeded from disk can't trust its own TTL,
//!   so it *always* kicks a refresh on the first read.
//! * Writes go straight through to the backend, then mark the snapshot stale
//!   and kick a refresh, so an operator's move shows up on the next tick
//!   instead of waiting out the TTL.
//! * Every successful read publishes the board to the on-disk snapshot
//!   (`<project_dir>/board-snapshot.json`) with an atomic temp-file-and-rename,
//!   so a process killed mid-write can't leave a truncated file — a torn or
//!   corrupt snapshot is treated as a cache miss and falls back to a live read.
//!
//! The in-memory cache is per **process**: each TUI pane (`__sidebar`,
//! `__tasks`) runs its own render loop *and* its own `WorkspacePoller` in the
//! same process, so both share one snapshot. The on-disk snapshot is what
//! carries a warm board *across* processes — from a running pane to the next
//! cold palette popup.
//!
//! Only remote backends are wrapped; `file_system` keeps going straight to
//! disk, where a cache would add staleness for no gain (its default
//! `list_state` reports [`BoardState::Warm`] over a plain live read).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use shelbi_core::{Column, Issue, IssueTrackerConfig, Result};

use crate::issue_store::{
    BoardState, Cursor, IssueChange, IssueComment, IssueFields, IssueStore, NewIssue, PrioMove,
    StatusMove,
};
use crate::IssueFile;

/// Basename of the per-project on-disk board snapshot, under the project's
/// state dir (`<shelbi-root>/projects/<project>/`). JSON so a torn write is a
/// clean parse failure (→ cache miss), not silently-valid garbage.
const BOARD_SNAPSHOT_FILE: &str = "board-snapshot.json";

/// How long a cached board is served before a background refresh is kicked.
///
/// Tuned against the ~7s cost of a real remote `list()`: long enough that a
/// refresh is not always in flight, short enough that out-of-band board
/// movement (an agent moving a card) surfaces promptly. Operator-driven writes
/// do not wait for it — they mark the snapshot stale immediately.
const TTL: Duration = Duration::from_secs(20);

/// One project's cached board.
struct Entry {
    /// Last successful full read. `Arc` so a reader clones a pointer under the
    /// lock and the (potentially large) clone happens outside it.
    board: Arc<Vec<IssueFile>>,
    /// When `board` was fetched. Writes push this into the past to force a
    /// refresh without discarding the data we can still serve.
    fetched_at: Instant,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Per-project "a refresh is in flight" flags, kept in their own registry
/// rather than on [`Entry`] so a genuinely cold read — one with no board entry
/// at all — can still single-flight its refresh through the same flag a later
/// [`publish`] and the poller share. `swap`-based, so N concurrent readers
/// spawn one refresh, not N.
fn refresh_flag(project: &str) -> Arc<AtomicBool> {
    static FLAGS: OnceLock<Mutex<HashMap<String, Arc<AtomicBool>>>> = OnceLock::new();
    let flags = FLAGS.get_or_init(|| Mutex::new(HashMap::new()));
    let Ok(mut guard) = flags.lock() else {
        // Poisoned: hand back a throwaway flag. Worst case is a duplicate
        // refresh, never a panic or a wedged "refreshing" state.
        return Arc::new(AtomicBool::new(false));
    };
    Arc::clone(guard.entry(project.to_string()).or_default())
}

/// The cached board plus whether it is due a refresh. `None` when cold.
///
/// A poisoned lock is treated as a cache miss rather than a panic: the caller
/// falls back to a live read, which is correct, just slower.
fn snapshot(project: &str) -> Option<(Arc<Vec<IssueFile>>, bool)> {
    let guard = cache().lock().ok()?;
    let entry = guard.get(project)?;
    Some((Arc::clone(&entry.board), entry.fetched_at.elapsed() >= TTL))
}

/// Publish a freshly read board to the in-memory cache and, when a snapshot
/// path is known, to the on-disk snapshot. The disk write is atomic and
/// best-effort: a failure just means the next cold process reads live.
fn publish(project: &str, board: Vec<IssueFile>, snapshot: Option<&Path>) {
    if let Some(path) = snapshot {
        write_snapshot_to_disk(path, &board);
    }
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    guard.insert(
        project.to_string(),
        Entry {
            board: Arc::new(board),
            fetched_at: Instant::now(),
        },
    );
}

/// Seed the in-memory cache from the on-disk snapshot on a cold process,
/// marked **stale** so the very next read serves it *and* kicks a refresh — a
/// cold process cannot trust the snapshot's age. Does not touch disk (the data
/// came from there). No-op if a fresher in-memory entry already exists, so a
/// completed background refresh is never clobbered by a stale disk seed.
fn seed_stale_from_disk(project: &str, board: Vec<IssueFile>) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    if guard.contains_key(project) {
        return;
    }
    guard.insert(
        project.to_string(),
        Entry {
            board: Arc::new(board),
            fetched_at: Instant::now()
                .checked_sub(TTL)
                .unwrap_or_else(Instant::now),
        },
    );
}

/// Age the snapshot out without dropping it, so the next read serves the last
/// known board *and* triggers a refresh. Dropping it instead would make the
/// next read synchronous — a multi-second freeze right after a user action.
fn mark_stale(project: &str) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    if let Some(entry) = guard.get_mut(project) {
        entry.fetched_at = Instant::now()
            .checked_sub(TTL)
            .unwrap_or_else(Instant::now);
    }
}

/// The on-disk snapshot path for `project`, or `None` when the shelbi root
/// can't be resolved / the name is invalid. Lives under the project's **state**
/// dir (`<shelbi-root>/projects/<project>/`), not `.claude/`.
fn snapshot_path(project: &str) -> Option<PathBuf> {
    crate::project_dir(project)
        .ok()
        .map(|dir| dir.join(BOARD_SNAPSHOT_FILE))
}

/// Read and parse the on-disk snapshot. A missing file, an unreadable file, or
/// a torn/corrupt one (truncated mid-write, invalid JSON) all resolve to
/// `None` — a cache miss the caller recovers from with a live read, never an
/// error or rendered junk.
fn read_snapshot_from_disk(path: &Path) -> Option<Vec<IssueFile>> {
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Persist the board to `path` atomically (temp file + rename via
/// [`crate::atomic_write`]), so a process killed mid-write leaves either the
/// old snapshot or the new one, never a truncated file. Best-effort: a
/// serialize/IO failure is swallowed (the next cold read just goes live).
fn write_snapshot_to_disk(path: &Path, board: &[IssueFile]) {
    if let Ok(bytes) = serde_json::to_vec(board) {
        let _ = crate::atomic_write(path, &bytes);
    }
}

/// A remote [`IssueStore`] with non-blocking cached reads.
///
/// Reads are served from the process-local snapshot; writes pass through and
/// invalidate. See the module docs for why only remote backends get this.
pub(crate) struct CachedIssueStore {
    inner: Box<dyn IssueStore>,
    project: String,
    cfg: IssueTrackerConfig,
    /// Where the persisted snapshot lives, resolved once at construction.
    /// `None` when the shelbi root can't be resolved — the store then behaves
    /// exactly like the old in-memory-only cache (no disk warm, no disk write).
    snapshot: Option<PathBuf>,
}

impl CachedIssueStore {
    pub(crate) fn new(inner: Box<dyn IssueStore>, project: &str, cfg: &IssueTrackerConfig) -> Self {
        Self {
            inner,
            project: project.to_string(),
            cfg: cfg.clone(),
            snapshot: snapshot_path(project),
        }
    }

    /// Refresh the snapshot on a background thread, at most one at a time.
    ///
    /// The thread builds its **own** store from the project's config rather
    /// than sharing `self.inner`: the trait carries no `Send + Sync` bound, and
    /// requiring one would ripple through every backend for no benefit. Config
    /// and project name are cheap to clone and are all a store needs.
    ///
    /// Best-effort by design — a failed refresh leaves the previous snapshot in
    /// place and the next read tries again. A board that briefly stops
    /// advancing beats one that renders an error or empties out because GitHub
    /// was slow for a moment.
    fn kick_refresh(&self, flag: Arc<AtomicBool>) {
        if flag.swap(true, Ordering::AcqRel) {
            return; // already refreshing
        }
        let project = self.project.clone();
        let cfg = self.cfg.clone();
        let snapshot = self.snapshot.clone();
        let thread_flag = Arc::clone(&flag);
        let spawned = std::thread::Builder::new()
            .name("shelbi-board-refresh".into())
            .spawn(move || {
                if let Ok(store) = crate::issue_store::build_store(&project, &cfg) {
                    match store.list() {
                        Ok(board) => publish(&project, board, snapshot.as_deref()),
                        Err(e) => {
                            tracing::debug!(project = %project, error = %e, "board refresh failed")
                        }
                    }
                }
                thread_flag.store(false, Ordering::Release);
            });
        // Spawn can fail under thread exhaustion; clear the flag so we are not
        // wedged "refreshing" forever with nothing running.
        if spawned.is_err() {
            flag.store(false, Ordering::Release);
        }
    }

    /// Pass-through for a write: invalidate, then kick a refresh so the change
    /// lands in the snapshot (in memory *and* on disk) without waiting out the
    /// TTL.
    fn invalidate(&self) {
        mark_stale(&self.project);
        self.kick_refresh(refresh_flag(&self.project));
    }
}

impl IssueStore for CachedIssueStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        if let Some((board, stale)) = snapshot(&self.project) {
            if stale {
                self.kick_refresh(refresh_flag(&self.project));
            }
            return Ok((*board).clone());
        }
        // Cold in memory: `list` is the authoritative path (CLI one-shots go
        // through it), so it reads live rather than serving the possibly-stale
        // disk snapshot — no command ever prints stale data. The live read is
        // published to memory *and* disk, warming the next cold process's
        // `list_state`.
        let board = self.inner.list()?;
        publish(&self.project, board.clone(), self.snapshot.as_deref());
        Ok(board)
    }

    fn list_state(&self) -> Result<BoardState> {
        // Warm/stale from the in-memory snapshot — the same fast path `list`
        // takes once this process has read once.
        if let Some((board, stale)) = snapshot(&self.project) {
            if stale {
                self.kick_refresh(refresh_flag(&self.project));
                return Ok(BoardState::Stale((*board).clone()));
            }
            return Ok(BoardState::Warm((*board).clone()));
        }
        // Cold in memory: serve the persisted on-disk snapshot immediately,
        // regardless of age, so this fresh process paints its last-known board
        // without the multi-second sweep. Seed memory (as stale) and always
        // kick a refresh — a cold process can't trust the snapshot's TTL.
        if let Some(path) = &self.snapshot {
            if let Some(board) = read_snapshot_from_disk(path) {
                seed_stale_from_disk(&self.project, board.clone());
                self.kick_refresh(refresh_flag(&self.project));
                return Ok(BoardState::Stale(board));
            }
        }
        // Genuinely cold: no memory, no snapshot on disk. Report it so the
        // caller renders a loading indicator (not an empty board) and kick the
        // background fetch that will fill the snapshot.
        self.kick_refresh(refresh_flag(&self.project));
        Ok(BoardState::Cold)
    }

    /// Filtered from the cached board rather than issuing its own sweep.
    /// `list()` is already canonical column-then-priority order, and filtering
    /// preserves relative order, so the result matches a live call.
    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        Ok(self
            .list()?
            .into_iter()
            .filter(|f| &f.task.column == status)
            .collect())
    }

    /// Deliberately live. A single-issue read is one cheap request, and `get`
    /// backs correctness-critical paths (dispatch, transitions) where a stale
    /// answer is worse than a slow one.
    fn get(&self, id: &str) -> Result<Option<IssueFile>> {
        self.inner.get(id)
    }

    fn add(&self, spec: NewIssue) -> Result<Issue> {
        let out = self.inner.add(spec)?;
        self.invalidate();
        Ok(out)
    }

    fn move_status(&self, id: &str, to: &Column, reason: &str) -> Result<Option<StatusMove>> {
        let out = self.inner.move_status(id, to, reason)?;
        self.invalidate();
        Ok(out)
    }

    fn set_priority(&self, id: &str, pos: PrioMove) -> Result<()> {
        self.inner.set_priority(id, pos)?;
        self.invalidate();
        Ok(())
    }

    fn set_fields(&self, id: &str, fields: IssueFields) -> Result<()> {
        self.inner.set_fields(id, fields)?;
        self.invalidate();
        Ok(())
    }

    fn cancel(&self, id: &str, reason: &str) -> Result<Option<StatusMove>> {
        let out = self.inner.cancel(id, reason)?;
        self.invalidate();
        Ok(out)
    }

    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        reason: &str,
    ) -> Result<Option<StatusMove>> {
        let out = self.inner.move_status_and_unassign(id, to, reason)?;
        self.invalidate();
        Ok(out)
    }

    fn delete(&self, id: &str) -> Result<()> {
        self.inner.delete(id)?;
        self.invalidate();
        Ok(())
    }

    fn renumber(&self, status: &Column) -> Result<()> {
        self.inner.renumber(status)?;
        self.invalidate();
        Ok(())
    }

    fn park_review(&self, id: &str) -> Result<Option<String>> {
        let out = self.inner.park_review(id)?;
        self.invalidate();
        Ok(out)
    }

    fn clear_parked(&self, id: &str) -> Result<()> {
        self.inner.clear_parked(id)?;
        self.invalidate();
        Ok(())
    }

    fn reject_review(
        &self,
        id: &str,
        ready: &Column,
        reason: &str,
        date: &str,
    ) -> Result<Option<StatusMove>> {
        let out = self.inner.reject_review(id, ready, reason, date)?;
        self.invalidate();
        Ok(out)
    }

    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        self.inner.poll_changes(since)
    }

    fn list_comments(&self, id: &str) -> Result<Vec<IssueComment>> {
        self.inner.list_comments(id)
    }

    fn add_comment(&self, id: &str, body: &str) -> Result<IssueComment> {
        let out = self.inner.add_comment(id, body)?;
        self.invalidate();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Counts `list()` calls so a test can prove a read was served from the
    /// snapshot rather than the backend. Every other method is an inert stub —
    /// the cache's job is deciding *whether* to call through, not what the
    /// backend returns.
    struct CountingStore {
        board: Vec<IssueFile>,
        lists: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
    }

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

    /// `file_system` so the cache's background refresh builds a harmless local
    /// store for a project that does not exist: it fails, logs, and leaves the
    /// snapshot untouched. Keeps the tests offline and deterministic.
    fn offline_cfg() -> IssueTrackerConfig {
        IssueTrackerConfig::default()
    }

    impl IssueStore for CountingStore {
        fn list(&self) -> Result<Vec<IssueFile>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            Ok(self.board.clone())
        }
        fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
            Ok(self
                .board
                .iter()
                .filter(|f| &f.task.column == status)
                .cloned()
                .collect())
        }
        fn get(&self, _id: &str) -> Result<Option<IssueFile>> {
            Ok(None)
        }
        fn add(&self, _spec: NewIssue) -> Result<Issue> {
            unreachable!("not exercised")
        }
        fn move_status(&self, _i: &str, _t: &Column, _r: &str) -> Result<Option<StatusMove>> {
            Ok(None)
        }
        fn set_priority(&self, _i: &str, _p: PrioMove) -> Result<()> {
            self.writes.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn set_fields(&self, _i: &str, _f: IssueFields) -> Result<()> {
            Ok(())
        }
        fn cancel(&self, _i: &str, _r: &str) -> Result<Option<StatusMove>> {
            Ok(None)
        }
        fn move_status_and_unassign(
            &self,
            _i: &str,
            _t: &Column,
            _r: &str,
        ) -> Result<Option<StatusMove>> {
            Ok(None)
        }
        fn delete(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        fn renumber(&self, _s: &Column) -> Result<()> {
            Ok(())
        }
        fn park_review(&self, _id: &str) -> Result<Option<String>> {
            Ok(None)
        }
        fn clear_parked(&self, _id: &str) -> Result<()> {
            Ok(())
        }
        fn reject_review(
            &self,
            _i: &str,
            _r: &Column,
            _s: &str,
            _d: &str,
        ) -> Result<Option<StatusMove>> {
            Ok(None)
        }
        fn poll_changes(&self, _s: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
            Ok((Vec::new(), Cursor::start()))
        }
        fn list_comments(&self, _id: &str) -> Result<Vec<IssueComment>> {
            Ok(Vec::new())
        }
        fn add_comment(&self, _id: &str, _b: &str) -> Result<IssueComment> {
            unreachable!("not exercised")
        }
    }

    /// A cache with **no** on-disk snapshot, so these in-memory-behavior tests
    /// never touch the real shelbi root (`snapshot_path` would otherwise
    /// resolve under `~/.shelbi`). Disk behavior is covered separately below
    /// with an explicit temp path.
    fn cached(project: &str, board: Vec<IssueFile>) -> (CachedIssueStore, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        cached_with_snapshot(project, board, None)
    }

    fn cached_with_snapshot(
        project: &str,
        board: Vec<IssueFile>,
        snapshot: Option<PathBuf>,
    ) -> (CachedIssueStore, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let lists = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let inner = CountingStore {
            board,
            lists: Arc::clone(&lists),
            writes: Arc::clone(&writes),
        };
        (
            CachedIssueStore {
                inner: Box::new(inner),
                project: project.to_string(),
                cfg: offline_cfg(),
                snapshot,
            },
            lists,
            writes,
        )
    }

    /// A unique temp path for a snapshot file, isolated from the real root and
    /// from other tests. Deterministic-free: keyed on pid + project name.
    fn temp_snapshot_path(project: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "shelbi-board-snap-{}-{}.json",
            std::process::id(),
            project
        ))
    }

    // The cache is process-global, so each test keys off a distinct project
    // name rather than sharing (and having to serialize on) one entry.

    #[test]
    fn first_read_hits_the_backend_and_later_reads_are_served_from_the_snapshot() {
        let (store, lists, _) = cached("cache-t1", vec![issue("a", "todo")]);
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(lists.load(Ordering::SeqCst), 1, "cold read must fetch");
        for _ in 0..5 {
            assert_eq!(store.list().unwrap().len(), 1);
        }
        assert_eq!(
            lists.load(Ordering::SeqCst),
            1,
            "warm reads must not touch the backend — this is the whole point"
        );
    }

    #[test]
    fn list_in_status_filters_the_snapshot_instead_of_re_reading() {
        let (store, lists, _) = cached(
            "cache-t2",
            vec![issue("a", "todo"), issue("b", "done"), issue("c", "todo")],
        );
        let todo = store.list_in_status(&Column::todo()).unwrap();
        assert_eq!(todo.len(), 2);
        assert!(todo.iter().all(|f| f.task.column.as_str() == "todo"));
        assert_eq!(
            lists.load(Ordering::SeqCst),
            1,
            "a status filter must not cost a second sweep"
        );
    }

    #[test]
    fn a_write_passes_through_and_keeps_serving_the_snapshot_without_blocking() {
        let (store, lists, writes) = cached("cache-t3", vec![issue("a", "todo")]);
        store.list().unwrap();
        store.set_priority("a", PrioMove::Up).unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 1, "write must reach backend");
        // Invalidation ages the snapshot but must not discard it: the next read
        // still answers from cache (no multi-second stall after a user action)
        // and the refresh happens on a background thread.
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(
            lists.load(Ordering::SeqCst),
            1,
            "post-write read must not block on a synchronous re-list"
        );
    }

    // ----- disk-backed cold-start layer -------------------------------------

    #[test]
    fn list_state_warm_in_memory_after_a_read() {
        let (store, _lists, _) = cached("cache-ds1", vec![issue("a", "todo")]);
        // Seed memory with a fresh read.
        store.list().unwrap();
        match store.list_state().unwrap() {
            BoardState::Warm(v) => assert_eq!(v.len(), 1),
            other => panic!("expected Warm after a fresh read, got {other:?}"),
        }
    }

    #[test]
    fn list_state_is_cold_with_no_memory_and_no_disk_and_does_not_block() {
        // A genuinely first-run process: nothing in memory, no snapshot on
        // disk (snapshot: None). It must report Cold *without* a synchronous
        // backend sweep — the whole point of not blocking first paint.
        let (store, lists, _) = cached("cache-ds2", vec![issue("a", "todo")]);
        assert!(store.list_state().unwrap().is_cold());
        assert_eq!(
            lists.load(Ordering::SeqCst),
            0,
            "cold list_state must not synchronously hit the backend"
        );
    }

    #[test]
    fn list_state_serves_the_disk_snapshot_on_a_cold_process() {
        // Write a snapshot as if a prior process had published it, then open a
        // brand-new cold cache (empty memory) pointed at it: list_state must
        // serve that board as Stale without ever calling the backend — this is
        // the cross-process warm start the palette relies on.
        let path = temp_snapshot_path("cache-ds3");
        let _ = std::fs::remove_file(&path);
        write_snapshot_to_disk(&path, &[issue("a", "todo"), issue("b", "done")]);

        let (store, lists, _) = cached_with_snapshot("cache-ds3", Vec::new(), Some(path.clone()));
        match store.list_state().unwrap() {
            BoardState::Stale(v) => assert_eq!(v.len(), 2, "served the persisted board"),
            other => panic!("expected Stale from the disk snapshot, got {other:?}"),
        }
        assert_eq!(
            lists.load(Ordering::SeqCst),
            0,
            "a disk-warm cold start must not block on a live sweep"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_corrupt_snapshot_is_a_cache_miss_not_an_error() {
        // A file truncated mid-write (invalid JSON) must read as Cold — a
        // recoverable cache miss — not an error or rendered junk.
        let path = temp_snapshot_path("cache-ds4");
        std::fs::write(&path, b"{\"task\": {\"id\": \"a\", tru").unwrap();
        assert!(
            read_snapshot_from_disk(&path).is_none(),
            "torn JSON must not parse"
        );

        let (store, lists, _) = cached_with_snapshot("cache-ds4", Vec::new(), Some(path.clone()));
        assert!(
            store.list_state().unwrap().is_cold(),
            "a corrupt snapshot falls back to Cold, never an error"
        );
        assert_eq!(lists.load(Ordering::SeqCst), 0);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn publish_round_trips_through_the_on_disk_snapshot() {
        let path = temp_snapshot_path("cache-ds5");
        let _ = std::fs::remove_file(&path);
        let board = vec![issue("a", "todo"), issue("b", "review")];
        write_snapshot_to_disk(&path, &board);
        let read = read_snapshot_from_disk(&path).expect("snapshot round-trips");
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].task.id, "a");
        assert_eq!(read[1].task.column.as_str(), "review");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_cold_list_warms_the_disk_snapshot_for_the_next_process() {
        // `list` (the authoritative path) publishes its live read to disk, so a
        // later cold `list_state` in another process starts warm.
        let path = temp_snapshot_path("cache-ds6");
        let _ = std::fs::remove_file(&path);
        let (store, lists, _) =
            cached_with_snapshot("cache-ds6", vec![issue("a", "todo")], Some(path.clone()));
        store.list().unwrap();
        assert_eq!(lists.load(Ordering::SeqCst), 1, "cold list reads live");
        let read = read_snapshot_from_disk(&path).expect("list wrote a snapshot");
        assert_eq!(read.len(), 1, "the live read was persisted to disk");
        let _ = std::fs::remove_file(&path);
    }
}
