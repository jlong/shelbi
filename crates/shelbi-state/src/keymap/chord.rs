//! Chord parsing — strings ↔ ([`Key`], [`Mods`]) pairs.
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

use super::key::{Key, Mods};

/// A single key chord — a [`Key`] plus the modifier set that was held when
/// it fired. Equality/hashing is straight off the two fields so a chord can
/// be used as the key in a binding map.
///
/// The code/mods are Shelbi's own toolkit-independent types; crossterm (or
/// any other input source) is converted to a chord at the edges, in
/// `shelbi-tui` and `shelbi-cli`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: Key,
    pub mods: Mods,
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
                    code: Key::Char(lower),
                    mods: Mods::SHIFT,
                }
            } else {
                KeyChord {
                    code: Key::Char(ch),
                    mods: Mods::NONE,
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
        let mut mods = Mods::NONE;
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

        // A trailing separator (`ctrl-`) leaves an empty keyname segment. That's
        // a dangling modifier, not a multi-key sequence — without this guard the
        // empty token reaches `parse_keyname`, whose `chars().all(..)` vacuously
        // succeeds on `""` and misreports it as `MultiKeyNotSupported("")`.
        if parts[idx].is_empty() {
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

    /// Build a chord from a raw ([`Key`], [`Mods`]) pair — what the edge
    /// crates (`shelbi-tui`, `shelbi-cli`) hand over after translating a
    /// crossterm `KeyEvent`. The code + mods are carried verbatim: no
    /// case-folding or implied-Shift normalization happens here. Terminals
    /// that report an uppercase `Char('A')` with no SHIFT bit are reconciled
    /// at lookup time by [`super::ModeKeymap::dispatch`], which retries the
    /// `shift-a` form before giving up.
    pub fn new(code: Key, mods: Mods) -> Self {
        KeyChord { code, mods }
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
        // An event-derived chord can carry an uppercase `Char('J')` (some
        // terminals report the shifted letter directly). Rendering it as the
        // bare `J` breaks the round-trip — `parse("J")` yields
        // `(Char('j'), SHIFT)`, a *different* chord. Normalize to the
        // documented `shift-j` form: lowercase the letter and treat the
        // uppercase as an implied Shift.
        let (code, implied_shift) = match self.code {
            Key::Char(c) if c.is_ascii_uppercase() => {
                (Key::Char(c.to_ascii_lowercase()), true)
            }
            other => (other, false),
        };
        let mut out = String::new();
        if self.mods.contains(Mods::CONTROL) {
            out.push_str("ctrl-");
        }
        if self.mods.contains(Mods::ALT) {
            out.push_str("alt-");
        }
        if implied_shift || self.mods.contains(Mods::SHIFT) {
            out.push_str("shift-");
        }
        if self.mods.contains(Mods::SUPER) {
            out.push_str("super-");
        }
        out.push_str(&keyname(code));
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

fn parse_modifier_opt(tok: &str) -> Option<Mods> {
    match tok.to_ascii_lowercase().as_str() {
        "ctrl" => Some(Mods::CONTROL),
        "alt" => Some(Mods::ALT),
        "shift" => Some(Mods::SHIFT),
        "super" => Some(Mods::SUPER),
        _ => None,
    }
}

/// Parse the keyname segment of a chord. Returns `(Key, implied_mods)`.
/// The only mod ever implied here is Shift, when the keyname is a single
/// uppercase letter.
fn parse_keyname(tok: &str) -> Result<(Key, Mods), ChordParseError> {
    // Single character key — case matters (uppercase → Shift).
    if tok.chars().count() == 1 {
        let ch = tok.chars().next().unwrap();
        if ch.is_ascii_uppercase() {
            return Ok((Key::Char(ch.to_ascii_lowercase()), Mods::SHIFT));
        }
        return Ok((Key::Char(ch), Mods::NONE));
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
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "enter" => Key::Enter,
        "space" => Key::Char(' '),
        "esc" => Key::Esc,
        "tab" => Key::Tab,
        "back-tab" => Key::BackTab,
        "backspace" => Key::Backspace,
        "delete" => Key::Delete,
        "insert" => Key::Insert,
        "home" => Key::Home,
        "end" => Key::End,
        "page-up" => Key::PageUp,
        "page-down" => Key::PageDown,
        // Function keys f1..f12.
        f if f.starts_with('f') && f.len() <= 3 => match f[1..].parse::<u8>() {
            Ok(n) if (1..=12).contains(&n) => Key::F(n),
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
    Ok((code, Mods::NONE))
}

/// Inverse of [`parse_keyname`]: render a [`Key`] back to its canonical
/// token. Every [`Key`] variant is in the chord vocabulary, so this is
/// total.
fn keyname(code: Key) -> String {
    match code {
        Key::Char(' ') => "space".to_string(),
        Key::Char(c) => c.to_string(),
        Key::Up => "up".to_string(),
        Key::Down => "down".to_string(),
        Key::Left => "left".to_string(),
        Key::Right => "right".to_string(),
        Key::Enter => "enter".to_string(),
        Key::Esc => "esc".to_string(),
        Key::Tab => "tab".to_string(),
        Key::BackTab => "back-tab".to_string(),
        Key::Backspace => "backspace".to_string(),
        Key::Delete => "delete".to_string(),
        Key::Insert => "insert".to_string(),
        Key::Home => "home".to_string(),
        Key::End => "end".to_string(),
        Key::PageUp => "page-up".to_string(),
        Key::PageDown => "page-down".to_string(),
        Key::F(n) => format!("f{n}"),
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
        assert_eq!(c.code, Key::Char('j'));
        assert_eq!(c.mods, Mods::NONE);
    }

    #[test]
    fn uppercase_char_implies_shift() {
        let c = parse("J");
        assert_eq!(c.code, Key::Char('j'));
        assert_eq!(c.mods, Mods::SHIFT);
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
            "up",
            "down",
            "left",
            "right",
            "enter",
            "space",
            "esc",
            "tab",
            "back-tab",
            "backspace",
            "delete",
            "insert",
            "home",
            "end",
            "page-up",
            "page-down",
        ] {
            let _ = parse(name);
        }
        for n in 1..=12 {
            let c = parse(&format!("f{n}"));
            assert_eq!(c.code, Key::F(n));
        }
    }

    #[test]
    fn parses_every_modifier() {
        assert!(parse("ctrl-x").mods.contains(Mods::CONTROL));
        assert!(parse("alt-x").mods.contains(Mods::ALT));
        assert!(parse("shift-x").mods.contains(Mods::SHIFT));
        assert!(parse("super-x").mods.contains(Mods::SUPER));
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
        assert!(matches!(
            KeyChord::parse("").unwrap_err(),
            ChordParseError::Empty
        ));
        assert!(matches!(
            KeyChord::parse("   ").unwrap_err(),
            ChordParseError::Empty
        ));
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
            "j",
            "shift-j",
            "ctrl-c",
            "alt-z",
            "ctrl-p",
            "up",
            "down",
            "left",
            "right",
            "enter",
            "space",
            "esc",
            "tab",
            "back-tab",
            "backspace",
            "delete",
            "insert",
            "home",
            "end",
            "page-up",
            "page-down",
            "f1",
            "f12",
            "shift-up",
            "shift-down",
            "ctrl-alt-shift-x",
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
        assert_eq!(c.code, Key::Char('-'));
        let c = parse("ctrl--");
        assert_eq!(c.code, Key::Char('-'));
        assert!(c.mods.contains(Mods::CONTROL));
    }

    #[test]
    fn trailing_separator_is_a_missing_keyname_not_multi_key() {
        // `ctrl-` is a dangling modifier — report it as an unknown/missing
        // keyname naming the whole input, not `MultiKeyNotSupported("")`.
        let err = KeyChord::parse("ctrl-").unwrap_err();
        assert_eq!(err, ChordParseError::UnknownKey("ctrl-".to_string()));
        let err = KeyChord::parse("ctrl-alt-").unwrap_err();
        assert_eq!(err, ChordParseError::UnknownKey("ctrl-alt-".to_string()));
    }

    #[test]
    fn canonical_round_trips_uppercase_char_from_event() {
        // A terminal can hand us `Char('J')` with the SHIFT bit already set.
        // `canonical()` must normalize to `shift-j` so the round-trip holds.
        let c = KeyChord::new(Key::Char('J'), Mods::SHIFT);
        assert_eq!(c.canonical(), "shift-j");
        assert_eq!(parse(&c.canonical()), parse("shift-j"));

        // And with no SHIFT bit (terminals that report only the glyph): the
        // canonical form still parses back to an equal chord.
        let c = KeyChord::new(Key::Char('J'), Mods::NONE);
        assert_eq!(c.canonical(), "shift-j");
        assert_eq!(parse(&c.canonical()), parse(&c.canonical()));
    }

    #[test]
    fn new_preserves_code_and_mods() {
        let c = KeyChord::new(Key::Char('x'), Mods::CONTROL);
        assert_eq!(c.code, Key::Char('x'));
        assert!(c.mods.contains(Mods::CONTROL));
    }








}
