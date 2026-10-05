use anyhow::{anyhow, Result};
use shelbi_orchestrator::ReloadTarget;
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
/// User-edited `instructions.md` prose is preserved. Exact legacy Zen PR
/// command tokens are pinned in place, and required orchestrator sections
/// may be repaired; the workspace-settings template is always re-aligned
/// with the shipped default (users who want customization point
/// `workspace_settings_template` at their own file).
pub fn run(
    project_opt: Option<String>,
    target: Option<String>,
    name: Option<String>,
) -> Result<()> {
    let target =
        ReloadTarget::parse(target.as_deref(), name.as_deref()).map_err(|e| anyhow!(e))?;

    // On the session runtime, `reload` restarts the daemon, re-execs attached
    // clients, and replaces the orchestrator *session* (workers keep running).
    // There are no per-pane processes to reload, so a targeted pane reload no
    // longer applies — every invocation runs the whole-project reload.
    if !matches!(target, ReloadTarget::All) {
        eprintln!(
            "note: targeted pane reloads no longer apply on the session runtime; \
             reloading the whole project"
        );
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
    let outcomes = self_heal_zen_automation(&project_name, true)?;
    // Version-agnostic config validate-and-upgrade pass: detect + classify
    // every legacy/deprecated form, apply the auto-heal write-backs (disclosing
    // each on events.log as a `config-upgrade` line), and write the
    // orchestrator-handoff findings file for what still needs judgment. Runs
    // after the existing self-heals (so it reconciles with, rather than fights,
    // them) and before the orchestrator respawns (so a fresh instance ingests
    // the residual findings).
    let upgrade = crate::commands::config_upgrade::run_for_project(&project_name);
    print_config_upgrade_summary(&project_name, &upgrade);
    // Session-runtime reload: restart the daemon, re-exec attached clients, and
    // replace the orchestrator session (workers left running).
    let ops = super::reload_session::LiveReload {
        project: project_name.clone(),
    };
    let report = super::reload_session::run(&ops)?;
    println!("reload · {project_name} (session runtime)");
    println!("  · handoff   {}", report.handoff_status);
    println!("  ✓ clients   signalled to re-exec");
    println!("  ✓ daemon    restarted on the current binary");
    println!("  ✓ orch      session replaced (workers left running)");
    for outcome in outcomes {
        print_agent_materialize_outcome(&outcome);
    }
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;
    let template_outcome =
        shelbi_state::self_heal_workspace_settings_template(&project).map_err(|e| anyhow!(e))?;
    print_workspace_settings_template_outcome(&template_outcome);
    Ok(())
}

/// Upgrade the two policy files that can carry the stock Zen PR sequence
/// before an orchestrator is respawned. The state layer preserves custom
/// prose and rewrites only exact legacy command tokens in customized files.
fn self_heal_zen_automation(
    project_name: &str,
    all_agents: bool,
) -> Result<Vec<shelbi_state::AgentMaterializeOutcome>> {
    match shelbi_state::scaffold_zenmode(project_name).map_err(|e| anyhow!(e))? {
        shelbi_state::ZenmodeOutcome::Created => {
            let path = shelbi_state::zenmode_path(project_name).map_err(|e| anyhow!(e))?;
            println!("✓ wrote Zen policy: {} (was missing)", path.display());
        }
        shelbi_state::ZenmodeOutcome::Migrated => {
            let path = shelbi_state::zenmode_path(project_name).map_err(|e| anyhow!(e))?;
            println!(
                "✓ pinned legacy Zen PR commands in {} (custom prose preserved)",
                path.display()
            );
        }
        shelbi_state::ZenmodeOutcome::Unchanged => {}
    }
    if all_agents {
        shelbi_state::self_heal_default_agents(project_name).map_err(|e| anyhow!(e))
    } else {
        let outcome = shelbi_state::self_heal_orchestrator_agent(project_name)
            .map_err(|e| anyhow!(e))?;
        Ok(vec![outcome])
    }
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

/// Summarize the config validate-and-upgrade pass. Silent when the config is
/// already current so a clean reload stays quiet; otherwise it points the user
/// at the read-only `shelbi config upgrade` detail and notes that
/// needs-judgment findings were handed to the orchestrator.
fn print_config_upgrade_summary(
    project: &str,
    outcome: &crate::commands::config_upgrade::UpgradeOutcome,
) {
    let healed = outcome.applied.len();
    if healed > 0 {
        println!(
            "config-upgrade: auto-healed {healed} legacy config form(s) for `{project}` \
             (each disclosed on events.log)"
        );
    }
    let residual = &outcome.residual;
    if residual.is_empty() {
        return;
    }
    let auto = residual.auto_heal_count();
    let judg = residual.needs_judgment_count();
    println!(
        "config-upgrade: {auto} auto-heal, {judg} needs-judgment finding(s) still open for \
         `{project}` (run `shelbi config upgrade -p {project}` for detail)"
    );
    if judg > 0 {
        println!(
            "  → {judg} needs-judgment finding(s) handed to the orchestrator to resolve with you \
             (`shelbi config upgrade --needs-judgment -p {project}` to view; \
             `--apply-finding <id>` to apply one you approve)"
        );
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








