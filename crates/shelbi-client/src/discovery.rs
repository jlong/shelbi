//! Discover sessions by scanning the sessions directory (`~/.shelbi/sessions/`).
//!
//! There is no central registry (the cost accepted for one-process-per-session):
//! sessions are found by listing the directory. Each subdirectory is a short
//! hash holding `sock`, `lock`, `meta.json`, and after exit `exit.json` and
//! `final.txt`. The directory name is a short hash rather than the session name
//! to stay under the 104-byte socket-path limit on macOS;
//! [`Meta`](shelbi_session::Meta) carries the readable name.
//!
//! A session whose `lock` is not held is **dead**, and its directory is stale
//! state to be reaped ([`reap_dead`]). Liveness is the same non-blocking `flock`
//! probe the session uses for its lifetime lock
//! ([`shelbi_session::lock::is_held`]).
//!
//! The scanning root is passed in rather than resolved here, so the crate needs
//! no dependency on `shelbi-state`; a caller passes `shelbi_state::sessions_dir()`.

use std::path::{Path, PathBuf};

use shelbi_session::lock::is_held;
use shelbi_session::Meta;

/// A discovered session directory: its short id, on-disk paths, parsed metadata,
/// and whether it is still alive (its lock held).
#[derive(Debug, Clone)]
pub struct DiscoveredSession {
    /// Short hash that names the directory under the sessions root.
    pub short_id: String,
    /// The session directory.
    pub dir: PathBuf,
    /// The Unix socket to connect to (present whether or not the session lives).
    pub sock: PathBuf,
    /// Parsed `meta.json`.
    pub meta: Meta,
    /// Whether the session's lock is currently held (i.e. it is live).
    pub alive: bool,
}

/// Enumerate sessions under `root`, newest unspecified order. Directories with
/// no readable `meta.json` are skipped (a session mid-creation, or a non-session
/// directory). Does not delete anything — see [`reap_dead`].
///
/// A missing `root` is not an error: it means no session has ever started.
pub fn list(root: &Path) -> Result<Vec<DiscoveredSession>, crate::ClientError> {
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(out),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let meta_path = dir.join("meta.json");
        let meta = match std::fs::read_to_string(&meta_path).ok().and_then(|s| Meta::from_json(&s).ok()) {
            Some(m) => m,
            None => continue, // not a session dir, or still being created
        };
        let short_id = entry.file_name().to_string_lossy().into_owned();
        out.push(DiscoveredSession {
            sock: dir.join("sock"),
            alive: is_held(&dir.join("lock")),
            short_id,
            dir,
            meta,
        });
    }
    Ok(out)
}

/// Remove every session directory under `root` whose lock is **not held** (the
/// session is dead), returning the short ids reaped. A caller that still wants a
/// dead session's `exit.json` / `final.txt` reads them before calling this.
///
/// A directory that cannot be removed is skipped (best-effort cleanup), not an
/// error; a missing `root` reaps nothing.
pub fn reap_dead(root: &Path) -> Result<Vec<String>, crate::ClientError> {
    let mut reaped = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(reaped),
        Err(e) => return Err(e.into()),
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        // Only reap things that look like session directories (have a lock file),
        // so an unrelated directory under the root is never deleted.
        let lock = dir.join("lock");
        if !lock.exists() {
            continue;
        }
        if !is_held(&lock) && std::fs::remove_dir_all(&dir).is_ok() {
            reaped.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    Ok(reaped)
}
