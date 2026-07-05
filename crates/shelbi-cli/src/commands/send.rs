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
use chrono::Utc;
use shelbi_core::{Host, Project, Status, TmuxAddr};
use shelbi_orchestrator::workspace as orch_workspace;

use super::require_project;

pub fn run(project: Option<String>, id: String, message: String) -> Result<()> {
    let project_name = require_project(project)?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;

    match resolve_target(&project, &id)? {
        ResolvedTarget::Workspace { host, addr } => {
            // Pane must be live — the workspace can be declared but idle, in
            // which case there's no runner to send to. Surface that as an
            // actionable error rather than the opaque `os error 2` the
            // legacy path produced.
            let alive = orch_workspace::workspace_pane_alive(&host, &addr)
                .map_err(|e| anyhow!(e))?;
            if !alive {
                return Err(anyhow!(
                    "workspace `{id}` has no live tmux pane at `{}` — open it with \
                     `shelbi workspace open {id}` (or `shelbi task start <task-id>` to \
                     dispatch a task onto it)",
                    addr.target(),
                ));
            }
            shelbi_tmux::send_line(&host, &addr, &message).map_err(|e| anyhow!(e))?;
            println!("✓ sent to {} ({})", id, addr.target());
            Ok(())
        }
        ResolvedTarget::LegacyAgent { host, addr } => {
            shelbi_tmux::send_line(&host, &addr, &message).map_err(|e| anyhow!(e))?;
            // Legacy path keeps the agent-file housekeeping the old
            // implementation did — bumping `status: running` + `updated`
            // and appending to the per-agent log so `shelbi tail` still
            // shows the send.
            let mut file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
            file.agent.status = Status::Running;
            file.agent.updated = Utc::now();
            shelbi_state::save_agent(&project_name, &file.agent, &file.body)
                .map_err(|e| anyhow!(e))?;
            shelbi_state::append_log(&project_name, &id, &format!("send: {message}"))
                .map_err(|e| anyhow!(e))?;
            println!("✓ sent to {} ({})", id, addr.target());
            Ok(())
        }
    }
}

/// Where the message should land. We resolve once up front so the
/// send + housekeeping arms each have a single code path.
enum ResolvedTarget {
    /// Name matched a declared workspace; address derived from the
    /// project YAML + machine spec.
    Workspace { host: Host, addr: TmuxAddr },
    /// Name only matched a legacy spawn-based agent file. Address read
    /// from the agent's frontmatter.
    LegacyAgent { host: Host, addr: TmuxAddr },
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
            orch_workspace::workspace_tmux_addr(project, workspace).map_err(|e| anyhow!(e))?;
        return Ok(ResolvedTarget::Workspace {
            host: machine.host(),
            addr,
        });
    }

    // Fall through to the legacy `shelbi spawn` registry. `load_agent`
    // is what the old send did; we just swallow the not-found error and
    // turn it into a unified `unknown id` message that lists both
    // registries' members.
    match shelbi_state::load_agent(&project.name, id) {
        Ok(file) => {
            let machine = project.machine(&file.agent.machine).ok_or_else(|| {
                anyhow!("machine `{}` no longer in project", file.agent.machine)
            })?;
            Ok(ResolvedTarget::LegacyAgent {
                host: machine.host(),
                addr: file.agent.tmux.clone(),
            })
        }
        Err(_) => Err(anyhow!("{}", unknown_id_error(project, id))),
    }
}

/// Build the "unknown id" error message that lists every workspace name
/// and any legacy agent id we can find. The legacy lookup is best-effort
/// — if the directory is missing or unreadable we just leave that line
/// out rather than masking the real error.
fn unknown_id_error(project: &Project, id: &str) -> String {
    let mut lines = vec![format!(
        "unknown workspace/agent `{id}` in project `{}`",
        project.name
    )];
    if project.workspaces.is_empty() {
        lines.push("(no workspaces declared in project YAML)".to_string());
    } else {
        let names: Vec<&str> = project.workspaces.iter().map(|w| w.name.as_str()).collect();
        lines.push(format!("workspaces: {}", names.join(", ")));
    }
    if let Some(ids) = list_legacy_agent_ids(&project.name) {
        if !ids.is_empty() {
            lines.push(format!("legacy spawn agents: {}", ids.join(", ")));
        }
    }
    lines.join("\n  ")
}

/// Enumerate ids under `~/.shelbi/projects/<proj>/agents/*.md` (skipping
/// the `.log.md` companions). Returns `None` on any I/O failure so the
/// caller can quietly omit the line — this is only ever used to enrich
/// an error message.
fn list_legacy_agent_ids(project: &str) -> Option<Vec<String>> {
    let dir = shelbi_state::agents_dir(project).ok()?;
    if !dir.exists() {
        return Some(Vec::new());
    }
    let mut ids = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()? {
        let entry = entry.ok()?;
        let name = entry.file_name();
        let name = name.to_str()?;
        if name.ends_with(".log.md") || !name.ends_with(".md") {
            continue;
        }
        ids.push(name.trim_end_matches(".md").to_string());
    }
    ids.sort();
    Some(ids)
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
                dialog_signatures: vec![],
            },
        );
        Project {
            name: name.into(),
            repo: "/tmp/repo".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/repo".into(),
                    host: None,
                    tags: Vec::new(),
                },
                Machine {
                    name: "devbox".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/repo".into(),
                    host: Some("devbox".into()),
                    tags: Vec::new(),
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
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        }
    }

    /// A local workspace resolves into a Workspace target with a
    /// `shelbi-<project>:<name>` tmux address — the same one the dashboard
    /// session uses for the workspace's window.
    #[test]
    fn local_workspace_resolves_to_dashboard_window() {
        let project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        match resolve_target(&project, "alpha").unwrap() {
            ResolvedTarget::Workspace { host, addr } => {
                assert!(host.is_local());
                assert_eq!(addr.session, "shelbi-demo");
                assert_eq!(addr.window, "alpha");
            }
            ResolvedTarget::LegacyAgent { .. } => {
                panic!("expected workspace resolution, got legacy")
            }
        }
    }

    /// A remote workspace resolves into a Workspace target with the
    /// per-workspace `shelbi-w-<name>` session that lives on the remote
    /// host's tmux server.
    #[test]
    fn remote_workspace_resolves_to_per_workspace_session() {
        let project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "delta".into(),
                machine: "devbox".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        match resolve_target(&project, "delta").unwrap() {
            ResolvedTarget::Workspace { host, addr } => {
                assert!(matches!(host, Host::Ssh { ref host } if host == "devbox"));
                assert_eq!(addr.session, "shelbi-w-delta");
                assert_eq!(addr.window, "agent");
            }
            ResolvedTarget::LegacyAgent { .. } => {
                panic!("expected workspace resolution, got legacy")
            }
        }
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
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "bravo".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                    tags: Vec::new(),
                    slot: None,
                },
            ],
        );
        let msg = unknown_id_error(&project, "charlie");
        assert!(msg.contains("unknown workspace/agent `charlie`"), "msg: {msg}");
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
