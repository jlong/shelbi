//! Connect to a session and issue blocking requests.
//!
//! A connection opens the session's socket, exchanges [`Hello`] frames (the
//! client reports its protocol version, colors, and the additive capabilities
//! it understands; the session replies announcing the capabilities it offers),
//! and then exposes a **blocking** request API: [`attach`], [`resize`],
//! [`snapshot`], [`kill`], and the raw/paste input paths. Output and pushed
//! events do not come back through the request API; they are delivered by the
//! [`reader`](crate::reader) over channels, because they are a continuous
//! stream rather than a reply.
//!
//! The client records which capabilities the session announced and uses an
//! additive one only when it is present, falling back to the frozen core
//! otherwise.
//!
//! TODO (`rt-protocol-client`): implement the handshake and the request methods
//! over the Unix-socket transport, and leave the transport behind a seam so the
//! relay transport (`rt-relay`) can slot in.
//!
//! [`Hello`]: shelbi_proto::Hello
//! [`attach`]: Connection::attach
//! [`resize`]: Connection::resize
//! [`snapshot`]: Connection::snapshot
//! [`kill`]: Connection::kill

use shelbi_proto::{Attach, ClientColors, Resize, Snapshot, SnapshotData};

/// A live connection to one session.
///
/// Skeleton: the transport and handshake are filled in by `rt-protocol-client`.
pub struct Connection {
    _private: (),
}

impl Connection {
    /// Open a connection to the session with the given short id and perform the
    /// hello handshake, reporting the client's colors and supported additive
    /// capabilities.
    ///
    /// TODO (`rt-protocol-client`): connect the socket, send and receive
    /// [`Hello`](shelbi_proto::Hello), and record the announced capabilities.
    pub fn open(_short_id: &str, _colors: ClientColors) -> Result<Self, crate::ClientError> {
        unimplemented!("rt-protocol-client implements the connection handshake")
    }

    /// The additive capabilities the session announced in its hello.
    pub fn capabilities(&self) -> &[String] {
        &[]
    }

    /// Subscribe to output. The session answers with a replay first, then live
    /// output, all delivered via the [`reader`](crate::reader).
    pub fn attach(&mut self, _req: Attach) -> Result<(), crate::ClientError> {
        unimplemented!("rt-protocol-client implements attach")
    }

    /// Report this client's viewport size.
    pub fn resize(&mut self, _req: Resize) -> Result<(), crate::ClientError> {
        unimplemented!("rt-protocol-client implements resize")
    }

    /// Request a text snapshot of the screen (optionally with history).
    pub fn snapshot(&mut self, _req: Snapshot) -> Result<SnapshotData, crate::ClientError> {
        unimplemented!("rt-snapshot implements snapshot")
    }

    /// Signal the child's process group.
    pub fn kill(&mut self, _signal: Option<i32>) -> Result<(), crate::ClientError> {
        unimplemented!("rt-protocol-client implements kill")
    }
}
