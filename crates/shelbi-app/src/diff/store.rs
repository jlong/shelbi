//! Where a task's comments and viewed marks live on disk: the project's state
//! directory (`~/.shelbi/projects/<project>/diff/<key>/`), never a worktree
//! and never `.claude/`.
//!
//! A write is a read-modify-write under a file lock, so two clients on one
//! task, and an agent replying from the CLI, don't lose each other's changes.
//! A file that doesn't parse is refused rather than overwritten: it holds a
//! person's comments.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::comments::{CommentDoc, CommentOp};

const COMMENTS_FILE: &str = "comments.json";
const VIEWED_FILE: &str = "viewed.json";
const LOCK_FILE: &str = ".lock";

/// Which files have been viewed: path to the fingerprint it was viewed at. A
/// file is viewed while its current fingerprint is the one here, so changing
/// it clears the mark ([`crate::diff::model::FileDiff::fingerprint`]).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ViewedDoc {
    #[serde(default)]
    pub files: BTreeMap<String, u64>,
}

/// One change to a [`ViewedDoc`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewedOp {
    Mark { path: String, fingerprint: u64 },
    Unmark { path: String },
}

impl ViewedDoc {
    pub fn is_viewed(&self, path: &str, fingerprint: u64) -> bool {
        self.files.get(path) == Some(&fingerprint)
    }

    pub fn apply(&mut self, op: &ViewedOp) {
        match op {
            ViewedOp::Mark { path, fingerprint } => {
                self.files.insert(path.clone(), *fingerprint);
            }
            ViewedOp::Unmark { path } => {
                self.files.remove(path);
            }
        }
    }
}

/// One task's stored review state.
#[derive(Debug, Clone)]
pub struct DiffStore {
    dir: PathBuf,
}

/// The state key for a task id or workspace name: one path component.
pub fn safe_key(key: &str) -> String {
    key.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') { c } else { '_' })
        .collect::<String>()
        .trim_start_matches('.')
        .to_string()
}

impl DiffStore {
    /// The store in `dir` (a test's temp directory; [`Self::for_project`] is
    /// the real one).
    pub fn at(dir: impl Into<PathBuf>) -> DiffStore {
        DiffStore { dir: dir.into() }
    }

    /// `key`'s store in `project`'s state directory.
    pub fn for_project(project: &str, key: &str) -> Result<DiffStore, String> {
        let root = shelbi_state::project_dir(project).map_err(|e| e.to_string())?;
        Ok(DiffStore::at(root.join("diff").join(safe_key(key))))
    }

    fn lock(&self) -> Result<shelbi_state::DiffStateLock, String> {
        shelbi_state::lock_diff_state(&self.dir.join(LOCK_FILE)).map_err(|e| e.to_string())
    }

    fn read<T: DeserializeOwned + Default>(&self, name: &str) -> Result<T, String> {
        read_json(&self.dir.join(name))
    }

    fn write<T: Serialize>(&self, name: &str, value: &T) -> Result<(), String> {
        let text = serde_json::to_string_pretty(value).map_err(|e| e.to_string())?;
        let path = self.dir.join(name);
        shelbi_state::atomic_write(&path, text.as_bytes()).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn comments(&self) -> Result<CommentDoc, String> {
        self.read(COMMENTS_FILE)
    }

    /// Apply `op` to the file as it is now, answering the result.
    pub fn apply_comment(&self, op: &CommentOp) -> Result<CommentDoc, String> {
        let _lock = self.lock()?;
        let mut doc: CommentDoc = self.read(COMMENTS_FILE)?;
        doc.apply(op);
        self.write(COMMENTS_FILE, &doc)?;
        Ok(doc)
    }

    pub fn viewed(&self) -> Result<ViewedDoc, String> {
        self.read(VIEWED_FILE)
    }

    /// Apply `op` to the file as it is now, answering the result.
    pub fn apply_viewed(&self, op: &ViewedOp) -> Result<ViewedDoc, String> {
        let _lock = self.lock()?;
        let mut doc: ViewedDoc = self.read(VIEWED_FILE)?;
        doc.apply(op);
        self.write(VIEWED_FILE, &doc)?;
        Ok(doc)
    }
}

fn read_json<T: DeserializeOwned + Default>(path: &Path) -> Result<T, String> {
    match fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).map_err(|e| format!("{} doesn't parse: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(T::default()),
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::comments::{Anchor, Author, Comment};
    use crate::diff::model::Side;

    fn comment(id: &str) -> Comment {
        Comment {
            id: id.to_string(),
            anchor: Anchor { path: "a.rs".into(), side: Side::New, start: 1, end: 1, code: vec!["x".into()] },
            author: Author::human("jlong"),
            body: "why?".into(),
            created_at: chrono::DateTime::from_timestamp(0, 0).unwrap(),
            replies: Vec::new(),
            sent: false,
        }
    }

    #[test]
    fn comments_and_viewed_marks_persist() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiffStore::at(dir.path().join("task"));
        assert_eq!(store.comments().unwrap(), CommentDoc::default());
        store.apply_comment(&CommentOp::Add(comment("a"))).unwrap();
        store.apply_viewed(&ViewedOp::Mark { path: "a.rs".into(), fingerprint: 7 }).unwrap();

        let reopened = DiffStore::at(dir.path().join("task"));
        assert_eq!(reopened.comments().unwrap().comments[0].id, "a");
        let viewed = reopened.viewed().unwrap();
        assert!(viewed.is_viewed("a.rs", 7));
        assert!(!viewed.is_viewed("a.rs", 8), "changing the file clears the mark");
        reopened.apply_viewed(&ViewedOp::Unmark { path: "a.rs".into() }).unwrap();
        assert!(!reopened.viewed().unwrap().is_viewed("a.rs", 7));
    }

    #[test]
    fn two_writers_keep_each_others_comments() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("task");
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let store = DiffStore::at(path.clone());
                std::thread::spawn(move || store.apply_comment(&CommentOp::Add(comment(&format!("c{i}")))).unwrap())
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(DiffStore::at(path).comments().unwrap().comments.len(), 8);
    }

    #[test]
    fn a_file_that_doesnt_parse_is_left_alone() {
        let dir = tempfile::tempdir().unwrap();
        let store = DiffStore::at(dir.path());
        fs::write(dir.path().join(COMMENTS_FILE), "not json").unwrap();
        assert!(store.comments().unwrap_err().contains("doesn't parse"));
        assert!(store.apply_comment(&CommentOp::Clear).is_err());
        assert_eq!(fs::read_to_string(dir.path().join(COMMENTS_FILE)).unwrap(), "not json");
    }

    #[test]
    fn keys_are_one_path_component() {
        assert_eq!(safe_key("native-diff-view"), "native-diff-view");
        assert_eq!(safe_key("../../etc"), "_.._etc");
    }
}
