//! Workflow loader — discover and parse workflow YAML files from a
//! project's `workflows/` directory, with a built-in fallback so legacy
//! projects (no workflows configured) keep working.
//!
//! Workflows live at `~/.shelbi/projects/<project>/workflows/<name>.yaml`.
//! Per-project **status identity** (id, name, category, declared order)
//! lives in a sibling `workflows/statuses.yaml`; each workflow file
//! references those ids and adds workflow-scoped fields (owner +
//! optional agent). The loader joins the two on read.
//!
//! [`list_workflows`] is the entry point downstream code reaches for:
//! when the directory is missing or holds no `.yaml` files, it returns
//! [`shelbi_core::default_workflow()`] instead of an empty list, so
//! every caller can assume at least one workflow is available.
//!
//! ## Two-field owner / agent validation
//!
//! Each `agent: <name>` reference in a loaded workflow must point at a
//! materialized subdirectory of `~/.shelbi/projects/<project>/agents/`.
//! The loader walks the agents directory when one exists and rejects
//! workflows that reference unknown agents, listing what *is* available
//! so the user can fix the workflow YAML in one shot. Projects that
//! pre-date the agent-workspaces feature (no `agents/` directory yet)
//! skip the check — the workflow loads as-is and the next `shelbi
//! reload` materializes the defaults.
//!
//! ## statuses.yaml requirement
//!
//! `workflows/statuses.yaml` is the project-wide source of truth for
//! status identity (id, name, category, ordering). Whenever at least
//! one workflow file is on disk, the loader requires `statuses.yaml`
//! and hard-fails when it's missing — `shelbi init` and `shelbi
//! reload` materialize the shipped default, so the user fix is "run
//! one of those" rather than hand-editing.
//!
//! Workflow files must use the reference-only form
//! (`id` + `owner` + optional `agent`). An inline `name:` or
//! `category:` under a `statuses:` entry — the pre-Phase-1 form
//! supported in a prior release — is rejected at parse time with a
//! pointer to move identity into `statuses.yaml`.
//!
//! ## Legacy migration warnings
//!
//! Workflow YAMLs authored before the two-field owner/agent split
//! ([`shelbi_core::Workflow::from_yaml_str_with_diagnostics`] surfaces
//! these) trigger a one-time-per-workflow deprecation warning. The
//! dedupe is process-local, keyed by workflow path — repeated calls to
//! `list_workflows` from a polling TUI emit the warning once. The
//! warning routes through `tracing::warn!` so TUI subcommands don't
//! paint it onto the alt-screen pane the sidebar / tasks / review
//! TUIs are drawing on.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use shelbi_core::{
    default_project_statuses, default_workflow, validate_workflow_name, Error, Project,
    ProjectStatuses, Result, Task, Workflow,
};

use crate::{agents_dir, atomic_write, config_project_dir};

/// Directory holding per-project workflow YAML files. Config path —
/// resolved config-mode-aware via [`config_project_dir`]: in-repo projects
/// resolve to `<repo>/.shelbi/workflows/`, global projects to
/// `~/.shelbi/projects/<project>/workflows/`.
pub fn workflows_dir(project: &str) -> Result<PathBuf> {
    Ok(config_project_dir(project)?.join("workflows"))
}

/// On-disk path of a single workflow by name:
/// `~/.shelbi/projects/<project>/workflows/<name>.yaml`.
pub fn workflow_path(project: &str, name: &str) -> Result<PathBuf> {
    validate_workflow_name(name)?;
    Ok(workflows_dir(project)?.join(format!("{name}.yaml")))
}

/// On-disk path of the project-wide status catalogue:
/// `~/.shelbi/projects/<project>/workflows/statuses.yaml`. This is the
/// canonical write path — reads go through [`statuses_read_path`], which
/// also accepts a legacy `.yml`.
pub fn statuses_path(project: &str) -> Result<PathBuf> {
    Ok(workflows_dir(project)?.join("statuses.yaml"))
}

/// Effective path to *read* the project's status catalogue. Prefers the
/// canonical `statuses.yaml`; falls back to a legacy `statuses.yml` when
/// only that is present — a project that predates the `.yaml`
/// standardization and hasn't been migrated yet. The normal load path
/// renames `.yml` → `.yaml` (via `migrate_statuses_extension` in
/// [`crate::load_project`]), so this fallback is transient; it only
/// matters for a reader that runs before that migration. Returns the
/// canonical `.yaml` path when neither file exists so "missing" is
/// reported against the extension shelbi actually writes.
fn statuses_read_path(project: &str) -> Result<PathBuf> {
    let canonical = statuses_path(project)?;
    if canonical.exists() {
        return Ok(canonical);
    }
    let legacy = workflows_dir(project)?.join("statuses.yml");
    if legacy.exists() {
        return Ok(legacy);
    }
    Ok(canonical)
}

/// Load and validate the project's `statuses.yaml`. Returns
/// [`default_project_statuses`] when the file is missing — callers
/// that need the strict "must exist" semantic should probe
/// [`statuses_path`] themselves before calling.
pub fn load_project_statuses(project: &str) -> Result<ProjectStatuses> {
    let path = statuses_read_path(project)?;
    // Read unconditionally and map only NotFound to the default — an
    // `exists()` probe also reports false on EACCES/ELOOP, which would
    // make a transiently unreadable file look missing.
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(default_project_statuses());
        }
        Err(e) => return Err(Error::Io(e)),
    };
    let statuses = ProjectStatuses::from_yaml_str(&text).map_err(|e| annotate(&path, e))?;
    emit_status_warnings_once(&path, &statuses);
    Ok(statuses)
}

/// Atomic write of `statuses.yaml` for `project`. Creates the workflows/
/// dir on demand.
pub fn save_project_statuses(project: &str, statuses: &ProjectStatuses) -> Result<()> {
    let path = statuses_path(project)?;
    atomic_write(&path, serde_yaml::to_string(statuses)?.as_bytes())
}

/// Write the self-documenting default `statuses.yaml` for a fresh project —
/// the canonical six statuses plus a docs-linked header and a commented
/// example for adding custom statuses (see
/// [`shelbi_core::scaffold::default_statuses_yaml`]). Used by `shelbi init`;
/// the explicit migration path writes the same content for projects that
/// pre-date the file. Creates the workflows/ dir on demand.
pub fn scaffold_project_statuses(project: &str) -> Result<()> {
    let path = statuses_path(project)?;
    let yaml = shelbi_core::scaffold::default_statuses_yaml()?;
    atomic_write(&path, yaml.as_bytes())
}

/// Write the self-documenting default workflow files for a fresh project:
/// `task.yaml` (the default track, with a review gate) and `subtask.yaml`
/// (a piece of a parent task, no PR). Each file is written **only when
/// absent**, so a re-run preserves user edits and a half-initialized project
/// (one file written, the other not) is completed rather than clobbered.
/// Used by `shelbi init` and explicit migration/reload paths; normal project
/// loads do not recreate these files when users remove them. Returns the
/// paths that were created (empty when both already existed).
pub fn scaffold_project_workflow(project: &str) -> Result<Vec<PathBuf>> {
    let files = [
        (
            shelbi_core::TASK_WORKFLOW_NAME,
            shelbi_core::scaffold::task_workflow_yaml()?,
        ),
        (
            shelbi_core::SUBTASK_WORKFLOW_NAME,
            shelbi_core::scaffold::subtask_workflow_yaml()?,
        ),
    ];
    let mut created = Vec::new();
    for (name, yaml) in files {
        let path = workflow_path(project, name)?;
        if path.exists() {
            continue;
        }
        atomic_write(&path, yaml.as_bytes())?;
        created.push(path);
    }
    Ok(created)
}

/// Load and validate a single workflow by name. Errors if the file is
/// missing, the YAML doesn't pass [`Workflow::validate`], the file
/// still carries pre-statuses.yaml inline identity fields, the project's
/// `workflows/statuses.yaml` is missing, or any `agent:` reference fails
/// the agents-directory existence check. The file's basename is *not*
/// substituted for the workflow's declared `name:` — callers that need
/// that contract should compare after loading.
///
/// Resolves the workflow against the project's `statuses.yaml`, which
/// must exist (the post-Phase-1 contract). `shelbi init` and `shelbi
/// reload` materialize the shipped default when missing.
pub fn load_workflow(project: &str, name: &str) -> Result<Workflow> {
    let name = resolve_workflow_alias(project, name)?;
    let path = workflow_path(project, &name)?;
    let text = fs::read_to_string(&path)?;

    let inline = Workflow::inline_identity_fields(&text).map_err(|e| annotate(&path, e))?;
    if !inline.is_empty() {
        return Err(mixed_form_error(&path, &inline));
    }

    let (wf, diags) =
        Workflow::from_yaml_str_with_diagnostics(&text).map_err(|e| annotate(&path, e))?;
    emit_deprecation_warnings_once(&path, &diags);

    let st_path = statuses_read_path(project)?;
    if !st_path.exists() {
        return Err(missing_statuses_error(&st_path));
    }
    let statuses = load_project_statuses(project)?;
    let resolved = wf
        .resolve_against(&statuses)
        .map_err(|e| annotate(&path, e))?;

    validate_agent_references(project, &resolved).map_err(|e| annotate(&path, e))?;
    Ok(resolved)
}

/// Alias the legacy workflow name `default` to `task` at load time.
///
/// Projects created before the `task`/`subtask` scaffold refer to the
/// workflow name `default` — both in a project's `default_workflow:` and
/// in existing task frontmatter (`workflow: default`). Once such a
/// project has been migrated to the new scaffold (ships `task.yaml`, no
/// longer carries its own `workflows/default.yaml`), those references
/// resolve here to `task`, so the board keeps loading and dispatching
/// against the current default track.
///
/// The alias fires **only when `default.yaml` is genuinely absent** and a
/// `task.yaml` is present: a project that still has a real `default.yaml`
/// (a user's own workflow, or a not-yet-migrated legacy default) loads it
/// verbatim, so the alias never shadows a workflow the user put on disk.
/// When neither file exists the name is returned unchanged, preserving the
/// existing built-in-default fallback the callers apply on the resulting
/// `NotFound`.
fn resolve_workflow_alias(project: &str, name: &str) -> Result<String> {
    if name != shelbi_core::DEFAULT_WORKFLOW_NAME {
        return Ok(name.to_string());
    }
    if workflow_path(project, shelbi_core::DEFAULT_WORKFLOW_NAME)?.exists() {
        return Ok(name.to_string());
    }
    if workflow_path(project, shelbi_core::TASK_WORKFLOW_NAME)?.exists() {
        return Ok(shelbi_core::TASK_WORKFLOW_NAME.to_string());
    }
    Ok(name.to_string())
}

/// Resolve the workflow name for `task` with project context:
/// explicit task `workflow:` wins, then project `default_workflow:`,
/// then the built-in `default` fallback.
pub fn resolve_task_workflow_name<'a>(project: &'a Project, task: &'a Task) -> &'a str {
    task.workflow
        .as_deref()
        .unwrap_or_else(|| project.default_workflow_name())
}

/// Load the workflow that applies to `task` under `project_config`.
/// This validates a configured project default through the same strict
/// named-workflow path used by explicit task workflows.
pub fn load_task_workflow(
    project: &str,
    project_config: &Project,
    task: &Task,
) -> Result<Workflow> {
    load_workflow(project, resolve_task_workflow_name(project_config, task))
}

/// Discover every `*.yaml` file in the project's `workflows/` directory
/// and load each into a [`Workflow`]. Results are sorted by workflow
/// `name` for deterministic order.
///
/// **Default-when-absent fallback.** Returns a single-element vector
/// containing [`default_workflow()`] when:
///
/// 1. The `workflows/` directory does not exist (legacy projects), or
/// 2. The directory exists but contains no `*.yaml` files.
///
/// **`statuses.yaml` required.** Once at least one workflow file is on
/// disk, the loader requires `workflows/statuses.yaml`. Missing →
/// hard-fail with a pointer to `shelbi init` / `shelbi reload`. A
/// workflow file that still carries inline `name:` / `category:` under
/// `statuses:` is rejected at parse time with a pointer to move
/// identity into `statuses.yaml`.
///
/// **Per-file isolation.** A malformed individual workflow file is
/// *skipped* with a loud, deduped warning (via the shared parse-warn
/// cache [`crate::should_warn_about_parse`], keyed by path + mtime) —
/// the valid workflows around it keep loading. This mirrors how
/// [`list_tasks`](crate::list_tasks) tolerates a single corrupt TASK
/// file: one fat-fingered `custom.yaml` no longer takes down every
/// caller's board. Callers that need the strict "reject the whole file"
/// contract for a *named* workflow use [`load_workflow`], which still
/// hard-errors.
///
/// Project-wide faults — a missing or unparseable `statuses.yaml` —
/// remain hard errors, because they invalidate *every* workflow, not
/// just one file. When every workflow file on disk is broken the loader
/// degrades to [`default_workflow()`] (after warning on each) so the
/// board still paints rather than going blank.
pub fn list_workflows(project: &str) -> Result<Vec<Workflow>> {
    let dir = workflows_dir(project)?;
    if !dir.exists() {
        return Ok(vec![default_workflow()]);
    }

    let raw_files = read_raw_workflow_files(&dir)?;
    if raw_files.is_empty() {
        return Ok(vec![default_workflow()]);
    }

    let st_path = statuses_read_path(project)?;
    if !st_path.exists() {
        return Err(missing_statuses_error(&st_path));
    }

    let st_text = fs::read_to_string(&st_path)?;
    let statuses = ProjectStatuses::from_yaml_str(&st_text).map_err(|e| annotate(&st_path, e))?;
    emit_status_warnings_once(&st_path, &statuses);

    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut seen_names: std::collections::HashMap<String, PathBuf> =
        std::collections::HashMap::new();
    for raw in raw_files {
        seen.insert(raw.path.clone());
        let resolved = match resolve_raw_workflow(project, &raw, &statuses) {
            Ok(resolved) => {
                crate::forget_parse_warn(&raw.path);
                resolved
            }
            Err(e) => {
                let msg = format!(
                    "shelbi: skipping malformed workflow file {}: {e}",
                    raw.path.display()
                );
                // Route through `tracing::warn!` (not `eprintln!`) so the
                // sidebar / tasks / review TUIs don't get the message
                // painted onto their alt-screen — same rationale as the
                // deprecation warnings above.
                if crate::should_warn_about_parse(&raw.path, raw.mtime, &msg) {
                    tracing::warn!("{msg}");
                }
                continue;
            }
        };
        // `load_workflow` resolves by *filename*, so a duplicated `name:`
        // or a name/stem mismatch means a picker-visible name that fails
        // to load with a raw NotFound. A duplicate is a cross-file
        // ambiguity — which file wins? — so it stays a hard error even
        // under per-file isolation; a stem mismatch only warns.
        if let Some(prev) = seen_names.insert(resolved.name.clone(), raw.path.clone()) {
            return Err(Error::InvalidWorkflow(format!(
                "workflow name `{}` is declared by both {} and {} — \
                 workflow names must be unique within a project",
                resolved.name,
                prev.display(),
                raw.path.display(),
            )));
        }
        warn_name_stem_mismatch_once(&raw.path, &resolved.name);
        out.push(resolved);
    }
    crate::prune_parse_warn(&dir, &seen);

    if out.is_empty() {
        // Every workflow file on disk failed to load; each surfaced its
        // own warning above. Degrade to the canonical default so callers
        // that assume at least one workflow keep working.
        return Ok(vec![default_workflow()]);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// Parse, resolve, and validate a single buffered workflow file against
/// the project's already-loaded `statuses`. Shared by [`list_workflows`]
/// (which skips on error) — the returned error carries the file path via
/// [`annotate`] / [`mixed_form_error`] so the skip warning names the
/// offending file.
fn resolve_raw_workflow(
    project: &str,
    raw: &RawWorkflowFile,
    statuses: &ProjectStatuses,
) -> Result<Workflow> {
    let inline = Workflow::inline_identity_fields(&raw.text).map_err(|e| annotate(&raw.path, e))?;
    if !inline.is_empty() {
        return Err(mixed_form_error(&raw.path, &inline));
    }
    let (wf, diags) =
        Workflow::from_yaml_str_with_diagnostics(&raw.text).map_err(|e| annotate(&raw.path, e))?;
    emit_deprecation_warnings_once(&raw.path, &diags);
    let resolved = wf
        .resolve_against(statuses)
        .map_err(|e| annotate(&raw.path, e))?;
    validate_agent_references(project, &resolved).map_err(|e| annotate(&raw.path, e))?;
    Ok(resolved)
}

/// One unparsed workflow file on disk — path + raw text + mtime.
/// Buffered so the inline-form probe, parse, and resolve steps all run
/// off the same in-memory copy without hitting disk three times per
/// file. `mtime` feeds the per-file skip-warning dedupe so a broken
/// file re-warns only after the user edits it.
struct RawWorkflowFile {
    path: PathBuf,
    text: String,
    mtime: Option<SystemTime>,
}

fn read_raw_workflow_files(dir: &Path) -> Result<Vec<RawWorkflowFile>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        // `statuses.yaml` is the project-wide status catalogue, not a
        // workflow file — it shares the `.yaml` extension, so exclude it
        // here by name.
        if path.file_name().and_then(|s| s.to_str()) == Some("statuses.yaml") {
            continue;
        }
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        let text = fs::read_to_string(&path)?;
        out.push(RawWorkflowFile { path, text, mtime });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

fn mixed_form_error(path: &Path, inline: &[shelbi_core::InlineIdentityField]) -> Error {
    let mut diff = String::new();
    for entry in inline {
        let mut fields = Vec::new();
        if entry.has_name {
            fields.push("name");
        }
        if entry.has_category {
            fields.push("category");
        }
        diff.push_str(&format!(
            "  status `{}`: drop inline {}\n",
            entry.id,
            fields.join(", "),
        ));
    }
    Error::InvalidWorkflow(format!(
        "{} carries inline status identity — \
         workflows/statuses.yaml is the source of truth for `name:` and `category:`. \
         Remove the following inline fields:\n{}",
        path.display(),
        diff,
    ))
}

fn missing_statuses_error(path: &Path) -> Error {
    Error::InvalidWorkflow(format!(
        "{} is missing — `workflows/statuses.yaml` is the project-wide source of truth \
         for status identity. Run `shelbi init --project <name>` or `shelbi reload` \
         to materialize the shipped default.",
        path.display(),
    ))
}

/// Check each declared `agent:` reference against the on-disk
/// `agents/<name>/` workspaces. Skipped silently when `agents/` doesn't
/// exist — that's the state of legacy projects that haven't been
/// re-initialized since the agent-workspaces feature landed; the next
/// `shelbi reload` materializes the defaults and a follow-up load
/// enforces the check.
///
/// On a hit the error message lists the known agents so the user can
/// pick the right one (or notice they need to add it).
fn validate_agent_references(project: &str, workflow: &Workflow) -> Result<()> {
    let dir = agents_dir(project)?;
    if !dir.is_dir() {
        return Ok(());
    }
    let known = list_known_agents(&dir)?;
    for status in &workflow.statuses {
        let Some(agent) = status.agent.as_deref() else {
            continue;
        };
        if !known.contains(agent) {
            let available = if known.is_empty() {
                "(none)".to_string()
            } else {
                known
                    .iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            return Err(Error::InvalidWorkflow(format!(
                "workflow `{}`: status `{}` references unknown agent `{}` \
                 (available: {})",
                workflow.name, status.id, agent, available,
            )));
        }
    }
    Ok(())
}

/// Enumerate `agents/<name>/` subdirectories — the canonical agent
/// registry for a project. Order is sorted/deterministic so error
/// messages don't surprise the user with churn between runs.
fn list_known_agents(dir: &Path) -> Result<BTreeSet<String>> {
    let mut out = BTreeSet::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if !file_type.is_dir() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            out.insert(name.to_string());
        }
    }
    Ok(out)
}

/// Process-local memo of workflow paths whose deprecation warning has
/// already been emitted. Keyed by absolute path so two workflows with
/// the same `name:` declared in different files don't suppress each
/// other.
static EMITTED_DEPRECATIONS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Surface every diagnostic in `diags` — once per workflow path.
fn emit_deprecation_warnings_once(path: &Path, diags: &[String]) {
    if diags.is_empty() {
        return;
    }
    let mut guard = match EMITTED_DEPRECATIONS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let seen = guard.get_or_insert_with(HashSet::new);
    if !seen.insert(path.to_path_buf()) {
        return;
    }
    drop(guard);
    for d in diags {
        tracing::warn!(workflow = %path.display(), "shelbi: {} — {d}", path.display());
    }
}

/// Process-local memo of `statuses.yaml` paths whose category-coherence
/// warnings have already been emitted — same once-per-process dedupe as
/// [`EMITTED_DEPRECATIONS`], so a polling TUI doesn't spam the log.
static EMITTED_STATUS_WARNINGS: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Surface [`ProjectStatuses::category_warnings`] — once per statuses
/// path per process. Non-fatal coherence guidance (missing `handoff`,
/// duplicated single-instance category); load still succeeds.
fn emit_status_warnings_once(path: &Path, statuses: &ProjectStatuses) {
    let warnings = statuses.category_warnings();
    if warnings.is_empty() {
        return;
    }
    let mut guard = match EMITTED_STATUS_WARNINGS.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let seen = guard.get_or_insert_with(HashSet::new);
    if !seen.insert(path.to_path_buf()) {
        return;
    }
    drop(guard);
    for w in warnings {
        tracing::warn!(statuses = %path.display(), "shelbi: {} — {w}", path.display());
    }
}

/// Process-local memo of workflow paths whose name/file-stem mismatch
/// warning has already been emitted — same once-per-process dedupe as
/// [`EMITTED_DEPRECATIONS`], so a polling TUI doesn't spam the log.
static EMITTED_NAME_MISMATCHES: Mutex<Option<HashSet<PathBuf>>> = Mutex::new(None);

/// Warn (once per path per process) when a workflow file's declared
/// `name:` differs from its file stem. [`load_workflow`] resolves by
/// filename, so the name shown in pickers wouldn't load.
fn warn_name_stem_mismatch_once(path: &Path, declared: &str) {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default();
    if stem == declared {
        return;
    }
    let mut guard = match EMITTED_NAME_MISMATCHES.lock() {
        Ok(g) => g,
        Err(p) => p.into_inner(),
    };
    let seen = guard.get_or_insert_with(HashSet::new);
    if !seen.insert(path.to_path_buf()) {
        return;
    }
    drop(guard);
    tracing::warn!(
        workflow = %path.display(),
        "shelbi: {} declares `name: {declared}` but workflows load by file stem \
         (`{stem}`) — rename the file to `{declared}.yaml` or fix `name:` so the \
         two agree",
        path.display(),
    );
}

/// Wrap a parse/validate error with the offending file's path so a
/// missing `description:` in `workflows/research.yaml` doesn't surface
/// as a bare "yaml: …" with no context.
fn annotate(path: &std::path::Path, err: Error) -> Error {
    Error::InvalidWorkflow(format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_workspaces::{
        materialize_default_agents, DEVELOPER_AGENT, ORCHESTRATOR_AGENT,
    };
    use crate::ensure_dir;
    use crate::project_dir;
    use crate::test_lock::LOCK as TEST_LOCK;
    use shelbi_core::{ProjectStatus, StatusCategory};
    use std::path::Path;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-workflows-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_workflow(dir: &Path, name: &str, yaml: &str) {
        ensure_dir(dir).unwrap();
        std::fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    }

    /// Materialize the shipped default `statuses.yaml` for `project` —
    /// every test that exercises [`list_workflows`] / [`load_workflow`]
    /// past the directory-empty fallback needs it on disk.
    fn write_default_statuses(project: &str) {
        save_project_statuses(project, &default_project_statuses()).unwrap();
    }

    fn reset_deprecation_cache() {
        let mut guard = match EMITTED_DEPRECATIONS.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(set) = guard.as_mut() {
            set.clear();
        }
    }

    /// Reference-form workflow that picks a subset of the default
    /// statuses. The post-Phase-3 on-disk shape — `id` + `owner` +
    /// optional `agent`, with identity coming from `statuses.yaml`.
    const SIMPLE_WORKFLOW: &str = r#"
name: design-review
description: A subset of the default catalogue.
statuses:
  - { id: backlog,     owner: user                          }
  - { id: in-progress, owner: agent, agent: developer       }
  - { id: review,      owner: user                          }
  - { id: done,        owner: user                          }
"#;

    const APP_WORKFLOW: &str = r#"
name: app
description: App development workflow.
statuses:
  - { id: backlog,     owner: user                          }
  - { id: in-progress, owner: agent, agent: developer       }
  - { id: review,      owner: user                          }
  - { id: done,        owner: user                          }
"#;

    fn write_project_yaml(home: &Path, name: &str, default_workflow: Option<&str>) {
        ensure_dir(&home.join("projects")).unwrap();
        let default_workflow = default_workflow
            .map(|w| format!("default_workflow: {w}\n"))
            .unwrap_or_default();
        std::fs::write(
            home.join(format!("projects/{name}.yaml")),
            format!(
                r#"name: {name}
repo: /tmp/{name}
default_branch: main
{default_workflow}orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
machines:
  - name: local
    kind: local
    work_dir: /tmp/{name}
workspaces:
  - {{ name: dev, machine: local, runner: claude }}
"#
            ),
        )
        .unwrap();
    }

    fn task_with_workflow(workflow: Option<&str>) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: "t".into(),
            title: "Task".into(),
            column: shelbi_core::Column::todo(),
            priority: 0,
            assigned_to: None,
            workflow: workflow.map(str::to_string),
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: Default::default(),
        }
    }

    fn write_app_workflow(project: &str) {
        write_default_statuses(project);
        let dir = workflows_dir(project).unwrap();
        write_workflow(&dir, "app", APP_WORKFLOW);
    }

    #[test]
    fn resolve_task_workflow_name_uses_project_default() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_project_yaml(&home, "p", Some("app"));
        write_app_workflow("p");
        let project = crate::load_project("p").unwrap();
        let task = task_with_workflow(None);

        assert_eq!(resolve_task_workflow_name(&project, &task), "app");
        let workflow = load_task_workflow("p", &project, &task).unwrap();
        assert_eq!(workflow.name, "app");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn resolve_task_workflow_name_prefers_explicit_task_workflow() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_project_yaml(&home, "p", Some("app"));
        write_app_workflow("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        let project = crate::load_project("p").unwrap();
        let task = task_with_workflow(Some("design-review"));

        assert_eq!(resolve_task_workflow_name(&project, &task), "design-review");
        let workflow = load_task_workflow("p", &project, &task).unwrap();
        assert_eq!(workflow.name, "design-review");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn resolve_task_workflow_name_keeps_legacy_default_fallback() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_project_yaml(&home, "p", None);
        let project = crate::load_project("p").unwrap();
        let task = task_with_workflow(None);

        assert_eq!(resolve_task_workflow_name(&project, &task), "default");
        assert_eq!(task.workflow_or_default(), "default");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_aliases_default_to_task_when_only_task_present() {
        // A migrated project ships `task.yaml`/`subtask.yaml` and no
        // `default.yaml`. Existing task frontmatter (`workflow: default`)
        // must keep loading — it resolves to the `task` workflow.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        scaffold_project_workflow("p").unwrap();
        // No `default.yaml` on disk.
        assert!(!workflow_path("p", "default").unwrap().exists());

        let wf = load_workflow("p", "default").unwrap();
        assert_eq!(wf.name, shelbi_core::TASK_WORKFLOW_NAME);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_prefers_real_default_yaml_over_alias() {
        // A project that still carries its own `default.yaml` (a user's
        // workflow, or a not-yet-migrated legacy default) loads it
        // verbatim — the alias never shadows a workflow on disk even when
        // `task.yaml` also exists.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        scaffold_project_workflow("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        // A real `default.yaml` with a distinguishable status subset.
        write_workflow(&dir, "default", SIMPLE_WORKFLOW);

        let wf = load_workflow("p", "default").unwrap();
        // Declared `name:` in SIMPLE_WORKFLOW is `design-review`; the point
        // is that the on-disk `default.yaml` was loaded, not `task.yaml`.
        assert_ne!(wf.name, shelbi_core::TASK_WORKFLOW_NAME);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_leaves_default_unchanged_when_no_task_yaml() {
        // Truly legacy project — neither `default.yaml` nor `task.yaml`
        // present. The alias is a no-op and the name is looked up as-is,
        // yielding the usual NotFound the callers fall back on.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_default_statuses("p");
        let err = load_workflow("p", "default").unwrap_err();
        assert!(matches!(err, Error::Io(_)), "got: {err}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_project_rejects_missing_configured_default_workflow() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_project_yaml(&home, "p", Some("app"));

        let err = crate::load_project("p").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("default_workflow"), "msg: {msg}");
        assert!(msg.contains("app"), "msg: {msg}");
        assert!(msg.contains("could not be loaded"), "msg: {msg}");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workflows_dir_lands_under_project_dir() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("myapp").unwrap();
        assert_eq!(dir, home.join("projects/myapp/workflows"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workflow_path_appends_yaml_extension() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = workflow_path("myapp", "research").unwrap();
        assert_eq!(path, home.join("projects/myapp/workflows/research.yaml"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn statuses_path_sits_beside_workflow_yamls() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = statuses_path("myapp").unwrap();
        assert_eq!(path, home.join("projects/myapp/workflows/statuses.yaml"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_returns_canonical_default_when_directory_absent() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let out = list_workflows("legacy-proj").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], default_workflow());
        assert!(!workflows_dir("legacy-proj").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_returns_default_when_directory_empty() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&workflows_dir("p").unwrap()).unwrap();
        let out = list_workflows("p").unwrap();
        assert_eq!(out, vec![default_workflow()]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_ignores_non_yaml_files() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        ensure_dir(&dir).unwrap();
        std::fs::write(dir.join("README.md"), "docs").unwrap();
        std::fs::write(dir.join("notes.yml"), "name: x").unwrap();
        std::fs::write(dir.join(".swp"), "junk").unwrap();
        let out = list_workflows("p").unwrap();
        assert_eq!(out, vec![default_workflow()]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_loads_every_yaml_in_directory_sorted_by_name() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: backlog, owner: user }
  - { id: done,    owner: user }
"#,
        );
        let out = list_workflows("p").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "default");
        assert_eq!(out[1].name, "design-review");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_rejects_duplicate_declared_names() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        // Two files, same declared `name:` — only one can win filename
        // resolution, so the loader must refuse rather than let a
        // picker-visible name fail to load.
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        write_workflow(&dir, "design-review-copy", SIMPLE_WORKFLOW);
        let err = list_workflows("p").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("design-review"), "msg: {msg}");
        assert!(msg.contains("design-review-copy.yaml"), "msg: {msg}");
        assert!(msg.contains("unique"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_warns_once_when_name_differs_from_file_stem() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        // File stem `renamed` vs declared `name: design-review`.
        write_workflow(&dir, "renamed", SIMPLE_WORKFLOW);
        let out = list_workflows("p").unwrap();
        assert_eq!(out.len(), 1);

        let path = workflow_path("p", "renamed").unwrap();
        let guard = EMITTED_NAME_MISMATCHES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        assert!(guard.as_ref().unwrap().contains(&path));
        drop(guard);

        // Second listing doesn't re-insert (dedupe holds).
        let _ = list_workflows("p").unwrap();
        let guard = EMITTED_NAME_MISMATCHES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        assert_eq!(
            guard
                .as_ref()
                .unwrap()
                .iter()
                .filter(|p| **p == path)
                .count(),
            1
        );
        drop(guard);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_does_not_warn_when_name_matches_file_stem() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        let _ = list_workflows("p").unwrap();
        let path = workflow_path("p", "design-review").unwrap();
        let guard = EMITTED_NAME_MISMATCHES
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        assert!(!guard.as_ref().map(|s| s.contains(&path)).unwrap_or(false));
        drop(guard);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_project_statuses_propagates_non_notfound_read_errors() {
        // F13: a directory squatting on statuses.yaml (EISDIR) must be an
        // error, not silently mapped to the shipped defaults.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(statuses_path("p").unwrap()).unwrap();
        assert!(load_project_statuses("p").is_err());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_skips_malformed_file_and_keeps_valid_ones() {
        // Per-file isolation: one broken workflow no longer takes down
        // the whole listing. The valid workflow beside it still loads.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        // `name: broken` is missing the required `statuses:` list.
        std::fs::write(dir.join("broken.yaml"), "name: broken\n").unwrap();
        let out = list_workflows("p").unwrap();
        assert_eq!(out.len(), 1, "broken file should be skipped, valid kept");
        assert_eq!(out[0].name, "design-review");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_degrades_to_default_when_every_file_is_broken() {
        // When no workflow file survives loading the loader still hands
        // callers the canonical default so the board keeps painting.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        std::fs::write(dir.join("broken.yaml"), "name: broken\n").unwrap();
        std::fs::write(dir.join("also-broken.yaml"), ": not valid yaml").unwrap();
        let out = list_workflows("p").unwrap();
        assert_eq!(out, vec![default_workflow()]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_surfaces_parse_errors_with_path_context() {
        // The named single-file loader keeps the strict contract: a
        // malformed file is a hard error naming the offending path.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        std::fs::write(dir.join("broken.yaml"), "name: broken\n").unwrap();
        let err = load_workflow("p", "broken").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("broken.yaml"),
            "error should name the offending file, got: {msg}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_does_not_create_directory_as_side_effect() {
        // The loader is read-only when the project has no `workflows/`
        // directory yet — the default-when-absent fallback never
        // materializes files on disk.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let _ = list_workflows("p").unwrap();
        assert!(!project_dir("p").unwrap().exists());
        assert!(!workflows_dir("p").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_hard_fails_when_statuses_yml_missing() {
        // Acceptance criterion: once at least one workflow file is on
        // disk, the loader requires `statuses.yaml`. The error must
        // point the user at the init/reload escape hatch rather than
        // expecting them to hand-author the file.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "default", SIMPLE_WORKFLOW);
        let err = list_workflows("p").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("statuses.yaml"), "msg: {msg}");
        assert!(
            msg.contains("shelbi init") || msg.contains("shelbi reload"),
            "msg: {msg}"
        );
        // No file was materialized as a side effect.
        assert!(
            !statuses_path("p").unwrap().exists(),
            "loader must not auto-create statuses.yaml"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_reads_and_validates_single_file() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        let wf = load_workflow("p", "design-review").unwrap();
        assert_eq!(wf.name, "design-review");
        assert_eq!(wf.statuses.len(), 4);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_propagates_io_errors_for_missing_files() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let err = load_workflow("p", "ghost").unwrap_err();
        assert!(matches!(err, Error::Io(_)), "got: {err}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_hard_fails_when_statuses_yml_missing() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "default", SIMPLE_WORKFLOW);
        let err = load_workflow("p", "default").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("statuses.yaml"), "msg: {msg}");
        assert!(
            msg.contains("shelbi init") || msg.contains("shelbi reload"),
            "msg: {msg}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    // ---------------------------------------------------------------------
    // Two-field owner/agent validation

    #[test]
    fn load_workflow_accepts_two_field_form_when_agents_exist() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: backlog,     owner: user,  agent: orchestrator }
  - { id: todo,        owner: agent, agent: orchestrator }
  - { id: in-progress, owner: agent, agent: developer    }
  - { id: review,      owner: user,  agent: orchestrator }
  - { id: done,        owner: user  }
  - { id: canceled,    owner: user  }
"#,
        );
        let wf = load_workflow("p", "default").unwrap();
        assert_eq!(wf.statuses.len(), 6);
        assert_eq!(wf.statuses[2].agent.as_deref(), Some("developer"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_rejects_unknown_agent_with_available_list() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        save_project_statuses(
            "p",
            &ProjectStatuses {
                statuses: vec![
                    ProjectStatus {
                        id: "doing".into(),
                        name: "Doing".into(),
                        category: StatusCategory::Active,
                    },
                    // A terminal is required for the set to validate; the
                    // workflow under test only references `doing`.
                    ProjectStatus {
                        id: "done".into(),
                        name: "Done".into(),
                        category: StatusCategory::Done,
                    },
                ],
            },
        )
        .unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "custom",
            r#"
name: custom
statuses:
  - { id: doing, owner: agent, agent: reviewer }
"#,
        );
        let err = load_workflow("p", "custom").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`reviewer`"), "msg: {msg}");
        assert!(msg.contains("available"), "msg: {msg}");
        assert!(msg.contains("`developer`"), "msg: {msg}");
        assert!(msg.contains("`orchestrator`"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_skips_agent_check_when_agents_dir_absent() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_default_statuses("p");
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: todo, owner: agent, agent: orchestrator }
"#,
        );
        let wf = load_workflow("p", "default").unwrap();
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_deprecation_warning_fires_once_per_workflow_path() {
        // The owner/agent migration warning (separate from the
        // inline-form rejection) still fires when a workflow uses the
        // legacy named-owner form — `owner: <name>` migrates to
        // `owner: agent, agent: <name>` and surfaces one bundled
        // diagnostic per workflow path. Skip the agents-dir materialize
        // step so [`validate_agent_references`] stays a no-op; we're
        // exercising the dedupe path, not the agent existence check.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_default_statuses("p");
        reset_deprecation_cache();

        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "legacy",
            r#"
name: legacy
statuses:
  - { id: todo,        owner: alice }
  - { id: in-progress, owner: bob   }
"#,
        );

        let wf = load_workflow("p", "legacy").unwrap();
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("alice"));
        assert_eq!(wf.statuses[1].agent.as_deref(), Some("bob"));

        let path = workflow_path("p", "legacy").unwrap();
        let guard = EMITTED_DEPRECATIONS.lock().unwrap();
        assert!(guard.as_ref().unwrap().contains(&path));
        drop(guard);

        let _ = load_workflow("p", "legacy").unwrap();
        let guard = EMITTED_DEPRECATIONS.lock().unwrap();
        assert_eq!(
            guard
                .as_ref()
                .unwrap()
                .iter()
                .filter(|p| **p == path)
                .count(),
            1
        );
        drop(guard);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_known_agents_picks_up_materialized_defaults() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = agents_dir("p").unwrap();
        let known = list_known_agents(&dir).unwrap();
        assert!(known.contains(ORCHESTRATOR_AGENT));
        assert!(known.contains(DEVELOPER_AGENT));
        std::env::remove_var("SHELBI_HOME");
    }

    // ---------------------------------------------------------------------
    // statuses.yaml validation

    #[test]
    fn statuses_yml_round_trips_through_parser() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let statuses = default_project_statuses();
        save_project_statuses("p", &statuses).unwrap();
        let back = load_project_statuses("p").unwrap();
        assert_eq!(statuses, back);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn loader_rejects_unknown_id_with_available_list() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        save_project_statuses(
            "p",
            &ProjectStatuses {
                statuses: vec![
                    ProjectStatus {
                        id: "todo".into(),
                        name: "Todo".into(),
                        category: StatusCategory::Ready,
                    },
                    // Terminal so the set validates; the workflow below
                    // references `done`, which is deliberately *not* here.
                    ProjectStatus {
                        id: "canceled".into(),
                        name: "Canceled".into(),
                        category: StatusCategory::Archived,
                    },
                ],
            },
        )
        .unwrap();
        write_workflow(
            &dir,
            "bad",
            r#"
name: bad
statuses:
  - { id: done, owner: user }
"#,
        );
        // `list_workflows` now skips a bad file; the strict rejection is
        // exercised through the named single-file loader.
        let err = load_workflow("p", "bad").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("status id `done`"), "msg: {msg}");
        assert!(msg.contains("`todo`"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn loader_rejects_mixed_form_when_statuses_yml_exists() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        write_default_statuses("p");
        write_workflow(
            &dir,
            "mixed",
            r#"
name: mixed
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
  - { id: review,  owner: user }
"#,
        );
        // `list_workflows` skips the mixed-form file; the strict
        // rejection is exercised through the named single-file loader.
        let err = load_workflow("p", "mixed").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mixed.yaml"), "msg: {msg}");
        assert!(msg.contains("inline status identity"), "msg: {msg}");
        assert!(msg.contains("status `backlog`"), "msg: {msg}");
        assert!(msg.contains("name"), "msg: {msg}");
        assert!(msg.contains("category"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn loader_rejects_inline_form_even_when_statuses_yml_missing() {
        // Acceptance criterion: an inline-form workflow file is
        // rejected at parse time regardless of whether statuses.yaml
        // exists. The error directs the user to move identity to
        // workflows/statuses.yaml — no auto-migration runs.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "mixed",
            r#"
name: mixed
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
"#,
        );
        let err = load_workflow("p", "mixed").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("inline status identity"), "msg: {msg}");
        assert!(msg.contains("workflows/statuses.yaml"), "msg: {msg}");
        // No statuses.yaml was generated from the inline content.
        assert!(
            !statuses_path("p").unwrap().exists(),
            "loader must not synthesize statuses.yaml from inline content"
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
