//! Chord parsing — strings ↔ ([`KeyCode`], [`KeyModifiers`]) pairs.
//!
//! Grammar (single chord only — multi-key sequences like `gg` or
//! `ctrl-x-ctrl-c` are deliberately rejected as out of scope):
//!
//! ```text
//! chord     := (modifier '-')* keyname
//! modifier  := ctrl | alt | shift | super
//! keyname   := single character | named-key
//! named-key := up | down | left | right | enter | space | esc | tab
//!            | back-tab | backspace | delete | insert | home | end
//!            | page-up | page-down | f1..f12
//! ```
//!
//! Lowercase keynames required (`Up` would be a parse error). Single
//! character keynames may be either case: `J` parses identically to
//! `shift-j`. The canonical form always normalizes to `shift-j`.
//!
//! Modifier order is normalized in canonical form to
//! `ctrl-alt-shift-super-`. Input may list them in any order.

use std::fmt;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single key chord — a `KeyCode` plus the modifier set that was held
/// when it fired. Equality/hashing is straight off the two fields so a
/// chord can be used as the key in a binding map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

/// Reasons the parser rejects an input string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChordParseError {
    #[error("empty chord string")]
    Empty,
    #[error("unknown keyname `{0}`")]
    UnknownKey(String),
    #[error("duplicate modifier `{0}`")]
    DuplicateModifier(String),
    /// Returned for inputs like `gg`, `dap`, or `ctrl-x-ctrl-c` — anything
    /// that names two keys back to back. Single character keys with a
    /// modifier (`ctrl-x`) are NOT rejected here; only `<key>-<key>`.
    #[error("multi-key sequences (`{0}`) are not supported")]
    MultiKeyNotSupported(String),
}

impl KeyChord {
    /// Parse a chord string into a [`KeyChord`]. See module docs for the
    /// grammar. Whitespace around the input is trimmed; ASCII case folding
    /// applies to modifier names but NOT to keyname characters — `K` is
    /// `shift-k`, not `k`.
    pub fn parse(s: &str) -> Result<Self, ChordParseError> {
        let raw = s.trim();
        if raw.is_empty() {
            return Err(ChordParseError::Empty);
        }

        // Single character "fast path" — skips the dash splitter so we
        // don't choke on `-` as a key (`-` is its own valid keyname).
        // A bare uppercase letter implies Shift.
        if raw.chars().count() == 1 {
            let ch = raw.chars().next().unwrap();
            return Ok(if ch.is_ascii_uppercase() {
                let lower = ch.to_ascii_lowercase();
                KeyChord {
                    code: KeyCode::Char(lower),
                    mods: KeyModifiers::SHIFT,
                }
            } else {
                KeyChord {
                    code: KeyCode::Char(ch),
                    mods: KeyModifiers::NONE,
                }
            });
        }

        // Split on `-`, preserving a trailing literal `-` as the keyname.
        // `ctrl--` → ["ctrl", "-"]; `page-up` → ["page", "up"].
        let parts = split_chord(raw);
        if parts.is_empty() {
            return Err(ChordParseError::Empty);
        }

        // Collect modifiers left-to-right until the first non-modifier
        // segment; the rest is the keyname (which may be a compound like
        // `page-up` spanning two segments).
        let mut mods = KeyModifiers::NONE;
        let mut idx = 0usize;
        while idx < parts.len() {
            match parse_modifier_opt(parts[idx]) {
                Some(bit) => {
                    if mods.contains(bit) {
                        return Err(ChordParseError::DuplicateModifier(
                            parts[idx].to_ascii_lowercase(),
                        ));
                    }
                    mods |= bit;
                    idx += 1;
                }
                None => break,
            }
        }

        if idx >= parts.len() {
            // All segments parsed as modifiers — no keyname supplied.
            return Err(ChordParseError::UnknownKey(raw.to_string()));
        }

        // Try compound keynames first (`page-up`, `back-tab`, `page-down`)
        // so the trailing 2 segments are consumed together. If that
        // fails, fall back to a single-segment keyname.
        let (code, key_mods, consumed) = if idx + 1 < parts.len() {
            let compound = format!("{}-{}", parts[idx], parts[idx + 1]);
            match parse_keyname(&compound) {
                Ok((c, m)) => (c, m, 2),
                Err(_) => {
                    let (c, m) = parse_keyname(parts[idx])?;
                    (c, m, 1)
                }
            }
        } else {
            let (c, m) = parse_keyname(parts[idx])?;
            (c, m, 1)
        };

        let remaining = &parts[idx + consumed..];
        if !remaining.is_empty() {
            return Err(ChordParseError::MultiKeyNotSupported(raw.to_string()));
        }

        Ok(KeyChord {
            code,
            mods: mods | key_mods,
        })
    }

    /// Build a chord from a crossterm `KeyEvent` so the runtime can look
    /// it up in a [`super::ModeKeymap`]. Drops the `KIND` etc.; just the
    /// code + mods are relevant for dispatch. Empty/superfluous Shift on
    /// `Char` events is left intact — the lookup map keeps both forms.
    pub fn from_event(ev: KeyEvent) -> Self {
        KeyChord {
            code: ev.code,
            mods: ev.modifiers,
        }
    }

    /// Render this chord in the canonical, lossless string form. Round-
    /// trips through [`KeyChord::parse`]:
    ///
    /// ```text
    /// parse(canonical(parse(s)?)?)?  == parse(s)?
    /// ```
    ///
    /// Modifier order: ctrl, alt, shift, super. Uppercase-letter chords
    /// emit as `shift-x`, not `X`.
    pub fn canonical(&self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("ctrl-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push_str("alt-");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            out.push_str("shift-");
        }
        if self.mods.contains(KeyModifiers::SUPER) {
            out.push_str("super-");
        }
        out.push_str(&keyname(self.code));
        out
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.canonical())
    }
}

/// Split `ctrl-alt-x` → `["ctrl", "alt", "x"]`. When the input ends with
/// a literal `-` keyname (e.g. `ctrl--`), the trailing `-` is preserved
/// as the final segment instead of emitting an empty token.
fn split_chord(s: &str) -> Vec<&str> {
    // Trailing `-` keyname is signaled by the input ending with `--`
    // (the separator dash followed by the literal-dash keyname). Detach
    // both characters, split what's left, then push the literal dash.
    if let Some(prefix) = s.strip_suffix("--") {
        let mut parts: Vec<&str> = if prefix.is_empty() {
            Vec::new()
        } else {
            prefix.split('-').collect()
        };
        parts.push(&s[s.len() - 1..]);
        return parts;
    }
    s.split('-').collect()
}

fn parse_modifier_opt(tok: &str) -> Option<KeyModifiers> {
    match tok.to_ascii_lowercase().as_str() {
        "ctrl" => Some(KeyModifiers::CONTROL),
        "alt" => Some(KeyModifiers::ALT),
        "shift" => Some(KeyModifiers::SHIFT),
        "super" => Some(KeyModifiers::SUPER),
        _ => None,
    }
}

/// Parse the keyname segment of a chord. Returns `(KeyCode, implied_mods)`.
/// The only mod ever implied here is Shift, when the keyname is a single
/// uppercase letter.
fn parse_keyname(tok: &str) -> Result<(KeyCode, KeyModifiers), ChordParseError> {
    // Single character key — case matters (uppercase → Shift).
    if tok.chars().count() == 1 {
        let ch = tok.chars().next().unwrap();
        if ch.is_ascii_uppercase() {
            return Ok((KeyCode::Char(ch.to_ascii_lowercase()), KeyModifiers::SHIFT));
        }
        return Ok((KeyCode::Char(ch), KeyModifiers::NONE));
    }

    // Multi-char keyname — must be one of the named keys. Lowercase only.
    if tok.chars().any(|c| c.is_ascii_uppercase()) {
        // Spotting an uppercase here usually means the user typed `Up` or
        // `Enter`. Reject with a hint rather than treating it as multi-key.
        return Err(ChordParseError::UnknownKey(tok.to_string()));
    }

    // Reject multi-character "words" that aren't named keys. They almost
    // always come from misuse like `gg` (multi-key sequence) — surface that
    // distinct error so the user knows why it failed.
    let code = match tok {
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "enter" => KeyCode::Enter,
        "space" => KeyCode::Char(' '),
        "esc" => KeyCode::Esc,
        "tab" => KeyCode::Tab,
        "back-tab" => KeyCode::BackTab,
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "page-up" => KeyCode::PageUp,
        "page-down" => KeyCode::PageDown,
        // Function keys f1..f12.
        f if f.starts_with('f') && f.len() <= 3 => match f[1..].parse::<u8>() {
            Ok(n) if (1..=12).contains(&n) => KeyCode::F(n),
            _ => return Err(ChordParseError::UnknownKey(tok.to_string())),
        },
        _ => {
            // Looks like a bare word that isn't a named key. Most likely a
            // multi-key sequence like `gg` / `dap`. Emit the dedicated error.
            if tok.chars().all(|c| c.is_ascii_alphabetic()) {
                return Err(ChordParseError::MultiKeyNotSupported(tok.to_string()));
            }
            return Err(ChordParseError::UnknownKey(tok.to_string()));
        }
    };
    Ok((code, KeyModifiers::NONE))
}

/// Inverse of [`parse_keyname`]: render a [`KeyCode`] back to its
/// canonical token. Unknown / unsupported codes fall through to a `?`
/// marker; callers that surface this should map it to a parse error so
/// the bad value can't silently survive a round-trip.
fn keyname(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "space".to_string(),
        KeyCode::Char(c) => c.to_string(),
        KeyCode::Up => "up".to_string(),
        KeyCode::Down => "down".to_string(),
        KeyCode::Left => "left".to_string(),
        KeyCode::Right => "right".to_string(),
        KeyCode::Enter => "enter".to_string(),
        KeyCode::Esc => "esc".to_string(),
        KeyCode::Tab => "tab".to_string(),
        KeyCode::BackTab => "back-tab".to_string(),
        KeyCode::Backspace => "backspace".to_string(),
        KeyCode::Delete => "delete".to_string(),
        KeyCode::Insert => "insert".to_string(),
        KeyCode::Home => "home".to_string(),
        KeyCode::End => "end".to_string(),
        KeyCode::PageUp => "page-up".to_string(),
        KeyCode::PageDown => "page-down".to_string(),
        KeyCode::F(n) => format!("f{n}"),
        other => format!("?{other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(s: &str) -> KeyChord {
        KeyChord::parse(s).unwrap_or_else(|e| panic!("parse {s:?} failed: {e}"))
    }

    #[test]
    fn parses_plain_char() {
        let c = parse("j");
        assert_eq!(c.code, KeyCode::Char('j'));
        assert_eq!(c.mods, KeyModifiers::NONE);
    }

    #[test]
    fn uppercase_char_implies_shift() {
        let c = parse("J");
        assert_eq!(c.code, KeyCode::Char('j'));
        assert_eq!(c.mods, KeyModifiers::SHIFT);
        assert_eq!(c.canonical(), "shift-j");
    }

    #[test]
    fn shift_letter_canonicalizes_to_lowercase_form() {
        let a = parse("J");
        let b = parse("shift-j");
        assert_eq!(a, b);
        assert_eq!(a.canonical(), b.canonical());
    }

    #[test]
    fn modifier_order_normalizes() {
        let a = parse("alt-ctrl-shift-x");
        let b = parse("ctrl-alt-shift-x");
        let c = parse("shift-ctrl-alt-x");
        assert_eq!(a, b);
        assert_eq!(b, c);
        assert_eq!(a.canonical(), "ctrl-alt-shift-x");
    }

    #[test]
    fn parses_every_named_key() {
        for name in [
            "up", "down", "left", "right", "enter", "space", "esc", "tab",
            "back-tab", "backspace", "delete", "insert", "home", "end",
            "page-up", "page-down",
        ] {
            let _ = parse(name);
        }
        for n in 1..=12 {
            let c = parse(&format!("f{n}"));
            assert_eq!(c.code, KeyCode::F(n));
        }
    }

    #[test]
    fn parses_every_modifier() {
        assert!(parse("ctrl-x").mods.contains(KeyModifiers::CONTROL));
        assert!(parse("alt-x").mods.contains(KeyModifiers::ALT));
        assert!(parse("shift-x").mods.contains(KeyModifiers::SHIFT));
        assert!(parse("super-x").mods.contains(KeyModifiers::SUPER));
    }

    #[test]
    fn rejects_multi_key_sequence() {
        for s in ["gg", "dap", "ctrl-x-ctrl-c"] {
            assert!(
                matches!(
                    KeyChord::parse(s).unwrap_err(),
                    ChordParseError::MultiKeyNotSupported(_)
                ),
                "{s} should reject as multi-key"
            );
        }
    }

    #[test]
    fn rejects_empty_input() {
        assert!(matches!(KeyChord::parse("").unwrap_err(), ChordParseError::Empty));
        assert!(matches!(KeyChord::parse("   ").unwrap_err(), ChordParseError::Empty));
    }

    #[test]
    fn rejects_uppercase_named_key() {
        // Per the grammar `Up` is invalid — keynames are lowercase.
        let err = KeyChord::parse("Up").unwrap_err();
        assert!(matches!(err, ChordParseError::UnknownKey(_)));
    }

    #[test]
    fn rejects_duplicate_modifier() {
        let err = KeyChord::parse("ctrl-ctrl-x").unwrap_err();
        assert!(matches!(err, ChordParseError::DuplicateModifier(_)));
    }

    #[test]
    fn round_trips_canonical_form() {
        // Every default chord we install must survive a parse→canonical→parse
        // round trip identically.
        let samples = [
            "j", "shift-j", "ctrl-c", "alt-z", "ctrl-p", "up", "down", "left",
            "right", "enter", "space", "esc", "tab", "back-tab", "backspace",
            "delete", "insert", "home", "end", "page-up", "page-down", "f1",
            "f12", "shift-up", "shift-down", "ctrl-alt-shift-x",
        ];
        for s in samples {
            let a = parse(s);
            let canon = a.canonical();
            let b = parse(&canon);
            assert_eq!(a, b, "round trip broken: {s} → {canon}");
            // Canonical form is itself a fixed point.
            assert_eq!(canon, b.canonical(), "canonical not idempotent for {s}");
        }
    }

    #[test]
    fn parses_dash_as_keyname() {
        let c = parse("-");
        assert_eq!(c.code, KeyCode::Char('-'));
        let c = parse("ctrl--");
        assert_eq!(c.code, KeyCode::Char('-'));
        assert!(c.mods.contains(KeyModifiers::CONTROL));
    }

    #[test]
    fn from_event_preserves_code_and_mods() {
        let ev = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL);
        let c = KeyChord::from_event(ev);
        assert_eq!(c.code, KeyCode::Char('x'));
        assert!(c.mods.contains(KeyModifiers::CONTROL));
    }
}
