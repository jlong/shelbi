//! The review interface's orchestration layer: reshape the dashboard window
//! into the faithful three-column layout (sidebar | reviewer content |
//! review panel), switch the middle content between the reviewer chat and an
//! editor, and run the Approve / Reject transitions.
//!
//! ## Layout mechanics
//!
//! The dashboard window normally holds two panes — the ratatui sidebar
//! (left) and a content slot (right) other views `swap-pane` into. Opening
//! the review interface:
//!
//! 1. splits a **third** pane onto the right running `shelbi review-panel`
//!    (the [`crate`]-external ratatui right sidebar), and
//! 2. `swap-pane`s the review workspace's chat pane into the **middle**
//!    content slot.
//!
//! Because `swap-pane` exchanges pane *positions* (pane ids travel with
//! their process), there is no stable "middle position id". We track the
//! pane id currently occupying the middle in the session env var
//! `SHELBI_REVIEW_MID`, updating it on every swap; [`show_review_view`]
//! swaps the requested pane against whatever's there. Closing the interface
//! restores the original content pane to the middle, kills the panel/editor
//! panes, and clears the env vars.
//!
//! Only **local** (hub) review workspaces can be embedded — `swap-pane`
//! can't reach a pane living in a remote workspace's own tmux server — so a
//! remote review slot degrades to focusing its window (the existing
//! `shelbi open` behavior) with a status note rather than a broken embed.
//!
//! All tmux calls here run on the hub (matching the `show_view` convention);
//! failures surface as `Err` for the caller to put on the status line, never
//! a panic. The pane-embedding is inherently integration-level and validated
//! by CI; the pure pieces (Approve/Reject transitions, session-env keys) are
//! unit-tested.

use chrono::Utc;
use shelbi_core::{Column, Error, Host, Result};

use crate::load;

/// Session env var holding the pane id currently shown in the middle content
/// slot. Updated on every [`show_review_view`] swap.
const MID_KEY: &str = "SHELBI_REVIEW_MID";
/// Session env var holding the original dashboard content pane id, so
/// [`close_review_interface`] can restore the two-pane layout.
const CONTENT_KEY: &str = "SHELBI_REVIEW_CONTENT";
/// Session env var holding the review-panel (right sidebar) pane id.
const PANEL_KEY: &str = "SHELBI_REVIEW_PANEL";
/// Session env var holding the lazily-created editor pane id.
const EDITOR_KEY: &str = "SHELBI_REVIEW_EDITOR";
/// Session env var holding the review agent's chat pane id.
const CHAT_KEY: &str = "SHELBI_REVIEW_CHAT";
/// Session env var holding the task id the interface is currently open on.
const TASK_KEY: &str = "SHELBI_REVIEW_TASK";

/// Which view the middle content slot should show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewMidView {
    /// The review workspace's chat pane (the review agent).
    Chat,
    /// An editor opened in the review worktree.
    Editor,
}

/// Outcome of [`open_review_interface`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOpenOutcome {
    /// The task wasn't loaded on a review slot yet; a background load was
    /// kicked off. The caller re-opens once the sidebar shows it Ready.
    Loading,
    /// The three-pane interface is up; the string is the tmux target to
    /// focus (`session:window`).
    Opened(String),
    /// A remote review slot can't be embedded — the caller focused the
    /// workspace window instead. Carries a human note for the status line.
    RemoteFallback(String),
}

// ---------------------------------------------------------------------------
// tmux helpers

fn tmux_run(args: &[&str]) -> Result<()> {
    let out = std::process::Command::new("tmux")
        .args(args)
        .output()
        .map_err(Error::Io)?;
    if out.status.success() {
        Ok(())
    } else {
        Err(Error::Other(format!(
            "tmux {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

fn tmux_capture(args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("tmux")
        .args(args)
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "tmux {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read a session env var (`SHELBI_REVIEW_*`), returning `None` when unset.
fn read_session_var(session: &str, key: &str) -> Option<String> {
    let line = tmux_capture(&["show-environment", "-t", session, key]).ok()?;
    if line.starts_with('-') {
        return None;
    }
    line.split_once('=')
        .map(|(_, v)| v.to_string())
        .filter(|v| !v.is_empty())
}

fn set_session_var(session: &str, key: &str, value: &str) -> Result<()> {
    tmux_run(&["set-environment", "-t", session, key, value])
}

fn unset_session_var(session: &str, key: &str) {
    let _ = tmux_run(&["set-environment", "-t", session, "-u", key]);
}

/// Pane id currently sitting in the dashboard's right (content) slot.
fn content_pane_id(session: &str) -> Result<String> {
    let target = format!("{session}:dashboard.{{right}}");
    let id = tmux_capture(&["display-message", "-p", "-t", &target, "#{pane_id}"])?;
    if id.is_empty() {
        return Err(Error::Other("dashboard has no content pane".into()));
    }
    Ok(id)
}

/// First pane id of a local workspace's window (`shelbi-<proj>:<ws>`).
fn local_workspace_pane_id(session: &str, window: &str) -> Result<String> {
    let target = format!("{session}:{window}");
    let out = tmux_capture(&["list-panes", "-t", &target, "-F", "#{pane_id}"])?;
    out.lines()
        .next()
        .map(str::to_string)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Error::Other(format!("workspace window `{window}` has no pane")))
}

// ---------------------------------------------------------------------------
// Open / switch / close

/// Open (or re-focus) the three-column review interface for `task_id`.
///
/// If the task isn't loaded onto a review workspace yet, kicks off a
/// background load (detached pane — no focus steal) and returns
/// [`ReviewOpenOutcome::Loading`]. Otherwise builds the three-pane layout
/// and returns the tmux target to focus.
pub fn open_review_interface(project_name: &str, task_id: &str) -> Result<ReviewOpenOutcome> {
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;

    // Is the task loaded on a *review* workspace (Ready), or still queued?
    let review_ws = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|name| project.workspace(name))
        .filter(|w| project.effective_tags(w).contains("review"))
        .cloned();

    let Some(ws) = review_ws else {
        // Queued: load it onto a free review slot in the background. The
        // start path creates the pane detached, so nothing steals focus.
        load::load_task_by_id(project_name, task_id)?;
        return Ok(ReviewOpenOutcome::Loading);
    };

    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let session = format!("shelbi-{project_name}");
    let dashboard = format!("{session}:dashboard");

    // Remote review slots live in their own tmux server; swap-pane can't
    // embed them. Degrade to focusing the workspace window.
    if !matches!(machine.host(), Host::Local) {
        crate::focus_workspace(project_name, &ws.name)?;
        return Ok(ReviewOpenOutcome::RemoteFallback(format!(
            "review slot `{}` is remote — opened its window instead of the embedded interface",
            ws.name
        )));
    }

    // Re-entrancy: a prior interface (opened on another review task, or left
    // over after a panel crash) is torn down first so we never leak its panes.
    if read_session_var(&session, PANEL_KEY).is_some() {
        let _ = close_review_interface(project_name);
    }

    // Capture the original content pane before adding the panel to its right.
    let content = content_pane_id(&session)?;
    set_session_var(&session, CONTENT_KEY, &content)?;
    set_session_var(&session, TASK_KEY, task_id)?;

    // 1. Split the review panel onto the right of the content pane.
    let shelbi_bin = crate::current_exe_string()?;
    let panel_cmd = review_panel_cmd(&shelbi_bin, project_name, task_id);
    let panel_id = tmux_capture(&[
        "split-window",
        "-h",
        "-d",
        "-t",
        &content,
        "-P",
        "-F",
        "#{pane_id}",
        "sh",
        "-c",
        &panel_cmd,
    ])?;
    set_session_var(&session, PANEL_KEY, &panel_id)?;

    // 2. Swap the review workspace's chat pane into the middle (content) slot.
    //    The displaced content pane parks in the workspace window and stays
    //    there for the whole session, so close can swap it straight back.
    let chat = local_workspace_pane_id(&session, &ws.name)?;
    if chat != content {
        tmux_run(&["swap-pane", "-s", &chat, "-t", &content])?;
    }
    set_session_var(&session, CHAT_KEY, &chat)?;
    set_session_var(&session, MID_KEY, &chat)?;

    // 3. Focus the dashboard + the middle pane.
    let _ = tmux_run(&["select-window", "-t", &dashboard]);
    let _ = tmux_run(&["select-pane", "-t", &chat]);
    Ok(ReviewOpenOutcome::Opened(dashboard))
}

/// Swap the requested view into the middle content slot, preserving both the
/// chat and editor panes (they're swapped, never killed). The editor pane is
/// created lazily on the first [`ReviewMidView::Editor`] request.
pub fn show_review_view(project_name: &str, task_id: &str, view: ReviewMidView) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;
    let ws = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|name| project.workspace(name))
        .ok_or_else(|| Error::Other(format!("task `{task_id}` isn't loaded on a workspace")))?;
    let session = format!("shelbi-{project_name}");

    let current_mid = read_session_var(&session, MID_KEY)
        .ok_or_else(|| Error::Other("review interface isn't open".into()))?;

    let target = match view {
        // The chat pane id is pinned at open time; fall back to a live lookup
        // if the var was somehow cleared.
        ReviewMidView::Chat => read_session_var(&session, CHAT_KEY)
            .map(Ok)
            .unwrap_or_else(|| local_workspace_pane_id(&session, &ws.name))?,
        ReviewMidView::Editor => ensure_editor_pane(&project, ws, &session)?,
    };

    if target == current_mid {
        return Ok(()); // Already showing.
    }
    // Swap the target pane into the middle position; whatever was there
    // (chat or editor) parks in the target's home window, never killed — so
    // both sessions survive the switch.
    tmux_run(&["swap-pane", "-s", &target, "-t", &current_mid])?;
    set_session_var(&session, MID_KEY, &target)?;
    let _ = tmux_run(&["select-pane", "-t", &target]);
    Ok(())
}

/// Create the editor pane in the review worktree if it doesn't exist yet,
/// returning its pane id. Parked in a **hidden window** (not a fourth visible
/// dashboard pane) so the caller can swap it into the middle without ever
/// showing four columns. The editor is resolved hub-wide via
/// [`shelbi_state::resolve_editor`].
fn ensure_editor_pane(
    project: &shelbi_core::Project,
    ws: &shelbi_core::WorkspaceSpec,
    session: &str,
) -> Result<String> {
    if let Some(existing) = read_session_var(session, EDITOR_KEY) {
        return Ok(existing);
    }
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let worktree = crate::workspace::workspace_worktree(machine, ws);
    let editor = shelbi_state::resolve_editor();
    // `cd <worktree> && exec <editor>` — exec so the pane dies with the editor
    // rather than dropping to a shell. The worktree path is shell-escaped; the
    // editor command may carry flags (`code --wait`) so it is *not* escaped as
    // a single word.
    let cmd = format!(
        "cd {} && exec {}",
        shelbi_core::shell_escape(&worktree.to_string_lossy()),
        editor,
    );
    let win = format!("{session}:__review-editor");
    let editor_id = tmux_capture(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("{session}:"),
        "-n",
        "__review-editor",
        "sh",
        "-c",
        &cmd,
    ])
    .map_err(|e| Error::Other(format!("could not open editor window {win}: {e}")))?;
    set_session_var(session, EDITOR_KEY, &editor_id)?;
    Ok(editor_id)
}

/// Tear the review interface back down to the normal two-pane dashboard:
/// restore the original content pane to the middle, kill the review-panel
/// and editor panes, and clear the `SHELBI_REVIEW_*` session vars. Idempotent
/// and best-effort — a missing var means nothing to undo.
pub fn close_review_interface(project_name: &str) -> Result<()> {
    let session = format!("shelbi-{project_name}");
    let dashboard = format!("{session}:dashboard");

    let content = read_session_var(&session, CONTENT_KEY);
    let chat = read_session_var(&session, CHAT_KEY);
    let mid = read_session_var(&session, MID_KEY);

    // Restore in two hops so the review agent's chat pane lands back in its
    // workspace window rather than being stranded in the editor's hidden
    // window (which the kill below would then destroy):
    //
    //   1. bring the chat pane back to the middle (if the editor is showing),
    //   2. swap the original content pane back into the middle — which returns
    //      the chat pane to the workspace window it was displaced from at open.
    if let (Some(chat), Some(mid)) = (chat.as_deref(), mid.as_deref()) {
        if mid != chat {
            let _ = tmux_run(&["swap-pane", "-s", chat, "-t", mid]);
        }
    }
    if let (Some(content), Some(chat)) = (content.as_deref(), chat.as_deref()) {
        if content != chat {
            let _ = tmux_run(&["swap-pane", "-s", content, "-t", chat]);
        }
    }
    // Kill the editor (its hidden window closes with it) and the review panel.
    if let Some(editor) = read_session_var(&session, EDITOR_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &editor]);
    }
    if let Some(panel) = read_session_var(&session, PANEL_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &panel]);
    }
    for key in [MID_KEY, CONTENT_KEY, PANEL_KEY, EDITOR_KEY, CHAT_KEY, TASK_KEY] {
        unset_session_var(&session, key);
    }
    let _ = tmux_run(&["select-window", "-t", &dashboard]);
    Ok(())
}

/// The `sh -c` body that runs the review panel in its pane. Kept as a
/// standalone builder so a test can lock the invocation shape (mirrors
/// [`crate::sidebar_cmd`]).
fn review_panel_cmd(shelbi_bin: &str, project_name: &str, task_id: &str) -> String {
    // Run once (no respawn loop): unlike the persistent sidebar/tasks panes,
    // the review panel is ephemeral — quitting it (q / Approve / Reject) tears
    // the interface down, and a respawn loop would fight that by re-launching.
    format!(
        "{bin} __review-panel {proj} {task}",
        bin = shelbi_core::shell_escape(shelbi_bin),
        proj = shelbi_core::shell_escape(project_name),
        task = shelbi_core::shell_escape(task_id),
    )
}

// ---------------------------------------------------------------------------
// Approve / Reject transitions

/// **Approve**: move the task out of its review status via the normal
/// forward (accept) transition — the same move the Kanban board makes on a
/// card sent one column right. Everything wired to that move (merge on
/// accept, review-workspace teardown, slot free) fires through the existing
/// orchestrator reaction to the emitted move event; no merge logic lives
/// here.
pub fn approve_review_task(project_name: &str, task_id: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;
    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let current = tf.task.column.as_str();
    let target = workflow
        .forward_status(current)
        .map(|s| Column::from_status_id(&s.id))
        .ok_or_else(|| {
            Error::Other(format!(
                "no accept transition out of status `{current}` in this workflow"
            ))
        })?;
    if let Some((from, to, wf)) = shelbi_state::move_task(project_name, task_id, target)? {
        let _ = shelbi_state::append_task_event(project_name, task_id, &wf, from, to, "user:review");
    }
    Ok(())
}

/// **Reject**: append the reviewer's `reason` to the task body as a marked
/// fix section and bounce the task back to the workflow's ready status so
/// normal auto-dispatch picks it back up with the feedback baked into the
/// task description. Emits the move event on the existing channel — the
/// structured signal the orchestrator reacts to — with the reason durably in
/// the task body rather than a transient message.
pub fn reject_review_task(project_name: &str, task_id: &str, reason: &str) -> Result<()> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    if let Some((from, to, wf)) =
        shelbi_state::reject_review_task(project_name, task_id, reason, &date)?
    {
        let _ = shelbi_state::append_task_event(
            project_name,
            task_id,
            &wf,
            from,
            to,
            "user:review-reject",
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_panel_cmd_is_a_respawn_loop_invoking_the_subcommand() {
        let cmd = review_panel_cmd("/usr/local/bin/shelbi", "myapp", "fix-login");
        assert!(cmd.contains("__review-panel"), "invokes subcommand: {cmd}");
        assert!(cmd.contains("myapp"), "passes project: {cmd}");
        assert!(cmd.contains("fix-login"), "passes task id: {cmd}");
        // Ephemeral: run once, no respawn loop (so q dismisses it).
        assert!(!cmd.contains("while true"), "no respawn loop: {cmd}");
    }

    #[test]
    fn review_panel_cmd_shell_escapes_a_spaced_binary_path() {
        let cmd = review_panel_cmd("/Users/jane doe/.cargo/bin/shelbi", "myapp", "t-1");
        assert!(
            cmd.contains("'/Users/jane doe/.cargo/bin/shelbi'"),
            "spaced path must be quoted: {cmd}"
        );
    }

    #[test]
    fn session_env_keys_are_distinct() {
        let keys = [MID_KEY, CONTENT_KEY, PANEL_KEY, EDITOR_KEY, CHAT_KEY, TASK_KEY];
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "review session keys must be unique");
    }
}
