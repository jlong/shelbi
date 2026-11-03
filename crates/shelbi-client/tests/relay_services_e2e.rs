//! End-to-end tests of the relay protocol v2 services: a [`RelayChannel`]
//! driving [`serve_relay_with`] over an in-process stdio pipe, with the far
//! daemon's `control.sock` and `hub.sock` played by test listeners. No SSH and
//! no real daemon (and nothing here reads `$SHELBI_HOME`).
//!
//! Covers the `rw-relay-services` acceptance criteria: an end-to-end round
//! trip for each service (`query`, `control`, `changes`); a v1 relay detected
//! from its hello and never sent `OpenService`; denylisted control messages
//! (and `QuitProject` for another project) refused by the relay; and the
//! `changes` service never forwarding client bytes to `hub.sock`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelbi_client::relay::services::subscribe_line;
use shelbi_client::relay::{QueryHandler, DATA_CHUNK};
use shelbi_client::{ClientError, RelayChannel, RelayServices, RelayStream, Transport};
use shelbi_proto::control::{
    self, ClientMsg, DetachSelector, MutationError, ServerMsg, CONTROL_PROTOCOL_VERSION,
};
use shelbi_proto::{ProtoError, RelayFrame, RelayService};

/// Generous bound for a reply on a loaded hub.
const DEADLINE: Duration = Duration::from_secs(10);

/// Start `serve_relay_with` over a fresh pipe (no sessions) and return a
/// channel to it.
fn start_relay(dir: &Path, services: RelayServices) -> RelayChannel {
    let (hub, relay) = UnixStream::pair().unwrap();
    let root = dir.join("sessions");
    std::fs::create_dir_all(&root).unwrap();
    let relay_read = relay.try_clone().unwrap();
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay_with(Box::new(relay_read), Box::new(relay), &root, services);
    });
    let hub_read = hub.try_clone().unwrap();
    RelayChannel::new(Box::new(hub_read), Box::new(hub)).expect("relay channel")
}

/// A relay stream split into its halves, with a bounded read.
struct Halves {
    read: Box<dyn shelbi_client::transport::ReadTimeout + Send>,
    write: Box<dyn Write + Send>,
    _shutdown: shelbi_client::transport::ShutdownHandle,
}

fn split(stream: RelayStream) -> Halves {
    let (read, write, shutdown) = Box::new(stream).split().unwrap();
    Halves {
        read,
        write,
        _shutdown: shutdown,
    }
}

// --- query -----------------------------------------------------------------

fn echo_handler() -> QueryHandler {
    Arc::new(|req: &[u8], reply: &mut dyn FnMut(Vec<u8>)| {
        let mut answer = b"answer:".to_vec();
        answer.extend_from_slice(req);
        reply(answer)
    })
}

#[test]
fn query_round_trips_requests_through_the_injected_handler() {
    let dir = tempfile::tempdir().unwrap();
    let relay = start_relay(
        dir.path(),
        RelayServices {
            query: Some(echo_handler()),
            ..RelayServices::default()
        },
    );
    assert!(relay.supports_services().unwrap());

    let mut query = relay.open_query().expect("open query");
    assert_eq!(query.call(b"{\"sidebar\":1}").unwrap(), b"answer:{\"sidebar\":1}");
    assert_eq!(query.call(b"second").unwrap(), b"answer:second");

    // An answer bigger than one Data frame arrives whole, in order.
    let big: Vec<u8> = (0..(DATA_CHUNK * 2 + 123)).map(|i| (i % 251) as u8).collect();
    let answer = query.call(&big).unwrap();
    assert_eq!(&answer[..7], b"answer:");
    assert_eq!(&answer[7..], &big[..]);

    // A second stream works side by side with the first.
    let mut other = relay.open_query().expect("open second query");
    assert_eq!(other.call(b"x").unwrap(), b"answer:x");
    assert_eq!(query.call(b"y").unwrap(), b"answer:y");
}

#[test]
fn a_query_answered_twice_arrives_as_two_replies_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let handler: QueryHandler = Arc::new(|req: &[u8], reply: &mut dyn FnMut(Vec<u8>)| {
        let mut early = b"early:".to_vec();
        early.extend_from_slice(req);
        reply(early);
        let mut full = b"full:".to_vec();
        full.extend_from_slice(req);
        reply(full)
    });
    let relay = start_relay(
        dir.path(),
        RelayServices {
            query: Some(handler),
            ..RelayServices::default()
        },
    );
    let mut query = relay.open_query().unwrap();
    query.send(b"a").unwrap();
    assert_eq!(query.recv().unwrap(), b"early:a");
    assert_eq!(query.recv().unwrap(), b"full:a");
    // The next request starts clean.
    query.send(b"b").unwrap();
    assert_eq!(query.recv().unwrap(), b"early:b");
    assert_eq!(query.recv().unwrap(), b"full:b");
}

#[test]
fn a_panicking_query_handler_closes_the_stream_not_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    let handler: QueryHandler = Arc::new(|req: &[u8], reply: &mut dyn FnMut(Vec<u8>)| {
        if req == b"boom" {
            panic!("reader blew up");
        }
        reply(req.to_vec())
    });
    let relay = start_relay(
        dir.path(),
        RelayServices {
            query: Some(handler),
            ..RelayServices::default()
        },
    );
    let mut query = relay.open_query().unwrap();
    assert!(matches!(query.call(b"boom"), Err(ClientError::UnexpectedEof)));
    assert!(relay.is_reachable());
    assert_eq!(relay.open_query().unwrap().call(b"ok").unwrap(), b"ok");
}

#[test]
fn a_service_the_relay_lacks_fails_the_open_and_keeps_the_channel() {
    let dir = tempfile::tempdir().unwrap();
    // No services configured, and a control socket nobody listens on.
    let relay = start_relay(
        dir.path(),
        RelayServices {
            control_socket: Some(dir.path().join("control.sock")),
            ..RelayServices::default()
        },
    );
    let err = relay.open_query().err().expect("no query service");
    assert!(matches!(&err, ClientError::Relay(m) if m.contains("query")), "{err:?}");
    let err = relay
        .open_service(RelayService::Control { project: "p".into() })
        .err()
        .expect("daemon not running");
    assert!(matches!(&err, ClientError::Relay(m) if m.contains("isn't running")), "{err:?}");
    let err = relay
        .open_service(RelayService::Changes { project: "p".into() })
        .err()
        .expect("no hub socket");
    assert!(matches!(&err, ClientError::Relay(m) if m.contains("changes")), "{err:?}");
    let err = relay.open_service(RelayService::Unknown).err().expect("unknown service");
    assert!(matches!(&err, ClientError::Relay(m) if m.contains("unknown")), "{err:?}");
    // The channel is still up.
    assert_eq!(relay.list_sessions().unwrap(), vec![]);
}

// --- control ---------------------------------------------------------------

/// A fake far daemon on `control.sock`: records every message it receives and
/// answers the ones the test sends through.
fn fake_daemon(path: &Path) -> Receiver<ClientMsg> {
    let listener = UnixListener::bind(path).unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let (mut conn, _) = listener.accept().unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match control::decode::<ClientMsg>(&buf) {
                Ok((msg, n)) => {
                    buf.drain(..n);
                    let reply = match &msg {
                        ClientMsg::Hello { .. } => Some(ServerMsg::Hello {
                            protocol: CONTROL_PROTOCOL_VERSION,
                            daemon_version: "9.9.9".into(),
                        }),
                        ClientMsg::QuitProject { request_id, .. } => Some(ServerMsg::Done {
                            request_id: *request_id,
                            state: None,
                            id: None,
                        }),
                        ClientMsg::ListClients { request_id } => Some(ServerMsg::ClientList {
                            request_id: *request_id,
                            clients: vec![],
                        }),
                        _ => None,
                    };
                    let _ = tx.send(msg);
                    if let Some(reply) = reply {
                        conn.write_all(&control::encode(&reply).unwrap()).unwrap();
                    }
                    continue;
                }
                Err(ProtoError::Incomplete { .. }) => {}
                Err(e) => panic!("fake daemon got a bad frame: {e}"),
            }
            match conn.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    });
    rx
}

fn read_server_msg(halves: &mut Halves, buf: &mut Vec<u8>) -> ServerMsg {
    let mut chunk = [0u8; 4096];
    loop {
        match control::decode::<ServerMsg>(buf) {
            Ok((msg, n)) => {
                buf.drain(..n);
                return msg;
            }
            Err(ProtoError::Incomplete { .. }) => {}
            Err(e) => panic!("bad control frame from the relay: {e}"),
        }
        let n = halves.read.read(&mut chunk).unwrap();
        assert!(n > 0, "control stream closed early");
        buf.extend_from_slice(&chunk[..n]);
    }
}

fn send(halves: &mut Halves, msg: &ClientMsg) {
    halves.write.write_all(&control::encode(msg).unwrap()).unwrap();
}

fn refusal(msg: ServerMsg) -> (u64, String) {
    match msg {
        ServerMsg::Failed {
            request_id,
            error: MutationError::Backend { message },
        } => (request_id, message),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn control_bridges_the_daemon_and_refuses_the_denylist() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("control.sock");
    let daemon = fake_daemon(&sock);
    let relay = start_relay(
        dir.path(),
        RelayServices {
            control_socket: Some(sock),
            ..RelayServices::default()
        },
    );
    let stream = relay
        .open_service(RelayService::Control {
            project: "mine".into(),
        })
        .expect("open control");
    let mut halves = split(stream);
    let mut buf = Vec::new();

    // The handshake goes through both ways.
    send(
        &mut halves,
        &ClientMsg::Hello {
            protocol: CONTROL_PROTOCOL_VERSION,
            client_version: "9.9.9".into(),
        },
    );
    assert!(matches!(read_server_msg(&mut halves, &mut buf), ServerMsg::Hello { .. }));

    // Each denylisted message is answered by the relay itself.
    send(&mut halves, &ClientMsg::QuitShelbi { request_id: 1 });
    let (id, why) = refusal(read_server_msg(&mut halves, &mut buf));
    assert_eq!(id, 1);
    assert!(why.contains("Quitting Shelbi"), "{why}");

    send(&mut halves, &ClientMsg::ReloadClients { request_id: 2 });
    assert_eq!(refusal(read_server_msg(&mut halves, &mut buf)).0, 2);

    send(
        &mut halves,
        &ClientMsg::DetachClients {
            request_id: 3,
            selector: DetachSelector::All,
            reason: "bye".into(),
        },
    );
    assert_eq!(refusal(read_server_msg(&mut halves, &mut buf)).0, 3);

    send(
        &mut halves,
        &ClientMsg::QuitProject {
            request_id: 4,
            project: "theirs".into(),
        },
    );
    let (id, why) = refusal(read_server_msg(&mut halves, &mut buf));
    assert_eq!(id, 4);
    assert!(why.contains("theirs"), "{why}");

    // The window's own project, and harmless requests, reach the daemon.
    send(
        &mut halves,
        &ClientMsg::QuitProject {
            request_id: 5,
            project: "mine".into(),
        },
    );
    assert_eq!(
        read_server_msg(&mut halves, &mut buf),
        ServerMsg::Done {
            request_id: 5,
            state: None,
            id: None,
        }
    );
    send(&mut halves, &ClientMsg::ListClients { request_id: 6 });
    assert!(matches!(
        read_server_msg(&mut halves, &mut buf),
        ServerMsg::ClientList { request_id: 6, .. }
    ));

    // The daemon saw exactly what was allowed through, in order.
    let seen: Vec<ClientMsg> = (0..3)
        .map(|_| daemon.recv_timeout(DEADLINE).expect("daemon message"))
        .collect();
    assert!(matches!(seen[0], ClientMsg::Hello { .. }));
    assert_eq!(
        seen[1],
        ClientMsg::QuitProject {
            request_id: 5,
            project: "mine".into()
        }
    );
    assert_eq!(seen[2], ClientMsg::ListClients { request_id: 6 });
    assert!(daemon.recv_timeout(Duration::from_millis(200)).is_err(), "nothing else reached it");
}

// --- changes ---------------------------------------------------------------

/// A fake far `hub.sock`: records every byte a connection sends until it
/// closes, and after the first line writes `lines` back.
fn fake_hub(path: &Path, lines: &'static [&'static str]) -> Receiver<Vec<u8>> {
    let listener = UnixListener::bind(path).unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        let mut write = conn.try_clone().unwrap();
        let mut reader = BufReader::new(conn);
        let mut received = Vec::new();
        let mut first = String::new();
        reader.read_line(&mut first).unwrap();
        received.extend_from_slice(first.as_bytes());
        for line in lines {
            write.write_all(line.as_bytes()).unwrap();
        }
        // Everything after the subscribe line, until the relay hangs up.
        let _ = reader.read_to_end(&mut received);
        let _ = tx.send(received);
    });
    rx
}

#[test]
fn changes_forwards_hub_lines_and_never_client_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let hub_path: PathBuf = dir.path().join("hub.sock");
    let hub = fake_hub(
        &hub_path,
        &[
            "{\"project\":\"mine\",\"kind\":\"board\"}\n",
            "{\"project\":\"mine\",\"kind\":\"workspace\",\"workspace\":\"alpha\"}\n",
        ],
    );
    let relay = start_relay(
        dir.path(),
        RelayServices {
            hub_socket: Some(hub_path),
            ..RelayServices::default()
        },
    );
    let stream = relay
        .open_service(RelayService::Changes {
            project: "mine".into(),
        })
        .expect("open changes");
    let mut halves = split(stream);

    // The hub's lines come down the stream.
    let mut got = Vec::new();
    let mut chunk = [0u8; 1024];
    while got.iter().filter(|b| **b == b'\n').count() < 2 {
        let n = halves.read.read(&mut chunk).unwrap();
        assert!(n > 0, "changes stream closed early");
        got.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8(got).unwrap();
    assert!(text.contains("\"board\""), "{text}");
    assert!(text.contains("\"alpha\""), "{text}");

    // Try to inject a worker event and another subscribe through the stream.
    halves
        .write
        .write_all(b"{\"verb\":\"event\",\"project\":\"mine\",\"line\":\"workspace=x note=forged\"}\n")
        .unwrap();
    halves
        .write
        .write_all(b"{\"verb\":\"subscribe\",\"project\":\"theirs\"}\n")
        .unwrap();

    // Close the stream: the relay hangs up on hub.sock, which then reports
    // every byte it ever got. The close is sent after the writes on the same
    // ordered channel, so the relay has handled them by then.
    drop(halves);
    let received = hub.recv_timeout(DEADLINE).expect("hub connection closed");
    assert_eq!(
        String::from_utf8(received).unwrap(),
        subscribe_line("mine"),
        "hub.sock must only ever see the relay's own subscribe"
    );
    assert!(relay.is_reachable());
}

// --- old relays ------------------------------------------------------------

/// A fake v1 relay: announces version 1, records every frame it gets, and
/// answers `Open` with `OpenError` (a marker the test uses to know everything
/// sent before it has arrived).
fn fake_v1_relay() -> (RelayChannel, Arc<Mutex<Vec<RelayFrame>>>, Receiver<()>) {
    let (hub, relay) = UnixStream::pair().unwrap();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let (marker_tx, marker_rx) = channel();
    let seen = frames.clone();
    std::thread::spawn(move || {
        let mut relay = relay;
        relay
            .write_all(&RelayFrame::Hello { version: 1 }.encode().unwrap())
            .unwrap();
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            while let Ok((frame, n)) = RelayFrame::decode(&buf) {
                buf.drain(..n);
                if let RelayFrame::Open { stream, .. } = &frame {
                    let reply = RelayFrame::OpenError {
                        stream: *stream,
                        error: "no such session".into(),
                    };
                    relay.write_all(&reply.encode().unwrap()).unwrap();
                    let _ = marker_tx.send(());
                }
                seen.lock().unwrap().push(frame);
            }
            match relay.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    });
    let hub_read = hub.try_clone().unwrap();
    let channel = RelayChannel::new(Box::new(hub_read), Box::new(hub)).unwrap();
    (channel, frames, marker_rx)
}

#[test]
fn a_v1_relay_is_detected_from_its_hello_and_never_sent_open_service() {
    let (relay, frames, marker) = fake_v1_relay();
    assert_eq!(relay.peer_version(DEADLINE).unwrap(), 1);
    assert!(!relay.supports_services().unwrap());

    for service in [
        RelayService::Query,
        RelayService::Control { project: "p".into() },
        RelayService::Changes { project: "p".into() },
    ] {
        let err = relay.open_service(service).err().expect("refused locally");
        assert!(
            matches!(err, ClientError::RelayServicesUnsupported { version: 1 }),
            "{err:?}"
        );
    }
    assert!(relay.open_query().is_err());

    // Sessions still work on it, and the marker proves the relay has read
    // everything the client sent before.
    assert!(matches!(relay.open("nope"), Err(ClientError::Relay(_))));
    marker.recv_timeout(DEADLINE).expect("marker reached the fake relay");
    let frames = frames.lock().unwrap();
    assert!(
        !frames.iter().any(|f| matches!(f, RelayFrame::OpenService { .. })),
        "a v1 relay was sent OpenService: {frames:?}"
    );
    assert!(relay.is_reachable(), "the v1 relay kept the channel up");
}

// --- settings writes and put -----------------------------------------------

#[test]
fn a_settings_write_for_another_project_is_refused_by_the_relay() {
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("control.sock");
    let _daemon = fake_daemon(&sock);
    let relay = start_relay(
        dir.path(),
        RelayServices {
            control_socket: Some(sock),
            ..RelayServices::default()
        },
    );
    let stream = relay
        .open_service(RelayService::Control {
            project: "mine".into(),
        })
        .expect("open control");
    let mut halves = split(stream);
    let mut buf = Vec::new();
    send(
        &mut halves,
        &ClientMsg::SaveConfig(control::SaveConfigRequest {
            request_id: 7,
            project: "theirs".into(),
            surface: "workflows".into(),
            plan: serde_json::json!({}),
        }),
    );
    let (id, why) = refusal(read_server_msg(&mut halves, &mut buf));
    assert_eq!(id, 7);
    assert!(why.contains("theirs"), "{why}");
    send(
        &mut halves,
        &ClientMsg::AppRequest(control::AppRequest {
            request_id: 8,
            project: "theirs".into(),
            body: serde_json::json!({}),
        }),
    );
    assert_eq!(refusal(read_server_msg(&mut halves, &mut buf)).0, 8);
}

#[test]
fn put_saves_a_dropped_file_far_side_and_answers_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let drops = dir.path().join("drops");
    let relay = start_relay(
        dir.path(),
        RelayServices {
            put_dir: Some(drops.clone()),
            ..RelayServices::default()
        },
    );
    // Bigger than one relay data chunk, so it crosses in pieces.
    let bytes: Vec<u8> = (0..DATA_CHUNK + 4096).map(|i| (i % 251) as u8).collect();
    let stream = relay
        .open_service(RelayService::Put {
            file: "../../Screen Shot.png".into(),
        })
        .expect("open put");
    let far = PathBuf::from(shelbi_client::put_file(stream, &bytes).unwrap());
    assert!(far.starts_with(&drops), "{}", far.display());
    assert_eq!(far.file_name().unwrap(), "Screen Shot.png", "only the base name is kept");
    assert_eq!(std::fs::read(&far).unwrap(), bytes);

    // Two drops of one name never collide.
    let again = relay.open_service(RelayService::Put { file: "Screen Shot.png".into() }).unwrap();
    let second = PathBuf::from(shelbi_client::put_file(again, b"two").unwrap());
    assert_ne!(second, far);
    assert_eq!(std::fs::read(&far).unwrap(), bytes, "the first stays");

    // A relay with nowhere to put files says so at open.
    let bare = start_relay(dir.path(), RelayServices::default());
    let err = bare
        .open_service(RelayService::Put { file: "x".into() })
        .err()
        .expect("no put service");
    assert!(matches!(&err, ClientError::Relay(m) if m.contains("put")), "{err:?}");
}

#[test]
fn put_file_names_keep_only_a_plain_base_name() {
    use shelbi_client::relay::services::put_file_name;
    assert_eq!(put_file_name("/Users/me/Desktop/shot.png"), "shot.png");
    assert_eq!(put_file_name("..\\..\\evil.txt"), "evil.txt");
    assert_eq!(put_file_name(".."), "drop");
    assert_eq!(put_file_name(".hidden"), "hidden");
    assert_eq!(put_file_name("a\nb"), "ab");
}
