//! The desktop's colors: its own palette ([`palette`]), Taskpage's hues and
//! semantic roles, with the component shapes that go with them ([`style`]).
//!
//! The TUI keeps the shared tokens ([`shelbi_app::theme`]); the desktop no
//! longer paints them. Views read roles (`theme.surface_recessed`,
//! `theme.text_quiet`, `theme.icon`), never a hue or a TUI token name. Where a
//! shared view model names a meaning (a [`Tone`], a badge's
//! [`DecorationColor`]), [`Theme::tone`] and [`Theme::decoration`] map it onto
//! the Taskpage hue for the same meaning.
//!
//! The `theme:` setting (`~/.shelbi/config.yaml`) picks the theme. `auto`
//! follows the window's appearance, and [`follow_system`] re-resolves it when
//! the system switches between light and dark while the app runs; `dark` and
//! `light` pin the theme and the window chrome with it.

pub mod palette;
pub mod style;

use std::ops::Deref;

use gpui::{rgba, App, Context, Global, Hsla, Subscription, Window, WindowAppearance};
use shelbi_app::theme::{resolve_appearance, Appearance, ThemePreference, Tone};
use shelbi_palette::DecorationColor;
use shelbi_term::render::Rgb as TermRgb;

pub use palette::{Palette, Rgba};

/// The resolved theme, installed as a gpui [`Global`]. Views read the roles
/// through it (`cx.global::<Theme>().text_quiet`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// The setting this theme was resolved from, kept so an appearance change
    /// can re-resolve `auto`.
    pub preference: ThemePreference,
    colors: Palette<Hsla>,
    /// The terminal element's colors for this appearance.
    pub terminal: TerminalPalette,
}

impl Deref for Theme {
    type Target = Palette<Hsla>;

    fn deref(&self) -> &Self::Target {
        &self.colors
    }
}

impl Global for Theme {}

/// A hue a chip or badge is tinted with: its text color and the tint behind
/// it come from the same scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hue {
    Neutral,
    Green,
    Gold,
    Cyan,
    Blue,
    Purple,
    Red,
    Orange,
}

impl Theme {
    /// The theme for `preference` in a window whose system appearance is
    /// `system`.
    pub fn resolve(preference: ThemePreference, system: WindowAppearance) -> Self {
        let palette = Palette::for_appearance(resolve_appearance(preference, Some(is_dark(system))));
        Self {
            preference,
            terminal: TerminalPalette::for_palette(&palette),
            colors: palette.map(hsla),
        }
    }

    pub fn is_dark(&self) -> bool {
        self.appearance == Appearance::Dark
    }

    /// The color a shared view model names by [`Tone`] (an activity row's
    /// chip, a machines badge): the Taskpage hue for the TUI hue's meaning.
    pub fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::White => self.text_loud,
            Tone::Gray | Tone::WorkflowBadge => self.text,
            Tone::DarkGray => self.text_quiet,
            Tone::Red => self.red,
            Tone::LightRed => self.orange,
            Tone::Green => self.green,
            Tone::Yellow => self.gold,
            Tone::Blue => self.blue,
            Tone::Magenta => self.purple,
            Tone::Cyan => self.cyan,
        }
    }

    /// The hue a [`Tone`] tints a chip with.
    pub fn tone_hue(tone: Tone) -> Hue {
        match tone {
            Tone::White | Tone::Gray | Tone::DarkGray | Tone::WorkflowBadge => Hue::Neutral,
            Tone::Red => Hue::Red,
            Tone::LightRed => Hue::Orange,
            Tone::Green => Hue::Green,
            Tone::Yellow => Hue::Gold,
            Tone::Blue => Hue::Blue,
            Tone::Magenta => Hue::Purple,
            Tone::Cyan => Hue::Cyan,
        }
    }

    /// A hue's text color and its chip tint.
    pub fn hue(&self, hue: Hue) -> (Hsla, Hsla) {
        match hue {
            Hue::Neutral => (self.text, self.neutral_tint),
            Hue::Green => (self.green, self.green_tint),
            Hue::Gold => (self.gold, self.gold_tint),
            Hue::Cyan => (self.cyan, self.cyan_tint),
            Hue::Blue => (self.blue, self.blue_tint),
            Hue::Purple => (self.purple, self.purple_tint),
            Hue::Red => (self.red, self.red_tint),
            Hue::Orange => (self.orange, self.orange_tint),
        }
    }

    /// The color a shared badge decoration paints in, for the meaning the TUI
    /// sidebar's `badge_color` gives it (busy green, muted bullet, ready
    /// cyan, normal text; the rest through the matching hues).
    pub fn decoration(&self, color: DecorationColor) -> Hsla {
        match color {
            DecorationColor::Green => self.green,
            DecorationColor::DarkGray | DecorationColor::Muted => self.text_quiet,
            DecorationColor::Cyan => self.cyan,
            DecorationColor::Gray | DecorationColor::Default => self.text,
            DecorationColor::Yellow => self.gold,
            DecorationColor::Red => self.red,
            DecorationColor::Blue => self.blue,
        }
    }
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// A role's color as gpui's.
pub fn hsla(color: Rgba) -> Hsla {
    let Rgba { rgb, a } = color;
    rgba(u32::from_be_bytes([rgb.r, rgb.g, rgb.b, a])).into()
}

/// The terminal element's colors: what a session's default foreground,
/// background, cursor, and sixteen ANSI colors resolve to.
///
/// In the TUI these are the user's own terminal's palette, which a window
/// doesn't have. The defaults come from the palette's roles (the background
/// is `surface`, the main area the terminal sits in, so a pane is part of the
/// window rather than a hole in it; the selection is `text_selection`; the
/// matches `search_match` / `search_current`), so they follow the theme. The
/// ANSI sixteen are a terminal palette rather than UI roles: Ghostty's
/// defaults (Tomorrow Night) in the dark theme, the palette the TUI's colors
/// were tuned against, so a session reads the same in both clients, and a
/// GitHub-light style set in the light theme, with "white" darkened so it
/// shows on white. Indexed colors 16..=255 are the fixed xterm cube and ramp
/// ([`shelbi_term::render::xterm_256`]) and true color passes through. A
/// program's own OSC 4/10/11/12 redefinitions win over these at render time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalPalette {
    /// The default foreground (`Foreground`): the `text` role.
    pub foreground: TermRgb,
    /// The default background (`Background`): the `surface` role.
    pub background: TermRgb,
    /// The cursor fill (and the beam / underline / outline color):
    /// `text_loud`.
    pub cursor: TermRgb,
    /// The glyph color under a block cursor: `surface`.
    pub cursor_text: TermRgb,
    /// The selection highlight behind selected cells: `text_selection`.
    pub selection: TermRgb,
    /// A search match's highlight: `search_match`.
    pub search_match: TermRgb,
    /// The current search match's highlight: `search_current`.
    pub search_current: TermRgb,
    /// ANSI colors 0..=15: black, red, green, yellow, blue, magenta, cyan,
    /// white, then the bright eight.
    pub ansi: [TermRgb; 16],
}

impl TerminalPalette {
    /// The terminal palette for a resolved palette. Every default is opaque:
    /// a translucent role is flattened over the terminal's background.
    pub fn for_palette(palette: &Palette) -> Self {
        let under = palette.surface.rgb;
        let c = |role: Rgba| {
            let rgb = role.over(under);
            TermRgb::new(rgb.r, rgb.g, rgb.b)
        };
        let ansi = if palette.appearance == Appearance::Dark {
            [
                0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6,
                0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
            ]
        } else {
            [
                0x24292f, 0xcf222e, 0x116329, 0x7d4e00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
                0x57606a, 0xa40e26, 0x1a7f37, 0x9a6700, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
            ]
        };
        Self {
            foreground: c(palette.text),
            background: c(palette.surface),
            cursor: c(palette.text_loud),
            cursor_text: c(palette.surface),
            selection: c(palette.text_selection),
            search_match: c(palette.search_match),
            search_current: c(palette.search_current),
            ansi: ansi.map(TermRgb::hex),
        }
    }
}

/// A terminal [`TermRgb`] as a gpui color.
pub fn rgb_hsla(c: TermRgb) -> Hsla {
    gpui::rgb(u32::from_be_bytes([0, c.r, c.g, c.b])).into()
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

/// Make `theme` current: our global, and gpui-kit's light/dark mode with its
/// colors set from our roles, so its widgets (markdown, inputs, text areas)
/// read as the same family. Repaints every window.
pub(crate) fn install(theme: Theme, cx: &mut App) {
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
    gpui_kit::component::Theme::update(cx, |kit| kit_colors(&mut kit.colors, &theme));
    cx.refresh_windows();
}

/// gpui-kit's widget colors from our roles.
fn kit_colors(c: &mut gpui_kit::component::ThemeColor, t: &Theme) {
    c.background = t.surface;
    c.foreground = t.text;
    c.border = t.rule_strong;
    c.input = t.rule_strong;
    c.ring = t.focus_ring;
    c.caret = t.text_loud;
    c.selection = t.text_selection;
    c.muted = t.fill_subtle;
    c.muted_foreground = t.text_quiet;
    c.accent = t.fill_subtle;
    c.accent_foreground = t.text_loud;
    c.popover = t.surface_raised;
    c.popover_foreground = t.text;
    c.overlay = t.scrim;
    c.primary = t.brand;
    c.primary_hover = t.brand_hover;
    c.primary_active = t.brand_hover;
    c.primary_foreground = t.text_on_color;
    c.secondary = t.fill;
    c.secondary_hover = t.fill_strong;
    c.secondary_active = t.fill_strong;
    c.secondary_foreground = t.text_loud;
    c.link = t.link;
    c.link_hover = t.link_hover;
    c.link_active = t.link_hover;
    c.list_hover = t.fill_subtle;
    c.list_active = t.row_selected;
    c.scrollbar_thumb = t.icon_secondary;
    c.scrollbar_thumb_hover = t.icon;
    c.danger = t.error_fill;
    c.danger_foreground = t.text_on_color;
    c.success = t.success;
    c.warning = t.warning;
    c.info = t.info;
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

    fn rgba8(c: Hsla) -> (u8, u8, u8, u8) {
        let c = c.to_rgb();
        let q = |v: f32| (v * 255.0).round() as u8;
        (q(c.r), q(c.g), q(c.b), q(c.a))
    }

    #[test]
    fn every_role_maps_to_its_exact_color() {
        for (palette, system) in [
            (Palette::dark(), WindowAppearance::Dark),
            (Palette::light(), WindowAppearance::Light),
        ] {
            let theme = Theme::resolve(ThemePreference::Auto, system);
            assert_eq!(theme.appearance, palette.appearance);
            for ((name, want), (_, got)) in palette.roles().into_iter().zip(theme.roles()) {
                assert_eq!(
                    rgba8(got),
                    (want.rgb.r, want.rgb.g, want.rgb.b, want.a),
                    "{name}"
                );
            }
        }
    }

    #[test]
    fn the_desktop_no_longer_paints_the_tui_tokens() {
        // The TUI's dark main area was #1c1c1c and its sidebar pure black;
        // the desktop's are Taskpage's black scale now.
        let t = Theme::resolve(ThemePreference::Dark, WindowAppearance::Light);
        assert!(t.is_dark());
        assert_eq!(rgba8(t.surface_recessed), (0x13, 0x14, 0x18, 0xff));
        assert_eq!(rgba8(t.surface), (0x18, 0x1a, 0x20, 0xff));
        let tui = shelbi_app::theme::Theme::dark();
        assert_eq!(tui.canvas.rgb, shelbi_app::theme::Rgb::hex(0x1c1c1c), "TUI unchanged");
    }

    #[test]
    fn terminal_palette_follows_the_theme() {
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            let t = Theme::resolve(ThemePreference::Auto, system);
            assert_eq!(rgb_hsla(t.terminal.background), t.surface, "the main surface");
            assert_eq!(rgb_hsla(t.terminal.foreground), t.text);
            assert_eq!(rgb_hsla(t.terminal.cursor), t.text_loud);
            assert_eq!(rgb_hsla(t.terminal.cursor_text), t.surface);
            assert_eq!(rgb_hsla(t.terminal.search_current), t.search_current);
        }
        let dark = Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark).terminal;
        assert_eq!(dark.ansi[1], TermRgb::hex(0xcc6666), "Ghostty's red");
        let light = Theme::resolve(ThemePreference::Light, WindowAppearance::Light).terminal;
        assert_ne!(
            light.ansi[7], light.background,
            "ANSI white must show on the light surface"
        );
    }

    #[test]
    fn terminal_text_stays_legible_on_both_surfaces() {
        // The default foreground and the ANSI foreground hues a CLI (Claude
        // Code included) prints in, on the new background. 3:1 for the hues
        // (a terminal palette, not UI text), AA for the default foreground.
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            let t = Theme::resolve(ThemePreference::Auto, system).terminal;
            let rgb = |c: TermRgb| shelbi_app::theme::Rgb::new(c.r, c.g, c.b);
            let bg = rgb(t.background);
            assert!(rgb(t.foreground).contrast_ratio(bg) >= 4.5, "{system:?} fg");
            for (i, c) in t.ansi.iter().enumerate() {
                // Skip black (0) and its bright (8) in dark, white (7, 15) in
                // light: the slot that matches the background by design.
                let skip = if system == WindowAppearance::Dark { [0, 8] } else { [7, 15] };
                if skip.contains(&i) {
                    continue;
                }
                let ratio = rgb(*c).contrast_ratio(bg);
                assert!(ratio >= 3.0, "{system:?} ansi {i}: {ratio:.2}");
            }
        }
    }

    #[test]
    fn shared_meanings_map_onto_taskpage_hues() {
        let t = Theme::resolve(ThemePreference::Light, WindowAppearance::Light);
        assert_eq!(t.decoration(DecorationColor::Green), t.green, "working");
        assert_eq!(t.decoration(DecorationColor::Cyan), t.cyan, "ready");
        assert_eq!(t.decoration(DecorationColor::Yellow), t.gold, "awaiting input");
        assert_eq!(t.decoration(DecorationColor::Muted), t.text_quiet, "idle");
        assert_eq!(t.tone(Tone::Magenta), t.purple, "handoff");
        assert_eq!(t.tone(Tone::Yellow), t.gold);
        assert_eq!(t.hue(Theme::tone_hue(Tone::Blue)).0, t.tone(Tone::Blue));
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
