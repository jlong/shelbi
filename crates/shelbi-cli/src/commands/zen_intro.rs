//! First-enable Zen Mode intro popover.
//!
//! Shown from the palette the first time the user picks "Turn Zen Mode
//! on" while `~/.shelbi/state.json::zen_intro_seen` is unset. Explains
//! what Zen does (auto-promote, auto-merge), what the safety mechanism
//! is (per-project checks), and the escape hatches (pause, off,
//! CLAUDE.md). On dismissal with "Don't show this again" checked we
//! persist the flag so the popover never re-fires.
//!
//! The state machine + step function are pure data so the focus-cycle,
//! checkbox-toggle, and confirm/cancel paths can be unit-tested
//! without spinning up a terminal. The render fn paints into the
//! palette's existing alt-screen — same pattern as the Quit-Project
//! and Quit-Shelbi confirmation popovers.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

/// Which control has keyboard focus inside the intro popover.
/// Tab cycles forward through these three in the order written; BackTab
/// cycles backwards. Enter / Space activates the focused control —
/// toggling the checkbox or firing the corresponding button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntroFocus {
    Checkbox,
    Cancel,
    Enable,
}

/// Mutable state the popover renderer reads each frame. Constructed via
/// [`IntroState::default`] — spec calls for the Enable Zen button to
/// own initial focus (the user just clicked Enable, are you sure?) and
/// the checkbox starts unchecked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntroState {
    pub focus: IntroFocus,
    pub dont_show_again: bool,
}

impl Default for IntroState {
    fn default() -> Self {
        Self {
            focus: IntroFocus::Enable,
            dont_show_again: false,
        }
    }
}

/// Result of feeding a key into the popover state machine. The caller
/// drives the loop: `Continue` keeps redrawing, `Cancelled` /
/// `Confirmed` exit and the caller looks at `state.dont_show_again` to
/// decide whether to persist the global flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntroOutcome {
    Continue,
    Cancelled,
    Confirmed,
}

/// Step the popover state machine. Pure function — no IO, no terminal,
/// so every key path is covered by the unit tests below.
///
/// Bindings (spec):
/// - `Tab` cycles Checkbox → Cancel → Enable → Checkbox; `BackTab`
///   walks backwards. Left/Right alias to BackTab/Tab so users who
///   reach for arrow keys land on the next control.
/// - `Enter` / `Space` activates focus: toggles the checkbox, fires
///   the Cancel button, or fires Enable.
/// - `Esc` is equivalent to Cancel regardless of focus.
pub fn step_intro(state: &mut IntroState, key: KeyEvent) -> IntroOutcome {
    match key.code {
        KeyCode::Esc => IntroOutcome::Cancelled,
        KeyCode::Tab | KeyCode::Right => {
            state.focus = match state.focus {
                IntroFocus::Checkbox => IntroFocus::Cancel,
                IntroFocus::Cancel => IntroFocus::Enable,
                IntroFocus::Enable => IntroFocus::Checkbox,
            };
            IntroOutcome::Continue
        }
        KeyCode::BackTab | KeyCode::Left => {
            state.focus = match state.focus {
                IntroFocus::Checkbox => IntroFocus::Enable,
                IntroFocus::Cancel => IntroFocus::Checkbox,
                IntroFocus::Enable => IntroFocus::Cancel,
            };
            IntroOutcome::Continue
        }
        KeyCode::Enter | KeyCode::Char(' ') => match state.focus {
            IntroFocus::Checkbox => {
                state.dont_show_again = !state.dont_show_again;
                IntroOutcome::Continue
            }
            IntroFocus::Cancel => IntroOutcome::Cancelled,
            IntroFocus::Enable => IntroOutcome::Confirmed,
        },
        _ => IntroOutcome::Continue,
    }
}

/// Paint the popover overlay. Sized to comfortably fit the body copy at
/// terminal widths from ~64 columns up; clipping kicks in on narrower
/// panes (see [`centered_rect`]). Buttons render at the bottom of the
/// inner block in the same Cancel-left / primary-right order the
/// existing Quit popovers use.
pub fn render_intro(f: &mut Frame, area: Rect, state: &IntroState) {
    let overlay = centered_rect(64, 18, area);
    f.render_widget(Clear, overlay);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(
            " Enable Zen Mode? ",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )));
    let inner = block.inner(overlay);
    f.render_widget(block, overlay);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // blank
            Constraint::Min(8),    // body copy
            Constraint::Length(1), // checkbox
            Constraint::Length(1), // blank
            Constraint::Length(1), // buttons
        ])
        .split(inner);

    let body = Paragraph::new(
        "Zen Mode lets the orchestrator act autonomously:\n\
         \n  \
         • Auto-promotes ready tasks to dispatch\n  \
         • Auto-merges completed tasks that pass your project's checks \
         (build/test + diff size + danger paths)\n\
         \n\
         You can pause it (palette → Pause Zen) or turn it off any time. \
         Edit CLAUDE.md to tune what qualifies as in-scope or what gates merges.",
    )
    .style(Style::default().fg(Color::Gray))
    .wrap(Wrap { trim: true });
    f.render_widget(body, layout[1]);

    let checkbox_glyph = if state.dont_show_again { "[x]" } else { "[ ]" };
    let checkbox_style = if state.focus == IntroFocus::Checkbox {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
    } else {
        Style::default().fg(Color::Gray)
    };
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!("{checkbox_glyph} Don't show this again"),
            checkbox_style,
        ))),
        layout[2],
    );

    let cancel_style = if state.focus == IntroFocus::Cancel {
        Style::default()
            .bg(Color::Blue)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let enable_style = if state.focus == IntroFocus::Enable {
        Style::default()
            .bg(Color::Green)
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let buttons = Paragraph::new(Line::from(vec![
        Span::styled("  [ Cancel ]  ", cancel_style),
        Span::raw("  "),
        Span::styled("  [ Enable Zen ]  ", enable_style),
    ]));
    f.render_widget(buttons, layout[4]);
}

/// Center a `w × h` rect inside `area`, clipping to the available size.
/// Mirrors the helper in `zen_probe` so the two overlays render at the
/// same anchor in the same pane.
fn centered_rect(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect {
        x,
        y,
        width: w,
        height: h,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
        // Spec: Tab cycles focus between checkbox, Cancel, Enable Zen.
        // Starting at Enable (default), forward Tab walks Enable →
        // Checkbox → Cancel → Enable.
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
        assert_eq!(step_intro(&mut s, key(KeyCode::Enter)), IntroOutcome::Continue);
        assert!(s.dont_show_again);
        // Second activation un-checks.
        assert_eq!(step_intro(&mut s, key(KeyCode::Enter)), IntroOutcome::Continue);
        assert!(!s.dont_show_again);
    }

    #[test]
    fn space_activates_focused_control_just_like_enter() {
        // Space on the checkbox toggles it; space on a button activates
        // it. Keeps muscle memory consistent across the focusables.
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
        assert_eq!(
            step_intro(&mut s, key(KeyCode::Left)),
            IntroOutcome::Continue
        );
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
        // Acceptance (a): the popover renders the explanatory copy
        // (auto-promote, auto-merge, the safety mechanism, the escape
        // hatches) plus the don't-show-again checkbox and both Cancel /
        // Enable Zen buttons. We dump a TestBackend and assert the
        // user-visible labels are present so a future copy edit can't
        // silently drop a section.
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
        // Default state shows an unchecked box, not `[x]`.
        assert!(
            dumped.contains("[ ] Don't show this again"),
            "default state must render unchecked checkbox:\n{dumped}",
        );
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
