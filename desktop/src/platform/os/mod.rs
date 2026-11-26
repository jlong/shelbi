//! The per-OS platform interface: everything the app does differently on
//! macOS and on Linux, behind one set of names.
//!
//! Each OS has a sibling file with the same items: `macos.rs` (AppKit,
//! Homebrew, Finder) and `linux.rs` (XDG, the AUR packages, `xdg-open`,
//! freedesktop notifications). Callers use `platform::os::<item>` and never
//! `cfg` on the OS themselves. Any OS that isn't macOS gets the Linux
//! implementations: they are plain XDG and POSIX, which is right for the
//! BSDs too.
//!
//! What lives here:
//!
//! - where the `shelbi` CLI is installed ([`known_cli_dirs`]), whether the
//!   app installs it itself ([`INSTALLS_CLI`]), and the package manager's
//!   install and upgrade commands the screens name;
//! - opening a URL, file or folder with the system's handler ([`open`]),
//!   a file in a text editor ([`open_text`]), a file in a chosen app
//!   ([`open_with_app`]), a command line in a terminal ([`run_in_terminal`]);
//! - relaunching the app ([`relaunch_command`]);
//! - the folders the open and save panels start in ([`BROWSE_START`],
//!   [`downloads_dir`]);
//! - the app's identity for notifications ([`set_identity`]) and the login
//!   shell a thin launcher environment leaves unnamed
//!   ([`adopt_login_shell`]).
//!
//! The system services gpui already implements per OS (the clipboard, system
//! notifications, the save panel) aren't wrapped again.

#[cfg_attr(target_os = "macos", path = "macos.rs")]
#[cfg_attr(not(target_os = "macos"), path = "linux.rs")]
mod imp;

pub use imp::*;

/// The app's reverse-DNS id: every window's Wayland app id (X11 `WM_CLASS`),
/// which Hyprland's window rules match and which names the `.desktop` file
/// (`packaging/linux/dev.shelbi.Desktop.desktop`) and the hicolor icons the
/// compositor and the bar show for it. gpui ignores it on macOS, whose
/// bundle names itself in `Info.plist`.
pub const APP_ID: &str = "dev.shelbi.Desktop";

/// The app's display name, as notifications carry it on Linux.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub const APP_NAME: &str = "Shelbi";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_cli_dirs_end_with_the_scripted_install_dir() {
        let dirs = known_cli_dirs(Some(std::path::Path::new("/home/me")));
        assert_eq!(dirs.last(), Some(&std::path::PathBuf::from("/home/me/bin")));
        assert!(known_cli_dirs(None).iter().all(|d| d.is_absolute()));
    }

    #[test]
    fn every_command_the_screens_name_is_one_line() {
        for cmd in [INSTALL_COMMAND, UPGRADE_COMMAND, APP_UPGRADE_COMMAND] {
            assert!(!cmd.is_empty() && !cmd.contains('\n'), "{cmd:?}");
        }
    }
}
