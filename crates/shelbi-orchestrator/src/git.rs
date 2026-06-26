//! Shared git/gh helpers for the per-workflow action primitives and the
//! Zen Mode merge primitives. Kept here so `zen.rs` and `actions.rs` don't
//! drift on the basics — running a shell command in a worktree, finding
//! the right host for an operation, looking up an open PR.

use std::path::PathBuf;
use std::process::Output;

use shelbi_core::{Error, Host, MachineKind, Project, Result, Task};

use crate::worker::worker_worktree;

/// Run `argv` with cwd = `dir` on `host`, picking up the user's login
/// `PATH` on remote SSH hosts so `gh` and `git` resolve the same way
/// they do in the user's terminal.
pub(crate) fn run_in_dir(host: &Host, dir: &str, argv: &[&str]) -> Result<Output> {
    let escaped: Vec<String> = argv.iter().map(|a| shelbi_agent::shell_escape(a)).collect();
    let line = format!(
        "cd {} && {}",
        shelbi_agent::shell_escape(dir),
        escaped.join(" ")
    );
    // Local: bash -c is fine — the user already has gh on PATH.
    // Remote: bash -lc so .zprofile/.bash_profile populate PATH first.
    let flag = if host.is_local() { "-c" } else { "-lc" };
    shelbi_ssh::run(host, ["bash", flag, line.as_str()]).map_err(Error::Io)
}

/// Find the worker assigned to `task`, then return its host + worktree.
/// Errors if the task is unassigned or the worker/machine resolution
/// fails — those are caller bugs, not policy decisions.
pub(crate) fn locate_worker_worktree(project: &Project, task: &Task) -> Result<(Host, PathBuf)> {
    let worker_name = task.assigned_to.as_deref().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no assigned worker — assign one before running this action",
            task.id
        ))
    })?;
    let worker = project.worker(worker_name).ok_or_else(|| {
        Error::Other(format!(
            "task `{}` references unknown worker `{worker_name}`",
            task.id
        ))
    })?;
    let machine = project
        .machine(&worker.machine)
        .ok_or_else(|| Error::UnknownMachine(worker.machine.clone()))?;
    Ok((machine.host(), worker_worktree(machine, worker)))
}

/// The first local machine in the project — by convention the hub. The
/// hub's `work_dir` is a clean checkout of the project repo, so gh / git
/// commands routed through it have a remote to talk to without needing
/// a worker's worktree to exist yet.
pub(crate) fn locate_hub_workdir(project: &Project) -> Result<(Host, PathBuf)> {
    let machine = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, MachineKind::Local))
        .ok_or_else(|| {
            Error::Other(
                "project has no local machine to run gh on — hub-side actions require one".into(),
            )
        })?;
    Ok((machine.host(), machine.work_dir.clone()))
}

/// Look up an *open* PR for `branch` and return its number, if any. Uses
/// `gh pr list --head <branch> --state open`; a closed/merged PR for the
/// same branch is intentionally ignored — a fresh push warrants a fresh
/// PR.
pub(crate) fn lookup_open_pr(host: &Host, wt: &str, branch: &str) -> Result<Option<u64>> {
    let out = run_in_dir(
        host,
        wt,
        &[
            "gh",
            "pr",
            "list",
            "--head",
            branch,
            "--state",
            "open",
            "--json",
            "number",
            "--jq",
            ".[0].number // empty",
        ],
    )?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr list --head {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    let s = String::from_utf8_lossy(&out.stdout);
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    trimmed.parse::<u64>().map(Some).map_err(|_| {
        Error::Other(format!(
            "gh pr list returned non-numeric value `{trimmed}` for branch `{branch}`"
        ))
    })
}
