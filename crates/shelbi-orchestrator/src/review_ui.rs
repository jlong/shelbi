//! The review interface's orchestration layer: build the faithful
//! three-column layout (sidebar | review agent/server | review panel) inside
//! the **review workspace's own window**, switch the middle content between
//! the reviewer chat and an editor, and run the Approve / Reject transitions.
//!
//! ## Layout mechanics
//!
//! Under the window-per-workspace model every review slot has its own window
//! (`shelbi-<proj>:<workspace>`) in the attached session, already holding the
//! review agent/server pane. Opening the review interface:
//!
//! 1. splits a pane onto the **right** of that agent pane running `shelbi
//!    review-panel` (the [`crate`]-external ratatui right sidebar), and
//! 2. `select-window`s the review window — the session's
//!    `after-select-window` hook relocates the single traveling sidebar to
//!    its **left**, giving `sidebar | agent | panel`.
//!
//! The dashboard window is never touched, so a review load adds no pane
//! there. Because `swap-pane` exchanges pane *positions* (pane ids travel
//! with their process), the middle column has no stable "position id" — we
//! track the pane id currently occupying the middle in the session env var
//! `SHELBI_REVIEW_MID`, updating it on every swap; [`show_review_view`] swaps
//! the requested pane (chat or editor) against whatever's there. Closing the
//! interface restores the chat pane to the middle, kills the panel/editor
//! panes, clears the env vars, and returns focus to the dashboard.
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
    /// The task is assigned to a review slot, but that slot's window isn't
    /// live yet — first use, or a window reaped by a prior teardown. There's
    /// nothing to embed into, so the caller must launch the workspace (a
    /// background load onto this slot, which checks out the branch and boots
    /// the review agent/server) and re-open once it's up. Carries the review
    /// workspace name to load onto. Without this the embed would target a
    /// nonexistent window and silently no-op (the first-run regression from
    /// window-per-workspace).
    NeedsLaunch { workspace: String },
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

/// Whether a local review workspace's window (`shelbi-<proj>:<ws>`) is live —
/// i.e. it currently holds at least one pane. A review slot that has never
/// hosted a task (or whose window was reaped by a prior teardown) has none, so
/// there is nothing to embed the review interface into and the caller must
/// launch it first. Piggy-backs on [`local_workspace_pane_id`], which fails
/// exactly when the window is absent.
fn review_window_live(session: &str, window: &str) -> bool {
    local_workspace_pane_id(session, window).is_ok()
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
/// [`ReviewOpenOutcome::Loading`]. If it *is* assigned to a review slot but
/// that slot's window isn't live yet (first use, or a reaped window), returns
/// [`ReviewOpenOutcome::NeedsLaunch`] so the caller launches the workspace
/// before re-opening — there is no window to embed into. Otherwise builds the
/// three-column layout **inside the review workspace's own window** —
/// splitting the review panel onto the right of the agent/server pane and
/// switching to that window so the traveling sidebar joins its left — and
/// returns that window's target. The dashboard window is never reshaped, so a
/// review load adds no pane there.
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
    let review_win = format!("{session}:{}", ws.name);

    // Remote review slots live in their own tmux server; swap-pane can't
    // embed them. Degrade to focusing the workspace window.
    if !matches!(machine.host(), Host::Local) {
        crate::focus_workspace(project_name, &ws.name)?;
        return Ok(ReviewOpenOutcome::RemoteFallback(format!(
            "review slot `{}` is remote — opened its window instead of the embedded interface",
            ws.name
        )));
    }

    // First use / reaped window: the review slot the task is assigned to has
    // no live window, so there is nothing to embed into. Signal the caller to
    // launch it (checkout the branch + boot the review agent/server) and
    // re-open once it's up, rather than splitting a panel against a window that
    // doesn't exist — which after window-per-workspace (#447) silently no-ops.
    // Any stale interface state left in the session vars from a prior open of a
    // now-reaped window is cleared first so the re-open takes the fresh path.
    if !review_window_live(&session, &ws.name) {
        if read_session_var(&session, PANEL_KEY).is_some() {
            let _ = close_review_interface(project_name);
        }
        return Ok(ReviewOpenOutcome::NeedsLaunch {
            workspace: ws.name.clone(),
        });
    }

    // Reuse: the interface is already up on this same task — just re-focus its
    // window (the traveling sidebar follows the select-window). Avoids
    // splitting a second panel on a repeat click.
    if read_session_var(&session, PANEL_KEY).is_some()
        && read_session_var(&session, TASK_KEY).as_deref() == Some(task_id)
    {
        let _ = tmux_run(&["select-window", "-t", &review_win]);
        return Ok(ReviewOpenOutcome::Opened(review_win));
    }
    // A panel left over from another task (or a crash) is torn down first so we
    // never leak its panes or stack a second panel into the window.
    if read_session_var(&session, PANEL_KEY).is_some() {
        let _ = close_review_interface(project_name);
    }

    // The review agent/server pane already occupies the review window — it is
    // the MIDDLE column. Pin it as the chat/middle before adding the panel.
    let chat = local_workspace_pane_id(&session, &ws.name)?;
    set_session_var(&session, TASK_KEY, task_id)?;
    set_session_var(&session, CHAT_KEY, &chat)?;
    set_session_var(&session, MID_KEY, &chat)?;

    // 1. Split the review panel onto the right of the agent pane (detached so
    //    focus stays on the agent pane — the sidebar join below inserts to the
    //    left of *that* focused pane, yielding `sidebar | agent | panel`).
    let shelbi_bin = crate::current_exe_string()?;
    let panel_cmd = review_panel_cmd(&shelbi_bin, project_name, task_id);
    let panel_id = tmux_capture(&[
        "split-window",
        "-h",
        "-d",
        "-t",
        &chat,
        "-P",
        "-F",
        "#{pane_id}",
        "sh",
        "-c",
        &panel_cmd,
    ])?;
    set_session_var(&session, PANEL_KEY, &panel_id)?;

    // 2. Switch to the review window; the `after-select-window` hook relocates
    //    the traveling sidebar to its left and restores focus to the agent
    //    pane.
    let _ = tmux_run(&["select-window", "-t", &review_win]);
    let _ = tmux_run(&["select-pane", "-t", &chat]);
    Ok(ReviewOpenOutcome::Opened(review_win))
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

/// Tear the review interface back down: bring the agent/chat pane back to the
/// review window's middle (if the editor is showing), kill the review-panel
/// and editor panes, clear the `SHELBI_REVIEW_*` session vars, and return
/// focus to the dashboard. The review window is left with just its agent pane
/// (plus the sidebar, which travels back on the dashboard select). Idempotent
/// and best-effort — a missing var means nothing to undo.
pub fn close_review_interface(project_name: &str) -> Result<()> {
    let session = format!("shelbi-{project_name}");
    let dashboard = format!("{session}:dashboard");

    let chat = read_session_var(&session, CHAT_KEY);
    let mid = read_session_var(&session, MID_KEY);

    // If the editor is currently in the middle, swap the chat pane back so the
    // agent pane returns to the review window before the editor's hidden
    // window is destroyed by the kill below.
    if let (Some(chat), Some(mid)) = (chat.as_deref(), mid.as_deref()) {
        if mid != chat {
            let _ = tmux_run(&["swap-pane", "-s", chat, "-t", mid]);
        }
    }
    // Kill the editor (its hidden window closes with it) and the review panel.
    if let Some(editor) = read_session_var(&session, EDITOR_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &editor]);
    }
    if let Some(panel) = read_session_var(&session, PANEL_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &panel]);
    }
    for key in [MID_KEY, PANEL_KEY, EDITOR_KEY, CHAT_KEY, TASK_KEY] {
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
        let keys = [MID_KEY, PANEL_KEY, EDITOR_KEY, CHAT_KEY, TASK_KEY];
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "review session keys must be unique");
    }
}
