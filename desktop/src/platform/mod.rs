//! Platform integration: everything the app needs from the machine before it
//! can talk to Shelbi.
//!
//! The app is a client of the `shelbi` CLI package: it embeds neither the
//! daemon nor the session process. Launched from Finder it has no shell
//! `PATH`, so [`Bootstrap`] resolves the login-shell environment ([`env`]),
//! finds `shelbi` ([`discovery`]) or, when there is none, installs the
//! release matching the app ([`install`]), has it start the daemon ([`daemon`]),
//! connects to the control socket and checks the versions match exactly
//! ([`control`], [`version`]), and fails at the step that broke with what its
//! screen needs to name the fix. `views::bootstrap` renders each state.
//!
//! `--print-version` ([`print_version`]) is bootstrap's headless check, and
//! [`update`] the once-a-day new-release check. [`font_smoothing`] turns
//! macOS stem thickening off for the app at launch, so text draws at the
//! frames' weight. Native menus and system
//! notifications (`dt-menus-notifications`) are platform concerns too, but
//! live where that subtask puts them; the app bundle is `scripts/bundle.sh`.

pub mod app_icons;
pub mod blur;
pub mod bootstrap;
pub mod control;
pub mod daemon;
pub mod discovery;
pub mod dock_icon;
pub mod env;
pub mod feedback;
pub mod font_smoothing;
pub mod install;
pub mod launch;
pub mod motion;
pub mod open_panel;
pub mod print_version;
pub mod relaunch;
pub mod scrollers;
pub mod update;
pub mod version;

#[cfg(test)]
mod integration_tests;

pub use bootstrap::{Bootstrap, BootstrapEvent, BootstrapState, Failure, Ready, SystemPlatform};
pub use control::RelaunchNotice;
