//! End-to-end tests for the session-process daemon watchdog (Phase 3 of the
//! remove-tmux effort — "Sessions restart the daemon").
//!
//! With the launchd/systemd units retired, each `shelbi __session` process is
//! what brings a crashed hub daemon back while its project is open. These tests
//! drive the real `shelbi __session` binary (`CARGO_BIN_EXE_shelbi`) with the
//! watchdog interval driven fast, and point the watchdog at the test binary via
//! `$SHELBI_BIN` so a restart comes up on the installed-binary path rather than
//! the session's own `current_exe`.
//!
//! Each test runs against its own short `SHELBI_HOME` under `/tmp` (macOS caps
//! Unix-socket paths at ~104 bytes), fully isolated from any real daemon.

use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// The per-project state subdir under a shelbi home. A const rather than an
/// inline `.join("projects")` because the callsite-scan lint flags the literal
/// (production code must route through `shelbi_state::projects_dir()`, which this
/// isolated-home fixture can't use — it writes a *child's* home layout).
const PROJECTS: &str = "projects";

/// A unique, short `SHELBI_HOME` for one test, with its sessions, removed on drop.
struct Home {
    path: PathBuf,
    sessions: Vec<Child>,
}

impl Home {
    fn new(tag: &str) -> Self {
        let path = PathBuf::from(format!("/tmp/shb-srd-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(PROJECTS)).unwrap();
        Self {
            path,
            sessions: Vec::new(),
        }
    }

    fn sock(&self) -> PathBuf {
        self.path.join("hub.sock")
    }

    /// Register a project and mark it open in `state.json`.
    fn open_project(&self, name: &str) {
        std::fs::write(
            self.path.join(PROJECTS).join(format!("{name}.yaml")),
            b"",
        )
        .unwrap();
        let dir = self.path.join(PROJECTS).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), br#"{"open":true}"#).unwrap();
    }

    /// Clear a project's open flag — the on-disk effect of quitting it.
    fn quit_project(&self, name: &str) {
        let dir = self.path.join(PROJECTS).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("state.json"), br#"{"open":false}"#).unwrap();
    }

    /// Launch a real detached-style `shelbi __session` whose project is
    /// `project`, running a long-lived `sleep` child in its PTY, with the
    /// watchdog driven fast and pointed at the test binary. The handle is
    /// retained so the session is torn down with the home.
    fn start_session(&mut self, id: &str, project: &str) {
        let child = Command::new(BIN)
            .args([
                "__session",
                "--id",
                id,
                "--name",
                &format!("{project}/orch"),
                "--cwd",
                self.path.to_str().unwrap(),
                "--cols",
                "80",
                "--rows",
                "24",
                "--",
                "/bin/sh",
                "-c",
                "exec sleep 300",
            ])
            .env("SHELBI_HOME", &self.path)
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_HUB_SOCK")
            // Resolve the daemon from the test binary, not this session image.
            .env("SHELBI_BIN", BIN)
            // Drive the watchdog loop fast so a restart lands in the test window.
            .env("SHELBI_DAEMON_WATCH_INTERVAL_MS", "150")
            .env("SHELBI_DAEMON_WATCH_JITTER_MS", "50")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn shelbi __session");
        self.sessions.push(child);
    }

    /// The daemon's recorded pid, if a pid file exists.
    fn daemon_pid(&self) -> Option<i32> {
        let text = std::fs::read_to_string(self.path.join("shelbi.pid")).ok()?;
        // The pid file is a single line "<pid> <start> <version>"; the pid is
        // the first whitespace-separated field.
        text.split_whitespace().next()?.parse().ok()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Stop the watchdogs first (so none re-spawns the daemon we're reaping),
        // then let the daemon idle-exit, then remove the dir.
        for mut child in self.sessions.drain(..) {
            let pid = child.id() as i32;
            // SIGTERM lets the session kill its own child group and exit cleanly;
            // SIGKILL is the fallback. Never a group kill — the session shares the
            // test runner's group when launched directly here.
            unsafe { libc::kill(pid, libc::SIGTERM) };
            if wait_until(Duration::from_secs(3), || {
                matches!(child.try_wait(), Ok(Some(_)))
            }) {
                let _ = child.wait();
            } else {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
        if let Some(pid) = self.daemon_pid() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
        let _ = wait_until(Duration::from_secs(5), || !socket_answers(&self.sock()));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// Does a hello probe to `sock` get an answer? Mirrors
/// `shelbi_state::probe_daemon_hello` without touching this process's env.
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
fn a_session_restarts_the_daemon_when_its_project_is_open() {
    let mut home = Home::new("restart");
    home.open_project("p");

    // No daemon yet; the session's watchdog brings one up because the project is
    // open and the lock is free.
    assert!(!socket_answers(&home.sock()), "no daemon before any session");
    home.start_session("srd1", "p");
    assert!(
        wait_until(Duration::from_secs(25), || socket_answers(&home.sock())),
        "the session watchdog must start a daemon for an open project"
    );

    // Kill it hard, as a crash would. The lock dies with the holder.
    let pid = home.daemon_pid().expect("a running daemon has a pid file");
    unsafe { libc::kill(pid, libc::SIGKILL) };
    assert!(
        wait_until(Duration::from_secs(5), || !socket_answers(&home.sock())),
        "the daemon must be gone after a SIGKILL"
    );

    // The watchdog notices the free lock on its next tick and restarts it.
    assert!(
        wait_until(Duration::from_secs(25), || socket_answers(&home.sock())),
        "the session watchdog must restart the crashed daemon within a few intervals"
    );
}

#[test]
fn several_sessions_converge_on_exactly_one_daemon() {
    let mut home = Home::new("race");
    home.open_project("p");

    // Several sessions each run a watchdog and may all try to start at once.
    for i in 0..4 {
        home.start_session(&format!("srd-r{i}"), "p");
    }
    assert!(
        wait_until(Duration::from_secs(25), || socket_answers(&home.sock())),
        "the racing watchdogs must bring up a daemon"
    );

    // Exactly one: a foreground `shelbi daemon` must fail fast because the
    // single-instance lock is held (the losing racers did the same and exited).
    let mut second = Command::new(BIN)
        .arg("daemon")
        .env("SHELBI_HOME", &home.path)
        .env_remove("SHELBI_ROOT")
        .env_remove("SHELBI_HUB_SOCK")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn second daemon");
    let exited = wait_until(Duration::from_secs(5), || {
        matches!(second.try_wait(), Ok(Some(_)))
    });
    if !exited {
        let _ = second.kill();
        panic!("a second `shelbi daemon` should have exited (lock held), but it kept running");
    }
    assert!(
        !second.wait().unwrap().success(),
        "a second daemon must refuse to start while the one daemon holds the lock"
    );
    assert!(
        socket_answers(&home.sock()),
        "the one daemon survives the losing foreground start"
    );
}

#[test]
fn a_closed_projects_session_never_starts_the_daemon() {
    let mut home = Home::new("closed");
    // Register the project but leave it closed (the on-disk effect of quit).
    home.quit_project("p");

    home.start_session("srd-c", "p");

    // Across many watchdog intervals (150ms each), no daemon must ever appear.
    assert!(
        !wait_until(Duration::from_secs(3), || socket_answers(&home.sock())),
        "a session whose project is closed must never start the daemon"
    );
}
