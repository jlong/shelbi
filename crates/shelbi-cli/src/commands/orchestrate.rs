use anyhow::{anyhow, Result};
use clap::Args as ClapArgs;
use shelbi_orchestrator::BootstrapStatus;

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Print attach instructions and exit even if the orchestrator was
    /// already running.
    #[arg(long)]
    pub status: bool,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let addr = shelbi_orchestrator::dashboard_addr(&project_name);
    // Opening a project starts the on-demand hub daemon if needed. Best-effort:
    // a daemon that won't start shouldn't block the orchestrator bootstrap.
    if let Err(e) = shelbi_state::ensure_daemon_running() {
        eprintln!("shelbi: warning: could not start the hub daemon: {e}");
    }
    let status = shelbi_orchestrator::ensure_dashboard(&project_name).map_err(|e| anyhow!(e))?;

    match status {
        BootstrapStatus::Started => {
            println!("✓ orchestrator started ({})", addr.label());
        }
        BootstrapStatus::AlreadyRunning => {
            // `args.status` is reserved for future status-only output; for
            // now both branches print the same line.
            let _ = args.status;
            println!("orchestrator already running ({})", addr.label());
        }
    }
    println!();
    println!("open the project with `shelbi` to see the orchestrator.");
    Ok(())
}
