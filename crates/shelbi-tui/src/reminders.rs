//! The Reminders screen (`View::Reminders`, palette-only and found by typing:
//! the palette's default list leaves it out): the project's `shelbi remind`
//! reminders as a table, the pending ones soonest first under `Pending`,
//! then the recently fired ones newest first under `Fired`, drawn from the
//! shared [`shelbi_app::reminders`] model the desktop's Reminders view draws
//! too, with the Jobs screen's pieces ([`crate::list_screen`]).
//!
//! ```text
//!   Reminders   3 pending · 2 fired
//!
//!    Id  │ When          │ Reminder                 │ Repeats
//!   ─────┼───────────────┼──────────────────────────┼─────────
//!   Pending
//!  ▐ r12 │ in 14m        │ check PR #2311 CI        │          ▌
//!    r9  │ in 2h         │ ask about devbox relay   │ every 1d
//!   Fired
//!    r8  │ fired 20m ago │ check deploy             │
//!
//!   Reminder  check PR #2311 CI                <- the selected row's, whole
//!   ↑/↓ select · enter open
//! ```
//!
//! Enter opens the selected reminder's sheet over the table, drawn the way
//! the Jobs screen's is (the issue detail's box, [`list_screen::render_sheet_frame`]):
//! its `id · text` in the border, its status, its whole text, when it is due
//! (absolute and relative), its repeat, when it was created. ↑/↓ move to the
//! next reminder and the sheet follows; `c` cancels a pending one (`shelbi
//! remind cancel`); Esc closes the sheet.
//!
//! The rows come in on the shell's core snapshot, like the Jobs screen's, so
//! a reminder appears, fires, or goes on the shell's refresh, and the
//! relative times tick over because the shell redraws on it.

use chrono::{DateTime, Utc};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::Frame;

use shelbi_app::jobs::one_line;
use shelbi_app::reminders::{ReminderRow, RemindersModel, RemindersRead, SheetState, CANCEL_LABEL, COLUMNS, TITLE};

use crate::list_screen;
use crate::table::{Cell, Column, Table};
use crate::theme::palette;

/// How many lines the selected row's whole text may take under the table.
const DETAIL_LINES: u16 = 3;

/// The page: the model, which row is selected, and the open sheet.
#[derive(Default)]
pub(crate) struct RemindersPage {
    model: RemindersModel,
    selected: usize,
    scroll: usize,
    /// The open side sheet (the shared [`SheetState`]).
    sheet: Option<SheetState>,
}

impl RemindersPage {
    /// Fold in a read. The selection and the sheet follow their reminder
    /// (into the fired list when it fires); a sheet whose reminder was
    /// canceled closes.
    pub(crate) fn apply(&mut self, read: RemindersRead) {
        let model = RemindersModel::from_read(read);
        if model == self.model {
            return;
        }
        let kept = self.selected_row().and_then(|r| model.follow(r)).map(|r| r.key.clone());
        if self.sheet.as_mut().is_some_and(|sheet| !sheet.follow(&model)) {
            self.sheet = None;
        }
        self.model = model;
        if let Some(at) = kept.and_then(|key| self.model.rows().iter().position(|r| r.key == key)) {
            self.selected = at;
        }
        self.clamp();
    }

    pub(crate) fn selected_row(&self) -> Option<&ReminderRow> {
        self.model.rows().get(self.selected)
    }

    /// Open the selected reminder's sheet (replacing an open one; its own
    /// reminder again keeps the sheet as it is).
    pub(crate) fn open_sheet(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if self.sheet.as_ref().is_some_and(|s| s.row.key == row.key) {
            return;
        }
        self.sheet = Some(SheetState::new(row));
    }

    /// With the sheet open, show the selected reminder in it (the selection
    /// moved under it).
    pub(crate) fn follow_selection(&mut self) {
        if self.sheet.is_some() {
            self.open_sheet();
        }
    }

    pub(crate) fn close_sheet(&mut self) {
        self.sheet = None;
    }

    pub(crate) fn sheet_open(&self) -> bool {
        self.sheet.is_some()
    }

    /// The open sheet's reminder, while it is pending: what `c` cancels.
    /// Marks the sheet as canceling.
    pub(crate) fn take_cancel(&mut self) -> Option<String> {
        self.sheet.as_mut()?.begin_cancel()
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
            Column::new(COLUMNS[0]).min(3).max(6),
            Column::new(COLUMNS[1]).min(8).max(16),
            Column::new(COLUMNS[2]).min(12).max(400).fill(),
            Column::new(COLUMNS[3]).min(8).max(16).drop(1),
        ])
        .selected(Some(self.selected), true);
        for row in self.model.rows() {
            let when = row.when_tone().map_or(muted, |t| Style::default().fg(t.pick(palette())));
            t.push(vec![
                Cell::text(row.id.clone(), muted),
                Cell::text(row.when(now), when),
                Cell::text(one_line(&row.text), text),
                Cell::text(row.repeats(), muted),
            ]);
        }
        t
    }
}

/// Draw the page into `area` at `now`: the table at its full size, and the
/// open sheet over it.
pub(crate) fn render_full(f: &mut Frame, page: &mut RemindersPage, area: Rect, now: DateTime<Utc>) {
    render_page(f, page, area, now);
    if page.sheet.is_some() {
        render_sheet(f, page, area, now);
    }
}

fn render_page(f: &mut Frame, page: &mut RemindersPage, area: Rect, now: DateTime<Utc>) {
    let areas = list_screen::page_areas(area, DETAIL_LINES);
    list_screen::render_title(f, areas.title, TITLE, page.model.summary());
    if let Some(message) = page.model.message() {
        list_screen::render_message(f, areas.body, message);
        return;
    }
    let headings = page.model.headings();
    list_screen::render_sectioned_table(f, areas.body, page.table(now), &headings, page.selected, &mut page.scroll);
    if let Some(row) = page.selected_row() {
        let detail = Line::from(vec![
            Span::styled("Reminder  ", Style::default().fg(palette().muted)),
            Span::styled(row.text.clone(), Style::default().fg(palette().foreground)),
        ]);
        f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }), areas.detail);
    }
    list_screen::render_footer(f, areas.footer, "↑/↓ select · enter open");
}

/// The sheet over `area`: the shared frame (the reminder's `id · text` in
/// its border, its status, the keys) around its whole text and its facts.
fn render_sheet(f: &mut Frame, page: &RemindersPage, area: Rect, now: DateTime<Utc>) {
    let Some(sheet) = page.sheet.as_ref() else {
        return;
    };
    let row = &sheet.row;
    let keys = list_screen::sheet_keys("reminder", sheet.offers_cancel().then_some(CANCEL_LABEL));
    let Some(body) = list_screen::render_sheet_frame(
        f,
        area,
        &format!("{} · {}", row.id, one_line(&row.text)),
        sheet.status(now),
        Style::default().fg(row.status_tone().pick(palette())),
        &keys,
    ) else {
        return;
    };
    let muted = Style::default().fg(palette().muted);
    let text = Style::default().fg(palette().foreground);
    let mut lines: Vec<Line<'static>> = row.text.lines().map(|l| Line::from(Span::styled(l.to_string(), text))).collect();
    lines.push(Line::default());
    let details = row.details(now);
    let label_w = details.iter().map(|(l, _)| l.chars().count()).max().unwrap_or(0) + 2;
    for (label, value) in details {
        lines.push(Line::from(vec![
            Span::styled(format!("{label:<label_w$}"), muted.add_modifier(Modifier::BOLD)),
            Span::styled(value, text),
        ]));
    }
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), body);
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use ratatui::{backend::TestBackend, Terminal};
    use shelbi_app::reminders::{EMPTY_TEXT, FIRED_HEADING, PENDING_HEADING};
    use shelbi_state::awareness::{FiredReminder, Reminder};

    fn at(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 14, h, m, 0).unwrap()
    }

    fn reminder(id: &str, text: &str, due_at: DateTime<Utc>, every: Option<u64>) -> Reminder {
        Reminder {
            id: id.into(),
            due_at,
            text: text.into(),
            created_at: at(9, 0),
            every: every.map(std::time::Duration::from_secs),
        }
    }

    fn draw(page: &mut RemindersPage, width: u16) -> Vec<String> {
        let mut term = Terminal::new(TestBackend::new(width, 18)).unwrap();
        term.draw(|f| render_full(f, page, f.area(), at(14, 10))).unwrap();
        let buf = term.backend().buffer();
        (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>())
            .collect()
    }

    fn read() -> RemindersRead {
        RemindersRead::Reminders {
            pending: vec![
                reminder("r9", "ask about devbox relay", at(16, 10), Some(86_400)),
                reminder("r12", "check PR #2311 CI", at(14, 24), None),
            ],
            fired: vec![FiredReminder::fired(&reminder("r8", "check deploy", at(13, 50), None), at(13, 50))],
        }
    }

    #[test]
    fn pending_then_fired_under_their_headings() {
        let mut page = RemindersPage::default();
        page.apply(read());
        let lines = draw(&mut page, 100);
        let text = lines.join("\n");
        assert!(lines[0].contains("Reminders") && lines[0].contains("2 pending · 1 fired"), "{text}");
        let row = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("{needle} in\n{text}"));
        let pending = lines.iter().position(|l| l.trim() == PENDING_HEADING).expect("a Pending heading");
        let fired = lines.iter().position(|l| l.trim() == FIRED_HEADING).expect("a Fired heading");
        assert!(pending < row("check PR #2311 CI"), "{text}");
        assert!(row("check PR #2311 CI") < row("ask about devbox relay"), "soonest first: {text}");
        assert!(row("ask about devbox relay") < fired && fired < row("check deploy"), "{text}");
        assert!(lines[row("check PR #2311 CI")].contains("in 14m"), "{text}");
        assert!(lines[row("ask about devbox relay")].contains("every 1d"), "{text}");
        assert!(lines[row("check deploy")].contains("fired 20m ago"), "{text}");
        // The selected row's whole text under the table.
        assert!(lines.iter().any(|l| l.trim_start().starts_with("Reminder  check PR #2311 CI")), "{text}");
        // Selecting down walks past the Fired heading onto the fired row.
        page.nav_down();
        page.nav_down();
        assert_eq!(page.selected_row().unwrap().id, "r8");
    }

    #[test]
    fn no_reminders_shows_the_empty_line() {
        let mut page = RemindersPage::default();
        page.apply(RemindersRead::Reminders { pending: vec![], fired: vec![] });
        let text = draw(&mut page, 100).join("\n");
        assert!(text.contains(EMPTY_TEXT), "{text}");
        assert!(!text.contains("Repeats"), "{text}");
    }

    #[test]
    fn enter_opens_the_details_and_c_cancels_a_pending_reminder() {
        let mut page = RemindersPage::default();
        page.apply(read());
        page.nav_down(); // r9, the recurring one
        page.open_sheet();
        let screen = draw(&mut page, 140).join("\n");
        assert!(screen.contains("┌ r9 · ask about devbox relay"), "the id and text in the border: {screen}");
        assert!(screen.contains("● Pending · in 2h"), "{screen}");
        assert!(screen.contains("Due") && screen.contains("Repeats") && screen.contains("Every 1d"), "{screen}");
        assert!(screen.contains("Created"), "{screen}");
        assert!(screen.contains("↑/↓ reminder · c cancel reminder · esc close"), "{screen}");
        assert_eq!(page.take_cancel().as_deref(), Some("r9"));
        assert_eq!(page.take_cancel(), None, "one cancel per sheet");
        assert!(draw(&mut page, 140).join("\n").contains("Canceling…"));
        // The next read no longer lists it: the sheet closes.
        page.apply(RemindersRead::Reminders {
            pending: vec![reminder("r12", "check PR #2311 CI", at(14, 24), None)],
            fired: vec![],
        });
        assert!(!page.sheet_open());
    }

    #[test]
    fn a_fired_reminder_has_no_cancel_and_esc_closes_its_sheet() {
        let mut page = RemindersPage::default();
        page.apply(read());
        page.nav_last();
        page.open_sheet();
        let screen = draw(&mut page, 140).join("\n");
        assert!(screen.contains("✓ Fired · 20m ago"), "{screen}");
        assert!(screen.contains("Fired") && screen.contains("Doesn't repeat"), "{screen}");
        assert!(screen.contains("esc close") && !screen.contains("cancel reminder"), "{screen}");
        assert_eq!(page.take_cancel(), None);
        page.close_sheet();
        assert!(!draw(&mut page, 140).join("\n").contains("Doesn't repeat"));
    }

    #[test]
    fn the_selection_and_sheet_follow_a_reminder_as_it_fires() {
        let mut page = RemindersPage::default();
        page.apply(read());
        assert_eq!(page.selected_row().unwrap().id, "r12");
        page.open_sheet();
        // r12 comes due and fires; a new one lands on top.
        let fired_r12 = FiredReminder::fired(&reminder("r12", "check PR #2311 CI", at(14, 24), None), at(14, 24));
        page.apply(RemindersRead::Reminders {
            pending: vec![
                reminder("r15", "new", at(14, 30), None),
                reminder("r9", "ask about devbox relay", at(16, 10), Some(86_400)),
            ],
            fired: vec![fired_r12],
        });
        let selected = page.selected_row().unwrap();
        assert_eq!((selected.id.as_str(), selected.is_pending()), ("r12", false));
        assert!(page.sheet_open());
        let screen = draw(&mut page, 140).join("\n");
        assert!(screen.contains("✓ Fired"), "the sheet shows it fired: {screen}");
    }

    #[test]
    fn the_sheet_follows_the_selection_and_keeps_its_own_reminder() {
        let mut page = RemindersPage::default();
        page.apply(read());
        page.open_sheet();
        page.take_cancel();
        // Its own reminder again: the sheet stays as it is (still canceling).
        page.follow_selection();
        assert!(draw(&mut page, 140).join("\n").contains("Canceling…"));
        page.nav_down();
        page.follow_selection();
        let screen = draw(&mut page, 140).join("\n");
        assert!(screen.contains("r9 · ask about devbox relay") && !screen.contains("Canceling…"), "{screen}");
        // A closed sheet stays closed as the selection moves.
        page.close_sheet();
        page.nav_down();
        page.follow_selection();
        assert!(!page.sheet_open());
    }
}
