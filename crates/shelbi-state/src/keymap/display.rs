//! Human-facing chord rendering for help footers.
//!
//! [`format_chord`] turns a [`KeyChord`] into the string shown in the
//! TUI's hint rows, in the host platform's native convention. macOS uses
//! the compact symbol stack you'd see in a menu (`⌃P`, `⌥Z`, `⇧↑`, `⏎`);
//! every other platform spells the modifiers out and joins them with `+`
//! (`Ctrl+P`, `Alt+Z`, `Shift+Up`, `Enter`).
//!
//! The rendering is lossy by design — it's for humans, not round-tripping.
//! For the canonical, parseable form use [`KeyChord::canonical`].

use crossterm::event::{KeyCode, KeyModifiers};

use super::chord::KeyChord;

/// Which platform convention to render chords in. Detected once at
/// startup with [`DisplayStyle::detect`] and cached — `format_chord` is
/// called per frame, but the detection is not.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum DisplayStyle {
    /// Compact menu-style symbols, no separators: `⌃⇧Space`.
    Mac,
    /// Spelled-out modifiers joined with `+`: `Ctrl+Shift+Space`.
    Linux,
}

impl DisplayStyle {
    /// `Mac` when compiled for macOS, `Linux` everywhere else. This is a
    /// compile-time `cfg!`, so it's effectively free — but callers should
    /// still cache the result on their `App` rather than calling it per
    /// frame, per the help-render contract.
    pub fn detect() -> Self {
        if cfg!(target_os = "macos") {
            Self::Mac
        } else {
            Self::Linux
        }
    }
}

/// Render `chord` for display in the host platform's convention.
///
/// Modifiers render in `ctrl, alt, shift, super` order. On `Mac` the
/// modifier symbols and the key concatenate with no separator
/// (`⌃⌥⇧Z`); on `Linux` each modifier is a `Word+` prefix
/// (`Ctrl+Alt+Shift+Z`).
///
/// Letter keys carrying a ctrl/alt/super modifier uppercase (`ctrl-p` →
/// `⌃P`, `ctrl-alt-shift-z` → `⌃⌥⇧Z`), matching the menu-shortcut
/// convention. A bare letter renders in its natural (lowercase) case
/// (`q` → `q`). A *shift-only* letter stays lowercase (`shift-j` → `⇧j`)
/// since the `⇧` glyph already conveys the shift — no point doubling up.
pub fn format_chord(chord: &KeyChord, style: DisplayStyle) -> String {
    let mut out = String::new();
    let m = chord.mods;
    if m.contains(KeyModifiers::CONTROL) {
        out.push_str(match style {
            DisplayStyle::Mac => "⌃",
            DisplayStyle::Linux => "Ctrl+",
        });
    }
    if m.contains(KeyModifiers::ALT) {
        out.push_str(match style {
            DisplayStyle::Mac => "⌥",
            DisplayStyle::Linux => "Alt+",
        });
    }
    if m.contains(KeyModifiers::SHIFT) {
        out.push_str(match style {
            DisplayStyle::Mac => "⇧",
            DisplayStyle::Linux => "Shift+",
        });
    }
    if m.contains(KeyModifiers::SUPER) {
        out.push_str(match style {
            DisplayStyle::Mac => "⌘",
            DisplayStyle::Linux => "Super+",
        });
    }
    out.push_str(&format_key(chord.code, m, style));
    out
}

/// Render just the keyname portion (no modifiers). Char keys apply the
/// case rule; named keys use the platform's glyph/word from the table.
fn format_key(code: KeyCode, mods: KeyModifiers, style: DisplayStyle) -> String {
    match code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(c) => format_char(c, mods),
        KeyCode::Up => sym(style, "↑", "Up"),
        KeyCode::Down => sym(style, "↓", "Down"),
        KeyCode::Left => sym(style, "←", "Left"),
        KeyCode::Right => sym(style, "→", "Right"),
        KeyCode::Enter => sym(style, "⏎", "Enter"),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Backspace => sym(style, "⌫", "Backspace"),
        KeyCode::Delete => sym(style, "⌦", "Delete"),
        KeyCode::Tab => sym(style, "⇥", "Tab"),
        KeyCode::BackTab => sym(style, "⇤", "BackTab"),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::Home => sym(style, "↖", "Home"),
        KeyCode::End => sym(style, "↘", "End"),
        KeyCode::PageUp => sym(style, "⇞", "PageUp"),
        KeyCode::PageDown => sym(style, "⇟", "PageDown"),
        KeyCode::F(n) => format!("F{n}"),
        // Anything outside our chord vocabulary (media keys etc.) falls
        // back to crossterm's debug form rather than panicking.
        other => format!("{other:?}"),
    }
}

/// Apply the letter-case rule for a single `Char` key. See
/// [`format_chord`] for the rationale.
fn format_char(c: char, mods: KeyModifiers) -> String {
    if c.is_ascii_alphabetic() {
        if mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER) {
            // Menu-shortcut convention: ⌃P, ⌥Z, Ctrl+C, ⌃⌥⇧Z.
            c.to_ascii_uppercase().to_string()
        } else if mods.contains(KeyModifiers::SHIFT) {
            // Shift glyph already carries the case — don't double up.
            c.to_ascii_lowercase().to_string()
        } else {
            c.to_string()
        }
    } else {
        c.to_string()
    }
}

/// Pick the Mac glyph or the Linux word for a named key.
fn sym(style: DisplayStyle, mac: &str, linux: &str) -> String {
    match style {
        DisplayStyle::Mac => mac.to_string(),
        DisplayStyle::Linux => linux.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap_or_else(|e| panic!("parse {s:?} failed: {e}"))
    }

    fn fmt(s: &str, style: DisplayStyle) -> String {
        format_chord(&parse(s), style)
    }

    /// Every row of the spec's example matrix, both styles.
    #[test]
    fn example_matrix() {
        let rows: &[(&str, &str, &str)] = &[
            // (chord, mac, linux)
            ("ctrl-p", "⌃P", "Ctrl+P"),
            ("alt-z", "⌥Z", "Alt+Z"),
            ("shift-up", "⇧↑", "Shift+Up"),
            ("q", "q", "q"),
            ("enter", "⏎", "Enter"),
            ("ctrl-c", "⌃C", "Ctrl+C"),
            ("ctrl-shift-space", "⌃⇧Space", "Ctrl+Shift+Space"),
            ("f1", "F1", "F1"),
        ];
        for (chord, mac, linux) in rows {
            assert_eq!(&fmt(chord, DisplayStyle::Mac), mac, "mac {chord}");
            assert_eq!(&fmt(chord, DisplayStyle::Linux), linux, "linux {chord}");
        }
    }

    #[test]
    fn three_modifier_stack_concatenates_in_order() {
        assert_eq!(fmt("ctrl-alt-shift-z", DisplayStyle::Mac), "⌃⌥⇧Z");
        assert_eq!(
            fmt("ctrl-alt-shift-z", DisplayStyle::Linux),
            "Ctrl+Alt+Shift+Z"
        );
        // Order is normalized regardless of input ordering.
        assert_eq!(fmt("shift-alt-ctrl-z", DisplayStyle::Mac), "⌃⌥⇧Z");
    }

    #[test]
    fn super_modifier_renders() {
        assert_eq!(fmt("super-x", DisplayStyle::Mac), "⌘X");
        assert_eq!(fmt("super-x", DisplayStyle::Linux), "Super+X");
    }

    #[test]
    fn bare_letter_keeps_natural_case() {
        assert_eq!(fmt("q", DisplayStyle::Mac), "q");
        assert_eq!(fmt("j", DisplayStyle::Linux), "j");
    }

    #[test]
    fn shift_letter_does_not_double_up_case() {
        // ⇧ carries the shift; the letter stays lowercase rather than
        // also capitalizing.
        assert_eq!(fmt("shift-j", DisplayStyle::Mac), "⇧j");
        assert_eq!(fmt("J", DisplayStyle::Linux), "Shift+j");
    }

    #[test]
    fn every_named_key_both_styles() {
        let rows: &[(&str, &str, &str)] = &[
            ("up", "↑", "Up"),
            ("down", "↓", "Down"),
            ("left", "←", "Left"),
            ("right", "→", "Right"),
            ("enter", "⏎", "Enter"),
            ("space", "Space", "Space"),
            ("esc", "Esc", "Esc"),
            ("backspace", "⌫", "Backspace"),
            ("delete", "⌦", "Delete"),
            ("tab", "⇥", "Tab"),
            ("back-tab", "⇤", "BackTab"),
            ("insert", "Insert", "Insert"),
            ("home", "↖", "Home"),
            ("end", "↘", "End"),
            ("page-up", "⇞", "PageUp"),
            ("page-down", "⇟", "PageDown"),
        ];
        for (chord, mac, linux) in rows {
            assert_eq!(&fmt(chord, DisplayStyle::Mac), mac, "mac {chord}");
            assert_eq!(&fmt(chord, DisplayStyle::Linux), linux, "linux {chord}");
        }
        for n in 1..=12 {
            let c = format!("f{n}");
            let want = format!("F{n}");
            assert_eq!(fmt(&c, DisplayStyle::Mac), want);
            assert_eq!(fmt(&c, DisplayStyle::Linux), want);
        }
    }

    #[test]
    fn detect_matches_compile_target() {
        let got = DisplayStyle::detect();
        if cfg!(target_os = "macos") {
            assert_eq!(got, DisplayStyle::Mac);
        } else {
            assert_eq!(got, DisplayStyle::Linux);
        }
    }
}
