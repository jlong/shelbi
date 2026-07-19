//! Hub-level config (`~/.shelbi/shelbi.yaml`) and the project index used by
//! both the CLI project picker and the in-TUI "Switch project" palette
//! action. The config is opt-in: missing file is treated as empty, never
//! written until something needs persisting (e.g. a launch timestamp).
//!
//! ProjectSummary carries the minimum a picker needs (name + repo path +
//! machine/workspace counts + recency) without forcing every caller through
//! the full Project deserialization at display time.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use shelbi_core::{Project, Result};

use crate::{atomic_write, ensure_dir, projects_dir, shelbi_home};

/// Per-project bookkeeping stored in the hub config. New fields land here
/// so the picker can grow surface area without invalidating older files.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_launched: Option<DateTime<Utc>>,
}

/// The single hub-wide config at `~/.shelbi/shelbi.yaml`. Optional —
/// absence is treated as default-empty.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct HubConfig {
    #[serde(default)]
    pub projects: BTreeMap<String, ProjectMeta>,
}

pub fn hub_config_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("shelbi.yaml"))
}

pub fn load_hub_config() -> Result<HubConfig> {
    let path = hub_config_path()?;
    match fs::read_to_string(&path) {
        Ok(s) => Ok(serde_yaml::from_str(&s).unwrap_or_default()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(HubConfig::default()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

pub fn save_hub_config(cfg: &HubConfig) -> Result<()> {
    let path = hub_config_path()?;
    ensure_dir(path.parent().unwrap())?;
    atomic_write(&path, serde_yaml::to_string(cfg)?.as_bytes())
}

/// Stamp `project`'s `last_launched` to now. Creates the entry (and the
/// config file) if missing.
pub fn touch_project_launched(project: &str) -> Result<()> {
    let mut cfg = load_hub_config().unwrap_or_default();
    cfg.projects
        .entry(project.to_string())
        .or_default()
        .last_launched = Some(Utc::now());
    save_hub_config(&cfg)
}

/// What pickers display per project. Counts come straight from the YAML so
/// a project with no `workspaces:` reads as 0 — same shape as the CLI's
/// existing `workspace list`.
#[derive(Debug, Clone)]
pub struct ProjectSummary {
    /// The slug/id — used for switching, tmux sessions, and every state key.
    pub name: String,
    /// Optional human-readable label. `None` for legacy projects, which then
    /// display under `name`. Pickers render [`ProjectSummary::display_label`].
    pub display_name: Option<String>,
    pub repo_path: String,
    pub machine_count: usize,
    pub workspace_count: usize,
    pub last_launched: Option<DateTime<Utc>>,
}

impl ProjectSummary {
    /// The label to show a human: [`display_name`](Self::display_name) when
    /// set, otherwise the slug [`name`](Self::name). Mirrors
    /// [`shelbi_core::Project::display_label`].
    pub fn display_label(&self) -> &str {
        self.display_name.as_deref().unwrap_or(&self.name)
    }
}

/// Scan `~/.shelbi/projects/*.yaml`, decorate each with the hub config's
/// last-launched timestamp, and return them sorted most-recently-launched
/// first (alphabetical for never-launched projects). Files that fail to
/// parse are skipped silently — the picker shouldn't refuse to open just
/// because one YAML is malformed.
pub fn list_projects() -> Result<Vec<ProjectSummary>> {
    let dir = projects_dir()?;
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let cfg = load_hub_config().unwrap_or_default();
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        // The id is the filename stem, not any YAML key. A file with no usable
        // stem can't be a project registration — skip it like an unparseable one.
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(_) => continue,
        };
        let project: Project = match serde_yaml::from_str(&text) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let last_launched = cfg.projects.get(&id).and_then(|m| m.last_launched);
        out.push(ProjectSummary {
            name: id,
            // Prefer the deprecated `display_name:` alias, then the free-form
            // `name:` label; `None` renders the id (see `display_label`).
            display_name: project.display_name.clone().or_else(|| project.label.clone()),
            repo_path: project.repo.clone(),
            machine_count: project.machines.len(),
            workspace_count: project.workspaces.len(),
            last_launched,
        });
    }
    out.sort_by(|a, b| {
        use std::cmp::Reverse;
        // Reverse on Option<DateTime>: None > Some(_) under Reverse,
        // landing never-launched projects after launched ones; within
        // launched the most recent wins.
        let ka = (Reverse(a.last_launched), &a.name);
        let kb = (Reverse(b.last_launched), &b.name);
        ka.cmp(&kb)
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-hub-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write_project_yaml(home: &std::path::Path, name: &str, workspaces: usize) {
        let dir = home.join("projects");
        fs::create_dir_all(&dir).unwrap();
        let workspaces_yaml: String = (0..workspaces)
            .map(|i| format!("  - {{ name: w{i}, machine: hub, runner: claude }}\n"))
            .collect();
        let yaml = format!(
            "name: {name}\n\
             repo: /tmp/{name}\n\
             machines:\n\
             \x20\x20- {{ name: hub, kind: local, work_dir: /tmp }}\n\
             orchestrator: {{ runner: claude }}\n\
             agent_runners:\n\
             \x20\x20claude: {{ command: claude, flags: [] }}\n\
             workspaces:\n\
             {workspaces_yaml}"
        );
        fs::write(dir.join(format!("{name}.yaml")), yaml).unwrap();
    }

    #[test]
    fn list_projects_finds_yaml_files_and_counts_fields() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_project_yaml(&home, "alpha", 2);
        write_project_yaml(&home, "beta", 0);

        let list = list_projects().unwrap();
        let by_name: BTreeMap<_, _> = list.iter().map(|p| (p.name.as_str(), p)).collect();
        assert_eq!(by_name.len(), 2);
        let alpha = by_name["alpha"];
        assert_eq!(alpha.repo_path, "/tmp/alpha");
        assert_eq!(alpha.machine_count, 1);
        assert_eq!(alpha.workspace_count, 2);
        assert_eq!(by_name["beta"].workspace_count, 0);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn touch_project_launched_sorts_most_recent_first() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        write_project_yaml(&home, "alpha", 0);
        write_project_yaml(&home, "bravo", 0);
        write_project_yaml(&home, "charlie", 0);

        // No touches → alphabetical.
        let names: Vec<_> = list_projects()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"]);

        // Touch in this order: alpha, charlie. charlie wins recency,
        // alpha second, bravo never-launched lands last.
        touch_project_launched("alpha").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        touch_project_launched("charlie").unwrap();
        let names: Vec<_> = list_projects()
            .unwrap()
            .into_iter()
            .map(|p| p.name)
            .collect();
        assert_eq!(names, vec!["charlie", "alpha", "bravo"]);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn load_hub_config_treats_missing_file_as_default() {
        let _g = TEST_LOCK.lock().unwrap();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let cfg = load_hub_config().unwrap();
        assert!(cfg.projects.is_empty());
        std::env::remove_var("SHELBI_HOME");
    }
}
