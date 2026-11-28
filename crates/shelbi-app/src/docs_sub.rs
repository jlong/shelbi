//! The docs' pushed changes, for a client showing it: the hub's docs
//! service announces every change to a document (`docs_changed`), every
//! change of its sync status, and a `shelbi docs open`, on the change bus
//! ([`shelbi_state::ChangeNotification::Docs`] and its siblings). A client
//! refreshes its tree (and an open document) from them, so an edit made
//! anywhere shows up at once.
//!
//! Its own stream, beside [`crate::changes`]' board/workspace wakes, so a
//! docs edit never costs a board refresh. A remote project's comes over the
//! relay ([`crate::source::remote::subscribe_docs`]); the shape is the same.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::Arc;
use std::thread;

use shelbi_state::ChangeNotification;

use crate::changes::{ChangeSubscription, StreamEnd};

/// A change to the project's docs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocsWake {
    /// Documents (and folders' files) changed: their docs-relative paths,
    /// and who changed them (`local`, `agent` or `pull`).
    Changed { paths: Vec<String>, source: String },
    /// The sync's status is now this word (`synced`, `syncing`, `pending`,
    /// `offline`, `conflict`, `error <detail>`).
    Status(String),
    /// Someone asked to see the docs at this path (its root when `None`).
    Open(Option<String>),
}

/// Follow `project`'s docs on this machine's hub. Dropping the handle stops
/// its thread.
pub fn spawn(project: &str) -> (ChangeSubscription, Receiver<DocsWake>) {
    let (tx, rx) = std::sync::mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let project = project.to_string();
    let handle = thread::Builder::new()
        .name("shelbi-docs-sub".to_string())
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
/// docs change. `Err(())` means the receiver is gone.
pub(crate) fn drain_lines(buf: &mut Vec<u8>, project: &str, tx: &Sender<DocsWake>) -> Result<(), ()> {
    while let Some(nl) = buf.iter().position(|&b| b == b'\n') {
        let line: Vec<u8> = buf.drain(..=nl).collect();
        if let Some(wake) = wake_for(&line[..line.len() - 1], project) {
            tx.send(wake).map_err(|_| ())?;
        }
    }
    Ok(())
}

/// The wake `line` carries for `project`'s docs, if any.
fn wake_for(line: &[u8], project: &str) -> Option<DocsWake> {
    let change = std::str::from_utf8(line).ok().and_then(ChangeNotification::from_line)?;
    if change.project() != project {
        return None;
    }
    match change {
        ChangeNotification::Docs { paths, source, .. } => Some(DocsWake::Changed { paths, source }),
        ChangeNotification::DocsStatus { status, .. } => Some(DocsWake::Status(status)),
        ChangeNotification::DocsOpen { path, .. } => Some(DocsWake::Open(path)),
        ChangeNotification::Board { .. } | ChangeNotification::Workspace { .. } | ChangeNotification::Layout { .. } => {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_projects_docs_lines_wake() {
        let changed = br#"{"change":"docs","project":"demo","paths":["Plans/a.md"],"source":"agent"}"#;
        assert_eq!(wake_for(changed, "demo"), Some(DocsWake::Changed { paths: vec!["Plans/a.md".into()], source: "agent".into() }));
        let status = br#"{"change":"docs-status","project":"demo","status":"syncing"}"#;
        assert_eq!(wake_for(status, "demo"), Some(DocsWake::Status("syncing".into())));
        let open = br#"{"change":"docs-open","project":"demo","path":"README.md"}"#;
        assert_eq!(wake_for(open, "demo"), Some(DocsWake::Open(Some("README.md".into()))));
        assert_eq!(wake_for(changed, "other"), None);
        assert_eq!(wake_for(br#"{"change":"board","project":"demo"}"#, "demo"), None);
        assert_eq!(wake_for(b"junk", "demo"), None);
    }

    #[test]
    fn lines_split_across_reads_are_assembled() {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut buf = br#"{"change":"docs-status","project":"demo","#.to_vec();
        drain_lines(&mut buf, "demo", &tx).unwrap();
        assert!(rx.try_recv().is_err());
        buf.extend_from_slice(b"\"status\":\"synced\"}\n");
        drain_lines(&mut buf, "demo", &tx).unwrap();
        assert_eq!(rx.try_recv().unwrap(), DocsWake::Status("synced".into()));
    }
}
