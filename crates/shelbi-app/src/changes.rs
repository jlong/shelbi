//! Hub-socket subscriber that wakes a client's refresher on board/workspace
//! changes. Shared by the TUI shell and the desktop app.
//!
//! The native views are fed from snapshots **and daemon change notifications** —
//! they do not run their own polling loop. When the daemon's poller observes a
//! board or workspace change it publishes a [`shelbi_state::ChangeNotification`]
//! on the hub's `subscribe` stream; this subscriber forwards a wake signal onto a
//! channel the client drains and turns into a refresh request. A periodic
//! refresh in the client is the fallback for when no daemon is running (so a
//! bare project still updates).
//!
//! It owns a background thread that keeps the stream open and reconnects with a
//! short backoff so a daemon that is still starting, idle-exits, or restarts is
//! tolerated. Threads and `std::sync::mpsc` only, like [`crate::refresh`].

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::Duration;

use shelbi_state::ChangeNotification;

/// A refresh-worthy change, forwarded to the client. A workspace change carries
/// its name so a client can retry just that workspace's idle/failed main-area
/// attach when its session starts (`rt-tui-idle-workspace-open`); a board change
/// only requests a data refresh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeWake {
    Board,
    Workspace(String),
    /// The far daemon asked its clients to reload (`shelbi reload` there):
    /// a remote window reconnects and decides again whether it may stay
    /// native. Only a remote link sends it
    /// ([`crate::source::remote::subscribe_changes`]); this machine's own
    /// reload reaches the app through its relaunch watch instead.
    Reload { reason: String },
    /// Documents of the project's wiki changed (wiki-relative `paths`): a
    /// client showing the Wiki reads its tree and open document again. Not a
    /// board change.
    Wiki { paths: Vec<String> },
    /// The project's wiki sync status is now `status` (`synced`, `syncing`,
    /// `pending`, `offline`, `conflict`, `error <detail>`).
    WikiStatus { status: String },
}

impl ChangeWake {
    /// A wiki wake: it refreshes the Wiki, not the board.
    pub fn is_wiki(&self) -> bool {
        matches!(self, ChangeWake::Wiki { .. } | ChangeWake::WikiStatus { .. })
    }
}

/// How long a connect attempt / idle read waits before re-checking the stop flag,
/// and the pause between reconnect attempts.
const POLL_SLICE: Duration = Duration::from_millis(500);

/// A live change subscription. Dropping it stops the thread (sets the flag and
/// joins).
pub struct ChangeSubscription {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
    /// A remote subscription's relay streams, closed on drop so their
    /// threads' reads end.
    streams: Vec<crate::source::remote::OpenStream>,
}

impl ChangeSubscription {
    /// A remote subscription: its threads, and the streams they read.
    pub(crate) fn remote(
        stop: Arc<AtomicBool>,
        handles: Vec<JoinHandle<()>>,
        streams: Vec<crate::source::remote::OpenStream>,
    ) -> Self {
        ChangeSubscription { stop, handles, streams }
    }
}

impl Drop for ChangeSubscription {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        for stream in &self.streams {
            stream.close();
        }
        for h in self.handles.drain(..) {
            let _ = h.join();
        }
    }
}

/// Spawn the subscriber thread for `project`, returning the RAII handle and the
/// receiver the client drains. Every board/workspace change for this project
/// yields one wake; other projects and layout-only changes are filtered out
/// thread-side.
pub fn spawn(project: &str) -> (ChangeSubscription, Receiver<ChangeWake>) {
    spawn_observed(project, Arc::new(AtomicBool::new(false)))
}

/// [`spawn`], also keeping `connected` current: `true` while the subscription
/// stream to the daemon is open, `false` while the daemon is unreachable and
/// the thread is retrying. A client shows a "polling" indicator from it.
pub fn spawn_observed(
    project: &str,
    connected: Arc<AtomicBool>,
) -> (ChangeSubscription, Receiver<ChangeWake>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let project = project.to_string();
    let handle = thread::Builder::new()
        .name("shelbi-changes".to_string())
        .spawn(move || subscriber_loop(&project, &tx, &stop_thread, &connected))
        .expect("spawn change subscriber thread");
    (
        ChangeSubscription {
            stop,
            handles: vec![handle],
            streams: Vec::new(),
        },
        rx,
    )
}

/// Spawn one subscriber for every project's board and workspace changes on
/// this machine (the hub's `subscribe` with no project), returning the RAII
/// handle and the receiver the client drains. Each change yields the project
/// it names with its wake. The desktop's project catalog follows it to keep
/// every open project's dot current (`desktop/src/projects.rs`), so a
/// project starting or stopping work repaints its tab without a timer.
pub fn spawn_all() -> (ChangeSubscription, Receiver<(String, ChangeWake)>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let handle = thread::Builder::new()
        .name("shelbi-changes-all".to_string())
        .spawn(move || {
            let connected = AtomicBool::new(false);
            let frame = "{\"verb\":\"subscribe\"}\n";
            while !stop_thread.load(Ordering::SeqCst) {
                let end = stream_lines(frame, &stop_thread, &connected, |line| {
                    match any_wake_for(line) {
                        Some(change) => tx.send(change).map_err(|_| ()),
                        None => Ok(()),
                    }
                });
                match end {
                    StreamEnd::ReceiverGone => return,
                    StreamEnd::Disconnected => sleep_slice(&stop_thread),
                }
            }
        })
        .expect("spawn change subscriber thread");
    (
        ChangeSubscription {
            stop,
            handles: vec![handle],
            streams: Vec::new(),
        },
        rx,
    )
}

fn subscriber_loop(
    project: &str,
    tx: &Sender<ChangeWake>,
    stop: &AtomicBool,
    connected: &AtomicBool,
) {
    let frame = shelbi_client::relay::services::subscribe_line(project);
    while !stop.load(Ordering::SeqCst) {
        let end = stream_lines(&frame, stop, connected, |line| match wake_for(line, project) {
            Some(wake) => tx.send(wake).map_err(|_| ()),
            None => Ok(()),
        });
        connected.store(false, Ordering::SeqCst);
        match end {
            StreamEnd::ReceiverGone => return,
            StreamEnd::Disconnected => sleep_slice(stop),
        }
    }
}

enum StreamEnd {
    Disconnected,
    ReceiverGone,
}

/// Open the hub socket, send the subscribe `frame`, and hand each complete
/// line to `on_line` until the stream ends, `stop` is raised, or `on_line`
/// reports its receiver gone (`Err`).
fn stream_lines(
    frame: &str,
    stop: &AtomicBool,
    connected: &AtomicBool,
    mut on_line: impl FnMut(&[u8]) -> std::result::Result<(), ()>,
) -> StreamEnd {
    let Ok(path) = shelbi_state::hub_socket_path() else {
        return StreamEnd::Disconnected;
    };
    let Ok(mut stream) = UnixStream::connect(&path) else {
        return StreamEnd::Disconnected;
    };
    let _ = stream.set_read_timeout(Some(POLL_SLICE));
    if stream.write_all(frame.as_bytes()).is_err() {
        return StreamEnd::Disconnected;
    }
    connected.store(true, Ordering::SeqCst);

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
                while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
                    let line: Vec<u8> = buf.drain(..=nl).collect();
                    if on_line(&line[..line.len() - 1]).is_err() {
                        return StreamEnd::ReceiverGone;
                    }
                }
            }
            Err(e) if is_timeout(&e) => continue,
            Err(_) => return StreamEnd::Disconnected,
        }
    }
}

/// Split complete lines out of `buf`; send a wake for each board/workspace change
/// for this project. `Err(())` means the receiver was dropped.
pub(crate) fn drain_lines(
    buf: &mut Vec<u8>,
    project: &str,
    tx: &Sender<ChangeWake>,
) -> std::result::Result<(), ()> {
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=nl).collect();
        let line = &line[..line.len() - 1];
        if let Some(wake) = wake_for(line, project) {
            tx.send(wake).map_err(|_| ())?;
        }
    }
    Ok(())
}

/// The [`ChangeWake`] `line` warrants for `project`, or `None` when it isn't a
/// board/workspace change for this project (layout events, other projects, and
/// malformed lines don't wake a refresh).
fn wake_for(line: &[u8], project: &str) -> Option<ChangeWake> {
    let change = std::str::from_utf8(line)
        .ok()
        .and_then(ChangeNotification::from_line)?;
    if change.project() != project {
        return None;
    }
    match change {
        ChangeNotification::Board { .. } => Some(ChangeWake::Board),
        ChangeNotification::Workspace { workspace, .. } => Some(ChangeWake::Workspace(workspace)),
        ChangeNotification::Wiki { paths, .. } => Some(ChangeWake::Wiki { paths }),
        ChangeNotification::WikiStatus { status, .. } => Some(ChangeWake::WikiStatus { status }),
        _ => None,
    }
}

/// [`wake_for`] for any project: the project `line` names with its wake.
fn any_wake_for(line: &[u8]) -> Option<(String, ChangeWake)> {
    let change = std::str::from_utf8(line)
        .ok()
        .and_then(ChangeNotification::from_line)?;
    let project = change.project().to_string();
    // The project dots follow work, not documents.
    let wake = wake_for(line, &project).filter(|w| !w.is_wiki())?;
    Some((project, wake))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn board_and_workspace_changes_for_the_project_wake_refresh() {
        let board = br#"{"change":"board","project":"demo"}"#;
        assert_eq!(wake_for(board, "demo"), Some(ChangeWake::Board));
        // A workspace change carries its name so the shell can retry just that
        // workspace's idle attach.
        let ws = br#"{"change":"workspace","project":"demo","workspace":"alpha"}"#;
        assert_eq!(
            wake_for(ws, "demo"),
            Some(ChangeWake::Workspace("alpha".into()))
        );
    }

    #[test]
    fn the_all_projects_stream_names_each_changes_project() {
        let ws = br#"{"change":"workspace","project":"web","workspace":"alpha"}"#;
        assert_eq!(any_wake_for(ws), Some(("web".into(), ChangeWake::Workspace("alpha".into()))));
        let board = br#"{"change":"board","project":"api"}"#;
        assert_eq!(any_wake_for(board), Some(("api".into(), ChangeWake::Board)));
        let layout =
            br#"{"change":"layout","project":"demo","event":{"layout":"orchestrator-restarted"}}"#;
        assert_eq!(any_wake_for(layout), None);
        assert_eq!(any_wake_for(b"not json"), None);
    }

    #[test]
    fn wiki_changes_wake_the_wiki_not_the_board() {
        let docs = br#"{"change":"wiki","project":"demo","paths":["Plans/a.md"],"source":"agent"}"#;
        let wake = wake_for(docs, "demo").unwrap();
        assert_eq!(wake, ChangeWake::Wiki { paths: vec!["Plans/a.md".into()] });
        assert!(wake.is_wiki());
        let status = br#"{"change":"wiki-status","project":"demo","status":"syncing"}"#;
        assert_eq!(wake_for(status, "demo"), Some(ChangeWake::WikiStatus { status: "syncing".into() }));
        assert!(!ChangeWake::Board.is_wiki());
        // The all-projects stream (the project dots) skips them.
        assert_eq!(any_wake_for(docs), None);
        assert_eq!(wake_for(docs, "other"), None);
    }

    #[test]
    fn other_projects_layout_events_and_junk_do_not_wake() {
        let other = br#"{"change":"board","project":"other"}"#;
        assert_eq!(wake_for(other, "demo"), None);
        let layout =
            br#"{"change":"layout","project":"demo","event":{"layout":"orchestrator-restarted"}}"#;
        assert_eq!(wake_for(layout, "demo"), None);
        assert_eq!(wake_for(b"not json", "demo"), None);
    }
}
