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
use shelbi_app::theme::{resolve_appearance, Appearance, Rgb, Theme as Tokens, ThemePreference};

/// The resolved theme, installed as a gpui [`Global`]. Views read the tokens
/// through it (`cx.global::<Theme>().gray`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Theme {
    /// The setting this theme was resolved from, kept so an appearance change
    /// can re-resolve `auto`.
    pub preference: ThemePreference,
    colors: Tokens<Hsla>,
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
            colors: tokens.map(|swatch| hsla(swatch.rgb)),
        }
    }

    pub fn is_dark(&self) -> bool {
        self.appearance == Appearance::Dark
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
