//! Evaluates the awareness store (`shelbi_state::awareness`) on the hub
//! poller's 5s supervisor tick, and runs the detached command jobs.
//!
//! Each tick:
//!
//! - fires every reminder whose `due_at` has passed (a recurring one moves to
//!   its next slot, so one that came due while the daemon was down fires once);
//! - reports every job whose target finished: a command's `result.json`, a pid
//!   that exited, a commit's CI or a PR that settled (those two sample GitHub
//!   on their own [`REMOTE_PROBE_CADENCE`], only while online);
//! - writes an `overdue` line for every active task past its next alert, and
//!   drops the estimate of every task that left the `active` category.
//!
//! The probing (process table, files, GitHub, the board) happens outside the
//! store lock; then one locked read-modify-write appends each event and
//! removes or advances its entry, re-checking that the entry still exists so
//! a `cancel` that raced the probe wins. An entry whose event fails to append
//! stays as it was and is retried next tick: exactly once from the store's
//! point of view.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use shelbi_core::{Error, Project, Result, StatusCategory};
use shelbi_state::awareness::{
    self, overdue_interval, next_slot_after, AwarenessStore, Job, JobKind, JobResult,
};

/// How often a `ci` / `pr` job samples GitHub.
pub const REMOTE_PROBE_CADENCE: Duration = Duration::from_secs(60);

/// A command job whose runner never recorded its pid after this long is
/// reported lost.
const RUNNER_START_GRACE: Duration = Duration::from_secs(120);

/// Finished command-job directories are kept this long for a later look at
/// their output.
const JOB_DIR_RETENTION: Duration = Duration::from_secs(7 * 24 * 3600);

/// How often the tick sweeps old job directories.
const JOB_DIR_PRUNE_CADENCE: Duration = Duration::from_secs(3600);

/// Cross-tick memory for [`tick`]: when each remote job was last sampled, and
/// when job directories were last pruned. In memory only; a restart samples
/// every remote job on its first tick, which is what a restart wants.
#[derive(Debug, Default)]
pub struct AwarenessTickState {
    last_remote_probe: HashMap<String, Instant>,
    last_prune: Option<Instant>,
}

/// A watched GitHub target's settled state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchVerdict {
    /// Every watched check passed.
    Passed,
    /// A watched check failed; names it.
    Failed { check: String },
    /// The PR merged.
    Merged,
    /// The PR closed without merging.
    Closed,
}

/// The outside world [`tick`] reads, behind a trait so the evaluation is
/// testable without processes, GitHub, or a board.
pub trait AwarenessProbe {
    /// Whether `pid` is still running.
    fn pid_alive(&self, pid: u32) -> bool;
    /// One sample of commit `sha`'s CI. `Ok(None)` while pending.
    fn commit_ci(&self, project: &Project, sha: &str, settled: bool) -> Result<Option<WatchVerdict>>;
    /// One sample of PR `number`. `Ok(None)` while open with checks pending.
    fn pull_request(&self, project: &Project, number: u64, settled: bool)
        -> Result<Option<WatchVerdict>>;
    /// Every task currently in an `active`-category status, mapped to the
    /// workspace it is assigned to. `None` when the board can't be read
    /// authoritatively (estimates are then left alone this tick).
    fn active_tasks(&self, project: &Project) -> Option<HashMap<String, Option<String>>>;
    /// Whether the box can reach GitHub.
    fn online(&self) -> bool;
}

/// What a job reports when it fires.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobOutcome {
    status: &'static str,
    exit: Option<i32>,
    detail: Option<String>,
    log: Option<PathBuf>,
}

/// Evaluate `project`'s awareness store once. Best-effort: errors are logged.
pub fn tick(
    project: &Project,
    state: &mut AwarenessTickState,
    now: DateTime<Utc>,
    probe: &dyn AwarenessProbe,
) {
    if let Err(e) = tick_inner(project, state, now, probe) {
        tracing::warn!(project = %project.name, error = %e, "awareness tick failed");
    }
}

fn tick_inner(
    project: &Project,
    state: &mut AwarenessTickState,
    now: DateTime<Utc>,
    probe: &dyn AwarenessProbe,
) -> Result<()> {
    let name = project.name.as_str();
    let store = awareness::read_awareness(name)?;
    maybe_prune_job_dirs(name, &store, state);
    if store == AwarenessStore::default() {
        state.last_remote_probe.clear();
        return Ok(());
    }

    let live: HashSet<&str> = store.jobs.iter().map(|j| j.id.as_str()).collect();
    state.last_remote_probe.retain(|id, _| live.contains(id.as_str()));

    let mut online: Option<bool> = None;
    let mut outcomes: HashMap<String, JobOutcome> = HashMap::new();
    for job in &store.jobs {
        let outcome = match job.kind {
            JobKind::Command => command_outcome(name, job, now, probe),
            JobKind::Pid => pid_outcome(job, probe),
            JobKind::Ci | JobKind::Pr => {
                let due = state
                    .last_remote_probe
                    .get(&job.id)
                    .is_none_or(|t| t.elapsed() >= REMOTE_PROBE_CADENCE);
                if !due || !*online.get_or_insert_with(|| probe.online()) {
                    continue;
                }
                state.last_remote_probe.insert(job.id.clone(), Instant::now());
                remote_outcome(project, job, now, probe)
            }
        };
        if let Some(outcome) = outcome {
            outcomes.insert(job.id.clone(), outcome);
        }
    }

    let active = if store.estimates.is_empty() {
        None
    } else {
        probe.active_tasks(project)
    };

    let due_reminders = store.reminders.iter().any(|r| r.due_at <= now);
    let estimate_work = active.as_ref().is_some_and(|active| {
        store
            .estimates
            .iter()
            .any(|e| !active.contains_key(&e.task_id) || e.next_alert_at <= now)
    });
    if !due_reminders && outcomes.is_empty() && !estimate_work {
        return Ok(());
    }

    awareness::update_awareness(name, |store| {
        fire_reminders(name, store, now);
        fire_jobs(name, store, &outcomes);
        if let Some(active) = &active {
            fire_estimates(name, store, active, now);
        }
        Ok(())
    })
}

fn fire_reminders(project: &str, store: &mut AwarenessStore, now: DateTime<Utc>) {
    store.reminders.retain_mut(|r| {
        if r.due_at > now {
            return true;
        }
        if let Err(e) = shelbi_state::append_reminder_event(project, &r.id, &r.text) {
            tracing::warn!(project, reminder = %r.id, error = %e, "reminder event not written; retrying next tick");
            return true;
        }
        match r.every {
            Some(every) => {
                r.due_at = next_slot_after(r.due_at, every, now);
                true
            }
            None => false,
        }
    });
}

fn fire_jobs(project: &str, store: &mut AwarenessStore, outcomes: &HashMap<String, JobOutcome>) {
    store.jobs.retain(|job| {
        let Some(outcome) = outcomes.get(&job.id) else {
            return true;
        };
        let label = job.display_label();
        let event = shelbi_state::JobEvent {
            id: &job.id,
            kind: job.kind.as_str(),
            label: &label,
            status: outcome.status,
            exit: outcome.exit,
            detail: outcome.detail.as_deref(),
            log: outcome.log.as_deref(),
        };
        match shelbi_state::append_job_event(project, &event) {
            Ok(()) => false,
            Err(e) => {
                tracing::warn!(project, job = %job.id, error = %e, "job event not written; retrying next tick");
                true
            }
        }
    });
}

fn fire_estimates(
    project: &str,
    store: &mut AwarenessStore,
    active: &HashMap<String, Option<String>>,
    now: DateTime<Utc>,
) {
    store.estimates.retain_mut(|est| {
        let Some(assigned) = active.get(&est.task_id) else {
            // Left the active category (handoff, done, back to ready, canceled).
            return false;
        };
        if est.next_alert_at > now {
            return true;
        }
        let elapsed = (now - est.started_at).to_std().unwrap_or_default();
        let workspace = assigned.as_deref().or(est.workspace.as_deref());
        if let Err(e) = shelbi_state::append_task_overdue_event(
            project,
            &est.task_id,
            est.estimate,
            elapsed,
            workspace,
        ) {
            tracing::warn!(project, task = %est.task_id, error = %e, "overdue event not written; retrying next tick");
            return true;
        }
        est.next_alert_at = next_slot_after(est.next_alert_at, overdue_interval(est.estimate), now);
        true
    });
}

fn command_outcome(
    project: &str,
    job: &Job,
    now: DateTime<Utc>,
    probe: &dyn AwarenessProbe,
) -> Option<JobOutcome> {
    let log = awareness::job_output_path(project, &job.id).ok();
    let finished = |result: JobResult| JobOutcome {
        status: if result.succeeded() { "ok" } else { "failed" },
        exit: result.exit_code.or(result.signal.map(|s| 128 + s)),
        detail: result.detail(),
        log: log.clone(),
    };
    match awareness::read_job_result(project, &job.id) {
        Ok(Some(result)) => return Some(finished(result)),
        Ok(None) => {}
        Err(e) => {
            tracing::debug!(project, job = %job.id, error = %e, "job result unreadable");
            return None;
        }
    }
    let lost = match job.runner_pid {
        Some(pid) => !probe.pid_alive(pid),
        None => (now - job.started_at).to_std().unwrap_or_default() > RUNNER_START_GRACE,
    };
    if !lost {
        return None;
    }
    // The runner may have written its result between the read above and the
    // liveness check: read once more before calling it lost.
    if let Ok(Some(result)) = awareness::read_job_result(project, &job.id) {
        return Some(finished(result));
    }
    Some(JobOutcome {
        status: "failed",
        exit: None,
        detail: Some("the job runner exited without recording a result".into()),
        log,
    })
}

fn pid_outcome(job: &Job, probe: &dyn AwarenessProbe) -> Option<JobOutcome> {
    let pid: u32 = job.target.parse().ok()?;
    (!probe.pid_alive(pid)).then_some(JobOutcome {
        status: "exited",
        exit: None,
        detail: None,
        log: None,
    })
}

fn remote_outcome(
    project: &Project,
    job: &Job,
    now: DateTime<Utc>,
    probe: &dyn AwarenessProbe,
) -> Option<JobOutcome> {
    let settled = (now - job.started_at).to_std().unwrap_or_default() >= crate::zen::CI_SETTLE_GRACE;
    let verdict = match job.kind {
        JobKind::Ci => probe.commit_ci(project, &job.target, settled),
        JobKind::Pr => match job.target.parse::<u64>() {
            Ok(n) => probe.pull_request(project, n, settled),
            Err(_) => Err(Error::Other(format!("PR job target `{}` is not a number", job.target))),
        },
        _ => return None,
    };
    let verdict = match verdict {
        Ok(Some(v)) => v,
        Ok(None) => return None,
        Err(e) => {
            tracing::debug!(project = %project.name, job = %job.id, error = %e, "awareness watch sample failed");
            return None;
        }
    };
    let (status, detail) = match verdict {
        WatchVerdict::Passed => ("ok", None),
        WatchVerdict::Failed { check } => ("failed", Some(format!("failing check: {check}"))),
        WatchVerdict::Merged => ("merged", None),
        WatchVerdict::Closed => ("closed", None),
    };
    Some(JobOutcome {
        status,
        exit: None,
        detail,
        log: None,
    })
}

fn maybe_prune_job_dirs(project: &str, store: &AwarenessStore, state: &mut AwarenessTickState) {
    if state
        .last_prune
        .is_some_and(|t| t.elapsed() < JOB_DIR_PRUNE_CADENCE)
    {
        return;
    }
    state.last_prune = Some(Instant::now());
    let live: Vec<&str> = store.jobs.iter().map(|j| j.id.as_str()).collect();
    awareness::prune_job_dirs(project, &live, JOB_DIR_RETENTION);
}

// ---------------------------------------------------------------------------
// The live probe

/// The production [`AwarenessProbe`]: the process table, GitHub through `gh`,
/// and the project's board index.
pub struct LiveProbe<F: Fn() -> bool> {
    pub is_online: F,
}

impl<F: Fn() -> bool> AwarenessProbe for LiveProbe<F> {
    fn pid_alive(&self, pid: u32) -> bool {
        pid_alive(pid)
    }

    fn commit_ci(&self, project: &Project, sha: &str, settled: bool) -> Result<Option<WatchVerdict>> {
        watch_commit_ci(project, sha, settled)
    }

    fn pull_request(
        &self,
        project: &Project,
        number: u64,
        settled: bool,
    ) -> Result<Option<WatchVerdict>> {
        watch_pull_request(project, number, settled)
    }

    fn active_tasks(&self, project: &Project) -> Option<HashMap<String, Option<String>>> {
        active_tasks(project)
    }

    fn online(&self) -> bool {
        (self.is_online)()
    }
}

/// Whether `pid` names a running process. `EPERM` means it exists but belongs
/// to someone else: still running.
pub fn pid_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 performs only the existence and permission checks.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Every open task in an `active`-category status (resolved through the
/// project's `statuses.yaml`), mapped to its assigned workspace. `None` unless
/// the board read is authoritative.
fn active_tasks(project: &Project) -> Option<HashMap<String, Option<String>>> {
    let board = match shelbi_state::read_board_with_cfg(&project.name, &project.issue_tracker) {
        Ok(shelbi_state::BoardState::Warm(board)) => board,
        Ok(_) => return None,
        Err(e) => {
            tracing::debug!(project = %project.name, error = %e, "awareness: board read failed");
            return None;
        }
    };
    let catalog = shelbi_state::load_project_statuses(&project.name).ok();
    let category = |col: &shelbi_core::Column| {
        catalog
            .as_ref()
            .and_then(|c| c.get(col.as_str()))
            .map(|s| s.category)
            .unwrap_or_else(|| col.category())
    };
    Some(
        board
            .into_iter()
            .filter(|tf| category(&tf.task.column) == StatusCategory::Active)
            .map(|tf| (tf.task.id, tf.task.assigned_to))
            .collect(),
    )
}

/// The current category of `task_id` (resolved through `statuses.yaml`), or
/// `None` when the task can't be found.
pub fn task_category(project: &str, task_id: &str) -> Result<Option<StatusCategory>> {
    let store = shelbi_state::issue_store_for(project)?;
    let Some(issue) = store.get(task_id)? else {
        return Ok(None);
    };
    let catalog = shelbi_state::load_project_statuses(project).ok();
    Ok(Some(
        catalog
            .as_ref()
            .and_then(|c| c.get(issue.task.column.as_str()))
            .map(|s| s.category)
            .unwrap_or_else(|| issue.task.column.category()),
    ))
}

// ---------------------------------------------------------------------------
// GitHub watches

/// A commit's check rollup. Commit-level contexts have no requiredness (that is
/// a property of a PR's base branch), so every reported check gates, the same
/// all-reported mode the PR poll falls back to.
const COMMIT_CI_QUERY: &str = r#"
query ShelbiCommitCi($owner: String!, $name: String!, $oid: GitObjectID!) {
  repository(owner: $owner, name: $name) {
    object(oid: $oid) {
      ... on Commit {
        oid
        statusCheckRollup {
          contexts(first: 100) {
            totalCount
            pageInfo { hasNextPage }
            nodes {
              __typename
              ... on CheckRun {
                name
                status
                conclusion
                detailsUrl
                checkSuite { app { slug } }
              }
              ... on StatusContext {
                context
                state
                targetUrl
              }
            }
          }
        }
      }
    }
  }
}
"#;

fn gh_graphql(project: &Project, query: &str, vars: &[String], what: &str) -> Result<serde_json::Value> {
    let (host, dir) = crate::git::locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();
    let repository = crate::git::lookup_origin_repository(&host, &wt)?;
    let (owner, name) = crate::git::repository_owner_and_name(&repository, what)?;
    let query = format!("query={query}");
    let owner = format!("owner={owner}");
    let name = format!("name={name}");
    let mut args: Vec<&str> = vec![
        "gh",
        "api",
        "graphql",
        "--hostname",
        repository.host.as_str(),
        "-f",
        query.as_str(),
        "-f",
        owner.as_str(),
        "-f",
        name.as_str(),
    ];
    for v in vars {
        args.push(if v.starts_with("number=") { "-F" } else { "-f" });
        args.push(v.as_str());
    }
    let out = crate::git::run_in_dir(&host, &wt, &args)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh api graphql <Shelbi {what}>"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| Error::Other(format!("GitHub returned invalid JSON for {what}: {e}")))?;
    if value
        .get("errors")
        .is_some_and(|e| !e.is_null() && e.as_array().is_none_or(|a| !a.is_empty()))
    {
        return Err(Error::Other(format!("GitHub GraphQL returned errors for {what}")));
    }
    Ok(value)
}

/// One sample of commit `sha`'s CI.
pub fn watch_commit_ci(project: &Project, sha: &str, settled: bool) -> Result<Option<WatchVerdict>> {
    let what = format!("commit {sha} CI");
    let value = gh_graphql(project, COMMIT_CI_QUERY, &[format!("oid={sha}")], &what)?;
    let commit = value
        .pointer("/data/repository/object")
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::Other(format!("GitHub has no commit {sha}")))?;
    commit_ci_verdict(sha, commit, settled)
}

/// Grade a commit object from [`COMMIT_CI_QUERY`].
fn commit_ci_verdict(
    sha: &str,
    commit: &serde_json::Value,
    settled: bool,
) -> Result<Option<WatchVerdict>> {
    // Commit contexts carry no requiredness; mark every one not-required so
    // the shared parser grades them all.
    let mut commit = commit.clone();
    if let Some(nodes) = commit
        .pointer_mut("/statusCheckRollup/contexts/nodes")
        .and_then(serde_json::Value::as_array_mut)
    {
        for node in nodes {
            if let Some(obj) = node.as_object_mut() {
                obj.insert("isRequired".into(), serde_json::Value::Bool(false));
            }
        }
    }
    let short = sha.get(..12).unwrap_or(sha);
    let checks = crate::zen::parse_rollup_checks(&format!("commit {short}"), &commit)?;
    let snapshot = crate::zen::CiSnapshot {
        repository_id: String::new(),
        base_ref: String::new(),
        base_oid: String::new(),
        head_oid: sha.to_string(),
        head_repository_id: String::new(),
        merge_state: "CLEAN".into(),
        checks,
    };
    Ok(verdict_of(&snapshot, settled))
}

/// One sample of PR `number`: merged / closed, or its checks settling.
pub fn watch_pull_request(
    project: &Project,
    number: u64,
    settled: bool,
) -> Result<Option<WatchVerdict>> {
    let what = format!("PR #{number} watch");
    let value = gh_graphql(
        project,
        crate::zen::CI_SNAPSHOT_QUERY,
        &[format!("number={number}")],
        &what,
    )?;
    let pr = value
        .pointer("/data/repository/pullRequest")
        .filter(|v| !v.is_null())
        .ok_or_else(|| Error::Other(format!("GitHub has no PR #{number}")))?;
    pull_request_verdict(number, pr, settled)
}

/// Grade a `pullRequest` object from the CI snapshot query. The merge state is
/// not consulted: the watch reports the checks settling, and a PR that is
/// blocked on review or behind its base still has settled checks.
fn pull_request_verdict(
    number: u64,
    pr: &serde_json::Value,
    settled: bool,
) -> Result<Option<WatchVerdict>> {
    match pr.get("state").and_then(serde_json::Value::as_str) {
        Some("MERGED") => return Ok(Some(WatchVerdict::Merged)),
        Some("CLOSED") => return Ok(Some(WatchVerdict::Closed)),
        _ => {}
    }
    let Some(commit) = pr.pointer("/commits/nodes/0/commit") else {
        return Ok(None);
    };
    let checks = crate::zen::parse_rollup_checks(&format!("PR #{number}"), commit)?;
    let snapshot = crate::zen::CiSnapshot {
        repository_id: String::new(),
        base_ref: String::new(),
        base_oid: String::new(),
        head_oid: String::new(),
        head_repository_id: String::new(),
        merge_state: "CLEAN".into(),
        checks,
    };
    Ok(verdict_of(&snapshot, settled))
}

fn verdict_of(snapshot: &crate::zen::CiSnapshot, settled: bool) -> Option<WatchVerdict> {
    match crate::zen::classify_ci_snapshot(snapshot, settled)? {
        crate::zen::CiVerdict::Green => Some(WatchVerdict::Passed),
        crate::zen::CiVerdict::Red { check, .. } => Some(WatchVerdict::Failed { check }),
        crate::zen::CiVerdict::Timeout => None,
    }
}

// ---------------------------------------------------------------------------
// Command jobs

/// Start `argv` as a detached command job for `project`: register the job,
/// spawn `shelbi __job-exec` in its own session (so it outlives this CLI and
/// any daemon restart, and its process group is the command's), and record
/// the runner's pid. Returns the job at once.
pub fn spawn_command_job(project: &str, argv: &[String], label: Option<String>) -> Result<Job> {
    if argv.is_empty() {
        return Err(Error::Other("no command given after `--`".into()));
    }
    let target = argv
        .iter()
        .map(|a| shelbi_core::shell_escape(a))
        .collect::<Vec<_>>()
        .join(" ");
    let job = awareness::add_job(
        project,
        awareness::NewJob {
            kind: JobKind::Command,
            target,
            label,
        },
    )?;
    let started = (|| -> Result<u32> {
        let dir = awareness::job_dir(project, &job.id)?;
        shelbi_state::ensure_dir(&dir)?;
        let exe = shelbi_core::current_shelbi_exe()
            .ok_or_else(|| Error::Other("cannot locate the shelbi binary".into()))?;
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--project")
            .arg(project)
            .arg("__job-exec")
            .arg(&job.id)
            .arg("--")
            .args(argv)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: async-signal-safe `setsid` in the forked child before exec.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
        let child = cmd.spawn().map_err(Error::Io)?;
        // Not waited on: the runner is its own session and outlives us.
        Ok(child.id())
    })();
    match started {
        Ok(pid) => {
            awareness::set_job_runner_pid(project, &job.id, pid)?;
            Ok(Job {
                runner_pid: Some(pid),
                ..job
            })
        }
        Err(e) => {
            let _ = awareness::remove_job(project, &job.id);
            Err(e)
        }
    }
}

/// The body of `shelbi __job-exec <id> -- <cmd...>`: run the command with its
/// combined output in the job's `output.log`, then write `result.json`. The
/// runner was started as a session leader, so the command shares its process
/// group and `job cancel` reaches both with one signal.
pub fn run_job_exec(project: &str, id: &str, argv: &[String]) -> Result<()> {
    let output_path = awareness::job_output_path(project, id)?;
    if let Some(parent) = output_path.parent() {
        shelbi_state::ensure_dir(parent)?;
    }
    let result = (|| -> std::result::Result<std::process::ExitStatus, String> {
        let program = argv.first().ok_or("no command given")?;
        let out = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&output_path)
            .map_err(|e| format!("cannot open {}: {e}", output_path.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        std::process::Command::new(program)
            .args(&argv[1..])
            .stdin(std::process::Stdio::null())
            .stdout(out)
            .stderr(err)
            .status()
            .map_err(|e| format!("could not start `{program}`: {e}"))
    })();
    let tail = awareness::tail_lines(&output_path, awareness::JOB_TAIL_LINES);
    let result = match result {
        Ok(status) => {
            #[cfg(unix)]
            let signal = std::os::unix::process::ExitStatusExt::signal(&status);
            #[cfg(not(unix))]
            let signal = None;
            JobResult {
                exit_code: status.code(),
                signal,
                finished_at: Utc::now(),
                tail,
                error: None,
            }
        }
        Err(error) => JobResult {
            exit_code: Some(127),
            signal: None,
            finished_at: Utc::now(),
            tail,
            error: Some(error),
        },
    };
    awareness::write_job_result(project, id, &result)
}

/// Cancel job `id`: drop it from the store and, for a command job whose runner
/// is still alive, SIGTERM the runner's process group (the command with it).
/// Returns the removed job, or `None` when there was none.
pub fn cancel_job(project: &str, id: &str) -> Result<Option<Job>> {
    let Some(job) = awareness::remove_job(project, id)? else {
        return Ok(None);
    };
    if job.kind == JobKind::Command {
        if let Some(pid) = job.runner_pid.and_then(|p| i32::try_from(p).ok()) {
            // Only a group this job's own runner leads: the result file is
            // absent (still running) and the leader is alive.
            let running = matches!(awareness::read_job_result(project, id), Ok(None));
            if running && pid > 1 && is_job_runner(pid as u32, id) {
                // SAFETY: plain kill(2) on the negated pgid of our own runner.
                unsafe {
                    libc::kill(-pid, libc::SIGTERM);
                }
            }
        }
    }
    Ok(Some(job))
}

/// Whether `pid` is still this job's `shelbi __job-exec <id>` runner, read
/// from the process table, so a recycled pid is never signalled.
fn is_job_runner(pid: u32, id: &str) -> bool {
    if !pid_alive(pid) {
        return false;
    }
    let Ok(out) = std::process::Command::new("ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
    else {
        return false;
    };
    let args = String::from_utf8_lossy(&out.stdout);
    let words: Vec<&str> = args.split_whitespace().collect();
    words
        .windows(2)
        .any(|w| w[0] == "__job-exec" && w[1] == id)
}

#[cfg(test)]
#[path = "awareness_tests.rs"]
mod tests;
