use anyhow::{anyhow, Result};
use shelbi_orchestrator::{PaneReloadStatus, ReloadReport};
use shelbi_state::WorkspaceSettingsTemplateOutcome;

use super::init::print_agent_materialize_outcome;
use super::require_project;

/// Respawn the four shelbi-owned panes (sidebar + tasks/review/machines
/// stash) in-place, then self-heal the per-project agent workspaces
/// (`agents/{orchestrator,developer}/`) and the workspace-settings
/// template so a freshly installed binary that ships updated defaults —
/// or a wiped/stale on-disk copy — lands without forcing the user to
/// recreate the project. User-edited `instructions.md` files are
/// preserved byte-for-byte; the workspace-settings template is always
/// re-aligned with the shipped default (users who want customization
/// point `workspace_settings_template` at their own file).
pub fn run(project_opt: Option<String>) -> Result<()> {
    // Migration hook for the dropped `.shelbi/project` marker: sweep every
    // registered project's work_dir, delete any leftover marker, and warn
    // about work_dirs that have gone missing. Runs before `require_project`
    // so the cleanup happens even when this invocation targets one project.
    cleanup_legacy_markers();

    let project_name = require_project(project_opt)?;
    // Re-materialize the resolved root + standard subdirectories before
    // the reload work runs. If the user nuked ~/.shelbi (or pointed
    // --root at a fresh path), this puts the layout back; if the root is
    // unwritable, it hard-fails with a source-tagged error.
    shelbi_state::ensure_root_subdirs().map_err(|e| anyhow!(e))?;
    // Touching `load_project` runs the `workflows/statuses.yml` +
    // `workflows/default.yaml` materialization migration as a side
    // effect. The pane respawn below already triggers `load_project`
    // indirectly, but doing it here makes the contract explicit:
    // `shelbi reload` always leaves both files on disk.
    let _ = shelbi_state::load_project(&project_name);
    let report = shelbi_orchestrator::reload(&project_name).map_err(|e| anyhow!(e))?;
    print_report(&project_name, &report);
    let outcomes = shelbi_state::self_heal_default_agents(&project_name)
        .map_err(|e| anyhow!(e))?;
    for outcome in outcomes {
        print_agent_materialize_outcome(&outcome);
    }
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let template_outcome =
        shelbi_state::self_heal_workspace_settings_template(&project).map_err(|e| anyhow!(e))?;
    print_workspace_settings_template_outcome(&template_outcome);
    Ok(())
}

/// Sweep registered project trees for the now-redundant `.shelbi/project`
/// marker and report missing work_dirs. Best-effort — a scan failure here
/// shouldn't block the pane respawn that is reload's primary job.
fn cleanup_legacy_markers() {
    let report = match shelbi_state::cleanup_legacy_markers() {
        Ok(r) => r,
        Err(_) => return,
    };
    for c in report {
        if c.marker_removed {
            println!(
                "shelbi: cleaned up legacy .shelbi/project marker in {} \
                 (resolution now uses ~/.shelbi/projects/*.yaml)",
                c.work_dir.display()
            );
        }
        if c.work_dir_missing {
            eprintln!(
                "shelbi: warning: project '{}' work_dir {} no longer exists — \
                 it won't resolve until you re-point or remove it",
                c.name,
                c.work_dir.display()
            );
        }
    }
}

fn print_workspace_settings_template_outcome(outcome: &WorkspaceSettingsTemplateOutcome) {
    match outcome {
        WorkspaceSettingsTemplateOutcome::SkippedOverride => {
            println!(
                "(skipped workspace-settings.json.template self-heal: project uses a \
                 custom `workspace_settings_template` path)"
            );
        }
        WorkspaceSettingsTemplateOutcome::Created => {
            println!("✓ wrote workspace-settings.json.template (was missing)");
        }
        WorkspaceSettingsTemplateOutcome::Unchanged => {
            println!("(workspace-settings.json.template already matches the shipped default)");
        }
        WorkspaceSettingsTemplateOutcome::Overwritten { had_legacy_placeholder } => {
            if *had_legacy_placeholder {
                println!(
                    "✓ healed workspace-settings.json.template — stale \
                     `{{{{worker_*}}}}` placeholder replaced with the shipped default"
                );
            } else {
                println!(
                    "✓ overwrote workspace-settings.json.template — on-disk copy diverged \
                     from the shipped default"
                );
            }
        }
    }
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
