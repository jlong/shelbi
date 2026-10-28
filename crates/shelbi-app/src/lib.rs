//! The shared Shelbi application model.
//!
//! This crate is the toolkit-independent core that both the single-process
//! TUI and the later desktop app render. It owns:
//!
//! - [`nav`] — the model of what a client is looking at: the current
//!   project, the sidebar selection, the current main view, focus, and
//!   overlay state. Per-client state ([`nav::ClientState`]) is separate
//!   from global one-shot flags ([`nav::GlobalFlags`]).
//! - [`command`] — a real typed command registry that replaces the
//!   palette's string-prefix dispatch. Each command has an id, a title,
//!   typed arguments, an availability check against the current model, and
//!   an [`Effect`](exec::Effect) it produces.
//! - [`board`] — the issues board's shared semantics (columns, status
//!   resolution, legal moves, card order, filters) and its background reads.
//! - [`view`] — plain-data view models for the sidebar, issues board,
//!   activity feed, machines list, review panel, and error log, built from
//!   the existing `shelbi-state` readers.
//! - [`review`] — the review interface: the panel's model and navigation,
//!   and the load / queue flow that opens a review.
//! - [`overlay`] — the overlays' state machines (palette, error log,
//!   review load, reject reason, Zen intro, add project), keyed on
//!   toolkit-free chords so both clients drive the same decision tables;
//!   [`command_model`] builds the palette's inputs from live state.
//! - [`settings`] — Settings' pages and sections, the settings sidebar's
//!   navigation over the pages a client offers, and the hooks a page uses
//!   for background loads and saves and the unsaved-edits leave prompt.
//! - [`text_area`] — a multi-line text buffer with a caret, for the editors
//!   both clients' toolkit-free states type into.
//! - [`theme`] — the named color tokens (dark and light) both clients
//!   paint with, and the `theme:` preference resolution.
//! - [`refresh`] — a runtime-agnostic background refresh worker that reads
//!   state off the caller's thread and publishes immutable snapshots over a
//!   channel. Threads and channels only, so a gpui client can consume it
//!   without tokio. [`changes`] is its wake source: a hub-socket subscriber
//!   for the daemon's board/workspace change notifications; [`layout_sub`]
//!   is the same for its layout events (a review slot opening or closing).
//! - [`session`] — showing a session in a main area: naming
//!   ([`session::SessionRef`]), attach with the relaunch-tolerant connect
//!   classification, and the per-pane restart controller over
//!   [`supervision`], the crash-loop core the daemon poller shares.
//! - [`about`] — About Shelbi's words and links (the tagline, the project's
//!   URLs, the copyright and license), so both clients' About screens read
//!   the same.
//!
//! # No UI toolkit
//!
//! Nothing here depends on ratatui, crossterm, or gpui, directly or
//! transitively. The key-chord type it leans on lives in `shelbi-state`,
//! which was moved off crossterm for exactly this reason; the conversion to
//! and from a terminal toolkit happens in the renderer crates
//! (`shelbi-tui`, `shelbi-cli`). The `tests/no_ui_deps.rs` integration test
//! asserts the invariant against the resolved dependency tree.

pub mod about;
pub mod activity;
pub mod add_project_wizard;
pub mod board;
pub mod changes;
pub mod command;
pub mod command_model;
pub mod exec;
pub mod exec_daemon;
pub mod layout_sub;
pub mod nav;
pub mod overlay;
pub mod reachability;
pub mod refresh;
pub mod review;
pub mod session;
pub mod settings;
pub mod supervision;
pub mod text_area;
pub mod theme;
pub mod view;
pub mod workspace_panel;
pub mod zen;

pub use command::{Command, CommandKind, CommandModel, CommandRegistry};
pub use exec::{EditTarget, Effect, ExecError, ExecOutcome, Executor, Mutation};
pub use exec_daemon::{
    execute_mutation, execute_mutation_expecting, review_session, workspace_session,
};
pub use shelbi_proto::control::{ExpectedState, ReviewRole, ReviewSessionOp, WorkspaceSessionOp};
pub use nav::{ClientState, Focus, GlobalFlags, Overlay, View};
pub use refresh::{spawn_refresher, spawn_worker, RefreshHandle, Refresher, Snapshot};
pub use view::{
    BoardFilters, ErrorLogModel, IssueCard, IssueColumn, IssuesModel,
    MachinesModel, ReviewPanelModel, SidebarModel, TaskDetailModel,
};

/// Serializes the tests that point the process-global `SHELBI_HOME` at a
/// throwaway directory: every such test in the crate takes this one lock.
#[cfg(test)]
pub(crate) mod test_env {
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
}
