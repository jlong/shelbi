//! `shelbi workflow <subcommand>` — manage the per-project workflow YAML
//! files that supersede the hardcoded five-column board.
//!
//! Workflows live at `~/.shelbi/projects/<project>/workflows/<name>.yaml`.
//! When no files exist the loader (`shelbi_state::list_workflows`) returns
//! a built-in default; these commands surface that fallback to the user
//! without writing anything until they explicitly run `new` or `edit`.

use std::fs;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use clap::Subcommand;
use shelbi_core::{default_workflow, Workflow};

use super::require_project;

#[derive(Debug, Subcommand)]
pub enum WorkflowCmd {
    /// List every workflow declared under
    /// `~/.shelbi/projects/<project>/workflows/`. Shows the built-in
    /// default with a `·` marker when no files have been written yet.
    List,
    /// Print a workflow YAML. The built-in `default` workflow is shown
    /// even when no file has been written; any other missing name errors.
    Show { name: String },
    /// Create a new workflow YAML pre-populated with the default
    /// statuses. Errors if a workflow with that name already exists.
    New {
        name: String,
        /// Open the new file in $EDITOR after creating it.
        #[arg(long)]
        edit: bool,
    },
    /// Open a workflow YAML in $EDITOR. When the file doesn't exist yet
    /// and the name is `default`, the built-in default is materialized
    /// first so the user has something concrete to tweak.
    Edit { name: String },
}

pub fn run(project_opt: Option<String>, cmd: WorkflowCmd) -> Result<()> {
    let project = require_project(project_opt)?;
    match cmd {
        WorkflowCmd::List => list(&project),
        WorkflowCmd::Show { name } => show(&project, &name),
        WorkflowCmd::New { name, edit } => new(&project, &name, edit),
        WorkflowCmd::Edit { name } => edit_cmd(&project, &name),
    }
}

fn list(project: &str) -> Result<()> {
    let workflows = shelbi_state::list_workflows(project).map_err(|e| anyhow!(e))?;
    let dir = shelbi_state::workflows_dir(project).map_err(|e| anyhow!(e))?;
    for wf in &workflows {
        let on_disk = dir.join(format!("{}.yaml", wf.name)).exists();
        let marker = if on_disk { " " } else { "·" };
        let summary = format!("{} statuses", wf.statuses.len());
        let desc = wf.description.as_deref().unwrap_or("");
        println!("{marker} {:<20} {:<14} {desc}", wf.name, summary);
    }
    Ok(())
}

fn show(project: &str, name: &str) -> Result<()> {
    let path = shelbi_state::workflow_path(project, name).map_err(|e| anyhow!(e))?;
    if path.exists() {
        let text = fs::read_to_string(&path)
            .map_err(|e| anyhow!("reading {}: {e}", path.display()))?;
        print!("{text}");
        if !text.ends_with('\n') {
            println!();
        }
        return Ok(());
    }
    // The default workflow is the one name we can serialize from code
    // even when no file exists on disk — surface it so users can see
    // the shape before deciding to customize.
    if name == "default" {
        let yaml = serde_yaml::to_string(&default_workflow())
            .map_err(|e| anyhow!("serializing built-in default: {e}"))?;
        println!(
            "# built-in default — no file on disk yet. \
             Run `shelbi workflow new default` (or `edit default`) to customize."
        );
        print!("{yaml}");
        return Ok(());
    }
    bail!("workflow `{name}` not found at {}", path.display())
}

fn new(project: &str, name: &str, edit_after: bool) -> Result<()> {
    validate_workflow_name(name)?;
    let path = shelbi_state::workflow_path(project, name).map_err(|e| anyhow!(e))?;
    if path.exists() {
        bail!("workflow `{name}` already exists at {}", path.display());
    }
    let dir = shelbi_state::workflows_dir(project).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&dir).map_err(|e| anyhow!(e))?;
    write_starter(&path, name)?;
    println!("✓ created {}", path.display());
    if edit_after {
        open_editor(&path)?;
    }
    Ok(())
}

fn edit_cmd(project: &str, name: &str) -> Result<()> {
    let path = shelbi_state::workflow_path(project, name).map_err(|e| anyhow!(e))?;
    if !path.exists() {
        if name != "default" {
            bail!(
                "workflow `{name}` not found at {} — run `shelbi workflow new {name}` first",
                path.display()
            );
        }
        let dir = shelbi_state::workflows_dir(project).map_err(|e| anyhow!(e))?;
        shelbi_state::ensure_dir(&dir).map_err(|e| anyhow!(e))?;
        write_starter(&path, name)?;
        println!("✓ materialized built-in default at {}", path.display());
    }
    open_editor(&path)
}

/// Write a starter workflow YAML for `name`. The shape is the canonical
/// five-status default; only the workflow id changes so the file's
/// `name:` matches its basename (the convention the loader documents).
/// For non-default names we drop the description — the default's copy
/// ("standard one-track flow…") would misrepresent a freshly-named
/// workflow whose author hasn't written a real description yet.
fn write_starter(path: &Path, name: &str) -> Result<()> {
    let mut wf: Workflow = default_workflow();
    wf.name = name.to_string();
    if name != "default" {
        wf.description = None;
    }
    let yaml = serde_yaml::to_string(&wf)
        .map_err(|e| anyhow!("serializing starter workflow: {e}"))?;
    fs::write(path, yaml).map_err(|e| anyhow!("writing {}: {e}", path.display()))?;
    Ok(())
}

fn open_editor(path: &Path) -> Result<()> {
    let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".to_string());
    let status = std::process::Command::new(&editor).arg(path).status()?;
    if !status.success() {
        bail!("{editor} exited with {status}");
    }
    Ok(())
}

/// Reject anything that wouldn't survive a round-trip through the
/// filesystem cleanly: empties, path separators, leading dots. We're
/// stricter than POSIX because the workflow name doubles as a YAML
/// identifier referenced from task frontmatter.
fn validate_workflow_name(name: &str) -> Result<()> {
    if name.is_empty() {
        bail!("workflow name must not be empty");
    }
    if name.starts_with('.') {
        bail!("workflow name `{name}` must not start with `.`");
    }
    for c in name.chars() {
        if !(c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            bail!(
                "workflow name `{name}` contains invalid character `{c}` — \
                 use a-z, 0-9, `-`, `_`"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK as TEST_LOCK;
    use std::path::PathBuf;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-cli-workflow-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    const DESIGN_YAML: &str = r#"
name: design
description: Smoke-test fixture.
statuses:
  - { name: Backlog, category: backlog, owner: user }
  - { name: Done,    category: done,    owner: user }
"#;

    #[test]
    fn validate_workflow_name_accepts_kebab_and_snake() {
        validate_workflow_name("default").unwrap();
        validate_workflow_name("design-review").unwrap();
        validate_workflow_name("research_v2").unwrap();
        validate_workflow_name("a1").unwrap();
    }

    #[test]
    fn validate_workflow_name_rejects_path_traversal_and_separators() {
        assert!(validate_workflow_name("").is_err());
        assert!(validate_workflow_name(".hidden").is_err());
        assert!(validate_workflow_name("..").is_err());
        assert!(validate_workflow_name("a/b").is_err());
        assert!(validate_workflow_name("foo bar").is_err());
        assert!(validate_workflow_name("foo.yaml").is_err());
    }

    #[test]
    fn new_creates_file_with_starter_yaml_that_round_trips() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        new("p", "research", false).unwrap();
        let path = shelbi_state::workflow_path("p", "research").unwrap();
        assert!(path.exists());

        // The file must parse + validate — otherwise a downstream
        // `list_workflows` would error rather than fall back.
        let text = std::fs::read_to_string(&path).unwrap();
        let wf = Workflow::from_yaml_str(&text).unwrap();
        assert_eq!(wf.name, "research");
        assert_eq!(wf.statuses.len(), default_workflow().statuses.len());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn new_refuses_to_clobber_existing_file() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        new("p", "research", false).unwrap();
        let err = new("p", "research", false).unwrap_err();
        assert!(
            err.to_string().contains("already exists"),
            "got: {err}"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn new_rejects_invalid_name_before_touching_disk() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let err = new("p", "escape/me", false).unwrap_err();
        assert!(err.to_string().contains("invalid character"));
        // Validation runs first — no workflows/ directory should have been
        // created as a side effect.
        assert!(!shelbi_state::workflows_dir("p").unwrap().exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn edit_cmd_errors_when_non_default_workflow_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let err = edit_cmd("p", "ghost").unwrap_err();
        assert!(err.to_string().contains("not found"));
        assert!(err.to_string().contains("shelbi workflow new"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn edit_cmd_materializes_default_when_missing_but_does_not_spawn_editor_in_test() {
        // We can't realistically launch $EDITOR from a unit test. Drive
        // the materialize-then-open path by pointing EDITOR at /usr/bin/true
        // so the spawn step is a successful no-op.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::set_var("EDITOR", "/usr/bin/true");

        edit_cmd("p", "default").unwrap();

        let path = shelbi_state::workflow_path("p", "default").unwrap();
        assert!(path.exists(), "default workflow should be on disk after edit");
        let wf = Workflow::from_yaml_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(wf, default_workflow());

        std::env::remove_var("EDITOR");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_succeeds_when_directory_is_absent() {
        // Smoke test: the fallback path doesn't crash and we don't blow up
        // on the on-disk marker probe (the file simply doesn't exist).
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        list("p").unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_sees_files_written_via_new() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        new("p", "design-review", false).unwrap();
        // The loader returns the on-disk workflow plus skips the default
        // fallback once at least one file exists.
        let workflows = shelbi_state::list_workflows("p").unwrap();
        assert_eq!(workflows.len(), 1);
        assert_eq!(workflows[0].name, "design-review");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn show_prints_file_when_present() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = shelbi_state::workflows_dir("p").unwrap();
        shelbi_state::ensure_dir(&dir).unwrap();
        std::fs::write(dir.join("design.yaml"), DESIGN_YAML).unwrap();
        show("p", "design").unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn show_falls_back_to_built_in_default_when_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // The default name has a built-in fallback even before any file
        // exists on disk; the call must succeed without writing anything.
        show("p", "default").unwrap();
        assert!(!shelbi_state::workflow_path("p", "default").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn show_errors_on_unknown_workflow() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let err = show("p", "ghost").unwrap_err();
        assert!(err.to_string().contains("not found"));
        std::env::remove_var("SHELBI_HOME");
    }
}
