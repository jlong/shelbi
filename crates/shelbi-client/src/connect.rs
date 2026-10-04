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

use shelbi_proto::{
    capability, ClientColors, ExtFrame, Frame, Hello, Info, Input, Kill, Resize, SetMeta, Snapshot,
    SnapshotData, PROTOCOL_VERSION,
};

use crate::error::ClientError;
use crate::reader::{self, Reply, SessionEvent};

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

/// A live connection to one session, past the hello handshake.
pub struct Connection {
    /// The write half, shared with the reader thread (which writes keepalive
    /// pongs). A whole frame is written under this lock, so writes never
    /// interleave mid-frame — the client-side half of input arbitration.
    write: Arc<Mutex<UnixStream>>,
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
        let stream = UnixStream::connect(sock)?;
        Self::handshake(stream, colors, capabilities)
    }

    /// Like [`open`](Connection::open) but over an already-connected stream (used
    /// by the relay transport seam and by tests).
    pub fn handshake(
        stream: UnixStream,
        colors: Option<ClientColors>,
        capabilities: &[&str],
    ) -> Result<(Self, SessionEvents), ClientError> {
        let mut hs = stream.try_clone()?;
        // Send our hello.
        let hello = Frame::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            colors,
            capabilities: capabilities.iter().map(|s| s.to_string()).collect(),
        })
        .encode()?;
        hs.write_all(&hello)?;
        hs.flush()?;

        // Read the session's hello (frame by frame off a small local buffer; any
        // output the session sends before we attach cannot arrive yet because we
        // have not attached, so the first frame is the hello).
        let (announced, session_protocol_version) = read_session_hello(&mut hs)?;

        // Split into a shared write half and a reader-owned read half.
        let write = Arc::new(Mutex::new(hs));
        let read_half = stream;
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

    /// Subscribe to the output stream. The session replies with a replay
    /// (currently a [`SessionEvent::Resync`] snapshot) first, then live output,
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

/// Read frames from `stream` until the session's [`Hello`] arrives, returning the
/// announced capabilities and the session's protocol version.
fn read_session_hello(stream: &mut UnixStream) -> Result<(Vec<String>, u16), ClientError> {
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
        match stream.read(&mut chunk)? {
            0 => return Err(ClientError::UnexpectedEof),
            n => buf.extend_from_slice(&chunk[..n]),
        }
    }
}
