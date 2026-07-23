//! `shelbi quit` — cleanly tear down the running project's shelbi-owned
//! surfaces from the command line. The teardown counterpart to
//! `shelbi reload`: where reload respawns the orchestrator pane and the
//! shelbi-owned TUI panes (sidebar + tasks/machines) in place, quit closes
//! them and the project's two tmux sessions (`shelbi-<project>` and the
//! hidden `_shelbi-<project>` views stash) and exits.
//!
//! This is the CLI sibling of the palette's "Quit Project" entry
//! ([`super::teardown::quit_project_with_progress`]) and mirrors the
//! in-TUI global-quit (Ctrl+C). It shares the same non-destructive
//! contract as those paths: a workspace mid-task keeps its durable
//! worktree + branch on disk, so quitting only stops the panes — the work
//! resumes on the next launch. When a live workspace still holds an
//! active-category task the user is warned and asked to confirm (bypass
//! with `-y`), since that workspace's agent session is being closed.
//!
//! Before the orchestrator pane is torn down it is given the same handoff
//! courtesy as reload: it may write `agents/orchestrator/handoff.md`, which
//! the next launch ingests-and-deletes so a relaunch resumes with context.
//! The handoff module sweeps any stale file before requesting a fresh write
//! and only reports `Written` on success, so quit never leaves a stale
//! handoff a future reload would mis-ingest.

use std::io::{self, IsTerminal, Write};

use anyhow::Result;

use shelbi_core::MachineKind;
use shelbi_orchestrator::handoff::HandoffOutcome;
use shelbi_orchestrator::workspace as orch_workspace;

use super::quit_project::{list_active_workspaces, ActiveWorkspace};
use super::require_project;

/// Tear down the resolved project's shelbi-owned surfaces. Idempotent: a
/// project with no live sessions is a clean no-op.
pub fn run(project_opt: Option<String>, yes: bool) -> Result<()> {
    let project_name = require_project(project_opt)?;

    // Idempotency: with neither the dashboard nor the views stash live there
    // is nothing to tear down. Checking both (rather than only the main
    // session) also mops up a partially-torn-down project whose stash
    // lingered — teardown below is idempotent, so re-running is always safe.
    let main = format!("shelbi-{project_name}");
    let stash = format!("_shelbi-{project_name}");
    if !session_exists(&main) && !session_exists(&stash) {
        println!("shelbi: project '{project_name}' is not running — nothing to quit.");
        return Ok(());
    }

    // Warn + confirm when a live workspace pane holds an active-category
    // task: quitting closes the pane its agent is running in. The worktree
    // and branch survive (work resumes next launch), so this is
    // non-destructive — but the running agent session is stopped, so the
    // user opts in unless they passed `-y`.
    let active = list_active_workspaces(&project_name);
    let busy = busy_workspaces(&active);
    if !busy.is_empty() && !yes && !confirm_teardown(&project_name, &busy)? {
        println!("shelbi: quit aborted — nothing was torn down.");
        return Ok(());
    }

    // Give the live orchestrator the chance to write its handoff before its
    // pane dies, exactly as reload does.
    match shelbi_orchestrator::handoff::request_orchestrator_handoff(&project_name) {
        Ok(outcome) => print_handoff(&outcome),
        Err(e) => eprintln!("shelbi: warning: handoff request failed: {e}"),
    }

    teardown_workspaces_and_stash(&project_name);

    // Record the close and tell the user before the final self-kill: if
    // `shelbi quit` was itself run from inside the dashboard session, killing
    // it below SIGHUPs this process, so everything user-visible and durable
    // must already be flushed by the time that kill fires.
    let _ = shelbi_state::append_project_event(&project_name, "closed", "user:quit-cli");
    println!(
        "shelbi: quit \"{project_name}\" — orchestrator + TUI panes closed and both tmux \
         sessions torn down; worktrees and branches left intact."
    );
    let _ = io::stdout().flush();

    kill_session_quiet(&main);
    Ok(())
}

/// The active-category workspaces from `list_active_workspaces` — those
/// whose live pane is assigned an in-progress (active) task rather than
/// sitting idle. Pure so the confirmation trigger is unit-testable without
/// standing up tmux + a project fixture.
fn busy_workspaces(active: &[ActiveWorkspace]) -> Vec<&ActiveWorkspace> {
    active.iter().filter(|w| w.task != "idle").collect()
}

/// Prompt on stderr for confirmation before closing panes that host a
/// running agent. Returns whether to proceed. A non-interactive invocation
/// (piped stdin) can't answer, so it declines with a hint to pass `-y` —
/// the safe default is to leave the running agents alone.
fn confirm_teardown(project: &str, busy: &[&ActiveWorkspace]) -> Result<bool> {
    eprintln!(
        "shelbi: {} workspace{} in project '{project}' still hold an active task:",
        busy.len(),
        if busy.len() == 1 { "" } else { "s" }
    );
    for w in busy {
        eprintln!("  · {} ({}) — task {}", w.name, w.state, w.task);
    }
    eprintln!(
        "Quitting closes their panes. Worktrees and branches are left intact, so the work \
         resumes on next launch — but the running agent sessions will be stopped."
    );

    if !io::stdin().is_terminal() {
        eprintln!("shelbi: refusing to close active workspaces non-interactively; re-run with -y to confirm.");
        return Ok(false);
    }

    eprint!("Proceed? [y/N] ");
    let _ = io::stderr().flush();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return Ok(false);
    }
    Ok(is_affirmative(&input))
}

/// Whether a prompt answer means "yes". Trimmed and case-insensitive;
/// anything but `y`/`yes` (including an empty EOF read) is a "no" so the
/// destructive path is never entered by accident.
fn is_affirmative(answer: &str) -> bool {
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Kill every declared workspace pane plus the hidden `_shelbi-<project>`
/// views stash, and remove the Shelbi-managed commit guard from the hub
/// checkout. The main dashboard session is left for the caller to kill last
/// (see [`run`]). Mirrors the palette teardown's
/// [`super::teardown`] step order minus the progress UI.
fn teardown_workspaces_and_stash(project: &str) {
    // Clear any zen crash-heartbeat so the next launch doesn't mistake this
    // clean shutdown for an orchestrator that died mid-flight.
    let _ = shelbi_state::zen_clear_crash(project);

    if let Ok(p) = shelbi_state::load_project(project) {
        // Local workspace panes die with the dashboard session below, but a
        // remote workspace lives in its own tmux session on another machine
        // and must be killed explicitly. Best-effort per workspace: an
        // unresolved machine/addr or an unreachable host is skipped rather
        // than blocking the rest of the teardown.
        for workspace in &p.workspaces {
            let Some(machine) = p.machine(&workspace.machine) else {
                continue;
            };
            let host = machine.host();
            let Ok(addr) = orch_workspace::workspace_tmux_addr(&p, workspace) else {
                continue;
            };
            let _ = orch_workspace::kill_workspace_pane(&host, &addr, &workspace.name);
        }

        // Nothing Shelbi installed should linger past teardown — drop the
        // context-scoped commit guard from the hub checkout. Best-effort, and
        // a user-authored hook is never touched.
        if let Some(hub) = p.machines.iter().find(|m| matches!(m.kind, MachineKind::Local)) {
            let _ = shelbi_orchestrator::githook::uninstall_hub_branch_guard(&hub.work_dir);
        }
    }

    // Kill the hidden views stash. It never hosts the invoking shell, so this
    // is safe in the foreground; the main session is killed last by `run`.
    kill_session_quiet(&format!("_shelbi-{project}"));
}

/// Print the handoff outcome as one status line. Mirrors reload's reporting
/// so the two teardown/respawn paths read consistently. Every variant is
/// "okay to proceed" — only the wording differs.
fn print_handoff(outcome: &HandoffOutcome) {
    match outcome {
        HandoffOutcome::NativeThread => {
            println!("  · handoff  skipped (Codex native thread retained)");
        }
        HandoffOutcome::Written { path } => {
            println!("  ✓ handoff  captured ({})", path.display());
        }
        HandoffOutcome::PaneNotAlive => {
            println!("  · handoff  skipped (orchestrator pane not running)");
        }
        HandoffOutcome::Timeout => {
            println!("  ⚠ handoff  timed out; next launch starts cold");
        }
        HandoffOutcome::SendFailed { reason } => {
            println!("  ⚠ handoff  couldn't ask the orchestrator: {reason}");
        }
        HandoffOutcome::SubmitUnconfirmed { detail } => {
            println!(
                "  ⚠ handoff  delivered but not confirmed submitted ({detail}); \
                 next launch may be cold"
            );
        }
    }
}

/// True when a tmux session named `session` currently exists. Any failure
/// (tmux absent, server down, non-zero exit) reads as "not running" — the
/// idempotent no-op path.
fn session_exists(session: &str) -> bool {
    std::process::Command::new("tmux")
        .args(["has-session", "-t", session])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Kill a tmux session, swallowing output. Idempotent — a raced or absent
/// target exits non-zero but there is nothing actionable to report, so
/// stderr is silenced (unlike [`super::run_tmux`], which surfaces failures).
fn kill_session_quiet(session: &str) {
    let _ = std::process::Command::new("tmux")
        .args(["kill-session", "-t", session])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(name: &str, state: &'static str, task: &str) -> ActiveWorkspace {
        ActiveWorkspace {
            name: name.to_string(),
            state,
            task: task.to_string(),
        }
    }

    #[test]
    fn busy_workspaces_selects_only_those_with_an_active_task() {
        let active = vec![
            ws("alpha", "working", "task-1"),
            ws("bravo", "idle", "idle"),
            ws("charlie", "awaiting input", "task-2, task-3"),
        ];
        let busy = busy_workspaces(&active);
        let names: Vec<&str> = busy.iter().map(|w| w.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "charlie"]);
    }

    #[test]
    fn busy_workspaces_is_empty_when_every_workspace_is_idle() {
        let active = vec![ws("alpha", "idle", "idle"), ws("bravo", "idle", "idle")];
        assert!(busy_workspaces(&active).is_empty());
    }

    #[test]
    fn busy_workspaces_is_empty_for_no_active_workspaces() {
        assert!(busy_workspaces(&[]).is_empty());
    }

    #[test]
    fn affirmative_accepts_only_y_and_yes_case_insensitively() {
        for ok in ["y", "Y", "yes", "YES", "  yes  ", "Yes\n"] {
            assert!(is_affirmative(ok), "{ok:?} should be affirmative");
        }
    }

    #[test]
    fn affirmative_rejects_everything_else_including_empty() {
        // An empty read (EOF on a piped stdin) must decline so the
        // destructive path is never entered by accident.
        for no in ["", "\n", "n", "no", "nope", "sure", "ya", "1"] {
            assert!(!is_affirmative(no), "{no:?} should not be affirmative");
        }
    }
}
