//! AC4: every key except Ctrl+Space reaches the focused agent, proven against a
//! **real** session process and a key-echo child.
//!
//! This spawns a genuine [`shelbi_session::run`] on a thread (the same server
//! `shelbi __session` runs, as `protocol_e2e.rs` and `session_cli.rs` do) whose
//! child is `cat` in raw, no-echo mode — so the PTY echoes back exactly the
//! bytes it receives, with no line-discipline cooking. A [`ShellState`] drives a
//! live binding to that session through the crate's real event-loop input path
//! (`handle_key`, the function the loop calls for a `crossterm` key event), and
//! a second, independent observer client reads the echoed bytes. Asserting the
//! observer sees exactly the encoder's output for a representative key set — and
//! never the NUL that Ctrl+Space would produce — is the end-to-end AC4 check.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use shelbi_client::{Connection, SessionEvent, SessionEvents};
use shelbi_proto::capability;
use shelbi_session::layout::SessionPaths;
use shelbi_session::RunArgs;
use shelbi_term::input::{encode_key, KeyEncoding};
use shelbi_term::Size;

use super::caps::Caps;
use super::session::{Connected, Connector, MainState, SessionRef};
use super::sidebar::RowTarget;
use super::ShellState;

const NONE: KeyModifiers = KeyModifiers::NONE;
const CTRL: KeyModifiers = KeyModifiers::CONTROL;
const ALT: KeyModifiers = KeyModifiers::ALT;
const SHIFT: KeyModifiers = KeyModifiers::SHIFT;
const SUPER: KeyModifiers = KeyModifiers::SUPER;

/// A connector that binds the shell's main area straight to an already-running
/// session socket (the production [`super::session::LiveConnector`] discovers by
/// name; here the test knows the exact socket).
struct DirectConnector {
    sock: PathBuf,
}

impl Connector for DirectConnector {
    fn connect(
        &self,
        _project: &str,
        _target: &SessionRef,
    ) -> Result<Connected, super::session::ConnectFailure> {
        use super::session::ConnectFailure;
        let (conn, events) = Connection::open(&self.sock, None, capability::ALL)
            .map_err(|e| ConnectFailure::Message(e.to_string()))?;
        let size = match conn.info() {
            Ok(info) => Size::new(info.cols.max(1), info.rows.max(1)),
            Err(_) => Size::new(80, 24),
        };
        conn.attach(None)
            .map_err(|e| ConnectFailure::Message(e.to_string()))?;
        Ok(Connected { conn, events, size })
    }
}

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Pump the shell's non-blocking background sources once, exactly as the event
/// loop does (drain live session output into the pane, advance the connect).
fn pump(st: &mut ShellState) {
    let mut ring = false;
    st.sessions.pump_output(&mut ring);
    st.sessions.poll();
}

/// Drain whatever the observer has received into `out` (output bytes only, so
/// the attach resync replay never pollutes the byte assertions).
fn drain(events: &SessionEvents, out: &mut Vec<u8>) {
    while let Some(ev) = events.try_recv() {
        if let SessionEvent::Output { data, .. } = ev {
            out.extend_from_slice(&data);
        }
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

/// Clean up the session and the process-global `SHELBI_HOME` even if an
/// assertion unwinds.
struct Cleanup {
    sock: PathBuf,
}

impl Drop for Cleanup {
    fn drop(&mut self) {
        if let Ok((conn, _)) = Connection::open(&self.sock, None, capability::ALL) {
            let _ = conn.kill(Some(libc::SIGKILL));
        }
        std::env::remove_var("SHELBI_HOME");
    }
}

#[test]
fn every_key_but_ctrl_space_reaches_the_agent_over_a_real_pty() {
    // Serialize against every other test that mutates the process-global
    // `SHELBI_HOME` (`session::run` resolves the sessions dir from it).
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    let name = "tuikeys/orch";
    let id = shelbi_session::layout::derive_id_now(name);
    let paths = SessionPaths::new(&home.path().join("sessions"), &id);

    // A raw, no-echo `cat`: the PTY passes control bytes through untouched (no
    // ISIG, no canonical mode) and echoes exactly what it reads.
    let args = RunArgs {
        id: id.clone(),
        name: name.to_string(),
        cwd: std::env::temp_dir(),
        cols: 80,
        rows: 24,
        task: None,
        raw_output_log: false,
        child_argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo 2>/dev/null; exec cat".into(),
        ],
        // In-process: no daemon watchdog racing our per-test `SHELBI_HOME`.
        manage_daemon: false,
    };
    let _session = std::thread::spawn(move || shelbi_session::run(args));
    let sock = paths.sock();
    // Generous deadline: the PTY session spawns on a worker thread, and under a
    // loaded host (several workers plus a parallel `cargo build` saturating the
    // CPU) its socket can take several seconds to bind. A tight 5s wait flaked
    // here; this returns the instant the socket appears, so a healthy run is
    // unaffected.
    wait_for(Duration::from_secs(30), || sock.exists().then_some(()))
        .expect("the session socket should appear");
    let _cleanup = Cleanup { sock: sock.clone() };

    // An independent observer that watches the echoed PTY output.
    let (observer, obs_events) =
        Connection::open(&sock, None, capability::ALL).expect("observer connects");
    observer.attach(None).expect("observer attaches");
    // Drain the attach resync so only live output lands in `out`.
    wait_for(Duration::from_secs(2), || {
        obs_events
            .try_recv()
            .and_then(|ev| matches!(ev, SessionEvent::Resync { .. }).then_some(()))
    });
    let mut out: Vec<u8> = Vec::new();

    // Drive the shell's real input path against this session.
    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st = ShellState::new("tuikeys", Arc::new(DirectConnector { sock: sock.clone() }), caps);
    st.show(RowTarget::Session(SessionRef::Orchestrator));
    wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        matches!(st.sessions.state(), MainState::Live(_)).then_some(())
    })
    .expect("the shell's main area should bind the session live");

    // --- a representative key set, encoded without the kitty protocol ---------
    let enc = KeyEncoding::default();
    let fkey = {
        let kev = KeyEvent::new(KeyCode::F(1), NONE);
        let (k, m) = super::terminal_view::map_key(&kev).unwrap();
        encode_key(k, m, enc)
    };
    let keys: Vec<(KeyEvent, Vec<u8>)> = vec![
        (KeyEvent::new(KeyCode::Char('a'), NONE), b"a".to_vec()),
        (KeyEvent::new(KeyCode::Char('b'), NONE), b"b".to_vec()),
        (KeyEvent::new(KeyCode::Char('c'), CTRL), vec![0x03]), // Ctrl+C → ETX
        (KeyEvent::new(KeyCode::Esc, NONE), vec![0x1b]),
        (KeyEvent::new(KeyCode::Up, NONE), b"\x1b[A".to_vec()),
        (KeyEvent::new(KeyCode::Down, NONE), b"\x1b[B".to_vec()),
        (KeyEvent::new(KeyCode::F(1), NONE), fkey.clone()),
        (KeyEvent::new(KeyCode::Char('x'), ALT), b"\x1bx".to_vec()), // Alt+x → ESC x
        (KeyEvent::new(KeyCode::Char(']'), CTRL), vec![0x1d]),       // Ctrl+] → GS
    ];
    let expected: Vec<u8> = keys.iter().flat_map(|(_, b)| b.clone()).collect();

    for (kev, _) in &keys {
        st.handle_key(*kev);
        pump(&mut st);
    }

    // The observer sees the whole representative set echoed, in order and intact.
    let saw = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out, &expected).then_some(())
    });
    assert!(
        saw.is_some(),
        "every representative key should reach the agent; expected {expected:?} within {out:?}"
    );
    // Spot-check the headline bytes individually too.
    assert!(contains(&out, &[0x03]), "Ctrl+C reached the agent as ETX");
    assert!(contains(&out, b"\x1b[A"), "Up arrow reached the agent");
    assert!(contains(&out, b"\x1bx"), "Alt+x reached the agent");

    // --- Shift+Enter under the kitty protocol ---------------------------------
    // Turn the protocol on in the pane the way a program does: emit `CSI > 1 u`
    // as session output (cat echoes the bytes we inject, which the pane feeds
    // into its emulator), then wait until the pane reports kitty is active.
    st.sessions.send_input(b"\x1b[>1u");
    let kitty_on = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        st.sessions
            .live_pane_mut()
            .map(|p| p.key_encoding().kitty)
            .unwrap_or(false)
            .then_some(())
    });
    assert!(kitty_on.is_some(), "the pane should pick up the kitty keyboard protocol");

    st.handle_key(KeyEvent::new(KeyCode::Enter, SHIFT));
    let shift_enter = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        // Under the protocol, Shift+Enter is the distinct CSI-u event ESC[13;2u
        // (a bare CR would be indistinguishable from plain Enter).
        contains(&out, b"\x1b[13;2u").then_some(())
    });
    assert!(
        shift_enter.is_some(),
        "Shift+Enter under the kitty protocol should reach the agent as ESC[13;2u: {out:?}"
    );

    // --- Tab under the kitty protocol reaches the agent as ESC[9u --------------
    // With the protocol on, the Tab *key* must arrive as the CSI-u event ESC[9u;
    // a bare `\t` is read as literal tab text (completion never fires). The shell
    // forwards Tab with the main pane focused — it is never consumed — so seeing
    // ESC[9u in the echoed stream proves both the encoding and the forwarding.
    let tab_before = out.len();
    st.handle_key(KeyEvent::new(KeyCode::Tab, NONE));
    let tab = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out[tab_before..], b"\x1b[9u").then_some(())
    });
    assert!(
        tab.is_some(),
        "Tab under the kitty protocol should reach the agent as ESC[9u: {:?}",
        &out[tab_before..]
    );

    // Shift+Tab stays the legacy backtab ESC[Z (Claude Code's mode cycle); the
    // kitty carve-out is scoped to the unmodified Tab, so this is unchanged.
    let backtab_before = out.len();
    st.handle_key(KeyEvent::new(KeyCode::BackTab, SHIFT));
    let backtab = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out[backtab_before..], b"\x1b[Z").then_some(())
    });
    assert!(
        backtab.is_some(),
        "Shift+Tab under the kitty protocol should reach the agent as ESC[Z: {:?}",
        &out[backtab_before..]
    );

    // --- Ctrl+Space is reserved: it opens the palette, never reaching the agent
    st.handle_key(KeyEvent::new(KeyCode::Char(' '), CTRL));
    assert!(
        matches!(st.overlay, Some(super::ActiveOverlay::Palette(_))),
        "Ctrl+Space opens the command palette instead of going to the agent"
    );
    // Give any (erroneously) forwarded byte time to echo back, then confirm the
    // agent never saw the NUL that Ctrl+Space would encode to.
    let settle = Instant::now() + Duration::from_millis(300);
    while Instant::now() < settle {
        pump(&mut st);
        drain(&obs_events, &mut out);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !out.contains(&0x00),
        "Ctrl+Space must not reach the agent (no NUL in the echoed stream): {out:?}"
    );
}

#[test]
fn a_copy_chord_with_a_selection_is_not_forwarded_to_the_agent() {
    // With text selected in the pane, Cmd+C (SUPER) and Ctrl+Shift+C copy the
    // selection and are consumed by the shell — no stray `c` / Ctrl+C reaches
    // the agent. Proven against a real PTY whose `cat` echoes what it receives.
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    let name = "tuicopy/orch";
    let id = shelbi_session::layout::derive_id_now(name);
    let paths = SessionPaths::new(&home.path().join("sessions"), &id);

    let args = RunArgs {
        id: id.clone(),
        name: name.to_string(),
        cwd: std::env::temp_dir(),
        cols: 80,
        rows: 24,
        task: None,
        raw_output_log: false,
        child_argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo 2>/dev/null; exec cat".into(),
        ],
        manage_daemon: false,
    };
    let _session = std::thread::spawn(move || shelbi_session::run(args));
    let sock = paths.sock();
    // Generous deadline: the PTY session spawns on a worker thread, and under a
    // loaded host (several workers plus a parallel `cargo build` saturating the
    // CPU) its socket can take several seconds to bind. A tight 5s wait flaked
    // here; this returns the instant the socket appears, so a healthy run is
    // unaffected.
    wait_for(Duration::from_secs(30), || sock.exists().then_some(()))
        .expect("the session socket should appear");
    let _cleanup = Cleanup { sock: sock.clone() };

    let (observer, obs_events) =
        Connection::open(&sock, None, capability::ALL).expect("observer connects");
    observer.attach(None).expect("observer attaches");
    wait_for(Duration::from_secs(2), || {
        obs_events
            .try_recv()
            .and_then(|ev| matches!(ev, SessionEvent::Resync { .. }).then_some(()))
    });
    let mut out: Vec<u8> = Vec::new();

    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st = ShellState::new("tuicopy", Arc::new(DirectConnector { sock: sock.clone() }), caps);
    st.show(RowTarget::Session(SessionRef::Orchestrator));
    wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        matches!(st.sessions.state(), MainState::Live(_)).then_some(())
    })
    .expect("the shell's main area should bind the session live");

    // Put "hello world" in the pane (cat echoes it; `pump` feeds the output into
    // the emulator) and wait until the pane's selection machinery can see it.
    st.sessions.send_input(b"hello world");
    let sz = Size::new(80, 24);
    let has_text = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        let p = st.sessions.live_pane_mut()?;
        // A throwaway drag select to probe the grid contents.
        p.on_mouse(&mev(MouseEventKind::Down(MouseButton::Left), 0, 0), 0, 0, sz);
        p.on_mouse(&mev(MouseEventKind::Drag(MouseButton::Left), 4, 0), 4, 0, sz);
        p.on_mouse(&mev(MouseEventKind::Up(MouseButton::Left), 4, 0), 4, 0, sz);
        (p.selection_copy().as_deref() == Some("hello")).then_some(())
    });
    assert!(has_text.is_some(), "the pane should hold a selectable \"hello\"");

    // From here the agent must receive nothing from the copy chords.
    let before = out.len();
    st.handle_key(KeyEvent::new(KeyCode::Char('c'), SUPER)); // Cmd+C
    st.handle_key(KeyEvent::new(KeyCode::Char('c'), CTRL | SHIFT)); // Ctrl+Shift+C
    let settle = Instant::now() + Duration::from_millis(300);
    while Instant::now() < settle {
        pump(&mut st);
        drain(&obs_events, &mut out);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !out[before..].contains(&b'c') && !out[before..].contains(&0x03),
        "a copy chord must not forward `c` or Ctrl+C to the agent: {:?}",
        &out[before..],
    );
    // The selection survives the copy (the chord does not clear it).
    assert_eq!(
        st.sessions.live_pane_mut().unwrap().selection_copy().as_deref(),
        Some("hello"),
        "the copy chord leaves the selection in place",
    );

    // A normal key still reaches the agent and dismisses the selection.
    st.handle_key(KeyEvent::new(KeyCode::Char('z'), NONE));
    let saw_z = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out[before..], b"z").then_some(())
    });
    assert!(saw_z.is_some(), "a normal key still reaches the agent: {:?}", &out[before..]);
    assert!(
        st.sessions.live_pane_mut().unwrap().selection_copy().is_none(),
        "a session keypress clears the selection",
    );
}

/// A pane-relative mouse event with no modifiers, for driving a selection.
fn mev(kind: MouseEventKind, column: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column, row, modifiers: NONE }
}

#[test]
fn focus_chords_are_not_forwarded_but_backspace_is() {
    // The vim-style focus moves (Ctrl+H / Ctrl+L) are intercepted by the shell
    // and never reach the agent; plain Backspace still reaches it. Proven
    // against a real PTY whose `cat` echoes exactly the bytes it receives.
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    let name = "tuifocus/orch";
    let id = shelbi_session::layout::derive_id_now(name);
    let paths = SessionPaths::new(&home.path().join("sessions"), &id);

    let args = RunArgs {
        id: id.clone(),
        name: name.to_string(),
        cwd: std::env::temp_dir(),
        cols: 80,
        rows: 24,
        task: None,
        raw_output_log: false,
        child_argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo 2>/dev/null; exec cat".into(),
        ],
        manage_daemon: false,
    };
    let _session = std::thread::spawn(move || shelbi_session::run(args));
    let sock = paths.sock();
    // Generous deadline: the PTY session spawns on a worker thread, and under a
    // loaded host (several workers plus a parallel `cargo build` saturating the
    // CPU) its socket can take several seconds to bind. A tight 5s wait flaked
    // here; this returns the instant the socket appears, so a healthy run is
    // unaffected.
    wait_for(Duration::from_secs(30), || sock.exists().then_some(()))
        .expect("the session socket should appear");
    let _cleanup = Cleanup { sock: sock.clone() };

    let (observer, obs_events) =
        Connection::open(&sock, None, capability::ALL).expect("observer connects");
    observer.attach(None).expect("observer attaches");
    wait_for(Duration::from_secs(2), || {
        obs_events
            .try_recv()
            .and_then(|ev| matches!(ev, SessionEvent::Resync { .. }).then_some(()))
    });
    let mut out: Vec<u8> = Vec::new();

    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st = ShellState::new("tuifocus", Arc::new(DirectConnector { sock: sock.clone() }), caps);
    st.show(RowTarget::Session(SessionRef::Orchestrator));
    wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        matches!(st.sessions.state(), MainState::Live(_)).then_some(())
    })
    .expect("the shell's main area should bind the session live");
    // `show` on a session takes main focus.
    assert!(st.focus_is_main(), "a live session starts focused");

    // --- plain Backspace still reaches the agent ------------------------------
    let bs_expected = {
        let kev = KeyEvent::new(KeyCode::Backspace, NONE);
        let (k, m) = super::terminal_view::map_key(&kev).unwrap();
        encode_key(k, m, KeyEncoding::default())
    };
    assert!(
        !bs_expected.is_empty() && !bs_expected.contains(&0x08),
        "Backspace encodes to a non-empty, non-BS (0x08) sequence: {bs_expected:?}",
    );
    st.handle_key(KeyEvent::new(KeyCode::Backspace, NONE));
    let saw_bs = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out, &bs_expected).then_some(())
    });
    assert!(
        saw_bs.is_some(),
        "plain Backspace should reach the agent; expected {bs_expected:?} within {out:?}",
    );

    // --- Ctrl+H moves focus to the sidebar and is NOT forwarded ---------------
    // Ctrl+H would encode to BS (0x08) if it were forwarded; assert no 0x08
    // appears after this point.
    let before = out.len();
    st.handle_key(KeyEvent::new(KeyCode::Char('h'), CTRL));
    assert!(!st.focus_is_main(), "Ctrl+H moves focus to the sidebar");
    let settle = Instant::now() + Duration::from_millis(300);
    while Instant::now() < settle {
        pump(&mut st);
        drain(&obs_events, &mut out);
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        !out[before..].contains(&0x08),
        "Ctrl+H must not reach the agent (no BS byte echoed): {:?}",
        &out[before..],
    );

    // --- Ctrl+L moves focus back to the main pane -----------------------------
    st.handle_key(KeyEvent::new(KeyCode::Char('l'), CTRL));
    assert!(st.focus_is_main(), "Ctrl+L moves focus back to the main pane");
}

// --- drag-and-drop paste routing (rt-drag-and-drop-paste-into-a-review-or-
// workspace-panel-and-with-sidebar-focus-is-dropped) -------------------------
//
// A terminal reports a file drag-and-drop as a bracketed paste of the dropped
// path, which the shell delivers as a `crossterm` `Event::Paste`. These tests
// prove the paste reaches whatever session the main area shows — even with the
// nav sidebar or an interface panel focused — against a real PTY whose `cat`
// echoes exactly the bytes it receives, so an observer sees what the agent got.

/// Spawn a raw, no-echo `cat` session named `name` under `home` and return its
/// socket path once it is bound. The PTY echoes exactly the bytes it receives.
fn spawn_cat(home: &std::path::Path, name: &str) -> PathBuf {
    let id = shelbi_session::layout::derive_id_now(name);
    let paths = SessionPaths::new(&home.join("sessions"), &id);
    let args = RunArgs {
        id,
        name: name.to_string(),
        cwd: std::env::temp_dir(),
        cols: 80,
        rows: 24,
        task: None,
        raw_output_log: false,
        child_argv: vec![
            "/bin/sh".into(),
            "-c".into(),
            "stty raw -echo 2>/dev/null; exec cat".into(),
        ],
        manage_daemon: false,
    };
    let _session = std::thread::spawn(move || shelbi_session::run(args));
    let sock = paths.sock();
    wait_for(Duration::from_secs(30), || sock.exists().then_some(()))
        .expect("the session socket should appear");
    sock
}

/// Open an observer client on `sock` and drain its attach resync, so only live
/// output lands in later `drain` calls. Returns the kept-alive connection and
/// its event stream.
fn attach_observer(sock: &std::path::Path) -> (Connection, SessionEvents) {
    let (observer, obs_events) =
        Connection::open(sock, None, capability::ALL).expect("observer connects");
    observer.attach(None).expect("observer attaches");
    wait_for(Duration::from_secs(2), || {
        obs_events
            .try_recv()
            .and_then(|ev| matches!(ev, SessionEvent::Resync { .. }).then_some(()))
    });
    (observer, obs_events)
}

#[test]
fn a_dropped_file_pastes_into_the_main_session_even_with_the_sidebar_focused() {
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    let sock = spawn_cat(home.path(), "tuipaste/orch");
    let _cleanup = Cleanup { sock: sock.clone() };
    let (_observer, obs_events) = attach_observer(&sock);
    let mut out: Vec<u8> = Vec::new();

    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st = ShellState::new("tuipaste", Arc::new(DirectConnector { sock: sock.clone() }), caps);
    st.show(RowTarget::Session(SessionRef::Orchestrator));
    wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        matches!(st.sessions.state(), MainState::Live(_)).then_some(())
    })
    .expect("the shell's main area should bind the session live");

    // Focus the nav sidebar: a drop targets the window, not the focused list.
    st.client.focus_sidebar();
    assert!(!st.focus_is_main(), "the sidebar holds focus before the drop");

    // A path with a space must arrive exactly as sent (no re-quoting / truncation).
    let path = "/tmp/holiday pic.png";
    st.handle_event(Event::Paste(path.to_string()));

    let saw = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out, path.as_bytes()).then_some(())
    });
    assert!(
        saw.is_some(),
        "the dropped path should reach the main session intact; expected {path:?} within {out:?}",
    );
    assert!(st.focus_is_main(), "a drop moves focus to the main area");
}

#[test]
fn a_paste_is_bracketed_when_the_program_enabled_bracketed_paste() {
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    let sock = spawn_cat(home.path(), "tuipastebr/orch");
    let _cleanup = Cleanup { sock: sock.clone() };
    let (_observer, obs_events) = attach_observer(&sock);
    let mut out: Vec<u8> = Vec::new();

    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st =
        ShellState::new("tuipastebr", Arc::new(DirectConnector { sock: sock.clone() }), caps);
    st.show(RowTarget::Session(SessionRef::Orchestrator));
    wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        matches!(st.sessions.state(), MainState::Live(_)).then_some(())
    })
    .expect("the shell's main area should bind the session live");

    // Turn on bracketed-paste mode (DECSET 2004) the way a program does: `cat`
    // echoes the sequence, and the session's emulator picks the mode up as it
    // processes that output (before broadcasting it), so seeing it on the
    // observer means the mode is already live for the next paste.
    st.sessions.send_input(b"\x1b[?2004h");
    let on = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out, b"\x1b[?2004h").then_some(())
    });
    assert!(on.is_some(), "the session should enable bracketed-paste mode: {out:?}");

    let before = out.len();
    st.handle_event(Event::Paste("hi".to_string()));
    let bracketed = wait_for(Duration::from_secs(5), || {
        pump(&mut st);
        drain(&obs_events, &mut out);
        contains(&out[before..], b"\x1b[200~hi\x1b[201~").then_some(())
    });
    assert!(
        bracketed.is_some(),
        "an enabled program should receive the paste wrapped in bracketed markers: {:?}",
        &out[before..],
    );
}

#[test]
fn a_dropped_file_pastes_into_the_open_review_content_session() {
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    // The review content view binds to the slot's workspace session.
    let sock = spawn_cat(home.path(), "tuipasterev/review-1");
    let _cleanup = Cleanup { sock: sock.clone() };
    let (_observer, obs_events) = attach_observer(&sock);
    let mut out: Vec<u8> = Vec::new();

    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st =
        ShellState::new("tuipasterev", Arc::new(DirectConnector { sock: sock.clone() }), caps);

    // Build a review interface whose content session is live (DirectConnector
    // binds whatever target to the one `cat` socket), then install it the way an
    // open review leaves the shell.
    let mut review = super::review::ReviewInterface::new(
        "tuipasterev",
        Arc::new(DirectConnector { sock: sock.clone() }),
        "fix-login",
        "review-1",
        "/wt",
        "Vim",
        true,
        None,
    );
    wait_for(Duration::from_secs(5), || {
        review.poll();
        let mut ring = false;
        review.pump_output(&mut ring);
        matches!(review.content_state(), MainState::Live(_)).then_some(())
    })
    .expect("the review content session should go live");
    st.review = Some(review);
    st.main_view = super::MainView::Review("fix-login".into());
    // The panel (not the content view) holds focus, and the shell focus is on
    // the sidebar — a drop must still reach the content session.
    st.client.focus_sidebar();

    let path = "/tmp/review shot.png";
    st.handle_event(Event::Paste(path.to_string()));

    let saw = wait_for(Duration::from_secs(5), || {
        if let Some(r) = st.review.as_mut() {
            r.poll();
            let mut ring = false;
            r.pump_output(&mut ring);
        }
        drain(&obs_events, &mut out);
        contains(&out, path.as_bytes()).then_some(())
    });
    assert!(
        saw.is_some(),
        "the dropped path should reach the review agent intact; expected {path:?} within {out:?}",
    );
    assert!(st.focus_is_main(), "a drop moves focus to the main area");
    assert!(
        st.review.as_ref().unwrap().content_focused(),
        "focus follows the drop to the review content view",
    );
}

#[test]
fn a_dropped_file_pastes_into_the_open_workspace_content_session() {
    let _lock = crate::test_support::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    let home = tempfile::tempdir().unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    // The workspace content view binds to the workspace's agent session.
    let sock = spawn_cat(home.path(), "tuipastews/ws-1");
    let _cleanup = Cleanup { sock: sock.clone() };
    let (_observer, obs_events) = attach_observer(&sock);
    let mut out: Vec<u8> = Vec::new();

    let caps = Caps { kitty: true, truecolor: true, nested: None };
    let mut st =
        ShellState::new("tuipastews", Arc::new(DirectConnector { sock: sock.clone() }), caps);

    let mut workspace = super::workspace::WorkspaceInterface::new(
        "tuipastews",
        Arc::new(DirectConnector { sock: sock.clone() }),
        "ws-1",
        "/wt",
        "Vim",
        "Developer",
        super::default_ws_status(),
        None,
    );
    wait_for(Duration::from_secs(5), || {
        workspace.poll();
        let mut ring = false;
        workspace.pump_output(&mut ring);
        matches!(workspace.content_state(), MainState::Live(_)).then_some(())
    })
    .expect("the workspace content session should go live");
    st.workspace = Some(workspace);
    st.main_view = super::MainView::Workspace("ws-1".into());
    st.client.focus_sidebar();

    let path = "/tmp/ws shot.png";
    st.handle_event(Event::Paste(path.to_string()));

    let saw = wait_for(Duration::from_secs(5), || {
        if let Some(w) = st.workspace.as_mut() {
            w.poll();
            let mut ring = false;
            w.pump_output(&mut ring);
        }
        drain(&obs_events, &mut out);
        contains(&out, path.as_bytes()).then_some(())
    });
    assert!(
        saw.is_some(),
        "the dropped path should reach the workspace agent intact; expected {path:?} within {out:?}",
    );
    assert!(st.focus_is_main(), "a drop moves focus to the main area");
    assert!(
        st.workspace.as_ref().unwrap().content_focused(),
        "focus follows the drop to the workspace content view",
    );
}
