//! State IO: load/save projects, sessions, and per-agent markdown files.
//!
//! Agent files use YAML frontmatter (`---` fenced) with a free-form markdown
//! body. We don't depend on `gray_matter` to keep the dep tree small;
//! splitting the file at the second `---` is good enough for our format.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{
    default_project_statuses, default_workflow, validate_agent_id, validate_branch,
    validate_project_name, validate_task_id, Agent, Column, Project, Result, Session, Task,
};

mod agent_workspaces;
mod hub_config;
pub mod keymap;
mod migrate;
mod project_paths;
mod resolve;
mod root;
mod ssh_control;
mod user_config;
mod workspace_status;
mod workflows;

pub use migrate::{
    append_gitignore_snippet, apply_migration_plan, gitignore_already_has_snippet,
    plan_in_repo_migration, MigrationAction, MigrationPlan, IN_REPO_CONFIG_DIRS,
    IN_REPO_CONFIG_FILES, IN_REPO_GITIGNORE_SNIPPET,
};
pub use project_paths::ProjectPaths;
pub use root::{
    ensure_root_subdirs, expand_tilde_path, expand_tilde_str, resolve as resolve_root, root,
    set_root_override, RootSource, STANDARD_SUBDIRS,
};

pub use agent_workspaces::{
    agent_instructions_path, agent_settings_path, agent_shared_preamble_path, agent_skills_dir,
    agent_workspace_dir, compose_agent_prompt, count_agent_skills, default_agent_body,
    default_agent_settings, is_default_agent, legacy_claude_md_path, list_agents,
    load_agent_settings, load_shared_preamble, materialize_default_agents,
    maybe_emit_claude_md_migration_hint, orchestrator_handoff_path, reset_claude_md_migration_hint,
    self_heal_default_agents, take_orchestrator_handoff, AgentMaterializeOutcome, BundledAgent,
    BundledSkill, DEFAULT_AGENTS, DEFAULT_DEVELOPER_INSTRUCTIONS, DEFAULT_ORCHESTRATOR_INSTRUCTIONS,
    DEFAULT_REVIEW_INSTRUCTIONS, DEFAULT_REVIEW_LOAD_RUN_SKILL, DEVELOPER_AGENT, HANDOFF_FILE,
    ORCHESTRATOR_AGENT, ORCHESTRATOR_HANDOFF_REL, REVIEW_AGENT, SHARED_AGENT_DIR,
    SHARED_PREAMBLE_FILE,
};
pub use hub_config::{
    hub_config_path, list_projects, load_hub_config, save_hub_config, touch_project_launched,
    HubConfig, ProjectMeta, ProjectSummary,
};
pub use resolve::{
    cleanup_legacy_markers, project_roots, resolve_project_for_cwd, MarkerCleanup, ProjectRoot,
};
pub use ssh_control::{
    cleanup_stale_control_masters, daemon_pid_file_path, ensure_ssh_control_dir, is_process_alive,
    read_daemon_pid, read_daemon_pid_record, remote_hub_socket_path, remove_daemon_pid_file,
    reverse_forward_spec, ssh_control_dir, ssh_control_path_template, write_daemon_pid,
    CmCleanupOutcome, DaemonPidRecord,
};
pub use user_config::{
    load_user_config, save_user_config, user_config_path, Keymap, UserConfig, ZenToggleChord,
};
pub use workspace_status::{
    append_clarification_event, append_contextstore_event, append_dispatch_event,
    append_external_event, append_heartbeat_event, append_message_ack_event, append_message_event,
    append_project_event, append_rebase_event, append_task_event, append_workspace_dialog_event,
    append_workspace_event, append_workspace_pane_event, append_workspace_server_event,
    append_zen_dryrun_event, append_zen_mode_event, clear_expected_teardown, clear_server_record,
    consume_expected_teardown, emit_event_body, events_log_path, expected_teardown_marker_path,
    hub_socket_path, load_server_record, load_workspace_status, mark_expected_teardown,
    parse_pane_title_marker, parse_pane_title_state, save_server_record, save_workspace_status,
    server_record_path, server_teardown_key, workspace_status_path, workspaces_dir, PaneMarker,
    ServerPaneRecord, WorkspaceState, WorkspaceStatus, DAEMON_ACK, EXPECTED_TEARDOWN_MAX_AGE,
};
pub use workflows::{
    list_workflows, load_project_statuses, load_workflow, save_project_statuses, statuses_path,
    workflow_path, workflows_dir,
};

/// Default assistant name surfaced in the sidebar header and the
/// orchestrator system prompt when the user hasn't picked one yet.
pub const DEFAULT_ASSISTANT_NAME: &str = "Orchestrator";

/// Global shelbi config, persisted at `~/.shelbi/shelbi.yaml`. Created and
/// populated by the onboarding wizard; everything is `Option` so that
/// loading an older or partial file still succeeds.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ShelbiConfig {
    /// What the user wants to call their assistant — shown above the
    /// orchestrator pane and substituted into the orchestrator system
    /// prompt. `None` means the user hasn't been through Phase 1 of the
    /// wizard yet; callers should fall back to [`DEFAULT_ASSISTANT_NAME`]
    /// via [`ShelbiConfig::assistant_name`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assistant_name: Option<String>,
}

impl ShelbiConfig {
    /// The configured assistant name, or [`DEFAULT_ASSISTANT_NAME`] if
    /// the wizard hasn't set one yet.
    pub fn assistant_name(&self) -> &str {
        self.assistant_name
            .as_deref()
            .unwrap_or(DEFAULT_ASSISTANT_NAME)
    }
}

/// Path to the global config file: `$SHELBI_HOME/shelbi.yaml`.
pub fn shelbi_config_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("shelbi.yaml"))
}

/// Load the global config. Missing file → default (empty) config; that's
/// not an error because every consumer has a sensible fallback.
pub fn load_shelbi_config() -> Result<ShelbiConfig> {
    let path = shelbi_config_path()?;
    // Read unconditionally and map only NotFound to the default: probing
    // with `exists()` first would also report false on EACCES/ELOOP, and
    // a transiently unreadable config must surface as an error rather
    // than silently reading as "missing".
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(ShelbiConfig::default());
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    Ok(serde_yaml::from_str(&text)?)
}

/// Atomically write the global config.
pub fn save_shelbi_config(cfg: &ShelbiConfig) -> Result<()> {
    ensure_dir(&shelbi_home()?)?;
    let path = shelbi_config_path()?;
    atomic_write(&path, serde_yaml::to_string(cfg)?.as_bytes())
}

#[cfg(test)]
pub(crate) mod test_lock {
    //! Shared mutex for all tests that mutate the process-wide
    //! `SHELBI_HOME` env var. Per-module locks would race because the
    //! env var is global.
    use std::sync::Mutex;
    pub static LOCK: Mutex<()> = Mutex::new(());
}

/// Default contents of the per-project workspace settings template. Lives at
/// `~/.shelbi/projects/<name>/workspace-settings.json.template` after
/// `shelbi init --project <name>` runs. The `.template` suffix is retained
/// because user-authored templates may still contain
/// `{{workspace_permissions_mode}}` — see [`render_workspace_settings`]. The
/// embedded default no longer needs that placeholder: the workspace spawn path
/// passes `--permission-mode` directly on claude's command line, so the
/// rendered `settings.json` is purely the title-state hooks.
pub const DEFAULT_WORKSPACE_SETTINGS_TEMPLATE: &str =
    include_str!("default_workspace_settings.json.template");

/// Default shelbi root directory.
///
/// Resolves via [`root::resolve`] — see that function for the full
/// precedence chain. This name is kept for the many in-flight callers
/// that pre-date the `--root` flag; new code should call
/// [`root::root`] directly.
pub fn shelbi_home() -> Result<PathBuf> {
    root::root()
}

pub fn projects_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("projects"))
}

pub fn sessions_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("sessions"))
}

pub fn project_dir(project: &str) -> Result<PathBuf> {
    // Storage-layer chokepoint: every per-project path (tasks/, agents/,
    // workflows/, state.json) derives from here, so validating the name
    // once keeps a `../`-style project from escaping the projects dir.
    validate_project_name(project)?;
    Ok(projects_dir()?.join(project))
}

/// Reject a name that isn't exactly one *normal* path component, closing
/// `..` / absolute / separator traversal at the path chokepoints that
/// `join` a caller-influenced name. Mirrors
/// [`shelbi_core::validate_project_name`]'s security-critical invariant for
/// the workspace/agent chokepoints #137's hardening didn't cover
/// (state-runtime F14): a hostile or synced workspace/agent name could
/// otherwise escape `~/.shelbi/workspaces/` or `~/.shelbi/projects/<p>/agents/`.
pub(crate) fn ensure_flat_path_component(kind: &str, name: &str) -> Result<()> {
    use std::path::{Component, Path};
    let mut comps = Path::new(name).components();
    match (comps.next(), comps.next()) {
        (Some(Component::Normal(c)), None) if c.to_str() == Some(name) => Ok(()),
        _ => Err(shelbi_core::Error::Other(format!(
            "invalid {kind} name (must be a single path component — no `/`, `..`, \
             or absolute path): {name:?}"
        ))),
    }
}

/// Resolve the workspace settings template path for a project: the override
/// in [`Project::workspace_settings_template`] (with `~` expansion) if set,
/// otherwise the mode-aware default resolved through [`ProjectPaths`].
///
/// As a one-shot migration, if the legacy `workspace-settings.json` (no
/// `.template` suffix) exists in the project dir and the new path doesn't,
/// the legacy file is renamed in place — see [`migrate_workspace_settings_template`].
pub fn workspace_settings_template_path(project: &Project) -> Result<PathBuf> {
    let resolved = <Project as ProjectPaths>::workspace_settings_template_path(project)?;
    // Legacy-file migration only makes sense when we're pointing at the
    // per-project directory: if `workspace_settings_template` overrides
    // the path entirely, the caller owns the file.
    if project.workspace_settings_template.is_none() {
        if let Some(parent) = resolved.parent() {
            migrate_workspace_settings_template(parent);
        }
    }
    Ok(resolved)
}

/// One-shot rename of a legacy `workspace-settings.json` to the new
/// `.json.template` name, plus the older `worker-settings.json[.template]`
/// path that pre-dates the worker→workspace rename. Idempotent: skips
/// when the new file already exists or the legacy file is missing.
/// Best-effort — any IO error is swallowed so a permissions hiccup
/// doesn't break workspace deploy; the caller will fall back to
/// [`DEFAULT_WORKSPACE_SETTINGS_TEMPLATE`] just like any other
/// missing-template case.
fn migrate_workspace_settings_template(project_dir: &Path) {
    let renamed = project_dir.join("workspace-settings.json.template");
    if renamed.exists() {
        return;
    }
    // Try each legacy name in turn, newest first. First hit wins.
    for legacy_name in [
        "workspace-settings.json",
        "worker-settings.json.template",
        "worker-settings.json",
    ] {
        let legacy = project_dir.join(legacy_name);
        if legacy.exists() {
            let _ = fs::rename(&legacy, &renamed);
            return;
        }
    }
}

/// One-shot migration: write `workflows/default.yaml` into the project's
/// workflows directory if it's missing. Serializes the canonical
/// [`default_workflow`] from `shelbi-core` so existing projects pick up
/// an editable copy on their next load without forcing a manual step.
///
/// Idempotent — already-present files are left untouched (the user may
/// have edited them). Best-effort — any IO or serialization error is
/// swallowed so a permissions hiccup or full disk doesn't break opening
/// the project; the loader's in-memory fallback covers the file-missing
/// case until the next successful run.
///
/// Companion to [`migrate_default_statuses`] — the on-disk form is the
/// post-split reference-only shape (id + owner + optional agent), with
/// status identity (id, name, category) living in `statuses.yml`.
fn migrate_default_workflow(project_dir: &Path) {
    let path = project_dir.join("workflows").join("default.yaml");
    if path.exists() {
        return;
    }
    let Ok(yaml) = serde_yaml::to_string(&default_workflow()) else {
        return;
    };
    let _ = atomic_write(&path, yaml.as_bytes());
}

/// One-shot migration companion: write `workflows/statuses.yml` if
/// missing. The file is the project-wide source of truth for status
/// identity (id, name, category, ordering); workflow YAMLs reference
/// ids declared here and add per-workflow owner/agent.
///
/// Idempotent and best-effort, same semantics as
/// [`migrate_default_workflow`]. The workflow loader requires
/// `statuses.yml` to be present whenever workflow files exist — this
/// helper is what `shelbi init` / `shelbi reload` (via `load_project`)
/// call to satisfy that contract for existing projects that pre-date
/// the file.
fn migrate_default_statuses(project_dir: &Path) {
    let path = project_dir.join("workflows").join("statuses.yml");
    if path.exists() {
        return;
    }
    let Ok(yaml) = serde_yaml::to_string(&default_project_statuses()) else {
        return;
    };
    let _ = atomic_write(&path, yaml.as_bytes());
}

/// Render the workspace settings JSON for `project`: read the template file
/// resolved by [`workspace_settings_template_path`] (falling back to
/// [`DEFAULT_WORKSPACE_SETTINGS_TEMPLATE`] when the file is missing — a fresh
/// project that hasn't run `shelbi init --project` yet) and substitute
/// `{{workspace_permissions_mode}}` with `project.workspace_permissions_mode`.
///
/// The placeholder substitution is kept for backward compatibility with
/// user-authored templates that still reference it. The shipped default no
/// longer uses the placeholder: claude's permission mode is now passed on
/// the CLI by the workspace spawn path, which is authoritative and immune to
/// the settings.json races that motivated this change.
pub fn render_workspace_settings(project: &Project) -> Result<String> {
    let path = workspace_settings_template_path(project)?;
    let template = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            DEFAULT_WORKSPACE_SETTINGS_TEMPLATE.to_string()
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    Ok(template.replace("{{workspace_permissions_mode}}", &project.workspace_permissions_mode))
}

/// Outcome of [`self_heal_workspace_settings_template`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceSettingsTemplateOutcome {
    /// Project YAML points at a custom `workspace_settings_template` path —
    /// the shipped default is not in play, so the user owns that file
    /// and self-heal stays out of the way.
    SkippedOverride,
    /// Template was missing; the bundled default has been written.
    Created,
    /// On-disk template byte-matches the bundled default. Nothing changed.
    Unchanged,
    /// On-disk template diverged from the bundled default and has been
    /// overwritten. `had_legacy_placeholder` is set when the prior
    /// contents carried a `{{worker_*}}` placeholder — the broken state
    /// left by binaries from before the agents-workspaces rename, which
    /// would render an invalid `defaultMode` into `.claude/settings.json`
    /// and put claude into plan mode.
    Overwritten { had_legacy_placeholder: bool },
}

/// `shelbi reload`'s self-heal pass for the workspace-settings template.
/// The shipped default is a fixed asset the binary owns — claude's
/// permission mode is set via `--permission-mode` on the CLI, not via
/// the JSON, so there's no per-project knob the template needs to carry
/// and no reason for users to hand-edit the file at the default path.
/// When a stale template is found (notably the pre-rename
/// `{{worker_permissions_mode}}` placeholder that leaks through the
/// substituter and breaks claude's settings.json), we overwrite with
/// the shipped default.
///
/// Users who deliberately want a custom template can point
/// [`Project::workspace_settings_template`] at their own file — in that
/// case we return [`WorkspaceSettingsTemplateOutcome::SkippedOverride`]
/// without touching disk.
pub fn self_heal_workspace_settings_template(
    project: &Project,
) -> Result<WorkspaceSettingsTemplateOutcome> {
    if project.workspace_settings_template.is_some() {
        return Ok(WorkspaceSettingsTemplateOutcome::SkippedOverride);
    }
    let path = workspace_settings_template_path(project)?;
    if let Some(parent) = path.parent() {
        ensure_dir(parent)?;
    }
    match fs::read_to_string(&path) {
        Ok(s) if s == DEFAULT_WORKSPACE_SETTINGS_TEMPLATE => {
            Ok(WorkspaceSettingsTemplateOutcome::Unchanged)
        }
        Ok(s) => {
            let had_legacy_placeholder = s.contains("{{worker_");
            atomic_write(&path, DEFAULT_WORKSPACE_SETTINGS_TEMPLATE.as_bytes())?;
            Ok(WorkspaceSettingsTemplateOutcome::Overwritten {
                had_legacy_placeholder,
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            atomic_write(&path, DEFAULT_WORKSPACE_SETTINGS_TEMPLATE.as_bytes())?;
            Ok(WorkspaceSettingsTemplateOutcome::Created)
        }
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

pub fn agents_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("agents"))
}

pub fn tasks_dir(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("tasks"))
}

/// Path to a project's default workflow YAML
/// (`<workflows_dir>/default.yaml`). Auto-created on first load when
/// missing — see [`migrate_default_workflow`].
pub fn default_workflow_path(project: &str) -> Result<PathBuf> {
    Ok(workflows_dir(project)?.join("default.yaml"))
}

/// Ensure a directory exists.
pub fn ensure_dir(p: &Path) -> Result<()> {
    fs::create_dir_all(p)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Advisory file locking

/// RAII guard for an exclusive advisory `flock` on a lock file. The lock
/// is released when the guard drops (closing the descriptor releases the
/// flock), so holding a guard across a read-modify-write serializes that
/// whole sequence against every other process — and every other thread of
/// this process — that locks the same path.
#[must_use = "the lock is released as soon as the guard is dropped"]
pub(crate) struct FileLockGuard {
    _file: fs::File,
}

/// Block until an exclusive advisory lock on `lock_path` is acquired. The
/// lock file is created (empty) when missing; it carries no data — its only
/// job is to be a stable inode for `flock`. Each caller opens its own
/// descriptor, so two threads of one process exclude each other just like
/// two processes do.
pub(crate) fn acquire_file_lock(lock_path: &Path) -> Result<FileLockGuard> {
    if let Some(parent) = lock_path.parent() {
        ensure_dir(parent)?;
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc == 0 {
                break;
            }
            let err = std::io::Error::last_os_error();
            if err.kind() != std::io::ErrorKind::Interrupted {
                return Err(shelbi_core::Error::Io(err));
            }
        }
    }
    Ok(FileLockGuard { _file: file })
}

/// Public RAII handle for the per-workspace dispatch lock. Wraps a
/// [`FileLockGuard`] so callers outside this crate can serialize the
/// sync-worktree + spawn sequence without gaining access to the internal
/// lock primitive. Released when dropped.
#[must_use = "the workspace lock is released as soon as the guard is dropped"]
pub struct WorkspaceLock(#[allow(dead_code)] FileLockGuard);

/// Block until the exclusive per-workspace dispatch lock is held.
///
/// Two concurrent `task start`s targeting the same workspace would
/// otherwise interleave their sync-worktree / checkout / pane-recreate
/// steps and leave the pane running one branch while the worktree sits on
/// another. Holding this guard across the whole dispatch serializes them:
/// the second start blocks here until the first finishes and drops it.
/// The lock file is a sibling of the project's other locks under
/// `<project_dir>/`.
pub fn lock_workspace(project: &str, workspace: &str) -> Result<WorkspaceLock> {
    // `validate_project_name` runs inside `project_dir`; the workspace name
    // comes from trusted project config, but keep the lock file name flat by
    // rejecting a name that would introduce a path separator.
    if workspace.contains('/') || workspace.contains('\\') || workspace.is_empty() {
        return Err(shelbi_core::Error::Other(format!(
            "invalid workspace name for lock: {workspace:?}"
        )));
    }
    let path = project_dir(project)?.join(format!("workspace-{workspace}.lock"));
    Ok(WorkspaceLock(acquire_file_lock(&path)?))
}

/// Public RAII handle for the per-project dashboard-bootstrap lock. Wraps a
/// [`FileLockGuard`] so `shelbi-orchestrator` can serialize `ensure_dashboard`
/// without reaching the internal lock primitive. Released when dropped.
#[must_use = "the dashboard lock is released as soon as the guard is dropped"]
pub struct DashboardLock(#[allow(dead_code)] FileLockGuard);

/// Block until the exclusive per-project dashboard-bootstrap lock is held.
///
/// `ensure_dashboard` is a check-then-act sequence (count the dashboard's
/// panes, then split if there are fewer than two). Two callers racing it —
/// the `shelbi orchestrate` CLI and the TUI launcher firing at once, say —
/// would each observe a single-pane window and each perform the split,
/// producing a double-split or an orphaned orchestrator pane. Holding this
/// guard across the whole bootstrap serializes them: the loser blocks here
/// until the winner finishes and drops it, then finds the layout already
/// present and heals rather than re-creates. The lock file is a sibling of
/// the project's other locks under `<project_dir>/`.
pub fn lock_dashboard(project: &str) -> Result<DashboardLock> {
    let path = project_dir(project)?.join("dashboard.lock");
    Ok(DashboardLock(acquire_file_lock(&path)?))
}

/// Sibling lock-file path for `path` (`state.json` → `state.json.lock`).
/// The suffix is appended to the full file name — never `with_extension`,
/// which would collide `a.json` and `a.yaml` onto the same lock.
fn sibling_lock_path(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".lock");
    path.with_file_name(name)
}

// ---------------------------------------------------------------------------
// Project / Session YAML

/// Load a project's config, mode-aware.
///
/// * **Global mode** (the pre-split layout): a single flat YAML at
///   `~/.shelbi/projects/<name>.yaml`. Loaded directly.
/// * **In-repo mode** (post `migrate-to-in-repo`): the global YAML is
///   gone, so we fall back to the two-file split —
///   `~/.shelbi/projects/<name>/local.yaml` (the per-user half, which
///   carries `repo:`) plus `<repo>/.shelbi/project.yaml` (the committed
///   shared half) — merged via [`Project::from_split_yaml_str`]. This is
///   the loader half of the in-repo chain: the migration writes both
///   halves and retires the global YAML, so every runtime caller reaches
///   the migrated config through here without a mode flag.
pub fn load_project(project: &str) -> Result<Project> {
    let global_path = projects_dir()?.join(format!("{project}.yaml"));
    let mut p = match fs::read_to_string(&global_path) {
        Ok(text) => {
            warn_legacy_workers_key(project, &text);
            Project::from_yaml_str(&text)?
        }
        // No global YAML — this is either an in-repo project (migrated)
        // or a genuinely missing one. `load_project_split` disambiguates.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => load_project_split(project)?,
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    // Defense-in-depth: the project name is interpolated into shell command
    // strings (tmux pane loops, `--project` args) all over the orchestrator.
    // Creation already enforces a kebab/snake slug via `validate_agent_id`,
    // but a hand-edited YAML could smuggle in quotes/metacharacters. Reject
    // them here so a name like `x'; rm -rf ~; echo '` can never reach a
    // shell, independent of any single call site's escaping.
    shelbi_core::validate_agent_id(&p.name)?;
    p.validate_workspaces()?;
    let repo = p.repo.clone();
    p.detect_shapes(repo);
    // Best-effort: drop workflows/default.yaml and workflows/statuses.yml
    // into the project directory on first load. Idempotent — see
    // migrate_default_workflow / migrate_default_statuses. After this
    // runs, the workflow loader's "statuses.yml must exist" contract
    // holds for any subsequent open of this project.
    if let Ok(dir) = project_dir(&p.name) {
        migrate_default_workflow(&dir);
        migrate_default_statuses(&dir);
    }
    Ok(p)
}

/// In-repo fallback for [`load_project`]: read
/// `~/.shelbi/projects/<name>/local.yaml`, recover `repo:` from it, open
/// `<repo>/.shelbi/project.yaml`, and merge the two halves. Mirrors the
/// resolution `migrate.rs` performs, so a migrated project loads
/// identically to how it was written.
///
/// A missing `local.yaml` means the project simply doesn't exist (neither
/// layout is present) — reported with both candidate paths so the error
/// is actionable. A present `local.yaml` whose shared half is missing is a
/// half-broken state we surface distinctly rather than as a bare
/// file-not-found.
fn load_project_split(project: &str) -> Result<Project> {
    let global_path = projects_dir()?.join(format!("{project}.yaml"));
    let local_path = project_dir(project)?.join("local.yaml");
    let local_text = match fs::read_to_string(&local_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(shelbi_core::Error::Other(format!(
                "project `{project}` not found — no {} and no {}",
                global_path.display(),
                local_path.display(),
            )));
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let repo = migrate::extract_repo_from_local_yaml(&local_text, &local_path)?;
    let shared_path = expand_tilde_str(&repo).join(".shelbi").join("project.yaml");
    let shared_text = match fs::read_to_string(&shared_path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(shelbi_core::Error::Other(format!(
                "project `{project}` has a local.yaml at {} pointing at repo `{repo}`, \
                 but no shared config at {} — restore the repo's `.shelbi/project.yaml` \
                 (e.g. from git) to open it",
                local_path.display(),
                shared_path.display(),
            )));
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    Project::from_split_yaml_str(&shared_text, &local_text)
}

/// One-time-per-process nag when a legacy `workers:` top-level key is
/// observed in a project YAML. Detection is line-prefix only — we scan for
/// an unindented `workers:` to avoid tripping on nested keys or substrings.
///
/// Routed through `tracing::warn!` (not `eprintln!`) so the TUI subcommands
/// — which init tracing with a file writer at `~/.shelbi/logs/tui.log` —
/// don't paint the warning straight onto the alt-screen pane the sidebar /
/// tasks / review TUIs are drawing on. CLI invocations from a real shell
/// keep the default stderr writer, so the warning still surfaces there.
fn warn_legacy_workers_key(project: &str, yaml: &str) {
    use std::sync::Mutex;
    static WARNED: Mutex<Option<HashSet<String>>> = Mutex::new(None);

    let has_legacy = yaml.lines().any(|line| {
        let stripped = line.trim_end();
        stripped.starts_with("workers:")
            && (stripped.len() == "workers:".len()
                || stripped[("workers:".len())..]
                    .chars()
                    .next()
                    .map(|c| c.is_whitespace())
                    .unwrap_or(false))
    });
    if !has_legacy {
        return;
    }
    let mut guard = WARNED.lock().unwrap();
    let seen = guard.get_or_insert_with(HashSet::new);
    if seen.insert(project.to_string()) {
        tracing::warn!(
            project,
            "shelbi: project `{project}` uses the legacy `workers:` key; \
             rename it to `workspaces:` (the alias will be removed in a future release)"
        );
    }
}

pub fn save_project(p: &Project) -> Result<()> {
    ensure_dir(&projects_dir()?)?;
    let path = projects_dir()?.join(format!("{}.yaml", p.name));
    atomic_write(&path, serde_yaml::to_string(p)?.as_bytes())
}

pub fn load_session(name: &str) -> Result<Session> {
    let p = sessions_dir()?.join(format!("{name}.yaml"));
    let text = fs::read_to_string(&p)?;
    Ok(serde_yaml::from_str(&text)?)
}

pub fn save_session(s: &Session) -> Result<()> {
    ensure_dir(&sessions_dir()?)?;
    let path = sessions_dir()?.join(format!("{}.yaml", s.name));
    atomic_write(&path, serde_yaml::to_string(s)?.as_bytes())
}

// ---------------------------------------------------------------------------
// Per-project runtime state (state.json)

/// Per-project runtime state persisted at
/// `~/.shelbi/projects/<project>/state.json`. Tracks Zen Mode toggles and
/// the timestamp of the most recent Zen-Mode auto-promote crash so the
/// orchestrator can keep the user from re-arming a flapping pipeline.
/// ## Forward-compatibility contract
///
/// The `extra` catch-all (adversarial review F6) keeps an *older* binary
/// from destroying fields a *newer* binary wrote. Without it, every field
/// serde doesn't recognize is dropped on read, so any read-modify-write
/// mutator (`zen_heartbeat`, `set_workspace_filter`, …) run by an old
/// binary permanently deletes every field a newer schema added — the exact
/// mixed-version deployment `ZenModeState::deserialize_lenient` exists to
/// tolerate. `#[serde(flatten)]` routes unknown keys into `extra`, and they
/// serialize back out verbatim on the next write.
///
/// The contract is *field preservation, not field understanding*: an old
/// binary round-trips a new field untouched but never acts on it. Two
/// consequences of holding raw [`serde_json::Value`]s: `State` can no longer
/// derive `Eq` (JSON numbers are `f64`), only `PartialEq` — which is all the
/// `update_state` change-detection needs; and a new field's semantics must
/// stay back-compatible enough that a blind carry-through by an old binary
/// is safe (the same discipline `deserialize_lenient` already assumes).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    #[serde(default, deserialize_with = "ZenModeState::deserialize_lenient")]
    pub zen_mode: ZenModeState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zen_last_crashed_at: Option<DateTime<Utc>>,
    /// Persisted Kanban workspace filter — `None` means "All workspaces". The
    /// Tasks TUI restores this on each launch so the user's last view
    /// survives a respawn or project switch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_filter: Option<String>,
    /// Default-agent names whose `instructions.md` we've already
    /// observed to differ from the bundled template. `shelbi reload`'s
    /// self-heal path uses this to fire its "you've customized this
    /// agent's prompt" notice exactly once per divergence — re-aligning
    /// with the default clears the agent's entry so a future edit
    /// re-notifies.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub notified_diverged_agents: BTreeSet<String>,
    /// Per-orchestrator-session latch for the legacy `CLAUDE.md`
    /// migration hint. Set the first time
    /// [`maybe_emit_claude_md_migration_hint`] sees a leftover
    /// `~/.shelbi/projects/<project>/CLAUDE.md` and emits its stderr
    /// hint, so subsequent workspace dispatches inside the same session
    /// don't re-fire it. Cleared at orchestrator startup via
    /// [`reset_claude_md_migration_hint`] so a fresh session re-checks
    /// the disk. v2 drops the file entirely; v1 keeps it as a one-shot
    /// guidepost.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub claude_md_migration_hinted: bool,
    /// Explicit Kanban-column collapse overrides keyed by
    /// `"<workflow_name>:<status_id>"`. Only deviations from the
    /// auto-default are persisted — an empty / non-existent entry means
    /// the column is in `Auto` mode (empty columns render collapsed,
    /// non-empty render expanded). See
    /// [`shelbi_tui::kanban::ColumnExpansion`] for the in-memory model.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub kanban_column_overrides: BTreeMap<String, KanbanColumnOverride>,
    /// Forward-compat catch-all: any `state.json` key this binary doesn't
    /// recognize (a field a newer binary added) is captured here and written
    /// back verbatim, instead of being silently dropped on the next
    /// read-modify-write. See the struct-level forward-compatibility
    /// contract. Empty on the happy path — `#[serde(flatten)]` of an empty
    /// map contributes no keys, so a fresh `state.json` stays byte-identical
    /// to before this field existed.
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// Persisted form of an explicit user override on a Kanban column's
/// collapse state. Serialized as a lowercase string (`"collapsed"` /
/// `"expanded"`) so the on-disk shape stays human-readable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KanbanColumnOverride {
    Collapsed,
    Expanded,
}

/// Tri-state Zen Mode toggle persisted in `state.json::zen_mode`.
///
/// - [`ZenModeState::Off`] — orchestrator never auto-promotes; humans
///   review every merge.
/// - [`ZenModeState::Paused`] — no *new* auto-promotions, but tasks
///   already on the Zen track may still complete their merge.
/// - [`ZenModeState::On`] — orchestrator may run checks and auto-merge.
///
/// Serialized as a lowercase string (`"off"` / `"paused"` / `"on"`). The
/// custom `deserialize_lenient` adapter also accepts the legacy boolean
/// representation (`true`/`false`) so older `state.json` files keep
/// loading after the schema widens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ZenModeState {
    #[default]
    Off,
    Paused,
    On,
}

impl ZenModeState {
    pub fn as_str(self) -> &'static str {
        match self {
            ZenModeState::Off => "off",
            ZenModeState::Paused => "paused",
            ZenModeState::On => "on",
        }
    }

    /// Accept the new lowercase-string form *or* the legacy boolean form
    /// (`true` → On, `false` → Off). The bool branch only matters for
    /// existing on-disk files written before the tri-state landed.
    fn deserialize_lenient<'de, D>(d: D) -> std::result::Result<ZenModeState, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::{Error, Unexpected};
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Str(String),
            Bool(bool),
        }
        match Repr::deserialize(d)? {
            Repr::Bool(true) => Ok(ZenModeState::On),
            Repr::Bool(false) => Ok(ZenModeState::Off),
            Repr::Str(s) => match s.to_ascii_lowercase().as_str() {
                "off" => Ok(ZenModeState::Off),
                "paused" => Ok(ZenModeState::Paused),
                "on" => Ok(ZenModeState::On),
                other => Err(D::Error::invalid_value(
                    Unexpected::Str(other),
                    &"\"off\", \"paused\", or \"on\"",
                )),
            },
        }
    }
}

impl std::fmt::Display for ZenModeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Path to a project's `state.json`.
pub fn state_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("state.json"))
}

// ---------------------------------------------------------------------------
// Global runtime state (~/.shelbi/state.json)

/// Global cross-project runtime state at `~/.shelbi/state.json`. Tracks
/// preferences that follow the user across every project: the most
/// recent tmux palette binding (so the orchestrator can unbind it
/// cleanly on rebind / project switch), the one-shot acknowledgement
/// of the Zen Mode intro popover (so the explanation doesn't re-fire in
/// every project the user opens), and the sidebar's per-machine
/// collapse state (a UI preference that follows the user across
/// projects sharing a machine name).
/// See [`State`]'s forward-compatibility contract — `GlobalState` carries
/// the same `extra` catch-all for the same reason (an older binary must not
/// drop `~/.shelbi/state.json` fields a newer one wrote), and likewise drops
/// `Eq` for it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GlobalState {
    /// The exact tmux key string passed to `tmux bind-key -n …` on the
    /// most recent install (e.g. `C-p`, `M-z`). `None` means no shelbi
    /// session has installed a palette binding yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_palette_key: Option<String>,
    /// Set to `true` once the user has dismissed the Zen Mode intro
    /// popover with "Don't show this again" checked. The popover gates
    /// on this flag before rendering on every `off → on` toggle.
    /// Defaults to `false` (or absent on older `state.json` files) so a
    /// fresh install gets the explanation on the first enable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub zen_intro_seen: bool,
    /// Sidebar UI preferences — currently just the per-machine collapse
    /// state for the Workspaces tree. Skipped when default so a fresh
    /// state.json doesn't carry the empty `"sidebar":{}` block.
    #[serde(default, skip_serializing_if = "SidebarPrefs::is_default")]
    pub sidebar: SidebarPrefs,
    /// Forward-compat catch-all for unknown `~/.shelbi/state.json` keys —
    /// see [`State::extra`] and the forward-compatibility contract on
    /// [`State`].
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

/// User-level sidebar preferences persisted under
/// [`GlobalState::sidebar`]. Currently only carries the set of machine
/// names the user has collapsed in the Workspaces tree; a missing entry
/// means "expanded" (today's default). Machine names that don't exist in
/// the current project are silently ignored at render time — the entry
/// is left in place so re-adding the machine restores the prior state.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarPrefs {
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub collapsed_machines: BTreeSet<String>,
}

impl SidebarPrefs {
    pub fn is_default(&self) -> bool {
        self.collapsed_machines.is_empty()
    }
}

/// Flip the collapse state for `machine` in `~/.shelbi/state.json` and
/// return whether the machine is now collapsed. Reads the current
/// [`GlobalState`], mutates the set, and writes it back — the rest of
/// the file is preserved (no overwriting `tmux_palette_key` or
/// `zen_intro_seen`). Used by the sidebar's Space/Enter handler when
/// focus is on a `MachineGroup` row.
pub fn toggle_sidebar_machine_collapsed(machine: &str) -> Result<bool> {
    update_global_state(|state| {
        Ok(if state.sidebar.collapsed_machines.contains(machine) {
            state.sidebar.collapsed_machines.remove(machine);
            false
        } else {
            state.sidebar.collapsed_machines.insert(machine.to_string());
            true
        })
    })
}

/// Snapshot of the user's currently-collapsed machine names. The TUI
/// reads this once per refresh tick instead of doing a fresh disk read
/// per render. Missing file → empty set (today's default — every
/// machine expanded).
pub fn sidebar_collapsed_machines() -> Result<BTreeSet<String>> {
    Ok(read_global_state()?.sidebar.collapsed_machines)
}

/// Path to the global `state.json` (`~/.shelbi/state.json`).
pub fn global_state_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("state.json"))
}

/// Read `~/.shelbi/state.json`. Missing file → `GlobalState::default()`.
/// Only `NotFound` maps to the default — any other read error (EACCES,
/// ELOOP, …) propagates, so a transiently unreadable file can't be
/// mistaken for a missing one and clobbered by the next mutator write.
pub fn read_global_state() -> Result<GlobalState> {
    let path = global_state_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(GlobalState::default());
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    serde_json::from_str(&text)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))
}

/// Atomically write `state` to `~/.shelbi/state.json`, holding the state
/// lock for the duration of the write.
///
/// This is a full-snapshot, last-writer-wins write: any field another
/// process changed since `state` was read is silently reverted. Mutators
/// must go through [`update_global_state`] instead — reserve this for
/// writing a state you constructed from scratch (tests, seeding).
pub fn write_global_state(state: &GlobalState) -> Result<()> {
    let path = global_state_path()?;
    let _lock = acquire_file_lock(&sibling_lock_path(&path))?;
    write_global_state_to(&path, state)
}

/// Serialize and atomically write `state` at `path`. Callers must hold the
/// global-state lock.
fn write_global_state_to(path: &Path, state: &GlobalState) -> Result<()> {
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))?;
    atomic_write(path, &body)
}

/// Locked read-modify-write on `~/.shelbi/state.json`. Takes an exclusive
/// advisory lock on the sibling `state.json.lock`, reads the current
/// [`GlobalState`], applies `f`, and writes the result back — so two
/// concurrent mutators can never lose each other's field updates. The
/// write is skipped when `f` leaves the state unchanged (idempotent
/// callers stay no-ops), and an `Err` from `f` aborts without writing.
pub fn update_global_state<R>(f: impl FnOnce(&mut GlobalState) -> Result<R>) -> Result<R> {
    let path = global_state_path()?;
    let _lock = acquire_file_lock(&sibling_lock_path(&path))?;
    let mut state = read_global_state()?;
    let before = state.clone();
    let out = f(&mut state)?;
    if state != before {
        write_global_state_to(&path, &state)?;
    }
    Ok(out)
}

/// Mark the Zen Mode intro popover as acknowledged. Routed through
/// [`update_global_state`] so the other fields are preserved even against
/// concurrent writers. Idempotent — a no-op when already set.
pub fn mark_zen_intro_seen() -> Result<()> {
    update_global_state(|state| {
        state.zen_intro_seen = true;
        Ok(())
    })
}

#[cfg(test)]
mod global_state_tests {
    use super::*;
    use crate::test_lock::LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-global-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn missing_state_file_returns_default() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let s = read_global_state().unwrap();
        assert_eq!(s, GlobalState::default());
        assert!(s.tmux_palette_key.is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn unreadable_state_file_is_an_error_not_default() {
        // F13: only NotFound maps to the default. Any other read error
        // (here: EISDIR via a directory squatting on the path) must
        // propagate, or a transiently unreadable state.json would be
        // silently replaced with defaults by the next mutator write.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        fs::create_dir_all(global_state_path().unwrap()).unwrap();
        assert!(read_global_state().is_err());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn global_state_read_modify_write_preserves_unknown_fields() {
        // Forward-compat (adversarial review F6), global half: an older
        // binary toggling a field it knows about must not drop a field a
        // newer binary added to `~/.shelbi/state.json`.
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = global_state_path().unwrap();
        fs::write(
            &path,
            r#"{"tmux_palette_key":"M-z","telemetry_opt_in":false,"future":[1,2]}"#,
        )
        .unwrap();

        let state = read_global_state().unwrap();
        assert_eq!(state.tmux_palette_key.as_deref(), Some("M-z"));
        assert_eq!(
            state.extra.get("telemetry_opt_in"),
            Some(&serde_json::json!(false))
        );

        // Old binary flips a known field.
        mark_zen_intro_seen().unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(disk["zen_intro_seen"], serde_json::json!(true));
        assert_eq!(disk["tmux_palette_key"], serde_json::json!("M-z"));
        assert_eq!(disk["telemetry_opt_in"], serde_json::json!(false));
        assert_eq!(disk["future"], serde_json::json!([1, 2]));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn round_trips_tmux_palette_key() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut s = read_global_state().unwrap();
        s.tmux_palette_key = Some("M-z".into());
        write_global_state(&s).unwrap();
        let read_back = read_global_state().unwrap();
        assert_eq!(read_back.tmux_palette_key.as_deref(), Some("M-z"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_intro_seen_defaults_to_false_and_omits_when_unset() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let s = read_global_state().unwrap();
        assert!(!s.zen_intro_seen);
        // Empty (default) state must not carry a `zen_intro_seen` line —
        // the flag is opt-in for users who've actually dismissed the
        // popover, so default state.json files stay compact.
        write_global_state(&s).unwrap();
        let body = fs::read_to_string(global_state_path().unwrap()).unwrap();
        assert!(
            !body.contains("zen_intro_seen"),
            "default state must omit zen_intro_seen; got {body}",
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn mark_zen_intro_seen_sets_flag_and_is_idempotent() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        mark_zen_intro_seen().unwrap();
        assert!(read_global_state().unwrap().zen_intro_seen);
        // Second call is a no-op (still true, no panic).
        mark_zen_intro_seen().unwrap();
        assert!(read_global_state().unwrap().zen_intro_seen);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_intro_seen_round_trips_through_disk() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut s = read_global_state().unwrap();
        s.zen_intro_seen = true;
        write_global_state(&s).unwrap();
        let read_back = read_global_state().unwrap();
        assert!(read_back.zen_intro_seen);
        std::env::remove_var("SHELBI_HOME");
    }

    /// `toggle_sidebar_machine_collapsed` flips set membership in
    /// `~/.shelbi/state.json::sidebar.collapsed_machines` and returns
    /// the new state. A round-trip read sees the same set, so the
    /// sidebar collapse state survives a TUI respawn.
    #[test]
    fn sidebar_collapse_toggle_round_trips_through_state_file() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // First toggle inserts.
        assert!(toggle_sidebar_machine_collapsed("hub").unwrap());
        let collapsed = sidebar_collapsed_machines().unwrap();
        assert!(collapsed.contains("hub"));
        assert_eq!(collapsed.len(), 1);

        // Second toggle removes.
        assert!(!toggle_sidebar_machine_collapsed("hub").unwrap());
        assert!(sidebar_collapsed_machines().unwrap().is_empty());

        // Multiple machines can coexist.
        toggle_sidebar_machine_collapsed("hub").unwrap();
        toggle_sidebar_machine_collapsed("devbox").unwrap();
        let collapsed = sidebar_collapsed_machines().unwrap();
        assert!(collapsed.contains("hub"));
        assert!(collapsed.contains("devbox"));

        std::env::remove_var("SHELBI_HOME");
    }

    /// Toggling sidebar collapse must not clobber unrelated fields in
    /// `state.json`. A `tmux_palette_key` and `zen_intro_seen` set
    /// first must survive a subsequent collapse toggle.
    #[test]
    fn sidebar_collapse_toggle_preserves_other_global_fields() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut s = read_global_state().unwrap();
        s.tmux_palette_key = Some("M-z".into());
        s.zen_intro_seen = true;
        write_global_state(&s).unwrap();

        toggle_sidebar_machine_collapsed("hub").unwrap();
        let after = read_global_state().unwrap();
        assert_eq!(after.tmux_palette_key.as_deref(), Some("M-z"));
        assert!(after.zen_intro_seen);
        assert!(after.sidebar.collapsed_machines.contains("hub"));

        std::env::remove_var("SHELBI_HOME");
    }

    /// A fresh `state.json` (no `sidebar` key) loads without error and
    /// reports an empty set — the "today's default" path.
    #[test]
    fn sidebar_collapse_missing_field_loads_as_empty() {
        let _g = LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Hand-write a state.json that lacks the `sidebar` field so we
        // exercise the `#[serde(default)]` fallback rather than a clean
        // default-constructed state.
        fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("state.json"), r#"{"tmux_palette_key":"C-p"}"#).unwrap();
        let s = read_global_state().unwrap();
        assert!(s.sidebar.collapsed_machines.is_empty());
        assert_eq!(s.tmux_palette_key.as_deref(), Some("C-p"));
        std::env::remove_var("SHELBI_HOME");
    }
}

/// Window during which a `zen_last_crashed_at` heartbeat counts as a
/// recent-crash signal. Sized to catch a same-session crash without
/// sandbagging a fresh Zen run hours after an unrelated abort.
pub const ZEN_CRASH_RECOVERY_WINDOW_SECS: i64 = 3600;

/// Outcome of [`zen_check_crash_recovery`] — the start-of-orchestrator
/// crash detector. Returned (rather than logged inline) so the caller
/// can surface the warning where it makes sense for it: the orchestrator
/// pane writes a `zen=off reason=crash-recovery` line to `events.log`
/// and a tracing warning so it shows up in `~/.shelbi/logs/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZenCrashRecovery {
    /// No recent crash signal; nothing was changed.
    NoCrash,
    /// `zen_last_crashed_at` was within the recovery window AND
    /// `zen_mode == On`. The mode has been forced to `Off` on disk;
    /// the caller should emit the warning event + log line.
    AutoDisabled {
        crashed_at: DateTime<Utc>,
    },
}

/// Heartbeat tick — refresh `zen_last_crashed_at = now`. The intent is
/// "the orchestrator was alive at this moment"; if the pane subsequently
/// dies without [`zen_clear_crash`], the recent timestamp lets the next
/// [`zen_check_crash_recovery`] detect the crash. Writes only to
/// `state.json` — keeps the events log clean.
pub fn zen_heartbeat(project: &str) -> Result<()> {
    update_state(project, |state| {
        state.zen_last_crashed_at = Some(Utc::now());
        Ok(())
    })
}

/// Clear `zen_last_crashed_at`. Called from the orchestrator's graceful
/// exit path and from `quit_project` so a clean shutdown doesn't leave
/// a stale timestamp on disk that the next start would misread as a
/// crash. Idempotent — a no-op when nothing is set.
pub fn zen_clear_crash(project: &str) -> Result<()> {
    update_state(project, |state| {
        state.zen_last_crashed_at = None;
        Ok(())
    })
}

/// Run at orchestrator start. If `zen_last_crashed_at` is within the
/// recovery window AND `zen_mode == On`, force the mode to `Off` and
/// report `AutoDisabled`. Either way the stale timestamp is cleared so
/// the new heartbeat starts from a fresh state. The signal has been
/// consumed once read — calling this a second time on the same disk
/// state returns `NoCrash`.
pub fn zen_check_crash_recovery(project: &str) -> Result<ZenCrashRecovery> {
    update_state(project, |state| {
        let Some(crashed_at) = state.zen_last_crashed_at else {
            return Ok(ZenCrashRecovery::NoCrash);
        };
        let age = Utc::now() - crashed_at;
        let recent = age <= chrono::Duration::seconds(ZEN_CRASH_RECOVERY_WINDOW_SECS);
        let should_disable = recent && state.zen_mode == ZenModeState::On;
        state.zen_last_crashed_at = None;
        if should_disable {
            state.zen_mode = ZenModeState::Off;
            Ok(ZenCrashRecovery::AutoDisabled { crashed_at })
        } else {
            Ok(ZenCrashRecovery::NoCrash)
        }
    })
}

/// Read `state.json` for `project`. Returns `State::default()` when the
/// file is missing — the first call after creating a project shouldn't
/// require a separate seeding step. Only `NotFound` maps to the default;
/// other read errors (EACCES, ELOOP, …) propagate so a transiently
/// unreadable file isn't replaced with defaults by the next write.
pub fn read_state(project: &str) -> Result<State> {
    let path = state_path(project)?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(State::default());
        }
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    serde_json::from_str(&text)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))
}

/// Atomically write `state` to `~/.shelbi/projects/<project>/state.json`,
/// holding the state lock for the duration of the write.
///
/// This is a full-snapshot, last-writer-wins write: any field another
/// process changed since `state` was read is silently reverted. Mutators
/// must go through [`update_state`] instead — reserve this for writing a
/// state you constructed from scratch (tests, seeding).
pub fn write_state(project: &str, state: &State) -> Result<()> {
    let path = state_path(project)?;
    let _lock = acquire_file_lock(&sibling_lock_path(&path))?;
    write_state_to(&path, state)
}

/// Serialize and atomically write `state` at `path`. Callers must hold the
/// project's state lock.
fn write_state_to(path: &Path, state: &State) -> Result<()> {
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))?;
    atomic_write(path, &body)
}

/// Locked read-modify-write on a project's `state.json`. Takes an
/// exclusive advisory lock on the sibling `state.json.lock`, reads the
/// current [`State`], applies `f`, and writes the result back — so two
/// concurrent mutators (a heartbeat tick vs `shelbi zen off`, a filter
/// change vs a toggle, …) can never lose each other's field updates. The
/// write is skipped when `f` leaves the state unchanged (idempotent
/// callers stay no-ops), and an `Err` from `f` aborts without writing.
///
/// Every `State` mutator must route through here; an ad-hoc `read_state` →
/// mutate → `write_state` sequence reintroduces the lost-update race this
/// exists to prevent. `f` must not call another state mutator for the same
/// project — the flock is not reentrant and the nested acquire would
/// deadlock.
pub fn update_state<R>(project: &str, f: impl FnOnce(&mut State) -> Result<R>) -> Result<R> {
    let path = state_path(project)?;
    let _lock = acquire_file_lock(&sibling_lock_path(&path))?;
    let mut state = read_state(project)?;
    let before = state.clone();
    let out = f(&mut state)?;
    if state != before {
        write_state_to(&path, &state)?;
    }
    Ok(out)
}

/// Persist `target` as the project's `zen_mode` and append a
/// `project=<project> mode=zen <prev> -> <target> reason=<source>` line to
/// `events.log`. The `project=` scope keeps a toggle from broadcasting to
/// every project's orchestrator across the hub-global log.
/// Single source of truth shared by `shelbi zen on|off|pause` (CLI),
/// the TUI's Alt+Z handler, and the palette's toggle entry — each
/// caller picks its own `source` token (`user:cli`, `user:hotkey`,
/// `user:palette`) so the activity feed can distinguish them. Returns
/// the prior mode for callers that want to render a diff without a
/// follow-up read.
pub fn set_zen_mode(project: &str, target: ZenModeState, source: &str) -> Result<ZenModeState> {
    let prev = update_state(project, |state| {
        let prev = state.zen_mode;
        state.zen_mode = target;
        Ok(prev)
    })?;
    let _ = append_zen_mode_event(project, prev.as_str(), target.as_str(), source);
    Ok(prev)
}

/// Persist the Kanban workspace filter for `project`. `None` clears it
/// back to "All workspaces". Reads, mutates, and re-writes `state.json` so
/// the other fields (Zen mode, crash timestamp) are preserved. No event
/// log entry — view-state changes are noise in the activity feed.
pub fn set_workspace_filter(project: &str, filter: Option<&str>) -> Result<()> {
    update_state(project, |state| {
        state.workspace_filter = filter.map(|s| s.to_string());
        Ok(())
    })
}

/// Compose the persistence key for a Kanban column override. The key
/// combines `workflow_name` and `status_id` so a column override scoped
/// to one workflow's view never bleeds into another workflow that
/// happens to share the same status id.
pub fn kanban_column_override_key(workflow: &str, status_id: &str) -> String {
    format!("{workflow}:{status_id}")
}

/// Persist (or clear) an explicit collapse override for one Kanban
/// column. Passing `None` removes the entry, returning that column to
/// its auto default. Reads, mutates, and re-writes `state.json` so
/// unrelated fields stay intact. No event-log entry — view-state
/// changes are noise in the activity feed.
pub fn set_kanban_column_override(
    project: &str,
    workflow: &str,
    status_id: &str,
    override_state: Option<KanbanColumnOverride>,
) -> Result<()> {
    update_state(project, |state| {
        let key = kanban_column_override_key(workflow, status_id);
        match override_state {
            Some(v) => {
                state.kanban_column_overrides.insert(key, v);
            }
            None => {
                state.kanban_column_overrides.remove(&key);
            }
        }
        Ok(())
    })
}

/// Binary toggle with [`set_zen_mode`]'s semantics: On flips to Off,
/// anything else (Off, Paused) flips to On. Paused collapses to On here
/// because the toggle is intentionally a two-state hop — the CLI is still
/// the path that can land on Paused. Returns the new mode so callers can
/// update their cached state without a re-read.
///
/// The read and the flip happen inside one [`update_state`] critical
/// section (not read-then-`set_zen_mode`) so a concurrent mutator can't
/// slip between the two and get its write flipped on a stale snapshot.
pub fn toggle_zen_mode(project: &str, source: &str) -> Result<ZenModeState> {
    let (prev, target) = update_state(project, |state| {
        let prev = state.zen_mode;
        let target = match prev {
            ZenModeState::On => ZenModeState::Off,
            ZenModeState::Off | ZenModeState::Paused => ZenModeState::On,
        };
        state.zen_mode = target;
        Ok((prev, target))
    })?;
    let _ = append_zen_mode_event(project, prev.as_str(), target.as_str(), source);
    Ok(target)
}

// ---------------------------------------------------------------------------
// Agent markdown files

pub fn agent_path(project: &str, id: &str) -> Result<PathBuf> {
    validate_agent_id(id)?;
    Ok(agents_dir(project)?.join(format!("{id}.md")))
}

pub fn agent_log_path(project: &str, id: &str) -> Result<PathBuf> {
    validate_agent_id(id)?;
    Ok(agents_dir(project)?.join(format!("{id}.log.md")))
}

/// Write an agent file with YAML frontmatter + markdown body.
pub fn save_agent(project: &str, agent: &Agent, body_md: &str) -> Result<()> {
    ensure_dir(&agents_dir(project)?)?;
    let path = agent_path(project, &agent.id)?;
    write_frontmatter_file(&path, agent, body_md)
}

/// Render a `---\n<yaml>\n---\n<body>` file. Caller owns the path/dir.
fn write_frontmatter_file<T: serde::Serialize>(path: &Path, front: &T, body: &str) -> Result<()> {
    let yaml = serde_yaml::to_string(front)?;
    let mut buf = String::with_capacity(yaml.len() + body.len() + 32);
    buf.push_str("---\n");
    buf.push_str(&yaml);
    if !yaml.ends_with('\n') {
        buf.push('\n');
    }
    buf.push_str("---\n");
    buf.push_str(body);
    if !body.ends_with('\n') {
        buf.push('\n');
    }
    atomic_write(path, buf.as_bytes())
}

/// Parsed result of an agent file.
pub struct AgentFile {
    pub agent: Agent,
    pub body: String,
}

/// Read an agent file from disk and split frontmatter from body.
pub fn load_agent(project: &str, id: &str) -> Result<AgentFile> {
    let path = agent_path(project, id)?;
    let text = fs::read_to_string(&path)?;
    parse_agent_file(&text)
}

pub fn parse_agent_file(text: &str) -> Result<AgentFile> {
    let (front, body) = split_frontmatter(text)
        .ok_or_else(|| shelbi_core::Error::Other("missing frontmatter".into()))?;
    let agent: Agent = serde_yaml::from_str(front)?;
    Ok(AgentFile {
        agent,
        body: body.to_string(),
    })
}

/// Append a line to the agent's `.log.md`. Each line is timestamped.
pub fn append_log(project: &str, id: &str, line: &str) -> Result<()> {
    use std::fs::OpenOptions;
    ensure_dir(&agents_dir(project)?)?;
    let path = agent_log_path(project, id)?;
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    let ts = chrono::Utc::now().to_rfc3339();
    writeln!(f, "[{ts}] {line}")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Task markdown files

pub fn task_path(project: &str, id: &str) -> Result<PathBuf> {
    validate_task_id(id)?;
    Ok(tasks_dir(project)?.join(format!("{id}.md")))
}

pub struct TaskFile {
    pub task: Task,
    pub body: String,
}

/// Path of the per-project task lock: `<project_dir>/tasks.lock`, a
/// sibling of the `tasks/` directory (so `list_tasks`' `*.md` scan never
/// sees it). Serializes multi-file column mutations — move, reprioritize,
/// renumber, create — so two concurrent moves can't both compute the same
/// destination priority, and a renumber can't interleave with a save.
fn tasks_lock_path(project: &str) -> Result<PathBuf> {
    Ok(project_dir(project)?.join("tasks.lock"))
}

fn lock_tasks(project: &str) -> Result<FileLockGuard> {
    acquire_file_lock(&tasks_lock_path(project)?)
}

pub fn save_task(project: &str, task: &Task, body_md: &str) -> Result<()> {
    let _lock = lock_tasks(project)?;
    save_task_unlocked(project, task, body_md)
}

/// [`save_task`] body without the task lock — for callers that already
/// hold it (nested `flock` acquisition would deadlock).
fn save_task_unlocked(project: &str, task: &Task, body_md: &str) -> Result<()> {
    ensure_dir(&tasks_dir(project)?)?;
    let path = task_path(project, &task.id)?;
    // Gate the `branch:` override at the write chokepoint so a value with a
    // leading `-` (git flag injection) or shell metacharacters can never be
    // persisted and later handed to `git checkout` / `git worktree add` on a
    // (possibly remote) worker. `task_path` already validated the id above.
    if let Some(branch) = &task.branch {
        validate_branch(branch)?;
    }
    write_frontmatter_file(&path, task, body_md)
}

/// Create-exclusive variant of [`save_task`]: fails when a task with the
/// same id already exists instead of silently overwriting it. The
/// existence check and the write happen under the per-project task lock,
/// closing the check-then-save TOCTOU in id generation.
pub fn create_task(project: &str, task: &Task, body_md: &str) -> Result<()> {
    let _lock = lock_tasks(project)?;
    let path = task_path(project, &task.id)?;
    if path.exists() {
        return Err(shelbi_core::Error::Other(format!(
            "task `{}` already exists",
            task.id
        )));
    }
    save_task_unlocked(project, task, body_md)
}

pub fn load_task(project: &str, id: &str) -> Result<TaskFile> {
    let path = task_path(project, id)?;
    let text = fs::read_to_string(&path)?;
    let tf = parse_task_file(&text)?;
    // Reads address the file by its `<id>.md` name; writes address it by
    // the parsed frontmatter id (`save_task`). If a hand-edit lets those
    // diverge, a subsequent load→save (e.g. `move_task`) would write a
    // *second* file and fork the card onto the board. Reject the mismatch
    // loudly instead so the task can never split in two.
    if tf.task.id != id {
        return Err(shelbi_core::Error::TaskIdMismatch {
            requested: id.to_string(),
            found: tf.task.id.clone(),
        });
    }
    Ok(tf)
}

pub fn parse_task_file(text: &str) -> Result<TaskFile> {
    let (front, body) = split_frontmatter(text)
        .ok_or_else(|| shelbi_core::Error::Other("missing frontmatter".into()))?;
    let task: Task = serde_yaml::from_str(front)?;
    Ok(TaskFile {
        task,
        body: body.to_string(),
    })
}

pub fn delete_task(project: &str, id: &str) -> Result<()> {
    let path = task_path(project, id)?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// `(mtime, message)` cached per malformed task path. Lazily allocated
/// because `HashMap::new()` isn't `const`, so the outer `Mutex` wraps an
/// `Option` that's populated on first write.
type ParseWarnCache = HashMap<PathBuf, (Option<SystemTime>, String)>;

/// Per-file dedupe cache for the parse/read warnings emitted by
/// [`list_tasks`]. A warning fires only when the cached entry is absent or
/// its `(mtime, message)` differs from what we just observed — so a sidebar
/// that calls `list_tasks` on every refresh tick stops flooding stderr with
/// the same error.
///
/// Entries are removed when a file parses cleanly, when its mtime advances
/// (the user edited it and the warning fires fresh), and when the file
/// disappears from its tasks directory.
static PARSE_WARN_CACHE: Mutex<Option<ParseWarnCache>> = Mutex::new(None);

/// Returns `true` if `(path, mtime, msg)` differs from the cached entry —
/// the caller should emit. Updates the cache to the new tuple as a
/// side-effect so subsequent identical observations are suppressed.
fn should_warn_about_parse(path: &Path, mtime: Option<SystemTime>, msg: &str) -> bool {
    let mut guard = PARSE_WARN_CACHE.lock().unwrap();
    let cache = guard.get_or_insert_with(HashMap::new);
    if let Some((prev_mtime, prev_msg)) = cache.get(path) {
        if *prev_mtime == mtime && prev_msg == msg {
            return false;
        }
    }
    cache.insert(path.to_path_buf(), (mtime, msg.to_string()));
    true
}

/// Drop the cache entry for `path` — used after a successful parse so a
/// future regression on the same file is treated as a fresh warning.
fn forget_parse_warn(path: &Path) {
    if let Ok(mut guard) = PARSE_WARN_CACHE.lock() {
        if let Some(cache) = guard.as_mut() {
            cache.remove(path);
        }
    }
}

/// Prune cache entries inside `dir` whose path wasn't observed in the most
/// recent scan — covers the "file was deleted" recovery so a freshly
/// re-created file with the same error path emits cleanly. Other projects'
/// directories are left untouched.
fn prune_parse_warn(dir: &Path, seen: &HashSet<PathBuf>) {
    if let Ok(mut guard) = PARSE_WARN_CACHE.lock() {
        if let Some(cache) = guard.as_mut() {
            cache.retain(|p, _| p.parent() != Some(dir) || seen.contains(p));
        }
    }
}

/// Every task in the project. Order: column (in [`Column::ALL`] order) then
/// priority ASC. Files that fail to parse are skipped — each per-file
/// warning is deduped via [`PARSE_WARN_CACHE`] so a refresh loop that calls
/// `list_tasks` repeatedly only sees a malformed file's warning once per
/// unchanged state.
pub fn list_tasks(project: &str) -> Result<Vec<TaskFile>> {
    let dir = tasks_dir(project)?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        seen.insert(path.clone());
        let mtime = entry.metadata().and_then(|m| m.modified()).ok();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                let msg = format!(
                    "shelbi: skipping unreadable task file {}: {e}",
                    path.display()
                );
                if should_warn_about_parse(&path, mtime, &msg) {
                    eprintln!("{msg}");
                }
                continue;
            }
        };
        match parse_task_file(&text) {
            Ok(tf) => {
                // Parse succeeded, but the flattened `params` catch-all may
                // hold a misspelled optional field or a non-string extra that
                // silently did nothing. Surface those by name — never drop the
                // card (that would undo the forward-compat tolerance the raw
                // `serde_yaml::Value` params were built for). Deduped through
                // the same per-file cache so a refresh loop doesn't re-flood.
                let diags = tf.task.validate_params();
                if diags.is_empty() {
                    forget_parse_warn(&path);
                } else {
                    let joined = diags
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>()
                        .join("; ");
                    let msg = format!(
                        "shelbi: task file {} has param issue(s): {joined}",
                        path.display()
                    );
                    if should_warn_about_parse(&path, mtime, &msg) {
                        eprintln!("{msg}");
                    }
                }
                out.push(tf);
            }
            Err(e) => {
                let msg = format!(
                    "shelbi: skipping malformed task file {}: {e}",
                    path.display()
                );
                if should_warn_about_parse(&path, mtime, &msg) {
                    eprintln!("{msg}");
                }
            }
        }
    }
    prune_parse_warn(&dir, &seen);
    // The id tiebreak keeps board order deterministic when priorities
    // collide — without it, ties resolve by `read_dir` order, which the
    // filesystem is free to flap between scans.
    out.sort_by(|a, b| {
        let col_idx = |tf: &TaskFile| {
            Column::ALL
                .iter()
                .position(|c| *c == tf.task.column)
                .unwrap_or(0)
        };
        (col_idx(a), a.task.priority, a.task.id.as_str()).cmp(&(
            col_idx(b),
            b.task.priority,
            b.task.id.as_str(),
        ))
    });
    Ok(out)
}

/// Tasks in one column, sorted by priority.
pub fn list_column(project: &str, column: Column) -> Result<Vec<TaskFile>> {
    Ok(list_tasks(project)?
        .into_iter()
        .filter(|tf| tf.task.column == column)
        .collect())
}

/// Map of every task id in `project` to its current column. Used to derive
/// the [blocked](Task::is_blocked) state without reloading individual files.
pub fn task_columns(project: &str) -> Result<HashMap<String, Column>> {
    Ok(list_tasks(project)?
        .into_iter()
        .map(|tf| (tf.task.id, tf.task.column))
        .collect())
}

/// Tasks ready to start: in [`Column::Todo`], not blocked by any unfinished
/// dependency. Returned in priority order. Both views — the id→column map
/// and the Todo subset — derive from a single [`list_tasks`] pass, so a
/// concurrent task move can't make them disagree mid-call.
pub fn list_ready(project: &str) -> Result<Vec<TaskFile>> {
    let tasks = list_tasks(project)?;
    let columns: HashMap<String, Column> = tasks
        .iter()
        .map(|tf| (tf.task.id.clone(), tf.task.column))
        .collect();
    Ok(tasks
        .into_iter()
        .filter(|tf| tf.task.column == Column::Todo && !tf.task.is_blocked(&columns))
        .collect())
}

/// Count workspaces with no `active`-category (in-progress) task assigned to
/// them. A workspace is "idle" when nothing in [`Column::InProgress`] names it
/// in `assigned_to` — the same predicate the poller uses to decide a workspace
/// is free. Surfaced in the heartbeat payload so the orchestrator can tell, at
/// emit time, whether there's spare capacity to absorb eligible backlog work.
pub fn idle_workspace_count(project: &Project) -> Result<usize> {
    let in_progress = list_column(&project.name, Column::InProgress)?;
    Ok(idle_workspace_count_from(&project.workspaces, &in_progress))
}

/// Pure core of [`idle_workspace_count`]. Split out so unit tests can drive it
/// with in-memory fixtures without touching disk or `SHELBI_HOME`.
pub fn idle_workspace_count_from(
    workspaces: &[shelbi_core::WorkspaceSpec],
    in_progress: &[TaskFile],
) -> usize {
    let busy: HashSet<&str> = in_progress
        .iter()
        .filter_map(|tf| tf.task.assigned_to.as_deref())
        .collect();
    workspaces
        .iter()
        .filter(|w| !busy.contains(w.name.as_str()))
        .count()
}

/// Validate `depends_on` for a task that is about to be saved. Rejects:
///
/// 1. Self-references.
/// 2. IDs that don't exist in the project.
/// 3. Changes that would introduce a cycle.
///
/// `existing_tasks` should be the full task list before the save. The
/// candidate's own id is taken from `task.id` and excluded from existence
/// checks (allowing this fn to be used for both add and modification).
pub fn validate_depends_on(task: &Task, existing_tasks: &[TaskFile]) -> Result<()> {
    if task.depends_on.is_empty() {
        return Ok(());
    }

    // Self-reference rejection.
    if task.depends_on.iter().any(|d| d == &task.id) {
        return Err(shelbi_core::Error::DependencyCycle(format!(
            "{} → {}",
            task.id, task.id
        )));
    }

    // Existence check (every dep must already be a task, EXCEPT the candidate
    // itself — which may or may not be in `existing_tasks` depending on whether
    // this is an add or an edit).
    let known: HashSet<&str> = existing_tasks
        .iter()
        .map(|tf| tf.task.id.as_str())
        .collect();
    let missing: Vec<&str> = task
        .depends_on
        .iter()
        .filter(|d| d.as_str() != task.id && !known.contains(d.as_str()))
        .map(String::as_str)
        .collect();
    if !missing.is_empty() {
        return Err(shelbi_core::Error::UnknownDepends(missing.join(", ")));
    }

    // Cycle detection. Build the dep graph from `existing_tasks` with the
    // candidate's depends_on substituted in (so we catch cycles introduced
    // by this change, not just pre-existing ones). DFS from the candidate.
    let mut graph: HashMap<&str, &[String]> = existing_tasks
        .iter()
        .map(|tf| (tf.task.id.as_str(), tf.task.depends_on.as_slice()))
        .collect();
    graph.insert(task.id.as_str(), task.depends_on.as_slice());

    if let Some(chain) = find_cycle(&graph, task.id.as_str()) {
        return Err(shelbi_core::Error::DependencyCycle(chain.join(" → ")));
    }
    Ok(())
}

/// DFS that returns the offending chain (e.g. `["a", "b", "c", "a"]`) if a
/// cycle is reachable from `start`, otherwise `None`. Iterative to avoid
/// blowing the stack on pathological graphs.
fn find_cycle(graph: &HashMap<&str, &[String]>, start: &str) -> Option<Vec<String>> {
    // Visiting stack with an iteration index so we can backtrack on pop.
    let mut path: Vec<&str> = vec![start];
    let mut on_path: HashSet<&str> = [start].into_iter().collect();
    let mut visited: HashSet<&str> = HashSet::new();
    let mut iter_stack: Vec<usize> = vec![0];

    while let Some(&node) = path.last() {
        let i = *iter_stack.last().unwrap();
        let deps = graph.get(node).copied().unwrap_or(&[]);
        if i < deps.len() {
            let next = deps[i].as_str();
            *iter_stack.last_mut().unwrap() += 1;
            if on_path.contains(next) {
                // Found a cycle. Build the offending chain starting from
                // the first occurrence of `next` in the path.
                let pos = path.iter().position(|p| *p == next).unwrap();
                let mut chain: Vec<String> = path[pos..].iter().map(|s| s.to_string()).collect();
                chain.push(next.to_string());
                return Some(chain);
            }
            if visited.contains(next) {
                continue;
            }
            // Resolve to a stable key in the graph (so &str borrows survive).
            let key = graph
                .get_key_value(next)
                .map(|(k, _)| *k)
                .unwrap_or(next);
            path.push(key);
            on_path.insert(key);
            iter_stack.push(0);
        } else {
            on_path.remove(node);
            visited.insert(node);
            path.pop();
            iter_stack.pop();
        }
    }
    None
}

/// Move `id` to `new_column`. The task lands at the bottom (priority = N)
/// and the old column gets renumbered contiguous from 0.
///
/// Returns `Some((from, to, workflow))` when the move happened, or `None`
/// when the task was already in `new_column`. The workflow name is the
/// task's resolved workflow (`task.workflow_or_default()`) so callers can
/// hand it straight to [`append_task_event`] without re-reading the task
/// file just to fill the events log line.
pub fn move_task(
    project: &str,
    id: &str,
    new_column: Column,
) -> Result<Option<(Column, Column, String)>> {
    // The whole load → count → save → renumber sequence runs under the
    // task lock: two concurrent moves into one column would otherwise
    // both read the same `len()` and land on duplicate priorities.
    let _lock = lock_tasks(project)?;
    let TaskFile { mut task, body } = load_task(project, id)?;
    if task.column == new_column {
        return Ok(None);
    }
    let old_column = task.column;
    let workflow = task.workflow_or_default().to_string();
    let new_priority = list_column(project, new_column)?.len() as u32;
    task.column = new_column;
    task.priority = new_priority;
    task.updated_at = chrono::Utc::now();
    save_task_unlocked(project, &task, &body)?;
    renumber_column_unlocked(project, old_column)?;
    // Renumber the destination too — it repairs duplicate priorities or
    // gaps left behind by older builds (or a crash between the save and
    // the renumber) instead of letting them skew the column forever.
    renumber_column_unlocked(project, new_column)?;
    Ok(Some((old_column, new_column, workflow)))
}

/// Release task `id` back to [`Column::Todo`] and clear its `assigned_to`
/// in a **single** locked write.
///
/// The workspace-teardown path used to unassign (`save_task`) and then move
/// (`move_task`) as two separate writes; a crash between them left the card
/// unowned but still in `in_progress`, and because the recovery scan keys off
/// `assigned_to == <workspace>`, the now-unassigned card was skipped forever
/// (cli-daemon-board F18). Folding both mutations into one save closes that
/// window, and clearing the owner even when the card is already in Todo makes
/// the operation an idempotent recovery for a card wedged by the old path.
///
/// Returns the `(from, to, workflow)` triple (as [`move_task`] does) when the
/// column actually changed so the caller can append a move event, or `None`
/// when nothing needed moving.
pub fn release_task_to_todo(project: &str, id: &str) -> Result<Option<(Column, Column, String)>> {
    let _lock = lock_tasks(project)?;
    let TaskFile { mut task, body } = load_task(project, id)?;
    let old_column = task.column;
    let workflow = task.workflow_or_default().to_string();
    let already_todo = old_column == Column::Todo;
    if already_todo && task.assigned_to.is_none() {
        return Ok(None);
    }
    task.assigned_to = None;
    if !already_todo {
        task.column = Column::Todo;
        task.priority = list_column(project, Column::Todo)?.len() as u32;
    }
    task.updated_at = chrono::Utc::now();
    save_task_unlocked(project, &task, &body)?;
    if already_todo {
        // Only the owner was cleared — no column move to renumber or log.
        return Ok(None);
    }
    renumber_column_unlocked(project, old_column)?;
    renumber_column_unlocked(project, Column::Todo)?;
    Ok(Some((old_column, Column::Todo, workflow)))
}

/// Re-position `id` to slot `new_priority` within its current column. Other
/// tasks shift to keep the column contiguous from 0.
pub fn set_task_priority(project: &str, id: &str, new_priority: u32) -> Result<()> {
    let _lock = lock_tasks(project)?;
    let target = load_task(project, id)?;
    let column = target.task.column;
    let mut col = list_column(project, column)?;
    let from = col
        .iter()
        .position(|tf| tf.task.id == id)
        .ok_or_else(|| shelbi_core::Error::Other(format!("task `{id}` not in its own column?")))?;
    let to = (new_priority as usize).min(col.len().saturating_sub(1));
    if from == to {
        return Ok(());
    }
    let item = col.remove(from);
    col.insert(to, item);
    write_column_order(project, &col)
}

/// Persist only the `branch:` field on task `id`, under the per-project
/// task lock.
///
/// The lock spans the load→mutate→save so a concurrent writer touching a
/// *different* field on the same task can't be clobbered by a stale
/// whole-task write — the lost-update the caller's unlocked
/// `load_task` → mutate → `save_task` would otherwise cause. No-op (and no
/// mtime bump) when the branch already matches, so re-running a dispatch on
/// an already-branched task doesn't churn `updated_at`.
pub fn set_task_branch(project: &str, id: &str, branch: &str) -> Result<()> {
    let _lock = lock_tasks(project)?;
    let mut tf = load_task(project, id)?;
    if tf.task.branch.as_deref() == Some(branch) {
        return Ok(());
    }
    tf.task.branch = Some(branch.to_string());
    tf.task.updated_at = chrono::Utc::now();
    save_task_unlocked(project, &tf.task, &tf.body)
}

/// Stamp 0..N priorities onto the ordered slice and persist only the
/// tasks whose priority actually changed. Callers must hold the task
/// lock — the slice is a snapshot, and an unserialized concurrent writer
/// would be clobbered by these saves.
fn write_column_order(project: &str, ordered: &[TaskFile]) -> Result<()> {
    let now = chrono::Utc::now();
    for (i, tf) in ordered.iter().enumerate() {
        let want = i as u32;
        if tf.task.priority == want {
            continue;
        }
        let mut task = tf.task.clone();
        task.priority = want;
        task.updated_at = now;
        save_task_unlocked(project, &task, &tf.body)?;
    }
    Ok(())
}

/// Reload `column`'s tasks, sort by current priority, and renumber 0..N.
pub fn renumber_column(project: &str, column: Column) -> Result<()> {
    let _lock = lock_tasks(project)?;
    renumber_column_unlocked(project, column)
}

/// [`renumber_column`] body without the task lock — for callers that
/// already hold it.
fn renumber_column_unlocked(project: &str, column: Column) -> Result<()> {
    let col = list_column(project, column)?;
    write_column_order(project, &col)
}

// ---------------------------------------------------------------------------
// Helpers

/// Split a string on `^---\n` … `^---\n`. Returns (frontmatter, body).
fn split_frontmatter(s: &str) -> Option<(&str, &str)> {
    let rest = s.strip_prefix("---\n").or_else(|| s.strip_prefix("---\r\n"))?;
    // Find closing `---` on its own line.
    let mut search_from = 0usize;
    while let Some(idx) = rest[search_from..].find("\n---") {
        let abs = search_from + idx + 1; // points at the line starting "---"
        let after_dashes = abs + 3;
        let after_byte = rest.as_bytes().get(after_dashes).copied();
        if matches!(after_byte, Some(b'\n') | Some(b'\r') | None) {
            let front = &rest[..abs - 1]; // strip the trailing \n before the closing dashes
            // Skip the closing line and its terminator.
            let body_start = match after_byte {
                Some(b'\r') => after_dashes + 2, // \r\n
                Some(b'\n') => after_dashes + 1,
                None => rest.len(),
                _ => after_dashes,
            };
            let body = &rest[body_start.min(rest.len())..];
            return Some((front, body));
        }
        search_from = abs + 3;
    }
    None
}

/// Atomic write: write to a uniquely named temp file in the same dir,
/// then rename over `path`.
///
/// The temp suffix is appended to the full file name — never
/// `with_extension`, which replaces the final extension and collides
/// same-stem files (`a.json` and `a.yaml` would share one temp path). It
/// carries the pid AND a per-process counter so two threads of one
/// process writing the same target (TUI input thread + poller) can't
/// scribble on each other's temp file. A failed write or rename removes
/// its temp file instead of leaving litter.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);

    let dir = path
        .parent()
        .ok_or_else(|| shelbi_core::Error::Other(format!("no parent dir for {path:?}")))?;
    ensure_dir(dir)?;
    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| shelbi_core::Error::Other(format!("no file name in {path:?}")))?
        .to_os_string();
    tmp_name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let tmp = dir.join(tmp_name);
    let write_and_rename = || -> Result<()> {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        fs::rename(&tmp, path)?;
        Ok(())
    };
    let result = write_and_rename();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_project(name: &str, override_template: Option<PathBuf>) -> shelbi_core::Project {
        use shelbi_core::*;
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![], dialog_signatures: vec![] },
        );
        Project {
            name: name.into(),
            repo: "r".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: "/tmp".into(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: override_template,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            review: Default::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn workspace_settings_template_path_defaults_under_project_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let path = workspace_settings_template_path(&p).unwrap();
        assert_eq!(
            path,
            home.join("projects/myapp/workspace-settings.json.template")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_settings_template_path_renames_legacy_file_in_project_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let legacy = home.join("projects/myapp/workspace-settings.json");
        ensure_dir(legacy.parent().unwrap()).unwrap();
        std::fs::write(&legacy, r#"{"custom":true}"#).unwrap();
        let path = workspace_settings_template_path(&p).unwrap();
        assert!(!legacy.exists(), "legacy file should be renamed away");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            r#"{"custom":true}"#
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn workspace_settings_template_path_leaves_legacy_when_new_already_exists() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let dir = home.join("projects/myapp");
        ensure_dir(&dir).unwrap();
        let legacy = dir.join("workspace-settings.json");
        let renamed = dir.join("workspace-settings.json.template");
        std::fs::write(&legacy, "legacy").unwrap();
        std::fs::write(&renamed, "new").unwrap();
        let _ = workspace_settings_template_path(&p).unwrap();
        // Both files survive; we never overwrite the new one.
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "legacy");
        assert_eq!(std::fs::read_to_string(&renamed).unwrap(), "new");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_read_modify_write_preserves_unknown_fields() {
        // Forward-compat (adversarial review F6): an *older* binary doing a
        // read-modify-write on `state.json` must not destroy a field a
        // *newer* binary wrote. Here the newer field is `review_rounds`; the
        // old binary flips `workspace_filter` (a known field) and the
        // unknown field has to survive the rewrite.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let path = state_path("myapp").unwrap();
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            r#"{"zen_mode":"on","review_rounds":3,"future":{"nested":true}}"#,
        )
        .unwrap();

        // A known field is read back correctly, and the unknown ones are
        // parked in `extra`.
        let state = read_state("myapp").unwrap();
        assert_eq!(state.zen_mode, ZenModeState::On);
        assert_eq!(
            state.extra.get("review_rounds"),
            Some(&serde_json::json!(3))
        );

        // Old binary mutates a field it *does* know about.
        set_workspace_filter("myapp", Some("backend")).unwrap();

        // On disk: the mutation landed AND the unknown fields are intact.
        let raw = std::fs::read_to_string(&path).unwrap();
        let disk: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(disk["workspace_filter"], serde_json::json!("backend"));
        assert_eq!(disk["review_rounds"], serde_json::json!(3));
        assert_eq!(disk["future"], serde_json::json!({"nested": true}));
        assert_eq!(disk["zen_mode"], serde_json::json!("on"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn default_state_serializes_without_an_extra_block() {
        // The empty catch-all must contribute no keys — a fresh `state.json`
        // stays byte-compatible with the pre-`extra` schema.
        let json = serde_json::to_string(&State::default()).unwrap();
        assert_eq!(json, "{\"zen_mode\":\"off\"}");
    }

    #[test]
    fn workspace_settings_template_path_honors_override() {
        let p = fixture_project("myapp", Some(PathBuf::from("/etc/shelbi/p.json")));
        let path = workspace_settings_template_path(&p).unwrap();
        assert_eq!(path, PathBuf::from("/etc/shelbi/p.json"));
    }

    #[test]
    fn workspace_settings_template_path_expands_tilde_in_override() {
        let p = fixture_project("myapp", Some(PathBuf::from("~/custom/p.json")));
        let path = workspace_settings_template_path(&p).unwrap();
        let expected = dirs::home_dir().unwrap().join("custom/p.json");
        assert_eq!(path, expected);
    }

    #[test]
    fn render_workspace_settings_uses_embedded_default_when_file_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // No template at ~/.shelbi/projects/myapp/workspace-settings.json.template yet.
        let p = fixture_project("myapp", None);
        let rendered = render_workspace_settings(&p).unwrap();
        // The default no longer ships a permissions block — claude's
        // permission mode is passed on the CLI by the spawn path now. What
        // the default DOES ship is the title-state hooks the sidebar
        // poller depends on.
        assert!(!rendered.contains("\"permissions\""));
        assert!(!rendered.contains("{{workspace_permissions_mode}}"));
        assert!(rendered.contains("shelbi:idle"));
        assert!(rendered.contains("shelbi:working"));
        let _: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn render_workspace_settings_reads_project_template_when_present() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let tpl_path = workspace_settings_template_path(&p).unwrap();
        ensure_dir(tpl_path.parent().unwrap()).unwrap();
        std::fs::write(
            &tpl_path,
            r#"{"permissions":{"defaultMode":"{{workspace_permissions_mode}}"},"custom":true}"#,
        )
        .unwrap();
        let rendered = render_workspace_settings(&p).unwrap();
        assert!(rendered.contains("\"custom\":true"));
        assert!(rendered.contains("\"defaultMode\":\"auto\""));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn render_workspace_settings_substitutes_placeholder_in_custom_template() {
        // Backward compatibility: user-authored templates that still carry
        // the {{workspace_permissions_mode}} placeholder must continue to be
        // substituted, even though the shipped default no longer uses it.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut p = fixture_project("myapp", None);
        p.workspace_permissions_mode = "bypassPermissions".into();
        let tpl_path = workspace_settings_template_path(&p).unwrap();
        ensure_dir(tpl_path.parent().unwrap()).unwrap();
        std::fs::write(
            &tpl_path,
            r#"{"permissions":{"defaultMode":"{{workspace_permissions_mode}}"}}"#,
        )
        .unwrap();
        let rendered = render_workspace_settings(&p).unwrap();
        assert!(rendered.contains("\"defaultMode\":\"bypassPermissions\""));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn default_workspace_settings_template_contains_hooks_and_is_valid_json() {
        let s = DEFAULT_WORKSPACE_SETTINGS_TEMPLATE;
        // The default no longer carries the {{workspace_permissions_mode}}
        // placeholder — claude's mode is set on the CLI by the spawn path.
        assert!(!s.contains("{{workspace_permissions_mode}}"));
        assert!(!s.contains("defaultMode"));
        assert!(s.contains("Stop"));
        assert!(s.contains("Notification"));
        assert!(s.contains("UserPromptSubmit"));
        assert!(s.contains("PreToolUse"));
        assert!(s.contains("shelbi:idle"));
        assert!(s.contains("shelbi:blocked"));
        assert!(s.contains("shelbi:working"));
        // The default ships ready to use — no substitution required.
        let _: serde_json::Value =
            serde_json::from_str(s).expect("default template is valid JSON as-shipped");
    }

    /// The bug this self-heal exists for: the agents-workspaces rename
    /// renamed every `{{worker_*}}` placeholder to `{{workspace_*}}`. A
    /// pre-rename template binary materialized into a project still
    /// referenced `{{worker_permissions_mode}}`, which slipped through
    /// the substituter and left an invalid `"defaultMode"` in claude's
    /// settings.json. Verify the shipped default has no surviving
    /// `{{worker_*}}` strings.
    #[test]
    fn default_workspace_settings_template_has_no_legacy_worker_placeholder() {
        assert!(
            !DEFAULT_WORKSPACE_SETTINGS_TEMPLATE.contains("{{worker_"),
            "shipped template carries a pre-rename `{{{{worker_*}}}}` placeholder: {DEFAULT_WORKSPACE_SETTINGS_TEMPLATE}",
        );
    }

    /// Regression: the SessionStart hook must NOT silently exit 0 when
    /// `$TASK_ID` is unset — that was the bug that made `shelbi
    /// message` look successful while the worker had no tail running.
    /// The current shape:
    ///  - `mkdir -p .shelbi/messages` (unconditional, so the dir exists
    ///    even for sidebar-click panes with no assigned task).
    ///  - When TASK_ID is empty: append to
    ///    `.shelbi/messages/.no-task-id.log` and print a warning to
    ///    stderr, THEN exit 0 (so the pane still starts).
    ///
    /// This test runs the hook body with TASK_ID unset in a temp
    /// worktree and asserts both side-effects.
    #[test]
    fn session_start_hook_records_diagnostic_when_task_id_missing() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-sessionstart-hook-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        // Yank the SessionStart hook command out of the template so the
        // test exercises the actual shipped script — no drift between
        // what the template ships and what this test asserts.
        let v: serde_json::Value =
            serde_json::from_str(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE).unwrap();
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .expect("SessionStart hook command must be a string");

        // Run the hook with TASK_ID unset, cwd = temp worktree.
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env_remove("TASK_ID")
            .current_dir(&tmp)
            .output()
            .expect("run SessionStart hook");
        assert!(
            out.status.success(),
            "hook should exit 0 (soft failure): status={:?} stderr={}",
            out.status,
            String::from_utf8_lossy(&out.stderr),
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("TASK_ID unset"),
            "hook must emit a loud stderr warning when TASK_ID missing, got: {stderr:?}"
        );

        // The messages dir must have been created even without a task
        // (so downstream tools can inspect state without racing dir
        // creation), and the diagnostic log must contain a timestamped
        // line naming the failure mode.
        let diag = tmp.join(".shelbi/messages/.no-task-id.log");
        assert!(diag.exists(), "diagnostic log missing at {}", diag.display());
        let body = std::fs::read_to_string(&diag).unwrap();
        assert!(
            body.contains("no TASK_ID"),
            "diagnostic log missing marker: {body:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Positive path: when TASK_ID IS set, the hook creates the lock
    /// dir, starts a tail, and records its pid — exactly the state
    /// `shelbi message`'s delivery-verification probe checks for. This
    /// is the round-trip contract between the two sides of the file
    /// channel.
    #[test]
    fn session_start_hook_starts_tail_and_records_pid_when_task_id_set() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-sessionstart-happy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();

        let v: serde_json::Value =
            serde_json::from_str(DEFAULT_WORKSPACE_SETTINGS_TEMPLATE).unwrap();
        let cmd = v["hooks"]["SessionStart"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();

        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .env("TASK_ID", "feat-x")
            .current_dir(&tmp)
            .output()
            .expect("run SessionStart hook");
        assert!(out.status.success(), "hook must succeed: {:?}", out.status);

        // Verify the durable beacon exists (this is what `shelbi
        // message`'s tail_pid_alive probe reads).
        let pid_path = tmp.join(".shelbi/messages/feat-x.tail.d/pid");
        assert!(pid_path.exists(), "tail pid file missing at {}", pid_path.display());
        let pid_text = std::fs::read_to_string(&pid_path).unwrap();
        let pid: libc::pid_t = pid_text.trim().parse().unwrap();

        // Reap the tail we just spawned so it doesn't linger past the
        // test run. Best-effort — a slow-to-die child is not this
        // test's concern.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// Acceptance criterion (a): the shipped default flows through the
    /// substituter without leaving any unresolved `{{…}}` placeholders.
    /// Renders against every `workspace_permissions_mode` value that
    /// could plausibly be configured.
    #[test]
    fn render_workspace_settings_default_has_no_unresolved_placeholders() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for mode in ["auto", "acceptEdits", "bypassPermissions", "default", "plan"] {
            let mut p = fixture_project("myapp", None);
            p.workspace_permissions_mode = mode.into();
            let rendered = render_workspace_settings(&p).unwrap();
            assert!(
                !rendered.contains("{{"),
                "mode `{mode}` left an unresolved placeholder: {rendered}",
            );
            // The output must still parse as valid JSON for claude.
            let _: serde_json::Value = serde_json::from_str(&rendered).unwrap();
        }
        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload` self-heal — file missing → bundled default lands
    /// on disk; outcome reports `Created`.
    #[test]
    fn self_heal_workspace_settings_template_creates_when_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let path = workspace_settings_template_path(&p).unwrap();
        assert!(!path.exists());

        let outcome = self_heal_workspace_settings_template(&p).unwrap();
        assert_eq!(outcome, WorkspaceSettingsTemplateOutcome::Created);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            DEFAULT_WORKSPACE_SETTINGS_TEMPLATE
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// `shelbi reload` self-heal — on-disk template byte-matches the
    /// shipped default → outcome reports `Unchanged` and disk is not
    /// touched.
    #[test]
    fn self_heal_workspace_settings_template_is_noop_when_aligned() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let path = workspace_settings_template_path(&p).unwrap();
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, DEFAULT_WORKSPACE_SETTINGS_TEMPLATE).unwrap();

        let outcome = self_heal_workspace_settings_template(&p).unwrap();
        assert_eq!(outcome, WorkspaceSettingsTemplateOutcome::Unchanged);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            DEFAULT_WORKSPACE_SETTINGS_TEMPLATE
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// Acceptance criterion (c): the bug-specific case. A project whose
    /// on-disk template still has the pre-rename
    /// `{{worker_permissions_mode}}` placeholder is healed back to the
    /// shipped default, with `had_legacy_placeholder` set so reload can
    /// surface a "we fixed your stale template" line.
    #[test]
    fn self_heal_workspace_settings_template_overwrites_stale_worker_placeholder() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let path = workspace_settings_template_path(&p).unwrap();
        ensure_dir(path.parent().unwrap()).unwrap();
        // Mimic the broken on-disk template a pre-rename binary would
        // have materialized into the project.
        std::fs::write(
            &path,
            r#"{"permissions":{"defaultMode":"{{worker_permissions_mode}}"}}"#,
        )
        .unwrap();

        let outcome = self_heal_workspace_settings_template(&p).unwrap();
        assert_eq!(
            outcome,
            WorkspaceSettingsTemplateOutcome::Overwritten {
                had_legacy_placeholder: true
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            DEFAULT_WORKSPACE_SETTINGS_TEMPLATE
        );
        // And the renderer now produces a clean settings.json for claude.
        let rendered = render_workspace_settings(&p).unwrap();
        assert!(!rendered.contains("{{worker_"));
        assert!(!rendered.contains("{{workspace_"));
        std::env::remove_var("SHELBI_HOME");
    }

    /// Divergent on-disk content with no legacy placeholder is still
    /// overwritten by the shipped default — the template is a managed
    /// asset; users who want a custom one use the
    /// `workspace_settings_template` override field. The outcome
    /// reflects that no legacy placeholder was found.
    #[test]
    fn self_heal_workspace_settings_template_overwrites_divergent_content() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        let path = workspace_settings_template_path(&p).unwrap();
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"custom":true}"#).unwrap();

        let outcome = self_heal_workspace_settings_template(&p).unwrap();
        assert_eq!(
            outcome,
            WorkspaceSettingsTemplateOutcome::Overwritten {
                had_legacy_placeholder: false
            }
        );
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            DEFAULT_WORKSPACE_SETTINGS_TEMPLATE
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// Self-heal must respect the per-project
    /// `workspace_settings_template` override — that path belongs to the
    /// user, not the binary, and we never touch it on reload.
    #[test]
    fn self_heal_workspace_settings_template_skips_when_project_overrides_path() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let custom = home.join("etc/custom-settings.json.template");
        ensure_dir(custom.parent().unwrap()).unwrap();
        std::fs::write(&custom, r#"{"user":"owns this"}"#).unwrap();
        let p = fixture_project("myapp", Some(custom.clone()));

        let outcome = self_heal_workspace_settings_template(&p).unwrap();
        assert_eq!(outcome, WorkspaceSettingsTemplateOutcome::SkippedOverride);
        // User's custom file is untouched.
        assert_eq!(
            std::fs::read_to_string(&custom).unwrap(),
            r#"{"user":"owns this"}"#
        );
        // The default-path location was never created — we stayed out of
        // the way entirely.
        let default_path =
            home.join("projects/myapp/workspace-settings.json.template");
        assert!(!default_path.exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn frontmatter_split_basic() {
        let s = "---\nfoo: 1\nbar: 2\n---\nhello body\n";
        let (front, body) = split_frontmatter(s).unwrap();
        assert_eq!(front, "foo: 1\nbar: 2");
        assert_eq!(body, "hello body\n");
    }

    #[test]
    fn frontmatter_no_frontmatter_returns_none() {
        let s = "just a markdown file\n";
        assert!(split_frontmatter(s).is_none());
    }

    // ---- Storage tests ------------------------------------------------
    //
    // These exercise the on-disk task layout via $SHELBI_HOME. The env var
    // is process-wide so tests must serialize on it.

    use crate::test_lock::LOCK as TEST_LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-state-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_task(id: &str, column: Column, priority: u32) -> shelbi_core::Task {
        let now = chrono::Utc::now();
        shelbi_core::Task {
            id: id.to_string(),
            title: id.replace('-', " "),
            column,
            priority,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    fn workspace(name: &str) -> shelbi_core::WorkspaceSpec {
        shelbi_core::WorkspaceSpec {
            name: name.to_string(),
            machine: "local".to_string(),
            runner: "claude".to_string(),
            role: Default::default(),
        }
    }

    fn assigned(id: &str, to: &str) -> TaskFile {
        let mut task = make_task(id, Column::InProgress, 0);
        task.assigned_to = Some(to.to_string());
        TaskFile {
            task,
            body: String::new(),
        }
    }

    #[test]
    fn idle_workspace_count_excludes_only_assigned_workspaces() {
        // Four workspaces, two of which hold an active-category task. The
        // other two are idle. An InProgress task whose `assigned_to` names a
        // workspace not in the pool doesn't suppress any real workspace.
        let workspaces = [
            workspace("alpha"),
            workspace("bravo"),
            workspace("charlie"),
            workspace("delta"),
        ];
        let in_progress = [
            assigned("t1", "alpha"),
            assigned("t2", "charlie"),
            assigned("t3", "ghost"), // stale assignment, no such workspace
        ];
        assert_eq!(idle_workspace_count_from(&workspaces, &in_progress), 2);
    }

    #[test]
    fn idle_workspace_count_all_idle_when_nothing_in_progress() {
        let workspaces = [workspace("alpha"), workspace("bravo")];
        assert_eq!(idle_workspace_count_from(&workspaces, &[]), 2);
    }

    #[test]
    fn task_save_load_roundtrip() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let task = make_task("fix-login", Column::Todo, 3);
        save_task("proj", &task, "# Description\n").unwrap();
        let back = load_task("proj", "fix-login").unwrap();
        assert_eq!(back.task.id, "fix-login");
        assert_eq!(back.task.column, Column::Todo);
        assert_eq!(back.task.priority, 3);
        assert!(back.body.contains("Description"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn save_task_rejects_dangerous_branch_override() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A `branch:` that would inject a git flag or shell metacharacters
        // must be rejected at the write chokepoint — never persisted, so it
        // can never reach `git checkout` / `git worktree add` on a worker.
        let mut task = make_task("evil", Column::Todo, 0);
        task.branch = Some("--upload-pack=touch /tmp/pwn".into());
        assert!(matches!(
            save_task("proj", &task, ""),
            Err(shelbi_core::Error::InvalidBranch(_))
        ));
        // And it truly didn't land on disk.
        assert!(load_task("proj", "evil").is_err());

        // A well-formed namespaced branch still saves fine.
        let mut ok = make_task("good", Column::Todo, 0);
        ok.branch = Some("shelbi/good".into());
        save_task("proj", &ok, "").unwrap();
        assert_eq!(
            load_task("proj", "good").unwrap().task.branch.as_deref(),
            Some("shelbi/good")
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn move_task_renumbers_old_column() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for (i, id) in ["a", "b", "c"].iter().enumerate() {
            save_task("p", &make_task(id, Column::Todo, i as u32), "").unwrap();
        }
        move_task("p", "b", Column::InProgress).unwrap();

        let todo = list_column("p", Column::Todo).unwrap();
        let ids: Vec<_> = todo.iter().map(|tf| tf.task.id.as_str()).collect();
        let prios: Vec<_> = todo.iter().map(|tf| tf.task.priority).collect();
        assert_eq!(ids, vec!["a", "c"]);
        assert_eq!(prios, vec![0, 1]); // renumbered

        let wip = list_column("p", Column::InProgress).unwrap();
        assert_eq!(wip.len(), 1);
        assert_eq!(wip[0].task.id, "b");
        assert_eq!(wip[0].task.priority, 0);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn release_task_to_todo_clears_owner_and_moves_in_one_write() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let mut task = make_task("t", Column::InProgress, 0);
        task.assigned_to = Some("worker-1".to_string());
        save_task("p", &task, "").unwrap();

        let moved = release_task_to_todo("p", "t").unwrap();
        assert_eq!(moved, Some((Column::InProgress, Column::Todo, "default".into())));

        // Single resulting state: in Todo AND unowned. No window where one
        // mutation landed without the other.
        let after = load_task("p", "t").unwrap().task;
        assert_eq!(after.column, Column::Todo);
        assert_eq!(after.assigned_to, None);
        assert!(list_column("p", Column::InProgress).unwrap().is_empty());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn release_task_to_todo_is_idempotent_recovery() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A card already in Todo but still owned (the exact wedge the old
        // unassign-then-move split could leave behind): clear the owner with
        // no move event, and report `None` since the column didn't change.
        let mut task = make_task("t", Column::Todo, 0);
        task.assigned_to = Some("worker-1".to_string());
        save_task("p", &task, "").unwrap();

        assert_eq!(release_task_to_todo("p", "t").unwrap(), None);
        assert_eq!(load_task("p", "t").unwrap().task.assigned_to, None);

        // Fully clean (Todo + unowned) → genuine no-op.
        assert_eq!(release_task_to_todo("p", "t").unwrap(), None);

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn traversal_task_ids_are_rejected_and_touch_nothing_outside_tasks_dir() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A real task (so the tasks dir exists) plus a sentinel one level up
        // in the project dir — exactly what `../HANDOFF` would resolve onto.
        save_task("p", &make_task("keep", Column::Todo, 0), "").unwrap();
        let sentinel = project_dir("p").unwrap().join("HANDOFF.md");
        std::fs::write(&sentinel, "precious").unwrap();

        for bad in ["../HANDOFF", "../../other/tasks/foo", "a/b", "..", "x/../y"] {
            // The chokepoint itself refuses to build the path.
            assert!(
                matches!(task_path("p", bad), Err(shelbi_core::Error::InvalidAgentId(_))),
                "task_path should reject `{bad}`"
            );
            // Every read/move/delete handler inherits the rejection.
            assert!(load_task("p", bad).is_err(), "load_task should reject `{bad}`");
            assert!(delete_task("p", bad).is_err(), "delete_task should reject `{bad}`");
            assert!(
                move_task("p", bad, Column::InProgress).is_err(),
                "move_task should reject `{bad}`"
            );
        }

        // Nothing outside the tasks dir was read, moved, or removed.
        assert_eq!(std::fs::read_to_string(&sentinel).unwrap(), "precious");
        assert!(task_path("p", "keep").unwrap().exists());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn traversal_project_names_are_rejected_at_chokepoint() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for bad in ["../evil", "..", "a/b", ""] {
            assert!(
                matches!(project_dir(bad), Err(shelbi_core::Error::InvalidProjectName(_))),
                "project_dir should reject `{bad}`"
            );
            assert!(tasks_dir(bad).is_err(), "tasks_dir should reject `{bad}`");
            assert!(state_path(bad).is_err(), "state_path should reject `{bad}`");
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn frontmatter_id_mismatch_cannot_fork_into_two_cards() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Hand-edit the frontmatter id so it no longer matches the filename
        // (`fix-login.md`) — the F8 fork trigger.
        save_task("p", &make_task("fix-login", Column::Todo, 0), "").unwrap();
        let path = task_path("p", "fix-login").unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let forged = text.replace("id: fix-login", "id: fix-auth");
        assert!(forged.contains("id: fix-auth"), "sanity: id line was rewritten");
        std::fs::write(&path, &forged).unwrap();

        // Loading by the filename stem is rejected rather than silently
        // accepted, so a following save can't write a second `fix-auth.md`.
        assert!(matches!(
            load_task("p", "fix-login"),
            Err(shelbi_core::Error::TaskIdMismatch { .. })
        ));
        assert!(
            move_task("p", "fix-login", Column::InProgress).is_err(),
            "move must not fork the card"
        );

        // Exactly one task file remains — the board can't show two cards.
        let md_count = std::fs::read_dir(tasks_dir("p").unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("md"))
            .count();
        assert_eq!(md_count, 1, "no fork: still one task file on disk");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_priority_reorders_within_column() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for (i, id) in ["a", "b", "c", "d"].iter().enumerate() {
            save_task("p", &make_task(id, Column::Backlog, i as u32), "").unwrap();
        }
        // Move 'd' to slot 1 → expected order: a, d, b, c
        set_task_priority("p", "d", 1).unwrap();
        let col = list_column("p", Column::Backlog).unwrap();
        let ids: Vec<_> = col.iter().map(|tf| tf.task.id.as_str()).collect();
        assert_eq!(ids, vec!["a", "d", "b", "c"]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_task_branch_persists_only_the_branch_field() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("t", Column::Todo, 0), "# body\n").unwrap();

        set_task_branch("p", "t", "shelbi/t").unwrap();
        let back = load_task("p", "t").unwrap();
        assert_eq!(back.task.branch.as_deref(), Some("shelbi/t"));
        // Body and other fields survive the targeted write.
        assert_eq!(back.body, "# body\n");
        assert_eq!(back.task.column, Column::Todo);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_task_branch_is_a_noop_when_unchanged() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let mut task = make_task("t", Column::Todo, 0);
        task.branch = Some("shelbi/t".into());
        save_task("p", &task, "").unwrap();

        let before = std::fs::metadata(task_path("p", "t").unwrap())
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        set_task_branch("p", "t", "shelbi/t").unwrap();
        let after = std::fs::metadata(task_path("p", "t").unwrap())
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after, "no-op set-branch must not rewrite the file");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_task_branch_does_not_clobber_a_concurrent_field_change() {
        // Models the lost-update F6 fixes: another writer moved the task to
        // a new column *after* a caller loaded it. A targeted set-branch
        // must re-read under the lock and preserve that column rather than
        // writing back a stale whole-task snapshot.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("t", Column::Todo, 0), "").unwrap();

        // Concurrent writer flips the column.
        move_task("p", "t", Column::InProgress).unwrap();

        // Targeted set-branch (as if from a caller holding a pre-move read).
        set_task_branch("p", "t", "shelbi/t").unwrap();

        let back = load_task("p", "t").unwrap();
        assert_eq!(back.task.branch.as_deref(), Some("shelbi/t"));
        assert_eq!(
            back.task.column,
            Column::InProgress,
            "the concurrent column change must survive the set-branch"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn lock_dashboard_serializes_concurrent_holders() {
        // A lock-contention fixture for the `ensure_dashboard` race (F11):
        // two threads that each grab `lock_dashboard` for the same project
        // must never hold the guard at the same instant. We detect overlap
        // with a shared "in critical section" flag — if the flock didn't
        // serialize, both threads would observe it set.
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        use std::sync::Arc;

        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Materialize the project dir so both threads lock the same inode.
        std::fs::create_dir_all(project_dir("proj").unwrap()).unwrap();

        let in_cs = Arc::new(AtomicBool::new(false));
        let overlaps = Arc::new(AtomicUsize::new(0));
        let acquisitions = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = (0..2)
            .map(|_| {
                let in_cs = Arc::clone(&in_cs);
                let overlaps = Arc::clone(&overlaps);
                let acquisitions = Arc::clone(&acquisitions);
                std::thread::spawn(move || {
                    for _ in 0..20 {
                        let _guard = lock_dashboard("proj").unwrap();
                        acquisitions.fetch_add(1, Ordering::SeqCst);
                        // If another holder is already inside, the lock failed.
                        if in_cs.swap(true, Ordering::SeqCst) {
                            overlaps.fetch_add(1, Ordering::SeqCst);
                        }
                        // Widen the window so an unserialized race is caught.
                        std::thread::yield_now();
                        in_cs.store(false, Ordering::SeqCst);
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            overlaps.load(Ordering::SeqCst),
            0,
            "two lock_dashboard holders were inside the critical section at once"
        );
        assert_eq!(acquisitions.load(Ordering::SeqCst), 40);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn lock_workspace_rejects_names_with_separators() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert!(lock_workspace("p", "../evil").is_err());
        assert!(lock_workspace("p", "a/b").is_err());
        assert!(lock_workspace("p", "").is_err());
        // A well-formed name yields a guard (dropped immediately here).
        assert!(lock_workspace("p", "alice").is_ok());
        std::env::remove_var("SHELBI_HOME");
    }

    fn make_task_with_deps(
        id: &str,
        column: Column,
        priority: u32,
        deps: &[&str],
    ) -> shelbi_core::Task {
        let mut t = make_task(id, column, priority);
        t.depends_on = deps.iter().map(|s| s.to_string()).collect();
        t
    }

    #[test]
    fn validate_depends_on_rejects_missing_id() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("b", Column::Todo, 1, &["a", "ghost"]);
        let err = validate_depends_on(&candidate, &existing).unwrap_err();
        assert!(matches!(err, shelbi_core::Error::UnknownDepends(_)));
        assert!(err.to_string().contains("ghost"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_depends_on_accepts_valid_chain() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Done, 0), "").unwrap();
        save_task("p", &make_task_with_deps("b", Column::Todo, 0, &["a"]), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("c", Column::Todo, 1, &["b"]);
        validate_depends_on(&candidate, &existing).unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_tasks_surfaces_param_issues_without_dropping_the_card() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // A clean task and a hand-edited one carrying a numeric extra
        // (`retries: 2`). save_task creates the tasks dir; we then write the
        // suspect file's frontmatter directly, the way a human edit would.
        save_task("p", &make_task("clean", Column::Todo, 0), "").unwrap();
        let dir = tasks_dir("p").unwrap();
        let now = "2026-06-19T00:00:00Z";
        let suspect = format!(
            "---\nid: numeric\ntitle: Numeric\ncolumn: todo\npriority: 1\n\
             created_at: {now}\nupdated_at: {now}\nretries: 2\n---\n# Task\n"
        );
        std::fs::write(dir.join("numeric.md"), suspect).unwrap();

        // Both cards load — the numeric extra is a diagnostic, not a drop.
        let ids: Vec<String> = list_tasks("p")
            .unwrap()
            .into_iter()
            .map(|tf| tf.task.id)
            .collect();
        assert!(ids.contains(&"clean".to_string()));
        assert!(
            ids.contains(&"numeric".to_string()),
            "the numeric-extra card must still load; forward-compat, not a drop"
        );

        // And the offending field is nameable via the public lint.
        let tf = load_task("p", "numeric").unwrap();
        let diags = tf.task.validate_params();
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].field(), "retries");
        assert!(diags[0].is_error());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_depends_on_rejects_self_reference() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("a", Column::Todo, 0, &["a"]);
        let err = validate_depends_on(&candidate, &existing).unwrap_err();
        assert!(matches!(err, shelbi_core::Error::DependencyCycle(_)));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn validate_depends_on_detects_cycle() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Existing chain: c → b → a (c depends on b, b depends on a).
        // Closing the loop with a → c should be rejected.
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        save_task("p", &make_task_with_deps("b", Column::Todo, 1, &["a"]), "").unwrap();
        save_task("p", &make_task_with_deps("c", Column::Todo, 2, &["b"]), "").unwrap();
        let existing = list_tasks("p").unwrap();
        let candidate = make_task_with_deps("a", Column::Todo, 0, &["c"]);
        let err = validate_depends_on(&candidate, &existing).unwrap_err();
        match err {
            shelbi_core::Error::DependencyCycle(chain) => {
                // Chain should mention a, b, c.
                for id in ["a", "b", "c"] {
                    assert!(chain.contains(id), "chain={chain} missing {id}");
                }
            }
            other => panic!("expected cycle, got {other:?}"),
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_ready_filters_blocked_todos() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("done-a", Column::Done, 0), "").unwrap();
        save_task("p", &make_task("inprog-b", Column::InProgress, 0), "").unwrap();
        save_task(
            "p",
            &make_task_with_deps("free", Column::Todo, 0, &["done-a"]),
            "",
        )
        .unwrap();
        save_task(
            "p",
            &make_task_with_deps("blocked", Column::Todo, 1, &["inprog-b"]),
            "",
        )
        .unwrap();
        save_task("p", &make_task("no-deps", Column::Todo, 2), "").unwrap();
        let ready = list_ready("p").unwrap();
        let ids: Vec<_> = ready.iter().map(|tf| tf.task.id.as_str()).collect();
        assert_eq!(ids, vec!["free", "no-deps"]);
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- state.json --------------------------------------------------------

    #[test]
    fn state_defaults_when_file_is_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let s = read_state("p").unwrap();
        assert_eq!(s, State::default());
        assert_eq!(s.zen_mode, ZenModeState::Off);
        assert!(s.zen_last_crashed_at.is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_round_trips_through_disk() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let original = State {
            zen_mode: ZenModeState::On,
            zen_last_crashed_at: Some(
                "2026-06-19T12:34:56Z".parse::<DateTime<Utc>>().unwrap(),
            ),
            ..State::default()
        };
        write_state("p", &original).unwrap();
        let back = read_state("p").unwrap();
        assert_eq!(back, original);
        // Second round-trip is byte-stable.
        let first = std::fs::read(state_path("p").unwrap()).unwrap();
        write_state("p", &back).unwrap();
        let second = std::fs::read(state_path("p").unwrap()).unwrap();
        assert_eq!(first, second);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_omits_missing_crashed_at() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let s = State { zen_mode: ZenModeState::On, zen_last_crashed_at: None, ..State::default() };
        write_state("p", &s).unwrap();
        let on_disk = std::fs::read_to_string(state_path("p").unwrap()).unwrap();
        assert!(!on_disk.contains("zen_last_crashed_at"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_zen_mode_serializes_as_lowercase_string() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for (mode, expect) in [
            (ZenModeState::Off, "\"off\""),
            (ZenModeState::Paused, "\"paused\""),
            (ZenModeState::On, "\"on\""),
        ] {
            let s = State { zen_mode: mode, zen_last_crashed_at: None, ..State::default() };
            write_state("p", &s).unwrap();
            let on_disk = std::fs::read_to_string(state_path("p").unwrap()).unwrap();
            assert!(on_disk.contains(expect), "{mode:?} → {on_disk}");
            let back = read_state("p").unwrap();
            assert_eq!(back.zen_mode, mode);
        }
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- kanban column overrides -----------------------------------------

    #[test]
    fn set_kanban_column_override_round_trips_and_clears() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Setting an override persists under the composed
        // workflow:status_id key so two workflows with the same status
        // id stay distinct.
        set_kanban_column_override("p", "default", "in-progress", Some(KanbanColumnOverride::Collapsed))
            .unwrap();
        set_kanban_column_override("p", "design-review", "in-progress", Some(KanbanColumnOverride::Expanded))
            .unwrap();
        let s = read_state("p").unwrap();
        assert_eq!(
            s.kanban_column_overrides.get("default:in-progress").copied(),
            Some(KanbanColumnOverride::Collapsed)
        );
        assert_eq!(
            s.kanban_column_overrides.get("design-review:in-progress").copied(),
            Some(KanbanColumnOverride::Expanded)
        );

        // Clearing removes the entry without touching siblings.
        set_kanban_column_override("p", "default", "in-progress", None).unwrap();
        let s = read_state("p").unwrap();
        assert!(!s.kanban_column_overrides.contains_key("default:in-progress"));
        assert!(s.kanban_column_overrides.contains_key("design-review:in-progress"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_omits_empty_kanban_column_overrides() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_state("p", &State::default()).unwrap();
        let on_disk = std::fs::read_to_string(state_path("p").unwrap()).unwrap();
        assert!(
            !on_disk.contains("kanban_column_overrides"),
            "empty overrides map must be skipped; got {on_disk}",
        );
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- crash recovery ---------------------------------------------------

    #[test]
    fn zen_heartbeat_writes_timestamp_into_state() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        zen_heartbeat("p").unwrap();
        let s = read_state("p").unwrap();
        let ts = s.zen_last_crashed_at.expect("heartbeat should set the timestamp");
        // Newly written timestamp should be within the last few seconds.
        let age = (Utc::now() - ts).num_seconds().abs();
        assert!(age < 5, "heartbeat timestamp suspiciously old: {age}s");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn zen_clear_crash_removes_timestamp_idempotently() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        zen_heartbeat("p").unwrap();
        assert!(read_state("p").unwrap().zen_last_crashed_at.is_some());
        zen_clear_crash("p").unwrap();
        assert!(read_state("p").unwrap().zen_last_crashed_at.is_none());
        // Second call on an already-clean state is a no-op.
        zen_clear_crash("p").unwrap();
        assert!(read_state("p").unwrap().zen_last_crashed_at.is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn crash_recovery_reports_no_crash_when_timestamp_unset() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_state(
            "p",
            &State { zen_mode: ZenModeState::On, zen_last_crashed_at: None, ..State::default() },
        )
        .unwrap();
        assert_eq!(zen_check_crash_recovery("p").unwrap(), ZenCrashRecovery::NoCrash);
        // Mode untouched.
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::On);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn crash_recovery_auto_disables_when_recent_crash_and_zen_on() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let crashed_at = Utc::now() - chrono::Duration::minutes(5);
        write_state(
            "p",
            &State {
                zen_mode: ZenModeState::On,
                zen_last_crashed_at: Some(crashed_at),
                ..State::default()
            },
        )
        .unwrap();
        match zen_check_crash_recovery("p").unwrap() {
            ZenCrashRecovery::AutoDisabled { crashed_at: got } => {
                assert_eq!(got, crashed_at);
            }
            other => panic!("expected AutoDisabled, got {other:?}"),
        }
        let s = read_state("p").unwrap();
        assert_eq!(s.zen_mode, ZenModeState::Off);
        assert!(s.zen_last_crashed_at.is_none(), "signal must be cleared after consumption");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn crash_recovery_skips_when_timestamp_is_outside_window() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Two hours ago — outside the 1h window.
        let stale = Utc::now() - chrono::Duration::hours(2);
        write_state(
            "p",
            &State {
                zen_mode: ZenModeState::On,
                zen_last_crashed_at: Some(stale),
                ..State::default()
            },
        )
        .unwrap();
        assert_eq!(zen_check_crash_recovery("p").unwrap(), ZenCrashRecovery::NoCrash);
        let s = read_state("p").unwrap();
        // Mode left alone; stale timestamp cleared so the next heartbeat
        // starts fresh.
        assert_eq!(s.zen_mode, ZenModeState::On);
        assert!(s.zen_last_crashed_at.is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn crash_recovery_does_not_change_mode_when_zen_was_off() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let recent = Utc::now() - chrono::Duration::minutes(5);
        write_state(
            "p",
            &State {
                zen_mode: ZenModeState::Off,
                zen_last_crashed_at: Some(recent),
                ..State::default()
            },
        )
        .unwrap();
        // Recent crash but Zen wasn't on — nothing to disable, just
        // clean up the signal.
        assert_eq!(zen_check_crash_recovery("p").unwrap(), ZenCrashRecovery::NoCrash);
        let s = read_state("p").unwrap();
        assert_eq!(s.zen_mode, ZenModeState::Off);
        assert!(s.zen_last_crashed_at.is_none());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn crash_recovery_does_not_change_mode_when_zen_was_paused() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let recent = Utc::now() - chrono::Duration::minutes(5);
        write_state(
            "p",
            &State {
                zen_mode: ZenModeState::Paused,
                zen_last_crashed_at: Some(recent),
                ..State::default()
            },
        )
        .unwrap();
        assert_eq!(zen_check_crash_recovery("p").unwrap(), ZenCrashRecovery::NoCrash);
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::Paused);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn crash_recovery_is_idempotent_on_repeat_calls() {
        // Second call returns NoCrash even when the first auto-disabled,
        // because the signal was consumed (cleared) by the first call.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let recent = Utc::now() - chrono::Duration::minutes(5);
        write_state(
            "p",
            &State {
                zen_mode: ZenModeState::On,
                zen_last_crashed_at: Some(recent),
                ..State::default()
            },
        )
        .unwrap();
        assert!(matches!(
            zen_check_crash_recovery("p").unwrap(),
            ZenCrashRecovery::AutoDisabled { .. }
        ));
        assert_eq!(zen_check_crash_recovery("p").unwrap(), ZenCrashRecovery::NoCrash);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn state_accepts_legacy_bool_zen_mode() {
        // Older state.json files used a bare boolean for zen_mode. The
        // tri-state widening keeps deserialization tolerant of that form
        // so existing projects don't crash on first read after upgrade.
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = state_path("p").unwrap();
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"zen_mode": true}"#).unwrap();
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::On);
        std::fs::write(&path, r#"{"zen_mode": false}"#).unwrap();
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::Off);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn toggle_zen_mode_flips_state_and_logs_under_source() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // From defaults (Off) → On.
        let new = toggle_zen_mode("p", "user:palette").unwrap();
        assert_eq!(new, ZenModeState::On);
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::On);

        // Toggle again → Off.
        let new = toggle_zen_mode("p", "user:palette").unwrap();
        assert_eq!(new, ZenModeState::Off);

        // Both transitions appear in the events log, scoped to the project
        // and tagged with the supplied source.
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert!(
            log.contains("project=p mode=zen off -> on reason=user:palette"),
            "missing on transition in: {log}"
        );
        assert!(
            log.contains("project=p mode=zen on -> off reason=user:palette"),
            "missing off transition in: {log}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn toggling_zen_in_one_project_leaves_others_untouched() {
        // Regression: toggling Zen in project A must not change project B's
        // `zen_mode`, and the emitted event line must carry A's project scope
        // so a hub-global tail can tell the two apart (the broadcast bug).
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Both projects start from defaults (Off).
        assert_eq!(read_state("alpha").unwrap().zen_mode, ZenModeState::Off);
        assert_eq!(read_state("bravo").unwrap().zen_mode, ZenModeState::Off);

        // Flip alpha on via the palette path.
        let new = toggle_zen_mode("alpha", "user:palette").unwrap();
        assert_eq!(new, ZenModeState::On);

        // alpha changed; bravo did not.
        assert_eq!(read_state("alpha").unwrap().zen_mode, ZenModeState::On);
        assert_eq!(
            read_state("bravo").unwrap().zen_mode,
            ZenModeState::Off,
            "toggling alpha must not touch bravo's zen_mode"
        );

        // The event line is scoped to alpha, and no bravo-scoped line exists.
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert!(
            log.contains("project=alpha mode=zen off -> on reason=user:palette"),
            "missing alpha-scoped toggle in: {log}"
        );
        assert!(
            !log.contains("project=bravo mode=zen"),
            "no bravo-scoped zen line should exist; got: {log}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn toggle_zen_mode_from_paused_collapses_to_on() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_state(
            "p",
            &State { zen_mode: ZenModeState::Paused, zen_last_crashed_at: None, ..State::default() },
        )
        .unwrap();
        let new = toggle_zen_mode("p", "user:hotkey").unwrap();
        assert_eq!(new, ZenModeState::On);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn set_zen_mode_returns_prev_and_writes_event() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let prev = set_zen_mode("p", ZenModeState::Paused, "user:cli").unwrap();
        assert_eq!(prev, ZenModeState::Off);
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::Paused);
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert!(
            log.contains("project=p mode=zen off -> paused reason=user:cli"),
            "got: {log}"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_tasks_sorts_by_column_then_priority() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("z", Column::Done, 0), "").unwrap();
        save_task("p", &make_task("a", Column::Backlog, 1), "").unwrap();
        save_task("p", &make_task("b", Column::Backlog, 0), "").unwrap();
        save_task("p", &make_task("c", Column::InProgress, 0), "").unwrap();
        let all = list_tasks("p").unwrap();
        let ids: Vec<_> = all.iter().map(|tf| tf.task.id.as_str()).collect();
        // Column::ALL ordering: backlog, todo, in_progress, review, done
        assert_eq!(ids, vec!["b", "a", "c", "z"]);
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- list_tasks parse-warning dedupe ---------------------------------
    //
    // Regression: pre-fix, every refresh tick re-emitted "skipping malformed
    // task file …" for the same broken file, drowning the sidebar in
    // duplicates. The dedupe is keyed by (path, mtime, message) — these
    // tests pin the four state-transition rules listed in the bug.

    fn cached_parse_warn(path: &Path) -> Option<(Option<SystemTime>, String)> {
        PARSE_WARN_CACHE
            .lock()
            .unwrap()
            .as_ref()
            .and_then(|c| c.get(path).cloned())
    }

    #[test]
    fn should_warn_about_parse_suppresses_identical_observation() {
        let path = std::env::temp_dir().join(format!(
            "shelbi-dedupe-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mtime = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1));
        let msg = format!("shelbi: skipping malformed task file {}: missing field `created_at`", path.display());
        assert!(should_warn_about_parse(&path, mtime, &msg));
        assert!(!should_warn_about_parse(&path, mtime, &msg));
    }

    #[test]
    fn should_warn_about_parse_fires_when_mtime_advances() {
        let path = std::env::temp_dir().join(format!(
            "shelbi-dedupe-mtime-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let earlier = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1));
        let later = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2));
        let msg = "shelbi: skipping malformed task file: missing field `created_at`".to_string();
        assert!(should_warn_about_parse(&path, earlier, &msg));
        assert!(!should_warn_about_parse(&path, earlier, &msg));
        assert!(should_warn_about_parse(&path, later, &msg));
    }

    #[test]
    fn should_warn_about_parse_fires_when_error_signature_changes() {
        let path = std::env::temp_dir().join(format!(
            "shelbi-dedupe-msg-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mtime = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1));
        let missing_created = "shelbi: skipping malformed task file: missing field `created_at`".to_string();
        let missing_title = "shelbi: skipping malformed task file: missing field `title`".to_string();
        assert!(should_warn_about_parse(&path, mtime, &missing_created));
        assert!(should_warn_about_parse(&path, mtime, &missing_title));
        // Latest signature is now cached — same args suppress.
        assert!(!should_warn_about_parse(&path, mtime, &missing_title));
    }

    #[test]
    fn forget_parse_warn_lets_next_emit_through() {
        let path = std::env::temp_dir().join(format!(
            "shelbi-dedupe-forget-{}-{}.md",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mtime = Some(SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1));
        let msg = "shelbi: skipping malformed task file: missing field `created_at`".to_string();
        assert!(should_warn_about_parse(&path, mtime, &msg));
        forget_parse_warn(&path);
        // After forget, an identical observation emits again — covers the
        // "file was fixed, then broke again with the same error" recovery.
        assert!(should_warn_about_parse(&path, mtime, &msg));
    }

    #[test]
    fn list_tasks_caches_malformed_warning_across_repeat_calls() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = tasks_dir("p").unwrap().join("broken.md");
        ensure_dir(path.parent().unwrap()).unwrap();
        // Missing `created_at` — the bug-report exemplar.
        std::fs::write(
            &path,
            "---\nid: broken\ntitle: t\ncolumn: todo\npriority: 0\n---\nbody\n",
        )
        .unwrap();

        let _ = list_tasks("p").unwrap();
        let first = cached_parse_warn(&path).expect("cache entry after first scan");
        assert!(first.1.contains("skipping malformed task file"));

        // Second scan — same file, same mtime, same error: cache unchanged.
        let _ = list_tasks("p").unwrap();
        let second = cached_parse_warn(&path).expect("cache entry preserved");
        assert_eq!(first, second);

        // The dedupe predicate also reports "suppress" for this exact tuple.
        assert!(!should_warn_about_parse(&path, second.0, &second.1));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_tasks_clears_cache_when_file_becomes_valid() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = tasks_dir("p").unwrap().join("fixme.md");
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "---\nid: fixme\ntitle: t\ncolumn: todo\npriority: 0\n---\n")
            .unwrap();

        let _ = list_tasks("p").unwrap();
        assert!(
            cached_parse_warn(&path).is_some(),
            "broken file should populate cache"
        );

        // User fixes the file (a full valid frontmatter via save_task).
        save_task("p", &make_task("fixme", Column::Todo, 0), "body\n").unwrap();
        let _ = list_tasks("p").unwrap();
        assert!(
            cached_parse_warn(&path).is_none(),
            "valid file should drop its cache entry so a later regression emits cleanly"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn list_tasks_prunes_cache_when_file_deleted() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let path = tasks_dir("p").unwrap().join("ghost.md");
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "---\nid: ghost\n---\nbody\n").unwrap();

        let _ = list_tasks("p").unwrap();
        assert!(cached_parse_warn(&path).is_some());

        std::fs::remove_file(&path).unwrap();
        let _ = list_tasks("p").unwrap();
        assert!(
            cached_parse_warn(&path).is_none(),
            "deleted file should be pruned so a re-create emits fresh"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- workflows/default.yaml migration --------------------------------

    #[test]
    fn migrate_default_workflow_writes_yaml_when_directory_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = home.join("projects/myapp");
        migrate_default_workflow(&dir);
        migrate_default_statuses(&dir);
        let path = dir.join("workflows/default.yaml");
        assert!(path.exists(), "default.yaml should be created");
        // The on-disk form is post-migration (id + owner + agent only).
        // Resolve it against the freshly written statuses.yml before
        // comparing to the canonical default.
        let text = std::fs::read_to_string(&path).unwrap();
        let st_text =
            std::fs::read_to_string(dir.join("workflows/statuses.yml")).unwrap();
        let statuses = shelbi_core::ProjectStatuses::from_yaml_str(&st_text).unwrap();
        let parsed = shelbi_core::Workflow::from_yaml_str(&text)
            .expect("created default.yaml should round-trip through the workflow parser")
            .resolve_against(&statuses)
            .expect("resolved workflow validates against statuses.yml");
        assert_eq!(parsed, default_workflow());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migrate_default_statuses_writes_yaml_when_directory_missing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = home.join("projects/myapp");
        migrate_default_statuses(&dir);
        let path = dir.join("workflows/statuses.yml");
        assert!(path.exists(), "statuses.yml should be created");
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed = shelbi_core::ProjectStatuses::from_yaml_str(&text)
            .expect("statuses.yml should parse");
        assert_eq!(parsed, default_project_statuses());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migrate_default_statuses_is_noop_when_file_already_exists() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = home.join("projects/myapp");
        let path = dir.join("workflows/statuses.yml");
        ensure_dir(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "statuses: []\n").unwrap();
        migrate_default_statuses(&dir);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "statuses: []\n");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn migrate_default_workflow_is_noop_when_file_already_exists() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = home.join("projects/myapp");
        let path = dir.join("workflows/default.yaml");
        ensure_dir(path.parent().unwrap()).unwrap();
        // A user-edited file we must not stomp on.
        std::fs::write(&path, "name: custom\n").unwrap();
        migrate_default_workflow(&dir);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "name: custom\n");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_project_auto_creates_default_workflow_on_first_load() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        save_project(&p).unwrap();
        let workflow_path = default_workflow_path("myapp").unwrap();
        assert!(!workflow_path.exists(), "precondition: workflow file absent");
        let loaded = load_project("myapp").unwrap();
        assert_eq!(loaded.name, "myapp");
        assert!(
            workflow_path.exists(),
            "load_project should auto-create workflows/default.yaml"
        );
        // Second load is a no-op — content unchanged.
        let first_bytes = std::fs::read(&workflow_path).unwrap();
        let _ = load_project("myapp").unwrap();
        let second_bytes = std::fs::read(&workflow_path).unwrap();
        assert_eq!(first_bytes, second_bytes);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_project_preserves_user_edits_to_default_workflow() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let p = fixture_project("myapp", None);
        save_project(&p).unwrap();
        let workflow_path = default_workflow_path("myapp").unwrap();
        ensure_dir(workflow_path.parent().unwrap()).unwrap();
        let user_yaml = "name: default\nstatuses:\n  - { name: Inbox, category: backlog, owner: user }\n";
        std::fs::write(&workflow_path, user_yaml).unwrap();
        let _ = load_project("myapp").unwrap();
        assert_eq!(std::fs::read_to_string(&workflow_path).unwrap(), user_yaml);
        std::env::remove_var("SHELBI_HOME");
    }

    /// One-release back-compat: a project YAML that still uses the legacy
    /// `workers:` key parses into `Project::workspaces` via the serde alias.
    /// New projects emit `workspaces:`; this test guards the inbound path.
    #[test]
    fn load_project_accepts_legacy_workers_key_as_workspaces() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = home.join("projects");
        std::fs::create_dir_all(&dir).unwrap();
        let repo = home.join("legacy-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let yaml = format!(
            "name: legacy\n\
             repo: {}\n\
             default_branch: main\n\
             machines:\n\
             \x20\x20- {{ name: hub, kind: local, work_dir: {} }}\n\
             orchestrator: {{ runner: claude }}\n\
             agent_runners:\n\
             \x20\x20claude: {{ command: claude, flags: [] }}\n\
             workers:\n\
             \x20\x20- {{ name: alpha, machine: hub, runner: claude }}\n\
             \x20\x20- {{ name: bravo, machine: hub, runner: claude }}\n",
            repo.display(),
            repo.display(),
        );
        std::fs::write(dir.join("legacy.yaml"), yaml).unwrap();
        let loaded = load_project("legacy").unwrap();
        assert_eq!(loaded.workspaces.len(), 2);
        assert_eq!(loaded.workspaces[0].name, "alpha");
        assert_eq!(loaded.workspaces[1].name, "bravo");
        std::env::remove_var("SHELBI_HOME");
    }

    /// New projects: `workspaces:` is the canonical key and parses into
    /// `Project::workspaces` directly (no alias path).
    #[test]
    fn load_project_accepts_new_workspaces_key() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = home.join("projects");
        std::fs::create_dir_all(&dir).unwrap();
        let repo = home.join("modern-repo");
        std::fs::create_dir_all(&repo).unwrap();
        let yaml = format!(
            "name: modern\n\
             repo: {}\n\
             default_branch: main\n\
             machines:\n\
             \x20\x20- {{ name: hub, kind: local, work_dir: {} }}\n\
             orchestrator: {{ runner: claude }}\n\
             agent_runners:\n\
             \x20\x20claude: {{ command: claude, flags: [] }}\n\
             workspaces:\n\
             \x20\x20- {{ name: alpha, machine: hub, runner: claude }}\n",
            repo.display(),
            repo.display(),
        );
        std::fs::write(dir.join("modern.yaml"), yaml).unwrap();
        let loaded = load_project("modern").unwrap();
        assert_eq!(loaded.workspaces.len(), 1);
        assert_eq!(loaded.workspaces[0].name, "alpha");
        std::env::remove_var("SHELBI_HOME");
    }

    // ---- write serialization (F3/F7/F9) -----------------------------------

    /// The F3 lost-update scenario: one thread heartbeats (touches only
    /// `zen_last_crashed_at`) while another sets the workspace filter and
    /// zen mode. Pre-locking, whichever write landed last reverted the
    /// other's fields from its stale snapshot — a heartbeat could silently
    /// re-arm Zen Mode after the user opted out. With `update_state`, every
    /// field update survives.
    #[test]
    fn concurrent_state_mutators_preserve_both_updates() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        write_state("p", &State::default()).unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let b1 = barrier.clone();
        let heartbeats = std::thread::spawn(move || {
            b1.wait();
            for _ in 0..25 {
                zen_heartbeat("p").unwrap();
            }
        });
        let b2 = barrier.clone();
        let toggles = std::thread::spawn(move || {
            b2.wait();
            for _ in 0..25 {
                set_workspace_filter("p", Some("alpha")).unwrap();
                set_zen_mode("p", ZenModeState::On, "test").unwrap();
            }
        });
        heartbeats.join().unwrap();
        toggles.join().unwrap();

        let s = read_state("p").unwrap();
        assert_eq!(
            s.zen_mode,
            ZenModeState::On,
            "zen mode reverted by a concurrent heartbeat's stale snapshot"
        );
        assert_eq!(
            s.workspace_filter.as_deref(),
            Some("alpha"),
            "workspace filter lost to a concurrent mutator"
        );
        assert!(
            s.zen_last_crashed_at.is_some(),
            "heartbeat timestamp lost to a concurrent mutator"
        );
        std::env::remove_var("SHELBI_HOME");
    }

    /// F7: two concurrent moves into the same column must not both compute
    /// `len()` before either saves — the destination ends up with
    /// contiguous, duplicate-free priorities.
    #[test]
    fn concurrent_moves_into_one_column_never_duplicate_priorities() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::Todo, 0), "").unwrap();
        save_task("p", &make_task("b", Column::Todo, 1), "").unwrap();

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let b1 = barrier.clone();
        let t1 = std::thread::spawn(move || {
            b1.wait();
            move_task("p", "a", Column::InProgress).unwrap();
        });
        let b2 = barrier.clone();
        let t2 = std::thread::spawn(move || {
            b2.wait();
            move_task("p", "b", Column::InProgress).unwrap();
        });
        t1.join().unwrap();
        t2.join().unwrap();

        let col = list_column("p", Column::InProgress).unwrap();
        let mut prios: Vec<_> = col.iter().map(|tf| tf.task.priority).collect();
        prios.sort_unstable();
        assert_eq!(prios, vec![0, 1], "destination priorities must be contiguous 0..N");
        std::env::remove_var("SHELBI_HOME");
    }

    /// Moving into a column that already carries duplicate priorities
    /// (older builds could produce them) repairs the destination instead
    /// of preserving the skew forever.
    #[test]
    fn move_task_renumbers_destination_and_repairs_duplicates() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        save_task("p", &make_task("a", Column::InProgress, 0), "").unwrap();
        save_task("p", &make_task("b", Column::InProgress, 0), "").unwrap(); // duplicate
        save_task("p", &make_task("c", Column::Todo, 0), "").unwrap();

        move_task("p", "c", Column::InProgress).unwrap();

        let col = list_column("p", Column::InProgress).unwrap();
        let prios: Vec<_> = col.iter().map(|tf| tf.task.priority).collect();
        assert_eq!(prios, vec![0, 1, 2], "destination must be renumbered contiguous");
        std::env::remove_var("SHELBI_HOME");
    }

    /// Priority ties order by id, not by whatever order `read_dir`
    /// happened to return — the board must render the same way on every
    /// scan.
    #[test]
    fn list_tasks_breaks_priority_ties_by_id() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Created in reverse-alphabetical order so read_dir order (often
        // creation order) disagrees with the id tiebreak.
        save_task("p", &make_task("zeta", Column::Todo, 0), "").unwrap();
        save_task("p", &make_task("alpha", Column::Todo, 0), "").unwrap();
        let col = list_column("p", Column::Todo).unwrap();
        let ids: Vec<_> = col.iter().map(|tf| tf.task.id.as_str()).collect();
        assert_eq!(ids, vec!["alpha", "zeta"]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn create_task_refuses_to_overwrite_existing() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        create_task("p", &make_task("dup", Column::Todo, 0), "original\n").unwrap();
        let err = create_task("p", &make_task("dup", Column::Todo, 1), "clobber\n").unwrap_err();
        assert!(err.to_string().contains("already exists"), "got: {err}");
        // Original body untouched.
        assert!(load_task("p", "dup").unwrap().body.contains("original"));
        std::env::remove_var("SHELBI_HOME");
    }

    /// F9(a): two threads of one process writing the same target used to
    /// share a pid-only temp path and corrupt each other. Every surviving
    /// file content must be one writer's bytes, intact.
    #[test]
    fn atomic_write_same_path_from_two_threads_stays_intact() {
        let dir = fresh_home();
        let path = dir.join("contended.json");
        let a = vec![b'A'; 64 * 1024];
        let b = vec![b'B'; 64 * 1024];

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mk = |bytes: Vec<u8>, barrier: std::sync::Arc<std::sync::Barrier>, path: PathBuf| {
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    atomic_write(&path, &bytes).unwrap();
                }
            })
        };
        let t1 = mk(a, barrier.clone(), path.clone());
        let t2 = mk(b, barrier.clone(), path.clone());
        t1.join().unwrap();
        t2.join().unwrap();

        let got = std::fs::read(&path).unwrap();
        assert_eq!(got.len(), 64 * 1024);
        assert!(
            got.iter().all(|&c| c == got[0]),
            "file is a mix of two writers' bytes"
        );
    }

    /// F9(b): the temp suffix must append to the file name, not replace
    /// the extension — `x.json` and `x.yaml` written concurrently used to
    /// collide on one `x.tmp.<pid>` path.
    #[test]
    fn atomic_write_same_stem_different_extension_do_not_collide() {
        let dir = fresh_home();
        let json = dir.join("x.json");
        let yaml = dir.join("x.yaml");

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mk = |path: PathBuf, byte: u8, barrier: std::sync::Arc<std::sync::Barrier>| {
            std::thread::spawn(move || {
                barrier.wait();
                for _ in 0..50 {
                    atomic_write(&path, &vec![byte; 4096]).unwrap();
                }
            })
        };
        let t1 = mk(json.clone(), b'J', barrier.clone());
        let t2 = mk(yaml.clone(), b'Y', barrier.clone());
        t1.join().unwrap();
        t2.join().unwrap();

        assert_eq!(std::fs::read(&json).unwrap(), vec![b'J'; 4096]);
        assert_eq!(std::fs::read(&yaml).unwrap(), vec![b'Y'; 4096]);
        // No temp litter left behind on the happy path.
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tmp."))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    /// `update_state` skips the disk write when the closure leaves the
    /// state unchanged, and propagates the closure's error without
    /// writing a partially mutated state.
    #[test]
    fn update_state_skips_noop_writes_and_aborts_on_closure_error() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        write_state("p", &State { zen_mode: ZenModeState::On, ..State::default() }).unwrap();
        let mtime_before = std::fs::metadata(state_path("p").unwrap())
            .unwrap()
            .modified()
            .unwrap();

        // No-op closure → file untouched.
        update_state("p", |_state| Ok(())).unwrap();
        let mtime_after = std::fs::metadata(state_path("p").unwrap())
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(mtime_before, mtime_after, "no-op update must not rewrite the file");

        // Failing closure → mutation not persisted.
        let err = update_state("p", |state| {
            state.zen_mode = ZenModeState::Off;
            Err::<(), _>(shelbi_core::Error::Other("boom".into()))
        })
        .unwrap_err();
        assert!(err.to_string().contains("boom"));
        assert_eq!(read_state("p").unwrap().zen_mode, ZenModeState::On);

        std::env::remove_var("SHELBI_HOME");
    }

    // -----------------------------------------------------------------
    // Mode-aware `load_project` (F1: migrated projects must stay loadable)

    /// A distinct temp dir to stand in for a project's repo root.
    fn fresh_repo() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-state-repo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    /// A pre-split project registered at `~/.shelbi/projects/<name>.yaml`
    /// loads through the flat parser exactly as before — the global-mode
    /// path is untouched by the split fallback.
    #[test]
    fn load_project_reads_global_yaml() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        save_project(&fixture_project("myapp", None)).unwrap();

        let loaded = load_project("myapp").unwrap();
        assert_eq!(loaded.name, "myapp");
        // Pre-split YAMLs carry no `config_mode:` → global mode.
        assert_eq!(loaded.config_mode, None);
        std::env::remove_var("SHELBI_HOME");
    }

    /// F1: once `migrate-to-in-repo` retires the global YAML,
    /// `load_project` must fall back to the split layout
    /// (`<name>/local.yaml` + `<repo>/.shelbi/project.yaml`) and return
    /// the same project in in-repo mode. This is the loader half every
    /// runtime caller (board, workspaces, review pane) depends on — a
    /// migrated project that this can't open is bricked.
    #[test]
    fn load_project_falls_back_to_split_after_migration() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        let repo = fresh_repo();
        std::env::set_var("SHELBI_HOME", &home);

        let mut p = fixture_project("myapp", None);
        p.repo = repo.to_string_lossy().into_owned();
        p.machines[0].work_dir = repo.clone();
        save_project(&p).unwrap();

        // Migrate: writes both halves, retires (renames) the global YAML.
        apply_migration_plan(&plan_in_repo_migration("myapp").unwrap()).unwrap();
        assert!(
            !home.join("projects/myapp.yaml").exists(),
            "global YAML should be retired after migration"
        );
        assert!(
            home.join("projects/myapp.yaml.migrated").is_file(),
            "retired copy should remain as a rollback path"
        );

        // With the global YAML gone, the loader resolves the config
        // through the two-file split.
        let loaded = load_project("myapp").unwrap();
        assert_eq!(loaded.name, "myapp");
        assert_eq!(loaded.config_mode, Some(shelbi_core::ConfigMode::InRepo));
        assert_eq!(loaded.repo, repo.to_string_lossy());
        std::env::remove_var("SHELBI_HOME");
    }

    /// Neither layout present → an error that names both candidate paths
    /// so the failure is debuggable (not a bare file-not-found on just
    /// the global YAML).
    #[test]
    fn load_project_missing_reports_both_candidate_paths() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let err = load_project("ghost").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("ghost.yaml"), "missing global path: {msg}");
        assert!(msg.contains("local.yaml"), "missing local path: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }

    /// A `local.yaml` whose referenced shared half is absent is a
    /// half-broken state — surfaced with both paths rather than a raw
    /// NotFound, so the user knows to restore `<repo>/.shelbi/project.yaml`.
    #[test]
    fn load_project_split_missing_shared_half_errors_clearly() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        let repo = fresh_repo();
        std::env::set_var("SHELBI_HOME", &home);

        let state = home.join("projects/myapp");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            state.join("local.yaml"),
            format!("repo: {}\nmachines: []\n", repo.display()),
        )
        .unwrap();
        // Note: no <repo>/.shelbi/project.yaml, and no global YAML.

        let err = load_project("myapp").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("no shared config"), "unexpected err: {msg}");
        assert!(msg.contains("project.yaml"), "should name shared path: {msg}");
        std::env::remove_var("SHELBI_HOME");
    }
}
