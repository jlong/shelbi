//! A process-global gate that blocks spawning new agent sessions.
//!
//! When the hub daemon detects it has lost its macOS GUI login session (see the
//! daemon's `session_health` module), every session it would spawn inherits a
//! dead bootstrap context — no DNS, no user lookup, a stale `SSH_AUTH_SOCK`. So
//! the daemon flips this gate on, and [`deploy_and_spawn`](crate::workspace)
//! refuses to launch a new agent until the session is healthy again (recovery)
//! or the operator reopens.
//!
//! The gate is an in-process `AtomicBool`, not the on-disk marker, on purpose:
//! it is only ever set inside the daemon process (the one that both detects the
//! loss and spawns the sessions), and reading it on the spawn path costs a
//! single atomic load with no `SHELBI_HOME` resolution — so it never trips the
//! `cfg(test)` home-isolation guard on the many spawn-path tests. The durable
//! marker is the *warning* surface (TUI / `shelbi status` / `shelbi doctor`);
//! this is the *behavior* gate.

use std::sync::atomic::{AtomicBool, Ordering};

static SPAWN_BLOCKED: AtomicBool = AtomicBool::new(false);

/// Stop spawning new sessions. Called by the daemon when it declares its login
/// session lost (and at daemon startup when a marker is already on disk).
pub fn block_spawning() {
    SPAWN_BLOCKED.store(true, Ordering::SeqCst);
}

/// Resume spawning. Called by the daemon when the session recovers.
pub fn allow_spawning() {
    SPAWN_BLOCKED.store(false, Ordering::SeqCst);
}

/// Whether spawning is currently blocked.
pub fn spawning_blocked() -> bool {
    SPAWN_BLOCKED.load(Ordering::SeqCst)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_and_allow_toggle_the_gate() {
        // Serialized against the other session_guard test via the single global;
        // leave the gate open on the way out so unrelated tests aren't affected.
        allow_spawning();
        assert!(!spawning_blocked());
        block_spawning();
        assert!(spawning_blocked());
        allow_spawning();
        assert!(!spawning_blocked());
    }
}
