//! `shelbi __error-log PROJECT` — the persistent error-log viewer, meant to run
//! inside a `tmux display-popup` launched from the sidebar's unread-errors
//! button (or the palette's "Open error log" entry, which runs it inline in the
//! palette's own popup pane).
//!
//! It lists the project's recorded errors newest-first, marks the unread ones
//! with a `●`, and marks the whole log read on open so the button clears once
//! the popup closes. `↑`/`↓` (or `k`/`j`, or the scroll wheel) scroll, `c`
//! clears the log, and `Esc`/`q` close.

use std::io;
use std::time::Duration;

use anyhow::Result;
use chrono::{DateTime, Local};
use crossterm::{
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, MouseButton,
        MouseEventKind,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
};

use shelbi_state::ErrorLogEntry;

/// Column where an entry's message starts (and where its wrapped continuation
/// lines align): `● ` marker (2) + `YYYY-MM-DD HH:MM` (16) + 2 spaces.
const MESSAGE_INDENT: usize = 20;

/// The viewer's mutable state: the entries (newest-first) with their unread
/// flag, plus the current scroll offset in rendered lines. Kept separate from
/// the event loop so the scroll/clear logic is unit-testable without a terminal.
struct Viewer {
    /// `(entry, was_unread_when_opened)`, newest first.
    rows: Vec<(ErrorLogEntry, bool)>,
    /// First rendered line shown at the top of the viewport.
    scroll: usize,
}

impl Viewer {
    fn new(rows: Vec<(ErrorLogEntry, bool)>) -> Self {
        Self { rows, scroll: 0 }
    }

    /// Clamp the scroll offset so it never scrolls past the last line. `total`
    /// is the rendered line count, `visible` the viewport height.
    fn clamp(&mut self, total: usize, visible: usize) {
        let max = total.saturating_sub(visible);
        if self.scroll > max {
            self.scroll = max;
        }
    }

    fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    fn scroll_down(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_add(n);
    }
}

/// Render + drive the error-log popup. Marks the log read on open, then loops
/// until the user closes it (`Esc`/`q`) or clears it (`c`). Best-effort: a
/// read/mark failure degrades to an empty view rather than aborting.
pub fn run(project: String) -> Result<()> {
    // Snapshot the unread state BEFORE marking read, so the `●` markers reflect
    // what was new when the log was opened; then mark everything read so the
    // sidebar button clears once this popup closes.
    let rows = shelbi_state::read_errors_with_unread(&project).unwrap_or_default();
    let _ = shelbi_state::mark_errors_read(&project);

    let mut term = setup_terminal()?;
    let _guard = TerminalGuard;
    let mut viewer = Viewer::new(rows);

    loop {
        term.draw(|f| render(f, &mut viewer))?;
        if !event::poll(Duration::from_millis(150))? {
            continue;
        }
        match event::read()? {
            Event::Key(k) if k.kind == KeyEventKind::Press => match k.code {
                KeyCode::Esc | KeyCode::Char('q') => break,
                KeyCode::Up | KeyCode::Char('k') => viewer.scroll_up(1),
                KeyCode::Down | KeyCode::Char('j') => viewer.scroll_down(1),
                KeyCode::PageUp => viewer.scroll_up(10),
                KeyCode::PageDown => viewer.scroll_down(10),
                KeyCode::Home => viewer.scroll = 0,
                KeyCode::Char('c') => {
                    // Clear resets both the log and the read marker; reflect it in
                    // the live view so the popup shows the empty state at once.
                    let _ = shelbi_state::clear_errors(&project);
                    viewer.rows.clear();
                    viewer.scroll = 0;
                }
                _ => {}
            },
            Event::Mouse(m) => match m.kind {
                MouseEventKind::ScrollUp => viewer.scroll_up(1),
                MouseEventKind::ScrollDown => viewer.scroll_down(1),
                MouseEventKind::Down(MouseButton::Left) => {}
                _ => {}
            },
            _ => {}
        }
    }

    restore_terminal(&mut term)?;
    Ok(())
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
        // A word longer than the whole width is hard-split across lines.
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
fn build_lines(rows: &[(ErrorLogEntry, bool)], width: usize) -> Vec<Line<'static>> {
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

fn render(f: &mut Frame, viewer: &mut Viewer) {
    // tmux has already centered and sized the popup pane; draw the single
    // bordered modal that fills it.
    let area = f.area();
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

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), DisableMouseCapture, LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

/// RAII backstop that restores the terminal on any early return / panic, so a
/// bail-out can't strand the popup pane in raw mode / the alt-screen.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // Continuation lines align under the message column.
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
        assert_eq!(v.scroll, 20, "clamped to total - visible");
        v.clamp(5, 10);
        assert_eq!(v.scroll, 0, "everything fits: no scroll");
    }
}
