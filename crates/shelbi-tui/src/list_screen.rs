//! The pieces the palette-only list screens share (Jobs, Reminders): the
//! title row with its muted count, the centered line in place of an empty
//! list, a [`Table`] whose rows sit under section headings and scroll to keep
//! the selection in view, the keys footer, and the sheet that opens over the
//! table, drawn the way the board's issue detail is (the same centered box,
//! cleared under it, in the dialogs' frame: [`crate::kanban::popover_rect`]):
//! the record's `id · title` in the border, a colored status line, a rule,
//! the body, the keys.
//!
//! ```text
//!   Jobs   1 running · 2 finished
//!    Desc┌ j4 · desktop tests ──────────────────────┐
//!   ─────│ ✓ OK · 2m 13s                            │
//!  ▐ bund│──────────────────────────────────────────│
//!   Fini│ <body>                                    │
//!    desk│                                          │
//!        │ ↑/↓ job · esc close                      │
//!        └──────────────────────────────────────────┘
//! ```

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::table::{self, Table};
use crate::theme::palette;

/// A list page's rows: its title, the list, the selected row's whole text
/// under it (`detail_lines` tall), and the keys.
pub(crate) struct PageAreas {
    pub(crate) title: Rect,
    pub(crate) body: Rect,
    pub(crate) detail: Rect,
    pub(crate) footer: Rect,
}

pub(crate) fn page_areas(area: Rect, detail_lines: u16) -> PageAreas {
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .horizontal_margin(2)
        .constraints([
            Constraint::Length(1),            // title
            Constraint::Length(1),            // spacer
            Constraint::Min(1),               // the list
            Constraint::Length(detail_lines), // the selected row's whole text
            Constraint::Length(1),            // footer
        ])
        .split(area);
    PageAreas { title: outer[0], body: outer[2], detail: outer[3], footer: outer[4] }
}

/// The title row: the screen's name, loud, then its count, muted.
pub(crate) fn render_title(f: &mut Frame, area: Rect, title: &str, summary: Option<String>) {
    let mut spans = vec![Span::styled(
        title.to_string(),
        Style::default().fg(palette().ansi_cyan).add_modifier(Modifier::BOLD),
    )];
    if let Some(summary) = summary {
        spans.push(Span::styled(format!("   {summary}"), Style::default().fg(palette().ansi_dark_gray)));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// A line in place of the list (loading, empty, unavailable), centered a
/// third of the way down `body`.
pub(crate) fn render_message(f: &mut Frame, body: Rect, message: String) {
    let at = Rect { y: body.y + body.height.saturating_sub(1) / 3, height: 1.min(body.height), ..body };
    f.render_widget(
        Paragraph::new(Span::styled(message, Style::default().fg(palette().muted))).alignment(Alignment::Center),
        at,
    );
}

/// The keys line, quiet.
pub(crate) fn render_footer(f: &mut Frame, area: Rect, keys: &str) {
    f.render_widget(
        Paragraph::new(Span::styled(keys.to_string(), Style::default().fg(palette().ansi_dark_gray))),
        area,
    );
}

/// The body line row `row` draws on: one more for each heading at or
/// above it.
pub(crate) fn line_of(row: usize, headings: &[(usize, &str)]) -> usize {
    row + headings.iter().filter(|(at, _)| *at <= row).count()
}

/// Draw `table` into `area`: its header and rule stay put, the rows scroll
/// under them (`scroll` is the first body line drawn) to keep row
/// `selected` in view, and each `(row, text)` heading is a body line of its
/// own above that row, kept in view with the row under it.
pub(crate) fn render_sectioned_table(
    f: &mut Frame,
    area: Rect,
    table: Table,
    headings: &[(usize, &str)],
    selected: usize,
    scroll: &mut usize,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = table.lines(area.width as usize);
    let (head, rows) = lines.split_at(lines.len().min(2));
    let mut body: Vec<Line<'static>> = rows.iter().map(|l| l.line.clone()).collect();
    // From the bottom up, so each insert leaves the earlier indices alone.
    for (at, text) in headings.iter().rev() {
        if *at <= body.len() {
            let heading = Line::from(Span::styled(
                format!("{}{text}", " ".repeat(table::BLEED)),
                Style::default().fg(palette().muted).add_modifier(Modifier::BOLD),
            ));
            body.insert(*at, heading);
        }
    }
    let rows_h = (area.height as usize).saturating_sub(head.len());
    let line = line_of(selected, headings);
    // Keep a heading in view above the row it heads.
    let top = if headings.iter().any(|(at, _)| *at == selected) { line - 1 } else { line };
    if top < *scroll {
        *scroll = top;
    } else if rows_h > 0 && line >= *scroll + rows_h {
        *scroll = line + 1 - rows_h;
    }
    *scroll = (*scroll).min(body.len().saturating_sub(rows_h));
    // A table line reaches into the page's gutter.
    let bleed = table::BLEED as u16;
    let wide = |y: u16| Rect { x: area.x.saturating_sub(bleed), y, width: area.width + 2 * bleed, height: 1 };
    let drawn = head.iter().map(|l| l.line.clone()).chain(body.into_iter().skip(*scroll).take(rows_h));
    for (i, line) in drawn.enumerate() {
        f.render_widget(Paragraph::new(line), wide(area.y + i as u16));
    }
}

/// The sheet over the page in `area`: the issue detail's box, cleared
/// under it, in the dialogs' frame with `title` in its border; then
/// `status` in `status_style`, a rule, the body, and `keys` at the bottom.
/// Returns the body's area for the caller to fill; `None` when `area` is too
/// small to hold a sheet.
pub(crate) fn render_sheet_frame(
    f: &mut Frame,
    area: Rect,
    title: &str,
    status: String,
    status_style: Style,
    keys: &str,
) -> Option<Rect> {
    let rect = crate::kanban::popover_rect(area);
    if rect.width < 6 || rect.height < 6 {
        return None;
    }
    // Cleared under it, so the table doesn't bleed through.
    f.render_widget(Clear, rect);
    let title = crate::kanban::truncate(title, rect.width.saturating_sub(4) as usize);
    let inner = crate::overlay::dialog_frame(f.buffer_mut(), rect, None, Some(&title));
    let parts = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // status
            Constraint::Length(1), // separator
            Constraint::Min(1),    // body
            Constraint::Length(1), // keys
        ])
        .split(inner);
    f.render_widget(Paragraph::new(Span::styled(status, status_style)), parts[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(parts[1].width as usize),
            Style::default().fg(palette().ansi_dark_gray),
        )),
        parts[1],
    );
    render_footer(f, parts[3], keys);
    Some(parts[2])
}

/// The sheet's keys: ↑/↓ to the next `noun` (the sheet follows the
/// selection), `c <action>` while `action` applies, and `esc close`.
pub(crate) fn sheet_keys(noun: &str, action: Option<&str>) -> String {
    match action {
        Some(action) => format!("↑/↓ {noun} · c {} · esc close", action.to_lowercase()),
        None => format!("↑/↓ {noun} · esc close"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn headings_push_the_rows_under_them_down() {
        let headings = [(0, "Pending"), (2, "Fired")];
        assert_eq!(line_of(0, &headings), 1);
        assert_eq!(line_of(1, &headings), 2);
        assert_eq!(line_of(2, &headings), 4);
        assert_eq!(line_of(3, &[]), 3);
    }

    #[test]
    fn sheet_keys_offer_the_action_only_while_it_applies() {
        assert_eq!(sheet_keys("job", Some("Cancel job")), "↑/↓ job · c cancel job · esc close");
        assert_eq!(sheet_keys("reminder", None), "↑/↓ reminder · esc close");
    }
}
