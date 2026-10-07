//! Daemon-side lifecycle of a **dev workspace's content sessions** — the editor
//! and diff tool the single-process TUI's *workspace sidebar* shows in a
//! terminal view (the workspace-sidebar task).
//!
//! This is the dev-workspace twin of [`crate::review_session`]: where that
//! module manages a *review slot's* editor/diff (resolved from a review-column
//! task), this one manages an *arbitrary dev workspace's* editor/diff, keyed by
//! the workspace name directly. Each is a real `shelbi __session` process named
//! `<project>/ws/<workspace>/<role>` that the client attaches a terminal view to
//! (distinct from the workspace's agent session, `<project>/ws/<workspace>`).
//!
//! The **daemon owns their lifetime** (same design as review): the TUI client
//! asks the daemon — over the control socket
//! ([`shelbi_proto::control::ClientMsg::WorkspaceSession`]) — to
//! [`ensure_content_session`] one on demand and to [`close_content`] them on
//! teardown, so they survive a client detach. Unlike review there is no dev
//! server to reap (a dev workspace runs no review server), so close only ends
//! the editor/diff sessions.
//!
//! Local workspaces only: a remote workspace has no local session to spawn, so a
//! remote workspace here is a clear `Err` the client surfaces on its status line.

use shelbi_core::{Column, Error, Host, Project, Result};

use crate::review_session::ReviewContentRole;
use crate::session_process_backend::SessionProcessBackend;

/// The session name a dev workspace's `role` content session is spawned and
/// looked up under: `<project>/ws/<workspace>/<role>`. Pure, so the daemon's
/// spawn and the client's attach agree without a shared helper crossing the
/// wire.
pub fn content_session_name(project_name: &str, workspace: &str, role: ReviewContentRole) -> String {
    format!("{project_name}/ws/{workspace}/{}", role.as_str())
}

/// Everything the single-process TUI needs to build its workspace panel for a
/// workspace, resolved from durable state in one place.
#[derive(Debug, Clone)]
pub struct WorkspaceOpenInfo {
    /// Absolute workspace worktree path (shown truncated, revealed on click).
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …).
    pub editor_name: String,
    /// The in-progress task on this workspace, for the panel's status / title /
    /// description preview and the full-description popover. `None` when the
    /// workspace has no in-progress task (the caller keeps the regular sidebar).
    /// Boxed — [`IssueFile`](shelbi_state::IssueFile) is large and isn't `Eq`.
    pub task: Option<Box<shelbi_state::IssueFile>>,
}

/// Find the in-progress task assigned to `workspace`, resolving assignment the
/// same way the sidebar does (the published board plus the local assignment
/// overlay, so a remote tracker's fresh assignment is honored).
fn in_progress_task_for(
    project_name: &str,
    project: &Project,
    workspace: &str,
) -> Result<Option<shelbi_state::IssueFile>> {
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let mut in_progress = store.list_in_status(&Column::in_progress())?;
    shelbi_state::fold_assignment_overlay(project_name, &project.issue_tracker, &mut in_progress);
    Ok(in_progress
        .into_iter()
        .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace)))
}

/// Resolve the workspace panel's inputs for `workspace`. Errors when the
/// workspace is unknown or remote (no local session to open).
pub fn workspace_open_info(project_name: &str, workspace: &str) -> Result<WorkspaceOpenInfo> {
    let project = shelbi_state::load_project(project_name)?;
    let ws = project
        .workspace(workspace)
        .ok_or_else(|| Error::Other(format!("unknown workspace `{workspace}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    if machine.host() != Host::Local {
        return Err(Error::Other(format!(
            "workspace `{workspace}` is remote; open it over SSH"
        )));
    }
    let worktree = crate::workspace::workspace_worktree(machine, ws)
        .to_string_lossy()
        .to_string();
    let editor_name = shelbi_state::editor_display_name(&shelbi_state::resolve_editor());
    let task = in_progress_task_for(project_name, &project, workspace)?;
    Ok(WorkspaceOpenInfo {
        worktree,
        editor_name,
        task: task.map(Box::new),
    })
}

/// Ensure the `role` content session for `workspace` is spawned and live, so the
/// TUI client can attach a terminal view to it. Idempotent: a session already
/// live under the name is left as is. Builds the same editor / diff command the
/// review slot's content sessions run (see
/// [`crate::review_ui::editor_session_command`] /
/// [`diff_session_command`](crate::review_ui)).
pub fn ensure_content_session(
    project_name: &str,
    workspace: &str,
    role: ReviewContentRole,
) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let ws = project
        .workspace(workspace)
        .ok_or_else(|| Error::Other(format!("unknown workspace `{workspace}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    if machine.host() != Host::Local {
        return Err(Error::Other(format!(
            "workspace `{workspace}` is remote; open it over SSH"
        )));
    }
    let worktree = crate::workspace::workspace_worktree(machine, ws);

    let name = content_session_name(project_name, workspace, role);
    let backend = SessionProcessBackend;
    // Idempotent: a live session under this name is reused as is.
    if backend
        .live_session_names(&Host::Local)
        .map(|names| names.iter().any(|n| n == &name))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let command = match role {
        ReviewContentRole::Editor => crate::review_ui::editor_session_command(&worktree),
        ReviewContentRole::Diff => crate::review_ui::diff_session_command(&project, &worktree)?,
    };
    // Tag the session with the current task id when there is one (best-effort —
    // the editor/diff work regardless).
    let task = in_progress_task_for(project_name, &project, workspace)
        .ok()
        .flatten()
        .map(|tf| tf.task.id);
    let (cols, rows) = SessionProcessBackend::default_size();
    let spec = shelbi_session::SpawnSpec {
        name,
        cwd: worktree,
        cols,
        rows,
        task,
        raw_output_log: false,
        child_argv: vec!["sh".into(), "-c".into(), command],
    };
    backend.spawn_session(spec)?;
    Ok(())
}

/// End `workspace`'s editor and diff content sessions. Idempotent and
/// best-effort — an already-gone session is fine. The agent session is left for
/// the workspace's normal teardown; a dev workspace runs no review server, so
/// there is nothing else to reap.
pub fn close_content(project_name: &str, workspace: &str) -> Result<()> {
    let backend = SessionProcessBackend;
    for role in [ReviewContentRole::Editor, ReviewContentRole::Diff] {
        let name = content_session_name(project_name, workspace, role);
        let _ = backend.kill_by_name(&Host::Local, &name);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_session_name_is_project_ws_workspace_role() {
        assert_eq!(
            content_session_name("myapp", "alpha", ReviewContentRole::Editor),
            "myapp/ws/alpha/editor"
        );
        assert_eq!(
            content_session_name("myapp", "bravo", ReviewContentRole::Diff),
            "myapp/ws/bravo/diff"
        );
    }

    /// The content session name is distinct from the workspace's agent session
    /// (`<project>/ws/<workspace>`), so attaching the diff/editor never collides
    /// with the agent.
    #[test]
    fn content_name_does_not_collide_with_the_agent_session() {
        let agent = "myapp/ws/alpha";
        let editor = content_session_name("myapp", "alpha", ReviewContentRole::Editor);
        assert_ne!(agent, editor);
        assert!(editor.starts_with(&format!("{agent}/")));
    }
}
