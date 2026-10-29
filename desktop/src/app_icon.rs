//! The app icon setting (`desktop.app_icon`): Command Grille, the bundle's
//! own icon and the default, or Retro.
//!
//! [`AppIconSetting`] is the choice in effect, a global: [`init`] reads it
//! from `config.yaml` at launch and [`set`] changes it (Settings ▸ General's
//! picker). Either one puts it in the Dock at once
//! ([`crate::platform::dock_icon`]); the About window and the picker
//! observe the global and redraw with it.
//!
//! The pictures the app draws itself are the icons as macOS shows them, mask
//! and shadow included, at twice the points they are drawn at (gpui samples
//! a texture without mipmaps, so a bigger picture drawn small would alias):
//! [`about_image`] at About's 112pt and [`tile_image`] at the picker's 64pt.
//! `assets/icon/README.md` has where each comes from.

use std::sync::{Arc, LazyLock};

use gpui::{App, Global, Image, ImageFormat};
use shelbi_state::AppIcon;

use crate::platform::dock_icon;

/// The icon in effect.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppIconSetting(pub AppIcon);

impl Global for AppIconSetting {}

/// Read `desktop.app_icon` and show it in the Dock. Call once at startup.
/// An unreadable config is the default, Command Grille.
pub fn init(cx: &mut App) {
    let cfg = shelbi_state::load_user_config().unwrap_or_default();
    set(AppIcon::from_config(&cfg.desktop), cx);
}

/// The icon in effect (Command Grille before [`init`]).
pub fn current(cx: &App) -> AppIcon {
    cx.try_global::<AppIconSetting>().copied().unwrap_or_default().0
}

/// Make `icon` the app's icon now: the Dock shows it, and what draws it
/// (About, the picker) repaints with it.
pub fn set(icon: AppIcon, cx: &mut App) {
    dock_icon::apply(icon);
    cx.set_global(AppIconSetting(icon));
}

fn png(bytes: &'static [u8]) -> Arc<Image> {
    Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec()))
}

/// The icons at 224px, for About's 112pt.
static ABOUT: LazyLock<[Arc<Image>; 2]> = LazyLock::new(|| {
    [
        png(include_bytes!("../assets/icon/command-grille-224.png")),
        png(include_bytes!("../assets/icon/retro-224.png")),
    ]
});

/// The icons at 128px, for the picker's 64pt tiles.
static TILE: LazyLock<[Arc<Image>; 2]> = LazyLock::new(|| {
    [
        png(include_bytes!("../assets/icon/command-grille-128.png")),
        png(include_bytes!("../assets/icon/retro-128.png")),
    ]
});

fn index(icon: AppIcon) -> usize {
    match icon {
        AppIcon::CommandGrille => 0,
        AppIcon::Retro => 1,
    }
}

/// `icon` for About, drawn at 112pt.
pub fn about_image(icon: AppIcon) -> Arc<Image> {
    ABOUT[index(icon)].clone()
}

/// `icon` for its tile in the picker, drawn at 64pt.
pub fn tile_image(icon: AppIcon) -> Arc<Image> {
    TILE[index(icon)].clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PNG's width and height and whether it is RGBA (its IHDR: width,
    /// height, then RGBA's color type 6).
    fn header(png: &[u8]) -> (u32, u32, bool) {
        let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
        assert_eq!(&png[12..16], b"IHDR");
        (be(16), be(20), png[25] == 6)
    }

    /// Each rendition is twice the points it is drawn at, with alpha.
    #[test]
    fn the_renditions_are_2x_with_alpha() {
        let about = crate::views::about::ICON as u32 * 2;
        for (png, size) in [
            (&include_bytes!("../assets/icon/command-grille-224.png")[..], about),
            (&include_bytes!("../assets/icon/retro-224.png")[..], about),
            (&include_bytes!("../assets/icon/command-grille-128.png")[..], 128),
            (&include_bytes!("../assets/icon/retro-128.png")[..], 128),
        ] {
            assert_eq!(header(png), (size, size, true));
        }
    }

    #[gpui::test]
    fn set_shows_the_icon_in_the_dock_and_unset_is_command_grille(cx: &mut gpui::TestAppContext) {
        dock_icon::tests::APPLIED.with(|a| a.borrow_mut().clear());
        assert_eq!(cx.update(|cx| current(cx)), AppIcon::CommandGrille);
        cx.update(|cx| set(AppIcon::Retro, cx));
        assert_eq!(cx.update(|cx| current(cx)), AppIcon::Retro);
        cx.update(|cx| set(AppIcon::CommandGrille, cx));
        assert_eq!(cx.update(|cx| current(cx)), AppIcon::CommandGrille);
        let applied = dock_icon::tests::APPLIED.with(|a| a.borrow().clone());
        assert_eq!(applied, [AppIcon::Retro, AppIcon::CommandGrille]);
    }
}
