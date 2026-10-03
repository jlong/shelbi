//! The one library behind every issue **mutation** — `move`, `start`,
//! `assign`/`unassign`, `edit`, `add`, and review `approve`/`reject`.
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
