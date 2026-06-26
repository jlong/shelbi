//! Background worker-state poller. Lives in the sidebar process and is
//! the only place the hub talks to worker panes for observability.
//!
//! Cadence: per-project `worker_poll_interval_secs` (default 5s). The
//! poller spawns ONE thread per declared worker — each running its own
//! independent poll loop — so a hung SSH call to one machine (unreachable
//! host, expired Tailscale auth, ProxyJump timeout) only freezes that
//! worker's thread, never the others. Earlier versions used a single
//! sequential loop, which would block every local-worker poll behind a
//! single stuck remote-worker SSH call and silently freeze the review
//! marker handoff for hours at a time.
//!
//! Each cycle asks tmux for the worker pane's title
//! (`display-message -p '#{pane_title}'`, routed over SSH for remote
//! machines via shelbi-ssh — which sets up ControlMaster so the marginal
//! cost per poll is a socket write, not a TCP handshake) and parses the
//! trailing `shelbi:<state>` marker. The marker file at
//! `<worktree>/.claude/shelbi-review-ready` is checked first, before any
//! pane operation, so the review handoff isn't gated on a working pane
//! title (Claude's own OSC writes often clobber the marker before the
//! poller sees it).
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
use std::net::{SocketAddr, TcpStream};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

use chrono::{DateTime, Utc};

use shelbi_core::{Column, Project};
use shelbi_state::{
    append_contextstore_event, append_heartbeat_event, append_worker_event, events_log_path,
    load_worker_status, parse_pane_title_marker, save_worker_status, PaneMarker, WorkerState,
    WorkerStatus,
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

/// Supervisor: spawns one persistent poll thread per worker declared in
/// the project YAML, then sleeps until shutdown. Each per-worker thread
/// owns its own SSH/IO calls, so a hung remote worker only blocks its
/// own thread — local workers keep polling on cadence.
///
/// We re-check the workers list every supervisor tick (5s) so that
/// workers added to the YAML at runtime get a thread spawned without a
/// hub restart. Removed workers' threads exit themselves when they
/// can't find their name in the YAML anymore.
fn run_poller_loop(project_name: String, shutdown: Arc<AtomicBool>) {
    let mut spawned: HashMap<String, JoinHandle<()>> = HashMap::new();

    // Heartbeat schedule. We seed `next_heartbeat_attempt` from "now"
    // rather than from the last events.log mtime so a poller restart
    // mid-interval doesn't immediately fire a heartbeat that was
    // technically "due" before the crash — the spec is one interval
    // from poller start, not from the missed slot.
    let mut next_heartbeat_attempt: Option<Instant> = None;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let project = match shelbi_state::load_project(&project_name) {
            Ok(p) => p,
            // YAML missing or malformed — back off and retry. Re-loading
            // every tick means the user can edit the project file and
            // new workers get threads spawned without a restart.
            Err(_) => {
                sleep_interruptible(Duration::from_secs(5), &shutdown);
                continue;
            }
        };

        for worker in &project.workers {
            // Drop dead-thread handles so a panic in poll_one for this
            // worker doesn't leave it un-respawned. (Per-worker threads
            // shouldn't normally panic — poll_one swallows errors — but
            // defense-in-depth.)
            if spawned.get(&worker.name).is_some_and(|h| h.is_finished()) {
                if let Some(h) = spawned.remove(&worker.name) {
                    let _ = h.join();
                }
            }
            if spawned.contains_key(&worker.name) {
                continue;
            }
            let worker_name = worker.name.clone();
            let project_name = project_name.clone();
            let shutdown_clone = shutdown.clone();
            let handle = thread::Builder::new()
                .name(format!("shelbi-poller-{worker_name}"))
                .spawn(move || run_worker_poll_loop(project_name, worker_name, shutdown_clone))
                .ok();
            if let Some(h) = handle {
                spawned.insert(worker.name.clone(), h);
            }
        }

        // Heartbeat is project-wide (one per project, not per worker),
        // so it lives on the supervisor rather than inside any per-worker
        // thread. The function is a no-op if heartbeat is off or not yet
        // due, and debounces against any other events.log activity.
        // `online_probe` is the connectivity gate: while the box is
        // offline the heartbeat skips silently so the feed doesn't fill
        // with no-op lines during a network drop.
        maybe_emit_heartbeat(&project, &mut next_heartbeat_attempt, online_probe);

        // Supervisor cadence is fixed at 5s — independent of
        // `worker_poll_interval_secs`, which governs each per-worker
        // loop. Cheap (one YAML reload + map lookup per tick) and fast
        // enough that a YAML edit gets new threads within ~5s.
        sleep_interruptible(Duration::from_secs(5), &shutdown);
    }

    // Drain finished threads on shutdown. Threads stuck on a hung SSH
    // call won't be joined here — they hold no resources we care about
    // and the OS reaps them when the sidebar process exits.
    for (_, h) in spawned.drain() {
        if h.is_finished() {
            let _ = h.join();
        }
    }
}

/// One worker's persistent poll loop. Drives [`poll_one`] every
/// `worker_poll_interval_secs`, reloading the project YAML each cycle so
/// the user can edit the worker list / interval without a hub restart.
/// Exits cleanly when shutdown is requested OR the worker is removed
/// from the YAML.
fn run_worker_poll_loop(
    project_name: String,
    worker_name: String,
    shutdown: Arc<AtomicBool>,
) {
    // Each worker thread keeps its own `last_known` so it doesn't need
    // to share a Mutex with the supervisor or its peers. Seeded from
    // status.yaml on first observation (handled inside `poll_one`).
    let mut last_known: Option<WorkerState> = None;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let project = match shelbi_state::load_project(&project_name) {
            Ok(p) => p,
            Err(_) => {
                sleep_interruptible(Duration::from_secs(5), &shutdown);
                continue;
            }
        };
        let Some(worker) = project.workers.iter().find(|w| w.name == worker_name) else {
            // Worker removed from the project YAML — exit this thread.
            // The supervisor will not respawn it because it's not in the
            // workers list any more.
            break;
        };
        let interval = Duration::from_secs(project.worker_poll_interval_secs.max(1));

        poll_one(&project, worker, &mut last_known);

        sleep_interruptible(interval, &shutdown);
    }
}

/// Consider emitting one heartbeat for `project`. Called once per poller
/// tick. Four rules from the spec land here:
///
/// 1. **Off** — `HeartbeatConfig::Off` skips every tick and also clears
///    any pending schedule so a project that toggles `heartbeat: off`
///    while the poller is running stops emitting immediately.
/// 2. **Crash-safe cadence** — the first attempt fires one
///    `heartbeat` interval after the poller observed the config (not
///    from the wall clock or the previous run's last write), so a
///    restart mid-interval doesn't catch up missed slots.
/// 3. **Debounced against other writes** — if any line was appended to
///    `events.log` within the last `interval`, skip this attempt.
///    Otherwise emit. Our own heartbeat counts as activity, so on a
///    fully quiet board the next emission lands exactly one interval
///    after the previous one.
/// 4. **Paused while offline** — if `is_online()` returns false at the
///    moment of emission, skip silently. The schedule still advances by
///    one interval so we don't probe (and pay its timeout) every
///    supervisor tick, and emission resumes naturally on the first
///    interval after connectivity is back. The "avoid noise" framing
///    is deliberate: a heartbeat during an offline window communicates
///    nothing — the only consumer (the orchestrator) can't act on it.
fn maybe_emit_heartbeat(
    project: &Project,
    next_attempt: &mut Option<Instant>,
    is_online: impl Fn() -> bool,
) {
    let Some(interval) = project.heartbeat.interval() else {
        // Heartbeat off — clear the schedule so flipping it back on
        // re-seeds from the next tick rather than firing a stale due.
        *next_attempt = None;
        return;
    };

    let now = Instant::now();
    let due = match *next_attempt {
        None => {
            // First tick after start (or after the config flipped from
            // off → on): schedule the first attempt one interval out.
            *next_attempt = Some(now + interval);
            return;
        }
        Some(t) => now >= t,
    };
    if !due {
        return;
    }

    // Debounce: skip if anything (us or anyone else) wrote to
    // events.log within the last interval. We read mtime fresh on each
    // attempt — cheap (one stat) and avoids tracking writes ourselves.
    let recent_activity = events_log_modified_within(interval);
    if !recent_activity && is_online() {
        if let Err(e) = append_heartbeat_event(&project.name) {
            tracing::warn!(
                project = %project.name,
                error = %e,
                "append_heartbeat_event failed",
            );
        }
    }

    *next_attempt = Some(now + interval);
}

/// Default connectivity probe used by the poller. TCP-connects to
/// `1.1.1.1:443` with a 1 s timeout — Cloudflare's anycast resolver, a
/// raw IP so we don't trip on local DNS being the first thing to die on
/// a captive portal. A successful connect (the TLS handshake never
/// runs) is enough signal that the box has an upstream route.
///
/// Runs at most once per `heartbeat` interval (default 3 min) on the
/// supervisor thread, so the 1 s blocking cost only matters when we're
/// already offline — the round-trip on a healthy connection is well
/// under 50 ms.
fn online_probe() -> bool {
    let addr: SocketAddr = ([1, 1, 1, 1], 443).into();
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

/// True iff `events.log` exists and was modified within the last
/// `window`. Missing log file → no activity. Any I/O hiccup → assume
/// active (safer to under-emit than spam the log on a transient stat
/// failure).
fn events_log_modified_within(window: Duration) -> bool {
    let path = match events_log_path() {
        Ok(p) => p,
        Err(_) => return true,
    };
    let mtime = match std::fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return false,
        Err(_) => return true,
    };
    match SystemTime::now().duration_since(mtime) {
        Ok(elapsed) => elapsed < window,
        // mtime is in the future (clock skew); treat as recent.
        Err(_) => true,
    }
}

fn poll_one(
    project: &Project,
    worker: &shelbi_core::WorkerSpec,
    last_known: &mut Option<WorkerState>,
) {
    let Some(machine) = project.machine(&worker.machine) else {
        return;
    };
    let host = machine.host();
    let Ok(addr) = shelbi_orchestrator::worker::worker_tmux_addr(project, worker) else {
        return;
    };

    // Review handoff is a file marker the worker writes when it's done, read
    // independently of the pane title. We check it *before* the pane-title
    // state below (and unconditionally, even if the pane has since died or
    // Claude has overwritten its title) so nothing the agent's UI does can
    // hide the signal.
    maybe_promote_to_review(project, worker, machine, &host);

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
    let prior = match *last_known {
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
            match shelbi_state::move_task(&project.name, task_id, Column::Review) {
                Ok(Some((from, to))) => {
                    if let Err(e) = shelbi_state::append_task_event(
                        task_id,
                        from,
                        to,
                        "worker:review-pane",
                    ) {
                        tracing::warn!(
                            worker = %worker.name,
                            task = %task_id,
                            error = %e,
                            "review handoff: append_task_event failed",
                        );
                    }
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(
                    worker = %worker.name,
                    task = %task_id,
                    error = %e,
                    "review handoff: move_task failed",
                ),
            }
        }
    }

    *last_known = Some(outcome.status.state);
}

/// Check the worker's review-ready file marker and, if present, move its
/// in-progress task to the review column. The marker is the worker's handoff
/// signal — it writes its task id into `<worktree>/.claude/shelbi-review-ready`
/// when done (see `shelbi_orchestrator::worker::worker_review_marker`).
///
/// Best-effort and idempotent: we consume the marker exactly once by clearing
/// it after a successful move, and `move_task` is a no-op once the task is
/// already in review, so a worker that keeps churning in its pane afterward
/// never gets pulled back out. A stale marker (worktree reused before the
/// previous one was cleared) names a task that's no longer in-progress for
/// this worker, so we clear it without moving anything.
fn maybe_promote_to_review(
    project: &Project,
    worker: &shelbi_core::WorkerSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
) {
    let marker = shelbi_orchestrator::worker::worker_review_marker(machine, worker);
    let task_id = match shelbi_orchestrator::worker::read_review_marker(host, &marker) {
        Ok(Some(id)) => id,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(worker = %worker.name, error = %e, "read_review_marker failed");
            return;
        }
    };

    // Capture the task body up front: we use it both for the column move
    // path (to gate sync on the ContextStore heuristic) and to know we
    // had a valid task at all. If the load fails or the task isn't ours
    // in-progress, we still fall through to clear the (stale) marker.
    let task_file = shelbi_state::load_task(&project.name, &task_id);

    match &task_file {
        Ok(tf)
            if tf.task.column == Column::InProgress
                && tf.task.assigned_to.as_deref() == Some(worker.name.as_str()) =>
        {
            match shelbi_state::move_task(&project.name, &task_id, Column::Review) {
                Ok(Some((from, to))) => {
                    if let Err(e) = shelbi_state::append_task_event(
                        &task_id,
                        from,
                        to,
                        "worker:review-marker",
                    ) {
                        tracing::warn!(worker = %worker.name, task = %task_id, error = %e, "append_task_event failed");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    // Leave the marker in place so we retry on the next tick.
                    tracing::warn!(worker = %worker.name, task = %task_id, error = %e, "move_task to review failed");
                    return;
                }
            }
            tracing::info!(worker = %worker.name, task = %task_id, "promoted task to review via marker");

            // Best-effort: pull any ContextStore writes the worker made
            // on its machine back to hub. Skipped silently when the
            // project has no `contextstore_sync` configured, when the
            // body doesn't trip the heuristic, or when the worker is
            // local. Failures log to events.log but never block the
            // promotion — that's the contract on this path.
            sync_contextstore_from_worker(project, machine, &tf.body);
        }
        Ok(_) => {
            tracing::debug!(worker = %worker.name, task = %task_id, "stale review marker (task not in-progress for this worker); clearing");
        }
        Err(e) => {
            tracing::warn!(worker = %worker.name, task = %task_id, error = %e, "review marker names unloadable task; clearing");
        }
    }

    if let Err(e) = shelbi_orchestrator::worker::clear_review_marker(host, &marker) {
        tracing::warn!(worker = %worker.name, error = %e, "clear_review_marker failed");
    }
}

/// Run the cross-machine ContextStore sync after a successful review
/// promotion and record one `events.log` line per attempted space.
///
/// Keeping this side-effecting wrapper next to the poller means the
/// `contextstore` module stays purely about the rsync mechanic — the
/// "where to log it" lives with the rest of the poller's event-logging
/// calls. Failures here are intentionally swallowed: the task is already
/// in review and the user can re-run sync manually if needed.
fn sync_contextstore_from_worker(
    project: &Project,
    machine: &shelbi_core::Machine,
    task_body: &str,
) {
    let outcomes =
        shelbi_orchestrator::contextstore::sync_after_review(project, machine, task_body);
    for outcome in outcomes {
        let status = outcome.status.label();
        let detail = outcome.status.detail();
        let detail_for_log = if detail.is_empty() {
            "-".to_string()
        } else {
            detail.clone()
        };
        if let Err(e) = append_contextstore_event(
            &outcome.space,
            &outcome.machine,
            status,
            &detail_for_log,
        ) {
            tracing::warn!(
                space = %outcome.space,
                machine = %outcome.machine,
                error = %e,
                "append_contextstore_event failed",
            );
        }
        match outcome.status {
            shelbi_orchestrator::contextstore::SyncStatus::Ok => {
                tracing::info!(
                    space = %outcome.space,
                    machine = %outcome.machine,
                    "contextstore synced from remote worker",
                );
            }
            shelbi_orchestrator::contextstore::SyncStatus::Failed { .. } => {
                tracing::warn!(
                    space = %outcome.space,
                    machine = %outcome.machine,
                    detail = %detail,
                    "contextstore sync failed",
                );
            }
            shelbi_orchestrator::contextstore::SyncStatus::SkippedLocal => {
                tracing::debug!(
                    space = %outcome.space,
                    machine = %outcome.machine,
                    "contextstore sync skipped — worker is local",
                );
            }
        }
    }
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

    use shelbi_core::{
        AgentRunnerSpec, Host, Machine, MachineKind, OrchestratorSpec, Task, WorkerSpec,
    };
    use std::collections::BTreeMap;

    /// A local-machine project with a single worker whose worktree lives
    /// under `work_dir`, so the marker path is a real writable local file.
    fn local_project(work_dir: &std::path::Path) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        Project {
            name: "demo".into(),
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: work_dir.to_path_buf(),
                host: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: vec![WorkerSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
            }],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    fn in_progress_task(id: &str, worker: &str) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: Column::InProgress,
            priority: 0,
            assigned_to: Some(worker.into()),
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn write_marker(project: &Project, body: &str) -> std::path::PathBuf {
        let marker = shelbi_orchestrator::worker::worker_review_marker(
            &project.machines[0],
            &project.workers[0],
        );
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, body).unwrap();
        marker
    }

    #[test]
    fn review_marker_promotes_in_progress_task_then_clears_itself() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-promote-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let project = local_project(&work_dir);
        shelbi_state::save_task("demo", &in_progress_task("fix-login", "alpha"), "body").unwrap();

        // Worker signals review by writing its task id into the marker.
        let marker = write_marker(&project, "fix-login\n");

        maybe_promote_to_review(&project, &project.workers[0], &project.machines[0], &Host::Local);

        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::Review,
            "task should be promoted to review"
        );
        assert!(!marker.exists(), "marker should be consumed (cleared)");

        // The promotion must also append a `task=...` line to events.log
        // tagged with the marker-driven reason, so `shelbi events tail`
        // surfaces the handoff as part of the canonical event stream.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let task_lines: Vec<&str> =
            log.lines().filter(|l| l.contains(" task=fix-login ")).collect();
        assert_eq!(task_lines.len(), 1, "log: {log:?}");
        assert!(
            task_lines[0].contains(" in_progress -> review "),
            "line: {}",
            task_lines[0]
        );
        assert!(
            task_lines[0].ends_with("reason=worker:review-marker"),
            "line: {}",
            task_lines[0]
        );

        // A leftover/stale marker naming a task that's no longer in-progress
        // for this worker is cleared without moving anything back out.
        let marker = write_marker(&project, "fix-login\n");
        maybe_promote_to_review(&project, &project.workers[0], &project.machines[0], &Host::Local);
        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::Review,
            "task already in review must not be pulled back out"
        );
        assert!(!marker.exists(), "stale marker should be cleared");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn promotion_with_contextstore_match_on_local_logs_skipped_event() {
        // Local-host worker — sync must short-circuit to SkippedLocal so
        // we don't shell out to rsync for files already on hub. Even on
        // the skip path we still log the decision: that's the contract
        // for surfacing what happened.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-cstore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        project
            .contextstore_sync
            .push(shelbi_core::ContextStoreSyncSpec {
                space: "Shelbi".into(),
                path: std::path::PathBuf::from("~/Documents/ContextStore/shelbi"),
            });
        // Body trips the heuristic via the `Shelbi/` substring.
        shelbi_state::save_task(
            "demo",
            &in_progress_task("write-notes", "alpha"),
            "Write Shelbi/Research/notes.md",
        )
        .unwrap();
        let _marker = write_marker(&project, "write-notes\n");

        maybe_promote_to_review(&project, &project.workers[0], &project.machines[0], &Host::Local);

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let cs_lines: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(" contextstore "))
            .collect();
        assert_eq!(cs_lines.len(), 1, "log: {log:?}");
        // Local worker = SkippedLocal status (`skipped-local`).
        assert!(
            cs_lines[0].contains("space=Shelbi"),
            "line: {}",
            cs_lines[0]
        );
        assert!(
            cs_lines[0].contains("status=skipped-local"),
            "line: {}",
            cs_lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn promotion_without_contextstore_heuristic_skips_sync_event() {
        // Body doesn't trip the heuristic — no `cstore` keyword, no
        // matching space path. The sync code should never run, so
        // events.log gets no `contextstore` line even though the project
        // is configured. Important: the heuristic exists precisely to
        // avoid rsync'ing for every single review handoff.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-cstore-nomatch-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        project
            .contextstore_sync
            .push(shelbi_core::ContextStoreSyncSpec {
                space: "Shelbi".into(),
                path: std::path::PathBuf::from("~/Documents/ContextStore/shelbi"),
            });
        shelbi_state::save_task(
            "demo",
            &in_progress_task("fix-login", "alpha"),
            "Fix the Safari SSO bug.",
        )
        .unwrap();
        let _marker = write_marker(&project, "fix-login\n");

        maybe_promote_to_review(&project, &project.workers[0], &project.machines[0], &Host::Local);

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(
            !log.contains(" contextstore "),
            "no cstore-touching body → no sync event; log: {log:?}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn absent_review_marker_is_a_noop() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-noop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let project = local_project(&work_dir);
        shelbi_state::save_task("demo", &in_progress_task("fix-login", "alpha"), "body").unwrap();

        // No marker on disk → task stays in progress.
        maybe_promote_to_review(&project, &project.workers[0], &project.machines[0], &Host::Local);
        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::InProgress
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_skips_first_tick_then_emits_when_quiet() {
        // The poller seeds `next_attempt` from "now" so the first tick
        // after start never fires (one full interval must pass first).
        // The second consideration, well past the interval and with no
        // recent events.log activity, emits exactly one heartbeat line.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-emit-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        // Tight 1ms interval so the test doesn't have to sleep for the
        // real 3-minute default.
        project.heartbeat =
            shelbi_core::HeartbeatConfig::Every(Duration::from_millis(1));

        let mut next: Option<Instant> = None;
        // First call seeds the schedule and returns without writing.
        maybe_emit_heartbeat(&project, &mut next, || true);
        assert!(next.is_some(), "first tick must seed the schedule");
        let log = shelbi_state::events_log_path().unwrap();
        assert!(
            !log.exists() || std::fs::read_to_string(&log).unwrap().is_empty(),
            "first tick must not emit a heartbeat"
        );

        // Wait past the interval, with no other writer touching the
        // log, and the next attempt emits one line.
        std::thread::sleep(Duration::from_millis(20));
        maybe_emit_heartbeat(&project, &mut next, || true);
        let body = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = body.lines().filter(|l| l.contains("heartbeat")).collect();
        assert_eq!(lines.len(), 1, "expected one heartbeat line, got: {body:?}");
        assert!(
            lines[0].contains(" project=demo heartbeat"),
            "line: {}",
            lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_debounces_against_recent_activity() {
        // A worker transition lands in events.log moments before the
        // heartbeat attempt — the heartbeat must skip this consideration
        // so active boards don't get padded with no-op lines.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-debounce-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        // Use a 1-second window so the events.log mtime test ("written
        // in the last interval") sees the worker event as recent.
        project.heartbeat = shelbi_core::HeartbeatConfig::Every(Duration::from_secs(1));

        let mut next: Option<Instant> = None;
        // Seed.
        maybe_emit_heartbeat(&project, &mut next, || true);
        // Force the next attempt to be due immediately, but write
        // unrelated activity first so the debounce trips.
        shelbi_state::append_worker_event("alpha", None, WorkerState::Working).unwrap();
        next = Some(Instant::now());

        maybe_emit_heartbeat(&project, &mut next, || true);

        let log = shelbi_state::events_log_path().unwrap();
        let body = std::fs::read_to_string(&log).unwrap();
        let hb_lines: Vec<&str> =
            body.lines().filter(|l| l.contains("heartbeat")).collect();
        assert!(
            hb_lines.is_empty(),
            "debounce must skip emission when events.log was just written; log: {body:?}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_off_never_emits_and_clears_schedule() {
        // Project sets `heartbeat: off`: the function must clear any
        // outstanding schedule (so flipping it back on later starts a
        // fresh interval) and never append to events.log.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-off-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        project.heartbeat = shelbi_core::HeartbeatConfig::Off;

        let mut next = Some(Instant::now() - Duration::from_secs(60));
        maybe_emit_heartbeat(&project, &mut next, || true);
        assert!(next.is_none(), "off must clear the pending schedule");

        let log = shelbi_state::events_log_path().unwrap();
        assert!(!log.exists() || std::fs::read_to_string(&log).unwrap().is_empty());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_pauses_while_offline_then_resumes_when_back_online() {
        // Disconnected box: probe returns false on every due tick, so
        // the feed must stay silent — no padding lines during the
        // offline window. The schedule still advances each attempt, and
        // once the probe flips back to true the next due tick emits.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-offline-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        project.heartbeat = shelbi_core::HeartbeatConfig::Every(Duration::from_millis(1));

        let mut next: Option<Instant> = None;
        // Seed, then drive several "due" attempts while offline.
        maybe_emit_heartbeat(&project, &mut next, || false);
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(5));
            maybe_emit_heartbeat(&project, &mut next, || false);
        }
        let log = shelbi_state::events_log_path().unwrap();
        let body = if log.exists() {
            std::fs::read_to_string(&log).unwrap()
        } else {
            String::new()
        };
        let offline_hb_lines: Vec<&str> =
            body.lines().filter(|l| l.contains("heartbeat")).collect();
        assert!(
            offline_hb_lines.is_empty(),
            "offline windows must not emit heartbeats; log: {body:?}"
        );

        // Internet returns. The next due tick (past the interval and
        // with no recent activity) emits one heartbeat line.
        std::thread::sleep(Duration::from_millis(5));
        maybe_emit_heartbeat(&project, &mut next, || true);
        let body = std::fs::read_to_string(&log).unwrap();
        let online_hb_lines: Vec<&str> =
            body.lines().filter(|l| l.contains("heartbeat")).collect();
        assert_eq!(
            online_hb_lines.len(),
            1,
            "exactly one heartbeat after reconnect, got: {body:?}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
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
