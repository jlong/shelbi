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

/// Hand `script` to a login shell on `host`.
///
/// Local just spawns `$SHELL -lc <script>` through `std::process`, which
/// passes `script` as a single argv element — so the shell sees exactly
/// the string we built.
///
/// SSH is the historical trap. `shelbi_ssh::build_command` now shell-
/// escapes every argv element at the wire (F2), so we can no longer smuggle
/// a bare `$SHELL` token across for the remote account shell to expand — it
/// would arrive single-quoted and the remote shell would try to `exec` a
/// file literally named `$SHELL`. Instead we hand the remote a fixed `sh -c`
/// bootstrap that reintroduces exactly one controlled layer of
/// interpretation: it expands `$SHELL` and re-execs the user's login shell
/// in login mode (`-lc`) so their rc files rebuild `PATH`. The real script
/// rides in as the positional parameter `$0` (not spliced into the code
/// string), so `build_command`'s escaping is the *only* quoting applied —
/// no caller-side pre-escape, nothing to double-escape.
pub(crate) fn run_login_shell_script(host: &Host, script: &str) -> std::io::Result<Output> {
    shelbi_ssh::run(host, login_shell_argv(host, script))
}

/// The argv `run_login_shell_script` hands to `shelbi_ssh::run` for `host`.
/// Split out so tests can inspect / round-trip the wire without spawning a
/// real command.
fn login_shell_argv(host: &Host, script: &str) -> Vec<String> {
    let (shell, flag) = login_shell_prefix(host);
    match host {
        Host::Local => vec![shell, flag.to_string(), script.to_string()],
        Host::Ssh { .. } => vec![
            "sh".to_string(),
            "-c".to_string(),
            // `sh -c CODE ARG0` binds ARG0 to `$0`; CODE expands $SHELL and
            // re-execs it in login mode with the script as its `-c` arg.
            format!("exec \"{shell}\" {flag} \"$0\""),
            script.to_string(),
        ],
    }
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
///
/// Errors when the branch has *more than one* open PR (same head, two
/// different bases — GitHub allows it). Every downstream action (merge,
/// close, retarget) assumes "the PR for this branch" is well-defined;
/// silently picking the first listing would merge or close an arbitrary
/// one.
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
            ".[].number",
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr list --head {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    parse_open_pr_list(&String::from_utf8_lossy(&out.stdout), branch)
}

/// Return the commit currently at an open PR's head.
///
/// Zen PR creation uses this after pushing the named task branch so a PR
/// number cannot cross into CI watching or merging unless GitHub reports the
/// exact reviewed commit that was just pushed.
pub(crate) fn lookup_pr_head_oid(host: &Host, wt: &str, pr: u64) -> Result<String> {
    let pr_str = pr.to_string();
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "pr",
            "view",
            &pr_str,
            "--json",
            "headRefOid",
            "--jq",
            ".headRefOid",
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr view {pr_str} --json headRefOid"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if oid.is_empty() {
        return Err(Error::Other(format!(
            "gh pr view {pr_str}: headRefOid is empty"
        )));
    }
    Ok(oid)
}

/// Parse `gh pr list`'s one-number-per-line `--jq .[].number` output.
/// Split out of [`lookup_open_pr`] so the zero/one/many rules are
/// unit-testable without gh.
fn parse_open_pr_list(stdout: &str, branch: &str) -> Result<Option<u64>> {
    let mut numbers = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let n = trimmed.parse::<u64>().map_err(|_| {
            Error::Other(format!(
                "gh pr list returned non-numeric value `{trimmed}` for branch `{branch}`"
            ))
        })?;
        numbers.push(n);
    }
    match numbers.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some(*one)),
        many => {
            let list = many
                .iter()
                .map(|n| format!("#{n}"))
                .collect::<Vec<_>>()
                .join(", ");
            Err(Error::Other(format!(
                "branch `{branch}` has {} open PRs ({list}) — close or retarget \
                 the extras so shelbi knows which one to operate on",
                many.len()
            )))
        }
    }
}

/// The stored base branch (`baseRefName`) of a PR. Read by [`crate::actions::merge`]
/// *before* merging so restack cascades land children on the branch
/// GitHub actually merged into — not a recomputation of what the base
/// "should" be.
pub(crate) fn lookup_pr_base(host: &Host, wt: &str, pr: u64) -> Result<String> {
    let pr_str = pr.to_string();
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "pr",
            "view",
            &pr_str,
            "--json",
            "baseRefName",
            "--jq",
            ".baseRefName",
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr view {pr_str} --json baseRefName"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let base = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if base.is_empty() {
        return Err(Error::Other(format!(
            "gh pr view {pr_str}: baseRefName is empty"
        )));
    }
    Ok(base)
}

/// Backoff schedule for [`wait_for_merge_commit_sha`] — ~15s total, enough
/// to cover GitHub's usual post-merge bookkeeping lag without stalling a
/// genuinely stuck merge for long.
const MERGE_SHA_BACKOFF_SECS: [u64; 4] = [1, 2, 4, 8];

/// Read the merge commit SHA of a just-merged PR, polling with backoff.
///
/// GitHub finalizes merges asynchronously (merge queues, busy repos), so
/// `mergeCommit` can be null for a window after `gh pr merge` exits 0.
/// Returns:
///
/// - `Ok(Some(sha))` once the SHA materializes;
/// - `Ok(None)` when the PR reports `MERGED` but the SHA still isn't
///   recorded after all retries — the merge *succeeded*, callers must
///   treat this as "merged, SHA pending", not a failure;
/// - `Err` when the PR never reaches `MERGED` (e.g. it's still queued)
///   or gh itself fails.
pub(crate) fn wait_for_merge_commit_sha(host: &Host, wt: &str, pr: u64) -> Result<Option<String>> {
    let pr_str = pr.to_string();
    let mut attempt = 0;
    loop {
        let out = run_in_dir(
            host,
            wt,
            &[
                "gh",
                "pr",
                "view",
                &pr_str,
                "--json",
                "state,mergeCommit",
                "--jq",
                r#".state + " " + (.mergeCommit.oid // "")"#,
            ],
        )?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!("gh pr view {pr_str} --json state,mergeCommit"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut parts = stdout.split_whitespace();
        let state = parts.next().unwrap_or("").to_string();
        let oid = parts.next().unwrap_or("").to_string();
        if !oid.is_empty() {
            return Ok(Some(oid));
        }
        if attempt >= MERGE_SHA_BACKOFF_SECS.len() {
            if state == "MERGED" {
                return Ok(None);
            }
            return Err(Error::Other(format!(
                "gh pr view {pr_str}: merge reported success but the PR is \
                 `{state}` and mergeCommit.oid is still empty after retries — \
                 if the repo uses a merge queue the merge may land later; \
                 check the PR on GitHub"
            )));
        }
        std::thread::sleep(std::time::Duration::from_secs(
            MERGE_SHA_BACKOFF_SECS[attempt],
        ));
        attempt += 1;
    }
}

/// `git log -1 --format=%s <revision>` in `wt`, used as the default PR title
/// when opening a fresh PR. Callers choose the revision explicitly so an idle
/// workspace that has since checked out another task cannot lend that new
/// task's title to an older branch's PR.
pub(crate) fn commit_subject(host: &Host, wt: &str, revision: &str) -> Result<String> {
    let out = run_in_dir(
        host,
        wt,
        &["git", "log", "-1", "--format=%s", revision, "--"],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} log -1 --format=%s {revision} --"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// [`commit_subject`] for the worktree's current `HEAD`.
pub(crate) fn head_commit_subject(host: &Host, wt: &str) -> Result<String> {
    commit_subject(host, wt, "HEAD")
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
    fn open_pr_list_zero_and_one_are_clean() {
        assert_eq!(parse_open_pr_list("", "b").unwrap(), None);
        assert_eq!(parse_open_pr_list("\n", "b").unwrap(), None);
        assert_eq!(parse_open_pr_list("42\n", "b").unwrap(), Some(42));
    }

    #[test]
    fn open_pr_list_with_multiple_prs_errors_naming_them_all() {
        // GitHub allows two open PRs with the same head and different
        // bases. Downstream actions must not silently operate on `.[0]`;
        // the error names every candidate so the operator can pick.
        let err = parse_open_pr_list("42\n77\n", "shelbi/x").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("2 open PRs"), "{msg}");
        assert!(msg.contains("#42"), "{msg}");
        assert!(msg.contains("#77"), "{msg}");
        assert!(msg.contains("shelbi/x"), "{msg}");
    }

    #[test]
    fn open_pr_list_rejects_non_numeric_rows() {
        let err = parse_open_pr_list("nope\n", "b").unwrap_err();
        assert!(err.to_string().contains("non-numeric"), "{err}");
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

    /// The `run_login_shell_script` SSH argv, replayed through the local
    /// `ssh` argv builder and then a local `sh -c` standing in for the
    /// remote login shell. Returns the words after `--` joined the way ssh
    /// would send them to the remote.
    fn ssh_login_shell_wire(host: &Host, script: &str) -> String {
        let cmd = shelbi_ssh::build_command(host, login_shell_argv(host, script));
        let parts: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let dd = parts.iter().position(|a| a == "--").expect("missing --");
        parts[dd + 1..].join(" ")
    }

    /// Regression test for the `shelbi zen probe` devbox bug (and its F2
    /// evolution): the `cd <wt> && <cmd>` script has to survive OpenSSH's
    /// "join argv with literal spaces" join AND the remote login-shell
    /// bootstrap, or the `&&` leaks out to the outer remote shell and the
    /// `cd` silently no-ops. We can't reach a real remote, so we replay the
    /// exact wire through a local `sh -c` (with `$SHELL` forced to `/bin/sh`
    /// for hermeticity) and assert both the `$SHELL` expansion and the `&&`
    /// survive: only if the whole chain holds does the second clause run.
    #[test]
    fn ssh_login_shell_script_round_trips_through_remote_shell() {
        let host = Host::Ssh {
            host: "devbox".into(),
        };
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_string_lossy();
        // `cd <dir> && printf` — dir is shell_escaped into the script the
        // way `run_in_dir` builds it. If `$SHELL` fails to expand, exec dies;
        // if `&&` leaks, the printf runs from the wrong cwd or not at all.
        let script = format!(
            "cd {} && printf ran-in:%s \"$PWD\"",
            shelbi_agent::shell_escape(&dir)
        );
        let wire = ssh_login_shell_wire(&host, &script);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&wire)
            .env("SHELL", "/bin/sh")
            .output()
            .expect("sh -c failed to run");
        assert!(out.status.success(), "sh exited nonzero (wire: {wire})");
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Canonicalize: macOS /var symlinks to /private/var, and login sh
        // may resolve $PWD differently — compare on the trailing component.
        assert!(
            stdout.starts_with("ran-in:")
                && stdout
                    .trim()
                    .ends_with(tmp.path().file_name().unwrap().to_str().unwrap()),
            "expected the cd+printf chain to run in {}, got: {stdout} (wire: {wire})",
            dir,
        );
    }

    /// Double-escape guard: the caller passes a *raw* script — no
    /// `shell_escape` before `run_login_shell_script`. If a future edit
    /// reintroduced a pre-escape, the script would arrive at the remote
    /// wrapped in an extra layer of quotes and `sh -c` would echo the quote
    /// characters back. Assert the payload comes through byte-for-byte.
    #[test]
    fn ssh_login_shell_script_is_not_double_escaped() {
        let host = Host::Ssh {
            host: "devbox".into(),
        };
        let script = "printf %s hello-world";
        let wire = ssh_login_shell_wire(&host, script);
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&wire)
            .env("SHELL", "/bin/sh")
            .output()
            .expect("sh -c failed to run");
        assert!(out.status.success(), "sh exited nonzero (wire: {wire})");
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            "hello-world",
            "payload was mangled — likely double-escaped (wire: {wire})",
        );
    }

    #[test]
    fn local_login_shell_argv_passes_script_verbatim() {
        // The local path goes through `std::process::Command`, which hands
        // each argv element to exec without a shell in between, so the
        // script must NOT be wrapped in single quotes — `$SHELL -lc` would
        // then try to run a command literally named `'cd ... && echo hi'`.
        let script = "cd '/tmp' && echo hi";
        let argv = login_shell_argv(&Host::Local, script);
        assert_eq!(argv.last().map(String::as_str), Some(script));
        assert_eq!(argv.get(1).map(String::as_str), Some("-lc"));
    }
}
