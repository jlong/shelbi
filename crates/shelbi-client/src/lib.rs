//! # shelbi-client
//!
//! The client side of the session protocol: discover sessions on disk, spawn
//! new `shelbi __session` processes, connect to a session's socket, make
//! blocking requests, and receive output and events over channels. Every
//! Shelbi UI — the TUI, `shelbi attach`, the desktop app, and the plain CLI —
//! drives sessions through this crate. See the "Removing tmux" plan, "Shared
//! client crates".
//!
//! ## Runtime-agnostic by design
//!
//! This crate has **no tokio** and pulls in no async runtime. The request API
//! is blocking and the output/event path is a plain OS thread that delivers
//! over channels. That is a deliberate constraint: gpui (the future desktop
//! app) runs its own event loop and cannot host tokio, so the shared client
//! must not impose one. A caller that wants async wraps the blocking API itself.
//!
//! ## Transport
//!
//! Local sessions are reached over a Unix socket under
//! `~/.shelbi/sessions/<short-id>/sock`. Remote sessions are reached through
//! one `ssh <host> shelbi relay` per machine that bridges a single stdio
//! channel to every session socket on that host. Both speak the same
//! [`shelbi_proto`] frames, so the transport is swappable beneath the request
//! API. The remote transport is Phase 5 work (`rt-relay`, `rt-remote-spawn`);
//! this crate defines the seam.
//!
//! ## Modules
//!
//! - [`discovery`]: enumerate sessions by scanning the sessions directory.
//! - [`spawn`]: launch a detached `shelbi __session` with an explicit
//!   environment.
//! - [`connect`]: open a connection, perform the hello handshake, and issue
//!   blocking requests.
//! - [`reader`]: the background reader that turns the output/event stream into
//!   channel messages.
//! - [`error`]: the crate's error type.
//!
//! The concrete bodies are filled in by the Phase 1 subtasks
//! (`rt-session-process`, `rt-protocol-client`, `rt-replay`, `rt-snapshot`);
//! this foundation establishes the crate, its boundaries, and its dependency
//! shape.

pub mod connect;
pub mod discovery;
pub mod error;
pub mod reader;
pub mod spawn;

pub use error::ClientError;
