//! Agent dispatch resolver — turn a workflow status + the project's Zen
//! state into a decision about whether (and as which agent) to spawn a
//! workspace.
//!
//! See `Plans/agents-workspaces.md` §5 for the rule set; the short
//! version: `owner: agent` always dispatches under the named agent;
//! `owner: user` only dispatches when an `agent:` is declared AND Zen is
//! on. Terminal statuses (no `agent:`) never dispatch.
//!
//! Pure function with no I/O — the caller resolves the workflow + status
//! and reads `state.zen_mode` itself, then asks this module "given these
//! values, what do I do?" Splitting it out keeps the rule table unit-
//! testable without spinning up a SHELBI_HOME fixture.

use shelbi_core::{Issue, Owner, WorkflowStatus};

/// The outcome of resolving a status against the current automation
/// state. Either spawn this agent, or skip with a structured reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchDecision {
    /// Spawn the workspace with this agent's `instructions.md` + skills.
    Dispatch { agent: String },
    /// Don't spawn — the status has no automation path for the current
    /// Zen state.
    Skip(SkipReason),
}

/// Why we skipped a dispatch. Surfaced to the user / events log so a
/// "shelbi task start did nothing" outcome is explainable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// `owner: user` with no `agent:` field — terminal or fully-human
    /// status (Done, Canceled, …). No automation here, period.
    NoAgentForStatus { status_id: String },
    /// `owner: user` with an `agent:` field, but Zen is off. The status
    /// is human-driven by default; Zen unlocks the agent path.
    AgentRequiresZen { status_id: String, agent: String },
}

impl SkipReason {
    /// Short message suitable for CLI / events-log surfacing.
    pub fn human_message(&self) -> String {
        match self {
            SkipReason::NoAgentForStatus { status_id } => {
                format!("status `{status_id}` has no agent declared — no automation path")
            }
            SkipReason::AgentRequiresZen { status_id, agent } => {
                format!(
                    "status `{status_id}` would dispatch as `{agent}` but Zen mode is off; \
                     turn Zen on or override explicitly"
                )
            }
        }
    }
}

/// Resolve which agent should run for `status` given the current Zen
/// state.
///
/// Rules (see module docs):
///
/// - `owner: agent` → always dispatch as `status.agent` (the loader
///   guarantees this is `Some`).
/// - `owner: user`, no `agent:` → never dispatch.
/// - `owner: user`, `agent:` set, Zen on → dispatch as that agent.
/// - `owner: user`, `agent:` set, Zen off → skip (human-driven status).
///
/// When `status.owner == Agent` but the loader somehow let `agent` slip
/// through as `None`, we still return `Skip` with a clear status_id
/// reference so the dispatcher fails loudly rather than crashing — the
/// loader's hard error is the authoritative defense, this is belt-and-
/// suspenders.
pub fn resolve_dispatch_agent(status: &WorkflowStatus, zen_on: bool) -> DispatchDecision {
    match status.owner {
        Owner::Agent => match &status.agent {
            Some(a) => DispatchDecision::Dispatch { agent: a.clone() },
            None => DispatchDecision::Skip(SkipReason::NoAgentForStatus {
                status_id: status.id.clone(),
            }),
        },
        Owner::User => match &status.agent {
            None => DispatchDecision::Skip(SkipReason::NoAgentForStatus {
                status_id: status.id.clone(),
            }),
            Some(a) if zen_on => DispatchDecision::Dispatch { agent: a.clone() },
            Some(a) => DispatchDecision::Skip(SkipReason::AgentRequiresZen {
                status_id: status.id.clone(),
                agent: a.clone(),
            }),
        },
    }
}

/// Resolve which agent should drive `task` in the active status it is
/// **currently** sitting in, for a re-dispatch that isn't an explicit CLI
/// invocation (the supervisor's automatic pane relaunch). Infallible: any
/// failure to load the workflow, a workflow with no active-category status, or
/// a status the resolver would `Skip` all fall back to the bundled `developer`
/// agent so the relaunch still deploys *some* agent context into the worktree.
///
/// The card is resolved by its **exact status id** first: a task parked in a
/// custom agent-owned active status (e.g. a `review-app` gate declared between
/// `in-progress` and `review`) must re-dispatch as *that* status's agent, not
/// the workflow's primary active status. Only when the current status isn't
/// declared in the workflow, or resolves to no dispatchable agent, do we fall
/// back to the workflow's **primary** active status
/// ([`Workflow::primary_active_status`] — the canonical `in-progress`, or the
/// first active-category status when that id is absent) and then to the bundled
/// developer. Keying off the first active status alone was the dispatch half of
/// the custom-active-status bug: a relaunch of a `review-app` pane would
/// silently come back as the developer. Using `primary_active_status()` for the
/// fallback (rather than a bare "first active" scan) keeps this in step with the
/// ready-dispatch path even when a downstream gate is declared before
/// `in-progress`.
///
/// Mirrors the CLI's `resolve_active_agent_for_dispatch` but stays quiet
/// (no stderr diagnostics — there's no human at a prompt) and reads the
/// project's live Zen state so an `owner: user` active status only pulls in
/// its agent when Zen is on, matching the declarative dispatch rules.
pub fn resolve_active_agent(project_name: &str, task: &Issue) -> String {
    let workflow = shelbi_state::load_project(project_name)
        .and_then(|project| shelbi_state::load_task_workflow(project_name, &project, task))
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let zen_on = matches!(
        shelbi_state::read_state(project_name).map(|s| s.zen_mode),
        Ok(shelbi_state::ZenModeState::On),
    );
    resolve_active_agent_in(&workflow, task.column.as_str(), zen_on)
}

/// The pure re-dispatch decision behind [`resolve_active_agent`]: given a
/// resolved workflow, the id of the status the card is currently in, and the
/// project's Zen state, pick the agent to relaunch as. Split out with no I/O so
/// the exact-id-then-first-active fallback ladder is unit-testable without a
/// `SHELBI_HOME` fixture (matching this module's testability contract).
fn resolve_active_agent_in(
    workflow: &shelbi_core::Workflow,
    current_status_id: &str,
    zen_on: bool,
) -> String {
    // Exact-id first: the status the card is actually in wins over the
    // workflow's primary active status.
    if let Some(status) = workflow.status(current_status_id) {
        if let DispatchDecision::Dispatch { agent } = resolve_dispatch_agent(status, zen_on) {
            return agent;
        }
    }
    // Fallback: the *primary* active status — the developer's column
    // (`in-progress` if declared, else the first active-category status),
    // never a downstream gate that merely happens to be declared first.
    // `Workflow::primary_active_status` is the shared helper for "the
    // developer's column"; using it keeps this fallback in step with the
    // ready-dispatch and Zen-probe paths instead of re-deriving "first
    // active" here.
    match workflow.primary_active_status() {
        Some(status) => match resolve_dispatch_agent(status, zen_on) {
            DispatchDecision::Dispatch { agent } => agent,
            DispatchDecision::Skip(_) => shelbi_state::DEVELOPER_AGENT.to_string(),
        },
        None => shelbi_state::DEVELOPER_AGENT.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::StatusCategory;

    fn status(id: &str, owner: Owner, agent: Option<&str>) -> WorkflowStatus {
        WorkflowStatus {
            id: id.into(),
            name: id.into(),
            category: StatusCategory::Active,
            owner,
            agent: agent.map(str::to_string),
            tags: Vec::new(),
        }
    }

    #[test]
    fn owner_agent_dispatches_regardless_of_zen() {
        // `in-progress` in the default workflow: owner=agent, agent=developer.
        // The developer is doing the work whether Zen is on or off.
        let s = status("in-progress", Owner::Agent, Some("developer"));
        assert_eq!(
            resolve_dispatch_agent(&s, false),
            DispatchDecision::Dispatch {
                agent: "developer".into()
            },
        );
        assert_eq!(
            resolve_dispatch_agent(&s, true),
            DispatchDecision::Dispatch {
                agent: "developer".into()
            },
        );
    }

    #[test]
    fn owner_user_without_agent_never_dispatches() {
        // Terminal statuses (Done / Canceled) — no agent declared at all,
        // so even Zen leaves them alone.
        let s = status("done", Owner::User, None);
        assert!(matches!(
            resolve_dispatch_agent(&s, false),
            DispatchDecision::Skip(SkipReason::NoAgentForStatus { .. }),
        ));
        assert!(matches!(
            resolve_dispatch_agent(&s, true),
            DispatchDecision::Skip(SkipReason::NoAgentForStatus { .. }),
        ));
    }

    #[test]
    fn owner_user_with_agent_requires_zen_on() {
        // `review` in the two-field default: owner=user, agent=orchestrator.
        // Zen off keeps the human in the loop; Zen on hands it to the
        // orchestrator for merge-condition checks.
        let s = status("review", Owner::User, Some("orchestrator"));
        match resolve_dispatch_agent(&s, false) {
            DispatchDecision::Skip(SkipReason::AgentRequiresZen { status_id, agent }) => {
                assert_eq!(status_id, "review");
                assert_eq!(agent, "orchestrator");
            }
            other => panic!("expected AgentRequiresZen, got {other:?}"),
        }
        assert_eq!(
            resolve_dispatch_agent(&s, true),
            DispatchDecision::Dispatch {
                agent: "orchestrator".into()
            },
        );
    }

    #[test]
    fn full_default_workflow_dispatches_developer_for_active_status() {
        // Acceptance criterion (a): the canonical `in-progress` status
        // in the two-field default workflow has owner=agent,
        // agent=developer. The resolver must return Dispatch{developer}
        // regardless of Zen state.
        let wf = shelbi_core::Workflow::from_yaml_str(
            r#"
name: default
statuses:
  - { id: backlog,     name: Backlog,    category: backlog,  owner: user,  agent: orchestrator }
  - { id: in-progress, name: InProgress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,       category: done,     owner: user                       }
"#,
        )
        .unwrap();
        let active = wf.status("in-progress").unwrap();
        assert_eq!(
            resolve_dispatch_agent(active, false),
            DispatchDecision::Dispatch {
                agent: "developer".into()
            },
        );
        assert_eq!(
            resolve_dispatch_agent(active, true),
            DispatchDecision::Dispatch {
                agent: "developer".into()
            },
        );
    }

    #[test]
    fn user_owned_review_status_dispatches_orchestrator_under_zen() {
        // Acceptance criterion (b): owner=user + agent=orchestrator +
        // Zen on must resolve to "dispatch as orchestrator." This is
        // the declarative Zen path from agents-workspaces.md §4: the
        // orchestrator auto-runs merge-conditions on review without
        // waiting for the user.
        let wf = shelbi_core::Workflow::from_yaml_str(
            r#"
name: default
statuses:
  - { id: in-progress, name: InProgress, category: active,  owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff, owner: user,  agent: orchestrator }
"#,
        )
        .unwrap();
        let review = wf.status("review").unwrap();
        // Zen off → human-driven, no dispatch.
        assert!(matches!(
            resolve_dispatch_agent(review, false),
            DispatchDecision::Skip(SkipReason::AgentRequiresZen { .. }),
        ));
        // Zen on → orchestrator takes over.
        assert_eq!(
            resolve_dispatch_agent(review, true),
            DispatchDecision::Dispatch {
                agent: "orchestrator".into()
            },
        );
    }

    #[test]
    fn resolve_active_agent_falls_back_to_developer_without_fixtures() {
        // No project on disk → `load_workflow` errors and we fall back to the
        // built-in default workflow, whose active status is
        // owner=agent/agent=developer, so a supervised re-dispatch still
        // resolves a concrete agent to deploy into the worktree.
        let _guard = crate::test_lock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", tmp.path());

        let task = Issue {
            id: "fix-login".into(),
            title: "fix-login".into(),
            column: shelbi_core::Column::in_progress(),
            priority: 0,
            assigned_to: Some("alpha".into()),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            params: std::collections::BTreeMap::new(),
        };
        assert_eq!(resolve_active_agent("no-such-project", &task), "developer");

        match prev_home {
            Some(v) => std::env::set_var("SHELBI_HOME", v),
            None => std::env::remove_var("SHELBI_HOME"),
        }
    }

    /// A workflow with two active statuses: the canonical `in-progress` and a
    /// custom agent-owned `review-app` gate, mirroring the documented
    /// intermediate-agent workflow (statuses.yaml + workflow reference form).
    fn two_active_workflow() -> shelbi_core::Workflow {
        shelbi_core::Workflow::from_yaml_str(
            r#"
name: task
statuses:
  - { id: backlog,     name: Backlog,    category: backlog, owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,       category: ready,   owner: agent, agent: orchestrator }
  - { id: in-progress, name: InProgress, category: active,  owner: agent, agent: developer    }
  - { id: review-app,  name: ReviewApp,  category: active,  owner: agent, agent: review-app   }
  - { id: review,      name: Review,     category: handoff, owner: user,  agent: review       }
  - { id: done,        name: Done,       category: done,    owner: user                       }
"#,
        )
        .unwrap()
    }

    #[test]
    fn re_dispatch_resolves_custom_active_status_by_exact_id() {
        // A card parked in the custom `review-app` gate must re-dispatch as the
        // `review-app` agent — NOT the workflow's first active status
        // (`in-progress` → developer). This is the dispatch half of the
        // custom-active-status bug.
        let wf = two_active_workflow();
        assert_eq!(
            resolve_active_agent_in(&wf, "review-app", false),
            "review-app",
        );
        // Zen state doesn't change an owner:agent status's answer.
        assert_eq!(
            resolve_active_agent_in(&wf, "review-app", true),
            "review-app",
        );
    }

    #[test]
    fn re_dispatch_resolves_in_progress_to_developer() {
        // The canonical active status still resolves to its own agent by exact
        // id — the first-active fallback is not what answers here.
        let wf = two_active_workflow();
        assert_eq!(
            resolve_active_agent_in(&wf, "in-progress", false),
            "developer",
        );
    }

    #[test]
    fn re_dispatch_falls_back_to_first_active_for_unknown_status() {
        // A status id the workflow doesn't declare (stale board state) falls
        // back to the first active status (`in-progress` → developer).
        let wf = two_active_workflow();
        assert_eq!(resolve_active_agent_in(&wf, "ghost", false), "developer");
    }

    #[test]
    fn re_dispatch_falls_back_when_current_status_is_not_dispatchable() {
        // Sitting in a terminal / non-dispatchable status (owner:user, no
        // agent) falls back to the first active status rather than returning
        // no agent.
        let wf = two_active_workflow();
        assert_eq!(resolve_active_agent_in(&wf, "done", false), "developer");
    }

    /// Same two active statuses, but with the developer's `in-progress`
    /// declared *after* the `review-app` gate. Exercises the fallback's use of
    /// `primary_active_status()` (which prefers the `in-progress` id) over a
    /// naive "first active status in declaration order" scan, which would pick
    /// the gate here.
    fn gate_before_in_progress_workflow() -> shelbi_core::Workflow {
        shelbi_core::Workflow::from_yaml_str(
            r#"
name: task
statuses:
  - { id: backlog,     name: Backlog,    category: backlog, owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,       category: ready,   owner: agent, agent: orchestrator }
  - { id: review-app,  name: ReviewApp,  category: active,  owner: agent, agent: review-app   }
  - { id: in-progress, name: InProgress, category: active,  owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff, owner: user,  agent: review       }
  - { id: done,        name: Done,       category: done,    owner: user                       }
"#,
        )
        .unwrap()
    }

    #[test]
    fn re_dispatch_fallback_prefers_in_progress_even_when_declared_after_a_gate() {
        // Regression (acceptance criterion 4, dispatch half): a non-dispatchable
        // current status must fall back to the *developer's* column
        // (`in-progress`), not merely the first active status in declaration
        // order. Here the `review-app` gate is declared before `in-progress`,
        // so a naive "first active" scan would wrongly re-dispatch a stranded
        // card as the reviewer. `primary_active_status()` keys off the
        // `in-progress` id, so the fallback still lands on the developer.
        let wf = gate_before_in_progress_workflow();
        assert_eq!(resolve_active_agent_in(&wf, "done", false), "developer");
        assert_eq!(resolve_active_agent_in(&wf, "ghost", false), "developer");
    }

    #[test]
    fn exact_id_forward_and_bounce_transitions_stay_valid() {
        // Acceptance criterion (5): a custom active status's exact-id forward
        // (`review-app -> review`) and bounce (`review-app -> in-progress`)
        // transitions remain legal moves (any-to-any among declared ids).
        let wf = two_active_workflow();
        assert!(wf.transition_allowed("review-app", "review"));
        assert!(wf.transition_allowed("review-app", "in-progress"));
        assert!(wf.transition_allowed("in-progress", "review-app"));
        // An undeclared id is still rejected.
        assert!(!wf.transition_allowed("review-app", "ghost"));
    }

    #[test]
    fn skip_reason_human_message_names_status_and_agent() {
        let no_agent = SkipReason::NoAgentForStatus {
            status_id: "done".into(),
        };
        let msg = no_agent.human_message();
        assert!(msg.contains("done"), "got: {msg}");
        assert!(msg.contains("no automation"), "got: {msg}");

        let needs_zen = SkipReason::AgentRequiresZen {
            status_id: "review".into(),
            agent: "orchestrator".into(),
        };
        let msg = needs_zen.human_message();
        assert!(msg.contains("review"), "got: {msg}");
        assert!(msg.contains("orchestrator"), "got: {msg}");
        assert!(msg.contains("Zen"), "got: {msg}");
    }
}
