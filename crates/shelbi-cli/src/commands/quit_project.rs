//! Tear down every tmux session belonging to a project, then switch the
//! attached client to whatever shelbi project session is next-most-recent.
//!
//! Invoked from the palette's "Quit Project" entry. Replaces the older
//! plain-`kill-session` flow, which left remote worker sessions orphaned
//! (they live on each worker's machine — the local `session-closed` hook
//! only catches the local stash) and dropped the user wherever tmux
//! happened to switch by default.

use anyhow::{anyhow, Result};

use shelbi_core::Column;
use shelbi_orchestrator::worker as orch_worker;
use shelbi_state::WorkerState;

/// One declared worker whose tmux pane is currently live. Surfaced in the
/// palette's quit-project confirmation popover so users see exactly what
/// they're about to tear down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorker {
    pub name: String,
    /// Display label: `"working"`, `"awaiting input"`, `"idle"`, or
    /// `"blocked"`. Derived from the same on-disk signals the sidebar's
    /// per-worker badge reads (in-progress task assignment + status.yaml).
    pub state: &'static str,
    /// Task id the worker is currently assigned to, or `"idle"` if it has
    /// no in-progress card. Multiple ids get comma-joined — the worker
    /// pool should never carry more than one but if it does, the popover
    /// should expose it rather than hide it.
    pub task: String,
}

/// Quit `project`:
///
/// 1. Kill every worker pane (local windows + remote sessions). The user
///    is closing the whole project, so we don't try to preserve in-flight
///    task assignments here — the cards stay on the board and get picked
///    up the next time the project's dispatched.
/// 2. Pick the most-recently-attached *other* `shelbi-*` session.
/// 3. `switch-client` to it BEFORE killing the current session — without
///    this the popup's tmux client briefly disconnects, which can flash
///    a bare terminal at the user.
/// 4. Kill the hidden stash session (`_shelbi-<project>`) and then the
///    visible session (`shelbi-<project>`). Both are idempotent and
///    cleared by the local `session-closed` hook anyway, but doing the
///    work explicitly keeps the teardown order deterministic.
/// 5. Append a `project=<name> closed reason=user:quit-project` line to
///    the events log so the activity feed shows the close.
///
/// Before killing anything we also clear `state.json::zen_last_crashed_at`.
/// The orchestrator pane's heartbeat loop writes a fresh timestamp every
/// 60s; tearing the session down via `kill-session` sends SIGHUP to the
/// pane and prevents the wrapper's own `__zen-orch-exit` from running, so
/// without this explicit clear the next start would misread the quit as
/// a crash and auto-disable Zen.
pub fn run(project: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;

    let _ = shelbi_state::zen_clear_crash(project);

    for worker in &p.workers {
        let Some(machine) = p.machine(&worker.machine) else {
            continue;
        };
        let host = machine.host();
        let Ok(addr) = orch_worker::worker_tmux_addr(&p, worker) else {
            continue;
        };
        let _ = orch_worker::kill_worker_pane(&host, &addr);
    }

    let current = format!("shelbi-{project}");
    if let Some(target) = pick_next_session(&list_sessions(), &current) {
        let _ = run_tmux(["switch-client", "-t", &target]);
    }

    let _ = run_tmux(["kill-session", "-t", &format!("_shelbi-{project}")]);
    let _ = run_tmux(["kill-session", "-t", &current]);

    let _ = shelbi_state::append_project_event(project, "closed", "user:quit-project");

    Ok(())
}

/// Enumerate declared workers whose tmux pane is currently live, decorated
/// with their state + current task. Used by the palette's quit-project
/// confirmation popover; the workers themselves are not consulted, the
/// hub-side `worker_pane_alive` check + `status.yaml` snapshot are.
///
/// Best-effort: a missing project YAML returns an empty list (the popover
/// then shows "No active workers."); workers whose machine lookup or tmux
/// addr derivation fails are silently dropped — the same shape as the
/// teardown loop below, so the popover only ever lists things this
/// process knows how to actually kill.
pub fn list_active_workers(project_name: &str) -> Vec<ActiveWorker> {
    let project = match shelbi_state::load_project(project_name) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let in_progress =
        shelbi_state::list_column(project_name, Column::InProgress).unwrap_or_default();

    let mut out = Vec::new();
    for worker in &project.workers {
        let Some(machine) = project.machine(&worker.machine) else {
            continue;
        };
        let host = machine.host();
        let Ok(addr) = orch_worker::worker_tmux_addr(&project, worker) else {
            continue;
        };
        if !orch_worker::worker_pane_alive(&host, &addr).unwrap_or(false) {
            continue;
        }

        let assigned: Vec<&str> = in_progress
            .iter()
            .filter(|tf| tf.task.assigned_to.as_deref() == Some(worker.name.as_str()))
            .map(|tf| tf.task.id.as_str())
            .collect();
        let has_task = !assigned.is_empty();
        let task = if has_task {
            assigned.join(", ")
        } else {
            "idle".to_string()
        };

        let status_state = shelbi_state::load_worker_status(&worker.name)
            .ok()
            .flatten()
            .map(|s| s.state);
        out.push(ActiveWorker {
            name: worker.name.clone(),
            state: worker_state_label(has_task, status_state),
            task,
        });
    }
    out
}

/// Pick the popover's state label for one worker. Pure so the mapping is
/// testable without standing up tmux + an HOME fixture.
///
/// - No in-progress card → `"idle"`. The status.yaml may still claim
///   `working` from a previous turn, but with no assigned task the
///   pane is idle by definition.
/// - In-progress card and a status snapshot → mirror the snapshot.
/// - In-progress card with no snapshot → `"working"`. The poller hasn't
///   observed a marker yet; default the optimistic side rather than
///   showing nothing — matches the sidebar's badge fallback.
fn worker_state_label(has_task: bool, status: Option<WorkerState>) -> &'static str {
    if !has_task {
        return "idle";
    }
    match status {
        Some(WorkerState::Working) => "working",
        Some(WorkerState::AwaitingInput) => "awaiting input",
        Some(WorkerState::Blocked) => "blocked",
        None => "working",
    }
}

/// `tmux list-sessions` output, one line per session, formatted
/// `<name> <last_attached>`. `last_attached` is unix seconds; 0 if the
/// session has never been attached. Returns an empty string if tmux
/// isn't reachable — callers treat that as "no other sessions".
fn list_sessions() -> String {
    std::process::Command::new("tmux")
        .args([
            "list-sessions",
            "-F",
            "#{session_name} #{session_last_attached}",
        ])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// From `tmux list-sessions` output, pick the most-recently-attached
/// session whose name starts with `shelbi-` and isn't `current`. The
/// `_shelbi-*` stash sessions are excluded automatically — their prefix
/// is `_shelbi-`, not `shelbi-`.
fn pick_next_session(listing: &str, current: &str) -> Option<String> {
    let mut best: Option<(String, u64)> = None;
    for line in listing.lines() {
        let mut parts = line.splitn(2, ' ');
        let name = parts.next().unwrap_or("").trim();
        let ts = parts.next().unwrap_or("").trim().parse::<u64>().unwrap_or(0);
        if name.is_empty() || name == current || !name.starts_with("shelbi-") {
            continue;
        }
        match &best {
            Some((_, best_ts)) if *best_ts >= ts => {}
            _ => best = Some((name.to_string(), ts)),
        }
    }
    best.map(|(name, _)| name)
}

fn run_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_most_recently_attached_other_shelbi_session() {
        let listing = "\
shelbi-alpha 1000
shelbi-bravo 3000
shelbi-charlie 2000
";
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }

    #[test]
    fn skips_current_and_stash_and_non_shelbi() {
        let listing = "\
shelbi-alpha 5000
_shelbi-alpha 9999
_shelbi-bravo 9999
plain-session 9999
shelbi-bravo 4000
";
        // Current is alpha. Bravo wins; both _shelbi stashes and plain
        // are excluded (stash by prefix, plain by missing shelbi- prefix).
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }

    #[test]
    fn returns_none_when_only_current_exists() {
        let listing = "shelbi-alpha 1000\n";
        assert!(pick_next_session(listing, "shelbi-alpha").is_none());
    }

    #[test]
    fn returns_none_when_listing_is_empty() {
        assert!(pick_next_session("", "shelbi-alpha").is_none());
    }

    #[test]
    fn never_attached_session_with_zero_timestamp_is_eligible() {
        // A freshly-bootstrapped session has last_attached=0 and should
        // still be a valid landing target — better than detaching the
        // client outright.
        let listing = "\
shelbi-alpha 1000
shelbi-bravo 0
";
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }

    #[test]
    fn state_label_falls_back_to_idle_when_no_task_assigned() {
        // No in-progress card on the board — the worker is idle even if
        // the status.yaml still says "working" from a previous turn.
        assert_eq!(worker_state_label(false, None), "idle");
        assert_eq!(worker_state_label(false, Some(WorkerState::Working)), "idle");
        assert_eq!(
            worker_state_label(false, Some(WorkerState::AwaitingInput)),
            "idle"
        );
        assert_eq!(worker_state_label(false, Some(WorkerState::Blocked)), "idle");
    }

    #[test]
    fn state_label_mirrors_status_when_task_assigned() {
        assert_eq!(
            worker_state_label(true, Some(WorkerState::Working)),
            "working"
        );
        assert_eq!(
            worker_state_label(true, Some(WorkerState::AwaitingInput)),
            "awaiting input"
        );
        assert_eq!(
            worker_state_label(true, Some(WorkerState::Blocked)),
            "blocked"
        );
    }

    #[test]
    fn state_label_defaults_to_working_when_task_assigned_but_no_snapshot() {
        // Sidebar uses the same optimistic fallback — the poller hasn't
        // observed a marker yet, so show `working` rather than dropping
        // the row or guessing idle.
        assert_eq!(worker_state_label(true, None), "working");
    }

    #[test]
    fn tolerates_malformed_lines() {
        let listing = "\
shelbi-alpha not-a-number
shelbi-bravo 500
\n\
shelbi-charlie
";
        // alpha parses to ts=0; bravo wins with ts=500; charlie's
        // missing field parses to 0.
        assert_eq!(
            pick_next_session(listing, "shelbi-alpha").as_deref(),
            Some("shelbi-bravo")
        );
    }
}
