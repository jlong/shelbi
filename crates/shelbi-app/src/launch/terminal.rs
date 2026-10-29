//! Running a remote session's terminal ([`super::ssh::terminal_command`]) and
//! riding out its connection dropping.
//!
//! The far side keeps the project running when `ssh` drops, so a dropped
//! terminal is reconnected rather than closed: the user sees the same
//! "Reconnecting to <host>… (attempt N)" notice a remote workspace pane shows
//! ([`crate::session::reconnecting_notice`]), on the same
//! [`ReconnectController`] backoff. Each attempt first checks the host with
//! [`RemoteConnector::connect`], so a reconnect never starts the far daemon:
//! a host whose Shelbi stopped ends the loop with
//! [`RemoteError::DaemonStopped`] for the client to offer a start.
//!
//! Two ways to drive it:
//!
//! - [`run`] is the blocking loop a client that hands its whole terminal over
//!   (the TUI) drives; [`TerminalRunner`] and [`ReconnectWait`] are its
//!   seams, so the loop is tested without `ssh` or a terminal.
//! - [`TerminalConnector`] is for a client that draws terminals itself (the
//!   desktop app): a [`crate::session::Connector`] whose session is a local
//!   Shelbi session ([`local_session_name`]) running the terminal command,
//!   attached like any other pane. The pane's view reconnects on its own
//!   [`ReconnectController`] when the command exits with
//!   [`TerminalExit::ConnectionLost`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use shelbi_client::{ClientError, Connection, SpawnSpec};
use shelbi_proto::capability;

use super::remote::{RemoteConnector, RemoteError, RemoteSession};
use super::ssh::{exit_meaning, terminal_command, TerminalExit};
use crate::session::{
    reconnecting_notice, ConnectFailure, Connected, Connector, GridSize, ReconnectController,
    ReconnectPolicy, SessionRef,
};

/// The reason shown with the first reconnect notice.
pub const DROPPED_REASON: &str = "The connection dropped.";

/// The line a client shows while the session is open: which host and project
/// it is connected to (`Connected to jlong@devbox · Shelbi`).
pub fn connected_label(session: &RemoteSession) -> String {
    let host = session
        .host
        .rsplit_once('@')
        .map_or(session.host.as_str(), |(_, h)| h);
    format!(
        "Connected to {}@{host} · {}",
        session.user,
        session.display_label()
    )
}

/// A terminal window's title for the session (`Shelbi · jlong@devbox ·
/// Shelbi`).
pub fn window_title(session: &RemoteSession) -> String {
    let host = session
        .host
        .rsplit_once('@')
        .map_or(session.host.as_str(), |(_, h)| h);
    format!("Shelbi · {}@{host} · {}", session.user, session.display_label())
}

/// Runs the terminal command in the foreground until it exits.
pub trait TerminalRunner {
    /// Run `argv` with the client's terminal and return its exit code
    /// (`None` on a signal).
    fn run(&mut self, argv: &[String]) -> std::io::Result<Option<i32>>;
}

/// Shows the reconnect notice and waits.
pub trait ReconnectWait {
    /// Show `notice` and wait `pause` (possibly zero). `false` when the user
    /// stopped reconnecting.
    fn wait(&mut self, notice: &str, pause: Duration) -> bool;
}

/// How a remote terminal ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalEnd {
    /// The user quit the far Shelbi.
    Closed,
    /// The user stopped reconnecting.
    Stopped,
    /// The far side ran and failed with this exit code.
    Failed(i32),
    /// A reconnect found the host refusing (its daemon stopped, the project
    /// gone, a version change) or the terminal couldn't run.
    Refused(RemoteError),
}

impl TerminalEnd {
    /// The client's status line after the terminal ends.
    pub fn status(&self, session: &RemoteSession) -> String {
        let host = &session.host;
        match self {
            TerminalEnd::Closed => format!("Disconnected from {host}"),
            TerminalEnd::Stopped => format!("Stopped reconnecting to {host}"),
            TerminalEnd::Failed(code) => {
                format!("Shelbi on {host} exited with status {code}")
            }
            TerminalEnd::Refused(error) => error.to_string(),
        }
    }
}

/// Run `session`'s terminal until the user quits it, reconnecting each time
/// the connection drops. Blocking.
pub fn run(
    session: &RemoteSession,
    connector: &dyn RemoteConnector,
    runner: &mut dyn TerminalRunner,
    waiter: &mut dyn ReconnectWait,
    policy: ReconnectPolicy,
) -> TerminalEnd {
    let argv = terminal_command(session);
    let mut reconnect = ReconnectController::new(policy);
    loop {
        let code = match runner.run(&argv) {
            Ok(code) => code,
            Err(e) => {
                return TerminalEnd::Refused(RemoteError::Failed(format!("Couldn't run ssh: {e}")))
            }
        };
        match exit_meaning(code) {
            TerminalExit::Closed => return TerminalEnd::Closed,
            TerminalExit::Failed => return TerminalEnd::Failed(code.unwrap_or(-1)),
            TerminalExit::ConnectionLost => reconnect.link_lost(DROPPED_REASON, Instant::now()),
        }
        while reconnect.is_reconnecting() {
            let now = Instant::now();
            let pause = reconnect.wait(now).unwrap_or_default();
            let notice = reconnecting_notice(&session.host, reconnect.attempt(), reconnect.reason());
            if !waiter.wait(&notice, pause) {
                return TerminalEnd::Stopped;
            }
            let now = Instant::now();
            if reconnect.take_due(now).is_none() {
                // Woken early: wait out the rest.
                continue;
            }
            match connector.connect(session) {
                Ok(()) => reconnect.connected(),
                Err(RemoteError::Unreachable { reason, .. }) => {
                    reconnect.unreachable(&reason, Instant::now())
                }
                Err(error) => return TerminalEnd::Refused(error),
            }
        }
    }
}

/// The local session that carries `session`'s terminal
/// (`remote/<host>/<project>`).
pub fn local_session_name(session: &RemoteSession) -> String {
    format!("remote/{}/{}", session.host, session.project)
}

/// End the local session carrying `session`'s terminal, when the client
/// closes it: `ssh` exits, and the far Shelbi keeps running. Blocking.
pub fn end_local_session(session: &RemoteSession) {
    let Ok(root) = shelbi_state::sessions_dir() else {
        return;
    };
    let name = local_session_name(session);
    let Some(live) = shelbi_client::list(&root)
        .ok()
        .and_then(|sessions| shelbi_client::choose_session(&sessions, &name))
    else {
        return;
    };
    if let Ok((conn, _events)) = Connection::open(&live.sock, None, capability::ALL) {
        if let Err(error) = conn.kill(None) {
            tracing::warn!(session = %name, %error, "couldn't end the remote session's terminal");
        }
    }
}

/// How long a just-spawned local session may take to bind its socket.
const BIND_DEADLINE: Duration = Duration::from_secs(5);

/// A remote session's terminal as a local Shelbi session, for a client that
/// draws terminals itself. See the module docs.
///
/// A live local session for the remote session is reused (a window closed
/// and reopened while `ssh` stayed up picks up where it was). Otherwise the
/// host is checked first ([`RemoteConnector::connect`]: an unreachable host is
/// [`ConnectFailure::Unreachable`], so the view keeps reconnecting; anything
/// else, such as a stopped daemon, is a message), then a fresh local session
/// runs [`terminal_command`].
pub struct TerminalConnector {
    pub session: RemoteSession,
    pub remote: Arc<dyn RemoteConnector>,
    /// The `shelbi` that runs the local session (`shelbi __session`).
    pub exe: PathBuf,
    /// The terminal's size at spawn.
    pub size: GridSize,
}

impl TerminalConnector {
    /// The [`SessionRef`] a view of the session shows (its notices name the
    /// project: "Connecting to Shelbi…").
    pub fn target(session: &RemoteSession) -> SessionRef {
        SessionRef::Workspace(session.display_label().to_string())
    }

    fn spawn(&self) -> Result<PathBuf, ConnectFailure> {
        self.remote.connect(&self.session).map_err(|error| match error {
            RemoteError::Unreachable { reason, .. } => ConnectFailure::Unreachable(reason),
            other => ConnectFailure::Message(other.to_string()),
        })?;
        let spec = SpawnSpec {
            name: local_session_name(&self.session),
            cwd: std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into()),
            cols: self.size.cols.max(1),
            rows: self.size.rows.max(1),
            task: None,
            raw_output_log: false,
            child_argv: terminal_command(&self.session),
        };
        shelbi_client::spawn_with_exe(&self.exe, &spec)
            .map(|spawned| spawned.sock)
            .map_err(|e| ConnectFailure::Message(e.to_string()))
    }
}

impl Connector for TerminalConnector {
    fn connect(&self, _: &str, _: &SessionRef) -> Result<Connected, ConnectFailure> {
        let root =
            shelbi_state::sessions_dir().map_err(|e| ConnectFailure::Message(e.to_string()))?;
        let name = local_session_name(&self.session);
        let live = shelbi_client::list(&root)
            .ok()
            .and_then(|sessions| shelbi_client::choose_session(&sessions, &name));
        let sock = match live {
            Some(live) => live.sock,
            None => self.spawn()?,
        };
        let deadline = Instant::now() + BIND_DEADLINE;
        let (conn, events) = loop {
            match Connection::open(&sock, None, capability::ALL) {
                Ok(opened) => break opened,
                Err(ClientError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(50))
                }
                Err(e) => return Err(ConnectFailure::Message(e.to_string())),
            }
        };
        let size = match conn.info() {
            Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
            Err(_) => self.size,
        };
        conn.attach(None)
            .map_err(|e| ConnectFailure::Message(e.to_string()))?;
        Ok(Connected { conn, events, size })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::remote::FakeRemote;
    use std::collections::VecDeque;
    use std::sync::Mutex;

    fn session() -> RemoteSession {
        RemoteSession {
            host: "devbox".into(),
            user: "jlong".into(),
            project: "shelbi".into(),
            label: Some("Shelbi".into()),
            path: "~/Workspaces/shelbi".into(),
        }
    }

    /// Exits with each scripted code in turn.
    struct Exits(VecDeque<Option<i32>>, usize);

    impl TerminalRunner for Exits {
        fn run(&mut self, argv: &[String]) -> std::io::Result<Option<i32>> {
            assert_eq!(argv[0], "ssh");
            self.1 += 1;
            Ok(self.0.pop_front().expect("ran more often than scripted"))
        }
    }

    /// Records each notice and pause; stops after `stop_after` waits.
    struct Waits {
        notices: Vec<(String, Duration)>,
        stop_after: usize,
    }

    impl ReconnectWait for Waits {
        fn wait(&mut self, notice: &str, pause: Duration) -> bool {
            std::thread::sleep(pause);
            self.notices.push((notice.to_string(), pause));
            self.notices.len() <= self.stop_after
        }
    }

    /// Answers each connect with the next scripted result, then Ok.
    struct Flaky(Mutex<VecDeque<Result<(), RemoteError>>>);

    impl RemoteConnector for Flaky {
        fn find_sessions(&self, _: &str) -> Result<(String, Vec<RemoteSession>), RemoteError> {
            unreachable!()
        }
        fn connect(&self, _: &RemoteSession) -> Result<(), RemoteError> {
            self.0.lock().unwrap().pop_front().unwrap_or(Ok(()))
        }
        fn start_daemon(&self, _: &str) -> Result<(), RemoteError> {
            panic!("a reconnect never starts the daemon")
        }
    }

    fn policy() -> ReconnectPolicy {
        ReconnectPolicy {
            first_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(2),
        }
    }

    fn down(reason: &str) -> Result<(), RemoteError> {
        Err(RemoteError::Unreachable {
            host: "devbox".into(),
            reason: reason.into(),
        })
    }

    #[test]
    fn quitting_the_far_shelbi_closes_the_session() {
        let mut runner = Exits(VecDeque::from([Some(0)]), 0);
        let mut waits = Waits { notices: Vec::new(), stop_after: usize::MAX };
        let end = run(&session(), &FakeRemote::new(), &mut runner, &mut waits, policy());
        assert_eq!(end, TerminalEnd::Closed);
        assert_eq!(end.status(&session()), "Disconnected from devbox");
        assert!(waits.notices.is_empty());
    }

    #[test]
    fn a_drop_reconnects_with_the_shared_notice_until_the_host_answers() {
        let mut runner = Exits(VecDeque::from([Some(255), Some(0)]), 0);
        let mut waits = Waits { notices: Vec::new(), stop_after: usize::MAX };
        let connector = Flaky(Mutex::new(VecDeque::from([down("Network is unreachable"), Ok(())])));
        let end = run(&session(), &connector, &mut runner, &mut waits, policy());
        assert_eq!(end, TerminalEnd::Closed);
        assert_eq!(runner.1, 2, "the terminal ran again after the reconnect");
        let notices: Vec<&str> = waits.notices.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            notices,
            [
                "Reconnecting to devbox… (attempt 1) The connection dropped.",
                "Reconnecting to devbox… (attempt 2) Network is unreachable",
            ]
        );
        assert_eq!(waits.notices[0].1, Duration::ZERO, "the first attempt is immediate");
        assert!(waits.notices[1].1 > Duration::ZERO, "later attempts back off");
    }

    #[test]
    fn the_user_can_stop_reconnecting() {
        let mut runner = Exits(VecDeque::from([None]), 0);
        let mut waits = Waits { notices: Vec::new(), stop_after: 1 };
        let connector = Flaky(Mutex::new(VecDeque::from([down("timed out"), down("timed out")])));
        let end = run(&session(), &connector, &mut runner, &mut waits, policy());
        assert_eq!(end, TerminalEnd::Stopped);
        assert_eq!(end.status(&session()), "Stopped reconnecting to devbox");
    }

    #[test]
    fn a_reconnect_that_finds_the_daemon_stopped_hands_it_back_without_starting_it() {
        let mut runner = Exits(VecDeque::from([Some(255)]), 0);
        let mut waits = Waits { notices: Vec::new(), stop_after: usize::MAX };
        let connector = Flaky(Mutex::new(VecDeque::from([Err(RemoteError::DaemonStopped {
            host: "devbox".into(),
        })])));
        let end = run(&session(), &connector, &mut runner, &mut waits, policy());
        assert_eq!(
            end,
            TerminalEnd::Refused(RemoteError::DaemonStopped { host: "devbox".into() })
        );
        assert_eq!(end.status(&session()), "Shelbi isn't running on devbox.");
    }

    #[test]
    fn a_far_failure_ends_with_its_status() {
        let mut runner = Exits(VecDeque::from([Some(127)]), 0);
        let mut waits = Waits { notices: Vec::new(), stop_after: usize::MAX };
        let end = run(&session(), &FakeRemote::new(), &mut runner, &mut waits, policy());
        assert_eq!(end, TerminalEnd::Failed(127));
        assert_eq!(end.status(&session()), "Shelbi on devbox exited with status 127");
    }

    #[test]
    fn the_local_session_is_named_for_the_host_and_project() {
        assert_eq!(local_session_name(&session()), "remote/devbox/shelbi");
        assert_eq!(
            TerminalConnector::target(&session()),
            SessionRef::Workspace("Shelbi".into())
        );
    }

    #[test]
    fn a_host_that_cant_be_reached_keeps_the_view_reconnecting() {
        let _sandbox = crate::launch::test_support::Sandbox::new("terminal-unreachable");
        let connector = TerminalConnector {
            session: session(),
            // `devbox` isn't scripted: the fake answers it as unreachable.
            remote: Arc::new(FakeRemote::new().failing_connect(RemoteError::Unreachable {
                host: "devbox".into(),
                reason: "Network is unreachable".into(),
            })),
            exe: PathBuf::from("/nonexistent/shelbi"),
            size: GridSize::new(80, 24),
        };
        let target = TerminalConnector::target(&session());
        assert!(matches!(
            connector.connect("shelbi", &target),
            Err(ConnectFailure::Unreachable(reason)) if reason == "Network is unreachable"
        ));

        let stopped = TerminalConnector {
            remote: Arc::new(FakeRemote::new().with_stopped_daemon("devbox")),
            ..connector
        };
        assert!(matches!(
            stopped.connect("shelbi", &target),
            Err(ConnectFailure::Message(m)) if m == "Shelbi isn't running on devbox."
        ));
    }

    #[test]
    fn the_labels_name_the_user_host_and_project() {
        assert_eq!(connected_label(&session()), "Connected to jlong@devbox · Shelbi");
        let mut typed = session();
        typed.host = "jlong@build".into();
        typed.label = None;
        assert_eq!(window_title(&typed), "Shelbi · jlong@build · shelbi");
    }
}
