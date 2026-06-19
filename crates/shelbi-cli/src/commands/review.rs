//! `shelbi review <task-id>` — bring a ready-for-review task into the
//! machine's main work_dir (the "review repository") and spin up a fresh
//! review-claude pane there.

use anyhow::{anyhow, bail, Result};
use clap::Args as ClapArgs;
use shelbi_core::Host;
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
    focus_review(&machine.host(), &addr)?;
    Ok(())
}

/// Drop the user into the review pane. Local + inside tmux: switch the
/// active client's window. Otherwise (not inside tmux, or a remote review
/// session that lives on a different tmux server) print the attach command
/// for the user to run — we can't steal their terminal from this process.
fn focus_review(host: &Host, addr: &shelbi_core::TmuxAddr) -> Result<()> {
    let target = addr.target();
    match host {
        Host::Local => {
            if std::env::var("TMUX").is_ok() {
                let out = std::process::Command::new("tmux")
                    .args(["select-window", "-t", &target])
                    .status()?;
                if !out.success() {
                    bail!("tmux select-window failed");
                }
            } else {
                println!("attach with:");
                println!("  tmux attach -t {} \\; select-window -t {}", addr.session, addr.window);
            }
        }
        Host::Ssh { host } => {
            println!("attach with:");
            println!("  ssh -t {host} -- tmux attach -t {}", addr.session);
        }
    }
    Ok(())
}
