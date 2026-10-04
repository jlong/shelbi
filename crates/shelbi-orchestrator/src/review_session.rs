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
    let editor_name = shelbi_state::editor_display_name(&shelbi_state::resolve_editor());
    let has_review_url = {
        let store = shelbi_state::resolve_issue_store(project_name, &project.issue_tracker)?;
        match store.get(task_id)? {
            Some(tf) => shelbi_state::load_task_workflow(project_name, project, &tf.task)
                .ok()
                .map(|wf| wf.review_url_for_status(tf.task.column.as_str()).is_some())
                .unwrap_or(false),
            None => false,
        }
    };
    Ok(ReviewOpenInfo {
        slot: slot.to_string(),
        worktree,
        editor_name,
        has_review_url,
    })
}

/// How the single-process TUI should open a review-column task, resolved from
/// durable state in one read (`rt-tui-review-load-queued`): either it is already
/// loaded on a review slot — open the interface straight away — or it is still
/// queued and must be loaded onto a slot first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewOpenTarget {
    /// The task is on a review slot; open the interface with these inputs
    /// (the same the tmux runtime and [`review_open_info`] resolve).
    Serving(ReviewOpenInfo),
    /// The task is a review-column task not yet on a review slot; the client
    /// must load it onto one (the slot picker, then a daemon
    /// [`Load`](shelbi_proto::control::ReviewSessionOp::Load)). `title` is the
    /// card title the picker shows.
    Queued { title: String },
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
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            launch: None,
            params: std::collections::BTreeMap::new(),
            created_at: now,
            updated_at: now,
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
        assert_eq!(
            review_open_target(&proj, "t-dev").unwrap(),
            ReviewOpenTarget::Queued {
                title: "On the dev slot".into()
            },
            "a handoff pinned to its dev slot is queued, not serving"
        );
        assert_eq!(
            review_open_target(&proj, "t-none").unwrap(),
            ReviewOpenTarget::Queued {
                title: "Unassigned".into()
            }
        );

        match prev_home {
            Some(h) => std::env::set_var("SHELBI_HOME", h),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&home);
    }
}
