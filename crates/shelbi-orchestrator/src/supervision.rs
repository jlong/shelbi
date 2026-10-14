//! Auto-restart supervision for shelbi-managed panes.
//!
//! The pure decision core (cap, backoff, sliding window, recovery, give-up)
//! lives in [`shelbi_app::supervision`] so every client (the TUI and the
//! desktop app) and the daemon poller run one copy of the rules. It is
//! re-exported here so the poller and the load path keep their
//! `shelbi_orchestrator::supervision` paths.

pub use shelbi_app::supervision::*;
