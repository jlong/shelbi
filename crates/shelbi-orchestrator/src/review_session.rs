//! Daemon-side lifecycle of a review slot's **content sessions** — the editor
//! and diff tool the single-process TUI review interface shows in a terminal
//! view (`rt-tui-review`, removing-tmux Phase 4e).
//!
//! In the tmux runtime these are lazily-created panes parked in a hidden stash
//! session ([`crate::review_ui::ensure_editor_pane`] /
//! [`ensure_diff_pane`](crate::review_ui)). In the single-process TUI there are
//! no panes: each is a real `shelbi __session` process named
//! `<project>/review/<slot>/<role>` that the client attaches a terminal view to.
//!
//! The **daemon owns their lifetime** (confirmed design): the TUI client asks
//! the daemon — over the control socket ([`shelbi_proto::control::ClientMsg::ReviewSession`])
//! — to [`ensure_content_session`] one on demand and to [`close_review`] the
//! whole interface on teardown. Because the daemon spawns them, they survive a
//! client detach, and freeing the slot's port on teardown (reaping the dev
//! server's process group) is the daemon's job, not a disconnecting client's.
//!
//! The agent (chat) session and the dev server are **not** spawned here — the
//! dispatch/resume path owns those ([`crate::load`]); this module only adds the
//! editor/diff and, on close, reaps the server via
//! [`stop_review_server`](crate::workspace::stop_review_server).
//!
//! Local slots only: a remote review slot has no local session to spawn (the
//! tmux path degrades it to a focused remote window), so a remote slot here is
//! a clear `Err` the client surfaces on its status line rather than a silent
//! no-op.

use shelbi_core::{Column, Error, Host, Project, Result};

use crate::session_process_backend::SessionProcessBackend;

/// Which content session of a review slot to manage. Mirrors
/// [`shelbi_proto::control::ReviewRole`] but kept local so this crate does not
/// depend on the wire type; the daemon handler maps between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewContentRole {
    /// `$EDITOR` opened on the review worktree.
    Editor,
    /// The configured git difftool over the review branch's changes.
    Diff,
}

impl ReviewContentRole {
    /// The `<role>` component of the session name.
    pub fn as_str(&self) -> &'static str {
        match self {
            ReviewContentRole::Editor => "editor",
            ReviewContentRole::Diff => "diff",
        }
    }
}

/// The session name a review slot's `role` content session is spawned and
/// looked up under: `<project>/review/<slot>/<role>`. Pure, so the daemon's
/// spawn and the client's attach agree without a shared helper crossing the
/// wire.
pub fn content_session_name(project_name: &str, slot: &str, role: ReviewContentRole) -> String {
    format!("{project_name}/review/{slot}/{}", role.as_str())
}

/// The review slot (workspace name) a review-column task is loaded on, resolved
/// the same way [`crate::review_ui::review_layout_state`] does: the local
/// assignment overlay for a remote backend, the card's `assigned_to` otherwise.
/// `None` when the task is not a review-column task pinned to a review-tagged
/// slot — nothing to manage.
fn resolve_review_slot(project_name: &str, project: &Project, task_id: &str) -> Result<Option<String>> {
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let Some(tf) = store.get(task_id)? else {
        return Ok(None);
    };
    if tf.task.column != Column::review() {
        return Ok(None);
    }
    let owner = if project.issue_tracker.backend.is_remote() {
        shelbi_state::task_assignments(project_name)
            .unwrap_or_default()
            .get(task_id)
            .cloned()
    } else {
        tf.task.assigned_to.clone()
    };
    let Some(ws_name) = owner else {
        return Ok(None);
    };
    let Some(ws) = project.workspace(&ws_name) else {
        return Ok(None);
    };
    if !project.effective_tags(ws).contains("review") {
        return Ok(None);
    }
    Ok(Some(ws_name))
}

/// Everything the single-process TUI needs to build its review panel for a
/// task, resolved from durable state in one place (so the client stays thin and
/// the slot resolution matches the spawn path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewOpenInfo {
    /// The review slot (workspace) the task is loaded on.
    pub slot: String,
    /// Absolute review worktree path (shown truncated, revealed on click).
    pub worktree: String,
    /// Display name of the resolved editor (`Vim`, `Helix`, …).
    pub editor_name: String,
    /// Whether the workflow declares a review URL (gates the Browser entry).
    pub has_review_url: bool,
}

/// Resolve the panel inputs for `task_id`. Errors when the task is not loaded on
/// a review slot (the interface can't open yet).
pub fn review_open_info(project_name: &str, task_id: &str) -> Result<ReviewOpenInfo> {
    let project = shelbi_state::load_project(project_name)?;
    let slot = resolve_review_slot(project_name, &project, task_id)?.ok_or_else(|| {
        Error::Other(format!("`{task_id}` is not loaded on a review slot"))
    })?;
    let ws = project
        .workspace(&slot)
        .ok_or_else(|| Error::Other(format!("unknown review slot `{slot}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let worktree = crate::workspace::workspace_worktree(machine, ws)
        .to_string_lossy()
        .to_string();
    let editor_name = shelbi_state::editor_display_name(&shelbi_state::resolve_editor());
    let has_review_url = {
        let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
        match store.get(task_id)? {
            Some(tf) => shelbi_state::load_task_workflow(project_name, &project, &tf.task)
                .ok()
                .map(|wf| wf.review_url_for_status(tf.task.column.as_str()).is_some())
                .unwrap_or(false),
            None => false,
        }
    };
    Ok(ReviewOpenInfo {
        slot,
        worktree,
        editor_name,
        has_review_url,
    })
}

/// Ensure the `role` content session for `task_id`'s review slot is spawned and
/// live, so the TUI client can attach a terminal view to it. Idempotent: a
/// session already live under the name is left as is. Builds the same editor /
/// diff command the tmux panes run (see
/// [`crate::review_ui::editor_session_command`] /
/// [`diff_session_command`](crate::review_ui)).
pub fn ensure_content_session(
    project_name: &str,
    task_id: &str,
    role: ReviewContentRole,
) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let Some(slot) = resolve_review_slot(project_name, &project, task_id)? else {
        return Err(Error::Other(format!(
            "`{task_id}` is not loaded on a review slot"
        )));
    };
    let ws = project
        .workspace(&slot)
        .ok_or_else(|| Error::Other(format!("unknown review slot `{slot}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    if machine.host() != Host::Local {
        return Err(Error::Other(format!(
            "review slot `{slot}` is remote; open it over SSH"
        )));
    }
    let worktree = crate::workspace::workspace_worktree(machine, ws);

    let name = content_session_name(project_name, &slot, role);
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
    let (cols, rows) = SessionProcessBackend::default_size();
    let spec = shelbi_session::SpawnSpec {
        name,
        cwd: worktree,
        cols,
        rows,
        task: Some(task_id.to_string()),
        raw_output_log: false,
        child_argv: vec!["sh".into(), "-c".into(), command],
    };
    backend.spawn_session(spec)?;
    Ok(())
}

/// Tear the review interface for `task_id` down: end its editor and diff
/// sessions and reap the slot's dev-server process group (which frees the
/// port). Idempotent and best-effort — an already-gone session or a slot with
/// no server record is fine. The agent (chat) session is left for the slot's
/// normal teardown ([`crate::workspace::kill_workspace_pane`] on accept).
///
/// This is the daemon's half of AC "closing a review leaves no editor, diff or
/// server process running and the port free": the client never kills these
/// itself, so detaching can't orphan a half-torn-down interface.
pub fn close_review(project_name: &str, task_id: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let Some(slot) = resolve_review_slot(project_name, &project, task_id)? else {
        // Not on a review slot (already advanced / unassigned): nothing to tear
        // down. Benign.
        return Ok(());
    };
    let backend = SessionProcessBackend;
    for role in [ReviewContentRole::Editor, ReviewContentRole::Diff] {
        let name = content_session_name(project_name, &slot, role);
        // Best-effort: an already-dead session is fine.
        let _ = backend.kill_by_name(&Host::Local, &name);
    }
    // Reap the setsid'd dev server's whole process group, freeing the port it
    // held. No-op without a pgid record.
    crate::workspace::stop_review_server(&slot);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_session_name_is_project_review_slot_role() {
        assert_eq!(
            content_session_name("myapp", "review-1", ReviewContentRole::Editor),
            "myapp/review/review-1/editor"
        );
        assert_eq!(
            content_session_name("myapp", "review-2", ReviewContentRole::Diff),
            "myapp/review/review-2/diff"
        );
    }

    #[test]
    fn role_as_str_round_trips_the_name_component() {
        assert_eq!(ReviewContentRole::Editor.as_str(), "editor");
        assert_eq!(ReviewContentRole::Diff.as_str(), "diff");
    }
}
