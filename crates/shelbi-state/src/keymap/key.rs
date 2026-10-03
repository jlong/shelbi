//! Shelbi's own key and modifier types, independent of any terminal
//! toolkit.
//!
//! [`KeyChord`](super::chord::KeyChord) is built from these. Keeping them
//! here means `shelbi-state` (and everything that reads keymaps, including
//! `shelbi-app`) has no crossterm dependency. The crossterm event types are
//! converted to and from [`Key`] / [`Mods`] only at the edges, in
//! `shelbi-tui` and `shelbi-cli`.
//!
//! The variant names deliberately mirror crossterm's `KeyCode` / the four
//! modifier bits in `KeyModifiers`, so the edge conversions are a flat
//! one-to-one rename.

/// A single logical key. Covers exactly the subset of terminal keys
/// Shelbi's chord vocabulary recognizes (see the
/// [`chord`](super::chord) grammar).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    /// A character key. `Char(' ')` is the space key; the parser spells it
    /// `space` in chord strings.
    Char(char),
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Insert,
    Home,
    End,
    PageUp,
    PageDown,
    /// Function key `F1`..`F12`.
    F(u8),
}

/// The set of modifiers held when a key fired. A small bitflags-style value
/// (not the `bitflags` crate, to keep `shelbi-state` dependency-light) with
/// the four modifiers Shelbi recognizes. Semantics mirror crossterm's
/// `KeyModifiers`: `contains(NONE)` is vacuously true, `|` unions, and the
/// empty set is [`Mods::NONE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Mods(u8);

impl Mods {
    /// No modifiers held.
    pub const NONE: Mods = Mods(0);
    pub const CONTROL: Mods = Mods(1 << 0);
    pub const ALT: Mods = Mods(1 << 1);
    pub const SHIFT: Mods = Mods(1 << 2);
    pub const SUPER: Mods = Mods(1 << 3);

    /// True when every bit in `other` is set in `self`. Matches crossterm's
    /// `bitflags` semantics, so `contains(Mods::NONE)` is always true.
    pub const fn contains(self, other: Mods) -> bool {
        (self.0 & other.0) == other.0
    }

    /// True when `self` and `other` share at least one bit.
    pub const fn intersects(self, other: Mods) -> bool {
        (self.0 & other.0) != 0
    }

    /// True when no modifier is held.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// The raw modifier bits, for the rare caller that needs a stable key
    /// (e.g. a serialization that isn't the canonical chord string).
    pub const fn bits(self) -> u8 {
        self.0
    }
}

impl std::ops::BitOr for Mods {
    type Output = Mods;
    fn bitor(self, rhs: Mods) -> Mods {
        Mods(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Mods {
    fn bitor_assign(&mut self, rhs: Mods) {
        self.0 |= rhs.0;
    }
}

impl std::ops::BitAnd for Mods {
    type Output = Mods;
    fn bitand(self, rhs: Mods) -> Mods {
        Mods(self.0 & rhs.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contains_matches_bitflags_semantics() {
        let cs = Mods::CONTROL | Mods::SHIFT;
        assert!(cs.contains(Mods::CONTROL));
        assert!(cs.contains(Mods::SHIFT));
        assert!(!cs.contains(Mods::ALT));
        // contains(NONE) is vacuously true, like crossterm's bitflags.
        assert!(cs.contains(Mods::NONE));
        assert!(Mods::NONE.contains(Mods::NONE));
    }

    #[test]
    fn intersects_and_empty() {
        let c = Mods::CONTROL;
        assert!(c.intersects(Mods::CONTROL | Mods::ALT));
        assert!(!c.intersects(Mods::ALT | Mods::SHIFT));
        assert!(Mods::NONE.is_empty());
        assert!(!c.is_empty());
    }

    #[test]
    fn bitor_unions_and_is_order_independent() {
        let a = Mods::ALT | Mods::CONTROL | Mods::SHIFT;
        let b = Mods::SHIFT | Mods::CONTROL | Mods::ALT;
        assert_eq!(a, b);
        let mut m = Mods::NONE;
        m |= Mods::CONTROL;
        m |= Mods::CONTROL;
        assert_eq!(m, Mods::CONTROL);
    }
}
