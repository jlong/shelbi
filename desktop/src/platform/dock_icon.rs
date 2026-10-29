//! The running app's Dock icon (`NSApplication.applicationIconImage`), set
//! from `desktop.app_icon` at launch and whenever Settings picks another
//! icon ([`crate::app_icon`]).
//!
//! The bundle's own icon is Command Grille, layered, which macOS draws with
//! its live glass in the Dock; choosing it puts that back (`nil`) rather than
//! a flat picture of it. Retro, and Command Grille in an unbundled
//! `cargo run` (which has no icon of its own), set a picture: the icon at
//! 512px, the Dock's largest magnified size on a Retina display.

use shelbi_state::AppIcon;

/// Command Grille as macOS draws it, at 512px (`assets/icon/`, rendered by
/// `scripts/app-icon.sh`).
const COMMAND_GRILLE: &[u8] = include_bytes!("../../assets/icon/command-grille-512.png");
/// Retro at 512px, cut from its master.
const RETRO: &[u8] = include_bytes!("../../assets/icon/retro-512.png");

/// The picture the Dock shows for `icon`.
fn png(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::CommandGrille => COMMAND_GRILLE,
        AppIcon::Retro => RETRO,
    }
}

/// Show `icon` in the Dock now. A test records it instead of touching the
/// test process's (nonexistent) Dock tile.
#[cfg(test)]
pub fn apply(icon: AppIcon) {
    tests::APPLIED.with(|a| a.borrow_mut().push(icon));
}

#[cfg(all(target_os = "macos", not(test)))]
pub fn apply(icon: AppIcon) {
    use objc2::{AllocAnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::{ns_string, NSBundle, NSData};

    let Some(mtm) = MainThreadMarker::new() else {
        tracing::warn!("the Dock icon can only be set on the main thread");
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    // A bundle that names its icon (Shelbi.app's CFBundleIconName) wears
    // Command Grille already.
    let bundled = NSBundle::mainBundle()
        .objectForInfoDictionaryKey(ns_string!("CFBundleIconName"))
        .is_some();
    let image = if icon == AppIcon::CommandGrille && bundled {
        None
    } else {
        let data = NSData::with_bytes(png(icon));
        let image = NSImage::initWithData(NSImage::alloc(), &data);
        if image.is_none() {
            tracing::error!(?icon, "could not decode the Dock icon");
            return;
        }
        image
    };
    // SAFETY: on the main thread (the marker), and `nil` is documented to
    // restore the bundle's icon.
    unsafe { app.setApplicationIconImage(image.as_deref()) };
}

#[cfg(all(not(target_os = "macos"), not(test)))]
pub fn apply(icon: AppIcon) {
    let _ = png(icon);
}

#[cfg(test)]
pub(crate) mod tests {
    use std::cell::RefCell;

    use super::*;

    thread_local! {
        /// The icons [`apply`] was asked to show, in order.
        pub(crate) static APPLIED: RefCell<Vec<AppIcon>> = const { RefCell::new(Vec::new()) };
    }

    /// Each Dock picture is a 512px RGBA PNG (its IHDR: width, height,
    /// then RGBA's color type 6), and the two differ.
    #[test]
    fn the_dock_pictures_are_512_with_alpha() {
        for icon in AppIcon::ALL {
            let png = png(icon);
            let be = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
            assert_eq!(&png[12..16], b"IHDR", "{icon:?}");
            assert_eq!((be(16), be(20)), (512, 512), "{icon:?}");
            assert_eq!(png[25], 6, "{icon:?}: RGBA");
        }
        assert_ne!(png(AppIcon::CommandGrille), png(AppIcon::Retro));
    }
}
