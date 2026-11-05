//! A remote window's terminal panes end to end: real sessions on a "far"
//! machine (a temp Shelbi home), an in-process relay over a socket pair
//! standing in for `ssh <host> shelbi relay`, and the window's
//! `RemoteSessionConnector` over the host's `HostLink`.
//!
//! Covers the panes a remote window attaches over the link (the orchestrator,
//! a far-local workspace's agent, review content, a user shell), the shared
//! "Reconnecting to <host>… (attempt N)" loop after the channel drops, resuming
//! the same session by attach and replay, a reconnect that never starts the
//! far daemon, content sessions ensured through the far `control` service, the
//! orchestrator's title asked over the link, and a workspace on another of the
//! far project's machines (a third machine only the far side's own relay
//! reaches) streamed through the far relay's `session` service and reconnected
//! when either hop drops. No SSH, no real daemon, nothing of the real
//! `~/.shelbi`.

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
    /// The far relay's `query` service, when a test answers one.
    query: Option<shelbi_client::QueryHandler>,
    /// The far relay's `session` service, when a test offers one.
    sessions: Option<shelbi_client::SessionOpener>,
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
            query: None,
            sessions: None,
        }
    }

    /// Answer the far relay's `query` service with `answer`.
    fn answer_queries<F>(&mut self, answer: F)
    where
        F: shelbi_app::source::wire::Answerer + Send + Sync + 'static,
    {
        self.query = Some(shelbi_app::source::wire::handler(answer));
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
        let (root, channels, down, control, query, sessions) = (
            self.root.clone(),
            self.channels.clone(),
            self.down.clone(),
            self.control.clone(),
            self.query.clone(),
            self.sessions.clone(),
        );
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
                query: query.clone(),
                session: sessions.clone(),
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
fn a_refused_sign_in_leaves_the_pane_waiting_and_it_comes_back_after_sign_in() {
    let mut far = Far::new();
    far.start("demo/orch", "printf 'orch ready\\n'; exec cat");
    let link = far.link();
    let refused = RemoteError::AuthFailed {
        host: HOST.into(),
        reason: "Permission denied (publickey,password).".into(),
        asked: true,
    };
    let saying_no = Arc::new(FakeRemote::new().failing_connect(refused.clone()));
    let connector = RemoteSessionConnector::new(link.clone(), session(), saying_no);
    for _ in 0..3 {
        // Waiting, not dead: a pane keeps its backoff, and nothing is asked
        // (the link holds the answer).
        match connector.connect(PROJECT, &SessionRef::Orchestrator) {
            Err(ConnectFailure::Unreachable(line)) => assert!(line.contains("Couldn't sign in to"), "{line}"),
            Err(other) => panic!("expected the pane to keep waiting, got {other:?}"),
            Ok(_) => panic!("no channel opens past a refused sign-in"),
        }
        assert_eq!(connector.refusal(), Some(refused.clone()), "the window offers Sign In");
    }
    assert_eq!(link.opens(), 0);

    // The user signs in from the bar: the window's refusal is over at once,
    // and the pane's next attempt attaches.
    link.allow_sign_in();
    link.connect(&|| Ok(())).unwrap();
    assert_eq!(connector.refusal(), None, "over before the pane has been back");
    let (_pane, events) = attach(&connector, &SessionRef::Orchestrator);
    read_until(&events, "orch ready");
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
        ClientMsg::Mutate(r) => r.request_id,
        other => panic!("unexpected request {other:?}"),
    };
    // An add reports the new issue's id, as the far daemon does.
    let id = matches!(request, ClientMsg::Mutate(_)).then(|| "42".to_string());
    let done = ServerMsg::Done { request_id, state: None, id };
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

/// One control frame off `stream`.
fn read_msg(stream: &mut UnixStream, buf: &mut Vec<u8>) -> ClientMsg {
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
}

#[test]
fn edit_starts_a_far_editor_session_the_window_attaches_to_by_name() {
    use shelbi_app::far::{FarFile, Request};
    let mut far = Far::new();
    let control = far.control_socket();
    let link = far.link();
    let connector = Arc::new(RemoteSessionConnector::new(link.clone(), session(), healthy()));
    let project = ProjectLink::remote(PROJECT, link.clone(), connector);
    link.connect(&|| Ok(())).unwrap();
    let file = far.home.path().join("notes.md");
    std::fs::write(&file, "far notes\n").unwrap();

    // The far daemon answers with the real far side, its editor a session
    // in the far sessions directory that names the file it was given.
    let root = far.root.clone();
    let started: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let sockets = started.clone();
    let far_daemon = std::thread::spawn(move || {
        let (mut stream, _) = control.accept().unwrap();
        let mut buf = Vec::new();
        assert!(matches!(read_msg(&mut stream, &mut buf), ClientMsg::Hello { .. }));
        let hello = ServerMsg::Hello {
            protocol: CONTROL_PROTOCOL_VERSION,
            daemon_version: shelbi_state::CLIENT_VERSION.to_string(),
        };
        stream.write_all(&control::encode(&hello).unwrap()).unwrap();
        let ClientMsg::AppRequest(req) = read_msg(&mut stream, &mut buf) else {
            panic!("expected an app request");
        };
        let request: Request = serde_json::from_value(req.body).unwrap();
        let services = shelbi_orchestrator::far::FarServices {
            pool: shelbi_orchestrator::pool_admin::workspace_service(),
            machines: shelbi_orchestrator::pool_admin::machine_service(),
            readiness: shelbi_orchestrator::machine_readiness::service(),
            spawn_editor: Arc::new(move |name, cwd, argv| {
                let id = shelbi_session::layout::derive_id_now(name);
                let paths = SessionPaths::new(&root, &id);
                let args = RunArgs {
                    id,
                    name: name.to_string(),
                    cwd: cwd.to_path_buf(),
                    cols: 80,
                    rows: 24,
                    task: None,
                    raw_output_log: false,
                    child_argv: vec![
                        "/bin/sh".into(),
                        "-c".into(),
                        "printf 'editing %s\\n' \"$1\"; exec cat".into(),
                        "edit".into(),
                        argv[4].clone(),
                    ],
                    manage_daemon: false,
                };
                std::thread::spawn(move || {
                    let _ = shelbi_session::run(args);
                });
                until("the editor session's socket", || UnixStream::connect(paths.sock()).ok().map(drop));
                sockets.lock().unwrap().push(paths.sock());
                Ok(())
            }),
            agent_send: shelbi_orchestrator::agent_send::service(),
        };
        let reply = shelbi_orchestrator::far::answer_with(&services, &req.project, request);
        let reply = ServerMsg::Reply {
            request_id: req.request_id,
            body: serde_json::to_value(reply).unwrap(),
        };
        stream.write_all(&control::encode(&reply).unwrap()).unwrap();
    });

    let name = project
        .open_far_editor(FarFile::path(file.to_string_lossy()))
        .expect("the far daemon starts the editor");
    far_daemon.join().unwrap();
    assert!(name.starts_with(&format!("{PROJECT}/edit/")), "{name}");
    let Connected { conn, events, .. } = project.attach_named(&name).expect("the window attaches by name");
    let shown = read_until(&events.into_inner(), "editing");
    assert!(shown.contains(&format!("editing {}", file.display())), "{shown:?}");
    let _ = conn.kill(Some(9));
    far.sockets.extend(started.lock().unwrap().drain(..));
}

#[test]
fn a_far_upgrade_while_away_refuses_the_reconnect_and_offers_reopen() {
    let mut far = Far::new();
    far.start("demo/orch", "printf 'orch ready\\n'; exec cat");
    let link = far.link();
    let remote = healthy();
    let connector = RemoteSessionConnector::new(link.clone(), session(), remote.clone());
    let (pane, events) = attach(&connector, &SessionRef::Orchestrator);
    read_until(&events, "orch ready");

    // The far Shelbi is upgraded while the connection is down.
    far.drop_channels();
    read_to_end(&events);
    drop(pane);
    remote.set_far_version(HOST, "0.0.2", Some(shelbi_app::launch::NativeCapability::current()));
    match connector.connect(PROJECT, &SessionRef::Orchestrator) {
        Err(ConnectFailure::Message(message)) => {
            assert!(message.starts_with("Shelbi on devbox is now 0.0.2 and "), "{message}");
            assert!(message.ends_with("Reopen to continue in its terminal view."), "{message}");
        }
        Err(other) => panic!("expected the refusal, got {other:?}"),
        Ok(_) => panic!("a changed host was reconnected"),
    }
    assert!(
        matches!(connector.refusal(), Some(RemoteError::NoLongerNative { ref host, .. }) if host == HOST),
        "the window offers Reopen: {:?}",
        connector.refusal()
    );
    assert_eq!(link.opens(), 1, "nothing is sent to a daemon that may reject it");

    // Back on this build (say both were updated), the next attempt gets through.
    remote.set_far_version(HOST, env!("CARGO_PKG_VERSION"), Some(shelbi_app::launch::NativeCapability::current()));
    let (_pane, events) = attach(&connector, &SessionRef::Orchestrator);
    read_until(&events, "orch ready");
    assert_eq!(connector.refusal(), None);
}

// --- two hops: a workspace on another of the far project's machines --------

/// The far project's other machine, which this machine has no route to.
const THIRD: &str = "gpu-box";

/// The far project: workspace `alpha` on the far host itself, `gamma` on
/// [`THIRD`], another of its machines.
fn write_far_project(far: &Far) {
    // `Far::new` pointed `SHELBI_HOME` at the far home.
    let projects = shelbi_state::projects_dir().unwrap();
    assert!(projects.starts_with(far.home.path()), "write into the far home");
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join(format!("{PROJECT}.yaml")),
        format!(
            "name: {PROJECT}\nrepo: ''\ndefault_branch: main\n\
             orchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp\n\
             \x20 - name: {THIRD}\n    kind: ssh\n    host: {THIRD}\n    work_dir: /tmp\n\
             workspaces:\n  - {{ name: alpha, machine: hub, runner: claude }}\n\
             \x20 - {{ name: gamma, machine: {THIRD}, runner: claude }}\n"
        ),
    )
    .unwrap();
}

/// The third machine as the far side reaches it: the far hub's relay seam
/// (`remote_session`'s, in place of `ssh gpu-box shelbi relay` with its own
/// `-R` forward) opening an in-process relay over this machine's own sessions
/// directory. Only the far side ever opens it.
struct ThirdMachine {
    root: PathBuf,
    /// The far side's ends of the relay channels opened so far, to cut them.
    channels: Mutex<Vec<UnixStream>>,
    /// How many opens to refuse as unreachable before opening again.
    down: AtomicUsize,
    opens: AtomicUsize,
}

impl ThirdMachine {
    fn cut(&self) {
        for channel in self.channels.lock().unwrap().drain(..) {
            let _ = channel.shutdown(Shutdown::Both);
        }
    }
}

impl shelbi_orchestrator::remote_session::RemoteSsh for ThirdMachine {
    fn launch(&self, _: &shelbi_core::Host, _: &str, _: &shelbi_session::SpawnSpec) -> shelbi_core::Result<()> {
        Err(shelbi_core::Error::Other("nothing is launched in this test".into()))
    }

    fn open_relay(
        &self,
        host: &shelbi_core::Host,
        _bin: &str,
    ) -> shelbi_core::Result<shelbi_orchestrator::remote_session::RelayHandle> {
        assert_eq!(
            host,
            &shelbi_core::Host::Ssh { host: THIRD.into() },
            "the far side only reaches its own machine"
        );
        if self.down.load(Ordering::SeqCst) > 0 {
            self.down.fetch_sub(1, Ordering::SeqCst);
            return Err(shelbi_core::Error::Other("Network is unreachable".into()));
        }
        self.opens.fetch_add(1, Ordering::SeqCst);
        let (near, far) = UnixStream::pair().unwrap();
        let far_read = far.try_clone().unwrap();
        let root = self.root.clone();
        std::thread::spawn(move || {
            let _ = shelbi_client::serve_relay(Box::new(far_read), Box::new(far), &root);
        });
        self.channels.lock().unwrap().push(near.try_clone().unwrap());
        let near_read = near.try_clone().unwrap();
        let channel = RelayChannel::new(Box::new(near_read), Box::new(near)).unwrap();
        Ok(shelbi_orchestrator::remote_session::RelayHandle::new(channel, Box::new(())))
    }
}

/// Installs a [`ThirdMachine`] as the far side's relay seam; clears it on drop.
struct Seam(Arc<ThirdMachine>);

impl Drop for Seam {
    fn drop(&mut self) {
        self.0.cut();
        shelbi_orchestrator::remote_session::set_test_seam(None);
    }
}

/// The far machine with the far project written, its relay answering
/// `session` (the real far-side opener) and `query` (workspace details as the
/// project declares them), and the third machine behind its seam.
fn two_hop_far() -> (Far, Seam) {
    use shelbi_app::source::wire::{Answer, Query, QueryError};
    use shelbi_app::source::WorkspaceDetails;
    let mut far = Far::new();
    write_far_project(&far);
    far.answer_queries(|query: Query, _early: &mut dyn FnMut(Answer)| match query {
        Query::WorkspaceDetails { workspace, .. } => Ok(Answer::WorkspaceDetails(Box::new(WorkspaceDetails {
            machine: if workspace == "gamma" { THIRD.into() } else { "hub".into() },
            is_remote: workspace == "gamma",
            ..WorkspaceDetails::default()
        }))),
        other => Err(QueryError::Unsupported {
            message: format!("{other:?}"),
        }),
    });
    far.sessions = Some(shelbi_orchestrator::session_connect::relay_opener());
    let third = Arc::new(ThirdMachine {
        root: far.home.path().join("third").join("sessions"),
        channels: Mutex::default(),
        down: AtomicUsize::new(0),
        opens: AtomicUsize::new(0),
    });
    std::fs::create_dir_all(&third.root).unwrap();
    shelbi_orchestrator::remote_session::set_test_seam(Some(third.clone()));
    (far, Seam(third))
}

/// Start a real session named `name` on the third machine: started as
/// [`Far::start`] does, then moved into the third machine's own sessions
/// directory, so the far host's session list never shows it.
fn start_on_third(far: &mut Far, seam: &Seam, name: &str, script: &str) {
    far.start(name, script);
    let sock = far.sockets.pop().unwrap();
    let dir = sock.parent().unwrap();
    let moved = seam.0.root.join(dir.file_name().unwrap());
    std::fs::rename(dir, &moved).unwrap();
    far.sockets.push(moved.join("sock"));
}

/// Reconnect `target` as a pane does after its stream ended, collecting the
/// shared notice for each attempt, until it attaches.
fn reconnect(connector: &dyn Connector, target: &SessionRef) -> (Connected, Vec<String>) {
    let policy = ReconnectPolicy {
        first_backoff: Duration::from_millis(20),
        max_backoff: Duration::from_millis(40),
    };
    let mut reconnect = ReconnectController::new(policy);
    reconnect.link_lost(CONNECTION_LOST, Instant::now());
    let mut notices = Vec::new();
    loop {
        let now = Instant::now();
        if reconnect.take_due(now).is_none() {
            std::thread::sleep(reconnect.wait(now).unwrap_or_default());
            continue;
        }
        notices.push(reconnecting_notice(HOST, reconnect.attempt(), reconnect.reason()));
        match connector.connect(PROJECT, target) {
            Ok(c) => return (c, notices),
            Err(ConnectFailure::Unreachable(reason)) => reconnect.unreachable(&reason, Instant::now()),
            Err(other) => panic!("expected to reconnect, got {other:?}"),
        }
        assert!(notices.len() < 10, "never reconnected: {notices:?}");
    }
}

#[test]
fn a_workspace_on_another_far_machine_streams_through_the_far_side() {
    let (mut far, seam) = two_hop_far();
    // The far hub's dispatch names a remote workspace's agent this way.
    start_on_third(&mut far, &seam, "w-gamma/ws/agent", "printf 'i am gamma on the gpu box\\n'; exec cat");
    far.start("demo/ws/alpha", "printf 'i am alpha on devbox\\n'; exec cat");
    let link = far.link();
    let connector = RemoteSessionConnector::new(link.clone(), session(), healthy());

    let (pane, events) = attach(&connector, &SessionRef::Workspace("gamma".into()));
    read_until(&events, "i am gamma on the gpu box");
    // Not on the far host's own list: only the far side can reach it.
    assert!(
        !link.list_sessions().unwrap().iter().any(|s| s.name.contains("gamma")),
        "the third machine's sessions aren't the far host's"
    );
    assert!(pane.size.cols > 0 && pane.size.rows > 0);
    pane.conn.input(b"typed on this mac\n").unwrap();
    read_until(&events, "typed on this mac");
    assert_eq!(seam.0.opens.load(Ordering::SeqCst), 1, "the far side opened its own relay to the third machine");
    assert_eq!(link.opens(), 1, "this machine opened one channel, to the far host only");

    // A far-local workspace still attaches straight from the far list.
    let (_alpha, alpha_events) = attach(&connector, &SessionRef::Workspace("alpha".into()));
    read_until(&alpha_events, "i am alpha on devbox");
    assert_eq!(seam.0.opens.load(Ordering::SeqCst), 1);
}

#[test]
fn a_two_hop_pane_reconnects_when_the_far_side_s_hop_drops() {
    let (mut far, seam) = two_hop_far();
    start_on_third(&mut far, &seam, "w-gamma/ws/agent", "printf 'gamma ready\\n'; exec cat");
    let link = far.link();
    let connector = RemoteSessionConnector::new(link.clone(), session(), healthy());
    let (pane, events) = attach(&connector, &SessionRef::Workspace("gamma".into()));
    read_until(&events, "gamma ready");
    pane.conn.input(b"before-the-drop\n").unwrap();
    read_until(&events, "before-the-drop");

    // The far side's connection to the third machine drops, and the third
    // machine doesn't answer the first attempt: the pane's stream ends with
    // no exit while this machine's channel stays up, and it reconnects.
    seam.0.down.store(1, Ordering::SeqCst);
    seam.0.cut();
    read_to_end(&events);
    drop(pane);
    assert!(link.is_up(), "only the second hop dropped");
    let (resumed, notices) = reconnect(&connector, &SessionRef::Workspace("gamma".into()));
    assert_eq!(
        notices,
        vec![
            "Reconnecting to devbox… (attempt 1) The connection was lost.".to_string(),
            "Reconnecting to devbox… (attempt 2) `gpu-box` is unreachable".to_string(),
        ]
    );

    // The same session, its screen replayed, and still taking input.
    let events = resumed.events.into_inner();
    let replay = read_until(&events, "before-the-drop");
    assert!(replay.contains("gamma ready"), "{replay:?}");
    resumed.conn.input(b"after-the-drop\n").unwrap();
    read_until(&events, "after-the-drop");
    assert_eq!(seam.0.opens.load(Ordering::SeqCst), 2, "a fresh far-side relay after the drop");
    assert_eq!(link.opens(), 1, "this machine's channel never dropped");
}

#[test]
fn a_two_hop_pane_reconnects_when_this_machine_s_hop_drops() {
    let (mut far, seam) = two_hop_far();
    start_on_third(&mut far, &seam, "w-gamma/ws/agent", "printf 'gamma ready\\n'; exec cat");
    let link = far.link();
    let remote = healthy();
    let connector = RemoteSessionConnector::new(link.clone(), session(), remote.clone());
    let (pane, events) = attach(&connector, &SessionRef::Workspace("gamma".into()));
    read_until(&events, "gamma ready");
    pane.conn.input(b"before-the-drop\n").unwrap();
    read_until(&events, "before-the-drop");

    // This machine's SSH connection to the far host drops, and the far host
    // doesn't answer the first attempt.
    far.go_down(1);
    far.drop_channels();
    read_to_end(&events);
    drop(pane);
    until("the link to notice", || (!link.is_up()).then_some(()));
    let (resumed, notices) = reconnect(&connector, &SessionRef::Workspace("gamma".into()));
    assert_eq!(
        notices,
        vec![
            "Reconnecting to devbox… (attempt 1) The connection was lost.".to_string(),
            "Reconnecting to devbox… (attempt 2) Network is unreachable".to_string(),
        ]
    );
    let events = resumed.events.into_inner();
    let replay = read_until(&events, "before-the-drop");
    assert!(replay.contains("gamma ready"), "{replay:?}");
    resumed.conn.input(b"after-the-drop\n").unwrap();
    read_until(&events, "after-the-drop");
    assert!(remote.started().is_empty(), "a reconnect never starts the far daemon");
    assert_eq!(link.opens(), 2, "one channel at first, one after the drop");
}

#[test]
fn a_two_hop_workspace_with_no_session_is_idle_on_its_own_machine() {
    let (far, _seam) = two_hop_far();
    let connector = RemoteSessionConnector::new(far.link(), session(), healthy());
    match connector.connect(PROJECT, &SessionRef::Workspace("gamma".into())) {
        Err(ConnectFailure::Idle(info)) => {
            assert_eq!(info.name, "gamma");
            assert_eq!(info.machine, THIRD, "idle on the machine the far project runs it on");
        }
        other => panic!("expected idle, got {:?}", other.err()),
    }
    // Its shell, just asked for, is still starting there.
    assert!(matches!(
        connector.connect(PROJECT, &SessionRef::workspace_shell("gamma")),
        Err(ConnectFailure::Starting(_))
    ));
    // On the far host itself, with no session: idle there, as before.
    match connector.connect(PROJECT, &SessionRef::Workspace("alpha".into())) {
        Err(ConnectFailure::Idle(info)) => assert_eq!(info.machine, HOST),
        other => panic!("expected idle, got {:?}", other.err()),
    }
}

#[test]
fn new_task_hands_the_write_up_to_the_far_orchestrator() {
    use shelbi_app::new_task::{create_linked, CreateRequest, WriteUp};
    let mut far = Far::new();
    let control = far.control_socket();
    far.start("demo/orch", "printf 'orch ready\\n'; exec cat");
    let link = far.link();
    let connector = Arc::new(RemoteSessionConnector::new(link.clone(), session(), healthy()));
    let project = ProjectLink::remote(PROJECT, link.clone(), connector.clone());
    // Watch the far orchestrator as the window's pane does.
    let (_pane, events) = attach(&*connector, &SessionRef::Orchestrator);
    read_until(&events, "orch ready");

    let served = std::thread::spawn(move || serve_one_request(&control));
    let request = CreateRequest {
        command: shelbi_app::CommandKind::AddIssue {
            title: "Warm the cache".into(),
            body: None,
            workflow: None,
            status: None,
            top: false,
        },
        write_up: true,
    };
    let created = create_linked(&project, &request).expect("created on the far daemon");
    assert!(matches!(served.join().unwrap(), ClientMsg::Mutate(_)), "the add went to the far daemon");
    assert_eq!(created.id.as_deref(), Some("42"));
    assert_eq!(created.write_up, WriteUp::Requested, "the far orchestrator is running");
    // The write-up was typed into the far orchestrator, not a local one.
    read_until(&events, "Please write up issue 42");
}
