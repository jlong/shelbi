//! A config save as data: what it expects to find, and what it writes.
//!
//! Settings saves a project's workflows ([`crate::workflow_config`]) and
//! agents ([`crate::agents_config`]) through comment-preserving patchers.
//! Each save is planned first, into a [`Plan`]:
//!
//! - **checks**, one per file or directory the save touches: the content
//!   hash it had when the model was loaded ([`content_hash`],
//!   [`dir_hash`]), or that it doesn't exist;
//! - **ops**, in the order they run: write a file (atomically), remove a
//!   file or a directory, rename a directory, make one.
//!
//! [`check_and_run`] refuses, writing nothing, if any check fails
//! ([`WriteError::Changed`], someone else edited the file), then runs the
//! ops. That's the whole save on this machine.
//!
//! The plan is serializable so a save can run somewhere else: a window on a
//! project on another machine builds the plan with its own patcher and
//! sends it to that machine's daemon (`SaveConfig` on the control socket),
//! whose [`apply`] first [`validate`]s it (every path inside the project's
//! config for the plan's [`Surface`], every YAML it writes re-parsed with
//! this binary's loader), then checks and runs it. A local window sends its
//! saves the same way, so the daemon stays the one writer.

use std::collections::BTreeMap;
use std::fs;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Which part of a project's config a plan writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    /// The status catalog, the workflow files and the project YAML's
    /// `default_workflow:` (Settings ▸ Workflows and the workflow editor).
    Workflows,
    /// The agent directories and the shared preamble (Settings ▸ Agents and
    /// the agent editor).
    Agents,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Workflows => "workflows",
            Surface::Agents => "agents",
        }
    }
}

/// What a save expects to find before it writes anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Check {
    /// The file's bytes hash to `hash` ([`content_hash`]); `None`: there's
    /// nothing at `path`.
    File { path: PathBuf, hash: Option<String> },
    /// The directory's files hash to `hash` ([`dir_hash`]); `None`: there's
    /// nothing at `path`.
    Dir { path: PathBuf, hash: Option<String> },
}

impl Check {
    pub fn path(&self) -> &Path {
        match self {
            Check::File { path, .. } | Check::Dir { path, .. } => path,
        }
    }
}

/// A file's new contents: text, or bytes that aren't UTF-8 (a skill's
/// image, say).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Contents {
    Text(String),
    Bytes(Vec<u8>),
}

impl Contents {
    pub fn from_bytes(bytes: Vec<u8>) -> Contents {
        match String::from_utf8(bytes) {
            Ok(text) => Contents::Text(text),
            Err(e) => Contents::Bytes(e.into_bytes()),
        }
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Contents::Text(t) => t.as_bytes(),
            Contents::Bytes(b) => b,
        }
    }
}

/// Where a removed file's emptied parent directories stop being pruned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prune {
    /// Never pruned, nor anything outside it.
    pub root: PathBuf,
    /// Directories under `root` that stay even when empty.
    #[serde(default)]
    pub keep: Vec<PathBuf>,
}

/// One step of a save, run in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Write `contents` to `path` atomically, making its directory.
    Write { path: PathBuf, contents: Contents },
    /// Remove the file at `path` (fine if it's gone), then its parents
    /// that it leaves empty, as `prune` allows.
    RemoveFile {
        path: PathBuf,
        #[serde(default)]
        prune: Option<Prune>,
    },
    /// Remove the directory at `path` and everything in it.
    RemoveDir { path: PathBuf },
    /// Move `from` to `to`.
    Rename { from: PathBuf, to: PathBuf },
    /// Make the directory at `path` (and its parents).
    EnsureDir { path: PathBuf },
}

impl Op {
    /// Every path the op touches.
    pub fn paths(&self) -> Vec<&Path> {
        match self {
            Op::Write { path, .. }
            | Op::RemoveFile { path, .. }
            | Op::RemoveDir { path }
            | Op::EnsureDir { path } => vec![path],
            Op::Rename { from, to } => vec![from, to],
        }
    }
}

/// A whole save: what it expects, then what it does.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Plan {
    pub surface: Surface,
    #[serde(default)]
    pub checks: Vec<Check>,
    #[serde(default)]
    pub ops: Vec<Op>,
}

impl Plan {
    pub fn new(surface: Surface) -> Plan {
        Plan {
            surface,
            checks: Vec::new(),
            ops: Vec::new(),
        }
    }

    /// Whether the plan writes nothing.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }
}

/// Why a plan didn't run.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WriteError {
    /// These changed since the model was loaded (a check failed). Nothing
    /// was written.
    #[error("changed since it was loaded: {}", display_paths(.0))]
    Changed(Vec<PathBuf>),
    /// The plan isn't one this machine writes: a path outside the project's
    /// config for its surface, or a file its loader can't read. Nothing was
    /// written.
    #[error("{}: {message}", .path.display())]
    Refused { path: PathBuf, message: String },
    /// A step failed. The steps before it stay done.
    #[error("{}: {message}", .path.display())]
    Io { path: PathBuf, message: String },
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---------------------------------------------------------------------------
// Hashes

/// A file's content hash as a check carries it: 64-bit FNV-1a, in hex. Not
/// a security measure: it only tells "the file I loaded" from "a file
/// someone changed since", and both ends run the same build.
pub fn content_hash(bytes: &[u8]) -> String {
    let mut hash = Fnv::new();
    hash.write(bytes);
    hash.hex()
}

/// A directory's files' hash, from a [`snapshot`] of it: every relative
/// path and its bytes, in path order.
pub fn dir_hash(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut hash = Fnv::new();
    for (rel, bytes) in files {
        hash.write(&(rel.len() as u64).to_be_bytes());
        hash.write(rel.as_bytes());
        hash.write(&(bytes.len() as u64).to_be_bytes());
        hash.write(bytes);
    }
    hash.hex()
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Fnv {
        Fnv(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for b in bytes {
            self.0 ^= u64::from(*b);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Deeper than any real agent tree; stops a symlink loop.
const MAX_DEPTH: usize = 16;

/// Every file under `dir` (following symlinks), by `/`-separated relative
/// path. Empty when `dir` doesn't exist.
pub fn snapshot(dir: &Path) -> std::io::Result<BTreeMap<String, Vec<u8>>> {
    let mut out = BTreeMap::new();
    if dir.is_dir() {
        walk(dir, "", 0, &mut out)?;
    }
    Ok(out)
}

fn walk(dir: &Path, prefix: &str, depth: usize, out: &mut BTreeMap<String, Vec<u8>>) -> std::io::Result<()> {
    if depth > MAX_DEPTH {
        return Err(std::io::Error::other(format!(
            "{} is nested too deeply (a symlink loop?)",
            dir.display()
        )));
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(name) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        let rel = format!("{prefix}{name}");
        // A dangling symlink has nothing to read; skip it like the mount
        // would fail to.
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            walk(&path, &format!("{rel}/"), depth + 1, out)?;
        } else {
            out.insert(rel, fs::read(&path)?);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Running a plan

/// Check, then run `plan` here, without [`validate`]: a save this process
/// planned from files it read itself.
pub fn check_and_run(plan: &Plan) -> Result<(), WriteError> {
    check(plan)?;
    run(plan)
}

/// Validate, check and run `plan` for `project`: a save planned elsewhere
/// (the daemon's `SaveConfig`).
pub fn apply(project: &str, plan: &Plan) -> Result<(), WriteError> {
    validate(project, plan)?;
    check_and_run(plan)
}

/// Every check that fails: the paths that changed since the plan was made.
pub fn check(plan: &Plan) -> Result<(), WriteError> {
    let mut changed: Vec<PathBuf> = Vec::new();
    for c in &plan.checks {
        let now = match c {
            Check::File { path, .. } => match fs::read(path) {
                Ok(bytes) => Ok(Some(content_hash(&bytes))),
                Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
                Err(e) => Err(e),
            },
            Check::Dir { path, .. } => {
                if path.exists() {
                    snapshot(path).map(|files| Some(dir_hash(&files)))
                } else {
                    Ok(None)
                }
            }
        };
        let expected = match c {
            Check::File { hash, .. } | Check::Dir { hash, .. } => hash,
        };
        // A path that can't be read counts as changed: it isn't what the
        // plan saw.
        if now.ok().as_ref() != Some(expected) && !changed.iter().any(|p| p == c.path()) {
            changed.push(c.path().to_path_buf());
        }
    }
    if changed.is_empty() {
        Ok(())
    } else {
        Err(WriteError::Changed(changed))
    }
}

/// Run `plan`'s ops in order. Stops at the first that fails.
pub fn run(plan: &Plan) -> Result<(), WriteError> {
    let io = |path: &Path, e: &dyn std::fmt::Display| WriteError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    for op in &plan.ops {
        match op {
            Op::Write { path, contents } => {
                crate::atomic_write(path, contents.as_bytes()).map_err(|e| io(path, &e))?;
            }
            Op::RemoveFile { path, prune } => {
                match fs::remove_file(path) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(io(path, &e)),
                }
                if let Some(prune) = prune {
                    let mut dir = path.parent();
                    while let Some(d) = dir {
                        if d == prune.root || prune.keep.iter().any(|k| k == d) || !d.starts_with(&prune.root) {
                            break;
                        }
                        if fs::remove_dir(d).is_err() {
                            break;
                        }
                        dir = d.parent();
                    }
                }
            }
            Op::RemoveDir { path } => match fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(io(path, &e)),
            },
            Op::Rename { from, to } => fs::rename(from, to).map_err(|e| io(to, &e))?,
            Op::EnsureDir { path } => crate::ensure_dir(path).map_err(|e| io(path, &e))?,
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Validating a plan made elsewhere

/// The directories (and single files) `surface` of `project` may write.
fn roots(project: &str, surface: Surface) -> Result<(Vec<PathBuf>, Vec<PathBuf>), String> {
    match surface {
        Surface::Workflows => {
            let dir = crate::workflows_dir(project).map_err(|e| e.to_string())?;
            let project_file = crate::workflow_config::project_config_file(project).map_err(|e| e.to_string())?;
            Ok((vec![dir], vec![project_file]))
        }
        Surface::Agents => Ok((vec![crate::agents_dir(project).map_err(|e| e.to_string())?], Vec::new())),
    }
}

/// Refuse a plan this machine won't write for `project`: one with a path
/// that isn't absolute, climbs (`..`), or lies outside the surface's config
/// (the workflows directory and the project YAML, or the agents
/// directory); or one that writes YAML this binary's loader can't read (a
/// status catalog, a workflow, the project YAML, an `agent.yaml`). Checks
/// only read, but stay inside the same config.
pub fn validate(project: &str, plan: &Plan) -> Result<(), WriteError> {
    let refused = |path: &Path, message: String| WriteError::Refused {
        path: path.to_path_buf(),
        message,
    };
    let (dirs, files) = roots(project, plan.surface).map_err(|e| refused(Path::new(project), e))?;
    let inside = |path: &Path| -> Result<(), WriteError> {
        if !path.is_absolute() || path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
            return Err(refused(path, "isn't a plain absolute path".into()));
        }
        let ok = files.iter().any(|f| f == path) || dirs.iter().any(|d| path.starts_with(d) && path != d);
        if ok {
            Ok(())
        } else {
            Err(refused(
                path,
                format!("is outside project `{project}`'s {} config", plan.surface.as_str()),
            ))
        }
    };
    for c in &plan.checks {
        inside(c.path())?;
    }
    for op in &plan.ops {
        for path in op.paths() {
            inside(path)?;
        }
        if let Op::Write { path, contents } = op {
            parse(plan.surface, path, &files, contents).map_err(|e| refused(path, e))?;
        }
    }
    Ok(())
}

/// Re-read a file a plan writes with the loader that will read it.
fn parse(surface: Surface, path: &Path, project_files: &[PathBuf], contents: &Contents) -> Result<(), String> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    let yaml = name.ends_with(".yaml") || name.ends_with(".yml");
    let text = || match contents {
        Contents::Text(t) => Ok(t.as_str()),
        Contents::Bytes(_) => Err("isn't text".to_string()),
    };
    match surface {
        Surface::Workflows => {
            if project_files.iter().any(|f| f == path) {
                let value = crate::yaml_edit::parse(text()?).map_err(|e| e.to_string())?;
                if !value.is_mapping() {
                    return Err("isn't a mapping".into());
                }
            } else if !yaml {
                return Err("isn't a YAML file".into());
            } else if name == "statuses.yaml" || name == "statuses.yml" {
                shelbi_core::ProjectStatuses::from_yaml_str(text()?).map_err(|e| e.to_string())?;
            } else {
                shelbi_core::Workflow::from_yaml_str(text()?).map_err(|e| e.to_string())?;
            }
        }
        Surface::Agents => {
            if name == "agent.yaml" {
                shelbi_core::AgentManifest::from_yaml_str(text()?).map_err(|e| e.to_string())?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("shelbi-config-write-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn hashes_are_stable_and_tell_contents_apart() {
        assert_eq!(content_hash(b""), "cbf29ce484222325");
        assert_eq!(content_hash(b"a"), content_hash(b"a"));
        assert_ne!(content_hash(b"a"), content_hash(b"b"));
        let mut files = BTreeMap::new();
        files.insert("a".to_string(), b"bc".to_vec());
        let mut moved = BTreeMap::new();
        moved.insert("ab".to_string(), b"c".to_vec());
        assert_ne!(dir_hash(&files), dir_hash(&moved), "a path and its bytes don't run together");
    }

    #[test]
    fn a_changed_file_refuses_the_whole_plan_and_writes_nothing() {
        let d = dir("refuse");
        let a = d.join("a.yaml");
        let b = d.join("b.yaml");
        fs::write(&a, "a: 1\n").unwrap();
        let plan = Plan {
            surface: Surface::Workflows,
            checks: vec![
                Check::File {
                    path: a.clone(),
                    hash: Some(content_hash(b"a: 0\n")),
                },
                Check::File { path: b.clone(), hash: None },
            ],
            ops: vec![
                Op::Write {
                    path: a.clone(),
                    contents: Contents::Text("a: 2\n".into()),
                },
                Op::Write {
                    path: b.clone(),
                    contents: Contents::Text("b: 2\n".into()),
                },
            ],
        };
        assert_eq!(check_and_run(&plan), Err(WriteError::Changed(vec![a.clone()])));
        assert_eq!(fs::read_to_string(&a).unwrap(), "a: 1\n");
        assert!(!b.exists());

        let plan = Plan {
            checks: vec![Check::File {
                path: a.clone(),
                hash: Some(content_hash(b"a: 1\n")),
            }],
            ..plan
        };
        check_and_run(&plan).unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "a: 2\n");
        assert_eq!(fs::read_to_string(&b).unwrap(), "b: 2\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn directory_checks_and_ops() {
        let d = dir("dirs");
        let agent = d.join("dev");
        fs::create_dir_all(agent.join("skills/x")).unwrap();
        fs::write(agent.join("skills/x/SKILL.md"), "x").unwrap();
        let files = snapshot(&agent).unwrap();
        let plan = Plan {
            surface: Surface::Agents,
            checks: vec![
                Check::Dir {
                    path: agent.clone(),
                    hash: Some(dir_hash(&files)),
                },
                Check::Dir {
                    path: d.join("new"),
                    hash: None,
                },
            ],
            ops: vec![
                Op::RemoveFile {
                    path: agent.join("skills/x/SKILL.md"),
                    prune: Some(Prune {
                        root: agent.clone(),
                        keep: vec![agent.join("skills")],
                    }),
                },
                Op::Rename {
                    from: agent.clone(),
                    to: d.join("new"),
                },
            ],
        };
        check_and_run(&plan).unwrap();
        assert!(d.join("new/skills").is_dir(), "the kept directory stays");
        assert!(!d.join("new/skills/x").exists(), "the emptied one goes");
        assert_eq!(check(&plan), Err(WriteError::Changed(vec![agent.clone(), d.join("new")])));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn plans_cross_the_wire() {
        let plan = Plan {
            surface: Surface::Agents,
            checks: vec![Check::Dir {
                path: "/p/agents/dev".into(),
                hash: None,
            }],
            ops: vec![
                Op::Write {
                    path: "/p/agents/dev/agent.yaml".into(),
                    contents: Contents::Text("name: dev\n".into()),
                },
                Op::Write {
                    path: "/p/agents/dev/skills/logo.png".into(),
                    contents: Contents::from_bytes(vec![0x89, b'P', 0xff]),
                },
                Op::EnsureDir {
                    path: "/p/agents/dev/skills".into(),
                },
            ],
        };
        let json = serde_json::to_string(&plan).unwrap();
        assert_eq!(serde_json::from_str::<Plan>(&json).unwrap(), plan);
        assert!(matches!(Contents::from_bytes(b"ok".to_vec()), Contents::Text(_)));
    }
}
