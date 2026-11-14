//! The Jobs screen (`View::Jobs`, palette-only): the project's active
//! awareness jobs as a plain three-column table, drawn from the shared
//! [`shelbi_app::jobs`] model the desktop's Jobs view draws too.
//!
//! ```text
//!   Jobs   3 active
//!
//!    Description          │ Command                  │ Started
//!   ──────────────────────┼──────────────────────────┼───────────────────
//!  ▐ bundle desktop app   │ zsh -c /Users/me/bund…   │ 2:06 PM · 4m ago ▌
//!    PR #2265             │ watch PR #2265           │ 12:58 PM · 1h ago
//!
//!   Command  zsh -c /Users/me/bundle.sh && open out      <- the selected row's, whole
//!   ↑/↓ select
//! ```
//!
//! The rows come in on the shell's core snapshot (read with the sidebar, so a
//! job appears or goes on the shell's refresh); the ages tick over because
//! the shell redraws on that refresh too.

use chrono::{DateTime, Utc};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use shelbi_app::jobs::{one_line, JobRow, JobsModel, JobsRead, COLUMNS, TITLE};

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
            Column::new(COLUMNS[2]).min(8).max(30).drop(1),
        ])
        .selected(Some(self.selected), true);
        for row in self.model.rows() {
            t.push(vec![
                Cell::text(one_line(&row.description), text),
                Cell::text(one_line(&row.command), text),
                Cell::text(row.started(now), muted),
            ]);
        }
        t
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

    let count = page.model.rows().len();
    let mut title = vec![Span::styled(
        TITLE,
        Style::default().fg(palette().ansi_cyan).add_modifier(Modifier::BOLD),
    )];
    if count > 0 {
        title.push(Span::styled(format!("   {count} active"), Style::default().fg(palette().ansi_dark_gray)));
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
    // selection in view.
    let (head, body) = lines.split_at(lines.len().min(2));
    let rows_h = (area.height as usize).saturating_sub(head.len());
    if page.selected < page.scroll {
        page.scroll = page.selected;
    } else if rows_h > 0 && page.selected >= page.scroll + rows_h {
        page.scroll = page.selected + 1 - rows_h;
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
    let drawn = head.iter().chain(body.iter().skip(page.scroll).take(rows_h));
    for (i, line) in drawn.enumerate() {
        f.render_widget(Paragraph::new(line.line.clone()), wide(area.y + i as u16));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_app::jobs::EMPTY_TEXT;
    use shelbi_state::awareness::{Job, JobKind};

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
        page.apply(JobsRead::Jobs(vec![
            job("j1", JobKind::Pr, "2265", None, at(12, 58)),
            job("j2", JobKind::Command, LONG, Some("bundle desktop app"), at(14, 6)),
            job("j3", JobKind::Pid, "4242", None, at(13, 31)),
        ]));
        page
    }

    #[test]
    fn the_table_lists_jobs_newest_first_with_three_columns() {
        let mut page = page();
        let lines = draw(&mut page, 90);
        let text = lines.join("\n");
        assert!(lines[0].contains("Jobs") && lines[0].contains("3 active"), "{text}");
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
        page.apply(JobsRead::Jobs(vec![]));
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
        page.apply(JobsRead::Jobs(vec![
            job("j1", JobKind::Pr, "2265", None, at(12, 58)),
            job("j3", JobKind::Pid, "4242", None, at(13, 31)),
            job("j4", JobKind::Command, "sleep 60", Some("x"), at(14, 9)),
        ]));
        assert_eq!(page.selected_row().unwrap().id, "j3");
        // It finishes: the selection stays in range.
        page.nav_last();
        page.apply(JobsRead::Jobs(vec![job("j4", JobKind::Command, "sleep 60", Some("x"), at(14, 9))]));
        assert_eq!(page.selected_row().unwrap().id, "j4");
    }
}
