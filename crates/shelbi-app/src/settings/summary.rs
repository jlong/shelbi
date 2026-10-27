//! The General summary: where this computer's settings and the shown
//! project's live, read from the real config files, for the TUI's General
//! page, with the appearance and editor preferences its controls edit
//! ([`super::general`]). Every read here is blocking IO, so a client runs [`read`] off its
//! UI thread under a [`Job`](super::Job) ticket.

use std::path::{Path, PathBuf};

use crate::command::EditItem;

use super::general::Preferences;

/// A config file and whether it is there and reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    /// It exists and parses.
    Present(PathBuf),
    /// It doesn't exist: the built-in defaults apply.
    Missing(PathBuf),
    /// It exists but doesn't read: why.
    Invalid { path: PathBuf, error: String },
}

impl FileState {
    pub fn path(&self) -> &Path {
        match self {
            FileState::Present(p) | FileState::Missing(p) => p,
            FileState::Invalid { path, .. } => path,
        }
    }
}

/// This computer's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalSummary {
    /// The Shelbi folder (`~/.shelbi`, or `$SHELBI_HOME`).
    pub home: PathBuf,
    /// `config.yaml`.
    pub config: FileState,
    /// Its appearance and editor settings, or why it doesn't read.
    pub preferences: Result<Preferences, String>,
    /// `$EDITOR`, the editor's fallback without an `editor:` setting.
    pub env_editor: Option<String>,
    /// `keys.yaml`.
    pub keys: FileState,
    /// Bindings in `keys.yaml` the loader refused (unknown actions, bad
    /// chords, conflicts).
    pub keymap_problems: usize,
}

/// The shown project's settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectSummary {
    pub project: String,
    pub label: String,
    /// Where its settings (agents, workflows, statuses) live.
    pub settings_dir: PathBuf,
    /// They live in the repository (`<repo>/.shelbi`), shared through Git.
    pub in_repo: bool,
    /// Its workflows and statuses, or why they don't read.
    pub workflows: Result<(usize, usize), String>,
    /// Its agents, or why they don't list.
    pub agents: Result<usize, String>,
    /// The machines it runs agents on.
    pub machines: usize,
    /// The files Open in editor offers, the palette's edit targets.
    pub edit_targets: Vec<EditItem>,
}

/// Everything the General page shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneralSummary {
    pub global: GlobalSummary,
    /// `None` with no project open; `Err` when the project doesn't read.
    pub project: Option<Result<ProjectSummary, String>>,
}

/// Read the summary for `project` (blocking).
pub fn read(project: Option<&str>) -> GeneralSummary {
    GeneralSummary {
        global: read_global(),
        project: project.map(read_project),
    }
}

/// This computer's settings (blocking).
pub fn read_global() -> GlobalSummary {
    let home = shelbi_state::shelbi_home().unwrap_or_else(|_| PathBuf::from("~/.shelbi"));
    let config_path = home.join("config.yaml");
    let (config, preferences) = match shelbi_state::load_user_config() {
        Ok(cfg) if config_path.exists() => (FileState::Present(config_path), Ok(Preferences::from_config(&cfg))),
        Ok(cfg) => (FileState::Missing(config_path), Ok(Preferences::from_config(&cfg))),
        Err(e) => (
            FileState::Invalid {
                path: config_path,
                error: e.to_string(),
            },
            Err(e.to_string()),
        ),
    };
    // Validated, not loaded: `load_keymaps` runs its one-shot migrations,
    // and a summary never writes.
    let keys_path = home.join(shelbi_state::keymap::KEYS_FILENAME);
    let (keys, keymap_problems) = match std::fs::read_to_string(&keys_path) {
        Ok(text) => {
            let (_, diagnostics) = shelbi_state::keymap::validate_keymaps_yaml(&text, None);
            (FileState::Present(keys_path), diagnostics.len())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (FileState::Missing(keys_path), 0),
        Err(e) => (
            FileState::Invalid {
                path: keys_path,
                error: e.to_string(),
            },
            0,
        ),
    };
    GlobalSummary {
        home,
        config,
        preferences,
        env_editor: std::env::var("EDITOR").ok(),
        keys,
        keymap_problems,
    }
}

/// The shown project's settings (blocking). `Err` when the project itself
/// doesn't load (unregistered, or its YAML doesn't parse); a part that
/// doesn't read (its workflows, its agents) carries its own error.
pub fn read_project(project: &str) -> Result<ProjectSummary, String> {
    let loaded = shelbi_state::load_project(project).map_err(|e| e.to_string())?;
    let in_repo = loaded.config_mode == Some(shelbi_core::ConfigMode::InRepo);
    let settings_dir = shelbi_state::config_project_dir(project).map_err(|e| e.to_string())?;
    let workflows = shelbi_state::workflow_config::WorkflowConfig::load(project)
        .map(|c| (c.workflows().count(), c.statuses().len()))
        .map_err(|e| e.to_string());
    let agents = shelbi_state::list_agents(project)
        .map(|a| a.len())
        .map_err(|e| e.to_string());
    Ok(ProjectSummary {
        project: project.to_string(),
        label: loaded.display_label().to_string(),
        settings_dir,
        in_repo,
        workflows,
        agents,
        machines: loaded.machines.len(),
        edit_targets: crate::command_model::edit_targets(project),
    })
}

/// `path` as the page prints it: the home folder shortened to `~`.
pub fn display_path(path: &Path) -> String {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    crate::add_project_wizard::tilde(path, home.as_deref())
}

/// `n` `noun`s, pluralized the simple way the page needs.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 {
        format!("1 {noun}")
    } else {
        format!("{n} {noun}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_pluralize() {
        assert_eq!(count(0, "agent"), "0 agents");
        assert_eq!(count(1, "agent"), "1 agent");
        assert_eq!(count(6, "machine"), "6 machines");
    }

    #[test]
    fn file_states_name_their_path() {
        let p = PathBuf::from("/h/.shelbi/keys.yaml");
        assert_eq!(FileState::Missing(p.clone()).path(), p.as_path());
        assert_eq!(
            FileState::Invalid {
                path: p.clone(),
                error: "bad".into()
            }
            .path(),
            p.as_path()
        );
    }
}
