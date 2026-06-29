use anyhow::{anyhow, Result};
use shelbi_orchestrator::{PaneReloadStatus, ReloadReport};

use super::init::print_agent_materialize_outcome;
use super::require_project;

/// Respawn the four shelbi-owned panes (sidebar + tasks/review/machines
/// stash) in-place, then self-heal the per-project agent workspaces
/// (`agents/{orchestrator,developer}/`) so a freshly installed binary
/// that ships an updated default prompt — or a wiped agent directory —
/// lands on disk without forcing the user to recreate the project.
/// User-edited `instructions.md` files are preserved byte-for-byte.
pub fn run(project_opt: Option<String>) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let report = shelbi_orchestrator::reload(&project_name).map_err(|e| anyhow!(e))?;
    print_report(&project_name, &report);
    let outcomes = shelbi_state::self_heal_default_agents(&project_name)
        .map_err(|e| anyhow!(e))?;
    for outcome in outcomes {
        print_agent_materialize_outcome(&outcome);
    }
    Ok(())
}

fn print_report(project: &str, r: &ReloadReport) {
    println!("reload · {project}");
    print_pane("sidebar", &r.sidebar);
    print_pane("tasks", &r.tasks);
    print_pane("review", &r.review);
    print_pane("machines", &r.machines);
    print_pane("activity", &r.activity);
}

fn print_pane(name: &str, status: &PaneReloadStatus) {
    match status {
        PaneReloadStatus::Respawned { target } => {
            println!("  ✓ {name:<9} respawned ({target})");
        }
        PaneReloadStatus::Created { target } => {
            println!("  ✓ {name:<9} created   ({target})");
        }
        PaneReloadStatus::Missing => {
            println!("  ⚠ {name:<9} no stored pane id; skipped");
        }
        PaneReloadStatus::Failed { target, reason } => {
            println!("  ✗ {name:<9} failed ({target}): {reason}");
        }
        PaneReloadStatus::NotAttempted => {
            println!("  · {name:<9} not attempted");
        }
    }
}
