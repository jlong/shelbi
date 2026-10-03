//! End-to-end tests for the on-demand daemon lifecycle (Phase 3 of the
//! remove-tmux effort, `docs/removing-tmux/phase3-daemon.md`):
//!
//! - Opening a project (here: calling the `__ensure-daemon` helper with a
//!   project marked open) starts a daemon, and racing clients converge on
//!   exactly one.
//! - The daemon exits once the last open project is quit.
//!
//! Each test runs against its own short `SHELBI_HOME` under `/tmp` (macOS caps
//! Unix-socket paths at ~104 bytes), fully isolated from any real daemon, and
//! drives the idle monitor fast via the `SHELBI_DAEMON_IDLE_*` overrides.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// The per-project state subdir under a shelbi home. Named via a const rather
/// than an inline `.join("projects")` literal: production callers must route
/// through `shelbi_state::projects_dir()` (a callsite-scan lint enforces it),
/// but this fixture writes an *isolated* home's layout for a child process and
/// can't use that helper, which reads the test process's own `SHELBI_HOME`.
const PROJECTS: &str = "projects";

/// A unique, short `SHELBI_HOME` for one test, removed on drop.
struct Home {
    path: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Self {
        let path = PathBuf::from(format!("/tmp/shb-dlt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(PROJECTS)).unwrap();
        Self { path }
    }

    fn sock(&self) -> PathBuf {
        self.path.join("hub.sock")
    }

    /// Register a project and set its open flag in `state.json`.
    fn open_project(&self, name: &str) {
        std::fs::write(self.path.join(PROJECTS).join(format!("{name}.yaml")), b"").unwrap();
        let dir = self.path.join(PROJECTS).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), br#"{"open":true}"#).unwrap();
    }

    /// Clear a project's open flag — the on-disk effect of quitting it.
    fn quit_project(&self, name: &str) {
        let dir = self.path.join(PROJECTS).join(name);
        std::fs::write(dir.join("state.json"), br#"{"open":false}"#).unwrap();
    }

    /// Spawn `shelbi <args>` against this home with the idle monitor driven
    /// fast. Returns the child so the caller controls waiting.
    fn spawn(&self, args: &[&str]) -> std::process::Child {
        Command::new(BIN)
            .args(args)
            .env("SHELBI_HOME", &self.path)
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_HUB_SOCK")
            .env("SHELBI_DAEMON_IDLE_GRACE_MS", "400")
            .env("SHELBI_DAEMON_IDLE_POLL_MS", "200")
            // The version gate and board refresh never need a real `gh`; keep
            // the daemon's startup quiet and fast.
            .env("SHELBI_YES", "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn shelbi")
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Best-effort: make sure no daemon lingers against this home, then
        // remove the dir.
        self.quit_project("p");
        let _ = wait_until(Duration::from_secs(5), || !socket_answers(&self.sock()));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Does a hello probe to `sock` get an answer? Mirrors
/// `shelbi_state::probe_daemon_hello` without touching this process's env: a
/// daemon answers an empty, write-closed probe with its hello line.
fn socket_answers(sock: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(sock) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.shutdown(Shutdown::Write);
    let mut reader = std::io::BufReader::new(&stream);
    let mut line = Vec::new();
    use std::io::BufRead;
    match reader.read_until(b'\n', &mut line) {
        Ok(n) => n > 0 && line.windows(7).any(|w| w == b"version"),
        Err(_) => false,
    }
}

/// Poll `cond` until it is true or `deadline` elapses.
fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn opening_a_project_starts_one_daemon_even_under_racing_clients() {
    let home = Home::new("race");
    home.open_project("p");
    assert!(!socket_answers(&home.sock()), "no daemon before any client");

    // Several clients race to ensure the daemon. Each exits 0 once the socket
    // answers; the daemon's own bind lock makes the race converge on one.
    let mut racers: Vec<_> = (0..8).map(|_| home.spawn(&["__ensure-daemon"])).collect();
    for mut r in racers.drain(..) {
        let status = r.wait().expect("wait ensure-daemon");
        assert!(
            status.success(),
            "every racing __ensure-daemon must succeed: {status:?}"
        );
    }

    assert!(
        socket_answers(&home.sock()),
        "a daemon must be listening after the ensure helpers returned"
    );

    // Exactly one: a second foreground `shelbi daemon` must fail fast because
    // the single-instance lock is held (it errors before serving, so it does
    // not block).
    let mut second = home.spawn(&["daemon"]);
    let got = wait_until(Duration::from_secs(5), || {
        matches!(second.try_wait(), Ok(Some(_)))
    });
    if !got {
        let _ = second.kill();
        panic!("a second `shelbi daemon` should have exited (lock held), but it kept running");
    }
    let status = second.wait().unwrap();
    assert!(
        !status.success(),
        "a second daemon must refuse to start while the lock is held"
    );

    // The original daemon is still up — the failed second start did not disturb it.
    assert!(socket_answers(&home.sock()), "the one daemon survives a losing racer");
}

#[test]
fn daemon_exits_when_the_last_open_project_is_quit() {
    let home = Home::new("idle");
    home.open_project("p");

    let mut ensure = home.spawn(&["__ensure-daemon"]);
    assert!(ensure.wait().expect("wait ensure").success());
    assert!(
        socket_answers(&home.sock()),
        "daemon must be up while a project is open"
    );

    // Quit the last open project: the idle monitor should notice and exit.
    home.quit_project("p");
    assert!(
        wait_until(Duration::from_secs(10), || !socket_answers(&home.sock())),
        "daemon must exit once no project is open"
    );
}

#[test]
fn worker_event_still_lands_over_hub_sock() {
    // The hub.sock NDJSON contract is unchanged: a worker `event` line is
    // accepted and acked exactly as before, independent of the new lifecycle.
    let home = Home::new("event");
    home.open_project("p");
    let mut ensure = home.spawn(&["__ensure-daemon"]);
    assert!(ensure.wait().unwrap().success());
    assert!(wait_until(Duration::from_secs(10), || socket_answers(&home.sock())));

    let mut stream = UnixStream::connect(home.sock()).expect("connect hub.sock");
    stream
        .write_all(b"{\"verb\":\"event\",\"project\":\"p\",\"line\":\"workspace=w note=hi\"}\n")
        .unwrap();
    stream.flush().unwrap();
    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let mut buf = Vec::new();
    use std::io::Read;
    let _ = stream.take(16).read_to_end(&mut buf);
    assert!(
        buf.starts_with(b"ok"),
        "daemon must ack the event line, got {:?}",
        String::from_utf8_lossy(&buf)
    );
}
