//! The crossterm→Shelbi key edge for the CLI's interactive screens
//! (palette, add-project form, confirm popovers).
//!
//! `shelbi-state`'s keymap layer is toolkit-independent — it dispatches on
//! Shelbi's own [`KeyChord`] rather than a crossterm `KeyEvent`. The CLI's
//! event loops read crossterm events and convert them here before
//! dispatching, mirroring `shelbi-tui`'s edge.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shelbi_state::keymap::{Key, KeyChord, Mods};

/// Translate a crossterm [`KeyEvent`] into Shelbi's [`KeyChord`]. Returns
/// `None` for keys outside Shelbi's chord vocabulary (exotic media and lock
/// keys), which can never be bound and so dispatch to nothing.
pub fn chord_from_event(ev: KeyEvent) -> Option<KeyChord> {
    Some(KeyChord::new(key_from_crossterm(ev.code)?, mods_from(ev.modifiers)))
}

fn key_from_crossterm(code: KeyCode) -> Option<Key> {
    Some(match code {
        KeyCode::Char(c) => Key::Char(c),
        KeyCode::Up => Key::Up,
        KeyCode::Down => Key::Down,
        KeyCode::Left => Key::Left,
        KeyCode::Right => Key::Right,
        KeyCode::Enter => Key::Enter,
        KeyCode::Esc => Key::Esc,
        KeyCode::Tab => Key::Tab,
        KeyCode::BackTab => Key::BackTab,
        KeyCode::Backspace => Key::Backspace,
        KeyCode::Delete => Key::Delete,
        KeyCode::Insert => Key::Insert,
        KeyCode::Home => Key::Home,
        KeyCode::End => Key::End,
        KeyCode::PageUp => Key::PageUp,
        KeyCode::PageDown => Key::PageDown,
        KeyCode::F(n) => Key::F(n),
        _ => return None,
    })
}

fn mods_from(m: KeyModifiers) -> Mods {
    let mut out = Mods::NONE;
    if m.contains(KeyModifiers::CONTROL) {
        out |= Mods::CONTROL;
    }
    if m.contains(KeyModifiers::ALT) {
        out |= Mods::ALT;
    }
    if m.contains(KeyModifiers::SHIFT) {
        out |= Mods::SHIFT;
    }
    if m.contains(KeyModifiers::SUPER) {
        out |= Mods::SUPER;
    }
    out
}
