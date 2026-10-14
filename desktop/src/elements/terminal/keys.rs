//! gpui keystrokes onto `shelbi_term::input`.
//!
//! [`classify`] decides, for one key-down, what the terminal does with it. It
//! is pure (a [`Keystroke`] is plain data), so the chord table is unit-tested
//! without a window. Encoding itself is `shelbi_term::input::encode_key`, the
//! encoder the TUI uses, so the bytes a key produces are the same in both
//! clients.
//!
//! Plain text is deliberately *not* encoded here: [`KeyAction::Text`] leaves
//! the event unhandled so gpui passes it to the pane's input handler, which is
//! where macOS delivers IME composition and dead keys. Everything with a
//! control meaning (Enter, Tab, arrows, Ctrl and Alt chords, function keys) is
//! encoded directly.

use gpui::Keystroke;
use shelbi_term::input::{Key, Modifiers};

/// What one key-down does in a terminal pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyAction {
    /// Encode this key for the program and send it.
    Send(Key, Modifiers),
    /// Typed text: let it reach the input handler (IME, dead keys), which
    /// sends the text.
    Text,
    /// Cmd+C: copy the selection.
    Copy,
    /// Cmd+V: paste the clipboard.
    Paste,
    /// Shift+PageUp / Shift+PageDown / Shift+Home / Shift+End: Shelbi's own
    /// scrollback (the program gets the key on the alternate screen, where
    /// there is no scrollback).
    Scroll(ScrollKey),
    /// Not the terminal's (an app shortcut such as Cmd+W): let it propagate.
    Ignore,
}

/// A scrollback key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollKey {
    /// One page up.
    PageUp,
    /// One page down.
    PageDown,
    /// The oldest line.
    Top,
    /// The live bottom.
    Bottom,
}

impl ScrollKey {
    /// The key itself, for sending to the program when there is no scrollback.
    pub fn key(self) -> Key {
        match self {
            ScrollKey::PageUp => Key::PageUp,
            ScrollKey::PageDown => Key::PageDown,
            ScrollKey::Top => Key::Home,
            ScrollKey::Bottom => Key::End,
        }
    }
}

/// Classify one key-down. `option_as_alt` makes Option a Meta key for
/// character keys (Option+B sends `ESC b`) instead of typing the layout's
/// Option character (`∫`); named keys (Enter, arrows, ...) always carry Alt.
pub fn classify(ks: &Keystroke, option_as_alt: bool) -> KeyAction {
    let m = &ks.modifiers;
    let mods = Modifiers {
        shift: m.shift,
        alt: m.alt,
        ctrl: m.control,
        sup: m.platform,
    };

    // Cmd chords are the app's, except the clipboard pair.
    if m.platform {
        let plain = !m.control && !m.alt && !m.shift;
        return match ks.key.as_str() {
            "c" if plain => KeyAction::Copy,
            "v" if plain => KeyAction::Paste,
            _ => KeyAction::Ignore,
        };
    }

    if let Some(key) = named_key(&ks.key) {
        if m.shift && !m.control && !m.alt {
            let scroll = match key {
                Key::PageUp => Some(ScrollKey::PageUp),
                Key::PageDown => Some(ScrollKey::PageDown),
                Key::Home => Some(ScrollKey::Top),
                Key::End => Some(ScrollKey::Bottom),
                _ => None,
            };
            if let Some(scroll) = scroll {
                return KeyAction::Scroll(scroll);
            }
        }
        return KeyAction::Send(key, mods);
    }

    let ch = match single_char(&ks.key) {
        Some(c) => c,
        // A key gpui names but the encoder has no form for (F25+, media keys).
        None => return KeyAction::Ignore,
    };

    let meta = m.alt && option_as_alt;
    if m.control || meta {
        // A chord: the encoder needs the unmodified key (gpui reports
        // Ctrl+Shift+A as key "a" with shift held).
        let mods = Modifiers { alt: meta, ..mods };
        return KeyAction::Send(Key::Char(ch), mods);
    }

    // Plain, shifted, or Option-composed text: the input handler sends it.
    KeyAction::Text
}

/// gpui's names for keys that aren't text.
fn named_key(name: &str) -> Option<Key> {
    Some(match name {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        _ => {
            let n: u8 = name.strip_prefix('f')?.parse().ok()?;
            return (1..=24).contains(&n).then_some(Key::Function(n));
        }
    })
}

/// The character a key types: `space` is a space, other keys are a single
/// character.
fn single_char(name: &str) -> Option<char> {
    if name == "space" {
        return Some(' ');
    }
    let mut chars = name.chars();
    let c = chars.next()?;
    chars.next().is_none().then_some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_term::input::{encode_key, KeyEncoding};

    fn ks(spec: &str) -> Keystroke {
        Keystroke::parse(spec).expect("valid keystroke")
    }

    fn send(spec: &str) -> (Key, Modifiers) {
        match classify(&ks(spec), true) {
            KeyAction::Send(k, m) => (k, m),
            other => panic!("{spec}: expected Send, got {other:?}"),
        }
    }

    fn bytes(spec: &str, enc: KeyEncoding) -> Vec<u8> {
        let (k, m) = send(spec);
        encode_key(k, m, enc)
    }

    const LEGACY: KeyEncoding = KeyEncoding {
        kitty: false,
        application_cursor_keys: false,
        newline_mode: false,
    };
    const KITTY: KeyEncoding = KeyEncoding {
        kitty: true,
        application_cursor_keys: false,
        newline_mode: false,
    };

    #[test]
    fn enter_and_shift_enter() {
        assert_eq!(bytes("enter", LEGACY), b"\r");
        assert_eq!(bytes("enter", KITTY), b"\r");
        // Claude Code's newline under the kitty protocol.
        assert_eq!(bytes("shift-enter", KITTY), b"\x1b[13;2u");
        // Option+Enter is Meta+Enter, Claude Code's other newline.
        assert_eq!(bytes("alt-enter", LEGACY), b"\x1b\r");
    }

    #[test]
    fn tab_and_backtab() {
        assert_eq!(bytes("tab", LEGACY), b"\t");
        assert_eq!(bytes("tab", KITTY), b"\x1b[9u");
        assert_eq!(bytes("shift-tab", LEGACY), b"\x1b[Z");
        assert_eq!(bytes("shift-tab", KITTY), b"\x1b[Z");
    }

    #[test]
    fn backspace_is_not_ctrl_h() {
        assert_eq!(bytes("backspace", LEGACY), b"\x7f");
        assert_eq!(bytes("ctrl-h", LEGACY), b"\x08");
    }

    #[test]
    fn ctrl_chords() {
        assert_eq!(bytes("ctrl-c", LEGACY), b"\x03");
        assert_eq!(bytes("ctrl-d", LEGACY), b"\x04");
        assert_eq!(bytes("ctrl-space", LEGACY), b"\x00");
        assert_eq!(bytes("ctrl-[", LEGACY), b"\x1b");
        // Ctrl+Shift chords keep both modifiers for the encoder.
        assert_eq!(
            send("ctrl-shift-a"),
            (
                Key::Char('a'),
                Modifiers {
                    shift: true,
                    ctrl: true,
                    ..Modifiers::NONE
                }
            )
        );
    }

    #[test]
    fn option_as_meta() {
        assert_eq!(bytes("alt-b", LEGACY), b"\x1bb");
        assert_eq!(bytes("alt-f", LEGACY), b"\x1bf");
        // With Option-as-Alt off, Option+letter is the layout's character,
        // typed through the input handler.
        assert_eq!(classify(&ks("alt-b"), false), KeyAction::Text);
        // Named keys carry Alt either way.
        assert_eq!(
            classify(&ks("alt-left"), false),
            KeyAction::Send(Key::Left, Modifiers::ALT)
        );
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        assert_eq!(bytes("up", LEGACY), b"\x1b[A");
        let app = KeyEncoding {
            application_cursor_keys: true,
            ..LEGACY
        };
        assert_eq!(bytes("up", app), b"\x1bOA");
        assert_eq!(bytes("ctrl-left", LEGACY), b"\x1b[1;5D");
        assert_eq!(bytes("shift-right", LEGACY), b"\x1b[1;2C");
    }

    #[test]
    fn function_and_editing_keys() {
        assert_eq!(bytes("f1", LEGACY), b"\x1bOP");
        assert_eq!(bytes("f5", LEGACY), b"\x1b[15~");
        assert_eq!(bytes("f12", LEGACY), b"\x1b[24~");
        assert_eq!(bytes("escape", LEGACY), b"\x1b");
        assert_eq!(bytes("delete", LEGACY), b"\x1b[3~");
        assert_eq!(bytes("home", LEGACY), b"\x1b[H");
        assert_eq!(bytes("pageup", LEGACY), b"\x1b[5~");
        assert_eq!(classify(&ks("f30"), true), KeyAction::Ignore);
    }

    #[test]
    fn text_goes_to_the_input_handler() {
        assert_eq!(classify(&ks("a"), true), KeyAction::Text);
        assert_eq!(classify(&ks("shift-a"), true), KeyAction::Text);
        assert_eq!(classify(&ks("space"), true), KeyAction::Text);
        assert_eq!(classify(&ks("shift-space"), true), KeyAction::Text);
        assert_eq!(classify(&ks("1"), true), KeyAction::Text);
    }

    #[test]
    fn cmd_chords_are_the_apps_except_copy_and_paste() {
        assert_eq!(classify(&ks("cmd-c"), true), KeyAction::Copy);
        assert_eq!(classify(&ks("cmd-v"), true), KeyAction::Paste);
        assert_eq!(classify(&ks("cmd-w"), true), KeyAction::Ignore);
        assert_eq!(classify(&ks("cmd-q"), true), KeyAction::Ignore);
        assert_eq!(classify(&ks("cmd-shift-c"), true), KeyAction::Ignore);
        assert_eq!(classify(&ks("cmd-enter"), true), KeyAction::Ignore);
    }

    #[test]
    fn shift_paging_is_shelbis_scrollback() {
        assert_eq!(
            classify(&ks("shift-pageup"), true),
            KeyAction::Scroll(ScrollKey::PageUp)
        );
        assert_eq!(
            classify(&ks("shift-pagedown"), true),
            KeyAction::Scroll(ScrollKey::PageDown)
        );
        assert_eq!(
            classify(&ks("shift-home"), true),
            KeyAction::Scroll(ScrollKey::Top)
        );
        assert_eq!(
            classify(&ks("shift-end"), true),
            KeyAction::Scroll(ScrollKey::Bottom)
        );
        // Unshifted paging is the program's.
        assert_eq!(
            classify(&ks("pageup"), true),
            KeyAction::Send(Key::PageUp, Modifiers::NONE)
        );
        assert_eq!(ScrollKey::Top.key(), Key::Home);
    }
}
