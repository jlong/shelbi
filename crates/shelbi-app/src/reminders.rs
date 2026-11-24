//! The Reminders screen: the project's `shelbi remind` reminders, the
//! pending ones soonest first, then, under a [`FIRED_HEADING`] row, the ones
//! that recently fired, newest first. Both clients draw it from this model,
//! the way they draw the Jobs screen from [`crate::jobs`] (and with that
//! screen's header, row and side-sheet pieces).
//!
//! A reminder is pending while it is in the awareness store
//! (`<project_dir>/awareness.json`). When it comes due the hub poller writes
//! its `reminder` event and records the firing in the reminder history
//! (`<project_dir>/reminders/history.json`): a one-shot reminder leaves the
//! store, a recurring one moves to its next slot and so is listed under both
//! headings. Nothing announces either file changing, so a client re-reads
//! them every [`REFRESH_INTERVAL`] while the screen is open ([`read_local`]),
//! off the UI thread, as the Jobs screen does.
//!
//! ```text
//! Reminders                                   3 pending · 2 fired
//! Pending
//!  r12  in 14m          check PR #2311 CI
//!  r9   in 2h           ask about devbox relay       every 1d
//! Fired
//!  r8   fired 20m ago   check deploy
//! ```
//!
//! The desktop draws the same rows as an agenda ([`RemindersModel::next_up`],
//! [`RemindersModel::agenda_days`], [`RemindersModel::fired_days`]): the
//! soonest pending reminder as a [`NEXT_UP_HEADING`] card counting down to
//! it, the rest under their local day (`Today`, `Tomorrow`, `Thursday`,
//! `Mon, Dec 7`), a pending one past its time under [`OVERDUE_HEADING`], and
//! the fired ones as history under the day they fired.
//!
//! A row opens a side sheet with the whole text, the due time (absolute and
//! relative), the repeat, when it was created and, for a pending reminder,
//! [`CANCEL_LABEL`], which does what `shelbi remind cancel <id>` does
//! ([`cancel_local`]).

use std::time::Duration;

use chrono::{DateTime, Datelike, Local, TimeZone, Utc};
use shelbi_state::awareness::{job_id_number, FiredReminder, Reminder};

use crate::estimate::OVERDUE_TONE;
use crate::jobs::{age_label, clock_label, day_label, duration_label};
use crate::theme::Tone;

/// The screen's title.
pub const TITLE: &str = "Reminders";

/// The column headings, in order.
pub const COLUMNS: [&str; 4] = ["Id", "When", "Reminder", "Repeats"];

/// The section row above the pending reminders.
pub const PENDING_HEADING: &str = "Pending";

/// The section row above the fired reminders.
pub const FIRED_HEADING: &str = "Fired";

/// What the screen says with no reminders, pending or fired.
pub const EMPTY_TEXT: &str = "No reminders. Set one with `shelbi remind in 30m <text>`.";

/// The desktop empty state's heading, over [`EMPTY_DETAIL`] and
/// [`EMPTY_COMMAND`].
pub const EMPTY_TITLE: &str = "No reminders";

/// The desktop empty state's sentence.
pub const EMPTY_DETAIL: &str =
    "A reminder is a note for later. When its time comes the hub hands it to the orchestrator, once or on a repeat.";

/// The command the desktop empty state offers to copy.
pub const EMPTY_COMMAND: &str = "shelbi remind in 30m \"check the deploy\"";

/// The desktop agenda's heading over the soonest pending reminder's card.
pub const NEXT_UP_HEADING: &str = "Next up";

/// The desktop agenda's heading over pending reminders past their time that
/// haven't fired yet (the hub fires one on its next tick, so these are
/// rare: a hub that was down).
pub const OVERDUE_HEADING: &str = "Overdue";

/// What the screen says in a remote project's window: like the jobs, its
/// store is on the far machine and nothing reads it over the link yet.
pub const REMOTE_TEXT: &str = "Reminders aren't available for remote projects yet.";

/// How often a client re-reads the store while the screen is open (the Jobs
/// screen's cadence). Also when the relative times tick over.
pub const REFRESH_INTERVAL: Duration = crate::jobs::REFRESH_INTERVAL;

/// The side sheet's Cancel button (the TUI's `c` key reads it too).
pub const CANCEL_LABEL: &str = "Cancel reminder";

/// One read of a project's reminders
/// ([`crate::source::ProjectSource::reminders`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemindersRead {
    /// The pending reminders (store order) and the fired ones (the history).
    Reminders { pending: Vec<Reminder>, fired: Vec<FiredReminder> },
    /// The source can't read the store (a remote project's window).
    Unavailable,
    /// The store or the history couldn't be read or parsed.
    Failed(String),
}

/// Read a local project's pending reminders and its reminder history.
pub fn read_local(project: &str) -> RemindersRead {
    let pending = match shelbi_state::awareness::read_awareness(project) {
        Ok(store) => store.reminders,
        Err(e) => return RemindersRead::Failed(e.to_string()),
    };
    match shelbi_state::awareness::read_reminder_history(project) {
        Ok(fired) => RemindersRead::Reminders { pending, fired },
        Err(e) => RemindersRead::Failed(e.to_string()),
    }
}

/// `shelbi remind cancel <id>` for a local project: drop pending reminder
/// `id` from the store. An id that is no longer pending (it just fired, or
/// was canceled elsewhere) says so.
pub fn cancel_local(project: &str, id: &str) -> Result<(), String> {
    match shelbi_state::awareness::cancel_reminder(project, id) {
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(format!("reminder {id} is no longer pending")),
        Err(e) => Err(e.to_string()),
    }
}

/// What the screen shows.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum RemindersModel {
    /// Before the first read lands.
    #[default]
    Loading,
    /// The pending reminders, soonest first, then the fired ones, newest
    /// first. Never empty: no reminders is [`Self::Empty`].
    Rows(Vec<ReminderRow>),
    /// No reminders ([`EMPTY_TEXT`]).
    Empty,
    /// Not readable from this window ([`REMOTE_TEXT`]).
    Unavailable,
    /// The read failed, with why.
    Failed(String),
}

impl RemindersModel {
    /// The model for one read.
    pub fn from_read(read: RemindersRead) -> Self {
        match read {
            RemindersRead::Reminders { pending, fired } if pending.is_empty() && fired.is_empty() => {
                RemindersModel::Empty
            }
            RemindersRead::Reminders { pending, fired } => RemindersModel::Rows(reminder_rows(&pending, &fired)),
            RemindersRead::Unavailable => RemindersModel::Unavailable,
            RemindersRead::Failed(e) => RemindersModel::Failed(e),
        }
    }

    /// The rows, empty unless the model is [`Self::Rows`].
    pub fn rows(&self) -> &[ReminderRow] {
        match self {
            RemindersModel::Rows(rows) => rows,
            _ => &[],
        }
    }

    /// The row whose [`ReminderRow::key`] is `key`.
    pub fn row(&self, key: &str) -> Option<&ReminderRow> {
        self.rows().iter().find(|r| r.key == key)
    }

    /// Where `row` (from an earlier read) is now: the row with its key, or,
    /// for a pending reminder that has since fired, that firing's row. `None`
    /// once it is gone (canceled, or aged out of the history).
    pub fn follow(&self, row: &ReminderRow) -> Option<&ReminderRow> {
        self.row(&row.key).or_else(|| {
            row.is_pending()
                .then(|| self.rows().iter().find(|r| !r.is_pending() && r.id == row.id && r.due_at == row.due_at))
                .flatten()
        })
    }

    /// How many rows are pending reminders (they come first).
    pub fn pending_count(&self) -> usize {
        self.rows().iter().filter(|r| r.is_pending()).count()
    }

    /// The pending reminders' rows, soonest due first.
    pub fn pending(&self) -> &[ReminderRow] {
        &self.rows()[..self.pending_count()]
    }

    /// The fired reminders' rows, newest fired first.
    pub fn fired(&self) -> &[ReminderRow] {
        &self.rows()[self.pending_count()..]
    }

    /// The soonest pending reminder: the agenda's [`NEXT_UP_HEADING`] card.
    pub fn next_up(&self) -> Option<&ReminderRow> {
        self.pending().first()
    }

    /// The agenda under the Next up card: the other pending reminders by
    /// their local day in `tz`, as of `now`. For each day, the index into
    /// [`pending`](Self::pending) of its first row (never 0: that one is
    /// Next up) and its heading ([`agenda_label`]). Empty with at most one
    /// pending reminder.
    pub fn agenda_days<Tz: TimeZone>(&self, now: DateTime<Utc>, tz: &Tz) -> Vec<(usize, String)>
    where
        Tz::Offset: std::fmt::Display,
    {
        group_runs(self.pending().iter().enumerate().skip(1).map(|(i, r)| (i, agenda_label(r.due_at, now, tz))))
    }

    /// The fired reminders by the local day in `tz` they fired, as of
    /// `now`: for each day, the index into [`fired`](Self::fired) of its
    /// first row and its heading ([`day_label`]: `Today`, `Yesterday`, `Mon,
    /// Nov 16`). Empty with nothing fired.
    pub fn fired_days<Tz: TimeZone>(&self, now: DateTime<Utc>, tz: &Tz) -> Vec<(usize, String)>
    where
        Tz::Offset: std::fmt::Display,
    {
        group_runs(self.fired().iter().enumerate().filter_map(|(i, r)| Some((i, day_label(r.fired_at()?, now, tz)))))
    }

    /// The index of the first fired row, where the [`FIRED_HEADING`] row
    /// goes; `None` with nothing fired.
    pub fn fired_start(&self) -> Option<usize> {
        self.rows().iter().position(|r| !r.is_pending())
    }

    /// The section headings and the row index each sits above:
    /// [`PENDING_HEADING`] at 0 when anything is pending, [`FIRED_HEADING`]
    /// at [`fired_start`](Self::fired_start).
    pub fn headings(&self) -> Vec<(usize, &'static str)> {
        let mut out = Vec::new();
        if self.pending_count() > 0 {
            out.push((0, PENDING_HEADING));
        }
        if let Some(at) = self.fired_start() {
            out.push((at, FIRED_HEADING));
        }
        out
    }

    /// The title's count: `3 pending · 2 fired`, either half alone when the
    /// other is zero, `None` with no rows.
    pub fn summary(&self) -> Option<String> {
        let pending = self.pending_count();
        let fired = self.rows().len() - pending;
        match (pending, fired) {
            (0, 0) => None,
            (p, 0) => Some(format!("{p} pending")),
            (0, f) => Some(format!("{f} fired")),
            (p, f) => Some(format!("{p} pending · {f} fired")),
        }
    }

    /// The line the screen shows in place of a list, if it shows one.
    pub fn message(&self) -> Option<String> {
        match self {
            RemindersModel::Loading => Some("Loading…".to_string()),
            RemindersModel::Rows(_) => None,
            RemindersModel::Empty => Some(EMPTY_TEXT.to_string()),
            RemindersModel::Unavailable => Some(REMOTE_TEXT.to_string()),
            RemindersModel::Failed(e) => Some(format!("Couldn't read the project's reminders: {e}")),
        }
    }
}

/// Whether a row's reminder is still to come, or when it fired.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReminderState {
    Pending,
    Fired { fired_at: DateTime<Utc> },
}

/// One reminder's row: a pending reminder, or one firing of a reminder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReminderRow {
    /// Unique among the rows: the id for a pending reminder, the id and the
    /// slot it fired for for a firing (a recurring reminder fires many times
    /// and is pending as well). What a client's selection and open sheet
    /// follow across reads.
    pub key: String,
    /// The store id (`r<N>`).
    pub id: String,
    /// The reminder's text. Whole; a cell cuts it.
    pub text: String,
    /// When it is (or was) due.
    pub due_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
    /// A recurring reminder's repeat.
    pub every: Option<Duration>,
    pub state: ReminderState,
}

impl ReminderRow {
    /// A pending reminder's row.
    pub fn from_pending(r: &Reminder) -> Self {
        ReminderRow {
            key: r.id.clone(),
            id: r.id.clone(),
            text: r.text.clone(),
            due_at: r.due_at,
            created_at: r.created_at,
            every: r.every,
            state: ReminderState::Pending,
        }
    }

    /// A firing's row, from the history.
    pub fn from_fired(r: &FiredReminder) -> Self {
        ReminderRow {
            key: format!("{}@{}", r.id, r.due_at.timestamp()),
            id: r.id.clone(),
            text: r.text.clone(),
            due_at: r.due_at,
            created_at: r.created_at,
            every: r.every,
            state: ReminderState::Fired { fired_at: r.fired_at },
        }
    }

    pub fn is_pending(&self) -> bool {
        self.state == ReminderState::Pending
    }

    /// When it fired; `None` while pending.
    pub fn fired_at(&self) -> Option<DateTime<Utc>> {
        match self.state {
            ReminderState::Pending => None,
            ReminderState::Fired { fired_at } => Some(fired_at),
        }
    }

    /// The When cell at `now`: `in 14m`, `in 2h`, `tomorrow` for a pending
    /// reminder ([`due_label`]); `fired 20m ago` for a fired one.
    pub fn when(&self, now: DateTime<Utc>) -> String {
        match self.state {
            ReminderState::Pending => due_label(self.due_at, now, &Local),
            ReminderState::Fired { fired_at } => format!("fired {}", age_label(fired_at, now)),
        }
    }

    /// The Repeats cell: `every 1d`, or empty for a one-shot reminder.
    pub fn repeats(&self) -> String {
        self.every.map(|e| format!("every {}", interval_label(e))).unwrap_or_default()
    }

    /// The color of the When cell: a pending reminder blue, a fired one
    /// muted (`None`, the cell's default).
    pub fn when_tone(&self) -> Option<Tone> {
        self.is_pending().then_some(Tone::Blue)
    }

    /// The side sheet's status line: `● Pending · in 14m`, `✓ Fired · 20m
    /// ago`. Colored with [`status_tone`](Self::status_tone).
    pub fn sheet_status(&self, now: DateTime<Utc>) -> String {
        match self.state {
            ReminderState::Pending => format!("● Pending · {}", due_label(self.due_at, now, &Local)),
            ReminderState::Fired { fired_at } => format!("✓ Fired · {}", age_label(fired_at, now)),
        }
    }

    /// The sheet's status color: pending blue, fired gray.
    pub fn status_tone(&self) -> Tone {
        match self.state {
            ReminderState::Pending => Tone::Blue,
            ReminderState::Fired { .. } => Tone::Gray,
        }
    }

    /// The sheet's facts, label and value, in order: Due (absolute and
    /// relative), Fired (a fired row), Repeats, Created, Id.
    pub fn details(&self, now: DateTime<Utc>) -> Vec<(&'static str, String)> {
        self.details_in(now, &Local)
    }

    fn details_in<Tz: TimeZone>(&self, now: DateTime<Utc>, tz: &Tz) -> Vec<(&'static str, String)>
    where
        Tz::Offset: std::fmt::Display,
    {
        let mut out = vec![("Due", format!("{} · {}", clock_label(self.due_at, now, tz), relative_label(self.due_at, now)))];
        if let Some(fired_at) = self.fired_at() {
            out.push(("Fired", format!("{} · {}", clock_label(fired_at, now, tz), age_label(fired_at, now))));
        }
        let repeats = match self.every {
            Some(e) if self.is_pending() => format!("Every {}", interval_label(e)),
            Some(e) => format!("Every {} (still pending on its next slot)", interval_label(e)),
            None => "Doesn't repeat".to_string(),
        };
        out.push(("Repeats", repeats));
        out.push(("Created", format!("{} · {}", clock_label(self.created_at, now, tz), age_label(self.created_at, now))));
        out.push(("Id", self.id.clone()));
        out
    }

    /// Whether it is pending at or past its time: the hub hasn't fired it
    /// yet. It wears [`OVERDUE_TONE`], like a job past its estimate.
    pub fn overdue(&self, now: DateTime<Utc>) -> bool {
        self.is_pending() && self.due_at <= now
    }

    /// The agenda's countdown at `now`: `in 45s`, `in 12m`, `in 3h 27m`,
    /// `in 2d 4h` ahead; `due now` in its first minute past, then `due 20m
    /// ago`. Empty for a fired row.
    pub fn countdown(&self, now: DateTime<Utc>) -> String {
        if !self.is_pending() {
            return String::new();
        }
        if self.due_at <= now {
            if (now - self.due_at).num_seconds() < 60 {
                return "due now".to_string();
            }
            return format!("due {}", age_label(self.due_at, now));
        }
        let secs = (self.due_at - now).num_seconds().unsigned_abs();
        if secs < 86_400 {
            return relative_label(self.due_at, now);
        }
        let hours = secs / 3600;
        match (hours / 24, hours % 24) {
            (d, 0) => format!("in {d}d"),
            (d, h) => format!("in {d}d {h}h"),
        }
    }

    /// The agenda's time gutter in `tz`: when a pending reminder is due, or
    /// when a fired one fired, as a time of day (`10:45 AM`); the day is its
    /// group's heading.
    pub fn gutter_time<Tz: TimeZone>(&self, tz: &Tz) -> String
    where
        Tz::Offset: std::fmt::Display,
    {
        let t = self.fired_at().unwrap_or(self.due_at);
        t.with_timezone(tz).format("%-I:%M %p").to_string()
    }

    /// The Next up card's due time in `tz`: the time alone today (`10:45
    /// AM`), the date too on another day ([`clock_label`]).
    pub fn due_clock<Tz: TimeZone>(&self, now: DateTime<Utc>, tz: &Tz) -> String
    where
        Tz::Offset: std::fmt::Display,
    {
        clock_label(self.due_at, now, tz)
    }

    /// How far along the wait from when it was set to when it is due, at
    /// `now`, from 0 to 1 (full once due, and for a fired row).
    pub fn progress(&self, now: DateTime<Utc>) -> f32 {
        let span = (self.due_at - self.created_at).num_milliseconds();
        if !self.is_pending() || span <= 0 || now >= self.due_at {
            return 1.0;
        }
        let gone = (now - self.created_at).num_milliseconds().max(0);
        (gone as f64 / span as f64).clamp(0.0, 1.0) as f32
    }

    /// The Next up bar's label: `set 48m ago`.
    pub fn set_label(&self, now: DateTime<Utc>) -> String {
        format!("set {}", age_label(self.created_at, now))
    }

    /// The color of its countdown and glyph at `now`: [`OVERDUE_TONE`] past
    /// its time, blue while pending, `None` (muted) once fired.
    pub fn tone_at(&self, now: DateTime<Utc>) -> Option<Tone> {
        if self.overdue(now) {
            return Some(OVERDUE_TONE);
        }
        self.when_tone()
    }

    /// The sheet's status color at `now`: [`OVERDUE_TONE`] past its time,
    /// else [`status_tone`](Self::status_tone).
    pub fn status_tone_at(&self, now: DateTime<Utc>) -> Tone {
        if self.overdue(now) {
            return OVERDUE_TONE;
        }
        self.status_tone()
    }

    /// Whether the sheet offers [`CANCEL_LABEL`]: the reminder is pending.
    pub fn can_cancel(&self) -> bool {
        self.is_pending()
    }
}

/// An open side sheet's reminder, as both clients track it across reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SheetState {
    /// The row it shows, as last seen in the model (kept when the reminder
    /// leaves it).
    pub row: ReminderRow,
    /// A cancel was asked for and the reminder is still pending.
    pub canceling: bool,
    /// Why the last cancel failed.
    pub cancel_error: Option<String>,
}

impl SheetState {
    pub fn new(row: ReminderRow) -> Self {
        SheetState { row, canceling: false, cancel_error: None }
    }

    /// Fold in a new model: the sheet follows its reminder, into the fired
    /// list when it fires. Returns `false` when the sheet should close: a
    /// reminder it was canceling has gone. One that went otherwise (aged
    /// out) stays as last seen.
    pub fn follow(&mut self, model: &RemindersModel) -> bool {
        match model.follow(&self.row) {
            Some(now) => {
                if !now.is_pending() {
                    self.canceling = false;
                }
                self.row = now.clone();
                true
            }
            None => !self.canceling,
        }
    }

    /// Whether the sheet offers [`CANCEL_LABEL`] now: pending, and no cancel
    /// in flight.
    pub fn offers_cancel(&self) -> bool {
        self.row.can_cancel() && !self.canceling
    }

    /// Start a cancel: the reminder's id, once, while it is pending.
    pub fn begin_cancel(&mut self) -> Option<String> {
        if !self.offers_cancel() {
            return None;
        }
        self.canceling = true;
        self.cancel_error = None;
        Some(self.row.id.clone())
    }

    /// The cancel failed with `error` (it stays offered).
    pub fn cancel_failed(&mut self, error: String) {
        self.canceling = false;
        self.cancel_error = Some(error);
    }

    /// The status line: `● Canceling…` while a cancel is in flight, else
    /// the row's ([`ReminderRow::sheet_status`]).
    pub fn status(&self, now: DateTime<Utc>) -> String {
        if self.canceling {
            "● Canceling…".to_string()
        } else {
            self.row.sheet_status(now)
        }
    }
}

/// Every reminder's row: the pending ones soonest due first, then the
/// fired ones newest fired first. Ties keep the lower id first among the
/// pending (set first), the newer id first among the fired.
pub fn reminder_rows(pending: &[Reminder], fired: &[FiredReminder]) -> Vec<ReminderRow> {
    let mut rows: Vec<ReminderRow> = pending.iter().map(ReminderRow::from_pending).collect();
    rows.sort_by(|a, b| a.due_at.cmp(&b.due_at).then_with(|| job_id_number(&a.id).cmp(&job_id_number(&b.id))));
    let mut done: Vec<ReminderRow> = fired.iter().map(ReminderRow::from_fired).collect();
    done.sort_by(|a, b| {
        b.fired_at()
            .cmp(&a.fired_at())
            .then_with(|| b.due_at.cmp(&a.due_at))
            .then_with(|| job_id_number(&b.id).cmp(&job_id_number(&a.id)))
    });
    rows.extend(done);
    rows
}

/// The heading over the pending reminders due on `due`'s day in `tz`, as of
/// `now`: [`OVERDUE_HEADING`] once its time has passed, `Today`, `Tomorrow`,
/// the weekday within the week (`Thursday`), then the date (`Mon, Dec 7`),
/// with the year in another year (`Mon, Jan 4, 2027`).
pub fn agenda_label<Tz: TimeZone>(due: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    if due <= now {
        return OVERDUE_HEADING.to_string();
    }
    let day = due.with_timezone(tz).date_naive();
    let today = now.with_timezone(tz).date_naive();
    match (day - today).num_days() {
        i64::MIN..=0 => "Today".to_string(),
        1 => "Tomorrow".to_string(),
        2..=6 => day.format("%A").to_string(),
        _ if day.year() == today.year() => day.format("%a, %b %-d").to_string(),
        _ => day.format("%a, %b %-d, %Y").to_string(),
    }
}

/// `(index, heading)` pairs, in order, cut to where each run of one heading
/// starts.
fn group_runs(items: impl Iterator<Item = (usize, String)>) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for (i, label) in items {
        if out.last().is_none_or(|(_, last)| *last != label) {
            out.push((i, label));
        }
    }
    out
}

/// A pending reminder's When, in `tz`: `due now` once its time has come
/// (the hub fires it on its next tick), then `in 45s`, `in 14m`, `in 2h`;
/// `tomorrow` for one due on the next calendar day at least a few hours
/// out, then `in 3d`.
pub fn due_label<Tz: TimeZone>(due: DateTime<Utc>, now: DateTime<Utc>, tz: &Tz) -> String {
    let secs = (due - now).num_seconds();
    if secs <= 0 {
        return "due now".to_string();
    }
    if secs < 60 {
        return format!("in {secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("in {mins}m");
    }
    let hours = mins / 60;
    let tomorrow = now.with_timezone(tz).date_naive().succ_opt();
    if hours >= 6 && Some(due.with_timezone(tz).date_naive()) == tomorrow {
        return "tomorrow".to_string();
    }
    if hours < 24 {
        return format!("in {hours}h");
    }
    format!("in {}d", hours / 24)
}

/// `t` relative to `now`, for the sheet beside an absolute time: `in 2h
/// 5m` ahead (to the minute once past one), `20m ago` behind
/// ([`age_label`]).
fn relative_label(t: DateTime<Utc>, now: DateTime<Utc>) -> String {
    if t <= now {
        return age_label(t, now);
    }
    let secs = (t - now).num_seconds().unsigned_abs();
    let rounded = if secs >= 60 { secs / 60 * 60 } else { secs };
    format!("in {}", duration_label(Duration::from_secs(rounded)))
}

/// A repeat interval as the screen shows it: whole days as `1d`, else the
/// two largest units (`1h`, `1h 30m`, `10m`).
pub fn interval_label(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 86_400 && secs.is_multiple_of(86_400) {
        return format!("{}d", secs / 86_400);
    }
    duration_label(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, h, m, 0).unwrap()
    }

    fn reminder(id: &str, due_at: DateTime<Utc>, every: Option<u64>) -> Reminder {
        Reminder {
            id: id.into(),
            due_at,
            text: format!("text of {id}"),
            created_at: at(9, 0),
            every: every.map(Duration::from_secs),
        }
    }

    #[test]
    fn pending_come_first_soonest_due_then_fired_newest_first() {
        let pending = vec![
            reminder("r9", at(16, 0), Some(86_400)),
            reminder("r12", at(14, 24), None),
            reminder("r14", at(15, 0), None),
        ];
        let fired = vec![
            FiredReminder::fired(&reminder("r5", at(11, 0), Some(3600)), at(11, 0)),
            FiredReminder::fired(&reminder("r8", at(13, 50), None), at(13, 50)),
        ];
        let model = RemindersModel::from_read(RemindersRead::Reminders { pending, fired });
        let ids: Vec<&str> = model.rows().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r12", "r14", "r9", "r8", "r5"]);
        assert_eq!(model.pending_count(), 3);
        assert_eq!(model.fired_start(), Some(3));
        assert_eq!(model.headings(), [(0, PENDING_HEADING), (3, FIRED_HEADING)]);
        assert_eq!(model.summary().as_deref(), Some("3 pending · 2 fired"));
        assert_eq!(model.message(), None);

        let now = at(14, 10);
        let rows = model.rows();
        assert_eq!(rows[0].when(now), "in 14m");
        assert_eq!(rows[0].repeats(), "");
        assert_eq!(rows[2].repeats(), "every 1d");
        assert_eq!(rows[3].when(now), "fired 20m ago");
        assert_eq!(rows[4].when(now), "fired 3h ago");
        assert_eq!(rows[4].repeats(), "every 1h");
        assert_eq!((rows[0].when_tone(), rows[3].when_tone()), (Some(Tone::Blue), None));
    }

    #[test]
    fn a_recurring_reminder_is_listed_pending_and_once_per_firing() {
        let every = reminder("r5", at(15, 0), Some(3600));
        let fired = vec![
            FiredReminder::fired(&reminder("r5", at(13, 0), Some(3600)), at(13, 0)),
            FiredReminder::fired(&reminder("r5", at(14, 0), Some(3600)), at(14, 0)),
        ];
        let model = RemindersModel::from_read(RemindersRead::Reminders { pending: vec![every], fired });
        let keys: Vec<&str> = model.rows().iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys.len(), 3);
        let unique: std::collections::HashSet<&&str> = keys.iter().collect();
        assert_eq!(unique.len(), 3, "each row has its own key: {keys:?}");
        assert_eq!(keys[0], "r5");
        assert_eq!(model.row(keys[1]).unwrap().fired_at(), Some(at(14, 0)));
        assert!(model.row("r5").unwrap().can_cancel());
        assert!(!model.row(keys[2]).unwrap().can_cancel());
    }

    #[test]
    fn a_pending_row_follows_its_reminder_into_the_fired_list() {
        let due = reminder("r3", at(14, 0), None);
        let before = RemindersModel::from_read(RemindersRead::Reminders { pending: vec![due.clone()], fired: vec![] });
        let row = before.rows()[0].clone();
        let after = RemindersModel::from_read(RemindersRead::Reminders {
            pending: vec![],
            fired: vec![FiredReminder::fired(&due, at(14, 0))],
        });
        let now = after.follow(&row).expect("its firing");
        assert!(!now.is_pending() && now.id == "r3");
        // Canceled: nothing to follow.
        let empty = RemindersModel::from_read(RemindersRead::Reminders { pending: vec![], fired: vec![] });
        assert_eq!(empty.follow(&row), None);
    }

    #[test]
    fn a_sheet_follows_its_reminder_and_closes_once_its_cancel_lands() {
        let due = reminder("r3", at(14, 0), None);
        let model = |pending: Vec<Reminder>, fired: Vec<FiredReminder>| {
            RemindersModel::from_read(RemindersRead::Reminders { pending, fired })
        };
        let open = model(vec![due.clone()], vec![]);
        let mut sheet = SheetState::new(open.rows()[0].clone());
        assert!(sheet.offers_cancel());
        assert_eq!(sheet.begin_cancel().as_deref(), Some("r3"));
        assert_eq!(sheet.begin_cancel(), None, "once");
        assert_eq!(sheet.status(at(13, 0)), "● Canceling…");
        // It failed: offered again, with why.
        sheet.cancel_failed("no".into());
        assert!(sheet.offers_cancel() && sheet.cancel_error.as_deref() == Some("no"));
        // It fired before the cancel landed: the sheet follows it.
        assert_eq!(sheet.begin_cancel().as_deref(), Some("r3"));
        assert!(sheet.follow(&model(vec![], vec![FiredReminder::fired(&due, at(14, 0))])));
        assert!(!sheet.row.is_pending() && !sheet.canceling && !sheet.offers_cancel());
        // A canceled one is gone: the sheet closes.
        let mut sheet = SheetState::new(open.rows()[0].clone());
        sheet.begin_cancel();
        assert!(!sheet.follow(&model(vec![], vec![])));
        // Gone without a cancel (aged out): it stays as last seen.
        let mut sheet = SheetState::new(open.rows()[0].clone());
        assert!(sheet.follow(&model(vec![], vec![])));
    }

    #[test]
    fn the_model_names_each_state() {
        let none = RemindersRead::Reminders { pending: vec![], fired: vec![] };
        assert_eq!(RemindersModel::from_read(none), RemindersModel::Empty);
        assert_eq!(RemindersModel::Empty.message().as_deref(), Some(EMPTY_TEXT));
        assert!(EMPTY_TEXT.contains("`shelbi remind in 30m <text>`"));
        assert_eq!(RemindersModel::from_read(RemindersRead::Unavailable).message().as_deref(), Some(REMOTE_TEXT));
        assert!(RemindersModel::from_read(RemindersRead::Failed("bad json".into()))
            .message()
            .unwrap()
            .contains("bad json"));
        assert_eq!(RemindersModel::default().message().as_deref(), Some("Loading…"));
        let only_fired = RemindersModel::from_read(RemindersRead::Reminders {
            pending: vec![],
            fired: vec![FiredReminder::fired(&reminder("r1", at(9, 0), None), at(9, 0))],
        });
        assert_eq!(only_fired.summary().as_deref(), Some("1 fired"));
        assert_eq!(only_fired.headings(), [(0, FIRED_HEADING)]);
        let only_pending = RemindersModel::from_read(RemindersRead::Reminders {
            pending: vec![reminder("r1", at(9, 0), None)],
            fired: vec![],
        });
        assert_eq!((only_pending.summary().as_deref(), only_pending.fired_start()), (Some("1 pending"), None));
    }

    #[test]
    fn due_labels_count_down_and_say_tomorrow() {
        // UTC-5: 19:10 UTC on the 14th is 2:10 PM there.
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at(19, 10);
        let after = |s: i64| now + chrono::Duration::seconds(s);
        assert_eq!(due_label(now, now, &tz), "due now");
        assert_eq!(due_label(now - chrono::Duration::seconds(3), now, &tz), "due now");
        assert_eq!(due_label(after(45), now, &tz), "in 45s");
        assert_eq!(due_label(after(14 * 60), now, &tz), "in 14m");
        assert_eq!(due_label(after(2 * 3600 + 300), now, &tz), "in 2h");
        // 9 AM the next local day.
        assert_eq!(due_label(Utc.with_ymd_and_hms(2026, 11, 15, 14, 0, 0).unwrap(), now, &tz), "tomorrow");
        assert_eq!(due_label(after(3 * 86_400), now, &tz), "in 3d");
        // Just past local midnight, a couple of hours out: still hours.
        let late = Utc.with_ymd_and_hms(2026, 11, 15, 4, 0, 0).unwrap(); // 11 PM local
        assert_eq!(due_label(late + chrono::Duration::hours(2), late, &tz), "in 2h");
    }

    #[test]
    fn the_agenda_puts_the_soonest_up_next_and_groups_the_rest_by_day() {
        // UTC-5: 19:10 UTC on Sat the 14th is 2:10 PM there.
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at(19, 10);
        let local = |d: u32, h: u32, m: u32| tz.with_ymd_and_hms(2026, 11, d, h, m, 0).unwrap().with_timezone(&Utc);
        let pending = vec![
            reminder("r1", now - chrono::Duration::minutes(20), None),
            reminder("r2", now + chrono::Duration::minutes(12), None),
            reminder("r3", local(14, 17, 30), Some(86_400)),
            reminder("r4", local(15, 9, 0), None),
            reminder("r5", local(15, 11, 0), None),
            reminder("r6", local(19, 9, 0), None),
            reminder("r7", local(30, 9, 0), None),
        ];
        let fired = vec![
            FiredReminder::fired(&reminder("r0", local(14, 10, 2), None), local(14, 10, 2)),
            FiredReminder::fired(&reminder("r9", local(13, 23, 40), None), local(13, 23, 40)),
            FiredReminder::fired(&reminder("r8", local(14, 8, 15), None), local(14, 8, 15)),
        ];
        let model = RemindersModel::from_read(RemindersRead::Reminders { pending, fired });
        assert_eq!(model.next_up().map(|r| r.id.as_str()), Some("r1"), "soonest, overdue or not");
        assert_eq!(model.pending().len(), 7);
        assert_eq!(model.fired().len(), 3);
        let days = model.agenda_days(now, &tz);
        assert_eq!(
            days,
            [
                (1, "Today".to_string()),
                (3, "Tomorrow".to_string()),
                (5, "Thursday".to_string()),
                (6, "Mon, Nov 30".to_string()),
            ],
            "Next up is left out; r2 is still ahead today"
        );
        // Past due, the next one leads an Overdue group.
        let later = RemindersModel::from_read(RemindersRead::Reminders {
            pending: vec![
                reminder("r1", now - chrono::Duration::minutes(30), None),
                reminder("r2", now - chrono::Duration::minutes(20), None),
                reminder("r3", now + chrono::Duration::minutes(20), None),
            ],
            fired: vec![],
        });
        assert_eq!(later.agenda_days(now, &tz), [(1, OVERDUE_HEADING.to_string()), (2, "Today".to_string())]);
        // Fired: newest first, by the day each fired.
        let ids: Vec<&str> = model.fired().iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, ["r0", "r8", "r9"]);
        assert_eq!(model.fired_days(now, &tz), [(0, "Today".to_string()), (2, "Yesterday".to_string())]);
        assert_eq!(model.fired()[0].gutter_time(&tz), "10:02 AM");
        assert_eq!(model.pending()[3].gutter_time(&tz), "9:00 AM");
        assert_eq!(model.pending()[4].due_clock(now, &tz), "Nov 15, 11:00 AM");
        assert_eq!(model.pending()[1].due_clock(now, &tz), "2:22 PM");
        assert_eq!(agenda_label(local(14, 0, 0) + chrono::Duration::days(400), now, &tz), "Sun, Dec 19, 2027");
    }

    #[test]
    fn countdowns_tick_down_then_say_due() {
        let now = at(14, 10);
        let row = |due: DateTime<Utc>| ReminderRow::from_pending(&reminder("r1", due, None));
        let s = |secs: i64| row(now + chrono::Duration::seconds(secs)).countdown(now);
        assert_eq!(s(45), "in 45s");
        assert_eq!(s(12 * 60 + 20), "in 12m");
        assert_eq!(s(3 * 3600 + 27 * 60 + 5), "in 3h 27m");
        assert_eq!(s(22 * 3600), "in 22h");
        assert_eq!(s(2 * 86_400 + 4 * 3600 + 60), "in 2d 4h");
        assert_eq!(s(3 * 86_400), "in 3d");
        assert_eq!(s(0), "due now");
        assert_eq!(s(-30), "due now");
        assert_eq!(s(-20 * 60), "due 20m ago");
        let fired = ReminderRow::from_fired(&FiredReminder::fired(&reminder("r1", now, None), now));
        assert_eq!(fired.countdown(now), "");
    }

    #[test]
    fn a_reminder_past_its_time_is_overdue_until_it_fires() {
        let now = at(14, 10);
        let ahead = ReminderRow::from_pending(&reminder("r1", at(14, 30), None));
        let past = ReminderRow::from_pending(&reminder("r2", at(14, 10), None));
        let fired = ReminderRow::from_fired(&FiredReminder::fired(&reminder("r3", at(14, 0), None), at(14, 0)));
        assert!(!ahead.overdue(now) && past.overdue(now) && !fired.overdue(now));
        assert_eq!(ahead.tone_at(now), Some(Tone::Blue));
        assert_eq!(past.tone_at(now), Some(OVERDUE_TONE));
        assert_eq!(fired.tone_at(now), None);
        assert_eq!(past.status_tone_at(now), OVERDUE_TONE);
        assert_eq!(ahead.status_tone_at(now), Tone::Blue);
    }

    #[test]
    fn progress_runs_from_set_to_due() {
        // Set at 9:00, due at 14:00.
        let row = ReminderRow::from_pending(&reminder("r1", at(14, 0), None));
        assert_eq!(row.progress(at(9, 0)), 0.0);
        assert_eq!(row.progress(at(8, 0)), 0.0, "a skewed clock");
        assert!((row.progress(at(11, 30)) - 0.5).abs() < 1e-6);
        assert_eq!(row.progress(at(14, 0)), 1.0);
        assert_eq!(row.progress(at(15, 0)), 1.0);
        assert_eq!(row.set_label(at(9, 48)), "set 48m ago");
        // Due when set: full.
        let instant = ReminderRow::from_pending(&reminder("r2", at(9, 0), None));
        assert_eq!(instant.progress(at(8, 0)), 1.0);
    }

    #[test]
    fn intervals_read_in_days_when_whole() {
        assert_eq!(interval_label(Duration::from_secs(86_400)), "1d");
        assert_eq!(interval_label(Duration::from_secs(2 * 86_400)), "2d");
        assert_eq!(interval_label(Duration::from_secs(3600)), "1h");
        assert_eq!(interval_label(Duration::from_secs(5400)), "1h 30m");
        assert_eq!(interval_label(Duration::from_secs(600)), "10m");
        assert_eq!(interval_label(Duration::from_secs(90_000)), "25h");
    }

    #[test]
    fn the_sheet_lists_due_repeat_created_and_offers_cancel_while_pending() {
        let tz = FixedOffset::west_opt(5 * 3600).unwrap();
        let now = at(19, 10);
        let pending = ReminderRow::from_pending(&reminder("r9", at(19, 24), Some(86_400)));
        let details = pending.details_in(now, &tz);
        assert_eq!(details[0], ("Due", "2:24 PM · in 14m".to_string()));
        assert_eq!(details[1], ("Repeats", "Every 1d".to_string()));
        assert_eq!(details[2], ("Created", "4:00 AM · 10h ago".to_string()));
        assert_eq!(details[3], ("Id", "r9".to_string()));
        assert!(pending.can_cancel());
        assert_eq!(pending.status_tone(), Tone::Blue);
        assert!(pending.sheet_status(now).starts_with("● Pending · "));

        let fired = ReminderRow::from_fired(&FiredReminder::fired(&reminder("r8", at(18, 50), None), at(18, 50)));
        let details = fired.details_in(now, &tz);
        let labels: Vec<&str> = details.iter().map(|(l, _)| *l).collect();
        assert_eq!(labels, ["Due", "Fired", "Repeats", "Created", "Id"]);
        assert_eq!(details[0].1, "1:50 PM · 20m ago");
        assert_eq!(details[1].1, "1:50 PM · 20m ago");
        assert_eq!(details[2].1, "Doesn't repeat");
        assert!(!fired.can_cancel());
        assert_eq!(fired.sheet_status(now), "✓ Fired · 20m ago");
        assert_eq!(fired.status_tone(), Tone::Gray);
    }

    #[test]
    fn cancel_local_drops_a_pending_reminder_and_says_when_it_is_gone() {
        let lock = crate::test_env::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _home = crate::settings::agents::fixture::Home::new("reminders", "demo", lock);
        let r = shelbi_state::awareness::add_reminder("demo", Utc::now() + chrono::Duration::hours(1), "x", None).unwrap();
        assert!(matches!(read_local("demo"), RemindersRead::Reminders { ref pending, .. } if pending.len() == 1));
        assert_eq!(cancel_local("demo", &r.id), Ok(()));
        assert!(cancel_local("demo", &r.id).unwrap_err().contains("no longer pending"));
        assert_eq!(read_local("demo"), RemindersRead::Reminders { pending: vec![], fired: vec![] });
    }
}
