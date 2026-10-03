//! Client side of the **mutation control protocol** ([`shelbi_proto::control`]).
//!
//! A [`ControlClient`] connects to the daemon's control socket, performs the
//! hello handshake, and either runs one mutation ([`ControlClient::mutate`],
//! driving a line callback and blocking until the daemon reports done/failed)
//! or subscribes to change notifications ([`ControlClient::subscribe`], a
//! blocking iterator of [`ChangeNote`]).
//!
//! Like the rest of this crate it is runtime-agnostic — a blocking Unix-socket
//! client, no tokio. Discovering the socket path and starting the daemon on
//! demand belong to the caller (they live in `shelbi-state`, which this crate
//! does not depend on); [`ControlClient::connect`] takes a ready path.

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use serde::de::DeserializeOwned;
use shelbi_proto::control::{
    self, ChangeNote, ClientMsg, MutationRequest, ServerMsg, Stream as OutStream,
    CONTROL_PROTOCOL_VERSION,
};

use crate::error::ClientError;

/// A connection to the daemon's mutation control socket, past the hello
/// handshake.
pub struct ControlClient {
    write: UnixStream,
    read: FrameReader<BufReader<UnixStream>>,
    /// The daemon version from its hello, for diagnostics.
    pub daemon_version: String,
}

impl ControlClient {
    /// Connect to the control socket at `path`, send the client hello, and read
    /// the daemon's hello. Errors on a control-protocol mismatch.
    pub fn connect(path: &Path, client_version: &str) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path)?;
        let read_half = stream.try_clone()?;
        let mut write = stream;
        let mut read = FrameReader::new(BufReader::new(read_half));

        write.write_all(&control::encode(&ClientMsg::Hello {
            protocol: CONTROL_PROTOCOL_VERSION,
            client_version: client_version.to_string(),
        })?)?;
        write.flush()?;

        let hello: ServerMsg = read.read_frame()?.ok_or(ClientError::UnexpectedEof)?;
        let daemon_version = match hello {
            ServerMsg::Hello {
                protocol,
                daemon_version,
            } => {
                if protocol != CONTROL_PROTOCOL_VERSION {
                    return Err(ClientError::ControlProtocolMismatch {
                        daemon: protocol,
                        client: CONTROL_PROTOCOL_VERSION,
                    });
                }
                daemon_version
            }
            _ => return Err(ClientError::UnexpectedEof),
        };

        Ok(Self {
            write,
            read,
            daemon_version,
        })
    }

    /// Run one mutation. `on_line` is called for each streamed output line (the
    /// daemon reproduces the in-process stdout/stderr). Blocks until the daemon
    /// reports [`ServerMsg::Done`] (returns `Ok`) or [`ServerMsg::Failed`]
    /// (returns [`ClientError::Mutation`]). Change notifications that arrive on
    /// this connection are ignored.
    pub fn mutate(
        &mut self,
        req: &MutationRequest,
        on_line: &mut dyn FnMut(OutStream, &str),
    ) -> Result<(), ClientError> {
        self.write
            .write_all(&control::encode(&ClientMsg::Mutate(req.clone()))?)?;
        self.write.flush()?;

        loop {
            let msg: ServerMsg = self.read.read_frame()?.ok_or(ClientError::UnexpectedEof)?;
            match msg {
                ServerMsg::Line {
                    request_id,
                    stream,
                    text,
                } if request_id == req.request_id => on_line(stream, &text),
                ServerMsg::Done { request_id } if request_id == req.request_id => return Ok(()),
                ServerMsg::Failed { request_id, error } if request_id == req.request_id => {
                    return Err(ClientError::Mutation(error))
                }
                // Lines/terminals for another request, or a broadcast Changed:
                // not ours — ignore and keep reading.
                _ => {}
            }
        }
    }

    /// Subscribe this connection to change notifications and return a blocking
    /// iterator of [`ChangeNote`]s. The iterator ends when the daemon closes the
    /// connection.
    pub fn subscribe(mut self) -> Result<Subscription, ClientError> {
        self.write
            .write_all(&control::encode(&ClientMsg::Subscribe)?)?;
        self.write.flush()?;
        Ok(Subscription { read: self.read })
    }
}

/// A blocking stream of [`ChangeNote`]s from a subscribed connection.
pub struct Subscription {
    read: FrameReader<BufReader<UnixStream>>,
}

impl Subscription {
    /// Block for the next change notification. `Ok(None)` when the daemon closed
    /// the connection.
    pub fn recv(&mut self) -> Result<Option<ChangeNote>, ClientError> {
        loop {
            match self.read.read_frame::<ServerMsg>()? {
                Some(ServerMsg::Changed(note)) => return Ok(Some(note)),
                Some(_) => continue, // ignore any non-Changed traffic
                None => return Ok(None),
            }
        }
    }
}

/// Reads length-prefixed control frames off a blocking reader, buffering partial
/// reads so a frame split across syscalls reassembles.
struct FrameReader<R: Read> {
    inner: R,
    buf: Vec<u8>,
    /// Offset of the first undecoded byte in `buf`.
    start: usize,
}

impl<R: Read> FrameReader<R> {
    fn new(inner: R) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(4096),
            start: 0,
        }
    }

    /// Decode the next message, reading more bytes as needed. `Ok(None)` on a
    /// clean EOF at a frame boundary.
    fn read_frame<T: DeserializeOwned>(&mut self) -> Result<Option<T>, ClientError> {
        loop {
            match control::decode::<T>(&self.buf[self.start..]) {
                Ok((msg, consumed)) => {
                    self.start += consumed;
                    // Compact occasionally so `buf` doesn't grow unbounded.
                    if self.start > 1 << 16 {
                        self.buf.drain(..self.start);
                        self.start = 0;
                    }
                    return Ok(Some(msg));
                }
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => {
                    let mut chunk = [0u8; 4096];
                    let n = self.inner.read(&mut chunk)?;
                    if n == 0 {
                        // EOF: clean only if nothing is buffered mid-frame.
                        return if self.start == self.buf.len() {
                            Ok(None)
                        } else {
                            Err(ClientError::UnexpectedEof)
                        };
                    }
                    self.buf.extend_from_slice(&chunk[..n]);
                }
                Err(e) => return Err(ClientError::Protocol(e)),
            }
        }
    }
}
