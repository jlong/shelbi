//! Signalling a child started as its own process group leader
//! (`Command::process_group(0)`), so a probe that spawns helpers stops them
//! all.

/// Ask the group `pgid` leads to stop (`SIGTERM`), or make it (`SIGKILL`)
/// when `force`. Errors (the group is already gone) are ignored. A no-op
/// off Unix.
pub fn signal_process_group(pgid: u32, force: bool) {
    #[cfg(unix)]
    {
        let Ok(pgid) = i32::try_from(pgid) else { return };
        if pgid <= 1 {
            return;
        }
        let signal = if force { libc::SIGKILL } else { libc::SIGTERM };
        // SAFETY: kill(2) with a negative pid signals that process group;
        // it reads no memory of ours.
        unsafe {
            libc::kill(-pgid, signal);
        }
    }
    #[cfg(not(unix))]
    let _ = (pgid, force);
}
