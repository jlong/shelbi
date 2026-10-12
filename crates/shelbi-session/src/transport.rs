//! The session socket server: one Unix socket, many clients.
//!
//! The wire format is `shelbi-proto`'s length-prefixed frames — the frozen core
//! ([`Frame`]) plus the additive capabilities ([`ExtFrame`]), read off one
//! stream by [`decode_any`]. This module implements the **full** session
//! protocol server: the hello handshake (announcing the session's capabilities),
//! `attach`/`detach`, `input`, `paste`, `resize`, `snapshot`, `info`,
//! `set-meta`, `kill`, keepalive `ping`/`pong`, and the pushed events (title,
//! bell, resized, exited). Attach **replay** sends a [`Resync`] carrying a full
//! replay of the emulator's state (see [`crate::replay`]).
//!
//! Each client connection gets two threads: a reader ([`serve_client`]) that
//! decodes request frames, and a writer ([`client_writer`]) fed by a bounded
//! per-client [`Outbox`]. A client that falls behind never blocks the PTY
//! reader: its queued output is dropped and it is sent a fresh [`Resync`] replay
//! (if it negotiated the capability) or disconnected (if it did not).

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicU16, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use shelbi_proto::{
    decode_any, AnyFrame, ExtFrame, Frame, Hello, InfoData, Resync, SnapshotData,
};

use crate::session::Shared;

/// How many frames a client's outbox may hold before it is considered lagging.
/// Past this, its queue is dropped and it is recovered by a [`Resync`] (or
/// disconnected if it did not negotiate `resync`), so the PTY reader never
/// blocks and memory stays bounded.
const CLIENT_QUEUE_LIMIT: usize = 2048;

/// How long a single socket write to a client may stall before the client is
/// treated as dead and its connection torn down.
///
/// A client that keeps its socket open but stops reading (it navigated away
/// without closing, or wedged) eventually fills the kernel send buffer, at which
/// point a blocking `write` to it never returns. Without a bound the writer
/// thread parks in that `write` forever, [`serve_client`]'s `writer.join()` then
/// blocks forever too, and the pair of threads plus the connection's three file
/// descriptors leak — one pair per abandoned attach, until the process runs out
/// of descriptors and can serve no new connection
/// (`rt-review-session-wedges-after-repeated-attaches`). A write that makes *no*
/// progress for this long means the peer is gone: the writer errors out of
/// `write_all`, winds the connection down, and releases its threads and fds. A
/// merely *slow* client still drains a little each `write` and is never dropped
/// here — the bounded [`Outbox`] and its [`Resync`] recovery handle backlog.
const CLIENT_WRITE_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a freshly accepted connection has to complete the [`Hello`]
/// handshake before the server closes it and winds its threads down.
///
/// Accepting a connection costs a reader thread, a writer thread, and three file
/// descriptors. A peer that connects and then never sends its hello (it wedged
/// mid-handshake, or it opened the socket and walked away) holds all of that: the
/// reader parks in `read` with nothing to decode, and `serve_client` cannot
/// return while the reader is parked. With no bound one such peer leaks a handler
/// per connect until the session hits `EMFILE` and can serve no one
/// (`rt-find-the-5s-connection-to-the-review-session`). A connection that has not
/// produced a decodable hello within this window is treated as dead: the reader
/// returns, `serve_client` tears the connection down, and both threads exit. A
/// healthy client sends its hello as its first bytes (sub-millisecond for the
/// local/relay peer), so this never trips a real handshake; the bound is cleared
/// the moment the hello arrives, so steady-state reads block normally afterward.
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);

/// The effective hello-handshake deadline. The default is [`HELLO_TIMEOUT`];
/// `SHELBI_HELLO_TIMEOUT_MS` overrides it (read fresh per connection — connects
/// are infrequent, so the cost is irrelevant), used only by the leak tests to
/// drive the reap on a short clock rather than waiting out the 5 s default.
fn hello_timeout() -> Duration {
    std::env::var("SHELBI_HELLO_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(HELLO_TIMEOUT)
}

// Capability bits, derived from a client's hello capability list. Stored in an
// `AtomicU16` on the channel so the broadcast path reads them lock-free.
const CAP_OUTPUT_RESIZED: u16 = 1 << 0;
const CAP_EVENT_TITLE: u16 = 1 << 1;
const CAP_EVENT_BELL: u16 = 1 << 2;
const CAP_EVENT_RESIZED: u16 = 1 << 3;
const CAP_RESYNC: u16 = 1 << 4;
const CAP_KEEPALIVE: u16 = 1 << 5;

fn caps_from_list(list: &[String]) -> u16 {
    use shelbi_proto::capability as cap;
    let mut bits = 0;
    for name in list {
        bits |= match name.as_str() {
            cap::OUTPUT_RESIZED => CAP_OUTPUT_RESIZED,
            cap::EVENT_TITLE => CAP_EVENT_TITLE,
            cap::EVENT_BELL => CAP_EVENT_BELL,
            cap::EVENT_RESIZED => CAP_EVENT_RESIZED,
            cap::RESYNC => CAP_RESYNC,
            cap::KEEPALIVE => CAP_KEEPALIVE,
            _ => 0,
        };
    }
    bits
}

/// The queue of encoded frames waiting to be written to one client, plus its
/// backpressure state.
#[derive(Default)]
struct Outbox {
    /// Encoded frames waiting to go out, oldest first.
    queue: VecDeque<Vec<u8>>,
    /// Set when the queue overflowed and was dropped: the writer refreshes the
    /// client with a fresh [`Resync`] replay before resuming live output.
    resync: bool,
    /// Set on disconnect (either side) so both threads wind down.
    closed: bool,
}

/// One connected client: its id, negotiated capabilities, subscription state,
/// and outbox. Shared (`Arc`) between the reader thread, the writer thread, and
/// every broadcaster.
pub struct ClientChannel {
    /// Stable id, used to deregister and to track the active (sizing) client.
    pub id: u64,
    caps: AtomicU16,
    /// Whether this client is subscribed to the output stream (`attach`).
    attached: std::sync::atomic::AtomicBool,
    out: Mutex<Outbox>,
    cv: Condvar,
}

impl ClientChannel {
    fn new(id: u64) -> Self {
        Self {
            id,
            caps: AtomicU16::new(0),
            attached: std::sync::atomic::AtomicBool::new(false),
            out: Mutex::new(Outbox::default()),
            cv: Condvar::new(),
        }
    }

    fn set_caps(&self, list: &[String]) {
        self.caps.store(caps_from_list(list), Ordering::Relaxed);
    }

    fn has(&self, bit: u16) -> bool {
        self.caps.load(Ordering::Relaxed) & bit != 0
    }

    /// Whether this client negotiated backpressure `resync` (so `attach` knows
    /// whether it can send the initial snapshot).
    pub(crate) fn wants_resync(&self) -> bool {
        self.has(CAP_RESYNC)
    }

    fn is_attached(&self) -> bool {
        self.attached.load(Ordering::Relaxed)
    }

    /// Enqueue an output-stream frame, honoring backpressure. On overflow the
    /// queue is dropped and the client is flagged for a [`Resync`] (if it
    /// negotiated the capability) or closed.
    fn enqueue(&self, bytes: &[u8]) {
        let mut st = self.out.lock().unwrap();
        if st.closed || st.resync {
            // While a resync is pending, drop everything: the writer will send a
            // fresh replay, so queuing stale bytes behind it is pointless and
            // would re-apply output the replay already reflects.
            return;
        }
        if st.queue.len() >= CLIENT_QUEUE_LIMIT {
            st.queue.clear();
            if self.has(CAP_RESYNC) {
                st.resync = true;
            } else {
                st.closed = true;
            }
        } else {
            st.queue.push_back(bytes.to_vec());
        }
        self.cv.notify_one();
    }

    /// Enqueue a required low-volume frame (a reply or keepalive) past the
    /// backpressure gate; these are rare and must not be dropped.
    pub(crate) fn enqueue_priority(&self, bytes: &[u8]) {
        let mut st = self.out.lock().unwrap();
        if st.closed {
            return;
        }
        st.queue.push_back(bytes.to_vec());
        self.cv.notify_one();
    }

    pub(crate) fn mark_attached(&self, yes: bool) {
        self.attached.store(yes, Ordering::Relaxed);
    }

    /// Drop everything queued (used on `detach`).
    pub(crate) fn clear(&self) {
        let mut st = self.out.lock().unwrap();
        st.queue.clear();
        st.resync = false;
    }

    /// Ask both threads to wind down.
    fn close(&self) {
        let mut st = self.out.lock().unwrap();
        st.closed = true;
        self.cv.notify_all();
    }
}

/// The set of connected clients the session broadcasts to.
#[derive(Default)]
pub struct ClientRegistry {
    clients: Mutex<Vec<Arc<ClientChannel>>>,
    next_id: AtomicU64,
}

impl ClientRegistry {
    fn register(&self, ch: Arc<ClientChannel>) {
        self.clients.lock().unwrap().push(ch);
    }

    fn deregister(&self, id: u64) {
        self.clients.lock().unwrap().retain(|c| c.id != id);
    }

    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Whether any client is subscribed to output (so the reader can skip
    /// encoding when nobody is listening).
    pub fn has_attached(&self) -> bool {
        self.clients.lock().unwrap().iter().any(|c| c.is_attached())
    }

    /// Number of connected clients (for tests / introspection).
    pub fn len(&self) -> usize {
        self.clients.lock().unwrap().len()
    }

    /// Whether no client is connected.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Enqueue an [`Output`](shelbi_proto::Output) frame to every attached client.
    pub fn enqueue_output(&self, bytes: &[u8]) {
        for c in self.clients.lock().unwrap().iter() {
            if c.is_attached() {
                c.enqueue(bytes);
            }
        }
    }

    /// Enqueue an in-band [`Resized`](shelbi_proto::Resized) marker to attached
    /// clients that negotiated `output-resized`. A client that did not stays on
    /// the output stream (ordered), it just never reflows.
    pub fn enqueue_inband_resized(&self, bytes: &[u8]) {
        for c in self.clients.lock().unwrap().iter() {
            if c.is_attached() && c.has(CAP_OUTPUT_RESIZED) {
                c.enqueue(bytes);
            }
        }
    }

    /// Push an event frame to attached clients that negotiated `cap`.
    fn push_event(&self, cap: u16, bytes: &[u8]) {
        for c in self.clients.lock().unwrap().iter() {
            if c.is_attached() && c.has(cap) {
                c.enqueue(bytes);
            }
        }
    }

    /// Pushed event: the title changed.
    pub fn push_event_title(&self, title: &str) {
        if let Ok(bytes) = ExtFrame::EventTitle(shelbi_proto::EventTitle {
            title: title.to_string(),
        })
        .encode()
        {
            self.push_event(CAP_EVENT_TITLE, &bytes);
        }
    }

    /// Pushed event: the bell rang.
    pub fn push_event_bell(&self) {
        if let Ok(bytes) = ExtFrame::EventBell.encode() {
            self.push_event(CAP_EVENT_BELL, &bytes);
        }
    }

    /// Pushed event: the size changed (out-of-band form).
    pub fn push_event_resized(&self, cols: u16, rows: u16) {
        if let Ok(bytes) = ExtFrame::EventResized(shelbi_proto::EventResized { cols, rows }).encode()
        {
            self.push_event(CAP_EVENT_RESIZED, &bytes);
        }
    }

    /// Broadcast the frozen-core [`Exited`](shelbi_proto::Exited) event to every
    /// connected client (attached or not), as a priority frame so a lagging
    /// client still learns the session ended.
    pub fn broadcast_exited(&self, exited: shelbi_proto::Exited) {
        if let Ok(bytes) = Frame::Exited(exited).encode() {
            for c in self.clients.lock().unwrap().iter() {
                c.enqueue_priority(&bytes);
            }
        }
    }

    /// Send a keepalive `ping` to every connected client that negotiated it.
    pub fn send_keepalives(&self) {
        if let Ok(bytes) = ExtFrame::Ping.encode() {
            for c in self.clients.lock().unwrap().iter() {
                if c.has(CAP_KEEPALIVE) {
                    c.enqueue_priority(&bytes);
                }
            }
        }
    }

    /// Close every client (used on session teardown so writer threads wind down).
    pub fn close_all(&self) {
        for c in self.clients.lock().unwrap().iter() {
            c.close();
        }
    }
}

/// Serve one client connection to completion (returns when the client
/// disconnects). Registers the client, spawns its writer thread, and handles
/// request frames on this thread.
pub fn serve_client(stream: UnixStream, shared: Arc<Shared>) {
    let ch = Arc::new(ClientChannel::new(shared.clients.next_id()));
    shared.clients.register(ch.clone());

    let write_half = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => {
            shared.clients.deregister(ch.id);
            return;
        }
    };
    // A second handle so the writer can unblock a reader parked on `read` when
    // the connection dies on the write side.
    let shutdown_half = stream.try_clone().ok();

    let writer = {
        let ch = ch.clone();
        let shared = shared.clone();
        std::thread::spawn(move || {
            client_writer(ch, write_half, || shared.resync_base(), shutdown_half)
        })
    };

    let _ = read_loop(stream, &shared, &ch);

    // Wind down: stop the writer, forget the client, release its sizing slot.
    ch.close();
    shared.clients.deregister(ch.id);
    shared.forget_client(ch.id);
    let _ = writer.join();
}

/// The per-client writer: drain the outbox to the socket, refreshing a
/// lagging client with a [`Resync`] replay before resuming live output.
///
/// `resync_base` yields the `(seq, replay)` a backpressure refresh sends (the
/// session passes [`Shared::resync_base`](crate::session::Shared::resync_base));
/// taking it as a closure rather than `Arc<Shared>` keeps the loop testable in
/// isolation.
fn client_writer(
    ch: Arc<ClientChannel>,
    mut sock: UnixStream,
    resync_base: impl Fn() -> (u64, Vec<u8>),
    shutdown_half: Option<UnixStream>,
) {
    // Bound every write so a client that stopped reading can never park this
    // thread (and the reader's `writer.join()`) forever. A stall past the
    // timeout surfaces as a `WouldBlock`/`TimedOut` error from `write_all`,
    // which the loop below treats like any other write failure: tear the
    // connection down. See [`CLIENT_WRITE_TIMEOUT`].
    let _ = sock.set_write_timeout(Some(CLIENT_WRITE_TIMEOUT));

    loop {
        let (batch, do_resync) = {
            let mut st = ch.out.lock().unwrap();
            while st.queue.is_empty() && !st.resync && !st.closed {
                st = ch.cv.wait(st).unwrap();
            }
            if st.closed && st.queue.is_empty() && !st.resync {
                break;
            }
            if st.resync {
                st.resync = false;
                st.queue.clear();
                (Vec::new(), true)
            } else {
                (st.queue.drain(..).collect::<Vec<_>>(), false)
            }
        };

        if do_resync {
            let (seq, replay) = resync_base();
            match ExtFrame::Resync(Resync { seq, replay }).encode() {
                Ok(bytes) if sock.write_all(&bytes).is_ok() => {}
                _ => break,
            }
            let _ = sock.flush();
            continue;
        }

        let mut failed = false;
        for frame in batch {
            if sock.write_all(&frame).is_err() {
                failed = true;
                break;
            }
        }
        if failed {
            break;
        }
        let _ = sock.flush();
    }

    // The connection is done on the write side; make sure the reader thread
    // parked on `read` wakes so `serve_client` can return.
    ch.close();
    if let Some(s) = shutdown_half {
        let _ = s.shutdown(Shutdown::Both);
    }
}

/// Read and dispatch request frames until the client disconnects or errors.
///
/// Until the client's [`Hello`] arrives, reads are bounded by [`HELLO_TIMEOUT`]:
/// a peer that connects but never completes the handshake is dropped so its
/// reader/writer threads and descriptors can't leak (see [`HELLO_TIMEOUT`]). Once
/// the hello is in, the bound is cleared and subsequent reads block normally —
/// a connected, handshaken client that simply sits idle is expected and kept.
fn read_loop(
    mut stream: UnixStream,
    shared: &Arc<Shared>,
    ch: &Arc<ClientChannel>,
) -> std::io::Result<()> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    let mut hello_seen = false;
    // Absolute deadline for the whole pre-hello phase, so a peer that dribbles
    // bytes without ever completing the hello still can't hold the handler past
    // the window — each pre-hello read is bounded by the time left, not a fresh
    // full timeout.
    let window = hello_timeout();
    let hello_deadline = Instant::now() + window;
    // Best-effort: `set_read_timeout` on a socket whose peer has already closed
    // errors (EINVAL on macOS). That is never a reason to abandon the connection
    // — buffered frames may still be waiting and the read below returns them (or
    // a prompt EOF). The bound matters only for a still-connected silent peer,
    // and on a live socket the call succeeds, so ignoring the error is safe.
    let _ = stream.set_read_timeout(Some(window));
    loop {
        loop {
            match decode_any(&buf) {
                Ok((frame, consumed)) => {
                    buf.drain(..consumed);
                    let is_hello = matches!(&frame, AnyFrame::Core(Frame::Hello(_)));
                    handle_frame(frame, shared, ch);
                    if is_hello && !hello_seen {
                        // Handshake done: drop the bound so steady-state reads
                        // (which wait for input that may be minutes away) block.
                        hello_seen = true;
                        let _ = stream.set_read_timeout(None);
                    }
                }
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => break,
                Err(_) => return Ok(()), // malformed/unknown: stop serving this peer
            }
        }
        if !hello_seen {
            // Tighten the per-read timeout to the time left in the window, so the
            // pre-hello phase as a whole can't outlast `HELLO_TIMEOUT`.
            let remaining = hello_deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(());
            }
            let _ = stream.set_read_timeout(Some(remaining));
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A read timeout (WouldBlock/TimedOut, platform-dependent) before the
            // hello means the peer never handshook in time: drop it so the handler
            // and its descriptors are released. After the hello the bound is
            // cleared, so this branch can only fire pre-hello.
            Err(e)
                if !hello_seen
                    && matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
            {
                return Ok(());
            }
            Err(e) => return Err(e),
        }
    }
}

fn handle_frame(frame: AnyFrame, shared: &Arc<Shared>, ch: &Arc<ClientChannel>) {
    match frame {
        AnyFrame::Core(Frame::Hello(hello)) => {
            ch.set_caps(&hello.capabilities);
            if let Some(colors) = hello.colors {
                shared.colors.set(
                    (colors.foreground.r, colors.foreground.g, colors.foreground.b),
                    (colors.background.r, colors.background.g, colors.background.b),
                );
            }
            // Announce every capability this session supports.
            let reply = Hello {
                protocol_version: shelbi_proto::PROTOCOL_VERSION,
                colors: None,
                capabilities: shelbi_proto::capability::ALL
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            };
            if let Ok(bytes) = Frame::Hello(reply).encode() {
                ch.enqueue_priority(&bytes);
            }
        }
        AnyFrame::Core(Frame::Attach(_)) => shared.attach(ch),
        AnyFrame::Core(Frame::Input(input)) => {
            shared.on_input(ch.id);
            shared.write_to_child(&input.data);
        }
        AnyFrame::Core(Frame::Resize(r)) => shared.on_resize(ch.id, r.cols, r.rows),
        AnyFrame::Core(Frame::Snapshot(req)) => {
            let text = shared.snapshot_text(req.history_lines);
            if let Ok(bytes) = Frame::SnapshotData(SnapshotData { text }).encode() {
                ch.enqueue_priority(&bytes);
            }
        }
        AnyFrame::Core(Frame::Kill(kill)) => shared.kill_child_group(kill.signal),
        AnyFrame::Ext(ExtFrame::Info(_)) => {
            let info = shared.info_data();
            if let Ok(bytes) = ExtFrame::InfoData(info).encode() {
                ch.enqueue_priority(&bytes);
            }
        }
        AnyFrame::Ext(ExtFrame::Paste(p)) => {
            shared.on_input(ch.id);
            shared.paste(&p.text);
        }
        AnyFrame::Ext(ExtFrame::SetMeta(m)) => shared.set_meta(m.name, m.task),
        AnyFrame::Ext(ExtFrame::Detach) => shared.detach(ch),
        AnyFrame::Ext(ExtFrame::Ping) => {
            if let Ok(bytes) = ExtFrame::Pong.encode() {
                ch.enqueue_priority(&bytes);
            }
        }
        // Everything else is a reply/event/pong the server never receives, or a
        // frame it does not act on: ignore.
        _ => {}
    }
}

/// Build the [`InfoData`] reply from the current session state. Lives here
/// because it reads several `Shared` pieces; called from [`handle_frame`].
pub fn info_data(shared: &Shared) -> InfoData {
    let (title, cols, rows, alt_screen, bracketed_paste) = {
        let emu = shared.emu.lock().unwrap();
        let (c, r) = emu.size();
        (
            emu.title(),
            c,
            r,
            emu.alt_screen_active(),
            emu.bracketed_paste_active(),
        )
    };
    let meta = shared.meta.lock().unwrap();
    InfoData {
        title,
        cols,
        rows,
        alt_screen,
        bracketed_paste,
        kitty_flags: shared.kitty.get(),
        name: meta.name.clone(),
        task: meta.task.clone(),
        argv: meta.argv.clone(),
        cwd: meta.cwd.to_string_lossy().into_owned(),
        child_running: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::io::AsRawFd;
    use std::time::{Duration, Instant};

    /// Join `handle` within `timeout`, returning whether it finished. The join
    /// runs on a helper thread so a writer that never winds down leaks that
    /// thread instead of hanging the test forever.
    fn join_within(handle: std::thread::JoinHandle<()>, timeout: Duration) -> bool {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = handle.join();
            let _ = tx.send(());
        });
        rx.recv_timeout(timeout).is_ok()
    }

    /// Shrink a socket's send/receive buffers so a modest queue is guaranteed to
    /// fill them — making "the write blocks" deterministic regardless of the
    /// platform's (auto-tuned, sometimes multi-MB) default buffer size.
    fn shrink_buffer(stream: &UnixStream, opt: libc::c_int) {
        let size: libc::c_int = 1024;
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &size as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
    }

    #[test]
    fn a_writer_to_a_client_that_never_reads_winds_down_within_the_timeout() {
        // The wedge this fixes: a client keeps its socket open but stops reading,
        // so the kernel buffers fill and a write to it blocks. Without a bound the
        // writer parks in that write forever (and `serve_client`'s `writer.join()`
        // with it), leaking the pair of threads and the connection's descriptors.
        // With the write timeout the blocked write errors out and the writer winds
        // down (`rt-review-session-wedges-after-repeated-attaches`).
        let (session_side, client_side) = UnixStream::pair().unwrap();
        // Tiny buffers on both ends so the queued frames below cannot be absorbed
        // without a reader: the write is forced to block on the silent peer.
        shrink_buffer(&session_side, libc::SO_SNDBUF);
        shrink_buffer(&client_side, libc::SO_RCVBUF);
        let ch = Arc::new(ClientChannel::new(1));
        ch.mark_attached(true);

        // Queue well past the (now tiny) socket buffer, so the writer is
        // guaranteed to block on a write to the never-reading `client_side`.
        let big = vec![0u8; 64 * 1024];
        for _ in 0..16 {
            ch.enqueue_priority(&big);
        }

        let shutdown_half = session_side.try_clone().ok();
        let writer = {
            let ch = ch.clone();
            std::thread::spawn(move || {
                client_writer(ch, session_side, || (0, Vec::new()), shutdown_half)
            })
        };

        let start = Instant::now();
        let wound_down = join_within(writer, CLIENT_WRITE_TIMEOUT + Duration::from_secs(10));
        assert!(
            wound_down,
            "the writer blocked forever on a non-reading client instead of timing out",
        );
        // It genuinely stalled on the write (rather than erroring instantly),
        // then wound down — within a slack of the timeout to tolerate a timer that
        // fires a touch early under load.
        assert!(
            start.elapsed() >= CLIENT_WRITE_TIMEOUT / 2,
            "it wound down before the write could even stall ({:?})",
            start.elapsed(),
        );

        // `client_side` is held open the whole time (the point of the test); drop
        // it only now.
        drop(client_side);
    }

    #[test]
    fn a_clean_close_winds_the_writer_down_promptly() {
        // The common path: the client closes its end. The writer, parked on the
        // condvar with nothing to send, must wake and return as soon as the
        // channel is closed — never wait out the write timeout.
        let (session_side, client_side) = UnixStream::pair().unwrap();
        let ch = Arc::new(ClientChannel::new(2));
        let shutdown_half = session_side.try_clone().ok();
        let writer = {
            let ch = ch.clone();
            std::thread::spawn(move || {
                client_writer(ch, session_side, || (0, Vec::new()), shutdown_half)
            })
        };
        drop(client_side);
        ch.close();
        assert!(
            join_within(writer, Duration::from_secs(2)),
            "a closed channel must wind the writer down at once",
        );
    }
}
