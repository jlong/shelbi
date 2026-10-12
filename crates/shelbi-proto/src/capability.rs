//! Additive capabilities — the **non-frozen** part of the protocol.
//!
//! NOT FROZEN. Everything in this module is additive: a capability is announced
//! in the [`Hello`](crate::Hello) and a client uses it only when the session
//! advertised it, falling back to the frozen core otherwise. Names and frames
//! here may be added, renamed before they ship, or reshaped by the protocol
//! subtask (`rt-protocol-client`). None of this is part of the compatibility
//! guarantee.
//!
//! ## Why capabilities exist
//!
//! A session keeps the binary it started with, so a current client may talk to
//! a session built from an older release. The frozen core (hello, attach,
//! output, input, resize, snapshot, kill, exited) is always available. Anything
//! newer is gated behind a capability string so an old session simply does not
//! announce it and the client degrades gracefully.
//!
//! ## The capabilities
//!
//! Each name gates one or more [`ExtFrame`](crate::ExtFrame) kinds, defined in
//! [`crate::ext`]. A **request** capability ([`INFO`], [`PASTE`], [`SET_META`],
//! [`DETACH`]) is used by a client only when the *session* announced it, falling
//! back to the frozen core otherwise. A **pushed** capability ([`EVENT_TITLE`],
//! [`EVENT_BELL`], [`EVENT_RESIZED`], [`OUTPUT_RESIZED`], [`RESYNC`]) is sent by
//! the session only to clients that announced understanding it, so an unknown
//! frame never lands on a peer that would reject it. [`KEEPALIVE`] is symmetric:
//! either end may `ping` and the other answers `pong`.
//!
//! - [`INFO`]: `info` request/reply — title, size, mode flags, metadata, child
//!   state.
//! - [`PASTE`]: `paste` — deliver text with bracketed paste when the program
//!   enabled it (distinct from raw [`Input`](crate::Input)).
//! - [`SET_META`]: `set-meta` — update a session's metadata (`meta.json`).
//! - [`DETACH`]: `detach` — explicit unsubscribe (the core only has implicit
//!   detach on disconnect).
//! - [`EVENT_TITLE`], [`EVENT_BELL`], [`EVENT_RESIZED`]: pushed events for a
//!   title change, a bell, and a size change.
//! - [`OUTPUT_RESIZED`]: the in-band `resized(seq, cols, rows)` marker that
//!   rides the output stream so every emulator reflows at the same byte offset;
//!   [`EVENT_RESIZED`] is the out-of-band form for clients that watch size
//!   without reading output.
//! - [`RESYNC`]: backpressure recovery — a dropped-behind client is sent a fresh
//!   snapshot and the sequence to resume from instead of blocking the PTY.
//! - [`KEEPALIVE`]: `ping`/`pong`, so a dead connection is noticed (the relay
//!   depends on it).

/// `info`: request current title, size, mode flags, metadata, and child state.
pub const INFO: &str = "info";

/// `paste`: deliver text using bracketed paste when the program enabled it.
pub const PASTE: &str = "paste";

/// `set-meta`: update a session's metadata.
pub const SET_META: &str = "set-meta";

/// `detach`: explicitly unsubscribe from a session's output.
pub const DETACH: &str = "detach";

/// Pushed event: the session's title changed.
pub const EVENT_TITLE: &str = "event-title";

/// Pushed event: the program rang the bell.
pub const EVENT_BELL: &str = "event-bell";

/// Pushed event: the session's size changed (out-of-band).
pub const EVENT_RESIZED: &str = "event-resized";

/// In-band `resized` markers riding the output stream (sequenced), so client
/// emulators reflow at the same point in the byte stream.
pub const OUTPUT_RESIZED: &str = "output-resized";

/// Backpressure recovery: a lagging client is dropped to a fresh snapshot
/// (`resync`) rather than blocking the PTY or growing a queue without bound.
pub const RESYNC: &str = "resync";

/// Keepalive `ping`/`pong`, so a dead connection is noticed.
pub const KEEPALIVE: &str = "keepalive";

/// Every additive capability name this build knows about, for use in a
/// [`Hello`](crate::Hello)'s capability list. NOT FROZEN.
pub const ALL: &[&str] = &[
    INFO,
    PASTE,
    SET_META,
    DETACH,
    EVENT_TITLE,
    EVENT_BELL,
    EVENT_RESIZED,
    OUTPUT_RESIZED,
    RESYNC,
    KEEPALIVE,
];
