use anyhow::{anyhow, Result};
use chrono::Utc;
use shelbi_core::Status;

use super::require_project;

pub fn run(project: Option<String>, id: String, message: String) -> Result<()> {
    let project_name = require_project(project)?;
    let mut file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;

    // Resolve the worker's host via its project / machine.
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let machine = project
        .machine(&file.agent.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?;
    let host = machine.host();

    shelbi_tmux::send_line(&host, &file.agent.tmux, &message).map_err(|e| anyhow!(e))?;

    file.agent.status = Status::Running;
    file.agent.updated = Utc::now();
    shelbi_state::save_agent(&project_name, &file.agent, &file.body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(&project_name, &id, &format!("send: {message}"))
        .map_err(|e| anyhow!(e))?;

    println!("✓ sent");
    Ok(())
}
