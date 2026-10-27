//! The activity feed's shared model: `events.log` parsing, task-title
//! resolution, the feed's filters and paging, and the rows both clients paint.
//!
//! The feed is a human-friendly view of `~/.shelbi/events.log`, the same
//! append-only stream `shelbi events tail` consumes, reformatted as a
//! date-bucketed, newest-first list: who started what, who finished what, who
//! is idle, who is waiting. Each row reads as a sentence: a tinted `[name]`
//! identity chip, a dim verb and the work's real task title, and a
//! category-tinted state pill plus a relative time.
//!
//! Everything here is toolkit-free. [`build_feed`] turns parsed [`Event`]s into
//! [`FeedItem`]s (day headers and [`FeedRow`]s carrying [`Tone`] token names,
//! never colors); the TUI's `activity.rs` lays a row out in terminal cells and
//! the desktop's `views/activity.rs` lays the same row out in gpui, so the two
//! show the same rows, in the same order, with the same glyphs.
//!
//! The feed is built from three sources:
//!
//! - `~/.shelbi/events.log`, the source of truth (append-only, RFC3339
//!   timestamped one-line records). [`read_activity_data`] reads all of it
//!   (the TUI); [`read_activity_window`] reads only its newest lines, so the
//!   desktop pages older events in on demand.
//! - The task store, for each row's title, branch and assignee: resolved
//!   incrementally and cached by id ([`ActivityMetaCache`]), never on the
//!   render path.
//! - The event list itself, walked backwards to pair an `in_progress ->
//!   review` event with its prior `* -> in_progress` so the row can show
//!   "took 18m".

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Datelike, Local, NaiveDate, Utc};
use shelbi_core::{Column, StatusCategory, DEFAULT_WORKFLOW_NAME};
use shelbi_state::{events_log_path, WorkspaceState};

use crate::theme::Tone;

/// How many `events.log` lines one "load older" step adds to a windowed read
/// ([`read_activity_window`]), and the window a feed starts with.
pub const ACTIVITY_PAGE: usize = 500;

/// Per-workspace tint. Identity in the feed is carried by color: the chip is
/// painted in the workspace's tint. The mapping is fixed to the six phonetic
/// workspace names; every other name falls back to gray, so a project that
/// names workspaces `frontend` / `backend` still renders, just without a
/// unique tint.
pub fn agent_tone(name: &str) -> Tone {
    match name {
        "alpha" => Tone::Cyan,
        "bravo" => Tone::Magenta,
        "charlie" => Tone::Green,
        "delta" => Tone::Yellow,
        "echo" => Tone::Blue,
        "foxtrot" => Tone::LightRed,
        _ => Tone::Gray,
    }
}

/// Per-category tint for a state pill. Mirrors the canonical 5-status palette
/// the kanban board uses, so "in progress" reads the same active-yellow here as
/// the In Progress column does there.
pub fn category_tone(category: StatusCategory) -> Tone {
    match category {
        StatusCategory::Backlog => Tone::Gray,
        StatusCategory::Ready => Tone::Blue,
        StatusCategory::Active => Tone::Yellow,
        StatusCategory::Handoff => Tone::Magenta,
        StatusCategory::Done => Tone::Green,
        StatusCategory::Archived => Tone::DarkGray,
    }
}

// ---------------------------------------------------------------------------
// Events and title resolution
// ---------------------------------------------------------------------------

/// One parsed line out of `events.log`. The raw line is kept on every
/// variant so the renderer can fall back to the original text if a
/// later format change introduces a shape we don't recognize — nothing
/// in the file should ever be silently dropped.
#[derive(Debug, Clone)]
pub enum Event {
    /// A task transition. Carries the workflow-aware fields documented in
    /// `Plans/workflows.md` §10 (`workflow=`, `from_category=`,
    /// `to_category=`); pre-workflow lines have those filled in by the
    /// back-compat parser — `workflow` defaults to
    /// [`DEFAULT_WORKFLOW_NAME`] and the categories are derived from the
    /// canonical 5-status column-to-category map.
    Issue {
        ts: DateTime<Utc>,
        /// The `project=` scope the line carried, if any.
        project: Option<String>,
        id: String,
        workflow: String,
        from: Column,
        to: Column,
        reason: String,
        /// Agent name embedded in the `reason=` field as `_agent=<name>`
        /// — set by `shelbi task start` when it spawns a workspace with a
        /// specific agent loaded. `None` for events without the segment
        /// (older lines from before this field was emitted, plus
        /// transitions that don't spawn an agent).
        agent: Option<String>,
        from_category: StatusCategory,
        to_category: StatusCategory,
        raw: String,
    },
    Workspace {
        ts: DateTime<Utc>,
        /// The `project=` scope the line carried, if any.
        project: Option<String>,
        name: String,
        prev: Option<WorkspaceState>,
        new: WorkspaceState,
        raw: String,
    },
    /// A `shelbi zen dry-run` preview decision — recorded but never
    /// acted on. Rendered with a distinct visual tag so the user can
    /// tell at a glance these rows reflect what Zen *would* have done,
    /// not a real state change.
    ZenDryRun {
        ts: DateTime<Utc>,
        /// The `project=` scope the line carried, if any.
        project: Option<String>,
        task_id: String,
        action: String,
        detail: String,
        raw: String,
    },
    /// Hub-poller heartbeat — `<ts> project=<name> heartbeat`. The
    /// orchestrator follows the events log and uses these as a
    /// guaranteed recurring wake-up when the board is otherwise quiet,
    /// but a human reading the activity feed shouldn't see them — they
    /// produce one row every few minutes saying nothing happened. We
    /// keep the parsed variant so the raw line survives any future
    /// "show internal events" toggle, then filter it out at render time.
    Heartbeat {
        ts: DateTime<Utc>,
        project: String,
        raw: String,
    },
    /// A classified system / infra event: everything that isn't a task
    /// transition, workspace-state ping, zen dry-run, or heartbeat. Covers
    /// the `ssh`, `dispatch`, `rebase`, `worktree-detach`, `closed`,
    /// `handoff`, `send`, `message`, `mode=zen`, `supervision`, and
    /// pane-death line kinds — each parsed into structured fields so the
    /// renderer can build a human row (never raw wire syntax) and so
    /// consecutive near-duplicates can be folded into one summary row.
    System(SystemEvent),
    /// Recognized timestamp but the rest doesn't match any known shape.
    /// Genuine last resort — rendered as a cleaned-up row (the leading ISO
    /// timestamp stripped, relative time on the right) so nothing vanishes
    /// and nothing shows raw wire syntax with a full timestamp.
    Unknown {
        ts: Option<DateTime<Utc>>,
        raw: String,
    },
}

/// The classified kind of a [`SystemEvent`]. Drives both the renderer's
/// per-kind verb/glyph choice and — together with the event's target and
/// status — the fold key that collapses a flapping infra loop into one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SystemKind {
    /// `ssh reverse-forward host=… status=…` — a reverse-tunnel attempt.
    Ssh,
    /// `dispatch task=… workspace=… status=…` — a task handed to a worker.
    Dispatch,
    /// `rebase task=… workspace=… status=…` — a branch rebased onto base.
    Rebase,
    /// `push task=… workspace=… branch=… status=up-to-date|pushed|force-pushed|failed|blocked detail=…`
    /// — the handoff push of a workspace branch to origin. The `blocked` /
    /// `failed` cases are the ones that stall a task in-progress, so the feed
    /// surfaces them in a warning tint instead of leaving them unparsed.
    Push,
    /// `worktree-detach task=… workspace=… status=…` — a worktree released.
    WorktreeDetach,
    /// `merge task=… workspace=… base=… status=ok|failed detail=…` — an
    /// accept-merge landing (`ok`) or refusal (`failed`, e.g. a non-mergeable
    /// PR). The failing case is exactly the one the review panel now surfaces.
    Merge,
    /// `project=… closed reason=…` — a project torn down.
    Closed,
    /// `project=… handoff outcome=…` — an orchestrator handoff attempt.
    Handoff,
    /// `send project=… workspace=… status=…` — a `shelbi send` delivery.
    Send,
    /// `message=… task=… push=…|ack=…` — a worker message push / ack.
    Message,
    /// `[project=…] task=… ci pr=… conclusion=success|failure` — a CI
    /// pass/fail verdict for an in-review PR from the hub's scoped CI poll.
    Ci,
    /// `project=… mode=zen <prev> -> <new>` — a Zen Mode toggle.
    Mode,
    /// `project=… supervision=…` — a pane restart / crash-loop give-up.
    Supervision,
    /// `[project=…] workspace=… pane_alive=<bool>` — a pane liveness change.
    PaneDeath,
    /// A project-scoped verb we don't model individually — still rendered
    /// as a clean human row (verb + humanized detail), never raw.
    Other,
}

/// One parsed system / infra event. Every field the renderer needs is
/// lifted out of the wire line so it never has to fall back to the raw
/// string. `target` and `status` are the stable identity of the event
/// (host / task / workspace and the status token) and form the fold key
/// together with `kind`; `detail` is the volatile tail and is excluded
/// from that key so a flapping loop still collapses when only its detail
/// text jitters.
#[derive(Debug, Clone)]
pub struct SystemEvent {
    pub ts: DateTime<Utc>,
    pub kind: SystemKind,
    /// Project scope, when the line carried a `project=<name>` prefix.
    pub project: Option<String>,
    /// The primary subject — host (ssh), task id (dispatch/rebase/detach/
    /// message), workspace (send/pane), or project (closed/handoff/mode).
    pub target: Option<String>,
    /// A short status / outcome token (`failed`, `established`, `ok`,
    /// `up-to-date`, `written`, …) when the line carries one.
    pub status: Option<String>,
    /// The dim detail tail — a humanized fragment (never raw `key=value`),
    /// or `None`. Excluded from the fold key.
    pub detail: Option<String>,
    pub raw: String,
}

impl SystemEvent {
    /// The identity used to fold consecutive near-duplicates: the triple of
    /// kind, target, and status, deliberately ignoring the timestamp and the
    /// volatile detail tail (per the task spec's dedup definition).
    pub fn fold_key(&self) -> (SystemKind, Option<&str>, Option<&str>) {
        (self.kind, self.target.as_deref(), self.status.as_deref())
    }
}

impl Event {
    pub fn ts(&self) -> Option<DateTime<Utc>> {
        match self {
            Event::Issue { ts, .. }
            | Event::Workspace { ts, .. }
            | Event::ZenDryRun { ts, .. }
            | Event::Heartbeat { ts, .. } => Some(*ts),
            Event::System(sys) => Some(sys.ts),
            Event::Unknown { ts, .. } => *ts,
        }
    }
}

/// Cached subset of an issue's frontmatter that the feed renders.
#[derive(Debug, Clone)]
pub struct TaskMeta {
    pub title: String,
    pub branch: Option<String>,
    pub assigned_to: Option<String>,
}

/// One entry in the feed's task-metadata cache.
///
/// Invalidation is backend-aware. For the filesystem board, `mtime` lets us
/// re-read lazily only when the task file changes — exact and free. A remote
/// backend (e.g. `issue_tracker.backend: github`) has no local task file, so
/// `mtime` is always `None`; a pure mtime check would then read `None == None`
/// and never refresh, freezing a remote issue's title / branch / assignment at
/// whatever the first fetch saw. For those entries we fall back to a short
/// time-to-live (`REMOTE_META_TTL`).
///
/// `meta` is `None` for a **confirmed miss** — an id the store resolved to
/// nothing (a pre-GitHub slug task id the board doesn't carry as an issue, a done
/// task the open index doesn't carry, or a deleted one). A miss is *frozen* for
/// the session (see [`entry_stale`]): searched at most once, never re-resolved on
/// a TTL. That is what stops the feed from re-resolving an unresolvable id — both
/// the per-render-frame storm the 2026-09-08 incident attributed to the Activity
/// view (`GitHubStore::get` per event, id-search + issue-fetch per tick, no
/// negative cache) and the per-sweep `id-search` re-run the sidebar-never-loads
/// rework flagged (a short-TTL miss going stale every 15s, forever).
#[derive(Debug, Clone)]
pub struct TaskCacheEntry {
    /// Resolved metadata, or `None` for a confirmed miss (negative cache).
    pub meta: Option<TaskMeta>,
    /// Task-file modification time on the filesystem backend; `None` for a
    /// backend with no local task file (the TTL path takes over then).
    pub(crate) mtime: Option<SystemTime>,
    /// When this entry was last resolved from the store — drives the TTL refresh
    /// for entries that have no `mtime` to compare against (and every miss).
    pub(crate) fetched: Instant,
    /// Whether the resolved issue sits in a terminal (done/canceled) column. A
    /// resolved terminal issue is **frozen**: closed history doesn't change, so
    /// the feed never re-resolves it on the remote TTL. This is what stops a
    /// remote board from re-sweeping hundreds of done issues every tick — the
    /// done-history refetch loop (sidebar-never-loads incident). `false` for a
    /// miss (frozen on its own rule) and for open issues (which keep the short
    /// TTL so edits show up).
    pub(crate) terminal: bool,
}

/// How long a task-metadata cache entry with no local file (a remote backend)
/// is trusted before it is re-fetched from the store. Long enough that a busy
/// activity view doesn't spam the tracker API per render frame, short enough
/// that a remote title / branch / assignment edit shows up promptly. An open
/// issue's refresh is served from the board index
/// ([`serve_open_ids_from_index`]), so the TTL costs no API call for it.
const REMOTE_META_TTL: Duration = Duration::from_secs(15);

/// First retry delay after a title fetch *errors* (not a miss — a GraphQL blip, a
/// rejected query, a rate-limit park). An errored id is held out of the wanted
/// set until this elapses, then retried once; each further failure doubles the
/// delay up to [`FETCH_BACKOFF_MAX`]. This is what keeps a persistently-failing
/// fetch to a few calls per hour instead of one per refresh pass — the storm the
/// "a fetch error is not a miss" change opened (one bad id re-searched every
/// 750ms forever). Reset on the id's first success.
const FETCH_BACKOFF_BASE: Duration = Duration::from_secs(30);

/// Ceiling on the exponential fetch-error backoff. A fetch that keeps failing
/// (an id GitHub always rejects, a long rate-limit park) retries at most this
/// often, so it costs ~6 calls/hour at the cap rather than growing unbounded.
const FETCH_BACKOFF_MAX: Duration = Duration::from_secs(600);

/// Everything one activity read produces: the parsed events (oldest →
/// newest) and the title snapshot the feed renders from. Read off the UI
/// thread ([`read_activity_data`], [`read_activity_window`]) and handed to the
/// client to fold in.
#[derive(Debug, Clone, Default)]
pub struct ActivityData {
    pub events: Vec<Event>,
    pub task_cache: TaskCache,
    /// A read error message for the status line, if the read failed.
    pub status: Option<String>,
    /// Whether `events.log` holds older lines than the ones read. Only a
    /// windowed read leaves any behind.
    pub has_older: bool,
    /// The line window this was read with; `None` for the whole log.
    pub window: Option<usize>,
}

impl ActivityData {
    fn failed(status: String) -> Self {
        ActivityData {
            status: Some(status),
            ..ActivityData::default()
        }
    }

    /// An empty read whose status line carries `tag`, for tests that need a
    /// stand-in without touching disk or the tracker and want to read back
    /// which refresher produced it.
    #[doc(hidden)]
    pub fn tagged_for_test(tag: &str) -> Self {
        Self::failed(tag.to_string())
    }

    /// The status line, for tests to read back a [`Self::tagged_for_test`] tag.
    #[doc(hidden)]
    pub fn tag_for_test(&self) -> Option<&str> {
        self.status.as_deref()
    }
}

/// How many previously-unresolved ids one off-thread activity read resolves.
///
/// The shell reads the feed on a background worker and re-requests it on the
/// refresh cadence, so resolution is **incremental**: each pass resolves at most
/// this many still-unknown ids, then publishes the growing cache; the next pass
/// picks up where it left off. That bounds the work (and the GraphQL spend) per
/// pass and lets closed-history titles fill in progressively instead of the
/// worker grinding through hundreds of issues in one blocking call before the
/// feed shows a single title. One batch maps to one aliased `fetch_many` request
/// on the `github` backend. Once every referenced id is cached (and the terminal
/// ones frozen) a pass resolves nothing and the loop goes quiet — the exit
/// condition the done-history refetch incident lacked.
const MAX_META_RESOLVE_PER_PASS: usize = 50;

/// Whether a resolved issue in `col` has a frozen title — a terminal
/// (done/canceled) column. Closed history doesn't change, so such an entry is
/// never re-resolved once cached. A custom status that doesn't map to
/// `Done`/`Archived` is treated as live (conservative: it just keeps the short
/// TTL).
fn is_frozen_column(col: &Column) -> bool {
    matches!(
        col.category(),
        StatusCategory::Done | StatusCategory::Archived
    )
}

/// Whether `entry` needs re-resolving. Shared by the standalone and shell
/// resolve paths (both route through [`resolve_task_meta_into`]) so they apply
/// one rule:
///
/// * an unknown id (no entry) is always wanted;
/// * a **confirmed miss** is frozen — searched at most once per session (the
///   steady-state fix: see below);
/// * a **resolved terminal** issue is frozen — never re-fetched (the loop fix);
/// * the filesystem backend invalidates exactly on the task file's `mtime`;
/// * a resolved *open* remote entry (no `mtime`) falls back to the short TTL.
///
/// A confirmed miss (`meta: None`) is an id the store resolved to nothing: a
/// pre-GitHub slug task id the board carries in its event history but that is not
/// a GitHub issue, or a deleted one. Such an id will practically never resolve,
/// so re-searching it once the short TTL lapses — every pass, forever — is the
/// endless GraphQL spend the sidebar-never-loads rework flagged (a feed full of
/// slug ids re-ran ~234 `id-search` queries per sweep). Freezing it means an
/// unresolvable id costs at most one search per session. Misses are recorded
/// only after a *successful* fetch (a transient fetch error leaves the id
/// unknown, so a later pass retries it — see the resolve paths), so a real
/// outage never permanently hides a live title.
fn entry_stale(entry: Option<&TaskCacheEntry>, mtime: Option<SystemTime>) -> bool {
    match entry {
        None => true,
        Some(entry) => {
            // A confirmed miss: frozen for the session (never re-searched).
            if entry.meta.is_none() {
                return false;
            }
            // A resolved terminal (done/canceled) issue: closed history is frozen.
            if entry.terminal {
                return false;
            }
            if mtime.is_some() || entry.mtime.is_some() {
                entry.mtime != mtime
            } else {
                entry.fetched.elapsed() >= REMOTE_META_TTL
            }
        }
    }
}

/// The persistent task-metadata cache the shell carries **across** background
/// activity reads. Keeping it (rather than rebuilding from scratch each pass) is
/// what makes resolution incremental: a pass only fetches ids that are unknown or
/// stale, so once closed history has loaded once the feed stops re-sweeping it.
/// Owned by the shell, locked for the duration of each off-thread read.
#[derive(Default)]
pub struct ActivityMetaCache {
    entries: HashMap<String, TaskCacheEntry>,
    /// Logged `true` once the first hydration pass has run, so the start line is
    /// emitted exactly once per process (not on every quiet pass).
    hydration_started: bool,
    /// Logged `true` once a pass finds nothing left to resolve after at least one
    /// fetch, so the finish line is emitted exactly once.
    hydration_finished: bool,
    /// Per-id retry backoff for ids whose *fetch errored* — distinct from a
    /// resolved entry (cached) and a confirmed miss (frozen in `entries`). A
    /// transient error leaves the id unknown so a later pass retries it, but
    /// **bounded**: an errored id is held here until its `retry_at`, then tried
    /// once; each further failure pushes `retry_at` out exponentially (see
    /// [`FETCH_BACKOFF_BASE`]/[`FETCH_BACKOFF_MAX`]). Cleared on the id's first
    /// success. Without this, one id GitHub keeps rejecting (or a rate-limit
    /// park) is re-fetched every 750ms pass — a faster 403 than the loop this
    /// whole task fixes.
    backoff: HashMap<String, FetchBackoff>,
}

/// One id's fetch-error backoff state (see [`ActivityMetaCache::backoff`]).
#[derive(Debug, Clone)]
struct FetchBackoff {
    /// The earliest instant the id may be fetched again.
    retry_at: Instant,
    /// Consecutive failures so far — drives the exponential delay.
    attempts: u32,
}

impl ActivityMetaCache {
    /// Whether `id` is inside an active fetch-error backoff window at `now`: its
    /// last fetch errored and the next retry time hasn't arrived. Such an id is
    /// held out of the wanted set so a persistently-failing fetch doesn't re-run
    /// every pass.
    fn in_backoff(&self, id: &str, now: Instant) -> bool {
        self.backoff.get(id).is_some_and(|b| now < b.retry_at)
    }

    /// Record a failed fetch for `id`, advancing its backoff. The delay doubles
    /// with each consecutive failure from [`FETCH_BACKOFF_BASE`], capped at
    /// [`FETCH_BACKOFF_MAX`].
    fn note_fetch_failure(&mut self, id: &str, now: Instant) {
        let entry = self.backoff.entry(id.to_string()).or_insert(FetchBackoff {
            retry_at: now,
            attempts: 0,
        });
        entry.attempts = entry.attempts.saturating_add(1);
        // 2^(attempts-1) * base, saturating and capped. `min(31)` keeps the shift
        // in range; `saturating_mul` + `.min` cap the product at the ceiling.
        let shift = entry.attempts.saturating_sub(1).min(31);
        let delay = FETCH_BACKOFF_BASE
            .saturating_mul(1u32 << shift)
            .min(FETCH_BACKOFF_MAX);
        entry.retry_at = now + delay;
    }

    /// Drop any backoff for `id` — called when a fetch for it finally succeeds or
    /// it resolves to a confirmed miss, so a future transient error starts the
    /// backoff fresh at the base delay rather than wherever it left off.
    fn clear_backoff(&mut self, id: &str) {
        self.backoff.remove(id);
    }
}

/// Resolve the `wanted` ids the daemon-published board index already carries,
/// removing them from `wanted`. A remote board's open issues are all in the index
/// (title, branch, and the assignment overlay folded on top), so an open id's
/// [`REMOTE_META_TTL`] refresh is a local file read rather than a GraphQL call;
/// only closed history (frozen once fetched) and ids the index doesn't know yet
/// reach the network. Without this, every open id the feed referenced was
/// re-fetched every 15s for as long as the TUI stayed open. A `file_system`
/// board, or a missing / mismatched index, leaves `wanted` untouched.
/// Returns whether the board is remote (what's left of `wanted` then waits
/// on the network).
fn serve_open_ids_from_index(
    project_name: &str,
    wanted: &mut Vec<String>,
    cache: &mut ActivityMetaCache,
    now: Instant,
) -> bool {
    let Ok(project) = shelbi_state::load_project(project_name) else {
        return false;
    };
    let cfg = &project.issue_tracker;
    if !cfg.backend.is_remote() {
        return false;
    }
    let Ok(state) = shelbi_state::read_board_with_cfg(project_name, cfg) else {
        return true;
    };
    let mut board = state.into_issues();
    if board.is_empty() {
        return true;
    }
    shelbi_state::fold_assignment_overlay(project_name, cfg, &mut board);
    let mut by_id: HashMap<String, shelbi_state::IssueFile> =
        board.into_iter().map(|tf| (tf.task.id.clone(), tf)).collect();
    wanted.retain(|id| {
        let Some(tf) = by_id.remove(id) else {
            return true;
        };
        let terminal = is_frozen_column(&tf.task.column);
        cache.clear_backoff(id);
        cache.entries.insert(
            id.clone(),
            TaskCacheEntry {
                meta: Some(TaskMeta {
                    title: tf.task.title,
                    branch: tf.task.branch,
                    assigned_to: tf.task.assigned_to,
                }),
                mtime: task_file_mtime(project_name, id),
                fetched: now,
                terminal,
            },
        );
        false
    });
    true
}

/// How far one resolve pass may reach for titles.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// The cache, the local board index and a filesystem board's task files:
    /// never the network.
    Local,
    /// Everything, the tracker included.
    All,
}

/// One resolve pass: the snapshot, and what it left for later.
struct Resolved {
    cache: TaskCache,
    /// A [`Reach::Local`] pass left ids that only the tracker can resolve.
    needs_network: bool,
    /// More ids wanted resolving than one batch takes; a later pass continues.
    more: bool,
}

/// Resolve task metadata for the ids referenced by `events`, folding the result
/// into the persistent `cache` and returning a snapshot the feed renders from.
/// `now` is the clock the TTL and fetch-error backoff read (injected so a test
/// can advance it); production passes [`Instant::now`].
///
/// Incremental and bounded (see [`MAX_META_RESOLVE_PER_PASS`]): only ids that are
/// unknown or stale ([`entry_stale`]) **and not in a fetch-error backoff window**
/// are candidates, at most a batch per pass. Resolved terminal issues are frozen
/// so an idle board with hundreds of done cards makes **no** repeat requests (the
/// done-history refetch loop fix); confirmed misses are frozen too (the slug-id
/// re-search fix); and an id whose fetch *errors* is retried under an exponential
/// backoff (the retry-storm fix), never every pass. The store's own TTL/negative
/// caches back the rare live fetch that remains.
pub fn resolve_task_meta_into(
    project_name: &str,
    events: &[Event],
    cache: &mut ActivityMetaCache,
    now: Instant,
) -> TaskCache {
    resolve_task_meta(project_name, events, cache, now, Reach::All).cache
}

fn resolve_task_meta(
    project_name: &str,
    events: &[Event],
    cache: &mut ActivityMetaCache,
    now: Instant,
    reach: Reach,
) -> Resolved {
    let done = |cache: &ActivityMetaCache, needs_network: bool, more: bool| Resolved {
        cache: cache.entries.clone(),
        needs_network,
        more,
    };
    // Candidate ids: unique, referenced, unknown-or-stale, and not currently
    // backing off from a recent fetch error.
    let mut wanted: Vec<String> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for ev in events {
        let Some(id) = event_task_id(ev) else {
            continue;
        };
        if !seen.insert(id) {
            continue;
        }
        let mtime = task_file_mtime(project_name, id);
        if entry_stale(cache.entries.get(id), mtime) && !cache.in_backoff(id, now) {
            wanted.push(id.to_string());
        }
    }
    let remote = !wanted.is_empty() && serve_open_ids_from_index(project_name, &mut wanted, cache, now);
    if reach == Reach::Local && remote && !wanted.is_empty() {
        // The rest are the tracker's to answer: a later full pass.
        return done(cache, true, false);
    }

    if wanted.is_empty() {
        // Nothing to fetch this pass. If every referenced id is truly settled
        // (cached or a confirmed miss, and nothing waiting on a retry) announce
        // the quiet steady state once so the next loop like this is diagnosable
        // from `tui.log`. While an errored id is still backing off we stay silent
        // but don't claim "complete" — a retry pass is still pending.
        if cache.hydration_started && !cache.hydration_finished && cache.backoff.is_empty() {
            cache.hydration_finished = true;
            tracing::info!(
                project = %project_name,
                cached = cache.entries.len(),
                "activity title hydration complete; feed is quiet"
            );
        }
        return done(cache, false, false);
    }

    let total_wanted = wanted.len();
    let more = total_wanted > MAX_META_RESOLVE_PER_PASS;
    // Resolve at most one batch this pass; the next pass continues.
    wanted.truncate(MAX_META_RESOLVE_PER_PASS);

    if !cache.hydration_started {
        cache.hydration_started = true;
        tracing::info!(
            project = %project_name,
            pending = total_wanted,
            batch = wanted.len(),
            "activity title hydration started"
        );
    } else {
        tracing::debug!(
            project = %project_name,
            pending = total_wanted,
            batch = wanted.len(),
            "activity title hydration pass"
        );
    }
    // A new batch means a later pass will reach the "complete" line again.
    cache.hydration_finished = false;

    let store = match shelbi_state::issue_store_for(project_name) {
        Ok(s) => s,
        Err(_) => return done(cache, false, false),
    };
    let refs: Vec<&str> = wanted.iter().map(String::as_str).collect();
    // `fetch_many_partial` isolates per-id failures: it returns the ids that
    // resolved *and* the ids whose fetch errored, keeping the two apart so one
    // bad id neither discards the others' results nor is mistaken for a miss. A
    // fetch *error* is not a miss (the store couldn't answer — a GraphQL blip, a
    // rate-limit park); such ids back off and retry later rather than freezing as
    // misses. A hard failure of the whole call backs off the entire batch.
    let (fetched, failed) = match store.fetch_many_partial(&refs) {
        Ok(result) => result,
        Err(e) => {
            tracing::debug!(
                project = %project_name,
                error = %e,
                "activity title fetch failed; backing off this batch to retry later"
            );
            for id in &wanted {
                cache.note_fetch_failure(id, now);
            }
            return done(cache, false, more);
        }
    };
    let mut resolved: HashSet<String> = HashSet::new();
    for tf in fetched {
        let id = tf.task.id.clone();
        let mtime = task_file_mtime(project_name, &id);
        let terminal = is_frozen_column(&tf.task.column);
        resolved.insert(id.clone());
        // A success clears any prior backoff so the next transient error on this
        // id starts fresh at the base delay.
        cache.clear_backoff(&id);
        cache.entries.insert(
            id,
            TaskCacheEntry {
                meta: Some(TaskMeta {
                    title: tf.task.title,
                    branch: tf.task.branch,
                    assigned_to: tf.task.assigned_to,
                }),
                mtime,
                fetched: now,
                terminal,
            },
        );
    }
    // The ids whose fetch errored: back them off (bounded retry), and keep them
    // out of the negative cache below so they stay retryable once the backoff
    // lapses.
    let failed_set: HashSet<&str> = failed.iter().map(String::as_str).collect();
    for id in &failed {
        cache.note_fetch_failure(id, now);
    }
    // Negative-cache the genuine misses: a wanted id that neither resolved nor
    // errored was absent from a *successful* fetch, so it resolved to nothing (a
    // pre-GitHub slug id, a deleted issue). `entry_stale` freezes it for the
    // session so it is searched at most once — not re-run every sweep.
    for id in wanted {
        if resolved.contains(&id) || failed_set.contains(id.as_str()) {
            continue;
        }
        cache.clear_backoff(&id);
        cache.entries.insert(
            id,
            TaskCacheEntry {
                meta: None,
                mtime: None,
                fetched: now,
                terminal: false,
            },
        );
    }
    done(cache, false, more)
}

impl Event {
    /// The original `events.log` line.
    pub fn raw(&self) -> &str {
        match self {
            Event::Issue { raw, .. }
            | Event::Workspace { raw, .. }
            | Event::ZenDryRun { raw, .. }
            | Event::Heartbeat { raw, .. }
            | Event::Unknown { raw, .. } => raw,
            Event::System(sys) => &sys.raw,
        }
    }

    /// The project the line is scoped to, when it names one.
    pub fn project(&self) -> Option<&str> {
        match self {
            Event::Issue { project, .. }
            | Event::Workspace { project, .. }
            | Event::ZenDryRun { project, .. } => project.as_deref(),
            Event::Heartbeat { project, .. } => Some(project),
            Event::System(sys) => sys.project.as_deref(),
            Event::Unknown { .. } => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Reading
// ---------------------------------------------------------------------------

/// Read the whole feed off the UI thread: a full re-read of `events.log`, with
/// task metadata resolved **incrementally** into the caller-owned persistent
/// `cache` (a pass only fetches ids that are unknown or stale, at most a batch
/// at a time ([`MAX_META_RESOLVE_PER_PASS`]), and resolved terminal issues are
/// frozen). So once closed history has loaded once the feed stops re-sweeping
/// it, instead of the per-pass full re-fetch that looped forever on a large
/// remote board (the sidebar-never-loads incident).
///
/// The TUI shell runs this on its dedicated activity worker, separate from the
/// sidebar/board read, so even the first cold hydration pass never delays the
/// sidebar painting.
pub fn read_activity_data(project_name: &str, cache: &Mutex<ActivityMetaCache>) -> ActivityData {
    let path = match events_log_path() {
        Ok(p) => p,
        Err(e) => return ActivityData::failed(format!("events.log path failed: {e}")),
    };
    // Missing log file → empty feed, no error.
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return ActivityData::failed(format!("read events.log: {e}")),
    };
    let events = parse_lines(&text);
    let task_cache = resolve_locked(project_name, &events, cache);
    ActivityData {
        events,
        task_cache,
        status: None,
        has_older: false,
        window: None,
    }
}

/// Read only the newest `window` lines of `events.log` (the desktop's paged
/// feed): the same parse and the same incremental title resolution as
/// [`read_activity_data`], over the tail. [`ActivityData::has_older`] says
/// whether the log holds older lines, so the view can offer "load older",
/// which re-reads with a window one [`ACTIVITY_PAGE`] larger.
///
/// Reading backwards from the end in blocks keeps a pass proportional to the
/// window, not the log (a busy hub's log runs to tens of thousands of lines),
/// so the feed can re-read it on the TUI's refresh cadence.
pub fn read_activity_window(
    project_name: &str,
    cache: &Mutex<ActivityMetaCache>,
    window: usize,
) -> ActivityData {
    let (events, has_older) = match read_window_events(window) {
        Ok(read) => read,
        Err(failed) => return failed,
    };
    let task_cache = resolve_locked(project_name, &events, cache);
    ActivityData {
        events,
        task_cache,
        status: None,
        has_older,
        window: Some(window),
    }
}

/// A [`read_activity_window_staged`] read.
#[derive(Debug, Clone, Default)]
pub struct WindowRead {
    pub data: ActivityData,
    /// More titles wait than one pass resolves: reading again soon fills
    /// them in (a batch a pass).
    pub more_titles: bool,
}

/// [`read_activity_window`] for a feed that shows what it has first: when
/// some of the window's titles wait on a remote tracker, `early` is handed
/// the read with the titles already at hand (the cache, the board index)
/// before the tracker is asked, and the call returns the read with them
/// resolved. `early` isn't called when nothing waits on the network.
pub fn read_activity_window_staged(
    project_name: &str,
    cache: &Mutex<ActivityMetaCache>,
    window: usize,
    early: impl FnOnce(ActivityData),
) -> WindowRead {
    let (events, has_older) = match read_window_events(window) {
        Ok(read) => read,
        Err(data) => return WindowRead { data, more_titles: false },
    };
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    let now = Instant::now();
    let local = resolve_task_meta(project_name, &events, &mut guard, now, Reach::Local);
    let resolved = if local.needs_network {
        early(ActivityData {
            events: events.clone(),
            task_cache: local.cache,
            status: None,
            has_older,
            window: Some(window),
        });
        resolve_task_meta(project_name, &events, &mut guard, now, Reach::All)
    } else {
        local
    };
    WindowRead {
        data: ActivityData {
            events,
            task_cache: resolved.cache,
            status: None,
            has_older,
            window: Some(window),
        },
        more_titles: resolved.more,
    }
}

/// The newest `window` lines of `events.log`, parsed, and whether older ones
/// exist; the failed read for the status line when the log won't read.
fn read_window_events(window: usize) -> Result<(Vec<Event>, bool), ActivityData> {
    let path = events_log_path()
        .map_err(|e| ActivityData::failed(format!("events.log path failed: {e}")))?;
    let (text, has_older) = match read_last_lines(&path, window) {
        Ok(read) => read,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (String::new(), false),
        Err(e) => return Err(ActivityData::failed(format!("read events.log: {e}"))),
    };
    Ok((parse_lines(&text), has_older))
}

fn parse_lines(text: &str) -> Vec<Event> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(parse_event_line)
        .collect()
}

fn resolve_locked(
    project_name: &str,
    events: &[Event],
    cache: &Mutex<ActivityMetaCache>,
) -> TaskCache {
    let mut guard = cache.lock().unwrap_or_else(|p| p.into_inner());
    resolve_task_meta_into(project_name, events, &mut guard, Instant::now())
}

/// The last `n` non-empty lines of `path` (oldest first, newline-joined), and
/// whether the file holds older lines before them. Reads backwards in blocks
/// until it has `n` complete lines or reaches the start, so the cost tracks
/// `n`, not the file. Non-UTF-8 bytes become replacement characters.
pub fn read_last_lines(path: &Path, n: usize) -> std::io::Result<(String, bool)> {
    const BLOCK: u64 = 64 * 1024;
    let mut f = fs::File::open(path)?;
    let len = f.metadata()?.len();
    let mut pos = len;
    let mut buf: Vec<u8> = Vec::new();
    // `n + 1` newlines past the first guarantee `n` complete lines even with a
    // trailing newline and a partial first line.
    while pos > 0 && buf.iter().filter(|&&b| b == b'\n').count() <= n + 1 {
        let step = BLOCK.min(pos);
        pos -= step;
        f.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0u8; step as usize];
        f.read_exact(&mut chunk)?;
        chunk.extend_from_slice(&buf);
        buf = chunk;
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    // Started mid-file: the first line is (possibly) a fragment of an older one.
    if pos > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let mut lines: Vec<&str> = lines.into_iter().filter(|l| !l.trim().is_empty()).collect();
    let mut has_older = pos > 0;
    if lines.len() > n {
        lines.drain(..lines.len() - n);
        has_older = true;
    }
    Ok((lines.join("\n"), has_older))
}

/// Read everything past `offset` in `path` as UTF-8: the TUI's standalone
/// process tails the log with this between full reads. Best-effort: non-UTF-8
/// bytes become replacement chars rather than failing the whole read.
pub fn read_tail(path: &PathBuf, offset: u64) -> std::io::Result<String> {
    let mut f = fs::File::open(path)?;
    if offset > 0 {
        f.seek(SeekFrom::Start(offset))?;
    }
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

// ---------------------------------------------------------------------------
// Filters
// ---------------------------------------------------------------------------

/// What kind of activity an event is, for the kind filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ActivityKind {
    /// A task status transition made by a person or an agent.
    Tasks,
    /// A Zen-driven transition (promote, merge, bail) or a Zen dry-run.
    Zen,
    /// A workspace state change (working, awaiting input, paused, ...).
    Workspaces,
    /// Everything else: dispatch, rebase, push, merge, CI, ssh, supervision.
    System,
}

impl ActivityKind {
    pub const ALL: [ActivityKind; 4] = [
        ActivityKind::Tasks,
        ActivityKind::Zen,
        ActivityKind::Workspaces,
        ActivityKind::System,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ActivityKind::Tasks => "Tasks",
            ActivityKind::Zen => "Zen",
            ActivityKind::Workspaces => "Workspaces",
            ActivityKind::System => "System",
        }
    }

    /// The kind of `ev`, or `None` for a heartbeat (never human-facing).
    pub fn of(ev: &Event) -> Option<ActivityKind> {
        Some(match ev {
            Event::Heartbeat { .. } => return None,
            Event::Issue { reason, .. } if parse_zen_reason(reason).is_some() => ActivityKind::Zen,
            Event::Issue { .. } => ActivityKind::Tasks,
            Event::ZenDryRun { .. } => ActivityKind::Zen,
            Event::Workspace { .. } => ActivityKind::Workspaces,
            Event::System(_) | Event::Unknown { .. } => ActivityKind::System,
        })
    }
}

/// The feed's filter: by kind, by project, and by workspace. The default shows
/// everything (except heartbeats, which no filter shows).
///
/// Kinds are a multi-select union: with none selected ("All") every kind
/// passes, otherwise an event of any selected kind does. The TUI's pill row
/// toggles [`ActivityKind::Zen`] and [`ActivityKind::Workspaces`]; the
/// desktop offers every kind plus the project and workspace pickers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ActivityFilter {
    /// Selected kinds, sorted; empty means all.
    pub kinds: Vec<ActivityKind>,
    /// Only events scoped to this project. An event that names no project
    /// (an ssh tunnel, a legacy line) is hidden while this is set.
    pub project: Option<String>,
    /// Only events attributed to this workspace ([`event_workspace`]).
    pub workspace: Option<String>,
}

impl ActivityFilter {
    /// No kind selected: the kind dimension shows everything (the "All" pill).
    pub fn is_all(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Nothing narrowed in any dimension.
    pub fn is_unfiltered(&self) -> bool {
        self.kinds.is_empty() && self.project.is_none() && self.workspace.is_none()
    }

    pub fn has_kind(&self, kind: ActivityKind) -> bool {
        self.kinds.contains(&kind)
    }

    /// Add or remove `kind` from the selection.
    pub fn toggle_kind(&mut self, kind: ActivityKind) {
        if let Some(i) = self.kinds.iter().position(|k| *k == kind) {
            self.kinds.remove(i);
        } else {
            self.kinds.push(kind);
            self.kinds.sort();
        }
    }

    /// Back to every kind (the `All` pill); project and workspace stay.
    pub fn clear_kinds(&mut self) {
        self.kinds.clear();
    }

    /// Whether `ev` passes. Heartbeats never do: they are the orchestrator's
    /// wake-up, and a "nothing happened" row every few minutes would drown the
    /// feed. `cache` attributes a task event to its workspace.
    pub fn matches(&self, ev: &Event, cache: &TaskCache) -> bool {
        let Some(kind) = ActivityKind::of(ev) else {
            return false;
        };
        if !self.kinds.is_empty() && !self.kinds.contains(&kind) {
            return false;
        }
        if let Some(project) = &self.project {
            if ev.project() != Some(project.as_str()) {
                return false;
            }
        }
        if let Some(workspace) = &self.workspace {
            if event_workspace(ev, cache) != Some(workspace.as_str()) {
                return false;
            }
        }
        true
    }
}

/// The workspace an event is attributed to, the same one its row's chip
/// names: a workspace event's own workspace, a task event's assignee, and the
/// workspace a dispatch / rebase / detach / send / pane / supervision line
/// names. `None` for an event about no workspace (or the orchestrator).
pub fn event_workspace<'a>(ev: &'a Event, cache: &'a TaskCache) -> Option<&'a str> {
    match ev {
        Event::Workspace { name, .. } => Some(name),
        Event::Issue { id, .. } => task_meta(cache, id).and_then(|m| m.assigned_to.as_deref()),
        Event::ZenDryRun { task_id, .. } => {
            task_meta(cache, task_id).and_then(|m| m.assigned_to.as_deref())
        }
        Event::System(sys) => match sys.kind {
            SystemKind::Dispatch | SystemKind::Rebase | SystemKind::WorktreeDetach => {
                sys.detail.as_deref()
            }
            SystemKind::Send | SystemKind::PaneDeath => sys.target.as_deref(),
            SystemKind::Supervision => sys.target.as_deref().filter(|t| *t != "orchestrator"),
            _ => None,
        },
        Event::Heartbeat { .. } | Event::Unknown { .. } => None,
    }
}

/// The distinct projects and workspaces the loaded events name, sorted: the
/// choices the desktop's project and workspace pickers offer.
pub fn filter_choices(events: &[Event], cache: &TaskCache) -> (Vec<String>, Vec<String>) {
    let mut projects: Vec<String> = events
        .iter()
        .filter(|e| ActivityKind::of(e).is_some())
        .filter_map(|e| e.project().map(str::to_string))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    projects.sort();
    let mut workspaces: Vec<String> = events
        .iter()
        .filter_map(|e| event_workspace(e, cache).map(str::to_string))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();
    workspaces.sort();
    (projects, workspaces)
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Split an optional leading `project=<name> ` scope off an event body.
/// Most hub-emitted lines are prefixed with the project the event belongs
/// to (`project=shelbi task=…`, `project=shelbi workspace=…`,
/// `project=shelbi closed …`); a handful of infra kinds (`ssh`, `dispatch`,
/// `rebase`, `worktree-detach`, `message=…`) are not, and `send` carries
/// its `project=` as a later field. Returns the parsed project name (if the
/// prefix was present) and the remainder to classify.
fn strip_project_prefix(rest: &str) -> (Option<String>, &str) {
    match rest.strip_prefix("project=") {
        Some(after) => match after.split_once(' ') {
            Some((name, tail)) => (Some(name.to_string()), tail),
            None => (Some(after.to_string()), ""),
        },
        None => (None, rest),
    }
}

/// Parse one `events.log` line into an [`Event`]. Best-effort: every
/// emitted kind is classified into a first-class, human-renderable shape.
/// Only a genuinely unrecognized line lands in [`Event::Unknown`], and even
/// that renders cleaned up (no raw leading ISO timestamp) rather than being
/// dropped.
pub fn parse_event_line(line: &str) -> Event {
    let raw = line.to_string();
    let mut parts = line.splitn(2, ' ');
    let ts_str = parts.next().unwrap_or("");
    let after_ts = parts.next().unwrap_or("");
    let ts = DateTime::parse_from_rfc3339(ts_str)
        .ok()
        .map(|t| t.with_timezone(&Utc));

    let Some(ts) = ts else {
        return Event::Unknown { ts: None, raw };
    };

    // Peel the `project=<name>` scope so both prefixed and legacy-bare
    // task/workspace lines route through the same classifier below.
    let (project, rest) = strip_project_prefix(after_ts);

    // CI verdict shape (project- and task-scoped):
    // `task=<id> ci pr=<pr> check=<check> conclusion=<success|failure> head_sha=<sha>`.
    // The bare ` ci ` keyword between the `task=` scope and the CI detail
    // distinguishes it from an ordinary `task=<id> <from> -> <to>` transition,
    // so it must be matched before the `task=` branch below or it would fall
    // through to a raw `Unknown` row.
    if rest.starts_with("task=") && rest.contains(" ci ") {
        let kv = parse_kv(rest);
        let detail = match (kv.get("pr"), kv.get("check")) {
            (Some(pr), Some(check)) => Some(format!("PR #{pr} {}", humanize_token(check))),
            (Some(pr), None) => Some(format!("PR #{pr}")),
            _ => None,
        };
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Ci,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("conclusion").cloned(),
            detail,
            raw,
        });
    }

    if let Some(rest) = rest.strip_prefix("task=") {
        // Two on-the-wire shapes coexist (`Plans/workflows.md` §10):
        //
        //   Old: `<id> <from> -> <to> reason=<reason>`
        //   New: `<id> workflow=<name> <from> -> <to> reason=<reason>
        //         from_category=<cat> to_category=<cat>`
        //
        // The second token after `task=<id>` disambiguates — if it starts
        // with `workflow=` we read the new fields, otherwise we default
        // `workflow` to "default" and derive categories from the canonical
        // 5-status column-to-category map (`Column::category`).
        let mut tokens = rest.split(' ');
        let id = tokens.next().unwrap_or("").to_string();
        let mut next = tokens.next().unwrap_or("");
        let workflow = if let Some(name) = next.strip_prefix("workflow=") {
            let name = name.to_string();
            next = tokens.next().unwrap_or("");
            name
        } else {
            DEFAULT_WORKFLOW_NAME.to_string()
        };
        let from_s = next;
        let arrow = tokens.next().unwrap_or("");
        let to_s = tokens.next().unwrap_or("");
        // Everything after `<to>` is k=v tokens (reason, from_category,
        // to_category) in any order — defensive against future field
        // shuffles, and tolerant of missing trailing fields on old lines.
        let kv = parse_kv(tokens.collect::<Vec<&str>>().join(" ").as_str());
        if arrow == "->" {
            if let (Ok(from), Ok(to)) = (from_s.parse::<Column>(), to_s.parse::<Column>()) {
                let reason = kv.get("reason").cloned().unwrap_or_default();
                let agent = agent_from_reason(&reason);
                let from_category = kv
                    .get("from_category")
                    .and_then(|s| s.parse::<StatusCategory>().ok())
                    .unwrap_or_else(|| from.category());
                let to_category = kv
                    .get("to_category")
                    .and_then(|s| s.parse::<StatusCategory>().ok())
                    .unwrap_or_else(|| to.category());
                return Event::Issue {
                    ts,
                    project,
                    id,
                    workflow,
                    from,
                    to,
                    reason,
                    agent,
                    from_category,
                    to_category,
                    raw,
                };
            }
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    if let Some(rest) = rest.strip_prefix("zen-dryrun ") {
        // Format: `task=<id> action=<verb> detail=<short>`
        let mut task_id = String::new();
        let mut action = String::new();
        let mut detail = String::new();
        for tok in rest.split_whitespace() {
            if let Some(v) = tok.strip_prefix("task=") {
                task_id = v.to_string();
            } else if let Some(v) = tok.strip_prefix("action=") {
                action = v.to_string();
            } else if let Some(v) = tok.strip_prefix("detail=") {
                detail = v.to_string();
            }
        }
        if !task_id.is_empty() && !action.is_empty() {
            return Event::ZenDryRun {
                ts,
                project,
                task_id,
                action,
                detail,
                raw,
            };
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    // Heartbeat shape (project-scoped): `heartbeat zen_eligible=<N>
    // idle_workspaces=<M>`. We match on the keyword token alone — the
    // trailing counts are for the orchestrator's react rule, not the feed.
    if rest == "heartbeat" || rest.starts_with("heartbeat ") {
        if let Some(name) = &project {
            return Event::Heartbeat {
                ts,
                project: name.clone(),
                raw,
            };
        }
    }

    // Workspace lines. New emissions use `workspace=<name>`; legacy lines
    // (and one-release tooling that lags the rename) use `worker=<name>` —
    // both accepted. A `workspace=<name>` line is one of three shapes: a
    // state transition (`<prev> -> <new>`), a pane-liveness change
    // (`pane_alive=<bool>`), or a supervision line (handled below); we
    // disambiguate on the fields present.
    let ws_rest = rest
        .strip_prefix("workspace=")
        .or_else(|| rest.strip_prefix("worker="));
    if let Some(ws_rest) = ws_rest {
        let mut tokens = ws_rest.split(' ');
        let name = tokens.next().unwrap_or("");
        let second = tokens.next().unwrap_or("");
        // `workspace=<name> pane_alive=<bool> reason=<r>` — a pane death /
        // recovery, not a state transition.
        if let Some(alive) = second.strip_prefix("pane_alive=") {
            let kv = parse_kv(ws_rest);
            return Event::System(SystemEvent {
                ts,
                kind: SystemKind::PaneDeath,
                project,
                target: Some(name.to_string()),
                status: Some(alive.to_string()),
                detail: kv.get("reason").map(|r| humanize_token(r)),
                raw,
            });
        }
        // `workspace=<name> supervision=<action> …` — a supervisor action
        // scoped to a workspace pane.
        if second.starts_with("supervision=") {
            return parse_supervision(ts, project, Some(name.to_string()), ws_rest, raw);
        }
        // Otherwise a state transition: `<prev> -> <new> [reason=…]`.
        let prev_s = second;
        let arrow = tokens.next().unwrap_or("");
        let new_s = tokens.next().unwrap_or("");
        if arrow == "->" {
            if let Some(new) = parse_workspace_state(new_s) {
                let prev = if prev_s == "none" {
                    None
                } else {
                    parse_workspace_state(prev_s)
                };
                return Event::Workspace {
                    ts,
                    project,
                    name: name.to_string(),
                    prev,
                    new,
                    raw,
                };
            }
        }
        return Event::Unknown { ts: Some(ts), raw };
    }

    // -- Infra kinds carrying no `project=` prefix ------------------------

    if let Some(body) = rest.strip_prefix("ssh reverse-forward ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Ssh,
            project,
            target: kv.get("host").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("detail").map(|d| humanize_token(d)),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("dispatch ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Dispatch,
            // `dispatch` carries `project=` as an in-body field (like `send` /
            // `review-ready`), not a leading prefix, so read it from the kv map;
            // fall back to any outer prefix, and leave it `None` for a legacy
            // line that has neither.
            project: kv.get("project").cloned().or(project),
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("workspace").cloned(),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("rebase ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Rebase,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("workspace").cloned(),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("push ") {
        let kv = parse_kv(body);
        // The `detail=` tail carries the force range on success or the reason
        // on a failed / blocked push; keep it as the row's dim tail so the feed
        // shows *why* a handoff didn't advance. A `-` placeholder collapses to None.
        let detail = kv
            .get("detail")
            .filter(|d| d.as_str() != "-")
            .map(|d| humanize_token(d));
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Push,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail,
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("worktree-detach ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::WorktreeDetach,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("workspace").cloned(),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("merge ") {
        let kv = parse_kv(body);
        // On failure the `detail=` field carries the reason (a `gh` stderr line);
        // keep it as the row's dim tail so the feed shows *why* the merge was
        // refused, not just that it was. A `-` placeholder collapses to None.
        let detail = kv
            .get("detail")
            .filter(|d| d.as_str() != "-")
            .map(|d| humanize_token(d));
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Merge,
            project,
            target: kv.get("task").cloned(),
            status: kv.get("status").cloned(),
            detail,
            raw,
        });
    }

    // `send project=<p> workspace=<name> status=<s> detail=<d>` — the
    // `project=` here is a field, not the leading scope, so it survived the
    // prefix peel and we read it back out of the k=v tail.
    if let Some(body) = rest.strip_prefix("send ") {
        let kv = parse_kv(body);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Send,
            project: kv.get("project").cloned(),
            target: kv.get("workspace").cloned(),
            status: kv.get("status").cloned(),
            detail: kv.get("detail").map(|d| humanize_token(d)),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("message=") {
        // `message=<id> task=<id> push=ok` or `… ack=<kind>`.
        let kv = parse_kv(body);
        let (status, verb) = if let Some(p) = kv.get("push") {
            (Some(p.clone()), "push")
        } else if let Some(a) = kv.get("ack") {
            (Some(a.clone()), "ack")
        } else {
            (None, "message")
        };
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Message,
            project,
            target: kv.get("task").cloned(),
            status,
            detail: Some(verb.to_string()),
            raw,
        });
    }

    // -- Project-scoped verbs (prefix already peeled) ---------------------

    if let Some(reason) = rest.strip_prefix("closed") {
        let kv = parse_kv(reason);
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Closed,
            project: project.clone(),
            target: project,
            status: None,
            detail: kv.get("reason").map(|r| humanize_token(r)),
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("handoff ") {
        let kv = parse_kv(body);
        let detail = kv
            .get("detail")
            .filter(|d| d.as_str() != "-")
            .map(|d| humanize_token(d));
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Handoff,
            project: project.clone(),
            target: project,
            status: kv.get("outcome").cloned(),
            detail,
            raw,
        });
    }

    if let Some(body) = rest.strip_prefix("mode=zen ") {
        // `<prev> -> <new> reason=<source>`.
        let mut tokens = body.split(' ');
        let prev = tokens.next().unwrap_or("");
        let _arrow = tokens.next();
        let new = tokens.next().unwrap_or("");
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Mode,
            project: project.clone(),
            target: project,
            status: Some(new.to_string()),
            detail: Some(format!("{prev} → {new}")),
            raw,
        });
    }

    if rest.starts_with("supervision=") {
        return parse_supervision(ts, project, None, rest, raw);
    }

    // Hub-global daemon lines with no project scope, e.g.
    // `daemon session-lost reason=…` from the macOS session-health monitor.
    if let Some(tail) = rest.strip_prefix("daemon ") {
        let kv = parse_kv(tail);
        let action = tail.split(' ').next().unwrap_or("event");
        let detail = match kv.get("reason") {
            Some(r) => format!("{} ({})", humanize_token(action), humanize_token(r)),
            None => humanize_token(action),
        };
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Other,
            project: None,
            target: Some("daemon".to_string()),
            status: None,
            detail: Some(detail),
            raw,
        });
    }

    // Any remaining project-scoped line: keep it human (verb + humanized
    // tail) rather than dumping raw wire syntax. Non-project lines with no
    // recognized shape are the genuine last resort → cleaned Unknown.
    if project.is_some() {
        let verb = rest.split(' ').next().unwrap_or("").to_string();
        let detail = rest
            .split_once(' ')
            .map(|(_, tail)| humanize_token(tail))
            .filter(|d| !d.is_empty());
        return Event::System(SystemEvent {
            ts,
            kind: SystemKind::Other,
            project,
            target: None,
            status: None,
            detail: detail.or(Some(verb)),
            raw,
        });
    }

    Event::Unknown { ts: Some(ts), raw }
}

/// Parse a supervision line body (`supervision=<action> [target=orchestrator]
/// reason=<r>`) into a [`SystemEvent`]. Shared by the workspace-scoped form
/// (`workspace=<name> supervision=…`) and the orchestrator-scoped form
/// (`supervision=… target=orchestrator …`).
fn parse_supervision(
    ts: DateTime<Utc>,
    project: Option<String>,
    workspace: Option<String>,
    body: &str,
    raw: String,
) -> Event {
    let kv = parse_kv(body);
    // Prefer the explicit workspace, else the `target=` field (orchestrator).
    let target = workspace.or_else(|| kv.get("target").cloned());
    Event::System(SystemEvent {
        ts,
        kind: SystemKind::Supervision,
        project,
        target,
        status: kv.get("supervision").cloned(),
        detail: kv.get("reason").map(|r| humanize_token(r)),
        raw,
    })
}

fn parse_workspace_state(s: &str) -> Option<WorkspaceState> {
    match s {
        "working" => Some(WorkspaceState::Working),
        "awaiting_input" => Some(WorkspaceState::AwaitingInput),
        "blocked" => Some(WorkspaceState::Blocked),
        "paused" => Some(WorkspaceState::Paused),
        "serving" => Some(WorkspaceState::Serving),
        _ => None,
    }
}

/// Turn a wire token into a human fragment: underscores and hyphens become
/// spaces so `master_open_failed` reads `master open failed` and
/// `up-to-date` reads `up to date`. Any residual `key=value` pairs are
/// dropped so a row never shows raw wire syntax; if that empties the string,
/// the caller falls back to its own label.
fn humanize_token(s: &str) -> String {
    s.split_whitespace()
        .filter(|tok| !tok.contains('='))
        .map(|tok| tok.replace(['_', '-'], " "))
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// One parsed `reason=` string from a zen-driven task event. The
/// orchestrator emits these as `key=value` pairs trailing a
/// `zen:`/`orchestrator:zen-*` head; this enum is the renderer-facing
/// shape after stripping the head and lifting out the fields each
/// variant displays. Unknown keys are dropped — we never want a missing
/// field to crash the feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZenReason {
    Promote {
        category: Option<String>,
    },
    Merge {
        pr: Option<String>,
        sha: Option<String>,
    },
    FailedChecks {
        cmd: Option<String>,
        exit: Option<String>,
    },
    DiffTooLarge {
        files: Option<String>,
        lines: Option<String>,
    },
    DangerPath {
        paths: Option<String>,
    },
    CiTimeout {
        duration: Option<String>,
    },
    MergeConflict {
        files: Option<String>,
    },
    /// Recognized zen-family prefix but not one of the specific kinds
    /// above. Renders with the generic zen badge + tint so future
    /// reasons still look "machine-driven" without a code change.
    Other,
}

/// Recognize `orchestrator:zen-*` and `zen:*` reason strings and pull
/// out the trailing `key=value` pairs each variant cares about. Returns
/// `None` for non-zen reasons so callers can fall through to the
/// default user-action renderer.
pub fn parse_zen_reason(reason: &str) -> Option<ZenReason> {
    // `orchestrator:zen-merge` is matched on its prefix, not an exact head.
    // The orchestrator emits the canonical reason with a space
    // (`orchestrator:zen-merge pr=<n>`), but `append_task_event` folds
    // spaces into underscores on write, so on disk the head arrives glued
    // to its tail as `orchestrator:zen-merge_pr=<n>` — an exact-head match
    // would miss every real event-log line and drop it to `Other`.
    // Historical lines (`orchestrator:zen-merge_ci-green(<n>)`) and the bare
    // `orchestrator:zen-merge` land here too and still render with the merge
    // badge, just without a parsed identifier. See `kv_from_mixed` for why
    // the tail is scanned across both separators.
    if reason == "orchestrator:zen-merge"
        || reason.starts_with("orchestrator:zen-merge ")
        || reason.starts_with("orchestrator:zen-merge_")
    {
        return Some(ZenReason::Merge {
            pr: kv_from_mixed(reason, "pr"),
            sha: kv_from_mixed(reason, "sha"),
        });
    }

    let (head, rest) = reason.split_once(' ').unwrap_or((reason, ""));
    let extras = parse_kv(rest);
    let get = |k: &str| extras.get(k).cloned();
    Some(match head {
        "orchestrator:zen-promote" => ZenReason::Promote {
            category: get("category"),
        },
        "zen:failed-checks" => ZenReason::FailedChecks {
            cmd: get("cmd"),
            exit: get("exit"),
        },
        "zen:diff-too-large" => ZenReason::DiffTooLarge {
            files: get("files"),
            lines: get("lines"),
        },
        "zen:danger-path" => ZenReason::DangerPath {
            paths: get("paths"),
        },
        "zen:ci-timeout" => ZenReason::CiTimeout {
            duration: get("duration"),
        },
        "zen:merge-conflict" => ZenReason::MergeConflict {
            files: get("files"),
        },
        other if other.starts_with("zen:") || other.starts_with("orchestrator:zen-") => {
            ZenReason::Other
        }
        _ => return None,
    })
}

/// Pull `agent=<name>` out of a sanitized reason value. The writer
/// (`shelbi task start`) appends ` agent=<name>` to the human reason and
/// `append_task_event` folds the space into an underscore — so on disk
/// the field lives as one of the underscore-joined segments inside
/// `reason=<value>` (e.g. `orchestrator:auto-dispatch_workspace=alpha_agent=developer`).
/// Returns `None` for lines emitted before this field existed.
pub fn agent_from_reason(reason: &str) -> Option<String> {
    reason
        .split('_')
        .find_map(|seg| seg.strip_prefix("agent=").map(str::to_string))
}

/// Pull `<key>=<value>` out of a reason whose tokens may be separated by
/// spaces (the pre-normalization string the orchestrator emits, and what
/// unit tests pass directly) or underscores (the on-disk form after
/// `append_task_event` folds spaces). Used by the zen-merge parse, whose
/// values are bare tokens — a PR number or short SHA — with no embedded
/// separators. An empty value (`pr=`) is treated as absent.
fn kv_from_mixed(reason: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    reason
        .split([' ', '_'])
        .find_map(|seg| seg.strip_prefix(prefix.as_str()))
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Parse `k=v k2=v2 …` from a reason tail. Values may be double-quoted
/// (`cmd="cargo test"`) to allow embedded spaces. Tokens missing a
/// `=` are skipped silently — the parser should never reject a real
/// event log line.
pub fn parse_kv(s: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut chars = s.chars().peekable();
    loop {
        while matches!(chars.peek(), Some(c) if c.is_whitespace()) {
            chars.next();
        }
        if chars.peek().is_none() {
            break;
        }
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }
        if chars.peek() != Some(&'=') {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                chars.next();
            }
            continue;
        }
        chars.next();
        let mut val = String::new();
        if chars.peek() == Some(&'"') {
            chars.next();
            while let Some(&c) = chars.peek() {
                if c == '"' {
                    chars.next();
                    break;
                }
                val.push(c);
                chars.next();
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                val.push(c);
                chars.next();
            }
        }
        if !key.is_empty() {
            out.insert(key, val);
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Feed rows
// ---------------------------------------------------------------------------

/// The `⎇ ` icon a branch secondary line leads with.
pub const BRANCH_ICON: &str = "⎇ ";

/// The task-metadata snapshot a feed renders from (see [`TaskCacheEntry`]).
pub type TaskCache = HashMap<String, TaskCacheEntry>;

/// One fully-specified feed row, ready for a client to lay out as a
/// top-aligned three-column flex: the identity chip pins top-left, the state
/// pill and relative time pin top-right, and the message (dim verb + title +
/// optional trail) wraps in between. The optional secondary prints as a dim
/// line beneath, aligned under the title.
///
/// ```text
/// [alpha]  finished  site: make the home feature-grid mockups feel     [ready for review]  now
///                    real, alive, and animated on hover
///                    ⎇ shelbi/home-feature-grid
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FeedRow {
    /// The bracketed identity chip (`[alpha]`, `[you]`, `[zen]`).
    pub chip: Option<Chip>,
    /// The dim verb (finished / is building / merged / ...). Empty means the
    /// message is a bare phrase with no verb column.
    pub verb: String,
    /// Task rows keep the dim verb tone; system rows tint it with the event's
    /// status so `unreachable` reads red.
    pub verb_tone: Tone,
    /// The task title (or system phrase): bright, wraps, never truncated.
    pub title: String,
    /// Bright white, or dim for a raw last-resort line.
    pub title_tone: Tone,
    /// Dim text that flows straight after the title (a zen bail tag
    /// ` — checks failed`, a fold trailer ` (×N)`).
    pub trail: Option<(String, Tone)>,
    /// The right-pinned state pill (`[in progress]`, `[done]`, ...).
    pub pill: Option<Pill>,
    /// The right-aligned relative time; a finished row prefixes `took Xm · `,
    /// a folded run reads `last 4m`.
    pub time: String,
    /// The dim second line.
    pub secondary: Option<Secondary>,
    /// What clicking the row opens.
    pub target: Option<RowTarget>,
}

/// A bracketed identity chip. Agent chips carry the workspace tint, bold;
/// `you` and machine sources are dimmed so human and agent activity stand out
/// from machine noise.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Chip {
    pub label: String,
    pub tone: Tone,
    pub bold: bool,
}

impl Chip {
    /// `[label]`.
    pub fn text(&self) -> String {
        format!("[{}]", self.label)
    }
}

/// The colored outcome pill pinned to the right of a task row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Pill {
    pub text: String,
    pub tone: Tone,
}

impl Pill {
    /// `[text]`.
    pub fn bracketed(&self) -> String {
        format!("[{}]", self.text)
    }
}

/// The dim second line: a git branch (shown with [`BRANCH_ICON`]) or any other
/// muted context fragment (`backlog → todo`, a zen bail reason, ...).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Secondary {
    Branch(String),
    Detail(String),
}

impl Secondary {
    /// The line's leading icon (empty for a detail) and text.
    pub fn parts(&self) -> (&'static str, &str) {
        match self {
            Secondary::Branch(b) => (BRANCH_ICON, b),
            Secondary::Detail(d) => ("", d),
        }
    }
}

/// What a row is about, for click-through: its task (opens the task detail)
/// or its workspace (focuses the workspace).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RowTarget {
    Task(String),
    Workspace(String),
}

/// One entry in the built feed.
// Rows are the common case and a feed is built once per refresh, so a boxed
// row would only add an allocation per row.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FeedItem {
    /// A day header: `Today`, `Yesterday`, `Monday, July 20`.
    Day(String),
    Row(FeedRow),
}

/// The built feed, newest first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Feed {
    pub items: Vec<FeedItem>,
    /// The placeholder to show instead when there are no rows.
    pub empty: Option<&'static str>,
}

impl Feed {
    /// The rows, without the day headers.
    pub fn rows(&self) -> impl Iterator<Item = &FeedRow> {
        self.items.iter().filter_map(|i| match i {
            FeedItem::Row(r) => Some(r),
            FeedItem::Day(_) => None,
        })
    }
}

/// Shown when the log has no events at all.
pub const EMPTY_FEED: &str = "no activity yet — waiting for the first event";
/// Shown when events exist but none pass the filter.
pub const EMPTY_FILTER: &str = "no events match this filter";

impl FeedRow {
    /// A row with the common defaults: no chip, dim verb, bright title, no
    /// trail, no pill, no secondary, no target.
    fn new(verb: impl Into<String>, title: impl Into<String>, time: String) -> Self {
        FeedRow {
            chip: None,
            verb: verb.into(),
            verb_tone: VERB_TONE,
            title: title.into(),
            title_tone: Tone::White,
            trail: None,
            pill: None,
            time,
            secondary: None,
            target: None,
        }
    }
}

/// The dim verb: visible but subordinate to the bright title.
const VERB_TONE: Tone = Tone::Gray;

/// Build the feed from `events` (oldest → newest, file order): newest first,
/// filtered, with a day header whenever the local-time day changes.
/// Consecutive system events sharing a fold key (kind + target + status) on
/// the same day collapse into one row, so a flapping infra loop (an offline
/// host's ssh reverse-forward retrying every couple of minutes) can't bury the
/// real activity below it. `now` drives the relative times and the
/// Today / Yesterday labels.
pub fn build_feed(
    events: &[Event],
    cache: &TaskCache,
    filter: &ActivityFilter,
    now: DateTime<Utc>,
) -> Feed {
    if events.is_empty() {
        return Feed {
            items: Vec::new(),
            empty: Some(EMPTY_FEED),
        };
    }
    let order: Vec<usize> = (0..events.len())
        .rev()
        .filter(|&i| filter.matches(&events[i], cache))
        .collect();
    if order.is_empty() {
        return Feed {
            items: Vec::new(),
            empty: Some(EMPTY_FILTER),
        };
    }

    let today = now.with_timezone(&Local).date_naive();
    let yesterday = today.pred_opt();
    let local_day = |ev: &Event| ev.ts().map(|t| t.with_timezone(&Local).date_naive());
    let mut items = Vec::new();
    let mut last_day: Option<Option<NaiveDate>> = None;
    let mut pos = 0;
    while pos < order.len() {
        let idx = order[pos];
        let ev = &events[idx];
        let day = local_day(ev);
        if last_day != Some(day) {
            items.push(FeedItem::Day(day_label(day, today, yesterday)));
            last_day = Some(day);
        }

        if let Event::System(sys) = ev {
            // How many following events fold into this one: same key, same
            // day, contiguous in the filtered stream. `order` is newest-first,
            // so the run's first element carries the "last seen" time.
            let key = sys.fold_key();
            let mut run_end = pos + 1;
            while run_end < order.len() {
                match &events[order[run_end]] {
                    Event::System(s2)
                        if s2.fold_key() == key && local_day(&events[order[run_end]]) == day =>
                    {
                        run_end += 1
                    }
                    _ => break,
                }
            }
            items.push(FeedItem::Row(system_row(sys, cache, now, run_end - pos)));
            pos = run_end;
            continue;
        }

        // A review handoff (in_progress → review) joins its prior
        // `* -> in_progress` partner for the "took Xm" prefix.
        let started = match ev {
            Event::Issue { id, to, .. } if *to == Column::review() => started_at(events, idx, id),
            _ => None,
        };
        if let Some(row) = event_row(ev, cache, now, started) {
            items.push(FeedItem::Row(row));
        }
        pos += 1;
    }
    Feed { items, empty: None }
}

fn day_label(day: Option<NaiveDate>, today: NaiveDate, yesterday: Option<NaiveDate>) -> String {
    match day {
        Some(d) if d == today => "Today".to_string(),
        Some(d) if yesterday == Some(d) => "Yesterday".to_string(),
        Some(d) => d.format("%A, %B %-d").to_string(),
        None => "—".to_string(),
    }
}

/// The matching `* -> in_progress` event before `idx` for the same task: its
/// timestamp, so the review handoff at `idx` can show "took Xm".
pub fn started_at(events: &[Event], idx: usize, task_id: &str) -> Option<DateTime<Utc>> {
    events[..idx].iter().rev().find_map(|e| match e {
        Event::Issue { id, to, ts, .. } if id == task_id && *to == Column::in_progress() => {
            Some(*ts)
        }
        _ => None,
    })
}

/// The latest known metadata for a task: a pure cache lookup that never
/// touches the store (that is [`resolve_task_meta_into`]'s job, on the refresh
/// cadence). `None` for a not-yet-resolved id, a confirmed miss, or a deleted
/// task, so callers fall back to the id as the label.
pub fn task_meta<'a>(cache: &'a TaskCache, id: &str) -> Option<&'a TaskMeta> {
    cache.get(id).and_then(|e| e.meta.as_ref())
}

/// The row for one (unfolded) event, or `None` for a heartbeat. Folded system
/// runs come from [`build_feed`] calling [`system_row`] with a count.
pub fn event_row(
    ev: &Event,
    cache: &TaskCache,
    now: DateTime<Utc>,
    started: Option<DateTime<Utc>>,
) -> Option<FeedRow> {
    Some(match ev {
        Event::Issue {
            ts,
            id,
            from,
            to,
            reason,
            ..
        } => task_row(cache, *ts, id, from, to, reason, now, started),
        Event::Workspace { ts, name, new, .. } => workspace_row(*ts, name, *new, now),
        Event::ZenDryRun {
            ts,
            task_id,
            action,
            detail,
            ..
        } => dryrun_row(cache, *ts, task_id, action, detail, now),
        // Never shown: `ActivityFilter::matches` rejects heartbeats.
        Event::Heartbeat { .. } => return None,
        Event::System(sys) => system_row(sys, cache, now, 1),
        Event::Unknown { ts, raw } => {
            let when = ts.map(|t| relative_time(t, now)).unwrap_or_default();
            // Strip the leading ISO timestamp so the row never shows raw wire
            // syntax with a full timestamp; the relative time is the row's only
            // clock. A line with no parseable timestamp shows verbatim.
            let body = if ts.is_some() {
                raw.split_once(' ').map(|x| x.1).unwrap_or(raw).to_string()
            } else {
                raw.to_string()
            };
            let mut row = FeedRow::new("", body, when);
            row.chip = system_chip("event");
            row.title_tone = Tone::DarkGray;
            row
        }
    })
}

/// A chip in `tone`; an empty label yields no chip at all.
fn identity_chip(label: &str, tone: Tone, bold: bool) -> Option<Chip> {
    (!label.is_empty()).then(|| Chip {
        label: label.to_string(),
        tone,
        bold,
    })
}

/// An agent-attributed row's chip: the workspace in its tint, bold.
fn agent_chip(name: &str, tone: Tone) -> Option<Chip> {
    identity_chip(name, tone, true)
}

/// A machine-driven or board-level row's chip: a dimmed neutral label (`you`,
/// `zen`, a host, a workspace ping, ...).
fn system_chip(label: &str) -> Option<Chip> {
    identity_chip(label, Tone::DarkGray, false)
}

/// The state pill for a task landing in `category`: the human column name in
/// the canonical category tint.
fn category_pill(category: StatusCategory) -> Pill {
    let text = match category {
        StatusCategory::Backlog => "backlog",
        StatusCategory::Ready => "ready",
        StatusCategory::Active => "in progress",
        StatusCategory::Handoff => "ready for review",
        StatusCategory::Done => "done",
        StatusCategory::Archived => "archived",
    };
    Pill {
        text: text.to_string(),
        tone: category_tone(category),
    }
}

/// A task's workspace as a chip name + tint. Unassigned tasks fall back to
/// `orchestrator` in gray.
fn agent_display(workspace: Option<&str>) -> (String, Tone) {
    match workspace {
        Some(w) => (w.to_string(), agent_tone(w)),
        None => ("orchestrator".to_string(), Tone::Gray),
    }
}

/// The row for a system / infra event. `count` is how many consecutive
/// near-duplicates it stands for (1 = a lone event); above one the row gains a
/// dim `(N attempts)` / `(×N)` trail and its time reads as the most recent
/// occurrence (`last 4m`).
pub fn system_row(
    sys: &SystemEvent,
    cache: &TaskCache,
    now: DateTime<Utc>,
    count: usize,
) -> FeedRow {
    let when = relative_time(sys.ts, now);
    let status = sys.status.as_deref();
    let title_of = |id: Option<&str>| system_task_title(cache, id);

    // Per-kind chip / verb / verb tone / title. The chip is the left-hand
    // subject (a tinted workspace where one applies, else a dimmed neutral
    // label); the verb carries the status tint; the title is the summary.
    // System rows carry no state pill: the verb tone is their signal.
    let (chip, verb, verb_tone, title): (Option<Chip>, &str, Tone, String) = match sys.kind {
        SystemKind::Ssh => {
            let host = sys.target.as_deref().unwrap_or("host");
            let (verb, tone) = match status {
                Some("failed") => ("unreachable", Tone::LightRed),
                Some("established") => ("connected", Tone::Green),
                _ => ("ssh", Tone::Gray),
            };
            let title = sys
                .detail
                .clone()
                .unwrap_or_else(|| "reverse-forward".to_string());
            (system_chip(host), verb, tone, title)
        }
        SystemKind::Dispatch => {
            let (name, tone) = agent_display(sys.detail.as_deref());
            (
                agent_chip(&name, tone),
                "dispatched",
                category_tone(StatusCategory::Active),
                title_of(sys.target.as_deref()),
            )
        }
        SystemKind::Rebase => {
            let (name, tone) = agent_display(sys.detail.as_deref());
            (
                agent_chip(&name, tone),
                "rebased",
                rebase_status_tone(status),
                title_of(sys.target.as_deref()),
            )
        }
        SystemKind::Push => (
            system_chip("push"),
            "pushed",
            push_status_tone(status),
            title_of(sys.target.as_deref()),
        ),
        SystemKind::WorktreeDetach => {
            let (name, tone) = agent_display(sys.detail.as_deref());
            (
                agent_chip(&name, tone),
                "detached",
                if status == Some("ok") {
                    Tone::Gray
                } else {
                    Tone::LightRed
                },
                title_of(sys.target.as_deref()),
            )
        }
        SystemKind::Merge => (
            system_chip("merge"),
            "merged",
            if status == Some("ok") {
                Tone::Green
            } else {
                Tone::LightRed
            },
            title_of(sys.target.as_deref()),
        ),
        SystemKind::Send => {
            let (name, tone) = agent_display(sys.target.as_deref());
            (
                agent_chip(&name, tone),
                "sent",
                if status == Some("stuck") {
                    Tone::Yellow
                } else {
                    Tone::Blue
                },
                send_title(status),
            )
        }
        SystemKind::Message => {
            let verb = if sys.detail.as_deref() == Some("ack") {
                "ack"
            } else {
                "delivered"
            };
            (
                system_chip("message"),
                verb,
                Tone::Blue,
                title_of(sys.target.as_deref()),
            )
        }
        SystemKind::Closed => (
            system_chip(sys.target.as_deref().unwrap_or("project")),
            "closed",
            Tone::Gray,
            sys.detail
                .clone()
                .unwrap_or_else(|| "project closed".into()),
        ),
        SystemKind::Handoff => (
            system_chip(sys.target.as_deref().unwrap_or("orchestrator")),
            "handoff",
            handoff_status_tone(status),
            format!("handoff {}", status.unwrap_or("attempted")),
        ),
        // `[zen] Zen mode turned off` reads as a plain sentence: no verb
        // column, the status carries into the phrase itself.
        SystemKind::Mode => (
            system_chip("zen"),
            "",
            Tone::Green,
            format!("Zen mode turned {}", status.unwrap_or("changed")),
        ),
        SystemKind::Supervision => (
            system_chip(sys.target.as_deref().unwrap_or("orchestrator")),
            "supervisor",
            if status == Some("gave-up") {
                Tone::LightRed
            } else {
                Tone::Yellow
            },
            humanize_token(status.unwrap_or("supervised")),
        ),
        SystemKind::PaneDeath => {
            let alive = status == Some("true");
            (
                system_chip(sys.target.as_deref().unwrap_or("pane")),
                "pane",
                if alive { Tone::Gray } else { Tone::LightRed },
                if alive { "pane back" } else { "pane died" }.to_string(),
            )
        }
        SystemKind::Ci => {
            let ok = status == Some("success");
            (
                system_chip("ci"),
                "CI",
                if ok { Tone::Green } else { Tone::LightRed },
                sys.detail
                    .clone()
                    .map(|d| format!("{d} {}", if ok { "passed" } else { "failed" }))
                    .unwrap_or_else(|| format!("CI {}", status.unwrap_or("reported"))),
            )
        }
        SystemKind::Other => (
            system_chip(sys.target.as_deref().unwrap_or("")),
            "event",
            Tone::Gray,
            sys.detail.clone().unwrap_or_else(|| "event".into()),
        ),
    };

    // Fold trail + last-seen time; ssh reads more naturally as "attempts".
    let (trail, time) = if count > 1 {
        let label = if matches!(sys.kind, SystemKind::Ssh) {
            format!(" ({count} attempts)")
        } else {
            format!(" (×{count})")
        };
        (Some((label, Tone::DarkGray)), format!("last {when}"))
    } else {
        (None, when)
    };

    let mut row = FeedRow::new(verb, title, time);
    row.chip = chip;
    row.verb_tone = verb_tone;
    row.trail = trail;
    // The volatile detail already fed the title for most kinds; only add it as
    // a secondary where it isn't the title.
    row.secondary = detail_secondary(sys).map(Secondary::Detail);
    row.target = system_target(sys);
    row
}

/// What a system row opens: the task it is about, else the workspace.
fn system_target(sys: &SystemEvent) -> Option<RowTarget> {
    let task = |t: &Option<String>| t.clone().map(RowTarget::Task);
    match sys.kind {
        SystemKind::Dispatch
        | SystemKind::Rebase
        | SystemKind::Push
        | SystemKind::WorktreeDetach
        | SystemKind::Merge
        | SystemKind::Message
        | SystemKind::Ci => task(&sys.target),
        SystemKind::Send | SystemKind::PaneDeath => sys.target.clone().map(RowTarget::Workspace),
        SystemKind::Supervision => sys
            .target
            .clone()
            .filter(|t| t != "orchestrator")
            .map(RowTarget::Workspace),
        SystemKind::Ssh
        | SystemKind::Closed
        | SystemKind::Handoff
        | SystemKind::Mode
        | SystemKind::Other => None,
    }
}

/// The dim second line for a system row, when its detail isn't already the
/// title. Task-scoped rows (dispatch / rebase / detach / push / merge) show the
/// status token under the task title; others fold their detail into the title.
fn detail_secondary(sys: &SystemEvent) -> Option<String> {
    match sys.kind {
        SystemKind::Dispatch | SystemKind::Rebase | SystemKind::WorktreeDetach => {
            sys.status.as_deref().map(humanize_token)
        }
        // The status token plus the reason / range tail, so a blocked push or
        // a refused merge shows *why*, not just that it didn't land.
        SystemKind::Push | SystemKind::Merge => {
            let status = sys.status.as_deref().map(humanize_token);
            match (status, sys.detail.as_deref()) {
                (Some(s), Some(reason)) => Some(format!("{s}: {reason}")),
                (Some(s), None) => Some(s),
                (None, Some(reason)) => Some(reason.to_string()),
                (None, None) => None,
            }
        }
        _ => None,
    }
}

/// The task id an event references for title resolution, or `None` for an
/// event that names no task. Mirrors the rows' own choice of which system
/// kinds carry a task id in `target`, so [`resolve_task_meta_into`]
/// batch-resolves exactly the ids the feed will look up.
fn event_task_id(ev: &Event) -> Option<&str> {
    match ev {
        Event::Issue { id, .. } => Some(id.as_str()),
        Event::ZenDryRun { task_id, .. } => Some(task_id.as_str()),
        Event::System(sys) => match sys.kind {
            SystemKind::Dispatch
            | SystemKind::Rebase
            | SystemKind::Push
            | SystemKind::WorktreeDetach
            | SystemKind::Merge
            | SystemKind::Message => sys.target.as_deref(),
            _ => None,
        },
        _ => None,
    }
}

/// The task file's modification time on the filesystem backend, or `None`
/// when there is no local file (a remote backend: the TTL path takes over) or
/// the id/path can't be resolved.
fn task_file_mtime(project: &str, id: &str) -> Option<SystemTime> {
    let path = shelbi_state::task_path(project, id).ok()?;
    fs::metadata(&path).ok().and_then(|m| m.modified().ok())
}

/// A task id's human title from the cache, falling back to the id (or a
/// neutral placeholder).
fn system_task_title(cache: &TaskCache, task_id: Option<&str>) -> String {
    match task_id {
        Some(id) => task_meta(cache, id)
            .map(|m| m.title.clone())
            .unwrap_or_else(|| id.to_string()),
        None => "task".to_string(),
    }
}

fn send_title(status: Option<&str>) -> String {
    match status {
        Some("submitted") | Some("queued") => "message delivered".to_string(),
        Some("stuck") => "delivery stuck".to_string(),
        Some("unverified") => "delivery unverified".to_string(),
        Some(s) => format!("message {s}"),
        None => "message sent".to_string(),
    }
}

fn rebase_status_tone(status: Option<&str>) -> Tone {
    match status {
        Some("conflict") => Tone::LightRed,
        Some("succeeded") => Tone::Green,
        _ => Tone::Gray,
    }
}

/// Tint for a handoff-`push` row. `blocked` (a force refused over foreign
/// commits, or the retry cap hit: a human must step in) is the loud red;
/// `failed` (a transient reject still retrying) is yellow; everything that
/// handed the tip off reads calm (green for a real push, gray for a no-op).
fn push_status_tone(status: Option<&str>) -> Tone {
    match status {
        Some("blocked") => Tone::LightRed,
        Some("failed") => Tone::Yellow,
        Some("pushed") | Some("force-pushed") => Tone::Green,
        _ => Tone::Gray,
    }
}

fn handoff_status_tone(status: Option<&str>) -> Tone {
    match status {
        Some("written") | Some("native-thread") => Tone::Green,
        Some("timeout") | Some("send-failed") | Some("unconfirmed") => Tone::Yellow,
        _ => Tone::Gray,
    }
}

/// A `[DRYRUN]` row for a `shelbi zen dry-run` preview: a yellow chip so the
/// row reads as what Zen *would* have done, visibly not a real move. The label
/// is spelled out (not just a glyph) so a grep over a screenshot finds it.
fn dryrun_row(
    cache: &TaskCache,
    ts: DateTime<Utc>,
    task_id: &str,
    action: &str,
    detail: &str,
    now: DateTime<Utc>,
) -> FeedRow {
    let title = task_meta(cache, task_id)
        .map(|m| m.title.clone())
        .unwrap_or_else(|| task_id.to_string());
    let mut row = FeedRow::new(
        format!("would {}", humanize_dryrun_action(action)),
        title,
        relative_time(ts, now),
    );
    row.chip = identity_chip("DRYRUN", Tone::Yellow, true);
    let detail = detail.replace('_', " ");
    if !detail.is_empty() {
        row.secondary = Some(Secondary::Detail(detail));
    }
    row.target = Some(RowTarget::Task(task_id.to_string()));
    row
}

fn humanize_dryrun_action(action: &str) -> String {
    match action {
        "consider-auto-promote" => "consider promoting".into(),
        "merge" => "merge".into(),
        "block-merge" => "block merge".into(),
        other => other.replace('-', " "),
    }
}

/// The row for a task transition.
#[allow(clippy::too_many_arguments)]
fn task_row(
    cache: &TaskCache,
    ts: DateTime<Utc>,
    id: &str,
    from: &Column,
    to: &Column,
    reason: &str,
    now: DateTime<Utc>,
    started: Option<DateTime<Utc>>,
) -> FeedRow {
    let meta = task_meta(cache, id);
    let title = meta
        .map(|m| m.title.clone())
        .unwrap_or_else(|| id.to_string());
    let branch = meta.and_then(|m| m.branch.clone());
    let workspace = meta.and_then(|m| m.assigned_to.clone());
    let when = relative_time(ts, now);

    // Zen-driven events win over the (from, to) rows so machine-driven moves
    // scan distinctly.
    let mut row = if let Some(zr) = parse_zen_reason(reason) {
        zen_row(zr, &title, from, to, when)
    } else {
        // Match on the canonical status ids so the rows stay a fixed vocabulary
        // for the stock flow; any other transition falls through to "moved".
        // Every arm carries a state pill naming where the task landed.
        let row = match (from.as_str(), to.as_str()) {
            ("backlog", "todo") => {
                // A board-level promote: the human's own action, so `[you]`.
                let mut row = FeedRow::new("promoted", title, when);
                row.chip = system_chip("you");
                row.secondary = Some(Secondary::Detail("backlog → todo".to_string()));
                row
            }
            ("todo", "in-progress") => {
                let (name, tone) = agent_display(workspace.as_deref());
                let mut row = FeedRow::new("is building", title, when);
                row.chip = agent_chip(&name, tone);
                row.secondary = branch.map(Secondary::Branch);
                row
            }
            ("in-progress", "review") => {
                let (name, tone) = agent_display(workspace.as_deref());
                // The started → review pairing: `took Xm · ` before the time.
                let took = started
                    .map(|s| format!("took {} · ", short_duration(ts - s)))
                    .unwrap_or_default();
                let mut row = FeedRow::new("finished", title, format!("{took}{when}"));
                row.chip = agent_chip(&name, tone);
                row.secondary = branch.map(Secondary::Branch);
                row
            }
            ("review", "done") => {
                // A board-level acceptance / merge: the human's board action.
                let mut row = FeedRow::new("merged", title, when);
                row.chip = system_chip("you");
                row.secondary = Some(Secondary::Detail("moved to done".to_string()));
                row
            }
            _ => {
                // An unrecognized transition: a clean "moved" row with the
                // from → to on the dim second line.
                let mut row = FeedRow::new("moved", title, when);
                row.chip = system_chip("you");
                row.secondary = Some(Secondary::Detail(format!("{from} → {to}")));
                row
            }
        };
        row.with_pill(category_pill(to.category()))
    };
    row.target = Some(RowTarget::Task(id.to_string()));
    row
}

impl FeedRow {
    fn with_pill(mut self, pill: Pill) -> Self {
        self.pill = Some(pill);
        self
    }
}

/// A Zen-driven transition. Every zen row is machine-driven, so it gets the
/// untinted `zen` chip (no agent identity), a verb, and a `·`-joined detail on
/// the dim second line. Bail reasons add a short colored tag after the title
/// and carry no state pill (the tag, not a "success" pill, is their outcome).
fn zen_row(zr: ZenReason, title: &str, from: &Column, to: &Column, when: String) -> FeedRow {
    let bail = |text: &str, tone: Tone| Some((text.to_string(), tone));
    #[allow(clippy::type_complexity)]
    let (verb, trail, secondary, pill): (
        &str,
        Option<(String, Tone)>,
        Option<String>,
        Option<Pill>,
    ) = match &zr {
        ZenReason::Promote { .. } => (
            "promoted",
            None,
            Some("backlog → todo".to_string()),
            Some(category_pill(to.category())),
        ),
        ZenReason::Merge { pr, sha } => {
            // What merged: the PR number first, falling back to a merge SHA,
            // then to a bare "merged" for historical lines with neither.
            let merged = pr
                .as_deref()
                .map(|n| format!("merged #{n}"))
                .or_else(|| sha.as_deref().map(|s| format!("merged {s}")))
                .unwrap_or_else(|| "merged".to_string());
            (
                "merged",
                None,
                Some(join_detail(&["tests green", "ci green", &merged])),
                Some(category_pill(StatusCategory::Done)),
            )
        }
        ZenReason::FailedChecks { cmd, exit } => {
            let parts: Vec<String> = [
                cmd.as_ref().map(|c| format!("`{c}`")),
                exit.as_ref().map(|e| format!("exit {e}")),
            ]
            .into_iter()
            .flatten()
            .collect();
            (
                "bailed on",
                bail(" — checks failed", Tone::LightRed),
                (!parts.is_empty()).then(|| parts.join(" · ")),
                None,
            )
        }
        ZenReason::DiffTooLarge { files, lines } => {
            let parts: Vec<String> = [
                files.as_ref().map(|f| format!("{f} files")),
                lines.as_ref().map(|l| format!("{l} lines")),
            ]
            .into_iter()
            .flatten()
            .collect();
            (
                "bailed on",
                bail(" — diff too large", Tone::Yellow),
                (!parts.is_empty()).then(|| parts.join(" · ")),
                None,
            )
        }
        ZenReason::DangerPath { paths } => (
            "bailed on",
            bail(" — danger path", Tone::Yellow),
            paths
                .as_ref()
                .map(|p| format!("touched: {}", humanize_list(p))),
            None,
        ),
        ZenReason::CiTimeout { duration } => (
            "bailed on",
            bail(" — ci timeout", Tone::Yellow),
            duration.as_ref().map(|d| format!("ci timeout after {d}")),
            None,
        ),
        ZenReason::MergeConflict { files } => (
            "bailed on",
            bail(" — merge conflict", Tone::Yellow),
            files
                .as_ref()
                .map(|f| format!("conflict in {}", humanize_list(f))),
            None,
        ),
        ZenReason::Other => (
            "moved",
            None,
            Some(format!("{from} → {to}")),
            Some(category_pill(to.category())),
        ),
    };
    let mut row = FeedRow::new(verb, title.to_string(), when);
    row.chip = system_chip("zen");
    row.trail = trail;
    row.pill = pill;
    row.secondary = secondary.map(Secondary::Detail);
    row
}

/// Comma list → human list: `"a,b,c"` → `"a, b, c"`.
fn humanize_list(s: &str) -> String {
    s.split(',')
        .map(|p| p.trim())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// A workspace state change. These pings are noisier than task transitions
/// and aren't a single agent action, so they read as machine-driven: a dimmed
/// `[name]` chip and a bare verb phrase, no title. The verb takes the state's
/// tone so `awaiting input` reads blue.
fn workspace_row(
    ts: DateTime<Utc>,
    name: &str,
    new: WorkspaceState,
    now: DateTime<Utc>,
) -> FeedRow {
    let (verb, tone, detail) = match new {
        WorkspaceState::Working => ("working", Tone::DarkGray, None),
        WorkspaceState::AwaitingInput => ("awaiting input", Tone::Blue, None),
        WorkspaceState::Blocked => ("blocked", Tone::Yellow, Some("needs human approval")),
        WorkspaceState::Paused => (
            "paused",
            Tone::DarkGray,
            Some("waiting for the limit to reset"),
        ),
        WorkspaceState::Serving => ("serving", Tone::Cyan, Some("branch is up for review")),
    };
    let mut row = FeedRow::new(verb, String::new(), relative_time(ts, now));
    row.chip = system_chip(name);
    row.verb_tone = tone;
    row.secondary = detail.map(|d| Secondary::Detail(d.to_string()));
    row.target = Some(RowTarget::Workspace(name.to_string()));
    row
}

// ---------------------------------------------------------------------------
// Formatting
// ---------------------------------------------------------------------------

/// Compact relative time — `"now"`, `"4m"`, `"2h"`, `"3d"`, then a short
/// date for anything older than a week. Never an ISO timestamp. Returns an
/// empty string for events in the future (clock skew) or with no timestamp.
pub fn relative_time(ts: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let delta = now - ts;
    if delta.num_seconds() < 0 {
        return String::new();
    }
    if delta.num_seconds() < 60 {
        return "now".into();
    }
    if delta.num_minutes() < 60 {
        return format!("{}m", delta.num_minutes());
    }
    if delta.num_hours() < 24 {
        return format!("{}h", delta.num_hours());
    }
    if delta.num_days() < 7 {
        return format!("{}d", delta.num_days());
    }
    let local = ts.with_timezone(&Local);
    if local.year() == Local::now().year() {
        local.format("%b %-d").to_string()
    } else {
        // "Jan 5 2025" — a human date, deliberately not an ISO timestamp.
        local.format("%b %-d %Y").to_string()
    }
}

pub fn short_duration(d: chrono::Duration) -> String {
    let secs = d.num_seconds().abs();
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    let rem = mins % 60;
    if rem == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{rem:02}m")
    }
}

/// Join a list of detail fragments with ` · ` separators, skipping
/// empties so a missing branch or priority doesn't leave a double
/// separator behind.
fn join_detail(parts: &[&str]) -> String {
    parts
        .iter()
        .filter(|p| !p.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// Serializes the tests that point `SHELBI_HOME` at a temp dir and stub
    /// `gh` (both process-global), with every other such test in the crate.
    use crate::test_env::ENV_LOCK;

    fn no_cache() -> TaskCache {
        TaskCache::new()
    }

    fn kinds(k: &[ActivityKind]) -> ActivityFilter {
        let mut f = ActivityFilter::default();
        for kind in k {
            f.toggle_kind(*kind);
        }
        f
    }

    #[test]
    fn parses_legacy_task_event_with_defaults() {
        // Pre-workflow line shape (`Plans/workflows.md` §10). The
        // back-compat parser must fill `workflow=default` and derive
        // categories from the canonical 5-status map — that's what the
        // orchestrator's reaction rules now key off, so old lines have to
        // come through with the same shape new ones do.
        let line =
            "2026-06-23T04:19:33.715717+00:00 task=foo todo -> in_progress reason=user:cli:start";
        match parse_event_line(line) {
            Event::Issue {
                id,
                workflow,
                from,
                to,
                reason,
                from_category,
                to_category,
                ..
            } => {
                assert_eq!(id, "foo");
                assert_eq!(workflow, "default");
                assert_eq!(from, Column::todo());
                assert_eq!(to, Column::in_progress());
                assert_eq!(reason, "user:cli:start");
                assert_eq!(from_category, StatusCategory::Ready);
                assert_eq!(to_category, StatusCategory::Active);
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn parses_workflow_aware_task_event() {
        // Full shape from §10:
        // `<ts> task=<id> workflow=<name> <from> -> <to> reason=<r>
        //  from_category=<c> to_category=<c>`
        let line = "2026-06-23T04:19:33+00:00 task=ship-it workflow=feature-task \
                    in_progress -> review reason=workspace:ready-marker \
                    from_category=active to_category=handoff";
        match parse_event_line(line) {
            Event::Issue {
                id,
                workflow,
                from,
                to,
                reason,
                from_category,
                to_category,
                ..
            } => {
                assert_eq!(id, "ship-it");
                assert_eq!(workflow, "feature-task");
                assert_eq!(from, Column::in_progress());
                assert_eq!(to, Column::review());
                assert_eq!(reason, "workspace:ready-marker");
                assert_eq!(from_category, StatusCategory::Active);
                assert_eq!(to_category, StatusCategory::Handoff);
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn workflow_aware_parser_tolerates_missing_category_fields() {
        // A future writer that drops the category annotations (or an
        // intermediate-format line) must still parse — the parser falls
        // back to deriving them from the canonical column map.
        let line = "2026-06-23T04:19:33+00:00 task=foo workflow=default todo -> in_progress reason=user:cli";
        match parse_event_line(line) {
            Event::Issue {
                workflow,
                from_category,
                to_category,
                ..
            } => {
                assert_eq!(workflow, "default");
                assert_eq!(from_category, StatusCategory::Ready);
                assert_eq!(to_category, StatusCategory::Active);
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn task_event_parser_extracts_agent_from_dispatch_reason() {
        // `shelbi task start` writes the resolved agent into `reason=` as
        // a `_agent=<name>` segment. The parser exposes it on the event
        // struct so the activity feed can render an inline `[<agent>]`
        // tag without re-parsing the line.
        let line = "2026-06-23T04:19:33+00:00 task=foo workflow=default \
                    todo -> in_progress \
                    reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer \
                    from_category=ready to_category=active";
        match parse_event_line(line) {
            Event::Issue { agent, reason, .. } => {
                assert_eq!(agent.as_deref(), Some("developer"));
                // Reason value is preserved verbatim; the agent field is
                // a parsed convenience layered on top, not a replacement.
                assert!(reason.contains("agent=developer"), "reason: {reason}");
            }
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn task_event_parser_leaves_agent_none_when_field_absent() {
        // Older lines (and transitions emitted from paths that don't
        // spawn a workspace) have no `_agent=` segment. The parser must
        // leave `agent` as `None` rather than guessing.
        let line = "2026-06-23T04:19:33+00:00 task=foo workflow=default \
                    backlog -> todo reason=user:cli \
                    from_category=backlog to_category=ready";
        match parse_event_line(line) {
            Event::Issue { agent, .. } => assert!(agent.is_none(), "agent: {agent:?}"),
            other => panic!("expected task event, got {other:?}"),
        }
    }

    #[test]
    fn parses_workspace_event() {
        let line = "2026-06-23T04:19:33Z workspace=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Workspace {
                name, prev, new, ..
            } => {
                assert_eq!(name, "alpha");
                assert_eq!(prev, Some(WorkspaceState::Working));
                assert_eq!(new, WorkspaceState::AwaitingInput);
            }
            other => panic!("expected workspace event, got {other:?}"),
        }
    }

    #[test]
    fn parses_first_observation_workspace_event_with_none_prev() {
        let line = "2026-06-23T04:19:33Z workspace=alpha none -> working";
        match parse_event_line(line) {
            Event::Workspace {
                name, prev, new, ..
            } => {
                assert_eq!(name, "alpha");
                assert!(prev.is_none(), "`none` prev must parse as Option::None");
                assert_eq!(new, WorkspaceState::Working);
            }
            other => panic!("expected workspace event, got {other:?}"),
        }
    }

    /// Legacy event-log lines (and any tooling that lags behind the
    /// `worker=` → `workspace=` rename for one release) must still parse
    /// as workspace events. Both forms route to the same `Event::Workspace`
    /// variant — the parser cares about the shape of the rest of the line,
    /// not which prefix introduced it.
    #[test]
    fn parses_legacy_worker_event_form_as_workspace_event() {
        let line = "2026-06-23T04:19:33Z worker=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Workspace {
                name, prev, new, ..
            } => {
                assert_eq!(name, "alpha");
                assert_eq!(prev, Some(WorkspaceState::Working));
                assert_eq!(new, WorkspaceState::AwaitingInput);
            }
            other => panic!("expected workspace event, got {other:?}"),
        }
    }

    #[test]
    fn parses_zen_dryrun_event() {
        let line = "2026-06-24T10:00:00Z zen-dryrun task=fix-typo action=consider-auto-promote detail=mechanically-eligible";
        match parse_event_line(line) {
            Event::ZenDryRun {
                task_id,
                action,
                detail,
                ..
            } => {
                assert_eq!(task_id, "fix-typo");
                assert_eq!(action, "consider-auto-promote");
                assert_eq!(detail, "mechanically-eligible");
            }
            other => panic!("expected zen-dryrun event, got {other:?}"),
        }
    }

    #[test]
    fn zen_dryrun_without_task_or_action_falls_back_to_unknown() {
        // Defensive: a malformed dry-run line (missing the required
        // `task=` and `action=` tokens) must not crash the parser.
        let line = "2026-06-24T10:00:00Z zen-dryrun detail=oops";
        match parse_event_line(line) {
            Event::Unknown { .. } => {}
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn unknown_line_falls_back_to_raw() {
        // A future event shape we don't recognize must still reach the
        // renderer as Unknown — the acceptance criteria require nothing
        // silently dropped.
        let line = "2026-06-23T04:19:33Z something=else";
        match parse_event_line(line) {
            Event::Unknown { ts, raw } => {
                assert!(ts.is_some());
                assert_eq!(raw, line);
            }
            other => panic!("expected unknown event, got {other:?}"),
        }
    }

    #[test]
    fn malformed_line_with_no_timestamp_is_unknown() {
        let line = "not even a timestamp";
        match parse_event_line(line) {
            Event::Unknown { ts, raw } => {
                assert!(ts.is_none());
                assert_eq!(raw, line);
            }
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn parses_project_scoped_task_transition() {
        // The real on-disk shape carries a leading `project=<name>` scope.
        // It must route to a first-class Issue event (the review handoff the
        // feed centers on), not fall into Unknown as raw wire syntax.
        let line = "2026-07-22T14:00:00+00:00 project=shelbi task=foo workflow=app \
                    in_progress -> review reason=workspace:ready-marker \
                    from_category=active to_category=handoff";
        match parse_event_line(line) {
            Event::Issue { id, from, to, .. } => {
                assert_eq!(id, "foo");
                assert_eq!(from, Column::in_progress());
                assert_eq!(to, Column::review());
            }
            other => panic!("expected Issue, got {other:?}"),
        }
    }

    #[test]
    fn parses_project_scoped_workspace_transition() {
        let line =
            "2026-07-22T14:00:00+00:00 project=shelbi workspace=alpha working -> awaiting_input";
        match parse_event_line(line) {
            Event::Workspace { name, new, .. } => {
                assert_eq!(name, "alpha");
                assert_eq!(new, WorkspaceState::AwaitingInput);
            }
            other => panic!("expected Workspace, got {other:?}"),
        }
    }

    #[test]
    fn parses_paused_workspace_transition() {
        // `-> paused` (usage-limit) must parse — previously `paused` was not
        // in the state map, so these lines fell through to Unknown.
        let line = "2026-07-22T14:00:00+00:00 project=demo workspace=alpha working -> paused reason=usage-limit";
        match parse_event_line(line) {
            Event::Workspace { new, .. } => assert_eq!(new, WorkspaceState::Paused),
            other => panic!("expected Workspace paused, got {other:?}"),
        }
    }

    #[test]
    fn parses_ssh_reverse_forward_event() {
        let line = "2026-07-22T14:26:16.430059+00:00 ssh reverse-forward \
                    host=devbox mode=tcp status=failed detail=master_open_failed";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::Ssh);
                assert_eq!(sys.target.as_deref(), Some("devbox"));
                assert_eq!(sys.status.as_deref(), Some("failed"));
                assert_eq!(sys.detail.as_deref(), Some("master open failed"));
            }
            other => panic!("expected System(Ssh), got {other:?}"),
        }
    }

    #[test]
    fn parses_the_required_infra_kinds_without_unknown_fallback() {
        // Acceptance — ssh/dispatch/rebase/push/worktree-detach/closed/handoff
        // each classify to a structured System event, never Unknown.
        let cases: &[(&str, SystemKind, Option<&str>, Option<&str>)] = &[
            (
                "2026-07-22T14:00:00+00:00 dispatch task=t workspace=alpha status=confirmed detail=busy_observed",
                SystemKind::Dispatch,
                Some("t"),
                Some("confirmed"),
            ),
            (
                "2026-07-22T14:00:00+00:00 rebase task=t workspace=alpha branch=b status=up-to-date detail=default=abc",
                SystemKind::Rebase,
                Some("t"),
                Some("up-to-date"),
            ),
            (
                "2026-07-22T14:00:00+00:00 push task=t workspace=alpha branch=b status=blocked detail=remote_has_unrebased_commits",
                SystemKind::Push,
                Some("t"),
                Some("blocked"),
            ),
            (
                "2026-07-22T14:00:00+00:00 worktree-detach task=t workspace=alpha detached-from=b status=ok",
                SystemKind::WorktreeDetach,
                Some("t"),
                Some("ok"),
            ),
            (
                "2026-07-22T14:00:00+00:00 merge task=t workspace=alpha base=main status=failed detail=not_mergeable",
                SystemKind::Merge,
                Some("t"),
                Some("failed"),
            ),
            (
                "2026-07-22T14:00:00+00:00 project=shelbi closed reason=user:quit-shelbi",
                SystemKind::Closed,
                Some("shelbi"),
                None,
            ),
            (
                "2026-07-22T14:00:00+00:00 project=shelbi handoff outcome=written detail=/tmp/h.md",
                SystemKind::Handoff,
                Some("shelbi"),
                Some("written"),
            ),
            (
                "2026-07-22T14:00:00+00:00 project=shelbi task=t ci pr=42 check=all conclusion=success head_sha=deadbeef",
                SystemKind::Ci,
                Some("t"),
                Some("success"),
            ),
            (
                "2026-07-22T14:00:00+00:00 project=shelbi task=t ci pr=42 check=build conclusion=failure head_sha=deadbeef",
                SystemKind::Ci,
                Some("t"),
                Some("failure"),
            ),
        ];
        for (line, kind, target, status) in cases {
            match parse_event_line(line) {
                Event::System(sys) => {
                    assert_eq!(sys.kind, *kind, "kind for {line}");
                    assert_eq!(sys.target.as_deref(), *target, "target for {line}");
                    assert_eq!(sys.status.as_deref(), *status, "status for {line}");
                }
                other => panic!("expected System for {line}, got {other:?}"),
            }
        }
    }

    #[test]
    fn parses_pane_death_event() {
        let line = "2026-07-22T14:00:00+00:00 project=demo workspace=alpha pane_alive=false reason=worktree-missing";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::PaneDeath);
                assert_eq!(sys.target.as_deref(), Some("alpha"));
                assert_eq!(sys.status.as_deref(), Some("false"));
            }
            other => panic!("expected System(PaneDeath), got {other:?}"),
        }
    }

    #[test]
    fn parses_send_event_reading_project_from_field() {
        // `send` keeps its `project=` as a later field, not the leading
        // scope, so the parser reads it back out of the k=v tail.
        let line = "2026-07-22T14:00:00+00:00 send project=demo workspace=alpha status=submitted detail=busy_observed";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::Send);
                assert_eq!(sys.project.as_deref(), Some("demo"));
                assert_eq!(sys.target.as_deref(), Some("alpha"));
                assert_eq!(sys.status.as_deref(), Some("submitted"));
            }
            other => panic!("expected System(Send), got {other:?}"),
        }
    }

    #[test]
    fn parses_dispatch_event_reading_project_from_field() {
        // `dispatch` now carries `project=` as a later field (like `send`), so
        // the parser reads it back out of the k=v tail onto the event's scope.
        let line = "2026-07-22T14:00:00+00:00 dispatch project=demo task=t workspace=alpha status=confirmed detail=busy_observed";
        match parse_event_line(line) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::Dispatch);
                assert_eq!(sys.project.as_deref(), Some("demo"));
                assert_eq!(sys.target.as_deref(), Some("t"));
                assert_eq!(sys.status.as_deref(), Some("confirmed"));
                assert_eq!(sys.detail.as_deref(), Some("alpha"));
            }
            other => panic!("expected System(Dispatch), got {other:?}"),
        }

        // A legacy line without `project=` still classifies, project `None`.
        let legacy = "2026-07-22T14:00:00+00:00 dispatch task=t workspace=alpha status=confirmed detail=busy_observed";
        match parse_event_line(legacy) {
            Event::System(sys) => {
                assert_eq!(sys.kind, SystemKind::Dispatch);
                assert_eq!(sys.project, None);
                assert_eq!(sys.target.as_deref(), Some("t"));
            }
            other => panic!("expected System(Dispatch), got {other:?}"),
        }
    }

    #[test]
    fn ssh_fold_key_ignores_volatile_detail() {
        // Dedup is (kind + target + status), ignoring the detail tail — so a
        // flapping host whose detail text jitters still collapses.
        let a = parse_event_line(
            "2026-07-22T14:00:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
        );
        let b = parse_event_line(
            "2026-07-22T14:02:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=tcp_forward_failed",
        );
        match (a, b) {
            (Event::System(a), Event::System(b)) => {
                assert_eq!(a.fold_key(), b.fold_key());
            }
            _ => panic!("expected two System events"),
        }
    }

    #[test]
    fn relative_time_buckets() {
        // Compact form to match the mockup — "now"/"4m"/"2h"/"3d", never an
        // ISO timestamp and never a "… ago" suffix.
        let now = Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap();
        assert_eq!(relative_time(now, now), "now");
        assert_eq!(relative_time(now - chrono::Duration::minutes(5), now), "5m");
        assert_eq!(relative_time(now - chrono::Duration::hours(2), now), "2h");
        assert_eq!(relative_time(now - chrono::Duration::days(3), now), "3d");
    }

    #[test]
    fn short_duration_formats() {
        assert_eq!(short_duration(chrono::Duration::seconds(45)), "45s");
        assert_eq!(short_duration(chrono::Duration::minutes(12)), "12m");
        assert_eq!(short_duration(chrono::Duration::hours(2)), "2h");
        assert_eq!(short_duration(chrono::Duration::minutes(125)), "2h05m");
    }

    #[test]
    fn each_phonetic_workspace_has_a_unique_tint() {
        // Identity is now color, not a face — recognizability depends on
        // each workspace getting a distinct tint, so regression-test that
        // no two phonetic names collide on the same color.
        let workspaces = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
        let mut seen: Vec<Tone> = Vec::new();
        for w in workspaces {
            let color = agent_tone(w);
            assert!(!seen.contains(&color), "duplicate tint for {w}");
            seen.push(color);
        }
    }

    #[test]
    fn unknown_workspace_falls_back_to_default_tint() {
        // A non-phonetic name still renders — just in the neutral gray
        // rather than a unique tint.
        assert_eq!(agent_tone("frontend"), Tone::Gray);
    }

    #[test]
    fn parse_zen_reason_recognizes_each_kind() {
        assert_eq!(
            parse_zen_reason("orchestrator:zen-promote category=4"),
            Some(ZenReason::Promote {
                category: Some("4".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("orchestrator:zen-merge pr=1343"),
            Some(ZenReason::Merge {
                pr: Some("1343".into()),
                sha: None,
            })
        );
        assert_eq!(
            parse_zen_reason("zen:failed-checks cmd=\"cargo test\" exit=1"),
            Some(ZenReason::FailedChecks {
                cmd: Some("cargo test".into()),
                exit: Some("1".into()),
            }),
            "quoted command values must survive parsing intact"
        );
        assert_eq!(
            parse_zen_reason("zen:diff-too-large files=12 lines=2543"),
            Some(ZenReason::DiffTooLarge {
                files: Some("12".into()),
                lines: Some("2543".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:danger-path paths=src/db.rs,migrations/001.sql"),
            Some(ZenReason::DangerPath {
                paths: Some("src/db.rs,migrations/001.sql".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:ci-timeout duration=15m"),
            Some(ZenReason::CiTimeout {
                duration: Some("15m".into()),
            })
        );
        assert_eq!(
            parse_zen_reason("zen:merge-conflict files=Cargo.lock,src/main.rs"),
            Some(ZenReason::MergeConflict {
                files: Some("Cargo.lock,src/main.rs".into()),
            })
        );
    }

    #[test]
    fn parse_zen_reason_recognizes_the_normalized_on_disk_merge_form() {
        // `append_task_event` folds the emitted reason's spaces into
        // underscores before it hits `events.log`, so the parser must
        // recognize the underscore-joined head+tail — an exact-head match
        // (the old behavior) dropped every real merge line to `Other`.
        assert_eq!(
            parse_zen_reason("orchestrator:zen-merge_pr=1343"),
            Some(ZenReason::Merge {
                pr: Some("1343".into()),
                sha: None,
            }),
            "canonical on-disk form must yield Merge with the PR number"
        );
        // A merge line that also carries a SHA, normalized.
        assert_eq!(
            parse_zen_reason("orchestrator:zen-merge_pr=1343_sha=abc1234"),
            Some(ZenReason::Merge {
                pr: Some("1343".into()),
                sha: Some("abc1234".into()),
            })
        );
        // Bare head (the pre-fix orchestrator template emitted this) still
        // renders as a merge, just without an identifier.
        assert_eq!(
            parse_zen_reason("orchestrator:zen-merge"),
            Some(ZenReason::Merge {
                pr: None,
                sha: None,
            })
        );
        // Historical lines really on disk (2026-09-22/23): the old zenmode
        // template emitted `ci-green(<n>)`, normalized to an underscore join.
        // These must still yield Merge (the badge) even though the PR number
        // is trapped inside `ci-green(...)` and not parsed out.
        assert_eq!(
            parse_zen_reason("orchestrator:zen-merge_ci-green(1343)"),
            Some(ZenReason::Merge {
                pr: None,
                sha: None,
            }),
            "historical ci-green(<n>) lines must still render as a merge"
        );
    }

    #[test]
    fn parse_zen_reason_keeps_future_zen_prefixes_under_zen_treatment() {
        // Anything starting with `zen:` or `orchestrator:zen-` we haven't
        // wired up yet still routes to the zen visual treatment — the
        // user sees the badge + tint and just doesn't get a per-kind
        // detail line. Better than silently rendering as a generic row.
        assert_eq!(
            parse_zen_reason("orchestrator:zen-decline reason-text=needs-tests"),
            Some(ZenReason::Other)
        );
        assert_eq!(parse_zen_reason("zen:auto-promote"), Some(ZenReason::Other));
    }

    #[test]
    fn parse_zen_reason_ignores_non_zen_reasons() {
        assert!(parse_zen_reason("user:cli:start").is_none());
        assert!(parse_zen_reason("workspace:ready-marker").is_none());
        assert!(parse_zen_reason("").is_none());
    }

    #[test]
    fn agent_from_reason_extracts_agent_segment() {
        // On disk the dispatch line carries
        // `reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer` —
        // the leading whitespace from the human reason has been folded
        // to underscores by `append_task_event`'s sanitizer. The helper
        // splits on `_` and surfaces the named segment.
        assert_eq!(
            agent_from_reason("orchestrator:auto-dispatch_workspace=alpha_agent=developer"),
            Some("developer".to_string())
        );
        assert_eq!(
            agent_from_reason("user:cli:start_agent=orchestrator"),
            Some("orchestrator".to_string())
        );
        assert_eq!(agent_from_reason("user:cli"), None);
        assert_eq!(agent_from_reason(""), None);
    }

    #[test]
    fn parse_kv_handles_quotes_and_bare_values() {
        let kv = parse_kv("a=1 b=\"two words\" c=three");
        assert_eq!(kv.get("a").map(String::as_str), Some("1"));
        assert_eq!(kv.get("b").map(String::as_str), Some("two words"));
        assert_eq!(kv.get("c").map(String::as_str), Some("three"));
    }

    fn task_event(reason: &str) -> Event {
        Event::Issue {
            project: None,
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            id: "demo-task".into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: reason.into(),
            agent: agent_from_reason(reason),
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        }
    }

    fn workspace_event() -> Event {
        Event::Workspace {
            project: None,
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            name: "alpha".into(),
            prev: None,
            new: WorkspaceState::Working,
            raw: String::new(),
        }
    }

    fn heartbeat_event() -> Event {
        Event::Heartbeat {
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            project: "demo".into(),
            raw: "2026-06-23T12:00:00+00:00 project=demo heartbeat".into(),
        }
    }

    #[test]
    fn parse_heartbeat_line_round_trips() {
        // Shape: `<ts> project=<name> heartbeat`. Must come back as the
        // `Heartbeat` variant (not `Unknown`) so the filter knows to
        // drop it.
        let line = "2026-06-23T12:00:00+00:00 project=demo heartbeat";
        match parse_event_line(line) {
            Event::Heartbeat { project, .. } => assert_eq!(project, "demo"),
            other => panic!("expected Heartbeat, got {other:?}"),
        }
    }

    #[test]
    fn parse_heartbeat_line_with_counts_round_trips() {
        // The poller appends `zen_eligible=`/`idle_workspaces=` counts after
        // the `heartbeat` keyword. The activity parser keys off the keyword
        // alone, so the row still classifies as `Heartbeat` (and stays
        // filtered out of the feed) regardless of the trailing tokens.
        let line =
            "2026-06-23T12:00:00+00:00 project=demo heartbeat zen_eligible=5 idle_workspaces=4";
        match parse_event_line(line) {
            Event::Heartbeat { project, .. } => assert_eq!(project, "demo"),
            other => panic!("expected Heartbeat, got {other:?}"),
        }
    }

    #[test]
    fn activity_filter_rejects_heartbeat_under_every_pill() {
        // Heartbeats are an orchestrator wake-up signal, not human-facing
        // activity. They must be filtered out regardless of which pill
        // (or no pill) is on — otherwise the feed gets a "nothing
        // happened" row every few minutes.
        let configs = [
            ActivityFilter::default(),
            kinds(&[ActivityKind::Zen]),
            kinds(&[ActivityKind::Workspaces]),
            kinds(&[ActivityKind::Zen, ActivityKind::Workspaces]),
            ActivityFilter {
                project: Some("demo".into()),
                ..ActivityFilter::default()
            },
        ];
        for f in configs {
            assert!(
                !f.matches(&heartbeat_event(), &no_cache()),
                "heartbeat passed filter: {f:?}"
            );
        }
    }

    #[test]
    fn activity_filter_all_matches_every_event() {
        let f = ActivityFilter::default();
        assert!(f.is_all());
        assert!(f.matches(&task_event("user:cli:start"), &no_cache()));
        assert!(f.matches(
            &task_event("orchestrator:zen-promote category=4"),
            &no_cache()
        ));
        assert!(f.matches(&workspace_event(), &no_cache()));
    }

    #[test]
    fn activity_filter_zen_keeps_only_zen_task_events() {
        let f = kinds(&[ActivityKind::Zen]);
        assert!(f.matches(
            &task_event("orchestrator:zen-promote category=4"),
            &no_cache()
        ));
        assert!(f.matches(&task_event("orchestrator:zen-merge_pr=1343"), &no_cache()));
        assert!(f.matches(
            &task_event("zen:failed-checks cmd=\"cargo test\""),
            &no_cache()
        ));
        assert!(!f.matches(&task_event("user:cli:start"), &no_cache()));
        assert!(!f.matches(&workspace_event(), &no_cache()));
    }

    #[test]
    fn activity_filter_workspaces_keeps_only_workspace_events() {
        let f = kinds(&[ActivityKind::Workspaces]);
        assert!(f.matches(&workspace_event(), &no_cache()));
        assert!(!f.matches(&task_event("user:cli:start"), &no_cache()));
        assert!(!f.matches(
            &task_event("orchestrator:zen-promote category=4"),
            &no_cache()
        ));
    }

    #[test]
    fn activity_filter_pills_are_multiselect_union() {
        let f = kinds(&[ActivityKind::Zen, ActivityKind::Workspaces]);
        assert!(f.matches(
            &task_event("orchestrator:zen-promote category=4"),
            &no_cache()
        ));
        assert!(f.matches(&workspace_event(), &no_cache()));
        // Regular user-action task still filtered out — neither pill matches it.
        assert!(!f.matches(&task_event("user:cli:start"), &no_cache()));
    }

    #[test]
    fn started_at_finds_nearest_prior_in_progress_transition() {
        // The "took 18m" line on a review handoff joins the review
        // event to its matching `* -> in_progress` event for the same
        // task. Walk the events list backwards from the review event
        // and return the in_progress event's timestamp.
        let events = vec![
            Event::Issue {
                project: None,
                ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap(),
                id: "foo".into(),
                workflow: DEFAULT_WORKFLOW_NAME.into(),
                from: Column::todo(),
                to: Column::in_progress(),
                reason: "user:cli".into(),
                agent: None,
                from_category: Column::todo().category(),
                to_category: Column::in_progress().category(),
                raw: String::new(),
            },
            // Unrelated task in between — must not affect the lookup.
            Event::Issue {
                project: None,
                ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 5, 0).unwrap(),
                id: "bar".into(),
                workflow: DEFAULT_WORKFLOW_NAME.into(),
                from: Column::todo(),
                to: Column::in_progress(),
                reason: "user:cli".into(),
                agent: None,
                from_category: Column::todo().category(),
                to_category: Column::in_progress().category(),
                raw: String::new(),
            },
            Event::Issue {
                project: None,
                ts: Utc.with_ymd_and_hms(2026, 1, 1, 10, 18, 0).unwrap(),
                id: "foo".into(),
                workflow: DEFAULT_WORKFLOW_NAME.into(),
                from: Column::in_progress(),
                to: Column::review(),
                reason: "workspace:ready-marker".into(),
                agent: None,
                from_category: Column::in_progress().category(),
                to_category: Column::review().category(),
                raw: String::new(),
            },
        ];
        let started = started_at(&events, 2, "foo");
        assert_eq!(
            started,
            Some(Utc.with_ymd_and_hms(2026, 1, 1, 10, 0, 0).unwrap()),
            "must pair the review event with its task's own in_progress event"
        );
    }

    fn register_github_project(home: &std::path::Path, name: &str) {
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join(format!("{name}.yaml")),
            format!(
                "name: {name}\nrepo: /tmp/{name}\ndefault_branch: main\n\
orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{name}\nworkspaces: []\n\
issue_tracker:\n  backend: github\n  github:\n    repo: owner/repo\n"
            ),
        )
        .unwrap();
    }

    fn done_issue_event(id: &str) -> Event {
        Event::Issue {
            project: None,
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            id: id.into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::review(),
            to: Column::done(),
            reason: "user:review-accept".into(),
            agent: None,
            from_category: Column::review().category(),
            to_category: Column::done().category(),
            raw: String::new(),
        }
    }

    /// AC4: with the Activity view open on a board of done tasks, one refresh
    /// resolves every referenced title in a single aliased `fetch_many` request,
    /// and every subsequent (idle) tick makes **no** further request — the
    /// negative/positive cache holds. This is the fix for the per-event `get`
    /// storm the 2026-09-08 incident attributed to `__activity`.
    #[test]
    fn activity_view_batches_titles_and_idle_ticks_cost_no_requests() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-batch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv");

        // Seed the id→number map so resolution takes the index fast path (no
        // per-id search) and `fetch_many` is a single aliased request for all
        // three — the O(1) batch AC4 requires.
        let mut idx = shelbi_state::BoardIndex::fresh(Vec::new());
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("actv", &idx).unwrap();
        for (id, n) in [("d1", 1), ("d2", 2), ("d3", 3)] {
            shelbi_state::record_board_index_number("actv", id, n).unwrap();
        }

        // A fake `gh` that counts GraphQL calls and answers the aliased
        // IssuesByNumber batch with all three done issues in one response.
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rec = std::sync::Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |args: &[&str]| {
            let joined = args.join(" ");
            if joined.contains("graphql") {
                rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if joined.contains("IssuesByNumber") {
                Ok(r#"{"data":{"rateLimit":{"remaining":4999,"resetAt":"2026-09-08T12:00:00Z"},
                "repository":{
                  "i0":{"number":1,"title":"Done One","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/d1"},{"name":"shelbi:status/done"}]}},
                  "i1":{"number":2,"title":"Done Two","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/d2"},{"name":"shelbi:status/done"}]}},
                  "i2":{"number":3,"title":"Done Three","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/d3"},{"name":"shelbi:status/done"}]}}
                }}}"#.to_string())
            } else {
                Ok(String::new())
            }
        });

        let events = vec![
            done_issue_event("d1"),
            done_issue_event("d2"),
            done_issue_event("d3"),
        ];
        let mut cache = ActivityMetaCache::default();
        let resolve = |cache: &mut ActivityMetaCache| {
            resolve_task_meta_into("actv", &events, cache, Instant::now())
        };

        // Warmup: one batched aliased request resolves all three titles.
        let snap = resolve(&mut cache);
        let after_warmup = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            after_warmup, 1,
            "three titles resolved in ONE aliased request"
        );
        assert_eq!(
            task_meta(&snap, "d1").map(|m| m.title.as_str()),
            Some("Done One")
        );
        assert_eq!(
            task_meta(&snap, "d3").map(|m| m.title.as_str()),
            Some("Done Three")
        );

        // Idle ticks: no new events, entries within their TTL → no requests.
        for _ in 0..5 {
            resolve(&mut cache);
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            after_warmup,
            "an idle board costs no GraphQL requests per tick (O(1), not O(N))"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// An idle TUI left on the Activity view re-resolved every **open** id the
    /// feed referenced each time its 15s TTL lapsed, one GraphQL fetch apiece,
    /// for as long as the TUI stayed open. Open issues are all in the
    /// daemon-published board index, so their titles must come from it: an hour
    /// of TTL expiries costs no `gh` call at all.
    #[test]
    fn activity_view_serves_open_titles_from_the_board_index() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-index-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-idx");

        let open = |id: &str, title: &str| {
            let now = Utc::now();
            shelbi_state::IssueFile {
                task: shelbi_core::Issue {
                    id: id.into(),
                    title: title.into(),
                    column: Column::in_progress(),
                    priority: 0,
                    assigned_to: None,
                    workflow: None,
                    branch: Some(format!("jlong/{id}")),
                    depends_on: Vec::new(),
                    prefers_machine: None,
                    zen: None,
                    launch: None,
                    params: std::collections::BTreeMap::new(),
                    created_at: now,
                    updated_at: now,
                },
                body: String::new(),
                tracker_assignees: Vec::new(),
            }
        };
        let mut idx = shelbi_state::BoardIndex::fresh(vec![
            open("o1", "Open One"),
            open("o2", "Open Two"),
        ]);
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("actv-idx", &idx).unwrap();
        shelbi_state::set_task_assignment("actv-idx", "o1", Some("alpha")).unwrap();

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rec = std::sync::Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |_args: &[&str]| {
            rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(String::new())
        });

        let started = |id: &str| Event::Issue {
            ts: Utc.with_ymd_and_hms(2026, 6, 23, 12, 0, 0).unwrap(),
            project: None,
            id: id.into(),
            workflow: DEFAULT_WORKFLOW_NAME.into(),
            from: Column::todo(),
            to: Column::in_progress(),
            reason: "user:cli".into(),
            agent: None,
            from_category: Column::todo().category(),
            to_category: Column::in_progress().category(),
            raw: String::new(),
        };
        let events = vec![started("o1"), started("o2")];
        let mut cache = ActivityMetaCache::default();
        let start = Instant::now();
        // An hour of passes, each one past the open-entry TTL.
        for minute in 0..60u64 {
            let now = start + Duration::from_secs(minute * 60);
            let snap = resolve_task_meta_into("actv-idx", &events, &mut cache, now);
            let o1 = snap.get("o1").and_then(|e| e.meta.as_ref()).expect("o1 resolved");
            assert_eq!(o1.title, "Open One");
            assert_eq!(o1.branch.as_deref(), Some("jlong/o1"));
            assert_eq!(o1.assigned_to.as_deref(), Some("alpha"), "overlay folded in");
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "open titles come from the board index, never a gh call"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// AC4: an id the store can't resolve is negative-cached, so it is not
    /// re-searched on every tick — the "no negative/closed-id cache" defect.
    #[test]
    fn activity_view_negative_caches_unresolvable_ids() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-neg-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-neg");

        // No index numbers → resolution falls to the id-search, which we answer
        // empty (the id resolves to nothing). Count those search calls.
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rec = std::sync::Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |args: &[&str]| {
            if args.join(" ").contains("graphql") {
                rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            // An empty search result: the id names no issue.
            Ok(r#"{"data":{"search":{"nodes":[]}}}"#.to_string())
        });

        let events = vec![done_issue_event("ghost")];
        let mut cache = ActivityMetaCache::default();
        let resolve = |cache: &mut ActivityMetaCache| {
            resolve_task_meta_into("actv-neg", &events, cache, Instant::now())
        };

        let snap = resolve(&mut cache);
        let after_first = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert!(after_first >= 1, "the first resolution searches for the id");
        assert!(
            task_meta(&snap, "ghost").is_none(),
            "an unresolved id has no title"
        );

        // The miss is cached: further ticks do not re-search within the TTL.
        for _ in 0..5 {
            resolve(&mut cache);
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            after_first,
            "an unresolvable id is negative-cached, not re-searched every tick"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn resolved_entry(terminal: bool, age: Duration) -> TaskCacheEntry {
        TaskCacheEntry {
            meta: Some(TaskMeta {
                title: "T".into(),
                branch: None,
                assigned_to: None,
            }),
            mtime: None,
            fetched: Instant::now() - age,
            terminal,
        }
    }

    /// The loop's exit condition, isolated: a resolved **terminal** (done/
    /// canceled) issue AND a confirmed **miss** are both frozen — never
    /// re-resolved, even long past the remote TTL — so an idle board full of done
    /// cards and unresolvable slug ids stops generating `fetch_many` traffic. An
    /// *open* issue past the TTL still re-resolves (edits must show). This is
    /// exactly the asymmetry the endless-refetch incident (and its rework) missed.
    #[test]
    fn resolved_terminal_entries_and_misses_are_frozen_past_the_ttl() {
        let long_ago = REMOTE_META_TTL + Duration::from_secs(3600);

        // A done issue resolved an hour ago: frozen, not stale (no re-fetch).
        assert!(
            !entry_stale(Some(&resolved_entry(true, long_ago)), None),
            "a resolved terminal issue is frozen and never re-resolved"
        );
        // An open issue resolved an hour ago: stale (titles can still change).
        assert!(
            entry_stale(Some(&resolved_entry(false, long_ago)), None),
            "a resolved open issue past the TTL is re-resolved"
        );
        // A confirmed miss, even far past the old 15s TTL, is frozen: searched at
        // most once per session, not re-run every sweep (the rework fix).
        let miss = TaskCacheEntry {
            meta: None,
            mtime: None,
            fetched: Instant::now() - long_ago,
            terminal: false,
        };
        assert!(
            !entry_stale(Some(&miss), None),
            "a confirmed miss is frozen past the TTL, not re-searched every sweep"
        );
        // An unknown id (no entry at all) is still always wanted — the freeze only
        // applies once an id has been searched and confirmed to resolve to nothing.
        assert!(entry_stale(None, None), "an unknown id is wanted");

        // is_frozen_column classifies the terminal columns and nothing else.
        assert!(is_frozen_column(&Column::done()));
        assert!(is_frozen_column(&Column::canceled()));
        assert!(!is_frozen_column(&Column::in_progress()));
        assert!(!is_frozen_column(&Column::todo()));
    }

    /// The shell's off-thread activity path (the one that looped): with a
    /// persistent [`ActivityMetaCache`] carried across reads, a board of done
    /// tasks hydrates its titles **once** and every subsequent read makes no
    /// further GraphQL request — the loop terminates and does not restart on each
    /// refresh. (Before the fix this path rebuilt the cache from scratch every
    /// pass and re-fetched every done issue forever.)
    #[test]
    fn done_history_hydration_terminates_and_does_not_restart_each_pass() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-loop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-loop");

        // Index numbers so resolution takes the fast path (no per-id search) and
        // the batch is a single aliased IssuesByNumber request.
        let mut idx = shelbi_state::BoardIndex::fresh(Vec::new());
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("actv-loop", &idx).unwrap();
        for (id, n) in [("d1", 1), ("d2", 2), ("d3", 3)] {
            shelbi_state::record_board_index_number("actv-loop", id, n).unwrap();
        }

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rec = std::sync::Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |args: &[&str]| {
            let joined = args.join(" ");
            if joined.contains("graphql") {
                rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            if joined.contains("IssuesByNumber") {
                Ok(r#"{"data":{"rateLimit":{"remaining":4999,"resetAt":"2026-09-08T12:00:00Z"},
                "repository":{
                  "i0":{"number":1,"title":"Done One","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/d1"},{"name":"shelbi:status/done"}]}},
                  "i1":{"number":2,"title":"Done Two","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/d2"},{"name":"shelbi:status/done"}]}},
                  "i2":{"number":3,"title":"Done Three","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/d3"},{"name":"shelbi:status/done"}]}}
                }}}"#.to_string())
            } else {
                Ok(String::new())
            }
        });

        let events = vec![
            done_issue_event("d1"),
            done_issue_event("d2"),
            done_issue_event("d3"),
        ];
        let mut cache = ActivityMetaCache::default();

        // First pass: one aliased request resolves all three done titles.
        let now = Instant::now();
        let snap = resolve_task_meta_into("actv-loop", &events, &mut cache, now);
        let after_first = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            after_first, 1,
            "done history hydrates in ONE aliased request"
        );
        assert_eq!(
            snap.get("d1")
                .and_then(|e| e.meta.as_ref())
                .map(|m| m.title.as_str()),
            Some("Done One")
        );
        assert!(
            cache.entries["d1"].terminal,
            "a done issue is cached as terminal (frozen)"
        );
        assert!(cache.hydration_started, "the start was recorded");

        // Every subsequent pass is quiet: the frozen done entries are never
        // re-resolved, so the loop does not restart on each refresh.
        for _ in 0..10 {
            resolve_task_meta_into("actv-loop", &events, &mut cache, now);
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            after_first,
            "done-history hydration terminates: no further requests once resolved"
        );
        assert!(
            cache.hydration_finished,
            "the quiet steady state was recorded"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The rework's steady-state fix: a feed full of **unresolvable** ids — the
    /// slug task ids from before the GitHub board (`rt-merge-main-into-umbrella`
    /// and friends) that are not GitHub issues — is searched at most *once per id*
    /// across many passes, including passes long after the old 15s TTL would have
    /// expired. Before the fix each miss went stale on the short TTL every sweep,
    /// so an idle board re-ran ~234 `id-search` queries forever (over budget).
    /// After hydration the worker makes **zero** GitHub requests per pass.
    #[test]
    fn unresolvable_slug_ids_are_searched_once_per_session_not_every_sweep() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-slug-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-slug");

        // Unique slug ids (no index number → resolution falls to the label
        // search). A nonce keeps them out of the process-global id→number miss
        // cache a sibling test might have warmed, so the first pass genuinely
        // searches each one.
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let slug_ids: Vec<String> = (0..5)
            .map(|i| format!("rt-legacy-slug-{nonce}-{i}"))
            .collect();

        // A fake `gh` that counts GraphQL calls and answers every id-search empty
        // — each slug resolves to nothing (it names no GitHub issue).
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rec = std::sync::Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |args: &[&str]| {
            if args.join(" ").contains("graphql") {
                rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }
            Ok(r#"{"data":{"search":{"nodes":[]}}}"#.to_string())
        });

        let events: Vec<Event> = slug_ids.iter().map(|id| done_issue_event(id)).collect();
        let mut cache = ActivityMetaCache::default();

        // First pass: one label search per slug, then every id is negative-cached.
        let now = Instant::now();
        resolve_task_meta_into("actv-slug", &events, &mut cache, now);
        let after_first = calls.load(std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            after_first,
            slug_ids.len(),
            "the first pass searches each unresolvable id exactly once"
        );
        for id in &slug_ids {
            assert!(
                cache.entries.get(id).is_some_and(|e| e.meta.is_none()),
                "{id} is negative-cached as a confirmed miss"
            );
        }

        // Age every cached miss well past the old 15s remote TTL, simulating many
        // minutes of sweeps. Under the old rule each would now be re-searched; the
        // frozen-miss rule holds, so the next passes make NO further request.
        let long_ago = Instant::now() - (REMOTE_META_TTL + Duration::from_secs(3600));
        for entry in cache.entries.values_mut() {
            entry.fetched = long_ago;
        }

        for _ in 0..10 {
            let now = Instant::now();
            resolve_task_meta_into("actv-slug", &events, &mut cache, now);
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            after_first,
            "a confirmed miss is frozen for the session: no re-search after the TTL lapses"
        );
        assert!(
            cache.hydration_finished,
            "an idle board of unresolvable ids reaches the quiet steady state"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A transient fetch *error* must not be mistaken for a miss: the ids stay
    /// unknown (not frozen), so a later pass retries them once the store recovers.
    /// Freezing a live id as a miss on one GraphQL blip would hide its title for
    /// the whole session — the safety valve on the steady-state freeze.
    #[test]
    fn a_fetch_error_does_not_negative_cache_ids() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-err-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-err");

        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let id = format!("rt-transient-{nonce}");

        // First: the search transport fails (a connection blip / rate-limit park).
        let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let fail_rt = std::sync::Arc::clone(&fail);
        shelbi_state::set_test_gh_runner(move |_args: &[&str]| {
            if fail_rt.load(std::sync::atomic::Ordering::SeqCst) {
                Err(shelbi_core::Error::Other("transient".into()))
            } else {
                Ok(r#"{"data":{"search":{"nodes":[]}}}"#.to_string())
            }
        });

        let events = vec![done_issue_event(&id)];
        let mut cache = ActivityMetaCache::default();
        let t0 = Instant::now();
        resolve_task_meta_into("actv-err", &events, &mut cache, t0);
        assert!(
            !cache.entries.contains_key(&id),
            "a fetch error leaves the id unknown, not negative-cached"
        );
        assert!(
            entry_stale(cache.entries.get(&id), None),
            "the id is still wanted"
        );
        // But it is now in a backoff window, so the very next pass does not retry.
        assert!(cache.in_backoff(&id, t0), "a failed fetch backs the id off");

        // The store recovers, but while still inside the backoff window no retry
        // fires — the id stays unknown rather than resolving immediately.
        fail.store(false, std::sync::atomic::Ordering::SeqCst);
        resolve_task_meta_into("actv-err", &events, &mut cache, t0);
        assert!(
            !cache.entries.contains_key(&id),
            "inside the backoff window the recovered store is not yet consulted"
        );

        // Once the backoff elapses the id is retried; the store answers (empty →
        // a genuine miss this time) and the backoff is cleared.
        let after_backoff = t0 + FETCH_BACKOFF_BASE + Duration::from_secs(1);
        resolve_task_meta_into("actv-err", &events, &mut cache, after_backoff);
        assert!(
            cache.entries.get(&id).is_some_and(|e| e.meta.is_none()),
            "once the store answers, the unresolvable id is negative-cached"
        );
        assert!(
            !entry_stale(cache.entries.get(&id), None),
            "and the confirmed miss is then frozen"
        );
        assert!(
            !cache.in_backoff(&id, after_backoff),
            "a resolved (even to a miss) id has its backoff cleared"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Round-2 storm path: one id whose fetch keeps erroring must NOT drag the
    /// whole batch into a per-pass retry loop. A fake `gh` resolves three ids from
    /// the index (one aliased batch, never a search) and errors every search for a
    /// fourth, unresolvable slug id. The good ids are cached after the first pass
    /// and never re-fetched; the failing id is retried only as its exponential
    /// backoff windows elapse, so total GraphQL traffic stays bounded across
    /// hundreds of passes (with an injected clock) instead of ~65 calls/s.
    #[test]
    fn a_failing_id_is_retried_under_backoff_not_every_pass() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = std::env::temp_dir().join(format!(
            "shelbi-activity-backoff-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-bo");

        // Three good ids with index numbers → the fast path (one aliased
        // IssuesByNumber, no search). The fourth is an unresolvable slug with no
        // number, so it falls to a label search — which the fake always errors.
        let mut idx = shelbi_state::BoardIndex::fresh(Vec::new());
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("actv-bo", &idx).unwrap();
        for (id, n) in [("g1", 1), ("g2", 2), ("g3", 3)] {
            shelbi_state::record_board_index_number("actv-bo", id, n).unwrap();
        }
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let bad = format!("rt-bad-slug-{nonce}");

        let batches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let searches = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let batches_rt = std::sync::Arc::clone(&batches);
        let searches_rt = std::sync::Arc::clone(&searches);
        shelbi_state::set_test_gh_runner(move |args: &[&str]| {
            let joined = args.join(" ");
            if joined.contains("IssuesByNumber") {
                batches_rt.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(r#"{"data":{"rateLimit":{"remaining":4999,"resetAt":"2026-09-08T12:00:00Z"},
                "repository":{
                  "i0":{"number":1,"title":"Good One","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/g1"},{"name":"shelbi:status/done"}]}},
                  "i1":{"number":2,"title":"Good Two","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/g2"},{"name":"shelbi:status/done"}]}},
                  "i2":{"number":3,"title":"Good Three","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-06-01T00:00:00Z","updatedAt":"2026-06-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/g3"},{"name":"shelbi:status/done"}]}}
                }}}"#.to_string())
            } else if joined.contains("graphql") {
                // A label search (for the unresolvable slug) — always errors.
                searches_rt.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Err(shelbi_core::Error::Other("query rejected".into()))
            } else {
                Ok(String::new())
            }
        });

        let events = vec![
            done_issue_event("g1"),
            done_issue_event("g2"),
            done_issue_event("g3"),
            done_issue_event(&bad),
        ];
        let mut cache = ActivityMetaCache::default();
        let t0 = Instant::now();

        // Pass 1: good ids resolve in one aliased batch; the bad id is searched
        // once (errors) and backed off — one bad id does not discard the good ones.
        resolve_task_meta_into("actv-bo", &events, &mut cache, t0);
        assert_eq!(batches.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(searches.load(std::sync::atomic::Ordering::SeqCst), 1);
        for g in ["g1", "g2", "g3"] {
            assert!(
                cache.entries.get(g).and_then(|e| e.meta.as_ref()).is_some(),
                "{g} is cached after the first pass despite the sibling failure"
            );
        }
        assert!(
            !cache.entries.contains_key(&bad),
            "the failing id is not cached as a miss"
        );
        assert!(cache.in_backoff(&bad, t0), "the failing id is backed off");

        // Many passes inside the first backoff window: no new requests at all.
        for i in 0..20u64 {
            resolve_task_meta_into(
                "actv-bo",
                &events,
                &mut cache,
                t0 + Duration::from_millis(i),
            );
        }
        assert_eq!(
            batches.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cached good ids are never re-fetched"
        );
        assert_eq!(
            searches.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the failing id is not re-searched inside the backoff window"
        );

        // Step a fake clock forward ~33 minutes in 10s increments. The backoff
        // doubles (30s, 60s, 120s, …, capped at 600s), so the failing id is
        // retried only a handful of times across 200 passes, never per pass.
        let mut now = t0;
        for _ in 0..200 {
            now += Duration::from_secs(10);
            resolve_task_meta_into("actv-bo", &events, &mut cache, now);
        }
        let total = searches.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            (2..=12).contains(&total),
            "over ~33 min the failing id retried {total} times — bounded by backoff, \
             not once per pass (would be 200+)"
        );
        assert_eq!(
            batches.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the good ids never trigger another batch request"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // --- the shared feed: order, day headers, folding, targets, filters ---

    /// A cache naming `t1`'s title and assignee, as a resolved read would.
    fn cache_with(id: &str, title: &str, assignee: Option<&str>) -> TaskCache {
        let mut cache = TaskCache::new();
        cache.insert(
            id.to_string(),
            TaskCacheEntry {
                meta: Some(TaskMeta {
                    title: title.into(),
                    branch: Some(format!("jlong/{id}")),
                    assigned_to: assignee.map(str::to_string),
                }),
                mtime: None,
                fetched: Instant::now(),
                terminal: false,
            },
        );
        cache
    }

    fn fixture_events() -> Vec<Event> {
        [
            "2026-07-21T09:00:00+00:00 project=demo task=t1 workflow=default todo -> in_progress reason=orchestrator:auto-dispatch_workspace=alpha_agent=developer",
            "2026-07-22T14:00:00+00:00 project=demo task=t1 workflow=default in_progress -> review reason=workspace:ready-marker from_category=active to_category=handoff",
            "2026-07-22T14:01:00+00:00 project=demo workspace=bravo working -> awaiting_input",
            "2026-07-22T14:02:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
            "2026-07-22T14:03:00+00:00 ssh reverse-forward host=devbox mode=tcp status=failed detail=master_open_failed",
            "2026-07-22T14:04:00+00:00 project=web task=w9 workflow=default backlog -> todo reason=user:board",
            "2026-07-22T14:05:00+00:00 project=demo heartbeat zen_eligible=1 idle_workspaces=2",
        ]
        .iter()
        .map(|l| parse_event_line(l))
        .collect()
    }

    fn row_summaries(feed: &Feed) -> Vec<String> {
        feed.items
            .iter()
            .map(|item| match item {
                FeedItem::Day(label) => format!("== {label}"),
                FeedItem::Row(r) => format!(
                    "{} {} {}{} | {} | {}",
                    r.chip.as_ref().map(Chip::text).unwrap_or_default(),
                    r.verb,
                    r.title,
                    r.trail.as_ref().map(|t| t.0.as_str()).unwrap_or(""),
                    r.pill.as_ref().map(Pill::bracketed).unwrap_or_default(),
                    r.time,
                ),
            })
            .collect()
    }

    #[test]
    fn feed_is_newest_first_with_day_headers_folds_and_no_heartbeats() {
        let now = Utc.with_ymd_and_hms(2026, 7, 22, 15, 0, 0).unwrap();
        let cache = cache_with("t1", "Fix the login page", Some("alpha"));
        let feed = build_feed(&fixture_events(), &cache, &ActivityFilter::default(), now);
        assert_eq!(feed.empty, None);
        let summaries = row_summaries(&feed);
        let rows: Vec<&String> = summaries.iter().filter(|r| !r.starts_with("== ")).collect();
        assert_eq!(
            rows,
            vec![
                "[you] promoted w9 | [ready] | 56m",
                // Two identical ssh failures fold into one row.
                "[devbox] unreachable master open failed (2 attempts) |  | last 57m",
                "[bravo] awaiting input  |  | 59m",
                "[alpha] finished Fix the login page | [ready for review] | took 29h · 1h",
                "[alpha] is building Fix the login page | [in progress] | 1d",
            ],
            "{summaries:#?}"
        );
        // A day header opens the feed and each new local day; the labels
        // depend on the machine's timezone, so count them against it.
        assert!(summaries[0].starts_with("== "), "{summaries:#?}");
        let days: HashSet<_> = fixture_events()
            .iter()
            .filter(|e| ActivityKind::of(e).is_some())
            .filter_map(|e| e.ts().map(|t| t.with_timezone(&Local).date_naive()))
            .collect();
        assert_eq!(
            summaries.iter().filter(|r| r.starts_with("== ")).count(),
            days.len(),
            "{summaries:#?}"
        );
        let targets: Vec<Option<RowTarget>> = feed.rows().map(|r| r.target.clone()).collect();
        assert_eq!(
            targets,
            vec![
                Some(RowTarget::Task("w9".into())),
                None,
                Some(RowTarget::Workspace("bravo".into())),
                Some(RowTarget::Task("t1".into())),
                Some(RowTarget::Task("t1".into())),
            ]
        );
        let finished = feed.rows().nth(3).unwrap();
        assert_eq!(
            finished.secondary,
            Some(Secondary::Branch("jlong/t1".into()))
        );
        assert_eq!(finished.pill.as_ref().map(|p| p.tone), Some(Tone::Magenta));
        assert_eq!(
            finished.chip.as_ref().map(|c| (c.tone, c.bold)),
            Some((Tone::Cyan, true))
        );
    }

    #[test]
    fn feed_reports_why_it_is_empty() {
        let now = Utc.with_ymd_and_hms(2026, 7, 22, 15, 0, 0).unwrap();
        let none = build_feed(&[], &no_cache(), &ActivityFilter::default(), now);
        assert_eq!(none.empty, Some(EMPTY_FEED));
        let filtered = build_feed(
            &fixture_events(),
            &no_cache(),
            &ActivityFilter {
                project: Some("nope".into()),
                ..ActivityFilter::default()
            },
            now,
        );
        assert_eq!(filtered.empty, Some(EMPTY_FILTER));
        assert!(filtered.items.is_empty());
    }

    #[test]
    fn filters_by_project_workspace_and_kind() {
        let events = fixture_events();
        let cache = cache_with("t1", "Fix the login page", Some("alpha"));
        let pass = |f: &ActivityFilter| -> Vec<String> {
            events
                .iter()
                .filter(|e| f.matches(e, &cache))
                .map(|e| e.raw().to_string())
                .map(|raw| raw.split(' ').nth(1).unwrap_or("").to_string())
                .collect()
        };
        // Project: only lines scoped to it; unscoped ssh lines drop out.
        let demo = ActivityFilter {
            project: Some("demo".into()),
            ..ActivityFilter::default()
        };
        assert_eq!(pass(&demo), vec!["project=demo"; 3]);
        // Workspace: a task event counts for its assignee.
        let alpha = ActivityFilter {
            workspace: Some("alpha".into()),
            ..ActivityFilter::default()
        };
        assert_eq!(
            events.iter().filter(|e| alpha.matches(e, &cache)).count(),
            2
        );
        // ...but not without the title cache saying who holds the task.
        assert_eq!(
            events
                .iter()
                .filter(|e| alpha.matches(e, &no_cache()))
                .count(),
            0
        );
        // Kind: System keeps the ssh lines; Tasks keeps the transitions.
        assert_eq!(
            events
                .iter()
                .filter(|e| kinds(&[ActivityKind::System]).matches(e, &cache))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|e| kinds(&[ActivityKind::Tasks]).matches(e, &cache))
                .count(),
            3
        );
        // Dimensions intersect.
        let both = ActivityFilter {
            kinds: vec![ActivityKind::Workspaces],
            project: Some("demo".into()),
            workspace: Some("bravo".into()),
        };
        assert_eq!(events.iter().filter(|e| both.matches(e, &cache)).count(), 1);
    }

    #[test]
    fn kinds_classify_zen_and_dry_runs_together() {
        let dry = parse_event_line(
            "2026-06-24T10:00:00Z zen-dryrun task=fix-typo action=consider-auto-promote detail=x",
        );
        assert_eq!(ActivityKind::of(&dry), Some(ActivityKind::Zen));
        assert_eq!(ActivityKind::of(&heartbeat_event()), None);
        assert_eq!(
            ActivityKind::of(&task_event("orchestrator:zen-promote category=4")),
            Some(ActivityKind::Zen)
        );
        assert_eq!(
            ActivityKind::of(&task_event("user:cli")),
            Some(ActivityKind::Tasks)
        );
        let mut f = ActivityFilter::default();
        f.toggle_kind(ActivityKind::System);
        f.toggle_kind(ActivityKind::Tasks);
        assert_eq!(
            f.kinds,
            vec![ActivityKind::Tasks, ActivityKind::System],
            "kept sorted"
        );
        f.toggle_kind(ActivityKind::System);
        assert_eq!(f.kinds, vec![ActivityKind::Tasks]);
        f.clear_kinds();
        assert!(f.is_all() && f.is_unfiltered());
    }

    #[test]
    fn filter_choices_list_the_projects_and_workspaces_in_view() {
        let cache = cache_with("t1", "Fix the login page", Some("alpha"));
        let (projects, workspaces) = filter_choices(&fixture_events(), &cache);
        assert_eq!(projects, vec!["demo".to_string(), "web".to_string()]);
        assert_eq!(workspaces, vec!["alpha".to_string(), "bravo".to_string()]);
    }

    #[test]
    fn events_carry_their_project_scope() {
        let events = fixture_events();
        let projects: Vec<Option<&str>> = events.iter().map(Event::project).collect();
        assert_eq!(
            projects,
            vec![
                Some("demo"),
                Some("demo"),
                Some("demo"),
                None,
                None,
                Some("web"),
                Some("demo")
            ]
        );
    }

    // --- paging: the windowed tail read ---

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-activity-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn log_lines(n: usize) -> String {
        (0..n)
            .map(|i| {
                format!(
                    "2026-07-22T{:02}:{:02}:00+00:00 project=demo workspace=alpha working -> awaiting_input\n",
                    i / 60 % 24,
                    i % 60
                )
            })
            .collect()
    }

    #[test]
    fn read_last_lines_pages_from_the_end() {
        let dir = temp_dir("tail");
        let path = dir.join("events.log");
        // Long enough to span several read blocks.
        fs::write(&path, log_lines(3000)).unwrap();
        let all: Vec<String> = log_lines(3000).lines().map(str::to_string).collect();

        let (text, older) = read_last_lines(&path, 10).unwrap();
        assert!(older);
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            all[2990..].iter().map(String::as_str).collect::<Vec<_>>()
        );

        let (text, older) = read_last_lines(&path, 2500).unwrap();
        assert!(older);
        assert_eq!(text.lines().count(), 2500);
        assert_eq!(text.lines().next(), Some(all[500].as_str()));

        // A window past the start returns every line and nothing older.
        let (text, older) = read_last_lines(&path, 5000).unwrap();
        assert!(!older);
        assert_eq!(text.lines().count(), 3000);

        // Blank lines don't count; a missing trailing newline is fine.
        fs::write(&path, "a\n\nb\nc").unwrap();
        let (text, older) = read_last_lines(&path, 2).unwrap();
        assert_eq!(text, "b\nc");
        assert!(older);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_windowed_read_returns_the_newest_events_and_says_more_exist() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_dir("window");
        std::env::set_var("SHELBI_HOME", &home);
        fs::write(home.join("events.log"), log_lines(1200)).unwrap();
        let cache = Mutex::new(ActivityMetaCache::default());

        let first = read_activity_window("demo", &cache, ACTIVITY_PAGE);
        assert_eq!(first.events.len(), ACTIVITY_PAGE);
        assert!(first.has_older);
        assert_eq!(first.window, Some(ACTIVITY_PAGE));
        assert_eq!(first.status, None);

        // "Load older": one page more, still the newest end of the log.
        let more = read_activity_window("demo", &cache, ACTIVITY_PAGE * 2);
        assert_eq!(more.events.len(), ACTIVITY_PAGE * 2);
        assert_eq!(
            more.events.last().map(Event::raw),
            first.events.last().map(Event::raw),
            "both windows end at the newest line"
        );
        let all = read_activity_window("demo", &cache, ACTIVITY_PAGE * 3);
        assert_eq!(all.events.len(), 1200);
        assert!(!all.has_older);

        // No log yet: an empty feed, not an error.
        fs::remove_file(home.join("events.log")).unwrap();
        let empty = read_activity_window("demo", &cache, ACTIVITY_PAGE);
        assert!(empty.events.is_empty() && !empty.has_older && empty.status.is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&home);
    }

    /// The desktop's first paint (`dt-activity-perf`): a staged read hands
    /// over the titles at hand (the board index's open ones) before the
    /// tracker is asked for the closed ones, and a filesystem board, with
    /// nothing waiting on the network, reads once.
    #[test]
    fn a_staged_read_shows_the_local_titles_before_asking_the_tracker() {
        let _g = ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = temp_dir("staged");
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "actv-staged");
        let now = Utc::now();
        let open = shelbi_state::IssueFile {
            task: shelbi_core::Issue {
                id: "o1".into(),
                title: "Open One".into(),
                column: Column::in_progress(),
                priority: 0,
                assigned_to: None,
                workflow: None,
                branch: None,
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                params: std::collections::BTreeMap::new(),
                created_at: now,
                updated_at: now,
            },
            body: String::new(),
            tracker_assignees: Vec::new(),
        };
        let mut idx = shelbi_state::BoardIndex::fresh(vec![open]);
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("actv-staged", &idx).unwrap();
        fs::write(
            home.join("events.log"),
            "2026-07-22T14:00:00+00:00 project=actv-staged task=o1 workflow=app todo -> in_progress reason=user:cli\n\
             2026-07-22T14:01:00+00:00 project=actv-staged task=d1 workflow=app review -> done reason=user:cli\n",
        )
        .unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let rec = std::sync::Arc::clone(&calls);
        shelbi_state::set_test_gh_runner(move |_args: &[&str]| {
            rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(String::new())
        });
        let cache = Mutex::new(ActivityMetaCache::default());
        let mut early = None;
        let read = read_activity_window_staged("actv-staged", &cache, ACTIVITY_PAGE, |data| {
            early = Some((data, calls.load(std::sync::atomic::Ordering::SeqCst)));
        });
        let (data, calls_before) = early.expect("d1 waits on the tracker, so the local titles come first");
        assert_eq!(calls_before, 0, "before any gh call");
        assert_eq!(data.events.len(), 2);
        assert_eq!(task_meta(&data.task_cache, "o1").map(|m| m.title.as_str()), Some("Open One"));
        assert!(task_meta(&data.task_cache, "d1").is_none());
        assert!(calls.load(std::sync::atomic::Ordering::SeqCst) > 0, "then the tracker is asked");
        assert_eq!(read.data.events.len(), 2);
        assert!(!read.more_titles);
        shelbi_state::clear_test_gh_runner();

        // A filesystem board resolves locally: one read, no early one.
        let fs_home = temp_dir("staged-fs");
        std::env::set_var("SHELBI_HOME", &fs_home);
        fs::write(fs_home.join("events.log"), log_lines(3)).unwrap();
        let cache = Mutex::new(ActivityMetaCache::default());
        let read = read_activity_window_staged("demo", &cache, ACTIVITY_PAGE, |_| panic!("no early read"));
        assert_eq!(read.data.events.len(), 3);

        std::env::remove_var("SHELBI_HOME");
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&fs_home);
    }
}
