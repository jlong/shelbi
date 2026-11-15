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
//! The rows come in on the shell's core snapshot (read with the sidebar, so a
//! job appears, or moves under Finished, on the shell's refresh); the
//! durations and ages tick over because the shell redraws on that refresh
//! too.

use chrono::{DateTime, Utc};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use shelbi_app::jobs::{one_line, JobRow, JobsModel, JobsRead, COLUMNS, FINISHED_HEADING, TITLE};

use crate::table::{self, Cell, Column, Table};
use crate::theme::palette;

/// How many lines the selected row's whole command may take under the table.
const DETAIL_LINES: u16 = 3;

/// The page: the model, and which row is selected.
#[derive(Debug, Default)]
pub(crate) struct JobsPage {
    model: JobsModel,
    selected: usize,
    scroll: usize,
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
            Column::new(COLUMNS[3]).min(6).max(10).drop(2),
            Column::new(COLUMNS[4]).min(8).max(30).drop(1),
        ])
        .selected(Some(self.selected), true);
        for row in self.model.rows() {
            let mut status = Style::default().fg(row.status_tone().pick(palette()));
            if row.failed() {
                status = status.add_modifier(Modifier::BOLD);
            }
            t.push(vec![
                Cell::text(one_line(&row.description), text),
                Cell::text(one_line(&row.command), text),
                Cell::text(row.status_label(), status),
                Cell::text(row.duration(now), muted),
                Cell::text(row.finished(now), muted),
            ]);
        }
        t
    }

    /// The body line row `row` draws on: one more past the Finished heading.
    fn line_of(&self, row: usize) -> usize {
        row + usize::from(self.model.finished_start().is_some_and(|f| row >= f))
    }
}

/// Draw the page into `area` at `now`.
pub(crate) fn render_full(f: &mut Frame, page: &mut JobsPage, area: Rect, now: DateTime<Utc>) {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(2)
        .constraints([
            Constraint::Length(1),            // title
            Constraint::Length(1),            // spacer
            Constraint::Min(1),               // table
            Constraint::Length(DETAIL_LINES), // the selected row's command
            Constraint::Length(1),            // footer
        ])
        .split(area);

    let mut title = vec![Span::styled(
        TITLE,
        Style::default().fg(palette().ansi_cyan).add_modifier(Modifier::BOLD),
    )];
    if let Some(summary) = page.model.summary() {
        title.push(Span::styled(format!("   {summary}"), Style::default().fg(palette().ansi_dark_gray)));
    }
    f.render_widget(Paragraph::new(Line::from(title)), outer[0]);

    if let Some(message) = page.model.message() {
        let body = outer[2];
        let at = Rect { y: body.y + body.height.saturating_sub(1) / 3, height: 1.min(body.height), ..body };
        f.render_widget(
            Paragraph::new(Span::styled(message, Style::default().fg(palette().muted)))
                .alignment(Alignment::Center),
            at,
        );
        return;
    }

    render_table(f, page, outer[2], now);
    if let Some(row) = page.selected_row() {
        let detail = Line::from(vec![
            Span::styled("Command  ", Style::default().fg(palette().muted)),
            Span::styled(row.command.clone(), Style::default().fg(palette().foreground)),
        ]);
        f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), outer[3]);
    }
    f.render_widget(
        Paragraph::new(Span::styled("↑/↓ select", Style::default().fg(palette().ansi_dark_gray))),
        outer[4],
    );
}

fn render_table(f: &mut Frame, page: &mut JobsPage, area: Rect, now: DateTime<Utc>) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = page.table(now).lines(area.width as usize);
    // The header and its rule stay; the rows scroll under them to keep the
    // selection in view. The Finished heading is a body line of its own.
    let (head, rows) = lines.split_at(lines.len().min(2));
    let mut body: Vec<Line<'static>> = rows.iter().map(|l| l.line.clone()).collect();
    if let Some(at) = page.model.finished_start().filter(|&at| at <= body.len()) {
        let heading = Line::from(Span::styled(
            format!("{}{FINISHED_HEADING}", " ".repeat(table::BLEED)),
            Style::default().fg(palette().muted).add_modifier(Modifier::BOLD),
        ));
        body.insert(at, heading);
    }
    let rows_h = (area.height as usize).saturating_sub(head.len());
    let selected = page.line_of(page.selected);
    // Keep the heading in view above the first finished row.
    let top = if page.model.finished_start() == Some(page.selected) { selected - 1 } else { selected };
    if top < page.scroll {
        page.scroll = top;
    } else if rows_h > 0 && selected >= page.scroll + rows_h {
        page.scroll = selected + 1 - rows_h;
    }
    page.scroll = page.scroll.min(body.len().saturating_sub(rows_h));
    // A table line reaches into the page's gutter.
    let bleed = table::BLEED as u16;
    let wide = |y: u16| Rect {
        x: area.x.saturating_sub(bleed),
        y,
        width: area.width + 2 * bleed,
        height: 1,
    };
    let drawn = head.iter().map(|l| l.line.clone()).chain(body.into_iter().skip(page.scroll).take(rows_h));
    for (i, line) in drawn.enumerate() {
        f.render_widget(Paragraph::new(line), wide(area.y + i as u16));
    }
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
        Job { id: id.into(), kind, target: target.into(), label: label.map(str::to_string), started_at, runner_pid: None }
    }

    fn draw(page: &mut JobsPage, width: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, 16)).unwrap();
        term.draw(|f| render_full(f, page, f.area(), at(14, 10))).unwrap();
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
        term.draw(|f| render_full(f, &mut page, f.area(), at(14, 10))).unwrap();
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
}
