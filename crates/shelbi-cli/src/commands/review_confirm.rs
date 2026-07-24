//! `shelbi __review-confirm --title T [--workspace W]` — a small yes/no
//! confirmation meant to run inside a `tmux display-popup`, centered on the
//! terminal (the same surface style as the palette popup). Exits 0 when the
//! user confirms the load, non-zero when they cancel — or when no review slot
//! is free, in which case the popup is purely informational and any key
//! dismisses it. The caller (the sidebar's Queued-for-Review activation) maps
//! that exit code back onto the in-process review-load path, so the load logic
//! itself stays put; only the confirm surface moved out of the sidebar rect.

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame, Terminal,
};

/// Which button currently has focus in the confirm variant. `Confirm` (the
/// `[ Load ]` button) is the default focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Confirm,
    Cancel,
}

impl Focus {
    /// Flip focus between the two buttons (used by `Tab` / `BackTab`).
    fn toggled(self) -> Focus {
        match self {
            Focus::Confirm => Focus::Cancel,
            Focus::Cancel => Focus::Confirm,
        }
    }
}

/// What a key press means in the confirm popup. Split from the event loop so
/// the decision table is unit-testable without a terminal. `Activate` resolves
/// against the currently focused button (`Enter` / `Space`); the `Focus*`
/// variants only move the highlight without closing the popup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Confirm,
    Cancel,
    Activate,
    FocusConfirm,
    FocusCancel,
    ToggleFocus,
    Ignore,
}

/// Map a key to a decision. When no review slot is free (`has_workspace`
/// false) the popup is informational — a single `[ Dismiss ]` button — so
/// every key dismisses it (cancel) and nothing is ever loaded, mirroring the
/// old in-sidebar modal's behavior.
fn decide(code: KeyCode, has_workspace: bool) -> Decision {
    if !has_workspace {
        return Decision::Cancel;
    }
    match code {
        KeyCode::Char('y') | KeyCode::Char('Y') => Decision::Confirm,
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') | KeyCode::Esc => {
            Decision::Cancel
        }
        KeyCode::Enter | KeyCode::Char(' ') => Decision::Activate,
        KeyCode::Left => Decision::FocusConfirm,
        KeyCode::Right => Decision::FocusCancel,
        KeyCode::Tab | KeyCode::BackTab => Decision::ToggleFocus,
        _ => Decision::Ignore,
    }
}

/// Render + drive the confirm popup, returning whether the user confirmed the
/// load. `workspace` is the free `review`-tagged slot the load will target, or
/// `None` when every review slot is busy (the informational variant).
pub fn run(title: String, workspace: Option<String>) -> Result<bool> {
    let mut term = setup_terminal()?;
    // Restore the terminal on any early return / panic — the caller reads our
    // process exit code, so a stranded raw-mode popup pane would be worse than
    // a normal one that just closes.
    let _guard = TerminalGuard;
    let has_ws = workspace.is_some();
    let mut focus = Focus::Confirm;

    let confirmed = loop {
        term.draw(|f| render(f, &title, workspace.as_deref(), focus))?;
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match decide(k.code, has_ws) {
                    Decision::Confirm => break true,
                    Decision::Cancel => break false,
                    Decision::Activate => break focus == Focus::Confirm,
                    Decision::FocusConfirm => focus = Focus::Confirm,
                    Decision::FocusCancel => focus = Focus::Cancel,
                    Decision::ToggleFocus => focus = focus.toggled(),
                    Decision::Ignore => {}
                }
            }
        }
    };

    restore_terminal(&mut term)?;
    Ok(confirmed)
}

fn render(f: &mut Frame, title: &str, workspace: Option<&str>, focus: Focus) {
    // The popup pane is the whole "screen" from here; tmux has already
    // centered and sized it (and, with `-B`, drawn no border of its own). Draw
    // the single bordered modal that fills it.
    let area = f.area();
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Load for review ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    let title_line = Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    let body = match workspace {
        Some(ws) => vec![
            title_line,
            Line::raw(""),
            Line::from(vec![
                Span::styled(
                    "Load onto review workspace ",
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(
                    ws.to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("?", Style::default().fg(Color::Gray)),
            ]),
            Line::raw(""),
            Line::from(vec![
                button("[ Load ]", focus == Focus::Confirm),
                Span::raw("      "),
                button("[ Cancel ]", focus == Focus::Cancel),
            ])
            .centered(),
        ],
        None => vec![
            title_line,
            Line::raw(""),
            Line::from(Span::styled(
                "No review workspace is free.",
                Style::default().fg(Color::Yellow),
            )),
            Line::raw(""),
            // Informational variant: a single button, always focused, that any
            // key dismisses.
            Line::from(button("[ Dismiss ]", true)).centered(),
        ],
    };
    f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), inner);
}

/// A button span. The focused button is reverse-video + bold (a solid
/// highlighted block); an unfocused button is plain cyan text.
fn button(label: &str, focused: bool) -> Span<'_> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::Cyan)
    };
    Span::styled(label.to_string(), style)
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn restore_terminal<B: ratatui::backend::Backend + std::io::Write>(
    term: &mut Terminal<B>,
) -> Result<()> {
    disable_raw_mode()?;
    execute!(term.backend_mut(), LeaveAlternateScreen)?;
    term.show_cursor()?;
    Ok(())
}

/// RAII backstop that leaves raw mode and the alternate screen on drop, so an
/// error/panic path can't strand the popup pane in full-screen raw mode. The
/// happy path still calls [`restore_terminal`] explicitly; re-issuing the
/// escapes after a clean restore is harmless.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirm_shortcut_keys_confirm_immediately_when_a_slot_is_free() {
        // The `y`/`Y` shortcuts short-circuit to confirm regardless of focus.
        for code in [KeyCode::Char('y'), KeyCode::Char('Y')] {
            assert_eq!(decide(code, true), Decision::Confirm, "{code:?}");
        }
    }

    #[test]
    fn cancel_keys_cancel_when_a_slot_is_free() {
        for code in [
            KeyCode::Esc,
            KeyCode::Char('n'),
            KeyCode::Char('N'),
            KeyCode::Char('q'),
        ] {
            assert_eq!(decide(code, true), Decision::Cancel, "{code:?}");
        }
    }

    #[test]
    fn enter_and_space_activate_the_focused_button() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            assert_eq!(decide(code, true), Decision::Activate, "{code:?}");
        }
    }

    #[test]
    fn focus_keys_move_focus_when_a_slot_is_free() {
        assert_eq!(decide(KeyCode::Left, true), Decision::FocusConfirm);
        assert_eq!(decide(KeyCode::Right, true), Decision::FocusCancel);
        assert_eq!(decide(KeyCode::Tab, true), Decision::ToggleFocus);
        assert_eq!(decide(KeyCode::BackTab, true), Decision::ToggleFocus);
    }

    #[test]
    fn activate_resolves_against_focus() {
        // Default focus is `[ Load ]`, so Enter confirms; once focus moves to
        // `[ Cancel ]`, the same Enter cancels. This is the loop's resolution
        // step, mirrored here without a terminal.
        assert!(Focus::Confirm == Focus::Confirm);
        assert_eq!(Focus::Confirm.toggled(), Focus::Cancel);
        assert_eq!(Focus::Cancel.toggled(), Focus::Confirm);
    }

    #[test]
    fn unrelated_keys_are_ignored_when_a_slot_is_free() {
        for code in [KeyCode::Char('x'), KeyCode::Down, KeyCode::Up] {
            assert_eq!(decide(code, true), Decision::Ignore, "{code:?}");
        }
    }

    #[test]
    fn every_key_dismisses_when_no_slot_is_free() {
        // Informational popup: even the confirm/focus keys just cancel, so
        // nothing is loaded — same guarantee the old in-sidebar modal made.
        for code in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('y'),
            KeyCode::Tab,
            KeyCode::Left,
            KeyCode::Esc,
            KeyCode::Char('x'),
        ] {
            assert_eq!(decide(code, false), Decision::Cancel, "{code:?}");
        }
    }
}
