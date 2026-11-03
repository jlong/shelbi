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
//! - `query`: [`answer`], backed by this machine's project source.
//! - `control`: this machine's `control.sock` (filtered by the relay).
//! - `changes`: this machine's `hub.sock` (subscribe only).
//!
//! It never starts the daemon: `control` and `changes` fail to open while it
//! is stopped. Not intended for direct use by hand.

use std::io::{stdin, stdout, Read, Write};

use anyhow::Result;
use clap::Parser;
use shelbi_app::source::wire::{self, Answer, Query, QueryError};
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
        query: Some(wire::handler(answer)),
        control_socket: shelbi_state::control_socket_path().ok(),
        hub_socket: shelbi_state::hub_socket_path().ok(),
    }
}

/// Answer one `query` through this machine's project source
/// (`shelbi_orchestrator::source::LocalSource`): the code a local window reads
/// its project through, so a remote window sees what a local one would.
fn answer(query: Query) -> Result<Answer, QueryError> {
    use shelbi_app::source::ProjectSource;
    // A name that isn't a registered project reads as nothing, never as a
    // path: every reader behind the source resolves it through the project
    // registry.
    let source = shelbi_orchestrator::source::LocalSource::new(query.project());
    let changelog = |source: &dyn ProjectSource| {
        // No stamp to match: the source always reads.
        source.changelog(None).ok_or_else(|| QueryError::Failed {
            message: "the changelog didn't read".into(),
        })
    };
    Ok(match query {
        Query::Sidebar { .. } => Answer::Sidebar(source.sidebar()),
        Query::ConfigSkeleton { .. } => Answer::ConfigSkeleton(source.config_skeleton()),
        Query::OpenBoard { .. } => Answer::OpenBoard(Box::new(source.open_board())),
        Query::ClosedPage { .. } => Answer::ClosedPage(source.closed_page()),
        Query::Changelog { .. } => Answer::Changelog(changelog(&source)?),
        Query::SearchCorpus { .. } => Answer::SearchCorpus(shelbi_app::overlay::palette_search::corpus_from_reads(
            source.open_board(),
            changelog(&source)?.entries,
        )),
        Query::Machines { .. } => Answer::Machines(
            source
                .machines()
                .map_err(|message| QueryError::Failed { message })?,
        ),
        Query::ProjectExists { .. } => Answer::ProjectExists(source.project_exists()),
    })
}
