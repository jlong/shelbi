//! `shelbi open <name>` — ensure a workspace's session is up.
//!
//! The single-process TUI switches its terminal view to a workspace in-process;
//! this command's job is to make sure the workspace has a live session to show.
//! For a workspace mid-task the dispatch path already owns its agent session, so
//! this is a no-op. For an IDLE workspace it opens a plain interactive login
//! shell in the worktree ([`orch_workspace::open_user_shell`]), marked as
//! user-occupied so dispatch skips the slot while it's open.

use anyhow::{anyhow, Result};

use shelbi_orchestrator::workspace as orch_workspace;

use super::require_project;

pub fn run(project_opt: Option<String>, name: String) -> Result<()> {
    let project = require_project(project_opt)?;
    open(&project, &name)
}

fn open(project: &str, name: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let workspace = p
        .workspace(name)
        .ok_or_else(|| {
            anyhow!(
                "workspace `{name}` not declared in project `{project}` (known: {})",
                p.workspaces
                    .iter()
                    .map(|w| w.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
        .clone();

    // A workspace with a task assigned is driven by the dispatch path, which
    // owns its agent session — don't open a user shell over it (that would mark
    // the slot user-occupied and make dispatch skip it). An idle workspace gets
    // a plain shell in its worktree so the TUI has a session to show.
    if workspace_has_assigned_task(project, name) {
        return Ok(());
    }
    orch_workspace::open_user_shell(&p, &workspace).map_err(|e| anyhow!(e))
}

/// Whether any open board task is assigned to `workspace`.
fn workspace_has_assigned_task(project: &str, workspace: &str) -> bool {
    super::read_open_board_for_cli(project)
        .map(|board| {
            board
                .iter()
                .any(|f| f.task.assigned_to.as_deref() == Some(workspace))
        })
        .unwrap_or(false)
}
