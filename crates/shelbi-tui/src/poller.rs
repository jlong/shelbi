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
//! Each cycle also takes a `tmux capture-pane` sample and inspects it two ways,
//! because a stalled pane keeps a stale `shelbi:working` title — no hook fires —
//! so the title path alone can't see the stall:
//!
//! - **Usage-limit pause.** If the sample shows the runner stalled on its
//!   usage/session limit (`shelbi_orchestrator::ready::detect_usage_limit`,
//!   anchored on claude's actual modal chrome — *not* a bare substring, so a
//!   pane that merely mentions the phrase doesn't trip it) the workspace is
//!   marked [`WorkspaceState::Paused`] (⏸ badge) and a `-> paused
//!   reason=usage-limit` line is emitted (with the reset time when shown). The
//!   state clears — reverting to the title-derived state — on the first poll
//!   after the limit lifts, so the orchestrator can hold new work off a limited
//!   slot until it frees up.
//! - **Blocking dialogs.** Otherwise the sample is matched against the runner's
//!   blocking-dialog signatures (see `shelbi_core::default_dialog_signatures` /
//!   `AgentRunnerSpec::dialog_signatures`) for a human-gated modal
//!   (workspace-trust, permission-confirm). On a match the poller emits a
//!   `working -> blocked reason=dialog:<kind>` line (deduped per incident, with
//!   a recovery line when the modal clears) so the orchestrator can react
//!   instead of discovering a wedged board hours later.
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

use shelbi_core::{default_workflow, Column, Project, StatusCategory, Workflow};
use shelbi_orchestrator::supervision::{SupervisionAction, SupervisionInputs, SupervisionState};
use shelbi_state::{
    append_contextstore_event, append_heartbeat_event, append_rebase_event,
    append_workspace_dialog_event, append_workspace_event, append_workspace_pause_event,
    events_log_path, load_workspace_status, parse_pane_title_marker, save_workspace_status,
    WorkspaceState, WorkspaceStatus,
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

    // Heartbeat schedule. We seed the first attempt from "now" rather than
    // from the last events.log mtime so a poller restart mid-interval doesn't
    // immediately fire a heartbeat that was technically "due" before the
    // crash — the spec is one interval from poller start, not from the missed
    // slot. Also carries the adaptive back-off state (current interval + the
    // mtime of our last emission) so a quiescent board slows the cadence.
    let mut heartbeat = HeartbeatSchedule::default();

    // Auto-restart supervision for the orchestrator pane is project-wide (one
    // pane per project, not per workspace), so it lives on the supervisor tick
    // alongside the heartbeat rather than inside any per-workspace thread.
    let mut orch_supervision = SupervisionState::default();

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
        maybe_emit_heartbeat(&project, &mut heartbeat, online_probe);

        // Relaunch the orchestrator pane if it crashed (Zen stays off after
        // the restart via its own `__zen-orch-start` crash-recovery step).
        maybe_supervise_orchestrator(&project, &mut orch_supervision);

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

    // Auto-restart supervision bookkeeping for this workspace's pane. Same
    // per-thread lifetime as `last_dialog` / `last_known`: a poller restart
    // re-seeds it, which at worst re-arms one restart for a pane that was
    // already mid-crash-loop. See `shelbi_orchestrator::supervision`.
    let mut supervision = SupervisionState::default();

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

        poll_one(
            &project,
            workspace,
            &mut last_known,
            &mut last_dialog,
            &mut supervision,
        );

        sleep_interruptible(interval, &shutdown);
    }
}

/// Adaptive heartbeat cadence state, threaded across supervisor ticks.
///
/// The cadence is `standard` (from config) whenever there's supervisable work
/// in flight, and backs off exponentially — doubling each idle tick, capped at
/// the configured `max` — while the board is quiescent. Any real event resets
/// it to `standard`. This struct carries the three things that survive between
/// ticks; the state machine lives in [`maybe_emit_heartbeat`].
#[derive(Default)]
struct HeartbeatSchedule {
    /// When the next emission attempt is due. `None` means "not yet seeded"
    /// (first tick after start / after `off → on`).
    next_attempt: Option<Instant>,
    /// The interval currently in effect. Grows (doubles, capped at `max`) each
    /// idle tick while quiescent; snaps back to `standard` on any real event or
    /// whenever work is in flight. `None` until the first emission fixes it.
    interval: Option<Duration>,
    /// The `events.log` mtime as of our last action — seed, emit, or an
    /// observed event. A genuine external event is one whose write advances
    /// the mtime past this baseline; comparing against it (rather than a fixed
    /// window) is what tells a real event apart from our own heartbeat write.
    /// Advanced on seed / emit / reset so pre-existing history and an already-
    /// consumed event can't be mistaken for fresh activity.
    last_log_mtime: Option<SystemTime>,
}

/// Consider emitting one heartbeat for `project`. Called once per poller
/// tick. The rules from the spec land here:
///
/// 1. **Off** — `HeartbeatConfig::Off` skips every tick and also clears the
///    schedule so a project that toggles `heartbeat: off` while the poller is
///    running stops emitting immediately.
/// 2. **Crash-safe cadence** — the first attempt fires one standard interval
///    after the poller observed the config (not from the wall clock or the
///    previous run's last write), so a restart mid-interval doesn't catch up
///    missed slots.
/// 3. **Reset on any real event** — if `events.log` advanced past our own last
///    heartbeat write, a real event (task transition, workspace state change,
///    dispatch, user action) already woke the orchestrator: skip this emission
///    *and* reset the back-off to standard so the sweep is prompt again.
/// 4. **Adaptive back-off** — otherwise emit, then choose the next interval:
///    `standard` while any supervisable work is in flight (an active/ready/
///    review task — even one emitting no events, which is exactly when the
///    sweep earns its keep), or `min(interval * 2, max)` while the board is
///    quiescent. So a fully idle board relaxes `standard → 2× → 4× → … → max`.
/// 5. **Paused while offline** — if `is_online()` returns false at emit time,
///    skip silently. The schedule still advances (by the current interval, no
///    back-off change) so we don't probe every tick, and emission resumes on
///    the first due tick after connectivity is back.
fn maybe_emit_heartbeat(
    project: &Project,
    schedule: &mut HeartbeatSchedule,
    is_online: impl Fn() -> bool,
) {
    // `interval()` / `max()` both yield `Some` iff heartbeats are on, so this
    // one destructure gates the whole function on the config being enabled.
    let (Some(standard), Some(max)) = (project.heartbeat.interval(), project.heartbeat.max())
    else {
        // Heartbeat off — clear the schedule so flipping it back on re-seeds
        // from the next tick rather than firing a stale due.
        *schedule = HeartbeatSchedule::default();
        return;
    };

    let now = Instant::now();
    let current_interval = schedule.interval.unwrap_or(standard);
    let due = match schedule.next_attempt {
        None => {
            // First tick after start (or after the config flipped from
            // off → on): schedule the first attempt one standard interval out.
            // Record the current log mtime as the baseline so pre-existing
            // history isn't later mistaken for a fresh event.
            schedule.next_attempt = Some(now + standard);
            schedule.interval = Some(standard);
            schedule.last_log_mtime = events_log_mtime();
            return;
        }
        Some(t) => now >= t,
    };
    if !due {
        return;
    }

    // Reset on any real event: if events.log advanced past our baseline,
    // something real happened since our last action. That event already serves
    // as the orchestrator's trigger, so we skip this emission (debounce) and
    // reset the cadence to standard. Advance the baseline to the event's mtime
    // so the same event doesn't re-trigger a reset next tick.
    if external_event_since(schedule.last_log_mtime) {
        schedule.interval = Some(standard);
        schedule.next_attempt = Some(now + standard);
        schedule.last_log_mtime = events_log_mtime();
        return;
    }

    if !is_online() {
        // Offline: emit nothing (a heartbeat the orchestrator can't act on is
        // pure noise) but keep the schedule advancing so we don't probe every
        // supervisor tick. Leave the back-off level untouched.
        schedule.next_attempt = Some(now + current_interval);
        return;
    }

    // Both counts are cheap on-disk reads (task YAMLs + the events log, same
    // files the tick already touches) and are computed fresh so the heartbeat
    // is accurate at emit time. A read failure shouldn't sink the heartbeat —
    // fall back to 0, which the orchestrator treats as "nothing to do" (a
    // silent ack), the same as a genuinely quiet board.
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
    // Advance the baseline to our own write's mtime so the next tick can tell
    // it apart from a real event.
    schedule.last_log_mtime = events_log_mtime();

    // Choose the next cadence: hold at standard while there's supervisable
    // work in flight, back off (double, capped at max) while quiescent.
    let next_interval = if board_is_quiescent(project) {
        current_interval.saturating_mul(2).min(max).max(standard)
    } else {
        standard
    };
    schedule.interval = Some(next_interval);
    schedule.next_attempt = Some(now + next_interval);
}

/// True iff `events.log` advanced past `baseline` — i.e. a real event landed
/// since our last action (seed / emit / reset). `baseline` is the mtime
/// recorded at that action; the seed records it up front so pre-existing
/// history never counts as fresh activity. A `None` baseline paired with an
/// existing log means the log appeared *after* the seed observed none — that's
/// a genuine new event (this is the debounce-after-a-transition case).
fn external_event_since(baseline: Option<SystemTime>) -> bool {
    match (baseline, events_log_mtime()) {
        // Log advanced strictly past the baseline → a real event.
        (Some(prev), Some(cur)) => cur > prev,
        // Baseline saw no log, but one exists now → it was created since → new.
        (None, Some(_)) => true,
        // No log on disk → nothing has happened.
        (_, None) => false,
    }
}

/// True iff the board has no supervisable work in flight. "In flight" is any
/// task in an active (`in_progress`), ready/queued (`todo`), or handoff
/// (`review`) column — the positions a silently-stuck task can hide in, and
/// which cover a pending-load review and an in-flight Zen merge (both operate
/// on a `review` task). Backlog and done don't count: backlog is waiting, not
/// in flight, and done is terminal. A read error is treated as *not*
/// quiescent, so a transient failure never accelerates the back-off past the
/// standard cadence. Derived from the same `list_tasks` pass the payload makes.
fn board_is_quiescent(project: &Project) -> bool {
    let Ok(tasks) = shelbi_state::list_tasks(&project.name) else {
        return false;
    };
    tasks_are_quiescent(&tasks)
}

/// Pure core of [`board_is_quiescent`]. Split out so unit tests can drive it
/// with in-memory fixtures without touching disk or `SHELBI_HOME`.
fn tasks_are_quiescent(tasks: &[shelbi_state::TaskFile]) -> bool {
    !tasks.iter().any(|tf| {
        matches!(
            tf.task.column,
            Column::InProgress | Column::Todo | Column::Review
        )
    })
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

/// Last-modified time of `events.log`, or `None` if the file doesn't exist
/// yet. Any other I/O hiccup also maps to `None` — the caller treats "unknown
/// mtime" conservatively (see [`external_event_since`]), and a transient stat
/// failure shouldn't be read as a real event.
fn events_log_mtime() -> Option<SystemTime> {
    let path = events_log_path().ok()?;
    std::fs::metadata(&path).and_then(|m| m.modified()).ok()
}

fn poll_one(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    last_known: &mut Option<WorkspaceState>,
    last_dialog: &mut Option<String>,
    supervision: &mut SupervisionState,
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
    // hide the signal. `addr` is threaded in so a dev workspace whose task we
    // just promoted can close its own session immediately (spec §16).
    maybe_promote_to_review(project, workspace, machine, &host, &addr);

    // Agent-initiated status transition (bounce / send-back). A reviewer or
    // gate agent writes a transition marker naming its own task and a target
    // status; the poller validates the requested edge against the workflow and,
    // if allowed, applies it. Checked right after the forward review handoff and
    // independently of the pane title, for the same reason: nothing the agent's
    // UI does can hide a file-based signal.
    maybe_apply_transition(project, workspace, machine, &host, &addr);

    // Reaper sweep (spec §10): a review workspace with a live server pane but
    // no active task has leaked its bound port, which blocks the next
    // dispatch onto that slot. Runs every poll as the heartbeat backstop for a
    // missed teardown; a no-op for the common case (no server pane, or the
    // server's task is still active).
    maybe_reap_server_pane(project, workspace);

    // No pane → no marker. The display-message call would fail anyway,
    // but checking up-front keeps stderr noise out of the log.
    let alive = shelbi_orchestrator::workspace::workspace_pane_alive(&host, &addr).unwrap_or(false);

    // Auto-restart supervision runs off this same liveness read (it's the
    // backstop for a lost `pane_alive=false` event): a pane that crashed with
    // a task still assigned gets relaunched and re-dispatched here.
    maybe_supervise_workspace(project, workspace, &host, alive, supervision);

    if !alive {
        // The pane is gone (dispatch teardown, crash, or normal exit). Any
        // dialog we were tracking can't be "cleared" in a meaningful way —
        // pane death has its own `pane_alive=false` event — so just drop the
        // stuck-state so a respawned pane re-detects from scratch.
        *last_dialog = None;
        return;
    }

    // Pane-stall detection. One `capture-pane` sample feeds both detectors,
    // run on the same tick as the title read because a stalled pane keeps a
    // stale `shelbi:working` title — no hook fires — so the title path alone
    // can't see it. Best-effort: a capture failure leaves both untouched and
    // we fall through to the title path.
    let screen = shelbi_tmux::capture(&host, &addr).ok();

    // Usage-limit *pause* takes priority. Matched structurally against
    // claude's modal chrome (see `ready::detect_usage_limit`) rather than a
    // bare substring — so a pane that merely mentions "usage limit" (editing
    // this code, reading docs, an agent reasoning about the feature) never
    // trips it. A real stall drives a first-class `Paused` state whose ⏸ badge
    // overrides the stale title, so we skip the title path this tick; the
    // state reverts on the first poll after the limit lifts (which then rides
    // the ordinary `paused -> working` title transition below).
    if let Some(stall) = screen
        .as_deref()
        .and_then(shelbi_orchestrator::ready::detect_usage_limit)
    {
        record_usage_limit_pause(project, workspace, last_known, stall.reset.as_deref());
        // Pause supersedes any tracked advisory dialog.
        *last_dialog = None;
        return;
    }

    // Generic blocking-dialog advisory (trust / permission), deduped via
    // `last_dialog` so a still-open modal produces one event per incident.
    maybe_emit_dialog_event(project, workspace, screen.as_deref(), last_dialog);

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
    let prior = load_prior(&workspace.name, last_known);

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
            &project.name,
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

/// Match a pre-captured pane sample against the runner's blocking-dialog
/// signatures (trust / permission) and emit a `blocked reason=dialog:*` (or
/// recovery) line on a change of stuck-state. Deduped via `last_dialog` so a
/// still-open modal only produces one event per incident.
///
/// Usage-limit is handled separately, upstream, by the structural pause
/// detector — this function only sees the interactive human-gated modals.
///
/// Best-effort: an unknown runner or a missing sample (`None`, a transient
/// `capture-pane` failure) just leaves the stuck-state untouched and retries
/// next tick — we'd rather miss a beat than fabricate a recovery on a hiccup.
fn maybe_emit_dialog_event(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    screen: Option<&str>,
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

    // No sample this tick (capture failed) — leave the stuck-state untouched
    // rather than fabricating a recovery.
    let Some(screen) = screen else {
        return;
    };
    let detected = shelbi_orchestrator::ready::detect_blocking_dialog(screen, &signatures);

    let (events, next) = decide_dialog(last_dialog.as_deref(), detected.as_deref());
    for ev in events {
        if let Err(e) =
            append_workspace_dialog_event(&project.name, &workspace.name, &ev.kind, ev.blocked)
        {
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

/// Load the prior [`PriorState`] for a workspace, preferring the in-memory
/// `last_known` (this thread's own last observation) and falling back to the
/// on-disk `status.yaml` on first sighting after a hub restart. Shared by the
/// title path and the usage-limit pause path so both bootstrap identically and
/// a restart doesn't emit a bogus `none -> X` for state already recorded.
fn load_prior(workspace_name: &str, last_known: &Option<WorkspaceState>) -> Option<PriorState> {
    match *last_known {
        Some(s) => Some(PriorState {
            state: s,
            last_transition: load_workspace_status(workspace_name)
                .ok()
                .flatten()
                .map(|s| s.last_transition),
        }),
        None => load_workspace_status(workspace_name)
            .ok()
            .flatten()
            .map(|s| PriorState {
                state: s.state,
                last_transition: Some(s.last_transition),
            }),
    }
}

/// Record a usage-limit stall as a first-class [`WorkspaceState::Paused`]:
/// persist the paused status (so the sidebar renders the ⏸ badge) and, on the
/// edge *into* the stall, emit one `... -> paused reason=usage-limit` line
/// (carrying the reset-time hint when claude showed one) so the activity feed
/// and the orchestrator both see the slot go quiet on the clock.
///
/// Dedupe rides the ordinary [`decide`] transition machinery: while the pane
/// stays on the limit, subsequent polls observe `prev == Paused == new`, so
/// only `last_seen` moves and no further event fires. The *resume* edge is not
/// emitted here — once the limit lifts the poll falls through to the title
/// path, which records the `paused -> working` transition off the live marker.
fn record_usage_limit_pause(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    last_known: &mut Option<WorkspaceState>,
    reset: Option<&str>,
) {
    let prior = load_prior(&workspace.name, last_known);
    let current_task = current_task_for(project, &workspace.name);
    let outcome = decide(
        &workspace.name,
        current_task,
        prior,
        WorkspaceState::Paused,
        Utc::now(),
    );

    if let Err(e) = save_workspace_status(&outcome.status) {
        tracing::warn!(workspace = %workspace.name, error = %e, "save_workspace_status failed");
    }
    if outcome.transitioned {
        if let Err(e) = append_workspace_pause_event(
            &project.name,
            &workspace.name,
            outcome.prev_state,
            reset,
        ) {
            tracing::warn!(workspace = %workspace.name, error = %e, "append_workspace_pause_event failed");
        }
        tracing::info!(
            workspace = %workspace.name,
            reset = ?reset,
            "workspace paused on usage limit",
        );
    }

    *last_known = Some(WorkspaceState::Paused);
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
///
/// On a real promotion this also **closes the dev workspace's session**
/// immediately (spec §16): the branch is safely handed off, so the finished
/// pane has no reason to linger and surface a completion glyph. Ordering is
/// load-bearing — the close happens only AFTER the rebase + column move, never
/// before, so work can't be stranded. Loading the promoted task onto a review
/// workspace (or queueing it) is the orchestrator's job, reacting to the
/// `to_category=handoff` event this move emits.
fn maybe_promote_to_review(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
    addr: &shelbi_core::TmuxAddr,
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

            // Close the dev session on completion (spec §16). The branch is
            // now safely promoted, so the finished pane has no reason to
            // linger — killing it here frees the slot immediately and keeps a
            // "done" glyph from lingering on the workspace row (the sidebar
            // represents completion entirely in the review sections). We only
            // do this for dev workspaces: a review workspace reaches Review by
            // being *loaded* onto, not by promoting an in-progress task, so it
            // never lands in this arm — the `!is_review()` guard is defensive.
            // Best-effort — a stuck kill must not block the marker clear below,
            // and `kill_workspace_pane` is idempotent, so a pane already gone
            // is a silent no-op.
            if !workspace.is_review() {
                if let Err(e) = shelbi_orchestrator::workspace::kill_workspace_pane(
                    host,
                    addr,
                    &workspace.name,
                ) {
                    tracing::warn!(
                        workspace = %workspace.name,
                        task = %task_id,
                        error = %e,
                        "close dev session on completion failed",
                    );
                } else {
                    tracing::info!(
                        workspace = %workspace.name,
                        task = %task_id,
                        "closed dev session on completion; workspace idle",
                    );
                }
            }
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

/// The board move an agent-transition request resolves to, or the reason it was
/// refused. Pure data produced by [`decide_transition`] so the validation +
/// resolution logic is unit-testable without touching disk.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TransitionDecision {
    /// The requested edge is allowed. Move the task to `to_column` and fire the
    /// `from_status -> to_status` edge's declared actions. `from_status` /
    /// `to_status` are the workflow status ids (verb expanded), needed to look
    /// up the edge's `actions:`.
    Apply {
        from_status: String,
        to_status: String,
        to_column: Column,
    },
    /// The request is refused — `reason` is a short token for the log. The task
    /// is NOT moved; the marker is still consumed so a persistently-illegal
    /// request doesn't re-log every tick.
    Reject { reason: &'static str },
}

/// Map a workflow [`StatusCategory`] onto the legacy [`Column`] a task's
/// position is actually stored in. The 5-column model has no dedicated
/// `Archived` lane, so a terminal `archived` status folds onto `Done` (both are
/// terminal and non-dispatchable) — the closest faithful home in the current
/// storage model. This is the inverse of [`Column::category`].
fn column_for_category(category: StatusCategory) -> Column {
    match category {
        StatusCategory::Backlog => Column::Backlog,
        StatusCategory::Ready => Column::Todo,
        StatusCategory::Active => Column::InProgress,
        StatusCategory::Handoff => Column::Review,
        StatusCategory::Done | StatusCategory::Archived => Column::Done,
    }
}

/// Resolve the workflow status id a task in `current_column` currently occupies.
/// Mirrors the orchestrator's `resolve_task_status` (and the TUI's
/// `resolve_task_status`): prefer an id match on the column's canonical status
/// id, then fall back to the first status sharing the column's category (so a
/// workflow that renamed `review` to `qa` still resolves), and finally to the
/// canonical id string when the workflow declares nothing compatible.
fn resolve_current_status_id(workflow: &Workflow, current_column: Column) -> String {
    let canonical = current_column.default_status_id();
    if workflow.status(canonical).is_some() {
        return canonical.to_string();
    }
    let cat = current_column.category();
    workflow
        .statuses
        .iter()
        .find(|s| s.category == cat)
        .map(|s| s.id.clone())
        .unwrap_or_else(|| canonical.to_string())
}

/// Pure core of the agent-transition handler. Given the task's current column,
/// its workflow, and the raw target the agent wrote (a status id, or the
/// `reject` / `bounce` verb sugar), decide whether the requested edge is legal
/// and where it lands.
///
/// Validation, in order:
///
/// 1. **Verb expansion.** `reject` / `bounce` resolve to the workflow's
///    designated active (`active`-category) status — the "send it back to be
///    reworked" target. A workflow with no active status can't be bounced into,
///    so the request is refused.
/// 2. **Target must be declared.** The resolved target must be a status id the
///    workflow declares.
/// 3. **Edge must be permitted.** If the workflow declares a `transitions:`
///    block, the `current -> target` edge must appear in it — agents may only
///    take edges the workflow author sanctioned. With no `transitions:` block,
///    moves are any-to-any (both statuses just have to be declared), matching
///    [`Workflow::transition_allowed`].
/// 4. **Must actually move.** The 5-column store can't represent a move between
///    two statuses that share a column, so a target resolving to the current
///    column is refused as a no-op (guarding against unassigning a task that
///    wouldn't actually change lanes).
fn decide_transition(
    workflow: &Workflow,
    current_column: Column,
    raw_target: &str,
) -> TransitionDecision {
    let from_status = resolve_current_status_id(workflow, current_column);

    let to_status = match raw_target {
        "reject" | "bounce" => match workflow
            .statuses
            .iter()
            .find(|s| s.category == StatusCategory::Active)
        {
            Some(s) => s.id.clone(),
            None => return TransitionDecision::Reject { reason: "no-active-status" },
        },
        other => other.to_string(),
    };

    let Some(target) = workflow.status(&to_status) else {
        return TransitionDecision::Reject { reason: "unknown-target-status" };
    };

    let allowed = match &workflow.transitions {
        Some(ts) => ts.iter().any(|t| t.from == from_status && t.to == to_status),
        None => true,
    };
    if !allowed {
        return TransitionDecision::Reject { reason: "edge-not-in-workflow" };
    }

    let to_column = column_for_category(target.category);
    if to_column == current_column {
        return TransitionDecision::Reject { reason: "target-is-current-column" };
    }

    TransitionDecision::Apply {
        from_status,
        to_status,
        to_column,
    }
}

/// Check the workspace's agent-transition marker and, if it names this
/// workspace's own task and requests an edge the workflow permits, apply the
/// move. The generalization of [`maybe_promote_to_review`] to arbitrary
/// (including backward) status transitions — see
/// [`shelbi_orchestrator::workspace::workspace_transition_marker`] for the
/// marker path + format.
///
/// Best-effort and idempotent, matching the review-marker contract:
///
/// - A marker naming a task **not owned by this workspace** (stale worktree
///   reuse, or a foreign id a stray program dropped) is cleared without moving.
/// - A move whose **workflow forbids the edge** is refused (logged) and the
///   marker cleared, so a persistently-illegal request doesn't re-log forever.
/// - A successful move emits a `reason=workspace:agent-transition` task event,
///   fires any actions the edge declares (a bounce edge typically has none),
///   then **closes the finishing pane** so the orchestrator re-dispatches the
///   task fresh onto an appropriate workspace off the resulting event. Ordering
///   is load-bearing: move first, then close, never strand work — the move also
///   clears the task's owner ([`shelbi_state::move_task_and_unassign`]) so the
///   just-closed pane isn't seen by the supervisor as still holding active work.
/// - Only a genuine move *failure* leaves the marker in place to retry; every
///   other path consumes it exactly once.
fn maybe_apply_transition(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
    addr: &shelbi_core::TmuxAddr,
) {
    let marker = shelbi_orchestrator::workspace::workspace_transition_marker(machine, workspace);
    let req = match shelbi_orchestrator::workspace::read_transition_marker(host, &marker) {
        Ok(Some(r)) => r,
        Ok(None) => return,
        Err(e) => {
            // Torn / hostile body — leave the marker (read didn't clear it) so a
            // half-flushed write survives to the next tick.
            tracing::warn!(workspace = %workspace.name, error = %e, "read_transition_marker failed");
            return;
        }
    };

    let task_file = shelbi_state::load_task(&project.name, &req.task_id);
    match &task_file {
        Ok(tf) if tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()) => {
            // Load the task's workflow; fall back to the built-in default
            // (no transitions → any-to-any) if the YAML is missing or invalid,
            // so a transient workflow typo doesn't wedge the bounce.
            let workflow = shelbi_state::load_workflow(&project.name, tf.task.workflow_or_default())
                .unwrap_or_else(|_| default_workflow());

            match decide_transition(&workflow, tf.task.column, &req.target) {
                TransitionDecision::Reject { reason } => {
                    tracing::warn!(
                        workspace = %workspace.name,
                        task = %req.task_id,
                        target = %req.target,
                        reason,
                        "agent transition rejected; clearing marker without moving",
                    );
                }
                TransitionDecision::Apply {
                    from_status,
                    to_status,
                    to_column,
                } => {
                    match shelbi_state::move_task_and_unassign(&project.name, &req.task_id, to_column)
                    {
                        Ok(Some((from, to, wf))) => {
                            if let Err(e) = shelbi_state::append_task_event(
                                &req.task_id,
                                &wf,
                                from,
                                to,
                                "workspace:agent-transition",
                            ) {
                                tracing::warn!(workspace = %workspace.name, task = %req.task_id, error = %e, "append_task_event failed");
                            }

                            // Fire any side-effect actions the edge declares
                            // (a bounce edge typically declares none; a gate
                            // that closes a PR on send-back would attach
                            // `close_pr`). Reuses the same primitive path the
                            // workflow engine walks. Best-effort — the move
                            // already happened, so an action failure logs but
                            // doesn't roll it back.
                            match shelbi_orchestrator::transition::execute_transition(
                                project,
                                &project.name,
                                &tf.task,
                                &tf.body,
                                &workflow,
                                &from_status,
                                &to_status,
                            ) {
                                Ok(outcomes) => {
                                    for o in outcomes {
                                        tracing::info!(
                                            workspace = %workspace.name,
                                            task = %req.task_id,
                                            action = %o.action,
                                            line = %o.line,
                                            "agent-transition action fired",
                                        );
                                    }
                                }
                                Err(e) => {
                                    tracing::warn!(workspace = %workspace.name, task = %req.task_id, error = %e, "agent-transition action failed");
                                }
                            }

                            tracing::info!(
                                workspace = %workspace.name,
                                task = %req.task_id,
                                from = %from_status,
                                to = %to_status,
                                "applied agent transition via marker",
                            );

                            // Close the finishing pane so the slot frees and the
                            // orchestrator re-dispatches fresh off the emitted
                            // event. Best-effort and idempotent — a pane already
                            // gone is a silent no-op — and it runs only AFTER the
                            // move so work is never stranded.
                            if let Err(e) = shelbi_orchestrator::workspace::kill_workspace_pane(
                                host,
                                addr,
                                &workspace.name,
                            ) {
                                tracing::warn!(workspace = %workspace.name, task = %req.task_id, error = %e, "close pane after agent transition failed");
                            } else {
                                tracing::info!(workspace = %workspace.name, task = %req.task_id, "closed pane after agent transition; workspace idle");
                            }
                        }
                        Ok(None) => {
                            // Already in the target column — `decide_transition`
                            // rejects same-column targets, so this is a rare
                            // race (the task moved between our load and the
                            // move). Nothing to do but clear the marker.
                            tracing::debug!(workspace = %workspace.name, task = %req.task_id, "agent transition no-op (task already at target); clearing marker");
                        }
                        Err(e) => {
                            // Leave the marker in place so we retry next tick.
                            tracing::warn!(workspace = %workspace.name, task = %req.task_id, error = %e, "move_task_and_unassign failed");
                            return;
                        }
                    }
                }
            }
        }
        Ok(_) => {
            tracing::debug!(workspace = %workspace.name, task = %req.task_id, "stale/foreign transition marker (task not owned by this workspace); clearing");
        }
        Err(e) => {
            tracing::warn!(workspace = %workspace.name, task = %req.task_id, error = %e, "transition marker names unloadable task; clearing");
        }
    }

    if let Err(e) = shelbi_orchestrator::workspace::clear_transition_marker(host, &marker) {
        tracing::warn!(workspace = %workspace.name, error = %e, "clear_transition_marker failed");
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

/// Auto-restart supervision for one workspace's agent pane, run every poll
/// tick off the same `alive` read the poll loop already took.
///
/// Local panes only: a remote workspace has no lifecycle wrapper to drop the
/// no-restart marker, so we couldn't tell a crash from a clean exit there and
/// would risk relaunching a pane the user deliberately closed. When the pane
/// is dead we gather the two discriminators the pure state machine needs —
/// was the shutdown deliberate (a fresh no-restart marker), and is there
/// still an active task to keep it up for — then act on its verdict:
/// relaunch + re-dispatch the same task (the card never leaves its active
/// status), emit a `supervision=gave-up reason=crash-loop` line when the
/// crash-loop cap trips, or do nothing.
fn maybe_supervise_workspace(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    host: &shelbi_core::Host,
    alive: bool,
    state: &mut SupervisionState,
) {
    if !matches!(host, shelbi_core::Host::Local) {
        return;
    }

    let (intentional_shutdown, task_id) = if alive {
        (false, None)
    } else {
        // Consume the supervisor's dedicated no-restart marker (distinct from
        // the wrapper's expected-teardown, which the wrapper already ate).
        let intentional = shelbi_state::consume_expected_teardown(
            &shelbi_state::supervision_shutdown_key(&workspace.name),
        )
        .unwrap_or(false);
        (intentional, current_task_for(project, &workspace.name))
    };

    let inputs = SupervisionInputs {
        alive,
        intentional_shutdown,
        has_work: task_id.is_some(),
    };
    match state.decide(&inputs, Instant::now()) {
        SupervisionAction::None => {}
        SupervisionAction::Restart => {
            // `Restart` is only returned for a dead pane with work, so a task
            // id is present here; the guard is defensive.
            let Some(task_id) = task_id else { return };
            match redispatch_workspace(project, workspace, &task_id) {
                Ok(()) => {
                    if let Err(e) = shelbi_state::append_supervision_event(
                        &project.name,
                        Some(&workspace.name),
                        "restart",
                        "crash",
                    ) {
                        tracing::warn!(workspace = %workspace.name, error = %e, "append_supervision_event failed");
                    }
                    tracing::info!(
                        workspace = %workspace.name,
                        task = %task_id,
                        "supervisor relaunched crashed workspace pane and re-dispatched task",
                    );
                }
                Err(e) => {
                    if let Err(le) = shelbi_state::append_supervision_event(
                        &project.name,
                        Some(&workspace.name),
                        "restart-failed",
                        &e,
                    ) {
                        tracing::warn!(workspace = %workspace.name, error = %le, "append_supervision_event failed");
                    }
                    tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "supervisor relaunch failed");
                }
            }
        }
        SupervisionAction::GiveUp => {
            if let Err(e) = shelbi_state::append_supervision_event(
                &project.name,
                Some(&workspace.name),
                "gave-up",
                "crash-loop",
            ) {
                tracing::warn!(workspace = %workspace.name, error = %e, "append_supervision_event failed");
            }
            tracing::warn!(
                workspace = %workspace.name,
                "supervisor gave up restarting workspace pane after the crash-loop cap; left for the user",
            );
        }
    }
}

/// Relaunch `workspace` on `task_id`, re-sending the task prompt. Reuses the
/// normal dispatch primitive ([`start_workspace_on_task`], which itself
/// kill-then-respawns the pane and re-pastes the prompt), so a supervised
/// restart is byte-for-byte the same as a fresh `task start` minus the board
/// move — the card is already in its active status and stays there.
fn redispatch_workspace(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    task_id: &str,
) -> std::result::Result<(), String> {
    let tf = shelbi_state::load_task(&project.name, task_id).map_err(|e| e.to_string())?;
    let branch = tf
        .task
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{task_id}"));
    let agent = shelbi_orchestrator::dispatch::resolve_active_agent(&project.name, &tf.task);
    shelbi_orchestrator::workspace::start_workspace_on_task(
        shelbi_orchestrator::workspace::StartSpec {
            project,
            workspace,
            task_id,
            branch: &branch,
            task_body: &tf.body,
            agent: Some(&agent),
        },
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Auto-restart supervision for the project's orchestrator pane, run once
/// per supervisor tick. Unlike a workspace it has no idle state and no
/// deliberate-shutdown marker: while its session is alive it should always be
/// running, and a real quit tears down the whole session (killing this poller
/// with it), so any orchestrator death this can observe is a crash. Relaunch
/// is [`ensure_dashboard`], whose `__zen-orch-start` step keeps the Zen
/// crash-recovery downgrade intact — a restarted orchestrator still comes up
/// with Zen off.
fn maybe_supervise_orchestrator(project: &Project, state: &mut SupervisionState) {
    let alive = shelbi_orchestrator::orchestrator_pane_alive(&project.name).unwrap_or(true);
    let inputs = SupervisionInputs {
        alive,
        intentional_shutdown: false,
        has_work: true,
    };
    match state.decide(&inputs, Instant::now()) {
        SupervisionAction::None => {}
        SupervisionAction::Restart => match shelbi_orchestrator::ensure_dashboard(&project.name) {
            Ok(_) => {
                if let Err(e) =
                    shelbi_state::append_supervision_event(&project.name, None, "restart", "crash")
                {
                    tracing::warn!(project = %project.name, error = %e, "append_supervision_event failed");
                }
                tracing::info!(project = %project.name, "supervisor relaunched crashed orchestrator pane");
            }
            Err(e) => {
                if let Err(le) = shelbi_state::append_supervision_event(
                    &project.name,
                    None,
                    "restart-failed",
                    &e.to_string(),
                ) {
                    tracing::warn!(project = %project.name, error = %le, "append_supervision_event failed");
                }
                tracing::warn!(project = %project.name, error = %e, "supervisor orchestrator relaunch failed");
            }
        },
        SupervisionAction::GiveUp => {
            if let Err(e) =
                shelbi_state::append_supervision_event(&project.name, None, "gave-up", "crash-loop")
            {
                tracing::warn!(project = %project.name, error = %e, "append_supervision_event failed");
            }
            tracing::warn!(project = %project.name, "supervisor gave up relaunching orchestrator pane after the crash-loop cap");
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

    #[test]
    fn usage_limit_pause_transitions_and_clears_via_decide() {
        // Into the stall: Working -> Paused is a transition (the poller emits
        // the `-> paused reason=usage-limit` line on this edge).
        let prior = Some(PriorState {
            state: WorkspaceState::Working,
            last_transition: Some(ts(50)),
        });
        let paused = decide("alpha", Some("t".into()), prior, WorkspaceState::Paused, ts(100));
        assert!(paused.transitioned);
        assert_eq!(paused.prev_state, Some(WorkspaceState::Working));
        assert_eq!(paused.status.state, WorkspaceState::Paused);
        assert_eq!(paused.status.last_transition, ts(100));

        // Still stalled: Paused -> Paused is deduped (only last_seen moves), so
        // a long limit produces exactly one event per incident.
        let still = Some(PriorState {
            state: WorkspaceState::Paused,
            last_transition: Some(ts(100)),
        });
        let out = decide("alpha", Some("t".into()), still, WorkspaceState::Paused, ts(160));
        assert!(!out.transitioned);
        assert_eq!(out.status.last_transition, ts(100));
        assert_eq!(out.status.last_seen, ts(160));

        // Resume: the limit lifts, the live marker reads working again, and
        // Paused -> Working transitions — this is the clear-on-resume edge the
        // title path emits (`paused -> working`) so the ⏸ badge reverts.
        let resumed = decide("alpha", Some("t".into()), still, WorkspaceState::Working, ts(200));
        assert!(resumed.transitioned);
        assert_eq!(resumed.prev_state, Some(WorkspaceState::Paused));
        assert_eq!(resumed.status.state, WorkspaceState::Working);
        assert_eq!(resumed.status.last_transition, ts(200));
    }

    use shelbi_core::{
        AgentRunnerSpec, Host, Machine, MachineKind, OrchestratorSpec, Task, TmuxAddr,
        WorkspaceSpec,
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

        maybe_promote_to_review(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr { session: "s".into(), window: "w".into() },
        );

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
        maybe_promote_to_review(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr { session: "s".into(), window: "w".into() },
        );
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

        maybe_promote_to_review(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr { session: "s".into(), window: "w".into() },
        );

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

        maybe_promote_to_review(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr { session: "s".into(), window: "w".into() },
        );

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
        maybe_promote_to_review(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr { session: "s".into(), window: "w".into() },
        );
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
        project.heartbeat = shelbi_core::HeartbeatConfig::every(Duration::from_millis(1));

        let mut hb = HeartbeatSchedule::default();
        // First call seeds the schedule and returns without writing.
        maybe_emit_heartbeat(&project, &mut hb, || true);
        assert!(hb.next_attempt.is_some(), "first tick must seed the schedule");
        let log = shelbi_state::events_log_path().unwrap();
        assert!(
            !log.exists() || std::fs::read_to_string(&log).unwrap().is_empty(),
            "first tick must not emit a heartbeat"
        );

        // Wait past the interval, with no other writer touching the
        // log, and the next attempt emits one line.
        std::thread::sleep(Duration::from_millis(20));
        maybe_emit_heartbeat(&project, &mut hb, || true);
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
        project.heartbeat = shelbi_core::HeartbeatConfig::every(Duration::from_secs(1));

        let mut hb = HeartbeatSchedule::default();
        // Seed.
        maybe_emit_heartbeat(&project, &mut hb, || true);
        // Force the next attempt to be due immediately, but write a real
        // event first so the reset-on-event debounce trips (events.log
        // advanced past our last — here, never-emitted — heartbeat).
        shelbi_state::append_workspace_event("demo", "alpha", None, WorkspaceState::Working)
            .unwrap();
        hb.next_attempt = Some(Instant::now());

        maybe_emit_heartbeat(&project, &mut hb, || true);

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

        let mut hb = HeartbeatSchedule {
            next_attempt: Some(Instant::now() - Duration::from_secs(60)),
            interval: Some(Duration::from_secs(180)),
            last_log_mtime: None,
        };
        maybe_emit_heartbeat(&project, &mut hb, || true);
        assert!(hb.next_attempt.is_none(), "off must clear the pending schedule");
        assert!(hb.interval.is_none(), "off must clear the back-off level");

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
        project.heartbeat = shelbi_core::HeartbeatConfig::every(Duration::from_millis(1));

        let mut hb = HeartbeatSchedule::default();
        // Seed, then drive several "due" attempts while offline.
        maybe_emit_heartbeat(&project, &mut hb, || false);
        for _ in 0..3 {
            std::thread::sleep(Duration::from_millis(5));
            maybe_emit_heartbeat(&project, &mut hb, || false);
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
        maybe_emit_heartbeat(&project, &mut hb, || true);
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

    fn todo_task(id: &str) -> Task {
        let mut t = in_progress_task(id, "alpha");
        t.column = Column::Todo;
        t.assigned_to = None;
        t
    }

    fn review_task(id: &str) -> Task {
        let mut t = in_progress_task(id, "alpha");
        t.column = Column::Review;
        t.assigned_to = None;
        t
    }

    fn tf(task: Task) -> shelbi_state::TaskFile {
        shelbi_state::TaskFile {
            task,
            body: String::new(),
        }
    }

    #[test]
    fn tasks_are_quiescent_only_when_no_active_ready_or_review_work() {
        // Empty board → quiescent.
        assert!(tasks_are_quiescent(&[]));
        // Backlog-only and done-only boards are quiescent: neither is
        // supervisable work in flight.
        assert!(tasks_are_quiescent(&[
            tf({
                let mut t = todo_task("b");
                t.column = Column::Backlog;
                t
            }),
            tf({
                let mut t = todo_task("d");
                t.column = Column::Done;
                t
            }),
        ]));
        // Any active / ready / review task breaks quiescence.
        assert!(!tasks_are_quiescent(&[tf(in_progress_task("a", "alpha"))]));
        assert!(!tasks_are_quiescent(&[tf(todo_task("r"))]));
        assert!(!tasks_are_quiescent(&[tf(review_task("v"))]));
        // Mixed backlog + one review task → still not quiescent.
        assert!(!tasks_are_quiescent(&[
            tf({
                let mut t = todo_task("b");
                t.column = Column::Backlog;
                t
            }),
            tf(review_task("v")),
        ]));
    }

    /// Force `hb` due and run one consideration. Returns the interval that is
    /// now in effect after the call (the back-off level for the *next* gap).
    fn force_tick(project: &Project, hb: &mut HeartbeatSchedule) -> Option<Duration> {
        hb.next_attempt = Some(Instant::now());
        maybe_emit_heartbeat(project, hb, || true);
        hb.interval
    }

    fn hb_line_count() -> usize {
        let log = shelbi_state::events_log_path().unwrap();
        std::fs::read_to_string(&log)
            .map(|b| b.lines().filter(|l| l.contains("heartbeat")).count())
            .unwrap_or(0)
    }

    #[test]
    fn maybe_emit_heartbeat_backs_off_exponentially_while_quiescent() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-backoff-{}-{}",
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
        // Standard 1s, cap 8s → the interval should climb 1→2→4→8 and pin.
        project.heartbeat = shelbi_core::HeartbeatConfig::On {
            interval: Duration::from_secs(1),
            max: Duration::from_secs(8),
        };
        // Empty board → quiescent every tick.

        let mut hb = HeartbeatSchedule::default();
        maybe_emit_heartbeat(&project, &mut hb, || true); // seed
        assert_eq!(hb.interval, Some(Duration::from_secs(1)), "seeded at standard");

        // Each forced tick emits once (quiescent) and doubles the cadence,
        // capped at max.
        assert_eq!(force_tick(&project, &mut hb), Some(Duration::from_secs(2)));
        assert_eq!(force_tick(&project, &mut hb), Some(Duration::from_secs(4)));
        assert_eq!(force_tick(&project, &mut hb), Some(Duration::from_secs(8)));
        assert_eq!(force_tick(&project, &mut hb), Some(Duration::from_secs(8)));
        assert_eq!(
            force_tick(&project, &mut hb),
            Some(Duration::from_secs(8)),
            "interval pins at the cap"
        );
        // Five quiescent emissions, one per forced tick.
        assert_eq!(hb_line_count(), 5, "one heartbeat per quiescent tick");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_holds_standard_while_work_in_flight() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-inflight-{}-{}",
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
        project.heartbeat = shelbi_core::HeartbeatConfig::On {
            interval: Duration::from_secs(1),
            max: Duration::from_secs(8),
        };
        // An in-progress task assigned to alpha → never quiescent, even though
        // it emits no events of its own. This is exactly the case the sweep
        // exists for.
        shelbi_state::save_task("demo", &in_progress_task("fix-login", "alpha"), "body").unwrap();

        let mut hb = HeartbeatSchedule::default();
        maybe_emit_heartbeat(&project, &mut hb, || true); // seed

        // The cadence must never leave standard while work is in flight.
        for _ in 0..4 {
            assert_eq!(
                force_tick(&project, &mut hb),
                Some(Duration::from_secs(1)),
                "cadence must stay at standard while a task is in flight"
            );
        }

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_emits_despite_preexisting_log_history() {
        // A poller that starts on a board whose events.log already carries old
        // history must still emit its first heartbeat — the seed captures the
        // existing mtime as the baseline, so stale history isn't mistaken for a
        // fresh event and the sweep isn't permanently debounced.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-history-{}-{}",
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
        project.heartbeat = shelbi_core::HeartbeatConfig::every(Duration::from_millis(1));

        // Pre-existing history, then a beat so its mtime is safely in the past.
        shelbi_state::append_workspace_event("demo", "alpha", None, WorkspaceState::Working)
            .unwrap();
        std::thread::sleep(Duration::from_millis(10));

        let mut hb = HeartbeatSchedule::default();
        maybe_emit_heartbeat(&project, &mut hb, || true); // seed captures baseline
        let before = hb_line_count();

        // Force a due tick with no new writes: must emit, not debounce forever.
        hb.next_attempt = Some(Instant::now());
        maybe_emit_heartbeat(&project, &mut hb, || true);
        assert_eq!(
            hb_line_count(),
            before + 1,
            "first heartbeat must fire despite pre-existing events.log history"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn maybe_emit_heartbeat_resets_to_standard_on_event_mid_backoff() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-hb-reset-{}-{}",
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
        project.heartbeat = shelbi_core::HeartbeatConfig::On {
            interval: Duration::from_secs(1),
            max: Duration::from_secs(8),
        };

        let mut hb = HeartbeatSchedule::default();
        maybe_emit_heartbeat(&project, &mut hb, || true); // seed

        // Climb into the back-off on a quiescent board.
        force_tick(&project, &mut hb);
        assert_eq!(force_tick(&project, &mut hb), Some(Duration::from_secs(4)));
        let before = hb_line_count();

        // A real event lands mid-backoff (a workspace transition). It advances
        // events.log past our last heartbeat write. Sleep briefly first so the
        // mtime is strictly newer even on a coarse-resolution filesystem.
        std::thread::sleep(Duration::from_millis(10));
        shelbi_state::append_workspace_event("demo", "alpha", None, WorkspaceState::Working)
            .unwrap();

        // Next consideration: the event already woke the orchestrator, so we
        // skip the emission AND snap the cadence back to standard.
        assert_eq!(
            force_tick(&project, &mut hb),
            Some(Duration::from_secs(1)),
            "a real event must reset the cadence to standard"
        );
        assert_eq!(
            hb_line_count(),
            before,
            "reset tick must not emit its own heartbeat (debounced by the event)"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // ---------------------------------------------------------------------
    // Agent-transition decision core

    /// A workflow WITH an explicit `transitions:` block. Declares only the
    /// `review -> in-progress` bounce edge (plus the forward `review -> done`),
    /// so any other edge out of `review` is refused for an agent request.
    fn workflow_with_bounce() -> Workflow {
        let yaml = r#"
name: gated
statuses:
  - { id: backlog,     name: Backlog,     category: backlog, owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,        category: ready,   owner: agent, agent: orchestrator }
  - { id: in-progress, name: In Progress, category: active,  owner: agent, agent: developer    }
  - { id: review,      name: Review,      category: handoff, owner: user,  agent: reviewer     }
  - { id: done,        name: Done,        category: done,    owner: user                       }
transitions:
  - { from: review, to: in-progress }
  - { from: review, to: done, actions: [merge] }
"#;
        Workflow::from_yaml_str(yaml).expect("workflow parses")
    }

    #[test]
    fn decide_transition_accepts_declared_bounce_edge() {
        // review -> in-progress is a declared edge → applied, landing in the
        // active column.
        let wf = workflow_with_bounce();
        assert_eq!(
            decide_transition(&wf, Column::Review, "in-progress"),
            TransitionDecision::Apply {
                from_status: "review".into(),
                to_status: "in-progress".into(),
                to_column: Column::InProgress,
            }
        );
    }

    #[test]
    fn decide_transition_resolves_reject_verb_to_active_status() {
        // The `reject` / `bounce` sugar resolves to the workflow's active
        // status, and that edge is declared → applied.
        let wf = workflow_with_bounce();
        for verb in ["reject", "bounce"] {
            assert_eq!(
                decide_transition(&wf, Column::Review, verb),
                TransitionDecision::Apply {
                    from_status: "review".into(),
                    to_status: "in-progress".into(),
                    to_column: Column::InProgress,
                },
                "verb {verb} should resolve to the active status",
            );
        }
    }

    #[test]
    fn decide_transition_rejects_edge_absent_from_transitions_block() {
        // review -> backlog is a legal status pair but NOT a declared edge, and
        // this workflow declares a transitions block → agents may only take
        // sanctioned edges, so it's refused and the task must not move.
        let wf = workflow_with_bounce();
        assert_eq!(
            decide_transition(&wf, Column::Review, "backlog"),
            TransitionDecision::Reject { reason: "edge-not-in-workflow" }
        );
    }

    #[test]
    fn decide_transition_rejects_unknown_target_status() {
        let wf = workflow_with_bounce();
        assert_eq!(
            decide_transition(&wf, Column::Review, "nonexistent"),
            TransitionDecision::Reject { reason: "unknown-target-status" }
        );
    }

    #[test]
    fn decide_transition_any_to_any_when_no_transitions_block() {
        // The default workflow declares NO transitions block, so moves are
        // any-to-any (both statuses just have to be declared). A bounce from
        // review back to in-progress is allowed.
        let wf = default_workflow();
        assert_eq!(
            decide_transition(&wf, Column::Review, "in-progress"),
            TransitionDecision::Apply {
                from_status: "review".into(),
                to_status: "in-progress".into(),
                to_column: Column::InProgress,
            }
        );
        // The verb sugar works there too.
        assert_eq!(
            decide_transition(&wf, Column::Review, "bounce"),
            TransitionDecision::Apply {
                from_status: "review".into(),
                to_status: "in-progress".into(),
                to_column: Column::InProgress,
            }
        );
    }

    #[test]
    fn decide_transition_rejects_same_column_noop() {
        // A target resolving to the column the task is already in can't be
        // represented as a move in the 5-column store → refused (so we never
        // unassign a task that wouldn't actually change lanes).
        let wf = default_workflow();
        assert_eq!(
            decide_transition(&wf, Column::InProgress, "in-progress"),
            TransitionDecision::Reject { reason: "target-is-current-column" }
        );
    }

    #[test]
    fn column_for_category_is_inverse_of_column_category() {
        // Every column round-trips through category → column …
        for col in Column::ALL {
            assert_eq!(column_for_category(col.category()), col);
        }
        // … and the archived category (no dedicated lane) folds onto Done.
        assert_eq!(column_for_category(StatusCategory::Archived), Column::Done);
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
