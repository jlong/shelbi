//! `shelbi worker <subcommand>` — manage the project's declared worker
//! pool. Workers are durable slots (one worktree each); tasks come and go.
//! See [`shelbi_orchestrator::worker`] for the lifecycle primitives.

use anyhow::{anyhow, Result};
use clap::Subcommand;
use shelbi_core::Column;
use shelbi_orchestrator::worker as orch_worker;

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum WorkerCmd {
    /// List declared workers, their machine/runner, current task (if any),
    /// and whether their tmux pane is live.
    List,
    /// Kill a worker's tmux pane. Doesn't touch any task's column.
    Stop { name: String },
}

pub fn run(project_opt: Option<String>, cmd: WorkerCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        WorkerCmd::List => list(&project),
        WorkerCmd::Stop { name } => stop(&project, &name),
    }
}

fn list(project: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    if p.workers.is_empty() {
        println!("(no workers declared in {project} — add a `workers:` block to the project YAML)");
        return Ok(());
    }

    // Build a {worker -> task_id} index from in-progress tasks. There
    // should be at most one in-progress task per worker, but if shelbi's
    // state diverged we surface all of them so the user can see the mess.
    let in_progress = shelbi_state::list_column(project, Column::InProgress)
        .map_err(|e| anyhow!(e))?;

    for worker in &p.workers {
        let machine = p
            .machine(&worker.machine)
            .ok_or_else(|| anyhow!("worker `{}` references unknown machine `{}`", worker.name, worker.machine))?;
        let host = machine.host();
        let addr = orch_worker::worker_tmux_addr(&p, worker).map_err(|e| anyhow!(e))?;
        let alive = orch_worker::worker_pane_alive(&host, &addr).unwrap_or(false);

        let mine: Vec<&str> = in_progress
            .iter()
            .filter(|tf| tf.task.assigned_to.as_deref() == Some(worker.name.as_str()))
            .map(|tf| tf.task.id.as_str())
            .collect();

        let pane_state = if alive { "●" } else { "·" };
        let task_summary = if mine.is_empty() {
            "(idle)".to_string()
        } else {
            mine.join(", ")
        };
        println!(
            "{pane_state} {:<12} {:<8} {:<8} {}",
            worker.name, worker.machine, worker.runner, task_summary
        );
    }
    Ok(())
}

fn stop(project: &str, name: &str) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let worker = p.worker(name).ok_or_else(|| {
        anyhow!(
            "worker `{name}` not declared in project `{project}` (known: {})",
            p.workers
                .iter()
                .map(|w| w.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )
    })?;
    let machine = p
        .machine(&worker.machine)
        .ok_or_else(|| anyhow!("worker references unknown machine `{}`", worker.machine))?;
    let host = machine.host();
    let addr = orch_worker::worker_tmux_addr(&p, worker).map_err(|e| anyhow!(e))?;
    orch_worker::kill_worker_pane(&host, &addr).map_err(|e| anyhow!(e))?;
    println!("✓ {name} pane stopped");
    Ok(())
}
