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
//! They work on any machine: a remote workspace's editor and diff are spawned
//! on that machine over SSH, in its worktree, and the client reaches them
//! through the machine's relay, as it does the remote agent
//! ([`crate::session_connect`]).
//!
//! The workspace's **user shell** (`<project>/ws/<workspace>/shell`, the
//! desktop Workspace view's split beside the agent, `dt-workspace-view`) is
//! managed here too, [`ensure_user_shell`] / [`close_user_shell`], and works on
//! any machine: a remote workspace's shell is spawned there over SSH and the
//! client reaches it through the machine's relay. It never occupies the
//! workspace's slot (that is `<project>/ws/<workspace>`), so dispatch ignores it.

use std::path::Path;

use shelbi_core::{Error, Host, Result};

use crate::review_session::ReviewContentRole;
use crate::session_process_backend::SessionProcessBackend;

/// The session name a dev workspace's `role` content session is spawned and
/// looked up under: `<project>/ws/<workspace>/<role>`. Pure, so the daemon's
/// spawn and the client's attach agree without a shared helper crossing the
/// wire.
pub fn content_session_name(project_name: &str, workspace: &str, role: ReviewContentRole) -> String {
    format!("{project_name}/ws/{workspace}/{}", role.as_str())
}

/// Find the task working on `workspace` (an in-progress card or a running gate),
/// read from the **same source the sidebar uses** — the daemon's published board
/// index through [`shelbi_state::BoardOccupancy`] — not a fresh backend /
/// process-cache read.
///
/// Reading the published index is what keeps the workspace panel's header
/// (status / title / description) consistent with the sidebar badge and the
/// issue board the moment the view opens. A `resolve_issue_store().list_in_status()`
/// read can lag the published index by a tick on a remote board — its
/// process-local snapshot cache is cold on the first read of a freshly launched
/// TUI — which surfaced as an `IDLE` header on the first open while the sidebar
/// already showed the task working, firming up only after a back-out + reopen.
/// The index is a local file, so this adds no backend round trip.
fn working_task_for(
    project: &shelbi_core::Project,
    workspace: &str,
) -> Result<Option<shelbi_state::IssueFile>> {
    let occupancy = shelbi_state::BoardOccupancy::read(project)?;
    let task = occupancy
        .working()
        .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace))
        .cloned();
    Ok(task)
}

/// What a workspace view shows about a workspace; the type lives in
/// `shelbi_app::source` so a project source can return it.
pub use shelbi_app::source::WorkspaceDetails;

/// Resolve a workspace's [`WorkspaceDetails`], local or remote. Errors only
/// when the workspace or its machine is unknown. Blocking (a config load, the
/// board index, and for a local idle workspace one `git rev-parse`).
pub fn workspace_details(project_name: &str, workspace: &str) -> Result<WorkspaceDetails> {
    let project = shelbi_state::load_project(project_name)?;
    let ws = project
        .workspace(workspace)
        .ok_or_else(|| Error::Other(format!("unknown workspace `{workspace}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let host = machine.host();
    let worktree = crate::workspace::workspace_worktree(machine, ws);
    let worktree_str = worktree.to_string_lossy().to_string();
    let (editor_name, apps, remote_address) = match &host {
        Host::Ssh { host } => (
            shelbi_state::ToolChoice::Command(shelbi_state::resolve_editor()).display_name(),
            remote_tools(crate::tools::app_tools(&project)),
            Some(format!("{host}:{worktree_str}")),
        ),
        Host::Local => (
            shelbi_state::resolved_editor_name(),
            crate::tools::app_tools(&project),
            None,
        ),
    };
    let task = working_task_for(&project, workspace)?;
    let branch = task
        .as_ref()
        .and_then(|t| t.task.branch.clone())
        .or_else(|| crate::workspace::worktree_current_branch(&host, &worktree));
    Ok(WorkspaceDetails {
        machine: ws.machine.clone(),
        is_remote: !host.is_local(),
        worktree: worktree_str,
        remote_address,
        editor_name,
        apps,
        branch,
        task: task.map(Box::new),
    })
}

/// Wrap a content session's `command` for a remote machine: run it under the
/// remote user's login shell, so the editor or diff tool is found on the
/// `PATH` it has there (a detached session starts with a bare environment).
fn remote_content_argv(command: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        format!(
            "exec \"${{SHELL:-/bin/sh}}\" -lc {}",
            shelbi_agent::shell_escape(command)
        ),
    ]
}

/// Ensure the `role` content session for `workspace` is spawned and live, on
/// the workspace's own machine, so a client can attach a terminal view to it.
/// Idempotent: a session already live under the name is left as is. Builds
/// the same editor / diff command the review slot's content sessions run (see
/// [`crate::review_ui::editor_session_command`] /
/// [`diff_session_command`](crate::review_ui)); a remote workspace's diff
/// resolves its merge-base there
/// ([`remote_diff_session_command`](crate::review_ui)).
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
    let host = machine.host();
    let worktree = crate::workspace::workspace_worktree(machine, ws);

    let name = content_session_name(project_name, workspace, role);
    let backend = SessionProcessBackend;
    // Idempotent: a live session under this name is reused as is.
    if backend
        .live_session_names(&host)
        .map(|names| names.iter().any(|n| n == &name))
        .unwrap_or(false)
    {
        return Ok(());
    }

    let command = match (role, host.is_ssh()) {
        (ReviewContentRole::Editor, _) => crate::review_ui::editor_session_command(&worktree),
        (ReviewContentRole::Diff, false) => crate::review_ui::diff_session_command(&project, &worktree)?,
        (ReviewContentRole::Diff, true) => crate::review_ui::remote_diff_session_command(
            &project,
            &worktree,
            shelbi_state::resolve_diff_tool().as_ref(),
        )?,
    };
    // Tag the session with the current task id when there is one (best-effort —
    // the editor/diff work regardless).
    let task = working_task_for(&project, workspace)
        .ok()
        .flatten()
        .map(|tf| tf.task.id);
    let (cols, rows) = SessionProcessBackend::default_size();
    if host.is_ssh() {
        // The command `cd`s into the worktree itself; the session starts in
        // the machine's `work_dir` (as the remote user shell does).
        let spec = shelbi_session::SpawnSpec {
            name,
            cwd: machine.work_dir.clone(),
            cols,
            rows,
            task,
            raw_output_log: false,
            child_argv: remote_content_argv(&command),
        };
        let bin = crate::remote_session::resolve_remote_bin(&ws.machine)?;
        return crate::remote_session::spawn_remote_session(&host, &bin, &spec);
    }
    let spec = shelbi_session::SpawnSpec {
        name,
        cwd: worktree,
        cols,
        rows,
        task,
        raw_output_log: false,
        child_argv: vec!["sh".into(), "-c".into(), command],
    };
    backend.spawn_session_to_attach(spec)
}

/// Open `workspace`'s worktree (the editor) or its branch diff in the app
/// chosen for `role` (`editor:` / `diff_tool:`), returning the status line
/// to show. A remote workspace is refused with the reason (an app opens
/// local worktrees only). Blocking, so call it off the UI thread.
pub fn open_in_app(project_name: &str, workspace: &str, role: ReviewContentRole) -> Result<String> {
    let project = shelbi_state::load_project(project_name)?;
    let ws = project
        .workspace(workspace)
        .ok_or_else(|| Error::Other(format!("unknown workspace `{workspace}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    if machine.host() != Host::Local {
        let cfg = shelbi_state::load_user_config().unwrap_or_default();
        return Err(Error::Other(crate::tools::remote_refusal(role, &cfg, &ws.machine)));
    }
    let worktree = crate::workspace::workspace_worktree(machine, ws);
    crate::tools::open_in_app(role, &worktree, project.base_branch())
}

/// What a workspace on another machine can use of `tools`: no app can open its
/// worktree, while Shelbi's own diff view reads any machine's.
fn remote_tools(tools: shelbi_state::AppTools) -> shelbi_state::AppTools {
    shelbi_state::AppTools { builtin_diff: tools.builtin_diff, ..Default::default() }
}

/// End `workspace`'s editor and diff content sessions, on whichever machine
/// they run. Idempotent and best-effort — an already-gone session is fine. The
/// agent session is left for the workspace's normal teardown; a dev workspace
/// runs no review server, so there is nothing else to reap.
pub fn close_content(project_name: &str, workspace: &str) -> Result<()> {
    let host = workspace_host(project_name, workspace).unwrap_or(Host::Local);
    let backend = SessionProcessBackend;
    for role in [ReviewContentRole::Editor, ReviewContentRole::Diff] {
        let name = content_session_name(project_name, workspace, role);
        let _ = backend.kill_by_name(&host, &name);
    }
    Ok(())
}

/// The host `workspace` lives on, or `None` when it (or its machine) is
/// unknown.
fn workspace_host(project_name: &str, workspace: &str) -> Option<Host> {
    let project = shelbi_state::load_project(project_name).ok()?;
    let ws = project.workspace(workspace)?;
    Some(project.machine(&ws.machine)?.host())
}

/// The role token of a workspace's user shell, the last segment of its
/// session name.
pub const SHELL_ROLE: &str = "shell";

/// The session name of `workspace`'s user shell:
/// `<project>/ws/<workspace>/shell`, beside the editor and diff content
/// sessions (and, like them, distinct from the agent's slot session).
pub fn shell_session_name(project_name: &str, workspace: &str) -> String {
    format!("{project_name}/ws/{workspace}/{SHELL_ROLE}")
}

/// Whether the session `name` is a workspace's user shell
/// ([`shell_session_name`]), and not an agent in a workspace called `shell`.
pub(crate) fn is_user_shell_session(name: &str) -> bool {
    name.rsplit_once("/ws/")
        .and_then(|(_, rest)| rest.split_once('/'))
        .is_some_and(|(_, role)| role == SHELL_ROLE)
}

/// What a user shell runs, and where: `(cwd, argv)`. A local shell starts in
/// the worktree (the machine's `work_dir` when the worktree is gone, as after a
/// handoff) as a login shell of the daemon's `$SHELL`. A remote one can't test
/// the remote filesystem from here, so it starts in `work_dir` and `cd`s into
/// the worktree on the remote, then execs the *remote* login shell.
fn user_shell_command(
    remote: bool,
    worktree: &Path,
    work_dir: &Path,
    local_shell: &str,
    worktree_exists: bool,
) -> (std::path::PathBuf, Vec<String>) {
    if remote {
        let script = format!(
            "cd {wt} 2>/dev/null || cd {wd}; exec \"${{SHELL:-/bin/sh}}\" -l",
            wt = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            wd = shelbi_agent::shell_escape(&work_dir.to_string_lossy()),
        );
        (work_dir.to_path_buf(), vec!["sh".into(), "-c".into(), script])
    } else {
        let cwd = if worktree_exists { worktree } else { work_dir };
        (cwd.to_path_buf(), vec![local_shell.to_string(), "-l".into()])
    }
}

/// Ensure `workspace`'s user shell is spawned and live, on the workspace's own
/// machine, so a client can attach a terminal to it. Idempotent: a shell
/// already live under the name is reused. Untagged (no task), so it is never
/// mistaken for the workspace's agent.
pub fn ensure_user_shell(project_name: &str, workspace: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let ws = project
        .workspace(workspace)
        .ok_or_else(|| Error::Other(format!("unknown workspace `{workspace}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let host = machine.host();
    let name = shell_session_name(project_name, workspace);
    let backend = SessionProcessBackend;
    if backend
        .live_session_names(&host)
        .map(|names| names.iter().any(|n| n == &name))
        .unwrap_or(false)
    {
        return Ok(());
    }
    let worktree = crate::workspace::workspace_worktree(machine, ws);
    let local_shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let (cwd, child_argv) = user_shell_command(
        host.is_ssh(),
        &worktree,
        &machine.work_dir,
        &local_shell,
        host.is_local() && worktree.is_dir(),
    );
    let (cols, rows) = SessionProcessBackend::default_size();
    let spec = shelbi_session::SpawnSpec {
        name,
        cwd,
        cols,
        rows,
        task: None,
        raw_output_log: false,
        child_argv,
    };
    if host.is_ssh() {
        let bin = crate::remote_session::resolve_remote_bin(&ws.machine)?;
        crate::remote_session::spawn_remote_session(&host, &bin, &spec)
    } else {
        backend.spawn_session_to_attach(spec)
    }
}

/// SIGHUP: what a terminal sends its shell when it closes. An interactive
/// shell ignores the session's default SIGTERM.
pub(crate) const SIGHUP: i32 = 1;

/// End `workspace`'s user shell, on whichever machine it runs, as closing its
/// terminal would (SIGHUP, which the shell passes on to its jobs). Idempotent
/// and best-effort: an already-gone shell is fine.
pub fn close_user_shell(project_name: &str, workspace: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let ws = project
        .workspace(workspace)
        .ok_or_else(|| Error::Other(format!("unknown workspace `{workspace}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    SessionProcessBackend.signal_by_name(
        &machine.host(),
        &shell_session_name(project_name, workspace),
        Some(SIGHUP),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remote_workspace_keeps_the_built_in_diff_and_no_app() {
        let tools = shelbi_state::AppTools { editor: true, diff: true, builtin_diff: false };
        assert_eq!(remote_tools(tools), shelbi_state::AppTools::default());
        let tools = shelbi_state::AppTools { editor: true, diff: false, builtin_diff: true };
        assert_eq!(remote_tools(tools), shelbi_state::AppTools { builtin_diff: true, ..Default::default() });
    }

    /// `WorkspaceDetails` is a remote-window wire type: pin its JSON shape,
    /// decode it back equal, and decode `{}` (every field defaults).
    #[test]
    fn workspace_details_json_shape_is_pinned() {
        let task: shelbi_state::IssueFile = serde_json::from_value(serde_json::json!({
            "task": {
                "id": "t-1",
                "title": "Fix it",
                "column": "in_progress",
                "priority": 0,
                "created_at": "2026-11-02T10:00:00Z",
                "updated_at": "2026-11-02T10:00:00Z"
            },
            "body": "The body."
        }))
        .unwrap();
        let details = WorkspaceDetails {
            machine: "devbox".into(),
            is_remote: true,
            worktree: "/srv/shelbi/.shelbi/wt/alpha".into(),
            remote_address: Some("jlong@devbox:/srv/shelbi/.shelbi/wt/alpha".into()),
            editor_name: "Helix".into(),
            apps: shelbi_state::AppTools {
                editor: false,
                diff: true,
                builtin_diff: false,
            },
            branch: Some("jlong/t-1".into()),
            task: Some(Box::new(task)),
        };
        let expected = serde_json::json!({
            "machine": "devbox",
            "is_remote": true,
            "worktree": "/srv/shelbi/.shelbi/wt/alpha",
            "remote_address": "jlong@devbox:/srv/shelbi/.shelbi/wt/alpha",
            "editor_name": "Helix",
            "apps": { "editor": false, "diff": true, "builtin_diff": false },
            "branch": "jlong/t-1",
            "task": {
                "task": {
                    "id": "t-1",
                    "title": "Fix it",
                    "column": "in_progress",
                    "priority": 0,
                    "created_at": "2026-11-02T10:00:00Z",
                    "updated_at": "2026-11-02T10:00:00Z"
                },
                "body": "The body."
            }
        });
        assert_eq!(serde_json::to_value(&details).unwrap(), expected);
        let back: WorkspaceDetails = serde_json::from_value(expected).unwrap();
        assert_eq!(back, details);

        let empty: WorkspaceDetails = serde_json::from_str("{}").unwrap();
        assert_eq!(empty, WorkspaceDetails::default());
    }

    /// A remote content session runs under the remote login shell (for its
    /// `PATH`), with the command quoted whole.
    #[test]
    fn a_remote_content_session_runs_under_the_login_shell() {
        let argv = remote_content_argv("cd /srv/wt && exec vim .");
        assert_eq!(
            argv,
            vec![
                "sh".to_string(),
                "-c".to_string(),
                "exec \"${SHELL:-/bin/sh}\" -lc 'cd /srv/wt && exec vim .'".to_string(),
            ]
        );
    }

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

    #[test]
    fn only_a_shell_session_reads_as_a_user_shell() {
        assert!(is_user_shell_session(&shell_session_name("myapp", "alpha")));
        assert!(is_user_shell_session(&shell_session_name("myapp", "shell")));
        assert!(!is_user_shell_session("myapp/ws/shell"), "an agent in a workspace called shell");
        assert!(!is_user_shell_session("w-shell/ws/agent"));
        assert!(!is_user_shell_session("myapp/ws/alpha/editor"));
        assert!(!is_user_shell_session("myapp/orch"));
    }

    #[test]
    fn shell_session_name_sits_beside_the_content_sessions() {
        assert_eq!(shell_session_name("myapp", "alpha"), "myapp/ws/alpha/shell");
        assert_ne!(
            shell_session_name("myapp", "alpha"),
            content_session_name("myapp", "alpha", ReviewContentRole::Editor)
        );
    }

    #[test]
    fn a_local_shell_starts_in_the_worktree_or_the_work_dir() {
        let (wt, wd) = (Path::new("/w/alpha"), Path::new("/w"));
        let (cwd, argv) = user_shell_command(false, wt, wd, "/bin/zsh", true);
        assert_eq!((cwd.as_path(), argv), (wt, vec!["/bin/zsh".to_string(), "-l".into()]));
        let (cwd, _) = user_shell_command(false, wt, wd, "/bin/zsh", false);
        assert_eq!(cwd.as_path(), wd, "a pruned worktree falls back to work_dir");
    }

    #[test]
    fn a_remote_shell_cds_on_the_remote_and_runs_its_own_shell() {
        let (cwd, argv) =
            user_shell_command(true, Path::new("/home/u/wt/a b"), Path::new("/home/u"), "/bin/zsh", false);
        assert_eq!(cwd, Path::new("/home/u"));
        assert_eq!(argv[..2], ["sh".to_string(), "-c".to_string()]);
        assert_eq!(
            argv[2],
            "cd '/home/u/wt/a b' 2>/dev/null || cd /home/u; exec \"${SHELL:-/bin/sh}\" -l"
        );
    }


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

    /// A GitHub-backend project pointed at `owner/repo` — the remote case where
    /// the published-index-vs-live-read divergence shows up (a `file_system`
    /// board reads the same bytes either way).
    fn github_project() -> shelbi_core::Project {
        let projects = shelbi_state::projects_dir().unwrap();
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(
            projects.join("proj.yaml"),
            r#"name: proj
repo: /tmp/proj
default_branch: main
issue_tracker:
  backend: github
  github:
    repo: owner/repo
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/proj
workspaces:
  - { name: alpha, machine: local }
  - { name: bravo, machine: local }
  - { name: charlie, machine: local }
"#,
        )
        .unwrap();
        shelbi_state::load_project("proj").unwrap()
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
                pr: None,
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
        for tf in &idx.board {
            if let Some(workspace) = tf.task.assigned_to.as_deref() {
                shelbi_state::set_task_assignment("proj", &tf.task.id, Some(workspace)).unwrap();
            }
        }
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

        let task = working_task_for(&github_project(), "alpha").unwrap();
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

        let project = github_project();
        let for_alpha = working_task_for(&project, "alpha").unwrap();
        let for_idle = working_task_for(&project, "charlie").unwrap();
        std::env::remove_var("SHELBI_HOME");
        assert!(for_alpha.is_none(), "a review-column task is not the running task");
        assert!(for_idle.is_none(), "an unassigned workspace has no running task");
    }
}
