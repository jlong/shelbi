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

use std::fs;
use std::path::PathBuf;

use shelbi_core::{default_workflow, Error, Result, Workflow};

use crate::project_dir;

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
/// missing or the YAML doesn't pass [`Workflow::validate`]. The file's
/// basename is *not* substituted for the workflow's declared `name:` —
/// callers that need that contract should compare after loading.
pub fn load_workflow(project: &str, name: &str) -> Result<Workflow> {
    let path = workflow_path(project, name)?;
    let text = fs::read_to_string(&path)?;
    Workflow::from_yaml_str(&text).map_err(|e| annotate(&path, e))
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
        let wf = Workflow::from_yaml_str(&text).map_err(|e| annotate(&path, e))?;
        out.push(wf);
    }
    if out.is_empty() {
        return Ok(vec![default_workflow()]);
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
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

    const SIMPLE_WORKFLOW: &str = r#"
name: design-review
description: Design pipeline with a user-owned QA step.
statuses:
  - { name: Backlog, category: backlog, owner: user  }
  - { name: Design,  category: active,  owner: agent }
  - { name: QA,      category: handoff, owner: user  }
  - { name: Done,    category: done,    owner: user  }
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
        let _g = TEST_LOCK.lock().unwrap();
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
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        ensure_dir(&workflows_dir("p").unwrap()).unwrap();
        let out = list_workflows("p").unwrap();
        assert_eq!(out, vec![default_workflow()]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_workflows_ignores_non_yaml_files() {
        let _g = TEST_LOCK.lock().unwrap();
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
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
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
        let _g = TEST_LOCK.lock().unwrap();
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
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let _ = list_workflows("p").unwrap();
        assert!(!project_dir("p").unwrap().exists());
        assert!(!workflows_dir("p").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_reads_and_validates_single_file() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = workflows_dir("p").unwrap();
        write_workflow(&dir, "design-review", SIMPLE_WORKFLOW);
        let wf = load_workflow("p", "design-review").unwrap();
        assert_eq!(wf.name, "design-review");
        assert_eq!(wf.statuses.len(), 4);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_workflow_propagates_io_errors_for_missing_files() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let err = load_workflow("p", "ghost").unwrap_err();
        assert!(matches!(err, Error::Io(_)), "got: {err}");
        std::env::remove_var("SHELBI_HOME");
    }
}
