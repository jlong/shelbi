//! The one library behind every issue **mutation** — `move`, `start`,
//! `assign`/`unassign`, `edit`, `add`, `prioritize` (reorder within a column),
//! and review `approve`/`reject`.
//!
//! Historically each of these lived in `shelbi-cli` (`commands/issue.rs` and the
//! review commands), reading the issue, running git/merge side effects, and
//! writing status under the store's file lock. With several clients (the CLI,
//! the single-process TUI, the desktop app) that is not enough: two clients can
//! interleave and land stale work (see the "Removing tmux" plan, "The daemon
//! executes mutations"). So the mutation logic moves here, and the daemon is the
//! one caller that runs it under a per-issue queue; the CLI and the app become
//! thin clients of the daemon's control socket.
//!
//! This module never prints and never touches a UI toolkit. User-facing output
//! flows through an [`OutputSink`] — the CLI's sink writes to stdout/stderr, the
//! daemon's sink streams [`shelbi_proto::control::ServerMsg::Line`] frames to the
//! requesting client — so the in-process path and the daemon path produce
//! identical output by construction. The terminal result is an
//! [`Ok`]/[`MutateError`]; the CLI turns a [`MutateError`] into the same
//! `anyhow` error (and exit code) it always produced.
//!
//! **Expected state and recheck.** A mutation that performs an irreversible step
//! (a merge, a push, a dispatch) takes a `recheck` callback it invokes
//! immediately before that step. The daemon supplies a closure that re-reads the
//! issue and compares it to the `(status, revision)` the client was looking at,
//! aborting with [`MutateError::Stale`] if it moved on; the in-process path
//! supplies a no-op. There is no issue etag, so the revision is `updated_at`.

use std::fmt;

use shelbi_core::Issue;
use shelbi_proto::control::{ChangeNote, ExpectedState, MutationKind, Stream};
use shelbi_state::{IssueFile, IssueStore};

mod add_edit;
mod start;

pub use start::StartParams;

/// Where a line of mutation output belongs. A [`MutateError`] is returned, not
/// emitted, so this only carries the informational lines a command prints.
pub trait OutputSink {
    /// Emit `text` (a single line, no trailing newline) on `stream`.
    fn emit(&mut self, stream: Stream, text: &str);

    /// A stdout line.
    fn out(&mut self, text: &str) {
        self.emit(Stream::Stdout, text);
    }

    /// A stderr line (callers pass the full text, including any `warning: `
    /// prefix, exactly as the CLI printed it).
    fn warn(&mut self, text: &str) {
        self.emit(Stream::Stderr, text);
    }
}

/// A sink that discards everything — handy for tests that only assert state.
pub struct NullSink;
impl OutputSink for NullSink {
    fn emit(&mut self, _stream: Stream, _text: &str) {}
}

/// A sink that records `(stream, text)` lines — for tests and the daemon's own
/// buffering.
#[derive(Debug, Default)]
pub struct RecordingSink {
    pub lines: Vec<(Stream, String)>,
}
impl OutputSink for RecordingSink {
    fn emit(&mut self, stream: Stream, text: &str) {
        self.lines.push((stream, text.to_string()));
    }
}

/// The recheck hook invoked immediately before an irreversible step. Returns
/// [`MutateError::Stale`] when the issue moved on since the client read it. The
/// in-process path passes [`no_recheck`].
pub type Recheck<'a> = &'a mut dyn FnMut() -> Result<(), MutateError>;

/// A recheck that always passes — the in-process (setting-off) path, which has
/// no concurrent clients to guard against.
pub fn no_recheck() -> impl FnMut() -> Result<(), MutateError> {
    || Ok(())
}

/// Why a mutation did not happen. Every variant leaves the issue unchanged
/// (approve/reject/move abort before writing; `start` rolls back a persisted
/// move on a clean spawn failure). `Display` is the text the CLI prints after
/// `Error: `, except [`MutateError::LaunchSpawn`], which the CLI off-path wraps
/// with the historical `launching workspace` context chain.
#[derive(Debug)]
pub enum MutateError {
    /// The issue moved on since the client read it.
    Stale {
        expected: ExpectedState,
        actual: ExpectedState,
    },
    /// A clean spawn failure in `start`. The inner error is the one the launch
    /// returned; the CLI renders it under `launching workspace`.
    LaunchSpawn(shelbi_core::Error),
    /// Everything else. `Display` is the verbatim message.
    Backend(String),
}

impl MutateError {
    /// Build a [`MutateError::Backend`] from anything printable — the common
    /// mapping for a `shelbi_core::Error` the library surfaces verbatim.
    pub fn backend(e: impl fmt::Display) -> Self {
        MutateError::Backend(e.to_string())
    }
}

impl fmt::Display for MutateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MutateError::Stale { expected, actual } => write!(
                f,
                "issue changed since you looked at it (you saw `{}` rev {}, it is now `{}` rev {}); \
                 nothing was changed — refresh and retry",
                expected.status, expected.updated_at, actual.status, actual.updated_at
            ),
            MutateError::LaunchSpawn(e) => write!(f, "launching workspace: {e}"),
            MutateError::Backend(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for MutateError {}

impl From<MutateError> for shelbi_proto::control::MutationError {
    fn from(e: MutateError) -> Self {
        use shelbi_proto::control::MutationError as P;
        match e {
            MutateError::Stale { expected, actual } => P::Stale { expected, actual },
            MutateError::LaunchSpawn(inner) => P::Backend {
                message: format!("launching workspace: {inner}"),
            },
            MutateError::Backend(message) => P::Backend { message },
        }
    }
}

/// Load one issue, mapping an absent issue to the historical
/// `issue \`id\` not found` message.
pub(crate) fn load_issue(
    store: &dyn IssueStore,
    id: &str,
) -> Result<IssueFile, MutateError> {
    store
        .get(id)
        .map_err(MutateError::backend)?
        .ok_or_else(|| MutateError::Backend(format!("issue `{id}` not found")))
}

/// The `(status, revision)` of `task`, the shape [`ExpectedState`] carries.
pub fn state_of(task: &Issue) -> ExpectedState {
    ExpectedState {
        status: task.column.as_str().to_string(),
        updated_at: task.updated_at.to_rfc3339(),
    }
}

/// The current on-disk `(status, revision)` for `id`, for the daemon's
/// expected-state gate and recheck closure. An absent issue is reported as
/// status `<deleted>` so a recheck against a since-deleted issue reads as
/// "moved on".
pub fn current_state(project: &str, id: &str) -> Result<ExpectedState, MutateError> {
    let store = issue_store(project)?;
    match store.get(id).map_err(MutateError::backend)? {
        Some(tf) => Ok(state_of(&tf.task)),
        None => Ok(ExpectedState {
            status: "<deleted>".to_string(),
            updated_at: String::new(),
        }),
    }
}

/// Did the issue move on in a way that should abort a mutation as stale, given
/// the `(status, revision)` the client was looking at (`expected`) and the
/// current one (`actual`)?
///
/// The **status** is the authoritative, client-controlled dimension: a real
/// concurrent change that matters — someone else moving the card to another
/// column — changes it, and a status change always counts as stale.
///
/// The **revision** (`updated_at`) is a trustworthy concurrency token only on a
/// local filesystem board, where it is bumped exactly when a field changes. On
/// a remote (GitHub) board it is the issue's server-side `updatedAt`, which
/// Shelbi's *own* writes bump out of band: `issue move` writes the status back
/// to GitHub (and the board refresh picks that write up) ~15-30s *after* the
/// command returns, so a follow-up `issue start` that reads the issue in that
/// window sees the revision tick forward with the status unchanged. That is a
/// self-write, not a conflict. So on a remote board a pure revision bump with an
/// unchanged status is NOT stale — only a status change is. (This is the
/// "`issue start` right after `issue move` fails with a stale-revision
/// conflict" bug.)
pub fn is_stale_change(project: &str, expected: &ExpectedState, actual: &ExpectedState) -> bool {
    if actual.status != expected.status {
        return true;
    }
    if actual.updated_at == expected.updated_at {
        return false;
    }
    // Same status, revision moved. Count it stale only where the revision is a
    // reliable token (a local board); on a remote board it is a self-write.
    revision_is_authoritative(project)
}

/// Is the issue `updated_at` a reliable concurrency token for `project`? True
/// for a local filesystem board (bumped exactly on a field change), false for a
/// remote board whose `updatedAt` Shelbi's own writes bump out of band. On a
/// config-read error, fall back to the strict (historical) behavior and treat
/// the revision as authoritative — but the caller reaches here only after
/// [`current_state`] already loaded the store, so a load failure is unlikely.
fn revision_is_authoritative(project: &str) -> bool {
    match shelbi_state::load_project(project) {
        Ok(p) => !p.issue_tracker.backend.is_remote(),
        Err(_) => true,
    }
}

/// Build the `Changed` notification for a completed mutation by re-reading the
/// issue's post-change state (best-effort; empty status/rev if it is gone).
fn change_note(store: &dyn IssueStore, project: &str, id: &str, verb: &str) -> ChangeNote {
    let (status, updated_at) = match store.get(id) {
        Ok(Some(tf)) => (
            tf.task.column.as_str().to_string(),
            tf.task.updated_at.to_rfc3339(),
        ),
        _ => (String::new(), String::new()),
    };
    ChangeNote {
        project: project.to_string(),
        id: id.to_string(),
        verb: verb.to_string(),
        status,
        updated_at,
    }
}

/// Run a mutation and return the change to announce. Dispatches by
/// [`MutationKind`]; `recheck` is invoked immediately before any irreversible
/// step (see the module docs). The issue id (and project) are the enclosing
/// request's; [`MutationKind::Add`] ignores `id` and uses its own.
pub fn apply(
    project: &str,
    id: &str,
    kind: &MutationKind,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<ChangeNote, MutateError> {
    match kind {
        MutationKind::Move {
            to,
            reason,
            skip_transition_actions,
        } => {
            apply_move(project, id, to, reason.as_deref(), *skip_transition_actions, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Start {
            workspace,
            branch,
            reason,
            force,
        } => {
            start::start(
                &StartParams {
                    project,
                    id,
                    workspace: workspace.as_deref(),
                    branch: branch.as_deref(),
                    reason: reason.as_deref(),
                    force: *force,
                },
                sink,
                recheck,
            )?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Assign { to, force } => {
            apply_assign(project, id, to, *force, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Unassign => {
            apply_unassign(project, id, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Add(spec) => {
            let created = add_edit::add(project, spec, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, &created, kind.verb()))
        }
        MutationKind::Edit(spec) => {
            add_edit::edit(project, id, spec, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Approve => {
            apply_approve(project, id, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Reject { reason } => {
            apply_reject(project, id, reason, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
        MutationKind::Prioritize { position } => {
            apply_prioritize(project, id, *position, sink, recheck)?;
            let store = issue_store(project)?;
            Ok(change_note(store.as_ref(), project, id, kind.verb()))
        }
    }
}

/// The configured issue store for `project`.
pub(crate) fn issue_store(project: &str) -> Result<Box<dyn IssueStore>, MutateError> {
    shelbi_state::issue_store_for(project).map_err(MutateError::backend)
}

// --- move -----------------------------------------------------------------

fn apply_move(
    project: &str,
    id: &str,
    to: &str,
    reason: Option<&str>,
    skip_transition_actions: bool,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    use crate::transition::{move_issue, MoveError, MoveRequest};
    // Irreversible for a merge edge; recheck immediately before.
    recheck()?;
    let outcome = move_issue(
        &MoveRequest {
            project,
            id,
            to,
            reason: reason.unwrap_or("user:cli"),
            workspace_fallback: "cli",
            skip_transition_actions,
        },
        &mut |w| sink.warn(&format!("warning: {w}")),
    )
    .map_err(|e| match e {
        MoveError::Move(e) | MoveError::LoadProject(e) | MoveError::BranchCut(e) => {
            MutateError::backend(e)
        }
        e @ (MoveError::Merge { .. } | MoveError::EventAppend { .. }) => {
            MutateError::Backend(e.to_string())
        }
    })?;
    sink.out(&format!("✓ {id} → {}", outcome.column));
    Ok(())
}

// --- prioritize -----------------------------------------------------------

/// Land `id` at absolute slot `position` in its column (the store re-derives
/// the column under its lock and clamps), the daemon-routed form of
/// `shelbi issue prio --set`. Prints the same line the CLI does.
fn apply_prioritize(
    project: &str,
    id: &str,
    position: u32,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    let store = issue_store(project)?;
    let tf = load_issue(store.as_ref(), id)?;
    // A plain state write, but a reorder queued behind a move that took the
    // card to another column must not land it at a slot in the new column.
    recheck()?;
    store
        .set_priority(id, shelbi_state::PrioMove::Set(position))
        .map_err(MutateError::backend)?;
    sink.out(&format!("✓ {id} now at slot {position} in {}", tf.task.column));
    Ok(())
}

// --- assign / unassign ----------------------------------------------------

fn apply_assign(
    project: &str,
    id: &str,
    workspace: &str,
    force: bool,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    let project_yaml = shelbi_state::load_project(project).map_err(MutateError::backend)?;
    let ws = project_yaml.workspace(workspace).ok_or_else(|| {
        MutateError::Backend(format!(
            "workspace `{workspace}` not declared in project `{project}` (known: {})",
            project_yaml
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;
    guard_review_slot(&project_yaml, ws, workspace, id, force, sink)?;
    let store = issue_store(project)?;
    // `id` must exist — surface a clear error before the assignment write.
    load_issue(store.as_ref(), id)?;
    ensure_workspace_dispatchable(project, workspace, id)?;
    // Assignment is a plain state write, but guard it on the recheck so a
    // queued assign that raced an out-of-band move is refused rather than
    // pinning a workspace to a card that moved on.
    recheck()?;
    store
        .set_fields(
            id,
            shelbi_state::IssueFields {
                assigned_to: Some(Some(workspace.to_string())),
                ..Default::default()
            },
        )
        .map_err(MutateError::backend)?;
    let _ = store.clear_parked(id);
    sink.out(&format!("✓ {id} assigned to {workspace}"));
    Ok(())
}

fn apply_unassign(
    project: &str,
    id: &str,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    let store = issue_store(project)?;
    let tf = load_issue(store.as_ref(), id)?;
    recheck()?;
    if tf.task.column == shelbi_core::Column::review() {
        store.park_review(id).map_err(MutateError::backend)?;
        sink.out(&format!(
            "✓ {id} unassigned (parked — won't auto-reload for review)"
        ));
        return Ok(());
    }
    store
        .set_fields(
            id,
            shelbi_state::IssueFields {
                assigned_to: Some(None),
                ..Default::default()
            },
        )
        .map_err(MutateError::backend)?;
    sink.out(&format!("✓ {id} unassigned"));
    Ok(())
}

// --- approve / reject -----------------------------------------------------

fn apply_approve(
    project: &str,
    id: &str,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    // The accept runs a gated merge (irreversible); recheck immediately before.
    recheck()?;
    crate::review_ui::approve_review_task(project, id).map_err(MutateError::backend)?;
    sink.out(&format!("✓ {id} approved"));
    Ok(())
}

fn apply_reject(
    project: &str,
    id: &str,
    reason: &str,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    recheck()?;
    crate::review_ui::reject_review(project, id, reason).map_err(MutateError::backend)?;
    sink.out(&format!("✓ {id} rejected back to the ready queue"));
    Ok(())
}

// --- shared guards (ported from shelbi-cli commands/issue.rs) --------------

/// The card genuinely occupying `workspace_name` right now (live board), if any.
pub(crate) fn workspace_occupied_by(
    project: &str,
    workspace_name: &str,
    exclude_id: &str,
) -> Result<Option<Issue>, MutateError> {
    Ok(shelbi_state::read_board(project)
        .map_err(MutateError::backend)?
        .into_issues()
        .into_iter()
        .map(|tf| tf.task)
        .find(|task| {
            task.column == shelbi_core::Column::in_progress()
                && task.assigned_to.as_deref() == Some(workspace_name)
                && task.id != exclude_id
        }))
}

/// The issue — other than `exclude_id` — that `workspace_name` is actively
/// assigned to right now, decided from **local, authoritative** state rather
/// than the daemon's published index.
///
/// This is the release guard behind the supplant decision: a dispatch tears down
/// the card's prior assignee only when that workspace is still on THIS card (or
/// idle). If it has since been re-dispatched to another issue this returns that
/// issue, so the release is skipped rather than killing a live worker mid-task on
/// an unrelated card.
///
/// Why not the published index: on a remote (GitHub) backend `read_board` serves
/// the daemon's `board-index.json`, which lags local routing — an assignment
/// write never bumps a GitHub `updatedAt`, and a parked or dead daemon stops
/// republishing entirely. In the 2026-10-04 incident the prior assignee had been
/// re-dispatched to another card *after* the last refresh and the daemon then
/// died: the index still showed that card in `todo` (and the old guard only
/// counted `in_progress`/`review`), so the workspace read as idle and its live
/// worker was killed. Assignment for a remote backend lives only in the local
/// overlay (`assignments/<id>`), which every dispatch writes synchronously and
/// `read_board`'s overlay re-resolve folds in. That overlay is the authoritative
/// routing record here: any non-terminal issue other than `exclude_id` routed to
/// the workspace counts as busy, whatever column a stale index shows it in.
///
/// Returns:
/// - `Ok(Some(issue))` — the workspace is busy on another non-terminal issue;
///   the caller skips the release and logs it.
/// - `Ok(None)` — the workspace is definitively free (on THIS card or idle);
///   the caller releases its pane.
/// - `Err(_)` — can't confirm (the overlay read failed, or local state names
///   another issue but the board isn't `Warm` enough to rule out that it has
///   since gone terminal). The caller errs toward NOT releasing.
pub(crate) fn workspace_busy_with_other(
    project: &str,
    workspace_name: &str,
    exclude_id: &str,
) -> Result<Option<Issue>, MutateError> {
    let cfg = shelbi_state::load_project(project)
        .map_err(MutateError::backend)?
        .issue_tracker;
    if !cfg.backend.is_remote() {
        // Local (`file_system`) board: assignment lives in the card frontmatter
        // and the directory read is always authoritative (`BoardState::Warm`).
        // The open board is open-only, so every row is non-terminal — any card
        // assigned to this workspace other than the one being dispatched counts,
        // whatever column it is parked in.
        return Ok(shelbi_state::read_board(project)
            .map_err(MutateError::backend)?
            .into_issues()
            .into_iter()
            .map(|tf| tf.task)
            .find(|task| {
                task.assigned_to.as_deref() == Some(workspace_name) && task.id != exclude_id
            }));
    }

    // Remote backend: the local assignment overlay is the authoritative routing
    // record. Every dispatch writes it synchronously, so it never lags the way
    // the daemon's published index can.
    let mut others: Vec<String> = shelbi_state::task_assignments(project)
        .map_err(MutateError::backend)?
        .into_iter()
        .filter(|(id, ws)| ws == workspace_name && id != exclude_id)
        .map(|(id, _)| id)
        .collect();
    if others.is_empty() {
        // Authoritative: nothing else is routed to this workspace, so it is on
        // THIS card or idle — safe to release.
        return Ok(None);
    }

    // At least one other issue is routed here. Confirm it is still non-terminal
    // before counting it busy: a terminal card's marker can linger, and a stale
    // marker alone must not block a legitimate release. The open board drops
    // terminal cards, so presence there is the confirmation — and it holds even
    // for a `Stale` index, which is exactly the incident shape (the routed card
    // still listed, in whatever column).
    let state = shelbi_state::read_board(project).map_err(MutateError::backend)?;
    if let Some(task) = state
        .issues()
        .iter()
        .find(|tf| others.contains(&tf.task.id))
        .map(|tf| tf.task.clone())
    {
        return Ok(Some(task));
    }

    // None of the routed issues are in the open board. A `Warm` board is the
    // current open-only set, so their absence proves they have gone terminal and
    // the workspace is free. A board that isn't warm (stale, cold, unreachable)
    // can't prove that, so we can't confirm — err toward NOT releasing.
    match state {
        shelbi_state::BoardState::Warm(_) => Ok(None),
        _ => {
            others.sort();
            Err(MutateError::Backend(format!(
                "the prior assignee `{workspace_name}` is routed to `{}` in the local overlay but \
                 the board index isn't warm enough to confirm whether it is still active",
                others.join("`, `"),
            )))
        }
    }
}

/// Refuse to dispatch/assign onto a workspace already running a *different*
/// in-flight issue. Shared by assign and start.
pub(crate) fn ensure_workspace_dispatchable(
    project: &str,
    workspace_name: &str,
    exclude_id: &str,
) -> Result<(), MutateError> {
    if let Some(other) = workspace_occupied_by(project, workspace_name, exclude_id)? {
        return Err(MutateError::Backend(format!(
            "workspace `{workspace_name}` is already on issue `{}` ({}) — \
             move it to another column first",
            other.id, other.column,
        )));
    }
    Ok(())
}

/// Guard against routing a normal dev issue onto a review slot. A `--force`
/// override is allowed but recorded on `events.log` (best-effort).
pub(crate) fn guard_review_slot(
    project_yaml: &shelbi_core::Project,
    workspace: &shelbi_core::WorkspaceSpec,
    workspace_name: &str,
    task_id: &str,
    force: bool,
    sink: &mut dyn OutputSink,
) -> Result<(), MutateError> {
    if !project_yaml.effective_tags(workspace).contains("review") {
        return Ok(());
    }
    if !force {
        return Err(MutateError::Backend(format!(
            "workspace `{workspace_name}` is a review slot (tagged `review`) — review issues \
             load via the review queue, not direct dispatch; pick a non-review workspace \
             (or pass --force to override)"
        )));
    }
    if let Err(e) =
        shelbi_state::append_review_slot_override_event(task_id, workspace_name, "user:force")
    {
        sink.warn(&format!(
            "warning: append_review_slot_override_event failed: {e}"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
