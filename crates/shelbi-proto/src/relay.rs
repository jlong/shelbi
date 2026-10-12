//! The **relay envelope** — the multiplexing layer for remote machines.
//!
//! NOT FROZEN, and wholly separate from the session protocol. A remote machine
//! runs one `shelbi relay` process (started by the hub over `ssh <host> shelbi
//! relay`) that bridges a **single stdio channel** to every session socket on
//! that host — one channel and not one per session, because sshd allows ten
//! sessions per multiplexed connection by default. Frames on that channel carry
//! a `stream` identifier so many logical connections share the one pipe; the
//! session-protocol frames ([`crate::Frame`] / [`crate::ExtFrame`]) ride inside
//! [`RelayFrame::Data`] **unchanged**, so the relay never has to understand
//! them and a current relay speaks the frozen core to sessions that may be
//! weeks old.
//!
//! The relay holds nothing — no PTYs, no session state. If it or the SSH
//! connection dies, the hub starts another and each client reattaches by
//! sequence number (the session, which never restarted, keeps a continuous
//! sequence counter). The channel carries its own [`RelayFrame::Ping`] /
//! [`RelayFrame::Pong`] keepalive so a dead connection is noticed quickly and
//! turned into "unreachable", never "dead".
//!
//! ## Framing
//!
//! Same length-prefix shape as the session protocol, `[len: u32 BE][type:
//! u8][payload]`, with its own one-byte type space (this is a distinct frame
//! family carried on a distinct channel, so there is no overlap with
//! [`crate::FrameType`]). Control frames carry JSON; the one high-volume frame,
//! [`RelayFrame::Data`], carries `[stream: u32 BE][session-frame bytes]` raw.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;

const LEN_PREFIX: usize = 4;

/// Version of the relay channel protocol this build speaks, exchanged in the
/// [`RelayFrame::Hello`]. The relay is always started from the installed
/// binary, so it is current; the hello lets the hub log a mismatch for
/// diagnostics. Not a frozen core.
pub const RELAY_PROTOCOL_VERSION: u16 = 1;

/// The one-byte type tag leading each relay frame's payload. Its own space,
/// unrelated to [`crate::FrameType`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RelayType {
    /// [`RelayFrame::Hello`] — JSON.
    Hello = 0x01,
    /// [`RelayFrame::ListSessions`] — discovery request (empty payload).
    ListSessions = 0x02,
    /// [`RelayFrame::SessionList`] — discovery reply (JSON).
    SessionList = 0x03,
    /// [`RelayFrame::Open`] — open a logical stream to a session (JSON).
    Open = 0x04,
    /// [`RelayFrame::Opened`] — stream opened (JSON).
    Opened = 0x05,
    /// [`RelayFrame::OpenError`] — open failed (JSON).
    OpenError = 0x06,
    /// [`RelayFrame::Data`] — multiplexed session bytes: `[stream BE][bytes]`.
    Data = 0x07,
    /// [`RelayFrame::Close`] — tear a logical stream down (JSON).
    Close = 0x08,
    /// [`RelayFrame::Ping`] — channel keepalive probe (empty payload).
    Ping = 0x09,
    /// [`RelayFrame::Pong`] — channel keepalive reply (empty payload).
    Pong = 0x0a,
}

impl RelayType {
    /// Map a wire byte to a relay type, or `None` if it is not one.
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x01 => Self::Hello,
            0x02 => Self::ListSessions,
            0x03 => Self::SessionList,
            0x04 => Self::Open,
            0x05 => Self::Opened,
            0x06 => Self::OpenError,
            0x07 => Self::Data,
            0x08 => Self::Close,
            0x09 => Self::Ping,
            0x0a => Self::Pong,
            _ => return None,
        })
    }
}

/// One session the relay found on its machine, reported in a [`SessionList`].
/// This is the relay's projection of a discovered session directory; it carries
/// what the hub needs to decide what to [`open`](RelayFrame::Open) without a
/// second round trip.
///
/// [`SessionList`]: RelayFrame::SessionList
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelaySession {
    /// Short directory id (the hash under `~/.shelbi/sessions/`) — the handle to
    /// pass back in [`RelayFrame::Open`].
    pub short_id: String,
    /// Readable session name from `meta.json`.
    pub name: String,
    /// Task id this session serves, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The child command line.
    pub argv: Vec<String>,
    /// The child's working directory.
    pub cwd: String,
    /// Whether the session's lifetime lock is held (i.e. it is live).
    pub alive: bool,
    /// The frozen-core protocol version the session speaks.
    pub protocol_version: u16,
}

/// A decoded relay-channel frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayFrame {
    /// Protocol hello, sent by both ends first. Diagnostic only.
    Hello {
        /// The sender's [`RELAY_PROTOCOL_VERSION`].
        version: u16,
    },
    /// Ask the relay to enumerate the sessions on its machine.
    ListSessions,
    /// The relay's answer to [`ListSessions`](RelayFrame::ListSessions).
    SessionList {
        /// Every session the relay found.
        sessions: Vec<RelaySession>,
    },
    /// Open a logical stream `stream` bridged to session `short_id`.
    Open {
        /// Hub-assigned stream id, unique for the channel's lifetime.
        stream: u32,
        /// The target session's short directory id.
        short_id: String,
    },
    /// The relay connected the session socket for `stream`.
    Opened {
        /// The stream that is now open.
        stream: u32,
    },
    /// The relay could not open `stream` (no such session, dead socket, …).
    OpenError {
        /// The stream that failed to open.
        stream: u32,
        /// A short human-readable reason.
        error: String,
    },
    /// Multiplexed session bytes for one logical stream, in either direction.
    /// From the relay these are whole session frames (so a hub-side drop stays
    /// frame-aligned); toward the relay they are whatever the client wrote.
    Data {
        /// The logical stream these bytes belong to.
        stream: u32,
        /// Raw session-protocol bytes, forwarded unchanged.
        bytes: Vec<u8>,
    },
    /// Tear down a logical stream (the session socket is closed on the far end).
    Close {
        /// The stream to close.
        stream: u32,
    },
    /// Channel keepalive probe.
    Ping,
    /// Channel keepalive reply.
    Pong,
}

// JSON bodies for the control variants. Kept private; the public surface is the
// `RelayFrame` enum.
#[derive(Serialize, Deserialize)]
struct HelloBody {
    version: u16,
}
#[derive(Serialize, Deserialize)]
struct SessionListBody {
    sessions: Vec<RelaySession>,
}
#[derive(Serialize, Deserialize)]
struct OpenBody {
    stream: u32,
    short_id: String,
}
#[derive(Serialize, Deserialize)]
struct StreamBody {
    stream: u32,
}
#[derive(Serialize, Deserialize)]
struct OpenErrorBody {
    stream: u32,
    error: String,
}

impl RelayFrame {
    /// The wire type byte for this frame.
    pub fn relay_type(&self) -> RelayType {
        match self {
            RelayFrame::Hello { .. } => RelayType::Hello,
            RelayFrame::ListSessions => RelayType::ListSessions,
            RelayFrame::SessionList { .. } => RelayType::SessionList,
            RelayFrame::Open { .. } => RelayType::Open,
            RelayFrame::Opened { .. } => RelayType::Opened,
            RelayFrame::OpenError { .. } => RelayType::OpenError,
            RelayFrame::Data { .. } => RelayType::Data,
            RelayFrame::Close { .. } => RelayType::Close,
            RelayFrame::Ping => RelayType::Ping,
            RelayFrame::Pong => RelayType::Pong,
        }
    }

    fn encode_payload(&self) -> Result<Vec<u8>, ProtoError> {
        Ok(match self {
            RelayFrame::Hello { version } => serde_json::to_vec(&HelloBody { version: *version })?,
            RelayFrame::ListSessions | RelayFrame::Ping | RelayFrame::Pong => Vec::new(),
            RelayFrame::SessionList { sessions } => serde_json::to_vec(&SessionListBody {
                sessions: sessions.clone(),
            })?,
            RelayFrame::Open { stream, short_id } => serde_json::to_vec(&OpenBody {
                stream: *stream,
                short_id: short_id.clone(),
            })?,
            RelayFrame::Opened { stream } => serde_json::to_vec(&StreamBody { stream: *stream })?,
            RelayFrame::OpenError { stream, error } => serde_json::to_vec(&OpenErrorBody {
                stream: *stream,
                error: error.clone(),
            })?,
            RelayFrame::Close { stream } => serde_json::to_vec(&StreamBody { stream: *stream })?,
            RelayFrame::Data { stream, bytes } => {
                let mut buf = Vec::with_capacity(4 + bytes.len());
                buf.extend_from_slice(&stream.to_be_bytes());
                buf.extend_from_slice(bytes);
                buf
            }
        })
    }

    /// Encode to full wire bytes (length prefix + type byte + payload).
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let payload = self.encode_payload()?;
        let body_len = 1 + payload.len();
        if body_len > crate::MAX_FRAME_LEN {
            return Err(ProtoError::FrameTooLarge(body_len));
        }
        let mut out = Vec::with_capacity(LEN_PREFIX + body_len);
        out.extend_from_slice(&(body_len as u32).to_be_bytes());
        out.push(self.relay_type() as u8);
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Decode a single relay frame from the front of `buf`, returning it and the
    /// number of bytes consumed. [`ProtoError::Incomplete`] when `buf` does not
    /// yet hold a whole frame.
    pub fn decode(buf: &[u8]) -> Result<(RelayFrame, usize), ProtoError> {
        if buf.len() < LEN_PREFIX {
            return Err(ProtoError::Incomplete {
                needed: Some(LEN_PREFIX - buf.len()),
            });
        }
        let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if body_len == 0 {
            return Err(ProtoError::MalformedPayload { kind: "relay" });
        }
        if body_len > crate::MAX_FRAME_LEN {
            return Err(ProtoError::FrameTooLarge(body_len));
        }
        let total = LEN_PREFIX + body_len;
        if buf.len() < total {
            return Err(ProtoError::Incomplete {
                needed: Some(total - buf.len()),
            });
        }
        let type_byte = buf[LEN_PREFIX];
        let payload = &buf[LEN_PREFIX + 1..total];
        let ty = RelayType::from_u8(type_byte).ok_or(ProtoError::UnknownFrameType(type_byte))?;
        let frame = match ty {
            RelayType::Hello => {
                let b: HelloBody = serde_json::from_slice(payload)?;
                RelayFrame::Hello { version: b.version }
            }
            RelayType::ListSessions => RelayFrame::ListSessions,
            RelayType::SessionList => {
                let b: SessionListBody = serde_json::from_slice(payload)?;
                RelayFrame::SessionList { sessions: b.sessions }
            }
            RelayType::Open => {
                let b: OpenBody = serde_json::from_slice(payload)?;
                RelayFrame::Open {
                    stream: b.stream,
                    short_id: b.short_id,
                }
            }
            RelayType::Opened => {
                let b: StreamBody = serde_json::from_slice(payload)?;
                RelayFrame::Opened { stream: b.stream }
            }
            RelayType::OpenError => {
                let b: OpenErrorBody = serde_json::from_slice(payload)?;
                RelayFrame::OpenError {
                    stream: b.stream,
                    error: b.error,
                }
            }
            RelayType::Data => {
                if payload.len() < 4 {
                    return Err(ProtoError::MalformedPayload { kind: "relay-data" });
                }
                let stream = u32::from_be_bytes(payload[..4].try_into().unwrap());
                RelayFrame::Data {
                    stream,
                    bytes: payload[4..].to_vec(),
                }
            }
            RelayType::Close => {
                let b: StreamBody = serde_json::from_slice(payload)?;
                RelayFrame::Close { stream: b.stream }
            }
            RelayType::Ping => RelayFrame::Ping,
            RelayType::Pong => RelayFrame::Pong,
        };
        Ok((frame, total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(frame: RelayFrame) {
        let bytes = frame.encode().expect("encode");
        let (decoded, consumed) = RelayFrame::decode(&bytes).expect("decode");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, bytes.len());
    }

    #[test]
    fn every_relay_frame_round_trips() {
        roundtrip(RelayFrame::Hello {
            version: RELAY_PROTOCOL_VERSION,
        });
        roundtrip(RelayFrame::ListSessions);
        roundtrip(RelayFrame::SessionList {
            sessions: vec![
                RelaySession {
                    short_id: "abc123".into(),
                    name: "demo/ws/alpha".into(),
                    task: Some("t-1".into()),
                    argv: vec!["claude".into()],
                    cwd: "/tmp/wt".into(),
                    alive: true,
                    protocol_version: 1,
                },
                RelaySession {
                    short_id: "def456".into(),
                    name: "demo/orch".into(),
                    task: None,
                    argv: vec!["/bin/sh".into()],
                    cwd: "/".into(),
                    alive: false,
                    protocol_version: 1,
                },
            ],
        });
        roundtrip(RelayFrame::Open {
            stream: 7,
            short_id: "abc123".into(),
        });
        roundtrip(RelayFrame::Opened { stream: 7 });
        roundtrip(RelayFrame::OpenError {
            stream: 9,
            error: "no such session".into(),
        });
        roundtrip(RelayFrame::Data {
            stream: 3,
            bytes: b"\x00\x00\x00\x05\x01{}".to_vec(),
        });
        roundtrip(RelayFrame::Data {
            stream: u32::MAX,
            bytes: vec![],
        });
        roundtrip(RelayFrame::Close { stream: 3 });
        roundtrip(RelayFrame::Ping);
        roundtrip(RelayFrame::Pong);
    }

    #[test]
    fn decode_reports_bytes_consumed_across_a_stream() {
        let a = RelayFrame::Ping;
        let b = RelayFrame::Data {
            stream: 1,
            bytes: b"xyz".to_vec(),
        };
        let mut stream = a.encode().unwrap();
        stream.extend(b.encode().unwrap());
        let (f1, n1) = RelayFrame::decode(&stream).unwrap();
        assert_eq!(f1, a);
        let (f2, n2) = RelayFrame::decode(&stream[n1..]).unwrap();
        assert_eq!(f2, b);
        assert_eq!(n1 + n2, stream.len());
    }

    #[test]
    fn decode_incomplete_until_whole_frame_present() {
        let bytes = RelayFrame::Open {
            stream: 1,
            short_id: "abc".into(),
        }
        .encode()
        .unwrap();
        assert!(matches!(
            RelayFrame::decode(&bytes[..2]),
            Err(ProtoError::Incomplete { .. })
        ));
        assert!(matches!(
            RelayFrame::decode(&bytes[..bytes.len() - 1]),
            Err(ProtoError::Incomplete { .. })
        ));
        assert!(RelayFrame::decode(&bytes).is_ok());
    }

    #[test]
    fn decode_rejects_truncated_data_header() {
        // body_len = 1 (type) + 3 payload bytes, too short for the 4-byte stream.
        let mut buf = 4u32.to_be_bytes().to_vec();
        buf.push(RelayType::Data as u8);
        buf.extend_from_slice(&[1, 2, 3]);
        assert!(matches!(
            RelayFrame::decode(&buf),
            Err(ProtoError::MalformedPayload { kind: "relay-data" })
        ));
    }

    #[test]
    fn decode_rejects_unknown_type() {
        let mut buf = 2u32.to_be_bytes().to_vec();
        buf.push(0x7f);
        buf.push(b'x');
        assert!(matches!(
            RelayFrame::decode(&buf),
            Err(ProtoError::UnknownFrameType(0x7f))
        ));
    }
}
