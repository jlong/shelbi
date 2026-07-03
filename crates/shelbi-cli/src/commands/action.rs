//! `shelbi action <subcommand>` — single-purpose workflow action primitives.
//!
//! Each subcommand wraps one function in `shelbi_orchestrator::actions` and
//! prints a one-line result on stdout so the orchestrator (or a workflow
//! engine) can grep the verdict without parsing JSON. Exit codes are
//! reserved for hard failures (bad config, gh/git errors); the "nothing
//! to do" cases — branch already deleted, no open PR to close — succeed
//! quietly because that's exactly the workflow contract.

use anyhow::{anyhow, Result};
use clap::Subcommand;

use shelbi_orchestrator::{actions, transition};
use shelbi_state::{load_project, load_task, load_workflow};

use crate::commands::require_project;

#[derive(Debug, Subcommand)]
pub enum ActionCmd {
    /// Push the task's branch from its assigned workspace's worktree to
    /// `origin`. Idempotent — pushing an up-to-date branch is a clean
    /// success.
    PushBranch { task_id: String },
    /// Open a PR for the task's branch. Prints the PR number on stdout.
    /// Idempotent — if an open PR for the branch already exists, returns
    /// that PR's number unchanged. The PR base is resolved by the chain
    /// documented on [`shelbi_orchestrator::actions::open_pr`]:
    /// `--target` (per-transition override) → first non-`Done`
    /// `depends_on:` parent's branch → project's effective `base_branch`.
    OpenPr {
        task_id: String,
        /// Override the PR base branch for this open_pr call. Mirrors
        /// the per-transition `target:` field on the workflow YAML when
        /// the workflow engine invokes this primitive.
        #[arg(long)]
        target: Option<String>,
    },
    /// Close any open PR for the task's branch. Prints the closed PR
    /// number on stdout, or `none` if there was nothing to close.
    ClosePr { task_id: String },
    /// Integrate the task's branch into the target branch using the
    /// project's configured `merge_strategy`. Picks one of two paths
    /// per `Plans/workflows.md` §12: if a PR is open for the branch,
    /// runs `gh pr merge --<strategy>`; otherwise the hub fetches the
    /// branch from `origin` and runs `git merge --<strategy>` locally,
    /// then pushes the result back. Prints `pr:<n>:<sha>` or
    /// `hub:<target>:<sha>` on the first line so the caller can tell
    /// the two paths apart. Does not delete the branch — pair with
    /// `delete-branch` for that.
    ///
    /// Auto-fires `restack` on every not-`Done` task that lists this
    /// task in its `depends_on:`; one extra line per child appears
    /// after the merge line (`restacked:...` or `skipped:...`).
    Merge {
        task_id: String,
        /// Override the merge target for this call. Mirrors the
        /// per-transition `target:` field on the workflow YAML; absent,
        /// the merge lands on the project's effective `base_branch`.
        #[arg(long)]
        target: Option<String>,
    },
    /// Delete the task's branch from `origin` and from the hub's local
    /// refs. Skipped if a workspace still has the branch checked out.
    /// Prints `deleted` / `skipped:<reason>` / `not-present`.
    DeleteBranch { task_id: String },
    /// Rewrite the task's branch onto a new base — typically the
    /// `target` the parent just merged into — and retarget any open PR.
    /// Idempotent: re-running on a branch that's already based on
    /// `--onto` prints `skipped:<id>:already-restacked`. Run on the
    /// hub via a detached worktree, so the hub's main work_dir keeps
    /// whatever branch you have checked out.
    Restack {
        /// The child task whose branch we're rebasing.
        task_id: String,
        /// The branch the child branch is currently based on. Usually
        /// the parent task's branch — the parent that just merged.
        #[arg(long)]
        from: String,
        /// New base for the child branch. Defaults to the project's
        /// effective `base_branch` when omitted, matching the common
        /// "stack collapses to main" shape.
        #[arg(long)]
        onto: Option<String>,
    },
    /// Fire every action a workflow transition declares for a
    /// `from -> to` status move, in order. This is the automatic
    /// counterpart to running the single-verb primitives by hand: it
    /// looks up the task's workflow, resolves the transition's `target:`
    /// (substituting `{{var}}` from the task's frontmatter params), and
    /// runs the declared `actions:`, short-circuiting on the first
    /// failure. Prints `<action>\t<result>` per fired action; an
    /// undeclared edge (or one with no `actions:`) prints nothing and
    /// exits 0.
    ApplyTransition {
        task_id: String,
        /// Stable id of the status the task is moving out of.
        #[arg(long)]
        from: String,
        /// Stable id of the status the task is moving into.
        #[arg(long)]
        to: String,
    },
}

pub fn run(project_opt: Option<String>, cmd: ActionCmd) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let project = load_project(&project_name).map_err(|e| anyhow!(e))?;

    match cmd {
        ActionCmd::PushBranch { task_id } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            actions::push_branch(&project, &tf.task).map_err(|e| anyhow!(e))?;
            println!("pushed");
            Ok(())
        }
        ActionCmd::OpenPr { task_id, target } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let pr = actions::open_pr(
                &project,
                &project_name,
                &tf.task,
                &tf.body,
                target.as_deref(),
            )
            .map_err(|e| anyhow!(e))?;
            println!("{pr}");
            Ok(())
        }
        ActionCmd::ClosePr { task_id } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            match actions::close_pr(&project, &tf.task).map_err(|e| anyhow!(e))? {
                Some(pr) => println!("{pr}"),
                None => println!("none"),
            }
            Ok(())
        }
        ActionCmd::Merge { task_id, target } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let result =
                actions::merge(&project, &project_name, &tf.task, target.as_deref())
                    .map_err(|e| anyhow!(e))?;
            println!("{}", result.merge.as_line());
            for r in &result.restacks {
                println!("{}", r.as_line());
            }
            Ok(())
        }
        ActionCmd::DeleteBranch { task_id } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let outcome = actions::delete_branch(&project, &tf.task).map_err(|e| anyhow!(e))?;
            println!("{}", outcome.as_line());
            Ok(())
        }
        ActionCmd::Restack { task_id, from, onto } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let outcome =
                actions::restack(&project, &tf.task, &from, onto.as_deref())
                    .map_err(|e| anyhow!(e))?;
            println!("{}", outcome.as_line());
            Ok(())
        }
        ActionCmd::ApplyTransition { task_id, from, to } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let workflow = load_workflow(&project_name, tf.task.workflow_or_default())
                .map_err(|e| anyhow!(e))?;
            let outcomes = transition::execute_transition(
                &project,
                &project_name,
                &tf.task,
                &tf.body,
                &workflow,
                &from,
                &to,
            )
            .map_err(|e| anyhow!(e))?;
            for outcome in &outcomes {
                // `merge` can emit multiple lines (merge + one per
                // restacked child); keep the action tag on the first and
                // indent continuation lines so the grouping stays legible.
                let mut lines = outcome.line.lines();
                if let Some(first) = lines.next() {
                    println!("{}\t{first}", outcome.action);
                }
                for cont in lines {
                    println!("\t{cont}");
                }
            }
            Ok(())
        }
    }
}
