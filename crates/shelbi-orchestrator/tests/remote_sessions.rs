//! A remote window's terminal panes end to end: real sessions on a "far"
//! machine (a temp Shelbi home), an in-process relay over a socket pair
//! standing in for `ssh <host> shelbi relay`, and the window's
//! `RemoteSessionConnector` over the host's `HostLink`.
//!
//! Covers the panes a remote window attaches over the link (the orchestrator,
//! a far-local workspace's agent, review content, a user shell), the shared
//! "Reconnecting to <host>… (attempt N)" loop after the channel drops, resuming
//! the same session by attach and replay, a reconnect that never starts the
//! far daemon, content sessions ensured through the far `control` service, and
//! the orchestrator's title asked over the link. No SSH, no real daemon,
//! nothing of the real `~/.shelbi`.

#![cfg(unix)]

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use shelbi_app::launch::link::HostLink;
use shelbi_app::launch::remote::{FakeRemote, RemoteConnector, RemoteError, RemoteSession};
use shelbi_app::session::remote::{RemoteSessionConnector, CONNECTION_LOST};
use shelbi_app::session::{
    reconnecting_notice, ConnectFailure, Connected, Connector, ReconnectController, ReconnectPolicy,
    Relauncher, ReviewRelauncher, SessionRef, WorkspaceContentRelauncher,
};
use shelbi_app::source::remote::{ChannelLink, RelayLink};
use shelbi_app::source::ProjectLink;
use shelbi_client::{Connection, RelayChannel, RelayServices, SessionEvent};
use shelbi_proto::control::{
    self, ClientMsg, RelaunchPane, ReviewRole, ReviewSessionOp, ServerMsg, StateWrite, WorkspaceSessionOp,
    CONTROL_PROTOCOL_VERSION,
};
use shelbi_proto::capability;
use shelbi_session::layout::SessionPaths;
use shelbi_session::RunArgs;

const PROJECT: &str = "demo";
const HOST: &str = "devbox";
const WAIT: Duration = Duration::from_secs(10);

/// `shelbi_session::run` finds its directory through `SHELBI_HOME`, which is
/// process-wide: one far machine at a time.
static ENV: Mutex<()> = Mutex::new(());

/// The far machine: a Shelbi home with real sessions, and the relays opened
/// to it.
struct Far {
    _lock: MutexGuard<'static, ()>,
    home: tempfile::TempDir,
    root: PathBuf,
    sockets: Vec<PathBuf>,
    /// The near ends of the relay channels opened so far, to cut them.
    channels: Arc<Mutex<Vec<UnixStream>>>,
    /// How many opens to refuse as unreachable before opening again.
    down: Arc<AtomicUsize>,
    /// The far daemon's control socket, when a test runs one.
    control: Option<PathBuf>,
}

impl Far {
    fn new() -> Far {
        let lock = ENV.lock().unwrap_or_else(|p| p.into_inner());
        for var in ["SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK", "SHELBI_HUB_ADDR", "SHELBI_ROOT"] {
            std::env::remove_var(var);
        }
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", home.path());
        let root = home.path().join("sessions");
        std::fs::create_dir_all(&root).unwrap();
        Far {
            _lock: lock,
            home,
            root,
            sockets: Vec::new(),
            channels: Arc::default(),
            down: Arc::default(),
            control: None,
        }
    }

    /// Start a real session named `name` running `script` under `sh`, and
    /// wait until it accepts connections.
    fn start(&mut self, name: &str, script: &str) {
        let id = shelbi_session::layout::derive_id_now(name);
        let paths = SessionPaths::new(&self.root, &id);
        let args = RunArgs {
            id,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            cols: 80,
            rows: 24,
            task: None,
            raw_output_log: false,
            child_argv: vec!["/bin/sh".into(), "-c".into(), script.into()],
            manage_daemon: false,
        };
        std::thread::spawn(move || {
            let _ = shelbi_session::run(args);
        });
        until("the session's socket", || UnixStream::connect(paths.sock()).ok().map(drop));
        self.sockets.push(paths.sock());
    }

    /// Run a far daemon's control socket here.
    fn control_socket(&mut self) -> UnixListener {
        let path = self.home.path().join("control.sock");
        let listener = UnixListener::bind(&path).unwrap();
        self.control = Some(path);
        listener
    }

    /// Opens a relay to the far machine over a fresh socket pair, refusing
    /// the next [`Far::go_down`] opens as unreachable.
    fn opener(&self) -> impl Fn(&str) -> Result<Arc<dyn RelayLink>, RemoteError> + Send + Sync + 'static {
        let (root, channels, down, control) =
            (self.root.clone(), self.channels.clone(), self.down.clone(), self.control.clone());
        move |host: &str| {
            if down.load(Ordering::SeqCst) > 0 {
                down.fetch_sub(1, Ordering::SeqCst);
                return Err(RemoteError::Unreachable {
                    host: host.to_string(),
                    reason: "Network is unreachable".into(),
                });
            }
            let (near, far) = UnixStream::pair().unwrap();
            let far_read = far.try_clone().unwrap();
            let services = RelayServices {
                control_socket: control.clone(),
                ..RelayServices::default()
            };
            let root = root.clone();
            std::thread::spawn(move || {
                let _ = shelbi_client::serve_relay_with(Box::new(far_read), Box::new(far), &root, services);
            });
            channels.lock().unwrap().push(near.try_clone().unwrap());
            let near_read = near.try_clone().unwrap();
            let channel = RelayChannel::new(Box::new(near_read), Box::new(near)).unwrap();
            Ok(Arc::new(ChannelLink::new(host, channel)) as Arc<dyn RelayLink>)
        }
    }

    /// A host link to the far machine (no pause between reopen attempts).
    fn link(&self) -> Arc<HostLink> {
        Arc::new(HostLink::new(HOST, self.opener()).with_pause(Duration::ZERO))
    }

    /// The next `n` opens find the host unreachable.
    fn go_down(&self, n: usize) {
        self.down.store(n, Ordering::SeqCst);
    }

    /// Cut every relay channel, as an SSH drop does.
    fn drop_channels(&self) {
        for channel in self.channels.lock().unwrap().drain(..) {
            let _ = channel.shutdown(Shutdown::Both);
        }
    }
}

impl Drop for Far {
    fn drop(&mut self) {
        for sock in &self.sockets {
            if let Ok((conn, _events)) = Connection::open(sock, None, capability::ALL) {
                let _ = conn.kill(Some(9));
            }
        }
    }
}

/// The far project as the Connect dialog found it.
fn session() -> RemoteSession {
    RemoteSession {
        host: HOST.into(),
        user: "jlong".into(),
        project: PROJECT.into(),
        label: None,
        path: "~/Workspaces/demo".into(),
    }
}

fn healthy() -> Arc<FakeRemote> {
    Arc::new(FakeRemote::new().with_host(HOST, "jlong", &[(PROJECT, "~/Workspaces/demo")]))
}

fn until<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Everything the session showed (the attach replay and live output) until
/// `needle` appears.
fn read_until(events: &std::sync::mpsc::Receiver<SessionEvent>, needle: &str) -> String {
    let deadline = Instant::now() + WAIT;
    let mut seen = Vec::new();
    loop {
        let text = String::from_utf8_lossy(&seen).to_string();
        if text.contains(needle) {
            return text;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(SessionEvent::Output { data, .. }) => seen.extend_from_slice(&data),
            Ok(SessionEvent::Resync { replay, .. }) => seen.extend_from_slice(&replay),
            Ok(_) => {}
            Err(e) => panic!("no `{needle}` before {e:?}; saw {text:?}"),
        }
    }
}

/// Wait for the stream to end (the channel under it dropped).
fn read_to_end(events: &std::sync::mpsc::Receiver<SessionEvent>) {
    let deadline = Instant::now() + WAIT;
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match events.recv_timeout(left) {
            Ok(_) => {}
            Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => panic!("the stream outlived its channel"),
        }
    }
}

/// An attached pane: its connection and size, and its event stream.
struct Pane {
    conn: Connection,
    size: shelbi_app::session::GridSize,
}

fn attach(connector: &dyn Connector, target: &SessionRef) -> (Pane, std::sync::mpsc::Receiver<SessionEvent>) {
    match connector.connect(PROJECT, target) {
        Ok(Connected { conn, events, size }) => (Pane { conn, size }, events.into_inner()),
        Err(e) => panic!("couldn't attach {target:?}: {e:?}"),
    }
}

fn short_id(link: &dyn RelayLink, name: &str) -> String {
    let sessions = link.list_sessions().unwrap();
    let mut ids = sessions.iter().filter(|s| s.name == name && s.alive).map(|s| s.short_id.clone());
    let id = ids.next().expect("the session is on the far list");
    assert!(ids.next().is_none(), "one session named {name}");
    id
}

#[test]
fn a_dropped_channel_reconnects_with_the_shared_notice_and_resumes_the_same_session() {
    let mut far = Far::new();
    far.start("demo/orch", "printf 'orch ready\\n'; exec cat");
    let link = far.link();
    let remote = healthy();
    let connector = RemoteSessionConnector::new(link.clone(), session(), remote.clone());

    let (pane, events) = attach(&connector, &SessionRef::Orchestrator);
    read_until(&events, "orch ready");
    pane.conn.input(b"before-the-drop\n").unwrap();
    read_until(&events, "before-the-drop");
    let before = short_id(&*link, "demo/orch");

    // The SSH connection drops, and the host doesn't answer the first
    // attempt: the pane's stream ends with no exit, and it reconnects.
    far.go_down(1);
    far.drop_channels();
    read_to_end(&events);
    drop(pane);
    until("the link to notice", || (!link.is_up()).then_some(()));

    let policy = ReconnectPolicy {
        first_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(40),
    };
    let mut reconnect = ReconnectController::new(policy);
    reconnect.link_lost(CONNECTION_LOST, Instant::now());
    let mut notices = Vec::new();
    let resumed = loop {
        let now = Instant::now();
        if reconnect.take_due(now).is_none() {
            std::thread::sleep(reconnect.wait(now).unwrap_or_default());
            continue;
        }
        notices.push(reconnecting_notice(HOST, reconnect.attempt(), reconnect.reason()));
        match connector.connect(PROJECT, &SessionRef::Orchestrator) {
            Ok(c) => {
                reconnect.connected();
                break c;
            }
            Err(ConnectFailure::Unreachable(reason)) => reconnect.unreachable(&reason, Instant::now()),
            Err(other) => panic!("expected to reconnect, got {other:?}"),
        }
        assert!(notices.len() < 10, "never reconnected: {notices:?}");
    };
    assert_eq!(
        notices,
        vec![
            "Reconnecting to devbox… (attempt 1) The connection was lost.".to_string(),
            "Reconnecting to devbox… (attempt 2) Network is unreachable".to_string(),
        ]
    );

    // The same session, its screen replayed, and still taking input.
    let events = resumed.events.into_inner();
    let replay = read_until(&events, "before-the-drop");
    assert!(replay.contains("orch ready"), "{replay:?}");
    assert_eq!(short_id(&*link, "demo/orch"), before, "the same session, not a new one");
    resumed.conn.input(b"after-the-drop\n").unwrap();
    read_until(&events, "after-the-drop");

    // Each attempt checked the host first; none started its daemon.
    assert_eq!(remote.connected().len(), 3, "the first attach and both attempts checked the host");
    assert!(remote.started().is_empty(), "a reconnect never starts the far daemon");
    assert_eq!(link.opens(), 2, "one channel at first, one after the drop");
}

#[test]
fn a_reconnect_to_a_stopped_far_daemon_is_refused_and_never_starts_it() {
    let mut far = Far::new();
    far.start("demo/orch", "printf 'orch ready\\n'; exec cat");
    let link = far.link();
    let (pane, events) = attach(
        &RemoteSessionConnector::new(link.clone(), session(), healthy()),
        &SessionRef::Orchestrator,
    );
    read_until(&events, "orch ready");

    // The far Shelbi is stopped while the connection is down.
    far.drop_channels();
    read_to_end(&events);
    drop(pane);
    let stopped = Arc::new(
        FakeRemote::new()
            .with_host(HOST, "jlong", &[(PROJECT, "~/Workspaces/demo")])
            .with_stopped_daemon(HOST),
    );
    let connector = RemoteSessionConnector::new(link.clone(), session(), stopped.clone());
    match connector.connect(PROJECT, &SessionRef::Orchestrator) {
        Err(ConnectFailure::Message(message)) => assert_eq!(message, "Shelbi isn't running on devbox."),
        Err(other) => panic!("expected the refusal, got {other:?}"),
        Ok(_) => panic!("a stopped daemon's host was reconnected"),
    }
    assert_eq!(connector.refusal(), Some(RemoteError::DaemonStopped { host: HOST.into() }));
    assert_eq!(link.opens(), 1, "no channel opens past the refused check");
    assert!(stopped.started().is_empty(), "the daemon is only ever started on the user's say-so");

    // Once the user starts it, the next attempt gets through.
    stopped.start_daemon(HOST).unwrap();
    let (_pane, events) = attach(&connector, &SessionRef::Orchestrator);
    read_until(&events, "orch ready");
    assert_eq!(connector.refusal(), None);
}

#[test]
fn every_far_pane_kind_attaches_over_the_link() {
    let mut far = Far::new();
    far.start("demo/orch", "printf 'i am the orchestrator\\n'; exec cat");
    far.start("demo/ws/alpha", "printf 'i am alpha s agent\\n'; exec cat");
    far.start("demo/review/r1/editor", "printf 'i am the review editor\\n'; exec cat");
    far.start("demo/ws/alpha/shell", "printf 'i am alpha s shell\\n'; exec cat");
    let connector = RemoteSessionConnector::new(far.link(), session(), healthy());

    for (target, says) in [
        (SessionRef::Orchestrator, "i am the orchestrator"),
        (SessionRef::Workspace("alpha".into()), "i am alpha s agent"),
        (
            SessionRef::Review {
                slot: "r1".into(),
                role: "editor".into(),
            },
            "i am the review editor",
        ),
        (SessionRef::workspace_shell("alpha"), "i am alpha s shell"),
    ] {
        let (pane, events) = attach(&connector, &target);
        read_until(&events, says);
        assert!(pane.size.cols > 0 && pane.size.rows > 0);
    }

    // A declared workspace with no session is idle on the host; a content
    // session just asked for is still starting.
    match connector.connect(PROJECT, &SessionRef::Workspace("beta".into())) {
        Err(ConnectFailure::Idle(info)) => {
            assert_eq!(info.name, "beta");
            assert_eq!(info.machine, HOST);
        }
        other => panic!("expected idle, got {:?}", other.err()),
    }
    let diff = SessionRef::Review {
        slot: "r1".into(),
        role: "diff".into(),
    };
    assert!(matches!(
        connector.connect(PROJECT, &diff),
        Err(ConnectFailure::Starting(_))
    ));
}

/// Serve one control connection as the far daemon: the hello, then one
/// request, answered `Done`.
fn serve_one_request(listener: &UnixListener) -> ClientMsg {
    let (mut stream, _) = listener.accept().unwrap();
    let mut buf = Vec::new();
    let read = |stream: &mut UnixStream, buf: &mut Vec<u8>| -> ClientMsg {
        loop {
            match control::decode::<ClientMsg>(buf) {
                Ok((msg, used)) => {
                    buf.drain(..used);
                    return msg;
                }
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => {
                    let mut chunk = [0u8; 4096];
                    let n = stream.read(&mut chunk).unwrap();
                    assert!(n > 0, "the link hung up mid-frame");
                    buf.extend_from_slice(&chunk[..n]);
                }
                Err(e) => panic!("bad control frame: {e}"),
            }
        }
    };
    assert!(matches!(read(&mut stream, &mut buf), ClientMsg::Hello { .. }));
    let hello = ServerMsg::Hello {
        protocol: CONTROL_PROTOCOL_VERSION,
        daemon_version: shelbi_state::CLIENT_VERSION.to_string(),
    };
    stream.write_all(&control::encode(&hello).unwrap()).unwrap();
    let request = read(&mut stream, &mut buf);
    let request_id = match &request {
        ClientMsg::ReviewSession(r) => r.request_id,
        ClientMsg::WorkspaceSession(r) => r.request_id,
        ClientMsg::WriteState(r) => r.request_id,
        other => panic!("unexpected request {other:?}"),
    };
    let done = ServerMsg::Done { request_id, state: None };
    stream.write_all(&control::encode(&done).unwrap()).unwrap();
    request
}

#[test]
fn content_sessions_are_ensured_through_the_far_control_service() {
    let mut far = Far::new();
    let control = far.control_socket();
    let link = far.link();
    let connector = Arc::new(RemoteSessionConnector::new(link.clone(), session(), healthy()));
    let project = ProjectLink::remote(PROJECT, link.clone(), connector);
    link.connect(&|| Ok(())).unwrap();

    let far_daemon = {
        let control = control.try_clone().unwrap();
        std::thread::spawn(move || (serve_one_request(&control), serve_one_request(&control)))
    };
    let review = ReviewRelauncher {
        project: PROJECT.into(),
        task: "t-7".into(),
        link: Some(project.clone()),
    };
    let editor = SessionRef::Review {
        slot: "r1".into(),
        role: "editor".into(),
    };
    review.relaunch(&editor, false).unwrap();
    let workspace = WorkspaceContentRelauncher {
        project: PROJECT.into(),
        link: Some(project.clone()),
    };
    workspace.relaunch(&SessionRef::workspace_shell("alpha"), false).unwrap();

    let (first, second) = far_daemon.join().unwrap();
    match first {
        ClientMsg::ReviewSession(r) => {
            assert_eq!((r.project.as_str(), r.task.as_str()), (PROJECT, "t-7"));
            assert_eq!(r.op, ReviewSessionOp::Ensure { role: ReviewRole::Editor });
        }
        other => panic!("expected the review ensure, got {other:?}"),
    }
    match second {
        ClientMsg::WorkspaceSession(r) => {
            assert_eq!((r.project.as_str(), r.workspace.as_str()), (PROJECT, "alpha"));
            assert_eq!(r.op, WorkspaceSessionOp::EnsureShell);
        }
        other => panic!("expected the shell ensure, got {other:?}"),
    }

    // A reopen of a persistent pane asks the far daemon for the relaunch
    // marker, and writes none into this machine's Shelbi home.
    let far_daemon = std::thread::spawn(move || serve_one_request(&control));
    review.relaunch(&SessionRef::Orchestrator, true).unwrap();
    match far_daemon.join().unwrap() {
        ClientMsg::WriteState(w) => {
            assert_eq!(w.project, PROJECT);
            assert_eq!(
                w.write,
                StateWrite::RequestRelaunch {
                    pane: RelaunchPane::Orchestrator
                }
            );
        }
        other => panic!("expected the relaunch request, got {other:?}"),
    }
    assert!(!shelbi_state::project_dir(PROJECT).unwrap().exists());
}

#[test]
fn the_orchestrator_title_comes_over_the_link() {
    let mut far = Far::new();
    let link = far.link();
    let connector = Arc::new(RemoteSessionConnector::new(link.clone(), session(), healthy()));
    let project = ProjectLink::remote(PROJECT, link.clone(), connector);
    assert_eq!(project.orchestrator_title(), None, "no channel yet: no title");

    link.connect(&|| Ok(())).unwrap();
    assert_eq!(project.orchestrator_title(), None, "no orchestrator yet");
    far.start("demo/orch", "printf '\\033]0;shelbi:waiting\\007'; exec cat");
    let title = until("the far title", || project.orchestrator_title());
    assert_eq!(title, "shelbi:waiting");

    far.drop_channels();
    until("the link to notice", || (!link.is_up()).then_some(()));
    assert_eq!(project.orchestrator_title(), None, "a dropped link has no title to give");
}
