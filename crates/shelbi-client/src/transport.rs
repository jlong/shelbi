//! The transport seam: one [`Connection`](crate::Connection) over either a
//! local socket or a remote relay channel.
//!
//! A [`Connection`] drives the session protocol over a byte channel it splits
//! into an independent read half and write half (the reader thread owns the
//! read half; requests and keepalive pongs share the write half under a lock).
//! What that channel *is* — a Unix socket to a session on this machine, or one
//! logical stream of a [`RelayChannel`](crate::relay::RelayChannel) bridged over
//! SSH to a remote machine — is the only thing that differs between local and
//! remote. [`Transport`] is that seam: the request API, capability gating, the
//! reader, and every frame are identical on both sides.
//!
//! Runtime-agnostic like the rest of the crate: the halves are plain blocking
//! [`Read`]/[`Write`] objects, no async runtime.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use crate::error::ClientError;

/// A [`Read`] whose reads can be bounded by a timeout. Used only to bound the
/// hello handshake so a channel that accepts the connection but never answers
/// can't block the connect forever; the timeout is cleared before the reader
/// thread starts, so steady-state output reads block normally
/// (`rt-review-screen-hangs-on-connecting`).
pub trait ReadTimeout: Read {
    /// Apply a read timeout to this half (`None` clears it). A channel with no
    /// socket-level timeout (the relay) is free to make this a no-op — its reads
    /// are already bounded by the relay keepalive.
    fn set_read_timeout(&self, dur: Option<Duration>) -> std::io::Result<()>;
}

impl ReadTimeout for UnixStream {
    fn set_read_timeout(&self, dur: Option<Duration>) -> std::io::Result<()> {
        UnixStream::set_read_timeout(self, dur)
    }
}

/// The owned read and write halves a [`Transport`] splits into: boxed so a
/// local socket and a relay stream are the same type. The read half carries
/// [`ReadTimeout`] so the connect can bound the hello handshake.
pub type TransportHalves = (Box<dyn ReadTimeout + Send>, Box<dyn Write + Send>);

/// A bidirectional byte channel to one session, splittable into independent,
/// owned read and write halves.
///
/// Implemented by [`LocalTransport`] (a Unix socket) and by the relay's
/// per-stream handle (`RelayStream`). A caller rarely names this type: it opens
/// a [`Connection`](crate::Connection) with [`Connection::open`](crate::Connection::open)
/// (local) or from a relay stream, both of which construct the transport.
pub trait Transport: Send {
    /// Consume the transport, yielding its read and write halves. The two must
    /// be usable from different threads concurrently (the reader thread reads
    /// while request methods write).
    fn split(self: Box<Self>) -> Result<TransportHalves, ClientError>;
}

/// The local transport: a Unix-domain socket to a session on this machine
/// (`~/.shelbi/sessions/<short-id>/sock`).
pub struct LocalTransport(pub UnixStream);

impl Transport for LocalTransport {
    fn split(self: Box<Self>) -> Result<TransportHalves, ClientError> {
        // Two handles on the one socket: the kernel lets the reader block on
        // `read` while the writer sends on the clone, with no interleave because
        // whole frames are written under the connection's write lock.
        let read = self.0;
        let write = read.try_clone()?;
        Ok((Box::new(read), Box::new(write)))
    }
}
