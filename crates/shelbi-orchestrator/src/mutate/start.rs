//! `start` — launch the assigned workspace on an issue. The heaviest mutation:
//! it resolves the workspace/agent, cuts the branch, persists the `in_progress`
//! move *before* spawning (so a crash can't leave an agent on a `todo` card),
//! bounds the launch on an idle deadline, and rolls the card back + tears the
//! pane down on a clean failure. Ported verbatim from `shelbi-cli`'s
//! `commands/issue.rs::start`, with `println!`/`eprintln!` replaced by the
//! [`OutputSink`](super::OutputSink) and a `recheck` added immediately before the
//! dispatch (the irreversible step), so a second concurrent dispatch of the same
//! issue is refused rather than starting a second agent.

use std::time::Duration;

use shelbi_core::{default_workflow, Column, Issue, Owner, StatusCategory, Workflow, WorkflowStatus};

use crate::session_backend::SessionTarget;
use super::{
    ensure_workspace_dispatchable, guard_review_slot, issue_store, load_issue, MutateError,
    OutputSink, Recheck,
};

/// One `start` request.
pub struct StartParams<'a> {
    pub project: &'a str,
    pub id: &'a str,
    pub workspace: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub reason: Option<&'a str>,
    pub force: bool,
}

/// Poll cadence of [`await_launch`].
const LAUNCH_POLL_INTERVAL: Duration = Duration::from_secs(2);

pub(crate) fn start(
    params: &StartParams<'_>,
    sink: &mut dyn OutputSink,
    recheck: Recheck<'_>,
) -> Result<(), MutateError> {
    let StartParams {
        project,
        id,
        workspace: workspace_arg,
        branch: branch_arg,
        reason,
        force,
    } = *params;

    let project_yaml = shelbi_state::load_project(project).map_err(MutateError::backend)?;
    let store = issue_store(project)?;
    let mut tf = load_issue(store.as_ref(), id)?;

    let workspace_name = workspace_arg
        .map(str::to_string)
        .or_else(|| tf.task.assigned_to.clone())
        .ok_or_else(|| {
            MutateError::Backend(format!(
                "issue `{id}` has no assigned workspace — pass `--workspace NAME` or run \
                 `shelbi issue assign {id} --to <workspace>` first"
            ))
        })?;
    let workspace = project_yaml.workspace(&workspace_name).ok_or_else(|| {
        MutateError::Backend(format!(
            "workspace `{workspace_name}` not declared in project `{project}` (known: {})",
            project_yaml
                .workspaces
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    guard_review_slot(&project_yaml, workspace, &workspace_name, id, force, sink)?;

    // Tag routing: if the active status requires workspace tags, the chosen
    // workspace's effective tags must be a superset.
    let required = required_active_tags(project, &tf.task, sink);
    if !required.is_empty() {
        let effective = project_yaml.effective_tags(workspace);
        let missing: Vec<&str> = required
            .iter()
            .filter(|t| !effective.contains(t.as_str()))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            return Err(MutateError::Backend(format!(
                "workspace `{workspace_name}` can't take issue `{id}`: its active status \
                 requires tag(s) {required:?} but the workspace's effective tags are \
                 {effective:?} (missing {missing:?}) — assign a workspace tagged accordingly"
            )));
        }
    }

    // Refuse to clobber a user shell. Best-effort probe.
    if let Some(ws_machine) = project_yaml.machine(&workspace.machine) {
        if let Ok(ws_addr) = crate::workspace::workspace_target(&project_yaml, workspace) {
            let shell_open =
                crate::workspace::workspace_user_shell_open(&ws_machine.host(), &ws_addr)
                    .unwrap_or(false);
            if shell_open {
                return Err(MutateError::Backend(format!(
                    "workspace `{workspace_name}` is occupied by a user shell (opened from \
                     the sidebar) — exit the shell there, or dispatch to another workspace"
                )));
            }
        }
    }

    // Refuse to clobber another in-flight issue on the same workspace.
    ensure_workspace_dispatchable(project, &workspace_name, id)?;

    // Cutover gate (`rt-cutover-migration`): on the session backend, refuse to
    // dispatch onto a workspace whose tmux→session migration hasn't completed,
    // with a message that says why and how to resolve it. `start_workspace_on_task`
    // re-checks this as a backstop, but surfacing it here keeps the message
    // synchronous for `task start` rather than buried in a worker-thread error.
    crate::migration::ensure_workspace_dispatchable(project, &workspace_name)
        .map_err(MutateError::backend)?;

    // Cut the branch on the hub if it hasn't been already (depends_on aware). An
    // explicit `--branch` override bypasses the cut and points sync at that ref.
    if branch_arg.is_none() {
        let updated = crate::lifecycle::ensure_branch_for_in_progress(&project_yaml, id)
            .map_err(MutateError::backend)?;
        tf = updated;
    }
    let branch = match branch_arg
        .map(str::to_string)
        .or_else(|| tf.task.branch.clone())
    {
        Some(b) => b,
        None => {
            let workflow = shelbi_state::load_task_workflow(project, &project_yaml, &tf.task)
                .unwrap_or_else(|_| default_workflow());
            crate::branch::branch_name_for_task(&project_yaml, Some(&workflow), &tf.task)
                .map_err(MutateError::backend)?
        }
    };

    let agent_name = resolve_active_agent_for_dispatch(project, &tf.task, sink)?;

    let dest_column = {
        let workflow = resolve_task_workflow(project, &tf.task, sink);
        dispatch_destination_status(&workflow, &tf.task.column)
            .map(|s| Column::from_status_id(&s.id))
            .unwrap_or_else(Column::in_progress)
    };

    // Immediately before the irreversible dispatch, recheck that the issue has
    // not moved on since the client looked at it. Under the daemon's per-issue
    // lock this is what makes a second concurrent `start` of the same issue
    // abort (the first already moved it to `in_progress`) instead of launching a
    // second agent on the same branch.
    recheck()?;

    // Persist the in_progress move BEFORE spawning (F7). `original` snapshots the
    // pre-move frontmatter so a spawn failure can roll back.
    let original = tf.task.clone();
    let prev_column = tf.task.column.clone();
    if prev_column != dest_column {
        store
            .move_status(id, &dest_column, reason.unwrap_or("user:cli"))
            .map_err(MutateError::backend)?;
    }
    store
        .set_fields(
            id,
            shelbi_state::IssueFields {
                assigned_to: Some(Some(workspace_name.clone())),
                branch: Some(Some(branch.clone())),
                ..Default::default()
            },
        )
        .map_err(MutateError::backend)?;

    sink.out(&format!(
        "→ launching {workspace_name} on {id} (branch: {branch}, agent: {agent_name})"
    ));

    // Bound the launch phase on an idle deadline; run it on a worker thread so a
    // genuinely-stuck launch can be abandoned rather than joined.
    let launch_deadline = crate::workspace::launch_timeout();
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let project_owned = project_yaml.clone();
        let workspace_owned = workspace.clone();
        let task_id_owned = id.to_string();
        let branch_owned = branch.clone();
        let body_owned = tf.body.clone();
        let agent_owned = agent_name.clone();
        let launch_owned = tf.task.launch.clone();
        std::thread::spawn(move || {
            let result = crate::workspace::start_workspace_on_task(crate::workspace::StartSpec {
                project: &project_owned,
                workspace: &workspace_owned,
                task_id: &task_id_owned,
                branch: &branch_owned,
                task_body: &body_owned,
                agent: Some(agent_owned.as_str()),
                launch_override: launch_owned.as_ref(),
            });
            let _ = tx.send(result);
        });
    }

    let mut launched_late = false;
    let addr = match await_launch(&rx, launch_deadline, LAUNCH_POLL_INTERVAL, || {
        dispatch_progress_token(project, id, &workspace_name)
    }) {
        LaunchWait::Completed(addr) => addr,
        LaunchWait::SpawnFailed(e) => {
            rollback_and_teardown(
                project,
                &project_yaml,
                workspace,
                &original,
                &tf.body,
                prev_column.clone(),
                id,
                sink,
            );
            return Err(MutateError::LaunchSpawn(e));
        }
        LaunchWait::Panicked => {
            if let Err(le) = shelbi_state::append_dispatch_event(
                project,
                id,
                &workspace_name,
                "failed",
                "launch_thread_panicked",
            ) {
                sink.warn(&format!("warning: append_dispatch_event failed: {le}"));
            }
            rollback_and_teardown(
                project,
                &project_yaml,
                workspace,
                &original,
                &tf.body,
                prev_column.clone(),
                id,
                sink,
            );
            return Err(MutateError::Backend(format!(
                "launching workspace `{workspace_name}` on `{id}` failed: the launch thread \
                 terminated unexpectedly before reporting a result"
            )));
        }
        LaunchWait::IdleTimeout if launch_appears_complete(&project_yaml, workspace, id) => {
            launched_late = true;
            crate::workspace::workspace_target(&project_yaml, workspace)
                .map_err(MutateError::backend)?
        }
        LaunchWait::IdleTimeout => {
            // Bump this workspace's generation before rolling back: the launch
            // worker thread we're about to abandon is still running
            // `start_workspace_on_task`, and this trips its cancellation guard
            // so that if it later unblocks it stands down at its pre-spawn check
            // instead of starting an agent on a task that's been rolled back and
            // may since have been redispatched (`rt-daemon-cancellation`,
            // acceptance criterion 1). A redispatch registers a fresh guard
            // after this bump, so it is unaffected.
            crate::cancel::bump_workspace(project, &workspace_name);
            if let Err(le) = shelbi_state::append_dispatch_event(
                project,
                id,
                &workspace_name,
                "failed",
                &format!("launch_timeout_after_{}s", launch_deadline.as_secs()),
            ) {
                sink.warn(&format!("warning: append_dispatch_event failed: {le}"));
            }
            rollback_and_teardown(
                project,
                &project_yaml,
                workspace,
                &original,
                &tf.body,
                prev_column.clone(),
                id,
                sink,
            );
            return Err(MutateError::Backend(format!(
                "launching workspace `{workspace_name}` on `{id}` timed out after {}s with no \
                 completion signal — dispatch aborted, the issue rolled back to `{prev_column}`, \
                 and the launched pane torn down. The launch likely blocked on git/ssh or a \
                 stale lock; check the workspace, then re-run the dispatch.",
                launch_deadline.as_secs(),
            )));
        }
    };

    // Spawn succeeded (possibly late) — record the dispatch event now.
    if prev_column != dest_column {
        let base_reason = reason.unwrap_or("user:cli:start");
        let dispatched_reason = dispatch_reason_with_agent(base_reason, &agent_name);
        let workflow = shelbi_state::resolve_task_workflow_name(&project_yaml, &tf.task);
        if let Err(e) = shelbi_state::append_task_event(
            project,
            id,
            workflow,
            prev_column.clone(),
            dest_column.clone(),
            &dispatched_reason,
        ) {
            sink.warn(&format!("warning: append_task_event failed: {e}"));
        }
    }

    // Release a supplanted prior workspace's pane when a *different* one takes
    // over. Best-effort.
    if let Some(prev_ws_name) =
        supplanted_workspace(original.assigned_to.as_deref(), &workspace_name)
    {
        if let Some(prev_ws) = project_yaml.workspace(prev_ws_name) {
            if let Err(e) = teardown_workspace_pane(&project_yaml, prev_ws) {
                sink.warn(&format!(
                    "warning: releasing the supplanted workspace pane on `{prev_ws_name}` \
                     failed ({e}) — check it and kill a stale worker by hand if one is left"
                ));
            } else if let Err(e) = shelbi_state::append_dispatch_event(
                project,
                id,
                prev_ws_name,
                "released",
                "supplanted by a new workspace taking over the card's active status",
            ) {
                sink.warn(&format!("warning: append_dispatch_event failed: {e}"));
            }
        }
    }

    if launched_late {
        sink.out(&format!(
            "✓ {id} → in_progress on {workspace_name} ({}) — launch confirmed after the {}s \
             deadline; card left in_progress rather than rolled back",
            addr.label(),
            launch_deadline.as_secs(),
        ));
    } else {
        sink.out(&format!(
            "✓ {id} → in_progress on {workspace_name} ({})",
            addr.label()
        ));
    }
    Ok(())
}

/// Undo the in_progress move `start` persisted before spawning, after a spawn
/// failure.
fn rollback_start(
    project: &str,
    original: &Issue,
    _body: &str,
    prev_column: Column,
) -> Result<(), MutateError> {
    let store = issue_store(project)?;
    if prev_column != Column::in_progress() {
        store
            .move_status(&original.id, &prev_column, "rollback:start-failed")
            .map_err(MutateError::backend)?;
    }
    store
        .set_fields(
            &original.id,
            shelbi_state::IssueFields {
                assigned_to: Some(original.assigned_to.clone()),
                branch: Some(original.branch.clone()),
                ..Default::default()
            },
        )
        .map_err(MutateError::backend)?;
    Ok(())
}

/// Roll the in_progress move back AND tear down the pane the failed launch
/// spawned. Both halves are best-effort; each surfaces its own failure.
#[allow(clippy::too_many_arguments)]
fn rollback_and_teardown(
    project: &str,
    project_yaml: &shelbi_core::Project,
    workspace: &shelbi_core::WorkspaceSpec,
    original: &Issue,
    body: &str,
    prev_column: Column,
    id: &str,
    sink: &mut dyn OutputSink,
) {
    if let Err(re) = rollback_start(project, original, body, prev_column.clone()) {
        sink.warn(&format!(
            "warning: `{id}` launch failed and the rollback also failed ({re}); run \
             `shelbi issue move {id} --to {prev_column}` to recover"
        ));
    }
    if let Err(te) = teardown_workspace_pane(project_yaml, workspace) {
        sink.warn(&format!(
            "warning: `{id}` launch failed; tearing down the workspace pane on `{}` also failed \
             ({te}) — check the pane and kill it by hand if a stale worker is left",
            workspace.name
        ));
    }
}

/// Kill a workspace's tmux pane. Used on the launch-failure path.
fn teardown_workspace_pane(
    project_yaml: &shelbi_core::Project,
    workspace: &shelbi_core::WorkspaceSpec,
) -> Result<(), shelbi_core::Error> {
    let machine = project_yaml.machine(&workspace.machine).ok_or_else(|| {
        shelbi_core::Error::Other(format!(
            "machine `{}` for workspace `{}` is not declared",
            workspace.machine, workspace.name
        ))
    })?;
    let host = machine.host();
    let addr = crate::workspace::workspace_target(project_yaml, workspace)?;
    crate::workspace::kill_workspace_pane(&host, &addr, &workspace.name)
}

/// Terminal outcome of [`await_launch`].
enum LaunchWait {
    Completed(SessionTarget),
    SpawnFailed(shelbi_core::Error),
    Panicked,
    IdleTimeout,
}

/// Wait for the launch worker thread, measuring the timeout from the LAST
/// observed launch-progress signal rather than one wall clock from the start.
fn await_launch(
    rx: &std::sync::mpsc::Receiver<shelbi_core::Result<SessionTarget>>,
    idle_deadline: Duration,
    poll_interval: Duration,
    mut progress_token: impl FnMut() -> u64,
) -> LaunchWait {
    use std::sync::mpsc::RecvTimeoutError;
    let mut last_token = progress_token();
    let mut last_progress = std::time::Instant::now();
    loop {
        match rx.recv_timeout(poll_interval) {
            Ok(Ok(addr)) => return LaunchWait::Completed(addr),
            Ok(Err(e)) => return LaunchWait::SpawnFailed(e),
            Err(RecvTimeoutError::Disconnected) => return LaunchWait::Panicked,
            Err(RecvTimeoutError::Timeout) => {
                let token = progress_token();
                if token != last_token {
                    last_token = token;
                    last_progress = std::time::Instant::now();
                }
                if last_progress.elapsed() >= idle_deadline {
                    return LaunchWait::IdleTimeout;
                }
            }
        }
    }
}

/// The launch-progress token [`await_launch`] watches.
fn dispatch_progress_token(project: &str, task_id: &str, workspace: &str) -> u64 {
    let Ok(path) = shelbi_state::events_log_path() else {
        return 0;
    };
    let Ok(text) = std::fs::read_to_string(&path) else {
        return 0;
    };
    count_dispatch_events(&text, project, task_id, workspace, &[])
}

/// Does the launch look genuinely complete? (Live non-user-shell pane + a
/// confirm-level dispatch signal.)
fn launch_appears_complete(
    project_yaml: &shelbi_core::Project,
    workspace: &shelbi_core::WorkspaceSpec,
    task_id: &str,
) -> bool {
    let Some(machine) = project_yaml.machine(&workspace.machine) else {
        return false;
    };
    let host = machine.host();
    let Ok(addr) = crate::workspace::workspace_target(project_yaml, workspace) else {
        return false;
    };
    let alive = matches!(
        crate::workspace::probe_workspace_slot(&host, &addr, crate::workspace::probe_deadline()),
        crate::workspace::SlotProbe::Alive { user_shell: false }
    );
    if !alive {
        return false;
    }
    let text = shelbi_state::events_log_path()
        .ok()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_default();
    count_dispatch_events(
        &text,
        &project_yaml.name,
        task_id,
        &workspace.name,
        &["confirmed", "unverified"],
    ) > 0
}

/// Count `dispatch … status=…` lines for one project/task/workspace.
fn count_dispatch_events(
    log: &str,
    project: &str,
    task_id: &str,
    workspace: &str,
    statuses: &[&str],
) -> u64 {
    let proj_tok = format!("project={project} ");
    let task_tok = format!("task={task_id} ");
    let ws_tok = format!("workspace={workspace} ");
    log.lines()
        .filter(|l| l.contains(" dispatch ") && l.contains(&task_tok) && l.contains(&ws_tok))
        .filter(|l| !l.contains("project=") || l.contains(&proj_tok))
        .filter(|l| {
            statuses.is_empty()
                || statuses.iter().any(|s| {
                    l.contains(&format!("status={s} ")) || l.ends_with(&format!("status={s}"))
                })
        })
        .count() as u64
}

/// Compose the dispatch event's `reason=` by appending the resolved agent name.
fn dispatch_reason_with_agent(base: &str, agent: &str) -> String {
    format!("{base} agent={agent}")
}

/// The workspace, if any, whose pane a dispatch supplants.
fn supplanted_workspace<'a>(prev_assigned: Option<&'a str>, new_workspace: &str) -> Option<&'a str> {
    match prev_assigned {
        Some(prev) if prev != new_workspace => Some(prev),
        _ => None,
    }
}

// --- workflow / agent resolution (ported from commands/issue.rs) ----------

/// Load the workflow assigned to `issue`, falling back to the built-in default
/// with a stderr warning when it can't be loaded.
fn resolve_task_workflow(project: &str, issue: &Issue, sink: &mut dyn OutputSink) -> Workflow {
    let project_yaml = shelbi_state::load_project(project).ok();
    let name = project_yaml
        .as_ref()
        .map(|p| shelbi_state::resolve_task_workflow_name(p, issue))
        .unwrap_or_else(|| issue.workflow_or_default());
    match shelbi_state::load_workflow(project, name) {
        Ok(wf) => wf,
        Err(e) => {
            sink.warn(&format!(
                "warning: workflow `{name}` could not be loaded ({e}); using built-in default"
            ));
            default_workflow()
        }
    }
}

/// The workflow status `issue start` lands a card in (canonical `in-progress`,
/// else the first active-category status).
fn start_destination_status(workflow: &Workflow) -> Option<&WorkflowStatus> {
    workflow
        .status(Column::in_progress().as_str())
        .or_else(|| {
            workflow
                .statuses
                .iter()
                .find(|s| s.category == StatusCategory::Active)
        })
}

/// Where a dispatch lands the card (keeping a card parked in an agent-owned
/// active gate in place).
fn dispatch_destination_status<'a>(
    workflow: &'a Workflow,
    current_column: &Column,
) -> Option<&'a WorkflowStatus> {
    if let Some(status) = workflow.status(current_column.as_str()) {
        if status.category == StatusCategory::Active && status.owner == Owner::Agent {
            return Some(status);
        }
    }
    start_destination_status(workflow)
}

/// The required workspace tags of the status `issue start` lands the card in.
fn required_active_tags(
    project: &str,
    issue: &Issue,
    sink: &mut dyn OutputSink,
) -> std::collections::BTreeSet<String> {
    let workflow = resolve_task_workflow(project, issue, sink);
    dispatch_destination_status(&workflow, &issue.column)
        .map(|s| s.tags.iter().cloned().collect())
        .unwrap_or_default()
}

fn resolve_active_agent_for_dispatch(
    project: &str,
    issue: &Issue,
    sink: &mut dyn OutputSink,
) -> Result<String, MutateError> {
    use crate::dispatch::{resolve_dispatch_agent, DispatchDecision};
    use shelbi_state::DEVELOPER_AGENT;

    let workflow = resolve_task_workflow(project, issue, sink);
    let active = dispatch_destination_status(&workflow, &issue.column);

    let zen_on = matches!(
        shelbi_state::read_state(project).map(|s| s.zen_mode),
        Ok(shelbi_state::ZenModeState::On),
    );

    let Some(status) = active else {
        return Ok(DEVELOPER_AGENT.to_string());
    };

    match resolve_dispatch_agent(status, zen_on) {
        DispatchDecision::Dispatch { agent } => Ok(agent),
        DispatchDecision::Skip(reason) => {
            sink.warn(&format!(
                "shelbi: workflow `{}` active status had no dispatchable agent \
                 ({}); falling back to `{DEVELOPER_AGENT}`",
                workflow.name,
                reason.human_message(),
            ));
            Ok(DEVELOPER_AGENT.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn await_launch_reports_completion_spawn_failure_and_panic() {
        // Completion.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Ok(SessionTarget::pane("%1"))).unwrap();
        assert!(matches!(
            await_launch(&rx, Duration::from_secs(5), Duration::from_millis(10), || 0),
            LaunchWait::Completed(_)
        ));

        // Spawn failure.
        let (tx, rx) = std::sync::mpsc::channel();
        tx.send(Err(shelbi_core::Error::Other("boom".into()))).unwrap();
        assert!(matches!(
            await_launch(&rx, Duration::from_secs(5), Duration::from_millis(10), || 0),
            LaunchWait::SpawnFailed(_)
        ));

        // Panic (sender dropped without sending).
        let (tx, rx) = std::sync::mpsc::channel::<shelbi_core::Result<SessionTarget>>();
        drop(tx);
        assert!(matches!(
            await_launch(&rx, Duration::from_secs(5), Duration::from_millis(10), || 0),
            LaunchWait::Panicked
        ));
    }

    #[test]
    fn await_launch_times_out_on_silence_but_a_moving_launch_extends_its_deadline() {
        // Silence past the deadline → IdleTimeout.
        let (_tx, rx) = std::sync::mpsc::channel::<shelbi_core::Result<SessionTarget>>();
        assert!(matches!(
            await_launch(&rx, Duration::from_millis(30), Duration::from_millis(10), || 0),
            LaunchWait::IdleTimeout
        ));

        // A token that keeps increasing resets the idle clock, so the launch is
        // never declared stuck while it is still moving; once it stops moving it
        // times out. Bound the counter so the test still terminates.
        let (_tx, rx) = std::sync::mpsc::channel::<shelbi_core::Result<SessionTarget>>();
        let mut n = 0u64;
        let out = await_launch(&rx, Duration::from_millis(30), Duration::from_millis(5), || {
            n += 1;
            n.min(10)
        });
        assert!(matches!(out, LaunchWait::IdleTimeout));
    }

    #[test]
    fn count_dispatch_events_scopes_by_project_task_workspace_and_status() {
        let log = "\
t1 dispatch project=p task=a workspace=w status=message-channel\n\
t2 dispatch project=p task=a workspace=w status=confirmed\n\
t3 dispatch project=p task=b workspace=w status=confirmed\n\
t4 dispatch project=p task=a workspace=v status=confirmed\n";
        assert_eq!(count_dispatch_events(log, "p", "a", "w", &[]), 2);
        assert_eq!(count_dispatch_events(log, "p", "a", "w", &["confirmed"]), 1);
        assert_eq!(count_dispatch_events(log, "p", "b", "w", &[]), 1);
    }

    #[test]
    fn count_dispatch_events_is_scoped_to_the_project() {
        let log = "\
t1 dispatch project=p task=a workspace=w status=confirmed\n\
t2 dispatch project=q task=a workspace=w status=confirmed\n\
t3 dispatch task=a workspace=w status=confirmed\n";
        // The legacy line without `project=` still counts (task+workspace).
        assert_eq!(count_dispatch_events(log, "p", "a", "w", &[]), 2);
    }

    #[test]
    fn dispatch_reason_appends_agent_segment() {
        assert_eq!(
            dispatch_reason_with_agent("user:cli:start", "developer"),
            "user:cli:start agent=developer"
        );
    }

    #[test]
    fn supplanted_workspace_only_fires_on_a_different_prior_assignee() {
        assert_eq!(supplanted_workspace(Some("a"), "b"), Some("a"));
        assert_eq!(supplanted_workspace(Some("a"), "a"), None);
        assert_eq!(supplanted_workspace(None, "b"), None);
    }
}
