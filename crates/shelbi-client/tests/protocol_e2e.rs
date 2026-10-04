//! End-to-end tests of the full session protocol: `shelbi-client`'s
//! [`Connection`] driving a real [`shelbi_session::run`] process over a Unix
//! socket, with a genuine PTY and a `/bin/sh` child.
//!
//! These cover the `rt-protocol-client` acceptance criteria: every request and
//! event end to end, sequence-numbered output with ordered `resized`, a slow
//! client dropped to a fresh snapshot without stalling the PTY or other clients,
//! non-interleaved concurrent input, the most-recently-active client's size
//! winning (debounced), capability fallback/gating, keepalive, and discovery
//! reaping dead session directories.
//!
//! `run()` resolves `~/.shelbi/sessions` from `$SHELBI_HOME`, a process-global
//! env var, so the whole file runs under one serial lock.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use shelbi_client::{Connection, SessionEvent};
use shelbi_proto::{
    capability, decode_any, AnyFrame, ExtFrame, Frame, Hello, PROTOCOL_VERSION,
};
use shelbi_session::{layout::SessionPaths, RunArgs};

// --- harness ---------------------------------------------------------------

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

/// A running in-process session, with its on-disk paths.
struct Session {
    paths: SessionPaths,
    handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    _home: tempfile::TempDir,
}

impl Session {
    fn start(name: &str, cols: u16, rows: u16, child_argv: &[&str]) -> Self {
        let home = tempfile::tempdir().expect("tempdir");
        std::env::set_var("SHELBI_HOME", home.path());
        let id = shelbi_session::layout::derive_id_now(name);
        let paths = SessionPaths::new(&home.path().join("sessions"), &id);
        let args = RunArgs {
            id: id.clone(),
            name: name.to_string(),
            cwd: std::env::temp_dir(),
            cols,
            rows,
            task: None,
            raw_output_log: false,
            child_argv: child_argv.iter().map(|s| s.to_string()).collect(),
        };
        let handle = std::thread::spawn(move || shelbi_session::run(args));
        wait_for(Duration::from_secs(5), || paths.sock().exists().then_some(()))
            .expect("session socket should appear");
        Self {
            paths,
            handle: Some(handle),
            _home: home,
        }
    }

    fn sock(&self) -> std::path::PathBuf {
        self.paths.sock()
    }

    /// A connected client that announces every capability.
    fn client(&self) -> (Connection, shelbi_client::SessionEvents) {
        Connection::open(&self.sock(), None, capability::ALL).expect("connect")
    }

    fn kill_and_join(&mut self) {
        if let Ok(mut s) = UnixStream::connect(self.sock()) {
            let _ = s.write_all(
                &Frame::Kill(shelbi_proto::Kill {
                    signal: Some(libc::SIGKILL),
                })
                .encode()
                .unwrap(),
            );
        }
        if let Some(h) = self.handle.take() {
            join_within(h, Duration::from_secs(10));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(h) = self.handle.take() {
            if let Ok(mut s) = UnixStream::connect(self.sock()) {
                let _ = s.write_all(
                    &Frame::Kill(shelbi_proto::Kill {
                        signal: Some(libc::SIGKILL),
                    })
                    .encode()
                    .unwrap(),
                );
            }
            join_within(h, Duration::from_secs(10));
        }
    }
}

fn join_within(handle: std::thread::JoinHandle<anyhow::Result<()>>, timeout: Duration) {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    let _ = rx.recv_timeout(timeout);
}

/// Read events until `pred` returns `Some`, or the deadline passes.
fn recv_until<T>(
    events: &shelbi_client::SessionEvents,
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

/// Read one frame of either kind from a raw socket, with a deadline.
fn read_any(stream: &mut UnixStream, buf: &mut Vec<u8>) -> Option<AnyFrame> {
    let mut chunk = [0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match decode_any(buf) {
            Ok((frame, consumed)) => {
                buf.drain(..consumed);
                return Some(frame);
            }
            Err(shelbi_proto::ProtoError::Incomplete { .. }) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

// --- tests -----------------------------------------------------------------

#[test]
fn handshake_announces_the_full_capability_set() {
    let _g = serial();
    let mut sess = Session::start("demo/ws/caps", 80, 24, &["/bin/sh", "-c", "exec sleep 30"]);
    let (conn, _events) = sess.client();
    assert_eq!(conn.session_protocol_version(), PROTOCOL_VERSION);
    for cap in capability::ALL {
        assert!(conn.supports(cap), "session should announce `{cap}`");
    }
    assert!(!conn.supports("no-such-capability"));
    sess.kill_and_join();
}

#[test]
fn attach_replays_a_snapshot_then_streams_live_output() {
    let _g = serial();
    // The child prints a marker, then idles; tty echo returns anything we type.
    let mut sess = Session::start(
        "demo/ws/attach",
        80,
        24,
        &["/bin/sh", "-c", "printf READYMARK; exec sleep 30"],
    );
    let (conn, events) = sess.client();
    conn.attach(None).unwrap();

    // The attach replay is a byte stream reconstructing the emulator; the cells
    // the child already drew are painted into it verbatim.
    let replay = recv_until(&events, Duration::from_secs(5), |ev| match ev {
        SessionEvent::Resync { replay, .. } => Some(replay.clone()),
        _ => None,
    })
    .expect("attach should deliver a resync replay first");
    assert!(
        replay.windows(9).any(|w| w == b"READYMARK"),
        "replay should carry the child output: {replay:?}"
    );

    // Live output: typed bytes are echoed by the tty and streamed as Output.
    conn.input(b"echoback").unwrap();
    let got = recv_until(&events, Duration::from_secs(5), |ev| match ev {
        SessionEvent::Output { data, .. } if data.windows(8).any(|w| w == b"echoback") => Some(()),
        _ => None,
    });
    assert!(got.is_some(), "typed input should stream back as live output");
    sess.kill_and_join();
}

#[test]
fn output_is_sequenced_and_resized_arrives_in_order() {
    let _g = serial();
    let mut sess = Session::start(
        "demo/ws/seq",
        80,
        24,
        &["/bin/sh", "-c", "exec sleep 30"],
    );
    let (conn, events) = sess.client();
    conn.attach(None).unwrap();
    // Drain the initial resync.
    recv_until(&events, Duration::from_secs(3), |ev| {
        matches!(ev, SessionEvent::Resync { .. }).then_some(())
    });

    // Generate some output, then resize, then generate more.
    conn.input(b"aaa").unwrap();
    std::thread::sleep(Duration::from_millis(50));
    conn.resize(100, 40).unwrap();
    // Wait for the in-band resized marker.
    let resized_seq = recv_until(&events, Duration::from_secs(3), |ev| match ev {
        SessionEvent::Resized { seq, cols, rows } if *cols == 100 && *rows == 40 => Some(*seq),
        _ => None,
    })
    .expect("an in-band resized marker should arrive in the output stream");
    conn.input(b"bbb").unwrap();
    let later_output = recv_until(&events, Duration::from_secs(3), |ev| match ev {
        SessionEvent::Output { seq, data } if data.windows(3).any(|w| w == b"bbb") => Some(*seq),
        _ => None,
    })
    .expect("output after the resize should arrive");

    assert!(
        later_output > resized_seq,
        "output after a resize must carry a higher sequence number ({later_output} > {resized_seq})"
    );
    sess.kill_and_join();
}

#[test]
fn slow_client_is_dropped_to_a_resync_without_stalling_others() {
    let _g = serial();
    // A child that floods output so a non-reading client overflows quickly.
    let mut sess = Session::start(
        "demo/ws/slow",
        80,
        24,
        &["/bin/sh", "-c", "while :; do printf 'flood-line-of-output\\n'; done"],
    );

    // A normal, draining client — must keep receiving despite the slow one.
    let (fast, fast_events) = sess.client();
    fast.attach(None).unwrap();

    // A slow raw client: handshake + attach, then never read.
    let mut slow = UnixStream::connect(sess.sock()).unwrap();
    slow.write_all(
        &Frame::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            colors: None,
            capabilities: capability::ALL.iter().map(|s| s.to_string()).collect(),
        })
        .encode()
        .unwrap(),
    )
    .unwrap();
    slow.write_all(&Frame::Attach(shelbi_proto::Attach { since_seq: None }).encode().unwrap())
        .unwrap();
    slow.flush().unwrap();

    // The fast client keeps getting output (the PTY reader is not stalled).
    let fast_ok = recv_until(&fast_events, Duration::from_secs(10), |ev| {
        matches!(ev, SessionEvent::Output { data, .. } if !data.is_empty()).then_some(())
    });
    assert!(fast_ok.is_some(), "the draining client must keep receiving output");

    // The slow client, once it reads, finds a resync somewhere in its stream:
    // its queued output was dropped and the screen refreshed.
    slow.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut buf = Vec::new();
    let mut saw_resync = false;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        match read_any(&mut slow, &mut buf) {
            Some(AnyFrame::Ext(ExtFrame::Resync(_))) => {
                saw_resync = true;
                break;
            }
            Some(_) => continue,
            None => break,
        }
    }
    assert!(saw_resync, "a lagging client should be dropped to a resync snapshot");

    // The fast client is still alive and receiving after all that.
    let still_ok = recv_until(&fast_events, Duration::from_secs(5), |ev| {
        matches!(ev, SessionEvent::Output { .. }).then_some(())
    });
    assert!(still_ok.is_some(), "the fast client must survive the slow client's drop");

    sess.kill_and_join();
}

#[test]
fn concurrent_input_from_two_clients_is_never_interleaved() {
    let _g = serial();
    // Raw-mode cat echoes exactly what it reads, with no tty cooking, so the
    // observer sees precisely the bytes each client wrote.
    let mut sess = Session::start(
        "demo/ws/arb",
        200,
        50,
        &["/bin/sh", "-c", "stty raw -echo 2>/dev/null; exec cat"],
    );

    let (observer, obs_events) = sess.client();
    observer.attach(None).unwrap();
    recv_until(&obs_events, Duration::from_secs(2), |ev| {
        matches!(ev, SessionEvent::Resync { .. }).then_some(())
    });

    const REPS: usize = 40;
    // Two clients send distinct 32-byte framed blocks concurrently.
    let (a, _ae) = sess.client();
    let (b, _be) = sess.client();
    let make = |c: u8| {
        let mut v = vec![b'<'];
        v.extend(std::iter::repeat_n(c, 30));
        v.push(b'>');
        v
    };
    let ta = std::thread::spawn(move || {
        let frame = make(b'A');
        for _ in 0..REPS {
            a.input(&frame).unwrap();
        }
    });
    let tb = std::thread::spawn(move || {
        let frame = make(b'B');
        for _ in 0..REPS {
            b.input(&frame).unwrap();
        }
    });
    ta.join().unwrap();
    tb.join().unwrap();

    // Collect echoed bytes until we have both streams' worth.
    let want = REPS * 32 * 2;
    let mut out: Vec<u8> = Vec::new();
    let _ = recv_until(&obs_events, Duration::from_secs(10), |ev| {
        if let SessionEvent::Output { data, .. } = ev {
            out.extend_from_slice(data);
        }
        (out.len() >= want).then_some(())
    });

    // Every `<....>` block must be a single letter — never A and B mixed, which
    // would mean one client's input was written into the middle of another's.
    let mut i = 0;
    let mut blocks = 0;
    while let Some(start) = out[i..].iter().position(|&c| c == b'<') {
        let s = i + start;
        if let Some(end_off) = out[s + 1..].iter().position(|&c| c == b'>') {
            let payload = &out[s + 1..s + 1 + end_off];
            let first = payload[0];
            assert!(
                payload.iter().all(|&c| c == first),
                "a frame was interleaved mid-write: {:?}",
                String::from_utf8_lossy(&out[s..=s + 1 + end_off])
            );
            blocks += 1;
            i = s + 1 + end_off + 1;
        } else {
            break;
        }
    }
    assert!(blocks >= REPS, "should have observed many whole frames, saw {blocks}");
    sess.kill_and_join();
}

#[test]
fn most_recently_active_client_size_wins_debounced() {
    let _g = serial();
    let mut sess = Session::start("demo/ws/size", 80, 24, &["/bin/sh", "-c", "exec sleep 30"]);
    let (a, _ae) = sess.client();
    let (b, _be) = sess.client();
    a.attach(None).unwrap();
    b.attach(None).unwrap();

    a.resize(100, 30).unwrap();
    b.resize(120, 40).unwrap();

    // A types: the PTY follows A's viewport.
    a.input(b"x").unwrap();
    let a_size = wait_for(Duration::from_secs(3), || {
        let info = a.info().ok()?;
        (info.cols == 100 && info.rows == 30).then_some(())
    });
    assert!(a_size.is_some(), "PTY should follow the client that last sent input (A: 100x30)");

    // B types: now the PTY follows B's viewport.
    b.input(b"y").unwrap();
    let b_size = wait_for(Duration::from_secs(3), || {
        let info = a.info().ok()?;
        (info.cols == 120 && info.rows == 40).then_some(())
    });
    assert!(b_size.is_some(), "PTY should follow the newly active client (B: 120x40)");

    // Debounce: a burst of resizes from the active client settles on the last.
    b.resize(130, 45).unwrap();
    b.resize(140, 50).unwrap();
    b.resize(150, 55).unwrap();
    let settled = wait_for(Duration::from_secs(3), || {
        let info = a.info().ok()?;
        (info.cols == 150 && info.rows == 55).then_some(())
    });
    assert!(settled.is_some(), "a burst of resizes should settle on the last size");
    sess.kill_and_join();
}

#[test]
fn pushed_events_are_gated_by_announced_capabilities() {
    let _g = serial();
    // The child re-sets the title on a loop, so a client that attaches after
    // startup reliably catches a title event (pushed events are not replayed).
    let mut sess = Session::start(
        "demo/ws/events",
        80,
        24,
        &["/bin/sh", "-c", "while :; do printf '\\033]0;NEWTITLE\\007'; sleep 0.2; done"],
    );

    // One client that understands events, one that announced none of them.
    let (full, full_events) = Connection::open(&sess.sock(), None, capability::ALL).unwrap();
    let (bare, bare_events) =
        Connection::open(&sess.sock(), None, &[capability::RESYNC]).unwrap();
    full.attach(None).unwrap();
    bare.attach(None).unwrap();

    // The full client receives the title (and the bell) events.
    let title = recv_until(&full_events, Duration::from_secs(5), |ev| match ev {
        SessionEvent::Title(t) => Some(t.clone()),
        _ => None,
    });
    assert_eq!(title.as_deref(), Some("NEWTITLE"), "a title-capable client gets the title event");

    // The bare client gets no title event (it did not announce the capability),
    // though it still receives output/resync on the core.
    let bare_title = recv_until(&bare_events, Duration::from_millis(800), |ev| {
        matches!(ev, SessionEvent::Title(_)).then_some(())
    });
    assert!(bare_title.is_none(), "a client that did not announce events must not receive them");
    sess.kill_and_join();
}

#[test]
fn info_snapshot_setmeta_and_detach_work() {
    let _g = serial();
    let mut sess = Session::start(
        "demo/ws/reqs",
        90,
        30,
        &["/bin/sh", "-c", "printf HELLOINFO; exec sleep 30"],
    );
    let (conn, events) = sess.client();

    // info: title/size/metadata/child state.
    let info = wait_for(Duration::from_secs(3), || conn.info().ok()).expect("info");
    assert_eq!(info.cols, 90);
    assert_eq!(info.rows, 30);
    assert_eq!(info.name, "demo/ws/reqs");
    assert!(info.child_running);
    assert_eq!(info.argv.first().map(String::as_str), Some("/bin/sh"));

    // snapshot: the visible screen text.
    let snap = conn.snapshot(None).expect("snapshot");
    assert!(snap.text.contains("HELLOINFO"), "snapshot should show child output: {:?}", snap.text);

    // set-meta: update name + task, reflected in meta.json and a later info.
    conn.set_meta(Some("demo/ws/renamed".into()), Some("t-42".into())).unwrap();
    let updated = wait_for(Duration::from_secs(3), || {
        let info = conn.info().ok()?;
        (info.name == "demo/ws/renamed" && info.task.as_deref() == Some("t-42")).then_some(())
    });
    assert!(updated.is_some(), "set-meta should update name and task");
    let on_disk =
        shelbi_session::Meta::from_json(&std::fs::read_to_string(sess.paths.meta()).unwrap())
            .unwrap();
    assert_eq!(on_disk.name, "demo/ws/renamed");
    assert_eq!(on_disk.task.as_deref(), Some("t-42"));

    // detach: after detaching, no further output is delivered.
    conn.attach(None).unwrap();
    recv_until(&events, Duration::from_secs(2), |ev| {
        matches!(ev, SessionEvent::Resync { .. }).then_some(())
    });
    conn.detach().unwrap();
    std::thread::sleep(Duration::from_millis(100));
    // Drain anything already in flight.
    while events.try_recv().is_some() {}
    conn.input(b"after-detach").unwrap();
    let leaked = recv_until(&events, Duration::from_millis(600), |ev| {
        matches!(ev, SessionEvent::Output { .. }).then_some(())
    });
    assert!(leaked.is_none(), "a detached client must not receive output");
    sess.kill_and_join();
}

#[test]
fn paste_uses_bracketed_paste_when_the_program_enabled_it() {
    let _g = serial();
    // Enable bracketed paste, raw mode, then echo input back verbatim.
    let mut sess = Session::start(
        "demo/ws/paste",
        80,
        24,
        &["/bin/sh", "-c", "stty raw -echo 2>/dev/null; printf '\\033[?2004h'; exec cat"],
    );
    let (conn, events) = sess.client();
    conn.attach(None).unwrap();
    recv_until(&events, Duration::from_secs(2), |ev| {
        matches!(ev, SessionEvent::Resync { .. }).then_some(())
    });
    // Give the child a moment to enable bracketed paste.
    std::thread::sleep(Duration::from_millis(200));

    conn.paste("pasted-text").unwrap();
    let mut out: Vec<u8> = Vec::new();
    let found = recv_until(&events, Duration::from_secs(5), |ev| {
        if let SessionEvent::Output { data, .. } = ev {
            out.extend_from_slice(data);
        }
        // The child echoes the bracketed-paste wrapper the session added.
        out.windows(6).any(|w| w == b"\x1b[200~").then_some(())
    });
    assert!(
        found.is_some(),
        "paste should be wrapped in bracketed-paste markers: {:?}",
        String::from_utf8_lossy(&out)
    );
    sess.kill_and_join();
}

#[test]
fn session_answers_a_keepalive_ping_with_a_pong() {
    let _g = serial();
    let mut sess = Session::start("demo/ws/ka", 80, 24, &["/bin/sh", "-c", "exec sleep 30"]);
    let mut raw = UnixStream::connect(sess.sock()).unwrap();
    raw.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    raw.write_all(
        &Frame::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            colors: None,
            capabilities: vec![capability::KEEPALIVE.to_string()],
        })
        .encode()
        .unwrap(),
    )
    .unwrap();
    let mut buf = Vec::new();
    // Session hello first.
    assert!(matches!(read_any(&mut raw, &mut buf), Some(AnyFrame::Core(Frame::Hello(_)))));
    // Ping → Pong.
    raw.write_all(&ExtFrame::Ping.encode().unwrap()).unwrap();
    raw.flush().unwrap();
    let pong = read_any(&mut raw, &mut buf);
    assert!(matches!(pong, Some(AnyFrame::Ext(ExtFrame::Pong))), "expected a Pong, got {pong:?}");
    sess.kill_and_join();
}

#[test]
fn exited_event_is_pushed_when_the_child_dies() {
    let _g = serial();
    let mut sess = Session::start("demo/ws/exit", 80, 24, &["/bin/sh", "-c", "exec sleep 30"]);
    let (conn, events) = sess.client();
    conn.attach(None).unwrap();
    recv_until(&events, Duration::from_secs(2), |ev| {
        matches!(ev, SessionEvent::Resync { .. }).then_some(())
    });
    // Kill the child; the session pushes the frozen-core exited event.
    conn.kill(Some(libc::SIGKILL)).unwrap();
    let exited = recv_until(&events, Duration::from_secs(5), |ev| match ev {
        SessionEvent::Exited(e) => Some(e.clone()),
        _ => None,
    });
    assert!(exited.is_some(), "an exited event should be pushed when the child dies");
    // run() returns on its own now the child is gone.
    if let Some(h) = sess.handle.take() {
        join_within(h, Duration::from_secs(10));
    }
}

#[test]
fn client_falls_back_to_core_when_a_capability_was_not_announced() {
    let _g = serial();
    // A mock session that announces NO capabilities. The client must then send a
    // paste as a core Input frame, not a Paste ext frame.
    let dir = tempfile::tempdir().unwrap();
    let sock = dir.path().join("mock.sock");
    let listener = std::os::unix::net::UnixListener::bind(&sock).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut buf = Vec::new();
        // Read the client hello.
        assert!(matches!(read_any(&mut stream, &mut buf), Some(AnyFrame::Core(Frame::Hello(_)))));
        // Reply announcing nothing.
        stream
            .write_all(
                &Frame::Hello(Hello {
                    protocol_version: PROTOCOL_VERSION,
                    colors: None,
                    capabilities: vec![],
                })
                .encode()
                .unwrap(),
            )
            .unwrap();
        // The next frame the client sends for a paste must be a core Input.
        match read_any(&mut stream, &mut buf) {
            Some(AnyFrame::Core(Frame::Input(i))) => String::from_utf8_lossy(&i.data).into_owned(),
            other => panic!("expected a core Input fallback, got {other:?}"),
        }
    });

    let (conn, _events) = Connection::open(&sock, None, capability::ALL).unwrap();
    assert!(!conn.supports(capability::PASTE), "mock announced no capabilities");
    conn.paste("falls-back").unwrap();
    let delivered = server.join().unwrap();
    assert_eq!(delivered, "falls-back", "paste should fall back to raw input");
    // info has no core fallback: it errors rather than sending a frame.
    assert!(matches!(
        conn.info(),
        Err(shelbi_client::ClientError::Unsupported(_))
    ));
}

#[test]
fn discovery_lists_sessions_and_reaps_dead_directories() {
    let _g = serial();
    let mut sess = Session::start("demo/ws/disco", 80, 24, &["/bin/sh", "-c", "exec sleep 30"]);
    let root = sess.paths.dir.parent().unwrap().to_path_buf();

    // A dead session directory: meta.json + an unheld lock file.
    let dead = root.join("deaddeaddeaddead");
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::write(
        dead.join("meta.json"),
        shelbi_session::Meta {
            id: "deaddeaddeaddead".into(),
            name: "demo/ws/gone".into(),
            argv: vec!["claude".into()],
            cwd: "/tmp".into(),
            task: None,
            launched_at: "2026-10-03T00:00:00Z".into(),
            protocol_version: PROTOCOL_VERSION,
        }
        .to_json()
        .unwrap(),
    )
    .unwrap();
    std::fs::write(dead.join("lock"), b"").unwrap(); // present but nobody holds it

    // list sees both; the live one is alive, the dead one is not.
    let listed = shelbi_client::list(&root).unwrap();
    let live = listed.iter().find(|s| s.meta.name == "demo/ws/disco").expect("live listed");
    assert!(live.alive, "the running session's lock is held");
    let gone = listed.iter().find(|s| s.meta.name == "demo/ws/gone").expect("dead listed");
    assert!(!gone.alive, "the dead session's lock is not held");

    // reap_dead removes the dead directory but leaves the live one.
    let reaped = shelbi_client::reap_dead(&root).unwrap();
    assert!(reaped.contains(&"deaddeaddeaddead".to_string()), "dead dir reaped: {reaped:?}");
    assert!(!dead.exists(), "dead directory should be gone");
    assert!(sess.paths.dir.exists(), "the live session directory must survive");
    assert!(shelbi_session::lock::is_held(&sess.paths.lock()), "live session still alive");

    sess.kill_and_join();
}
