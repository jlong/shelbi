//! Where a project's wiki lives: the optional `wiki:` block of the project
//! YAML.
//!
//! ```yaml
//! wiki:
//!   source: branch        # branch | folder
//!   branch: docs/planning # source: branch
//!   path: docs            # source: folder (relative to the repo root)
//!   sync: auto            # auto | manual
//! ```
//!
//! - **`branch`** (the default): an orphan branch, [`DEFAULT_WIKI_BRANCH`]
//!   unless set, checked out by the hub as its own worktree at
//!   `~/.shelbi/projects/<name>/wiki/`.
//! - **`folder`**: a folder of the main repo, [`DEFAULT_WIKI_PATH`] unless
//!   set. Edits land on the rolling [`WIKI_DOCS_BRANCH`] (its own hub
//!   worktree, at the same place), with one rolling PR.
//!
//! A project with no block has no wiki until one is asked for (the Wiki
//! opened in a client, or `shelbi wiki init`); it then gets `branch` mode.

use serde::{Deserialize, Serialize};

/// The orphan branch a `branch`-mode wiki uses when the block names none.
pub const DEFAULT_WIKI_BRANCH: &str = "docs/planning";

/// The repo folder a `folder`-mode wiki uses when the block names none.
pub const DEFAULT_WIKI_PATH: &str = "docs";

/// The rolling branch a `folder`-mode wiki's edits are committed to.
pub const WIKI_DOCS_BRANCH: &str = "shelbi/wiki-docs";

/// A project's `wiki:` block.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiConfig {
    #[serde(default)]
    pub source: WikiSource,
    /// The orphan branch (`source: branch`). `None` is
    /// [`DEFAULT_WIKI_BRANCH`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The repo folder (`source: folder`), relative to the repo root.
    /// `None` is [`DEFAULT_WIKI_PATH`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub sync: WikiSyncMode,
}

impl WikiConfig {
    /// The branch the wiki's worktree has checked out: the orphan branch in
    /// `branch` mode, the rolling [`WIKI_DOCS_BRANCH`] in `folder` mode.
    pub fn worktree_branch(&self) -> &str {
        match self.source {
            WikiSource::Branch => self.branch.as_deref().unwrap_or(DEFAULT_WIKI_BRANCH),
            WikiSource::Folder => WIKI_DOCS_BRANCH,
        }
    }

    /// The wiki's folder inside the worktree: `None` in `branch` mode (the
    /// whole worktree), the repo folder in `folder` mode.
    pub fn folder(&self) -> Option<&str> {
        match self.source {
            WikiSource::Branch => None,
            WikiSource::Folder => Some(
                self.path
                    .as_deref()
                    .map(|p| p.trim_matches('/'))
                    .filter(|p| !p.is_empty())
                    .unwrap_or(DEFAULT_WIKI_PATH),
            ),
        }
    }
}

/// Where the wiki's documents live.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiSource {
    /// An orphan branch of the project repo.
    #[default]
    Branch,
    /// A folder of the main repo, edited through a rolling PR branch.
    Folder,
}

/// Whether the hub commits, pulls and pushes on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiSyncMode {
    /// Commit a few seconds after the last change, pull and push on a timer.
    #[default]
    Auto,
    /// Only when asked (`shelbi wiki sync now`).
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_block_is_branch_mode_on_docs_planning() {
        let cfg: WikiConfig = yaml_serde::from_str("{}").unwrap();
        assert_eq!(cfg.source, WikiSource::Branch);
        assert_eq!(cfg.sync, WikiSyncMode::Auto);
        assert_eq!(cfg.worktree_branch(), "docs/planning");
        assert_eq!(cfg.folder(), None);
    }

    #[test]
    fn folder_mode_reads_its_path_and_uses_the_rolling_branch() {
        let cfg: WikiConfig = yaml_serde::from_str("source: folder\npath: /handbook/\nsync: manual\n").unwrap();
        assert_eq!(cfg.source, WikiSource::Folder);
        assert_eq!(cfg.sync, WikiSyncMode::Manual);
        assert_eq!(cfg.worktree_branch(), WIKI_DOCS_BRANCH);
        assert_eq!(cfg.folder(), Some("handbook"));
        let default: WikiConfig = yaml_serde::from_str("source: folder\n").unwrap();
        assert_eq!(default.folder(), Some("docs"));
    }

    #[test]
    fn a_named_branch_is_kept() {
        let cfg: WikiConfig = yaml_serde::from_str("branch: wiki\n").unwrap();
        assert_eq!(cfg.worktree_branch(), "wiki");
        let text = yaml_serde::to_string(&cfg).unwrap();
        assert!(!text.contains("path"), "{text}");
    }
}
