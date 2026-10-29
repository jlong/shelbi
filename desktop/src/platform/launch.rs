//! What the launch window and the Connect to Remote Session dialog run
//! (`shelbi_app::launch`): the remote connector, and the daemon start behind
//! opening a local project.
//!
//! - [`LaunchServices`] is the [`RemoteConnector`] both run and the recent
//!   remote sessions they keep, a seam a test (or the `launch-remote-connect`
//!   work, with the real SSH connector) sets. With none set the app has
//!   [`Unavailable`], whose every call says connecting isn't in this build
//!   yet, and the list in the Shelbi home; a test gets the foundation's
//!   [`shelbi_app::launch::FakeRemote`] and no list, never a real `ssh` or
//!   the real home.
//! - [`BootstrapDaemon`] starts the daemon the way a project window does:
//!   the app's bootstrap (find the CLI, installing it when there is none,
//!   start the daemon, connect). Once a window has bootstrapped, the CLI's
//!   launcher is installed and the shared on-demand start does
//!   ([`shelbi_app::launch::SharedDaemon`]).

use std::sync::{Arc, Mutex};

use gpui::{App, Global};
use shelbi_app::launch::{DaemonStarter, RecentRemotes, RemoteConnector, RemoteError, RemoteSession};

use super::version::APP_VERSION;
use super::{Bootstrap, BootstrapEvent, BootstrapState, Ready, SystemPlatform};

/// Why a remote step fails until the real connector is in the build.
pub const UNAVAILABLE: &str = "Connecting to Shelbi on another machine isn't available in this build yet.";

/// What the launch window and the Connect dialog run.
#[derive(Clone)]
pub struct LaunchServices {
    pub remote: Arc<dyn RemoteConnector>,
    pub recents: RecentRemotes,
}

impl Global for LaunchServices {}

/// The services set for the app, else the build's defaults.
pub fn services(cx: &App) -> LaunchServices {
    if let Some(s) = cx.try_global::<LaunchServices>() {
        return s.clone();
    }
    #[cfg(test)]
    return LaunchServices {
        remote: Arc::new(shelbi_app::launch::FakeRemote::new()),
        recents: RecentRemotes::none(),
    };
    #[cfg(not(test))]
    LaunchServices {
        remote: Arc::new(Unavailable),
        recents: RecentRemotes::live(),
    }
}

/// The connector before `launch-remote-connect`: refuses, in words the
/// dialog shows.
pub struct Unavailable;

impl RemoteConnector for Unavailable {
    fn find_sessions(&self, host: &str) -> Result<(String, Vec<RemoteSession>), RemoteError> {
        shelbi_app::launch::validate_host(host)?;
        Err(RemoteError::Failed(UNAVAILABLE.into()))
    }

    fn connect(&self, _: &RemoteSession) -> Result<(), RemoteError> {
        Err(RemoteError::Failed(UNAVAILABLE.into()))
    }
}

/// A finished bootstrap: its result, and its events (the daemon's relaunch
/// and detach pushes still arrive on them).
pub type Booted = (Ready, async_channel::Receiver<BootstrapEvent>);

/// The daemon start for a project opened from the launch window: a
/// bootstrap run, blocking until it is ready or fails. The run is kept for
/// the window to share ([`BootstrapDaemon::take`]).
pub struct BootstrapDaemon {
    client_id: String,
    booted: Mutex<Option<Booted>>,
}

impl BootstrapDaemon {
    pub fn new(client_id: String) -> Self {
        BootstrapDaemon {
            client_id,
            booted: Mutex::new(None),
        }
    }

    /// The finished run, once [`DaemonStarter::ensure_running`] succeeded.
    pub fn take(&self) -> Option<Booted> {
        self.booted.lock().unwrap_or_else(|p| p.into_inner()).take()
    }
}

impl DaemonStarter for BootstrapDaemon {
    fn ensure_running(&self) -> Result<(), String> {
        let events = Bootstrap::spawn_as(SystemPlatform::default(), APP_VERSION.to_string(), self.client_id.clone());
        while let Ok(event) = events.recv_blocking() {
            match event {
                BootstrapEvent::State(BootstrapState::Ready(ready)) => {
                    *self.booted.lock().unwrap_or_else(|p| p.into_inner()) = Some((ready, events));
                    return Ok(());
                }
                BootstrapEvent::State(BootstrapState::Failed(failure)) => {
                    return Err(crate::views::bootstrap::failure_screen(&failure).title);
                }
                _ => {}
            }
        }
        Err("Shelbi stopped starting up.".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unavailable_connector_says_so_but_still_checks_the_host() {
        let remote = Unavailable;
        assert_eq!(
            remote.find_sessions("devbox").unwrap_err(),
            RemoteError::Failed(UNAVAILABLE.into())
        );
        assert!(matches!(remote.find_sessions("-x"), Err(RemoteError::InvalidHost(_))));
    }
}
