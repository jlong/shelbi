//! `shelbi review <task-id>` — bring a ready-for-review task into the
//! machine's main work_dir (the "review repository") and spin up a fresh
//! review-claude pane there.

use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use shelbi_orchestrator::review as orch_review;

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Task id to review.
    pub id: String,
    /// Override the machine to review on. Defaults to the machine of the
    /// task's assigned worker, or the first local machine otherwise.
    #[arg(long)]
    pub machine: Option<String>,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let tf = shelbi_state::load_task(&project_name, &args.id).map_err(|e| anyhow!(e))?;

    let machine =
        orch_review::resolve_review_machine(&project, &tf.task, args.machine.as_deref())
            .map_err(|e| anyhow!(e))?;

    println!("→ reviewing {} on {} ({})", tf.task.id, machine.name, machine.work_dir.display());
    let addr = orch_review::start_review(orch_review::ReviewSpec {
        project: &project,
        machine,
        task: &tf.task,
        task_body: &tf.body,
    })
    .map_err(|e| anyhow!(e))?;

    println!("✓ checked out, review pane up at {}", addr.target());
    Ok(())
}
