//! Shared git/gh helpers for the per-workflow action primitives and the
//! Zen Mode merge primitives. Kept here so `zen.rs` and `actions.rs` don't
//! drift on the basics — running a shell command in a worktree, finding
//! the right host for an operation, looking up an open PR, composing a PR
//! body, parsing the PR number out of `gh pr create`'s URL.

use std::path::PathBuf;
use std::process::Output;

use shelbi_core::{Error, Host, MachineKind, Project, Result, Task};

use crate::workspace::workspace_worktree;

/// Run `argv` with cwd = `dir` on `host`, picking up the user's login
/// `PATH` on both local and remote hosts so `gh` / `git` (and anything
/// else a per-workflow primitive may reach for) resolve the same way they
/// do in the user's own terminal.
///
/// We can't trust the orchestrator's inherited environment on local
/// either: when shelbi is launched outside an interactive terminal (from
/// launchd, Spotlight, a cron schedule, or a tmux server that itself
/// started in a non-login context), the inherited `PATH` is the
/// `/usr/bin:/bin` skeleton and is missing every tool installed via a
/// version manager or under `/opt/homebrew/bin`. The login shell sources
/// the user's rc files and rebuilds `PATH` the same way it does in
/// their terminal — see [`login_shell_prefix`] for the exact contract.
pub(crate) fn run_in_dir(host: &Host, dir: &str, argv: &[&str]) -> Result<Output> {
    let escaped: Vec<String> = argv.iter().map(|a| shelbi_agent::shell_escape(a)).collect();
    let line = format!(
        "cd {} && {}",
        shelbi_agent::shell_escape(dir),
        escaped.join(" ")
    );
    run_login_shell_script(host, &line).map_err(Error::Io)
}

/// Hand `script` to a login shell on `host`. Local just spawns
/// `$SHELL -lc <script>` through `std::process`, which passes `script`
/// as a single argv element — so the shell sees exactly the string we
/// built. SSH is the trap: `shelbi_ssh::run` joins our argv with literal
/// spaces and the remote default shell re-parses the result, so a script
/// like `cd /worktree && cargo build` arrives at the remote as
/// `$SHELL -lc cd /worktree && cargo build`. The remote shell then runs
/// `$SHELL -lc cd /worktree` (the `cd` becomes the inner shell's
/// command with `/worktree` as `$0` — a no-op) and `&& cargo build`
/// falls out into the *outer* remote shell, executing from its cwd of
/// `$HOME`. Single-quoting the script collapses it back to one token,
/// which the remote shell unquotes before handing it to `$SHELL -lc`.
/// `$SHELL` itself stays unquoted so the remote shell expands it to
/// whichever shell that account uses.
pub(crate) fn run_login_shell_script(host: &Host, script: &str) -> std::io::Result<Output> {
    let (shell, flag) = login_shell_prefix(host);
    let arg = match host {
        Host::Local => script.to_string(),
        Host::Ssh { .. } => shelbi_agent::shell_escape(script),
    };
    shelbi_ssh::run(host, [shell.as_str(), flag, arg.as_str()])
}

/// `(shell, "-lc")` — the pair shelbi prepends to a shell script so it
/// runs through a login shell on `host`. Used by every primitive that
/// shells out to environment-sensitive tools (zen probe's local checks,
/// pr-create / ci-watch / pr-merge, the per-workflow actions).
///
/// Local: read `$SHELL` from the orchestrator's env, defaulting to
/// `/bin/sh` for the no-`$SHELL` edge case (launchd, container, etc.).
/// Reading `$SHELL` matters because zsh users want `.zprofile` /
/// `.zshenv` sourced rather than bash's startup files — picking the
/// wrong shell silently runs the wrong rc files and misses whatever
/// `PATH` mutations the user wired up there.
///
/// Remote: pass the literal string `"$SHELL"` so the user's login shell
/// — which `sshd` hands the command to — expands it to whichever shell
/// that account actually uses. Avoids us guessing which shells are
/// installed on the remote.
pub(crate) fn login_shell_prefix(host: &Host) -> (String, &'static str) {
    let shell = match host {
        Host::Local => std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into()),
        Host::Ssh { .. } => "$SHELL".to_string(),
    };
    (shell, "-lc")
}

/// Find the workspace assigned to `task`, then return its host + worktree.
/// Errors if the task is unassigned or the workspace/machine resolution
/// fails — those are caller bugs, not policy decisions.
pub(crate) fn locate_workspace_worktree(project: &Project, task: &Task) -> Result<(Host, PathBuf)> {
    let workspace_name = task.assigned_to.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no assigned workspace — assign one before running this action",
            task.id
        ))
    })?;
    let workspace = project.workspace(workspace_name).ok_or_else(|| {
        Error::Other(format!(
            "task `{}` references unknown workspace `{workspace_name}`",
            task.id
        ))
    })?;
    let machine = project
        .machine(&workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(workspace.machine.clone()))?;
    Ok((machine.host(), workspace_worktree(machine, workspace)))
}

/// The first local machine in the project — by convention the hub. The
/// hub's `work_dir` is a clean checkout of the project repo, so gh / git
/// commands routed through it have a remote to talk to without needing
/// a workspace's worktree to exist yet.
pub(crate) fn locate_hub_workdir(project: &Project) -> Result<(Host, PathBuf)> {
    let machine = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .ok_or_else(|| {
            Error::Other(
                "project has no local machine to run gh on — hub-side actions require one".into(),
            )
        })?;
    Ok((machine.host(), machine.work_dir.clone()))
}

/// Look up an *open* PR for `branch` and return its number, if any. Uses
/// `gh pr list --head <branch> --state open`; a closed/merged PR for the
/// same branch is intentionally ignored — a fresh push warrants a fresh
/// PR.
pub(crate) fn lookup_open_pr(host: &Host, wt: &str, branch: &str) -> Result<Option<u64>> {
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number // empty",
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr list --head {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u64>().map(Some).map_err(|_| {
        Error::Other(format!(
            "gh pr list returned non-numeric value `{trimmed}` for branch `{branch}`"
        ))
    })
}

/// `git log -1 --format=%s` in `wt` — used as the default PR title when
/// opening a fresh PR. Falls back to a generic title only if the caller
/// preprocesses the empty case; we prefer surfacing a hard error here so
/// a broken worktree doesn't silently produce a blank-title PR.
pub(crate) fn head_commit_subject(host: &Host, wt: &str) -> Result<String> {
    let out = run_in_dir(host, wt, &["git", "log", "-1", "--format=%s"])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} log -1 --format=%s"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Lay out the PR body: the task summary (or an empty body when the task
/// has no body) followed by an auto-opened footer that points the reviewer
/// back at the task file on disk.
pub(crate) fn compose_pr_body(task_body: &str, task_path: &str) -> String {
    let trimmed = task_body.trim();
    let summary = if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n\n")
    };
    format!("{summary}---\n\nAuto-opened by Shelbi — review at: {task_path}\n")
}

/// `gh pr create` prints the new PR's URL like
/// `https://github.com/owner/repo/pull/42`. Pull the trailing `42`.
pub(crate) fn parse_pr_number_from_url(s: &str) -> Option<u64> {
    let last = s
        .rsplit_terminator(|c: char| c == '/' || c.is_whitespace())
        .next()?;
    last.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_body_includes_summary_and_footer() {
        let body = compose_pr_body("Add foo to bar.", "/tmp/p/tasks/add-foo.md");
        assert!(body.starts_with("Add foo to bar.\n\n---\n"));
        assert!(body.contains("Auto-opened by Shelbi"));
        assert!(body.contains("/tmp/p/tasks/add-foo.md"));
    }

    #[test]
    fn pr_body_handles_empty_task_body() {
        let body = compose_pr_body("", "/tmp/t.md");
        assert!(body.starts_with("---\n"));
        assert!(body.contains("Auto-opened by Shelbi"));
    }

    #[test]
    fn parses_pr_number_from_url() {
        assert_eq!(
            parse_pr_number_from_url("https://github.com/jlong/shelbi/pull/42"),
            Some(42)
        );
        assert_eq!(
            parse_pr_number_from_url("https://github.com/jlong/shelbi/pull/42\n"),
            Some(42)
        );
    }

    #[test]
    fn parse_pr_number_rejects_garbage() {
        assert_eq!(parse_pr_number_from_url(""), None);
        assert_eq!(parse_pr_number_from_url("not a url"), None);
    }

    #[test]
    fn login_shell_prefix_local_uses_shell_env() {
        let _guard = crate::test_lock::acquire();
        let prev = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/bin/sh");
        let (shell, flag) = login_shell_prefix(&Host::Local);
        match prev {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        assert_eq!(shell, "/bin/sh");
        assert_eq!(flag, "-lc");
    }

    #[test]
    fn login_shell_prefix_local_defaults_when_shell_unset() {
        let _guard = crate::test_lock::acquire();
        let prev = std::env::var_os("SHELL");
        std::env::remove_var("SHELL");
        let (shell, _) = login_shell_prefix(&Host::Local);
        match prev {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        assert_eq!(shell, "/bin/sh");
    }

    #[test]
    fn run_in_dir_runs_in_login_shell_that_sources_rc() {
        let _guard = crate::test_lock::acquire();
        // `pr_create` / `ci_watch` / `pr_merge` all funnel through
        // `run_in_dir` — they have to launch under a login shell so the
        // user's `PATH` (gh/git installed via homebrew, asdf, mise, nvm,
        // etc.) is rebuilt from their rc files before the command runs.
        //
        // Override `$HOME` to a tempdir, drop a `.profile` that exports
        // a marker, and assert the marker came through — proving the
        // outer login shell sourced rc files before executing the
        // wrapped argv. (Bash, dash, and bash-as-sh in login mode all
        // source `~/.profile`; zsh sources `~/.zprofile` / `~/.zshenv`
        // — pinning `$SHELL` to `/bin/sh` keeps this hermetic.)
        let tmp = tempfile::tempdir().unwrap();
        let profile = tmp.path().join(".profile");
        std::fs::write(&profile, "export SHELBI_LOGIN_MARK=ran-in-login\n").unwrap();

        let prev_shell = std::env::var_os("SHELL");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("SHELL", "/bin/sh");
        std::env::set_var("HOME", tmp.path());

        let out = run_in_dir(
            &Host::Local,
            tmp.path().to_str().unwrap(),
            &["sh", "-c", "echo $SHELBI_LOGIN_MARK"],
        );

        match prev_shell {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        let out = out.expect("run_in_dir failed");
        assert!(out.status.success(), "exited {:?}", out.status);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("ran-in-login"),
            "expected outer login shell to source ~/.profile, got: {stdout}"
        );
    }

    /// Regression test for the `shelbi zen probe` devbox bug: when a
    /// workspace is on a remote host, the script passed to the login
    /// shell has to survive OpenSSH's "join argv with literal spaces"
    /// behavior as a single token, or the `&&` in `cd <wt> && <cmd>`
    /// leaks out to the *outer* remote shell and the command runs from
    /// `$HOME`. We can't exercise a real ssh round-trip in unit tests, so
    /// we inspect the argv that `shelbi_ssh::build_command` would hand to
    /// the local `ssh` binary and assert the script is single-quoted.
    #[test]
    fn ssh_login_shell_script_is_single_quoted_on_the_wire() {
        let host = Host::Ssh {
            host: "devbox".into(),
        };
        let (shell, flag) = login_shell_prefix(&host);
        // Same shape `run_one_check` builds: `cd <wt> && <cmd>`.
        let script = "cd '/home/jlong/Workspaces/shelbi/.shelbi/wt/foxtrot' && cargo build --workspace";
        let arg = match &host {
            Host::Local => script.to_string(),
            Host::Ssh { .. } => shelbi_agent::shell_escape(script),
        };
        let cmd = shelbi_ssh::build_command(&host, [shell.as_str(), flag, arg.as_str()]);
        let argv: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        // The script must appear as a single quoted argv element so the
        // remote shell hands it whole to `$SHELL -lc`. If this regresses
        // to the raw script, `&&` would re-anchor to the remote shell
        // and the cd would silently no-op (the original bug).
        let script_arg = argv
            .iter()
            .find(|a| a.contains("cargo build --workspace"))
            .expect("script arg missing from ssh argv");
        assert!(
            script_arg.starts_with('\'') && script_arg.ends_with('\''),
            "expected script wrapped in single quotes for SSH, got: {script_arg}"
        );
        // `$SHELL` itself must stay unquoted so the remote shell expands
        // it to the user's actual login shell.
        assert!(
            argv.iter().any(|a| a == "$SHELL"),
            "expected unquoted $SHELL token in ssh argv, got: {argv:?}"
        );
    }

    #[test]
    fn local_login_shell_script_is_passed_verbatim() {
        // The local path goes through `std::process::Command`, which hands
        // each argv element to exec without a shell in between, so the
        // script must NOT be wrapped in single quotes — bash -lc would
        // then try to run a command literally named `'cd ... && cargo
        // build'`.
        let script = "cd '/tmp' && echo hi";
        let arg = match Host::Local {
            Host::Local => script.to_string(),
            Host::Ssh { .. } => shelbi_agent::shell_escape(script),
        };
        assert_eq!(arg, script);
    }
}
