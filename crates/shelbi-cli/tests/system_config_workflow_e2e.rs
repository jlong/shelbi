//! End-to-end verification of the system-owned configuration-update workflow.
//!
//! The `update-shelbi-configuration` skill is prose an orchestrator agent
//! follows, but every operational step it prescribes bottoms out in the real
//! `shelbi config inventory` / `shelbi config lint` CLI plus deterministic
//! filesystem operations. These tests play the role of that agent: they drive
//! the shipped binary through the exact inventory -> stage -> lint -> preview
//! -> confirm -> race-check -> apply -> live-lint sequence and assert the
//! guardrails the skill promises.
//!
//! Two runners carry the skill into a session (Claude via `--plugin-dir`,
//! Codex via the app-server skill root / developer-instructions fallback), but
//! the workflow they drive is runner-agnostic: it is the same CLI either way.
//! The happy path therefore runs under a project configured for each runner
//! and additionally asserts the per-runner injection transport, so "successful
//! configuration changes through both orchestrator runners" is demonstrated
//! end to end rather than assumed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use shelbi_agent::{OrchestratorPluginInjection, RunnerAdapter};
use tempfile::TempDir;

// ---------------------------------------------------------------------------
// CLI harness

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

// ---------------------------------------------------------------------------
// Fixtures — a representative flat (global) project and split (in-repo) project.

fn project_shared(runner: &str) -> String {
    format!(
        r#"name: Demo
config_mode: in-repo
default_branch: main
default_workflow: task
orchestrator:
  runner: {runner}
agent_runners:
  claude:
    command: claude
    flags: []
  codex:
    command: codex
    flags: []
"#
    )
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

fn global_project(repo: &Path, runner: &str) -> String {
    format!(
        "{}{}",
        project_shared(runner).replace("config_mode: in-repo\n", ""),
        project_local(repo)
    )
}

/// Materialize valid content for every non-registration configuration surface.
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
    write(
        config_root.join("pr-template.md"),
        "# PR description template\n\nSummary, technical details, QA checklist.\n",
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

/// A flat/global project registration under `~/.shelbi/projects/demo.yaml`.
fn scaffold_flat(runner: &str) -> (TempDir, PathBuf) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("home");
    let repo = fixture.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    write(root.join("config.yaml"), "editor: vim\n");
    write(root.join("keys.yaml"), "defaults: {}\n");
    write(root.join("shelbi.yaml"), "projects: {}\n");
    write(
        root.join("projects/demo.yaml"),
        &global_project(&repo, runner),
    );
    scaffold_assets(&root.join("projects/demo"));
    (fixture, root)
}

/// A split/in-repo project — the shared half lives in the repo's
/// `.shelbi/project.yaml`, the machine-local half in `~/.shelbi/projects/demo`.
fn scaffold_split(runner: &str) -> (TempDir, PathBuf) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().join("home");
    let repo = fixture.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    write(root.join("config.yaml"), "editor: vim\n");
    write(root.join("keys.yaml"), "defaults: {}\n");
    write(root.join("shelbi.yaml"), "projects: {}\n");
    write(root.join("projects/demo/local.yaml"), &project_local(&repo));
    write(repo.join(".shelbi/project.yaml"), &project_shared(runner));
    scaffold_assets(&repo.join(".shelbi"));
    (fixture, root)
}

// ---------------------------------------------------------------------------
// Skill-workflow harness — one struct that mirrors the SKILL.md steps.

#[derive(Clone, Debug)]
struct Entry {
    logical_id: String,
    canonical_path: PathBuf,
    candidate_path: PathBuf,
    lifecycle_owned: bool,
    exists: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileChange {
    candidate: PathBuf,
    after: Vec<u8>,
}

/// One combined preview: the exact filesystem diff plus the ordered
/// operational command list. Equality is the re-confirmation contract — any
/// change to a byte or a command yields a different preview.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Preview {
    changes: Vec<FileChange>,
    commands: Vec<String>,
}

struct Session {
    root: PathBuf,
    project: String,
    staged: PathBuf,
    /// Byte-for-byte inventory-time snapshot used for previews and race checks.
    baseline: TempDir,
    entries: Vec<Entry>,
    confirmed: Option<Preview>,
}

impl Drop for Session {
    fn drop(&mut self) {
        // The CLI stages into the system temp dir, outside any TempDir we own.
        let _ = fs::remove_dir_all(&self.staged);
    }
}

impl Session {
    /// Step 1: inventory and preserve the starting point.
    fn open(root: &Path, project: &str) -> Session {
        let output = shelbi(
            root,
            &["config", "inventory", "--project", project, "--format", "json"],
        );
        assert!(
            output.status.success(),
            "inventory failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let inventory: Value = serde_json::from_slice(&output.stdout).unwrap();
        let staged = PathBuf::from(inventory["staged_dir"].as_str().unwrap());
        let entries = inventory["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| Entry {
                logical_id: entry["logical_id"].as_str().unwrap().to_string(),
                canonical_path: PathBuf::from(entry["canonical_path"].as_str().unwrap()),
                candidate_path: PathBuf::from(entry["candidate_path"].as_str().unwrap()),
                lifecycle_owned: entry["lifecycle_owned"].as_bool().unwrap(),
                exists: entry["exists"].as_bool().unwrap(),
            })
            .collect();

        // Byte-for-byte baseline copy of the entire staged tree.
        let baseline = tempfile::tempdir().unwrap();
        copy_tree(&staged, baseline.path());

        Session {
            root: root.to_path_buf(),
            project: project.to_string(),
            staged,
            baseline,
            entries,
            confirmed: None,
        }
    }

    fn entry(&self, logical_id: &str) -> &Entry {
        self.entries
            .iter()
            .find(|e| e.logical_id == logical_id)
            .unwrap_or_else(|| panic!("no inventory entry `{logical_id}`"))
    }

    /// Step 2: edit a staged candidate (never a live file).
    fn edit(&self, logical_id: &str, body: &str) {
        let target = self.staged.join(&self.entry(logical_id).candidate_path);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::write(target, body).unwrap();
    }

    /// Step 2: lint the staged candidates. Returns `(clean, report)`.
    fn lint_staged(&self) -> (bool, Value) {
        let output = shelbi(
            &self.root,
            &[
                "config",
                "lint",
                "--project",
                &self.project,
                "--staged",
                self.staged.to_str().unwrap(),
                "--format",
                "json",
            ],
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.success(), report)
    }

    /// Step 3: build the single combined preview — the exact byte diff plus the
    /// ordered operational command list (lifecycle commands for changed
    /// lifecycle-owned surfaces, then the final live lint).
    fn build_preview(&self) -> Preview {
        let mut changes = Vec::new();
        let mut needs_reload = false;
        for entry in &self.entries {
            let before = fs::read(self.baseline.path().join(&entry.candidate_path)).ok();
            let after = fs::read(self.staged.join(&entry.candidate_path)).ok();
            if before != after {
                if let Some(after) = after {
                    changes.push(FileChange {
                        candidate: entry.candidate_path.clone(),
                        after,
                    });
                }
                if entry.lifecycle_owned {
                    needs_reload = true;
                }
            }
        }
        changes.sort_by(|a, b| a.candidate.cmp(&b.candidate));

        let mut commands = Vec::new();
        if needs_reload {
            commands.push(format!("shelbi reload --project {}", self.project));
        }
        commands.push(format!(
            "shelbi config lint --project {} --format json",
            self.project
        ));
        Preview { changes, commands }
    }

    /// Step 3: explicit confirmation of exactly this preview.
    fn confirm(&mut self, preview: &Preview) {
        self.confirmed = Some(preview.clone());
    }

    /// Step 4: race detection. Returns the logical ids whose live source no
    /// longer matches the inventory-time baseline.
    fn detect_races(&self) -> Vec<String> {
        let mut changed = Vec::new();
        for entry in &self.entries {
            let live = fs::read(&entry.canonical_path).ok();
            if entry.exists {
                let baseline = fs::read(self.baseline.path().join(&entry.candidate_path)).ok();
                if live.as_ref() != baseline.as_ref() {
                    changed.push(entry.logical_id.clone());
                }
            } else if live.is_some() {
                changed.push(entry.logical_id.clone());
            }
        }
        changed
    }

    /// The confirmed preview may only be applied when it was confirmed and no
    /// source raced underneath it.
    fn may_apply(&self) -> bool {
        self.confirmed.is_some() && self.detect_races().is_empty()
    }

    /// Step 5: apply exactly the confirmed change with same-directory atomic
    /// replacement.
    fn apply(&self) {
        assert!(self.may_apply(), "refused to apply: unconfirmed or raced");
        let confirmed = self.confirmed.as_ref().unwrap();
        for change in &confirmed.changes {
            let entry = self
                .entries
                .iter()
                .find(|e| e.candidate_path == change.candidate)
                .expect("confirmed change maps to an inventory entry");
            let dest = &entry.canonical_path;
            fs::create_dir_all(dest.parent().unwrap()).unwrap();
            let tmp = dest.with_extension("shelbi-apply-tmp");
            fs::write(&tmp, &change.after).unwrap();
            fs::rename(&tmp, dest).unwrap();
        }
    }

    /// Step 6: lint live configuration. Returns `(clean, report)`.
    fn lint_live(&self) -> (bool, Value) {
        let output = shelbi(
            &self.root,
            &["config", "lint", "--project", &self.project, "--format", "json"],
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        (output.status.success(), report)
    }

    /// Step 5 recovery rule: a bounded recovery is safe only while it preserves
    /// exactly the confirmed diff and command list. Anything else re-enters the
    /// confirmation gate.
    fn recovery_requires_reconfirmation(&self, recovered: &Preview) -> bool {
        self.confirmed.as_ref() != Some(recovered)
    }

    fn live_bytes(&self, logical_id: &str) -> Option<Vec<u8>> {
        fs::read(&self.entry(logical_id).canonical_path).ok()
    }
}

fn copy_tree(src: &Path, dst: &Path) {
    for item in fs::read_dir(src).unwrap() {
        let item = item.unwrap();
        let from = item.path();
        let to = dst.join(item.file_name());
        if from.is_dir() {
            fs::create_dir_all(&to).unwrap();
            copy_tree(&from, &to);
        } else {
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            fs::copy(&from, &to).unwrap();
        }
    }
}

fn has_code(report: &Value, code: &str) -> bool {
    report["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["code"] == code)
}

// ---------------------------------------------------------------------------
// Criterion: successful changes through both orchestrator runners.

/// The system plugin resolves to a single bundle, and each runner selects its
/// own transport for that one bundle. This is the runner half of "through both
/// orchestrator runners" — the config workflow below is the runner-agnostic
/// half.
fn assert_runner_transport(runner: &str) {
    let adapter = RunnerAdapter::for_command(runner);
    let injection = adapter.orchestrator_plugin_injection(true);
    match runner {
        "claude" => assert_eq!(injection, OrchestratorPluginInjection::ClaudePluginDir),
        "codex" => {
            assert_eq!(injection, OrchestratorPluginInjection::CodexNativeSkillRoot);
            // Older app-servers without the native skill root fall back to
            // developer instructions carrying the same SKILL.md bytes.
            assert_eq!(
                adapter.orchestrator_plugin_injection(false),
                OrchestratorPluginInjection::CodexDeveloperInstructions
            );
        }
        other => panic!("unexpected runner {other}"),
    }

    // One resolved bundle feeds every runner.
    let plugin = shelbi_orchestrator::system_plugin::resolve_system_plugin();
    assert!(
        !plugin.skill.trim().is_empty(),
        "resolved system skill must be non-empty"
    );
}

/// Full happy path on a flat/global project for each runner: a lifecycle-owned
/// edit (global preferences) and a non-lifecycle edit (keybindings) staged,
/// linted clean, previewed, confirmed, applied to live files, and verified by
/// a real live lint.
#[test]
fn successful_change_through_both_runners_flat_project() {
    for runner in ["claude", "codex"] {
        assert_runner_transport(runner);

        let (_fixture, root) = scaffold_flat(runner);
        let mut session = Session::open(&root, "demo");

        session.edit("global.preferences", "editor: nvim\n");
        session.edit(
            "global.keybindings",
            "defaults:\n  sidebar:\n    nav_up: w\n",
        );

        let (clean, report) = session.lint_staged();
        assert!(clean, "staged edits must lint clean: {report}");

        let preview = session.build_preview();
        assert_eq!(preview.changes.len(), 2, "both edits appear in the diff");
        // global.preferences is lifecycle-owned -> reload precedes the final lint.
        assert_eq!(
            preview.commands,
            vec![
                "shelbi reload --project demo".to_string(),
                "shelbi config lint --project demo --format json".to_string(),
            ]
        );

        // Step 4: no live write yet.
        assert_eq!(session.live_bytes("global.preferences").unwrap(), b"editor: vim\n");
        assert_eq!(
            session.live_bytes("global.keybindings").unwrap(),
            b"defaults: {}\n"
        );

        session.confirm(&preview);
        assert!(session.detect_races().is_empty(), "no source raced");
        session.apply();

        // Live files now carry exactly the confirmed change.
        assert_eq!(
            session.live_bytes("global.preferences").unwrap(),
            b"editor: nvim\n"
        );
        assert_eq!(
            session.live_bytes("global.keybindings").unwrap(),
            b"defaults:\n  sidebar:\n    nav_up: w\n"
        );

        let (clean, report) = session.lint_live();
        assert!(clean, "live configuration must lint clean after apply: {report}");
    }
}

/// In-repo (split) project end to end, exercising a non-global surface
/// (`workflows/statuses.yaml`) so both representative layouts are covered.
#[test]
fn successful_change_through_split_in_repo_project() {
    let (_fixture, root) = scaffold_split("codex");
    let mut session = Session::open(&root, "demo");

    session.edit(
        "project.demo.statuses",
        "statuses:\n  - id: todo\n    name: Backlog\n    category: ready\n  - id: review\n    name: Review\n    category: handoff\n  - id: done\n    name: Done\n    category: done\n",
    );
    let (clean, report) = session.lint_staged();
    assert!(clean, "split-project staged edit must lint clean: {report}");

    let preview = session.build_preview();
    assert_eq!(preview.changes.len(), 1);
    // statuses.yaml is not lifecycle-owned: only the final live lint is run.
    assert_eq!(
        preview.commands,
        vec!["shelbi config lint --project demo --format json".to_string()]
    );

    session.confirm(&preview);
    session.apply();
    assert!(
        String::from_utf8_lossy(&session.live_bytes("project.demo.statuses").unwrap())
            .contains("Backlog")
    );
    let (clean, report) = session.lint_live();
    assert!(clean, "split-project live lint must be clean: {report}");
}

// ---------------------------------------------------------------------------
// Criterion: no live configuration changes before explicit confirmation.

#[test]
fn no_live_write_before_confirmation() {
    let (_fixture, root) = scaffold_flat("claude");
    let session = Session::open(&root, "demo");

    // Snapshot every live canonical file before we touch anything.
    let before: Vec<(String, Option<Vec<u8>>)> = session
        .entries
        .iter()
        .map(|e| (e.logical_id.clone(), fs::read(&e.canonical_path).ok()))
        .collect();

    // Stage edits and run the full read-only half of the workflow: edit, lint,
    // preview. None of this is allowed to write a live file.
    session.edit("global.preferences", "editor: hx\n");
    session.edit("project.demo.workflow.task", "# workflow\nname: task\nstatuses:\n  - id: todo\n    owner: user\n  - id: done\n    owner: user\ntransitions:\n  - from: todo\n    to: done\n    actions: []\ngit:\n  base_branch: main\n");
    let (clean, report) = session.lint_staged();
    assert!(clean, "{report}");
    let _preview = session.build_preview();

    let after: Vec<(String, Option<Vec<u8>>)> = session
        .entries
        .iter()
        .map(|e| (e.logical_id.clone(), fs::read(&e.canonical_path).ok()))
        .collect();
    assert_eq!(before, after, "no live file may change before confirmation");
}

// ---------------------------------------------------------------------------
// Criterion: concurrent source changes force a rebuilt preview and renewed
// confirmation.

#[test]
fn concurrent_source_change_forces_new_preview_and_confirmation() {
    let (_fixture, root) = scaffold_flat("claude");
    let mut session = Session::open(&root, "demo");

    session.edit("global.preferences", "editor: nvim\n");
    let preview1 = session.build_preview();
    session.confirm(&preview1);

    // A concurrent editor changes an in-scope live source after confirmation.
    let keys = session.entry("global.keybindings").canonical_path.clone();
    fs::write(&keys, "defaults:\n  sidebar:\n    nav_down: j\n").unwrap();

    // The race is detected and the confirmed apply is refused: an unreviewed
    // concurrent change is never merged into the confirmed proposal.
    let races = session.detect_races();
    assert_eq!(races, vec!["global.keybindings".to_string()]);
    assert!(!session.may_apply(), "a raced source must block apply");

    // The workflow discards the apply, takes a fresh inventory, and re-stages.
    let session2 = Session::open(&root, "demo");
    session2.edit("global.preferences", "editor: nvim\n");
    let _preview2 = session2.build_preview();

    // The rebuild is built against the new source state — its baseline absorbed
    // the concurrent change — and it carries no confirmation forward, so a fresh
    // confirmation is required before anything applies. (The desired end-state
    // bytes can coincide with the discarded preview; the renewed confirmation,
    // not a byte difference, is the guardrail.)
    let rebuilt_keys = fs::read(
        session2
            .baseline
            .path()
            .join(&session2.entry("global.keybindings").candidate_path),
    )
    .unwrap();
    assert_eq!(
        rebuilt_keys, b"defaults:\n  sidebar:\n    nav_down: j\n",
        "the rebuilt preview must reflect the concurrent change"
    );
    assert!(session2.confirmed.is_none(), "the rebuild is unconfirmed");
    assert!(
        !session2.may_apply(),
        "the rebuilt proposal cannot apply until it is confirmed again"
    );
}

// ---------------------------------------------------------------------------
// Criterion: a recovery that changes the approved diff or command list requires
// another confirmation.

#[test]
fn recovery_altering_diff_or_commands_requires_reconfirmation() {
    let (_fixture, root) = scaffold_flat("claude");
    let mut session = Session::open(&root, "demo");

    session.edit("global.preferences", "editor: nvim\n");
    let preview = session.build_preview();
    session.confirm(&preview);

    // A bounded recovery that reproduces the confirmed preview exactly may
    // continue without asking again.
    assert!(!session.recovery_requires_reconfirmation(&preview));

    // A recovery that appends a command re-enters the confirmation gate.
    let mut extra_command = preview.clone();
    extra_command
        .commands
        .push("shelbi reload --project demo".to_string());
    assert!(session.recovery_requires_reconfirmation(&extra_command));

    // A recovery that alters a staged byte re-enters the confirmation gate.
    let mut extra_bytes = preview.clone();
    extra_bytes.changes[0].after.extend_from_slice(b"# recovered\n");
    assert!(session.recovery_requires_reconfirmation(&extra_bytes));
}

// ---------------------------------------------------------------------------
// Criterion: every supported configuration family has a valid and an invalid
// end-to-end scenario.

struct FamilyCase {
    logical_id: &'static str,
    valid: &'static str,
    invalid: &'static str,
    invalid_code: &'static str,
}

/// Each family, edited on the flat fixture, with a valid body that lints clean
/// and an invalid body that produces its signature diagnostic. Split-only
/// registration is covered separately below.
fn flat_family_cases() -> Vec<FamilyCase> {
    vec![
        FamilyCase {
            logical_id: "global.preferences",
            valid: "editor: nvim\n",
            invalid: "editor: nvim\nmystery_field: true\n",
            invalid_code: "CONFIG_UNKNOWN_FIELD",
        },
        FamilyCase {
            logical_id: "global.keybindings",
            valid: "defaults:\n  sidebar:\n    nav_up: w\n",
            invalid: "defaults:\n  sidebar:\n    nav_up: x\n    nav_down: x\n",
            invalid_code: "KEYBINDINGS_COLLISION",
        },
        FamilyCase {
            logical_id: "global.hub",
            valid: "# hub-wide config\nprojects: {}\n",
            invalid: "projects: {}\nbogus_hub_key: 1\n",
            invalid_code: "CONFIG_UNKNOWN_FIELD",
        },
        FamilyCase {
            logical_id: "project.demo.registration",
            valid: "# registration\nname: Demo\ndefault_branch: main\ndefault_workflow: task\norchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\nrepo: /tmp/demo-repo\nmachines:\n  - name: hub\n    kind: local\n    work_dir: /tmp\nworkspaces:\n  - name: alpha\n    machine: hub\n    runner: claude\n    tags: [build]\n",
            invalid: "name: Demo\ndefault_branch: main\ndefault_workflow: task\nconfig_mode: in-repo\norchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\nrepo: /tmp/demo-repo\nmachines:\n  - name: hub\n    kind: local\n    work_dir: /tmp\nworkspaces:\n  - name: alpha\n    machine: hub\n    runner: claude\n    tags: [build]\n",
            invalid_code: "PROJECT_LAYOUT_INCONSISTENT",
        },
        FamilyCase {
            logical_id: "project.demo.statuses",
            valid: "statuses:\n  - id: todo\n    name: Backlog\n    category: ready\n  - id: review\n    name: Review\n    category: handoff\n  - id: done\n    name: Done\n    category: done\n",
            invalid: "statuses:\n  - id: todo\n    name: To Do\n    category: ready\n  - id: done\n    name: Done\n    category: done\nbogus: true\n",
            invalid_code: "CONFIG_UNKNOWN_FIELD",
        },
        FamilyCase {
            logical_id: "project.demo.workflow.task",
            valid: "# workflow\nname: task\nstatuses:\n  - id: todo\n    owner: user\n  - id: done\n    owner: user\ntransitions:\n  - from: todo\n    to: done\n    actions: []\ngit:\n  base_branch: main\n",
            invalid: "name: task\nstatuses:\n  - id: nonexistent\n    owner: user\n  - id: done\n    owner: user\ntransitions:\n  - from: nonexistent\n    to: done\n    actions: []\ngit:\n  base_branch: main\n",
            invalid_code: "WORKFLOW_STATUS_REFERENCE_INVALID",
        },
        FamilyCase {
            logical_id: "project.demo.workspace-settings-template",
            valid: "{\"hooks\": {}, \"env\": {}}\n",
            invalid: "{\"custom\": \"{{unsupported}}\"}\n",
            invalid_code: "TEMPLATE_UNKNOWN_PLACEHOLDER",
        },
        FamilyCase {
            logical_id: "project.demo.agent.developer.settings",
            valid: "{\"permissions\": {}}\n",
            invalid: "{ not valid json\n",
            invalid_code: "CONFIG_JSON_SYNTAX",
        },
        FamilyCase {
            logical_id: "project.demo.zenmode",
            valid: "Backlog keeps moving.\n\nUpdated policy.\n",
            invalid: "\n\nMissing summary line.\n",
            invalid_code: "ZENMODE_SUMMARY_MISSING",
        },
        FamilyCase {
            logical_id: "project.demo.agent.developer.instructions",
            valid: "# developer\n\nUpdated instructions.\n",
            invalid: "   \n",
            invalid_code: "MARKDOWN_EMPTY",
        },
        FamilyCase {
            logical_id: "project.demo.agents.shared-preamble",
            valid: "# Project context\n\nUpdated shared preamble.\n",
            invalid: "",
            invalid_code: "MARKDOWN_EMPTY",
        },
        FamilyCase {
            logical_id: "project.demo.agent.review.skill.load-run-detection.SKILL",
            valid: "# Load and run\n\nUpdated skill.\n",
            invalid: "",
            invalid_code: "MARKDOWN_EMPTY",
        },
    ]
}

#[test]
fn every_flat_family_has_valid_and_invalid_scenario() {
    for case in flat_family_cases() {
        // Valid scenario: a fresh session, one clean edit, clean staged lint.
        let (_fixture, root) = scaffold_flat("claude");
        let session = Session::open(&root, "demo");
        session.edit(case.logical_id, case.valid);
        let (clean, report) = session.lint_staged();
        assert!(
            clean,
            "family `{}` valid scenario should lint clean: {report}",
            case.logical_id
        );

        // Invalid scenario: the same edit made bad produces its diagnostic and
        // a non-zero exit.
        let (_fixture, root) = scaffold_flat("claude");
        let session = Session::open(&root, "demo");
        session.edit(case.logical_id, case.invalid);
        let (clean, report) = session.lint_staged();
        assert!(
            !clean,
            "family `{}` invalid scenario must fail lint: {report}",
            case.logical_id
        );
        assert!(
            has_code(&report, case.invalid_code),
            "family `{}` invalid scenario expected `{}`: {report}",
            case.logical_id,
            case.invalid_code
        );
    }
}

/// The split registration family (`project.yaml` + `local.yaml`) only exists in
/// the in-repo layout, so it gets its own valid/invalid pair.
#[test]
fn split_registration_family_has_valid_and_invalid_scenario() {
    // Valid: edit the shared half, keeping `config_mode: in-repo`.
    let (_fixture, root) = scaffold_split("claude");
    let session = Session::open(&root, "demo");
    session.edit(
        "project.demo.registration.shared",
        "name: Demo Two\nconfig_mode: in-repo\ndefault_branch: main\ndefault_workflow: task\norchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n",
    );
    let (clean, report) = session.lint_staged();
    assert!(clean, "split registration valid scenario should lint clean: {report}");

    // Invalid: drop the in-repo marker from the shared half.
    let (_fixture, root) = scaffold_split("claude");
    let session = Session::open(&root, "demo");
    session.edit(
        "project.demo.registration.shared",
        "name: Demo\ndefault_branch: main\ndefault_workflow: task\norchestrator:\n  runner: claude\nagent_runners:\n  claude:\n    command: claude\n    flags: []\n",
    );
    let (clean, report) = session.lint_staged();
    assert!(!clean, "split registration invalid scenario must fail lint: {report}");
    assert!(
        has_code(&report, "PROJECT_LAYOUT_INCONSISTENT"),
        "expected PROJECT_LAYOUT_INCONSISTENT: {report}"
    );
}
