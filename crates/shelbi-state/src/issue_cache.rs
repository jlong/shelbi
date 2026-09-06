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
//! * The **first** `list()` in a process fetches synchronously — there is no
//!   data to serve yet, and returning an empty board would render a lie.
//! * Every later `list()` returns the cached board **immediately**, never
//!   blocking the caller. Once the snapshot ages past [`TTL`] a single
//!   background refresh is kicked off and the (slightly stale) snapshot is
//!   served meanwhile.
//! * Writes go straight through to the backend, then mark the snapshot stale
//!   and kick a refresh, so an operator's move shows up on the next tick
//!   instead of waiting out the TTL.
//!
//! The cache is per **process**, which is the useful scope: each TUI pane
//! (`__sidebar`, `__tasks`) runs its own render loop *and* its own
//! `WorkspacePoller` in the same process, so both share one snapshot. A
//! one-shot CLI invocation starts cold and therefore always reads live — no
//! command ever reports stale data.
//!
//! Only remote backends are wrapped; `file_system` keeps going straight to
//! disk, where a cache would add staleness for no gain.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use shelbi_core::{Column, Issue, IssueTrackerConfig, Result};

use crate::issue_store::{
    Cursor, IssueChange, IssueComment, IssueFields, IssueStore, NewIssue, PrioMove, StatusMove,
};
use crate::IssueFile;

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
    /// Set while a background refresh is in flight, so N stale reads spawn one
    /// refresh rather than N.
    refreshing: Arc<AtomicBool>,
}

fn cache() -> &'static Mutex<HashMap<String, Entry>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Entry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The cached board plus whether it is due a refresh. `None` when cold.
///
/// A poisoned lock is treated as a cache miss rather than a panic: the caller
/// falls back to a live read, which is correct, just slower.
fn snapshot(project: &str) -> Option<(Arc<Vec<IssueFile>>, bool, Arc<AtomicBool>)> {
    let guard = cache().lock().ok()?;
    let entry = guard.get(project)?;
    Some((
        Arc::clone(&entry.board),
        entry.fetched_at.elapsed() >= TTL,
        Arc::clone(&entry.refreshing),
    ))
}

/// Publish a freshly read board, preserving the in-flight flag so a concurrent
/// reader's view of "a refresh is running" stays accurate.
fn publish(project: &str, board: Vec<IssueFile>) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    let refreshing = guard
        .get(project)
        .map(|e| Arc::clone(&e.refreshing))
        .unwrap_or_default();
    guard.insert(
        project.to_string(),
        Entry {
            board: Arc::new(board),
            fetched_at: Instant::now(),
            refreshing,
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

/// A remote [`IssueStore`] with non-blocking cached reads.
///
/// Reads are served from the process-local snapshot; writes pass through and
/// invalidate. See the module docs for why only remote backends get this.
pub(crate) struct CachedIssueStore {
    inner: Box<dyn IssueStore>,
    project: String,
    cfg: IssueTrackerConfig,
}

impl CachedIssueStore {
    pub(crate) fn new(inner: Box<dyn IssueStore>, project: &str, cfg: &IssueTrackerConfig) -> Self {
        Self {
            inner,
            project: project.to_string(),
            cfg: cfg.clone(),
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
        let thread_flag = Arc::clone(&flag);
        let spawned = std::thread::Builder::new()
            .name("shelbi-board-refresh".into())
            .spawn(move || {
                if let Ok(store) = crate::issue_store::build_store(&project, &cfg) {
                    match store.list() {
                        Ok(board) => publish(&project, board),
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
    /// lands in the snapshot without waiting out the TTL.
    fn invalidate(&self) {
        mark_stale(&self.project);
        if let Some((_, _, flag)) = snapshot(&self.project) {
            self.kick_refresh(flag);
        }
    }
}

impl IssueStore for CachedIssueStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        if let Some((board, stale, flag)) = snapshot(&self.project) {
            if stale {
                self.kick_refresh(flag);
            }
            return Ok((*board).clone());
        }
        // Cold: nothing to serve, so this one read is synchronous.
        let board = self.inner.list()?;
        publish(&self.project, board.clone());
        Ok(board)
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

    fn cached(project: &str, board: Vec<IssueFile>) -> (CachedIssueStore, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let lists = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let inner = CountingStore {
            board,
            lists: Arc::clone(&lists),
            writes: Arc::clone(&writes),
        };
        (
            CachedIssueStore::new(Box::new(inner), project, &offline_cfg()),
            lists,
            writes,
        )
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
}
