//! The Linear backend for the [`IssueStore`] seam.
//!
//! A project whose `issue_tracker.backend` is `linear` keeps its board as
//! issues in one Linear team, narrowed by the project's filters
//! ([`LinearConnection`]: team required; project, label and assignee
//! optional). It follows the GitHub backend's contract
//! ([`crate::github_store`]) with Linear's own mechanisms:
//!
//! * **Visible set.** Every read is `issues(filter: …)` with the team filter
//!   and each set optional filter, so the board (and every Shelbi agent) sees
//!   exactly the matching issues, done and canceled history included
//!   ([`IssueStore::list`]). `list_open` / `list_closed` add a state-type
//!   filter; `closed_page` reads 50 at a time, newest-updated first.
//! * **Creates apply the filters.** `issueCreate` sets the team, the
//!   project, the label and the assignee filters; with no assignee filter the
//!   issue is assigned to the API key's owner, so it is the user's for the
//!   Zen ownership gate (the parity of GitHub's auto-assign).
//! * **Status.** Each issue's workflow state maps to a Shelbi status in one
//!   function, [`mapping::column_for_state`]: through the synced status
//!   catalog's `external_id` (the state uuid, [`crate::linear_statuses`])
//!   when there is one; otherwise `completed` → `done`, `canceled` →
//!   `canceled`, anything else by slugged name (`In Review` → `in-review`).
//!   A move writes the team state that maps to the target
//!   ([`mapping::state_for_column`]). Open vs. history follows Linear's state
//!   type, so a synced `shipped` or `duplicate` status is still history.
//! * **Board order** is Linear's own `sortOrder` (lower first): a card's
//!   `priority` is its rank within its status, and `set_priority` writes a
//!   `sortOrder` between its new neighbours. `renumber` has nothing to do.
//! * **Shelbi-only fields** (workflow, branch, depends_on, pr, zen, launch,
//!   params, and the Shelbi id) live in one attachment per issue at
//!   `https://shelbi.dev/i/<team-key>/<shelbi-id>`, in its `metadata.shelbi`
//!   ([`mapping::LinearMeta`]). Linear upserts an attachment by URL, so a
//!   metadata write is one idempotent `attachmentCreate`. The description
//!   stays entirely human prose.
//! * **Ids.** A Shelbi id is the one in the attachment, else the lowercased
//!   identifier (`ENG-123` → `eng-123`). `get(id)` resolves the team-scoped
//!   issue number from a process cache, the published board index's
//!   `numbers`, or the id itself, and reads that one issue; any other id is
//!   found by its attachment URL. Both reads keep the visible filters, so an
//!   issue outside them is absent.
//! * **Ownership.** `tracker_assignees` holds the assignee's display name
//!   (unique within a Linear workspace) and [`IssueStore::viewer_login`] the
//!   API key owner's, so [`crate::ownership`] works unchanged; `claim`
//!   assigns the viewer (Linear issues have one assignee, so it replaces).
//! * **Workspace assignment** (`assigned_to`) stays in the local overlay,
//!   exactly as on GitHub.
//! * **Changes.** `refresh_board` reads the whole open board cold, and on a
//!   warm tick only the team's issues updated since the watermark (archived
//!   ones included), dropping any that closed, were archived or left the
//!   filters. `poll_changes` reports issues updated past its cursor (so a card
//!   moved in Linear becomes the poller's normal `from -> to` event) and
//!   comments created past it.
//! * **Writes with an unknown outcome.** Issues and comments are created with
//!   a client-chosen uuid, so a create that timed out or half-applied is
//!   settled by one read of that id: if it landed it is finished (its
//!   attachment written if missing), otherwise the typed
//!   [`Error::UnknownWriteOutcome`] names the recovery.
//!
//! The client ([`crate::linear_client`]) owns auth, retries, the rate-limit
//! budget and the request log. A rate limit surfaces from every method as an
//! error naming when calls resume.

mod mapping;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use shelbi_core::{
    Column, Error, Issue, LinearConnection, ProjectStatuses, Result, TrackerName,
    DEFAULT_WORKFLOW_NAME,
};

use crate::issue_store::{
    is_terminal_column as is_terminal, BoardRead, ClosedPage, Cursor, IssueChange, IssueComment,
    IssueFields, IssueStore, NewIssue, PrioMove, StatusMove,
};
use crate::linear_client::{
    merge_attachment, resolve_filters_cached, IssueOrder, IssueQuery, LinearClient, LinearError,
    LinearIssue, LinearState, LinearUser, ResolvedLinearFilters, TransportFailure,
};
use crate::IssueFile;

use mapping::{
    attachment_filter, attachment_input, column_for_state, is_closed_state, matches_filters,
    new_uuid,
    number_from_id, read_meta, shelbi_id, sort_between, state_for_column, state_ids_for_column,
    strictly_between, team_filter, terminal_filter, user_token, visible_filter, with, LinearMeta,
};

/// How long a team's workflow states are reused before being read again.
const STATES_TTL: Duration = Duration::from_secs(600);

/// Issues per `closed_page`.
const CLOSED_PAGE_SIZE: u32 = 50;

/// The Linear issues backend. A cheap handle: it holds the project name and
/// its filters, and resolves the API key, the filter ids and the team states
/// per call (each behind a cache).
#[derive(Clone)]
pub struct LinearStore {
    project: String,
    conn: LinearConnection,
    /// An injected client (tests); `None` builds one per call from the
    /// project's key (or the installed fake).
    client: Option<LinearClient>,
}

impl std::fmt::Debug for LinearStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LinearStore")
            .field("project", &self.project)
            .field("team", &self.conn.team)
            .finish_non_exhaustive()
    }
}

/// One call's resolved context.
struct Ctx {
    client: LinearClient,
    filters: ResolvedLinearFilters,
    /// The project's status catalog when it is synced from Linear (each
    /// status carries its state's uuid); `None` maps by state name.
    catalog: Option<ProjectStatuses>,
}

/// A mapped issue plus what the store needs beside it.
struct Row {
    tf: IssueFile,
    /// Closed in Linear (a completed or canceled state): done history, not
    /// the open board, whatever the status id.
    closed: bool,
    uuid: String,
    number: i64,
    sort: f64,
}

impl LinearStore {
    /// A store for `project` over its Linear filters, using the project's API
    /// key (env → keychain → `tokens.yml`), or the fake a test installed.
    pub fn new(project: impl Into<String>, conn: LinearConnection) -> Self {
        // A test that installed the fake builds a fresh store per scenario;
        // start it with clean process caches (they outlive any one test).
        #[cfg(any(test, feature = "test-support"))]
        if crate::linear_client::test_runner_installed() {
            clear_linear_caches_for_test();
        }
        LinearStore {
            project: project.into(),
            conn,
            client: None,
        }
    }

    /// A store over an explicit client.
    pub fn with_client(
        project: impl Into<String>,
        conn: LinearConnection,
        client: LinearClient,
    ) -> Self {
        #[cfg(any(test, feature = "test-support"))]
        clear_linear_caches_for_test();
        LinearStore {
            project: project.into(),
            conn,
            client: Some(client),
        }
    }

    /// The project's filters as configured.
    pub fn connection(&self) -> &LinearConnection {
        &self.conn
    }

    fn client(&self) -> Result<LinearClient> {
        match &self.client {
            Some(c) => Ok(c.clone()),
            None => Ok(LinearClient::for_project(Some(&self.project))?.caller("store")),
        }
    }

    fn ctx(&self) -> Result<Ctx> {
        let client = self.client()?;
        let filters = resolve_filters_cached(&client, &self.project, &self.conn)?;
        let catalog = crate::load_project_statuses(&self.project)
            .ok()
            .filter(|c| c.managed_by == Some(TrackerName::Linear));
        Ok(Ctx { client, filters, catalog })
    }

    /// The board-index identity this store's index carries.
    fn identity(&self) -> String {
        crate::board_index::linear_board_repo(&self.conn)
    }

    // --- caches ---------------------------------------------------------------

    /// The team's workflow states, from the process cache when fresh.
    fn states(&self, ctx: &Ctx, fresh: bool) -> Result<Vec<LinearState>> {
        let team = &ctx.filters.team_id;
        if !fresh {
            if let Some((at, states)) = lock(states_cache()).get(team) {
                if at.elapsed() < STATES_TTL {
                    return Ok(states.clone());
                }
            }
        }
        let states = ctx.client.team_states(team)?;
        lock(states_cache()).insert(team.clone(), (Instant::now(), states.clone()));
        Ok(states)
    }

    /// The state a move to `col` writes, re-reading the team's states once
    /// when the cached list has none (a state added in Linear since).
    fn target_state(&self, ctx: &Ctx, col: &Column) -> Result<LinearState> {
        for fresh in [false, true] {
            let states = self.states(ctx, fresh)?;
            if let Some(s) = state_for_column(&states, col, ctx.catalog.as_ref()) {
                return Ok(s.clone());
            }
        }
        Err(Error::Other(format!(
            "no Linear workflow state in team {} maps to status `{}`; add a state named `{}` \
             to the team (or rename one)",
            ctx.filters.team_key,
            col.as_str(),
            col.default_status_name(),
        )))
    }

    /// The API key's owner, cached per key for the process.
    fn viewer(&self, ctx: &Ctx) -> Result<LinearUser> {
        let key = ctx.client.budget_key().to_string();
        if let Some(v) = lock(viewer_cache()).get(&key) {
            return Ok(v.clone());
        }
        let viewer = ctx.client.viewer()?;
        lock(viewer_cache()).insert(key, viewer.clone());
        Ok(viewer)
    }

    fn remember(&self, ctx: &Ctx, row: &Row) {
        let key = (ctx.filters.team_id.clone(), row.tf.task.id.clone());
        lock(number_cache()).insert(key.clone(), row.number);
        lock(sort_cache()).insert(key, row.sort);
    }

    fn cached_number(&self, ctx: &Ctx, id: &str) -> Option<i64> {
        lock(number_cache())
            .get(&(ctx.filters.team_id.clone(), id.to_string()))
            .copied()
    }

    fn cached_sort(&self, ctx: &Ctx, id: &str) -> Option<f64> {
        lock(sort_cache())
            .get(&(ctx.filters.team_id.clone(), id.to_string()))
            .copied()
    }

    fn valid_index(&self) -> Option<crate::BoardIndex> {
        crate::board_index::read_valid_board_index(&self.project, Some(&self.identity()))
    }

    // --- mapping --------------------------------------------------------------

    /// Map one issue, with `priority` as its rank (the caller ranks).
    fn row(&self, ctx: &Ctx, issue: LinearIssue, priority: u32) -> Row {
        let meta = match read_meta(&issue) {
            Ok(m) => m,
            Err(detail) => {
                tracing::warn!(
                    project = %self.project,
                    issue = %issue.identifier,
                    %detail,
                    "Linear issue's Shelbi attachment metadata doesn't parse; reading it \
                     without Shelbi fields"
                );
                LinearMeta::default()
            }
        };
        let id = shelbi_id(&issue, &meta);
        let task = Issue {
            id,
            title: issue.title.clone(),
            column: column_for_state(&issue.state, ctx.catalog.as_ref()),
            priority,
            assigned_to: None,
            workflow: meta.workflow,
            branch: meta.branch,
            pr: meta.pr,
            depends_on: meta.depends_on,
            prefers_machine: meta.prefers_machine,
            zen: meta.zen,
            launch: meta.launch,
            created_at: issue.created_at,
            updated_at: issue.updated_at,
            params: meta.params,
        };
        Row {
            tf: IssueFile {
                task,
                body: issue.description.clone().unwrap_or_default(),
                tracker_assignees: issue.assignee.iter().map(user_token).collect(),
            },
            closed: is_closed_state(&issue.state),
            uuid: issue.id.clone(),
            number: issue.number_i64(),
            sort: issue.sort_order,
        }
    }

    /// Map a set of issues, ranking each within its status by `sortOrder`.
    fn rows(&self, ctx: &Ctx, issues: Vec<LinearIssue>) -> Vec<Row> {
        let mut rows: Vec<Row> = issues.into_iter().map(|i| self.row(ctx, i, 0)).collect();
        rank(&mut rows);
        for row in &rows {
            self.remember(ctx, row);
        }
        rows
    }

    /// One issue mapped outside a full read: its rank comes from where its
    /// `sortOrder` falls among the published index's cards in its status.
    fn single_row(&self, ctx: &Ctx, issue: LinearIssue) -> Row {
        let mut row = self.row(ctx, issue, 0);
        row.tf.task.priority = self
            .valid_index()
            .map(|idx| {
                idx.board
                    .iter()
                    .filter(|tf| tf.task.column == row.tf.task.column && tf.task.id != row.tf.task.id)
                    .filter(|tf| self.cached_sort(ctx, &tf.task.id).is_some_and(|s| s < row.sort))
                    .count() as u32
            })
            .unwrap_or(0);
        self.remember(ctx, &row);
        row
    }

    fn with_overlay(&self, mut tf: IssueFile) -> Result<IssueFile> {
        tf.task.assigned_to = crate::get_task_assignment(&self.project, &tf.task.id)?;
        Ok(tf)
    }

    fn workflow_name(&self, tf: &IssueFile) -> String {
        match &tf.task.workflow {
            Some(w) => w.clone(),
            None => crate::load_project(&self.project)
                .map(|p| p.default_workflow_name().to_string())
                .unwrap_or_else(|_| DEFAULT_WORKFLOW_NAME.to_string()),
        }
    }

    // --- reads ----------------------------------------------------------------

    fn read(&self, ctx: &Ctx, extra: Value) -> Result<Vec<Row>> {
        let filter = with(visible_filter(&ctx.filters), extra);
        let issues = ctx.client.issues(&IssueQuery::new(filter))?;
        Ok(self.rows(ctx, issues))
    }

    fn board(&self, ctx: &Ctx, extra: Value) -> Result<Vec<IssueFile>> {
        let assignments = crate::task_assignments(&self.project)?;
        Ok(finish(self.read(ctx, extra)?, &assignments).0)
    }

    /// The visible issues matching `extra` (one page is plenty for a lookup).
    fn lookup(&self, ctx: &Ctx, extra: Value) -> Result<Vec<LinearIssue>> {
        let filter = with(visible_filter(&ctx.filters), extra);
        Ok(ctx.client.issues_page(&IssueQuery::new(filter), 10, None)?.nodes)
    }

    /// Resolve a Shelbi id to its Linear issue (visible filters applied).
    fn get_raw(&self, ctx: &Ctx, id: &str) -> Result<Option<LinearIssue>> {
        shelbi_core::validate_task_id(id)?;
        let mut numbers: Vec<i64> = Vec::new();
        let index_number = || self.valid_index().and_then(|i| i.numbers.get(id).copied());
        for n in [
            self.cached_number(ctx, id),
            index_number(),
            number_from_id(id, &ctx.filters.team_key),
        ]
        .into_iter()
        .flatten()
        {
            if !numbers.contains(&n) {
                numbers.push(n);
            }
        }
        for n in numbers {
            for issue in self.lookup(ctx, json!({ "number": { "eq": n } }))? {
                if self.id_of(&issue) == id {
                    return Ok(Some(issue));
                }
            }
        }
        Ok(self
            .lookup(ctx, attachment_filter(id))?
            .into_iter()
            .find(|issue| self.id_of(issue) == id))
    }

    fn id_of(&self, issue: &LinearIssue) -> String {
        shelbi_id(issue, &read_meta(issue).unwrap_or_default())
    }

    fn require(&self, ctx: &Ctx, id: &str) -> Result<LinearIssue> {
        self.get_raw(ctx, id)?.ok_or_else(|| {
            Error::Other(format!(
                "issue `{id}` not found in Linear team {} (or outside this project's filters)",
                ctx.filters.team_key
            ))
        })
    }

    /// An issue by its uuid, team-scoped but without the optional filters
    /// (the reconcile read after an unknown-outcome create).
    fn by_uuid(&self, ctx: &Ctx, uuid: &str) -> Result<Option<LinearIssue>> {
        let filter = with(team_filter(&ctx.filters), json!({ "id": { "eq": uuid } }));
        Ok(ctx
            .client
            .issues_page(&IssueQuery::new(filter), 1, None)?
            .nodes
            .into_iter()
            .next())
    }

    /// The cards in `col`, ordered (ranked) by `sortOrder`, as `(id, uuid,
    /// sort)`.
    fn column_order(&self, ctx: &Ctx, col: &Column) -> Result<Vec<(String, String, f64)>> {
        let state_ids = state_ids_for_column(&self.states(ctx, false)?, col, ctx.catalog.as_ref());
        if state_ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = self.read(ctx, json!({ "state": { "id": { "in": state_ids } } }))?;
        Ok(rows
            .into_iter()
            .map(|r| (r.tf.task.id, r.uuid, r.sort))
            .collect())
    }

    // --- writes ---------------------------------------------------------------

    /// Publish a just-written issue to the board index / done history (the
    /// GitHub backend's write-through), best-effort.
    fn publish(&self, row: &Row) -> Result<IssueFile> {
        let tf = self.with_overlay(row.tf.clone())?;
        if row.closed {
            let _ = crate::board_index::remove_board_index_issue(&self.project, &tf.task.id);
            let _ = crate::done_history::patch_done_history_issue(&self.project, &tf);
        } else {
            let _ = crate::board_index::patch_board_index_issue_with_number(
                &self.project,
                &tf,
                Some(row.number),
            );
            let _ = crate::done_history::remove_done_history_issue(&self.project, &tf.task.id);
        }
        Ok(tf)
    }

    /// The `assigned_to`-only publish (see `GitHubStore::set_fields`).
    fn publish_assignment_overlay(&self, id: &str) -> Result<()> {
        let Some(idx) = crate::board_index::read_board_index(&self.project) else {
            return Ok(());
        };
        let Some(entry) = idx.board.iter().find(|f| f.task.id == id) else {
            return Ok(());
        };
        let tf = self.with_overlay(entry.clone())?;
        let _ = crate::board_index::patch_board_index_issue(&self.project, &tf);
        Ok(())
    }

    fn update(&self, ctx: &Ctx, uuid: &str, input: Value) -> Result<LinearIssue> {
        Ok(ctx.client.update_issue(uuid, input)?)
    }

    /// Write `meta` into the issue's Shelbi attachment and fold it onto
    /// `issue`.
    fn write_meta(&self, ctx: &Ctx, issue: &mut LinearIssue, id: &str, meta: &LinearMeta) -> Result<()> {
        let input = attachment_input(&issue.id, &ctx.filters.team_key, id, meta)
            .map_err(|detail| Error::MalformedIssueMetadata { id: id.to_string(), detail })?;
        let att = ctx.client.upsert_attachment(input)?;
        merge_attachment(issue, att);
        Ok(())
    }

    fn meta_for_write(&self, issue: &LinearIssue, id: &str) -> Result<LinearMeta> {
        read_meta(issue).map_err(|detail| Error::MalformedIssueMetadata {
            id: id.to_string(),
            detail,
        })
    }

    /// The sortOrder that puts a card at `position` (`None` = the end) of
    /// `col`, given its current order (without the card).
    fn sort_for_position(order: &[(String, String, f64)], position: Option<usize>) -> f64 {
        let at = position.unwrap_or(order.len()).min(order.len());
        let before = at.checked_sub(1).map(|i| order[i].2);
        let after = order.get(at).map(|o| o.2);
        sort_between(before, after)
    }

    fn move_inner(&self, ctx: &Ctx, id: &str, to: &Column) -> Result<Option<StatusMove>> {
        let issue = self.require(ctx, id)?;
        let from = column_for_state(&issue.state, ctx.catalog.as_ref());
        if from == *to {
            return Ok(None);
        }
        let workflow = self.workflow_name(&self.row(ctx, issue.clone(), 0).tf);
        let state = self.target_state(ctx, to)?;
        let after = self.update(ctx, &issue.id, json!({ "stateId": state.id }))?;
        let applied = column_for_state(&after.state, ctx.catalog.as_ref());
        if applied != *to {
            return Err(Error::Other(format!(
                "issue `{id}` in Linear: move did not apply; its state is `{}` (status `{}`), \
                 expected status `{}`",
                after.state.name,
                applied.as_str(),
                to.as_str()
            )));
        }
        let row = self.single_row(ctx, after);
        self.publish(&row)?;
        Ok(Some(StatusMove {
            from,
            to: to.clone(),
            workflow,
        }))
    }
}

impl IssueStore for LinearStore {
    fn list(&self) -> Result<Vec<IssueFile>> {
        let ctx = self.ctx()?;
        self.board(&ctx, Value::Null)
    }

    fn list_open(&self) -> Result<Vec<IssueFile>> {
        let ctx = self.ctx()?;
        self.board(&ctx, terminal_filter(false))
    }

    fn list_closed(&self) -> Result<Vec<IssueFile>> {
        let ctx = self.ctx()?;
        self.board(&ctx, terminal_filter(true))
    }

    fn closed_page(&self, after: Option<&str>) -> Result<ClosedPage> {
        let ctx = self.ctx()?;
        let filter = with(visible_filter(&ctx.filters), terminal_filter(true));
        let page = ctx.client.issues_page(
            &IssueQuery::new(filter).order(IssueOrder::UpdatedAt),
            CLOSED_PAGE_SIZE,
            after,
        )?;
        let assignments = crate::task_assignments(&self.project)?;
        let mut rows = self.rows(&ctx, page.nodes);
        // Newest-updated first, as the page was read.
        rows.sort_by_key(|r| std::cmp::Reverse(r.tf.task.updated_at));
        let budget = ctx.client.budget();
        Ok(ClosedPage {
            issues: rows.into_iter().map(|r| fold(r.tf, &assignments)).collect(),
            next_cursor: page.page_info.end_cursor,
            remaining: budget.requests.remaining,
            reset: budget.requests.reset_at,
        })
    }

    fn refresh_board(
        &self,
        since: Option<DateTime<Utc>>,
        previous: &[IssueFile],
    ) -> Result<BoardRead> {
        let ctx = self.ctx()?;
        let viewer_login = self.viewer(&ctx).ok().map(|v| user_token(&v));
        let budget = |client: &LinearClient| {
            let b = client.budget();
            (b.requests.remaining, b.requests.reset_at)
        };
        let assignments = crate::task_assignments(&self.project)?;
        let cold = |ctx: &Ctx| -> Result<BoardRead> {
            let (board, numbers) = finish(self.read(ctx, terminal_filter(false))?, &assignments);
            let (remaining, reset) = budget(&ctx.client);
            Ok(BoardRead {
                board,
                numbers,
                remaining,
                reset,
                rest_fallback: false,
                reopened: Vec::new(),
                viewer_login: viewer_login.clone(),
            })
        };
        let Some(since) = since else {
            return cold(&ctx);
        };

        // Warm tick: every team issue touched since the watermark, archived
        // ones included, so a card that closed, was archived, or left the
        // filters leaves the board.
        let filter = with(
            team_filter(&ctx.filters),
            json!({ "updatedAt": { "gt": since.to_rfc3339() } }),
        );
        let touched = ctx.client.issues(
            &IssueQuery::new(filter)
                .order(IssueOrder::UpdatedAt)
                .include_archived(true),
        )?;
        // Ranking the merged board needs every card's sortOrder; a process
        // that hasn't seen one (a daemon restart) reads the board whole once.
        if !touched.is_empty() && previous.iter().any(|tf| self.cached_sort(&ctx, &tf.task.id).is_none()) {
            return cold(&ctx);
        }
        let mut by_id: BTreeMap<String, Row> = previous
            .iter()
            .map(|tf| {
                let id = tf.task.id.clone();
                let row = Row {
                    tf: tf.clone(),
                    closed: false,
                    uuid: String::new(),
                    number: self.cached_number(&ctx, &id).unwrap_or(0),
                    sort: self.cached_sort(&ctx, &id).unwrap_or(f64::MAX),
                };
                (id, row)
            })
            .collect();
        let mut numbers = Vec::new();
        for issue in touched {
            let gone = issue.archived_at.is_some() || !matches_filters(&issue, &ctx.filters);
            let row = self.row(&ctx, issue, 0);
            if gone || row.closed {
                by_id.remove(&row.tf.task.id);
            } else {
                self.remember(&ctx, &row);
                numbers.push((row.tf.task.id.clone(), row.number));
                by_id.insert(row.tf.task.id.clone(), row);
            }
        }
        let mut rows: Vec<Row> = by_id.into_values().collect();
        rank(&mut rows);
        let (board, _) = finish(rows, &assignments);
        let (remaining, reset) = budget(&ctx.client);
        Ok(BoardRead {
            board,
            numbers,
            remaining,
            reset,
            rest_fallback: false,
            reopened: Vec::new(),
            viewer_login,
        })
    }

    fn list_in_status(&self, status: &Column) -> Result<Vec<IssueFile>> {
        let ctx = self.ctx()?;
        // Read the side of the board (open or history) the status's states
        // are on; a status with no state yet falls back to its category.
        let states = self.states(&ctx, false)?;
        let closed = match state_for_column(&states, status, ctx.catalog.as_ref()) {
            Some(st) => is_closed_state(st),
            None => is_terminal(status),
        };
        Ok(self
            .board(&ctx, terminal_filter(closed))?
            .into_iter()
            .filter(|tf| tf.task.column == *status)
            .collect())
    }

    fn get(&self, id: &str) -> Result<Option<IssueFile>> {
        let ctx = self.ctx()?;
        let Some(issue) = self.get_raw(&ctx, id)? else {
            return Ok(None);
        };
        let row = self.single_row(&ctx, issue);
        Ok(Some(self.with_overlay(row.tf)?))
    }

    fn fetch_many(&self, ids: &[&str]) -> Result<Vec<IssueFile>> {
        let (found, failed) = self.fetch_many_partial(ids)?;
        if let Some(id) = failed.first() {
            // Re-run the failing id to surface its error.
            self.get(id)?;
        }
        Ok(found)
    }

    fn fetch_many_partial(&self, ids: &[&str]) -> Result<(Vec<IssueFile>, Vec<String>)> {
        let ctx = self.ctx()?;
        // Ids with a known number are read in one request; the rest resolve
        // one at a time (by attachment), each failure isolated.
        let mut numbered: BTreeMap<i64, String> = BTreeMap::new();
        let mut others: Vec<&str> = Vec::new();
        let index = self.valid_index();
        for id in ids {
            let n = self
                .cached_number(&ctx, id)
                .or_else(|| index.as_ref().and_then(|i| i.numbers.get(*id).copied()))
                .or_else(|| number_from_id(id, &ctx.filters.team_key));
            match n {
                Some(n) => {
                    numbered.insert(n, (*id).to_string());
                }
                None => others.push(id),
            }
        }
        let mut found = Vec::new();
        let mut failed = Vec::new();
        if !numbered.is_empty() {
            let nums: Vec<i64> = numbered.keys().copied().collect();
            let filter = with(visible_filter(&ctx.filters), json!({ "number": { "in": nums } }));
            match ctx.client.issues(&IssueQuery::new(filter)) {
                Ok(issues) => {
                    let wanted: Vec<&String> = numbered.values().collect();
                    let mut hit: Vec<String> = Vec::new();
                    for issue in issues {
                        let row = self.single_row(&ctx, issue);
                        if wanted.contains(&&row.tf.task.id) {
                            hit.push(row.tf.task.id.clone());
                            found.push(self.with_overlay(row.tf)?);
                        }
                    }
                    // A number that named a different issue: resolve by id.
                    others.extend(
                        numbered
                            .values()
                            .filter(|id| !hit.contains(id))
                            .filter_map(|id| ids.iter().find(|i| **i == id.as_str()).copied()),
                    );
                }
                Err(_) => failed.extend(numbered.into_values()),
            }
        }
        for id in others {
            match self.get_raw(&ctx, id) {
                Ok(Some(issue)) => {
                    let row = self.single_row(&ctx, issue);
                    found.push(self.with_overlay(row.tf)?);
                }
                Ok(None) => {}
                Err(_) => failed.push(id.to_string()),
            }
        }
        Ok((found, failed))
    }

    fn add(&self, spec: NewIssue) -> Result<Issue> {
        Ok(self.add_with_assignees(spec)?.0)
    }

    fn add_with_assignees(&self, spec: NewIssue) -> Result<(Issue, Vec<String>)> {
        shelbi_core::validate_task_id(&spec.id)?;
        let ctx = self.ctx()?;
        let f = &ctx.filters;
        if self.get_raw(&ctx, &spec.id)?.is_some() {
            return Err(Error::Other(format!(
                "issue `{}` already exists in Linear team {}",
                spec.id, f.team_key
            )));
        }
        let state = self.target_state(&ctx, &spec.column)?;
        let order = self.column_order(&ctx, &spec.column)?;
        let position = spec.priority.map(|p| p as usize).unwrap_or(order.len()).min(order.len());
        let sort = Self::sort_for_position(&order, Some(position));

        // The assignee filter when set; otherwise the API key's owner, so the
        // issue is the user's (Zen only starts issues assigned to the user).
        let assignee = match &f.assignee_id {
            Some(a) => Some(a.clone()),
            None => match self.viewer(&ctx) {
                Ok(v) => Some(v.id),
                Err(e) => {
                    tracing::warn!(
                        project = %self.project,
                        id = %spec.id,
                        error = %e,
                        "could not resolve the Linear API key's user; creating the issue \
                         unassigned (it will not be Zen-eligible until assigned)"
                    );
                    None
                }
            },
        };

        let uuid = new_uuid();
        let mut input = json!({
            "id": uuid,
            "teamId": f.team_id,
            "title": spec.title,
            "description": spec.body,
            "stateId": state.id,
            "sortOrder": sort,
            "preserveSortOrderOnCreate": true,
        });
        if let Some(p) = &f.project_id {
            input["projectId"] = json!(p);
        }
        if let Some(l) = &f.label_id {
            input["labelIds"] = json!([l]);
        }
        if let Some(a) = &assignee {
            input["assigneeId"] = json!(a);
        }
        let meta = LinearMeta {
            id: Some(spec.id.clone()),
            workflow: spec.workflow.clone(),
            branch: spec.branch.clone(),
            pr: spec.pr.clone(),
            depends_on: spec.depends_on.clone(),
            prefers_machine: spec.prefers_machine.clone(),
            zen: spec.zen.clone(),
            launch: spec.launch.clone(),
            params: spec.params.clone(),
        };
        let attachment = attachment_input(&uuid, &f.team_key, &spec.id, &meta).map_err(|detail| {
            Error::MalformedIssueMetadata {
                id: spec.id.clone(),
                detail,
            }
        })?;

        let created = match ctx.client.create_issue(input, Some(attachment.clone())) {
            Ok(issue) => issue,
            // The create may have applied (a timeout, a 5xx, or the issue
            // landed and its attachment failed): read the client-chosen id.
            Err(e) if may_have_applied(&e) => match self.by_uuid(&ctx, &uuid) {
                Ok(Some(mut issue)) => {
                    if issue.shelbi_attachment().is_none() {
                        let att = ctx.client.upsert_attachment(attachment)?;
                        merge_attachment(&mut issue, att);
                    }
                    issue
                }
                Ok(None) if !is_ambiguous(&e) => return Err(e.into()),
                _ => {
                    return Err(Error::UnknownWriteOutcome(format!(
                        "creating issue `{id}` in Linear team {team}: the request reached \
                         Linear but no usable response came back ({e}), so the issue may or \
                         may not have been created; it was not retried to avoid a duplicate. \
                         Run `shelbi issue show {id}` to check, then retry only if it is absent.",
                        id = spec.id,
                        team = f.team_key,
                    )))
                }
            },
            Err(e) => return Err(e.into()),
        };

        let mut row = self.row(&ctx, created, position as u32);
        row.tf.task.column = spec.column.clone();
        self.remember(&ctx, &row);
        if !row.closed {
            let _ = crate::board_index::record_board_index_number(&self.project, &spec.id, row.number);
        }
        let tf = self.publish(&row)?;
        let assignees = tf.tracker_assignees.clone();
        let mut issue = tf.task;
        issue.assigned_to = None;
        Ok((issue, assignees))
    }

    fn viewer_login(&self) -> Option<String> {
        if let Some(login) = self.valid_index().and_then(|i| i.viewer_login) {
            if !login.is_empty() {
                return Some(login);
            }
        }
        let ctx = self.ctx().ok()?;
        self.viewer(&ctx).ok().map(|v| user_token(&v))
    }

    fn claim(&self, id: &str) -> Result<Vec<String>> {
        let ctx = self.ctx()?;
        let issue = self.require(&ctx, id)?;
        let viewer = self.viewer(&ctx)?;
        let after = self.update(&ctx, &issue.id, json!({ "assigneeId": viewer.id }))?;
        let row = self.single_row(&ctx, after);
        Ok(self.publish(&row)?.tracker_assignees)
    }

    fn move_status(&self, id: &str, to: &Column, _reason: &str) -> Result<Option<StatusMove>> {
        let ctx = self.ctx()?;
        self.move_inner(&ctx, id, to)
    }

    fn set_priority(&self, id: &str, pos: PrioMove) -> Result<()> {
        let ctx = self.ctx()?;
        let issue = self.require(&ctx, id)?;
        let col = column_for_state(&issue.state, ctx.catalog.as_ref());
        let mut order = self.column_order(&ctx, &col)?;
        let idx = match order.iter().position(|o| o.1 == issue.id) {
            Some(i) => i,
            // A list that lags a just-created issue: splice it in by sortOrder.
            None => {
                let at = order.partition_point(|o| o.2 < issue.sort_order);
                order.insert(at, (id.to_string(), issue.id.clone(), issue.sort_order));
                at
            }
        };
        let last = order.len().saturating_sub(1);
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
        let moved = order.remove(idx);
        let sort = Self::sort_for_position(&order, Some(dest));
        let before = dest.checked_sub(1).map(|i| order[i].2);
        let after = order.get(dest).map(|o| o.2);
        if strictly_between(sort, before, after) {
            let updated = self.update(&ctx, &moved.1, json!({ "sortOrder": sort }))?;
            let row = self.single_row(&ctx, updated);
            self.publish(&row)?;
            return Ok(());
        }
        // Neighbours too close for a midpoint: respace the whole column.
        order.insert(dest, moved);
        for (i, (_, uuid, current)) in order.iter().enumerate() {
            let want = (i as f64 + 1.0) * 1000.0;
            if *current != want {
                self.update(&ctx, uuid, json!({ "sortOrder": want }))?;
            }
        }
        Ok(())
    }

    fn set_fields(&self, id: &str, fields: IssueFields) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        // Workspace routing is local state, never written to Linear.
        if let Some(assigned_to) = &fields.assigned_to {
            crate::set_task_assignment(&self.project, id, assigned_to.as_deref())?;
        }
        let IssueFields {
            branch,
            depends_on,
            prefers_machine,
            assigned_to: _,
            title,
            workflow,
            body,
            pr,
            zen,
            launch,
            params,
        } = fields;
        if branch.is_none()
            && depends_on.is_none()
            && prefers_machine.is_none()
            && title.is_none()
            && workflow.is_none()
            && body.is_none()
            && pr.is_none()
            && zen.is_none()
            && launch.is_none()
            && params.is_empty()
        {
            return self.publish_assignment_overlay(id);
        }
        let ctx = self.ctx()?;
        let mut issue = self.require(&ctx, id)?;
        let mut meta = self.meta_for_write(&issue, id)?;
        let before = meta.clone();
        if let Some(v) = branch {
            meta.branch = v;
        }
        if let Some(v) = depends_on {
            meta.depends_on = v;
        }
        if let Some(v) = prefers_machine {
            meta.prefers_machine = v;
        }
        if let Some(v) = workflow {
            meta.workflow = v;
        }
        if let Some(v) = pr {
            meta.pr = v;
        }
        if let Some(v) = zen {
            meta.zen = v;
        }
        if let Some(v) = launch {
            meta.launch = v;
        }
        crate::issue_store::apply_param_edits(&mut meta.params, params);
        // A Linear-authored issue has no attachment yet: its first write
        // records the id it is known by.
        let meta_changed = meta != before || issue.shelbi_attachment().is_none() && meta != LinearMeta::default();

        let mut input = serde_json::Map::new();
        if let Some(t) = title.filter(|t| *t != issue.title) {
            input.insert("title".into(), json!(t));
        }
        if let Some(b) = body.filter(|b| Some(b) != issue.description.as_ref()) {
            input.insert("description".into(), json!(b));
        }
        if input.is_empty() && !meta_changed {
            return Ok(());
        }
        if !input.is_empty() {
            let atts = issue.attachments.take();
            issue = self.update(&ctx, &issue.id.clone(), Value::Object(input))?;
            if issue.attachments.is_none() {
                issue.attachments = atts;
            }
        }
        if meta_changed {
            self.write_meta(&ctx, &mut issue, id, &meta)?;
        }
        let row = self.single_row(&ctx, issue);
        self.publish(&row)?;
        Ok(())
    }

    fn cancel(&self, id: &str, reason: &str) -> Result<Option<StatusMove>> {
        self.move_status_and_unassign(id, &Column::canceled(), reason)
    }

    fn move_status_and_unassign(
        &self,
        id: &str,
        to: &Column,
        reason: &str,
    ) -> Result<Option<StatusMove>> {
        let mv = self.move_status(id, to, reason)?;
        crate::set_task_assignment(&self.project, id, None)?;
        Ok(mv)
    }

    fn delete(&self, id: &str) -> Result<()> {
        // The nearest durable effect that loses nothing: cancel it (the same
        // choice the GitHub backend makes). Idempotent.
        let ctx = self.ctx()?;
        if let Some(issue) = self.get_raw(&ctx, id)? {
            if !is_closed_state(&issue.state) {
                self.move_inner(&ctx, id, &Column::canceled())?;
            }
        }
        crate::set_task_assignment(&self.project, id, None)?;
        Ok(())
    }

    fn renumber(&self, _status: &Column) -> Result<()> {
        // A card's priority is its rank by Linear `sortOrder`, contiguous by
        // construction: nothing to repair.
        Ok(())
    }

    fn park_review(&self, id: &str) -> Result<Option<String>> {
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
        // Feedback first, then the move: a failure between them leaves the
        // feedback recorded rather than a moved card with no context.
        let ctx = self.ctx()?;
        let issue = self.require(&ctx, id)?;
        let prose = issue.description.clone().unwrap_or_default();
        let feedback = crate::format_review_feedback_section(reason, date);
        self.update(&ctx, &issue.id, json!({ "description": format!("{prose}{feedback}") }))?;
        self.move_status_and_unassign(id, ready, "user:review-reject")
    }

    fn poll_changes(&self, since: &Cursor) -> Result<(Vec<IssueChange>, Cursor)> {
        let ctx = self.ctx()?;
        let Some(w) = since.watermark() else {
            // First poll: just the high-water mark.
            let q = IssueQuery::new(Value::Object(visible_filter(&ctx.filters)))
                .order(IssueOrder::UpdatedAt);
            let newest = ctx.client.issues_page(&q, 1, None)?.nodes;
            let high = newest.first().map(|i| i.updated_at).unwrap_or_else(Utc::now);
            return Ok((Vec::new(), Cursor::at(high)));
        };
        let filter = with(
            visible_filter(&ctx.filters),
            json!({ "updatedAt": { "gt": w.to_rfc3339() } }),
        );
        let touched = ctx
            .client
            .issues(&IssueQuery::new(filter).order(IssueOrder::UpdatedAt))?;
        let mut changes = Vec::new();
        let mut high = w;
        for issue in touched {
            if issue.updated_at <= w {
                continue;
            }
            high = high.max(issue.updated_at);
            let uuid = issue.id.clone();
            let row = self.single_row(&ctx, issue);
            let issue_id = row.tf.task.id.clone();
            changes.push(IssueChange::Upserted(Box::new(row.tf)));
            // A new comment bumps its issue's updatedAt, so comments can only
            // be new on an issue in this set.
            for c in ctx.client.issue_comments(&uuid, Some(w))? {
                if c.created_at > w {
                    high = high.max(c.created_at);
                    changes.push(IssueChange::CommentAdded {
                        issue_id: issue_id.clone(),
                        comment: to_comment(c),
                    });
                }
            }
        }
        Ok((changes, Cursor::at(high)))
    }

    fn list_comments(&self, id: &str) -> Result<Vec<IssueComment>> {
        let ctx = self.ctx()?;
        let Some(issue) = self.get_raw(&ctx, id)? else {
            return Ok(Vec::new());
        };
        Ok(ctx
            .client
            .issue_comments(&issue.id, None)?
            .into_iter()
            .map(to_comment)
            .collect())
    }

    fn add_comment(&self, id: &str, body: &str) -> Result<IssueComment> {
        let ctx = self.ctx()?;
        let issue = self.require(&ctx, id)?;
        // A client-chosen id settles an unknown outcome with one read; the
        // hidden marker is the convention every Shelbi-posted comment carries
        // (stripped on read).
        let comment_id = new_uuid();
        let posted = format!("{body}\n\n{}", crate::github_store::comment_marker());
        let input = json!({ "id": comment_id, "issueId": issue.id, "body": posted });
        match ctx.client.create_comment(input) {
            Ok(c) => Ok(to_comment(c)),
            Err(e) if may_have_applied(&e) => {
                let landed = ctx
                    .client
                    .issue_comments(&issue.id, None)
                    .ok()
                    .and_then(|cs| cs.into_iter().find(|c| c.id == comment_id));
                match landed {
                    Some(c) => Ok(to_comment(c)),
                    None if !is_ambiguous(&e) => Err(e.into()),
                    None => Err(Error::UnknownWriteOutcome(format!(
                        "commenting on issue `{id}` in Linear: the request reached Linear but no \
                         usable response came back ({e}), so the comment may or may not have \
                         been posted; it was not retried to avoid a duplicate. Check {} before \
                         retrying.",
                        issue.url
                    ))),
                }
            }
            Err(e) => Err(e.into()),
        }
    }
}

// --- helpers ------------------------------------------------------------------

/// Rank rows within their status by `sortOrder` (then number), setting each
/// card's `priority` to its 0-based rank.
fn rank(rows: &mut [Row]) {
    rows.sort_by(|a, b| {
        a.tf.task
            .column
            .as_str()
            .cmp(b.tf.task.column.as_str())
            .then(a.sort.total_cmp(&b.sort))
            .then(a.number.cmp(&b.number))
    });
    let mut current: Option<Column> = None;
    let mut n = 0u32;
    for row in rows.iter_mut() {
        if current.as_ref() != Some(&row.tf.task.column) {
            current = Some(row.tf.task.column.clone());
            n = 0;
        }
        row.tf.task.priority = n;
        n += 1;
    }
}

/// The board (canonical order, overlay folded) and its id→number pairs.
fn finish(mut rows: Vec<Row>, assignments: &BTreeMap<String, String>) -> (Vec<IssueFile>, Vec<(String, i64)>) {
    rows.sort_by(|a, b| {
        let (a, b) = (&a.tf.task, &b.tf.task);
        a.column
            .board_order()
            .cmp(&b.column.board_order())
            .then(a.column.as_str().cmp(b.column.as_str()))
            .then(a.priority.cmp(&b.priority))
            .then(a.id.cmp(&b.id))
    });
    let numbers = rows
        .iter()
        .filter(|r| r.number > 0)
        .map(|r| (r.tf.task.id.clone(), r.number))
        .collect();
    let board = rows.into_iter().map(|r| fold(r.tf, assignments)).collect();
    (board, numbers)
}

fn fold(mut tf: IssueFile, assignments: &BTreeMap<String, String>) -> IssueFile {
    tf.task.assigned_to = assignments.get(&tf.task.id).cloned();
    tf
}

fn to_comment(c: crate::linear_client::LinearComment) -> IssueComment {
    IssueComment {
        id: c.id,
        author: c.user.as_ref().map(user_token),
        created_at: c.created_at,
        body: crate::github_store::strip_comment_marker(&c.body),
    }
}

/// A write error after which the write may nonetheless have landed: worth
/// one read to settle. (Auth, a rate limit, a connect failure, or a request
/// Linear rejected as invalid before running it never applied.)
fn may_have_applied(e: &LinearError) -> bool {
    !matches!(
        e,
        LinearError::Local(_)
            | LinearError::Unauthenticated { .. }
            | LinearError::Forbidden { .. }
            | LinearError::RateLimited { .. }
            | LinearError::Transport(TransportFailure::Connect(_))
            | LinearError::Resolve { .. }
    )
}

/// A write error that says nothing about whether the write applied.
fn is_ambiguous(e: &LinearError) -> bool {
    matches!(
        e,
        LinearError::Server { .. }
            | LinearError::Transport(TransportFailure::Timeout(_))
            | LinearError::Transport(TransportFailure::Other(_))
            | LinearError::Decode { .. }
    )
}

// --- process caches -----------------------------------------------------------

type TeamId = String;
type StatesCache = Mutex<HashMap<TeamId, (Instant, Vec<LinearState>)>>;
type IdCache<T> = Mutex<HashMap<(TeamId, String), T>>;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn states_cache() -> &'static StatesCache {
    static C: OnceLock<StatesCache> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn viewer_cache() -> &'static Mutex<HashMap<String, LinearUser>> {
    static C: OnceLock<Mutex<HashMap<String, LinearUser>>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn number_cache() -> &'static IdCache<i64> {
    static C: OnceLock<IdCache<i64>> = OnceLock::new();
    C.get_or_init(Default::default)
}

fn sort_cache() -> &'static IdCache<f64> {
    static C: OnceLock<IdCache<f64>> = OnceLock::new();
    C.get_or_init(Default::default)
}

/// Empty the process caches (a test builds a fresh store per scenario, and
/// the caches outlive it).
#[cfg(any(test, feature = "test-support"))]
pub fn clear_linear_caches_for_test() {
    lock(states_cache()).clear();
    lock(viewer_cache()).clear();
    lock(number_cache()).clear();
    lock(sort_cache()).clear();
}
