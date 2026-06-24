//! Zen Mode primitives — `pr_create`, `ci_watch`, `pr_merge`.
//!
//! Each function does one thing. The orchestrator sequences them per its
//! Merge Conditions policy; no primitive implies what the next should do.
//! Same shape as the readiness probe primitives: Rust performs the I/O,
//! the orchestrator's prompt makes the decisions.
//!
//! `pr_create` runs against the worker's worktree (the branch lives there
//! until it's pushed). `ci_watch` and `pr_merge` run on the project's
//! first local machine — by convention the hub — because by the time the
//! orchestrator is watching CI the branch is already on origin and gh is
//! happy from any checkout of the repo.
//!
//! Polling vs. `gh pr checks --watch`: `--watch` has no built-in timeout
//! and `timeout(1)` is not standard on macOS. Polling `gh pr checks`
//! (without `--watch`) lets us bound wall-clock exactly with stdlib timers
//! while still leaning on gh's own exit-code contract (0 / 1 / 8) for the
//! verdict.

use std::path::PathBuf;
use std::process::Output;
use std::time::{Duration, Instant};

use shelbi_core::{Error, Host, MachineKind, Project, Result, Task};

use crate::worker::worker_worktree;

/// How often `ci_watch` re-runs `gh pr checks` while waiting for the
/// pending bucket to clear. Matches gh's own `--watch` default.
const CI_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Outcome of a `gh pr checks --required` poll loop.
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
                let safe_summary = summary.replace('\n', " ").replace(':', " ");
                let trimmed = safe_summary.trim();
                format!("red:{safe_check}:{trimmed}")
            }
            CiVerdict::Timeout => "timeout".to_string(),
        }
    }
}

/// Push the task's branch and open a PR. Idempotent — if an open PR for
/// the branch already exists, returns its number instead of opening a
/// second one.
pub fn pr_create(
    project: &Project,
    project_name: &str,
    task: &Task,
    task_body: &str,
) -> Result<u64> {
    let (host, worktree) = locate_worker_worktree(project, task)?;
    let wt = worktree.to_string_lossy().into_owned();
    let branch = task
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{}", task.id));
    let target = project.default_branch.as_str();

    // Idempotency: if a PR for this branch is already open, return it
    // unchanged. Picking `state=open` intentionally — a closed/merged PR
    // for this branch is stale; a fresh push warrants a fresh PR.
    if let Some(num) = lookup_open_pr(&host, &wt, &branch)? {
        return Ok(num);
    }

    // Push (with -u so gh has a tracking branch to work against).
    let push = run_in_dir(&host, &wt, &["git", "push", "-u", "origin", &branch])?;
    if !push.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} push -u origin {branch}"),
            status: push.status.to_string(),
            stderr: String::from_utf8_lossy(&push.stderr).into_owned(),
        });
    }

    // Race window: a server-side push hook or a branch-protection setup
    // can auto-open a PR. Re-check before we open our own.
    if let Some(num) = lookup_open_pr(&host, &wt, &branch)? {
        return Ok(num);
    }

    let title = head_commit_subject(&host, &wt)?;
    let task_path = shelbi_state::task_path(project_name, &task.id)?
        .to_string_lossy()
        .into_owned();
    let body = compose_pr_body(task_body, &task_path);

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
    parse_pr_number_from_url(stdout.trim()).ok_or_else(|| {
        Error::Other(format!(
            "gh pr create returned `{}` — couldn't parse a PR number out of it",
            stdout.trim()
        ))
    })
}

/// Poll `gh pr checks --required` on `pr` until every required check
/// settles (pass or fail) or `timeout` elapses.
pub fn ci_watch(project: &Project, pr: u64, timeout: Duration) -> Result<CiVerdict> {
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    let pr_str = pr.to_string();

    let deadline = Instant::now() + timeout;
    loop {
        let out = run_in_dir(
            &host,
            &wt,
            &["gh", "pr", "checks", &pr_str, "--required"],
        )?;
        let code = out.status.code();
        match code {
            // 0 — all required checks passed.
            Some(0) => return Ok(CiVerdict::Green),
            // 8 — at least one required check is still pending.
            Some(8) => { /* fall through to sleep + retry */ }
            // Any other non-zero — at least one required check failed.
            Some(_) => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                let stderr = String::from_utf8_lossy(&out.stderr);
                let (check, summary) = first_failing_check(&stdout).unwrap_or_else(|| {
                    let fallback = stdout
                        .lines()
                        .last()
                        .or_else(|| stderr.lines().last())
                        .unwrap_or("")
                        .trim()
                        .to_string();
                    ("unknown".to_string(), fallback)
                });
                return Ok(CiVerdict::Red { check, summary });
            }
            None => {
                return Err(Error::Other(format!(
                    "gh pr checks {pr_str} terminated without an exit code"
                )));
            }
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

/// Squash-merge `pr` and delete its source branch. Returns the merge SHA.
pub fn pr_merge(project: &Project, pr: u64) -> Result<String> {
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    let pr_str = pr.to_string();

    let out = run_in_dir(
        &host,
        &wt,
        &["gh", "pr", "merge", &pr_str, "--squash", "--delete-branch"],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr merge {pr_str} --squash --delete-branch"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }

    // gh pr merge doesn't print the merge SHA. Ask gh for it separately.
    let view = run_in_dir(
        &host,
        &wt,
        &[
            "gh",
            "pr",
            "view",
            &pr_str,
            "--json",
            "mergeCommit",
            "--jq",
            ".mergeCommit.oid // empty",
        ],
    )?;
    if !view.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr view {pr_str} --json mergeCommit"),
            status: view.status.to_string(),
            stderr: String::from_utf8_lossy(&view.stderr).into_owned(),
        });
    }
    let sha = String::from_utf8_lossy(&view.stdout).trim().to_string();
    if sha.is_empty() {
        return Err(Error::Other(format!(
            "gh pr view {pr_str}: merge reported success but mergeCommit.oid is empty"
        )));
    }
    Ok(sha)
}

// ---------------------------------------------------------------------------
// helpers (kept `pub` where they're worth unit-testing on their own)

/// Lay out the PR body: the task summary (or a `# Task <id>` placeholder
/// when the body is empty) followed by an auto-opened footer that points
/// the reviewer back at the task file on disk.
pub fn compose_pr_body(task_body: &str, task_path: &str) -> String {
    let trimmed = task_body.trim();
    let summary = if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n\n")
    };
    format!("{summary}---\n\nAuto-opened by Shelbi Zen Mode — review at: {task_path}\n")
}

/// `gh pr create` prints the new PR's URL like
/// `https://github.com/owner/repo/pull/42`. Pull the trailing `42`.
pub fn parse_pr_number_from_url(s: &str) -> Option<u64> {
    let last = s.rsplit_terminator(|c: char| c == '/' || c.is_whitespace()).next()?;
    last.parse().ok()
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

/// Find the worker assigned to `task`, then return its host + worktree.
/// Errors if the task is unassigned or the worker/machine resolution
/// fails — those are orchestrator bugs, not orchestrator decisions.
fn locate_worker_worktree(project: &Project, task: &Task) -> Result<(Host, PathBuf)> {
    let worker_name = task.assigned_to.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no assigned worker — assign one before running zen primitives",
            task.id
        ))
    })?;
    let worker = project.worker(worker_name).ok_or_else(|| {
        Error::Other(format!(
            "task `{}` references unknown worker `{worker_name}`",
            task.id
        ))
    })?;
    let machine = project
        .machine(&worker.machine)
        .ok_or_else(|| Error::UnknownMachine(worker.machine.clone()))?;
    Ok((machine.host(), worker_worktree(machine, worker)))
}

/// The first local machine in the project — by convention the hub. The
/// hub's `work_dir` is a clean checkout of the project repo, so gh
/// commands routed through it have a remote to talk to without needing
/// a worker's worktree to exist yet.
fn locate_hub_workdir(project: &Project) -> Result<(Host, PathBuf)> {
    let machine = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .ok_or_else(|| {
            Error::Other(
                "project has no local machine to run gh on — zen mode requires a hub".into(),
            )
        })?;
    Ok((machine.host(), machine.work_dir.clone()))
}

/// Run `argv` with cwd = `dir` on `host`, picking up the user's login
/// `PATH` on remote SSH hosts so `gh` and `git` resolve the same way
/// they do in the user's terminal.
fn run_in_dir(host: &Host, dir: &str, argv: &[&str]) -> Result<Output> {
    let escaped: Vec<String> = argv.iter().map(|a| shelbi_agent::shell_escape(a)).collect();
    let line = format!(
        "cd {} && {}",
        shelbi_agent::shell_escape(dir),
        escaped.join(" ")
    );
    // Local: bash -c is fine — the user already has gh on PATH.
    // Remote: bash -lc so .zprofile/.bash_profile populate PATH first.
    let flag = if host.is_local() { "-c" } else { "-lc" };
    shelbi_ssh::run(host, ["bash", flag, line.as_str()]).map_err(Error::Io)
}

fn lookup_open_pr(host: &Host, wt: &str, branch: &str) -> Result<Option<u64>> {
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

fn head_commit_subject(host: &Host, wt: &str) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pr_body_includes_summary_and_footer() {
        let body = compose_pr_body("Add foo to bar.", "/tmp/p/tasks/add-foo.md");
        assert!(body.starts_with("Add foo to bar.\n\n---\n"));
        assert!(body.contains("Auto-opened by Shelbi Zen Mode"));
        assert!(body.contains("/tmp/p/tasks/add-foo.md"));
    }

    #[test]
    fn pr_body_handles_empty_task_body() {
        let body = compose_pr_body("", "/tmp/t.md");
        assert!(body.starts_with("---\n"));
        assert!(body.contains("Auto-opened by Shelbi Zen Mode"));
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
}
