//! Read the clients' [`MachinesModel`] from disk: the IO half of the machines
//! view, shared by the TUI and the desktop so both show the same machines and
//! workspaces from the same state.
//!
//! [`MachinesModel::build`] (`shelbi-app`) is the pure mapping; this module
//! reads what it maps: the project's machines and workspaces, the recorded
//! remote binaries (`rt-machine-setup`), each workspace's `status.yaml`, and
//! the board's in-progress and review tasks the same way `shelbi workspace
//! list` does (the daemon-published board index, with review-slot ownership
//! from the local assignment overlay). For a review slot it resolves the review
//! server's state (the slot's serving marker, as the sidebar does) and URL.
//!
//! Every function here can block (disk, and for a remote review slot an SSH
//! marker read), so call them off the UI thread. Live machine reachability is
//! not read here: it comes from a `shelbi_app::reachability::ReachabilityProber`
//! the view folds in, so a down host never stalls this read.

use std::collections::HashMap;

use shelbi_app::view::{MachinesInput, MachinesModel, ReviewSlot, ReviewState};
use shelbi_core::Column;
use shelbi_state::{
    fold_assignment_overlay, load_project, load_task_workflow, load_workspace_status, read_board,
    IssueFile,
};

/// Read `project`'s machines view. `Err` carries why the project config
/// couldn't load (the view shows it); a board that can't be read yet leaves
/// every workspace idle rather than failing the read.
pub fn read_machines_model(project: &str) -> anyhow::Result<MachinesModel> {
    Ok(MachinesModel::build(&read_machines_input(project)?))
}

/// The inputs [`read_machines_model`] maps.
pub fn read_machines_input(project_name: &str) -> anyhow::Result<MachinesInput> {
    let project = load_project(project_name)?;
    let records = shelbi_state::machine_state::load_machine_records();
    let statuses = project
        .workspaces
        .iter()
        .filter_map(|w| {
            load_workspace_status(&w.name)
                .ok()
                .flatten()
                .map(|s| (w.name.clone(), s))
        })
        .collect();

    // One read of the daemon-owned board index, filtered in memory, exactly as
    // `shelbi workspace list` does: never a backend sweep from a client.
    let board = read_board(project_name)
        .map(|b| b.into_issues())
        .unwrap_or_default();
    let in_progress: Vec<IssueFile> = board
        .iter()
        .filter(|tf| tf.task.column == Column::in_progress())
        .cloned()
        .collect();
    let mut review: Vec<IssueFile> = board
        .iter()
        .filter(|tf| tf.task.column == Column::review())
        .cloned()
        .collect();
    // Review-slot ownership from the local assignment overlay: the index's
    // publish-time fold can lag a fresh review load.
    fold_assignment_overlay(project_name, &project.issue_tracker, &mut review);

    let branch_of = |tf: &IssueFile| {
        let workflow = load_task_workflow(project_name, &project, &tf.task).ok();
        crate::branch::branch_name_for_task(&project, workflow.as_ref(), &tf.task)
            .ok()
            .or_else(|| tf.task.branch.clone())
    };
    let mut branches = HashMap::new();
    for tf in &in_progress {
        if let Some(b) = branch_of(tf) {
            branches.insert(tf.task.id.clone(), b);
        }
    }

    let mut review_slots = HashMap::new();
    let mut titles = HashMap::new();
    for tf in &review {
        let Some(ws) = tf
            .task
            .assigned_to
            .as_deref()
            .and_then(|name| project.workspace(name))
            .filter(|w| project.effective_tags(w).contains("review"))
        else {
            continue;
        };
        let state = if crate::workspace::review_slot_is_serving(&project, ws, &tf.task.id) {
            ReviewState::Serving
        } else {
            ReviewState::Loading
        };
        let url = load_task_workflow(project_name, &project, &tf.task)
            .ok()
            .and_then(|wf| {
                wf.review_url_for_status(tf.task.column.as_str())
                    .map(str::to_string)
            })
            .map(|template| {
                let port = ws.slot.and_then(|s| u16::try_from(s).ok());
                shelbi_core::substitute_review_url(&template, port)
            });
        review_slots.insert(
            ws.name.clone(),
            ReviewSlot {
                task_id: tf.task.id.clone(),
                state,
                url,
            },
        );
        titles.insert(tf.task.id.clone(), tf.task.title.clone());
        if let Some(b) = branch_of(tf) {
            branches.insert(tf.task.id.clone(), b);
        }
    }

    let workspaces = project
        .workspaces
        .iter()
        .map(|w| (w.clone(), project.effective_tags(w).contains("review")))
        .collect();

    Ok(MachinesInput {
        display_name: project
            .display_name
            .clone()
            .or_else(|| project.label.clone()),
        machines: project.machines.clone(),
        workspaces,
        records,
        statuses,
        in_progress,
        review_slots,
        branches,
        titles,
    })
}
