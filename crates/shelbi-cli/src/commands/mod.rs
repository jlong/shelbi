pub mod action;
pub mod agent;
pub mod archive;
pub mod attach;
pub mod config;
pub mod daemon;
pub mod diff;
pub mod events;
pub mod init;
pub mod list;
pub mod merge;
pub mod message;
pub mod open;
pub mod orchestrator;
pub mod orchestrate;
pub mod palette;
pub mod picker;
pub mod popup;
pub mod project;
pub mod quit_project;
pub mod quit_shelbi;
pub mod reload;
pub mod send;
pub mod spawn;
pub mod status;
pub mod tail;
pub mod task;
pub mod wizard;
pub mod workspace;
pub mod workflow;
pub mod zen;
pub mod zen_intro;
pub mod zen_lifecycle;

use std::path::Path;

use anyhow::{anyhow, Result};

/// Resolve the active project name. Precedence:
///
/// 1. The `--project` / `$SHELBI_PROJECT` value passed in.
/// 2. Reverse-lookup: scan `~/.shelbi/projects/*.yaml` and match the
///    current directory (or an ancestor) against each project's local
///    `work_dir`, deepest match wins. See
///    [`shelbi_state::resolve_project_for_cwd`].
///
/// Errors if nothing resolves.
pub fn require_project(explicit: Option<String>) -> Result<String> {
    if let Some(p) = explicit {
        return Ok(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(name) = shelbi_state::resolve_project_for_cwd(&cwd).map_err(|e| anyhow!(e))? {
            return Ok(name);
        }
    }
    Err(anyhow!(
        "no project specified — pass --project NAME, set SHELBI_PROJECT, or run from inside a \
         registered project's work_dir (see ~/.shelbi/projects/*.yaml)"
    ))
}

/// Open `path` in the user's editor, honoring the conventional
/// `$VISUAL` → `$EDITOR` → `vi` precedence and splitting an editor value
/// that carries arguments (`VISUAL="code --wait"`,
/// `EDITOR="emacsclient -t"`) into program + args before the file is
/// appended. The split is whitespace-based (matching the git/less
/// convention); no shell is spawned, so the file path is never re-parsed
/// for metacharacters. Shared by `task edit` and `agent edit` so the
/// argument-handling and precedence rules stay in one place (F14).
pub fn launch_editor(path: &Path) -> Result<()> {
    let (program, args) = resolve_editor_command();
    let status = std::process::Command::new(&program)
        .args(&args)
        .arg(path)
        .status()
        .map_err(|e| anyhow!("launching editor `{program}`: {e}"))?;
    if !status.success() {
        return Err(anyhow!("editor `{program}` exited with {status}"));
    }
    Ok(())
}

/// Run a `tmux` subcommand for its side effect, returning whether it exited
/// zero. Unlike a bare `.status()` call this captures stderr and, on failure
/// (non-zero exit OR a spawn error), surfaces it on our own stderr so a broken
/// tmux invocation is diagnosable instead of silently collapsing to `false`
/// (cli-session-ux F12). Shared across the CLI's non-TUI tmux call sites
/// (`open`, `palette`, `quit_project`, `quit_shelbi`) so the diagnostics and
/// stderr handling live in one place (F14). Not for use inside a live ratatui
/// screen — writing to stderr there would corrupt the alt-screen; those paths
/// surface failures through their own status line instead.
pub(crate) fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let args: Vec<std::ffi::OsString> =
        args.into_iter().map(|a| a.as_ref().to_os_string()).collect();
    let argv = || {
        args.iter()
            .map(|a| a.to_string_lossy())
            .collect::<Vec<_>>()
            .join(" ")
    };
    match std::process::Command::new("tmux").args(&args).output() {
        Ok(out) if out.status.success() => true,
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if stderr.is_empty() {
                eprintln!("warning: `tmux {}` exited {}", argv(), out.status);
            } else {
                eprintln!("warning: `tmux {}` failed: {stderr}", argv());
            }
            false
        }
        Err(e) => {
            eprintln!("warning: failed to run `tmux {}`: {e}", argv());
            false
        }
    }
}

/// Resolve the editor command as `(program, leading-args)`, honoring
/// `$VISUAL` before `$EDITOR` (the traditional Unix precedence) and
/// falling back to `vi`. A blank or whitespace-only value is skipped so
/// `EDITOR=` falls through to the next candidate. Split out from
/// [`launch_editor`] so the parsing can be unit-tested without spawning a
/// process.
pub fn resolve_editor_command() -> (String, Vec<String>) {
    for var in ["VISUAL", "EDITOR"] {
        if let Ok(val) = std::env::var(var) {
            let mut parts = val.split_whitespace();
            if let Some(program) = parts.next() {
                let args = parts.map(str::to_string).collect();
                return (program.to_string(), args);
            }
        }
    }
    ("vi".to_string(), Vec::new())
}

#[cfg(test)]
mod editor_tests {
    use super::resolve_editor_command;
    use crate::commands::test_support::ENV_LOCK;

    #[test]
    fn splits_args_and_honors_visual_before_editor() {
        let _g = ENV_LOCK.lock().unwrap();
        std::env::remove_var("EDITOR");
        std::env::remove_var("VISUAL");
        // Nothing set → POSIX `vi`, no args.
        assert_eq!(resolve_editor_command(), ("vi".to_string(), vec![]));

        // Multi-word EDITOR splits into program + args.
        std::env::set_var("EDITOR", "code --wait");
        assert_eq!(
            resolve_editor_command(),
            ("code".to_string(), vec!["--wait".to_string()]),
        );

        // VISUAL wins over EDITOR when both are set.
        std::env::set_var("VISUAL", "emacsclient -t");
        assert_eq!(
            resolve_editor_command(),
            ("emacsclient".to_string(), vec!["-t".to_string()]),
        );

        // Blank VISUAL falls through to EDITOR.
        std::env::set_var("VISUAL", "   ");
        assert_eq!(
            resolve_editor_command(),
            ("code".to_string(), vec!["--wait".to_string()]),
        );

        std::env::remove_var("EDITOR");
        std::env::remove_var("VISUAL");
    }
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
                dialog_signatures: vec![],
            },
        );
        let project = Project {
            name: project_name.into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.clone(),
                host: None,
                tags: Vec::new(),
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
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        repo
    }
}

// Project-resolution unit tests live in `shelbi_state::resolve` now that
// the walk-up logic moved into the state crate.
