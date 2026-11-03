//! A window's writes to its project's state files, through the daemon that
//! owns them (control protocol v7, `rw-remote-state-writes`).
//!
//! The board's workspace filter and column overrides, Zen Mode, the error
//! log and its read mark, and a persistent pane's reopen marker live in the
//! project's state on the machine that runs it. A client never writes them
//! itself: it sends a [`StateWrite`] to that machine's daemon, the single
//! writer, which tells its other clients. A window on this machine sends it
//! on the local control socket and a remote window over its host's relay, so
//! both follow one path ([`write`]).
//!
//! The viewer's own state (`zen_intro_seen`, in this machine's global state)
//! is not here: it stays local whichever project the window shows.

pub use shelbi_proto::control::{ColumnOverride, RelaunchPane, StateWrite};

use crate::source::ProjectLink;

/// Make `write` to `project`'s state through the window's `link`: a remote
/// link's host daemon, over its relay, or (a local link, or none) this
/// machine's daemon, started on demand. A remote link writes only its own
/// project. Blocking: run it off the UI thread.
pub fn write(project: &str, link: Option<&ProjectLink>, write: StateWrite) -> Result<(), String> {
    match link {
        Some(link) if link.is_remote() => {
            if link.key().name != project {
                return Err(format!("`{project}` isn't this window's project"));
            }
            link.write_state(write)
        }
        _ => crate::exec_daemon::write_state(project, write).map_err(|e| e.to_string()),
    }
}

/// The wire form of a board column's collapse override.
pub fn column_override(value: shelbi_state::KanbanColumnOverride) -> ColumnOverride {
    match value {
        shelbi_state::KanbanColumnOverride::Collapsed => ColumnOverride::Collapsed,
        shelbi_state::KanbanColumnOverride::Expanded => ColumnOverride::Expanded,
    }
}

/// A test's stand-in for this machine's daemon: a process with no daemon
/// (a unit test) makes its local state writes through an in-process writer
/// instead, normally `shelbi_orchestrator::state_write::apply`, the very
/// function the daemon runs. A remote link is unaffected.
#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use std::sync::{Arc, RwLock};

    use super::StateWrite;

    /// Makes one write to a project's state, as the daemon would.
    pub type LocalWriter = Arc<dyn Fn(&str, &StateWrite) -> Result<(), String> + Send + Sync>;

    static LOCAL: RwLock<Option<LocalWriter>> = RwLock::new(None);

    /// Make every later local state write in this process with `writer`
    /// instead of the daemon (replacing any earlier one).
    pub fn write_locally_with(writer: LocalWriter) {
        *LOCAL.write().unwrap_or_else(|p| p.into_inner()) = Some(writer);
    }

    pub(crate) fn local_writer() -> Option<LocalWriter> {
        LOCAL.read().unwrap_or_else(|p| p.into_inner()).clone()
    }
}
