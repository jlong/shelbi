//! Mode-aware path helpers for [`Project`].
//!
//! Every filesystem path under `~/.shelbi/projects/<name>/` should be
//! routed through the [`ProjectPaths`] trait, which decides between the
//! historical global layout and the in-repo layout based on the
//! project's [`shelbi_core::ConfigMode`]:
//!
//! * **Global mode** (the default and the pre-split behavior): shared
//!   and user-local halves live under `~/.shelbi/projects/<name>/`, so
//!   config paths and state paths both resolve there.
//! * **In-repo mode**: the shared half is committed to
//!   `<repo>/.shelbi/project.yaml`, so config paths (workflows, agent
//!   prompts, templates) resolve to `<repo>/.shelbi/…`. State paths
//!   (tasks, `state.json`, `events.log`, workspaces, `.claude/`) stay in
//!   `~/.shelbi/` regardless of the mode — see the type table on
//!   [`ProjectPaths`].
//!
//! See `Plans/in-repo-vs-global-project-config.md` for the design
//! rationale.

use std::path::PathBuf;

use shelbi_core::{ConfigMode, Project, Result};

use crate::{expand_tilde_path, expand_tilde_str, project_dir, shelbi_home};

/// Path resolution helpers for a [`Project`].
///
/// The trait's two halves — *config paths* and *state paths* — decide
/// where a given file lives based on [`Project::config_mode`]:
///
/// | Helper                              | `Global` (default)                          | `InRepo`                            |
/// | ----------------------------------- | ------------------------------------------- | ----------------------------------- |
/// | `workflows_dir`                     | `~/.shelbi/projects/<name>/workflows/`      | `<repo>/.shelbi/workflows/`         |
/// | `agents_dir`                        | `~/.shelbi/projects/<name>/agents/`         | `<repo>/.shelbi/agents/`            |
/// | `workspace_settings_template_path`  | `~/.shelbi/projects/<name>/workspace-…`     | `<repo>/.shelbi/workspace-…`        |
/// | `statuses_yaml_path`                | `~/.shelbi/projects/<name>/workflows/…`     | `<repo>/.shelbi/workflows/…`        |
/// | `state_json_path`                   | `~/.shelbi/projects/<name>/state.json`      | *same*                              |
/// | `tasks_dir`                         | `~/.shelbi/projects/<name>/tasks/`          | *same*                              |
/// | `handoff_md_path`                   | `~/.shelbi/projects/<name>/HANDOFF.md`      | *same*                              |
/// | `events_log_path`                   | `~/.shelbi/events.log`                      | *same*                              |
/// | `workspaces_dir`                    | `~/.shelbi/projects/<name>/workspaces/`     | *same*                              |
/// | `claude_dir`                        | `~/.shelbi/projects/<name>/.claude/`        | *same*                              |
///
/// Callers holding a `Project` should prefer these methods over the
/// string-based free functions in [`crate`] so in-repo projects
/// resolve their config to `<repo>/.shelbi/…` without extra plumbing.
///
/// The `workspace_settings_template_path` helper respects
/// [`Project::workspace_settings_template`] as an absolute override —
/// when set, both modes yield the same path (with `~` expansion).
pub trait ProjectPaths {
    /// Root directory for the project's *config* half. Points at
    /// `<repo>/.shelbi/` under [`ConfigMode::InRepo`] and at
    /// `~/.shelbi/projects/<name>/` under [`ConfigMode::Global`].
    fn config_root(&self) -> Result<PathBuf>;

    /// Root directory for the project's *state* half. Always
    /// `~/.shelbi/projects/<name>/`, regardless of mode.
    fn state_root(&self) -> Result<PathBuf>;

    // ---- Config helpers (mode-aware) -------------------------------------

    /// `<config_root>/workflows/`.
    fn workflows_dir(&self) -> Result<PathBuf>;

    /// `<config_root>/agents/`.
    fn agents_dir(&self) -> Result<PathBuf>;

    /// `<config_root>/workflows/statuses.yaml`.
    fn statuses_yaml_path(&self) -> Result<PathBuf>;

    /// `<config_root>/workspace-settings.json.template`, unless the
    /// project's `workspace_settings_template` override is set — in
    /// which case that path is returned verbatim (with `~` expansion).
    fn workspace_settings_template_path(&self) -> Result<PathBuf>;

    // ---- State helpers (always global) -----------------------------------

    /// `<state_root>/state.json`.
    fn state_json_path(&self) -> Result<PathBuf>;

    /// `<state_root>/tasks/`.
    fn tasks_dir(&self) -> Result<PathBuf>;

    /// `<state_root>/HANDOFF.md`.
    fn handoff_md_path(&self) -> Result<PathBuf>;

    /// `<shelbi_home>/events.log`. Cross-project, hence outside
    /// `state_root`.
    fn events_log_path(&self) -> Result<PathBuf>;

    /// `<state_root>/workspaces/` — per-project workspace status
    /// directory. Distinct from the cross-project
    /// [`crate::workspaces_dir`] free function that returns
    /// `~/.shelbi/workspaces/`.
    fn workspaces_dir(&self) -> Result<PathBuf>;

    /// `<state_root>/.claude/` — the orchestrator dashboard workdir's
    /// Claude Code deploy footprint.
    fn claude_dir(&self) -> Result<PathBuf>;
}

impl ProjectPaths for Project {
    fn config_root(&self) -> Result<PathBuf> {
        match self.config_mode.unwrap_or_default() {
            ConfigMode::Global => project_dir(&self.name),
            ConfigMode::InRepo => Ok(expand_tilde_str(&self.repo).join(".shelbi")),
        }
    }

    fn state_root(&self) -> Result<PathBuf> {
        project_dir(&self.name)
    }

    fn workflows_dir(&self) -> Result<PathBuf> {
        Ok(self.config_root()?.join("workflows"))
    }

    fn agents_dir(&self) -> Result<PathBuf> {
        Ok(self.config_root()?.join("agents"))
    }

    fn statuses_yaml_path(&self) -> Result<PathBuf> {
        Ok(self.workflows_dir()?.join("statuses.yaml"))
    }

    fn workspace_settings_template_path(&self) -> Result<PathBuf> {
        if let Some(p) = &self.workspace_settings_template {
            return Ok(expand_tilde_path(p));
        }
        Ok(self.config_root()?.join("workspace-settings.json.template"))
    }

    fn state_json_path(&self) -> Result<PathBuf> {
        Ok(self.state_root()?.join("state.json"))
    }

    fn tasks_dir(&self) -> Result<PathBuf> {
        Ok(self.state_root()?.join("tasks"))
    }

    fn handoff_md_path(&self) -> Result<PathBuf> {
        Ok(self.state_root()?.join("HANDOFF.md"))
    }

    fn events_log_path(&self) -> Result<PathBuf> {
        Ok(shelbi_home()?.join("events.log"))
    }

    fn workspaces_dir(&self) -> Result<PathBuf> {
        Ok(self.state_root()?.join("workspaces"))
    }

    fn claude_dir(&self) -> Result<PathBuf> {
        Ok(self.state_root()?.join(".claude"))
    }
}

/// Assert at compile time that the trait is object-safe / callable in
/// generic code — the workflow loader relies on this shape.
#[allow(dead_code)]
fn _paths_dyn_probe(p: &dyn ProjectPaths) -> Result<PathBuf> {
    p.state_root()
}

// ---------------------------------------------------------------------------
// Callsite scan test
//
// Guard against regressions where a new caller hand-composes a per-project
// path like `<something>.join("projects").join(name)` instead of routing
// through [`ProjectPaths`] or one of the crate's existing helper wrappers.
//
// The scanner walks every `.rs` file under `crates/*/src/` and flags any
// **executable** line whose `.shelbi/projects/…` fragment is being
// concatenated onto a `PathBuf`. Doc comments (`///`, `//!`, `//`) and
// user-facing string literals inside `println!`/`eprintln!`/`format!`
// stay out of scope — the acceptance criterion is about routing, not
// display copy — so those lines never trigger. This keeps the check
// robust without a hand-maintained per-file allowlist.

#[cfg(test)]
mod scan {
    use std::fs;
    use std::path::PathBuf;

    /// This module owns the pattern by design — the docs above list
    /// every allowed shape. Skip it wholesale so the doc examples don't
    /// fail their own scan.
    const SELF_SUFFIX: &str = "shelbi-state/src/project_paths.rs";

    /// Locate the workspace root by walking up from CARGO_MANIFEST_DIR
    /// until we hit a `Cargo.toml` with a `[workspace]` section.
    fn workspace_root() -> PathBuf {
        let start = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let mut cur: &std::path::Path = &start;
        loop {
            let cargo = cur.join("Cargo.toml");
            if let Ok(text) = fs::read_to_string(&cargo) {
                if text.contains("[workspace]") {
                    return cur.to_path_buf();
                }
            }
            match cur.parent() {
                Some(p) => cur = p,
                None => panic!("could not locate workspace root from {}", start.display()),
            }
        }
    }

    fn collect_rs(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let file_type = match entry.file_type() {
                Ok(t) => t,
                Err(_) => continue,
            };
            if file_type.is_dir() {
                if path.file_name().map(|n| n == "target").unwrap_or(false) {
                    continue;
                }
                collect_rs(&path, out);
            } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
                out.push(path);
            }
        }
    }

    /// Does the line look like a caller hand-composing a per-project
    /// path via `PathBuf::join`? We look for the specific shapes:
    ///
    /// * `.join("projects")` — the string literal that turns a
    ///   `shelbi_home()` handle into `~/.shelbi/projects/`,
    /// * `.join("projects/…")` — the same, with a subpath fragment,
    /// * `.join("<name>/state.json")` / `.join("state.json")` chained
    ///   against a home dir — caught by the more direct
    ///   `home.join(\"projects\")` above.
    ///
    /// The check is intentionally narrow: it targets the *literal*
    /// `"projects"` string as an argument to `.join(...)`, which is the
    /// exact shape a new callsite would fall back to if they didn't
    /// know about the trait.
    fn line_hand_builds_projects_path(line: &str) -> bool {
        let trimmed = line.trim_start();
        // Doc comments and prose are out of scope.
        if trimmed.starts_with("//") {
            return false;
        }
        // The exact shape: `.join("projects` — covers both
        // `.join("projects")` and `.join("projects/xyz")`. This is the
        // fragment `projects_dir()` alone would encode; anything that
        // already goes through `projects_dir()` / `project_dir()` won't
        // hit the pattern.
        line.contains(".join(\"projects\"")
    }

    /// Files that own the mapping from `shelbi_home()` to
    /// `~/.shelbi/projects/`. The raw `.join("projects")` literal has to
    /// live somewhere so every other caller can route through them.
    const CRATE_OWNER_SUFFIXES: &[&str] = &[
        "shelbi-state/src/lib.rs",
        "shelbi-state/src/hub_config.rs",
        "shelbi-state/src/resolve.rs",
    ];

    /// Skip everything after the first `#[cfg(test)]` line in a file —
    /// test fixtures routinely hand-build paths for setup/tear-down and
    /// aren't the callers we're guarding here.
    fn strip_test_scope(text: &str) -> impl Iterator<Item = (usize, &str)> {
        let cut = text
            .lines()
            .enumerate()
            .find(|(_, l)| l.trim_start().starts_with("#[cfg(test)]"))
            .map(|(i, _)| i)
            .unwrap_or(usize::MAX);
        text.lines().enumerate().take_while(move |(i, _)| *i < cut)
    }

    #[test]
    fn no_new_callsite_hand_builds_a_per_project_shelbi_path() {
        let root = workspace_root();
        let crates_dir = root.join("crates");
        assert!(
            crates_dir.is_dir(),
            "expected crates/ at {}",
            crates_dir.display()
        );
        let mut files = Vec::new();
        collect_rs(&crates_dir, &mut files);
        files.sort();

        let mut offenders: Vec<String> = Vec::new();
        for path in &files {
            let s = path.to_string_lossy().replace('\\', "/");
            if s.ends_with(SELF_SUFFIX) {
                continue;
            }
            if CRATE_OWNER_SUFFIXES
                .iter()
                .any(|suffix| s.ends_with(*suffix))
            {
                continue;
            }
            let Ok(text) = fs::read_to_string(path) else {
                continue;
            };
            for (i, line) in strip_test_scope(&text) {
                if line_hand_builds_projects_path(line) {
                    offenders.push(format!(
                        "{}:{}  {}",
                        path.strip_prefix(&root).unwrap_or(path).display(),
                        i + 1,
                        line.trim_end()
                    ));
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "found `.join(\"projects…\")` outside the crate-owned helpers:\n{}\n\
             Route through `shelbi_state::projects_dir()` / `project_dir()` \
             or the mode-aware `ProjectPaths` trait \
             (see shelbi-state/src/project_paths.rs).",
            offenders.join("\n"),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use shelbi_core::{
        AgentRunnerSpec, ConfigMode, GitConfig, HeartbeatConfig, Machine, MachineKind,
        OrchestratorSpec, ZenConfig,
    };

    use crate::test_lock::LOCK as TEST_LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-project-paths-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn fixture_project(name: &str, repo: &str, mode: Option<ConfigMode>) -> Project {
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                prompt_injection: None,
                dialog_signatures: vec![],
            },
        );
        Project {
            name: name.into(),
            repo: repo.into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: mode,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.into(),
                host: None,
                tags: Vec::new(),
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn global_mode_config_paths_land_under_home_projects_dir() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        for mode in [None, Some(ConfigMode::Global)] {
            let p = fixture_project("myapp", "/repos/myapp", mode);
            assert_eq!(p.config_root().unwrap(), home.join("projects/myapp"));
            assert_eq!(
                p.workflows_dir().unwrap(),
                home.join("projects/myapp/workflows")
            );
            assert_eq!(p.agents_dir().unwrap(), home.join("projects/myapp/agents"));
            assert_eq!(
                p.statuses_yaml_path().unwrap(),
                home.join("projects/myapp/workflows/statuses.yaml"),
            );
            assert_eq!(
                p.workspace_settings_template_path().unwrap(),
                home.join("projects/myapp/workspace-settings.json.template"),
            );
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn in_repo_mode_config_paths_land_under_repo_dot_shelbi() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let repo = PathBuf::from("/repos/myapp");
        let p = fixture_project("myapp", repo.to_str().unwrap(), Some(ConfigMode::InRepo));
        assert_eq!(p.config_root().unwrap(), repo.join(".shelbi"));
        assert_eq!(p.workflows_dir().unwrap(), repo.join(".shelbi/workflows"));
        assert_eq!(p.agents_dir().unwrap(), repo.join(".shelbi/agents"));
        assert_eq!(
            p.statuses_yaml_path().unwrap(),
            repo.join(".shelbi/workflows/statuses.yaml"),
        );
        assert_eq!(
            p.workspace_settings_template_path().unwrap(),
            repo.join(".shelbi/workspace-settings.json.template"),
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_paths_always_land_under_home_projects_dir_regardless_of_mode() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        for mode in [None, Some(ConfigMode::Global), Some(ConfigMode::InRepo)] {
            let p = fixture_project("myapp", "/repos/myapp", mode);
            assert_eq!(p.state_root().unwrap(), home.join("projects/myapp"));
            assert_eq!(
                p.state_json_path().unwrap(),
                home.join("projects/myapp/state.json")
            );
            assert_eq!(p.tasks_dir().unwrap(), home.join("projects/myapp/tasks"));
            assert_eq!(
                p.handoff_md_path().unwrap(),
                home.join("projects/myapp/HANDOFF.md")
            );
            assert_eq!(
                p.workspaces_dir().unwrap(),
                home.join("projects/myapp/workspaces")
            );
            assert_eq!(p.claude_dir().unwrap(), home.join("projects/myapp/.claude"));
            assert_eq!(p.events_log_path().unwrap(), home.join("events.log"));
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_settings_template_override_wins_in_both_modes() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        for mode in [Some(ConfigMode::Global), Some(ConfigMode::InRepo)] {
            let mut p = fixture_project("myapp", "/repos/myapp", mode);
            p.workspace_settings_template = Some(PathBuf::from("/etc/shelbi/tpl.json"));
            assert_eq!(
                p.workspace_settings_template_path().unwrap(),
                PathBuf::from("/etc/shelbi/tpl.json"),
            );
            // With `~` expansion — matches the pre-trait behavior.
            p.workspace_settings_template = Some(PathBuf::from("~/custom/tpl.json"));
            assert_eq!(
                p.workspace_settings_template_path().unwrap(),
                dirs::home_dir().unwrap().join("custom/tpl.json"),
            );
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn config_and_state_roots_diverge_only_in_repo_mode() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let global = fixture_project("g", "/repos/g", Some(ConfigMode::Global));
        assert_eq!(global.config_root().unwrap(), global.state_root().unwrap());

        let in_repo = fixture_project("ir", "/repos/ir", Some(ConfigMode::InRepo));
        assert_ne!(
            in_repo.config_root().unwrap(),
            in_repo.state_root().unwrap()
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
