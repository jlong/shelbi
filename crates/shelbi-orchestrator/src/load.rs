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

use std::collections::{BTreeMap, BTreeSet, HashSet};

use shelbi_core::{Column, Error, Project, Result, Issue, WorkspaceSpec, Workflow};
use shelbi_state::{IssueFile, ReviewLoadFailure};

use crate::branch;
use crate::supervision::{BASE_BACKOFF, CRASH_LOOP_WINDOW, MAX_RESTARTS_IN_WINDOW};
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
    // Review activation persists assignment/branch and starts a workspace.
    // Both sidebar and palette converge here, so keep the mismatch guard at
    // this shared boundary rather than relying on every UI surface to remember.
    shelbi_state::ensure_daemon_matches_for_mutation()?;
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::issue_store_for_project(&project)?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;

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
    let mut active = store.list_in_status(&Column::in_progress())?;
    active.extend(store.list_in_status(&Column::review())?);
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

    dispatch_task_onto(project_name, &project, &workflow, tf, &ws, agent)
}

/// Idle `review`-tagged workspaces for `project_name`, in declaration order.
///
/// "Idle" = not currently assigned an active (in-progress or review-column)
/// task. The sidebar's "Load onto a review workspace?" confirm dialog reads
/// this to pick the slot it will load onto — and to report "none free" when
/// every review slot is busy. Kept beside [`load_review_task`] so the busy
/// definition (the same in-progress + review scan the generic loader uses)
/// lives in one place.
pub fn free_review_workspaces(project_name: &str) -> Result<Vec<WorkspaceSpec>> {
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::issue_store_for_project(&project)?;
    let review_tag: BTreeSet<String> = std::iter::once("review".to_string()).collect();
    let mut active = store.list_in_status(&Column::in_progress())?;
    active.extend(store.list_in_status(&Column::review())?);
    let busy: HashSet<&str> = active
        .iter()
        .filter_map(|t| t.task.assigned_to.as_deref())
        .collect();
    Ok(project
        .workspaces_matching(&review_tag)
        .into_iter()
        .filter(|w| !busy.contains(w.name.as_str()))
        .cloned()
        .collect())
}

/// One review-tagged workspace and the active task (if any) currently loaded
/// on it. The sidebar's "load onto which review workspace?" picker reads this
/// to list *every* review slot with its free/occupied state — the superset of
/// [`free_review_workspaces`], which drops the occupied ones. Eviction (below)
/// then lets a load target an occupied slot, so the picker must show them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSlot {
    /// The `review`-tagged workspace name.
    pub name: String,
    /// The task currently loaded on this slot, or `None` when it's free.
    pub occupant: Option<ReviewSlotOccupant>,
}

/// The task currently occupying a [`ReviewSlot`] — its id (what eviction acts
/// on) and title (what the picker shows in quotes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSlotOccupant {
    pub task_id: String,
    pub title: String,
}

/// Every `review`-tagged workspace for `project_name`, in declaration order,
/// each paired with the active task currently loaded on it (`None` when free).
///
/// The picker/confirm dialog's data source: unlike [`free_review_workspaces`]
/// it lists occupied slots too, so a human can deliberately reuse one (evicting
/// its current task back to the queue). "Occupied" uses the same in-progress +
/// review-column scan the busy check does, so the two never disagree.
pub fn review_slots(project_name: &str) -> Result<Vec<ReviewSlot>> {
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::issue_store_for_project(&project)?;
    let mut active = store.list_in_status(&Column::in_progress())?;
    active.extend(store.list_in_status(&Column::review())?);
    Ok(review_slots_from(&project, &active))
}

/// Pure slot enumeration for [`review_slots`]: map each `review`-tagged
/// workspace (declaration order) to the active task assigned to it. Split out
/// with no I/O so the free/occupied labeling is unit-testable on in-memory
/// fixtures.
fn review_slots_from(project: &Project, active: &[IssueFile]) -> Vec<ReviewSlot> {
    let review_tag: BTreeSet<String> = std::iter::once("review".to_string()).collect();
    project
        .workspaces_matching(&review_tag)
        .into_iter()
        .map(|w| ReviewSlot {
            name: w.name.clone(),
            occupant: active
                .iter()
                .find(|tf| tf.task.assigned_to.as_deref() == Some(w.name.as_str()))
                .map(|tf| ReviewSlotOccupant {
                    task_id: tf.task.id.clone(),
                    title: tf.task.title.clone(),
                }),
        })
        .collect()
}

/// Load a Queued-for-Review task onto a *specific* review workspace.
///
/// The workspace-targeted counterpart to [`load_task_by_id`]: the caller (the
/// sidebar's confirm dialog) has already picked a free `review`-tagged slot
/// from [`free_review_workspaces`], so this never consults — and never
/// re-seeds — the task's dev `assigned_to`. That distinction is the whole
/// point. A handoff task sitting in Review still carries the dev workspace
/// that built it in `assigned_to`; the generic loader's "reuse the assigned
/// slot" step would bounce it straight back to that dev pane. Here the target
/// is explicit, so the dev workspace is never a candidate.
///
/// Validates that `workspace_name` is a declared `review`-tagged slot, then
/// reassigns the task, resolves the branch, and dispatches the status's agent
/// — persisting the assignment before dispatch and rolling it back on failure,
/// exactly as [`load_task_by_id`] does.
pub fn load_review_task(project_name: &str, task_id: &str, workspace_name: &str) -> Result<String> {
    shelbi_state::ensure_daemon_matches_for_mutation()?;
    // Serialize the claim against the daemon auto-loader and any other manual
    // load so two evaluations can't load two tasks onto one slot (or this task
    // onto two). The guards inside the locked body then reject a slot already
    // taken, or a task already serving elsewhere, that a race snuck in.
    let _guard = shelbi_state::lock_review_load(project_name)?;
    load_review_task_locked(project_name, task_id, workspace_name)
}

/// Load `task_id` onto `workspace_name`, first **evicting** whatever other task
/// currently occupies that review slot back to the review queue.
///
/// The reuse-an-occupied-slot counterpart to [`load_review_task`]: the sidebar
/// picker lets a human target a slot already serving another task, so this
/// clears the occupant (un-serves it and drops its review-slot assignment, so
/// it re-appears as Queued/Pending — not accepted or rejected) *before* the
/// [`load_review_task_locked`] busy guard would otherwise reject the slot. Both
/// the eviction and the load run under one hold of the review-load lock, so no
/// concurrent claim can slip a third task onto the freed slot in between.
///
/// A no-op eviction (the slot is free, or already holds `task_id` itself — the
/// same-slot resume case) leaves this identical to [`load_review_task`], which
/// is why the non-evicting callers can route through it unchanged.
pub fn load_review_task_evicting(
    project_name: &str,
    task_id: &str,
    workspace_name: &str,
) -> Result<String> {
    shelbi_state::ensure_daemon_matches_for_mutation()?;
    let _guard = shelbi_state::lock_review_load(project_name)?;
    evict_review_slot_locked(project_name, workspace_name, task_id)?;
    load_review_task_locked(project_name, task_id, workspace_name)
}

/// Return whatever review-column task currently occupies `workspace_name`
/// (other than `keep`) to the review queue, so the caller can load a new task
/// onto the freed slot. Returns the evicted task's id, or `None` when the slot
/// is free / already holds `keep`.
///
/// "Return to the queue" mirrors how a review slot is otherwise cleared: tear
/// the serving session down ([`crate::review_ui::close_review_window`], the
/// same close the accept path uses) and drop the task's review-slot
/// `assigned_to` so [`crate::load`]'s split reads it as Queued/Pending again.
/// The task stays in the Review column — it is neither accepted nor rejected,
/// just un-loaded. The teardown is best-effort (a missing/dead window must not
/// block the eviction's authoritative board change); the assignment clear is
/// not. Runs with the review-load lock already held (see
/// [`load_review_task_evicting`]).
fn evict_review_slot_locked(
    project_name: &str,
    workspace_name: &str,
    keep: &str,
) -> Result<Option<String>> {
    let store = shelbi_state::issue_store_for(project_name)?;
    // Only review-column tasks are "loaded for review"; an in-progress task on
    // the slot (an odd state) isn't ours to bounce back to the review queue —
    // leave it for `load_review_task_locked`'s busy guard to reject.
    let review = store.list_in_status(&Column::review())?;
    let Some(occupant) = review
        .into_iter()
        .find(|tf| tf.task.id != keep && tf.task.assigned_to.as_deref() == Some(workspace_name))
    else {
        return Ok(None);
    };
    let evicted_id = occupant.task.id.clone();

    // Tear down the occupant's serving window first, while its `assigned_to`
    // still names the slot (that's what `close_review_window` derives from).
    // Best-effort: a slot whose window already died (or a test env with no
    // tmux) must still have its board state cleared below.
    if let Err(e) = crate::review_ui::close_review_window(project_name, &evicted_id) {
        tracing::warn!(
            project = %project_name,
            task = %evicted_id,
            workspace = %workspace_name,
            error = %e,
            "review-slot eviction: tearing down the occupant's window failed; \
             clearing its assignment anyway",
        );
    }

    // Drop the review-slot assignment, keeping the task in Review → it
    // re-appears as Queued/Pending for a later free slot or another manual load.
    // `set_fields` does the locked load→clear→save, so no separate read is
    // needed to keep the body authoritative.
    store.set_fields(
        &evicted_id,
        shelbi_state::IssueFields {
            assigned_to: Some(None),
            ..Default::default()
        },
    )?;

    let _ = shelbi_state::append_dispatch_event(
        &evicted_id,
        workspace_name,
        "review-evict",
        "returned to review queue so the slot could be reused",
    );
    Ok(Some(evicted_id))
}

/// The body of [`load_review_task`], run with the project-scoped review-load
/// lock already held. Split out so the daemon auto-loader
/// ([`autoload_review_queue`]) can hold the lock once across a whole batch of
/// loads and call this per task without re-acquiring it (a second `flock` on the
/// same file from the same process would deadlock, not recurse).
fn load_review_task_locked(
    project_name: &str,
    task_id: &str,
    workspace_name: &str,
) -> Result<String> {
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::issue_store_for_project(&project)?;
    let ws = project
        .workspace(workspace_name)
        .filter(|w| project.effective_tags(w).contains("review"))
        .cloned()
        .ok_or_else(|| {
            Error::Other(format!(
                "`{workspace_name}` is not a declared review-tagged workspace"
            ))
        })?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;

    // Guard: this task is already assigned to a review slot *other than* the
    // target. A race (the auto-loader placed it between a human opening the
    // confirm and pressing Enter) must not re-dispatch it onto a *second*
    // slot. Reject rather than move it — the card is already loaded where it
    // is. Re-loading onto the SAME slot it already owns is NOT a conflict: it
    // is a resume of a stranded slot (its pane died on `quit`/crash while the
    // assignment persisted on disk), and `start_workspace_on_task` kills any
    // stale pane and relaunches, so a same-slot re-load is safe and
    // idempotent. Callers only reach it for a genuinely dead slot — the
    // auto-loader gates on pane liveness, and the sidebar only re-loads a
    // Ready row whose window needs launching.
    if let Some(other) = conflicting_review_slot(&project, &tf.task, workspace_name) {
        return Err(Error::Other(format!(
            "`{task_id}` is already loaded on review slot `{other}`"
        )));
    }

    // Guard: the target slot is already serving a *different* active task. The
    // slot-selection that picked it may have raced another claim; refuse rather
    // than clobber the pane already running there.
    if review_slot_busy_with_other(project_name, workspace_name, task_id)? {
        return Err(Error::Other(format!(
            "review slot `{workspace_name}` is already serving another task"
        )));
    }

    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let agent = workflow
        .status(tf.task.column.as_str())
        .and_then(|s| s.agent.clone());
    dispatch_task_onto(project_name, &project, &workflow, tf, &ws, agent)
}

/// If `task` is currently assigned to a review slot *other than* `target`,
/// return that slot's name — loading here would give the same task a second,
/// conflicting review slot and must be rejected.
///
/// Returns `None` when the task is unassigned, assigned to a non-review (dev)
/// slot, or assigned to `target` itself. That last case is the load-bearing
/// one: a task whose `assigned_to` already names `target` is being re-loaded
/// onto the SAME slot it owns — a resume of a stranded review slot whose pane
/// died on `quit`/crash — which is allowed, not a double-load.
fn conflicting_review_slot(project: &Project, task: &Issue, target: &str) -> Option<String> {
    task.assigned_to
        .as_deref()
        .filter(|name| *name != target)
        .and_then(|name| project.workspace(name))
        .filter(|w| project.effective_tags(w).contains("review"))
        .map(|w| w.name.clone())
}

/// True iff `workspace_name` is currently assigned to an active
/// (in-progress or review-column) task other than `task_id`. The same "busy"
/// definition [`free_review_workspaces`] uses, but asked of one slot — the
/// race guard for a targeted load.
fn review_slot_busy_with_other(
    project_name: &str,
    workspace_name: &str,
    task_id: &str,
) -> Result<bool> {
    let store = shelbi_state::issue_store_for(project_name)?;
    let mut active = store.list_in_status(&Column::in_progress())?;
    active.extend(store.list_in_status(&Column::review())?);
    Ok(active.iter().any(|t| {
        t.task.id != task_id && t.task.assigned_to.as_deref() == Some(workspace_name)
    }))
}

/// Auto-load queued review tasks onto idle review slots, one claim per free
/// slot in board order, until slots or queued tasks run out. Returns the
/// `(task, workspace)` pairs actually loaded.
///
/// This is the daemon's headless equivalent of a human pressing Enter on each
/// Queued-for-Review row: it re-derives state from disk (task board +
/// assignments), so it works identically on a fresh poller tick, after
/// `shelbi reload`, and after `shelbi quit` + restart — no live TUI session
/// need have witnessed anything. "Queued" is a review-column task not already
/// assigned to a review-tagged slot (a handoff card still pinned to the dev
/// slot that built it, or one with no assignment yet); a task already serving
/// on a review slot is skipped. It holds the project-scoped review-load lock
/// across the whole batch and dispatches through the same
/// [`load_review_task_locked`] the manual path uses, so the emitted events and
/// the booted Review agent are identical, and a concurrent manual Enter can't
/// interleave to double-load a slot.
pub fn autoload_review_queue(project_name: &str) -> Result<Vec<AutoLoadedReview>> {
    shelbi_state::ensure_daemon_matches_for_mutation()?;
    // One lock across the whole batch: the free slots computed below stay valid
    // for the duration because no other claim (manual or a second tick) can
    // proceed until we release it.
    let _guard = shelbi_state::lock_review_load(project_name)?;

    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::issue_store_for_project(&project)?;
    // Require a WARM board read before consuming a scarce review slot. A stale
    // snapshot (a rate-limit park serving the last board), a cold cache, or a
    // failed read cannot prove a task is still sitting in review awaiting a slot;
    // auto-loading off one would dispatch against untrusted state. Skip the tick
    // and retry when the board is warm again — the same "no destructive action
    // on a stale read" rule the poller's reapers follow.
    let review_tasks: Vec<IssueFile> = match store.list_state()? {
        shelbi_state::BoardState::Warm(board) => board
            .into_iter()
            // Board order (priority, then id) — the same order the sidebar shows.
            .filter(|tf| tf.task.column == Column::review())
            .collect(),
        shelbi_state::BoardState::Stale(_) | shelbi_state::BoardState::Cold => {
            return Ok(Vec::new())
        }
    };
    // Only tasks whose workflow review status is *review-tagged* may be
    // auto-grabbed onto a scarce review slot. A handoff status that declares no
    // `review` tag (an orchestrator-owned `review` status, the bare default
    // workflow) is left in the review column for a human sidebar load — routing
    // it here would consume a review slot purely for being idle, diverging from
    // the model where loading onto a review workspace is a deliberate action.
    let eligible: Vec<IssueFile> = review_tasks
        .into_iter()
        .filter(|tf| {
            let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
                .unwrap_or_else(|_| shelbi_core::default_workflow());
            status_routes_to_review(&workflow, tf.task.column.as_str())
        })
        .collect();
    // Idle review slots in declaration order (never lists a dev slot).
    let free = free_review_workspaces(project_name)?;
    // Tasks an operator deliberately unloaded (`shelbi workspace stop` /
    // `task unassign`). Skipped so a parked task stays unloaded instead of
    // being re-grabbed on the next tick.
    let parked = shelbi_state::parked_review_tasks(project_name)?;
    // Per-`(task, slot)` failure ledger: a load that keeps failing for a
    // durable reason (its branch checked out in another worktree, say) backs
    // off and eventually gives up instead of retrying identically forever. Read
    // once for the whole batch; the planner drops any pair still inside its
    // backoff or already given up.
    let failures = shelbi_state::review_load_failures(project_name)?;
    let now_secs = chrono::Utc::now().timestamp();
    let plan = plan_review_autoload(&eligible, &project, &free, &parked, &failures, now_secs);
    if plan.is_empty() {
        return Ok(Vec::new());
    }

    let mut loaded = Vec::with_capacity(plan.len());
    for (task_id, workspace) in plan {
        // Mirror the manual path's observable event exactly (`dispatch task=…
        // workspace=… status=review-load …`) so the two are indistinguishable
        // in `events.log`.
        let _ = shelbi_state::append_dispatch_event(
            &task_id,
            &workspace,
            "review-load",
            "auto-loading branch onto idle review slot",
        );
        match load_review_task_locked(project_name, &task_id, &workspace) {
            Ok(_) => loaded.push(AutoLoadedReview {
                task_id,
                workspace,
            }),
            Err(e) => {
                // Surface the failure in events.log, not just the logs: an
                // auto-load that rejects a slot (busy, conflicting assignment)
                // or fails to dispatch is otherwise invisible to the
                // orchestrator, which is exactly the "no observable event"
                // gap that let a stalled review-load go unnoticed. The
                // dispatch primitive already logs sync/branch failures; this
                // covers every other rejection before it.
                let _ = shelbi_state::append_dispatch_event(
                    &task_id,
                    &workspace,
                    "review-load-failed",
                    &e.to_string(),
                );
                tracing::warn!(
                    project = %project_name,
                    task = %task_id,
                    workspace = %workspace,
                    error = %e,
                    "auto review-load failed for one slot",
                );
                // Record the failure so a durably-failing pair backs off and
                // eventually gives up, instead of the planner re-deriving the
                // same doomed attempt every ~5s tick forever. On the attempt
                // that trips the crash-loop cap, emit ONE `supervision=gave-up`
                // line (the same shape the stranded-slot resume path emits) so
                // the orchestrator gets one actionable signal, then stay quiet.
                let prior = failures
                    .get(&task_id)
                    .and_then(|m| m.get(&workspace))
                    .cloned()
                    .unwrap_or_default();
                let (updated, gave_up_now) = note_autoload_failure(&prior, now_secs);
                if let Err(re) = shelbi_state::record_review_load_failure(
                    project_name,
                    &task_id,
                    &workspace,
                    &updated,
                ) {
                    tracing::warn!(
                        project = %project_name,
                        task = %task_id,
                        workspace = %workspace,
                        error = %re,
                        "recording review-load failure for backoff failed",
                    );
                }
                if gave_up_now {
                    let _ = shelbi_state::append_supervision_event(
                        project_name,
                        Some(&workspace),
                        "gave-up",
                        "review-load-crash-loop",
                    );
                    tracing::warn!(
                        project = %project_name,
                        task = %task_id,
                        workspace = %workspace,
                        "gave up auto-loading review slot after the crash-loop cap; left for the user",
                    );
                }
            }
        }
    }
    Ok(loaded)
}

/// True iff `workflow`'s status `status_id` carries the `review` tag — the gate
/// for whether the auto-loader may consume a review slot for a task sitting in
/// that status.
///
/// The auto-loader's counterpart to the generic superset match in
/// [`load_task_by_id`]: a review status that declares `tags: [review]` routes
/// to review slots, so a handed-off task in it is auto-served; one that does not
/// (an orchestrator-owned `user` handoff status, the bare default workflow) must
/// NOT be auto-grabbed onto a scarce review slot merely for being idle — it
/// stays in the review column for a deliberate human sidebar load.
fn status_routes_to_review(workflow: &Workflow, status_id: &str) -> bool {
    workflow
        .status(status_id)
        .is_some_and(|s| s.tags.iter().any(|t| t == "review"))
}

/// Whether a retry of a `(task, slot)` auto-load is currently suppressed, given
/// its failure record and the current wall-clock (`now_secs`, unix seconds).
///
/// Suppressed while the pair has given up (latched — leave it for the user), or
/// while it is still inside the exponential backoff since its last failed
/// attempt (`BASE_BACKOFF * 2^(attempts-1)`), so no identical attempt is
/// re-emitted more than once per backoff window. A record whose last attempt is
/// older than [`CRASH_LOOP_WINDOW`] has aged out and is not suppressed: its
/// counter restarts on the next failure, so a slow drip never accumulates into
/// a give-up.
fn autoload_retry_suppressed(rec: &ReviewLoadFailure, now_secs: i64) -> bool {
    if rec.gave_up {
        return true;
    }
    if rec.attempts == 0 {
        return false;
    }
    let since = now_secs.saturating_sub(rec.last_attempt);
    if since >= CRASH_LOOP_WINDOW.as_secs() as i64 {
        return false;
    }
    // `attempts` is capped by the give-up latch above (a given-up record never
    // reaches here), but clamp the shift regardless so a hand-edited/corrupt
    // record can't overflow.
    let shift = (rec.attempts - 1).min(16);
    let wait = BASE_BACKOFF.as_secs() as i64 * (1i64 << shift);
    since < wait
}

/// Fold a fresh failed attempt at `now_secs` into `rec`, returning the updated
/// record and whether this failure *trips* give-up (so the caller emits the
/// gave-up event exactly once).
///
/// An attempt landing outside [`CRASH_LOOP_WINDOW`] resets the counter first —
/// only a genuine tight loop trips the cap. Give-up latches once the attempt
/// count reaches [`MAX_RESTARTS_IN_WINDOW`]; `trips` is true only on the
/// transition into give-up, never again, so the event fires once.
fn note_autoload_failure(rec: &ReviewLoadFailure, now_secs: i64) -> (ReviewLoadFailure, bool) {
    let aged_out = rec.attempts > 0
        && now_secs.saturating_sub(rec.last_attempt) >= CRASH_LOOP_WINDOW.as_secs() as i64;
    let base = if aged_out { 0 } else { rec.attempts };
    let attempts = base.saturating_add(1);
    let gave_up = attempts >= MAX_RESTARTS_IN_WINDOW as u32;
    let trips = gave_up && !rec.gave_up;
    (
        ReviewLoadFailure {
            attempts,
            last_attempt: now_secs,
            gave_up,
        },
        trips,
    )
}

/// Pure slot-selection for [`autoload_review_queue`]: pair each queued review
/// task (board order) with one idle review slot (declaration order), capping at
/// `min(queued, free)`. "Queued" is a review-column task not already assigned to
/// a review-tagged slot — a handoff card still pinned to the dev slot that built
/// it, or one with no assignment; a task already serving on a review slot is
/// dropped. A task in `parked` (deliberately unloaded by the operator) is also
/// dropped, so a parked task stays unloaded instead of being re-grabbed on the
/// next tick.
///
/// A `(task, slot)` pair currently suppressed by the failure ledger (`failures`,
/// evaluated against `now_secs`) is skipped **for that slot only**: a pair in
/// backoff, or one that has given up, does not consume the slot, and the slot is
/// offered to the next queued task instead — so one durably-failing task never
/// blocks a slot other tasks could use, and the loader stops re-attempting the
/// same doomed pair every tick. Split out with no I/O so board order, capacity
/// limiting, the parked skip, and failure suppression are unit-testable on
/// in-memory fixtures.
fn plan_review_autoload(
    review_tasks: &[IssueFile],
    project: &Project,
    free: &[WorkspaceSpec],
    parked: &BTreeSet<String>,
    failures: &BTreeMap<String, BTreeMap<String, ReviewLoadFailure>>,
    now_secs: i64,
) -> Vec<(String, String)> {
    let queued: Vec<&IssueFile> = review_tasks
        .iter()
        .filter(|tf| !parked.contains(&tf.task.id))
        .filter(|tf| {
            let on_review_slot = tf
                .task
                .assigned_to
                .as_deref()
                .and_then(|name| project.workspace(name))
                .is_some_and(|w| project.effective_tags(w).contains("review"));
            !on_review_slot
        })
        .collect();

    // Greedy pairing: for each free slot (declaration order) take the first
    // still-unplaced queued task (board order) that isn't suppressed for THIS
    // slot. A pair suppressed for one slot stays available for a later one, so
    // suppression is truly per-`(task, slot)` rather than dropping the task
    // wholesale.
    let mut out: Vec<(String, String)> = Vec::new();
    let mut placed: HashSet<usize> = HashSet::new();
    for slot in free {
        for (i, tf) in queued.iter().enumerate() {
            if placed.contains(&i) {
                continue;
            }
            let suppressed = failures
                .get(&tf.task.id)
                .and_then(|m| m.get(&slot.name))
                .is_some_and(|rec| autoload_retry_suppressed(rec, now_secs));
            if suppressed {
                continue;
            }
            out.push((tf.task.id.clone(), slot.name.clone()));
            placed.insert(i);
            break;
        }
    }
    out
}

/// One task auto-loaded onto a review slot by [`autoload_review_queue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoLoadedReview {
    pub task_id: String,
    pub workspace: String,
}

/// Load `task_id` onto the review slot it should serve on, for callers that
/// only hold a task id (the command palette, the review-interface fallback).
///
/// The id-only counterpart to [`load_review_task`]: reuse the review-tagged
/// slot the task is already on, else the first free review slot. Unlike the
/// generic [`load_task_by_id`], it never reuses a task's *dev* `assigned_to`
/// (a handoff task still points at the workspace that built it) and never
/// depends on the workflow declaring `tags: [review]` on its handoff status —
/// the live `site`/`app` workflows don't. Routing purely through the
/// `review`-tag query keeps a review load off the dev slot, and dispatch
/// through [`dispatch_task_onto`] launches the Review agent.
pub fn load_task_for_review(project_name: &str, task_id: &str) -> Result<String> {
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::issue_store_for_project(&project)?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;
    let already = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|name| project.workspace(name))
        .filter(|w| project.effective_tags(w).contains("review"))
        .map(|w| w.name.clone());
    let target = match already {
        Some(name) => name,
        None => free_review_workspaces(project_name)?
            .into_iter()
            .next()
            .map(|w| w.name)
            .ok_or_else(|| {
                Error::Other(
                    "no free review workspace to load onto — free one or wait".to_string(),
                )
            })?,
    };
    load_review_task(project_name, task_id, &target)
}

/// Resolve which agent a load dispatches onto `ws`, given the workflow
/// status's declared `agent:` (`status_agent`).
///
/// A review-tagged workspace exists to *serve* the branch for a human to run —
/// that is the Review agent's job (install / build / boot / health-check), and
/// it explicitly does not rebase or open a PR. The status's `agent:` is NOT who
/// serves there: on a `user`-owned review status it is a Zen-automation hint
/// ("who may auto-accept under Zen", commonly `orchestrator`), which the
/// generic loader would otherwise dispatch onto the review slot — launching the
/// orchestrator/developer instead of the reviewer (the bug this fixes). So any
/// load onto a review slot dispatches the Review agent regardless of the
/// status's declared agent. Non-review loads keep the status's agent untouched.
fn dispatch_agent_for(
    project: &Project,
    ws: &WorkspaceSpec,
    status_agent: Option<String>,
) -> Option<String> {
    if project.effective_tags(ws).contains("review") {
        Some(shelbi_state::REVIEW_AGENT.to_string())
    } else {
        status_agent
    }
}

/// Persist the assignment of `tf`'s task to `ws`, resolve its branch, and
/// dispatch `agent` there. The assignment is written before dispatch so a
/// concurrent load can't grab the same slot, and rolled back if the dispatch
/// fails. Returns the tmux target (`session:window`) to focus. Shared by
/// [`load_task_by_id`] and [`load_review_task`].
fn dispatch_task_onto(
    project_name: &str,
    project: &Project,
    workflow: &Workflow,
    mut tf: IssueFile,
    ws: &WorkspaceSpec,
    agent: Option<String>,
) -> Result<String> {
    // Refuse to launch when the workflow's templated `base_branch` can't be
    // fully resolved from this task's frontmatter — a first-class guard at the
    // dispatch chokepoint, not an incidental side effect of branch naming. A
    // subtask filed without its `feature:`/`task:`/`update:` link leaves a
    // `{{var}}` in the base template unresolved; degrading the base (historically
    // to `main`) and launching anyway cuts the worker's branch from the wrong
    // base, and a later squash-merge into the parent can revert already-merged
    // sibling subtasks. Fail loudly, naming the missing field(s), before
    // persisting any assignment or touching a pane, so the task stays put in its
    // ready status. Scoped to a fresh cut (`branch` not yet pinned): a re-serve
    // / resume of an existing branch needs no base to cut from. `resolve_git`
    // returns `Ok(None)` for a workflow with no `git:` block and `Ok(Some(_))`
    // when the base resolves, so a fully-resolved task dispatches unchanged.
    if tf.task.branch.is_none() {
        workflow.resolve_git(&tf.task.string_params())?;
    }

    let branch = branch::branch_name_for_task(project, Some(workflow), &tf.task)?;

    let agent = dispatch_agent_for(project, ws, agent);
    let store = shelbi_state::issue_store_for_project(project)?;

    // Persist the assignment before dispatch so a concurrent load can't pick
    // the same slot, and roll it back on a dispatch failure. `set_fields` does
    // the locked read-modify-write, so a concurrent writer touching another
    // field on the same card can't be clobbered by this assignment.
    let original = tf.task.clone();
    tf.task.assigned_to = Some(ws.name.clone());
    tf.task.branch = Some(branch.clone());
    store.set_fields(
        &tf.task.id,
        shelbi_state::IssueFields {
            assigned_to: Some(Some(ws.name.clone())),
            branch: Some(Some(branch.clone())),
            ..Default::default()
        },
    )?;

    // Any fresh dispatch/assignment un-parks the task: an operator-parked task
    // that is now being loaded again (manually, or re-dispatched for rework)
    // must stop being skipped by the auto-loader. Best-effort — a stale marker
    // only ever suppresses an auto-load, never blocks this explicit dispatch.
    let _ = store.clear_parked(&tf.task.id);

    let addr = match start_workspace_on_task(StartSpec {
        project,
        workspace: ws,
        task_id: &tf.task.id,
        branch: &branch,
        task_body: &tf.body,
        agent: agent.as_deref(),
        launch_override: tf.task.launch.as_ref(),
    }) {
        Ok(addr) => addr,
        Err(e) => {
            let task_id = &tf.task.id;
            // Restore the pre-dispatch assignment/branch so the card isn't left
            // pinned to a slot whose launch never happened.
            if let Err(re) = store.set_fields(
                task_id,
                shelbi_state::IssueFields {
                    assigned_to: Some(original.assigned_to.clone()),
                    branch: Some(original.branch.clone()),
                    ..Default::default()
                },
            ) {
                eprintln!(
                    "shelbi: load for `{task_id}` failed and the assignment rollback \
                     also failed ({re}); run `shelbi task assign {task_id} --to \
                     <workspace>` to fix the board"
                );
            }
            return Err(e);
        }
    };

    // The load succeeded, so the branch is loadable again: drop any review-load
    // failure history for the task, so a pair that failed transiently and now
    // recovers is never left permanently backed-off / given-up by the
    // auto-loader. Only success clears — a failed attempt keeps its counter so
    // the backoff/give-up still trips. Best-effort; it only ever gates the
    // auto-loader.
    let _ = shelbi_state::clear_review_load_failures_for_task(project_name, &tf.task.id);

    Ok(addr.target())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, GitConfig, Machine, MachineKind, MergeStrategy, OrchestratorSpec, Project,
        Issue, WorkspaceSpec,
    };
    use std::collections::BTreeMap;

    /// A fresh `todo` task with no branch cut yet — the shape a subtask is in
    /// before dispatch. `params` seeds the frontmatter the workflow templates
    /// resolve against.
    fn todo_task(id: &str, params: &[(&str, &str)]) -> Issue {
        let now = chrono::Utc::now();
        Issue {
            id: id.into(),
            title: id.into(),
            column: Column::todo(),
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: params
                .iter()
                .map(|(k, v)| ((*k).to_string(), serde_yaml::Value::from(*v)))
                .collect(),
            created_at: now,
            updated_at: now,
        }
    }

    /// The default workflow with a `git.base_branch` template (may carry
    /// `{{var}}` placeholders) so dispatch has a templated base to resolve.
    fn wf_with_templated_base(base: &str) -> Workflow {
        let mut wf = shelbi_core::default_workflow();
        wf.git = Some(GitConfig {
            base_branch: Some(base.to_string()),
            branch: None,
            branch_prefix: None,
            merge_strategy: MergeStrategy::Squash,
        });
        wf
    }

    /// A review-column task assigned to `assigned_to` — the shape a
    /// Queued-for-Review card is in (still pinned to the slot that built it).
    fn review_task(id: &str, assigned_to: &str) -> Issue {
        let now = chrono::Utc::now();
        Issue {
            id: id.into(),
            title: id.into(),
            column: Column::review(),
            priority: 0,
            assigned_to: Some(assigned_to.into()),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// A hub project with one dev slot (`alpha`, no tags) and two
    /// `review`-tagged slots. Saved to `SHELBI_HOME` so the on-disk load
    /// paths can read it back.
    fn tagged_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            name: "demo".into(),
            label: None,
            display_name: None,
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp/demo".into(),
                host: None,
                tags: Vec::new(),
                forward: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![
                WorkspaceSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "review-1".into(),
                    machine: "hub".into(),
                    tags: vec!["review".into()],
                    slot: None,
                },
                WorkspaceSpec {
                    name: "review-2".into(),
                    machine: "hub".into(),
                    tags: vec!["review".into()],
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            github_reconcile_interval_secs: 900,
            workspace_permissions_mode: Some("auto".into()),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            issue_tracker: Default::default(),
            detected_shapes: Vec::new(),
        }
    }

    fn fresh_home() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-load-test-{}-{}",
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
    fn free_review_workspaces_lists_only_idle_review_slots() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // No active tasks yet → both review slots are free; the dev slot
        // (`alpha`) never appears because it isn't review-tagged.
        let free = free_review_workspaces("demo").unwrap();
        let names: Vec<&str> = free.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["review-1", "review-2"]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn free_review_workspaces_drops_a_busy_review_slot() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // A review task loaded on review-1 marks that slot busy; only the
        // other review slot is offered.
        shelbi_state::save_task("demo", &review_task("t-loaded", "review-1"), "body").unwrap();

        let free = free_review_workspaces("demo").unwrap();
        let names: Vec<&str> = free.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, ["review-2"]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn load_review_task_rejects_a_non_review_workspace() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // Still pinned to the dev slot that built it — exactly the state a
        // Queued-for-Review card is in.
        shelbi_state::save_task("demo", &review_task("t-queued", "alpha"), "body").unwrap();

        // Targeting the dev slot is refused before any dispatch — the guard
        // that stops a handoff task being re-seeded to the dev pane.
        let err = load_review_task("demo", "t-queued", "alpha").unwrap_err();
        assert!(
            err.to_string().contains("not a declared review-tagged workspace"),
            "got: {err}"
        );
        // The task is untouched: still assigned to the dev slot, no branch
        // written by the aborted load.
        let after = shelbi_state::load_task("demo", "t-queued").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("alpha"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn dispatch_refuses_when_templated_base_branch_is_unresolved() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let project = tagged_project();
        shelbi_state::save_project(&project).unwrap();

        // A fresh subtask (no branch cut yet) filed without the `feature:` link
        // its workflow's `base_branch: feature/{{feature}}` needs to resolve.
        let task = todo_task("orphan-subtask", &[]);
        shelbi_state::save_task("demo", &task, "body").unwrap();
        let tf = shelbi_state::load_task("demo", "orphan-subtask").unwrap();

        let wf = wf_with_templated_base("feature/{{feature}}");
        let ws = project.workspace("alpha").unwrap().clone();

        // Dispatch refuses loudly, naming the missing frontmatter field, before
        // launching the worker or touching the board.
        let err = dispatch_task_onto("demo", &project, &wf, tf, &ws, None).unwrap_err();
        assert!(
            err.to_string().contains("feature"),
            "error should name the missing `feature` field, got: {err}"
        );

        // The task never left its ready status and was neither assigned nor
        // branch-stamped by the aborted dispatch.
        let after = shelbi_state::load_task("demo", "orphan-subtask").unwrap();
        assert_eq!(after.task.column, Column::todo());
        assert_eq!(after.task.assigned_to, None);
        assert_eq!(after.task.branch, None);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn dispatch_agent_for_review_slot_forces_the_review_agent() {
        let project = tagged_project();
        let review = project.workspace("review-1").unwrap();

        // The status's declared agent (a Zen hint like `orchestrator`, or even
        // `developer`, or none) is overridden: a review-slot load always
        // dispatches the Review agent that serves the branch.
        for status_agent in [
            Some("orchestrator".to_string()),
            Some("developer".to_string()),
            None,
        ] {
            assert_eq!(
                dispatch_agent_for(&project, review, status_agent),
                Some(shelbi_state::REVIEW_AGENT.to_string()),
            );
        }
    }

    #[test]
    fn dispatch_agent_for_non_review_slot_keeps_the_status_agent() {
        let project = tagged_project();
        let dev = project.workspace("alpha").unwrap();

        // A non-review load is untouched — the generic status agent flows
        // through exactly as declared.
        assert_eq!(
            dispatch_agent_for(&project, dev, Some("developer".to_string())),
            Some("developer".to_string()),
        );
        assert_eq!(dispatch_agent_for(&project, dev, None), None);
    }

    #[test]
    fn load_task_for_review_needs_a_free_review_slot() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // Both review slots busy with *other* tasks; the queued task can't be
        // placed, so the id-only review loader reports it rather than silently
        // re-seeding the dev slot.
        shelbi_state::save_task("demo", &review_task("t-a", "review-1"), "body").unwrap();
        shelbi_state::save_task("demo", &review_task("t-b", "review-2"), "body").unwrap();
        shelbi_state::save_task("demo", &review_task("t-queued", "alpha"), "body").unwrap();

        let err = load_task_for_review("demo", "t-queued").unwrap_err();
        assert!(
            err.to_string().contains("no free review workspace"),
            "got: {err}"
        );
        // Untouched: still on the dev slot, never bounced back to a dev pane.
        let after = shelbi_state::load_task("demo", "t-queued").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("alpha"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A review-column task with a chosen priority and optional assignment —
    /// lets a test spell out board order and the queued-vs-serving shape.
    fn review_task_pri(id: &str, assigned_to: Option<&str>, priority: u32) -> Issue {
        let now = chrono::Utc::now();
        Issue {
            id: id.into(),
            title: id.into(),
            column: Column::review(),
            priority,
            assigned_to: assigned_to.map(Into::into),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn tf(task: Issue) -> IssueFile {
        IssueFile {
            task,
            body: "body".into(),
        }
    }

    // -- auto-load selection (pure) -----------------------------------------

    #[test]
    fn plan_pairs_queued_tasks_with_free_slots_in_board_order() {
        let project = tagged_project();
        // Two queued review cards (one still on the dev slot, one unassigned)
        // and two idle review slots → both pair up, input (board) order kept.
        let review = [
            tf(review_task_pri("t-1", Some("alpha"), 0)),
            tf(review_task_pri("t-2", None, 1)),
        ];
        let free = vec![
            project.workspace("review-1").unwrap().clone(),
            project.workspace("review-2").unwrap().clone(),
        ];
        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &BTreeMap::new(), 0);
        assert_eq!(
            plan,
            vec![
                ("t-1".to_string(), "review-1".to_string()),
                ("t-2".to_string(), "review-2".to_string()),
            ]
        );
    }

    /// The CLI `task start`/`task assign` review-slot guard (which refuses to
    /// route a normal dev task onto a `review`-tagged slot without `--force`)
    /// lives in the CLI handlers only. The autoload review path never goes
    /// through them: a review-routed task (unassigned, or still on its dev slot)
    /// is planned straight onto a free review slot with no `--force` involved.
    /// This pins that the guard leaves the legitimate review-load path alone.
    #[test]
    fn plan_routes_review_task_onto_review_slot_without_force() {
        let project = tagged_project();
        // A queued review card still pointing at its dev slot, plus one still
        // unassigned — both belong on a review slot, and the planner puts them
        // there directly (the autoloader never consults the CLI guard).
        let review = [
            tf(review_task_pri("from-dev", Some("alpha"), 0)),
            tf(review_task_pri("unassigned", None, 1)),
        ];
        let free = vec![
            project.workspace("review-1").unwrap().clone(),
            project.workspace("review-2").unwrap().clone(),
        ];
        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &BTreeMap::new(), 0);
        assert_eq!(
            plan,
            vec![
                ("from-dev".to_string(), "review-1".to_string()),
                ("unassigned".to_string(), "review-2".to_string()),
            ],
            "autoload must still place review-routed tasks onto review slots",
        );
    }

    #[test]
    fn plan_skips_tasks_already_serving_on_a_review_slot() {
        let project = tagged_project();
        // `t-serving` is already on review-1 (serving) → dropped; only the
        // genuinely queued `t-queued` is paired, onto the one free slot.
        let review = [
            tf(review_task_pri("t-serving", Some("review-1"), 0)),
            tf(review_task_pri("t-queued", Some("alpha"), 1)),
        ];
        let free = vec![project.workspace("review-2").unwrap().clone()];
        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &BTreeMap::new(), 0);
        assert_eq!(plan, vec![("t-queued".to_string(), "review-2".to_string())]);
    }

    #[test]
    fn plan_caps_at_min_of_slots_and_queued() {
        let project = tagged_project();
        // Three queued cards, one free slot → only the first (board order) is
        // paired; the rest stay queued.
        let review = [
            tf(review_task_pri("t-1", None, 0)),
            tf(review_task_pri("t-2", None, 1)),
            tf(review_task_pri("t-3", None, 2)),
        ];
        let free = vec![project.workspace("review-1").unwrap().clone()];
        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &BTreeMap::new(), 0);
        assert_eq!(plan, vec![("t-1".to_string(), "review-1".to_string())]);

        // No free slots → nothing planned even with queued work.
        assert!(plan_review_autoload(&review, &project, &[], &BTreeSet::new(), &BTreeMap::new(), 0).is_empty());
        // No queued work → nothing planned even with free slots.
        let serving = [tf(review_task_pri("t-x", Some("review-1"), 0))];
        assert!(plan_review_autoload(&serving, &project, &free, &BTreeSet::new(), &BTreeMap::new(), 0).is_empty());
    }

    #[test]
    fn plan_skips_a_parked_task_and_still_serves_the_rest() {
        let project = tagged_project();
        // Two queued cards, two free slots. `t-parked` was deliberately
        // unloaded by the operator, so it must NOT be re-grabbed even though a
        // slot is free — the other queued card is still served.
        let review = [
            tf(review_task_pri("t-parked", None, 0)),
            tf(review_task_pri("t-queued", Some("alpha"), 1)),
        ];
        let free = vec![
            project.workspace("review-1").unwrap().clone(),
            project.workspace("review-2").unwrap().clone(),
        ];
        let parked: BTreeSet<String> = std::iter::once("t-parked".to_string()).collect();
        let plan = plan_review_autoload(&review, &project, &free, &parked, &BTreeMap::new(), 0);
        // The parked card is dropped; the queued one lands on the first slot.
        assert_eq!(plan, vec![("t-queued".to_string(), "review-1".to_string())]);

        // With only the parked card queued, nothing loads at all.
        let only_parked = [tf(review_task_pri("t-parked", None, 0))];
        assert!(plan_review_autoload(&only_parked, &project, &free, &parked, &BTreeMap::new(), 0).is_empty());
    }

    // -- conflicting-slot guard (pure) --------------------------------------

    #[test]
    fn conflicting_review_slot_flags_only_a_different_review_slot() {
        let project = tagged_project();

        // Assigned to a DIFFERENT review slot → conflict (a second slot).
        let on_review_1 = review_task("t", "review-1");
        assert_eq!(
            conflicting_review_slot(&project, &on_review_1, "review-2"),
            Some("review-1".to_string()),
        );

        // Assigned to the SAME slot we're targeting → NOT a conflict: this is
        // the resume-onto-the-same-slot case a stranded (dead-pane) review
        // slot needs, so it must be allowed through to dispatch.
        assert_eq!(
            conflicting_review_slot(&project, &on_review_1, "review-1"),
            None,
        );

        // Assigned to a dev slot (a queued handoff card still pinned to the
        // slot that built it) → not a review-slot conflict.
        let on_dev = review_task("t", "alpha");
        assert_eq!(conflicting_review_slot(&project, &on_dev, "review-1"), None);

        // Unassigned → nothing to conflict with.
        let mut unassigned = review_task("t", "alpha");
        unassigned.assigned_to = None;
        assert_eq!(
            conflicting_review_slot(&project, &unassigned, "review-1"),
            None,
        );
    }

    #[test]
    fn load_review_task_allows_resume_onto_the_same_slot() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // A task stranded on review-1 (its pane died on quit) is still
        // assigned there on disk. Re-loading onto review-1 is a resume, so the
        // "already loaded on review slot" guard must NOT fire — the load
        // proceeds past the guard and only fails later at dispatch (no tmux in
        // the test env), never with the conflicting-slot rejection.
        shelbi_state::save_task("demo", &review_task("t-stranded", "review-1"), "body").unwrap();

        let err = load_review_task("demo", "t-stranded", "review-1").unwrap_err();
        assert!(
            !err.to_string().contains("already loaded on review slot"),
            "same-slot resume must clear the conflicting-slot guard, got: {err}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- auto-load / manual race guards (on disk, reject before dispatch) ----

    #[test]
    fn load_review_task_rejects_a_task_already_serving_on_a_review_slot() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // The task was auto-loaded onto review-1 between a human opening the
        // confirm and pressing Enter. A manual load targeting review-2 must not
        // re-dispatch it onto a second slot — the guard rejects before dispatch.
        shelbi_state::save_task("demo", &review_task("t-x", "review-1"), "body").unwrap();

        let err = load_review_task("demo", "t-x", "review-2").unwrap_err();
        assert!(
            err.to_string().contains("already loaded on review slot"),
            "got: {err}"
        );
        // Untouched: still on the slot it was already serving from.
        let after = shelbi_state::load_task("demo", "t-x").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("review-1"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn load_review_task_rejects_a_slot_already_serving_another_task() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // review-1 is already busy with another review task; a second claim of
        // it (a poller tick and a human racing for the same slot) is refused
        // before dispatch, so two tasks can't land on one slot.
        shelbi_state::save_task("demo", &review_task("t-loaded", "review-1"), "body").unwrap();
        shelbi_state::save_task("demo", &review_task("t-queued", "alpha"), "body").unwrap();

        let err = load_review_task("demo", "t-queued", "review-1").unwrap_err();
        assert!(
            err.to_string().contains("already serving another task"),
            "got: {err}"
        );
        // The queued task is untouched — still on the dev slot, no branch written.
        let after = shelbi_state::load_task("demo", "t-queued").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("alpha"));
        assert!(after.task.branch.is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- picker enumeration + eviction (reuse an occupied slot) -------------

    #[test]
    fn review_slots_lists_every_slot_with_its_free_or_occupied_state() {
        let project = tagged_project();
        // review-1 holds a task, review-2 is free. Both review slots appear in
        // declaration order (the dev slot `alpha` never does), each carrying
        // its occupant (or `None`) — the picker's data source.
        let active = [tf(review_task_pri("t-x", Some("review-1"), 0))];
        let slots = review_slots_from(&project, &active);
        assert_eq!(
            slots,
            vec![
                ReviewSlot {
                    name: "review-1".into(),
                    occupant: Some(ReviewSlotOccupant {
                        task_id: "t-x".into(),
                        title: "t-x".into(),
                    }),
                },
                ReviewSlot {
                    name: "review-2".into(),
                    occupant: None,
                },
            ]
        );
    }

    #[test]
    fn evict_review_slot_returns_the_occupant_to_the_queue() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // review-1 is serving `t-loaded`; a picker choice targets it for a new
        // task, so `t-loaded` must be bounced back to the queue first.
        shelbi_state::save_task("demo", &review_task("t-loaded", "review-1"), "body").unwrap();

        let evicted = evict_review_slot_locked("demo", "review-1", "t-queued").unwrap();
        assert_eq!(evicted.as_deref(), Some("t-loaded"));

        // Bounced back, not accepted/rejected: still in the Review column, but
        // its review-slot assignment is dropped so it reads as Queued/Pending.
        let after = shelbi_state::load_task("demo", "t-loaded").unwrap();
        assert_eq!(after.task.column, Column::review());
        assert_eq!(after.task.assigned_to, None);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn evict_review_slot_is_a_noop_for_a_free_slot_or_the_same_task() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // `t-self` already owns review-1 (a same-slot resume, keep == occupant)
        // and review-2 is free. Neither is an eviction: nobody else to bounce.
        shelbi_state::save_task("demo", &review_task("t-self", "review-1"), "body").unwrap();

        assert_eq!(
            evict_review_slot_locked("demo", "review-1", "t-self").unwrap(),
            None,
            "the slot's own occupant is never evicted (same-slot resume)"
        );
        assert_eq!(
            evict_review_slot_locked("demo", "review-2", "t-new").unwrap(),
            None,
            "a free slot has nothing to evict"
        );
        // `t-self` is untouched by either no-op eviction.
        let after = shelbi_state::load_task("demo", "t-self").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("review-1"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- startup / reload re-evaluation (disk-derived, no live session) ------

    #[test]
    fn autoload_review_queue_is_a_noop_when_every_review_slot_is_busy() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // Both review slots already serving, plus a queued card that can't be
        // placed. Capacity is respected: nothing loads (no dispatch attempted),
        // and the queued card is left untouched for a later free slot.
        shelbi_state::save_task("demo", &review_task("t-a", "review-1"), "body").unwrap();
        shelbi_state::save_task("demo", &review_task("t-b", "review-2"), "body").unwrap();
        shelbi_state::save_task("demo", &review_task("t-queued", "alpha"), "body").unwrap();

        let loaded = autoload_review_queue("demo").unwrap();
        assert!(loaded.is_empty(), "expected no auto-loads, got {loaded:?}");
        let after = shelbi_state::load_task("demo", "t-queued").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("alpha"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn autoload_review_queue_leaves_a_parked_task_unloaded() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // A queued review card the operator deliberately unloaded (parked) —
        // still in the review column, on the dev slot, with a free review slot
        // available. The end-to-end auto-loader must skip it, so it stays
        // unloaded rather than being re-grabbed on the next tick (the churn
        // this fixes). No dispatch is attempted, so the assertion holds even
        // without tmux.
        shelbi_state::save_task("demo", &review_task("t-parked", "alpha"), "body").unwrap();
        shelbi_state::set_task_parked("demo", "t-parked").unwrap();

        let loaded = autoload_review_queue("demo").unwrap();
        assert!(loaded.is_empty(), "parked task must not auto-load, got {loaded:?}");
        let after = shelbi_state::load_task("demo", "t-parked").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("alpha"));
        assert!(after.task.branch.is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn autoload_review_queue_skips_a_non_warm_github_board() {
        // Freshness guard (plan Phase 0): auto-load must not consume a scarce
        // review slot off a stale/cold/failed board. A `github` project whose
        // board never warms (a failing `gh` runner, standing in for a rate-limit
        // park) must produce no auto-loads and no dispatch — the tick is skipped
        // and retried when the board is warm again.
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut project = tagged_project();
        project.name = "ghguard-autoload".into();
        project.issue_tracker = shelbi_core::IssueTrackerConfig {
            backend: shelbi_core::IssueTrackerBackend::Github,
            github: Some(shelbi_core::GithubConnection {
                repo: "owner/repo".into(),
            }),
            ..Default::default()
        };
        shelbi_state::save_project(&project).unwrap();
        shelbi_state::set_test_gh_runner(|_| Err(shelbi_core::Error::Other("boom".into())));

        let loaded = autoload_review_queue("ghguard-autoload").unwrap();
        assert!(
            loaded.is_empty(),
            "a non-warm board must yield no auto-loads, got {loaded:?}"
        );

        shelbi_state::clear_test_gh_runner();
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn autoload_review_queue_plans_from_disk_on_a_fresh_evaluation() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // The state a poller sees on the first tick after `shelbi reload` /
        // `quit`+restart: a queued review card and a free slot, re-derived from
        // disk with no live TUI session. The disk-derived plan (list_column +
        // free_review_workspaces, exactly what `autoload_review_queue` runs)
        // pairs them, proving the startup path would load without a keystroke.
        shelbi_state::save_task("demo", &review_task_pri("t-queued", Some("alpha"), 0), "body")
            .unwrap();

        let project = shelbi_state::load_project("demo").unwrap();
        let review = shelbi_state::list_column("demo", Column::review()).unwrap();
        let free = free_review_workspaces("demo").unwrap();
        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &BTreeMap::new(), 0);
        assert_eq!(plan, vec![("t-queued".to_string(), "review-1".to_string())]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- review-status routing gate (pure) ----------------------------------

    #[test]
    fn status_routes_to_review_gates_on_the_review_tag() {
        // The shipped `task` workflow tags its `review` status `[review]` → an
        // auto-load onto a review slot is permitted.
        assert!(status_routes_to_review(
            &shelbi_core::task_workflow(),
            "review"
        ));
        // The bare default workflow's `review` status carries no tag → it must
        // NOT be auto-grabbed onto a scarce review slot; it stays in the review
        // column for a deliberate human sidebar load.
        assert!(!status_routes_to_review(
            &shelbi_core::default_workflow(),
            "review"
        ));
        // An unknown status id is never review-routed.
        assert!(!status_routes_to_review(
            &shelbi_core::task_workflow(),
            "no-such-status"
        ));
    }

    // -- failure backoff / give-up (pure) -----------------------------------

    fn failure(attempts: u32, last_attempt: i64, gave_up: bool) -> ReviewLoadFailure {
        ReviewLoadFailure {
            attempts,
            last_attempt,
            gave_up,
        }
    }

    #[test]
    fn autoload_retry_suppressed_holds_off_within_backoff_then_releases() {
        let backoff = BASE_BACKOFF.as_secs() as i64;
        let window = CRASH_LOOP_WINDOW.as_secs() as i64;

        // A given-up pair is suppressed forever (left for the user).
        assert!(autoload_retry_suppressed(&failure(9, 0, true), 10_000));
        // A never-failed pair is free to attempt.
        assert!(!autoload_retry_suppressed(&failure(0, 0, false), 0));

        // One failure: suppressed until BASE_BACKOFF has elapsed since it.
        assert!(autoload_retry_suppressed(&failure(1, 100, false), 100 + backoff - 1));
        assert!(!autoload_retry_suppressed(&failure(1, 100, false), 100 + backoff));
        // Two failures: the window doubles (BASE_BACKOFF * 2).
        assert!(autoload_retry_suppressed(
            &failure(2, 100, false),
            100 + 2 * backoff - 1
        ));
        assert!(!autoload_retry_suppressed(
            &failure(2, 100, false),
            100 + 2 * backoff
        ));
        // A last attempt older than the crash-loop window has aged out: the
        // counter is stale, so the pair is retryable again (a fresh start).
        assert!(!autoload_retry_suppressed(&failure(2, 100, false), 100 + window));
    }

    #[test]
    fn note_autoload_failure_counts_backs_off_then_trips_giveup_once() {
        let window = CRASH_LOOP_WINDOW.as_secs() as i64;

        // First failure from a clean slate.
        let (r1, trips1) = note_autoload_failure(&ReviewLoadFailure::default(), 100);
        assert_eq!(r1, failure(1, 100, false));
        assert!(!trips1);

        // Second failure within the window increments, no give-up yet.
        let (r2, trips2) = note_autoload_failure(&r1, 110);
        assert_eq!(r2, failure(2, 110, false));
        assert!(!trips2);

        // Third failure reaches the cap → latches give-up and trips the event
        // exactly once.
        let (r3, trips3) = note_autoload_failure(&r2, 120);
        assert_eq!(r3, failure(3, 120, true));
        assert!(trips3, "reaching the cap must trip the gave-up event");

        // A further failure while already given up does not re-trip the event.
        let (r4, trips4) = note_autoload_failure(&r3, 130);
        assert!(r4.gave_up);
        assert!(!trips4, "give-up fires once, never again");

        // A failure landing outside the crash-loop window resets the counter,
        // so a slow drip never accumulates to a give-up.
        let (drip, trips_drip) = note_autoload_failure(&failure(2, 100, false), 100 + window);
        assert_eq!(drip, failure(1, 100 + window, false));
        assert!(!trips_drip);
    }

    #[test]
    fn plan_suppresses_a_given_up_pair_but_still_places_it_on_another_slot() {
        let project = tagged_project();
        // t-1 has given up on review-1 specifically. t-2 is fresh.
        let review = [
            tf(review_task_pri("t-1", None, 0)),
            tf(review_task_pri("t-2", None, 1)),
        ];
        let free = vec![
            project.workspace("review-1").unwrap().clone(),
            project.workspace("review-2").unwrap().clone(),
        ];
        let mut failures: BTreeMap<String, BTreeMap<String, ReviewLoadFailure>> = BTreeMap::new();
        failures
            .entry("t-1".into())
            .or_default()
            .insert("review-1".into(), failure(3, 0, true));

        // review-1: t-1 is suppressed there, so the slot goes to t-2 instead.
        // review-2: t-1 is NOT suppressed there (per-`(task, slot)`), so it
        // still lands on the other slot rather than being abandoned wholesale.
        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &failures, 10_000);
        assert_eq!(
            plan,
            vec![
                ("t-2".to_string(), "review-1".to_string()),
                ("t-1".to_string(), "review-2".to_string()),
            ]
        );
    }

    #[test]
    fn plan_drops_a_given_up_pair_when_it_is_the_only_slot() {
        let project = tagged_project();
        // The bug's resting state: one queued task, one review slot, and the
        // pair has given up. Nothing is planned — no identical retry is emitted.
        let review = [tf(review_task_pri("t-doomed", None, 0))];
        let free = vec![project.workspace("review-1").unwrap().clone()];
        let mut failures: BTreeMap<String, BTreeMap<String, ReviewLoadFailure>> = BTreeMap::new();
        failures
            .entry("t-doomed".into())
            .or_default()
            .insert("review-1".into(), failure(3, 0, true));

        let plan = plan_review_autoload(&review, &project, &free, &BTreeSet::new(), &failures, 10_000);
        assert!(plan.is_empty(), "a given-up pair must not be re-planned");
    }

    // -- routing gate + failure ledger (on disk, end-to-end) ----------------

    #[test]
    fn autoload_skips_a_task_whose_review_status_is_not_review_tagged() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();

        // No workflows scaffolded → the task resolves to the bare default
        // workflow, whose `review` status carries no `review` tag. A free review
        // slot is available, but the loader must leave the task in the review
        // column rather than consuming a slot for an untagged handoff status.
        shelbi_state::save_task("demo", &review_task("t-untagged", "alpha"), "body").unwrap();

        let loaded = autoload_review_queue("demo").unwrap();
        assert!(loaded.is_empty(), "an untagged review status must not auto-load");
        // It was never even attempted, so no failure was recorded.
        assert!(shelbi_state::review_load_failures("demo").unwrap().is_empty());
        // Untouched on the dev slot.
        let after = shelbi_state::load_task("demo", "t-untagged").unwrap();
        assert_eq!(after.task.assigned_to.as_deref(), Some("alpha"));

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn autoload_records_a_failure_and_backs_off_a_doomed_load() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        shelbi_state::save_project(&tagged_project()).unwrap();
        // Scaffold the shipped `task`/`subtask` workflows + statuses so the task
        // resolves to the review-tagged `task` workflow and is eligible to load.
        shelbi_state::scaffold_project_statuses("demo").unwrap();
        shelbi_state::scaffold_project_workflow("demo").unwrap();

        // Occupy review-2 so review-1 is the *only* free slot — otherwise a
        // pair backed off on review-1 would (correctly, per-`(task, slot)`)
        // spill onto review-2, which is a different pairing. Pinning to one slot
        // isolates the backoff-of-the-same-pair behavior under test.
        shelbi_state::save_task("demo", &review_task("t-occupied", "review-2"), "body").unwrap();

        // A queued, review-eligible task with the one free slot. Every load
        // attempt fails at dispatch (no tmux in the test env) — the
        // durable-failure shape the fix targets.
        shelbi_state::save_task("demo", &review_task("t-doomed", "alpha"), "body").unwrap();

        // First tick: the pair is attempted and fails, so a failure record is
        // written. No success is ever reported.
        let loaded = autoload_review_queue("demo").unwrap();
        assert!(loaded.is_empty());
        let after_first = shelbi_state::review_load_failures("demo").unwrap();
        let rec = after_first
            .get("t-doomed")
            .and_then(|m| m.get("review-1"))
            .expect("the failed pair must be recorded for backoff");
        assert_eq!(rec.attempts, 1, "one attempt recorded on the first failure");
        assert!(!rec.gave_up);

        // Second tick within the backoff window: the planner suppresses the
        // pair, so it is NOT re-attempted and the counter does not climb — the
        // identical retry the bug emitted every tick is gone.
        let loaded = autoload_review_queue("demo").unwrap();
        assert!(loaded.is_empty());
        let after_second = shelbi_state::review_load_failures("demo").unwrap();
        assert_eq!(
            after_second["t-doomed"]["review-1"].attempts, 1,
            "a within-backoff tick must not re-attempt the same pair"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
