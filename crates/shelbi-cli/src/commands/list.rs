use std::fs;

use anyhow::{anyhow, Result};
use shelbi_core::Status;

use super::require_project;

pub fn run(project: Option<String>) -> Result<()> {
    let project = require_project(project)?;
    let dir = shelbi_state::agents_dir(&project).map_err(|e| anyhow!(e))?;
    if !dir.exists() {
        println!("(no agents yet)");
        return Ok(());
    }
    let mut rows: Vec<(String, String, String, Status)> = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Skip the `.log.md` companions.
        if name.ends_with(".log.md") || !name.ends_with(".md") {
            continue;
        }
        let id = name.trim_end_matches(".md");
        let file = shelbi_state::load_agent(&project, id).map_err(|e| anyhow!(e))?;
        rows.push((
            file.agent.id.clone(),
            file.agent.machine.clone(),
            file.agent.branch.clone(),
            file.agent.status,
        ));
    }
    rows.sort_by_key(|(_, _, _, st)| status_order(*st));
    if rows.is_empty() {
        println!("(no agents yet)");
        return Ok(());
    }
    for (id, machine, branch, status) in rows {
        println!(
            "{} {:<24} {:<10} {}",
            status.glyph(),
            id,
            machine,
            branch
        );
    }
    Ok(())
}

fn status_order(s: Status) -> u8 {
    match s {
        Status::Running => 0,
        Status::Waiting => 1,
        Status::Queued => 2,
        Status::Done => 3,
        Status::Error => 4,
        Status::Archived => 5,
    }
}
