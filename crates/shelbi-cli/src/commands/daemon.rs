//! `shelbi daemon` — hub-side Unix-socket listener for worker → hub
//! messages plus the OS-supervisor install/uninstall/status/restart
//! plumbing that makes the daemon survive crashes and reboots. Phases 1,
//! 2, 4, and 9 of the Worker → Orchestrator Communication feature
//! (see `Plans/worker-orchestrator-communication.md` §5, §6, §8, §9, §13).
//!
//! The implementation is split into two focused submodules that share
//! nothing but `Result`:
//!
//! - [`serve`] — the foreground socket server: bind + accept loop,
//!   message protocol, the unacked-message reaper, and graceful
//!   shutdown. This is what `launchd`/`systemd` run with a bare
//!   `shelbi daemon`.
//! - [`supervise`] — the `install`/`uninstall`/`status`/`restart`
//!   subcommands that manage the platform supervisor unit (launchd
//!   plist on macOS, systemd `--user` service on Linux).

mod serve;
mod supervise;

use anyhow::Result;

/// `shelbi daemon <subcommand>`. The foreground entry runs when no
/// subcommand is supplied — launchd/systemd invoke us with bare
/// `shelbi daemon` and we serve until killed.
#[derive(Debug, clap::Subcommand)]
pub enum DaemonCmd {
    /// (default — also the form launchd/systemd invoke) Bind the hub
    /// socket and accept worker messages in the foreground until killed.
    Run,
    /// Install the platform supervisor unit (launchd plist on macOS,
    /// systemd user service on Linux) so the daemon auto-starts at login
    /// and is restarted on crash. Idempotent — re-running just refreshes
    /// the unit file and reloads it.
    Install,
    /// Stop the daemon and remove the platform supervisor unit.
    Uninstall,
    /// Print a short human-readable status by wrapping `launchctl print`
    /// or `systemctl --user status`.
    Status,
    /// Stop the daemon so the supervisor relaunches it — picks up a
    /// freshly installed binary without losing the auto-restart
    /// guarantee.
    Restart,
}

pub fn run(cmd: Option<DaemonCmd>) -> Result<()> {
    match cmd {
        None | Some(DaemonCmd::Run) => serve::run_foreground(),
        Some(DaemonCmd::Install) => supervise::install(),
        Some(DaemonCmd::Uninstall) => supervise::uninstall(),
        Some(DaemonCmd::Status) => supervise::status(),
        Some(DaemonCmd::Restart) => supervise::restart(),
    }
}
