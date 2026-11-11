//! A local dispatch-to-handoff cycle driven entirely through the **session
//! backend** (`rt-backend-sessions`), against a real `shelbi __session` process
//! running a stub agent.
//!
//! This is the Phase 2 acceptance: the orchestrator
//! spawns a worker as a detached session, detects ready/busy through the
//! backend's `snapshot` + `title`, delivers a message through the same seam
//! `submit::deliver_text` uses, and observes the review-ready marker the agent
//! writes — the full cycle a tmux dispatch runs, on the session backend.
//!
//! The session is spawned with the real binary (`CARGO_BIN_EXE_shelbi` via
//! `spawn_with_exe`) so a genuine `shelbi __session` owns the PTY and emulator;
//! every subsequent operation (probe, snapshot, title, deliver, kill) goes
//! through `session_backend::backend()`, the one runtime. Production `spawn_local_pane`
//! finds the binary via `current_exe` (covered by the `to_session_spawn_spec`
//! unit test); a cargo test's `current_exe` is the harness, so the e2e uses the
//! explicit-exe spawn to run the real session binary.

#![cfg(unix)]

use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use shelbi_core::Host;
use shelbi_orchestrator::ready;
use shelbi_orchestrator::session_backend::{
    backend, KillReason, Liveness, SessionBackend, SessionTarget, SlotDeath,
};
use shelbi_session::SpawnSpec;
use shelbi_state::{parse_pane_title_marker, PaneMarker};

/// These tests mutate process-global env (`SHELBI_HOME`, `SHELL`), so they
/// serialize against each other through this lock.
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

    // Point the whole process at this isolated home so `sessions_dir()` lands
    // under the temp home.
    std::env::set_var("SHELBI_HOME", home.path());

    // The worker addressed the way the orchestrator addresses a local workspace:
    // a window named `alice` inside the project session `shelbi-demo`. The
    // session backend derives the logical name `demo/ws/alice` from this.
    let target = SessionTarget::slot("shelbi-demo", "alice");

    // Before dispatch, nothing is bound to the slot.
    assert_eq!(backend().probe(&Host::Local, &target, None), Liveness::Dead);

    // A stub agent: it draws a Claude-like ready input box (`? for shortcuts`,
    // which `is_input_ready` matches) and sets the `shelbi:idle` OSC title the
    // worker hooks would, then blocks for a line. On receiving the delivered
    // message it flips its title to `shelbi:review` and writes the review-ready
    // marker — the handoff signal — before idling on `cat` so it stays live.
    //
    // The marker is written tmp+rename (the same atomic pattern real workers use
    // for Shelbi markers): a bare `> {marker}` creates and truncates the file
    // before `printf` writes, so a reader can catch it empty in that window. The
    // rename makes the marker appear only once its full contents are there.
    let stub = format!(
        "printf '? for shortcuts\\n'; \
         printf '\\033]2;shelbi:idle\\007'; \
         IFS= read -r line; \
         printf '\\033]2;shelbi:review\\007'; \
         printf '%s\\n' \"$line\" > {marker}.tmp && mv {marker}.tmp {marker}; \
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
    shelbi_orchestrator::submit::deliver_text(&Host::Local, &target, "please review")
        .expect("deliver the handoff message through the session backend");

    // The handoff: the agent wrote its review-ready marker, and its title now
    // reads `shelbi:review`. Observing the marker is the dispatch-to-handoff
    // cycle completing on the session backend.
    // Accept the marker only once its full contents are present: a partial or
    // empty read keeps polling instead of ending the wait (a belt-and-braces
    // guard alongside the stub's atomic write).
    let delivered = wait_for(Duration::from_secs(10), || {
        let got = std::fs::read_to_string(&marker).ok()?;
        (!got.trim().is_empty()).then_some(got)
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

    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    let tmux_pane_file = home.path().join("tmux_pane.txt");
    let delivered_file = home.path().join("delivered.txt");

    std::env::set_var("SHELBI_HOME", home.path());
    // A deterministic login shell for `orchestrator_session_spec`'s `$SHELL -lc`.
    std::env::set_var("SHELL", "/bin/sh");

    // A stub standing in for the Codex orchestrator bridge: on startup it records
    // what `$TMUX_PANE` is (expected: unset — the session scrubbed it), paints a
    // ready input box + the `shelbi:idle` title the worker hooks would, then
    // appends every stdin line it reads to a file so the test can count how many
    // times a single delivered steer arrives.
    let stub = cwd.path().join("stub-codex.sh");
    shelbi_state::test_support::write_executable(
        &stub,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"${{TMUX_PANE:-unset}}\" > {pane}.tmp && mv {pane}.tmp {pane}\n\
             printf '? for shortcuts\\n'\n\
             printf '\\033]2;shelbi:idle\\007'\n\
             while IFS= read -r line; do printf '%s\\n' \"$line\" >> {deliv}; done\n",
            pane = tmux_pane_file.display(),
            deliv = delivered_file.display(),
        ),
    );

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

    // No `$TMUX_PANE` in the orchestrator's environment. Accept the file only
    // once its contents are present so an empty read in the create/write window
    // keeps polling instead of ending the wait.
    let pane = wait_for(Duration::from_secs(10), || {
        let got = std::fs::read_to_string(&tmux_pane_file).ok()?;
        (!got.trim().is_empty()).then_some(got)
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

    // Deliver one line through the production submit path. It must reach the
    // orchestrator exactly once: a single PTY stdin, no teed / duplicated pane
    // input. `deliver_text` is how Shelbi actually steers a session (paste →
    // settle → Enter under the injection lock); the settle is load-bearing here
    // too — the backend's raw `send_line` sends the text and the Enter as two
    // separate connections with no settle between them, so under load the `\r`
    // can overtake the pasted bytes and terminate an empty line, leaving
    // `steer-once` sitting un-newlined in the tty buffer where `read` never
    // completes it. The settled path keeps the delivery deterministic without
    // changing what this asserts (exactly-once over the one PTY stdin).
    shelbi_orchestrator::submit::deliver_text(&Host::Local, &target, "steer-once")
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

/// `gY`: a thread's message reaches a real session as a submitted prompt, the
/// text and then its Enter, and the thread is marked sent. A workspace with no
/// session refuses it, and the thread stays unsent.
#[test]
fn diff_comments_reach_a_real_session_as_a_submitted_prompt() {
    use shelbi_app::diff::backend::{DiffBackend, DiffTarget, HostBackend, TargetKind};
    use shelbi_app::diff::comments::{Anchor, Author, Comment, CommentOp};
    use shelbi_app::diff::model::Side;
    use shelbi_app::diff::store::DiffStore;
    use shelbi_app::diff::{DiffOptions, DiffSession};
    use shelbi_state::keymap::{validate_keymaps_yaml, KeyChord};

    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    let work_dir = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    struct Guard(SessionTarget);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = backend().kill(&Host::Local, &self.0);
            std::env::remove_var("SHELBI_HOME");
        }
    }
    let target = SessionTarget::slot("shelbi-demo", "alice");
    let _guard = Guard(target.clone());

    // The project: a custom runner, which gets the race-safe delivery without a
    // parser for a screen this stub doesn't have.
    let projects = shelbi_state::projects_dir().unwrap();
    std::fs::create_dir_all(&projects).unwrap();
    std::fs::write(
        projects.join("demo.yaml"),
        format!(
            "repo: {dir}\nbase_branch: main\nagent_runners:\n  stub: {{ command: stub-agent, flags: [] }}\n\
             orchestrator: {{ runner: stub }}\nmachines:\n  - {{ name: hub, kind: local, work_dir: {dir} }}\n\
             workspaces:\n  - {{ name: alice, machine: hub, runner: stub }}\n  - {{ name: bob, machine: hub, runner: stub }}\n",
            dir = work_dir.path().display(),
        ),
    )
    .unwrap();

    // A worktree with a change on it.
    let worktree = work_dir.path().join("wt");
    std::fs::create_dir_all(&worktree).unwrap();
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git").arg("-C").arg(&worktree).args(args).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    git(&["init", "-q", "-b", "main"]);
    git(&["config", "user.name", "F"]);
    git(&["config", "user.email", "f@example.com"]);
    std::fs::write(worktree.join("a.txt"), "one\n").unwrap();
    git(&["add", "-A"]);
    git(&["commit", "-q", "-m", "base"]);
    std::fs::write(worktree.join("a.txt"), "ONE\n").unwrap();

    // The agent: `cat` on the session's terminal, so a line lands in the file
    // only when its Enter arrives. Multi-line text comes as a bracketed paste
    // to a real agent; this one asks for none, so each line is its own.
    let received = home.path().join("received.txt");
    let spec = SpawnSpec {
        name: "demo/ws/alice".into(),
        cwd: work_dir.path().to_path_buf(),
        cols: 80,
        rows: 24,
        task: None,
        raw_output_log: false,
        child_argv: vec!["/bin/sh".into(), "-c".into(), format!("exec cat > {}", received.display())],
    };
    shelbi_client::spawn_with_exe(Path::new(env!("CARGO_BIN_EXE_shelbi")), &spec).expect("spawn the worker session");
    // Up and accepting: alive by its lock comes a moment before its socket.
    assert!(
        wait_for(Duration::from_secs(10), || backend().snapshot(&Host::Local, &target).ok()).is_some(),
        "the session should come up"
    );

    let open = |workspace: &str, store: &Path| {
        let target = DiffTarget {
            project: "demo".into(),
            workspace: workspace.into(),
            kind: TargetKind::Workspace,
            task: Some("t-1".into()),
        };
        let store = DiffStore::at(store);
        store
            .apply_comment(&CommentOp::Add(Comment {
                id: "c1".into(),
                anchor: Anchor { path: "a.txt".into(), side: Side::New, start: 1, end: 1, code: vec!["ONE".into()] },
                author: Author::human("jlong"),
                body: "why upper case?".into(),
                created_at: chrono::Utc::now(),
                replies: Vec::new(),
                sent: false,
            }))
            .unwrap();
        let backend: std::sync::Arc<dyn DiffBackend> = std::sync::Arc::new(HostBackend::over(
            Host::Local,
            worktree.to_string_lossy().into_owned(),
            target.clone(),
            "main",
            store,
            shelbi_orchestrator::agent_send::service(),
        ));
        let options =
            DiffOptions { author: Author::human("jlong"), target, watch: None, wake: std::sync::Arc::new(|| {}) };
        DiffSession::open(move || Ok(backend), options)
    };
    let keymap = validate_keymaps_yaml("", None).0.diff;
    let send = |session: &mut DiffSession| {
        wait_for(Duration::from_secs(10), || {
            session.poll();
            (session.state().comments().len() == 1).then_some(())
        })
        .expect("the thread loads");
        for chord in ["g", "shift-y"] {
            session.key(KeyChord::parse(chord).unwrap(), &keymap);
        }
    };

    // No session on `bob`: refused, and the thread is still to send.
    let bob_store = tempfile::tempdir().unwrap();
    let mut bob = open("bob", bob_store.path());
    send(&mut bob);
    wait_for(Duration::from_secs(40), || {
        bob.poll();
        bob.state().flash().filter(|f| f.starts_with("Couldn't")).map(str::to_string)
    })
    .map(|flash| assert!(flash.contains("no agent is running on `bob`"), "{flash}"))
    .expect("a refusal");
    assert!(!bob.state().comments()[0].sent);

    // `alice` receives it.
    let alice_store = tempfile::tempdir().unwrap();
    let mut alice = open("alice", alice_store.path());
    send(&mut alice);
    wait_for(Duration::from_secs(40), || {
        alice.poll();
        alice.state().comments()[0].sent.then_some(())
    })
    .unwrap_or_else(|| panic!("the delivery should land: {:?}", alice.state().flash()));
    assert_eq!(alice.state().flash(), Some("Sent 1 comment to the agent (this agent can't confirm it)"));

    let got = wait_for(Duration::from_secs(10), || {
        let text = std::fs::read_to_string(&received).ok()?;
        text.contains("your reply").then_some(text)
    })
    .unwrap_or_else(|| panic!("the prompt's last line needs its Enter: {:?}", std::fs::read_to_string(&received)));
    for piece in ["[thread c1] a.txt:1", "ONE", "jlong: why upper case?", "diff comment reply --task t-1"] {
        assert!(got.contains(piece), "{piece} in {got:?}");
    }
    let events = std::fs::read_to_string(home.path().join("events.log")).unwrap_or_default();
    assert!(events.contains("status=unverified"), "{events}");
}

/// The crash-versus-deliberate input to pane supervision, against real session
/// processes: how each one ended is read back from what it left on disk.
#[test]
fn a_session_says_how_it_ended() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
        }
    }
    let _guard = Guard;

    let run = |slot: &str, script: &str| -> SessionTarget {
        let spec = SpawnSpec {
            name: format!("demo/ws/{slot}"),
            cwd: cwd.path().to_path_buf(),
            cols: 80,
            rows: 24,
            task: None,
            raw_output_log: false,
            child_argv: vec!["/bin/sh".into(), "-c".into(), script.to_string()],
        };
        shelbi_client::spawn_with_exe(Path::new(env!("CARGO_BIN_EXE_shelbi")), &spec)
            .expect("spawn the session");
        SessionTarget::slot("shelbi-demo", slot)
    };
    let settled = |target: &SessionTarget, want: SlotDeath| {
        let got = wait_for(Duration::from_secs(10), || {
            let dead = !backend().probe(&Host::Local, target, None).is_alive();
            let death = backend().slot_death(&Host::Local, target);
            (dead && death == want).then_some(death)
        });
        assert_eq!(got, Some(want), "{}", target.label());
    };

    // A user `/exit`: the agent ends with status 0.
    settled(&run("clean", "exit 0"), SlotDeath::CleanExit);
    // A crash: the agent ends with anything else.
    settled(&run("crash", "exit 3"), SlotDeath::Crash);

    // A deliberate kill reads as such whatever the agent exits with, including
    // an agent that handles the SIGTERM and exits 0.
    for (slot, script, reason) in [
        ("stop", "printf READY; exec cat", KillReason::Stop),
        (
            "polite",
            "trap 'exit 0' TERM; printf READY; while :; do sleep 0.05; done",
            KillReason::Handoff,
        ),
    ] {
        let target = run(slot, script);
        // Wait on the screen, not `probe`: a session's lock is held before its
        // socket exists, and the kill has to connect. It also proves the trap is
        // installed before the signal lands.
        assert!(
            wait_for(Duration::from_secs(10), || {
                let screen = backend().snapshot(&Host::Local, &target).unwrap_or_default();
                screen.contains("READY").then_some(())
            })
            .is_some(),
            "{slot} should come up",
        );
        shelbi_orchestrator::workspace::kill_workspace_pane("demo", &Host::Local, &target, slot, reason)
            .expect("kill");
        settled(&target, SlotDeath::Killed(reason));
    }
}
