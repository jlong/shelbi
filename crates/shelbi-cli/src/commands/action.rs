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

use shelbi_orchestrator::actions;
use shelbi_state::{load_project, load_task};

use crate::commands::require_project;

#[derive(Debug, Subcommand)]
pub enum ActionCmd {
    /// Push the task's branch from its assigned worker's worktree to
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
    /// Delete the task's branch from `origin` and from the hub's local
    /// refs. Skipped if a worker still has the branch checked out.
    /// Prints `deleted` / `skipped:<reason>` / `not-present`.
    DeleteBranch { task_id: String },
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
        ActionCmd::DeleteBranch { task_id } => {
            let tf = load_task(&project_name, &task_id).map_err(|e| anyhow!(e))?;
            let outcome = actions::delete_branch(&project, &tf.task).map_err(|e| anyhow!(e))?;
            println!("{}", outcome.as_line());
            Ok(())
        }
    }
}
