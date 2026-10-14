//! The bootstrap state machine: from a bare launch to a version-checked
//! control connection.
//!
//! `ResolvingEnv → FindingCli → StartingDaemon → Connecting → Ready`, or
//! `Failed(kind)` at the step that broke. [`run`] is the machine, written
//! against the [`Platform`] seam so every path is tested with a scripted
//! platform; [`Bootstrap::spawn`] runs it on its own thread with the real
//! [`SystemPlatform`] and publishes each state (and, after `Ready`, a
//! relaunch notice) over a channel the root view reads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::control::{self, ControlHandle, RelaunchNotice};
use super::daemon::{self, DaemonFailure};
use super::discovery::{self, CliLocation, CliNotFound};
use super::env::{self, Env};
use super::version::{self, VersionCheck};

/// How long `Connecting` keeps retrying a control socket that isn't there yet
/// (the daemon binds its hub socket, which the start waits on, a moment
/// before its control socket).
const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const CONNECT_RETRY: Duration = Duration::from_millis(100);

/// Where bootstrap is.
#[derive(Debug, Clone)]
pub enum BootstrapState {
    ResolvingEnv,
    FindingCli,
    StartingDaemon { cli: CliLocation },
    Connecting { cli: CliLocation },
    Ready(Ready),
    Failed(Failure),
}

impl BootstrapState {
    /// A one-line progress label for the in-flight states.
    pub fn progress_label(&self) -> Option<&'static str> {
        match self {
            BootstrapState::ResolvingEnv => Some("Reading your shell environment"),
            BootstrapState::FindingCli => Some("Looking for the shelbi CLI"),
            BootstrapState::StartingDaemon { .. } => Some("Starting the Shelbi daemon"),
            BootstrapState::Connecting { .. } => Some("Connecting to the daemon"),
            BootstrapState::Ready(_) | BootstrapState::Failed(_) => None,
        }
    }
}

/// Everything the shell needs from bootstrap.
#[derive(Debug, Clone)]
pub struct Ready {
    pub cli: CliLocation,
    /// `shelbi --version`, when it answered.
    pub cli_version: Option<String>,
    /// The login environment `shelbi`, `git`, `gh`, and `ssh` run under.
    #[allow(dead_code)] // for the shell's child processes (dt-shell-sidebar on)
    pub env: Arc<Env>,
    pub control: ControlHandle,
}

/// The step that failed, with what its screen needs to name the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    CliNotFound(CliNotFound),
    DaemonFailedToStart {
        cli: PathBuf,
        failure: DaemonFailure,
    },
    ControlUnreachable {
        socket: PathBuf,
        error: String,
    },
    /// Never [`VersionCheck::Match`].
    VersionMismatch(VersionCheck),
}

/// What [`Bootstrap::spawn`] publishes.
#[derive(Debug, Clone)]
pub enum BootstrapEvent {
    State(BootstrapState),
    /// After `Ready`: the CLI changed under the running app. Shown as a
    /// relaunch prompt over the shell; the handle in `Ready` already refuses
    /// commands.
    RelaunchRequired(RelaunchNotice),
    /// After `Ready`: the daemon told this client to detach (`shelbi detach`,
    /// another client's "Detach Other Clients"). Carries the reason.
    Detach(String),
}

/// A connected, hello-complete control socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connected {
    pub socket: PathBuf,
    pub daemon_version: String,
}

/// What bootstrap needs from the machine.
pub trait Platform {
    /// The login-shell environment for the app's children.
    fn login_env(&mut self) -> Env;
    /// `desktop.cli_path` from `config.yaml`.
    fn cli_setting(&mut self) -> Result<Option<PathBuf>, String>;
    fn home_dir(&self) -> Option<PathBuf>;
    fn is_executable(&self, path: &Path) -> bool;
    fn cli_version(&mut self, cli: &Path, env: &Env) -> Option<String>;
    /// Ensure a daemon is running, starting it with `<cli> daemon`.
    fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure>;
    /// Connect to the control socket and complete the hello.
    fn connect(&mut self, app_version: &str) -> Result<Connected, (PathBuf, String)>;
}

/// Run bootstrap to its end, calling `publish` with each state entered (the
/// last one, `Ready` or `Failed`, is also returned).
pub fn run(
    platform: &mut impl Platform,
    app_version: &str,
    mut publish: impl FnMut(&BootstrapState),
) -> BootstrapState {
    let mut enter = |state: BootstrapState| {
        publish(&state);
        state
    };

    enter(BootstrapState::ResolvingEnv);
    let env = platform.login_env();

    enter(BootstrapState::FindingCli);
    let cli = match find_cli(platform, &env) {
        Ok(cli) => cli,
        Err(not_found) => return enter(BootstrapState::Failed(Failure::CliNotFound(not_found))),
    };
    tracing::info!(cli = %cli.path.display(), source = cli.source.label(), "found the shelbi CLI");
    let cli_version = platform.cli_version(&cli.path, &env);

    enter(BootstrapState::StartingDaemon { cli: cli.clone() });
    if let Err(failure) = platform.start_daemon(&cli.path, &env) {
        return enter(BootstrapState::Failed(Failure::DaemonFailedToStart {
            cli: cli.path.clone(),
            failure,
        }));
    }

    enter(BootstrapState::Connecting { cli: cli.clone() });
    let connected = match platform.connect(app_version) {
        Ok(connected) => connected,
        Err((socket, error)) => {
            return enter(BootstrapState::Failed(Failure::ControlUnreachable {
                socket,
                error,
            }))
        }
    };
    match version::check(
        app_version,
        cli_version.as_deref(),
        &connected.daemon_version,
    ) {
        VersionCheck::Match => enter(BootstrapState::Ready(Ready {
            cli,
            cli_version,
            env: Arc::new(env),
            control: ControlHandle::new(
                connected.socket,
                app_version.to_string(),
                connected.daemon_version,
            ),
        })),
        mismatch => enter(BootstrapState::Failed(Failure::VersionMismatch(mismatch))),
    }
}

/// Find the `shelbi` CLI the way bootstrap does: the `desktop.cli_path`
/// setting, then `env`'s `PATH`, then the known install locations.
pub fn find_cli(platform: &mut impl Platform, env: &Env) -> Result<CliLocation, CliNotFound> {
    let home = platform.home_dir();
    let (setting, config_error) = match platform.cli_setting() {
        Ok(setting) => (setting, None),
        Err(e) => {
            tracing::warn!("ignoring config.yaml for CLI discovery: {e}");
            (None, Some(e))
        }
    };
    discovery::discover(
        setting.as_deref(),
        &env::path_dirs(env),
        home.as_deref(),
        |p| platform.is_executable(p),
    )
    .map_err(|mut not_found| {
        not_found.config_error = config_error;
        not_found
    })
}

/// The real machine.
pub struct SystemPlatform {
    pub daemon_deadline: Duration,
}

impl Default for SystemPlatform {
    fn default() -> Self {
        Self {
            daemon_deadline: daemon::START_DEADLINE,
        }
    }
}

impl Platform for SystemPlatform {
    fn login_env(&mut self) -> Env {
        env::resolve()
    }

    fn cli_setting(&mut self) -> Result<Option<PathBuf>, String> {
        discovery::read_setting()
    }

    fn home_dir(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }

    fn is_executable(&self, path: &Path) -> bool {
        discovery::is_executable_file(path)
    }

    fn cli_version(&mut self, cli: &Path, env: &Env) -> Option<String> {
        discovery::cli_version(cli, env)
    }

    fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure> {
        daemon::start(cli, env, self.daemon_deadline)
    }

    fn connect(&mut self, app_version: &str) -> Result<Connected, (PathBuf, String)> {
        let socket = shelbi_state::control_socket_path()
            .map_err(|e| (PathBuf::from("~/.shelbi/control.sock"), e.to_string()))?;
        let start = Instant::now();
        loop {
            match control::hello(&socket, app_version) {
                Ok(client) => {
                    return Ok(Connected {
                        socket,
                        daemon_version: client.daemon_version,
                    })
                }
                Err(e) if start.elapsed() >= CONNECT_DEADLINE => {
                    return Err((socket, e.to_string()))
                }
                Err(_) => std::thread::sleep(CONNECT_RETRY),
            }
        }
    }
}

/// Runs bootstrap off the UI thread.
pub struct Bootstrap;

impl Bootstrap {
    /// Start bootstrap on its own thread. The receiver yields every state, then
    /// (after `Ready`) at most one [`BootstrapEvent::RelaunchRequired`].
    // The app spawns with a client id (`spawn_as`); tests use this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn spawn(
        platform: impl Platform + Send + 'static,
        app_version: String,
    ) -> async_channel::Receiver<BootstrapEvent> {
        Self::spawn_as(platform, app_version, shelbi_client::fresh_client_id())
    }

    /// [`spawn`](Self::spawn), with the control watch registered as attached
    /// client `client_id` and forwarding detach pushes as
    /// [`BootstrapEvent::Detach`].
    pub fn spawn_as(
        mut platform: impl Platform + Send + 'static,
        app_version: String,
        client_id: String,
    ) -> async_channel::Receiver<BootstrapEvent> {
        let (tx, rx) = async_channel::unbounded();
        let spawned = std::thread::Builder::new()
            .name("shelbi-desktop-bootstrap".into())
            .spawn(move || {
                let end = run(&mut platform, &app_version, |state| {
                    let _ = tx.send_blocking(BootstrapEvent::State(state.clone()));
                });
                if let BootstrapState::Ready(ready) = end {
                    let detach_tx = tx.clone();
                    ready.control.watch_as(
                        client_id,
                        move |notice| {
                            let _ = tx.send_blocking(BootstrapEvent::RelaunchRequired(notice));
                        },
                        move |reason| {
                            let _ = detach_tx.send_blocking(BootstrapEvent::Detach(reason));
                        },
                    );
                }
            });
        if let Err(e) = spawned {
            tracing::error!("could not start bootstrap: {e}");
        }
        rx
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::discovery::CliSource;

    /// A scripted machine: every answer is a field.
    struct Scripted {
        path: String,
        setting: Result<Option<PathBuf>, String>,
        executables: Vec<PathBuf>,
        cli_version: Option<String>,
        start: Result<(), DaemonFailure>,
        connect: Result<Connected, (PathBuf, String)>,
        started_with: Option<(PathBuf, Env)>,
    }

    impl Scripted {
        fn healthy(daemon_version: &str) -> Self {
            Self {
                path: "/usr/bin:/bin".into(),
                setting: Ok(None),
                executables: vec![PathBuf::from("/opt/homebrew/bin/shelbi")],
                cli_version: Some(daemon_version.into()),
                start: Ok(()),
                connect: Ok(Connected {
                    socket: PathBuf::from("/h/control.sock"),
                    daemon_version: daemon_version.into(),
                }),
                started_with: None,
            }
        }
    }

    impl Platform for Scripted {
        fn login_env(&mut self) -> Env {
            [("PATH".to_string(), self.path.clone())].into()
        }
        fn cli_setting(&mut self) -> Result<Option<PathBuf>, String> {
            self.setting.clone()
        }
        fn home_dir(&self) -> Option<PathBuf> {
            Some(PathBuf::from("/Users/me"))
        }
        fn is_executable(&self, path: &Path) -> bool {
            self.executables.iter().any(|p| p == path)
        }
        fn cli_version(&mut self, _cli: &Path, _env: &Env) -> Option<String> {
            self.cli_version.clone()
        }
        fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure> {
            self.started_with = Some((cli.to_path_buf(), env.clone()));
            self.start.clone()
        }
        fn connect(&mut self, _app_version: &str) -> Result<Connected, (PathBuf, String)> {
            self.connect.clone()
        }
    }

    fn label(state: &BootstrapState) -> &'static str {
        match state {
            BootstrapState::ResolvingEnv => "env",
            BootstrapState::FindingCli => "cli",
            BootstrapState::StartingDaemon { .. } => "daemon",
            BootstrapState::Connecting { .. } => "connect",
            BootstrapState::Ready(_) => "ready",
            BootstrapState::Failed(_) => "failed",
        }
    }

    fn run_scripted(platform: &mut Scripted, app: &str) -> (Vec<&'static str>, BootstrapState) {
        let mut seen = Vec::new();
        let end = run(platform, app, |s| seen.push(label(s)));
        (seen, end)
    }

    #[test]
    fn a_healthy_launch_walks_every_state_to_ready() {
        let mut p = Scripted::healthy("0.10.0");
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "daemon", "connect", "ready"]);
        let BootstrapState::Ready(ready) = end else {
            panic!("expected Ready")
        };
        assert_eq!(ready.cli.path, PathBuf::from("/opt/homebrew/bin/shelbi"));
        assert_eq!(ready.cli.source, CliSource::KnownLocation);
        assert_eq!(ready.control.daemon_version(), "0.10.0");
        assert!(ready.control.relaunch_required().is_none());
        // The daemon starts with the found CLI under the login environment.
        let (cli, env) = p.started_with.unwrap();
        assert_eq!(cli, PathBuf::from("/opt/homebrew/bin/shelbi"));
        assert_eq!(env["PATH"], "/usr/bin:/bin");
    }

    #[test]
    fn the_login_path_is_searched() {
        let mut p = Scripted::healthy("0.10.0");
        p.path = "/Users/me/.local/bin:/usr/bin".into();
        p.executables = vec![
            PathBuf::from("/Users/me/.local/bin/shelbi"),
            PathBuf::from("/opt/homebrew/bin/shelbi"),
        ];
        let (_, end) = run_scripted(&mut p, "0.10.0");
        let BootstrapState::Ready(ready) = end else {
            panic!("expected Ready")
        };
        assert_eq!(ready.cli.path, PathBuf::from("/Users/me/.local/bin/shelbi"));
        assert_eq!(ready.cli.source, CliSource::LoginPath);
    }

    #[test]
    fn the_setting_overrides_discovery() {
        let mut p = Scripted::healthy("0.10.0");
        p.setting = Ok(Some(PathBuf::from("~/dev/shelbi")));
        p.executables.push(PathBuf::from("/Users/me/dev/shelbi"));
        let (_, end) = run_scripted(&mut p, "0.10.0");
        let BootstrapState::Ready(ready) = end else {
            panic!("expected Ready")
        };
        assert_eq!(ready.cli.path, PathBuf::from("/Users/me/dev/shelbi"));
        assert_eq!(ready.cli.source, CliSource::Setting);
    }

    #[test]
    fn no_cli_fails_at_finding_cli_and_never_starts_a_daemon() {
        let mut p = Scripted::healthy("0.10.0");
        p.executables.clear();
        p.setting = Err("config.yaml: bad indentation".into());
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "failed"]);
        let BootstrapState::Failed(Failure::CliNotFound(nf)) = end else {
            panic!("expected CliNotFound")
        };
        assert_eq!(nf.searched.len(), 5, "{:?}", nf.searched);
        assert_eq!(
            nf.config_error.as_deref(),
            Some("config.yaml: bad indentation")
        );
        assert!(p.started_with.is_none());
    }

    #[test]
    fn a_daemon_that_fails_to_start_carries_its_stderr() {
        let mut p = Scripted::healthy("0.10.0");
        p.start = Err(DaemonFailure {
            reason: "exited".into(),
            stderr_tail: vec!["boom".into()],
            stderr_log: None,
        });
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "daemon", "failed"]);
        let BootstrapState::Failed(Failure::DaemonFailedToStart { failure, .. }) = end else {
            panic!("expected DaemonFailedToStart")
        };
        assert_eq!(failure.stderr_tail, ["boom"]);
    }

    #[test]
    fn an_unreachable_control_socket_fails_at_connecting() {
        let mut p = Scripted::healthy("0.10.0");
        p.connect = Err((
            PathBuf::from("/h/control.sock"),
            "connection refused".into(),
        ));
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "daemon", "connect", "failed"]);
        assert!(matches!(
            end,
            BootstrapState::Failed(Failure::ControlUnreachable { .. })
        ));
    }

    #[test]
    fn a_version_mismatch_at_hello_fails_with_the_verdict() {
        let mut p = Scripted::healthy("0.10.0");
        let (seen, end) = run_scripted(&mut p, "0.11.0");
        assert_eq!(seen, ["env", "cli", "daemon", "connect", "failed"]);
        let BootstrapState::Failed(Failure::VersionMismatch(check)) = end else {
            panic!("expected VersionMismatch")
        };
        assert_eq!(
            check,
            VersionCheck::AppNewer {
                app: "0.11.0".into(),
                cli: "0.10.0".into()
            }
        );
    }
}
