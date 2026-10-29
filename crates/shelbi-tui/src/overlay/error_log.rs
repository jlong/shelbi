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
//! [`handle_key`]: fn@handle_key

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};

use shelbi_state::ErrorLogEntry;
use crate::theme::palette;

// The viewer state, its keys, and the row layout are shared with the desktop
// app (`shelbi_app::overlay::error_log`).
pub use shelbi_app::overlay::error_log::{
    format_ts, wrap, ErrorLogOutcome, Viewer, EMPTY, HINT, MESSAGE_INDENT,
};

/// Feed one crossterm key to the viewer ([`Viewer::handle_key`]). The caller
/// performs the clear IO when this returns [`ErrorLogOutcome::Clear`].
pub fn handle_key(viewer: &mut Viewer, k: KeyEvent) -> ErrorLogOutcome {
    match crate::keymap::chord_from_event(k) {
        Some(chord) => viewer.handle_key(chord),
        None => ErrorLogOutcome::Continue,
    }
}

/// Build the full list of rendered lines for the log, newest first. Each entry
/// is a header line (`● YYYY-MM-DD HH:MM  <message start>`) plus any wrapped
/// continuation lines indented under the message column. Pure so the layout is
/// unit-testable. `width` is the inner (bordered) content width.
pub fn build_lines(rows: &[(ErrorLogEntry, bool)], width: usize) -> Vec<Line<'static>> {
    if rows.is_empty() {
        return vec![Line::from(Span::styled(
            EMPTY,
            Style::default().fg(palette().ansi_dark_gray),
        ))];
    }
    let msg_width = width.saturating_sub(MESSAGE_INDENT).max(1);
    let mut out: Vec<Line> = Vec::new();
    for (entry, unread) in rows {
        let marker = if *unread { "● " } else { "  " };
        let marker_style = if *unread {
            Style::default()
                .fg(palette().error_marker)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().ansi_dark_gray)
        };
        let ts = format_ts(entry);
        let msg_lines = wrap(&entry.message, msg_width);
        for (i, chunk) in msg_lines.iter().enumerate() {
            if i == 0 {
                out.push(Line::from(vec![
                    Span::styled(marker.to_string(), marker_style),
                    Span::styled(format!("{ts}  "), Style::default().fg(palette().ansi_dark_gray)),
                    Span::styled(chunk.clone(), Style::default().fg(palette().ansi_white)),
                ]));
            } else {
                out.push(Line::from(vec![
                    Span::raw(" ".repeat(MESSAGE_INDENT)),
                    Span::styled(chunk.clone(), Style::default().fg(palette().ansi_white)),
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
    let inner = crate::overlay::dialog_frame(f.buffer_mut(), area, None, Some("Error log"));

    // body (grows) + a one-line hint at the bottom.
    let rows = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(inner);
    let body = rows[0];
    let visible = body.height as usize;

    let lines = build_lines(&viewer.rows, body.width as usize);
    viewer.clamp(lines.len(), visible);

    f.render_widget(
        Paragraph::new(lines).scroll((viewer.scroll() as u16, 0)),
        body,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            HINT,
            Style::default().fg(palette().ansi_dark_gray),
        ))),
        rows[1],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

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

    /// The viewer (border, title, unread and read entries with their
    /// timestamps and wrapped messages, the footer) and its empty state are
    /// readable on a light terminal.
    #[test]
    fn renders_readable_in_light() {
        use ratatui::{backend::TestBackend, Terminal};
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            let mut viewer = Viewer::new(vec![
                (
                    entry(
                        "2026-09-26T14:02:00.000000000Z",
                        "merge failed: PR #9 is not mergeable: the merge commit cannot be cleanly created",
                    ),
                    true,
                ),
                (entry("2026-09-25T17:40:00.000000000Z", "dispatch failed: workspace busy"), false),
            ]);
            let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
            term.draw(|f| render(f, f.area(), &mut viewer)).unwrap();
            crate::theme::assert_light_readable("error-log", term.backend().buffer());

            let mut empty = Viewer::new(Vec::new());
            let mut term = Terminal::new(TestBackend::new(70, 16)).unwrap();
            term.draw(|f| render(f, f.area(), &mut empty)).unwrap();
            crate::theme::assert_light_readable("error-log-empty", term.backend().buffer());
        });
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
        assert_eq!(handle_key(&mut v, key(KeyCode::Down)), ErrorLogOutcome::Continue);
        assert_eq!(v.scroll(), 1);
        assert_eq!(handle_key(&mut v, key(KeyCode::Up)), ErrorLogOutcome::Continue);
        assert_eq!(v.scroll(), 0);
        assert_eq!(handle_key(&mut v, key(KeyCode::Char('c'))), ErrorLogOutcome::Clear);
        // The viewer does not clear itself; the caller does after the IO.
        assert_eq!(v.rows.len(), 1);
        v.clear();
        assert!(v.rows.is_empty());
        assert_eq!(handle_key(&mut v, key(KeyCode::Esc)), ErrorLogOutcome::Close);
        assert_eq!(handle_key(&mut v, key(KeyCode::Char('q'))), ErrorLogOutcome::Close);
    }
}
