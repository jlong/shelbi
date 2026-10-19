//! The terminal's ANSI colors as the user changed them
//! (`desktop.terminal_colors`): sixteen optional overrides of the theme's
//! [`TerminalPalette`], parsed from and written back to `config.yaml`'s
//! `#rrggbb` strings.
//!
//! An unset color takes the theme's for the appearance (dark or light), so
//! only what was changed is written and the defaults can move. One set of
//! overrides applies in both appearances, as one palette does in other
//! terminals. Slots 0 to 7 are a color's regular value, 8 to 15 its bold one
//! (the bright eight, which Settings labels Bold).

use shelbi_state::TerminalColors;
use shelbi_term::render::Rgb as TermRgb;

use crate::theme::TerminalPalette;

/// The eight colors' names as Settings shows them, in ANSI order.
pub const NAMES: [&str; 8] = ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"];

/// The colors the user changed: `None` keeps the theme's.
pub type AnsiOverrides = [Option<TermRgb>; 16];

/// A typed color: six hex digits, with or without the leading `#` (case and
/// surrounding space don't matter). Anything else, the three-digit short
/// form included, isn't one, so a half-typed value never shows as a color.
pub fn parse_hex(text: &str) -> Option<TermRgb> {
    let text = text.trim();
    let digits = text.strip_prefix('#').unwrap_or(text);
    if digits.len() != 6 || !digits.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok().map(TermRgb::hex)
}

/// A color as Settings writes it (and the frame shows it): `#E04F52`.
pub fn hex(c: TermRgb) -> String {
    format!("#{:02X}{:02X}{:02X}", c.r, c.g, c.b)
}

/// The overrides `config.yaml` sets; a value that isn't a color is ignored.
pub fn from_config(colors: &TerminalColors) -> AnsiOverrides {
    std::array::from_fn(|slot| colors.get(slot).and_then(parse_hex))
}

/// The overrides as `config.yaml` stores them.
pub fn to_config(overrides: &AnsiOverrides) -> TerminalColors {
    let mut colors = TerminalColors::default();
    for (slot, color) in overrides.iter().enumerate() {
        colors.set(slot, color.map(hex));
    }
    colors
}

impl TerminalPalette {
    /// This palette with the user's ANSI colors in place of its own.
    pub fn with_ansi(mut self, overrides: &AnsiOverrides) -> Self {
        for (slot, color) in overrides.iter().enumerate() {
            if let Some(color) = color {
                self.ansi[slot] = *color;
            }
        }
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_takes_six_digits_with_or_without_the_hash() {
        assert_eq!(parse_hex("#E04F52"), Some(TermRgb::hex(0xe04f52)));
        assert_eq!(parse_hex(" e04f52 "), Some(TermRgb::hex(0xe04f52)));
        assert_eq!(parse_hex("#e04F52"), Some(TermRgb::hex(0xe04f52)));
        for bad in ["", "#", "#E04", "#E04F5", "#E04F522", "#GGGGGG", "E0 4F52", "##E04F52", "red"] {
            assert_eq!(parse_hex(bad), None, "{bad:?}");
        }
        assert_eq!(hex(TermRgb::hex(0x00a6b2)), "#00A6B2");
        assert_eq!(parse_hex(&hex(TermRgb::hex(0x5acd25))), Some(TermRgb::hex(0x5acd25)));
    }

    #[test]
    fn overrides_round_trip_through_the_config_and_bad_values_are_ignored() {
        let mut overrides: AnsiOverrides = [None; 16];
        overrides[1] = Some(TermRgb::hex(0xe04f52));
        overrides[15] = Some(TermRgb::hex(0xffffff));
        let colors = to_config(&overrides);
        assert_eq!(colors.red.regular.as_deref(), Some("#E04F52"));
        assert_eq!(colors.white.bold.as_deref(), Some("#FFFFFF"));
        assert_eq!(colors.red.bold, None, "only what was changed is written");
        assert_eq!(from_config(&colors), overrides);

        let mut hand = TerminalColors::default();
        hand.set(2, Some("green".into()));
        hand.set(10, Some("#9bdaa6".into()));
        let read = from_config(&hand);
        assert_eq!((read[2], read[10]), (None, Some(TermRgb::hex(0x9bdaa6))));
        assert!(to_config(&[None; 16]).is_empty());
    }

    #[test]
    fn overrides_replace_only_their_slots() {
        let base = crate::theme::Theme::resolve(
            shelbi_app::theme::ThemePreference::Dark,
            gpui::WindowAppearance::Dark,
        )
        .terminal;
        let mut overrides: AnsiOverrides = [None; 16];
        overrides[0] = Some(TermRgb::hex(0x000000));
        overrides[9] = Some(TermRgb::hex(0xe69589));
        let p = base.with_ansi(&overrides);
        assert_eq!((p.ansi[0], p.ansi[9]), (TermRgb::hex(0x000000), TermRgb::hex(0xe69589)));
        assert_eq!(p.ansi[1], base.ansi[1]);
        assert_eq!((p.foreground, p.background), (base.foreground, base.background));
        assert_eq!(base.with_ansi(&[None; 16]), base);
    }
}
