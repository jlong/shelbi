//! Typed control messages of the **frozen core**.
//!
//! FROZEN: the shape and meaning of every type in this module is part of the
//! session protocol's frozen core. Fields may only be **added** as additive
//! capabilities (see [`crate::capability`]) and in a way old peers ignore;
//! existing fields are never removed or repurposed. Changing anything here is a
//! breaking protocol change that bumps [`crate::PROTOCOL_VERSION`].
//!
//! Control messages travel as JSON inside a [`crate::Frame`]. The two
//! high-volume frames, [`Output`] and [`Input`], carry **raw bytes** rather
//! than JSON and are framed directly (see [`crate::frame`]).

use serde::{Deserialize, Serialize};

/// A 24-bit RGB color, used to report a client's terminal colors so the session
/// can answer the agent's color queries (OSC 10/11, DECRQSS) with real values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    /// Red channel.
    pub r: u8,
    /// Green channel.
    pub g: u8,
    /// Blue channel.
    pub b: u8,
}

/// The foreground and background a client is rendering with.
///
/// Reported in the [`Hello`] so that, with no client attached, the session can
/// still answer color queries (it falls back to a dark default before any
/// client has connected). Codex and other agents query these at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientColors {
    /// The client's default foreground color.
    pub foreground: Rgb,
    /// The client's default background color.
    pub background: Rgb,
}

/// First frame on every connection, sent by **both** ends.
///
/// A client sends its [`protocol_version`](Hello::protocol_version), its
/// [`colors`](Hello::colors), and the additive [`capabilities`](Hello::capabilities)
/// it understands. The session replies with its own `Hello` whose
/// `capabilities` are the additive features it **announces**; a client uses an
/// additive capability only if the session announced it, and falls back to the
/// core otherwise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hello {
    /// Frozen-core version the sender speaks ([`crate::PROTOCOL_VERSION`]).
    pub protocol_version: u16,

    /// The sender's terminal colors. Meaningful from a client; `None` from a
    /// session (a session has no colors of its own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<ClientColors>,

    /// Additive capability names the sender understands (from a client) or
    /// announces (from a session). Names are defined in [`crate::capability`].
    /// Unknown names are ignored, which is what lets the set grow without a
    /// version bump.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<String>,
}

/// Subscribe to a session's output stream. The session answers an attach with a
/// **replay first** (a run of [`Output`] frames reconstructing full emulator
/// state) and then live output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attach {
    /// Resume point. `None` requests a full replay from the start of retained
    /// history; `Some(seq)` asks the session to resume right after that output
    /// sequence number, which is how a client reconnects exactly after an SSH
    /// or relay drop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_seq: Option<u64>,
}

/// A chunk of raw PTY output, carrying a monotonically increasing sequence
/// number so reconnect is exact and all emulators reflow at the same point in
/// the byte stream. Encoded as `[seq: u64 BE][raw bytes]`, **not** JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    /// Monotonic sequence number of this output chunk.
    pub seq: u64,
    /// Raw bytes as read from the PTY.
    pub data: Vec<u8>,
}

/// Raw bytes to write to the PTY. Input frames from different clients are
/// written whole and in arrival order, never interleaved mid-frame. Encoded as
/// raw bytes, **not** JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Input {
    /// Raw bytes to deliver to the PTY.
    pub data: Vec<u8>,
}

/// A client's current viewport size. The PTY takes the size of the most
/// recently active client (last to send input), debounced; other viewers clip
/// or letterbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resize {
    /// Viewport width in columns.
    pub cols: u16,
    /// Viewport height in rows.
    pub rows: u16,
}

/// Request the visible screen as text, optionally with history.
///
/// The reply ([`SnapshotData`]) is shaped to match `tmux capture-pane -p -J`
/// (joined wrapped lines, trailing-whitespace handling), because roughly 80
/// detector and baseline tests in `ready.rs` and `submit.rs` are anchored on
/// that shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    /// Number of history (scrollback) lines to include above the visible
    /// screen. `None` means the visible screen only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_lines: Option<u32>,
}

/// Reply to a [`Snapshot`] request: the rendered screen (and optional history)
/// as text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotData {
    /// The captured text, in `capture-pane -p -J` shape.
    pub text: String,
}

/// Signal the child's **process group**. Replaces tmux's kill-pane and reaches
/// the whole group so no descendant is left behind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Kill {
    /// Signal number to send, or `None` for the session's default
    /// (graceful: SIGHUP/SIGTERM semantics are the session's to define).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
}

/// Pushed event: the child process exited. After this the session writes its
/// `exit.json` and `final.txt` and exits itself. This replaces liveness
/// polling and the `--as-pane` wrapper's exit event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Exited {
    /// Exit status code, if the child exited normally.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<i32>,
    /// Terminating signal number, if the child was killed by a signal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// A short human-readable reason, when the session has one to offer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
