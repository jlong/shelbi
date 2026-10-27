//! A project's machine and workspace pool as config: the `machines:` and
//! `workspaces:` lists, read from and written back to the one file that holds
//! them.
//!
//! Both lists are user-local. A global project keeps them in
//! `~/.shelbi/projects/<name>.yaml` with everything else; an in-repo project
//! keeps them in `~/.shelbi/projects/<name>/local.yaml`, never in the
//! committed `<repo>/.shelbi/project.yaml`, so machine paths and SSH hosts
//! stay out of the repository. [`PoolConfig::load`] finds the right file the
//! way [`crate::load_project`] does (a present `local.yaml` means in-repo).
//!
//! [`PoolConfig::save`] writes the whole pool at once, but only the entries
//! that changed are rewritten, and only the keys that changed within them.
//! The write goes through [`yaml_edit`](crate::yaml_edit), so comments, key
//! order, unknown keys (a machine's `forward:`, a hand-added key on a
//! workspace) and every unrelated setting in the file survive. Nothing here
//! serializes a whole [`Project`].
//!
//! Concurrent changes: a save re-reads the file first. If the file is
//! byte-identical to what was loaded, the edit is applied to it. If something
//! else changed but both lists are exactly as loaded, the edit is applied to
//! the new text instead (an unrelated hand edit isn't lost or refused). If
//! either list changed, the save refuses with [`PoolConfigError::Conflict`]
//! and writes nothing. The resulting file must load and validate as a project
//! before it is written. Callers that read, decide and write (the lifecycle
//! operations in `shelbi_orchestrator::pool_admin`) additionally hold
//! [`crate::lock_pool_config`] so two Shelbi writers never interleave.

use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml::Value;
use shelbi_core::{ConfigMode, Machine, Project, WorkspaceSpec};

use crate::yaml_edit;

/// `tags:` may be spelled `tag:` on a machine or workspace.
const ALIASES: &[(&str, &str)] = &[("tags", "tag")];

/// Read-only legacy workspace keys. The loader folds `role:` into `tags` and
/// ignores `runner:`; a workspace entry that's rewritten drops them so the
/// file keeps meaning what was written (a kept `role: review` would put the
/// `review` tag back on reload).
const LEGACY_WORKSPACE_KEYS: &[&str] = &["role", "runner"];

/// Why a pool couldn't be loaded or saved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PoolConfigError {
    /// A file couldn't be read, parsed or written.
    #[error("{}: {message}", .path.display())]
    File { path: PathBuf, message: String },
    /// The edit would leave a config that doesn't load; the message is the
    /// loader's.
    #[error("{0}")]
    Invalid(String),
    /// The machines or workspaces in the file changed since it was loaded.
    /// Nothing was written; reload and redo the edit.
    #[error(
        "{} changed on disk since it was read (machines or workspaces were edited \
         elsewhere); reload and try again",
        .path.display()
    )]
    Conflict { path: PathBuf },
}

pub type Result<T, E = PoolConfigError> = std::result::Result<T, E>;

/// One entry of the pool as it should be saved. `was` names the entry it
/// continues (its name as loaded, so a rename keeps the entry's unknown keys
/// and comments); `None` is a new entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolEntry<T> {
    pub was: Option<String>,
    pub value: T,
}

impl<T: Named> PoolEntry<T> {
    /// An entry saved under the name it was loaded with.
    pub fn kept(value: T) -> Self {
        PoolEntry {
            was: Some(value.name().to_string()),
            value,
        }
    }

    /// An entry that didn't exist before.
    pub fn new(value: T) -> Self {
        PoolEntry { was: None, value }
    }
}

/// A pool entry's identity within its list.
pub trait Named {
    fn name(&self) -> &str;
}

impl Named for Machine {
    fn name(&self) -> &str {
        &self.name
    }
}

impl Named for WorkspaceSpec {
    fn name(&self) -> &str {
        &self.name
    }
}

/// A project's machines and workspaces as loaded, with the file they came
/// from.
#[derive(Debug, Clone)]
pub struct PoolConfig {
    project: String,
    mode: ConfigMode,
    path: PathBuf,
    text: String,
    /// The committed shared half (in-repo only): a save validates against it.
    shared_path: Option<PathBuf>,
    machines: Vec<Machine>,
    workspaces: Vec<WorkspaceSpec>,
}

impl PoolConfig {
    /// Load `project`'s pool from the file that holds it.
    pub fn load(project: &str) -> Result<Self> {
        let projects = crate::projects_dir().map_err(invalid)?;
        let local = projects.join(project).join("local.yaml");
        let (mode, path, shared_path) = if local.is_file() {
            let text = read(&local)?;
            let repo =
                crate::migrate::extract_repo_from_local_yaml(&text, &local).map_err(invalid)?;
            let shared = crate::expand_tilde_str(&repo)
                .join(".shelbi")
                .join("project.yaml");
            (ConfigMode::InRepo, local, Some(shared))
        } else {
            (
                ConfigMode::Global,
                projects.join(format!("{project}.yaml")),
                None,
            )
        };
        let text = read(&path)?;
        let parsed = parse_project(project, &path, &text, shared_path.as_deref())?;
        Ok(PoolConfig {
            project: project.to_string(),
            mode,
            path,
            text,
            shared_path,
            machines: parsed.machines,
            workspaces: parsed.workspaces,
        })
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    /// Which layout the project uses; says which file [`path`](Self::path)
    /// is.
    pub fn mode(&self) -> ConfigMode {
        self.mode
    }

    /// The file holding `machines:` and `workspaces:`: the project YAML in
    /// global mode, `local.yaml` in in-repo mode.
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn machines(&self) -> &[Machine] {
        &self.machines
    }

    pub fn workspaces(&self) -> &[WorkspaceSpec] {
        &self.workspaces
    }

    pub fn machine(&self, name: &str) -> Option<&Machine> {
        self.machines.iter().find(|m| m.name == name)
    }

    pub fn workspace(&self, name: &str) -> Option<&WorkspaceSpec> {
        self.workspaces.iter().find(|w| w.name == name)
    }

    /// Write the pool as `machines` and `workspaces` (each the whole list,
    /// in order; an entry left out is removed) and return it as now on disk.
    /// Writes nothing when nothing changed. Refuses, writing nothing, if
    /// either list changed on disk since the load or the result wouldn't
    /// load.
    pub fn save(
        &self,
        machines: &[PoolEntry<Machine>],
        workspaces: &[PoolEntry<WorkspaceSpec>],
    ) -> Result<PoolConfig> {
        let current = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) if e.kind() == ErrorKind::NotFound => {
                return Err(PoolConfigError::Conflict {
                    path: self.path.clone(),
                })
            }
            Err(e) => return Err(file_err(&self.path, e)),
        };
        let raw_then = yaml_edit::parse(&self.text).map_err(|e| file_err(&self.path, e))?;
        let raw = if current == self.text {
            raw_then
        } else {
            let raw_now = yaml_edit::parse(&current).map_err(|e| file_err(&self.path, e))?;
            if pool_values(&raw_now) != pool_values(&raw_then) {
                return Err(PoolConfigError::Conflict {
                    path: self.path.clone(),
                });
            }
            raw_now
        };
        let target = self.target(&raw, machines, workspaces)?;
        if target == raw {
            return PoolConfig::load(&self.project);
        }
        let patched = yaml_edit::patch(&current, &target).map_err(|e| file_err(&self.path, e))?;
        parse_project(
            &self.project,
            &self.path,
            &patched.text,
            self.shared_path.as_deref(),
        )?
        .validate_workspaces()
        .map_err(invalid)?;
        crate::atomic_write(&self.path, patched.text.as_bytes())
            .map_err(|e| file_err(&self.path, e))?;
        PoolConfig::load(&self.project)
    }

    /// `raw` with the two lists replaced by the edit, each entry merged onto
    /// its raw form so the keys the typed model doesn't know survive.
    fn target(
        &self,
        raw: &Value,
        machines: &[PoolEntry<Machine>],
        workspaces: &[PoolEntry<WorkspaceSpec>],
    ) -> Result<Value> {
        let map = raw
            .as_mapping()
            .ok_or_else(|| file_err(&self.path, "isn't a mapping"))?;
        let mut out = map.clone();

        let raw_machines = sequence(map.get("machines"));
        let new_machines = merge_list(&raw_machines, &self.machines, machines, &[])?;
        if new_machines != raw_machines || !map.contains_key("machines") {
            out.insert("machines".into(), Value::Sequence(new_machines));
        }

        let legacy_key = !map.contains_key("workspaces") && map.contains_key("workers");
        let key = if legacy_key { "workers" } else { "workspaces" };
        let raw_workspaces = sequence(map.get(key));
        let new_workspaces = merge_list(
            &raw_workspaces,
            &self.workspaces,
            workspaces,
            LEGACY_WORKSPACE_KEYS,
        )?;
        if new_workspaces != raw_workspaces {
            let value = Value::Sequence(new_workspaces);
            if legacy_key {
                // Rewritten under the current key, in the legacy key's place.
                out = out
                    .into_iter()
                    .map(|(k, v)| {
                        if k.as_str() == Some("workers") {
                            ("workspaces".into(), value.clone())
                        } else {
                            (k, v)
                        }
                    })
                    .collect();
            } else {
                out.insert("workspaces".into(), value);
            }
        }
        Ok(Value::Mapping(out))
    }
}

/// The two lists as written (`workers:` standing in for `workspaces:`).
fn pool_values(raw: &Value) -> (Option<&Value>, Option<&Value>) {
    (
        raw.get("machines"),
        raw.get("workspaces").or_else(|| raw.get("workers")),
    )
}

fn sequence(v: Option<&Value>) -> Vec<Value> {
    match v {
        Some(Value::Sequence(items)) => items.clone(),
        _ => Vec::new(),
    }
}

/// The new list: each kept entry merged onto its raw item (unchanged ones
/// verbatim), new entries serialized.
fn merge_list<T: Serialize + Named>(
    raw: &[Value],
    base: &[T],
    entries: &[PoolEntry<T>],
    legacy_keys: &[&str],
) -> Result<Vec<Value>> {
    entries
        .iter()
        .map(|entry| {
            let new = to_value(&entry.value)?;
            let Some(was) = &entry.was else {
                return Ok(new);
            };
            let old = base.iter().find(|b| b.name() == was).ok_or_else(|| {
                PoolConfigError::Invalid(format!("no `{was}` in the loaded config"))
            })?;
            let old = to_value(old)?;
            let raw_item = raw
                .iter()
                .find(|v| v.get("name").and_then(Value::as_str) == Some(was.as_str()))
                .cloned()
                .unwrap_or_else(|| old.clone());
            if old == new {
                return Ok(raw_item);
            }
            let mut merged = yaml_edit::merge(&raw_item, &old, &new, ALIASES);
            if let Value::Mapping(m) = &mut merged {
                for key in legacy_keys {
                    m.shift_remove(*key);
                }
            }
            Ok(merged)
        })
        .collect()
}

fn to_value(v: &impl Serialize) -> Result<Value> {
    serde_yaml::to_value(v).map_err(|e| PoolConfigError::Invalid(e.to_string()))
}

/// Parse the pool file (with its shared half, in-repo) as a whole project.
fn parse_project(
    project: &str,
    path: &Path,
    text: &str,
    shared_path: Option<&Path>,
) -> Result<Project> {
    let mut p = match shared_path {
        Some(shared) => {
            let shared_text = read(shared)?;
            Project::from_split_yaml_str(&shared_text, text)
        }
        None => Project::from_yaml_str(text),
    }
    .map_err(|e| file_err(path, e))?;
    p.name = project.to_string();
    Ok(p)
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).map_err(|e| file_err(path, e))
}

fn invalid(e: shelbi_core::Error) -> PoolConfigError {
    PoolConfigError::Invalid(e.to_string())
}

fn file_err(path: &Path, e: impl std::fmt::Display) -> PoolConfigError {
    PoolConfigError::File {
        path: path.to_path_buf(),
        message: e.to_string(),
    }
}

#[cfg(test)]
mod tests;
