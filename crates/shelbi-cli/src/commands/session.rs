//! `shelbi __session` — the hidden entry point for the per-session process.
//!
//! This just parses the argv the detached spawner passes
//! ([`shelbi_session::spawn_detached`]) and hands off to
//! [`shelbi_session::run`]. All the behavior lives in `shelbi-session`; this is
//! the thin CLI seam.

use std::path::PathBuf;

use anyhow::Result;
use clap::Parser;

/// Arguments for `shelbi __session`. Mirrors
/// [`shelbi_session::RunArgs`](shelbi_session::RunArgs); the trailing child argv
/// comes after `--`.
#[derive(Debug, Parser)]
pub struct Args {
    /// Short directory id (computed by the spawner so it knows the socket path).
    #[arg(long)]
    id: String,
    /// Readable session name, recorded in `meta.json`.
    #[arg(long)]
    name: String,
    /// Working directory for the child.
    #[arg(long)]
    cwd: PathBuf,
    /// Initial screen width in columns.
    #[arg(long)]
    cols: u16,
    /// Initial screen height in rows.
    #[arg(long)]
    rows: u16,
    /// Task id this session serves, if any.
    #[arg(long)]
    task: Option<String>,
    /// Keep the full raw output log (the project's opt-in). Off unless passed.
    #[arg(long = "raw-log")]
    raw_log: bool,
    /// The child program and its arguments — everything after `--`.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
    child_argv: Vec<String>,
}

/// Dispatch `shelbi __session`.
pub fn run(args: Args) -> Result<()> {
    shelbi_session::run(shelbi_session::RunArgs {
        id: args.id,
        name: args.name,
        cwd: args.cwd,
        cols: args.cols,
        rows: args.rows,
        task: args.task,
        raw_output_log: args.raw_log,
        child_argv: args.child_argv,
        manage_daemon: true,
    })
}
