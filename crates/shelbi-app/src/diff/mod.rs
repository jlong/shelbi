//! Shelbi's own diff view, apart from any toolkit: the foundation both clients
//! draw (`desktop/src/views`, `crates/shelbi-tui/src/shell`). Its keys are
//! Skim's, in the rebindable `diff` keymap context
//! ([`shelbi_state::keymap::DiffAction`]).
//!
//! How the pieces fit, from the worktree up:
//!
//! - [`git`] reads a changeset out of a worktree on any machine, as a
//!   [`git::RawChangeset`]. [`backend`] chooses the path there: this machine,
//!   SSH to a workspace on another, or a far daemon for a window on a project
//!   that lives elsewhere ([`DiffTarget`] names what's being read).
//! - [`parse`] and [`intraline`] turn it into the [`model`]: files, hunks,
//!   numbered lines, word highlights. [`highlight`] adds syntax classes, off
//!   the UI thread, a file at a time.
//! - [`comments`] and [`store`] hold a task's threads and viewed marks, in the
//!   project's state directory, finding each thread's lines again after a
//!   refresh.
//! - [`rows`] flattens all of it, through folds, the hunk filter and the view
//!   mode, into the row table a client virtualizes over. [`search`] and
//!   [`palette`] sit beside it.
//! - [`help`] is the `?` overlay's contents, from the live keymap.
//! - [`state`] is the state machine ([`DiffState`]): the cursor, the modes,
//!   what each action does. [`session`] runs it with the worker threads
//!   ([`DiffSession`]): this is what a client holds.
//!
//! A client's job is then: open a [`DiffSession`], pass it keys, draw
//! [`DiffState::visible_rows`], and carry out the [`DiffEffect`]s only it can
//! (copy, open the editor, send to the agent).

pub mod backend;
pub mod comments;
pub mod draft;
pub mod git;
pub mod help;
pub mod highlight;
pub mod intraline;
pub mod model;
pub mod palette;
pub mod parse;
pub mod rows;
pub mod search;
pub mod session;
pub mod state;
pub mod store;

pub use backend::{
    answer, backend_for, DiffBackend, DiffOp, DiffReply, DiffRequest, DiffTarget, FarBackend, HostBackend, TargetKind,
};
pub use model::{Changeset, CommitInfo, CommitScope, DiffLine, Eol, FileDiff, FileStatus, Hunk, LineKind, Side};
pub use rows::{Folds, HunkFilter, NoticeKind, Row, RowTable, ThreadPart, ViewMode};
pub use session::{DiffOptions, DiffSession, Wake, WORKSPACE_WATCH};
pub use shelbi_state::keymap::DiffAction;
pub use state::{DiffEffect, DiffState, FindKind, Mode};

#[cfg(test)]
mod state_tests;
