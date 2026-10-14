//! The gpui → Shelbi key edge.
//!
//! Shelbi's keymaps (`keys.yaml`, merged over the shipped defaults by
//! `shelbi_state::keymap::load_keymaps`) are toolkit-free: they dispatch a
//! [`KeyChord`] to an action. The TUI converts a crossterm event into a chord
//! (`crates/shelbi-tui/src/keymap.rs`); this is the same conversion for a gpui
//! [`Keystroke`], so the desktop's board keys are exactly the user's TUI keys,
//! rebinds included.

use gpui::Keystroke;
use shelbi_state::keymap::{Key, KeyChord, Mods};

/// Translate a gpui keystroke into a Shelbi chord. `None` for keys outside
/// Shelbi's chord vocabulary (media keys and the like), which can never be
/// bound.
///
/// gpui reports Shift+H as key `h` with the shift bit set; the chord parser
/// reads `H` as `(h, shift)`, so the two meet without case folding here.
pub fn chord_from_keystroke(keystroke: &Keystroke) -> Option<KeyChord> {
    let mut key = key_from_name(&keystroke.key)?;
    let m = &keystroke.modifiers;
    // Shift+Tab is its own key in Shelbi's vocabulary (crossterm's BackTab),
    // which carries no shift bit.
    let mut shift = m.shift;
    if key == Key::Tab && shift {
        key = Key::BackTab;
        shift = false;
    }
    let mut mods = Mods::NONE;
    if m.control {
        mods |= Mods::CONTROL;
    }
    if m.alt {
        mods |= Mods::ALT;
    }
    if shift {
        mods |= Mods::SHIFT;
    }
    if m.platform {
        mods |= Mods::SUPER;
    }
    Some(KeyChord::new(key, mods))
}

/// gpui's key names to Shelbi keys.
fn key_from_name(name: &str) -> Option<Key> {
    Some(match name {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "escape" => Key::Esc,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Char(' '),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            Key::F(f[1..].parse().ok()?)
        }
        other => {
            let mut chars = other.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    fn stroke(s: &str) -> Keystroke {
        Keystroke::parse(s).unwrap()
    }

    #[test]
    fn letters_arrows_and_named_keys_match_the_chord_grammar() {
        assert_eq!(chord_from_keystroke(&stroke("h")), Some(parse("h")));
        assert_eq!(chord_from_keystroke(&stroke("left")), Some(parse("left")));
        assert_eq!(chord_from_keystroke(&stroke("enter")), Some(parse("enter")));
        assert_eq!(chord_from_keystroke(&stroke("space")), Some(parse("space")));
        assert_eq!(chord_from_keystroke(&stroke("escape")), Some(parse("esc")));
        assert_eq!(chord_from_keystroke(&stroke("tab")), Some(parse("tab")));
        assert_eq!(chord_from_keystroke(&stroke("f5")), Some(parse("f5")));
    }

    #[test]
    fn shifted_letters_meet_the_uppercase_chords() {
        // keys.yaml spells the move keys `H` / `L`; gpui sends shift-h.
        assert_eq!(chord_from_keystroke(&stroke("shift-h")), Some(parse("H")));
        assert_eq!(chord_from_keystroke(&stroke("shift-up")), Some(parse("shift-up")));
        assert_eq!(chord_from_keystroke(&stroke("ctrl-p")), Some(parse("ctrl-p")));
        assert_eq!(chord_from_keystroke(&stroke("cmd-k")), Some(parse("super-k")));
    }
}
