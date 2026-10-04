//! Additive-capability frames — the **non-frozen** wire types.
//!
//! NOT FROZEN. Everything here is additive: a capability is announced in the
//! [`Hello`](crate::Hello) and used by a peer only when the other end advertised
//! it (see [`crate::capability`]). These frame kinds take type bytes at or above
//! [`FrameType::CAPABILITY_BASE`](crate::FrameType::CAPABILITY_BASE), so a
//! frozen-core-only peer treats them as [`ProtoError::UnknownFrameType`] and
//! never misreads one. The names, byte values, and payloads here may change in
//! lockstep with the client/daemon version; none of it is part of the
//! compatibility guarantee the frozen core gives.
//!
//! ## Framing
//!
//! Identical length-prefix shape to the frozen core
//! (`[len: u32 BE][type: u8][payload]`), so one [`decode_any`] can read either a
//! core [`Frame`](crate::Frame) or an [`ExtFrame`] off the same stream. Every
//! ext payload is JSON (none are high-volume), which keeps the module small and
//! the wire self-describing.

use serde::{Deserialize, Serialize};

use crate::error::ProtoError;
use crate::frame::{Frame, FrameType};

const LEN_PREFIX: usize = 4;

/// Type bytes for the additive frames. All at or above
/// [`FrameType::CAPABILITY_BASE`]; a frozen-core peer rejects them as unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExtType {
    /// [`Info`] request.
    Info = 0x80,
    /// [`InfoData`] reply.
    InfoData = 0x81,
    /// [`Paste`] — text with bracketed paste when the program enabled it.
    Paste = 0x82,
    /// [`SetMeta`] — update `meta.json`.
    SetMeta = 0x83,
    /// `detach` — explicit unsubscribe (no payload).
    Detach = 0x84,
    /// [`Resized`] — in-band resize marker riding the output stream (sequenced).
    Resized = 0x85,
    /// [`EventTitle`] — pushed: the title changed.
    EventTitle = 0x86,
    /// `event-bell` — pushed: the bell rang (no payload).
    EventBell = 0x87,
    /// [`EventResized`] — pushed, out-of-band: the size changed.
    EventResized = 0x88,
    /// [`Resync`] — backpressure recovery: a fresh snapshot and the sequence it
    /// resumes from.
    Resync = 0x89,
    /// `ping` — keepalive probe (no payload).
    Ping = 0x8a,
    /// `pong` — keepalive reply (no payload).
    Pong = 0x8b,
}

impl ExtType {
    /// Map a wire byte to an ext type, or `None` if it is not one.
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x80 => Self::Info,
            0x81 => Self::InfoData,
            0x82 => Self::Paste,
            0x83 => Self::SetMeta,
            0x84 => Self::Detach,
            0x85 => Self::Resized,
            0x86 => Self::EventTitle,
            0x87 => Self::EventBell,
            0x88 => Self::EventResized,
            0x89 => Self::Resync,
            0x8a => Self::Ping,
            0x8b => Self::Pong,
            _ => return None,
        })
    }
}

/// Request current session facts: title, size, mode flags, metadata, child
/// state. The reply is [`InfoData`]. Reserved for future fields; empty today.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {}

/// Reply to [`Info`]: everything a client needs to render chrome without reading
/// the output stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InfoData {
    /// The window title the program last set, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Current PTY width in columns.
    pub cols: u16,
    /// Current PTY height in rows.
    pub rows: u16,
    /// Whether the program is on the alternate screen.
    pub alt_screen: bool,
    /// Whether bracketed paste is enabled.
    pub bracketed_paste: bool,
    /// Active Kitty keyboard-protocol flags (0 if none).
    pub kitty_flags: u8,
    /// The readable session name (`meta.json`).
    pub name: String,
    /// The task id this session serves, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// The child command line.
    pub argv: Vec<String>,
    /// The child's working directory.
    pub cwd: String,
    /// Whether the child is still running (always true while a session serves;
    /// a client learns of exit from the [`Exited`](crate::Exited) event).
    pub child_running: bool,
}

/// Deliver `text` as a paste. The session wraps it in bracketed-paste markers
/// when the program enabled that mode, and otherwise writes it raw; either way
/// it is written to the PTY whole, arbitrated against other input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paste {
    /// The text to paste.
    pub text: String,
}

/// Update a session's `meta.json`. A `None` field is left unchanged. Setting
/// `task` to `Some("")` clears the task.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetMeta {
    /// New readable name, or `None` to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// New task id (`Some("")` clears it), or `None` to leave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
}

/// In-band resize marker: travels in the **same ordered stream** as
/// [`Output`](crate::Output), carrying its own sequence number, so every client
/// emulator reflows at exactly the same point in the byte stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resized {
    /// Monotonic sequence number, from the same counter as [`Output`](crate::Output).
    pub seq: u64,
    /// New width in columns.
    pub cols: u16,
    /// New height in rows.
    pub rows: u16,
}

/// Pushed event: the program set a new window title.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTitle {
    /// The new title.
    pub title: String,
}

/// Pushed event (out-of-band): the session's size changed. The out-of-band form
/// of [`Resized`], for a client that watches size without reading output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventResized {
    /// New width in columns.
    pub cols: u16,
    /// New height in rows.
    pub rows: u16,
}

/// Attach replay / backpressure recovery. Sent on `attach` (the initial
/// replay) and when a client falls too far behind (its queued output is dropped
/// and it is refreshed). Carries a **regenerated escape-sequence byte stream**
/// that reconstructs the session's full emulator state — both screen buffers,
/// scrollback, saved cursors, scroll region, tab stops, charsets, every mode
/// including the kitty keyboard-protocol stack — plus the sequence number live
/// output resumes from. The client feeds `replay` into a fresh emulator and
/// continues from `seq`; the stream is self-contained (it begins with a full
/// reset), so a lagging client need not clear its emulator first.
///
/// Encoded as `[seq: u64 BE][replay bytes]` — **not** JSON — like
/// [`Output`](crate::Output), so a large replay (scrollback can be many KB)
/// does not pay JSON's byte-array blow-up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resync {
    /// The sequence number the next live [`Output`](crate::Output) will carry.
    pub seq: u64,
    /// The regenerated escape-sequence byte stream reconstructing full emulator
    /// state. Fed into a fresh emulator, it ends up identical to the session's.
    pub replay: Vec<u8>,
}

/// A decoded additive-capability frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtFrame {
    /// `info` request.
    Info(Info),
    /// `info` reply.
    InfoData(InfoData),
    /// `paste`.
    Paste(Paste),
    /// `set-meta`.
    SetMeta(SetMeta),
    /// `detach` (no payload).
    Detach,
    /// In-band resize marker.
    Resized(Resized),
    /// Pushed title-changed event.
    EventTitle(EventTitle),
    /// Pushed bell event (no payload).
    EventBell,
    /// Pushed out-of-band resize event.
    EventResized(EventResized),
    /// Backpressure-recovery snapshot.
    Resync(Resync),
    /// Keepalive probe (no payload).
    Ping,
    /// Keepalive reply (no payload).
    Pong,
}

impl ExtFrame {
    /// The wire type byte for this frame.
    pub fn ext_type(&self) -> ExtType {
        match self {
            ExtFrame::Info(_) => ExtType::Info,
            ExtFrame::InfoData(_) => ExtType::InfoData,
            ExtFrame::Paste(_) => ExtType::Paste,
            ExtFrame::SetMeta(_) => ExtType::SetMeta,
            ExtFrame::Detach => ExtType::Detach,
            ExtFrame::Resized(_) => ExtType::Resized,
            ExtFrame::EventTitle(_) => ExtType::EventTitle,
            ExtFrame::EventBell => ExtType::EventBell,
            ExtFrame::EventResized(_) => ExtType::EventResized,
            ExtFrame::Resync(_) => ExtType::Resync,
            ExtFrame::Ping => ExtType::Ping,
            ExtFrame::Pong => ExtType::Pong,
        }
    }

    fn encode_payload(&self) -> Result<Vec<u8>, ProtoError> {
        Ok(match self {
            ExtFrame::Info(m) => serde_json::to_vec(m)?,
            ExtFrame::InfoData(m) => serde_json::to_vec(m)?,
            ExtFrame::Paste(m) => serde_json::to_vec(m)?,
            ExtFrame::SetMeta(m) => serde_json::to_vec(m)?,
            ExtFrame::Resized(m) => serde_json::to_vec(m)?,
            ExtFrame::EventTitle(m) => serde_json::to_vec(m)?,
            ExtFrame::EventResized(m) => serde_json::to_vec(m)?,
            // Binary, not JSON: `[seq: u64 BE][replay bytes]` (see [`Resync`]).
            ExtFrame::Resync(m) => {
                let mut buf = Vec::with_capacity(8 + m.replay.len());
                buf.extend_from_slice(&m.seq.to_be_bytes());
                buf.extend_from_slice(&m.replay);
                buf
            }
            // Payload-less frames carry an empty JSON object so the body is never
            // zero-length (a zero body is rejected by the decoder).
            ExtFrame::Detach | ExtFrame::EventBell | ExtFrame::Ping | ExtFrame::Pong => {
                b"{}".to_vec()
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
        out.push(self.ext_type() as u8);
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Decode a single ext frame from the front of `buf`. The type byte must be
    /// an ext byte; a core or unknown byte is [`ProtoError::UnknownFrameType`].
    pub fn decode(buf: &[u8]) -> Result<(ExtFrame, usize), ProtoError> {
        if buf.len() < LEN_PREFIX {
            return Err(ProtoError::Incomplete {
                needed: Some(LEN_PREFIX - buf.len()),
            });
        }
        let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if body_len == 0 {
            return Err(ProtoError::MalformedPayload { kind: "frame" });
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
        let ty = ExtType::from_u8(type_byte).ok_or(ProtoError::UnknownFrameType(type_byte))?;
        let frame = match ty {
            ExtType::Info => ExtFrame::Info(serde_json::from_slice(payload)?),
            ExtType::InfoData => ExtFrame::InfoData(serde_json::from_slice(payload)?),
            ExtType::Paste => ExtFrame::Paste(serde_json::from_slice(payload)?),
            ExtType::SetMeta => ExtFrame::SetMeta(serde_json::from_slice(payload)?),
            ExtType::Detach => ExtFrame::Detach,
            ExtType::Resized => ExtFrame::Resized(serde_json::from_slice(payload)?),
            ExtType::EventTitle => ExtFrame::EventTitle(serde_json::from_slice(payload)?),
            ExtType::EventBell => ExtFrame::EventBell,
            ExtType::EventResized => ExtFrame::EventResized(serde_json::from_slice(payload)?),
            ExtType::Resync => {
                if payload.len() < 8 {
                    return Err(ProtoError::MalformedPayload { kind: "resync" });
                }
                let seq = u64::from_be_bytes(payload[..8].try_into().unwrap());
                ExtFrame::Resync(Resync {
                    seq,
                    replay: payload[8..].to_vec(),
                })
            }
            ExtType::Ping => ExtFrame::Ping,
            ExtType::Pong => ExtFrame::Pong,
        };
        Ok((frame, total))
    }
}

/// Either a frozen-core [`Frame`] or an additive [`ExtFrame`]. What a peer
/// reading the shared stream decodes from each frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnyFrame {
    /// A frozen-core frame.
    Core(Frame),
    /// An additive-capability frame.
    Ext(ExtFrame),
}

/// Decode one frame of either kind from the front of `buf`, dispatching on the
/// type byte: below [`FrameType::CAPABILITY_BASE`] it is a core frame, at or
/// above it an ext frame. Returns the frame and bytes consumed, or
/// [`ProtoError::Incomplete`] when the buffer does not yet hold a whole frame.
pub fn decode_any(buf: &[u8]) -> Result<(AnyFrame, usize), ProtoError> {
    if buf.len() < LEN_PREFIX {
        return Err(ProtoError::Incomplete {
            needed: Some(LEN_PREFIX - buf.len()),
        });
    }
    let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if body_len == 0 {
        return Err(ProtoError::MalformedPayload { kind: "frame" });
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
    if type_byte < FrameType::CAPABILITY_BASE {
        let (f, n) = Frame::decode(buf)?;
        Ok((AnyFrame::Core(f), n))
    } else {
        let (e, n) = ExtFrame::decode(buf)?;
        Ok((AnyFrame::Ext(e), n))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(frame: ExtFrame) {
        let bytes = frame.encode().expect("encode");
        let (decoded, consumed) = ExtFrame::decode(&bytes).expect("decode");
        assert_eq!(decoded, frame);
        assert_eq!(consumed, bytes.len());
        // And through the unified decoder.
        let (any, n) = decode_any(&bytes).expect("decode_any");
        assert_eq!(any, AnyFrame::Ext(frame));
        assert_eq!(n, bytes.len());
    }

    #[test]
    fn every_ext_frame_round_trips() {
        roundtrip(ExtFrame::Info(Info::default()));
        roundtrip(ExtFrame::InfoData(InfoData {
            title: Some("agent".into()),
            cols: 120,
            rows: 40,
            alt_screen: true,
            bracketed_paste: true,
            kitty_flags: 1,
            name: "demo/ws/alpha".into(),
            task: Some("fix-login".into()),
            argv: vec!["claude".into()],
            cwd: "/tmp/wt".into(),
            child_running: true,
        }));
        roundtrip(ExtFrame::Paste(Paste {
            text: "hello\nworld".into(),
        }));
        roundtrip(ExtFrame::SetMeta(SetMeta {
            name: Some("demo/ws/beta".into()),
            task: Some(String::new()),
        }));
        roundtrip(ExtFrame::Detach);
        roundtrip(ExtFrame::Resized(Resized {
            seq: 99,
            cols: 100,
            rows: 30,
        }));
        roundtrip(ExtFrame::EventTitle(EventTitle {
            title: "t".into(),
        }));
        roundtrip(ExtFrame::EventBell);
        roundtrip(ExtFrame::EventResized(EventResized { cols: 80, rows: 24 }));
        roundtrip(ExtFrame::Resync(Resync {
            seq: 7,
            replay: b"\x1bc\x1b[0mhi".to_vec(),
        }));
        // An empty replay still round-trips (the seq prefix is always present).
        roundtrip(ExtFrame::Resync(Resync {
            seq: 0,
            replay: Vec::new(),
        }));
        roundtrip(ExtFrame::Ping);
        roundtrip(ExtFrame::Pong);
    }

    #[test]
    fn decode_any_routes_core_and_ext() {
        let core = Frame::Resize(crate::Resize { cols: 80, rows: 24 })
            .encode()
            .unwrap();
        let (any, _) = decode_any(&core).unwrap();
        assert!(matches!(any, AnyFrame::Core(Frame::Resize(_))));

        let ext = ExtFrame::Ping.encode().unwrap();
        let (any, _) = decode_any(&ext).unwrap();
        assert!(matches!(any, AnyFrame::Ext(ExtFrame::Ping)));
    }

    #[test]
    fn decode_any_reports_bytes_consumed_across_a_mixed_stream() {
        let mut stream = Frame::Input(crate::Input { data: b"x".to_vec() })
            .encode()
            .unwrap();
        stream.extend(ExtFrame::EventBell.encode().unwrap());
        let (first, n1) = decode_any(&stream).unwrap();
        assert!(matches!(first, AnyFrame::Core(Frame::Input(_))));
        let (second, n2) = decode_any(&stream[n1..]).unwrap();
        assert_eq!(second, AnyFrame::Ext(ExtFrame::EventBell));
        assert_eq!(n1 + n2, stream.len());
    }

    #[test]
    fn ext_type_bytes_are_all_in_the_capability_range() {
        for b in 0x80u8..=0x8b {
            let ty = ExtType::from_u8(b).unwrap();
            assert!(ty as u8 >= FrameType::CAPABILITY_BASE);
        }
        assert!(ExtType::from_u8(0x8c).is_none());
    }

    #[test]
    fn incomplete_is_reported_until_the_whole_frame_is_present() {
        let bytes = ExtFrame::Resync(Resync {
            seq: 1,
            replay: b"state".to_vec(),
        })
        .encode()
        .unwrap();
        assert!(matches!(
            ExtFrame::decode(&bytes[..3]),
            Err(ProtoError::Incomplete { .. })
        ));
        assert!(matches!(
            ExtFrame::decode(&bytes[..bytes.len() - 1]),
            Err(ProtoError::Incomplete { .. })
        ));
        assert!(ExtFrame::decode(&bytes).is_ok());
    }
}
