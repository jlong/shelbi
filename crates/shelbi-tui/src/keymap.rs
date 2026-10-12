//! Chord matching for the Zen Mode toggle hotkey.
//!
//! The user picks the chord via the first-run probe (saved to
//! `~/.shelbi/config.yaml::keymap.zen_toggle`). At runtime each `KeyEvent`
//! is tested against [`matches_zen_toggle`] before any other binding —
//! Alt+Z, Ctrl+G, etc. must take priority over `g` / `z` as nav keys.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shelbi_state::keymap::{format_chord, DisplayStyle, Key, KeyChord, Mods};

/// Translate a crossterm [`KeyEvent`] into Shelbi's toolkit-independent
/// [`KeyChord`]. This is the crossterm→Shelbi edge for the TUI: the keymap
/// layer in `shelbi-state` no longer knows about crossterm, so callers
/// convert here before dispatching.
///
/// Returns `None` for keys outside Shelbi's chord vocabulary (exotic media
/// and lock keys). Such keys can never be bound, so a `None` means "no
/// chord" — the same outcome the old crossterm-keyed dispatch produced when
/// the event wasn't present in the bindings map.
pub fn chord_from_event(ev: KeyEvent) -> Option<KeyChord> {
    Some(KeyChord::new(key_from_crossterm(ev.code)?, mods_from(ev.modifiers)))
}

/// Map a crossterm [`KeyCode`] to a Shelbi [`Key`]. `None` for codes outside
/// the chord vocabulary.
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

/// Map crossterm [`KeyModifiers`] to Shelbi [`Mods`]. Modifiers Shelbi
/// doesn't model (HYPER, META) are dropped.
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



