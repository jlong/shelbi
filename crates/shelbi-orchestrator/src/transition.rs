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

use shelbi_core::{Error, Project, Result, Task, TransitionAction, Workflow};

use crate::actions;

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
    Ok(outcomes)
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
            contextstore_sync: Vec::new(),
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
}
