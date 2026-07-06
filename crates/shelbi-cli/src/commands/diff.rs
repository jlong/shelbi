use anyhow::{anyhow, Result};

use super::require_project;

pub fn run(project: Option<String>, id: String) -> Result<()> {
    let project_name = require_project(project)?;
    let file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let machine = project
        .machine(&file.agent.machine)
        .ok_or_else(|| anyhow!("machine `{}` no longer in project", file.agent.machine))?;
    let host = machine.host();

    let wt = file.agent.worktree.to_string_lossy().into_owned();
    let parent_branch = project.base_branch().to_string();
    let merge_base = shelbi_ssh::run_capture(
        &host,
        ["git", "-C", &wt, "merge-base", &parent_branch, "HEAD"],
    )
    .map_err(|e| anyhow!(e))?;
    let base = merge_base.trim();

    let diff = shelbi_ssh::run_capture(&host, ["git", "-C", &wt, "diff", &format!("{base}..HEAD")])
        .map_err(|e| anyhow!(e))?;
    let working =
        shelbi_ssh::run_capture(&host, ["git", "-C", &wt, "diff"]).map_err(|e| anyhow!(e))?;

    if diff.is_empty() && working.is_empty() {
        println!("(no diff)");
    } else {
        print!("{diff}");
        if !working.is_empty() {
            println!("\n--- uncommitted changes ---");
            print!("{working}");
        }
    }
    Ok(())
}
