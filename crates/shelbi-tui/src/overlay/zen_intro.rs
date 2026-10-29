//! First-enable Zen Mode intro popover — shared overlay logic + rendering.
//!
//! Shown the first time the user picks "Turn Zen Mode on" while
//! `~/.shelbi/state.json::zen_intro_seen` is unset. Explains what Zen does
//! (auto-promote, auto-merge), the safety mechanism (per-project checks), and
//! the escape hatches (pause, off, CLAUDE.md). On dismissal with "Don't show
//! this again" checked the caller persists the flag so the popover never
//! re-fires.
//!
//! This module is the single implementation shared by the in-process TUI
//! overlay (removing-tmux Phase 4d) and the legacy tmux palette's in-alt-screen
//! popover (`shelbi-cli`): the state machine + step function are pure data, and
//! [`render_intro`] paints into any caller-supplied [`Rect`]. The caller owns
//! the event loop and the one-shot flag.

use crossterm::event::KeyEvent;
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};
use crate::theme::palette;

// The state machine is shared with the desktop app (`shelbi_app::overlay`).
pub use shelbi_app::overlay::zen_intro::{IntroFocus, IntroOutcome, IntroState};
use shelbi_app::overlay::zen_intro as shared;

/// Step the popover state machine ([`shared::step_intro`]) with a crossterm
/// key. Pure, so every key path is unit-tested below.
///
/// Bindings (spec):
/// - `Tab` cycles Checkbox → Cancel → Enable → Checkbox; `BackTab`
///   walks backwards. Left/Right alias to BackTab/Tab so users who
///   reach for arrow keys land on the next control.
/// - `Enter` / `Space` activates focus: toggles the checkbox, fires
///   the Cancel button, or fires Enable.
/// - `Esc` is equivalent to Cancel regardless of focus.
pub fn step_intro(state: &mut IntroState, key: KeyEvent) -> IntroOutcome {
    match crate::keymap::chord_from_event(key) {
        Some(chord) => shared::step_intro(state, chord),
        None => IntroOutcome::Continue,
    }
}

/// Paint the intro dialog into `area` (a centered overlay [`Rect`] over the
/// dimmed app), in the dialogs' shared border ([`super::dialog_frame`]).
/// The " Enable Zen Mode? " title is a plain in-content heading rather than
/// a border title. Buttons render at the bottom in the same Cancel-left /
/// primary-right order the existing Quit popovers use.
pub fn render_intro(f: &mut Frame, area: Rect, state: &IntroState) {
    // Clear the whole pane so whatever is behind us doesn't bleed through
    // around the copy, then lay the content out inside the border.
    f.render_widget(Clear, area);
    let content = super::dialog_frame(f.buffer_mut(), area, None, None);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // heading (in-content title)
            Constraint::Length(1), // blank
            Constraint::Min(8),    // body copy
            Constraint::Length(1), // checkbox
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
        ])
        .split(content);

    let heading = Paragraph::new(Line::from(Span::styled(
        shared::HEADING,
        Style::default()
            .fg(palette().ansi_cyan)
            .add_modifier(Modifier::BOLD),
    )));
    f.render_widget(heading, layout[0]);

    let body = Paragraph::new(shared::BODY)
    .style(Style::default().fg(palette().ansi_gray))
    .wrap(Wrap { trim: true });
    f.render_widget(body, layout[2]);

    let checkbox_glyph = if state.dont_show_again { "[x]" } else { "[ ]" };
    let checkbox_style = if state.focus == IntroFocus::Checkbox {
        Style::default()
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(palette().ansi_gray)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{checkbox_glyph} {}", shared::CHECKBOX),
            checkbox_style,
        ))),
        layout[3],
    );

    let cancel_style = if state.focus == IntroFocus::Cancel {
        Style::default()
            .bg(palette().selection)
            .fg(palette().ansi_white)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_dark_gray)
    };
    let enable_style = if state.focus == IntroFocus::Enable {
        Style::default()
            .bg(palette().ansi_green)
            .fg(palette().on_color)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_dark_gray)
    };
    let buttons = Paragraph::new(Line::from(vec![
        Span::styled(format!("  [ {} ]  ", shared::CANCEL), cancel_style),
        Span::raw("  "),
        Span::styled(format!("  [ {} ]  ", shared::ENABLE), enable_style),
    ]));
    f.render_widget(buttons, layout[5]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// The intro (title, body, the checkbox, Cancel and Enable Zen each
    /// focused) is readable on a light terminal.
    #[test]
    fn renders_readable_in_light() {
        use ratatui::{backend::TestBackend, Terminal};
        use shelbi_app::theme::Appearance;
        crate::theme::with_appearance(Appearance::Light, || {
            for (name, focus, checked) in [
                ("zen-intro-enable", IntroFocus::Enable, false),
                ("zen-intro-cancel", IntroFocus::Cancel, true),
                ("zen-intro-checkbox", IntroFocus::Checkbox, true),
            ] {
                let state = IntroState {
                    focus,
                    dont_show_again: checked,
                };
                let mut term = Terminal::new(TestBackend::new(70, 20)).unwrap();
                term.draw(|f| render_intro(f, f.area(), &state)).unwrap();
                crate::theme::assert_light_readable(name, term.backend().buffer());
            }
        });
    }

    #[test]
    fn default_focus_is_enable_and_checkbox_unchecked() {
        let s = IntroState::default();
        assert_eq!(s.focus, IntroFocus::Enable);
        assert!(
            !s.dont_show_again,
            "spec: checkbox starts unchecked so the user opts in explicitly",
        );
    }

    #[test]
    fn tab_cycles_focus_checkbox_cancel_enable_in_order() {
        let mut s = IntroState::default();
        assert_eq!(step_intro(&mut s, key(KeyCode::Tab)), IntroOutcome::Continue);
        assert_eq!(s.focus, IntroFocus::Checkbox);
        assert_eq!(step_intro(&mut s, key(KeyCode::Tab)), IntroOutcome::Continue);
        assert_eq!(s.focus, IntroFocus::Cancel);
        assert_eq!(step_intro(&mut s, key(KeyCode::Tab)), IntroOutcome::Continue);
        assert_eq!(s.focus, IntroFocus::Enable);
    }

    #[test]
    fn back_tab_walks_focus_in_reverse() {
        let mut s = IntroState::default();
        assert_eq!(
            step_intro(&mut s, key(KeyCode::BackTab)),
            IntroOutcome::Continue
        );
        assert_eq!(s.focus, IntroFocus::Cancel);
        assert_eq!(
            step_intro(&mut s, key(KeyCode::BackTab)),
            IntroOutcome::Continue
        );
        assert_eq!(s.focus, IntroFocus::Checkbox);
        assert_eq!(
            step_intro(&mut s, key(KeyCode::BackTab)),
            IntroOutcome::Continue
        );
        assert_eq!(s.focus, IntroFocus::Enable);
    }

    #[test]
    fn esc_cancels_regardless_of_focus() {
        for focus in [IntroFocus::Checkbox, IntroFocus::Cancel, IntroFocus::Enable] {
            let mut s = IntroState {
                focus,
                dont_show_again: false,
            };
            assert_eq!(step_intro(&mut s, key(KeyCode::Esc)), IntroOutcome::Cancelled);
        }
    }

    #[test]
    fn enter_on_enable_button_confirms() {
        let mut s = IntroState::default(); // focus = Enable
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Enter)),
            IntroOutcome::Confirmed
        );
    }

    #[test]
    fn enter_on_cancel_button_cancels() {
        let mut s = IntroState {
            focus: IntroFocus::Cancel,
            dont_show_again: false,
        };
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Enter)),
            IntroOutcome::Cancelled
        );
    }

    #[test]
    fn enter_on_checkbox_toggles_without_dismissing() {
        let mut s = IntroState {
            focus: IntroFocus::Checkbox,
            dont_show_again: false,
        };
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Enter)),
            IntroOutcome::Continue
        );
        assert!(s.dont_show_again);
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Enter)),
            IntroOutcome::Continue
        );
        assert!(!s.dont_show_again);
    }

    #[test]
    fn space_activates_focused_control_just_like_enter() {
        let mut s = IntroState {
            focus: IntroFocus::Checkbox,
            dont_show_again: false,
        };
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Char(' '))),
            IntroOutcome::Continue,
        );
        assert!(s.dont_show_again);
        let mut s = IntroState {
            focus: IntroFocus::Enable,
            dont_show_again: false,
        };
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Char(' '))),
            IntroOutcome::Confirmed,
        );
    }

    #[test]
    fn left_right_arrows_alias_to_backtab_tab() {
        let mut s = IntroState::default();
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Right)),
            IntroOutcome::Continue
        );
        assert_eq!(s.focus, IntroFocus::Checkbox);
        assert_eq!(step_intro(&mut s, key(KeyCode::Left)), IntroOutcome::Continue);
        assert_eq!(s.focus, IntroFocus::Enable);
    }

    #[test]
    fn unbound_key_is_a_no_op() {
        let mut before = IntroState::default();
        let outcome = step_intro(&mut before, key(KeyCode::Char('x')));
        assert_eq!(outcome, IntroOutcome::Continue);
        assert_eq!(before, IntroState::default());
    }

    #[test]
    fn render_paints_title_body_checkbox_and_buttons() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let state = IntroState::default();
        term.draw(|f| render_intro(f, f.area(), &state)).unwrap();
        let buf = term.backend().buffer().clone();
        let dumped: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");

        for needle in [
            "Enable Zen Mode?",
            "Auto-promotes",
            "Auto-merges",
            "CLAUDE.md",
            "Don't show this again",
            "[ Cancel ]",
            "[ Enable Zen ]",
        ] {
            assert!(
                dumped.contains(needle),
                "missing {needle:?} in rendered popover:\n{dumped}",
            );
        }
        assert!(
            dumped.contains("[ ] Don't show this again"),
            "default state must render unchecked checkbox:\n{dumped}",
        );
        // The dialogs' shared border frames it, once.
        crate::overlay::assert_dialog_border(&buf, buf.area).unwrap();
        assert_eq!(dumped.matches('┌').count(), 1, "one border:\n{dumped}");
    }

    #[test]
    fn render_paints_checked_checkbox_after_toggle() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let state = IntroState {
            focus: IntroFocus::Checkbox,
            dont_show_again: true,
        };
        term.draw(|f| render_intro(f, f.area(), &state)).unwrap();
        let buf = term.backend().buffer().clone();
        let dumped: String = (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            dumped.contains("[x] Don't show this again"),
            "checked state must render `[x]` glyph:\n{dumped}",
        );
    }
}
