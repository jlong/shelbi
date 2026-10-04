//! Hub-socket subscriber that wakes the shell's refresher on board/workspace
//! changes.
//!
//! The native views are fed from snapshots **and daemon change notifications** —
//! they do not run their own polling loop. When the daemon's poller observes a
//! board or workspace change it publishes a [`shelbi_state::ChangeNotification`]
//! on the hub's `subscribe` stream; this subscriber forwards a wake signal onto a
//! channel the shell drains each tick and turns into a
//! [`ShellRefresher::request`](super::refresh::ShellRefresher::request). A short
//! periodic refresh in the event loop is the fallback for when no daemon is
//! running (so a bare project still updates).
//!
//! It owns a background thread that keeps the stream open and reconnects with a
//! short backoff so a daemon that is still starting, idle-exits, or restarts is
//! tolerated — the same connection shape as [`crate::layout_sub`], but it reacts
//! to the board/workspace variants rather than the layout ones.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use shelbi_state::ChangeNotification;

/// How long a connect attempt / idle read waits before re-checking the stop flag,
/// and the pause between reconnect attempts.
const POLL_SLICE: Duration = Duration::from_millis(500);

/// A live change subscription. Dropping it stops the thread (sets the flag and
/// joins).
pub struct ChangeSubscription {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Drop for ChangeSubscription {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Spawn the subscriber thread for `project`, returning the RAII handle and the
/// receiver the shell drains each tick. Every board/workspace change for this
/// project yields one `()` wake; other projects and layout-only changes are
/// filtered out thread-side.
pub fn spawn(project: &str) -> (ChangeSubscription, Receiver<()>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let project = project.to_string();
    let handle = thread::Builder::new()
        .name("shelbi-shell-changes".to_string())
        .spawn(move || subscriber_loop(&project, &tx, &stop_thread))
        .expect("spawn shell change subscriber thread");
    (ChangeSubscription { stop, handle: Some(handle) }, rx)
}

fn subscriber_loop(project: &str, tx: &Sender<()>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match connect_and_stream(project, tx, stop) {
            StreamEnd::ReceiverGone => return,
            StreamEnd::Disconnected => sleep_slice(stop),
        }
    }
}

enum StreamEnd {
    Disconnected,
    ReceiverGone,
}

fn connect_and_stream(project: &str, tx: &Sender<()>, stop: &AtomicBool) -> StreamEnd {
    let Ok(path) = shelbi_state::hub_socket_path() else {
        return StreamEnd::Disconnected;
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return StreamEnd::Disconnected;
    };
    let _ = stream.set_read_timeout(Some(POLL_SLICE));

    let frame = format!("{{\"verb\":\"subscribe\",\"project\":{}}}\n", json_string(project));
    if stream.write_all(frame.as_bytes()).is_err() {
        return StreamEnd::Disconnected;
    }

    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        if stop.load(Ordering::SeqCst) {
            return StreamEnd::Disconnected;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return StreamEnd::Disconnected,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if drain_lines(&mut buf, project, tx).is_err() {
                    return StreamEnd::ReceiverGone;
                }
            }
            Err(e) if is_timeout(&e) => continue,
            Err(_) => return StreamEnd::Disconnected,
        }
    }
}

/// Split complete lines out of `buf`; send a wake for each board/workspace change
/// for this project. `Err(())` means the receiver was dropped.
fn drain_lines(buf: &mut Vec<u8>, project: &str, tx: &Sender<()>) -> std::result::Result<(), ()> {
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=nl).collect();
        let line = &line[..line.len() - 1];
        if wakes_refresh(line, project) {
            tx.send(()).map_err(|_| ())?;
        }
    }
    Ok(())
}

/// Whether `line` is a board/workspace change for `project` (the changes that
/// should trigger a data refresh). Layout events, other projects, and malformed
/// lines do not.
fn wakes_refresh(line: &[u8], project: &str) -> bool {
    let Some(change) = std::str::from_utf8(line)
        .ok()
        .and_then(ChangeNotification::from_line)
    else {
        return false;
    };
    if change.project() != project {
        return false;
    }
    matches!(
        change,
        ChangeNotification::Board { .. } | ChangeNotification::Workspace { .. }
    )
}

fn sleep_slice(stop: &AtomicBool) {
    for _ in 0..5 {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(POLL_SLICE / 5);
    }
}

fn is_timeout(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

/// Minimal JSON string encoder for the project name in the subscribe frame.
fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_and_workspace_changes_for_the_project_wake_refresh() {
        let board = br#"{"change":"board","project":"demo"}"#;
        assert!(wakes_refresh(board, "demo"));
        let ws = br#"{"change":"workspace","project":"demo","workspace":"alpha"}"#;
        assert!(wakes_refresh(ws, "demo"));
    }

    #[test]
    fn other_projects_layout_events_and_junk_do_not_wake() {
        let other = br#"{"change":"board","project":"other"}"#;
        assert!(!wakes_refresh(other, "demo"));
        let layout =
            br#"{"change":"layout","project":"demo","event":{"layout":"orchestrator-restarted"}}"#;
        assert!(!wakes_refresh(layout, "demo"));
        assert!(!wakes_refresh(b"not json", "demo"));
    }

    #[test]
    fn json_string_escapes_quotes_and_controls() {
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        assert_eq!(json_string("x\ty"), "\"x\\ty\"");
    }
}
