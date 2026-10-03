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
}
