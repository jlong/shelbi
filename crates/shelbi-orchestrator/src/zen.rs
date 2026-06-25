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

use globset::{Glob, GlobSetBuilder};
use serde::Serialize;
use shelbi_core::{
    checks_for_task, danger_paths_for_project, Error, Host, Machine, MachineKind, Project, Result,
    Task, WorkerSpec,
};

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
    pub local_checks: Vec<LocalCheck>,
    pub merge_conflict: ConflictProbe,
    pub diff_size: DiffSize,
    pub danger_paths: DangerPaths,
}

// ---------------------------------------------------------------------------
// Entry point

/// Run every primitive for `task` on `branch` and return the report.
///
/// Resolves the worker's worktree (and machine) from `task.assigned_to` —
/// the probe always operates against the worker that produced the branch,
/// not against the hub's parent repo. This matters for remote workers: the
/// branch only exists in the remote worktree's git repo until it's
/// pushed.
pub fn probe(project: &Project, task: &Task, branch: &str) -> Result<ProbeReport> {
    let (machine, worker) = resolve_worker(project, task)?;
    let host = machine.host();
    let worktree = worker_worktree(&machine, worker);

    let merge_conflict = probe_merge_conflict(&host, &worktree, branch, &project.default_branch)?;
    let diff_size = probe_diff_size(&host, &worktree, branch, &project.default_branch)?;
    let danger_paths = probe_danger_paths(project, &host, &worktree, branch)?;
    let local_checks = probe_local_checks(&host, &worktree, project, task)?;

    Ok(ProbeReport {
        local_checks,
        merge_conflict,
        diff_size,
        danger_paths,
    })
}

fn resolve_worker<'a>(
    project: &'a Project,
    task: &Task,
) -> Result<(Machine, &'a WorkerSpec)> {
    let worker_name = task.assigned_to.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no assigned worker — assign one before probing",
            task.id
        ))
    })?;
    let worker = project.worker(worker_name).ok_or_else(|| {
        Error::Other(format!(
            "worker `{}` (assigned to task `{}`) is not declared in project `{}`",
            worker_name, task.id, project.name
        ))
    })?;
    let machine = project
        .machine(&worker.machine)
        .ok_or_else(|| Error::UnknownMachine(worker.machine.clone()))?
        .clone();
    Ok((machine, worker))
}

// ---------------------------------------------------------------------------
// local_checks

fn probe_local_checks(
    host: &Host,
    worktree: &std::path::Path,
    project: &Project,
    task: &Task,
) -> Result<Vec<LocalCheck>> {
    let commands = checks_for_task(project, task);
    if commands.is_empty() {
        return Ok(Vec::new());
    }

    // Best-effort log file on the hub. A failure here just means the log
    // is missing; it doesn't block the probe.
    let log_path = log_file_path(&task.id).ok();
    if let Some(p) = &log_path {
        let _ = init_log(p, &task.id, commands.len());
    }

    let mut out = Vec::with_capacity(commands.len());
    for cmd in commands {
        let res = run_one_check(host, worktree, &cmd);
        if let Some(p) = &log_path {
            let _ = append_log(p, &res);
        }
        out.push(res);
    }
    Ok(out)
}

fn run_one_check(host: &Host, worktree: &std::path::Path, cmd: &str) -> LocalCheck {
    let wt = worktree.to_string_lossy().into_owned();
    // `sh -c` so the project author can chain commands, use pipes, etc.
    // We `cd` into the worktree first because some checks (cargo, pytest)
    // care about the working directory, not just argv[0]'s path.
    let script = format!("cd {} && {}", shell_escape(&wt), cmd);

    let started = Instant::now();
    let output = shelbi_ssh::run(host, ["sh", "-c", script.as_str()]);
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

    LocalCheck {
        command: cmd.to_string(),
        exit_code,
        duration_ms: ms_truncating(elapsed),
        output_tail: tail_lines(&combined, OUTPUT_TAIL_LINES),
    }
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
    writeln!(f, "# shelbi zen probe — task {task_id} — {n_checks} check(s)")?;
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
    Ok(ConflictProbe { conflicts: true, files })
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
    let range = format!("{main}..{branch}");
    let stdout = shelbi_ssh::run_capture(
        host,
        ["git", "-C", wt.as_str(), "diff", "--shortstat", range.as_str()],
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
    host: &Host,
    worktree: &std::path::Path,
    branch: &str,
) -> Result<DangerPaths> {
    let patterns = danger_paths_for_project(project);
    if patterns.is_empty() {
        return Ok(DangerPaths::default());
    }
    let wt = worktree.to_string_lossy().into_owned();
    let range = format!("{}..{}", project.default_branch, branch);
    let stdout = shelbi_ssh::run_capture(
        host,
        ["git", "-C", wt.as_str(), "diff", "--name-only", range.as_str()],
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
mod tests {
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
        let status = Command::new("git").current_dir(cwd).args(args).status().unwrap();
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
}
