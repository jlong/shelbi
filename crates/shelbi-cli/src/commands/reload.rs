use anyhow::{anyhow, Result};
use shelbi_orchestrator::{PaneReloadStatus, ReloadReport};

use super::require_project;

/// Respawn the four shelbi-owned panes (sidebar + tasks/review/machines
/// stash) in-place. Used after installing a new shelbi binary — the
/// long-lived processes hold the old code in memory until they're
/// respawned.
pub fn run(project_opt: Option<String>) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let report = shelbi_orchestrator::reload(&project_name).map_err(|e| anyhow!(e))?;
    print_report(&project_name, &report);
    Ok(())
}

fn print_report(project: &str, r: &ReloadReport) {
    println!("reload · {project}");
    print_pane("sidebar", &r.sidebar);
    print_pane("tasks", &r.tasks);
    print_pane("review", &r.review);
    print_pane("machines", &r.machines);
}

fn print_pane(name: &str, status: &PaneReloadStatus) {
    match status {
        PaneReloadStatus::Respawned { target } => {
            println!("  ✓ {name:<9} respawned ({target})");
        }
        PaneReloadStatus::Missing => {
            println!("  · {name:<9} no stored pane id; skipped");
        }
        PaneReloadStatus::Failed { target, reason } => {
            println!("  ✗ {name:<9} failed ({target}): {reason}");
        }
        PaneReloadStatus::NotAttempted => {
            println!("  · {name:<9} not attempted");
        }
    }
}
