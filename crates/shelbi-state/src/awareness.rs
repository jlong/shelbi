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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// A command Shelbi runs detached (`shelbi job run`).
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
    /// `command` only: the pid of the detached runner, which is also the
    /// process group of the command it runs. `None` for the instant between
    /// minting the entry and spawning the runner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_pid: Option<u32>,
}

impl Job {
    /// The label the event line and listings show: the caller's `--label`, or
    /// a short description of the target.
    pub fn display_label(&self) -> String {
        if let Some(label) = self.label.as_deref().filter(|l| !l.trim().is_empty()) {
            return label.to_string();
        }
        match self.kind {
            JobKind::Command => self.target.clone(),
            JobKind::Pid => format!("pid {}", self.target),
            JobKind::Ci => format!("ci {}", short_sha(&self.target)),
            JobKind::Pr => format!("PR #{}", self.target),
        }
    }
}

fn short_sha(sha: &str) -> &str {
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
    let path = awareness_path(project)?;
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
#[derive(Debug, Clone)]
pub struct NewJob {
    pub kind: JobKind,
    pub target: String,
    pub label: Option<String>,
}

/// Register a job and return it with its minted id.
pub fn add_job(project: &str, job: NewJob) -> Result<Job> {
    update_awareness(project, |store| {
        let job = Job {
            id: store.mint_id('j'),
            kind: job.kind,
            target: job.target,
            label: job.label.filter(|l| !l.trim().is_empty()),
            started_at: Utc::now(),
            runner_pid: None,
        };
        store.jobs.push(job.clone());
        Ok(job)
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
    let first = started_at + chrono::Duration::from_std(estimate).unwrap_or(chrono::Duration::MAX);
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

/// Remove job directories older than `max_age` that no live job owns. Best
/// effort: a directory that can't be read or removed is left alone.
pub fn prune_job_dirs(project: &str, live_ids: &[&str], max_age: Duration) {
    let Ok(root) = project_dir(project).map(|d| d.join(JOBS_DIR)) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(&root) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if live_ids.contains(&name) {
            continue;
        }
        let old = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > max_age);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
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
    fn ids_are_minted_per_project_and_never_reused() {
        let _home = Home::new("ids");
        let due = Utc::now();
        let a = add_reminder("demo", due, "a", None).unwrap();
        let j = add_job(
            "demo",
            NewJob { kind: JobKind::Pid, target: "1".into(), label: None },
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
        let lines = log_lines();
        assert_eq!(
            lines,
            [
                "project=demo reminder id=r1 text=\"check the 'tap' PR heartbeat ci task=x\"",
                "project=demo job id=j2 kind=command label=\"desktop tests\" status=failed exit=3 detail=\"error: 1 test failed\" log=/tmp/jobs/j2/output.log",
                "project=demo task=t-1 overdue estimate=45m elapsed=1h10m workspace=alpha",
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
            [crate::EventKind::Reminder, crate::EventKind::JobDone, crate::EventKind::TaskOverdue]
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
}
