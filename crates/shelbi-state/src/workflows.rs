//! Workflow loader — discover and parse workflow YAML files from a
//! project's `workflows/` directory, with a built-in fallback so legacy
//! projects (no workflows configured) keep working.
//!
//! Workflows live at `~/.shelbi/projects/<project>/workflows/<name>.yaml`.
//! [`list_workflows`] is the entry point downstream code reaches for:
//! when the directory is missing or holds no `.yaml` files, it returns
//! [`shelbi_core::default_workflow()`] instead of an empty list, so every
//! caller can assume at least one workflow is available. The migration
//! that actually materializes `workflows/default.yaml` on disk is a
//! separate step (Plans/workflows.md §11) — this module never writes.
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
//! ## Legacy migration warnings
//!
//! Workflow YAMLs authored before the two-field owner/agent split
//! ([`shelbi_core::Workflow::from_yaml_str_with_diagnostics`] surfaces
//! these) trigger a one-time-per-workflow deprecation warning. The
//! dedupe is process-local, keyed by workflow path — repeated calls to
//! `list_workflows` from a polling TUI emit the warning once.
//!
//! The warning routes through `tracing::warn!` so TUI subcommands (which
//! init tracing with a file writer at `~/.shelbi/logs/tui.log`) don't
//! paint it straight onto the alt-screen pane the sidebar / tasks /
//! review TUIs are drawing on. Plain CLI invocations inherit the
//! default stderr writer, so the warning still surfaces in a real shell.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use shelbi_core::{default_workflow, Error, Result, Workflow};

use crate::{agents_dir, project_dir};

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

/// Load and validate a single workflow by name. Errors if the file is
/// missing, the YAML doesn't pass [`Workflow::validate`], or any
/// `agent:` reference fails the agents-directory existence check. The
/// file's basename is *not* substituted for the workflow's declared
/// `name:` — callers that need that contract should compare after
/// loading.
///
/// Legacy single-field owner forms migrate silently in-memory and
/// surface a one-time-per-workflow-path deprecation warning so
/// repeated loads from a polling UI don't flood the console. The
/// warning routes through `tracing::warn!` so it lands in the TUI's
/// log file rather than on the alt-screen pane.
pub fn load_workflow(project: &str, name: &str) -> Result<Workflow> {
    let path = workflow_path(project, name)?;
    let text = fs::read_to_string(&path)?;
    let (wf, diags) =
        Workflow::from_yaml_str_with_diagnostics(&text).map_err(|e| annotate(&path, e))?;
    emit_deprecation_warnings_once(&path, &diags);
    validate_agent_references(project, &wf).map_err(|e| annotate(&path, e))?;
    Ok(wf)
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
/// Bad workflow files surface as errors (with the file path quoted in
/// the message) rather than being silently skipped — a malformed
/// `default.yaml` would otherwise leave callers thinking they had no
/// workflows when in fact the user's customization is broken.
pub fn list_workflows(project: &str) -> Result<Vec<Workflow>> {
    let dir = workflows_dir(project)?;
    if !dir.exists() {
        return Ok(vec![default_workflow()]);
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let text = fs::read_to_string(&path)?;
        let (wf, diags) =
            Workflow::from_yaml_str_with_diagnostics(&text).map_err(|e| annotate(&path, e))?;
        emit_deprecation_warnings_once(&path, &diags);
        validate_agent_references(project, &wf).map_err(|e| annotate(&path, e))?;
        out.push(wf);
    }
    if out.is_empty() {
        return Ok(vec![default_workflow()]);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
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
/// Repeated loads (the sidebar's poll loop, a `list_workflows` call
/// followed by `load_workflow`) silently no-op so the user isn't
/// spammed.
///
/// Routes through `tracing::warn!` (not `eprintln!`) so the TUI
/// subcommands' file-backed tracing writer captures these instead of
/// letting them race ratatui's redraw on the shared pane TTY.
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
        // Tests share the process-local dedupe set; clearing it between
        // tests keeps the "exactly once" assertion meaningful across runs.
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
  - { name: Backlog, category: backlog, owner: user                          }
  - { name: Design,  category: active,  owner: agent, agent: developer       }
  - { name: QA,      category: handoff, owner: user                          }
  - { name: Done,    category: done,    owner: user                          }
"#;

    #[test]
    fn workflows_dir_lands_under_project_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("myapp").unwrap();
        assert_eq!(dir, home.join("projects/myapp/workflows"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workflow_path_appends_yaml_extension() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = workflow_path("myapp", "research").unwrap();
        assert_eq!(path, home.join("projects/myapp/workflows/research.yaml"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_returns_canonical_default_when_directory_absent() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No workflows/ directory at all — the legacy state every existing
        // project starts in.
        let out = list_workflows("legacy-proj").unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0], default_workflow());
        // Nothing was written to disk — fallback must be pure.
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
        // A README, a .yml (intentionally — we only honor .yaml), and a
        // hidden file should all be ignored.
        std::fs::write(dir.join("README.md"), "docs").unwrap();
        std::fs::write(dir.join("notes.yml"), "name: x").unwrap();
        std::fs::write(dir.join(".swp"), "junk").unwrap();
        // No real workflows present → fallback fires.
        let out = list_workflows("p").unwrap();
        assert_eq!(out, vec![default_workflow()]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_loads_every_yaml_in_directory_sorted_by_name() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Materialize the default agents so the two-field validation can
        // resolve the `developer` agent referenced by SIMPLE_WORKFLOW.
        materialize_default_agents("p").unwrap();
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        write_workflow(
            &dir,
            "default",
            r#"
name: default
statuses:
  - { name: Backlog, category: backlog, owner: user }
  - { name: Done,    category: done,    owner: user }
"#,
        );
        let out = list_workflows("p").unwrap();
        assert_eq!(out.len(), 2);
        // Sorted by workflow name, not insertion order — readers get a
        // deterministic listing regardless of filesystem read order.
        assert_eq!(out[0].name, "default");
        assert_eq!(out[1].name, "design-review");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_surfaces_parse_errors_with_path_context() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        ensure_dir(&dir).unwrap();
        // Missing required `statuses:` — semantic validate failure.
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
        // The loader is read-only. Migration lives elsewhere; calling
        // list_workflows on a legacy project must not leave behind a
        // freshly-created workflows/ dir.
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

    /// Acceptance test (a): the canonical two-field form parses and
    /// validates as long as every referenced agent is materialized.
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

    /// Acceptance test (c): unknown `agent:` name errors with the
    /// available-list rendered alphabetically.
    #[test]
    fn load_workflow_rejects_unknown_agent_with_available_list() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Materialize defaults (orchestrator, developer) — then reference
        // a third name that isn't in the set.
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

    /// Acceptance test (b): bare `owner: agent` on a category with no
    /// default migration (`done`) hard-errors at load time.
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

    /// Loader is forgiving when the project hasn't materialized its
    /// `agents/` workspaces yet — agent references parse without an
    /// existence check. The next `shelbi reload` plants the defaults.
    #[test]
    fn load_workflow_skips_agent_check_when_agents_dir_absent() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Deliberately do NOT materialize agents — agents/ does not exist.
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

    /// Acceptance test (d): legacy `owner: agent` alone auto-migrates
    /// and the stderr deprecation warning fires exactly once per
    /// workflow path, even across repeated loads.
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
        // Migration filled in category defaults.
        assert_eq!(wf.statuses[0].agent.as_deref(), Some("orchestrator"));
        assert_eq!(wf.statuses[1].agent.as_deref(), Some("developer"));

        // The dedupe set now lists this path; a re-load must not re-emit.
        let path = workflow_path("p", "legacy").unwrap();
        let guard = EMITTED_DEPRECATIONS.lock().unwrap();
        assert!(guard.as_ref().unwrap().contains(&path));
        drop(guard);

        // Second load works and doesn't add a new entry — the set already
        // had it. (We can't capture stderr in a unit test cleanly, but
        // covering the dedupe membership is the real contract.)
        let _ = load_workflow("p", "legacy").unwrap();
        let guard = EMITTED_DEPRECATIONS.lock().unwrap();
        assert_eq!(
            guard.as_ref().unwrap().iter().filter(|p| **p == path).count(),
            1
        );
        drop(guard);

        std::env::remove_var("SHELBI_HOME");
    }

    /// Default agents (`orchestrator`, `developer`) populated by
    /// [`materialize_default_agents`] surface to the loader's known-agent
    /// list — the canonical happy path for default-workflow loads.
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
}
