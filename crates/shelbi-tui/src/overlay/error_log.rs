//! The persistent error-log viewer — shared overlay logic + rendering.
//!
//! Lists the project's recorded errors newest-first, marks the unread ones with
//! a `●`, and (by the caller's contract) marks the whole log read on open so the
//! sidebar's unread button clears once the viewer closes. `↑`/`↓` (or `k`/`j`,
//! or the scroll wheel) scroll, `c` clears the log, and `Esc`/`q` close.
//!
//! One implementation, two callers: the in-process TUI overlay (removing-tmux
//! Phase 4d) and the legacy tmux `shelbi __error-log` popup. The reading /
//! marking / clearing of the on-disk log stays in [`shelbi_state`]; this module
//! owns only the scroll state, the line layout, and the render. [`handle_key`]
//! returns an [`ErrorLogOutcome`] so the caller performs the IO (clearing the
//! log) and closes its own loop.
//!
//! [`handle_key`]: Viewer::handle_key

use chrono::{DateTime, Local};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use shelbi_state::ErrorLogEntry;

/// Column where an entry's message starts (and where its wrapped continuation
/// lines align): `● ` marker (2) + `YYYY-MM-DD HH:MM` (16) + 2 spaces.
pub const MESSAGE_INDENT: usize = 20;

/// What a key did to the viewer. `Continue` keeps the overlay open; `Close`
/// dismisses it; `Clear` asks the caller to wipe the on-disk log (the caller
/// then calls [`shelbi_state::clear_errors`] and [`Viewer::clear`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorLogOutcome {
    Continue,
    Close,
    Clear,
}

/// The viewer's mutable state: the entries (newest-first) with their unread
/// flag, plus the current scroll offset in rendered lines. Kept separate from
/// any event loop so the scroll/clear logic is unit-testable without a terminal.
pub struct Viewer {
    /// `(entry, was_unread_when_opened)`, newest first.
    pub rows: Vec<(ErrorLogEntry, bool)>,
    /// First rendered line shown at the top of the viewport.
    scroll: usize,
}

impl Viewer {
    pub fn new(rows: Vec<(ErrorLogEntry, bool)>) -> Self {
        Self { rows, scroll: 0 }
    }

    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// Clamp the scroll offset so it never scrolls past the last line. `total`
    /// is the rendered line count, `visible` the viewport height.
    pub fn clamp(&mut self, total: usize, visible: usize) {
        let max = total.saturating_sub(visible);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_add(n);
    }

    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    /// Empty the live view after the on-disk log is cleared.
    pub fn clear(&mut self) {
        self.rows.clear();
        self.scroll = 0;
    }

    /// Handle one key press. Pure except for the viewer's own scroll state; the
    /// caller performs the clear IO when this returns [`ErrorLogOutcome::Clear`].
    pub fn handle_key(&mut self, k: KeyEvent) -> ErrorLogOutcome {
        match k.code {
            KeyCode::Esc | KeyCode::Char('q') => return ErrorLogOutcome::Close,
            KeyCode::Up | KeyCode::Char('k') => self.scroll_up(1),
            KeyCode::Down | KeyCode::Char('j') => self.scroll_down(1),
            KeyCode::PageUp => self.scroll_up(10),
            KeyCode::PageDown => self.scroll_down(10),
            KeyCode::Home => self.scroll_top(),
            KeyCode::Char('c') => return ErrorLogOutcome::Clear,
            _ => {}
        }
        ErrorLogOutcome::Continue
    }
}

/// Format an entry's stored RFC3339 timestamp as local `YYYY-MM-DD HH:MM` for
/// display, falling back to the raw string when it doesn't parse.
fn format_ts(entry: &ErrorLogEntry) -> String {
    match DateTime::parse_from_rfc3339(&entry.ts) {
        Ok(dt) => dt
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string(),
        Err(_) => entry.ts.clone(),
    }
}

/// Word-wrap `text` to `width` columns, hard-splitting any single word longer
/// than the width so nothing overflows. Always returns at least one (possibly
/// empty) line.
fn wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if word.chars().count() > width {
            if !current.is_empty() {
                lines.push(std::mem::take(&mut current));
            }
            let mut chunk = String::new();
            for ch in word.chars() {
                if chunk.chars().count() == width {
                    lines.push(std::mem::take(&mut chunk));
                }
                chunk.push(ch);
            }
            current = chunk;
            continue;
        }
        let sep = usize::from(!current.is_empty());
        if current.chars().count() + sep + word.chars().count() > width {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        } else {
            if sep == 1 {
                current.push(' ');
            }
            current.push_str(word);
        }
    }
    lines.push(current);
    lines
}

/// Build the full list of rendered lines for the log, newest first. Each entry
/// is a header line (`● YYYY-MM-DD HH:MM  <message start>`) plus any wrapped
/// continuation lines indented under the message column. Pure so the layout is
/// unit-testable. `width` is the inner (bordered) content width.
pub fn build_lines(rows: &[(ErrorLogEntry, bool)], width: usize) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::from(Span::styled(
            "No errors logged.",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    let msg_width = width.saturating_sub(MESSAGE_INDENT).max(1);
    let mut out: Vec<Line> = Vec::new();
    for (entry, unread) in rows {
        let marker = if *unread { "● " } else { "  " };
        let marker_style = if *unread {
            Style::default()
                .fg(Color::Rgb(210, 120, 120))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let ts = format_ts(entry);
        let msg_lines = wrap(&entry.message, msg_width);
        for (i, chunk) in msg_lines.iter().enumerate() {
            if i == 0 {
                out.push(Line::from(vec![
                    Span::styled(marker.to_string(), marker_style),
                    Span::styled(format!("{ts}  "), Style::default().fg(Color::DarkGray)),
                    Span::styled(chunk.clone(), Style::default().fg(Color::White)),
                ]));
            } else {
                out.push(Line::from(vec![
                    Span::raw(" ".repeat(MESSAGE_INDENT)),
                    Span::styled(chunk.clone(), Style::default().fg(Color::White)),
                ]));
            }
        }
    }
    out
}

/// Paint the error-log modal into `area` (the whole popup pane in the tmux
/// runtime, a centered overlay rect in the single-process TUI).
pub fn render(f: &mut Frame, area: Rect, viewer: &mut Viewer) {
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow))
        .title(" Error log ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // body (grows) + a one-line hint at the bottom.
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let body = rows[0];
    let visible = body.height as usize;

    let lines = build_lines(&viewer.rows, body.width as usize);
    viewer.clamp(lines.len(), visible);

    f.render_widget(
        Paragraph::new(lines).scroll((viewer.scroll as u16, 0)),
        body,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "↑/↓ scroll · c clear · esc close",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn entry(ts: &str, message: &str) -> ErrorLogEntry {
        ErrorLogEntry {
            ts: ts.to_string(),
            message: message.to_string(),
            source: None,
        }
    }

    fn flatten(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn empty_log_shows_the_empty_state() {
        let lines = build_lines(&[], 60);
        assert_eq!(flatten(&lines), vec!["No errors logged.".to_string()]);
    }

    #[test]
    fn unread_entries_are_marked_and_read_ones_are_not() {
        let rows = vec![
            (entry("2026-09-26T14:02:00.000000000Z", "new failed: boom"), true),
            (entry("2026-09-25T17:40:00.000000000Z", "old failed: bang"), false),
        ];
        let text = flatten(&build_lines(&rows, 60));
        assert!(text[0].starts_with("● "), "unread carries ●: {:?}", text[0]);
        assert!(text[0].contains("new failed: boom"));
        assert!(
            text[1].starts_with("  ") && !text[1].starts_with("● "),
            "read entry has no ●: {:?}",
            text[1]
        );
        assert!(text[1].contains("old failed: bang"));
    }

    #[test]
    fn long_messages_wrap_and_continuation_lines_indent() {
        let long = "this is a fairly long error message that certainly exceeds the narrow width";
        let rows = vec![(entry("2026-09-26T14:02:00.000000000Z", long), true)];
        let lines = build_lines(&rows, 40);
        assert!(lines.len() > 1, "a long message wraps to multiple lines");
        let text = flatten(&lines);
        assert!(
            text[1].starts_with(&" ".repeat(MESSAGE_INDENT)),
            "continuation line indents to the message column: {:?}",
            text[1]
        );
    }

    #[test]
    fn wrap_hard_splits_a_word_longer_than_the_width() {
        let out = wrap("abcdefghij", 4);
        assert_eq!(out, vec!["abcd", "efgh", "ij"]);
    }

    #[test]
    fn scroll_clamps_to_the_last_page() {
        let mut v = Viewer::new(Vec::new());
        v.scroll_down(100);
        v.clamp(30, 10);
        assert_eq!(v.scroll(), 20, "clamped to total - visible");
        v.clamp(5, 10);
        assert_eq!(v.scroll(), 0, "everything fits: no scroll");
    }

    #[test]
    fn keys_scroll_close_and_request_clear() {
        let mut v = Viewer::new(vec![(entry("2026-09-26T14:02:00Z", "boom"), true)]);
        assert_eq!(v.handle_key(key(KeyCode::Down)), ErrorLogOutcome::Continue);
        assert_eq!(v.scroll(), 1);
        assert_eq!(v.handle_key(key(KeyCode::Up)), ErrorLogOutcome::Continue);
        assert_eq!(v.scroll(), 0);
        assert_eq!(v.handle_key(key(KeyCode::Char('c'))), ErrorLogOutcome::Clear);
        // The viewer does not clear itself; the caller does after the IO.
        assert_eq!(v.rows.len(), 1);
        v.clear();
        assert!(v.rows.is_empty());
        assert_eq!(v.handle_key(key(KeyCode::Esc)), ErrorLogOutcome::Close);
        assert_eq!(v.handle_key(key(KeyCode::Char('q'))), ErrorLogOutcome::Close);
    }
}
