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
    append_contextstore_event, append_worker_event, load_worker_status, parse_pane_title_marker,
    save_worker_status, PaneMarker, WorkerState, WorkerStatus,
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

    last_known.insert(worker.name.clone(), outcome.status.state);
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
