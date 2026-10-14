//! Platform integration: everything the app needs from the machine before it
//! can talk to Shelbi.
//!
//! Filled by **`dt-platform-bootstrap`**: capture the user's login-shell
//! environment (an app launched from Finder doesn't get the shell's `PATH`),
//! discover the `shelbi` CLI (both Homebrew prefixes, `~/.cargo/bin`, a
//! settings override), ask it to start the daemon, check that the app and
//! CLI versions match exactly, and an error state for each step failing.
//!
//! Native menus and system notifications (`dt-menus-notifications`) and the
//! app bundle (`dt-packaging`) are platform concerns too, but live where
//! those subtasks put them.
