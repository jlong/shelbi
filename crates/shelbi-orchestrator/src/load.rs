//! Generic task-load onto a tag-matched workspace.
//!
//! The workspace-neutral replacement for the retired review-specific load
//! path: given a task, resolve the status it currently sits in, take that
//! status's **required tags**, and load the task onto a free workspace whose
//! [effective tags](shelbi_core::Project::effective_tags) are a superset of
//! them — then dispatch the status's `agent:` there. Nothing here branches on
//! the name "review"; a status that declares `tags: [review]` routes to
//! `review`-tagged workspaces purely by the generic superset query.
//!
//! Serving is a separate concern: it comes from the status's enter-transition
//! `run:` / `ready:` commands (Phase 1), fired when the task moves into the
//! status — not from this loader.

use std::collections::{BTreeSet, HashSet};

use shelbi_core::{Column, Error, Result};

use crate::branch;
use crate::workspace::{start_workspace_on_task, StartSpec};

/// Load `task_id` onto a free workspace whose effective tags satisfy the
/// task's current status's required tags, dispatching that status's agent.
/// Returns the tmux target (`session:window`) of the pane the caller should
/// focus.
///
/// The workspace is chosen by:
/// 1. reusing the slot this task is already assigned to, if it still matches;
/// 2. otherwise the first free (not holding another active task) matching
///    workspace in declaration order.
///
/// Fails when no declared workspace matches the required tags, or when every
/// matching workspace is busy. The assignment is persisted before dispatch and
/// rolled back if the dispatch fails, so a failed load never strands the card
/// pinned to a workspace that isn't running.
pub fn load_task_by_id(project_name: &str, task_id: &str) -> Result<String> {
    let project = shelbi_state::load_project(project_name)?;
    let mut tf = shelbi_state::load_task(project_name, task_id)?;

    // Resolve the status the task currently sits in and its routing tags +
    // agent. A missing/invalid workflow falls back to the built-in default
    // (no required tags → any free workspace), so a transient config typo
    // doesn't wedge the load.
    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let status_id = tf.task.column.as_str();
    let status = workflow.status(status_id);
    let required: BTreeSet<String> = status
        .map(|s| s.tags.iter().cloned().collect())
        .unwrap_or_default();
    let agent = status.and_then(|s| s.agent.clone());

    let candidates = project.workspaces_matching(&required);
    if candidates.is_empty() {
        return Err(Error::Other(format!(
            "no workspace matches the tags {required:?} required by status \
             `{status_id}` — declare one (e.g. `tags: {required:?}`) or drop the \
             requirement from the workflow status"
        )));
    }

    // Busy = holding some *other* active (in-progress / handoff) task.
    let mut active = shelbi_state::list_column(project_name, Column::in_progress())?;
    active.extend(shelbi_state::list_column(project_name, Column::review())?);
    let busy: HashSet<&str> = active
        .iter()
        .filter(|t| t.task.id != task_id)
        .filter_map(|t| t.task.assigned_to.as_deref())
        .collect();

    let chosen = candidates
        .iter()
        .find(|w| tf.task.assigned_to.as_deref() == Some(w.name.as_str()))
        .or_else(|| candidates.iter().find(|w| !busy.contains(w.name.as_str())))
        .ok_or_else(|| {
            Error::Other(format!(
                "every workspace matching {required:?} is busy — free one or wait"
            ))
        })?;
    let ws = (*chosen).clone();

    let branch = branch::branch_name_for_task(&project, Some(&workflow), &tf.task)?;

    // Persist the assignment before dispatch so a concurrent load can't pick
    // the same slot, and roll it back on a dispatch failure.
    let original = tf.task.clone();
    tf.task.assigned_to = Some(ws.name.clone());
    tf.task.branch = Some(branch.clone());
    tf.task.updated_at = chrono::Utc::now();
    shelbi_state::save_task(project_name, &tf.task, &tf.body)?;

    let addr = match start_workspace_on_task(StartSpec {
        project: &project,
        workspace: &ws,
        task_id,
        branch: &branch,
        task_body: &tf.body,
        agent: agent.as_deref(),
    }) {
        Ok(addr) => addr,
        Err(e) => {
            if let Err(re) = shelbi_state::save_task(project_name, &original, &tf.body) {
                eprintln!(
                    "shelbi: load for `{task_id}` failed and the assignment rollback \
                     also failed ({re}); run `shelbi task assign {task_id} --to \
                     <workspace>` to fix the board"
                );
            }
            return Err(e);
        }
    };

    Ok(addr.target())
}
