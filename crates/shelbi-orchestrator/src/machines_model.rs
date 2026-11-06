//! Read the clients' [`MachinesModel`] from disk: the IO half of the machines
//! view, shared by the TUI and the desktop so both show the same machines and
//! workspaces from the same state.
//!
//! [`MachinesModel::build`] (`shelbi-app`) is the pure mapping; this module
//! reads what it maps: the project's machines and workspaces, the recorded
//! remote binaries (`rt-machine-setup`), each workspace's `status.yaml`, and
//! the board's working and serving tasks the same way `shelbi workspace list`
//! does (the published board index through [`BoardOccupancy`]). For a review slot it resolves the review
//! server's state (the slot's serving marker, as the sidebar does) and URL.
//!
//! Every function here can block (disk, and for a remote review slot an SSH
//! marker read), so call them off the UI thread. Live machine reachability is
//! not read here: it comes from a `shelbi_app::reachability::ReachabilityProber`
//! the view folds in, so a down host never stalls this read.

use std::collections::HashMap;

use shelbi_app::view::{MachinesInput, MachinesModel, ReviewSlot, ReviewState};
use shelbi_state::{
    load_project, load_task_workflow, load_workspace_status, BoardOccupancy, IssueFile,
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
            load_workspace_status(project_name, &w.name)
                .ok()
                .flatten()
                .map(|s| (w.name.clone(), s))
        })
        .collect();

    // One read of the daemon-owned board index with owners resolved from the
    // assignment overlay, exactly as `shelbi workspace list` does: never a
    // backend sweep from a client.
    let occupancy = BoardOccupancy::read(&project).unwrap_or_default();
    let in_progress: Vec<IssueFile> = occupancy.working().cloned().collect();
    let review: Vec<IssueFile> = occupancy.serving().cloned().collect();

    let branch_of = |tf: &IssueFile| {
        let workflow = load_task_workflow(project_name, &project, &tf.task).ok();
        crate::branch::branch_name_for_task(&project, workflow.as_ref(), &tf.task)
            .ok()
            .or_else(|| tf.task.branch.clone())
    };
    // The runner a task's agent launches on, through the same seam dispatch
    // uses (task `launch:` -> project `agents.<name>.runner` -> the agent's
    // `preferred_runner` -> the fleet's launcher for that kind).
    let runner_of = |tf: &IssueFile, ws: &shelbi_core::WorkspaceSpec, agent: &str| {
        crate::workspace::resolve_workspace_launch(&project, ws, Some(agent), tf.task.launch.as_ref())
            .ok()
            .map(|launch| runner_label(&launch.runner))
    };
    let mut branches = HashMap::new();
    let mut runners = HashMap::new();
    for tf in &in_progress {
        if let Some(b) = branch_of(tf) {
            branches.insert(tf.task.id.clone(), b);
        }
        let agent = tf
            .task
            .param_str("agent")
            .unwrap_or(shelbi_app::view::DEFAULT_TASK_AGENT);
        let ws = tf
            .task
            .assigned_to
            .as_deref()
            .and_then(|name| project.workspace(name));
        if let Some(r) = ws.and_then(|ws| runner_of(tf, ws, agent)) {
            runners.insert(tf.task.id.clone(), r);
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
        if let Some(r) = runner_of(tf, ws, shelbi_app::view::REVIEW_SLOT_AGENT) {
            runners.insert(tf.task.id.clone(), r);
        }
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
        runners,
    })
}

/// A runner's name as the Machines view shows it: `Claude`, `Codex`, or a
/// generic runner's command name (`aider`).
fn runner_label(spec: &shelbi_core::AgentRunnerSpec) -> String {
    match shelbi_core::RunnerKind::detect(spec) {
        shelbi_core::RunnerKind::Claude => "Claude".to_string(),
        shelbi_core::RunnerKind::Codex => "Codex".to_string(),
        shelbi_core::RunnerKind::Generic => std::path::Path::new(&spec.command)
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(&spec.command)
            .to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(yaml: &str) -> shelbi_core::AgentRunnerSpec {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn runners_read_as_their_cli() {
        assert_eq!(runner_label(&spec("command: /usr/local/bin/claude")), "Claude");
        assert_eq!(runner_label(&spec("command: codex")), "Codex");
        // A wrapper pinned to a contract reads as that contract's CLI.
        assert_eq!(
            runner_label(&spec("command: my-wrapper\nintegration: codex")),
            "Codex"
        );
        assert_eq!(runner_label(&spec("command: /opt/bin/aider")), "aider");
    }
}
