//! One-way migration of a project's config from global mode
//! (`~/.shelbi/projects/<name>.yaml` + `~/.shelbi/projects/<name>/…`) to
//! in-repo mode (`<repo>/.shelbi/project.yaml` + `<repo>/.shelbi/…` for
//! config, `~/.shelbi/projects/<name>/local.yaml` + `~/.shelbi/projects/<name>/…`
//! for state).
//!
//! [`plan_in_repo_migration`] resolves the project (accepting global,
//! in-repo, or half-migrated on-disk states) and returns a
//! [`MigrationPlan`] listing the concrete actions needed to reach a fully
//! in-repo layout. [`apply_migration_plan`] then executes those actions
//! atomically-ish (write-then-swap for YAMLs, copy-then-remove for
//! directories so a cross-device migration still succeeds).
//!
//! The command is intentionally idempotent:
//!
//! * Already fully in in-repo mode → plan is empty; apply is a no-op.
//! * Partially migrated (e.g. shared YAML written but `local.yaml`
//!   missing, or `workflows/` moved but `agents/` not) → plan lists only
//!   the outstanding steps; apply completes them without touching
//!   already-migrated pieces.
//!
//! Reversal is deliberately not offered here. See the command's `--help`
//! for the git-revert-based rollback recipe.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use shelbi_core::{ConfigMode, Error, Project, Result};

use crate::{expand_tilde_str, project_dir, projects_dir};

/// Names of the config subdirectories that live under
/// `<config_root>/` and must move when a project migrates from global to
/// in-repo mode. Kept in one place so the plan builder and the tests
/// stay in lockstep.
pub const IN_REPO_CONFIG_DIRS: &[&str] = &["workflows", "agents"];

/// Config *files* (not directories) that live directly under
/// `<config_root>/` and must move to their in-repo counterpart on
/// migration. Today that's just the workspace-settings template.
pub const IN_REPO_CONFIG_FILES: &[&str] = &["workspace-settings.json.template"];

/// The `.gitignore` snippet the migration prints (and optionally
/// appends) at the repo root so the state pieces that Shelbi keeps
/// under `<repo>/.shelbi/` — should the user ever bind-mount / symlink
/// them into the repo — never accidentally land in a commit. The
/// literal content is deliberately verbose (one entry per state
/// footprint) so a reader who greps `.gitignore` for `shelbi` sees the
/// full list without having to know the layout.
pub const IN_REPO_GITIGNORE_SNIPPET: &str = "\
.shelbi/state.json
.shelbi/tasks/
.shelbi/HANDOFF.md
.shelbi/.claude/
.shelbi/workspaces/
.shelbi/events.log
.shelbi/local.yaml
";

/// A single filesystem mutation the migration will perform. Each variant
/// carries the source and destination path so callers (CLI dry-run,
/// tests) can render or replay the plan without re-deriving anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationAction {
    /// Write the shared half of the project YAML to `path`. The body
    /// already carries `config_mode: in-repo`.
    WriteSharedYaml { path: PathBuf, body: String },
    /// Write the user-local half of the project YAML to `path`.
    WriteLocalYaml { path: PathBuf, body: String },
    /// Move a config directory (`workflows/` or `agents/`) from
    /// `~/.shelbi/projects/<name>/…` to `<repo>/.shelbi/…`. The mover
    /// prefers `fs::rename`; on failure (cross-filesystem, EXDEV) it
    /// copies then removes.
    MoveConfigDir { src: PathBuf, dst: PathBuf },
    /// Move a top-level config file (currently just the workspace
    /// settings template) from the state dir to the in-repo dir.
    MoveConfigFile { src: PathBuf, dst: PathBuf },
    /// Delete the now-superseded global project YAML at
    /// `~/.shelbi/projects/<name>.yaml`. Only present in the plan when
    /// that file still exists.
    DeleteGlobalYaml { path: PathBuf },
}

/// The full migration recipe for one project — a rendered plan plus the
/// paths downstream code needs (repo root, `.gitignore`) so a caller can
/// display or apply it without re-loading anything.
///
/// `already_in_repo` distinguishes the "nothing to do" success case
/// (plan is empty because we're already there) from an unrelated empty
/// action list (which shouldn't happen but is handled gracefully). The
/// CLI uses it to render the right success message.
#[derive(Debug, Clone)]
pub struct MigrationPlan {
    /// Project name (as declared in the YAML).
    pub project_name: String,
    /// Fully-expanded path of the project's repo (after `~/` expansion).
    pub repo_root: PathBuf,
    /// `<repo>/.shelbi/`.
    pub in_repo_config_root: PathBuf,
    /// `~/.shelbi/projects/<name>/`.
    pub state_root: PathBuf,
    /// `<repo>/.shelbi/project.yaml`.
    pub shared_yaml_path: PathBuf,
    /// `~/.shelbi/projects/<name>/local.yaml`.
    pub local_yaml_path: PathBuf,
    /// `~/.shelbi/projects/<name>.yaml` — the file the migration
    /// deletes as its last step. Recorded even when it's already gone
    /// so callers can render the intended layout consistently.
    pub global_yaml_path: PathBuf,
    /// `<repo>/.gitignore`.
    pub gitignore_path: PathBuf,
    /// The `.gitignore` snippet the caller may print/append. Always
    /// set to [`IN_REPO_GITIGNORE_SNIPPET`] today — carried on the plan
    /// so a future customization has one place to grow.
    pub gitignore_snippet: &'static str,
    /// Ordered list of mutations. Empty when the project is already
    /// fully migrated.
    pub actions: Vec<MigrationAction>,
    /// `true` iff the project was already in in-repo mode when the
    /// plan was computed AND every expected in-repo file/dir was
    /// present. Empty `actions` with `already_in_repo == false` means a
    /// self-heal that happened to have nothing to do (shouldn't
    /// happen; the plan builder always adds *something* if any piece
    /// is missing).
    pub already_in_repo: bool,
}

impl MigrationPlan {
    /// Convenience predicate for the CLI's dry-run and success paths.
    pub fn is_noop(&self) -> bool {
        self.actions.is_empty()
    }
}

/// Compute the [`MigrationPlan`] for `project_name`.
///
/// The source project is resolved by trying, in order:
///
/// 1. `~/.shelbi/projects/<name>.yaml` (global mode — the pre-migration
///    layout). If found, we load it directly.
/// 2. `~/.shelbi/projects/<name>/local.yaml` (in-repo mode — the
///    post-migration layout). We read `repo:` out of `local.yaml`,
///    open `<repo>/.shelbi/project.yaml`, and reparse via the split
///    parser. If the shared half is missing, this is a half-migrated
///    state that the migration itself will heal (we synthesize the
///    plan against whatever remains).
/// 3. Neither present → [`Error::Other`] naming the project we couldn't
///    locate.
///
/// Once loaded, the plan is populated by comparing on-disk state
/// against the target in-repo layout. Anything already at its
/// destination is skipped so the migration is idempotent and safely
/// re-runnable after partial failures.
pub fn plan_in_repo_migration(project_name: &str) -> Result<MigrationPlan> {
    let projects_root = projects_dir()?;
    let state_root = project_dir(project_name)?;
    let global_yaml_path = projects_root.join(format!("{project_name}.yaml"));
    let local_yaml_path = state_root.join("local.yaml");

    // The Project we'll use to derive the shared/local YAML bodies. We
    // force `config_mode: InRepo` unconditionally — the migration is
    // one-way and the emitted YAML must reflect the new mode.
    let mut project =
        load_project_for_migration(project_name, &global_yaml_path, &local_yaml_path)?;
    let was_in_repo = matches!(project.config_mode, Some(ConfigMode::InRepo));
    project.config_mode = Some(ConfigMode::InRepo);

    let repo_root = expand_tilde_str(&project.repo);
    let in_repo_config_root = repo_root.join(".shelbi");
    let shared_yaml_path = in_repo_config_root.join("project.yaml");
    let gitignore_path = repo_root.join(".gitignore");

    let shared_body = project.to_shared_yaml_string()?;
    let local_body = project.to_local_yaml_string()?;

    let mut actions: Vec<MigrationAction> = Vec::new();

    if !shared_yaml_path.is_file() {
        actions.push(MigrationAction::WriteSharedYaml {
            path: shared_yaml_path.clone(),
            body: shared_body,
        });
    }
    if !local_yaml_path.is_file() {
        actions.push(MigrationAction::WriteLocalYaml {
            path: local_yaml_path.clone(),
            body: local_body,
        });
    }
    for dir in IN_REPO_CONFIG_DIRS {
        let src = state_root.join(dir);
        let dst = in_repo_config_root.join(dir);
        if src.is_dir() && !dst.exists() {
            actions.push(MigrationAction::MoveConfigDir { src, dst });
        }
    }
    for file in IN_REPO_CONFIG_FILES {
        let src = state_root.join(file);
        let dst = in_repo_config_root.join(file);
        if src.is_file() && !dst.exists() {
            actions.push(MigrationAction::MoveConfigFile { src, dst });
        }
    }
    if global_yaml_path.is_file() {
        actions.push(MigrationAction::DeleteGlobalYaml {
            path: global_yaml_path.clone(),
        });
    }

    // `already_in_repo` reports the *initial* state: the project's
    // config_mode was InRepo AND the plan is empty. If we set
    // config_mode ourselves above, that alone doesn't count.
    let already_in_repo = was_in_repo && actions.is_empty();

    Ok(MigrationPlan {
        project_name: project.name,
        repo_root,
        in_repo_config_root,
        state_root,
        shared_yaml_path,
        local_yaml_path,
        global_yaml_path,
        gitignore_path,
        gitignore_snippet: IN_REPO_GITIGNORE_SNIPPET,
        actions,
        already_in_repo,
    })
}

/// Execute each action in `plan.actions` in order. Returns the number
/// of actions that ran; an empty plan is a successful zero-action
/// application (the idempotent no-op path).
pub fn apply_migration_plan(plan: &MigrationPlan) -> Result<usize> {
    for action in &plan.actions {
        apply_action(action)?;
    }
    Ok(plan.actions.len())
}

fn apply_action(action: &MigrationAction) -> Result<()> {
    match action {
        MigrationAction::WriteSharedYaml { path, body }
        | MigrationAction::WriteLocalYaml { path, body } => write_yaml_file(path, body),
        MigrationAction::MoveConfigDir { src, dst } => move_dir(src, dst),
        MigrationAction::MoveConfigFile { src, dst } => move_file(src, dst),
        MigrationAction::DeleteGlobalYaml { path } => {
            match fs::remove_file(path) {
                Ok(()) => Ok(()),
                // Already gone — treat as success so a partial re-run
                // that already deleted it can still complete.
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(Error::Io(e)),
            }
        }
    }
}

fn write_yaml_file(path: &Path, body: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    fs::write(path, body).map_err(Error::Io)
}

/// Move a directory from `src` to `dst`. Tries `fs::rename` first, then
/// falls back to a recursive copy + `remove_dir_all` when rename fails
/// (typically EXDEV — the repo and shelbi state live on different
/// filesystems, common on Linux with `/home` and `/repos` mounts).
fn move_dir(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            copy_dir_recursive(src, dst)?;
            fs::remove_dir_all(src).map_err(Error::Io)?;
            Ok(())
        }
    }
}

fn move_file(src: &Path, dst: &Path) -> Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    match fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            fs::copy(src, dst).map_err(Error::Io)?;
            fs::remove_file(src).map_err(Error::Io)?;
            Ok(())
        }
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(Error::Io)?;
    for entry in fs::read_dir(src).map_err(Error::Io)? {
        let entry = entry.map_err(Error::Io)?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        let ty = entry.file_type().map_err(Error::Io)?;
        if ty.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else if ty.is_symlink() {
            let target = fs::read_link(&from).map_err(Error::Io)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&target, &to).map_err(Error::Io)?;
            #[cfg(windows)]
            {
                let _ = target;
                fs::copy(&from, &to).map_err(Error::Io)?;
            }
        } else {
            fs::copy(&from, &to).map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Load the project regardless of which on-disk layout is currently
/// present. See [`plan_in_repo_migration`] for the resolution order.
fn load_project_for_migration(
    project_name: &str,
    global_yaml_path: &Path,
    local_yaml_path: &Path,
) -> Result<Project> {
    if global_yaml_path.is_file() {
        let text = fs::read_to_string(global_yaml_path).map_err(Error::Io)?;
        return Project::from_yaml_str(&text);
    }
    if local_yaml_path.is_file() {
        let local_text = fs::read_to_string(local_yaml_path).map_err(Error::Io)?;
        let repo = extract_repo_from_local_yaml(&local_text, local_yaml_path)?;
        let shared_path = expand_tilde_str(&repo).join(".shelbi").join("project.yaml");
        if !shared_path.is_file() {
            return Err(Error::Other(format!(
                "project `{project_name}` has a local.yaml at {} pointing at repo `{repo}`, \
                 but no shared config at {} — cannot migrate from this half-broken state; \
                 restore either the shared YAML or the global YAML first",
                local_yaml_path.display(),
                shared_path.display(),
            )));
        }
        let shared_text = fs::read_to_string(&shared_path).map_err(Error::Io)?;
        return Project::from_split_yaml_str(&shared_text, &local_text);
    }
    Err(Error::Other(format!(
        "project `{project_name}` not found — no {} and no {}",
        global_yaml_path.display(),
        local_yaml_path.display(),
    )))
}

/// Deserialize just enough of `local.yaml` to recover the `repo:`
/// field, without pulling in the full split-parser (which insists on a
/// matching shared half). Serde skips unknown fields by default.
#[derive(Deserialize)]
struct LocalHeader {
    repo: String,
}

fn extract_repo_from_local_yaml(text: &str, path: &Path) -> Result<String> {
    let hdr: LocalHeader = serde_yaml::from_str(text).map_err(|e| {
        Error::Other(format!(
            "failed to read `repo:` from local.yaml at {}: {e}",
            path.display()
        ))
    })?;
    Ok(hdr.repo)
}

/// Detect whether `path`'s `.gitignore` already carries the migration
/// snippet. Comparison is line-wise (each non-blank snippet line must
/// appear as its own line in `.gitignore`) so a user who has already
/// added a subset (or interleaved the entries with other rules)
/// doesn't get duplicate entries appended.
///
/// A missing `.gitignore` counts as "does not contain the snippet" so
/// the migration prompts to create it.
pub fn gitignore_already_has_snippet(path: &Path, snippet: &str) -> Result<bool> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(Error::Io(e)),
    };
    let existing_lines: std::collections::HashSet<&str> = existing
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    Ok(snippet
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .all(|l| existing_lines.contains(l)))
}

/// Append `snippet` to `path` (creating the file if it doesn't exist),
/// with a leading blank line separator when the existing content
/// doesn't already end in one. Idempotent by way of a caller-side
/// [`gitignore_already_has_snippet`] check — this function unconditionally
/// appends, so the caller must guard.
pub fn append_gitignore_snippet(path: &Path, snippet: &str) -> Result<()> {
    let existing = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(Error::Io(e)),
    };
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    if !new_content.is_empty() {
        new_content.push('\n');
    }
    new_content.push_str(snippet);
    if !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(Error::Io)?;
    }
    fs::write(path, new_content).map_err(Error::Io)
}

// ---------------------------------------------------------------------------
// Tests

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use shelbi_core::{
        AgentRunnerSpec, ConfigMode, GitConfig, HeartbeatConfig, Machine, MachineKind,
        OrchestratorSpec, Project, ZenConfig,
    };

    use crate::save_project;
    use crate::test_lock::LOCK as TEST_LOCK;

    fn fresh_home_and_repo(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!(
            "shelbi-migrate-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        let home = base.join("home");
        let repo = base.join("repo");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&repo).unwrap();
        (home, repo)
    }

    fn fixture_project(name: &str, repo: &Path) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        Project {
            name: name.into(),
            repo: repo.to_string_lossy().into_owned(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
            }],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: Vec::new(),
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    /// Full plan for a fresh global-mode project: writes both YAMLs,
    /// moves `workflows/` + `agents/` + the template, and deletes the
    /// global YAML.
    #[test]
    fn plans_full_migration_for_fresh_global_project() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("plan-full");
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", &repo)).unwrap();
        let state = home.join("projects/myapp");
        fs::create_dir_all(state.join("workflows")).unwrap();
        fs::write(state.join("workflows/default.yaml"), "workflows: []\n").unwrap();
        fs::create_dir_all(state.join("agents/developer")).unwrap();
        fs::write(state.join("agents/developer/instructions.md"), "hi\n").unwrap();
        fs::write(state.join("workspace-settings.json.template"), "{}\n").unwrap();

        let plan = plan_in_repo_migration("myapp").unwrap();
        assert!(!plan.already_in_repo);
        assert!(!plan.actions.is_empty());
        assert_eq!(plan.repo_root, repo);
        assert_eq!(plan.in_repo_config_root, repo.join(".shelbi"));

        // Order matters: shared → local → dirs → file → delete global.
        let kinds: Vec<&str> = plan
            .actions
            .iter()
            .map(|a| match a {
                MigrationAction::WriteSharedYaml { .. } => "shared",
                MigrationAction::WriteLocalYaml { .. } => "local",
                MigrationAction::MoveConfigDir { .. } => "dir",
                MigrationAction::MoveConfigFile { .. } => "file",
                MigrationAction::DeleteGlobalYaml { .. } => "delete",
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["shared", "local", "dir", "dir", "file", "delete"]
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// Applying the plan lays down both YAMLs, moves the config
    /// dirs/files, and removes the global YAML. A round-trip re-plan
    /// then yields an empty action list — the idempotence guarantee.
    #[test]
    fn apply_migrates_and_second_run_is_noop() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("apply");
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", &repo)).unwrap();
        let state = home.join("projects/myapp");
        fs::create_dir_all(state.join("workflows")).unwrap();
        fs::write(state.join("workflows/default.yaml"), "workflows: []\n").unwrap();
        fs::create_dir_all(state.join("agents/developer")).unwrap();
        fs::write(state.join("agents/developer/instructions.md"), "hi\n").unwrap();

        let plan = plan_in_repo_migration("myapp").unwrap();
        apply_migration_plan(&plan).unwrap();

        // The end state: shared+local YAMLs exist, dirs are under the
        // repo, template is (absent here — none in the fixture), and the
        // global YAML is gone.
        assert!(repo.join(".shelbi/project.yaml").is_file());
        assert!(home.join("projects/myapp/local.yaml").is_file());
        assert!(repo.join(".shelbi/workflows/default.yaml").is_file());
        assert!(repo
            .join(".shelbi/agents/developer/instructions.md")
            .is_file());
        assert!(!state.join("workflows").exists());
        assert!(!state.join("agents").exists());
        assert!(!home.join("projects/myapp.yaml").exists());

        // The shared YAML must carry `config_mode: in-repo` — that's
        // what tells the loader to route through the in-repo layout on
        // the next open. The local YAML must NOT (the field belongs on
        // the shared side per SHARED_PROJECT_FIELDS).
        let shared = fs::read_to_string(repo.join(".shelbi/project.yaml")).unwrap();
        assert!(shared.contains("config_mode: in-repo"), "got: {shared}");
        let local = fs::read_to_string(home.join("projects/myapp/local.yaml")).unwrap();
        assert!(!local.contains("config_mode"), "got: {local}");

        // Re-planning against the migrated project → empty plan, and
        // `already_in_repo` is true (the loader saw the InRepo mode
        // from the shared half).
        let plan2 = plan_in_repo_migration("myapp").unwrap();
        assert!(plan2.actions.is_empty());
        assert!(plan2.already_in_repo);
        std::env::remove_var("SHELBI_HOME");
    }

    /// Half-migrated recovery: shared YAML written but `local.yaml`
    /// missing (e.g. the previous run crashed after step 3). Planning
    /// against this state completes only the outstanding steps.
    #[test]
    fn heals_half_migrated_state_missing_local_yaml() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("heal-local");
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", &repo)).unwrap();
        // Simulate: shared YAML already written to the repo, but the
        // rest of the migration never ran.
        fs::create_dir_all(repo.join(".shelbi")).unwrap();
        fs::write(
            repo.join(".shelbi/project.yaml"),
            "name: myapp\nconfig_mode: in-repo\ndefault_branch: main\n\
             orchestrator: {runner: claude}\nagent_runners: {claude: {command: claude, flags: []}}\n\
             workspace_poll_interval_secs: 5\nworkspace_permissions_mode: auto\n\
             zen: {}\nheartbeat: {}\ngit: {}\n",
        )
        .unwrap();

        let plan = plan_in_repo_migration("myapp").unwrap();
        // Shared write should NOT be in the plan (already exists);
        // local write and global delete should.
        let kinds: Vec<&str> = plan
            .actions
            .iter()
            .map(|a| match a {
                MigrationAction::WriteSharedYaml { .. } => "shared",
                MigrationAction::WriteLocalYaml { .. } => "local",
                MigrationAction::MoveConfigDir { .. } => "dir",
                MigrationAction::MoveConfigFile { .. } => "file",
                MigrationAction::DeleteGlobalYaml { .. } => "delete",
            })
            .collect();
        assert!(
            !kinds.contains(&"shared"),
            "unexpected shared write in {kinds:?}"
        );
        assert!(kinds.contains(&"local"), "missing local write in {kinds:?}");
        assert!(
            kinds.contains(&"delete"),
            "missing global delete in {kinds:?}"
        );

        apply_migration_plan(&plan).unwrap();
        assert!(home.join("projects/myapp/local.yaml").is_file());
        assert!(!home.join("projects/myapp.yaml").exists());
        std::env::remove_var("SHELBI_HOME");
    }

    /// Already in-repo (both YAMLs present, config dirs already moved,
    /// no global YAML) → empty plan, `already_in_repo = true`.
    #[test]
    fn noop_when_already_fully_in_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("noop");
        std::env::set_var("SHELBI_HOME", &home);

        // Start global, migrate once, then plan again.
        save_project(&fixture_project("myapp", &repo)).unwrap();
        let plan = plan_in_repo_migration("myapp").unwrap();
        apply_migration_plan(&plan).unwrap();

        let plan2 = plan_in_repo_migration("myapp").unwrap();
        assert!(plan2.actions.is_empty());
        assert!(plan2.already_in_repo);
        assert!(plan2.is_noop());
        std::env::remove_var("SHELBI_HOME");
    }

    /// Config dirs that *already* exist at the destination (e.g. the
    /// user hand-created `<repo>/.shelbi/workflows/` before running
    /// migrate) are left alone — we don't merge, we don't overwrite,
    /// we just skip. The source is also left in place; the user can
    /// reconcile.
    #[test]
    fn skips_dirs_already_present_at_destination() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("skip");
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", &repo)).unwrap();
        let state = home.join("projects/myapp");
        fs::create_dir_all(state.join("workflows")).unwrap();
        fs::write(state.join("workflows/from_state.yaml"), "src\n").unwrap();
        fs::create_dir_all(repo.join(".shelbi/workflows")).unwrap();
        fs::write(repo.join(".shelbi/workflows/from_repo.yaml"), "dst\n").unwrap();

        let plan = plan_in_repo_migration("myapp").unwrap();
        for a in &plan.actions {
            if let MigrationAction::MoveConfigDir { src, .. } = a {
                assert_ne!(
                    src,
                    &state.join("workflows"),
                    "workflows dir should have been skipped, not queued",
                );
            }
        }
        apply_migration_plan(&plan).unwrap();
        // Source workflows still exists (we didn't touch it), and the
        // destination `from_repo.yaml` was untouched.
        assert!(state.join("workflows/from_state.yaml").is_file());
        assert!(repo.join(".shelbi/workflows/from_repo.yaml").is_file());
        std::env::remove_var("SHELBI_HOME");
    }

    /// The generated shared half includes `config_mode: in-repo` even
    /// when the source project didn't (typical: fresh global project).
    #[test]
    fn shared_yaml_sets_config_mode_in_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("mode");
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", &repo)).unwrap();
        let plan = plan_in_repo_migration("myapp").unwrap();

        let shared_body = plan
            .actions
            .iter()
            .find_map(|a| match a {
                MigrationAction::WriteSharedYaml { body, .. } => Some(body.as_str()),
                _ => None,
            })
            .expect("plan should include a shared-yaml write");
        assert!(
            shared_body.contains("config_mode: in-repo"),
            "shared body missing config_mode: {shared_body}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// The `.gitignore` snippet detection is line-wise: a `.gitignore`
    /// missing even one snippet line reports false so the migration
    /// can complete it.
    #[test]
    fn gitignore_snippet_detection_is_line_wise() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-migrate-gitignore-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(&dir).unwrap();
        let gi = dir.join(".gitignore");

        // Missing file → false.
        assert!(!gitignore_already_has_snippet(&gi, IN_REPO_GITIGNORE_SNIPPET).unwrap());

        // Only some lines → false.
        fs::write(&gi, "target/\n.shelbi/state.json\n").unwrap();
        assert!(!gitignore_already_has_snippet(&gi, IN_REPO_GITIGNORE_SNIPPET).unwrap());

        // All lines present (order/interleave irrelevant) → true.
        let mut all = String::from("target/\nnode_modules/\n");
        all.push_str(IN_REPO_GITIGNORE_SNIPPET);
        fs::write(&gi, all).unwrap();
        assert!(gitignore_already_has_snippet(&gi, IN_REPO_GITIGNORE_SNIPPET).unwrap());
    }

    /// `append_gitignore_snippet` creates the file when missing and
    /// keeps the existing content otherwise, adding a blank-line
    /// separator so the snippet is visually distinct.
    #[test]
    fn append_gitignore_snippet_creates_or_extends() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-migrate-gi-append-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        fs::create_dir_all(&dir).unwrap();
        let gi = dir.join(".gitignore");

        // From nothing.
        append_gitignore_snippet(&gi, IN_REPO_GITIGNORE_SNIPPET).unwrap();
        let after = fs::read_to_string(&gi).unwrap();
        assert!(after.starts_with(".shelbi/state.json"));
        assert!(after.ends_with('\n'));

        // Existing content — separator + snippet appended.
        fs::write(&gi, "target/\nnode_modules/").unwrap();
        append_gitignore_snippet(&gi, IN_REPO_GITIGNORE_SNIPPET).unwrap();
        let after = fs::read_to_string(&gi).unwrap();
        assert!(after.starts_with("target/\nnode_modules/\n\n.shelbi/"));
    }

    /// Missing project → error naming both candidate paths so the user
    /// knows where the migrator looked.
    #[test]
    fn missing_project_errors_with_both_candidate_paths() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, _repo) = fresh_home_and_repo("missing");
        std::env::set_var("SHELBI_HOME", &home);

        let err = plan_in_repo_migration("nope").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nope.yaml"), "err missing global path: {msg}");
        assert!(msg.contains("local.yaml"), "err missing local path: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    /// If `local.yaml` is present but the shared half it references
    /// doesn't exist, we refuse to plan — this is a state we can't
    /// safely rebuild without the user's help.
    #[test]
    fn errors_when_local_yaml_points_at_missing_shared_yaml() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("orphan-local");
        std::env::set_var("SHELBI_HOME", &home);

        let state = home.join("projects/myapp");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("local.yaml"),
            format!("repo: {}\nmachines: []\n", repo.display()),
        )
        .unwrap();

        // Note: no `<repo>/.shelbi/project.yaml`, and no global YAML.
        let err = plan_in_repo_migration("myapp").unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("cannot migrate"), "unexpected err: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    /// End-to-end: after applying the plan, `<Project as ProjectPaths>`
    /// resolves the config half to `<repo>/.shelbi/…` — i.e. the
    /// in-repo layout is fully live.
    #[test]
    fn post_migration_project_paths_route_to_repo() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let (home, repo) = fresh_home_and_repo("paths");
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", &repo)).unwrap();
        apply_migration_plan(&plan_in_repo_migration("myapp").unwrap()).unwrap();

        // Reparse via the split reader — mirrors what a future loader
        // would do once it's mode-aware.
        let shared = fs::read_to_string(repo.join(".shelbi/project.yaml")).unwrap();
        let local = fs::read_to_string(home.join("projects/myapp/local.yaml")).unwrap();
        let project = Project::from_split_yaml_str(&shared, &local).unwrap();
        assert_eq!(project.config_mode, Some(ConfigMode::InRepo));

        use crate::ProjectPaths;
        assert_eq!(
            project.workflows_dir().unwrap(),
            repo.join(".shelbi/workflows"),
        );
        assert_eq!(project.agents_dir().unwrap(), repo.join(".shelbi/agents"),);
        // State stays under home regardless.
        assert_eq!(
            project.state_json_path().unwrap(),
            home.join("projects/myapp/state.json"),
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
