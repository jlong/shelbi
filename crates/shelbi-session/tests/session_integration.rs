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

/// A generous upper bound for polling a one-time condition to completion:
/// the socket binding, the child writing a file, fd/thread counts settling, the
/// run thread joining after a kill. Sized so a loaded host — several workers and
/// a parallel `cargo build --workspace` saturating every core — still satisfies
/// a healthy condition far inside it, while a genuine hang or leak never
/// satisfies it and still trips the caller's assertion at the deadline. A longer
/// deadline never weakens a check; it only removes the false failures that come
/// from scheduler starvation, not from the behavior under test.
const SETTLE: Duration = Duration::from_secs(30);

/// How long to wait for a protocol frame we expect to arrive (a hello reply, an
/// attach replay). Generous for the same load reasons as [`SETTLE`]: a reply
/// that is merely slow under load still reads cleanly, while one that never
/// comes still fails the caller's assertion once this elapses.
const FRAME_READ: Duration = Duration::from_secs(15);

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
        // Wait for the socket to be bound. `run()` binds it only after opening
        // the PTY, spawning the child, and starting three background threads
        // (session.rs); on a host under parallel-build load that cold startup can
        // take several seconds, so poll up to the generous `SETTLE` deadline
        // rather than a tight one that races scheduler starvation.
        let sock = paths.sock();
        wait_for(SETTLE, || sock.exists().then_some(()))
            .expect("session socket should appear");
        Self {
            paths,
            handle: Some(handle),
            _home: home,
        }
    }

    fn connect(&self) -> UnixStream {
        // Retry a transient connect failure rather than panic. The leak probes
        // fire connects in rapid bursts; on a loaded host the accept thread can be
        // starved long enough for the listen backlog to fill, and the kernel then
        // refuses a connect (`ECONNREFUSED` on macOS) even though the session is
        // perfectly healthy and draining — a test artifact of the burst, not the
        // behavior under test. A not-yet-visible socket file (`NotFound`) is the
        // same kind of transient startup race. Poll past both up to `SETTLE`; a
        // genuinely dead listener keeps refusing and still fails at the deadline.
        let sock = self.paths.sock();
        let stream = wait_for(SETTLE, || match UnixStream::connect(&sock) {
            Ok(s) => Some(s),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) =>
            {
                None
            }
            Err(e) => panic!("connect: {e:?}"),
        })
        .expect("session accepts a connection within the deadline");
        // Bound reads so a never-arriving frame surfaces as a prompt error rather
        // than a hang, but keep the bound generous (`FRAME_READ`): a reply that is
        // merely slow under load must still read cleanly. It is far past every
        // handshake window the session enforces, so a reaped probe still returns
        // EOF long before this fires.
        stream.set_read_timeout(Some(FRAME_READ)).unwrap();
        stream
    }

    /// Send a kill frame to end the child, then join the run thread.
    fn kill_and_join(&mut self) -> anyhow::Result<()> {
        send_kill(&self.paths.sock());
        join_run(self.handle.take().expect("joined once"), SETTLE)
    }
}

/// Best-effort: connect to the session socket and send a `SIGKILL` frame. Retries
/// a transient connection refusal (a momentarily full accept backlog under load)
/// for a short window so the kill still reaches a healthy session; a *missing*
/// socket means the session already exited (nothing to kill), so that returns at
/// once rather than waiting out the window.
fn send_kill(sock: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match UnixStream::connect(sock) {
            Ok(mut s) => {
                if let Ok(frame) = Frame::Kill(Kill {
                    signal: Some(libc::SIGKILL),
                })
                .encode()
                {
                    let _ = s.write_all(&frame);
                }
                return;
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::ConnectionRefused
                    && Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            // Socket gone (session exited) or any other error: best effort, done.
            Err(_) => return,
        }
    }
}

impl Drop for RunningSession {
    fn drop(&mut self) {
        // Backstop: if a test returned early, make sure the run thread is not
        // left waiting on a live child.
        if let Some(handle) = self.handle.take() {
            send_kill(&self.paths.sock());
            // Bounded join: never let a stuck run thread hang teardown, and
            // never panic from Drop (a double-panic would abort the process).
            let _ = join_within(handle, SETTLE);
        }
    }
}

/// Read one frame from a stream (helper for the hello handshake).
fn read_frame(stream: &mut UnixStream) -> Option<Frame> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + FRAME_READ;
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

    let body = wait_for(SETTLE, || {
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

    let reply = wait_for(SETTLE, || {
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

    let gc: i32 = wait_for(SETTLE, || {
        std::fs::read_to_string(&out)
            .ok()
            .and_then(|s| s.trim().parse().ok())
    })
    .expect("grandchild pid recorded");
    assert!(alive(gc), "grandchild alive before kill");

    sess.kill_and_join().unwrap();

    // After the session reaps the group, the grandchild is gone.
    let reaped = wait_for(SETTLE, || (!alive(gc)).then_some(()));
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

    let body = wait_for(SETTLE, || {
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
        let has_marker = wait_for(SETTLE, || {
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
    let base = wait_for(SETTLE, {
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
    let after = wait_for(SETTLE, || {
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
    // Let output pile up against the silent client so its writer is parked on a
    // full socket buffer before the second client arrives. `yes` fills the kernel
    // buffer near-instantly, so this only ever under-wedges on a pathologically
    // slow host — which would make the test trivially pass, never falsely fail.
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
    // `read_frame` waits up to the generous `FRAME_READ` window, so a reply that
    // is merely slow under load is still read rather than lost to a tight read
    // timeout — we rely on the measured `elapsed`, not the read bound, to catch a
    // regression.
    let reply = read_frame(&mut fresh);
    let elapsed = start.elapsed();
    assert!(
        matches!(reply, Some(Frame::Hello(_))),
        "the second client must get its hello reply (got {reply:?} after {elapsed:?})",
    );
    // The discriminator. A correct session answers the fresh hello on that
    // connection's own handler/writer threads in milliseconds. The only
    // regression this guards — serializing the broadcast's socket writes behind a
    // held registry lock — would stall the fresh client's registration behind the
    // stuck client's blocked write for at least the 2 s client write timeout (and,
    // under the continuous `yes` flood, on every broadcast cycle after). The 1.5 s
    // bound sits with clear margin on both sides: far above the millisecond-scale
    // healthy path even on a loaded host (two thread wake-ups), and below the 2 s
    // regression floor.
    assert!(
        elapsed < Duration::from_millis(1500),
        "a silent client blocked a second client's hello ({elapsed:?})",
    );

    drop(stuck);
    drop(fresh);
    sess.kill_and_join().unwrap();
}

/// Count this process's live threads. Linux exposes them under
/// `/proc/self/task`; macOS has no equally portable equivalent here, so on macOS
/// the leak signal is the fd count alone (a leaked per-connection handler holds
/// three descriptors as well as its two threads). Returns `None` when the count
/// can't be read, so the caller skips the thread assertion on an unsupported
/// platform rather than failing — the fd assertion still carries the leak check,
/// and CI (Linux) exercises the thread count.
fn thread_count() -> Option<usize> {
    std::fs::read_dir("/proc/self/task").ok().map(|rd| rd.count())
}

/// Wait for the live thread count to fall back to within `slack` of `base`, then
/// return the settled count. Returns `None` only where `thread_count()` itself is
/// unavailable (non-Linux), so callers skip the thread assertion exactly as they
/// would with a bare `thread_count()`.
///
/// Why this isn't a bare sample: a handler winds down by *returning* from
/// `serve_client` / `client_writer`, which drops its sockets — so the fd count
/// falls back the instant the threads finish. But Linux reclaims a returned
/// thread's `/proc/self/task` entry asynchronously, a little after the function
/// returns and the client has already seen EOF. A sample taken the moment the
/// last probe's fds settle can still count handler threads that have returned but
/// whose task entries the kernel hasn't reaped yet, so the thread count lags the
/// (already-flat) fd count on Linux. Polling it the same way the fd check does
/// lets that lag drain without loosening the bound: a genuine thread leak never
/// settles and still trips the assertion after `timeout`.
fn settled_thread_count(base: usize, slack: usize, timeout: Duration) -> Option<usize> {
    thread_count()?; // availability gate: None on platforms without /proc/self/task
    Some(
        wait_for(timeout, || thread_count().filter(|&n| n <= base + slack))
            .or_else(thread_count)
            .unwrap_or(0),
    )
}

/// Wait for the open-fd count to stop moving, then return it — the baseline a
/// leak check measures deltas against (lazy first-connection allocations settle
/// first). Falls back to a bare sample if it never fully settles.
fn settled_fd_count() -> usize {
    wait_for(SETTLE, {
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
    .unwrap_or_else(open_fd_count)
}

#[test]
fn bare_connect_and_drop_probes_do_not_leak() {
    // A peer that connects and drops at once without ever sending a hello (an
    // aborted probe). The session sees the EOF and must wind its handler down
    // immediately; 200 of them leave the thread and fd counts flat
    // (`rt-find-the-5s-connection-to-the-review-session`).
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    let mut sess = RunningSession::start(
        "demo/ws/dropprobe",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
    );

    // Run the probes in bounded batches, draining each batch's handlers back to
    // the baseline before the next. This caps how many connections are in flight
    // at once (a tight unpaced `connect(); drop();` loop can outrun the session's
    // accept-and-reap and pile up handlers past the process's own fd limit — a
    // test artifact, not the behavior under test), while still exercising 200
    // connect-and-drops and proving none of them leaves a handler behind.
    let drop_batch = |sess: &RunningSession, n: usize| {
        let probes: Vec<UnixStream> = (0..n).map(|_| sess.connect()).collect();
        drop(probes);
    };

    // Warm up so first-connection lazy allocations settle, then baseline.
    drop_batch(&sess, 5);
    let base_fds = settled_fd_count();
    let base_threads = thread_count();

    let (total, batch) = (200usize, 20usize);
    let mut done = 0;
    while done < total {
        let n = batch.min(total - done);
        drop_batch(&sess, n);
        // The session must reap this batch (EOF on each dropped probe) back to the
        // baseline before we add more, or a real leak would be masked by the cap.
        let settled = wait_for(SETTLE, || {
            let now = open_fd_count();
            (now <= base_fds + 8).then_some(now)
        });
        assert!(
            settled.is_some(),
            "a connect-and-drop batch did not wind its handlers down (base={base_fds}, now={})",
            open_fd_count(),
        );
        done += n;
    }

    let after_fds = wait_for(SETTLE, || {
        let now = open_fd_count();
        (now <= base_fds + 8).then_some(now)
    })
    .unwrap_or_else(open_fd_count);
    assert!(
        after_fds <= base_fds + 8,
        "descriptors leaked across 200 connect-and-drop probes: base={base_fds}, after={after_fds}",
    );
    if let Some(base) = base_threads {
        // Poll the thread count down, same as the fd check above: on Linux the
        // reaped handler threads' task entries drain a beat after their fds do.
        let after = settled_thread_count(base, 4, SETTLE)
            .expect("thread_count is available since base was Some");
        assert!(
            after <= base + 4,
            "threads leaked across 200 connect-and-drop probes: base={base}, after={after}",
        );
    }

    sess.kill_and_join().unwrap();
}

#[test]
fn connect_and_hang_probes_do_not_leak() {
    // A peer that connects, sends no hello, and holds its socket open past the
    // handshake window (it wedged mid-handshake, or opened the socket and walked
    // away). Without a server-side bound each one parks a reader thread and holds
    // the handler and its three fds forever, until the session hits EMFILE. The
    // session must close such a connection on its own clock. We drive that clock
    // short (`SHELBI_HELLO_TIMEOUT_MS`) so the test doesn't wait out the 5 s
    // default, and confirm the SESSION closes each probe — the client read
    // returns EOF while the client still holds its end open — leaving the thread
    // and fd counts flat across 200 (`rt-find-the-5s-connection-to-the-review-session`).
    let _g = serial();
    std::env::set_var("SHELBI_HELLO_TIMEOUT_MS", "300");
    let dir = tempfile::tempdir().unwrap();
    let mut sess = RunningSession::start(
        "demo/ws/hangprobe",
        dir.path(),
        80,
        24,
        vec!["/bin/sh".into(), "-c".into(), "exec sleep 60".into()],
    );

    // Warm up (each probe held until the session reaps it), then baseline with no
    // probe outstanding.
    let one_batch = |sess: &RunningSession, n: usize| {
        let mut probes: Vec<UnixStream> = (0..n).map(|_| sess.connect()).collect();
        for p in &mut probes {
            // `connect` set the generous `FRAME_READ` read timeout — far past the
            // short handshake window — so a genuine reap returns EOF while a
            // regression that never closes the connection surfaces as a timeout
            // the assert rejects, not a hang. (We must not re-set the timeout
            // here: on macOS, once the
            // session has already closed its end with `shutdown(Both)`,
            // `set_read_timeout` on the half-closed socket fails EINVAL — which is
            // itself proof the session reaped it.)
            let mut b = [0u8; 1];
            let r = p.read(&mut b);
            assert!(
                matches!(r, Ok(0)),
                "the session must close an un-handshaken probe on its own clock; got {r:?}",
            );
        }
        drop(probes);
    };

    one_batch(&sess, 10);
    let base_fds = settled_fd_count();
    let base_threads = thread_count();

    // 200 probes, batched so at most ~20 handlers are alive at once (the test
    // process stays well under its own descriptor limit).
    let (total, batch) = (200usize, 20usize);
    let mut done = 0;
    while done < total {
        let n = batch.min(total - done);
        one_batch(&sess, n);
        done += n;
    }

    let after_fds = wait_for(SETTLE, || {
        let now = open_fd_count();
        (now <= base_fds + 8).then_some(now)
    })
    .unwrap_or_else(open_fd_count);
    assert!(
        after_fds <= base_fds + 8,
        "descriptors leaked across 200 connect-and-hang probes: base={base_fds}, after={after_fds}",
    );
    if let Some(base) = base_threads {
        // Poll the thread count down, same as the fd check above: on Linux the
        // reaped handler threads' task entries drain a beat after their fds do.
        let after = settled_thread_count(base, 4, SETTLE)
            .expect("thread_count is available since base was Some");
        assert!(
            after <= base + 4,
            "threads leaked across 200 connect-and-hang probes: base={base}, after={after}",
        );
    }

    std::env::remove_var("SHELBI_HELLO_TIMEOUT_MS");
    sess.kill_and_join().unwrap();
}
