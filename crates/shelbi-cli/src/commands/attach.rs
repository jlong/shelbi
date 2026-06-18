use anyhow::{anyhow, bail, Result};
use shelbi_core::{Host, MachineKind};

use super::require_project;

/// Attach the user's terminal to a worker's tmux pane.
///
/// Local worker: switch the user into `shelbi-<project>:w-<id>` (if they're
/// already inside a tmux client) or print the attach command.
/// Remote worker: print the `ssh -t host -- tmux attach -t shelbi-w-<id>`
/// command. We don't exec it directly — it would steal the user's terminal
/// from this process; safer to let them run it.
pub fn run(project_opt: Option<String>, id: String) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let machine = project
        .machine(&file.agent.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?;

    match (&machine.kind, machine.host()) {
        (MachineKind::Local, _) => {
            let target = format!("{}:{}", file.agent.tmux.session, file.agent.tmux.window);
            if std::env::var("TMUX").is_ok() {
                // Inside tmux — switch the active client's window.
                let out = std::process::Command::new("tmux")
                    .args(["select-window", "-t", &target])
                    .status()?;
                if !out.success() {
                    bail!("tmux select-window failed");
                }
            } else {
                println!("attach with:");
                println!("  tmux attach -t {target}");
            }
        }
        (MachineKind::Ssh, Host::Ssh { host }) => {
            let target = file.agent.tmux.target();
            println!("attach with:");
            println!("  ssh -t {host} -- tmux attach -t {target}");
        }
        _ => unreachable!(),
    }
    Ok(())
}
