use anyhow::{anyhow, Result};
use shelbi_orchestrator::handoff::HandoffOutcome;
use shelbi_orchestrator::{PaneReloadStatus, ReloadReport, ReloadTarget};
use shelbi_state::WorkspaceSettingsTemplateOutcome;

use super::init::print_agent_materialize_outcome;
use super::require_project;

/// Respawn the shelbi-owned panes (sidebar + tasks/machines
/// stash) AND the orchestrator pane in-place, then self-heal the
/// per-project agent workspaces (`agents/{orchestrator,developer}/`)
/// and the workspace-settings template so a freshly installed binary
/// that ships updated defaults — or a wiped/stale on-disk copy —
/// lands without forcing the user to recreate the project.
///
/// Before respawning the orchestrator pane, the previous instance is
/// asked to write `agents/orchestrator/handoff.md` covering its
/// in-flight state. The new instance ingests that file as a
/// `<system-reminder>` block in its system prompt and deletes it, so
/// `shelbi reload` carries the orchestrator's mid-thought context
/// forward instead of starting cold. A missing or timed-out handoff
/// is degraded (next orchestrator starts cold) but not fatal.
///
/// User-edited `instructions.md` files are preserved byte-for-byte;
/// the workspace-settings template is always re-aligned with the
/// shipped default (users who want customization point
/// `workspace_settings_template` at their own file).
pub fn run(
    project_opt: Option<String>,
    target: Option<String>,
    name: Option<String>,
) -> Result<()> {
    let target =
        ReloadTarget::parse(target.as_deref(), name.as_deref()).map_err(|e| anyhow!(e))?;

    // A targeted pane reload respawns one pane in place and deliberately
    // skips the whole-hub self-heal (root/subdir re-materialization,
    // workflow + statuses compatibility migration, agent-workspace and
    // settings-template repair, legacy-marker sweep). Each target carries
    // its own dependency refresh: `chat` re-deploys the orchestrator agent
    // context inside the respawn, and the TUI panes render derived state
    // straight from disk.
    if !matches!(target, ReloadTarget::All) {
        let project_name = require_project(project_opt)?;
        let report =
            shelbi_orchestrator::reload_target(&project_name, &target).map_err(|e| anyhow!(e))?;
        print_report(&project_name, &report);
        return Ok(());
    }

    run_all(project_opt)
}

/// The whole-hub reload: sweep legacy markers, self-heal the project's
/// materialized state, then respawn every shelbi-owned pane and the
/// orchestrator.
fn run_all(project_opt: Option<String>) -> Result<()> {
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
    // Explicit compatibility materialization for projects created before
    // workflows/statuses.yaml and the shipped workflow files were split out.
    // Ordinary project loads stay read-only with respect to these files, but
    // `shelbi reload` remains the user-facing repair path.
    //
    // The default-workflow migration self-guards: it only writes the
    // task.yaml / subtask.yaml that are actually missing, and only rewrites
    // `default_workflow:` when it is unset or the legacy `default` — a
    // deliberate custom default is left alone, and no task frontmatter or
    // existing `default.yaml` is touched.
    let wf_migration =
        shelbi_state::migrate_default_workflow_to_task(&project_name).map_err(|e| anyhow!(e))?;
    print_default_workflow_migration(&wf_migration);
    let statuses_path = shelbi_state::statuses_path(&project_name).map_err(|e| anyhow!(e))?;
    if !statuses_path.exists() {
        shelbi_state::scaffold_project_statuses(&project_name).map_err(|e| anyhow!(e))?;
    }
    // Self-heal `zenmode.md` for projects that predate it — written only when
    // absent, so a user's edits (including the first-line Zen summary the
    // heartbeat re-injects) are preserved byte-for-byte, same as
    // `instructions.md`.
    if shelbi_state::scaffold_zenmode(&project_name).map_err(|e| anyhow!(e))?
        == shelbi_state::ZenmodeOutcome::Created
    {
        let zenmode_path = shelbi_state::zenmode_path(&project_name).map_err(|e| anyhow!(e))?;
        println!("✓ wrote Zen policy: {} (was missing)", zenmode_path.display());
    }
    let outcomes = shelbi_state::self_heal_default_agents(&project_name).map_err(|e| anyhow!(e))?;
    let report = shelbi_orchestrator::reload(&project_name).map_err(|e| anyhow!(e))?;
    print_report(&project_name, &report);
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

fn print_default_workflow_migration(m: &shelbi_state::DefaultWorkflowMigration) {
    for path in &m.created_workflows {
        println!("✓ wrote {} (was missing)", path.display());
    }
    if m.default_workflow_set_to_task {
        println!("✓ set default_workflow: task (migrated from the legacy `default`)");
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
        WorkspaceSettingsTemplateOutcome::Overwritten {
            had_legacy_placeholder,
        } => {
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
    // A whole-hub reload attempts every pane; a targeted reload leaves the
    // untouched panes `NotAttempted`. Skip those so a targeted reload prints
    // only the pane(s) it actually respawned.
    print_pane_if_attempted("sidebar", &r.sidebar);
    print_pane_if_attempted("tasks", &r.tasks);
    print_pane_if_attempted("machines", &r.machines);
    print_pane_if_attempted("activity", &r.activity);
    if let Some(h) = &r.handoff {
        print_handoff(h);
    }
    print_pane_if_attempted("orch", &r.orchestrator);
    if let Some(ws) = &r.workspace {
        print_pane(&format!("ws:{}", ws.name), &ws.status);
    }
}

fn print_pane_if_attempted(name: &str, status: &PaneReloadStatus) {
    if matches!(status, PaneReloadStatus::NotAttempted) {
        return;
    }
    print_pane(name, status);
}

fn print_handoff(outcome: &HandoffOutcome) {
    match outcome {
        HandoffOutcome::NativeThread => {
            println!("  · handoff   skipped (Codex native thread retained)");
        }
        HandoffOutcome::Written { path } => {
            println!("  ✓ handoff   captured ({})", path.display());
        }
        HandoffOutcome::PaneNotAlive => {
            println!("  · handoff   skipped (orchestrator pane not running)");
        }
        HandoffOutcome::Timeout => {
            println!(
                "  ⚠ handoff   timed out waiting for the orchestrator to write \
                 handoff.md; next start will be cold"
            );
        }
        HandoffOutcome::SendFailed { reason } => {
            println!("  ⚠ handoff   couldn't ask the orchestrator: {reason}");
        }
    }
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
