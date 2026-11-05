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

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock};

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

impl From<shelbi_proto::control::ReviewRole> for ReviewContentRole {
    fn from(role: shelbi_proto::control::ReviewRole) -> Self {
        match role {
            shelbi_proto::control::ReviewRole::Editor => ReviewContentRole::Editor,
            shelbi_proto::control::ReviewRole::Diff => ReviewContentRole::Diff,
        }
    }
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

/// The review panel's inputs and how to open a review-column task; the types
/// live in `shelbi_app::source` so a project source can return them.
pub use shelbi_app::source::{ReviewOpenInfo, ReviewOpenTarget};

/// Resolve the panel inputs for `task_id`. Errors when the task is not loaded on
/// a review slot (the interface can't open yet).
pub fn review_open_info(project_name: &str, task_id: &str) -> Result<ReviewOpenInfo> {
    let project = shelbi_state::load_project(project_name)?;
    let slot = resolve_review_slot(project_name, &project, task_id)?.ok_or_else(|| {
        Error::Other(format!("`{task_id}` is not loaded on a review slot"))
    })?;
    review_open_info_for_slot(project_name, &project, task_id, &slot)
}

/// Build the panel inputs for a task already resolved onto review slot `slot`.
/// Shared by [`review_open_info`] and [`review_open_target`] so the queued and
/// serving paths agree on how a slot becomes panel state.
fn review_open_info_for_slot(
    project_name: &str,
    project: &Project,
    task_id: &str,
    slot: &str,
) -> Result<ReviewOpenInfo> {
    let ws = project
        .workspace(slot)
        .ok_or_else(|| Error::Other(format!("unknown review slot `{slot}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    let worktree = crate::workspace::workspace_worktree(machine, ws)
        .to_string_lossy()
        .to_string();
    let editor_name = shelbi_state::resolved_editor_name();
    let apps = crate::tools::app_tools(project);
    // Read the task once, for both the review-URL gate and the panel's task
    // info (title / description preview / popover).
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let mut task = store.get(task_id)?;
    if let Some(tf) = task.as_mut() {
        shelbi_state::pr_status::fold(project_name, std::slice::from_mut(tf));
    }
    let workflow = task
        .as_ref()
        .and_then(|tf| shelbi_state::load_task_workflow(project_name, project, &tf.task).ok());
    let review_url = task.as_ref().zip(workflow.as_ref()).and_then(|(tf, wf)| {
        wf.review_url_for_status(tf.task.column.as_str())
            .map(|template| substitute_slot_url(template, ws))
    });
    let has_health_check = workflow
        .as_ref()
        .is_some_and(crate::workspace::workflow_declares_health_check);
    let remote_machine = (machine.host() != Host::Local).then(|| ws.machine.clone());
    Ok(ReviewOpenInfo {
        slot: slot.to_string(),
        worktree,
        editor_name,
        apps,
        has_review_url: review_url.is_some(),
        review_url,
        has_health_check,
        remote_machine,
        task: task.map(Box::new),
    })
}

/// The review URL to open for `task_id` (the workflow's `review:` template,
/// with the `$PORT` / `$SLOT` of the slot it is assigned to substituted), or
/// `None` when none is configured. Read fresh at click time by the TUI's Open
/// Browser.
pub fn review_url(project_name: &str, task_id: &str) -> Option<String> {
    let project = shelbi_state::load_project(project_name).ok()?;
    let store = shelbi_state::issue_store_for_project(&project).ok()?;
    let tf = store.get(task_id).ok().flatten()?;
    let workflow = shelbi_state::load_task_workflow(project_name, &project, &tf.task).ok()?;
    let template = workflow.review_url_for_status(tf.task.column.as_str())?;
    let port = tf
        .task
        .assigned_to
        .as_deref()
        .and_then(|ws| project.workspace(ws))
        .and_then(|ws| ws.slot)
        .and_then(|s| u16::try_from(s).ok());
    Some(shelbi_core::substitute_review_url(template, port))
}

/// `template` with `ws`'s slot port substituted.
fn substitute_slot_url(template: &str, ws: &shelbi_core::WorkspaceSpec) -> String {
    let port = ws.slot.and_then(|s| u16::try_from(s).ok());
    shelbi_core::substitute_review_url(template, port)
}


/// Resolve how to open `task_id`: serving on a slot, or still queued. The
/// decision the shell makes between opening the native review interface
/// directly and raising the "load onto which review slot?" picker
/// (`rt-tui-review-load-queued`). Errors when the task is missing or is not a
/// review-column task (nothing to open or load).
pub fn review_open_target(project_name: &str, task_id: &str) -> Result<ReviewOpenTarget> {
    let project = shelbi_state::load_project(project_name)?;
    if let Some(slot) = resolve_review_slot(project_name, &project, task_id)? {
        let info = review_open_info_for_slot(project_name, &project, task_id, &slot)?;
        return Ok(ReviewOpenTarget::Serving(info));
    }
    // Not on a review slot. It must still be a review-column task to be loadable
    // — a card that has moved on (merged, rejected) is neither serving nor
    // queued, so the interface has nothing to open.
    let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
    let tf = store
        .get(task_id)?
        .ok_or_else(|| Error::Other(format!("issue `{task_id}` not found")))?;
    if tf.task.column != Column::review() {
        return Err(Error::Other(format!(
            "`{task_id}` is not a review task"
        )));
    }
    Ok(ReviewOpenTarget::Queued {
        title: tf.task.title.clone(),
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
    let branch = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?
        .get(task_id)?
        .and_then(|tf| tf.task.branch);
    let worktree = crate::workspace::workspace_worktree(machine, ws);

    let name = content_session_name(project_name, &slot, role);
    let backend = SessionProcessBackend;
    // Clients and their relaunchers ask concurrently (a retry, a second
    // client): the check and the spawn are one step, or two askers each find
    // nothing live and start a session under the one name.
    let lock = ensure_lock(&name);
    let _spawning = lock.lock().unwrap_or_else(|e| e.into_inner());
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
        ReviewContentRole::Diff => {
            // The slot is assigned before its branch is checked out, so a
            // review can ask for its diff while the load is still running.
            // Refuse until it's there: a missing worktree would otherwise read
            // as "no diff tool configured", and the previous task's checkout
            // would give a live session diffing the wrong commit.
            let slot_alive = crate::workspace::workspace_target(&project, ws)
                .and_then(|addr| crate::workspace::workspace_slot_alive(&Host::Local, &addr))
                .unwrap_or(false);
            worktree_ready_for_diff(&worktree, branch.as_deref(), slot_alive)?;
            crate::review_ui::diff_session_command(&project, &worktree)?
        }
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
    backend.spawn_session_to_attach(spec)
}

/// The per-session lock [`ensure_content_session`] holds across its live check
/// and spawn. One per name, so a slow first spawn (it asks a login shell for
/// `PATH`) holds up only the askers for that same session.
fn ensure_lock(name: &str) -> Arc<Mutex<()>> {
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    LOCKS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .entry(name.to_string())
        .or_default()
        .clone()
}

/// Whether a review slot's worktree holds what its diff should show. `Err`
/// names what it's waiting on, and is expected for a moment during a load: the
/// client retries.
///
/// - No worktree or no commit: the load hasn't created it yet.
/// - At the branch's tip: checked out, whatever else the load is still doing.
/// - Elsewhere, with the slot's agent running: settled. A load checks out
///   before it starts the agent, and the branch can move on under a review
///   that's already up (a push, a fetch), which must not lock its diff out.
/// - Elsewhere, with no agent: mid-load, still on the slot's previous task.
///
/// `branch` is `None` when the board doesn't know it: there is nothing to
/// compare, so a checked-out worktree is taken as is.
fn worktree_ready_for_diff(worktree: &Path, branch: Option<&str>, slot_alive: bool) -> Result<()> {
    let not_there = || {
        Error::Other(format!(
            "the review worktree at {} isn't checked out yet",
            worktree.display()
        ))
    };
    if !worktree.is_dir() {
        return Err(not_there());
    }
    let head = rev_parse(worktree, "HEAD").ok_or_else(not_there)?;
    let Some(branch) = branch else {
        return Ok(());
    };
    if slot_alive {
        return Ok(());
    }
    let tip = rev_parse(worktree, &format!("refs/remotes/origin/{branch}"))
        .or_else(|| rev_parse(worktree, &format!("refs/heads/{branch}")));
    if tip.as_deref() == Some(head.as_str()) {
        return Ok(());
    }
    Err(Error::Other(format!(
        "the review worktree hasn't checked out `{branch}` yet (it's still at {})",
        head.get(..8).unwrap_or(&head)
    )))
}

/// Whether `task_id`'s review worktree is checked out, so its diff is the
/// task's and not the slot's previous one. Blocking (git, the board): for
/// Shelbi's own diff view to wait on, off the UI thread. A slot on another
/// machine has no local worktree to look at, so it's taken as ready.
pub fn diff_ready(project_name: &str, task_id: &str) -> Result<()> {
    let project = shelbi_state::load_project(project_name)?;
    let Some(slot) = resolve_review_slot(project_name, &project, task_id)? else {
        return Err(Error::Other(format!("`{task_id}` is not loaded on a review slot")));
    };
    let ws = project
        .workspace(&slot)
        .ok_or_else(|| Error::Other(format!("unknown review slot `{slot}`")))?;
    let machine = project
        .machine(&ws.machine)
        .ok_or_else(|| Error::UnknownMachine(ws.machine.clone()))?;
    if machine.host() != Host::Local {
        return Ok(());
    }
    let branch = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?
        .get(task_id)?
        .and_then(|tf| tf.task.branch);
    let slot_alive = crate::workspace::workspace_target(&project, ws)
        .and_then(|addr| crate::workspace::workspace_slot_alive(&Host::Local, &addr))
        .unwrap_or(false);
    worktree_ready_for_diff(&crate::workspace::workspace_worktree(machine, ws), branch.as_deref(), slot_alive)
}

/// The commit `rev` names in `worktree`, if it names one.
fn rev_parse(worktree: &Path, rev: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Open `task_id`'s review worktree (the editor) or its branch diff in the
/// app chosen for `role` (`editor:` / `diff_tool:`), returning the status
/// line to show. A remote slot is refused with the reason (an app opens
/// local worktrees only). Blocking (config, board, and a short watch of the
/// launch), so call it off the UI thread.
pub fn open_in_app(project_name: &str, task_id: &str, role: ReviewContentRole) -> Result<String> {
    let project = shelbi_state::load_project(project_name)?;
    let slot = resolve_review_slot(project_name, &project, task_id)?
        .ok_or_else(|| Error::Other(format!("`{task_id}` is not loaded on a review slot")))?;
    let ws = project
        .workspace(&slot)
        .ok_or_else(|| Error::Other(format!("unknown review slot `{slot}`")))?;
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

    // -- review_open_target (the serving-vs-queued decision, rt-tui-review-load-queued) --

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
            session: Default::default(),
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
            disk: shelbi_core::DiskConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    fn task_on(id: &str, title: &str, slot: Option<&str>, column: Column) -> shelbi_core::Issue {
        let now = chrono::Utc::now();
        shelbi_core::Issue {
            id: id.into(),
            title: title.into(),
            column,
            priority: 0,
            assigned_to: slot.map(|s| s.into()),
            workflow: None,
            branch: None,
            pr: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: std::collections::BTreeMap::new(),
            created_at: now,
            updated_at: now,
        }
    }

    /// A repo with `main` and a branch `task` one commit past it, on `origin`
    /// too, and a worktree detached at `main`: a review slot still holding its
    /// previous task. Returns the tempdir, the worktree, and the branch tip.
    fn slot_on_previous_task() -> (tempfile::TempDir, std::path::PathBuf, String) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let git = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .arg("-C")
                .arg(cwd)
                .args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.hooksPath=/dev/null"])
                .args(args)
                .output()
                .unwrap();
            assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "base"]);
        git(&repo, &["checkout", "-q", "-b", "task"]);
        git(&repo, &["commit", "-q", "--allow-empty", "-m", "work"]);
        let tip = git(&repo, &["rev-parse", "HEAD"]);
        git(&repo, &["update-ref", "refs/remotes/origin/task", &tip]);
        git(&repo, &["checkout", "-q", "main"]);
        let wt = dir.path().join("slot");
        git(&repo, &["worktree", "add", "-q", "--detach", wt.to_str().unwrap(), "main"]);
        (dir, wt, tip)
    }

    fn checkout(wt: &Path, rev: &str) {
        let ok = std::process::Command::new("git")
            .arg("-C")
            .arg(wt)
            .args(["checkout", "-q", "--detach", rev])
            .status()
            .unwrap()
            .success();
        assert!(ok);
    }

    #[test]
    fn a_missing_worktree_is_not_ready_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let err = worktree_ready_for_diff(&dir.path().join("nope"), Some("task"), false).unwrap_err();
        assert!(err.to_string().contains("isn't checked out yet"), "{err}");
        // A live agent doesn't change that: there's still nothing to diff.
        assert!(worktree_ready_for_diff(&dir.path().join("nope"), Some("task"), true).is_err());
        // A folder that isn't a checkout isn't one either.
        assert!(worktree_ready_for_diff(dir.path(), Some("task"), false).is_err());
    }

    #[test]
    fn a_slot_on_the_previous_task_is_not_ready_until_the_branch_is_checked_out() {
        let (_dir, wt, tip) = slot_on_previous_task();
        let err = worktree_ready_for_diff(&wt, Some("task"), false).unwrap_err();
        assert!(err.to_string().contains("hasn't checked out `task`"), "{err}");

        checkout(&wt, &tip);
        worktree_ready_for_diff(&wt, Some("task"), false).unwrap();
    }

    #[test]
    fn a_branch_with_no_remote_is_compared_by_its_local_ref() {
        let (_dir, wt, tip) = slot_on_previous_task();
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["update-ref", "-d", "refs/remotes/origin/task"])
            .status()
            .unwrap();
        assert!(out.success());
        assert!(worktree_ready_for_diff(&wt, Some("task"), false).is_err());
        checkout(&wt, &tip);
        worktree_ready_for_diff(&wt, Some("task"), false).unwrap();
    }

    #[test]
    fn a_settled_review_is_ready_though_its_branch_moved_on() {
        // The slot's agent is up, so the load is past its checkout; a push or
        // fetch that moved the tip must not lock the diff out.
        let (_dir, wt, _tip) = slot_on_previous_task();
        worktree_ready_for_diff(&wt, Some("task"), true).unwrap();
    }

    #[test]
    fn an_unknown_branch_takes_any_checkout() {
        let (_dir, wt, _tip) = slot_on_previous_task();
        worktree_ready_for_diff(&wt, None, false).unwrap();
    }

    #[test]
    fn ensure_serializes_per_session_only() {
        assert!(Arc::ptr_eq(&ensure_lock("demo/review/r/diff"), &ensure_lock("demo/review/r/diff")));
        assert!(!Arc::ptr_eq(&ensure_lock("demo/review/r/diff"), &ensure_lock("demo/review/r/editor")));
    }

    /// A load in flight on a fresh slot used to fail its diff with "no diff tool
    /// configured" (git config read from a folder that isn't there).
    #[test]
    fn a_fresh_slots_diff_says_the_worktree_is_not_ready() {
        let _lock = crate::test_lock::acquire();
        let proj = format!("rfs-{}", std::process::id());
        let home = tempfile::tempdir().unwrap();
        let prev_home = std::env::var("SHELBI_HOME").ok();
        std::env::set_var("SHELBI_HOME", home.path());

        let mut project = demo_project(&proj);
        project.machines[0].work_dir = home.path().join("repo");
        shelbi_state::save_project(&project).unwrap();
        shelbi_state::save_task(&proj, &task_on("t-load", "Loading", Some("review-1"), Column::review()), "b").unwrap();

        let err = ensure_content_session(&proj, "t-load", ReviewContentRole::Diff).unwrap_err();
        assert!(err.to_string().contains("isn't checked out yet"), "{err}");

        match prev_home {
            Some(h) => std::env::set_var("SHELBI_HOME", h),
            None => std::env::remove_var("SHELBI_HOME"),
        }
    }

    /// `review_open_target` reports a review task on a review slot as serving
    /// (open the interface), and one still pinned to its dev slot — or
    /// unassigned — as queued (raise the slot picker). The queued verdict is the
    /// gap `rt-tui-review-load-queued` fills: without it the TUI couldn't load a
    /// handed-off task that isn't on a review slot yet.
    #[test]
    fn review_open_target_distinguishes_serving_from_queued() {
        let _lock = crate::test_lock::acquire();
        let proj = format!("rot-{}", std::process::id());
        let home = std::env::temp_dir().join(format!("shelbi-rot-{}", std::process::id()));
        std::fs::create_dir_all(&home).unwrap();
        let prev_home = std::env::var("SHELBI_HOME").ok();
        std::env::set_var("SHELBI_HOME", &home);

        shelbi_state::save_project(&demo_project(&proj)).unwrap();
        // Serving: a review-column task on the review-tagged slot.
        shelbi_state::save_task(
            &proj,
            &task_on("t-serve", "Serve me", Some("review-1"), Column::review()),
            "b",
        )
        .unwrap();
        // Queued: a handoff still pinned to the dev slot that built it.
        shelbi_state::save_task(
            &proj,
            &task_on("t-dev", "On the dev slot", Some("alpha"), Column::review()),
            "b",
        )
        .unwrap();
        // Queued: a review task with no assignment yet.
        shelbi_state::save_task(
            &proj,
            &task_on("t-none", "Unassigned", None, Column::review()),
            "b",
        )
        .unwrap();

        match review_open_target(&proj, "t-serve").unwrap() {
            ReviewOpenTarget::Serving(info) => assert_eq!(info.slot, "review-1"),
            other => panic!("expected Serving, got {other:?}"),
        }
        // `ReviewOpenTarget` is no longer `PartialEq` (it now carries the task's
        // `IssueFile`, which isn't), so match the variant and check its field.
        match review_open_target(&proj, "t-dev").unwrap() {
            ReviewOpenTarget::Queued { title } => assert_eq!(title, "On the dev slot"),
            other => panic!("a handoff pinned to its dev slot is queued, got {other:?}"),
        }
        match review_open_target(&proj, "t-none").unwrap() {
            ReviewOpenTarget::Queued { title } => assert_eq!(title, "Unassigned"),
            other => panic!("an unassigned review task is queued, got {other:?}"),
        }

        match prev_home {
            Some(h) => std::env::set_var("SHELBI_HOME", h),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
