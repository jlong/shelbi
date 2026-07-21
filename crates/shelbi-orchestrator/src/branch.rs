//! Task branch-name resolution.
//!
//! Explicit task `branch:` wins. Otherwise Shelbi prefers a full `git.branch`
//! template (rendered against `{{github_user}}`, `{{id}}`, and task params),
//! then falls back to composing `<branch_prefix>/<task-id>` from workflow
//! config, project config, or the authenticated GitHub username. The GitHub
//! lookup is cached per process so branch creation does not pay a `gh` call
//! every time.

use std::process::Command;
use std::sync::OnceLock;

use shelbi_core::{substitute_placeholders, validate_branch, Error, Project, Result, Task, Workflow};

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

    let task_params = task.string_params();
    let workflow_git = workflow
        .map(|wf| wf.resolve_git(&task_params))
        .transpose()?
        .flatten();

    // A workflow `git:` block that declares either branch-naming key wins
    // wholesale over the project block; otherwise fall through to project
    // config. Within each level `branch` (a full template) is preferred over
    // `branch_prefix` — the two are mutually exclusive per block, so at most
    // one is ever set.
    let workflow_branch = workflow_git.as_ref().and_then(|g| g.branch.clone());
    let workflow_prefix = workflow_git.as_ref().and_then(|g| g.branch_prefix.clone());
    let (branch_template, config_prefix, scope) = if workflow_branch.is_some() {
        (workflow_branch, None, workflow.map(|w| w.name.clone()))
    } else if workflow_prefix.is_some() {
        (None, workflow_prefix, workflow.map(|w| w.name.clone()))
    } else {
        (
            project.git.branch.clone(),
            project.git.branch_prefix.clone(),
            None,
        )
    };

    // Preferred path: a full `git.branch` template. Render it against the
    // task params plus `{{id}}` and `{{github_user}}`. `github_user` shares
    // `branch_prefix`'s fallback — the authenticated login, else the stable
    // `user` placeholder — so a `{{github_user}}/{{id}}` template degrades to
    // `user/<id>` exactly as the prefix path does when `gh` is unavailable.
    if let Some(template) = branch_template {
        let github_user = login().unwrap_or_else(|| {
            tracing::warn!(
                task = %task.id,
                fallback_prefix = FALLBACK_BRANCH_PREFIX,
                "could not determine GitHub username for `git.branch` template; \
                 falling back to stable value"
            );
            FALLBACK_BRANCH_PREFIX.to_string()
        });
        let mut params = task_params;
        params.insert("github_user".to_string(), github_user);
        params.insert("id".to_string(), task.id.clone());
        let mut missing: Vec<String> = Vec::new();
        let branch = substitute_placeholders(&template, &params, &mut missing);
        if !missing.is_empty() {
            return Err(Error::MissingTaskParams {
                workflow: scope.unwrap_or_else(|| project.name.clone()),
                params: missing,
            });
        }
        validate_branch(&branch).map_err(|e| Error::Other(format!("task `{}`: {e}", task.id)))?;
        return Ok(branch);
    }

    // Fallback path: `<branch_prefix>/<task-id>`.
    let prefix = config_prefix
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
            label: None,
            display_name: None,
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
            review: None,
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

    /// A project whose `git:` block carries a full `branch` template instead
    /// of a `branch_prefix`.
    fn project_with_branch(template: &str) -> Project {
        let mut p = project(None);
        p.git.branch = Some(template.to_string());
        p
    }

    /// A workflow whose `git:` block carries a full `branch` template.
    fn workflow_with_branch(template: &str) -> Workflow {
        let mut wf = workflow(None);
        wf.git = Some(GitConfig {
            branch: Some(template.to_string()),
            merge_strategy: MergeStrategy::Squash,
            ..Default::default()
        });
        wf
    }

    #[test]
    fn branch_template_renders_github_user_and_id() {
        let p = project(None);
        let wf = workflow_with_branch("{{github_user}}/{{id}}");
        let t = task("fix-login", None, Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "jlong/fix-login"
        );
    }

    #[test]
    fn branch_template_falls_back_to_stable_user_when_login_fails() {
        // github_user degrades to the same `user` placeholder the prefix path
        // uses, so the shipped `{{github_user}}/{{id}}` default still yields a
        // valid `user/<id>` branch without `gh`.
        let p = project(None);
        let wf = workflow_with_branch("{{github_user}}/{{id}}");
        let t = task("fix-login", None, Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || None).unwrap(),
            "user/fix-login"
        );
    }

    #[test]
    fn shipped_subtask_workflow_renders_subtask_id_without_github_user() {
        // The shipped subtask workflow uses `branch: 'subtask/{{id}}'`. It must
        // render to `subtask/<id>` with NO github_user prefix even when a login
        // is available — the old `branch_prefix: subtask` path never prepended
        // the login, and the migration to a full template preserves that output.
        let p = project(None);
        let wf = shelbi_core::subtask_workflow();
        let mut t = task("add-csv-export", None, Some("subtask"));
        // The subtask's `task:` frontmatter names its parent, which the
        // templated `base_branch: task/{{task}}` needs to resolve.
        t.params
            .insert("task".into(), serde_yaml::Value::String("add-auth".into()));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "subtask/add-csv-export"
        );
    }

    #[test]
    fn workflow_branch_template_wins_over_project_branch_prefix() {
        let p = project(Some("project"));
        let wf = workflow_with_branch("{{github_user}}/{{id}}");
        let t = task("fix-login", None, Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "jlong/fix-login"
        );
    }

    #[test]
    fn project_branch_template_is_used_without_workflow_override() {
        let p = project_with_branch("{{github_user}}/{{id}}");
        let wf = workflow(None);
        let t = task("fix-login", None, Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "jlong/fix-login"
        );
    }

    #[test]
    fn explicit_task_branch_overrides_branch_template() {
        let p = project(None);
        let wf = workflow_with_branch("{{github_user}}/{{id}}");
        let t = task("fix-login", Some("release/hotfix"), Some("app"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "release/hotfix"
        );
    }

    #[test]
    fn branch_template_resolves_task_params() {
        let p = project(None);
        let wf = workflow_with_branch("{{github_user}}/{{feature}}/{{id}}");
        let mut t = task("fix-login", None, Some("app"));
        t.params
            .insert("feature".into(), serde_yaml::Value::from("auth"));
        assert_eq!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into())).unwrap(),
            "jlong/auth/fix-login"
        );
    }

    #[test]
    fn branch_template_missing_param_errors() {
        let p = project(None);
        let wf = workflow_with_branch("{{github_user}}/{{feature}}/{{id}}");
        let t = task("fix-login", None, Some("app"));
        let err = branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("jlong".into()))
            .expect_err("unresolved {{feature}} should error");
        assert!(
            matches!(&err, Error::MissingTaskParams { params, .. } if params == &["feature"]),
            "got: {err:?}"
        );
    }

    #[test]
    fn branch_template_rejects_invalid_rendered_branch() {
        // A template that renders to a shell-metacharacter branch is caught by
        // `validate_branch` after substitution.
        let p = project(None);
        let wf = workflow_with_branch("{{github_user}}/{{id}}");
        let t = task("fix-login", None, Some("app"));
        assert!(
            branch_name_for_task_with_login(&p, Some(&wf), &t, || Some("bad user".into())).is_err()
        );
    }
}
