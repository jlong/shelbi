//! Snapshot a session's screen, whether it is alive or dead.
//!
//! A **live** session answers the [`snapshot`](crate::connect::Connection::snapshot)
//! request over its socket, rendering its emulator in the `capture-pane -p -J`
//! shape the orchestrator's detectors expect. A **dead** session has no socket:
//! on exit it writes its last screen (plus recent scrollback) to `final.txt`, so
//! a dead session's snapshot is read straight from that file.
//!
//! This is the one-call replacement for the `tmux capture-pane` tail Shelbi used
//! for crash records: the orchestrator no longer has to know whether a worker's
//! session is still up to read what its screen last said.

use std::path::Path;

use crate::connect::Connection;
use crate::discovery::DiscoveredSession;
use crate::error::ClientError;

/// Where a [`snapshot`] came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotSource {
    /// Read live from the session's emulator over its socket.
    Live,
    /// Read from a dead session's `final.txt` (the last screen written on exit).
    Final,
}

/// The rendered screen of a session, and where it was read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    /// The screen text, in `capture-pane -p -J` shape.
    pub text: String,
    /// Whether this came from the live session or its `final.txt`.
    pub source: SnapshotSource,
}

/// Snapshot `session`'s screen as text.
///
/// If the session is live, this connects and issues the `snapshot` request with
/// `history_lines` of scrollback (as [`Connection::snapshot`] does). If it is
/// dead — or it dies in the race between discovery and connecting — the last
/// screen is read from `final.txt` instead; `history_lines` does not apply to a
/// `final.txt` read, which already carries the scrollback captured at exit.
///
/// A dead session with no `final.txt` (killed before it could write one, or a
/// still-forming directory) surfaces as an [`Io`](ClientError::Io) not-found.
pub fn snapshot(
    session: &DiscoveredSession,
    history_lines: Option<u32>,
) -> Result<Snapshot, ClientError> {
    if session.alive {
        // `snapshot` is a frozen-core request, so no capability is required.
        if let Ok((conn, _events)) = Connection::open(&session.sock, None, &[]) {
            let data = conn.snapshot(history_lines)?;
            return Ok(Snapshot {
                text: data.text,
                source: SnapshotSource::Live,
            });
        }
        // Raced: the session died (or its socket went away) between discovery
        // and connect. Fall through to its final screen.
    }
    read_final(&session.dir)
}

/// Read a dead session's last screen from `<dir>/final.txt`.
fn read_final(dir: &Path) -> Result<Snapshot, ClientError> {
    let text = std::fs::read_to_string(dir.join("final.txt"))?;
    Ok(Snapshot {
        text,
        source: SnapshotSource::Final,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_session::Meta;

    /// A dead [`DiscoveredSession`] rooted at `dir` (no live socket).
    fn dead_session(dir: &Path) -> DiscoveredSession {
        DiscoveredSession {
            short_id: "deadbeefdeadbeef".into(),
            dir: dir.to_path_buf(),
            sock: dir.join("sock"),
            meta: Meta {
                id: "deadbeefdeadbeef".into(),
                name: "demo/ws/alice".into(),
                argv: vec!["claude".into()],
                cwd: dir.to_path_buf(),
                task: None,
                launched_at: "2026-10-03T00:00:00Z".into(),
                protocol_version: shelbi_proto::PROTOCOL_VERSION,
            },
            alive: false,
        }
    }

    #[test]
    fn dead_session_snapshot_comes_from_final_txt() {
        let dir = tempfile::tempdir().unwrap();
        let screen = "agent exited\n  last line of the final screen";
        std::fs::write(dir.path().join("final.txt"), screen).unwrap();

        let snap = snapshot(&dead_session(dir.path()), None).expect("final.txt snapshot");
        assert_eq!(snap.source, SnapshotSource::Final);
        assert_eq!(snap.text, screen);
    }

    #[test]
    fn history_lines_are_ignored_for_a_final_txt_read() {
        // `final.txt` already carries the scrollback captured at exit, so a
        // requested history count doesn't change what a dead session returns.
        let dir = tempfile::tempdir().unwrap();
        let screen = "final screen contents";
        std::fs::write(dir.path().join("final.txt"), screen).unwrap();

        let snap = snapshot(&dead_session(dir.path()), Some(500)).expect("final.txt snapshot");
        assert_eq!(snap.source, SnapshotSource::Final);
        assert_eq!(snap.text, screen);
    }

    #[test]
    fn dead_session_without_final_txt_is_a_not_found_error() {
        let dir = tempfile::tempdir().unwrap();
        let err = snapshot(&dead_session(dir.path()), None).unwrap_err();
        match err {
            ClientError::Io(e) => assert_eq!(e.kind(), std::io::ErrorKind::NotFound),
            other => panic!("expected a not-found io error, got {other:?}"),
        }
    }
}
