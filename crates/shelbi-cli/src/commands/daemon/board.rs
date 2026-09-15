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

/// The longest a published board may go without a full **cold read** — the
/// paginated open-board query that rebuilds the whole open set. Incremental
/// deltas serve every tick between cold reads, but a delta (`filterBy: { since }`)
/// can only add and update rows: an issue hard-deleted or transferred out of the
/// repository never appears in one, so its card and id→number entry would survive
/// forever. Forcing a cold read at least this often reconciles those vanished
/// rows away within one period. Ten minutes is six cold reads per hour per open
/// project against the GraphQL points budget — negligible next to a delta per
/// tick — while bounding how long a ghost row can linger.
const COLD_READ_INTERVAL: Duration = Duration::from_secs(600);

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
    /// actual refresh holds the inner per-project lock so a `refresh-board`
    /// socket handler and a manager tick never double-read the *same* project.
    /// This lock does **not** buy cross-project concurrency: the manager loop
    /// ([`refresh_manager_loop`], the single `shelbi-board-refresh-mgr` thread)
    /// drives every open project's tick sequentially, so a slow (or wedged)
    /// refresh already serializes every other project — the per-project lock only
    /// guards a tick against a concurrent socket refresh of that one project.
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
    /// `interval_secs` is the cadence the governor chose for this tick, stamped
    /// onto the published index so readers compute freshness against the interval
    /// the daemon was actually ticking at. `None` — a manual `refresh-board` with
    /// no governor decision in hand — carries the previous index's recorded cadence
    /// forward rather than resetting it (see [`refresh_with_store`]).
    fn refresh(&self, project: &str, interval_secs: Option<u64>) -> Result<RefreshOutcome> {
        let lock = self.lock_for(project);
        let _g = lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let store = shelbi_state::raw_issue_store_for(project).map_err(|e| anyhow!(e))?;
        refresh_with_store(project, store.as_ref(), interval_secs)
    }

    /// Refresh now and return just the new `fetched_at` — the `refresh-board`
    /// hub verb's reply. The caller waits at most two seconds for it. Carries no
    /// governor decision, so it passes `None` and the recorded cadence is preserved.
    pub(super) fn refresh_now(&self, project: &str) -> Result<String> {
        Ok(self.refresh(project, None)?.fetched_at)
    }

    /// A tick-driven refresh: errors are logged and swallowed, since a single
    /// failed tick must not take the manager loop down — the previous index
    /// stays in place and the next tick tries again.
    ///
    /// On failure the previous index is rewritten **stale**, keeping its last
    /// good board but refreshing its budget envelope from the token's GraphQL
    /// budget (Phase 3, review note 1). Without this a rate-limit park or network
    /// blip would leave the file silently un-updated — the board would still read
    /// as stale once its age crossed the threshold, but the `quota resets HH:MM`
    /// banner would have no reset time to show until then. `tier` is the token's
    /// last-seen GraphQL budget, resolved once by the manager loop and threaded
    /// in so the tick doesn't re-shell to the keychain.
    fn tick(&self, project: &str, tier: &BudgetTier, interval_secs: u64) {
        handle_tick_result(project, self.refresh(project, Some(interval_secs)), tier);
    }
}

/// Apply one tick's outcome: log a real change, stay silent on a quiet tick, and
/// on a failure (a rate-limit park, a network blip) rewrite the published index
/// stale so it keeps rendering with the reset time (review note 1). Split from
/// [`BoardRefresher::tick`] — which builds the store under the single-flight lock
/// — so the failed-tick / stale-marking behavior is unit-testable with a fake
/// [`IssueStore`].
fn handle_tick_result(project: &str, result: Result<RefreshOutcome>, tier: &BudgetTier) {
    match result {
        Ok(out) if out.changed > 0 => {
            clear_refresh_error(project);
            tracing::debug!(project, changed = out.changed, "shelbi daemon: board refreshed")
        }
        Ok(_) => clear_refresh_error(project),
        Err(e) => {
            report_refresh_failure(project, &e);
            mark_index_stale_from_tier(project, tier);
        }
    }
}

/// Record a failing refresh tick and surface it **once per failure episode**
/// (not once per tick): the first failure after a healthy run warns and drops a
/// single `board refresh-failed` line on events.log carrying the error text;
/// subsequent failures in the same episode only refresh the persisted error the
/// status/doctor GitHub section reads. The per-episode gate lives on disk (the
/// error sidecar's presence), so it survives a daemon restart and is testable
/// without loop state. Best-effort: a failed record still logs at debug so the
/// signal isn't wholly lost.
fn report_refresh_failure(project: &str, error: &anyhow::Error) {
    let text = format!("{error:#}");
    match shelbi_state::record_board_refresh_error(project, &text) {
        Ok(true) => {
            tracing::warn!(project, error = %error, "shelbi daemon: board refresh failed; serving last index and marking status");
            // Single-line the error so it can't tear the events.log record.
            let line_safe = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let body = format!("project={project} board refresh-failed error={line_safe}");
            if let Err(e) = shelbi_state::append_external_event(&body) {
                tracing::debug!(project, error = %e, "shelbi daemon: failed to append board refresh-failed event");
            }
        }
        Ok(false) => {
            // Same episode continuing — stay quiet, but keep the debug breadcrumb.
            tracing::debug!(project, error = %error, "shelbi daemon: board refresh tick failed (episode ongoing)");
        }
        Err(e) => {
            tracing::debug!(project, record_error = %e, error = %error, "shelbi daemon: board refresh tick failed; could not persist error");
        }
    }
}

/// Clear a project's persisted refresh error after a successful tick, so a
/// recovered board stops advertising a stale failure. Best-effort.
fn clear_refresh_error(project: &str) {
    if let Err(e) = shelbi_state::clear_board_refresh_error(project) {
        tracing::debug!(project, error = %e, "shelbi daemon: failed to clear board refresh error");
    }
}

/// Rewrite `project`'s published index as stale, carrying the `tier`'s last-seen
/// `remaining`/`reset` into it so a failed tick or a paused governor still shows
/// the `quota resets HH:MM` banner with a real reset time. Best-effort: a rewrite
/// error is logged, never propagated.
fn mark_index_stale_from_tier(project: &str, tier: &BudgetTier) {
    let remaining = tier.remaining.and_then(|r| u64::try_from(r).ok());
    if let Err(e) = shelbi_state::mark_board_index_stale(project, remaining, tier.reset_at) {
        tracing::debug!(project, error = %e, "shelbi daemon: failed to mark board index stale");
    }
}

/// Read `project`'s board through `store`, publish it to `board-index.json`, and
/// emit a `board refreshed=<ts> changed=<n>` event **iff** the board changed.
///
/// The file is rewritten every call (so `fetched_at`/mtime advance and stay a
/// live freshness signal), but the events.log line is gated on a real change so
/// a quiet board produces no log noise. Split from the store construction so it
/// is unit-testable with a fake [`IssueStore`].
fn refresh_with_store(
    project: &str,
    store: &dyn IssueStore,
    interval_secs: Option<u64>,
) -> Result<RefreshOutcome> {
    // The repository identity to stamp on this publish, from the project's tracker
    // config — the same config the store was built from. A read path validates a
    // published index against this and treats a mismatch as no index. In
    // production the store was just built from this config, so the load succeeds;
    // a unit test driving `refresh_with_store` with a fake store and no registered
    // project gets `None`, which is stamped and matches its own later ticks.
    let expected_repo = shelbi_state::load_project(project)
        .ok()
        .and_then(|p| board_index::expected_board_repo(&p.issue_tracker));

    // Only a previous index that still describes THIS repository (and the current
    // schema) counts. A mismatched or pre-identity file (a retargeted project, an
    // upgrade) is treated as "no prior index": a full cold read that then
    // republishes with the correct identity.
    let previous = board_index::read_board_index(project)
        .filter(|p| p.identity_matches(expected_repo.as_deref()));

    // The incremental watermark, or `None` to force a full cold read. Cold when
    // there is no valid prior index, when the cold-read schedule is due (the last
    // full read is missing, unparseable, or older than [`COLD_READ_INTERVAL`]), or
    // when the previous `fetched_at` doesn't parse. Between cold reads the previous
    // `fetched_at` is the delta watermark, so steady-state cost is one delta per
    // tick plus a periodic full reconcile that drops any vanished issue.
    let since = previous.as_ref().and_then(cold_read_watermark);

    // The cold-read schedule timestamp an incremental tick carries forward
    // untouched; a cold read resets it to this tick's `fetched_at` below.
    let prev_cold_read = previous.as_ref().and_then(|p| p.last_cold_read.clone());

    // The refresh cadence to record on this publish. A governed tick passes the
    // interval the governor chose; a manual `refresh-board` passes `None` and we
    // carry forward whatever the last governed tick recorded, so a manual refresh
    // never un-throttles the cadence a reader judges freshness against.
    let interval_secs = interval_secs.or_else(|| previous.as_ref().and_then(|p| p.interval_secs));

    // The `prev_board` slice is the merge base: the board the store folds an
    // incremental delta onto, and the base of the three-way merge below. Owned
    // (not borrowed from `previous`) so it can move into the locked closure.
    let prev_board: Vec<shelbi_state::IssueFile> =
        previous.map(|p| p.board).unwrap_or_default();

    // Stamp `fetched_at` *before* the read so the next tick's `since` never skips
    // an update that lands while this read is in flight (see
    // `BoardIndex::fresh_at`). The read itself runs with **no lock held** — a
    // daemon stalled inside this network call must never block a concurrent
    // `shelbi issue move`, which takes the board-index lock, publishes and exits
    // while we wait here.
    let fetched_at = Utc::now().to_rfc3339();
    let read = store.refresh_board(since, &prev_board).map_err(|e| anyhow!(e))?;

    // Which read actually ran decides how we reconcile against the current index.
    // A cold read (`since` is None) and the REST fallback both return the *whole*
    // open board rather than a delta folded onto `prev_board`, so they are
    // authoritative and publish wholesale; a true incremental delta embeds the
    // `prev_board` base and must be three-way merged so a CLI write-through that
    // landed mid-read survives.
    let authoritative = since.is_none() || read.rest_fallback;
    // Both full-read paths (cold read and REST fallback) give an authoritative
    // whole-open-board view that reconciles deletions, so either refreshes the
    // cold-read schedule; a true incremental delta carries the previous timestamp
    // forward so the ten-minute floor keeps advancing toward the next cold read.
    let last_cold_read = if authoritative {
        Some(fetched_at.clone())
    } else {
        prev_cold_read
    };
    let fetched_at_return = fetched_at.clone();
    let remaining_return = read.remaining;

    // The reopened issues to announce on this tick: those the read observed open
    // on GitHub while still carrying a terminal `shelbi:status/*` label, minus
    // any already on the previous open index. Computed *before* the publish
    // closure moves `read` and `prev_board`. Interpretation only — no write, no
    // label repair, no `gh` mutation (decision 7). The dedup key is presence on
    // the previous open index, so two known gaps re-announce once: an issue that
    // was already open when the terminal label appeared (a human labeling an
    // open backlog card) is never announced, and a cold read with no prior index
    // (a daemon restart with a deleted `board-index.json`) re-announces. Closing
    // either needs persistent per-id state, out of scope here.
    let reopened_to_emit: Vec<(String, String)> = read
        .reopened
        .iter()
        .filter(|(id, _)| !prev_board.iter().any(|tf| tf.task.id == *id))
        .cloned()
        .collect();

    // Publish inside the board-index lock, reconciling the read against the index
    // as re-read *under* the lock (`fresh`) rather than against the pre-call copy,
    // so a write-through that landed while the read was in flight is not lost. The
    // events append happens after the guard drops (below), never inside it.
    let changed = board_index::update_board_index(project, move |fresh| {
        let ours: &[shelbi_state::IssueFile] =
            fresh.as_ref().map(|f| f.board.as_slice()).unwrap_or(&[]);
        let published_board = if authoritative {
            // The full open set is authoritative: publish it wholesale. An id it
            // no longer returns is closed/deleted/transferred and must be dropped
            // even if a write-through patched it in during the read.
            read.board.clone()
        } else {
            three_way_merge_board(&prev_board, &read.board, ours)
        };
        // Fold the numbers this read observed onto the *fresh* index's map (so a
        // concurrent `record_board_index_number` isn't dropped), then retain only
        // ids still on the published board.
        let numbers = merge_index_numbers(fresh.as_ref(), &read.numbers, &published_board);
        // What this publish actually changed relative to what was on disk. A tick
        // whose only difference was a CLI write-through already in the file reports
        // zero — the daemon changed nothing, so it emits no line.
        let changed = board_index::board_diff_count(Some(ours), &published_board);
        let mut index =
            BoardIndex::fresh_at(published_board, numbers, fetched_at, read.remaining, read.reset);
        // Record which read path produced this board so `shelbi status` can report
        // whether the reader is on GraphQL or the REST fallback (Phase 3 §6).
        index.rest_fallback = read.rest_fallback;
        // Stamp the repository identity on every publish so a reader can prove the
        // file describes this project's configured repository, and carry the
        // cold-read schedule so the ten-minute floor survives a daemon restart.
        // `schema_version` is already the current constant (set by `fresh_at`).
        index.repo = expected_repo;
        index.last_cold_read = last_cold_read;
        // Record the cadence this tick ran at (or the carried-forward value) so
        // readers derive their staleness threshold from the interval the daemon is
        // actually ticking at, not the project's configured cadence.
        index.interval_secs = interval_secs;
        Ok((index, changed))
    })
    .map_err(|e| anyhow!(e))?;

    if changed > 0 {
        emit_board_refreshed(project, &fetched_at_return, changed, remaining_return);
    }
    // Announce each newly-observed reopen, independent of `changed`. Best-effort,
    // and no store call — the loop stays read-only against GitHub.
    for (id, status) in &reopened_to_emit {
        emit_board_reopened(project, id, status);
    }
    Ok(RefreshOutcome {
        fetched_at: fetched_at_return,
        changed,
    })
}

/// The incremental `since` watermark to read from, or `None` to force a full
/// cold read, derived from a *valid* previous index.
///
/// Returns `None` — a cold read — when the recorded last cold read is missing,
/// unparseable, or older than [`COLD_READ_INTERVAL`], or when the previous
/// `fetched_at` itself doesn't parse (a corrupt timestamp falls back to a safe
/// cold read rather than an error). Otherwise the previous `fetched_at`, so the
/// tick reads only issues touched since it. Deriving the schedule from a
/// timestamp on the published index rather than from loop state means the
/// ten-minute floor holds across the manager's variable cadence and across a
/// daemon restart.
fn cold_read_watermark(prev: &BoardIndex) -> Option<DateTime<Utc>> {
    let last_cold = DateTime::parse_from_rfc3339(prev.last_cold_read.as_deref()?).ok()?;
    let age = Utc::now().signed_duration_since(last_cold.with_timezone(&Utc));
    if age.to_std().is_ok_and(|a| a >= COLD_READ_INTERVAL) {
        return None; // the cold-read schedule is due
    }
    DateTime::parse_from_rfc3339(&prev.fetched_at)
        .ok()
        .map(|dt| dt.with_timezone(&Utc))
}

/// Three-way merge an incremental delta against an index that a CLI write-through
/// may have updated during the network read.
///
/// * `base` — the `prev_board` slice the store folded the delta onto (the board
///   as of the previous tick's watermark).
/// * `theirs` — `read.board`, the store's board with the delta already folded in.
/// * `ours` — the index as re-read under the board-index lock, which embeds any
///   write-through that landed since `base`.
///
/// Per task id: an id whose `ours` entry is unchanged from `base` takes the
/// daemon's outcome (present in `theirs`, or absent — that is how the delta's
/// close still removes a card); an id whose `ours` entry differs from `base`, or
/// that `ours` has and `base` does not, is a CLI write-through that read the
/// issue directly after its own mutation and wins. `theirs` order (the store's
/// canonical column-then-priority order) is preserved, with any write-through-only
/// id appended after.
fn three_way_merge_board(
    base: &[shelbi_state::IssueFile],
    theirs: &[shelbi_state::IssueFile],
    ours: &[shelbi_state::IssueFile],
) -> Vec<shelbi_state::IssueFile> {
    let by_id = |board: &[shelbi_state::IssueFile]| -> HashMap<String, shelbi_state::IssueFile> {
        board
            .iter()
            .map(|f| (f.task.id.clone(), f.clone()))
            .collect()
    };
    let base_by = by_id(base);
    let ours_by = by_id(ours);

    // Whether `ours` left this id exactly as `base` had it (both absent counts as
    // unchanged). An unchanged id defers to the daemon; a changed one is a
    // write-through that wins.
    let ours_unchanged = |id: &str| -> bool {
        match (ours_by.get(id), base_by.get(id)) {
            (None, None) => true,
            (Some(o), Some(b)) => board_index::issue_files_eq(o, b),
            _ => false,
        }
    };

    let mut out = Vec::new();
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // The delta's ids first, in the store's canonical order.
    for t in theirs {
        let id = t.task.id.as_str();
        seen.insert(id);
        if ours_unchanged(id) {
            out.push(t.clone()); // defer to the daemon's outcome
        } else if let Some(o) = ours_by.get(id) {
            out.push(o.clone()); // a write-through changed it: it wins
        }
        // else: `ours` removed it (a write-through drop) — honor that removal.
    }
    // Ids only `ours` carries (not mentioned by the delta). Keep the ones a
    // write-through changed from base; drop the ones unchanged from base whose
    // daemon outcome is "absent" (`theirs` didn't return them).
    for o in ours {
        let id = o.task.id.as_str();
        if seen.contains(id) {
            continue;
        }
        if !ours_unchanged(id) {
            out.push(o.clone());
        }
    }
    out
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

/// Append the `board reopened issue=<id> status=<stale-terminal-status>` line
/// for `project` — a human pulled a terminal issue back open on github.com and
/// the board renders it in `backlog`, so this line is the only signal the
/// orchestrator gets that a reopen happened (decision 7). `issue=` and not
/// `task=` so [`shelbi_state::EventKind::from_body`] classifies it as
/// `Project` (the bucket the `board refreshed=` line also lands in) rather than
/// a task transition. Best-effort: a failed append is logged, never propagated
/// — the index file is the durable artifact, the event is the orchestrator's
/// nudge. Emits no `gh` mutation and no label repair.
fn emit_board_reopened(project: &str, id: &str, status: &str) {
    let body = format!("project={project} board reopened issue={id} status={status}");
    if let Err(e) = shelbi_state::append_external_event(&body) {
        tracing::debug!(project, error = %e, "shelbi daemon: failed to append board-reopened event");
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
            // Resolve the token's GraphQL budget once, then derive both the tick
            // plan and (on a failed tick / pause) the stale index's budget envelope
            // from it — so the reset time the banner shows comes straight from the
            // budget that governed the decision.
            let tier = graphql_tier_for(project, &mut token_keys);
            match governor_plan_from_tier(project, &tier, now) {
                TickPlan::Refresh(interval) => {
                    if paused.remove(project) {
                        tracing::info!(project, "shelbi daemon: board refresh resumed (budget recovered)");
                    }
                    let due = last.get(project).is_none_or(|t| t.elapsed() >= interval);
                    if due {
                        // Stamp the governor's chosen cadence onto the published
                        // index so readers judge freshness against the interval the
                        // daemon is actually ticking at, not the configured one.
                        refresher.tick(project, &tier, interval.as_secs());
                        last.insert(project.clone(), Instant::now());
                    }
                }
                TickPlan::Pause { until } => {
                    // Budget too low (or parked): skip the list read and serve the
                    // last index, now rewritten stale with the reset time so the
                    // banner shows `quota resets HH:MM` immediately rather than
                    // waiting for the file's age to cross the stale threshold.
                    mark_index_stale_from_tier(project, &tier);
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
fn governor_plan_from_tier(project: &str, tier: &BudgetTier, now: i64) -> TickPlan {
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
    gh_budget::tick_plan(tier, &thresholds, now)
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
    use std::sync::atomic::{AtomicBool, AtomicUsize};

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
        /// When set, `refresh_board` returns a rate-limit error instead of a
        /// board — the integration test's stand-in for a live 403/429 from the
        /// GraphQL reader, so the daemon's failed-tick path can be driven.
        fail: Arc<AtomicBool>,
        /// The `(shelbi id, stale terminal status id)` pairs `refresh_board`
        /// surfaces as [`shelbi_state::BoardRead::reopened`], so a test can drive
        /// the daemon's reopened-event path. Empty by default.
        reopened: Vec<(String, String)>,
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
            if self.fail.load(Ordering::SeqCst) {
                // Stand in for a live GraphQL 403/429: a rate-limit error whose
                // stderr carries the phrasing + reset the classifier recognizes.
                return Err(shelbi_core::Error::Command {
                    cmd: "gh api graphql".to_string(),
                    status: "exit status: 1".to_string(),
                    stderr: "HTTP 403: API rate limit exceeded\nx-ratelimit-reset: 1800000000"
                        .to_string(),
                });
            }
            Ok(shelbi_state::BoardRead {
                board: self.list_open()?,
                numbers: Vec::new(),
                remaining: self.budget.0,
                reset: self.budget.1,
                rest_fallback: false,
                reopened: self.reopened.clone(),
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
                fail: Arc::new(AtomicBool::new(false)),
                reopened: Vec::new(),
            },
            opens,
        )
    }

    /// A fake whose `refresh_board` can be flipped to a rate-limit error via the
    /// returned flag — the integration test's stand-in for a live 403/429.
    fn fake_failable(board: Vec<IssueFile>) -> (FakeStore, Arc<AtomicBool>) {
        let fail = Arc::new(AtomicBool::new(false));
        (
            FakeStore {
                board,
                opens: Arc::new(AtomicUsize::new(0)),
                closeds: Arc::new(AtomicUsize::new(0)),
                seen_since: Arc::new(Mutex::new(Vec::new())),
                budget: (Some(4_000), Some(1_800_000_000)),
                fail: Arc::clone(&fail),
                reopened: Vec::new(),
            },
            fail,
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
                fail: Arc::new(AtomicBool::new(false)),
                reopened: Vec::new(),
            },
            closeds,
        )
    }

    /// A fake reporting a fixed set of reopened-with-terminal-label pairs, plus
    /// its `opens` and `closeds` counters, so a test can drive the daemon's
    /// reopened-event path and prove the tick makes no extra backend call.
    fn fake_reopened(
        board: Vec<IssueFile>,
        reopened: Vec<(String, String)>,
    ) -> (FakeStore, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let opens = Arc::new(AtomicUsize::new(0));
        let closeds = Arc::new(AtomicUsize::new(0));
        (
            FakeStore {
                board,
                opens: Arc::clone(&opens),
                closeds: Arc::clone(&closeds),
                seen_since: Arc::new(Mutex::new(Vec::new())),
                budget: (None, None),
                fail: Arc::new(AtomicBool::new(false)),
                reopened,
            },
            opens,
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
                fail: Arc::new(AtomicBool::new(false)),
                reopened: Vec::new(),
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
    fn refresh_emits_one_reopened_line_once_and_makes_no_extra_backend_call() {
        let _iso = IsolatedHome::new("reopened");
        // The reopened issue renders in `backlog` (the status-move mapping);
        // `b` is an ordinary card. The store surfaces the one reopened pair.
        let board = vec![issue("r", "backlog", 0), issue("b", "todo", 0)];
        let (store, opens, closeds) =
            fake_reopened(board, vec![("r".to_string(), "done".to_string())]);

        // First tick: exactly one reopened line, carrying the project, the id and
        // the stale terminal status.
        refresh_with_store("proj", &store, None).unwrap();
        let lines: Vec<String> = events_lines()
            .into_iter()
            .filter(|l| l.contains("board reopened issue="))
            .collect();
        assert_eq!(lines.len(), 1, "one reopened line on the first tick");
        let line = &lines[0];
        assert!(line.contains("project=proj"), "carries the project: {line}");
        assert!(line.contains("issue=r"), "carries the id: {line}");
        assert!(line.contains("status=done"), "carries the stale status: {line}");

        // Second tick over the same board and now-primed index: `r` is on the
        // previous open board, so the dedup suppresses a second line.
        refresh_with_store("proj", &store, None).unwrap();
        let after = events_lines()
            .into_iter()
            .filter(|l| l.contains("board reopened issue="))
            .count();
        assert_eq!(after, 1, "the primed index suppresses a second reopened line");

        // Read-only against the backend: one open read per tick, never a
        // closed-history read.
        assert_eq!(opens.load(Ordering::SeqCst), 2, "one backend open read per tick");
        assert_eq!(closeds.load(Ordering::SeqCst), 0, "no closed-history read");
    }

    #[test]
    fn first_refresh_writes_the_index_and_emits_one_changed_line() {
        let _iso = IsolatedHome::new("first");
        let (store, opens) = fake(vec![issue("a", "todo", 0), issue("b", "review", 0)]);
        let out = refresh_with_store("proj", &store, None).unwrap();
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
    fn a_governed_tick_records_the_interval_it_ran_at() {
        // AC: the published index records the refresh interval the daemon was
        // actually ticking at when it wrote the file. A governed tick passes the
        // governor's chosen cadence; it lands on the index verbatim.
        let _iso = IsolatedHome::new("records-interval");
        let (store, _) = fake(vec![issue("a", "todo", 0)]);
        refresh_with_store("proj", &store, Some(120)).unwrap();

        let idx = board_index::read_board_index("proj").expect("index written");
        assert_eq!(
            idx.interval_secs,
            Some(120),
            "the governed cadence is stamped on the published index"
        );
    }

    #[test]
    fn a_manual_refresh_carries_the_recorded_interval_forward() {
        // AC: a `refresh-board` request holds no governor decision (interval None),
        // so it must leave the recorded cadence at the value the last governed tick
        // wrote rather than resetting it to the configured default. Without the
        // carry-forward the throttled hub's flicker would return until the next
        // governed tick.
        let _iso = IsolatedHome::new("carry-interval");
        let (store, _) = fake(vec![issue("a", "todo", 0)]);
        // A governed tick throttled to 120s.
        refresh_with_store("proj", &store, Some(120)).unwrap();
        // A manual refresh with no governor decision.
        refresh_with_store("proj", &store, None).unwrap();

        let idx = board_index::read_board_index("proj").expect("index written");
        assert_eq!(
            idx.interval_secs,
            Some(120),
            "a manual refresh preserves the last governed cadence, not the configured one"
        );
    }

    #[test]
    fn a_quiet_tick_rewrites_the_file_but_emits_no_line() {
        let _iso = IsolatedHome::new("quiet");
        let (store, _) = fake(vec![issue("a", "todo", 0)]);
        // Prime the index.
        let first = refresh_with_store("proj", &store, None).unwrap();
        assert_eq!(first.changed, 1);

        // Second tick over an unchanged board: no new event, but fetched_at
        // advances so the file stays a live freshness signal.
        std::thread::sleep(Duration::from_millis(5));
        let second = refresh_with_store("proj", &store, None).unwrap();
        assert_eq!(second.changed, 0, "an unchanged board reports no change");
        assert_ne!(
            second.fetched_at, first.fetched_at,
            "fetched_at advances every tick even when quiet"
        );

        // A third quiet tick, then assert still exactly one refreshed line
        // total (only the first, changed tick emitted).
        let third = refresh_with_store("proj", &store, None).unwrap();
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
        refresh_with_store("proj", &store, None).unwrap();

        // Now the board moves: `a` changes column. A fresh store models the
        // next tick reading the moved board.
        let (moved, _) = fake(vec![issue("a", "in_progress", 0)]);
        let out = refresh_with_store("proj", &moved, None).unwrap();
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
        let first = refresh_with_store("proj", &store, None).unwrap();
        // Second tick: the prior index's `fetched_at` becomes the incremental
        // watermark.
        let second = refresh_with_store("proj", &store, None).unwrap();

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
    fn an_overdue_cold_read_timestamp_forces_a_cold_tick() {
        // A long-running daemon between cold reads: `fetched_at` is recent but the
        // recorded last cold read is older than the ten-minute floor, so the next
        // tick reads cold (since = None) to reconcile any vanished issue.
        let _iso = IsolatedHome::new("cold-overdue");
        let (store, seen_since) = fake_with_budget(vec![issue("a", "todo", 0)], (None, None));

        let mut seed = BoardIndex::fresh(vec![issue("a", "todo", 0)]);
        let now = Utc::now();
        seed.fetched_at = (now - chrono::Duration::seconds(30)).to_rfc3339();
        seed.last_cold_read = Some((now - chrono::Duration::minutes(11)).to_rfc3339());
        shelbi_state::write_board_index("proj", &seed).unwrap();

        refresh_with_store("proj", &store, None).unwrap();

        let seen = seen_since.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert!(seen[0].is_none(), "an overdue cold-read timestamp forces since=None");
        drop(seen);
        // The cold tick reset the schedule to its own fresh `fetched_at`.
        let idx = board_index::read_board_index("proj").unwrap();
        let cold = DateTime::parse_from_rfc3339(idx.last_cold_read.as_deref().unwrap()).unwrap();
        assert!(
            Utc::now().signed_duration_since(cold.with_timezone(&Utc)) < chrono::Duration::minutes(1),
            "the cold read reset the schedule"
        );
    }

    #[test]
    fn a_recent_cold_read_timestamp_ticks_incrementally_and_carries_the_schedule() {
        // The schedule lives on the published index, so a freshly constructed
        // refresher reading a recent cold-read timestamp ticks incrementally
        // (since = the previous fetched_at) and carries the cold-read timestamp
        // forward unchanged rather than resetting it.
        let _iso = IsolatedHome::new("cold-recent");
        let (store, seen_since) = fake_with_budget(vec![issue("a", "todo", 0)], (None, None));

        let mut seed = BoardIndex::fresh(vec![issue("a", "todo", 0)]);
        let now = Utc::now();
        seed.fetched_at = (now - chrono::Duration::seconds(30)).to_rfc3339();
        let cold_read_ts = (now - chrono::Duration::minutes(2)).to_rfc3339();
        seed.last_cold_read = Some(cold_read_ts.clone());
        let expected_watermark = DateTime::parse_from_rfc3339(&seed.fetched_at)
            .unwrap()
            .with_timezone(&Utc);
        shelbi_state::write_board_index("proj", &seed).unwrap();

        refresh_with_store("proj", &store, None).unwrap();

        let seen = seen_since.lock().unwrap();
        assert_eq!(
            seen[0],
            Some(expected_watermark),
            "a recent cold read ⇒ incremental from the previous fetched_at"
        );
        drop(seen);
        let idx = board_index::read_board_index("proj").unwrap();
        assert_eq!(
            idx.last_cold_read.as_deref(),
            Some(cold_read_ts.as_str()),
            "an incremental tick carries the cold-read schedule forward unchanged"
        );
    }

    #[test]
    fn a_mismatched_index_forces_a_cold_read_and_republishes_the_identity() {
        // A retargeted project: the on-disk index is stamped for a different
        // repository. The daemon treats it as no prior index (cold read) and
        // republishes with the configured repository's identity, carrying none of
        // the other repository's cards forward.
        let _iso = IsolatedHome::new("mismatch-cold");
        register_github_project("proj"); // configured for owner/repo
        let (store, seen_since) = fake_with_budget(vec![issue("a", "todo", 0)], (None, None));

        let mut seed = BoardIndex::fresh(vec![issue("ghost-from-other-repo", "todo", 0)]);
        seed.repo = Some(shelbi_state::github_board_repo("other/repo"));
        shelbi_state::write_board_index("proj", &seed).unwrap();

        refresh_with_store("proj", &store, None).unwrap();

        assert!(
            seen_since.lock().unwrap()[0].is_none(),
            "a mismatched prior index forces a cold read"
        );
        let idx = board_index::read_board_index("proj").unwrap();
        assert_eq!(
            idx.repo.as_deref(),
            Some(shelbi_state::github_board_repo("owner/repo").as_str()),
            "the republished index carries the configured repository"
        );
        assert!(
            idx.board.iter().all(|f| f.task.id != "ghost-from-other-repo"),
            "the other repository's card is not carried forward"
        );
        assert!(idx.board.iter().any(|f| f.task.id == "a"));
    }

    #[test]
    fn a_scheduled_cold_read_drops_a_ghost_absent_from_the_full_board() {
        // A hard delete or a transfer out: an issue on the published board that a
        // full cold read no longer returns is gone from both the board and the
        // numbers map after that cold read — the reconcile an incremental delta
        // (add/update only) can never perform.
        let _iso = IsolatedHome::new("ghost");
        let mut seed =
            BoardIndex::fresh(vec![issue("keep", "todo", 0), issue("ghost", "review", 0)]);
        seed.numbers = [("keep".to_string(), 1), ("ghost".to_string(), 9)]
            .into_iter()
            .collect();
        // Overdue schedule so the next tick is cold.
        seed.last_cold_read = Some((Utc::now() - chrono::Duration::minutes(11)).to_rfc3339());
        shelbi_state::write_board_index("proj", &seed).unwrap();

        // The cold read returns only `keep`; `ghost` vanished from the repository.
        let (store, seen_since) = fake_with_budget(vec![issue("keep", "todo", 0)], (None, None));
        refresh_with_store("proj", &store, None).unwrap();
        assert!(seen_since.lock().unwrap()[0].is_none(), "the tick was cold");

        let idx = board_index::read_board_index("proj").unwrap();
        assert!(
            idx.board.iter().all(|f| f.task.id != "ghost"),
            "the ghost is gone from the published board"
        );
        assert_eq!(idx.numbers.get("ghost"), None, "and from the numbers map");
        assert!(idx.board.iter().any(|f| f.task.id == "keep"), "the live card survives");
    }

    #[test]
    fn a_surfaced_budget_lands_in_the_index_and_the_events_line() {
        let _iso = IsolatedHome::new("budget");
        let (store, _) = fake_with_budget(vec![issue("a", "todo", 0)], (Some(4989), Some(1_800_000_000)));

        refresh_with_store("proj", &store, None).unwrap();

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
        refresh_with_store("proj", &store, None).unwrap();
        refresh_with_store("proj", &store, None).unwrap();
        refresh_with_store("proj", &store, None).unwrap();
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

    // --- Phase 3 integration: 403 → 429 → park → recovery ---------------------

    /// Register a `github`-backed project under the isolated home so
    /// `read_board_report` / `load_project` resolve a remote backend whose board
    /// the daemon owns (read from `board-index.json`, never a real `gh`).
    fn register_github_project(name: &str) {
        let projects = shelbi_state::shelbi_home().unwrap().join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join(format!("{name}.yaml")),
            format!(
                "name: {name}\n\
repo: /tmp/{name}\n\
default_branch: main\n\
orchestrator:\n\
\x20 runner: claude\n\
agent_runners:\n\
\x20 claude:\n\
\x20\x20\x20 command: claude\n\
\x20\x20\x20 flags: []\n\
machines:\n\
\x20 - name: local\n\
\x20\x20\x20 kind: local\n\
\x20\x20\x20 work_dir: /tmp/{name}\n\
workspaces: []\n\
issue_tracker:\n\
\x20 backend: github\n\
\x20 refresh_secs: 30\n\
\x20 github:\n\
\x20\x20\x20 repo: owner/repo\n"
            ),
        )
        .unwrap();
    }

    fn tier(remaining: Option<i64>, reset_at: Option<i64>, parked_until: Option<i64>) -> BudgetTier {
        BudgetTier {
            remaining,
            reset_at,
            parked_until,
        }
    }

    /// Whether any events.log line looks like a destructive poller action — a
    /// reap, an orphan action, a marker clear, or an idle mark. Phase 3's rule is
    /// that none of these may fire while the board is stale/failed.
    fn any_destructive_events() -> bool {
        events_lines().iter().any(|l| {
            l.contains("reaped")
                || l.contains("orphan")
                || l.contains("marker")
                || l.contains("mark_idle")
                || l.contains("idle=")
        })
    }

    #[test]
    fn board_survives_403_429_park_and_recovery_with_no_destructive_action() {
        // The whole loop through an outage (plan Phase 3 acceptance): a good tick
        // is warm; a 403 then a 429 leave the board *rendering* (stale, with the
        // reset time) rather than empty; while parked the governor pauses the read
        // (no backend hammering); and once the budget recovers a tick goes warm
        // again. Throughout, the board never reads `Warm` during the outage — the
        // single condition every destructive poller path (reap / orphan / marker
        // clear / mark idle) requires — so none of them can fire, and no such line
        // is ever written.
        let _iso = IsolatedHome::new("outage");
        register_github_project("proj");
        let reset = 1_800_000_000i64; // a fixed future epoch, so the banner has an HH:MM
        let now = 1_700_000_000i64; // well before the reset → the park is live

        let (store, fail) =
            fake_failable(vec![issue("a", "review", 0), issue("b", "in-progress", 0)]);
        let healthy = tier(Some(4_000), Some(reset), None);

        // (1) WARM: a good tick publishes the index; the board reads Warm with a
        // plain `board …` banner and no stale/quota clause.
        handle_tick_result("proj", refresh_with_store("proj", &store, None), &healthy);
        let report = shelbi_state::read_board_report("proj").unwrap();
        assert!(
            matches!(report.state, shelbi_state::BoardState::Warm(_)),
            "a good tick is warm"
        );
        let banner = report.freshness.banner().expect("remote board has a banner");
        assert!(banner.starts_with("board "), "banner: {banner}");
        assert!(!banner.contains("stale"), "warm banner is not stale: {banner}");

        // (2) 403: the reader errors; the daemon rewrites the index STALE, carrying
        // the reset from the (now parked) budget, so the board keeps rendering with
        // `quota resets HH:MM` instead of collapsing.
        fail.store(true, Ordering::SeqCst);
        let parked = tier(Some(50), Some(reset), Some(reset));
        handle_tick_result("proj", refresh_with_store("proj", &store, None), &parked);
        let report = shelbi_state::read_board_report("proj").unwrap();
        match &report.state {
            shelbi_state::BoardState::Stale(board) => {
                assert_eq!(board.len(), 2, "the last good board is carried forward")
            }
            other => panic!("expected Stale after a 403, got {other:?}"),
        }
        assert_eq!(report.freshness.reset, Some(reset), "reset carried into the index");
        let banner = report.freshness.banner().unwrap();
        assert!(banner.contains("stale"), "banner marks stale: {banner}");
        assert!(banner.contains("quota resets"), "banner names the reset: {banner}");
        // The thin `read_board` — the one every destructive poller path gates on —
        // is non-Warm, so a reaper would bail.
        assert!(
            !matches!(shelbi_state::read_board("proj").unwrap(), shelbi_state::BoardState::Warm(_)),
            "a stale board must never read Warm (the reap gate)"
        );

        // (3) 429 → PARK: with the budget parked, the governor pauses this tick —
        // the loop skips the backend read entirely (no hammering) and only refreshes
        // the stale marker.
        assert!(
            matches!(
                governor_plan_from_tier("proj", &parked, now),
                TickPlan::Pause { until } if until == reset
            ),
            "a parked budget pauses the refresh until reset"
        );
        mark_index_stale_from_tier("proj", &parked);
        assert!(
            !matches!(shelbi_state::read_board("proj").unwrap(), shelbi_state::BoardState::Warm(_)),
            "still non-Warm while parked"
        );

        // (4) RECOVERY: the window resets, the budget refills, the reader succeeds
        // again → the governor refreshes and the index goes warm, banner clears.
        fail.store(false, Ordering::SeqCst);
        assert!(
            matches!(governor_plan_from_tier("proj", &healthy, now), TickPlan::Refresh(_)),
            "a healthy budget refreshes"
        );
        handle_tick_result("proj", refresh_with_store("proj", &store, None), &healthy);
        let report = shelbi_state::read_board_report("proj").unwrap();
        assert!(
            matches!(report.state, shelbi_state::BoardState::Warm(_)),
            "recovery goes warm again"
        );
        assert!(
            !report.freshness.banner().unwrap().contains("stale"),
            "the stale banner clears on recovery"
        );

        // Across the whole outage, not one destructive poller line was written.
        assert!(
            !any_destructive_events(),
            "no reap / orphan / marker-clear / idle line may fire during the outage: {:?}",
            events_lines(),
        );
    }

    #[test]
    fn a_failing_tick_warns_and_records_once_per_episode() {
        // A refresh that keeps failing (a token that won't resolve, a 403) must
        // surface once per *episode*, not once per tick: exactly one
        // `board refresh-failed` events line (the observable twin of the `warn`)
        // and a persisted error the status/doctor section reads. Recovery clears
        // it; a fresh failure after recovery is a new episode and warns again.
        let _iso = IsolatedHome::new("fail-episode");
        register_github_project("proj");
        let reset = 1_800_000_000i64;
        let (store, fail) = fake_failable(vec![issue("a", "review", 0)]);
        let healthy = tier(Some(4_000), Some(reset), None);

        // Prime a good index — a healthy tick records no error.
        handle_tick_result("proj", refresh_with_store("proj", &store, None), &healthy);
        assert!(
            shelbi_state::read_board_refresh_error("proj").is_none(),
            "a healthy tick leaves no recorded error"
        );

        // Three consecutive failing ticks = one episode.
        fail.store(true, Ordering::SeqCst);
        for _ in 0..3 {
            handle_tick_result("proj", refresh_with_store("proj", &store, None), &healthy);
        }
        let err = shelbi_state::read_board_refresh_error("proj").expect("error recorded");
        assert!(!err.error.is_empty(), "error text is recorded: {err:?}");

        let failed: Vec<_> = events_lines()
            .into_iter()
            .filter(|l| l.contains("board refresh-failed"))
            .collect();
        assert_eq!(
            failed.len(),
            1,
            "one refresh-failed line per episode, not per tick: {failed:?}"
        );
        assert!(failed[0].contains("project=proj"), "{}", failed[0]);
        assert!(failed[0].contains("error="), "carries the error text: {}", failed[0]);

        // Recovery clears the recorded error.
        fail.store(false, Ordering::SeqCst);
        handle_tick_result("proj", refresh_with_store("proj", &store, None), &healthy);
        assert!(
            shelbi_state::read_board_refresh_error("proj").is_none(),
            "a successful tick clears the recorded error"
        );

        // A new failure after recovery is a new episode → warns again.
        fail.store(true, Ordering::SeqCst);
        handle_tick_result("proj", refresh_with_store("proj", &store, None), &healthy);
        let failed_again = events_lines()
            .into_iter()
            .filter(|l| l.contains("board refresh-failed"))
            .count();
        assert_eq!(failed_again, 2, "a fresh episode after recovery warns again");
    }

    // --- lock reconciliation: write-through during the board read ------------

    /// A store whose `refresh_board` runs an arbitrary hook **before** returning
    /// its board — the deterministic stand-in for a CLI write-through landing
    /// inside the network window (`T2`). The hook runs with no board-index lock
    /// held (`refresh_with_store` takes the lock only *after* the read returns),
    /// so it can splice into the index exactly as a real concurrent
    /// `shelbi issue move` would.
    struct HookStore {
        board: Vec<IssueFile>,
        numbers: Vec<(String, i64)>,
        rest_fallback: bool,
        on_refresh: Box<dyn Fn()>,
    }

    impl IssueStore for HookStore {
        fn list(&self) -> CoreResult<Vec<IssueFile>> {
            Ok(self.board.clone())
        }
        fn list_open(&self) -> CoreResult<Vec<IssueFile>> {
            Ok(self.board.clone())
        }
        fn refresh_board(
            &self,
            _since: Option<DateTime<Utc>>,
            _previous: &[IssueFile],
        ) -> CoreResult<shelbi_state::BoardRead> {
            (self.on_refresh)();
            Ok(shelbi_state::BoardRead {
                board: self.board.clone(),
                numbers: self.numbers.clone(),
                remaining: None,
                reset: None,
                rest_fallback: self.rest_fallback,
                reopened: Vec::new(),
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

    #[test]
    fn a_write_through_during_the_board_read_survives_and_holds_no_lock() {
        // AC: a fake store performs a move-style write-through *while*
        // `refresh_board` is in flight; the published index still contains the
        // moved card in its new column and still carries its number. The
        // write-through runs on another thread and must complete while the read is
        // stalled — proving no board-index lock is held across the read (otherwise
        // the write-through would deadlock on the lock the daemon would be holding,
        // and this test would hang rather than pass).
        use std::sync::atomic::AtomicUsize;
        use std::sync::mpsc;
        let _iso = IsolatedHome::new("refresh-race");

        let mut seed = BoardIndex::fresh(vec![issue("a", "todo", 0), issue("b", "review", 0)]);
        seed.numbers = [("a".to_string(), 1), ("b".to_string(), 2)]
            .into_iter()
            .collect();
        shelbi_state::write_board_index("proj", &seed).unwrap();

        let (started_tx, started_rx) = mpsc::channel::<()>();
        let (proceed_tx, proceed_rx) = mpsc::channel::<()>();
        let seq = Arc::new(AtomicUsize::new(0));
        let read_order = Arc::new(AtomicUsize::new(usize::MAX));

        // The incremental delta didn't touch the operator's card: it returns A and
        // B unchanged from the previous tick's watermark.
        let seq_hook = Arc::clone(&seq);
        let read_order_hook = Arc::clone(&read_order);
        let store = HookStore {
            board: vec![issue("a", "todo", 0), issue("b", "review", 0)],
            numbers: vec![("a".to_string(), 1), ("b".to_string(), 2)],
            rest_fallback: false,
            on_refresh: Box::new(move || {
                started_tx.send(()).unwrap(); // the read is now in flight
                proceed_rx.recv().unwrap(); // stall until the write-through is done
                read_order_hook.store(seq_hook.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
            }),
        };

        // The concurrent CLI move: A → review, number 1, while the read is stalled.
        let seq_wt = Arc::clone(&seq);
        let wt_order = Arc::new(AtomicUsize::new(usize::MAX));
        let wt_order_thr = Arc::clone(&wt_order);
        let writer = std::thread::spawn(move || {
            started_rx.recv().unwrap();
            board_index::patch_board_index_issue_with_number(
                "proj",
                &issue("a", "review", 5),
                Some(1),
            )
            .unwrap();
            wt_order_thr.store(seq_wt.fetch_add(1, Ordering::SeqCst), Ordering::SeqCst);
            proceed_tx.send(()).unwrap(); // let the stalled read return
        });

        refresh_with_store("proj", &store, None).unwrap();
        writer.join().unwrap();

        assert_eq!(
            wt_order.load(Ordering::SeqCst),
            0,
            "the write-through completed while the board read was still stalled"
        );
        assert_eq!(
            read_order.load(Ordering::SeqCst),
            1,
            "the board read returned only after the write-through finished"
        );

        let idx = shelbi_state::read_board_index("proj").unwrap();
        let a = idx.board.iter().find(|f| f.task.id == "a").expect("a present");
        assert_eq!(a.task.column.as_str(), "review", "the write-through's move won");
        assert_eq!(idx.numbers.get("a"), Some(&1), "its number is retained");
    }

    #[test]
    fn an_authoritative_read_drops_a_write_through_the_full_board_omits() {
        // AC: a card an authoritative full read (here the REST fallback, on an
        // incremental tick) no longer returns is dropped from the board *and* the
        // numbers map, even though a write-through patched it in mid-read.
        let _iso = IsolatedHome::new("authoritative-drop");
        let mut seed = BoardIndex::fresh(vec![issue("a", "todo", 0)]);
        seed.numbers = [("a".to_string(), 1)].into_iter().collect();
        shelbi_state::write_board_index("proj", &seed).unwrap();

        let store = HookStore {
            board: vec![issue("a", "todo", 0)], // the authoritative open set: no `c`
            numbers: vec![("a".to_string(), 1)],
            rest_fallback: true, // → authoritative, publish wholesale
            on_refresh: Box::new(|| {
                board_index::patch_board_index_issue_with_number(
                    "proj",
                    &issue("c", "review", 0),
                    Some(9),
                )
                .unwrap();
            }),
        };
        refresh_with_store("proj", &store, None).unwrap();

        let idx = shelbi_state::read_board_index("proj").unwrap();
        assert!(
            idx.board.iter().all(|f| f.task.id != "c"),
            "an id the full read omits is not resurrected"
        );
        assert_eq!(idx.numbers.get("c"), None, "and it leaves the numbers map");
        assert!(idx.board.iter().any(|f| f.task.id == "a"), "the full board is published");
    }

    #[test]
    fn an_incremental_tick_keeps_untouched_and_mid_read_write_through_cards() {
        // AC: a card an incremental delta did not mention survives — whether it was
        // untouched since the previous tick (`b`) or patched in by a write-through
        // during the call (`c`) — while the delta's own change (`a` moved) applies.
        let _iso = IsolatedHome::new("incremental-survive");
        let mut seed = BoardIndex::fresh(vec![issue("a", "todo", 0), issue("b", "review", 0)]);
        seed.numbers = [("a".to_string(), 1), ("b".to_string(), 2)]
            .into_iter()
            .collect();
        shelbi_state::write_board_index("proj", &seed).unwrap();

        let store = HookStore {
            board: vec![issue("a", "in_progress", 0), issue("b", "review", 0)],
            numbers: vec![("a".to_string(), 1)],
            rest_fallback: false, // incremental → three-way merge
            on_refresh: Box::new(|| {
                board_index::patch_board_index_issue_with_number(
                    "proj",
                    &issue("c", "todo", 0),
                    Some(3),
                )
                .unwrap();
            }),
        };
        refresh_with_store("proj", &store, None).unwrap();

        let idx = shelbi_state::read_board_index("proj").unwrap();
        let by_id = |id: &str| idx.board.iter().find(|f| f.task.id == id);
        assert_eq!(
            by_id("a").expect("a present").task.column.as_str(),
            "in-progress",
            "the delta's move applied to the untouched card"
        );
        assert!(by_id("b").is_some(), "an untouched card the delta didn't mention survives");
        let c = by_id("c").expect("the mid-read write-through survives");
        assert_eq!(c.task.column.as_str(), "todo");
        assert_eq!(idx.numbers.get("c"), Some(&3), "its number is kept");
    }

    #[test]
    fn three_way_merge_defers_unchanged_ids_and_lets_write_throughs_win() {
        // The merge rule in isolation: `base` = the previous tick's board; `theirs`
        // = the store's delta-folded board; `ours` = the index a write-through
        // updated. Unchanged ids take the daemon's outcome (present or absent); a
        // changed/added id is the write-through and wins.
        let base = vec![issue("keep", "todo", 0), issue("closed", "review", 0)];
        // The delta moved `keep` and closed `closed` (dropped from theirs).
        let theirs = vec![issue("keep", "done_review", 0)];
        // A write-through moved `keep` differently and added `new`; it left
        // `closed` exactly as base had it.
        let ours = vec![
            issue("keep", "in_progress", 0),
            issue("closed", "review", 0),
            issue("new", "todo", 0),
        ];
        let merged = three_way_merge_board(&base, &theirs, &ours);
        let col = |id: &str| {
            merged
                .iter()
                .find(|f| f.task.id == id)
                .map(|f| f.task.column.as_str().to_string())
        };
        assert_eq!(
            col("keep").as_deref(),
            Some("in-progress"),
            "a write-through that changed an id from base wins over the delta"
        );
        assert_eq!(
            col("closed"),
            None,
            "an id unchanged in `ours` takes the daemon's outcome — the delta's close"
        );
        assert_eq!(
            col("new").as_deref(),
            Some("todo"),
            "a write-through-only id the delta never mentioned survives"
        );
    }
}
