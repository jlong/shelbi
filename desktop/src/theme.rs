//! The desktop's colors: the shared tokens ([`shelbi_app::theme`]) as gpui
//! colors.
//!
//! The token names are the TUI's (`background`, `gray`, `white`, `muted`,
//! `cyan`, `selection`, ...), one set for both clients, so the two read as one
//! product. A window has no terminal palette, so a token's exact RGB is what
//! paints here even where the TUI uses an ANSI slot.
//!
//! The `theme:` setting (`~/.shelbi/config.yaml`) picks the theme. `auto`
//! follows the window's appearance, and [`follow_system`] re-resolves it when
//! the system switches between light and dark while the app runs; `dark` and
//! `light` pin the theme and the window chrome with it.

use std::ops::Deref;

use gpui::{rgb, App, Context, Global, Hsla, Subscription, Window, WindowAppearance};
use shelbi_palette::DecorationColor;
use shelbi_app::theme::{resolve_appearance, Appearance, Rgb, Theme as Tokens, ThemePreference};
use shelbi_term::render::Rgb as TermRgb;

/// The resolved theme, installed as a gpui [`Global`]. Views read the tokens
/// through it (`cx.global::<Theme>().gray`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// The setting this theme was resolved from, kept so an appearance change
    /// can re-resolve `auto`.
    pub preference: ThemePreference,
    colors: Tokens<Hsla>,
    /// The terminal element's colors for this appearance.
    pub terminal: TerminalPalette,
}

impl Deref for Theme {
    type Target = Tokens<Hsla>;

    fn deref(&self) -> &Self::Target {
        &self.colors
    }
}

impl Global for Theme {}

impl Theme {
    /// The theme for `preference` in a window whose system appearance is
    /// `system`.
    pub fn resolve(preference: ThemePreference, system: WindowAppearance) -> Self {
        let tokens = match resolve_appearance(preference, Some(is_dark(system))) {
            Appearance::Dark => Tokens::dark(),
            Appearance::Light => Tokens::light(),
        };
        Self {
            preference,
            terminal: TerminalPalette::for_tokens(&tokens),
            colors: tokens.map(|swatch| hsla(swatch.rgb)),
        }
    }

    pub fn is_dark(&self) -> bool {
        self.appearance == Appearance::Dark
    }

    /// The color a shared badge decoration paints in, mapped exactly as the
    /// TUI sidebar's `badge_color` maps it (busy green, muted bullet, ready
    /// cyan, normal gray; the rest through the terminal hues).
    pub fn decoration(&self, color: DecorationColor) -> Hsla {
        match color {
            DecorationColor::Green => self.green,
            DecorationColor::DarkGray | DecorationColor::Muted => self.muted,
            DecorationColor::Cyan => self.cyan,
            DecorationColor::Gray | DecorationColor::Default => self.gray,
            DecorationColor::Yellow => self.ansi_yellow,
            DecorationColor::Red => self.ansi_red,
            DecorationColor::Blue => self.ansi_blue,
        }
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

fn hsla(color: Rgb) -> Hsla {
    rgb(u32::from_be_bytes([0, color.r, color.g, color.b])).into()
}

/// The terminal element's colors: what a session's default foreground,
/// background, cursor, and sixteen ANSI colors resolve to.
///
/// In the TUI these are the user's own terminal's palette, which a window
/// doesn't have. The defaults come from the shared tokens (the background is
/// `canvas`, the main area the terminal sits in; the selection is `selection`;
/// the current search match is `cyan`), so they follow the theme. The ANSI
/// sixteen are a terminal palette rather than UI tokens: Ghostty's defaults
/// (Tomorrow Night) in the dark theme, the palette the TUI's tokens were tuned
/// against, so a session reads the same in both clients, and a GitHub-light
/// style set in the light theme, with "white" darkened so it shows on white.
/// Indexed colors 16..=255 are the fixed xterm cube and ramp
/// ([`shelbi_term::render::xterm_256`]) and true color passes through. A
/// program's own OSC 4/10/11/12 redefinitions win over these at render time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalPalette {
    /// The default foreground (`Foreground`): the `foreground` token.
    pub foreground: TermRgb,
    /// The default background (`Background`): the `canvas` token.
    pub background: TermRgb,
    /// The cursor fill (and the beam / underline / outline color): `white`.
    pub cursor: TermRgb,
    /// The glyph color under a block cursor: `canvas`.
    pub cursor_text: TermRgb,
    /// The selection highlight behind selected cells: `selection`.
    pub selection: TermRgb,
    /// A search match's highlight.
    pub search_match: TermRgb,
    /// The current search match's highlight: `cyan`.
    pub search_current: TermRgb,
    /// ANSI colors 0..=15: black, red, green, yellow, blue, magenta, cyan,
    /// white, then the bright eight.
    pub ansi: [TermRgb; 16],
}

impl TerminalPalette {
    /// The terminal palette for a resolved token set.
    pub fn for_tokens(tokens: &Tokens) -> Self {
        let c = |swatch: shelbi_app::theme::Swatch| {
            TermRgb::new(swatch.rgb.r, swatch.rgb.g, swatch.rgb.b)
        };
        let dark = tokens.appearance == Appearance::Dark;
        let (search_match, ansi) = if dark {
            (
                0x5c5c5c,
                [
                    0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6,
                    0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
                ],
            )
        } else {
            (
                0xc8c8c8,
                [
                    0x24292f, 0xcf222e, 0x116329, 0x7d4e00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
                    0x57606a, 0xa40e26, 0x1a7f37, 0x9a6700, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
                ],
            )
        };
        Self {
            foreground: c(tokens.foreground),
            background: c(tokens.canvas),
            cursor: c(tokens.white),
            cursor_text: c(tokens.canvas),
            selection: c(tokens.selection),
            search_match: TermRgb::hex(search_match),
            search_current: c(tokens.cyan),
            ansi: ansi.map(TermRgb::hex),
        }
    }
}

/// A terminal [`TermRgb`] as a gpui color.
pub fn rgb_hsla(c: TermRgb) -> Hsla {
    rgb(u32::from_be_bytes([0, c.r, c.g, c.b])).into()
}

/// The window chrome (title bar, borders) for a setting: pinned for `dark` /
/// `light`, the system's for `auto`.
fn chrome_appearance(preference: ThemePreference) -> Option<WindowAppearance> {
    match preference {
        ThemePreference::Auto => None,
        ThemePreference::Dark => Some(WindowAppearance::Dark),
        ThemePreference::Light => Some(WindowAppearance::Light),
    }
}

/// Read the `theme:` setting and install the resolved theme. Call once at
/// startup, before the first window opens. A missing or unreadable config is
/// `auto`.
pub fn init(cx: &mut App) {
    let preference = shelbi_state::load_user_config()
        .map(|c| c.theme)
        .unwrap_or_default();
    cx.set_window_appearance(chrome_appearance(preference));
    install(Theme::resolve(preference, cx.window_appearance()), cx);
}

/// Switch to `preference` now (the settings view's theme choice): re-resolve
/// against `window`'s appearance, pin or release the window chrome, and
/// repaint.
pub fn apply_preference(preference: ThemePreference, window: &Window, cx: &mut App) {
    cx.set_window_appearance(chrome_appearance(preference));
    install(Theme::resolve(preference, window.appearance()), cx);
}

/// Make `theme` current: our global, and gpui-kit's light/dark mode so its
/// widgets match. Repaints every window.
fn install(theme: Theme, cx: &mut App) {
    let mode = if theme.is_dark() {
        gpui_kit::component::ThemeMode::Dark
    } else {
        gpui_kit::component::ThemeMode::Light
    };
    tracing::info!(
        preference = ?theme.preference,
        appearance = ?theme.appearance,
        "theme resolved"
    );
    cx.set_global(theme);
    gpui_kit::component::Theme::change(mode, None, cx);
    cx.refresh_windows();
}

/// Keep the theme in step with `window`'s appearance: re-resolve now (the
/// window's appearance is authoritative once it exists) and again whenever
/// the system switches between light and dark. Hold the returned
/// subscription for the window's lifetime.
pub fn follow_system<V: 'static>(window: &mut Window, cx: &mut Context<V>) -> Subscription {
    sync(window, cx);
    cx.observe_window_appearance(window, |_, window, cx| sync(window, cx))
}

fn sync(window: &Window, cx: &mut App) {
    let current = *cx.global::<Theme>();
    let next = Theme::resolve(current.preference, window.appearance());
    if next != current {
        install(next, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb8(c: Hsla) -> (u8, u8, u8) {
        let c = c.to_rgb();
        (
            (c.r * 255.0).round() as u8,
            (c.g * 255.0).round() as u8,
            (c.b * 255.0).round() as u8,
        )
    }

    #[test]
    fn dark_tokens_carry_the_tui_values() {
        let t = Theme::resolve(ThemePreference::Dark, WindowAppearance::Light);
        assert!(t.is_dark());
        assert_eq!(rgb8(t.background), (0, 0, 0));
        assert_eq!(rgb8(t.canvas), (0x1c, 0x1c, 0x1c));
        assert_eq!(rgb8(t.gray), (186, 186, 186));
        assert_eq!(rgb8(t.cyan), (0, 166, 178));
        assert_eq!(rgb8(t.selection), (63, 63, 63));
        assert_eq!(rgb8(t.divider), (65, 65, 65));
        assert_eq!(rgb8(t.workflow_badge_bg), (58, 66, 88));
        assert_eq!(rgb8(t.yellow), (0xdb, 0xc3, 0x00));
        // ANSI-backed tokens paint their stand-in RGB in a window.
        assert_eq!(rgb8(t.divider_active), (0, 166, 178));
    }

    #[test]
    fn every_token_maps_to_its_exact_rgb() {
        for tokens in [Tokens::dark(), Tokens::light()] {
            let system = if tokens.appearance == Appearance::Dark {
                WindowAppearance::Dark
            } else {
                WindowAppearance::Light
            };
            let theme = Theme::resolve(ThemePreference::Auto, system);
            for ((name, want), (_, got)) in tokens.tokens().into_iter().zip(theme.tokens()) {
                assert_eq!(rgb8(got), (want.rgb.r, want.rgb.g, want.rgb.b), "{name}");
            }
        }
    }

    #[test]
    fn terminal_palette_follows_the_theme() {
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            let t = Theme::resolve(ThemePreference::Auto, system);
            assert_eq!(rgb_hsla(t.terminal.background), t.canvas);
            assert_eq!(rgb_hsla(t.terminal.foreground), t.foreground);
            assert_eq!(rgb_hsla(t.terminal.selection), t.selection);
            assert_eq!(rgb_hsla(t.terminal.search_current), t.cyan);
            assert_eq!(rgb_hsla(t.terminal.cursor_text), t.canvas);
        }
        let dark = Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark).terminal;
        assert_eq!(dark.ansi[1], TermRgb::hex(0xcc6666), "Ghostty's red");
        let light = Theme::resolve(ThemePreference::Light, WindowAppearance::Light).terminal;
        assert_ne!(
            light.ansi[7], light.background,
            "ANSI white must show on the light canvas"
        );
    }

    #[test]
    fn auto_follows_the_window_and_overrides_win() {
        use ThemePreference::*;
        use WindowAppearance as W;
        for (pref, system, dark) in [
            (Auto, W::Dark, true),
            (Auto, W::VibrantDark, true),
            (Auto, W::Light, false),
            (Auto, W::VibrantLight, false),
            (Dark, W::Light, true),
            (Dark, W::Dark, true),
            (Light, W::Dark, false),
            (Light, W::Light, false),
        ] {
            let theme = Theme::resolve(pref, system);
            assert_eq!(theme.is_dark(), dark, "{pref:?} under {system:?}");
            assert_eq!(theme.preference, pref);
        }
        assert_eq!(chrome_appearance(Auto), None, "auto leaves the chrome to the system");
        assert_eq!(chrome_appearance(Dark), Some(W::Dark));
        assert_eq!(chrome_appearance(Light), Some(W::Light));
    }
}
