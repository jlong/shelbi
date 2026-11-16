//! The Jobs screen (`View::Jobs`, palette-only): the project's awareness
//! jobs as a plain five-column table, running ones first, then the finished
//! ones under a `Finished` row, drawn from the shared [`shelbi_app::jobs`]
//! model the desktop's Jobs view draws too. A failed job's Status is red.
//!
//! ```text
//!   Jobs   1 running · 2 finished
//!
//!    Description        │ Command               │ Status          │ Duration │ Finished
//!   ────────────────────┼───────────────────────┼─────────────────┼──────────┼──────────────────
//!  ▐ bundle desktop app │ zsh -c /Users/me/bu…  │ Running         │ 4m 12s   │                  ▌
//!   Finished
//!    desktop tests      │ cargo test            │ Failed · exit 3 │ 2m 13s   │ 1:58 PM · 12m ago
//!    PR #2265           │ watch PR #2265        │ Merged          │ 1h 4m    │ 12:58 PM · 1h ago
//!
//!   Command  zsh -c /Users/me/bundle.sh && open out      <- the selected row's, whole
//!   ↑/↓ select
//! ```
//!
//! A running job started with `--estimate` draws a one-row block bar in its
//! Duration cell, elapsed against the estimate, in front of the same text;
//! the bar stays full in the overdue tone once the estimate is used up, and
//! the Status reads `Overdue`. A finished job shows its actual duration
//! against the estimate as text only.
//!
//! ```text
//!    Description        │ Command     │ Status  │ Duration
//!  ▐ desktop tests      │ cargo test  │ Running │ ███▍░░░░ 4m 12s / 10m ▌
//!    bundle desktop app │ ./bundle.sh │ Overdue │ ████████ 45m / 30m
//! ```
//!
//! Enter opens the selected job's sheet over the table, drawn the way the
//! board's issue detail is (the same centered box, cleared under it, in the
//! dialogs' frame, [`crate::kanban::popover_rect`]): its `id · label` in the
//! border, its status and elapsed time, then a running command's live
//! terminal (attached read-only: keys never reach the job) or a finished
//! one's saved output. ↑/↓ move to the next job and the sheet follows, as a
//! click on another row does on the desktop; `c` cancels a running job; Esc
//! closes the sheet.
//!
//! ```text
//!  Jobs   1 running
//!   Desc┌ j4 · demo: count to 120 ─────────────────┐
//!  ▐ dem│ ● Running · 42s                           │
//!       │───────────────────────────────────────────│
//!       │ step 41 of 120                            │
//!       │ step 42 of 120                            │
//!       │ ↑/↓ job · c cancel job · esc close        │
//!       └───────────────────────────────────────────┘
//! ```
//!
//! The rows come in on the shell's core snapshot (read with the sidebar, so a
//! job appears, or moves under Finished, on the shell's refresh); the
//! durations and ages tick over because the shell redraws on that refresh
//! too.

use chrono::{DateTime, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use std::sync::mpsc;

use shelbi_app::jobs::{
    one_line, JobRow, JobsModel, JobsRead, SheetLoad, CANCEL_LABEL, COLUMNS, FINISHED_HEADING, TITLE,
};
use shelbi_term::Size;

use crate::list_screen;
use crate::shell::terminal_view::TerminalPane;
use crate::table::{Cell, Column, Table};

/// The cells the Duration column's progress bar takes.
const BAR_CELLS: usize = 8;
use crate::theme::palette;

/// How many lines the selected row's whole command may take under the table.
const DETAIL_LINES: u16 = 3;

/// The page: the model, which row is selected, and the open sheet.
#[derive(Default)]
pub(crate) struct JobsPage {
    model: JobsModel,
    selected: usize,
    scroll: usize,
    sheet: Option<JobSheet>,
}

/// The side sheet for one job: its terminal (live, or the saved output), or
/// a line saying why there is none.
pub(crate) struct JobSheet {
    id: String,
    /// The row it opened on, for when the job leaves the model (pruned).
    opened: JobRow,
    /// The load, until it lands.
    loading: Option<mpsc::Receiver<SheetLoad>>,
    body: SheetBody,
    /// A cancel was asked for and the row still says running.
    canceling: bool,
}

enum SheetBody {
    Loading,
    Live {
        pane: TerminalPane,
        conn: shelbi_client::Connection,
        events: shelbi_client::SessionEvents,
        /// The size last reported to the session.
        sent: Option<(u16, u16)>,
    },
    /// Saved output, laid out at the size it was last drawn at.
    Saved { bytes: Vec<u8>, pane: Option<(Size, TerminalPane)> },
    Message(String),
}

impl JobsPage {
    /// Fold in a read. The selection follows its job when it moves.
    pub(crate) fn apply(&mut self, read: JobsRead) {
        let kept = self.selected_row().map(|r| r.id.clone());
        let model = JobsModel::from_read(read);
        if model == self.model {
            return;
        }
        self.model = model;
        if let Some(at) = kept.and_then(|id| self.model.rows().iter().position(|r| r.id == id)) {
            self.selected = at;
        }
        self.clamp();
    }

    pub(crate) fn selected_row(&self) -> Option<&JobRow> {
        self.model.rows().get(self.selected)
    }

    /// Open the selected job's sheet (replacing an open one), loading it off
    /// this thread with [`shelbi_app::jobs::open_sheet`].
    pub(crate) fn open_sheet(&mut self, project: &str) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        // Its own job again: the sheet stays as it is.
        if self.sheet.as_ref().is_some_and(|s| s.id == row.id) {
            return;
        }
        let (tx, rx) = mpsc::channel();
        let (project, load_row) = (project.to_string(), row.clone());
        std::thread::spawn(move || {
            let _ = tx.send(shelbi_app::jobs::open_sheet(&project, &load_row));
        });
        self.sheet = Some(JobSheet {
            id: row.id.clone(),
            opened: row,
            loading: Some(rx),
            body: SheetBody::Loading,
            canceling: false,
        });
    }

    pub(crate) fn close_sheet(&mut self) {
        self.sheet = None;
    }

    pub(crate) fn sheet_open(&self) -> bool {
        self.sheet.is_some()
    }

    /// With the sheet open, show the selected job in it (the selection
    /// moved under it).
    pub(crate) fn follow_selection(&mut self, project: &str) {
        if self.sheet.is_some() {
            self.open_sheet(project);
        }
    }

    /// The open sheet's job, while it is running: what `c` cancels. Marks the
    /// sheet as canceling.
    pub(crate) fn take_cancel(&mut self) -> Option<String> {
        let running = self.sheet_row().is_some_and(JobRow::can_cancel);
        let sheet = self.sheet.as_mut()?;
        if !running || sheet.canceling {
            return None;
        }
        sheet.canceling = true;
        Some(sheet.id.clone())
    }

    /// The open sheet's row, current from the model.
    fn sheet_row(&self) -> Option<&JobRow> {
        let sheet = self.sheet.as_ref()?;
        Some(self.model.rows().iter().find(|r| r.id == sheet.id).unwrap_or(&sheet.opened))
    }

    /// Take in the sheet's load and its terminal's output. Returns whether
    /// anything changed on screen.
    pub(crate) fn pump(&mut self) -> bool {
        let Some(sheet) = self.sheet.as_mut() else {
            return false;
        };
        let mut changed = false;
        if let Some(rx) = &sheet.loading {
            match rx.try_recv() {
                Ok(load) => {
                    sheet.body = match load {
                        SheetLoad::Live(c) => SheetBody::Live {
                            pane: TerminalPane::new(Size::new(c.size.cols, c.size.rows)),
                            conn: c.conn,
                            events: c.events,
                            sent: None,
                        },
                        SheetLoad::Saved(bytes) => SheetBody::Saved { bytes, pane: None },
                        SheetLoad::Message(m) => SheetBody::Message(m),
                    };
                    sheet.loading = None;
                    changed = true;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    sheet.body = SheetBody::Message("Couldn't open the job.".into());
                    sheet.loading = None;
                    changed = true;
                }
            }
        }
        if let SheetBody::Live { pane, events, .. } = &mut sheet.body {
            while let Some(ev) = events.try_recv() {
                changed |= pane.apply_event(ev);
            }
        }
        changed
    }

    pub(crate) fn nav_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    pub(crate) fn nav_down(&mut self) {
        self.selected += 1;
        self.clamp();
    }

    pub(crate) fn nav_first(&mut self) {
        self.selected = 0;
    }

    pub(crate) fn nav_last(&mut self) {
        self.selected = self.model.rows().len().saturating_sub(1);
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.model.rows().len().saturating_sub(1));
    }

    /// The table at `now`.
    fn table(&self, now: DateTime<Utc>) -> Table {
        let text = Style::default().fg(palette().foreground);
        let muted = Style::default().fg(palette().muted);
        let mut t = Table::new(vec![
            Column::new(COLUMNS[0]).min(10).max(32),
            Column::new(COLUMNS[1]).min(12).max(400).fill(),
            Column::new(COLUMNS[2]).min(7).max(20),
            Column::new(COLUMNS[3]).min(6).max(26).drop(2),
            Column::new(COLUMNS[4]).min(8).max(30).drop(1),
        ])
        .selected(Some(self.selected), true);
        for row in self.model.rows() {
            let mut status = Style::default().fg(row.status_tone_at(now).pick(palette()));
            if row.failed() || row.overdue(now) {
                status = status.add_modifier(Modifier::BOLD);
            }
            let duration_style = row.duration_tone(now).map_or(muted, |t| Style::default().fg(t.pick(palette())));
            let duration = match row.bar(now) {
                Some(bar) => {
                    let mut spans = crate::kanban::estimate_bar_spans(&bar, BAR_CELLS);
                    spans.push(Span::styled(format!(" {}", row.duration(now)), duration_style));
                    Cell::spans(spans)
                }
                None => Cell::text(row.duration(now), duration_style),
            };
            t.push(vec![
                Cell::text(one_line(&row.description), text),
                Cell::text(one_line(&row.command), text),
                Cell::text(row.status_label_at(now), status),
                duration,
                Cell::text(row.finished(now), muted),
            ]);
        }
        t
    }

    /// The section headings: [`FINISHED_HEADING`] above the first finished
    /// job.
    fn headings(&self) -> Vec<(usize, &'static str)> {
        self.model.finished_start().map(|at| (at, FINISHED_HEADING)).into_iter().collect()
    }
}

/// Draw the page into `area` at `now`: the table at its full size, and the
/// open sheet over it in the issue detail's box.
pub(crate) fn render_full(f: &mut Frame, page: &mut JobsPage, area: Rect, now: DateTime<Utc>, truecolor: bool) {
    render_page(f, page, area, now);
    if page.sheet.is_some() {
        render_sheet(f, page, area, now, truecolor);
    }
}

/// The sheet over `area`: the shared frame ([`list_screen::render_sheet_frame`]:
/// the job's `id · label` in its border, the status, the keys) around its
/// terminal.
fn render_sheet(f: &mut Frame, page: &mut JobsPage, area: Rect, now: DateTime<Utc>, truecolor: bool) {
    let Some(row) = page.sheet_row().cloned() else {
        return;
    };
    let sheet = page.sheet.as_mut().expect("a sheet row means a sheet");
    let status = if sheet.canceling && row.is_running() {
        "● Canceling…".to_string()
    } else {
        row.sheet_status(now)
    };
    let keys = list_screen::sheet_keys("job", (row.can_cancel() && !sheet.canceling).then_some(CANCEL_LABEL));
    let Some(term) = list_screen::render_sheet_frame(
        f,
        area,
        &format!("{} · {}", row.id, one_line(&row.description)),
        status,
        Style::default().fg(row.status_tone_at(now).pick(palette())),
        &keys,
    ) else {
        return;
    };
    let muted = Style::default().fg(palette().muted);
    let message = |f: &mut Frame, text: &str| {
        f.render_widget(Paragraph::new(Span::styled(text.to_string(), muted)).wrap(Wrap { trim: false }), term);
    };
    match &mut sheet.body {
        SheetBody::Loading => message(f, "Loading…"),
        SheetBody::Message(m) => message(f, m),
        SheetBody::Live { pane, conn, sent, .. } => {
            let size = (term.width, term.height);
            if *sent != Some(size) && size.0 > 0 && size.1 > 0 {
                // Only a size: the sheet watches, so it never claims the
                // terminal or types into it.
                let _ = conn.resize(size.0, size.1);
                *sent = Some(size);
            }
            pane.render(f.buffer_mut(), term, truecolor);
        }
        SheetBody::Saved { bytes, pane } => {
            let size = Size::new(term.width.max(1), term.height.max(1));
            if pane.as_ref().is_none_or(|(s, _)| *s != size) {
                *pane = Some((size, TerminalPane::canned(size, bytes)));
            }
            if let Some((_, pane)) = pane {
                pane.render(f.buffer_mut(), term, truecolor);
            }
        }
    }
}

fn render_page(f: &mut Frame, page: &mut JobsPage, area: Rect, now: DateTime<Utc>) {
    let areas = list_screen::page_areas(area, DETAIL_LINES);
    list_screen::render_title(f, areas.title, TITLE, page.model.summary());
    if let Some(message) = page.model.message() {
        list_screen::render_message(f, areas.body, message);
        return;
    }
    let headings = page.headings();
    list_screen::render_sectioned_table(f, areas.body, page.table(now), &headings, page.selected, &mut page.scroll);
    if let Some(row) = page.selected_row() {
        let detail = Line::from(vec![
            Span::styled("Command  ", Style::default().fg(palette().muted)),
            Span::styled(row.command.clone(), Style::default().fg(palette().foreground)),
        ]);
        f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), areas.detail);
    }
    list_screen::render_footer(f, areas.footer, "↑/↓ select · enter open");
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_app::jobs::EMPTY_TEXT;
    use shelbi_state::awareness::{Job, JobKind, JobRecord, JobStatus};

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, h, m, 0).unwrap()
    }

    fn job(id: &str, kind: JobKind, target: &str, label: Option<&str>, started_at: DateTime<Utc>) -> Job {
        Job { id: id.into(), kind, target: target.into(), label: label.map(str::to_string), started_at, runner_pid: None, estimate: None, next_alert_at: None, started_by: None }
    }

    fn draw(page: &mut JobsPage, width: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, 16)).unwrap();
        term.draw(|f| render_full(f, page, f.area(), at(14, 10), true)).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect()
    }

    const LONG: &str = "zsh -c /Users/me/Workspaces/shelbi/scripts/bundle.sh && open target/release/bundle";

    fn page() -> JobsPage {
        let mut page = JobsPage::default();
        page.apply(running(vec![
            job("j1", JobKind::Pr, "2265", None, at(12, 58)),
            job("j2", JobKind::Command, LONG, Some("bundle desktop app"), at(14, 6)),
            job("j3", JobKind::Pid, "4242", None, at(13, 31)),
        ]));
        page
    }

    fn running(jobs: Vec<Job>) -> JobsRead {
        JobsRead::Jobs { running: jobs, finished: vec![] }
    }

    fn record(id: &str, label: &str, status: JobStatus, exit: Option<i32>, finished: DateTime<Utc>) -> JobRecord {
        let started = finished - chrono::Duration::seconds(133);
        JobRecord {
            exit_code: exit,
            ..JobRecord::finished(&job(id, JobKind::Command, "cargo test", Some(label), started), status, finished)
        }
    }

    #[test]
    fn the_table_lists_jobs_newest_first_with_its_columns() {
        let mut page = page();
        let lines = draw(&mut page, 90);
        let text = lines.join("\n");
        assert!(lines[0].contains("Jobs") && lines[0].contains("3 running"), "{text}");
        for heading in COLUMNS {
            assert!(text.contains(heading), "{heading} heading in\n{text}");
        }
        let pos = |s: &str| text.find(s).unwrap_or_else(|| panic!("{s} in\n{text}"));
        assert!(pos("bundle desktop app") < pos("pid 4242"));
        assert!(pos("pid 4242") < pos("PR #2265"));
        assert!(text.contains("watch PID 4242") && text.contains("watch PR #2265"), "{text}");
    }

    #[test]
    fn a_long_command_is_cut_in_its_cell_and_whole_in_the_detail() {
        let mut page = page();
        let lines = draw(&mut page, 90);
        let row = lines.iter().find(|l| l.contains("bundle desktop app")).unwrap();
        assert!(row.contains('…'), "the command cell is cut: {row:?}");
        assert!(!row.contains("target/release/bundle"), "{row:?}");
        // The selected (newest) job's whole command, wrapped under the table.
        let detail: String = lines
            .iter()
            .skip_while(|l| !l.trim_start().starts_with("Command  "))
            .take(3)
            .map(|l| l.trim())
            .collect();
        assert!(detail.contains("target/release/bundle"), "{lines:#?}");
        page.nav_down();
        let lines = draw(&mut page, 90);
        assert!(lines.iter().any(|l| l.trim_start().starts_with("Command  watch PID 4242")), "{lines:#?}");
    }

    #[test]
    fn no_jobs_shows_the_empty_line() {
        let mut page = JobsPage::default();
        page.apply(running(vec![]));
        let text = draw(&mut page, 100).join("\n");
        assert!(text.contains(EMPTY_TEXT), "{text}");
        assert!(!text.contains("Description"), "{text}");
    }

    #[test]
    fn the_selection_follows_its_job_across_reads() {
        let mut page = page();
        page.nav_down();
        assert_eq!(page.selected_row().unwrap().id, "j3");
        // A newer job lands on top: j3 stays selected.
        page.apply(running(vec![
            job("j1", JobKind::Pr, "2265", None, at(12, 58)),
            job("j3", JobKind::Pid, "4242", None, at(13, 31)),
            job("j4", JobKind::Command, "sleep 60", Some("x"), at(14, 9)),
        ]));
        assert_eq!(page.selected_row().unwrap().id, "j3");
        // It finishes: the selection stays in range.
        page.nav_last();
        page.apply(running(vec![job("j4", JobKind::Command, "sleep 60", Some("x"), at(14, 9))]));
        assert_eq!(page.selected_row().unwrap().id, "j4");
        // j4 finishes: it moves under Finished and stays selected.
        page.apply(JobsRead::Jobs {
            running: vec![job("j5", JobKind::Pid, "1", None, at(14, 9))],
            finished: vec![record("j4", "x", JobStatus::Ok, Some(0), at(14, 9))],
        });
        assert_eq!(page.selected_row().unwrap().id, "j4");
    }

    #[test]
    fn finished_jobs_sit_under_a_heading_with_failures_in_red() {
        let mut page = JobsPage::default();
        page.apply(JobsRead::Jobs {
            running: vec![job("j9", JobKind::Command, "sleep 60", Some("bundle"), at(14, 6))],
            finished: vec![
                record("j7", "desktop tests", JobStatus::Failed, Some(3), at(14, 0)),
                record("j8", "site build", JobStatus::Ok, Some(0), at(14, 5)),
            ],
        });
        let mut term = Terminal::new(TestBackend::new(120, 16)).unwrap();
        term.draw(|f| render_full(f, &mut page, f.area(), at(14, 10), true)).unwrap();
        let buf = term.backend().buffer().clone();
        let lines: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        let text = lines.join("\n");
        assert!(lines[0].contains("1 running · 2 finished"), "{text}");
        for heading in COLUMNS {
            assert!(text.contains(heading), "{heading} in\n{text}");
        }
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in\n{text}"));
        let heading = lines.iter().position(|l| l.trim() == FINISHED_HEADING).expect("a Finished heading row");
        assert!(row("bundle") < heading && heading < row("site build") && row("site build") < row("desktop tests"), "{text}");
        assert!(lines[row("bundle")].contains("Running"), "{text}");
        assert!(lines[row("site build")].contains("OK") && lines[row("site build")].contains("2m 13s"), "{text}");
        let failed = &lines[row("desktop tests")];
        assert!(failed.contains("Failed · exit 3"), "{text}");
        // The failed status is drawn red.
        let y = row("desktop tests") as u16;
        let x = failed.find("Failed").map(|b| failed[..b].chars().count()).unwrap() as u16;
        assert_eq!(buf[(x, y)].fg, palette().ansi_red, "{text}");
        // Selecting down walks past the heading onto the finished rows.
        page.nav_down();
        assert_eq!(page.selected_row().unwrap().id, "j8");
    }

    #[test]
    fn a_running_job_with_an_estimate_draws_a_bar_that_turns_overdue() {
        let estimated = |id: &str, label: &str, started, mins: u64| {
            let mut j = job(id, JobKind::Command, "cargo test", Some(label), started);
            j.set_estimate(std::time::Duration::from_secs(mins * 60));
            j
        };
        let mut done = record("j1", "site build", JobStatus::Ok, Some(0), at(14, 0));
        done.estimate = Some(std::time::Duration::from_secs(60));
        let mut page = JobsPage::default();
        page.apply(JobsRead::Jobs {
            // At 14:10: 5m into 10m, and 40m into 30m.
            running: vec![estimated("j3", "half way", at(14, 5), 10), estimated("j2", "running late", at(13, 30), 30)],
            finished: vec![done],
        });
        let mut term = Terminal::new(TestBackend::new(130, 16)).unwrap();
        term.draw(|f| render_full(f, &mut page, f.area(), at(14, 10), true)).unwrap();
        let buf = term.backend().buffer().clone();
        let lines: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        let text = lines.join("\n");
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in\n{text}"));
        let half = &lines[row("half way")];
        assert!(half.contains("▬▬▬▬──── 5m / 10m") && half.contains("Running"), "{text}");
        let late = &lines[row("running late")];
        assert!(late.contains("▬▬▬▬▬▬▬▬ 40m / 30m") && late.contains("Overdue"), "{text}");
        // The board cards' bar: full and in the warning color once overdue,
        // the fill cyan over a dark gray track before that.
        let x_of = |line: &str, needle: &str| line.find(needle).map(|b| line[..b].chars().count()).unwrap() as u16;
        let y = row("running late") as u16;
        assert_eq!(buf[(x_of(late, "▬"), y)].fg, palette().ansi_yellow);
        assert_eq!(buf[(x_of(late, "Overdue"), y)].fg, shelbi_app::estimate::OVERDUE_TONE.pick(palette()));
        let y = row("half way") as u16;
        assert_eq!(buf[(x_of(half, "▬"), y)].fg, palette().ansi_cyan);
        // The dark gray track, lifted to gray on this (selected) row like
        // the table's other quiet text.
        assert_eq!(buf[(x_of(half, "▬▬▬▬─") + 4, y)].fg, palette().gray, "the track past the fill");
        // Finished: actual against the estimate as text, no bar.
        let site = &lines[row("site build")];
        assert!(site.contains("2m 13s / 1m") && !site.contains('▬'), "{text}");
    }

    /// Pump `page` until its sheet has loaded.
    fn loaded(page: &mut JobsPage) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while page.sheet.as_ref().is_some_and(|s| s.loading.is_some()) {
            assert!(std::time::Instant::now() < deadline, "the sheet never loaded");
            page.pump();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    fn write_log(project: &str, id: &str, text: &str) {
        let path = shelbi_state::awareness::job_output_path(project, id).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn enter_opens_a_finished_jobs_saved_output_over_the_table_and_esc_closes_it() {
        // The live test below swaps `SHELBI_HOME`.
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = "tui-job-sheet-finished";
        write_log(project, "j4", "compiling\nbuilt ok\n");
        let mut page = JobsPage::default();
        page.apply(JobsRead::Jobs {
            running: vec![],
            finished: vec![record("j4", "desktop tests", JobStatus::Ok, Some(0), at(14, 9))],
        });
        page.open_sheet(project);
        loaded(&mut page);
        let lines = draw(&mut page, 120);
        let screen = lines.join("\n");
        // The issue detail's box: 80% of the page, centered, its title in
        // the top border.
        let rect = crate::kanban::popover_rect(Rect::new(0, 0, 120, 16));
        let top = &lines[rect.y as usize];
        assert!(top.contains("j4 · desktop tests"), "{screen}");
        let chars: Vec<char> = top.chars().collect();
        assert_eq!(chars[rect.x as usize], '┌', "{screen}");
        assert_eq!(chars[(rect.x + rect.width - 1) as usize], '┐', "{screen}");
        let bottom: Vec<char> = lines[(rect.y + rect.height - 1) as usize].chars().collect();
        assert_eq!(bottom[rect.x as usize], '└', "{screen}");
        // The table still draws around it.
        assert!(lines[0].contains("Jobs"), "{screen}");
        assert!(screen.contains("✓ OK · 2m 13s"), "{screen}");
        assert!(screen.contains("compiling") && screen.contains("built ok"), "{screen}");
        assert!(screen.contains("esc close") && !screen.contains("cancel job"), "{screen}");
        assert_eq!(page.take_cancel(), None, "a finished job can't be canceled");
        page.close_sheet();
        assert!(!draw(&mut page, 120).join("\n").contains("built ok"));
    }

    #[test]
    fn the_open_sheet_follows_the_selection_to_another_job() {
        // The live test below swaps `SHELBI_HOME`.
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = "tui-job-sheet-follow";
        write_log(project, "j4", "first job output\n");
        write_log(project, "j3", "second job output\n");
        let mut page = JobsPage::default();
        page.apply(JobsRead::Jobs {
            running: vec![],
            finished: vec![
                record("j4", "desktop tests", JobStatus::Ok, Some(0), at(14, 9)),
                record("j3", "site build", JobStatus::Ok, Some(0), at(14, 1)),
            ],
        });
        // Closed, moving selects and opens nothing.
        page.follow_selection(project);
        assert!(!page.sheet_open());
        page.open_sheet(project);
        loaded(&mut page);
        assert!(draw(&mut page, 120).join("\n").contains("first job output"));
        page.nav_down();
        page.follow_selection(project);
        loaded(&mut page);
        let screen = draw(&mut page, 120).join("\n");
        assert!(screen.contains("j3 · site build") && screen.contains("second job output"), "{screen}");
        // Its own job again keeps the sheet as it is (no reload).
        page.open_sheet(project);
        assert!(page.sheet.as_ref().is_some_and(|s| s.loading.is_none()));
    }

    #[test]
    fn a_running_job_without_a_terminal_shows_its_output_so_far_and_offers_cancel() {
        // The live test below swaps `SHELBI_HOME`.
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let project = "tui-job-sheet-running";
        write_log(project, "j8", "step 1 of 3\n");
        let mut page = JobsPage::default();
        page.apply(running(vec![job("j8", JobKind::Command, "make", Some("build"), at(14, 9))]));
        page.open_sheet(project);
        loaded(&mut page);
        let screen = draw(&mut page, 120).join("\n");
        assert!(screen.contains("● Running · 1m"), "{screen}");
        assert!(screen.contains("step 1 of 3"), "{screen}");
        assert!(screen.contains("↑/↓ job · c cancel job · esc close"), "{screen}");
        assert_eq!(page.take_cancel().as_deref(), Some("j8"));
        assert_eq!(page.take_cancel(), None, "one cancel per sheet");
        assert!(draw(&mut page, 120).join("\n").contains("Canceling…"));
    }


    #[test]
    fn enter_on_a_running_job_attaches_to_its_live_terminal() {
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let project = "tui-job-live";
        let name = shelbi_state::awareness::job_session_name(project, "j1");
        let (sock, _cleanup) = crate::shell::session::tests::spawn_cat_session(home.path(), &name);
        let mut page = JobsPage::default();
        page.apply(running(vec![job("j1", JobKind::Command, "cat", Some("live job"), at(14, 9))]));
        page.open_sheet(project);
        loaded(&mut page);
        assert!(
            matches!(page.sheet.as_ref().unwrap().body, SheetBody::Live { .. }),
            "a running job with a session attaches to it"
        );
        // Something the job prints shows up in the sheet.
        let (writer, _events) =
            shelbi_client::Connection::open(&sock, None, shelbi_proto::capability::ALL).unwrap();
        writer.input(b"live-line\n").unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            page.pump();
            let screen = draw(&mut page, 120).join("\n");
            if screen.contains("live-line") {
                assert!(screen.contains("c cancel job"), "{screen}");
                break;
            }
            assert!(std::time::Instant::now() < deadline, "the live output never showed: {screen}");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

}
