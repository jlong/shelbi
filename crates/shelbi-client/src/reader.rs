//! The background reader: stream output and events over channels.
//!
//! Output and pushed events arrive continuously, so they are not part of the
//! blocking request API. A plain OS thread (no async runtime, per the crate's
//! runtime-agnostic rule) reads frames from the connection and forwards them as
//! [`SessionEvent`]s over a channel the caller owns.
//!
//! Backpressure is the session's concern on the wire (a client that falls
//! behind is dropped back to a fresh replay), but the reader still bounds its
//! own channel so a slow consumer cannot grow memory without limit.
//!
//! TODO (`rt-protocol-client`): implement the reader thread, the channel
//! wiring, and reconnect-by-sequence-number after a transport drop.

/// Something the reader delivers to the client: a chunk of output, a pushed
/// event, or the end of the stream.
///
/// Skeleton: output and the exited event are modeled now because they are
/// frozen core; additive pushed events (title, bell, resized) are added when
/// `rt-protocol-client` defines their frames.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A chunk of PTY output with its sequence number.
    Output {
        /// Monotonic output sequence number.
        seq: u64,
        /// Raw output bytes.
        data: Vec<u8>,
    },
    /// The child exited; the stream is finished.
    Exited(shelbi_proto::Exited),
}
