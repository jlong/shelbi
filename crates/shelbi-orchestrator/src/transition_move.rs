//! [`move_issue`] — the one library function behind every user-driven status
//! move: `shelbi issue move` and a card dragged across the TUI board.
//!
//! Both surfaces used to carry their own copy of the move. The CLI's copy
//! gated a merge edge on the workflow's `merge` (see [`run_gated_merge`]); the
//! board's copy only wrote the status, so a card dragged `review -> done` read
//! done with nothing merged and its branch still alive. Routing both through
//! this function is what keeps a declared transition action from depending on
//! *where* the move was made.
//!
//! The steps, in order:
//!
//! 1. Load the issue and its workflow, and resolve the target against the
//!    statuses that workflow declares (an undeclared target errors).
//! 2. A move INTO `in-progress` cuts the issue's branch first.
//! 3. An edge that declares `merge` runs its gated merge (the pre-merge prefix
//!    plus `merge`). A failure aborts here: nothing is written and the issue
//!    stays where it was.
//! 4. Write the status and append the move event (an event that can't be
//!    appended rolls the status back).
//! 5. Fire the merge edge's remaining actions (`delete_branch`, `run:` /
//!    `ready:`), best-effort — the move already landed.
//!
//! This function never prints: a caller's surface may be a full-screen TUI.
//! Non-fatal conditions are handed to the `warn` callback as they occur and
//! the terminal result comes back as a [`MoveOutcome`] / [`MoveError`], so the
//! CLI can keep its exact stdout/stderr and the board can route the same facts
//! to its status line and error log.
//!
//! [`run_gated_merge`]: super::run_gated_merge

use std::fmt;

use shelbi_core::{
    default_workflow, Column, Error, Issue, Project, Result, TransitionAction, Workflow,
};
use shelbi_state::{IssueFile, IssueStore};

use super::{ActionOutcome, GatedMerge};

/// The `from -> to` edge an issue is crossing, with everything a
/// [`TransitionRunner`] needs to fire that edge's actions.
#[derive(Clone, Copy)]
pub struct TransitionEdge<'a> {
    pub project: &'a Project,
    pub project_name: &'a str,
    pub issue: &'a IssueFile,
    pub workflow: &'a Workflow,
    pub from: &'a str,
    pub to: &'a str,
}

/// The git / GitHub side effects of a move, behind a seam so a test can
/// record them instead of running `git` and `gh`. [`GitTransitionRunner`] is
/// the real implementation; [`move_issue`] uses it.
pub trait TransitionRunner {
    /// Cut the issue's branch for a move into `in-progress`
    /// ([`crate::lifecycle::ensure_branch_for_in_progress`]).
    fn cut_branch(&self, project: &Project, task_id: &str) -> Result<()>;

    /// Run the edge's gated merge ([`super::run_gated_merge`]).
    fn gated_merge(
        &self,
        edge: &TransitionEdge<'_>,
        workspace_label: &str,
    ) -> Result<Option<GatedMerge>>;

    /// Fire the edge's actions other than `skip`
    /// ([`super::execute_transition_except`]).
    fn remaining_actions(
        &self,
        edge: &TransitionEdge<'_>,
        skip: &[TransitionAction],
    ) -> Result<Vec<ActionOutcome>>;
}

/// The live [`TransitionRunner`]: real branches, real merges.
#[derive(Debug, Clone, Copy, Default)]
pub struct GitTransitionRunner;

impl TransitionRunner for GitTransitionRunner {
    fn cut_branch(&self, project: &Project, task_id: &str) -> Result<()> {
        crate::lifecycle::ensure_branch_for_in_progress(project, task_id).map(|_| ())
    }

    fn gated_merge(
        &self,
        edge: &TransitionEdge<'_>,
        workspace_label: &str,
    ) -> Result<Option<GatedMerge>> {
        super::run_gated_merge(
            edge.project,
            edge.project_name,
            &edge.issue.task,
            &edge.issue.body,
            edge.workflow,
            edge.from,
            edge.to,
            workspace_label,
        )
    }

    fn remaining_actions(
        &self,
        edge: &TransitionEdge<'_>,
        skip: &[TransitionAction],
    ) -> Result<Vec<ActionOutcome>> {
        super::execute_transition_except(
            edge.project,
            edge.project_name,
            &edge.issue.task,
            &edge.issue.body,
            edge.workflow,
            edge.from,
            edge.to,
            skip,
        )
    }
}

/// One requested move.
#[derive(Debug, Clone, Copy)]
pub struct MoveRequest<'a> {
    pub project: &'a str,
    pub id: &'a str,
    /// The target status as the caller spells it. Resolved against the issue's
    /// workflow, alias-normalized first (`wip` / `in_progress` land on
    /// `in-progress`) and then verbatim.
    pub to: &'a str,
    /// The `reason=` recorded on the move event (`user:cli`, `user:tui`, …).
    pub reason: &'a str,
    /// The `workspace=` label on the gated merge's event when the issue has no
    /// assigned workspace (`cli`, `board`).
    pub workspace_fallback: &'a str,
    /// Recovery escape hatch: cross the edge WITHOUT running any of its
    /// actions, stamping the move event `actions=skipped`.
    pub skip_transition_actions: bool,
}

/// A successful [`move_issue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveOutcome {
    /// The resolved target status.
    pub column: Column,
    /// False when the issue was already in `column` (nothing written, no event).
    pub moved: bool,
    /// The gated merge that ran before the move, when the edge declares one.
    pub merge: Option<GatedMerge>,
}

/// A non-fatal condition raised during a move. `Display` is the text the CLI
/// prints after `warning: `.
#[derive(Debug)]
pub enum MoveWarning {
    /// The issue's workflow couldn't be loaded; the built-in default was used.
    WorkflowFallback { name: String, error: Error },
    /// The merge landed and the issue moved, but a remaining edge action
    /// (`delete_branch`, a `run:` command) failed.
    PostMergeCleanup { id: String, error: Error },
}

impl fmt::Display for MoveWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoveWarning::WorkflowFallback { name, error } => write!(
                f,
                "workflow `{name}` could not be loaded ({error}); using built-in default"
            ),
            MoveWarning::PostMergeCleanup { id, error } => write!(
                f,
                "post-merge cleanup for `{id}` failed (merge already landed): {error}"
            ),
        }
    }
}

/// Why a move didn't happen. Every variant leaves the issue in its original
/// status. `Display` is the message the CLI has always printed for that case;
/// a caller with its own vocabulary (the board's `branch cut failed:` /
/// `move failed:` prefixes) matches on the variant instead.
#[derive(Debug)]
pub enum MoveError {
    /// Loading the issue, resolving the target, or writing the status failed.
    Move(Error),
    /// The project config a branch cut or merge needs couldn't be loaded.
    LoadProject(Error),
    /// The `in-progress` branch cut failed.
    BranchCut(Error),
    /// The edge's gated merge failed. Nothing merged, nothing written.
    Merge {
        id: String,
        from: String,
        to: String,
        error: Box<Error>,
    },
    /// The status was written but its move event couldn't be appended, so the
    /// status was rolled back (`rollback` is `Some` when that failed too).
    EventAppend {
        id: String,
        from: Column,
        to: Column,
        error: Box<Error>,
        rollback: Option<Box<Error>>,
    },
}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MoveError::Move(e) | MoveError::LoadProject(e) | MoveError::BranchCut(e) => {
                write!(f, "{e}")
            }
            MoveError::Merge {
                id,
                from,
                to,
                error,
            } => write!(
                f,
                "merge for `{id}` failed; leaving it in `{from}` \
                 (NOT advancing to `{to}`): {error}"
            ),
            MoveError::EventAppend {
                id,
                from,
                to,
                error,
                rollback: None,
            } => write!(
                f,
                "moved {id} to {to}, but failed to append issue event ({error}); \
                 rolled back to {from}. Fix events.log permissions or restart the \
                 Shelbi daemon, then retry the move"
            ),
            MoveError::EventAppend {
                id,
                from,
                to,
                error,
                rollback: Some(re),
            } => write!(
                f,
                "moved {id} to {to}, but failed to append issue event ({error}); \
                 rollback to {from} also failed ({re}). Fix events.log permissions, \
                 then run `shelbi issue move {id} --to {from}` or retry the intended move"
            ),
        }
    }
}

impl std::error::Error for MoveError {}

/// Move issue `req.id` to status `req.to`, running the workflow transition's
/// actions on the way. See the [module docs](self) for the step order.
///
/// Blocks on git / GitHub for a merge edge, so a UI caller runs it off its
/// event loop. Callers gate on the hub daemon version themselves, before
/// calling in.
pub fn move_issue(
    req: &MoveRequest<'_>,
    warn: &mut dyn FnMut(MoveWarning),
) -> std::result::Result<MoveOutcome, MoveError> {
    let store = shelbi_state::issue_store_for(req.project).map_err(MoveError::Move)?;
    move_issue_with(store.as_ref(), &GitTransitionRunner, req, warn)
}

/// [`move_issue`] against an explicit store and [`TransitionRunner`].
pub fn move_issue_with(
    store: &dyn IssueStore,
    runner: &dyn TransitionRunner,
    req: &MoveRequest<'_>,
    warn: &mut dyn FnMut(MoveWarning),
) -> std::result::Result<MoveOutcome, MoveError> {
    let MoveRequest {
        project,
        id,
        reason,
        skip_transition_actions,
        ..
    } = *req;
    let tf = store
        .get(id)
        .map_err(MoveError::Move)?
        .ok_or_else(|| MoveError::Move(Error::Other(format!("issue `{id}` not found"))))?;
    let workflow = resolve_issue_workflow(project, &tf.task, warn);
    // Resolve the destination against the issue's workflow. An issue's position
    // is a status id, so ANY status the workflow declares is a valid target
    // — including `canceled` / archived and any status a user adds. A target
    // the workflow doesn't declare errors, naming the declared statuses.
    let column = resolve_move_target(&workflow, req.to).map_err(MoveError::Move)?;

    // Status ids for the edge we're crossing (used to fire the edge's
    // transition actions below). `column` is the target, `tf.task.column`
    // the current position.
    let from_status = tf.task.column.as_str().to_string();
    let to_status = column.as_str().to_string();
    // Does this edge declare a `merge`? An accept move (e.g. `review -> done`)
    // must actually integrate the branch, not just re-color the card. The
    // merge is GATED before the column move (below) so a failed or absent
    // merge never leaves the board reading the target status with an open PR
    // / unmerged branch, and the edge's remaining actions (`delete_branch`,
    // …) fire after the move.
    //
    // `skip_transition_actions` forces this off: the recovery escape hatch
    // advances the card WITHOUT running the merge (or any other action), for
    // when the git work already landed out of band and the merge would
    // re-fail. `declares_merge` gates the gated-merge block AND the post-move
    // cleanup below, so clearing it here is enough to bypass the whole action
    // list — the move is stamped `actions=skipped` in the event log instead.
    let declares_merge = !skip_transition_actions
        && column != tf.task.column
        && workflow
            .actions_for_transition(&from_status, &to_status)
            .contains(&TransitionAction::Merge);

    // Lifecycle hook: a move INTO `in_progress` cuts the issue's branch on
    // the hub (with depends_on awareness — see `crate::lifecycle`) and
    // persists `branch:` onto the issue. Skip when the destination matches the
    // current column, so a move to `in_progress` of an already-in-progress
    // issue doesn't run the cut for no reason. A failure inside the cut
    // (e.g. depends_on names a branch that hasn't been pushed yet) DOES
    // abort the move — silently dropping the depends_on intent and
    // shipping the card to in_progress without a usable branch would be
    // the worst of both worlds.
    if column == Column::in_progress() && tf.task.column != Column::in_progress() {
        let project_yaml = shelbi_state::load_project(project).map_err(MoveError::LoadProject)?;
        runner
            .cut_branch(&project_yaml, id)
            .map_err(MoveError::BranchCut)?;
    }

    // Gated merge for an accept edge. Integrate the branch (via the PR when
    // one is open — the path a protected `main` accepts) BEFORE the card
    // moves. A failed merge emits a `merge … status=failed` event and aborts
    // the move: the issue stays put rather than showing the target status with
    // nothing merged. The project is loaded once here and reused for the
    // post-move cleanup.
    let gated = if declares_merge {
        let project_yaml = shelbi_state::load_project(project).map_err(MoveError::LoadProject)?;
        let ws_label = tf
            .task
            .assigned_to
            .clone()
            .unwrap_or_else(|| req.workspace_fallback.to_string());
        let edge = TransitionEdge {
            project: &project_yaml,
            project_name: project,
            issue: &tf,
            workflow: &workflow,
            from: &from_status,
            to: &to_status,
        };
        let merge = runner
            .gated_merge(&edge, &ws_label)
            .map_err(|error| MoveError::Merge {
                id: id.to_string(),
                from: from_status.clone(),
                to: to_status.clone(),
                error: Box::new(error),
            })?;
        Some((project_yaml, merge))
    } else {
        None
    };

    let moved = store
        .move_status(id, &column, reason)
        .map_err(MoveError::Move)?;
    if let Some(mv) = &moved {
        // Stamp `actions=skipped` on the line when the escape hatch bypassed
        // the transition's actions, so the board history stays honest that
        // side effects were NOT run — kept distinct from the caller's `reason`.
        let append = if skip_transition_actions {
            shelbi_state::append_task_event_actions_skipped
        } else {
            shelbi_state::append_task_event
        };
        if let Err(error) = append(
            project,
            id,
            &mv.workflow,
            mv.from.clone(),
            mv.to.clone(),
            reason,
        ) {
            let rollback = store
                .move_status(id, &mv.from, "rollback:event-append-failed")
                .err()
                .map(Box::new);
            return Err(MoveError::EventAppend {
                id: id.to_string(),
                from: mv.from.clone(),
                to: mv.to.clone(),
                error: Box::new(error),
                rollback,
            });
        }
    }

    // Merge already landed and gated the move above; now fire the edge's
    // remaining actions (`delete_branch`, `run:`/`ready:`), skipping the ones
    // the gate already ran (the pre-merge prefix plus `merge`) so none is
    // re-run. Best-effort — the move already happened, so a cleanup failure
    // warns rather than rolling it back.
    let mut merge = None;
    if let Some((project_yaml, gated_merge)) = gated {
        if moved.is_some() {
            // `declares_merge` guarantees the gate returned `Some`, but fall
            // back to skipping just `merge` rather than unwrapping.
            let skip = gated_merge
                .as_ref()
                .map(|gm| gm.ran.clone())
                .unwrap_or_else(|| vec![TransitionAction::Merge]);
            let edge = TransitionEdge {
                project: &project_yaml,
                project_name: project,
                issue: &tf,
                workflow: &workflow,
                from: &from_status,
                to: &to_status,
            };
            if let Err(error) = runner.remaining_actions(&edge, &skip) {
                warn(MoveWarning::PostMergeCleanup {
                    id: id.to_string(),
                    error,
                });
            }
        }
        merge = gated_merge;
    }

    Ok(MoveOutcome {
        column,
        moved: moved.is_some(),
        merge,
    })
}

/// Load the workflow assigned to `issue`. Project defaults are resolved via
/// project config; a workflow that can't be loaded — whatever the reason —
/// falls back to the canonical default workflow with a warning.
///
/// The fail-soft is deliberate and load-bearing: this sits on the status
/// transition path, and a workflow YAML can be absent through no fault of the
/// project's config — a stale daemon managing an older state layout the CLI
/// doesn't expect (the observed field failure: a 0.1 daemon under a 0.3.2
/// CLI), or an in-repo `<repo>/.shelbi/workflows/` momentarily blipped by a
/// git checkout. Hard-failing here froze the whole board from the CLI (bare
/// `io: ENOENT`, no state change) while the poller — whose transition path
/// already swallows this load — kept working. Falling back mirrors the
/// poller's behavior; the warning keeps a genuinely misconfigured workflow
/// loud.
fn resolve_issue_workflow(
    project: &str,
    issue: &Issue,
    warn: &mut dyn FnMut(MoveWarning),
) -> Workflow {
    let project_yaml = shelbi_state::load_project(project).ok();
    let name = project_yaml
        .as_ref()
        .map(|p| shelbi_state::resolve_task_workflow_name(p, issue))
        .unwrap_or_else(|| issue.workflow_or_default());
    match shelbi_state::load_workflow(project, name) {
        Ok(wf) => wf,
        Err(error) => {
            warn(MoveWarning::WorkflowFallback {
                name: name.to_string(),
                error,
            });
            default_workflow()
        }
    }
}

/// Resolve a move's target status against the issue's workflow, returning the
/// target position (a status id).
///
/// An issue's position is a status id, so any status the workflow declares
/// is a reachable target — including `canceled` / archived statuses and
/// any status a user adds later. `to` is matched against the declared
/// status ids, first through the same alias normalization a stored
/// position gets (so `wip` / `in_progress` resolve onto `in-progress`),
/// then verbatim against the raw declared ids (for custom ids the
/// normalizer passes through untouched). A `to` the workflow doesn't
/// declare errors, listing the ids it does.
fn resolve_move_target(workflow: &Workflow, to: &str) -> Result<Column> {
    // Alias-normalized lookup: folds the friendly CLI spellings onto the
    // canonical id before checking the workflow.
    let normalized = Column::from_status_id(to);
    if let Some(status) = workflow.status(normalized.as_str()) {
        return Ok(Column::from_status_id(&status.id));
    }
    // Verbatim lookup: a custom id the normalizer left untouched still has
    // to match a declared status id exactly (modulo surrounding whitespace).
    if let Some(status) = workflow.statuses.iter().find(|s| s.id == to.trim()) {
        return Ok(Column::from_status_id(&status.id));
    }

    let valid = workflow
        .statuses
        .iter()
        .map(|s| s.id.clone())
        .collect::<Vec<_>>()
        .join(", ");
    Err(Error::Other(format!(
        "`{to}` is not a status in workflow `{}` (valid: {valid})",
        workflow.name,
    )))
}

#[cfg(test)]
#[path = "transition_move_tests.rs"]
mod tests;
