//! Hub-socket subscriber for pushed layout events (Phase 3,
//! `rt-daemon-layout-split`; `docs/removing-tmux/phase3-daemon.md`).
//!
//! When the daemon runs the poller (the default on this branch), the poller
//! drives the *session* half of a layout split and publishes a typed
//! [`shelbi_state::LayoutEvent`] to the daemon's change bus. The sidebar — the
//! always-present layout client on the tmux runtime — subscribes to that bus
//! over the hub socket and reacts by doing the pane/window work the poller used
//! to do inline. This module owns the connection: a background thread that
//! keeps a `subscribe` stream open, forwards each layout notification for this
//! project onto an [`std::sync::mpsc`] channel, and reconnects with backoff so a
//! daemon that is still starting, idle-exits, or restarts is tolerated.
//!
//! The in-sidebar poller (the setting-off fallback) publishes to this process's
//! *own* change bus instead, which the sidebar drains directly
//! ([`crate::app::App::poll_layout_events`]); this socket path is the other
//! half, for when the producing poller lives in the daemon.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use shelbi_state::{ChangeNotification, LayoutEvent};

/// How long a connect attempt / idle read waits before the thread re-checks the
/// stop flag, and the pause between reconnect attempts. Short enough that the
/// sidebar notices a stop promptly and picks up a just-started daemon quickly.
const POLL_SLICE: Duration = Duration::from_millis(500);

/// A live hub-socket layout subscription. Dropping it stops the thread (sets the
/// flag and joins), so it shuts down whichever way the sidebar exits — mirroring
/// the [`WorkspacePoller`](crate::WorkspacePoller) RAII handle.
pub struct LayoutSubscription {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for LayoutSubscription {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the subscriber thread for `project`, returning the RAII handle and the
/// receiver the sidebar drains each tick. Layout events for other projects (the
/// bus is hub-global) are filtered out thread-side, so the receiver only ever
/// carries this project's events.
pub fn spawn(project: &str) -> (LayoutSubscription, Receiver<LayoutEvent>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let project = project.to_string();
    let handle = thread::Builder::new()
        .name("shelbi-layout-sub".into())
        .spawn(move || subscriber_loop(&project, &tx, &stop_thread))
        .ok();
    (LayoutSubscription { stop, handle }, rx)
}

/// Connect/subscribe/read loop, reconnecting until `stop` is set or the receiver
/// is dropped.
fn subscriber_loop(project: &str, tx: &Sender<LayoutEvent>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match connect_and_stream(project, tx, stop) {
            // The receiver was dropped (the sidebar is gone) — nothing left to
            // feed, so exit instead of reconnecting.
            StreamEnd::ReceiverGone => return,
            // A disconnect / connect failure: pause briefly, then reconnect. The
            // daemon may be starting, idle-exited, or mid-restart.
            StreamEnd::Disconnected => sleep_slice(stop),
        }
    }
}

/// Why [`connect_and_stream`] returned.
enum StreamEnd {
    /// The stream closed or could not be opened — reconnect.
    Disconnected,
    /// The sidebar's receiver was dropped — stop for good.
    ReceiverGone,
}

/// Open one subscribe stream and forward this project's layout events until it
/// closes, `stop` is set, or the receiver is gone.
fn connect_and_stream(project: &str, tx: &Sender<LayoutEvent>, stop: &AtomicBool) -> StreamEnd {
    let Ok(path) = shelbi_state::hub_socket_path() else {
        return StreamEnd::Disconnected;
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return StreamEnd::Disconnected;
    };
    // A read timeout lets the loop re-check `stop` on an idle connection instead
    // of blocking forever on a quiet board.
    let _ = stream.set_read_timeout(Some(POLL_SLICE));

    // The subscribe frame names this project so the daemon streams only its
    // changes (the bus is hub-global). The connection then only reads — no ack.
    let frame = format!("{{\"verb\":\"subscribe\",\"project\":{}}}\n", json_string(project));
    if stream.write_all(frame.as_bytes()).is_err() {
        return StreamEnd::Disconnected;
    }

    // Parse NDJSON lines as they arrive, buffering partial reads across the read
    // timeout so a line split across two reads is still assembled whole.
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if stop.load(Ordering::SeqCst) {
            return StreamEnd::Disconnected;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return StreamEnd::Disconnected, // daemon closed the stream
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if drain_lines(&mut buf, project, tx).is_err() {
                    return StreamEnd::ReceiverGone;
                }
            }
            Err(e) if is_timeout(&e) => continue, // idle; re-check stop
            Err(_) => return StreamEnd::Disconnected,
        }
    }
}

/// Split complete `\n`-terminated lines out of `buf`, forwarding each that
/// parses to one of this project's layout events. Leaves any trailing partial
/// line in `buf`. `Err(())` means the receiver was dropped.
fn drain_lines(buf: &mut Vec<u8>, project: &str, tx: &Sender<LayoutEvent>) -> std::result::Result<(), ()> {
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=nl).collect();
        let line = &line[..line.len() - 1]; // drop the newline
        if let Some(event) = parse_layout_line(line, project) {
            tx.send(event).map_err(|_| ())?;
        }
    }
    Ok(())
}

/// Parse one NDJSON line into this project's layout event, if it is one. A
/// board/workspace change, another project's change, or a malformed line yields
/// `None`.
fn parse_layout_line(line: &[u8], project: &str) -> Option<LayoutEvent> {
    let change = ChangeNotification::from_line(std::str::from_utf8(line).ok()?)?;
    if change.project() != project {
        return None;
    }
    change.layout().cloned()
}

/// Minimal JSON string encoder for the project name in the subscribe frame —
/// enough to keep a name with a quote or backslash well-formed without pulling
/// the value through a serializer.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether a read error is the idle read-timeout (so the loop re-checks `stop`)
/// rather than a real failure. macOS reports `WouldBlock`; Linux `TimedOut`.
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

fn sleep_slice(stop: &AtomicBool) {
    if !stop.load(Ordering::SeqCst) {
        thread::sleep(POLL_SLICE);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_layout_line_extracts_this_projects_layout_event() {
        let line =
            br#"{"change":"layout","project":"p","event":{"layout":"orchestrator-restarted"}}"#;
        assert_eq!(
            parse_layout_line(line, "p"),
            Some(LayoutEvent::OrchestratorRestarted)
        );
    }

    #[test]
    fn parse_layout_line_ignores_other_projects_and_non_layout_changes() {
        let other =
            br#"{"change":"layout","project":"other","event":{"layout":"orchestrator-restarted"}}"#;
        assert_eq!(parse_layout_line(other, "p"), None);
        let board = br#"{"change":"board","project":"p"}"#;
        assert_eq!(parse_layout_line(board, "p"), None);
        assert_eq!(parse_layout_line(b"not json", "p"), None);
    }

    #[test]
    fn drain_lines_assembles_events_and_keeps_a_partial_tail() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut buf = Vec::new();
        // A whole line plus the start of a second.
        buf.extend_from_slice(
            br#"{"change":"layout","project":"p","event":{"layout":"review-opened","workspace":"r","task":"t"}}
{"change":"layout","project":"p","event":{"layout":"rev"#,
        );
        drain_lines(&mut buf, "p", &tx).unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            LayoutEvent::ReviewOpened {
                workspace: "r".into(),
                task: "t".into()
            }
        );
        assert!(rx.try_recv().is_err(), "the partial second line is not emitted yet");
        // Completing the line emits it.
        buf.extend_from_slice(b"iew-agent-recovered\",\"workspace\":\"r\"}}\n");
        drain_lines(&mut buf, "p", &tx).unwrap();
        assert_eq!(
            rx.try_recv().unwrap(),
            LayoutEvent::ReviewAgentRecovered { workspace: "r".into() }
        );
        assert!(buf.is_empty(), "no trailing bytes remain");
    }

    #[test]
    fn drain_lines_reports_receiver_gone() {
        let (tx, rx) = std::sync::mpsc::channel::<LayoutEvent>();
        drop(rx);
        let mut buf =
            br#"{"change":"layout","project":"p","event":{"layout":"orchestrator-restarted"}}
"#
            .to_vec();
        assert!(drain_lines(&mut buf, "p", &tx).is_err(), "a dropped receiver is reported");
    }

    #[test]
    fn json_string_escapes_quotes_and_backslashes() {
        assert_eq!(json_string("plain"), r#""plain""#);
        assert_eq!(json_string(r#"a"b\c"#), r#""a\"b\\c""#);
    }
}
