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
//!
//! A job started with `--estimate` carries it: its Duration reads elapsed
//! against the estimate (`4m 12s / 10m`), a running one draws a progress bar
//! ([`JobRow::bar`], [`crate::estimate`]) and reads `Overdue` once the
//! estimate is used up, and a finished one keeps its actual duration against
//! the estimate as text.
//!
//! The desktop screen draws richer rows from the same model: a running job's
//! last output line ([`JobRow::activity_line`], read off the job's
//! `output.log` by [`read_local`]), its kind ([`JobRow::kind_label`]) and who
//! started it ([`JobRow::started_by_label`]); a finished job's failure inline
//! ([`JobRow::failure_detail`]) and how long ago it finished
//! ([`JobRow::finished_ago`]), the history grouped by day
//! ([`JobsModel::finished_days`]).

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use shelbi_state::awareness::{job_id_number, short_sha, Job, JobKind, JobRecord, JobStarter, JobStatus};

use crate::estimate::{EstimateProgress, OVERDUE_TONE};
use crate::theme::Tone;

/// The screen's title.
pub const TITLE: &str = "Jobs";

/// The column headings, in order.
pub const COLUMNS: [&str; 5] = ["Description", "Command", "Status", "Duration", "Finished"];

/// The section row above the finished jobs.
pub const FINISHED_HEADING: &str = "Finished";

/// What the screen says with no jobs, running or finished.
pub const EMPTY_TEXT: &str = "No jobs yet. `shelbi job run` and `shelbi job watch` add them here.";

/// The desktop empty state's heading, over [`EMPTY_DETAIL`] and
/// [`EMPTY_COMMAND`].
pub const EMPTY_TITLE: &str = "No jobs yet";

/// The desktop empty state's sentence.
pub const EMPTY_DETAIL: &str =
    "Builds, test runs and PR watches started with shelbi job show up here while they run, then stay as history.";

/// The command the empty state offers to copy.
pub const EMPTY_COMMAND: &str = "shelbi job run --label \"<label>\" -- <command>";

/// The section over the running jobs (the desktop screen's).
pub const RUNNING_HEADING: &str = "Running";

/// What a running command job shows before it has printed anything.
pub const NO_OUTPUT_YET: &str = "No output yet";

/// How much of a running job's `output.log` [`last_output_line`] reads back
/// from its end to find the last line.
const TAIL_READ: u64 = 8 * 1024;

/// The longest last line a row keeps (a client cuts it to its width).
const TAIL_MAX_CHARS: usize = 400;

/// What the screen says in a remote project's window: its store is on the
/// far machine and nothing reads it over the link yet.
pub const REMOTE_TEXT: &str = "Jobs aren't available for remote projects yet.";

/// How often a client re-reads the store while the screen is open. Also the
/// cadence the running Duration and the Finished ages tick over at.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(3);

/// One read of a project's jobs ([`crate::source::ProjectSource::jobs`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobsRead {
    /// The running jobs (store order) and the finished ones (the history),
    /// with each running command's last output line by job id (`tails`: a
    /// job missing from it has printed nothing yet).
    Jobs { running: Vec<Job>, finished: Vec<JobRecord>, tails: BTreeMap<String, String> },
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
    let tails = running
        .iter()
        .filter(|j| j.kind == JobKind::Command)
        .filter_map(|j| {
            let path = shelbi_state::awareness::job_output_path(project, &j.id).ok()?;
            Some((j.id.clone(), last_output_line(&path)?))
        })
        .collect();
    match shelbi_state::awareness::read_job_history(project) {
        Ok(finished) => JobsRead::Jobs { running, finished, tails },
        Err(e) => JobsRead::Failed(e.to_string()),
    }
}

/// The last line of the log at `path` that has something on it, as a row
/// shows it: escape sequences dropped, a line a carriage return redrew (a
/// progress bar) as its last drawing, trimmed and cut to a few hundred
/// characters. `None` when the log is missing or blank. Reads only the
/// log's last few KiB.
pub fn last_output_line(path: &std::path::Path) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(TAIL_READ);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    // A read that starts mid-file starts mid-line: skip that partial line,
    // unless it is the only one.
    let lines: Vec<&str> = text.split('\n').collect();
    let skip = usize::from(start > 0 && lines.len() > 1);
    lines[skip..].iter().rev().find_map(|line| {
        let drawn = line.rsplit('\r').map(strip_escapes).find(|seg| !seg.trim().is_empty())?;
        Some(truncate(drawn.trim(), TAIL_MAX_CHARS))
    })
}

/// `text` without its terminal escape sequences (CSI `ESC [ … final`, OSC
/// `ESC ] … BEL` or `ESC \\`, and two-byte escapes) or other control
/// characters; a tab is a space.
pub fn strip_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    // Parameters and intermediates, then one final byte.
                    for c in chars.by_ref() {
                        if ('@'..='~').contains(&c) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' {
                            break;
                        }
                        if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                // A character set designation: one more byte.
                Some('(' | ')' | '*' | '+') => {
                    chars.next();
                }
                _ => {}
            },
            '\t' => out.push(' '),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
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
            JobsRead::Jobs { running, finished, .. } if running.is_empty() && finished.is_empty() => {
                JobsModel::Empty
            }
            JobsRead::Jobs { running, finished, tails } => JobsModel::Rows(job_rows(&running, &finished, &tails)),
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

    /// The running jobs' rows, newest first.
    pub fn running(&self) -> &[JobRow] {
        &self.rows()[..self.running_count()]
    }

    /// The finished jobs' rows, newest finished first.
    pub fn finished(&self) -> &[JobRow] {
        &self.rows()[self.running_count()..]
    }

    /// The finished jobs by the day they finished in `tz`, as of `now`: for
    /// each day, the index into [`finished`](Self::finished) of its first
    /// row and its heading ([`day_label`]: `Today`, `Yesterday`, `Mon, Nov
    /// 16`, ...). Empty with no finished jobs.
    pub fn finished_days<Tz: TimeZone>(&self, now: DateTime<Utc>, tz: &Tz) -> Vec<(usize, String)>
    where
        Tz::Offset: std::fmt::Display,
    {
        let mut days: Vec<(usize, String)> = Vec::new();
        let mut last = None;
        for (i, row) in self.finished().iter().enumerate() {
            let day = row.end(now).with_timezone(tz).date_naive();
            if last != Some(day) {
                last = Some(day);
                days.push((i, day_label(row.end(now), now, tz)));
            }
        }
        days
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
    /// What it is: only a command job has a terminal and output.
    pub kind: JobKind,
    /// What it runs or watches: the command line, pid, commit sha or PR.
    pub target: String,
    /// How long it was expected to take (`--estimate`), if it had a say.
    pub estimate: Option<Duration>,
    /// The worker that started it; `None` is the orchestrator (or a shell
    /// outside any workspace).
    pub started_by: Option<JobStarter>,
    /// A running command's last output line ([`last_output_line`]), or a
    /// finished job's `detail` (its last line, why it never started, a
    /// watch's failing check).
    pub last_line: Option<String>,
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
            kind: job.kind,
            target: job.target.clone(),
            estimate: job.estimate,
            started_by: job.started_by.clone(),
            last_line: None,
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
            kind: record.kind,
            target: record.target.clone(),
            estimate: record.estimate,
            started_by: record.started_by.clone(),
            last_line: record.detail.as_deref().map(one_line).filter(|d| !d.trim().is_empty()),
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

    /// What kind of job it is, with what it watches: `Command`, `PR #2265`,
    /// `CI d482e3f` (a short sha), `PID 4242`.
    pub fn kind_label(&self) -> String {
        match self.kind {
            JobKind::Command => "Command".to_string(),
            JobKind::Pr => format!("PR #{}", self.target.trim_start_matches('#')),
            JobKind::Ci => format!("CI {}", sha7(&self.target)),
            JobKind::Pid => format!("PID {}", self.target),
        }
    }

    /// Who started it: `alpha`, the worker's workspace, or `orchestrator`.
    pub fn started_by_label(&self) -> String {
        match &self.started_by {
            Some(s) => s.workspace.clone(),
            None => "orchestrator".to_string(),
        }
    }

    /// What a running job is doing, in a line: a command's last output line
    /// ([`NO_OUTPUT_YET`] before it prints any), or what a watch waits for.
    pub fn activity_line(&self) -> String {
        match self.kind {
            JobKind::Command => self.last_line.clone().unwrap_or_else(|| NO_OUTPUT_YET.to_string()),
            JobKind::Pr => format!("Waiting for PR #{} to merge or close", self.target.trim_start_matches('#')),
            JobKind::Ci => format!("Waiting for CI on {}", sha7(&self.target)),
            JobKind::Pid => format!("Waiting for process {} to exit", self.target),
        }
    }

    /// Whether [`activity_line`](Self::activity_line) is program output
    /// (drawn in monospace) rather than Shelbi's own words.
    pub fn activity_is_output(&self) -> bool {
        self.kind == JobKind::Command && self.last_line.is_some()
    }

    /// Why a failed job failed, for its history row: its exit code and its
    /// last line (`exit 101 · test result: FAILED`), either alone. `None`
    /// unless it failed.
    pub fn failure_detail(&self) -> Option<String> {
        let JobState::Finished { status: JobStatus::Failed, exit_code, .. } = self.state else {
            return None;
        };
        let exit = exit_code.filter(|c| *c != 0).map(|c| format!("exit {c}"));
        match (exit, self.last_line.as_deref()) {
            (Some(e), Some(l)) => Some(format!("{e} · {l}")),
            (Some(e), None) => Some(e),
            (None, Some(l)) => Some(l.to_string()),
            (None, None) => Some("Failed".to_string()),
        }
    }

    /// How long ago it finished (`12m ago`); empty while it runs.
    pub fn finished_ago(&self, now: DateTime<Utc>) -> String {
        match self.state {
            JobState::Running => String::new(),
            JobState::Finished { finished_at, .. } => age_label(finished_at, now),
        }
    }

    /// The estimate's share used, as a whole percent (`35%`), capped at
    /// 100; `None` without a bar.
    pub fn percent(&self, now: DateTime<Utc>) -> Option<u32> {
        self.bar(now).map(|p| (p.fraction() * 100.0).round() as u32)
    }

    /// The words beside a running job's bar: the share used (`35%`), or by
    /// how much it is past its estimate (`9m over`). `None` without a bar.
    pub fn bar_label(&self, now: DateTime<Utc>) -> Option<String> {
        let bar = self.bar(now)?;
        if bar.overdue() {
            let over = bar.elapsed.saturating_sub(bar.estimate);
            return Some(format!("{} over", duration_label(over)));
        }
        self.percent(now).map(|p| format!("{p}%"))
    }

    /// Who started it, in full: `alpha (developer, task-7)`, or
    /// `orchestrator`.
    pub fn started_by_detail(&self) -> String {
        match &self.started_by {
            Some(s) => s.describe(),
            None => "orchestrator".to_string(),
        }
    }

    /// When it stopped running, or `now` while it runs.
    fn end(&self, now: DateTime<Utc>) -> DateTime<Utc> {
        match self.state {
            JobState::Running => now,
            JobState::Finished { finished_at, .. } => finished_at,
        }
    }

    /// The Duration cell: how long it ran, or has been running as of `now`;
    /// against its estimate when it has one (`4m 12s / 10m`).
    pub fn duration(&self, now: DateTime<Utc>) -> String {
        match self.progress(now) {
            Some(p) => format!("{} / {}", duration_label(p.elapsed), duration_label(p.estimate)),
            None => duration_label((self.end(now) - self.started_at).to_std().unwrap_or_default()),
        }
    }

    /// How far into its estimate it ran (a finished job) or is (a running
    /// one) at `now`; `None` without an estimate.
    pub fn progress(&self, now: DateTime<Utc>) -> Option<EstimateProgress> {
        let estimate = self.estimate?;
        Some(EstimateProgress::between(self.started_at, self.end(now), estimate))
    }

    /// The progress bar a running job with an estimate draws (the issue
    /// cards' bar, from the same [`EstimateProgress`]); `None` for a finished
    /// job (its Duration says how it went) or one with no estimate.
    pub fn bar(&self, now: DateTime<Utc>) -> Option<EstimateProgress> {
        self.progress(now).filter(|_| self.is_running())
    }

    /// Whether it is still running past its estimate.
    pub fn overdue(&self, now: DateTime<Utc>) -> bool {
        self.bar(now).is_some_and(|p| p.overdue())
    }

    /// The Duration cell's color: [`OVERDUE_TONE`] once it ran (or is
    /// running) past its estimate; `None` is the cell's muted default.
    pub fn duration_tone(&self, now: DateTime<Utc>) -> Option<Tone> {
        self.progress(now).filter(EstimateProgress::overdue).map(|_| OVERDUE_TONE)
    }

    /// The Status cell at `now`: [`status_label`](Self::status_label), but
    /// `Overdue` for a job still running past its estimate.
    pub fn status_label_at(&self, now: DateTime<Utc>) -> String {
        if self.overdue(now) {
            return "Overdue".to_string();
        }
        self.status_label()
    }

    /// The Status cell's color at `now`: [`OVERDUE_TONE`] for a job running
    /// past its estimate, else [`status_tone`](Self::status_tone).
    pub fn status_tone_at(&self, now: DateTime<Utc>) -> Tone {
        if self.overdue(now) {
            return OVERDUE_TONE;
        }
        self.status_tone()
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

    /// The side sheet's status line: a glyph, the status, and how long it
    /// ran or has been running (`● Running · 42s`, `✕ Failed · exit 3 ·
    /// 1m 30s`, `● Overdue · 45m / 30m`). Colored with
    /// [`status_tone_at`](Self::status_tone_at).
    pub fn sheet_status(&self, now: DateTime<Utc>) -> String {
        format!("{} · {}", self.status_glyph_label(now), self.duration(now))
    }

    /// The status with its glyph, and no duration (`● Running`, `✕ Failed ·
    /// exit 3`): the desktop sheet's Status property, which shows the
    /// duration on a row of its own.
    pub fn status_glyph_label(&self, now: DateTime<Utc>) -> String {
        let glyph = match self.state {
            JobState::Running => "●",
            JobState::Finished { status, .. } => match status {
                JobStatus::Ok | JobStatus::Merged => "✓",
                JobStatus::Failed => "✕",
                JobStatus::Canceled => "■",
                JobStatus::Exited | JobStatus::Closed => "○",
            },
        };
        format!("{glyph} {}", self.status_label_at(now))
    }

    /// Whether the sheet offers [`CANCEL_LABEL`]: the job is still running.
    pub fn can_cancel(&self) -> bool {
        self.is_running()
    }
}

// ---------------------------------------------------------------------------
// The side sheet

/// The side sheet's Cancel button.
pub const CANCEL_LABEL: &str = "Cancel job";

/// What the sheet says for a `job watch` job, which has no output.
pub const WATCH_TEXT: &str = "A watch has no output: it follows something that runs elsewhere.";

/// The most of a finished job's `output.log` the sheet shows: its end.
pub const SAVED_OUTPUT_MAX: u64 = 256 * 1024;

/// What a job's side sheet shows under its title and status: the live
/// terminal of a running command, the saved output of a finished one, or a
/// line saying why there is neither. Both clients open a sheet from a row
/// with [`sheet_output`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SheetOutput {
    /// Attach (read-only) to the session of this name: the job's terminal.
    Live { session: String },
    /// The saved output, ready to feed a terminal emulator ([`saved_output`]).
    Saved(Vec<u8>),
    /// Nothing to show, and why.
    Message(String),
}

/// What the sheet for `row` (a job of the local project `project`) shows. A
/// running command whose session is up is [`SheetOutput::Live`]; one whose
/// session is gone (a job started by an older Shelbi, or one just ending)
/// shows its output so far. Reads the sessions directory and the log, so a
/// client calls it off its UI thread.
pub fn sheet_output(project: &str, row: &JobRow) -> SheetOutput {
    if row.kind != JobKind::Command {
        return SheetOutput::Message(WATCH_TEXT.to_string());
    }
    if row.is_running() {
        let session = shelbi_state::awareness::job_session_name(project, &row.id);
        let live = shelbi_state::sessions_dir()
            .ok()
            .and_then(|root| shelbi_client::list(&root).ok())
            .is_some_and(|all| shelbi_client::choose_session(&all, &session).is_some());
        if live {
            return SheetOutput::Live { session };
        }
    }
    match shelbi_state::awareness::job_output_path(project, &row.id).map(|p| saved_output(&p)) {
        Ok(Ok(bytes)) if !bytes.is_empty() => SheetOutput::Saved(bytes),
        Ok(Ok(_)) => SheetOutput::Message("No output.".to_string()),
        Ok(Err(e)) if e.kind() == std::io::ErrorKind::NotFound => {
            SheetOutput::Message("Its output is no longer kept.".to_string())
        }
        Ok(Err(e)) => SheetOutput::Message(format!("Couldn't read its output: {e}")),
        Err(e) => SheetOutput::Message(format!("Couldn't read its output: {e}")),
    }
}

/// How long [`attach_live`] keeps trying a job session whose socket isn't
/// answering yet (one that has only just started).
const ATTACH_TRIES: u32 = 30;
const ATTACH_RETRY: Duration = Duration::from_millis(100);

/// Attach to the live job session `session` ([`SheetOutput::Live`]) with a
/// full replay, by its exact name. The sheet only watches: a client never
/// sends it input, so nothing typed reaches the job. Blocking: off the UI
/// thread.
pub fn attach_live(session: &str) -> Result<crate::session::Connected, String> {
    let root = shelbi_state::sessions_dir().map_err(|e| e.to_string())?;
    let mut last = String::new();
    for _ in 0..ATTACH_TRIES {
        let sessions = shelbi_client::list(&root).map_err(|e| e.to_string())?;
        let Some(found) = shelbi_client::choose_session(&sessions, session) else {
            return Err(format!("the job's terminal `{session}` has closed"));
        };
        match shelbi_client::Connection::open(&found.sock, None, shelbi_proto::capability::ALL) {
            Ok((conn, events)) => {
                let size = conn
                    .info()
                    .map(|i| crate::session::GridSize::new(i.cols.max(1), i.rows.max(1)))
                    .unwrap_or(crate::session::GridSize::new(80, 24));
                conn.attach(None).map_err(|e| e.to_string())?;
                return Ok(crate::session::Connected { conn, events, size });
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(ATTACH_RETRY);
    }
    Err(format!("couldn't attach to the job's terminal: {last}"))
}

/// A job's sheet, loaded ([`open_sheet`]).
pub enum SheetLoad {
    /// Attached to the running job's terminal.
    Live(crate::session::Connected),
    /// The saved output ([`saved_output`]).
    Saved(Vec<u8>),
    /// Nothing to show, and why.
    Message(String),
}

/// Load the sheet for `row`: attach to its live terminal, or read its saved
/// output, as [`sheet_output`] says. A terminal that closes before the
/// attach lands (the job just ended) falls back to the output. Blocking:
/// off the UI thread.
pub fn open_sheet(project: &str, row: &JobRow) -> SheetLoad {
    let saved = |why: String| match shelbi_state::awareness::job_output_path(project, &row.id)
        .map_err(|e| e.to_string())
        .and_then(|p| saved_output(&p).map_err(|e| e.to_string()))
    {
        Ok(bytes) if !bytes.is_empty() => SheetLoad::Saved(bytes),
        _ => SheetLoad::Message(why),
    };
    match sheet_output(project, row) {
        SheetOutput::Live { session } => match attach_live(&session) {
            Ok(connected) => SheetLoad::Live(connected),
            Err(e) => saved(e),
        },
        SheetOutput::Saved(bytes) => SheetLoad::Saved(bytes),
        SheetOutput::Message(m) => SheetLoad::Message(m),
    }
}

/// The end of a plain-text log at `path` as terminal input: at most
/// [`SAVED_OUTPUT_MAX`] bytes, starting on a line, each `\n` a `\r\n` so an
/// emulator starts every line at the left edge.
pub fn saved_output(path: &std::path::Path) -> std::io::Result<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(SAVED_OUTPUT_MAX);
    file.seek(SeekFrom::Start(start))?;
    let mut text = Vec::new();
    file.read_to_end(&mut text)?;
    if start > 0 {
        let first = text.iter().position(|b| *b == b'\n').map_or(text.len(), |i| i + 1);
        text.drain(..first);
    }
    let mut out = Vec::with_capacity(text.len() + text.len() / 16);
    for b in text {
        if b == b'\n' {
            out.push(b'\r');
        }
        out.push(b);
    }
    Ok(out)
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
pub fn job_rows(running: &[Job], finished: &[JobRecord], tails: &BTreeMap<String, String>) -> Vec<JobRow> {
    let mut active: Vec<JobRow> = running
        .iter()
        .map(|j| JobRow { last_line: tails.get(&j.id).cloned(), ..JobRow::from_job(j) })
        .collect();
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
    format!("{} · {}", clock_label(started, now, tz), age_label(started, now))
}

/// `t` in `tz` as the screens show a time: the time alone on `now`'s day
/// (`2:06 PM`), the date too on another day of the year (`Nov 12, 2:06
/// PM`), and the year too in another year. Past or future alike.
pub fn clock_label<Tz: TimeZone>(t: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let local = t.with_timezone(tz);
    let today = now.with_timezone(tz);
    if local.date_naive() == today.date_naive() {
        local.format("%-I:%M %p").to_string()
    } else if local.year() == today.year() {
        local.format("%b %-d, %-I:%M %p").to_string()
    } else {
        local.format("%b %-d %Y, %-I:%M %p").to_string()
    }
}

/// A commit's short form, as git abbreviates it: its first 7 characters.
fn sha7(sha: &str) -> &str {
    sha.get(..7).unwrap_or(sha)
}

/// The heading over the jobs that finished on `t`'s day in `tz`, as of
/// `now`: `Today`, `Yesterday`, then the weekday and date within the week
/// (`Mon, Nov 16`), the date within the year (`Nov 12`), and the year too
/// before that (`Dec 30, 2025`).
pub fn day_label<Tz: TimeZone>(t: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let day = t.with_timezone(tz).date_naive();
    let today = now.with_timezone(tz).date_naive();
    let back = (today - day).num_days();
    match back {
        i64::MIN..=0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        2..=6 => day.format("%a, %b %-d").to_string(),
        _ if day.year() == today.year() => day.format("%b %-d").to_string(),
        _ => day.format("%b %-d, %Y").to_string(),
    }
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
            estimate: None,
            next_alert_at: None,
            started_by: None,
        }
    }

    fn read(running: Vec<Job>, finished: Vec<JobRecord>) -> JobsRead {
        JobsRead::Jobs { running, finished, tails: BTreeMap::new() }
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
        let ids: Vec<String> = job_rows(&jobs, &[], &BTreeMap::new()).into_iter().map(|r| r.id).collect();
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
        let model = JobsModel::from_read(read(running, finished));
        let ids: Vec<&str> = model.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["j9", "j8", "j2", "j3", "j1"]);
        assert_eq!(model.running_count(), 2);
        assert_eq!(model.finished_start(), Some(2));
        assert_eq!(model.summary().as_deref(), Some("2 running · 3 finished"));

        let only_done = JobsModel::from_read(read(vec![], vec![record("j1", JobStatus::Ok, Some(0), at(9, 0), at(9, 30))]));
        assert_eq!(only_done.finished_start(), Some(0));
        assert_eq!(only_done.summary().as_deref(), Some("1 finished"));
        let only_running = JobsModel::from_read(read(vec![job("j1", JobKind::Pid, "1", None, at(9, 0))], vec![]));
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
        let none = read(vec![], vec![]);
        assert_eq!(JobsModel::from_read(none), JobsModel::Empty);
        assert_eq!(JobsModel::Empty.message().as_deref(), Some(EMPTY_TEXT));
        assert_eq!(JobsModel::from_read(JobsRead::Unavailable).message().as_deref(), Some(REMOTE_TEXT));
        let rows = JobsModel::from_read(read(vec![job("j1", JobKind::Pid, "1", None, at(1, 0))], vec![]));
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

    #[test]
    fn the_sheet_status_says_how_the_job_is_doing_and_for_how_long() {
        let now = at(14, 10);
        let running = JobRow::from_job(&job("j4", JobKind::Command, "make", None, at(14, 9)));
        assert_eq!(running.sheet_status(now), "● Running · 1m");
        assert!(running.can_cancel());
        let failed = JobRow::from_record(&record("j5", JobStatus::Failed, Some(3), at(14, 0), at(14, 1)));
        assert_eq!(failed.sheet_status(now), "✕ Failed · exit 3 · 1m");
        assert_eq!(failed.status_glyph_label(now), "✕ Failed · exit 3", "the status alone");
        assert!(!failed.can_cancel());
        let ok = JobRow::from_record(&record("j6", JobStatus::Ok, Some(0), at(14, 0), at(14, 2)));
        assert_eq!(ok.sheet_status(now), "✓ OK · 2m");
    }

    #[test]
    fn an_estimate_shows_elapsed_against_it_and_a_bar_while_running() {
        let mut j = job("j1", JobKind::Command, "cargo test", None, at(14, 0));
        j.estimate = Some(Duration::from_secs(600));
        let row = JobRow::from_job(&j);
        let now = at(14, 4) + chrono::Duration::seconds(12);
        assert_eq!(row.duration(now), "4m 12s / 10m");
        let bar = row.bar(now).unwrap();
        assert!((bar.fraction() - 0.42).abs() < 1e-6);
        assert!(!row.overdue(now));
        assert_eq!((row.status_label_at(now).as_str(), row.status_tone_at(now)), ("Running", Tone::Blue));
        assert_eq!(row.duration_tone(now), None);

        // Past it: overdue, the bar full in the overdue tone.
        let late = at(14, 15);
        assert!(row.overdue(late));
        assert_eq!(row.bar(late).unwrap().fraction(), 1.0);
        assert!(row.bar(late).unwrap().overdue());
        assert_eq!((row.status_label_at(late).as_str(), row.status_tone_at(late)), ("Overdue", OVERDUE_TONE));
        assert_eq!(row.duration_tone(late), Some(OVERDUE_TONE));
        assert_eq!(row.sheet_status(late), "● Overdue · 15m / 10m");

        // No estimate: no bar, never overdue.
        let plain = JobRow::from_job(&job("j2", JobKind::Command, "x", None, at(14, 0)));
        assert_eq!((plain.bar(late), plain.overdue(late), plain.duration(late).as_str()), (None, false, "15m"));
    }

    #[test]
    fn a_finished_job_keeps_its_duration_against_the_estimate_as_text() {
        let mut r = record("j3", JobStatus::Ok, Some(0), at(14, 0), at(14, 12));
        r.estimate = Some(Duration::from_secs(600));
        let row = JobRow::from_record(&r);
        let now = at(19, 0);
        assert_eq!(row.duration(now), "12m / 10m");
        assert_eq!(row.bar(now), None, "a finished job draws no bar");
        assert!(!row.overdue(now));
        assert_eq!(row.status_label_at(now), "OK");
        assert_eq!(row.duration_tone(now), Some(OVERDUE_TONE), "it ran over");
        r.finished_at = at(14, 8);
        assert_eq!(JobRow::from_record(&r).duration_tone(now), None);
    }

    #[test]
    fn a_row_names_its_kind_and_who_started_it() {
        let sha = "d482e3fe5a1b2c3d4e5f60718293a4b5c6d7e8f9";
        let kinds = [
            (JobKind::Command, "cargo test", "Command"),
            (JobKind::Pr, "2265", "PR #2265"),
            (JobKind::Pr, "#2265", "PR #2265"),
            (JobKind::Ci, sha, "CI d482e3f"),
            (JobKind::Ci, "abc", "CI abc"),
            (JobKind::Pid, "4242", "PID 4242"),
        ];
        for (kind, target, want) in kinds {
            assert_eq!(JobRow::from_job(&job("j1", kind, target, None, at(14, 0))).kind_label(), want);
        }
        let mut j = job("j1", JobKind::Command, "make", None, at(14, 0));
        assert_eq!(JobRow::from_job(&j).started_by_label(), "orchestrator");
        j.started_by = Some(JobStarter { workspace: "alpha".into(), agent: Some("developer".into()), task: None });
        assert_eq!(JobRow::from_job(&j).started_by_label(), "alpha");
        assert_eq!(JobRow::from_job(&j).started_by_detail(), "alpha (developer)");
        let r = JobRecord::finished(&j, JobStatus::Ok, at(14, 2));
        assert_eq!(JobRow::from_record(&r).started_by_label(), "alpha", "the history keeps it");
    }

    #[test]
    fn a_running_job_says_what_it_is_doing() {
        let cmd = job("j1", JobKind::Command, "make", None, at(14, 0));
        let mut tails = BTreeMap::new();
        let rows = job_rows(std::slice::from_ref(&cmd), &[], &tails);
        assert_eq!((rows[0].activity_line().as_str(), rows[0].activity_is_output()), (NO_OUTPUT_YET, false));
        tails.insert("j1".to_string(), "Compiling shelbi v0.42.0".to_string());
        let rows = job_rows(&[cmd], &[], &tails);
        assert_eq!((rows[0].activity_line().as_str(), rows[0].activity_is_output()), ("Compiling shelbi v0.42.0", true));
        let pr = JobRow::from_job(&job("j2", JobKind::Pr, "2265", None, at(14, 0)));
        assert_eq!((pr.activity_line().as_str(), pr.activity_is_output()), ("Waiting for PR #2265 to merge or close", false));
        let ci = JobRow::from_job(&job("j3", JobKind::Ci, "d482e3fe5a1b2c3d4e5f", None, at(14, 0)));
        assert_eq!(ci.activity_line(), "Waiting for CI on d482e3f");
        let pid = JobRow::from_job(&job("j4", JobKind::Pid, "4242", None, at(14, 0)));
        assert_eq!(pid.activity_line(), "Waiting for process 4242 to exit");
    }

    #[test]
    fn a_failed_job_says_why_inline() {
        let mut r = record("j1", JobStatus::Failed, Some(101), at(14, 0), at(14, 7));
        r.detail = Some("test result: FAILED. 3 passed; 1 failed".into());
        let row = JobRow::from_record(&r);
        assert_eq!(row.failure_detail().as_deref(), Some("exit 101 · test result: FAILED. 3 passed; 1 failed"));
        assert_eq!(row.finished_ago(at(14, 19)), "12m ago");
        r.detail = None;
        assert_eq!(JobRow::from_record(&r).failure_detail().as_deref(), Some("exit 101"));
        r.exit_code = None;
        r.detail = Some("the job runner exited without recording a result".into());
        assert_eq!(
            JobRow::from_record(&r).failure_detail().as_deref(),
            Some("the job runner exited without recording a result")
        );
        r.detail = Some("  ".into());
        assert_eq!(JobRow::from_record(&r).failure_detail().as_deref(), Some("Failed"));
        let ok = JobRow::from_record(&record("j2", JobStatus::Ok, Some(0), at(14, 0), at(14, 1)));
        assert_eq!((ok.failure_detail(), ok.finished_ago(at(14, 1))), (None, "just now".to_string()));
        let running = JobRow::from_job(&job("j3", JobKind::Command, "x", None, at(14, 0)));
        assert_eq!((running.failure_detail(), running.finished_ago(at(15, 0))), (None, String::new()));
    }

    #[test]
    fn the_percent_used_tracks_the_bar() {
        let mut j = job("j1", JobKind::Command, "cargo test", None, at(14, 0));
        j.set_estimate(Duration::from_secs(1200));
        let row = JobRow::from_job(&j);
        assert_eq!(row.percent(at(14, 7)), Some(35));
        assert_eq!(row.percent(at(15, 0)), Some(100), "capped once overdue");
        assert_eq!(row.bar_label(at(14, 7)).as_deref(), Some("35%"));
        assert_eq!(row.bar_label(at(14, 29)).as_deref(), Some("9m over"));
        let plain = JobRow::from_job(&job("j2", JobKind::Command, "x", None, at(14, 0)));
        assert_eq!((plain.percent(at(14, 7)), plain.bar_label(at(14, 7))), (None, None));
    }

    #[test]
    fn the_history_groups_by_the_day_each_job_finished() {
        // UTC-5: the 14th 19:10 UTC is 2:10 PM there.
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at(19, 10);
        let day = |d: u32, h: u32| Utc.with_ymd_and_hms(2026, 11, d, h, 0, 0).unwrap();
        let finished = vec![
            record("j1", JobStatus::Ok, Some(0), day(14, 17), day(14, 18)),
            record("j2", JobStatus::Ok, Some(0), day(14, 14), day(14, 15)),
            // 03:00 UTC on the 14th is the 13th in UTC-5: yesterday.
            record("j3", JobStatus::Failed, Some(1), day(14, 2), day(14, 3)),
            record("j4", JobStatus::Ok, Some(0), day(11, 12), day(11, 13)),
            record("j5", JobStatus::Ok, Some(0), day(2, 12), day(2, 13)),
        ];
        let model = JobsModel::from_read(read(vec![job("j9", JobKind::Pid, "1", None, at(19, 0))], finished));
        assert_eq!(model.running().len(), 1);
        assert_eq!(model.finished().len(), 5);
        assert_eq!(
            model.finished_days(now, &tz),
            [
                (0, "Today".to_string()),
                (2, "Yesterday".to_string()),
                (3, "Wed, Nov 11".to_string()),
                (4, "Nov 2".to_string()),
            ]
        );
        let last_year = Utc.with_ymd_and_hms(2025, 12, 30, 19, 0, 0).unwrap();
        assert_eq!(day_label(last_year, now, &tz), "Dec 30, 2025");
        assert_eq!(day_label(now + chrono::Duration::hours(1), now, &tz), "Today", "a skewed clock");
        assert!(JobsModel::Empty.finished_days(now, &tz).is_empty());
    }

    #[test]
    fn the_last_output_line_is_the_last_one_drawn() {
        let path = std::env::temp_dir().join(format!("shelbi-jobs-tail-{}.log", std::process::id()));
        let tail = |text: &str| {
            std::fs::write(&path, text).unwrap();
            last_output_line(&path)
        };
        assert_eq!(tail("one\ntwo\n").as_deref(), Some("two"));
        assert_eq!(tail("one\ntwo\n\n   \n").as_deref(), Some("two"), "blank lines skipped");
        assert_eq!(tail("").as_deref(), None);
        assert_eq!(tail("\n\n").as_deref(), None);
        // A progress bar redrawn with carriage returns: its last drawing.
        assert_eq!(tail("Downloading  10%\rDownloading  55%\r").as_deref(), Some("Downloading  55%"));
        // Colors and other escapes go.
        assert_eq!(
            tail("\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m shelbi\tv0.42\u{1b}]0;title\u{7}\n").as_deref(),
            Some("Compiling shelbi v0.42")
        );
        // A long log reads only its end, skipping the cut line.
        let big = "x".repeat(TAIL_READ as usize * 2) + "\nlast line\n";
        assert_eq!(tail(&big).as_deref(), Some("last line"));
        let one_huge = "y".repeat(TAIL_READ as usize * 2);
        assert_eq!(tail(&one_huge).map(|l| l.chars().count()), Some(TAIL_MAX_CHARS), "cut to the cap");
        let _ = std::fs::remove_file(&path);
        assert_eq!(last_output_line(&path), None, "a missing log");
        assert_eq!(strip_escapes("a\u{1b}[38;5;196mb\u{1b}(Bc\u{7}"), "abc");
    }

    #[test]
    fn a_watch_has_no_output_to_show() {
        let row = JobRow::from_job(&job("j7", JobKind::Pid, "4242", None, at(14, 0)));
        assert_eq!(sheet_output("demo", &row), SheetOutput::Message(WATCH_TEXT.to_string()));
    }

    #[test]
    fn saved_output_is_the_logs_end_as_terminal_lines() {
        let path = std::env::temp_dir().join(format!("shelbi-jobs-saved-{}.log", std::process::id()));
        std::fs::write(&path, "one\ntwo\n").unwrap();
        assert_eq!(saved_output(&path).unwrap(), b"one\r\ntwo\r\n");
        // A long log starts on a whole line within the window.
        let line = "x".repeat(99) + "\n";
        let big = line.repeat((SAVED_OUTPUT_MAX as usize / line.len()) + 10);
        std::fs::write(&path, format!("{big}last\n")).unwrap();
        let out = saved_output(&path).unwrap();
        assert!(out.len() as u64 <= SAVED_OUTPUT_MAX + SAVED_OUTPUT_MAX / 50);
        assert!(out.starts_with(b"xxx"));
        assert!(out.ends_with(b"last\r\n"));
        let _ = std::fs::remove_file(&path);
    }

}
