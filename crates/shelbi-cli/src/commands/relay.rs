//! `shelbi relay` — bridge one stdio channel to every session on this machine,
//! and (relay protocol v2) to this machine's project reads, daemon control
//! socket and change notifications.
//!
//! Two kinds of client start this over SSH as a short-lived process:
//!
//! - the hub, to reach the sessions of a project's workspaces on this machine
//!   (`ssh <host> shelbi relay`);
//! - a remote desktop window on another machine, to show a project that lives
//!   here (`shelbi_app::launch::RemoteLink`), which also opens the services.
//!
//! It reads the [`shelbi_proto::relay`] envelope on stdin and writes it on
//! stdout. It holds no PTYs and no session state, so if it or the SSH
//! connection dies the client simply starts another. All the protocol
//! behavior lives in [`shelbi_client::serve_relay_with`]; this is the thin CLI
//! seam that injects the services:
//!
//! - `query`: `shelbi_orchestrator::source::QueryAnswers`, this machine's
//!   project source.
//! - `control`: this machine's `control.sock` (filtered by the relay).
//! - `changes`: this machine's `hub.sock` (subscribe only).
//!
//! It never starts the daemon: `control` and `changes` fail to open while it
//! is stopped. Not intended for direct use by hand.

use std::io::{stdin, stdout, Read, Write};

use anyhow::Result;
use clap::Parser;
use shelbi_client::RelayServices;

/// Arguments for `shelbi relay`. There are none today: the relay serves the one
/// stdio channel it was started with and discovers sessions from the default
/// `~/.shelbi/sessions/` root.
#[derive(Debug, Parser)]
pub struct Args {}

/// Dispatch `shelbi relay`: serve the relay protocol over stdin/stdout until the
/// channel closes.
pub fn run(_args: Args) -> Result<()> {
    let root = shelbi_state::sessions_dir()?;
    let read: Box<dyn Read + Send> = Box::new(stdin());
    let write: Box<dyn Write + Send> = Box::new(stdout());
    shelbi_client::serve_relay_with(read, write, &root, services())?;
    Ok(())
}

/// The services this machine's relay offers.
fn services() -> RelayServices {
    RelayServices {
        // This machine's project source (`shelbi_orchestrator::source::
        // LocalSource`), the code a local window reads its project through,
        // so a remote window sees what a local one would.
        query: Some(shelbi_orchestrator::source::QueryAnswers::default().handler()),
        control_socket: shelbi_state::control_socket_path().ok(),
        hub_socket: shelbi_state::hub_socket_path().ok(),
    }
}
