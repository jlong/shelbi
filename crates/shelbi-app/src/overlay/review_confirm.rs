//! The "Load for review" dialog's state machine.
//!
//! Choosing a queued review task (the palette's `review:<id>`, a Queued for
//! Review sidebar row) asks which review slot to load it onto. One dialog,
//! three shapes, by how many slots the host passes:
//!
//! - **0 slots**, informational: "No review workspace is configured." (or the
//!   host's own line, e.g. every slot is busy). Any key dismisses.
//! - **1 slot**, a yes/no confirm: "Load onto review workspace `review`?"
//!   with `[ Load ]` / `[ Cancel ]`.
//! - **>1 slots**, a picker listing every slot with its free / occupied
//!   state; pick one, then `[ Load ]` / `[ Cancel ]`.
//!
//! It resolves to [`Outcome::Load`] with the slot's name, or
//! [`Outcome::Cancel`].

use shelbi_state::keymap::{Key, KeyChord};

/// The dialog's title and buttons.
pub const TITLE: &str = "Load for review";
pub const LOAD_LABEL: &str = "[ Load ]";
pub const CANCEL_LABEL: &str = "[ Cancel ]";
pub const DISMISS_LABEL: &str = "[ Dismiss ]";

/// The default no-slots line.
pub const NO_WORKSPACE: &str = "No review workspace is configured.";

/// A review slot: its workspace name and, when occupied, the title of the
/// task loaded on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Slot {
    pub name: String,
    pub occupant: Option<String>,
}

impl Slot {
    /// The picker row's state note: `  (free)` / `  (serving "<title>")`.
    pub fn state_note(&self) -> String {
        match &self.occupant {
            Some(title) => format!("  (serving \"{title}\")"),
            None => "  (free)".to_string(),
        }
    }
}

/// Which control has focus. The picker's list, and both buttons, take focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmFocus {
    List,
    Load,
    Cancel,
}

/// The dialog's resolution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Load the task onto this slot (the workspace name).
    Load(String),
    Cancel,
}

/// One key or click: keep the dialog open, or resolve it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Continue,
    Done(Outcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Decision {
    Load,
    Cancel,
    Activate,
    Up,
    Down,
    FocusLoad,
    FocusCancel,
    ToggleFocus,
    Ignore,
}

/// The decision table. With no slots every key dismisses. `y` confirms only
/// in the single-slot shape (in the picker it would be ambiguous).
fn decide(code: Key, has_slots: bool, is_picker: bool) -> Decision {
    if !has_slots {
        return Decision::Cancel;
    }
    match code {
        Key::Char('l') | Key::Char('L') => Decision::Load,
        Key::Char('c')
        | Key::Char('C')
        | Key::Char('n')
        | Key::Char('N')
        | Key::Char('q')
        | Key::Esc => Decision::Cancel,
        Key::Char('y') | Key::Char('Y') if !is_picker => Decision::Load,
        Key::Enter | Key::Char(' ') => Decision::Activate,
        Key::Up | Key::Char('k') if is_picker => Decision::Up,
        Key::Down | Key::Char('j') if is_picker => Decision::Down,
        Key::Left => Decision::FocusLoad,
        Key::Right => Decision::FocusCancel,
        Key::Tab | Key::BackTab => Decision::ToggleFocus,
        _ => Decision::Ignore,
    }
}

/// The dialog's state.
#[derive(Debug, Clone)]
pub struct Dialog {
    title: String,
    slots: Vec<Slot>,
    selected: usize,
    focus: ConfirmFocus,
    empty_message: Option<String>,
}

impl Dialog {
    /// The dialog for task `title` over `slots`. The picker starts on the
    /// list; the confirm starts on `[ Load ]`.
    pub fn new(title: impl Into<String>, slots: Vec<Slot>) -> Self {
        let is_picker = slots.len() > 1;
        Dialog {
            title: title.into(),
            slots,
            selected: 0,
            focus: if is_picker {
                ConfirmFocus::List
            } else {
                ConfirmFocus::Load
            },
            empty_message: None,
        }
    }

    /// The no-slots shape with the host's own line (e.g. every review slot is
    /// busy). Any key dismisses it.
    pub fn informational(title: impl Into<String>, message: impl Into<String>) -> Self {
        Dialog {
            title: title.into(),
            slots: Vec::new(),
            selected: 0,
            focus: ConfirmFocus::Load,
            empty_message: Some(message.into()),
        }
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn slots(&self) -> &[Slot] {
        &self.slots
    }

    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn focus(&self) -> ConfirmFocus {
        self.focus
    }

    pub fn is_picker(&self) -> bool {
        self.slots.len() > 1
    }

    pub fn has_slots(&self) -> bool {
        !self.slots.is_empty()
    }

    /// The no-slots shape's line.
    pub fn empty_message(&self) -> &str {
        self.empty_message.as_deref().unwrap_or(NO_WORKSPACE)
    }

    /// Feed one key.
    pub fn handle_key(&mut self, chord: KeyChord) -> Step {
        self.handle_code(chord.code)
    }

    /// Feed one key by its code alone (the table ignores modifiers). A key
    /// outside the chord vocabulary should still dismiss the no-slots shape:
    /// hosts pass any code they have, or call [`Self::dismiss_if_empty`].
    pub fn handle_code(&mut self, code: Key) -> Step {
        match decide(code, self.has_slots(), self.is_picker()) {
            Decision::Load => Step::Done(self.load_selected()),
            Decision::Cancel => Step::Done(Outcome::Cancel),
            Decision::Activate => Step::Done(self.activate()),
            Decision::Up => {
                self.move_selected(true);
                Step::Continue
            }
            Decision::Down => {
                self.move_selected(false);
                Step::Continue
            }
            Decision::FocusLoad => {
                self.focus = ConfirmFocus::Load;
                Step::Continue
            }
            Decision::FocusCancel => {
                self.focus = ConfirmFocus::Cancel;
                Step::Continue
            }
            Decision::ToggleFocus => {
                self.focus = match self.focus {
                    ConfirmFocus::Load => ConfirmFocus::Cancel,
                    ConfirmFocus::Cancel | ConfirmFocus::List => ConfirmFocus::Load,
                };
                Step::Continue
            }
            Decision::Ignore => Step::Continue,
        }
    }

    /// For a key with no chord: the no-slots shape still dismisses.
    pub fn dismiss_if_empty(&self) -> Step {
        if self.has_slots() {
            Step::Continue
        } else {
            Step::Done(Outcome::Cancel)
        }
    }

    /// A click on the `index`-th slot row selects it. On the no-slots shape
    /// any click dismisses.
    pub fn click_slot(&mut self, index: usize) -> Step {
        if !self.has_slots() {
            return Step::Done(Outcome::Cancel);
        }
        if index < self.slots.len() {
            self.selected = index;
            self.focus = ConfirmFocus::List;
        }
        Step::Continue
    }

    /// A click on `[ Load ]` (or `[ Dismiss ]` on the no-slots shape).
    pub fn click_load(&mut self) -> Step {
        Step::Done(self.load_selected())
    }

    /// A click on `[ Cancel ]`.
    pub fn click_cancel(&mut self) -> Step {
        Step::Done(Outcome::Cancel)
    }

    fn move_selected(&mut self, up: bool) {
        if up {
            self.selected = self.selected.saturating_sub(1);
        } else if self.selected + 1 < self.slots.len() {
            self.selected += 1;
        }
        self.focus = ConfirmFocus::List;
    }

    fn activate(&self) -> Outcome {
        match self.focus {
            ConfirmFocus::Cancel => Outcome::Cancel,
            ConfirmFocus::Load | ConfirmFocus::List => self.load_selected(),
        }
    }

    fn load_selected(&self) -> Outcome {
        match self.slots.get(self.selected) {
            Some(slot) => Outcome::Load(slot.name.clone()),
            None => Outcome::Cancel,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, key};

    fn slots(n: usize) -> Vec<Slot> {
        (0..n)
            .map(|i| Slot {
                name: format!("review-{}", i + 1),
                occupant: (i == 1).then(|| "Busy task".to_string()),
            })
            .collect()
    }

    fn load(name: &str) -> Step {
        Step::Done(Outcome::Load(name.into()))
    }

    #[test]
    fn the_slot_count_picks_the_shape() {
        assert!(!Dialog::new("t", slots(0)).has_slots());
        assert!(!Dialog::new("t", slots(1)).is_picker());
        assert!(Dialog::new("t", slots(2)).is_picker());
        assert_eq!(Dialog::new("t", slots(1)).focus(), ConfirmFocus::Load);
        assert_eq!(Dialog::new("t", slots(3)).focus(), ConfirmFocus::List);
    }

    #[test]
    fn the_confirm_loads_on_l_y_or_enter_and_cancels_on_esc_c_n_q() {
        for c in ['l', 'L', 'y', 'Y'] {
            assert_eq!(Dialog::new("t", slots(1)).handle_key(ch(c)), load("review-1"));
        }
        assert_eq!(Dialog::new("t", slots(1)).handle_key(key(Key::Enter)), load("review-1"));
        for chord in [key(Key::Esc), ch('c'), ch('n'), ch('q')] {
            assert_eq!(
                Dialog::new("t", slots(1)).handle_key(chord),
                Step::Done(Outcome::Cancel)
            );
        }
    }

    #[test]
    fn the_picker_navigates_and_ignores_y() {
        let mut d = Dialog::new("t", slots(3));
        assert_eq!(d.handle_key(ch('y')), Step::Continue);
        d.handle_key(key(Key::Down));
        d.handle_key(ch('j'));
        d.handle_key(ch('j'));
        assert_eq!(d.selected(), 2, "saturates at the last slot");
        d.handle_key(ch('k'));
        assert_eq!(d.handle_key(key(Key::Enter)), load("review-2"));
    }

    #[test]
    fn focus_moves_with_tab_and_arrows() {
        let mut d = Dialog::new("t", slots(1));
        d.handle_key(key(Key::Tab));
        assert_eq!(d.focus(), ConfirmFocus::Cancel);
        assert_eq!(d.handle_key(ch(' ')), Step::Done(Outcome::Cancel));
        d.handle_key(key(Key::Left));
        assert_eq!(d.focus(), ConfirmFocus::Load);
        d.handle_key(key(Key::Right));
        assert_eq!(d.focus(), ConfirmFocus::Cancel);
        d.handle_key(key(Key::BackTab));
        assert_eq!(d.focus(), ConfirmFocus::Load);
    }

    #[test]
    fn the_no_slots_shapes_dismiss_on_any_key_or_click() {
        let mut d = Dialog::new("t", Vec::new());
        assert_eq!(d.empty_message(), NO_WORKSPACE);
        assert_eq!(d.handle_key(ch('l')), Step::Done(Outcome::Cancel));
        assert_eq!(d.dismiss_if_empty(), Step::Done(Outcome::Cancel));
        let mut busy = Dialog::informational("t", "Every review slot is busy.");
        assert_eq!(busy.empty_message(), "Every review slot is busy.");
        assert_eq!(busy.click_slot(0), Step::Done(Outcome::Cancel));
        assert_eq!(busy.click_load(), Step::Done(Outcome::Cancel));
        assert_eq!(
            Dialog::new("t", slots(1)).dismiss_if_empty(),
            Step::Continue
        );
    }

    #[test]
    fn clicks_select_slots_and_press_buttons() {
        let mut d = Dialog::new("t", slots(3));
        assert_eq!(d.click_slot(2), Step::Continue);
        assert_eq!(d.selected(), 2);
        assert_eq!(d.click_load(), load("review-3"));
        assert_eq!(d.click_cancel(), Step::Done(Outcome::Cancel));
        assert_eq!(slots(2)[1].state_note(), "  (serving \"Busy task\")");
        assert_eq!(slots(1)[0].state_note(), "  (free)");
    }
}
