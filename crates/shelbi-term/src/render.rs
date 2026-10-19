//! Render-facing plain data: what a viewer needs to paint a session that is
//! not a cell's text.
//!
//! A ratatui pane and a gpui element both read the emulator's grid directly
//! (cell characters and colors), but the pieces around it are plain data here
//! so neither client re-derives them from `alacritty_terminal` internals:
//!
//! - [`Cursor`]: where the cursor is in viewport coordinates, its shape, and
//!   whether the program asked for it to blink
//!   ([`TermEmulator::cursor`](crate::emulator::TermEmulator::cursor)).
//! - [`Damage`]: which viewport rows changed since the last read
//!   ([`TermEmulator::take_damage`](crate::emulator::TermEmulator::take_damage)),
//!   so a viewer repaints from damage rather than on a timer and can rebuild
//!   only the rows that changed.
//! - [`CellAttrs`]: a cell's text attributes as plain booleans.
//! - [`Rgb`] and [`xterm_256`]: the xterm-256 palette's fixed half (the color
//!   cube and grayscale ramp), which a viewer without a terminal palette of its
//!   own (a window) resolves indexed colors against.

use alacritty_terminal::term::cell::Flags;

/// A 24-bit color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red.
    pub r: u8,
    /// Green.
    pub g: u8,
    /// Blue.
    pub b: u8,
}

impl Rgb {
    /// A color from its components.
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    /// A `0xRRGGBB` literal.
    pub const fn hex(value: u32) -> Self {
        Self { r: (value >> 16) as u8, g: (value >> 8) as u8, b: value as u8 }
    }
}

impl From<alacritty_terminal::vte::ansi::Rgb> for Rgb {
    fn from(c: alacritty_terminal::vte::ansi::Rgb) -> Self {
        Self { r: c.r, g: c.g, b: c.b }
    }
}

/// The fixed part of the xterm-256 palette: index 16..=231 is the 6x6x6 color
/// cube, 232..=255 the 24-step grayscale ramp. Returns `None` for 0..=15, the
/// sixteen ANSI colors, which belong to the viewer's theme.
pub fn xterm_256(index: u8) -> Option<Rgb> {
    const STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => None,
        16..=231 => {
            let i = index - 16;
            Some(Rgb::new(STEPS[(i / 36) as usize], STEPS[(i / 6 % 6) as usize], STEPS[(i % 6) as usize]))
        }
        232..=255 => {
            let v = 8 + (index - 232) * 10;
            Some(Rgb::new(v, v, v))
        }
    }
}

/// How the cursor is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorShape {
    /// A filled cell.
    Block,
    /// A line under the cell.
    Underline,
    /// A vertical bar at the cell's left edge.
    Beam,
    /// An outlined cell (alacritty's unfocused block).
    HollowBlock,
}

/// The cursor as a viewer paints it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    /// Viewport row (0 = top of what is shown, after the scrollback offset).
    pub row: u16,
    /// Column.
    pub col: u16,
    /// Shape the program asked for (DECSCUSR), or the default block.
    pub shape: CursorShape,
    /// Whether the program asked for a blinking cursor. Blinking itself is the
    /// viewer's timing concern.
    pub blinking: bool,
}

/// One damaged viewport row: columns `left..=right` changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RowDamage {
    /// Viewport row.
    pub row: u16,
    /// Leftmost changed column.
    pub left: u16,
    /// Rightmost changed column (inclusive).
    pub right: u16,
}

/// What changed on screen since the damage was last taken.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Damage {
    /// Everything: a resize, a scroll of the viewport, a mode that redraws the
    /// whole screen.
    Full,
    /// Only these rows. The cursor's old and new rows are always included, so
    /// a cursor move alone shows up here.
    Rows(Vec<RowDamage>),
}

impl Damage {
    /// Whether nothing changed.
    pub fn is_empty(&self) -> bool {
        matches!(self, Damage::Rows(rows) if rows.is_empty())
    }

    /// Whether viewport `row` changed.
    pub fn contains_row(&self, row: u16) -> bool {
        match self {
            Damage::Full => true,
            Damage::Rows(rows) => rows.iter().any(|d| d.row == row),
        }
    }

    /// Fold `other` into this damage (a viewer that takes damage more often
    /// than it paints accumulates it).
    pub fn merge(&mut self, other: Damage) {
        match (&mut *self, other) {
            (Damage::Full, _) => {}
            (_, Damage::Full) => *self = Damage::Full,
            (Damage::Rows(mine), Damage::Rows(theirs)) => {
                for d in theirs {
                    match mine.iter_mut().find(|m| m.row == d.row) {
                        Some(m) => {
                            m.left = m.left.min(d.left);
                            m.right = m.right.max(d.right);
                        }
                        None => mine.push(d),
                    }
                }
            }
        }
    }
}

/// The underline style on a cell.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Underline {
    /// No underline.
    #[default]
    None,
    /// A single line.
    Single,
    /// Two lines.
    Double,
    /// A wavy line.
    Curly,
    /// A dotted line.
    Dotted,
    /// A dashed line.
    Dashed,
}

/// A cell's text attributes as plain data.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CellAttrs {
    /// Bold (SGR 1).
    pub bold: bool,
    /// Dim / faint (SGR 2).
    pub dim: bool,
    /// Italic (SGR 3).
    pub italic: bool,
    /// Underline style (SGR 4 and its sub-parameters).
    pub underline: Underline,
    /// Strikethrough (SGR 9).
    pub strikethrough: bool,
    /// Reverse video (SGR 7): swap the cell's foreground and background.
    pub inverse: bool,
    /// Hidden / concealed (SGR 8).
    pub hidden: bool,
    /// The cell holds a double-width character that also covers the next cell.
    pub wide: bool,
    /// The cell is the placeholder half of a double-width character (the cell
    /// after a wide one, or the padding at a row's end before a wide character
    /// wrapped). Paint nothing for it.
    pub spacer: bool,
}

impl CellAttrs {
    /// Read the attributes off an `alacritty_terminal` cell's flags.
    pub fn from_flags(f: Flags) -> Self {
        let underline = if f.contains(Flags::DOUBLE_UNDERLINE) {
            Underline::Double
        } else if f.contains(Flags::UNDERCURL) {
            Underline::Curly
        } else if f.contains(Flags::DOTTED_UNDERLINE) {
            Underline::Dotted
        } else if f.contains(Flags::DASHED_UNDERLINE) {
            Underline::Dashed
        } else if f.contains(Flags::UNDERLINE) {
            Underline::Single
        } else {
            Underline::None
        };
        Self {
            bold: f.contains(Flags::BOLD),
            dim: f.contains(Flags::DIM),
            italic: f.contains(Flags::ITALIC),
            underline,
            strikethrough: f.contains(Flags::STRIKEOUT),
            inverse: f.contains(Flags::INVERSE),
            hidden: f.contains(Flags::HIDDEN),
            wide: f.contains(Flags::WIDE_CHAR),
            spacer: f.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xterm_256_leaves_the_ansi_sixteen_to_the_theme() {
        for i in 0..16 {
            assert_eq!(xterm_256(i), None, "index {i}");
        }
    }

    #[test]
    fn xterm_256_cube_and_ramp_anchors() {
        assert_eq!(xterm_256(16), Some(Rgb::new(0, 0, 0)));
        assert_eq!(xterm_256(196), Some(Rgb::new(255, 0, 0)));
        assert_eq!(xterm_256(46), Some(Rgb::new(0, 255, 0)));
        assert_eq!(xterm_256(21), Some(Rgb::new(0, 0, 255)));
        assert_eq!(xterm_256(231), Some(Rgb::new(255, 255, 255)));
        // 16 + 36*1 + 6*2 + 3 = 67 -> (95, 135, 175)
        assert_eq!(xterm_256(67), Some(Rgb::new(95, 135, 175)));
        assert_eq!(xterm_256(232), Some(Rgb::new(8, 8, 8)));
        assert_eq!(xterm_256(255), Some(Rgb::new(238, 238, 238)));
    }

    #[test]
    fn rgb_hex_splits_components() {
        assert_eq!(Rgb::hex(0x00a6b2), Rgb::new(0x00, 0xa6, 0xb2));
    }

    #[test]
    fn cell_attrs_read_every_flag() {
        let a = CellAttrs::from_flags(
            Flags::BOLD | Flags::ITALIC | Flags::STRIKEOUT | Flags::INVERSE | Flags::DIM | Flags::HIDDEN,
        );
        assert!(a.bold && a.italic && a.strikethrough && a.inverse && a.dim && a.hidden);
        assert_eq!(a.underline, Underline::None);
        assert!(!a.wide && !a.spacer);

        assert_eq!(CellAttrs::from_flags(Flags::UNDERLINE).underline, Underline::Single);
        assert_eq!(CellAttrs::from_flags(Flags::DOUBLE_UNDERLINE).underline, Underline::Double);
        assert_eq!(CellAttrs::from_flags(Flags::UNDERCURL).underline, Underline::Curly);
        assert_eq!(CellAttrs::from_flags(Flags::DOTTED_UNDERLINE).underline, Underline::Dotted);
        assert_eq!(CellAttrs::from_flags(Flags::DASHED_UNDERLINE).underline, Underline::Dashed);

        assert!(CellAttrs::from_flags(Flags::WIDE_CHAR).wide);
        assert!(CellAttrs::from_flags(Flags::WIDE_CHAR_SPACER).spacer);
        assert!(CellAttrs::from_flags(Flags::LEADING_WIDE_CHAR_SPACER).spacer);
    }

    #[test]
    fn damage_merge_widens_rows_and_full_wins() {
        let mut d = Damage::Rows(vec![RowDamage { row: 1, left: 4, right: 6 }]);
        d.merge(Damage::Rows(vec![
            RowDamage { row: 1, left: 2, right: 5 },
            RowDamage { row: 3, left: 0, right: 0 },
        ]));
        assert_eq!(
            d,
            Damage::Rows(vec![
                RowDamage { row: 1, left: 2, right: 6 },
                RowDamage { row: 3, left: 0, right: 0 },
            ])
        );
        assert!(d.contains_row(3) && !d.contains_row(2));
        d.merge(Damage::Full);
        assert_eq!(d, Damage::Full);
        d.merge(Damage::Rows(vec![]));
        assert_eq!(d, Damage::Full);
        assert!(Damage::Rows(vec![]).is_empty());
        assert!(!Damage::Full.is_empty());
    }
}
