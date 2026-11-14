//! Close All Other Projects' confirmation (the palette's command): one
//! dialog for every project it closes, saying how many, which, and that
//! their agents stop ([`shelbi_app::close_others::confirmation`], the
//! desktop's sheet's copy). `[ Close Projects ]` / `[ Cancel ]`, the review
//! dialog's buttons; Enter presses the focused one, `y` confirms, Esc / `n`
//! / `q` cancel, Tab and the arrows move between them, and a click presses
//! either.

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Wrap},
    Frame,
};
use shelbi_app::close_others::Confirmation;

use crate::theme::palette;

/// The dialog's border title.
pub const TITLE: &str = "Close All Other Projects";
const CANCEL_LABEL: &str = "[ Cancel ]";
/// The columns between the two buttons.
const GAP: u16 = 6;

/// How the dialog resolved, or that it's still up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Confirm,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Confirm,
    Cancel,
}

/// The dialog's state: what it says and which button has focus (the
/// confirm, as the review dialog's Load does).
#[derive(Debug, Clone)]
pub struct Dialog {
    copy: Confirmation,
    focus: Focus,
}

impl Dialog {
    pub fn new(copy: Confirmation) -> Self {
        Dialog {
            copy,
            focus: Focus::Confirm,
        }
    }

    pub fn copy(&self) -> &Confirmation {
        &self.copy
    }

    fn confirm_label(&self) -> String {
        format!("[ {} ]", self.copy.confirm)
    }

    pub fn handle_key(&mut self, ev: KeyEvent) -> Step {
        match ev.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => Step::Confirm,
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Char('q') => Step::Cancel,
            KeyCode::Enter | KeyCode::Char(' ') => match self.focus {
                Focus::Confirm => Step::Confirm,
                Focus::Cancel => Step::Cancel,
            },
            KeyCode::Left => {
                self.focus = Focus::Confirm;
                Step::Continue
            }
            KeyCode::Right => {
                self.focus = Focus::Cancel;
                Step::Continue
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.focus = match self.focus {
                    Focus::Confirm => Focus::Cancel,
                    Focus::Cancel => Focus::Confirm,
                };
                Step::Continue
            }
            _ => Step::Continue,
        }
    }

    /// A left-click at `(col, row)` in the dialog's `area`.
    pub fn handle_click(&mut self, area: Rect, col: u16, row: u16) -> Step {
        let (confirm, cancel) = self.buttons(area);
        let hit = |r: Rect| row == r.y && col >= r.x && col < r.x + r.width;
        if hit(confirm) {
            Step::Confirm
        } else if hit(cancel) {
            Step::Cancel
        } else {
            Step::Continue
        }
    }

    /// The buttons' cells: on the content's last row, centered as the
    /// rendered line is.
    fn buttons(&self, area: Rect) -> (Rect, Rect) {
        let inner = super::dialog_content(area);
        let y = inner.y + inner.height.saturating_sub(1);
        let confirm_w = self.confirm_label().chars().count() as u16;
        let cancel_w = CANCEL_LABEL.chars().count() as u16;
        let start = inner.x + inner.width.saturating_sub(confirm_w + GAP + cancel_w) / 2;
        (
            Rect::new(start, y, confirm_w, 1),
            Rect::new(start + confirm_w + GAP, y, cancel_w, 1),
        )
    }

    /// Paint into `area` (centered over the window by the shell).
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let inner = super::dialog_frame(f.buffer_mut(), area, None, Some(TITLE));
        let body = vec![
            Line::from(Span::styled(
                self.copy.title.clone(),
                Style::default().fg(palette().ansi_white).add_modifier(Modifier::BOLD),
            )),
            Line::raw(""),
            Line::from(Span::styled(self.copy.detail.clone(), Style::default().fg(palette().ansi_gray))),
        ];
        let text = Rect {
            height: inner.height.saturating_sub(2),
            ..inner
        };
        f.render_widget(Paragraph::new(body).wrap(Wrap { trim: true }), text);
        let confirm = self.confirm_label();
        let buttons = Line::from(vec![
            button(&confirm, self.focus == Focus::Confirm),
            Span::raw(" ".repeat(GAP as usize)),
            button(CANCEL_LABEL, self.focus == Focus::Cancel),
        ])
        .centered();
        let (row, _) = self.buttons(area);
        f.render_widget(
            Paragraph::new(buttons),
            Rect {
                y: row.y,
                height: 1,
                ..inner
            },
        );
    }
}

/// The review dialog's buttons: the focused one reversed and bold.
fn button(label: &str, focused: bool) -> Span<'_> {
    let style = if focused {
        Style::default()
            .fg(palette().ansi_cyan)
            .add_modifier(Modifier::REVERSED | Modifier::BOLD)
    } else {
        Style::default().fg(palette().ansi_cyan)
    };
    Span::styled(label, style)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn dialog() -> Dialog {
        Dialog::new(shelbi_app::close_others::confirmation(
            "proj",
            &["Beta".to_string(), "Gamma".to_string()],
        ))
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn enter_confirms_and_esc_cancels() {
        assert_eq!(dialog().handle_key(key(KeyCode::Enter)), Step::Confirm);
        assert_eq!(dialog().handle_key(key(KeyCode::Char('y'))), Step::Confirm);
        assert_eq!(dialog().handle_key(key(KeyCode::Esc)), Step::Cancel);
        let mut d = dialog();
        assert_eq!(d.handle_key(key(KeyCode::Tab)), Step::Continue);
        assert_eq!(d.handle_key(key(KeyCode::Enter)), Step::Cancel, "Enter on Cancel");
        assert_eq!(d.handle_key(key(KeyCode::Left)), Step::Continue);
        assert_eq!(d.handle_key(key(KeyCode::Enter)), Step::Confirm);
    }

    #[test]
    fn it_names_the_projects_and_its_buttons_take_clicks() {
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(64, 9)).unwrap();
        let area = Rect::new(0, 0, 64, 9);
        let d = dialog();
        term.draw(|f| d.render(f, area)).unwrap();
        let screen: String = term
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("Close 2 other projects?"), "{screen}");
        assert!(screen.contains("Beta and Gamma close"), "{screen}");
        assert!(screen.contains("[ Close Projects ]"), "{screen}");
        let (confirm, cancel) = d.buttons(area);
        assert_eq!(dialog().handle_click(area, confirm.x, confirm.y), Step::Confirm);
        assert_eq!(dialog().handle_click(area, cancel.x + 1, cancel.y), Step::Cancel);
        assert_eq!(dialog().handle_click(area, 0, 0), Step::Continue);
    }
}
