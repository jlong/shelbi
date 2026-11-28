//! The per-project wiki manager: one wiki service
//! ([`shelbi_orchestrator::wiki`]) for each open project that has a wiki.
//!
//! Like the poller manager next door, it wakes on an interval and reconciles
//! the running services against [`shelbi_state::list_open_projects`]; a
//! request for a project whose service isn't running starts it on demand
//! (`shelbi_orchestrator::wiki::answer`), so the tick only matters for the
//! sync clock of a wiki nobody has asked about since the daemon started.
//! On stop every service commits what's pending and exits.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

/// How often the manager reconciles. Slower than the poller manager's: it
/// reads each open project's YAML, and a wiki's own clocks are seconds to
/// minutes.
const MANAGER_TICK: Duration = Duration::from_secs(10);

/// Slice the manager sleeps in so a stop is noticed quickly.
const STOP_POLL_SLICE: Duration = Duration::from_millis(250);

/// Env override (milliseconds) for [`MANAGER_TICK`].
const MANAGER_TICK_ENV: &str = "SHELBI_WIKI_MANAGER_TICK_MS";

fn manager_tick() -> Duration {
    match std::env::var(MANAGER_TICK_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => MANAGER_TICK,
    }
}

/// Spawn the wiki manager thread. It exits when `stop` is set, stopping
/// every wiki service on the way out.
pub(super) fn spawn_wiki_manager(stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("shelbi-wiki-mgr".into())
        .spawn(move || {
            let tick = manager_tick();
            while !stop.load(Ordering::SeqCst) {
                let open = shelbi_state::list_open_projects().unwrap_or_default();
                shelbi_orchestrator::wiki::reconcile(&open);
                let mut waited = Duration::ZERO;
                while waited < tick && !stop.load(Ordering::SeqCst) {
                    thread::sleep(STOP_POLL_SLICE);
                    waited += STOP_POLL_SLICE;
                }
            }
            shelbi_orchestrator::wiki::stop_all();
        })
        .ok();
}
