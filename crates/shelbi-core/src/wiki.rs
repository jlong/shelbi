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
//!
//! An unknown `source:` or `sync:` fails the block's parse (and so the
//! project's, as any unknown word in the project YAML does) with a message
//! that lists the choices. The branch and path are checked by
//! [`WikiConfig::validate`] (a branch git takes, a relative folder inside
//! the repo), which the hub runs before it sets the wiki up, Settings ▸ Wiki
//! and `shelbi wiki init` run before they write the block, and `shelbi
//! config lint` reports; like `releases:`, a bad value never fails the
//! project's load.

use serde::{Deserialize, Deserializer, Serialize};

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

    /// The block's branch and path as git and the repo take them, or what's
    /// wrong, naming the field (`wiki.path: …`). Both are checked whichever
    /// the source, so switching it never lands on a bad value.
    pub fn validate(&self) -> crate::Result<()> {
        if let Some(branch) = &self.branch {
            check_branch(branch).map_err(|e| crate::Error::InvalidWiki(format!("wiki.branch `{branch}`: {e}")))?;
        }
        if let Some(path) = &self.path {
            check_path(path).map_err(|e| crate::Error::InvalidWiki(format!("wiki.path `{path}`: {e}")))?;
        }
        Ok(())
    }
}

/// `branch` is a branch name git takes (`git check-ref-format --branch`'s
/// rules) and isn't [`WIKI_DOCS_BRANCH`], which folder mode owns.
pub fn check_branch(branch: &str) -> Result<(), String> {
    let b = branch;
    if b.trim().is_empty() {
        return Err("name a branch (or leave it out for docs/planning)".into());
    }
    if b.trim() != b {
        return Err("a branch name can't start or end with a space".into());
    }
    if b.starts_with('-') {
        return Err("a branch name can't start with `-`".into());
    }
    if b.starts_with('/') || b.ends_with('/') || b.contains("//") {
        return Err("a branch name can't start or end with `/`, or have `//`".into());
    }
    if b.contains("..") || b.contains("@{") || b == "@" {
        return Err("a branch name can't have `..` or `@{`, or be `@`".into());
    }
    if b.ends_with('.') || b.ends_with(".lock") {
        return Err("a branch name can't end with `.` or `.lock`".into());
    }
    if let Some(c) = b.chars().find(|c| c.is_control() || " ~^:?*[\\".contains(*c)) {
        return Err(if c.is_control() {
            "a branch name can't have control characters".into()
        } else {
            format!("a branch name can't have `{c}`")
        });
    }
    if b.split('/').any(|part| part.starts_with('.')) {
        return Err("no part of a branch name can start with `.`".into());
    }
    if b == WIKI_DOCS_BRANCH {
        return Err(format!("`{WIKI_DOCS_BRANCH}` is the branch folder mode commits to; pick another"));
    }
    Ok(())
}

/// `path` names a folder inside the repo: relative, not the repo's root,
/// never out of it through `..`, and not inside `.git`. An empty path is
/// the default ([`DEFAULT_WIKI_PATH`]).
pub fn check_path(path: &str) -> Result<(), String> {
    let p = path.trim();
    if p.is_empty() {
        return Ok(());
    }
    let windows_drive = p.len() >= 2 && p.as_bytes()[1] == b':' && p.as_bytes()[0].is_ascii_alphabetic();
    if p.starts_with('/') || p.starts_with('\\') || p.starts_with('~') || windows_drive {
        return Err("use a folder relative to the repo's root (like `docs`), not an absolute path".into());
    }
    let parts: Vec<&str> = p.split(['/', '\\']).filter(|s| !s.is_empty() && *s != ".").collect();
    if parts.contains(&"..") {
        return Err("the folder must be inside the repo; `..` leads out of it".into());
    }
    if parts.is_empty() {
        return Err("name a folder inside the repo, not the repo's root".into());
    }
    if parts[0] == ".git" {
        return Err("the folder can't be inside `.git`".into());
    }
    Ok(())
}

/// Where the wiki's documents live.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiSource {
    /// An orphan branch of the project repo.
    #[default]
    Branch,
    /// A folder of the main repo, edited through a rolling PR branch.
    Folder,
}

impl WikiSource {
    pub const ALL: [WikiSource; 2] = [WikiSource::Branch, WikiSource::Folder];

    /// Its word in the YAML.
    pub fn as_str(self) -> &'static str {
        match self {
            WikiSource::Branch => "branch",
            WikiSource::Folder => "folder",
        }
    }
}

impl std::str::FromStr for WikiSource {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "branch" => Ok(WikiSource::Branch),
            "folder" => Ok(WikiSource::Folder),
            _ => Err(format!(
                "unknown wiki source `{s}`: use `branch` (an orphan branch of the repo) or `folder` (a folder of the repo)"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for WikiSource {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let word = String::deserialize(d)?;
        word.parse().map_err(serde::de::Error::custom)
    }
}

/// Whether the hub commits, pulls and pushes on its own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WikiSyncMode {
    /// Commit a few seconds after the last change, pull and push on a timer.
    #[default]
    Auto,
    /// Only when asked (`shelbi wiki sync now`).
    Manual,
}

impl WikiSyncMode {
    /// Its word in the YAML.
    pub fn as_str(self) -> &'static str {
        match self {
            WikiSyncMode::Auto => "auto",
            WikiSyncMode::Manual => "manual",
        }
    }
}

impl std::str::FromStr for WikiSyncMode {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "auto" => Ok(WikiSyncMode::Auto),
            "manual" => Ok(WikiSyncMode::Manual),
            _ => Err(format!(
                "unknown wiki sync `{s}`: use `auto` (Shelbi commits, pulls and pushes on its own) or `manual`"
            )),
        }
    }
}

impl<'de> Deserialize<'de> for WikiSyncMode {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let word = String::deserialize(d)?;
        word.parse().map_err(serde::de::Error::custom)
    }
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

    #[test]
    fn the_block_round_trips() {
        let cfg = WikiConfig {
            source: WikiSource::Folder,
            branch: Some("wiki".into()),
            path: Some("handbook".into()),
            sync: WikiSyncMode::Manual,
        };
        let text = yaml_serde::to_string(&cfg).unwrap();
        assert_eq!(text, "source: folder\nbranch: wiki\npath: handbook\nsync: manual\n");
        let back: WikiConfig = yaml_serde::from_str(&text).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn an_unknown_source_or_sync_names_the_choices() {
        let err = yaml_serde::from_str::<WikiConfig>("source: git\n").unwrap_err().to_string();
        assert!(err.contains("unknown wiki source `git`"), "{err}");
        assert!(err.contains("`branch`") && err.contains("`folder`"), "{err}");
        let err = yaml_serde::from_str::<WikiConfig>("sync: sometimes\n").unwrap_err().to_string();
        assert!(err.contains("unknown wiki sync `sometimes`") && err.contains("`manual`"), "{err}");
        // The words read in any case.
        let cfg: WikiConfig = yaml_serde::from_str("source: Folder\nsync: MANUAL\n").unwrap();
        assert_eq!((cfg.source, cfg.sync), (WikiSource::Folder, WikiSyncMode::Manual));
    }

    #[test]
    fn a_path_must_be_a_folder_inside_the_repo() {
        let with_path = |p: &str| WikiConfig {
            source: WikiSource::Folder,
            path: Some(p.into()),
            ..WikiConfig::default()
        };
        for ok in ["docs", "docs/handbook", "./docs", "docs/", ""] {
            assert!(with_path(ok).validate().is_ok(), "{ok}");
        }
        for (bad, says) in [
            ("/etc/docs", "absolute"),
            ("~/docs", "absolute"),
            ("C:\\docs", "absolute"),
            ("../elsewhere", "inside the repo"),
            ("docs/../../x", "inside the repo"),
            (".", "root"),
            ("./", "root"),
            (".git/wiki", ".git"),
        ] {
            let err = with_path(bad).validate().unwrap_err().to_string();
            assert!(err.starts_with(&format!("wiki.path `{bad}`")), "{err}");
            assert!(err.contains(says), "{bad}: {err}");
        }
    }

    #[test]
    fn a_branch_must_be_one_git_takes() {
        let with_branch = |b: &str| WikiConfig {
            branch: Some(b.into()),
            ..WikiConfig::default()
        };
        for ok in ["docs/planning", "wiki", "team/docs-2026"] {
            assert!(with_branch(ok).validate().is_ok(), "{ok}");
        }
        for bad in [
            "", "  ", "has space", "-x", "/x", "x/", "a//b", "a..b", "x.lock", "x.", "a:b", "a~b", "a^b", "a?b",
            "a*b", "a[b", ".hidden", "a/.b", "@", "a@{b", WIKI_DOCS_BRANCH,
        ] {
            let err = with_branch(bad).validate().unwrap_err().to_string();
            assert!(err.starts_with("wiki.branch `"), "{bad}: {err}");
        }
    }
}
