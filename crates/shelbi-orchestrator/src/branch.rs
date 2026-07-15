//! Task branch-name resolution.
//!
//! Explicit task `branch:` wins. Otherwise Shelbi composes
//! `<prefix>/<task-id>` from workflow config, project config, or the
//! authenticated GitHub username. The GitHub lookup is cached per process so
//! branch creation does not pay a `gh` call every time.

use std::process::Command;
use std::sync::OnceLock;

use shelbi_core::{validate_branch, Error, Project, Result, Task, Workflow};

const FALLBACK_BRANCH_PREFIX: &str = "user";

static GITHUB_LOGIN: OnceLock<Option<String>> = OnceLock::new();

pub fn branch_name_for_task(
    project: &Project,
    workflow: Option<&Workflow>,
    task: &Task,
) -> Result<String> {
    branch_name_for_task_with_login(project, workflow, task, github_login)
}

fn branch_name_for_task_with_login<F>(
    project: &Project,
    workflow: Option<&Workflow>,
    task: &Task,
    login: F,
) -> Result<String>
where
    F: FnOnce() -> Option<String>,
{
    if let Some(branch) = &task.branch {
        validate_branch(branch).map_err(|e| Error::Other(format!("task `{}`: {e}", task.id)))?;
        return Ok(branch.clone());
    }

    let workflow_prefix = workflow
        .map(|wf| wf.resolve_git(&task.string_params()))
        .transpose()?
        .flatten()
        .and_then(|git| git.branch_prefix);
    let prefix = workflow_prefix
        .or_else(|| project.git.branch_prefix.clone())
        .or_else(login)
        .unwrap_or_else(|| {
            tracing::warn!(
                task = %task.id,
                fallback_prefix = FALLBACK_BRANCH_PREFIX,
                "could not determine GitHub username for generated task branch; \
                 falling back to stable prefix"
            );
            FALLBACK_BRANCH_PREFIX.to_string()
        });
    let branch = format!("{prefix}/{}", task.id);
    validate_branch(&branch).map_err(|e| Error::Other(format!("task `{}`: {e}", task.id)))?;
    Ok(branch)
}

fn github_login() -> Option<String> {
    GITHUB_LOGIN
        .get_or_init(|| {
            let out = Command::new("gh")
                .args(["api", "user", "--jq", ".login"])
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let login = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if login.is_empty() {
                None
            } else {
                Some(login)
            }
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use shelbi_core::{
        AgentRunnerSpec, Column, GitConfig, HeartbeatConfig, Machine, MachineKind, MergeStrategy,
        OrchestratorSpec, WorkspaceSpec, ZenConfig,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn project(prefix: Option<&str>) -> Project {
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
            name: "p".into(),
            repo: "/tmp/p".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: PathBuf::from("/tmp/p"),
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
            workspaces: vec![WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig {
                branch_prefix: prefix.map(str::to_string),
                ..Default::default()
            },
        }
    }

    fn task(id: &str, branch: Option<&str>, workflow: Option<&str>) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: Column::todo(),
            priority: 0,
            assigned_to: None,
            workflow: workflow.map(str::to_string),
            branch: branch.map(str::to_string),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        }
    }

    fn workflow(prefix: Option<&str>) -> Workflow {
        let git = prefix.map(|p| GitConfig {
            branch_prefix: Some(p.to_string()),
            merge_strategy: MergeStrategy::Squash,
            ..Default::default()
        });
        Workflow {
            name: "app".into(),
            description: None,
            statuses: vec![shelbi_core::WorkflowStatus {
                id: "todo".into(),
                name: "Todo".into(),
                category: shelbi_core::StatusCategory::Ready,
                owner: shelbi_core::Owner::Agent,
                agent: Some("developer".into()),
                tags: Vec::new(),
            }],
            initial_status: None,
            transitions: None,
            git,
            zen: None,
        }
    }

    #[test]
    fn explicit_task_branch_wins() {
        let p = project(Some("project"));
        let wf = workflow(Some("workflow"));
        let t = task("fix-login", Some("release/hotfix"), Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "release/hotfix"
        );
    }

    #[test]
    fn workflow_prefix_wins_over_project_prefix() {
        let p = project(Some("project"));
        let wf = workflow(Some("app"));
        let t = task("fix-login", None, Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "app/fix-login"
        );
    }

    #[test]
    fn project_prefix_is_used_without_workflow_override() {
        let p = project(Some("project"));
        let wf = workflow(None);
        let t = task("fix-login", None, Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "project/fix-login"
        );
    }

    #[test]
    fn github_username_is_default_prefix() {
        let p = project(None);
        let t = task("fix-login", None, None);
        assert_eq!(
            branch_name_for_task_with_login(&p, None, &t, || Some("jlong".into())).unwrap(),
            "jlong/fix-login"
        );
    }

    #[test]
    fn stable_fallback_is_used_when_username_discovery_fails() {
        let p = project(None);
        let t = task("fix-login", None, None);
        assert_eq!(
            branch_name_for_task_with_login(&p, None, &t, || None).unwrap(),
            "user/fix-login"
        );
    }

    #[test]
    fn invalid_generated_branch_is_rejected() {
        let p = project(Some("bad//prefix"));
        let t = task("fix-login", None, None);
        assert!(branch_name_for_task_with_login(&p, None, &t, || None).is_err());
    }
}
