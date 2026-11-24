//! The running app's Dock icon (`NSApplication.applicationIconImage`), set
//! from `desktop.app_icon` at launch and whenever Settings picks another
//! icon ([`crate::app_icon`]). On Linux, the icon the icon theme gives the
//! app id (see the Linux [`apply`]).
//!
//! The bundle's own icon is Command Grille, layered, which macOS draws with
//! its live glass in the Dock; choosing it puts that back (`nil`) rather than
//! a flat picture of it. Retro, Racing S, Retro Racing S, and Command Grille
//! in an unbundled `cargo run` (which has no icon of its own), set a
//! picture: the icon at 512px, the Dock's largest magnified size on a Retina
//! display.

use shelbi_state::AppIcon;

/// Command Grille as macOS draws it, at 512px (`assets/icon/`, rendered by
/// `scripts/app-icon.sh`).
const COMMAND_GRILLE: &[u8] = include_bytes!("../../assets/icon/command-grille-512.png");
/// Retro at 512px, cut from its master.
const RETRO: &[u8] = include_bytes!("../../assets/icon/retro-512.png");
/// Racing S as macOS draws it, at 512px (rendered like Command Grille).
const RACING_S: &[u8] = include_bytes!("../../assets/icon/racing-s-512.png");
/// Retro Racing S at 512px, cut from its master (like Retro).
const RETRO_RACING_S: &[u8] = include_bytes!("../../assets/icon/retro-racing-s-512.png");

/// The picture the Dock shows for `icon`.
fn png(icon: AppIcon) -> &'static [u8] {
    match icon {
        AppIcon::CommandGrille => COMMAND_GRILLE,
        AppIcon::Retro => RETRO,
        AppIcon::RacingS => RACING_S,
        AppIcon::RetroRacingS => RETRO_RACING_S,
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

/// Linux: the icon the compositor's bar, Walker and the window switcher show
/// is the icon theme's for the window's app id ([`crate::platform::os::APP_ID`],
/// whose desktop entry names it), not one the app hands over. So the chosen
/// icon goes into the user's own hicolor theme
/// (`$XDG_DATA_HOME/icons/hicolor/<n>x<n>/apps/dev.shelbi.Desktop.png`, at
/// [`HICOLOR_SIZES`]), which is looked up before the package's
/// (`/usr/share/icons/hicolor`, Command Grille) and stands in for it at every
/// size. Written on a thread of its own, and only when the 512px one differs,
/// so a launch with the same icon writes nothing; the theme folder's time is
/// touched so icon caches look again. Shells that cache icons until a restart
/// show it from their next.
#[cfg(all(not(target_os = "macos"), not(test)))]
pub fn apply(icon: AppIcon) {
    let Some(dir) = user_hicolor() else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("shelbi-desktop-icon".into())
        .spawn(move || {
            if let Err(e) = linux::install(png(icon), &dir) {
                tracing::warn!(?icon, %e, dir = %dir.display(), "couldn't install the app icon");
            }
        });
    if let Err(e) = spawned {
        tracing::warn!(%e, "couldn't start the app icon install");
    }
}

/// The sizes the user's hicolor theme gets the icon at.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const HICOLOR_SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

/// `$XDG_DATA_HOME/icons/hicolor` (`~/.local/share/icons/hicolor`).
#[cfg(all(not(target_os = "macos"), not(test)))]
fn user_hicolor() -> Option<std::path::PathBuf> {
    let data = std::env::var_os("XDG_DATA_HOME")
        .filter(|d| !d.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/share")))?;
    Some(data.join("icons/hicolor"))
}

#[cfg(not(target_os = "macos"))]
mod linux {
    use std::path::{Path, PathBuf};

    use super::HICOLOR_SIZES;

    /// Where the icon goes at `px` in the theme folder `hicolor`.
    pub fn icon_path(hicolor: &Path, px: u32) -> PathBuf {
        hicolor
            .join(format!("{px}x{px}"))
            .join("apps")
            .join(format!("{}.png", crate::platform::os::APP_ID))
    }

    /// Write `png` (512px) into `hicolor` at every size, unless it is there
    /// already. Whether anything was written.
    pub fn install(png: &[u8], hicolor: &Path) -> Result<bool, String> {
        let largest = icon_path(hicolor, 512);
        if std::fs::read(&largest).is_ok_and(|have| have == png) {
            return Ok(false);
        }
        let source = image::load_from_memory_with_format(png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        // The largest last: it is the "already installed" check, so a run
        // cut short writes them all again next time.
        for px in HICOLOR_SIZES.into_iter().filter(|&px| px != 512) {
            let scaled = source.resize_exact(px, px, image::imageops::FilterType::Lanczos3);
            let path = icon_path(hicolor, px);
            create_parent(&path)?;
            scaled
                .save_with_format(&path, image::ImageFormat::Png)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        }
        create_parent(&largest)?;
        std::fs::write(&largest, png).map_err(|e| format!("{}: {e}", largest.display()))?;
        // Icon caches (GTK's, the bar's) compare the theme folder's time.
        if let Ok(dir) = std::fs::File::open(hicolor) {
            let _ = dir.set_modified(std::time::SystemTime::now());
        }
        Ok(true)
    }

    fn create_parent(path: &Path) -> Result<(), String> {
        let parent = path.parent().ok_or("no folder")?;
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))
    }
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

    /// On Linux the icon lands in the user's hicolor theme at every size,
    /// once: the same icon again writes nothing, another replaces it.
    #[cfg(not(target_os = "macos"))]
    #[test]
    fn the_icon_installs_into_the_hicolor_theme_at_every_size() {
        let dir = std::env::temp_dir().join(format!("shelbi-hicolor-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(super::linux::install(png(AppIcon::Retro), &dir), Ok(true));
        for px in HICOLOR_SIZES {
            let file = std::fs::read(super::linux::icon_path(&dir, px)).expect("written");
            let be = |at: usize| u32::from_be_bytes(file[at..at + 4].try_into().unwrap());
            assert_eq!((be(16), be(20)), (px, px), "{px}");
        }
        assert!(dir.join("48x48/apps/dev.shelbi.Desktop.png").is_file());
        assert_eq!(super::linux::install(png(AppIcon::Retro), &dir), Ok(false), "already there");
        assert_eq!(super::linux::install(png(AppIcon::RacingS), &dir), Ok(true));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
