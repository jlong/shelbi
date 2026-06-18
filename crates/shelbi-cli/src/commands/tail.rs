use anyhow::{anyhow, Result};

use super::require_project;

pub fn run(project: Option<String>, id: String, lines: usize) -> Result<()> {
    let project_name = require_project(project)?;
    let file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let machine = project
        .machine(&file.agent.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?;
    let host = machine.host();

    let out = shelbi_tmux::capture_history(&host, &file.agent.tmux, lines)
        .map_err(|e| anyhow!(e))?;
    print!("{}", out);
    if !out.ends_with('\n') {
        println!();
    }
    Ok(())
}
