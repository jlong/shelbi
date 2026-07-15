//! Byte-for-byte freeze for the generated Claude artifacts.
//!
//! The runner-adapter refactor moves Claude's launch-flag assembly, hook
//! deployment set, and settings rendering behind a single
//! [`shelbi_agent::RunnerAdapter`]. Nothing about the *output* is allowed to
//! change while that plumbing is reshaped, so this module snapshots the four
//! Claude-visible artifacts named in the task — orchestrator argv, worker
//! argv, `settings.json`, and the deployed hook script set — and asserts the
//! current code reproduces them exactly.
//!
//! Regenerate intentionally (only when an artifact is *meant* to change) with
//! `UPDATE_GOLDEN=1 cargo test -p shelbi-orchestrator golden`.

use std::path::{Path, PathBuf};

use shelbi_core::{AgentRunnerSpec, Project};

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Compare `actual` against the committed golden file `name`, or (re)write it
/// when `UPDATE_GOLDEN` is set in the environment.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!("golden `{name}` missing ({e}); run with UPDATE_GOLDEN=1 to create it")
    });
    assert_eq!(
        actual, expected,
        "golden `{name}` drifted — a Claude artifact changed. If intended, \
         regenerate with UPDATE_GOLDEN=1."
    );
}

fn claude_runner() -> AgentRunnerSpec {
    AgentRunnerSpec {
        command: "claude".into(),
        flags: vec![],
        prompt_injection: None,
        dialog_signatures: vec![],
        integration: None,
    }
}

/// A project whose settings template resolves to a nonexistent override path,
/// forcing [`shelbi_state::render_workspace_settings`] onto the bundled default
/// template. Keeps the settings snapshot independent of `SHELBI_HOME`.
fn settings_project() -> Project {
    // `workspace_settings_template` points at an absolute path that does not
    // exist, so the renderer falls back to the bundled default template
    // without depending on `SHELBI_HOME`.
    Project::from_yaml_str(
        r#"
name: golden
repo: /tmp/golden
workspace_permissions_mode: auto
workspace_settings_template: /nonexistent/golden.template
machines:
  - name: local
    kind: local
    work_dir: /tmp/golden
orchestrator:
  runner: claude
agent_runners:
  claude:
    command: claude
workspaces: []
"#,
    )
    .unwrap()
}

#[test]
fn claude_orchestrator_argv_is_frozen() {
    let argv = crate::launch_with_bootstrap(
        &claude_runner(),
        "myapp",
        Path::new("/tmp/myapp"),
        crate::ORCH_BOOTSTRAP_PROMPT,
    );
    assert_golden("claude/orchestrator-argv.txt", &argv);
}

#[test]
fn claude_worker_argv_is_frozen() {
    let cold =
        crate::workspace::workspace_launch_command(&claude_runner(), "auto", true, false);
    assert_golden("claude/worker-argv-cold.txt", &cold);

    let resume =
        crate::workspace::workspace_launch_command(&claude_runner(), "auto", true, true);
    assert_golden("claude/worker-argv-resume.txt", &resume);
}

#[test]
fn claude_workspace_settings_json_is_frozen() {
    let rendered = shelbi_state::render_workspace_settings(&settings_project()).unwrap();
    assert_golden("claude/settings.json", &rendered);
}

#[test]
fn claude_hook_script_set_is_frozen() {
    // The deployed hook set: one `rel_path\nexecutable\n---\nbody` record per
    // file, in declaration order. Freezes both the file list and every body.
    let mut manifest = String::new();
    for file in crate::workspace::RUNNER_HOOK_FILES {
        manifest.push_str(file.rel_path);
        manifest.push('\n');
        manifest.push_str(if file.executable { "exec" } else { "plain" });
        manifest.push_str("\n----\n");
        manifest.push_str(file.body);
        manifest.push_str("\n========\n");
    }
    assert_golden("claude/hooks.manifest", &manifest);
}
