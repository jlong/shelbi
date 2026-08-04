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

use shelbi_core::{Column, Error, Project, Result, Task, WorkspaceSpec, Workflow};
use shelbi_state::TaskFile;

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
    // Review activation persists assignment/branch and starts a workspace.
    // Both sidebar and palette converge here, so keep the mismatch guard at
    // this shared boundary rather than relying on every UI surface to remember.
    shelbi_state::ensure_daemon_matches_for_mutation()?;
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;

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
    let review_tag: BTreeSet<String> = std::iter::once("review".to_string()).collect();
    let mut active = shelbi_state::list_column(project_name, Column::in_progress())?;
    active.extend(shelbi_state::list_column(project_name, Column::review())?);
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
    let ws = project
        .workspace(workspace_name)
        .filter(|w| project.effective_tags(w).contains("review"))
        .cloned()
        .ok_or_else(|| {
            Error::Other(format!(
                "`{workspace_name}` is not a declared review-tagged workspace"
            ))
        })?;
    let tf = shelbi_state::load_task(project_name, task_id)?;

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
fn conflicting_review_slot(project: &Project, task: &Task, target: &str) -> Option<String> {
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
    let mut active = shelbi_state::list_column(project_name, Column::in_progress())?;
    active.extend(shelbi_state::list_column(project_name, Column::review())?);
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
    // Board order (priority, then id) — the same order the sidebar shows.
    let review_tasks = shelbi_state::list_column(project_name, Column::review())?;
    // Idle review slots in declaration order (never lists a dev slot).
    let free = free_review_workspaces(project_name)?;
    let plan = plan_review_autoload(&review_tasks, &project, &free);
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
            }
        }
    }
    Ok(loaded)
}

/// Pure slot-selection for [`autoload_review_queue`]: pair each queued review
/// task (board order) with one idle review slot (declaration order), capping at
/// `min(queued, free)`. "Queued" is a review-column task not already assigned to
/// a review-tagged slot — a handoff card still pinned to the dev slot that built
/// it, or one with no assignment; a task already serving on a review slot is
/// dropped. Split out with no I/O so board order and capacity limiting are
/// unit-testable on in-memory fixtures.
fn plan_review_autoload(
    review_tasks: &[TaskFile],
    project: &Project,
    free: &[WorkspaceSpec],
) -> Vec<(String, String)> {
    review_tasks
        .iter()
        .filter(|tf| {
            let on_review_slot = tf
                .task
                .assigned_to
                .as_deref()
                .and_then(|name| project.workspace(name))
                .is_some_and(|w| project.effective_tags(w).contains("review"));
            !on_review_slot
        })
        .map(|tf| tf.task.id.clone())
        .zip(free.iter().map(|w| w.name.clone()))
        .collect()
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
    let tf = shelbi_state::load_task(project_name, task_id)?;
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
    mut tf: TaskFile,
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

    // Persist the assignment before dispatch so a concurrent load can't pick
    // the same slot, and roll it back on a dispatch failure.
    let original = tf.task.clone();
    tf.task.assigned_to = Some(ws.name.clone());
    tf.task.branch = Some(branch.clone());
    tf.task.updated_at = chrono::Utc::now();
    shelbi_state::save_task(project_name, &tf.task, &tf.body)?;

    let addr = match start_workspace_on_task(StartSpec {
        project,
        workspace: ws,
        task_id: &tf.task.id,
        branch: &branch,
        task_body: &tf.body,
        agent: agent.as_deref(),
    }) {
        Ok(addr) => addr,
        Err(e) => {
            let task_id = &tf.task.id;
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

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, GitConfig, Machine, MachineKind, MergeStrategy, OrchestratorSpec, Project,
        Task, WorkspaceSpec,
    };
    use std::collections::BTreeMap;

    /// A fresh `todo` task with no branch cut yet — the shape a subtask is in
    /// before dispatch. `params` seeds the frontmatter the workflow templates
    /// resolve against.
    fn todo_task(id: &str, params: &[(&str, &str)]) -> Task {
        let now = chrono::Utc::now();
        Task {
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
    fn review_task(id: &str, assigned_to: &str) -> Task {
        let now = chrono::Utc::now();
        Task {
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
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "review-1".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: vec!["review".into()],
                    slot: None,
                },
                WorkspaceSpec {
                    name: "review-2".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: vec!["review".into()],
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
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
    fn review_task_pri(id: &str, assigned_to: Option<&str>, priority: u32) -> Task {
        let now = chrono::Utc::now();
        Task {
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
            params: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn tf(task: Task) -> TaskFile {
        TaskFile {
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
        let plan = plan_review_autoload(&review, &project, &free);
        assert_eq!(
            plan,
            vec![
                ("t-1".to_string(), "review-1".to_string()),
                ("t-2".to_string(), "review-2".to_string()),
            ]
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
        let plan = plan_review_autoload(&review, &project, &free);
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
        let plan = plan_review_autoload(&review, &project, &free);
        assert_eq!(plan, vec![("t-1".to_string(), "review-1".to_string())]);

        // No free slots → nothing planned even with queued work.
        assert!(plan_review_autoload(&review, &project, &[]).is_empty());
        // No queued work → nothing planned even with free slots.
        let serving = [tf(review_task_pri("t-x", Some("review-1"), 0))];
        assert!(plan_review_autoload(&serving, &project, &free).is_empty());
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
        let plan = plan_review_autoload(&review, &project, &free);
        assert_eq!(plan, vec![("t-queued".to_string(), "review-1".to_string())]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }
}
