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
//! `<worktree>/.claude/shelbi-ready` is checked first, before any
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
//!   state clears from a verified resume or fresh live busy/input evidence
//!   after the limit lifts, so the orchestrator can hold new work off a limited
//!   slot until it frees up without trusting Claude's stale pane title. The
//!   detection also arms an **auto-resume**: the
//!   reset time is parsed from the banner and, shortly after it passes, the
//!   poller nudges the pane back to work with a verified-submitted resume
//!   prompt (see [`LimitResumeState`]), so a limited worker doesn't sit idle
//!   until a human notices. Unparseable banners degrade to a
//!   `status=needs-human` warning instead of guessing a resume time.
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
    append_heartbeat_event, append_limit_resume_event, append_rebase_event,
    append_workspace_dialog_event, append_workspace_event, append_workspace_pause_event,
    append_worktree_detach_event, events_log_path, load_workspace_status, parse_pane_title_marker,
    read_state, read_zenmode_summary, save_workspace_status, WorkspaceState, WorkspaceStatus,
    ZenHeartbeatCue, ZenModeState,
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
            if spawned
                .get(&workspace.name)
                .is_some_and(|h| h.is_finished())
            {
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
                .spawn(move || {
                    run_workspace_poll_loop(project_name, workspace_name, shutdown_clone)
                })
                .ok();
            if let Some(h) = handle {
                spawned.insert(workspace.name.clone(), h);
            }
        }

        // Heartbeats and pane supervision both mutate hub state. Re-evaluate
        // compatibility every tick so a stale-daemon period is read-only and
        // the poller resumes automatically on the first tick after restart.
        match shelbi_state::ensure_daemon_matches_for_mutation() {
            Ok(()) => {
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
            }
            Err(e) => tracing::debug!(
                project = %project.name,
                error = %e,
                "poller supervisor mutations paused for daemon mismatch",
            ),
        }

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
    // worker→hub message (Shelbi ContextStore
    // docs/planning:reviews/adversarial-2026-07/process-boundaries.md F7).
    // `None` means "due now";
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

    // Usage-limit auto-resume schedule for this workspace's pane. Same
    // per-thread lifetime as the rest: a poller restart drops it, and the
    // next poll re-detects the (still-on-screen) banner and re-schedules —
    // rebuilding from the persisted pause transition recovers even when the
    // reset passed hours while the hub was down.
    let mut limit_resume = LimitResumeState::default();

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
                if let Err(e) = shelbi_ssh::ensure_reverse_forward(&host, machine.forward) {
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
            &mut limit_resume,
        );

        sleep_interruptible(
            limit_resume_sleep_interval(&limit_resume, interval, Utc::now()),
            &shutdown,
        );
    }
}

/// How often (in emitted Zen-on heartbeats) the one-line `zenmode.md`
/// summary is re-injected. The cheap, frequent reminder of what Zen means:
/// every Nth Zen heartbeat carries the summary; the ticks in between carry a
/// bare `zen=on`. Tunable here rather than hardcoded inline at the callsite.
const ZEN_SUMMARY_EVERY_N_HEARTBEATS: u32 = 3;

/// How often the fuller "re-read `zenmode.md` now" instruction is injected
/// while Zen is on — the deeper periodic refresh of Zen policy, layered over
/// the frequent summary reminder. Tunable here rather than hardcoded inline.
const ZEN_REREAD_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// Adaptive heartbeat cadence state, threaded across supervisor ticks.
///
/// The cadence is `standard` (from config) whenever there's supervisable work
/// in flight, and backs off exponentially — doubling each idle tick, capped at
/// the configured `max` — while the board is quiescent. Any real event resets
/// it to `standard`. This struct carries the things that survive between
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
    /// Count of Zen-on heartbeats emitted so far (reset to 0 whenever Zen is
    /// observed off). Drives the every-Nth-heartbeat summary cadence.
    zen_heartbeats: u32,
    /// When the last full re-read instruction was injected. `None` until the
    /// first Zen-on heartbeat seeds it (so the first re-read waits a full
    /// [`ZEN_REREAD_INTERVAL`] rather than firing immediately); reset to
    /// `None` whenever Zen is observed off so re-enabling Zen doesn't fire a
    /// re-read off a stale, hour-old timestamp.
    zen_last_reread: Option<Instant>,
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
    // Zen-pairing: only while Zen Mode is On does the heartbeat carry a
    // `zen=on` marker and (on two cadences) a re-injected reminder of what
    // Zen means. Off / paused / an unreadable state.json all leave the line
    // in its plain shape. Computed here — the cadence counters live in the
    // schedule — so the summary/reread decision is one place, not inline.
    let zen_cue = zen_heartbeat_cue(project, schedule, now);
    if let Err(e) = append_heartbeat_event(&project.name, zen_eligible, idle_workspaces, zen_cue) {
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

/// Decide the Zen reminder to attach to this heartbeat and advance the Zen
/// cadence counters in `schedule`. Called only from the emit path of
/// [`maybe_emit_heartbeat`], so every call corresponds to a heartbeat that
/// is actually being written.
///
/// Returns `None` — a plain heartbeat with no `zen=on` marker at all — when
/// Zen Mode is not `On` (off, paused, or an unreadable `state.json`), and
/// resets the Zen cadence so a later off→on re-enable starts clean. When Zen
/// is `On`, advances `zen_heartbeats` and returns, in priority order:
///
/// 1. [`ZenHeartbeatCue::Reread`] once at least [`ZEN_REREAD_INTERVAL`] has
///    elapsed since the last full re-read injection (the deeper hourly
///    refresh, which subsumes the summary);
/// 2. [`ZenHeartbeatCue::Summary`] on every
///    [`ZEN_SUMMARY_EVERY_N_HEARTBEATS`]th Zen heartbeat — the one-line
///    `zenmode.md` summary read *fresh* so a user's edit to the first line
///    shows up without a reload, degrading to `Plain` if it can't be read;
/// 3. [`ZenHeartbeatCue::Plain`] (bare `zen=on`) on the ticks in between.
fn zen_heartbeat_cue(
    project: &Project,
    schedule: &mut HeartbeatSchedule,
    now: Instant,
) -> Option<ZenHeartbeatCue> {
    let zen_on = read_state(&project.name)
        .map(|s| s.zen_mode == ZenModeState::On)
        .unwrap_or(false);
    if !zen_on {
        schedule.zen_heartbeats = 0;
        schedule.zen_last_reread = None;
        return None;
    }

    schedule.zen_heartbeats = schedule.zen_heartbeats.saturating_add(1);

    // Seed the re-read timer on the first Zen heartbeat so the first re-read
    // waits a full interval rather than firing immediately.
    let reread_due = match schedule.zen_last_reread {
        None => {
            schedule.zen_last_reread = Some(now);
            false
        }
        Some(t) => now.saturating_duration_since(t) >= ZEN_REREAD_INTERVAL,
    };
    if reread_due {
        schedule.zen_last_reread = Some(now);
        return Some(ZenHeartbeatCue::Reread);
    }

    if schedule.zen_heartbeats % ZEN_SUMMARY_EVERY_N_HEARTBEATS == 0 {
        return match read_zenmode_summary(&project.name) {
            Ok(Some(summary)) => Some(ZenHeartbeatCue::Summary(summary)),
            _ => Some(ZenHeartbeatCue::Plain),
        };
    }

    Some(ZenHeartbeatCue::Plain)
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
        // Quiescent = nothing ready/active/in-handoff. Keyed off the
        // semantic category so a workflow that renames these statuses still
        // counts; terminal (done/archived) and backlog tasks don't.
        matches!(
            tf.task.column.category(),
            StatusCategory::Ready | StatusCategory::Active | StatusCategory::Handoff
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
    limit_resume: &mut LimitResumeState,
) {
    // Gate the whole tick before reading/consuming ready or transition
    // markers. Under a mismatch those durable markers stay in place, and the
    // next compatible tick applies each one once. This also covers workspace
    // status writes, event appends, and pane supervision later in the tick.
    if let Err(e) = shelbi_state::ensure_daemon_matches_for_mutation() {
        tracing::debug!(
            project = %project.name,
            workspace = %workspace.name,
            error = %e,
            "poller mutations paused for daemon mismatch",
        );
        return;
    }
    let Some(machine) = project.machine(&workspace.machine) else {
        return;
    };
    let host = machine.host();
    let Ok(addr) = shelbi_orchestrator::workspace::workspace_tmux_addr(project, workspace) else {
        return;
    };

    // Ready handoff is a file marker the workspace writes when it's done, read
    // independently of the pane title. We check it *before* the pane-title
    // state below (and unconditionally, even if the pane has since died or
    // Claude has overwritten its title) so nothing the agent's UI does can
    // hide the signal. `addr` is threaded in so a dev workspace whose task we
    // just handed off can close its own session immediately (spec §16).
    maybe_apply_ready_handoff(project, workspace, machine, &host, &addr);

    // Agent-initiated status transition (bounce / send-back). A reviewer or
    // gate agent writes a transition marker naming its own task and a target
    // status; the poller validates the requested edge against the workflow and,
    // if allowed, applies it. Checked right after the forward ready handoff and
    // independently of the pane title, for the same reason: nothing the agent's
    // UI does can hide a file-based signal.
    maybe_apply_transition(project, workspace, machine, &host, &addr);

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
        // stuck-state so a respawned pane re-detects from scratch. Same for
        // a pending limit-resume: there's no pane left to nudge.
        *last_dialog = None;
        *limit_resume = LimitResumeState::Idle;
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
    // state reverts after a confirmed send or fresh live busy/input evidence;
    // Claude's title alone is not enough because it stays stale across the
    // modal and is commonly clobbered again during resume.
    let runner_is_claude = project
        .runner(&workspace.runner)
        .is_some_and(|runner| shelbi_agent::RunnerAdapter::for_spec(runner).is_claude());
    if !runner_is_claude && *limit_resume != LimitResumeState::Idle {
        if limit_resume.tracked_task().is_some() {
            if let Err(e) = append_limit_resume_event(
                &project.name,
                &workspace.name,
                "skipped",
                &[("reason", "runner-not-claude")],
            ) {
                tracing::warn!(workspace = %workspace.name, error = %e, "append_limit_resume_event failed");
            }
        }
        *limit_resume = LimitResumeState::Idle;
    }
    let limit_stall = runner_is_claude
        .then(|| {
            screen
                .as_deref()
                .and_then(shelbi_orchestrator::ready::detect_usage_limit)
        })
        .flatten();
    if let Some(stall) = limit_stall {
        let banner = limit_banner_key(&stall);
        let active_task = current_task_for(project, &workspace.name);

        // The modal is runner-specific and actionable only while this slot
        // still owns active work. A pane left behind after cancel, handoff,
        // bounce, or unassign must never be nudged. Latch this visible banner
        // as suppressed so assigning a different task before the pane clears
        // cannot recycle the old task's schedule.
        let Some(task_id) = active_task else {
            suppress_limit_banner(project, workspace, limit_resume, banner, "no-active-task");
            *last_dialog = None;
            return;
        };

        // A confirmed send (or a lifecycle change) can leave the old modal's
        // pixels visible while Claude is already working and has clobbered the
        // pane title. The same-banner latch prevents those pixels from
        // re-recording Paused or arming another send.
        if limit_resume.suppresses_banner(&banner) {
            *last_dialog = None;
            return;
        }

        // Bind every schedule to the exact task that owned the pane when the
        // stall was observed. If task A was replaced by task B, suppress A's
        // still-visible banner rather than scheduling it anew for B.
        if limit_resume
            .tracked_task()
            .is_some_and(|scheduled| scheduled != task_id)
            || paused_status_belongs_to_other_task(&workspace.name, &task_id)
        {
            suppress_limit_banner(
                project,
                workspace,
                limit_resume,
                banner,
                "active-task-changed",
            );
            *last_dialog = None;
            return;
        }

        let stalled_at =
            record_usage_limit_pause(project, workspace, last_known, stall.reset.as_deref());
        // The action half of the detection: schedule a resume off this
        // structurally paired banner's reset time and, once it comes due,
        // nudge the pane back to work with a verified-submitted prompt.
        handle_limit_stall(
            project,
            workspace,
            &host,
            LimitResumeIncident {
                task_id,
                banner: stall.banner,
                reset_hint: stall.reset,
            },
            stalled_at,
            limit_resume,
            last_known,
        );
        // Pause supersedes any tracked advisory dialog.
        *last_dialog = None;
        return;
    }
    // A good sample with no current limit banner invalidates a not-yet-fired
    // schedule, but it does not by itself prove a paused worker recovered:
    // Claude's `shelbi:working` title is known to remain stale while the modal
    // is open. Clear Paused only from a live busy footer or a clean ready input
    // (one that is not holding our unsubmitted resume prompt). A post-delivery
    // needs-human latch survives a banner-free screen until that proof arrives.
    if runner_is_claude {
        if let Some(screen) = screen.as_deref() {
            let recovered =
                record_limit_recovery_from_screen(project, workspace, screen, last_known);
            if recovered {
                advance_limit_resume_without_banner(limit_resume, true);
            } else if let Some(reason) = advance_limit_resume_without_banner(limit_resume, false) {
                if let Err(e) = append_limit_resume_event(
                    &project.name,
                    &workspace.name,
                    "needs-human",
                    &[("reason", reason)],
                ) {
                    tracing::warn!(workspace = %workspace.name, error = %e, "append_limit_resume_event failed");
                }
            }

            // Do not let the known-stale pane title clear a persisted pause
            // when the screen offered no current recovery proof. A later
            // busy/ready sample, pane death, or lifecycle change advances it.
            if workspace_is_paused(&workspace.name, last_known) && !recovered {
                *last_dialog = None;
                return;
            }
        }
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
    // review marker, consumed by `maybe_apply_ready_handoff` above — a file
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
/// only `last_seen` moves and no further event fires. The resume edge is
/// recorded either immediately after a verified auto-resume or from fresh
/// busy/clean-input evidence after a manual resume; the stale pane title is
/// never the sole recovery signal.
fn record_usage_limit_pause(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    last_known: &mut Option<WorkspaceState>,
    reset: Option<&str>,
) -> DateTime<Utc> {
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
        if let Err(e) =
            append_workspace_pause_event(&project.name, &workspace.name, outcome.prev_state, reset)
        {
            tracing::warn!(workspace = %workspace.name, error = %e, "append_workspace_pause_event failed");
        }
        tracing::info!(
            workspace = %workspace.name,
            reset = ?reset,
            "workspace paused on usage limit",
        );
    }

    let stalled_at = outcome.status.last_transition;
    *last_known = Some(WorkspaceState::Paused);
    stalled_at
}

/// Persist a strongly observed post-limit state immediately instead of
/// waiting for a pane title that Claude may overwrite before the next
/// heartbeat. This clears the sidebar's pause badge even when old modal pixels
/// remain visible.
fn record_limit_resume_state(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    current_task: Option<String>,
    new_state: WorkspaceState,
    last_known: &mut Option<WorkspaceState>,
) {
    let prior = load_prior(&workspace.name, last_known);
    let outcome = decide(&workspace.name, current_task, prior, new_state, Utc::now());
    if let Err(e) = save_workspace_status(&outcome.status) {
        tracing::warn!(workspace = %workspace.name, error = %e, "save_workspace_status after limit resume failed");
    }
    if outcome.transitioned {
        if let Err(e) = append_workspace_event(
            &project.name,
            &workspace.name,
            outcome.prev_state,
            new_state,
        ) {
            tracing::warn!(workspace = %workspace.name, error = %e, "append_workspace_event after limit resume failed");
        }
    }
    *last_known = Some(new_state);
}

fn workspace_is_paused(workspace_name: &str, last_known: &Option<WorkspaceState>) -> bool {
    matches!(last_known, Some(WorkspaceState::Paused))
        || matches!(
            load_workspace_status(workspace_name),
            Ok(Some(status)) if status.state == WorkspaceState::Paused
        )
}

/// Clear a persisted usage-limit pause only from current UI evidence. Busy is
/// proof Claude is processing; a ready input means the modal is gone and the
/// worker is awaiting input, provided our resume prompt is not still parked in
/// that box. Returns whether a paused status was advanced.
fn record_limit_recovery_from_screen(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    screen: &str,
    last_known: &mut Option<WorkspaceState>,
) -> bool {
    if !workspace_is_paused(&workspace.name, last_known) {
        return false;
    }
    let observed = if shelbi_orchestrator::ready::is_claude_busy(screen) {
        Some(WorkspaceState::Working)
    } else if shelbi_orchestrator::ready::is_input_ready(screen)
        && !shelbi_orchestrator::submit::input_holds_unsubmitted_prompt(screen, LIMIT_RESUME_PROMPT)
    {
        Some(WorkspaceState::AwaitingInput)
    } else {
        None
    };
    let Some(observed) = observed else {
        return false;
    };
    record_limit_resume_state(
        project,
        workspace,
        current_task_for(project, &workspace.name),
        observed,
        last_known,
    );
    true
}

/// Grace margin past the banner's stated reset time before the resume nudge
/// fires — covers clock skew between the hub and Anthropic's window clock,
/// and the minute-granularity of the stated time itself.
const LIMIT_RESUME_GRACE_SECS: i64 = 90;

/// While a reset is scheduled, never let a user-configured slow workspace
/// poll cadence push the attempt far past the reset. Ninety seconds of grace
/// plus at most this 30-second heartbeat delay keeps delivery within roughly
/// two minutes of the stated time.
const LIMIT_RESUME_MAX_POLL_SLEEP: Duration = Duration::from_secs(30);

/// Backoff between resume attempts against the same banner. A failed nudge
/// (window not actually reset yet, submit unconfirmed) retries on this
/// cadence rather than hammering the pane every poll tick.
const LIMIT_RESUME_RETRY_SECS: i64 = 5 * 60;

/// How many resume attempts one banner gets before the poller stops and
/// surfaces a needs-human line instead.
const LIMIT_RESUME_MAX_ATTEMPTS: u32 = 3;

/// The prompt the auto-resume submits to the stalled pane.
const LIMIT_RESUME_PROMPT: &str = "The session limit has reset. Please continue with the task.";

/// One limit incident is bound to both the structurally selected banner and
/// the exact active task that owned the workspace when it was observed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LimitResumeIncident {
    task_id: String,
    /// The raw structurally paired banner line, retained so the delivery path
    /// can prove the due schedule still refers to the current modal.
    banner: String,
    reset_hint: Option<String>,
}

impl LimitResumeIncident {
    fn stall(&self) -> shelbi_orchestrator::ready::UsageLimitStall {
        shelbi_orchestrator::ready::UsageLimitStall {
            banner: self.banner.clone(),
            reset: self.reset_hint.clone(),
        }
    }

    fn banner_key(&self) -> String {
        limit_banner_key(&self.stall())
    }
}

/// Per-workspace auto-resume bookkeeping for a usage-limit stall, advanced
/// once per poll tick that observes the banner ([`advance_limit_resume`]) and
/// reset to `Idle` when the banner is gone or the pane dies.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum LimitResumeState {
    /// No stall being tracked.
    #[default]
    Idle,
    /// Reset time parsed; nudge the pane at `due`. `attempts` counts nudges
    /// already fired against this banner (each pushes `due` out by the retry
    /// backoff until [`LIMIT_RESUME_MAX_ATTEMPTS`] trips needs-human).
    Scheduled {
        incident: LimitResumeIncident,
        due: DateTime<Utc>,
        attempts: u32,
    },
    /// This banner can't drive a resume (unparseable reset time, or every
    /// attempt failed). Warned once; inert until the banner changes.
    NeedsHuman { incident: LimitResumeIncident },
    /// This banner was already resumed or invalidated by a lifecycle change.
    /// Keep it latched until it disappears so stale visible modal text cannot
    /// re-pause the badge or nudge a newly assigned task.
    Resumed { banner: String },
}

impl LimitResumeState {
    fn tracked_task(&self) -> Option<&str> {
        match self {
            LimitResumeState::Scheduled { incident, .. }
            | LimitResumeState::NeedsHuman { incident } => Some(&incident.task_id),
            LimitResumeState::Idle | LimitResumeState::Resumed { .. } => None,
        }
    }

    fn suppresses_banner(&self, banner: &str) -> bool {
        matches!(self, LimitResumeState::Resumed { banner: seen } if seen == banner)
    }
}

fn limit_resume_sleep_interval(
    state: &LimitResumeState,
    configured: Duration,
    now: DateTime<Utc>,
) -> Duration {
    let LimitResumeState::Scheduled { due, .. } = state else {
        return configured;
    };
    let until_due = due
        .signed_duration_since(now)
        .to_std()
        .unwrap_or(Duration::from_secs(1))
        .max(Duration::from_secs(1));
    configured.min(LIMIT_RESUME_MAX_POLL_SLEEP).min(until_due)
}

/// Advance a tracked incident after a successful pane capture no longer shows
/// its banner. Returns a needs-human reason exactly on the edge where a
/// retryable modal attempt became ambiguous. Terminal delivery uncertainty is
/// already represented by `NeedsHuman` and stays latched until live recovery
/// proof arrives; an unfired schedule is simply invalidated to prevent a late
/// duplicate nudge.
fn advance_limit_resume_without_banner(
    state: &mut LimitResumeState,
    recovered: bool,
) -> Option<&'static str> {
    if recovered {
        *state = LimitResumeState::Idle;
        return None;
    }
    match state {
        LimitResumeState::Scheduled {
            attempts, incident, ..
        } if *attempts > 0 => {
            let incident = incident.clone();
            *state = LimitResumeState::NeedsHuman { incident };
            Some("banner-gone-after-retryable-attempt")
        }
        LimitResumeState::NeedsHuman { .. } => None,
        LimitResumeState::Idle => None,
        LimitResumeState::Scheduled { .. } | LimitResumeState::Resumed { .. } => {
            *state = LimitResumeState::Idle;
            None
        }
    }
}

/// What one stall tick asks the caller to do. Split from the I/O so the
/// schedule/retry/give-up rules are unit-testable on fixtures.
#[derive(Debug, Clone, PartialEq, Eq)]
enum LimitResumeAction {
    /// Nothing this tick (waiting for the due time, or already inert).
    None,
    /// A resume was just scheduled — emit the `status=scheduled` line.
    Scheduled { due: DateTime<Utc> },
    /// The schedule is due — attempt the resume nudge now.
    Attempt { incident: LimitResumeIncident },
    /// This banner needs a human — emit the `status=needs-human` line once.
    NeedsHuman { reason: &'static str },
}

/// Advance the limit-resume state machine for one poll tick that observed
/// the usage-limit banner (with `hint` as its scraped reset wording, if any)
/// and return what to do. Pure — `now` is threaded in and all I/O (the
/// nudge itself, events.log lines) stays in [`handle_limit_stall`].
fn advance_limit_resume(
    state: &mut LimitResumeState,
    incident: &LimitResumeIncident,
    stalled_at: DateTime<Utc>,
    now: DateTime<Utc>,
    allow_local_implied: bool,
) -> LimitResumeAction {
    let fresh_banner = match state {
        LimitResumeState::Idle => true,
        LimitResumeState::Scheduled {
            incident: current, ..
        }
        | LimitResumeState::NeedsHuman { incident: current } => current != incident,
        LimitResumeState::Resumed { banner } => banner != &incident.banner_key(),
    };
    if fresh_banner {
        // The banner time is minute-granular. Referencing one grace window
        // before the persisted first observation keeps a 07:20:30 sample of a
        // `7:20am` reset on that occurrence instead of rolling to tomorrow.
        let reference = stalled_at - chrono::Duration::seconds(LIMIT_RESUME_GRACE_SECS);
        return match incident.reset_hint.as_deref().and_then(|hint| {
            shelbi_orchestrator::ready::next_reset_instant(hint, reference, allow_local_implied)
        }) {
            Some(reset) => {
                let due = reset + chrono::Duration::seconds(LIMIT_RESUME_GRACE_SECS);
                *state = LimitResumeState::Scheduled {
                    incident: incident.clone(),
                    due,
                    attempts: 0,
                };
                LimitResumeAction::Scheduled { due }
            }
            None => {
                *state = LimitResumeState::NeedsHuman {
                    incident: incident.clone(),
                };
                LimitResumeAction::NeedsHuman {
                    reason: if !allow_local_implied
                        && incident
                            .reset_hint
                            .as_deref()
                            .is_some_and(|hint| !hint.contains('('))
                    {
                        "missing-timezone-remote"
                    } else {
                        "unparseable-reset"
                    },
                }
            }
        };
    }
    if let LimitResumeState::Scheduled {
        incident,
        due,
        attempts,
    } = state
    {
        if now >= *due {
            if *attempts >= LIMIT_RESUME_MAX_ATTEMPTS {
                let incident = incident.clone();
                *state = LimitResumeState::NeedsHuman { incident };
                return LimitResumeAction::NeedsHuman {
                    reason: "resume-attempts-exhausted",
                };
            }
            *attempts += 1;
            *due = now + chrono::Duration::seconds(LIMIT_RESUME_RETRY_SECS);
            return LimitResumeAction::Attempt {
                incident: incident.clone(),
            };
        }
    }
    LimitResumeAction::None
}

/// The I/O half of the usage-limit auto-resume, run on every poll tick that
/// detects the stall banner (right after [`record_usage_limit_pause`]):
/// advance the schedule and act on its verdict — emit the scheduled /
/// needs-human events.log lines, or fire the resume nudge
/// ([`shelbi_orchestrator::workspace::resume_limit_stalled_pane`]) and
/// record how it went. Every emitted line carries
/// `supervision=limit-resume` so the orchestrator and the activity feed can
/// follow the cycle.
fn handle_limit_stall(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    host: &shelbi_core::Host,
    incident: LimitResumeIncident,
    stalled_at: DateTime<Utc>,
    state: &mut LimitResumeState,
    last_known: &mut Option<WorkspaceState>,
) {
    let append = |status: &str, details: &[(&str, &str)]| {
        if let Err(e) = append_limit_resume_event(&project.name, &workspace.name, status, details) {
            tracing::warn!(workspace = %workspace.name, error = %e, "append_limit_resume_event failed");
        }
    };
    match advance_limit_resume(state, &incident, stalled_at, Utc::now(), host.is_local()) {
        LimitResumeAction::None => {}
        LimitResumeAction::Scheduled { due } => {
            append("scheduled", &[("scheduled_for", &due.to_rfc3339())]);
            tracing::info!(
                workspace = %workspace.name,
                due = %due.to_rfc3339(),
                reset = ?incident.reset_hint,
                "usage-limit stall: auto-resume scheduled",
            );
        }
        LimitResumeAction::NeedsHuman { reason } => {
            append(
                "needs-human",
                &[
                    ("reason", reason),
                    ("reset", incident.reset_hint.as_deref().unwrap_or("none")),
                ],
            );
            tracing::warn!(
                workspace = %workspace.name,
                reason,
                reset = ?incident.reset_hint,
                "usage-limit stall can't be auto-resumed — waiting for a human",
            );
        }
        LimitResumeAction::Attempt { incident } => {
            use shelbi_orchestrator::workspace::LimitResumeOutcome;
            let project_name = project.name.clone();
            let workspace_name = workspace.name.clone();
            let task_id = incident.task_id.clone();
            let Ok(addr) = shelbi_orchestrator::workspace::workspace_tmux_addr(project, workspace)
            else {
                append("needs-human", &[("reason", "invalid-pane-address")]);
                *state = LimitResumeState::NeedsHuman {
                    incident: incident.clone(),
                };
                return;
            };

            // Re-resolve immediately before entering the delivery helper. The
            // helper invokes this same live check once more after modal
            // dismissal, directly before it types the prompt.
            if !limit_resume_eligible_now(&project_name, &workspace_name, &task_id) {
                append("skipped", &[("reason", "no-longer-eligible")]);
                *state = LimitResumeState::Resumed {
                    banner: incident.banner_key(),
                };
                return;
            }
            match shelbi_orchestrator::workspace::resume_limit_stalled_pane(
                host,
                &addr,
                &incident.stall(),
                LIMIT_RESUME_PROMPT,
                || limit_resume_eligible_now(&project_name, &workspace_name, &task_id),
            ) {
                Ok(LimitResumeOutcome::Submitted) => {
                    append("sent", &[]);
                    // Submission is an irreversible fact and is logged even
                    // if ownership changed while confirmation was arriving.
                    // Clear Paused on that confirmed fact and resolve the
                    // current owner afresh, so a lifecycle race never leaves
                    // the badge stuck or stamps the stale incident task id.
                    record_limit_resume_state(
                        project,
                        workspace,
                        current_task_for(project, &workspace.name),
                        WorkspaceState::Working,
                        last_known,
                    );
                    *state = LimitResumeState::Resumed {
                        banner: incident.banner_key(),
                    };
                    tracing::info!(
                        workspace = %workspace.name,
                        "usage-limit auto-resume prompt submitted",
                    );
                }
                Ok(LimitResumeOutcome::SkippedBannerGone) => {
                    // Someone resumed the pane between our capture and the
                    // nudge, so nothing was typed. Latch this incident until a
                    // fresh screen sample proves its recovery or a different
                    // structural banner starts a new cycle.
                    append("skipped", &[("reason", "banner-gone")]);
                    *state = LimitResumeState::Resumed {
                        banner: incident.banner_key(),
                    };
                }
                Ok(LimitResumeOutcome::SkippedIncidentChanged) => {
                    append("skipped", &[("reason", "incident-changed")]);
                    // Suppress only the old scheduled incident. The changed
                    // current banner has a different key and schedules on the
                    // next heartbeat with its own reset time.
                    *state = LimitResumeState::Resumed {
                        banner: incident.banner_key(),
                    };
                }
                Ok(LimitResumeOutcome::SkippedIneligible) => {
                    append("skipped", &[("reason", "no-longer-eligible")]);
                    *state = LimitResumeState::Resumed {
                        banner: incident.banner_key(),
                    };
                }
                Ok(LimitResumeOutcome::InputNotReady) => {
                    append("failed", &[("reason", "input-not-ready")]);
                    tracing::warn!(
                        workspace = %workspace.name,
                        "usage-limit auto-resume: exact modal still active after dismissal; will retry",
                    );
                }
                Ok(LimitResumeOutcome::DeliveryUncertain) => {
                    append(
                        "needs-human",
                        &[("reason", "state-uncertain-after-dismiss")],
                    );
                    *state = LimitResumeState::NeedsHuman {
                        incident: incident.clone(),
                    };
                    tracing::warn!(
                        workspace = %workspace.name,
                        "usage-limit auto-resume: modal disappeared without a ready input; waiting for a human",
                    );
                }
                Ok(LimitResumeOutcome::PromptParkedIneligible) => {
                    append("needs-human", &[("reason", "owner-changed-prompt-parked")]);
                    *state = LimitResumeState::NeedsHuman {
                        incident: incident.clone(),
                    };
                    tracing::warn!(
                        workspace = %workspace.name,
                        "usage-limit auto-resume: task ownership changed with the prompt parked; retry Enter withheld",
                    );
                }
                Ok(LimitResumeOutcome::SubmitUnconfirmed) => {
                    append("needs-human", &[("reason", "submit-unconfirmed")]);
                    *state = LimitResumeState::NeedsHuman {
                        incident: incident.clone(),
                    };
                    tracing::warn!(
                        workspace = %workspace.name,
                        "usage-limit auto-resume: prompt delivery unconfirmed; not retrying a possibly parked prompt",
                    );
                }
                Err(e) => {
                    append("needs-human", &[("reason", "io-error-during-delivery")]);
                    *state = LimitResumeState::NeedsHuman {
                        incident: incident.clone(),
                    };
                    tracing::warn!(
                        workspace = %workspace.name,
                        error = %e,
                        "usage-limit auto-resume: pane io failed at an unknown delivery phase; waiting for a human",
                    );
                }
            }
        }
    }
}

fn limit_banner_key(stall: &shelbi_orchestrator::ready::UsageLimitStall) -> String {
    format!(
        "{}\nreset={}",
        stall.banner,
        stall.reset.as_deref().unwrap_or("none")
    )
}

/// A persisted Paused state survives poller restarts and carries the task that
/// originally hit the limit. If that task no longer owns the slot, its visible
/// banner must not seed a schedule for a replacement task.
fn paused_status_belongs_to_other_task(workspace: &str, task_id: &str) -> bool {
    matches!(
        load_workspace_status(workspace),
        Ok(Some(status))
            if status.state == WorkspaceState::Paused
                && status.current_task.as_deref() != Some(task_id)
    )
}

fn suppress_limit_banner(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    state: &mut LimitResumeState,
    banner: String,
    reason: &'static str,
) {
    if state.tracked_task().is_some() {
        if let Err(e) = append_limit_resume_event(
            &project.name,
            &workspace.name,
            "skipped",
            &[("reason", reason)],
        ) {
            tracing::warn!(workspace = %workspace.name, error = %e, "append_limit_resume_event failed");
        }
    }
    *state = LimitResumeState::Resumed { banner };
}

/// Fail-closed live authorization for the final pane mutation and send. Reload
/// project config as well as board state so both the runner and the exact task
/// assignment are current after a potentially long readiness wait.
fn limit_resume_eligible_now(project_name: &str, workspace_name: &str, task_id: &str) -> bool {
    let Ok(project) = shelbi_state::load_project(project_name) else {
        return false;
    };
    let Some(workspace) = project.workspace(workspace_name) else {
        return false;
    };
    let runner_is_claude = project
        .runner(&workspace.runner)
        .is_some_and(|runner| shelbi_agent::RunnerAdapter::for_spec(runner).is_claude());
    runner_is_claude && current_task_for(&project, workspace_name).as_deref() == Some(task_id)
}

/// Check the workspace's ready-handoff file marker and, if present, advance
/// its in-progress task to the workflow's handoff status. The marker is the
/// workspace's "I'm done" signal — it writes its task id into
/// `<worktree>/.claude/shelbi-ready` when done (see
/// `shelbi_orchestrator::workspace::workspace_ready_marker`).
///
/// The forward target is resolved generically from the task's workflow: the
/// first status in the workflow's [`StatusCategory::Handoff`] category, not a
/// hardcoded "review" id — so a workflow that renames its handoff status (e.g.
/// `qa`) advances there just the same. A workflow with *no* handoff status
/// (e.g. `app-feature-subtask`: todo → in-progress → done) instead advances
/// along the active status's outgoing merge-firing transition
/// ([`Workflow::outgoing_merge_transitions`]), landing the task straight in
/// that edge's target — never in a review workspace, which only receives
/// tasks in a handoff status. Only a workflow with neither is treated as
/// misconfigured (warn + clear). The edge's transition actions +
/// `run:` / `ready:` commands fire via [`execute_transition`], so serving is
/// driven entirely by the generic transition-command path.
///
/// Best-effort and idempotent: we consume the marker exactly once by clearing
/// it after a successful move. A stale marker (worktree reused before the
/// previous one was cleared) names a task that's no longer in-progress for
/// this workspace, so we clear it without moving anything.
///
/// On a real handoff this also **closes the dev workspace's session**
/// immediately (spec §16): the branch is safely handed off, so the finished
/// pane has no reason to linger and surface a completion glyph. Ordering is
/// load-bearing — the close happens only AFTER the rebase + column move, never
/// before, so work can't be stranded. A `review`-tagged workspace is left
/// running (it reaches the handoff status by being *loaded* onto, not by
/// finishing an in-progress task), so the guard is defensive.
fn maybe_apply_ready_handoff(
    project: &Project,
    workspace: &shelbi_core::WorkspaceSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
    addr: &shelbi_core::TmuxAddr,
) {
    let marker = shelbi_orchestrator::workspace::workspace_ready_marker(machine, workspace);
    let task_id = match shelbi_orchestrator::workspace::read_ready_marker(host, &marker) {
        Ok(Some(id)) => id,
        Ok(None) => return,
        Err(e) => {
            tracing::warn!(workspace = %workspace.name, error = %e, "read_ready_marker failed");
            return;
        }
    };

    // Load the task up front so we can confirm we had a valid task at all.
    // If the load fails or the task isn't ours in-progress, we still fall
    // through to clear the (stale) marker.
    let task_file = shelbi_state::load_task(&project.name, &task_id);

    match &task_file {
        Ok(tf)
            if tf.task.column == Column::in_progress()
                && tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()) =>
        {
            // Resolve the forward target from the task's workflow: the first
            // handoff-category status. A workflow may legitimately declare no
            // handoff status (e.g. a subtask flow that merges straight to
            // done) — then fall back to the active status's outgoing
            // merge-firing edge and auto-advance along it, so the finished
            // task isn't stranded in-progress. That edge lands the task
            // directly in its (done) target — it is NOT a handoff, so it can
            // never be routed to a review workspace, which only receives
            // tasks sitting in a handoff-category status. Neither a handoff
            // status nor a merge edge → nothing to advance to; clear the
            // (misconfigured) marker below.
            let workflow = shelbi_state::load_task_workflow(&project.name, project, &tf.task)
                .unwrap_or_else(|_| default_workflow());
            let from_status = resolve_current_status_id(&workflow, Column::in_progress());
            let to_status = if let Some(handoff) = workflow
                .statuses
                .iter()
                .find(|s| s.category == StatusCategory::Handoff)
            {
                handoff.id.clone()
            } else if let Some(edge) = workflow.outgoing_merge_transitions(&from_status).first() {
                tracing::info!(workspace = %workspace.name, task = %task_id, from = %from_status, to = %edge.to, "workflow declares no handoff status; auto-advancing along merge transition");
                edge.to.clone()
            } else {
                tracing::warn!(workspace = %workspace.name, task = %task_id, "workflow declares no handoff status and no merge transition out of the active status; clearing ready marker");
                let _ = shelbi_orchestrator::workspace::clear_ready_marker(host, &marker);
                return;
            };
            let to_column = Column::from_status_id(&to_status);

            // Auto-rebase the workspace's branch onto its base branch (the
            // branch the work merges into — see
            // `rebase_workspace_branch_before_handoff`) before the column
            // move, so the human reviewer sees a single clean diff instead of
            // running the rebase + force-push by hand. Done BEFORE the move
            // (rather than blocking on it) so the
            // row showing up already reflects the rewritten branch; a conflict
            // is logged but doesn't block the handoff.
            rebase_workspace_branch_before_handoff(project, workspace, machine, host, &task_id);

            match shelbi_state::move_task(&project.name, &task_id, to_column) {
                Ok(Some((from, to, workflow))) => {
                    if let Err(e) = shelbi_state::append_task_event(
                        &project.name,
                        &task_id,
                        &workflow,
                        from,
                        to,
                        shelbi_state::READY_MARKER_HANDOFF_CAUSE,
                    ) {
                        tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "append_task_event failed");
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    // Leave the marker in place so we retry on the next tick.
                    tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "move_task on ready handoff failed");
                    return;
                }
            }

            // Release the worker's worktree from the task branch now that the
            // handoff has landed (rebase + column move both succeeded). Detach
            // the worktree's HEAD in place so `<task.branch>` is no longer held
            // by any worktree — the review checkout and the later merge /
            // `delete_branch` would otherwise die on `already checked out at
            // <worktree>`. Done BEFORE `execute_transition` on purpose: a
            // handoff-less workflow's edge fires `merge` + `delete_branch` right
            // here, and `delete_branch` skips a branch still held by a worktree
            // (see `actions::workspace_holding_branch`), so freeing it first is
            // what lets the immediate delete succeed. Ordering stays
            // load-bearing — this runs only AFTER the move, and its failure
            // never rolls the handoff back (the promoted task is the source of
            // truth). This is also the missed-marker recovery path (this whole
            // function runs on later ticks even after the pane has died), so
            // both routes leave the branch free.
            detach_workspace_worktree_after_handoff(workspace, machine, host, &task_id);

            // Fire the edge's transition actions + `run:` / `ready:` commands
            // (serving). Best-effort — the move already happened, so a command
            // failure logs but doesn't roll it back. A workflow with no edge
            // declared for this move is a clean no-op.
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
                        tracing::info!(workspace = %workspace.name, task = %task_id, action = %o.action, line = %o.line, "ready-handoff action fired");
                    }
                }
                Err(e) => {
                    tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "ready-handoff transition command failed");
                }
            }

            tracing::info!(workspace = %workspace.name, task = %task_id, to = %to_status, "advanced task via ready marker");

            // Close the dev session on completion (spec §16). Best-effort and
            // idempotent. A `review`-tagged workspace is left running (it holds
            // a loaded task, not a just-finished in-progress one).
            if !project.effective_tags(workspace).contains("review") {
                if let Err(e) =
                    shelbi_orchestrator::workspace::kill_workspace_pane(host, addr, &workspace.name)
                {
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
            tracing::debug!(workspace = %workspace.name, task = %task_id, "stale ready marker (task not in-progress for this workspace); clearing");
        }
        Err(e) => {
            tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "ready marker names unloadable task; clearing");
        }
    }

    if let Err(e) = shelbi_orchestrator::workspace::clear_ready_marker(host, &marker) {
        tracing::warn!(workspace = %workspace.name, error = %e, "clear_ready_marker failed");
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

/// Resolve the workflow status id a task in `current_column` currently occupies.
/// A task's position is itself a status id, so this is an identity when the
/// workflow declares that id; it only falls back to a same-category status
/// (so a workflow that renamed `review` to `qa` still resolves) or, last, the
/// stored id string when the workflow declares nothing compatible.
fn resolve_current_status_id(workflow: &Workflow, current_column: Column) -> String {
    let stored = current_column.as_str();
    if workflow.status(stored).is_some() {
        return stored.to_string();
    }
    let cat = current_column.category();
    workflow
        .statuses
        .iter()
        .find(|s| s.category == cat)
        .map(|s| s.id.clone())
        .unwrap_or_else(|| stored.to_string())
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
/// 4. **Must actually move.** A target resolving to the status the task is
///    already in is refused as a no-op (guarding against unassigning a task
///    that wouldn't actually change lanes).
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
            None => {
                return TransitionDecision::Reject {
                    reason: "no-active-status",
                }
            }
        },
        other => other.to_string(),
    };

    if workflow.status(&to_status).is_none() {
        return TransitionDecision::Reject {
            reason: "unknown-target-status",
        };
    };

    let allowed = match &workflow.transitions {
        Some(ts) => ts
            .iter()
            .any(|t| t.from == from_status && t.to == to_status),
        None => true,
    };
    if !allowed {
        return TransitionDecision::Reject {
            reason: "edge-not-in-workflow",
        };
    }

    // The target's status id IS the destination position — no category round
    // trip, so a move between two distinct statuses in the same category is a
    // real move, not a false no-op.
    if to_status == from_status {
        return TransitionDecision::Reject {
            reason: "target-is-current-status",
        };
    }
    let to_column = Column::from_status_id(&to_status);

    TransitionDecision::Apply {
        from_status,
        to_status,
        to_column,
    }
}

/// Check the workspace's agent-transition marker and, if it names this
/// workspace's own task and requests an edge the workflow permits, apply the
/// move. The generalization of [`maybe_apply_ready_handoff`] to arbitrary
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
            let workflow = shelbi_state::load_task_workflow(&project.name, project, &tf.task)
                .unwrap_or_else(|_| default_workflow());

            match decide_transition(&workflow, tf.task.column.clone(), &req.target) {
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
                    match shelbi_state::move_task_and_unassign(
                        &project.name,
                        &req.task_id,
                        to_column,
                    ) {
                        Ok(Some((from, to, wf))) => {
                            if let Err(e) = shelbi_state::append_task_event(
                                &project.name,
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
/// the workflow's resolved `git.base_branch` (the branch the work merges
/// into — e.g. `feature/{{feature}}` for a subtask flow), falling back to the
/// project's default branch when the workflow declares none. Records one
/// `rebase` line in `events.log` describing the outcome (ok / up-to-date /
/// conflict / skipped). Never blocks the calling handoff — failures here are
/// advisory.
///
/// `branch` uses the same explicit/workflow/project/GitHub fallback resolver
/// as task dispatch when the task frontmatter doesn't pin one explicitly.
fn rebase_workspace_branch_before_handoff(
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
    let workflow = match shelbi_state::load_task_workflow(&project.name, project, &task_file.task) {
        Ok(wf) => wf,
        Err(e) => {
            tracing::debug!(workspace = %workspace.name, task = %task_id, error = %e, "skip rebase: load_task_workflow failed");
            return;
        }
    };
    let branch = match shelbi_orchestrator::branch::branch_name_for_task(
        project,
        Some(&workflow),
        &task_file.task,
    ) {
        Ok(branch) => branch,
        Err(e) => {
            tracing::debug!(workspace = %workspace.name, task = %task_id, error = %e, "skip rebase: branch resolution failed");
            return;
        }
    };

    // Rebase onto the branch this work will merge into: the workflow's
    // resolved `git.base_branch` when declared (a subtask branch cut from
    // `feature/x` must not be rewritten onto main), else the project
    // default. An unresolvable placeholder skips the (advisory) rebase
    // rather than rebasing onto the wrong base.
    let base_branch = match workflow.resolve_git(&task_file.task.string_params()) {
        Ok(git) => git
            .and_then(|g| g.base_branch)
            .unwrap_or_else(|| project.default_branch.clone()),
        Err(e) => {
            tracing::debug!(workspace = %workspace.name, task = %task_id, error = %e, "skip rebase: workflow git block unresolvable");
            return;
        }
    };

    let worktree = shelbi_orchestrator::workspace::workspace_worktree(machine, workspace);
    let outcome = shelbi_orchestrator::workspace::rebase_workspace_branch_onto_default(
        host,
        &worktree,
        &base_branch,
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
                base = %base_branch,
                detail = %detail,
                "auto-rebase onto base branch conflicted; worktree returned to pre-rebase state",
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

/// Detach the finishing worker's worktree from its task branch after a ready
/// handoff, on the workspace's own host (SSH for a remote machine, local for
/// the hub — resolved through the same `host`/`workspace_worktree` path the
/// rebase step uses). Frees the branch so the review checkout and the later
/// merge / `delete_branch` don't hit `already checked out at <worktree>`.
///
/// Best-effort and non-blocking by contract: the handoff (task already promoted
/// to its handoff status) is the source of truth, so every outcome — success,
/// missing worktree, or a real git failure — is only recorded to `events.log`,
/// never propagated. A failure emits an explicit `reason=worktree-detach-failed`
/// line so a downstream `already checked out` merge error is traceable to a
/// still-held branch rather than looking silent.
fn detach_workspace_worktree_after_handoff(
    workspace: &shelbi_core::WorkspaceSpec,
    machine: &shelbi_core::Machine,
    host: &shelbi_core::Host,
    task_id: &str,
) {
    let worktree = shelbi_orchestrator::workspace::workspace_worktree(machine, workspace);
    match shelbi_orchestrator::workspace::detach_workspace_worktree(host, &worktree) {
        shelbi_orchestrator::workspace::DetachOutcome::Detached { from_branch } => {
            let branch = from_branch.as_deref().unwrap_or("(already-detached)");
            if let Err(e) = append_worktree_detach_event(task_id, &workspace.name, branch, true, "")
            {
                tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "append_worktree_detach_event failed");
            }
            tracing::info!(
                workspace = %workspace.name,
                task = %task_id,
                branch = %branch,
                "detached worker worktree from task branch; branch free for review checkout / merge",
            );
        }
        shelbi_orchestrator::workspace::DetachOutcome::NoWorktree => {
            // No worktree to release (never created or already torn down) — the
            // branch isn't held, so there's nothing to trace. Debug-only.
            tracing::debug!(workspace = %workspace.name, task = %task_id, "no worktree to detach on handoff");
        }
        shelbi_orchestrator::workspace::DetachOutcome::Failed { reason } => {
            if let Err(e) =
                append_worktree_detach_event(task_id, &workspace.name, "?", false, &reason)
            {
                tracing::warn!(workspace = %workspace.name, task = %task_id, error = %e, "append_worktree_detach_event failed");
            }
            tracing::warn!(
                workspace = %workspace.name,
                task = %task_id,
                reason = %reason,
                "worktree detach on handoff failed; task branch may still be held by the worktree — a later merge / delete may report `already checked out`",
            );
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
    let workflow = shelbi_state::load_task_workflow(&project.name, project, &tf.task)
        .map_err(|e| e.to_string())?;
    let branch =
        shelbi_orchestrator::branch::branch_name_for_task(project, Some(&workflow), &tf.task)
            .map_err(|e| e.to_string())?;
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
    shelbi_state::list_tasks(&project.name)
        .ok()?
        .into_iter()
        .find(|tf| {
            tf.task.assigned_to.as_deref() == Some(workspace_name)
                && task_is_active(project, &tf.task)
        })
        .map(|tf| tf.task.id)
}

/// Resolve task activity from its workflow, not from the literal
/// `in-progress` storage id. Custom workflows may call their active status
/// `coding`, `research`, or anything else. A configured workflow that cannot
/// be loaded fails closed; only the legacy implicit `default` workflow uses
/// the built-in fallback.
fn task_is_active(project: &Project, task: &shelbi_core::Task) -> bool {
    let workflow = match shelbi_state::load_task_workflow(&project.name, project, task) {
        Ok(workflow) => workflow,
        Err(_)
            if shelbi_state::resolve_task_workflow_name(project, task)
                == shelbi_core::DEFAULT_WORKFLOW_NAME =>
        {
            default_workflow()
        }
        Err(_) => return false,
    };
    workflow_status_is_active(&workflow, task.column.clone())
}

fn workflow_status_is_active(workflow: &Workflow, column: Column) -> bool {
    let status = resolve_current_status_id(workflow, column);
    workflow
        .status(&status)
        .is_some_and(|status| status.category == StatusCategory::Active)
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
        let paused = decide(
            "alpha",
            Some("t".into()),
            prior,
            WorkspaceState::Paused,
            ts(100),
        );
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
        let out = decide(
            "alpha",
            Some("t".into()),
            still,
            WorkspaceState::Paused,
            ts(160),
        );
        assert!(!out.transitioned);
        assert_eq!(out.status.last_transition, ts(100));
        assert_eq!(out.status.last_seen, ts(160));

        // Resume: the limit lifts, the live marker reads working again, and
        // Paused -> Working transitions — this is the clear-on-resume edge the
        // title path emits (`paused -> working`) so the ⏸ badge reverts.
        let resumed = decide(
            "alpha",
            Some("t".into()),
            still,
            WorkspaceState::Working,
            ts(200),
        );
        assert!(resumed.transitioned);
        assert_eq!(resumed.prev_state, Some(WorkspaceState::Paused));
        assert_eq!(resumed.status.state, WorkspaceState::Working);
        assert_eq!(resumed.status.last_transition, ts(200));
    }

    #[test]
    fn workflow_status_activity_uses_custom_status_category() {
        let mut workflow = default_workflow();
        let active = workflow
            .statuses
            .iter_mut()
            .find(|status| status.category == StatusCategory::Active)
            .unwrap();
        active.id = "coding".into();

        assert!(workflow_status_is_active(
            &workflow,
            Column::from_status_id("coding")
        ));
        assert!(!workflow_status_is_active(
            &workflow,
            Column::from_status_id("review")
        ));
    }

    #[test]
    fn limit_resume_schedules_once_then_attempts_at_due_with_backoff() {
        // The observed incident: alpha stalled at 02:28 UTC on a banner whose
        // reset ("2:20am America/New_York") is 06:20 UTC later that morning.
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        let reset = Utc.with_ymd_and_hms(2026, 7, 11, 6, 20, 0).unwrap();
        let incident = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "session-limit-2:20".into(),
            reset_hint: Some("2:20am (America/New_York)".into()),
        };
        let mut state = LimitResumeState::default();

        // First stall tick: schedule at reset + grace, emit once.
        let due = reset + chrono::Duration::seconds(LIMIT_RESUME_GRACE_SECS);
        assert_eq!(
            advance_limit_resume(&mut state, &incident, now, now, true),
            LimitResumeAction::Scheduled { due },
        );
        // Subsequent ticks on the same banner before due: silent — no
        // re-schedule spam and, crucially, no re-parse (which would roll the
        // occurrence to tomorrow once the time passes).
        assert_eq!(
            advance_limit_resume(
                &mut state,
                &incident,
                now,
                now + chrono::Duration::minutes(5),
                true,
            ),
            LimitResumeAction::None,
        );
        // Due passes and the banner is still up: attempt the nudge.
        let tick = due + chrono::Duration::seconds(5);
        assert_eq!(
            advance_limit_resume(&mut state, &incident, now, tick, true),
            LimitResumeAction::Attempt {
                incident: incident.clone()
            },
        );
        // Right after a (failed) attempt: backed off, not hammering.
        assert_eq!(
            advance_limit_resume(
                &mut state,
                &incident,
                now,
                tick + chrono::Duration::seconds(10),
                true,
            ),
            LimitResumeAction::None,
        );
        // Retries fire on the backoff cadence until the cap…
        let mut tick = tick;
        for _ in 1..LIMIT_RESUME_MAX_ATTEMPTS {
            tick += chrono::Duration::seconds(LIMIT_RESUME_RETRY_SECS + 5);
            assert_eq!(
                advance_limit_resume(&mut state, &incident, now, tick, true),
                LimitResumeAction::Attempt {
                    incident: incident.clone()
                },
            );
        }
        // …then the banner is handed to a human, once, and goes quiet.
        tick += chrono::Duration::seconds(LIMIT_RESUME_RETRY_SECS + 5);
        assert_eq!(
            advance_limit_resume(&mut state, &incident, now, tick, true),
            LimitResumeAction::NeedsHuman {
                reason: "resume-attempts-exhausted"
            },
        );
        assert_eq!(
            advance_limit_resume(
                &mut state,
                &incident,
                now,
                tick + chrono::Duration::minutes(10),
                true,
            ),
            LimitResumeAction::None,
        );
    }

    #[test]
    fn limit_resume_unparseable_banner_warns_once_never_guesses() {
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        let mut state = LimitResumeState::default();
        let invalid_zone = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "bad-zone".into(),
            reset_hint: Some("7:20am (ET)".into()),
        };

        // A zone we can't resolve must not drive a wrong-time resume.
        assert_eq!(
            advance_limit_resume(&mut state, &invalid_zone, now, now, true),
            LimitResumeAction::NeedsHuman {
                reason: "unparseable-reset"
            },
        );
        // Same banner keeps quiet — one warning per incident.
        assert_eq!(
            advance_limit_resume(&mut state, &invalid_zone, now, now, true),
            LimitResumeAction::None,
        );
        // A banner with no reset wording at all takes the same path.
        let mut state = LimitResumeState::default();
        let no_hint = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "no-hint".into(),
            reset_hint: None,
        };
        assert_eq!(
            advance_limit_resume(&mut state, &no_hint, now, now, true),
            LimitResumeAction::NeedsHuman {
                reason: "unparseable-reset"
            },
        );
        assert_eq!(
            advance_limit_resume(&mut state, &no_hint, now, now, true),
            LimitResumeAction::None
        );
    }

    #[test]
    fn limit_resume_remote_banner_without_timezone_needs_human() {
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        let incident = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "remote-no-zone".into(),
            reset_hint: Some("7:20am".into()),
        };
        let mut state = LimitResumeState::default();
        assert_eq!(
            advance_limit_resume(&mut state, &incident, now, now, false),
            LimitResumeAction::NeedsHuman {
                reason: "missing-timezone-remote"
            },
        );
    }

    #[test]
    fn limit_resume_restart_rebuilds_due_from_persisted_stall_time() {
        let stalled_at = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        let restarted_at = Utc.with_ymd_and_hms(2026, 7, 11, 10, 30, 0).unwrap();
        let due = Utc.with_ymd_and_hms(2026, 7, 11, 6, 20, 0).unwrap()
            + chrono::Duration::seconds(LIMIT_RESUME_GRACE_SECS);
        let incident = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "session-limit-2:20".into(),
            reset_hint: Some("2:20am (America/New_York)".into()),
        };
        let mut state = LimitResumeState::default();

        assert_eq!(
            advance_limit_resume(&mut state, &incident, stalled_at, restarted_at, true),
            LimitResumeAction::Scheduled { due },
        );
        assert_eq!(
            advance_limit_resume(&mut state, &incident, stalled_at, restarted_at, true),
            LimitResumeAction::Attempt {
                incident: incident.clone()
            },
        );
    }

    #[test]
    fn limit_resume_fresh_banner_restarts_the_cycle() {
        // Worker resumed, worked, and hit the NEXT window: the new banner
        // carries a different reset time and must re-schedule from scratch —
        // even from the needs-human latch.
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 2, 28, 0).unwrap();
        let old = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "bad-zone".into(),
            reset_hint: Some("7:20am (ET)".into()),
        };
        let mut state = LimitResumeState::NeedsHuman { incident: old };
        let current = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "session-limit-7:20".into(),
            reset_hint: Some("7:20am (America/New_York)".into()),
        };
        let due = Utc.with_ymd_and_hms(2026, 7, 11, 11, 20, 0).unwrap()
            + chrono::Duration::seconds(LIMIT_RESUME_GRACE_SECS);
        assert_eq!(
            advance_limit_resume(&mut state, &current, now, now, true),
            LimitResumeAction::Scheduled { due },
        );
        // And a different hint while Scheduled also re-schedules (the stall
        // rolled to a new window before the old due fired).
        let next = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "session-limit-11:20".into(),
            reset_hint: Some("11:20am (America/New_York)".into()),
        };
        assert_eq!(
            advance_limit_resume(&mut state, &next, now, now, true),
            LimitResumeAction::Scheduled {
                due: Utc.with_ymd_and_hms(2026, 7, 11, 15, 20, 0).unwrap()
                    + chrono::Duration::seconds(LIMIT_RESUME_GRACE_SECS)
            },
        );
    }

    #[test]
    fn limit_resume_needs_human_survives_banner_free_ticks_until_live_recovery() {
        let incident = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "session-limit-7:20".into(),
            reset_hint: Some("7:20am (America/New_York)".into()),
        };
        let mut uncertain = LimitResumeState::NeedsHuman {
            incident: incident.clone(),
        };
        assert_eq!(
            advance_limit_resume_without_banner(&mut uncertain, false),
            None
        );
        assert!(matches!(uncertain, LimitResumeState::NeedsHuman { .. }));

        // A human resumed before the scheduled due tick. Invalidating the
        // unfired schedule is what prevents its later prompt from duplicating
        // that manual recovery.
        let mut unfired = LimitResumeState::Scheduled {
            incident: incident.clone(),
            due: Utc::now() + chrono::Duration::minutes(5),
            attempts: 0,
        };
        assert_eq!(
            advance_limit_resume_without_banner(&mut unfired, false),
            None
        );
        assert_eq!(unfired, LimitResumeState::Idle);

        // The exact modal was present when the safe retry outcome returned,
        // then disappeared before the next heartbeat. Surface that ambiguity
        // once and retain it rather than silently becoming Idle.
        let mut attempted = LimitResumeState::Scheduled {
            incident,
            due: Utc::now(),
            attempts: 1,
        };
        assert_eq!(
            advance_limit_resume_without_banner(&mut attempted, false),
            Some("banner-gone-after-retryable-attempt")
        );
        assert!(matches!(attempted, LimitResumeState::NeedsHuman { .. }));
        assert_eq!(
            advance_limit_resume_without_banner(&mut attempted, false),
            None
        );

        advance_limit_resume_without_banner(&mut attempted, true);
        assert_eq!(attempted, LimitResumeState::Idle);
    }

    #[test]
    fn limit_resume_schedule_caps_slow_polling_and_wakes_at_due_time() {
        let now = Utc.with_ymd_and_hms(2026, 7, 11, 6, 0, 0).unwrap();
        let configured = Duration::from_secs(10 * 60);
        let incident = LimitResumeIncident {
            task_id: "task-a".into(),
            banner: "session-limit".into(),
            reset_hint: Some("2:20am (America/New_York)".into()),
        };
        let scheduled = |due| LimitResumeState::Scheduled {
            incident: incident.clone(),
            due,
            attempts: 0,
        };

        assert_eq!(
            limit_resume_sleep_interval(
                &scheduled(now + chrono::Duration::minutes(5)),
                configured,
                now,
            ),
            LIMIT_RESUME_MAX_POLL_SLEEP
        );
        assert_eq!(
            limit_resume_sleep_interval(
                &scheduled(now + chrono::Duration::seconds(10)),
                configured,
                now,
            ),
            Duration::from_secs(10)
        );
        assert_eq!(
            limit_resume_sleep_interval(
                &scheduled(now - chrono::Duration::seconds(1)),
                configured,
                now,
            ),
            Duration::from_secs(1)
        );
        assert_eq!(
            limit_resume_sleep_interval(&LimitResumeState::Idle, configured, now),
            configured
        );
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
                prompt_injection: None,
                dialog_signatures: vec![],
                integration: None,
            },
        );
        Project {
            name: "demo".into(),
            display_name: None,
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: work_dir.to_path_buf(),
                host: None,
                tags: Vec::new(),
                forward: None,
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
                tags: Vec::new(),
                slot: None,
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    fn in_progress_task(id: &str, workspace: &str) -> Task {
        let now = Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: Column::in_progress(),
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

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
    }

    fn kill_tmux_session(session: &str) {
        let _ = std::process::Command::new("tmux")
            .args(["kill-session", "-t", &format!("={session}")])
            .output();
    }

    /// Start the fake-Claude pane and wait until tmux has registered it. Real
    /// tmux tests share one server, whose socket can briefly race concurrent
    /// session creation, so use the retry pattern from the orchestrator's
    /// existing tmux integration tests rather than trusting one spawn.
    fn start_limit_resume_tmux_session(
        session: &str,
        script: &std::path::Path,
        receipt: &std::path::Path,
    ) {
        kill_tmux_session(session);
        for _ in 0..50 {
            let _ = std::process::Command::new("tmux")
                .args(["new-session", "-d", "-s", session, "-n", "alpha"])
                .arg("sh")
                .arg(script)
                .arg(receipt)
                .status();
            let live = std::process::Command::new("tmux")
                .args(["has-session", "-t", &format!("={session}")])
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false);
            if live {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!("tmux session `{session}` never came up");
    }

    fn wait_for_limit_modal(host: &Host, addr: &TmuxAddr) -> String {
        let start = std::time::Instant::now();
        let timeout = std::time::Duration::from_secs(3);
        while start.elapsed() < timeout {
            let screen = shelbi_tmux::capture(host, addr).unwrap_or_default();
            if shelbi_orchestrator::ready::detect_usage_limit(&screen).is_some() {
                return screen;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        panic!(
            "fake Claude never rendered its limit modal; last screen:\n{}",
            shelbi_tmux::capture(host, addr).unwrap_or_default()
        );
    }

    fn sidebar_badge(project: &str, workspace: &str) -> crate::WorkspaceBadge {
        let mut app = crate::App::new_sidebar(project);
        app.refresh().unwrap();
        app.rows()
            .into_iter()
            .find_map(|row| match row {
                crate::Row::Workspace { name, badge, .. } if name == workspace => Some(badge),
                _ => None,
            })
            .unwrap_or_else(|| panic!("workspace `{workspace}` missing from sidebar rows"))
    }

    struct LimitResumeTmuxCleanup {
        session: String,
        home: std::path::PathBuf,
        prior_home: Option<std::ffi::OsString>,
        prior_hub_sock: Option<std::ffi::OsString>,
    }

    impl Drop for LimitResumeTmuxCleanup {
        fn drop(&mut self) {
            kill_tmux_session(&self.session);
            if let Some(home) = &self.prior_home {
                std::env::set_var("SHELBI_HOME", home);
            } else {
                std::env::remove_var("SHELBI_HOME");
            }
            if let Some(sock) = &self.prior_hub_sock {
                std::env::set_var("SHELBI_HUB_SOCK", sock);
            } else {
                std::env::remove_var("SHELBI_HUB_SOCK");
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    #[test]
    fn limit_resume_eligibility_is_task_runner_and_workflow_bound() {
        let _env = crate::test_support::ENV_LOCK.lock().unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let project_name = format!("limit-resume-gates-{nonce}");
        let home = std::env::temp_dir().join(&project_name);
        std::fs::create_dir_all(&home).unwrap();
        let prior_home = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", &home);
        let _cleanup = LimitResumeTmuxCleanup {
            session: format!("unused-{project_name}"),
            home: home.clone(),
            prior_home,
            prior_hub_sock: std::env::var_os("SHELBI_HUB_SOCK"),
        };

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        project.name.clone_from(&project_name);
        project.default_workflow = Some("custom".into());
        shelbi_state::save_project(&project).unwrap();
        shelbi_state::save_project_statuses(
            &project.name,
            &shelbi_core::ProjectStatuses {
                statuses: vec![
                    shelbi_core::ProjectStatus {
                        id: "backlog".into(),
                        name: "Backlog".into(),
                        category: StatusCategory::Backlog,
                    },
                    shelbi_core::ProjectStatus {
                        id: "coding".into(),
                        name: "Coding".into(),
                        category: StatusCategory::Active,
                    },
                    shelbi_core::ProjectStatus {
                        id: "done".into(),
                        name: "Done".into(),
                        category: StatusCategory::Done,
                    },
                ],
            },
        )
        .unwrap();
        let workflow_path = shelbi_state::workflow_path(&project.name, "custom").unwrap();
        std::fs::write(
            workflow_path,
            "name: custom\nstatuses:\n  - { id: backlog, owner: user }\n  - { id: coding, owner: user }\n  - { id: done, owner: user }\n",
        )
        .unwrap();

        let task_id = "custom-active-task";
        let mut task = in_progress_task(task_id, "alpha");
        task.column = Column::from_status_id("coding");
        task.workflow = Some("custom".into());
        shelbi_state::save_task(&project.name, &task, "work").unwrap();

        assert_eq!(
            current_task_for(&project, "alpha").as_deref(),
            Some(task_id)
        );
        assert!(limit_resume_eligible_now(&project.name, "alpha", task_id));
        assert!(!limit_resume_eligible_now(
            &project.name,
            "alpha",
            "different-task"
        ));

        task.assigned_to = Some("beta".into());
        shelbi_state::save_task(&project.name, &task, "work").unwrap();
        assert!(!limit_resume_eligible_now(&project.name, "alpha", task_id));

        task.assigned_to = Some("alpha".into());
        task.column = Column::from_status_id("done");
        shelbi_state::save_task(&project.name, &task, "work").unwrap();
        assert!(!limit_resume_eligible_now(&project.name, "alpha", task_id));

        task.column = Column::from_status_id("coding");
        shelbi_state::save_task(&project.name, &task, "work").unwrap();
        project.agent_runners.get_mut("claude").unwrap().command = "codex".into();
        shelbi_state::save_project(&project).unwrap();
        assert!(!limit_resume_eligible_now(&project.name, "alpha", task_id));
    }

    /// Full wire-path regression for a limited Claude worker: the poller
    /// captures a real tmux pane, schedules the structurally current banner,
    /// dismisses the modal, delivers + verifies the prompt, clears the pause
    /// badge, and suppresses the same stale banner on the next heartbeat.
    #[test]
    fn limit_resume_tmux_round_trip_clears_pause_without_duplicate() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }

        let _env = crate::test_support::ENV_LOCK.lock().unwrap();
        let nonce = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let project_name = format!("limit-resume-e2e-{nonce}");
        let session = format!("shelbi-{project_name}");
        let home = std::env::temp_dir().join(&project_name);
        std::fs::create_dir_all(&home).unwrap();
        let prior_home = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", &home);
        let prior_hub_sock = std::env::var_os("SHELBI_HUB_SOCK");
        let hub_sock = home.join("hub.sock");
        let hub = std::os::unix::net::UnixListener::bind(&hub_sock).unwrap();
        std::env::set_var("SHELBI_HUB_SOCK", &hub_sock);
        let matching_daemon = std::thread::spawn(move || {
            use std::io::{Read, Write};
            for _ in 0..3 {
                let (mut stream, _) = hub.accept().unwrap();
                let mut request = Vec::new();
                stream.read_to_end(&mut request).unwrap();
                assert!(request.is_empty(), "version probe must send no frame");
                stream
                    .write_all(
                        shelbi_state::DaemonHello::new(env!("CARGO_PKG_VERSION"))
                            .to_line()
                            .as_bytes(),
                    )
                    .unwrap();
            }
        });
        let _cleanup = LimitResumeTmuxCleanup {
            session: session.clone(),
            home: home.clone(),
            prior_home,
            prior_hub_sock,
        };

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let mut project = local_project(&work_dir);
        project.name.clone_from(&project_name);
        shelbi_state::save_project(&project).unwrap();
        let task_id = "resume-limited-worker";
        shelbi_state::save_task(
            &project.name,
            &in_progress_task(task_id, "alpha"),
            "keep working",
        )
        .unwrap();

        // A tiny deterministic terminal app stands in for Claude. The first
        // read is the modal-selection Enter. The second is the resume prompt;
        // after consuming it the app deliberately clobbers its title and
        // redraws stale modal pixels beside a genuine busy footer. That final
        // screen pins the Resumed latch and direct badge-clear behavior.
        let receipt = home.join("fake-claude.receipt");
        let script = home.join("fake-claude.sh");
        std::fs::write(
            &script,
            r#"receipt=$1
show_current_modal() {
  printf '%s\n' \
    "⏱ You've hit your session limit · resets 7:20am (America/New_York)" \
    "" \
    " ❯ 1. Stop and wait for limit to reset" \
    "   2. Upgrade your plan"
}

printf '\033]2;shelbi:working\007'
printf '\033[2J\033[H'
printf '%s\n' \
  "Earlier conversation:" \
  "⏱ You've hit your session limit · resets 1:05am (Europe/London)" \
  " ❯ 1. Stop and wait for limit to reset" \
  "   2. Upgrade your plan" \
  ""
show_current_modal

IFS= read -r modal_choice
printf 'dismissed=%s\n' "$modal_choice" > "$receipt"
printf '\033[2J\033[H'
printf '%s\n' \
  "────────────────────────────────────────────────────────" \
  "❯ " \
  "────────────────────────────────────────────────────────" \
  "  ⏵⏵ auto mode on (shift+tab to cycle)"

IFS= read -r prompt
printf 'prompt=%s\n' "$prompt" >> "$receipt"
printf '\033]2;Claude Code\007'
printf '\033[2J\033[H'
show_current_modal
printf '%s\n' "" "⏺ Working…" "  esc to interrupt"
while :; do sleep 60; done
"#,
        )
        .unwrap();
        start_limit_resume_tmux_session(&session, &script, &receipt);

        let host = Host::Local;
        let addr = TmuxAddr {
            session,
            window: "alpha".into(),
        };
        let initial_screen = wait_for_limit_modal(&host, &addr);
        assert!(initial_screen.contains("1:05am (Europe/London)"));
        assert!(initial_screen.contains("7:20am (America/New_York)"));

        let workspace = &project.workspaces[0];
        let mut last_known = None;
        let mut last_dialog = None;
        let mut supervision = SupervisionState::default();
        let mut limit_resume = LimitResumeState::default();

        // Banner -> scheduled: persist the pause and surface the actual badge,
        // but do not touch the modal before the stated due time.
        poll_one(
            &project,
            workspace,
            &mut last_known,
            &mut last_dialog,
            &mut supervision,
            &mut limit_resume,
        );
        assert_eq!(last_known, Some(WorkspaceState::Paused));
        let paused = load_workspace_status("alpha").unwrap().unwrap();
        assert_eq!(paused.state, WorkspaceState::Paused);
        assert_eq!(paused.current_task.as_deref(), Some(task_id));
        let badge = sidebar_badge(&project.name, "alpha");
        assert_eq!(badge, crate::WorkspaceBadge::Paused);
        assert_eq!(badge.glyph(), "⏸");
        assert!(!receipt.exists(), "the modal was touched before due");

        let due = match &mut limit_resume {
            LimitResumeState::Scheduled {
                incident,
                due,
                attempts,
            } => {
                assert_eq!(incident.task_id, task_id);
                assert_eq!(
                    incident.banner,
                    "⏱ You've hit your session limit · resets 7:20am (America/New_York)"
                );
                assert_eq!(
                    incident.reset_hint.as_deref(),
                    Some("7:20am (America/New_York)")
                );
                assert_eq!(*attempts, 0);
                let scheduled = *due;
                *due = Utc::now() - chrono::Duration::seconds(1);
                scheduled
            }
            other => panic!("expected a scheduled resume, got {other:?}"),
        };
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("supervision=limit-resume status=scheduled"))
                .count(),
            1,
            "events.log: {log}"
        );
        assert!(
            log.contains(&format!("scheduled_for={}", due.to_rfc3339())),
            "events.log: {log}"
        );
        assert!(
            !log.contains("reset=1:05am_(Europe/London)"),
            "stale reset leaked into the scheduled incident: {log}"
        );

        // Due -> modal dismissal -> verified submission. The fake app only
        // writes the prompt receipt after its terminal read consumed Enter.
        poll_one(
            &project,
            workspace,
            &mut last_known,
            &mut last_dialog,
            &mut supervision,
            &mut limit_resume,
        );
        assert_eq!(
            std::fs::read_to_string(&receipt).unwrap(),
            format!("dismissed=\nprompt={LIMIT_RESUME_PROMPT}\n")
        );
        assert_eq!(last_known, Some(WorkspaceState::Working));
        let working = load_workspace_status("alpha").unwrap().unwrap();
        assert_eq!(working.state, WorkspaceState::Working);
        assert_eq!(working.current_task.as_deref(), Some(task_id));
        let badge = sidebar_badge(&project.name, "alpha");
        assert_eq!(badge, crate::WorkspaceBadge::Working);
        assert_eq!(badge.glyph(), "⏵");
        assert!(matches!(limit_resume, LimitResumeState::Resumed { .. }));

        let stale_screen = shelbi_tmux::capture(&host, &addr).unwrap();
        assert!(stale_screen.contains("7:20am (America/New_York)"));
        assert!(stale_screen.contains("esc to interrupt"));
        let title = shelbi_tmux::pane_title(&host, &addr).unwrap();
        assert!(
            parse_pane_title_marker(&title).is_none(),
            "fake Claude must clobber the working marker, got `{title}`"
        );

        // Simulate a poller restart that inherited Paused on disk from a
        // manual-resume race and lost its in-memory Resumed latch. The live
        // busy footer must recover the badge even though the old modal and a
        // clobbered title remain visible; it must not deliver a duplicate.
        let restart_time = Utc::now();
        save_workspace_status(&WorkspaceStatus {
            workspace: "alpha".into(),
            current_task: Some(task_id.into()),
            state: WorkspaceState::Paused,
            last_transition: restart_time,
            last_seen: restart_time,
        })
        .unwrap();
        last_known = None;
        limit_resume = LimitResumeState::default();
        poll_one(
            &project,
            workspace,
            &mut last_known,
            &mut last_dialog,
            &mut supervision,
            &mut limit_resume,
        );
        assert_eq!(last_known, Some(WorkspaceState::Working));
        assert_eq!(
            load_workspace_status("alpha").unwrap().unwrap().state,
            WorkspaceState::Working
        );
        assert_eq!(
            sidebar_badge(&project.name, "alpha"),
            crate::WorkspaceBadge::Working
        );
        assert_eq!(
            std::fs::read_to_string(&receipt).unwrap(),
            format!("dismissed=\nprompt={LIMIT_RESUME_PROMPT}\n")
        );

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("supervision=limit-resume status=scheduled"))
                .count(),
            1,
            "events.log: {log}"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("supervision=limit-resume status=sent"))
                .count(),
            1,
            "events.log: {log}"
        );
        assert_eq!(
            log.lines()
                .filter(|line| line.contains(" -> paused reason=usage-limit"))
                .count(),
            1,
            "events.log: {log}"
        );
        assert!(!log.contains("status=failed"), "events.log: {log}");
        matching_daemon.join().unwrap();
    }

    fn write_marker(project: &Project, body: &str) -> std::path::PathBuf {
        let marker = shelbi_orchestrator::workspace::workspace_ready_marker(
            &project.machines[0],
            &project.workspaces[0],
        );
        std::fs::create_dir_all(marker.parent().unwrap()).unwrap();
        std::fs::write(&marker, body).unwrap();
        marker
    }

    #[test]
    fn poll_tick_preserves_markers_and_task_on_daemon_version_mismatch() {
        use std::io::{Read, Write};
        use std::os::unix::net::UnixListener;

        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-version-mismatch-{}-{nonce}",
            std::process::id(),
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        let project = local_project(&work_dir);
        shelbi_state::save_task("demo", &in_progress_task("fix-login", "alpha"), "body").unwrap();

        let ready_marker = write_marker(&project, "fix-login\n");
        let transition_marker = shelbi_orchestrator::workspace::workspace_transition_marker(
            &project.machines[0],
            &project.workspaces[0],
        );
        std::fs::write(&transition_marker, "fix-login\nreview\n").unwrap();
        let task_path = shelbi_state::task_path("demo", "fix-login").unwrap();
        let task_before = std::fs::read(&task_path).unwrap();

        // The probe protocol is an empty half-closed connection. Advertise a
        // different workspace version so poll_one must return before reading
        // either marker or reaching any tmux operation.
        let sock =
            std::env::temp_dir().join(format!("shb-pm-{}-{nonce}.sock", std::process::id(),));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        std::env::set_var("SHELBI_HUB_SOCK", &sock);
        let daemon = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            stream.read_to_end(&mut request).unwrap();
            assert!(request.is_empty(), "version probe must send no frame");
            let daemon_version = if env!("CARGO_PKG_VERSION") == "0.0.0" {
                "0.0.1"
            } else {
                "0.0.0"
            };
            stream
                .write_all(
                    shelbi_state::DaemonHello::new(daemon_version)
                        .to_line()
                        .as_bytes(),
                )
                .unwrap();
        });

        let mut last_known = None;
        let mut last_dialog = None;
        let mut supervision = SupervisionState::default();
        let mut limit_resume = LimitResumeState::default();
        poll_one(
            &project,
            &project.workspaces[0],
            &mut last_known,
            &mut last_dialog,
            &mut supervision,
            &mut limit_resume,
        );
        daemon.join().unwrap();

        assert_eq!(
            std::fs::read(&ready_marker).unwrap(),
            b"fix-login\n",
            "ready marker must survive the mismatched tick",
        );
        assert_eq!(
            std::fs::read(&transition_marker).unwrap(),
            b"fix-login\nreview\n",
            "transition marker must survive the mismatched tick",
        );
        assert_eq!(
            std::fs::read(&task_path).unwrap(),
            task_before,
            "task frontmatter and body must remain byte-for-byte unchanged",
        );
        assert!(last_known.is_none(), "workspace state must not be sampled");

        std::env::remove_var("SHELBI_HUB_SOCK");
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_file(&sock);
        let _ = std::fs::remove_dir_all(&home);
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

        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );

        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::review(),
            "task should be promoted to review"
        );
        assert!(!marker.exists(), "marker should be consumed (cleared)");

        // The promotion must also append a `task=...` line to events.log
        // tagged with the marker-driven reason, so `shelbi events tail`
        // surfaces the handoff as part of the canonical event stream.
        // Shape from `Plans/workflows.md` §10. We match on the canonical
        // `<ts> task=<id> <from> -> <to>` shape so other event kinds that
        // happen to mention the same task id don't get counted as task
        // transitions.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let task_lines: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(" project=demo task=fix-login "))
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
            task_lines[0].contains(" reason=workspace:ready-marker "),
            "line: {}",
            task_lines[0]
        );
        assert!(
            task_lines[0].ends_with(" to_category=handoff"),
            "line: {}",
            task_lines[0]
        );

        // A leftover/stale marker naming a task that's no longer in-progress
        // for this workspace is cleared without moving anything back out.
        let marker = write_marker(&project, "fix-login\n");
        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );
        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::review(),
            "task already in review must not be pulled back out"
        );
        assert!(!marker.exists(), "stale marker should be cleared");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn git_available() -> bool {
        std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn git_in(dir: &std::path::Path, args: &[&str]) -> std::process::Output {
        let mut cmd = std::process::Command::new("git");
        cmd.arg("-C").arg(dir);
        for a in args {
            cmd.arg(a);
        }
        cmd.env("GIT_AUTHOR_NAME", "Shelbi Test")
            .env("GIT_AUTHOR_EMAIL", "test@shelbi.local")
            .env("GIT_COMMITTER_NAME", "Shelbi Test")
            .env("GIT_COMMITTER_EMAIL", "test@shelbi.local");
        cmd.output().expect("git failed to spawn")
    }

    #[test]
    fn review_marker_detaches_worker_worktree_and_frees_branch() {
        // End-to-end: a normal ready-marker handoff must, after the rebase +
        // column move, detach the worker's worktree from its task branch so the
        // branch is no longer held — free for the review checkout / merge /
        // delete — while the branch ref and its commits survive.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-detach-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&home).unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // A real repo at work_dir doubles as the main clone; the workspace's
        // worktree is checked out on the task branch, cut from main.
        let work_dir = home.join("repo");
        std::fs::create_dir_all(&work_dir).unwrap();
        assert!(git_in(&work_dir, &["init", "-q", "-b", "main"])
            .status
            .success());
        std::fs::write(work_dir.join("README.md"), "# repo\n").unwrap();
        assert!(git_in(&work_dir, &["add", "README.md"]).status.success());
        assert!(git_in(&work_dir, &["commit", "-q", "-m", "init"])
            .status
            .success());

        let project = local_project(&work_dir);
        let wt = shelbi_orchestrator::workspace::workspace_worktree(
            &project.machines[0],
            &project.workspaces[0],
        );
        std::fs::create_dir_all(wt.parent().unwrap()).unwrap();
        assert!(git_in(
            &work_dir,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "shelbi/fix-login",
                wt.to_str().unwrap(),
                "main"
            ],
        )
        .status
        .success());
        assert_eq!(
            String::from_utf8_lossy(&git_in(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout)
                .trim(),
            "shelbi/fix-login",
        );

        shelbi_state::save_task("demo", &in_progress_task("fix-login", "alpha"), "body").unwrap();
        let marker = write_marker(&project, "fix-login\n");

        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );

        // Handoff landed.
        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::review(),
            "task should be promoted to review"
        );
        assert!(!marker.exists(), "marker should be consumed");

        // Worktree HEAD is detached; the branch is no longer held.
        assert_eq!(
            String::from_utf8_lossy(&git_in(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]).stdout)
                .trim(),
            "HEAD",
            "worker worktree must be detached from the task branch"
        );
        // Branch ref + commits survive the detach.
        assert!(
            git_in(&work_dir, &["rev-parse", "--verify", "shelbi/fix-login"])
                .status
                .success(),
            "task branch ref must be preserved"
        );
        // Branch is free: it deletes without an `already checked out` error.
        assert!(
            git_in(&work_dir, &["branch", "-D", "shelbi/fix-login"])
                .status
                .success(),
            "freed branch must be deletable"
        );

        // The detach is recorded as a traceable ok event.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let detach_lines: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(" worktree-detach ") && l.contains(" task=fix-login "))
            .collect();
        assert_eq!(detach_lines.len(), 1, "log: {log:?}");
        assert!(
            detach_lines[0].contains(" detached-from=shelbi/fix-login ")
                && detach_lines[0].contains(" status=ok"),
            "line: {}",
            detach_lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn review_marker_detach_failure_still_leaves_task_in_handoff() {
        // A detach failure must not roll back or block the handoff: the task
        // stays promoted and the failure is emitted as a traceable
        // `reason=worktree-detach-failed` event.
        if !git_available() {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-detachfail-{}-{}",
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

        // A worktree dir whose `.git` is present-but-invalid: the existence
        // probe passes, but `git checkout --detach` fails — simulating a broken
        // worktree the handoff must survive.
        let wt = shelbi_orchestrator::workspace::workspace_worktree(
            &project.machines[0],
            &project.workspaces[0],
        );
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /nonexistent\n").unwrap();

        shelbi_state::save_task("demo", &in_progress_task("fix-login", "alpha"), "body").unwrap();
        let marker = write_marker(&project, "fix-login\n");

        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );

        // Handoff still stands despite the detach failure.
        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::review(),
            "handoff must not roll back on a detach failure"
        );
        assert!(!marker.exists(), "marker should still be consumed");

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(
            log.lines().any(|l| l.contains(" worktree-detach ")
                && l.contains(" task=fix-login ")
                && l.contains("reason=worktree-detach-failed")),
            "a detach failure must be traceably logged; log: {log:?}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Materialize a named workflow (reference form) plus the default
    /// `statuses.yaml` and bundled agents, so `load_task_workflow` resolves
    /// it through the real on-disk loader.
    fn write_project_workflow(name: &str, yaml: &str) {
        shelbi_state::materialize_default_agents("demo").unwrap();
        shelbi_state::save_project_statuses("demo", &shelbi_core::default_project_statuses())
            .unwrap();
        let path = shelbi_state::workflow_path("demo", name).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, yaml).unwrap();
    }

    #[test]
    fn handoffless_workflow_auto_advances_along_merge_edge() {
        // A workflow with NO handoff-category status but an
        // `in-progress -> done: [merge, delete_branch]` edge — the
        // `app-feature-subtask` shape. The ready marker must advance the
        // task straight to done instead of stranding it in-progress.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-automerge-{}-{}",
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
        write_project_workflow(
            "subtask",
            r#"
name: subtask
statuses:
  - { id: todo,        owner: agent, agent: orchestrator }
  - { id: in-progress, owner: agent, agent: developer    }
  - { id: done,        owner: user }
transitions:
  - { from: todo, to: in-progress }
  - { from: in-progress, to: done, actions: [merge, delete_branch] }
"#,
        );
        let mut task = in_progress_task("subtask-a", "alpha");
        task.workflow = Some("subtask".into());
        shelbi_state::save_task("demo", &task, "body").unwrap();

        let marker = write_marker(&project, "subtask-a\n");
        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );

        assert_eq!(
            shelbi_state::load_task("demo", "subtask-a")
                .unwrap()
                .task
                .column,
            Column::done(),
            "task should auto-advance along the merge edge"
        );
        assert!(!marker.exists(), "marker should be consumed (cleared)");

        // The move lands in the canonical event stream like any other
        // marker-driven advance.
        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let task_lines: Vec<&str> = log
            .lines()
            .filter(|l| l.contains(" project=demo task=subtask-a "))
            .collect();
        assert_eq!(task_lines.len(), 1, "log: {log:?}");
        assert!(
            task_lines[0].contains(" in_progress -> done "),
            "line: {}",
            task_lines[0]
        );
        assert!(
            task_lines[0].contains(" reason=workspace:ready-marker "),
            "line: {}",
            task_lines[0]
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn workflow_without_handoff_or_merge_edge_warns_and_clears_marker() {
        // Neither a handoff status nor a merge-firing edge out of the
        // active status — still a misconfiguration: the task stays put and
        // the marker is consumed so it doesn't re-log every tick.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-deadend-{}-{}",
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
        write_project_workflow(
            "deadend",
            r#"
name: deadend
statuses:
  - { id: todo,        owner: agent, agent: orchestrator }
  - { id: in-progress, owner: agent, agent: developer    }
  - { id: done,        owner: user }
transitions:
  - { from: todo, to: in-progress }
  - { from: in-progress, to: done }
"#,
        );
        let mut task = in_progress_task("stuck-a", "alpha");
        task.workflow = Some("deadend".into());
        shelbi_state::save_task("demo", &task, "body").unwrap();

        let marker = write_marker(&project, "stuck-a\n");
        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );

        assert_eq!(
            shelbi_state::load_task("demo", "stuck-a")
                .unwrap()
                .task
                .column,
            Column::in_progress(),
            "task must stay in-progress when no advance target exists"
        );
        assert!(!marker.exists(), "misconfigured marker should be cleared");

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
        maybe_apply_ready_handoff(
            &project,
            &project.workspaces[0],
            &project.machines[0],
            &Host::Local,
            &TmuxAddr {
                session: "s".into(),
                window: "w".into(),
            },
        );
        assert_eq!(
            shelbi_state::load_task("demo", "fix-login")
                .unwrap()
                .task
                .column,
            Column::in_progress()
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
        assert!(
            hb.next_attempt.is_some(),
            "first tick must seed the schedule"
        );
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
    fn zen_heartbeat_cue_only_fires_when_zen_is_on() {
        // Zen off (no state.json) → no cue at all, and any stale Zen cadence
        // counters are cleared so a later off→on re-enable starts fresh.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-zencue-off-{}-{}",
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

        let mut hb = HeartbeatSchedule {
            zen_heartbeats: 5,
            zen_last_reread: Some(Instant::now()),
            ..HeartbeatSchedule::default()
        };
        assert_eq!(zen_heartbeat_cue(&project, &mut hb, Instant::now()), None);
        assert_eq!(hb.zen_heartbeats, 0, "off must reset the summary counter");
        assert_eq!(hb.zen_last_reread, None, "off must reset the reread timer");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn zen_heartbeat_cue_summary_and_reread_cadences() {
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-zencue-on-{}-{}",
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

        shelbi_state::set_zen_mode("demo", ZenModeState::On, "test").unwrap();
        shelbi_state::scaffold_zenmode("demo").unwrap();
        let summary = shelbi_state::read_zenmode_summary("demo").unwrap().unwrap();

        let mut hb = HeartbeatSchedule::default();
        let t0 = Instant::now();

        // Summary cadence: bare `zen=on` on the ticks in between, the one-line
        // summary on every ZEN_SUMMARY_EVERY_N_HEARTBEATS-th Zen heartbeat.
        // The first tick also seeds the reread timer.
        for i in 1..ZEN_SUMMARY_EVERY_N_HEARTBEATS {
            assert_eq!(
                zen_heartbeat_cue(&project, &mut hb, t0),
                Some(ZenHeartbeatCue::Plain),
                "heartbeat {i} should be plain"
            );
        }
        assert!(hb.zen_last_reread.is_some(), "first Zen tick seeds reread");
        assert_eq!(
            zen_heartbeat_cue(&project, &mut hb, t0),
            Some(ZenHeartbeatCue::Summary(summary.clone())),
            "Nth heartbeat carries the fresh summary"
        );

        // Reread cadence: once the interval elapses the next tick injects the
        // full re-read cue (subsuming the summary), then the timer resets so
        // the following tick is not another reread.
        let later = t0 + ZEN_REREAD_INTERVAL;
        assert_eq!(
            zen_heartbeat_cue(&project, &mut hb, later),
            Some(ZenHeartbeatCue::Reread),
        );
        assert!(
            !matches!(
                zen_heartbeat_cue(&project, &mut hb, later),
                Some(ZenHeartbeatCue::Reread)
            ),
            "reread must not fire twice back-to-back"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn zen_heartbeat_cue_degrades_to_plain_when_zenmode_missing() {
        // Zen on but no zenmode.md on disk (e.g. before the next reload
        // materializes it): the summary tick degrades to a bare `zen=on`
        // rather than dropping the heartbeat.
        let _g = crate::test_support::ENV_LOCK.lock().unwrap();
        let home = std::env::temp_dir().join(format!(
            "shelbi-poller-zencue-missing-{}-{}",
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

        shelbi_state::set_zen_mode("demo", ZenModeState::On, "test").unwrap();
        // Deliberately do NOT scaffold zenmode.md.
        assert_eq!(shelbi_state::read_zenmode_summary("demo").unwrap(), None);

        let mut hb = HeartbeatSchedule::default();
        let t0 = Instant::now();
        for _ in 1..ZEN_SUMMARY_EVERY_N_HEARTBEATS {
            zen_heartbeat_cue(&project, &mut hb, t0);
        }
        // The summary tick lands on Plain because the file can't be read.
        assert_eq!(
            zen_heartbeat_cue(&project, &mut hb, t0),
            Some(ZenHeartbeatCue::Plain),
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
        let hb_lines: Vec<&str> = body.lines().filter(|l| l.contains("heartbeat")).collect();
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
            ..HeartbeatSchedule::default()
        };
        maybe_emit_heartbeat(&project, &mut hb, || true);
        assert!(
            hb.next_attempt.is_none(),
            "off must clear the pending schedule"
        );
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
        let online_hb_lines: Vec<&str> = body.lines().filter(|l| l.contains("heartbeat")).collect();
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
        t.column = Column::todo();
        t.assigned_to = None;
        t
    }

    fn review_task(id: &str) -> Task {
        let mut t = in_progress_task(id, "alpha");
        t.column = Column::review();
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
                t.column = Column::backlog();
                t
            }),
            tf({
                let mut t = todo_task("d");
                t.column = Column::done();
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
                t.column = Column::backlog();
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
        assert_eq!(
            hb.interval,
            Some(Duration::from_secs(1)),
            "seeded at standard"
        );

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
            decide_transition(&wf, Column::review(), "in-progress"),
            TransitionDecision::Apply {
                from_status: "review".into(),
                to_status: "in-progress".into(),
                to_column: Column::in_progress(),
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
                decide_transition(&wf, Column::review(), verb),
                TransitionDecision::Apply {
                    from_status: "review".into(),
                    to_status: "in-progress".into(),
                    to_column: Column::in_progress(),
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
            decide_transition(&wf, Column::review(), "backlog"),
            TransitionDecision::Reject {
                reason: "edge-not-in-workflow"
            }
        );
    }

    #[test]
    fn decide_transition_rejects_unknown_target_status() {
        let wf = workflow_with_bounce();
        assert_eq!(
            decide_transition(&wf, Column::review(), "nonexistent"),
            TransitionDecision::Reject {
                reason: "unknown-target-status"
            }
        );
    }

    #[test]
    fn decide_transition_any_to_any_when_no_transitions_block() {
        // The default workflow declares NO transitions block, so moves are
        // any-to-any (both statuses just have to be declared). A bounce from
        // review back to in-progress is allowed.
        let wf = default_workflow();
        assert_eq!(
            decide_transition(&wf, Column::review(), "in-progress"),
            TransitionDecision::Apply {
                from_status: "review".into(),
                to_status: "in-progress".into(),
                to_column: Column::in_progress(),
            }
        );
        // The verb sugar works there too.
        assert_eq!(
            decide_transition(&wf, Column::review(), "bounce"),
            TransitionDecision::Apply {
                from_status: "review".into(),
                to_status: "in-progress".into(),
                to_column: Column::in_progress(),
            }
        );
    }

    #[test]
    fn decide_transition_rejects_same_status_noop() {
        // A target resolving to the status the task is already in is a no-op
        // → refused (so we never unassign a task that wouldn't actually
        // change lanes).
        let wf = default_workflow();
        assert_eq!(
            decide_transition(&wf, Column::in_progress(), "in-progress"),
            TransitionDecision::Reject {
                reason: "target-is-current-status"
            }
        );
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
