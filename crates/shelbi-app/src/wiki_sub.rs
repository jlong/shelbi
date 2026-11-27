//! The wiki's pushed changes, for a client showing it: the hub's wiki
//! service announces every change to a document (`wiki_changed`), every
//! change of its sync status, and a `shelbi wiki open`, on the change bus
//! ([`shelbi_state::ChangeNotification::Wiki`] and its siblings). A client
//! refreshes its tree (and an open document) from them, so an edit made
//! anywhere shows up at once.
//!
//! Its own stream, beside [`crate::changes`]' board/workspace wakes, so a
//! wiki edit never costs a board refresh. A remote project's comes over the
//! relay ([`crate::source::remote::subscribe_wiki`]); the shape is the same.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;

use shelbi_state::ChangeNotification;

use crate::changes::{ChangeSubscription, StreamEnd};

/// A change to the project's wiki.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WikiWake {
    /// Documents (and folders' files) changed: their wiki-relative paths.
    Changed { paths: Vec<String> },
    /// The sync's status is now this word (`synced`, `syncing`, `pending`,
    /// `offline`, `conflict`, `error <detail>`).
    Status(String),
    /// Someone asked to see the wiki at this path (its root when `None`).
    Open(Option<String>),
}

/// Follow `project`'s wiki on this machine's hub. Dropping the handle stops
/// its thread.
pub fn spawn(project: &str) -> (ChangeSubscription, Receiver<WikiWake>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let project = project.to_string();
    let handle = thread::Builder::new()
        .name("shelbi-wiki-sub".to_string())
        .spawn(move || {
            let connected = AtomicBool::new(false);
            let frame = shelbi_client::relay::services::subscribe_line(&project);
            while !stop_thread.load(Ordering::SeqCst) {
                let end = crate::changes::stream_lines(&frame, &stop_thread, &connected, |line| {
                    match wake_for(line, &project) {
                        Some(wake) => tx.send(wake).map_err(|_| ()),
                        None => Ok(()),
                    }
                });
                match end {
                    StreamEnd::ReceiverGone => return,
                    StreamEnd::Disconnected => crate::changes::sleep_slice(&stop_thread),
                }
            }
        })
        .ok();
    (ChangeSubscription::remote(stop, handle.into_iter().collect(), Vec::new()), rx)
}

/// Split whole lines out of `buf`, sending a wake for each of `project`'s
/// wiki changes. `Err(())` means the receiver is gone.
pub(crate) fn drain_lines(buf: &mut Vec<u8>, project: &str, tx: &Sender<WikiWake>) -> Result<(), ()> {
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=nl).collect();
        if let Some(wake) = wake_for(&line[..line.len() - 1], project) {
            tx.send(wake).map_err(|_| ())?;
        }
    }
    Ok(())
}

/// The wake `line` carries for `project`'s wiki, if any.
fn wake_for(line: &[u8], project: &str) -> Option<WikiWake> {
    let change = std::str::from_utf8(line).ok().and_then(ChangeNotification::from_line)?;
    if change.project() != project {
        return None;
    }
    match change {
        ChangeNotification::Wiki { paths, .. } => Some(WikiWake::Changed { paths }),
        ChangeNotification::WikiStatus { status, .. } => Some(WikiWake::Status(status)),
        ChangeNotification::WikiOpen { path, .. } => Some(WikiWake::Open(path)),
        ChangeNotification::Board { .. } | ChangeNotification::Workspace { .. } | ChangeNotification::Layout { .. } => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_projects_wiki_lines_wake() {
        let changed = br#"{"change":"wiki","project":"demo","paths":["Plans/a.md"],"source":"agent"}"#;
        assert_eq!(wake_for(changed, "demo"), Some(WikiWake::Changed { paths: vec!["Plans/a.md".into()] }));
        let status = br#"{"change":"wiki-status","project":"demo","status":"syncing"}"#;
        assert_eq!(wake_for(status, "demo"), Some(WikiWake::Status("syncing".into())));
        let open = br#"{"change":"wiki-open","project":"demo","path":"README.md"}"#;
        assert_eq!(wake_for(open, "demo"), Some(WikiWake::Open(Some("README.md".into()))));
        assert_eq!(wake_for(changed, "other"), None);
        assert_eq!(wake_for(br#"{"change":"board","project":"demo"}"#, "demo"), None);
        assert_eq!(wake_for(b"junk", "demo"), None);
    }

    #[test]
    fn lines_split_across_reads_are_assembled() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut buf = br#"{"change":"wiki-status","project":"demo","#.to_vec();
        drain_lines(&mut buf, "demo", &tx).unwrap();
        assert!(rx.try_recv().is_err());
        buf.extend_from_slice(b"\"status\":\"synced\"}\n");
        drain_lines(&mut buf, "demo", &tx).unwrap();
        assert_eq!(rx.try_recv().unwrap(), WikiWake::Status("synced".into()));
    }
}
