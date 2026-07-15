//! Tear down every tmux session belonging to a project, then switch the
//! attached client to whatever shelbi project session is next-most-recent.
//!
//! Invoked from the palette's "Quit Project" entry. Replaces the older
//! plain-`kill-session` flow, which left remote workspace sessions orphaned
//! (they live on each workspace's machine — the local `session-closed` hook
//! only catches the local stash) and dropped the user wherever tmux
//! happened to switch by default.

use shelbi_core::Column;
use shelbi_orchestrator::workspace as orch_workspace;
use shelbi_state::WorkspaceState;

/// One declared workspace whose tmux pane is currently live. Surfaced in the
/// palette's quit-project confirmation popover so users see exactly what
/// they're about to tear down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveWorkspace {
    pub name: String,
    /// Display label: `"working"`, `"awaiting input"`, `"idle"`, or
    /// `"blocked"`. Derived from the same on-disk signals the sidebar's
    /// per-workspace badge reads (in-progress task assignment + status.yaml).
    pub state: &'static str,
    /// Task id the workspace is currently assigned to, or `"idle"` if it has
    /// no in-progress card. Multiple ids get comma-joined — the workspace
    /// pool should never carry more than one but if it does, the popover
    /// should expose it rather than hide it.
    pub task: String,
}

/// The most-recently-attached *other* `shelbi-*` session the attached
/// client should land on once `project` is torn down, or `None` when this
/// is the only shelbi project open. The progress path `switch-client`s to
/// it BEFORE the `shelbi-<project>` self-kill fires — without this the
/// popup's tmux client briefly disconnects, which can flash a bare
/// terminal at the user.
pub(crate) fn next_session_target(project: &str) -> Option<String> {
    let current = format!("shelbi-{project}");
    pick_next_session(&list_sessions(), &current)
}

/// Build the interruption backstop for a single-project quit: kill the
/// hidden stash (`_shelbi-<project>`) and then the visible session
/// (`shelbi-<project>`). The progress path fires this via
/// `tmux run-shell -b` as its last act so the self-kill is forked into the
/// tmux server and survives the popup process dying to its own SIGHUP.
///
/// No `detach-client` here (unlike the whole-host quit): the progress path
/// `switch-client`s the attached client to the next project first, so a
/// blanket detach would bounce the user off the session they just landed
/// on. Stderr is silenced because both kills are idempotent — the stash
/// may already be gone via the foreground pass or the `session-closed`
/// hook, and `kill-session` on an absent target is noisy but harmless.
pub(crate) fn build_project_teardown_script(project: &str) -> String {
    format!(
        "tmux kill-session -t _shelbi-{project} 2>/dev/null; \
         tmux kill-session -t shelbi-{project} 2>/dev/null; true"
    )
}

/// Enumerate declared workspaces whose tmux pane is currently live, decorated
/// with their state + current task. Used by the palette's quit-project
/// confirmation popover; the workspaces themselves are not consulted, the
/// hub-side `workspace_pane_alive` check + `status.yaml` snapshot are.
///
/// Best-effort: a missing project YAML returns an empty list (the popover
/// then shows "No active workspaces."); workspaces whose machine lookup or tmux
/// addr derivation fails are silently dropped — the same shape as the
/// teardown loop below, so the popover only ever lists things this
/// process knows how to actually kill.
pub fn list_active_workspaces(project_name: &str) -> Vec<ActiveWorkspace> {
    let project = match shelbi_state::load_project(project_name) {
        Ok(p) => p,
        Err(_) => return Vec::new(),
    };
    let in_progress =
        shelbi_state::list_column(project_name, Column::in_progress()).unwrap_or_default();

    let mut out = Vec::new();
    for workspace in &project.workspaces {
        let Some(machine) = project.machine(&workspace.machine) else {
            continue;
        };
        let host = machine.host();
        let Ok(addr) = orch_workspace::workspace_tmux_addr(&project, workspace) else {
            continue;
        };
        if !orch_workspace::workspace_pane_alive(&host, &addr).unwrap_or(false) {
            continue;
        }

        let assigned: Vec<&str> = in_progress
            .iter()
            .filter(|tf| tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()))
            .map(|tf| tf.task.id.as_str())
            .collect();
        let has_task = !assigned.is_empty();
        let task = if has_task {
            assigned.join(", ")
        } else {
            "idle".to_string()
        };

        let status_state = shelbi_state::load_workspace_status(&workspace.name)
            .ok()
            .flatten()
            .map(|s| s.state);
        out.push(ActiveWorkspace {
            name: workspace.name.clone(),
            state: workspace_state_label(has_task, status_state),
            task,
        });
    }
    out
}

/// Pick the popover's state label for one workspace. Pure so the mapping is
/// testable without standing up tmux + an HOME fixture.
///
/// - No in-progress card → `"idle"`. The status.yaml may still claim
///   `working` from a previous turn, but with no assigned task the
///   pane is idle by definition.
/// - In-progress card and a status snapshot → mirror the snapshot.
/// - In-progress card with no snapshot → `"working"`. The poller hasn't
///   observed a marker yet; default the optimistic side rather than
///   showing nothing — matches the sidebar's badge fallback.
fn workspace_state_label(has_task: bool, status: Option<WorkspaceState>) -> &'static str {
    if !has_task {
        return "idle";
    }
    match status {
        Some(WorkspaceState::Working) => "working",
        Some(WorkspaceState::AwaitingInput) => "awaiting input",
        Some(WorkspaceState::Blocked) => "blocked",
        Some(WorkspaceState::Paused) => "paused",
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
        let ts = parts
            .next()
            .unwrap_or("")
            .trim()
            .parse::<u64>()
            .unwrap_or(0);
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
        // No in-progress card on the board — the workspace is idle even if
        // the status.yaml still says "working" from a previous turn.
        assert_eq!(workspace_state_label(false, None), "idle");
        assert_eq!(
            workspace_state_label(false, Some(WorkspaceState::Working)),
            "idle"
        );
        assert_eq!(
            workspace_state_label(false, Some(WorkspaceState::AwaitingInput)),
            "idle"
        );
        assert_eq!(
            workspace_state_label(false, Some(WorkspaceState::Blocked)),
            "idle"
        );
    }

    #[test]
    fn state_label_mirrors_status_when_task_assigned() {
        assert_eq!(
            workspace_state_label(true, Some(WorkspaceState::Working)),
            "working"
        );
        assert_eq!(
            workspace_state_label(true, Some(WorkspaceState::AwaitingInput)),
            "awaiting input"
        );
        assert_eq!(
            workspace_state_label(true, Some(WorkspaceState::Blocked)),
            "blocked"
        );
    }

    #[test]
    fn state_label_defaults_to_working_when_task_assigned_but_no_snapshot() {
        // Sidebar uses the same optimistic fallback — the poller hasn't
        // observed a marker yet, so show `working` rather than dropping
        // the row or guessing idle.
        assert_eq!(workspace_state_label(true, None), "working");
    }

    #[test]
    fn teardown_script_kills_stash_then_main_and_ends_with_true() {
        // The backstop must kill `_shelbi-*` before `shelbi-*` (stash first,
        // matching the whole-host quit) and end with `true` so a raced kill
        // returning non-zero doesn't surface in tmux's run-shell job log.
        let script = build_project_teardown_script("alpha");
        let stash_pos = script.find("kill-session -t _shelbi-alpha").unwrap();
        let main_pos = script.find("kill-session -t shelbi-alpha").unwrap();
        assert!(
            stash_pos < main_pos,
            "_shelbi-* must be killed before shelbi-* (script={script:?})"
        );
        assert!(script.trim_end().ends_with("true"), "script={script:?}");
    }

    #[test]
    fn teardown_script_silences_kill_session_stderr() {
        // Both kills are idempotent — the stash may already be gone from the
        // foreground progress pass — so absent-target noise is redirected.
        let script = build_project_teardown_script("alpha");
        assert!(script.contains("kill-session -t _shelbi-alpha 2>/dev/null"));
        assert!(script.contains("kill-session -t shelbi-alpha 2>/dev/null"));
    }

    #[test]
    fn teardown_script_does_not_detach_client() {
        // Unlike the whole-host quit, a single-project quit switches the
        // client to the next project first, so a blanket detach-client would
        // bounce the user off the session they just landed on.
        let script = build_project_teardown_script("alpha");
        assert!(
            !script.contains("detach-client"),
            "single-project teardown must not detach (script={script:?})"
        );
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
