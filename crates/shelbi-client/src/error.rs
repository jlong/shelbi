//! The client error type.

use thiserror::Error;

/// Errors from discovering, spawning, or talking to a session.
///
/// This enum is a skeleton: variants are added as the Phase 1 subtasks fill in
/// discovery, spawn, connect, and the reader. The [`Protocol`](ClientError::Protocol)
/// and [`Io`](ClientError::Io) variants exist now because every later variant
/// builds on them.
#[derive(Debug, Error)]
pub enum ClientError {
    /// An error framing or parsing a protocol message.
    #[error("protocol error: {0}")]
    Protocol(#[from] shelbi_proto::ProtoError),

    /// An underlying I/O error (socket, spawn, directory scan).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// The peer closed the connection before sending an expected frame.
    #[error("connection closed before a reply was received")]
    UnexpectedEof,

    /// The daemon and client speak incompatible control protocols; relaunch.
    #[error("hub daemon control protocol {daemon} != client {client}; run `shelbi daemon restart`")]
    ControlProtocolMismatch { daemon: u32, client: u32 },

    /// A mutation the daemon ran (or refused) returned this failure. Carries the
    /// daemon's typed [`MutationError`](shelbi_proto::control::MutationError) so
    /// the caller can render it exactly and set the right exit code.
    #[error("{0}")]
    Mutation(shelbi_proto::control::MutationError),
}
