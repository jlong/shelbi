//! The **frozen** wire framing.
//!
//! FROZEN: the wire shape defined here is part of the protocol's frozen core
//! and must stay bit-compatible across releases. New frame kinds may only be
//! added as additive-capability types using byte values from the reserved
//! range (see [`crate::capability`]); the eight frozen type bytes and the
//! length-prefix layout never change.
//!
//! ## Layout
//!
//! Every frame on the wire is:
//!
//! ```text
//! +----------------+--------+------------------------+
//! | length: u32 BE | type:  | payload: length-1 bytes|
//! | = 1 + payload  | u8     |                        |
//! +----------------+--------+------------------------+
//! ```
//!
//! The length prefix counts the type byte plus the payload, so a reader reads
//! four bytes to learn `N`, then reads `N` more bytes: the first is the type,
//! the rest is the payload.
//!
//! Control frames carry **JSON** payloads (the typed messages in
//! [`crate::message`]). The two high-volume frames carry **raw bytes**:
//! [`Output`] is `[seq: u64 BE][raw bytes]` and [`Input`] is the raw bytes
//! alone.

use crate::error::ProtoError;
use crate::message::{
    Attach, Exited, Hello, Input, Kill, Output, Resize, Snapshot, SnapshotData,
};

/// Hard cap on a single frame's declared length, to bound memory from a
/// hostile or corrupt peer. Output chunks are far smaller in practice.
pub const MAX_FRAME_LEN: usize = 64 * 1024 * 1024;

/// Number of bytes in the length prefix.
const LEN_PREFIX: usize = 4;

/// The one-byte type tag that leads each frame's payload.
///
/// FROZEN: these eight discriminants are permanent. Additive-capability frames
/// (owned by `rt-protocol-client`) take values at or above
/// [`FrameType::CAPABILITY_BASE`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    /// [`Hello`] — control (JSON).
    Hello = 0x01,
    /// [`Attach`] — control (JSON).
    Attach = 0x02,
    /// [`Output`] — raw bytes with a sequence-number prefix.
    Output = 0x03,
    /// [`Input`] — raw bytes.
    Input = 0x04,
    /// [`Resize`] — control (JSON).
    Resize = 0x05,
    /// [`Snapshot`] request — control (JSON).
    Snapshot = 0x06,
    /// [`SnapshotData`] reply — control (JSON).
    SnapshotData = 0x07,
    /// [`Kill`] — control (JSON).
    Kill = 0x08,
    /// [`Exited`] event — control (JSON).
    Exited = 0x09,
}

impl FrameType {
    /// First type byte reserved for additive-capability frames. Frozen-core
    /// types stay strictly below this; the protocol subtask allocates from here
    /// up for `info`, `paste`, `set-meta`, `detach`, and the title/bell/resized
    /// events.
    pub const CAPABILITY_BASE: u8 = 0x80;

    /// Map a wire byte back to a frozen-core type, or `None` if it is not a
    /// known frozen-core type (an unknown or additive-capability byte).
    pub fn from_u8(b: u8) -> Option<Self> {
        Some(match b {
            0x01 => Self::Hello,
            0x02 => Self::Attach,
            0x03 => Self::Output,
            0x04 => Self::Input,
            0x05 => Self::Resize,
            0x06 => Self::Snapshot,
            0x07 => Self::SnapshotData,
            0x08 => Self::Kill,
            0x09 => Self::Exited,
            _ => return None,
        })
    }
}

/// A decoded frozen-core frame.
///
/// Encode one with [`Frame::encode`] (returns the full wire bytes including the
/// length prefix) and decode one with [`Frame::decode`] (reads a single frame
/// from the front of a buffer). Both are pure: this crate performs no I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Frame {
    /// Protocol handshake, sent by both ends.
    Hello(Hello),
    /// Subscribe to output, with replay.
    Attach(Attach),
    /// A chunk of PTY output.
    Output(Output),
    /// Raw bytes for the PTY.
    Input(Input),
    /// A client's viewport size.
    Resize(Resize),
    /// Request a text snapshot of the screen.
    Snapshot(Snapshot),
    /// A text snapshot reply.
    SnapshotData(SnapshotData),
    /// Signal the child's process group.
    Kill(Kill),
    /// The child exited.
    Exited(Exited),
}

impl Frame {
    /// The wire type byte for this frame.
    pub fn frame_type(&self) -> FrameType {
        match self {
            Frame::Hello(_) => FrameType::Hello,
            Frame::Attach(_) => FrameType::Attach,
            Frame::Output(_) => FrameType::Output,
            Frame::Input(_) => FrameType::Input,
            Frame::Resize(_) => FrameType::Resize,
            Frame::Snapshot(_) => FrameType::Snapshot,
            Frame::SnapshotData(_) => FrameType::SnapshotData,
            Frame::Kill(_) => FrameType::Kill,
            Frame::Exited(_) => FrameType::Exited,
        }
    }

    /// Serialize the payload (without the length prefix or type byte).
    fn encode_payload(&self) -> Result<Vec<u8>, ProtoError> {
        Ok(match self {
            Frame::Output(o) => {
                let mut buf = Vec::with_capacity(8 + o.data.len());
                buf.extend_from_slice(&o.seq.to_be_bytes());
                buf.extend_from_slice(&o.data);
                buf
            }
            Frame::Input(i) => i.data.clone(),
            Frame::Hello(m) => serde_json::to_vec(m)?,
            Frame::Attach(m) => serde_json::to_vec(m)?,
            Frame::Resize(m) => serde_json::to_vec(m)?,
            Frame::Snapshot(m) => serde_json::to_vec(m)?,
            Frame::SnapshotData(m) => serde_json::to_vec(m)?,
            Frame::Kill(m) => serde_json::to_vec(m)?,
            Frame::Exited(m) => serde_json::to_vec(m)?,
        })
    }

    /// Encode this frame to its full wire bytes, including the length prefix and
    /// type byte.
    pub fn encode(&self) -> Result<Vec<u8>, ProtoError> {
        let payload = self.encode_payload()?;
        // length prefix covers the type byte plus the payload
        let body_len = 1 + payload.len();
        if body_len > MAX_FRAME_LEN {
            return Err(ProtoError::FrameTooLarge(body_len));
        }
        let mut out = Vec::with_capacity(LEN_PREFIX + body_len);
        out.extend_from_slice(&(body_len as u32).to_be_bytes());
        out.push(self.frame_type() as u8);
        out.extend_from_slice(&payload);
        Ok(out)
    }

    /// Decode a single frame from the front of `buf`.
    ///
    /// On success returns the frame and the number of bytes consumed, so a
    /// caller streaming from a socket can drain its buffer frame by frame.
    /// Returns [`ProtoError::Incomplete`] when `buf` does not yet hold a whole
    /// frame; the caller should read more and retry.
    pub fn decode(buf: &[u8]) -> Result<(Frame, usize), ProtoError> {
        if buf.len() < LEN_PREFIX {
            return Err(ProtoError::Incomplete {
                needed: Some(LEN_PREFIX - buf.len()),
            });
        }
        let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if body_len == 0 {
            // A frame must carry at least its type byte.
            return Err(ProtoError::MalformedPayload { kind: "frame" });
        }
        if body_len > MAX_FRAME_LEN {
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
        let ty = FrameType::from_u8(type_byte).ok_or(ProtoError::UnknownFrameType(type_byte))?;
        let frame = match ty {
            FrameType::Output => {
                if payload.len() < 8 {
                    return Err(ProtoError::MalformedPayload { kind: "output" });
                }
                let seq = u64::from_be_bytes(payload[..8].try_into().unwrap());
                Frame::Output(Output {
                    seq,
                    data: payload[8..].to_vec(),
                })
            }
            FrameType::Input => Frame::Input(Input {
                data: payload.to_vec(),
            }),
            FrameType::Hello => Frame::Hello(serde_json::from_slice(payload)?),
            FrameType::Attach => Frame::Attach(serde_json::from_slice(payload)?),
            FrameType::Resize => Frame::Resize(serde_json::from_slice(payload)?),
            FrameType::Snapshot => Frame::Snapshot(serde_json::from_slice(payload)?),
            FrameType::SnapshotData => Frame::SnapshotData(serde_json::from_slice(payload)?),
            FrameType::Kill => Frame::Kill(serde_json::from_slice(payload)?),
            FrameType::Exited => Frame::Exited(serde_json::from_slice(payload)?),
        };
        Ok((frame, total))
    }
}

/// Total wire length (prefix + body) of the frame at the front of `buf`, read
/// from the length prefix **alone** — without decoding the type byte or the
/// payload.
///
/// A relay forwards whole session frames between a remote session and the hub
/// over a multiplexed channel; it must peel them one at a time so a drop or a
/// re-wrap never splits a frame, yet it must not choke on a frame type it
/// cannot itself decode (an additive-capability frame emitted by a session
/// built after the relay). Frame boundaries are defined by the length prefix,
/// so this gives the relay exactly that — the byte length of the next whole
/// frame — and nothing it would have to understand. Returns
/// [`ProtoError::Incomplete`] until the whole frame is buffered, and rejects a
/// zero body or an over-cap length the same way [`Frame::decode`] does.
pub fn frame_boundary(buf: &[u8]) -> Result<usize, ProtoError> {
    if buf.len() < LEN_PREFIX {
        return Err(ProtoError::Incomplete {
            needed: Some(LEN_PREFIX - buf.len()),
        });
    }
    let body_len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if body_len == 0 {
        return Err(ProtoError::MalformedPayload { kind: "frame" });
    }
    if body_len > MAX_FRAME_LEN {
        return Err(ProtoError::FrameTooLarge(body_len));
    }
    let total = LEN_PREFIX + body_len;
    if buf.len() < total {
        return Err(ProtoError::Incomplete {
            needed: Some(total - buf.len()),
        });
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{ClientColors, Rgb};
    use crate::PROTOCOL_VERSION;

    fn roundtrip(frame: Frame) {
        let bytes = frame.encode().expect("encode");
        let (decoded, consumed) = Frame::decode(&bytes).expect("decode");
        assert_eq!(decoded, frame, "frame survives round trip");
        assert_eq!(consumed, bytes.len(), "consumed the whole frame");
    }

    #[test]
    fn hello_roundtrip() {
        roundtrip(Frame::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            colors: Some(ClientColors {
                foreground: Rgb {
                    r: 0xEE,
                    g: 0xEE,
                    b: 0xEE,
                },
                background: Rgb { r: 0, g: 0, b: 0 },
            }),
            capabilities: vec!["paste".into(), "detach".into()],
        }));
    }

    #[test]
    fn hello_minimal_roundtrip() {
        roundtrip(Frame::Hello(Hello {
            protocol_version: PROTOCOL_VERSION,
            colors: None,
            capabilities: vec![],
        }));
    }

    #[test]
    fn attach_roundtrip() {
        roundtrip(Frame::Attach(Attach { since_seq: None }));
        roundtrip(Frame::Attach(Attach {
            since_seq: Some(42),
        }));
    }

    #[test]
    fn output_roundtrip() {
        roundtrip(Frame::Output(Output {
            seq: 0,
            data: vec![],
        }));
        roundtrip(Frame::Output(Output {
            seq: u64::MAX,
            data: b"\x1b[31mhello\x1b[0m\r\n".to_vec(),
        }));
    }

    #[test]
    fn input_roundtrip() {
        roundtrip(Frame::Input(Input {
            data: b"ls -la\r".to_vec(),
        }));
    }

    #[test]
    fn resize_roundtrip() {
        roundtrip(Frame::Resize(Resize {
            cols: 200,
            rows: 50,
        }));
    }

    #[test]
    fn snapshot_roundtrip() {
        roundtrip(Frame::Snapshot(Snapshot {
            history_lines: None,
        }));
        roundtrip(Frame::Snapshot(Snapshot {
            history_lines: Some(10_000),
        }));
        roundtrip(Frame::SnapshotData(SnapshotData {
            text: "line one\nline two".into(),
        }));
    }

    #[test]
    fn kill_roundtrip() {
        roundtrip(Frame::Kill(Kill { signal: None }));
        roundtrip(Frame::Kill(Kill { signal: Some(15) }));
    }

    #[test]
    fn exited_roundtrip() {
        roundtrip(Frame::Exited(Exited {
            code: Some(0),
            signal: None,
            reason: None,
        }));
        roundtrip(Frame::Exited(Exited {
            code: None,
            signal: Some(9),
            reason: Some("killed".into()),
        }));
    }

    #[test]
    fn decode_reports_bytes_consumed_for_concatenated_frames() {
        let a = Frame::Resize(Resize { cols: 80, rows: 24 });
        let b = Frame::Input(Input {
            data: b"x".to_vec(),
        });
        let mut stream = a.encode().unwrap();
        stream.extend_from_slice(&b.encode().unwrap());

        let (first, n1) = Frame::decode(&stream).unwrap();
        assert_eq!(first, a);
        let (second, n2) = Frame::decode(&stream[n1..]).unwrap();
        assert_eq!(second, b);
        assert_eq!(n1 + n2, stream.len());
    }

    #[test]
    fn decode_incomplete_before_length_prefix() {
        let err = Frame::decode(&[0, 0]).unwrap_err();
        assert!(matches!(err, ProtoError::Incomplete { needed: Some(2) }));
    }

    #[test]
    fn decode_incomplete_before_body() {
        let full = Frame::Resize(Resize { cols: 80, rows: 24 })
            .encode()
            .unwrap();
        let err = Frame::decode(&full[..full.len() - 1]).unwrap_err();
        assert!(matches!(err, ProtoError::Incomplete { needed: Some(1) }));
    }

    #[test]
    fn decode_unknown_frame_type() {
        // A well-formed frame whose type byte is in the additive-capability
        // range decodes as "unknown" to a frozen-core-only peer.
        let payload = b"{}";
        let body_len = 1 + payload.len();
        let mut buf = (body_len as u32).to_be_bytes().to_vec();
        buf.push(FrameType::CAPABILITY_BASE);
        buf.extend_from_slice(payload);
        let err = Frame::decode(&buf).unwrap_err();
        assert!(matches!(err, ProtoError::UnknownFrameType(b) if b == FrameType::CAPABILITY_BASE));
    }

    #[test]
    fn decode_rejects_zero_length() {
        let buf = [0u8, 0, 0, 0];
        assert!(matches!(
            Frame::decode(&buf),
            Err(ProtoError::MalformedPayload { kind: "frame" })
        ));
    }

    #[test]
    fn decode_rejects_truncated_output_header() {
        // body_len = 1 (type) + 3 payload bytes, which is too short for the
        // 8-byte output sequence prefix.
        let mut buf = 4u32.to_be_bytes().to_vec();
        buf.push(FrameType::Output as u8);
        buf.extend_from_slice(&[1, 2, 3]);
        assert!(matches!(
            Frame::decode(&buf),
            Err(ProtoError::MalformedPayload { kind: "output" })
        ));
    }

    #[test]
    fn frame_boundary_measures_whole_frames_without_decoding() {
        // A well-formed core frame: boundary equals its full encoded length.
        let a = Frame::Resize(Resize { cols: 80, rows: 24 }).encode().unwrap();
        assert_eq!(frame_boundary(&a).unwrap(), a.len());

        // An additive-capability byte the core cannot decode is still measurable
        // from the length prefix alone — this is the relay's forwarding path.
        let payload = b"arbitrary";
        let body_len = 1 + payload.len();
        let mut unknown = (body_len as u32).to_be_bytes().to_vec();
        unknown.push(FrameType::CAPABILITY_BASE + 9); // not a known ext byte either
        unknown.extend_from_slice(payload);
        assert_eq!(frame_boundary(&unknown).unwrap(), unknown.len());
        // ...even though a full decode rejects it.
        assert!(Frame::decode(&unknown).is_err());

        // Incomplete until the whole frame is present.
        assert!(matches!(
            frame_boundary(&a[..a.len() - 1]),
            Err(ProtoError::Incomplete { .. })
        ));
        assert!(matches!(frame_boundary(&[0, 0]), Err(ProtoError::Incomplete { .. })));
        // Zero body and over-cap are rejected, matching decode.
        assert!(matches!(
            frame_boundary(&[0, 0, 0, 0]),
            Err(ProtoError::MalformedPayload { kind: "frame" })
        ));
    }

    #[test]
    fn frame_type_round_trips_through_u8() {
        for ty in [
            FrameType::Hello,
            FrameType::Attach,
            FrameType::Output,
            FrameType::Input,
            FrameType::Resize,
            FrameType::Snapshot,
            FrameType::SnapshotData,
            FrameType::Kill,
            FrameType::Exited,
        ] {
            assert_eq!(FrameType::from_u8(ty as u8), Some(ty));
        }
        assert_eq!(FrameType::from_u8(FrameType::CAPABILITY_BASE), None);
    }
}
