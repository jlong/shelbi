//! Tear down every Shelbi-owned tmux session on the host — every
//! `shelbi-<project>` and every `_shelbi-<project>` stash, plus the
//! per-workspace panes (local windows + remote sessions). The local
//! tmux kills happen via `tmux run-shell -b` so they survive the
//! popup process dying mid-teardown — see [`run`] for the rationale.
//!
//! Invoked from the palette's "Quit Shelbi" entry after the
//! confirmation popover. Sibling to `quit_project` — that closes one
//! project, this closes all of them.
//!
//! Like `quit_project`, this writes no task or worktree state; the
//! agents on disk are already authoritative, and any in-flight workspace
//! changes stay in their worktrees for the user to pick up later.

use std::path::Path;

use anyhow::Result;

use shelbi_core::Host;
use shelbi_orchestrator::workspace as orch_workspace;

/// One project the host is currently managing — has a live
/// `shelbi-<name>` tmux session. Used by the confirmation popover so
/// the user can see exactly what's about to be torn down.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedProject {
    pub name: String,
    /// Workspaces whose tmux pane is currently live. Zero is rendered as
    /// "no active workspaces" in the popover; the project still appears
    /// because the main session is open.
    pub active_workspaces: usize,
    /// Names of active workspaces whose worktree has uncommitted
    /// user-authored changes. Surfaces in the popover as an explicit
    /// warning so the destructive confirm isn't a silent path past
    /// in-flight work. The worktree itself survives the quit — only
    /// the agent's running session is killed — but the warning prompts
    /// the user to inspect / commit before tearing down.
    pub dirty_workspaces: Vec<String>,
}

/// Enumerate every project with a live `shelbi-<name>` session,
/// decorated with a count of its currently-live workspace panes and
/// the names of any whose worktree has uncommitted changes. Sorted
/// alphabetically so the popover order is stable across runs.
///
/// Best-effort: a project whose YAML fails to load shows up with
/// `active_workspaces = 0` rather than being dropped — the session is
/// still real and still needs to be killed.
pub fn list_managed_projects() -> Vec<ManagedProject> {
    let listing = list_sessions_listing();
    let mut out: Vec<ManagedProject> = shelbi_project_session_names(&listing)
        .map(|name| match shelbi_state::load_project(&name) {
            Ok(p) => {
                let scan = scan_workspaces(&p);
                ManagedProject {
                    name,
                    active_workspaces: scan.active,
                    dirty_workspaces: scan.dirty,
                }
            }
            Err(_) => ManagedProject {
                name,
                active_workspaces: 0,
                dirty_workspaces: Vec::new(),
            },
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// Per-project snapshot returned by [`scan_workspaces`].
struct WorkspaceScan {
    active: usize,
    dirty: Vec<String>,
}

fn scan_workspaces(project: &shelbi_core::Project) -> WorkspaceScan {
    let mut active = 0;
    let mut dirty = Vec::new();
    for workspace in &project.workspaces {
        let Some(machine) = project.machine(&workspace.machine) else {
            continue;
        };
        let host = machine.host();
        let Ok(addr) = orch_workspace::workspace_tmux_addr(project, workspace) else {
            continue;
        };
        if !orch_workspace::workspace_pane_alive(&host, &addr).unwrap_or(false) {
            continue;
        }
        active += 1;
        let worktree = orch_workspace::workspace_worktree(machine, workspace);
        if worktree_is_dirty(&host, &worktree).unwrap_or(false) {
            dirty.push(workspace.name.clone());
        }
    }
    WorkspaceScan { active, dirty }
}

/// True when `worktree` has user-authored uncommitted changes — anything
/// outside `.claude/`, which is shelbi's overwrite-on-deploy footprint
/// (settings, agent prompt, review marker) and intentionally excluded.
/// Mirrors the carve-out in `rebase_workspace_branch_onto_default`.
///
/// Returns `Ok(false)` when the host is unreachable, the path isn't a
/// git worktree, or `git status` errors — the warning is best-effort
/// and never blocks the quit. The worst case is we skip the warning;
/// teardown still runs.
fn worktree_is_dirty(host: &Host, worktree: &Path) -> shelbi_core::Result<bool> {
    let wt_str = worktree.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["git", "-C", &wt_str, "status", "--porcelain"])
        .map_err(shelbi_core::Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(any_user_dirty_line(&stdout))
}

/// True when at least one line of `git status --porcelain` output names a
/// path the user wrote. The `XY ` prefix is fixed-width, so anything from
/// offset 3 onward is the path; `.claude/` is shelbi's overwrite-on-deploy
/// footprint and excluded so a fresh-deploy settings rewrite doesn't dirty
/// every workspace in the popover.
fn any_user_dirty_line(porcelain: &str) -> bool {
    porcelain.lines().any(|line| {
        let path = line.get(3..).unwrap_or("");
        !(path.starts_with(".claude/") || path == ".claude")
    })
}

/// Tear down every shelbi tmux session on this host.
///
/// First, fan out: ask every live orchestrator pane to write its
/// `agents/orchestrator/handoff.md` in parallel. Each request runs in
/// its own thread (capped at 30s by
/// [`shelbi_orchestrator::handoff::request_orchestrator_handoff`]) so a
/// multi-project quit doesn't serialize the per-project wait. Files
/// persist between quit and the next launch; the next instance
/// ingests + deletes them.
///
/// Synchronously: per project, kill workspace panes (local windows +
/// remote `shelbi-w-*` sessions), clear the zen crash heartbeat so the
/// next launch doesn't misread the quit as a crash, and append a
/// `closed reason=user:quit-shelbi` row to events.log. These steps need
/// shelbi state (project YAML, events.log) and are too involved to
/// ship to a server-side shell snippet.
///
/// Then asynchronously (via `tmux run-shell -b`): kill every
/// `_shelbi-<name>` stash and every `shelbi-<name>` main session, then
/// `detach-client` as a flush for clients that were attached to a
/// non-shelbi session.
///
/// The async hop matters: this function runs inside the popup process
/// spawned by `tmux display-popup -E shelbi __palette …`. The popup's
/// pane lives inside whichever `shelbi-<name>` session the user
/// summoned the palette from — so killing that session synchronously
/// would send SIGHUP to this process before the rest of the teardown
/// loop could run, leaving the main session alive and the user staring
/// at a frozen popup. Handing the kills to `tmux run-shell -b` forks
/// them inside the tmux server process, which is owned by launchd /
/// the user's shell — independent of any pane or client lifecycle.
///
/// Idempotent throughout — `kill-session` on an absent target is a
/// no-op, and best-effort tmux/SSH errors don't abort the rest of
/// the teardown.
pub fn run() -> Result<()> {
    let listing = list_sessions_listing();
    let names: Vec<String> = shelbi_project_session_names(&listing).collect();

    // Fan out the handoff requests so multiple projects' orchestrators
    // can write their handoff in parallel. Each thread is bounded by
    // the 30s timeout inside `request_orchestrator_handoff`, so the
    // worst-case wait is ~30s regardless of how many projects are
    // live. Best-effort — every variant of the outcome is "okay to
    // proceed" and we don't surface it to the user here.
    let handoff_threads: Vec<_> = names
        .iter()
        .cloned()
        .map(|name| {
            std::thread::spawn(move || {
                let _ = shelbi_orchestrator::handoff::request_orchestrator_handoff(&name);
            })
        })
        .collect();
    for t in handoff_threads {
        let _ = t.join();
    }

    for name in &names {
        let _ = shelbi_state::zen_clear_crash(name);
        if let Ok(p) = shelbi_state::load_project(name) {
            for workspace in &p.workspaces {
                let Some(machine) = p.machine(&workspace.machine) else {
                    continue;
                };
                let host = machine.host();
                let Ok(addr) = orch_workspace::workspace_tmux_addr(&p, workspace) else {
                    continue;
                };
                // Remote workspace kills happen over SSH; an unreachable
                // host returns Err here. Swallow so one unreachable
                // machine doesn't block the rest of the teardown.
                let _ = orch_workspace::kill_workspace_pane(&host, &addr, &workspace.name);
            }
        }
        let _ = shelbi_state::append_project_event(name, "closed", "user:quit-shelbi");
    }

    let script = build_local_teardown_script(&names);
    if !script.is_empty() {
        let _ = super::run_tmux(["run-shell", "-b", &script]);
    }

    Ok(())
}

/// Build the shell snippet that tears down the hub's tmux sessions:
/// every `_shelbi-<name>` stash, then every `shelbi-<name>` main
/// session, then a `detach-client` flush. Returns an empty string
/// when there's nothing to tear down so the caller can skip
/// `run-shell` entirely.
///
/// Stderr is silenced (`2>/dev/null`) because `kill-session` on a
/// session that's already gone — possible if `quit_project` raced —
/// prints to stderr and would clutter tmux's job log without
/// signalling anything actionable.
fn build_local_teardown_script(names: &[String]) -> String {
    if names.is_empty() {
        return String::new();
    }
    let mut s = String::new();
    for name in names {
        s.push_str(&format!(
            "tmux kill-session -t _shelbi-{name} 2>/dev/null; "
        ));
    }
    for name in names {
        s.push_str(&format!("tmux kill-session -t shelbi-{name} 2>/dev/null; "));
    }
    // Belt-and-braces: most clients detach automatically when their
    // session is killed above, but a client attached to a non-shelbi
    // session (e.g. a user who summoned the palette via the global
    // Ctrl+P chord from a sibling session) won't, and we'd otherwise
    // leave them staring at the popup's now-orphan surface.
    s.push_str("tmux detach-client 2>/dev/null; ");
    s.push_str("true");
    s
}

/// `tmux list-sessions` output, one session per line. Empty string
/// if tmux isn't reachable — callers then treat that as "nothing to
/// quit" and the run is a no-op.
fn list_sessions_listing() -> String {
    std::process::Command::new("tmux")
        .args(["list-sessions", "-F", "#{session_name}"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Extract project names from session names of the form
/// `shelbi-<name>`. The hidden `_shelbi-<name>` stash sessions are
/// skipped automatically — their prefix is `_shelbi-`, not `shelbi-`.
fn shelbi_project_session_names(listing: &str) -> impl Iterator<Item = String> + '_ {
    listing.lines().filter_map(|line| {
        let name = line.trim();
        let rest = name.strip_prefix("shelbi-")?;
        if rest.is_empty() {
            None
        } else {
            Some(rest.to_string())
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_names_strip_shelbi_prefix() {
        let listing = "\
shelbi-alpha
shelbi-bravo
";
        let names: Vec<_> = shelbi_project_session_names(listing).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }

    #[test]
    fn project_names_skip_stash_and_non_shelbi_sessions() {
        let listing = "\
shelbi-alpha
_shelbi-alpha
_shelbi-bravo
plain-session
shelbi-bravo
";
        let names: Vec<_> = shelbi_project_session_names(listing).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }

    #[test]
    fn project_names_tolerate_blank_lines_and_whitespace() {
        let listing = "\
shelbi-alpha

   shelbi-bravo
shelbi-
";
        // The "shelbi-" line with no project name is dropped — strip
        // returns an empty string and the filter excludes it.
        let names: Vec<_> = shelbi_project_session_names(listing).collect();
        assert_eq!(names, vec!["alpha", "bravo"]);
    }

    #[test]
    fn project_names_returns_nothing_for_empty_listing() {
        let names: Vec<_> = shelbi_project_session_names("").collect();
        assert!(names.is_empty());
    }

    #[test]
    fn teardown_script_kills_every_project_main_session() {
        // The bug we're guarding against: the original code ran
        // `kill-session -t shelbi-<name>` after `detach-client`, so
        // the popup process died before the kill could run and the
        // main session leaked. The script must kill every project's
        // main session — that's the test.
        let script = build_local_teardown_script(&["alpha".to_string(), "bravo".to_string()]);
        assert!(script.contains("tmux kill-session -t shelbi-alpha"));
        assert!(script.contains("tmux kill-session -t shelbi-bravo"));
    }

    #[test]
    fn teardown_script_kills_stash_sessions_before_main_sessions() {
        // The session-closed hook on the main session is what triggers
        // the stash cleanup on a normal exit, but here we're killing
        // both manually so the order matters only for log/timing
        // determinism. Lock in `_shelbi-*` first so a future reorder
        // shows up as a test failure rather than silent flakiness.
        let script = build_local_teardown_script(&["alpha".to_string()]);
        let stash_pos = script.find("kill-session -t _shelbi-alpha").unwrap();
        let main_pos = script.find("kill-session -t shelbi-alpha").unwrap();
        assert!(
            stash_pos < main_pos,
            "_shelbi-* must be killed before shelbi-* (script={script:?})"
        );
    }

    #[test]
    fn teardown_script_runs_detach_client_after_kills_as_a_flush() {
        // detach-client is a safety net for clients attached to a
        // non-shelbi session that wouldn't auto-detach when the
        // shelbi sessions die. Belongs after the kills so any client
        // whose session was killed is already gone before detach
        // runs (and so detach can't disconnect a still-needed client).
        let script = build_local_teardown_script(&["alpha".to_string()]);
        let kill_pos = script.find("kill-session -t shelbi-alpha").unwrap();
        let detach_pos = script.find("detach-client").unwrap();
        assert!(
            kill_pos < detach_pos,
            "detach-client must come after kill-session (script={script:?})"
        );
    }

    #[test]
    fn teardown_script_silences_kill_session_stderr() {
        // kill-session on an absent target writes a noisy error to
        // stderr that tmux's run-shell job log would otherwise
        // surface; absent targets are normal here (a stash may have
        // already been cleaned by the session-closed hook).
        let script = build_local_teardown_script(&["alpha".to_string()]);
        assert!(
            script.contains("kill-session -t _shelbi-alpha 2>/dev/null"),
            "kill-session calls must redirect stderr (script={script:?})"
        );
        assert!(
            script.contains("kill-session -t shelbi-alpha 2>/dev/null"),
            "kill-session calls must redirect stderr (script={script:?})"
        );
    }

    #[test]
    fn teardown_script_ends_with_true_so_nonzero_exits_dont_propagate() {
        // tmux's run-shell logs the script's exit status; trailing
        // `true` guarantees a clean zero exit even when every kill
        // raced something else and returned non-zero.
        let script = build_local_teardown_script(&["alpha".to_string()]);
        assert!(
            script.trim_end().ends_with("true"),
            "script must end with `true` (script={script:?})"
        );
    }

    #[test]
    fn teardown_script_is_empty_when_no_projects() {
        // Caller uses the empty string as the signal to skip
        // `run-shell` entirely — don't paper that over with a
        // bare `true` snippet.
        assert!(build_local_teardown_script(&[]).is_empty());
    }

    #[test]
    fn dirty_check_ignores_shelbi_deploy_footprint() {
        // `.claude/settings.json` and `.claude/agent-instructions.md`
        // are shelbi's overwrite-on-deploy footprint and gitignored
        // in well-formed worktrees, but a user who hasn't yet pulled
        // the gitignore can have them appear in `git status`. They
        // are NOT user-authored work — quitting won't lose them — so
        // they must not trigger the popover warning.
        let porcelain = " M .claude/settings.json\n?? .claude/agent-instructions.md\n";
        assert!(!any_user_dirty_line(porcelain));
    }

    #[test]
    fn dirty_check_flags_user_authored_changes() {
        let porcelain = " M src/lib.rs\n?? notes/scratch.md\n";
        assert!(any_user_dirty_line(porcelain));
    }

    #[test]
    fn dirty_check_returns_false_for_clean_worktree() {
        assert!(!any_user_dirty_line(""));
    }

    #[test]
    fn dirty_check_handles_mixed_user_and_deploy_paths() {
        // One real edit beside a deploy footprint is still dirty.
        let porcelain = " M src/lib.rs\n M .claude/settings.json\n";
        assert!(any_user_dirty_line(porcelain));
    }

    #[test]
    fn worktree_is_dirty_returns_false_when_path_is_not_a_git_repo() {
        // Acceptance criterion: an unreachable / non-existent worktree
        // must NOT block the quit. `git -C <missing> status` exits
        // non-zero; the helper swallows the failure and reports
        // "not dirty" so the warning is omitted but teardown runs.
        let nowhere = std::path::Path::new("/var/empty/shelbi-test-does-not-exist");
        let dirty = worktree_is_dirty(&Host::Local, nowhere).unwrap();
        assert!(
            !dirty,
            "missing worktree must not be reported as dirty (would scare the user away from a safe quit)"
        );
    }

    #[test]
    fn run_with_no_shelbi_sessions_is_a_clean_noop() {
        // Smoke test for the "nothing to do" branch — no `tmux ls` on
        // the host shows shelbi-* sessions, so the loop body never
        // runs, the script is empty, and we don't shell out to
        // `tmux run-shell`. Important: this is the path taken when the
        // user re-runs `shelbi` immediately after a quit; the
        // re-launch must not error on the empty teardown.
        //
        // We can't easily intercept the live `tmux list-sessions`, but
        // the empty-listing path is the only one that doesn't shell
        // out, so feed it directly and assert the derived script is
        // empty.
        let names: Vec<String> = shelbi_project_session_names("").collect();
        assert!(names.is_empty());
        assert!(build_local_teardown_script(&names).is_empty());
    }
}
