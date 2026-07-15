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

/// Like [`run_login_shell_script`], but bound the child's total wall-clock
/// time: if the login shell (and whatever it spawns) hasn't finished within
/// `deadline`, it is killed and the call returns `ErrorKind::TimedOut`.
///
/// This is the guard the Zen local-check runner uses. A local check is
/// arbitrary user-supplied shell (`cargo test --workspace`, `npm test`, …);
/// on a loaded hub such a command can wedge for many minutes — long enough
/// that a worker looping on verification, or `shelbi zen probe` itself,
/// appears hung. Without a deadline the wedge propagates all the way up and
/// stalls the orchestrator; with one, the check fails fast and CI (which
/// runs the authoritative suite in isolation) stays the source of truth.
pub(crate) fn run_login_shell_script_with_deadline(
    host: &Host,
    script: &str,
    deadline: std::time::Duration,
) -> std::io::Result<Output> {
    shelbi_ssh::run_with_deadline(host, login_shell_argv(host, script), deadline)
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
    lookup_open_pr_impl(host, wt, branch, None)
}

/// Repository-bound variant used by Zen after resolving the exact `origin`
/// push target. This prevents a `gh repo set-default` override from selecting
/// a same-named branch in a different base repository.
pub(crate) fn lookup_open_pr_in_repository(
    host: &Host,
    wt: &str,
    branch: &str,
    repository: &str,
) -> Result<Option<u64>> {
    lookup_open_pr_impl(host, wt, branch, Some(repository))
}

fn lookup_open_pr_impl(
    host: &Host,
    wt: &str,
    branch: &str,
    repository: Option<&str>,
) -> Result<Option<u64>> {
    let mut args = vec!["gh", "pr", "list"];
    if let Some(repository) = repository {
        args.extend(["--repo", repository]);
    }
    args.extend([
        "--head",
        branch,
        "--state",
        "open",
        "--json",
        "number",
        "--jq",
        ".[].number",
    ]);
    let out = run_in_dir(host, wt, &args)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!(
                "gh pr list{} --head {branch}",
                repository.map_or(String::new(), |repo| format!(" --repo {repo}"))
            ),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    parse_open_pr_list(&String::from_utf8_lossy(&out.stdout), branch)
}

/// The immutable and routing-sensitive identity of an open pull request.
///
/// A branch name and commit OID are not sufficient to identify the PR Shelbi
/// just published: GitHub permits the same head branch to have PRs against
/// different bases, and a fork can expose the same branch name and commit.
/// Zen PR reuse checks every field before allowing the number into CI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PrIdentity {
    pub(crate) head_oid: String,
    pub(crate) head_ref: String,
    pub(crate) base_ref: String,
    pub(crate) base_oid: String,
    pub(crate) head_repository: RepositoryIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RepositoryIdentity {
    pub(crate) id: String,
    pub(crate) name_with_owner: String,
    /// Credential-free `[HOST/]OWNER/REPO` selector for every downstream gh
    /// command. Keeping the host is mandatory for GitHub Enterprise.
    pub(crate) selector: String,
    pub(crate) host: String,
}

/// Return the PR head commit, head/base branch names, and owning repository.
pub(crate) fn lookup_pr_identity(
    host: &Host,
    wt: &str,
    repository: &str,
    pr: u64,
) -> Result<PrIdentity> {
    let pr_str = pr.to_string();
    let fields = "headRefOid,headRefName,baseRefName,baseRefOid,headRepository";
    let query = r#"[.headRefOid, .headRefName, .baseRefName, .baseRefOid, .headRepository.id, .headRepository.nameWithOwner] | .[]"#;
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh", "pr", "view", &pr_str, "--repo", repository, "--json", fields, "--jq", query,
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr view {pr_str} --repo {repository} --json {fields}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let values: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .map(str::to_string)
        .collect();
    let [head_oid, head_ref, base_ref, base_oid, head_repository_id, head_repository_name] =
        values.as_slice()
    else {
        return Err(Error::Other(format!(
            "gh pr view {pr_str}: expected head/base OIDs, head/base names, and head repository identity, got {} value(s)",
            values.len()
        )));
    };
    if values
        .iter()
        .any(|value| value.is_empty() || value == "null")
    {
        return Err(Error::Other(format!(
            "gh pr view {pr_str}: PR identity contains an empty head, base, or repository field"
        )));
    }
    Ok(PrIdentity {
        head_oid: head_oid.clone(),
        head_ref: head_ref.clone(),
        base_ref: base_ref.clone(),
        base_oid: base_oid.clone(),
        head_repository: RepositoryIdentity {
            id: head_repository_id.clone(),
            name_with_owner: head_repository_name.clone(),
            // The PR was queried through the already resolved origin selector,
            // so its host/routing identity is the same API host. Equality is
            // still decided by the immutable repository id.
            selector: repository.to_string(),
            host: repository
                .split('/')
                .next()
                .unwrap_or("github.com")
                .to_string(),
        },
    })
}

/// Resolve the immutable GitHub identity of the exact `origin` push target.
/// This avoids both similarly named forks and a `gh repo set-default` override
/// that points somewhere other than the remote Shelbi just pushed.
pub(crate) fn lookup_origin_repository(host: &Host, wt: &str) -> Result<RepositoryIdentity> {
    lookup_origin_repository_with_push_target(host, wt).map(|(repository, _)| repository)
}

/// Resolve `origin` once and return both its immutable GitHub identity and the
/// exact push target that produced that identity. Callers that mutate remote
/// state can use the captured target instead of re-reading a mutable remote
/// name after the identity check.
pub(crate) fn lookup_origin_repository_with_push_target(
    host: &Host,
    wt: &str,
) -> Result<(RepositoryIdentity, String)> {
    let (remote, selector) = lookup_origin_push_target(host, wt)?;
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "repo",
            "view",
            &selector,
            "--json",
            "id,nameWithOwner,url",
            "--jq",
            "[.id, .nameWithOwner, .url] | .[]",
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            // Push URLs can contain credentials. Keep the actual argv out of
            // diagnostics while still naming the failed verification step.
            cmd: "gh repo view <origin-push-url> --json id,nameWithOwner,url".into(),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).replace(&remote, "<origin-push-url>"),
        });
    }
    let values: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .map(str::to_string)
        .collect();
    let [id, name_with_owner, repository_url] = values.as_slice() else {
        return Err(Error::Other(
            "gh repo view: expected repository id, nameWithOwner, and URL; refusing to identify a PR by branch name alone"
                .into(),
        ));
    };
    if id.is_empty()
        || id == "null"
        || name_with_owner.is_empty()
        || name_with_owner == "null"
        || repository_url.is_empty()
        || repository_url == "null"
    {
        return Err(Error::Other(
            "gh repo view: repository id, nameWithOwner, or URL is empty; refusing to identify a PR by branch name alone"
                .into(),
        ));
    }
    let canonical = credential_free_repository_selector(repository_url)?;
    let host = canonical.split('/').next().unwrap_or("").to_string();
    if host.is_empty() {
        return Err(Error::Other(
            "gh repo view: repository URL has no host; refusing ambiguous GitHub routing".into(),
        ));
    }
    Ok((
        RepositoryIdentity {
            id: id.clone(),
            name_with_owner: name_with_owner.clone(),
            selector: format!("{host}/{name_with_owner}"),
            host,
        },
        remote,
    ))
}

pub(crate) fn lookup_origin_repository_selector(host: &Host, wt: &str) -> Result<String> {
    lookup_origin_push_target(host, wt).map(|(_, selector)| selector)
}

fn lookup_origin_push_target(host: &Host, wt: &str) -> Result<(String, String)> {
    let remote = run_in_dir(
        host,
        wt,
        &["git", "remote", "get-url", "--push", "--all", "origin"],
    )?;
    if !remote.status.success() {
        return Err(Error::Command {
            cmd: "git remote get-url --push --all origin".into(),
            status: remote.status.to_string(),
            stderr: String::from_utf8_lossy(&remote.stderr).into_owned(),
        });
    }
    let mut remotes: Vec<String> = String::from_utf8_lossy(&remote.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect();
    remotes.sort();
    remotes.dedup();
    let [remote] = remotes.as_slice() else {
        return Err(Error::Other(
            "origin must have exactly one distinct push target before Shelbi can verify a PR's head repository"
                .into(),
        ));
    };

    let selector = credential_free_repository_selector(remote)?;
    Ok((remote.clone(), selector))
}

/// Convert a network Git URL to gh's credential-free `[HOST/]OWNER/REPO`
/// selector. Local filesystem remotes are retained for hermetic/test setups;
/// unlike URL userinfo they do not place an authentication secret in argv.
fn credential_free_repository_selector(remote: &str) -> Result<String> {
    if remote
        .chars()
        .any(|character| matches!(character, '\n' | '\r' | '\0'))
    {
        return Err(Error::Other(
            "origin push URL contains control characters; refusing GitHub repository lookup".into(),
        ));
    }

    if let Some((scheme, rest)) = remote.split_once("://") {
        if scheme.is_empty() {
            return Err(Error::Other(
                "origin push URL has an empty scheme; refusing GitHub repository lookup".into(),
            ));
        }
        let (authority, path) = rest.split_once('/').ok_or_else(|| {
            Error::Other(
                "origin push URL does not identify an owner and repository; refusing GitHub repository lookup"
                    .into(),
            )
        })?;
        let host = authority.rsplit('@').next().unwrap_or("");
        return hosted_repository_selector(host, path);
    }

    // SCP-style SSH remotes: `git@github.example:owner/repo.git`.
    if let Some((authority, path)) = remote.split_once(':') {
        if authority.contains('@') && !authority.contains('/') {
            let host = authority.rsplit('@').next().unwrap_or("");
            return hosted_repository_selector(host, path);
        }
    }

    Ok(remote.to_string())
}

fn hosted_repository_selector(host: &str, path: &str) -> Result<String> {
    let host = host.trim();
    let path = path
        .split(['?', '#'])
        .next()
        .unwrap_or("")
        .trim_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty() || path.is_empty() || !path.contains('/') {
        return Err(Error::Other(
            "origin push URL does not identify a host, owner, and repository; refusing GitHub repository lookup"
                .into(),
        ));
    }
    Ok(format!("{host}/{path}"))
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
/// merge-queue acknowledgement for long.
const MERGE_SHA_BACKOFF_SECS: [u64; 4] = [1, 2, 4, 8];

#[derive(Debug, Clone, PartialEq, Eq)]
enum MergeCommitPoll {
    Complete(Option<String>),
    Retry,
}

/// Interpret one `gh pr view` result from the post-merge poll.
fn classify_merge_commit_poll(
    pr: u64,
    state: &str,
    oid: &str,
    retries_exhausted: bool,
) -> Result<MergeCommitPoll> {
    if !oid.is_empty() {
        return Ok(MergeCommitPoll::Complete(Some(oid.to_string())));
    }
    if !retries_exhausted {
        return Ok(MergeCommitPoll::Retry);
    }
    match state {
        "MERGED" => Ok(MergeCommitPoll::Complete(None)),
        _ => Err(Error::Other(format!(
            "gh pr view {pr}: merge reported success but the PR is `{state}` and \
             mergeCommit.oid is still empty after retries; check the PR on GitHub"
        ))),
    }
}

/// Read the merge commit SHA of a merged PR, polling with backoff.
///
/// GitHub finalizes merges asynchronously (merge queues, busy repos), so
/// `mergeCommit` can be null for a window after `gh pr merge` exits 0.
/// Returns:
///
/// - `Ok(Some(sha))` once the SHA materializes;
/// - `Ok(None)` when the PR reports `MERGED` without a recorded SHA;
/// - `Err` when gh fails or the PR reaches another terminal state.
pub(crate) fn wait_for_merge_commit_sha(host: &Host, wt: &str, pr: u64) -> Result<Option<String>> {
    wait_for_merge_commit_sha_impl(host, wt, pr)
}

fn wait_for_merge_commit_sha_impl(
    host: &Host,
    wt: &str,
    pr: u64,
) -> Result<Option<String>> {
    let pr_str = pr.to_string();
    let mut attempt = 0;
    loop {
        let mut args = vec!["gh", "pr", "view", pr_str.as_str()];
        args.extend([
            "--json",
            "state,mergeCommit",
            "--jq",
            r#".state + " " + (.mergeCommit.oid // "")"#,
        ]);
        let out = run_in_dir(host, wt, &args)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!(
                    "gh pr view {pr_str} --json state,mergeCommit"
                ),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let mut parts = stdout.split_whitespace();
        let state = parts.next().unwrap_or("").to_string();
        let oid = parts.next().unwrap_or("").to_string();
        match classify_merge_commit_poll(
            pr,
            &state,
            &oid,
            attempt >= MERGE_SHA_BACKOFF_SECS.len(),
        )? {
            MergeCommitPoll::Complete(sha) => return Ok(sha),
            MergeCommitPoll::Retry => {}
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
    fn repository_selector_strips_network_credentials() {
        assert_eq!(
            credential_free_repository_selector(
                "https://oauth2:secret-token@github.example/owner/repo.git"
            )
            .unwrap(),
            "github.example/owner/repo"
        );
        assert_eq!(
            credential_free_repository_selector("git@github.com:owner/repo.git").unwrap(),
            "github.com/owner/repo"
        );
    }

    #[test]
    fn repository_selector_retains_credential_free_local_remote() {
        assert_eq!(
            credential_free_repository_selector("/tmp/repositories/repo.git").unwrap(),
            "/tmp/repositories/repo.git"
        );
    }

    #[test]
    fn generic_workflow_merge_does_not_treat_open_pr_as_landed() {
        let error = classify_merge_commit_poll(379, "OPEN", "", true).unwrap_err();
        assert!(error.to_string().contains("PR is `OPEN`"));
    }

    #[test]
    fn post_merge_poll_preserves_existing_terminal_behavior() {
        assert_eq!(
            classify_merge_commit_poll(379, "MERGED", "abc123", false).unwrap(),
            MergeCommitPoll::Complete(Some("abc123".into()))
        );
        assert_eq!(
            classify_merge_commit_poll(379, "MERGED", "", true).unwrap(),
            MergeCommitPoll::Complete(None)
        );

        let error = classify_merge_commit_poll(379, "CLOSED", "", true).unwrap_err();
        let message = error.to_string();
        assert!(message.contains("PR is `CLOSED`"), "{message}");
        assert!(message.contains("mergeCommit.oid"), "{message}");
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
