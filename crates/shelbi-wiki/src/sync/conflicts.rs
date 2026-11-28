//! The files a pull couldn't merge, kept on disk so the hold survives a hub
//! restart and the conflict inspector can show every side.
//!
//! A conflicted file is **held**: the merge commit takes the remote's
//! version (so pushing never overwrites someone else's edit), the worktree
//! keeps the local one, and the sync leaves the file out of every commit
//! until it is resolved. This store remembers each held file's three sides.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One held file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// Wiki-relative path.
    pub path: String,
    /// The common ancestor's content; `None` when both sides added the file.
    #[serde(default)]
    pub base: Option<String>,
    /// The local content when the conflict was found; `None` when the local
    /// side deleted the file.
    #[serde(default)]
    pub ours: Option<String>,
    /// The remote's content (what is committed now); `None` when the remote
    /// deleted it.
    #[serde(default)]
    pub theirs: Option<String>,
    /// When it was found, RFC3339.
    pub detected_at: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    conflicts: Vec<Conflict>,
}

/// The held files of one wiki, backed by a JSON file.
#[derive(Debug)]
pub struct ConflictStore {
    path: PathBuf,
    conflicts: Vec<Conflict>,
}

impl ConflictStore {
    /// The store at `path`; empty when the file is missing or unreadable
    /// (an unreadable store loses the hold, never the documents).
    pub fn load(path: &Path) -> Self {
        let conflicts = std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<File>(&b).ok())
            .map(|f| f.conflicts)
            .unwrap_or_default();
        Self {
            path: path.to_path_buf(),
            conflicts,
        }
    }

    pub fn all(&self) -> &[Conflict] {
        &self.conflicts
    }

    pub fn get(&self, path: &str) -> Option<&Conflict> {
        self.conflicts.iter().find(|c| c.path == path)
    }

    pub fn is_held(&self, path: &str) -> bool {
        self.get(path).is_some()
    }

    pub fn paths(&self) -> Vec<String> {
        self.conflicts.iter().map(|c| c.path.clone()).collect()
    }

    /// Hold `conflict`, replacing an earlier record of the same path but
    /// keeping its base and local side (the first divergence is the one the
    /// inspector resolves).
    pub fn hold(&mut self, conflict: Conflict) -> std::io::Result<()> {
        match self.conflicts.iter_mut().find(|c| c.path == conflict.path) {
            Some(existing) => existing.theirs = conflict.theirs,
            None => self.conflicts.push(conflict),
        }
        self.save()
    }

    /// Record the remote's newer content for a file already held.
    pub fn update_theirs(&mut self, path: &str, theirs: Option<String>) -> std::io::Result<()> {
        if let Some(c) = self.conflicts.iter_mut().find(|c| c.path == path) {
            if c.theirs != theirs {
                c.theirs = theirs;
                return self.save();
            }
        }
        Ok(())
    }

    /// Stop holding `path`. Answers whether it was held.
    pub fn release(&mut self, path: &str) -> std::io::Result<bool> {
        let before = self.conflicts.len();
        self.conflicts.retain(|c| c.path != path);
        if self.conflicts.len() == before {
            return Ok(false);
        }
        self.save()?;
        Ok(true)
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let file = File {
            conflicts: self.conflicts.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&file).map_err(std::io::Error::other)?;
        crate::fs::paths::write_atomic(&self.path, &bytes).map_err(std::io::Error::other)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conflict(path: &str, theirs: &str) -> Conflict {
        Conflict {
            path: path.into(),
            base: Some("base".into()),
            ours: Some("ours".into()),
            theirs: Some(theirs.into()),
            detected_at: "2026-11-27T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_hold_survives_a_reload_and_releases() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("state").join("conflicts.json");
        let mut store = ConflictStore::load(&file);
        store.hold(conflict("Plans/a.md", "one")).unwrap();
        store.hold(conflict("Plans/a.md", "two")).unwrap();
        let reloaded = ConflictStore::load(&file);
        assert_eq!(reloaded.paths(), vec!["Plans/a.md".to_string()]);
        assert_eq!(reloaded.get("Plans/a.md").unwrap().theirs.as_deref(), Some("two"));
        let mut store = reloaded;
        assert!(store.release("Plans/a.md").unwrap());
        assert!(!store.release("Plans/a.md").unwrap());
        assert!(ConflictStore::load(&file).all().is_empty());
    }
}
