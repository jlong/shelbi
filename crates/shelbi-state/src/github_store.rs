//! The GitHub issues backend for the [`IssueStore`] seam.
//!
//! A project whose `issue_tracker.backend` is `github` keeps its board *as*
//! GitHub issues in a single repo (`owner/repo`). This module implements the
//! full contract — the read half (`list` / `list_in_status` / `get` /
//! `list_comments`, plus the `poll_changes` watermark) and the write half
//! (`add` / `move_status` / `set_priority` / `set_fields` / `cancel` /
//! `add_comment`) — by driving the GitHub REST API live through the `gh` CLI.
//!
//! ## Writing a shelbi mutation back onto a GitHub issue (plan §3/§5)
//!
//! The write path is the inverse of the read mapping:
//!
//! * **`add`** creates an issue carrying a `shelbi:id/<slug>` label, a
//!   `shelbi:status/<column>` label, and the fenced `<!-- shelbi:begin -->`
//!   metadata block (workflow / branch / depends_on / prefers_machine /
//!   priority / zen / launch / params). Creating into a terminal status closes
//!   the issue.
//! * **`move_status`** swaps the single `shelbi:status/*` label (keeping every
//!   other label) and opens / closes the issue when the target status is
//!   terminal — `done` closes as `completed`, `canceled` as `not_planned`, so
//!   the read path recovers which terminal even from GitHub state alone.
//! * **`set_fields` / `set_priority`** rewrite only the fenced metadata block in
//!   the issue body, never the human prose around it (the round-trip-safety
//!   contract). Priority is a plain integer in that block, renumbered
//!   client-side across a column exactly like the filesystem board.
//! * **`add_comment`** posts a native issue comment.
//!
//! Workspace assignment (`assigned_to`) is deliberately *not* written to GitHub
//! (plan §3): it is ephemeral local routing, not board state a github.com viewer
//! should see. Instead it is persisted to the **local assignment overlay**
//! (`crate::set_task_assignment` / `crate::get_task_assignment`, marker files
//! under `<project_dir>/assignments/`). [`GitHubStore::set_fields`] writes the
//! overlay for that one field; every read ([`GitHubStore::get`] /
//! [`GitHubStore::list`]) folds the overlay back onto the reconstructed issue,
//! so the active-workspace, conflict, and supervision scans still recover which
//! workspace owns a card even though the tracker stores no assignment. The
//! unassigning transitions ([`GitHubStore::move_status_and_unassign`] /
//! [`GitHubStore::cancel`] / [`GitHubStore::park_review`]) clear the overlay.
//!
//! ## Label hygiene — auto-create on first use (plan §6)
//!
//! Every write that applies a `shelbi:status/*` (or `shelbi:id/*`) label first
//! ensures the label exists in the repo, creating any that are missing. The
//! creation is idempotent: a label that already exists is left untouched
//! (GitHub's "already_exists" is swallowed), so first-use bootstrap and steady
//! state both work without a separate provisioning step.
//!
//! ## No content cache — live reads only (plan D3)
//!
//! Deliberately, nothing is mirrored to disk. Every read hits the API through
//! `gh`; when the tracker is unreachable the call surfaces a clear
//! [`Error::Command`] rather than rendering stale data. The only cross-call
//! state is the [`Cursor`] watermark threaded through [`poll_changes`] for
//! change detection — a last-seen `updated_at`, not a copy of any issue's
//! content. The caller (the orchestrator heartbeat) owns and persists that
//! cursor; the store holds no board state of its own.
//!
//! ## Mapping a GitHub issue onto an [`Issue`] (plan §3)
//!
//! One shelbi issue ⇔ one GitHub issue:
//!
//! * **id** — the `shelbi:id/<slug>` label (the round-trip anchor). GitHub
//!   caps a label at 50 chars, so an id whose label would overflow is anchored
//!   under a truncated `shelbi:id/<slug>-<hash8>` label and carries its full id
//!   in the metadata block; the read path prefers that block, falling back to
//!   the label, then the issue number for an un-migrated issue. Because the
//!   label stays a deterministic function of the id, `get(id)` still resolves
//!   with a single server-side `labels=` query.
//! * **status** — the `shelbi:status/<id>` label. A *closed* issue always maps
//!   to a terminal status (`done`, or `canceled` when GitHub's `state_reason`
//!   is `not_planned`), so closing an issue on github.com reads as a terminal
//!   card regardless of a stale non-terminal label.
//! * **shelbi-only fields** (`workflow` / `branch` / `depends_on` /
//!   `prefers_machine` / `priority` / `zen` / `launch`) — parsed from the fenced
//!   `<!-- shelbi:begin -->` … `<!-- shelbi:end -->` YAML block in the issue
//!   body. The block is stripped from [`IssueFile::body`] so the human prose and
//!   the shelbi metadata never clobber each other.
//! * **assignment** (`assigned_to`) — never read from GitHub. The raw mapping
//!   ([`GhIssue::into_issue_file`]) always yields `None`; the [`IssueStore`]
//!   read methods then fold in the local assignment overlay (see above), so a
//!   consumer sees the owning workspace without the tracker storing it.
//!
//! ## Testability
//!
//! The `gh` invocation is a single injectable closure ([`GhRunner`]). The
//! production constructor ([`GitHubStore::new`]) builds a runner that resolves
//! auth via [`crate::resolve_github_token`] and shells out to `gh api`; unit
//! tests inject a closure that returns canned JSON, so every mapping rule is
//! exercised without a network or a real repo.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{
    Column, Error, IssueLaunchConfig, IssueZenConfig, Result, DEFAULT_WORKFLOW_NAME,
};

use crate::issue_store::{BoardRead, Cursor, IssueChange, IssueComment, IssueFields, IssueStore, NewIssue, PrioMove, StatusMove};
use crate::{resolve_github_token_by_name, IssueFile, SecretToken};
use shelbi_core::Issue;

/// Label prefix carrying an issue's stable shelbi id (`shelbi:id/<slug>`).
const ID_LABEL_PREFIX: &str = "shelbi:id/";
/// Label prefix carrying an issue's shelbi status (`shelbi:status/<id>`).
const STATUS_LABEL_PREFIX: &str = "shelbi:status/";
/// GitHub's hard cap on a label name (`name is too long (maximum is 50
/// characters)`, HTTP 422). The id anchor label must fit inside this.
const GITHUB_LABEL_MAX: usize = 50;
/// Byte budget for the truncated slug in a long id's anchor label:
/// `50 - 10 (prefix) - 1 (separator) - 8 (hash) = 31`.
const ID_SLUG_BUDGET: usize = GITHUB_LABEL_MAX - ID_LABEL_PREFIX.len() - 1 - 8;
/// Opening marker of the fenced shelbi-metadata block in an issue body.
const META_BEGIN: &str = "<!-- shelbi:begin -->";
/// Closing marker of the fenced shelbi-metadata block in an issue body.
const META_END: &str = "<!-- shelbi:end -->";

/// A `gh` invocation: given the args that follow `gh`, yield stdout on success
/// or a typed [`Error`] on failure. Boxed so the production runner (real `gh`
/// with resolved auth) and a test runner (canned JSON) are interchangeable.
type GhRunner = Arc<dyn Fn(&[&str]) -> Result<String> + Send + Sync>;

// --- test-support: injectable `gh` runner ------------------------------------
//
// A consumer crate (the CLI) needs to exercise its command paths — which resolve
// a store from project config via `resolve_issue_store` and can't be handed a
// store directly — against a GitHub backend without a network. An override lets
// a test install a canned `gh` runner that every `GitHubStore::new` picks up,
// returning canned JSON instead of shelling out.
//
// It is **process-global** (not thread-local): a cached remote board refreshes
// on a background thread that builds its *own* store via
// [`crate::issue_store::build_store`] (see [`crate::issue_cache`]), so a
// thread-local override the refresh thread can't see would let that thread make
// a real `gh` call — the live-call-under-test bug this guards against. Every
// test that installs a runner already serializes on the shared test lock, so a
// process-global cell doesn't let parallel tests clobber each other. Gated to
// test builds so it never ships.

#[cfg(any(test, feature = "test-support"))]
static GH_RUNNER_OVERRIDE: std::sync::RwLock<Option<GhRunner>> =
    std::sync::RwLock::new(None);

#[cfg(any(test, feature = "test-support"))]
fn test_gh_runner_override() -> Option<GhRunner> {
    GH_RUNNER_OVERRIDE
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Install a fake `gh` runner process-wide: every [`GitHubStore`] built after
/// this — including those resolved from project config through
/// [`crate::resolve_issue_store`] and the ones the cache's background refresh
/// thread builds — routes its `gh api` calls to `runner`, which returns canned
/// JSON. Test-only. Pair with [`clear_test_gh_runner`] so the override doesn't
/// leak into a later test.
#[cfg(any(test, feature = "test-support"))]
pub fn set_test_gh_runner<F>(runner: F)
where
    F: Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
{
    *GH_RUNNER_OVERRIDE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(Arc::new(runner));
}

/// Remove any `gh` runner installed by [`set_test_gh_runner`].
#[cfg(any(test, feature = "test-support"))]
pub fn clear_test_gh_runner() {
    *GH_RUNNER_OVERRIDE
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
}

// --- test-support: the read-path rate-limit park is inert under test ----------
//
// [`park_aware_read`] reacts to a live `gh` 403 by (a) probing `/rate_limit`
// (another live call), (b) parking the token in `gh-budget/`, and (c) appending
// a `board rate-limited` line to `events.log`. Those writes are keyed off the
// *current* process-wide `SHELBI_HOME`, so a background board-refresh thread
// that raced a real 403 from a rate-limited developer token would scribble into
// whichever test's home was mounted at that instant, poisoning an unrelated
// test's `events.log` assertions (the 44-failure cascade this fixes). In a test
// build those side effects are therefore suppressed by default; a test that
// deliberately exercises the park opts in with `set_test_park_side_effects`.

#[cfg(any(test, feature = "test-support"))]
static TEST_PARK_SIDE_EFFECTS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Whether the read-path park may run its `/rate_limit` probe and write its
/// shared state (`gh-budget/` + the `board rate-limited` events.log line).
/// Always true in a shipped build; in a test build it is gated on the
/// [`set_test_park_side_effects`] opt-in so a raced background refresh can't
/// poison a sibling test's home.
fn read_park_side_effects_enabled() -> bool {
    #[cfg(any(test, feature = "test-support"))]
    {
        TEST_PARK_SIDE_EFFECTS.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[cfg(not(any(test, feature = "test-support")))]
    {
        true
    }
}

/// Opt a test into the read-path park's shared-state writes, which are
/// otherwise inert under test (see [`read_park_side_effects_enabled`]). Pair the
/// `true` call with a `false` reset so the opt-in doesn't leak into a later test.
#[cfg(any(test, feature = "test-support"))]
pub fn set_test_park_side_effects(on: bool) {
    TEST_PARK_SIDE_EFFECTS.store(on, std::sync::atomic::Ordering::Relaxed);
}

/// The GitHub issues backend. A cheap handle: it holds the `owner/repo`
/// selector and the `gh` runner closure, and resolves everything else per call.
#[derive(Clone)]
pub struct GitHubStore {
    /// The project *name* (registry alias / on-disk dir). GitHub is the source
    /// of truth for board state, but the review-slot parked markers are local
    /// daemon state under `<project_dir>/parked-review/`, so park/clear still
    /// need the project name to reach them.
    project: String,
    repo: String,
    gh: GhRunner,
    /// The runner for GraphQL reads (the board index). It shares `gh`'s token
    /// resolution but deliberately **not** its read-park governor: GraphQL is a
    /// separate points budget from the REST hourly limit, so a board read must
    /// survive REST exhaustion — the whole reason the board index moved to
    /// GraphQL. See [`GitHubStore::new`].
    graphql: GhRunner,
}

impl std::fmt::Debug for GitHubStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubStore")
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
}

impl GitHubStore {
    /// A store bound to `repo` (`owner/repo`) for the project named `project`,
    /// using the real `gh` CLI. Auth is resolved lazily per call via
    /// [`crate::resolve_github_token_by_name`] and handed to the `gh` subprocess
    /// as `GH_TOKEN`, so all three token sources (env / keychain / out-of-repo
    /// `tokens.yml`) funnel through one place. The project *name* is enough to
    /// locate the out-of-repo `tokens.yml`, so the store never has to hold (or
    /// re-load) a full `Project`. Nothing secret is written down.
    pub fn new(project: impl Into<String>, repo: impl Into<String>) -> Self {
        let project = project.into();
        let repo = repo.into();
        // Test builds may install a thread-local `gh` runner override so a
        // consumer crate can drive its command paths through a GitHub-configured
        // store (resolved via `resolve_issue_store`) against canned JSON instead
        // of a real repo. Absent an override — always, in a normal build — this
        // is the real `gh` CLI with resolved auth.
        #[cfg(any(test, feature = "test-support"))]
        if let Some(runner) = test_gh_runner_override() {
            // A test builds a fresh store per command; start it with clean
            // process-global caches so a number/issue one test cached can't be
            // served to another (both statics outlive a single test).
            clear_issue_caches_for_test();
            return Self {
                project,
                repo,
                gh: runner.clone(),
                graphql: runner,
            };
        }
        let project_for_write = project.clone();
        let base: GhRunner = Arc::new(move |args: &[&str]| run_gh(&project_for_write, args));
        // Two retry policies, chosen per call by HTTP method:
        //
        // * Mutating calls (`POST`/`PATCH`/`PUT`/`DELETE` — create, move, edit,
        //   comment, labels, and so the whole `issue-store migrate` loop) get the
        //   long production policy: a rate limit (esp. the secondary
        //   content-creation limit a bulk migration trips) or a transient blip is
        //   waited out with backoff, honoring any `Retry-After`.
        // * Read calls (`GET`) get the fail-fast read policy: when the *primary*
        //   hourly limit is exhausted `gh` reports `API rate limit exceeded` with
        //   no `Retry-After`, and blocking every board read for minutes behind
        //   backoff would stall dialog detection, marker handling and dispatch —
        //   so a hintless rate limit fails immediately there.
        //
        // See [`crate::gh_retry`].
        let read_policy = crate::gh_retry::RetryPolicy::reads();
        let write_policy = crate::gh_retry::RetryPolicy::production();
        let project_for_read = project.clone();
        let gh: GhRunner = Arc::new(move |args: &[&str]| {
            if is_mutating_gh(args) {
                return write_policy.run(|| base(args));
            }
            // Read path. The primary REST budget is per token and shared across
            // every shelbi process on the hub; when it is exhausted, retrying
            // per-caller-per-tick (six pollers, the sidebar, the daemon drain)
            // just re-issues thousands of 403s an hour and drives destructive
            // decisions off failed reads. So the first read-path 403/429 *parks*
            // this token until its reset (plan Phase 0, item 3): after that a
            // read short-circuits before spawning `gh`, returning the same typed
            // rate-limit error the live call would — the cache keeps serving its
            // last snapshot, marked stale, and no further request is made.
            park_aware_read(&project_for_read, &read_policy, args)
        });
        // GraphQL board reads share the REST token resolution (`run_gh`: env →
        // keychain → out-of-repo `tokens.yml`) and the fail-fast read retry
        // policy, but skip the REST read-park: GitHub prices GraphQL points on a
        // separate 5,000/hour budget, so a board read stays available even when
        // the REST hourly limit is exhausted. The Phase 3 governor adds a
        // GraphQL-budget reserve of its own.
        let graphql_project = project.clone();
        let graphql_read_policy = crate::gh_retry::RetryPolicy::reads();
        let graphql: GhRunner = Arc::new(move |args: &[&str]| {
            graphql_governed_read(&graphql_project, &graphql_read_policy, args)
        });
        Self {
            project,
            repo,
            gh,
            graphql,
        }
    }

    /// Construct a store over an arbitrary `gh` runner. The production seam for
    /// tests: inject a closure returning canned JSON instead of shelling out.
    /// `pub(crate)` under `cfg(test)` so the `issue_cache` tests can wrap a
    /// recording store in a [`crate::issue_cache::CachedIssueStore`] and assert
    /// the exact `-f state=…` each cached read path sends through to `gh`.
    #[cfg(test)]
    pub(crate) fn with_runner(
        repo: impl Into<String>,
        runner: impl Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
    ) -> Self {
        // Clean process-global caches so one test's cached number/issue can't
        // be served to the next (see [`clear_issue_caches_for_test`]).
        clear_issue_caches_for_test();
        let gh: GhRunner = Arc::new(runner);
        Self {
            project: "test-project".to_string(),
            repo: repo.into(),
            graphql: Arc::clone(&gh),
            gh,
        }
    }

    /// Like [`GitHubStore::with_runner`] but routes the injected runner through
    /// `policy`, so a test can drive the retry/backoff seam (e.g. a simulated
    /// 429) against canned responses without a network or a real wait.
    #[cfg(test)]
    fn with_runner_and_policy(
        repo: impl Into<String>,
        policy: crate::gh_retry::RetryPolicy,
        runner: impl Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
    ) -> Self {
        let base: GhRunner = Arc::new(runner);
        let gh: GhRunner = Arc::new(move |args: &[&str]| policy.run(|| base(args)));
        Self {
            project: "test-project".to_string(),
            repo: repo.into(),
            graphql: Arc::clone(&gh),
            gh,
        }
    }

    /// Like [`GitHubStore::with_runner_and_policy`] but selects between a read
    /// and a write policy per call by HTTP method, exactly as [`GitHubStore::new`]
    /// does — so a test can assert that the `GET` path fails fast while the
    /// mutating path still retries.
    #[cfg(test)]
    fn with_runner_and_policies(
        repo: impl Into<String>,
        read_policy: crate::gh_retry::RetryPolicy,
        write_policy: crate::gh_retry::RetryPolicy,
        runner: impl Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
    ) -> Self {
        let base: GhRunner = Arc::new(runner);
        let gh: GhRunner = Arc::new(move |args: &[&str]| {
            let policy = if is_mutating_gh(args) {
                &write_policy
            } else {
                &read_policy
            };
            policy.run(|| base(args))
        });
        Self {
            project: "test-project".to_string(),
            repo: repo.into(),
            graphql: Arc::clone(&gh),
            gh,
        }
    }

    /// The `owner/repo` this store reads.
    pub fn repo(&self) -> &str {
        &self.repo
    }

    /// Run `gh api` for a REST endpoint under this repo, streaming each element
    /// of the returned JSON array as one object per line (`--jq '.[]'`, with
    /// `--paginate` following `Link` headers). Returns the parsed issue objects.
    /// `extra` carries additional `-f key=value` query params (e.g. a label
    /// filter or a `since=` watermark).
    fn api_issues(&self, path: &str, extra: &[&str]) -> Result<Vec<GhIssue>> {
        let mut args: Vec<&str> = vec!["api", "-X", "GET", path, "--paginate"];
        args.extend_from_slice(extra);
        // One JSON object per line, so paginated arrays never concatenate into
        // invalid JSON — parse line by line.
        args.extend_from_slice(&["--jq", ".[]"]);
        let out = (self.gh)(&args)?;
        parse_jsonl(&out)
    }

    /// List the repo's issues at a single GitHub `state` (`open` / `closed` /
    /// `all`), mapped onto `IssueFile`s with the local assignment overlay folded
    /// in and the board sorted into canonical order. The shared core of
    /// [`GitHubStore::list`], [`GitHubStore::list_open`] and
    /// [`GitHubStore::list_in_status`], which differ only in the `-f state=` they
    /// send — the whole point of the split is that a render/poll read pulls one
    /// page of open issues, not the six-page `state=all` history.
    fn list_with_state(&self, state: &str) -> Result<Vec<IssueFile>> {
        let path = format!("repos/{}/issues", self.repo);
        let state_arg = format!("state={state}");
        let issues = self.api_issues(&path, &["-f", &state_arg, "-f", "per_page=100"])?;
        // Fold the local assignment overlay onto every issue in one read, rather
        // than statting a marker file per card.
        let assignments = crate::task_assignments(&self.project)?;
        let mut out: Vec<IssueFile> = issues
            .into_iter()
            .filter(|gh| !gh.is_pull_request())
            .map(|gh| {
                let mut tf = gh.into_issue_file();
                tf.task.assigned_to = assignments.get(&tf.task.id).cloned();
                tf
            })
            .collect();
        sort_board(&mut out);
        Ok(out)
    }
}

impl IssueStore for GitHubStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        // Full history — `state=all`, a ~six-page sweep on a large board. Kept
        // for the migrate / reconcile / dependency-resolution paths that
        // genuinely need every issue, closed ones included; no render or poll
        // path calls this. Those read [`GitHubStore::list_open`] (or a
        // [`GitHubStore::list_in_status`] scoped by state), which request only
        // the open (or only the closed) issues they need.
        self.list_with_state("all")
    }

    fn list_open(&self) -> Result<Vec<IssueFile>> {
        // Everything on the board except history. A closed GitHub issue always
        // maps to a terminal `done`/`canceled` status (see the module doc), so
        // `state=open` is exactly the non-terminal board the pollers, sidebar,
        // `zen scan`, orchestrator drain and unfiltered `issue list` render and
        // route from — one page for a board under 100 open issues, versus the
        // six-page `state=all` sweep every one of them used to pay.
        self.list_with_state("open")
    }

    fn list_closed(&self) -> Result<Vec<IssueFile>> {
        // The terminal history in a single `state=closed` sweep — a closed
        // GitHub issue always maps to `done`/`canceled` (see the module doc),
        // so this is exactly the two terminal lanes. The process cache serves
        // both of them by filtering this one read, instead of paying a
        // per-column closed sweep for each.
        self.list_with_state("closed")
    }

    fn refresh_board(
        &self,
        since: Option<DateTime<Utc>>,
        previous: &[IssueFile],
    ) -> Result<BoardRead> {
        // The daemon's board-index read, on the separate GraphQL points budget.
        // The local assignment overlay is folded onto the *whole* board every
        // tick, cold or incremental: `assigned_to` is local routing that never
        // bumps a GitHub issue's `updatedAt`, so it can't ride in an incremental
        // delta — re-reading it here keeps ownership fresh even on a quiet board.
        // Run the GraphQL read, but fall back to the REST open list on any
        // failure that isn't a rate limit — a GHES host without `filterBy.since`,
        // a token missing GraphQL scope, or a transport blip. A rate limit still
        // propagates so the daemon leaves the index untouched and the Phase 3
        // governor sees it. Whichever path we're on is logged once per repo, not
        // per tick.
        let graphql = match since {
            // Cold read: every open issue in one paginated GraphQL query
            // (`states: [OPEN]`), which excludes pull requests for free.
            None => self.graphql_open_board(),
            // Incremental tick: only issues touched since the last refresh
            // (`filterBy: { since }`, no `states` filter), so a just-closed issue
            // comes back and is dropped from the open index — done/canceled
            // transitions reach the board without ever listing history. A quiet
            // board returns zero nodes and costs a single point.
            Some(since) => self.graphql_board_delta(since),
        };
        let page = match graphql {
            Ok(page) => {
                self.note_graphql_recovered();
                page
            }
            // A rate limit is terminal for this tick: propagate it (no REST
            // fallback — the two budgets are separate, and hammering REST on a
            // GraphQL limit helps nobody).
            Err(e) if crate::gh_retry::is_rate_limit_error(&e) => return Err(e),
            // Any other failure: serve the REST open list as a full board. The
            // delta path falls back the same way — a full REST open read simply
            // replaces the previous board.
            Err(e) => {
                self.note_graphql_fallback(&e);
                let board = self.list_with_state("open")?;
                // The degraded REST list doesn't surface issue numbers, so the
                // index carries none this tick and a `get` falls back to search
                // until the GraphQL path recovers.
                return Ok(BoardRead {
                    board,
                    numbers: Vec::new(),
                    remaining: None,
                    reset: None,
                });
            }
        };

        let assignments = crate::task_assignments(&self.project)?;
        match since {
            None => {
                // Cold read: capture every open issue's number alongside its
                // mapped card, so the daemon can publish the full id→number map.
                let mut numbers: Vec<(String, i64)> = Vec::with_capacity(page.issues.len());
                let mut board: Vec<IssueFile> = page
                    .issues
                    .into_iter()
                    .map(|gh| {
                        let number = gh.number;
                        let tf = fold_assignment(gh.into_issue_file(), &assignments);
                        numbers.push((tf.task.id.clone(), number));
                        tf
                    })
                    .collect();
                sort_board(&mut board);
                Ok(BoardRead {
                    board,
                    numbers,
                    remaining: page.remaining,
                    reset: page.reset,
                })
            }
            Some(_) => {
                let mut by_id: std::collections::HashMap<String, IssueFile> = previous
                    .iter()
                    .map(|tf| (tf.task.id.clone(), tf.clone()))
                    .collect();
                // Numbers for the issues this delta actually saw and left open;
                // the daemon merges them onto the prior index's map, so untouched
                // issues keep the number an earlier tick recorded.
                let mut numbers: Vec<(String, i64)> = Vec::new();
                for gh in page.issues {
                    // The index is the GitHub-*open* set. A closed issue (however
                    // its stale status label reads) leaves the open board; every
                    // other touched issue is upserted in place.
                    let closed = gh.is_closed();
                    let number = gh.number;
                    let tf = gh.into_issue_file();
                    if closed {
                        by_id.remove(&tf.task.id);
                    } else {
                        numbers.push((tf.task.id.clone(), number));
                        by_id.insert(tf.task.id.clone(), tf);
                    }
                }
                let mut board: Vec<IssueFile> = by_id
                    .into_values()
                    .map(|tf| fold_assignment(tf, &assignments))
                    .collect();
                sort_board(&mut board);
                Ok(BoardRead {
                    board,
                    numbers,
                    remaining: page.remaining,
                    reset: page.reset,
                })
            }
        }
    }

    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        // A closed issue is always terminal (`done`/`canceled`); an open issue
        // never is. So a terminal-status query needs only the closed issues and
        // any other status needs only the open ones — never the whole
        // `state=all` history. Request exactly that state, then filter in
        // memory: the status lives in a label we already parse, so this keeps a
        // single mapping path (no second query shape to keep in sync), and the
        // read has already applied the assignment overlay so the
        // conflict/active-workspace scans see the owner.
        let state = if is_terminal(status) { "closed" } else { "open" };
        Ok(self
            .list_with_state(state)?
            .into_iter()
            .filter(|tf| tf.task.column == *status)
            .collect())
    }

    fn closed_page(&self, after: Option<&str>) -> Result<crate::issue_store::ClosedPage> {
        // The done/canceled history on demand (plan §4): one GraphQL page of 50
        // closed issues, newest-updated first, never the six-page `state=closed`
        // REST sweep. Falls back to the REST closed list on any non-rate-limit
        // failure (a GHES gap, a token scope, a transport blip), matching
        // `refresh_board`; a rate limit propagates so the caller leaves the
        // cached page untouched. Which path is active is logged once per repo.
        match self.graphql_closed_page(after) {
            Ok(page) => {
                self.note_graphql_recovered();
                let assignments = crate::task_assignments(&self.project)?;
                let issues = page
                    .issues
                    .into_iter()
                    .map(|gh| fold_assignment(gh.into_issue_file(), &assignments))
                    .collect();
                Ok(crate::issue_store::ClosedPage {
                    issues,
                    next_cursor: page.next_cursor,
                    remaining: page.remaining,
                    reset: page.reset,
                })
            }
            Err(e) if crate::gh_retry::is_rate_limit_error(&e) => Err(e),
            Err(e) => {
                self.note_graphql_fallback(&e);
                // REST fallback: the full closed sweep as one page (no cursor),
                // ordered newest-closed first so the degraded path still reads
                // like the GraphQL one.
                let mut issues = self.list_with_state("closed")?;
                issues.sort_by_key(|f| std::cmp::Reverse(f.task.updated_at));
                Ok(crate::issue_store::ClosedPage {
                    issues,
                    next_cursor: None,
                    remaining: None,
                    reset: None,
                })
            }
        }
    }

    fn get(&self, id: &str) -> Result<Option<IssueFile>> {
        shelbi_core::validate_task_id(id)?;
        // The fresh single-issue path (`Plans/github-issue-caching-and-rate-limits.md`
        // §3). Resolve the id to a GitHub `number` — from the process-local cache,
        // then the published index's id→number map — and fetch that one issue
        // through GraphQL (one point), instead of the old eventually-consistent
        // label-filtered REST list. Every action path (`issue show`/`start`/
        // `resume`/`move`/`edit`/`prio`, review-slot load, `zen probe`, the
        // ready-marker handoff, the review→done transition) reads through here, so
        // each acts on the latest body and status, never a stale index copy.
        //
        // Fast path: a number the index or a prior read already resolved. It is
        // verified against the fetched issue's id, so a (practically impossible)
        // stale mapping can never return the wrong issue — it just falls through
        // to the authoritative search.
        if let Some(number) = self.cached_number(id).or_else(|| self.index_number(id)) {
            if let Some(tf) = self.fetch(number)? {
                if tf.task.id == id {
                    self.remember_number(id, number);
                    self.refresh_index_entry(&tf, number);
                    return Ok(Some(tf));
                }
            }
            // The cached/index number no longer names this id — re-resolve.
            self.forget_number(id);
        }
        // Search fallback: an id the open index does not carry — a done/canceled
        // task, or one added since the last daemon tick. One GraphQL point, and
        // (unlike REST label search) the number it returns is fetched directly,
        // so the eventual-consistency window only ever delays *resolution*, never
        // returns a stale body.
        let Some(number) = self.search_number(id)? else {
            return Ok(None);
        };
        let Some(tf) = self.fetch(number)? else {
            return Ok(None);
        };
        if tf.task.id != id {
            return Ok(None);
        }
        self.remember_number(id, number);
        self.refresh_index_entry(&tf, number);
        Ok(Some(tf))
    }

    fn fetch_many(&self, ids: &[&str]) -> Result<Vec<IssueFile>> {
        // Resolve every id to a number (cache / index / search), then pull them
        // all in one aliased GraphQL request — the batch read the orchestrator
        // drain and `zen scan` use when they need fresh copies of a handful of
        // specific issues rather than the whole published board. Ids that don't
        // resolve are simply absent from the result, matching the default's
        // "present issues only, never an error" contract.
        let mut numbers: Vec<i64> = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(n) = self.resolve_number(id)? {
                numbers.push(n);
            }
        }
        numbers.sort_unstable();
        numbers.dedup();
        let fetched = self.fetch_many_numbers(&numbers)?;
        // Warm the per-number cache and the id→number map from the batch, but
        // leave the published index to the daemon: a bulk fresh read is a
        // decision input, not a sidebar update, and rewriting the index file
        // once per issue would be needless write amplification.
        for (tf, number) in &fetched {
            self.cache_issue(*number, tf);
            self.remember_number(&tf.task.id, *number);
        }
        Ok(fetched.into_iter().map(|(tf, _)| tf).collect())
    }

    fn add(&self, spec: NewIssue) -> Result<Issue> {
        shelbi_core::validate_task_id(&spec.id)?;
        // A card that already exists (same shelbi id) must not be silently
        // duplicated into a second issue — `add` is create-exclusive, matching
        // the filesystem backend's no-overwrite guarantee.
        if self.get_raw(&spec.id)?.is_some() {
            return Err(Error::Other(format!(
                "issue `{}` already exists in {}",
                spec.id, self.repo
            )));
        }
        // Priority: an explicit slot wins; otherwise append to the destination
        // status (priority = current count), the same rule the filesystem
        // backend uses. Priority is a plain int in the metadata block.
        let priority = match spec.priority {
            Some(p) => p,
            None => self.list_in_status(&spec.column)?.len() as u32,
        };

        let id_anchor = id_label(&spec.id);
        let status_label = status_label_name(&spec.column);
        // Auto-create every label this write applies (id + the status set) so a
        // fresh repo bootstraps without a separate provisioning step.
        self.ensure_labels(&self.bootstrap_labels(&[id_anchor.clone(), status_label.clone()]))?;

        let mut meta = meta_from_new(&spec, priority);
        // When the anchor label had to be truncated to fit GitHub's 50-char cap
        // it no longer carries the full id, so stamp the authoritative id into
        // the body block for a lossless read-back. A short id round-trips
        // through its verbatim label alone, so its body stays unchanged.
        if id_is_truncated(&spec.id) {
            meta.id = Some(spec.id.clone());
        }
        let body = build_body(&spec.body, &meta);

        let fields = vec![
            ("title", spec.title.clone()),
            ("body", body),
            ("labels[]", id_anchor),
            ("labels[]", status_label),
        ];
        let out = self.api_send("POST", &format!("repos/{}/issues", self.repo), &fields)?;
        let created: GhIssue = parse_json_object(&out)?;

        // Record the number the create returned so an immediate `get(id)` after
        // `add` resolves it directly (plan open question: "`add` followed by an
        // immediate `get` should use the number the create returned, not the
        // search", which is eventually consistent). A non-terminal card also
        // seeds the published index's id→number map for other processes before
        // the next daemon tick; a terminal card stays off the open index.
        self.remember_number(&spec.id, created.number);
        if !is_terminal(&spec.column) {
            let _ = crate::board_index::record_board_index_number(
                &self.project,
                &spec.id,
                created.number,
            );
        }

        // Creating straight into a terminal status closes the issue, so the
        // board and GitHub agree the moment the card exists.
        if is_terminal(&spec.column) {
            self.set_state(created.number, "closed", terminal_reason(&spec.column))?;
        }

        Ok(Issue {
            id: spec.id,
            title: spec.title,
            column: spec.column,
            priority,
            // Assignment is ephemeral local routing, never stored on GitHub.
            assigned_to: None,
            workflow: spec.workflow,
            branch: spec.branch,
            depends_on: spec.depends_on,
            prefers_machine: spec.prefers_machine,
            zen: spec.zen,
            launch: spec.launch,
            created_at: created.created_at,
            updated_at: created.updated_at,
            params: spec.params,
        })
    }

    fn move_status(&self, id: &str, to: &Column, _reason: &str) -> Result<Option<StatusMove>> {
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let from = gh.column();
        if from == *to {
            // Already there — no label swap, no event. Mirrors the filesystem
            // backend returning `None` for a no-op move.
            return Ok(None);
        }
        let workflow = gh.workflow_name();

        // Ensure the destination status label exists before applying it.
        let to_label = status_label_name(to);
        self.ensure_labels(std::slice::from_ref(&to_label))?;

        // Swap the status label: keep every non-status label (the id anchor and
        // any human-added labels), replace the single `shelbi:status/*` one.
        let mut labels = gh.non_status_labels();
        labels.push(to_label);
        self.set_labels(gh.number, &labels)?;

        // Terminal target closes the issue (recording which terminal via
        // `state_reason`); a non-terminal target reopens a closed issue so a
        // reopened card leaves the terminal lane.
        if is_terminal(to) {
            self.set_state(gh.number, "closed", terminal_reason(to))?;
        } else if gh.state == "closed" {
            self.set_state(gh.number, "open", None)?;
        }

        Ok(Some(StatusMove {
            from,
            to: to.clone(),
            workflow,
        }))
    }

    fn set_priority(&self, id: &str, pos: PrioMove) -> Result<()> {
        let Some(target) = self.get(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        // Reorder within the issue's current status, then renumber the column to
        // contiguous 0..N — the same client-side ordering the read path sorts
        // by and the filesystem backend maintains on disk.
        let mut column = self.list_in_status(&target.task.column)?;
        let Some(idx) = column.iter().position(|tf| tf.task.id == id) else {
            return Err(Error::Other(format!("issue `{id}` not in its own status?")));
        };
        let last = column.len().saturating_sub(1);
        let dest = match pos {
            PrioMove::Top => 0,
            PrioMove::Bottom => last,
            PrioMove::Up => idx.saturating_sub(1),
            PrioMove::Down => (idx + 1).min(last),
            PrioMove::Set(n) => (n as usize).min(last),
        };
        if dest == idx {
            return Ok(());
        }
        let moved = column.remove(idx);
        column.insert(dest, moved);
        // Only rewrite the issues whose integer priority actually changed.
        for (new_prio, tf) in column.iter().enumerate() {
            if tf.task.priority != new_prio as u32 {
                self.rewrite_priority(&tf.task.id, new_prio as u32)?;
            }
        }
        Ok(())
    }

    fn set_fields(&self, id: &str, fields: IssueFields) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        // `assigned_to` is ephemeral local routing (plan §3) — never stored on
        // GitHub. It is persisted to the local assignment overlay instead, so an
        // assign/start/resume through this seam is recoverable by the daemon's
        // ownership scans. Apply it first so an `assigned_to`-only update still
        // lands (it just touches nothing on the remote).
        if let Some(assigned_to) = &fields.assigned_to {
            crate::set_task_assignment(&self.project, id, assigned_to.as_deref())?;
        }
        if fields.branch.is_none()
            && fields.depends_on.is_none()
            && fields.prefers_machine.is_none()
            && fields.title.is_none()
            && fields.workflow.is_none()
            && fields.body.is_none()
        {
            // `assigned_to` was the only field — the overlay write above is the
            // whole operation; nothing to PATCH on the remote.
            return Ok(());
        }
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let body_raw = gh.body.clone().unwrap_or_default();
        let (mut prose, mut meta) = split_shelbi_meta(&body_raw);
        // The GitHub issue title and the shelbi body prose are stored natively;
        // branch / depends_on / prefers_machine / workflow live in the meta
        // block folded into the issue body. Accumulate a single PATCH.
        let mut params: Vec<(&str, String)> = Vec::new();
        let mut body_changed = false;
        if let Some(branch) = fields.branch {
            if meta.branch != branch {
                meta.branch = branch;
                body_changed = true;
            }
        }
        if let Some(depends_on) = fields.depends_on {
            if meta.depends_on != depends_on {
                meta.depends_on = depends_on;
                body_changed = true;
            }
        }
        if let Some(prefers_machine) = fields.prefers_machine {
            if meta.prefers_machine != prefers_machine {
                meta.prefers_machine = prefers_machine;
                body_changed = true;
            }
        }
        if let Some(workflow) = fields.workflow {
            if meta.workflow != workflow {
                meta.workflow = workflow;
                body_changed = true;
            }
        }
        if let Some(new_prose) = fields.body {
            if prose != new_prose {
                prose = new_prose;
                body_changed = true;
            }
        }
        if let Some(title) = fields.title {
            if gh.title != title {
                params.push(("title", title));
            }
        }
        if body_changed {
            params.push(("body", build_body(&prose, &meta)));
        }
        if params.is_empty() {
            return Ok(());
        }
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repo, gh.number),
            &params,
        )?;
        Ok(())
    }

    fn cancel(&self, id: &str, reason: &str) -> Result<Option<StatusMove>> {
        // Cancel = move-and-unassign to the terminal `canceled` status (closes
        // the issue as `not_planned`), clearing the local assignment overlay so a
        // canceled card isn't relaunched on its old workspace — the same
        // reasoning as the filesystem backend.
        self.move_status_and_unassign(id, &Column::canceled(), reason)
    }

    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        reason: &str,
    ) -> Result<Option<StatusMove>> {
        let mv = self.move_status(id, to, reason)?;
        // Drop the local assignment overlay regardless of whether the status
        // actually changed (matching the filesystem backend's move-and-unassign,
        // which clears the owner even on a no-op move).
        crate::set_task_assignment(&self.project, id, None)?;
        Ok(mv)
    }

    fn delete(&self, id: &str) -> Result<()> {
        // GitHub's REST API cannot hard-delete an issue, so `rm` maps to the
        // nearest durable effect: close it as `not_planned` (the same terminal
        // state a cancel reaches). Idempotent — a missing/closed issue is a
        // no-op. GraphQL `deleteIssue` exists but needs elevated repo scope, so
        // the REST close is the portable choice for the experimental backend.
        let Some(gh) = self.get_raw(id)? else {
            // Still drop any stale local assignment for a card GitHub no longer
            // knows about, so the overlay doesn't leak owners.
            crate::set_task_assignment(&self.project, id, None)?;
            return Ok(());
        };
        if gh.state != "closed" {
            self.set_state(gh.number, "closed", Some("not_planned"))?;
        }
        // A deleted card has no owner — clear the overlay.
        crate::set_task_assignment(&self.project, id, None)?;
        Ok(())
    }

    fn renumber(&self, status: &Column) -> Result<()> {
        // Rewrite the column's stored priorities to contiguous 0..N — the same
        // repair the filesystem backend does, expressed as the client-side
        // reorder GitHub priorities are maintained by.
        let column = self.list_in_status(status)?;
        for (idx, tf) in column.iter().enumerate() {
            if tf.task.priority != idx as u32 {
                self.rewrite_priority(&tf.task.id, idx as u32)?;
            }
        }
        Ok(())
    }

    fn park_review(&self, id: &str) -> Result<Option<String>> {
        // Parked markers are local daemon state for the review-slot auto-loader.
        // The prior owner lives in the local assignment overlay (GitHub stores
        // none), so report and clear it, then set the parked marker — mirroring
        // the filesystem backend's park (clear owner + mark parked in one step).
        let was = crate::get_task_assignment(&self.project, id)?;
        crate::set_task_assignment(&self.project, id, None)?;
        crate::set_task_parked(&self.project, id)?;
        Ok(was)
    }

    fn clear_parked(&self, id: &str) -> Result<()> {
        crate::clear_task_parked(&self.project, id)
    }

    fn reject_review(
        &self,
        id: &str,
        ready: &Column,
        reason: &str,
        date: &str,
    ) -> Result<Option<StatusMove>> {
        // The github expression of the atomic reject: append the reviewer's
        // feedback to the human prose (preserving the fenced metadata block),
        // then bounce the card back to `ready` and drop the local assignment
        // overlay. GitHub has no cross-request transaction, so the body edit and
        // the label swap are two calls; ordering the edit first means a failure
        // between them leaves the feedback recorded rather than a moved-but-
        // unedited card the next worker would pick up with no context.
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!(
                "issue `{id}` not found in {}",
                self.repo
            )));
        };
        let body_raw = gh.body.clone().unwrap_or_default();
        let (prose, meta) = split_shelbi_meta(&body_raw);
        let feedback = crate::format_review_feedback_section(reason, date);
        let new_prose = format!("{}{}", prose, feedback);
        let body = build_body(&new_prose, &meta);
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repo, gh.number),
            &[("body", body)],
        )?;
        // Move to `ready` and clear the owner overlay in one step — the same
        // move-and-unassign the filesystem reject performs. Returns `None` when
        // the card was already in `ready` (the body edit + unassign still land),
        // matching the filesystem backend.
        self.move_status_and_unassign(id, ready, "user:review-reject")
    }

    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        // Live read + watermark, no content cache (plan D3). The first poll
        // from `Cursor::start()` just establishes the high-water `updated_at`
        // and reports nothing; later edits (issue `updated_at` moved past the
        // cursor) surface as upserts, and comments on those same freshly-touched
        // issues (created past the cursor) surface as CommentAdded.
        let path = format!("repos/{}/issues", self.repo);
        // On a warm cursor, scope the list to issues touched at/after the
        // watermark (`since=`, plan D3 — "GitHub supports this cheaply"). On a
        // quiescent board this transfers next to nothing, so the pass stays well
        // inside the `gh` rate budget instead of re-listing the whole repo every
        // tick. The cold first poll has no watermark and lists the board once to
        // seed the high-water mark. `since=` is *inclusive* (updated_at >= w),
        // but the strictly-greater filter below drops an issue sitting exactly at
        // the watermark, so a change is never surfaced twice across polls.
        let since_param = since.watermark().map(|w| format!("since={}", w.to_rfc3339()));
        let mut extra: Vec<&str> = vec!["-f", "state=all", "-f", "per_page=100"];
        if let Some(param) = since_param.as_deref() {
            extra.push("-f");
            extra.push(param);
        }
        let issues = self.api_issues(&path, &extra)?;

        let mut changes = Vec::new();
        let mut high = since.watermark();
        let mut touched: Vec<(String, i64)> = Vec::new();

        for gh in issues {
            if gh.is_pull_request() {
                continue;
            }
            let updated = gh.updated_at;
            high = Some(high.map_or(updated, |h| h.max(updated)));
            if since.watermark().is_some_and(|w| updated > w) {
                let number = gh.number;
                let tf = gh.into_issue_file();
                touched.push((tf.task.id.clone(), number));
                changes.push(IssueChange::Upserted(Box::new(tf)));
            }
        }

        // Comment detection is scoped to issues whose `updated_at` advanced —
        // adding a comment bumps the issue's `updated_at`, so a new comment can
        // only be on one of those. Only meaningful once past `start`.
        if let Some(w) = since.watermark() {
            for (issue_id, number) in touched {
                // Scope the per-issue comment fetch with the same `since=`
                // watermark so we only pull comments GitHub touched since the
                // last poll (plan D3 "comments `since=`"), then keep only the
                // ones genuinely *created* past the cursor — an edit to a
                // pre-cursor comment is returned by `since=` but is not a new
                // comment, so the `created_at > w` filter drops it.
                for comment in self.comments_for_number_since(number, Some(w))? {
                    if comment.created_at > w {
                        high = Some(high.map_or(comment.created_at, |h| h.max(comment.created_at)));
                        changes.push(IssueChange::CommentAdded {
                            issue_id: issue_id.clone(),
                            comment,
                        });
                    }
                }
            }
        }

        Ok((changes, high.map_or_else(Cursor::start, Cursor::at)))
    }

    fn list_comments(&self, id: &str) -> Result<Vec<IssueComment>> {
        // Resolve the shelbi id to an issue number, then read its comments live.
        let Some(gh) = self.get_raw(id)? else {
            return Ok(Vec::new());
        };
        self.comments_for_number(gh.number)
    }

    fn add_comment(&self, id: &str, body: &str) -> Result<IssueComment> {
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let out = self.api_send(
            "POST",
            &format!("repos/{}/issues/{}/comments", self.repo, gh.number),
            &[("body", body.to_string())],
        )?;
        let created: GhComment = parse_json_object(&out)?;
        Ok(created.into_comment())
    }
}

impl GitHubStore {
    /// Resolve a shelbi id to the raw GitHub issue (number + fields), or `None`.
    fn get_raw(&self, id: &str) -> Result<Option<GhIssue>> {
        shelbi_core::validate_task_id(id)?;
        let path = format!("repos/{}/issues", self.repo);
        let label = format!("labels={}", id_label(id));
        let issues = self.api_issues(
            &path,
            &["-f", "state=all", "-f", &label, "-f", "per_page=100"],
        )?;
        let found = issues.into_iter().find(|gh| !gh.is_pull_request());
        if let Some(gh) = &found {
            // A write helper already fetched this issue's number; remember it so
            // the post-write `get` (the CachedIssueStore write-through) resolves
            // it from the cache and never pays a search.
            self.remember_number(id, gh.number);
        }
        Ok(found)
    }

    /// Split `owner/repo` into its two halves for the GraphQL `owner`/`name`
    /// variables. `validate()` already accepted the config, so this only guards
    /// against an empty or over-slashed value reaching the query.
    fn owner_and_name(&self) -> Result<(&str, &str)> {
        self.repo
            .split_once('/')
            .filter(|(owner, name)| !owner.is_empty() && !name.is_empty() && !name.contains('/'))
            .ok_or_else(|| {
                Error::InvalidIssueTracker(format!(
                    "issue_tracker.github.repo must be `owner/repo`, got `{}`",
                    self.repo
                ))
            })
    }

    /// Cold board read: every open issue via the paginated `BoardIndex` query.
    fn graphql_open_board(&self) -> Result<GraphQlBoardPage> {
        self.graphql_paginate(BOARD_INDEX_QUERY, None)
    }

    /// Incremental board read: issues touched at/after `since` (both open and
    /// just-closed), via the `BoardIndexDelta` query.
    fn graphql_board_delta(&self, since: DateTime<Utc>) -> Result<GraphQlBoardPage> {
        self.graphql_paginate(BOARD_INDEX_DELTA_QUERY, Some(since))
    }

    /// Record that this repo just fell back to the REST list, and warn **once**
    /// per fallback episode (not per tick). A repo already in the fallback set
    /// logs nothing until it recovers.
    fn note_graphql_fallback(&self, err: &Error) {
        let newly = graphql_fallback_repos()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(self.repo.clone());
        if newly {
            tracing::warn!(
                repo = %self.repo,
                error = %err,
                "GitHub GraphQL board read failed (not a rate limit); falling back to the REST open list",
            );
        }
    }

    /// Record that this repo's GraphQL board read succeeded, and — only if it was
    /// previously in the REST fallback — log **once** that it recovered. A repo
    /// that was never in fallback logs nothing.
    fn note_graphql_recovered(&self) {
        let was = graphql_fallback_repos()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.repo);
        if was {
            tracing::info!(
                repo = %self.repo,
                "GitHub GraphQL board read recovered; back on the GraphQL path",
            );
        }
    }

    /// One `gh api graphql` request for `query` with the optional `after` cursor
    /// and `since` watermark, parsed into its issues-connection page + budget.
    /// The single-page primitive both the paginated board read and the one-shot
    /// closed-page read are built on.
    fn graphql_request(
        &self,
        query: &str,
        after: Option<&str>,
        since: Option<&str>,
    ) -> Result<GraphQlResponsePage> {
        let (owner, name) = self.owner_and_name()?;
        let query_arg = format!("query={query}");
        let owner_arg = format!("owner={owner}");
        let name_arg = format!("name={name}");
        // All variables are passed with `-f` (raw string), which is the right
        // wire form for the `String`/`DateTime` GraphQL scalars these queries
        // declare and avoids `-F`'s magic coercion of a numeric-looking cursor or
        // timestamp.
        let mut args: Vec<&str> = vec![
            "api", "graphql", "-f", &query_arg, "-f", &owner_arg, "-f", &name_arg,
        ];
        let after_arg = after.map(|c| format!("after={c}"));
        if let Some(arg) = after_arg.as_deref() {
            args.push("-f");
            args.push(arg);
        }
        let since_arg = since.map(|s| format!("since={s}"));
        if let Some(arg) = since_arg.as_deref() {
            args.push("-f");
            args.push(arg);
        }
        let out = (self.graphql)(&args)?;
        parse_graphql_board_response(&out)
    }

    /// Drive `query` across every page, following `pageInfo.endCursor`, and
    /// collect the issue nodes (mapped onto the REST [`GhIssue`] shape so the one
    /// existing label/metadata mapping serves both backends). The token budget
    /// from the last page's `rateLimit` rides back on the result.
    fn graphql_paginate(
        &self,
        query: &str,
        since: Option<DateTime<Utc>>,
    ) -> Result<GraphQlBoardPage> {
        let since_str = since.map(|s| s.to_rfc3339());
        let mut after: Option<String> = None;
        let mut issues: Vec<GhIssue> = Vec::new();
        // The budget from the most recent page. Left uninitialized: the `loop`
        // body assigns it before any break, so it is definitely set by the time
        // it is read after the loop, and no dead initializer is flagged.
        let mut budget: Option<GhRateLimit>;

        loop {
            let page = self.graphql_request(query, after.as_deref(), since_str.as_deref())?;
            budget = page.rate_limit;
            issues.extend(page.connection.nodes.into_iter().map(GhIssueNode::into_gh_issue));

            match page.connection.page_info.end_cursor {
                Some(cursor) if page.connection.page_info.has_next_page => after = Some(cursor),
                // No next page — or `hasNextPage` with no cursor to advance on,
                // which we stop on rather than loop forever.
                _ => break,
            }
        }
        Ok(GraphQlBoardPage {
            issues,
            remaining: budget.as_ref().and_then(|r| r.remaining),
            reset: budget.and_then(|r| r.reset_at).map(|dt| dt.timestamp()),
        })
    }

    // --- fresh single-issue fetch (plan §3) ----------------------------------

    /// Fetch one issue by its GitHub `number`, fresh through GraphQL (one point),
    /// with a small per-number cache keyed by `updatedAt`.
    ///
    /// The cache is served only when the published board index shows **no newer**
    /// `updatedAt` for this issue than the cached copy — so a card that moved or
    /// was edited (its `updatedAt` advanced in the index) is always re-read live,
    /// while a repeat read of an unchanged issue in the same process costs no
    /// request. An issue the index doesn't carry (a done task, or no index yet)
    /// can't be proven fresh, so it is always fetched live. Returns `None` for a
    /// number that names no issue (deleted, or a pull request).
    fn fetch(&self, number: i64) -> Result<Option<IssueFile>> {
        if let Some(tf) = self.cached_issue_if_fresh(number)? {
            return Ok(Some(tf));
        }
        let Some(node) = self.graphql_single_issue(number)? else {
            self.forget_issue(number);
            return Ok(None);
        };
        let mut tf = node.into_gh_issue().into_issue_file();
        // Fold the local assignment overlay so a caller reads the owning
        // workspace even though the tracker stores no assignment.
        tf.task.assigned_to = crate::get_task_assignment(&self.project, &tf.task.id)?;
        self.cache_issue(number, &tf);
        self.remember_number(&tf.task.id, number);
        Ok(Some(tf))
    }

    /// The cached full issue for `number` when the published index does not show
    /// a newer `updatedAt` than the cached copy — the freshness gate the plan
    /// calls for ("invalidated whenever the index shows a newer `updatedAt` for
    /// that number"). `None` (a live fetch) on a cache miss, on an index that has
    /// a strictly newer `updatedAt`, or on an issue the index doesn't carry.
    fn cached_issue_if_fresh(&self, number: i64) -> Result<Option<IssueFile>> {
        let Some((cached_updated, tf)) = issue_cache_get(&self.repo, number) else {
            return Ok(None);
        };
        let Some(idx) = crate::board_index::read_board_index(&self.project) else {
            return Ok(None);
        };
        let Some(entry) = idx.board.iter().find(|f| f.task.id == tf.task.id) else {
            return Ok(None);
        };
        if entry.task.updated_at > cached_updated {
            return Ok(None);
        }
        // Not newer than what we cached — serve it, re-folding the owner overlay
        // (which can change without bumping GitHub's `updatedAt`).
        let mut tf = tf;
        tf.task.assigned_to = crate::get_task_assignment(&self.project, &tf.task.id)?;
        Ok(Some(tf))
    }

    /// Fetch several issues by number in one aliased GraphQL request
    /// (`i0: issue(number: …) { … } i1: …`), one point each and a single round
    /// trip. Chunked so a very large batch stays within a sane query size.
    /// Returns each `(issue, number)` that exists; missing numbers are dropped.
    fn fetch_many_numbers(&self, numbers: &[i64]) -> Result<Vec<(IssueFile, i64)>> {
        const CHUNK: usize = 50;
        let mut out = Vec::with_capacity(numbers.len());
        for chunk in numbers.chunks(CHUNK) {
            for (node, number) in self.graphql_issues_by_number(chunk)? {
                let mut tf = node.into_gh_issue().into_issue_file();
                tf.task.assigned_to = crate::get_task_assignment(&self.project, &tf.task.id)?;
                out.push((tf, number));
            }
        }
        Ok(out)
    }

    /// Resolve a shelbi id to a GitHub `number`: the process-local cache, then
    /// the published index's id→number map, then a label search (the fallback for
    /// an id the open index doesn't carry). A number learned from the index or
    /// search is remembered so the next resolution is free.
    fn resolve_number(&self, id: &str) -> Result<Option<i64>> {
        if let Some(n) = self.cached_number(id) {
            return Ok(Some(n));
        }
        if let Some(n) = self.index_number(id) {
            self.remember_number(id, n);
            return Ok(Some(n));
        }
        let n = self.search_number(id)?;
        if let Some(n) = n {
            self.remember_number(id, n);
        }
        Ok(n)
    }

    /// The number for `id` from the process-local id→number cache, if known.
    fn cached_number(&self, id: &str) -> Option<i64> {
        id_number_cache()
            .lock()
            .ok()?
            .get(&(self.repo.clone(), id.to_string()))
            .copied()
    }

    /// The number for `id` from the published board index's id→number map, if the
    /// open index carries it.
    fn index_number(&self, id: &str) -> Option<i64> {
        crate::board_index::read_board_index(&self.project)?
            .numbers
            .get(id)
            .copied()
    }

    /// Remember `id`→`number` in the process-local cache. A poisoned lock is a
    /// silent miss (the next resolution just re-reads the index or searches).
    fn remember_number(&self, id: &str, number: i64) {
        if let Ok(mut guard) = id_number_cache().lock() {
            guard.insert((self.repo.clone(), id.to_string()), number);
        }
    }

    /// Drop `id`'s cached number after a fetch proved the mapping stale, so the
    /// search fallback re-resolves it authoritatively.
    fn forget_number(&self, id: &str) {
        if let Ok(mut guard) = id_number_cache().lock() {
            guard.remove(&(self.repo.clone(), id.to_string()));
        }
    }

    /// Cache the full issue for `number`, keyed by its `updatedAt`.
    fn cache_issue(&self, number: i64, tf: &IssueFile) {
        if let Ok(mut guard) = issue_cache().lock() {
            guard.insert(
                (self.repo.clone(), number),
                (tf.task.updated_at, tf.clone()),
            );
        }
    }

    /// Drop any cached full issue for `number` (a fetch found it gone).
    fn forget_issue(&self, number: i64) {
        if let Ok(mut guard) = issue_cache().lock() {
            guard.remove(&(self.repo.clone(), number));
        }
    }

    /// Write a freshly-read issue back into the published index so a move/edit we
    /// just read is visible to the sidebar on its next paint (§3 "reads also
    /// refresh that issue's entry in the index"). A terminal card is dropped from
    /// the open index; a non-terminal one is upserted with its number. A no-op
    /// when no index has been published yet.
    fn refresh_index_entry(&self, tf: &IssueFile, number: i64) {
        if is_terminal(&tf.task.column) {
            let _ = crate::board_index::remove_board_index_issue(&self.project, &tf.task.id);
        } else {
            let _ = crate::board_index::patch_board_index_issue_with_number(
                &self.project,
                tf,
                Some(number),
            );
        }
    }

    /// Fetch one issue node by number via the single-issue GraphQL query, or
    /// `None` when the number names no issue (deleted, or a pull request — GraphQL
    /// `issue(number:)` returns null for a PR).
    fn graphql_single_issue(&self, number: i64) -> Result<Option<GhIssueNode>> {
        let (owner, name) = self.owner_and_name()?;
        let query_arg = format!("query={SINGLE_ISSUE_QUERY}");
        let owner_arg = format!("owner={owner}");
        let name_arg = format!("name={name}");
        // `number` is an `Int!`, so pass it with `-F` (typed) rather than `-f`
        // (raw string), which would send `"5"` and fail the scalar coercion.
        let number_arg = format!("number={number}");
        let args: Vec<&str> = vec![
            "api", "graphql", "-f", &query_arg, "-f", &owner_arg, "-f", &name_arg, "-F",
            &number_arg,
        ];
        let out = (self.graphql)(&args)?;
        parse_single_issue_response(&out)
    }

    /// Fetch a chunk of issues in one aliased GraphQL request. Numbers are
    /// integers inlined into the query text (never user input), aliased `i0`,
    /// `i1`, … so one response carries them all. Returns each `(node, number)`
    /// that resolved to a real issue.
    fn graphql_issues_by_number(&self, numbers: &[i64]) -> Result<Vec<(GhIssueNode, i64)>> {
        if numbers.is_empty() {
            return Ok(Vec::new());
        }
        let (owner, name) = self.owner_and_name()?;
        let query = build_issues_by_number_query(numbers);
        let query_arg = format!("query={query}");
        let owner_arg = format!("owner={owner}");
        let name_arg = format!("name={name}");
        let args: Vec<&str> = vec![
            "api", "graphql", "-f", &query_arg, "-f", &owner_arg, "-f", &name_arg,
        ];
        let out = (self.graphql)(&args)?;
        parse_issues_by_number_response(&out, numbers)
    }

    /// Resolve an id to a number via GitHub's search API — the fallback for an id
    /// the open index doesn't carry (a done task, or one added since the last
    /// tick). One point: `search(query: "repo:o/r label:\"shelbi:id/<label>\"",
    /// type: ISSUE, first: 2)`, taking the first issue's number.
    fn search_number(&self, id: &str) -> Result<Option<i64>> {
        let q = format!("repo:{} label:\"{}\"", self.repo, id_label(id));
        let query_arg = format!("query={ID_SEARCH_QUERY}");
        let q_arg = format!("q={q}");
        let args: Vec<&str> = vec!["api", "graphql", "-f", &query_arg, "-f", &q_arg];
        let out = (self.graphql)(&args)?;
        parse_search_number_response(&out)
    }

    /// One page (50, newest-updated first) of the terminal `done`/`canceled`
    /// history via the `BoardClosed` query (`states: [CLOSED]`). `after` is the
    /// cursor from a prior page, or `None` for the first. Returns the page's
    /// issues, the cursor for the next page (`None` when this is the last), and
    /// the token budget — a single GraphQL request, never the six-page history
    /// sweep. Closed issues always map to a terminal column, so no PR/open filter
    /// is needed.
    fn graphql_closed_page(&self, after: Option<&str>) -> Result<GraphQlClosedPage> {
        let page = self.graphql_request(BOARD_CLOSED_QUERY, after, None)?;
        let next_cursor = if page.connection.page_info.has_next_page {
            page.connection.page_info.end_cursor
        } else {
            None
        };
        let issues: Vec<GhIssue> = page
            .connection
            .nodes
            .into_iter()
            .map(GhIssueNode::into_gh_issue)
            .collect();
        Ok(GraphQlClosedPage {
            issues,
            next_cursor,
            remaining: page.rate_limit.as_ref().and_then(|r| r.remaining),
            reset: page
                .rate_limit
                .and_then(|r| r.reset_at)
                .map(|dt| dt.timestamp()),
        })
    }

    /// Live-read the comments on a GitHub issue by its number, oldest first.
    fn comments_for_number(&self, number: i64) -> Result<Vec<IssueComment>> {
        self.comments_for_number_since(number, None)
    }

    /// Like [`GitHubStore::comments_for_number`] but, when `since` is set, scopes
    /// the fetch to comments GitHub touched at/after that watermark (`since=`).
    /// The change-detection path passes the poll watermark so a quiescent issue
    /// transfers no comment bodies; `list_comments` passes `None` for the full
    /// history. `since=` filters on the comment's `updated_at`, so an edited
    /// pre-watermark comment can still come back — the caller keeps only those
    /// whose `created_at` is genuinely past the cursor.
    fn comments_for_number_since(
        &self,
        number: i64,
        since: Option<DateTime<Utc>>,
    ) -> Result<Vec<IssueComment>> {
        let path = format!("repos/{}/issues/{number}/comments", self.repo);
        let since_param = since.map(|w| format!("since={}", w.to_rfc3339()));
        let mut args: Vec<&str> = vec!["api", "-X", "GET", &path, "--paginate"];
        args.extend_from_slice(&["-f", "per_page=100"]);
        if let Some(param) = since_param.as_deref() {
            args.push("-f");
            args.push(param);
        }
        args.extend_from_slice(&["--jq", ".[]"]);
        let out = (self.gh)(&args)?;
        let raw: Vec<GhComment> = parse_jsonl(&out)?;
        let mut comments: Vec<IssueComment> = raw.into_iter().map(GhComment::into_comment).collect();
        // The API returns comments in creation order already; sort defensively
        // on the id so the ordering contract holds regardless.
        comments.sort_by_key(|c| c.created_at);
        Ok(comments)
    }

    // --- write helpers -------------------------------------------------------

    /// Run a mutating `gh api` call (`POST` / `PATCH` / `PUT`) with a set of
    /// `-f key=value` form fields, returning the raw response body. Repeated
    /// keys (e.g. `labels[]`) build an array, which is how GitHub expects label
    /// lists. Every value is passed as a distinct argv entry, so newlines and
    /// shell metacharacters in a title / body / comment are never interpreted.
    fn api_send(&self, method: &str, path: &str, fields: &[(&str, String)]) -> Result<String> {
        // Write reserve (plan §6): the single choke point for every REST mutation,
        // so a low REST budget refuses the write here — up front, naming the reset
        // time — instead of letting it fail on a 403 deep inside a transition. Only
        // an actual write is gated (this method is writes-only); the preliminary
        // reads a mutator makes ride the separate GraphQL budget.
        // Resolve the token once (gated inert under test) and reuse the key for
        // both the reserve check and the REST-budget recording, so a write shells
        // to the keychain at most once here.
        let key = self.budget_token_key();
        self.check_write_reserve(key.as_deref())?;
        // `--include` prepends the response's status line + headers so the REST
        // rate-limit budget (`x-ratelimit-*`) can be recorded; the body is split
        // back off before the caller parses it. See [`record_and_strip_rest`].
        let mut args: Vec<String> =
            vec!["api".into(), "--include".into(), "-X".into(), method.into(), path.into()];
        for (k, v) in fields {
            args.push("-f".into());
            args.push(format!("{k}={v}"));
        }
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = (self.gh)(&refs)?;
        Ok(record_and_strip_rest(key.as_deref(), &out))
    }

    /// The token-file key for this store's project, resolved for the budget
    /// governor — or `None` when governance is inert. Inert under test by default
    /// (like the read-path park): keying the budget file would shell to the
    /// keychain on every write in every write-path test and read whichever
    /// `SHELBI_HOME` is mounted. A shipped build always resolves; a test opts in
    /// via [`set_test_park_side_effects`]. Also `None` when no token resolves
    /// (then the reserve fails open and no REST budget is recorded).
    fn budget_token_key(&self) -> Option<String> {
        if !read_park_side_effects_enabled() {
            return None;
        }
        resolve_github_token_by_name(&self.project)
            .ok()
            .map(|t| crate::gh_budget::token_key(t.expose()))
    }

    /// Refuse a mutation up front when the token's REST budget is below the
    /// configured reserve floor (plan §6). Returns the reset time in the error so
    /// the caller sees *when* it can write again, rather than a bare 403. Fails
    /// open: a `None` key (governance inert, or no token) or an unrecorded REST
    /// `remaining` lets the write proceed — the reserve narrows the failure
    /// window, it is not a hard gate.
    fn check_write_reserve(&self, key: Option<&str>) -> Result<()> {
        let Some(key) = key else {
            return Ok(());
        };
        let rest = crate::gh_budget::read_state(key)
            .tier(crate::gh_budget::Budget::Rest)
            .clone();
        let Some(remaining) = rest.remaining else {
            return Ok(());
        };
        let reserve = self.budget_config().rest_reserve() as i64;
        if remaining >= reserve {
            return Ok(());
        }
        let when = rest
            .reset_at
            .and_then(|r| DateTime::from_timestamp(r, 0))
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_else(|| "an unknown time".to_string());
        Err(Error::Other(format!(
            "shelbi: refusing to write to {}: only {remaining} GitHub REST requests remain \
             (reserve floor {reserve}); the limit resets at {when}. Not sending the request.",
            self.repo
        )))
    }

    /// The project's `issue_tracker.budget` thresholds, defaulting when the
    /// project can't be loaded (a test store, or a transient config read) — a
    /// missing config just means the shipped defaults.
    fn budget_config(&self) -> shelbi_core::BudgetConfig {
        crate::load_project(&self.project)
            .map(|p| p.issue_tracker.budget)
            .unwrap_or_default()
    }


    /// Replace an issue's entire label set (`PUT .../labels`). The caller
    /// computes the full desired set — keeping the id anchor and any human
    /// labels — so this is the atomic "these are the labels now" primitive the
    /// status swap builds on.
    fn set_labels(&self, number: i64, labels: &[String]) -> Result<()> {
        let fields: Vec<(&str, String)> =
            labels.iter().map(|l| ("labels[]", l.clone())).collect();
        self.api_send(
            "PUT",
            &format!("repos/{}/issues/{number}/labels", self.repo),
            &fields,
        )?;
        Ok(())
    }

    /// Open or close an issue, optionally recording a close `state_reason`
    /// (`completed` for `done`, `not_planned` for `canceled`).
    fn set_state(&self, number: i64, state: &str, reason: Option<&str>) -> Result<()> {
        let mut fields = vec![("state", state.to_string())];
        if let Some(r) = reason {
            fields.push(("state_reason", r.to_string()));
        }
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{number}", self.repo),
            &fields,
        )?;
        Ok(())
    }

    /// Rewrite a single issue's stored priority integer inside its fenced
    /// metadata block, leaving the human prose (and every other metadata field)
    /// untouched. Reads the live body so a concurrent human prose edit is
    /// preserved.
    fn rewrite_priority(&self, id: &str, priority: u32) -> Result<()> {
        let Some(gh) = self.get_raw(id)? else {
            return Err(Error::Other(format!("issue `{id}` not found in {}", self.repo)));
        };
        let body_raw = gh.body.clone().unwrap_or_default();
        let (prose, mut meta) = split_shelbi_meta(&body_raw);
        meta.priority = Some(priority);
        let body = build_body(&prose, &meta);
        self.api_send(
            "PATCH",
            &format!("repos/{}/issues/{}", self.repo, gh.number),
            &[("body", body)],
        )?;
        Ok(())
    }

    /// The full label set to ensure on first use: the caller's specific labels
    /// (id + the status being applied) plus the stock `shelbi:status/*` set, so
    /// a fresh repo lands every status label the board can move a card into.
    fn bootstrap_labels(&self, specific: &[String]) -> Vec<String> {
        let mut out: Vec<String> = specific.to_vec();
        for col in Column::core() {
            out.push(status_label_name(&col));
        }
        out
    }

    /// Ensure every named label exists in the repo, creating the missing ones.
    /// Idempotent: existing labels are left as-is (one list call snapshots the
    /// repo, and [`GitHubStore::create_label`] additionally swallows GitHub's
    /// "already_exists" so a label that raced into existence is not an error).
    fn ensure_labels(&self, names: &[String]) -> Result<()> {
        let existing = self.list_label_names()?;
        let mut seen: std::collections::HashSet<&str> =
            existing.iter().map(String::as_str).collect();
        for name in names {
            if seen.insert(name.as_str()) {
                self.create_label(name)?;
            }
        }
        Ok(())
    }

    /// Every label name defined in the repo.
    fn list_label_names(&self) -> Result<Vec<String>> {
        let path = format!("repos/{}/labels", self.repo);
        let args = ["api", "-X", "GET", &path, "--paginate", "--jq", ".[]"];
        let out = (self.gh)(&args)?;
        let labels: Vec<GhLabel> = parse_jsonl(&out)?;
        Ok(labels.into_iter().map(|l| l.name).collect())
    }

    /// Create one repo label, tolerating a concurrent creation: GitHub answers a
    /// duplicate `POST /labels` with `422 already_exists`, which we treat as
    /// success so label bootstrap stays idempotent even against a stale list.
    fn create_label(&self, name: &str) -> Result<()> {
        let fields = vec![
            ("name", name.to_string()),
            ("color", label_color(name).to_string()),
            ("description", "Managed by shelbi".to_string()),
        ];
        match self.api_send("POST", &format!("repos/{}/labels", self.repo), &fields) {
            Ok(_) => Ok(()),
            Err(Error::Command { stderr, .. }) if stderr.contains("already_exists") => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// Whether a `gh api` invocation mutates state, inferred from its `-X <method>`
/// argument: `POST` / `PATCH` / `PUT` / `DELETE` are writes, everything else
/// (`GET`, or an absent `-X`) is a read. Drives which retry policy wraps the
/// call in [`GitHubStore::new`].
fn is_mutating_gh(args: &[&str]) -> bool {
    args.iter()
        .position(|a| *a == "-X")
        .and_then(|i| args.get(i + 1))
        .map(|method| {
            matches!(
                method.to_ascii_uppercase().as_str(),
                "POST" | "PATCH" | "PUT" | "DELETE"
            )
        })
        .unwrap_or(false)
}

/// Run the real `gh` CLI with resolved auth. The token is resolved through the
/// full chain (env → `gh` keychain → out-of-repo `tokens.yml`) and handed to
/// the child as `GH_TOKEN`; a failure to resolve surfaces the actionable
/// [`Error::MissingIssueTrackerAuth`]. A non-zero exit (network down, repo not
/// found, not authed) becomes an [`Error::Command`] — never a stale render.
fn run_gh(project: &str, args: &[&str]) -> Result<String> {
    let token = resolve_github_token_by_name(project)?;
    run_gh_with_token(&token, args)
}

/// Run `gh` with an already-resolved token. Split from [`run_gh`] so the
/// read-path park governor can resolve the token once (to key its per-token
/// budget file) and reuse it for the call, and so the free `/rate_limit` probe
/// can shell out without going back through the park check that would block it.
fn run_gh_with_token(token: &SecretToken, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("gh")
        .args(args)
        .env("GH_TOKEN", token.expose())
        .output()
        .map_err(|e| Error::Command {
            cmd: format!("gh {}", args.join(" ")),
            status: "failed to spawn".to_string(),
            stderr: e.to_string(),
        })?;
    if !output.status.success() {
        // `gh api` prints the API's JSON error body — the field-level reason,
        // e.g. `name is too long (maximum is 50 characters)` — to *stdout* and
        // only a terse one-liner to stderr. Capturing stderr alone drops the
        // actionable half, so combine both: the response body is what makes an
        // `external command failed` self-diagnosing.
        let detail = combine_gh_error_detail(
            &String::from_utf8_lossy(&output.stderr),
            &String::from_utf8_lossy(&output.stdout),
        );
        return Err(Error::Command {
            cmd: format!("gh {}", args.join(" ")),
            status: output.status.to_string(),
            stderr: detail,
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Run a read (`GET`) call under the per-token rate-limit park (plan Phase 0,
/// item 3):
///
/// 1. Resolve the token once and key the shared per-token budget file off a hash
///    of it.
/// 2. If the token is already parked (a prior 403 set `parked_until` past now),
///    short-circuit with a typed rate-limit error **without spawning `gh`** —
///    this is what stops the thousand-per-hour 403 storm.
/// 3. Otherwise run through the fail-fast read retry policy. On a rate-limit
///    failure, resolve the reset time (from the error, else a free `/rate_limit`
///    probe, else a short fallback) and park the token. The park write reports
///    whether *this* call was the one that transitioned to parked, so the
///    `board rate-limited` events.log line is written exactly once per window.
fn park_aware_read(
    project: &str,
    read_policy: &crate::gh_retry::RetryPolicy,
    args: &[&str],
) -> Result<String> {
    let token = resolve_github_token_by_name(project)?;
    let key = crate::gh_budget::token_key(token.expose());
    let now = Utc::now().timestamp();
    if let Some(reset) = crate::gh_budget::parked_until(&key, crate::gh_budget::Budget::Rest, now) {
        return Err(rate_limited_park_error(project, args, reset));
    }
    let result = read_policy.run(|| run_gh_with_token(&token, args));
    if let Err(ref e) = result {
        // `read_park_side_effects_enabled` is always true in a shipped build; in
        // a test build it gates the whole block — the `/rate_limit` reset probe
        // (itself a live `gh` call) and the home-keyed park writes — off the
        // opt-in, so a raced background refresh never touches shared state.
        if crate::gh_retry::is_rate_limit_error(e) && read_park_side_effects_enabled() {
            let reset = crate::gh_retry::rate_limit_reset_epoch(e, now)
                .or_else(|| probe_core_reset_and_record(&token, &key))
                .unwrap_or(now + crate::gh_budget::DEFAULT_PARK_SECS);
            record_read_park(project, &key, reset, now);
        }
    }
    result
}

/// Park the token keyed by `key` until `reset` and, iff this call is the one
/// that transitioned it to parked, append the single `board rate-limited`
/// events.log line for the window. Split out of [`park_aware_read`] so the park
/// bookkeeping is unit-testable without a live `gh` call, and so the test-build
/// opt-in gate has one place to wrap.
fn record_read_park(project: &str, key: &str, reset: i64, now: i64) {
    if crate::gh_budget::park(key, crate::gh_budget::Budget::Rest, reset, now) {
        let until = DateTime::from_timestamp(reset, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        if let Err(ev) = crate::append_board_rate_limited_event(project, &until) {
            tracing::warn!(project = %project, error = %ev, "append_board_rate_limited_event failed");
        }
        tracing::warn!(
            project = %project,
            until = %until,
            "GitHub read rate-limited; parking board reads for this token until reset",
        );
    }
}

/// Run a GraphQL read (the board index and single-issue fetches) under the
/// per-token **GraphQL** budget park — the governor's read side (plan Phase 3
/// §6). Mirrors [`park_aware_read`] but on the `graphql` tier, which GitHub
/// prices on a budget separate from REST:
///
/// 1. Resolve the token once and key the shared per-token budget file off it.
/// 2. If the GraphQL budget is already parked (a prior 403/429 set its
///    `parked_until`), short-circuit with the typed rate-limit error **without
///    spawning `gh`** — so a genuinely exhausted GraphQL budget stops re-issuing
///    403s, and the daemon's governor holds the tick until the reset.
/// 3. Otherwise run through the fail-fast read retry policy. On success, record
///    the `rateLimit { remaining resetAt }` the response carried into the
///    `graphql` tier (the number the governor scales the tick from). On a
///    rate-limit failure, park the GraphQL budget until its reset — once, with a
///    single `board rate-limited` events.log line.
fn graphql_governed_read(
    project: &str,
    read_policy: &crate::gh_retry::RetryPolicy,
    args: &[&str],
) -> Result<String> {
    let token = resolve_github_token_by_name(project)?;
    let key = crate::gh_budget::token_key(token.expose());
    let now = Utc::now().timestamp();
    if let Some(reset) =
        crate::gh_budget::parked_until(&key, crate::gh_budget::Budget::Graphql, now)
    {
        return Err(rate_limited_park_error(project, args, reset));
    }
    let result = read_policy.run(|| run_gh_with_token(&token, args));
    match &result {
        Ok(body) => {
            // Every board / single-issue / search / batch response carries
            // `data.rateLimit`; fold it into the governor's `graphql` tier.
            if let Some((remaining, reset)) = extract_graphql_rate_limit(body) {
                crate::gh_budget::record(&key, crate::gh_budget::Budget::Graphql, remaining, reset);
            }
        }
        // See `read_park_side_effects_enabled`: always true in a shipped build; in
        // a test build the park write is gated on the opt-in so a raced refresh
        // never touches a sibling test's home.
        Err(e) if crate::gh_retry::is_rate_limit_error(e) && read_park_side_effects_enabled() => {
            let reset = crate::gh_retry::rate_limit_reset_epoch(e, now)
                .unwrap_or(now + crate::gh_budget::DEFAULT_PARK_SECS);
            record_graphql_park(project, &key, reset, now);
        }
        Err(_) => {}
    }
    result
}

/// Park the token's **GraphQL** budget until `reset` and, iff this call is the
/// one that transitioned it to parked, append the single `board rate-limited`
/// events.log line for the window. The GraphQL sibling of [`record_read_park`];
/// split out so the park bookkeeping is unit-testable and the test-build opt-in
/// gate has one place to wrap.
fn record_graphql_park(project: &str, key: &str, reset: i64, now: i64) {
    if crate::gh_budget::park(key, crate::gh_budget::Budget::Graphql, reset, now) {
        let until = DateTime::from_timestamp(reset, 0)
            .map(|dt| dt.to_rfc3339())
            .unwrap_or_default();
        if let Err(ev) = crate::append_board_rate_limited_event(project, &until) {
            tracing::warn!(project = %project, error = %ev, "append_board_rate_limited_event failed");
        }
        tracing::warn!(
            project = %project,
            until = %until,
            "GitHub GraphQL rate-limited; parking board reads for this token until reset",
        );
    }
}

/// Extract `data.rateLimit { remaining resetAt }` from any GraphQL response —
/// board page, single issue, search, or aliased batch, all of which request it.
/// Returns `(remaining, reset_epoch)`; best-effort `None` when the body isn't the
/// expected envelope (a `gh` error string, an `errors`-only response), which just
/// means this response updates no budget.
fn extract_graphql_rate_limit(body: &str) -> Option<(Option<i64>, Option<i64>)> {
    #[derive(Deserialize)]
    struct Env {
        data: Option<Data>,
    }
    #[derive(Deserialize)]
    struct Data {
        #[serde(rename = "rateLimit")]
        rate_limit: Option<GhRateLimit>,
    }
    let env: Env = serde_json::from_str(body.trim()).ok()?;
    let rl = env.data?.rate_limit?;
    Some((
        rl.remaining.map(|r| r as i64),
        rl.reset_at.map(|dt| dt.timestamp()),
    ))
}

/// The typed error a parked read returns instead of calling `gh`. Its `stderr`
/// carries the "rate limit" phrasing and the `x-ratelimit-reset` epoch so
/// [`crate::gh_retry::is_rate_limit_error`] classifies it as rate-limited and
/// [`crate::gh_retry::rate_limit_reset_epoch`] can recover the reset — keeping a
/// short-circuited read indistinguishable, to every downstream classifier, from
/// the live 403 it stands in for.
fn rate_limited_park_error(project: &str, args: &[&str], reset: i64) -> Error {
    Error::Command {
        cmd: format!("gh {}", args.join(" ")),
        status: "parked (rate limit)".to_string(),
        stderr: format!(
            "shelbi: board reads for project `{project}` are parked until the GitHub \
             API rate limit resets (x-ratelimit-reset: {reset}); not calling gh"
        ),
    }
}

/// Fetch the authoritative core-REST reset epoch via the free `/rate_limit`
/// endpoint (it never consumes quota and answers `200` even when the core budget
/// is exhausted), and opportunistically record the response's rate-limit headers
/// to the per-token budget file for the Phase 3 governor. Returns the core reset
/// from the JSON body — the value a bare, headerless 403 could not carry.
fn probe_core_reset_and_record(token: &SecretToken, key: &str) -> Option<i64> {
    let out = run_gh_with_token(token, &["api", "--include", "-X", "GET", "rate_limit"]).ok()?;
    // Best-effort budget snapshot from the header block (stops at the blank
    // line, so it never reads the body below).
    crate::gh_budget::record_rest_headers(key, &crate::gh_budget::parse_rate_limit_headers(&out));
    let body = http_response_body(&out);
    let probe: RateLimitProbe = serde_json::from_str(body).ok()?;
    probe.resources.core.reset
}

/// Split the body off a `gh api --include` write response, recording the REST
/// rate-limit headers into the token's `rest` budget on the way (plan §6 — the
/// REST reserve reads what this records). A real `--include` response begins with
/// the HTTP status line (`HTTP/…`); a canned test body (or an unexpected shape)
/// does not, and is returned verbatim with no recording — so injected test
/// runners that answer with bare JSON are untouched. A `None` key (governance
/// inert) strips the body but records nothing.
fn record_and_strip_rest(key: Option<&str>, raw: &str) -> String {
    if !raw.starts_with("HTTP/") {
        return raw.to_string();
    }
    if let Some(key) = key {
        crate::gh_budget::record_rest_headers(
            key,
            &crate::gh_budget::parse_rate_limit_headers(raw),
        );
    }
    http_response_body(raw).to_string()
}

/// The body of a `gh api --include` response: everything after the first blank
/// line that separates the header block from the payload. Handles both CRLF and
/// LF separators; falls back to the whole string when no blank line is found
/// (e.g. `--include` was not honored), so a plain JSON body still parses.
fn http_response_body(raw: &str) -> &str {
    if let Some(idx) = raw.find("\r\n\r\n") {
        &raw[idx + 4..]
    } else if let Some(idx) = raw.find("\n\n") {
        &raw[idx + 2..]
    } else {
        raw
    }
}

/// Minimal shape of `GET /rate_limit` — only the core resource's reset, which is
/// all the park needs; other fields are ignored.
#[derive(Deserialize)]
struct RateLimitProbe {
    resources: RateLimitProbeResources,
}

#[derive(Deserialize)]
struct RateLimitProbeResources {
    core: RateLimitProbeResource,
}

#[derive(Deserialize)]
struct RateLimitProbeResource {
    reset: Option<i64>,
}

/// Merge a failed `gh` invocation's `stderr` and `stdout` into one diagnostic
/// string for [`Error::Command`]. `gh api` writes the terse status line to
/// stderr and the rich JSON error body (the field-level reason) to stdout, so
/// both halves matter; either may be empty. When both carry text they are
/// joined with a newline, stderr first (the summary), stdout second (the body).
fn combine_gh_error_detail(stderr: &str, stdout: &str) -> String {
    match (stderr.trim().is_empty(), stdout.trim().is_empty()) {
        (false, false) => format!("{}\n{}", stderr.trim_end(), stdout.trim_end()),
        (true, false) => stdout.trim_end().to_string(),
        _ => stderr.to_string(),
    }
}

/// Parse a `--jq '.[]'` stream (one JSON object per line) into a vec, skipping
/// blank lines. A malformed line is a hard error — a partial read must never be
/// silently rendered as a truncated board.
fn parse_jsonl<T: for<'de> Deserialize<'de>>(text: &str) -> Result<Vec<T>> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        out.push(
            serde_json::from_str(line)
                .map_err(|e| Error::Other(format!("gh returned unparseable JSON: {e}")))?,
        );
    }
    Ok(out)
}

/// Canonical board order: column order first, then priority, then id — the same
/// ordering [`crate::list_tasks`] gives the filesystem board.
fn sort_board(issues: &mut [IssueFile]) {
    issues.sort_by(|a, b| {
        a.task
            .column
            .board_order()
            .cmp(&b.task.column.board_order())
            .then(a.task.priority.cmp(&b.task.priority))
            .then(a.task.id.cmp(&b.task.id))
    });
}

/// Fold the current local assignment overlay onto one issue — the workspace
/// that owns it, or `None`. Shared by the cold and incremental board reads so
/// ownership is refreshed every tick regardless of GitHub `updatedAt`.
fn fold_assignment(
    mut tf: IssueFile,
    assignments: &std::collections::BTreeMap<String, String>,
) -> IssueFile {
    tf.task.assigned_to = assignments.get(&tf.task.id).cloned();
    tf
}

// --- GitHub GraphQL board index ----------------------------------------------
//
// The board index reads through GraphQL rather than REST (plan §2): one query
// returns exactly the fields the board needs, `body` rides along for free
// (GraphQL prices connections, not fields), and `repository.issues` excludes
// pull requests so the `is_pull_request` filter isn't needed on this path. The
// nodes map onto the same [`GhIssue`] shape the REST path parses, so the whole
// label → id/status/column + fenced-metadata mapping is reused unchanged.

/// Repos currently served from the REST fallback because their last GraphQL
/// board read failed with a non-rate-limit error. Process-global (the daemon is
/// one process) and keyed by `owner/repo`, so the "which path is active" log
/// fires once per repo on each transition — a warning on the first fallback, an
/// info when GraphQL recovers — never on every 30-second tick.
static GRAPHQL_FALLBACK_REPOS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashSet<String>>,
> = std::sync::OnceLock::new();

/// The (lazily initialized) set of repos in GraphQL fallback.
fn graphql_fallback_repos() -> &'static std::sync::Mutex<std::collections::HashSet<String>> {
    GRAPHQL_FALLBACK_REPOS.get_or_init(Default::default)
}

/// Cold fetch of every open issue, newest-updated first, one page of 100. `body`
/// and `labels(first: 10)` ride along; `rateLimit` reports the points budget.
const BOARD_INDEX_QUERY: &str = r#"
query BoardIndex($owner: String!, $name: String!, $after: String) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    issues(states: [OPEN], first: 100, after: $after,
           orderBy: { field: UPDATED_AT, direction: DESC }) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state stateReason createdAt updatedAt body
        labels(first: 10) { nodes { name } }
      }
    }
  }
}
"#;

/// Incremental fetch: issues touched at/after `$since`, with **no** `states`
/// filter so a just-closed issue comes back and is dropped from the open index.
/// A quiet board returns zero nodes (one point). Identical node shape to
/// [`BOARD_INDEX_QUERY`] so both share [`GhIssueNode`].
const BOARD_INDEX_DELTA_QUERY: &str = r#"
query BoardIndexDelta($owner: String!, $name: String!, $after: String, $since: DateTime!) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    issues(first: 100, after: $after,
           filterBy: { since: $since },
           orderBy: { field: UPDATED_AT, direction: DESC }) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state stateReason createdAt updatedAt body
        labels(first: 10) { nodes { name } }
      }
    }
  }
}
"#;

/// The done/canceled history on demand (plan §4): one page of 50 closed issues,
/// newest-updated first. Same node shape as [`BOARD_INDEX_QUERY`] (so both share
/// [`GhIssueNode`]); `states: [CLOSED]` means every node maps to a terminal
/// column, and `pageInfo` carries the cursor a "load more" pages with.
const BOARD_CLOSED_QUERY: &str = r#"
query BoardClosed($owner: String!, $name: String!, $after: String) {
  rateLimit { cost remaining resetAt }
  repository(owner: $owner, name: $name) {
    issues(states: [CLOSED], first: 50, after: $after,
           orderBy: { field: UPDATED_AT, direction: DESC }) {
      pageInfo { hasNextPage endCursor }
      nodes {
        number title state stateReason createdAt updatedAt body
        labels(first: 10) { nodes { name } }
      }
    }
  }
}
"#;

/// The collected result of a (possibly paginated) GraphQL board read: every
/// issue node mapped onto [`GhIssue`], plus the token budget the last page
/// reported.
struct GraphQlBoardPage {
    issues: Vec<GhIssue>,
    remaining: Option<u64>,
    reset: Option<i64>,
}

// --- single-issue + aliased multi-fetch + id search (plan §3) -----------------

/// The fresh single-issue read: one issue by number, the same node shape the
/// board index parses so [`GhIssueNode`] serves both. One point.
const SINGLE_ISSUE_QUERY: &str = r#"
query Issue($owner: String!, $name: String!, $number: Int!) {
  rateLimit { remaining resetAt }
  repository(owner: $owner, name: $name) {
    issue(number: $number) {
      number title state stateReason createdAt updatedAt body
      labels(first: 10) { nodes { name } }
    }
  }
}
"#;

/// Resolve an id to a number for an issue the open index doesn't carry (a done
/// task, or one added since the last tick): search by the `shelbi:id/*` label.
/// One point.
const ID_SEARCH_QUERY: &str = r#"
query IdSearch($q: String!) {
  rateLimit { remaining resetAt }
  search(query: $q, type: ISSUE, first: 2) {
    nodes { ... on Issue { number } }
  }
}
"#;

/// Build the aliased multi-issue query for `numbers`: each is inlined as its own
/// `i<k>: issue(number: <n>) { … }` field (numbers are integers, never user
/// input), so one request and one round trip return them all. The node shape
/// matches [`GhIssueNode`] so the same mapping serves single, aliased and board
/// reads.
fn build_issues_by_number_query(numbers: &[i64]) -> String {
    let mut aliases = String::new();
    for (k, n) in numbers.iter().enumerate() {
        aliases.push_str(&format!(
            "    i{k}: issue(number: {n}) {{ number title state stateReason createdAt \
             updatedAt body labels(first: 10) {{ nodes {{ name }} }} }}\n"
        ));
    }
    format!(
        "query IssuesByNumber($owner: String!, $name: String!) {{\n  \
         rateLimit {{ remaining resetAt }}\n  \
         repository(owner: $owner, name: $name) {{\n{aliases}  }}\n}}\n"
    )
}

/// `{ "data": { "rateLimit": …, "repository": { "issue": <node>|null } } }`.
#[derive(Debug, Deserialize)]
struct SingleIssueResponse {
    #[serde(default)]
    data: Option<SingleIssueData>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct SingleIssueData {
    repository: Option<SingleIssueRepo>,
}

#[derive(Debug, Deserialize)]
struct SingleIssueRepo {
    #[serde(default)]
    issue: Option<GhIssueNode>,
}

/// Parse a single-issue GraphQL response into its node, or `None` when the
/// number names no issue (deleted, or a pull request → `issue` is null). A
/// GraphQL `errors` array or a missing `repository` is a hard error, exactly as
/// on the board read.
fn parse_single_issue_response(text: &str) -> Result<Option<GhIssueNode>> {
    let resp: SingleIssueResponse = serde_json::from_str(text.trim())
        .map_err(|e| Error::Other(format!("gh graphql returned unparseable JSON: {e}")))?;
    if let Some(errors) = resp.errors.as_ref().filter(|e| !e.is_empty()) {
        return Err(Error::Other(format!(
            "GitHub GraphQL returned errors on the single-issue read: {}",
            summarize_graphql_errors(errors)
        )));
    }
    let data = resp
        .data
        .ok_or_else(|| Error::Other("gh graphql single-issue response carried no data".into()))?;
    let repository = data.repository.ok_or_else(|| {
        Error::Other(
            "gh graphql single-issue response has no repository (wrong name, or the token \
             can't see it)"
                .into(),
        )
    })?;
    Ok(repository.issue)
}

/// `{ "data": { "rateLimit": …, "repository": { "i0": <node>|null, … } } }`.
#[derive(Debug, Deserialize)]
struct AliasResponse {
    #[serde(default)]
    data: Option<AliasData>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct AliasData {
    repository: Option<serde_json::Map<String, serde_json::Value>>,
}

/// Parse an aliased multi-issue response, pairing each present `i<k>` node with
/// `numbers[k]` (the order the aliases were built in). Null aliases (a number
/// that named no issue) are skipped.
fn parse_issues_by_number_response(
    text: &str,
    numbers: &[i64],
) -> Result<Vec<(GhIssueNode, i64)>> {
    let resp: AliasResponse = serde_json::from_str(text.trim())
        .map_err(|e| Error::Other(format!("gh graphql returned unparseable JSON: {e}")))?;
    if let Some(errors) = resp.errors.as_ref().filter(|e| !e.is_empty()) {
        return Err(Error::Other(format!(
            "GitHub GraphQL returned errors on the multi-issue read: {}",
            summarize_graphql_errors(errors)
        )));
    }
    let data = resp
        .data
        .ok_or_else(|| Error::Other("gh graphql multi-issue response carried no data".into()))?;
    let repository = data.repository.ok_or_else(|| {
        Error::Other("gh graphql multi-issue response has no repository".into())
    })?;
    let mut out = Vec::new();
    for (k, number) in numbers.iter().enumerate() {
        let Some(value) = repository.get(&format!("i{k}")) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let node: GhIssueNode = serde_json::from_value(value.clone())
            .map_err(|e| Error::Other(format!("gh graphql returned unparseable issue node: {e}")))?;
        out.push((node, *number));
    }
    Ok(out)
}

/// `{ "data": { "rateLimit": …, "search": { "nodes": [ { "number": n }, … ] } } }`.
#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default)]
    data: Option<SearchData>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct SearchData {
    search: Option<SearchConnection>,
}

#[derive(Debug, Deserialize)]
struct SearchConnection {
    #[serde(default)]
    nodes: Vec<SearchNode>,
}

#[derive(Debug, Deserialize)]
struct SearchNode {
    /// Present via the `... on Issue { number }` inline fragment; a non-Issue
    /// node (a PR the query filtered out anyway) has none.
    #[serde(default)]
    number: Option<i64>,
}

/// The first issue number a label search returned, or `None` when nothing
/// matched. A GraphQL `errors` array is a hard error.
fn parse_search_number_response(text: &str) -> Result<Option<i64>> {
    let resp: SearchResponse = serde_json::from_str(text.trim())
        .map_err(|e| Error::Other(format!("gh graphql returned unparseable JSON: {e}")))?;
    if let Some(errors) = resp.errors.as_ref().filter(|e| !e.is_empty()) {
        return Err(Error::Other(format!(
            "GitHub GraphQL returned errors on the id search: {}",
            summarize_graphql_errors(errors)
        )));
    }
    Ok(resp
        .data
        .and_then(|d| d.search)
        .map(|s| s.nodes)
        .unwrap_or_default()
        .into_iter()
        .find_map(|n| n.number))
}

/// Process-local id→number resolution cache, keyed by `(repo, shelbi id)`. Small
/// and process-scoped (like the board snapshot cache); the daemon's index is the
/// cross-process authority.
type IdNumberCache = std::sync::Mutex<std::collections::HashMap<(String, String), i64>>;
static ID_NUMBER_CACHE: std::sync::OnceLock<IdNumberCache> = std::sync::OnceLock::new();
fn id_number_cache() -> &'static IdNumberCache {
    ID_NUMBER_CACHE.get_or_init(Default::default)
}

/// Per-number full-issue cache keyed by `(repo, number)` → `(updatedAt, issue)`,
/// invalidated when the published index shows a newer `updatedAt` (plan §3).
type IssueCache = std::sync::Mutex<std::collections::HashMap<(String, i64), (DateTime<Utc>, IssueFile)>>;
static ISSUE_CACHE: std::sync::OnceLock<IssueCache> = std::sync::OnceLock::new();
fn issue_cache() -> &'static IssueCache {
    ISSUE_CACHE.get_or_init(Default::default)
}

/// The cached `(updatedAt, issue)` for `(repo, number)`, if any.
fn issue_cache_get(repo: &str, number: i64) -> Option<(DateTime<Utc>, IssueFile)> {
    issue_cache()
        .lock()
        .ok()?
        .get(&(repo.to_string(), number))
        .cloned()
}

/// Clear both process-local caches — the id→number map and the per-number full
/// issue cache — so a test's cache state can't leak into another (both statics
/// are process-global). Test-only.
#[cfg(any(test, feature = "test-support"))]
pub fn clear_issue_caches_for_test() {
    if let Ok(mut g) = id_number_cache().lock() {
        g.clear();
    }
    if let Ok(mut g) = issue_cache().lock() {
        g.clear();
    }
}

/// One page of closed history from [`GitHubStore::graphql_closed_page`]: the
/// mapped issue nodes, the cursor for the next page (`None` on the last), and the
/// token budget the read reported.
struct GraphQlClosedPage {
    issues: Vec<GhIssue>,
    next_cursor: Option<String>,
    remaining: Option<u64>,
    reset: Option<i64>,
}

/// Top-level GraphQL envelope: `{ "data": {...}, "errors": [...] }`.
#[derive(Debug, Deserialize)]
struct GraphQlResponse {
    #[serde(default)]
    data: Option<GraphQlData>,
    #[serde(default)]
    errors: Option<Vec<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct GraphQlData {
    #[serde(rename = "rateLimit")]
    rate_limit: Option<GhRateLimit>,
    repository: Option<GhRepository>,
}

/// The `rateLimit` block on every board response — the free budget signal the
/// index stamps into `board-index.json` and the Phase 3 governor reads.
#[derive(Debug, Deserialize)]
struct GhRateLimit {
    #[serde(default)]
    remaining: Option<u64>,
    #[serde(rename = "resetAt", default)]
    reset_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Deserialize)]
struct GhRepository {
    issues: GhIssuesConnection,
}

#[derive(Debug, Deserialize)]
struct GhIssuesConnection {
    #[serde(rename = "pageInfo")]
    page_info: GhPageInfo,
    nodes: Vec<GhIssueNode>,
}

#[derive(Debug, Deserialize)]
struct GhPageInfo {
    #[serde(rename = "hasNextPage")]
    has_next_page: bool,
    #[serde(rename = "endCursor")]
    end_cursor: Option<String>,
}

/// One issue node from `repository.issues`. GraphQL enums arrive upper-cased
/// (`OPEN`/`CLOSED`, `COMPLETED`/`NOT_PLANNED`), so [`GhIssueNode::into_gh_issue`]
/// lower-cases them onto the REST [`GhIssue`] shape the mapping already handles.
#[derive(Debug, Deserialize)]
struct GhIssueNode {
    number: i64,
    title: String,
    state: String,
    #[serde(rename = "stateReason", default)]
    state_reason: Option<String>,
    #[serde(rename = "createdAt")]
    created_at: DateTime<Utc>,
    #[serde(rename = "updatedAt")]
    updated_at: DateTime<Utc>,
    #[serde(default)]
    body: Option<String>,
    labels: GhLabelConnection,
}

#[derive(Debug, Deserialize)]
struct GhLabelConnection {
    nodes: Vec<GhLabel>,
}

impl GhIssueNode {
    /// Map a GraphQL issue node onto the REST [`GhIssue`] the whole board
    /// mapping is written against: lower-case the `state`/`stateReason` enums to
    /// match REST, and never set `pull_request` (the issues connection excludes
    /// PRs). `createdAt` is carried so the persisted `Issue.created_at` stays
    /// accurate rather than collapsing onto `updatedAt`.
    fn into_gh_issue(self) -> GhIssue {
        GhIssue {
            number: self.number,
            title: self.title,
            body: self.body,
            state: self.state.to_ascii_lowercase(),
            state_reason: self.state_reason.map(|r| r.to_ascii_lowercase()),
            labels: self.labels.nodes,
            created_at: self.created_at,
            updated_at: self.updated_at,
            pull_request: None,
        }
    }
}

/// The one issues-connection page a single GraphQL response carries.
struct GraphQlResponsePage {
    connection: GhIssuesConnection,
    rate_limit: Option<GhRateLimit>,
}

/// Parse one `gh api graphql` board response into its issues page + budget.
///
/// A GraphQL `errors` array (returned with HTTP 200, so `gh` exits 0) fails the
/// read: a partial board must never be published as if it were complete — the
/// previous index stays in place and the next tick retries. A missing
/// `repository` (bad name, or no access) is the same hard error.
fn parse_graphql_board_response(text: &str) -> Result<GraphQlResponsePage> {
    let resp: GraphQlResponse = serde_json::from_str(text.trim())
        .map_err(|e| Error::Other(format!("gh graphql returned unparseable JSON: {e}")))?;
    if let Some(errors) = resp.errors.as_ref().filter(|e| !e.is_empty()) {
        return Err(Error::Other(format!(
            "GitHub GraphQL returned errors on the board read: {}",
            summarize_graphql_errors(errors)
        )));
    }
    let data = resp
        .data
        .ok_or_else(|| Error::Other("gh graphql board response carried no data".into()))?;
    let repository = data.repository.ok_or_else(|| {
        Error::Other(
            "gh graphql board response has no repository (wrong name, or the token \
             can't see it)"
                .into(),
        )
    })?;
    Ok(GraphQlResponsePage {
        connection: repository.issues,
        rate_limit: data.rate_limit,
    })
}

/// A short, log-safe summary of a GraphQL `errors` array — each error's
/// `message`, joined — for the [`Error::Other`] a failed board read surfaces.
fn summarize_graphql_errors(errors: &[serde_json::Value]) -> String {
    let msgs: Vec<String> = errors
        .iter()
        .map(|e| {
            e.get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("<no message>")
                .to_string()
        })
        .collect();
    msgs.join("; ")
}

// --- GitHub REST wire shapes -------------------------------------------------

/// One issue object from `GET /repos/{owner}/{repo}/issues`. Only the fields
/// the mapping needs are named; the rest are ignored.
#[derive(Debug, Deserialize)]
struct GhIssue {
    number: i64,
    title: String,
    #[serde(default)]
    body: Option<String>,
    /// `"open"` | `"closed"`.
    state: String,
    /// `"completed"` | `"not_planned"` | null — only set when closed.
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    labels: Vec<GhLabel>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    /// Present only on pull requests, which the issues endpoint also returns;
    /// its presence is how we tell a PR apart from an issue.
    #[serde(default)]
    pull_request: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct GhLabel {
    name: String,
}

impl GhIssue {
    /// True when this "issue" is really a pull request (the issues endpoint
    /// returns both; a PR carries a `pull_request` object).
    fn is_pull_request(&self) -> bool {
        self.pull_request.is_some()
    }

    /// True when GitHub reports the issue closed — the signal the incremental
    /// board merge uses to drop it from the open index (a closed issue always
    /// maps to a terminal `done`/`canceled` status).
    fn is_closed(&self) -> bool {
        self.state == "closed"
    }

    /// The stable shelbi id, in resolution order: the authoritative `id` in the
    /// parsed metadata block (set when the anchor label was truncated) wins;
    /// else the `shelbi:id/<slug>` label, prefix-stripped (the anchor for a
    /// short id); else the issue number as a string (so an un-migrated repo
    /// still renders). Takes the already-parsed `meta` so the body isn't split
    /// twice.
    fn resolve_id(&self, meta: &ShelbiMeta) -> String {
        if let Some(id) = &meta.id {
            return id.clone();
        }
        self.labels
            .iter()
            .find_map(|l| l.name.strip_prefix(ID_LABEL_PREFIX))
            .map(str::to_string)
            .unwrap_or_else(|| self.number.to_string())
    }

    /// The `shelbi:status/<id>` label value, if any.
    fn status_label(&self) -> Option<&str> {
        self.labels
            .iter()
            .find_map(|l| l.name.strip_prefix(STATUS_LABEL_PREFIX))
    }

    /// Every label name that is NOT a `shelbi:status/*` label — the set to keep
    /// when swapping the status label on a move.
    fn non_status_labels(&self) -> Vec<String> {
        self.labels
            .iter()
            .filter(|l| !l.name.starts_with(STATUS_LABEL_PREFIX))
            .map(|l| l.name.clone())
            .collect()
    }

    /// The workflow this issue runs under, parsed from its metadata block, or
    /// the canonical default when unset — the value a [`StatusMove`] carries.
    fn workflow_name(&self) -> String {
        let body = self.body.clone().unwrap_or_default();
        split_shelbi_meta(&body)
            .1
            .workflow
            .unwrap_or_else(|| DEFAULT_WORKFLOW_NAME.to_string())
    }

    /// Map GitHub state + labels onto a shelbi [`Column`]. A closed issue is
    /// always terminal; an open issue takes its status label, defaulting to
    /// `todo` when unlabeled.
    fn column(&self) -> Column {
        let closed = self.state == "closed";
        match self.status_label() {
            Some(id) => {
                let col = Column::from_status_id(id);
                if closed && !is_terminal(&col) {
                    // A closed issue overrides a stale non-terminal label so it
                    // can never render in an active lane.
                    terminal_from_reason(self.state_reason.as_deref())
                } else {
                    col
                }
            }
            None if closed => terminal_from_reason(self.state_reason.as_deref()),
            None => Column::todo(),
        }
    }

    /// Full mapping onto an [`IssueFile`]: native fields plus the parsed fenced
    /// metadata block, with that block stripped from the body prose.
    fn into_issue_file(self) -> IssueFile {
        let body_raw = self.body.clone().unwrap_or_default();
        let (prose, meta) = split_shelbi_meta(&body_raw);
        let column = self.column();
        let id = self.resolve_id(&meta);

        let task = Issue {
            id,
            title: self.title,
            column,
            priority: meta.priority.unwrap_or(0),
            // Workspace routing is ephemeral local state, never read from GitHub.
            assigned_to: None,
            workflow: meta.workflow,
            branch: meta.branch,
            depends_on: meta.depends_on,
            prefers_machine: meta.prefers_machine,
            zen: meta.zen,
            launch: meta.launch,
            created_at: self.created_at,
            updated_at: self.updated_at,
            params: meta.params,
        };
        IssueFile { task, body: prose }
    }
}

/// One comment object from `GET /repos/{owner}/{repo}/issues/{n}/comments`.
#[derive(Debug, Deserialize)]
struct GhComment {
    id: i64,
    #[serde(default)]
    body: Option<String>,
    created_at: DateTime<Utc>,
    #[serde(default)]
    user: Option<GhUser>,
}

#[derive(Debug, Deserialize)]
struct GhUser {
    login: String,
}

impl GhComment {
    fn into_comment(self) -> IssueComment {
        IssueComment {
            id: self.id.to_string(),
            author: self.user.map(|u| u.login),
            created_at: self.created_at,
            body: self.body.unwrap_or_default(),
        }
    }
}

/// True for the terminal columns (`done` / `canceled`).
fn is_terminal(col: &Column) -> bool {
    *col == Column::done() || *col == Column::canceled()
}

/// The terminal column implied by a closed issue's `state_reason`:
/// `not_planned` → `canceled`, everything else → `done`.
fn terminal_from_reason(reason: Option<&str>) -> Column {
    match reason {
        Some("not_planned") => Column::canceled(),
        _ => Column::done(),
    }
}

/// The GitHub `state_reason` to close an issue with for a terminal target:
/// `canceled` → `not_planned`, `done` → `completed`. `None` for a non-terminal
/// column (which never closes the issue). Round-trips with
/// [`terminal_from_reason`] so a closed issue reads back to the same terminal.
fn terminal_reason(col: &Column) -> Option<&'static str> {
    if *col == Column::canceled() {
        Some("not_planned")
    } else if *col == Column::done() {
        Some("completed")
    } else {
        None
    }
}

/// The full `shelbi:status/<id>` label name for a column.
fn status_label_name(col: &Column) -> String {
    format!("{STATUS_LABEL_PREFIX}{}", col.as_str())
}

/// The `shelbi:id/<…>` anchor label for a shelbi id, kept within GitHub's
/// 50-char label cap.
///
/// A short id (`prefix + id <= 50`) is used verbatim, so a repo already
/// migrated under short ids keeps byte-identical anchors — no churn. A long id
/// is truncated to a [`ID_SLUG_BUDGET`]-byte slug plus a stable 8-hex-char hash
/// (`shelbi:id/<slug>-<hash8>`). The label stays a pure function of the id
/// (recomputable for the server-side `labels=` query without fetching), and two
/// ids sharing their first 31 bytes are disambiguated by the hash. When the
/// label is truncated the authoritative id no longer lives in the label, so the
/// caller carries it in the body metadata block (`ShelbiMeta::id`) for a
/// lossless read-back — see [`id_is_truncated`].
fn id_label(id: &str) -> String {
    if ID_LABEL_PREFIX.len() + id.len() <= GITHUB_LABEL_MAX {
        return format!("{ID_LABEL_PREFIX}{id}");
    }
    let slug = truncate_on_char_boundary(id, ID_SLUG_BUDGET);
    let hash = fnv1a64(id) as u32;
    format!("{ID_LABEL_PREFIX}{slug}-{hash:08x}")
}

/// True when [`id_label`] had to truncate — i.e. the anchor label is no longer
/// the full id and the id must be carried in the body metadata block instead.
fn id_is_truncated(id: &str) -> bool {
    ID_LABEL_PREFIX.len() + id.len() > GITHUB_LABEL_MAX
}

/// The longest prefix of `s` that fits in `max_bytes` without splitting a
/// multi-byte char. Ids are ASCII slugs today, but a char-boundary walk means a
/// hand-authored non-ASCII id truncates cleanly rather than panicking.
fn truncate_on_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// FNV-1a 64-bit hash of `id`. Hand-rolled (not
/// [`std::collections::hash_map::DefaultHasher`], which is explicitly *not*
/// stable across releases) so the same id yields the same anchor label in this
/// build and every future one — the property the round-trip depends on. The
/// caller renders the low 32 bits as 8 hex chars.
fn fnv1a64(id: &str) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut hash = FNV_OFFSET_BASIS;
    for byte in id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A stable label colour (6 hex digits, no `#`) for a managed shelbi label. The
/// stock status labels get distinct hues so the GitHub label list reads as a
/// board; the id anchor and any custom status share a neutral grey. Cosmetic
/// only — nothing keys off the colour.
fn label_color(name: &str) -> &'static str {
    match name {
        "shelbi:status/backlog" => "c5def5",
        "shelbi:status/todo" => "1d76db",
        "shelbi:status/in-progress" => "fbca04",
        "shelbi:status/review" => "d93f0b",
        "shelbi:status/done" => "0e8a16",
        "shelbi:status/canceled" => "6a737d",
        _ => "ededed",
    }
}

/// Build a [`ShelbiMeta`] from a creation spec, stamping the resolved priority.
fn meta_from_new(spec: &NewIssue, priority: u32) -> ShelbiMeta {
    ShelbiMeta {
        // Stamped by the caller (`add`) only when the anchor label is truncated.
        id: None,
        workflow: spec.workflow.clone(),
        branch: spec.branch.clone(),
        depends_on: spec.depends_on.clone(),
        prefers_machine: spec.prefers_machine.clone(),
        priority: Some(priority),
        zen: spec.zen.clone(),
        launch: spec.launch.clone(),
        params: spec.params.clone(),
    }
}

/// Compose an issue body from human `prose` and shelbi `meta`, emitting the
/// fenced `<!-- shelbi:begin -->` … `<!-- shelbi:end -->` block that
/// [`split_shelbi_meta`] reads back. The inverse of that split, so a
/// read-modify-write round-trips: prose is preserved verbatim (trimmed), and an
/// all-empty `meta` emits no block at all rather than an empty fence.
fn build_body(prose: &str, meta: &ShelbiMeta) -> String {
    let prose = prose.trim();
    let yaml = serde_yaml::to_string(meta).unwrap_or_default();
    let yaml = yaml.trim();
    // serde_yaml renders a struct with every field skipped as `{}`.
    if yaml.is_empty() || yaml == "{}" {
        return if prose.is_empty() {
            String::new()
        } else {
            format!("{prose}\n")
        };
    }
    let block = format!("{META_BEGIN}\n```yaml\n{yaml}\n```\n{META_END}\n");
    if prose.is_empty() {
        block
    } else {
        format!("{prose}\n\n{block}")
    }
}

/// Parse a single JSON object (a `gh api` write response) into `T`. Unlike
/// [`parse_jsonl`], the write endpoints return one object, not a `--jq '.[]'`
/// stream.
fn parse_json_object<T: for<'de> Deserialize<'de>>(text: &str) -> Result<T> {
    serde_json::from_str(text.trim())
        .map_err(|e| Error::Other(format!("gh returned unparseable JSON: {e}")))
}

/// The shelbi-only fields carried in the fenced `<!-- shelbi:begin -->` block.
/// Every field is optional; unknown keys flatten into `params`, mirroring
/// [`Issue::params`] so a newer binary's fields survive an older read.
#[derive(Debug, Default, Deserialize, Serialize)]
struct ShelbiMeta {
    /// The authoritative shelbi id, carried here only when the `shelbi:id/*`
    /// anchor label had to be truncated to fit GitHub's 50-char cap (a short id
    /// round-trips through the label alone, so it is omitted). The read path
    /// prefers this over the label — see [`GhIssue::resolve_id`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workflow: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prefers_machine: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    priority: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    zen: Option<IssueZenConfig>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    launch: Option<IssueLaunchConfig>,
    #[serde(flatten, default)]
    params: BTreeMap<String, serde_yaml::Value>,
}

/// Split an issue body into `(prose, metadata)`: the fenced shelbi block is
/// removed from the returned prose and parsed as YAML into [`ShelbiMeta`]. A
/// body with no block yields the whole body as prose and a default (empty)
/// metadata. A malformed block is treated as absent — read must never fail on a
/// hand-edited body — so its fields simply default.
fn split_shelbi_meta(body: &str) -> (String, ShelbiMeta) {
    let Some(begin) = body.find(META_BEGIN) else {
        return (body.trim().to_string(), ShelbiMeta::default());
    };
    let after_begin = begin + META_BEGIN.len();
    let Some(end_rel) = body[after_begin..].find(META_END) else {
        // Unterminated marker: leave the body untouched, no metadata.
        return (body.trim().to_string(), ShelbiMeta::default());
    };
    let inner = &body[after_begin..after_begin + end_rel];
    let after_end = after_begin + end_rel + META_END.len();

    // Prose = everything before the marker + everything after it, with the
    // seam's surrounding blank lines collapsed so a stripped block doesn't
    // leave a double blank gap.
    let prose = format!("{}{}", &body[..begin], &body[after_end..]);
    let prose = prose.trim().to_string();

    let meta = parse_meta_yaml(inner).unwrap_or_default();
    (prose, meta)
}

/// Parse the inner text of a fenced shelbi block into [`ShelbiMeta`]. The inner
/// text is a fenced ```` ```yaml ```` code block; strip the fence lines and
/// deserialize the YAML. Returns `None` on any parse failure so a malformed
/// block reads as absent metadata rather than erroring the whole board.
fn parse_meta_yaml(inner: &str) -> Option<ShelbiMeta> {
    let yaml = strip_code_fence(inner);
    if yaml.trim().is_empty() {
        return Some(ShelbiMeta::default());
    }
    serde_yaml::from_str(&yaml).ok()
}

/// Strip a leading ```` ```yaml ```` / ```` ``` ```` fence and its closing
/// ```` ``` ```` from a block, returning the inner YAML. Lines outside a fence
/// are kept, so a block written without a fence still parses as raw YAML.
fn strip_code_fence(inner: &str) -> String {
    let trimmed = inner.trim();
    let mut lines: Vec<&str> = trimmed.lines().collect();
    if lines.first().is_some_and(|l| l.trim_start().starts_with("```")) {
        lines.remove(0);
    }
    if lines.last().is_some_and(|l| l.trim() == "```") {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reshape one canned REST issue object (the first non-blank JSONL line, or
    /// `""` for "no issue") into the GraphQL response the reworked `get` /
    /// `fetch` / `search` path expects, dispatching on the query name in `args`.
    /// This lets the existing REST-shaped fake runners keep driving `get` now
    /// that it resolves through GraphQL: a `search` gets the issue's number, a
    /// single or aliased `Issue` gets the mapped node.
    fn rest_to_graphql(args: &[&str], rest_issue: &str) -> String {
        let joined = args.join(" ");
        let first = rest_issue
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        let value: Option<serde_json::Value> =
            serde_json::from_str(first).ok().filter(serde_json::Value::is_object);
        if joined.contains("IdSearch") {
            let number = value
                .as_ref()
                .and_then(|v| v.get("number"))
                .and_then(serde_json::Value::as_i64);
            return match number {
                Some(n) => format!(
                    r#"{{"data":{{"rateLimit":{{"remaining":4999}},"search":{{"nodes":[{{"number":{n}}}]}}}}}}"#
                ),
                None => r#"{"data":{"rateLimit":{"remaining":4999},"search":{"nodes":[]}}}"#
                    .to_string(),
            };
        }
        let node = value.as_ref().map(rest_issue_to_gql_node);
        if joined.contains("IssuesByNumber") {
            return match node {
                Some(n) => format!(
                    r#"{{"data":{{"rateLimit":{{"remaining":4999}},"repository":{{"i0":{n}}}}}}}"#
                ),
                None => r#"{"data":{"rateLimit":{"remaining":4999},"repository":{}}}"#.to_string(),
            };
        }
        match node {
            Some(n) => format!(
                r#"{{"data":{{"rateLimit":{{"remaining":4999}},"repository":{{"issue":{n}}}}}}}"#
            ),
            None => r#"{"data":{"rateLimit":{"remaining":4999},"repository":{"issue":null}}}"#
                .to_string(),
        }
    }

    /// Map a REST issue object onto the GraphQL node shape [`GhIssueNode`] parses
    /// (camelCase keys, `labels { nodes { name } }`, upper-cased `state`).
    fn rest_issue_to_gql_node(v: &serde_json::Value) -> String {
        let label_nodes: Vec<serde_json::Value> = v
            .get("labels")
            .and_then(|l| l.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|l| serde_json::json!({ "name": l.get("name").cloned().unwrap_or(serde_json::Value::Null) }))
            .collect();
        serde_json::json!({
            "number": v.get("number").cloned().unwrap_or(serde_json::json!(0)),
            "title": v.get("title").cloned().unwrap_or(serde_json::json!("")),
            "state": v.get("state").and_then(|s| s.as_str()).unwrap_or("open").to_uppercase(),
            "stateReason": v.get("state_reason").cloned().unwrap_or(serde_json::Value::Null),
            "createdAt": v.get("created_at").cloned().unwrap_or(serde_json::json!("2026-01-01T00:00:00Z")),
            "updatedAt": v.get("updated_at").cloned().unwrap_or(serde_json::json!("2026-01-01T00:00:00Z")),
            "body": v.get("body").cloned().unwrap_or(serde_json::json!("")),
            "labels": { "nodes": label_nodes },
        })
        .to_string()
    }

    /// Build a store whose `gh` runner dispatches on the endpoint path in the
    /// args, returning canned JSONL. `issues_json` answers the issues endpoint;
    /// `comments_json` answers any `/comments` endpoint. A GraphQL call (the
    /// reworked `get`/`fetch`/`search` path) is answered from `issues_json`
    /// reshaped by [`rest_to_graphql`].
    fn store_with(
        issues_json: &'static str,
        comments_json: &'static str,
    ) -> GitHubStore {
        GitHubStore::with_runner("owner/repo", move |args| {
            if args.contains(&"graphql") {
                return Ok(rest_to_graphql(args, issues_json));
            }
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if path.contains("/comments") {
                Ok(comments_json.to_string())
            } else {
                Ok(issues_json.to_string())
            }
        })
    }

    #[test]
    fn rate_limited_park_error_round_trips_through_the_shared_classifiers() {
        // The short-circuit error a parked read returns must be indistinguishable
        // to every downstream classifier from the live 403 it stands in for:
        // classified as a rate limit, and carrying the reset it is parked until.
        let err = rate_limited_park_error("gh", &["api", "repos/owner/repo/issues"], 1_700_000_500);
        assert!(
            crate::gh_retry::is_rate_limit_error(&err),
            "a parked read must classify as rate-limited"
        );
        assert_eq!(
            crate::gh_retry::rate_limit_reset_epoch(&err, 1_700_000_000),
            Some(1_700_000_500),
            "the reset epoch must be recoverable from the parked error"
        );
    }

    #[test]
    fn http_response_body_splits_off_the_header_block() {
        // CRLF-separated (real gh --include output).
        assert_eq!(
            http_response_body("HTTP/2.0 200 OK\r\nx-ratelimit-reset: 5\r\n\r\n{\"ok\":true}"),
            "{\"ok\":true}"
        );
        // LF-separated.
        assert_eq!(
            http_response_body("HTTP/2.0 200 OK\nx-ratelimit-reset: 5\n\n{\"ok\":true}"),
            "{\"ok\":true}"
        );
        // No header block (a plain body): returned unchanged so it still parses.
        assert_eq!(http_response_body("{\"ok\":true}"), "{\"ok\":true}");
    }

    #[test]
    fn rate_limit_probe_parses_the_core_reset_from_the_body() {
        let body = r#"{"resources":{"core":{"limit":5000,"remaining":0,"reset":1700000123},"graphql":{"remaining":42}}}"#;
        let probe: RateLimitProbe = serde_json::from_str(body).unwrap();
        assert_eq!(probe.resources.core.reset, Some(1_700_000_123));
    }

    #[test]
    fn list_maps_labels_body_and_orders_the_board() {
        // Two issues: one in-progress with a full metadata block, one closed.
        let issues = r#"{"number":7,"title":"Do the thing","body":"Prose here.\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\nbranch: jlong/do-thing\ndepends_on: [other]\nprefers_machine: hub\npriority: 3\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/do-thing"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}
{"number":4,"title":"Old task","body":"done body","state":"closed","state_reason":"completed","labels":[{"name":"shelbi:id/old-task"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");

        let board = store.list().unwrap();
        assert_eq!(board.len(), 2);

        // Board order: in-progress sorts before done.
        assert_eq!(board[0].task.id, "do-thing");
        assert_eq!(board[1].task.id, "old-task");

        let first = &board[0];
        assert_eq!(first.task.column, Column::in_progress());
        assert_eq!(first.task.title, "Do the thing");
        assert_eq!(first.task.workflow.as_deref(), Some("app"));
        assert_eq!(first.task.branch.as_deref(), Some("jlong/do-thing"));
        assert_eq!(first.task.depends_on, vec!["other".to_string()]);
        assert_eq!(first.task.prefers_machine.as_deref(), Some("hub"));
        assert_eq!(first.task.priority, 3);
        // Assignment is never read from GitHub.
        assert_eq!(first.task.assigned_to, None);
        // The fenced block is stripped from the prose.
        assert_eq!(first.body, "Prose here.");
        assert!(!first.body.contains("shelbi:begin"));

        // Closed + completed → done.
        assert_eq!(board[1].task.column, Column::done());
    }

    #[test]
    fn closed_not_planned_is_canceled_and_overrides_a_stale_label() {
        // Closed with a stale non-terminal status label → terminal wins.
        let issues = r#"{"number":9,"title":"Abandoned","body":"","state":"closed","state_reason":"not_planned","labels":[{"name":"shelbi:id/abandoned"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board[0].task.column, Column::canceled());
    }

    #[test]
    fn open_issue_without_status_label_defaults_to_todo() {
        let issues = r#"{"number":1,"title":"Fresh","body":"hi","state":"open","labels":[{"name":"shelbi:id/fresh"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board[0].task.column, Column::todo());
    }

    #[test]
    fn issue_without_id_label_falls_back_to_number() {
        let issues = r#"{"number":42,"title":"Unmigrated","body":"body","state":"open","labels":[],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board[0].task.id, "42");
    }

    #[test]
    fn pull_requests_are_filtered_out() {
        let issues = r#"{"number":1,"title":"A real issue","body":"","state":"open","labels":[{"name":"shelbi:id/real"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}
{"number":2,"title":"A PR","body":"","state":"open","labels":[],"pull_request":{"url":"https://x"},"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");
        let board = store.list().unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].task.id, "real");
    }

    /// A store whose `gh` runner records every call (space-joined) and answers
    /// any issues query with `issues_json`. Returns the store and the shared
    /// call log so a test can assert the exact `-f state=…` each read path
    /// sends.
    fn recording_reader(
        issues_json: &'static str,
    ) -> (GitHubStore, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            if args.contains(&"graphql") {
                return Ok(rest_to_graphql(args, issues_json));
            }
            Ok(issues_json.to_string())
        });
        (store, calls)
    }

    /// The `gh api` issues-list calls recorded so far (a GET on the issues
    /// endpoint, excluding per-issue comment fetches).
    fn issues_list_calls(calls: &std::sync::Arc<std::sync::Mutex<Vec<String>>>) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.contains("repos/owner/repo/issues") && !c.contains("/comments"))
            .cloned()
            .collect()
    }

    #[test]
    fn list_in_status_requests_state_open_for_non_terminal() {
        // Every non-terminal status is served from the open issues alone — one
        // `state=open` list call, never the six-page `state=all` history.
        for col in [
            Column::backlog(),
            Column::todo(),
            Column::in_progress(),
            Column::review(),
        ] {
            let (store, calls) = recording_reader("");
            store.list_in_status(&col).unwrap();
            let list_calls = issues_list_calls(&calls);
            assert_eq!(list_calls.len(), 1, "{col}: exactly one list call");
            let call = &list_calls[0];
            assert!(call.contains("state=open"), "{col}: {call}");
            assert!(!call.contains("state=all"), "{col} must not sweep all: {call}");
            assert!(
                !call.contains("state=closed"),
                "{col} must not request closed: {call}"
            );
        }
    }

    #[test]
    fn list_in_status_requests_state_closed_for_terminal() {
        // The terminal lanes (`done`/`canceled`) are history — they read the
        // closed issues, never `state=all`, never `state=open`.
        for col in [Column::done(), Column::canceled()] {
            let (store, calls) = recording_reader("");
            store.list_in_status(&col).unwrap();
            let list_calls = issues_list_calls(&calls);
            assert_eq!(list_calls.len(), 1, "{col}: exactly one list call");
            let call = &list_calls[0];
            assert!(call.contains("state=closed"), "{col}: {call}");
            assert!(!call.contains("state=all"), "{col} must not sweep all: {call}");
            assert!(
                !call.contains("state=open"),
                "{col} must not request open: {call}"
            );
        }
    }

    #[test]
    fn list_open_requests_state_open() {
        let (store, calls) = recording_reader("");
        store.list_open().unwrap();
        let list_calls = issues_list_calls(&calls);
        assert_eq!(list_calls.len(), 1);
        assert!(list_calls[0].contains("state=open"), "{}", list_calls[0]);
        assert!(!list_calls[0].contains("state=all"), "{}", list_calls[0]);
    }

    #[test]
    fn list_keeps_the_full_state_all_sweep() {
        // `list` still carries its full-history contract for migrate / reconcile
        // callers — the one path that requests every issue.
        let (store, calls) = recording_reader("");
        store.list().unwrap();
        let list_calls = issues_list_calls(&calls);
        assert_eq!(list_calls.len(), 1);
        assert!(list_calls[0].contains("state=all"), "{}", list_calls[0]);
    }

    #[test]
    fn get_returns_the_matching_issue_and_none_for_missing() {
        let issues = r#"{"number":7,"title":"Do the thing","body":"prose","state":"open","labels":[{"name":"shelbi:id/do-thing"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        // The runner returns the issue for any issues query; for the "missing"
        // case we return an empty result. GraphQL reads (the reworked `get`) are
        // reshaped from the same JSON.
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            let has_missing = args.iter().any(|a| a.contains("shelbi:id/missing"));
            if args.contains(&"graphql") {
                return Ok(rest_to_graphql(args, if has_missing { "" } else { issues }));
            }
            if has_missing {
                Ok(String::new())
            } else {
                Ok(issues.to_string())
            }
        });

        let got = store.get("do-thing").unwrap().expect("issue exists");
        assert_eq!(got.task.id, "do-thing");
        assert_eq!(got.task.column, Column::review());
        assert_eq!(got.body, "prose");

        assert!(store.get("missing").unwrap().is_none());
    }

    #[test]
    fn list_comments_reads_live_and_orders_by_creation() {
        let issues = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let comments = r#"{"id":100,"body":"first","created_at":"2026-08-01T01:00:00Z","user":{"login":"alice"}}
{"id":101,"body":"second","created_at":"2026-08-01T02:00:00Z","user":{"login":"bob"}}"#;
        let store = store_with(issues, comments);

        let got = store.list_comments("t").unwrap();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0].body, "first");
        assert_eq!(got[0].author.as_deref(), Some("alice"));
        assert_eq!(got[0].id, "100");
        assert_eq!(got[1].body, "second");
        assert_eq!(got[1].author.as_deref(), Some("bob"));
    }

    #[test]
    fn list_comments_for_missing_issue_is_empty() {
        let store = GitHubStore::with_runner("owner/repo", |_args| Ok(String::new()));
        assert!(store.list_comments("nope").unwrap().is_empty());
    }

    #[test]
    fn unreachable_tracker_surfaces_a_command_error_not_stale_data() {
        let store = GitHubStore::with_runner("owner/repo", |args| {
            Err(Error::Command {
                cmd: format!("gh {}", args.join(" ")),
                status: "exit status: 1".to_string(),
                stderr: "could not resolve host: api.github.com".to_string(),
            })
        });
        let err = store.list().unwrap_err();
        match err {
            Error::Command { stderr, .. } => assert!(stderr.contains("could not resolve host")),
            other => panic!("expected Error::Command, got {other:?}"),
        }
    }

    #[test]
    fn body_without_a_meta_block_is_all_prose() {
        let (prose, meta) = split_shelbi_meta("Just a plain description.\n");
        assert_eq!(prose, "Just a plain description.");
        assert!(meta.workflow.is_none());
        assert_eq!(meta.priority, None);
    }

    #[test]
    fn malformed_meta_block_reads_as_absent_metadata() {
        // An unterminated block leaves the body intact and yields no metadata.
        let body = "Prose\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\n";
        let (prose, meta) = split_shelbi_meta(body);
        assert!(prose.contains("Prose"));
        assert!(meta.workflow.is_none());
    }

    #[test]
    fn meta_block_carries_unknown_keys_into_params() {
        let body = "P\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\nfeature: auth-rewrite\n```\n<!-- shelbi:end -->";
        let (_prose, meta) = split_shelbi_meta(body);
        assert_eq!(meta.workflow.as_deref(), Some("app"));
        assert_eq!(
            meta.params.get("feature").and_then(|v| v.as_str()),
            Some("auth-rewrite")
        );
    }

    #[test]
    fn poll_changes_watermark_surfaces_later_edits_and_comments() {
        // Issue updated_at 2026-08-02; a first poll from start reports nothing
        // and sets the high-water mark there.
        let issues = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let store = store_with(issues, "[]");

        let (changes, cursor) = store.poll_changes(&Cursor::start()).unwrap();
        assert!(changes.is_empty());
        assert_eq!(
            cursor.watermark(),
            Some("2026-08-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );

        // Now poll against an *older* cursor: the issue's updated_at is past it,
        // so it surfaces as an upsert, and its post-cursor comment as CommentAdded.
        let older = Cursor::at("2026-08-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap());
        let comments = r#"{"id":100,"body":"new comment","created_at":"2026-08-01T18:00:00Z","user":{"login":"alice"}}"#;
        let store = store_with(issues, comments);
        let (changes, _cursor) = store.poll_changes(&older).unwrap();

        let upserts = changes
            .iter()
            .filter(|c| matches!(c, IssueChange::Upserted(_)))
            .count();
        let comment_adds: Vec<_> = changes
            .iter()
            .filter_map(|c| match c {
                IssueChange::CommentAdded { issue_id, comment } => Some((issue_id, comment)),
                _ => None,
            })
            .collect();
        assert_eq!(upserts, 1);
        assert_eq!(comment_adds.len(), 1);
        assert_eq!(comment_adds[0].0, "t");
        assert_eq!(comment_adds[0].1.body, "new comment");
    }

    #[test]
    fn poll_changes_scopes_the_query_with_the_since_watermark() {
        // A recording runner captures every `gh` call so we can assert the
        // `since=` watermark is (a) absent on a cold first poll and (b) present
        // on both the issues query and the per-touched-issue comments query on a
        // warm poll — the rate-limit-respecting `since=` scoping (plan D3).
        let issues = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let comments = r#"{"id":100,"body":"new comment","created_at":"2026-08-01T18:00:00Z","user":{"login":"alice"}}"#;
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            Ok(if path.contains("/comments") { comments } else { issues }.to_string())
        });

        // Cold poll: no watermark, so no `since=` scoping — the whole board is
        // listed once to seed the high-water mark.
        let (_changes, cursor) = store.poll_changes(&Cursor::start()).unwrap();
        {
            let calls = calls.lock().unwrap();
            let issues_call = calls
                .iter()
                .find(|c| c.contains("repos/owner/repo/issues") && !c.contains("/comments"))
                .expect("issues list call");
            assert!(!issues_call.contains("since="), "cold poll must not scope: {issues_call}");
        }

        // Warm poll against an older cursor: the issue's updated_at (2026-08-02)
        // is past it, so it surfaces — and both the issues query and the comments
        // query carry the exact `since=<watermark>` param.
        calls.lock().unwrap().clear();
        let older = Cursor::at("2026-08-01T12:00:00Z".parse::<DateTime<Utc>>().unwrap());
        let (_changes, _next) = store.poll_changes(&older).unwrap();
        let calls = calls.lock().unwrap();
        let issues_call = calls
            .iter()
            .find(|c| c.contains("repos/owner/repo/issues") && !c.contains("/comments"))
            .expect("issues list call");
        assert!(
            issues_call.contains("since=2026-08-01T12:00:00+00:00"),
            "warm poll must scope issues: {issues_call}"
        );
        let comments_call = calls
            .iter()
            .find(|c| c.contains("/comments"))
            .expect("comments call");
        assert!(
            comments_call.contains("since=2026-08-01T12:00:00+00:00"),
            "warm poll must scope comments: {comments_call}"
        );

        // The cursor advanced to the issue's updated_at high-water mark.
        assert_eq!(
            cursor.watermark(),
            Some("2026-08-02T00:00:00Z".parse::<DateTime<Utc>>().unwrap())
        );
    }

    // --- write path ----------------------------------------------------------

    type Calls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

    /// A store whose `gh` runner records every call (space-joined) and answers
    /// reads from canned JSON: `by_id_json` for a `get`-by-id lookup (an issues
    /// GET carrying a `labels=shelbi:id/…` filter), `list_json` for a plain
    /// issues list, `labels_json` for a `GET .../labels`, `[]` for comments.
    /// Every mutating call (`POST` / `PATCH` / `PUT`) echoes `write_json`.
    fn recording_store(
        by_id_json: &'static str,
        list_json: &'static str,
        labels_json: &'static str,
        write_json: &'static str,
    ) -> (GitHubStore, Calls) {
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            // The reworked `get` reads through GraphQL; answer it from the same
            // single-issue JSON the REST `get_raw` path returns.
            if args.contains(&"graphql") {
                return Ok(rest_to_graphql(args, by_id_json));
            }
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            if method != "GET" {
                return Ok(write_json.to_string());
            }
            let path = args
                .iter()
                .find(|a| a.contains("repos/"))
                .copied()
                .unwrap_or("");
            if path.ends_with("/labels") {
                return Ok(labels_json.to_string());
            }
            if path.contains("/comments") {
                return Ok(String::new());
            }
            let by_id = args.iter().any(|a| a.contains("labels=shelbi:id/"));
            Ok(if by_id { by_id_json } else { list_json }.to_string())
        });
        (store, calls)
    }

    /// Every call that swaps a status label or opens/closes the issue. Used by
    /// the terminal / reopen assertions.
    fn call_containing<'a>(calls: &'a [String], needles: &[&str]) -> Option<&'a String> {
        calls
            .iter()
            .find(|c| needles.iter().all(|n| c.contains(n)))
    }

    #[test]
    fn add_creates_issue_with_anchor_labels_and_meta_block() {
        // Fresh repo: the id lookup and the column list are both empty, and no
        // labels exist yet.
        let created = r#"{"number":10,"title":"Do the thing","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", "", created);

        let mut spec = NewIssue::new("do-thing", "Do the thing", Column::todo(), "Prose body");
        spec.workflow = Some("app".into());
        spec.branch = Some("jlong/do-thing".into());
        let issue = store.add(spec).unwrap();
        assert_eq!(issue.id, "do-thing");
        assert_eq!(issue.priority, 0); // appended to an empty column
        assert_eq!(
            issue.created_at,
            "2026-08-03T00:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );

        let calls = calls.lock().unwrap();
        // The status label set is auto-created (todo among the stock set).
        assert!(call_containing(
            &calls,
            &["-X POST", "repos/owner/repo/labels", "name=shelbi:status/todo"]
        )
        .is_some());
        // The create carries both anchor labels and a metadata block.
        let create = call_containing(&calls, &["-X POST", "repos/owner/repo/issues", "title=Do the thing"])
            .expect("issue create POST");
        assert!(create.contains("labels[]=shelbi:id/do-thing"));
        assert!(create.contains("labels[]=shelbi:status/todo"));
        assert!(create.contains("workflow: app"));
        assert!(create.contains("branch: jlong/do-thing"));
        assert!(create.contains("priority: 0"));
        assert!(create.contains("Prose body"));
        assert!(create.contains(META_BEGIN));
    }

    #[test]
    fn add_retries_a_secondary_rate_limit_on_the_create_and_completes() {
        // The issue-create POST is rate-limited once (403 secondary limit with a
        // Retry-After), then succeeds. With a no-wait retry policy the `add`
        // still lands, exercising the store's retry seam end to end.
        use std::sync::atomic::{AtomicU32, Ordering};
        let created = r#"{"number":10,"title":"T","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let create_posts = std::sync::Arc::new(AtomicU32::new(0));
        let counter = create_posts.clone();
        let sleep: std::sync::Arc<dyn Fn(std::time::Duration) + Send + Sync> =
            std::sync::Arc::new(|_| {});
        let notify: std::sync::Arc<dyn Fn(&crate::gh_retry::RetryNotice) + Send + Sync> =
            std::sync::Arc::new(|_| {});
        let policy = crate::gh_retry::RetryPolicy::for_test(5, sleep, notify);
        let store = GitHubStore::with_runner_and_policy("owner/repo", policy, move |args| {
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if method == "POST" && path.ends_with("/issues") {
                // First create attempt is throttled; the retry succeeds.
                if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(Error::Command {
                        cmd: "gh api ...".into(),
                        status: "exit status: 1".into(),
                        stderr: "HTTP 403: You have exceeded a secondary rate limit.\nRetry-After: 1".into(),
                    });
                }
                return Ok(created.to_string());
            }
            if method != "GET" {
                return Ok("{}".to_string());
            }
            // All reads (dup check, column list, labels) come back empty.
            Ok(String::new())
        });

        let issue = store
            .add(NewIssue::new("do-thing", "Do the thing", Column::todo(), "b"))
            .unwrap();
        assert_eq!(issue.id, "do-thing");
        assert_eq!(create_posts.load(Ordering::SeqCst), 2, "create was retried once");
    }

    #[test]
    fn add_does_not_retry_a_terminal_validation_error_on_the_create() {
        // A 422 validation failure on the create is terminal — `add` fails after
        // exactly one create attempt, never spinning on a permanent error.
        use std::sync::atomic::{AtomicU32, Ordering};
        let create_posts = std::sync::Arc::new(AtomicU32::new(0));
        let counter = create_posts.clone();
        let sleep: std::sync::Arc<dyn Fn(std::time::Duration) + Send + Sync> =
            std::sync::Arc::new(|_| {});
        let notify: std::sync::Arc<dyn Fn(&crate::gh_retry::RetryNotice) + Send + Sync> =
            std::sync::Arc::new(|_| {});
        let policy = crate::gh_retry::RetryPolicy::for_test(5, sleep, notify);
        let store = GitHubStore::with_runner_and_policy("owner/repo", policy, move |args| {
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if method == "POST" && path.ends_with("/issues") {
                counter.fetch_add(1, Ordering::SeqCst);
                return Err(Error::Command {
                    cmd: "gh api ...".into(),
                    status: "exit status: 1".into(),
                    stderr: "HTTP 422: Validation Failed\ninvalid field".into(),
                });
            }
            if method != "GET" {
                return Ok("{}".to_string());
            }
            Ok(String::new())
        });

        assert!(store
            .add(NewIssue::new("do-thing", "Do the thing", Column::todo(), "b"))
            .is_err());
        assert_eq!(create_posts.load(Ordering::SeqCst), 1, "no retry on a 422");
    }

    #[test]
    fn list_under_the_primary_rate_limit_fails_fast_without_retrying() {
        // A read (`GET`) under the exhausted primary limit — `API rate limit
        // exceeded`, which `gh` reports with no Retry-After — must return Err
        // after exactly one attempt and never sleep, so a board read is never
        // blocked for minutes behind backoff.
        use std::sync::atomic::{AtomicU32, Ordering};
        let get_calls = std::sync::Arc::new(AtomicU32::new(0));
        let counter = get_calls.clone();
        let slept = std::sync::Arc::new(AtomicU32::new(0));
        let sleep_counter = slept.clone();
        let read_sleep: std::sync::Arc<dyn Fn(std::time::Duration) + Send + Sync> =
            std::sync::Arc::new(move |_| {
                sleep_counter.fetch_add(1, Ordering::SeqCst);
            });
        let notify: std::sync::Arc<dyn Fn(&crate::gh_retry::RetryNotice) + Send + Sync> =
            std::sync::Arc::new(|_| {});
        let read_policy = crate::gh_retry::RetryPolicy::for_test_reads(2, read_sleep, notify.clone());
        let write_policy = crate::gh_retry::RetryPolicy::for_test(5, std::sync::Arc::new(|_| {}), notify);
        let store = GitHubStore::with_runner_and_policies(
            "owner/repo",
            read_policy,
            write_policy,
            move |args| {
                let method = args
                    .iter()
                    .position(|a| *a == "-X")
                    .and_then(|i| args.get(i + 1))
                    .copied()
                    .unwrap_or("GET");
                assert_eq!(method, "GET", "list issues only a GET");
                counter.fetch_add(1, Ordering::SeqCst);
                Err(Error::Command {
                    cmd: "gh api ...".into(),
                    status: "exit status: 1".into(),
                    stderr: "HTTP 403: API rate limit exceeded for user ID 1.".into(),
                })
            },
        );

        assert!(store.list().is_err());
        assert_eq!(get_calls.load(Ordering::SeqCst), 1, "read is not retried");
        assert_eq!(slept.load(Ordering::SeqCst), 0, "read never sleeps on a hintless limit");
    }

    #[test]
    fn add_rejects_a_duplicate_id() {
        let existing = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/do-thing"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, _calls) = recording_store(existing, existing, "", "{}");
        assert!(store
            .add(NewIssue::new("do-thing", "T", Column::todo(), "b"))
            .is_err());
    }

    #[test]
    fn add_into_a_terminal_status_closes_the_issue() {
        let created = r#"{"number":11,"title":"Done thing","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", "", created);
        store
            .add(NewIssue::new("done-thing", "Done thing", Column::done(), "b"))
            .unwrap();
        let calls = calls.lock().unwrap();
        assert!(call_containing(
            &calls,
            &["-X PATCH", "repos/owner/repo/issues/11", "state=closed", "state_reason=completed"]
        )
        .is_some());
    }

    #[test]
    fn move_status_swaps_the_label_and_closes_on_a_terminal_target() {
        // In-progress issue with a workflow in its meta block.
        let issue = r#"{"number":7,"title":"T","body":"P\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");

        let mv = store
            .move_status("t", &Column::done(), "accept")
            .unwrap()
            .expect("status changed");
        assert_eq!(mv.from, Column::in_progress());
        assert_eq!(mv.to, Column::done());
        assert_eq!(mv.workflow, "app");

        let calls = calls.lock().unwrap();
        let put = call_containing(&calls, &["-X PUT", "repos/owner/repo/issues/7/labels"])
            .expect("PUT labels");
        // New status applied, old status dropped, id anchor preserved.
        assert!(put.contains("labels[]=shelbi:status/done"));
        assert!(put.contains("labels[]=shelbi:id/t"));
        assert!(!put.contains("shelbi:status/in-progress"));
        // Terminal target closes the issue as completed.
        assert!(call_containing(
            &calls,
            &["-X PATCH", "state=closed", "state_reason=completed"]
        )
        .is_some());
    }

    #[test]
    fn move_status_is_a_noop_when_already_in_the_target() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        assert!(store.move_status("t", &Column::review(), "again").unwrap().is_none());
        // No label swap issued.
        assert!(call_containing(&calls.lock().unwrap(), &["-X PUT", "/labels"]).is_none());
    }

    #[test]
    fn move_status_reopens_a_closed_issue_for_a_non_terminal_target() {
        // A closed (done) issue moved back into an active lane must reopen.
        let issue = r#"{"number":7,"title":"T","body":"","state":"closed","state_reason":"completed","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/done"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        let mv = store
            .move_status("t", &Column::in_progress(), "reopen")
            .unwrap()
            .expect("moved");
        assert_eq!(mv.from, Column::done());
        assert_eq!(mv.to, Column::in_progress());
        assert!(call_containing(&calls.lock().unwrap(), &["-X PATCH", "state=open"]).is_some());
    }

    #[test]
    fn cancel_closes_the_issue_as_not_planned() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        let mv = store.cancel("t", "obsolete").unwrap().expect("moved");
        assert_eq!(mv.to, Column::canceled());
        assert!(call_containing(
            &calls.lock().unwrap(),
            &["-X PATCH", "state=closed", "state_reason=not_planned"]
        )
        .is_some());
    }

    #[test]
    fn set_fields_rewrites_only_the_meta_block() {
        let issue = r#"{"number":7,"title":"T","body":"Prose stays.\n\n<!-- shelbi:begin -->\n```yaml\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    branch: Some(Some("jlong/t".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let calls = calls.lock().unwrap();
        let patch = call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/7"])
            .expect("body PATCH");
        assert!(patch.contains("branch: jlong/t"));
        // Human prose is preserved around the block.
        assert!(patch.contains("Prose stays."));
    }

    #[test]
    fn set_fields_patches_title_and_body_prose() {
        let issue = r#"{"number":7,"title":"Old","body":"Old prose.\n\n<!-- shelbi:begin -->\n```yaml\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    title: Some("New title".into()),
                    body: Some("Fresh prose.".into()),
                    workflow: Some(Some("app".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let calls = calls.lock().unwrap();
        let patch = call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/7"])
            .expect("title/body PATCH");
        assert!(patch.contains("title=New title"));
        assert!(patch.contains("Fresh prose."));
        assert!(patch.contains("workflow: app"));
    }

    #[test]
    fn delete_closes_the_issue_as_not_planned() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store.delete("t").unwrap();
        assert!(call_containing(
            &calls.lock().unwrap(),
            &["-X PATCH", "state=closed", "state_reason=not_planned"]
        )
        .is_some());
    }

    #[test]
    fn set_fields_with_only_assigned_to_touches_the_overlay_not_the_api() {
        // Assignment is ephemeral local routing; GitHub never stores it, so an
        // `assigned_to`-only update must not touch the API at all — it lands in
        // the local overlay instead, and a subsequent read folds it back on.
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("alpha".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        // No gh API call — the only reads/writes were the local overlay.
        assert!(calls.lock().unwrap().is_empty());

        // The overlay is folded onto reads.
        assert_eq!(
            store.get("t").unwrap().unwrap().task.assigned_to.as_deref(),
            Some("alpha")
        );
        assert_eq!(
            store.list().unwrap()[0].task.assigned_to.as_deref(),
            Some("alpha")
        );

        // Clearing it removes the overlay again.
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(None),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(store.get("t").unwrap().unwrap().task.assigned_to, None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn move_and_unassign_and_park_clear_the_assignment_overlay() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let review = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, _calls) = recording_store(review, review, "", "{}");

        // Assign, then move-and-unassign back to todo: the overlay is cleared.
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        store
            .move_status_and_unassign("t", &Column::todo(), "stop")
            .unwrap();
        assert_eq!(crate::get_task_assignment("test-project", "t").unwrap(), None);

        // Re-assign, then park: park reports the prior owner, clears the overlay,
        // and sets the local parked marker.
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        let was = store.park_review("t").unwrap();
        assert_eq!(was.as_deref(), Some("review-1"));
        assert_eq!(crate::get_task_assignment("test-project", "t").unwrap(), None);
        assert!(crate::is_task_parked("test-project", "t").unwrap());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn reject_review_appends_feedback_moves_back_to_ready_and_clears_the_owner() {
        let _g = crate::test_lock::LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A review-status issue with human prose and a fenced metadata block,
        // owned by the review slot via the local overlay.
        let review = r#"{"number":7,"title":"T","body":"Original prose.\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/review"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(review, review, "", "{}");
        store
            .set_fields(
                "t",
                IssueFields {
                    assigned_to: Some(Some("review-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();

        let mv = store
            .reject_review("t", &Column::todo(), "please restore the handler", "2026-09-03")
            .unwrap()
            .expect("status changed review -> todo");
        assert_eq!(mv.from, Column::review());
        assert_eq!(mv.to, Column::todo());
        assert_eq!(mv.workflow, "app");

        let calls = calls.lock().unwrap();
        // The body PATCH appends the dated feedback section while preserving the
        // original prose and the metadata block.
        let body_patch = call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/7", "body="])
            .expect("body PATCH");
        assert!(body_patch.contains("Review feedback (rejected 2026-09-03)"));
        assert!(body_patch.contains("please restore the handler"));
        assert!(body_patch.contains("Original prose."));
        assert!(body_patch.contains(META_BEGIN));
        // The status label was swapped to the ready column (todo).
        assert!(call_containing(
            &calls,
            &["-X PUT", "repos/owner/repo/issues/7/labels", "labels[]=shelbi:status/todo"]
        )
        .is_some());
        // The owner overlay is cleared — the freed review slot has no stale owner.
        assert_eq!(crate::get_task_assignment("test-project", "t").unwrap(), None);

        std::env::remove_var("SHELBI_HOME");
    }

    /// The read-path park writes home-keyed shared state (`gh-budget/` + a
    /// `board rate-limited` events.log line). Those writes must be **inert under
    /// test by default** — a background board-refresh thread that raced a real
    /// 403 must not scribble into whichever test's `SHELBI_HOME` is mounted —
    /// and only fire when a test explicitly opts in. This is the guard that
    /// broke the 44-failure cascade.
    #[test]
    fn read_path_park_is_inert_under_test_unless_opted_in() {
        // Poison-tolerant so a panic in this test can't cascade PoisonError
        // failures onto every later shelbi-state test that shares the lock.
        let _g = crate::test_lock::LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let key = crate::gh_budget::token_key("tok-park-inert");
        let now = 1_000i64;
        let reset = now + 3_600;

        // Default in a test build: the gate is closed, so a caller that respects
        // it (as `park_aware_read` does) performs no park and no events.log write.
        assert!(
            !read_park_side_effects_enabled(),
            "park side effects must default off under test"
        );

        // Opt in: the bookkeeping parks the token and writes exactly one line.
        set_test_park_side_effects(true);
        assert!(read_park_side_effects_enabled());
        record_read_park("park-inert-proj", &key, reset, now);
        assert_eq!(
            crate::gh_budget::parked_until(&key, crate::gh_budget::Budget::Rest, now),
            Some(reset),
            "opting in must actually park the token"
        );
        let log = std::fs::read_to_string(crate::events_log_path().unwrap()).unwrap_or_default();
        let hits = log
            .lines()
            .filter(|l| l.contains("project=park-inert-proj") && l.contains("board rate-limited"))
            .count();
        assert_eq!(hits, 1, "exactly one board rate-limited line per window");

        // Reset the opt-in so it can't leak into a later test on this process.
        set_test_park_side_effects(false);
        assert!(!read_park_side_effects_enabled());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn write_reserve_refuses_a_mutation_naming_the_reset_and_sends_no_write() {
        // Acceptance (plan §6): with the REST budget under the reserve floor an
        // `issue move` is refused up front — the message names the reset time and
        // no mutating request is sent, rather than failing on a 403 mid-transition.
        let _g = crate::test_lock::LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::set_var("GH_TOKEN", "tok-reserve-test");

        // Seed the token's REST budget below the default reserve (100), with a
        // known reset the refusal must echo.
        let key = crate::gh_budget::token_key("tok-reserve-test");
        let reset = 1_800_000_000i64;
        crate::gh_budget::record(&key, crate::gh_budget::Budget::Rest, Some(50), Some(reset));
        // The reserve is inert under test by default; opt it in.
        set_test_park_side_effects(true);

        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"},{"name":"shelbi:status/in-progress"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let (store, calls) = recording_store(issue, issue, "", "{}");

        let err = store
            .move_status("t", &Column::review(), "handoff")
            .expect_err("a low REST budget must refuse the move");
        let msg = err.to_string();
        let reset_rfc = DateTime::from_timestamp(reset, 0).unwrap().to_rfc3339();
        assert!(msg.contains(&reset_rfc), "refusal names the reset time: {msg}");
        assert!(msg.contains("50"), "refusal names the remaining: {msg}");

        // No mutating request reached `gh`: refused before the write.
        let calls = calls.lock().unwrap();
        assert!(
            !calls.iter().any(|c| {
                c.contains("-X POST")
                    || c.contains("-X PATCH")
                    || c.contains("-X PUT")
                    || c.contains("-X DELETE")
            }),
            "no mutating request must be sent under a low REST budget: {calls:?}"
        );

        set_test_park_side_effects(false);
        std::env::remove_var("GH_TOKEN");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn graphql_rate_limit_extracts_and_parks_once() {
        // The `rateLimit` parser pulls remaining/resetAt from any GraphQL envelope
        // (board, single-issue, search, batch all carry `data.rateLimit`).
        let body = r#"{"data":{"rateLimit":{"remaining":1234,"resetAt":"2026-09-08T01:00:00Z"},"repository":{"issue":null}}}"#;
        let (remaining, reset) = extract_graphql_rate_limit(body).expect("rateLimit present");
        assert_eq!(remaining, Some(1234));
        let expected = DateTime::parse_from_rfc3339("2026-09-08T01:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(reset, Some(expected));
        // A bare `gh` error string is not a GraphQL envelope.
        assert!(extract_graphql_rate_limit("gh: HTTP 403 Forbidden").is_none());

        // Park-once: two 429s in the same window write exactly one events line and
        // the GraphQL budget short-circuits until its reset, leaving REST alone.
        let _g = crate::test_lock::LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        set_test_park_side_effects(true);

        let key = crate::gh_budget::token_key("tok-gql-park");
        let now = 2_000i64;
        let reset_epoch = now + 3_600;
        record_graphql_park("gql-park-proj", &key, reset_epoch, now);
        record_graphql_park("gql-park-proj", &key, reset_epoch, now + 10);
        assert_eq!(
            crate::gh_budget::parked_until(&key, crate::gh_budget::Budget::Graphql, now),
            Some(reset_epoch),
            "the GraphQL budget is parked until reset"
        );
        assert_eq!(
            crate::gh_budget::parked_until(&key, crate::gh_budget::Budget::Rest, now),
            None,
            "the REST budget is untouched by a GraphQL park"
        );
        let log = std::fs::read_to_string(crate::events_log_path().unwrap()).unwrap_or_default();
        let hits = log
            .lines()
            .filter(|l| l.contains("project=gql-park-proj") && l.contains("board rate-limited"))
            .count();
        assert_eq!(hits, 1, "exactly one board rate-limited line per window");

        set_test_park_side_effects(false);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A throwaway `SHELBI_HOME` for tests that exercise the local assignment
    /// overlay (marker files under `<project_dir>/assignments/`).
    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-github-store-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn set_priority_renumbers_the_column_contiguously() {
        // Three issues a(0) b(1) c(2) in todo; send c to the top.
        let list = r#"{"number":1,"title":"A","body":"<!-- shelbi:begin -->\n```yaml\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/a"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}
{"number":2,"title":"B","body":"<!-- shelbi:begin -->\n```yaml\npriority: 1\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/b"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}
{"number":3,"title":"C","body":"<!-- shelbi:begin -->\n```yaml\npriority: 2\n```\n<!-- shelbi:end -->","state":"open","labels":[{"name":"shelbi:id/c"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;

        // A by-id GET returns just the matching issue so each rewrite patches the
        // right number; a plain list returns all three.
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let table = [("a", 1, 0), ("b", 2, 1), ("c", 3, 2)];
        let rest_line = |id: &str, num: i64, prio: i64| {
            format!(
                r#"{{"number":{num},"title":"{id}","body":"<!-- shelbi:begin -->\n```yaml\npriority: {prio}\n```\n<!-- shelbi:end -->","state":"open","labels":[{{"name":"shelbi:id/{id}"}},{{"name":"shelbi:status/todo"}}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}}"#
            )
        };
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            // The reworked `get` (which set_priority uses to read the target)
            // reads through GraphQL: answer the search and single-issue queries
            // for whichever id/number they name.
            if args.contains(&"graphql") {
                let joined = args.join(" ");
                let rest = table
                    .iter()
                    .find(|(id, num, _)| {
                        joined.contains(&format!("shelbi:id/{id}"))
                            || joined.contains(&format!("number={num}"))
                    })
                    .map(|(id, num, prio)| rest_line(id, *num, *prio))
                    .unwrap_or_default();
                return Ok(rest_to_graphql(args, &rest));
            }
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            if method != "GET" {
                return Ok("{}".to_string());
            }
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if path.ends_with("/labels") {
                return Ok(String::new());
            }
            for (id, num, prio) in table {
                if args.iter().any(|a| *a == format!("labels=shelbi:id/{id}")) {
                    return Ok(rest_line(id, num, prio));
                }
            }
            Ok(list.to_string())
        });

        store.set_priority("c", PrioMove::Top).unwrap();

        let calls = calls.lock().unwrap();
        // c (issue 3) → priority 0, a (issue 1) → 1, b (issue 2) → 2.
        assert!(call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/3", "priority: 0"]).is_some());
        assert!(call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/1", "priority: 1"]).is_some());
        assert!(call_containing(&calls, &["-X PATCH", "repos/owner/repo/issues/2", "priority: 2"]).is_some());
    }

    #[test]
    fn add_comment_posts_and_returns_the_created_comment() {
        let issue = r#"{"number":7,"title":"T","body":"","state":"open","labels":[{"name":"shelbi:id/t"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#;
        let comment = r#"{"id":555,"body":"hello there","created_at":"2026-08-03T00:00:00Z","user":{"login":"alice"}}"#;
        let (store, calls) = recording_store(issue, issue, "", comment);
        let c = store.add_comment("t", "hello there").unwrap();
        assert_eq!(c.id, "555");
        assert_eq!(c.body, "hello there");
        assert_eq!(c.author.as_deref(), Some("alice"));
        assert!(call_containing(
            &calls.lock().unwrap(),
            &["-X POST", "repos/owner/repo/issues/7/comments", "body=hello there"]
        )
        .is_some());
    }

    #[test]
    fn add_comment_on_a_missing_issue_errors() {
        let (store, _calls) = recording_store("", "", "", "{}");
        assert!(store.add_comment("nope", "hi").is_err());
    }

    #[test]
    fn existing_labels_are_not_recreated() {
        // Every label the create needs already exists → no label POSTs.
        let labels = r#"{"name":"shelbi:id/do-thing"}
{"name":"shelbi:status/backlog"}
{"name":"shelbi:status/todo"}
{"name":"shelbi:status/in-progress"}
{"name":"shelbi:status/review"}
{"name":"shelbi:status/done"}
{"name":"shelbi:status/canceled"}"#;
        let created = r#"{"number":10,"title":"Do the thing","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", labels, created);
        store
            .add(NewIssue::new("do-thing", "Do the thing", Column::todo(), "b"))
            .unwrap();
        assert!(call_containing(&calls.lock().unwrap(), &["-X POST", "repos/owner/repo/labels"]).is_none());
    }

    #[test]
    fn create_label_tolerates_a_concurrent_already_exists() {
        // The label list is stale (empty) so the store tries to create, but the
        // POST races and GitHub answers 422 already_exists — which must be
        // swallowed so `add` still succeeds.
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            let path = args.iter().find(|a| a.contains("repos/")).copied().unwrap_or("");
            if method == "POST" && path.ends_with("/labels") {
                return Err(Error::Command {
                    cmd: "gh api ...".into(),
                    status: "exit status: 1".into(),
                    stderr: "HTTP 422: Validation Failed (already_exists)".into(),
                });
            }
            if method != "GET" {
                return Ok(r#"{"number":10,"title":"X","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#.to_string());
            }
            if path.ends_with("/labels") {
                return Ok(String::new());
            }
            Ok(String::new())
        });
        assert!(store
            .add(NewIssue::new("x", "X", Column::todo(), "b"))
            .is_ok());
    }

    #[test]
    fn build_body_round_trips_through_split_shelbi_meta() {
        let meta = ShelbiMeta {
            workflow: Some("app".into()),
            branch: Some("jlong/x".into()),
            priority: Some(4),
            ..Default::default()
        };
        let body = build_body("Human prose.", &meta);
        let (prose, parsed) = split_shelbi_meta(&body);
        assert_eq!(prose, "Human prose.");
        assert_eq!(parsed.workflow.as_deref(), Some("app"));
        assert_eq!(parsed.branch.as_deref(), Some("jlong/x"));
        assert_eq!(parsed.priority, Some(4));

        // An all-empty meta emits no block at all.
        let plain = build_body("Just prose.", &ShelbiMeta::default());
        assert!(!plain.contains(META_BEGIN));
        assert_eq!(plain.trim(), "Just prose.");
    }

    // --- id-label truncation (GitHub's 50-char label cap) --------------------

    #[test]
    fn id_label_short_id_is_verbatim_and_unchanged() {
        // A short id (prefix + id <= 50) keeps its exact current anchor, so a
        // repo already migrated under short ids sees no churn.
        assert_eq!(id_label("do-thing"), "shelbi:id/do-thing");
        assert!(!id_is_truncated("do-thing"));

        // Exactly at the boundary: a 40-char id → a 50-char label, still verbatim.
        let id40 = "a".repeat(40);
        assert_eq!(id40.len(), 40);
        let label = id_label(&id40);
        assert_eq!(label, format!("shelbi:id/{id40}"));
        assert_eq!(label.len(), GITHUB_LABEL_MAX);
        assert!(!id_is_truncated(&id40));
    }

    #[test]
    fn id_label_long_id_truncates_within_the_cap() {
        // One over the boundary: a 41-char id would make a 51-char label, so it
        // is truncated to a 31-byte slug plus an 8-hex-char hash.
        let id41 = "b".repeat(41);
        assert!(id_is_truncated(&id41));
        let label = id_label(&id41);
        assert!(
            label.len() <= GITHUB_LABEL_MAX,
            "label exceeds the cap: {label} ({} chars)",
            label.len()
        );
        assert!(label.starts_with(ID_LABEL_PREFIX));
        // slug is the first 31 bytes of the id.
        assert!(label.starts_with(&format!("{ID_LABEL_PREFIX}{}-", &id41[..ID_SLUG_BUDGET])));
        // The tail is 8 lowercase hex digits.
        let tail = label.rsplit('-').next().unwrap();
        assert_eq!(tail.len(), 8);
        assert!(tail.chars().all(|c| c.is_ascii_hexdigit()));

        // The real longest-id class from the migration also fits.
        let long = "review-load-retry-loop-when-branch-is-checked-out-in-another-worktree";
        assert!(id_label(long).len() <= GITHUB_LABEL_MAX);
    }

    #[test]
    fn ids_sharing_their_first_31_chars_get_distinct_labels() {
        let a = "review-load-retry-loop-when-branch-checked-out-worktree-a";
        let b = "review-load-retry-loop-when-branch-checked-out-worktree-b";
        assert_eq!(&a[..ID_SLUG_BUDGET], &b[..ID_SLUG_BUDGET], "test ids must share the slug");
        assert_ne!(id_label(a), id_label(b), "same slug must be disambiguated by the hash");
    }

    #[test]
    fn id_label_hash_is_stable_across_builds() {
        // Pin a known id → known label. FNV-1a is chosen precisely so this value
        // can never drift between releases; a change here would orphan every
        // already-migrated long-id issue, so it must be a conscious edit.
        let id = "review-load-retry-loop-when-branch-is-checked-out-in-another-worktree";
        assert_eq!(fnv1a64(id) as u32, 0x6e57_f8c4);
        assert_eq!(
            id_label(id),
            "shelbi:id/review-load-retry-loop-when-bra-6e57f8c4"
        );
    }

    #[test]
    fn add_a_long_id_uses_a_truncated_label_and_carries_the_full_id_in_the_body() {
        let created = r#"{"number":10,"title":"T","body":"","state":"open","labels":[],"created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z"}"#;
        let (store, calls) = recording_store("", "", "", created);

        let id = "review-load-retry-loop-when-branch-is-checked-out-in-another-worktree";
        store
            .add(NewIssue::new(id, "T", Column::todo(), "Prose body"))
            .unwrap();

        let calls = calls.lock().unwrap();
        let create = call_containing(&calls, &["-X POST", "repos/owner/repo/issues", "title=T"])
            .expect("issue create POST");
        // The anchor label is the truncated form (never the 68-char raw id).
        assert!(create.contains("labels[]=shelbi:id/review-load-retry-loop-when-bra-6e57f8c4"));
        assert!(!create.contains(&format!("labels[]=shelbi:id/{id}")));
        // The authoritative id rides in the body metadata block for read-back.
        assert!(create.contains(&format!("id: {id}")));

        // The by-id lookup the dup check ran queries the *truncated* label, so a
        // re-run still resolves the card server-side.
        assert!(calls
            .iter()
            .any(|c| c.contains("labels=shelbi:id/review-load-retry-loop-when-bra-6e57f8c4")));
    }

    #[test]
    fn a_long_id_round_trips_losslessly_through_read_back() {
        // Simulate the issue as `add` would have written it: truncated anchor
        // label + full id in the metadata block. Reading it back yields the full
        // id byte-for-byte, and never the number or the truncated slug.
        let id = "review-load-retry-loop-when-branch-is-checked-out-in-another-worktree";
        let issue = format!(
            r#"{{"number":10,"title":"T","body":"Prose.\n\n<!-- shelbi:begin -->\n```yaml\nid: {id}\npriority: 0\n```\n<!-- shelbi:end -->","state":"open","labels":[{{"name":"shelbi:id/review-load-retry-loop-when-bra-6e57f8c4"}},{{"name":"shelbi:status/todo"}}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}}"#
        );
        let leaked: &'static str = Box::leak(issue.into_boxed_str());
        let store = store_with(leaked, "[]");

        let board = store.list().unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].task.id, id);
        // The stripped-of-metadata prose survives too.
        assert_eq!(board[0].body, "Prose.");

        // `get` resolves via the same single server-side label query.
        let got = store.get(id).unwrap().expect("issue exists");
        assert_eq!(got.task.id, id);
    }

    // --- GraphQL board index (Phase 2) --------------------------------------

    /// RAII guard pointing `$SHELBI_HOME` at a fresh temp dir under the shared
    /// test lock, so `refresh_board`'s `task_assignments` read is deterministic
    /// (an empty overlay unless the test writes one) and never touches the
    /// developer's real `~/.shelbi`.
    struct IsolatedHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        home: std::path::PathBuf,
    }
    impl IsolatedHome {
        fn new(tag: &str) -> Self {
            let lock = crate::test_lock::LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let home = std::env::temp_dir().join(format!(
                "shelbi-gh-graphql-{tag}-{}-{}",
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

    /// A store whose `gh` runner answers any GraphQL call with `json` (and
    /// asserts the call really is a `graphql` invocation). `refresh_board` reads
    /// only through the GraphQL runner, so this drives the whole board path.
    fn graphql_store(json: &'static str) -> GitHubStore {
        GitHubStore::with_runner("owner/repo", move |args| {
            assert!(
                args.contains(&"graphql"),
                "expected a graphql call, got {args:?}"
            );
            Ok(json.to_string())
        })
    }

    /// A minimal open `IssueFile` for a previous-board fixture.
    fn prev_issue(id: &str, column: &str, priority: u32) -> IssueFile {
        let task: shelbi_core::Issue = serde_yaml::from_str(&format!(
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
    fn refresh_board_cold_maps_open_issues_and_surfaces_the_budget() {
        let _iso = IsolatedHome::new("cold");
        // One GraphQL page of open issues: the node shape mirrors the REST
        // mapping (labels → id/status/column, fenced metadata block), and
        // `rateLimit` rides along.
        let json = r#"{"data":{"rateLimit":{"cost":11,"remaining":4989,"resetAt":"2026-09-08T01:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"c1"},"nodes":[
{"number":7,"title":"Do the thing","state":"OPEN","stateReason":null,"createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z","body":"Prose here.\n\n<!-- shelbi:begin -->\n```yaml\nworkflow: app\npriority: 2\n```\n<!-- shelbi:end -->","labels":{"nodes":[{"name":"shelbi:id/do-thing"},{"name":"shelbi:status/in-progress"}]}},
{"number":3,"title":"Fresh","state":"OPEN","stateReason":null,"createdAt":"2026-08-03T00:00:00Z","updatedAt":"2026-08-03T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/fresh"},{"name":"shelbi:status/todo"}]}}
]}}}}"#;
        let store = graphql_store(json);

        let read = store.refresh_board(None, &[]).unwrap();
        assert_eq!(read.board.len(), 2);
        // Canonical order: todo sorts before in-progress.
        assert_eq!(read.board[0].task.id, "fresh");
        assert_eq!(read.board[0].task.column, Column::todo());
        assert_eq!(read.board[1].task.id, "do-thing");
        assert_eq!(read.board[1].task.column, Column::in_progress());
        assert_eq!(read.board[1].task.workflow.as_deref(), Some("app"));
        assert_eq!(read.board[1].task.priority, 2);
        assert_eq!(read.board[1].body, "Prose here.");
        // createdAt is carried, not collapsed onto updatedAt.
        assert_ne!(
            read.board[1].task.created_at,
            read.board[1].task.updated_at
        );

        // The budget rides back for the index envelope.
        assert_eq!(read.remaining, Some(4989));
        let expected_reset = DateTime::parse_from_rfc3339("2026-09-08T01:00:00Z")
            .unwrap()
            .timestamp();
        assert_eq!(read.reset, Some(expected_reset));
    }

    #[test]
    fn refresh_board_incremental_removes_closed_and_upserts_open() {
        let _iso = IsolatedHome::new("delta");
        // Previous open board: a (todo), b (review).
        let previous = vec![prev_issue("a", "todo", 0), prev_issue("b", "review", 0)];

        // Delta since the last refresh: `b` was closed as completed (leaves the
        // open index), `a` moved to in-progress (upserted), and a new `c` opened.
        let json = r#"{"data":{"rateLimit":{"remaining":4999,"resetAt":"2026-09-08T02:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"z"},"nodes":[
{"number":2,"title":"b","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-09-08T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/b"},{"name":"shelbi:status/review"}]}},
{"number":1,"title":"a","state":"OPEN","stateReason":null,"createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-09-08T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/a"},{"name":"shelbi:status/in-progress"}]}},
{"number":9,"title":"c","state":"OPEN","stateReason":null,"createdAt":"2026-09-08T00:00:00Z","updatedAt":"2026-09-08T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/c"},{"name":"shelbi:status/todo"}]}}
]}}}}"#;
        let store = graphql_store(json);

        let since = Some(Utc::now());
        let read = store.refresh_board(since, &previous).unwrap();
        let ids: Vec<_> = read.board.iter().map(|f| f.task.id.clone()).collect();
        // b is gone (closed); a and c remain, in canonical order (todo < in-progress).
        assert_eq!(ids, vec!["c".to_string(), "a".to_string()]);
        // a's status was upserted from the delta.
        let a = read.board.iter().find(|f| f.task.id == "a").unwrap();
        assert_eq!(a.task.column, Column::in_progress());
        assert_eq!(read.remaining, Some(4999));
    }

    #[test]
    fn refresh_board_incremental_is_a_noop_on_a_quiet_board() {
        let _iso = IsolatedHome::new("quiet");
        let previous = vec![prev_issue("a", "todo", 0)];
        // Zero touched nodes — a quiet tick. The previous board carries forward
        // unchanged, and the single point still reports the budget.
        let json = r#"{"data":{"rateLimit":{"remaining":4998,"resetAt":"2026-09-08T03:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
        let store = graphql_store(json);

        let read = store.refresh_board(Some(Utc::now()), &previous).unwrap();
        assert_eq!(read.board.len(), 1);
        assert_eq!(read.board[0].task.id, "a");
        assert_eq!(read.remaining, Some(4998));
    }

    #[test]
    fn refresh_board_refolds_the_assignment_overlay_each_tick() {
        let _iso = IsolatedHome::new("overlay");
        // The overlay carries an owner for `a` that never bumps GitHub
        // `updatedAt`, so it can't ride in the delta — the tick must re-read it.
        crate::set_task_assignment("test-project", "a", Some("alpha")).unwrap();
        let previous = vec![prev_issue("a", "todo", 0)];
        // A quiet delta (no GitHub activity on `a`).
        let json = r#"{"data":{"rateLimit":{"remaining":5000,"resetAt":"2026-09-08T04:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
        let store = graphql_store(json);

        let read = store.refresh_board(Some(Utc::now()), &previous).unwrap();
        assert_eq!(read.board.len(), 1);
        assert_eq!(
            read.board[0].task.assigned_to.as_deref(),
            Some("alpha"),
            "the local assignment overlay is folded onto the board every tick"
        );
    }

    #[test]
    fn refresh_board_paginates_until_has_next_page_is_false() {
        let _iso = IsolatedHome::new("paginate");
        // Two pages: the first advertises a next page + cursor, the second ends
        // it. Both nodes must land in the board.
        let page1 = r#"{"data":{"rateLimit":{"remaining":4990,"resetAt":"2026-09-08T05:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":true,"endCursor":"CUR1"},"nodes":[
{"number":1,"title":"a","state":"OPEN","stateReason":null,"createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-08-01T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/a"},{"name":"shelbi:status/todo"}]}}
]}}}}"#;
        let page2 = r#"{"data":{"rateLimit":{"remaining":4989,"resetAt":"2026-09-08T05:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":"CUR2"},"nodes":[
{"number":2,"title":"b","state":"OPEN","stateReason":null,"createdAt":"2026-08-02T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/b"},{"name":"shelbi:status/todo"}]}}
]}}}}"#;
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let call_rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            let n = call_rec.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                // First page must not carry an `after` cursor.
                assert!(!args.iter().any(|a| a.starts_with("after=")), "{args:?}");
                Ok(page1.to_string())
            } else {
                // Second page follows the first page's endCursor.
                assert!(
                    args.contains(&"after=CUR1"),
                    "second page must page on CUR1: {args:?}"
                );
                Ok(page2.to_string())
            }
        });

        let read = store.refresh_board(None, &[]).unwrap();
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 2);
        let ids: Vec<_> = read.board.iter().map(|f| f.task.id.clone()).collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
        // The budget from the last page wins.
        assert_eq!(read.remaining, Some(4989));
    }

    #[test]
    fn refresh_board_falls_back_to_rest_on_a_graphql_errors_array() {
        let _iso = IsolatedHome::new("fallback-errors");
        // A non-rate-limit GraphQL failure (here an `errors` array — the shape a
        // GHES host or a token missing GraphQL scope returns) falls back to the
        // REST open list rather than failing the whole refresh. The runner
        // answers the GraphQL call with the errors array and the REST list with
        // one open issue.
        let store = GitHubStore::with_runner("owner/repo", |args| {
            if args.contains(&"graphql") {
                Ok(
                    r#"{"data":null,"errors":[{"message":"Something went wrong while fetching"}]}"#
                        .to_string(),
                )
            } else {
                Ok(r#"{"number":5,"title":"Rest one","state":"open","labels":[{"name":"shelbi:id/rest-one"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#.to_string())
            }
        });

        let read = store.refresh_board(None, &[]).unwrap();
        assert_eq!(read.board.len(), 1);
        assert_eq!(read.board[0].task.id, "rest-one");
        // A REST fallback carries no GraphQL budget.
        assert_eq!(read.remaining, None);
        assert_eq!(read.reset, None);
    }

    #[test]
    fn refresh_board_delta_falls_back_to_a_full_rest_open_read() {
        let _iso = IsolatedHome::new("fallback-delta");
        // A GHES host that doesn't support `filterBy.since` fails the delta query;
        // the fallback is a *full* REST open read that replaces the previous board
        // (the stale card is gone, the live one is in).
        let previous = vec![prev_issue("stale", "todo", 0)];
        let store = GitHubStore::with_runner("owner/repo", |args| {
            if args.contains(&"graphql") {
                Err(Error::Command {
                    cmd: "gh api graphql".into(),
                    status: "HTTP 400".into(),
                    stderr: "Field 'filterBy' doesn't exist on type 'IssueConnection'".into(),
                })
            } else {
                Ok(r#"{"number":1,"title":"a","state":"open","labels":[{"name":"shelbi:id/a"},{"name":"shelbi:status/todo"}],"created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z"}"#.to_string())
            }
        });

        let read = store.refresh_board(Some(Utc::now()), &previous).unwrap();
        let ids: Vec<_> = read.board.iter().map(|f| f.task.id.clone()).collect();
        assert_eq!(ids, vec!["a".to_string()], "the full REST read replaces the previous board");
        assert_eq!(read.remaining, None);
    }

    #[test]
    fn refresh_board_propagates_a_graphql_rate_limit_error() {
        let _iso = IsolatedHome::new("ratelimit");
        // A rate limit is NOT a fallback trigger: it propagates so the daemon
        // leaves the index untouched and the Phase 3 governor sees it — and the
        // REST path (a separate budget) is never touched.
        let rest_called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = rest_called.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            if args.contains(&"graphql") {
                Err(Error::Command {
                    cmd: "gh api graphql".into(),
                    status: "HTTP 403".into(),
                    stderr: "API rate limit exceeded; x-ratelimit-reset: 1800000000".into(),
                })
            } else {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Ok(String::new())
            }
        });

        let err = store.refresh_board(None, &[]).unwrap_err();
        assert!(
            crate::gh_retry::is_rate_limit_error(&err),
            "a rate limit propagates rather than falling back: {err}"
        );
        assert!(
            !rest_called.load(std::sync::atomic::Ordering::SeqCst),
            "the REST list must not be called on a rate limit"
        );
    }

    #[test]
    fn refresh_board_incremental_sends_the_since_variable() {
        let _iso = IsolatedHome::new("since-var");
        let since = DateTime::parse_from_rfc3339("2026-09-01T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let json = r#"{"data":{"rateLimit":{"remaining":5000,"resetAt":"2026-09-08T06:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let rec = seen.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            Ok(json.to_string())
        });
        store.refresh_board(Some(since), &[]).unwrap();
        let call = &seen.lock().unwrap()[0];
        assert!(
            call.contains("since=2026-09-01T12:00:00+00:00"),
            "the delta query carries the since watermark: {call}"
        );
    }

    #[test]
    fn closed_page_cold_maps_the_first_page_and_carries_the_next_cursor() {
        let _iso = IsolatedHome::new("closed-page");
        // One page of 50-limit closed history: a completed + a canceled issue,
        // and `hasNextPage` so a "load more" cursor is offered. The node shape is
        // the same as the board index (labels → id/status/column).
        let json = r#"{"data":{"rateLimit":{"remaining":4999,"resetAt":"2026-09-08T07:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":true,"endCursor":"PAGE2"},"nodes":[
{"number":40,"title":"shipped","state":"CLOSED","stateReason":"COMPLETED","createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-09-07T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/shipped"},{"name":"shelbi:status/done"}]}},
{"number":39,"title":"dropped","state":"CLOSED","stateReason":"NOT_PLANNED","createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-09-06T00:00:00Z","body":"","labels":{"nodes":[{"name":"shelbi:id/dropped"},{"name":"shelbi:status/canceled"}]}}
]}}}}"#;
        let store = graphql_store(json);

        let page = store.closed_page(None).unwrap();
        assert_eq!(page.issues.len(), 2);
        // Newest-updated first, as the query orders it.
        assert_eq!(page.issues[0].task.id, "shipped");
        assert_eq!(page.issues[0].task.column, Column::done());
        assert_eq!(page.issues[1].task.column, Column::canceled());
        // A next page is offered.
        assert_eq!(page.next_cursor.as_deref(), Some("PAGE2"));
        assert_eq!(page.remaining, Some(4999));
    }

    #[test]
    fn closed_page_sends_the_states_closed_query_and_a_load_more_cursor() {
        let _iso = IsolatedHome::new("closed-args");
        let json = r#"{"data":{"rateLimit":{"remaining":4998,"resetAt":"2026-09-08T07:00:00Z"},"repository":{"issues":{"pageInfo":{"hasNextPage":false,"endCursor":null},"nodes":[]}}}}"#;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let rec = seen.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            Ok(json.to_string())
        });

        // First page: the CLOSED query, no `after`.
        let page = store.closed_page(None).unwrap();
        assert!(page.next_cursor.is_none(), "last page offers no cursor");
        // Load more: the same query, this time carrying the caller's cursor.
        store.closed_page(Some("PAGE2")).unwrap();

        let calls = seen.lock().unwrap();
        assert!(
            calls[0].contains("BoardClosed") && calls[0].contains("states: [CLOSED]"),
            "the first page uses the GraphQL BoardClosed query: {}",
            calls[0]
        );
        assert!(
            !calls[0].contains("after="),
            "the first page carries no cursor: {}",
            calls[0]
        );
        assert!(
            calls[1].contains("after=PAGE2"),
            "a load-more page follows the caller's cursor: {}",
            calls[1]
        );
    }

    #[test]
    fn closed_page_falls_back_to_the_rest_closed_sweep_off_graphql() {
        let _iso = IsolatedHome::new("closed-fallback");
        // A non-rate-limit GraphQL failure falls back to the REST `state=closed`
        // list (newest-closed first), never failing the read.
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let rec = seen.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            if args.contains(&"graphql") {
                Ok(r#"{"data":null,"errors":[{"message":"Field 'filterBy' doesn't exist"}]}"#
                    .to_string())
            } else {
                Ok(r#"{"number":8,"title":"old","state":"closed","state_reason":"completed","labels":[{"name":"shelbi:id/old"},{"name":"shelbi:status/done"}],"created_at":"2026-07-01T00:00:00Z","updated_at":"2026-07-02T00:00:00Z"}"#.to_string())
            }
        });

        let page = store.closed_page(None).unwrap();
        assert_eq!(page.issues.len(), 1);
        assert_eq!(page.issues[0].task.id, "old");
        assert!(page.next_cursor.is_none(), "the REST fallback is one page");
        let calls = seen.lock().unwrap();
        assert!(
            calls.iter().any(|c| c.contains("state=closed")),
            "the fallback sweeps state=closed: {calls:?}"
        );
    }

    #[test]
    fn closed_page_propagates_a_graphql_rate_limit_error() {
        let _iso = IsolatedHome::new("closed-ratelimit");
        // A rate limit is terminal — it must propagate so the caller leaves the
        // cached page untouched, never hammering REST as a fallback.
        let store = GitHubStore::with_runner("owner/repo", |_args| {
            Err(Error::Command {
                cmd: "gh api graphql".into(),
                status: "HTTP 403".into(),
                stderr: "API rate limit exceeded".into(),
            })
        });
        let err = store.closed_page(None).unwrap_err();
        assert!(
            crate::gh_retry::is_rate_limit_error(&err),
            "a rate limit propagates: {err:?}"
        );
    }

    #[test]
    fn gh_error_detail_includes_the_response_body_where_the_422_lives() {
        // `gh api` puts the field-level reason in the JSON body on stdout; a
        // stderr-only capture used to drop it. The combined detail keeps both.
        let stderr = "gh: Validation Failed (HTTP 422)";
        let stdout = r#"{"message":"Validation Failed","errors":[{"resource":"Label","field":"name","message":"name is too long (maximum is 50 characters)"}],"status":"422"}"#;
        let detail = combine_gh_error_detail(stderr, stdout);
        assert!(detail.contains("Validation Failed (HTTP 422)"));
        assert!(detail.contains("name is too long (maximum is 50 characters)"));

        // Body-only (gh wrote nothing to stderr) still surfaces the reason.
        let body_only = combine_gh_error_detail("", stdout);
        assert!(body_only.contains("name is too long"));

        // Stderr-only (a non-`api` failure) is preserved verbatim.
        assert_eq!(
            combine_gh_error_detail("could not resolve host", ""),
            "could not resolve host"
        );
    }

    // --- fresh single-issue fetch (plan §3) ----------------------------------

    /// An isolated `SHELBI_HOME` (so a test's board-index writes and assignment
    /// overlay reads land in a temp dir, never the real `~/.shelbi`) that also
    /// resets the process-global issue caches. Holds the crate test lock because
    /// `set_var` and the caches are process-global.
    struct HomeGuard {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        home: std::path::PathBuf,
    }
    impl HomeGuard {
        fn new(tag: &str) -> Self {
            let lock = crate::test_lock::LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-gh-fetch-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
            let prev = std::env::var("SHELBI_HOME").ok();
            std::env::set_var("SHELBI_HOME", &home);
            clear_issue_caches_for_test();
            Self {
                _lock: lock,
                prev,
                home,
            }
        }
    }
    impl Drop for HomeGuard {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    /// A store whose runner records every call and answers from `f`.
    fn graphql_recorder<F>(f: F) -> (GitHubStore, Calls)
    where
        F: Fn(&[&str]) -> Result<String> + Send + Sync + 'static,
    {
        let calls: Calls = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let rec = calls.clone();
        let store = GitHubStore::with_runner("owner/repo", move |args| {
            rec.lock().unwrap().push(args.join(" "));
            f(args)
        });
        (store, calls)
    }

    /// The recorded `gh api graphql` calls.
    fn graphql_calls(calls: &Calls) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.contains("graphql"))
            .cloned()
            .collect()
    }

    /// One GraphQL issue node (the shape [`GhIssueNode`] parses). `reason` empty
    /// → `null`.
    fn gql_node(
        number: i64,
        id: &str,
        status: &str,
        state: &str,
        reason: &str,
        body: &str,
        updated: &str,
    ) -> String {
        let reason_json = if reason.is_empty() {
            "null".to_string()
        } else {
            format!("\"{reason}\"")
        };
        format!(
            r#"{{"number":{number},"title":"T {id}","state":"{state}","stateReason":{reason_json},"createdAt":"2026-01-01T00:00:00Z","updatedAt":"{updated}","body":"{body}","labels":{{"nodes":[{{"name":"shelbi:id/{id}"}},{{"name":"shelbi:status/{status}"}}]}}}}"#
        )
    }

    fn gql_single(node: &str) -> String {
        format!(
            r#"{{"data":{{"rateLimit":{{"remaining":4999,"resetAt":"2026-01-01T01:00:00Z"}},"repository":{{"issue":{node}}}}}}}"#
        )
    }

    fn gql_search(number: i64) -> String {
        format!(
            r#"{{"data":{{"rateLimit":{{"remaining":4999}},"search":{{"nodes":[{{"number":{number}}}]}}}}}}"#
        )
    }

    fn gql_aliased(nodes: &[(usize, &str)]) -> String {
        let inner: Vec<String> = nodes.iter().map(|(k, n)| format!("\"i{k}\":{n}")).collect();
        format!(
            r#"{{"data":{{"rateLimit":{{"remaining":4999}},"repository":{{{}}}}}}}"#,
            inner.join(",")
        )
    }

    fn idx_issue(id: &str, column: &str, updated: &str) -> IssueFile {
        let task: shelbi_core::Issue = serde_yaml::from_str(&format!(
            "id: {id}\ntitle: {id}\ncolumn: {column}\npriority: 0\n\
             created_at: 2026-01-01T00:00:00Z\nupdated_at: {updated}\n"
        ))
        .expect("issue fixture parses");
        IssueFile {
            task,
            body: String::new(),
        }
    }

    /// Publish a board index for `project` with an explicit id→number map.
    fn write_test_index(project: &str, numbers: &[(&str, i64)], board: Vec<IssueFile>) {
        let idx = crate::board_index::BoardIndex::fresh_at(
            board,
            numbers.iter().map(|(id, n)| (id.to_string(), *n)).collect(),
            Utc::now().to_rfc3339(),
            None,
            None,
        );
        crate::board_index::write_board_index(project, &idx).unwrap();
    }

    #[test]
    fn fetch_reads_one_issue_via_a_single_graphql_request() {
        let _home = HomeGuard::new("fetch");
        let node = gql_node(7, "foo", "in-progress", "OPEN", "", "Fresh body", "2026-08-02T00:00:00Z");
        let (store, calls) = graphql_recorder(move |_args| Ok(gql_single(&node)));

        let tf = store.fetch(7).unwrap().expect("issue exists");
        assert_eq!(tf.task.id, "foo");
        assert_eq!(tf.task.column, Column::in_progress());
        assert_eq!(tf.body, "Fresh body");
        assert_eq!(graphql_calls(&calls).len(), 1, "one single-issue request");
    }

    #[test]
    fn fetch_returns_none_for_a_number_that_is_not_an_issue() {
        let _home = HomeGuard::new("fetch-none");
        let (store, _calls) = graphql_recorder(|_args| {
            Ok(r#"{"data":{"repository":{"issue":null}}}"#.to_string())
        });
        assert!(store.fetch(404).unwrap().is_none());
    }

    #[test]
    fn get_resolves_through_the_index_and_reads_the_edit_in_one_request() {
        // Acceptance: `issue show <id>` after an out-of-band edit prints the
        // edit, with one GraphQL request — the index gives the number, and the
        // single-issue fetch reads the live body/status.
        let _home = HomeGuard::new("getidx");
        write_test_index(
            "test-project",
            &[("foo", 7)],
            vec![idx_issue("foo", "todo", "2026-08-01T00:00:00Z")],
        );
        let node = gql_node(7, "foo", "in-progress", "OPEN", "", "Edited on GitHub", "2026-08-03T00:00:00Z");
        let (store, calls) = graphql_recorder(move |args| {
            let joined = args.join(" ");
            assert!(joined.contains("query Issue"), "expected the single-issue query: {joined}");
            Ok(gql_single(&node))
        });

        let tf = store.get("foo").unwrap().expect("issue exists");
        assert_eq!(tf.body, "Edited on GitHub");
        assert_eq!(tf.task.column, Column::in_progress());
        assert_eq!(
            graphql_calls(&calls).len(),
            1,
            "the index carried the number, so no search — exactly one request"
        );
    }

    #[test]
    fn get_falls_back_to_search_for_a_done_task_not_in_the_index() {
        // Acceptance: `get` for a done task (absent from the open index) succeeds
        // via the search fallback.
        let _home = HomeGuard::new("getsearch");
        let node = gql_node(42, "done-task", "done", "CLOSED", "completed", "done body", "2026-07-02T00:00:00Z");
        let (store, calls) = graphql_recorder(move |args| {
            if args.join(" ").contains("IdSearch") {
                Ok(gql_search(42))
            } else {
                Ok(gql_single(&node))
            }
        });

        let tf = store.get("done-task").unwrap().expect("found via search");
        assert_eq!(tf.task.id, "done-task");
        assert_eq!(tf.task.column, Column::done());
        let g = graphql_calls(&calls);
        assert_eq!(g.len(), 2, "one search + one fetch");
        assert!(g[0].contains("IdSearch"), "the first request is the search: {}", g[0]);
    }

    #[test]
    fn fetch_many_resolves_several_ids_in_one_aliased_request() {
        // Acceptance: several ids resolve in one aliased request. The index
        // carries the numbers, so no per-id search precedes the batch.
        let _home = HomeGuard::new("many");
        write_test_index(
            "test-project",
            &[("a", 1), ("b", 2)],
            vec![
                idx_issue("a", "todo", "2026-01-01T00:00:00Z"),
                idx_issue("b", "review", "2026-01-01T00:00:00Z"),
            ],
        );
        let na = gql_node(1, "a", "todo", "OPEN", "", "body a", "2026-01-01T00:00:00Z");
        let nb = gql_node(2, "b", "review", "OPEN", "", "body b", "2026-01-01T00:00:00Z");
        let (store, calls) = graphql_recorder(move |args| {
            assert!(
                args.join(" ").contains("IssuesByNumber"),
                "expected the aliased query"
            );
            Ok(gql_aliased(&[(0, &na), (1, &nb)]))
        });

        let got = store.fetch_many(&["a", "b"]).unwrap();
        let ids: std::collections::HashSet<_> = got.iter().map(|t| t.task.id.clone()).collect();
        assert_eq!(got.len(), 2);
        assert!(ids.contains("a") && ids.contains("b"));
        assert_eq!(
            graphql_calls(&calls).len(),
            1,
            "one aliased request; the index resolved both numbers"
        );
    }

    #[test]
    fn fetch_serves_the_cache_until_the_index_shows_a_newer_updated_at() {
        // Acceptance: the per-number cache is invalidated by a newer index
        // `updatedAt` for that number.
        let _home = HomeGuard::new("cacheinv");
        let t1 = "2026-08-01T00:00:00Z";
        let t2 = "2026-08-05T00:00:00Z";
        write_test_index("test-project", &[("foo", 5)], vec![idx_issue("foo", "todo", t1)]);
        let node = gql_node(5, "foo", "todo", "OPEN", "", "body", t1);
        let (store, calls) = graphql_recorder(move |_a| Ok(gql_single(&node)));

        // Cold: a live read that caches the issue at updatedAt T1.
        store.fetch(5).unwrap().expect("issue exists");
        assert_eq!(graphql_calls(&calls).len(), 1);

        // The index still shows T1 (not newer than the cache) — served from cache.
        store.fetch(5).unwrap().expect("issue exists");
        assert_eq!(
            graphql_calls(&calls).len(),
            1,
            "an unchanged index serves the cached issue"
        );

        // Bump the index's updatedAt for this issue — the cache is now invalid.
        write_test_index("test-project", &[("foo", 5)], vec![idx_issue("foo", "todo", t2)]);
        store.fetch(5).unwrap().expect("issue exists");
        assert_eq!(
            graphql_calls(&calls).len(),
            2,
            "a newer index updatedAt forces a live re-read"
        );
    }

    #[test]
    fn add_records_the_created_number_so_get_resolves_without_a_search() {
        // Acceptance: a task created seconds ago resolves via the create-returned
        // number, never the eventually-consistent label search.
        let _home = HomeGuard::new("add");
        // A published (empty) index for `record_board_index_number` to patch.
        write_test_index("test-project", &[], vec![]);
        let (store, calls) = graphql_recorder(move |args| {
            let joined = args.join(" ");
            let method = args
                .iter()
                .position(|a| *a == "-X")
                .and_then(|i| args.get(i + 1))
                .copied()
                .unwrap_or("GET");
            if method == "POST" && joined.contains("/issues") && !joined.contains("/labels") {
                return Ok(r#"{"number":99,"title":"New card","state":"open","created_at":"2026-08-03T00:00:00Z","updated_at":"2026-08-03T00:00:00Z","labels":[]}"#.to_string());
            }
            if method == "POST" && joined.contains("/labels") {
                return Ok("{}".to_string());
            }
            // Every GET (dup check, column list, label list) is empty.
            Ok(String::new())
        });

        let created = store.add(NewIssue::new("newcard", "New card", Column::todo(), "Body")).unwrap();
        assert_eq!(created.id, "newcard");

        // The create-returned number is remembered in the process cache and
        // written into the published index's id→number map.
        assert_eq!(store.cached_number("newcard"), Some(99));
        let idx = crate::board_index::read_board_index("test-project").expect("index present");
        assert_eq!(idx.numbers.get("newcard"), Some(&99));

        // Resolving the id now takes the create number — no GraphQL search.
        assert_eq!(store.resolve_number("newcard").unwrap(), Some(99));
        assert!(
            graphql_calls(&calls).is_empty(),
            "a just-created card resolves without a label search"
        );
    }
}
