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

use shelbi_core::{Column, Error, Project, Result, WorkspaceSpec, Workflow};
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
    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let agent = workflow
        .status(tf.task.column.as_str())
        .and_then(|s| s.agent.clone());
    dispatch_task_onto(project_name, &project, &workflow, tf, &ws, agent)
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
        AgentRunnerSpec, Machine, MachineKind, OrchestratorSpec, Project, Task, WorkspaceSpec,
    };
    use std::collections::BTreeMap;

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
}
