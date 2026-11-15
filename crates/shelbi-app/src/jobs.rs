//! The Jobs screen: the project's awareness jobs, the ones `shelbi job run`
//! and `shelbi job watch` create and `shelbi job list --all` lists, as a
//! five-column table both clients draw (Description, Command, Status,
//! Duration, Finished): the running jobs first, then, under a
//! [`FINISHED_HEADING`] row, the finished ones from the job history.
//!
//! A job is running while it is still in the awareness store
//! (`<project_dir>/awareness.json`); the hub poller moves it to the history
//! (`<project_dir>/jobs/history.json`) when it finishes and sends its `job`
//! event. Nothing tells a client when either file changes, so a client
//! re-reads them every [`REFRESH_INTERVAL`] while the screen is open: two
//! small JSON files ([`read_local`]), off the UI thread.
//!
//! Everything here is plain data and formatting, so the TUI and the desktop
//! app show the same rows: [`JobRow::from_job`] / [`JobRow::from_record`] for
//! the columns, [`job_rows`] for the order, [`JobRow::status_tone`] for how
//! loud a status is (a failure is red), [`started_label`] for a time,
//! [`truncate`] for a cell that would overflow.

use std::time::Duration;

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use shelbi_state::awareness::{job_id_number, short_sha, Job, JobKind, JobRecord, JobStatus};

use crate::theme::Tone;

/// The screen's title.
pub const TITLE: &str = "Jobs";

/// The column headings, in order.
pub const COLUMNS: [&str; 5] = ["Description", "Command", "Status", "Duration", "Finished"];

/// The section row above the finished jobs.
pub const FINISHED_HEADING: &str = "Finished";

/// What the screen says with no jobs, running or finished.
pub const EMPTY_TEXT: &str = "No jobs yet. `shelbi job run` and `shelbi job watch` add them here.";

/// What the screen says in a remote project's window: its store is on the
/// far machine and nothing reads it over the link yet.
pub const REMOTE_TEXT: &str = "Jobs aren't available for remote projects yet.";

/// How often a client re-reads the store while the screen is open. Also the
/// cadence the running Duration and the Finished ages tick over at.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// One read of a project's jobs ([`crate::source::ProjectSource::jobs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobsRead {
    /// The running jobs (store order) and the finished ones (the history).
    Jobs { running: Vec<Job>, finished: Vec<JobRecord> },
    /// The source can't read the store (a remote project's window).
    Unavailable,
    /// The store couldn't be read or parsed.
    Failed(String),
}

/// Read a local project's running jobs and its job history.
pub fn read_local(project: &str) -> JobsRead {
    let running = match shelbi_state::awareness::read_awareness(project) {
        Ok(store) => store.jobs,
        Err(e) => return JobsRead::Failed(e.to_string()),
    };
    match shelbi_state::awareness::read_job_history(project) {
        Ok(finished) => JobsRead::Jobs { running, finished },
        Err(e) => JobsRead::Failed(e.to_string()),
    }
}

/// What the screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum JobsModel {
    /// Before the first read lands.
    #[default]
    Loading,
    /// The running jobs, newest first, then the finished ones, newest
    /// first. Never empty: no jobs is [`Self::Empty`].
    Rows(Vec<JobRow>),
    /// No jobs ([`EMPTY_TEXT`]).
    Empty,
    /// Not readable from this window ([`REMOTE_TEXT`]).
    Unavailable,
    /// The read failed, with why.
    Failed(String),
}

impl JobsModel {
    /// The model for one read.
    pub fn from_read(read: JobsRead) -> Self {
        match read {
            JobsRead::Jobs { running, finished } if running.is_empty() && finished.is_empty() => {
                JobsModel::Empty
            }
            JobsRead::Jobs { running, finished } => JobsModel::Rows(job_rows(&running, &finished)),
            JobsRead::Unavailable => JobsModel::Unavailable,
            JobsRead::Failed(e) => JobsModel::Failed(e),
        }
    }

    /// The rows, empty unless the model is [`Self::Rows`].
    pub fn rows(&self) -> &[JobRow] {
        match self {
            JobsModel::Rows(rows) => rows,
            _ => &[],
        }
    }

    /// How many rows are running jobs (they come first).
    pub fn running_count(&self) -> usize {
        self.rows().iter().filter(|r| r.is_running()).count()
    }

    /// The index of the first finished row, where the [`FINISHED_HEADING`]
    /// row goes; `None` with no finished jobs.
    pub fn finished_start(&self) -> Option<usize> {
        self.rows().iter().position(|r| !r.is_running())
    }

    /// The title's count: `2 running · 5 finished`, either half alone when
    /// the other is zero, `None` with no rows.
    pub fn summary(&self) -> Option<String> {
        let running = self.running_count();
        let finished = self.rows().len() - running;
        match (running, finished) {
            (0, 0) => None,
            (r, 0) => Some(format!("{r} running")),
            (0, f) => Some(format!("{f} finished")),
            (r, f) => Some(format!("{r} running · {f} finished")),
        }
    }

    /// The line the screen shows in place of a table, if it shows one.
    pub fn message(&self) -> Option<String> {
        match self {
            JobsModel::Loading => Some("Loading…".to_string()),
            JobsModel::Rows(_) => None,
            JobsModel::Empty => Some(EMPTY_TEXT.to_string()),
            JobsModel::Unavailable => Some(REMOTE_TEXT.to_string()),
            JobsModel::Failed(e) => Some(format!("Couldn't read the project's jobs: {e}")),
        }
    }
}

/// Whether a row's job is still running, and how a finished one ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Running,
    Finished {
        status: JobStatus,
        /// A command's exit code.
        exit_code: Option<i32>,
        finished_at: DateTime<Utc>,
    },
}

/// One job's row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobRow {
    /// The store id (`j<N>`).
    pub id: String,
    /// Description: the `--label`, or a short description of the target
    /// ([`Job::display_label`]).
    pub description: String,
    /// Command: the full command line for a command job, what a watch
    /// watches otherwise ([`command_text`]). Untruncated; a cell cuts it.
    pub command: String,
    /// When the job started. [`started_label`] formats it.
    pub started_at: DateTime<Utc>,
    /// Running, or how it ended.
    pub state: JobState,
}

impl JobRow {
    /// A running job's row.
    pub fn from_job(job: &Job) -> Self {
        JobRow {
            id: job.id.clone(),
            description: job.display_label(),
            command: command_text_for(job.kind, &job.target),
            started_at: job.started_at,
            state: JobState::Running,
        }
    }

    /// A finished job's row, from the history.
    pub fn from_record(record: &JobRecord) -> Self {
        JobRow {
            id: record.id.clone(),
            description: record.display_label(),
            command: command_text_for(record.kind, &record.target),
            started_at: record.started_at,
            state: JobState::Finished {
                status: record.status,
                exit_code: record.exit_code,
                finished_at: record.finished_at,
            },
        }
    }

    pub fn is_running(&self) -> bool {
        self.state == JobState::Running
    }

    /// Whether it ended in failure: a non-zero exit, a lost runner, a
    /// failing check.
    pub fn failed(&self) -> bool {
        matches!(self.state, JobState::Finished { status: JobStatus::Failed, .. })
    }

    /// The Status cell: `Running`, `OK`, `Failed · exit 3`, `Merged`, ... A
    /// zero exit isn't spelled out; any other exit code is.
    pub fn status_label(&self) -> String {
        let JobState::Finished { status, exit_code, .. } = self.state else {
            return "Running".to_string();
        };
        let word = match status {
            JobStatus::Ok => "OK",
            JobStatus::Failed => "Failed",
            JobStatus::Exited => "Exited",
            JobStatus::Merged => "Merged",
            JobStatus::Closed => "Closed",
            JobStatus::Canceled => "Canceled",
        };
        match exit_code {
            Some(code) if code != 0 => format!("{word} · exit {code}"),
            _ => word.to_string(),
        }
    }

    /// The Status cell's color: a failure is red, success green, a running
    /// job blue, a cancel yellow, the rest plain.
    pub fn status_tone(&self) -> Tone {
        match self.state {
            JobState::Running => Tone::Blue,
            JobState::Finished { status, .. } => match status {
                JobStatus::Failed => Tone::Red,
                JobStatus::Ok | JobStatus::Merged => Tone::Green,
                JobStatus::Canceled => Tone::Yellow,
                JobStatus::Exited | JobStatus::Closed => Tone::Gray,
            },
        }
    }

    /// The Duration cell: how long it ran, or has been running as of `now`.
    pub fn duration(&self, now: DateTime<Utc>) -> String {
        let end = match self.state {
            JobState::Running => now,
            JobState::Finished { finished_at, .. } => finished_at,
        };
        duration_label((end - self.started_at).to_std().unwrap_or_default())
    }

    /// The Finished cell in the local time zone, as of `now`; empty while
    /// the job is running.
    pub fn finished(&self, now: DateTime<Utc>) -> String {
        match self.state {
            JobState::Running => String::new(),
            JobState::Finished { finished_at, .. } => started_label(finished_at, now, &Local),
        }
    }

    /// When it started, in the local time zone, as of `now`.
    pub fn started(&self, now: DateTime<Utc>) -> String {
        started_label(self.started_at, now, &Local)
    }
}

/// A duration in whole seconds, its two largest units: `45s`, `2m 13s`,
/// `1h 4m`, `3h`.
pub fn duration_label(d: Duration) -> String {
    let secs = d.as_secs();
    let (h, m, s) = (secs / 3600, secs % 3600 / 60, secs % 60);
    match (h, m, s) {
        (0, 0, s) => format!("{s}s"),
        (0, m, 0) => format!("{m}m"),
        (0, m, s) => format!("{m}m {s}s"),
        (h, 0, _) => format!("{h}h"),
        (h, m, _) => format!("{h}h {m}m"),
    }
}

/// The Command column: a command job's command line, or what a watch
/// watches (`watch PID 4242`, `watch CI 0123456789ab`, `watch PR #2265`).
pub fn command_text(job: &Job) -> String {
    command_text_for(job.kind, &job.target)
}

fn command_text_for(kind: JobKind, target: &str) -> String {
    match kind {
        JobKind::Command => target.to_string(),
        JobKind::Pid => format!("watch PID {target}"),
        JobKind::Ci => format!("watch CI {}", short_sha(target)),
        JobKind::Pr => format!("watch PR #{}", target.trim_start_matches('#')),
    }
}

/// Every job's row: the running ones newest started first, then the
/// finished ones newest finished first. Jobs in the same second keep the
/// newer id first (ids count up).
pub fn job_rows(running: &[Job], finished: &[JobRecord]) -> Vec<JobRow> {
    let mut active: Vec<JobRow> = running.iter().map(JobRow::from_job).collect();
    active.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| job_id_number(&b.id).cmp(&job_id_number(&a.id)))
    });
    let mut done: Vec<JobRow> = finished.iter().map(JobRow::from_record).collect();
    let finished_at = |r: &JobRow| match r.state {
        JobState::Finished { finished_at, .. } => finished_at,
        JobState::Running => r.started_at,
    };
    done.sort_by(|a, b| {
        finished_at(b)
            .cmp(&finished_at(a))
            .then_with(|| job_id_number(&b.id).cmp(&job_id_number(&a.id)))
    });
    active.extend(done);
    active
}

/// The Started cell: when, in `tz`, then how long ago. A time alone for a
/// job started today (`2:06 PM · 4m ago`), the date too for an earlier day
/// (`Nov 12, 2:06 PM · 2d ago`), and the year for an earlier year.
pub fn started_label<Tz: TimeZone>(started: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = started.with_timezone(tz);
    let today = now.with_timezone(tz);
    let when = if local.date_naive() == today.date_naive() {
        local.format("%-I:%M %p").to_string()
    } else if local.year() == today.year() {
        local.format("%b %-d, %-I:%M %p").to_string()
    } else {
        local.format("%b %-d %Y, %-I:%M %p").to_string()
    };
    format!("{when} · {}", age_label(started, now))
}

/// How long ago `started` was: `just now` under a minute (or in the
/// future, a skewed clock), then `4m ago`, `1h ago`, `3d ago`.
pub fn age_label(started: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let secs = (now - started).num_seconds();
    if secs < 60 {
        return "just now".to_string();
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h ago");
    }
    format!("{}d ago", hours / 24)
}

/// `text` on one line: each newline, carriage return or tab a space, so a
/// multi-line command keeps its row.
pub fn one_line(text: &str) -> String {
    text.chars().map(|c| if matches!(c, '\n' | '\r' | '\t') { ' ' } else { c }).collect()
}

/// `text` in at most `max` characters: cut with a trailing `…` when it is
/// longer. `max == 0` is empty.
pub fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(max - 1).collect();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn job(id: &str, kind: JobKind, target: &str, label: Option<&str>, started_at: DateTime<Utc>) -> Job {
        Job {
            id: id.into(),
            kind,
            target: target.into(),
            label: label.map(str::to_string),
            started_at,
            runner_pid: None,
        }
    }

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, h, m, 0).unwrap()
    }

    #[test]
    fn a_command_job_shows_its_command_line() {
        let j = job("j1", JobKind::Command, "zsh -c ./bundle.sh && open out", Some("bundle desktop app"), at(14, 6));
        let row = JobRow::from_job(&j);
        assert_eq!(row.description, "bundle desktop app");
        assert_eq!(row.command, "zsh -c ./bundle.sh && open out");
        // No label: the command is the description too.
        let bare = job("j2", JobKind::Command, "sleep 60", None, at(14, 6));
        assert_eq!(JobRow::from_job(&bare).description, "sleep 60");
        assert_eq!(JobRow::from_job(&bare).command, "sleep 60");
    }

    #[test]
    fn watches_show_what_they_watch() {
        let pid = job("j1", JobKind::Pid, "4242", None, at(14, 0));
        assert_eq!(command_text(&pid), "watch PID 4242");
        assert_eq!(JobRow::from_job(&pid).description, "pid 4242");

        let sha = "0123456789abcdef0123456789abcdef01234567";
        let ci = job("j2", JobKind::Ci, sha, None, at(14, 0));
        assert_eq!(command_text(&ci), "watch CI 0123456789ab");
        assert_eq!(JobRow::from_job(&ci).description, "ci 0123456789ab");
        let short = job("j3", JobKind::Ci, "abc123", None, at(14, 0));
        assert_eq!(command_text(&short), "watch CI abc123");

        let pr = job("j4", JobKind::Pr, "2265", None, at(14, 0));
        assert_eq!(command_text(&pr), "watch PR #2265");
        assert_eq!(JobRow::from_job(&pr).description, "PR #2265");
        // A label names the description; the command still says what's watched.
        let labelled = job("j5", JobKind::Pr, "2265", Some("project tabs PR"), at(14, 0));
        assert_eq!(JobRow::from_job(&labelled).description, "project tabs PR");
        assert_eq!(JobRow::from_job(&labelled).command, "watch PR #2265");
    }

    #[test]
    fn rows_are_newest_first() {
        let jobs = vec![
            job("j1", JobKind::Pr, "1", None, at(12, 58)),
            job("j2", JobKind::Command, "a", None, at(14, 6)),
            job("j3", JobKind::Command, "b", None, at(13, 31)),
            // Same second as j2: the newer id first.
            job("j10", JobKind::Command, "c", None, at(14, 6)),
        ];
        let ids: Vec<String> = job_rows(&jobs, &[]).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["j10", "j2", "j3", "j1"]);
    }

    fn record(id: &str, status: JobStatus, exit: Option<i32>, started: DateTime<Utc>, finished: DateTime<Utc>) -> JobRecord {
        JobRecord {
            exit_code: exit,
            ..JobRecord::finished(&job(id, JobKind::Command, "cargo test", None, started), status, finished)
        }
    }

    #[test]
    fn finished_jobs_come_after_running_ones_newest_finished_first() {
        let running = vec![job("j8", JobKind::Command, "a", None, at(13, 0)), job("j9", JobKind::Pid, "1", None, at(14, 0))];
        let finished = vec![
            record("j1", JobStatus::Ok, Some(0), at(9, 0), at(9, 30)),
            // Started first, finished last: still the newest finished.
            record("j2", JobStatus::Failed, Some(3), at(8, 0), at(12, 0)),
            record("j3", JobStatus::Canceled, None, at(10, 0), at(10, 5)),
        ];
        let model = JobsModel::from_read(JobsRead::Jobs { running, finished });
        let ids: Vec<&str> = model.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["j9", "j8", "j2", "j3", "j1"]);
        assert_eq!(model.running_count(), 2);
        assert_eq!(model.finished_start(), Some(2));
        assert_eq!(model.summary().as_deref(), Some("2 running · 3 finished"));

        let only_done = JobsModel::from_read(JobsRead::Jobs {
            running: vec![],
            finished: vec![record("j1", JobStatus::Ok, Some(0), at(9, 0), at(9, 30))],
        });
        assert_eq!(only_done.finished_start(), Some(0));
        assert_eq!(only_done.summary().as_deref(), Some("1 finished"));
        let only_running = JobsModel::from_read(JobsRead::Jobs {
            running: vec![job("j1", JobKind::Pid, "1", None, at(9, 0))],
            finished: vec![],
        });
        assert_eq!((only_running.finished_start(), only_running.summary().as_deref()), (None, Some("1 running")));
    }

    #[test]
    fn a_finished_row_shows_its_status_exit_duration_and_finish() {
        let failed = JobRow::from_record(&record("j2", JobStatus::Failed, Some(3), at(14, 0), at(14, 2)));
        assert_eq!(failed.status_label(), "Failed · exit 3");
        assert_eq!(failed.status_tone(), Tone::Red);
        assert!(failed.failed() && !failed.is_running());
        assert_eq!(failed.duration(at(19, 0)), "2m");
        assert!(!failed.finished(at(19, 0)).is_empty());

        let ok = JobRow::from_record(&record("j1", JobStatus::Ok, Some(0), at(14, 0), at(14, 0)));
        assert_eq!((ok.status_label().as_str(), ok.status_tone(), ok.failed()), ("OK", Tone::Green, false));
        let signal = JobRow::from_record(&record("j3", JobStatus::Failed, Some(143), at(14, 0), at(14, 0)));
        assert_eq!(signal.status_label(), "Failed · exit 143");
        let canceled = JobRow::from_record(&record("j4", JobStatus::Canceled, None, at(14, 0), at(14, 0)));
        assert_eq!((canceled.status_label().as_str(), canceled.status_tone()), ("Canceled", Tone::Yellow));

        let running = JobRow::from_job(&job("j5", JobKind::Command, "x", None, at(14, 0)));
        assert_eq!((running.status_label().as_str(), running.status_tone()), ("Running", Tone::Blue));
        assert_eq!(running.duration(at(14, 4) + chrono::Duration::seconds(12)), "4m 12s");
        assert_eq!(running.finished(at(15, 0)), "");
    }

    #[test]
    fn durations_keep_their_two_largest_units() {
        let d = |s: u64| duration_label(Duration::from_secs(s));
        assert_eq!(d(0), "0s");
        assert_eq!(d(45), "45s");
        assert_eq!(d(120), "2m");
        assert_eq!(d(133), "2m 13s");
        assert_eq!(d(3600), "1h");
        assert_eq!(d(3600 * 3 + 240 + 9), "3h 4m");
    }

    #[test]
    fn the_model_names_each_state() {
        let none = JobsRead::Jobs { running: vec![], finished: vec![] };
        assert_eq!(JobsModel::from_read(none), JobsModel::Empty);
        assert_eq!(JobsModel::Empty.message().as_deref(), Some(EMPTY_TEXT));
        assert_eq!(JobsModel::from_read(JobsRead::Unavailable).message().as_deref(), Some(REMOTE_TEXT));
        let rows = JobsModel::from_read(JobsRead::Jobs {
            running: vec![job("j1", JobKind::Pid, "1", None, at(1, 0))],
            finished: vec![],
        });
        assert_eq!(rows.rows().len(), 1);
        assert_eq!(rows.message(), None);
        assert!(JobsModel::from_read(JobsRead::Failed("bad json".into()))
            .message()
            .unwrap()
            .contains("bad json"));
        assert_eq!(JobsModel::default().message().as_deref(), Some("Loading…"));
    }

    #[test]
    fn started_is_a_local_time_today_and_a_date_before() {
        // UTC-5: 19:06 UTC is 2:06 PM there.
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at(19, 10);
        assert_eq!(started_label(at(19, 6), now, &tz), "2:06 PM · 4m ago");
        assert_eq!(started_label(at(17, 58), now, &tz), "12:58 PM · 1h ago");
        // 03:00 UTC on the 14th is still the 13th in UTC-5.
        assert_eq!(started_label(at(3, 0), now, &tz), "Nov 13, 10:00 PM · 16h ago");
        let earlier = Utc.with_ymd_and_hms(2026, 11, 12, 19, 6, 0).unwrap();
        assert_eq!(started_label(earlier, now, &tz), "Nov 12, 2:06 PM · 2d ago");
        let last_year = Utc.with_ymd_and_hms(2025, 12, 30, 19, 6, 0).unwrap();
        assert_eq!(started_label(last_year, now, &tz), "Dec 30 2025, 2:06 PM · 319d ago");
    }

    #[test]
    fn the_age_ticks_over() {
        let started = at(14, 0);
        assert_eq!(age_label(started, started), "just now");
        assert_eq!(age_label(started, started + chrono::Duration::seconds(59)), "just now");
        assert_eq!(age_label(started, started + chrono::Duration::seconds(60)), "1m ago");
        assert_eq!(age_label(started, started + chrono::Duration::minutes(59)), "59m ago");
        assert_eq!(age_label(started, started + chrono::Duration::minutes(60)), "1h ago");
        assert_eq!(age_label(started, started + chrono::Duration::hours(49)), "2d ago");
        // A skewed clock never reads negative.
        assert_eq!(age_label(started, started - chrono::Duration::minutes(5)), "just now");
    }

    #[test]
    fn truncation_cuts_with_an_ellipsis() {
        assert_eq!(truncate("sleep 60", 8), "sleep 60");
        assert_eq!(truncate("sleep 60", 20), "sleep 60");
        assert_eq!(truncate("sleep 600", 8), "sleep 6…");
        assert_eq!(truncate("zsh -c /Users/me/bundle.sh", 10).chars().count(), 10);
        assert_eq!(truncate("héllo wörld", 6), "héllo…");
        assert_eq!(truncate("abc", 1), "…");
        assert_eq!(truncate("abc", 0), "");
        assert_eq!(one_line("a\nb\tc\r\nd"), "a b c  d");
    }
}
