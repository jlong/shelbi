//! The remote transport: one relay per machine, one stdio channel for all its
//! sessions.
//!
//! A remote machine runs a single `shelbi relay` process the hub starts with
//! `ssh <host> shelbi relay`. It bridges one stdio channel to every session
//! socket on that host — one channel and not one per session, because sshd
//! allows ten sessions per multiplexed connection by default. This module has
//! both ends:
//!
//! - [`RelayChannel`] (hub side): drives the [`shelbi_proto::relay`] envelope
//!   over the channel, demultiplexes it into per-session logical streams, and
//!   hands each out as a [`RelayStream`] the [`Connection`](crate::Connection)
//!   drives with the exact same API as a local socket. It carries the
//!   channel-level keepalive and turns silence into **unreachable**.
//! - [`serve_relay`] (remote side): the body of `shelbi relay`. It answers
//!   discovery from `~/.shelbi/sessions/`, connects a session socket per
//!   [`Open`](shelbi_proto::RelayFrame::Open), and forwards whole session
//!   frames in both directions **unchanged**, so it never has to understand the
//!   session protocol and speaks the frozen core to sessions weeks older than
//!   itself.
//!
//! **The relay holds nothing** — no PTYs, no session state. If it or the SSH
//! connection dies, the hub starts another; because the session never
//! restarted and keeps a continuous output sequence counter, a client reattach
//! (`Attach { since_seq }`) resumes with no lost or duplicated output.
//!
//! **Backpressure.** The shared channel must never let one session's slow
//! consumer stall another's. Each hub-side stream has a bounded inbound queue
//! of whole frames; when it overflows the demux **drops the queue and asks the
//! session to replay** (a fresh `attach`, answered with a `resync` snapshot) —
//! the same drop-to-replay rule the session applies per client — and never
//! blocks the channel reader, so other streams keep flowing.
//!
//! Runtime-agnostic like the rest of the crate: plain threads and blocking
//! [`Read`]/[`Write`], no tokio.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use shelbi_proto::{frame_boundary, Attach, ExtType, Frame, ProtoError, RelayFrame, RelaySession};

use crate::error::ClientError;
use crate::transport::{Transport, TransportHalves};

/// How many whole session frames a hub-side stream may buffer before the demux
/// drops its queue and asks the session to replay. Mirrors the session's own
/// `CLIENT_QUEUE_LIMIT`.
const STREAM_QUEUE_LIMIT: usize = 2048;

/// Default keepalive cadence: how often the hub probes the channel with a
/// `Ping`.
const DEFAULT_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// Default silence deadline: no inbound byte for this long marks the channel
/// unreachable.
const DEFAULT_UNREACHABLE_DEADLINE: Duration = Duration::from_secs(30);

/// Keepalive timing, overridable for tests.
#[derive(Debug, Clone, Copy)]
pub struct Keepalive {
    /// How often to send a channel `Ping`.
    pub interval: Duration,
    /// How long the channel may be silent before it is declared unreachable.
    pub deadline: Duration,
}

impl Default for Keepalive {
    fn default() -> Self {
        Self {
            interval: DEFAULT_KEEPALIVE_INTERVAL,
            deadline: DEFAULT_UNREACHABLE_DEADLINE,
        }
    }
}

// ---------------------------------------------------------------------------
// Hub side: RelayChannel + RelayStream
// ---------------------------------------------------------------------------

/// Per-stream inbound buffer of whole session frames.
#[derive(Default)]
struct Inbound {
    /// Whole session frames waiting for the stream's reader, oldest first.
    queue: VecDeque<Vec<u8>>,
    /// Set once the stream is torn down (relay said `Close`, or the channel
    /// died): the reader returns EOF.
    closed: bool,
    /// A drop-to-replay is in flight: the queue overflowed, was cleared, and a
    /// fresh `attach` was sent. Kept set until the reader drains, so we send one
    /// re-attach per overflow episode, not one per dropped frame.
    replay_pending: bool,
}

/// State for one logical stream: its inbound buffer and a condvar the reader
/// parks on.
struct StreamState {
    stream: u32,
    inbound: Mutex<Inbound>,
    cv: Condvar,
}

impl StreamState {
    fn new(stream: u32) -> Self {
        Self {
            stream,
            inbound: Mutex::new(Inbound::default()),
            cv: Condvar::new(),
        }
    }

    fn close(&self) {
        let mut st = self.inbound.lock().unwrap();
        st.closed = true;
        self.cv.notify_all();
    }
}

/// Shared channel state, held by [`RelayChannel`], every [`RelayStream`], and
/// the demux and keepalive threads.
struct Inner {
    /// The channel's write half. Whole relay frames are written under this lock,
    /// so a per-stream `Data` write never interleaves with another's.
    write: Mutex<Box<dyn Write + Send>>,
    /// Open logical streams by id.
    streams: Mutex<HashMap<u32, Arc<StreamState>>>,
    /// Next stream id to hand out.
    next_stream: AtomicU32,
    /// Waiters for a `SessionList` reply, in FIFO order.
    pending_list: Mutex<VecDeque<SyncSender<Vec<RelaySession>>>>,
    /// Waiters for an `Opened`/`OpenError` reply, by stream id.
    pending_open: Mutex<HashMap<u32, SyncSender<Result<(), String>>>>,
    /// Last time any byte arrived on the channel, for the keepalive deadline.
    last_activity: Mutex<Instant>,
    /// False once the channel is declared unreachable.
    reachable: AtomicBool,
    /// Set when the channel is being torn down (unreachable, or dropped).
    closed: AtomicBool,
}

impl Inner {
    /// Write one whole relay frame to the channel under the write lock.
    fn send(&self, frame: &RelayFrame) -> Result<(), ClientError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(ClientError::RelayUnreachable);
        }
        let bytes = frame.encode()?;
        let mut w = self.write.lock().map_err(|_| ClientError::RelayUnreachable)?;
        w.write_all(&bytes)?;
        w.flush()?;
        Ok(())
    }

    /// Tear the channel down: declare it unreachable (or just closed), wake
    /// every stream reader with EOF, and fail every outstanding request.
    fn teardown(&self, reachable: bool) {
        self.closed.store(true, Ordering::Relaxed);
        self.reachable.store(reachable, Ordering::Relaxed);
        for st in self.streams.lock().unwrap().values() {
            st.close();
        }
        // Dropping the senders makes any waiting `recv` return `Err`.
        self.pending_list.lock().unwrap().clear();
        self.pending_open.lock().unwrap().clear();
    }
}

/// The hub side of one machine's relay: a single stdio channel multiplexed into
/// per-session [`RelayStream`]s.
///
/// Construct it over whatever carries the channel — in production the piped
/// stdin/stdout of an `ssh <host> shelbi relay` child; in tests a local pipe.
/// [`list_sessions`](RelayChannel::list_sessions) enumerates the machine's
/// sessions and [`open`](RelayChannel::open) bridges one, returning a
/// [`RelayStream`] to hand to [`Connection::connect`](crate::Connection::connect).
pub struct RelayChannel {
    inner: Arc<Inner>,
    _demux: std::thread::JoinHandle<()>,
    _keepalive: std::thread::JoinHandle<()>,
}

impl RelayChannel {
    /// Open a relay channel over `read`/`write` with the default keepalive.
    pub fn new(
        read: Box<dyn Read + Send>,
        write: Box<dyn Write + Send>,
    ) -> Result<Self, ClientError> {
        Self::with_keepalive(read, write, Keepalive::default())
    }

    /// Open a relay channel with explicit keepalive timing (tests use a short
    /// interval and deadline).
    pub fn with_keepalive(
        read: Box<dyn Read + Send>,
        write: Box<dyn Write + Send>,
        keepalive: Keepalive,
    ) -> Result<Self, ClientError> {
        let inner = Arc::new(Inner {
            write: Mutex::new(write),
            streams: Mutex::new(HashMap::new()),
            next_stream: AtomicU32::new(1),
            pending_list: Mutex::new(VecDeque::new()),
            pending_open: Mutex::new(HashMap::new()),
            last_activity: Mutex::new(Instant::now()),
            reachable: AtomicBool::new(true),
            closed: AtomicBool::new(false),
        });

        // Announce ourselves (diagnostic; the relay ignores an unknown version).
        inner.send(&RelayFrame::Hello {
            version: shelbi_proto::RELAY_PROTOCOL_VERSION,
        })?;

        let demux = {
            let inner = inner.clone();
            std::thread::spawn(move || demux_loop(inner, read))
        };
        let keepalive = {
            let inner = inner.clone();
            std::thread::spawn(move || keepalive_loop(inner, keepalive))
        };

        Ok(Self {
            inner,
            _demux: demux,
            _keepalive: keepalive,
        })
    }

    /// Whether the channel is still reachable (the keepalive deadline has not
    /// been exceeded and the far end has not closed).
    pub fn is_reachable(&self) -> bool {
        self.inner.reachable.load(Ordering::Relaxed) && !self.inner.closed.load(Ordering::Relaxed)
    }

    /// Enumerate the sessions on the relay's machine.
    pub fn list_sessions(&self) -> Result<Vec<RelaySession>, ClientError> {
        let (tx, rx) = sync_channel(1);
        self.inner.pending_list.lock().unwrap().push_back(tx);
        self.inner.send(&RelayFrame::ListSessions)?;
        rx.recv().map_err(|_| ClientError::RelayUnreachable)
    }

    /// Bridge a logical stream to the session `short_id` and return it as a
    /// [`Transport`]. Pass the result to
    /// [`Connection::connect`](crate::Connection::connect).
    pub fn open(&self, short_id: &str) -> Result<RelayStream, ClientError> {
        let stream = self.inner.next_stream.fetch_add(1, Ordering::Relaxed);
        let state = Arc::new(StreamState::new(stream));
        self.inner.streams.lock().unwrap().insert(stream, state.clone());

        let (tx, rx) = sync_channel(1);
        self.inner.pending_open.lock().unwrap().insert(stream, tx);

        if let Err(e) = self.inner.send(&RelayFrame::Open {
            stream,
            short_id: short_id.to_string(),
        }) {
            self.inner.streams.lock().unwrap().remove(&stream);
            self.inner.pending_open.lock().unwrap().remove(&stream);
            return Err(e);
        }

        match rx.recv() {
            Ok(Ok(())) => Ok(RelayStream {
                inner: self.inner.clone(),
                state,
            }),
            Ok(Err(reason)) => {
                self.inner.streams.lock().unwrap().remove(&stream);
                Err(ClientError::Relay(reason))
            }
            Err(_) => {
                self.inner.streams.lock().unwrap().remove(&stream);
                Err(ClientError::RelayUnreachable)
            }
        }
    }
}

impl Drop for RelayChannel {
    fn drop(&mut self) {
        // Stop the keepalive thread and wake any stream readers; the demux
        // thread exits on the next channel EOF/error.
        self.inner.teardown(self.inner.reachable.load(Ordering::Relaxed));
    }
}

/// One logical stream over a [`RelayChannel`], usable as a [`Transport`] so a
/// [`Connection`](crate::Connection) drives it exactly like a local socket.
pub struct RelayStream {
    inner: Arc<Inner>,
    state: Arc<StreamState>,
}

impl Transport for RelayStream {
    fn split(self: Box<Self>) -> Result<TransportHalves, ClientError> {
        let reader = RelayStreamReader {
            state: self.state.clone(),
            leftover: Vec::new(),
            pos: 0,
        };
        let writer = RelayStreamWriter {
            inner: self.inner.clone(),
            stream: self.state.stream,
        };
        // `Connection`'s `Drop` fires this to wind a finished stream down from
        // outside the reader thread. Closing the stream state wakes the reader's
        // parked `read` with EOF so it exits (it cannot wait for the writer's own
        // `Drop`, which the reader itself keeps from running by holding a clone of
        // the shared write half); the relay-side socket is then dropped when the
        // last writer handle goes away. Mirrors `RelayStreamWriter::Drop`.
        let state = self.state.clone();
        let shutdown: crate::transport::ShutdownHandle = Box::new(move || {
            state.close();
        });
        Ok((Box::new(reader), Box::new(writer), shutdown))
    }
}

/// The read half of a [`RelayStream`]: pops whole session frames off the
/// stream's inbound queue and presents them as a byte stream to the session
/// reader.
struct RelayStreamReader {
    state: Arc<StreamState>,
    /// The frame currently being handed out, and how far through it we are.
    leftover: Vec<u8>,
    pos: usize,
}

impl Read for RelayStreamReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.leftover.len() {
            let mut st = self.state.inbound.lock().unwrap();
            let frame = loop {
                if let Some(f) = st.queue.pop_front() {
                    break f;
                }
                if st.closed {
                    return Ok(0); // EOF
                }
                st = self.state.cv.wait(st).unwrap();
            };
            drop(st);
            self.leftover = frame;
            self.pos = 0;
        }
        let n = (out.len()).min(self.leftover.len() - self.pos);
        out[..n].copy_from_slice(&self.leftover[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl crate::transport::ReadTimeout for RelayStreamReader {
    /// No-op: a relay stream has no socket-level read timeout. Its reads are
    /// already bounded by the relay keepalive (silence past the deadline surfaces
    /// as [`ClientError::RelayUnreachable`](crate::ClientError::RelayUnreachable)),
    /// so the hello handshake over a relay does not need this extra bound.
    fn set_read_timeout(&self, _dur: Option<std::time::Duration>) -> std::io::Result<()> {
        Ok(())
    }
}

/// The write half of a [`RelayStream`]: wraps each whole session frame the
/// [`Connection`](crate::Connection) writes in one `Data` envelope.
struct RelayStreamWriter {
    inner: Arc<Inner>,
    stream: u32,
}

impl Write for RelayStreamWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // The connection always hands us one whole encoded session frame per
        // `write_all`, so one `Data` carries one frame.
        self.inner
            .send(&RelayFrame::Data {
                stream: self.stream,
                bytes: buf.to_vec(),
            })
            .map_err(io::Error::other)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        // `Inner::send` already flushed the channel.
        Ok(())
    }
}

impl Drop for RelayStreamWriter {
    fn drop(&mut self) {
        // Forget the stream (a late `Data` for it is then ignored) and wake its
        // reader with EOF directly — we must not depend on the relay echoing our
        // `Close` back, because that echo finds the stream already removed and
        // would never close the reader. Then tell the relay to drop the socket.
        if let Some(state) = self.inner.streams.lock().unwrap().remove(&self.stream) {
            state.close();
        }
        let _ = self.inner.send(&RelayFrame::Close {
            stream: self.stream,
        });
    }
}

/// Deliver one whole session frame to a stream's reader, honoring backpressure.
/// Never blocks the demux: on overflow it drops the queue and asks the session
/// to replay (a fresh `attach`, answered with a `resync`). While that replay is
/// pending it drops live output until the `resync` lands — the same rule the
/// session applies to a lagging client — so the reader next sees a coherent
/// snapshot, never a torn mid-drop stream.
fn deliver(inner: &Arc<Inner>, state: &Arc<StreamState>, bytes: Vec<u8>) {
    let trigger_replay = {
        let mut st = state.inbound.lock().unwrap();
        if st.closed {
            return;
        }
        // The frame is a whole session frame: byte 4 (after the 4-byte length
        // prefix) is its type. A `resync` is the snapshot our re-attach asked
        // for — it ends the drop.
        let is_resync = bytes.get(4) == Some(&(ExtType::Resync as u8));
        if st.replay_pending {
            if is_resync {
                st.queue.clear();
                st.queue.push_back(bytes);
                st.replay_pending = false;
                state.cv.notify_one();
            }
            // else: drop live output while waiting for the resync snapshot.
            return;
        }
        if st.queue.len() >= STREAM_QUEUE_LIMIT {
            st.queue.clear();
            st.replay_pending = true;
            true
        } else {
            st.queue.push_back(bytes);
            state.cv.notify_one();
            false
        }
    };
    if trigger_replay {
        // Re-attach from the start: the session answers with a fresh `resync`
        // snapshot and resumes live output, exactly as its own drop-to-resync
        // does for a lagging client. Done outside the inbound lock.
        if let Ok(attach) = Frame::Attach(Attach { since_seq: None }).encode() {
            let _ = inner.send(&RelayFrame::Data {
                stream: state.stream,
                bytes: attach,
            });
        }
    }
}

/// The demux thread: read relay frames off the channel and route them.
fn demux_loop(inner: Arc<Inner>, mut read: Box<dyn Read + Send>) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        loop {
            match RelayFrame::decode(&buf) {
                Ok((frame, consumed)) => {
                    buf.drain(..consumed);
                    route_relay_frame(&inner, frame);
                }
                Err(ProtoError::Incomplete { .. }) => break,
                Err(_) => {
                    inner.teardown(false);
                    return;
                }
            }
        }
        match read.read(&mut chunk) {
            Ok(0) | Err(_) => {
                // The far end closed or errored: unreachable, not dead.
                inner.teardown(false);
                return;
            }
            Ok(n) => {
                *inner.last_activity.lock().unwrap() = Instant::now();
                buf.extend_from_slice(&chunk[..n]);
            }
        }
    }
}

fn route_relay_frame(inner: &Arc<Inner>, frame: RelayFrame) {
    match frame {
        RelayFrame::Hello { .. } => {} // diagnostic only
        RelayFrame::SessionList { sessions } => {
            if let Some(tx) = inner.pending_list.lock().unwrap().pop_front() {
                let _ = tx.send(sessions);
            }
        }
        RelayFrame::Opened { stream } => {
            if let Some(tx) = inner.pending_open.lock().unwrap().remove(&stream) {
                let _ = tx.send(Ok(()));
            }
        }
        RelayFrame::OpenError { stream, error } => {
            if let Some(tx) = inner.pending_open.lock().unwrap().remove(&stream) {
                let _ = tx.send(Err(error));
            }
            inner.streams.lock().unwrap().remove(&stream);
        }
        RelayFrame::Data { stream, bytes } => {
            let state = inner.streams.lock().unwrap().get(&stream).cloned();
            if let Some(state) = state {
                deliver(inner, &state, bytes);
            }
        }
        RelayFrame::Close { stream } => {
            if let Some(state) = inner.streams.lock().unwrap().remove(&stream) {
                state.close();
            }
        }
        RelayFrame::Ping => {
            let _ = inner.send(&RelayFrame::Pong);
        }
        // Pong (activity already recorded) and requests the hub never receives.
        RelayFrame::Pong | RelayFrame::ListSessions | RelayFrame::Open { .. } => {}
    }
}

/// The keepalive thread: probe the channel and declare it unreachable after the
/// silence deadline.
fn keepalive_loop(inner: Arc<Inner>, cfg: Keepalive) {
    loop {
        std::thread::sleep(cfg.interval);
        if inner.closed.load(Ordering::Relaxed) {
            return;
        }
        // A failed probe means the write side is gone: unreachable.
        if inner.send(&RelayFrame::Ping).is_err() {
            inner.teardown(false);
            return;
        }
        let silent_for = inner.last_activity.lock().unwrap().elapsed();
        if silent_for > cfg.deadline {
            inner.teardown(false);
            return;
        }
    }
}

// ---------------------------------------------------------------------------
// Remote side: the `shelbi relay` server
// ---------------------------------------------------------------------------

/// A shared, lockable channel writer for the relay server: forwarder threads
/// and the main loop all write whole relay frames through it.
type SharedOut = Arc<Mutex<Box<dyn Write + Send>>>;

fn send_out(out: &SharedOut, frame: &RelayFrame) -> io::Result<()> {
    let bytes = frame.encode().map_err(io::Error::other)?;
    let mut w = out.lock().map_err(|_| io::Error::other("relay out poisoned"))?;
    w.write_all(&bytes)?;
    w.flush()
}

/// One bridged session on the relay: the socket write half for hub→session
/// bytes. The session→hub forwarder thread owns the read half.
struct SessionLink {
    sock_write: std::os::unix::net::UnixStream,
}

/// Run the `shelbi relay` server: bridge the stdio channel (`read`/`write`) to
/// every session socket under `sessions_root` (`~/.shelbi/sessions/`).
///
/// It answers discovery, connects a session socket per `Open`, and forwards
/// whole frames both ways unchanged. Returns when the channel closes (the SSH
/// connection dropped, EOF on `read`); the hub then starts a fresh relay. The
/// relay holds no session state, so there is nothing to clean up but the
/// in-flight socket connections, which close with it.
pub fn serve_relay(
    read: Box<dyn Read + Send>,
    write: Box<dyn Write + Send>,
    sessions_root: &Path,
) -> Result<(), ClientError> {
    let out: SharedOut = Arc::new(Mutex::new(write));
    let links: Arc<Mutex<HashMap<u32, SessionLink>>> = Arc::new(Mutex::new(HashMap::new()));
    let sessions_root = sessions_root.to_path_buf();

    // Announce ourselves (diagnostic).
    send_out(
        &out,
        &RelayFrame::Hello {
            version: shelbi_proto::RELAY_PROTOCOL_VERSION,
        },
    )?;

    let mut read = read;
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        loop {
            match RelayFrame::decode(&buf) {
                Ok((frame, consumed)) => {
                    buf.drain(..consumed);
                    handle_server_frame(frame, &out, &links, &sessions_root);
                }
                Err(ProtoError::Incomplete { .. }) => break,
                Err(_) => {
                    shutdown_links(&links);
                    return Ok(());
                }
            }
        }
        match read.read(&mut chunk) {
            Ok(0) | Err(_) => {
                shutdown_links(&links);
                return Ok(());
            }
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn handle_server_frame(
    frame: RelayFrame,
    out: &SharedOut,
    links: &Arc<Mutex<HashMap<u32, SessionLink>>>,
    sessions_root: &Path,
) {
    match frame {
        RelayFrame::Hello { .. } => {}
        RelayFrame::ListSessions => {
            let sessions = discover_sessions(sessions_root);
            let _ = send_out(out, &RelayFrame::SessionList { sessions });
        }
        RelayFrame::Open { stream, short_id } => {
            open_session_stream(stream, &short_id, out, links, sessions_root);
        }
        RelayFrame::Data { stream, bytes } => {
            // hub → session: write the bytes straight to the socket. The session
            // reassembles whole frames itself, so no splitting is needed here.
            let mut guard = links.lock().unwrap();
            let drop_it = match guard.get_mut(&stream) {
                Some(link) => link.sock_write.write_all(&bytes).is_err(),
                None => false,
            };
            if drop_it {
                guard.remove(&stream);
                drop(guard);
                let _ = send_out(out, &RelayFrame::Close { stream });
            }
        }
        RelayFrame::Close { stream } => {
            if let Some(link) = links.lock().unwrap().remove(&stream) {
                let _ = link
                    .sock_write
                    .shutdown(std::net::Shutdown::Both);
            }
        }
        RelayFrame::Ping => {
            let _ = send_out(out, &RelayFrame::Pong);
        }
        RelayFrame::Pong | RelayFrame::SessionList { .. } | RelayFrame::Opened { .. }
        | RelayFrame::OpenError { .. } => {}
    }
}

/// Connect the session socket for `short_id` and start forwarding it to the
/// hub as `stream`.
fn open_session_stream(
    stream: u32,
    short_id: &str,
    out: &SharedOut,
    links: &Arc<Mutex<HashMap<u32, SessionLink>>>,
    sessions_root: &Path,
) {
    let sock = sessions_root.join(short_id).join("sock");
    let conn = match std::os::unix::net::UnixStream::connect(&sock) {
        Ok(c) => c,
        Err(e) => {
            let _ = send_out(
                out,
                &RelayFrame::OpenError {
                    stream,
                    error: format!("connect {}: {e}", sock.display()),
                },
            );
            return;
        }
    };
    let read_half = match conn.try_clone() {
        Ok(r) => r,
        Err(e) => {
            let _ = send_out(
                out,
                &RelayFrame::OpenError {
                    stream,
                    error: format!("clone socket: {e}"),
                },
            );
            return;
        }
    };
    links
        .lock()
        .unwrap()
        .insert(stream, SessionLink { sock_write: conn });
    let _ = send_out(out, &RelayFrame::Opened { stream });

    // Forward session → hub: peel whole frames (by the length prefix alone, so
    // an additive-capability frame we cannot decode still forwards whole) and
    // wrap each in a `Data`.
    let out = out.clone();
    let links = links.clone();
    std::thread::spawn(move || forward_session_to_hub(stream, read_half, out, links));
}

fn forward_session_to_hub(
    stream: u32,
    mut sock_read: std::os::unix::net::UnixStream,
    out: SharedOut,
    links: Arc<Mutex<HashMap<u32, SessionLink>>>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    'outer: loop {
        loop {
            match frame_boundary(&buf) {
                Ok(total) => {
                    let frame: Vec<u8> = buf.drain(..total).collect();
                    if send_out(&out, &RelayFrame::Data { stream, bytes: frame }).is_err() {
                        break 'outer;
                    }
                }
                Err(ProtoError::Incomplete { .. }) => break,
                Err(_) => break 'outer, // malformed from the session: stop
            }
        }
        match sock_read.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    // The session closed (or we stopped): tell the hub and forget the link.
    links.lock().unwrap().remove(&stream);
    let _ = send_out(&out, &RelayFrame::Close { stream });
}

fn shutdown_links(links: &Arc<Mutex<HashMap<u32, SessionLink>>>) {
    for (_, link) in links.lock().unwrap().drain() {
        let _ = link.sock_write.shutdown(std::net::Shutdown::Both);
    }
}

/// Enumerate the machine's sessions as [`RelaySession`]s, skipping any that
/// cannot be read. A missing root means no session has ever started.
fn discover_sessions(root: &Path) -> Vec<RelaySession> {
    match crate::discovery::list(root) {
        Ok(found) => found
            .into_iter()
            .map(|s| RelaySession {
                short_id: s.short_id,
                name: s.meta.name,
                task: s.meta.task,
                argv: s.meta.argv,
                cwd: s.meta.cwd.to_string_lossy().into_owned(),
                alive: s.alive,
                protocol_version: s.meta.protocol_version,
            })
            .collect(),
        Err(_) => Vec::new(),
    }
}
