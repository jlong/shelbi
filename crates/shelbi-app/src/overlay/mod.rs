//! The overlays' state machines, shared by every client.
//!
//! The command palette, the error log, the review-load dialog, the
//! reject-reason prompt, the first-run Zen intro, and the add-project form are
//! each a small state machine: keys in, a step out. They live here, free of
//! any UI toolkit, so the TUI (`shelbi-tui/src/overlay`) and the desktop app
//! drive the same decision tables and differ only in how they paint.
//!
//! Every machine takes keys as a [`KeyChord`], the toolkit-independent chord
//! both clients already convert their native key events to (crossterm's in
//! the TUI, gpui's in the desktop). A typed character arrives as
//! `Key::Char(c)` with at most Shift held ([`typed_char`]); geometry (what a
//! click lands on) stays with the renderer, which calls the machines'
//! click-level methods (`click_item`, `click_load`, ...) once it has hit-tested.

pub mod add_project;
pub mod error_log;
pub mod palette;
pub mod review_confirm;
pub mod review_reject;
pub mod zen_intro;

use shelbi_state::keymap::{GlobalAction, Key, KeyChord, Keymaps, Mods};

/// The character a chord types into a text field: a `Char` with no modifier
/// other than Shift (an uppercase letter may arrive as `Char('A')` with Shift
/// held, or without it). `None` for anything else, including Ctrl/Alt/Super
/// chords, which are commands rather than text.
pub fn typed_char(chord: &KeyChord) -> Option<char> {
    match chord.code {
        Key::Char(c) if chord.mods.is_empty() || chord.mods == Mods::SHIFT => Some(c),
        _ => None,
    }
}

/// Whether `chord` is exactly `key` with no modifiers held.
pub(crate) fn is_bare(chord: &KeyChord, key: Key) -> bool {
    chord.code == key && chord.mods.is_empty()
}

/// Ctrl+Space: the palette chord every client reserves, whatever `keys.yaml`
/// binds (many terminals deliver it as Ctrl+@, which the TUI also accepts).
pub const RESERVED_PALETTE_CHORD: KeyChord = KeyChord {
    code: Key::Char(' '),
    mods: Mods::CONTROL,
};

/// Cmd+K: the desktop's conventional palette chord. A terminal never delivers
/// Super, so it only ever reaches the desktop app.
pub const PLATFORM_PALETTE_CHORD: KeyChord = KeyChord {
    code: Key::Char('k'),
    mods: Mods::SUPER,
};

/// Every chord that opens the palette: the user's `open_palette` bindings
/// (`keys.yaml`, default Ctrl+P) plus the two reserved chords. The configured
/// chords come first, so the first entry is the one to show as a hint.
pub fn palette_open_chords(keymaps: &Keymaps) -> Vec<KeyChord> {
    let mut out: Vec<KeyChord> = keymaps
        .global
        .chords_for(GlobalAction::OpenPalette)
        .into_iter()
        .copied()
        .collect();
    for reserved in [RESERVED_PALETTE_CHORD, PLATFORM_PALETTE_CHORD] {
        if !out.contains(&reserved) {
            out.push(reserved);
        }
    }
    out
}

/// Whether `chord` opens the palette under `keymaps` (see
/// [`palette_open_chords`]).
pub fn opens_palette(keymaps: &Keymaps, chord: &KeyChord) -> bool {
    palette_open_chords(keymaps).contains(chord)
}

#[cfg(test)]
pub(crate) mod test_keys {
    //! Chord constructors for the machines' tests.
    use shelbi_state::keymap::{Key, KeyChord, Mods};

    pub fn key(code: Key) -> KeyChord {
        KeyChord::new(code, Mods::NONE)
    }

    pub fn ch(c: char) -> KeyChord {
        KeyChord::new(Key::Char(c), Mods::NONE)
    }

    pub fn ctrl(c: char) -> KeyChord {
        KeyChord::new(Key::Char(c), Mods::CONTROL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_state::keymap::validate_keymaps_yaml;

    fn chord(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap()
    }

    #[test]
    fn typed_char_accepts_bare_and_shifted_characters_only() {
        assert_eq!(typed_char(&chord("a")), Some('a'));
        assert_eq!(
            typed_char(&KeyChord::new(Key::Char('A'), Mods::SHIFT)),
            Some('A')
        );
        assert_eq!(typed_char(&chord("ctrl-a")), None);
        assert_eq!(typed_char(&chord("alt-a")), None);
        assert_eq!(typed_char(&chord("super-a")), None);
        assert_eq!(typed_char(&chord("enter")), None);
    }

    #[test]
    fn the_palette_opens_on_the_default_binding_and_both_reserved_chords() {
        let (keymaps, diags) = validate_keymaps_yaml("", None);
        assert!(diags.is_empty());
        let chords: Vec<String> = palette_open_chords(&keymaps)
            .iter()
            .map(KeyChord::canonical)
            .collect();
        assert_eq!(chords, ["ctrl-p", "ctrl-space", "super-k"]);
        assert!(opens_palette(&keymaps, &chord("ctrl-space")));
        assert!(opens_palette(&keymaps, &chord("super-k")));
        assert!(!opens_palette(&keymaps, &chord("ctrl-k")));
    }

    #[test]
    fn a_keys_yaml_rebinding_replaces_the_default_but_not_the_reserved_chords() {
        let yaml = "defaults:\n  global:\n    open_palette: ctrl-o\n";
        let (keymaps, diags) = validate_keymaps_yaml(yaml, None);
        assert!(diags.is_empty(), "{diags:?}");
        assert!(opens_palette(&keymaps, &chord("ctrl-o")));
        assert!(!opens_palette(&keymaps, &chord("ctrl-p")), "the default is replaced");
        assert!(opens_palette(&keymaps, &chord("ctrl-space")));
        assert!(opens_palette(&keymaps, &chord("super-k")));
        assert_eq!(palette_open_chords(&keymaps)[0], chord("ctrl-o"), "the hint shows the user's chord");
    }
}
