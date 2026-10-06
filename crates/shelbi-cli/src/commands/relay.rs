//! `shelbi relay` — bridge one stdio channel to every session on this machine.
//!
//! The hub starts this over SSH (`ssh <host> shelbi relay`) as a short-lived
//! process: it reads the [`shelbi_proto::relay`] envelope on stdin and writes it
//! on stdout, multiplexing a single channel to every session socket under
//! `~/.shelbi/sessions/`. It holds no PTYs and no session state, so if it or the
//! SSH connection dies the hub simply starts another. All the behavior lives in
//! [`shelbi_client::serve_relay`]; this is the thin CLI seam.
//!
//! The hub side — resolving the remote `shelbi` binary path and wiring the SSH
//! child's piped stdio to a [`shelbi_client::RelayChannel`] — is `rt-machine-setup`
//! / `rt-remote-spawn`; this crate only provides the server end and the
//! transport. Not intended for direct use by hand.

use std::io::{stdin, stdout, Read, Write};

use anyhow::Result;
use clap::Parser;

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
    shelbi_client::serve_relay(read, write, &root)?;
    Ok(())
}
