//! `shelbi review <task-id>` — load a ready-for-review task onto a **review
//! workspace** (a `role: review` pool slot) and start the review agent there to
//! serve the branch for a human. When every review workspace on the resolved
//! machine is busy, it reports the task's queue position rather than failing.
//!
//! This replaces the retired top-level-clone checkout: the branch now lives in
//! the review workspace's own worktree, so the machine's main clone is never
//! dirtied by review. A project that declares no review workspace gets a loud
//! onboarding error from [`shelbi_orchestrator::review::review_task`].

use anyhow::{anyhow, bail, Result};
use clap::Args as ClapArgs;
use shelbi_core::Host;
use shelbi_orchestrator::review::{self as orch_review, ReviewOutcome};

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Task id to review.
    pub id: String,
    /// Override the machine to review on. Defaults to the machine of the
    /// task's assigned workspace (when it has review workspaces), else the
    /// first machine that declares a `role: review` workspace.
    #[arg(long)]
    pub machine: Option<String>,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;

    match orch_review::review_task(&project_name, &args.id, args.machine.as_deref())
        .map_err(|e| anyhow!(e))?
    {
        ReviewOutcome::Loaded { workspace, machine, host, addr, port } => {
            let where_at = match port {
                Some(p) => format!("{workspace} on {machine} (:{p})"),
                None => format!("{workspace} on {machine}"),
            };
            println!("→ loading {} onto review workspace {where_at}", args.id);
            println!("✓ review agent up at {}", addr.target());
            focus_review(&host, &addr)?;
        }
        ReviewOutcome::Queued { machine, position, busy } => {
            println!(
                "⏳ all review workspaces on {machine} are busy — `{}` is queued at position {position}",
                args.id
            );
            for slot in &busy {
                println!("   {} → {}", slot.workspace, slot.task_id);
            }
            println!("It will load automatically when a review workspace frees.");
        }
    }
    Ok(())
}

/// Drop the user into the review pane. Local + inside tmux: switch the
/// active client's window. Otherwise (not inside tmux, or a remote review
/// workspace that lives on a different tmux server) print the attach command
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
