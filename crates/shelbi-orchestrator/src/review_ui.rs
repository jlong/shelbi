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
use shelbi_core::{Column, Error, Result};

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





















// ---------------------------------------------------------------------------
// Open / switch / close




















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

/// Build the `sh -c` command string the diff window runs, for either the
/// default `git difftool` path or a `review.diff_command` override.
///
/// * **Override** (`override_cmd = Some`): the template's `{worktree}` /
///   `{base}` / `{head}` placeholders are substituted (shell-escaped) and the
///   result is run as-is — the escape hatch for a diff tool that reviews a
///   **revision range** (`skim {base} {head}`) instead of git's `--dir-diff`
///   directory pair, which such a tool cannot interpret.
/// * **Default** (`override_cmd = None`): `git difftool -d -y [-g] <base> HEAD`
///   — `-d` opens the whole changeset, `-y` skips the per-file prompt, `-g`
///   selects the GUI tool when [`resolve_difftool_gui`] chose it.
///
/// Either invocation is wrapped so a **non-zero exit** leaves a readable
/// message in the pane and holds it (a `read`) instead of leaving only git's
/// warnings behind — the "tool exited without rendering" case an incompatible
/// tool produces. A clean exit (the reviewer quit the tool) returns 0, the
/// shell falls through, and the pane dies as before, so the panel's
/// [`mid_content_pane_dead`] recovery swaps chat back in.
fn diff_pane_command(
    worktree: &std::path::Path,
    base: &str,
    override_cmd: Option<&str>,
    use_gui: bool,
) -> String {
    let wt = shelbi_core::shell_escape(&worktree.to_string_lossy());
    let invocation = match override_cmd {
        Some(template) => render_diff_command(template, worktree, base),
        None => {
            let gui = if use_gui { "-g " } else { "" };
            format!(
                "git difftool -d -y {gui}{base} HEAD",
                base = shelbi_core::shell_escape(base),
            )
        }
    };
    // `{ …; }` groups the invocation so `$?` is the tool's own exit status; on
    // a non-zero exit we print a plain-language explanation and `read` to keep
    // the pane up so the message is legible (rather than the pane collapsing
    // straight back to chat with only git warnings scrolled past).
    format!(
        "cd {wt} && {{ {invocation}; }}; __st=$?; \
if [ \"$__st\" -ne 0 ]; then \
printf '\\n[shelbi] diff tool exited without rendering a diff (status %s).\\n' \"$__st\"; \
printf 'The configured diff tool may not accept this diff mode; set review.diff_command in project.yaml to a revision-range command (e.g. skim {{base}} {{head}}).\\n'; \
printf 'Press Enter to return to chat.'; read -r __; fi"
    )
}

/// Substitute the `{worktree}` / `{base}` / `{head}` placeholders in a
/// `review.diff_command` template with shell-escaped values. The template is
/// otherwise a literal shell command line (the tool name plus its own args),
/// run as-is. `{head}` is always the literal `HEAD` ref — the same head the
/// default `git difftool` path diffs against — and `{base}` is
/// `merge-base(base_branch, HEAD)`, so `skim {base} {head}` reviews exactly the
/// range `shelbi diff` reports.
fn render_diff_command(template: &str, worktree: &std::path::Path, base: &str) -> String {
    template
        .replace(
            "{worktree}",
            &shelbi_core::shell_escape(&worktree.to_string_lossy()),
        )
        .replace("{base}", &shelbi_core::shell_escape(base))
        .replace("{head}", "HEAD")
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

/// Build the `sh -c` command for a review **editor** content session: `$EDITOR`
/// opened on the review worktree. Shared with the single-process TUI
/// (`rt-tui-review`), which spawns this as a `<project>/review/<slot>/editor`
/// session process instead of a tmux pane; the tmux path builds the identical
/// line inline in [`ensure_editor_pane`]. `exec` so the session ends when the
/// editor quits; the worktree is shell-escaped, the editor command is not (it
/// may carry flags, e.g. `code --wait`).
pub(crate) fn editor_session_command(worktree: &std::path::Path) -> String {
    format!(
        "cd {} && exec {}",
        shelbi_core::shell_escape(&worktree.to_string_lossy()),
        shelbi_state::resolve_editor(),
    )
}

/// Build the `sh -c` command for a review **diff** content session — the same
/// `git difftool` (or `review.diff_command` override) invocation
/// [`ensure_diff_pane`] runs, over `merge-base(base, HEAD)..HEAD`. Shared with
/// the single-process TUI, which spawns it as a `<project>/review/<slot>/diff`
/// session. On the default path the tool is resolved first so an unconfigured
/// `diff.tool` surfaces as an `Err` (a status-line warning) rather than a
/// session that dies on launch.
pub(crate) fn diff_session_command(
    project: &shelbi_core::Project,
    worktree: &std::path::Path,
) -> Result<String> {
    let override_cmd = project.review_diff_command();
    let use_gui = match override_cmd {
        Some(_) => false,
        None => resolve_difftool_gui(worktree)?,
    };
    let base = git_capture(worktree, &["merge-base", project.base_branch(), "HEAD"])?;
    Ok(diff_pane_command(worktree, &base, override_cmd, use_gui))
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
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;
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
    )?;

    if let Some(mv) = store.move_status(task_id, &target, "user:review")? {
        let _ = shelbi_state::append_task_event(
            project_name,
            task_id,
            &mv.workflow,
            mv.from,
            mv.to,
            "user:review",
        );
    }

    // Fire the edge's remaining actions (delete_branch, …) after the move,
    // skipping the actions the gate already ran (the pre-merge prefix plus
    // `merge`) so none is re-run. Best-effort — the move landed.
    if let Some(gm) = &merged {
        if let Err(e) = crate::transition::execute_transition_except(
            &project,
            project_name,
            &tf.task,
            &tf.body,
            &workflow,
            &from_status,
            &to_status,
            &gm.ran,
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
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;
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
    let addr = crate::workspace::workspace_target(&project, &ws)?;
    crate::workspace::kill_workspace_pane(&machine.host(), &addr, &ws.name)?;
    // Drop the freed slot's stale status.yaml so it returns to a clean idle. A
    // killed pane emits no further markers, so the poller can't refresh the file
    // — left in place it would report the review agent's last observed state
    // (frozen) rather than the empty slot it now is.
    shelbi_state::clear_workspace_status(&ws.name)
}

/// **Reject**: append the reviewer's `reason` to the task body as a marked
/// fix section and bounce the task back to the workflow's ready status so
/// normal auto-dispatch picks it back up with the feedback baked into the
/// task description. Emits the move event on the existing channel — the
/// structured signal the orchestrator reacts to — with the reason durably in
/// the task body rather than a transient message.
pub fn reject_review(project_name: &str, task_id: &str, reason: &str) -> Result<()> {
    let date = Utc::now().format("%Y-%m-%d").to_string();
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;

    // Resolve the ready status the card bounces back to from its workflow — the
    // reject mirror of the accept path's `forward_status`. Fall back to the
    // stock `todo` column when the workflow can't be loaded or declares no ready
    // status, so a config hiccup still frees the review slot. Resolving `ready`
    // here (not in the store) keeps the workflow layer out of the backend.
    let ready = shelbi_state::load_task_workflow(project_name, &project, &tf.task)
        .ok()
        .and_then(|wf| wf.ready_status().map(|s| Column::from_status_id(&s.id)))
        .unwrap_or_else(Column::todo);

    if let Some(mv) = store.reject_review(task_id, &ready, reason, &date)? {
        let _ = shelbi_state::append_task_event(
            project_name,
            task_id,
            &mv.workflow,
            mv.from,
            mv.to,
            "user:review-reject",
        );
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Layout state query (for a late-connecting client)

/// One review slot a client should lay out: a review-column task pinned to a
/// review-tagged workspace, with its panel/agent interface expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSlotLayout {
    /// The review slot's workspace name.
    pub workspace: String,
    /// The task loaded on that slot.
    pub task: String,
}

/// The review slots a client should show the interface for, derived purely from
/// durable state (the board + the project's workspace tags) — **no tmux**.
///
/// The layout events ([`shelbi_state::LayoutEvent`]) are deltas the daemon
/// pushes and does not queue, so a client that was not connected when one fired
/// reads this on connect and lays itself out from current state
/// (`rt-daemon-layout-split`; `docs/removing-tmux/phase3-daemon.md`, "No client
/// attached"). It returns every review-column task assigned to a review-tagged
/// slot — the set whose windows should carry the `panel | agent` interface — so
/// a freshly started sidebar builds exactly the panels an already-running one
/// would have, and closes any review window whose slot is absent here.
///
/// Ownership resolves the same way the dispatch/reap passes do: through the
/// local assignment overlay for a remote backend (assignment lives in the hub's
/// overlay, never on the remote), and the card's `assigned_to` for a
/// `file_system` backend.
pub fn review_layout_state(project_name: &str) -> Result<Vec<ReviewSlotLayout>> {
    let project = shelbi_state::load_project(project_name)?;
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let overlay = if project.issue_tracker.backend.is_remote() {
        shelbi_state::task_assignments(project_name).unwrap_or_default()
    } else {
        std::collections::BTreeMap::new()
    };
    let mut out = Vec::new();
    for tf in store.list()? {
        if tf.task.column != Column::review() {
            continue;
        }
        let owner = if project.issue_tracker.backend.is_remote() {
            overlay.get(&tf.task.id).map(String::as_str)
        } else {
            tf.task.assigned_to.as_deref()
        };
        let Some(ws_name) = owner else { continue };
        let Some(ws) = project.workspace(ws_name) else {
            continue;
        };
        if !project.effective_tags(ws).contains("review") {
            continue;
        }
        out.push(ReviewSlotLayout {
            workspace: ws_name.to_string(),
            task: tf.task.id.clone(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- diff pane command construction -------------------------------------

    /// The default (no `review.diff_command`) path still opens the changeset via
    /// `git difftool -d -y <base> HEAD` — the historical behavior AC #2 pins —
    /// run in the worktree, with no `-g` when the terminal tool was chosen.
    #[test]
    fn diff_pane_command_default_uses_git_difftool_dir_diff() {
        let cmd = diff_pane_command(std::path::Path::new("/tmp/wt"), "abc123", None, false);
        assert!(
            cmd.contains("git difftool -d -y abc123 HEAD"),
            "default path is git difftool -d -y <base> HEAD: {cmd}"
        );
        assert!(!cmd.contains(" -g "), "terminal tool gets no -g: {cmd}");
        assert!(cmd.starts_with("cd /tmp/wt &&"), "runs in the worktree: {cmd}");
    }

    /// A configured `diff.guitool` selects the GUI variant (`-g`) on the default
    /// path.
    #[test]
    fn diff_pane_command_default_gui_passes_dash_g() {
        let cmd = diff_pane_command(std::path::Path::new("/tmp/wt"), "abc123", None, true);
        assert!(
            cmd.contains("git difftool -d -y -g abc123 HEAD"),
            "gui path passes -g: {cmd}"
        );
    }

    /// A `review.diff_command` override bypasses `git difftool -d` entirely
    /// (AC #1): the pane runs the substituted template instead, so a
    /// revision-range tool never sees the two directory paths dir-diff would
    /// hand it.
    #[test]
    fn diff_pane_command_override_bypasses_dir_diff() {
        let cmd = diff_pane_command(
            std::path::Path::new("/tmp/wt"),
            "abc123",
            Some("skim {base} {head}"),
            false,
        );
        assert!(cmd.contains("skim abc123 HEAD"), "runs the override tool: {cmd}");
        assert!(
            !cmd.contains("git difftool"),
            "override never falls back to git difftool -d: {cmd}"
        );
    }

    /// The override receives the same `{base}` = `merge-base(base, HEAD)` and
    /// `{head}` = `HEAD` range the pane computes today (AC #3), and `{worktree}`
    /// is the worktree path. Values are shell-escaped so a path/ref with spaces
    /// or metacharacters stays a single, safe argument.
    #[test]
    fn render_diff_command_substitutes_and_shell_escapes_placeholders() {
        let out = render_diff_command(
            "mytool {worktree} {base} {head}",
            std::path::Path::new("/tmp/my wt"),
            "abc123",
        );
        assert_eq!(out, "mytool '/tmp/my wt' abc123 HEAD");
    }

    /// Either invocation is wrapped so a diff tool that exits without rendering
    /// leaves a readable message (and holds the pane) rather than only git
    /// warnings — AC #4.
    #[test]
    fn diff_pane_command_surfaces_a_message_on_nonzero_exit() {
        for override_cmd in [None, Some("skim {base} {head}")] {
            let cmd =
                diff_pane_command(std::path::Path::new("/tmp/wt"), "abc123", override_cmd, false);
            assert!(
                cmd.contains("exited without rendering"),
                "carries a readable failure message: {cmd}"
            );
            assert!(
                cmd.contains("review.diff_command"),
                "points at the escape hatch: {cmd}"
            );
            assert!(
                cmd.contains("read -r"),
                "holds the pane so the message is legible: {cmd}"
            );
        }
    }







    // -- review_layout_state (the late-connecting client's state query) -----

    /// A client that connects after the layout events have fired reads
    /// [`review_layout_state`] and lays itself out from current state — no tmux
    /// involved. It must return exactly the review-column tasks pinned to a
    /// review-tagged slot, and nothing else: not a dev-column task, not a done
    /// task, and not a review-column task parked on a non-review slot.
    #[test]
    fn review_layout_state_lists_only_review_column_tasks_on_review_slots() {
        let _lock = crate::test_lock::acquire();
        let proj = format!("review-layout-state-{}", std::process::id());
        let home = std::env::temp_dir().join(format!("shelbi-rls-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var("SHELBI_HOME").ok();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_project(&demo_project(&proj)).unwrap();
        // The one slot a client should lay out: a review-column task on the
        // review-tagged slot.
        shelbi_state::save_task(&proj, &task_on("t-rev", "review-1", Column::review()), "b").unwrap();
        // Excluded: a dev-column task on the dev slot…
        shelbi_state::save_task(&proj, &task_on("t-dev", "alpha", Column::in_progress()), "b").unwrap();
        // …a finished task still pinned to the review slot (it left review)…
        shelbi_state::save_task(&proj, &task_on("t-done", "review-1", Column::done()), "b").unwrap();
        // …and a review-column task parked on a NON-review slot (its window is a
        // plain dev pane, never the embedded interface).
        shelbi_state::save_task(&proj, &task_on("t-misowned", "alpha", Column::review()), "b").unwrap();

        let state = review_layout_state(&proj).unwrap();
        assert_eq!(
            state,
            vec![ReviewSlotLayout {
                workspace: "review-1".into(),
                task: "t-rev".into(),
            }],
            "only the review-column task on the review-tagged slot is laid out"
        );

        match prev_home {
            Some(h) => std::env::set_var("SHELBI_HOME", h),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- review_session::close_review (the native TUI teardown, AC #6) ------

    /// Closing a review (the single-process TUI's teardown, `rt-tui-review`)
    /// reaps the review slot's dev-server process group — freeing the port it
    /// held — and clears the pgid record. The editor/diff session kills are
    /// best-effort over sessions that aren't up in this unit test (a no-op), so
    /// this focuses on the server+port half, which is the part with a durable
    /// side effect to assert. (The process-group reap mechanics themselves are
    /// also covered by `workspace::stop_review_server`'s test.)
    #[test]
    fn close_review_reaps_the_dev_server_and_frees_the_port() {
        use std::os::unix::process::CommandExt;

        let _lock = crate::test_lock::acquire();
        let proj = format!("close-review-{}", std::process::id());
        let home = std::env::temp_dir().join(format!("shelbi-closerev-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var("SHELBI_HOME").ok();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_project(&demo_project(&proj)).unwrap();
        // A review-column task loaded on the review-tagged slot `review-1`.
        shelbi_state::save_task(&proj, &task_on("t-rev", "review-1", Column::review()), "b")
            .unwrap();

        // Stand in for the `shelbi __review-serve` launch: a setsid session
        // leader (its own process group) that outlives the call until reaped.
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg("sleep 300");
        cmd.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Safety: async-signal-safe `setsid` in the forked child before exec.
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn().expect("spawn stub server");
        let pgid = child.id() as i32;
        let pgroup_alive = |pgid: i32| unsafe { libc::kill(-pgid, 0) == 0 };

        let path = shelbi_state::review_serve_pgid_path("review-1").unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, format!("{pgid}\n")).unwrap();
        assert!(pgroup_alive(pgid), "server group alive before close");

        // Close the review: the daemon's half ends the sessions and frees the
        // port (reaps the server's process group).
        crate::review_session::close_review(&proj, "t-rev").unwrap();

        let _ = child.wait();
        let mut gone = false;
        for _ in 0..200 {
            if !pgroup_alive(pgid) {
                gone = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(gone, "the dev-server process group must be gone (port freed)");
        assert!(
            shelbi_state::read_review_serve_pgid("review-1")
                .unwrap()
                .is_none(),
            "the pgid record must be cleared after close"
        );

        match prev_home {
            Some(h) => std::env::set_var("SHELBI_HOME", h),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    // -- close_review_window (accept teardown) ------------------------------



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
        Project { session: Default::default(),
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
            github_reconcile_interval_secs: 900,
            workspace_permissions_mode: Some("auto".into()),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            review: shelbi_core::ReviewConfig::default(),
            runners: Default::default(),
            agents: Default::default(),
            issue_tracker: Default::default(),
            detected_shapes: Vec::new(),
        }
    }

    fn task_on(id: &str, slot: &str, column: Column) -> shelbi_core::Issue {
        let now = chrono::Utc::now();
        shelbi_core::Issue {
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



























}
