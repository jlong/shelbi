//! The control connection the shell uses once bootstrap is `Ready`, and the
//! watch that notices the CLI was upgraded under a running app.
//!
//! A version change while the app runs reaches it two ways: the daemon pushes
//! `Reexec` to a subscribed client whose hello version differs from its own
//! (and on `shelbi reload`), and a daemon restarted onto a new binary answers a
//! later hello with the new version. Either one flips the handle into
//! "relaunch required": [`ControlHandle::connect`] then refuses, so no further
//! command reaches the daemon. Session views don't go through this handle (the
//! session protocol is compatible across versions) and keep streaming.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelbi_client::{ClientError, ControlClient, Notice};
use shelbi_proto::control::{ClientInfo, ClientKind};

/// How long a control-socket hello may take before the app gives up on it.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The watch's pause before reconnecting after the daemon went away (it exits
/// when no project is open, and restarts on upgrade).
const RECONNECT_PAUSE: Duration = Duration::from_secs(2);

/// Why the app must be relaunched before it sends another command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelaunchNotice {
    pub app_version: String,
    /// The daemon's version, when the app learned it from a hello.
    pub daemon_version: Option<String>,
    /// The daemon's reason, or the app's own.
    pub reason: String,
}

/// A command could not be sent.
#[allow(dead_code)] // returned to the shell's commands (dt-shell-sidebar on)
#[derive(Debug)]
pub enum ControlError {
    /// The app is out of date; nothing is sent until it is relaunched.
    RelaunchRequired(RelaunchNotice),
    /// The control socket refused or failed.
    Client(ClientError),
}

impl std::fmt::Display for ControlError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ControlError::RelaunchRequired(_) => {
                write!(
                    f,
                    "Shelbi was updated; relaunch the app before making changes"
                )
            }
            ControlError::Client(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ControlError {}

/// The daemon's control socket, reached and version-checked by bootstrap.
/// Cheap to clone; every clone shares the relaunch state.
#[derive(Debug, Clone)]
pub struct ControlHandle {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    socket: PathBuf,
    app_version: String,
    daemon_version: String,
    relaunch: Mutex<Option<RelaunchNotice>>,
}

// `socket`, `app_version`, and `connect` are the shell's API; the
// foundation's commands are still stubs.
#[allow(dead_code)]
impl ControlHandle {
    pub fn new(socket: PathBuf, app_version: String, daemon_version: String) -> Self {
        Self {
            inner: Arc::new(Inner {
                socket,
                app_version,
                daemon_version,
                relaunch: Mutex::new(None),
            }),
        }
    }

    pub fn socket(&self) -> &Path {
        &self.inner.socket
    }

    pub fn app_version(&self) -> &str {
        &self.inner.app_version
    }

    /// The daemon version at bootstrap's hello.
    pub fn daemon_version(&self) -> &str {
        &self.inner.daemon_version
    }

    /// Set once the app must be relaunched; never cleared.
    pub fn relaunch_required(&self) -> Option<RelaunchNotice> {
        self.lock().clone()
    }

    /// Record that the app must be relaunched. Returns `true` the first time
    /// (the caller announces it), `false` when it was already required.
    pub fn require_relaunch(&self, notice: RelaunchNotice) -> bool {
        let mut slot = self.lock();
        if slot.is_some() {
            return false;
        }
        tracing::warn!(reason = %notice.reason, "the Shelbi CLI changed under the app; relaunch required");
        *slot = Some(notice);
        true
    }

    /// Open a control connection for a command. Refused once a relaunch is
    /// required, and a hello that reports a different daemon version makes it
    /// required (the CLI was upgraded and the daemon restarted on it).
    pub fn connect(&self) -> Result<ControlClient, ControlError> {
        if let Some(notice) = self.relaunch_required() {
            return Err(ControlError::RelaunchRequired(notice));
        }
        let client =
            hello(&self.inner.socket, &self.inner.app_version).map_err(ControlError::Client)?;
        if client.is_out_of_date(&self.inner.app_version) {
            let notice = self.notice(Some(client.daemon_version.clone()), None);
            self.require_relaunch(notice.clone());
            return Err(ControlError::RelaunchRequired(notice));
        }
        Ok(client)
    }

    /// Watch for a version change on a background thread: subscribe as an
    /// attached desktop client, treat a `Reexec` push or a reconnect hello
    /// with another version as "relaunch required", and call `on_relaunch`
    /// once when that happens. The thread ends then.
    pub fn watch(&self, on_relaunch: impl Fn(RelaunchNotice) + Send + 'static) {
        let handle = self.clone();
        let spawned = std::thread::Builder::new()
            .name("shelbi-desktop-control-watch".into())
            .spawn(move || handle.watch_loop(on_relaunch));
        if let Err(e) = spawned {
            tracing::error!("could not start the control watch: {e}");
        }
    }

    fn watch_loop(&self, on_relaunch: impl Fn(RelaunchNotice)) {
        let info = ClientInfo {
            client_id: shelbi_client::fresh_client_id(),
            kind: ClientKind::Desktop,
            host: shelbi_client::hostname(),
            pid: Some(std::process::id()),
            project: None,
        };
        let announce = |notice: RelaunchNotice| {
            if self.require_relaunch(notice.clone()) {
                on_relaunch(notice);
            }
        };
        loop {
            if self.relaunch_required().is_some() {
                return;
            }
            let client = match hello(&self.inner.socket, &self.inner.app_version) {
                Ok(client) => client,
                Err(_) => {
                    // No daemon right now (idle exit, or mid-restart).
                    std::thread::sleep(RECONNECT_PAUSE);
                    continue;
                }
            };
            if client.is_out_of_date(&self.inner.app_version) {
                announce(self.notice(Some(client.daemon_version.clone()), None));
                return;
            }
            let daemon_version = client.daemon_version.clone();
            let Ok(mut sub) = client.subscribe_as(info.clone()) else {
                std::thread::sleep(RECONNECT_PAUSE);
                continue;
            };
            loop {
                match sub.recv() {
                    Ok(Some(Notice::Reexec { reason })) => {
                        announce(self.notice(Some(daemon_version), Some(reason)));
                        return;
                    }
                    // Detach is the shell's to handle (dt-shell-sidebar).
                    Ok(Some(Notice::Detach { reason })) => {
                        tracing::info!(%reason, "daemon asked this client to detach");
                    }
                    Ok(Some(Notice::Changed(_))) => {}
                    Ok(None) | Err(_) => break,
                }
            }
            std::thread::sleep(RECONNECT_PAUSE);
        }
    }

    fn notice(&self, daemon_version: Option<String>, reason: Option<String>) -> RelaunchNotice {
        let reason = reason.unwrap_or_else(|| match &daemon_version {
            Some(v) => format!("the daemon is now {v}"),
            None => "the daemon asked the app to relaunch".to_string(),
        });
        RelaunchNotice {
            app_version: self.inner.app_version.clone(),
            daemon_version,
            reason,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<RelaunchNotice>> {
        self.inner
            .relaunch
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }
}

/// Connect and complete the hello, giving up after [`HELLO_TIMEOUT`] (the
/// client's hello read has no deadline of its own, and a wedged daemon must
/// not wedge bootstrap). A timed-out attempt's thread is left to finish on
/// its own.
pub fn hello(socket: &Path, app_version: &str) -> Result<ControlClient, ClientError> {
    let (tx, rx) = std::sync::mpsc::channel();
    let socket = socket.to_path_buf();
    let app_version = app_version.to_string();
    std::thread::Builder::new()
        .name("shelbi-desktop-hello".into())
        .spawn(move || {
            let _ = tx.send(ControlClient::connect(&socket, &app_version));
        })
        .map_err(ClientError::Io)?;
    rx.recv_timeout(HELLO_TIMEOUT)
        .unwrap_or(Err(ClientError::HandshakeTimeout))
}
