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
    let addr = shelbi_orchestrator::orchestrator_addr(&project_name);
    let status = shelbi_orchestrator::ensure_running(&project_name).map_err(|e| anyhow!(e))?;

    match status {
        BootstrapStatus::Started => {
            println!("✓ orchestrator started in {}", addr.target());
        }
        BootstrapStatus::AlreadyRunning => {
            if args.status {
                println!("orchestrator already running in {}", addr.target());
            } else {
                println!("orchestrator already running in {}", addr.target());
            }
        }
    }
    print_attach(&addr.session, &addr.window);
    Ok(())
}

fn print_attach(session: &str, window: &str) {
    println!();
    println!("attach with:");
    if std::env::var("TMUX").is_ok() {
        println!("  tmux select-window -t {session}:{window}");
    } else {
        println!("  tmux attach -t {session} \\; select-window -t {window}");
    }
}
