//! # shelbi-proto
//!
//! Frame and message types for the **session protocol** that Shelbi speaks
//! between a client (TUI, `shelbi attach`, desktop app, CLI) and a
//! `shelbi __session` process (one PTY + one headless terminal emulator per
//! session). See the "Removing tmux" plan, sections "The protocol" and
//! "Shared client crates".
//!
//! This crate has **no I/O**. It defines the wire framing and the typed
//! control messages, and provides pure [`Frame::encode`] / [`Frame::decode`]
//! that operate on byte buffers. Reading frames off a socket, spawning
//! sessions, and connecting live in [`shelbi-client`]; everything about
//! *rendering* a session lives in [`shelbi-term`].
//!
//! ## Compatibility policy
//!
//! A session keeps the binary it started with, so newer clients, relays, and
//! daemons must keep controlling sessions that may be weeks old. The protocol
//! is therefore split in two:
//!
//! - A **frozen core** ([`frame`], [`message`]): hello, attach-with-replay,
//!   output, input, resize, snapshot, kill, and the exited event. These frames
//!   never change meaning and are never removed. There is no "session too old"
//!   state: an old session is always attachable and killable.
//! - **Additive capabilities** ([`capability`]): announced in the hello and
//!   used by a client only when the session advertised them. Everything beyond
//!   the core is additive. These definitions are **not frozen** and are owned
//!   by the protocol subtask (`rt-protocol-client`).
//!
//! A CI test runs the current client against session binaries built from each
//! previous release to keep the frozen core honest.
//!
//! [`shelbi-client`]: https://docs.rs/shelbi-client
//! [`shelbi-term`]: https://docs.rs/shelbi-term

pub mod capability;
pub mod control;
pub mod error;
pub mod ext;
pub mod frame;
pub mod message;

pub use error::ProtoError;
pub use ext::{
    decode_any, AnyFrame, EventResized, EventTitle, ExtFrame, ExtType, Info, InfoData, Paste,
    Resized, Resync, SetMeta,
};
pub use frame::{Frame, FrameType, MAX_FRAME_LEN};
pub use message::{
    Attach, ClientColors, Exited, Hello, Input, Kill, Output, Resize, Rgb, Snapshot, SnapshotData,
};

/// Version of the **frozen core** this build speaks.
///
/// Carried in every [`Hello`]. A mismatch is *detected* here; it is resolved by
/// the compatibility policy above (frozen core + additive capabilities), not by
/// refusing the connection. Bumping this is a deliberate, breaking act and
/// should be rare.
pub const PROTOCOL_VERSION: u16 = 1;
