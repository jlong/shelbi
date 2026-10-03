//! # shelbi-term
//!
//! Everything about *showing* a session that is not tied to a UI toolkit: the
//! client-side terminal emulator, scrollback navigation, selection, search, and
//! input encoding. Both the ratatui TUI and the future gpui desktop app render
//! over this crate, so it holds **no** ratatui, crossterm, or gpui types. See
//! the "Removing tmux" plan, "Shared client crates".
//!
//! ## No emulator dependency yet
//!
//! This crate deliberately pulls in **no terminal-emulator crate**. The choice
//! between a vendored `alacritty_terminal` and a `vt100`-family crate is a
//! Phase 0 spike decision (`rt-spike-emulator-replay`), made on which one gives
//! full access to both screen buffers, saved cursors, modes, and history, since
//! replay serializes all of it. [`emulator`] defines the seam the chosen crate
//! sits behind; `rt-term` wires it up.
//!
//! ## What lives here vs. the client
//!
//! [`shelbi_client`] moves frames; this crate interprets them for display. It
//! owns a *client-side* emulator fed by the output stream, and the input
//! encoding that turns key, mouse, paste, and focus events into the bytes an
//! [`Input`](shelbi_proto::Input) frame carries. Replies the client emulator
//! generates to terminal queries are discarded — only the session answers
//! queries.
//!
//! ## Modules
//!
//! - [`emulator`]: the client-side emulator seam (fed by output, queried for
//!   the grid to render).
//! - [`scrollback`]: scrollback navigation for sessions on the normal screen.
//! - [`selection`]: text selection and the copy model (OSC 52, native
//!   fallback).
//! - [`search`]: search within scrollback.
//! - [`input`]: encode keys, mouse, paste, and focus into PTY bytes.
//!
//! [`shelbi_client`]: https://docs.rs/shelbi-client

pub mod emulator;
pub mod input;
pub mod scrollback;
pub mod search;
pub mod selection;
