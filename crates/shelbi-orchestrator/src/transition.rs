//! Transition executor — fire the action primitives a workflow's
//! `from -> to` transition declares.
//!
//! The workflow schema lets an author attach an ordered `actions:` list
//! (and an optional `target:` branch override) to each transition edge —
//! see [`shelbi_core::Transition`] and `Plans/workflows.md` §12. Every
//! [`TransitionAction`] maps 1:1 onto a function in [`crate::actions`].
//! This module is the glue that, given a concrete task and a `from -> to`
//! move, resolves the target's `{{var}}` placeholders and runs the
//! declared actions in order — the automatic counterpart to invoking the
//! `shelbi action <verb>` primitives by hand.
//!
//! Before this module existed, `Transition.actions` / `Transition.target`
//! were parsed, validated, and round-tripped but never executed (the
//! dead-code half of core-model finding F5). The primitives themselves
//! already took a `target_override`; all that was missing was the walker
//! that turns a declared edge into the ordered primitive calls.
//!
//! **Short-circuit semantics.** The first action that errors stops the
//! run; the remaining actions do not fire. This matches the ordering
//! contract documented on [`shelbi_core::Transition::actions`] ("failures
//! short-circuit the rest") — a `[merge, delete_branch]` edge must never
//! delete the branch when the merge itself failed.

use std::time::{Duration, Instant};

use shelbi_core::{Error, Host, Project, Result, Task, TransitionAction, Workflow};

use crate::actions;
use crate::workspace::workspace_worktree;

/// How long to sleep between [`Transition::ready`] poll attempts.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// One executed action plus the one-line result it produced — the same
/// wire line the matching `shelbi action` subcommand prints, so a caller
/// can log or grep the outcome without re-deriving it. [`merge`] can
/// contribute multiple lines (the merge line plus one per restacked
/// child); they are joined with `\n` into a single `line`.
///
/// [`merge`]: crate::actions::merge
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionOutcome {
    pub action: TransitionAction,
    pub line: String,
}

/// Walk the `from -> to` transition's declared actions and fire each one
/// in declaration order, returning one [`ActionOutcome`] per action.
///
/// An undeclared edge — or one whose `actions:` list is empty — is a
/// clean no-op: an empty `Vec`, not an error, matching the "no
/// side-effects" semantics of [`Workflow::actions_for_transition`].
///
/// The transition's `target:` override (if any) is resolved once up front
/// via [`Workflow::resolve_transition_target`] — substituting `{{var}}`
/// placeholders from the task's frontmatter params — and threaded into
/// the `merge` / `open_pr` primitives; every other primitive ignores it.
/// A `target:` naming a param the task doesn't provide fails the whole
/// transition before any action runs (`Error::MissingTaskParams`).
pub fn execute_transition(
    project: &Project,
    project_name: &str,
    task: &Task,
    task_body: &str,
    workflow: &Workflow,
    from: &str,
    to: &str,
) -> Result<Vec<ActionOutcome>> {
    let target = workflow.resolve_transition_target(from, to, &task.string_params())?;
    let mut outcomes = Vec::new();
    for &action in workflow.actions_for_transition(from, to) {
        let line = run_action(
            project,
            project_name,
            task,
            task_body,
            action,
            target.as_deref(),
        )?;
        outcomes.push(ActionOutcome { action, line });
    }

    // Shell `run:` commands compose with — and run *after* — the git
    // actions on the same edge, sharing its short-circuit contract: the
    // git actions have all succeeded by the time we get here, and the
    // first failing `run:` command aborts the edge (leaving the earlier
    // git side-effects done, exactly as a mid-list action failure would).
    if let Some(transition) = workflow.transition(from, to) {
        if !transition.run.is_empty() || transition.ready.is_some() {
            run_shell_commands(project, task, transition)?;
        }
    }

    Ok(outcomes)
}

/// Execute a transition's `run:` commands (in order) and then poll its
/// optional `ready` probe, all in the task's worktree on its assigned
/// workspace's machine.
///
/// Host-routed via [`shelbi_ssh::run`] so it works on remote machines: each
/// command is a `sh -c` script prefixed with `cd <worktree>` and the
/// exported `$SLOT` / `$SHELBI_*` env. A non-zero exit short-circuits the
/// remaining commands (and the rest of the edge).
///
/// **Blocking mechanic (documented on [`shelbi_core::Transition::run`]):**
/// `shelbi_ssh::run` waits for each command to exit, so a long-running
/// server must background itself; the `ready` probe is what confirms it is
/// actually serving.
fn run_shell_commands(
    project: &Project,
    task: &Task,
    transition: &shelbi_core::Transition,
) -> Result<()> {
    // Resolve the worktree/host/slot from the task's assigned workspace.
    // A transition with commands but no assignment can't know *where* to
    // run — surface that rather than silently running on the hub.
    let workspace_name = task.assigned_to.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "transition {} -> {} declares `run:`/`ready:` commands but task `{}` \
             has no assigned workspace to run them in",
            transition.from, transition.to, task.id
        ))
    })?;
    let workspace = project.workspace(workspace_name).ok_or_else(|| {
        Error::Other(format!(
            "task `{}` is assigned to workspace `{workspace_name}`, which is not \
             declared in project `{}`",
            task.id, project.name
        ))
    })?;
    let machine = project.machine(&workspace.machine).ok_or_else(|| {
        Error::UnknownMachine(workspace.machine.clone())
    })?;
    let host = machine.host();
    let worktree = workspace_worktree(machine, workspace);
    let worktree = worktree.to_string_lossy().into_owned();
    let slot = project.workspace_slot(workspace);
    let branch = task
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{}", task.id));

    let env = TransitionEnv {
        slot,
        task: &task.id,
        branch: &branch,
        worktree: &worktree,
        machine: &workspace.machine,
    };

    for cmd in &transition.run {
        tracing::info!(
            task = %task.id, from = %transition.from, to = %transition.to,
            command = %cmd, "transition run"
        );
        let out = shelbi_ssh::run(&host, ["sh", "-c", &env.script(cmd)])
            .map_err(Error::Io)?;
        if !out.status.success() {
            // Short-circuit: mirror the git-action failure contract.
            return Err(Error::Command {
                cmd: cmd.clone(),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    if let Some(probe) = &transition.ready {
        wait_ready(&host, &env, probe, transition.ready_timeout_or_default())?;
    }

    Ok(())
}

/// The env exported into every `run:` / `ready` command, plus the helper
/// that wraps a user command into a `sh -c` script that `cd`s into the
/// worktree and exports the variables first.
struct TransitionEnv<'a> {
    slot: u32,
    task: &'a str,
    branch: &'a str,
    worktree: &'a str,
    machine: &'a str,
}

impl TransitionEnv<'_> {
    /// Build the `sh -c` script body for `command`: export the transition
    /// env, `cd` into the worktree (failing the whole command if it's
    /// gone), then run the user's command. Every interpolated value is
    /// shell-escaped so a path with spaces or a branch with shell
    /// metacharacters can't break out of the assignment.
    fn script(&self, command: &str) -> String {
        let esc = shelbi_core::shell_escape;
        format!(
            "export SLOT={slot}\n\
             export SHELBI_TASK={task}\n\
             export SHELBI_BRANCH={branch}\n\
             export SHELBI_WORKTREE={worktree}\n\
             export SHELBI_MACHINE={machine}\n\
             cd {worktree} || exit 1\n\
             {command}",
            slot = self.slot,
            task = esc(self.task),
            branch = esc(self.branch),
            worktree = esc(self.worktree),
            machine = esc(self.machine),
            command = command,
        )
    }
}

/// Poll `probe` (in the transition env) until it exits 0 or `timeout`
/// elapses. A timeout is an edge failure, matching the short-circuit
/// contract — a serve edge whose server never came up must not be reported
/// as clean.
fn wait_ready(
    host: &Host,
    env: &TransitionEnv<'_>,
    probe: &str,
    timeout: Duration,
) -> Result<()> {
    let script = env.script(probe);
    let started = Instant::now();
    loop {
        match shelbi_ssh::run(host, ["sh", "-c", &script]) {
            Ok(out) if out.status.success() => {
                tracing::info!(command = %probe, "transition ready probe succeeded");
                return Ok(());
            }
            Ok(_) => {} // not ready yet
            Err(e) => {
                // Transport failure (unreachable host, etc.) is fatal — no
                // amount of polling fixes it.
                return Err(Error::Io(e));
            }
        }
        if started.elapsed() >= timeout {
            return Err(Error::Other(format!(
                "transition ready probe `{probe}` did not succeed within {}s",
                timeout.as_secs()
            )));
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    }
}

/// Dispatch a single [`TransitionAction`] to its [`crate::actions`]
/// primitive, returning the primitive's one-line result.
fn run_action(
    project: &Project,
    project_name: &str,
    task: &Task,
    task_body: &str,
    action: TransitionAction,
    target: Option<&str>,
) -> Result<String> {
    match action {
        TransitionAction::PushBranch => {
            actions::push_branch(project, task)?;
            Ok("pushed".to_string())
        }
        TransitionAction::OpenPr => {
            let pr = actions::open_pr(project, project_name, task, task_body, target)?;
            Ok(pr.to_string())
        }
        TransitionAction::ClosePr => match actions::close_pr(project, task)? {
            Some(pr) => Ok(pr.to_string()),
            None => Ok("none".to_string()),
        },
        TransitionAction::Merge => {
            let result = actions::merge(project, project_name, task, target)?;
            let mut lines = vec![result.merge.as_line()];
            lines.extend(result.restacks.iter().map(|r| r.as_line()));
            Ok(lines.join("\n"))
        }
        TransitionAction::DeleteBranch => Ok(actions::delete_branch(project, task)?.as_line()),
        // `restack` isn't a self-contained edge action: it needs the
        // parent branch it's rebasing *from*, which a `from -> to` status
        // move doesn't carry. It's fired automatically by `merge` on
        // every dependent task (see `actions::merge` → `restack_children`),
        // so a bare `restack` in a transition's `actions:` is always a
        // mistake — reject it with a message that points at the fix
        // rather than silently rebasing onto the wrong base.
        TransitionAction::Restack => Err(Error::Other(format!(
            "transition action `restack` is not a standalone edge action — it is \
             fired automatically by `merge` on every dependent task; drop it from \
             the transition's `actions:` (task `{}`)",
            task.id
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec, ZenConfig,
    };
    use std::collections::BTreeMap;

    /// Minimal project — enough to satisfy the `execute_transition`
    /// signature. The tests here only exercise paths that return before
    /// any action touches git/gh, so the machine/workspace surface is
    /// deliberately empty.
    fn bare_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "fixture".into(),
            repo: "/tmp/fixture".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp/fixture".into(),
                host: None,
                tags: Vec::new(),
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
        }
    }

    fn bare_task(id: &str) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: shelbi_core::Column::Review,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: Some(format!("shelbi/{id}")),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            params: BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// A workflow whose `review -> done` edge declares `actions`. Uses the
    /// explicit-transitions form so `actions_for_transition` has something
    /// to walk.
    fn workflow_with_edge(actions_yaml: &str, target: Option<&str>) -> Workflow {
        let target_line = target
            .map(|t| format!(", target: \"{t}\""))
            .unwrap_or_default();
        let yaml = format!(
            r#"
name: default
statuses:
  - {{ id: in-progress, name: In Progress, category: active,  owner: agent, agent: developer    }}
  - {{ id: review,      name: Review,      category: handoff, owner: user,  agent: orchestrator }}
  - {{ id: done,        name: Done,        category: done,    owner: user                       }}
transitions:
  - {{ from: review, to: done, actions: [{actions_yaml}]{target_line} }}
"#
        );
        Workflow::from_yaml_str(&yaml).expect("workflow parses")
    }

    #[test]
    fn undeclared_edge_is_a_clean_noop() {
        // `in-progress -> review` has no transition declared, so nothing
        // fires and we get an empty outcome list — not an error, and no
        // git is touched (the empty project would blow up if it were).
        let wf = workflow_with_edge("merge", None);
        let task = bare_task("t-1");
        let out = execute_transition(
            &bare_project(),
            "fixture",
            &task,
            "body",
            &wf,
            "in-progress",
            "review",
        )
        .expect("no-op transition");
        assert!(out.is_empty(), "undeclared edge should run nothing: {out:?}");
    }

    #[test]
    fn empty_action_list_is_a_clean_noop() {
        let wf = workflow_with_edge("", None);
        let task = bare_task("t-1");
        let out =
            execute_transition(&bare_project(), "fixture", &task, "body", &wf, "review", "done")
                .expect("empty-action transition");
        assert!(out.is_empty(), "empty actions should run nothing: {out:?}");
    }

    #[test]
    fn standalone_restack_action_is_rejected() {
        // `restack` needs a parent branch a status move doesn't carry, so
        // the executor rejects it with an actionable message instead of
        // guessing a base. The error must fire before any git call.
        let wf = workflow_with_edge("restack", None);
        let task = bare_task("t-1");
        let err = execute_transition(
            &bare_project(),
            "fixture",
            &task,
            "body",
            &wf,
            "review",
            "done",
        )
        .expect_err("restack must be rejected");
        let msg = err.to_string();
        assert!(msg.contains("restack"), "{msg}");
        assert!(msg.contains("t-1"), "{msg}");
    }

    #[test]
    fn missing_target_param_fails_before_any_action() {
        // A `target: release/{{version}}` with no `version` param must
        // fail the whole transition up front (MissingTaskParams), never
        // reaching the merge primitive.
        let wf = workflow_with_edge("merge", Some("release/{{version}}"));
        let task = bare_task("t-1");
        let err = execute_transition(
            &bare_project(),
            "fixture",
            &task,
            "body",
            &wf,
            "review",
            "done",
        )
        .expect_err("missing target param must fail");
        let msg = err.to_string();
        assert!(msg.contains("version"), "error should name the param: {msg}");
    }

    // ---- run: / ready: shell-command execution ---------------------------

    use shelbi_core::WorkspaceSpec;

    /// A local project whose single workspace `alpha` lives under a temp
    /// `work_dir`, with its worktree directory materialized so `run:`
    /// commands have somewhere to `cd`.
    fn local_project_with_worktree(work_dir: &std::path::Path) -> Project {
        let mut p = bare_project();
        p.machines[0].work_dir = work_dir.to_path_buf();
        p.workspaces = vec![WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "claude".into(),
            tags: Vec::new(),
            slot: None,
        }];
        // Materialize <work_dir>/.shelbi/wt/alpha.
        let wt = work_dir.join(".shelbi").join("wt").join("alpha");
        std::fs::create_dir_all(&wt).unwrap();
        p
    }

    fn task_assigned(id: &str, workspace: &str) -> Task {
        let mut t = bare_task(id);
        t.assigned_to = Some(workspace.into());
        t
    }

    fn workflow_with_run(run: &[&str], ready: Option<&str>, ready_timeout: Option<u64>) -> Workflow {
        let run_lines: String = run.iter().map(|c| format!("\n      - {c:?}")).collect();
        let ready_line = ready
            .map(|r| format!("\n    ready: {r:?}"))
            .unwrap_or_default();
        let timeout_line = ready_timeout
            .map(|s| format!("\n    ready_timeout: {s}"))
            .unwrap_or_default();
        let yaml = format!(
            r#"
name: default
statuses:
  - {{ id: in-progress, name: In Progress, category: active,  owner: agent, agent: developer    }}
  - {{ id: serve,       name: Serve,       category: handoff, owner: user,  agent: orchestrator }}
transitions:
  - from: in-progress
    to: serve
    run:{run_lines}{ready_line}{timeout_line}
"#
        );
        Workflow::from_yaml_str(&yaml).expect("workflow parses")
    }

    #[test]
    fn run_commands_execute_in_worktree_with_slot_env() {
        let dir = tempfile::tempdir().unwrap();
        let project = local_project_with_worktree(dir.path());
        let wt = dir.path().join(".shelbi").join("wt").join("alpha");

        // The command writes $SLOT and the worktree $PWD out to a file so
        // the test can assert both the env injection and the `cd`.
        let wf = workflow_with_run(
            &["printf '%s %s' \"$SLOT\" \"$PWD\" > env.out"],
            None,
            None,
        );
        let task = task_assigned("t-1", "alpha");
        let out = execute_transition(&project, "fixture", &task, "body", &wf, "in-progress", "serve")
            .expect("run commands succeed");
        // No git actions declared, so the outcome list is empty.
        assert!(out.is_empty());

        let written = std::fs::read_to_string(wt.join("env.out")).expect("env.out written");
        // slot defaults to declaration index 0; PWD is the worktree. `cd`
        // keeps the logical path, so we assert the tail rather than a
        // canonicalized match (macOS temp dirs are /var → /private symlinks).
        assert!(
            written.starts_with("0 "),
            "SLOT should be 0, got: {written:?}"
        );
        assert!(
            written.trim_end().ends_with("/.shelbi/wt/alpha"),
            "command should run in the worktree, got: {written:?}"
        );
    }

    #[test]
    fn failing_run_command_short_circuits() {
        let dir = tempfile::tempdir().unwrap();
        let project = local_project_with_worktree(dir.path());
        let wt = dir.path().join(".shelbi").join("wt").join("alpha");

        // First command fails; the second must never run.
        let wf = workflow_with_run(&["exit 3", "touch should_not_exist"], None, None);
        let task = task_assigned("t-1", "alpha");
        let err = execute_transition(
            &project, "fixture", &task, "body", &wf, "in-progress", "serve",
        )
        .expect_err("failing command aborts the edge");
        assert!(err.to_string().contains("exit 3") || matches!(err, Error::Command { .. }));
        assert!(
            !wt.join("should_not_exist").exists(),
            "second command must not run after the first fails"
        );
    }

    #[test]
    fn ready_probe_polls_until_success() {
        let dir = tempfile::tempdir().unwrap();
        let project = local_project_with_worktree(dir.path());

        // The run command creates the readiness marker; the probe checks it.
        let wf = workflow_with_run(&["touch ready.marker"], Some("test -f ready.marker"), Some(5));
        let task = task_assigned("t-1", "alpha");
        execute_transition(&project, "fixture", &task, "body", &wf, "in-progress", "serve")
            .expect("ready probe should pass once the marker exists");
    }

    #[test]
    fn ready_probe_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let project = local_project_with_worktree(dir.path());

        // No run command creates the marker, so the probe never succeeds;
        // a 1s timeout keeps the test fast.
        let wf = workflow_with_run(&[], Some("test -f never.marker"), Some(1));
        let task = task_assigned("t-1", "alpha");
        let err = execute_transition(
            &project, "fixture", &task, "body", &wf, "in-progress", "serve",
        )
        .expect_err("probe that never passes must time out");
        assert!(
            err.to_string().contains("ready probe"),
            "got: {err}"
        );
    }

    #[test]
    fn run_without_assignment_errors() {
        let dir = tempfile::tempdir().unwrap();
        let project = local_project_with_worktree(dir.path());
        let wf = workflow_with_run(&["true"], None, None);
        let task = bare_task("t-1"); // no assigned_to
        let err = execute_transition(
            &project, "fixture", &task, "body", &wf, "in-progress", "serve",
        )
        .expect_err("run without assignment can't resolve a worktree");
        assert!(
            err.to_string().contains("no assigned workspace"),
            "got: {err}"
        );
    }
}
