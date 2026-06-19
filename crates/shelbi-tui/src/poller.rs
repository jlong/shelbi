//! Background worker-state poller. Lives in the sidebar process and is
//! the only place the hub talks to worker panes for observability.
//!
//! Cadence: per-project `worker_poll_interval_secs` (default 5s). Each
//! tick the poller iterates the project's declared workers, asks tmux
//! for each pane's title (`display-message -p '#{pane_title}'`, routed
//! over SSH for remote machines via shelbi-ssh — which sets up
//! ControlMaster so the marginal cost per poll is a socket write, not a
//! TCP handshake), and parses the trailing `shelbi:<state>` marker.
//!
//! On a state change the poller writes two files:
//! - `~/.shelbi/workers/<name>/status.yaml` — last observed state.
//! - `~/.shelbi/events.log` — append-only transition history.
//!
//! On a same-state observation it still bumps `last_seen` in
//! `status.yaml` so the UI can tell stale from fresh observations.
//!
//! All authoritative state stays on the hub — workers themselves only
//! emit the markers via their `.claude/settings.json` hooks.

use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use chrono::{DateTime, Utc};

use shelbi_core::{Column, Project};
use shelbi_state::{
    append_worker_event, load_worker_status, parse_pane_title_marker, save_worker_status,
    PaneMarker, WorkerState, WorkerStatus,
};

/// Spawned poller handle. Dropping it asks the thread to exit and joins it.
pub struct WorkerPoller {
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorkerPoller {
    pub fn start(project_name: impl Into<String>) -> Self {
        let project_name = project_name.into();
        let shutdown = Arc::new(AtomicBool::new(false));
        let shutdown_clone = shutdown.clone();
        let handle = thread::Builder::new()
            .name(format!("shelbi-poller-{project_name}"))
            .spawn(move || run_poller_loop(project_name, shutdown_clone))
            .ok();
        Self { shutdown, handle }
    }
}

impl Drop for WorkerPoller {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

fn run_poller_loop(project_name: String, shutdown: Arc<AtomicBool>) {
    // In-memory mirror of each worker's last persisted state so we can
    // detect transitions without hitting disk every tick. Seeded from
    // status.yaml on first observation so a hub restart doesn't emit a
    // bogus `none -> X` event for state we already recorded.
    let mut last_known: HashMap<String, WorkerState> = HashMap::new();

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let project = match shelbi_state::load_project(&project_name) {
            Ok(p) => p,
            // YAML missing or malformed — back off and retry. Re-loading
            // every tick means the user can edit the project file and
            // the poller picks up the change without a restart.
            Err(_) => {
                sleep_interruptible(Duration::from_secs(5), &shutdown);
                continue;
            }
        };
        let interval = Duration::from_secs(project.worker_poll_interval_secs.max(1));

        for worker in &project.workers {
            if shutdown.load(Ordering::SeqCst) {
                return;
            }
            poll_one(&project, worker, &mut last_known);
        }

        sleep_interruptible(interval, &shutdown);
    }
}

fn poll_one(
    project: &Project,
    worker: &shelbi_core::WorkerSpec,
    last_known: &mut HashMap<String, WorkerState>,
) {
    let Some(machine) = project.machine(&worker.machine) else {
        return;
    };
    let host = machine.host();
    let Ok(addr) = shelbi_orchestrator::worker::worker_tmux_addr(project, worker) else {
        return;
    };

    // No pane → no marker. The display-message call would fail anyway,
    // but checking up-front keeps stderr noise out of the log.
    if !shelbi_orchestrator::worker::worker_pane_alive(&host, &addr).unwrap_or(false) {
        return;
    }

    let title = match shelbi_tmux::pane_title(&host, &addr) {
        Ok(t) => t,
        Err(_) => return,
    };
    let Some(marker) = parse_pane_title_marker(&title) else {
        return;
    };
    let new_state = marker.worker_state();

    // Bootstrap previous state from disk on first sighting so a hub
    // restart doesn't emit a bogus `none -> X` event for state we've
    // already recorded.
    let prior = match last_known.get(&worker.name).copied() {
        Some(s) => Some(PriorState {
            state: s,
            last_transition: load_worker_status(&worker.name)
                .ok()
                .flatten()
                .map(|s| s.last_transition),
        }),
        None => load_worker_status(&worker.name)
            .ok()
            .flatten()
            .map(|s| PriorState {
                state: s.state,
                last_transition: Some(s.last_transition),
            }),
    };

    let current_task = current_task_for(project, &worker.name);
    let outcome = decide(
        &worker.name,
        current_task.clone(),
        prior,
        new_state,
        Utc::now(),
    );

    if let Err(e) = save_worker_status(&outcome.status) {
        tracing::warn!(worker = %worker.name, error = %e, "save_worker_status failed");
    }
    if outcome.transitioned {
        if let Err(e) = append_worker_event(
            &worker.name,
            outcome.prev_state,
            outcome.status.state,
        ) {
            tracing::warn!(worker = %worker.name, error = %e, "append_worker_event failed");
        }
    }

    // `shelbi:review` is the worker's explicit "ready for review" handoff
    // (distinct from `shelbi:idle`, which fires on every claude turn
    // end). When we see it, move the worker's in-progress task into the
    // review column — the same effect `shelbi task move <id> --to review`
    // would have had, except shelbi isn't installed on remote workers.
    //
    // Idempotent: `current_task_for` only finds tasks still in
    // InProgress, so once the move happens, subsequent observations of
    // `shelbi:review` produce no task and we no-op. `move_task` is also
    // itself a no-op when the column is unchanged.
    if marker == PaneMarker::Review {
        if let Some(task_id) = &current_task {
            if let Err(e) = shelbi_state::move_task(&project.name, task_id, Column::Review) {
                tracing::warn!(
                    worker = %worker.name,
                    task = %task_id,
                    error = %e,
                    "review handoff: move_task failed",
                );
            }
        }
    }

    last_known.insert(worker.name.clone(), outcome.status.state);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorState {
    state: WorkerState,
    last_transition: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct PollOutcome {
    transitioned: bool,
    prev_state: Option<WorkerState>,
    status: WorkerStatus,
}

/// Pure transition decision: given the previous state (if any) and a
/// fresh observation, build the [`WorkerStatus`] to persist and decide
/// whether the change deserves an `events.log` line.
fn decide(
    worker: &str,
    current_task: Option<String>,
    prior: Option<PriorState>,
    new_state: WorkerState,
    now: DateTime<Utc>,
) -> PollOutcome {
    let prev_state = prior.map(|p| p.state);
    let transitioned = match prev_state {
        Some(p) => p != new_state,
        None => true,
    };
    // Preserve the existing transition timestamp across same-state
    // polls — only `last_seen` should move when nothing changed.
    let last_transition = if transitioned {
        now
    } else {
        prior.and_then(|p| p.last_transition).unwrap_or(now)
    };
    PollOutcome {
        transitioned,
        prev_state,
        status: WorkerStatus {
            worker: worker.to_string(),
            current_task,
            state: new_state,
            last_transition,
            last_seen: now,
        },
    }
}

/// In-progress task currently assigned to `worker`, if any. Cheap (one
/// task-dir scan); called once per worker per poll tick.
fn current_task_for(project: &Project, worker_name: &str) -> Option<String> {
    shelbi_state::list_column(&project.name, Column::InProgress)
        .ok()?
        .into_iter()
        .find(|tf| tf.task.assigned_to.as_deref() == Some(worker_name))
        .map(|tf| tf.task.id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    #[test]
    fn first_observation_is_a_transition_from_none() {
        let out = decide("alpha", None, None, WorkerState::Working, ts(100));
        assert!(out.transitioned);
        assert!(out.prev_state.is_none());
        assert_eq!(out.status.state, WorkerState::Working);
        assert_eq!(out.status.last_transition, ts(100));
        assert_eq!(out.status.last_seen, ts(100));
    }

    #[test]
    fn same_state_observation_bumps_last_seen_only() {
        // Prior state already on disk from an earlier transition. Same
        // marker on the next poll: keep `last_transition` put, bump
        // `last_seen` to now.
        let prior = Some(PriorState {
            state: WorkerState::Working,
            last_transition: Some(ts(50)),
        });
        let out = decide("alpha", None, prior, WorkerState::Working, ts(120));
        assert!(!out.transitioned);
        assert_eq!(out.status.last_transition, ts(50));
        assert_eq!(out.status.last_seen, ts(120));
    }

    #[test]
    fn state_change_records_previous_and_resets_transition() {
        // Working → AwaitingInput → Blocked.
        let prior = Some(PriorState {
            state: WorkerState::Working,
            last_transition: Some(ts(50)),
        });
        let out = decide(
            "alpha",
            Some("task-1".into()),
            prior,
            WorkerState::AwaitingInput,
            ts(200),
        );
        assert!(out.transitioned);
        assert_eq!(out.prev_state, Some(WorkerState::Working));
        assert_eq!(out.status.state, WorkerState::AwaitingInput);
        assert_eq!(out.status.last_transition, ts(200));
        assert_eq!(out.status.current_task.as_deref(), Some("task-1"));
    }
}

/// Sleep for `dur`, but wake every 200ms to honor a shutdown request so
/// the sidebar process exits promptly.
fn sleep_interruptible(dur: Duration, shutdown: &Arc<AtomicBool>) {
    let step = Duration::from_millis(200);
    let mut elapsed = Duration::ZERO;
    while elapsed < dur {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }
        let chunk = step.min(dur - elapsed);
        thread::sleep(chunk);
        elapsed += chunk;
    }
}
