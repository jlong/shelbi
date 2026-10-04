//! # shelbi-term
//!
//! Everything about *showing* a session that is not tied to a UI toolkit: the
//! client-side terminal emulator, scrollback navigation, selection, search, and
//! input encoding. Both the ratatui TUI and the future gpui desktop app render
//! over this crate, so its public API holds **no** ratatui, crossterm, or gpui
//! types. See the "Removing tmux" plan, "Shared client crates".
//!
//! ## What lives here vs. the client
//!
//! [`shelbi_client`] moves frames; this crate interprets them for display. It
//! owns a *client-side* emulator fed by the output stream, and the input
//! encoding that turns key, mouse, paste, and focus events into the bytes an
//! [`Input`](shelbi_proto::Input) frame carries (or the text a
//! [`Paste`](shelbi_proto::Paste) frame carries).
//!
//! The emulator is a vendored `alacritty_terminal` fork driven headless (see
//! [`emulator`]). It is fed replay plus the sequenced output stream, and locked
//! to the session's size — a `resized` frame travels in that same stream so
//! every client reflows at the same byte offset. Until `rt-replay` lands, a
//! client develops against a snapshot plus live output instead of a full replay.
//!
//! **The client emulator never answers terminal queries.** Replies it would
//! generate (cursor-position reports, device attributes, color and text-area
//! queries, clipboard loads) are discarded: only the session process answers
//! queries, because it is the one sitting beside the real PTY. See
//! [`emulator::TermEmulator::discarded_query_replies`].
//!
//! ## Modules
//!
//! - [`emulator`]: the client-side emulator (fed output, queried for the grid
//!   and mode to render).
//! - [`scrollback`]: scrollback navigation for sessions on the normal screen.
//! - [`selection`]: text selection and the OSC 52 copy encoder.
//! - [`search`]: search within scrollback and the visible screen.
//! - [`input`]: encode keys, mouse, paste, and focus into PTY bytes, with the
//!   mouse-ownership policy as a pure function.
//! - [`viewport`]: clipping and letterboxing for viewers whose size differs
//!   from the session's.
//! - [`view`]: [`view::TerminalView`], which ties the emulator to scrollback,
//!   selection, and search and enforces the normal-screen-only rule.
//!
//! [`shelbi_client`]: https://docs.rs/shelbi-client

pub mod emulator;
pub mod input;
pub mod scrollback;
pub mod search;
pub mod selection;
pub mod view;
pub mod viewport;

/// A terminal size in character cells.
///
/// Toolkit-neutral; a viewer converts its own pixel/cell geometry into this
/// before handing it to the emulator or the viewport math.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Size {
    /// Width in columns.
    pub cols: u16,
    /// Height in rows.
    pub rows: u16,
}

impl Size {
    /// A size, clamped so neither dimension is zero (an emulator needs at
    /// least a 1x1 grid).
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols, rows }
    }

    /// This size with each axis forced to at least 1.
    pub(crate) fn non_zero(self) -> Self {
        Self { cols: self.cols.max(1), rows: self.rows.max(1) }
    }
}
