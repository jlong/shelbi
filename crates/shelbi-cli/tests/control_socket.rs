//! End-to-end tests for the daemon's **mutation control socket** (Phase 4a of
//! the remove-tmux effort, "The daemon executes mutations").
//!
//! Each test stands up an isolated `SHELBI_HOME` under `/tmp` with a filesystem
//! project and one issue, spawns a real `shelbi daemon` against it (short socket
//! paths — macOS caps Unix-socket paths at ~104 bytes), and drives mutations
//! over the typed control protocol via [`shelbi_client::ControlClient`]. Only
//! `move` mutations are used: they are deterministic (no git, no agent spawn),
//! and the daemon's concurrency guarantees — one mutation per issue at a time,
//! the expected-state gate, finish-if-the-client-leaves, and change
//! notifications — are identical for every mutation kind. The merge/dispatch
//! specifics of approve/reject/start reuse the same machinery and are covered by
//! the orchestrator's own tests.

use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use shelbi_client::ControlClient;
use shelbi_proto::control::{ExpectedState, MutationKind, MutationRequest, Stream};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// A fixed point of parse∘to_rfc3339 for a UTC time, so the string we write into
/// the task frontmatter equals the string the daemon computes back from it.
const T0: &str = "2026-01-01T00:00:00+00:00";

/// An isolated home holding one filesystem project `p` with one `backlog` issue.
struct Home {
    path: PathBuf,
    hub: PathBuf,
    control: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new(tag: &str) -> Self {
        let path = PathBuf::from(format!("/tmp/shb-ctl-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("projects/p/tasks")).unwrap();
        // Minimal filesystem project.
        std::fs::write(
            path.join("projects/p.yaml"),
            "name: p\nrepo: /tmp/p\ndefault_branch: main\n\
             orchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/p\n\
             workspaces:\n  - { name: dev, machine: local, runner: claude }\n",
        )
        .unwrap();
        // Keep the daemon alive (the idle monitor exits when no project is open).
        std::fs::write(path.join("projects/p/state.json"), br#"{"open":true}"#).unwrap();
        let hub = PathBuf::from(format!("/tmp/shb-ctl-{tag}-{}-h.sock", std::process::id()));
        let control = PathBuf::from(format!("/tmp/shb-ctl-{tag}-{}-c.sock", std::process::id()));
        let _ = std::fs::remove_file(&hub);
        let _ = std::fs::remove_file(&control);
        Self {
            path,
            hub,
            control,
            daemon: None,
        }
    }

    /// Write issue `id` in `column` with the fixed `updated_at` [`T0`].
    fn write_issue(&self, id: &str, column: &str) {
        std::fs::write(
            self.path.join(format!("projects/p/tasks/{id}.md")),
            format!(
                "---\nid: {id}\ntitle: {id}\ncolumn: {column}\npriority: 0\n\
                 created_at: {T0}\nupdated_at: {T0}\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    /// The current `column:` value of issue `id` on disk.
    fn column_of(&self, id: &str) -> String {
        let text =
            std::fs::read_to_string(self.path.join(format!("projects/p/tasks/{id}.md"))).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("column:"))
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    }

    fn start_daemon(&mut self) {
        let child = Command::new(BIN)
            .arg("daemon")
            .env("SHELBI_HOME", &self.path)
            .env("SHELBI_HUB_SOCK", &self.hub)
            .env("SHELBI_CONTROL_SOCK", &self.control)
            .env_remove("SHELBI_ROOT")
            // Belt-and-braces: this harness sets a temp SHELBI_HOME but inherits
            // the developer's real $HOME, so a retire would have hit the live
            // ~/Library/LaunchAgents plist. The default-root gate already skips
            // retire here; this makes the hermeticity explicit and local.
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env("SHELBI_DAEMON_IDLE_GRACE_MS", "3000")
            .env("SHELBI_DAEMON_IDLE_POLL_MS", "500")
            .env("SHELBI_YES", "0")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn shelbi daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(10), || self.control.exists()
                && connect(&self.control).is_ok()),
            "control socket never came up"
        );
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(&self.hub);
        let _ = std::fs::remove_file(&self.control);
    }
}

fn connect(sock: &Path) -> Result<ControlClient, shelbi_client::ClientError> {
    // Connect as a current-version client: the daemon now tells a version-
    // mismatched subscriber to re-exec straight away (Phase 4f out-of-date
    // handling), which would pre-empt the change notifications these tests
    // assert. `CARGO_PKG_VERSION` here is the same version the daemon reports.
    ControlClient::connect(sock, env!("CARGO_PKG_VERSION"))
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
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn expected_backlog() -> ExpectedState {
    ExpectedState {
        status: "backlog".into(),
        updated_at: T0.into(),
    }
}

fn move_req(request_id: u64, id: &str, to: &str, expected: Option<ExpectedState>) -> MutationRequest {
    MutationRequest {
        request_id,
        project: "p".into(),
        id: id.into(),
        expected,
        kind: MutationKind::Move {
            to: to.into(),
            reason: Some("user:test".into()),
            skip_transition_actions: false,
        },
    }
}

/// A line sink that discards.
fn discard() -> impl FnMut(Stream, &str) {
    |_s, _t| {}
}

#[test]
fn a_stale_expected_state_is_rejected_and_nothing_changes() {
    let mut home = Home::new("stale");
    home.write_issue("t1", "backlog");
    home.start_daemon();

    let mut client = connect(&home.control).unwrap();
    // Claim we saw the issue in `in-progress` — it is actually in `backlog`.
    let stale = ExpectedState {
        status: "in-progress".into(),
        updated_at: T0.into(),
    };
    let err = client
        .mutate(&move_req(1, "t1", "todo", Some(stale)), &mut discard())
        .unwrap_err();
    match err {
        shelbi_client::ClientError::Mutation(shelbi_proto::control::MutationError::Stale {
            ..
        }) => {}
        other => panic!("expected a stale-state rejection, got {other:?}"),
    }
    // The issue never moved.
    assert_eq!(home.column_of("t1"), "backlog");
}

#[test]
fn two_clients_moving_the_same_issue_one_wins_one_is_stale() {
    let mut home = Home::new("race");
    home.write_issue("t1", "backlog");
    home.start_daemon();

    let control = home.control.clone();
    // Both clients saw the issue in `backlog` at T0 and both try to advance it.
    let handles: Vec<_> = (0..2)
        .map(|i| {
            let control = control.clone();
            std::thread::spawn(move || {
                let mut client = connect(&control).unwrap();
                client.mutate(
                    &move_req(i as u64 + 1, "t1", "todo", Some(expected_backlog())),
                    &mut discard(),
                )
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let oks = results.iter().filter(|r| r.is_ok()).count();
    let stales = results
        .iter()
        .filter(|r| {
            matches!(
                r,
                Err(shelbi_client::ClientError::Mutation(
                    shelbi_proto::control::MutationError::Stale { .. }
                ))
            )
        })
        .count();
    assert_eq!(oks, 1, "exactly one mutation must win: {results:?}");
    assert_eq!(stales, 1, "the loser must be rejected as stale: {results:?}");
    // The winner's single move landed.
    assert_eq!(home.column_of("t1"), "todo");
}

#[test]
fn a_mutation_finishes_after_the_client_disconnects() {
    let mut home = Home::new("leave");
    home.write_issue("t1", "backlog");
    home.start_daemon();

    // Send the request, then drop the client immediately without reading the
    // reply. The daemon runs the job on a detached thread, so the move must
    // still land.
    {
        let mut raw = UnixStream::connect(&home.control).unwrap();
        // hello
        raw.write_all(
            &shelbi_proto::control::encode(&shelbi_proto::control::ClientMsg::Hello {
                protocol: shelbi_proto::control::CONTROL_PROTOCOL_VERSION,
                client_version: "test".into(),
            })
            .unwrap(),
        )
        .unwrap();
        // mutate
        raw.write_all(
            &shelbi_proto::control::encode(&shelbi_proto::control::ClientMsg::Mutate(move_req(
                1,
                "t1",
                "todo",
                Some(expected_backlog()),
            )))
            .unwrap(),
        )
        .unwrap();
        raw.flush().unwrap();
        // Leave at once — do not read the reply.
        let _ = raw.shutdown(Shutdown::Both);
    }

    assert!(
        wait_until(Duration::from_secs(5), || home.column_of("t1") == "todo"),
        "the move must complete even though the client left (got `{}`)",
        home.column_of("t1")
    );
}

#[test]
fn other_connected_clients_are_notified_of_a_change() {
    let mut home = Home::new("notify");
    home.write_issue("t1", "backlog");
    home.start_daemon();

    // Subscriber connection.
    let sub_client = connect(&home.control).unwrap();
    let mut sub = sub_client.subscribe().unwrap();

    // A different client performs a move.
    let mut mover = connect(&home.control).unwrap();
    mover
        .mutate(
            &move_req(1, "t1", "todo", Some(expected_backlog())),
            &mut discard(),
        )
        .unwrap();

    // The subscriber is told about it.
    let notice = sub
        .recv()
        .expect("subscription read")
        .expect("a change notification");
    let note = match notice {
        shelbi_client::Notice::Changed(note) => note,
        other => panic!("expected a change notification, got {other:?}"),
    };
    assert_eq!(note.id, "t1");
    assert_eq!(note.verb, "move");
    assert_eq!(note.status, "todo");
}
