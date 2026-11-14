//! The Jobs screen: the project's active awareness jobs, the ones
//! `shelbi job run` and `shelbi job watch` create and `shelbi job list`
//! lists, as a three-column table both clients draw (Description, Command,
//! Started).
//!
//! A job is active while it is still in the awareness store
//! (`<project_dir>/awareness.json`); the hub poller removes it when it
//! finishes and sends its `job` event. Nothing tells a client when the store
//! changes, so a client re-reads it every [`REFRESH_INTERVAL`] while the
//! screen is open: the read is one small JSON file, off the UI thread.
//!
//! Everything here is plain data and formatting, so the TUI and the desktop
//! app show the same rows: [`JobRow::from_job`] for the columns, [`job_rows`]
//! for the order, [`started_label`] for the time, [`truncate`] for a cell
//! that would overflow.

use std::time::Duration;

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use shelbi_state::awareness::{short_sha, Job, JobKind};

/// The screen's title.
pub const TITLE: &str = "Jobs";

/// The column headings, in order.
pub const COLUMNS: [&str; 3] = ["Description", "Command", "Started"];

/// What the screen says with no active jobs.
pub const EMPTY_TEXT: &str = "No active jobs. `shelbi job run` and `shelbi job watch` add them here.";

/// What the screen says in a remote project's window: its store is on the
/// far machine and nothing reads it over the link yet.
pub const REMOTE_TEXT: &str = "Jobs aren't available for remote projects yet.";

/// How often a client re-reads the store while the screen is open. Also the
/// cadence the "Started" ages tick over at.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// One read of a project's jobs ([`crate::source::ProjectSource::jobs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobsRead {
    /// The store's jobs, in store order.
    Jobs(Vec<Job>),
    /// The source can't read the store (a remote project's window).
    Unavailable,
    /// The store couldn't be read or parsed.
    Failed(String),
}

/// What the screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum JobsModel {
    /// Before the first read lands.
    #[default]
    Loading,
    /// The active jobs, newest first. Never empty: no jobs is [`Self::Empty`].
    Rows(Vec<JobRow>),
    /// No active jobs ([`EMPTY_TEXT`]).
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
            JobsRead::Jobs(jobs) if jobs.is_empty() => JobsModel::Empty,
            JobsRead::Jobs(jobs) => JobsModel::Rows(job_rows(&jobs)),
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
}

impl JobRow {
    pub fn from_job(job: &Job) -> Self {
        JobRow {
            id: job.id.clone(),
            description: job.display_label(),
            command: command_text(job),
            started_at: job.started_at,
        }
    }

    /// The Started cell in the local time zone, as of `now`.
    pub fn started(&self, now: DateTime<Utc>) -> String {
        started_label(self.started_at, now, &Local)
    }
}

/// The Command column: a command job's command line, or what a watch
/// watches (`watch PID 4242`, `watch CI 0123456789ab`, `watch PR #2265`).
pub fn command_text(job: &Job) -> String {
    match job.kind {
        JobKind::Command => job.target.clone(),
        JobKind::Pid => format!("watch PID {}", job.target),
        JobKind::Ci => format!("watch CI {}", short_sha(&job.target)),
        JobKind::Pr => format!("watch PR #{}", job.target.trim_start_matches('#')),
    }
}

/// Every job's row, newest first. Jobs that started in the same second keep
/// the newer id first (ids count up).
pub fn job_rows(jobs: &[Job]) -> Vec<JobRow> {
    let mut rows: Vec<JobRow> = jobs.iter().map(JobRow::from_job).collect();
    rows.sort_by(|a, b| {
        b.started_at
            .cmp(&a.started_at)
            .then_with(|| id_number(&b.id).cmp(&id_number(&a.id)))
    });
    rows
}

fn id_number(id: &str) -> u64 {
    id.trim_start_matches(|c: char| !c.is_ascii_digit()).parse().unwrap_or(0)
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
        let ids: Vec<String> = job_rows(&jobs).into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["j10", "j2", "j3", "j1"]);
    }

    #[test]
    fn the_model_names_each_state() {
        assert_eq!(JobsModel::from_read(JobsRead::Jobs(vec![])), JobsModel::Empty);
        assert_eq!(JobsModel::Empty.message().as_deref(), Some(EMPTY_TEXT));
        assert_eq!(JobsModel::from_read(JobsRead::Unavailable).message().as_deref(), Some(REMOTE_TEXT));
        let rows = JobsModel::from_read(JobsRead::Jobs(vec![job("j1", JobKind::Pid, "1", None, at(1, 0))]));
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
