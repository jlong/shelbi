//! Per-project poller manager (Phase 3 of the remove-tmux effort,
//! `rt-daemon-poller`; design: `docs/removing-tmux/phase3-daemon.md`).
//!
//! The daemon is hub-global, but a sidebar was per-project. This manager runs
//! one [`WorkspacePoller`] per open project, following the precedent of the
//! board-refresh manager ([`super::board::spawn_refresh_manager`]): on an
//! interval it reconciles the running pollers against
//! [`shelbi_state::list_open_projects`], starting a poller for a newly opened
//! project and stopping the one for a project that has closed.
//!
//! It runs only while the hidden `SHELBI_DAEMON_POLLER` dev setting is on. With
//! the setting off (the default), the sidebar owns the poller and this manager
//! keeps none — so the two never run at once. The per-project poller lock
//! ([`shelbi_state::acquire_poller_lock`], taken inside `WorkspacePoller::start`)
//! is the hard backstop: even if a stale sidebar overlaps the switch, only the
//! lock holder polls, and this manager retries an inert start on its next tick.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use shelbi_orchestrator::poller::WorkspacePoller;

/// How often the manager wakes to reconcile running pollers against the open
/// set. Matches the board refresh manager's cadence so a freshly opened project
/// gets a poller within a couple of seconds.
const MANAGER_TICK: Duration = Duration::from_secs(2);

/// Slice the manager sleeps in so a stop signal is noticed within ~250ms rather
/// than up to a full [`MANAGER_TICK`]. Overridable (ms) so tests drive it fast.
const STOP_POLL_SLICE: Duration = Duration::from_millis(250);

/// Env override (milliseconds) for [`MANAGER_TICK`], so a test sees a reconcile
/// within a tick or two instead of waiting the production cadence.
const MANAGER_TICK_ENV: &str = "SHELBI_POLLER_MANAGER_TICK_MS";

fn manager_tick() -> Duration {
    match std::env::var(MANAGER_TICK_ENV).ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => MANAGER_TICK,
    }
}

/// Spawn the per-project poller manager thread. Exits when the shared stop flag
/// is set (the same flag the accept loop and the other managers watch, so one
/// signal stops them all); dropping the pollers on the way out joins their
/// threads and releases their per-project locks.
pub(super) fn spawn_poller_manager(stop: Arc<AtomicBool>) {
    thread::Builder::new()
        .name("shelbi-poller-mgr".into())
        .spawn(move || poller_manager_loop(&stop))
        .ok();
}

fn poller_manager_loop(stop: &AtomicBool) {
    let tick = manager_tick();
    let mut running: HashMap<String, WorkspacePoller> = HashMap::new();
    while !stop.load(Ordering::SeqCst) {
        reconcile(&mut running);
        sleep_until_stop(tick, stop);
    }
    // On stop, drop every poller; each `Drop` asks its thread to exit, joins it,
    // and releases the per-project lock.
    running.clear();
}

/// Reconcile the running pollers against the open set for one tick. Split from
/// the loop so a test can drive a single reconcile without threads or timing.
fn reconcile(running: &mut HashMap<String, WorkspacePoller>) {
    // The daemon owns the poller only while the dev setting is on. Off (the
    // default): the sidebar polls, so the daemon keeps none and tears down any
    // it was running if the setting was just flipped off.
    if !shelbi_state::daemon_poller_enabled() {
        running.clear();
        return;
    }

    let open = shelbi_state::list_open_projects().unwrap_or_default();

    // Stop pollers for projects that have closed (their `Drop` joins + unlocks).
    running.retain(|project, _| open.iter().any(|p| p == project));

    // Start a poller for each open project that isn't already running. A start
    // that comes back inert (the per-project lock is still held by a stale
    // sidebar) is dropped so the next tick retries, rather than cached as a
    // dead entry that would never poll.
    for project in &open {
        if running.contains_key(project) {
            continue;
        }
        let poller = WorkspacePoller::start(project.clone());
        if poller.is_active() {
            running.insert(project.clone(), poller);
        }
    }
}

/// Sleep up to `total`, waking early (within [`STOP_POLL_SLICE`]) when `stop`
/// is set — the manager's responsiveness to SIGTERM.
fn sleep_until_stop(total: Duration, stop: &AtomicBool) {
    let mut waited = Duration::ZERO;
    while waited < total && !stop.load(Ordering::SeqCst) {
        thread::sleep(STOP_POLL_SLICE);
        waited += STOP_POLL_SLICE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Point `$SHELBI_HOME` at a fresh temp dir and turn the daemon-poller
    /// setting on for the test, restoring both on drop. Holds the shared env
    /// lock because `set_var` is process-global.
    struct DaemonPollerHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev_home: Option<std::ffi::OsString>,
        prev_setting: Option<std::ffi::OsString>,
        home: PathBuf,
    }
    impl DaemonPollerHome {
        fn new(tag: &str) -> Self {
            let lock = crate::commands::test_support::ENV_LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-poller-mgr-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(home.join("projects")).unwrap();
            let prev_home = std::env::var_os("SHELBI_HOME");
            let prev_setting = std::env::var_os(shelbi_state::DAEMON_POLLER_ENV);
            std::env::set_var("SHELBI_HOME", &home);
            std::env::set_var(shelbi_state::DAEMON_POLLER_ENV, "1");
            Self {
                _lock: lock,
                prev_home,
                prev_setting,
                home,
            }
        }
        /// Register a project (so `list_open_projects` sees it) and mark it open.
        /// A minimal `file_system` project YAML is enough: the manager only keys
        /// off the registered-and-open set, and a poller started for it idles
        /// harmlessly against the fixture workspace.
        fn open_project(&self, name: &str) {
            std::fs::write(
                self.home.join("projects").join(format!("{name}.yaml")),
                format!(
                    "name: {name}\nrepo: /tmp/{name}\ndefault_branch: main\n\
                     orchestrator:\n  runner: claude\n\
                     agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                     machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{name}\n\
                     workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n"
                ),
            )
            .unwrap();
            shelbi_state::set_project_open(name, true).unwrap();
        }
    }
    impl Drop for DaemonPollerHome {
        fn drop(&mut self) {
            match self.prev_home.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            match self.prev_setting.take() {
                Some(v) => std::env::set_var(shelbi_state::DAEMON_POLLER_ENV, v),
                None => std::env::remove_var(shelbi_state::DAEMON_POLLER_ENV),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn reconcile_starts_a_poller_per_open_project_and_stops_closed_ones() {
        let home = DaemonPollerHome::new("lifecycle");
        home.open_project("alpha");
        home.open_project("beta");

        let mut running: HashMap<String, WorkspacePoller> = HashMap::new();
        reconcile(&mut running);
        let mut names: Vec<&String> = running.keys().collect();
        names.sort();
        assert_eq!(names, vec!["alpha", "beta"], "one poller per open project");
        assert!(running.values().all(|p| p.is_active()), "both pollers are active");

        // Close beta; the next reconcile stops its poller without restarting the
        // daemon and without disturbing alpha's.
        shelbi_state::set_project_open("beta", false).unwrap();
        reconcile(&mut running);
        let names: Vec<&String> = running.keys().collect();
        assert_eq!(names, vec![&"alpha".to_string()], "closed project's poller stopped");
    }

    #[test]
    fn with_the_setting_off_the_manager_runs_no_pollers() {
        let home = DaemonPollerHome::new("off");
        home.open_project("alpha");
        // Flip the setting off: the daemon must run none (the sidebar owns it).
        std::env::set_var(shelbi_state::DAEMON_POLLER_ENV, "0");

        let mut running: HashMap<String, WorkspacePoller> = HashMap::new();
        reconcile(&mut running);
        assert!(running.is_empty(), "setting off → daemon runs no pollers");
    }

    #[test]
    fn two_pollers_never_run_for_one_project_at_once() {
        let home = DaemonPollerHome::new("exclusion");
        home.open_project("alpha");

        // The manager starts alpha's poller and holds its lock.
        let mut running: HashMap<String, WorkspacePoller> = HashMap::new();
        reconcile(&mut running);
        assert!(running["alpha"].is_active());

        // A second poller for the same project — a stale sidebar — can't run
        // while the manager holds the lock: its handle comes back inert.
        let stale = WorkspacePoller::start("alpha");
        assert!(!stale.is_active(), "the lock blocks a second concurrent poller");
    }
}
