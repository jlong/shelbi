//! The per-project docs manager: one docs service
//! ([`shelbi_orchestrator::docs`]) for each open project that has docs.
//!
//! Like the poller manager next door, it wakes on an interval and reconciles
//! the running services against [`shelbi_state::list_open_projects`]; a
//! request for a project whose service isn't running starts it on demand
//! (`shelbi_orchestrator::docs::answer`), so the tick only matters for the
//! sync clock of docs nobody has asked about since the daemon started.
//! On stop every service commits what's pending and exits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// How often the manager reconciles. Slower than the poller manager's: it
/// reads each open project's YAML, and a docs' own clocks are seconds to
/// minutes.
const MANAGER_TICK: Duration = Duration::from_secs(10);

/// Slice the manager sleeps in so a stop is noticed quickly.
const STOP_POLL_SLICE: Duration = Duration::from_millis(250);

/// Env override (milliseconds) for [`MANAGER_TICK`].
const MANAGER_TICK_ENV: &str = "SHELBI_DOCS_MANAGER_TICK_MS";

fn manager_tick() -> Duration {
    match std::env::var(MANAGER_TICK_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => MANAGER_TICK,
    }
}

/// Spawn the docs manager thread. It exits when `stop` is set, stopping
/// every docs service on the way out.
pub(super) fn spawn_docs_manager(stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("shelbi-docs-mgr".into())
        .spawn(move || {
            let tick = manager_tick();
            while !stop.load(Ordering::SeqCst) {
                let open = shelbi_state::list_open_projects().unwrap_or_default();
                shelbi_orchestrator::docs::reconcile(&open);
                let mut waited = Duration::ZERO;
                while waited < tick && !stop.load(Ordering::SeqCst) {
                    thread::sleep(STOP_POLL_SLICE);
                    waited += STOP_POLL_SLICE;
                }
            }
            shelbi_orchestrator::docs::stop_all();
        })
        .ok();
}
