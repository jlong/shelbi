//! A remote project link's subscriptions end to end over an in-process relay:
//! a `changes` line on the far hub wakes the feed, a far `Changed` note on
//! the far daemon's control socket wakes it too (the link subscribing there
//! as `ClientKind::Remote`), and a far reload arrives as a reload wake. The
//! far `hub.sock` and `control.sock` are test listeners in a temp dir; nothing
//! reaches the real hub.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shelbi_app::changes::ChangeWake;
use shelbi_app::source::remote::{self, ChannelLink, RelayLink};
use shelbi_client::{RelayChannel, RelayServices};
use shelbi_proto::control::{self, ChangeNote, ClientKind, ClientMsg, ServerMsg, CONTROL_PROTOCOL_VERSION};
use shelbi_state::LayoutEvent;

const WAIT: Duration = Duration::from_secs(10);

/// A throwaway far machine's socket directory.
struct Far {
    dir: PathBuf,
}

impl Far {
    fn new(tag: &str) -> Far {
        // Short: sockets live in here (macOS caps socket paths).
        let dir = std::env::temp_dir().join(format!("rl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Far { dir }
    }

    fn hub(&self) -> PathBuf {
        self.dir.join("hub.sock")
    }

    fn control(&self) -> PathBuf {
        self.dir.join("control.sock")
    }

    /// A relay serving this far machine's sockets, and the link over it.
    fn link(&self) -> Arc<dyn RelayLink> {
        let (near, far) = UnixStream::pair().unwrap();
        let far_read = far.try_clone().unwrap();
        let services = RelayServices {
            hub_socket: Some(self.hub()),
            control_socket: Some(self.control()),
            ..RelayServices::default()
        };
        let root = self.dir.join("no-sessions");
        std::thread::spawn(move || {
            let _ = shelbi_client::serve_relay_with(Box::new(far_read), Box::new(far), &root, services);
        });
        let near_read = near.try_clone().unwrap();
        Arc::new(ChannelLink::new(
            "devbox",
            RelayChannel::new(Box::new(near_read), Box::new(near)).unwrap(),
        ))
    }
}

impl Drop for Far {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Accept one subscriber on the far `hub.sock` and return it after reading
/// its subscribe line, which names `project`.
fn accept_subscriber(hub: &UnixListener, project: &str) -> UnixStream {
    let (stream, _) = hub.accept().unwrap();
    let mut line = String::new();
    BufReader::new(stream.try_clone().unwrap()).read_line(&mut line).unwrap();
    assert!(line.contains("\"subscribe\""), "{line}");
    assert!(line.contains(&format!("\"{project}\"")), "{line}");
    stream
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

/// Read one control frame off `stream`.
fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream, buf: &mut Vec<u8>) -> T {
    loop {
        match control::decode::<T>(buf) {
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

/// Serve one control connection as a far daemon of `version` would: answer
/// the hello, take the subscribe, and hand the connection back for pushes.
fn accept_control(control: &UnixListener, version: &str) -> (UnixStream, ClientMsg) {
    let (mut stream, _) = control.accept().unwrap();
    let mut buf = Vec::new();
    match read_frame::<ClientMsg>(&mut stream, &mut buf) {
        ClientMsg::Hello { protocol, .. } => assert_eq!(protocol, CONTROL_PROTOCOL_VERSION),
        other => panic!("expected the hello, got {other:?}"),
    }
    let hello = ServerMsg::Hello {
        protocol: CONTROL_PROTOCOL_VERSION,
        daemon_version: version.to_string(),
    };
    stream.write_all(&control::encode(&hello).unwrap()).unwrap();
    let subscribe = read_frame::<ClientMsg>(&mut stream, &mut buf);
    (stream, subscribe)
}

fn bind(path: &Path) -> UnixListener {
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).unwrap()
}

#[test]
fn a_far_change_line_wakes_a_remote_feed() {
    let far = Far::new("chg");
    let hub = bind(&far.hub());
    // No far daemon: the control half retries quietly, the lines still flow.
    let connected = Arc::new(AtomicBool::new(false));
    let (subscription, wakes) = remote::subscribe_changes(far.link(), "demo", connected.clone());

    let mut sub = accept_subscriber(&hub, "demo");
    until("the change stream to open", || connected.load(Ordering::SeqCst).then_some(()));
    sub.write_all(b"{\"change\":\"board\",\"project\":\"other\"}\n").unwrap();
    sub.write_all(b"{\"change\":\"board\",\"project\":\"demo\"}\n").unwrap();
    sub.write_all(b"{\"change\":\"workspace\",\"project\":\"demo\",\"workspace\":\"alpha\"}\n")
        .unwrap();
    assert_eq!(wakes.recv_timeout(WAIT).unwrap(), ChangeWake::Board);
    assert_eq!(wakes.recv_timeout(WAIT).unwrap(), ChangeWake::Workspace("alpha".into()));

    // The far hub goes away: the link says so, then reconnects on its own.
    drop(sub);
    until("the drop to show", || (!connected.load(Ordering::SeqCst)).then_some(()));
    let mut sub = accept_subscriber(&hub, "demo");
    until("the reconnect", || connected.load(Ordering::SeqCst).then_some(()));
    sub.write_all(b"{\"change\":\"board\",\"project\":\"demo\"}\n").unwrap();
    assert_eq!(wakes.recv_timeout(WAIT).unwrap(), ChangeWake::Board);

    // Dropping the subscription ends it promptly, though its threads sit in
    // reads on relay streams that have no timeout.
    let t0 = Instant::now();
    drop(subscription);
    assert!(t0.elapsed() < Duration::from_secs(3), "drop took {:?}", t0.elapsed());
}

#[test]
fn far_layout_lines_reach_a_remote_layout_subscription() {
    let far = Far::new("lay");
    let hub = bind(&far.hub());
    let (subscription, events) = remote::subscribe_layout(far.link(), "demo");
    let mut sub = accept_subscriber(&hub, "demo");
    sub.write_all(b"{\"change\":\"board\",\"project\":\"demo\"}\n").unwrap();
    sub.write_all(
        b"{\"change\":\"layout\",\"project\":\"demo\",\"event\":{\"layout\":\"orchestrator-restarted\"}}\n",
    )
    .unwrap();
    let event = events.recv_timeout(WAIT).unwrap();
    assert_eq!(event, LayoutEvent::OrchestratorRestarted);
    drop(subscription);
}

#[test]
fn far_changed_notes_and_a_reload_arrive_over_control_as_a_remote_client() {
    let far = Far::new("ctl");
    let _hub = bind(&far.hub());
    let control = bind(&far.control());
    let connected = Arc::new(AtomicBool::new(false));
    let (subscription, wakes) = remote::subscribe_changes(far.link(), "demo", connected);

    // The far daemon runs another version: the link still subscribes, as a
    // remote client, and says nothing of re-exec.
    let (mut daemon, subscribe) = accept_control(&control, "9.9.9-far");
    match subscribe {
        ClientMsg::Subscribe { info: Some(info) } => {
            assert_eq!(info.kind, ClientKind::Remote);
            assert_eq!(info.project.as_deref(), Some("demo"));
        }
        other => panic!("expected a remote subscribe, got {other:?}"),
    }

    let note = |project: &str| {
        ServerMsg::Changed(ChangeNote {
            project: project.into(),
            id: "t-1".into(),
            verb: "move".into(),
            status: "done".into(),
            updated_at: String::new(),
        })
    };
    daemon.write_all(&control::encode(&note("other")).unwrap()).unwrap();
    daemon.write_all(&control::encode(&note("demo")).unwrap()).unwrap();
    assert_eq!(wakes.recv_timeout(WAIT).unwrap(), ChangeWake::Board);

    // A deliberate reload on the far machine: the link is told, and comes
    // back with a fresh subscription.
    let reload = ServerMsg::Reexec {
        reason: "shelbi reload".into(),
    };
    daemon.write_all(&control::encode(&reload).unwrap()).unwrap();
    assert_eq!(
        wakes.recv_timeout(WAIT).unwrap(),
        ChangeWake::Reload {
            reason: "shelbi reload".into()
        }
    );
    let (_again, subscribe) = accept_control(&control, "9.9.9-far");
    assert!(matches!(subscribe, ClientMsg::Subscribe { info: Some(ref i) } if i.kind == ClientKind::Remote));
    drop(subscription);
}

/// Panes never attach: the state-write test needs only the link's control.
struct NoSessions;

impl shelbi_app::session::Connector for NoSessions {
    fn connect(
        &self,
        _project: &str,
        _target: &shelbi_app::session::SessionRef,
    ) -> Result<shelbi_app::session::Connected, shelbi_app::session::ConnectFailure> {
        Err(shelbi_app::session::ConnectFailure::Message("no sessions here".into()))
    }
}

#[test]
fn a_project_state_write_goes_to_the_far_daemon_as_one_control_message() {
    use shelbi_app::source::ProjectLink;
    use shelbi_app::state_write::{self, ColumnOverride, StateWrite};
    use shelbi_proto::control::MutationError;

    let far = Far::new("sw");
    let _hub = bind(&far.hub());
    let control = bind(&far.control());
    let link = ProjectLink::remote("demo", far.link(), Arc::new(NoSessions));

    // Another project's state is never this window's to write.
    let other = state_write::write("other", Some(&link), StateWrite::ClearErrors).unwrap_err();
    assert!(other.contains("`other`"), "{other}");

    let write = StateWrite::SetColumnOverride {
        workflow: "task".into(),
        status: "done".into(),
        value: Some(ColumnOverride::Collapsed),
    };
    let writer = {
        let (link, write) = (link.clone(), write.clone());
        std::thread::spawn(move || state_write::write("demo", Some(&link), write))
    };
    let (mut daemon, msg) = accept_control(&control, env!("CARGO_PKG_VERSION"));
    let ClientMsg::WriteState(req) = msg else {
        panic!("expected the state write, got {msg:?}");
    };
    assert_eq!(req.project, "demo");
    assert_eq!(req.write, write);
    let done = ServerMsg::Done {
        request_id: req.request_id,
        state: None,
        id: None,
    };
    daemon.write_all(&control::encode(&done).unwrap()).unwrap();
    assert_eq!(writer.join().unwrap(), Ok(()));

    // A write the far daemon refuses comes back with the host and its reason.
    let writer = {
        let link = link.clone();
        std::thread::spawn(move || link.write_state(StateWrite::MarkErrorsRead))
    };
    let (mut daemon, msg) = accept_control(&control, env!("CARGO_PKG_VERSION"));
    let ClientMsg::WriteState(req) = msg else {
        panic!("expected the state write, got {msg:?}");
    };
    assert_eq!(req.write, StateWrite::MarkErrorsRead);
    let failed = ServerMsg::Failed {
        request_id: req.request_id,
        error: MutationError::Backend {
            message: "disk full".into(),
        },
    };
    daemon.write_all(&control::encode(&failed).unwrap()).unwrap();
    let err = writer.join().unwrap().unwrap_err();
    assert!(err.starts_with("devbox: ") && err.contains("disk full"), "{err}");
}

#[test]
fn a_new_task_add_goes_to_the_far_daemon_and_answers_its_id() {
    use shelbi_app::exec::Mutation;
    use shelbi_app::source::ProjectLink;
    use shelbi_proto::control::MutationKind;

    let far = Far::new("add");
    let _hub = bind(&far.hub());
    let control = bind(&far.control());
    let link = ProjectLink::remote("demo", far.link(), Arc::new(NoSessions));

    let mutation = Mutation::AddIssue {
        project: "demo".into(),
        title: "Write docs".into(),
        body: Some("the body".into()),
        workflow: None,
        status: Some("todo".into()),
        top: true,
    };
    let adder = {
        let link = link.clone();
        std::thread::spawn(move || link.add_issue(&mutation, &mut |_, _| {}))
    };
    let (mut daemon, msg) = accept_control(&control, env!("CARGO_PKG_VERSION"));
    let ClientMsg::Mutate(req) = msg else {
        panic!("expected the add, got {msg:?}");
    };
    assert_eq!(req.project, "demo");
    let MutationKind::Add(spec) = &req.kind else {
        panic!("expected an add, got {:?}", req.kind);
    };
    assert_eq!(
        (spec.title.as_str(), spec.status.as_str(), spec.body.as_deref(), spec.top),
        ("Write docs", "todo", Some("the body"), true)
    );
    let done = ServerMsg::Done {
        request_id: req.request_id,
        state: None,
        id: Some("write-docs".into()),
    };
    daemon.write_all(&control::encode(&done).unwrap()).unwrap();
    assert_eq!(adder.join().unwrap().unwrap(), Some("write-docs".to_string()));
}
