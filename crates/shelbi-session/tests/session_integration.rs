//! Real-PTY integration tests for the `shelbi __session` body ([`shelbi_session::run`]).
//!
//! These drive `run()` on a background thread against a genuine PTY with a real
//! `/bin/sh` child (no external agent binary), covering the acceptance criteria:
//! the on-disk layout and socket-path limit, the scrubbed child environment, the
//! query responder answering with **no client attached**, `exit.json` /
//! `final.txt` on child exit, process-group kill, and no descriptor leak.
//!
//! `run()` resolves `~/.shelbi/sessions` from `$SHELBI_HOME`, which is a
//! process-global env var, so the whole file runs under one serial lock.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use shelbi_proto::{Attach, Frame, Hello, Kill};
use shelbi_session::{layout::SessionPaths, RunArgs};

fn serial() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

/// Set `$SHELBI_HOME` to a fresh temp dir for this test; returns the guard that
/// cleans it up when dropped.
fn set_home() -> tempfile::TempDir {
    let home = tempfile::tempdir().expect("tempdir");
    std::env::set_var("SHELBI_HOME", home.path());
    home
}

/// Poll `f` until it returns `Some`, or the deadline passes.
fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Join a `run()` thread with a hard deadline, returning `None` if it does not
/// finish in time (rather than blocking forever on a broken kill path). The
/// handle is moved into a helper thread so a stuck join leaks that thread
/// instead of wedging the test; the process exits normally regardless.
fn join_within(
    handle: std::thread::JoinHandle<anyhow::Result<()>>,
    timeout: Duration,
) -> Option<anyhow::Result<()>> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(handle.join());
    });
    rx.recv_timeout(timeout)
        .ok()
        .map(|joined| joined.expect("run thread panicked"))
}

/// Join with a hard deadline, failing loudly if the thread does not return: a
/// kill that never reached the child would otherwise hang the whole CI job.
fn join_run(
    handle: std::thread::JoinHandle<anyhow::Result<()>>,
    timeout: Duration,
) -> anyhow::Result<()> {
    join_within(handle, timeout).unwrap_or_else(|| {
        panic!("session run() did not return within {timeout:?}; the kill did not reach the child")
    })
}

struct RunningSession {
    paths: SessionPaths,
    handle: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
    // Kept alive for the session's lifetime.
    _home: tempfile::TempDir,
}

impl RunningSession {
    /// Start `run()` on a thread with the given child argv and size, and wait for
    /// the socket to appear.
    fn start(name: &str, cwd: &Path, cols: u16, rows: u16, child_argv: Vec<String>) -> Self {
        Self::start_inner(name, cwd, cols, rows, child_argv, false)
    }

    fn start_with_raw_log(
        name: &str,
        cwd: &Path,
        child_argv: Vec<String>,
    ) -> Self {
        Self::start_inner(name, cwd, 80, 24, child_argv, true)
    }

    fn start_inner(
        name: &str,
        cwd: &Path,
        cols: u16,
        rows: u16,
        child_argv: Vec<String>,
        raw_output_log: bool,
    ) -> Self {
        let home = set_home();
        let id = shelbi_session::layout::derive_id_now(name);
        let paths = SessionPaths::new(&home.path().join("sessions"), &id);
        let args = RunArgs {
            id: id.clone(),
            name: name.to_string(),
            cwd: cwd.to_path_buf(),
            cols,
            rows,
            task: None,
            raw_output_log,
            child_argv,
            // No daemon watchdog in-process: it loops reading the global
            // `$SHELBI_HOME` this harness mutates per test, which would be a
            // data race (see `RunArgs::manage_daemon`).
            manage_daemon: false,
        };
        let handle = std::thread::spawn(move || shelbi_session::run(args));
        // Wait for the socket to be bound.
        let sock = paths.sock();
        wait_for(Duration::from_secs(5), || sock.exists().then_some(()))
            .expect("session socket should appear");
        Self {
            paths,
            handle: Some(handle),
            _home: home,
        }
    }

    fn connect(&self) -> UnixStream {
        let stream = UnixStream::connect(self.paths.sock()).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
    }

    /// Send a kill frame to end the child, then join the run thread.
    fn kill_and_join(&mut self) -> anyhow::Result<()> {
        if let Ok(mut s) = UnixStream::connect(self.paths.sock()) {
            let frame = Frame::Kill(Kill {
                signal: Some(libc::SIGKILL),
            })
            .encode()
            .unwrap();
            let _ = s.write_all(&frame);
        }
        join_run(
            self.handle.take().expect("joined once"),
            Duration::from_secs(10),
        )
    }
}

impl Drop for RunningSession {
    fn drop(&mut self) {
        // Backstop: if a test returned early, make sure the run thread is not
        // left waiting on a live child.
        if let Some(handle) = self.handle.take() {
            if let Ok(mut s) = UnixStream::connect(self.paths.sock()) {
                let frame = Frame::Kill(Kill {
                    signal: Some(libc::SIGKILL),
                })
                .encode()
                .unwrap();
                let _ = s.write_all(&frame);
            }
            // Bounded join: never let a stuck run thread hang teardown, and
            // never panic from Drop (a double-panic would abort the process).
            let _ = join_within(handle, Duration::from_secs(10));
        }
    }
}

/// Read one frame from a stream (helper for the hello handshake).
fn read_frame(stream: &mut UnixStream) -> Option<Frame> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        match Frame::decode(&buf) {
            Ok((frame, _)) => return Some(frame),
            Err(shelbi_proto::ProtoError::Incomplete { .. }) => {}
            Err(_) => return None,
        }
        if Instant::now() >= deadline {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(_) => return None,
        }
    }
}

#[test]
fn creates_layout_and_answers_hello_then_exits_on_kill() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut sess = RunningSession::start(
        "demo/ws/alpha",
        dir.path(),
        90,
        30,
        vec!["/bin/sh".into(), "-c".into(), "exec sleep 10".into()],
    );

    // meta.json, lock, sock all present; socket path under the limit.
    assert!(sess.paths.meta().exists(), "meta.json written");
    assert!(shelbi_session::lock::is_held(&sess.paths.lock()), "lock held while alive");
    sess.paths.check_socket_fits().expect("socket path fits");

    let meta = shelbi_session::Meta::from_json(
        &std::fs::read_to_string(sess.paths.meta()).unwrap(),
    )
    .unwrap();
    assert_eq!(meta.name, "demo/ws/alpha");
    assert_eq!(meta.argv, vec!["/bin/sh", "-c", "exec sleep 10"]);
    assert_eq!(meta.protocol_version, shelbi_proto::PROTOCOL_VERSION);

    // Hello handshake over the socket.
    let mut stream = sess.connect();
    let hello = Frame::Hello(Hello {
        protocol_version: shelbi_proto::PROTOCOL_VERSION,
        colors: None,
        capabilities: vec![],
    })
    .encode()
    .unwrap();
    stream.write_all(&hello).unwrap();
    match read_frame(&mut stream) {
        Some(Frame::Hello(reply)) => {
            assert_eq!(reply.protocol_version, shelbi_proto::PROTOCOL_VERSION)
        }
        other => panic!("expected a Hello reply, got {other:?}"),
    }
    drop(stream);

    // Kill ends the child; run() returns and writes exit.json + final.txt.
    sess.kill_and_join().unwrap();
    assert!(sess.paths.exit().exists(), "exit.json written on exit");
    assert!(sess.paths.final_txt().exists(), "final.txt written on exit");
    // The lock is released once run() returns.
    assert!(!shelbi_session::lock::is_held(&sess.paths.lock()));
}

#[test]
fn child_gets_scrubbed_login_environment_with_shelbi_term_vars() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("env.txt");
    // Only shell builtins (echo) so this does not depend on the child's PATH.
    let script = format!(
        "echo \"T=$TERM C=$COLORTERM P=$TERM_PROGRAM M=${{TMUX:-none}}\" > {}; exec sleep 10",
        out.display()
    );
    let mut sess = RunningSession::start(
        "demo/shell/alpha",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), script],
    );

    let body = wait_for(Duration::from_secs(5), || {
        std::fs::read_to_string(&out).ok().filter(|s| !s.is_empty())
    })
    .expect("child should write its environment");

    assert!(body.contains("T=xterm-256color"), "TERM set: {body}");
    assert!(body.contains("C=truecolor"), "COLORTERM set: {body}");
    assert!(body.contains("P=shelbi"), "TERM_PROGRAM set: {body}");
    assert!(body.contains("M=none"), "TMUX scrubbed: {body}");

    sess.kill_and_join().unwrap();
}

#[test]
fn responder_answers_queries_with_no_client_attached() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("reply.bin");
    // Emit a DSR cursor-position query and a primary-DA query, then read exactly
    // the 15 reply bytes the session writes back onto our stdin. No client is
    // ever attached — the session answers from its own emulator.
    //   DSR-6 reply for cursor 1;1 => ESC [ 1 ; 1 R           (6 bytes)
    //   DA1 reply                  => ESC [ ? 6 2 ; 2 2 c     (9 bytes)
    // Put the tty in raw mode first: in the default canonical mode the line
    // discipline withholds the reply (it has no newline) and `head` would block
    // forever. A real agent raw-modes the tty before querying for the same reason.
    let script = format!(
        "stty raw -echo 2>/dev/null; printf '\\033[6n\\033[c'; /usr/bin/head -c 15 > {}; sleep 10",
        out.display()
    );
    let mut sess = RunningSession::start(
        "demo/ws/query",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), script],
    );

    let reply = wait_for(Duration::from_secs(5), || {
        std::fs::read(&out).ok().filter(|b| b.len() >= 15)
    });
    sess.kill_and_join().unwrap();

    let reply = reply.expect("session must answer the queries with no client attached");
    assert!(
        reply.windows(6).any(|w| w == b"\x1b[1;1R"),
        "expected a DSR cursor-position reply in {reply:?}"
    );
    assert!(
        reply.windows(9).any(|w| w == b"\x1b[?62;22c"),
        "expected a primary-DA reply in {reply:?}"
    );
}

#[test]
fn kill_reaches_the_whole_process_group() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("gc.txt");
    // The child starts a grandchild in the same group and records its pid.
    let script = format!(
        "sleep 300 & echo $! > {}; exec sleep 300",
        out.display()
    );
    let mut sess = RunningSession::start(
        "demo/ws/group",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), script],
    );

    let gc: i32 = wait_for(Duration::from_secs(5), || {
        std::fs::read_to_string(&out)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    })
    .expect("grandchild pid recorded");
    assert!(alive(gc), "grandchild alive before kill");

    sess.kill_and_join().unwrap();

    // After the session reaps the group, the grandchild is gone.
    let reaped = wait_for(Duration::from_secs(5), || (!alive(gc)).then_some(()));
    assert!(reaped.is_some(), "grandchild survived the group kill");
}

#[test]
fn no_descriptor_leaks_into_the_child() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("fds.txt");
    // Count char-device fds the child holds. The three pty fds (0/1/2) are char
    // devices; a leaked master would show as a fourth.
    // Write the fd list via a dedicated fd (3 → the file) so stdout (fd 1) stays
    // the pty during enumeration; otherwise redirecting the whole loop to a file
    // moves fd 1 and the shell saves the tty onto a high fd, skewing the count.
    let script = format!(
        "exec 3> {}; for f in /dev/fd/*; do n=${{f##*/}}; [ -c \"$f\" ] && echo $n >&3; done; exec 3>&-; exec sleep 10",
        out.display()
    );
    let mut sess = RunningSession::start(
        "demo/ws/fds",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), script],
    );

    let body = wait_for(Duration::from_secs(5), || {
        std::fs::read_to_string(&out).ok().filter(|s| !s.is_empty())
    })
    .expect("child should list its fds");
    sess.kill_and_join().unwrap();

    let mut fds: Vec<i32> = body.lines().filter_map(|l| l.trim().parse().ok()).collect();
    fds.sort_unstable();
    fds.dedup();
    assert_eq!(
        fds,
        vec![0, 1, 2],
        "child should hold exactly the three pty fds as char devices; a fourth is a leaked master:\n{body}"
    );
}

#[test]
fn raw_output_log_is_written_only_when_enabled() {
    let _g = serial();

    // Enabled: the raw log captures the child's output.
    {
        let dir = tempfile::tempdir().unwrap();
        let mut sess = RunningSession::start_with_raw_log(
            "demo/ws/rawon",
            dir.path(),
            vec!["/bin/sh".into(), "-c".into(), "printf RAWMARKER; exec sleep 10".into()],
        );
        let has_marker = wait_for(Duration::from_secs(5), || {
            std::fs::read(sess.paths.raw_log())
                .ok()
                .filter(|b| b.windows(9).any(|w| w == b"RAWMARKER"))
                .map(|_| ())
        });
        sess.kill_and_join().unwrap();
        assert!(has_marker.is_some(), "raw log should capture child output when enabled");
    }

    // Disabled (default): no raw log file is created.
    {
        let dir = tempfile::tempdir().unwrap();
        let mut sess = RunningSession::start(
            "demo/ws/rawoff",
            dir.path(),
            80,
            24,
            vec!["/bin/sh".into(), "-c".into(), "printf RAWMARKER; exec sleep 10".into()],
        );
        // Give the child a moment to produce output.
        std::thread::sleep(Duration::from_millis(300));
        let raw_exists = sess.paths.raw_log().exists();
        sess.kill_and_join().unwrap();
        assert!(!raw_exists, "raw log must not exist when the project did not enable it");
    }
}

// --- connection lifecycle: no thread/fd leak on repeated attach, and a slow or
// dead client never wedges its handler or blocks others
// (`rt-review-session-wedges-after-repeated-attaches`). ---

/// Count the file descriptors this process currently has open. The session runs
/// in-process (on a `run()` thread), so a leaked per-connection handler shows up
/// here as held descriptors. Portable across the session's platforms: Linux
/// exposes `/proc/self/fd`, macOS `/dev/fd`. Reading the directory opens one
/// transient fd, but it is counted identically on every call so deltas cancel.
fn open_fd_count() -> usize {
    for dir in ["/proc/self/fd", "/dev/fd"] {
        if let Ok(rd) = std::fs::read_dir(dir) {
            return rd.count();
        }
    }
    0
}

/// Send the client hello (negotiating every capability, so the session treats
/// this as a full client — resync backpressure included) and read the session's
/// hello reply.
fn client_hello(stream: &mut UnixStream) {
    let hello = Frame::Hello(Hello {
        protocol_version: shelbi_proto::PROTOCOL_VERSION,
        colors: None,
        capabilities: shelbi_proto::capability::ALL
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
    .encode()
    .unwrap();
    stream.write_all(&hello).unwrap();
    stream.flush().unwrap();
    read_frame(stream).expect("session answers the hello");
}

/// Subscribe this client to the output stream.
fn client_attach(stream: &mut UnixStream) {
    let frame = Frame::Attach(Attach { since_seq: None }).encode().unwrap();
    stream.write_all(&frame).unwrap();
    stream.flush().unwrap();
}

#[test]
fn repeated_attach_detach_does_not_leak_descriptors() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut sess = RunningSession::start(
        "demo/ws/cycle",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
    );

    // One full attach/detach: connect, handshake, subscribe, read the attach
    // replay, then drop the connection (a clean close).
    let cycle = |sess: &RunningSession| {
        let mut s = sess.connect();
        client_hello(&mut s);
        client_attach(&mut s);
        let _ = read_frame(&mut s); // the attach Resync replay
        drop(s);
    };

    // Warm up so first-connection lazy allocations settle, then take a baseline
    // once the fd count stops moving.
    for _ in 0..5 {
        cycle(&sess);
    }
    let base = wait_for(Duration::from_secs(5), {
        let mut last = 0usize;
        let mut stable = 0u8;
        move || {
            let now = open_fd_count();
            if now == last {
                stable += 1;
            } else {
                stable = 0;
                last = now;
            }
            (stable >= 2).then_some(last)
        }
    })
    .expect("fd count settles before the run");

    for _ in 0..200 {
        cycle(&sess);
    }

    // After 200 clean cycles the session must have wound every handler down, so
    // the fd count returns to the baseline (a per-connection leak would grow it
    // by a few descriptors each cycle — hundreds total).
    let after = wait_for(Duration::from_secs(8), || {
        let now = open_fd_count();
        (now <= base + 8).then_some(now)
    });
    let after = after.unwrap_or_else(open_fd_count);
    assert!(
        after <= base + 8,
        "descriptors leaked across 200 attach/detach cycles: base={base}, after={after}",
    );

    sess.kill_and_join().unwrap();
}

#[test]
fn a_silent_client_does_not_block_another_clients_hello() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut sess = RunningSession::start(
        "demo/ws/indep",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), "exec yes".into()],
    );

    // One client attaches and then stops reading, wedging its own writer on the
    // flood of output.
    let mut stuck = sess.connect();
    client_hello(&mut stuck);
    client_attach(&mut stuck);
    // Let output pile up against the silent client.
    std::thread::sleep(Duration::from_millis(300));

    // A second client's hello must still complete promptly — each connection has
    // its own handler threads and the broadcast holds no lock across a blocking
    // write, so the stuck client cannot stall it.
    let mut fresh = sess.connect();
    let hello = Frame::Hello(Hello {
        protocol_version: shelbi_proto::PROTOCOL_VERSION,
        colors: None,
        capabilities: shelbi_proto::capability::ALL
            .iter()
            .map(|s| s.to_string())
            .collect(),
    })
    .encode()
    .unwrap();
    let start = Instant::now();
    fresh.write_all(&hello).unwrap();
    fresh.flush().unwrap();
    let reply = read_frame(&mut fresh);
    let elapsed = start.elapsed();
    assert!(
        matches!(reply, Some(Frame::Hello(_))),
        "the second client must get its hello reply",
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "a silent client blocked a second client's hello ({elapsed:?})",
    );

    drop(stuck);
    drop(fresh);
    sess.kill_and_join().unwrap();
}
