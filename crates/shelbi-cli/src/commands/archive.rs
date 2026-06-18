use anyhow::{anyhow, Result};
use chrono::Utc;
use shelbi_core::Status;

use super::require_project;

pub fn run(project: Option<String>, id: String) -> Result<()> {
    let project_name = require_project(project)?;
    let mut file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let machine = project
        .machine(&file.agent.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?;
    let host = machine.host();

    // Best-effort: kill the tmux window and remove the worktree. Don't fail
    // the whole archive if these are already gone.
    let _ = shelbi_tmux::kill_window(&host, &file.agent.tmux);
    let repo_dir = machine.work_dir.to_string_lossy().into_owned();
    let _ = shelbi_ssh::run_capture(
        &host,
        [
            "git",
            "-C",
            &repo_dir,
            "worktree",
            "remove",
            "--force",
            &file.agent.worktree.to_string_lossy(),
        ],
    );

    file.agent.status = Status::Archived;
    file.agent.updated = Utc::now();
    shelbi_state::save_agent(&project_name, &file.agent, &file.body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(&project_name, &id, "archive").map_err(|e| anyhow!(e))?;
    println!("✓ archived {id}");
    Ok(())
}
