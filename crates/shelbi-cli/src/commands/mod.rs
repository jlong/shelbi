pub mod action;
pub mod archive;
pub mod attach;
pub mod config;
pub mod diff;
pub mod events;
pub mod init;
pub mod list;
pub mod merge;
pub mod orchestrate;
pub mod palette;
pub mod picker;
pub mod popup;
pub mod project;
pub mod quit_project;
pub mod quit_shelbi;
pub mod reload;
pub mod review;
pub mod send;
pub mod spawn;
pub mod status;
pub mod tail;
pub mod task;
pub mod wizard;
pub mod workspace;
pub mod workflow;
pub mod zen;
pub mod zen_lifecycle;

use anyhow::{anyhow, Result};
use std::path::Path;
#[cfg(test)]
use std::path::PathBuf;

/// Resolve the active project name. Precedence:
///
/// 1. The `--project` / `$SHELBI_PROJECT` value passed in.
/// 2. The contents of the nearest `.shelbi/project` marker file walking up
///    from the current directory.
///
/// Errors if nothing resolves.
pub fn require_project(explicit: Option<String>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = discover_project_marker(&cwd)? {
            return Ok(name);
        }
    }
    Err(anyhow!(
        "no project specified — pass --project NAME, set SHELBI_PROJECT, or write the project \
         name into a `.shelbi/project` file at the top of your repo"
    ))
}

/// Walk up from `start`, looking for `.shelbi/project`. Returns the trimmed
/// contents of the first one found, or `None` if no marker exists.
fn discover_project_marker(start: &Path) -> Result<Option<String>> {
    let mut cur: Option<&Path> = Some(start);
    while let Some(dir) = cur {
        let marker = dir.join(".shelbi").join("project");
        if marker.is_file() {
            let name = std::fs::read_to_string(&marker)?.trim().to_string();
            if name.is_empty() {
                return Err(anyhow!(
                    "`.shelbi/project` at {} is empty",
                    marker.display()
                ));
            }
            return Ok(Some(name));
        }
        cur = dir.parent();
    }
    Ok(None)
}

/// Resolve the working session (workspace) name. Precedence: explicit > env >
/// "default".
pub fn _resolve_session(explicit: Option<String>) -> String {
    explicit
        .or_else(|| std::env::var("SHELBI_SESSION").ok())
        .unwrap_or_else(|| "default".to_string())
}

#[cfg(test)]
pub(crate) fn _marker_for_test(start: &Path) -> Result<Option<String>> {
    discover_project_marker(start)
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    /// Shared mutex for any test in this binary that mutates `SHELBI_HOME`.
    /// Tests across the `task` and `workspace` modules race on this env var,
    /// so they must all lock the *same* static — per-module locks would
    /// silently interleave and produce flaky failures.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Provision a real git repo + project YAML at `<home>/projects/<name>.yaml`
    /// pointing the hub machine at the repo. Used by tests that exercise CLI
    /// paths now gated on `shelbi_orchestrator::lifecycle` running a
    /// hub-side `git branch` — the lifecycle hook needs both a loadable
    /// project YAML and a real git repo at the hub workdir to succeed.
    ///
    /// Caller must hold `ENV_LOCK` and have `SHELBI_HOME` pointing at
    /// `home`. Initializes a single commit on `main` so cuts off `main`
    /// have something to resolve against. Returns the repo path so the
    /// test can drive further git operations against it.
    pub fn provision_hub_repo_for_project(home: &Path, project_name: &str) -> PathBuf {
        use shelbi_core::{
            AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind,
            OrchestratorSpec, Project, ZenConfig,
        };
        use std::collections::BTreeMap;
        use std::process::Command;

        let repo = home.join(format!("{project_name}-repo"));
        std::fs::create_dir_all(&repo).unwrap();

        let run = |args: &[&str]| {
            let ok = Command::new("git")
                .current_dir(&repo)
                .args(args)
                .status()
                .unwrap()
                .success();
            assert!(ok, "git {args:?} failed in {}", repo.display());
        };
        run(&["init", "-q", "-b", "main", "."]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run(&["add", "README.md"]);
        run(&["commit", "-q", "-m", "init"]);

        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        let project = Project {
            name: project_name.into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.clone(),
                host: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        repo
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marker_walks_up() {
        let tmp = tempfile_dir();
        let sub = tmp.join("a/b/c");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(tmp.join(".shelbi")).unwrap();
        std::fs::write(tmp.join(".shelbi/project"), "myapp\n").unwrap();

        let found = _marker_for_test(&sub).unwrap();
        assert_eq!(found.as_deref(), Some("myapp"));
    }

    #[test]
    fn marker_absent_returns_none() {
        let tmp = tempfile_dir();
        let found = _marker_for_test(&tmp).unwrap();
        assert!(found.is_none());
    }

    fn tempfile_dir() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-test-{}-{}",
            std::process::id(),
            // poor-man's unique suffix
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
