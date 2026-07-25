//! `shelbi __review-reject-reason --out PATH` — the reject-reason prompt,
//! meant to run inside a `tmux display-popup` (centered, `-B`-borderless so the
//! widget's own frame is the only border — the same surface style as the
//! `__review-confirm` popup). It shows a bordered textbox for the rejection
//! reason plus `[ Reject ]` / `[ Cancel ]` buttons.
//!
//! On submit it writes the typed reason to `PATH` and exits 0; on cancel it
//! writes nothing and exits non-zero. The launcher (the review panel) reads
//! `PATH` back to drive the existing `reject_review_task` transition — the same
//! plumbing the old inline prompt used, only the input surface moved out into a
//! popover. Empty reasons can't submit, matching the old inline prompt.

use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame, Terminal,
    backend::CrosstermBackend,
};

/// What currently holds focus. The textbox is the default focus so the
/// reviewer can start typing immediately; `Tab` cycles through the two
/// buttons and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Focus {
    #[default]
    Text,
    Reject,
    Cancel,
}

impl Focus {
    /// Tab order: textbox → Reject → Cancel → textbox.
    fn next(self) -> Focus {
        match self {
            Focus::Text => Focus::Reject,
            Focus::Reject => Focus::Cancel,
            Focus::Cancel => Focus::Text,
        }
    }

    /// Reverse tab order (Shift+Tab).
    fn prev(self) -> Focus {
        match self {
            Focus::Text => Focus::Cancel,
            Focus::Reject => Focus::Text,
            Focus::Cancel => Focus::Reject,
        }
    }
}

/// The result of feeding one key to the prompt. Split from the event loop so
/// the key logic is unit-testable without a terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// Stay open (typed a char, moved focus, or tried to submit an empty
    /// reason).
    Continue,
    /// Submit the (non-empty) reason.
    Submit,
    /// Dismiss without rejecting.
    Cancel,
}

/// Pure prompt state — the typed reason and current focus. Side-effect free so
/// [`RejectPrompt::key`] can be exercised in unit tests.
#[derive(Debug, Clone, Default)]
struct RejectPrompt {
    reason: String,
    focus: Focus,
}

impl RejectPrompt {
    /// Feed one key. `Esc` always cancels; `Tab`/`BackTab` cycle focus;
    /// `Enter`/`Space` activate the focused button (or submit from the
    /// textbox); `Left`/`Right` move between the buttons when one is focused;
    /// printable chars and `Backspace` edit the reason only when the textbox
    /// is focused.
    fn key(&mut self, code: KeyCode) -> Outcome {
        match code {
            KeyCode::Esc => Outcome::Cancel,
            KeyCode::Tab => {
                self.focus = self.focus.next();
                Outcome::Continue
            }
            KeyCode::BackTab => {
                self.focus = self.focus.prev();
                Outcome::Continue
            }
            KeyCode::Enter => match self.focus {
                Focus::Cancel => Outcome::Cancel,
                Focus::Text | Focus::Reject => self.try_submit(),
            },
            // Space activates a focused button; inside the textbox it types a
            // literal space (handled by the general `Char` arm below).
            KeyCode::Char(' ') if self.focus == Focus::Reject => self.try_submit(),
            KeyCode::Char(' ') if self.focus == Focus::Cancel => Outcome::Cancel,
            // Left/Right nudge between the two buttons when a button is focused.
            KeyCode::Left | KeyCode::Right if self.focus != Focus::Text => {
                self.focus = match self.focus {
                    Focus::Cancel => Focus::Reject,
                    _ => Focus::Cancel,
                };
                Outcome::Continue
            }
            KeyCode::Backspace if self.focus == Focus::Text => {
                self.reason.pop();
                Outcome::Continue
            }
            KeyCode::Char(c) if self.focus == Focus::Text => {
                self.reason.push(c);
                Outcome::Continue
            }
            _ => Outcome::Continue,
        }
    }

    /// A reason must be non-blank to submit; an empty submit is a no-op that
    /// keeps the popup open (matching the old inline prompt).
    fn try_submit(&self) -> Outcome {
        if self.reason.trim().is_empty() {
            Outcome::Continue
        } else {
            Outcome::Submit
        }
    }
}

/// Render + drive the reject-reason popup. On submit, writes the trimmed reason
/// to `out` and returns `true`; on cancel, writes nothing and returns `false`.
pub fn run(out: String) -> Result<bool> {
    let mut term = setup_terminal()?;
    // Restore the terminal on any early return / panic — the caller reads our
    // process exit code, so a stranded raw-mode popup pane would be worse than
    // one that just closes.
    let _guard = TerminalGuard;
    let mut prompt = RejectPrompt::default();

    let submitted = loop {
        term.draw(|f| render(f, &prompt))?;
        if event::poll(Duration::from_millis(150))? {
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match prompt.key(k.code) {
                    Outcome::Submit => break true,
                    Outcome::Cancel => break false,
                    Outcome::Continue => {}
                }
            }
        }
    };

    restore_terminal(&mut term)?;
    if submitted {
        std::fs::write(&out, prompt.reason.trim())
            .with_context(|| format!("writing reject reason to {out}"))?;
    }
    Ok(submitted)
}

fn render(f: &mut Frame, prompt: &RejectPrompt) {
    // The popup pane is the whole "screen" from here; tmux has already
    // centered and sized it (and, with `-B`, drawn no border of its own). Draw
    // the single bordered modal that fills it.
    let area = f.area();
    f.render_widget(Clear, area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Red))
        .title(" Reject task ");
    let inner = block.inner(area);
    f.render_widget(block, area);

    // label / textbox (3 lines: border + content + border) / spacer / buttons
    // / hint.
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(3),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Reason for rejecting:",
            Style::default().fg(Color::Gray),
        ))),
        rows[0],
    );

    render_textbox(f, prompt, rows[1]);

    let buttons = Line::from(vec![
        button("[ Reject ]", prompt.focus == Focus::Reject),
        Span::raw("   "),
        button("[ Cancel ]", prompt.focus == Focus::Cancel),
    ])
    .centered();
    f.render_widget(Paragraph::new(buttons), rows[3]);

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "Enter reject · Esc cancel · Tab focus",
            Style::default().fg(Color::DarkGray),
        ))),
        rows[4],
    );
}

/// The bordered reason textbox. Its border brightens to cyan when focused; a
/// block cursor always trails the text so the field reads as an input.
fn render_textbox(f: &mut Frame, prompt: &RejectPrompt, area: Rect) {
    let focused = prompt.focus == Focus::Text;
    let border = if focused { Color::Cyan } else { Color::DarkGray };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border));
    let text_area = block.inner(area);
    f.render_widget(block, area);
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{}▌", prompt.reason),
            Style::default().fg(Color::White),
        ))),
        text_area,
    );
}

/// A button span. The focused button is reverse-video + bold (a solid
/// highlighted block); an unfocused button is plain red text (Reject/Cancel
/// share the destructive tint of the frame).
fn button(label: &str, focused: bool) -> Span<'_> {
    let style = if focused {
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(Color::Red)
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
    use ratatui::{backend::TestBackend, Terminal};

    fn render_to_string(prompt: &RejectPrompt, w: u16, h: u16) -> String {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, prompt)).unwrap();
        let buf = term.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn popover_renders_frame_textbox_cursor_and_both_buttons() {
        let prompt = RejectPrompt {
            reason: "needs a test".into(),
            focus: Focus::Text,
        };
        let out = render_to_string(&prompt, 60, 11);
        assert!(out.contains("Reject task"), "titled frame: {out}");
        assert!(out.contains("Reason for rejecting:"), "label: {out}");
        // The textbox shows the typed reason with a trailing block cursor.
        assert!(out.contains("needs a test▌"), "textbox + cursor: {out}");
        assert!(out.contains("[ Reject ]"), "reject button: {out}");
        assert!(out.contains("[ Cancel ]"), "cancel button: {out}");
    }

    #[test]
    fn focused_button_is_visibly_highlighted() {
        // The focused button carries the reverse-video highlight; an unfocused
        // one does not. Assert on the rendered cell modifiers rather than the
        // glyphs (both buttons print the same label text either way).
        use ratatui::style::Modifier;
        let prompt = RejectPrompt {
            reason: "x".into(),
            focus: Focus::Reject,
        };
        let mut term = Terminal::new(TestBackend::new(60, 11)).unwrap();
        term.draw(|f| render(f, &prompt)).unwrap();
        let buf = term.backend().buffer().clone();
        let reversed_cells = (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| buf[(x, y)].modifier.contains(Modifier::REVERSED))
        });
        assert!(reversed_cells, "the focused button must be reverse-video");
    }

    fn typed(prompt: &mut RejectPrompt, text: &str) {
        for c in text.chars() {
            assert_eq!(prompt.key(KeyCode::Char(c)), Outcome::Continue);
        }
    }

    #[test]
    fn textbox_is_focused_by_default() {
        assert_eq!(RejectPrompt::default().focus, Focus::Text);
    }

    #[test]
    fn typing_appends_to_the_reason_and_backspace_deletes() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "abc");
        assert_eq!(prompt.reason, "abc");
        assert_eq!(prompt.key(KeyCode::Backspace), Outcome::Continue);
        assert_eq!(prompt.reason, "ab");
    }

    #[test]
    fn space_is_typed_into_the_textbox_not_treated_as_activate() {
        let mut prompt = RejectPrompt::default();
        typed(&mut prompt, "a b");
        assert_eq!(prompt.reason, "a b");
    }

    #[test]
    fn enter_submits_only_a_non_empty_reason_from_the_textbox() {
        let mut prompt = RejectPrompt::default();
        // Empty reason: Enter is a no-op, popup stays open.
        assert_eq!(prompt.key(KeyCode::Enter), Outcome::Continue);
        typed(&mut prompt, "please fix the null deref");
        assert_eq!(prompt.key(KeyCode::Enter), Outcome::Submit);
    }

    #[test]
    fn esc_cancels_from_any_focus() {
        for focus in [Focus::Text, Focus::Reject, Focus::Cancel] {
            let mut prompt = RejectPrompt {
                reason: "some reason".into(),
                focus,
            };
            assert_eq!(prompt.key(KeyCode::Esc), Outcome::Cancel, "focus={focus:?}");
        }
    }

    #[test]
    fn tab_cycles_focus_through_textbox_and_both_buttons() {
        let mut prompt = RejectPrompt::default();
        assert_eq!(prompt.focus, Focus::Text);
        prompt.key(KeyCode::Tab);
        assert_eq!(prompt.focus, Focus::Reject);
        prompt.key(KeyCode::Tab);
        assert_eq!(prompt.focus, Focus::Cancel);
        prompt.key(KeyCode::Tab);
        assert_eq!(prompt.focus, Focus::Text);
        // BackTab reverses.
        prompt.key(KeyCode::BackTab);
        assert_eq!(prompt.focus, Focus::Cancel);
    }

    #[test]
    fn enter_on_the_reject_button_submits_a_non_empty_reason() {
        let mut prompt = RejectPrompt {
            reason: "fix it".into(),
            focus: Focus::Reject,
        };
        assert_eq!(prompt.key(KeyCode::Enter), Outcome::Submit);
    }

    #[test]
    fn reject_button_cannot_submit_an_empty_reason() {
        let mut prompt = RejectPrompt {
            reason: "   ".into(),
            focus: Focus::Reject,
        };
        assert_eq!(prompt.key(KeyCode::Enter), Outcome::Continue);
        assert_eq!(prompt.key(KeyCode::Char(' ')), Outcome::Continue);
    }

    #[test]
    fn enter_or_space_on_the_cancel_button_cancels() {
        for code in [KeyCode::Enter, KeyCode::Char(' ')] {
            let mut prompt = RejectPrompt {
                reason: "fix it".into(),
                focus: Focus::Cancel,
            };
            assert_eq!(prompt.key(code), Outcome::Cancel, "{code:?}");
        }
    }

    #[test]
    fn arrow_keys_move_between_buttons_but_not_from_the_textbox() {
        // On a button, Left/Right toggle between Reject and Cancel.
        let mut prompt = RejectPrompt {
            focus: Focus::Reject,
            ..Default::default()
        };
        prompt.key(KeyCode::Right);
        assert_eq!(prompt.focus, Focus::Cancel);
        prompt.key(KeyCode::Left);
        assert_eq!(prompt.focus, Focus::Reject);
        // From the textbox, arrows don't move focus (they're inert here).
        let mut prompt = RejectPrompt::default();
        prompt.key(KeyCode::Right);
        assert_eq!(prompt.focus, Focus::Text);
    }
}
