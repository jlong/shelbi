//! Discover sessions by scanning `~/.shelbi/sessions/`.
//!
//! There is no central registry (the cost accepted for one-process-per-session):
//! sessions are found by listing the sessions directory. Each subdirectory is a
//! short hash holding `sock`, `lock`, `meta.json`, and after exit `exit.json`
//! and `final.txt`. The directory name is a short hash rather than the session
//! name to stay under the 104-byte socket-path limit on macOS;
//! [`SessionMeta`] carries the readable name.
//!
//! A session whose `lock` is not held is dead, and its directory is stale state
//! to be reaped.
//!
//! TODO (`rt-session-process`): implement [`list`] (scan the directory, parse
//! each `meta.json`, check liveness via the lock) and the reaper for stale
//! directories.

use serde::{Deserialize, Serialize};

/// The readable metadata a session records in `meta.json`.
///
/// The `name` follows the scheme `<project>/orch`, `<project>/ws/<workspace>`,
/// `<project>/review/<slot>/<role>`, or `<project>/shell/<workspace>`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    /// Human-readable session name (see the scheme above).
    pub name: String,
    /// The command line the session is running.
    pub argv: Vec<String>,
    /// The working directory the child was started in.
    pub cwd: String,
    /// The task id this session serves, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// RFC3339 launch time.
    pub launched_at: String,
    /// Frozen-core protocol version the session speaks.
    pub protocol_version: u16,
}

/// A discovered session directory: its short id, on-disk path, parsed metadata,
/// and whether it is still alive (its lock held).
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    /// Short hash that names the directory under `~/.shelbi/sessions/`.
    pub short_id: String,
    /// Parsed `meta.json`.
    pub meta: SessionMeta,
    /// Whether the session's lock is currently held (i.e. the session is live).
    pub alive: bool,
}

/// Enumerate sessions under the sessions directory.
///
/// TODO (`rt-session-process`): scan `~/.shelbi/sessions/`, parse each
/// `meta.json`, and probe the lock for liveness.
pub fn list() -> Result<Vec<DiscoveredSession>, crate::ClientError> {
    // Skeleton: the real scan lands with the session process.
    Ok(Vec::new())
}
