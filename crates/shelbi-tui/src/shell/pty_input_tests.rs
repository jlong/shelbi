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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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

/// A connector that binds the shell's main area straight to an already-running
/// session socket (the production [`super::session::LiveConnector`] discovers by
/// name; here the test knows the exact socket).
struct DirectConnector {
    sock: PathBuf,
}

impl Connector for DirectConnector {
    fn connect(&self, _project: &str, _target: &SessionRef) -> Result<Connected, String> {
        let (conn, events) =
            Connection::open(&self.sock, None, capability::ALL).map_err(|e| e.to_string())?;
        let size = match conn.info() {
            Ok(info) => Size::new(info.cols.max(1), info.rows.max(1)),
            Err(_) => Size::new(80, 24),
        };
        conn.attach(None).map_err(|e| e.to_string())?;
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
    wait_for(Duration::from_secs(5), || sock.exists().then_some(()))
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
