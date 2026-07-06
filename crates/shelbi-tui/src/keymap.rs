//! Chord matching for the Zen Mode toggle hotkey.
//!
//! The user picks the chord via the first-run probe (saved to
//! `~/.shelbi/config.yaml::keymap.zen_toggle`). At runtime each `KeyEvent`
//! is tested against [`matches_zen_toggle`] before any other binding —
//! Alt+Z, Ctrl+G, etc. must take priority over `g` / `z` as nav keys.

use crossterm::event::{KeyCode, KeyModifiers};
use shelbi_state::keymap::{format_chord, DisplayStyle, KeyChord};
use shelbi_state::ZenToggleChord;

/// Render a chord for a help footer, falling back to `<unbound>` when the
/// action has no binding. Help rows reference actions by enum, so a user
/// who unbinds a help-referenced action (via `keys.yaml`) gets a visible
/// `<unbound>` marker rather than a panic or a silently dropped hint.
pub fn format_chord_or_unbound(chord: Option<&KeyChord>, style: DisplayStyle) -> String {
    match chord {
        Some(c) => format_chord(c, style),
        None => "<unbound>".to_string(),
    }
}

/// True when the given key event matches the configured Zen toggle chord.
/// [`ZenToggleChord::None`] never matches — that's the "skip" outcome of
/// the first-run probe.
pub fn matches_zen_toggle(code: KeyCode, mods: KeyModifiers, chord: ZenToggleChord) -> bool {
    match chord {
        ZenToggleChord::AltZ => {
            // Crossterm sometimes reports the SHIFT bit set on Alt+letter
            // depending on the terminal; the only thing we care about is
            // that ALT is held and the key is z (case-insensitive).
            mods.contains(KeyModifiers::ALT)
                && matches!(code, KeyCode::Char('z') | KeyCode::Char('Z'))
        }
        ZenToggleChord::CtrlBackslash => {
            mods.contains(KeyModifiers::CONTROL) && matches!(code, KeyCode::Char('\\'))
        }
        ZenToggleChord::CtrlG => {
            mods.contains(KeyModifiers::CONTROL)
                && matches!(code, KeyCode::Char('g') | KeyCode::Char('G'))
        }
        ZenToggleChord::CtrlShiftZ => {
            mods.contains(KeyModifiers::CONTROL)
                && mods.contains(KeyModifiers::SHIFT)
                && matches!(code, KeyCode::Char('z') | KeyCode::Char('Z'))
        }
        ZenToggleChord::None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_z_matches_lower_and_upper() {
        assert!(matches_zen_toggle(
            KeyCode::Char('z'),
            KeyModifiers::ALT,
            ZenToggleChord::AltZ
        ));
        assert!(matches_zen_toggle(
            KeyCode::Char('Z'),
            KeyModifiers::ALT,
            ZenToggleChord::AltZ
        ));
        // Alt+Shift+Z must still match — some terminals add SHIFT to Alt+letter.
        assert!(matches_zen_toggle(
            KeyCode::Char('z'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
            ZenToggleChord::AltZ
        ));
    }

    #[test]
    fn plain_z_does_not_match_alt_z_binding() {
        // The nav `z` key (if ever bound) must keep working when Zen isn't
        // the configured chord — only ALT+z fires the toggle.
        assert!(!matches_zen_toggle(
            KeyCode::Char('z'),
            KeyModifiers::NONE,
            ZenToggleChord::AltZ
        ));
    }

    #[test]
    fn ctrl_g_matches_only_with_ctrl() {
        assert!(matches_zen_toggle(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
            ZenToggleChord::CtrlG
        ));
        assert!(!matches_zen_toggle(
            KeyCode::Char('g'),
            KeyModifiers::NONE,
            ZenToggleChord::CtrlG
        ));
    }

    #[test]
    fn ctrl_shift_z_requires_both_modifiers() {
        assert!(matches_zen_toggle(
            KeyCode::Char('Z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ZenToggleChord::CtrlShiftZ
        ));
        assert!(!matches_zen_toggle(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
            ZenToggleChord::CtrlShiftZ
        ));
    }

    #[test]
    fn none_chord_never_matches() {
        for code in [KeyCode::Char('z'), KeyCode::Char('\\'), KeyCode::Char('g')] {
            for mods in [
                KeyModifiers::NONE,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL,
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            ] {
                assert!(
                    !matches_zen_toggle(code, mods, ZenToggleChord::None),
                    "{code:?}+{mods:?} should not match None"
                );
            }
        }
    }

    #[test]
    fn alt_letter_other_than_z_does_not_match() {
        assert!(!matches_zen_toggle(
            KeyCode::Char('a'),
            KeyModifiers::ALT,
            ZenToggleChord::AltZ
        ));
    }
}
