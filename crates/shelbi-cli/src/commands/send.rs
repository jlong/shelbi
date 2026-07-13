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
use shelbi_core::{AgentRunnerSpec, Host, Project, Status, TmuxAddr};
use shelbi_orchestrator::submit::{PaneBaseline, SubmitProfile, SubmitStatus};
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

    match target {
        ResolvedTarget::Workspace { host, addr, runner } => {
            // Pane must be live — the workspace can be declared but idle, in
            // which case there's no runner to send to. Surface that as an
            // actionable error rather than the opaque `os error 2` the
            // legacy path produced.
            let alive =
                orch_workspace::workspace_pane_alive(&host, &addr).map_err(|e| anyhow!(e))?;
            if !alive {
                return Err(anyhow!(
                    "workspace `{id}` has no live tmux pane at `{}` — open it with \
                     `shelbi workspace open {id}` (or `shelbi task start <task-id>` to \
                     dispatch a task onto it)",
                    addr.target(),
                ));
            }
            let delivery = send_verified(&project_name, &id, &runner, &host, &addr, &message)?;
            println!("✓ {delivery} to {} ({})", id, addr.target());
            Ok(())
        }
        ResolvedTarget::LegacyAgent { host, addr, runner } => {
            let delivery = send_verified(&project_name, &id, &runner, &host, &addr, &message)?;
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
            println!("✓ {delivery} to {} ({})", id, addr.target());
            Ok(())
        }
    }
}

/// Human-facing success wording for a verified pane injection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SendDelivery {
    Submitted,
    /// Claude was already working. The separately-delivered Enter leaves the
    /// text in its visible queued-input area until the current turn ends; that
    /// is an accepted delivery, not a stuck idle prompt.
    Queued,
    /// The runner has no pane parser Shelbi knows how to verify. Text and
    /// Enter were still delivered through the shared race-safe primitive,
    /// but the CLI is explicit that no runner-specific submit signal exists.
    Unverified,
}

impl std::fmt::Display for SendDelivery {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SendDelivery::Submitted => f.write_str("sent"),
            SendDelivery::Queued => f.write_str("queued"),
            SendDelivery::Unverified => f.write_str("sent (unverified)"),
        }
    }
}

/// Route `shelbi send` through the orchestrator's shared verified-submit
/// primitive and record every verdict. A transport failure is also surfaced
/// as `status=stuck`: without that event, an orchestrator tailing events.log
/// would still silently assume the nudge arrived.
pub(super) fn send_verified(
    project: &str,
    id: &str,
    runner: &AgentRunnerSpec,
    host: &Host,
    addr: &TmuxAddr,
    message: &str,
) -> Result<SendDelivery> {
    let profile = SubmitProfile::for_runner(runner);
    let baseline = PaneBaseline::capture(host, addr, profile);
    let status = match shelbi_orchestrator::submit::send_verified(host, addr, message, &baseline) {
        Ok(status) => status,
        Err(e) => {
            shelbi_state::append_send_event(project, id, "stuck", "transport_error")
                .map_err(|log_err| {
                    anyhow!(
                        "sending to `{id}` failed ({e}); recording the stuck delivery also failed: {log_err}"
                    )
                })?;
            return Err(anyhow!("sending to `{id}` failed: {e}"));
        }
    };

    // A busy baseline alone is stale by the time the verifier has spent up
    // to two polling windows waiting. Claude may have completed that turn in
    // the meantime, leaving a genuinely wedged prompt in an idle input box.
    // Accept visible input as a queue only when the pane was busy before the
    // send and still has strong current-turn evidence at the final verdict.
    let finally_actively_busy = matches!(status, SubmitStatus::StillInBox)
        && PaneBaseline::capture(host, addr, profile).actively_busy;
    let (event_status, detail, delivery) = classify_delivery(
        status,
        baseline.actively_busy,
        finally_actively_busy,
    );
    shelbi_state::append_send_event(project, id, event_status, detail).map_err(|e| anyhow!(e))?;
    delivery.ok_or_else(|| {
        anyhow!(
            "message to `{id}` is stuck in {} after a retry Enter; the failure was recorded in events.log",
            addr.target()
        )
    })
}

/// Map the transport-neutral verifier result to `shelbi send` semantics.
/// A visibly parked message is acceptable only when the pane was genuinely
/// busy both before delivery and at the final verdict: Claude keeps submitted
/// mid-turn input visible as a queue and consumes it when the current turn
/// ends. The same screen after that turn has ended is the bug this command
/// must report as stuck.
fn classify_delivery(
    status: SubmitStatus,
    baseline_actively_busy: bool,
    finally_actively_busy: bool,
) -> (&'static str, &'static str, Option<SendDelivery>) {
    match status {
        SubmitStatus::Submitted { detail } => ("submitted", detail, Some(SendDelivery::Submitted)),
        SubmitStatus::DeliveredUnverified { detail } => {
            ("unverified", detail, Some(SendDelivery::Unverified))
        }
        SubmitStatus::StillInBox if baseline_actively_busy && finally_actively_busy => (
            "queued",
            "busy_pane_visible_queue",
            Some(SendDelivery::Queued),
        ),
        SubmitStatus::StillInBox => ("stuck", "still_in_input_after_retry", None),
        SubmitStatus::Unconfirmed => ("stuck", "unconfirmed_after_retry", None),
    }
}

/// Where the message should land. We resolve once up front so the
/// send + housekeeping arms each have a single code path.
#[derive(Debug)]
enum ResolvedTarget {
    /// Name matched a declared workspace; address derived from the
    /// project YAML + machine spec.
    Workspace {
        host: Host,
        addr: TmuxAddr,
        runner: AgentRunnerSpec,
    },
    /// Name only matched a legacy spawn-based agent file. Address read
    /// from the agent's frontmatter.
    LegacyAgent {
        host: Host,
        addr: TmuxAddr,
        runner: AgentRunnerSpec,
    },
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
        let runner = project.runner(&workspace.runner).ok_or_else(|| {
            anyhow!(
                "workspace `{id}` references runner `{}` which is not declared in agent_runners",
                workspace.runner
            )
        })?;
        return Ok(ResolvedTarget::Workspace {
            host: machine.host(),
            addr,
            runner: runner.clone(),
        });
    }

    // Fall through to the legacy `shelbi spawn` registry. `load_agent`
    // is what the old send did; we just swallow the not-found error and
    // turn it into a unified `unknown id` message that lists both
    // registries' members.
    match shelbi_state::load_agent(&project.name, id) {
        Ok(file) => {
            let machine = project
                .machine(&file.agent.machine)
                .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?;
            let runner = project.runner(&file.agent.runner).ok_or_else(|| {
                anyhow!(
                    "legacy agent `{id}` references runner `{}` which is no longer declared in agent_runners",
                    file.agent.runner
                )
            })?;
            Ok(ResolvedTarget::LegacyAgent {
                host: machine.host(),
                addr: file.agent.tmux.clone(),
                runner: runner.clone(),
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
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use shelbi_core::{
        Agent, AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind,
        OrchestratorSpec, WorkspaceSpec, ZenConfig,
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
            },
        );
        runners.insert(
            "codex".to_string(),
            AgentRunnerSpec {
                command: "/opt/homebrew/bin/codex".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
            },
        );
        Project {
            name: name.into(),
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
            ResolvedTarget::Workspace { host, addr, runner } => {
                assert!(host.is_local());
                assert_eq!(addr.session, "shelbi-demo");
                assert_eq!(addr.window, "alpha");
                assert_eq!(runner.command, "claude");
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
            ResolvedTarget::Workspace { host, addr, runner } => {
                assert!(matches!(host, Host::Ssh { ref host } if host == "devbox"));
                assert_eq!(addr.session, "shelbi-w-delta");
                assert_eq!(addr.window, "agent");
                assert_eq!(runner.command, "claude");
            }
            ResolvedTarget::LegacyAgent { .. } => {
                panic!("expected workspace resolution, got legacy")
            }
        }
    }

    #[test]
    fn workspace_resolution_retains_codex_runner_for_submit_gating() {
        let project = project_with_workspaces(
            "demo",
            vec![WorkspaceSpec {
                name: "bravo".into(),
                machine: "hub".into(),
                runner: "codex".into(),
                tags: Vec::new(),
                slot: None,
            }],
        );
        match resolve_target(&project, "bravo").unwrap() {
            ResolvedTarget::Workspace { runner, .. } => {
                assert_eq!(runner.command, "/opt/homebrew/bin/codex");
                assert!(!SubmitProfile::for_runner(&runner).has_ui_verifier());
            }
            ResolvedTarget::LegacyAgent { .. } => {
                panic!("expected workspace resolution, got legacy")
            }
        }
    }

    #[test]
    fn legacy_resolution_retains_runner_and_reports_removed_runner() {
        let _lock = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", tmp.path());

        let mut project = project_with_workspaces("demo", Vec::new());
        let now = Utc::now();
        let agent = Agent {
            id: "legacy-codex".into(),
            project: project.name.clone(),
            machine: "hub".into(),
            runner: "codex".into(),
            branch: "shelbi/legacy-codex".into(),
            worktree: tmp.path().join("legacy-codex"),
            status: Status::Running,
            created: now,
            updated: now,
            tmux: TmuxAddr {
                session: "shelbi-demo".into(),
                window: "legacy-codex".into(),
            },
        };
        shelbi_state::save_agent(&project.name, &agent, "# Task\n").unwrap();

        match resolve_target(&project, "legacy-codex").unwrap() {
            ResolvedTarget::LegacyAgent { runner, .. } => {
                assert_eq!(runner.command, "/opt/homebrew/bin/codex");
            }
            ResolvedTarget::Workspace { .. } => {
                panic!("expected legacy resolution, got workspace")
            }
        }

        project.agent_runners.remove("codex");
        let error = resolve_target(&project, "legacy-codex").unwrap_err();
        assert!(
            error.to_string().contains(
                "legacy agent `legacy-codex` references runner `codex` which is no longer declared"
            ),
            "error: {error}"
        );
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
        assert!(
            msg.contains("unknown workspace/agent `charlie`"),
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

    #[test]
    fn idle_visible_input_is_stuck_but_busy_visible_input_is_queued() {
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, false, false),
            ("stuck", "still_in_input_after_retry", None)
        );
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, true, true),
            (
                "queued",
                "busy_pane_visible_queue",
                Some(SendDelivery::Queued)
            )
        );
    }

    #[test]
    fn stale_busy_baseline_does_not_hide_a_wedged_idle_prompt() {
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, true, false),
            ("stuck", "still_in_input_after_retry", None)
        );
        assert_eq!(
            classify_delivery(SubmitStatus::StillInBox, false, true),
            ("stuck", "still_in_input_after_retry", None)
        );
    }

    #[test]
    fn confirmed_and_unconfirmed_verdicts_map_to_delivery_events() {
        assert_eq!(
            classify_delivery(
                SubmitStatus::Submitted {
                    detail: "retry_enter"
                },
                false,
                false,
            ),
            ("submitted", "retry_enter", Some(SendDelivery::Submitted))
        );
        assert_eq!(
            classify_delivery(SubmitStatus::Unconfirmed, true, true),
            ("stuck", "unconfirmed_after_retry", None)
        );
    }

    #[test]
    fn unsupported_runner_delivery_is_success_but_explicitly_unverified() {
        assert_eq!(
            classify_delivery(
                SubmitStatus::DeliveredUnverified {
                    detail: "verification_unsupported"
                },
                false,
                false,
            ),
            (
                "unverified",
                "verification_unsupported",
                Some(SendDelivery::Unverified)
            )
        );
        assert_eq!(SendDelivery::Unverified.to_string(), "sent (unverified)");
    }
}
