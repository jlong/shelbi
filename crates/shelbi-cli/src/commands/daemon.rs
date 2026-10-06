//! `shelbi daemon` — hub-side Unix-socket listener for worker → hub messages
//! plus the on-demand lifecycle (`restart`/`status`) that replaces the retired
//! launchd/systemd supervisor. Phases 1, 2, 4, and 9 of the Worker →
//! Orchestrator Communication feature (see
//! `Plans/worker-orchestrator-communication.md`), and Phase 3 of the
//! remove-tmux effort (`docs/removing-tmux/phase3-daemon.md`).
//!
//! The daemon is started on demand (the first client that needs it calls
//! `shelbi_state::ensure_daemon_running`) and exits when no project is open, so
//! there is no installed service: `shelbi daemon install`/`uninstall` are gone,
//! and an upgrade step in [`serve`] retires any leftover unit on startup.
//!
//! The implementation is split into focused submodules that share nothing but
//! `Result`:
//!
//! - [`serve`] — the foreground socket server: bind + accept loop, message
//!   protocol, the unacked-message reaper, the login-shell environment overlay,
//!   the unit-retire upgrade step, the idle-exit monitor, and graceful
//!   shutdown. This is what a bare `shelbi daemon` runs.
//! - [`board`] — the hub-owned board-index refresh loop (one per open project)
//!   and the `refresh-board` hub verb.
//! - [`poller`] — the per-project workspace-poller manager (one poller per open
//!   project).
//! - [`lifecycle`] — `restart` and `status` without a supervisor, plus the
//!   `retire_supervisor_units` upgrade step [`serve`] calls on startup.

mod board;
mod control;
mod lifecycle;
mod poller;
mod serve;

use anyhow::Result;

/// `shelbi daemon <subcommand>`. The foreground entry runs when no subcommand
/// is supplied — a bare `shelbi daemon` serves until killed or idle.
#[derive(Debug, clap::Subcommand)]
pub enum DaemonCmd {
    /// (default) Bind the hub socket and accept worker messages in the
    /// foreground until killed or until no project is open.
    Run,
    /// Print a short human-readable status from the single-instance lock and
    /// the socket (no supervisor involved).
    Status,
    /// Stop the running daemon and start a fresh one on the current binary,
    /// directly — no supervisor. Picks up a freshly installed binary and is the
    /// path the version-mismatch flow uses.
    Restart,
}

pub fn run(cmd: Option<DaemonCmd>) -> Result<()> {
    match cmd {
        None | Some(DaemonCmd::Run) => serve::run_foreground(),
        Some(DaemonCmd::Status) => lifecycle::status(),
        Some(DaemonCmd::Restart) => lifecycle::restart(),
    }
}
