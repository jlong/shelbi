//! Platform integration: everything the app needs from the machine before it
//! can talk to Shelbi.
//!
//! The app is a client of the `shelbi` CLI package: it embeds neither the
//! daemon nor the session process. Launched from Finder it has no shell
//! `PATH`, so [`Bootstrap`] resolves the login-shell environment ([`env`]),
//! finds `shelbi` ([`discovery`]), has it start the daemon ([`daemon`]),
//! connects to the control socket and checks the versions match exactly
//! ([`control`], [`version`]), and fails at the step that broke with what its
//! screen needs to name the fix. `views::bootstrap` renders each state.
//!
//! Native menus and system notifications (`dt-menus-notifications`) and the
//! app bundle (`dt-packaging`) are platform concerns too, but live where
//! those subtasks put them.

pub mod bootstrap;
pub mod control;
pub mod daemon;
pub mod discovery;
pub mod env;
pub mod relaunch;
pub mod version;

#[cfg(test)]
mod integration_tests;

pub use bootstrap::{Bootstrap, BootstrapEvent, BootstrapState, Failure, Ready, SystemPlatform};
pub use control::RelaunchNotice;
