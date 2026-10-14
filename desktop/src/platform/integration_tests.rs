//! Bootstrap end to end against real processes and sockets: a fake `shelbi`
//! script on a fake `PATH` (it records being asked to start the daemon), and
//! a test daemon serving the hub socket's hello and the control socket's
//! protocol from `shelbi_proto::control`. (The real control server lives in
//! the `shelbi` binary crate, which the app can't link.)
//!
//! Only the login-shell capture is replaced (it would run the developer's own
//! shell); discovery, the daemon start, the hello, and the version check are
//! the real code.

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use shelbi_proto::control::{self, ClientMsg, ServerMsg, CONTROL_PROTOCOL_VERSION};
use shelbi_proto::ProtoError;

use super::bootstrap::{
    Bootstrap, BootstrapEvent, BootstrapState, Failure, Platform, SystemPlatform,
};
use super::control::ControlError;
use super::daemon::DaemonFailure;
use super::discovery::CliSource;
use super::env::{self, Env};
use super::version::APP_VERSION;

/// The tests set process-wide env vars (`SHELBI_HOME`), so they run one at a
/// time.
static ENV_LOCK: Mutex<()> = Mutex::new(());

const LOCATION_VARS: [&str; 3] = ["SHELBI_HOME", "SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK"];

/// A throwaway `SHELBI_HOME` (short, under `/tmp`: macOS caps socket paths at
/// ~104 bytes) with a `bin/` for the fake CLI. Restores the env on drop and
/// releases any fake daemon still waiting on its stop file.
struct Sandbox {
    home: PathBuf,
    bin: PathBuf,
    prev: Vec<(&'static str, Option<std::ffi::OsString>)>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = PathBuf::from(format!("/tmp/shb-dt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let prev = LOCATION_VARS
            .iter()
            .map(|var| (*var, std::env::var_os(var)))
            .collect();
        // A worker pane exports SHELBI_HUB_SOCK; the sandbox must not inherit it.
        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_CONTROL_SOCK");
        std::env::set_var("SHELBI_HOME", &home);
        Self {
            home,
            bin,
            prev,
            _lock: lock,
        }
    }

    /// Write the fake `shelbi`: `--version` prints `version`, `daemon` runs
    /// `daemon_body`.
    fn write_cli(&self, version: &str, daemon_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let cli = self.bin.join("shelbi");
        let script = format!(
            "#!/bin/sh\n\
             case \"$1\" in\n\
               --version) echo \"shelbi {version}\" ;;\n\
               daemon) {daemon_body} ;;\n\
               *) exit 64 ;;\n\
             esac\n"
        );
        std::fs::write(&cli, script).unwrap();
        std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o755)).unwrap();
        cli
    }

    /// A `daemon` body that records its start, then stays up (as a real
    /// daemon does) until the sandbox is dropped.
    fn staying_daemon(&self) -> String {
        format!(
            "touch {started}; while [ ! -f {stop} ]; do sleep 0.05; done",
            started = self.started_marker().display(),
            stop = self.home.join("stop").display(),
        )
    }

    fn started_marker(&self) -> PathBuf {
        self.home.join("daemon-started")
    }

    /// The platform: the real one, except the login environment is a fixed
    /// `PATH` with the fake CLI first, and `$HOME` is the sandbox.
    fn platform(&self) -> TestPlatform {
        TestPlatform {
            real: SystemPlatform {
                daemon_deadline: Duration::from_secs(15),
            },
            path: format!("{}:/usr/bin:/bin", self.bin.display()),
            home: self.home.clone(),
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::write(self.home.join("stop"), "");
        for (var, value) in &self.prev {
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
        // Let the fake daemon notice the stop file before its dir goes.
        std::thread::sleep(Duration::from_millis(150));
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

struct TestPlatform {
    real: SystemPlatform,
    path: String,
    home: PathBuf,
}

impl Platform for TestPlatform {
    fn login_env(&mut self) -> Env {
        let login: Env = [
            ("PATH".to_string(), self.path.clone()),
            ("HOME".to_string(), self.home.display().to_string()),
        ]
        .into();
        env::child_env(login, |var| std::env::var(var).ok())
    }
    fn cli_setting(&mut self) -> Result<Option<PathBuf>, String> {
        self.real.cli_setting()
    }
    fn home_dir(&self) -> Option<PathBuf> {
        Some(self.home.clone())
    }
    fn is_executable(&self, path: &Path) -> bool {
        self.real.is_executable(path)
    }
    fn cli_version(&mut self, cli: &Path, env: &Env) -> Option<String> {
        self.real.cli_version(cli, env)
    }
    fn start_daemon(&mut self, cli: &Path, env: &Env) -> Result<(), DaemonFailure> {
        self.real.start_daemon(cli, env)
    }
    fn connect(
        &mut self,
        app_version: &str,
    ) -> Result<super::bootstrap::Connected, (PathBuf, String)> {
        self.real.connect(app_version)
    }
}

/// A stand-in daemon: answers the hub socket's hello probe and speaks the
/// control protocol (hello, subscribe), with a version the test can change
/// and `Reexec` pushes it can send. Stops serving when dropped.
struct TestDaemon {
    shared: Arc<Shared>,
}

struct Shared {
    version: Mutex<String>,
    subscribers: Mutex<Vec<UnixStream>>,
    stop: AtomicBool,
}

impl TestDaemon {
    /// Serve `<home>/hub.sock` and `<home>/control.sock`.
    fn serve(home: &Path, version: &str) -> Self {
        let shared = Arc::new(Shared {
            version: Mutex::new(version.to_string()),
            subscribers: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let hub = UnixListener::bind(home.join("hub.sock")).unwrap();
        let ctl = UnixListener::bind(home.join("control.sock")).unwrap();
        let s = shared.clone();
        std::thread::spawn(move || s.accept_loop(hub, Shared::hub_conn));
        let s = shared.clone();
        std::thread::spawn(move || s.accept_loop(ctl, Shared::control_conn));
        TestDaemon { shared }
    }

    fn push_reexec(&self, reason: &str) {
        let frame = control::encode(&ServerMsg::Reexec {
            reason: reason.to_string(),
        })
        .unwrap();
        for sub in self.shared.subscribers.lock().unwrap().iter_mut() {
            let _ = sub.write_all(&frame);
        }
    }

    /// "Restart onto a new binary": drop every subscriber, answer later hellos
    /// with `version`.
    fn restart_as(&self, version: &str) {
        *self.shared.version.lock().unwrap() = version.to_string();
        for sub in self.shared.subscribers.lock().unwrap().drain(..) {
            let _ = sub.shutdown(std::net::Shutdown::Both);
        }
    }

    fn has_subscriber(&self) -> bool {
        !self.shared.subscribers.lock().unwrap().is_empty()
    }
}

impl Drop for TestDaemon {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}

impl Shared {
    fn accept_loop(self: Arc<Self>, listener: UnixListener, handle: fn(&Shared, UnixStream)) {
        listener.set_nonblocking(true).unwrap();
        while !self.stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream.set_nonblocking(false).unwrap();
                    let s = self.clone();
                    std::thread::spawn(move || handle(&s, stream));
                }
                Err(_) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }

    /// The hub probe half-closes and reads one hello line.
    fn hub_conn(&self, mut stream: UnixStream) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut sink = Vec::new();
        let _ = stream.read_to_end(&mut sink);
        let line = shelbi_state::DaemonHello::new(&self.version.lock().unwrap()).to_line();
        let _ = stream.write_all(line.as_bytes());
    }

    fn control_conn(&self, mut stream: UnixStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match control::decode::<ClientMsg>(&buf) {
                Ok((msg, used)) => {
                    buf.drain(..used);
                    self.answer(msg, &mut stream);
                    continue;
                }
                Err(ProtoError::Incomplete { .. }) => {}
                Err(_) => return,
            }
            match stream.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }

    fn answer(&self, msg: ClientMsg, stream: &mut UnixStream) {
        match msg {
            ClientMsg::Hello { .. } => {
                let hello = ServerMsg::Hello {
                    protocol: CONTROL_PROTOCOL_VERSION,
                    daemon_version: self.version.lock().unwrap().clone(),
                };
                let _ = stream.write_all(&control::encode(&hello).unwrap());
            }
            ClientMsg::Subscribe { .. } => {
                self.subscribers
                    .lock()
                    .unwrap()
                    .push(stream.try_clone().unwrap());
            }
            _ => {}
        }
    }
}

fn wait_for(what: &str, timeout: Duration, mut cond: impl FnMut() -> bool) {
    let start = Instant::now();
    while !cond() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Collect events until `done` accepts one, or panic after `timeout`.
fn events_until(
    rx: &async_channel::Receiver<BootstrapEvent>,
    timeout: Duration,
    mut done: impl FnMut(&BootstrapEvent) -> bool,
) -> Vec<BootstrapEvent> {
    let start = Instant::now();
    let mut seen = Vec::new();
    loop {
        match rx.try_recv() {
            Ok(event) => {
                let finished = done(&event);
                seen.push(event);
                if finished {
                    return seen;
                }
            }
            Err(_) => {
                assert!(start.elapsed() < timeout, "timed out; saw {seen:?}");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn is_terminal(event: &BootstrapEvent) -> bool {
    matches!(
        event,
        BootstrapEvent::State(BootstrapState::Ready(_) | BootstrapState::Failed(_))
    )
}

/// Bring a sandbox to `Ready`, starting the fake daemon through the fake CLI.
fn boot_to_ready(
    sandbox: &Sandbox,
) -> (
    TestDaemon,
    super::bootstrap::Ready,
    async_channel::Receiver<BootstrapEvent>,
) {
    sandbox.write_cli(APP_VERSION, &sandbox.staying_daemon());
    let rx = Bootstrap::spawn(sandbox.platform(), APP_VERSION.to_string());

    // The CLI was asked to start the daemon; bind its sockets on its behalf.
    wait_for(
        "the CLI to be asked to start the daemon",
        Duration::from_secs(20),
        || sandbox.started_marker().exists(),
    );
    let daemon = TestDaemon::serve(&sandbox.home, APP_VERSION);

    let events = events_until(&rx, Duration::from_secs(30), is_terminal);
    let labels: Vec<&str> = events
        .iter()
        .map(|e| match e {
            BootstrapEvent::State(BootstrapState::ResolvingEnv) => "env",
            BootstrapEvent::State(BootstrapState::FindingCli) => "cli",
            BootstrapEvent::State(BootstrapState::StartingDaemon { .. }) => "daemon",
            BootstrapEvent::State(BootstrapState::Connecting { .. }) => "connect",
            BootstrapEvent::State(BootstrapState::Ready(_)) => "ready",
            BootstrapEvent::State(BootstrapState::Failed(_)) => "failed",
            BootstrapEvent::RelaunchRequired(_) => "relaunch",
            BootstrapEvent::Detach(_) => "detach",
        })
        .collect();
    assert_eq!(
        labels,
        ["env", "cli", "daemon", "connect", "ready"],
        "{events:?}"
    );
    let Some(BootstrapEvent::State(BootstrapState::Ready(ready))) = events.into_iter().last()
    else {
        unreachable!()
    };
    (daemon, ready, rx)
}

#[test]
fn bootstrap_starts_a_stopped_daemon_through_the_cli_and_reaches_ready() {
    let sandbox = Sandbox::new("ready");
    let (_daemon, ready, _rx) = boot_to_ready(&sandbox);

    assert_eq!(ready.cli.path, sandbox.bin.join("shelbi"));
    assert_eq!(ready.cli.source, CliSource::LoginPath);
    assert_eq!(ready.cli_version.as_deref(), Some(APP_VERSION));
    assert_eq!(ready.control.daemon_version(), APP_VERSION);
    assert_eq!(ready.control.socket(), sandbox.home.join("control.sock"));
    // The handle opens command connections.
    assert!(ready.control.connect().is_ok());
}

#[test]
fn the_cli_path_setting_overrides_discovery() {
    let sandbox = Sandbox::new("setting");
    // The fake CLI lives outside PATH; only the setting can find it.
    let elsewhere = sandbox.home.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let cli = sandbox.write_cli(APP_VERSION, "exit 1");
    let moved = elsewhere.join("shelbi");
    std::fs::rename(&cli, &moved).unwrap();
    std::fs::write(
        sandbox.home.join("config.yaml"),
        format!("desktop:\n  cli_path: {}\n", moved.display()),
    )
    .unwrap();

    let rx = Bootstrap::spawn(sandbox.platform(), APP_VERSION.to_string());
    let events = events_until(&rx, Duration::from_secs(30), is_terminal);
    // It got as far as starting the daemon with the overridden CLI.
    let started = events.iter().find_map(|e| match e {
        BootstrapEvent::State(BootstrapState::StartingDaemon { cli }) => Some(cli.clone()),
        _ => None,
    });
    let started = started.expect("reached StartingDaemon");
    assert_eq!(started.path, moved);
    assert_eq!(started.source, CliSource::Setting);
}

#[test]
fn a_daemon_that_dies_on_start_fails_with_its_stderr_tail() {
    let sandbox = Sandbox::new("dies");
    sandbox.write_cli(
        APP_VERSION,
        "echo 'error: config.yaml line 3: bad key' >&2; exit 2",
    );
    let rx = Bootstrap::spawn(sandbox.platform(), APP_VERSION.to_string());
    let events = events_until(&rx, Duration::from_secs(30), is_terminal);
    let Some(BootstrapEvent::State(BootstrapState::Failed(Failure::DaemonFailedToStart {
        cli,
        failure,
    }))) = events.last()
    else {
        panic!("expected DaemonFailedToStart, got {events:?}")
    };
    assert_eq!(cli, &sandbox.bin.join("shelbi"));
    assert!(failure.reason.contains("exited"), "{}", failure.reason);
    assert_eq!(failure.stderr_tail, ["error: config.yaml line 3: bad key"]);
}

#[test]
fn a_reexec_push_requires_a_relaunch_and_blocks_commands() {
    let sandbox = Sandbox::new("reexec");
    let (daemon, ready, rx) = boot_to_ready(&sandbox);
    wait_for("the watch to subscribe", Duration::from_secs(10), || {
        daemon.has_subscriber()
    });

    daemon.push_reexec("client 0.10.0 is out of date (daemon 0.10.1)");
    let events = events_until(&rx, Duration::from_secs(10), |e| {
        matches!(e, BootstrapEvent::RelaunchRequired(_))
    });
    let Some(BootstrapEvent::RelaunchRequired(notice)) = events.last() else {
        unreachable!()
    };
    assert_eq!(
        notice.reason,
        "client 0.10.0 is out of date (daemon 0.10.1)"
    );
    assert!(matches!(
        ready.control.connect(),
        Err(ControlError::RelaunchRequired(_))
    ));
}

#[test]
fn a_daemon_restarted_on_a_new_version_requires_a_relaunch() {
    let sandbox = Sandbox::new("upgrade");
    let (daemon, ready, rx) = boot_to_ready(&sandbox);
    wait_for("the watch to subscribe", Duration::from_secs(10), || {
        daemon.has_subscriber()
    });

    daemon.restart_as("99.0.0");
    let events = events_until(&rx, Duration::from_secs(15), |e| {
        matches!(e, BootstrapEvent::RelaunchRequired(_))
    });
    let Some(BootstrapEvent::RelaunchRequired(notice)) = events.last() else {
        unreachable!()
    };
    assert_eq!(notice.daemon_version.as_deref(), Some("99.0.0"));
    assert_eq!(notice.app_version, APP_VERSION);
    assert!(matches!(
        ready.control.connect(),
        Err(ControlError::RelaunchRequired(_))
    ));
}
