//! Old-session compatibility harness (remove-tmux cutover,
//! `docs/removing-tmux/README.md`, "Compatibility with old sessions").
//!
//! The session protocol has a **frozen core** (`shelbi-proto`): the current
//! client must keep driving a session process built from any previous release.
//! This harness runs the *current* [`shelbi_client`] against a list of session
//! *binaries* and exercises every frozen-core operation end to end against a
//! real `shelbi __session`:
//!
//!   hello · attach-with-replay · output · input · resize · snapshot · kill ·
//!   exit event
//!
//! There are no previous session-capable releases yet, so the list holds only
//! the current build (`CARGO_BIN_EXE_shelbi`) and the harness proves the current
//! client against the current session. **Adding a release is a one-line change**:
//! append one entry to [`releases`] (or inject paths at run time through
//! `$SHELBI_COMPAT_BINARIES`, a colon-separated list of `label=path` pairs, so
//! CI can point at downloaded release binaries without editing this file).
//!
//! `run()` resolves `~/.shelbi/sessions` from the process-global `$SHELBI_HOME`,
//! so the whole file runs under one serial lock, like the other session tests.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use shelbi_client::{spawn_with_exe, Connection, SessionEvent};
use shelbi_proto::{capability, PROTOCOL_VERSION};
use shelbi_session::SpawnSpec;

/// These tests mutate process-global env (`SHELBI_HOME`), so they serialize
/// against each other through this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// A session binary to prove the current client against.
struct Release {
    /// Human-readable label used in assertion messages (e.g. `current`, `v0.9.0`).
    label: String,
    /// Path to the `shelbi` binary whose `__session` body is exercised.
    exe: PathBuf,
}

/// The session binaries the current client is tested against.
///
/// To add a previously released binary, append **one line** here, e.g.:
/// ```ignore
/// v.push(release("v0.9.0", "/opt/shelbi-releases/v0.9.0/shelbi"));
/// ```
/// or, without editing this file, export
/// `SHELBI_COMPAT_BINARIES="v0.9.0=/opt/shelbi-releases/v0.9.0/shelbi"` (several
/// entries separated by `:`).
fn releases() -> Vec<Release> {
    let mut v = vec![release("current", env!("CARGO_BIN_EXE_shelbi"))];
    if let Ok(extra) = std::env::var("SHELBI_COMPAT_BINARIES") {
        for entry in extra.split(':').filter(|e| !e.is_empty()) {
            let (label, path) = entry
                .split_once('=')
                .unwrap_or_else(|| panic!("SHELBI_COMPAT_BINARIES entry `{entry}` is not label=path"));
            v.push(release(label, path));
        }
    }
    v
}

fn release(label: &str, exe: impl Into<PathBuf>) -> Release {
    Release {
        label: label.to_string(),
        exe: exe.into(),
    }
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

#[test]
fn current_client_drives_each_release_session_through_the_frozen_core() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    for rel in releases() {
        exercise_frozen_core(&rel);
    }
}

fn exercise_frozen_core(rel: &Release) {
    let label = &rel.label;
    assert!(
        rel.exe.is_file(),
        "[{label}] session binary not found at {}",
        rel.exe.display()
    );

    // Isolated home so the session's sockets/metadata land under a temp dir and
    // never touch the developer's real `~/.shelbi`.
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());

    // The child echoes its input back (the PTY line discipline echoes typed
    // input, and `cat` re-emits each line), so a sent marker shows up in both
    // the live output stream and a screen snapshot. It never exits on its own,
    // so the harness controls termination with an explicit kill.
    let spec = SpawnSpec {
        name: format!("compat/{label}/ws"),
        cwd: cwd.path().to_path_buf(),
        cols: 80,
        rows: 24,
        task: None,
        raw_output_log: false,
        child_argv: vec!["/bin/sh".into(), "-c".into(), "exec cat".into()],
    };
    let spawned =
        spawn_with_exe(&rel.exe, &spec).unwrap_or_else(|e| panic!("[{label}] spawn session: {e}"));

    // Kill the session even if an assertion panics, then clear the global env.
    struct Guard {
        sock: PathBuf,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            if let Ok((conn, _events)) = Connection::open(&self.sock, None, capability::ALL) {
                let _ = conn.kill(Some(libc::SIGKILL));
            }
            std::env::remove_var("SHELBI_HOME");
        }
    }
    let _guard = Guard {
        sock: spawned.sock.clone(),
    };

    // Wait for the session to bind its socket.
    wait_for(Duration::from_secs(10), || spawned.sock.exists().then_some(()))
        .unwrap_or_else(|| panic!("[{label}] session socket never appeared"));

    // 1. hello: the handshake succeeds and the session speaks the frozen-core
    //    protocol version this client was built against. We negotiate the full
    //    capability set the real client uses (`capability::ALL`), exactly as the
    //    TUI and `shelbi attach` do.
    let (conn, events) = Connection::open(&spawned.sock, None, capability::ALL)
        .unwrap_or_else(|e| panic!("[{label}] hello handshake: {e}"));
    assert_eq!(
        conn.session_protocol_version(),
        PROTOCOL_VERSION,
        "[{label}] session announced a different frozen-core protocol version"
    );

    // 2. attach: subscribe to the output stream.
    conn.attach(None)
        .unwrap_or_else(|e| panic!("[{label}] attach: {e}"));

    // 3 + 4. input + output: a sent marker echoes back on the output stream.
    conn.input(b"MARKER-ONE\r")
        .unwrap_or_else(|e| panic!("[{label}] input: {e}"));
    let saw_output = wait_for(Duration::from_secs(5), || {
        while let Some(ev) = events.try_recv() {
            if let SessionEvent::Output { data, .. } = ev {
                if contains(&data, b"MARKER-ONE") {
                    return Some(());
                }
            }
        }
        None
    });
    assert!(
        saw_output.is_some(),
        "[{label}] input marker never came back on the output stream"
    );

    // 5. attach with replay: a second client attaching from scratch is brought
    //    up to the current screen through the attach replay, so the replay
    //    stream reproduces the marker already on screen.
    {
        let (conn2, events2) = Connection::open(&spawned.sock, None, capability::ALL)
            .unwrap_or_else(|e| panic!("[{label}] second hello handshake: {e}"));
        conn2
            .attach(None)
            .unwrap_or_else(|e| panic!("[{label}] second attach: {e}"));
        let replayed = wait_for(Duration::from_secs(5), || {
            while let Some(ev) = events2.try_recv() {
                if let SessionEvent::Resync { replay, .. } = ev {
                    if contains(&replay, b"MARKER-ONE") {
                        return Some(());
                    }
                }
            }
            None
        });
        assert!(
            replayed.is_some(),
            "[{label}] attach replay did not reconstruct the on-screen marker for a fresh client"
        );
    }

    // 6. resize: the new size is reported in-band (Resized) or out-of-band
    //    (SizeChanged). A second line of input guarantees an in-band marker even
    //    if the out-of-band event was already drained above.
    conn.resize(100, 40)
        .unwrap_or_else(|e| panic!("[{label}] resize: {e}"));
    conn.input(b"MARKER-TWO\r")
        .unwrap_or_else(|e| panic!("[{label}] input after resize: {e}"));
    let saw_resize = wait_for(Duration::from_secs(5), || {
        while let Some(ev) = events.try_recv() {
            match ev {
                SessionEvent::Resized { cols, rows, .. } | SessionEvent::SizeChanged { cols, rows }
                    if cols == 100 && rows == 40 =>
                {
                    return Some(())
                }
                _ => {}
            }
        }
        None
    });
    assert!(
        saw_resize.is_some(),
        "[{label}] resize to 100x40 was never reflected in a Resized/SizeChanged event"
    );

    // 7. snapshot: the visible screen carries the echoed marker.
    let snap = wait_for(Duration::from_secs(5), || {
        let text = conn.snapshot(None).ok()?.text;
        text.contains("MARKER-ONE").then_some(text)
    });
    assert!(
        snap.is_some(),
        "[{label}] snapshot never showed the echoed marker; last screen: {:?}",
        conn.snapshot(None).map(|s| s.text)
    );

    // 8 + 9. kill + exit event: killing the child's process group ends the
    //    stream with an Exited event. (The session reaps through portable-pty,
    //    which collapses a signal death into an exit code, so the harness
    //    asserts the event is delivered, not its exact code/signal shape.)
    conn.kill(Some(libc::SIGKILL))
        .unwrap_or_else(|e| panic!("[{label}] kill: {e}"));
    let exited = wait_for(Duration::from_secs(10), || {
        while let Some(ev) = events.try_recv() {
            if let SessionEvent::Exited(exited) = ev {
                return Some(exited);
            }
        }
        None
    });
    assert!(
        exited.is_some(),
        "[{label}] no Exited event was delivered after kill"
    );
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}
