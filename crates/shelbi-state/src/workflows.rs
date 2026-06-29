//! Workflow loader — discover and parse workflow YAML files from a
//! project's `workflows/` directory, with a built-in fallback so legacy
//! projects (no workflows configured) keep working.
//!
//! Workflows live at `~/.shelbi/projects/<project>/workflows/<name>.yaml`.
//! Per-project **status identity** (id, name, category, declared order)
//! lives in a sibling `workflows/statuses.yml`; each workflow file
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
//! ## statuses.yml migration
//!
//! When `statuses.yml` is missing but the project already has workflow
//! files in the pre-split (legacy) form — `id` + `name` + `category` +
//! `owner` inline under `statuses:` — the loader runs a one-time
//! migration:
//!
//! 1. Walk every workflow file and collect each inline status's
//!    `(id, name, category)`.
//! 2. Hard-fail if two workflows declare the same id with different
//!    names or categories (the conflict the project-wide source of
//!    truth was meant to eliminate).
//! 3. Write `statuses.yml` with the merged set, preserving first-seen
//!    declaration order across files.
//! 4. Rewrite each workflow file's `statuses:` block to the new
//!    reference-only form, dropping `name:` and `category:`.
//! 5. Emit a one-time stderr hint summarizing what was migrated.
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

use shelbi_core::{
    default_project_statuses, default_workflow, Error, ProjectStatus, ProjectStatuses, Result,
    StatusCategory, Workflow,
};

use crate::{agents_dir, atomic_write, project_dir};

/// Directory holding per-project workflow YAML files:
/// `~/.shelbi/projects/<project>/workflows/`.
pub fn workflows_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("workflows"))
}

/// On-disk path of a single workflow by name:
/// `~/.shelbi/projects/<project>/workflows/<name>.yaml`.
pub fn workflow_path(project: &str, name: &str) -> Result<PathBuf> {
    Ok(workflows_dir(project)?.join(format!("{name}.yaml")))
}

/// On-disk path of the project-wide status catalogue:
/// `~/.shelbi/projects/<project>/workflows/statuses.yml`.
pub fn statuses_path(project: &str) -> Result<PathBuf> {
    Ok(workflows_dir(project)?.join("statuses.yml"))
}

/// Load and validate the project's `statuses.yml`. Returns
/// [`default_project_statuses`] when the file is missing — callers
/// that need the strict "must exist" semantic should probe
/// [`statuses_path`] themselves before calling.
pub fn load_project_statuses(project: &str) -> Result<ProjectStatuses> {
    let path = statuses_path(project)?;
    if !path.exists() {
        return Ok(default_project_statuses());
    }
    let text = fs::read_to_string(&path)?;
    ProjectStatuses::from_yaml_str(&text).map_err(|e| annotate(&path, e))
}

/// Atomic write of `statuses.yml` for `project`. Creates the workflows/
/// dir on demand.
pub fn save_project_statuses(project: &str, statuses: &ProjectStatuses) -> Result<()> {
    let path = statuses_path(project)?;
    atomic_write(&path, serde_yaml::to_string(statuses)?.as_bytes())
}

/// Load and validate a single workflow by name. Errors if the file is
/// missing, the YAML doesn't pass [`Workflow::validate`], or any
/// `agent:` reference fails the agents-directory existence check. The
/// file's basename is *not* substituted for the workflow's declared
/// `name:` — callers that need that contract should compare after
/// loading.
///
/// Resolves the workflow against the project's `statuses.yml` when one
/// exists; otherwise validates the (legacy) inline form directly and
/// surfaces the one-time-per-workflow-path deprecation warning. The
/// warning routes through `tracing::warn!` so it lands in the TUI's
/// log file rather than on the alt-screen pane.
pub fn load_workflow(project: &str, name: &str) -> Result<Workflow> {
    let path = workflow_path(project, name)?;
    let text = fs::read_to_string(&path)?;
    let (wf, diags) =
        Workflow::from_yaml_str_with_diagnostics(&text).map_err(|e| annotate(&path, e))?;
    emit_deprecation_warnings_once(&path, &diags);

    let st_path = statuses_path(project)?;
    let resolved = if st_path.exists() {
        let inline = Workflow::inline_identity_fields(&text).map_err(|e| annotate(&path, e))?;
        if !inline.is_empty() {
            return Err(mixed_form_error(&path, &inline));
        }
        let statuses = load_project_statuses(project)?;
        wf.resolve_against(&statuses).map_err(|e| annotate(&path, e))?
    } else {
        wf
    };

    validate_agent_references(project, &resolved).map_err(|e| annotate(&path, e))?;
    Ok(resolved)
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
/// **Migration.** When `statuses.yml` is missing but legacy workflow
/// files are present, runs the one-time migration described in the
/// module docs before resolving — so the very next read sees the
/// post-migration form.
///
/// Bad workflow files surface as errors (with the file path quoted in
/// the message) rather than being silently skipped — a malformed
/// `default.yaml` would otherwise leave callers thinking they had no
/// workflows when in fact the user's customization is broken.
pub fn list_workflows(project: &str) -> Result<Vec<Workflow>> {
    let dir = workflows_dir(project)?;
    if !dir.exists() {
        return Ok(vec![default_workflow()]);
    }

    let initial = read_raw_workflow_files(&dir)?;
    if initial.is_empty() {
        return Ok(vec![default_workflow()]);
    }

    let st_path = dir.join("statuses.yml");
    if !st_path.exists() {
        // First load after this lands and the project has legacy inline
        // workflows. Run the one-shot migration, which rewrites the
        // files on disk — we re-read them below to pick up the new form.
        run_migration(&dir, &initial)?;
    }

    let raw_files = if st_path.exists() {
        read_raw_workflow_files(&dir)?
    } else {
        // No statuses.yml + no legacy form — write the default so the
        // post-condition (statuses.yml present whenever workflows/ has
        // at least one .yaml) holds.
        let s = default_project_statuses();
        atomic_write(&st_path, serde_yaml::to_string(&s)?.as_bytes())?;
        initial
    };

    let st_text = fs::read_to_string(&st_path)?;
    let statuses = ProjectStatuses::from_yaml_str(&st_text).map_err(|e| annotate(&st_path, e))?;

    let mut out = Vec::new();
    for raw in raw_files {
        let inline =
            Workflow::inline_identity_fields(&raw.text).map_err(|e| annotate(&raw.path, e))?;
        if !inline.is_empty() {
            return Err(mixed_form_error(&raw.path, &inline));
        }
        let (wf, diags) = Workflow::from_yaml_str_with_diagnostics(&raw.text)
            .map_err(|e| annotate(&raw.path, e))?;
        emit_deprecation_warnings_once(&raw.path, &diags);
        let resolved = wf
            .resolve_against(&statuses)
            .map_err(|e| annotate(&raw.path, e))?;
        validate_agent_references(project, &resolved).map_err(|e| annotate(&raw.path, e))?;
        out.push(resolved);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

/// One unparsed workflow file on disk — path + raw text. Buffered so the
/// migration can re-read and rewrite without hitting the filesystem
/// twice per file.
struct RawWorkflowFile {
    path: PathBuf,
    text: String,
}

fn read_raw_workflow_files(dir: &Path) -> Result<Vec<RawWorkflowFile>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        // statuses.yml uses the `.yml` extension; defensive filter in
        // case a project author renames it to `.yaml`.
        if path.file_name().and_then(|s| s.to_str()) == Some("statuses.yaml") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        out.push(RawWorkflowFile { path, text });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Walk the raw workflow files, collect every inline status declaration,
/// hard-fail on name/category conflicts across workflows, write
/// `statuses.yml`, and rewrite each workflow file to the reference-only
/// form. Emits a one-time stderr deprecation hint when at least one
/// file was rewritten.
fn run_migration(dir: &Path, files: &[RawWorkflowFile]) -> Result<()> {
    let mut collected: Vec<ProjectStatus> = Vec::new();
    let mut rewrites: Vec<(PathBuf, String)> = Vec::new();
    let mut had_inline = false;

    for file in files {
        let inline = Workflow::inline_identity_fields(&file.text)
            .map_err(|e| annotate(&file.path, e))?;
        let value: serde_yaml::Value =
            serde_yaml::from_str(&file.text).map_err(|e| annotate(&file.path, e.into()))?;
        let statuses = value
            .get(serde_yaml::Value::String("statuses".into()))
            .and_then(|v| v.as_sequence())
            .cloned()
            .unwrap_or_default();

        for entry in &statuses {
            let id = entry
                .get(serde_yaml::Value::String("id".into()))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    entry
                        .get(serde_yaml::Value::String("name".into()))
                        .and_then(|v| v.as_str())
                })
                .unwrap_or("")
                .to_string();
            if id.is_empty() {
                continue;
            }
            let name = entry
                .get(serde_yaml::Value::String("name".into()))
                .and_then(|v| v.as_str())
                .unwrap_or(&id)
                .to_string();
            let category = entry
                .get(serde_yaml::Value::String("category".into()))
                .and_then(|v| v.as_str())
                .map(parse_category)
                .transpose()
                .map_err(|e| annotate(&file.path, e))?
                .unwrap_or(StatusCategory::Backlog);

            if let Some(existing) = collected.iter().find(|s| s.id == id) {
                if existing.name != name || existing.category != category {
                    return Err(conflict_error(&id, existing, &name, category, &file.path));
                }
            } else {
                collected.push(ProjectStatus {
                    id: id.clone(),
                    name,
                    category,
                });
            }
        }

        if !inline.is_empty() {
            had_inline = true;
            rewrites.push((file.path.clone(), rewrite_workflow_yaml(&file.text)?));
        }
    }

    if !had_inline {
        // No legacy files — fall back to writing the default catalogue
        // so the loader's post-condition (statuses.yml present whenever
        // workflows/ has at least one .yaml) holds.
        let statuses = default_project_statuses();
        atomic_write(
            &dir.join("statuses.yml"),
            serde_yaml::to_string(&statuses)?.as_bytes(),
        )?;
        return Ok(());
    }

    let statuses = ProjectStatuses { statuses: collected };
    statuses.validate()?;
    atomic_write(
        &dir.join("statuses.yml"),
        serde_yaml::to_string(&statuses)?.as_bytes(),
    )?;

    let count = rewrites.len();
    for (path, text) in rewrites {
        atomic_write(&path, text.as_bytes())?;
    }

    eprintln!(
        "shelbi: migrated {count} workflow file(s) and wrote {} — please commit",
        dir.join("statuses.yml").display()
    );
    Ok(())
}

fn parse_category(s: &str) -> Result<StatusCategory> {
    s.trim()
        .parse::<StatusCategory>()
        .map_err(|_| Error::InvalidWorkflow(format!("unknown status category: {s}")))
}

fn conflict_error(
    id: &str,
    existing: &ProjectStatus,
    new_name: &str,
    new_category: StatusCategory,
    file: &Path,
) -> Error {
    let mut diff = Vec::new();
    if existing.name != new_name {
        diff.push(format!(
            "  name:     `{}` vs `{}`",
            existing.name, new_name
        ));
    }
    if existing.category != new_category {
        diff.push(format!(
            "  category: `{}` vs `{}`",
            existing.category, new_category
        ));
    }
    Error::InvalidWorkflow(format!(
        "migration aborted: status id `{id}` declared with conflicting identity in {file_disp}:\n{diff}\n\
         resolve the diff in the workflow YAMLs before re-running, or pre-create `workflows/statuses.yml`",
        file_disp = file.display(),
        diff = diff.join("\n"),
    ))
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
        "{} carries inline status identity after migration — \
         workflows/statuses.yml is now the source of truth. \
         Remove the following inline fields:\n{}",
        path.display(),
        diff,
    ))
}

/// Rewrite a workflow YAML's `statuses:` block to the post-migration
/// reference-only form: drop `name:`, `category:`, and `description:`
/// (the discarded legacy field) from each entry, keep `id`, `owner`,
/// `agent`. Everything outside the statuses block is preserved
/// verbatim so user comments at the top of the file aren't disturbed.
fn rewrite_workflow_yaml(text: &str) -> Result<String> {
    let value: serde_yaml::Value = serde_yaml::from_str(text)?;
    let mapping = match value {
        serde_yaml::Value::Mapping(m) => m,
        _ => return Err(Error::InvalidWorkflow("workflow root must be a mapping".into())),
    };

    let mut out = serde_yaml::Mapping::new();
    for (k, v) in mapping {
        if matches!(&k, serde_yaml::Value::String(s) if s == "statuses") {
            if let serde_yaml::Value::Sequence(seq) = v {
                let rewritten = seq
                    .into_iter()
                    .map(|entry| match entry {
                        serde_yaml::Value::Mapping(mut m) => {
                            m.remove(serde_yaml::Value::String("name".into()));
                            m.remove(serde_yaml::Value::String("category".into()));
                            m.remove(serde_yaml::Value::String("description".into()));
                            serde_yaml::Value::Mapping(m)
                        }
                        other => other,
                    })
                    .collect::<Vec<_>>();
                out.insert(k, serde_yaml::Value::Sequence(rewritten));
            } else {
                out.insert(k, v);
            }
        } else {
            out.insert(k, v);
        }
    }

    let yaml = serde_yaml::to_string(&serde_yaml::Value::Mapping(out))?;
    Ok(yaml)
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
    use crate::test_lock::LOCK as TEST_LOCK;
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

    fn reset_deprecation_cache() {
        let mut guard = match EMITTED_DEPRECATIONS.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        if let Some(set) = guard.as_mut() {
            set.clear();
        }
    }

    const SIMPLE_WORKFLOW: &str = r#"
name: design-review
description: Design pipeline with a user-owned QA step.
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user                          }
  - { id: design,  name: Design,  category: active,  owner: agent, agent: developer       }
  - { id: qa,      name: QA,      category: handoff, owner: user                          }
  - { id: done,    name: Done,    category: done,    owner: user                          }
"#;

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
        assert_eq!(path, home.join("projects/myapp/workflows/statuses.yml"));
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
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
  - { id: done,    name: Done,    category: done,    owner: user }
"#,
        );
        let out = list_workflows("p").unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].name, "default");
        assert_eq!(out[1].name, "design-review");
        assert!(statuses_path("p").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_surfaces_parse_errors_with_path_context() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        ensure_dir(&dir).unwrap();
        std::fs::write(dir.join("broken.yaml"), "name: broken\n").unwrap();
        let err = list_workflows("p").unwrap_err();
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
        // directory yet — migration only runs once at least one workflow
        // file is on disk.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let _ = list_workflows("p").unwrap();
        assert!(!project_dir("p").unwrap().exists());
        assert!(!workflows_dir("p").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_reads_and_validates_single_file() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
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

    // ---------------------------------------------------------------------
    // Two-field owner/agent validation

    #[test]
    fn load_workflow_accepts_two_field_form_when_agents_exist() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: backlog,     name: Backlog,    category: backlog,  owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,       category: ready,    owner: agent, agent: orchestrator }
  - { id: in-progress, name: InProgress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,     category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,       category: done,     owner: user  }
  - { id: canceled,    name: Canceled,   category: archived, owner: user  }
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
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "custom",
            r#"
name: custom
statuses:
  - { id: doing, name: Doing, category: active, owner: agent, agent: reviewer }
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
    fn load_workflow_rejects_bare_owner_agent_in_done_category() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "broken",
            r#"
name: broken
statuses:
  - { id: ship, name: Ship, category: done, owner: agent }
"#,
        );
        let err = load_workflow("p", "broken").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("`ship`"), "msg: {msg}");
        assert!(msg.contains("owner: agent"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_skips_agent_check_when_agents_dir_absent() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: todo, name: Todo, category: ready, owner: agent, agent: orchestrator }
"#,
        );
        let wf = load_workflow("p", "default").unwrap();
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_deprecation_warning_fires_once_per_workflow_path() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        reset_deprecation_cache();

        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "legacy",
            r#"
name: legacy
statuses:
  - { id: todo,  name: Todo,  category: ready,  owner: agent }
  - { id: doing, name: Doing, category: active, owner: agent }
"#,
        );

        let wf = load_workflow("p", "legacy").unwrap();
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        assert_eq!(wf.statuses[1].agent.as_deref(), Some("developer"));

        let path = workflow_path("p", "legacy").unwrap();
        let guard = EMITTED_DEPRECATIONS.lock().unwrap();
        assert!(guard.as_ref().unwrap().contains(&path));
        drop(guard);

        let _ = load_workflow("p", "legacy").unwrap();
        let guard = EMITTED_DEPRECATIONS.lock().unwrap();
        assert_eq!(
            guard.as_ref().unwrap().iter().filter(|p| **p == path).count(),
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
    // statuses.yml + migration

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
                statuses: vec![ProjectStatus {
                    id: "todo".into(),
                    name: "Todo".into(),
                    category: StatusCategory::Ready,
                }],
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
        let err = list_workflows("p").unwrap_err();
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
        save_project_statuses("p", &default_project_statuses()).unwrap();
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
        let err = list_workflows("p").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("mixed.yaml"), "msg: {msg}");
        assert!(msg.contains("inline status identity"), "msg: {msg}");
        assert!(msg.contains("status `backlog`"), "msg: {msg}");
        assert!(msg.contains("name"), "msg: {msg}");
        assert!(msg.contains("category"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migration_hard_fails_on_conflicting_names_or_categories() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "a",
            r#"
name: a
statuses:
  - { id: review, name: Review, category: handoff, owner: user }
"#,
        );
        write_workflow(
            &dir,
            "b",
            r#"
name: b
statuses:
  - { id: review, name: QA, category: handoff, owner: user }
"#,
        );
        let err = list_workflows("p").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("status id `review`"),
            "should name the conflicting id: {msg}"
        );
        assert!(msg.contains("Review"), "should echo the existing name: {msg}");
        assert!(msg.contains("QA"), "should echo the new name: {msg}");
        assert!(!statuses_path("p").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migration_hard_fails_on_conflicting_categories() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        write_workflow(
            &dir,
            "a",
            r#"
name: a
statuses:
  - { id: review, name: Review, category: handoff, owner: user }
"#,
        );
        write_workflow(
            &dir,
            "b",
            r#"
name: b
statuses:
  - { id: review, name: Review, category: active, owner: user }
"#,
        );
        let err = list_workflows("p").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("category"), "msg: {msg}");
        assert!(msg.contains("handoff"), "msg: {msg}");
        assert!(msg.contains("active"), "msg: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migration_rewrites_every_workflow_and_writes_statuses_yml() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { id: backlog, name: Backlog, category: backlog, owner: user }
  - { id: done,    name: Done,    category: done,    owner: user }
"#,
        );

        let workflows = list_workflows("p").unwrap();
        assert_eq!(workflows.len(), 2);

        let statuses = load_project_statuses("p").unwrap();
        let ids: Vec<&str> = statuses.statuses.iter().map(|s| s.id.as_str()).collect();
        // First-seen ordering across files sorted by path. `default.yaml`
        // alphabetically precedes `design-review.yaml`, so its ids land
        // first; ids unique to `design-review` follow in declaration
        // order.
        assert_eq!(ids, vec!["backlog", "done", "design", "qa"]);

        let design_text =
            std::fs::read_to_string(dir.join("design-review.yaml")).unwrap();
        assert!(!design_text.contains("name: Backlog"), "{design_text}");
        assert!(!design_text.contains("category:"), "{design_text}");
        let default_text = std::fs::read_to_string(dir.join("default.yaml")).unwrap();
        assert!(!default_text.contains("name: Backlog"), "{default_text}");
        assert!(!default_text.contains("category:"), "{default_text}");

        let workflows2 = list_workflows("p").unwrap();
        assert_eq!(workflows, workflows2);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migration_emits_one_time_hint_only_on_first_load() {
        // Load-twice probe: statuses.yml contents don't change on the
        // second load — proxy for "no side effects after first migration".
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "default", SIMPLE_WORKFLOW);

        let first = list_workflows("p").unwrap();
        let after_first =
            std::fs::read_to_string(dir.join("statuses.yml")).unwrap();
        let second = list_workflows("p").unwrap();
        let after_second =
            std::fs::read_to_string(dir.join("statuses.yml")).unwrap();
        assert_eq!(first, second);
        assert_eq!(after_first, after_second);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn shipped_templates_parse_cleanly_after_migration() {
        // Stand-in for the four real shipped workflow files (app,
        // app-feature, default, site). All four declare the same six
        // status ids in the same legacy inline form, so a representative
        // pair is enough to exercise the migration path end-to-end.
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();

        const SHIPPED_DEFAULT: &str = r#"
name: default
description: The standard one-track flow shipped with every project.
statuses:
  - { id: backlog,     name: Backlog,     category: backlog,  owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,        category: ready,    owner: agent, agent: orchestrator }
  - { id: in-progress, name: In Progress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,      category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,        category: done,     owner: user  }
  - { id: canceled,    name: Canceled,    category: archived, owner: user  }
"#;
        const SHIPPED_APP: &str = r#"
name: app
statuses:
  - { id: backlog,     name: Backlog,     category: backlog,  owner: user,  agent: orchestrator }
  - { id: todo,        name: Todo,        category: ready,    owner: agent, agent: orchestrator }
  - { id: in-progress, name: In Progress, category: active,   owner: agent, agent: developer    }
  - { id: review,      name: Review,      category: handoff,  owner: user,  agent: orchestrator }
  - { id: done,        name: Done,        category: done,     owner: user  }
  - { id: canceled,    name: Canceled,    category: archived, owner: user  }
initial_status: backlog
transitions:
  - { from: in-progress, to: review,   actions: [push_branch, open_pr]      }
  - { from: review,      to: done,     actions: [merge, delete_branch]      }
  - { from: in-progress, to: canceled, actions: [close_pr, delete_branch]   }
"#;
        write_workflow(&dir, "default", SHIPPED_DEFAULT);
        write_workflow(&dir, "app", SHIPPED_APP);

        let workflows = list_workflows("p").unwrap();
        assert_eq!(workflows.len(), 2);
        for wf in &workflows {
            assert_eq!(wf.statuses.len(), 6);
            assert!(
                wf.statuses.iter().all(|s| !s.name.is_empty()),
                "all statuses must have a resolved name"
            );
        }
        let statuses = load_project_statuses("p").unwrap();
        assert_eq!(statuses.statuses.len(), 6);
        assert_eq!(
            statuses
                .statuses
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            vec!["backlog", "todo", "in-progress", "review", "done", "canceled"],
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
