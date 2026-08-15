//! The review interface's orchestration layer: build the faithful
//! two-column layout (review panel | review agent/server) inside the **review
//! workspace's own window**, switch the right-hand content between the reviewer
//! chat and an editor, and run the Approve / Reject transitions.
//!
//! ## Layout mechanics
//!
//! Under the window-per-workspace model every review slot has its own window
//! (`shelbi-<proj>:<workspace>`) in the attached session, already holding the
//! review agent/server pane. Opening the review interface:
//!
//! 1. splits a pane onto the **left** of that agent pane running `shelbi
//!    review-panel` (the [`crate`]-external ratatui review panel — the review
//!    window's own left nav), and
//! 2. `select-window`s the review window, giving `panel | agent`.
//!
//! The dashboard sidebar lives permanently in the dashboard window and never
//! travels, so opening a review window never touches it: the review panel is
//! that window's own left nav and the dashboard keeps its sidebar. Each review
//! window carries its own panel (created here on open, torn down on close), so
//! loading or closing one review window leaves any other review window's panel
//! and the dashboard sidebar untouched. The panel's top **back button**
//! ([`focus_dashboard`]) switches focus back to the dashboard without tearing
//! the interface down.
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
/// Session env var holding the review window's own panel (left nav) pane id.
///
/// Read by the sidebar-clamp script (see [`crate::sidebar_clamp_script`]) so the
/// panel is sized within its own window, independently of the dashboard sidebar.
pub(crate) const PANEL_KEY: &str = "SHELBI_REVIEW_PANEL";
/// Session env var holding the lazily-created editor pane id.
const EDITOR_KEY: &str = "SHELBI_REVIEW_EDITOR";
/// Session env var holding the lazily-created diff-tool pane id.
const DIFF_KEY: &str = "SHELBI_REVIEW_DIFF";
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
    /// The OS-configured diff tool (`git difftool`) run over the review
    /// branch's changes in the review worktree.
    Diff,
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

/// Whether `pane_id` is currently a live pane **of the review window**
/// (`session:window`). The review panel is spawned run-once (no respawn loop),
/// so a crash / accidental Ctrl-C / any exit that bypasses
/// [`close_review_interface`]'s teardown closes the pane but leaves its id
/// stashed in [`PANEL_KEY`]. The reuse path must confirm the pane is actually
/// present — and present in the *review* window, not stranded elsewhere — before
/// trusting a stashed panel id; otherwise it re-focuses a window whose review
/// sidebar has vanished (the "sidebar disappears and never comes back" bug).
/// A `list-panes` failure (window gone) or an absent id reads as not-live so the
/// caller (re)spawns.
fn panel_pane_live(session: &str, window: &str, pane_id: &str) -> bool {
    let target = format!("{session}:{window}");
    tmux_capture(&["list-panes", "-t", &target, "-F", "#{pane_id}"])
        .map(|out| out.lines().any(|l| l.trim() == pane_id))
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Open / switch / close

/// Open (or re-focus) the two-column review interface for `task_id`.
///
/// If the task isn't loaded onto a review workspace yet, kicks off a
/// background load (detached pane — no focus steal) and returns
/// [`ReviewOpenOutcome::Loading`]. If it *is* assigned to a review slot but
/// that slot's window isn't live yet (first use, or a reaped window), returns
/// [`ReviewOpenOutcome::NeedsLaunch`] so the caller launches the workspace
/// before re-opening — there is no window to embed into. Otherwise builds the
/// two-column layout **inside the review workspace's own window** —
/// splitting the review panel onto the left of the agent/server pane and
/// switching to that window — and returns that window's target. The dashboard
/// sidebar never travels, so it stays docked in the dashboard and the dashboard
/// window is never reshaped — a review load adds a panel only to its own window.
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
        // Routed through the review-specific loader (not the generic
        // `load_task_by_id`) so a handoff task pinned to its dev slot is never
        // re-seeded there, and the Review agent — not the review status's Zen
        // agent — is dispatched onto the slot.
        load::load_task_for_review(project_name, task_id)?;
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

    // Reuse: the interface is already up on this same task AND its panel pane is
    // still live in the review window — just re-focus (switching windows
    // relocates nothing). Avoids splitting a second panel on a repeat click.
    //
    // The panel is run-once (no respawn loop), so a crash / accidental Ctrl-C /
    // any exit that bypasses `close_review_interface` closes the pane but leaves
    // its id stashed in PANEL_KEY. Verifying liveness before re-focusing is the
    // guard that keeps a dead panel from resurfacing as a blank review sidebar:
    // a dead (or wrong-window) panel falls through to the teardown + (re)spawn
    // below instead of re-focusing a window whose sidebar has vanished.
    if let Some(panel) = read_session_var(&session, PANEL_KEY) {
        if read_session_var(&session, TASK_KEY).as_deref() == Some(task_id)
            && panel_pane_live(&session, &ws.name, &panel)
        {
            let _ = tmux_run(&["select-window", "-t", &review_win]);
            return Ok(ReviewOpenOutcome::Opened(review_win));
        }
        // A panel left over from another task, or one that crashed/exited (dead,
        // or stranded in the wrong window), is torn down first so we never leak
        // its panes or stack a second panel into the window before (re)spawning.
        let _ = close_review_interface(project_name);
    }

    // The review agent/server pane already occupies the review window — it is
    // the CONTENT (right) column. Pin it as the chat/content before adding the
    // panel on its left.
    let chat = local_workspace_pane_id(&session, &ws.name)?;
    set_session_var(&session, TASK_KEY, task_id)?;
    set_session_var(&session, CHAT_KEY, &chat)?;
    set_session_var(&session, MID_KEY, &chat)?;

    // 1. Split the review panel onto the LEFT of the agent pane (`-b`, and
    //    detached so focus stays on the agent pane), yielding `panel | agent`.
    //    This panel is the review window's own left nav; the dashboard sidebar
    //    is a separate pane in the dashboard window and is never involved.
    let shelbi_bin = crate::current_exe_string()?;
    let panel_cmd = review_panel_cmd(&shelbi_bin, project_name, task_id);
    // Open the panel at the *shared* nav width — the user's stored width
    // override (SHELBI_SIDEBAR_W) if they've dragged a divider, else the
    // SIDEBAR_TARGET_PCT-of-window default clamped to [MIN, MAX] — instead of
    // tmux's default 50/50 split of the agent pane. This makes the panel open
    // at the very width the sidebar already has, so the two never flash to
    // different sizes before the clamp hook (which also keeps them locked as
    // either is dragged) first fires. A failed `#{window_width}` query with no
    // override falls back to the max sidebar width, never the 50% default.
    let width_override = read_session_var(&session, crate::SIDEBAR_WIDTH_KEY)
        .and_then(|v| v.trim().parse::<u32>().ok());
    let panel_cols = tmux_capture(&["display-message", "-p", "-t", &chat, "#{window_width}"])
        .ok()
        .and_then(|w| w.trim().parse::<u32>().ok())
        .map(|w| crate::shared_sidebar_cols(w, width_override))
        .or_else(|| width_override.map(|o| o.clamp(crate::SIDEBAR_MIN_COLS, crate::SIDEBAR_MAX_COLS)))
        .unwrap_or(crate::SIDEBAR_MAX_COLS);
    let panel_cols = panel_cols.to_string();
    let panel_id = tmux_capture(&[
        "split-window",
        "-h",
        "-b",
        "-l",
        &panel_cols,
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

    // 2. Switch to the review window. Switching windows relocates no panes, so
    //    the dashboard keeps its sidebar and this window shows just
    //    `panel | agent`.
    let _ = tmux_run(&["select-window", "-t", &review_win]);
    let _ = tmux_run(&["select-pane", "-t", &chat]);
    Ok(ReviewOpenOutcome::Opened(review_win))
}

/// Switch the attached client's active window back to the dashboard without
/// tearing the review interface down. This is the review panel's **back
/// button** action: it navigates focus (the same `select-window` the close
/// path ends with) while leaving the review window's `panel | agent` panes
/// intact, so the reviewer can return to the still-loaded review by re-opening
/// it from the sidebar. Best-effort — a tmux failure surfaces as `Err` for the
/// caller's status line.
pub fn focus_dashboard(project_name: &str) -> Result<()> {
    let session = format!("shelbi-{project_name}");
    let dashboard = format!("{session}:dashboard");
    tmux_run(&["select-window", "-t", &dashboard])
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
        ReviewMidView::Diff => ensure_diff_pane(&project, ws, &session)?,
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
/// returning its pane id. The pane is parked in its own `__review-editor`
/// window inside the **hidden stash session** `_{session}` — the same detached
/// session that holds the `__views` panes — so it never appears in the user's
/// visible window list (tmux has no hidden-*window* concept; hiding means
/// living in a detached session no one is attached to). [`show_review_view`]
/// swaps it into the middle column on demand; pane ids are global in tmux, so
/// the cross-session swap works exactly like a within-session one. The editor
/// is resolved hub-wide via [`shelbi_state::resolve_editor`].
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
    // Park the editor in the hidden stash session, not the visible one, so no
    // `__review-editor` window shows in the user's window list. Bootstrap
    // builds the stash, but the editor can be opened before/without a full
    // dashboard rebuild, so ensure it exists first (building it whole — with
    // its `views` window — rather than failing).
    ensure_stash_session(project, session)?;
    let stash = format!("_{session}");
    // Its own window inside the stash (not spliced into the `views` window) so
    // it tears down independently — killing this window on close leaves the
    // `views` panes untouched.
    let win = format!("{stash}:__review-editor");
    let editor_id = tmux_capture(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("{stash}:"),
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

/// Create the diff pane in the review worktree if it doesn't exist yet,
/// returning its pane id. Like [`ensure_editor_pane`] it parks the pane in its
/// own `__review-diff` window inside the hidden stash session `_{session}` so
/// no window shows in the user's window list; [`show_review_view`] swaps it
/// into the middle column on demand.
///
/// The pane runs `git difftool` — Shelbi's reuse of the OS/git-configured diff
/// tool — over the review branch's changes: `-d` (dir-diff, so the whole
/// changeset opens at once) `-y` (no per-file prompt) against the merge base of
/// the project's base branch and `HEAD`, exactly the range `shelbi diff`
/// reports. The GUI variant (`-g`) is used when a `diff.guitool` is configured.
///
/// The diff tool is resolved *before* the pane is spawned so an unconfigured or
/// unusable tool surfaces as an `Err` on the panel's status line rather than a
/// dead pane in the middle column (see [`resolve_difftool_gui`]).
fn ensure_diff_pane(
    project: &shelbi_core::Project,
    ws: &shelbi_core::WorkspaceSpec,
    session: &str,
) -> Result<String> {
    if let Some(existing) = read_session_var(session, DIFF_KEY) {
        return Ok(existing);
    }
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let worktree = crate::workspace::workspace_worktree(machine, ws);
    // Resolve the configured diff tool up front: a missing/unusable one is a
    // clear error here, not a pane that flashes a git message and dies.
    let use_gui = resolve_difftool_gui(&worktree)?;
    // Diff the branch against where it forked from the base branch — the same
    // `merge-base(base, HEAD)..HEAD` range `shelbi diff` shows.
    let base = git_capture(&worktree, &["merge-base", project.base_branch(), "HEAD"])?;
    let gui = if use_gui { "-g " } else { "" };
    // `cd <worktree> && exec git difftool …` — exec so the pane dies with the
    // diff tool (as the editor pane dies with the editor) rather than dropping
    // to a shell. `-d` opens the whole changeset at once; `-y` skips the
    // per-file prompt.
    let cmd = format!(
        "cd {wt} && exec git difftool -d -y {gui}{base} HEAD",
        wt = shelbi_core::shell_escape(&worktree.to_string_lossy()),
        base = shelbi_core::shell_escape(&base),
    );
    ensure_stash_session(project, session)?;
    let stash = format!("_{session}");
    // Its own window in the stash (like the editor's) so closing it on teardown
    // leaves the `views` panes untouched.
    let win = format!("{stash}:__review-diff");
    let diff_id = tmux_capture(&[
        "new-window",
        "-d",
        "-P",
        "-F",
        "#{pane_id}",
        "-t",
        &format!("{stash}:"),
        "-n",
        "__review-diff",
        "sh",
        "-c",
        &cmd,
    ])
    .map_err(|e| Error::Other(format!("could not open diff window {win}: {e}")))?;
    set_session_var(session, DIFF_KEY, &diff_id)?;
    Ok(diff_id)
}

/// Decide how to invoke `git difftool` for `worktree`, returning `true` when a
/// GUI tool (`-g`) should be used and `false` for the terminal tool. Errors
/// with a clear, actionable message when no diff tool is configured at all —
/// exactly the state `git difftool` itself would reject — so the caller can put
/// it on the status line instead of spawning a pane that immediately dies.
///
/// Precedence mirrors what `git difftool` consults: a `diff.guitool` /
/// `merge.guitool` (preferred for a desktop review) selects the GUI path; a
/// `diff.tool` / `merge.tool` selects the terminal path; nothing configured is
/// the error.
fn resolve_difftool_gui(worktree: &std::path::Path) -> Result<bool> {
    if git_config_value(worktree, "diff.guitool")
        .or_else(|| git_config_value(worktree, "merge.guitool"))
        .is_some()
    {
        return Ok(true);
    }
    if git_config_value(worktree, "diff.tool")
        .or_else(|| git_config_value(worktree, "merge.tool"))
        .is_some()
    {
        return Ok(false);
    }
    Err(Error::Other(
        "no diff tool configured — set git `diff.tool` (or `diff.guitool`) to your preferred diff tool"
            .into(),
    ))
}

/// Run `git -C <worktree> <args>` and return trimmed stdout, mapping a non-zero
/// exit to an `Err` carrying git's stderr. Used for the merge-base lookup where
/// a failure should surface, not be swallowed.
fn git_capture(worktree: &std::path::Path, args: &[&str]) -> Result<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(args)
        .output()
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "git {} failed: {}",
            args.first().copied().unwrap_or(""),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Read a single git config value from `worktree`, returning `None` when the
/// key is unset (a non-zero `git config --get` exit) or blank. Used to probe
/// for a configured diff tool without treating "unset" as a hard error.
fn git_config_value(worktree: &std::path::Path, key: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["config", "--get", key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// Make sure the hidden stash session `_{session}` exists before we park the
/// editor window in it. Bootstrap's [`crate::ensure_hidden_views`] builds it,
/// but the editor view can be opened before/without a full dashboard rebuild,
/// so if the stash is missing we build it whole (which seeds its `views`
/// window) rather than failing. A bare `new-session` here would leave the
/// stash without the `views` window the heal path later assumes, so we route
/// through the same ensure path the bootstrap uses. Local-only: remote review
/// slots never reach here (they degrade to focusing their window).
fn ensure_stash_session(project: &shelbi_core::Project, session: &str) -> Result<()> {
    let stash = format!("_{session}");
    if tmux_run(&["has-session", "-t", &stash]).is_ok() {
        return Ok(());
    }
    let shelbi_bin = crate::current_exe_string()?;
    crate::ensure_hidden_views(&Host::Local, session, &project.name, &shelbi_bin)
}

/// Tear the review interface back down: bring the agent/chat pane back to the
/// review window's content slot (if the editor is showing), kill the
/// review-panel and editor panes, clear the `SHELBI_REVIEW_*` session vars, and
/// return focus to the dashboard. The review window is left with just its agent
/// pane, so a later visit to the (now interface-free) review slot shows just
/// that pane. Closing one review window touches only its own panes — other
/// review windows' panels and the dashboard sidebar are untouched. Idempotent
/// and best-effort — a missing var means nothing to undo.
pub fn close_review_interface(project_name: &str) -> Result<()> {
    let session = format!("shelbi-{project_name}");
    let dashboard = format!("{session}:dashboard");

    let chat = read_session_var(&session, CHAT_KEY);
    let mid = read_session_var(&session, MID_KEY);

    // If the editor is currently in the middle, swap the chat pane back so the
    // agent pane returns to the review window before the editor's stash-session
    // window is destroyed by the kill below.
    if let (Some(chat), Some(mid)) = (chat.as_deref(), mid.as_deref()) {
        if mid != chat {
            let _ = tmux_run(&["swap-pane", "-s", chat, "-t", mid]);
        }
    }
    // Kill the editor pane. It is the sole pane of its own `__review-editor`
    // window in the hidden stash session, so this closes that window only —
    // the stash session and its `views` panes (swapped out elsewhere) survive.
    if let Some(editor) = read_session_var(&session, EDITOR_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &editor]);
    }
    // The diff pane, like the editor, is the sole pane of its own
    // `__review-diff` window in the hidden stash session — killing it closes
    // that window only.
    if let Some(diff) = read_session_var(&session, DIFF_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &diff]);
    }
    if let Some(panel) = read_session_var(&session, PANEL_KEY) {
        let _ = tmux_run(&["kill-pane", "-t", &panel]);
    }
    for key in [MID_KEY, PANEL_KEY, EDITOR_KEY, DIFF_KEY, CHAT_KEY, TASK_KEY] {
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
/// card sent one column right.
///
/// The accept edge's `merge` action is GATED before the column move: the
/// branch is integrated (via its open PR — the path a protected `main`
/// accepts) and a `merge` event emitted BEFORE the card leaves `review`, so
/// a failed or absent merge never leaves the board reading `done` with an
/// open PR / unmerged branch. On merge failure the move is abandoned and the
/// error propagated so the reviewer sees it. The edge's remaining cleanup
/// (`delete_branch`, review-workspace teardown, slot free) fires after the
/// move — the teardown half is [`close_review_window`], called by the caller.
pub fn approve_review_task(project_name: &str, task_id: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;
    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    let from_status = tf.task.column.as_str().to_string();
    let target = workflow
        .forward_status(&from_status)
        .map(|s| Column::from_status_id(&s.id))
        .ok_or_else(|| {
            Error::Other(format!(
                "no accept transition out of status `{from_status}` in this workflow"
            ))
        })?;
    let to_status = target.as_str().to_string();

    // Gate the move on the accept edge's `merge` (a no-op for an edge that
    // declares none). A failed merge aborts the accept — the task stays in
    // review with the error surfaced — rather than advancing to done with an
    // open PR.
    let ws_label = tf
        .task
        .assigned_to
        .clone()
        .unwrap_or_else(|| "board".to_string());
    let merged = crate::transition::run_gated_merge(
        &project,
        project_name,
        &tf.task,
        &tf.body,
        &workflow,
        &from_status,
        &to_status,
        &ws_label,
    )?
    .is_some();

    if let Some((from, to, wf)) = shelbi_state::move_task(project_name, task_id, target)? {
        let _ = shelbi_state::append_task_event(project_name, task_id, &wf, from, to, "user:review");
    }

    // Fire the edge's remaining actions (delete_branch, …) after the move,
    // skipping `merge` so it isn't re-run. Best-effort — the move landed.
    if merged {
        if let Err(e) = crate::transition::execute_transition_except(
            &project,
            project_name,
            &tf.task,
            &tf.body,
            &workflow,
            &from_status,
            &to_status,
            &[shelbi_core::TransitionAction::Merge],
        ) {
            tracing::warn!(task = %task_id, error = %e, "post-merge accept cleanup failed (merge already landed)");
        }
    }
    Ok(())
}

/// Close the review slot's tmux window after acceptance so the slot returns to
/// idle instead of lingering with just its agent pane.
///
/// The accept signal ([`approve_review_task`]) must have fired **first** — this
/// is the teardown half, mirroring the dev-workspace ordering (branch promoted
/// before the pane closes) so nothing is stranded by the close. Scoped to the
/// **review-tagged** slot the accepted task is assigned to (`move_task` leaves
/// `assigned_to` intact, so it still resolves here); a task on a non-review slot
/// or with no assignment is a no-op, leaving dev-workspace teardown and the
/// Reject flow untouched.
///
/// Routes through [`crate::workspace::kill_workspace_pane`] rather than a bare
/// `kill-window` so the expected-teardown mark is set (the review agent pane's
/// lifecycle wrapper won't emit a spurious `pane_alive=false reason=signal:SIGHUP`)
/// and every window bound to the slot is reaped — the invariant that keeps a
/// half-torn-down review window from resurfacing as an `orphaned session`.
/// Best-effort: closing one review slot's window touches only that slot, so
/// other review windows and the dashboard sidebar are left alone.
pub fn close_review_window(project_name: &str, task_id: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;
    let Some(ws) = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|name| project.workspace(name))
        .filter(|w| project.effective_tags(w).contains("review"))
        .cloned()
    else {
        // Not a review slot (or no assignment): nothing of ours to close.
        return Ok(());
    };
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let addr = crate::workspace::workspace_tmux_addr(&project, &ws)?;
    crate::workspace::kill_workspace_pane(&machine.host(), &addr, &ws.name)
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
        let keys = [MID_KEY, PANEL_KEY, EDITOR_KEY, DIFF_KEY, CHAT_KEY, TASK_KEY];
        let mut sorted = keys.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), keys.len(), "review session keys must be unique");
    }

    // -- close_review_window (accept teardown) ------------------------------

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    /// A hub project named `name` with one dev slot (`alpha`, untagged) and one
    /// `review`-tagged slot (`review-1`), so the accept-teardown scoping can be
    /// exercised against both.
    fn demo_project(name: &str) -> shelbi_core::Project {
        use shelbi_core::*;
        let mut runners = std::collections::BTreeMap::new();
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
            name: name.into(),
            label: None,
            display_name: None,
            repo: "git@example:demo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp/demo".into(),
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
            workspaces: vec![
                WorkspaceSpec {
                    name: "alpha".into(),
                    machine: "hub".into(),
                    tags: Vec::new(),
                    slot: None,
                },
                WorkspaceSpec {
                    name: "review-1".into(),
                    machine: "hub".into(),
                    tags: vec!["review".into()],
                    slot: None,
                },
            ],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            detected_shapes: Vec::new(),
        }
    }

    fn task_on(id: &str, slot: &str, column: Column) -> shelbi_core::Task {
        let now = chrono::Utc::now();
        shelbi_core::Task {
            id: id.into(),
            title: id.into(),
            column,
            priority: 0,
            assigned_to: Some(slot.into()),
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: std::collections::BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn add_window(session: &str, name: &str) {
        let ok = std::process::Command::new("tmux")
            .args([
                "new-window",
                "-d",
                "-t",
                &format!("={session}:"),
                "-n",
                name,
                "sh",
                "-c",
                "sleep 600",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        assert!(ok, "failed to create window `{name}` in `{session}`");
    }

    /// The liveness guard behind the reuse path: `panel_pane_live` is true only
    /// while a pane with that id is actually present **in the review window**.
    /// A killed panel (the run-once crash the fix targets), a bogus id, and a
    /// live pane living in a *different* window all read as not-live, so the
    /// reuse path (re)spawns instead of re-focusing a blank review sidebar.
    #[test]
    fn panel_pane_live_tracks_the_review_window_pane() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let _lock = crate::test_lock::acquire();
        crate::tmux_test_support::use_private_tmux_server();

        let session = format!("shelbi-panel-live-{}", std::process::id());
        crate::tmux_test_support::start_session(&session, "holder");
        add_window(&session, "review-1");
        add_window(&session, "other");

        // Split a "panel" pane into the review window and capture its id.
        let review_target = format!("{session}:review-1");
        let panel_id = std::process::Command::new("tmux")
            .args([
                "split-window", "-h", "-d", "-t", &review_target, "-P", "-F", "#{pane_id}", "sh",
                "-c", "sleep 600",
            ])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .expect("split a panel pane into the review window");
        assert!(!panel_id.is_empty(), "captured a panel pane id");

        // Live in the review window => true; a bogus id => false.
        assert!(
            panel_pane_live(&session, "review-1", &panel_id),
            "a live pane in the review window reads as live"
        );
        assert!(
            !panel_pane_live(&session, "review-1", "%999999"),
            "a bogus pane id reads as not-live"
        );

        // The same live pane, queried against a *different* window, reads as
        // not-live — the guard is window-scoped so a stranded panel (wrong
        // target) triggers a (re)spawn rather than a false reuse.
        assert!(
            !panel_pane_live(&session, "other", &panel_id),
            "a pane live elsewhere is not-live for the review window target"
        );

        // Kill the panel pane (the run-once crash): the guard now reads not-live.
        let _ = std::process::Command::new("tmux")
            .args(["kill-pane", "-t", &panel_id])
            .status();
        assert!(
            !panel_pane_live(&session, "review-1", &panel_id),
            "a crashed/killed panel pane is detected as not-live"
        );

        crate::tmux_test_support::kill_session(&session);
    }

    /// Accepting a review item closes *only* the review-tagged slot's window,
    /// and only for the accepted task — the dev slot and any other window are
    /// left standing. Locks the scoping half of the accept-teardown fix.
    #[test]
    fn close_review_window_reaps_only_the_review_slot() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let _lock = crate::test_lock::acquire();
        crate::tmux_test_support::use_private_tmux_server();

        let proj = format!("review-ui-{}", std::process::id());
        let home = std::env::temp_dir().join(format!("shelbi-review-ui-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var("SHELBI_HOME").ok();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_project(&demo_project(&proj)).unwrap();
        shelbi_state::save_task(&proj, &task_on("t-dev", "alpha", Column::in_progress()), "body")
            .unwrap();
        shelbi_state::save_task(&proj, &task_on("t-rev", "review-1", Column::review()), "body")
            .unwrap();

        // Stand up the project session with a long-lived holder plus the dev and
        // review windows the addrs resolve to (`shelbi-<proj>:<slot>`).
        let session = format!("shelbi-{proj}");
        crate::tmux_test_support::start_session(&session, "holder");
        add_window(&session, "alpha");
        add_window(&session, "review-1");

        let host = Host::Local;
        let alpha_addr = shelbi_core::TmuxAddr {
            session: session.clone(),
            window: "alpha".into(),
        };
        let review_addr = shelbi_core::TmuxAddr {
            session: session.clone(),
            window: "review-1".into(),
        };
        assert!(crate::workspace::workspace_pane_alive(&host, &alpha_addr).unwrap());
        assert!(crate::workspace::workspace_pane_alive(&host, &review_addr).unwrap());

        // Accepting a task on the DEV slot is a no-op — this path only tears down
        // review-tagged slots, so the dev window (and the review one) survive.
        close_review_window(&proj, "t-dev").unwrap();
        assert!(
            crate::workspace::workspace_pane_alive(&host, &alpha_addr).unwrap(),
            "dev-slot accept must not close the dev window"
        );
        assert!(
            crate::workspace::workspace_pane_alive(&host, &review_addr).unwrap(),
            "dev-slot accept must not touch the review window"
        );

        // Accepting the review task closes its review slot's window — and only
        // that window; the dev slot is left standing.
        close_review_window(&proj, "t-rev").unwrap();
        assert!(
            !crate::workspace::workspace_pane_alive(&host, &review_addr).unwrap(),
            "review accept must close the review slot's window"
        );
        assert!(
            crate::workspace::workspace_pane_alive(&host, &alpha_addr).unwrap(),
            "closing one review window must not disturb the dev slot"
        );

        crate::tmux_test_support::kill_session(&session);
        match prev_home {
            Some(h) => std::env::set_var("SHELBI_HOME", h),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
