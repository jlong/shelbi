//! `shelbi send <name> <message>` — deliver `<message>` to a workspace's
//! runner pane.
//!
//! Resolution order:
//!
//! 1. If `<name>` matches a workspace declared in the project YAML, use
//!    the workspace-based tmux addressing (same registry as
//!    `shelbi workspace list` / `shelbi task start`). This is the
//!    canonical path — workspace panes are how every task-started agent
//!    runs today.
//!
//! 2. Otherwise fall back to the legacy `shelbi spawn` agent registry
//!    (`~/.shelbi/projects/<proj>/agents/<id>.md`). Kept so projects
//!    still using the pre-workspace flow keep working.
//!
//! An unknown name on both paths surfaces a single error that lists the
//! valid options across both registries so the user can spot a typo
//! without having to grep two places.
//!
//! Encountered as: "shelbi send bravo ..." failing with
//! `io: No such file or directory (os error 2)` because the previous
//! implementation consulted only the legacy registry, which is empty in
//! workspace-based projects.

use anyhow::{anyhow, Result};
use shelbi_app::diff::AgentDelivery;
use shelbi_core::{AgentRunnerSpec, Host, Project};
use shelbi_orchestrator::agent_send::send_verified;
use shelbi_orchestrator::session_backend::SessionTarget;
use shelbi_orchestrator::workspace as orch_workspace;

use super::require_project;

pub fn run(project: Option<String>, id: String, message: String) -> Result<()> {
    let project_name = require_project(project)?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let target = resolve_target(&project, &id)?;

    // Keep the text -> settle -> Enter sequence atomic with respect to every
    // other dispatch, restart, or send targeting this workspace. Dispatch and
    // resume hold this same lock across pane recreation and prompt delivery;
    // using it here also serializes concurrent CLI sends so their 300ms settle
    // windows cannot merge two messages into one Claude prompt. Legacy agent
    // ids use the same flat lock namespace.
    let _pane_injection_lock =
        shelbi_state::lock_workspace(&project_name, &id).map_err(|e| anyhow!(e))?;

    let ResolvedTarget { host, addr, runner } = target;
    // Session must be live — the workspace can be declared but idle, in which
    // case there's no runner to send to. Surface that as an actionable error.
    let alive = orch_workspace::workspace_pane_alive(&host, &addr).map_err(|e| anyhow!(e))?;
    if !alive {
        return Err(anyhow!(
            "workspace `{id}` has no live session at `{}` — open it with \
             `shelbi workspace open {id}` (or `shelbi task start <task-id>` to \
             dispatch a task onto it)",
            addr.label(),
        ));
    }
    let delivery = send_verified(&project_name, &id, &runner, &host, &addr, &message).map_err(|e| anyhow!(e))?;
    let delivery = match delivery {
        AgentDelivery::Submitted => "sent",
        AgentDelivery::Queued => "queued",
        AgentDelivery::Unverified => "sent (unverified)",
    };
    println!("✓ {delivery} to {} ({})", id, addr.label());
    Ok(())
}

/// Where the message should land. The name must match a declared workspace;
/// its session target is derived from the project YAML + machine spec.
#[derive(Debug)]
struct ResolvedTarget {
    host: Host,
    addr: SessionTarget,
    runner: AgentRunnerSpec,
}

fn resolve_target(project: &Project, id: &str) -> Result<ResolvedTarget> {
    if let Some(workspace) = project.workspace(id) {
        let machine = project.machine(&workspace.machine).ok_or_else(|| {
            anyhow!(
                "workspace `{id}` references unknown machine `{}`",
                workspace.machine
            )
        })?;
        let addr =
            orch_workspace::workspace_target(project, workspace).map_err(|e| anyhow!(e))?;
        // A workspace no longer selects a runner; a plain `send` to a slot
        // (no dispatched agent to resolve) targets the project's baseline
        // runner for its submit/prompt-injection profile.
        let runner = project.default_runner_spec().ok_or_else(|| {
            anyhow!(
                "project `{}` declares no runner (orchestrator runner `{}` not in agent_runners)",
                project.name,
                project.orchestrator.runner
            )
        })?;
        return Ok(ResolvedTarget {
            host: machine.host(),
            addr,
            runner: runner.clone(),
        });
    }

    Err(anyhow!("{}", unknown_id_error(project, id)))
}

/// Build the "unknown workspace" error message that lists every declared
/// workspace name.
fn unknown_id_error(project: &Project, id: &str) -> String {
    let mut lines = vec![format!("unknown workspace `{id}` in project `{}`", project.name)];
    if project.workspaces.is_empty() {
        lines.push("(no workspaces declared in project YAML)".to_string());
    } else {
        let names: Vec<&str> = project.workspaces.iter().map(|w| w.name.as_str()).collect();
        lines.push(format!("workspaces: {}", names.join(", ")));
    }
    lines.join("\n  ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
        WorkspaceSpec, ZenConfig,
    };
    use std::collections::BTreeMap;

    fn project_with_workspaces(name: &str, workspaces: Vec<WorkspaceSpec>) -> Project {
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
        runners.insert(
            "codex".to_string(),
            AgentRunnerSpec {
                command: "/opt/homebrew/bin/codex".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project { session: Default::default(),
            name: name.into(),
            label: None,
            display_name: None,
            repo: "/tmp/repo".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/repo".into(),
                    host: None,
                    tags: Vec::new(),
                    forward: None,
                },
                Machine {
                    name: "devbox".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/repo".into(),
                    host: Some("devbox".into()),
                    tags: Vec::new(),
                    forward: None,
                },
            ],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces,
            workspace_poll_interval_secs: 5,
            github_reconcile_interval_secs: 900,
            workspace_permissions_mode: Some("auto".into()),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            issue_tracker: Default::default(),
            disk: shelbi_core::DiskConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
            review: shelbi_core::ReviewConfig::default(),
        }
    }

    /// A local workspace resolves to its slot session
    /// (`shelbi-<project>:<name>` label).
    #[test]
    fn local_workspace_resolves_to_dashboard_window() {
        let project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        let t = resolve_target(&project, "alpha").unwrap();
        assert!(t.host.is_local());
        assert_eq!(t.addr.label(), "shelbi-demo:alpha");
        assert_eq!(t.runner.command, "claude");
    }

    /// A remote workspace resolves to its per-workspace `shelbi-w-<name>`
    /// session.
    #[test]
    fn remote_workspace_resolves_to_per_workspace_session() {
        let project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "delta".into(),
                machine: "devbox".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        let t = resolve_target(&project, "delta").unwrap();
        assert!(matches!(t.host, Host::Ssh { ref host } if host == "devbox"));
        assert_eq!(t.addr.label(), "shelbi-w-delta:agent");
        assert_eq!(t.runner.command, "claude");
    }

    #[test]
    fn workspace_send_uses_project_baseline_runner_for_submit_gating() {
        // A workspace no longer selects a runner; a plain `send` to a slot
        // targets the project's baseline (orchestrator) runner. With a codex
        // baseline, the codex submit profile (UI verifier, non-Claude UI) is
        // what gates delivery.
        let mut project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "bravo".into(),
                machine: "hub".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        project.orchestrator.runner = "codex".into();
        let t = resolve_target(&project, "bravo").unwrap();
        assert_eq!(t.runner.command, "/opt/homebrew/bin/codex");
        let profile = shelbi_orchestrator::submit::SubmitProfile::for_runner(&t.runner);
        assert!(profile.has_ui_verifier());
        assert!(!profile.uses_claude_ui());
    }

    #[test]
    fn unknown_workspace_errors_with_the_workspace_list() {
        let project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        let error = resolve_target(&project, "nope").unwrap_err();
        assert!(error.to_string().contains("unknown workspace `nope`"), "error: {error}");
        assert!(error.to_string().contains("alpha"), "error: {error}");
    }

    /// An unknown name on a project with no legacy agent files surfaces
    /// the workspace list — that's how the user spots a typo.
    #[test]
    fn unknown_id_error_lists_declared_workspaces() {
        let project = project_with_workspaces(
            "demo",
            vec![
                WorkspaceSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "bravo".into(),
                    machine: "hub".into(),
                    tags: Vec::new(),
                    slot: None,
                },
            ],
        );
        let msg = unknown_id_error(&project, "charlie");
        assert!(
            msg.contains("unknown workspace `charlie`"),
            "msg: {msg}"
        );
        assert!(msg.contains("alpha"), "msg: {msg}");
        assert!(msg.contains("bravo"), "msg: {msg}");
    }

    /// A project with no `workspaces:` block at all gets a hint pointing
    /// at the YAML — better than a bare "unknown" with no follow-up.
    #[test]
    fn unknown_id_error_calls_out_empty_workspaces() {
        let project = project_with_workspaces("demo", Vec::new());
        let msg = unknown_id_error(&project, "alpha");
        assert!(
            msg.contains("no workspaces declared"),
            "msg should mention empty pool: {msg}"
        );
    }
}
