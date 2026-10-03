//! Errors raised while encoding or decoding frames.

use thiserror::Error;

/// An error from framing or message (de)serialization.
#[derive(Debug, Error)]
pub enum ProtoError {
    /// The buffer does not yet hold a complete frame. The caller should read
    /// more bytes and retry; this is not a protocol violation. The payload is
    /// the number of additional bytes known to be needed, when that is known
    /// (it is known once the length prefix has been read, `None` before).
    #[error("incomplete frame: need more bytes")]
    Incomplete {
        /// Additional bytes required beyond what the buffer holds, if known.
        needed: Option<usize>,
    },

    /// The one-byte frame type is not a known frozen-core type. (Additive
    /// capability frames use type bytes carved from the reserved range; an
    /// older peer that does not know one treats it as unknown.)
    #[error("unknown frame type: 0x{0:02x}")]
    UnknownFrameType(u8),

    /// A frame's declared length exceeds [`crate::MAX_FRAME_LEN`].
    #[error("frame length {0} exceeds maximum {max}", max = crate::MAX_FRAME_LEN)]
    FrameTooLarge(usize),

    /// A control frame's JSON payload failed to (de)serialize.
    #[error("control payload JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A binary frame's fixed-size header (e.g. the output sequence number) was
    /// truncated.
    #[error("malformed {kind} frame payload")]
    MalformedPayload {
        /// Which frame kind was malformed.
        kind: &'static str,
    },
}
