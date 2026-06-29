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

use std::collections::{BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shelbi_core::{
    default_project_statuses, default_workflow, Agent, Column, Project, Result, Session, Task,
};

mod agent_workspaces;
mod hub_config;
pub mod keymap;
mod user_config;
mod workspace_status;
mod workflows;

pub use agent_workspaces::{
    agent_instructions_path, agent_shared_preamble_path, agent_skills_dir, agent_workspace_dir,
    compose_agent_prompt, count_agent_skills, default_agent_body, is_default_agent,
    legacy_claude_md_path, list_agents, load_shared_preamble, materialize_default_agents,
    maybe_emit_claude_md_migration_hint, reset_claude_md_migration_hint, self_heal_default_agents,
    AgentMaterializeOutcome, DEFAULT_AGENTS, DEFAULT_DEVELOPER_INSTRUCTIONS,
    DEFAULT_ORCHESTRATOR_INSTRUCTIONS, DEVELOPER_AGENT, ORCHESTRATOR_AGENT, SHARED_AGENT_DIR,
    SHARED_PREAMBLE_FILE,
};
pub use hub_config::{
    hub_config_path, list_projects, load_hub_config, save_hub_config, touch_project_launched,
    HubConfig, ProjectMeta, ProjectSummary,
};
pub use user_config::{
    load_user_config, save_user_config, user_config_path, Keymap, UserConfig, ZenToggleChord,
};
pub use workspace_status::{
    append_contextstore_event, append_dispatch_event, append_heartbeat_event,
    append_project_event, append_rebase_event, append_task_event, append_workspace_event,
    append_zen_dryrun_event, append_zen_mode_event, events_log_path, load_workspace_status,
    parse_pane_title_marker, parse_pane_title_state, save_workspace_status, workspace_status_path,
    workspaces_dir, PaneMarker, WorkspaceState, WorkspaceStatus,
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
    if !path.exists() {
        return Ok(ShelbiConfig::default());
    }
    let text = fs::read_to_string(&path)?;
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

/// Default shelbi home directory: `~/.shelbi`, overridable via
/// `$SHELBI_HOME` (useful for tests and sandboxed CI).
pub fn shelbi_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SHELBI_HOME") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    dirs::home_dir()
        .map(|h| h.join(".shelbi"))
        .ok_or_else(|| shelbi_core::Error::Other("no home directory".into()))
}

pub fn projects_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("projects"))
}

pub fn sessions_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("sessions"))
}

pub fn project_dir(project: &str) -> Result<PathBuf> {
    Ok(projects_dir()?.join(project))
}

/// Resolve the workspace settings template path for a project: the override
/// in [`Project::workspace_settings_template`] (with `~` expansion) if set,
/// otherwise the default at
/// `~/.shelbi/projects/<name>/workspace-settings.json.template`.
///
/// As a one-shot migration, if the legacy `workspace-settings.json` (no
/// `.template` suffix) exists in the project dir and the new path doesn't,
/// the legacy file is renamed in place — see [`migrate_workspace_settings_template`].
pub fn workspace_settings_template_path(project: &Project) -> Result<PathBuf> {
    if let Some(p) = &project.workspace_settings_template {
        return Ok(expand_tilde(p));
    }
    let dir = project_dir(&project.name)?;
    migrate_workspace_settings_template(&dir);
    Ok(dir.join("workspace-settings.json.template"))
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
/// [`migrate_default_workflow`]. Either file may exist alone (e.g. a
/// user-managed `statuses.yml` with no workflow files yet, or a legacy
/// `default.yaml` whose `statuses.yml` was just stripped); the workflow
/// loader's migration path covers both cases on the next read.
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

fn expand_tilde(p: &Path) -> PathBuf {
    if let Some(rest) = p.to_str().and_then(|s| s.strip_prefix("~/")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    p.to_path_buf()
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
// Project / Session YAML

pub fn load_project(project: &str) -> Result<Project> {
    let p = projects_dir()?.join(format!("{project}.yaml"));
    let text = fs::read_to_string(&p)?;
    warn_legacy_workers_key(project, &text);
    let mut p: Project = serde_yaml::from_str(&text)?;
    p.validate_workspaces()?;
    let repo = p.repo.clone();
    p.detect_shapes(repo);
    // Best-effort: drop workflows/default.yaml and workflows/statuses.yml
    // into the project directory on first load. Idempotent — see
    // migrate_default_workflow / migrate_default_statuses. The workflow
    // loader will additionally run the legacy-form migration on demand
    // when it discovers inline name/category fields.
    if let Ok(dir) = project_dir(&p.name) {
        migrate_default_workflow(&dir);
        migrate_default_statuses(&dir);
    }
    Ok(p)
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
    /// Persisted Kanban workflow filter — `None` means "All workflows"
    /// (All-mode rendering). When set to a workflow name, the board
    /// narrows to that workflow's columns only. Same lifecycle as
    /// `workspace_filter`: written by the dropdown commit path, read on
    /// every refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_filter: Option<String>,
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
/// preferences that should follow the user across every project — the
/// most recent tmux palette binding (so the orchestrator can unbind it
/// cleanly on rebind / project switch), and the one-shot acknowledgement
/// of the Zen Mode intro popover (so first-time-Zen explanation doesn't
/// re-fire in every project the user opens).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// Path to the global `state.json` (`~/.shelbi/state.json`).
pub fn global_state_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("state.json"))
}

/// Read `~/.shelbi/state.json`. Missing file → `GlobalState::default()`.
pub fn read_global_state() -> Result<GlobalState> {
    let path = global_state_path()?;
    if !path.exists() {
        return Ok(GlobalState::default());
    }
    let text = fs::read_to_string(&path)?;
    serde_json::from_str(&text)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))
}

/// Atomically write `state` to `~/.shelbi/state.json`.
pub fn write_global_state(state: &GlobalState) -> Result<()> {
    let path = global_state_path()?;
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))?;
    atomic_write(&path, &body)
}

/// Mark the Zen Mode intro popover as acknowledged. Reads, mutates, and
/// re-writes `~/.shelbi/state.json` so the other fields are preserved.
/// Idempotent — a no-op when already set.
pub fn mark_zen_intro_seen() -> Result<()> {
    let mut state = read_global_state()?;
    if state.zen_intro_seen {
        return Ok(());
    }
    state.zen_intro_seen = true;
    write_global_state(&state)
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
    let mut state = read_state(project)?;
    state.zen_last_crashed_at = Some(Utc::now());
    write_state(project, &state)
}

/// Clear `zen_last_crashed_at`. Called from the orchestrator's graceful
/// exit path and from `quit_project` so a clean shutdown doesn't leave
/// a stale timestamp on disk that the next start would misread as a
/// crash. Idempotent — a no-op when nothing is set.
pub fn zen_clear_crash(project: &str) -> Result<()> {
    let mut state = read_state(project)?;
    if state.zen_last_crashed_at.is_none() {
        return Ok(());
    }
    state.zen_last_crashed_at = None;
    write_state(project, &state)
}

/// Run at orchestrator start. If `zen_last_crashed_at` is within the
/// recovery window AND `zen_mode == On`, force the mode to `Off` and
/// report `AutoDisabled`. Either way the stale timestamp is cleared so
/// the new heartbeat starts from a fresh state. The signal has been
/// consumed once read — calling this a second time on the same disk
/// state returns `NoCrash`.
pub fn zen_check_crash_recovery(project: &str) -> Result<ZenCrashRecovery> {
    let mut state = read_state(project)?;
    let Some(crashed_at) = state.zen_last_crashed_at else {
        return Ok(ZenCrashRecovery::NoCrash);
    };
    let age = Utc::now() - crashed_at;
    let recent = age <= chrono::Duration::seconds(ZEN_CRASH_RECOVERY_WINDOW_SECS);
    let should_disable = recent && state.zen_mode == ZenModeState::On;
    state.zen_last_crashed_at = None;
    if should_disable {
        state.zen_mode = ZenModeState::Off;
    }
    write_state(project, &state)?;
    if should_disable {
        Ok(ZenCrashRecovery::AutoDisabled { crashed_at })
    } else {
        Ok(ZenCrashRecovery::NoCrash)
    }
}

/// Read `state.json` for `project`. Returns `State::default()` when the
/// file is missing — the first call after creating a project shouldn't
/// require a separate seeding step.
pub fn read_state(project: &str) -> Result<State> {
    let path = state_path(project)?;
    if !path.exists() {
        return Ok(State::default());
    }
    let text = fs::read_to_string(&path)?;
    serde_json::from_str(&text)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))
}

/// Atomically write `state` to `~/.shelbi/projects/<project>/state.json`.
pub fn write_state(project: &str, state: &State) -> Result<()> {
    let path = state_path(project)?;
    let body = serde_json::to_vec_pretty(state)
        .map_err(|e| shelbi_core::Error::Other(format!("state.json: {e}")))?;
    atomic_write(&path, &body)
}

/// Persist `target` as the project's `zen_mode` and append a
/// `mode=zen <prev> -> <target> reason=<source>` line to `events.log`.
/// Single source of truth shared by `shelbi zen on|off|pause` (CLI),
/// the TUI's Alt+Z handler, and the palette's toggle entry — each
/// caller picks its own `source` token (`user:cli`, `user:hotkey`,
/// `user:palette`) so the activity feed can distinguish them. Returns
/// the prior mode for callers that want to render a diff without a
/// follow-up read.
pub fn set_zen_mode(project: &str, target: ZenModeState, source: &str) -> Result<ZenModeState> {
    let mut state = read_state(project)?;
    let prev = state.zen_mode;
    state.zen_mode = target;
    write_state(project, &state)?;
    let _ = append_zen_mode_event(prev.as_str(), target.as_str(), source);
    Ok(prev)
}

/// Persist the Kanban workspace filter for `project`. `None` clears it
/// back to "All workspaces". Reads, mutates, and re-writes `state.json` so
/// the other fields (Zen mode, crash timestamp) are preserved. No event
/// log entry — view-state changes are noise in the activity feed.
pub fn set_workspace_filter(project: &str, filter: Option<&str>) -> Result<()> {
    let mut state = read_state(project)?;
    state.workspace_filter = filter.map(|s| s.to_string());
    write_state(project, &state)
}

/// Persist the Kanban workflow filter for `project`. `None` clears it
/// back to "All workflows" (All-mode union rendering). Mirrors
/// [`set_workspace_filter`] — same merge-then-write pattern, no event log
/// entry.
pub fn set_workflow_filter(project: &str, filter: Option<&str>) -> Result<()> {
    let mut state = read_state(project)?;
    state.workflow_filter = filter.map(|s| s.to_string());
    write_state(project, &state)
}

/// Binary toggle on top of [`set_zen_mode`]: On flips to Off, anything
/// else (Off, Paused) flips to On. Paused collapses to On here because
/// the toggle is intentionally a two-state hop — the CLI is still the
/// path that can land on Paused. Returns the new mode so callers can
/// update their cached state without a re-read.
pub fn toggle_zen_mode(project: &str, source: &str) -> Result<ZenModeState> {
    let current = read_state(project)?.zen_mode;
    let target = match current {
        ZenModeState::On => ZenModeState::Off,
        ZenModeState::Off | ZenModeState::Paused => ZenModeState::On,
    };
    set_zen_mode(project, target, source)?;
    Ok(target)
}

// ---------------------------------------------------------------------------
// Agent markdown files

pub fn agent_path(project: &str, id: &str) -> Result<PathBuf> {
    Ok(agents_dir(project)?.join(format!("{id}.md")))
}

pub fn agent_log_path(project: &str, id: &str) -> Result<PathBuf> {
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
    Ok(tasks_dir(project)?.join(format!("{id}.md")))
}

pub struct TaskFile {
    pub task: Task,
    pub body: String,
}

pub fn save_task(project: &str, task: &Task, body_md: &str) -> Result<()> {
    ensure_dir(&tasks_dir(project)?)?;
    let path = task_path(project, &task.id)?;
    write_frontmatter_file(&path, task, body_md)
}

pub fn load_task(project: &str, id: &str) -> Result<TaskFile> {
    let path = task_path(project, id)?;
    let text = fs::read_to_string(&path)?;
    parse_task_file(&text)
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
                forget_parse_warn(&path);
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
    out.sort_by_key(|tf| {
        let col_idx = Column::ALL.iter().position(|c| *c == tf.task.column).unwrap_or(0);
        (col_idx, tf.task.priority)
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
/// dependency. Returned in priority order.
pub fn list_ready(project: &str) -> Result<Vec<TaskFile>> {
    let columns = task_columns(project)?;
    Ok(list_column(project, Column::Todo)?
        .into_iter()
        .filter(|tf| !tf.task.is_blocked(&columns))
        .collect())
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
    save_task(project, &task, &body)?;
    renumber_column(project, old_column)?;
    Ok(Some((old_column, new_column, workflow)))
}

/// Re-position `id` to slot `new_priority` within its current column. Other
/// tasks shift to keep the column contiguous from 0.
pub fn set_task_priority(project: &str, id: &str, new_priority: u32) -> Result<()> {
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

/// Stamp 0..N priorities onto the ordered slice and persist only the
/// tasks whose priority actually changed.
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
        save_task(project, &task, &tf.body)?;
    }
    Ok(())
}

/// Reload `column`'s tasks, sort by current priority, and renumber 0..N.
pub fn renumber_column(project: &str, column: Column) -> Result<()> {
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

/// Atomic write: write to a temp file in the same dir, then rename.
pub(crate) fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| shelbi_core::Error::Other(format!("no parent dir for {path:?}")))?;
    ensure_dir(dir)?;
    let tmp = path.with_extension(format!(
        "tmp.{}",
        std::process::id()
    ));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_project(name: &str, override_template: Option<PathBuf>) -> shelbi_core::Project {
        use shelbi_core::*;
        let mut runners = std::collections::BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        Project {
            name: name.into(),
            repo: "r".into(),
            default_branch: "main".into(),
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

        // Both transitions appear in the events log under the supplied source.
        let log = std::fs::read_to_string(events_log_path().unwrap()).unwrap();
        assert!(
            log.contains("mode=zen off -> on reason=user:palette"),
            "missing on transition in: {log}"
        );
        assert!(
            log.contains("mode=zen on -> off reason=user:palette"),
            "missing off transition in: {log}"
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
            log.contains("mode=zen off -> paused reason=user:cli"),
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
}
