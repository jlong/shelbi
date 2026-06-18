use anyhow::{anyhow, bail, Result};

use super::require_project;

pub fn run(project: Option<String>, id: String, pr: bool) -> Result<()> {
    let project_name = require_project(project)?;
    let _file = shelbi_state::load_agent(&project_name, &id).map_err(|e| anyhow!(e))?;
    if pr {
        bail!("`shelbi merge --pr` lands in Phase 7 (GitHub PR flow)");
    }
    bail!("local `shelbi merge` lands in Phase 7. For now, merge by hand in the worktree.");
}
