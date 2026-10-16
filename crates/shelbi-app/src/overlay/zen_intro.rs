//! The first-run Zen Mode intro's state machine and copy.
//!
//! Shown the first time the user turns Zen on while the global
//! `zen_intro_seen` flag is unset ([`crate::zen::should_show_intro`]). On
//! dismissal with "Don't show this again" checked, the host persists the flag.

use shelbi_state::keymap::{Key, KeyChord};

/// The heading.
pub const HEADING: &str = "Enable Zen Mode?";

/// The body copy, with its own line breaks.
pub const BODY: &str = "Zen Mode lets the orchestrator act autonomously:\n\
\n  \
• Auto-promotes ready tasks to dispatch\n  \
• Auto-merges completed tasks that pass your project's checks \
(build/test + diff size + danger paths)\n\
\n\
You can pause it (palette → Pause Zen) or turn it off any time. \
Edit CLAUDE.md to tune what qualifies as in-scope or what gates merges.";

/// The checkbox label.
pub const CHECKBOX: &str = "Don't show this again";

/// The two buttons, Cancel left of the primary.
pub const CANCEL: &str = "Cancel";
pub const ENABLE: &str = "Enable Zen";

/// Which control has focus. Tab cycles Checkbox → Cancel → Enable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntroFocus {
    Checkbox,
    Cancel,
    Enable,
}

/// The intro's state. Enable starts focused (the user just asked for Zen);
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

/// What a key did. On `Cancelled` / `Confirmed` the host reads
/// `state.dont_show_again` to decide whether to persist the flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntroOutcome {
    Continue,
    Cancelled,
    Confirmed,
}

/// Step the intro. Tab / Right cycle focus forward, BackTab / Left back;
/// Enter / Space activate the focused control (toggle the checkbox, or fire
/// a button); Esc cancels from anywhere.
pub fn step_intro(state: &mut IntroState, chord: KeyChord) -> IntroOutcome {
    match chord.code {
        Key::Esc => IntroOutcome::Cancelled,
        Key::Tab | Key::Right => {
            state.focus = match state.focus {
                IntroFocus::Checkbox => IntroFocus::Cancel,
                IntroFocus::Cancel => IntroFocus::Enable,
                IntroFocus::Enable => IntroFocus::Checkbox,
            };
            IntroOutcome::Continue
        }
        Key::BackTab | Key::Left => {
            state.focus = match state.focus {
                IntroFocus::Checkbox => IntroFocus::Enable,
                IntroFocus::Cancel => IntroFocus::Checkbox,
                IntroFocus::Enable => IntroFocus::Cancel,
            };
            IntroOutcome::Continue
        }
        Key::Enter | Key::Char(' ') => activate(state),
        _ => IntroOutcome::Continue,
    }
}

/// A click on a control: focus it, then activate it as Enter would.
pub fn click(state: &mut IntroState, control: IntroFocus) -> IntroOutcome {
    state.focus = control;
    activate(state)
}

fn activate(state: &mut IntroState) -> IntroOutcome {
    match state.focus {
        IntroFocus::Checkbox => {
            state.dont_show_again = !state.dont_show_again;
            IntroOutcome::Continue
        }
        IntroFocus::Cancel => IntroOutcome::Cancelled,
        IntroFocus::Enable => IntroOutcome::Confirmed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, key};

    #[test]
    fn default_focus_is_enable_and_unchecked() {
        let s = IntroState::default();
        assert_eq!(s.focus, IntroFocus::Enable);
        assert!(!s.dont_show_again);
    }

    #[test]
    fn tab_and_arrows_cycle_focus_both_ways() {
        let mut s = IntroState::default();
        step_intro(&mut s, key(Key::Tab));
        assert_eq!(s.focus, IntroFocus::Checkbox);
        step_intro(&mut s, key(Key::Right));
        assert_eq!(s.focus, IntroFocus::Cancel);
        step_intro(&mut s, key(Key::BackTab));
        assert_eq!(s.focus, IntroFocus::Checkbox);
        step_intro(&mut s, key(Key::Left));
        assert_eq!(s.focus, IntroFocus::Enable);
    }

    #[test]
    fn enter_or_space_activates_the_focus() {
        let mut s = IntroState::default();
        assert_eq!(step_intro(&mut s, key(Key::Enter)), IntroOutcome::Confirmed);
        s.focus = IntroFocus::Checkbox;
        assert_eq!(step_intro(&mut s, ch(' ')), IntroOutcome::Continue);
        assert!(s.dont_show_again);
        s.focus = IntroFocus::Cancel;
        assert_eq!(step_intro(&mut s, ch(' ')), IntroOutcome::Cancelled);
    }

    #[test]
    fn esc_cancels_and_other_keys_do_nothing() {
        let mut s = IntroState::default();
        assert_eq!(step_intro(&mut s, ch('x')), IntroOutcome::Continue);
        assert_eq!(step_intro(&mut s, key(Key::Esc)), IntroOutcome::Cancelled);
    }

    #[test]
    fn clicks_focus_then_activate() {
        let mut s = IntroState::default();
        assert_eq!(click(&mut s, IntroFocus::Checkbox), IntroOutcome::Continue);
        assert!(s.dont_show_again);
        assert_eq!(click(&mut s, IntroFocus::Enable), IntroOutcome::Confirmed);
        assert_eq!(click(&mut s, IntroFocus::Cancel), IntroOutcome::Cancelled);
    }
}
