//! The shared Shelbi application model.
//!
//! This crate is the toolkit-independent core that both the single-process
//! TUI and the later desktop app render. It owns four things:
//!
//! - [`nav`] — the model of what a client is looking at: the current
//!   project, the sidebar selection, the current main view, focus, and
//!   overlay state. Per-client state ([`nav::ClientState`]) is separate
//!   from global one-shot flags ([`nav::GlobalFlags`]).
//! - [`command`] — a real typed command registry that replaces the
//!   palette's string-prefix dispatch. Each command has an id, a title,
//!   typed arguments, an availability check against the current model, and
//!   an [`Effect`](exec::Effect) it produces.
//! - [`view`] — plain-data view models for the sidebar, issues board,
//!   activity feed, machines list, review panel, and error log, built from
//!   the existing `shelbi-state` readers.
//! - [`refresh`] — a runtime-agnostic background refresh worker that reads
//!   state off the caller's thread and publishes immutable snapshots over a
//!   channel. Threads and channels only, so a gpui client can consume it
//!   without tokio.
//!
//! # No UI toolkit
//!
//! Nothing here depends on ratatui, crossterm, or gpui, directly or
//! transitively. The key-chord type it leans on lives in `shelbi-state`,
//! which was moved off crossterm for exactly this reason; the conversion to
//! and from a terminal toolkit happens in the renderer crates
//! (`shelbi-tui`, `shelbi-cli`). The `tests/no_ui_deps.rs` integration test
//! asserts the invariant against the resolved dependency tree.

pub mod command;
pub mod exec;
pub mod exec_daemon;
pub mod nav;
pub mod refresh;
pub mod view;

pub use command::{Command, CommandKind, CommandModel, CommandRegistry};
pub use exec::{EditTarget, Effect, ExecError, ExecOutcome, Executor, Mutation};
pub use exec_daemon::{execute_mutation, review_session};
pub use shelbi_proto::control::{ReviewRole, ReviewSessionOp};
pub use nav::{ClientState, Focus, GlobalFlags, Overlay, View};
pub use refresh::{spawn_refresher, RefreshHandle, Snapshot};
pub use view::{
    ActivityModel, ErrorLogModel, IssuesModel, MachinesModel, ReviewPanelModel, SidebarModel,
};
