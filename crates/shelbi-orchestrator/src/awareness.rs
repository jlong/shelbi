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
//!   drops the estimate of every task that left the `active` category;
//! - writes a job `overdue` line for every running job past its estimate's
//!   next alert, on the same grid as a task's (the estimate, then every half
//!   of it, at least [`awareness::MIN_OVERDUE_INTERVAL`]), until it finishes.
//!
//! A job a worker started ([`Job::started_by`]) reports to that worker's
//! session as a prompt instead of the feed, finished or overdue, while the
//! session still serves the task that started it ([`crate::job_route`]).
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

pub use crate::job_exec::{job_of_session, job_session_name, run_job_exec};
pub use crate::job_route::JobNotice;
use crate::session_process_backend::SessionProcessBackend;
use shelbi_state::awareness::{
    self, overdue_interval, next_slot_after, AwarenessStore, Job, JobKind, JobRecord, JobResult,
    JobStarter, JobStatus,
};

/// How often a `ci` / `pr` job samples GitHub.
pub const REMOTE_PROBE_CADENCE: Duration = Duration::from_secs(60);

/// A command job whose runner never recorded its pid after this long is
/// reported lost.
const RUNNER_START_GRACE: Duration = Duration::from_secs(120);

/// How often the tick sweeps the finished-job history past its retention
/// window (`awareness::JOB_HISTORY_MAX_JOBS` / `JOB_HISTORY_MAX_AGE`). A
/// finish prunes too, so this only matters for age running out on a quiet
/// project.
const JOB_HISTORY_PRUNE_CADENCE: Duration = Duration::from_secs(3600);

/// Cross-tick memory for [`tick`]: when each remote job was last sampled, and
/// when job directories were last pruned. In memory only; a restart samples
/// every remote job on its first tick, which is what a restart wants.
#[derive(Debug, Default)]
pub struct AwarenessTickState {
    last_remote_probe: HashMap<String, Instant>,
    last_prune: Option<Instant>,
    /// The command jobs the store listed on the last tick.
    command_jobs: HashSet<String>,
    /// Command jobs that have left the store (finished or canceled) whose
    /// session directory is still to be removed, with how many ticks have
    /// tried. See [`remove_job_sessions`].
    ended_sessions: HashMap<String, u32>,
}

/// How many ticks keep trying to remove an ended job's session while it is
/// still up (its runner still writing its result). The hourly sweep in
/// [`maybe_prune_job_history`] removes any left after that.
const ENDED_SESSION_TRIES: u32 = 120;

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
    /// Whether the worker that started a job is still there to hear about it:
    /// its session is up and serves the same task.
    fn starter_live(&self, project: &Project, starter: &JobStarter) -> bool;
    /// Hand `notice` to `starter`'s session. Must not block the tick: the
    /// live probe sends on a thread of its own, falling back to the feed if
    /// the send doesn't land.
    fn deliver_to_starter(&self, project: &Project, starter: &JobStarter, notice: JobNotice);
}

/// What a job reports when it fires.
#[derive(Debug, Clone, PartialEq, Eq)]
struct JobOutcome {
    status: JobStatus,
    exit: Option<i32>,
    detail: Option<String>,
    log: Option<PathBuf>,
    /// When it ended, when the job says (a command's `result.json`); else the
    /// tick that noticed.
    finished_at: Option<DateTime<Utc>>,
}

impl JobOutcome {
    fn record(&self, job: &Job, now: DateTime<Utc>) -> JobRecord {
        JobRecord {
            exit_code: self.exit,
            detail: self.detail.clone(),
            log: self.log.clone(),
            ..JobRecord::finished(job, self.status, self.finished_at.unwrap_or(now))
        }
    }
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
    maybe_prune_job_history(name, &store, state, now);
    clean_ended_job_sessions(name, &store, state);
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
    let overdue_jobs = store.jobs.iter().any(|j| job_alert_due(j, now));
    let estimate_work = active.as_ref().is_some_and(|active| {
        store
            .estimates
            .iter()
            .any(|e| !active.contains_key(&e.task_id) || e.next_alert_at <= now)
    });
    if !due_reminders && outcomes.is_empty() && !estimate_work && !overdue_jobs {
        return Ok(());
    }

    // Which reporting jobs' starters are still there to hear it, asked
    // outside the lock. A job missing here reports to the feed.
    let live_starters: HashSet<String> = store
        .jobs
        .iter()
        .filter(|j| outcomes.contains_key(&j.id) || job_alert_due(j, now))
        .filter(|j| j.started_by.as_ref().is_some_and(|s| probe.starter_live(project, s)))
        .map(|j| j.id.clone())
        .collect();

    let mut to_workers = Vec::new();
    let finished = awareness::update_awareness(name, |store| {
        let mut reports = Reports { project: name, live_starters: &live_starters, to_workers: &mut to_workers };
        fire_reminders(name, store, now);
        let finished = fire_jobs(&mut reports, store, &outcomes, now);
        // After the finished ones leave: a job that ends this tick reports
        // its end, not that it ran long.
        fire_job_overdue(&mut reports, store, now);
        if let Some(active) = &active {
            fire_estimates(name, store, active, now);
        }
        Ok(finished)
    })?;
    // Only once the store no longer lists them as due, so a report is handed
    // over once.
    for (starter, notice) in to_workers {
        probe.deliver_to_starter(project, &starter, notice);
    }
    // Into the history once the store no longer lists them. A failure here
    // loses only the record: the event is already out.
    if let Err(e) = awareness::record_finished_jobs(name, finished, now) {
        tracing::warn!(project = name, error = %e, "finished jobs not recorded in the job history");
    }
    Ok(())
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

/// Where this tick's job reports go: the worker that started a job, while it
/// is live, else the orchestrator's feed.
struct Reports<'a> {
    project: &'a str,
    /// Jobs whose starter is live (see [`AwarenessProbe::starter_live`]).
    live_starters: &'a HashSet<String>,
    /// The reports for workers, handed over once the store is written.
    to_workers: &'a mut Vec<(JobStarter, JobNotice)>,
}

impl Reports<'_> {
    /// Route `notice` for `job`. `Err` only when the feed line failed to
    /// append (the caller retries next tick).
    fn send(&mut self, job: &Job, notice: JobNotice) -> Result<()> {
        match &job.started_by {
            Some(starter) if self.live_starters.contains(&job.id) => {
                self.to_workers.push((starter.clone(), notice));
                Ok(())
            }
            _ => notice.append_to_feed(self.project),
        }
    }
}

/// Report every job with an outcome and drop it from the store. Returns the
/// history record of each one reported.
fn fire_jobs(
    reports: &mut Reports<'_>,
    store: &mut AwarenessStore,
    outcomes: &HashMap<String, JobOutcome>,
    now: DateTime<Utc>,
) -> Vec<JobRecord> {
    let mut finished = Vec::new();
    store.jobs.retain(|job| {
        let Some(outcome) = outcomes.get(&job.id) else {
            return true;
        };
        let notice = JobNotice::Finished {
            id: job.id.clone(),
            kind: job.kind,
            label: job.display_label(),
            status: outcome.status,
            exit: outcome.exit,
            detail: outcome.detail.clone(),
            log: outcome.log.clone(),
        };
        match reports.send(job, notice) {
            Ok(()) => {
                finished.push(outcome.record(job, now));
                false
            }
            Err(e) => {
                tracing::warn!(project = reports.project, job = %job.id, error = %e, "job event not written; retrying next tick");
                true
            }
        }
    });
    finished
}

/// Whether running job `job` has an overdue line due at `now`.
fn job_alert_due(job: &Job, now: DateTime<Utc>) -> bool {
    job.estimate.is_some() && job.next_alert_at.is_some_and(|at| at <= now)
}

/// Write an `overdue` line for every running job past its next alert and move
/// that alert along the overdue grid ([`overdue_interval`]), exactly as a
/// task's estimate does. One whose line fails to append is retried next tick.
fn fire_job_overdue(reports: &mut Reports<'_>, store: &mut AwarenessStore, now: DateTime<Utc>) {
    for job in store.jobs.iter_mut().filter(|j| job_alert_due(j, now)) {
        let (Some(estimate), Some(next)) = (job.estimate, job.next_alert_at) else {
            continue;
        };
        let elapsed = (now - job.started_at).to_std().unwrap_or_default();
        let Some(notice) = JobNotice::overdue(job, elapsed) else {
            continue;
        };
        if let Err(e) = reports.send(job, notice) {
            tracing::warn!(project = reports.project, job = %job.id, error = %e, "job overdue event not written; retrying next tick");
            continue;
        }
        job.next_alert_at = Some(next_slot_after(next, overdue_interval(estimate), now));
    }
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
    let finished = |result: JobResult| result_outcome(&result, log.clone());
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
        status: JobStatus::Failed,
        exit: None,
        detail: Some("the job runner exited without recording a result".into()),
        log,
        finished_at: None,
    })
}

/// A command job's outcome from its runner's `result.json`.
fn result_outcome(result: &JobResult, log: Option<PathBuf>) -> JobOutcome {
    JobOutcome {
        status: if result.succeeded() { JobStatus::Ok } else { JobStatus::Failed },
        exit: result.exit_code.or(result.signal.map(|s| 128 + s)),
        detail: result.detail(),
        log,
        finished_at: Some(result.finished_at),
    }
}

fn pid_outcome(job: &Job, probe: &dyn AwarenessProbe) -> Option<JobOutcome> {
    let pid: u32 = job.target.parse().ok()?;
    (!probe.pid_alive(pid)).then_some(JobOutcome {
        status: JobStatus::Exited,
        exit: None,
        detail: None,
        log: None,
        finished_at: None,
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
        WatchVerdict::Passed => (JobStatus::Ok, None),
        WatchVerdict::Failed { check } => (JobStatus::Failed, Some(format!("failing check: {check}"))),
        WatchVerdict::Merged => (JobStatus::Merged, None),
        WatchVerdict::Closed => (JobStatus::Closed, None),
    };
    Some(JobOutcome {
        status,
        exit: None,
        detail,
        log: None,
        finished_at: None,
    })
}

fn maybe_prune_job_history(
    project: &str,
    store: &AwarenessStore,
    state: &mut AwarenessTickState,
    now: DateTime<Utc>,
) {
    if state
        .last_prune
        .is_some_and(|t| t.elapsed() < JOB_HISTORY_PRUNE_CADENCE)
    {
        return;
    }
    state.last_prune = Some(Instant::now());
    let live: Vec<&str> = store.jobs.iter().map(|j| j.id.as_str()).collect();
    awareness::prune_job_history(project, &live, now);
    remove_job_sessions(project, |id| !live.contains(&id));
}

/// Remove the session of every command job that left the store since the
/// last tick (it finished, or was canceled), once that session has exited.
fn clean_ended_job_sessions(project: &str, store: &AwarenessStore, state: &mut AwarenessTickState) {
    let current: HashSet<String> = store
        .jobs
        .iter()
        .filter(|j| j.kind == JobKind::Command)
        .map(|j| j.id.clone())
        .collect();
    for gone in state.command_jobs.difference(&current) {
        state.ended_sessions.entry(gone.clone()).or_insert(0);
    }
    state.command_jobs = current;
    if state.ended_sessions.is_empty() {
        return;
    }
    let up = remove_job_sessions(project, |id| state.ended_sessions.contains_key(id));
    state.ended_sessions.retain(|id, tries| {
        *tries += 1;
        up.contains(id) && *tries < ENDED_SESSION_TRIES
    });
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

    fn starter_live(&self, project: &Project, starter: &JobStarter) -> bool {
        crate::job_route::starter_live(project, starter)
    }

    fn deliver_to_starter(&self, project: &Project, starter: &JobStarter, notice: JobNotice) {
        let job = notice.id().to_string();
        let send = {
            let (project, starter, notice) = (project.name.clone(), starter.clone(), notice.clone());
            move || crate::job_route::deliver_or_fall_back(&project, &starter, &notice)
        };
        // A verified send blocks for seconds; the tick must not.
        if let Err(e) = std::thread::Builder::new().name(format!("job-report-{job}")).spawn(send) {
            tracing::warn!(project = %project.name, job, error = %e, "no thread to deliver the job report; reporting to the orchestrator");
            if let Err(e) = notice.append_to_feed(&project.name) {
                tracing::warn!(project = %project.name, job, error = %e, "job report lost");
            }
        }
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

/// Start `argv` as a detached command job for `project`: register the job and
/// start its runner (`shelbi __job-exec`, see [`crate::job_exec`]) as the
/// child of a Shelbi session of its own, [`job_session_name`], so it outlives
/// this CLI and any daemon restart, has a terminal a client can watch, and
/// leads the process group `job cancel` signals. The runner records its own
/// pid once it is up. `started_by` is the worker to report back to (see
/// [`crate::job_route::starter_from_env`]). Returns the job at once.
pub fn spawn_command_job(
    project: &str,
    argv: &[String],
    label: Option<String>,
    estimate: Option<Duration>,
    started_by: Option<JobStarter>,
) -> Result<Job> {
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
            estimate,
            started_by,
        },
    )?;
    let started = (|| -> Result<()> {
        let dir = awareness::job_dir(project, &job.id)?;
        shelbi_state::ensure_dir(&dir)?;
        crate::job_exec::write_job_env(project, &job.id)?;
        let exe = shelbi_core::current_shelbi_exe()
            .ok_or_else(|| Error::Other("cannot locate the shelbi binary".into()))?;
        let mut child_argv = vec![
            exe.to_string_lossy().into_owned(),
            "--project".to_string(),
            project.to_string(),
            "__job-exec".to_string(),
            job.id.clone(),
            "--".to_string(),
        ];
        child_argv.extend(argv.iter().cloned());
        let cwd = std::env::current_dir()
            .ok()
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("/"));
        let (cols, rows) = SessionProcessBackend::default_size();
        SessionProcessBackend.spawn_session(shelbi_session::SpawnSpec {
            name: job_session_name(project, &job.id),
            cwd,
            cols,
            rows,
            task: None,
            raw_output_log: false,
            child_argv,
        })?;
        Ok(())
    })();
    match started {
        Ok(()) => Ok(job),
        Err(e) => {
            let _ = awareness::remove_job(project, &job.id);
            if let Ok(dir) = awareness::job_dir(project, &job.id) {
                let _ = std::fs::remove_dir_all(dir);
            }
            Err(e)
        }
    }
}

/// Remove the directory of each of `project`'s job sessions
/// ([`job_session_name`]) that has exited, for the jobs `remove` picks. A
/// finished job's output stays in its `output.log`: the session was only its
/// live terminal. Matched on the exact name, so no other session is ever
/// touched. Returns the picked jobs whose session is still up.
pub fn remove_job_sessions(project: &str, remove: impl Fn(&str) -> bool) -> HashSet<String> {
    let mut up = HashSet::new();
    let Ok(root) = shelbi_state::sessions_dir() else {
        return up;
    };
    for session in shelbi_client::list(&root).unwrap_or_default() {
        let Some(id) = job_of_session(project, &session.meta.name) else {
            continue;
        };
        if !remove(id) {
            continue;
        }
        if session.alive {
            up.insert(id.to_string());
        } else {
            let _ = std::fs::remove_dir_all(&session.dir);
        }
    }
    up
}

/// Cancel job `id`: drop it from the store and, for a command job whose runner
/// is still alive, SIGTERM the runner's process group (the runner passes it on
/// to the command's).
/// The job goes into the history as `canceled` (or, for a command that had
/// already finished unreported, as it ended). Returns the removed job, or
/// `None` when there was none.
pub fn cancel_job(project: &str, id: &str) -> Result<Option<Job>> {
    let Some(job) = awareness::remove_job(project, id)? else {
        return Ok(None);
    };
    let now = Utc::now();
    let mut record = JobRecord::finished(&job, JobStatus::Canceled, now);
    if job.kind == JobKind::Command {
        record.log = awareness::job_output_path(project, id).ok();
        if let Ok(Some(result)) = awareness::read_job_result(project, id) {
            record = result_outcome(&result, record.log.clone()).record(&job, now);
        }
    }
    if let Err(e) = awareness::record_finished_jobs(project, vec![record], now) {
        tracing::warn!(project, job = %id, error = %e, "canceled job not recorded in the job history");
    }
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
        // A runner that hasn't recorded its pid yet is reached through its
        // session, by the job's exact session name: the session sends its
        // child's group SIGTERM, which the runner passes on to the command.
        if job.runner_pid.is_none() && matches!(awareness::read_job_result(project, id), Ok(None)) {
            let name = job_session_name(project, id);
            if let Err(e) = SessionProcessBackend.signal_by_name(&shelbi_core::Host::Local, &name, None) {
                tracing::warn!(project, job = %id, error = %e, "job session not signalled");
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
