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

use shelbi_core::{Column, Error, Host, Result};

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

/// Find the in-progress task assigned to `workspace`, read from the **same
/// source the sidebar uses** — the daemon's published board index
/// ([`shelbi_state::read_board_report_with_cfg`]) — not a fresh backend /
/// process-cache read.
///
/// Reading the published index is what keeps the workspace panel's header
/// (status / title / description) consistent with the sidebar badge and the
/// issue board the moment the view opens. A `resolve_issue_store().list_in_status()`
/// read can lag the published index by a tick on a remote board — its
/// process-local snapshot cache is cold on the first read of a freshly launched
/// TUI — which surfaced as an `IDLE` header on the first open while the sidebar
/// already showed the task working, firming up only after a back-out + reopen.
/// The index is a local file, so this adds no backend round trip. The assignment
/// is taken straight from the index (the daemon already folded the overlay at
/// publish), exactly as the sidebar does for its in-progress rows.
fn in_progress_task_for(
    project_name: &str,
    cfg: &shelbi_core::IssueTrackerConfig,
    workspace: &str,
) -> Result<Option<shelbi_state::IssueFile>> {
    let report = shelbi_state::read_board_report_with_cfg(project_name, cfg)?;
    Ok(report
        .state
        .into_issues()
        .into_iter()
        .filter(|tf| tf.task.column == Column::in_progress())
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
    let task = in_progress_task_for(project_name, &project.issue_tracker, workspace)?;
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
    let task = in_progress_task_for(project_name, &project.issue_tracker, workspace)
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

    use shelbi_core::{GithubConnection, IssueTrackerBackend, IssueTrackerConfig};

    fn fresh_shelbi_home(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-ws-session-test-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A GitHub-backend tracker config pointed at `owner/repo` — the remote case
    /// where the published-index-vs-live-read divergence shows up (a
    /// `file_system` board reads the same bytes either way).
    fn github_cfg() -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection {
                repo: "owner/repo".into(),
            }),
            ..Default::default()
        }
    }

    fn issue(id: &str, column: shelbi_core::Column, assigned_to: Option<&str>) -> shelbi_state::IssueFile {
        let now = chrono::Utc::now();
        shelbi_state::IssueFile {
            task: shelbi_core::Issue {
                id: id.into(),
                title: id.into(),
                column,
                priority: 0,
                assigned_to: assigned_to.map(str::to_string),
                workflow: None,
                branch: Some(format!("shelbi/{id}")),
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: now,
                updated_at: now,
                params: std::collections::BTreeMap::new(),
            },
            body: format!("body of {id}"),
            tracker_assignees: Vec::new(),
        }
    }

    /// Publish a warm board index for `proj` carrying `board`, stamped with the
    /// identity a `github` config expects so the reader trusts the file.
    fn publish_index(board: Vec<shelbi_state::IssueFile>) {
        let mut idx = shelbi_state::BoardIndex::fresh(board);
        idx.repo = Some(shelbi_state::github_board_repo("owner/repo"));
        shelbi_state::write_board_index("proj", &idx).unwrap();
    }

    /// The first-open regression: the panel's task (header status / title /
    /// description) is resolved from the daemon's **published board index** — the
    /// same source the sidebar badge reads — so a working workspace resolves its
    /// in-progress task on the first open. A GitHub backend's own
    /// `resolve_issue_store().list_in_status()` read would miss here (no live
    /// backend / a cold process cache), which is exactly the "IDLE on first open"
    /// bug: reading the index can't regress to that.
    #[test]
    fn resolves_the_in_progress_task_from_the_published_index() {
        let _g = crate::test_lock::acquire();
        let home = fresh_shelbi_home("published-index");
        std::env::set_var("SHELBI_HOME", &home);

        publish_index(vec![issue(
            "cold-start",
            shelbi_core::Column::in_progress(),
            Some("alpha"),
        )]);

        let task = in_progress_task_for("proj", &github_cfg(), "alpha").unwrap();
        std::env::remove_var("SHELBI_HOME");
        let task = task.expect("the in-progress task is resolved from the published index");
        assert_eq!(task.task.id, "cold-start");
        assert_eq!(task.task.column, shelbi_core::Column::in_progress());
        assert_eq!(task.body, "body of cold-start");
    }

    /// Only an in-progress task assigned to *this* workspace counts: a task in
    /// another column, or one assigned to another workspace, is not the panel's
    /// task (so a truly idle workspace resolves `None` → an `IDLE` header).
    #[test]
    fn ignores_other_columns_and_other_workspaces() {
        let _g = crate::test_lock::acquire();
        let home = fresh_shelbi_home("filters");
        std::env::set_var("SHELBI_HOME", &home);

        publish_index(vec![
            // In review, not in progress — not the dev workspace's running task.
            issue("in-review", shelbi_core::Column::review(), Some("alpha")),
            // In progress but assigned to a different workspace.
            issue("other-ws", shelbi_core::Column::in_progress(), Some("bravo")),
        ]);

        let for_alpha = in_progress_task_for("proj", &github_cfg(), "alpha").unwrap();
        let for_idle = in_progress_task_for("proj", &github_cfg(), "charlie").unwrap();
        std::env::remove_var("SHELBI_HOME");
        assert!(for_alpha.is_none(), "a review-column task is not the running task");
        assert!(for_idle.is_none(), "an unassigned workspace has no running task");
    }
}
