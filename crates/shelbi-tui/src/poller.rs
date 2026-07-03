//! Background workspace-state poller. Lives in the sidebar process and is
//! the only place the hub talks to workspace panes for observability.
//!
//! Cadence: per-project `workspace_poll_interval_secs` (default 5s). The
//! poller spawns ONE thread per declared workspace — each running its own
//! independent poll loop — so a hung SSH call to one machine (unreachable
//! host, expired Tailscale auth, ProxyJump timeout) only freezes that
//! workspace's thread, never the others. Earlier versions used a single
//! sequential loop, which would block every local-workspace poll behind a
//! single stuck remote-workspace SSH call and silently freeze the review
//! marker handoff for hours at a time.
//!
//! Each cycle asks tmux for the workspace pane's title
//! (`display-message -p '#{pane_title}'`, routed over SSH for remote
//! machines via shelbi-ssh — which sets up ControlMaster so the marginal
//! cost per poll is a socket write, not a TCP handshake) and parses the
//! trailing `shelbi:<state>` marker. The marker file at
//! `<worktree>/.claude/shelbi-review-ready` is checked first, before any
//! pane operation, so the review handoff isn't gated on a working pane
//! title (Claude's own OSC writes often clobber the marker before the
//! poller sees it).
//!
//! Each cycle also takes a `tmux capture-pane` sample and matches it against
//! the runner's blocking-dialog signatures (see
//! `shelbi_core::default_dialog_signatures` / `AgentRunnerSpec::dialog_signatures`).
//! A pane frozen on an interactive modal (usage-limit, workspace-trust,
//! permission-confirm) keeps a stale `shelbi:working` title — no hook fires —
//! so the title path alone can't see the stall. On a match the poller emits a
//! `working -> blocked reason=dialog:<kind>` line (deduped per incident, with a
//! recovery line when the modal clears) so the orchestrator can react instead
//! of discovering a wedged board hours later.
//!
//! On a state change the poller writes two files:
//! - `~/.shelbi/workspaces/<name>/status.yaml` — last observed state.
//! - `~/.shelbi/events.log` — append-only transition history.
//!
//! On a same-state observation it still bumps `last_seen` in
//! `status.yaml` so the UI can tell stale from fresh observations.
//!
//! All authoritative state stays on the hub — workspaces themselves only
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
    append_contextstore_event, append_heartbeat_event, append_rebase_event,
    append_workspace_dialog_event, append_workspace_event, events_log_path, load_workspace_status,
    parse_pane_title_marker, save_workspace_status, WorkspaceState, WorkspaceStatus,
};

/// How often each per-workspace thread re-verifies its host's reverse
/// forward. Kept well under `ControlPersist=600` (10 min) so a master that
/// lapses and reopens — the moment a stale remote socket would wedge the
/// `-R` rebind — gets its forward re-checked and repaired within a couple
/// of minutes rather than staying silently broken until the thread
/// restarts. Local workspaces short-circuit the check, so the only cost
/// this cadence imposes is on SSH hosts.
const FORWARD_RECHECK_INTERVAL: Duration = Duration::from_secs(120);

/// Spawned poller handle. Dropping it asks the thread to exit and joins it.
pub struct WorkspacePoller {
    shutdown: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl WorkspacePoller {
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

impl Drop for WorkspacePoller {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// Supervisor: spawns one persistent poll thread per workspace declared in
/// the project YAML, then sleeps until shutdown. Each per-workspace thread
/// owns its own SSH/IO calls, so a hung remote workspace only blocks its
/// own thread — local workspaces keep polling on cadence.
///
/// We re-check the workspaces list every supervisor tick (5s) so that
/// workspaces added to the YAML at runtime get a thread spawned without a
/// hub restart. Removed workspaces' threads exit themselves when they
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
            // new workspaces get threads spawned without a restart.
            Err(_) => {
                sleep_interruptible(Duration::from_secs(5), &shutdown);
                continue;
            }
        };

        for workspace in &project.workspaces {
            // Drop dead-thread handles so a panic in poll_one for this
            // workspace doesn't leave it un-respawned. (Per-workspace threads
            // shouldn't normally panic — poll_one swallows errors — but
            // defense-in-depth.)
            if spawned.get(&workspace.name).is_some_and(|h| h.is_finished()) {
                if let Some(h) = spawned.remove(&workspace.name) {
                    let _ = h.join();
                }
            }
            if spawned.contains_key(&workspace.name) {
                continue;
            }
            let workspace_name = workspace.name.clone();
            let project_name = project_name.clone();
            let shutdown_clone = shutdown.clone();
            let handle = thread::Builder::new()
                .name(format!("shelbi-poller-{workspace_name}"))
                .spawn(move || run_workspace_poll_loop(project_name, workspace_name, shutdown_clone))
                .ok();
            if let Some(h) = handle {
                spawned.insert(workspace.name.clone(), h);
            }
        }

        // Heartbeat is project-wide (one per project, not per workspace),
        // so it lives on the supervisor rather than inside any per-workspace
        // thread. The function is a no-op if heartbeat is off or not yet
        // due, and debounces against any other events.log activity.
        // `online_probe` is the connectivity gate: while the box is
        // offline the heartbeat skips silently so the feed doesn't fill
        // with no-op lines during a network drop.
        maybe_emit_heartbeat(&project, &mut next_heartbeat_attempt, online_probe);

        // Supervisor cadence is fixed at 5s — independent of
        // `workspace_poll_interval_secs`, which governs each per-workspace
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

/// One workspace's persistent poll loop. Drives [`poll_one`] every
/// `workspace_poll_interval_secs`, reloading the project YAML each cycle so
/// the user can edit the workspace list / interval without a hub restart.
/// Exits cleanly when shutdown is requested OR the workspace is removed
/// from the YAML.
fn run_workspace_poll_loop(
    project_name: String,
    workspace_name: String,
    shutdown: Arc<AtomicBool>,
) {
    // Each workspace thread keeps its own `last_known` so it doesn't need
    // to share a Mutex with the supervisor or its peers. Seeded from
    // status.yaml on first observation (handled inside `poll_one`).
    let mut last_known: Option<WorkspaceState> = None;

    // Reverse-forward health schedule. For SSH workspaces we (re)establish
    // and verify the hub's `-R` forward at thread start and on a slow
    // cadence after, so a ControlMaster that died and left a stale remote
    // socket behind gets repaired instead of silently swallowing every
    // worker→hub message (adversarial review F7). `None` means "due now";
    // the check is a cheap no-op for local hosts and two probe round-trips
    // when the forward is already healthy.
    let mut next_forward_check: Option<Instant> = None;

    // Which blocking-dialog kind this workspace is currently stuck on (if
    // any). In-memory, per-thread — the whole point is dedupe *across*
    // consecutive polls so we emit one `blocked reason=dialog:*` line per
    // incident and one recovery line when it clears. A hub restart re-seeds
    // to `None`, so at worst a still-open dialog re-emits once after a
    // restart — acceptable for an advisory heads-up.
    let mut last_dialog: Option<String> = None;

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
        let Some(workspace) = project.workspaces.iter().find(|w| w.name == workspace_name) else {
            // Workspace removed from the project YAML — exit this thread.
            // The supervisor will not respawn it because it's not in the
            // workspaces list any more.
            break;
        };
        let interval = Duration::from_secs(project.workspace_poll_interval_secs.max(1));

        // Keep the reverse forward healthy before polling over it. Runs on
        // its own slow cadence (independent of the poll interval) so the
        // common case adds no per-poll cost. Failures are logged, not fatal
        // — a wedged forward shouldn't stop us observing pane state.
        if next_forward_check.map_or(true, |t| Instant::now() >= t) {
            if let Some(machine) = project.machine(&workspace.machine) {
                let host = machine.host();
                if let Err(e) = shelbi_ssh::ensure_reverse_forward(&host) {
                    tracing::warn!(
                        workspace = %workspace_name,
                        error = %e,
                        "reverse-forward health check failed",
                    );
                }
            }
            next_forward_check = Some(Instant::now() + FORWARD_RECHECK_INTERVAL);
        }

        poll_one(&project, workspace, &mut last_known, &mut last_dialog);

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
        // Both counts are cheap on-disk reads (task YAMLs + the events log,
        // same files the tick already touches) and are computed fresh so the
        // heartbeat is accurate at emit time. A read failure shouldn't sink
        // the heartbeat — fall back to 0, which the orchestrator treats as
        // "nothing to do" (a silent ack), the same as a genuinely quiet board.
        let zen_eligible = shelbi_orchestrator::zen::mechanically_eligible(project)
            .map(|ids| ids.len())
            .unwrap_or(0);
        let idle_workspaces = shelbi_state::idle_workspace_count(project).unwrap_or(0);
        if let Err(e) = append_heartbeat_event(&project.name, zen_eligible, idle_workspaces) {
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
    workspace: &shelbi_core::WorkspaceSpec,
    last_known: &mut Option<WorkspaceState>,
    last_dialog: &mut Option<String>,
) {
    let Some(machine) = project.machine(&workspace.machine) else {
        return;
    };
    let host = machine.host();
    let Ok(addr) = shelbi_orchestrator::workspace::workspace_tmux_addr(project, workspace) else {
        return;
    };

    // Review handoff is a file marker the workspace writes when it's done, read
    // independently of the pane title. We check it *before* the pane-title
    // state below (and unconditionally, even if the pane has since died or
    // Claude has overwritten its title) so nothing the agent's UI does can
    // hide the signal.
    maybe_promote_to_review(project, workspace, machine, &host);

    // Reaper sweep (spec §10): a review workspace with a live server pane but
    // no active task has leaked its bound port, which blocks the next
    // dispatch onto that slot. Runs every poll as the heartbeat backstop for a
    // missed teardown; a no-op for the common case (no server pane, or the
    // server's task is still active).
    maybe_reap_server_pane(project, workspace);

    // No pane → no marker. The display-message call would fail anyway,
    // but checking up-front keeps stderr noise out of the log.
    if !shelbi_orchestrator::workspace::workspace_pane_alive(&host, &addr).unwrap_or(false) {
        // The pane is gone (dispatch teardown, crash, or normal exit). Any
        // dialog we were tracking can't be "cleared" in a meaningful way —
        // pane death has its own `pane_alive=false` event — so just drop the
        // stuck-state so a respawned pane re-detects from scratch.
        *last_dialog = None;
        return;
    }

    // Blocking-dialog detection. Runs on the same tick as the title read but
    // via a separate `capture-pane` sample, because a pane frozen on a
    // usage-limit / trust / permission modal keeps a stale `shelbi:working`
    // title — no hook fires — so the title path alone can't see the stall.
    maybe_emit_dialog_event(project, workspace, &host, &addr, last_dialog);

    let title = match shelbi_tmux::pane_title(&host, &addr) {
        Ok(t) => t,
        Err(_) => return,
    };
    let Some(marker) = parse_pane_title_marker(&title) else {
        return;
    };
    let new_state = marker.workspace_state();

    // Bootstrap previous state from disk on first sighting so a hub
    // restart doesn't emit a bogus `none -> X` event for state we've
    // already recorded.
    let prior = match *last_known {
        Some(s) => Some(PriorState {
            state: s,
            last_transition: load_workspace_status(&workspace.name)
                .ok()
                .flatten()
                .map(|s| s.last_transition),
        }),
        None => load_workspace_status(&workspace.name)
            .ok()
            .flatten()
            .map(|s| PriorState {
                state: s.state,
                last_transition: Some(s.last_transition),
            }),
    };

    let current_task = current_task_for(project, &workspace.name);
    let outcome = decide(
        &workspace.name,
        current_task.clone(),
        prior,
        new_state,
        Utc::now(),
    );

    if let Err(e) = save_workspace_status(&outcome.status) {
        tracing::warn!(workspace = %workspace.name, error = %e, "save_workspace_status failed");
    }
    if outcome.transitioned {
        if let Err(e) = append_workspace_event(
            &workspace.name,
            outcome.prev_state,
            outcome.status.state,
        ) {
            tracing::warn!(workspace = %workspace.name, error = %e, "append_workspace_event failed");
        }
    }

    // A `shelbi:review` pane title is treated as a *state hint* only, never
    // as a board-move trigger. Any program the agent runs (a build script,
    // `cat` of a hostile file, test output) can emit an OSC title sequence
    // ending in `shelbi:review` and drive the pane title, so acting on it
    // here would let untrusted checked-out code promote a task mid-work. The
    // sole trigger for the review column move is the independent file-based
    // review marker, consumed by `maybe_promote_to_review` above — a file
    // the agent's UI can't clobber.

    *last_known = Some(outcome.status.state);
}

/// One dialog transition the poller should emit this tick.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DialogEvent {
    kind: String,
    /// `true` → `working -> blocked`; `false` → `blocked -> working` (recovery).
    blocked: bool,
}

/// Pure dedupe decision for blocking-dialog detection. Given the dialog kind
/// this workspace was previously stuck on (`prev`) and the kind detected on
/// the current pane sample (`detected`), return the event(s) to emit and the
/// new stuck-state to remember.
///
/// - none → some: newly blocked, emit one `blocked` line.
/// - some → same: still stuck on the same dialog, emit nothing (dedupe).
/// - some → none: the modal cleared, emit one recovery line.
/// - some → other: the dialog changed kind without a clear in between (rare;
///   e.g. trust prompt replaced by a permission confirm) — emit a recovery
///   for the old kind then a block for the new so the stream stays balanced.
/// - none → none: nothing happening.
fn decide_dialog(prev: Option<&str>, detected: Option<&str>) -> (Vec<DialogEvent>, Option<String>) {
    match (prev, detected) {
        (None, None) => (Vec::new(), None),
        (None, Some(kind)) => (
            vec![DialogEvent {
                kind: kind.to_string(),
                blocked: true,
            }],
            Some(kind.to_string()),
        ),
        (Some(prev), None) => (
            vec![DialogEvent {
                kind: prev.to_string(),
                blocked: false,
            }],
            None,
        ),
        (Some(prev), Some(kind)) if prev == kind => (Vec::new(), Some(kind.to_string())),
        (Some(prev), Some(kind)) => (
            vec![
                DialogEvent {
                    kind: prev.to_string(),
                    blocked: false,
                },
                DialogEvent {
                    kind: kind.to_string(),
                    blocked: true,
                },
            ],
            Some(kind.to_string()),
        ),
    }
}

/// Sample the workspace pane, match it against the runner's blocking-dialog
/// signatures, and emit a `blocked reason=dialog:*` (or recovery) line on a
/// change of stuck-state. Deduped via `last_dialog` so a still-open modal
/// only produces one event per incident.
///
/// Best-effort: an unknown runner or a transient `capture-pane` failure just
/// leaves the stuck-state untouched and retries next tick — we'd rather miss
/// a beat than fabricate a recovery on a capture hiccup.
fn maybe_emit_dialog_event(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    host: &shelbi_core::Host,
    addr: &shelbi_core::TmuxAddr,
    last_dialog: &mut Option<String>,
) {
    let Some(runner) = project.runner(&workspace.runner) else {
        return;
    };
    let signatures = runner.effective_dialog_signatures();
    if signatures.is_empty() {
        // Nothing configured for this runner (and no built-in default) —
        // clear any prior stuck-state so a config change that removes the
        // last signature doesn't leave us thinking the pane is still blocked.
        *last_dialog = None;
        return;
    }

    let screen = match shelbi_tmux::capture(host, addr) {
        Ok(s) => s,
        Err(_) => return,
    };
    let detected = shelbi_orchestrator::ready::detect_blocking_dialog(&screen, &signatures);

    let (events, next) = decide_dialog(last_dialog.as_deref(), detected.as_deref());
    for ev in events {
        if let Err(e) = append_workspace_dialog_event(&workspace.name, &ev.kind, ev.blocked) {
            tracing::warn!(
                workspace = %workspace.name,
                kind = %ev.kind,
                blocked = ev.blocked,
                error = %e,
                "append_workspace_dialog_event failed",
            );
        }
    }
    *last_dialog = next;
}

/// Check the workspace's review-ready file marker and, if present, move its
/// in-progress task to the review column. The marker is the workspace's handoff
/// signal — it writes its task id into `<worktree>/.claude/shelbi-review-ready`
/// when done (see `shelbi_orchestrator::workspace::workspace_review_marker`).
///
/// Best-effort and idempotent: we consume the marker exactly once by clearing
/// it after a successful move, and `move_task` is a no-op once the task is
/// already in review, so a workspace that keeps churning in its pane afterward
/// never gets pulled back out. A stale marker (worktree reused before the
/// previous one was cleared) names a task that's no longer in-progress for
/// this workspace, so we clear it without moving anything.
fn maybe_promote_to_review(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
) {
    let marker = shelbi_orchestrator::workspace::workspace_review_marker(machine, workspace);
    let task_id = match shelbi_orchestrator::workspace::read_review_marker(host, &marker) {
        Ok(Some(id)) => id,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(workspace = %workspace.name, error = %e, "read_review_marker failed");
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
                && tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()) =>
        {
            // Auto-rebase the workspace's branch onto the project's default
            // branch before the column move. The goal is to absorb any
            // prereq commits that landed on main while the workspace was
            // working, so the human reviewer sees a single clean diff
            // instead of having to drop into the worktree and run the
            // rebase + force-push by hand. We do this BEFORE the column
            // move (rather than blocking on it) so the row showing up in
            // `review` already reflects the rewritten branch; a conflict
            // is logged but doesn't block the promotion — the human still
            // wants to see the work in review and resolve the conflict
            // during the review checkout.
            rebase_workspace_branch_before_review(project, workspace, machine, host, &task_id);

            match shelbi_state::move_task(&project.name, &task_id, Column::Review) {
                Ok(Some((from, to, workflow))) => {
                    if let Err(e) = shelbi_state::append_task_event(
                        &task_id,
                        &workflow,
                        from,
                        to,
                        "workspace:review-marker",
                    ) {
                        tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "append_task_event failed");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    // Leave the marker in place so we retry on the next tick.
                    tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "move_task to review failed");
                    return;
                }
            }
            tracing::info!(workspace = %workspace.name, task = %task_id, "promoted task to review via marker");

            // Best-effort: pull any ContextStore writes the workspace made
            // on its machine back to hub. Skipped silently when the
            // project has no `contextstore_sync` configured, when the
            // body doesn't trip the heuristic, or when the workspace is
            // local. Failures log to events.log but never block the
            // promotion — that's the contract on this path.
            sync_contextstore_from_workspace(project, machine, &tf.body);
        }
        Ok(_) => {
            tracing::debug!(workspace = %workspace.name, task = %task_id, "stale review marker (task not in-progress for this workspace); clearing");
        }
        Err(e) => {
            tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "review marker names unloadable task; clearing");
        }
    }

    if let Err(e) = shelbi_orchestrator::workspace::clear_review_marker(host, &marker) {
        tracing::warn!(workspace = %workspace.name, error = %e, "clear_review_marker failed");
    }
}

/// Resolve the workspace's branch for the in-progress task and rebase it onto
/// the project's default branch. Records one `rebase` line in `events.log`
/// describing the outcome (ok / up-to-date / conflict / skipped). Never
/// blocks the calling review promotion — failures here are advisory.
///
/// `branch` falls back to the conventional `shelbi/<task-id>` when the task
/// frontmatter doesn't pin one explicitly; that mirrors the review-load path.
fn rebase_workspace_branch_before_review(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
    task_id: &str,
) {
    let task_file = match shelbi_state::load_task(&project.name, task_id) {
        Ok(tf) => tf,
        Err(e) => {
            tracing::debug!(workspace = %workspace.name, task = %task_id, error = %e, "skip rebase: load_task failed");
            return;
        }
    };
    let branch = task_file
        .task
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{task_id}"));

    let worktree = shelbi_orchestrator::workspace::workspace_worktree(machine, workspace);
    let outcome = shelbi_orchestrator::workspace::rebase_workspace_branch_onto_default(
        host,
        &worktree,
        &project.default_branch,
    );

    let status = outcome.status_token();
    let detail = outcome.detail();
    if let Err(e) = append_rebase_event(task_id, &workspace.name, &branch, status, &detail) {
        tracing::warn!(
            workspace = %workspace.name,
            task = %task_id,
            error = %e,
            "append_rebase_event failed",
        );
    }
    match &outcome {
        shelbi_orchestrator::workspace::RebaseOutcome::Conflict { .. } => {
            tracing::warn!(
                workspace = %workspace.name,
                task = %task_id,
                branch = %branch,
                detail = %detail,
                "auto-rebase onto default branch conflicted; worktree returned to pre-rebase state",
            );
        }
        shelbi_orchestrator::workspace::RebaseOutcome::Skipped { .. } => {
            tracing::info!(
                workspace = %workspace.name,
                task = %task_id,
                branch = %branch,
                detail = %detail,
                "auto-rebase skipped",
            );
        }
        _ => {
            tracing::info!(
                workspace = %workspace.name,
                task = %task_id,
                branch = %branch,
                status = %status,
                detail = %detail,
                "auto-rebase outcome",
            );
        }
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
fn sync_contextstore_from_workspace(
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
                    "contextstore synced from remote workspace",
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
                    "contextstore sync skipped — workspace is local",
                );
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PriorState {
    state: WorkspaceState,
    last_transition: Option<DateTime<Utc>>,
}

#[derive(Debug)]
struct PollOutcome {
    transitioned: bool,
    prev_state: Option<WorkspaceState>,
    status: WorkspaceStatus,
}

/// Pure transition decision: given the previous state (if any) and a
/// fresh observation, build the [`WorkspaceStatus`] to persist and decide
/// whether the change deserves an `events.log` line.
fn decide(
    workspace: &str,
    current_task: Option<String>,
    prior: Option<PriorState>,
    new_state: WorkspaceState,
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
        status: WorkspaceStatus {
            workspace: workspace.to_string(),
            current_task,
            state: new_state,
            last_transition,
            last_seen: now,
        },
    }
}

/// In-progress task currently assigned to `workspace`, if any. Cheap (one
/// task-dir scan); called once per workspace per poll tick.
/// Best-effort reaper pass over one workspace's server pane. Logs a reaped
/// leak (a server whose task has moved on) so the action is visible in the
/// TUI log; all other outcomes (no server, still-active, GC of a dead record)
/// are silent. Never propagates — a reaper error must not sink the poll loop.
fn maybe_reap_server_pane(project: &Project, workspace: &shelbi_core::WorkspaceSpec) {
    use shelbi_orchestrator::server_pane::ReapOutcome;
    match shelbi_orchestrator::server_pane::reap_server_pane_if_leaked(project, workspace) {
        Ok(ReapOutcome::Reaped { task_id, port }) => {
            tracing::info!(
                workspace = %workspace.name,
                task = %task_id,
                port,
                "reaped leaked review server pane",
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                workspace = %workspace.name,
                error = %e,
                "reap_server_pane_if_leaked failed",
            );
        }
    }
}

fn current_task_for(project: &Project, workspace_name: &str) -> Option<String> {
    shelbi_state::list_column(&project.name, Column::InProgress)
        .ok()?
        .into_iter()
        .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace_name))
        .map(|tf| tf.task.id)
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn ts(secs: i64) -> DateTime<Utc> {
        Utc.timestamp_opt(secs, 0).unwrap()
    }

    fn ev(kind: &str, blocked: bool) -> DialogEvent {
        DialogEvent {
            kind: kind.to_string(),
            blocked,
        }
    }

    #[test]
    fn decide_dialog_emits_block_once_and_recovery_on_clear() {
        // Nothing → nothing: silent.
        let (out, next) = decide_dialog(None, None);
        assert!(out.is_empty());
        assert_eq!(next, None);

        // Newly blocked: one `blocked` line, remember the kind.
        let (out, next) = decide_dialog(None, Some("usage-limit"));
        assert_eq!(out, vec![ev("usage-limit", true)]);
        assert_eq!(next.as_deref(), Some("usage-limit"));

        // Still stuck on the SAME dialog: deduped — no event, kind retained.
        // This is the "emitted once per incident" guarantee across heartbeats.
        let (out, next) = decide_dialog(Some("usage-limit"), Some("usage-limit"));
        assert!(out.is_empty(), "same dialog must not re-emit: {out:?}");
        assert_eq!(next.as_deref(), Some("usage-limit"));

        // Cleared: one recovery line, forget the kind.
        let (out, next) = decide_dialog(Some("usage-limit"), None);
        assert_eq!(out, vec![ev("usage-limit", false)]);
        assert_eq!(next, None);
    }

    #[test]
    fn decide_dialog_rebalances_when_kind_changes_without_clearing() {
        // A trust prompt is replaced by a permission confirm with no
        // in-between clear: emit a recovery for the old kind then a block for
        // the new so the blocked/recovery stream stays balanced.
        let (out, next) = decide_dialog(Some("trust"), Some("permission"));
        assert_eq!(out, vec![ev("trust", false), ev("permission", true)]);
        assert_eq!(next.as_deref(), Some("permission"));
    }

    #[test]
    fn first_observation_is_a_transition_from_none() {
        let out = decide("alpha", None, None, WorkspaceState::Working, ts(100));
        assert!(out.transitioned);
        assert!(out.prev_state.is_none());
        assert_eq!(out.status.state, WorkspaceState::Working);
        assert_eq!(out.status.last_transition, ts(100));
        assert_eq!(out.status.last_seen, ts(100));
    }

    #[test]
    fn same_state_observation_bumps_last_seen_only() {
        // Prior state already on disk from an earlier transition. Same
        // marker on the next poll: keep `last_transition` put, bump
        // `last_seen` to now.
        let prior = Some(PriorState {
            state: WorkspaceState::Working,
            last_transition: Some(ts(50)),
        });
        let out = decide("alpha", None, prior, WorkspaceState::Working, ts(120));
        assert!(!out.transitioned);
        assert_eq!(out.status.last_transition, ts(50));
        assert_eq!(out.status.last_seen, ts(120));
    }

    #[test]
    fn state_change_records_previous_and_resets_transition() {
        // Working → AwaitingInput → Blocked.
        let prior = Some(PriorState {
            state: WorkspaceState::Working,
            last_transition: Some(ts(50)),
        });
        let out = decide(
            "alpha",
            Some("task-1".into()),
            prior,
            WorkspaceState::AwaitingInput,
            ts(200),
        );
        assert!(out.transitioned);
        assert_eq!(out.prev_state, Some(WorkspaceState::Working));
        assert_eq!(out.status.state, WorkspaceState::AwaitingInput);
        assert_eq!(out.status.last_transition, ts(200));
        assert_eq!(out.status.current_task.as_deref(), Some("task-1"));
    }

    use shelbi_core::{
        AgentRunnerSpec, Host, Machine, MachineKind, OrchestratorSpec, Task, WorkspaceSpec,
    };
    use std::collections::BTreeMap;

    /// A local-machine project with a single workspace whose worktree lives
    /// under `work_dir`, so the marker path is a real writable local file.
    fn local_project(work_dir: &std::path::Path) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "demo".into(),
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            config_mode: None,
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
            workspaces: vec![WorkspaceSpec {
                name: "alpha".into(),
                machine: "hub".into(),
                runner: "claude".into(),
                role: Default::default(),
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: Default::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    fn in_progress_task(id: &str, workspace: &str) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: Column::InProgress,
            priority: 0,
            assigned_to: Some(workspace.into()),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    fn write_marker(project: &Project, body: &str) -> std::path::PathBuf {
        let marker = shelbi_orchestrator::workspace::workspace_review_marker(
            &project.machines[0],
            &project.workspaces[0],
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

        // Workspace signals review by writing its task id into the marker.
        let marker = write_marker(&project, "fix-login\n");

        maybe_promote_to_review(&project, &project.workspaces[0], &project.machines[0], &Host::Local);

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
        // Shape from `Plans/workflows.md` §10. We match on the canonical
        // `<ts> task=<id> <from> -> <to>` shape so other event kinds that
        // happen to mention the same task id (e.g. the auto-rebase line
        // emitted just before the promotion) don't get counted as task
        // transitions.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let task_lines: Vec<&str> = log
            .lines()
            .filter(|l| {
                let mut parts = l.splitn(3, ' ');
                let _ts = parts.next();
                parts.next() == Some("task=fix-login")
            })
            .collect();
        assert_eq!(task_lines.len(), 1, "log: {log:?}");
        assert!(
            task_lines[0].contains(" workflow=default "),
            "line: {}",
            task_lines[0]
        );
        assert!(
            task_lines[0].contains(" in_progress -> review "),
            "line: {}",
            task_lines[0]
        );
        assert!(
            task_lines[0].contains(" reason=workspace:review-marker "),
            "line: {}",
            task_lines[0]
        );
        assert!(
            task_lines[0].ends_with(" to_category=handoff"),
            "line: {}",
            task_lines[0]
        );

        // Auto-rebase also lands one line per promotion. In this test the
        // work_dir isn't a real git repo, so the rebase short-circuits to
        // `skipped`; the event still gets recorded so the user can tell
        // from events.log whether the auto-rebase ran or punted.
        let rebase_lines: Vec<&str> = log
            .lines()
            .filter(|l| {
                let mut parts = l.splitn(3, ' ');
                let _ts = parts.next();
                parts.next() == Some("rebase")
            })
            .collect();
        assert_eq!(rebase_lines.len(), 1, "log: {log:?}");
        let rebase_line = rebase_lines[0];
        assert!(
            rebase_line.contains("task=fix-login"),
            "line: {rebase_line}"
        );
        assert!(
            rebase_line.contains("workspace=alpha"),
            "line: {rebase_line}"
        );
        assert!(
            rebase_line.contains("branch=shelbi/fix-login"),
            "line: {rebase_line}"
        );
        assert!(
            rebase_line.contains("status=skipped"),
            "expected skipped on a non-git work_dir, got: {rebase_line}"
        );

        // A leftover/stale marker naming a task that's no longer in-progress
        // for this workspace is cleared without moving anything back out.
        let marker = write_marker(&project, "fix-login\n");
        maybe_promote_to_review(&project, &project.workspaces[0], &project.machines[0], &Host::Local);
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
        // Local-host workspace — sync must short-circuit to SkippedLocal so
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

        maybe_promote_to_review(&project, &project.workspaces[0], &project.machines[0], &Host::Local);

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let cs_lines: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(" contextstore "))
            .collect();
        assert_eq!(cs_lines.len(), 1, "log: {log:?}");
        // Local workspace = SkippedLocal status (`skipped-local`).
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

        maybe_promote_to_review(&project, &project.workspaces[0], &project.machines[0], &Host::Local);

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
        maybe_promote_to_review(&project, &project.workspaces[0], &project.machines[0], &Host::Local);
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
        // Empty backlog → zen_eligible=0; the one declared workspace ("alpha")
        // holds no active-category task → idle_workspaces=1. Both counts are
        // always present so the orchestrator's react-to-heartbeat rule can
        // parse them every tick.
        assert!(
            lines[0].contains(" project=demo heartbeat zen_eligible=0 idle_workspaces=1"),
            "line: {}",
            lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_debounces_against_recent_activity() {
        // A workspace transition lands in events.log moments before the
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
        // in the last interval") sees the workspace event as recent.
        project.heartbeat = shelbi_core::HeartbeatConfig::Every(Duration::from_secs(1));

        let mut next: Option<Instant> = None;
        // Seed.
        maybe_emit_heartbeat(&project, &mut next, || true);
        // Force the next attempt to be due immediately, but write
        // unrelated activity first so the debounce trips.
        shelbi_state::append_workspace_event("alpha", None, WorkspaceState::Working).unwrap();
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
