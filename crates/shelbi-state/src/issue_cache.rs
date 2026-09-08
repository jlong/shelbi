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

use shelbi_core::{Column, Issue, IssueTrackerConfig, Result, StatusCategory};

use crate::issue_store::{
    BoardState, Cursor, IssueChange, IssueComment, IssueFields, IssueStore, NewIssue, PrioMove,
    StatusMove,
};
use crate::IssueFile;

/// Basename of the per-project on-disk board snapshot, under the project's
/// state dir (`<shelbi-root>/projects/<project>/`). JSON so a torn write is a
/// clean parse failure (→ cache miss), not silently-valid garbage.
const BOARD_SNAPSHOT_FILE: &str = "board-snapshot.json";

/// Basename of the per-project on-disk snapshot for the **closed** (terminal
/// `done`/`canceled`) board, a sibling of [`BOARD_SNAPSHOT_FILE`]. Kept
/// separate so a resuming process serves the terminal columns from disk
/// without a blocking `state=closed` sweep, and so the frequently-rewritten
/// open snapshot the sidebar / `shelbi status` read stays exactly the open
/// board (never bloated with history).
const CLOSED_SNAPSHOT_FILE: &str = "board-snapshot-closed.json";

/// How long a cached board is served before a background refresh is kicked.
///
/// Tuned against the ~7s cost of a real remote `list()`: long enough that a
/// refresh is not always in flight, short enough that out-of-band board
/// movement (an agent moving a card) surfaces promptly. Operator-driven writes
/// do not wait for it — they mark the snapshot stale immediately.
///
/// Exposed as [`BOARD_CACHE_TTL`] so out-of-crate consumers that make decisions
/// off a cached board read (the poller's orphaned-pane reaper) can size their
/// own staleness-tolerance windows against the same bound instead of
/// hardcoding a duplicate of this number.
const TTL: Duration = Duration::from_secs(20);

/// Public alias of the process-local board cache's serve-stale window. See
/// [`TTL`]. A cached `list()` may lag the backend by up to this long (plus one
/// background refresh round trip) before the snapshot catches up.
pub const BOARD_CACHE_TTL: Duration = TTL;

/// How long the **closed** (terminal `done`/`canceled`) board is served before
/// a background refresh is kicked. Far longer than [`TTL`]: the terminal
/// history barely changes, only the Kanban's terminal columns read it, and a
/// `state=closed` sweep is the expensive read on a large board. A terminal
/// status move *through this cache* invalidates it immediately, so an
/// operator's own completion still shows promptly; an out-of-band close lands
/// within this window.
const CLOSED_TTL: Duration = Duration::from_secs(120);

/// Which slice of a project's board a cache operation addresses. The two are
/// cached wholly independently — distinct in-memory entry, on-disk snapshot,
/// staleness window and single-flight refresh flag — because the open board
/// moves constantly (short TTL, disk-persisted, the render/poll hot path)
/// while the closed history is nearly static (long TTL, only the Kanban's
/// terminal columns).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// Non-terminal cards — `state=open` on the github backend.
    Open,
    /// Terminal `done`/`canceled` history — `state=closed` on the github backend.
    Closed,
}

/// The read a background/cold refresh runs for `scope` against a freshly built
/// store. Split out (rather than inlined in the refresh thread) so it is unit
/// testable with a recording `gh` runner: `Open` must send `state=open`,
/// `Closed` must send `state=closed`, neither the full `state=all` sweep.
fn scoped_read(store: &dyn IssueStore, scope: Scope) -> Result<Vec<IssueFile>> {
    match scope {
        Scope::Open => store.list_open(),
        Scope::Closed => store.list_closed(),
    }
}

/// True for the terminal categories a closed GitHub issue maps to
/// (`done`/`canceled`). A `list_in_status` for one of these is served from the
/// closed cache; every other status from the open cache.
fn is_terminal(status: &Column) -> bool {
    matches!(
        status.category(),
        StatusCategory::Done | StatusCategory::Archived
    )
}

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

/// The cached board for `key` plus whether it is older than `ttl` (due a
/// refresh). `None` when cold. `key` is the scoped cache key (the bare project
/// name for the open board, [`closed_key`] for the closed history), so the two
/// scopes never collide.
///
/// A poisoned lock is treated as a cache miss rather than a panic: the caller
/// falls back to a live read, which is correct, just slower.
fn snapshot(key: &str, ttl: Duration) -> Option<(Arc<Vec<IssueFile>>, bool)> {
    let guard = cache().lock().ok()?;
    let entry = guard.get(key)?;
    Some((Arc::clone(&entry.board), entry.fetched_at.elapsed() >= ttl))
}

/// The in-memory / refresh-flag / disk key for a project's **closed**-history
/// cache — the open board keys off the bare project name, so this must not
/// collide with it. The `\u{1}` separator can't appear in a project name.
fn closed_key(project: &str) -> String {
    format!("{project}\u{1}closed")
}

/// Publish a freshly read board to the in-memory cache and, when a snapshot
/// path is known, to the on-disk snapshot. The disk write is atomic and
/// best-effort: a failure just means the next cold process reads live.
fn publish(key: &str, board: Vec<IssueFile>, snapshot: Option<&Path>) {
    if let Some(path) = snapshot {
        write_snapshot_to_disk(path, &board);
    }
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    guard.insert(
        key.to_string(),
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
fn seed_stale_from_disk(key: &str, board: Vec<IssueFile>, ttl: Duration) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    if guard.contains_key(key) {
        return;
    }
    guard.insert(
        key.to_string(),
        Entry {
            board: Arc::new(board),
            fetched_at: Instant::now()
                .checked_sub(ttl)
                .unwrap_or_else(Instant::now),
        },
    );
}

/// Age the snapshot out without dropping it, so the next read serves the last
/// known board *and* triggers a refresh. Dropping it instead would make the
/// next read synchronous — a multi-second freeze right after a user action.
fn mark_stale(key: &str, ttl: Duration) {
    let Ok(mut guard) = cache().lock() else {
        return;
    };
    if let Some(entry) = guard.get_mut(key) {
        entry.fetched_at = Instant::now()
            .checked_sub(ttl)
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

/// The on-disk snapshot path for `project`'s **closed** history, a sibling of
/// [`snapshot_path`]'s open board. `None` on the same conditions.
fn closed_snapshot_path(project: &str) -> Option<PathBuf> {
    crate::project_dir(project)
        .ok()
        .map(|dir| dir.join(CLOSED_SNAPSHOT_FILE))
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

/// Test-support: seed the on-disk board snapshot for `project` exactly as a
/// successful read would, so a *cold* process serves it (reported as
/// [`BoardState::Stale`]) from [`IssueStore::list_state`] without any live read.
///
/// Lets cross-crate tests exercise the snapshot-served (degraded) render path —
/// a remote board whose live refresh is failing — deterministically, without
/// standing up a real backend. Keeps the snapshot's on-disk location and format
/// encapsulated here rather than hardcoded in each test.
#[cfg(any(test, feature = "test-support"))]
pub fn seed_board_snapshot_for_test(project: &str, board: &[IssueFile]) {
    if let Some(path) = snapshot_path(project) {
        write_snapshot_to_disk(&path, board);
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
    /// Where the persisted open-board snapshot lives, resolved once at
    /// construction. `None` when the shelbi root can't be resolved — the store
    /// then behaves exactly like the old in-memory-only cache (no disk warm, no
    /// disk write).
    snapshot: Option<PathBuf>,
    /// Where the persisted closed-history snapshot lives (see
    /// [`closed_snapshot_path`]). `None` on the same conditions as `snapshot`.
    closed_snapshot: Option<PathBuf>,
}

impl CachedIssueStore {
    pub(crate) fn new(inner: Box<dyn IssueStore>, project: &str, cfg: &IssueTrackerConfig) -> Self {
        Self {
            inner,
            project: project.to_string(),
            cfg: cfg.clone(),
            snapshot: snapshot_path(project),
            closed_snapshot: closed_snapshot_path(project),
        }
    }

    /// The scoped cache key, staleness window and on-disk snapshot path for
    /// `scope` — the one place the open/closed asymmetry is spelled out.
    fn scope_parts(&self, scope: Scope) -> (String, Duration, Option<PathBuf>) {
        match scope {
            Scope::Open => (self.project.clone(), TTL, self.snapshot.clone()),
            Scope::Closed => (closed_key(&self.project), CLOSED_TTL, self.closed_snapshot.clone()),
        }
    }

    /// Serve a scope's board as a plain `Vec` (the `list_open` / `list_closed`
    /// contract). Non-blocking whenever it can be: the in-memory snapshot when
    /// present (kicking a background refresh once it is stale), else the
    /// on-disk snapshot on a warm resume (seeded stale, refreshed in the
    /// background — no blocking sweep). Only a genuinely cold process with no
    /// snapshot at all reads live and publishes, so a CLI one-shot never prints
    /// an empty board.
    fn serve(&self, scope: Scope) -> Result<Vec<IssueFile>> {
        let (key, ttl, disk) = self.scope_parts(scope);
        if let Some((board, stale)) = snapshot(&key, ttl) {
            if stale {
                self.kick_refresh(scope);
            }
            return Ok((*board).clone());
        }
        if let Some(path) = &disk {
            if let Some(board) = read_snapshot_from_disk(path) {
                seed_stale_from_disk(&key, board.clone(), ttl);
                self.kick_refresh(scope);
                return Ok(board);
            }
        }
        // Genuinely cold: read live so a one-shot is correct, and publish to
        // warm the next process (memory + disk).
        let board = scoped_read(self.inner.as_ref(), scope)?;
        publish(&key, board.clone(), disk.as_deref());
        Ok(board)
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
    fn kick_refresh(&self, scope: Scope) {
        let (key, _ttl, disk) = self.scope_parts(scope);
        let flag = refresh_flag(&key);
        if flag.swap(true, Ordering::AcqRel) {
            return; // already refreshing this scope
        }
        let project = self.project.clone();
        let cfg = self.cfg.clone();
        let thread_flag = Arc::clone(&flag);
        let spawned = std::thread::Builder::new()
            .name("shelbi-board-refresh".into())
            .spawn(move || {
                if let Ok(store) = crate::issue_store::build_store(&project, &cfg) {
                    // Scope-scoped read: `state=open` for the open board,
                    // `state=closed` for the terminal history — never the full
                    // `state=all` sweep this cache exists to eliminate.
                    match scoped_read(store.as_ref(), scope) {
                        Ok(board) => publish(&key, board, disk.as_deref()),
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

    /// Pass-through for a write that touches only non-terminal state:
    /// invalidate the **open** board, then kick its refresh so the change lands
    /// in the snapshot (in memory *and* on disk) without waiting out the TTL.
    fn invalidate(&self) {
        mark_stale(&self.project, TTL);
        self.kick_refresh(Scope::Open);
    }

    /// Pass-through for a write that moves a card **to or from a terminal
    /// column** (a completion / cancellation): invalidate both the open board
    /// *and* the closed history so the card leaves in-progress and appears in
    /// done/canceled promptly, rather than the closed side lagging out its long
    /// TTL after an operator's own action.
    fn invalidate_with_closed(&self) {
        self.invalidate();
        mark_stale(&closed_key(&self.project), CLOSED_TTL);
        self.kick_refresh(Scope::Closed);
    }
}

impl IssueStore for CachedIssueStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        // The authoritative *full* board (`state=all` on the github backend),
        // read live and uncached. Only the migrate / reconcile / dependency
        // paths call this now; every render and poll path takes `list_open`
        // (served from the process-local open snapshot) or `list_in_status`, so
        // this full sweep never lands on the hot path. Serving it from a cache
        // would either lag those callers or force the open snapshot to carry
        // the terminal history it deliberately omits, so it stays live.
        self.inner.list()
    }

    fn list_open(&self) -> Result<Vec<IssueFile>> {
        // The board the render/poll paths (pollers, sidebar, `zen scan`, the
        // drain, unfiltered `issue list`) actually need — served from the
        // process-local open snapshot, refreshed via `inner.list_open()`
        // (`state=open`), never the full sweep.
        self.serve(Scope::Open)
    }

    fn list_closed(&self) -> Result<Vec<IssueFile>> {
        // The terminal history, served from the separate long-TTL closed
        // snapshot (refreshed via `inner.list_closed()` = `state=closed`).
        self.serve(Scope::Closed)
    }

    fn list_state(&self) -> Result<BoardState> {
        // Warm/stale from the in-memory open snapshot — the same fast path
        // `list_open` takes once this process has read once. Open-only by
        // design: the sidebar and `shelbi status` filter it to the non-terminal
        // columns they show, and the Kanban merges the terminal columns from
        // its own `list_in_status(done|canceled)` reads.
        if let Some((board, stale)) = snapshot(&self.project, TTL) {
            if stale {
                self.kick_refresh(Scope::Open);
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
                seed_stale_from_disk(&self.project, board.clone(), TTL);
                self.kick_refresh(Scope::Open);
                return Ok(BoardState::Stale(board));
            }
        }
        // Genuinely cold: no memory, no snapshot on disk. Report it so the
        // caller renders a loading indicator (not an empty board) and kick the
        // background fetch that will fill the snapshot.
        self.kick_refresh(Scope::Open);
        Ok(BoardState::Cold)
    }

    /// Filtered from the scope-appropriate cached board rather than issuing its
    /// own sweep: the terminal `done`/`canceled` lanes come from the closed
    /// cache (`state=closed`), every other status from the open cache
    /// (`state=open`). Both are canonical column-then-priority order and
    /// filtering preserves relative order, so the result matches a live call.
    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        let scope = if is_terminal(status) {
            Scope::Closed
        } else {
            Scope::Open
        };
        Ok(self
            .serve(scope)?
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
        // A move can land in — or leave — a terminal column, so refresh the
        // closed history too, not just the open board.
        self.invalidate_with_closed();
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
        // Cancel lands the card in the terminal `canceled` column.
        self.invalidate_with_closed();
        Ok(out)
    }

    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        reason: &str,
    ) -> Result<Option<StatusMove>> {
        let out = self.inner.move_status_and_unassign(id, to, reason)?;
        // Like `move_status`, the target may be terminal.
        self.invalidate_with_closed();
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
        // The open / closed reads the cache actually calls to fill each scope's
        // snapshot. Each counts as one backend list read (shared `lists`
        // counter) and returns its slice of the board, so a test can prove the
        // open and closed caches are filled and served independently — exactly
        // as a real backend partitions by `state=open` / `state=closed`.
        fn list_open(&self) -> Result<Vec<IssueFile>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .board
                .iter()
                .filter(|f| !is_terminal(&f.task.column))
                .cloned()
                .collect())
        }
        fn list_closed(&self) -> Result<Vec<IssueFile>> {
            self.lists.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .board
                .iter()
                .filter(|f| is_terminal(&f.task.column))
                .cloned()
                .collect())
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
                closed_snapshot: None,
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
        // `list_open` is the render/poll path; it, not `list`, is the cached
        // read. (`list` is now always a live pass-through — see the dedicated
        // test below.)
        let (store, lists, _) = cached("cache-t1", vec![issue("a", "todo")]);
        assert_eq!(store.list_open().unwrap().len(), 1);
        assert_eq!(lists.load(Ordering::SeqCst), 1, "cold read must fetch");
        for _ in 0..5 {
            assert_eq!(store.list_open().unwrap().len(), 1);
        }
        assert_eq!(
            lists.load(Ordering::SeqCst),
            1,
            "warm reads must not touch the backend — this is the whole point"
        );
    }

    #[test]
    fn list_is_always_a_live_pass_through_never_cached() {
        // `list` keeps the full-board contract for migrate / reconcile and is
        // deliberately uncached: every call reaches the backend, and it never
        // pollutes the open snapshot with terminal history.
        let (store, lists, _) = cached("cache-t1b", vec![issue("a", "todo")]);
        for _ in 0..3 {
            assert_eq!(store.list().unwrap().len(), 1);
        }
        assert_eq!(
            lists.load(Ordering::SeqCst),
            3,
            "list must pass through live on every call"
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
        // Repeated non-terminal status reads are served from the one open
        // snapshot — no per-status sweep.
        for _ in 0..5 {
            store.list_in_status(&Column::todo()).unwrap();
            store.list_in_status(&Column::in_progress()).unwrap();
        }
        assert_eq!(
            lists.load(Ordering::SeqCst),
            1,
            "a status filter must not cost a second sweep"
        );
    }

    #[test]
    fn terminal_and_open_statuses_use_independent_caches() {
        // done/canceled are served from the long-TTL closed cache; every other
        // status from the open cache. Each fills once and serves repeatedly,
        // and reading one scope never disturbs the other.
        let (store, lists, _) = cached(
            "cache-t2b",
            vec![issue("a", "todo"), issue("b", "done"), issue("c", "canceled")],
        );
        // First terminal read fills the closed cache (one backend read).
        assert_eq!(store.list_in_status(&Column::done()).unwrap().len(), 1);
        assert_eq!(lists.load(Ordering::SeqCst), 1);
        // canceled is served from the same closed snapshot — no extra read.
        assert_eq!(store.list_in_status(&Column::canceled()).unwrap().len(), 1);
        assert_eq!(
            lists.load(Ordering::SeqCst),
            1,
            "both terminal lanes come from one closed read"
        );
        // The open cache is a separate fill; it must not carry terminal cards.
        let open = store.list_open().unwrap();
        assert_eq!(open.len(), 1);
        assert!(open.iter().all(|f| f.task.column == Column::todo()));
        assert_eq!(lists.load(Ordering::SeqCst), 2, "one open + one closed read");
        // Repeated reads of either scope stay served from cache.
        for _ in 0..5 {
            store.list_in_status(&Column::done()).unwrap();
            store.list_open().unwrap();
        }
        assert_eq!(lists.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_write_passes_through_and_keeps_serving_the_snapshot_without_blocking() {
        let (store, lists, writes) = cached("cache-t3", vec![issue("a", "todo")]);
        store.list_open().unwrap();
        store.set_priority("a", PrioMove::Up).unwrap();
        assert_eq!(writes.load(Ordering::SeqCst), 1, "write must reach backend");
        // Invalidation ages the snapshot but must not discard it: the next read
        // still answers from cache (no multi-second stall after a user action)
        // and the refresh happens on a background thread.
        assert_eq!(store.list_open().unwrap().len(), 1);
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
        store.list_open().unwrap();
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

    // ----- gh `state=` argument, end-to-end through the cache ---------------

    /// A `CachedIssueStore` wrapping a **real** [`crate::GitHubStore`] whose
    /// `gh` runner records every call, so a test can assert the exact
    /// `-f state=…` a cached read path sends through to `gh`. `snapshot` /
    /// `closed_snapshot` are `None` so a single cold read never kicks a
    /// background refresh (it would build a fresh, non-recording store), and
    /// `cfg` is left `file_system` so any refresh that *does_ fire is a harmless
    /// offline read that never touches `gh`. Each test uses a unique `project`
    /// so the process-global cache starts cold.
    fn cached_github(
        project: &str,
    ) -> (CachedIssueStore, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let gh = crate::GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            Ok(String::new()) // empty issues array
        });
        (
            CachedIssueStore {
                inner: Box::new(gh),
                project: project.to_string(),
                cfg: offline_cfg(),
                snapshot: None,
                closed_snapshot: None,
            },
            calls,
        )
    }

    /// The recorded `gh api` issues-list calls (a GET on the issues endpoint,
    /// excluding per-issue comment fetches).
    fn recorded_list_calls(calls: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.contains("repos/owner/repo/issues") && !c.contains("/comments"))
            .cloned()
            .collect()
    }

    #[test]
    fn cached_list_open_sends_exactly_one_state_open() {
        let (store, calls) = cached_github("cache-gh-open");
        store.list_open().unwrap();
        let list = recorded_list_calls(&calls);
        assert_eq!(list.len(), 1, "one page for a board under 100 open issues");
        assert!(list[0].contains("state=open"), "{}", list[0]);
        assert!(!list[0].contains("state=all"), "{}", list[0]);
        assert!(!list[0].contains("state=closed"), "{}", list[0]);
    }

    #[test]
    fn cached_list_in_status_non_terminal_sends_state_open() {
        let (store, calls) = cached_github("cache-gh-todo");
        store.list_in_status(&Column::todo()).unwrap();
        let list = recorded_list_calls(&calls);
        assert_eq!(list.len(), 1);
        assert!(list[0].contains("state=open"), "{}", list[0]);
        assert!(!list[0].contains("state=all"), "{}", list[0]);
    }

    #[test]
    fn cached_list_in_status_terminal_sends_state_closed() {
        let (store, calls) = cached_github("cache-gh-done");
        store.list_in_status(&Column::done()).unwrap();
        let list = recorded_list_calls(&calls);
        assert_eq!(list.len(), 1);
        assert!(list[0].contains("state=closed"), "{}", list[0]);
        assert!(!list[0].contains("state=all"), "{}", list[0]);
        assert!(!list[0].contains("state=open"), "{}", list[0]);
    }

    #[test]
    fn cached_list_still_sends_the_full_state_all_sweep() {
        // The one path that keeps the full contract, for migrate / reconcile.
        let (store, calls) = cached_github("cache-gh-all");
        store.list().unwrap();
        let list = recorded_list_calls(&calls);
        assert_eq!(list.len(), 1);
        assert!(list[0].contains("state=all"), "{}", list[0]);
    }

    #[test]
    fn scoped_read_maps_open_to_state_open_and_closed_to_state_closed() {
        // The exact read the background refresh runs for each scope. Asserted
        // directly against a recording store so the refresh's `state=` is
        // covered without spawning its thread (which rebuilds a fresh store).
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let gh = crate::GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            Ok(String::new())
        });
        scoped_read(&gh, Scope::Open).unwrap();
        scoped_read(&gh, Scope::Closed).unwrap();
        let list = recorded_list_calls(&calls);
        assert_eq!(list.len(), 2);
        assert!(list[0].contains("state=open"), "{}", list[0]);
        assert!(list[1].contains("state=closed"), "{}", list[1]);
        assert!(list.iter().all(|c| !c.contains("state=all")));
    }

    #[test]
    fn a_cold_list_open_warms_the_disk_snapshot_for_the_next_process() {
        // `list_open` (the render/poll path) publishes its cold live read to
        // disk, so a later cold `list_state` in another process starts warm.
        let path = temp_snapshot_path("cache-ds6");
        let _ = std::fs::remove_file(&path);
        let (store, lists, _) =
            cached_with_snapshot("cache-ds6", vec![issue("a", "todo")], Some(path.clone()));
        store.list_open().unwrap();
        assert_eq!(lists.load(Ordering::SeqCst), 1, "cold list_open reads live");
        let read = read_snapshot_from_disk(&path).expect("list_open wrote a snapshot");
        assert_eq!(read.len(), 1, "the live read was persisted to disk");
        let _ = std::fs::remove_file(&path);
    }
}
