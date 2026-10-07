//! Connect to a session and issue blocking requests.
//!
//! [`Connection::open`] opens the session's socket, exchanges [`Hello`] frames
//! (the client reports its protocol version, colors, and the additive
//! capabilities it understands; the session replies announcing the capabilities
//! it offers), spawns the background [`reader`](crate::reader), and returns the
//! connection together with the [`SessionEvent`] stream.
//!
//! The returned [`Connection`] exposes a **blocking** request API:
//! [`attach`](Connection::attach), [`detach`](Connection::detach),
//! [`input`](Connection::input), [`paste`](Connection::paste),
//! [`resize`](Connection::resize), [`snapshot`](Connection::snapshot),
//! [`info`](Connection::info), [`set_meta`](Connection::set_meta), and
//! [`kill`](Connection::kill). Output and pushed events do **not** come back
//! through these methods; they are delivered by the reader over the event
//! channel, because they are a continuous stream rather than a reply.
//!
//! The client records which capabilities the session announced
//! ([`Connection::supports`]) and uses an additive one only when it is present,
//! falling back to the frozen core otherwise: [`paste`](Connection::paste) sends
//! raw [`Input`](shelbi_proto::Input) when `paste` was not announced, and
//! [`detach`](Connection::detach) is a no-op the caller handles by just dropping
//! the connection.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelbi_proto::{
    capability, ClientColors, ExtFrame, Frame, Hello, Info, Input, Kill, Resize, SetMeta, Snapshot,
    SnapshotData, PROTOCOL_VERSION,
};

use crate::error::ClientError;
use crate::reader::{self, Reply, SessionEvent, SharedWrite};
use crate::transport::{LocalTransport, Transport};

/// The receiving end of the session's output/event stream.
///
/// A plain wrapper over a channel receiver so the reader's transport stays
/// private; the caller blocks on [`recv`](SessionEvents::recv) or polls with
/// [`try_recv`](SessionEvents::try_recv).
pub struct SessionEvents {
    rx: Receiver<SessionEvent>,
}

impl SessionEvents {
    /// Block for the next event. `Err` once the reader has stopped (the child
    /// exited or the connection dropped).
    pub fn recv(&self) -> Result<SessionEvent, ClientError> {
        self.rx.recv().map_err(|_| ClientError::ReaderGone)
    }

    /// Non-blocking poll for the next event.
    pub fn try_recv(&self) -> Option<SessionEvent> {
        self.rx.try_recv().ok()
    }

    /// The underlying receiver, for callers that want to `select`/iterate it.
    pub fn into_inner(self) -> Receiver<SessionEvent> {
        self.rx
    }
}

/// How long [`Connection::open`] waits for the session's hello before giving up.
/// A session that accepts the connection but never completes the handshake (a
/// wedged session, or one from an older build this client can't speak to) would
/// otherwise block the connect — and the caller's "Connecting…" view — forever
/// (`rt-review-screen-hangs-on-connecting`). Generous enough to never trip a
/// healthy local session.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// A live connection to one session, past the hello handshake.
pub struct Connection {
    /// The write half, shared with the reader thread (which writes keepalive
    /// pongs). A whole frame is written under this lock, so writes never
    /// interleave mid-frame — the client-side half of input arbitration. Boxed
    /// so a local socket and a relay stream are the same type.
    write: SharedWrite,
    /// Serializes request/reply round-trips and owns the reply receiver, so only
    /// one in-flight request waits on a reply at a time.
    replies: Mutex<Receiver<Reply>>,
    /// Capabilities the session announced in its hello.
    announced: Vec<String>,
    /// The frozen-core protocol version the session speaks (detected, not
    /// enforced: an old session is always usable, per the compatibility policy).
    session_protocol_version: u16,
    _reader: std::thread::JoinHandle<()>,
}

impl Connection {
    /// Open a connection to the session at `sock`, perform the hello handshake
    /// reporting `colors` and the additive `capabilities` this client
    /// understands, and start the reader. Returns the connection and the event
    /// stream.
    pub fn open(
        sock: &Path,
        colors: Option<ClientColors>,
        capabilities: &[&str],
    ) -> Result<(Self, SessionEvents), ClientError> {
        Self::open_with_timeout(sock, colors, capabilities, HANDSHAKE_TIMEOUT)
    }

    /// Like [`open`](Connection::open) but with an explicit hello-handshake
    /// timeout. Exposed mainly so a caller (or a test) can bound the connect
    /// more tightly than the default [`HANDSHAKE_TIMEOUT`]; `open` is the normal
    /// entry point.
    pub fn open_with_timeout(
        sock: &Path,
        colors: Option<ClientColors>,
        capabilities: &[&str],
        handshake_timeout: Duration,
    ) -> Result<(Self, SessionEvents), ClientError> {
        let stream = UnixStream::connect(sock)?;
        Self::connect_with_timeout(
            Box::new(LocalTransport(stream)),
            colors,
            capabilities,
            Some(handshake_timeout),
        )
    }

    /// Like [`open`](Connection::open) but over an already-connected Unix
    /// socket (used by tests). Equivalent to [`connect`](Connection::connect)
    /// over a [`LocalTransport`].
    pub fn handshake(
        stream: UnixStream,
        colors: Option<ClientColors>,
        capabilities: &[&str],
    ) -> Result<(Self, SessionEvents), ClientError> {
        Self::connect(Box::new(LocalTransport(stream)), colors, capabilities)
    }

    /// Like [`handshake`](Connection::handshake) but with an explicit
    /// hello-handshake timeout (used by tests that connect a raw socket pair).
    pub fn handshake_with_timeout(
        stream: UnixStream,
        colors: Option<ClientColors>,
        capabilities: &[&str],
        handshake_timeout: Option<Duration>,
    ) -> Result<(Self, SessionEvents), ClientError> {
        Self::connect_with_timeout(
            Box::new(LocalTransport(stream)),
            colors,
            capabilities,
            handshake_timeout,
        )
    }

    /// Open a connection over any [`Transport`] — a local socket or one logical
    /// stream of a [`RelayChannel`](crate::relay::RelayChannel). Exchanges the
    /// [`Hello`] frames, starts the reader, and returns the connection and event
    /// stream. This is the single code path both local and remote connections
    /// share.
    pub fn connect(
        transport: Box<dyn Transport>,
        colors: Option<ClientColors>,
        capabilities: &[&str],
    ) -> Result<(Self, SessionEvents), ClientError> {
        Self::connect_with_timeout(transport, colors, capabilities, Some(HANDSHAKE_TIMEOUT))
    }

    /// The shared connect path, with the hello-handshake read bounded by
    /// `handshake_timeout` (`None` leaves it unbounded — used only by callers
    /// that have their own bound). A session that accepts the connection but
    /// never answers the hello surfaces [`ClientError::HandshakeTimeout`] after
    /// the deadline instead of blocking forever
    /// (`rt-review-screen-hangs-on-connecting`).
    fn connect_with_timeout(
        transport: Box<dyn Transport>,
        colors: Option<ClientColors>,
        capabilities: &[&str],
        handshake_timeout: Option<Duration>,
    ) -> Result<(Self, SessionEvents), ClientError> {
        let (mut read_half, mut write_half) = transport.split()?;

        // Send our hello.
        let hello = Frame::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            colors,
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
        })
        .encode()?;
        write_half.write_all(&hello)?;
        write_half.flush()?;

        // Bound the hello read so a channel that accepts the connection but never
        // answers can't wedge the connect forever (the "Connecting…" hang). The
        // timeout is cleared below before the reader thread starts: steady-state
        // output reads must block until output arrives.
        if let Some(t) = handshake_timeout {
            read_half.set_read_timeout(Some(t)).map_err(ClientError::Io)?;
        }
        // Read the session's hello off the read half (frame by frame off a small
        // local buffer; any output the session sends before we attach cannot
        // arrive yet because we have not attached, so the first frame is the
        // hello).
        let hello_result = read_session_hello(&mut read_half);
        // Always clear the handshake timeout before the reader takes the half,
        // even on error (the half is dropped on the error path anyway).
        let _ = read_half.set_read_timeout(None);
        let (announced, session_protocol_version) = hello_result?;

        let write: SharedWrite = Arc::new(Mutex::new(write_half));
        let (event_tx, event_rx) = channel::<SessionEvent>();
        let (reply_tx, reply_rx) = channel::<Reply>();
        let reader = reader::spawn(read_half, write.clone(), event_tx, reply_tx);

        let conn = Connection {
            write,
            replies: Mutex::new(reply_rx),
            announced,
            session_protocol_version,
            _reader: reader,
        };
        Ok((conn, SessionEvents { rx: event_rx }))
    }

    /// The additive capabilities the session announced in its hello.
    pub fn capabilities(&self) -> &[String] {
        &self.announced
    }

    /// The frozen-core protocol version the session speaks.
    pub fn session_protocol_version(&self) -> u16 {
        self.session_protocol_version
    }

    /// Whether the session announced the additive capability `name`.
    pub fn supports(&self, name: &str) -> bool {
        self.announced.iter().any(|c| c == name)
    }

    /// Write a complete frame to the session under the write lock (whole-frame,
    /// never interleaved with another writer).
    fn send(&self, bytes: &[u8]) -> Result<(), ClientError> {
        let mut w = self.write.lock().map_err(|_| ClientError::ReaderGone)?;
        w.write_all(bytes)?;
        w.flush()?;
        Ok(())
    }

    /// Send a request and block for its reply, serialized against other requests.
    fn request(&self, bytes: &[u8]) -> Result<Reply, ClientError> {
        let rx = self.replies.lock().map_err(|_| ClientError::ReaderGone)?;
        self.send(bytes)?;
        rx.recv().map_err(|_| ClientError::ReaderGone)
    }

    /// Subscribe to the output stream. The session replies with a full-state
    /// replay (a [`SessionEvent::Resync`] byte stream) first, then live output,
    /// all over the event channel. `since_seq` is reserved for exact reconnect;
    /// pass `None` for a full replay.
    pub fn attach(&self, since_seq: Option<u64>) -> Result<(), ClientError> {
        self.send(&Frame::Attach(shelbi_proto::Attach { since_seq }).encode()?)
    }

    /// Unsubscribe from the output stream. Uses the `detach` capability when the
    /// session announced it; otherwise this is a no-op (the caller falls back to
    /// dropping the connection).
    pub fn detach(&self) -> Result<(), ClientError> {
        if self.supports(capability::DETACH) {
            self.send(&ExtFrame::Detach.encode()?)
        } else {
            Ok(())
        }
    }

    /// Send raw bytes to the PTY.
    pub fn input(&self, bytes: &[u8]) -> Result<(), ClientError> {
        self.send(&Frame::Input(Input { data: bytes.to_vec() }).encode()?)
    }

    /// Paste `text`. Uses the `paste` capability (bracketed paste when the
    /// program enabled it) when the session announced it; otherwise falls back to
    /// sending the text as raw [`Input`](shelbi_proto::Input).
    pub fn paste(&self, text: &str) -> Result<(), ClientError> {
        if self.supports(capability::PASTE) {
            self.send(&ExtFrame::Paste(shelbi_proto::Paste { text: text.to_string() }).encode()?)
        } else {
            self.input(text.as_bytes())
        }
    }

    /// Report this client's viewport size.
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), ClientError> {
        self.send(&Frame::Resize(Resize { cols, rows }).encode()?)
    }

    /// Request a text snapshot of the screen (optionally with `history_lines` of
    /// scrollback). Blocks for the reply.
    pub fn snapshot(&self, history_lines: Option<u32>) -> Result<SnapshotData, ClientError> {
        match self.request(&Frame::Snapshot(Snapshot { history_lines }).encode()?)? {
            Reply::Snapshot(s) => Ok(s),
            Reply::Info(_) => Err(ClientError::UnexpectedEof),
        }
    }

    /// Request current session facts (title, size, mode flags, metadata, child
    /// state). Requires the `info` capability — there is no frozen-core fallback.
    pub fn info(&self) -> Result<shelbi_proto::InfoData, ClientError> {
        if !self.supports(capability::INFO) {
            return Err(ClientError::Unsupported(capability::INFO));
        }
        match self.request(&ExtFrame::Info(Info::default()).encode()?)? {
            Reply::Info(i) => Ok(i),
            Reply::Snapshot(_) => Err(ClientError::UnexpectedEof),
        }
    }

    /// Update the session's metadata (`None` leaves a field unchanged; a `task`
    /// of `Some("")` clears it). Requires the `set-meta` capability.
    pub fn set_meta(
        &self,
        name: Option<String>,
        task: Option<String>,
    ) -> Result<(), ClientError> {
        if !self.supports(capability::SET_META) {
            return Err(ClientError::Unsupported(capability::SET_META));
        }
        self.send(&ExtFrame::SetMeta(SetMeta { name, task }).encode()?)
    }

    /// Signal the child's process group (`None` = the session default).
    pub fn kill(&self, signal: Option<i32>) -> Result<(), ClientError> {
        self.send(&Frame::Kill(Kill { signal }).encode()?)
    }
}

/// Whether the session at an already-connected `stream` answers the hello
/// handshake within `timeout` — the round-trip a supervision liveness probe
/// needs to tell a healthy listener from a *wedged* session that accepts the
/// connection and then never replies.
///
/// A bare `connect` cannot distinguish the two: a session whose per-connection
/// handlers are all blocked still completes the TCP/Unix accept, so every connect
/// succeeds even though no client can attach
/// (`rt-review-session-wedges-after-repeated-attaches`). This sends a hello and
/// waits for the reply under a bounded read, so a silent session surfaces instead
/// of looking alive.
///
/// A **throwaway** probe: it borrows the stream, exchanges the hello, and returns
/// — it does **not** start a reader thread or build a [`Connection`]. The caller
/// drops `stream` right after, closing the probe connection so the session winds
/// its handler down. Returns `Ok(())` when the session answered,
/// [`ClientError::HandshakeTimeout`] when it stayed silent past the deadline, and
/// [`ClientError::UnexpectedEof`] when it dropped the connection before replying
/// (both of which the caller reads as "not answering").
pub fn probe_handshake(stream: &UnixStream, timeout: Duration) -> Result<(), ClientError> {
    stream.set_read_timeout(Some(timeout)).map_err(ClientError::Io)?;
    stream.set_write_timeout(Some(timeout)).map_err(ClientError::Io)?;
    let hello = Frame::Hello(Hello {
        protocol_version: PROTOCOL_VERSION,
        colors: None,
        capabilities: Vec::new(),
    })
    .encode()?;
    {
        // `&UnixStream` is `Write`, so we can write without consuming the stream.
        let mut w: &UnixStream = stream;
        w.write_all(&hello)?;
        w.flush()?;
    }
    // `&UnixStream` is `Read`; `read_session_hello` maps a timed-out read to
    // `HandshakeTimeout` and an EOF-before-hello to `UnexpectedEof`.
    let mut r: &UnixStream = stream;
    read_session_hello(&mut r).map(|_| ())
}

/// Read frames from `stream` until the session's [`Hello`] arrives, returning the
/// announced capabilities and the session's protocol version.
///
/// When `stream` has a read timeout set (the handshake bound), a read that times
/// out surfaces as [`ClientError::HandshakeTimeout`] so a session that accepts
/// the connection but never answers ends the connect instead of blocking it
/// (`rt-review-screen-hangs-on-connecting`).
fn read_session_hello<R: Read + ?Sized>(stream: &mut R) -> Result<(Vec<String>, u16), ClientError> {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match Frame::decode(&buf) {
            Ok((Frame::Hello(h), _)) => return Ok((h.capabilities, h.protocol_version)),
            // A non-hello frame before the hello would be a protocol error.
            Ok(_) => return Err(ClientError::UnexpectedEof),
            // Not a whole frame yet: read more and retry.
            Err(shelbi_proto::ProtoError::Incomplete { .. }) => {}
            Err(e) => return Err(ClientError::Protocol(e)),
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err(ClientError::UnexpectedEof),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            // A socket read timeout (the handshake bound) returns WouldBlock or
            // TimedOut depending on the platform; either means the session never
            // sent its hello in time.
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                return Err(ClientError::HandshakeTimeout)
            }
            Err(e) => return Err(ClientError::Io(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_proto::capability;
    use std::os::unix::net::UnixListener;
    use std::time::Instant;

    /// A session socket that *accepts* the connection but never sends its hello
    /// (the live hang: a wedged or older-build session). The connect must give
    /// up within the handshake timeout rather than block forever.
    #[test]
    fn a_socket_that_accepts_but_never_answers_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        // Accept on a thread and hold the connection open without ever writing,
        // so the client's hello read has nothing to decode.
        let accepted = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            // Keep the accepted end alive (and silent) past the client's bound.
            std::thread::sleep(Duration::from_secs(2));
            drop(stream);
        });

        let timeout = Duration::from_millis(150);
        let start = Instant::now();
        let result = Connection::open_with_timeout(&sock, None, capability::ALL, timeout);
        let elapsed = start.elapsed();

        match result {
            Err(ClientError::HandshakeTimeout) => {}
            Err(other) => panic!("expected HandshakeTimeout, got {other:?}"),
            Ok(_) => panic!("a silent session must not complete the connect"),
        }
        // It gave up near the deadline, not instantly and not forever.
        assert!(
            elapsed >= timeout && elapsed < Duration::from_secs(1),
            "the connect should end right after the bound (took {elapsed:?})",
        );
        let _ = accepted.join();
    }
}
