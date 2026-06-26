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
use std::time::{Duration, Instant};

use globset::{Glob, GlobSetBuilder};
use serde::Serialize;
use shelbi_core::{
    checks_for_task, danger_paths_for_project, Column, Error, Host, Machine, Project, Result,
    Task, WorkerSpec,
};

use crate::git::{
    compose_pr_body, head_commit_subject, locate_hub_workdir, locate_worker_worktree,
    lookup_open_pr, parse_pr_number_from_url, run_in_dir,
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
    let target = project.base_branch();

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

/// Integrate `pr` and delete its source branch using the project's
/// configured [`shelbi_core::MergeStrategy`]. Returns the merge SHA.
pub fn pr_merge(project: &Project, pr: u64) -> Result<String> {
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    let pr_str = pr.to_string();
    let strategy_flag = project.merge_strategy().gh_flag();

    let out = run_in_dir(
        &host,
        &wt,
        &["gh", "pr", "merge", &pr_str, strategy_flag, "--delete-branch"],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr merge {pr_str} {strategy_flag} --delete-branch"),
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

    let base = project.base_branch();
    let merge_conflict = probe_merge_conflict(&host, &worktree, branch, base)?;
    let diff_size = probe_diff_size(&host, &worktree, branch, base)?;
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
    let range = format!("{}..{}", project.base_branch(), branch);
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
        .map(|tf| (tf.task.id.clone(), tf.task.column))
        .collect();

    let in_flight_bodies: Vec<&str> = tasks
        .iter()
        .filter(|tf| tf.task.column == Column::InProgress)
        .map(|tf| tf.body.as_str())
        .collect();

    let mut candidates: Vec<&Task> = tasks
        .iter()
        .filter(|tf| tf.task.column == Column::Backlog)
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
    tokens
        .iter()
        .any(|tok| in_flight_bodies.iter().any(|body| body.contains(tok.as_str())))
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
    for raw in body.split(|c: char| {
        !(c.is_alphanumeric() || c == '/' || c == '.' || c == '-' || c == '_')
    }) {
        if let Some(tok) = canonical_path_token(raw) {
            out.push(tok);
        }
    }
    out
}

fn canonical_path_token(raw: &str) -> Option<String> {
    // Only trim the *trailing* end. Stripping leading punctuation would
    // eat the slash in tokens like `./foo.rs` and lose the path signal.
    let trimmed = raw.trim_end_matches(|c: char| c == '.' || c == '/' || c == '-' || c == '_');
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
/// line is anything else (worker events, other transitions, non-user
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
            tf(task("done-a", Column::Done, 0, &[]), ""),
            tf(task("todo-a", Column::Todo, 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty());
    }

    #[test]
    fn returns_eligible_in_priority_order() {
        let tasks = vec![
            tf(task("b", Column::Backlog, 2, &[]), ""),
            tf(task("a", Column::Backlog, 0, &[]), ""),
            tf(task("c", Column::Backlog, 1, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["a", "c", "b"]);
    }

    #[test]
    fn excludes_blocked_by_unfinished_deps() {
        let tasks = vec![
            tf(task("blocked", Column::Backlog, 0, &["other"]), ""),
            tf(task("other", Column::Todo, 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn returns_empty_when_every_backlog_task_is_blocked() {
        // Mix of blocked-by-todo and blocked-by-in-progress. None can move.
        let tasks = vec![
            tf(task("a", Column::Backlog, 0, &["x"]), ""),
            tf(task("b", Column::Backlog, 1, &["y"]), ""),
            tf(task("x", Column::Todo, 0, &[]), ""),
            tf(task("y", Column::InProgress, 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn done_deps_unblock_a_task() {
        let tasks = vec![
            tf(task("waiting", Column::Backlog, 0, &["dep"]), ""),
            tf(task("dep", Column::Done, 0, &[]), ""),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["waiting"]);
    }

    #[test]
    fn excludes_zen_enabled_false() {
        let mut t = task("opt-out", Column::Backlog, 0, &[]);
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
        let mut opt_in = task("opt-in", Column::Backlog, 0, &[]);
        opt_in.zen = Some(shelbi_core::TaskZenConfig {
            enabled: Some(true),
            ..Default::default()
        });
        let unset = task("unset", Column::Backlog, 1, &[]);
        let tasks = vec![tf(opt_in, ""), tf(unset, "")];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["opt-in", "unset"]);
    }

    #[test]
    fn excludes_previously_user_demoted() {
        let tasks = vec![
            tf(task("demoted", Column::Backlog, 0, &[]), ""),
            tf(task("fresh", Column::Backlog, 1, &[]), ""),
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
            tf(task("in-flight", Column::InProgress, 0, &[]), body_a),
            tf(task("candidate", Column::Backlog, 0, &[]), body_b),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert!(got.is_empty(), "{got:?}");
    }

    #[test]
    fn file_overlap_does_not_trigger_on_unrelated_paths() {
        let in_flight_body = "Working on `crates/shelbi-tui/src/app.rs`.";
        let candidate_body = "Touch `crates/shelbi-state/src/lib.rs` only.";
        let tasks = vec![
            tf(task("in-flight", Column::InProgress, 0, &[]), in_flight_body),
            tf(task("candidate", Column::Backlog, 0, &[]), candidate_body),
        ];
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["candidate"]);
    }

    #[test]
    fn does_not_cap_result_count() {
        // Ten eligible tasks; we get all ten back. The orchestrator's
        // judgment layer picks how many to actually promote.
        let tasks: Vec<TaskFile> = (0..10)
            .map(|i| tf(task(&format!("t-{i}"), Column::Backlog, i, &[]), ""))
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
2026-06-24T00:00:00+00:00 worker=alpha none -> working
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
            .map(|(i, body)| tf(task(&format!("t-{i}"), Column::Backlog, i as u32, &[]), body))
            .collect();
        let got = mechanically_eligible_from(&tasks, &HashSet::new());
        assert_eq!(got, vec!["t-0", "t-1", "t-2"]);
    }
}


// ===========================================================================
// Dry-run preview — what would Zen do, without doing it
// ===========================================================================
//
// `dry_run_tick` runs the same two read-only steps Zen Mode runs every loop:
//
// 1. Scan the backlog for mechanically-eligible auto-promotion candidates.
// 2. Probe every task currently in `review` and apply the default mechanical
//    bar (the thresholds documented in the orchestrator prompt template).
//
// It returns one `DryRunDecision` per finding so the CLI can log "would
// have …" without touching any state. The orchestrator's judgment layer
// (the auto-promote categories in the prompt) is *not* simulated — that
// requires an LLM. The decisions for backlog candidates make this
// explicit by labelling them `WouldConsiderAutoPromote` rather than
// `WouldAutoPromote`.

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

/// Run one read-only Zen pass for `project` and return every decision
/// the live loop would have made. Probes (which shell out) are best-
/// effort: a probe that errors is surfaced as a `BlockMerge` decision
/// labelled `probe-failed` so the user still sees the task, rather than
/// silently dropping it.
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

    // 2. Review-column probes — apply the default mechanical bar.
    let review_tasks = shelbi_state::list_column(&project.name, Column::Review)?;
    for tf in review_tasks {
        let branch = tf
            .task
            .branch
            .clone()
            .unwrap_or_else(|| format!("shelbi/{}", tf.task.id));
        match probe(project, &tf.task, &branch) {
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

/// Apply the default merge-conditions bar to a probe report. Returns a
/// `Merge` decision if every gate passes, a `BlockMerge` decision tagged
/// with the first failing gate otherwise.
///
/// Gate order matches the prompt template — first failure wins so the
/// user sees the same single reason live Zen would emit.
pub fn evaluate_probe(task_id: &str, report: &ProbeReport) -> DryRunDecision {
    if let Some(failed) = report
        .local_checks
        .iter()
        .find(|c| c.exit_code != 0)
    {
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
                report.diff_size.files,
                total_lines,
                DRYRUN_MAX_DIFF_FILES,
                DRYRUN_MAX_DIFF_LINES,
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
            local_checks: vec![LocalCheck {
                command: "cargo test".into(),
                exit_code: 0,
                duration_ms: 100,
                output_tail: String::new(),
            }],
            merge_conflict: ConflictProbe::default(),
            diff_size: DiffSize { files: 3, lines_added: 40, lines_removed: 5 },
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
}
