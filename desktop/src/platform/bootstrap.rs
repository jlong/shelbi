//! The bootstrap state machine: from a bare launch to a version-checked
//! control connection.
//!
//! `ResolvingEnv → FindingCli → StartingDaemon → Connecting → Ready`, or
//! `Failed(kind)` at the step that broke. When discovery finds no CLI and
//! nothing says where one should be (no `desktop.cli_path`, a readable
//! `config.yaml`), `FindingCli` is followed by `InstallingCli` (the release
//! matching the app, [`install`]) and discovery runs again; a configured
//! path that isn't there is reported, never installed over. [`run`] is the machine, written
//! against the [`Platform`] seam so every path is tested with a scripted
//! platform; [`Bootstrap::spawn`] runs it on its own thread with the real
//! [`SystemPlatform`] and publishes each state (and, after `Ready`, a
//! relaunch notice) over a channel the root view reads.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shelbi_client::ClientError;

use super::control::{self, ControlHandle, RelaunchNotice};
use super::daemon::{self, DaemonFailure};
use super::discovery::{self, CliLocation, CliNotFound, CliSource};
use super::env::{self, Env};
use super::install::{self, InstallError, InstallErrorKind};
use super::version::{self, ProtocolCheck, VersionCheck};

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
    /// No CLI was found: installing the app's release of it.
    InstallingCli { step: install::Step },
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
            BootstrapState::InstallingCli { step } => Some(match step {
                install::Step::Downloading => "Downloading the shelbi CLI",
                install::Step::Verifying => "Verifying the shelbi CLI download",
                install::Step::Installing => "Installing the shelbi CLI",
            }),
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
    /// The `shelbi` discovery finds without a `desktop.cli_path`: `cli`
    /// itself unless the setting chose it, else what the login `PATH` and
    /// the known locations give (`None` when they give nothing). Settings
    /// shows it while no override is set.
    pub automatic: Option<PathBuf>,
    /// `shelbi --version`, when it answered.
    pub cli_version: Option<String>,
    /// The login environment `shelbi`, `git`, `gh`, and `ssh` run under,
    /// with the CLI's directory on its `PATH`.
    pub env: Arc<Env>,
    /// Whether a terminal finds this CLI by name: it came from the login
    /// `PATH` or the setting. `false` for one found (or installed) in a
    /// known location the user's shell doesn't list, which Settings explains.
    pub on_login_path: bool,
    pub control: ControlHandle,
}

/// The step that failed, with what its screen needs to name the fix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    CliNotFound(CliNotFound),
    /// No CLI was found and installing one failed. Nothing working was
    /// replaced; Try Again runs bootstrap (and the install) again.
    InstallFailed(InstallError),
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
    /// The control socket answered with another control protocol. `cli` is
    /// the CLI bootstrap found (the one a restart would run).
    ProtocolMismatch { cli: PathBuf, check: ProtocolCheck },
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

/// Why [`Platform::connect`] has no connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnectError {
    /// Nothing answered a hello before the deadline.
    Unreachable { socket: PathBuf, error: String },
    /// The daemon answered, with a control protocol other than the app's.
    ProtocolMismatch { daemon: u32, app: u32 },
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
    /// `shelbi __control-protocol`, when it answered (asked only after a
    /// protocol mismatch).
    fn cli_control_protocol(&mut self, cli: &Path, env: &Env) -> Option<u32>;
    /// Install the CLI of `version` (blocking), reporting each step; the
    /// installed executable's path.
    fn install_cli(
        &mut self,
        version: &str,
        env: &Env,
        progress: &mut dyn FnMut(install::Step),
    ) -> Result<PathBuf, InstallError>;
    /// Ensure a daemon is running, starting it with `<cli> daemon`.
    fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure>;
    /// Connect to the control socket and complete the hello.
    fn connect(&mut self, app_version: &str) -> Result<Connected, ConnectError>;
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
        Err(not_found) if !may_install(&not_found) => {
            return enter(BootstrapState::Failed(Failure::CliNotFound(not_found)))
        }
        Err(_) => {
            tracing::info!(version = app_version, "no shelbi CLI found; installing it");
            enter(BootstrapState::InstallingCli {
                step: install::Step::Downloading,
            });
            let installed = platform.install_cli(app_version, &env, &mut |step| {
                enter(BootstrapState::InstallingCli { step });
            });
            if let Err(e) = installed {
                tracing::warn!(?e, "the CLI install failed");
                return enter(BootstrapState::Failed(Failure::InstallFailed(e)));
            }
            // Readiness is what discovery and the daemon say, not the
            // download: find it again, then check it as any other CLI.
            enter(BootstrapState::FindingCli);
            match find_cli(platform, &env) {
                Ok(cli) => cli,
                Err(not_found) => {
                    return enter(BootstrapState::Failed(Failure::InstallFailed(
                        InstallError::new(
                            InstallErrorKind::PostInstallCheck,
                            "The CLI was installed, but the app still can't find it.",
                        )
                        .with_detail(not_found.searched.iter().map(|p| p.display().to_string())),
                    )))
                }
            }
        }
    };
    let automatic = match cli.source {
        CliSource::Setting => {
            let home = platform.home_dir();
            discovery::discover(None, &env::path_dirs(&env), home.as_deref(), |p| platform.is_executable(p))
                .ok()
                .map(|found| found.path)
        }
        CliSource::LoginPath | CliSource::KnownLocation => Some(cli.path.clone()),
    };
    // The app's children (the daemon, `shelbi` commands) find the CLI by
    // name even when the login shell doesn't list its directory.
    let env = match cli.path.parent() {
        Some(dir) => env::with_dir_on_path(env, dir),
        None => env,
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
        Err(ConnectError::Unreachable { socket, error }) => {
            return enter(BootstrapState::Failed(Failure::ControlUnreachable {
                socket,
                error,
            }))
        }
        // The socket answered. Which side is behind decides the fix, and only
        // the CLI on disk can say whether a restart would help.
        Err(ConnectError::ProtocolMismatch { daemon, app }) => {
            let cli_protocol = platform.cli_control_protocol(&cli.path, &env);
            tracing::warn!(
                daemon,
                app,
                cli = ?cli_protocol,
                "the daemon speaks another control protocol"
            );
            return enter(BootstrapState::Failed(Failure::ProtocolMismatch {
                cli: cli.path,
                check: version::check_protocol(app, cli_protocol, daemon),
            }));
        }
    };
    match version::check(
        app_version,
        cli_version.as_deref(),
        &connected.daemon_version,
    ) {
        VersionCheck::Match => enter(BootstrapState::Ready(Ready {
            on_login_path: cli.source != CliSource::KnownLocation,
            cli,
            automatic,
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

/// Whether a failed discovery may install the CLI: nothing configures one
/// (a `desktop.cli_path` that isn't there, or a `config.yaml` that can't be
/// read and so might set one, keeps its diagnosis).
pub fn may_install(not_found: &CliNotFound) -> bool {
    not_found.setting.is_none() && not_found.config_error.is_none()
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

    fn cli_control_protocol(&mut self, cli: &Path, env: &Env) -> Option<u32> {
        discovery::cli_control_protocol(cli, env)
    }

    fn install_cli(
        &mut self,
        version: &str,
        env: &Env,
        progress: &mut dyn FnMut(install::Step),
    ) -> Result<PathBuf, InstallError> {
        let plan = install::Plan::for_app(version, self.home_dir().as_deref())?;
        install::install(&plan, &install::SystemIo { env: env.clone() }, progress)
    }

    fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure> {
        daemon::start(cli, env, self.daemon_deadline)
    }

    fn connect(&mut self, app_version: &str) -> Result<Connected, ConnectError> {
        let socket =
            shelbi_state::control_socket_path().map_err(|e| ConnectError::Unreachable {
                socket: PathBuf::from("~/.shelbi/control.sock"),
                error: e.to_string(),
            })?;
        let start = Instant::now();
        loop {
            match control::hello(&socket, app_version) {
                Ok(client) => {
                    return Ok(Connected {
                        socket,
                        daemon_version: client.daemon_version,
                    })
                }
                // An answer, not an absence: retrying gets the same one.
                Err(ClientError::ControlProtocolMismatch { daemon, client }) => {
                    return Err(ConnectError::ProtocolMismatch {
                        daemon,
                        app: client,
                    })
                }
                Err(e) if start.elapsed() >= CONNECT_DEADLINE => {
                    return Err(ConnectError::Unreachable {
                        socket,
                        error: e.to_string(),
                    })
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

    /// A scripted machine: every answer is a field.
    struct Scripted {
        path: String,
        setting: Result<Option<PathBuf>, String>,
        executables: Vec<PathBuf>,
        cli_version: Option<String>,
        cli_protocol: Option<u32>,
        start: Result<(), DaemonFailure>,
        connect: Result<Connected, ConnectError>,
        started_with: Option<(PathBuf, Env)>,
        /// What each install does, in order (the last one repeats); an `Ok`
        /// path becomes executable.
        installs: Vec<Result<PathBuf, InstallError>>,
        installed: usize,
    }

    impl Scripted {
        fn healthy(daemon_version: &str) -> Self {
            Self {
                path: "/usr/bin:/bin".into(),
                setting: Ok(None),
                executables: vec![PathBuf::from("/opt/homebrew/bin/shelbi")],
                cli_version: Some(daemon_version.into()),
                cli_protocol: Some(6),
                start: Ok(()),
                connect: Ok(Connected {
                    socket: PathBuf::from("/h/control.sock"),
                    daemon_version: daemon_version.into(),
                }),
                started_with: None,
                installs: vec![Ok(PathBuf::from("/Users/me/bin/shelbi"))],
                installed: 0,
            }
        }

        /// A Mac with no CLI anywhere.
        fn fresh(daemon_version: &str) -> Self {
            let mut p = Self::healthy(daemon_version);
            p.executables.clear();
            p
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
        fn cli_control_protocol(&mut self, _cli: &Path, _env: &Env) -> Option<u32> {
            self.cli_protocol
        }
        fn install_cli(
            &mut self,
            _version: &str,
            _env: &Env,
            progress: &mut dyn FnMut(install::Step),
        ) -> Result<PathBuf, InstallError> {
            let ix = self.installed.min(self.installs.len() - 1);
            self.installed += 1;
            progress(install::Step::Verifying);
            progress(install::Step::Installing);
            let result = self.installs[ix].clone();
            if let Ok(path) = &result {
                self.executables.push(path.clone());
            }
            result
        }
        fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure> {
            self.started_with = Some((cli.to_path_buf(), env.clone()));
            self.start.clone()
        }
        fn connect(&mut self, _app_version: &str) -> Result<Connected, ConnectError> {
            self.connect.clone()
        }
    }

    fn label(state: &BootstrapState) -> &'static str {
        match state {
            BootstrapState::ResolvingEnv => "env",
            BootstrapState::FindingCli => "cli",
            BootstrapState::InstallingCli { .. } => "install",
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
        // The CLI's directory joins the login PATH for the app's children.
        assert_eq!(env["PATH"], "/usr/bin:/bin:/opt/homebrew/bin");
        assert_eq!(p.installed, 0, "a found CLI is never installed over");
        assert!(!ready.on_login_path, "Homebrew isn't on this login PATH");
        assert_eq!(ready.automatic.as_ref(), Some(&ready.cli.path), "discovery found it");
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
        assert!(ready.on_login_path);
        assert_eq!(ready.env["PATH"], "/Users/me/.local/bin:/usr/bin", "already there");
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
        // What Settings shows once the override is cleared: the search
        // without it, never the override itself.
        assert_eq!(ready.automatic, Some(PathBuf::from("/opt/homebrew/bin/shelbi")));
    }

    #[test]
    fn an_override_with_nothing_else_installed_has_no_automatic_cli() {
        let mut p = Scripted::healthy("0.10.0");
        p.setting = Ok(Some(PathBuf::from("/Users/me/dev/shelbi")));
        p.executables = vec![PathBuf::from("/Users/me/dev/shelbi")];
        let (_, end) = run_scripted(&mut p, "0.10.0");
        let BootstrapState::Ready(ready) = end else {
            panic!("expected Ready")
        };
        assert_eq!(ready.automatic, None);
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
        assert_eq!(nf.searched.len(), 6, "{:?}", nf.searched);
        assert_eq!(
            nf.config_error.as_deref(),
            Some("config.yaml: bad indentation")
        );
        assert!(p.started_with.is_none());
        // An unreadable config.yaml might name a CLI: never install over it.
        assert_eq!(p.installed, 0);
    }

    #[test]
    fn a_missing_cli_is_installed_then_found_and_checked_like_any_other() {
        let mut p = Scripted::fresh("0.10.0");
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(
            seen,
            ["env", "cli", "install", "install", "install", "cli", "daemon", "connect", "ready"]
        );
        let BootstrapState::Ready(ready) = end else {
            panic!("expected Ready, got {end:?}")
        };
        assert_eq!(p.installed, 1);
        assert_eq!(ready.cli.path, PathBuf::from("/Users/me/bin/shelbi"));
        assert_eq!(ready.cli.source, CliSource::KnownLocation);
        assert!(!ready.on_login_path, "the terminal needs ~/bin on its PATH");
        // The daemon starts from the installed CLI, which its children find.
        let (cli, env) = p.started_with.take().unwrap();
        assert_eq!(cli, PathBuf::from("/Users/me/bin/shelbi"));
        assert_eq!(env["PATH"], "/usr/bin:/bin:/Users/me/bin");

        // The next launch finds it: no second install.
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "daemon", "connect", "ready"]);
        assert!(matches!(end, BootstrapState::Ready(_)));
        assert_eq!(p.installed, 1);
    }

    #[test]
    fn a_failed_install_fails_before_the_daemon_and_a_retry_installs() {
        let mut p = Scripted::fresh("0.10.0");
        p.installs = vec![
            Err(InstallError::new(InstallErrorKind::Network, "offline")),
            Ok(PathBuf::from("/Users/me/bin/shelbi")),
        ];
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "install", "install", "install", "failed"]);
        let BootstrapState::Failed(Failure::InstallFailed(e)) = end else {
            panic!("expected InstallFailed, got {end:?}")
        };
        assert_eq!(e.kind, InstallErrorKind::Network);
        assert!(p.started_with.is_none());

        let (_, end) = run_scripted(&mut p, "0.10.0");
        assert!(matches!(end, BootstrapState::Ready(_)), "{end:?}");
        assert_eq!(p.installed, 2);
    }

    #[test]
    fn a_configured_path_that_isnt_there_is_reported_not_installed_over() {
        let mut p = Scripted::fresh("0.10.0");
        p.setting = Ok(Some(PathBuf::from("~/dev/shelbi")));
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "failed"]);
        let BootstrapState::Failed(Failure::CliNotFound(nf)) = end else {
            panic!("expected CliNotFound")
        };
        assert_eq!(nf.setting, Some(PathBuf::from("/Users/me/dev/shelbi")));
        assert_eq!(p.installed, 0);
    }

    #[test]
    fn an_incompatible_cli_keeps_its_diagnosis_and_is_not_replaced() {
        let mut p = Scripted::healthy("0.10.0");
        let (_, end) = run_scripted(&mut p, "0.11.0");
        assert!(
            matches!(end, BootstrapState::Failed(Failure::VersionMismatch(_))),
            "{end:?}"
        );
        assert_eq!(p.installed, 0);
    }

    #[test]
    fn an_install_discovery_still_cant_find_fails_its_post_install_check() {
        let mut p = Scripted::fresh("0.10.0");
        p.installs = vec![Ok(PathBuf::from("/somewhere/else/shelbi"))];
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen.last(), Some(&"failed"));
        let BootstrapState::Failed(Failure::InstallFailed(e)) = end else {
            panic!("expected InstallFailed, got {end:?}")
        };
        assert_eq!(e.kind, InstallErrorKind::PostInstallCheck);
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
        p.connect = Err(ConnectError::Unreachable {
            socket: PathBuf::from("/h/control.sock"),
            error: "connection refused".into(),
        });
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

    /// A daemon left on an older protocol, under a CLI that speaks the same
    /// old one: the version strings match, so only the protocols tell.
    #[test]
    fn an_older_protocol_under_an_older_cli_fails_as_cli_older() {
        let mut p = Scripted::healthy("0.10.0");
        p.connect = Err(ConnectError::ProtocolMismatch { daemon: 4, app: 6 });
        p.cli_protocol = Some(4);
        let (seen, end) = run_scripted(&mut p, "0.10.0");
        assert_eq!(seen, ["env", "cli", "daemon", "connect", "failed"]);
        let BootstrapState::Failed(Failure::ProtocolMismatch { cli, check }) = end else {
            panic!("expected ProtocolMismatch, got {end:?}")
        };
        assert_eq!(cli, PathBuf::from("/opt/homebrew/bin/shelbi"));
        assert_eq!(
            check,
            ProtocolCheck::CliOlder {
                app: 6,
                cli: Some(4),
                daemon: 4
            }
        );
    }

    #[test]
    fn an_older_protocol_under_a_current_cli_fails_as_a_stale_daemon() {
        let mut p = Scripted::healthy("0.10.0");
        p.connect = Err(ConnectError::ProtocolMismatch { daemon: 4, app: 6 });
        p.cli_protocol = Some(6);
        let (_, end) = run_scripted(&mut p, "0.10.0");
        let BootstrapState::Failed(Failure::ProtocolMismatch { check, .. }) = end else {
            panic!("expected ProtocolMismatch, got {end:?}")
        };
        assert_eq!(
            check,
            ProtocolCheck::DaemonStale {
                app: 6,
                cli: 6,
                daemon: 4
            }
        );
    }
}
