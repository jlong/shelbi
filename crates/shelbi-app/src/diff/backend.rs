//! Where a diff's data comes from. A [`DiffBackend`] answers the reads and
//! writes the session's worker threads need; which one a session gets depends
//! on where the project lives, and nothing above this file knows.
//!
//! - [`HostBackend`]: a project on this machine. Git runs here, or over SSH
//!   for a workspace on another machine (the worktree's [`Host`]). Comments
//!   and viewed marks are in this machine's state directory.
//! - [`FarBackend`]: a window on a project on another machine. Each call is a
//!   control request ([`crate::far::Request::Diff`]) to that machine's daemon,
//!   which answers it with a [`HostBackend`] of its own ([`answer`]).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shelbi_core::Host;

use super::comments::{CommentDoc, CommentOp};
use super::git::{self, BlameLine, GitRunner, HostGit, RawChangeset};
use super::model::CommitScope;
use super::store::{DiffStore, ViewedDoc, ViewedOp};
use crate::source::ProjectLink;

/// What a diff is of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetKind {
    /// A review slot: the branch as it would merge.
    Review,
    /// A workspace: the branch plus what the agent hasn't committed.
    Workspace,
}

/// Which worktree to read, and whose comments go with it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DiffTarget {
    pub project: String,
    /// The workspace (or review slot) whose worktree it is.
    pub workspace: String,
    pub kind: TargetKind,
    /// The task on it. Its comments follow the task between the workspace and
    /// its review. Without one, they're the workspace's.
    pub task: Option<String>,
}

impl DiffTarget {
    /// The key of this target's stored comments and viewed marks.
    pub fn state_key(&self) -> String {
        match &self.task {
            Some(task) => task.clone(),
            None => format!("ws-{}", self.workspace),
        }
    }
}

/// A diff's reads and writes. Blocking: only worker threads call these.
pub trait DiffBackend: Send + Sync {
    fn changeset(&self, scope: &CommitScope) -> Result<RawChangeset, String>;
    /// Blame `path` at `rev`, or the worktree.
    fn blame(&self, path: &str, rev: Option<&str>) -> Result<Vec<BlameLine>, String>;
    fn comments(&self) -> Result<CommentDoc, String>;
    fn apply_comment(&self, op: &CommentOp) -> Result<CommentDoc, String>;
    fn viewed(&self) -> Result<ViewedDoc, String>;
    fn apply_viewed(&self, op: &ViewedOp) -> Result<ViewedDoc, String>;
}

/// A target's data on this machine, or on a worktree this machine reaches
/// over SSH.
pub struct HostBackend {
    pub kind: TargetKind,
    pub base_branch: String,
    pub git: Arc<dyn GitRunner>,
    pub store: DiffStore,
}

impl HostBackend {
    /// `target`'s backend, from its project's config: the worktree, its
    /// machine, the base branch.
    pub fn for_target(target: &DiffTarget) -> Result<HostBackend, String> {
        let project = shelbi_state::load_project(&target.project).map_err(|e| e.to_string())?;
        let spec = project
            .workspace(&target.workspace)
            .ok_or_else(|| format!("`{}` isn't a workspace in `{}`", target.workspace, target.project))?;
        let machine = project
            .machine(&spec.machine)
            .ok_or_else(|| format!("`{}` isn't a machine in `{}`", spec.machine, target.project))?;
        let worktree = shelbi_core::pool::workspace_worktree(machine, spec);
        Ok(HostBackend {
            kind: target.kind,
            base_branch: project.base_branch().to_string(),
            git: Arc::new(HostGit {
                host: machine.host(),
                worktree: worktree.to_string_lossy().into_owned(),
            }),
            store: DiffStore::for_project(&target.project, &target.state_key())?,
        })
    }

    /// A backend over a worktree on `host`.
    pub fn over(
        host: Host,
        worktree: impl Into<String>,
        kind: TargetKind,
        base_branch: impl Into<String>,
        store: DiffStore,
    ) -> HostBackend {
        HostBackend {
            kind,
            base_branch: base_branch.into(),
            git: Arc::new(HostGit { host, worktree: worktree.into() }),
            store,
        }
    }
}

impl DiffBackend for HostBackend {
    fn changeset(&self, scope: &CommitScope) -> Result<RawChangeset, String> {
        git::collect(&*self.git, self.kind, &self.base_branch, scope)
    }

    fn blame(&self, path: &str, rev: Option<&str>) -> Result<Vec<BlameLine>, String> {
        git::blame(&*self.git, path, rev)
    }

    fn comments(&self) -> Result<CommentDoc, String> {
        self.store.comments()
    }

    fn apply_comment(&self, op: &CommentOp) -> Result<CommentDoc, String> {
        self.store.apply_comment(op)
    }

    fn viewed(&self) -> Result<ViewedDoc, String> {
        self.store.viewed()
    }

    fn apply_viewed(&self, op: &ViewedOp) -> Result<ViewedDoc, String> {
        self.store.apply_viewed(op)
    }
}

/// The most a far daemon sends of a changeset. The control socket's frame
/// limit is 16 MiB of JSON; past this, the window says so instead of the
/// request failing on the wire.
pub const MAX_FAR_PATCH: usize = 12 * 1024 * 1024;

/// A diff request over the control socket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiffRequest {
    pub target: DiffTarget,
    pub op: DiffOp,
}

/// What a [`DiffRequest`] asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffOp {
    Changeset(CommitScope),
    Blame { path: String, rev: Option<String> },
    Comments,
    ApplyComment(CommentOp),
    Viewed,
    ApplyViewed(ViewedOp),
}

/// A far daemon's answer to a [`DiffRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffReply {
    Changeset(Result<RawChangeset, String>),
    Blame(Result<Vec<BlameLine>, String>),
    Comments(Result<CommentDoc, String>),
    Viewed(Result<ViewedDoc, String>),
}

/// Answer a [`DiffRequest`] on the machine the project lives on.
pub fn answer(request: &DiffRequest) -> DiffReply {
    let backend = HostBackend::for_target(&request.target);
    answer_with(backend.as_ref().map_err(String::clone), &request.op)
}

/// Answer `op` with `backend`.
pub fn answer_with(backend: Result<&HostBackend, String>, op: &DiffOp) -> DiffReply {
    let run = |f: &dyn Fn(&HostBackend) -> DiffReply, fail: &dyn Fn(String) -> DiffReply| match &backend {
        Ok(b) => f(b),
        Err(e) => fail(e.clone()),
    };
    match op {
        DiffOp::Changeset(scope) => run(
            &|b| {
                DiffReply::Changeset(b.changeset(scope).and_then(|raw| {
                    if raw.len() > MAX_FAR_PATCH {
                        return Err(format!(
                            "This diff is too large to send from the project's machine (over {} MB).",
                            MAX_FAR_PATCH / (1024 * 1024)
                        ));
                    }
                    Ok(raw)
                }))
            },
            &|e| DiffReply::Changeset(Err(e)),
        ),
        DiffOp::Blame { path, rev } => run(
            &|b| DiffReply::Blame(b.blame(path, rev.as_deref())),
            &|e| DiffReply::Blame(Err(e)),
        ),
        DiffOp::Comments => run(&|b| DiffReply::Comments(b.comments()), &|e| DiffReply::Comments(Err(e))),
        DiffOp::ApplyComment(c) => run(
            &|b| DiffReply::Comments(b.apply_comment(c)),
            &|e| DiffReply::Comments(Err(e)),
        ),
        DiffOp::Viewed => run(&|b| DiffReply::Viewed(b.viewed()), &|e| DiffReply::Viewed(Err(e))),
        DiffOp::ApplyViewed(v) => run(
            &|b| DiffReply::Viewed(b.apply_viewed(v)),
            &|e| DiffReply::Viewed(Err(e)),
        ),
    }
}

/// A target's data on another machine, through its daemon.
pub struct FarBackend {
    pub target: DiffTarget,
    pub link: ProjectLink,
}

impl FarBackend {
    fn ask(&self, op: DiffOp) -> Result<DiffReply, String> {
        let request = crate::far::Request::Diff(DiffRequest { target: self.target.clone(), op });
        match self.link.far_request(&request)? {
            crate::far::Reply::Diff(reply) => Ok(reply),
            other => Err(format!("the far daemon answered something else ({other:?})")),
        }
    }
}

fn wrong(reply: DiffReply) -> String {
    format!("the far daemon answered something else ({reply:?})")
}

impl DiffBackend for FarBackend {
    fn changeset(&self, scope: &CommitScope) -> Result<RawChangeset, String> {
        match self.ask(DiffOp::Changeset(scope.clone()))? {
            DiffReply::Changeset(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn blame(&self, path: &str, rev: Option<&str>) -> Result<Vec<BlameLine>, String> {
        let op = DiffOp::Blame { path: path.to_string(), rev: rev.map(str::to_string) };
        match self.ask(op)? {
            DiffReply::Blame(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn comments(&self) -> Result<CommentDoc, String> {
        match self.ask(DiffOp::Comments)? {
            DiffReply::Comments(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn apply_comment(&self, op: &CommentOp) -> Result<CommentDoc, String> {
        match self.ask(DiffOp::ApplyComment(op.clone()))? {
            DiffReply::Comments(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn viewed(&self) -> Result<ViewedDoc, String> {
        match self.ask(DiffOp::Viewed)? {
            DiffReply::Viewed(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn apply_viewed(&self, op: &ViewedOp) -> Result<ViewedDoc, String> {
        match self.ask(DiffOp::ApplyViewed(op.clone()))? {
            DiffReply::Viewed(result) => result,
            other => Err(wrong(other)),
        }
    }
}

/// The backend for `target` as seen through `link`. Blocking the first time
/// (it reads the project's config), so a worker thread calls it.
pub fn backend_for(target: &DiffTarget, link: Option<&ProjectLink>) -> Result<Arc<dyn DiffBackend>, String> {
    if let Some(link) = link.filter(|l| l.is_remote()) {
        return Ok(Arc::new(FarBackend { target: target.clone(), link: link.clone() }));
    }
    Ok(Arc::new(HostBackend::for_target(target)?))
}
