//! Shared harness for the crate's real-tmux round-trip tests.
//!
//! Every tmux-backed test in this crate compiles into ONE test binary and,
//! historically, drove the machine-wide *default* tmux server. Under CI load
//! several of them (plus `shelbi zen probe`) hit that one server at once, and a
//! concurrent teardown there could drop a test's client mid-command with
//! `server exited unexpectedly` — failing PRs whose code was unrelated. The fix
//! is to give this test *process* its own tmux server: [`use_private_tmux_server`]
//! pins `TMUX_TMPDIR` at a private directory, so every `tmux` these tests spawn
//! — directly and through the production workspace/pane helpers, which inherit
//! our env — lands on a server socket no other process (or crate's test binary)
//! can see.
//!
//! Because that pin is process-global (a `TMUX_TMPDIR` `set_var`, inherited by
//! every later `tmux` spawn in the process), it MUST hold for the whole binary:
//! a test left on the default server while others pin the private one is still
//! exposed to the original flake, and a short-lived fixture left on the *now
//! private* server can empty it and take the server down under a sibling test.
//! So the doctrine is uniform across the crate's real-tmux tests:
//!
//! * call [`use_private_tmux_server`] before the first tmux interaction,
//! * hold [`crate::test_lock`] (serializes the env pin and any `SHELBI_HOME`
//!   writes against the rest of the suite),
//! * keep at least one long-lived holder session ([`start_session`], `sleep 600`
//!   — never `sleep 2`) alive for the whole test so the private server can't
//!   empty and exit mid-run,
//! * use unique per-pid session names, and
//! * only ever `kill-session` (never `kill-server`), so tests sharing this
//!   process coexist on the private server without tearing it out from under
//!   each other.

use std::time::Duration;

/// Point this test process's tmux at a private server socket. Idempotent and
/// thread-safe: the first caller creates the dir and sets the env, all later
/// callers are a no-op. Never restored — the process is a throwaway test runner
/// and every real-tmux test wants the same private server. Call it before the
/// FIRST tmux interaction of a test (some assert Dead on a not-yet-created
/// session, so even the opening probe must hit the private server, not the
/// default one). Callers hold [`crate::test_lock`] so the one-time `set_var`
/// can't race a concurrent env reader elsewhere in the suite.
pub(crate) fn use_private_tmux_server() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let dir = std::env::temp_dir().join(format!("shelbi-tmux-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::env::set_var("TMUX_TMPDIR", &dir);
    });
}

/// Try to create a detached `sleep`-holding session on the private server,
/// returning whether it came up live. tmux forks its server lazily on the first
/// client; under load that fork can lose the race with the follow-up command,
/// surfacing as `server exited unexpectedly`, so we retry a transient cold-start
/// failure and confirm liveness via `has-session` before returning. The pane
/// sleeps far longer than any test body needs so its shell can't exit mid-test
/// and tear the slot (or the whole private server) down early.
///
/// Returns `false` only when tmux can't create the session at all after the
/// retry budget — e.g. a sandbox that denies socket access. Callers that treat
/// tmux as optional use that to skip; [`start_session`] panics instead.
pub(crate) fn try_start_session(session: &str, window: &str) -> bool {
    use_private_tmux_server();
    // Clear a same-named session left by a prior aborted run in this process
    // (unique per-pid names mean this only ever hits our own).
    kill_session(session);
    for _ in 0..20 {
        let started = std::process::Command::new("tmux")
            .args([
                "new-session", "-d", "-s", session, "-n", window, "sh", "-c", "sleep 600",
            ])
            .output();
        if matches!(&started, Ok(out) if out.status.success()) && wait_for_session(session) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    false
}

/// [`try_start_session`] but panics if the session never comes up. Use it once
/// tmux is known to be available (`tmux -V` succeeded) and a missing server is a
/// genuine failure rather than a skip.
pub(crate) fn start_session(session: &str, window: &str) {
    assert!(
        try_start_session(session, window),
        "failed to start isolated tmux session `{session}`"
    );
}

/// Poll `has-session` until the private server confirms `session` is live,
/// giving the freshly-forked server time to answer. Returns whether it came up
/// within the budget.
fn wait_for_session(session: &str) -> bool {
    for _ in 0..100 {
        let up = std::process::Command::new("tmux")
            .args(["has-session", "-t", &format!("={session}")])
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        if up {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

/// Kill a session by exact name on the private server (`=name` disables tmux's
/// fnmatch so we never reap a look-alike). Best-effort: a missing session is
/// fine. Never `kill-server`, so sibling tests on the shared private server
/// survive.
pub(crate) fn kill_session(session: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", &format!("={session}")])
        .output();
}
