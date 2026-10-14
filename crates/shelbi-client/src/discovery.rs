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
use std::time::Duration;

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
    /// The connect **succeeded** but the session never answered the hello
    /// handshake within the probe deadline (or dropped the connection before
    /// replying): the accept loop still runs but the per-connection handlers are
    /// wedged, so every connect succeeds yet no client can attach. Detected only
    /// by [`probe_reachable`] (the hello round-trip), never by the bare
    /// [`probe_socket`] connect (`rt-review-session-wedges-after-repeated-attaches`).
    Wedged,
    /// Anything else (a non-socket path, a permission or transport error we
    /// can't classify). Treated conservatively as *not* a zombie, so an
    /// ambiguous read never triggers a relaunch.
    Unknown,
}

/// How long [`probe_reachable`] waits for a session's hello reply before
/// declaring it [`Wedged`](SocketReachability::Wedged). Short — a healthy local
/// session answers a fresh connection's hello in well under a millisecond, and a
/// dedicated probe connection contends with nothing — but generous enough that a
/// momentarily busy machine is never falsely reaped.
pub const HELLO_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

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

/// Probe whether the session socket at `sock` is not just *accepting* connections
/// but actually *answering* the protocol: connect, then exchange the hello
/// handshake bounded by [`HELLO_PROBE_TIMEOUT`].
///
/// This is the liveness probe supervision needs. A bare [`probe_socket`] connect
/// reports a *wedged* session — one whose accept loop runs but whose
/// per-connection handlers are all blocked (a leaked write to a gone client that
/// never completes, so threads and descriptors pile up until no new connection
/// can be served) — as [`Reachable`](SocketReachability::Reachable), because the
/// connect itself still succeeds. Only the hello round-trip tells them apart: a
/// wedged session accepts the probe connection but never sends its hello, so it
/// is reported [`Wedged`](SocketReachability::Wedged) and supervision relaunches
/// and reaps it instead of stranding every client on it forever
/// (`rt-review-session-wedges-after-repeated-attaches`).
///
/// Classification mirrors [`probe_socket`] for the connect itself (missing file →
/// [`NotBound`](SocketReachability::NotBound), refused → [`Refusing`](SocketReachability::Refusing),
/// non-socket or odd error → [`Unknown`](SocketReachability::Unknown)); the hello
/// round-trip then refines a successful connect into
/// [`Reachable`](SocketReachability::Reachable) or
/// [`Wedged`](SocketReachability::Wedged).
pub fn probe_reachable(sock: &Path) -> SocketReachability {
    match std::fs::symlink_metadata(sock) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketReachability::NotBound,
        Err(_) => return SocketReachability::Unknown,
        Ok(m) if !m.file_type().is_socket() => return SocketReachability::Unknown,
        Ok(_) => {}
    }
    let stream = match UnixStream::connect(sock) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
            return SocketReachability::Refusing
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return SocketReachability::NotBound,
        Err(_) => return SocketReachability::Unknown,
    };
    match crate::connect::probe_handshake(&stream, HELLO_PROBE_TIMEOUT) {
        Ok(()) => SocketReachability::Reachable,
        // Accepted the connection but never completed the hello (timed out) or
        // dropped it before replying (EOF): the wedged session.
        Err(crate::ClientError::HandshakeTimeout) | Err(crate::ClientError::UnexpectedEof) => {
            SocketReachability::Wedged
        }
        // Any other transport error after a successful connect is ambiguous;
        // treat it conservatively as not-a-zombie so a flaky read never triggers
        // a relaunch.
        Err(_) => SocketReachability::Unknown,
    }
}

impl DiscoveredSession {
    /// This (lock-held) session's reachability over its socket, via the full
    /// hello round-trip ([`probe_reachable`]) rather than a bare connect — so a
    /// *wedged* session (accepts connections but never answers) is distinguished
    /// from a healthy one. Only meaningful for a live-by-lock session; a dead
    /// session's socket is already gone or refusing.
    pub fn reachability(&self) -> SocketReachability {
        probe_reachable(&self.sock)
    }

    /// Whether this (lock-held) session accepts connections but does not answer
    /// the protocol — the "alive but not listening" zombie, in either of its two
    /// shapes: the listener is gone so connects are **refused**
    /// ([`Refusing`](SocketReachability::Refusing)), or the listener runs but the
    /// per-connection handlers are **wedged** so a connect succeeds yet the hello
    /// never comes ([`Wedged`](SocketReachability::Wedged)). Either way no client
    /// can attach, so supervision should relaunch and reap it. A session whose
    /// lock is **not** held is already dead by the ordinary liveness probe, so
    /// this only ever reports `true` for a live-by-lock session.
    pub fn not_listening(&self) -> bool {
        self.alive
            && matches!(
                self.reachability(),
                SocketReachability::Refusing | SocketReachability::Wedged
            )
    }

    /// Whether this session is actually **usable**: its lock is held *and* its
    /// socket is answering the protocol. A locked-but-not-listening session (a
    /// refusing *or* wedged zombie) is not usable — callers that pick a session to
    /// attach, probe, or enumerate should treat it as gone so supervision
    /// relaunches it. A session still binding its socket at startup
    /// ([`NotBound`](SocketReachability::NotBound)) or giving an ambiguous read
    /// ([`Unknown`](SocketReachability::Unknown)) is left usable, so neither a
    /// startup race nor a flaky probe is mistaken for a dead session.
    pub fn usable(&self) -> bool {
        self.alive && !self.not_listening()
    }

    /// Whether this session has already begun exiting: its child is gone and it
    /// has written its `exiting` marker (just before pushing `exited` to its
    /// clients) or its `exit.json`. Such a session still holds its lock and
    /// still accepts connections through its exit flush grace, so it can read as
    /// [`alive`](Self::alive) and even [`Reachable`](SocketReachability::Reachable),
    /// yet a client attaching to it is dropped a moment later and sees a second
    /// death. Read fresh from disk on every call (not cached by [`list`]), so a
    /// session that started exiting after the scan is still caught at pick time
    /// (`client-connect-skips-exited-sessions`).
    pub fn exiting(&self) -> bool {
        self.dir.join("exiting").exists() || self.dir.join("exit.json").exists()
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
            alive: is_live(&dir),
            short_id,
            dir,
            meta,
        });
    }
    Ok(out)
}

/// Whether the session in `dir` is alive: its lifetime lock is held **and** it
/// has not written its exit record. `exit.json` is written only after the child
/// is gone (and after `final.txt`), so its presence is proof of death even in
/// the instant before the session process exits and drops its lock — a dead
/// session never reads as alive off a lock read alone
/// (`phantom-working-dead-session`).
fn is_live(dir: &Path) -> bool {
    !dir.join("exit.json").exists() && is_held(&dir.join("lock"))
}

/// Remove **every** session directory under `root` whose lock is not held (the
/// session is dead), returning the short ids reaped. Equivalent to
/// [`reap_dead_keeping_recent`] with `keep_per_name = 0`. A caller that still
/// wants a dead session's `exit.json` / `final.txt` reads them before calling
/// this.
///
/// A directory that cannot be removed is skipped (best-effort cleanup), not an
/// error; a missing `root` reaps nothing.
pub fn reap_dead(root: &Path) -> Result<Vec<String>, crate::ClientError> {
    reap_dead_keeping_recent(root, 0)
}

/// Remove dead session directories under `root`, but keep the `keep_per_name`
/// most recently launched dead directories for each logical session name — so a
/// just-relaunched session's predecessor stays around for a `final.txt` /
/// crash-record read while *older* duplicates of the same name are pruned.
///
/// Returns the short ids reaped. A **live** session (lock held) is never
/// removed and never counts against `keep_per_name` (the kept-dead budget is for
/// post-exit screens, which a live session doesn't need). "Most recent" is by
/// `meta.launched_at` (a fixed-format RFC3339 UTC stamp from one producer, so a
/// lexical compare orders it); a dead directory with no readable `meta.json` is
/// treated as the oldest (reaped first). A directory that cannot be removed is
/// skipped (best-effort), not an error; a missing `root` reaps nothing.
///
/// This is the automatic prune that keeps `~/.shelbi/sessions` from filling with
/// the exited predecessors every relaunch leaves behind — the pile-up that made
/// `shelbi session snapshot <workspace>` ambiguous ("matches several sessions")
/// and the directory grow without bound (`rt-review-slot-relaunch-loop`).
pub fn reap_dead_keeping_recent(
    root: &Path,
    keep_per_name: usize,
) -> Result<Vec<String>, crate::ClientError> {
    use std::collections::HashMap;
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    };

    // A dead session directory and the sort key we prune by.
    struct Dead {
        dir: PathBuf,
        short_id: String,
        // `launched_at` from meta.json, or "" when unreadable (sorts oldest).
        launched_at: String,
    }
    let mut by_name: HashMap<String, Vec<Dead>> = HashMap::new();

    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Only consider things that look like session directories (have a lock
        // file), so an unrelated directory under the root is never deleted.
        let lock = dir.join("lock");
        if !lock.exists() {
            continue;
        }
        // A live session is off-limits — never removed, never counted.
        if is_held(&lock) {
            continue;
        }
        let short_id = entry.file_name().to_string_lossy().into_owned();
        // The name keys the per-name budget; an unreadable meta sorts oldest and
        // is grouped under its own short id so it is still eligible for pruning.
        let (name, launched_at) = std::fs::read_to_string(dir.join("meta.json"))
            .ok()
            .and_then(|s| Meta::from_json(&s).ok())
            .map(|m| (m.name, m.launched_at))
            .unwrap_or_else(|| (format!("<unknown>/{short_id}"), String::new()));
        by_name.entry(name).or_default().push(Dead {
            dir,
            short_id,
            launched_at,
        });
    }

    let mut reaped = Vec::new();
    for (_name, mut deads) in by_name {
        // Newest first, so the first `keep_per_name` are the ones to keep.
        deads.sort_by(|a, b| b.launched_at.cmp(&a.launched_at));
        for d in deads.into_iter().skip(keep_per_name) {
            if std::fs::remove_dir_all(&d.dir).is_ok() {
                reaped.push(d.short_id);
            }
        }
    }
    Ok(reaped)
}

/// Among all discovered `sessions`, choose the one a client should attach to for
/// logical `name`: a *usable* session (lock held **and** socket accepting),
/// preferring the most recently launched. Returns `None` when no live session
/// carries the name.
///
/// A session that has already begun exiting ([`DiscoveredSession::exiting`]) is
/// never chosen, even as the lone candidate: across a reload or crash relaunch
/// the dying predecessor keeps its lock and keeps accepting for a moment after
/// it has pushed `exited`, and binding to it drops the client straight back out,
/// costing a second restart and its backoff. Skipping it leaves the replacement
/// (or, if it has not appeared yet, `None`, which the attach paths retry) as the
/// answer (`client-connect-skips-exited-sessions`).
///
/// Duplicates by name are normal — a relaunch races a not-yet-reaped predecessor,
/// and an "alive but not listening" zombie ([`DiscoveredSession::not_listening`])
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
        .filter(|s| s.alive && s.meta.name == name && !s.exiting())
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
    // Probe each once, newest first, with the full hello round-trip so a *wedged*
    // session (accepts connections but never answers) is excluded just like a
    // refusing one. Take the first socket actually answering the protocol — never
    // a zombie over a live one.
    let probed: Vec<(&DiscoveredSession, SocketReachability)> =
        live.iter().map(|s| (*s, probe_reachable(&s.sock))).collect();
    if let Some((s, _)) = probed
        .iter()
        .find(|(_, r)| *r == SocketReachability::Reachable)
    {
        return Some((*s).clone());
    }
    // None is answering yet. Prefer one merely still *binding* its socket
    // (`NotBound`, a replacement coming up) over a refusing or wedged zombie,
    // newest first, so the attach path retries into the real session instead of
    // the zombie. Never return a `Refusing`/`Wedged` candidate here.
    probed
        .iter()
        .find(|(_, r)| {
            !matches!(r, SocketReachability::Refusing | SocketReachability::Wedged)
        })
        .map(|(s, _)| (*s).clone())
}

/// Among `sessions`, the "alive but not listening" zombies sharing `name` that a
/// supervisor should reap, each paired with *why* (so the caller can log a
/// `reap-zombie` vs `reap-wedged` event): every live candidate that is **not
/// listening** ([`DiscoveredSession::not_listening`] — its socket refuses
/// connects, or accepts them but never answers the hello), **except** the most
/// recently launched live one — kept as the intended session (a replacement that
/// exists or is still coming up). Returns an empty list unless the name has more
/// than one live candidate, so a lone session is never reaped out from under the
/// slot it still nominally owns (`rt-re-entering-a-review-fails-to-attach`).
///
/// Each returned pair carries the [`SocketReachability`] the reap decision was
/// made on (always [`Refusing`](SocketReachability::Refusing) or
/// [`Wedged`](SocketReachability::Wedged)), probed once here so the caller need
/// not probe again. Pure selection: the caller does the termination and directory
/// removal.
pub fn zombies_to_reap<'a>(
    sessions: &'a [DiscoveredSession],
    name: &str,
) -> Vec<(&'a DiscoveredSession, SocketReachability)> {
    let mut live: Vec<&DiscoveredSession> = sessions
        .iter()
        .filter(|s| s.alive && s.meta.name == name)
        .collect();
    if live.len() < 2 {
        return Vec::new();
    }
    // Newest first; keep `live[0]` (the replacement), reap older not-listening
    // ones (refusing or wedged).
    live.sort_by(|a, b| b.meta.launched_at.cmp(&a.meta.launched_at));
    live[1..]
        .iter()
        .copied()
        .filter_map(|s| {
            // One probe per candidate: carry the reachability so the reason is
            // not re-derived by a second (racy) probe at the call site.
            let reach = s.reachability();
            matches!(
                reach,
                SocketReachability::Refusing | SocketReachability::Wedged
            )
            .then_some((s, reach))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_proto::{Frame, Hello};
    use std::io::{Read, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    /// A stand-in session socket that *answers the hello* — binds `sock` and,
    /// from a detached accept loop, serves every connection the way a real
    /// session does: it reads the client's hello *first*, replies with its own
    /// [`Hello`] frame, then keeps the connection open (draining to EOF) until
    /// the client closes it. This is what the hello-round-trip probe
    /// ([`probe_reachable`]) needs to classify a listener as truly
    /// [`Reachable`](SocketReachability::Reachable); a bare `UnixListener` that
    /// never writes would read as [`Wedged`](SocketReachability::Wedged).
    ///
    /// Reading the client's hello before replying — and not dropping the stream
    /// while unread client bytes are still buffered — matters on Linux: closing
    /// a Unix stream socket that still holds unread data resets the connection
    /// (sends an RST), which can discard the hello reply in flight or fail the
    /// probe's own write, so the probe reads the session as
    /// [`Wedged`](SocketReachability::Wedged) and the test flakes. macOS is more
    /// forgiving, which is why the reply-then-drop version passed locally but
    /// failed on CI. Each connection is served on its own thread so one slow
    /// client never blocks the accept loop; the accept loop owns the listener,
    /// so the socket stays bound for the test, and every thread is intentionally
    /// detached (the test process exits at the end).
    fn hello_listener(sock: &Path) -> UnixListener {
        let listener = UnixListener::bind(sock).unwrap();
        let accept = listener.try_clone().unwrap();
        std::thread::spawn(move || {
            for stream in accept.incoming() {
                match stream {
                    Ok(stream) => {
                        std::thread::spawn(move || serve_hello(stream));
                    }
                    Err(_) => break,
                }
            }
        });
        listener
    }

    /// Serve one probe connection like a real session handler: read the client's
    /// hello, reply with ours, then drain the connection until the client closes
    /// it. Any error (the client hung up, a malformed frame) just ends the
    /// handler — this is a test stand-in, not a hardened server.
    fn serve_hello(mut stream: UnixStream) {
        // Read the client's hello before replying, so we never close with its
        // bytes still unread (which would reset the connection on Linux).
        let mut buf: Vec<u8> = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match Frame::decode(&buf) {
                Ok((Frame::Hello(_), _)) => break,
                // Anything other than a hello first is a protocol error; drop.
                Ok(_) => return,
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => {}
                Err(_) => return,
            }
            match stream.read(&mut chunk) {
                Ok(0) => return, // client closed before sending its hello
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(_) => return,
            }
        }

        let reply = Frame::Hello(Hello {
            protocol_version: shelbi_proto::PROTOCOL_VERSION,
            colors: None,
            capabilities: Vec::new(),
        })
        .encode()
        .unwrap();
        if stream.write_all(&reply).is_err() || stream.flush().is_err() {
            return;
        }

        // Stay open until the client closes the connection, draining whatever it
        // sends. This mirrors a real session's handler lifetime and ensures the
        // close is driven by the client (the probe), never by us dropping unread
        // data out from under it.
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => return,
                Ok(_) => {}
                Err(_) => return,
            }
        }
    }

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
    fn a_live_session_that_answers_the_hello_is_usable() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let _listener = hello_listener(&sock);
        let s = discovered(dir.path(), sock, true);
        assert!(s.usable(), "a live, answering session is usable");
        assert!(!s.not_listening());
        assert_eq!(s.reachability(), SocketReachability::Reachable);
    }

    #[test]
    fn a_live_session_that_accepts_but_never_answers_is_wedged_and_not_usable() {
        // Lock held (alive=true) and the socket *accepts* connections, but the
        // session never sends its hello — the wedge this task fixes. A bare
        // connect would call it reachable; the hello round-trip must classify it
        // Wedged so supervision relaunches it instead of reusing the stranded
        // slot (`rt-review-session-wedges-after-repeated-attaches`).
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        // A bare listener accepts connections but never writes a hello.
        let _listener = UnixListener::bind(&sock).unwrap();
        let s = discovered(dir.path(), sock, true);
        assert_eq!(s.reachability(), SocketReachability::Wedged);
        assert!(s.not_listening(), "an accept-but-silent session is not listening");
        assert!(!s.usable(), "a wedged session is not usable");
    }

    #[test]
    fn a_live_session_whose_socket_refuses_is_a_zombie_and_not_usable() {
        // Lock held (alive=true) but the listener is gone: the "alive but not
        // listening" zombie whose connects are refused. It must read as not usable
        // so supervision relaunches it instead of reusing the stranded slot.
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
        assert_eq!(s.reachability(), SocketReachability::Refusing);
        assert!(s.not_listening(), "a refusing live socket is a zombie");
        assert!(!s.usable(), "a zombie is not usable");
    }

    #[test]
    fn a_dead_session_is_never_reported_as_a_zombie() {
        // alive=false is already dead by the ordinary lock probe; not_listening
        // only ever speaks to live-by-lock sessions, so it stays false.
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener);
        let s = discovered(dir.path(), sock, false);
        assert!(!s.not_listening());
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
        let _listener = hello_listener(&live_sock);

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
        let _la = hello_listener(&a_sock);
        let b_dir = tempfile::tempdir().unwrap();
        let b_sock = b_dir.path().join("sock");
        let _lb = hello_listener(&b_sock);

        let older = named_at(a_dir.path(), a_sock, true, "demo/ws/x", "2026-01-01T00:00:00Z");
        let newer = named_at(b_dir.path(), b_sock, true, "demo/ws/x", "2026-02-01T00:00:00Z");
        let chosen = choose_session(&[older, newer], "demo/ws/x").expect("a usable session");
        assert_eq!(chosen.meta.launched_at, "2026-02-01T00:00:00Z");
    }

    #[test]
    fn choose_session_skips_an_exiting_session_for_its_live_replacement() {
        // The reload race (`client-connect-skips-exited-sessions`): the old
        // session has pushed `exited` (its `exiting` marker is on disk) but still
        // holds its lock and still answers the hello through its flush grace,
        // while the replacement is live but has not bound its socket yet. Before
        // the fix the probe saw the dying session Reachable and the replacement
        // NotBound, so the client attached to the dying one, was dropped, and
        // counted a second death. The replacement must win, in any list order
        // and whichever one launched later.
        let dying_dir = tempfile::tempdir().unwrap();
        let dying_sock = dying_dir.path().join("sock");
        let _dying_listener = hello_listener(&dying_sock);
        std::fs::write(dying_dir.path().join("exiting"), b"").unwrap();

        let fresh_dir = tempfile::tempdir().unwrap();
        let fresh_sock = fresh_dir.path().join("sock"); // not bound yet

        for (dying_at, fresh_at) in [("t1", "t2"), ("t2", "t1")] {
            let dying = named_at(dying_dir.path(), dying_sock.clone(), true, "demo/orch", dying_at);
            let fresh = named_at(fresh_dir.path(), fresh_sock.clone(), true, "demo/orch", fresh_at);
            assert!(dying.exiting(), "the marker reads as exiting");
            assert!(!fresh.exiting());
            for order in [
                vec![dying.clone(), fresh.clone()],
                vec![fresh.clone(), dying.clone()],
            ] {
                let chosen = choose_session(&order, "demo/orch").expect("the live replacement");
                assert_eq!(
                    chosen.dir,
                    fresh_dir.path(),
                    "an exiting session must never be chosen over a live one",
                );
            }
        }
    }

    #[test]
    fn choose_session_never_returns_a_lone_exiting_session() {
        // With only the dying session on disk (the replacement has not written
        // its meta yet), there is nothing to attach to: `None`, which the attach
        // paths retry until the replacement appears. Either exit signal counts:
        // the early `exiting` marker or the later `exit.json`.
        for marker in ["exiting", "exit.json"] {
            let dir = tempfile::tempdir().unwrap();
            let sock = dir.path().join("sock");
            let _listener = hello_listener(&sock);
            std::fs::write(dir.path().join(marker), b"").unwrap();
            let only = named_at(dir.path(), sock, true, "demo/orch", "t1");
            assert!(only.exiting(), "{marker} reads as exiting");
            assert!(
                choose_session(&[only], "demo/orch").is_none(),
                "a lone session with {marker} must not be chosen",
            );
        }
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
        assert_eq!(reap[0].0.meta.launched_at, "t1", "and it is the stale one");
        assert_eq!(
            reap[0].1,
            SocketReachability::Refusing,
            "a refusing zombie is reaped as a refusing one"
        );
    }

    #[test]
    fn zombies_to_reap_drops_a_stale_wedged_one_keeping_the_replacement() {
        // The wedge case: a live (answering) replacement and an older *wedged*
        // sibling — one that accepts connections but never answers — share a
        // name. The wedged one is reaped and reported as Wedged (so the caller
        // logs `reap-wedged`), the replacement kept
        // (`rt-review-session-wedges-after-repeated-attaches`).
        let live_dir = tempfile::tempdir().unwrap();
        let live_sock = live_dir.path().join("sock");
        let _listener = hello_listener(&live_sock);

        // A bare listener accepts but never answers the hello: wedged.
        let wedged_dir = tempfile::tempdir().unwrap();
        let wedged_sock = wedged_dir.path().join("sock");
        let _wedged = UnixListener::bind(&wedged_sock).unwrap();

        // The live one is the newer (the replacement), the wedged one older.
        let live = named_at(live_dir.path(), live_sock, true, "demo/ws/x", "t2");
        let wedged = named_at(wedged_dir.path(), wedged_sock, true, "demo/ws/x", "t1");

        let candidates = [live, wedged];
        let reap = zombies_to_reap(&candidates, "demo/ws/x");
        assert_eq!(reap.len(), 1, "exactly the one wedged session is reaped");
        assert_eq!(reap[0].0.meta.launched_at, "t1", "and it is the stale one");
        assert_eq!(
            reap[0].1,
            SocketReachability::Wedged,
            "an accept-but-silent session is reaped as wedged"
        );
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
        assert_eq!(reap[0].0.meta.launched_at, "t1", "the newest is kept");
    }

    /// Write a dead session directory (an unheld `lock`, so `is_held` is false)
    /// under `root` keyed by `short_id`, with the given logical `name` and
    /// `launched_at` in its `meta.json`.
    fn write_dead_dir(root: &Path, short_id: &str, name: &str, launched_at: &str) {
        let dir = root.join(short_id);
        std::fs::create_dir_all(&dir).unwrap();
        let meta = Meta {
            id: short_id.into(),
            name: name.into(),
            argv: vec!["cat".into()],
            cwd: dir.clone(),
            task: None,
            launched_at: launched_at.into(),
            protocol_version: shelbi_proto::PROTOCOL_VERSION,
            pid: 0,
        };
        std::fs::write(dir.join("meta.json"), meta.to_json().unwrap()).unwrap();
        // A lock file nobody holds: `is_held` reads it as dead.
        std::fs::write(dir.join("lock"), b"").unwrap();
    }

    #[test]
    fn reap_dead_keeping_recent_prunes_older_duplicates_per_name() {
        // Three dead directories share one name and a fourth carries another.
        // Keeping the single most recent per name must prune the two older
        // same-named ones and leave the other name's lone dead dir untouched.
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write_dead_dir(root, "a1", "demo/ws/alpha", "2026-01-01T00:00:00Z");
        write_dead_dir(root, "a2", "demo/ws/alpha", "2026-02-01T00:00:00Z");
        write_dead_dir(root, "a3", "demo/ws/alpha", "2026-03-01T00:00:00Z"); // newest
        write_dead_dir(root, "b1", "demo/ws/beta", "2026-01-15T00:00:00Z");

        let mut reaped = reap_dead_keeping_recent(root, 1).unwrap();
        reaped.sort();
        assert_eq!(reaped, vec!["a1".to_string(), "a2".to_string()]);
        assert!(!root.join("a1").exists());
        assert!(!root.join("a2").exists());
        assert!(root.join("a3").exists(), "the newest same-named dead dir is kept");
        assert!(root.join("b1").exists(), "the other name's lone dead dir is kept");
    }

    #[test]
    fn reap_dead_keeping_recent_never_removes_a_live_session() {
        // A held lock (live) is never pruned, and never counts against the
        // kept-dead budget — so an older *dead* sibling of a *live* session is
        // still reaped when keep_per_name is 0.
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let live_dir = root.join("live");
        std::fs::create_dir_all(&live_dir).unwrap();
        let meta = Meta {
            id: "live".into(),
            name: "demo/ws/alpha".into(),
            argv: vec!["cat".into()],
            cwd: live_dir.clone(),
            task: None,
            launched_at: "2026-04-01T00:00:00Z".into(),
            protocol_version: shelbi_proto::PROTOCOL_VERSION,
            pid: 0,
        };
        std::fs::write(live_dir.join("meta.json"), meta.to_json().unwrap()).unwrap();
        let _live_lock = shelbi_session::lock::SessionLock::acquire(&live_dir.join("lock")).unwrap();
        write_dead_dir(root, "old", "demo/ws/alpha", "2026-01-01T00:00:00Z");

        let reaped = reap_dead_keeping_recent(root, 0).unwrap();
        assert_eq!(reaped, vec!["old".to_string()]);
        assert!(live_dir.exists(), "the live session survives");
        assert!(!root.join("old").exists(), "the dead sibling is reaped");
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

    #[test]
    fn a_session_that_wrote_its_exit_record_is_dead_even_with_its_lock_held() {
        // The exit record lands before the session process drops its lock, and
        // a lock read can be fooled; `exit.json` is proof of death either way,
        // so discovery never offers an exited session as live (and so never as
        // "usable" — a dead dir has no `sock`, which reads as still-binding).
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        write_dead_dir(root, "gone", "demo/ws/golf", "2026-01-01T00:00:00Z");
        let dir = root.join("gone");
        let _held = shelbi_session::lock::SessionLock::acquire(&dir.join("lock")).unwrap();
        assert!(list(root).unwrap()[0].alive, "lock held, no exit record: live");

        std::fs::write(dir.join("exit.json"), br#"{"code":143}"#).unwrap();
        let found = list(root).unwrap();
        assert!(!found[0].alive, "an exit record means dead");
        assert!(!found[0].usable());
        assert!(choose_session(&found, "demo/ws/golf").is_none());
    }
}
