//! The Chat view: the project's orchestrator session (`<project>/orch`) as a
//! full-height terminal in the main area. This is the view the user lives in.
//!
//! It is a [`SessionView`](super::session_view::SessionView) over
//! `SessionRef::Orchestrator` (built with `SessionView::orchestrator`): the
//! attach, relaunch-tolerant connect, restart states and sizing are the shared
//! session view's, so the Workspace view's panes behave the same.

pub use super::session_view::{
    SessionDeps as ChatDeps, SessionView as ChatView, SessionViewEvent as ChatEvent,
};
