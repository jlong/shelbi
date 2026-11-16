//! The orchestrator awareness store (`<project_dir>/awareness.json`): the
//! reminders, watched jobs and task estimates the orchestrator is waiting on.
//!
//! The orchestrator creates entries through `shelbi remind`, `shelbi job` and
//! `shelbi issue start --estimate` / `shelbi issue estimate`; the hub poller
//! evaluates the store on its supervisor tick and writes one `events.log` line
//! when an entry fires (a `reminder`, a `job`, or a task `overdue`), then
//! removes or advances the entry. Because the timing lives here, on disk, an
//! entry survives orchestrator restarts, runner switches and daemon restarts;
//! one that came due while the daemon was down fires once on the next tick.
//!
//! Every mutation goes through [`update_awareness`], a locked
//! read-modify-write on the sibling `awareness.json.lock`, the same discipline
//! `state.json` follows, so the CLI and the poller never lose each other's
//! writes.
//!
//! A `command` job also owns a directory, `<project_dir>/jobs/<id>/`, holding
//! the command's combined output (`output.log`) and, once it exits, its
//! `result.json`. The detached runner (`shelbi __job-exec`) writes both; the
//! poller reads the result and reports it.
//!
//! A job that finishes (or is canceled) leaves the store for the history,
//! `<project_dir>/jobs/history.json`: one [`JobRecord`] per finished job, with
//! how it ended and its log path. The history keeps the newest
//! [`JOB_HISTORY_MAX_JOBS`] jobs finished within [`JOB_HISTORY_MAX_AGE`],
//! whichever is fewer, and a job's directory goes when its record does.
//!
//! The store never creates the project's state directory: a write for a
//! project whose `<project_dir>` is missing fails instead, so a mistyped
//! `--project` (or a stale `SHELBI_PROJECT`) can't leave a stray project
//! folder holding entries no orchestrator will ever read.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::duration::serde_compact;
use shelbi_core::{Error, Result};

use crate::{acquire_file_lock, atomic_write, project_dir, read_to_string_at};

/// File name of the store under the project state dir.
pub const AWARENESS_FILE: &str = "awareness.json";

/// Directory under the project state dir holding each command job's output.
pub const JOBS_DIR: &str = "jobs";

/// The shortest repeat a recurring reminder may use.
pub const MIN_REMINDER_EVERY: Duration = Duration::from_secs(10 * 60);

/// The shortest gap between two overdue alerts for one task.
pub const MIN_OVERDUE_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// How many trailing output lines a finished command job keeps in its result.
pub const JOB_TAIL_LINES: usize = 20;

/// The finished-jobs history file, under [`JOBS_DIR`].
pub const JOB_HISTORY_FILE: &str = "history.json";

/// The most finished jobs a project's history keeps. Past it the oldest
/// record goes, and its job directory (its `output.log`) with it.
pub const JOB_HISTORY_MAX_JOBS: usize = 200;

/// The longest a finished job stays in the history. Whichever of this and
/// [`JOB_HISTORY_MAX_JOBS`] is smaller wins: a record (and its log) is kept
/// only while it is both among the newest 200 and under 30 days old.
pub const JOB_HISTORY_MAX_AGE: Duration = Duration::from_secs(30 * 24 * 3600);

/// The whole store. Each list is elided from the file while empty.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AwarenessStore {
    /// Counter behind the `r<N>` / `j<N>` ids, so an id is never reused within
    /// a project even after its entry fires.
    #[serde(default)]
    pub next_id: u64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reminders: Vec<Reminder>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<Job>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub estimates: Vec<Estimate>,
}

impl AwarenessStore {
    fn mint_id(&mut self, prefix: char) -> String {
        self.next_id += 1;
        format!("{prefix}{}", self.next_id)
    }

    pub fn estimate(&self, task_id: &str) -> Option<&Estimate> {
        self.estimates.iter().find(|e| e.task_id == task_id)
    }
}

/// "Tell me about this at `due_at`." One-shot unless `every` is set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reminder {
    pub id: String,
    pub due_at: DateTime<Utc>,
    pub text: String,
    pub created_at: DateTime<Utc>,
    /// Repeat interval for a recurring reminder (at least
    /// [`MIN_REMINDER_EVERY`]). The next `due_at` is the first slot after the
    /// firing tick, so a backlog of missed slots fires once, not once each.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_compact::option"
    )]
    pub every: Option<Duration>,
}

/// What a [`Job`] watches.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// A command Shelbi runs detached (`shelbi job run`).
    #[default]
    Command,
    /// An existing process, until it exits.
    Pid,
    /// A commit's CI result.
    Ci,
    /// A pull request reaching merged / closed, or its checks settling.
    Pr,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::Command => "command",
            JobKind::Pid => "pid",
            JobKind::Ci => "ci",
            JobKind::Pr => "pr",
        }
    }
}

impl std::fmt::Display for JobKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A watched job. `target` is what it watches: the command line, a pid, a
/// commit sha, or a PR number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Job {
    pub id: String,
    pub kind: JobKind,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub started_at: DateTime<Utc>,
    /// `command` only: the pid of the runner (`shelbi __job-exec`), which
    /// leads its process group in the job's session. The runner records it
    /// itself once it is up; `None` until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_pid: Option<u32>,
    /// How long the job is expected to run, counted from `started_at`
    /// (`job run --estimate`, `job watch --estimate`, revised by `job
    /// estimate`). Past it the tick writes a job `overdue` line, then again
    /// every [`overdue_interval`] until the job finishes. `None`: no alerts.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_compact::option"
    )]
    pub estimate: Option<Duration>,
    /// When the next overdue line is due: `started_at + estimate`, then
    /// advanced on the overdue grid each time one is written. Set exactly
    /// when `estimate` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_alert_at: Option<DateTime<Utc>>,
    /// The worker that started the job (`job run` / `job watch` from inside a
    /// workspace's agent session). Its result, and any overdue alert, go back
    /// to that session as a prompt while it still serves the task; otherwise
    /// (and for a job with no starter: the orchestrator's, the user's shell)
    /// they go to the orchestrator's feed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<JobStarter>,
}

/// Who started a job: the workspace whose agent session ran `shelbi job run`
/// / `job watch`, with the agent and task it was serving then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobStarter {
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

impl JobStarter {
    /// `alpha (developer, task-7)`, as the listings show it.
    pub fn describe(&self) -> String {
        let extra: Vec<&str> = [self.agent.as_deref(), self.task.as_deref()]
            .into_iter()
            .flatten()
            .collect();
        if extra.is_empty() {
            self.workspace.clone()
        } else {
            format!("{} ({})", self.workspace, extra.join(", "))
        }
    }
}

impl Job {
    /// The label the event line and listings show: the caller's `--label`, or
    /// a short description of the target.
    pub fn display_label(&self) -> String {
        job_label(self.kind, &self.target, self.label.as_deref())
    }

    /// Set (or replace) the estimate, counted from `started_at`. The first
    /// overdue line is due once it is used up, so a revision that is already
    /// exceeded alerts on the next tick.
    pub fn set_estimate(&mut self, estimate: Duration) {
        self.estimate = Some(estimate);
        self.next_alert_at = Some(self.started_at + chrono_duration(estimate));
    }
}

/// `d` as a chrono duration, saturating instead of failing.
fn chrono_duration(d: Duration) -> chrono::Duration {
    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::MAX)
}

/// A job's label: `label` when it has one, else a short description of what
/// a `kind` job on `target` watches. Shared by [`Job`] and [`JobRecord`].
pub fn job_label(kind: JobKind, target: &str, label: Option<&str>) -> String {
    if let Some(label) = label.filter(|l| !l.trim().is_empty()) {
        return label.to_string();
    }
    match kind {
        JobKind::Command => target.to_string(),
        JobKind::Pid => format!("pid {target}"),
        JobKind::Ci => format!("ci {}", short_sha(target)),
        JobKind::Pr => format!("PR #{target}"),
    }
}

/// A commit sha cut to the 12 characters the job listings show.
pub fn short_sha(sha: &str) -> &str {
    sha.get(..12).unwrap_or(sha)
}

/// How long a task is expected to stay active, and when it next alerts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Estimate {
    pub task_id: String,
    #[serde(with = "serde_compact")]
    pub estimate: Duration,
    pub started_at: DateTime<Utc>,
    pub next_alert_at: DateTime<Utc>,
    /// The workspace the task was started on, for the overdue line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
}

/// The repeat between overdue alerts: half the estimate, never under
/// [`MIN_OVERDUE_INTERVAL`]. A 90m task alerts at 90m, 135m, 180m, ...
pub fn overdue_interval(estimate: Duration) -> Duration {
    (estimate / 2).max(MIN_OVERDUE_INTERVAL)
}

/// The first `anchor + k * interval` (k >= 1) strictly after `now`. Used to
/// advance a recurring entry past every slot it missed, so a late tick fires
/// once and the schedule stays on its original grid.
pub fn next_slot_after(
    anchor: DateTime<Utc>,
    interval: Duration,
    now: DateTime<Utc>,
) -> DateTime<Utc> {
    let step = chrono::Duration::from_std(interval).unwrap_or(chrono::Duration::MAX);
    let step_secs = step.num_seconds().max(1);
    let behind = (now - anchor).num_seconds();
    let k = if behind < 0 { 1 } else { behind / step_secs + 1 };
    anchor + chrono::Duration::seconds(step_secs.saturating_mul(k))
}

/// `<project_dir>/awareness.json`.
pub fn awareness_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join(AWARENESS_FILE))
}

/// The project's state dir, which must already exist: every registered
/// project has one (it holds the project's config and state), and the store's
/// writers refuse to create it for a name that isn't one.
fn existing_project_dir(project: &str) -> Result<PathBuf> {
    let dir = project_dir(project)?;
    if dir.is_dir() {
        return Ok(dir);
    }
    Err(Error::Other(format!(
        "project `{project}` has no state directory ({}); is it registered? The awareness \
         store does not create one",
        dir.display()
    )))
}

/// Read the store. A missing file is an empty store; any other read or parse
/// error propagates, so a transiently unreadable file is never replaced with
/// an empty one by the next write.
pub fn read_awareness(project: &str) -> Result<AwarenessStore> {
    read_awareness_at(&awareness_path(project)?)
}

fn read_awareness_at(path: &Path) -> Result<AwarenessStore> {
    let text = match read_to_string_at(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(AwarenessStore::default()),
        Err(e) => return Err(Error::Io(e)),
    };
    serde_json::from_str(&text).map_err(|e| Error::Other(format!("{}: {e}", path.display())))
}

/// Locked read-modify-write on the store. The write is skipped when `f`
/// leaves the store unchanged, and an `Err` from `f` aborts without writing.
/// `f` must not call another awareness mutator for the same project (the lock
/// is not reentrant).
pub fn update_awareness<R>(
    project: &str,
    f: impl FnOnce(&mut AwarenessStore) -> Result<R>,
) -> Result<R> {
    let path = existing_project_dir(project)?.join(AWARENESS_FILE);
    let _lock = acquire_file_lock(&crate::sibling_lock_path(&path))?;
    let mut store = read_awareness_at(&path)?;
    let before = store.clone();
    let out = f(&mut store)?;
    if store != before {
        let body = serde_json::to_vec_pretty(&store)
            .map_err(|e| Error::Other(format!("{AWARENESS_FILE}: {e}")))?;
        atomic_write(&path, &body)?;
    }
    Ok(out)
}

/// Add a reminder due at `due_at`. `every`, when set, must be at least
/// [`MIN_REMINDER_EVERY`].
pub fn add_reminder(
    project: &str,
    due_at: DateTime<Utc>,
    text: &str,
    every: Option<Duration>,
) -> Result<Reminder> {
    if text.trim().is_empty() {
        return Err(Error::Other("a reminder needs some text".into()));
    }
    if let Some(every) = every {
        if every < MIN_REMINDER_EVERY {
            return Err(Error::Other(format!(
                "--every must be at least {}",
                shelbi_core::format_duration(MIN_REMINDER_EVERY)
            )));
        }
    }
    update_awareness(project, |store| {
        let reminder = Reminder {
            id: store.mint_id('r'),
            due_at,
            text: text.trim().to_string(),
            created_at: Utc::now(),
            every,
        };
        store.reminders.push(reminder.clone());
        Ok(reminder)
    })
}

/// Remove reminder `id`. Returns it, or `None` when there was none.
pub fn cancel_reminder(project: &str, id: &str) -> Result<Option<Reminder>> {
    update_awareness(project, |store| {
        let idx = store.reminders.iter().position(|r| r.id == id);
        Ok(idx.map(|i| store.reminders.remove(i)))
    })
}

/// A job as the CLI registers it, before it has an id.
#[derive(Debug, Clone, Default)]
pub struct NewJob {
    pub kind: JobKind,
    pub target: String,
    pub label: Option<String>,
    /// `--estimate`: how long it should take (see [`Job::estimate`]).
    pub estimate: Option<Duration>,
    /// The worker starting it (see [`Job::started_by`]).
    pub started_by: Option<JobStarter>,
}

/// Register a job and return it with its minted id.
pub fn add_job(project: &str, job: NewJob) -> Result<Job> {
    validate_job_estimate(job.estimate)?;
    update_awareness(project, |store| {
        let mut new = Job {
            id: store.mint_id('j'),
            kind: job.kind,
            target: job.target,
            label: job.label.filter(|l| !l.trim().is_empty()),
            started_at: Utc::now(),
            runner_pid: None,
            estimate: None,
            next_alert_at: None,
            started_by: job.started_by,
        };
        if let Some(estimate) = job.estimate {
            new.set_estimate(estimate);
        }
        store.jobs.push(new.clone());
        Ok(new)
    })
}

/// A zero estimate would alert on the first tick and mean nothing.
fn validate_job_estimate(estimate: Option<Duration>) -> Result<()> {
    match estimate {
        Some(d) if d.is_zero() => Err(Error::Other("an estimate must be longer than zero".into())),
        _ => Ok(()),
    }
}

/// Set or revise running job `id`'s estimate, counted from when it started.
/// Returns the job as revised, or `None` when no running job has that id.
pub fn set_job_estimate(project: &str, id: &str, estimate: Duration) -> Result<Option<Job>> {
    validate_job_estimate(Some(estimate))?;
    update_awareness(project, |store| {
        Ok(store.jobs.iter_mut().find(|j| j.id == id).map(|job| {
            job.set_estimate(estimate);
            job.clone()
        }))
    })
}

/// Record the detached runner's pid on command job `id`.
pub fn set_job_runner_pid(project: &str, id: &str, pid: u32) -> Result<()> {
    update_awareness(project, |store| {
        if let Some(job) = store.jobs.iter_mut().find(|j| j.id == id) {
            job.runner_pid = Some(pid);
        }
        Ok(())
    })
}

/// Remove job `id`. Returns it, or `None` when there was none.
pub fn remove_job(project: &str, id: &str) -> Result<Option<Job>> {
    update_awareness(project, |store| {
        let idx = store.jobs.iter().position(|j| j.id == id);
        Ok(idx.map(|i| store.jobs.remove(i)))
    })
}

/// Set (or replace) `task_id`'s estimate. The first alert is due `estimate`
/// after `started_at`; a revision that is already exceeded alerts on the next
/// tick.
pub fn set_estimate(
    project: &str,
    task_id: &str,
    estimate: Duration,
    started_at: DateTime<Utc>,
    workspace: Option<&str>,
) -> Result<Estimate> {
    let first = started_at + chrono_duration(estimate);
    let entry = Estimate {
        task_id: task_id.to_string(),
        estimate,
        started_at,
        next_alert_at: first,
        workspace: workspace.map(str::to_string),
    };
    update_awareness(project, |store| {
        store.estimates.retain(|e| e.task_id != task_id);
        store.estimates.push(entry.clone());
        Ok(entry)
    })
}

/// Drop `task_id`'s estimate. Returns it, or `None` when there was none.
pub fn clear_estimate(project: &str, task_id: &str) -> Result<Option<Estimate>> {
    update_awareness(project, |store| {
        let idx = store.estimates.iter().position(|e| e.task_id == task_id);
        Ok(idx.map(|i| store.estimates.remove(i)))
    })
}

// ---------------------------------------------------------------------------
// Command job files

/// The session a command job runs in: `<project>/job/<id>`. Workspace
/// sessions are `<project>/ws/<workspace>[/<role>]` and the orchestrator's
/// is `<project>/orch`, so a job's never collides with either.
pub fn job_session_name(project: &str, id: &str) -> String {
    format!("{project}/{JOB_SESSION_SEGMENT}/{id}")
}

/// The segment after the project in a job session's name.
pub const JOB_SESSION_SEGMENT: &str = "job";

/// The job id a session named [`job_session_name`] for `project` runs.
pub fn job_of_session<'a>(project: &str, name: &'a str) -> Option<&'a str> {
    name.strip_prefix(project)?
        .strip_prefix('/')?
        .strip_prefix(JOB_SESSION_SEGMENT)?
        .strip_prefix('/')
        .filter(|id| !id.is_empty() && !id.contains('/'))
}

/// Whether the session `name` is some project's job session.
pub fn is_job_session(name: &str) -> bool {
    let mut parts = name.split('/');
    matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(p), Some(JOB_SESSION_SEGMENT), Some(id), None) if !p.is_empty() && !id.is_empty()
    )
}


/// `<project_dir>/jobs/<id>/`. The id must be a single plain path component.
pub fn job_dir(project: &str, id: &str) -> Result<PathBuf> {
    crate::ensure_flat_path_component("job id", id)?;
    Ok(project_dir(project)?.join(JOBS_DIR).join(id))
}

/// The command's combined stdout + stderr.
pub fn job_output_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(job_dir(project, id)?.join("output.log"))
}

/// Written by the runner once the command exits.
pub fn job_result_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(job_dir(project, id)?.join("result.json"))
}

/// How a command job ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobResult {
    /// The exit code, or `None` when a signal ended it.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// The terminating signal, when there was one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    pub finished_at: DateTime<Utc>,
    /// The last [`JOB_TAIL_LINES`] lines of output.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tail: Vec<String>,
    /// Why the command could not be started at all, when it couldn't.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl JobResult {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0) && self.error.is_none()
    }

    /// The one-line summary the event carries: the spawn error, else the last
    /// non-blank output line.
    pub fn detail(&self) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        self.tail
            .iter()
            .rev()
            .find(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
    }
}

/// Atomically write job `id`'s result.
pub fn write_job_result(project: &str, id: &str, result: &JobResult) -> Result<()> {
    existing_project_dir(project)?;
    let body = serde_json::to_vec_pretty(result)
        .map_err(|e| Error::Other(format!("job result: {e}")))?;
    atomic_write(&job_result_path(project, id)?, &body)
}

/// Job `id`'s result, or `None` while it is still running.
pub fn read_job_result(project: &str, id: &str) -> Result<Option<JobResult>> {
    let path = job_result_path(project, id)?;
    let text = match read_to_string_at(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(Error::Io(e)),
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| Error::Other(format!("{}: {e}", path.display())))
}

/// The last `n` lines of `path` (lossy UTF-8), reading at most the final
/// 64 KiB so a huge log costs nothing extra. Missing or unreadable → empty.
pub fn tail_lines(path: &Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    const WINDOW: u64 = 64 * 1024;
    let Ok(mut file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(WINDOW);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return Vec::new();
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.lines().collect();
    // A window that starts mid-file starts mid-line: drop the partial line.
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let skip = lines.len().saturating_sub(n);
    lines[skip..].iter().map(|l| l.to_string()).collect()
}

/// Where the last `n` lines of the file at `path` start, as a byte offset,
/// `tail -n` style: a final newline ends the last line rather than starting
/// an empty one. Reads backwards in chunks, so a long log costs only what it
/// skips over. `n == 0` is the end of the file.
pub fn last_lines_offset(path: &Path, n: usize) -> std::io::Result<u64> {
    use std::io::{Read, Seek, SeekFrom};
    const CHUNK: u64 = 64 * 1024;
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    if n == 0 || len == 0 {
        return Ok(len);
    }
    let mut end = len;
    let mut newlines = 0usize;
    let mut skip_final = true;
    let mut buf = vec![0u8; CHUNK as usize];
    while end > 0 {
        let start = end.saturating_sub(CHUNK);
        let size = (end - start) as usize;
        file.seek(SeekFrom::Start(start))?;
        file.read_exact(&mut buf[..size])?;
        for i in (0..size).rev() {
            if buf[i] != b'\n' {
                skip_final = false;
                continue;
            }
            if std::mem::take(&mut skip_final) {
                // The newline that ends the file's last line.
                continue;
            }
            newlines += 1;
            if newlines == n {
                return Ok(start + i as u64 + 1);
            }
        }
        end = start;
    }
    Ok(0)
}

// ---------------------------------------------------------------------------
// Finished-job history

/// How a finished job ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// A command exited 0, or the watched CI / PR checks passed.
    Ok,
    /// A command exited non-zero (or never started, or lost its runner), or
    /// a watched check failed.
    Failed,
    /// A watched process exited.
    Exited,
    /// A watched PR merged.
    Merged,
    /// A watched PR closed without merging.
    Closed,
    /// `shelbi job cancel` stopped it.
    #[serde(alias = "cancelled")]
    Canceled,
}

impl JobStatus {
    /// The word the `job` event's `status=` and the listings use.
    pub fn as_str(self) -> &'static str {
        match self {
            JobStatus::Ok => "ok",
            JobStatus::Failed => "failed",
            JobStatus::Exited => "exited",
            JobStatus::Merged => "merged",
            JobStatus::Closed => "closed",
            JobStatus::Canceled => "canceled",
        }
    }
}

impl std::fmt::Display for JobStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A finished job, as the history keeps it: the [`Job`] it was, plus how and
/// when it ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRecord {
    pub id: String,
    pub kind: JobKind,
    /// The command line, or the pid / commit sha / PR number watched.
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: JobStatus,
    /// A command's exit code (128 + the signal when a signal ended it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// The event's `detail`: a command's last output line (or why it never
    /// started), a watch's failing check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// A command's `output.log`, while the history keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<PathBuf>,
    /// The estimate the job had when it finished, so the history shows
    /// estimate against actual ([`Self::duration`]).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "serde_compact::option"
    )]
    pub estimate: Option<Duration>,
    /// The worker that started it (see [`Job::started_by`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_by: Option<JobStarter>,
}

impl JobRecord {
    /// The record for `job`, finished at `finished_at` with `status`.
    pub fn finished(job: &Job, status: JobStatus, finished_at: DateTime<Utc>) -> Self {
        JobRecord {
            id: job.id.clone(),
            kind: job.kind,
            target: job.target.clone(),
            label: job.label.clone(),
            started_at: job.started_at,
            finished_at,
            status,
            exit_code: None,
            detail: None,
            log: None,
            estimate: job.estimate,
            started_by: job.started_by.clone(),
        }
    }

    /// The label the listings show (see [`Job::display_label`]).
    pub fn display_label(&self) -> String {
        job_label(self.kind, &self.target, self.label.as_deref())
    }

    /// How long it ran. A skewed clock reads zero, never negative.
    pub fn duration(&self) -> Duration {
        (self.finished_at - self.started_at).to_std().unwrap_or_default()
    }
}

/// The history file's shape: the records, oldest first.
#[derive(Debug, Default, Serialize, Deserialize)]
struct JobHistoryFile {
    #[serde(default)]
    jobs: Vec<JobRecord>,
}

/// `<project_dir>/jobs/history.json`.
pub fn job_history_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join(JOBS_DIR).join(JOB_HISTORY_FILE))
}

/// The numeric part of a `j<N>` id, for ordering jobs minted in one second.
pub fn job_id_number(id: &str) -> u64 {
    id.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(0)
}

fn newest_first(records: &mut [JobRecord]) {
    records.sort_by(|a, b| {
        b.finished_at
            .cmp(&a.finished_at)
            .then_with(|| job_id_number(&b.id).cmp(&job_id_number(&a.id)))
    });
}

/// The project's finished jobs, newest first. A missing file is no history;
/// any other read or parse error propagates.
pub fn read_job_history(project: &str) -> Result<Vec<JobRecord>> {
    read_job_history_at(&job_history_path(project)?)
}

fn read_job_history_at(path: &Path) -> Result<Vec<JobRecord>> {
    let text = match read_to_string_at(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut file: JobHistoryFile =
        serde_json::from_str(&text).map_err(|e| Error::Other(format!("{}: {e}", path.display())))?;
    newest_first(&mut file.jobs);
    Ok(file.jobs)
}

/// The finished job `id`, if the history still has it.
pub fn find_job_record(project: &str, id: &str) -> Result<Option<JobRecord>> {
    Ok(read_job_history(project)?.into_iter().find(|r| r.id == id))
}

/// Split `records` (newest first) into those the retention window keeps at
/// `now` and those it drops: the newest [`JOB_HISTORY_MAX_JOBS`] that
/// finished within [`JOB_HISTORY_MAX_AGE`].
fn retention_split(records: Vec<JobRecord>, now: DateTime<Utc>) -> (Vec<JobRecord>, Vec<JobRecord>) {
    let oldest = now - chrono::Duration::from_std(JOB_HISTORY_MAX_AGE).unwrap_or(chrono::Duration::MAX);
    let (mut kept, mut dropped) = (Vec::new(), Vec::new());
    for record in records {
        if kept.len() < JOB_HISTORY_MAX_JOBS && record.finished_at >= oldest {
            kept.push(record);
        } else {
            dropped.push(record);
        }
    }
    (kept, dropped)
}

/// Locked read-modify-write on the history: `f` edits the records (newest
/// first), then the retention window applies at `now` and the directories of
/// the records it dropped are removed. Written only when something changed.
fn update_job_history(
    project: &str,
    now: DateTime<Utc>,
    f: impl FnOnce(&mut Vec<JobRecord>),
) -> Result<Vec<JobRecord>> {
    let jobs = existing_project_dir(project)?.join(JOBS_DIR);
    crate::ensure_dir(&jobs)?;
    let path = jobs.join(JOB_HISTORY_FILE);
    let _lock = acquire_file_lock(&crate::sibling_lock_path(&path))?;
    let before = read_job_history_at(&path)?;
    let mut records = before.clone();
    f(&mut records);
    newest_first(&mut records);
    let (kept, dropped) = retention_split(records, now);
    if kept != before {
        let mut oldest_first = kept.clone();
        oldest_first.reverse();
        let body = serde_json::to_vec_pretty(&JobHistoryFile { jobs: oldest_first })
            .map_err(|e| Error::Other(format!("{JOB_HISTORY_FILE}: {e}")))?;
        atomic_write(&path, &body)?;
    }
    for record in &dropped {
        if let Ok(dir) = job_dir(project, &record.id) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    Ok(dropped)
}

/// Add `records` to the history, replacing any record with the same id (so a
/// tick that is retried after a failed store write never lists a job twice),
/// then prune past the retention window.
pub fn record_finished_jobs(project: &str, records: Vec<JobRecord>, now: DateTime<Utc>) -> Result<()> {
    if records.is_empty() {
        return Ok(());
    }
    update_job_history(project, now, |history| {
        history.retain(|old| !records.iter().any(|r| r.id == old.id));
        history.extend(records);
    })
    .map(drop)
}

/// The tick's sweep: drop history records past the retention window (and
/// their directories), then remove every job directory that neither a live
/// job (`live_ids`) nor a kept record owns once it is older than
/// [`JOB_HISTORY_MAX_AGE`] (one from before the history existed, or from a
/// runner that failed to start). Best effort: errors are skipped. Returns
/// the ids of the records it dropped.
pub fn prune_job_history(project: &str, live_ids: &[&str], now: DateTime<Utc>) -> Vec<String> {
    let Ok(dir) = project_dir(project) else {
        return Vec::new();
    };
    let root = dir.join(JOBS_DIR);
    if !root.is_dir() {
        return Vec::new();
    }
    let dropped = update_job_history(project, now, |_| {}).unwrap_or_default();
    let Ok(kept) = read_job_history(project) else {
        return dropped.into_iter().map(|r| r.id).collect();
    };
    if let Ok(entries) = std::fs::read_dir(&root) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if live_ids.contains(&name) || kept.iter().any(|r| r.id == name) {
                continue;
            }
            let old = entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > JOB_HISTORY_MAX_AGE);
            if old {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    dropped.into_iter().map(|r| r.id).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    struct Home {
        path: PathBuf,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            let lock = TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let path = std::env::temp_dir().join(format!(
                "shelbi-awareness-{tag}-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(path.join("projects/demo")).unwrap();
            std::env::set_var("SHELBI_HOME", &path);
            Self { path, _lock: lock }
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn mins(n: u64) -> Duration {
        Duration::from_secs(n * 60)
    }

    #[test]
    fn writes_refuse_a_project_without_a_state_dir() {
        let home = Home::new("nodir");
        let due = Utc::now();
        let err = add_reminder("typo", due, "lost", None).unwrap_err().to_string();
        assert!(err.contains("project `typo` has no state directory"), "{err}");
        let pid = NewJob { kind: JobKind::Pid, target: "1".into(), label: None, estimate: None, started_by: None };
        assert!(add_job("typo", pid).is_err());
        assert!(set_estimate("typo", "t1", mins(30), due, None).is_err());
        let result = JobResult {
            exit_code: Some(0),
            signal: None,
            finished_at: due,
            tail: Vec::new(),
            error: None,
        };
        assert!(write_job_result("typo", "j1", &result).is_err());
        assert!(!home.path.join("projects/typo").exists());
        // A read of a missing project stays an empty store, and an existing
        // project still takes writes.
        assert!(read_awareness("typo").unwrap().reminders.is_empty());
        add_reminder("demo", due, "kept", None).unwrap();
    }

    #[test]
    fn ids_are_minted_per_project_and_never_reused() {
        let _home = Home::new("ids");
        let due = Utc::now();
        let a = add_reminder("demo", due, "a", None).unwrap();
        let j = add_job(
            "demo",
            NewJob { kind: JobKind::Pid, target: "1".into(), label: None, estimate: None, started_by: None },
        )
        .unwrap();
        assert_eq!((a.id.as_str(), j.id.as_str()), ("r1", "j2"));
        cancel_reminder("demo", "r1").unwrap();
        let b = add_reminder("demo", due, "b", None).unwrap();
        assert_eq!(b.id, "r3");
    }

    #[test]
    fn the_store_round_trips_with_compact_durations() {
        let _home = Home::new("roundtrip");
        add_reminder("demo", Utc::now(), "check the tap", Some(mins(30))).unwrap();
        set_estimate("demo", "t-1", mins(45), Utc::now(), Some("alpha")).unwrap();
        let text = std::fs::read_to_string(awareness_path("demo").unwrap()).unwrap();
        assert!(text.contains("\"every\": \"30m\""), "{text}");
        assert!(text.contains("\"estimate\": \"45m\""), "{text}");
        assert!(!text.contains("\"jobs\""), "empty lists are elided: {text}");
        let store = read_awareness("demo").unwrap();
        assert_eq!(store.reminders[0].every, Some(mins(30)));
        assert_eq!(store.estimate("t-1").unwrap().workspace.as_deref(), Some("alpha"));
    }

    #[test]
    fn reminders_reject_empty_text_and_a_short_repeat() {
        let _home = Home::new("validate");
        assert!(add_reminder("demo", Utc::now(), "  ", None).is_err());
        assert!(add_reminder("demo", Utc::now(), "x", Some(mins(5))).is_err());
        assert!(add_reminder("demo", Utc::now(), "x", Some(mins(10))).is_ok());
    }

    #[test]
    fn set_estimate_replaces_and_clear_removes() {
        let _home = Home::new("estimate");
        let start = Utc::now();
        set_estimate("demo", "t", mins(10), start, None).unwrap();
        let revised = set_estimate("demo", "t", mins(60), start, None).unwrap();
        assert_eq!(revised.next_alert_at, start + chrono::Duration::minutes(60));
        assert_eq!(read_awareness("demo").unwrap().estimates.len(), 1);
        assert!(clear_estimate("demo", "t").unwrap().is_some());
        assert!(clear_estimate("demo", "t").unwrap().is_none());
    }

    #[test]
    fn a_job_estimate_is_kept_on_the_job_and_its_record() {
        let _home = Home::new("job-estimate");
        let job = add_job(
            "demo",
            NewJob { kind: JobKind::Pid, target: "1".into(), label: None, estimate: Some(mins(30)), started_by: None },
        )
        .unwrap();
        assert_eq!(job.next_alert_at, Some(job.started_at + chrono::Duration::minutes(30)));
        let text = std::fs::read_to_string(awareness_path("demo").unwrap()).unwrap();
        assert!(text.contains("\"estimate\": \"30m\""), "{text}");
        let revised = set_job_estimate("demo", &job.id, mins(90)).unwrap().unwrap();
        assert_eq!(revised.next_alert_at, Some(job.started_at + chrono::Duration::minutes(90)));
        assert!(set_job_estimate("demo", "j99", mins(5)).unwrap().is_none());
        let zero = NewJob { kind: JobKind::Pid, target: "1".into(), label: None, estimate: Some(Duration::ZERO), started_by: None };
        assert!(add_job("demo", zero).is_err());
        // A job from before estimates reads as having none.
        let old: Job = serde_json::from_str(
            r#"{"id":"j1","kind":"pid","target":"1","started_at":"2026-11-10T10:00:00Z"}"#,
        )
        .unwrap();
        assert_eq!((old.estimate, old.next_alert_at), (None, None));
        let record = JobRecord::finished(&revised, JobStatus::Exited, Utc::now());
        assert_eq!(record.estimate, Some(mins(90)));
        record_finished_jobs("demo", vec![record], Utc::now()).unwrap();
        let text = std::fs::read_to_string(job_history_path("demo").unwrap()).unwrap();
        assert!(text.contains("\"estimate\": \"1h30m\""), "{text}");
    }

    #[test]
    fn overdue_repeats_every_half_the_estimate_with_a_ten_minute_floor() {
        assert_eq!(overdue_interval(mins(90)), mins(45));
        assert_eq!(overdue_interval(mins(10)), mins(10));
        assert_eq!(overdue_interval(mins(30)), mins(15));
    }

    #[test]
    fn next_slot_skips_every_missed_slot() {
        let anchor = DateTime::parse_from_rfc3339("2026-11-10T10:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let at = |m: i64| anchor + chrono::Duration::minutes(m);
        assert_eq!(next_slot_after(anchor, mins(10), at(0)), at(10));
        assert_eq!(next_slot_after(anchor, mins(10), at(5)), at(10));
        assert_eq!(next_slot_after(anchor, mins(10), at(10)), at(20));
        assert_eq!(next_slot_after(anchor, mins(10), at(47)), at(50));
        assert_eq!(next_slot_after(anchor, mins(10), at(-3)), at(10));
    }

    fn log_lines() -> Vec<String> {
        let path = crate::events_log_path().unwrap();
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .map(|l| l.split_once(' ').map(|x| x.1).unwrap_or("").to_string())
            .collect()
    }

    #[test]
    fn awareness_event_lines_have_the_documented_shape_and_kinds() {
        let _home = Home::new("events");
        crate::append_reminder_event("demo", "r1", "check the \"tap\" PR\nheartbeat ci task=x")
            .unwrap();
        crate::append_job_event(
            "demo",
            &crate::JobEvent {
                id: "j2",
                kind: "command",
                label: "desktop tests",
                status: "failed",
                exit: Some(3),
                detail: Some("error: 1 test failed"),
                log: Some(Path::new("/tmp/jobs/j2/output.log")),
            },
        )
        .unwrap();
        crate::append_task_overdue_event("demo", "t-1", mins(45), Duration::from_secs(70 * 60 + 12), Some("alpha"))
            .unwrap();
        crate::append_job_overdue_event("demo", "j3", mins(30), Duration::from_secs(45 * 60 + 40), "bundle \"app\"")
            .unwrap();
        let lines = log_lines();
        assert_eq!(
            lines,
            [
                "project=demo reminder id=r1 text=\"check the 'tap' PR heartbeat ci task=x\"",
                "project=demo job id=j2 kind=command label=\"desktop tests\" status=failed exit=3 detail=\"error: 1 test failed\" log=/tmp/jobs/j2/output.log",
                "project=demo task=t-1 overdue estimate=45m elapsed=1h10m workspace=alpha",
                "project=demo job id=j3 overdue estimate=30m elapsed=45m label=\"bundle 'app'\"",
            ]
        );
        let path = crate::events_log_path().unwrap();
        let kinds: Vec<_> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| crate::EventEnvelope::from_log_line(l).kind)
            .collect();
        assert_eq!(
            kinds,
            [
                crate::EventKind::Reminder,
                crate::EventKind::JobDone,
                crate::EventKind::TaskOverdue,
                crate::EventKind::JobOverdue
            ]
        );
    }

    #[test]
    fn split_event_words_keeps_quoted_values_whole() {
        assert_eq!(
            crate::split_event_words(r#"ts project=p reminder id=r1 text="a b  c" x=1"#),
            ["ts", "project=p", "reminder", "id=r1", r#"text="a b  c""#, "x=1"]
        );
        assert_eq!(crate::split_event_words("  a   b "), ["a", "b"]);
        // A stray quote inside an unquoted value doesn't swallow later words.
        assert_eq!(
            crate::split_event_words(r#"t reason=it"s fine x=1 y="a b" z=a="b c""#),
            ["t", r#"reason=it"s"#, "fine", "x=1", r#"y="a b""#, r#"z=a="b"#, r#"c""#]
        );
        assert_eq!(crate::unquote_event_value(r#""a b""#), "a b");
        assert_eq!(crate::unquote_event_value("plain"), "plain");
    }

    #[test]
    fn job_results_and_tails() {
        let _home = Home::new("jobs");
        assert!(read_job_result("demo", "j1").unwrap().is_none());
        let out = job_output_path("demo", "j1").unwrap();
        std::fs::create_dir_all(out.parent().unwrap()).unwrap();
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&out, format!("{body}\n")).unwrap();
        let tail = tail_lines(&out, JOB_TAIL_LINES);
        assert_eq!(tail.len(), 20);
        assert_eq!(tail.first().map(String::as_str), Some("line 12"));
        let result = JobResult {
            exit_code: Some(3),
            signal: None,
            finished_at: Utc::now(),
            tail,
            error: None,
        };
        write_job_result("demo", "j1", &result).unwrap();
        let back = read_job_result("demo", "j1").unwrap().unwrap();
        assert!(!back.succeeded());
        assert_eq!(back.detail().as_deref(), Some("line 30"));
        assert!(job_dir("demo", "../x").is_err());
    }

    fn record(id: &str, finished_at: DateTime<Utc>) -> JobRecord {
        let job = Job {
            id: id.into(),
            kind: JobKind::Command,
            target: "cargo test".into(),
            label: None,
            started_at: finished_at - chrono::Duration::seconds(90),
            runner_pid: None,
            estimate: None,
            next_alert_at: None,
            started_by: None,
        };
        JobRecord { exit_code: Some(0), ..JobRecord::finished(&job, JobStatus::Ok, finished_at) }
    }

    fn history_ids() -> Vec<String> {
        read_job_history("demo").unwrap().into_iter().map(|r| r.id).collect()
    }

    #[test]
    fn the_history_is_newest_first_and_a_rerecord_replaces() {
        let _home = Home::new("history");
        let now = Utc::now();
        let t = |m: i64| now - chrono::Duration::minutes(m);
        record_finished_jobs("demo", vec![record("j1", t(30)), record("j3", t(10))], now).unwrap();
        record_finished_jobs("demo", vec![record("j2", t(20))], now).unwrap();
        assert_eq!(history_ids(), ["j3", "j2", "j1"]);
        // A retried tick records j2 again: still listed once.
        let mut again = record("j2", t(20));
        again.status = JobStatus::Failed;
        record_finished_jobs("demo", vec![again], now).unwrap();
        assert_eq!(history_ids(), ["j3", "j2", "j1"]);
        let j2 = find_job_record("demo", "j2").unwrap().unwrap();
        assert_eq!(j2.status, JobStatus::Failed);
        assert_eq!(j2.duration(), Duration::from_secs(90));
        // The file is oldest first, so it reads like a log.
        let text = std::fs::read_to_string(job_history_path("demo").unwrap()).unwrap();
        assert!(text.find("\"j1\"").unwrap() < text.find("\"j3\"").unwrap(), "{text}");
        assert!(text.contains("\"status\": \"failed\""), "{text}");
    }

    #[test]
    fn retention_drops_the_oldest_past_the_count_with_their_logs() {
        let _home = Home::new("retain-count");
        let now = Utc::now();
        let records: Vec<JobRecord> = (1..=JOB_HISTORY_MAX_JOBS + 3)
            .map(|i| record(&format!("j{i}"), now - chrono::Duration::seconds((1000 - i) as i64)))
            .collect();
        for r in &records[..4] {
            let out = job_output_path("demo", &r.id).unwrap();
            std::fs::create_dir_all(out.parent().unwrap()).unwrap();
            std::fs::write(out, "x\n").unwrap();
        }
        record_finished_jobs("demo", records, now).unwrap();
        let ids = history_ids();
        assert_eq!(ids.len(), JOB_HISTORY_MAX_JOBS);
        assert_eq!(ids.first().map(String::as_str), Some("j203"));
        assert_eq!(ids.last().map(String::as_str), Some("j4"));
        // The three oldest went, and their directories with them; j4's stays.
        for gone in ["j1", "j2", "j3"] {
            assert!(!job_dir("demo", gone).unwrap().exists(), "{gone}");
        }
        assert!(job_output_path("demo", "j4").unwrap().exists());
    }

    #[test]
    fn retention_drops_records_past_the_age_limit() {
        let _home = Home::new("retain-age");
        let now = Utc::now();
        let limit = chrono::Duration::from_std(JOB_HISTORY_MAX_AGE).unwrap();
        record_finished_jobs(
            "demo",
            vec![
                record("j1", now - limit - chrono::Duration::hours(1)),
                record("j2", now - limit + chrono::Duration::hours(1)),
            ],
            now,
        )
        .unwrap();
        assert_eq!(history_ids(), ["j2"]);
        // A day later the sweep ages j2 out too, and leaves a live job's dir.
        let live = job_dir("demo", "j9").unwrap();
        std::fs::create_dir_all(&live).unwrap();
        let dropped = prune_job_history("demo", &["j9"], now + chrono::Duration::days(1));
        assert_eq!(dropped, ["j2"]);
        assert!(history_ids().is_empty());
        assert!(live.exists());
    }

    #[test]
    fn last_lines_offset_is_tail_n() {
        let _home = Home::new("tail-n");
        let path = job_output_path("demo", "j1").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let tail = |n: usize| {
            let at = last_lines_offset(&path, n).unwrap() as usize;
            std::fs::read_to_string(&path).unwrap()[at..].to_string()
        };
        let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
        std::fs::write(&path, &body).unwrap();
        assert_eq!(tail(2), "line 29\nline 30\n");
        assert_eq!(tail(1), "line 30\n");
        assert_eq!(tail(0), "");
        assert_eq!(tail(30), body);
        assert_eq!(tail(500), body);
        // No final newline: the last partial line counts as a line.
        std::fs::write(&path, "a\nb\nc").unwrap();
        assert_eq!(tail(2), "b\nc");
        // Blank lines count.
        std::fs::write(&path, "a\n\n\n").unwrap();
        assert_eq!(tail(2), "\n\n");
        std::fs::write(&path, "").unwrap();
        assert_eq!(tail(5), "");
        // Longer than one read chunk.
        let big: String = (0..20_000).map(|i| format!("{i:08}\n")).collect();
        std::fs::write(&path, &big).unwrap();
        assert_eq!(tail(3), "00019997\n00019998\n00019999\n");
        assert_eq!(tail(15_000).lines().count(), 15_000);
    }

    #[test]
    fn job_session_names_sit_apart_from_workspace_sessions() {
        let name = job_session_name("demo", "j12");
        assert_eq!(name, "demo/job/j12");
        assert_eq!(job_of_session("demo", &name), Some("j12"));
        assert!(is_job_session(&name));
        assert_eq!(job_of_session("dem", &name), None);
        assert_eq!(job_of_session("demo", "demo/ws/job"), None);
        assert!(!is_job_session("demo/ws/j12"));
        assert!(!is_job_session("demo/ws/job"));
        assert!(!is_job_session("demo/orch"));
        assert!(!is_job_session("demo/ws/job/shell"));
    }
}
