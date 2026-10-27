//! The reject-reason prompt's state machine.
//!
//! Reviewers write multi-paragraph feedback, so the reason is a real
//! multi-line editor: Enter inserts a line break, and submitting is its own
//! action (Ctrl-D from anywhere, or Tab to `[ Reject ]` and Enter / Space).
//! The arrows, Home / End, Backspace (across lines) and Delete edit. A blank
//! reason never submits. On [`Step::Submit`] the host reads
//! [`RejectPrompt::reason`] and sends `RejectReview { reason }`.

use shelbi_state::keymap::{Key, KeyChord, Mods};

/// The dialog's title, label, buttons and hint.
pub const TITLE: &str = "Reject task";
pub const LABEL: &str = "Reason for rejecting:";
pub const REJECT_LABEL: &str = "[ Reject ]";
pub const CANCEL_LABEL: &str = "[ Cancel ]";
pub const HINT: &str = "Ctrl-D reject · Enter newline · Esc cancel · Tab focus";

/// What holds focus. The text area starts focused; Tab cycles through the two
/// buttons and back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RejectFocus {
    #[default]
    Text,
    Reject,
    Cancel,
}

impl RejectFocus {
    fn next(self) -> RejectFocus {
        match self {
            RejectFocus::Text => RejectFocus::Reject,
            RejectFocus::Reject => RejectFocus::Cancel,
            RejectFocus::Cancel => RejectFocus::Text,
        }
    }

    fn prev(self) -> RejectFocus {
        match self {
            RejectFocus::Text => RejectFocus::Cancel,
            RejectFocus::Reject => RejectFocus::Text,
            RejectFocus::Cancel => RejectFocus::Reject,
        }
    }
}

pub use crate::text_area::TextArea;

/// What one key or click resolved to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Continue,
    Submit,
    Cancel,
}

/// The prompt: the reason buffer and the focus.
#[derive(Debug, Clone, Default)]
pub struct RejectPrompt {
    text: TextArea,
    focus: RejectFocus,
}

impl RejectPrompt {
    pub fn new() -> Self {
        Self::default()
    }

    /// The composed reason, newlines kept.
    pub fn reason(&self) -> String {
        self.text.text()
    }

    pub fn text(&self) -> &TextArea {
        &self.text
    }

    pub fn focus(&self) -> RejectFocus {
        self.focus
    }

    pub fn set_focus(&mut self, focus: RejectFocus) {
        self.focus = focus;
    }

    /// Feed one key, applying edits and navigation in place.
    pub fn handle_key(&mut self, chord: KeyChord) -> Step {
        let ctrl = chord.mods.contains(Mods::CONTROL);
        if ctrl && matches!(chord.code, Key::Char('d') | Key::Char('D')) {
            return self.try_submit();
        }
        let on_text = self.focus == RejectFocus::Text;
        match chord.code {
            Key::Esc => Step::Cancel,
            Key::Tab => {
                self.focus = self.focus.next();
                Step::Continue
            }
            Key::BackTab => {
                self.focus = self.focus.prev();
                Step::Continue
            }
            // On a button.
            Key::Enter | Key::Char(' ') if self.focus == RejectFocus::Reject => self.try_submit(),
            Key::Enter | Key::Char(' ') if self.focus == RejectFocus::Cancel => Step::Cancel,
            Key::Left | Key::Right if !on_text => {
                self.focus = match self.focus {
                    RejectFocus::Cancel => RejectFocus::Reject,
                    _ => RejectFocus::Cancel,
                };
                Step::Continue
            }
            // In the text area.
            _ if !on_text => Step::Continue,
            Key::Enter => {
                self.text.newline();
                Step::Continue
            }
            Key::Backspace => {
                self.text.backspace();
                Step::Continue
            }
            Key::Delete => {
                self.text.delete();
                Step::Continue
            }
            Key::Left => {
                self.text.move_left();
                Step::Continue
            }
            Key::Right => {
                self.text.move_right();
                Step::Continue
            }
            Key::Up => {
                self.text.move_up();
                Step::Continue
            }
            Key::Down => {
                self.text.move_down();
                Step::Continue
            }
            Key::Home => {
                self.text.col = 0;
                Step::Continue
            }
            Key::End => {
                self.text.col = self.text.lines[self.text.row].len();
                Step::Continue
            }
            Key::Char(c) if !ctrl && !chord.mods.contains(Mods::SUPER) => {
                self.text.insert(c);
                Step::Continue
            }
            _ => Step::Continue,
        }
    }

    /// A click on `[ Reject ]`: focus it and submit (a blank reason stays
    /// open).
    pub fn click_reject(&mut self) -> Step {
        self.focus = RejectFocus::Reject;
        self.try_submit()
    }

    /// A click on `[ Cancel ]`.
    pub fn click_cancel(&mut self) -> Step {
        Step::Cancel
    }

    /// A click in the text area focuses it.
    pub fn click_text(&mut self) {
        self.focus = RejectFocus::Text;
    }

    fn try_submit(&self) -> Step {
        if self.text.is_blank() {
            Step::Continue
        } else {
            Step::Submit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, ctrl, key};

    fn typed(p: &mut RejectPrompt, s: &str) {
        for c in s.chars() {
            p.handle_key(ch(c));
        }
    }

    #[test]
    fn typing_enter_and_backspace_edit_across_lines() {
        let mut p = RejectPrompt::new();
        typed(&mut p, "ab");
        p.handle_key(key(Key::Enter));
        typed(&mut p, "cd");
        assert_eq!(p.reason(), "ab\ncd");
        p.handle_key(key(Key::Home));
        p.handle_key(key(Key::Backspace));
        assert_eq!(p.reason(), "abcd", "backspace at a line start joins the lines");
        assert_eq!(p.text().cursor(), (0, 2));
        p.handle_key(key(Key::End));
        p.handle_key(key(Key::Delete));
        assert_eq!(p.reason(), "abcd");
    }

    #[test]
    fn arrows_move_the_caret_and_clamp_the_column() {
        let mut p = RejectPrompt::new();
        typed(&mut p, "long line");
        p.handle_key(key(Key::Enter));
        typed(&mut p, "ab");
        p.handle_key(key(Key::Up));
        assert_eq!(p.text().cursor(), (0, 2));
        p.handle_key(key(Key::Down));
        p.handle_key(key(Key::Down));
        assert_eq!(p.text().cursor(), (1, 2), "down on the last line goes to its end");
        p.handle_key(key(Key::Left));
        p.handle_key(key(Key::Left));
        p.handle_key(key(Key::Left));
        assert_eq!(p.text().cursor(), (0, 9), "left wraps to the previous line end");
    }

    #[test]
    fn ctrl_d_submits_a_non_blank_reason_from_any_focus() {
        let mut p = RejectPrompt::new();
        assert_eq!(p.handle_key(ctrl('d')), Step::Continue, "blank never submits");
        typed(&mut p, "null deref");
        p.handle_key(key(Key::Tab));
        p.handle_key(key(Key::Tab));
        assert_eq!(p.focus(), RejectFocus::Cancel);
        assert_eq!(p.handle_key(ctrl('d')), Step::Submit);
        assert_eq!(p.reason(), "null deref");
    }

    #[test]
    fn buttons_take_enter_space_and_arrows() {
        let mut p = RejectPrompt::new();
        typed(&mut p, "why");
        p.handle_key(key(Key::Tab));
        assert_eq!(p.focus(), RejectFocus::Reject);
        p.handle_key(key(Key::Right));
        assert_eq!(p.focus(), RejectFocus::Cancel);
        p.handle_key(key(Key::Left));
        assert_eq!(p.handle_key(ch(' ')), Step::Submit);
        p.handle_key(key(Key::BackTab));
        p.handle_key(key(Key::BackTab));
        assert_eq!(p.focus(), RejectFocus::Cancel);
        assert_eq!(p.handle_key(key(Key::Enter)), Step::Cancel);
        assert_eq!(p.reason(), "why", "keys on a button never type");
    }

    #[test]
    fn space_and_enter_type_in_the_text_area() {
        let mut p = RejectPrompt::new();
        typed(&mut p, "a b");
        p.handle_key(key(Key::Enter));
        assert_eq!(p.reason(), "a b\n");
    }

    #[test]
    fn esc_cancels_and_clicks_resolve() {
        let mut p = RejectPrompt::new();
        assert_eq!(p.handle_key(key(Key::Esc)), Step::Cancel);
        assert_eq!(p.click_reject(), Step::Continue, "blank reason stays open");
        assert_eq!(p.focus(), RejectFocus::Reject);
        p.click_text();
        typed(&mut p, "x");
        assert_eq!(p.click_reject(), Step::Submit);
        assert_eq!(p.click_cancel(), Step::Cancel);
    }
}
