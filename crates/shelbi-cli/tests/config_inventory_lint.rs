use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use tempfile::TempDir;

fn shelbi(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shelbi"))
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .expect("run shelbi")
}

fn write(path: impl AsRef<Path>, body: &str) {
    let path = path.as_ref();
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn project_shared() -> &'static str {
    r#"name: Demo
config_mode: in-repo
default_branch: main
default_workflow: task
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
    flags: []
"#
}

fn project_local(repo: &Path) -> String {
    format!(
        r#"repo: {}
machines:
  - name: hub
    kind: local
    work_dir: /tmp
workspaces:
  - name: alpha
    machine: hub
    runner: claude
    tags: [build]
"#,
        repo.display()
    )
}

fn global_project(repo: &Path) -> String {
    format!(
        "{}{}",
        project_shared().replace("config_mode: in-repo\n", ""),
        project_local(repo)
    )
}

fn scaffold_assets(config_root: &Path) {
    write(
        config_root.join("workflows/statuses.yaml"),
        "statuses:\n  - id: todo\n    name: To Do\n    category: ready\n  - id: review\n    name: Review\n    category: handoff\n  - id: done\n    name: Done\n    category: done\n",
    );
    write(
        config_root.join("workflows/task.yaml"),
        "name: task\nstatuses:\n  - id: todo\n    owner: user\n  - id: done\n    owner: user\ntransitions:\n  - from: todo\n    to: done\n    actions: []\ngit:\n  base_branch: main\n",
    );
    write(
        config_root.join("workspace-settings.json.template"),
        "{\"hooks\": {}}\n",
    );
    write(
        config_root.join("zenmode.md"),
        "Zen keeps this project moving.\n\nFree-form policy.\n",
    );
    for agent in [
        "orchestrator",
        "developer",
        "review",
        "qa",
        "security",
        "adversarial",
    ] {
        write(
            config_root.join(format!("agents/{agent}/instructions.md")),
            &format!("# {agent}\n\nInstructions.\n"),
        );
        write(
            config_root.join(format!("agents/{agent}/settings.json")),
            "{}\n",
        );
    }
    write(
        config_root.join("agents/review/skills/load-run-detection/SKILL.md"),
        "# Load and run\n\nInstructions.\n",
    );
    write(
        config_root.join("agents/_shared/preamble.md"),
        "# Project context\n",
    );
}

fn scaffold_root(in_repo: bool) -> (TempDir, PathBuf) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("home");
    let repo = fixture.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    write(root.join("config.yaml"), "editor: vim\n");
    write(root.join("keys.yaml"), "defaults: {}\n");
    write(root.join("shelbi.yaml"), "projects: {}\n");
    if in_repo {
        write(root.join("projects/demo/local.yaml"), &project_local(&repo));
        write(repo.join(".shelbi/project.yaml"), project_shared());
        scaffold_assets(&repo.join(".shelbi"));
    } else {
        write(root.join("projects/demo.yaml"), &global_project(&repo));
        scaffold_assets(&root.join("projects/demo"));
    }
    (fixture, root)
}

fn inventory(root: &Path) -> Value {
    let output = shelbi(
        root,
        &[
            "config",
            "inventory",
            "--project",
            "demo",
            "--format",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn valid_global_inventory_materializes_and_lints() {
    let (_fixture, root) = scaffold_root(false);
    let inventory = inventory(&root);
    assert_eq!(inventory["schema_version"], 1);
    let entries = inventory["entries"].as_array().unwrap();
    assert!(entries.iter().any(|entry| {
        entry["logical_id"] == "project.demo.registration"
            && entry["scope"] == "project:demo"
            && entry["format"] == "yaml"
            && entry["exists"] == true
            && entry.get("canonical_path").is_some()
            && entry.get("lifecycle_owned").is_some()
    }));

    let staged = inventory["staged_dir"].as_str().unwrap();
    let output = shelbi(
        &root,
        &[
            "config",
            "lint",
            "--project",
            "demo",
            "--staged",
            staged,
            "--format",
            "json",
        ],
    );
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["warnings"], 0);
    assert_eq!(report["errors"], 0);
}

#[test]
fn global_template_override_does_not_move_other_config_surfaces() {
    let (fixture, root) = scaffold_root(false);
    let custom_template = fixture.path().join("custom-workspace-settings.json");
    write(&custom_template, "{}\n");
    let registration = fs::read_to_string(root.join("projects/demo.yaml")).unwrap();
    write(
        root.join("projects/demo.yaml"),
        &format!(
            "{registration}workspace_settings_template: {}\n",
            custom_template.display()
        ),
    );

    let inventory = inventory(&root);
    let entries = inventory["entries"].as_array().unwrap();
    let workflow = entries
        .iter()
        .find(|entry| entry["logical_id"] == "project.demo.workflow.task")
        .unwrap();
    assert_eq!(
        workflow["canonical_path"],
        root.join("projects/demo/workflows/task.yaml")
            .to_string_lossy()
            .as_ref()
    );
    let template = entries
        .iter()
        .find(|entry| entry["logical_id"] == "project.demo.workspace-settings-template")
        .unwrap();
    assert_eq!(
        template["canonical_path"],
        custom_template.to_string_lossy().as_ref()
    );
}

#[test]
fn in_repo_staged_invalid_workflow_reports_contract_and_exits_nonzero() {
    let (_fixture, root) = scaffold_root(true);
    let inventory = inventory(&root);
    let entries = inventory["entries"].as_array().unwrap();
    assert!(entries
        .iter()
        .any(|entry| entry["logical_id"] == "project.demo.registration.local"));
    assert!(entries
        .iter()
        .any(|entry| entry["logical_id"] == "project.demo.registration.shared"));

    let staged = PathBuf::from(inventory["staged_dir"].as_str().unwrap());
    write(
        staged.join("projects/demo/workflows/bad.yaml"),
        "name: bad\nstatuses:\n  - id: missing\n    owner: user\nunknown_knob: true\n",
    );
    let output = shelbi(
        &root,
        &[
            "config",
            "lint",
            "--project",
            "demo",
            "--staged",
            staged.to_str().unwrap(),
            "--format",
            "json",
        ],
    );
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let diagnostics = report["diagnostics"].as_array().unwrap();
    assert!(diagnostics
        .iter()
        .any(|d| d["code"] == "CONFIG_UNKNOWN_FIELD"));
    assert!(diagnostics.iter().any(|d| {
        d["code"] == "WORKFLOW_STATUS_REFERENCE_INVALID"
            && d["logical_id"] == "project.demo.workflow.bad"
            && d["file"].is_string()
            && d["location"]["line"].is_number()
            && d["severity"] == "error"
            && d["message"].is_string()
    }));
}

#[test]
fn warnings_alone_exit_nonzero() {
    let (_fixture, root) = scaffold_root(false);
    write(
        root.join("keys.yaml"),
        "defaults:\n  global:\n    quit: q\n",
    );
    let output = shelbi(
        &root,
        &["config", "lint", "--project", "demo", "--format", "json"],
    );
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["errors"], 0);
    assert!(report["warnings"].as_u64().unwrap() > 0);
    assert!(report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "KEYBINDINGS_RESERVED_CHORD_REBOUND"));
}

#[test]
fn staged_all_uses_manifest_projects_and_validates_settings_templates() {
    let (fixture, root) = scaffold_root(false);
    let inventory = inventory(&root);
    let staged = PathBuf::from(inventory["staged_dir"].as_str().unwrap());
    let unrelated_root = fixture.path().join("unrelated-home");
    let unrelated_repo = fixture.path().join("unrelated-repo");
    fs::create_dir_all(&unrelated_repo).unwrap();
    write(
        unrelated_root.join("projects/other.yaml"),
        &global_project(&unrelated_repo),
    );

    write(
        staged.join("projects/demo/agents/developer/settings.json"),
        r#"{"permissions":{"defaultMode":"{{workspace_permissions_mode}}"}}"#,
    );
    let valid = shelbi(
        &unrelated_root,
        &[
            "config",
            "lint",
            "--all",
            "--staged",
            staged.to_str().unwrap(),
            "--format",
            "json",
        ],
    );
    assert!(
        valid.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&valid.stdout),
        String::from_utf8_lossy(&valid.stderr)
    );

    write(
        staged.join("projects/demo/agents/developer/settings.json"),
        r#"{"custom":"{{unsupported}}"}"#,
    );
    let invalid = shelbi(
        &unrelated_root,
        &[
            "config",
            "lint",
            "--all",
            "--staged",
            staged.to_str().unwrap(),
            "--format",
            "json",
        ],
    );
    assert!(!invalid.status.success());
    let report: Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert!(report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == "TEMPLATE_UNKNOWN_PLACEHOLDER"));
}

#[test]
fn staged_explicit_project_must_exist_in_manifest() {
    let (_fixture, root) = scaffold_root(false);
    let inventory = inventory(&root);
    let staged = inventory["staged_dir"].as_str().unwrap();
    let output = shelbi(
        &root,
        &[
            "config",
            "lint",
            "--project",
            "missing",
            "--staged",
            staged,
            "--format",
            "json",
        ],
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr)
        .contains("project `missing` is not present in staged inventory"));
}
