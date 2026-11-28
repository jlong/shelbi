//! Git sync for one docs, over the git CLI.
//!
//! Shelbi's own code: it ports the flow of ContextStore's sync engine
//! (`ContextStoreUI/src/sync/engine.rs`: commit, fast-forward or merge with
//! per-file auto-resolution, push) and the orphan-branch bootstrap of
//! `cstore-git`, but runs the user's `git` instead of libgit2.
//!
//! Docs is a git worktree of the project repo that the hub owns
//! (`~/.shelbi/projects/<name>/docs/`):
//!
//! - **branch mode**: the worktree has the docs' orphan branch
//!   (`docs/planning`) checked out, and the whole worktree is the docs.
//! - **folder mode**: the worktree has the rolling `shelbi/wiki-docs` branch
//!   checked out, cut from the base branch; the docs are one folder of it
//!   (`docs/`), and only that folder is committed. Each sync also merges the
//!   base branch in, so the docs shows the base's docs plus the pending edits.
//!
//! [`Engine`] does the git work; the service ([`crate::service`]) decides
//! when. A file a pull can't merge is held (see [`conflicts`]).

pub mod conflicts;
pub mod history;
pub mod message;
pub mod pr;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::git::{split_z, Git, GitError, NETWORK_DEADLINE};
use conflicts::{Conflict, ConflictStore};

/// The remote every docs sync with.
pub const REMOTE: &str = "origin";

/// The commit trailer naming who made a commit's changes (`person`,
/// `agent`), one line each.
pub const BY_TRAILER: &str = "Shelbi-By";

/// The subject of the commit that settles a held file.
pub const RESOLVE_MESSAGE: &str = "Resolve merge conflicts";

/// Why a sync step failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Git(#[from] GitError),
    /// The worktree can't be set up as asked.
    #[error("{0}")]
    Setup(String),
    #[error("{0}")]
    Io(String),
}

impl SyncError {
    /// Whether this is the remote being out of reach.
    pub fn is_offline(&self) -> bool {
        matches!(self, SyncError::Git(e) if e.is_offline())
    }
}

fn io(context: &str, e: impl std::fmt::Display) -> SyncError {
    SyncError::Io(format!("{context}: {e}"))
}

/// Where docs lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    /// The project repo the worktree belongs to (the hub's checkout).
    pub repo: PathBuf,
    /// The docs' worktree.
    pub worktree: PathBuf,
    /// The branch the worktree has checked out.
    pub branch: String,
    /// Folder mode: the docs' folder in the worktree, and the base branch
    /// the rolling branch is cut from and kept merged with.
    pub folder: Option<FolderMode>,
}

/// The folder-mode half of a [`Layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderMode {
    pub path: String,
    pub base: String,
}

impl Layout {
    /// A branch-mode docs: `branch` checked out at `worktree`.
    pub fn branch(repo: impl Into<PathBuf>, worktree: impl Into<PathBuf>, branch: &str) -> Self {
        Self {
            repo: repo.into(),
            worktree: worktree.into(),
            branch: branch.to_string(),
            folder: None,
        }
    }

    /// A folder-mode docs: `path` of the repo, edited on `rolling` (cut from
    /// and merged with `base`).
    pub fn folder(
        repo: impl Into<PathBuf>,
        worktree: impl Into<PathBuf>,
        rolling: &str,
        path: &str,
        base: &str,
    ) -> Self {
        Self {
            repo: repo.into(),
            worktree: worktree.into(),
            branch: rolling.to_string(),
            folder: Some(FolderMode {
                path: path.trim_matches('/').to_string(),
                base: base.to_string(),
            }),
        }
    }

    /// The docs' root: the worktree, or its folder.
    pub fn docs_root(&self) -> PathBuf {
        match &self.folder {
            Some(f) => self.worktree.join(&f.path),
            None => self.worktree.clone(),
        }
    }

    /// The pathspec that covers the docs, as git sees it from the worktree.
    fn scope(&self) -> &str {
        self.folder.as_ref().map_or(".", |f| f.path.as_str())
    }

    /// A worktree-relative path as docs-relative one; `None` outside the
    /// docs' folder.
    pub fn docs_path(&self, repo_path: &str) -> Option<String> {
        match &self.folder {
            None => Some(repo_path.to_string()),
            Some(f) => repo_path
                .strip_prefix(&f.path)
                .and_then(|rest| rest.strip_prefix('/'))
                .map(str::to_string),
        }
    }

    /// Docs-relative path as a worktree-relative one.
    pub fn repo_path(&self, docs_path: &str) -> String {
        match &self.folder {
            None => docs_path.to_string(),
            Some(f) => format!("{}/{}", f.path, docs_path),
        }
    }

    fn remote_ref(branch: &str) -> String {
        format!("refs/remotes/{REMOTE}/{branch}")
    }
}

/// How [`Engine::setup`] found the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Setup {
    /// It was already there.
    Existing,
    /// The branch existed (on origin or locally) and was checked out.
    CheckedOut,
    /// There was no such branch anywhere: it was created as an orphan with
    /// starter files (branch mode), and is pushed by the first sync.
    Created,
}

/// A commit the sync made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Committed {
    pub message: String,
    /// Documentation-relative paths it touched.
    pub paths: Vec<String>,
}

/// What [`Engine::integrate`] brought in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Integrated {
    /// Documentation-relative paths the pulled commits changed.
    pub changed: Vec<String>,
    /// Paths newly held because they couldn't be merged.
    pub conflicts: Vec<String>,
}

/// What [`Engine::fetch`] found on the remote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Fetched {
    /// The docs' branch exists on the remote.
    pub branch: bool,
    /// Folder mode: the base branch exists on the remote.
    pub base: bool,
}

/// What [`Engine::push`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// The remote already had everything.
    Nothing,
    Pushed,
}

/// The git half of one docs' sync. Not thread-safe by itself: the service
/// runs the steps that touch the worktree under one lock, and the network
/// steps ([`Engine::fetch`], [`Engine::push`]) outside it.
#[derive(Debug)]
pub struct Engine {
    layout: Layout,
    git: Git,
    conflicts: ConflictStore,
}

impl Engine {
    /// Set the docs' worktree up (creating it as [`Setup`] describes) and
    /// open it. `name` titles a freshly created docs' starter files.
    /// `conflicts_file` is where held files are remembered; `env` is added
    /// to every git command's environment.
    pub fn setup(
        layout: Layout,
        name: &str,
        conflicts_file: &Path,
        env: &[(String, String)],
    ) -> Result<(Self, Setup), SyncError> {
        let repo_git = Git::new(&layout.repo).with_env(env);
        let outcome = if is_worktree(&layout.worktree) {
            Setup::Existing
        } else {
            clear_empty_dir(&layout.worktree)?;
            if let Some(parent) = layout.worktree.parent() {
                std::fs::create_dir_all(parent).map_err(|e| io("creating the docs' folder", e))?;
            }
            match &layout.folder {
                None => setup_branch(&repo_git, &layout, name, env)?,
                Some(folder) => setup_folder(&repo_git, &layout, folder)?,
            }
        };
        let engine = Self::open(layout, conflicts_file, env)?;
        if let Some(folder) = &engine.layout.folder {
            // The folder may not exist on the base branch yet; the docs
            // starts empty rather than missing.
            let root = engine.layout.worktree.join(&folder.path);
            std::fs::create_dir_all(&root).map_err(|e| io("creating the docs' folder", e))?;
        }
        Ok((engine, outcome))
    }

    /// Open a worktree that is already set up.
    pub fn open(layout: Layout, conflicts_file: &Path, env: &[(String, String)]) -> Result<Self, SyncError> {
        if !is_worktree(&layout.worktree) {
            return Err(SyncError::Setup(format!(
                "the docs at {} aren't set up",
                layout.worktree.display()
            )));
        }
        let mut git = Git::new(&layout.worktree).with_env(env);
        git.ensure_identity();
        let mut engine = Self {
            layout,
            git,
            conflicts: ConflictStore::load(conflicts_file),
        };
        engine.clear_stale_merge()?;
        Ok(engine)
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// The files held for a conflict.
    pub fn conflicts(&self) -> &[Conflict] {
        self.conflicts.all()
    }

    /// A merge left unfinished (the hub stopped in the middle of one) would
    /// make the next commit a merge that took the local side whole. Abort it
    /// and put the held files' local content back; the next pull merges
    /// again.
    fn clear_stale_merge(&mut self) -> Result<(), SyncError> {
        if self.git.run(&["rev-parse", "-q", "--verify", "MERGE_HEAD"], crate::git::LOCAL_DEADLINE)?.success() {
            self.git.ok(&["merge", "--abort"])?;
            for c in self.conflicts.all().to_vec() {
                self.write_worktree(&c.path, c.ours.as_deref())?;
            }
        }
        Ok(())
    }

    /// Stage the docs' changes, except held files, and commit them with a
    /// generated message. `None` when there was nothing to commit.
    pub fn commit_pending(&mut self) -> Result<Option<Committed>, SyncError> {
        self.commit_pending_by(&[])
    }

    /// [`Self::commit_pending`], saying who made the changes: a
    /// [`BY_TRAILER`] line for each of `by` (`person`, `agent`), which
    /// [`Engine::log`] reads back, so a client can say who last edited a
    /// document (every commit is the hub's, in the user's name).
    pub fn commit_pending_by(&mut self, by: &[&str]) -> Result<Option<Committed>, SyncError> {
        let scope = self.layout.scope().to_string();
        if self.layout.folder.is_some() && !self.layout.docs_root().exists() && self.git.ok(&["ls-files", "--", &scope])?.trim().is_empty() {
            return Ok(None);
        }
        let mut add: Vec<String> = vec!["add".into(), "-A".into(), "--".into(), format!(":(literal){scope}")];
        for held in self.conflicts.paths() {
            add.push(format!(":(exclude,literal){}", self.layout.repo_path(&held)));
        }
        let add: Vec<&str> = add.iter().map(String::as_str).collect();
        self.git.ok(&add)?;
        self.commit_staged(None, by)
    }

    /// Commit what is staged; `message` overrides the generated one.
    fn commit_staged(&mut self, message: Option<&str>, by: &[&str]) -> Result<Option<Committed>, SyncError> {
        let staged = self.git.ok(&["diff", "--cached", "--name-status", "-z", "--no-renames"])?;
        let fields = split_z(&staged);
        let mut entries = Vec::new();
        for pair in fields.chunks(2) {
            if let [status, path] = pair {
                entries.push((status.chars().next().unwrap_or('M').to_string(), path.to_string()));
            }
        }
        if entries.is_empty() {
            return Ok(None);
        }
        let message = message.map(str::to_string).unwrap_or_else(|| message::commit_message(&entries));
        let mut full = message.clone();
        if !by.is_empty() {
            full.push('\n');
            for who in by {
                full.push_str(&format!("\n{BY_TRAILER}: {who}"));
            }
        }
        self.git.ok(&["commit", "-q", "--no-verify", "-m", &full])?;
        let paths = entries.iter().filter_map(|(_, p)| self.layout.docs_path(p)).collect();
        Ok(Some(Committed { message, paths }))
    }

    /// The network half of the sync, which runs without the worktree lock.
    pub fn remote(&self) -> Remote {
        Remote {
            git: self.git.clone(),
            layout: self.layout.clone(),
        }
    }

    /// Merge what [`Remote::fetch`] brought in: fast-forward when the local
    /// branch hasn't moved, else a merge whose conflicted Markdown files go
    /// through [`crate::merge::resolve`]. A file that can't be resolved is
    /// held: the merge commit takes the remote's side, the worktree keeps
    /// the local one, and it stays out of commits until resolved.
    pub fn integrate(&mut self, fetched: Fetched) -> Result<Integrated, SyncError> {
        let mut out = Integrated::default();
        let before = self.head()?;
        let mut targets = Vec::new();
        if fetched.branch {
            targets.push(Layout::remote_ref(&self.layout.branch));
        }
        if let (Some(f), true) = (&self.layout.folder, fetched.base) {
            targets.push(Layout::remote_ref(&f.base));
        }
        for target in targets {
            out.conflicts.extend(self.merge_one(&target)?);
        }
        let after = self.head()?;
        if let (Some(before), Some(after)) = (&before, &after) {
            if before != after {
                let changed = self
                    .git
                    .ok(&["diff", "--name-only", "-z", "--no-renames", before, after, "--", self.layout.scope()])?;
                out.changed = split_z(&changed).into_iter().filter_map(|p| self.layout.docs_path(p)).collect();
            }
        }
        for path in &out.conflicts {
            if !out.changed.contains(path) {
                out.changed.push(path.clone());
            }
        }
        Ok(out)
    }

    fn head(&self) -> Result<Option<String>, SyncError> {
        head(&self.git)
    }

    fn is_ancestor(&self, a: &str, b: &str) -> Result<bool, SyncError> {
        let out = self.git.run(&["merge-base", "--is-ancestor", a, b], crate::git::LOCAL_DEADLINE)?;
        match out.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(SyncError::Git(GitError::Failed {
                command: "merge-base".into(),
                code: out.code,
                stderr: out.stderr,
                stdout: out.stdout,
            })),
        }
    }

    /// Merge one ref into the checked-out branch. Answers the paths newly
    /// held.
    fn merge_one(&mut self, target: &str) -> Result<Vec<String>, SyncError> {
        let Some(head) = self.head()? else {
            // An unborn branch (never committed): take the remote's whole.
            self.git.ok(&["reset", "-q", "--keep", target])?;
            return Ok(Vec::new());
        };
        if self.is_ancestor(target, &head)? {
            return Ok(Vec::new());
        }
        // Held files have local content the merge mustn't see (it would
        // refuse to overwrite it). Set it aside and put it back after.
        let stashed = self.stash_held()?;
        let result = self.merge_into_head(target, &head);
        self.unstash_held(stashed)?;
        result
    }

    fn merge_into_head(&mut self, target: &str, head: &str) -> Result<Vec<String>, SyncError> {
        if self.is_ancestor(head, target)? {
            self.git.ok(&["merge", "-q", "--ff-only", target])?;
            return Ok(Vec::new());
        }
        let merged = self.git.run(
            &[
                "merge",
                "-q",
                "--no-ff",
                "--no-edit",
                "--no-verify",
                "--allow-unrelated-histories",
                "-m",
                "Merge remote changes",
                target,
            ],
            crate::git::LOCAL_DEADLINE,
        )?;
        if merged.success() {
            return Ok(Vec::new());
        }
        let unmerged = self.git.ok(&["diff", "--name-only", "--diff-filter=U", "-z"])?;
        let unmerged: Vec<String> = split_z(&unmerged).into_iter().map(str::to_string).collect();
        if unmerged.is_empty() {
            // It failed for another reason; leave the branch as it was.
            let _ = self.git.ok(&["merge", "--abort"]);
            return Err(SyncError::Git(GitError::Failed {
                command: "merge".into(),
                code: merged.code,
                stderr: merged.stderr,
                stdout: merged.stdout,
            }));
        }
        let mut held = Vec::new();
        let mut keep_ours = Vec::new();
        for repo_path in &unmerged {
            let sides = self.sides(repo_path)?;
            let resolved = match (&sides.ours, &sides.theirs) {
                (Some(ours), Some(theirs)) if repo_path.ends_with(".md") => {
                    match crate::merge::resolve(sides.base.as_deref().unwrap_or(""), ours, theirs) {
                        crate::merge::MergeResult::Resolved(content) => Some(content),
                        crate::merge::MergeResult::Conflict => None,
                    }
                }
                _ => None,
            };
            match resolved {
                Some(content) => self.stage_repo_path(repo_path, Some(&content))?,
                None => {
                    // Commit the remote's side; keep ours in the worktree.
                    self.stage_repo_path(repo_path, sides.theirs.as_deref())?;
                    let Some(path) = self.layout.docs_path(repo_path) else {
                        continue;
                    };
                    self.conflicts
                        .hold(Conflict {
                            path: path.clone(),
                            base: sides.base,
                            ours: sides.ours.clone(),
                            theirs: sides.theirs,
                            detected_at: crate::fs::paths::iso8601_now(),
                        })
                        .map_err(|e| io("recording a conflict", e))?;
                    keep_ours.push((path.clone(), sides.ours));
                    held.push(path);
                }
            }
        }
        self.git.ok(&["commit", "-q", "--no-edit", "--no-verify"])?;
        for (path, ours) in keep_ours {
            self.write_worktree(&path, ours.as_deref())?;
        }
        Ok(held)
    }

    /// The three sides of an unmerged path, from the index's stages.
    fn sides(&self, repo_path: &str) -> Result<Sides, SyncError> {
        let listed = self.git.ok(&["ls-files", "-u", "-z", "--", &format!(":(literal){repo_path}")])?;
        let mut sides = Sides::default();
        for entry in split_z(&listed) {
            // `<mode> <sha> <stage>\t<path>`
            let Some((meta, _)) = entry.split_once('\t') else {
                continue;
            };
            let mut parts = meta.split_whitespace();
            let (_mode, Some(sha), Some(stage)) = (parts.next(), parts.next(), parts.next()) else {
                continue;
            };
            let content = String::from_utf8_lossy(&self.git.bytes(&["cat-file", "blob", sha])?).into_owned();
            match stage {
                "1" => sides.base = Some(content),
                "2" => sides.ours = Some(content),
                "3" => sides.theirs = Some(content),
                _ => {}
            }
        }
        Ok(sides)
    }

    /// Put `content` (or a deletion) in the worktree at `repo_path` and the
    /// index.
    fn stage_repo_path(&self, repo_path: &str, content: Option<&str>) -> Result<(), SyncError> {
        let abs = self.layout.worktree.join(repo_path);
        match content {
            Some(c) => {
                write_file(&abs, c)?;
                self.git.ok(&["add", "--", &format!(":(literal){repo_path}")])?;
            }
            None => {
                let _ = std::fs::remove_file(&abs);
                self.git
                    .ok(&["rm", "-q", "--cached", "--ignore-unmatch", "--", &format!(":(literal){repo_path}")])?;
            }
        }
        Ok(())
    }

    /// Write docs path's worktree content (`None` deletes it).
    fn write_worktree(&self, docs_path: &str, content: Option<&str>) -> Result<(), SyncError> {
        let abs = self.layout.docs_root().join(docs_path);
        match content {
            Some(c) => write_file(&abs, c),
            None => match std::fs::remove_file(&abs) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(io("removing a held file", e)),
            },
        }
    }

    /// Set each held file's local content aside and put the committed
    /// version in its place.
    fn stash_held(&self) -> Result<Vec<(String, Option<String>)>, SyncError> {
        let mut out = Vec::new();
        for path in self.conflicts.paths() {
            let abs = self.layout.docs_root().join(&path);
            let local = std::fs::read_to_string(&abs).ok();
            let committed = self.committed(&path)?;
            self.write_worktree(&path, committed.as_deref())?;
            out.push((path, local));
        }
        Ok(out)
    }

    /// Undo [`Self::stash_held`], recording the remote's newer side.
    fn unstash_held(&mut self, stashed: Vec<(String, Option<String>)>) -> Result<(), SyncError> {
        for (path, local) in stashed {
            let committed = self.committed(&path)?;
            self.conflicts
                .update_theirs(&path, committed)
                .map_err(|e| io("recording a conflict", e))?;
            self.write_worktree(&path, local.as_deref())?;
        }
        Ok(())
    }

    /// Docs path's content at `HEAD`; `None` when it isn't there.
    fn committed(&self, docs_path: &str) -> Result<Option<String>, SyncError> {
        let spec = format!("HEAD:{}", self.layout.repo_path(docs_path));
        let out = self.git.run(&["cat-file", "-e", &spec], crate::git::LOCAL_DEADLINE)?;
        if !out.success() {
            return Ok(None);
        }
        Ok(Some(String::from_utf8_lossy(&self.git.bytes(&["cat-file", "blob", &spec])?).into_owned()))
    }

    /// Whether anything waits to be committed (held files aside) or pushed.
    pub fn has_pending(&self) -> Result<bool, SyncError> {
        let status = self.git.ok(&[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=all",
            "--",
            &format!(":(literal){}", self.layout.scope()),
        ])?;
        let held: BTreeSet<String> = self.conflicts.paths().into_iter().map(|p| self.layout.repo_path(&p)).collect();
        let mut fields = split_z(&status).into_iter();
        while let Some(entry) = fields.next() {
            let code = entry.get(..2).unwrap_or("");
            let path = entry.get(3..).unwrap_or("");
            if code.starts_with('R') || code.starts_with('C') {
                fields.next();
            }
            if !held.contains(path) {
                return Ok(true);
            }
        }
        if self.head()?.is_none() {
            return Ok(false);
        }
        let remote = Layout::remote_ref(&self.layout.branch);
        if !self.git.run(&["rev-parse", "-q", "--verify", &remote], crate::git::LOCAL_DEADLINE)?.success() {
            return Ok(true);
        }
        Ok(ahead_of(&self.git, &remote)? > 0)
    }

    /// Resolve a held file with `content` (`None` deletes it): write it,
    /// commit it as "Resolve merge conflicts", and stop holding it. The
    /// commit is made even when the result is what's committed already
    /// (taking theirs whole), so the history says the conflict was settled.
    /// `by` is the commit's [`BY_TRAILER`]s. The caller pushes.
    pub fn resolve(&mut self, docs_path: &str, content: Option<&str>, by: &[&str]) -> Result<Committed, SyncError> {
        if !self.conflicts.is_held(docs_path) {
            return Err(SyncError::Setup(format!("`{docs_path}` isn't in conflict")));
        }
        self.write_worktree(docs_path, content)?;
        let repo_path = self.layout.repo_path(docs_path);
        self.git.ok(&["add", "-A", "--", &format!(":(literal){repo_path}")])?;
        let committed = match self.commit_staged(Some(RESOLVE_MESSAGE), by)? {
            Some(c) => c,
            None => {
                let mut full = RESOLVE_MESSAGE.to_string();
                if !by.is_empty() {
                    full.push('\n');
                    for who in by {
                        full.push_str(&format!("\n{BY_TRAILER}: {who}"));
                    }
                }
                self.git.ok(&["commit", "-q", "--no-verify", "--allow-empty", "-m", &full])?;
                Committed {
                    message: RESOLVE_MESSAGE.to_string(),
                    paths: vec![docs_path.to_string()],
                }
            }
        };
        self.conflicts.release(docs_path).map_err(|e| io("releasing a conflict", e))?;
        Ok(committed)
    }
}

/// Fetch and push for one docs: they touch only refs and objects, so they
/// run beside writes to the worktree.
#[derive(Debug, Clone)]
pub struct Remote {
    git: Git,
    layout: Layout,
}

impl Remote {
    /// Fetch the docs' branch (and, in folder mode, the base) from the
    /// remote. Touches only refs and objects, so it runs beside writes.
    pub fn fetch(&self) -> Result<Fetched, SyncError> {
        let mut fetched = Fetched {
            branch: fetch_branch(&self.git, &self.layout.branch)?,
            base: false,
        };
        if let Some(f) = &self.layout.folder {
            fetched.base = fetch_branch(&self.git, &f.base)?;
        }
        Ok(fetched)
    }

    /// Push the branch when the remote lacks some of it.
    pub fn push(&self) -> Result<Pushed, SyncError> {
        // Push the commit as it is now: a commit landing meanwhile (this runs
        // without the worktree lock) is the next push's, never marked pushed.
        let Some(tip) = head(&self.git)? else {
            return Ok(Pushed::Nothing);
        };
        let remote = Layout::remote_ref(&self.layout.branch);
        let has_remote = self.git.run(&["rev-parse", "-q", "--verify", &remote], crate::git::LOCAL_DEADLINE)?.success();
        if has_remote && ahead_of(&self.git, &remote)? == 0 {
            return Ok(Pushed::Nothing);
        }
        let refspec = format!("{tip}:refs/heads/{}", self.layout.branch);
        self.git
            .ok_within(&["push", "-q", "--no-verify", REMOTE, &refspec], NETWORK_DEADLINE)?;
        // Not every remote setup updates the tracking ref on push; make sure
        // the next ahead count starts from what was pushed.
        let _ = self.git.ok(&["update-ref", &remote, &tip]);
        Ok(Pushed::Pushed)
    }

    /// Folder mode: whether the rolling branch changes the docs' folder
    /// relative to the base (so a PR has something to show).
    pub fn differs_from_base(&self) -> Result<bool, SyncError> {
        let Some(f) = &self.layout.folder else {
            return Ok(false);
        };
        let base = Layout::remote_ref(&f.base);
        if !self.git.run(&["rev-parse", "-q", "--verify", &base], crate::git::LOCAL_DEADLINE)?.success() {
            return Ok(false);
        }
        let out = self.git.run(
            &["diff", "--quiet", &format!("{base}...HEAD"), "--", self.layout.scope()],
            crate::git::LOCAL_DEADLINE,
        )?;
        Ok(out.code == Some(1))
    }
}

fn head(git: &Git) -> Result<Option<String>, SyncError> {
    let out = git.run(&["rev-parse", "-q", "--verify", "HEAD"], crate::git::LOCAL_DEADLINE)?;
    Ok(out.success().then(|| out.stdout.trim().to_string()))
}

fn ahead_of(git: &Git, reference: &str) -> Result<u64, SyncError> {
    let out = git.ok(&["rev-list", "--count", &format!("{reference}..HEAD")])?;
    Ok(out.trim().parse().unwrap_or(0))
}

#[derive(Debug, Default)]
struct Sides {
    base: Option<String>,
    ours: Option<String>,
    theirs: Option<String>,
}

/// Whether `dir` is a git checkout (a worktree has a `.git` file).
fn is_worktree(dir: &Path) -> bool {
    dir.join(".git").exists()
}

fn clear_empty_dir(dir: &Path) -> Result<(), SyncError> {
    match std::fs::read_dir(dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io("reading the docs' folder", e)),
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Err(SyncError::Setup(format!(
                    "{} already exists and isn't a git checkout; move it aside to set the docs up",
                    dir.display()
                )));
            }
            std::fs::remove_dir(dir).map_err(|e| io("clearing the docs' folder", e))
        }
    }
}

fn write_file(abs: &Path, content: &str) -> Result<(), SyncError> {
    if let Some(dir) = abs.parent() {
        std::fs::create_dir_all(dir).map_err(|e| io("creating a folder", e))?;
    }
    crate::fs::paths::write_atomic(abs, content.as_bytes()).map_err(|e| io("writing a file", e))
}

fn is_missing_remote_ref(e: &GitError) -> bool {
    let m = e.stderr().to_ascii_lowercase();
    m.contains("couldn't find remote ref") || m.contains("could not find remote ref")
}

fn ref_exists(git: &Git, reference: &str) -> Result<bool, SyncError> {
    Ok(git.run(&["rev-parse", "-q", "--verify", reference], crate::git::LOCAL_DEADLINE)?.success())
}

/// Fetch `branch` into its remote-tracking ref; answers whether origin has
/// it.
fn fetch_branch(git: &Git, branch: &str) -> Result<bool, SyncError> {
    let refspec = format!("+refs/heads/{branch}:{}", Layout::remote_ref(branch));
    match git.ok_within(&["fetch", "-q", "--no-tags", REMOTE, &refspec], NETWORK_DEADLINE) {
        Ok(_) => Ok(true),
        Err(e) if is_missing_remote_ref(&e) => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn worktree_str(layout: &Layout) -> String {
    layout.worktree.to_string_lossy().into_owned()
}

/// Check out an existing branch (from origin, else local) into the
/// worktree; `false` when neither has it.
fn check_out_existing(git: &Git, layout: &Layout, on_origin: bool) -> Result<bool, SyncError> {
    let branch = &layout.branch;
    let wt = worktree_str(layout);
    let local = ref_exists(git, &format!("refs/heads/{branch}"))?;
    if local {
        git.ok(&["worktree", "add", "-q", &wt, branch])?;
        return Ok(true);
    }
    if on_origin {
        git.ok(&["worktree", "add", "-q", "--no-track", "-b", branch, &wt, &Layout::remote_ref(branch)])?;
        return Ok(true);
    }
    Ok(false)
}

fn setup_branch(git: &Git, layout: &Layout, name: &str, env: &[(String, String)]) -> Result<Setup, SyncError> {
    let on_origin = fetch_branch(git, &layout.branch)?;
    if check_out_existing(git, layout, on_origin)? {
        return Ok(Setup::CheckedOut);
    }
    // No such branch anywhere: an orphan with starter files. The worktree
    // starts detached with nothing checked out (so a large repo isn't
    // materialized only to be emptied), then HEAD is pointed at the unborn
    // branch with an empty index, which leaves the empty folder alone.
    let wt = worktree_str(layout);
    if ref_exists(git, "HEAD")? {
        git.ok(&["worktree", "add", "-q", "--detach", "--no-checkout", &wt])?;
        let wt_git = git.at(&layout.worktree);
        wt_git.ok(&["symbolic-ref", "HEAD", &format!("refs/heads/{}", layout.branch)])?;
        wt_git.ok(&["read-tree", "--empty"])?;
    } else {
        git.ok(&["worktree", "add", "-q", "--orphan", "-b", &layout.branch, &wt])?;
    }
    write_starter_files(&layout.worktree, name)?;
    let mut wt_git = Git::new(&layout.worktree).with_env(env);
    wt_git.ensure_identity();
    wt_git.ok(&["add", "-A", "--", "."])?;
    wt_git.ok(&["commit", "-q", "--no-verify", "-m", "Create docs"])?;
    Ok(Setup::Created)
}

fn setup_folder(git: &Git, layout: &Layout, folder: &FolderMode) -> Result<Setup, SyncError> {
    let on_origin = fetch_branch(git, &layout.branch)?;
    let base_on_origin = fetch_branch(git, &folder.base)?;
    if check_out_existing(git, layout, on_origin)? {
        return Ok(Setup::CheckedOut);
    }
    let start = if base_on_origin {
        Layout::remote_ref(&folder.base)
    } else {
        folder.base.clone()
    };
    let wt = worktree_str(layout);
    git.ok(&["worktree", "add", "-q", "--no-track", "-b", &layout.branch, &wt, &start])?;
    Ok(Setup::CheckedOut)
}

/// The README a new docs starts with.
pub fn starter_readme(name: &str) -> String {
    format!(
        "# {name}\n\nThese are the project's docs: plans, decisions and notes that belong beside the code but not in it.\n\
         Shelbi keeps them in sync on its own; edit them from Documentation in Shelbi, with `shelbi docs`, or in this folder.\n"
    )
}

/// The files a new branch-mode docs starts with: a ContextStore space's
/// settings, ignore files and template, and a README.
pub fn write_starter_files(root: &Path, name: &str) -> Result<(), SyncError> {
    crate::fs::settings::initialize_as_space(root, name, None, None).map_err(|e| io("writing the docs' settings", e))?;
    let readme = root.join("README.md");
    if !readme.exists() {
        write_file(&readme, &starter_readme(name))?;
    }
    Ok(())
}
