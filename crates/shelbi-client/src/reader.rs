//! The background reader: stream output and events over channels.
//!
//! Output and pushed events arrive continuously, so they are not part of the
//! blocking request API. A plain OS thread (no async runtime, per the crate's
//! runtime-agnostic rule) reads frames from the connection and forwards them:
//!
//! - output, the in-band `resized` marker, and the pushed events (title, bell,
//!   out-of-band resized, resync, exited) go to the caller's
//!   [`SessionEvent`] channel;
//! - request **replies** ([`SnapshotData`], [`InfoData`]) go to a private reply
//!   channel the blocking request methods wait on;
//! - a keepalive `ping` is answered with a `pong` on the shared write half, so
//!   neither the caller nor the request path sees keepalive traffic.
//!
//! The reader bounds nothing itself; the session is the backpressure authority
//! (it drops a lagging client to a [`SessionEvent::Resync`]). The caller's
//! receiver is unbounded, so a consumer that stops draining grows memory — a
//! caller that needs a bound wraps the receiver.

use std::io::{Read, Write};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};

use shelbi_proto::{decode_any, AnyFrame, ExtFrame, Frame};

/// The write half shared between the request path and the reader thread (which
/// writes keepalive pongs). Boxed so it is the same type for a local socket or
/// a relay stream — the transport seam.
pub(crate) type SharedWrite = Arc<Mutex<Box<dyn Write + Send>>>;

/// A request reply routed back to a blocking [`Connection`](crate::connect::Connection)
/// method. Output and events never travel this way.
#[derive(Debug, Clone)]
pub(crate) enum Reply {
    /// Reply to a `snapshot` request.
    Snapshot(shelbi_proto::SnapshotData),
    /// Reply to an `info` request.
    Info(shelbi_proto::InfoData),
}

/// Something the reader delivers to the client: a chunk of output, a pushed
/// event, or the end of the stream.
#[derive(Debug, Clone)]
pub enum SessionEvent {
    /// A chunk of PTY output with its sequence number.
    Output {
        /// Monotonic output sequence number.
        seq: u64,
        /// Raw output bytes.
        data: Vec<u8>,
    },
    /// In-band resize marker: the session's size changed at this point in the
    /// ordered output stream. Clients lock their emulator to this size so every
    /// emulator reflows at the same byte offset.
    Resized {
        /// Sequence number, shared with [`SessionEvent::Output`].
        seq: u64,
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
    },
    /// Attach replay / backpressure recovery: feed `replay` into a fresh
    /// emulator to reconstruct the session's full state, then resume live output
    /// from `seq`. Sent on attach and when the client was dropped for falling
    /// behind. The stream is self-contained (it begins with a full reset), so a
    /// lagging client need not clear its emulator first.
    Resync {
        /// The sequence the next [`SessionEvent::Output`] will carry.
        seq: u64,
        /// The regenerated escape-sequence byte stream reconstructing full
        /// emulator state.
        replay: Vec<u8>,
    },
    /// Pushed event: the program set a new window title (empty = reset).
    Title(String),
    /// Pushed event: the program rang the bell.
    Bell,
    /// Pushed out-of-band event: the size changed. The in-band form is
    /// [`SessionEvent::Resized`]; this is for watching size without reading output.
    SizeChanged {
        /// New width in columns.
        cols: u16,
        /// New height in rows.
        rows: u16,
    },
    /// The child exited; the stream is finished and the reader thread ends.
    Exited(shelbi_proto::Exited),
}

/// Spawn the reader thread. It reads frames off `read_half`, routes events to
/// `events`, replies to `replies`, and answers keepalive pings on `write`.
/// Returns when the stream ends (EOF, error, or after an `exited` event).
pub(crate) fn spawn(
    read_half: Box<dyn crate::transport::ReadTimeout + Send>,
    write: SharedWrite,
    events: Sender<SessionEvent>,
    replies: Sender<Reply>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || run(read_half, write, events, replies))
}

fn run(
    mut read_half: Box<dyn crate::transport::ReadTimeout + Send>,
    write: SharedWrite,
    events: Sender<SessionEvent>,
    replies: Sender<Reply>,
) {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        // Drain whole frames already buffered.
        loop {
            match decode_any(&buf) {
                Ok((frame, consumed)) => {
                    buf.drain(..consumed);
                    if route(frame, &write, &events, &replies).is_break() {
                        return;
                    }
                }
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => break,
                // A malformed/unknown frame: stop rather than desync the stream.
                Err(_) => return,
            }
        }
        match read_half.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

/// Route one decoded frame. Returns `Break` when the stream is finished (the
/// child exited or a channel receiver was dropped).
fn route(
    frame: AnyFrame,
    write: &SharedWrite,
    events: &Sender<SessionEvent>,
    replies: &Sender<Reply>,
) -> std::ops::ControlFlow<()> {
    use std::ops::ControlFlow::{Break, Continue};
    match frame {
        AnyFrame::Core(Frame::Output(o)) => {
            if events
                .send(SessionEvent::Output {
                    seq: o.seq,
                    data: o.data,
                })
                .is_err()
            {
                return Break(());
            }
        }
        AnyFrame::Core(Frame::Exited(e)) => {
            let _ = events.send(SessionEvent::Exited(e));
            return Break(());
        }
        AnyFrame::Core(Frame::SnapshotData(s)) => {
            if replies.send(Reply::Snapshot(s)).is_err() {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::Resized(r)) => {
            if events
                .send(SessionEvent::Resized {
                    seq: r.seq,
                    cols: r.cols,
                    rows: r.rows,
                })
                .is_err()
            {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::Resync(r)) => {
            if events
                .send(SessionEvent::Resync {
                    seq: r.seq,
                    replay: r.replay,
                })
                .is_err()
            {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::EventTitle(t)) => {
            if events.send(SessionEvent::Title(t.title)).is_err() {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::EventBell) => {
            if events.send(SessionEvent::Bell).is_err() {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::EventResized(r)) => {
            if events
                .send(SessionEvent::SizeChanged {
                    cols: r.cols,
                    rows: r.rows,
                })
                .is_err()
            {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::InfoData(i)) => {
            if replies.send(Reply::Info(i)).is_err() {
                return Break(());
            }
        }
        AnyFrame::Ext(ExtFrame::Ping) => {
            // Keepalive: answer with a pong on the shared write half (whole-frame
            // write under the same lock request writes take, so no interleave).
            if let Ok(bytes) = ExtFrame::Pong.encode() {
                if let Ok(mut w) = write.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
        }
        // Hello (post-handshake), Pong, our own request frames echoed back, and
        // anything else a session should not push: ignore.
        _ => {}
    }
    Continue(())
}
