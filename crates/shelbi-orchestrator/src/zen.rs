//! Zen Mode primitives — `pr_create`, `ci_watch`, `pr_merge`.
//!
//! Each function does one thing. The orchestrator sequences them per its
//! Merge Conditions policy; no primitive implies what the next should do.
//! Same shape as the readiness probe primitives: Rust performs the I/O,
//! the orchestrator's prompt makes the decisions.
//!
//! `pr_create` resolves the task's durable named branch through its assigned
//! workspace repository, without trusting that workspace's current checkout.
//! `ci_watch` and `pr_merge` run on the project's first local machine — by
//! convention the hub — because by the time the orchestrator is watching CI
//! the branch is already on origin and gh is happy from any checkout of the
//! repo.
//!
//! Polling vs. `gh pr checks --watch`: `--watch` has no built-in timeout
//! and `timeout(1)` is not standard on macOS. Polling `gh pr checks`
//! (without `--watch`) lets us bound wall-clock exactly with stdlib timers
//! while still leaning on gh's own exit-code contract (0 / 1 / 8) for the
//! verdict.

use std::path::{Component, PathBuf};
use std::process::Output;
use std::time::{Duration, Instant};

use globset::{Glob, GlobSetBuilder};
use serde::Serialize;
use shelbi_core::{
    checks_for_task_in_workflow, danger_paths_for_workflow, Column, Error, Host, Machine, Project,
    Result, StatusCategory, Task, Workflow, WorkflowStatus, WorkspaceSpec,
};

use crate::branch;
use crate::git::{
    commit_subject, compose_pr_body, locate_hub_workdir, locate_workspace_worktree,
    login_shell_prefix, lookup_open_pr, lookup_pr_head_oid, parse_pr_number_from_url, run_in_dir,
    run_login_shell_script, wait_for_merge_commit_sha,
};
use crate::workspace::{rebase_workspace_branch_onto_default, workspace_worktree, RebaseOutcome};

/// How often `ci_watch` re-runs `gh pr checks` while waiting for the
/// pending bucket to clear. Matches gh's own `--watch` default.
const CI_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Which set of checks [`ci_watch`] is watching on this poll loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WatchMode {
    /// Poll `gh pr checks --required` — the strict path used when the
    /// target repo *does* configure branch-protection required status
    /// checks. Only the required set counts.
    Required,
    /// Poll `gh pr checks` (no `--required`) — the fallback used when
    /// the target repo has no required checks configured (unprotected
    /// branch or protected-but-no-required-set). Every check reported
    /// on the PR counts.
    AllReported,
}

/// Outcome of a `gh pr checks` poll loop. In required-checks mode the
/// verdict reflects only the required set; in all-reported mode it
/// reflects every check reported on the PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiVerdict {
    /// All required checks finished in a passing bucket.
    Green,
    /// At least one required check landed in a failing bucket. We surface
    /// the first failing check by name with whatever short message the
    /// gh output gave us, so the orchestrator can quote it back to the
    /// user without needing to re-run gh.
    Red { check: String, summary: String },
    /// The user-supplied (or project-default) timeout fired before all
    /// required checks resolved.
    Timeout,
}

impl CiVerdict {
    /// Single-line wire format printed on stdout by `shelbi zen ci-watch`.
    /// Colon-delimited so the orchestrator's prompt can match on prefix
    /// (`green` / `red:` / `timeout`) without parsing JSON. Internal
    /// colons in the check name and summary are flattened so the line
    /// stays exactly three fields.
    pub fn as_line(&self) -> String {
        match self {
            CiVerdict::Green => "green".to_string(),
            CiVerdict::Red { check, summary } => {
                let safe_check = check.replace(':', "_");
                let safe_summary = summary.replace(['\n', ':'], " ");
                let trimmed = safe_summary.trim();
                format!("red:{safe_check}:{trimmed}")
            }
            CiVerdict::Timeout => "timeout".to_string(),
        }
    }
}

/// Detect the "no required checks reported" message gh emits when the
/// target branch has no branch-protection required status checks
/// configured. Matched on message text — gh conflates this case with a
/// real failure by returning exit 1 in both, so the wire text is the
/// only disambiguator.
pub fn is_no_required_checks_message(stdout: &str, stderr: &str) -> bool {
    let needle = "no required checks reported";
    stdout.contains(needle) || stderr.contains(needle)
}

/// Detect gh's "no checks reported on the '<branch>' branch" message —
/// the zero-checks-at-all case. This is what `gh pr checks` (no
/// `--required`) prints when the PR has no checks whatsoever: e.g. a
/// docs-only diff whose path filters skip every CI workflow. Distinct
/// from [`is_no_required_checks_message`], which only appears under
/// `--required` and whose text ("no *required* checks reported") does
/// not contain this needle. When this fires in the all-reported
/// fallback there are no checks to grade, so `ci_watch` falls back to
/// the PR's merge state to decide green.
pub fn is_no_checks_reported_message(stdout: &str, stderr: &str) -> bool {
    let needle = "no checks reported";
    stdout.contains(needle) || stderr.contains(needle)
}

/// gh's `mergeStateStatus`, distilled to the three outcomes `ci_watch`'s
/// no-checks fallback cares about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MergeReadiness {
    /// `CLEAN` — GitHub considers the PR mergeable with nothing pending.
    /// This is the only value the fallback treats as green.
    Clean,
    /// `UNKNOWN` / empty — GitHub hasn't finished computing mergeability.
    /// Transient; keep polling until it resolves or the deadline fires.
    Pending,
    /// Anything else (`BLOCKED`, `DIRTY`, `BEHIND`, `UNSTABLE`, `DRAFT`,
    /// `HAS_HOOKS`, ...) — not mergeable-and-green right now. Keep polling
    /// too: some of these clear on their own (a required review lands, the
    /// base updates) and the caller's deadline bounds the wait.
    Blocked,
}

/// Map a raw gh `mergeStateStatus` string to a [`MergeReadiness`].
fn classify_merge_state(status: &str) -> MergeReadiness {
    match status.trim().to_ascii_uppercase().as_str() {
        "CLEAN" => MergeReadiness::Clean,
        "" | "UNKNOWN" => MergeReadiness::Pending,
        _ => MergeReadiness::Blocked,
    }
}

/// What a single `gh pr checks` poll tells `ci_watch` to do next. Pure
/// function of the poll's exit code and output plus the current
/// [`WatchMode`], so the branchy verdict logic is unit-testable without
/// spawning gh.
#[derive(Debug, Clone, PartialEq, Eq)]
enum PollOutcome {
    /// Required-checks mode, exit 0 — every required check passed. Green
    /// outright; the strict path never consults merge state.
    Green,
    /// A watched check landed in a failing bucket. Surfaced verbatim.
    Red { check: String, summary: String },
    /// A watched check is still pending (gh exit 8). Sleep and re-poll.
    Pending,
    /// Required mode saw "no required checks reported" — switch to the
    /// all-reported fallback and re-poll immediately.
    FlipToAllReported,
    /// All-reported fallback: either every reported check passed, or the
    /// PR has no checks at all. Neither settles the verdict on its own —
    /// consult the PR's merge state and go green only when it's CLEAN.
    ConfirmMergeState,
}

/// Interpret one `gh pr checks` poll. `code` is the process exit code.
fn classify_poll(mode: WatchMode, code: i32, stdout: &str, stderr: &str) -> PollOutcome {
    match code {
        // 0 — every watched check passed.
        0 => match mode {
            WatchMode::Required => PollOutcome::Green,
            // In the fallback, all-green checks are necessary but not
            // sufficient: gate on merge state so a repo with only
            // non-required checks still lands on CLEAN before we call it.
            WatchMode::AllReported => PollOutcome::ConfirmMergeState,
        },
        // 8 — at least one watched check is still pending.
        8 => PollOutcome::Pending,
        // Any other non-zero — a failure, OR a "no (required) checks"
        // sentinel that gh conflates with failure via the same exit code.
        _ => match mode {
            WatchMode::Required => {
                if is_no_required_checks_message(stdout, stderr) {
                    PollOutcome::FlipToAllReported
                } else {
                    red_from_output(stdout, stderr)
                }
            }
            WatchMode::AllReported => {
                if is_no_checks_reported_message(stdout, stderr) {
                    // No checks exist at all — defer to merge state.
                    PollOutcome::ConfirmMergeState
                } else {
                    red_from_output(stdout, stderr)
                }
            }
        },
    }
}

/// Build a [`PollOutcome::Red`] from failing `gh pr checks` output,
/// falling back to the last output line when no row parses as a failure.
fn red_from_output(stdout: &str, stderr: &str) -> PollOutcome {
    let (check, summary) = first_failing_check(stdout).unwrap_or_else(|| {
        let fallback = stdout
            .lines()
            .last()
            .or_else(|| stderr.lines().last())
            .unwrap_or("")
            .trim()
            .to_string();
        ("unknown".to_string(), fallback)
    });
    PollOutcome::Red { check, summary }
}

/// Push the task's branch and open a PR, pinned to the exact branch commit
/// previously reviewed by a Zen probe. Idempotent: if an open PR for the
/// branch already exists, reuse it only after verifying that its head is the
/// exact task branch commit that was pushed.
pub fn pr_create_at_head(
    project: &Project,
    project_name: &str,
    task: &Task,
    task_body: &str,
    expected_head: &str,
) -> Result<u64> {
    pr_create_impl(project, project_name, task, task_body, expected_head)
}

fn pr_create_impl(
    project: &Project,
    project_name: &str,
    task: &Task,
    task_body: &str,
    expected_head: &str,
) -> Result<u64> {
    let (host, worktree) = locate_workspace_worktree(project, task)?;
    let wt = worktree.to_string_lossy().into_owned();
    let workflow = shelbi_state::load_task_workflow(project_name, project, task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let branch = branch::branch_name_for_task(project, Some(&workflow), task)?;
    let target = project.base_branch();

    // Handoff detaches the finished worktree and immediately frees its slot.
    // A later dispatch may therefore put this worktree's HEAD on an unrelated
    // task while the old task remains assigned to the workspace in Review.
    // The surviving named task ref is the durable authority; the worktree is
    // only an execution anchor for git and gh.
    let local_ref = format!("refs/heads/{branch}");
    let branch_commit = format!("{local_ref}^{{commit}}");
    let operation_head = probe_head_sha(&host, &worktree, &branch_commit)?;
    if expected_head != operation_head {
        return Err(Error::Other(format!(
            "task branch `{branch}` moved since it was probed: expected {expected_head}, found \
             {operation_head}; refusing to push or report a PR ready for CI or merge \
             (re-run `shelbi zen probe {}`)",
            task.id
        )));
    }

    // Push the immutable commit snapshot before looking up an existing PR.
    // Using the OID as the refspec source prevents a concurrent local ref move
    // from changing what git publishes during negotiation. This is a normal,
    // non-force push: fast-forward stale PR branches advance, while rewritten
    // or divergent remote branches are rejected rather than overwritten.
    let refspec = format!("{operation_head}:{local_ref}");
    let push = run_in_dir(&host, &wt, &["git", "push", "origin", "--", &refspec])?;
    if !push.status.success() {
        let mut stderr = String::from_utf8_lossy(&push.stderr).into_owned();
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "shelbi: task branch `{branch}` at {operation_head} was not pushed; refusing to \
             reuse or create a PR because the remote branch may have been rewritten or diverged"
        ));
        return Err(Error::Command {
            cmd: format!("git -C {wt} push origin -- {refspec}"),
            status: push.status.to_string(),
            stderr,
        });
    }
    verify_task_branch_head(
        &host,
        &worktree,
        &branch,
        &operation_head,
        "while it was being pushed",
    )?;

    // Idempotency, after the push: an existing PR now has the opportunity to
    // advance to the reviewed task commit. Picking `state=open` intentionally;
    // a closed or merged PR for this branch is stale and a fresh push warrants
    // a fresh PR. A server-side push hook may also have opened a PR, so this
    // lookup covers both normal reuse and that race window.
    if let Some(num) = lookup_open_pr(&host, &wt, &branch)? {
        verify_pr_head(&host, &worktree, &branch, num, &operation_head)?;
        return Ok(num);
    }

    let title = commit_subject(&host, &wt, &operation_head)?;
    let task_path = shelbi_state::task_path(project_name, &task.id)?
        .to_string_lossy()
        .into_owned();
    let body = compose_pr_body(task_body, &task_path);
    verify_task_branch_head(
        &host,
        &worktree,
        &branch,
        &operation_head,
        "before a new PR was created",
    )?;

    let out = run_in_dir(
        &host,
        &wt,
        &[
            "gh", "pr", "create", "--head", &branch, "--base", target, "--title", &title, "--body",
            &body,
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr create --head {branch} --base {target}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let num = parse_pr_number_from_url(stdout.trim()).ok_or_else(|| {
        Error::Other(format!(
            "gh pr create returned `{}` — couldn't parse a PR number out of it",
            stdout.trim()
        ))
    })?;
    verify_pr_head(&host, &worktree, &branch, num, &operation_head)?;
    Ok(num)
}

/// Prove that `pr` still names the exact reviewed task branch commit before
/// its number is allowed to reach CI watching or merging.
fn verify_pr_head(
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
    pr: u64,
    pushed_head: &str,
) -> Result<()> {
    let wt = worktree.to_string_lossy().into_owned();
    verify_task_branch_head(
        host,
        worktree,
        branch,
        pushed_head,
        "before its PR head was verified",
    )?;
    let pr_head = lookup_pr_head_oid(host, &wt, pr)?;
    verify_task_branch_head(
        host,
        worktree,
        branch,
        pushed_head,
        "while its PR head was being verified",
    )?;
    if pr_head != pushed_head {
        return Err(Error::Other(format!(
            "open PR #{pr} for branch `{branch}` points to {pr_head}, but the reviewed task \
             branch commit is {pushed_head}; refusing to report the stale PR ready for CI or merge"
        )));
    }
    Ok(())
}

/// Re-read the durable task branch ref and require it to stay on the commit
/// this PR operation snapshotted. The assigned worktree's `HEAD` is
/// intentionally irrelevant because the slot may already serve another task.
fn verify_task_branch_head(
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
    expected_head: &str,
    context: &str,
) -> Result<()> {
    let local_ref = format!("refs/heads/{branch}^{{commit}}");
    let current_head = probe_head_sha(host, worktree, &local_ref)?;
    if current_head != expected_head {
        return Err(Error::Other(format!(
            "task branch `{branch}` moved from {expected_head} to {current_head} {context}; \
             refusing to report its PR ready for CI or merge"
        )));
    }
    Ok(())
}

/// Poll `gh pr checks` on `pr` until every watched check settles (pass
/// or fail) or `timeout` elapses.
///
/// Two modes, selected at runtime:
///
/// - **Required-checks mode** (`gh pr checks --required`) — the strict
///   path. Only branch-protection required status checks count; every
///   other reported check is ignored.
/// - **All-reported fallback** (`gh pr checks` with no `--required`) —
///   auto-selected when the target repo has no required checks
///   configured (unprotected branch, or protected-but-no-required-set).
///   Every check reported on the PR counts. In this mode a green
///   verdict is confirmed against the PR's `mergeStateStatus`: all
///   reported checks passing (or *no checks at all* — e.g. a docs-only
///   diff that skips every CI path filter) yields `green` only once gh
///   reports the PR `CLEAN`. Until then we keep polling.
///
/// Rationale for the fallback: many repos never configure
/// branch-protection required status checks. Without the fallback,
/// `gh pr checks --required` on such a PR exits non-zero with `no
/// required checks reported on the '<branch>' branch`, and `ci-watch`
/// would surface that as `red:unknown:...` within a second — never
/// observing the actual `app-ci` / `Vercel` / etc. checks that were
/// queued or running (see issue #102 for the failure story that
/// motivated this fix). The strict path is preserved when required
/// checks *are* configured. The merge-state confirmation additionally
/// closes the zero-checks hole: a PR with no checks whatsoever used to
/// fall through to `red:unknown:no checks reported`; now a CLEAN such
/// PR reports `green`.
pub fn ci_watch(project: &Project, pr: u64, timeout: Duration) -> Result<CiVerdict> {
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    let pr_str = pr.to_string();

    let deadline = Instant::now() + timeout;
    // Start in the strict required-checks mode. On the first poll that
    // returns "no required checks reported" we switch to the fallback
    // for the remainder of the run. The mode never flips back — a repo
    // doesn't gain required checks mid-poll.
    let mut mode = WatchMode::Required;
    loop {
        let args: &[&str] = match mode {
            WatchMode::Required => &["gh", "pr", "checks", &pr_str, "--required"],
            WatchMode::AllReported => &["gh", "pr", "checks", &pr_str],
        };
        let out = run_in_dir(&host, &wt, args)?;
        let Some(code) = out.status.code() else {
            return Err(Error::Other(format!(
                "gh pr checks {pr_str} terminated without an exit code"
            )));
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        match classify_poll(mode, code, &stdout, &stderr) {
            PollOutcome::Green => return Ok(CiVerdict::Green),
            PollOutcome::Red { check, summary } => return Ok(CiVerdict::Red { check, summary }),
            PollOutcome::FlipToAllReported => {
                // No required checks configured on the target branch —
                // flip to the all-reported fallback and re-poll
                // immediately so we don't burn a sleep interval on a mode
                // we've already ruled out.
                mode = WatchMode::AllReported;
                continue;
            }
            PollOutcome::ConfirmMergeState => {
                // Checks are green or absent. The all-reported rollup
                // can't distinguish "mergeable" from "blocked on
                // something else", so borrow gh's own mergeability
                // verdict: CLEAN is green, everything else keeps polling.
                if let MergeReadiness::Clean =
                    classify_merge_state(&merge_state_status(&host, &wt, &pr_str)?)
                {
                    return Ok(CiVerdict::Green);
                }
                // Not CLEAN yet — fall through to sleep + retry.
            }
            PollOutcome::Pending => { /* fall through to sleep + retry */ }
        }

        let now = Instant::now();
        if now >= deadline {
            return Ok(CiVerdict::Timeout);
        }
        // Don't oversleep the deadline — sleep at most until it fires so
        // the user-visible timeout is honored within ~one poll interval.
        let remaining = deadline.saturating_duration_since(now);
        std::thread::sleep(remaining.min(CI_POLL_INTERVAL));
    }
}

/// Ask gh for the PR's `mergeStateStatus`. Used by [`ci_watch`]'s
/// no-required-checks fallback to decide green when the check rollup
/// can't (all-passing-but-non-required, or no checks at all).
///
/// A gh call that runs but reports a non-zero exit (or empty output) is
/// treated as "not yet known" — we return an empty string, which
/// [`classify_merge_state`] maps to `Pending` so the caller keeps
/// polling rather than hard-failing the watch on a transient hiccup. A
/// gh process that can't launch at all still propagates as an error.
fn merge_state_status(host: &Host, wt: &str, pr_str: &str) -> Result<String> {
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "pr",
            "view",
            pr_str,
            "--json",
            "mergeStateStatus",
            "--jq",
            ".mergeStateStatus // empty",
        ],
    )?;
    if !out.status.success() {
        return Ok(String::new());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Integrate `pr` using the project's configured
/// [`shelbi_core::MergeStrategy`] and delete its *remote* source branch.
/// Returns the merge SHA, or `None` when GitHub reported the PR merged but
/// hadn't recorded the merge commit yet after the polling window (merge
/// queues, busy repos) — the merge itself succeeded.
///
/// The *local* branch is intentionally left alone: it's still checked out
/// in the finishing workspace's worktree, so `gh pr merge --delete-branch`
/// would fail to delete it and report the whole merge as a failure even
/// though it landed (observed on PRs #154/#155). We drop that flag (see
/// [`pr_merge_args`]) and delete only the remote branch, best-effort, after
/// the merge succeeds; the local branch gets replaced on the workspace's
/// next dispatch.
///
/// `expected_head` pins the merge to the exact commit the probe and
/// ci-watch evaluated. GitHub receives `--match-head-commit <sha>` and
/// refuses server-side if the PR head has moved since, closing the TOCTOU
/// window between "checks were green on X" and the merge.
pub fn pr_merge(project: &Project, pr: u64, expected_head: &str) -> Result<Option<String>> {
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    let pr_str = pr.to_string();
    let strategy_flag = project.merge_strategy().gh_flag();

    let args = pr_merge_args(&pr_str, strategy_flag, expected_head);
    let argv: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = run_in_dir(&host, &wt, &argv)?;
    if !out.status.success() {
        let mut stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        // gh's own message names the mismatched SHAs but not the fix;
        // tell the orchestrator what to do about it.
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "shelbi: merge was pinned to head {expected_head} — if the branch \
             gained commits since the probe, re-run `shelbi zen probe` \
             (and ci-watch) and retry with the new head_sha"
        ));
        return Err(Error::Command {
            cmd: args.join(" "),
            status: out.status.to_string(),
            stderr,
        });
    }

    // gh pr merge doesn't print the merge SHA. Ask gh for it separately,
    // polling — GitHub records the merge commit asynchronously.
    let sha = wait_for_merge_commit_sha(&host, &wt, pr)?;

    // The merge landed (wait_for_merge_commit_sha only returns Ok once the
    // PR reached MERGED). Clean up the remote branch ourselves since we
    // dropped `--delete-branch`. Best-effort: a delete that fails (branch
    // already gone via GitHub's auto-delete-on-merge, a protected branch, a
    // transient blip) must not flip a landed merge into a reported failure.
    delete_remote_head_branch(&host, &wt, &pr_str);

    Ok(sha)
}

/// Best-effort deletion of a merged PR's *remote* head branch. Looks the
/// branch name up with `gh pr view` (so it works from the hub checkout
/// without needing the branch locally) and pushes a delete. Every step is
/// swallowed: this runs only after the merge already succeeded, so a
/// failure here is cosmetic. The local branch is left untouched on purpose
/// — it's still checked out in the finishing workspace's worktree.
fn delete_remote_head_branch(host: &Host, wt: &str, pr_str: &str) {
    let view = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "pr",
            "view",
            pr_str,
            "--json",
            "headRefName",
            "--jq",
            ".headRefName // empty",
        ],
    );
    let head_ref = match view {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        _ => return,
    };
    if head_ref.is_empty() {
        return;
    }
    let _ = run_in_dir(host, wt, &["git", "push", "origin", "--delete", &head_ref]);
}

// ---------------------------------------------------------------------------
// helpers (kept `pub` where they're worth unit-testing on their own)

/// Argv for the `gh pr merge` invocation [`pr_merge`] runs. Pinning to
/// `expected_head` appends `--match-head-commit <sha>`, which makes GitHub
/// itself reject the merge if the PR head is no longer the probed commit —
/// the check-and-merge is atomic on the server, so there is no window for
/// a sneaked-in commit between our poll and the merge.
///
/// Deliberately *no* `--delete-branch`: that flag also deletes the local
/// branch, which fails when the finishing workspace's worktree still has
/// the task branch checked out (the normal case in shelbi's flow) and gh
/// then propagates the cleanup failure as a non-zero exit even though the
/// merge landed. [`pr_merge`] deletes the remote branch itself afterward.
pub fn pr_merge_args(pr: &str, strategy_flag: &str, expected_head: &str) -> Vec<String> {
    let mut args: Vec<String> = ["gh", "pr", "merge", pr, strategy_flag]
        .iter()
        .map(|s| s.to_string())
        .collect();
    args.push("--match-head-commit".to_string());
    args.push(expected_head.to_string());
    args
}

/// Best-effort extraction of the first failing required check from the
/// `gh pr checks` output. Rows are tab-separated:
/// `NAME\tSTATUS\tELAPSED\tURL\t[description]`. Status buckets that count
/// as "failing" mirror gh's own buckets: `fail`, `failure`, `cancel`,
/// `cancelled`, `error`.
pub fn first_failing_check(stdout: &str) -> Option<(String, String)> {
    for line in stdout.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 2 {
            continue;
        }
        let name = cols[0].trim();
        let status = cols[1].trim().to_ascii_lowercase();
        let is_fail = matches!(
            status.as_str(),
            "fail" | "failure" | "cancel" | "cancelled" | "error" | "timed_out"
        );
        if !is_fail {
            continue;
        }
        let summary = cols
            .get(4)
            .or_else(|| cols.get(3))
            .copied()
            .unwrap_or("")
            .trim()
            .to_string();
        return Some((name.to_string(), summary));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_failing_check_picks_the_first_fail() {
        let stdout = "lint\tpass\t1m0s\thttps://example/lint\n\
                      build\tfail\t2m0s\thttps://example/build\tcompilation error\n\
                      test\tpending\t0s\thttps://example/test\t\n";
        let got = first_failing_check(stdout).unwrap();
        assert_eq!(got.0, "build");
        assert_eq!(got.1, "compilation error");
    }

    #[test]
    fn first_failing_check_handles_no_failures() {
        let stdout = "lint\tpass\t1m0s\thttps://example/lint\n\
                      build\tpass\t2m0s\thttps://example/build\n";
        assert!(first_failing_check(stdout).is_none());
    }

    #[test]
    fn first_failing_check_handles_alt_fail_buckets() {
        let stdout = "deploy\tcancelled\t30s\thttps://example/deploy\tjob cancelled\n";
        let got = first_failing_check(stdout).unwrap();
        assert_eq!(got.0, "deploy");
        assert_eq!(got.1, "job cancelled");
    }

    #[test]
    fn ci_verdict_wire_format_is_single_line_three_fields() {
        assert_eq!(CiVerdict::Green.as_line(), "green");
        assert_eq!(CiVerdict::Timeout.as_line(), "timeout");
        let red = CiVerdict::Red {
            check: "build".into(),
            summary: "cargo test\nfailed".into(),
        };
        let line = red.as_line();
        assert!(!line.contains('\n'));
        assert!(line.starts_with("red:build:"));
    }

    #[test]
    fn ci_verdict_red_strips_internal_colons() {
        let red = CiVerdict::Red {
            check: "lint:strict".into(),
            summary: "module: oops".into(),
        };
        let line = red.as_line();
        // The wire format keeps exactly three colon-delimited fields so
        // the orchestrator's prompt can split on `:` without ambiguity.
        assert_eq!(line.matches(':').count(), 2);
        assert!(line.starts_with("red:lint_strict:"));
    }

    #[test]
    fn pr_merge_args_always_pins_to_expected_head() {
        let args = pr_merge_args("42", "--squash", "abc123");
        assert_eq!(
            args,
            vec![
                "gh",
                "pr",
                "merge",
                "42",
                "--squash",
                "--match-head-commit",
                "abc123",
            ]
        );
    }

    #[test]
    fn pr_merge_args_never_deletes_branch() {
        // `--delete-branch` would also delete the local branch, which fails
        // when the finishing workspace's worktree still has it checked out;
        // pr_merge deletes only the remote branch afterward instead.
        let args = pr_merge_args("1", "--squash", "sha");
        assert!(!args.iter().any(|a| a == "--delete-branch"));
    }

    #[test]
    fn detects_no_required_checks_message_in_stdout() {
        // gh's "no required checks" wire text — exact match on the
        // needle is what triggers the fallback to all-reported mode.
        let stdout = "no required checks reported on the 'main' branch\n";
        assert!(is_no_required_checks_message(stdout, ""));
    }

    #[test]
    fn detects_no_required_checks_message_in_stderr() {
        // gh has flipped between stdout and stderr for this message
        // across versions; both surfaces need to trigger the fallback.
        let stderr = "no required checks reported on the 'develop' branch\n";
        assert!(is_no_required_checks_message("", stderr));
    }

    #[test]
    fn does_not_confuse_real_failures_with_no_required_checks() {
        // A real failing required check must not trip the fallback —
        // an unrelated check output that happens to mention "required"
        // shouldn't either.
        let stdout = "build\tfail\t2m0s\thttps://example/build\tcompilation error\n";
        assert!(!is_no_required_checks_message(stdout, ""));
        let stdout = "no checks reported on the 'feature' branch\n"; // gh's "no checks at all" variant
        assert!(!is_no_required_checks_message(stdout, ""));
    }

    #[test]
    fn detects_no_checks_reported_message() {
        // gh's zero-checks-at-all wire text (no `--required`).
        let stdout = "no checks reported on the 'feature' branch\n";
        assert!(is_no_checks_reported_message(stdout, ""));
        assert!(is_no_checks_reported_message("", stdout));
    }

    #[test]
    fn no_required_message_is_not_a_no_checks_message() {
        // The "no *required* checks reported" text (required mode only)
        // must not be mistaken for the zero-checks case — its substring
        // is "required checks reported", never "no checks reported".
        let stdout = "no required checks reported on the 'main' branch\n";
        assert!(!is_no_checks_reported_message(stdout, ""));
    }

    #[test]
    fn classify_merge_state_maps_gh_statuses() {
        assert_eq!(classify_merge_state("CLEAN"), MergeReadiness::Clean);
        assert_eq!(classify_merge_state("clean"), MergeReadiness::Clean);
        assert_eq!(classify_merge_state(" CLEAN \n"), MergeReadiness::Clean);
        assert_eq!(classify_merge_state("UNKNOWN"), MergeReadiness::Pending);
        assert_eq!(classify_merge_state(""), MergeReadiness::Pending);
        for blocked in ["BLOCKED", "DIRTY", "BEHIND", "UNSTABLE", "DRAFT"] {
            assert_eq!(
                classify_merge_state(blocked),
                MergeReadiness::Blocked,
                "{blocked} should be Blocked"
            );
        }
    }

    #[test]
    fn classify_poll_required_mode() {
        // Exit 0 in the strict path is green outright — no merge-state
        // detour.
        assert_eq!(
            classify_poll(WatchMode::Required, 0, "", ""),
            PollOutcome::Green
        );
        // Exit 8 is pending.
        assert_eq!(
            classify_poll(WatchMode::Required, 8, "", ""),
            PollOutcome::Pending
        );
        // "no required checks" flips to the fallback.
        assert_eq!(
            classify_poll(
                WatchMode::Required,
                1,
                "no required checks reported on the 'main' branch\n",
                "",
            ),
            PollOutcome::FlipToAllReported
        );
        // A genuine required-check failure stays red with the check named.
        let red = classify_poll(
            WatchMode::Required,
            1,
            "build\tfail\t2m0s\thttps://example/build\tcompilation error\n",
            "",
        );
        assert_eq!(
            red,
            PollOutcome::Red {
                check: "build".into(),
                summary: "compilation error".into(),
            }
        );
    }

    #[test]
    fn classify_poll_all_reported_mode() {
        // All reported checks passing → confirm merge state, don't go
        // green blind.
        assert_eq!(
            classify_poll(WatchMode::AllReported, 0, "", ""),
            PollOutcome::ConfirmMergeState
        );
        // Exit 8 is still pending.
        assert_eq!(
            classify_poll(WatchMode::AllReported, 8, "", ""),
            PollOutcome::Pending
        );
        // No checks at all → confirm merge state (the zero-checks fix).
        assert_eq!(
            classify_poll(
                WatchMode::AllReported,
                1,
                "no checks reported on the 'docs-only' branch\n",
                "",
            ),
            PollOutcome::ConfirmMergeState
        );
        // A non-required check that actually failed still goes red.
        let red = classify_poll(
            WatchMode::AllReported,
            1,
            "Vercel\tfail\t30s\thttps://example/vercel\tbuild error\n",
            "",
        );
        assert_eq!(
            red,
            PollOutcome::Red {
                check: "Vercel".into(),
                summary: "build error".into(),
            }
        );
    }
}

#[cfg(all(test, unix))]
mod pr_create_tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use shelbi_core::{
        AgentRunnerSpec, GitConfig, HeartbeatConfig, MachineKind, OrchestratorSpec, ZenConfig,
        ZenDangerPaths,
    };

    const TASK_BRANCH: &str = "jlong/reviewed-head";
    const REPLACEMENT_BRANCH: &str = "jlong/replacement-task";
    const PROJECT_NAME: &str = "pr-create-test";

    struct EnvGuard {
        shell: Option<OsString>,
        home: Option<OsString>,
        shelbi_home: Option<OsString>,
    }

    impl EnvGuard {
        fn install(home: &Path) -> Self {
            let guard = Self {
                shell: std::env::var_os("SHELL"),
                home: std::env::var_os("HOME"),
                shelbi_home: std::env::var_os("SHELBI_HOME"),
            };
            std::fs::create_dir_all(home.join("shelbi-home")).unwrap();
            std::env::set_var("SHELL", "/bin/sh");
            std::env::set_var("HOME", home);
            std::env::set_var("SHELBI_HOME", home.join("shelbi-home"));
            guard
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            restore_env("SHELL", self.shell.take());
            restore_env("HOME", self.home.take());
            restore_env("SHELBI_HOME", self.shelbi_home.take());
        }
    }

    fn restore_env(key: &str, value: Option<OsString>) {
        match value {
            Some(value) => std::env::set_var(key, value),
            None => std::env::remove_var(key),
        }
    }

    fn run_git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn git_stdout(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {} failed in {}: {}",
            args.join(" "),
            dir.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Create a real bare origin and a workspace clone at the path Shelbi
    /// resolves for `ws1`. The task branch starts with one reviewed commit;
    /// callers choose whether that initial head is already on origin.
    fn setup_repo(push_task_branch: bool) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let origin = base.path().join("origin.git");
        run_git(
            base.path(),
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );

        let worktree = base.path().join(".shelbi").join("wt").join("ws1");
        std::fs::create_dir_all(worktree.parent().unwrap()).unwrap();
        run_git(
            base.path(),
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                worktree.to_str().unwrap(),
            ],
        );
        run_git(&worktree, &["config", "user.email", "test@example.com"]);
        run_git(&worktree, &["config", "user.name", "Test"]);

        std::fs::write(worktree.join("seed.txt"), "seed\n").unwrap();
        run_git(&worktree, &["add", "seed.txt"]);
        run_git(&worktree, &["commit", "-q", "-m", "seed main"]);
        run_git(&worktree, &["push", "-q", "-u", "origin", "main"]);
        run_git(&worktree, &["checkout", "-q", "-b", TASK_BRANCH]);
        std::fs::write(worktree.join("task.txt"), "initial task work\n").unwrap();
        run_git(&worktree, &["add", "task.txt"]);
        run_git(&worktree, &["commit", "-q", "-m", "initial task work"]);
        if push_task_branch {
            run_git(&worktree, &["push", "-q", "-u", "origin", TASK_BRANCH]);
        }

        (base, origin, worktree)
    }

    fn project(work_dir: &Path) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: Vec::new(),
                prompt_injection: None,
                dialog_signatures: Vec::new(),
            },
        );
        Project {
            name: PROJECT_NAME.into(),
            repo: work_dir.to_string_lossy().into_owned(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: work_dir.to_path_buf(),
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
                name: "ws1".into(),
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
            git: GitConfig::default(),
        }
    }

    fn project_with_probe_check(work_dir: &Path) -> Project {
        let mut project = project(work_dir);
        project.zen.checks.local = vec![
            "test -f task.txt && test -f base-fix.txt && test ! -e replacement.txt && git rev-parse HEAD"
                .into(),
        ];
        project.zen.danger_paths = ZenDangerPaths::Override(vec!["task.txt".into()]);
        project
    }

    fn task() -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "reviewed-head".into(),
            title: "reviewed head".into(),
            column: Column::review(),
            priority: 0,
            assigned_to: Some("ws1".into()),
            workflow: None,
            branch: Some(TASK_BRANCH.into()),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: BTreeMap::new(),
        }
    }

    fn local_head(worktree: &Path) -> String {
        git_stdout(worktree, &["rev-parse", "HEAD"])
    }

    fn task_branch_head(worktree: &Path) -> String {
        git_stdout(
            worktree,
            &["rev-parse", &format!("refs/heads/{TASK_BRANCH}")],
        )
    }

    fn remote_head(origin: &Path) -> String {
        git_stdout(
            origin.parent().unwrap(),
            &[
                "--git-dir",
                origin.to_str().unwrap(),
                "rev-parse",
                &format!("refs/heads/{TASK_BRANCH}"),
            ],
        )
    }

    fn remote_branch_exists(worktree: &Path) -> bool {
        !git_stdout(worktree, &["ls-remote", "--heads", "origin", TASK_BRANCH]).is_empty()
    }

    fn gh_calls(log: &Path) -> String {
        std::fs::read_to_string(log).unwrap_or_default()
    }

    /// Install a stateful `gh` stub in a login-shell PATH. `pr list` exposes
    /// `initial_pr`, `pr create` creates #42, and `pr view` normally reports
    /// the real bare-origin branch tip. `head_override` simulates GitHub still
    /// reporting an obsolete PR head after a push.
    fn install_gh_stub(
        stub_home: &Path,
        origin: &Path,
        initial_pr: Option<u64>,
        head_override: Option<&str>,
    ) -> PathBuf {
        let bin = stub_home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let number_file = stub_home.join("pr-number");
        let override_file = stub_home.join("head-override");
        let log_file = stub_home.join("gh.log");
        if let Some(number) = initial_pr {
            std::fs::write(&number_file, format!("{number}\n")).unwrap();
        }
        if let Some(oid) = head_override {
            std::fs::write(&override_file, format!("{oid}\n")).unwrap();
        }

        let q = shelbi_agent::shell_escape;
        let script = format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> {log}
case "$1 $2" in
  "pr list")
    if [ -f {number} ]; then cat {number}; fi
    ;;
  "pr view")
    if [ -f {head_override} ]; then
      cat {head_override}
    else
      git --git-dir={origin} rev-parse {remote_ref}
    fi
    ;;
  "pr create")
    printf '%s\n' 42 > {number}
    printf '%s\n' https://github.com/example/repo/pull/42
    ;;
  *)
    printf 'unexpected gh invocation: %s\n' "$*" >&2
    exit 64
    ;;
esac
"#,
            log = q(&log_file.to_string_lossy()),
            number = q(&number_file.to_string_lossy()),
            head_override = q(&override_file.to_string_lossy()),
            origin = q(&origin.to_string_lossy()),
            remote_ref = q(&format!("refs/heads/{TASK_BRANCH}")),
        );
        let gh = bin.join("gh");
        std::fs::write(&gh, script).unwrap();
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(
            stub_home.join(".profile"),
            format!("export PATH=\"{}:$PATH\"\n", bin.display()),
        )
        .unwrap();
        log_file
    }

    fn advance_local_head(worktree: &Path) -> String {
        std::fs::write(worktree.join("reviewed.txt"), "reviewed local head\n").unwrap();
        run_git(worktree, &["add", "reviewed.txt"]);
        run_git(worktree, &["commit", "-q", "-m", "reviewed local head"]);
        task_branch_head(worktree)
    }

    /// Model the real lifecycle after handoff: the poller detaches the
    /// finished worktree, then a later dispatch reattaches that same slot to a
    /// different task. The old task branch ref survives in the repository.
    fn reattach_workspace_to_replacement_task(worktree: &Path) -> String {
        let detached = crate::workspace::detach_workspace_worktree(&Host::Local, worktree);
        assert!(
            matches!(detached, crate::workspace::DetachOutcome::Detached { .. }),
            "handoff detachment failed: {detached:?}"
        );
        run_git(
            worktree,
            &["checkout", "-q", "-b", REPLACEMENT_BRANCH, "main"],
        );
        std::fs::write(worktree.join("replacement.txt"), "replacement task\n").unwrap();
        run_git(worktree, &["add", "replacement.txt"]);
        run_git(worktree, &["commit", "-q", "-m", "replacement task work"]);
        local_head(worktree)
    }

    fn advance_origin_main_with_base_fix(base: &Path, origin: &Path) {
        let bump = base.join("advance-main-for-probe");
        run_git(
            base,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                bump.to_str().unwrap(),
            ],
        );
        run_git(&bump, &["config", "user.email", "test@example.com"]);
        run_git(&bump, &["config", "user.name", "Test"]);
        std::fs::write(bump.join("base-fix.txt"), "new default content\n").unwrap();
        run_git(&bump, &["add", "base-fix.txt"]);
        run_git(&bump, &["commit", "-q", "-m", "advance main"]);
        run_git(&bump, &["push", "-q", "origin", "main"]);
    }

    /// Rewrite the reviewed task tip so the local and remote branches become
    /// siblings. A normal push must reject this rather than overwrite the
    /// stale remote PR branch.
    fn rewrite_local_task_head(worktree: &Path) -> String {
        std::fs::write(worktree.join("task.txt"), "rewritten reviewed work\n").unwrap();
        run_git(worktree, &["add", "task.txt"]);
        run_git(worktree, &["commit", "-q", "--amend", "--no-edit"]);
        task_branch_head(worktree)
    }

    #[test]
    fn stale_open_pr_is_updated_and_verified_before_reuse() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(true);
        let stale_head = remote_head(&origin);
        let reviewed_head = advance_local_head(&worktree);
        assert_ne!(stale_head, reviewed_head);

        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, Some(379), None);
        let result = {
            let _env = EnvGuard::install(stub.path());
            pr_create_at_head(
                &project(base.path()),
                PROJECT_NAME,
                &task(),
                "body",
                &reviewed_head,
            )
        };

        assert_eq!(result.unwrap(), 379);
        assert_eq!(
            remote_head(&origin),
            reviewed_head,
            "PR #379's remote branch must advance to the exact reviewed task ref before reuse"
        );
        let calls = gh_calls(&log);
        assert!(calls.contains("pr list"), "{calls}");
        assert!(calls.contains("pr view 379 --json headRefOid"), "{calls}");
        assert!(!calls.contains("pr create"), "{calls}");
    }

    #[test]
    fn reattached_workspace_reuses_old_tasks_exact_reviewed_branch() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(true);
        let reviewed_head = advance_local_head(&worktree);
        // Handoff detaches the finished checkout, then a replacement task is
        // dispatched before either the probe or PR creation runs.
        let replacement_head = reattach_workspace_to_replacement_task(&worktree);
        let project = project(base.path());
        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, Some(379), None);
        let (report, result) = {
            let _env = EnvGuard::install(stub.path());
            let report = probe(&project, &task(), &format!("refs/heads/{TASK_BRANCH}")).unwrap();
            let result =
                pr_create_at_head(&project, PROJECT_NAME, &task(), "body", &report.head_sha);
            (report, result)
        };
        assert_eq!(report.head_sha, reviewed_head);
        assert_ne!(replacement_head, reviewed_head);
        assert_eq!(task_branch_head(&worktree), reviewed_head);

        assert_eq!(result.unwrap(), 379);
        assert_eq!(remote_head(&origin), reviewed_head);
        assert_eq!(task_branch_head(&worktree), reviewed_head);
        assert_eq!(local_head(&worktree), replacement_head);
        assert_eq!(
            git_stdout(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            REPLACEMENT_BRANCH,
            "PR creation must not disturb the task now occupying the workspace"
        );
        let calls = gh_calls(&log);
        assert!(
            calls.contains(&format!("pr list --head {TASK_BRANCH}")),
            "{calls}"
        );
        assert!(calls.contains("pr view 379 --json headRefOid"), "{calls}");
        assert!(!calls.contains("pr create"), "{calls}");
    }

    #[test]
    fn recycled_workspace_probe_rebases_old_task_and_pins_new_pr_to_reported_head() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(false);
        let old_task_head = task_branch_head(&worktree);
        run_git(
            &worktree,
            &["update-ref", "refs/heads/review-sibling", &old_task_head],
        );
        run_git(&worktree, &["config", "rebase.updateRefs", "true"]);
        let replacement_head = reattach_workspace_to_replacement_task(&worktree);

        // Model a replacement worker with staged, unstaged, and untracked
        // state. The old task's probe must leave every byte and index entry
        // alone even though the task still names this workspace assignment.
        std::fs::write(worktree.join("replacement.txt"), "staged replacement\n").unwrap();
        run_git(&worktree, &["add", "replacement.txt"]);
        std::fs::write(worktree.join("replacement.txt"), "unstaged replacement\n").unwrap();
        std::fs::write(worktree.join("replacement-untracked.txt"), "untracked\n").unwrap();
        let replacement_branch_before =
            git_stdout(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let replacement_status_before = git_stdout(
            &worktree,
            &["status", "--porcelain=v1", "--untracked-files=all"],
        );
        let replacement_index_before = git_stdout(&worktree, &["diff", "--cached", "--binary"]);
        let replacement_diff_before = git_stdout(&worktree, &["diff", "--binary"]);
        let replacement_file_before = std::fs::read(worktree.join("replacement.txt")).unwrap();
        let replacement_untracked_before =
            std::fs::read(worktree.join("replacement-untracked.txt")).unwrap();
        assert!(!worktree.join("task.txt").exists());

        let stale_tracking_main = git_stdout(&worktree, &["rev-parse", "origin/main"]);
        advance_origin_main_with_base_fix(base.path(), &origin);
        // A user's fetch refspec need not update origin/main. The probe must
        // use the exact commit fetched into FETCH_HEAD, not a stale tracking
        // ref left behind by the clone.
        run_git(&worktree, &["config", "--unset-all", "remote.origin.fetch"]);
        run_git(
            &worktree,
            &[
                "config",
                "--add",
                "remote.origin.fetch",
                "+refs/heads/unrelated:refs/remotes/origin/unrelated",
            ],
        );
        let project = project_with_probe_check(base.path());
        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, None, None);
        let (report, pr) = {
            let _env = EnvGuard::install(stub.path());
            let report = probe_in_workflow(
                &project,
                None,
                &task(),
                TASK_BRANCH,
                RebasePolicy::RebaseOntoDefault,
            )
            .unwrap();
            let pr = pr_create_at_head(&project, PROJECT_NAME, &task(), "body", &report.head_sha)
                .unwrap();
            (report, pr)
        };

        assert_eq!(pr, 42);
        assert_ne!(report.head_sha, old_task_head, "the stale task must rebase");
        assert_eq!(report.head_sha, task_branch_head(&worktree));
        assert_eq!(report.head_sha, remote_head(&origin));
        assert_eq!(
            git_stdout(&worktree, &["rev-parse", "origin/main"]),
            stale_tracking_main,
            "the regression requires origin/main to remain stale"
        );
        assert_eq!(
            git_stdout(&worktree, &["rev-parse", "refs/heads/review-sibling"]),
            old_task_head,
            "--no-update-refs must override user config until Shelbi CAS-updates the task ref"
        );
        assert!(!report.rebase_conflict.conflicts);
        assert!(!report.merge_conflict.conflicts);
        assert_eq!(
            report.diff_size,
            DiffSize {
                files: 1,
                lines_added: 1,
                lines_removed: 0,
            }
        );
        assert_eq!(report.danger_paths.matched, vec!["task.txt"]);
        assert_eq!(report.local_checks.len(), 1);
        assert_eq!(report.local_checks[0].exit_code, 0);
        assert_eq!(report.local_checks[0].output_tail, report.head_sha);

        assert_eq!(local_head(&worktree), replacement_head);
        assert_eq!(
            git_stdout(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            replacement_branch_before
        );
        assert_eq!(
            git_stdout(
                &worktree,
                &["status", "--porcelain=v1", "--untracked-files=all"]
            ),
            replacement_status_before
        );
        assert_eq!(
            git_stdout(&worktree, &["diff", "--cached", "--binary"]),
            replacement_index_before
        );
        assert_eq!(
            git_stdout(&worktree, &["diff", "--binary"]),
            replacement_diff_before
        );
        assert_eq!(
            std::fs::read(worktree.join("replacement.txt")).unwrap(),
            replacement_file_before
        );
        assert_eq!(
            std::fs::read(worktree.join("replacement-untracked.txt")).unwrap(),
            replacement_untracked_before
        );
        assert!(!worktree.join("task.txt").exists());
        assert!(!worktree.join("base-fix.txt").exists());

        let worktree_list = git_stdout(&worktree, &["worktree", "list", "--porcelain"]);
        assert_eq!(
            worktree_list
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1,
            "the isolated probe worktree must be removed:\n{worktree_list}"
        );
        let calls = gh_calls(&log);
        assert!(calls.contains("pr create --head"), "{calls}");
        assert!(calls.contains("pr view 42 --json headRefOid"), "{calls}");
    }

    #[test]
    fn current_open_pr_is_verified_and_reused_at_the_probed_head() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(true);
        // Finished workspaces are normally detached at handoff. The branch
        // ref and HEAD still name the same commit, so reuse must work without
        // requiring an attached checkout.
        run_git(&worktree, &["checkout", "-q", "--detach"]);
        let reviewed_head = task_branch_head(&worktree);

        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, Some(21), None);
        let result = {
            let _env = EnvGuard::install(stub.path());
            pr_create_at_head(
                &project(base.path()),
                PROJECT_NAME,
                &task(),
                "body",
                &reviewed_head,
            )
        };

        assert_eq!(result.unwrap(), 21);
        assert_eq!(remote_head(&origin), reviewed_head);
        let calls = gh_calls(&log);
        assert!(calls.contains("pr view 21 --json headRefOid"), "{calls}");
        assert!(!calls.contains("pr create"), "{calls}");
    }

    #[test]
    fn new_pr_uses_reviewed_branch_when_workspace_has_been_reattached() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(false);
        let reviewed_head = task_branch_head(&worktree);
        let replacement_head = reattach_workspace_to_replacement_task(&worktree);

        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, None, None);
        let result = {
            let _env = EnvGuard::install(stub.path());
            pr_create_at_head(
                &project(base.path()),
                PROJECT_NAME,
                &task(),
                "body",
                &reviewed_head,
            )
        };

        assert_eq!(result.unwrap(), 42);
        assert_eq!(remote_head(&origin), reviewed_head);
        assert_eq!(local_head(&worktree), replacement_head);
        let calls = gh_calls(&log);
        assert!(calls.contains("pr create --head"), "{calls}");
        assert!(calls.contains("--title initial task work"), "{calls}");
        assert!(!calls.contains("--title replacement task work"), "{calls}");
        assert!(calls.contains("pr view 42 --json headRefOid"), "{calls}");
    }

    #[test]
    fn task_branch_moving_after_probe_is_rejected_before_push() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(false);
        let probed_head = task_branch_head(&worktree);
        let moved_head = advance_local_head(&worktree);

        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, Some(379), None);
        let result = {
            let _env = EnvGuard::install(stub.path());
            pr_create_at_head(
                &project(base.path()),
                PROJECT_NAME,
                &task(),
                "body",
                &probed_head,
            )
        };

        let err = result.unwrap_err().to_string();
        assert!(err.contains("moved since it was probed"), "{err}");
        assert!(err.contains(&probed_head), "{err}");
        assert!(err.contains(&moved_head), "{err}");
        assert!(!remote_branch_exists(&worktree));
        assert!(gh_calls(&log).is_empty());
    }

    #[test]
    fn rewritten_task_branch_is_rejected_without_reusing_stale_pr() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(true);
        let stale_remote_head = remote_head(&origin);
        let reviewed_head = rewrite_local_task_head(&worktree);
        assert_ne!(stale_remote_head, reviewed_head);

        let stub = tempfile::tempdir().unwrap();
        let log = install_gh_stub(stub.path(), &origin, Some(379), None);
        let result = {
            let _env = EnvGuard::install(stub.path());
            pr_create_at_head(
                &project(base.path()),
                PROJECT_NAME,
                &task(),
                "body",
                &reviewed_head,
            )
        };

        let err = result.unwrap_err().to_string();
        assert!(err.contains(TASK_BRANCH), "{err}");
        assert!(err.contains(&reviewed_head), "{err}");
        assert!(err.contains("rewritten or diverged"), "{err}");
        assert_eq!(remote_head(&origin), stale_remote_head);
        assert_eq!(task_branch_head(&worktree), reviewed_head);
        assert!(
            gh_calls(&log).is_empty(),
            "a rejected push must not allow stale PR #379 into the workflow"
        );
    }

    #[test]
    fn pr_head_mismatch_is_rejected_without_returning_stale_number() {
        let _lock = crate::test_lock::acquire();
        let (base, origin, worktree) = setup_repo(true);
        let stale_head = remote_head(&origin);
        let reviewed_head = advance_local_head(&worktree);

        let stub = tempfile::tempdir().unwrap();
        install_gh_stub(stub.path(), &origin, Some(379), Some(&stale_head));
        let result = {
            let _env = EnvGuard::install(stub.path());
            pr_create_at_head(
                &project(base.path()),
                PROJECT_NAME,
                &task(),
                "body",
                &reviewed_head,
            )
        };

        let err = result.unwrap_err().to_string();
        assert!(err.contains("PR #379"), "{err}");
        assert!(err.contains(&stale_head), "{err}");
        assert!(err.contains(&reviewed_head), "{err}");
        assert!(err.contains("refusing to report"), "{err}");
        assert_eq!(
            remote_head(&origin),
            reviewed_head,
            "the safe push may succeed, but a stale GitHub head must still block reuse"
        );
    }
}

// ===========================================================================
// Probe primitives — local checks, conflict, diff size, danger paths
// ===========================================================================

// ---------------------------------------------------------------------------
// Public types

/// Result of one configured shell command, captured verbatim with no
/// pass/fail interpretation. The prompt decides whether a non-zero exit
/// blocks the merge.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LocalCheck {
    /// The command line as configured in `zen.checks.local` (or its
    /// per-task override). We pass it to `sh -c` verbatim; quoting and
    /// shell metacharacters are the project author's responsibility.
    pub command: String,
    /// Process exit code. `-1` when the runner couldn't return one (e.g.
    /// the process was signalled or `sh` itself failed to launch).
    pub exit_code: i32,
    /// Wall-clock duration in milliseconds.
    pub duration_ms: u64,
    /// Last ~40 lines of combined stdout+stderr. Bounded so probe reports
    /// stay small enough to round-trip through the orchestrator prompt.
    pub output_tail: String,
}

/// Result of a no-touch test merge of `branch` into the project's default
/// branch. Reports whether conflicts would occur and which files are
/// involved; the merge is always aborted (the worktree is read-only from
/// the caller's perspective).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct ConflictProbe {
    pub conflicts: bool,
    pub files: Vec<String>,
}

/// `git diff --shortstat` decomposed into machine-readable fields.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DiffSize {
    pub files: usize,
    pub lines_added: usize,
    pub lines_removed: usize,
}

/// Subset of the branch's changed files that match one of the project's
/// configured danger globs (built-ins plus extends / override per
/// [`shelbi_core::danger_paths_for_project`]).
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct DangerPaths {
    pub matched: Vec<String>,
}

/// The full probe report for one branch. Each field is independent; no
/// field gates the rest.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ProbeReport {
    /// The branch tip every fact in this report was computed against,
    /// captured *after* the optional rebase. The orchestrator hands it
    /// back to both `shelbi zen pr-create --match-head-commit <sha>` and
    /// `shelbi zen pr-merge --match-head-commit <sha>` so the published PR
    /// and eventual merge are pinned to exactly the commit that was probed.
    /// A branch move after the probe makes either operation refuse instead of
    /// publishing or landing unchecked content.
    pub head_sha: String,
    pub local_checks: Vec<LocalCheck>,
    pub merge_conflict: ConflictProbe,
    /// Outcome of rebasing the branch onto the *current* default before the
    /// probe ran. Populated only under [`RebasePolicy::RebaseOntoDefault`];
    /// always clean under `AsIs`. When `conflicts` is true the rebase was
    /// aborted (the durable task ref stays at its pre-rebase commit) and the
    /// local checks were skipped — `files` names the conflicting paths.
    pub rebase_conflict: ConflictProbe,
    pub diff_size: DiffSize,
    pub danger_paths: DangerPaths,
}

/// Whether [`probe_in_workflow`] rebases the branch onto the current
/// default before gathering facts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebasePolicy {
    /// Fetch the project's default branch and rebase the named task branch
    /// onto it in an isolated worktree before probing, so every fact reflects
    /// the default as it stands *now* — not as it stood at handoff. A clean
    /// rebase advances the durable task ref only after the isolated checks
    /// and facts all describe the same new commit.
    RebaseOntoDefault,
    /// Probe an isolated checkout of the named task ref with no fetch or ref
    /// rewrite. Used by the read-only dry-run preview and the legacy
    /// [`probe`] entry point.
    AsIs,
}

// ---------------------------------------------------------------------------
// Entry point

/// Run every primitive for `task` on `branch` and return the report.
///
/// Resolves the workspace's repository (and machine) from `task.assigned_to`,
/// then checks out the named task ref in a temporary worktree. The assigned
/// workspace's current branch and files are never used as probe input. This
/// matters after handoff, when that workspace may already serve another task,
/// and for remote workspaces, where the unpushed task ref exists only on the
/// worker machine.
///
/// Legacy entry point (workflow-unaware) — calls
/// [`probe_in_workflow`] with `workflow = None`. New callers should
/// reach for the workflow-aware form so per-workflow `zen:` overrides
/// (checks, danger_paths) take effect.
pub fn probe(project: &Project, task: &Task, branch: &str) -> Result<ProbeReport> {
    probe_in_workflow(project, None, task, branch, RebasePolicy::AsIs)
}

/// Run every primitive for `task` on `branch`, threading `workflow`
/// through the per-workflow zen resolution helpers.
///
/// When `workflow` is `Some`, that workflow's `zen.checks` and
/// `zen.danger_paths` shadow the project-level defaults (see
/// [`shelbi_core::checks_for_task_in_workflow`] and
/// [`shelbi_core::danger_paths_for_workflow`] for the exact rules).
/// `None` matches legacy [`probe`] behavior — useful for callers that
/// haven't migrated to workflow-aware lookups yet.
pub fn probe_in_workflow(
    project: &Project,
    workflow: Option<&Workflow>,
    task: &Task,
    branch: &str,
    policy: RebasePolicy,
) -> Result<ProbeReport> {
    let (machine, workspace) = resolve_workspace(project, task)?;
    // Keep the assigned slot stable while the probe copies its ignored
    // installed dependencies. This lock cannot coordinate a checkout in a
    // different slot; finalizing the named ref uses the project-wide Git
    // worktree/ref lock below for that purpose.
    let _workspace_lock = shelbi_state::lock_workspace(&project.name, &workspace.name)?;
    let host = machine.host();
    let repository_anchor = workspace_worktree(&machine, workspace);
    let shared_cargo_target = machine.work_dir.join("target");
    let branch = normalize_probe_branch(branch)?;
    let task_ref = format!("refs/heads/{branch}");
    let initial_head =
        probe_head_sha(&host, &repository_anchor, &format!("{task_ref}^{{commit}}"))?;

    // Handoff detaches a finished task and immediately makes its workspace
    // reusable. The assigned path may therefore be checked out on a wholly
    // different task by the time Zen probes the old review. Use it only as a
    // repository anchor: every mutable and content-sensitive operation runs
    // in a throwaway detached worktree at the exact named-ref snapshot.
    let probe_worktree = unique_probe_worktree_path(&repository_anchor, &task.id);
    add_probe_worktree(&host, &repository_anchor, &probe_worktree, &initial_head)?;

    let probe_result = probe_isolated_task_ref(
        project,
        workflow,
        task,
        &host,
        &repository_anchor,
        &shared_cargo_target,
        &probe_worktree,
        &branch,
        &task_ref,
        &initial_head,
        policy,
    );
    let cleanup_result = remove_probe_worktree(&host, &repository_anchor, &probe_worktree);

    match (probe_result, cleanup_result) {
        (Err(probe_err), Err(cleanup_err)) => Err(Error::Other(format!(
            "{probe_err}; additionally failed to remove the isolated probe worktree: \
             {cleanup_err}"
        ))),
        (Err(probe_err), Ok(())) => Err(probe_err),
        (Ok(_), Err(cleanup_err)) => Err(cleanup_err),
        (Ok(report), Ok(())) => Ok(report),
    }
}

#[allow(clippy::too_many_arguments)]
fn probe_isolated_task_ref(
    project: &Project,
    workflow: Option<&Workflow>,
    task: &Task,
    host: &Host,
    repository_anchor: &std::path::Path,
    shared_cargo_target: &std::path::Path,
    probe_worktree: &std::path::Path,
    branch: &str,
    task_ref: &str,
    initial_head: &str,
    policy: RebasePolicy,
) -> Result<ProbeReport> {
    // Freeze the base OID too. A concurrent fetch must not make rebase and
    // diff inspection silently describe different versions of the default
    // branch within one report.
    let default_branch = project.base_branch();
    let base_ref = match policy {
        RebasePolicy::AsIs => default_branch.to_string(),
        RebasePolicy::RebaseOntoDefault => fetch_probe_base(host, probe_worktree, default_branch),
    };
    let base_sha = probe_head_sha(host, probe_worktree, &format!("{base_ref}^{{commit}}"))?;

    let rebase_conflict = match policy {
        RebasePolicy::AsIs => ConflictProbe::default(),
        RebasePolicy::RebaseOntoDefault => {
            match rebase_workspace_branch_onto_default(host, probe_worktree, &base_sha) {
                RebaseOutcome::AlreadyUpToDate { .. } | RebaseOutcome::Rebased { .. } => {
                    ConflictProbe::default()
                }
                RebaseOutcome::Conflict { files, .. } => ConflictProbe {
                    conflicts: true,
                    files,
                },
                RebaseOutcome::Skipped { reason } => {
                    return Err(Error::Other(format!(
                        "could not rebase task branch `{branch}` in its isolated Zen probe: \
                         {reason}; refusing to report unverified probe results"
                    )));
                }
            }
        }
    };

    // The detached temp worktree is the sole content authority for this
    // report. A clean rebase changes only its HEAD for now; the durable named
    // ref is advanced with an expected-old compare-and-swap after every fact
    // and local check has successfully inspected this exact commit.
    let head_sha = probe_head_sha(host, probe_worktree, "HEAD^{commit}")?;
    if rebase_conflict.conflicts && head_sha != initial_head {
        return Err(Error::Other(format!(
            "the conflicted rebase for task branch `{branch}` did not restore its isolated \
             checkout from {head_sha} to the original commit {initial_head}; refusing to \
             advance or report the task branch"
        )));
    }
    let merge_conflict = probe_merge_conflict(host, probe_worktree, &head_sha, &base_sha)?;
    let diff_size = probe_diff_size(host, probe_worktree, &head_sha, &base_sha)?;
    let danger_paths = probe_danger_paths(
        project,
        workflow,
        host,
        probe_worktree,
        &head_sha,
        &base_sha,
    )?;
    // A rebase conflict was aborted back to the pre-probe commit. Testing
    // that stale content would be misleading, so the conflict itself blocks
    // the merge and local checks stay empty.
    let local_checks = if rebase_conflict.conflicts {
        Vec::new()
    } else {
        probe_local_checks(
            host,
            repository_anchor,
            probe_worktree,
            shared_cargo_target,
            &head_sha,
            project,
            workflow,
            task,
        )?
    };

    let final_temp_head = probe_head_sha(host, probe_worktree, "HEAD^{commit}")?;
    if final_temp_head != head_sha {
        return Err(Error::Other(format!(
            "a local Zen check moved the isolated task checkout from {head_sha} to \
             {final_temp_head}; refusing to report results for a different commit"
        )));
    }

    finalize_probed_task_ref(
        &project.name,
        host,
        repository_anchor,
        branch,
        task_ref,
        initial_head,
        &head_sha,
    )?;

    Ok(ProbeReport {
        head_sha,
        local_checks,
        merge_conflict,
        rebase_conflict,
        diff_size,
        danger_paths,
    })
}

fn normalize_probe_branch(branch: &str) -> Result<String> {
    let branch = branch.strip_prefix("refs/heads/").unwrap_or(branch);
    shelbi_core::validate_branch(branch)
        .map_err(|e| Error::Other(format!("cannot probe task branch `{branch}`: {e}")))?;
    Ok(branch.to_string())
}

fn unique_probe_worktree_path(repository_anchor: &std::path::Path, task_id: &str) -> PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Task ids can approach GitHub's 255-byte ref limit. Keep the diagnostic
    // fragment short so the generated path component stays below NAME_MAX;
    // pid + process-local sequence provide uniqueness.
    let safe_id: String = task_id
        .chars()
        .take(48)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    repository_anchor
        .parent()
        .unwrap_or(repository_anchor)
        .join(format!(
            ".shelbi-probe-{}-{safe_id}-{seq}",
            std::process::id()
        ))
}

fn add_probe_worktree(
    host: &Host,
    repository_anchor: &std::path::Path,
    probe_worktree: &std::path::Path,
    head_sha: &str,
) -> Result<()> {
    let anchor = repository_anchor.to_string_lossy().into_owned();
    let probe = probe_worktree.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(
        host,
        [
            "git", "-C", &anchor, "worktree", "add", "--detach", &probe, head_sha,
        ],
    )
    .map_err(Error::Io)?;
    if !out.status.success() {
        // Checkout hooks and smudge filters can fail after git has already
        // registered and populated the worktree. Do not leave that partial
        // probe behind just because `worktree add` itself returned non-zero.
        let _ = remove_probe_worktree(host, repository_anchor, probe_worktree);
        return Err(Error::Command {
            cmd: format!("git -C {anchor} worktree add --detach {probe} {head_sha}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

fn remove_probe_worktree(
    host: &Host,
    repository_anchor: &std::path::Path,
    probe_worktree: &std::path::Path,
) -> Result<()> {
    let anchor = repository_anchor.to_string_lossy().into_owned();
    let probe = probe_worktree.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(
        host,
        [
            "git", "-C", &anchor, "worktree", "remove", "--force", &probe,
        ],
    )
    .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {anchor} worktree remove --force {probe}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

fn finalize_probed_task_ref(
    project_name: &str,
    host: &Host,
    repository_anchor: &std::path::Path,
    branch: &str,
    task_ref: &str,
    expected_head: &str,
    probed_head: &str,
) -> Result<()> {
    finalize_probed_task_ref_after_scan(
        project_name,
        host,
        repository_anchor,
        branch,
        task_ref,
        expected_head,
        probed_head,
        || {},
    )
}

#[allow(clippy::too_many_arguments)]
fn finalize_probed_task_ref_after_scan<F: FnOnce()>(
    project_name: &str,
    host: &Host,
    repository_anchor: &std::path::Path,
    branch: &str,
    task_ref: &str,
    expected_head: &str,
    probed_head: &str,
    after_scan: F,
) -> Result<()> {
    // Lock order is workspace -> Git worktrees/refs: probe_in_workflow still
    // holds the assigned workspace lock. Dispatch, resume, pane recovery, and
    // legacy spawn all take this same inner lock before attaching a named ref.
    // Keep it through final verification so scan + CAS + pin confirmation are
    // one critical section.
    let _git_worktree_lock = shelbi_state::lock_git_worktrees(project_name)?;
    let anchor = repository_anchor.to_string_lossy().into_owned();
    if expected_head != probed_head {
        let checked_out = task_ref_checkout_paths(host, repository_anchor, task_ref)?;
        if !checked_out.is_empty() {
            return Err(Error::Other(format!(
                "task branch `{branch}` is still checked out in {}; refusing to advance it from \
                 an isolated Zen probe because that would disturb another worktree",
                checked_out.join(", ")
            )));
        }

        // Test hook for the exact reviewed race: a second named checkout is
        // started after this empty scan and must remain blocked until the CAS
        // and verification finish under the lock.
        after_scan();

        let out = shelbi_ssh::run(
            host,
            [
                "git",
                "-C",
                &anchor,
                "update-ref",
                task_ref,
                probed_head,
                expected_head,
            ],
        )
        .map_err(Error::Io)?;
        if !out.status.success() {
            let current =
                probe_head_sha(host, repository_anchor, &format!("{task_ref}^{{commit}}"))
                    .unwrap_or_else(|_| "<missing>".to_string());
            if current != expected_head {
                return Err(Error::Other(format!(
                    "task branch `{branch}` moved from {expected_head} to {current} while its \
                     isolated Zen probe was running; refusing to replace the concurrent update \
                     with probed commit {probed_head}"
                )));
            }
            return Err(Error::Command {
                cmd: format!("git -C {anchor} update-ref {task_ref} {probed_head} {expected_head}"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    verify_task_branch_head(
        host,
        repository_anchor,
        branch,
        probed_head,
        "while its isolated Zen probe was finishing",
    )
}

fn task_ref_checkout_paths(
    host: &Host,
    repository_anchor: &std::path::Path,
    task_ref: &str,
) -> Result<Vec<String>> {
    let anchor = repository_anchor.to_string_lossy().into_owned();
    let porcelain = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &anchor, "worktree", "list", "--porcelain"],
    )?;
    let mut paths = Vec::new();
    let mut current_path: Option<&str> = None;
    for line in porcelain.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_path = Some(path);
        } else if line.strip_prefix("branch ") == Some(task_ref) {
            if let Some(path) = current_path {
                paths.push(path.to_string());
            }
        } else if line.is_empty() {
            current_path = None;
        }
    }
    Ok(paths)
}

/// Resolve `branch`'s tip SHA in the workspace worktree. Read-only —
/// safe under both rebase policies.
fn probe_head_sha(host: &Host, worktree: &std::path::Path, branch: &str) -> Result<String> {
    let wt = worktree.to_string_lossy().into_owned();
    let stdout = shelbi_ssh::run_capture(host, ["git", "-C", wt.as_str(), "rev-parse", branch])?;
    Ok(stdout.trim().to_string())
}

/// Fetch `base` from `origin` into the workspace worktree and return the
/// ref the probe should rebase onto and compare against.
///
/// On a successful fetch this returns `FETCH_HEAD`, which names the exact
/// commit Git just received even when a custom fetch refspec does not update
/// `origin/<base>`. The caller immediately freezes it to an OID. A fetch
/// failure — no `origin` remote (local-only project), an offline host, or an
/// unknown branch name — is non-fatal: we fall back to the local `<base>` ref
/// the repository already has. This is the probe's only network call.
fn fetch_probe_base(host: &Host, worktree: &std::path::Path, base: &str) -> String {
    let wt = worktree.to_string_lossy().into_owned();
    let remote_ref = format!("refs/heads/{base}");
    match shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            wt.as_str(),
            "fetch",
            "origin",
            "--",
            remote_ref.as_str(),
        ],
    ) {
        Ok(o) if o.status.success() => "FETCH_HEAD".to_string(),
        _ => base.to_string(),
    }
}

fn resolve_workspace<'a>(
    project: &'a Project,
    task: &Task,
) -> Result<(Machine, &'a WorkspaceSpec)> {
    let workspace_name = task.assigned_to.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no assigned workspace — assign one before probing",
            task.id
        ))
    })?;
    let workspace = project.workspace(workspace_name).ok_or_else(|| {
        Error::Other(format!(
            "workspace `{}` (assigned to task `{}`) is not declared in project `{}`",
            workspace_name, task.id, project.name
        ))
    })?;
    let machine = project
        .machine(&workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(workspace.machine.clone()))?
        .clone();
    Ok((machine, workspace))
}

// ---------------------------------------------------------------------------
// local_checks

#[allow(clippy::too_many_arguments)]
fn probe_local_checks(
    host: &Host,
    repository_anchor: &std::path::Path,
    worktree: &std::path::Path,
    shared_cargo_target: &std::path::Path,
    expected_head: &str,
    project: &Project,
    workflow: Option<&Workflow>,
    task: &Task,
) -> Result<Vec<LocalCheck>> {
    let commands = checks_for_task_in_workflow(project, workflow, task);
    if commands.is_empty() {
        return Ok(Vec::new());
    }

    ensure_worktree_present(host, worktree)?;
    bootstrap_isolated_node_dependencies(host, repository_anchor, worktree)?;

    // Best-effort log file on the hub. A failure here just means the log
    // is missing; it doesn't block the probe.
    let log_path = log_file_path(&task.id).ok();
    if let Some(p) = &log_path {
        let _ = init_log(p, &task.id, commands.len());
    }

    let mut out = Vec::with_capacity(commands.len());
    for cmd in commands {
        let mut res =
            run_one_check_with_shared_cargo_target(host, worktree, &cmd, Some(shared_cargo_target));
        let checkout_changed = verify_check_checkout(host, worktree, expected_head, &mut res)?;
        if let Some(p) = &log_path {
            let _ = append_log(p, &res);
        }
        out.push(res);
        if checkout_changed {
            // Even ignored build artifacts may now have been derived from
            // changed source. Do not let a later check observe them.
            break;
        }
    }
    Ok(out)
}

/// Keep every command in a multi-check probe pinned to the same commit. Build
/// artifacts in ignored paths may survive for later checks, but changes to
/// tracked/index content, a moved HEAD, or new non-ignored source files make
/// the check fail and stop the sequence before another command can observe
/// state derived from those changes.
fn verify_check_checkout(
    host: &Host,
    worktree: &std::path::Path,
    expected_head: &str,
    check: &mut LocalCheck,
) -> Result<bool> {
    let wt = worktree.to_string_lossy().into_owned();
    let actual_head = probe_head_sha(host, worktree, "HEAD^{commit}")?;
    let status = shelbi_ssh::run_capture(
        host,
        [
            "git",
            "-C",
            wt.as_str(),
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
        ],
    )?;
    if actual_head == expected_head && status.trim().is_empty() {
        return Ok(false);
    }

    if check.exit_code == 0 {
        check.exit_code = 1;
    }
    let mut detail = Vec::new();
    if actual_head != expected_head {
        detail.push(format!("HEAD moved to {actual_head}"));
    }
    if !status.trim().is_empty() {
        detail.push(format!("working tree changed: {}", status.trim()));
    }
    let combined = format!(
        "{}\nshelbi: local check changed the isolated checkout ({}) - remaining local checks \
         were skipped so no result can drift from probed commit {expected_head}",
        check.output_tail.trim_end(),
        detail.join("; ")
    );
    check.output_tail = tail_lines(&combined, OUTPUT_TAIL_LINES);
    Ok(true)
}

/// Populate dependency directories that Git intentionally omits from a fresh
/// worktree. Package roots come from the exact reviewed commit, while the
/// installed `node_modules` source comes from the persistent repository
/// workspace on the same machine. Each tree is copied independently into the
/// isolated checkout. Symlinks are preserved only when their fully resolved
/// target stays inside that checkout, retaining package-manager layout without
/// leaving a write-through path into the replacement task or an external store.
///
/// Copy-on-write is preferred on macOS (`cp -cR`) and reflink-capable GNU
/// systems (`cp -a --reflink=auto`). The portable recursive fallback favors
/// correctness over speed when neither optimization exists.
fn bootstrap_isolated_node_dependencies(
    host: &Host,
    repository_anchor: &std::path::Path,
    probe_worktree: &std::path::Path,
) -> Result<()> {
    let probe = probe_worktree.to_string_lossy().into_owned();
    let mut dependency_roots = Vec::new();
    let package_paths = shelbi_ssh::run_capture(
        host,
        [
            "git",
            "-C",
            probe.as_str(),
            "ls-files",
            "-z",
            "--",
            "package.json",
            ":(glob)**/package.json",
        ],
    )?;

    for raw in package_paths.split_terminator('\0') {
        let package_path = std::path::Path::new(raw);
        if package_path.file_name().and_then(|name| name.to_str()) != Some("package.json")
            || !package_path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(Error::Other(format!(
                "refusing to import installed dependencies for unsafe package path `{raw}`"
            )));
        }

        let package_dir = package_path
            .parent()
            .unwrap_or_else(|| std::path::Path::new(""));
        let relative_dependencies = package_dir.join("node_modules");
        let source = repository_anchor.join(&relative_dependencies);
        let destination = probe_worktree.join(&relative_dependencies);
        let source_text = source.to_string_lossy().into_owned();
        let destination_text = destination.to_string_lossy().into_owned();

        let destination_exists =
            shelbi_ssh::run(host, ["test", "-e", destination_text.as_str()]).map_err(Error::Io)?;
        let destination_is_link =
            shelbi_ssh::run(host, ["test", "-L", destination_text.as_str()]).map_err(Error::Io)?;
        if destination_exists.status.success() || destination_is_link.status.success() {
            // A project that deliberately tracks node_modules already has
            // the reviewed content in the isolated checkout. Never overlay
            // persistent ignored state on top of it, but still validate its
            // links against the same no-escape rule.
            dependency_roots.push(destination);
            continue;
        }

        let source_exists =
            shelbi_ssh::run(host, ["test", "-d", source_text.as_str()]).map_err(Error::Io)?;
        if !source_exists.status.success() {
            continue;
        }

        copy_dependency_tree(host, &source, &destination)?;
        dependency_roots.push(destination);
    }
    validate_isolated_dependency_links(host, probe_worktree, &dependency_roots)
}

fn copy_dependency_tree(
    host: &Host,
    source: &std::path::Path,
    destination: &std::path::Path,
) -> Result<()> {
    // Appending `/.` follows a top-level node_modules symlink, if present,
    // while recursive copy semantics retain internal links such as .bin
    // entries. The caller validates every copied link before checks run.
    let source_contents = format!("{}/.", source.to_string_lossy());
    let destination_text = destination.to_string_lossy().into_owned();
    let attempts = [
        vec![
            "cp",
            "-cR",
            source_contents.as_str(),
            destination_text.as_str(),
        ],
        vec![
            "cp",
            "-a",
            "--reflink=auto",
            source_contents.as_str(),
            destination_text.as_str(),
        ],
        vec![
            "cp",
            "-R",
            source_contents.as_str(),
            destination_text.as_str(),
        ],
    ];
    let mut diagnostics = Vec::new();

    for args in attempts {
        let out = shelbi_ssh::run(host, args.iter().copied()).map_err(Error::Io)?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        if !stderr.is_empty() {
            diagnostics.push(stderr);
        }

        // An unsupported option normally fails before creating anything, but
        // clean a possible partial destination before trying the next copy
        // strategy. The path is generated beneath the isolated worktree and
        // always ends in a validated package-root `node_modules` component.
        let cleanup =
            shelbi_ssh::run(host, ["rm", "-rf", destination_text.as_str()]).map_err(Error::Io)?;
        if !cleanup.status.success() {
            return Err(Error::Command {
                cmd: format!("rm -rf {destination_text}"),
                status: cleanup.status.to_string(),
                stderr: String::from_utf8_lossy(&cleanup.stderr).into_owned(),
            });
        }
    }

    Err(Error::Other(format!(
        "could not copy installed dependencies from {} into the isolated Zen probe at {}{}",
        source.display(),
        destination.display(),
        if diagnostics.is_empty() {
            String::new()
        } else {
            format!(": {}", diagnostics.join("; "))
        }
    )))
}

fn validate_isolated_dependency_links(
    host: &Host,
    probe_worktree: &std::path::Path,
    dependency_roots: &[PathBuf],
) -> Result<()> {
    if dependency_roots.is_empty() {
        return Ok(());
    }

    // One remote round trip collects path/target NUL-delimited pairs. `find`
    // does not follow links, so a hostile cycle cannot make the scan recurse
    // outside the disposable worktree.
    let probe = probe_worktree.to_string_lossy().into_owned();
    let scan_script = r#"find "$1" -type l -exec sh -c '
for link do
    printf "%s\\000" "$link" || exit 1
    readlink "$link" || exit 1
    printf "\\000" || exit 1
done
' sh {} +"#;
    let out = shelbi_ssh::run(host, ["sh", "-c", scan_script, "sh", probe.as_str()])
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("find {probe} -type l -exec readlink"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    if stdout.contains('\u{fffd}') {
        return Err(Error::Other(
            "refusing to validate installed dependency links with non-UTF-8 paths".into(),
        ));
    }
    let fields: Vec<&str> = stdout.split_terminator('\0').collect();
    if fields.len() % 2 != 0 {
        return Err(Error::Other(
            "installed dependency link scan returned an incomplete path/target pair".into(),
        ));
    }

    let probe_root = normalize_absolute_path(probe_worktree).ok_or_else(|| {
        Error::Other(format!(
            "isolated Zen probe path `{}` is not a safe absolute path",
            probe_worktree.display()
        ))
    })?;
    let dependency_roots: Vec<PathBuf> = dependency_roots
        .iter()
        .map(|path| {
            normalize_absolute_path(path).ok_or_else(|| {
                Error::Other(format!(
                    "isolated dependency path `{}` is not a safe absolute path",
                    path.display()
                ))
            })
        })
        .collect::<Result<_>>()?;
    let mut links = std::collections::HashMap::new();
    for pair in fields.chunks_exact(2) {
        let path = normalize_absolute_path(std::path::Path::new(pair[0])).ok_or_else(|| {
            Error::Other(format!(
                "installed dependency link `{}` is not a safe absolute path",
                pair[0]
            ))
        })?;
        let target = pair[1].strip_suffix('\n').unwrap_or(pair[1]);
        links.insert(path, PathBuf::from(target));
    }

    for link in links
        .keys()
        .filter(|link| dependency_roots.iter().any(|root| link.starts_with(root)))
    {
        let resolved = resolve_link_path(link, &links).ok_or_else(|| {
            Error::Other(format!(
                "installed dependency link `{}` has a cyclic or unsafe target; refusing to run \
                 isolated Zen checks",
                link.display()
            ))
        })?;
        if !resolved.starts_with(&probe_root) {
            return Err(Error::Other(format!(
                "installed dependency link `{}` resolves outside the isolated Zen checkout to \
                 `{}`; refusing to run checks because writes could escape into persistent state",
                link.display(),
                resolved.display()
            )));
        }
    }
    Ok(())
}

fn normalize_absolute_path(path: &std::path::Path) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(std::path::MAIN_SEPARATOR_STR),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

fn resolve_link_path(
    start: &std::path::Path,
    links: &std::collections::HashMap<PathBuf, PathBuf>,
) -> Option<PathBuf> {
    if !start.is_absolute() {
        return None;
    }
    let mut pending = owned_path_components(start);
    let mut resolved = PathBuf::new();
    let mut followed_links = 0usize;

    while let Some(component) = pending.pop_front() {
        match component {
            OwnedPathComponent::Prefix(prefix) => {
                resolved.clear();
                resolved.push(prefix);
            }
            OwnedPathComponent::RootDir => {
                resolved.clear();
                resolved.push(std::path::MAIN_SEPARATOR_STR);
            }
            OwnedPathComponent::CurDir => {}
            OwnedPathComponent::ParentDir => {
                if !resolved.pop() {
                    return None;
                }
            }
            OwnedPathComponent::Normal(part) => {
                resolved.push(part);
                let Some(target) = links.get(&resolved) else {
                    continue;
                };
                followed_links += 1;
                if followed_links > 128 || !resolved.pop() {
                    return None;
                }

                // Process the target before the original remainder. This is
                // intentionally component-by-component: for a target such as
                // `bridge/../file`, the kernel resolves `bridge` before it
                // applies `..`, and the validator must do the same.
                let mut target_components = owned_path_components(target);
                while let Some(target_component) = target_components.pop_back() {
                    pending.push_front(target_component);
                }
            }
        }
    }
    resolved.is_absolute().then_some(resolved)
}

#[derive(Clone)]
enum OwnedPathComponent {
    Prefix(std::ffi::OsString),
    RootDir,
    CurDir,
    ParentDir,
    Normal(std::ffi::OsString),
}

fn owned_path_components(path: &std::path::Path) -> std::collections::VecDeque<OwnedPathComponent> {
    path.components()
        .map(|component| match component {
            Component::Prefix(prefix) => {
                OwnedPathComponent::Prefix(prefix.as_os_str().to_os_string())
            }
            Component::RootDir => OwnedPathComponent::RootDir,
            Component::CurDir => OwnedPathComponent::CurDir,
            Component::ParentDir => OwnedPathComponent::ParentDir,
            Component::Normal(part) => OwnedPathComponent::Normal(part.to_os_string()),
        })
        .collect()
}

/// Refuse to launch any check whose `cd <worktree>` would silently land
/// in `$HOME`. The local-host case is the easy one to detect: just stat
/// the path. For SSH hosts we let the remote shell surface its own cd
/// error rather than round-trip a separate "does the path exist" probe —
/// it still surfaces in the `output_tail` for the very first check.
fn ensure_worktree_present(host: &Host, worktree: &std::path::Path) -> Result<()> {
    if matches!(host, Host::Local) && !worktree.exists() {
        return Err(Error::Other(format!(
            "workspace worktree `{}` does not exist on disk — \
             dispatch the task to its workspace before probing, \
             or remove the stale assignment from the task",
            worktree.display()
        )));
    }
    Ok(())
}

#[cfg(test)]
fn run_one_check(host: &Host, worktree: &std::path::Path, cmd: &str) -> LocalCheck {
    run_one_check_with_shared_cargo_target(host, worktree, cmd, None)
}

fn run_one_check_with_shared_cargo_target(
    host: &Host,
    worktree: &std::path::Path,
    cmd: &str,
    shared_cargo_target: Option<&std::path::Path>,
) -> LocalCheck {
    let wt = worktree.to_string_lossy().into_owned();
    // We `cd` into the worktree first because some checks care about the
    // working directory, not just argv[0]'s path — anything that walks up
    // to a project root (Cargo.toml / package.json / pyproject.toml /
    // go.mod / mix.exs / Gemfile / ...) breaks if launched from `$HOME`.
    let cargo_setup = shared_cargo_target
        .map(|target| {
            format!(
                "if [ -z \"${{CARGO_TARGET_DIR:-}}\" ]; then export CARGO_TARGET_DIR={}; fi; ",
                shell_escape(&target.to_string_lossy())
            )
        })
        .unwrap_or_default();
    let script = format!("{cargo_setup}cd {} && {}", shell_escape(&wt), cmd);

    let started = Instant::now();
    let output = run_check_script(host, &script);
    let elapsed = started.elapsed();

    let (exit_code, combined) = match output {
        Ok(o) => {
            let code = o.status.code().unwrap_or(-1);
            let mut buf = String::new();
            buf.push_str(&String::from_utf8_lossy(&o.stdout));
            if !o.stderr.is_empty() {
                if !buf.is_empty() && !buf.ends_with('\n') {
                    buf.push('\n');
                }
                buf.push_str(&String::from_utf8_lossy(&o.stderr));
            }
            (code, buf)
        }
        Err(e) => (-1, format!("(shelbi: failed to launch command: {e})\n")),
    };

    let tail = tail_lines(&combined, OUTPUT_TAIL_LINES);
    let tail = augment_command_not_found(host, cmd, exit_code, tail);

    LocalCheck {
        command: cmd.to_string(),
        exit_code,
        duration_ms: ms_truncating(elapsed),
        output_tail: tail,
    }
}

/// Run a check script through a login shell so the user's rc files
/// (~/.zprofile, ~/.bash_profile, etc.) populate `PATH` with tools
/// installed via rustup, asdf, mise, nvm, pyenv, rbenv, volta, or
/// homebrew before the check runs.
///
/// The hub process can inherit a stripped-down `PATH` when it's launched
/// outside the user's terminal — from launchd / Spotlight, or from a
/// tmux server that itself started in a non-login context — so trusting
/// the inherited environment isn't enough. Same trick `workspace.rs`,
/// `spawn.rs`, and [`run_in_dir`] use to keep agent launches and
/// `gh`/`git` calls finding the same tools the user sees in their own
/// terminal — see [`login_shell_prefix`] for the host-specific shell
/// resolution.
fn run_check_script(host: &Host, script: &str) -> std::io::Result<Output> {
    run_login_shell_script(host, script)
}

/// If the check exited 127 (POSIX "command not found"), append a shelbi
/// hint that names the first token tried and what was searched. The hint
/// is templated on the user's actual command — no specific tool name is
/// baked into the binary, so this stays language-agnostic.
fn augment_command_not_found(host: &Host, cmd: &str, exit_code: i32, tail: String) -> String {
    if exit_code != 127 {
        return tail;
    }
    let Some(tool) = cmd.split_whitespace().next() else {
        return tail;
    };
    let (shell, _) = login_shell_prefix(host);
    // Strip a trailing `;` / `&&` / `|` if someone happened to chain on
    // the first token (`foo; bar`) — the human-readable name is just `foo`.
    let tool = tool.trim_end_matches([';', '&', '|']);
    let hint = format!(
        "shelbi: `{tool}` was not found on the login-shell PATH \
         (checked via `{shell} -lc 'command -v {tool}'`). \
         Install it or add it to PATH in your login shell's rc \
         (e.g. ~/.zprofile, ~/.bash_profile)."
    );
    let mut out = tail;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&hint);
    out
}

/// How many trailing lines of combined output to keep per check.
pub const OUTPUT_TAIL_LINES: usize = 40;

fn ms_truncating(d: Duration) -> u64 {
    // Saturating cast: a check that ran for >500 million years truncates to
    // u64::MAX rather than overflowing. Realistic local checks fit in u64 ms
    // by ~10 orders of magnitude; this is just defensive against
    // pathological `Duration` values from tests.
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Return the last `n` non-empty trailing lines of `s`, joined with `\n`.
/// We trim the trailing blank line (most command output ends with one) so
/// the limit reflects what a human sees on screen, not the raw line count.
fn tail_lines(s: &str, n: usize) -> String {
    let trimmed = s.trim_end_matches('\n');
    let lines: Vec<&str> = trimmed.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

fn shell_escape(s: &str) -> String {
    // Single-quote, doubled-up for any embedded single quotes. Works for
    // POSIX shells which is what `sh -c` invokes on every supported host.
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

fn log_file_path(task_id: &str) -> Result<PathBuf> {
    let dir = shelbi_state::shelbi_home()?.join("logs");
    Ok(dir.join(format!("zen-{task_id}.log")))
}

fn init_log(path: &std::path::Path, task_id: &str, n_checks: usize) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // Truncate on each probe — the log is for this run, not history.
    let mut f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)?;
    writeln!(
        f,
        "# shelbi zen probe — task {task_id} — {n_checks} check(s)"
    )?;
    Ok(())
}

fn append_log(path: &std::path::Path, check: &LocalCheck) -> std::io::Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write;
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f)?;
    writeln!(
        f,
        "$ {} (exit {}, {}ms)",
        check.command, check.exit_code, check.duration_ms
    )?;
    f.write_all(check.output_tail.as_bytes())?;
    if !check.output_tail.ends_with('\n') {
        writeln!(f)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// merge_conflict

fn probe_merge_conflict(
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
    main: &str,
) -> Result<ConflictProbe> {
    let wt = worktree.to_string_lossy().into_owned();

    // Use `git merge-tree --write-tree` (the new variant, git >= 2.38) to
    // simulate a merge without touching any working tree. On success it
    // prints the merged tree OID; on conflict it exits non-zero and prints
    // conflict information we can parse for file names. This is
    // dramatically simpler than spinning up a temp worktree, and has no
    // cleanup obligation — the spirit of "abort regardless of outcome" is
    // satisfied because nothing was ever mutated.
    let out = shelbi_ssh::run(
        host,
        [
            "git",
            "-C",
            wt.as_str(),
            "merge-tree",
            "--write-tree",
            "--name-only",
            main,
            branch,
        ],
    )
    .map_err(Error::Io)?;

    let exit_code = out.status.code().unwrap_or(-1);
    if exit_code == 0 {
        return Ok(ConflictProbe::default());
    }
    if exit_code != 1 {
        // Anything other than 0 (clean) or 1 (conflict) is a hard error
        // — bad ref, missing main, etc. Surface it rather than masking
        // it as a conflict report.
        return Err(Error::Command {
            cmd: format!("git -C {wt} merge-tree --write-tree --name-only {main} {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    // Exit 1 = conflicts. With --name-only the conflicted-file section is
    // a list of paths separated from the trailing informational messages
    // by a blank line. Stop at the first blank line so we don't slurp in
    // "Auto-merging foo.rs" etc.
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut lines = stdout.lines();
    let _tree_oid = lines.next(); // first line is the merged tree OID
    let files: Vec<String> = lines
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim().to_string())
        .collect();
    Ok(ConflictProbe {
        conflicts: true,
        files,
    })
}

// ---------------------------------------------------------------------------
// diff_size

fn probe_diff_size(
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
    main: &str,
) -> Result<DiffSize> {
    let wt = worktree.to_string_lossy().into_owned();
    // Three-dot (merge-base) diff, not two-dot. Two-dot `main..branch`
    // diffs the two tips, so anything that landed on `main` after this
    // branch was cut leaks into the count as spurious removals (or masks
    // real additions) — the -402 phantom in the task report was another
    // branch's churn that had just merged into main. Three-dot
    // `main...branch` diffs against the *merge base*, which is exactly
    // what a squash-merge of this branch will apply.
    let range = format!("{main}...{branch}");
    let stdout = shelbi_ssh::run_capture(
        host,
        [
            "git",
            "-C",
            wt.as_str(),
            "diff",
            "--shortstat",
            range.as_str(),
        ],
    )?;
    Ok(parse_shortstat(&stdout))
}

/// Parse the trailing summary line emitted by `git diff --shortstat`.
///
/// Expected shapes (note the leading space):
///
/// - `` (empty) — no diff
/// - ` 5 files changed, 87 insertions(+), 12 deletions(-)`
/// - ` 1 file changed, 5 insertions(+)`
/// - ` 1 file changed, 5 deletions(-)`
/// - ` 1 file changed` (binary-only / mode-only diffs)
pub fn parse_shortstat(s: &str) -> DiffSize {
    let line = s.trim();
    if line.is_empty() {
        return DiffSize::default();
    }
    let mut out = DiffSize::default();
    for part in line.split(',') {
        let part = part.trim();
        // " N files changed"
        if let Some(n) = strip_suffix_then_parse(part, " files changed")
            .or_else(|| strip_suffix_then_parse(part, " file changed"))
        {
            out.files = n;
        } else if let Some(n) = strip_suffix_then_parse(part, " insertions(+)")
            .or_else(|| strip_suffix_then_parse(part, " insertion(+)"))
        {
            out.lines_added = n;
        } else if let Some(n) = strip_suffix_then_parse(part, " deletions(-)")
            .or_else(|| strip_suffix_then_parse(part, " deletion(-)"))
        {
            out.lines_removed = n;
        }
    }
    out
}

fn strip_suffix_then_parse(s: &str, suffix: &str) -> Option<usize> {
    s.strip_suffix(suffix)
        .and_then(|head| head.trim().parse::<usize>().ok())
}

// ---------------------------------------------------------------------------
// danger_paths

fn probe_danger_paths(
    project: &Project,
    workflow: Option<&Workflow>,
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
    base: &str,
) -> Result<DangerPaths> {
    let patterns = danger_paths_for_workflow(project, workflow);
    if patterns.is_empty() {
        return Ok(DangerPaths::default());
    }
    let wt = worktree.to_string_lossy().into_owned();
    // Three-dot (merge-base) diff — same rationale as `probe_diff_size`.
    // Two-dot would surface files touched on `base` after the branch was
    // cut and wrongly flag the branch for danger paths it never touched.
    let range = format!("{base}...{branch}");
    let stdout = shelbi_ssh::run_capture(
        host,
        [
            "git",
            "-C",
            wt.as_str(),
            "diff",
            "--name-only",
            range.as_str(),
        ],
    )?;
    let changed: Vec<&str> = stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Ok(match_danger_paths(&patterns, &changed))
}

/// Match `changed` paths against `patterns` (any-of). A bad glob is
/// silently skipped — the project YAML is user-authored and we'd rather
/// over-report than blow up the whole probe over one typo. The orchestrator
/// already validates the YAML on save; this is belt-and-suspenders.
pub fn match_danger_paths(patterns: &[String], changed: &[&str]) -> DangerPaths {
    let mut builder = GlobSetBuilder::new();
    for p in patterns {
        if let Ok(g) = Glob::new(p) {
            builder.add(g);
        }
    }
    let set = match builder.build() {
        Ok(s) => s,
        Err(_) => return DangerPaths::default(),
    };
    let mut matched = Vec::new();
    for path in changed {
        if set.is_match(path) {
            matched.push((*path).to_string());
        }
    }
    DangerPaths { matched }
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod probe_tests {
    use super::*;
    use std::process::Command;

    // --- diff_size parsing -------------------------------------------------

    #[test]
    fn parse_shortstat_empty() {
        assert_eq!(parse_shortstat(""), DiffSize::default());
        assert_eq!(parse_shortstat("   \n"), DiffSize::default());
    }

    #[test]
    fn parse_shortstat_full() {
        let s = " 5 files changed, 87 insertions(+), 12 deletions(-)\n";
        assert_eq!(
            parse_shortstat(s),
            DiffSize {
                files: 5,
                lines_added: 87,
                lines_removed: 12,
            }
        );
    }

    #[test]
    fn parse_shortstat_singular_units() {
        let s = " 1 file changed, 1 insertion(+), 1 deletion(-)";
        assert_eq!(
            parse_shortstat(s),
            DiffSize {
                files: 1,
                lines_added: 1,
                lines_removed: 1,
            }
        );
    }

    #[test]
    fn parse_shortstat_additions_only() {
        let s = " 1 file changed, 5 insertions(+)";
        assert_eq!(
            parse_shortstat(s),
            DiffSize {
                files: 1,
                lines_added: 5,
                lines_removed: 0,
            }
        );
    }

    #[test]
    fn parse_shortstat_deletions_only() {
        let s = " 2 files changed, 9 deletions(-)";
        assert_eq!(
            parse_shortstat(s),
            DiffSize {
                files: 2,
                lines_added: 0,
                lines_removed: 9,
            }
        );
    }

    #[test]
    fn parse_shortstat_binary_only() {
        let s = " 1 file changed";
        assert_eq!(
            parse_shortstat(s),
            DiffSize {
                files: 1,
                lines_added: 0,
                lines_removed: 0,
            }
        );
    }

    // --- tail / shell escape ----------------------------------------------

    #[test]
    fn tail_keeps_last_n() {
        let s = (1..=100)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let tail = tail_lines(&s, 5);
        assert_eq!(tail, "96\n97\n98\n99\n100");
    }

    #[test]
    fn tail_handles_short_input() {
        assert_eq!(tail_lines("a\nb", 5), "a\nb");
        assert_eq!(tail_lines("", 5), "");
    }

    #[test]
    fn tail_trims_trailing_blank_line() {
        // Typical command output ends with a newline; that shouldn't
        // count as an empty tail entry.
        let s = "one\ntwo\nthree\n";
        assert_eq!(tail_lines(s, 5), "one\ntwo\nthree");
    }

    #[test]
    fn shell_escape_wraps_in_single_quotes() {
        assert_eq!(shell_escape("/tmp/foo bar"), "'/tmp/foo bar'");
    }

    #[test]
    fn shell_escape_handles_embedded_single_quotes() {
        assert_eq!(shell_escape("it's fine"), "'it'\\''s fine'");
    }

    // --- danger paths matching --------------------------------------------

    #[test]
    fn danger_paths_matches_builtin_globs() {
        let patterns: Vec<String> = shelbi_core::BUILTIN_DANGER_PATHS
            .iter()
            .map(|s| s.to_string())
            .collect();
        let changed = vec![
            ".github/workflows/ci.yml",
            "src/foo.rs",
            "Cargo.lock",
            "LICENSE",
            "docs/intro.md",
        ];
        let res = match_danger_paths(&patterns, &changed);
        assert!(res.matched.iter().any(|p| p == ".github/workflows/ci.yml"));
        assert!(res.matched.iter().any(|p| p == "Cargo.lock"));
        assert!(res.matched.iter().any(|p| p == "LICENSE"));
        assert!(!res.matched.iter().any(|p| p == "src/foo.rs"));
        assert!(!res.matched.iter().any(|p| p == "docs/intro.md"));
    }

    #[test]
    fn danger_paths_glob_not_literal() {
        let patterns = vec!["migrations/**".to_string()];
        let changed = vec![
            "migrations/202601010001_init.sql",
            "src/migrations.rs",
            "migrations",
        ];
        let res = match_danger_paths(&patterns, &changed);
        assert_eq!(
            res.matched,
            vec!["migrations/202601010001_init.sql".to_string()]
        );
    }

    #[test]
    fn danger_paths_bad_glob_is_skipped_not_fatal() {
        // The unbalanced bracket isn't a valid glob; the good one still
        // applies.
        let patterns = vec!["[unclosed".to_string(), "*.yaml".to_string()];
        let changed = vec!["project.yaml", "src/foo.rs"];
        let res = match_danger_paths(&patterns, &changed);
        assert_eq!(res.matched, vec!["project.yaml".to_string()]);
    }

    #[test]
    fn danger_paths_empty_patterns_means_no_matches() {
        let res = match_danger_paths(&[], &["anything"]);
        assert!(res.matched.is_empty());
    }

    // --- git-backed primitives (real fixture repos) -----------------------

    /// Build a one-shot fixture repo on disk with two branches:
    /// - `main` — the baseline.
    /// - `feature` — diverges per `mutate_feature`.
    ///
    /// Returns the worktree path of the parent repo (main checked out).
    fn fixture_repo<F: FnOnce(&std::path::Path)>(
        mutate_feature: F,
    ) -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        run_git(&repo, &["init", "-q", "-b", "main", "."]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);

        // Create feature branch, apply mutation, commit.
        run_git(&repo, &["checkout", "-q", "-b", "feature"]);
        mutate_feature(&repo);
        run_git(&repo, &["add", "-A"]);
        run_git(&repo, &["commit", "-q", "-m", "feature work"]);
        run_git(&repo, &["checkout", "-q", "main"]);
        (tmp, repo)
    }

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(cwd)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    #[test]
    fn diff_size_against_real_repo() {
        let (_tmp, repo) = fixture_repo(|r| {
            std::fs::write(r.join("a.txt"), "1\n2\n3\n").unwrap();
            std::fs::write(r.join("b.txt"), "x\n").unwrap();
        });
        let size = probe_diff_size(&Host::Local, &repo, "feature", "main").unwrap();
        assert_eq!(size.files, 2);
        assert_eq!(size.lines_added, 4);
        assert_eq!(size.lines_removed, 0);
    }

    #[test]
    fn diff_size_empty_when_branches_identical() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q", "-b", "main", "."]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("x"), "x\n").unwrap();
        run_git(repo, &["add", "x"]);
        run_git(repo, &["commit", "-q", "-m", "init"]);
        run_git(repo, &["branch", "feature"]);
        let size = probe_diff_size(&Host::Local, repo, "feature", "main").unwrap();
        assert_eq!(size, DiffSize::default());
    }

    #[test]
    fn diff_size_ignores_main_side_churn_after_branch_cut() {
        // Reproduces the phantom -402 from the task report: an unrelated
        // branch merges into `main` *after* `feature` was cut. Two-dot
        // `main..feature` would subtract main's post-cut lines (and count
        // its new files); three-dot `main...feature` diffs against the
        // merge base, so `feature`'s size is exactly the one file it added
        // regardless of what landed on main in the meantime.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q", "-b", "main", "."]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run_git(repo, &["add", "README.md"]);
        run_git(repo, &["commit", "-q", "-m", "init"]);

        // Cut feature and add ONE new file (analog of the 304-line file).
        run_git(repo, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(repo.join("new.txt"), "a\nb\nc\n").unwrap();
        run_git(repo, &["add", "new.txt"]);
        run_git(repo, &["commit", "-q", "-m", "feature: one new file"]);

        // Meanwhile main advances with unrelated churn (the "other branch
        // merged" case). Two-dot would fold `other.txt` into feature's diff
        // as a spurious removal.
        run_git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("other.txt"), "x\ny\nz\n").unwrap();
        run_git(repo, &["add", "other.txt"]);
        run_git(repo, &["commit", "-q", "-m", "unrelated merge into main"]);

        let size = probe_diff_size(&Host::Local, repo, "feature", "main").unwrap();
        assert_eq!(size.files, 1, "only feature's own file should count");
        assert_eq!(size.lines_added, 3);
        assert_eq!(size.lines_removed, 0, "no main-side churn should leak in");
    }

    #[test]
    fn diff_size_deletion_only() {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q", "-b", "main", "."]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("a.txt"), "1\n2\n3\n").unwrap();
        run_git(repo, &["add", "a.txt"]);
        run_git(repo, &["commit", "-q", "-m", "init"]);
        run_git(repo, &["checkout", "-q", "-b", "feature"]);
        std::fs::remove_file(repo.join("a.txt")).unwrap();
        run_git(repo, &["commit", "-q", "-am", "remove"]);
        run_git(repo, &["checkout", "-q", "main"]);
        let size = probe_diff_size(&Host::Local, repo, "feature", "main").unwrap();
        assert_eq!(size.files, 1);
        assert_eq!(size.lines_added, 0);
        assert_eq!(size.lines_removed, 3);
    }

    #[test]
    fn merge_conflict_clean_branch_reports_no_conflict() {
        let (_tmp, repo) = fixture_repo(|r| {
            std::fs::write(r.join("a.txt"), "added on feature\n").unwrap();
        });
        let probe = probe_merge_conflict(&Host::Local, &repo, "feature", "main").unwrap();
        assert!(!probe.conflicts);
        assert!(probe.files.is_empty());
    }

    #[test]
    fn merge_conflict_diverged_file_reports_conflict_and_filename() {
        // Branch both edit README.md at the same lines.
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        run_git(repo, &["init", "-q", "-b", "main", "."]);
        run_git(repo, &["config", "user.email", "test@example.com"]);
        run_git(repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hello\n").unwrap();
        run_git(repo, &["add", "README.md"]);
        run_git(repo, &["commit", "-q", "-m", "init"]);

        // feature: change line 1 to "feature-side"
        run_git(repo, &["checkout", "-q", "-b", "feature"]);
        std::fs::write(repo.join("README.md"), "feature-side\n").unwrap();
        run_git(repo, &["commit", "-q", "-am", "feature edit"]);

        // main: change line 1 to "main-side"
        run_git(repo, &["checkout", "-q", "main"]);
        std::fs::write(repo.join("README.md"), "main-side\n").unwrap();
        run_git(repo, &["commit", "-q", "-am", "main edit"]);

        let probe = probe_merge_conflict(&Host::Local, repo, "feature", "main").unwrap();
        assert!(probe.conflicts, "expected conflict");
        assert!(
            probe.files.iter().any(|f| f == "README.md"),
            "expected README.md in files, got {:?}",
            probe.files
        );

        // Worktree must be clean — nothing was checked out.
        let status_out = Command::new("git")
            .current_dir(repo)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(status_out.stdout.is_empty(), "worktree must be clean");
    }

    #[test]
    fn local_check_runs_all_even_after_failure() {
        // Verify the "no short-circuit" promise without needing a Project
        // by calling run_one_check directly on a sequence.
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        let fail = run_one_check(&Host::Local, wt, "exit 7");
        let ok = run_one_check(&Host::Local, wt, "echo ran-after-failure");
        assert_eq!(fail.exit_code, 7);
        assert_eq!(ok.exit_code, 0);
        assert!(ok.output_tail.contains("ran-after-failure"));
    }

    #[test]
    fn local_check_captures_stderr_too() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        let res = run_one_check(&Host::Local, wt, "echo out; echo err 1>&2");
        assert_eq!(res.exit_code, 0);
        assert!(res.output_tail.contains("out"));
        assert!(res.output_tail.contains("err"));
    }

    #[test]
    fn local_check_runs_in_a_login_shell() {
        // Login shells prepend `-` to `$0` (e.g. `-bash`, `-sh`), per the
        // POSIX convention every UNIX shell honors. If `run_one_check`
        // regresses to a non-login `sh -c`, this assertion catches it —
        // and with it the rustup/asdf/homebrew-PATH bug that motivated
        // the login-shell switch.
        //
        // Override `$SHELL` to `/bin/sh` so the test doesn't depend on the
        // developer's preferred login shell being installed and well-
        // behaved (e.g. zsh's compinit complaining about insecure dirs).
        let _guard = crate::test_lock::acquire();
        let prev_shell = std::env::var_os("SHELL");
        std::env::set_var("SHELL", "/bin/sh");
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        let res = run_one_check(
            &Host::Local,
            wt,
            r#"case "$0" in -*) echo login;; *) echo non-login;; esac"#,
        );
        match prev_shell {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        assert_eq!(res.exit_code, 0);
        assert!(
            res.output_tail.contains("login"),
            "expected login-shell `$0`; got: {}",
            res.output_tail
        );
    }

    #[test]
    fn local_check_missing_tool_appends_shelbi_hint() {
        // A check whose first token genuinely isn't on the login-shell
        // PATH must surface a friendly "shelbi:" hint that names the
        // tool and what was searched — never the bare
        // `sh: <tool>: command not found` that bricks users new to
        // version-manager setups. Verified with a deliberately-random
        // identifier (not "cargo"/"npm"/"go" etc.) so the test stays
        // language-agnostic — same as the production code.
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        let tool = "shelbi-test-no-such-tool-9c3a7b";
        let res = run_one_check(&Host::Local, wt, tool);
        assert_eq!(
            res.exit_code, 127,
            "expected POSIX 'command not found' exit"
        );
        assert!(
            res.output_tail.contains("shelbi:"),
            "expected shelbi-prefixed hint, got: {}",
            res.output_tail
        );
        assert!(
            res.output_tail.contains(tool),
            "expected hint to name the missing tool `{tool}`, got: {}",
            res.output_tail
        );
        assert!(
            res.output_tail.contains("login-shell PATH"),
            "expected hint to name what was searched, got: {}",
            res.output_tail
        );
    }

    #[test]
    fn local_check_resolves_tool_only_on_login_shell_path() {
        // Simulate the "tool installed via a version manager that
        // initialises PATH from the user's rc" case without touching
        // the developer's real rustup/asdf/nvm setup. Drops a fake
        // binary into a temp dir, then writes a private rc file that
        // adds that dir to PATH and points $SHELL/$ZDOTDIR/$HOME at
        // the test setup so the login shell sources it.
        //
        // Mirrors the rustup/asdf/mise/nvm/pyenv/rbenv/volta/homebrew
        // failure mode from the task spec — same shape, but with a
        // fixture binary so the test isn't Rust-coupled.
        let _guard = crate::test_lock::acquire();
        let tmp = tempfile::tempdir().unwrap();
        let bindir = tmp.path().join("custom-bin");
        std::fs::create_dir_all(&bindir).unwrap();
        let tool = "shelbi-test-login-shell-tool";
        let tool_path = bindir.join(tool);
        std::fs::write(&tool_path, "#!/bin/sh\necho login-shell-tool-ran\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        // Login bash-as-sh sources ~/.profile. Override $HOME so we drop
        // the rc file in the fixture's tmp dir without touching the
        // developer's real shell setup. Same shape every login shell
        // honors (bash, dash, ksh, ash) so the test isn't pinned to a
        // specific shell.
        let profile = tmp.path().join(".profile");
        std::fs::write(
            &profile,
            format!("export PATH=\"{}:$PATH\"\n", bindir.display()),
        )
        .unwrap();

        let prev_shell = std::env::var_os("SHELL");
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("SHELL", "/bin/sh");
        std::env::set_var("HOME", tmp.path());

        let res = run_one_check(&Host::Local, tmp.path(), tool);

        match prev_shell {
            Some(v) => std::env::set_var("SHELL", v),
            None => std::env::remove_var("SHELL"),
        }
        match prev_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }

        assert_eq!(
            res.exit_code, 0,
            "expected fake tool to be reachable via login-shell PATH; got: {}",
            res.output_tail
        );
        assert!(
            res.output_tail.contains("login-shell-tool-ran"),
            "expected fake tool's output, got: {}",
            res.output_tail
        );
    }

    #[test]
    fn ensure_worktree_present_errors_with_friendly_message() {
        // The whole point of the preflight: when the worktree is gone,
        // the error has to name the path and explain *why* (so the user
        // knows they probed an unassigned task, not that cargo / npm /
        // pytest is broken). Hard-fail before we ever `cd $HOME && cmd`.
        let missing = std::path::Path::new("/definitely/does/not/exist/shelbi-probe-test");
        let err = ensure_worktree_present(&Host::Local, missing).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("does not exist on disk"),
            "expected 'does not exist on disk' in error, got: {msg}"
        );
        assert!(
            msg.contains(missing.to_str().unwrap()),
            "expected error to name the missing path, got: {msg}"
        );
    }

    #[test]
    fn ensure_worktree_present_accepts_real_dir() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_worktree_present(&Host::Local, tmp.path()).unwrap();
    }

    #[test]
    fn local_check_output_tail_is_bounded() {
        let tmp = tempfile::tempdir().unwrap();
        let wt = tmp.path();
        let res = run_one_check(
            &Host::Local,
            wt,
            "i=0; while [ $i -lt 200 ]; do echo line-$i; i=$((i+1)); done",
        );
        let line_count = res.output_tail.lines().count();
        assert_eq!(line_count, OUTPUT_TAIL_LINES);
        // Last line is line-199.
        assert!(res.output_tail.ends_with("line-199"));
    }

    // --- rebase-onto-default before probing -------------------------------
    //
    // These exercise `probe_in_workflow` end-to-end against real git repos
    // wired exactly the way production does: a workspace worktree at
    // `<machine.work_dir>/.shelbi/wt/<workspace>` with an `origin` it can
    // fetch from. The scenarios mirror the bug: the default branch advanced
    // after handoff, so the probe must fetch + rebase before it can speak
    // to the *current* state.

    use shelbi_core::{
        AgentRunnerSpec, GitConfig, HeartbeatConfig, MachineKind, OrchestratorSpec, ZenChecks,
        ZenConfig,
    };

    struct ProbeHomeGuard {
        previous: Option<std::ffi::OsString>,
        previous_cargo_target: Option<std::ffi::OsString>,
        _home: tempfile::TempDir,
    }

    impl ProbeHomeGuard {
        fn install() -> Self {
            let home = tempfile::tempdir().unwrap();
            let previous = std::env::var_os("SHELBI_HOME");
            let previous_cargo_target = std::env::var_os("CARGO_TARGET_DIR");
            std::env::set_var("SHELBI_HOME", home.path());
            std::env::remove_var("CARGO_TARGET_DIR");
            Self {
                previous,
                previous_cargo_target,
                _home: home,
            }
        }
    }

    impl Drop for ProbeHomeGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(previous) => std::env::set_var("SHELBI_HOME", previous),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            match self.previous_cargo_target.take() {
                Some(previous) => std::env::set_var("CARGO_TARGET_DIR", previous),
                None => std::env::remove_var("CARGO_TARGET_DIR"),
            }
        }
    }

    /// Stand up an `origin` bare repo seeded with `main` (one commit adding
    /// `seed.txt`) and clone it into the workspace's conventional worktree
    /// path. Returns the temp base (machine work_dir), the origin path, and
    /// the worktree path. The caller advances `origin/main` and creates the
    /// task branch to shape each scenario.
    fn setup_origin_and_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let base = tempfile::tempdir().unwrap();
        let base_path = base.path().to_path_buf();

        // Bare origin holding the canonical default branch.
        let origin = base_path.join("origin.git");
        run_git(
            &base_path,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                origin.to_str().unwrap(),
            ],
        );

        // A throwaway seed clone to push the initial `main` commit.
        let seed = base_path.join("seed");
        run_git(
            &base_path,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                seed.to_str().unwrap(),
            ],
        );
        run_git(&seed, &["config", "user.email", "test@example.com"]);
        run_git(&seed, &["config", "user.name", "Test"]);
        std::fs::write(seed.join("seed.txt"), "seed\n").unwrap();
        run_git(&seed, &["add", "seed.txt"]);
        run_git(&seed, &["commit", "-q", "-m", "seed main"]);
        run_git(&seed, &["push", "-q", "origin", "main"]);

        // The workspace worktree lives at the path `workspace_worktree`
        // computes, so `probe_in_workflow` resolves to it unchanged.
        let wt = base_path.join(".shelbi").join("wt").join("ws1");
        std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
        run_git(
            &base_path,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                wt.to_str().unwrap(),
            ],
        );
        run_git(&wt, &["config", "user.email", "test@example.com"]);
        run_git(&wt, &["config", "user.name", "Test"]);

        (base, origin, wt)
    }

    /// Push a new commit onto `origin/main` from a fresh clone — simulating a
    /// blocker fix landing on the default branch after the workspace handed
    /// off. `mutate` shapes the working tree of that commit.
    fn advance_origin_main<F: FnOnce(&std::path::Path)>(
        base: &std::path::Path,
        origin: &std::path::Path,
        msg: &str,
        mutate: F,
    ) {
        let bump = base.join(format!("bump-{msg}").replace(' ', "-"));
        run_git(
            base,
            &[
                "clone",
                "-q",
                origin.to_str().unwrap(),
                bump.to_str().unwrap(),
            ],
        );
        run_git(&bump, &["config", "user.email", "test@example.com"]);
        run_git(&bump, &["config", "user.name", "Test"]);
        mutate(&bump);
        run_git(&bump, &["add", "-A"]);
        run_git(&bump, &["commit", "-q", "-m", msg]);
        run_git(&bump, &["push", "-q", "origin", "main"]);
    }

    /// Project pointing its single workspace's worktree at `work_dir`, with
    /// `local_checks` as its Zen local checks.
    fn probe_project(work_dir: &std::path::Path, local_checks: &[&str]) -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "probe-test".into(),
            repo: work_dir.to_string_lossy().into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: work_dir.to_path_buf(),
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
                name: "ws1".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                tags: Vec::new(),
                slot: None,
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig {
                checks: ZenChecks {
                    local: local_checks.iter().map(|s| s.to_string()).collect(),
                },
                ..ZenConfig::default()
            },
            heartbeat: HeartbeatConfig::default(),
            detected_shapes: Vec::new(),
            git: GitConfig::default(),
        }
    }

    fn probe_task(branch: &str) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "task1".into(),
            title: "task1".into(),
            column: Column::review(),
            priority: 0,
            assigned_to: Some("ws1".into()),
            workflow: None,
            branch: Some(branch.into()),
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    fn head_sha(repo: &std::path::Path) -> String {
        String::from_utf8_lossy(
            &Command::new("git")
                .current_dir(repo)
                .args(["rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .trim()
        .to_string()
    }

    fn probe_git_stdout(repo: &std::path::Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(repo)
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {} failed in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn branch_sha(repo: &std::path::Path, branch: &str) -> String {
        probe_git_stdout(repo, &["rev-parse", &format!("refs/heads/{branch}")])
    }

    fn detach_for_handoff(repo: &std::path::Path, branch: &str) {
        let outcome = crate::workspace::detach_workspace_worktree(&Host::Local, repo);
        assert_eq!(
            outcome,
            crate::workspace::DetachOutcome::Detached {
                from_branch: Some(branch.into())
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn failed_probe_worktree_checkout_is_removed() {
        use std::os::unix::fs::PermissionsExt;

        let (_base, _origin, wt) = setup_origin_and_worktree();
        let hook = wt.join(".git/hooks/post-checkout");
        std::fs::write(&hook, "#!/bin/sh\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let probe_path = unique_probe_worktree_path(&wt, "hook-failure");
        let head = head_sha(&wt);
        let err = add_probe_worktree(&Host::Local, &wt, &probe_path, &head)
            .unwrap_err()
            .to_string();
        assert!(err.contains("worktree add"), "{err}");

        let worktrees = probe_git_stdout(&wt, &["worktree", "list", "--porcelain"]);
        assert!(
            !worktrees.contains(&probe_path.to_string_lossy().into_owned()),
            "failed checkout left a registered probe worktree:\n{worktrees}"
        );
        assert!(
            !probe_path.exists(),
            "failed checkout left probe files at {}",
            probe_path.display()
        );
    }

    #[test]
    fn probe_rebases_stale_worktree_onto_advanced_default() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        // (a) The default advanced (a blocker fix added `fix.txt`) after the
        // workspace handed off. The probe must fetch + rebase so the local
        // check — which only passes when `fix.txt` is present — sees the new
        // default. Before the rebase the worktree is stale and the check
        // would fail.
        let (base, origin, wt) = setup_origin_and_worktree();

        // Task branch off the seed commit, with its own work.
        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("work.txt"), "task work\n").unwrap();
        run_git(&wt, &["add", "-A"]);
        run_git(&wt, &["commit", "-q", "-m", "task work"]);
        let before = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");
        let detached_workspace_head = head_sha(&wt);

        // Blocker fix lands on origin/main after handoff.
        advance_origin_main(base.path(), &origin, "add fix", |r| {
            std::fs::write(r.join("fix.txt"), "fixed\n").unwrap();
        });

        let project = probe_project(base.path(), &["test -f fix.txt"]);
        let task = probe_task("shelbi/task1");
        let report = probe_in_workflow(
            &project,
            None,
            &task,
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();

        assert!(!report.rebase_conflict.conflicts, "clean rebase expected");
        assert_eq!(report.local_checks.len(), 1, "the local check must run");
        assert_eq!(
            report.local_checks[0].exit_code, 0,
            "check should see the rebased default (fix.txt present): {}",
            report.local_checks[0].output_tail
        );
        // The durable task ref was rewritten onto the advanced default, but
        // the detached workspace checkout was not touched.
        assert_ne!(
            branch_sha(&wt, "shelbi/task1"),
            before,
            "task branch must move after the rebase"
        );
        assert_eq!(
            head_sha(&wt),
            detached_workspace_head,
            "the handed-off workspace HEAD must remain untouched"
        );
        assert!(
            !wt.join("fix.txt").exists(),
            "the assigned workspace must not receive files from the probe rebase"
        );
        // The report pins the *post-rebase* tip — the SHA pr-merge must be
        // matched against, not the stale handoff commit.
        assert_eq!(
            report.head_sha,
            branch_sha(&wt, "shelbi/task1"),
            "head_sha must be the rebased branch tip"
        );
    }

    #[test]
    fn probe_reports_rebase_conflict_and_skips_checks() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        // (b) The default advanced on the same lines the task touched, so the
        // rebase conflicts. The probe must report `rebase_conflict` with the
        // conflicting file, abort the rebase, and NOT run the local checks.
        let (base, origin, wt) = setup_origin_and_worktree();

        // Task edits seed.txt.
        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("seed.txt"), "task side\n").unwrap();
        run_git(&wt, &["commit", "-q", "-am", "task edits seed"]);
        let task_head = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");
        let detached_workspace_head = head_sha(&wt);

        // Conflicting edit lands on origin/main.
        advance_origin_main(base.path(), &origin, "main edits seed", |r| {
            std::fs::write(r.join("seed.txt"), "main side\n").unwrap();
        });

        // A check that would obviously "pass" — proving the skip, not the
        // check result, is what suppresses it.
        let project = probe_project(base.path(), &["true"]);
        let task = probe_task("shelbi/task1");
        let report = probe_in_workflow(
            &project,
            None,
            &task,
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();

        assert!(
            report.rebase_conflict.conflicts,
            "expected a rebase conflict"
        );
        assert!(
            report.rebase_conflict.files.iter().any(|f| f == "seed.txt"),
            "conflict files should name seed.txt, got {:?}",
            report.rebase_conflict.files
        );
        assert!(
            report.local_checks.is_empty(),
            "local checks must be skipped on rebase conflict, got {:?}",
            report.local_checks
        );
        assert_eq!(report.head_sha, task_head);
        assert_eq!(branch_sha(&wt, "shelbi/task1"), task_head);
        assert_eq!(head_sha(&wt), detached_workspace_head);

        // The conflict and abort happened only in the temporary worktree.
        // The detached assigned workspace remains clean and unchanged.
        let status = Command::new("git")
            .current_dir(&wt)
            .args(["status", "--porcelain"])
            .output()
            .unwrap();
        assert!(
            status.stdout.is_empty(),
            "worktree must be clean after the aborted rebase"
        );
    }

    #[test]
    fn probe_on_current_worktree_does_not_rewrite_and_runs_checks() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        // (c) The worktree already contains the current default — fetch is a
        // no-op, the rebase is up-to-date, and the checks run immediately. We
        // prove "no extra work" by asserting HEAD is byte-for-byte unchanged
        // (no rewrite happened).
        let (base, _origin, wt) = setup_origin_and_worktree();

        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("work.txt"), "task work\n").unwrap();
        run_git(&wt, &["add", "-A"]);
        run_git(&wt, &["commit", "-q", "-m", "task work"]);
        let before = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");
        let detached_workspace_head = head_sha(&wt);

        let project = probe_project(base.path(), &["true"]);
        let task = probe_task("shelbi/task1");
        let report = probe_in_workflow(
            &project,
            None,
            &task,
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();

        assert!(
            !report.rebase_conflict.conflicts,
            "no conflict on a current worktree"
        );
        assert_eq!(
            report.local_checks.len(),
            1,
            "the check runs when up to date"
        );
        assert_eq!(report.local_checks[0].exit_code, 0);
        assert_eq!(
            branch_sha(&wt, "shelbi/task1"),
            before,
            "an up-to-date branch must not be rewritten"
        );
        assert_eq!(head_sha(&wt), detached_workspace_head);
        assert_eq!(
            report.head_sha, before,
            "head_sha must be the (unmoved) branch tip"
        );
    }

    #[cfg(unix)]
    #[test]
    fn reused_workspace_probe_copies_ignored_dependencies_for_workflow_checks() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        let (base, _origin, wt) = setup_origin_and_worktree();

        // Package metadata is tracked, while the installed dependency tree is
        // intentionally ignored just like site/node_modules in this repo.
        std::fs::create_dir_all(wt.join("site")).unwrap();
        std::fs::write(wt.join(".gitignore"), "site/node_modules/\n").unwrap();
        std::fs::write(wt.join("site/package.json"), "{\"private\":true}\n").unwrap();
        std::fs::write(wt.join("replacement.txt"), "replacement base\n").unwrap();
        run_git(
            &wt,
            &["add", ".gitignore", "site/package.json", "replacement.txt"],
        );
        run_git(&wt, &["commit", "-q", "-m", "add site package"]);
        run_git(&wt, &["push", "-q", "origin", "main"]);

        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("site/reviewed.txt"), "old reviewed task\n").unwrap();
        run_git(&wt, &["add", "site/reviewed.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "reviewed site task"]);
        let reviewed_head = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");

        // The freed slot now serves an unrelated replacement task with both
        // staged and unstaged work. Its ignored dependencies are the only
        // installed copy available to the old task's isolated probe.
        run_git(&wt, &["checkout", "-q", "-b", "replacement-task", "main"]);
        std::fs::write(wt.join("replacement.txt"), "replacement staged\n").unwrap();
        run_git(&wt, &["add", "replacement.txt"]);
        std::fs::write(wt.join("replacement.txt"), "replacement unstaged\n").unwrap();
        std::fs::write(wt.join("replacement-untracked.txt"), "untracked\n").unwrap();

        let dependency = wt.join("site/node_modules/review-tool");
        let dependency_bin = wt.join("site/node_modules/.bin");
        std::fs::create_dir_all(&dependency).unwrap();
        std::fs::create_dir_all(&dependency_bin).unwrap();
        let script = dependency.join("check-reviewed");
        std::fs::write(
            &script,
            "#!/bin/sh\n\
test \"$(cat reviewed.txt)\" = \"old reviewed task\" || exit 21\n\
test \"$(cat node_modules/review-tool/source-marker)\" = pristine || exit 22\n\
printf 'mutated only in probe\\n' > node_modules/review-tool/source-marker\n\
printf 'checked:%s\\n' \"$(git rev-parse HEAD)\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dependency.join("source-marker"), "pristine\n").unwrap();
        symlink(
            "../review-tool/check-reviewed",
            dependency_bin.join("review-tool"),
        )
        .unwrap();

        let replacement_branch_before =
            probe_git_stdout(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);
        let replacement_head_before = head_sha(&wt);
        let replacement_status_before =
            probe_git_stdout(&wt, &["status", "--porcelain=v1", "--untracked-files=all"]);
        let replacement_index_before = probe_git_stdout(&wt, &["diff", "--cached", "--binary"]);
        let replacement_diff_before = probe_git_stdout(&wt, &["diff", "--binary"]);
        let replacement_file_before = std::fs::read(wt.join("replacement.txt")).unwrap();
        let replacement_untracked_before =
            std::fs::read(wt.join("replacement-untracked.txt")).unwrap();
        let source_dependency_before = std::fs::read(dependency.join("source-marker")).unwrap();

        let project = probe_project(base.path(), &["exit 99"]);
        let mut workflow = shelbi_core::default_workflow();
        workflow.zen = Some(shelbi_core::WorkflowZenConfig {
            checks: Some(ZenChecks {
                local: vec!["cd site && test -L node_modules/.bin/review-tool && \
                     ./node_modules/.bin/review-tool"
                    .into()],
            }),
            ..Default::default()
        });
        let report = probe_in_workflow(
            &project,
            Some(&workflow),
            &probe_task("shelbi/task1"),
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();

        assert_eq!(report.head_sha, reviewed_head);
        assert_eq!(report.head_sha, branch_sha(&wt, "shelbi/task1"));
        assert_eq!(report.local_checks.len(), 1);
        assert_eq!(
            report.local_checks[0].exit_code, 0,
            "ignored dependency-backed check failed: {}",
            report.local_checks[0].output_tail
        );
        assert!(
            report.local_checks[0]
                .output_tail
                .contains(&format!("checked:{}", report.head_sha)),
            "check must run against the reviewed task commit: {}",
            report.local_checks[0].output_tail
        );

        // The dependency was an independent copy: the check's write did not
        // flow back through a symlink/hardlink into the replacement slot.
        assert_eq!(
            std::fs::read(dependency.join("source-marker")).unwrap(),
            source_dependency_before
        );
        assert!(
            std::fs::symlink_metadata(dependency_bin.join("review-tool"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "source .bin link should remain untouched"
        );
        assert_eq!(
            probe_git_stdout(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
            replacement_branch_before
        );
        assert_eq!(head_sha(&wt), replacement_head_before);
        assert_eq!(
            probe_git_stdout(&wt, &["status", "--porcelain=v1", "--untracked-files=all"]),
            replacement_status_before
        );
        assert_eq!(
            probe_git_stdout(&wt, &["diff", "--cached", "--binary"]),
            replacement_index_before
        );
        assert_eq!(
            probe_git_stdout(&wt, &["diff", "--binary"]),
            replacement_diff_before
        );
        assert_eq!(
            std::fs::read(wt.join("replacement.txt")).unwrap(),
            replacement_file_before
        );
        assert_eq!(
            std::fs::read(wt.join("replacement-untracked.txt")).unwrap(),
            replacement_untracked_before
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_probe_rejects_dependency_links_that_escape_the_checkout() {
        use std::os::unix::fs::symlink;

        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        let (base, _origin, wt) = setup_origin_and_worktree();

        let external = base.path().join("persistent-package-store");
        std::fs::create_dir_all(external.join("dir")).unwrap();
        let external_marker = external.join("marker");
        std::fs::write(&external_marker, "pristine\n").unwrap();

        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::write(wt.join("package.json"), "{\"private\":true}\n").unwrap();
        // The intermediate tracked link is outside node_modules, so it is not
        // independently subject to dependency-root validation. The copied
        // dependency link must still resolve it before applying `..`.
        symlink(external.join("dir"), wt.join("bridge")).unwrap();
        run_git(&wt, &["add", ".gitignore", "package.json", "bridge"]);
        run_git(&wt, &["commit", "-q", "-m", "reviewed package task"]);
        let reviewed_head = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");

        let dependencies = wt.join("node_modules");
        std::fs::create_dir_all(&dependencies).unwrap();
        symlink("../bridge/../marker", dependencies.join("escape-chain")).unwrap();

        let project = probe_project(base.path(), &["true"]);
        let err = probe_in_workflow(
            &project,
            None,
            &probe_task("shelbi/task1"),
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("resolves outside the isolated Zen checkout"),
            "{err}"
        );
        assert_eq!(
            std::fs::read_to_string(&external_marker).unwrap(),
            "pristine\n"
        );
        assert_eq!(branch_sha(&wt, "shelbi/task1"), reviewed_head);
        assert!(
            std::fs::symlink_metadata(dependencies.join("escape-chain"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "source dependency link must remain untouched"
        );
    }

    #[test]
    fn checkout_mutation_stops_later_local_checks() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        let (base, _origin, wt) = setup_origin_and_worktree();

        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("seed.txt"), "reviewed task\n").unwrap();
        run_git(&wt, &["add", "seed.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "reviewed task"]);
        let reviewed_head = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");

        let project = probe_project(
            base.path(),
            &[
                "printf 'tampered\\n' > seed.txt; printf 'leaked\\n' > probe-leak.txt",
                "printf 'ran\\n' > \"$CARGO_TARGET_DIR/zen-check-after-mutation\"",
            ],
        );
        let report = probe_in_workflow(
            &project,
            None,
            &probe_task("shelbi/task1"),
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();

        assert_eq!(report.head_sha, reviewed_head);
        assert_eq!(
            report.local_checks.len(),
            1,
            "no later check may observe state derived from changed reviewed content"
        );
        assert_eq!(
            report.local_checks[0].exit_code, 1,
            "a check that changes reviewed source must fail closed"
        );
        assert!(
            report.local_checks[0]
                .output_tail
                .contains("remaining local checks were skipped"),
            "{}",
            report.local_checks[0].output_tail
        );
        assert!(
            !base.path().join("target/zen-check-after-mutation").exists(),
            "the later check must not execute"
        );
        assert_eq!(branch_sha(&wt, "shelbi/task1"), reviewed_head);
    }

    #[test]
    fn isolated_probes_share_the_machine_cargo_target_cache() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        let (base, _origin, wt) = setup_origin_and_worktree();

        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("work.txt"), "task work\n").unwrap();
        run_git(&wt, &["add", "work.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "task work"]);
        detach_for_handoff(&wt, "shelbi/task1");

        let cache_check = "mkdir -p \"$CARGO_TARGET_DIR\"; \
if test -f \"$CARGO_TARGET_DIR/zen-probe-sentinel\"; then echo cache:warm; \
else echo cache:cold; : > \"$CARGO_TARGET_DIR/zen-probe-sentinel\"; fi; \
printf 'target:%s\\n' \"$CARGO_TARGET_DIR\"";
        let project = probe_project(base.path(), &[cache_check]);
        let task = probe_task("shelbi/task1");

        let first = probe_in_workflow(
            &project,
            None,
            &task,
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();
        assert_eq!(first.local_checks[0].exit_code, 0);
        assert!(
            first.local_checks[0].output_tail.contains("cache:cold"),
            "{}",
            first.local_checks[0].output_tail
        );

        let second = probe_in_workflow(
            &project,
            None,
            &task,
            "shelbi/task1",
            RebasePolicy::RebaseOntoDefault,
        )
        .unwrap();
        assert_eq!(second.local_checks[0].exit_code, 0);
        assert!(
            second.local_checks[0].output_tail.contains("cache:warm"),
            "{}",
            second.local_checks[0].output_tail
        );

        let shared_target = base.path().join("target");
        assert!(shared_target.join("zen-probe-sentinel").is_file());
        assert!(
            second.local_checks[0]
                .output_tail
                .contains(&format!("target:{}", shared_target.display())),
            "{}",
            second.local_checks[0].output_tail
        );
        assert!(
            !shared_target.starts_with(&wt),
            "shared cache must live outside the replacement workspace"
        );
        let worktrees = probe_git_stdout(&wt, &["worktree", "list", "--porcelain"]);
        assert_eq!(
            worktrees
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count(),
            1,
            "probe cleanup must not delete the shared cache or retain temp worktrees:\n{worktrees}"
        );
    }

    #[test]
    fn checkout_cannot_attach_between_probe_scan_and_ref_advance() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        let (_base, _origin, wt) = setup_origin_and_worktree();

        // The reviewed task ref is durable but no longer checked out after
        // handoff. A separate branch models the isolated probe's rewritten
        // result without moving the task ref yet.
        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("reviewed.txt"), "reviewed task\n").unwrap();
        run_git(&wt, &["add", "reviewed.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "reviewed task"]);
        let expected = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");

        run_git(&wt, &["checkout", "-q", "-b", "probed-result"]);
        std::fs::write(wt.join("probed-only.txt"), "exact probed tree\n").unwrap();
        run_git(&wt, &["add", "probed-only.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "probed result"]);
        let proposed = head_sha(&wt);
        assert_eq!(branch_sha(&wt, "shelbi/task1"), expected);

        // A missing second workspace will attempt the real pane-recovery
        // named-attach path against this same repository.
        let machine = Machine {
            name: "nested-hub".into(),
            kind: MachineKind::Local,
            work_dir: wt.clone(),
            host: None,
            tags: Vec::new(),
            forward: None,
        };
        let workspace = WorkspaceSpec {
            name: "ws2".into(),
            machine: machine.name.clone(),
            runner: "claude".into(),
            tags: Vec::new(),
            slot: None,
        };
        let attached_worktree = crate::workspace::workspace_worktree(&machine, &workspace);
        assert!(!attached_worktree.exists());

        let (scanned_tx, scanned_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        let probe_repo = wt.clone();
        let probe_expected = expected.clone();
        let probe_proposed = proposed.clone();
        let probe_thread = std::thread::spawn(move || {
            finalize_probed_task_ref_after_scan(
                "probe-test",
                &Host::Local,
                &probe_repo,
                "shelbi/task1",
                "refs/heads/shelbi/task1",
                &probe_expected,
                &probe_proposed,
                || {
                    scanned_tx.send(()).unwrap();
                    resume_rx.recv().unwrap();
                },
            )
        });

        // Wait until the probe has observed no checkout while holding the Git
        // lock, then start the second workspace attach at that exact point.
        scanned_rx.recv().unwrap();
        let (attempting_tx, attempting_rx) = std::sync::mpsc::channel();
        let (completed_tx, completed_rx) = std::sync::mpsc::channel();
        let attach_machine = machine.clone();
        let attach_workspace = workspace.clone();
        let attach_thread = std::thread::spawn(move || {
            attempting_tx.send(()).unwrap();
            let result = crate::workspace::ensure_workspace_worktree(
                "probe-test",
                &attach_machine,
                &attach_workspace,
                "shelbi/task1",
                "main",
            );
            completed_tx.send(()).unwrap();
            result
        });
        attempting_rx.recv().unwrap();

        // The attach must not finish while the probe is paused between scan
        // and CAS. Without the shared lock it checks out `expected` here, then
        // the CAS silently leaves its symbolic HEAD at `proposed` while its
        // index and files still describe `expected`.
        let completed_while_paused = completed_rx.recv_timeout(Duration::from_millis(500));
        resume_tx.send(()).unwrap();

        probe_thread.join().unwrap().unwrap();
        attach_thread.join().unwrap().unwrap();
        assert!(
            matches!(
                completed_while_paused,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout)
            ),
            "the second workspace attached while probe scan/CAS was paused"
        );

        assert_eq!(branch_sha(&wt, "shelbi/task1"), proposed);
        assert_eq!(
            probe_git_stdout(&attached_worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "shelbi/task1"
        );
        assert_eq!(head_sha(&attached_worktree), proposed);
        assert_eq!(
            std::fs::read_to_string(attached_worktree.join("probed-only.txt")).unwrap(),
            "exact probed tree\n"
        );
        assert_eq!(
            probe_git_stdout(&attached_worktree, &["status", "--porcelain"]),
            "",
            "the attached worktree's index/files must match the advanced ref"
        );
    }

    #[test]
    fn advancing_probed_ref_rejects_a_concurrent_branch_move() {
        let _lock = crate::test_lock::acquire();
        let _home = ProbeHomeGuard::install();
        let (base, _origin, wt) = setup_origin_and_worktree();
        run_git(&wt, &["checkout", "-q", "-b", "shelbi/task1"]);
        std::fs::write(wt.join("work.txt"), "task work\n").unwrap();
        run_git(&wt, &["add", "work.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "task work"]);
        let expected = branch_sha(&wt, "shelbi/task1");
        detach_for_handoff(&wt, "shelbi/task1");

        run_git(&wt, &["checkout", "-q", "-b", "concurrent", "main"]);
        std::fs::write(wt.join("concurrent.txt"), "concurrent work\n").unwrap();
        run_git(&wt, &["add", "concurrent.txt"]);
        run_git(&wt, &["commit", "-q", "-m", "concurrent update"]);
        let concurrent = head_sha(&wt);
        let proposed = probe_git_stdout(&wt, &["rev-parse", "main"]);
        run_git(
            &wt,
            &[
                "update-ref",
                "refs/heads/shelbi/task1",
                &concurrent,
                &expected,
            ],
        );

        let err = finalize_probed_task_ref(
            "probe-test",
            &Host::Local,
            &wt,
            "shelbi/task1",
            "refs/heads/shelbi/task1",
            &expected,
            &proposed,
        )
        .unwrap_err()
        .to_string();
        assert!(err.contains("moved"), "{err}");
        assert!(err.contains(&expected), "{err}");
        assert!(err.contains(&concurrent), "{err}");
        assert_eq!(branch_sha(&wt, "shelbi/task1"), concurrent);
        drop(base);
    }
}

// ===========================================================================
// Backlog scan — mechanical eligibility for Zen auto-promotion
// ===========================================================================
//
// `mechanically_eligible` answers a narrow question: which backlog task ids
// are safe to lift to `todo` purely from a state-machine standpoint? It is
// *not* the final say — the orchestrator's prompt layers judgment about
// "type of work", "recent issue follow-up", and "larger body of work the
// user kicked off" on top of this list. That separation is deliberate: the
// rules here are mechanical (and Rust-tested); the rules there are
// user-tunable (and live in the prompt).

/// Backlog task ids that are mechanically eligible for Zen auto-promotion,
/// sorted by priority (lower number = higher priority). See module docs for
/// the rules — and what we *don't* check.
///
/// I/O: loads every task file in the project plus the events log. Returns an
/// empty list when the backlog is empty or every backlog task is blocked.
pub fn mechanically_eligible(project: &Project) -> Result<Vec<String>> {
    let tasks = shelbi_state::list_tasks(&project.name)?;
    let demoted = read_demoted_task_ids()?;
    Ok(mechanically_eligible_from(&tasks, &demoted))
}

/// Pure-logic core of [`mechanically_eligible`]. Split out so the unit
/// tests can drive it with in-memory fixtures without touching disk or
/// `SHELBI_HOME`.
pub fn mechanically_eligible_from(
    tasks: &[shelbi_state::TaskFile],
    demoted: &std::collections::HashSet<String>,
) -> Vec<String> {
    let columns: std::collections::HashMap<String, Column> = tasks
        .iter()
        .map(|tf| (tf.task.id.clone(), tf.task.column.clone()))
        .collect();

    let in_flight_bodies: Vec<&str> = tasks
        .iter()
        .filter(|tf| tf.task.column == Column::in_progress())
        .map(|tf| tf.body.as_str())
        .collect();

    let mut candidates: Vec<&Task> = tasks
        .iter()
        .filter(|tf| tf.task.column == Column::backlog())
        .filter(|tf| !tf.task.is_blocked(&columns))
        .filter(|tf| !zen_disabled(&tf.task))
        .filter(|tf| !demoted.contains(&tf.task.id))
        .filter(|tf| !file_overlaps_in_flight(&tf.body, &in_flight_bodies))
        .map(|tf| &tf.task)
        .collect();

    // Stable secondary sort by id so equal-priority ties have a deterministic
    // order — matters for the CLI wrapper that prints one ID per line.
    candidates.sort_by(|a, b| a.priority.cmp(&b.priority).then_with(|| a.id.cmp(&b.id)));
    candidates.into_iter().map(|t| t.id.clone()).collect()
}

/// True iff the task's frontmatter explicitly opts out via `zen.enabled:
/// false`. `None` (no override) and `Some(true)` both count as "follow
/// project default" — which, for this gate, means "eligible".
fn zen_disabled(task: &Task) -> bool {
    matches!(task.zen.as_ref().and_then(|z| z.enabled), Some(false))
}

/// File-overlap heuristic: extract path-like tokens from `candidate_body`
/// and return true iff any token appears as a substring in any in-flight
/// task body. Asymmetric on purpose — the candidate is the new arrival we
/// might queue behind something already being touched.
fn file_overlaps_in_flight(candidate_body: &str, in_flight_bodies: &[&str]) -> bool {
    let tokens = extract_path_tokens(candidate_body);
    tokens.iter().any(|tok| {
        in_flight_bodies
            .iter()
            .any(|body| body.contains(tok.as_str()))
    })
}

/// Pull out tokens that look like file paths. A "path-like" token is a
/// run of `[A-Za-z0-9._/-]` that contains at least one `/` and ends in a
/// `.<ext>` segment of 1–8 word characters. This catches the common cases
/// the spec calls out (`crates/shelbi-tui/src/app.rs`,
/// `site/components/Footer.tsx`) without dragging in unrelated dotted
/// identifiers like `task.zen.enabled`. Markdown wrappers (backticks,
/// brackets) drop out automatically because they're not in the path
/// alphabet.
pub fn extract_path_tokens(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    for raw in
        body.split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_'))
    {
        if let Some(tok) = canonical_path_token(raw) {
            out.push(tok);
        }
    }
    out
}

fn canonical_path_token(raw: &str) -> Option<String> {
    // Only trim the *trailing* end. Stripping leading punctuation would
    // eat the slash in tokens like `./foo.rs` and lose the path signal.
    let trimmed = raw.trim_end_matches(['.', '/', '-', '_']);
    if trimmed.is_empty() || !trimmed.contains('/') {
        return None;
    }
    let last_seg = trimmed.rsplit('/').next()?;
    let (_, ext) = last_seg.rsplit_once('.')?;
    if ext.is_empty() || ext.len() > 8 {
        return None;
    }
    if !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(trimmed.to_string())
}

/// Read every `task=<id> todo -> backlog reason=user:*` line from the
/// events log and collect the demoted task ids. Once a user demotes a
/// task, Zen never re-promotes it — see the spec.
fn read_demoted_task_ids() -> Result<std::collections::HashSet<String>> {
    let path = shelbi_state::events_log_path()?;
    if !path.exists() {
        return Ok(std::collections::HashSet::new());
    }
    let text = std::fs::read_to_string(&path).map_err(Error::Io)?;
    Ok(parse_demoted_task_ids(&text))
}

/// Pure scan over event-log text. Each matching line contributes one id.
pub fn parse_demoted_task_ids(log: &str) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for line in log.lines() {
        if let Some(id) = parse_user_demotion_line(line) {
            out.insert(id.to_string());
        }
    }
    out
}

/// Returns the task id from a line of the form
/// `<ts> task=<id> todo -> backlog reason=user:<rest>`, or `None` if the
/// line is anything else (workspace events, other transitions, non-user
/// reasons, etc.).
fn parse_user_demotion_line(line: &str) -> Option<&str> {
    // Cheap prefilter — most lines aren't demotions.
    if !line.contains(" todo -> backlog ") {
        return None;
    }
    if !line.contains(" reason=user:") {
        return None;
    }
    let after_task = line.split(" task=").nth(1)?;
    let id_end = after_task.find(' ')?;
    let id = &after_task[..id_end];
    // Re-anchor the transition + reason check to the slice *after* the id,
    // so an unrelated " task=" elsewhere in the line can't fool us.
    let rest = &after_task[id_end..];
    if !rest.starts_with(" todo -> backlog ") {
        return None;
    }
    let reason = rest.split(" reason=").nth(1)?;
    if !reason.starts_with("user:") {
        return None;
    }
    Some(id)
}

#[cfg(test)]
mod scan_tests {
    use super::*;
    use chrono::Utc;
    use shelbi_core::Column;
    use shelbi_state::TaskFile;
    use std::collections::HashSet;

    fn task(id: &str, column: Column, priority: u32, deps: &[&str]) -> Task {
        Task {
            id: id.into(),
            title: id.into(),
            column,
            priority,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: deps.iter().map(|s| s.to_string()).collect(),
            prefers_machine: None,
            zen: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            params: std::collections::BTreeMap::new(),
        }
    }

    fn tf(task: Task, body: &str) -> TaskFile {
        TaskFile {
            task,
            body: body.into(),
        }
    }

    #[test]
    fn empty_backlog_returns_empty() {
        let tasks = vec![
            tf(task("done-a", Column::done(), 0, &[]), ""),
            tf(task("todo-a", Column::todo(), 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty());
    }

    #[test]
    fn returns_eligible_in_priority_order() {
        let tasks = vec![
            tf(task("b", Column::backlog(), 2, &[]), ""),
            tf(task("a", Column::backlog(), 0, &[]), ""),
            tf(task("c", Column::backlog(), 1, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["a", "c", "b"]);
    }

    #[test]
    fn excludes_blocked_by_unfinished_deps() {
        let tasks = vec![
            tf(task("blocked", Column::backlog(), 0, &["other"]), ""),
            tf(task("other", Column::todo(), 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn returns_empty_when_every_backlog_task_is_blocked() {
        // Mix of blocked-by-todo and blocked-by-in-progress. None can move.
        let tasks = vec![
            tf(task("a", Column::backlog(), 0, &["x"]), ""),
            tf(task("b", Column::backlog(), 1, &["y"]), ""),
            tf(task("x", Column::todo(), 0, &[]), ""),
            tf(task("y", Column::in_progress(), 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn done_deps_unblock_a_task() {
        let tasks = vec![
            tf(task("waiting", Column::backlog(), 0, &["dep"]), ""),
            tf(task("dep", Column::done(), 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["waiting"]);
    }

    #[test]
    fn excludes_zen_enabled_false() {
        let mut t = task("opt-out", Column::backlog(), 0, &[]);
        t.zen = Some(shelbi_core::TaskZenConfig {
            enabled: Some(false),
            ..Default::default()
        });
        let tasks = vec![tf(t, "")];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn zen_enabled_true_or_unset_is_eligible() {
        let mut opt_in = task("opt-in", Column::backlog(), 0, &[]);
        opt_in.zen = Some(shelbi_core::TaskZenConfig {
            enabled: Some(true),
            ..Default::default()
        });
        let unset = task("unset", Column::backlog(), 1, &[]);
        let tasks = vec![tf(opt_in, ""), tf(unset, "")];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["opt-in", "unset"]);
    }

    #[test]
    fn excludes_previously_user_demoted() {
        let tasks = vec![
            tf(task("demoted", Column::backlog(), 0, &[]), ""),
            tf(task("fresh", Column::backlog(), 1, &[]), ""),
        ];
        let mut demoted = HashSet::new();
        demoted.insert("demoted".to_string());
        let got = mechanically_eligible_from(&tasks, &demoted);
        assert_eq!(got, vec!["fresh"]);
    }

    #[test]
    fn file_overlap_with_in_progress_excludes_candidate() {
        // Both backlog tasks mention crates/shelbi-cli/src/main.rs; one of
        // those tasks is already in_progress, so the *other* must be
        // skipped (the spec's worked example).
        let body_a = "Refactor `crates/shelbi-cli/src/main.rs` to split the dispatch path.";
        let body_b = "Add tests covering crates/shelbi-cli/src/main.rs error paths.";
        let tasks = vec![
            tf(task("in-flight", Column::in_progress(), 0, &[]), body_a),
            tf(task("candidate", Column::backlog(), 0, &[]), body_b),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn file_overlap_does_not_trigger_on_unrelated_paths() {
        let in_flight_body = "Working on `crates/shelbi-tui/src/app.rs`.";
        let candidate_body = "Touch `crates/shelbi-state/src/lib.rs` only.";
        let tasks = vec![
            tf(
                task("in-flight", Column::in_progress(), 0, &[]),
                in_flight_body,
            ),
            tf(task("candidate", Column::backlog(), 0, &[]), candidate_body),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["candidate"]);
    }

    #[test]
    fn does_not_cap_result_count() {
        // Ten eligible tasks; we get all ten back. The orchestrator's
        // judgment layer picks how many to actually promote.
        let tasks: Vec<TaskFile> = (0..10)
            .map(|i| tf(task(&format!("t-{i}"), Column::backlog(), i, &[]), ""))
            .collect();
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got.len(), 10);
    }

    // --- helpers --------------------------------------------------------

    #[test]
    fn path_token_extraction_picks_up_typical_paths() {
        let body = "Edit `crates/shelbi-tui/src/app.rs` and \
                    `site/components/Footer.tsx`. Also: bare/path.rs.";
        let toks = extract_path_tokens(body);
        assert!(toks.iter().any(|t| t == "crates/shelbi-tui/src/app.rs"));
        assert!(toks.iter().any(|t| t == "site/components/Footer.tsx"));
        assert!(toks.iter().any(|t| t == "bare/path.rs"));
    }

    #[test]
    fn path_token_extraction_ignores_dotted_identifiers() {
        // `task.zen.enabled` is a config key, not a file path — no slash.
        let toks = extract_path_tokens("Set task.zen.enabled to false.");
        assert!(toks.is_empty(), "{toks:?}");
    }

    #[test]
    fn path_token_extraction_handles_dot_slash_prefix() {
        // `./foo.rs` is uncommon in markdown but still a path — the leading
        // `./` shouldn't disqualify it.
        let toks = extract_path_tokens("Edit ./foo.rs please.");
        assert!(toks.iter().any(|t| t == "./foo.rs"), "{toks:?}");
    }

    #[test]
    fn path_token_extraction_strips_trailing_period() {
        // `bare/path.rs.` at end of a sentence loses the trailing period.
        let toks = extract_path_tokens("Touch bare/path.rs.");
        assert!(toks.iter().any(|t| t == "bare/path.rs"), "{toks:?}");
        assert!(!toks.iter().any(|t| t.ends_with('.')), "{toks:?}");
    }

    #[test]
    fn path_token_extraction_ignores_extensionless_words() {
        // No `.<ext>` segment after the last `/` → drop.
        let toks = extract_path_tokens("See README under crates/shelbi-core directory.");
        assert!(toks.is_empty(), "{toks:?}");
    }

    #[test]
    fn parse_demoted_task_ids_matches_user_demotions_only() {
        let log = "\
2026-06-24T00:00:00+00:00 workspace=alpha none -> working
2026-06-24T00:01:00+00:00 task=foo backlog -> todo reason=zen:auto-promote
2026-06-24T00:02:00+00:00 task=foo todo -> backlog reason=user:cli
2026-06-24T00:03:00+00:00 task=bar todo -> backlog reason=zen:rollback
2026-06-24T00:04:00+00:00 task=baz todo -> in_progress reason=user:cli:start
2026-06-24T00:05:00+00:00 task=qux todo -> backlog reason=user:tui
";
        let demoted = parse_demoted_task_ids(log);
        assert!(demoted.contains("foo"));
        assert!(demoted.contains("qux"));
        assert!(!demoted.contains("bar"));
        assert!(!demoted.contains("baz"));
        assert_eq!(demoted.len(), 2);
    }

    #[test]
    fn parse_demoted_task_ids_handles_empty_log() {
        assert!(parse_demoted_task_ids("").is_empty());
    }

    #[test]
    fn does_not_inspect_body_for_judgment_signals() {
        // Wording the spec explicitly tells us NOT to gate on — task type
        // hints, "recent issue" language, kickoff-context phrases. None of
        // these should keep the task out of the eligible list; the
        // orchestrator decides what to do with the signal.
        let bodies = [
            "Quick docs typo.",
            "Follow-up to the auth incident we just shipped a hotfix for.",
            "Phase 3 of the kickoff John kicked off yesterday.",
        ];
        let tasks: Vec<TaskFile> = bodies
            .iter()
            .enumerate()
            .map(|(i, body)| {
                tf(
                    task(&format!("t-{i}"), Column::backlog(), i as u32, &[]),
                    body,
                )
            })
            .collect();
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["t-0", "t-1", "t-2"]);
    }
}

// ===========================================================================
// Dry-run preview — what would Zen do, without doing it
// ===========================================================================
//
// `dry_run_tick` runs the same two non-publishing steps Zen Mode runs each loop:
//
// 1. Scan the backlog for mechanically-eligible auto-promotion candidates.
// 2. Probe every task currently in `review` and apply the default mechanical
//    bar (the thresholds documented in the orchestrator prompt template).
//
// It returns one `DryRunDecision` per finding so the CLI can log "would
// have …" without changing durable task, board, PR, or branch state. The
// orchestrator's judgment layer (the auto-promote categories in the prompt)
// is *not* simulated — that requires an LLM. The decisions for backlog
// candidates make this explicit by labelling them `WouldConsiderAutoPromote`
// rather than `WouldAutoPromote`.

/// Default merge-conditions thresholds — mirror the values in the
/// orchestrator prompt template (`default_orchestrator.md.template`,
/// "Merge conditions" section). The prompt is the source of truth for
/// live Zen runs (the user can tune it per project); the dry-run uses
/// these defaults to give an honest preview of what the out-of-the-box
/// policy would do.
pub const DRYRUN_MAX_DIFF_FILES: usize = 30;
pub const DRYRUN_MAX_DIFF_LINES: usize = 2000;

/// One simulated decision the live Zen loop would have taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DryRunDecision {
    pub action: DryRunAction,
    pub task_id: String,
    /// Short, single-token reason (whitespace already collapsed) suitable
    /// for the `detail=` field of an events.log line.
    pub detail: String,
    /// Human-readable explanation for stdout + the dedicated log.
    pub explanation: String,
}

impl DryRunDecision {
    /// Stable key for run-local deduplication — same `(action, task_id,
    /// detail)` triple shouldn't be re-logged on every tick.
    pub fn dedup_key(&self) -> String {
        format!("{}|{}|{}", self.action.as_str(), self.task_id, self.detail)
    }

    /// One-line stdout/log shape the spec calls for:
    /// `zen-dryrun: would have <action> <task> because <explanation>`.
    pub fn as_line(&self) -> String {
        format!(
            "zen-dryrun: would have {action} {task} because {why}",
            action = self.action.verb(),
            task = self.task_id,
            why = self.explanation,
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryRunAction {
    /// Backlog task is mechanically eligible — live Zen would surface it
    /// to the auto-promote judgment layer.
    ConsiderAutoPromote,
    /// In-review task passes every mechanical gate — live Zen would have
    /// kicked off the PR / merge flow.
    Merge,
    /// In-review task fails at least one mechanical gate — live Zen
    /// would have left it for the user with the gate's reason.
    BlockMerge,
}

impl DryRunAction {
    pub fn as_str(self) -> &'static str {
        match self {
            DryRunAction::ConsiderAutoPromote => "consider-auto-promote",
            DryRunAction::Merge => "merge",
            DryRunAction::BlockMerge => "block-merge",
        }
    }

    /// Verb used in the user-facing `would have <verb> <task>` line.
    pub fn verb(self) -> &'static str {
        match self {
            DryRunAction::ConsiderAutoPromote => "considered auto-promoting",
            DryRunAction::Merge => "merged",
            DryRunAction::BlockMerge => "blocked merge of",
        }
    }
}

/// Run one non-publishing Zen pass for `project` and return every decision the
/// live loop would have made. Task, board, PR, and branch state remain
/// unchanged; probes use transient isolated worktrees for accurate local
/// checks. Probes are best-effort: an error is surfaced as a `BlockMerge`
/// decision labelled `probe-failed` so the user still sees the task, rather
/// than silently dropping it.
///
/// The merge bar is **action-based**: for each task in a `handoff`-
/// category status, we look up the task's workflow and apply the bar
/// only when the workflow declares a `merge` action on an outgoing
/// transition from that status. A workflow with no `transitions:` block
/// at all (e.g., the migrated `default.yaml` on existing projects) falls
/// back to the legacy "Review fires the bar" semantic — see
/// [`Workflow::fires_merge_bar`]. Tasks in workflows whose transitions
/// explicitly *don't* declare merge (a pure-bookkeeping research
/// workflow, say) sit in their handoff status without ever tripping the
/// dry-run preview.
///
/// Iteration is by **category**, not by hardcoded [`Column::review()`]:
/// a custom workflow whose handoff status is named `QA` or
/// `Awaiting Sign-off` (instead of `Review`) trips the same bar.
pub fn dry_run_tick(project: &Project) -> Result<Vec<DryRunDecision>> {
    let mut decisions = Vec::new();

    // 1. Backlog scan — mechanical eligibility only. The orchestrator's
    //    judgment categories aren't simulated; we just surface what live
    //    Zen would *consider*.
    for task_id in mechanically_eligible(project)? {
        decisions.push(DryRunDecision {
            action: DryRunAction::ConsiderAutoPromote,
            task_id,
            detail: "mechanically-eligible".into(),
            explanation: "mechanically eligible (orchestrator judgment still needed)".into(),
        });
    }

    // 2. Handoff-category probes — action-based bar gated by the task's
    //    workflow. Filter is on the resolved workflow status's category
    //    rather than `Column::review()` so custom workflows with renamed
    //    handoff statuses still get probed.
    for tf in shelbi_state::list_tasks(&project.name)? {
        let workflow = load_task_workflow(&project.name, &tf.task);
        let workflow_ref = workflow.as_ref();
        let status = workflow_ref.and_then(|w| resolve_task_status(&tf.task, w));
        let category = status
            .map(|s| s.category)
            .unwrap_or_else(|| tf.task.column.category());
        if category != StatusCategory::Handoff {
            continue;
        }
        let status_id = status
            .map(|s| s.id.as_str())
            .unwrap_or_else(|| tf.task.column.as_str());
        let fires_bar = workflow_ref
            .map(|w| w.fires_merge_bar(status_id))
            .unwrap_or(true);
        if !fires_bar {
            // Workflow explicitly declares transitions but none from this
            // status fire `merge` — skip silently. The task lives in this
            // workflow for bookkeeping only.
            continue;
        }
        let branch = branch::branch_name_for_task(project, workflow_ref, &tf.task)?;
        // Dry-run never fetches or rewrites the task ref. `AsIs` probes its
        // exact commit in a transient isolated worktree.
        match probe_in_workflow(project, workflow_ref, &tf.task, &branch, RebasePolicy::AsIs) {
            Ok(report) => {
                decisions.push(evaluate_probe(&tf.task.id, &report));
            }
            Err(e) => {
                // Don't let one bad probe silence the rest of the pass.
                // Surface it so the user knows the dry-run couldn't speak
                // to this task.
                decisions.push(DryRunDecision {
                    action: DryRunAction::BlockMerge,
                    task_id: tf.task.id.clone(),
                    detail: "probe-failed".into(),
                    explanation: format!("probe failed: {e}"),
                });
            }
        }
    }

    Ok(decisions)
}

/// Resolve which workflow status `task` currently lives in. Mirrors the
/// TUI's `resolve_task_status` (kanban.rs) so generic code can ask
/// "what status is this task in?" without assuming the task's column
/// is `Review` for a handoff-category resolution.
///
/// Resolution order:
///
/// 1. **Id match** — workflow declares a status whose `id` equals
///    `task.column.as_str()` (`backlog` / `todo` /
///    `in-progress` / `review` / `done`). Covers the default workflow
///    and any custom workflow that reuses the canonical ids.
/// 2. **Category match** — first status in the workflow whose category
///    equals `task.column.category()`. Lets a custom workflow that
///    renamed `Review` to `QA` still resolve to a handoff status.
/// 3. **None** — the workflow declares no compatible status. Callers
///    fall back to column-level metadata.
fn resolve_task_status<'w>(task: &Task, workflow: &'w Workflow) -> Option<&'w WorkflowStatus> {
    let canonical = task.column.as_str();
    if let Some(s) = workflow.status(canonical) {
        return Some(s);
    }
    let cat = task.column.category();
    workflow.statuses.iter().find(|s| s.category == cat)
}

/// Best-effort load of a task's workflow definition. Returns `None`
/// when the workflow file can't be read or fails validation — the
/// caller should treat that as "fall back to project-level config".
/// Loading is best-effort because the dry-run loop runs against live
/// state, and a transient typo in a workflow YAML shouldn't kill the
/// whole preview pass for unrelated tasks.
fn load_task_workflow(project: &str, task: &Task) -> Option<Workflow> {
    let project_yaml = shelbi_state::load_project(project).ok()?;
    shelbi_state::load_task_workflow(project, &project_yaml, task).ok()
}

/// Apply the default merge-conditions bar to a probe report. Returns a
/// `Merge` decision if every gate passes, a `BlockMerge` decision tagged
/// with the first failing gate otherwise.
///
/// Gate order matches the prompt template — first failure wins so the
/// user sees the same single reason live Zen would emit.
pub fn evaluate_probe(task_id: &str, report: &ProbeReport) -> DryRunDecision {
    if let Some(failed) = report.local_checks.iter().find(|c| c.exit_code != 0) {
        return DryRunDecision {
            action: DryRunAction::BlockMerge,
            task_id: task_id.to_string(),
            detail: "failed-checks".into(),
            explanation: format!(
                "local check failed: `{}` (exit {})",
                failed.command, failed.exit_code
            ),
        };
    }
    if report.rebase_conflict.conflicts {
        let files = if report.rebase_conflict.files.is_empty() {
            "(unknown files)".to_string()
        } else {
            report.rebase_conflict.files.join(",")
        };
        return DryRunDecision {
            action: DryRunAction::BlockMerge,
            task_id: task_id.to_string(),
            detail: "rebase-conflict".into(),
            explanation: format!("rebase conflict in: {files}"),
        };
    }
    if report.merge_conflict.conflicts {
        let files = if report.merge_conflict.files.is_empty() {
            "(unknown files)".to_string()
        } else {
            report.merge_conflict.files.join(",")
        };
        return DryRunDecision {
            action: DryRunAction::BlockMerge,
            task_id: task_id.to_string(),
            detail: "merge-conflict".into(),
            explanation: format!("merge conflict in: {files}"),
        };
    }
    let total_lines = report.diff_size.lines_added + report.diff_size.lines_removed;
    if report.diff_size.files > DRYRUN_MAX_DIFF_FILES || total_lines > DRYRUN_MAX_DIFF_LINES {
        return DryRunDecision {
            action: DryRunAction::BlockMerge,
            task_id: task_id.to_string(),
            detail: "diff-too-large".into(),
            explanation: format!(
                "diff too large ({} files / {} lines; max {} files / {} lines)",
                report.diff_size.files, total_lines, DRYRUN_MAX_DIFF_FILES, DRYRUN_MAX_DIFF_LINES,
            ),
        };
    }
    if !report.danger_paths.matched.is_empty() {
        return DryRunDecision {
            action: DryRunAction::BlockMerge,
            task_id: task_id.to_string(),
            detail: "danger-path".into(),
            explanation: format!(
                "danger paths touched: {}",
                report.danger_paths.matched.join(",")
            ),
        };
    }
    DryRunDecision {
        action: DryRunAction::Merge,
        task_id: task_id.to_string(),
        detail: "all-gates-passed".into(),
        explanation: format!(
            "all gates passed (checks ok, no conflict, {} files / {} lines, no danger paths)",
            report.diff_size.files, total_lines
        ),
    }
}

#[cfg(test)]
mod dry_run_tests {
    use super::*;

    fn ok_report() -> ProbeReport {
        ProbeReport {
            head_sha: "deadbeef".into(),
            local_checks: vec![LocalCheck {
                command: "cargo test".into(),
                exit_code: 0,
                duration_ms: 100,
                output_tail: String::new(),
            }],
            merge_conflict: ConflictProbe::default(),
            rebase_conflict: ConflictProbe::default(),
            diff_size: DiffSize {
                files: 3,
                lines_added: 40,
                lines_removed: 5,
            },
            danger_paths: DangerPaths::default(),
        }
    }

    #[test]
    fn clean_probe_yields_merge_decision() {
        let d = evaluate_probe("t", &ok_report());
        assert_eq!(d.action, DryRunAction::Merge);
        assert_eq!(d.task_id, "t");
        assert!(d.explanation.contains("all gates passed"));
    }

    #[test]
    fn failing_check_blocks_merge_with_check_detail() {
        let mut r = ok_report();
        r.local_checks[0].exit_code = 7;
        let d = evaluate_probe("t", &r);
        assert_eq!(d.action, DryRunAction::BlockMerge);
        assert_eq!(d.detail, "failed-checks");
        assert!(d.explanation.contains("cargo test"));
        assert!(d.explanation.contains("exit 7"));
    }

    #[test]
    fn merge_conflict_blocks_merge_with_files() {
        let mut r = ok_report();
        r.merge_conflict = ConflictProbe {
            conflicts: true,
            files: vec!["src/a.rs".into(), "src/b.rs".into()],
        };
        let d = evaluate_probe("t", &r);
        assert_eq!(d.action, DryRunAction::BlockMerge);
        assert_eq!(d.detail, "merge-conflict");
        assert!(d.explanation.contains("src/a.rs"));
        assert!(d.explanation.contains("src/b.rs"));
    }

    #[test]
    fn rebase_conflict_blocks_merge_with_files() {
        let mut r = ok_report();
        r.rebase_conflict = ConflictProbe {
            conflicts: true,
            files: vec!["src/lib.rs".into()],
        };
        let d = evaluate_probe("t", &r);
        assert_eq!(d.action, DryRunAction::BlockMerge);
        assert_eq!(d.detail, "rebase-conflict");
        assert!(d.explanation.contains("src/lib.rs"));
    }

    #[test]
    fn rebase_conflict_outranks_merge_conflict() {
        // A rebase that couldn't even complete is the more fundamental
        // blocker — surface it before the (now meaningless) merge-tree probe.
        let mut r = ok_report();
        r.rebase_conflict.conflicts = true;
        r.merge_conflict.conflicts = true;
        let d = evaluate_probe("t", &r);
        assert_eq!(d.detail, "rebase-conflict");
    }

    #[test]
    fn oversize_diff_blocks_merge() {
        let mut r = ok_report();
        r.diff_size.files = DRYRUN_MAX_DIFF_FILES + 1;
        let d = evaluate_probe("t", &r);
        assert_eq!(d.action, DryRunAction::BlockMerge);
        assert_eq!(d.detail, "diff-too-large");

        let mut r = ok_report();
        r.diff_size.lines_added = DRYRUN_MAX_DIFF_LINES + 1;
        let d = evaluate_probe("t", &r);
        assert_eq!(d.action, DryRunAction::BlockMerge);
        assert_eq!(d.detail, "diff-too-large");
    }

    #[test]
    fn danger_path_match_blocks_merge() {
        let mut r = ok_report();
        r.danger_paths.matched = vec![".github/workflows/ci.yml".into()];
        let d = evaluate_probe("t", &r);
        assert_eq!(d.action, DryRunAction::BlockMerge);
        assert_eq!(d.detail, "danger-path");
        assert!(d.explanation.contains(".github/workflows/ci.yml"));
    }

    #[test]
    fn first_failing_gate_wins() {
        // A report that fails on multiple gates is still labelled with
        // the first failure in prompt order (checks > conflict > diff
        // > danger).
        let mut r = ok_report();
        r.local_checks[0].exit_code = 1;
        r.merge_conflict.conflicts = true;
        r.diff_size.files = DRYRUN_MAX_DIFF_FILES + 1;
        let d = evaluate_probe("t", &r);
        assert_eq!(d.detail, "failed-checks");
    }

    #[test]
    fn decision_line_matches_spec_shape() {
        let d = DryRunDecision {
            action: DryRunAction::ConsiderAutoPromote,
            task_id: "fix-typo".into(),
            detail: "mechanically-eligible".into(),
            explanation: "mechanically eligible".into(),
        };
        assert_eq!(
            d.as_line(),
            "zen-dryrun: would have considered auto-promoting fix-typo because mechanically eligible"
        );
    }

    #[test]
    fn dedup_key_is_stable_across_identical_decisions() {
        let a = DryRunDecision {
            action: DryRunAction::Merge,
            task_id: "x".into(),
            detail: "all-gates-passed".into(),
            explanation: "irrelevant for dedup".into(),
        };
        let b = DryRunDecision {
            action: DryRunAction::Merge,
            task_id: "x".into(),
            detail: "all-gates-passed".into(),
            explanation: "wholly different prose".into(),
        };
        assert_eq!(a.dedup_key(), b.dedup_key());
    }

    fn workflow_with(name: &str, statuses: &[(&str, StatusCategory)]) -> Workflow {
        Workflow {
            name: name.into(),
            description: None,
            statuses: statuses
                .iter()
                .map(|(n, c)| WorkflowStatus {
                    // Tests pass a single label; collapse it onto both
                    // id and name. resolve_task_status keys off id, so
                    // it has to match `task.column.as_str()`
                    // for the name-match branch — but the inputs here
                    // (`Backlog`, `Design`, `QA`, …) are deliberate
                    // mismatches against the canonical ids, leaving the
                    // category-fallback path as the one under test.
                    id: (*n).into(),
                    name: (*n).into(),
                    category: *c,
                    owner: shelbi_core::Owner::Agent,
                    agent: Some("orchestrator".into()),
                    tags: Vec::new(),
                })
                .collect(),
            initial_status: None,
            transitions: None,
            git: None,
            zen: None,
        }
    }

    fn task_in_column(id: &str, column: Column) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    /// A task in `Column::review()` against the canonical default workflow
    /// resolves to the `Review` status — name match wins. The category
    /// readback is what the dry-run handoff filter keys off.
    #[test]
    fn resolve_status_name_match_picks_default_review() {
        let wf = shelbi_core::default_workflow();
        let t = task_in_column("t", Column::review());
        let s = resolve_task_status(&t, &wf).expect("default workflow declares Review");
        assert_eq!(s.name, "Review");
        assert_eq!(s.category, StatusCategory::Handoff);
    }

    /// A custom workflow that renames the handoff status (here `QA`)
    /// drops the `Review` name match but the category fallback still
    /// resolves a Handoff status — exactly the case the iterate-by-
    /// category change exists for.
    #[test]
    fn resolve_status_category_fallback_picks_renamed_handoff() {
        let wf = workflow_with(
            "design-review",
            &[
                ("Backlog", StatusCategory::Backlog),
                ("Design", StatusCategory::Active),
                ("QA", StatusCategory::Handoff),
                ("Done", StatusCategory::Done),
            ],
        );
        let t = task_in_column("t", Column::review());
        let s = resolve_task_status(&t, &wf).expect("category fallback should find QA");
        assert_eq!(s.name, "QA");
        assert_eq!(s.category, StatusCategory::Handoff);
    }

    /// A workflow that declares neither a `Review`-named status nor any
    /// handoff-category status returns `None`. Dry-run callers then
    /// fall back to `task.column.category()`, which is the legacy
    /// 5-column semantic.
    #[test]
    fn resolve_status_returns_none_when_workflow_has_no_match() {
        let wf = workflow_with(
            "research",
            &[
                ("Inbox", StatusCategory::Backlog),
                ("Reading", StatusCategory::Active),
                ("Shipped", StatusCategory::Done),
            ],
        );
        let t = task_in_column("t", Column::review());
        assert!(resolve_task_status(&t, &wf).is_none());
    }
}
