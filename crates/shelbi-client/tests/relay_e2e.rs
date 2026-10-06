//! End-to-end tests of the remote transport: a [`RelayChannel`] driving real
//! [`shelbi_session::run`] sessions through [`serve_relay`] over an in-process
//! stdio pipe — no SSH needed.
//!
//! Covers the `rt-relay` acceptance criteria: one channel bridging three
//! sessions at once; the same client API (attach/input/resize/snapshot/kill)
//! over the relay as over a local socket; a relay killed mid-stream and
//! replaced, with the client resuming by sequence number; a silent channel
//! detected as unreachable by keepalive; one slow session's client not stalling
//! another on the shared channel; and a session that speaks only the frozen
//! core.
//!
//! `run()` resolves `~/.shelbi/sessions` from `$SHELBI_HOME`, a process-global
//! env var, so the whole file runs under one serial lock.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use shelbi_client::relay::Keepalive;
use shelbi_client::{Connection, RelayChannel, SessionEvent, SessionEvents, Transport};
use shelbi_proto::{
    capability, decode_any, AnyFrame, Exited, Frame, Hello, Output, PROTOCOL_VERSION,
};
use shelbi_session::layout::SessionPaths;
use shelbi_session::RunArgs;

// --- harness ---------------------------------------------------------------

/// A generous deadline for condition-based waits. On a loaded hub several
/// workers (and `shelbi zen probe`) hammer the build tool at once, and the relay
/// adds an extra bridging hop, so threads competing to be scheduled can take far
/// longer than they do idle. Every positive wait polls up to this bound and
/// reports what it actually saw on timeout.
const DEADLINE: Duration = Duration::from_secs(10);

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A test home with one or more real sessions and the relay bridging them.
struct Harness {
    _home: tempfile::TempDir,
    sessions_root: PathBuf,
    sessions: Vec<(SessionPaths, Option<JoinHandle<anyhow::Result<()>>>)>,
}

impl Harness {
    /// Set `$SHELBI_HOME` to a fresh tempdir and prepare an empty session root.
    fn new() -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("SHELBI_HOME", home.path());
        let sessions_root = home.path().join("sessions");
        std::fs::create_dir_all(&sessions_root).unwrap();
        Self {
            sessions_root,
            _home: home,
            sessions: Vec::new(),
        }
    }

    /// Start a real session and wait for its socket to appear.
    fn start_session(&mut self, name: &str, cols: u16, rows: u16, argv: &[&str]) -> SessionPaths {
        let id = shelbi_session::layout::derive_id_now(name);
        let paths = SessionPaths::new(&self.sessions_root, &id);
        let args = RunArgs {
            id,
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            cols,
            rows,
            task: None,
            raw_output_log: false,
            child_argv: argv.iter().map(|s| s.to_string()).collect(),
            // No daemon watchdog in-process: it loops reading the global
            // `$SHELBI_HOME` this harness mutates per test, which would be a
            // data race (see `RunArgs::manage_daemon`).
            manage_daemon: false,
        };
        let handle = std::thread::spawn(move || shelbi_session::run(args));
        // Wait until the listener is actually accepting, not merely until the
        // socket file exists: `bind()` creates the file before `listen()` runs,
        // so a connect in that window is refused (ECONNREFUSED), and the window
        // widens under load. Probing with a real connect (immediately dropped)
        // proves the session is accepting before the relay or any test connects.
        wait_for(DEADLINE, || UnixStream::connect(paths.sock()).ok().map(|_| ()))
            .expect("session socket should accept connections");
        self.sessions.push((paths.clone(), Some(handle)));
        paths
    }

    /// Start the relay server over a fresh pipe and return a channel to it.
    fn start_relay(&mut self) -> RelayChannel {
        self.start_relay_with(Keepalive::default())
    }

    fn start_relay_with(&mut self, ka: Keepalive) -> RelayChannel {
        let (hub, relay) = UnixStream::pair().unwrap();
        let root = self.sessions_root.clone();
        let relay_read = relay.try_clone().unwrap();
        // The relay thread is detached: it exits when the channel's socket
        // closes, and otherwise leaks harmlessly until the test process exits.
        std::thread::spawn(move || {
            let _ = shelbi_client::serve_relay(Box::new(relay_read), Box::new(relay), &root);
        });
        let hub_read = hub.try_clone().unwrap();
        RelayChannel::with_keepalive(Box::new(hub_read), Box::new(hub), ka).expect("relay channel")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for (paths, handle) in &mut self.sessions {
            if let Ok(mut s) = UnixStream::connect(paths.sock()) {
                let _ = s.write_all(
                    &Frame::Kill(shelbi_proto::Kill {
                        signal: Some(libc::SIGKILL),
                    })
                    .encode()
                    .unwrap(),
                );
            }
            if let Some(h) = handle.take() {
                join_within(h, Duration::from_secs(10));
            }
        }
    }
}

fn join_within(handle: JoinHandle<anyhow::Result<()>>, timeout: Duration) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    let _ = rx.recv_timeout(timeout);
}

/// Read events until `pred` returns `Some`, or the deadline passes.
fn recv_until<T>(
    events: &SessionEvents,
    timeout: Duration,
    mut pred: impl FnMut(&SessionEvent) -> Option<T>,
) -> Option<T> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if let Some(ev) = events.try_recv() {
            if let Some(v) = pred(&ev) {
                return Some(v);
            }
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    None
}

/// Accumulate [`SessionEvent::Output`] payloads across events until `needle`
/// appears in the running buffer, or the deadline passes. Returns the sequence
/// number of the `Output` event that completed the match, else the bytes seen so
/// far.
///
/// A single typed token's echo can split across two `Output` frames (the PTY
/// master read, or the relay's re-read of the bridged stream, lands on a byte
/// boundary mid-token), so a per-event `windows()` check races the framing and
/// flakes. Matching against the accumulated stream is boundary-independent.
fn recv_output_contains(
    events: &SessionEvents,
    timeout: Duration,
    needle: &[u8],
) -> Result<u64, Vec<u8>> {
    let mut acc: Vec<u8> = Vec::new();
    let found = recv_until(events, timeout, |ev| match ev {
        SessionEvent::Output { seq, data } => {
            acc.extend_from_slice(data);
            acc.windows(needle.len()).any(|w| w == needle).then_some(*seq)
        }
        _ => None,
    });
    found.ok_or(acc)
}

/// Connect to a session named by its short id through the relay, with the full
/// capability set.
fn connect(channel: &RelayChannel, short_id: &str) -> (Connection, SessionEvents) {
    let stream = channel.open(short_id).expect("open relay stream");
    Connection::connect(Box::new(stream), None, capability::ALL).expect("connect over relay")
}

// --- tests -----------------------------------------------------------------

#[test]
fn relay_lists_and_bridges_three_sessions_at_once() {
    let _g = serial();
    let mut h = Harness::new();
    h.start_session("demo/ws/a", 80, 24, &["/bin/sh", "-c", "printf AAA; exec sleep 30"]);
    h.start_session("demo/ws/b", 80, 24, &["/bin/sh", "-c", "printf BBB; exec sleep 30"]);
    h.start_session("demo/ws/c", 80, 24, &["/bin/sh", "-c", "printf CCC; exec sleep 30"]);
    let channel = h.start_relay();

    // Discovery enumerates all three sessions on the machine.
    let listed = channel.list_sessions().expect("list sessions");
    assert_eq!(listed.len(), 3, "relay should enumerate all three sessions: {listed:?}");
    let by_name = |n: &str| listed.iter().find(|s| s.name == n).cloned();
    for n in ["demo/ws/a", "demo/ws/b", "demo/ws/c"] {
        let s = by_name(n).unwrap_or_else(|| panic!("missing {n}"));
        assert!(s.alive, "{n} should be live");
    }

    // One channel, three concurrent connections, each reaching its own session.
    for (name, marker) in [("demo/ws/a", "AAA"), ("demo/ws/b", "BBB"), ("demo/ws/c", "CCC")] {
        let short = by_name(name).unwrap().short_id;
        let (conn, events) = connect(&channel, &short);
        // Wait for the child's output to be drawn into the emulator before
        // attaching, so the resync replay is guaranteed to carry the marker
        // (a replay taken before the reader thread feeds the first output is
        // legitimately empty).
        wait_for(DEADLINE, || {
            conn.snapshot(None).ok().filter(|s| s.text.contains(marker))
        })
        .unwrap_or_else(|| panic!("{name} output should be drawn into the session"));
        conn.attach(None).unwrap();
        // The resync is a byte stream reconstructing the emulator; the marker the
        // child already printed is painted into it verbatim.
        let replay = recv_until(&events, DEADLINE, |ev| match ev {
            SessionEvent::Resync { replay, .. } => Some(replay.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("{name} should deliver a resync"));
        assert!(
            replay.windows(marker.len()).any(|w| w == marker.as_bytes()),
            "{name} replay should carry {marker}: {replay:?}"
        );
    }
}

#[test]
fn relay_connection_matches_the_local_api() {
    let _g = serial();
    let mut h = Harness::new();
    let _paths = h.start_session(
        "demo/ws/api",
        90,
        30,
        &["/bin/sh", "-c", "printf HELLOINFO; exec sleep 30"],
    );
    let channel = h.start_relay();
    let short = channel.list_sessions().unwrap()[0].short_id.clone();
    let (conn, events) = connect(&channel, &short);

    // Wait for the child's output to be drawn into the emulator before attaching.
    // The reader thread feeds the child's first output into the emulator a short
    // moment after the socket appears (longer under load), so a resync replay
    // taken before that lands is legitimately empty. Poll a snapshot for the
    // marker first so the replay is guaranteed to carry it.
    wait_for(DEADLINE, || {
        conn.snapshot(None).ok().filter(|s| s.text.contains("HELLOINFO"))
    })
    .expect("child output should be drawn into the session");

    // attach → resync replay carrying the child's output.
    conn.attach(None).unwrap();
    let replay = recv_until(&events, DEADLINE, |ev| match ev {
        SessionEvent::Resync { replay, .. } => Some(replay.clone()),
        _ => None,
    })
    .expect("attach should resync");
    assert!(
        replay.windows(9).any(|w| w == b"HELLOINFO"),
        "replay: {replay:?}"
    );

    // input → tty echo streams back as output.
    conn.input(b"echoback").unwrap();
    let echoed = recv_output_contains(&events, DEADLINE, b"echoback");
    assert!(
        echoed.is_ok(),
        "typed input should echo back over the relay; saw: {:?}",
        echoed.map_err(|b| String::from_utf8_lossy(&b).into_owned())
    );

    // resize → info reflects the new size (the active client's viewport).
    conn.resize(100, 40).unwrap();
    let sized = wait_for(DEADLINE, || {
        let info = conn.info().ok()?;
        (info.cols == 100 && info.rows == 40).then_some(())
    });
    assert!(sized.is_some(), "resize should take effect and be visible via info");

    // snapshot → the visible screen text.
    let snap = conn.snapshot(None).expect("snapshot over relay");
    assert!(snap.text.contains("HELLOINFO"), "snapshot text: {:?}", snap.text);

    // kill → the frozen-core exited event is pushed.
    conn.kill(Some(libc::SIGKILL)).unwrap();
    let exited = recv_until(&events, DEADLINE, |ev| {
        matches!(ev, SessionEvent::Exited(_)).then_some(())
    });
    assert!(exited.is_some(), "kill should push an exited event over the relay");
}

#[test]
fn relay_kill_mid_stream_resumes_by_sequence_number() {
    let _g = serial();
    let mut h = Harness::new();
    h.start_session("demo/ws/resume", 80, 24, &["/bin/sh", "-c", "exec sleep 30"]);
    let root = h.sessions_root.clone();

    // Relay #1, built with a handle we can sever to simulate the relay process
    // dying mid-stream.
    let (hub1, relay1) = UnixStream::pair().unwrap();
    let relay1_kill = relay1.try_clone().unwrap();
    let r1_read = relay1.try_clone().unwrap();
    let root1 = root.clone();
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay(Box::new(r1_read), Box::new(relay1), &root1);
    });
    let channel1 =
        RelayChannel::new(Box::new(hub1.try_clone().unwrap()), Box::new(hub1)).unwrap();
    let short = channel1.list_sessions().unwrap()[0].short_id.clone();
    let (conn1, events1) = connect(&channel1, &short);
    conn1.attach(None).unwrap();
    recv_until(&events1, DEADLINE, |ev| {
        matches!(ev, SessionEvent::Resync { .. }).then_some(())
    });
    conn1.input(b"first").unwrap();
    let last_seq = recv_output_contains(&events1, DEADLINE, b"first")
        .expect("should see the first output with a sequence number");

    // Kill the relay mid-stream: sever its socket so the server end EOFs, and
    // drop the client side. The session itself keeps running — the relay held
    // no session state.
    relay1_kill.shutdown(Shutdown::Both).ok();
    drop(conn1);
    drop(channel1);

    // Relay #2: a fresh relay on the same session root, reconnect the same
    // session, resume from the last sequence number.
    let (hub2, relay2) = UnixStream::pair().unwrap();
    let r2_read = relay2.try_clone().unwrap();
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay(Box::new(r2_read), Box::new(relay2), &root);
    });
    let channel2 =
        RelayChannel::new(Box::new(hub2.try_clone().unwrap()), Box::new(hub2)).unwrap();
    let short2 = channel2.list_sessions().unwrap()[0].short_id.clone();
    assert_eq!(short2, short, "the session survived the relay restart");
    let (conn2, events2) = connect(&channel2, &short);
    conn2.attach(Some(last_seq)).unwrap();
    // The resync rebases us at or past where we were — never behind it.
    let resync_seq = recv_until(&events2, DEADLINE, |ev| match ev {
        SessionEvent::Resync { seq, .. } => Some(*seq),
        _ => None,
    })
    .expect("reattach should resync");
    assert!(
        resync_seq >= last_seq,
        "resume must not rewind: resync seq {resync_seq} >= last seq {last_seq}"
    );

    // New output after the reconnect carries strictly higher sequence numbers —
    // no duplication of what we already saw, no loss of the new output.
    conn2.input(b"second").unwrap();
    let next_seq = recv_output_contains(&events2, DEADLINE, b"second")
        .expect("should receive output generated after the reconnect");
    assert!(
        next_seq > last_seq,
        "post-reconnect output must continue the sequence ({next_seq} > {last_seq})"
    );
}

#[test]
fn silent_channel_is_detected_as_unreachable_within_the_deadline() {
    let _g = serial();
    // A peer that accepts the connection but never answers: no relay server on
    // the far end. The keepalive must flip the channel to unreachable.
    let (hub, _dead_peer) = UnixStream::pair().unwrap();
    let hub_read = hub.try_clone().unwrap();
    let ka = Keepalive {
        interval: Duration::from_millis(50),
        deadline: Duration::from_millis(300),
    };
    let channel =
        RelayChannel::with_keepalive(Box::new(hub_read), Box::new(hub), ka).expect("channel");

    assert!(channel.is_reachable(), "a fresh channel starts reachable");
    let became_unreachable = wait_for(DEADLINE, || {
        (!channel.is_reachable()).then_some(())
    });
    assert!(
        became_unreachable.is_some(),
        "a silent channel must be reported unreachable within the deadline"
    );
    // Keep the dead peer alive until here so the writes do not fail early for
    // the wrong reason.
    drop(_dead_peer);
}

#[test]
fn slow_stream_does_not_stall_another_on_the_same_channel() {
    let _g = serial();
    let mut h = Harness::new();
    let flood = ["/bin/sh", "-c", "while :; do printf 'flood-line-of-output\\n'; done"];
    h.start_session("demo/ws/fast", 80, 24, &flood);
    h.start_session("demo/ws/slow", 80, 24, &flood);
    let channel = h.start_relay();

    let listed = channel.list_sessions().unwrap();
    let fast_id = listed.iter().find(|s| s.name == "demo/ws/fast").unwrap().short_id.clone();
    let slow_id = listed.iter().find(|s| s.name == "demo/ws/slow").unwrap().short_id.clone();

    // Drive both streams at the transport level so the slow one's inbound queue
    // actually overflows (a full `Connection` would auto-drain it). Both attach
    // so the sessions flood them; only the fast one is ever read.
    let (mut fast_r, mut fast_w) = Box::new(channel.open(&fast_id).unwrap()).split().unwrap();
    let (slow_r, mut slow_w) = Box::new(channel.open(&slow_id).unwrap()).split().unwrap();

    for w in [&mut fast_w, &mut slow_w] {
        w.write_all(
            &Frame::Hello(Hello {
                protocol_version: PROTOCOL_VERSION,
                colors: None,
                capabilities: capability::ALL.iter().map(|s| s.to_string()).collect(),
            })
            .encode()
            .unwrap(),
        )
        .unwrap();
        w.write_all(&Frame::Attach(shelbi_proto::Attach { since_seq: None }).encode().unwrap())
            .unwrap();
        w.flush().unwrap();
    }

    // The fast reader counts output frames in the background; the slow reader is
    // deliberately never polled, so its bounded queue overflows and is dropped.
    let fast_outputs = Arc::new(AtomicUsize::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    let reader_outputs = fast_outputs.clone();
    let reader_stop = stop.clone();
    let fast_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            while let Ok((frame, consumed)) = decode_any(&buf) {
                buf.drain(..consumed);
                if matches!(frame, AnyFrame::Core(Frame::Output(_))) {
                    reader_outputs.fetch_add(1, Ordering::Relaxed);
                }
            }
            if reader_stop.load(Ordering::Relaxed) {
                return;
            }
            match fast_r.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    });

    // Confirm the fast stream keeps flowing across two checkpoints — it is not
    // stalled by the slow one, whose unread queue overflows concurrently. Poll
    // for each checkpoint rather than sleeping a fixed amount, so a loaded hub
    // that merely slows the throughput doesn't trip the test.
    let t1 = wait_for(DEADLINE, || {
        let n = fast_outputs.load(Ordering::Relaxed);
        (n > 0).then_some(n)
    })
    .expect("the fast stream should be receiving output");
    let t2 = wait_for(DEADLINE, || {
        let n = fast_outputs.load(Ordering::Relaxed);
        (n > t1).then_some(n)
    })
    .expect("the fast stream must keep flowing while the slow stream is stalled");
    assert!(
        t2 > t1,
        "the fast stream must keep flowing while the slow stream is stalled ({t1} -> {t2})"
    );

    // The slow stream was dropped-to-replay: reading it now surfaces a resync
    // (its backlog was discarded and the session re-snapshotted).
    let saw_resync = read_for_resync(slow_r, DEADLINE);
    assert!(saw_resync, "the slow stream should have been dropped to a resync");

    stop.store(true, Ordering::Relaxed);
    drop(fast_w);
    drop(slow_w);
    drop(channel);
    let _ = fast_thread.join();
}

/// Read a relay stream looking for a backpressure `resync` frame, bounded by
/// `timeout`.
fn read_for_resync(mut reader: Box<dyn Read + Send>, timeout: Duration) -> bool {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        while let Ok((frame, consumed)) = decode_any(&buf) {
            buf.drain(..consumed);
            if matches!(frame, AnyFrame::Ext(shelbi_proto::ExtFrame::Resync(_))) {
                return true;
            }
        }
        match reader.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    false
}

#[test]
fn relay_bridges_a_frozen_core_session() {
    let _g = serial();
    let h = Harness::new();

    // A mock session speaking only the frozen core: it announces no additive
    // capabilities, answers attach with an output frame, and answers kill with
    // the exited event. The relay must forward its frames unchanged.
    let id = "frozencore00";
    let dir = h.sessions_root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let sock = dir.join("sock");
    let listener = UnixListener::bind(&sock).unwrap();
    let mock = std::thread::spawn(move || run_frozen_core_session(listener));

    // Stand the relay up over a pipe (own the pieces here rather than via the
    // harness, which keys off real sessions).
    let (hub, relay) = UnixStream::pair().unwrap();
    let root = h.sessions_root.clone();
    let relay_kill = relay.try_clone().unwrap();
    let relay_read = relay.try_clone().unwrap();
    std::thread::spawn(move || {
        let _ = shelbi_client::serve_relay(Box::new(relay_read), Box::new(relay), &root);
    });
    let hub_read = hub.try_clone().unwrap();
    let channel = RelayChannel::new(Box::new(hub_read), Box::new(hub)).unwrap();

    let stream = channel.open(id).expect("open frozen-core stream");
    let (conn, events) = Connection::connect(Box::new(stream), None, capability::ALL).unwrap();
    assert_eq!(conn.session_protocol_version(), PROTOCOL_VERSION);
    assert!(
        conn.capabilities().is_empty(),
        "a frozen-core session announces no additive capabilities"
    );

    conn.attach(None).unwrap();
    let got = recv_output_contains(&events, DEADLINE, b"COREOUT");
    assert!(
        got.is_ok(),
        "core attach output should arrive through the relay; saw: {:?}",
        got.map_err(|b| String::from_utf8_lossy(&b).into_owned())
    );

    conn.kill(None).unwrap();
    let exited = recv_until(&events, DEADLINE, |ev| {
        matches!(ev, SessionEvent::Exited(_)).then_some(())
    });
    assert!(exited.is_some(), "core exited event should arrive through the relay");

    drop(conn);
    drop(channel);
    // Sever the relay so its server thread exits rather than leaking, then join
    // the mock (which returned on the kill above).
    relay_kill.shutdown(Shutdown::Both).ok();
    let _ = mock.join();
}

/// A minimal session that speaks only the frozen core: hello (no caps), an
/// output on attach, and an exited event on kill.
fn run_frozen_core_session(listener: UnixListener) {
    let (mut stream, _) = listener.accept().unwrap();
    stream.set_read_timeout(Some(DEADLINE)).ok();
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let mut sent_hello = false;
    loop {
        while let Ok((frame, consumed)) = Frame::decode(&buf) {
            buf.drain(..consumed);
            match frame {
                Frame::Hello(_) => {
                    let _ = stream.write_all(
                        &Frame::Hello(Hello {
                            protocol_version: PROTOCOL_VERSION,
                            colors: None,
                            capabilities: vec![],
                        })
                        .encode()
                        .unwrap(),
                    );
                    let _ = stream.flush();
                    sent_hello = true;
                }
                Frame::Attach(_) if sent_hello => {
                    let _ = stream.write_all(
                        &Frame::Output(Output {
                            seq: 1,
                            data: b"COREOUT\r\n".to_vec(),
                        })
                        .encode()
                        .unwrap(),
                    );
                    let _ = stream.flush();
                }
                Frame::Kill(_) => {
                    let _ = stream.write_all(
                        &Frame::Exited(Exited {
                            code: Some(0),
                            signal: None,
                            reason: Some("killed".into()),
                        })
                        .encode()
                        .unwrap(),
                    );
                    let _ = stream.flush();
                    return;
                }
                _ => {}
            }
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}
