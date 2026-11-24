//! Byte-for-byte freeze for the generated runner artifacts.
//!
//! The runner-adapter refactor moves Claude's launch-flag assembly, hook
//! deployment set, and settings rendering behind a single
//! [`shelbi_agent::RunnerAdapter`]. Nothing about the *output* is allowed to
//! change while that plumbing is reshaped, so this module snapshots the four
//! Claude-visible artifacts named in the task — orchestrator argv, worker
//! argv, `settings.json`, and the deployed hook script set — and asserts the
//! current code reproduces them exactly.
//!
//! OpenCode's artifacts are frozen the same way under `tests/golden/opencode/`:
//! the orchestrator and worker launch lines, the config layer a worker's
//! dispatch writes to `.shelbi/opencode/config.json` (one file per permission
//! ceiling, plus the model variant, the review agent's edit deny and the
//! merge over a user's own `OPENCODE_CONFIG_CONTENT`), the instructions file
//! and the plugin's package manifest.
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
        "golden `{name}` drifted — a generated runner artifact changed. If \
         intended, regenerate with UPDATE_GOLDEN=1."
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
        &crate::orch_bootstrap_prompt_base("myapp"),
        false,
        None,
    );
    assert_golden("claude/orchestrator-argv.txt", &argv);
}

/// Awareness (v2): the second, session-only `--plugin-dir` and the bootstrap
/// that leaves event delivery to the plugin. v1 stays frozen above.
#[test]
fn claude_awareness_orchestrator_argv_is_frozen() {
    let argv = crate::orchestrator_launch_command(
        "/usr/local/bin/shelbi",
        &claude_runner(),
        "myapp",
        Path::new("/tmp/myapp"),
        None,
        true,
        None,
    );
    assert_golden("claude/orchestrator-awareness-argv.txt", &argv);

    // v1 (the plugin not staged) launches exactly as before.
    let v1 = crate::orchestrator_launch_command(
        "/usr/local/bin/shelbi",
        &claude_runner(),
        "myapp",
        Path::new("/tmp/myapp"),
        None,
        false,
        None,
    );
    assert_golden("claude/orchestrator-argv.txt", &v1);
}

/// The OpenCode orchestrator's launch, awareness (v2) and heartbeat (v1):
/// instructions and skill through the config layer, the project's own
/// session, and (v2) the plugin playing the orchestrator.
#[test]
fn opencode_orchestrator_argv_is_frozen() {
    let opencode = AgentRunnerSpec { command: "opencode".into(), ..claude_runner() };
    for (name, plugin) in [("opencode/orchestrator-awareness-argv.txt", true), ("opencode/orchestrator-argv.txt", false)] {
        let argv = crate::orchestrator_launch_command(
            "/usr/local/bin/shelbi",
            &opencode,
            "myapp",
            Path::new("/tmp/myapp"),
            None,
            plugin,
            None,
        );
        assert_golden(name, &argv);
    }
}

#[test]
fn claude_worker_argv_is_frozen() {
    let cold = crate::workspace::workspace_launch_command(&claude_runner(), Some("auto"), false);
    assert_golden("claude/worker-argv-cold.txt", &cold);

    let resume = crate::workspace::workspace_launch_command(&claude_runner(), Some("auto"), true);
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

fn opencode_runner() -> AgentRunnerSpec {
    AgentRunnerSpec { command: "opencode".into(), ..claude_runner() }
}

/// The worktree the OpenCode goldens are generated for.
const OPENCODE_WT: &str = "/tmp/myapp-wt";

/// The layer a worker's dispatch builds beside the spec's own flags: the
/// staged instructions, skills and Shelbi's plugin, all absolute paths into
/// the worktree.
fn opencode_worker_layer() -> shelbi_agent::OpenCodeLayer {
    let abs = |rel: &str| format!("{OPENCODE_WT}/{rel}");
    shelbi_agent::OpenCodeLayer {
        instructions: vec![abs(crate::workspace::OPENCODE_INSTRUCTIONS_REL)],
        skills: vec![abs(crate::workspace::OPENCODE_SKILLS_REL)],
        plugins: vec![abs(crate::workspace::OPENCODE_PLUGIN_DIR_REL)],
        ..Default::default()
    }
}

/// The JSON a worker's dispatch writes to `.shelbi/opencode/config.json` for
/// `spec` and `layer` (the same call `deploy_opencode_layer` makes).
fn opencode_config(spec: &AgentRunnerSpec, layer: &shelbi_agent::OpenCodeLayer) -> shelbi_agent::OpenCodeConfig {
    shelbi_agent::RunnerAdapter::for_spec(spec).opencode_config(spec, layer)
}

/// The worker's launch line: the dispatch's named session, the config layer
/// read from its file, and the startup prompt through `--prompt`. A resume
/// reopens the same session and launches identically (never `--continue`);
/// only `bypassPermissions` adds `--auto`.
#[test]
fn opencode_worker_argv_is_frozen() {
    let id = shelbi_core::opencode_session_id("myapp", "alpha", "fix-it", 0);
    let title = shelbi_core::opencode_session_title("alpha", "fix-it");
    let runner = shelbi_agent::with_opencode_session(&opencode_runner(), &id, &title);
    let launch = |mode: &str, resume: bool| {
        crate::workspace::workspace_launch_command_with_layer(
            &runner,
            Some(mode),
            resume,
            Some(crate::workspace::WORKTREE_STARTUP_PROMPT_REL),
            Some(crate::workspace::OPENCODE_CONFIG_REL),
        )
    };
    assert_golden("opencode/worker-argv.txt", &launch("auto", false));
    assert_eq!(launch("auto", true), launch("auto", false), "a resume reopens the named session");
    assert_golden("opencode/worker-argv-bypass.txt", &launch("bypassPermissions", false));
}

/// The config layer for each permission ceiling (none, `plan`, `default`,
/// `read-only`, `auto`/`acceptEdits`, `bypassPermissions`), on a worker with
/// instructions, skills and the plugin.
#[test]
fn opencode_config_layer_per_ceiling_is_frozen() {
    let layer = opencode_worker_layer();
    for (name, mode) in [
        ("none", None),
        ("plan", Some("plan")),
        ("default", Some("default")),
        ("read-only", Some("read-only")),
        ("auto", Some("auto")),
        ("bypass", Some("bypassPermissions")),
    ] {
        let spec = shelbi_agent::with_permission_mode(&opencode_runner(), mode);
        let config = opencode_config(&spec, &layer);
        assert!(!config.user_config_unreadable);
        assert_eq!(config.effort_unapplied, None);
        assert_golden(&format!("opencode/config-{name}.json"), &config.json.expect("the layer names the plugin"));
    }
    // `acceptEdits` ranks with `auto`, and so gets the same rules.
    let auto = opencode_config(&shelbi_agent::with_permission_mode(&opencode_runner(), Some("auto")), &layer);
    let accept = opencode_config(&shelbi_agent::with_permission_mode(&opencode_runner(), Some("acceptEdits")), &layer);
    assert_eq!(accept, auto);
}

/// The review agent's no-edit posture appends an `edit` deny over the
/// ceiling's rules.
#[test]
fn opencode_config_layer_review_edit_deny_is_frozen() {
    let spec = shelbi_agent::with_edits_denied(
        &shelbi_agent::with_permission_mode(&opencode_runner(), Some("auto")),
        true,
    );
    let config = opencode_config(&spec, &opencode_worker_layer());
    assert_golden("opencode/config-review.json", &config.json.unwrap());
}

/// A model with a resolved effort the model list reports: the model goes on
/// the `build` and `plan` agents with the effort as its `#variant`.
#[test]
fn opencode_config_layer_model_variant_is_frozen() {
    let spec = shelbi_agent::with_reasoning_effort(
        &shelbi_agent::with_model(
            &shelbi_agent::with_permission_mode(&opencode_runner(), Some("auto")),
            Some("anthropic/claude-sonnet-5-5"),
        ),
        Some(shelbi_core::ReasoningEffort::High),
    );
    let layer = shelbi_agent::OpenCodeLayer {
        models: vec![shelbi_core::OpenCodeModel {
            id: "anthropic/claude-sonnet-5-5".into(),
            variants: vec!["low".into(), "high".into(), "max".into()],
        }],
        ..opencode_worker_layer()
    };
    let config = opencode_config(&spec, &layer);
    assert_eq!(config.effort_unapplied, None);
    assert_golden("opencode/config-model-variant.json", &config.json.unwrap());
}

/// The user's own `OPENCODE_CONFIG_CONTENT` merged under Shelbi's keys: their
/// keys stay, their list entries come first (so Shelbi's permission rules
/// still load last), and Shelbi wins only where it sets a value.
#[test]
fn opencode_config_layer_user_merge_is_frozen() {
    let spec = shelbi_agent::with_model(
        &shelbi_agent::with_permission_mode(&opencode_runner(), Some("default")),
        Some("anthropic/claude-sonnet-5-5"),
    );
    let layer = shelbi_agent::OpenCodeLayer {
        user_config: Some(
            r#"{"theme":"tokyonight","instructions":["/home/me/house-rules.md"],"agents":{"build":{"model":"openai/gpt-5","permissions":[{"action":"shell","resource":"git *","effect":"allow"}]}}}"#
                .into(),
        ),
        ..opencode_worker_layer()
    };
    let config = opencode_config(&spec, &layer);
    assert!(!config.user_config_unreadable);
    assert_golden("opencode/config-user-merge.json", &config.json.unwrap());
}

/// The instructions file a worker's layer names (the role and authority
/// framing ahead of the composed instructions), and the plugin directory's
/// package manifest.
#[test]
fn opencode_instructions_and_plugin_manifest_are_frozen() {
    let body = crate::workspace::render_opencode_instructions(
        "developer",
        "# Developer\n\nImplement the task on its branch and write the ready marker.\n",
    );
    assert_golden("opencode/instructions.md", &body);

    let wt = std::env::temp_dir().join(format!("shelbi-golden-opencode-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&wt);
    std::fs::create_dir_all(&wt).unwrap();
    crate::workspace::deploy_opencode_plugin(&shelbi_core::Host::Local, &wt).unwrap();
    let dir = wt.join(crate::workspace::OPENCODE_PLUGIN_DIR_REL);
    let manifest = std::fs::read_to_string(dir.join("package.json")).unwrap();
    let index = std::fs::read_to_string(dir.join("index.js")).unwrap();
    let _ = std::fs::remove_dir_all(&wt);
    assert_golden("opencode/plugin-package.json", &manifest);
    assert_eq!(index, crate::workspace::opencode_plugin_js(), "the plugin is written as embedded");
}
