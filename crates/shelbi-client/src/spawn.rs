//! Spawn a detached `shelbi __session` process.
//!
//! A client runs `shelbi __session` detached, passing argv, cwd, initial size,
//! metadata, and an explicit, scrubbed environment; the session process never
//! inherits the environment of whatever launched it. The detach recipe (setsid,
//! the opt-in Linux `systemd-run --user --scope`, stdio to `/dev/null`) and the
//! login-shell environment capture live in [`shelbi_session::spawn`]; this crate
//! re-exports them so a client has one entry point for discover + spawn +
//! connect. See the plan's "One process per session".

pub use shelbi_session::{SpawnSpec, SpawnedSession};

use crate::error::ClientError;

/// Spawn `spec` as a detached session process and return its handle (short id,
/// directory, socket path, pid). Finds the running `shelbi` binary via
/// `current_exe`; the session reparents to init and outlives this caller.
///
/// Connect to the returned [`SpawnedSession::sock`] with
/// [`Connection::open`](crate::connect::Connection::open) once it appears.
pub fn spawn(spec: &SpawnSpec) -> Result<SpawnedSession, ClientError> {
    shelbi_session::spawn_detached(spec).map_err(|e| ClientError::Spawn(e.to_string()))
}

/// Like [`spawn`] but with the `shelbi` executable named explicitly — tests
/// point this at `CARGO_BIN_EXE_shelbi` instead of the test harness binary.
pub fn spawn_with_exe(
    exe: &std::path::Path,
    spec: &SpawnSpec,
) -> Result<SpawnedSession, ClientError> {
    shelbi_session::spawn_detached_with_exe(exe, spec).map_err(|e| ClientError::Spawn(e.to_string()))
}
