//! The lost-login-session re-exec, end to end with real daemon processes: a
//! daemon that loses its session launches a successor and stands down, and the
//! successor has to outwait the predecessor's drain for the bind lock before it
//! can serve.
//!
//! `SHELBI_SESSION_HEALTH_TEST_PROBE_FILE` stands in for the two things a test
//! can't do for real: lose a macOS login session (the file holds the probe's
//! answer) and run `launchctl asuser` (the successor is launched directly, with
//! the same arguments). Everything else is the production path, on every OS.
//!
//! Each test has its own short `SHELBI_HOME` under `/tmp` (macOS caps Unix-socket
//! paths at ~104 bytes). No test signals a process: daemons exit through the idle
//! monitor when `Home` drops and clears the open project.

use std::io::{BufRead, BufReader};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");
const PROJECTS: &str = "projects";

struct Home {
    path: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Self {
        let path = PathBuf::from(format!("/tmp/shb-slt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join(PROJECTS)).unwrap();
        // An open project keeps each generation from idling out mid-test.
        std::fs::write(path.join(PROJECTS).join("p.yaml"), b"").unwrap();
        let project = path.join(PROJECTS).join("p");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("state.json"), br#"{"open":true}"#).unwrap();
        Self { path }
    }

    fn sock(&self) -> PathBuf {
        self.path.join("hub.sock")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .env("SHELBI_HOME", &self.path)
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("SHELBI_CONTROL_SOCK")
            .env_remove("SHELBI_HUB_ADDR")
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env("SHELBI_DAEMON_IDLE_GRACE_MS", "400")
            .env("SHELBI_DAEMON_IDLE_POLL_MS", "200")
            .env("SHELBI_YES", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd
    }

    /// Start a daemon whose session health is read from the probe file, starting
    /// healthy, and wait for it to serve. Its successor inherits all of this.
    fn start_monitored_daemon(&self) -> Child {
        self.set_probe("healthy");
        let child = self
            .command(&["daemon"])
            .env("SHELBI_SESSION_HEALTH_TEST_PROBE_FILE", self.path.join("probe"))
            .env("SHELBI_SESSION_HEALTH_INTERVAL_MS", "50")
            .env("SHELBI_SESSION_HEALTH_THRESHOLD", "2")
            .spawn()
            .expect("spawn daemon");
        assert!(
            wait_until(Duration::from_secs(20), || socket_answers(&self.sock())),
            "first daemon must serve"
        );
        child
    }

    /// Write the probe file atomically, so a reader never sees it torn.
    fn set_probe(&self, verdict: &str) {
        let tmp = self.path.join("probe.tmp");
        std::fs::write(&tmp, verdict).unwrap();
        std::fs::rename(&tmp, self.path.join("probe")).unwrap();
    }

    fn pid_on_file(&self) -> Option<u32> {
        std::fs::read_to_string(self.path.join("shelbi.pid"))
            .ok()?
            .split_whitespace()
            .next()?
            .parse()
            .ok()
    }

    /// `not-attempted` / `attempted` / `gave-up`, or `None` with no marker.
    fn marker_recovery(&self) -> Option<String> {
        let text = std::fs::read_to_string(self.path.join("daemon-session-lost.json")).ok()?;
        let record: serde_json::Value = serde_json::from_str(&text).ok()?;
        record["recovery"].as_str().map(str::to_string)
    }

    fn marker_says(&self, recovery: &str) -> bool {
        self.marker_recovery().as_deref() == Some(recovery)
    }

    fn event_count(&self, action: &str) -> usize {
        std::fs::read_to_string(self.path.join("events.log"))
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(&format!(" daemon {action} ")))
            .count()
    }

    fn wait_for_successor_of(&self, predecessor: &Child) -> u32 {
        let first = predecessor.id();
        let mut successor = None;
        assert!(
            wait_until(Duration::from_secs(25), || {
                successor = self.pid_on_file().filter(|pid| *pid != first);
                successor.is_some() && socket_answers(&self.sock())
            }),
            "a successor must end up serving; marker={:?}",
            self.marker_recovery()
        );
        successor.unwrap()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let project = self.path.join(PROJECTS).join("p");
        let _ = std::fs::write(project.join("state.json"), br#"{"open":false}"#);
        let _ = wait_until(Duration::from_secs(5), || !socket_answers(&self.sock()));
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A daemon answers an empty, write-closed connection with its hello line.
fn socket_answers(sock: &Path) -> bool {
    let Ok(stream) = UnixStream::connect(sock) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.shutdown(Shutdown::Write);
    let mut line = Vec::new();
    match BufReader::new(&stream).read_until(b'\n', &mut line) {
        Ok(n) => n > 0 && line.windows(7).any(|w| w == b"version"),
        Err(_) => false,
    }
}

fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// The race the takeover exists for. An attached client keeps the first daemon in
/// its drain, holding the bind lock, for seconds after the successor is launched.
#[test]
fn a_successor_outlasts_the_drain_serves_and_clears_the_marker() {
    let home = Home::new("recover");
    let mut first = home.start_monitored_daemon();
    let first_pid = first.id();

    // An idle client the drain has to wait out, as an attached TUI would be.
    let client = UnixStream::connect(home.sock()).unwrap();

    home.set_probe("lost");
    assert!(
        wait_until(Duration::from_secs(10), || home.marker_says("attempted")),
        "the first daemon must declare the loss and record its attempt"
    );
    // The live session the re-exec lands in. The successor can't read this yet:
    // it has no lock, and so no monitor, until the first daemon is gone.
    home.set_probe("healthy");
    assert!(
        wait_until(Duration::from_secs(5), || {
            home.event_count("session-recovery-attempting") == 1
        }),
        "the successor must have launched"
    );
    assert_eq!(
        home.pid_on_file(),
        Some(first_pid),
        "the successor must not have got past the first daemon's lock"
    );

    let second_pid = home.wait_for_successor_of(&first);
    assert_ne!(second_pid, first_pid);
    assert!(
        first.try_wait().expect("try_wait").is_some(),
        "the first daemon has stood down"
    );

    assert!(
        wait_until(Duration::from_secs(10), || home.marker_recovery().is_none()),
        "a healthy successor settles the marker; it says {:?}",
        home.marker_recovery()
    );
    assert_eq!(home.event_count("session-recovered"), 1);
    assert_eq!(home.event_count("session-recovery-gave-up"), 0);
    drop(client);
}

/// The escalation: a re-exec that lands in a session just as lost ends at gave-up
/// with the new daemon still serving, and doesn't re-exec again.
#[test]
fn a_successor_still_lost_gives_up_and_keeps_serving() {
    let home = Home::new("gaveup");
    let mut first = home.start_monitored_daemon();

    home.set_probe("lost");
    let second_pid = home.wait_for_successor_of(&first);
    assert!(first.try_wait().expect("try_wait").is_some());

    assert!(
        wait_until(Duration::from_secs(10), || home.marker_says("gave-up")),
        "a still-lost successor escalates; the marker says {:?}",
        home.marker_recovery()
    );
    assert_eq!(home.event_count("session-recovery-gave-up"), 1);

    // Many ticks later: same daemon, one attempt, no loop.
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(home.pid_on_file(), Some(second_pid));
    assert!(socket_answers(&home.sock()));
    assert_eq!(home.event_count("session-recovery-attempting"), 1);
    assert_eq!(home.event_count("session-recovery-gave-up"), 1);

    // The user logs back in properly: the same daemon sees it and clears up.
    home.set_probe("healthy");
    assert!(
        wait_until(Duration::from_secs(10), || home.marker_recovery().is_none()),
        "a healthy reading clears even a gave-up marker"
    );
}

/// A predecessor that never lets go: the successor stops at its bound, closes out
/// the attempt, and leaves the daemon that holds the lock alone.
#[test]
fn a_successor_that_cannot_get_the_lock_gives_up_the_attempt() {
    let home = Home::new("wedged");
    let mut holder = home.command(&["daemon"]).spawn().expect("spawn daemon");
    assert!(
        wait_until(Duration::from_secs(20), || socket_answers(&home.sock())),
        "the holder must serve"
    );
    std::fs::write(
        home.path.join("daemon-session-lost.json"),
        br#"{"reason":"test","detected_at":"2026-11-05T00:00:00Z","recovery":"attempted"}"#,
    )
    .unwrap();

    let status = home
        .command(&["daemon", "run", "--takeover"])
        .env("SHELBI_DAEMON_TAKEOVER_WAIT_MS", "300")
        .status()
        .expect("run successor");

    assert!(!status.success(), "it could not take the lock");
    assert!(home.marker_says("gave-up"), "{:?}", home.marker_recovery());
    assert_eq!(home.event_count("session-recovery-failed"), 1);
    assert!(holder.try_wait().expect("try_wait").is_none(), "holder untouched");
    assert!(socket_answers(&home.sock()));
}

/// Without `--takeover` a daemon still refuses a held lock after only its short
/// probe retry: every client racing to start a daemon relies on that to fail fast.
#[test]
fn a_plain_daemon_still_refuses_a_held_lock_immediately() {
    let home = Home::new("plain");
    let mut holder = home.command(&["daemon"]).spawn().expect("spawn daemon");
    assert!(wait_until(Duration::from_secs(20), || socket_answers(&home.sock())));

    let start = Instant::now();
    let status = home.command(&["daemon"]).status().expect("run second daemon");
    assert!(!status.success());
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "took {:?}",
        start.elapsed()
    );
    assert!(holder.try_wait().expect("try_wait").is_none(), "holder untouched");
}
