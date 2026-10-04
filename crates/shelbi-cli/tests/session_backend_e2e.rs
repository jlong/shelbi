//! A local dispatch-to-handoff cycle driven entirely through the **session
//! backend** (`rt-backend-sessions`), against a real `shelbi __session` process
//! running a stub agent.
//!
//! This is the Phase 2 acceptance: with the hidden dev flag on, the orchestrator
//! spawns a worker as a detached session, detects ready/busy through the
//! backend's `snapshot` + `title`, delivers a message through the same seam
//! `submit::deliver_text` uses, and observes the review-ready marker the agent
//! writes — the full cycle a tmux dispatch runs, on the session backend.
//!
//! The session is spawned with the real binary (`CARGO_BIN_EXE_shelbi` via
//! `spawn_with_exe`) so a genuine `shelbi __session` owns the PTY and emulator;
//! every subsequent operation (probe, snapshot, title, deliver, kill) goes
//! through `session_backend::backend()`, which the `SHELBI_SESSION_BACKEND=1`
//! flag points at the session-process backend. Production `spawn_local_pane`
//! finds the binary via `current_exe` (covered by the `to_session_spawn_spec`
//! unit test); a cargo test's `current_exe` is the harness, so the e2e uses the
//! explicit-exe spawn to run the real session binary.

#![cfg(unix)]

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use shelbi_core::{Host, TmuxAddr};
use shelbi_orchestrator::ready;
use shelbi_orchestrator::session_backend::{backend, Liveness, SessionBackend, SessionTarget};
use shelbi_session::SpawnSpec;
use shelbi_state::{parse_pane_title_marker, PaneMarker};

/// These tests mutate process-global env (`SHELBI_HOME`, `SHELBI_SESSION_BACKEND`,
/// `SHELL`), so they serialize against each other through this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

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
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn local_dispatch_to_handoff_cycle_on_the_session_backend() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let marker = home.path().join("review-ready.marker");

    // Point the whole process at this isolated home and turn the hidden dev flag
    // on, so `backend()` resolves to the session-process backend and
    // `sessions_dir()` lands under the temp home.
    std::env::set_var("SHELBI_HOME", home.path());
    std::env::set_var("SHELBI_SESSION_BACKEND", "1");

    // The worker addressed the way the orchestrator addresses a local workspace:
    // a window named `alice` inside the project session `shelbi-demo`. The
    // session backend derives the logical name `demo/ws/alice` from this.
    let addr = TmuxAddr {
        session: "shelbi-demo".into(),
        window: "alice".into(),
    };
    let target = SessionTarget::from_tmux_addr(&addr);

    // Before dispatch, nothing is bound to the slot.
    assert_eq!(backend().probe(&Host::Local, &target, None), Liveness::Dead);

    // A stub agent: it draws a Claude-like ready input box (`? for shortcuts`,
    // which `is_input_ready` matches) and sets the `shelbi:idle` OSC title the
    // worker hooks would, then blocks for a line. On receiving the delivered
    // message it flips its title to `shelbi:review` and writes the review-ready
    // marker — the handoff signal — before idling on `cat` so it stays live.
    let stub = format!(
        "printf '? for shortcuts\\n'; \
         printf '\\033]2;shelbi:idle\\007'; \
         IFS= read -r line; \
         printf '\\033]2;shelbi:review\\007'; \
         printf '%s\\n' \"$line\" > {marker}; \
         printf 'REVIEW-READY\\n'; \
         exec cat",
        marker = marker.display(),
    );

    // Spawn the worker as a real detached session (the dispatch), named exactly
    // as the backend's `spawn_local_pane` would name it.
    let spec = SpawnSpec {
        name: "demo/ws/alice".into(),
        cwd: cwd.path().to_path_buf(),
        cols: 80,
        rows: 24,
        task: Some("t-42".into()),
        raw_output_log: false,
        child_argv: vec!["/bin/sh".into(), "-c".into(), stub],
    };
    let spawned = shelbi_client::spawn_with_exe(Path::new(env!("CARGO_BIN_EXE_shelbi")), &spec)
        .expect("spawn the worker session");

    // Cleanup: kill the session even if an assertion panics, then clear env.
    struct Guard<'a> {
        target: &'a SessionTarget,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = backend().kill(&Host::Local, self.target);
            std::env::remove_var("SHELBI_SESSION_BACKEND");
            std::env::remove_var("SHELBI_HOME");
        }
    }
    let _guard = Guard { target: &target };

    // The agent starts: the backend's probe reports it Alive.
    let alive = wait_for(Duration::from_secs(10), || {
        matches!(backend().probe(&Host::Local, &target, None), Liveness::Alive).then_some(())
    });
    assert!(alive.is_some(), "the worker session should come up Alive");

    // Ready is detected through the backend's snapshot (the ready detector reads
    // the session's `capture-pane -p -J`-shaped screen) AND its title marker.
    let ready_snap = wait_for(Duration::from_secs(10), || {
        let screen = backend().snapshot(&Host::Local, &target).unwrap_or_default();
        ready::is_input_ready(&screen).then_some(screen)
    });
    assert!(
        ready_snap.is_some(),
        "is_input_ready should fire on the session snapshot; last screen: {:?}",
        backend().snapshot(&Host::Local, &target)
    );
    let idle = wait_for(Duration::from_secs(5), || {
        let title = backend().title(&Host::Local, &target).unwrap_or_default();
        parse_pane_title_marker(&title)
    });
    assert_eq!(
        idle,
        Some(PaneMarker::Idle),
        "the session title should carry the `shelbi:idle` ready marker"
    );

    // A message is delivered through the same path `submit` uses — the backend's
    // injection lock + paste + Enter. `spawned.id` keeps the session reachable
    // for the cleanup guard even though we address by target.
    let _ = &spawned.id;
    shelbi_orchestrator::submit::deliver_text(&Host::Local, &addr, "please review")
        .expect("deliver the handoff message through the session backend");

    // The handoff: the agent wrote its review-ready marker, and its title now
    // reads `shelbi:review`. Observing the marker is the dispatch-to-handoff
    // cycle completing on the session backend.
    let delivered = wait_for(Duration::from_secs(10), || {
        std::fs::read_to_string(&marker).ok()
    });
    assert_eq!(
        delivered.as_deref().map(str::trim),
        Some("please review"),
        "the agent should receive the delivered message and write the review marker"
    );
    let review = wait_for(Duration::from_secs(5), || {
        let title = backend().title(&Host::Local, &target).unwrap_or_default();
        (parse_pane_title_marker(&title) == Some(PaneMarker::Review)).then_some(())
    });
    assert!(
        review.is_some(),
        "the session title should flip to the `shelbi:review` handoff marker"
    );
}

/// AC4: with the dev flag on, the **orchestrator** runs as a session process —
/// not a tmux pane — so it has no `$TMUX_PANE` in its environment (the session
/// scrubs the tmux terminal vars and owns the PTY directly), and a steer sent
/// through the backend reaches it **exactly once** (the PTY slave is its only
/// stdin; there is no duplicated / teed pane stdin to double-deliver).
///
/// Driven through the real `orchestrator_session_spec` — the same spec builder
/// `ensure_dashboard`'s session branch uses — with a stub standing in for the
/// Codex bridge, so the dup-free, `$TMUX_PANE`-free command shape is exercised
/// end to end against a genuine `shelbi __session`.
#[test]
fn orchestrator_runs_as_a_session_without_tmux_pane_and_delivers_input_once() {
    use std::os::unix::fs::PermissionsExt;

    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let tmux_pane_file = home.path().join("tmux_pane.txt");
    let delivered_file = home.path().join("delivered.txt");

    std::env::set_var("SHELBI_HOME", home.path());
    std::env::set_var("SHELBI_SESSION_BACKEND", "1");
    // A deterministic login shell for `orchestrator_session_spec`'s `$SHELL -lc`.
    std::env::set_var("SHELL", "/bin/sh");

    // A stub standing in for the Codex orchestrator bridge: on startup it records
    // what `$TMUX_PANE` is (expected: unset — the session scrubbed it), paints a
    // ready input box + the `shelbi:idle` title the worker hooks would, then
    // appends every stdin line it reads to a file so the test can count how many
    // times a single delivered steer arrives.
    let stub = cwd.path().join("stub-codex.sh");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"${{TMUX_PANE:-unset}}\" > {pane}\n\
             printf '? for shortcuts\\n'\n\
             printf '\\033]2;shelbi:idle\\007'\n\
             while IFS= read -r line; do printf '%s\\n' \"$line\" >> {deliv}; done\n",
            pane = tmux_pane_file.display(),
            deliv = delivered_file.display(),
        ),
    )
    .unwrap();
    std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).unwrap();

    // Build the real orchestrator session spec, with the stub as the launch
    // command. `exec {launch}` runs the stub as the PTY's foreground child.
    let launch = format!("'{}'", stub.display());
    let spec =
        shelbi_orchestrator::orchestrator_session_spec("demo", "shelbi-demo", cwd.path(), &launch);
    // The spec is named exactly what `SessionTarget::session("shelbi-demo")`
    // resolves to, and carries neither the stdin dup nor `$TMUX_PANE`.
    assert_eq!(spec.name, "demo/orch");
    let body = spec.child_argv.last().unwrap();
    assert!(!body.contains("TMUX_PANE"), "spec must not read $TMUX_PANE: {body}");
    assert!(
        !body.contains("3<&0") && !body.contains("<&3"),
        "spec must not dup pane stdin: {body}"
    );

    let spawned = shelbi_client::spawn_with_exe(Path::new(env!("CARGO_BIN_EXE_shelbi")), &spec)
        .expect("spawn the orchestrator session");

    // The orchestrator is addressed as a whole session (`<project>/orch`).
    let target = SessionTarget::session("shelbi-demo");

    struct Guard<'a> {
        target: &'a SessionTarget,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = backend().kill(&Host::Local, self.target);
            std::env::remove_var("SHELBI_SESSION_BACKEND");
            std::env::remove_var("SHELBI_HOME");
            std::env::remove_var("SHELL");
        }
    }
    let _guard = Guard { target: &target };
    let _ = &spawned.id;

    // It comes up Alive.
    let alive = wait_for(Duration::from_secs(10), || {
        matches!(backend().probe(&Host::Local, &target, None), Liveness::Alive).then_some(())
    });
    assert!(alive.is_some(), "the orchestrator session should come up Alive");

    // No `$TMUX_PANE` in the orchestrator's environment.
    let pane = wait_for(Duration::from_secs(10), || {
        std::fs::read_to_string(&tmux_pane_file).ok()
    });
    assert_eq!(
        pane.as_deref().map(str::trim),
        Some("unset"),
        "the orchestrator session must run with no $TMUX_PANE in its env"
    );

    // Wait until it has painted a ready input box (so it is reading stdin) before
    // delivering, so the steer can't race ahead of the read loop.
    let ready = wait_for(Duration::from_secs(10), || {
        let screen = backend().snapshot(&Host::Local, &target).unwrap_or_default();
        ready::is_input_ready(&screen).then_some(())
    });
    assert!(
        ready.is_some(),
        "the orchestrator session should paint a ready input box"
    );

    // Deliver one line through the backend. It must reach the orchestrator
    // exactly once: a single PTY stdin, no teed / duplicated pane input.
    backend()
        .send_line(&Host::Local, &target, "steer-once")
        .expect("deliver a line through the session backend");

    let delivered = wait_for(Duration::from_secs(10), || {
        let got = std::fs::read_to_string(&delivered_file).ok()?;
        got.lines().any(|l| l == "steer-once").then_some(got)
    })
    .expect("the orchestrator should receive the delivered line");
    let count = delivered.lines().filter(|l| *l == "steer-once").count();
    assert_eq!(
        count, 1,
        "the steer must reach the orchestrator exactly once, got {count}: {delivered:?}"
    );
}
