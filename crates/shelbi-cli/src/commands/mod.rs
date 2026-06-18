pub mod archive;
pub mod diff;
pub mod init;
pub mod list;
pub mod merge;
pub mod send;
pub mod spawn;
pub mod status;
pub mod tail;

use anyhow::{anyhow, Result};

/// Resolve the active project name from the `--project` flag or env.
/// For v1 we just require it explicitly until session/project discovery
/// lands in Phase 6.
pub fn require_project(p: Option<String>) -> Result<String> {
    p.ok_or_else(|| {
        anyhow!(
            "no project specified — pass --project NAME or set SHELBI_PROJECT (session-based \
             discovery lands in Phase 6)"
        )
    })
}
