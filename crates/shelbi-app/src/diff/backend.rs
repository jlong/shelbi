//! Where a diff's data comes from. A [`DiffBackend`] answers the reads and
//! writes the session's worker threads need; which one a session gets depends
//! on where the project lives, and nothing above this file knows.
//!
//! - [`HostBackend`]: a project on this machine. Git runs here, or over SSH
//!   for a workspace on another machine (the worktree's [`Host`]). Comments
//!   and viewed marks are in this machine's state directory.
//!   The task's pull request comes through `gh` on this machine, whichever
//!   machine the worktree is on.
//! - [`FarBackend`]: a window on a project on another machine. Each call is a
//!   control request ([`crate::far::Request::Diff`]) to that machine's daemon,
//!   which answers it with a [`HostBackend`] of its own ([`answer`]).

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use shelbi_core::{Host, IssuePr};

use super::comments::{CommentDoc, CommentOp};
use super::git::{self, BlameLine, GitRunner, HostGit, RawChangeset};
use super::model::CommitScope;
use super::pr::{NoPr, PrDetails, PrSource, PrWrite, ProjectGh, TaskPr};
use super::revert::{self, RevertPlan, RevertSpec};
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

/// How a message to an agent landed. Each is a delivery: anything short of one
/// is an `Err`, and the comments stay unsent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelivery {
    /// Seen submitted.
    Submitted,
    /// The agent was mid-turn: it's queued, and goes when the turn ends.
    Queued,
    /// Delivered to a runner Shelbi can't read back.
    Unverified,
}

/// Puts a message into a target's agent session, verifying it was submitted
/// and not left in the input. Passed in, because that's the orchestrator's and
/// this crate sits below it: `shelbi_orchestrator::agent_send::service`.
pub type AgentSend = Arc<dyn Fn(&DiffTarget, &str) -> Result<AgentDelivery, String> + Send + Sync>;

/// A diff's reads and writes. Blocking: only worker threads call these.
pub trait DiffBackend: Send + Sync {
    fn changeset(&self, scope: &CommitScope) -> Result<RawChangeset, String>;
    /// Blame `path` at `rev`, or the worktree.
    fn blame(&self, path: &str, rev: Option<&str>) -> Result<Vec<BlameLine>, String>;
    fn comments(&self) -> Result<CommentDoc, String>;
    fn apply_comment(&self, op: &CommentOp) -> Result<CommentDoc, String>;
    fn viewed(&self) -> Result<ViewedDoc, String>;
    fn apply_viewed(&self, op: &ViewedOp) -> Result<ViewedDoc, String>;
    /// Send `message` to the agent in the target's session.
    fn send_to_agent(&self, message: &str) -> Result<AgentDelivery, String>;
    /// Check a revert, and say what it would do.
    fn plan_revert(&self, spec: &RevertSpec) -> Result<RevertPlan, String>;
    /// Carry out a checked revert, answering what the status bar says.
    fn revert(&self, plan: &RevertPlan) -> Result<String, String>;
    /// The task's pull request, if it has one.
    fn pr_lookup(&self) -> Result<Option<IssuePr>, String>;
    /// All about that pull request, from GitHub.
    fn pr_details(&self, pr: &IssuePr) -> Result<PrDetails, String>;
    /// Change something on it, answering what the status bar says.
    fn pr_write(&self, pr: &IssuePr, change: &PrWrite) -> Result<String, String>;
}

/// A target's data on this machine, or on a worktree this machine reaches
/// over SSH.
pub struct HostBackend {
    pub target: DiffTarget,
    pub base_branch: String,
    pub git: Arc<dyn GitRunner>,
    pub store: DiffStore,
    pub agent: AgentSend,
    pub pr: Arc<dyn PrSource>,
}

impl HostBackend {
    /// `target`'s backend, from its project's config: the worktree, its
    /// machine, the base branch.
    pub fn for_target(target: &DiffTarget, agent: AgentSend) -> Result<HostBackend, String> {
        let project = shelbi_state::load_project(&target.project).map_err(|e| e.to_string())?;
        let spec = project
            .workspace(&target.workspace)
            .ok_or_else(|| format!("`{}` isn't a workspace in `{}`", target.workspace, target.project))?;
        let machine = project
            .machine(&spec.machine)
            .ok_or_else(|| format!("`{}` isn't a machine in `{}`", spec.machine, target.project))?;
        let worktree = shelbi_core::pool::workspace_worktree(machine, spec);
        let git: Arc<dyn GitRunner> = Arc::new(HostGit {
            host: machine.host(),
            worktree: worktree.to_string_lossy().into_owned(),
        });
        Ok(HostBackend {
            target: target.clone(),
            agent,
            base_branch: project.base_branch().to_string(),
            pr: Arc::new(TaskPr {
                project: target.project.clone(),
                task: target.task.clone(),
                gh: Arc::new(ProjectGh { project: target.project.clone() }),
                git: git.clone(),
            }),
            git,
            store: DiffStore::for_project(&target.project, &target.state_key())?,
        })
    }

    /// A backend over a worktree on `host`, for tests and a fixture: no project
    /// config, and an agent that's whatever `agent` does.
    pub fn over(
        host: Host,
        worktree: impl Into<String>,
        target: DiffTarget,
        base_branch: impl Into<String>,
        store: DiffStore,
        agent: AgentSend,
    ) -> HostBackend {
        HostBackend {
            target,
            agent,
            base_branch: base_branch.into(),
            git: Arc::new(HostGit { host, worktree: worktree.into() }),
            store,
            pr: Arc::new(NoPr),
        }
    }

    /// With this pull request, for a test or a fixture.
    pub fn with_pr(mut self, pr: Arc<dyn PrSource>) -> HostBackend {
        self.pr = pr;
        self
    }
}

impl DiffBackend for HostBackend {
    fn changeset(&self, scope: &CommitScope) -> Result<RawChangeset, String> {
        git::collect(&*self.git, self.target.kind, &self.base_branch, scope)
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

    fn send_to_agent(&self, message: &str) -> Result<AgentDelivery, String> {
        (self.agent)(&self.target, message)
    }

    fn plan_revert(&self, spec: &RevertSpec) -> Result<RevertPlan, String> {
        revert::plan(&*self.git, self.target.kind, &self.base_branch, spec)
    }

    fn revert(&self, plan: &RevertPlan) -> Result<String, String> {
        revert::run(&*self.git, self.target.kind, &self.base_branch, plan)
    }

    fn pr_lookup(&self) -> Result<Option<IssuePr>, String> {
        self.pr.lookup()
    }

    fn pr_details(&self, pr: &IssuePr) -> Result<PrDetails, String> {
        self.pr.details(pr)
    }

    fn pr_write(&self, pr: &IssuePr, change: &PrWrite) -> Result<String, String> {
        self.pr.write(pr, change)
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
    SendToAgent(String),
    PlanRevert(RevertSpec),
    Revert(RevertPlan),
    PrLookup,
    PrDetails(IssuePr),
    PrWrite(IssuePr, PrWrite),
}

/// A far daemon's answer to a [`DiffRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiffReply {
    Changeset(Result<RawChangeset, String>),
    Blame(Result<Vec<BlameLine>, String>),
    Comments(Result<CommentDoc, String>),
    Viewed(Result<ViewedDoc, String>),
    Sent(Result<AgentDelivery, String>),
    RevertPlanned(Result<RevertPlan, String>),
    Reverted(Result<String, String>),
    PrLookup(Result<Option<IssuePr>, String>),
    PrDetails(Box<Result<PrDetails, String>>),
    PrWritten(Result<String, String>),
}

/// Answer a [`DiffRequest`] on the machine the project lives on. A task's
/// comments don't need its worktree, so they answer whether or not the target
/// names a workspace (a Reject from the palette knows only the task).
pub fn answer(request: &DiffRequest, agent: AgentSend) -> DiffReply {
    match &request.op {
        DiffOp::Comments => DiffReply::Comments(store_of(&request.target).and_then(|s| s.comments())),
        DiffOp::ApplyComment(op) => DiffReply::Comments(store_of(&request.target).and_then(|s| s.apply_comment(op))),
        op => {
            let backend = HostBackend::for_target(&request.target, agent);
            answer_with(backend.as_ref().map_err(String::clone), op)
        }
    }
}

fn store_of(target: &DiffTarget) -> Result<DiffStore, String> {
    DiffStore::for_project(&target.project, &target.state_key())
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
        DiffOp::SendToAgent(message) => run(
            &|b| DiffReply::Sent(b.send_to_agent(message)),
            &|e| DiffReply::Sent(Err(e)),
        ),
        DiffOp::PlanRevert(spec) => run(
            &|b| DiffReply::RevertPlanned(b.plan_revert(spec)),
            &|e| DiffReply::RevertPlanned(Err(e)),
        ),
        DiffOp::Revert(plan) => run(
            &|b| DiffReply::Reverted(b.revert(plan)),
            &|e| DiffReply::Reverted(Err(e)),
        ),
        DiffOp::PrLookup => run(&|b| DiffReply::PrLookup(b.pr_lookup()), &|e| DiffReply::PrLookup(Err(e))),
        DiffOp::PrDetails(pr) => run(
            &|b| DiffReply::PrDetails(Box::new(b.pr_details(pr))),
            &|e| DiffReply::PrDetails(Box::new(Err(e))),
        ),
        DiffOp::PrWrite(pr, change) => run(
            &|b| DiffReply::PrWritten(b.pr_write(pr, change)),
            &|e| DiffReply::PrWritten(Err(e)),
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

    fn send_to_agent(&self, message: &str) -> Result<AgentDelivery, String> {
        match self.ask(DiffOp::SendToAgent(message.to_string()))? {
            DiffReply::Sent(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn plan_revert(&self, spec: &RevertSpec) -> Result<RevertPlan, String> {
        match self.ask(DiffOp::PlanRevert(spec.clone()))? {
            DiffReply::RevertPlanned(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn revert(&self, plan: &RevertPlan) -> Result<String, String> {
        match self.ask(DiffOp::Revert(plan.clone()))? {
            DiffReply::Reverted(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn pr_lookup(&self) -> Result<Option<IssuePr>, String> {
        match self.ask(DiffOp::PrLookup)? {
            DiffReply::PrLookup(result) => result,
            other => Err(wrong(other)),
        }
    }

    fn pr_details(&self, pr: &IssuePr) -> Result<PrDetails, String> {
        match self.ask(DiffOp::PrDetails(pr.clone()))? {
            DiffReply::PrDetails(result) => *result,
            other => Err(wrong(other)),
        }
    }

    fn pr_write(&self, pr: &IssuePr, change: &PrWrite) -> Result<String, String> {
        match self.ask(DiffOp::PrWrite(pr.clone(), change.clone()))? {
            DiffReply::PrWritten(result) => result,
            other => Err(wrong(other)),
        }
    }
}

/// The backend for `target` as seen through `link`. Blocking the first time
/// (it reads the project's config), so a worker thread calls it. `agent` is how
/// this machine reaches an agent's session; a far project's daemon uses its own.
pub fn backend_for(
    target: &DiffTarget,
    link: Option<&ProjectLink>,
    agent: AgentSend,
) -> Result<Arc<dyn DiffBackend>, String> {
    if let Some(link) = link.filter(|l| l.is_remote()) {
        return Ok(Arc::new(FarBackend { target: target.clone(), link: link.clone() }));
    }
    Ok(Arc::new(HostBackend::for_target(target, agent)?))
}

/// A task's comments, reached without its worktree: on this machine, or through
/// a far project's daemon. What Reject reads to pre-fill its reason and marks
/// sent afterwards, and where a CLI reply goes. Blocking.
pub struct CommentAccess {
    target: DiffTarget,
    far: Option<FarBackend>,
}

/// The access for `task`'s comments.
pub fn comment_access(project: &str, task: &str, link: Option<&ProjectLink>) -> CommentAccess {
    let target = DiffTarget {
        project: project.to_string(),
        workspace: String::new(),
        kind: TargetKind::Review,
        task: Some(task.to_string()),
    };
    let far = link
        .filter(|l| l.is_remote())
        .map(|link| FarBackend { target: target.clone(), link: link.clone() });
    CommentAccess { target, far }
}

impl CommentAccess {
    pub fn comments(&self) -> Result<CommentDoc, String> {
        match &self.far {
            Some(far) => far.comments(),
            None => store_of(&self.target)?.comments(),
        }
    }

    pub fn apply(&self, op: &CommentOp) -> Result<CommentDoc, String> {
        match &self.far {
            Some(far) => far.apply_comment(op),
            None => store_of(&self.target)?.apply_comment(op),
        }
    }

    /// The reason a Reject starts from: the task's unsent comments, if any.
    pub fn reject_reason(&self) -> Result<Option<String>, String> {
        let doc = self.comments()?;
        Ok(super::comments::agent_message(&doc, &self.target.project, &self.target.state_key()).map(|(message, _)| message))
    }

    /// A reject with `reason` landed: the threads it carried have been sent.
    pub fn mark_sent_in(&self, reason: &str) -> Result<(), String> {
        let ids = super::comments::threads_in(reason, &self.comments()?);
        if !ids.is_empty() {
            self.apply(&CommentOp::MarkSent { ids })?;
        }
        Ok(())
    }
}
