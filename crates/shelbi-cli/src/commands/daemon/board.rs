//! Hub-owned board-index refresh: the daemon is the single board reader per
//! hub. Phase 1 of `Plans/github-issue-caching-and-rate-limits.md` §5.
//!
//! One [refresh loop](spawn_refresh_manager) per open project reads the board
//! through the (uncached) store on the project's `issue_tracker.refresh_secs`
//! cadence and publishes it to `<project_dir>/board-index.json`
//! ([`shelbi_state::board_index`]). It appends a `board refreshed=<ts>
//! changed=<n>` line to `events.log` **only when the board actually changed**,
//! so a quiet board is silent. The file itself is rewritten every tick, so its
//! `fetched_at` (and mtime) advance on the interval and stay a live freshness
//! signal even when nothing moved.
//!
//! A [`refresh-board <project>`](BoardRefresher::refresh_now) hub message forces
//! an immediate refresh and replies with the new `fetched_at`; callers wait at
//! most two seconds for it.
//!
//! ## What this slice does *not* do
//!
//! Consumers still read the board through their own process caches — switching
//! them to `board-index.json` is a follow-up (`gh-cache-p1-consumers-read-index`),
//! as is write-through on mutations. Only remote backends are refreshed; a
//! `file_system` project's local read is free and needs no daemon.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};

use shelbi_state::board_index::{self, BoardIndex};
use shelbi_state::gh_budget::{self, BudgetThresholds, BudgetTier, TickPlan};
use shelbi_state::IssueStore;

/// How often the refresh manager wakes to re-discover open projects and run any
/// whose per-project interval has elapsed. Far shorter than the default
/// `refresh_secs` (30s) so a freshly opened project starts refreshing within a
/// couple of seconds; the per-project interval gate keeps the actual backend
/// reads on the configured cadence, not on this slice.
const MANAGER_TICK: Duration = Duration::from_secs(2);

/// Slice the manager sleeps in so a stop signal is noticed within ~250ms rather
/// than up to a full [`MANAGER_TICK`].
const STOP_POLL_SLICE: Duration = Duration::from_millis(250);

/// The outcome of one board refresh: the `fetched_at` written and how many
/// issues changed since the previously published index.
struct RefreshOutcome {
    fetched_at: String,
    changed: usize,
}

/// Shared, cloneable handle to the daemon's board-refresh machinery. Held by
/// both the manager loop and every `refresh-board` socket handler so the two
/// never double-read the same project: each refresh takes that project's
/// single-flight lock.
#[derive(Clone, Default)]
pub(super) struct BoardRefresher {
    /// Per-project single-flight locks, minted on first use. The outer map is
    /// only ever locked briefly to fetch (or create) a project's lock; the
    /// actual refresh holds the inner per-project lock, so refreshes of
    /// *different* projects still run concurrently.
    locks: Arc<Mutex<HashMap<String, Arc<Mutex<()>>>>>,
}

impl BoardRefresher {
    /// The single-flight lock for `project`, created on first request. A
    /// poisoned outer map is recovered rather than propagated — worst case is a
    /// throwaway lock and a possible duplicate read, never a wedged daemon.
    fn lock_for(&self, project: &str) -> Arc<Mutex<()>> {
        let mut guard = self
            .locks
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(guard.entry(project.to_string()).or_default())
    }

    /// Refresh `project` now, blocking on its single-flight lock so a concurrent
    /// tick can't double-read: build the uncached store, publish the index, and
    /// return the outcome (the new `fetched_at` and the change count).
    fn refresh(&self, project: &str) -> Result<RefreshOutcome> {
        let lock = self.lock_for(project);
        let _g = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let store = shelbi_state::raw_issue_store_for(project).map_err(|e| anyhow!(e))?;
        refresh_with_store(project, store.as_ref())
    }

    /// Refresh now and return just the new `fetched_at` — the `refresh-board`
    /// hub verb's reply. The caller waits at most two seconds for it.
    pub(super) fn refresh_now(&self, project: &str) -> Result<String> {
        Ok(self.refresh(project)?.fetched_at)
    }

    /// A tick-driven refresh: errors are logged and swallowed, since a single
    /// failed tick must not take the manager loop down — the previous index
    /// stays in place and the next tick tries again.
    fn tick(&self, project: &str) {
        match self.refresh(project) {
            Ok(out) if out.changed > 0 => {
                tracing::debug!(project, changed = out.changed, "shelbi daemon: board refreshed")
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(project, error = %e, "shelbi daemon: board refresh tick failed")
            }
        }
    }
}

/// Read `project`'s board through `store`, publish it to `board-index.json`, and
/// emit a `board refreshed=<ts> changed=<n>` event **iff** the board changed.
///
/// The file is rewritten every call (so `fetched_at`/mtime advance and stay a
/// live freshness signal), but the events.log line is gated on a real change so
/// a quiet board produces no log noise. Split from the store construction so it
/// is unit-testable with a fake [`IssueStore`].
fn refresh_with_store(project: &str, store: &dyn IssueStore) -> Result<RefreshOutcome> {
    let previous = board_index::read_board_index(project);
    // The previous index's `fetched_at` is the incremental watermark: read only
    // issues touched since it. `None` (no prior index, or a torn one) forces a
    // cold full read. It parses as RFC3339 — a value we can't parse is treated as
    // "no watermark" so a corrupt timestamp falls back to a safe cold read rather
    // than an error.
    let since = previous
        .as_ref()
        .and_then(|p| DateTime::parse_from_rfc3339(&p.fetched_at).ok())
        .map(|dt| dt.with_timezone(&Utc));
    let prev_board: &[shelbi_state::IssueFile] =
        previous.as_ref().map(|p| p.board.as_slice()).unwrap_or(&[]);

    // Stamp `fetched_at` *before* the read so the next tick's `since` never skips
    // an update that lands while this read is in flight (see
    // `BoardIndex::fresh_at`).
    let fetched_at = Utc::now().to_rfc3339();
    let read = store.refresh_board(since, prev_board).map_err(|e| anyhow!(e))?;

    let changed =
        board_index::board_diff_count(previous.as_ref().map(|p| p.board.as_slice()), &read.board);
    // Merge the numbers this read observed onto the prior index's id→number map,
    // then retain only ids still on the board. An incremental tick only reports
    // numbers for the issues it touched, so without the merge an untouched
    // issue's number would drop out of the index every quiet tick and force a
    // `get` back to the label search.
    let numbers = merge_index_numbers(previous.as_ref(), &read.numbers, &read.board);
    let index = BoardIndex::fresh_at(read.board, numbers, fetched_at, read.remaining, read.reset);
    board_index::write_board_index(project, &index).map_err(|e| anyhow!(e))?;

    if changed > 0 {
        emit_board_refreshed(project, &index.fetched_at, changed, index.remaining);
    }
    Ok(RefreshOutcome {
        fetched_at: index.fetched_at,
        changed,
    })
}

/// Merge the numbers a refresh observed onto the prior index's id→number map,
/// keeping only ids still on the new board.
///
/// A cold read reports every open issue's number, so the merge is a full
/// replace; an incremental read reports numbers only for the issues it touched,
/// so folding them onto the previous map preserves the numbers of untouched
/// issues (which the delta never mentions) while dropping any id that has left
/// the open board. The result is the complete id→number map for exactly the
/// issues on `board`.
fn merge_index_numbers(
    previous: Option<&BoardIndex>,
    observed: &[(String, i64)],
    board: &[shelbi_state::IssueFile],
) -> Vec<(String, i64)> {
    let mut numbers: HashMap<String, i64> = previous
        .map(|p| p.numbers.clone().into_iter().collect())
        .unwrap_or_default();
    for (id, number) in observed {
        numbers.insert(id.clone(), *number);
    }
    let on_board: std::collections::HashSet<&str> =
        board.iter().map(|f| f.task.id.as_str()).collect();
    numbers.retain(|id, _| on_board.contains(id.as_str()));
    numbers.into_iter().collect()
}

/// Append the `board refreshed=<ts> changed=<n> [remaining=<n>]` line for
/// `project`. The `remaining` GraphQL points budget is appended when the read
/// surfaced it (the GraphQL board path), omitted otherwise. Best-effort: a
/// failed events append is logged, never propagated — the index file is the
/// durable artifact, the event is the orchestrator's nudge.
fn emit_board_refreshed(project: &str, fetched_at: &str, changed: usize, remaining: Option<u64>) {
    let mut body = format!("project={project} board refreshed={fetched_at} changed={changed}");
    if let Some(remaining) = remaining {
        body.push_str(&format!(" remaining={remaining}"));
    }
    if let Err(e) = shelbi_state::append_external_event(&body) {
        tracing::debug!(project, error = %e, "shelbi daemon: failed to append board-refreshed event");
    }
}

/// Spawn the board-refresh manager thread. Wakes every [`MANAGER_TICK`],
/// discovers open projects on a remote backend, and refreshes any whose
/// per-project interval has elapsed. Exits when the shared stop flag is set
/// (the same flag the accept loop and reaper watch, so one signal stops all).
pub(super) fn spawn_refresh_manager(refresher: BoardRefresher, stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("shelbi-board-refresh-mgr".into())
        .spawn(move || refresh_manager_loop(&refresher, &stop))
        .ok();
}

fn refresh_manager_loop(refresher: &BoardRefresher, stop: &AtomicBool) {
    // Last successful/attempted refresh time per project, so each project ticks
    // on its own governed interval rather than on MANAGER_TICK.
    let mut last: HashMap<String, Instant> = HashMap::new();
    // Per-project token key, resolved once and cached — the budget file is keyed
    // by a hash of the token, and resolving it can shell out to the keychain, so
    // we must not do it every MANAGER_TICK.
    let mut token_keys: HashMap<String, Option<String>> = HashMap::new();
    // Projects currently paused by the governor, so a pause logs once per episode
    // (on entry) rather than every 2s tick.
    let mut paused: std::collections::HashSet<String> = std::collections::HashSet::new();
    while !stop.load(Ordering::SeqCst) {
        let open = open_remote_projects();
        for project in &open {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let now = chrono::Utc::now().timestamp();
            match governor_plan(project, &mut token_keys, now) {
                TickPlan::Refresh(interval) => {
                    if paused.remove(project) {
                        tracing::info!(project, "shelbi daemon: board refresh resumed (budget recovered)");
                    }
                    let due = last.get(project).map_or(true, |t| t.elapsed() >= interval);
                    if due {
                        refresher.tick(project);
                        last.insert(project.clone(), Instant::now());
                    }
                }
                TickPlan::Pause { until } => {
                    // Budget too low (or parked): skip the list read, serve the
                    // last index (marked stale by its age). Log once per episode.
                    if paused.insert(project.clone()) {
                        tracing::warn!(
                            project,
                            until,
                            "shelbi daemon: board refresh paused (low/parked GraphQL budget); serving cache until reset",
                        );
                    }
                }
            }
        }
        // Drop tracking for projects that have closed, so a reopen refreshes
        // immediately rather than waiting out its stale last-tick time.
        let open_set: std::collections::HashSet<&String> = open.iter().collect();
        last.retain(|p, _| open_set.contains(p));
        token_keys.retain(|p, _| open_set.contains(p));
        paused.retain(|p| open_set.contains(p));
        sleep_until_stop(MANAGER_TICK, stop);
    }
}

/// The governor's decision for this project's tick (plan Phase 3 §6): scale the
/// configured cadence, or pause the refresh, from the token's GraphQL budget and
/// the project's `issue_tracker.budget` thresholds.
fn governor_plan(
    project: &str,
    token_keys: &mut HashMap<String, Option<String>>,
    now: i64,
) -> TickPlan {
    let cfg = shelbi_state::load_project(project)
        .map(|p| p.issue_tracker)
        .unwrap_or_default();
    let budget = &cfg.budget;
    let thresholds = BudgetThresholds {
        graphql_high: budget.graphql_high(),
        graphql_medium: budget.graphql_medium(),
        base_secs: cfg.refresh_interval_secs(),
        slow_secs: budget.slow_refresh_secs(),
    };
    let tier = graphql_tier_for(project, token_keys);
    gh_budget::tick_plan(&tier, &thresholds, now)
}

/// The GraphQL budget tier the governor scales from: the per-token `budget.json`
/// (the plan's per-token, hub-wide source), or — when the token can't be resolved
/// in the daemon — the per-project board index's last-seen `remaining`/`reset`,
/// which needs no token. A cold hub with neither reads as the default (unknown)
/// tier, which the governor runs at the configured cadence.
fn graphql_tier_for(project: &str, token_keys: &mut HashMap<String, Option<String>>) -> BudgetTier {
    if let Some(key) = token_key_for(project, token_keys) {
        return gh_budget::read_state(&key).graphql;
    }
    board_index::read_board_index(project)
        .map(|idx| BudgetTier {
            remaining: idx.remaining.map(|r| r as i64),
            reset_at: idx.reset,
            parked_until: None,
        })
        .unwrap_or_default()
}

/// The token-file key for `project`, resolved once and memoized in `token_keys`
/// (a `None` entry memoizes a resolution failure so it isn't retried every tick).
fn token_key_for(project: &str, token_keys: &mut HashMap<String, Option<String>>) -> Option<String> {
    if let Some(cached) = token_keys.get(project) {
        return cached.clone();
    }
    let key = shelbi_state::resolve_github_token_by_name(project)
        .ok()
        .map(|t| gh_budget::token_key(t.expose()));
    token_keys.insert(project.to_string(), key.clone());
    key
}

/// Sleep up to `total`, waking early (within [`STOP_POLL_SLICE`]) when `stop`
/// is set — the manager's responsiveness to SIGTERM.
fn sleep_until_stop(total: Duration, stop: &AtomicBool) {
    let mut waited = Duration::ZERO;
    while waited < total && !stop.load(Ordering::SeqCst) {
        thread::sleep(STOP_POLL_SLICE);
        waited += STOP_POLL_SLICE;
    }
}

/// Open projects (a live `shelbi-<name>` tmux session) whose issue tracker is a
/// remote backend — the only ones a daemon refresh helps. A `file_system`
/// project's board is a free local read and is skipped. tmux being unreachable
/// (an empty listing) resolves to "no open projects": consumers then keep
/// reading through their per-process caches, degraded but safe.
fn open_remote_projects() -> Vec<String> {
    open_project_names()
        .into_iter()
        .filter(|p| project_uses_remote_backend(p))
        .collect()
}

/// True when `project`'s configured backend is remote (github / jira / linear).
/// A load failure reads as "not remote" so a half-written config during
/// teardown never trips a refresh.
fn project_uses_remote_backend(project: &str) -> bool {
    shelbi_state::load_project(project)
        .map(|p| p.issue_tracker.backend.is_remote())
        .unwrap_or(false)
}

/// Project names with a live `shelbi-<name>` tmux session. Mirrors the
/// discovery `shelbi quit` uses; the hidden `_shelbi-<name>` stash sessions are
/// excluded (their prefix is `_shelbi-`, not `shelbi-`). Empty when tmux is
/// unreachable.
fn open_project_names() -> Vec<String> {
    let listing = std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    parse_open_project_names(&listing)
}

/// Extract `<name>` from each `shelbi-<name>` session line, skipping the
/// `_shelbi-` stash sessions, blanks, and the prefix-only `shelbi-` line.
fn parse_open_project_names(listing: &str) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let name = line.trim();
            let rest = name.strip_prefix("shelbi-")?;
            if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{Column, Issue, Result as CoreResult};
    use shelbi_state::issue_store::{
        Cursor, IssueChange, IssueComment, IssueFields, NewIssue, PrioMove, StatusMove,
    };
    use shelbi_state::IssueFile;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicUsize;

    /// A fake store returning a fixed open board and counting `list_open`
    /// calls, so a test can drive the refresh without a real backend.
    ///
    /// It overrides [`IssueStore::refresh_board`] to record each `since`
    /// watermark it is handed and to surface a configurable budget, so a test
    /// can assert the daemon threads the previous index's `fetched_at` and lands
    /// the budget in the new index. The override still calls `list_open`, so the
    /// `opens` counter (and the existing tests keyed on it) keep working.
    struct FakeStore {
        board: Vec<IssueFile>,
        opens: Arc<AtomicUsize>,
        /// Count of closed-history reads (`list_closed` / `closed_page`). The
        /// daemon tick must never touch closed issues (§4), so a test asserts
        /// this stays zero across refreshes.
        closeds: Arc<AtomicUsize>,
        /// Every `since` `refresh_board` was called with, in order.
        seen_since: SinceLog,
        /// The `(remaining, reset)` budget the fake reports on each read.
        budget: (Option<u64>, Option<i64>),
    }

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

    #[test]
    fn merge_index_numbers_keeps_untouched_adds_new_and_drops_off_board() {
        // Prior index maps a→1, b→2. An incremental read observed only b (moved)
        // and a new c→3, and the board now holds a, b, c (a untouched, d gone).
        let mut prev = BoardIndex::fresh(vec![issue("a", "todo", 0), issue("b", "todo", 1)]);
        prev.numbers = [("a".to_string(), 1), ("b".to_string(), 2), ("d".to_string(), 4)]
            .into_iter()
            .collect();
        let observed = vec![("b".to_string(), 2), ("c".to_string(), 3)];
        let board = vec![issue("a", "todo", 0), issue("b", "todo", 1), issue("c", "todo", 2)];

        let merged: std::collections::BTreeMap<String, i64> =
            merge_index_numbers(Some(&prev), &observed, &board).into_iter().collect();
        assert_eq!(merged.get("a"), Some(&1), "untouched issue keeps its number");
        assert_eq!(merged.get("b"), Some(&2), "touched issue's number preserved");
        assert_eq!(merged.get("c"), Some(&3), "new issue's number added");
        assert_eq!(merged.get("d"), None, "an issue off the board is dropped");
    }

    #[test]
    fn merge_index_numbers_is_a_full_replace_with_no_prior_index() {
        let observed = vec![("a".to_string(), 1)];
        let board = vec![issue("a", "todo", 0)];
        let merged: Vec<(String, i64)> = merge_index_numbers(None, &observed, &board);
        assert_eq!(merged, vec![("a".to_string(), 1)]);
    }

    impl IssueStore for FakeStore {
        fn list(&self) -> CoreResult<Vec<IssueFile>> {
            Ok(self.board.clone())
        }
        fn list_open(&self) -> CoreResult<Vec<IssueFile>> {
            self.opens.fetch_add(1, Ordering::SeqCst);
            Ok(self.board.clone())
        }
        fn list_closed(&self) -> CoreResult<Vec<IssueFile>> {
            self.closeds.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
        fn closed_page(
            &self,
            _after: Option<&str>,
        ) -> CoreResult<shelbi_state::ClosedPage> {
            self.closeds.fetch_add(1, Ordering::SeqCst);
            Ok(shelbi_state::ClosedPage {
                issues: Vec::new(),
                next_cursor: None,
                remaining: None,
                reset: None,
            })
        }
        fn refresh_board(
            &self,
            since: Option<DateTime<Utc>>,
            _previous: &[IssueFile],
        ) -> CoreResult<shelbi_state::BoardRead> {
            self.seen_since.lock().unwrap().push(since);
            Ok(shelbi_state::BoardRead {
                board: self.list_open()?,
                numbers: Vec::new(),
                remaining: self.budget.0,
                reset: self.budget.1,
            })
        }
        fn list_in_status(&self, status: &Column) -> CoreResult<Vec<IssueFile>> {
            Ok(self
                .board
                .iter()
                .filter(|f| &f.task.column == status)
                .cloned()
                .collect())
        }
        fn get(&self, _id: &str) -> CoreResult<Option<IssueFile>> {
            Ok(None)
        }
        fn add(&self, _s: NewIssue) -> CoreResult<Issue> {
            unreachable!()
        }
        fn move_status(&self, _i: &str, _t: &Column, _r: &str) -> CoreResult<Option<StatusMove>> {
            Ok(None)
        }
        fn set_priority(&self, _i: &str, _p: PrioMove) -> CoreResult<()> {
            Ok(())
        }
        fn set_fields(&self, _i: &str, _f: IssueFields) -> CoreResult<()> {
            Ok(())
        }
        fn cancel(&self, _i: &str, _r: &str) -> CoreResult<Option<StatusMove>> {
            Ok(None)
        }
        fn move_status_and_unassign(
            &self,
            _i: &str,
            _t: &Column,
            _r: &str,
        ) -> CoreResult<Option<StatusMove>> {
            Ok(None)
        }
        fn delete(&self, _id: &str) -> CoreResult<()> {
            Ok(())
        }
        fn renumber(&self, _s: &Column) -> CoreResult<()> {
            Ok(())
        }
        fn park_review(&self, _id: &str) -> CoreResult<Option<String>> {
            Ok(None)
        }
        fn clear_parked(&self, _id: &str) -> CoreResult<()> {
            Ok(())
        }
        fn reject_review(
            &self,
            _i: &str,
            _r: &Column,
            _s: &str,
            _d: &str,
        ) -> CoreResult<Option<StatusMove>> {
            Ok(None)
        }
        fn poll_changes(&self, _s: &Cursor) -> CoreResult<(Vec<IssueChange>, Cursor)> {
            Ok((Vec::new(), Cursor::start()))
        }
        fn list_comments(&self, _id: &str) -> CoreResult<Vec<IssueComment>> {
            Ok(Vec::new())
        }
        fn add_comment(&self, _id: &str, _b: &str) -> CoreResult<IssueComment> {
            unreachable!()
        }
    }

    /// RAII guard pointing `$SHELBI_HOME` at a fresh temp dir so a test's board
    /// index and events.log writes land in isolation, never in the developer's
    /// real `~/.shelbi`. Holds the shared ENV_LOCK because `set_var` is
    /// process-global and tests run in parallel.
    struct IsolatedHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        home: PathBuf,
    }
    impl IsolatedHome {
        fn new(tag: &str) -> Self {
            let lock = crate::commands::test_support::ENV_LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-board-daemon-{tag}-{}-{}",
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

    fn fake(board: Vec<IssueFile>) -> (FakeStore, Arc<AtomicUsize>) {
        let opens = Arc::new(AtomicUsize::new(0));
        (
            FakeStore {
                board,
                opens: Arc::clone(&opens),
                closeds: Arc::new(AtomicUsize::new(0)),
                seen_since: Arc::new(Mutex::new(Vec::new())),
                budget: (None, None),
            },
            opens,
        )
    }

    /// A fake plus a shared count of its closed-history reads, so a test can
    /// assert the daemon tick never touches closed issues (§4).
    fn fake_with_closed_counter(board: Vec<IssueFile>) -> (FakeStore, Arc<AtomicUsize>) {
        let closeds = Arc::new(AtomicUsize::new(0));
        (
            FakeStore {
                board,
                opens: Arc::new(AtomicUsize::new(0)),
                closeds: Arc::clone(&closeds),
                seen_since: Arc::new(Mutex::new(Vec::new())),
                budget: (None, None),
            },
            closeds,
        )
    }

    /// A shared log of every `since` watermark a fake store was asked to read at.
    type SinceLog = Arc<Mutex<Vec<Option<DateTime<Utc>>>>>;

    /// A fake plus the shared `since`-recording handle and a configurable budget.
    fn fake_with_budget(
        board: Vec<IssueFile>,
        budget: (Option<u64>, Option<i64>),
    ) -> (FakeStore, SinceLog) {
        let seen_since = Arc::new(Mutex::new(Vec::new()));
        (
            FakeStore {
                board,
                opens: Arc::new(AtomicUsize::new(0)),
                closeds: Arc::new(AtomicUsize::new(0)),
                seen_since: Arc::clone(&seen_since),
                budget,
            },
            seen_since,
        )
    }

    fn events_lines() -> Vec<String> {
        std::fs::read_to_string(shelbi_state::events_log_path().unwrap())
            .map(|s| s.lines().map(str::to_string).collect())
            .unwrap_or_default()
    }

    #[test]
    fn first_refresh_writes_the_index_and_emits_one_changed_line() {
        let _iso = IsolatedHome::new("first");
        let (store, opens) = fake(vec![issue("a", "todo", 0), issue("b", "review", 0)]);
        let out = refresh_with_store("proj", &store).unwrap();
        assert_eq!(opens.load(Ordering::SeqCst), 1, "one backend read");
        assert_eq!(out.changed, 2, "a cold first tick counts every issue new");

        // The index file exists and holds the board + a matching fetched_at.
        let idx = board_index::read_board_index("proj").expect("index written");
        assert_eq!(idx.board.len(), 2);
        assert_eq!(idx.fetched_at, out.fetched_at);
        assert!(!idx.stale);

        // Exactly one `board refreshed` line, carrying the project + count.
        let refreshed: Vec<_> = events_lines()
            .into_iter()
            .filter(|l| l.contains("board refreshed="))
            .collect();
        assert_eq!(refreshed.len(), 1, "one refreshed line: {refreshed:?}");
        assert!(refreshed[0].contains("project=proj"), "{}", refreshed[0]);
        assert!(refreshed[0].contains("changed=2"), "{}", refreshed[0]);
    }

    #[test]
    fn a_quiet_tick_rewrites_the_file_but_emits_no_line() {
        let _iso = IsolatedHome::new("quiet");
        let (store, _) = fake(vec![issue("a", "todo", 0)]);
        // Prime the index.
        let first = refresh_with_store("proj", &store).unwrap();
        assert_eq!(first.changed, 1);

        // Second tick over an unchanged board: no new event, but fetched_at
        // advances so the file stays a live freshness signal.
        std::thread::sleep(Duration::from_millis(5));
        let second = refresh_with_store("proj", &store).unwrap();
        assert_eq!(second.changed, 0, "an unchanged board reports no change");
        assert_ne!(
            second.fetched_at, first.fetched_at,
            "fetched_at advances every tick even when quiet"
        );

        // A third quiet tick, then assert still exactly one refreshed line
        // total (only the first, changed tick emitted).
        let third = refresh_with_store("proj", &store).unwrap();
        assert_eq!(third.changed, 0);
        let refreshed = events_lines()
            .into_iter()
            .filter(|l| l.contains("board refreshed="))
            .count();
        assert_eq!(refreshed, 1, "two quiet ticks add no lines");
    }

    #[test]
    fn a_changed_board_emits_exactly_one_line_on_the_next_tick() {
        let _iso = IsolatedHome::new("changed");
        // Prime with one board.
        let (store, _) = fake(vec![issue("a", "todo", 0)]);
        refresh_with_store("proj", &store).unwrap();

        // Now the board moves: `a` changes column. A fresh store models the
        // next tick reading the moved board.
        let (moved, _) = fake(vec![issue("a", "in_progress", 0)]);
        let out = refresh_with_store("proj", &moved).unwrap();
        assert_eq!(out.changed, 1, "one issue moved");

        let refreshed = events_lines()
            .into_iter()
            .filter(|l| l.contains("board refreshed="))
            .count();
        assert_eq!(
            refreshed, 2,
            "the priming tick and the move each emit exactly one line"
        );
    }

    #[test]
    fn first_tick_reads_cold_then_subsequent_ticks_read_incrementally() {
        let _iso = IsolatedHome::new("since");
        let (store, seen_since) = fake_with_budget(vec![issue("a", "todo", 0)], (None, None));

        // First tick: no prior index ⇒ cold read (`since` is None).
        let first = refresh_with_store("proj", &store).unwrap();
        // Second tick: the prior index's `fetched_at` becomes the incremental
        // watermark.
        let second = refresh_with_store("proj", &store).unwrap();

        let seen = seen_since.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(seen[0].is_none(), "first tick is a cold read");
        let since = seen[1].expect("second tick reads incrementally");
        // The watermark equals the *first* index's fetched_at (stamped before
        // that read), to the RFC3339 second.
        let expected = DateTime::parse_from_rfc3339(&first.fetched_at)
            .unwrap()
            .with_timezone(&Utc);
        assert_eq!(since, expected);
        // The second index advanced its own fetched_at.
        assert_ne!(second.fetched_at, first.fetched_at);
    }

    #[test]
    fn a_surfaced_budget_lands_in_the_index_and_the_events_line() {
        let _iso = IsolatedHome::new("budget");
        let (store, _) = fake_with_budget(vec![issue("a", "todo", 0)], (Some(4989), Some(1_800_000_000)));

        refresh_with_store("proj", &store).unwrap();

        // The index carries the GraphQL budget the read reported.
        let idx = board_index::read_board_index("proj").expect("index written");
        assert_eq!(idx.remaining, Some(4989));
        assert_eq!(idx.reset, Some(1_800_000_000));

        // The changed tick's events line carries `remaining=`.
        let refreshed: Vec<_> = events_lines()
            .into_iter()
            .filter(|l| l.contains("board refreshed="))
            .collect();
        assert_eq!(refreshed.len(), 1);
        assert!(refreshed[0].contains("remaining=4989"), "{}", refreshed[0]);
    }

    #[test]
    fn a_refresh_tick_never_reads_closed_history() {
        // §4: done/canceled are loaded on demand, never on the daemon cadence.
        // Several ticks (cold then incremental) must issue zero closed reads.
        let _iso = IsolatedHome::new("no-closed");
        let (store, closeds) = fake_with_closed_counter(vec![issue("a", "todo", 0)]);
        refresh_with_store("proj", &store).unwrap();
        refresh_with_store("proj", &store).unwrap();
        refresh_with_store("proj", &store).unwrap();
        assert_eq!(
            closeds.load(Ordering::SeqCst),
            0,
            "the daemon tick must never request closed issues"
        );
    }

    #[test]
    fn refresher_single_flights_per_project_lock() {
        // The same project hands back the same lock (single-flight); a
        // different project gets a distinct one (so unrelated projects refresh
        // concurrently).
        let r = BoardRefresher::default();
        let a1 = r.lock_for("alpha");
        let a2 = r.lock_for("alpha");
        let b = r.lock_for("bravo");
        assert!(Arc::ptr_eq(&a1, &a2), "same project ⇒ same lock");
        assert!(!Arc::ptr_eq(&a1, &b), "different project ⇒ different lock");
    }

    #[test]
    fn parse_open_project_names_strips_prefix_and_skips_stash() {
        let listing = "shelbi-alpha\n_shelbi-alpha\nplain\nshelbi-\n   shelbi-bravo\n";
        assert_eq!(parse_open_project_names(listing), vec!["alpha", "bravo"]);
    }
}
