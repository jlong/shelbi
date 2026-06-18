use anyhow::{anyhow, Result};

use super::require_project;

pub fn run(project: Option<String>, id: Option<String>) -> Result<()> {
    let project = require_project(project)?;
    match id {
        None => super::list::run(Some(project)),
        Some(id) => {
            let file = shelbi_state::load_agent(&project, &id).map_err(|e| anyhow!(e))?;
            let a = &file.agent;
            println!("id:       {}", a.id);
            println!("project:  {}", a.project);
            println!("machine:  {}", a.machine);
            println!("runner:   {}", a.runner);
            println!("branch:   {}", a.branch);
            println!("worktree: {}", a.worktree.display());
            println!("status:   {} {:?}", a.status.glyph(), a.status);
            println!("tmux:     {}:{}", a.tmux.session, a.tmux.window);
            println!("created:  {}", a.created);
            println!("updated:  {}", a.updated);
            Ok(())
        }
    }
}
