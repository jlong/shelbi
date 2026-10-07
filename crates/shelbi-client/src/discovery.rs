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

/// Among all discovered `sessions`, choose the one a client should attach to for
/// logical `name`: a *usable* session (lock held **and** socket accepting),
/// preferring the most recently launched. Returns `None` when no live session
/// carries the name.
///
/// Duplicates by name are normal — a relaunch races a not-yet-reaped predecessor,
/// and an "alive but not listening" zombie ([`DiscoveredSession::socket_refusing`])
/// lingers beside its replacement — so a lookup must choose *deliberately* rather
/// than take whatever [`list`] happened to return first (its order is the
/// unsorted `read_dir` order). Picking the zombie strands the client on a connect
/// that the socket refuses (`rt-re-entering-a-review-fails-to-attach`).
///
/// **Cheap by design.** With a single live candidate it is returned *without* a
/// socket probe: the caller's real connect follows immediately and surfaces a
/// dead socket on its own (the attach path retries a refused/not-bound socket),
/// so a redundant probe would only double the work. The probe runs only to
/// disambiguate two or more live candidates — the duplicate case — and there it
/// never knowingly returns a refusing socket.
pub fn choose_session(sessions: &[DiscoveredSession], name: &str) -> Option<DiscoveredSession> {
    let mut live: Vec<&DiscoveredSession> = sessions
        .iter()
        .filter(|s| s.alive && s.meta.name == name)
        .collect();
    match live.len() {
        0 => return None,
        // Single candidate: trust the lock and skip the probe (see the doc note).
        1 => return Some(live[0].clone()),
        _ => {}
    }
    // Several share the name. Newest first, so a fresh replacement is preferred
    // over a stale predecessor. `launched_at` is a fixed-format RFC3339 UTC stamp
    // from one producer, so a lexical compare orders them correctly.
    live.sort_by(|a, b| b.meta.launched_at.cmp(&a.meta.launched_at));
    // Probe each once, newest first, and take the first socket actually accepting
    // connections — never a zombie over a live one.
    let probed: Vec<(&DiscoveredSession, SocketReachability)> =
        live.iter().map(|s| (*s, probe_socket(&s.sock))).collect();
    if let Some((s, _)) = probed
        .iter()
        .find(|(_, r)| *r == SocketReachability::Reachable)
    {
        return Some((*s).clone());
    }
    // None is accepting yet. Prefer one merely still *binding* its socket
    // (`NotBound`, a replacement coming up) over a refusing zombie, newest first,
    // so the attach path retries into the real session instead of the zombie.
    // Never return a `Refusing` candidate here.
    probed
        .iter()
        .find(|(_, r)| *r != SocketReachability::Refusing)
        .map(|(s, _)| (*s).clone())
}

/// Among `sessions`, the "alive but not listening" zombies sharing `name` that a
/// supervisor should reap: every live candidate whose socket *refuses* connects
/// ([`DiscoveredSession::socket_refusing`]), **except** the most recently
/// launched live one — kept as the intended session (a replacement that exists or
/// is still coming up). Returns an empty list unless the name has more than one
/// live candidate, so a lone session is never reaped out from under the slot it
/// still nominally owns (`rt-re-entering-a-review-fails-to-attach`).
///
/// Pure selection: the caller does the termination and directory removal.
pub fn zombies_to_reap<'a>(
    sessions: &'a [DiscoveredSession],
    name: &str,
) -> Vec<&'a DiscoveredSession> {
    let mut live: Vec<&DiscoveredSession> = sessions
        .iter()
        .filter(|s| s.alive && s.meta.name == name)
        .collect();
    if live.len() < 2 {
        return Vec::new();
    }
    // Newest first; keep `live[0]` (the replacement), reap older refusing ones.
    live.sort_by(|a, b| b.meta.launched_at.cmp(&a.meta.launched_at));
    live[1..]
        .iter()
        .copied()
        .filter(|s| s.socket_refusing())
        .collect()
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
        named_at(dir, sock, alive, "demo/ws/review", "1970-01-01T00:00:00Z")
    }

    /// A [`DiscoveredSession`] with an explicit name and launch time, so a test
    /// can set up two same-named candidates and assert which one is chosen.
    fn named_at(
        dir: &Path,
        sock: PathBuf,
        alive: bool,
        name: &str,
        launched_at: &str,
    ) -> DiscoveredSession {
        DiscoveredSession {
            short_id: "test".into(),
            dir: dir.to_path_buf(),
            sock,
            meta: Meta {
                id: "test".into(),
                name: name.into(),
                argv: vec!["cat".into()],
                cwd: dir.to_path_buf(),
                task: None,
                launched_at: launched_at.into(),
                protocol_version: shelbi_proto::PROTOCOL_VERSION,
                pid: 0,
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
    fn choose_session_returns_none_when_no_live_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let dead = named_at(dir.path(), dir.path().join("sock"), false, "demo/ws/x", "t1");
        assert!(choose_session(&[dead], "demo/ws/x").is_none());
        assert!(choose_session(&[], "demo/ws/x").is_none());
    }

    #[test]
    fn choose_session_returns_the_lone_live_candidate_without_probing() {
        // A single live candidate is returned as-is (no socket probe): its path
        // need not even be a real socket, proving nothing was connected to.
        let dir = tempfile::tempdir().unwrap();
        let only = named_at(
            dir.path(),
            dir.path().join("no-such-sock"),
            true,
            "demo/ws/x",
            "t1",
        );
        let chosen = choose_session(&[only], "demo/ws/x").expect("the lone live candidate");
        assert_eq!(chosen.meta.launched_at, "t1");
    }

    #[test]
    fn choose_session_prefers_the_live_socket_over_a_refusing_zombie() {
        // The crux (`rt-re-entering-a-review-fails-to-attach`): two sessions share
        // a name — one a live listener, one a zombie whose socket refuses — and
        // the pick must be the live one *regardless of list order*, never the
        // zombie.
        let live_dir = tempfile::tempdir().unwrap();
        let live_sock = live_dir.path().join("sock");
        let _listener = UnixListener::bind(&live_sock).unwrap();

        let zombie_dir = tempfile::tempdir().unwrap();
        let zombie_sock = zombie_dir.path().join("sock");
        let z = UnixListener::bind(&zombie_sock).unwrap();
        drop(z);
        if !zombie_sock.exists() {
            return; // platform removed the socket file on drop; refusing case N/A
        }

        // The zombie is the *newer* one, so a naive "prefer newest" without a
        // probe would wrongly choose it.
        let live = named_at(live_dir.path(), live_sock, true, "demo/ws/x", "t1");
        let zombie = named_at(zombie_dir.path(), zombie_sock, true, "demo/ws/x", "t2");

        for order in [
            vec![live.clone(), zombie.clone()],
            vec![zombie.clone(), live.clone()],
        ] {
            let chosen = choose_session(&order, "demo/ws/x").expect("a usable session");
            assert_eq!(
                chosen.meta.launched_at, "t1",
                "the live listener must win over the refusing zombie, any order",
            );
        }
    }

    #[test]
    fn choose_session_prefers_the_newest_reachable_of_several() {
        // Two live listeners share a name: the most recently launched wins.
        let a_dir = tempfile::tempdir().unwrap();
        let a_sock = a_dir.path().join("sock");
        let _la = UnixListener::bind(&a_sock).unwrap();
        let b_dir = tempfile::tempdir().unwrap();
        let b_sock = b_dir.path().join("sock");
        let _lb = UnixListener::bind(&b_sock).unwrap();

        let older = named_at(a_dir.path(), a_sock, true, "demo/ws/x", "2026-01-01T00:00:00Z");
        let newer = named_at(b_dir.path(), b_sock, true, "demo/ws/x", "2026-02-01T00:00:00Z");
        let chosen = choose_session(&[older, newer], "demo/ws/x").expect("a usable session");
        assert_eq!(chosen.meta.launched_at, "2026-02-01T00:00:00Z");
    }

    #[test]
    fn zombies_to_reap_is_empty_without_a_duplicate() {
        // A lone live session (zombie or not) is never reaped — there is no
        // replacement to reap it against.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let z = UnixListener::bind(&sock).unwrap();
        drop(z);
        if !sock.exists() {
            return;
        }
        let lone = named_at(dir.path(), sock, true, "demo/ws/x", "t1");
        assert!(zombies_to_reap(&[lone], "demo/ws/x").is_empty());
    }

    #[test]
    fn zombies_to_reap_drops_the_stale_refusing_one_keeping_the_replacement() {
        // The reap case: a live listener and an older refusing zombie share a
        // name. The zombie is reaped; the live replacement is kept.
        let live_dir = tempfile::tempdir().unwrap();
        let live_sock = live_dir.path().join("sock");
        let _listener = UnixListener::bind(&live_sock).unwrap();

        let zombie_dir = tempfile::tempdir().unwrap();
        let zombie_sock = zombie_dir.path().join("sock");
        let z = UnixListener::bind(&zombie_sock).unwrap();
        drop(z);
        if !zombie_sock.exists() {
            return;
        }

        // The live one is the newer (the replacement), the zombie older.
        let live = named_at(live_dir.path(), live_sock, true, "demo/ws/x", "t2");
        let zombie = named_at(zombie_dir.path(), zombie_sock, true, "demo/ws/x", "t1");

        let candidates = [live, zombie.clone()];
        let reap = zombies_to_reap(&candidates, "demo/ws/x");
        assert_eq!(reap.len(), 1, "exactly the one zombie is reaped");
        assert_eq!(reap[0].meta.launched_at, "t1", "and it is the stale one");
    }

    #[test]
    fn zombies_to_reap_never_reaps_the_newest_even_if_it_is_refusing() {
        // If the *newest* live session is itself still refusing (a replacement
        // mid-startup) and an older one also refuses, we keep the newest and reap
        // only the older — never leaving the slot with nothing intended.
        let a_dir = tempfile::tempdir().unwrap();
        let a_sock = a_dir.path().join("sock");
        let za = UnixListener::bind(&a_sock).unwrap();
        drop(za);
        let b_dir = tempfile::tempdir().unwrap();
        let b_sock = b_dir.path().join("sock");
        let zb = UnixListener::bind(&b_sock).unwrap();
        drop(zb);
        if !a_sock.exists() || !b_sock.exists() {
            return;
        }
        let older = named_at(a_dir.path(), a_sock, true, "demo/ws/x", "t1");
        let newer = named_at(b_dir.path(), b_sock, true, "demo/ws/x", "t2");
        let candidates = [older, newer];
        let reap = zombies_to_reap(&candidates, "demo/ws/x");
        assert_eq!(reap.len(), 1);
        assert_eq!(reap[0].meta.launched_at, "t1", "the newest is kept");
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
