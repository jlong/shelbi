//! Background reachability probing for the machines view.
//!
//! The machines view ([`crate::machines`]) shows each remote machine's recorded
//! `shelbi` binary path/version — but that is a durability-level fact from
//! `rt-machine-setup`; it says nothing about whether the host is answering *right
//! now*. The old tmux `while true; shelbi workspace list; sleep 5` loop probed
//! remote panes over SSH every few seconds, so a box going down was visible
//! there and isn't in the native view. This module restores that live signal
//! without ever putting an SSH call on the render path.
//!
//! [`ReachabilityProber`] owns a background thread that probes each declared
//! remote on a modest cadence (default 30s via [`REACHABILITY_CADENCE`]), backs
//! off geometrically on failure up to a cap, and publishes the latest
//! per-machine [`Reachability`] into a shared map. The view folds that map in on
//! each refresh ([`crate::machines::MachinesApp::apply_data`]); the probe itself
//! runs only on this thread, never on the UI thread or the shell's refresh
//! worker, so neither a slow SSH connect nor a wedged host can stall rendering.
//!
//! The probe is injected as a closure, so the standalone `__machines` process
//! and the in-process shell both drive the real SSH probe while tests supply a
//! deterministic stub.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use shelbi_orchestrator::machine::Reachability;

/// Default successful-probe cadence: re-check a healthy remote every 30s,
/// matching the feel of the old `workspace list` shell loop without a per-frame
/// SSH call.
pub const REACHABILITY_CADENCE: Duration = Duration::from_secs(30);

/// Upper bound the per-host interval backs off to after repeated failures, so a
/// box that stays down is re-checked every ~5 minutes rather than every 30s (and
/// a recovery is still noticed within that window).
pub const REACHABILITY_BACKOFF_MAX: Duration = Duration::from_secs(300);

/// The probe closure: given `(machine_name, ssh_host)` it answers whether the
/// host is reachable. Runs on the prober thread only.
type ProbeFn = dyn Fn(&str, &str) -> Reachability + Send + Sync;

/// The next polling interval for a host given its current interval and the
/// latest probe result. A success resets to `cadence`; a failure doubles the
/// current interval (saturating) up to `max`. Split out as a pure function so
/// the backoff shape is unit-testable without threads or sleeping.
fn next_interval(current: Option<Duration>, cadence: Duration, max: Duration, result: &Reachability) -> Duration {
    match result {
        Reachability::Reachable => cadence,
        // Unknown never reaches the store path, but treat it like a failure for
        // totality.
        Reachability::Unreachable { .. } | Reachability::Unknown => {
            current.unwrap_or(cadence).saturating_mul(2).min(max)
        }
    }
}

/// Shared state between the public handle and the worker thread.
struct Shared {
    state: Mutex<State>,
    cvar: Condvar,
    probe: Box<ProbeFn>,
    cadence: Duration,
    backoff_max: Duration,
    stop: AtomicBool,
}

#[derive(Default)]
struct State {
    /// The remotes to probe, as `(machine_name, ssh_host)`. Replaced wholesale
    /// by [`ReachabilityProber::set_targets`].
    targets: Vec<(String, String)>,
    /// Latest probe result per machine name.
    results: HashMap<String, Reachability>,
    /// When each machine is next due for a probe. A machine with no entry is due
    /// immediately (a freshly declared target probes at once).
    next_due: HashMap<String, Instant>,
    /// Current backoff interval per machine.
    interval: HashMap<String, Duration>,
}

/// A handle to a running reachability prober. Dropping it stops the worker and
/// joins its thread.
pub struct ReachabilityProber {
    shared: Arc<Shared>,
    join: Option<JoinHandle<()>>,
}

impl ReachabilityProber {
    /// Spawn a prober. `probe` runs on the worker thread for each due target;
    /// `cadence` is the healthy re-check interval and `backoff_max` caps the
    /// failure backoff.
    pub fn spawn<F>(cadence: Duration, backoff_max: Duration, probe: F) -> Self
    where
        F: Fn(&str, &str) -> Reachability + Send + Sync + 'static,
    {
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            cvar: Condvar::new(),
            probe: Box::new(probe),
            cadence,
            backoff_max,
            stop: AtomicBool::new(false),
        });
        let worker = Arc::clone(&shared);
        let join = std::thread::Builder::new()
            .name("shelbi-reachability".to_string())
            .spawn(move || run(&worker))
            .expect("spawn reachability prober thread");
        ReachabilityProber {
            shared,
            join: Some(join),
        }
    }

    /// Replace the set of remotes to probe. A no-op (no wakeup) when the set is
    /// unchanged, so the view can call this on every refresh cheaply; a genuine
    /// change wakes the worker so a newly declared remote is probed at once.
    pub fn set_targets(&self, targets: Vec<(String, String)>) {
        {
            let mut st = lock(&self.shared.state);
            if st.targets == targets {
                return;
            }
            st.targets = targets;
        }
        self.shared.cvar.notify_all();
    }

    /// The latest per-machine reachability. Cheap, never blocks on SSH — just a
    /// clone of the in-memory map.
    pub fn snapshot(&self) -> HashMap<String, Reachability> {
        lock(&self.shared.state).results.clone()
    }

    /// Stop the worker and join its thread. Idempotent; also runs on drop.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.cvar.notify_all();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

impl Drop for ReachabilityProber {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Recover a mutex guard even if a prior holder panicked — the state has no
/// invariant that a panic can break, so proceeding on the poisoned value is
/// correct and keeps a single panic from wedging the prober.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Drop bookkeeping for machines no longer targeted, so a removed remote stops
/// showing a stale reachability and its backoff is forgotten.
fn prune(st: &mut State) {
    let live: HashSet<&str> = st.targets.iter().map(|(m, _)| m.as_str()).collect();
    let live: HashSet<String> = live.into_iter().map(str::to_string).collect();
    st.results.retain(|m, _| live.contains(m));
    st.next_due.retain(|m, _| live.contains(m));
    st.interval.retain(|m, _| live.contains(m));
}

/// Collect the targets due at `now` (next_due in the past, or never scheduled).
fn collect_due(st: &State, now: Instant) -> Vec<(String, String)> {
    st.targets
        .iter()
        .filter(|(m, _)| st.next_due.get(m).map(|due| *due <= now).unwrap_or(true))
        .cloned()
        .collect()
}

/// How long to sleep when nothing is due: until the soonest scheduled probe,
/// capped at `cadence` so a change notification is never waited out for long.
fn wait_until_next(st: &State, now: Instant, cadence: Duration) -> Duration {
    st.targets
        .iter()
        .filter_map(|(m, _)| st.next_due.get(m))
        .map(|due| due.saturating_duration_since(now))
        .min()
        .map(|d| d.min(cadence))
        .unwrap_or(cadence)
}

fn run(shared: &Shared) {
    loop {
        let due = {
            let mut st = lock(&shared.state);
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            prune(&mut st);
            let now = Instant::now();
            let due = collect_due(&st, now);
            if due.is_empty() {
                let wait = wait_until_next(&st, now, shared.cadence);
                // Wait releases the lock; it returns on timeout, a target change,
                // or stop. We re-loop and recompute either way.
                let _ = shared.cvar.wait_timeout(st, wait);
                continue;
            }
            due
        };

        // Probe outside the lock so a slow/wedged SSH connect never blocks
        // set_targets / snapshot on the UI thread.
        for (machine, host) in due {
            let result = (shared.probe)(&machine, &host);
            let mut st = lock(&shared.state);
            if shared.stop.load(Ordering::Acquire) {
                return;
            }
            // The target may have been removed while we probed; only record a
            // result for one still declared.
            if !st.targets.iter().any(|(m, _)| *m == machine) {
                continue;
            }
            let interval = next_interval(
                st.interval.get(&machine).copied(),
                shared.cadence,
                shared.backoff_max,
                &result,
            );
            st.interval.insert(machine.clone(), interval);
            st.next_due.insert(machine.clone(), Instant::now() + interval);
            st.results.insert(machine, result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// Poll `f` until it returns `Some`, or panic after `timeout`.
    fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> T {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(v) = f() {
                return v;
            }
            if Instant::now() >= deadline {
                panic!("condition not met within {timeout:?}");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn next_interval_resets_on_success_and_backs_off_on_failure() {
        let cadence = Duration::from_secs(30);
        let max = Duration::from_secs(300);
        let down = Reachability::Unreachable { error: String::new() };
        // First failure doubles the cadence, each subsequent one doubles again
        // up to the cap.
        assert_eq!(next_interval(None, cadence, max, &down), Duration::from_secs(60));
        assert_eq!(
            next_interval(Some(Duration::from_secs(60)), cadence, max, &down),
            Duration::from_secs(120)
        );
        assert_eq!(
            next_interval(Some(Duration::from_secs(240)), cadence, max, &down),
            Duration::from_secs(300),
            "caps at backoff_max"
        );
        // A success snaps straight back to the healthy cadence.
        assert_eq!(
            next_interval(Some(Duration::from_secs(300)), cadence, max, &Reachability::Reachable),
            cadence
        );
    }

    #[test]
    fn probes_a_down_host_and_recovers_when_it_comes_back() {
        // A stubbed probe backed by a flag: down until `up` flips true.
        let up = Arc::new(AtomicBool::new(false));
        let up_probe = Arc::clone(&up);
        let prober = ReachabilityProber::spawn(
            Duration::from_millis(20),
            Duration::from_millis(80),
            move |_machine, _host| {
                if up_probe.load(Ordering::Acquire) {
                    Reachability::Reachable
                } else {
                    Reachability::Unreachable {
                        error: "connection refused".to_string(),
                    }
                }
            },
        );
        prober.set_targets(vec![("gpu".to_string(), "gpu.local".to_string())]);

        // First it reports the host down, with the error.
        wait_for(Duration::from_secs(2), || match prober.snapshot().get("gpu") {
            Some(Reachability::Unreachable { error }) => {
                assert!(error.contains("refused"));
                Some(())
            }
            _ => None,
        });

        // When the host comes back, the next probe recovers it to reachable.
        up.store(true, Ordering::Release);
        wait_for(Duration::from_secs(2), || {
            matches!(prober.snapshot().get("gpu"), Some(Reachability::Reachable)).then_some(())
        });
    }

    #[test]
    fn set_targets_and_snapshot_never_block_on_a_slow_probe() {
        // A probe that takes 150ms must not make set_targets/snapshot wait on it:
        // they only touch the in-memory state, never the SSH call.
        let prober = ReachabilityProber::spawn(
            Duration::from_millis(20),
            Duration::from_millis(80),
            |_m, _h| {
                std::thread::sleep(Duration::from_millis(150));
                Reachability::Reachable
            },
        );
        let t0 = Instant::now();
        prober.set_targets(vec![("gpu".to_string(), "gpu.local".to_string())]);
        let _ = prober.snapshot();
        assert!(
            t0.elapsed() < Duration::from_millis(50),
            "set_targets + snapshot must not block on the probe, took {:?}",
            t0.elapsed()
        );
    }

    #[test]
    fn only_declared_targets_are_probed() {
        // The prober probes exactly the targets it's given — the machines view is
        // responsible for excluding locals before calling set_targets, and a
        // prober with no targets does no probing at all.
        let calls = Arc::new(AtomicUsize::new(0));
        let probe_calls = Arc::clone(&calls);
        let prober = ReachabilityProber::spawn(
            Duration::from_millis(20),
            Duration::from_millis(80),
            move |_m, _h| {
                probe_calls.fetch_add(1, Ordering::AcqRel);
                Reachability::Reachable
            },
        );
        // No targets: nothing is probed.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(calls.load(Ordering::Acquire), 0, "no targets => no probes");

        // Declare one: it gets probed.
        prober.set_targets(vec![("gpu".to_string(), "gpu.local".to_string())]);
        wait_for(Duration::from_secs(2), || {
            (calls.load(Ordering::Acquire) > 0).then_some(())
        });
    }
}
