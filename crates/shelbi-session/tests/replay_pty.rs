//! Real-PTY attach-replay tests exercising the production path end to end:
//! a genuine `/bin/sh` child on a real PTY, output split at parser-rest
//! boundaries exactly as the session reader splits it
//! ([`shelbi_session::output_split::RestSplitter`]), fed into the production
//! [`shelbi_session::emulator::Emulator`], then serialized by
//! [`shelbi_session::emulator::Emulator::replay`] and replayed into a fresh
//! client emulator. No external binary beyond `/bin/sh`.

use std::io::{Read, Write};
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use shelbi_session::emulator::Emulator;
use shelbi_session::output_split::RestSplitter;

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// A real PTY running `/bin/sh`, with a reader thread that splits output at
/// rest boundaries (as the session does) and feeds a "session" emulator, while
/// recording every rest-aligned frame so a "client" can replay them later.
struct PtySession {
    writer: Box<dyn Write + Send>,
    session: Emulator,
    reader: std::sync::mpsc::Receiver<Vec<u8>>,
    _child: Box<dyn portable_pty::Child + Send + Sync>,
}

impl PtySession {
    fn start() -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        // `cat` in raw mode echoes exactly the bytes we write, with no tty
        // cooking or echo, so the test injects arbitrary terminal output
        // (escape sequences included) straight into the session's output stream.
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("stty raw -echo 2>/dev/null; exec cat");
        let child = pair.slave.spawn_command(cmd).expect("spawn");
        drop(pair.slave);
        let writer = pair.master.take_writer().expect("writer");
        let mut reader = pair.master.try_clone_reader().expect("reader");

        let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
        std::thread::spawn(move || {
            let mut splitter = RestSplitter::new();
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let piece = splitter.push(&buf[..n]);
                        if !piece.is_empty() && tx.send(piece).is_err() {
                            break;
                        }
                    }
                }
            }
            let tail = splitter.flush();
            if !tail.is_empty() {
                let _ = tx.send(tail);
            }
        });

        PtySession {
            writer,
            session: Emulator::new(COLS, ROWS),
            reader: rx,
            _child: child,
        }
    }

    /// Write raw bytes for the child (raw `cat`) to echo back into the output
    /// stream verbatim.
    fn emit(&mut self, bytes: &[u8]) {
        let _ = self.writer.write_all(bytes);
        let _ = self.writer.flush();
    }

    /// Wait until the child has entered raw mode and is echoing, so that escape
    /// bytes written afterward are not mangled by the initial cooked-mode line
    /// discipline. Then wipe the handshake noise from the (normal) screen.
    fn wait_ready(&mut self) {
        const SENTINEL: &str = "RT_REPLAY_READY";
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            self.emit(format!("{SENTINEL}\r\n").as_bytes());
            let _ = self.pump_until(Duration::from_millis(300), |t| t.contains(SENTINEL));
            if self.session.visible_text().contains(SENTINEL) || Instant::now() >= deadline {
                break;
            }
        }
        // Clear the normal screen and scrollback so later assertions see only
        // the content the test draws.
        self.emit(b"\x1b[3J\x1b[2J\x1b[H");
        let _ = self.pump_until(Duration::from_millis(300), |t| !t.contains(SENTINEL));
    }

    /// Drain output frames into the session emulator until `pred(text)` holds or
    /// the deadline passes. Returns the frames consumed (for replay).
    fn pump_until(
        &mut self,
        timeout: Duration,
        mut pred: impl FnMut(&str) -> bool,
    ) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let deadline = Instant::now() + timeout;
        loop {
            if pred(&self.session.visible_text()) {
                return frames;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return frames;
            }
            match self.reader.recv_timeout(remaining.min(Duration::from_millis(200))) {
                Ok(frame) => {
                    self.session.feed(&frame);
                    frames.push(frame);
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(_) => return frames,
            }
        }
    }
}

/// Replay the session into a fresh same-size client emulator.
fn client_from_replay(session: &Emulator) -> Emulator {
    let (cols, rows) = session.size();
    let mut client = Emulator::new(cols, rows);
    client.feed(&session.replay());
    client
}

// ---------------------------------------------------------------------------
// Real PTY, full-screen program: reattach reproduces the screen.
// ---------------------------------------------------------------------------
#[test]
fn real_pty_fullscreen_reattach_reproduces_screen() {
    let mut s = PtySession::start();
    s.wait_ready();
    // Enter the alternate screen and paint a recognizable full-screen view.
    s.emit(b"\x1b[?1049h\x1b[2J\x1b[H");
    s.emit(b"ALT_SCREEN_TOP\r\n");
    s.emit(b"\x1b[12;30HMIDDLE_MARKER");
    let _ = s.pump_until(Duration::from_secs(5), |t| t.contains("MIDDLE_MARKER"));

    assert!(s.session.alt_screen_active(), "program is on the alternate screen");
    let client = client_from_replay(&s.session);
    assert!(client.alt_screen_active(), "client lands on the alternate screen");
    assert_eq!(
        s.session.visible_text(),
        client.visible_text(),
        "reattached client's screen must match the session's"
    );
}

// ---------------------------------------------------------------------------
// Real PTY: attach while a full-screen program is open, quit it, shell intact.
// ---------------------------------------------------------------------------
#[test]
fn real_pty_quit_fullscreen_reveals_shell_underneath() {
    let mut s = PtySession::start();
    s.wait_ready();
    s.emit(b"UNDERNEATH_abc123\r\n");
    let _ = s.pump_until(Duration::from_secs(5), |t| t.contains("UNDERNEATH_abc123"));
    // Now open a full-screen program over it.
    s.emit(b"\x1b[?1049h\x1b[2J\x1b[HFULLSCREEN_VIEW");
    let _ = s.pump_until(Duration::from_secs(5), |t| t.contains("FULLSCREEN_VIEW"));
    assert!(s.session.alt_screen_active());

    // A client attaches now (mid-full-screen) and gets the replay.
    let mut client = client_from_replay(&s.session);
    assert_eq!(s.session.visible_text(), client.visible_text());

    // Quit the full-screen program on both sides.
    client.feed(b"\x1b[?1049l");
    s.emit(b"\x1b[?1049l");
    let _ = s.pump_until(Duration::from_secs(5), |t| t.contains("UNDERNEATH_abc123"));

    assert!(
        client.visible_text().contains("UNDERNEATH_abc123"),
        "the client's shell screen underneath must be intact, not blank: {:?}",
        client.visible_text()
    );
}

// ---------------------------------------------------------------------------
// Real PTY: the kitty keyboard protocol survives replay.
// ---------------------------------------------------------------------------
#[test]
fn real_pty_keyboard_protocol_survives_replay() {
    let mut s = PtySession::start();
    s.wait_ready();
    s.emit(b"\x1b[>1u"); // push kitty "disambiguate escape codes"
    s.emit(b"KBD_READY\r\n");
    let _ = s.pump_until(Duration::from_secs(5), |t| t.contains("KBD_READY"));
    assert!(
        s.session.kitty_disambiguate_active(),
        "precondition: session has the kitty flag active"
    );

    let client = client_from_replay(&s.session);
    assert!(
        client.kitty_disambiguate_active(),
        "the kitty keyboard mode (Shift+Enter) must survive replay"
    );
}

// ---------------------------------------------------------------------------
// Real PTY, heavy output: no bytes lost or duplicated across the replay/live
// boundary. A client attaches mid-stream (replay), then the remaining frames
// are applied as live output; feeding the same total stream directly must land
// on the identical screen.
// ---------------------------------------------------------------------------
#[test]
fn real_pty_no_loss_or_duplication_across_replay_live_boundary() {
    let mut s = PtySession::start();
    // Emit a long, escape-sequence-dense stream (colored, numbered lines). The
    // reader's PTY read boundaries fall arbitrarily across these sequences, so
    // frames are torn unless the rest-splitter frames them at Ground. The
    // background reader drains the echo concurrently, so these writes never
    // block.
    for i in 0..400u32 {
        let line = format!("\x1b[3{}mrow-{:03}-\x1b[0mxy\r\n", i % 8, i);
        s.emit(line.as_bytes());
    }
    s.emit(b"STREAM_DONE\r\n");

    // Pump the first portion into the session, then snapshot a replay — this is
    // the moment a client attaches.
    let _ = s.pump_until(Duration::from_secs(5), |t| t.contains("row-050-"));
    let mut client = client_from_replay(&s.session);

    // Apply the remaining live frames to both the session and the attached
    // client, exactly as the broadcast path would.
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if s.session.visible_text().contains("STREAM_DONE") {
            break;
        }
        match s.reader.recv_timeout(Duration::from_millis(200)) {
            Ok(frame) => {
                s.session.feed(&frame);
                client.feed(&frame); // the client's live output after replay
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }

    assert!(
        s.session.visible_text().contains("STREAM_DONE"),
        "precondition: the whole stream was consumed"
    );
    assert_eq!(
        s.session.visible_text(),
        client.visible_text(),
        "replay + live must equal the session screen exactly (no loss/dup)"
    );
}
