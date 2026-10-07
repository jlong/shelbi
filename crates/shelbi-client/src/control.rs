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
    self, ChangeNote, ClientMsg, MutationRequest, ReviewSessionRequest, ServerMsg,
    Stream as OutStream, WorkspaceSessionRequest, CONTROL_PROTOCOL_VERSION,
};

use crate::error::ClientError;

/// A push a subscribed connection receives from the daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    /// Another client changed an issue.
    Changed(ChangeNote),
    /// The daemon asked this client to re-exec (a TUI) or prompt for relaunch
    /// (the desktop app) and stop sending commands. `reason` is operator-facing.
    Reexec { reason: String },
}

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

    /// Quit a project (Phase 4f): the daemon ends its sessions, cancels its
    /// in-flight jobs through the quit barrier, and marks it closed. Blocks
    /// until the daemon reports done.
    pub fn quit_project(&mut self, project: &str) -> Result<(), ClientError> {
        self.lifecycle(ClientMsg::QuitProject {
            request_id: 1,
            project: project.to_string(),
        })
    }

    /// Quit Shelbi (Phase 4f): the daemon closes every project, ends all
    /// sessions, acknowledges, and then stops. Blocks until the acknowledgement;
    /// the daemon may close the connection as it stops, which counts as done.
    pub fn quit_shelbi(&mut self) -> Result<(), ClientError> {
        self.lifecycle(ClientMsg::QuitShelbi { request_id: 1 })
    }

    /// Ask the daemon to tell every subscribed client to re-exec (the reload
    /// signal). Blocks until the daemon acknowledges.
    pub fn reload_clients(&mut self) -> Result<(), ClientError> {
        self.lifecycle(ClientMsg::ReloadClients { request_id: 1 })
    }

    /// Send a lifecycle request (quit/reload) and wait for its `Done`/`Failed`.
    /// A clean EOF before either — the daemon stopping as part of the action —
    /// is treated as success, so `quit_shelbi` doesn't error on the race between
    /// the ack and the shutdown.
    fn lifecycle(&mut self, msg: ClientMsg) -> Result<(), ClientError> {
        let request_id = match &msg {
            ClientMsg::QuitProject { request_id, .. }
            | ClientMsg::QuitShelbi { request_id }
            | ClientMsg::ReloadClients { request_id } => *request_id,
            _ => 0,
        };
        self.write.write_all(&control::encode(&msg)?)?;
        self.write.flush()?;
        loop {
            match self.read.read_frame::<ServerMsg>() {
                Ok(Some(ServerMsg::Done { request_id: id })) if id == request_id => return Ok(()),
                Ok(Some(ServerMsg::Failed { request_id: id, error })) if id == request_id => {
                    return Err(ClientError::Mutation(error))
                }
                // A frame for something else (a stray broadcast) — keep reading.
                Ok(Some(_)) => {}
                // The daemon closed as it stopped: the action took effect.
                Ok(None) | Err(ClientError::UnexpectedEof) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    /// Ask the daemon to start or stop a review slot's editor/diff/server
    /// sessions (`rt-tui-review`). Blocks until the daemon reports
    /// [`ServerMsg::Done`] (`Ok`) or [`ServerMsg::Failed`]
    /// ([`ClientError::Mutation`]), draining any streamed lines through
    /// `on_line`. The daemon owns the sessions' lifetime, so a `Close` that
    /// returns `Ok` guarantees the editor/diff/server are gone and the port is
    /// free.
    pub fn review_session(
        &mut self,
        req: &ReviewSessionRequest,
        on_line: &mut dyn FnMut(OutStream, &str),
    ) -> Result<(), ClientError> {
        self.write
            .write_all(&control::encode(&ClientMsg::ReviewSession(req.clone()))?)?;
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
                _ => {}
            }
        }
    }

    /// Ask the daemon to start/stop a dev workspace's editor/diff content
    /// sessions (the workspace-sidebar task) — the dev-workspace twin of
    /// [`review_session`](Self::review_session). Blocks until the daemon replies
    /// [`ServerMsg::Done`] (`Ok`) or [`ServerMsg::Failed`], draining streamed
    /// lines through `on_line`.
    pub fn workspace_session(
        &mut self,
        req: &WorkspaceSessionRequest,
        on_line: &mut dyn FnMut(OutStream, &str),
    ) -> Result<(), ClientError> {
        self.write
            .write_all(&control::encode(&ClientMsg::WorkspaceSession(req.clone()))?)?;
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
                _ => {}
            }
        }
    }

    /// Subscribe this connection to change / re-exec notifications and return a
    /// blocking iterator of [`Notice`]s. The iterator ends when the daemon closes
    /// the connection.
    pub fn subscribe(mut self) -> Result<Subscription, ClientError> {
        self.write
            .write_all(&control::encode(&ClientMsg::Subscribe)?)?;
        self.write.flush()?;
        Ok(Subscription { read: self.read })
    }

    /// Whether the daemon this client connected to runs a different version than
    /// `client_version` — i.e. this client is out of date and should re-exec /
    /// relaunch rather than send mutations.
    pub fn is_out_of_date(&self, client_version: &str) -> bool {
        self.daemon_version != client_version
    }
}

/// A blocking stream of [`Notice`]s from a subscribed connection.
pub struct Subscription {
    read: FrameReader<BufReader<UnixStream>>,
}

impl Subscription {
    /// Block for the next notification (a change or a re-exec push). `Ok(None)`
    /// when the daemon closed the connection.
    pub fn recv(&mut self) -> Result<Option<Notice>, ClientError> {
        loop {
            match self.read.read_frame::<ServerMsg>()? {
                Some(ServerMsg::Changed(note)) => return Ok(Some(Notice::Changed(note))),
                Some(ServerMsg::Reexec { reason }) => return Ok(Some(Notice::Reexec { reason })),
                Some(_) => continue, // ignore mutation-stream traffic
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
