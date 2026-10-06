//! Discover sessions by scanning the sessions directory (`~/.shelbi/sessions/`).
//!
//! There is no central registry (the cost accepted for one-process-per-session):
//! sessions are found by listing the directory. Each subdirectory is a short
//! hash holding `sock`, `lock`, `meta.json`, and after exit `exit.json` and
//! `final.txt`. The directory name is a short hash rather than the session name
//! to stay under the 104-byte socket-path limit on macOS;
//! [`Meta`](shelbi_session::Meta) carries the readable name.
//!
//! A session whose `lock` is not held is **dead**, and its directory is stale
//! state to be reaped ([`reap_dead`]). Liveness is the same non-blocking `flock`
//! probe the session uses for its lifetime lock
//! ([`shelbi_session::lock::is_held`]).
//!
//! The scanning root is passed in rather than resolved here, so the crate needs
//! no dependency on `shelbi-state`; a caller passes `shelbi_state::sessions_dir()`.

use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use shelbi_session::lock::is_held;
use shelbi_session::Meta;

/// A discovered session directory: its short id, on-disk paths, parsed metadata,
/// and whether it is still alive (its lock held).
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    /// Short hash that names the directory under the sessions root.
    pub short_id: String,
    /// The session directory.
    pub dir: PathBuf,
    /// The Unix socket to connect to (present whether or not the session lives).
    pub sock: PathBuf,
    /// Parsed `meta.json`.
    pub meta: Meta,
    /// Whether the session's lock is currently held (i.e. it is live).
    pub alive: bool,
}

/// Whether a session's socket is actually accepting connections — the second
/// half of liveness the lock alone can't tell you.
///
/// The lifetime lock proves the *process* is up; it does **not** prove the
/// process is still *listening*. A session whose accept loop died (or whose
/// listener fd was closed) while the process lived on holds its lock yet refuses
/// every connect — the "alive but not listening" zombie that strands every
/// client on "Connecting…". This classifies that case apart from a session that
/// is simply still binding its socket at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketReachability {
    /// The socket exists and a connect succeeded — the listener is up.
    Reachable,
    /// No socket file yet: a session still binding its `sock` at startup. Not a
    /// zombie — the listener is expected to appear a beat later.
    NotBound,
    /// The socket is a real socket file but a connect was **refused**: the
    /// listener is gone though the process lives. The zombie.
    Refusing,
    /// Anything else (a non-socket path, a permission or transport error we
    /// can't classify). Treated conservatively as *not* a zombie, so an
    /// ambiguous read never triggers a relaunch.
    Unknown,
}

/// Probe whether the session socket at `sock` is accepting connections, without
/// performing the (blocking) hello handshake — a bare connect is enough to tell
/// a live listener (connect succeeds) from a zombie (connect refused) from a
/// still-starting session (no socket file yet).
///
/// Only a genuine refused connect on a real socket file is reported as
/// [`Refusing`](SocketReachability::Refusing): a missing file is
/// [`NotBound`](SocketReachability::NotBound) (startup), and a non-socket path or
/// any other error is [`Unknown`](SocketReachability::Unknown), so a fabricated
/// or unexpected path is never mistaken for a zombie.
pub fn probe_socket(sock: &Path) -> SocketReachability {
    match std::fs::symlink_metadata(sock) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketReachability::NotBound,
        Err(_) => return SocketReachability::Unknown,
        // A path that exists but is not a socket is not ours to judge: connecting
        // to a regular file has platform-dependent errors, so never read it as a
        // refusing listener.
        Ok(m) if !m.file_type().is_socket() => return SocketReachability::Unknown,
        Ok(_) => {}
    }
    match UnixStream::connect(sock) {
        Ok(_) => SocketReachability::Reachable,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => SocketReachability::Refusing,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => SocketReachability::NotBound,
        Err(_) => SocketReachability::Unknown,
    }
}

impl DiscoveredSession {
    /// Whether this (lock-held) session's socket is refusing connections — the
    /// "alive but not listening" zombie. A session whose lock is **not** held is
    /// already dead by the ordinary liveness probe, so this only ever reports
    /// `true` for a live-by-lock session whose listener is gone.
    pub fn socket_refusing(&self) -> bool {
        self.alive && probe_socket(&self.sock) == SocketReachability::Refusing
    }

    /// Whether this session is actually **usable**: its lock is held *and* its
    /// socket is accepting connections. A locked-but-refusing session (a zombie)
    /// is not usable — callers that pick a session to attach, probe, or enumerate
    /// should treat it as gone so supervision relaunches it.
    pub fn usable(&self) -> bool {
        self.alive && !self.socket_refusing()
    }
}

/// Enumerate sessions under `root`, newest unspecified order. Directories with
/// no readable `meta.json` are skipped (a session mid-creation, or a non-session
/// directory). Does not delete anything — see [`reap_dead`].
///
/// A missing `root` is not an error: it means no session has ever started.
pub fn list(root: &Path) -> Result<Vec<DiscoveredSession>, crate::ClientError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let meta_path = dir.join("meta.json");
        let meta = match std::fs::read_to_string(&meta_path).ok().and_then(|s| Meta::from_json(&s).ok()) {
            Some(m) => m,
            None => continue, // not a session dir, or still being created
        };
        let short_id = entry.file_name().to_string_lossy().into_owned();
        out.push(DiscoveredSession {
            sock: dir.join("sock"),
            alive: is_held(&dir.join("lock")),
            short_id,
            dir,
            meta,
        });
    }
    Ok(out)
}

/// Remove every session directory under `root` whose lock is **not held** (the
/// session is dead), returning the short ids reaped. A caller that still wants a
/// dead session's `exit.json` / `final.txt` reads them before calling this.
///
/// A directory that cannot be removed is skipped (best-effort cleanup), not an
/// error; a missing `root` reaps nothing.
pub fn reap_dead(root: &Path) -> Result<Vec<String>, crate::ClientError> {
    let mut reaped = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(reaped),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Only reap things that look like session directories (have a lock file),
        // so an unrelated directory under the root is never deleted.
        let lock = dir.join("lock");
        if !lock.exists() {
            continue;
        }
        if !is_held(&lock) && std::fs::remove_dir_all(&dir).is_ok() {
            reaped.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(reaped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn probe_socket_reports_a_bound_listener_reachable() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        assert_eq!(probe_socket(&sock), SocketReachability::Reachable);
    }

    #[test]
    fn probe_socket_reports_a_missing_socket_not_bound() {
        let dir = tempfile::tempdir().unwrap();
        // A session still binding its `sock`: the file isn't there yet. This must
        // never read as a zombie (that would relaunch a session mid-startup).
        assert_eq!(
            probe_socket(&dir.path().join("sock")),
            SocketReachability::NotBound
        );
    }

    #[test]
    fn probe_socket_reports_a_dead_listener_refusing() {
        // Bind a listener, capture its path, then drop it: the socket file is
        // left behind (as a session's teardown-crash would leave it) but connects
        // are now refused — the "alive but not listening" zombie's signature.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        // Dropping the listener unbinds it; the path may or may not remain, so
        // re-create the socket file shape only if the drop removed it. On Unix
        // the file persists after drop, so a connect is refused.
        drop(listener);
        if sock.exists() {
            assert_eq!(probe_socket(&sock), SocketReachability::Refusing);
        } else {
            // The platform removed the file on drop; then it's NotBound, which is
            // also correctly "not a reachable listener".
            assert_eq!(probe_socket(&sock), SocketReachability::NotBound);
        }
    }

    fn discovered(dir: &Path, sock: PathBuf, alive: bool) -> DiscoveredSession {
        DiscoveredSession {
            short_id: "test".into(),
            dir: dir.to_path_buf(),
            sock,
            meta: Meta {
                id: "test".into(),
                name: "demo/ws/review".into(),
                argv: vec!["cat".into()],
                cwd: dir.to_path_buf(),
                task: None,
                launched_at: "1970-01-01T00:00:00Z".into(),
                protocol_version: shelbi_proto::PROTOCOL_VERSION,
            },
            alive,
        }
    }

    #[test]
    fn a_live_session_with_a_reachable_socket_is_usable() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let _listener = UnixListener::bind(&sock).unwrap();
        let s = discovered(dir.path(), sock, true);
        assert!(s.usable(), "a live, listening session is usable");
        assert!(!s.socket_refusing());
    }

    #[test]
    fn a_live_session_whose_socket_refuses_is_a_zombie_and_not_usable() {
        // Lock held (alive=true) but the listener is gone: the exact "alive but
        // not listening" zombie. It must read as not usable so supervision
        // relaunches it instead of reusing the stranded slot.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener);
        if !sock.exists() {
            // Platform removed the file on drop; the refusing case can't be
            // reproduced here, so skip (the NotBound path is covered elsewhere).
            return;
        }
        let s = discovered(dir.path(), sock, true);
        assert!(s.socket_refusing(), "a refusing live socket is a zombie");
        assert!(!s.usable(), "a zombie is not usable");
    }

    #[test]
    fn a_dead_session_is_never_reported_as_a_zombie() {
        // alive=false is already dead by the ordinary lock probe; socket_refusing
        // only ever speaks to live-by-lock sessions, so it stays false.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener);
        let s = discovered(dir.path(), sock, false);
        assert!(!s.socket_refusing());
        assert!(!s.usable());
    }

    #[test]
    fn probe_socket_never_calls_a_non_socket_path_a_zombie() {
        // A fabricated `sock` that is a regular file (a test fixture, say) must
        // not be mistaken for a refusing listener: connecting to a non-socket has
        // platform-dependent errors, so it reads as Unknown, never Refusing.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        std::fs::write(&sock, b"not a socket").unwrap();
        assert_eq!(probe_socket(&sock), SocketReachability::Unknown);
    }
}
