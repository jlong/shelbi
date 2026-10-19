//! The desktop's colors: its own palette ([`palette`]), the Figma design
//! system's tokens as semantic roles, with the type styles and component
//! shapes that go with them ([`style`]).
//!
//! The TUI keeps the shared tokens ([`shelbi_app::theme`]); the desktop no
//! longer paints them. Views read roles (`theme.sidebar`, `theme.muted`,
//! `theme.icon`), never a hue or a TUI token name. Where a shared view model
//! names a meaning (a [`Tone`], a badge's [`DecorationColor`]),
//! [`Theme::tone`] and [`Theme::decoration`] map it onto the Figma color for
//! the same meaning.
//!
//! The `theme:` setting (`~/.shelbi/config.yaml`) picks the theme. `auto`
//! follows the window's appearance, and [`follow_system`] re-resolves it when
//! the system switches between light and dark while the app runs; `dark` and
//! `light` pin the theme and the window chrome with it.

pub mod motion;
pub mod palette;
pub mod style;

use std::ops::Deref;

use gpui::{rgba, App, Context, Global, Hsla, Subscription, Window, WindowAppearance};
use shelbi_app::theme::{resolve_appearance, Appearance, ThemePreference, Tone};
use shelbi_palette::DecorationColor;
use shelbi_term::render::Rgb as TermRgb;

pub use palette::{Palette, Rgba};

/// The resolved theme, installed as a gpui [`Global`]. Views read the roles
/// through it (`cx.global::<Theme>().muted`).
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
    Yellow,
    Red,
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
    /// tag, a machines badge): the Figma log color for the TUI hue's
    /// meaning (Activity's `theme.ansi_*`).
    pub fn tone(&self, tone: Tone) -> Hsla {
        match tone {
            Tone::White => self.text_loud,
            Tone::Gray | Tone::WorkflowBadge => self.foreground,
            Tone::DarkGray => self.dark_gray,
            Tone::Red | Tone::LightRed => self.red,
            Tone::Green => self.green,
            Tone::Yellow => self.yellow,
            Tone::Blue => self.blue,
            Tone::Magenta => self.magenta,
            Tone::Cyan => self.cyan,
        }
    }

    /// A hue's text color and its chip tint.
    pub fn hue(&self, hue: Hue) -> (Hsla, Hsla) {
        match hue {
            Hue::Neutral => (self.workflow_badge_text, self.workflow_badge),
            Hue::Yellow => (self.yellow, self.yellow_tint),
            Hue::Red => (self.red, self.red_tint),
        }
    }

    /// The color a shared badge decoration paints in, for the meaning the TUI
    /// sidebar's `badge_color` gives it: the Navigation component's status
    /// icons (busy `icon/green`, ready `icon/cyan`, awaiting input
    /// `icon/yellow`, an idle bullet muted), normal text, and the log hues
    /// for the rest.
    pub fn decoration(&self, color: DecorationColor) -> Hsla {
        match color {
            DecorationColor::Green => self.icon_green,
            DecorationColor::DarkGray | DecorationColor::Muted => self.muted,
            DecorationColor::Cyan => self.icon_cyan,
            DecorationColor::Gray | DecorationColor::Default => self.foreground,
            DecorationColor::Yellow => self.icon_yellow,
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
/// is `canvas`, the main area the terminal sits in, so a pane is part of the
/// window rather than a hole in it; the foreground `foreground`, bold text
/// in the default color `text_loud` and dim text `muted`, as the terminal
/// frames draw them; the selection is `text_selection`; the matches
/// `search_match` / `search_current`), so they follow the theme. The ANSI
/// sixteen take the Figma terminal colors where Figma has them (red, green
/// and cyan output from the terminal frames' `theme.red` / `theme.green` /
/// `theme.cyan`; yellow, blue, magenta and bright black from
/// `theme.ansi_*`), and keep a
/// terminal palette for the rest: Ghostty's defaults (Tomorrow Night) in the
/// dark theme, a GitHub-light style set in the light theme, with "white"
/// darkened so it shows on white. Indexed colors 16..=255 are the fixed xterm cube and ramp
/// ([`shelbi_term::render::xterm_256`]) and true color passes through. A
/// program's own OSC 4/10/11/12 redefinitions win over these at render time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalPalette {
    /// The default foreground (`Foreground`): the `foreground` role.
    pub foreground: TermRgb,
    /// Bold text in the default foreground (`BrightForeground`): the
    /// `text_loud` role (`theme.white`, the frames' bold prompt lines).
    pub bold_foreground: TermRgb,
    /// Dim (SGR 2) text in the default foreground (`DimForeground`): the
    /// `muted` role (the frames' tool output and footers).
    pub dim_foreground: TermRgb,
    /// The default background (`Background`): the `canvas` role.
    pub background: TermRgb,
    /// The cursor fill (and the beam / underline / outline color):
    /// `text_loud`.
    pub cursor: TermRgb,
    /// The glyph color under a block cursor: `canvas`.
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
        let under = palette.canvas.rgb;
        let c = |role: Rgba| {
            let rgb = role.over(under);
            TermRgb::new(rgb.r, rgb.g, rgb.b)
        };
        let mut ansi = if palette.appearance == Appearance::Dark {
            [
                0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6,
                0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
            ]
            .map(TermRgb::hex)
        } else {
            [
                0x24292f, 0xcf222e, 0x116329, 0x7d4e00, 0x0969da, 0x8250df, 0x1b7c83, 0x6e7781,
                0x57606a, 0xa40e26, 0x1a7f37, 0x9a6700, 0x218bff, 0xa475f9, 0x3192aa, 0x8c959f,
            ]
            .map(TermRgb::hex)
        };
        // The Figma terminal colors, in their ANSI slots.
        for (slot, role) in [
            (1, palette.theme_red),
            (2, palette.theme_green),
            (3, palette.yellow),
            (4, palette.blue),
            (5, palette.magenta),
            (6, palette.theme_cyan),
            (8, palette.dark_gray),
        ] {
            ansi[slot] = c(role);
        }
        Self {
            foreground: c(palette.foreground),
            bold_foreground: c(palette.text_loud),
            dim_foreground: c(palette.muted),
            background: c(palette.canvas),
            cursor: c(palette.text_loud),
            cursor_text: c(palette.canvas),
            selection: c(palette.text_selection),
            search_match: c(palette.search_match),
            search_current: c(palette.search_current),
            ansi,
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
    c.background = t.canvas;
    c.foreground = t.foreground;
    c.border = t.divider_strong;
    c.input = t.input_border;
    c.ring = t.focus_ring;
    // The Textbox component draws its cursor in `focus/default/ring` (the
    // palette's own caret is the same family, `text/cyan`).
    c.caret = t.focus_ring;
    c.selection = t.text_selection;
    c.muted = t.background;
    c.muted_foreground = t.muted;
    c.accent = t.background;
    c.accent_foreground = t.text_loud;
    c.popover = t.raised;
    c.popover_foreground = t.foreground;
    c.overlay = t.scrim;
    c.primary = t.success_fill;
    c.primary_hover = t.success_fill_hover;
    c.primary_active = t.success_fill_active;
    c.primary_foreground = t.success_label;
    c.secondary = t.background;
    c.secondary_hover = t.background_hover;
    c.secondary_active = t.background_active;
    c.secondary_foreground = t.text_loud;
    c.link = t.link;
    c.link_hover = t.link_hover;
    c.link_active = t.link_hover;
    c.list_hover = t.background;
    c.list_active = t.row_selected;
    c.scrollbar_thumb = t.muted;
    c.scrollbar_thumb_hover = t.icon;
    c.danger = t.danger_fill;
    c.danger_foreground = t.error;
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
        // the desktop's are Figma's blue-slate canvas and its lighter tint.
        let t = Theme::resolve(ThemePreference::Dark, WindowAppearance::Light);
        assert!(t.is_dark());
        assert_eq!(rgba8(t.sidebar), (0x2c, 0x31, 0x40, 0xff));
        assert_eq!(rgba8(t.canvas), (0x1e, 0x23, 0x30, 0xff));
        let tui = shelbi_app::theme::Theme::dark();
        assert_eq!(tui.canvas.rgb, shelbi_app::theme::Rgb::hex(0x1c1c1c), "TUI unchanged");
    }

    #[test]
    fn terminal_palette_follows_the_theme() {
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            let t = Theme::resolve(ThemePreference::Auto, system);
            assert_eq!(rgb_hsla(t.terminal.background), t.canvas, "the main surface");
            assert_eq!(rgb_hsla(t.terminal.cursor), t.text_loud);
            assert_eq!(rgb_hsla(t.terminal.cursor_text), t.canvas);
        }
        let dark = Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark).terminal;
        assert_eq!(dark.background, TermRgb::hex(0x1e2330), "theme.canvas");
        // theme.foreground (#f2f3f7 at 70%) over the canvas: the Orchestrator
        // frame's body text (#b2b5bb in the PNG, a unit off in green).
        assert_eq!(dark.foreground, TermRgb::hex(0xb2b4bb));
        assert_eq!(dark.ansi[1], TermRgb::hex(0xe04f52), "theme.red");
        assert_eq!(dark.ansi[9], TermRgb::hex(0xd54e53), "Ghostty's bright red");
        // Bold and dim default text: the frames' bold prompts (theme.white)
        // and dim tool output (theme.muted).
        assert_eq!(dark.bold_foreground, TermRgb::hex(0xf8f8fa));
        assert_eq!(dark.dim_foreground, TermRgb::hex(0x777a86));
        assert_eq!(dark.ansi[2], TermRgb::hex(0x5acd25), "theme.green");
        assert_eq!(dark.ansi[6], TermRgb::hex(0x00a6b2), "theme.cyan");
        assert_eq!(dark.ansi[4], TermRgb::hex(0x4a8fd7), "theme.ansi_blue");
        let light = Theme::resolve(ThemePreference::Light, WindowAppearance::Light).terminal;
        assert_eq!(light.ansi[1], TermRgb::hex(0xc23b3e), "theme.red");
        assert_eq!(light.ansi[2], TermRgb::hex(0x367b16), "theme.green");
        assert_eq!(light.ansi[6], TermRgb::hex(0x007a83), "theme.cyan");
        assert_eq!(light.bold_foreground, TermRgb::hex(0x020303), "theme.white");
        // theme.muted (#444c65 at 65%) over white.
        assert_eq!(light.dim_foreground, TermRgb::hex(0x858a9b));
        assert_ne!(
            light.ansi[7], light.background,
            "ANSI white must show on the light surface"
        );
    }

    #[test]
    fn terminal_text_stays_legible_on_both_surfaces() {
        // The default foreground (plain and bold), dim text and the ANSI
        // foreground hues a CLI (Claude Code included) prints in, on the
        // canvas. 3:1 for the hues and dim text (a terminal palette, not UI
        // text), AA for the default foreground.
        for system in [WindowAppearance::Dark, WindowAppearance::Light] {
            let t = Theme::resolve(ThemePreference::Auto, system).terminal;
            let rgb = |c: TermRgb| shelbi_app::theme::Rgb::new(c.r, c.g, c.b);
            let bg = rgb(t.background);
            assert!(rgb(t.foreground).contrast_ratio(bg) >= 4.5, "{system:?} fg");
            assert!(rgb(t.bold_foreground).contrast_ratio(bg) >= 4.5, "{system:?} bold fg");
            // Dim text is de-emphasis by design (theme.muted): held to the
            // hues' 3:1.
            let dim = rgb(t.dim_foreground).contrast_ratio(bg);
            assert!(dim >= 3.0, "{system:?} dim fg: {dim:.2}");
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
    fn shared_meanings_map_onto_figma_colors() {
        let t = Theme::resolve(ThemePreference::Light, WindowAppearance::Light);
        assert_eq!(t.decoration(DecorationColor::Green), t.icon_green, "working");
        assert_eq!(t.decoration(DecorationColor::Cyan), t.icon_cyan, "ready");
        assert_eq!(t.decoration(DecorationColor::Yellow), t.icon_yellow, "awaiting input");
        assert_eq!(t.decoration(DecorationColor::Muted), t.muted, "idle");
        assert_eq!(t.tone(Tone::Magenta), t.magenta, "[review]");
        assert_eq!(t.tone(Tone::Green), t.green, "[done]");
        assert_eq!(t.tone(Tone::Yellow), t.yellow, "[in progress]");
        assert_eq!(t.tone(Tone::Blue), t.blue, "[ready]");
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
