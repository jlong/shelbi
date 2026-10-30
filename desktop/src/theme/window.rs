//! The windows' translucency: how much of the desktop shows through a window
//! (`desktop.window_opacity`) and how much what shows through is blurred
//! (`desktop.window_blur`), Ghostty's `background-opacity` and
//! `background-blur`.
//!
//! The opacity is a [`Theme`] resolve-time change, not an opacity layer: the
//! window's surface roles (`canvas`, `sidebar`) take it as their alpha, the
//! terminals' default background follows them, and the opaque fills on them
//! become tints of the glass ([`super::glass`]), while text, icons, borders
//! and overlays keep their own colors ([`Theme::opaque`] for an overlay), so
//! content stays as readable as it is opaque. Below
//! 100% the window's background is [`WindowBackgroundAppearance::Transparent`],
//! and the blur is the window server's own background blur at the chosen
//! radius ([`crate::platform::blur`]).
//!
//! Reduce Transparency (System Settings ▸ Accessibility ▸ Display) makes
//! every window opaque whatever the setting says; [`sync_reduce_transparency`]
//! reads it when a window activates, as Reduce Motion is.

use std::ops::RangeInclusive;

use gpui::{App, Global, Window, WindowBackgroundAppearance};
use shelbi_state::DesktopConfig;

use super::Theme;

/// The opacity's range, in percent. The floor keeps body text at 3:1 or
/// better over a mid-gray backdrop in both themes (what a blurred desktop
/// tends to average to), and at 30% the window's own color still reads as
/// the window's.
pub const OPACITY_RANGE: RangeInclusive<u8> = 30..=100;

/// The blur radius's range, in points. Past 50 the window server's blur
/// reads as a flat wash: there is nothing left of what is behind to blur.
pub const BLUR_RANGE: RangeInclusive<u8> = 0..=50;

/// The opacity and blur a window is drawn with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Translucency {
    /// Percent, within [`OPACITY_RANGE`]: 100 is opaque.
    pub opacity: u8,
    /// The blur radius in points, within [`BLUR_RANGE`]: 0 is clear.
    pub blur: u8,
}

impl Default for Translucency {
    /// Opaque, unblurred: the app as it looks without the setting.
    fn default() -> Self {
        Translucency { opacity: 100, blur: 0 }
    }
}

impl Translucency {
    /// `opacity` and `blur` clamped into their ranges.
    pub fn clamped(opacity: i32, blur: i32) -> Self {
        let clamp = |v: i32, r: &RangeInclusive<u8>| v.clamp(*r.start() as i32, *r.end() as i32) as u8;
        Translucency {
            opacity: clamp(opacity, &OPACITY_RANGE),
            blur: clamp(blur, &BLUR_RANGE),
        }
    }

    /// The setting in `cfg`, clamped (unset is opaque and clear).
    pub fn from_config(cfg: &DesktopConfig) -> Self {
        let d = Translucency::default();
        Translucency::clamped(
            cfg.window_opacity.map_or(d.opacity, u8::into) as i32,
            cfg.window_blur.map_or(d.blur, u8::into) as i32,
        )
    }

    /// Write the setting into `cfg`; a default isn't written.
    pub fn apply(&self, cfg: &mut DesktopConfig) {
        let d = Translucency::default();
        cfg.window_opacity = (self.opacity != d.opacity).then_some(self.opacity);
        cfg.window_blur = (self.blur != d.blur).then_some(self.blur);
    }

    /// The opacity as an alpha, 0 to 1.
    pub fn alpha(&self) -> f32 {
        self.opacity as f32 / 100.0
    }

    pub fn is_opaque(&self) -> bool {
        self.opacity >= 100
    }

    /// The window background this asks gpui for. A blur on an opaque window
    /// would blur nothing, so opaque is opaque whatever the radius.
    pub fn background(&self) -> WindowBackgroundAppearance {
        if self.is_opaque() {
            WindowBackgroundAppearance::Opaque
        } else {
            WindowBackgroundAppearance::Transparent
        }
    }

    /// The blur radius the window server applies: none on an opaque window.
    pub fn blur_radius(&self) -> u8 {
        if self.is_opaque() {
            0
        } else {
            self.blur
        }
    }
}

/// The translucency setting and the system's Reduce Transparency, installed
/// as a gpui [`Global`]. [`WindowLook::effective`] is what windows draw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowLook {
    /// What the user chose (`config.yaml`, Settings ▸ General).
    pub setting: Translucency,
    /// Reduce Transparency is on: every window is opaque.
    pub reduce_transparency: bool,
}

impl Global for WindowLook {}

impl WindowLook {
    /// What windows are drawn with: the setting, or opaque under Reduce
    /// Transparency.
    pub fn effective(&self) -> Translucency {
        if self.reduce_transparency {
            Translucency { opacity: 100, blur: 0 }
        } else {
            self.setting
        }
    }

    /// The installed look (the default before [`init`]).
    pub fn current(cx: &App) -> WindowLook {
        cx.try_global::<WindowLook>().copied().unwrap_or_default()
    }
}

/// Install the setting read from `config.yaml` (before the first window
/// opens; [`super::init`] calls it).
pub(super) fn init(setting: Translucency, cx: &mut App) {
    cx.set_global(WindowLook {
        setting,
        reduce_transparency: reduce_transparency(),
    });
}

/// Change the setting: the theme's surfaces and every open window follow at
/// once (Settings' sliders call it as they move).
pub fn set(setting: Translucency, cx: &mut App) {
    let look = WindowLook {
        setting,
        ..WindowLook::current(cx)
    };
    install(look, cx);
}

/// Re-read Reduce Transparency (on window activation): a change re-applies
/// the look to every window.
pub fn sync_reduce_transparency(cx: &mut App) {
    let look = WindowLook {
        reduce_transparency: reduce_transparency(),
        ..WindowLook::current(cx)
    };
    if look != WindowLook::current(cx) {
        install(look, cx);
    }
}

/// The system setting; a test's app never reads the machine's.
fn reduce_transparency() -> bool {
    !cfg!(test) && crate::platform::blur::reduce_transparency()
}

/// Turn Reduce Transparency on or off as the system would (a test's app
/// never reads the machine's).
#[cfg(test)]
pub fn set_reduce_transparency(on: bool, cx: &mut App) {
    let look = WindowLook {
        reduce_transparency: on,
        ..WindowLook::current(cx)
    };
    install(look, cx);
}

/// What `id` was last drawn with (opaque before any apply).
#[cfg(test)]
pub fn applied(id: gpui::WindowId) -> Translucency {
    APPLIED.with(|a| a.borrow().get(&id).copied().unwrap_or_default())
}

fn install(look: WindowLook, cx: &mut App) {
    let changed = look != WindowLook::current(cx);
    cx.set_global(look);
    let alpha = look.effective().alpha();
    if let Some(theme) = cx.try_global::<Theme>().copied() {
        if theme.opacity() != alpha {
            super::install(theme.with_opacity(alpha), cx);
        }
    }
    if changed {
        // Deferred: a slider moves inside its own window's update, which
        // holds that window.
        cx.defer(move |cx| {
            for handle in cx.windows() {
                let _ = handle.update(cx, |_, window, _| apply(look.effective(), window));
            }
        });
    }
}

thread_local! {
    /// What each window was last drawn with, so a re-apply that changes
    /// nothing doesn't touch AppKit.
    static APPLIED: std::cell::RefCell<std::collections::HashMap<gpui::WindowId, Translucency>> =
        Default::default();
}

/// Draw `window` with the installed look: its background and blur. Call
/// once a window exists (the blur is the window server's, which needs the
/// window's number). Re-applying what a window already has does nothing.
pub fn apply_to(window: &mut Window, cx: &App) {
    apply(WindowLook::current(cx).effective(), window);
}

fn apply(look: Translucency, window: &mut Window) {
    let id = window.window_handle().window_id();
    let before = APPLIED.with(|a| a.borrow_mut().insert(id, look));
    // A window opens opaque with no blur.
    let before = before.unwrap_or_default();
    if before.background() != look.background() {
        window.set_background_appearance(look.background());
    }
    if before.blur_radius() != look.blur_radius() {
        crate::platform::blur::set_blur_radius(window, look.blur_radius());
    }
}

/// Forget a closed window.
pub fn forget(id: gpui::WindowId) {
    APPLIED.with(|a| a.borrow_mut().remove(&id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_clamps_into_its_ranges() {
        assert_eq!(Translucency::clamped(5, -3), Translucency { opacity: 30, blur: 0 });
        assert_eq!(Translucency::clamped(140, 400), Translucency { opacity: 100, blur: 50 });
        assert_eq!(Translucency::clamped(85, 20), Translucency { opacity: 85, blur: 20 });
        let cfg = DesktopConfig {
            window_opacity: Some(0),
            window_blur: Some(255),
            ..DesktopConfig::default()
        };
        assert_eq!(Translucency::from_config(&cfg), Translucency { opacity: 30, blur: 50 });
    }

    #[test]
    fn the_setting_round_trips_and_defaults_are_not_written() {
        let mut cfg = DesktopConfig::default();
        assert_eq!(Translucency::from_config(&cfg), Translucency::default());
        let set = Translucency { opacity: 85, blur: 20 };
        set.apply(&mut cfg);
        assert_eq!((cfg.window_opacity, cfg.window_blur), (Some(85), Some(20)));
        assert_eq!(Translucency::from_config(&cfg), set);
        Translucency { opacity: 100, blur: 20 }.apply(&mut cfg);
        assert_eq!((cfg.window_opacity, cfg.window_blur), (None, Some(20)), "a blur is kept for later");
        Translucency::default().apply(&mut cfg);
        assert!(cfg.is_empty());
    }

    #[test]
    fn opaque_is_opaque_whatever_the_blur() {
        let opaque = Translucency { opacity: 100, blur: 30 };
        assert_eq!(opaque.background(), WindowBackgroundAppearance::Opaque);
        assert_eq!(opaque.blur_radius(), 0);
        let glass = Translucency { opacity: 85, blur: 20 };
        assert_eq!(glass.background(), WindowBackgroundAppearance::Transparent);
        assert_eq!((glass.blur_radius(), glass.alpha()), (20, 0.85));
    }

    #[test]
    fn reduce_transparency_forces_opaque() {
        let look = WindowLook {
            setting: Translucency { opacity: 50, blur: 40 },
            reduce_transparency: true,
        };
        assert_eq!(look.effective(), Translucency::default());
        assert_eq!(WindowLook { reduce_transparency: false, ..look }.effective(), look.setting);
    }
}
