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
//! ## TODO (owned by `rt-protocol-client`)
//!
//! The following are planned additive capabilities from the plan's protocol
//! table. They are listed here as name constants so call sites have a single
//! source of truth, but their **frames and payloads are not yet defined** —
//! that is the protocol subtask's work. Define each frame with a type byte at
//! or above [`FrameType::CAPABILITY_BASE`](crate::FrameType::CAPABILITY_BASE)
//! and a typed message, mirroring the frozen core's structure.
//!
//! - [`INFO`]: `info` request/reply — title, size, mode flags, metadata, child
//!   state.
//! - [`PASTE`]: `paste` — deliver text with bracketed paste when the program
//!   enabled it (distinct from raw [`Input`](crate::Input)).
//! - [`SET_META`]: `set-meta` — update a session's metadata (`meta.json`).
//! - [`DETACH`]: `detach` — explicit unsubscribe (the core only has implicit
//!   detach on disconnect).
//! - [`EVENT_TITLE`], [`EVENT_BELL`], [`EVENT_RESIZED`]: pushed events for a
//!   title change, a bell, and a size change. (`resized` also rides the output
//!   stream inline per the plan; the pushed event is the out-of-band form for
//!   clients that want it without reading output.)

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

/// Pushed event: the session's size changed.
pub const EVENT_RESIZED: &str = "event-resized";

/// Every additive capability name this build knows about, for use in a
/// [`Hello`](crate::Hello)'s capability list. NOT FROZEN: entries are added as
/// the protocol subtask defines each frame.
pub const ALL: &[&str] = &[
    INFO,
    PASTE,
    SET_META,
    DETACH,
    EVENT_TITLE,
    EVENT_BELL,
    EVENT_RESIZED,
];
