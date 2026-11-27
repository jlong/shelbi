//! The wiki's git history and uncommitted change, for `shelbi wiki diff`
//! and `shelbi wiki log`. Shelbi's own code.
//!
//! Both answer wiki-relative paths: in folder mode the worktree's paths
//! carry the wiki's folder, which is stripped.

use super::{Engine, SyncError};
use crate::git::LOCAL_DEADLINE;

/// One commit of [`Engine::log`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitInfo {
    pub sha: String,
    pub author: String,
    /// RFC3339.
    pub date: String,
    pub subject: String,
    /// The wiki-relative files it touched (within the asked-for path).
    pub paths: Vec<String>,
    /// Who made its changes, from its [`super::BY_TRAILER`] lines
    /// (`person`, `agent`); empty for a commit made elsewhere.
    pub by: Vec<String>,
}

impl Engine {
    /// The pathspec for wiki-relative `path` (the whole wiki when `None`).
    fn pathspec(&self, path: Option<&str>) -> String {
        let path = path.map(|p| p.trim_matches('/')).filter(|p| !p.is_empty());
        match path {
            Some(p) => format!(":(literal){}", self.layout.repo_path(p)),
            None => format!(":(literal){}", self.layout.scope()),
        }
    }

    /// The uncommitted change under `path` as a unified diff against the
    /// last commit, new files included. Held (conflicted) files show as
    /// they are on disk. Empty when nothing changed.
    pub fn diff(&self, path: Option<&str>) -> Result<String, SyncError> {
        let spec = self.pathspec(path);
        let relative = self.layout.folder.as_ref().map(|f| format!("--relative={}", f.path));
        let mut args: Vec<&str> = vec!["diff", "--no-color", "--no-ext-diff"];
        if let Some(r) = &relative {
            args.push(r);
        }
        // An unborn branch has nothing to diff against: everything is new.
        let has_head = self.head()?.is_some();
        if has_head {
            args.push("HEAD");
        }
        args.extend(["--", &spec]);
        let mut out = if has_head { self.git.ok(&args)? } else { String::new() };

        let untracked = self
            .git
            .ok(&["ls-files", "-z", "--others", "--exclude-standard", "--", &spec])?;
        // Run from the wiki's root so the new files' paths read
        // wiki-relative, as the tracked ones do.
        let at_root = self.git.at(self.layout.wiki_root());
        for file in crate::git::split_z(&untracked) {
            let Some(rel) = self.layout.wiki_path(file).filter(|f| !f.is_empty()) else {
                continue;
            };
            let args = ["diff", "--no-color", "--no-ext-diff", "--no-index", "--", "/dev/null", &rel];
            // `--no-index` exits 1 when the files differ, which they do.
            let run = at_root.run(&args, LOCAL_DEADLINE)?;
            out.push_str(&run.stdout);
        }
        Ok(out)
    }

    /// The commits that touched `path` (the whole wiki when `None`), newest
    /// first, at most `limit`. A single document's history follows renames.
    pub fn log(&self, path: Option<&str>, limit: usize) -> Result<Vec<CommitInfo>, SyncError> {
        if self.head()?.is_none() {
            return Ok(Vec::new());
        }
        let spec = self.pathspec(path);
        let max = format!("--max-count={}", limit.max(1));
        let follow = path.is_some_and(|p| p.ends_with(".md"));
        let mut args = vec![
            "log",
            "--no-color",
            "--format=%x1e%H%x1f%an%x1f%aI%x1f%s%x1f%(trailers:key=Shelbi-By,valueonly,separator=%x2c)",
            "--name-only",
            "-z",
            &max,
        ];
        if follow {
            args.push("--follow");
        }
        args.extend(["--", &spec]);
        let out = self.git.ok(&args)?;
        let mut commits = Vec::new();
        for record in out.split('\u{1e}').filter(|r| !r.trim_matches(['\0', '\n']).is_empty()) {
            let (header, files) = record.split_once('\n').unwrap_or((record, ""));
            let mut fields = header.trim_end_matches('\0').split('\u{1f}');
            let (Some(sha), Some(author), Some(date), Some(subject)) =
                (fields.next(), fields.next(), fields.next(), fields.next())
            else {
                continue;
            };
            let by = fields
                .next()
                .unwrap_or("")
                .split(',')
                .map(|w| w.trim().to_string())
                .filter(|w| !w.is_empty())
                .collect();
            let paths = files
                .split('\0')
                .map(|f| f.trim_matches('\n'))
                .filter(|f| !f.is_empty())
                .filter_map(|f| self.layout.wiki_path(f))
                .collect();
            commits.push(CommitInfo {
                sha: sha.to_string(),
                author: author.to_string(),
                date: date.to_string(),
                subject: subject.to_string(),
                paths,
                by,
            });
        }
        Ok(commits)
    }
}
