//! gpui key events to Shelbi's toolkit-independent [`KeyChord`].
//!
//! The desktop twin of the TUI's crossterm edge (`shelbi_tui::keymap::
//! chord_from_event`): the keymap layer in `shelbi_state::keymap` knows nothing
//! about any toolkit, so every key the window handles is converted here first
//! and then dispatched through the shared `keys.yaml` keymaps. The result
//! matches what the crossterm conversion produces for the same physical key,
//! so one `keys.yaml` binding means the same thing in both clients: the
//! sidebar/global keys in `crate::app` and the board's `kanban:`/`popover:`
//! keys in `crate::views::issues` both go through here.

use gpui::Keystroke;
use shelbi_state::keymap::{Key, KeyChord, Mods};

/// Translate a gpui [`Keystroke`] into a [`KeyChord`], or `None` for a key
/// outside Shelbi's chord vocabulary (a bare modifier, a media key). Such keys
/// can never be bound, so `None` means "no chord".
///
/// Letters keep the physical key and the Shift bit (`shift-k`, the canonical
/// form `keys.yaml`'s `K` parses to). Shifted punctuation takes the character
/// it types and drops Shift (`shift-/` is `?`), the way a terminal reports it.
/// Option-modified keys keep the physical key (`alt-z`, not `Ω`). Cmd maps to
/// Super; the fn key is not modeled.
pub fn chord_from_keystroke(keystroke: &Keystroke) -> Option<KeyChord> {
    let m = &keystroke.modifiers;
    let mut mods = Mods::NONE;
    if m.control {
        mods |= Mods::CONTROL;
    }
    if m.alt {
        mods |= Mods::ALT;
    }
    if m.shift {
        mods |= Mods::SHIFT;
    }
    if m.platform {
        mods |= Mods::SUPER;
    }

    let key = match keystroke.key.as_str() {
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "escape" => Key::Esc,
        // Shift+Tab is reported as BackTab (with Shift still held), as
        // crossterm does.
        "tab" if m.shift => Key::BackTab,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "space" => Key::Char(' '),
        name => {
            if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                if (1..=12).contains(&n) {
                    return Some(KeyChord::new(Key::F(n), mods));
                }
                return None;
            }
            let mut chars = name.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                return None;
            };
            if c.is_ascii_alphabetic() {
                Key::Char(c.to_ascii_lowercase())
            } else if m.shift && !m.control && !m.alt && !m.platform {
                // Shifted punctuation: the typed character, without Shift.
                match keystroke.key_char.as_deref().map(single_char) {
                    Some(Some(typed)) => {
                        mods = Mods::NONE;
                        Key::Char(typed)
                    }
                    _ => Key::Char(c),
                }
            } else {
                Key::Char(c)
            }
        }
    };
    Some(KeyChord::new(key, mods))
}

fn single_char(s: &str) -> Option<char> {
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Some(c),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::{GlobalAction, Keymaps, SidebarAction};

    /// A keystroke as macOS reports it: `key` is the physical key,
    /// `key_char` what it types.
    fn ks(spec: &str, key_char: Option<&str>) -> Keystroke {
        let mut k = Keystroke::parse(spec).expect(spec);
        k.key_char = key_char.map(str::to_string);
        k
    }

    fn chord(spec: &str, key_char: Option<&str>) -> String {
        chord_from_keystroke(&ks(spec, key_char))
            .map(|c| c.canonical())
            .unwrap_or_else(|| "<none>".into())
    }

    #[test]
    fn named_keys_map_to_the_chord_vocabulary() {
        for (spec, want) in [
            ("up", "up"),
            ("down", "down"),
            ("left", "left"),
            ("right", "right"),
            ("enter", "enter"),
            ("escape", "esc"),
            ("tab", "tab"),
            ("backspace", "backspace"),
            ("delete", "delete"),
            ("home", "home"),
            ("end", "end"),
            ("pageup", "page-up"),
            ("pagedown", "page-down"),
            ("space", "space"),
            ("f5", "f5"),
        ] {
            assert_eq!(chord(spec, None), want, "{spec}");
        }
    }

    #[test]
    fn modifiers_carry_over() {
        assert_eq!(chord("ctrl-h", None), "ctrl-h");
        assert_eq!(chord("ctrl-l", None), "ctrl-l");
        assert_eq!(chord("cmd-p", None), "super-p");
        // Option keeps the physical key, not the character it types.
        assert_eq!(chord("alt-z", Some("Ω")), "alt-z");
        assert_eq!(chord("ctrl-shift-z", None), "ctrl-shift-z");
    }

    #[test]
    fn shifted_letters_and_punctuation() {
        // `K` in keys.yaml parses to shift-k; gpui reports key "k" + shift.
        assert_eq!(chord("shift-k", Some("K")), "shift-k");
        // keys.yaml spells the board's move keys `H` / `L`.
        assert_eq!(
            chord_from_keystroke(&ks("shift-h", Some("H"))),
            Some(KeyChord::parse("H").unwrap())
        );
        assert_eq!(chord("shift-up", None), "shift-up");
        // Shifted punctuation is the typed character, as a terminal sends it.
        assert_eq!(chord("shift-/", Some("?")), "?");
        assert_eq!(chord("/", Some("/")), "/");
        // Shift+Tab is BackTab, as crossterm reports it.
        assert_eq!(
            chord_from_keystroke(&ks("shift-tab", None)).map(|c| c.code),
            Some(Key::BackTab)
        );
    }

    #[test]
    fn keys_outside_the_vocabulary_have_no_chord() {
        assert_eq!(chord("shift", None), "<none>");
        assert_eq!(chord("f20", None), "<none>");
        assert_eq!(chord("capslock", None), "<none>");
    }

    #[test]
    fn converted_chords_dispatch_through_the_shared_keymaps() {
        // The built-in defaults, without reading any `keys.yaml`.
        let keymaps: Keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        let dispatch_global =
            |spec: &str| keymaps.global.dispatch(chord_from_keystroke(&ks(spec, None)).unwrap());
        assert_eq!(dispatch_global("ctrl-h"), Some(GlobalAction::FocusSidebar));
        assert_eq!(dispatch_global("ctrl-l"), Some(GlobalAction::FocusMain));
        assert_eq!(dispatch_global("ctrl-p"), Some(GlobalAction::OpenPalette));
        // Backspace is never mistaken for the Ctrl+H focus move.
        assert_eq!(dispatch_global("backspace"), None);

        let dispatch_sidebar =
            |spec: &str| keymaps.sidebar.dispatch(chord_from_keystroke(&ks(spec, None)).unwrap());
        assert_eq!(dispatch_sidebar("j"), Some(SidebarAction::NavDown));
        assert_eq!(dispatch_sidebar("down"), Some(SidebarAction::NavDown));
        assert_eq!(dispatch_sidebar("k"), Some(SidebarAction::NavUp));
        assert_eq!(dispatch_sidebar("up"), Some(SidebarAction::NavUp));
        assert_eq!(dispatch_sidebar("enter"), Some(SidebarAction::Activate));
        assert_eq!(dispatch_sidebar("space"), Some(SidebarAction::Activate));
    }
}
