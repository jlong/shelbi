//! Build the clients' [`SidebarModel`] from disk: the rich, IO-backed builder
//! a client's off-thread refresher calls each tick. Shared by the TUI shell and
//! the desktop app, so both read the same sidebar from the same state.
//!
//! `shelbi_app::view::SidebarModel::from_board` is the toolkit- and
//! orchestrator-free *structural* builder (it can't read a workspace's
//! `status.yaml` or resolve a review slot's serving marker, since `shelbi-app`
//! must not depend on `shelbi-orchestrator`). This module layers the
//! disk-derived data the former tmux-runtime sidebar showed on top: machine
//! grouping, per-workspace state badges, the review sections split by lifecycle
//! state (with branch + serving location), the config-load error, the collapse
//! set, and the footer's daemon-version probe and board-freshness banner.
//!
//! Every function here can block (disk, the daemon socket, and for a remote
//! review slot an SSH liveness probe), so call them off the UI thread.

use shelbi_app::view::{
    default_nav_items, ReviewRow, ReviewState, SidebarModel, WorkspaceBadge, WorkspaceRow,
};
use shelbi_core::Column;
use shelbi_state::{
    daemon_version_status, fold_assignment_overlay, issue_store_for, load_project,
    load_task_workflow, load_workspace_status, read_board_index_numbers, read_board_report, read_state,
    sidebar_collapsed_machines, unread_error_count, DaemonVersionStatus, IssueFile, WorkspaceState,
    ZenModeState,
};

/// Read every surface the sidebar needs for `project` and assemble the model.
/// Mirrors the old sidebar's `refresh`: one board read feeds both the workspace
/// pool and the review sections; the store-build result gates config-error
/// classification exactly as before.
pub fn read_sidebar_model(project: &str) -> Option<SidebarModel> {
    let mut model = SidebarModel {
        project_label: project.to_string(),
        nav: default_nav_items(),
        workspaces: Vec::new(),
        reviews: Vec::new(),
        config_error: None,
        board_loading: false,
        collapsed_machines: sidebar_collapsed_machines().unwrap_or_default(),
        board_banner: None,
        daemon_version_line: None,
        daemon_version_mismatch: false,
        // Set on the UI thread (startup state, not a per-refresh read).
        status_line: String::new(),
        zen_mode: read_state(project)
            .map(|s| s.zen_mode)
            .unwrap_or(ZenModeState::Off),
        unread_errors: unread_error_count(project).unwrap_or(0),
        // Hub-global: present when the daemon recorded a lost macOS login
        // session. Read once per refresh (not per frame) and painted as a
        // persistent top banner by the shell's `draw`.
        session_lost: shelbi_state::read_session_lost().map(|_| {
            shelbi_state::SESSION_LOST_BANNER.to_string()
        }),
        // Daemon-written when the orchestrator supervisor's relaunch failed or
        // gave up; cleared once the orchestrator is alive again.
        orchestrator_status: shelbi_state::read_orchestrator_supervision_status(project),
    };

    // A human-readable label when the project config carries one.
    if let Ok(p) = load_project(project) {
        if let Some(label) = p.display_name.clone().or_else(|| p.label.clone()) {
            model.project_label = label;
        }
    }

    // The store-build is the config-validity gate: a broken/absent project
    // classifies in `apply_workspaces` below. A readable store then serves the
    // published board index (Warm/Stale/Cold), never a live backend read here.
    match issue_store_for(project) {
        Ok(_store) => match read_board_report(project) {
            Ok(report) if report.state.is_cold() => {
                model.board_banner = report.freshness.banner();
                model.board_loading = true;
            }
            Ok(report) => {
                model.board_banner = report.freshness.banner();
                let board = report.state.into_issues();
                let mut review: Vec<IssueFile> = board
                    .iter()
                    .filter(|f| f.task.column == Column::review())
                    .cloned()
                    .collect();
                // Resolve review-slot ownership from the local assignment
                // overlay, not the index's publish-time fold — a fresh load
                // lands in the overlay first and the index can lag it.
                let mut numbers = Default::default();
                if let Ok(p) = load_project(project) {
                    fold_assignment_overlay(project, &p.issue_tracker, &mut review);
                    // The desktop labels each review row with its tracker
                    // number (`#401`); only a remote board has one.
                    if !review.is_empty() {
                        numbers = read_board_index_numbers(project, &p.issue_tracker);
                    }
                }
                model.reviews = split_reviews(project, review);
                for row in &mut model.reviews {
                    row.number = numbers.get(&row.task_id).copied();
                }
                let in_progress: Vec<IssueFile> = board
                    .iter()
                    .filter(|f| f.task.column == Column::in_progress())
                    .cloned()
                    .collect();
                apply_workspaces(project, &in_progress, &mut model);
            }
            // The board read itself failed: keep the (empty) sections rather
            // than flagging the config as broken.
            Err(_) => {}
        },
        Err(_) => {
            // The store couldn't be built — broken/absent config. Let
            // `apply_workspaces` decide broken (inline error) vs absent (quiet).
            apply_workspaces(project, &[], &mut model);
        }
    }

    // Footer daemon/CLI version segment, refreshed with the rest of the state.
    let (line, mismatch) = probe_daemon_version();
    model.daemon_version_line = Some(line);
    model.daemon_version_mismatch = mismatch;

    Some(model)
}

/// A sidebar skeleton for `project` built from its **config alone** — the
/// display label and the declared (non-review) workspaces, each with a
/// [`WorkspaceBadge::Loading`] badge — plus the persisted machine-collapse set.
/// No board, status, review-slot, or daemon reads, so it is cheap, but it still
/// touches disk: the shell builds these off the UI thread ahead of time
/// (the TUI's `ShellState::spawn_skeleton_prefetch`, the desktop's project
/// prefetch) so a *cold* project switch can paint
/// the target's workspace rows on the very next frame. `None` when the
/// project's config doesn't load (the caller falls back to the bare skeleton).
pub fn read_config_skeleton(project: &str) -> Option<SidebarModel> {
    let p = load_project(project).ok()?;
    let workspaces = p
        .workspaces
        .iter()
        .filter(|w| !p.effective_tags(w).contains("review"))
        .filter_map(|w| {
            let machine = p.machine(&w.machine)?;
            Some(WorkspaceRow {
                name: w.name.clone(),
                machine: w.machine.clone(),
                is_remote: !machine.host().is_local(),
                current_task: None,
                agent: None,
                badge: WorkspaceBadge::Loading,
            })
        })
        .collect();
    Some(SidebarModel {
        project_label: p
            .display_name
            .clone()
            .or_else(|| p.label.clone())
            .unwrap_or_else(|| project.to_string()),
        nav: default_nav_items(),
        workspaces,
        reviews: Vec::new(),
        config_error: None,
        board_loading: false,
        collapsed_machines: sidebar_collapsed_machines().unwrap_or_default(),
        board_banner: None,
        daemon_version_line: None,
        daemon_version_mismatch: false,
        status_line: String::new(),
        zen_mode: ZenModeState::Off,
        unread_errors: 0,
        session_lost: None,
        orchestrator_status: None,
    })
}

/// Default agent surfaced when a task has no explicit `agent:` — matches
/// `shelbi workspace list`'s `DEFAULT_TASK_AGENT`.
const DEFAULT_TASK_AGENT: &str = "developer";

/// Build the workspace rows from `in_progress` and set `config_error`: a
/// *present* but unloadable config surfaces an inline error; an absent one
/// stays quiet (a fresh/half-set-up project).
fn apply_workspaces(project: &str, in_progress: &[IssueFile], model: &mut SidebarModel) {
    match load_workspaces(project, in_progress) {
        Ok(ws) => {
            model.workspaces = ws;
            model.config_error = None;
        }
        Err(e) => {
            model.workspaces = Vec::new();
            model.config_error = project_config_present(project).then(|| e.to_string());
        }
    }
}

/// The sidebar's view of declared dev workspaces — review-tagged slots are
/// skipped (their capacity surfaces only through the review sections). Errors
/// when the project config can't load, so the caller can distinguish "not set
/// up" from "broken config".
fn load_workspaces(project: &str, in_progress: &[IssueFile]) -> anyhow::Result<Vec<WorkspaceRow>> {
    let p = load_project(project)?;
    let mut out = Vec::with_capacity(p.workspaces.len());
    for workspace in &p.workspaces {
        if p.effective_tags(workspace).contains("review") {
            continue;
        }
        let machine = match p.machine(&workspace.machine) {
            Some(m) => m,
            None => continue, // mis-configured workspace, skip silently
        };
        let is_remote = !machine.host().is_local();
        let assigned_task = in_progress
            .iter()
            .find(|tf| tf.task.assigned_to.as_deref() == Some(workspace.name.as_str()));
        let current_task = assigned_task.map(|tf| tf.task.id.clone());
        let agent = assigned_task.map(|tf| {
            tf.task
                .param_str("agent")
                .map(str::to_string)
                .unwrap_or_else(|| DEFAULT_TASK_AGENT.to_string())
        });
        let badge = derive_workspace_badge(&workspace.name, current_task.as_deref());
        out.push(WorkspaceRow {
            name: workspace.name.clone(),
            machine: workspace.machine.clone(),
            is_remote,
            current_task,
            agent,
            badge,
        });
    }
    Ok(out)
}

/// Pick the badge for a workspace from the task-board signal + its on-disk
/// `status.yaml`. Idle wins when there is no in-progress task, so a stale
/// status file never shows "working" for an idle slot.
fn derive_workspace_badge(workspace_name: &str, current_task: Option<&str>) -> WorkspaceBadge {
    let Some(task_id) = current_task else {
        return WorkspaceBadge::Idle;
    };
    match load_workspace_status(workspace_name).ok().flatten() {
        // A Blocked pane is hard-stuck on a human — surface the red ⚠ whether
        // or not the status names the currently-assigned task.
        Some(s) if s.state == WorkspaceState::Blocked => WorkspaceBadge::AwaitingPermission,
        // Otherwise only trust a status that describes the current task; a
        // status still naming a prior task is stale.
        Some(s) if s.current_task.as_deref() == Some(task_id) => match s.state {
            WorkspaceState::Working => WorkspaceBadge::Working,
            WorkspaceState::AwaitingInput => WorkspaceBadge::AwaitingInput,
            WorkspaceState::Blocked => WorkspaceBadge::AwaitingPermission,
            WorkspaceState::Paused => WorkspaceBadge::Paused,
            WorkspaceState::Serving => WorkspaceBadge::Working,
        },
        // Assigned but no matching marker yet — best-guess working; firms up
        // within one poll tick.
        _ => WorkspaceBadge::Working,
    }
}

/// Split the Review column into the Ready (Serving/Loading) and Queued
/// (Pending) sections, resolving each task's branch and (when serving) its
/// `machine:port` location. A single flat vec is returned — the renderer
/// partitions it by [`ReviewState`]. When the project can't load, every row
/// falls back to Pending with no location.
fn split_reviews(project_name: &str, queue: Vec<IssueFile>) -> Vec<ReviewRow> {
    let project = match load_project(project_name) {
        Ok(p) => p,
        Err(_) => {
            return queue
                .iter()
                .map(|tf| ReviewRow {
                    task_id: tf.task.id.clone(),
                    title: tf.task.title.clone(),
                    branch: tf
                        .task
                        .branch
                        .clone()
                        .unwrap_or_else(|| format!("user/{}", tf.task.id)),
                    location: None,
                    workspace: None,
                    state: ReviewState::Pending,
                    number: None,
                })
                .collect();
        }
    };

    let entry = |task: &shelbi_core::Issue,
                 location: Option<String>,
                 workspace: Option<String>,
                 state: ReviewState| {
        let workflow = load_task_workflow(project_name, &project, task).ok();
        let branch =
            crate::branch::branch_name_for_task(&project, workflow.as_ref(), task)
                .unwrap_or_else(|_| {
                    task.branch
                        .clone()
                        .unwrap_or_else(|| format!("user/{}", task.id))
                });
        ReviewRow {
            task_id: task.id.clone(),
            title: task.title.clone(),
            branch,
            location,
            workspace,
            state,
            number: None,
        }
    };

    let mut out = Vec::with_capacity(queue.len());
    for tf in &queue {
        let loaded_on = tf
            .task
            .assigned_to
            .as_deref()
            .and_then(|name| project.workspace(name))
            .filter(|w| project.effective_tags(w).contains("review"));
        match loaded_on {
            Some(ws)
                if crate::workspace::review_slot_is_serving(
                    &project,
                    ws,
                    &tf.task.id,
                ) =>
            {
                let location = Some(format!("{}:{}", ws.machine, ws.name));
                out.push(entry(
                    &tf.task,
                    location,
                    Some(ws.name.clone()),
                    ReviewState::Serving,
                ));
            }
            Some(ws) => out.push(entry(
                &tf.task,
                None,
                Some(ws.name.clone()),
                ReviewState::Loading,
            )),
            None => out.push(entry(&tf.task, None, None, ReviewState::Pending)),
        }
    }
    out
}

/// Whether a config file for `project` exists on disk in either supported
/// layout — deliberately *not* validating the name (the point is to detect a
/// present-but-invalid config).
fn project_config_present(project: &str) -> bool {
    let Ok(dir) = shelbi_state::projects_dir() else {
        return false;
    };
    dir.join(format!("{project}.yaml")).is_file() || dir.join(project).join("local.yaml").is_file()
}

/// Probe the hub daemon and precompute the footer version segment + mismatch
/// flag. `cli` is this build's version (the workspace version every client
/// shares).
fn probe_daemon_version() -> (String, bool) {
    let cli = shelbi_state::CLIENT_VERSION;
    match daemon_version_status() {
        DaemonVersionStatus::NotRunning => (format!("daemon not running · cli {cli}"), false),
        DaemonVersionStatus::Match { version } => (format!("daemon {version} · cli {cli}"), false),
        DaemonVersionStatus::Mismatch { daemon } => (
            format!("daemon {daemon} ≠ cli {cli} — shelbi daemon restart"),
            true,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_config_skeleton_lists_declared_workspaces_awaiting_state() {
        let _g = crate::test_lock::acquire();
        let home = std::env::temp_dir().join(format!(
            "shelbi-skeleton-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(
            home.join("projects/demo.yaml"),
            "name: demo\ndisplay_name: Demo Board\nrepo: ''\ndefault_branch: main\n\
             orchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n- name: hub\n  kind: local\n  work_dir: /tmp/demo\n\
             - name: devbox\n  kind: ssh\n  host: devbox.local\n  work_dir: /work/demo\n\
             workspaces:\n- name: review\n  machine: hub\n  tags: [review]\n\
             - name: alpha\n  machine: hub\n- name: delta\n  machine: devbox\n",
        )
        .unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        let model = read_config_skeleton("demo").expect("a skeleton from config");
        let missing = read_config_skeleton("nope");
        std::env::remove_var("SHELBI_HOME");

        assert_eq!(model.project_label, "Demo Board");
        let rows: Vec<_> = model
            .workspaces
            .iter()
            .map(|w| (w.name.as_str(), w.machine.as_str(), w.is_remote))
            .collect();
        assert_eq!(
            rows,
            [("alpha", "hub", false), ("delta", "devbox", true)],
            "declared dev workspaces, review slot excluded"
        );
        assert!(model
            .workspaces
            .iter()
            .all(|w| w.badge == WorkspaceBadge::Loading && w.agent.is_none()));
        assert!(!model.board_loading, "the rows themselves carry the loading state");
        assert!(missing.is_none(), "an unknown project falls back to the bare skeleton");
    }
}
