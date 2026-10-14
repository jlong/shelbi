//! End-to-end tests for the daemon's **mutation control socket** (Phase 4a of
//! the remove-tmux effort, "The daemon executes mutations").
//!
//! Each test stands up an isolated `SHELBI_HOME` under `/tmp` with a filesystem
//! project and one issue, spawns a real `shelbi daemon` against it (short socket
//! paths — macOS caps Unix-socket paths at ~104 bytes), and drives mutations
//! over the typed control protocol via [`shelbi_client::ControlClient`]. Only
//! `move` and `prioritize` mutations are used: they are deterministic (no git,
//! no agent spawn),
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
        self.write_issue_at(id, column, 0);
    }

    /// [`Self::write_issue`] at `priority`.
    fn write_issue_at(&self, id: &str, column: &str, priority: u32) {
        std::fs::write(
            self.path.join(format!("projects/p/tasks/{id}.md")),
            format!(
                "---\nid: {id}\ntitle: {id}\ncolumn: {column}\npriority: {priority}\n\
                 created_at: {T0}\nupdated_at: {T0}\n---\nbody\n"
            ),
        )
        .unwrap();
    }

    /// The current `priority:` value of issue `id` on disk.
    fn priority_of(&self, id: &str) -> u32 {
        let text =
            std::fs::read_to_string(self.path.join(format!("projects/p/tasks/{id}.md"))).unwrap();
        text.lines()
            .find_map(|l| l.strip_prefix("priority:"))
            .and_then(|v| v.trim().parse().ok())
            .unwrap()
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
        self.start_daemon_with(&[]);
    }

    /// Start the daemon with extra environment (`(name, value)` pairs).
    fn start_daemon_with(&mut self, env: &[(&str, &str)]) {
        let child = Command::new(BIN)
            .envs(env.iter().copied())
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

/// The board's refusal path end to end (`dt-issues-board`): a client renders a
/// card, the CLI moves that issue before the client acts, and the client's
/// move, sent with the state its card was read at (`IssueCard::expected`, what
/// `execute_mutation_expecting` sends), is refused as stale and changes
/// nothing; re-reading the board shows where the CLI put it.
#[test]
fn a_board_move_from_a_stale_card_is_refused_after_a_cli_move() {
    let mut home = Home::new("board");
    home.write_issue("t1", "backlog");
    home.start_daemon();

    // The client's read: the board model built from the issue as it is now.
    let read_card = |home: &Home| {
        let text =
            std::fs::read_to_string(home.path.join("projects/p/tasks/t1.md")).unwrap();
        let input = shelbi_app::board::BoardInput {
            open: vec![shelbi_state::parse_task_file(&text).unwrap()],
            loading: false,
            closed_loaded: true,
            ..Default::default()
        };
        let model =
            shelbi_app::IssuesModel::build(&input, &shelbi_app::BoardFilters::default());
        model.card("t1").cloned().unwrap()
    };
    let card = read_card(&home);
    assert_eq!(card.status_id, "backlog");

    // Between the read and the move, the CLI moves the issue.
    let status = Command::new(BIN)
        .args(["issue", "move", "t1", "--to", "todo", "-p", "p"])
        .env("SHELBI_HOME", &home.path)
        .env("SHELBI_HUB_SOCK", &home.hub)
        .env("SHELBI_CONTROL_SOCK", &home.control)
        .env_remove("SHELBI_ROOT")
        .env("SHELBI_YES", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success(), "the CLI move ran");
    assert_eq!(home.column_of("t1"), "todo");

    // The client now moves its (stale) card, sending the state the card was
    // read at, exactly what the desktop board puts on its mutation.
    let mut client = connect(&home.control).unwrap();
    let err = client
        .mutate(&move_req(1, "t1", "in-progress", Some(card.expected())), &mut discard())
        .unwrap_err();
    match err {
        shelbi_client::ClientError::Mutation(shelbi_proto::control::MutationError::Stale {
            actual,
            ..
        }) => assert_eq!(actual.status, "todo", "the refusal names the real state"),
        other => panic!("expected the daemon to refuse the stale card, got {other:?}"),
    }
    // Nothing changed; the board's re-read shows the CLI's move.
    assert_eq!(home.column_of("t1"), "todo");
    assert_eq!(read_card(&home).status_id, "todo");
}

#[test]
fn a_prioritize_reorders_the_column_and_a_stale_sibling_reorder_is_refused() {
    let mut home = Home::new("prio");
    for (i, id) in ["a", "b", "c"].iter().enumerate() {
        home.write_issue_at(id, "todo", i as u32);
    }
    home.start_daemon();
    let at_t0 = ExpectedState {
        status: "todo".into(),
        updated_at: T0.into(),
    };
    let prio_req = |request_id, id: &str, position| MutationRequest {
        request_id,
        project: "p".into(),
        id: id.into(),
        expected: Some(at_t0.clone()),
        kind: MutationKind::Prioritize { position },
    };

    // `c` to the top: the board's drag sends the registry's PrioritizeIssue as
    // this request, carrying the card's read state.
    let mut client = connect(&home.control).unwrap();
    client.mutate(&prio_req(1, "c", 0), &mut discard()).unwrap();
    assert_eq!(
        ["c", "a", "b"].map(|id| home.priority_of(id)),
        [0, 1, 2],
        "c landed on top and the rest shifted down"
    );

    // The renumber rewrote `b`'s revision, so a reorder sent from the old
    // snapshot is refused rather than applied to a column the user didn't see.
    let err = client.mutate(&prio_req(2, "b", 0), &mut discard()).unwrap_err();
    assert!(
        matches!(
            err,
            shelbi_client::ClientError::Mutation(shelbi_proto::control::MutationError::Stale { .. })
        ),
        "expected a stale refusal, got {err:?}"
    );
    assert_eq!(home.priority_of("b"), 2, "nothing changed");
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

/// Kill every live session under `home`'s sessions directory (the daemon spawns
/// them detached, so they outlive it).
fn kill_sessions(home: &Path) {
    let Ok(sessions) = shelbi_client::list(&home.join("sessions")) else {
        return;
    };
    for s in sessions.iter().filter(|s| s.alive) {
        if let Ok((conn, _)) = shelbi_client::Connection::open(&s.sock, None, &[]) {
            let _ = conn.kill(None);
        }
    }
}

/// `dt-workspace-view`: the daemon starts a workspace's user shell on request
/// (`<project>/ws/<workspace>/shell`, a login shell in the worktree), reuses it
/// when asked again, and ends it on `CloseShell`, leaving the agent's slot
/// alone.
#[test]
fn the_daemon_starts_and_ends_a_workspaces_user_shell() {
    use shelbi_proto::control::{WorkspaceSessionOp, WorkspaceSessionRequest};

    let mut home = Home::new("shell");
    let work_dir = home.path.join("w");
    let worktree = work_dir.join(".shelbi/wt/dev");
    std::fs::create_dir_all(&worktree).unwrap();
    std::fs::write(
        home.path.join("projects/p.yaml"),
        format!(
            "name: p\nrepo: /tmp/p\ndefault_branch: main\n\
             orchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n  - name: local\n    kind: local\n    work_dir: {}\n\
             workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n",
            work_dir.display()
        ),
    )
    .unwrap();
    // A plain `sh` as the login shell, so the test doesn't run the developer's.
    home.start_daemon_with(&[("SHELL", "/bin/sh")]);
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            kill_sessions(&self.0);
        }
    }
    let _cleanup = Cleanup(home.path.clone());

    let req = |request_id, op| WorkspaceSessionRequest {
        request_id,
        project: "p".into(),
        workspace: "dev".into(),
        op,
    };
    let live_shells = |home: &Path| {
        shelbi_client::list(&home.join("sessions"))
            .map(|l| {
                l.into_iter()
                    .filter(|s| s.alive && s.meta.name == "p/ws/dev/shell")
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    };

    let mut client = connect(&home.control).unwrap();
    client
        .workspace_session(&req(1, WorkspaceSessionOp::EnsureShell), &mut discard())
        .expect("ensure the shell");
    assert!(
        wait_until(Duration::from_secs(10), || live_shells(&home.path).len() == 1),
        "the shell session never came up"
    );
    // Asking again reuses it.
    client
        .workspace_session(&req(2, WorkspaceSessionOp::EnsureShell), &mut discard())
        .unwrap();
    assert_eq!(live_shells(&home.path).len(), 1, "an ensure is idempotent");

    // It is a shell in the worktree.
    let shell = live_shells(&home.path).remove(0);
    // Its socket binds a beat after the session is listed.
    let mut opened = None;
    assert!(wait_until(Duration::from_secs(10), || {
        opened = shelbi_client::Connection::open(&shell.sock, None, shelbi_proto::capability::ALL).ok();
        opened.is_some()
    }));
    let (conn, events) = opened.unwrap();
    conn.attach(None).unwrap();
    conn.input(b"pwd\r").unwrap();
    let want = worktree.canonicalize().unwrap().display().to_string();
    let rx = events.into_inner();
    let mut seen = Vec::new();
    let found = wait_until(Duration::from_secs(10), || {
        while let Ok(ev) = rx.try_recv() {
            if let shelbi_client::SessionEvent::Output { data, .. }
            | shelbi_client::SessionEvent::Resync { replay: data, .. } = ev
            {
                seen.extend_from_slice(&data);
            }
        }
        String::from_utf8_lossy(&seen).contains(&want)
    });
    assert!(found, "pwd in the shell names the worktree: {:?}", String::from_utf8_lossy(&seen));
    drop(conn);

    client
        .workspace_session(&req(3, WorkspaceSessionOp::CloseShell), &mut discard())
        .expect("close the shell");
    assert!(
        wait_until(Duration::from_secs(10), || live_shells(&home.path).is_empty()),
        "the shell session ended"
    );
    // Closing an already-closed shell is fine.
    client
        .workspace_session(&req(4, WorkspaceSessionOp::CloseShell), &mut discard())
        .unwrap();
}

