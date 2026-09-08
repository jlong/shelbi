pub mod action;
pub mod add_project;
pub mod agent;
pub mod archive;
pub mod attach;
pub mod config;
mod config_surfaces;
mod config_upgrade;
mod config_upgrade_apply;
pub mod daemon;
pub mod diff;
pub mod events;
pub mod guard;
pub mod hub_version;
pub mod init;
pub mod issue;
pub mod issue_store;
pub mod list;
pub mod merge;
pub mod message;
pub mod open;
pub mod orchestrate;
pub mod orchestrator;
pub mod palette;
pub mod popup;
pub mod project;
pub mod quit;
pub mod quit_project;
pub mod quit_shelbi;
pub mod reload;
pub mod review_confirm;
pub mod review_reject;
pub mod send;
pub mod spawn;
pub mod status;
pub mod tail;
pub mod teardown;
pub mod wizard;
pub mod workflow;
pub mod workspace;
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

/// Read a project's **open** board for a CLI listing through the daemon-owned
/// `board-index.json` (the consumer half of
/// `Plans/github-issue-caching-and-rate-limits.md` §5), never the backend.
///
/// On a hub the daemon is the single board reader, so `shelbi issue list`
/// renders from the published file and issues no `gh api` list of its own. When
/// the index is lagging — a stopped or wedged daemon, so its `fetched_at` has
/// aged past the refresh cadence — a one-line note is printed to **stderr** so
/// the listing on stdout stays clean and pipeable while the operator still
/// learns the data may be old. A `file_system` project has no daemon and reads
/// its local board straight from disk (always warm, no note).
///
/// When the index is genuinely **cold** (no daemon has ever published one — a
/// bare CLI on a machine with no hub), this falls back to a direct backend read
/// so a script still lists the board, exactly as before the daemon owned it
/// (§5's "behaviour is unchanged for scripts"). That is the one path here that
/// may touch the backend, and only when there is no hub to read from.
///
/// Returns the issues to display.
pub(crate) fn read_open_board_for_cli(project: &str) -> Result<Vec<shelbi_state::IssueFile>> {
    use shelbi_state::BoardState;
    match shelbi_state::read_board(project).map_err(|e| anyhow!(e))? {
        BoardState::Warm(board) => Ok(board),
        BoardState::Stale(board) => {
            eprintln!(
                "note: the board index is stale (the hub daemon may be stopped) — \
                 showing the last published board"
            );
            Ok(board)
        }
        // No hub has published an index. Fall back to a direct backend read so a
        // bare CLI still lists the board (the daemon, once running, takes over).
        BoardState::Cold => shelbi_state::issue_store_for(project)
            .and_then(|s| s.list_open())
            .map_err(|e| anyhow!(e)),
    }
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
/// (Shelbi ContextStore docs/planning:reviews/adversarial-2026-07/cli-session-ux.md
/// F12). Shared across the CLI's non-TUI tmux call sites
/// (`open`, `palette`, `quit_project`, `quit_shelbi`) so the diagnostics and
/// stderr handling live in one place (F14). Not for use inside a live ratatui
/// screen — writing to stderr there would corrupt the alt-screen; those paths
/// surface failures through their own status line instead.
pub(crate) fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let args: Vec<std::ffi::OsString> = args
        .into_iter()
        .map(|a| a.as_ref().to_os_string())
        .collect();
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
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
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
mod board_cli_tests {
    use crate::commands::test_support::ENV_LOCK;

    fn fresh_home() -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "shelbi-cli-board-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }

    fn register_github_project(home: &std::path::Path, name: &str) {
        let projects = home.join("projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join(format!("{name}.yaml")),
            format!(
                "name: {name}\nrepo: /tmp/{name}\ndefault_branch: main\n\
orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{name}\nworkspaces: []\n\
issue_tracker:\n  backend: github\n  github:\n    repo: owner/repo\n"
            ),
        )
        .unwrap();
    }

    fn ifile(id: &str, column: &str) -> shelbi_state::IssueFile {
        let task: shelbi_core::Issue = serde_yaml::from_str(&format!(
            "id: {id}\ntitle: {id}\ncolumn: {column}\npriority: 0\n\
             created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n"
        ))
        .unwrap();
        shelbi_state::IssueFile {
            task,
            body: String::new(),
        }
    }

    #[test]
    fn read_open_board_for_cli_serves_the_index_without_a_backend_list() {
        // `shelbi issue list` (and `status`/`workspace list`) read the board
        // from the daemon-owned `board-index.json` (§5). A `github` project with
        // a seeded index and no `gh` reachable still lists — the sentinel id
        // exists on no backend, so returning it proves the file was the source.
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);
        register_github_project(&home, "g");
        shelbi_state::write_board_index(
            "g",
            &shelbi_state::BoardIndex::fresh(vec![ifile("sentinel-only-in-index", "todo")]),
        )
        .unwrap();

        let board = super::read_open_board_for_cli("g").unwrap();
        assert_eq!(board.len(), 1);
        assert_eq!(board[0].task.id, "sentinel-only-in-index");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
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
    ///
    /// Acquire it with `.lock().unwrap_or_else(|p| p.into_inner())`, never a
    /// bare `.unwrap()`: a single panicking test (e.g. a timing-sensitive
    /// assertion) would otherwise poison this static and cascade into a
    /// PoisonError failure in every later test that touches it — turning one
    /// red test into dozens. Recovering the guard from the poison keeps the
    /// seed failure isolated to the one test that actually failed.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Restores process environment variables on drop.
    ///
    /// Caller must hold `ENV_LOCK`; this guard only makes cleanup
    /// panic-safe for tests that intentionally set or clear env vars.
    pub struct EnvGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvGuard {
        pub fn new(keys: &[&'static str]) -> Self {
            Self {
                saved: keys
                    .iter()
                    .copied()
                    .map(|key| (key, std::env::var_os(key)))
                    .collect(),
            }
        }

        pub fn set<K, V>(&self, key: K, value: V)
        where
            K: AsRef<std::ffi::OsStr>,
            V: AsRef<std::ffi::OsStr>,
        {
            std::env::set_var(key, value);
        }

        pub fn remove<K>(&self, key: K)
        where
            K: AsRef<std::ffi::OsStr>,
        {
            std::env::remove_var(key);
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (key, value) in self.saved.iter().rev() {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }

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
            AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
            Project, ZenConfig,
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
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        let project = Project {
            name: project_name.into(),
            label: None,
            display_name: None,
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            // Mirror a fresh `shelbi init`: default to the shipped `task`
            // workflow, which `scaffold_project_workflow` materializes below as
            // `workflows/task.yaml`. Leaving this unset would resolve to the
            // built-in `default` name, whose YAML the scaffold no longer writes.
            default_workflow: Some(shelbi_core::TASK_WORKFLOW_NAME.into()),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.clone(),
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
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            github_reconcile_interval_secs: 900,
            workspace_permissions_mode: Some("auto".into()),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            issue_tracker: Default::default(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        };
        shelbi_state::save_project(&project).unwrap();
        shelbi_state::scaffold_project_statuses(project_name).unwrap();
        shelbi_state::scaffold_project_workflow(project_name).unwrap();
        repo
    }
}

// Project-resolution unit tests live in `shelbi_state::resolve` now that
// the walk-up logic moved into the state crate.
