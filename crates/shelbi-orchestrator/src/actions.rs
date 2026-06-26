//! Workflow action primitives — `push_branch`, `close_pr`, `delete_branch`.
//!
//! Each function does one git/gh thing the workflow `transitions:` block
//! can name (see Plans/workflows.md, "Action set"). They are deliberately
//! single-purpose so the workflow engine — and a human at the CLI — can
//! sequence them per the active workflow without the primitive deciding
//! what should run next.
//!
//! All actions are idempotent and silently no-op when not applicable:
//!
//! - `push_branch` pushes the task's branch from the worker's worktree.
//!   Pushing an up-to-date branch reports `Everything up-to-date` and
//!   still succeeds.
//! - `close_pr` closes any *open* PR for the task's branch; with no open
//!   PR it returns `None` instead of erroring.
//! - `delete_branch` removes the branch from origin and from the hub's
//!   local refs. Skipped when a worker still has it checked out so we
//!   don't yank a branch out from under an active task.
//!
//! `push_branch` runs against the worker's worktree (that's where the
//! branch lives until it's pushed). `close_pr` and `delete_branch` run on
//! the hub — by the time the orchestrator is cleaning up a branch the
//! branch is on origin, so gh / git from any hub checkout work fine.

use shelbi_core::{Error, Host, Project, Result, Task};

use crate::git::{locate_hub_workdir, locate_worker_worktree, lookup_open_pr, run_in_dir};
use crate::worker::worker_worktree;

/// Outcome of [`delete_branch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// Branch was removed from at least one of (origin, hub local).
    Deleted,
    /// A worker still has the branch checked out; nothing was touched.
    /// Per the workflow spec, the branch will be replaced naturally on
    /// that worker's next dispatch.
    Skipped { reason: String },
    /// Branch wasn't present in either location — there was nothing to do.
    NotPresent,
}

impl DeleteOutcome {
    /// Single-line wire format printed on stdout by `shelbi action
    /// delete-branch`. Prefix-keyed so a caller can match on
    /// `deleted` / `skipped:` / `not-present` without parsing JSON.
    pub fn as_line(&self) -> String {
        match self {
            DeleteOutcome::Deleted => "deleted".to_string(),
            DeleteOutcome::Skipped { reason } => {
                let safe = reason.replace('\n', " ");
                format!("skipped:{safe}")
            }
            DeleteOutcome::NotPresent => "not-present".to_string(),
        }
    }
}

/// Push the task's branch from the worker's worktree to `origin`.
///
/// Errors when the task has no assigned worker or no `branch` field — both
/// are caller bugs (the workflow contract guarantees both fields by the
/// time this fires). Re-pushing an up-to-date branch is a clean success.
pub fn push_branch(project: &Project, task: &Task) -> Result<()> {
    let branch = require_branch(task)?;
    let (host, worktree) = locate_worker_worktree(project, task)?;
    let wt = worktree.to_string_lossy().into_owned();

    let out = run_in_dir(&host, &wt, &["git", "push", "-u", "origin", &branch])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} push -u origin {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

/// Close any open PR for the task's branch on the hub.
///
/// Returns `Some(pr_number)` when a PR was closed, `None` when no open PR
/// existed for the branch (the spec's "no-op if none open" case).
pub fn close_pr(project: &Project, task: &Task) -> Result<Option<u64>> {
    let branch = require_branch(task)?;
    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();

    let Some(num) = lookup_open_pr(&host, &wt, &branch)? else {
        return Ok(None);
    };
    let num_str = num.to_string();
    let out = run_in_dir(&host, &wt, &["gh", "pr", "close", &num_str])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("gh pr close {num_str}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(Some(num))
}

/// Delete the task's branch from origin and from the hub's local refs.
///
/// Skipped when any of the project's workers currently has the branch
/// checked out in its worktree — yanking the branch out from under an
/// active task would force the worker into a detached HEAD on its next
/// fetch. Returns [`DeleteOutcome::NotPresent`] when the branch is already
/// gone in both places (idempotent).
pub fn delete_branch(project: &Project, task: &Task) -> Result<DeleteOutcome> {
    let branch = require_branch(task)?;

    if let Some(worker_name) = worker_holding_branch(project, &branch)? {
        return Ok(DeleteOutcome::Skipped {
            reason: format!("branch is checked out in worker `{worker_name}`"),
        });
    }

    let (host, dir) = locate_hub_workdir(project)?;
    let wt = dir.to_string_lossy().into_owned();

    let local_present = local_branch_exists(&host, &wt, &branch)?;
    let remote_present = remote_branch_exists(&host, &wt, &branch)?;
    if !local_present && !remote_present {
        return Ok(DeleteOutcome::NotPresent);
    }

    if remote_present {
        let out = run_in_dir(&host, &wt, &["git", "push", "origin", "--delete", &branch])?;
        if !out.status.success() {
            // Race: the remote branch was removed between our probe and
            // the push (e.g. by a concurrent `gh pr merge --delete-branch`).
            // git reports `remote ref does not exist` and exits non-zero;
            // for an idempotent primitive that's a benign success.
            let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
            if !stderr.contains("remote ref does not exist") {
                return Err(Error::Command {
                    cmd: format!("git -C {wt} push origin --delete {branch}"),
                    status: out.status.to_string(),
                    stderr,
                });
            }
        }
    }

    if local_present {
        let out = run_in_dir(&host, &wt, &["git", "branch", "-D", &branch])?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!("git -C {wt} branch -D {branch}"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }

    Ok(DeleteOutcome::Deleted)
}

// ---------------------------------------------------------------------------
// helpers

fn require_branch(task: &Task) -> Result<String> {
    task.branch.clone().ok_or_else(|| {
        Error::Other(format!(
            "task `{}` has no branch — populate `branch:` in its frontmatter \
             before running this action",
            task.id
        ))
    })
}

/// Return the name of the first worker whose worktree has `branch` checked
/// out, or `None` if no worker is holding it. Workers whose worktree
/// doesn't exist yet are silently skipped — they can't be holding any
/// branch.
fn worker_holding_branch(project: &Project, branch: &str) -> Result<Option<String>> {
    for worker in &project.workers {
        let Some(machine) = project.machine(&worker.machine) else {
            // Misconfiguration — `validate_workers` already rejects this
            // shape on project load, so we can defensively skip it here
            // without short-circuiting the whole delete on an unrelated
            // YAML typo.
            continue;
        };
        let host = machine.host();
        let wt = worker_worktree(machine, worker);
        match worktree_branch(&host, &wt.to_string_lossy())? {
            Some(b) if b == branch => return Ok(Some(worker.name.clone())),
            _ => {}
        }
    }
    Ok(None)
}

/// Return the current branch name in `wt`, or `None` if the worktree
/// doesn't exist yet (a fresh worker that's never been dispatched) or is
/// in a detached HEAD state.
fn worktree_branch(host: &Host, wt: &str) -> Result<Option<String>> {
    let out = run_in_dir(host, wt, &["git", "rev-parse", "--abbrev-ref", "HEAD"])?;
    if !out.status.success() {
        // No worktree, no git repo, or read failure — none of which means
        // "this worker is holding our branch." Treat as "nothing to skip
        // here" rather than failing the delete.
        return Ok(None);
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if s.is_empty() || s == "HEAD" {
        // Detached HEAD: the branch ref doesn't gate the delete, so
        // we don't need to know what commit they're on.
        return Ok(None);
    }
    Ok(Some(s))
}

fn local_branch_exists(host: &Host, wt: &str, branch: &str) -> Result<bool> {
    let ref_name = format!("refs/heads/{branch}");
    let out = run_in_dir(host, wt, &["git", "rev-parse", "--verify", "--quiet", &ref_name])?;
    // `--verify --quiet` exits 0 iff the ref resolves; any non-zero means
    // "no such ref" (the only realistic failure mode here).
    Ok(out.status.success())
}

fn remote_branch_exists(host: &Host, wt: &str, branch: &str) -> Result<bool> {
    let out = run_in_dir(host, wt, &["git", "ls-remote", "--heads", "origin", branch])?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {wt} ls-remote --heads origin {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    // ls-remote prints one `<sha>\t<ref>` line per match, or empty when
    // the ref doesn't exist on the remote.
    Ok(!String::from_utf8_lossy(&out.stdout).trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{
        AgentRunnerSpec, Column, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
        WorkerSpec, ZenConfig,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::process::Command;

    // --- DeleteOutcome wire format -----------------------------------------

    #[test]
    fn delete_outcome_as_line_is_prefix_keyed() {
        assert_eq!(DeleteOutcome::Deleted.as_line(), "deleted");
        assert_eq!(DeleteOutcome::NotPresent.as_line(), "not-present");
        let line = DeleteOutcome::Skipped {
            reason: "branch is checked out in worker `alice`".into(),
        }
        .as_line();
        assert!(line.starts_with("skipped:"));
        assert!(line.contains("alice"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn delete_outcome_skipped_flattens_newlines() {
        let line = DeleteOutcome::Skipped {
            reason: "first line\nsecond".into(),
        }
        .as_line();
        assert!(!line.contains('\n'));
    }

    // --- require_branch contract ------------------------------------------

    fn bare_task(id: &str) -> Task {
        let now = chrono::Utc::now();
        Task {
            id: id.into(),
            title: id.into(),
            column: Column::InProgress,
            priority: 0,
            assigned_to: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn require_branch_errors_when_task_has_none() {
        let t = bare_task("orphan");
        let err = require_branch(&t).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("orphan"), "{msg}");
        assert!(msg.contains("branch"), "{msg}");
    }

    #[test]
    fn require_branch_returns_branch_when_present() {
        let mut t = bare_task("ok");
        t.branch = Some("shelbi/ok".into());
        assert_eq!(require_branch(&t).unwrap(), "shelbi/ok");
    }

    // --- git-backed primitives against fixture repos ----------------------

    fn run_git(cwd: &std::path::Path, args: &[&str]) {
        let status = Command::new("git").current_dir(cwd).args(args).status().unwrap();
        assert!(status.success(), "git {args:?} failed in {}", cwd.display());
    }

    /// Build a tiny fixture repo with a `main` branch and an extra
    /// `feature` branch. Returns the repo path.
    fn fixture_repo_with_feature() -> (tempfile::TempDir, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().to_path_buf();
        run_git(&repo, &["init", "-q", "-b", "main", "."]);
        run_git(&repo, &["config", "user.email", "test@example.com"]);
        run_git(&repo, &["config", "user.name", "Test"]);
        std::fs::write(repo.join("README.md"), "hi\n").unwrap();
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-q", "-m", "init"]);
        run_git(&repo, &["branch", "feature"]);
        (tmp, repo)
    }

    #[test]
    fn local_branch_exists_finds_present_and_missing() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let wt = repo.to_string_lossy().into_owned();
        assert!(local_branch_exists(&Host::Local, &wt, "main").unwrap());
        assert!(local_branch_exists(&Host::Local, &wt, "feature").unwrap());
        assert!(!local_branch_exists(&Host::Local, &wt, "nope").unwrap());
    }

    #[test]
    fn worktree_branch_returns_current_branch() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let wt = repo.to_string_lossy().into_owned();
        assert_eq!(
            worktree_branch(&Host::Local, &wt).unwrap().as_deref(),
            Some("main"),
        );
    }

    #[test]
    fn worktree_branch_returns_none_for_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist");
        let wt = missing.to_string_lossy().into_owned();
        // No git repo here — the helper must report "no branch" rather
        // than error, so the delete probe can keep looking at the
        // remaining workers.
        assert_eq!(worktree_branch(&Host::Local, &wt).unwrap(), None);
    }

    /// Build a project with one local worker pointed at `repo` so the
    /// worktree-branch probe finds the right HEAD. We piggy-back on
    /// `worker_worktree`'s `<work_dir>/.shelbi/wt/<worker>` derivation
    /// by creating a worktree at that path off `repo`.
    fn project_with_local_worker_holding(
        repo: &std::path::Path,
        worker: &str,
        branch: &str,
    ) -> Project {
        let wt_path = repo.join(".shelbi").join("wt").join(worker);
        // git worktree add requires the parent dir to exist.
        std::fs::create_dir_all(wt_path.parent().unwrap()).unwrap();
        run_git(
            repo,
            &[
                "worktree",
                "add",
                wt_path.to_str().unwrap(),
                branch,
            ],
        );

        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        Project {
            name: "fixture".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: vec![WorkerSpec {
                name: worker.into(),
                machine: "hub".into(),
                runner: "claude".into(),
            }],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    #[test]
    fn worker_holding_branch_finds_the_holder() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let project = project_with_local_worker_holding(&repo, "alice", "feature");

        let holder = worker_holding_branch(&project, "feature").unwrap();
        assert_eq!(holder.as_deref(), Some("alice"));

        // A branch nobody holds returns None.
        assert!(worker_holding_branch(&project, "other").unwrap().is_none());
    }

    #[test]
    fn worker_holding_branch_ignores_missing_worktrees() {
        // A worker whose worktree hasn't been provisioned yet must not
        // count as holding any branch — otherwise a delete_branch on a
        // fresh project would always say "skipped".
        let (_tmp, repo) = fixture_repo_with_feature();
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        let project = Project {
            name: "fixture".into(),
            repo: repo.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: repo.to_path_buf(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: vec![WorkerSpec {
                name: "never-spawned".into(),
                machine: "hub".into(),
                runner: "claude".into(),
            }],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        };
        assert!(worker_holding_branch(&project, "feature").unwrap().is_none());
    }

    fn task_on_branch(id: &str, branch: &str) -> Task {
        let mut t = bare_task(id);
        t.branch = Some(branch.into());
        t
    }

    #[test]
    fn delete_branch_skipped_when_a_worker_holds_it() {
        let (_tmp, repo) = fixture_repo_with_feature();
        let project = project_with_local_worker_holding(&repo, "alice", "feature");

        let out = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        match out {
            DeleteOutcome::Skipped { reason } => {
                assert!(reason.contains("alice"), "{reason}");
            }
            other => panic!("expected Skipped, got {other:?}"),
        }
        // Branch must still exist.
        let wt = repo.to_string_lossy().into_owned();
        assert!(local_branch_exists(&Host::Local, &wt, "feature").unwrap());
    }

    #[test]
    fn delete_branch_not_present_when_branch_already_gone() {
        // Use the origin-bearing fixture so `remote_branch_exists` has
        // a remote to probe; a hub repo without `origin` configured is
        // not a realistic shape for this primitive.
        let (_tmp, _remote, local) = fixture_repo_with_origin();
        run_git(&local, &["push", "origin", "--delete", "feature"]);
        run_git(&local, &["branch", "-D", "feature"]);

        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        let project = Project {
            name: "fixture".into(),
            repo: local.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: local.clone(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: Vec::new(),
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        };
        let out = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        assert!(matches!(out, DeleteOutcome::NotPresent), "{out:?}");
    }

    /// Build two linked local repos: a "remote" bare repo and a working
    /// clone whose `origin` points at it. Lets us drive `delete_branch`'s
    /// remote-side codepath without GitHub.
    fn fixture_repo_with_origin() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let remote = tmp.path().join("origin.git");
        let local = tmp.path().join("local");
        run_git(tmp.path(), &["init", "-q", "--bare", "origin.git"]);

        std::fs::create_dir_all(&local).unwrap();
        run_git(&local, &["init", "-q", "-b", "main", "."]);
        run_git(&local, &["config", "user.email", "test@example.com"]);
        run_git(&local, &["config", "user.name", "Test"]);
        run_git(&local, &["remote", "add", "origin", remote.to_str().unwrap()]);
        std::fs::write(local.join("README.md"), "hi\n").unwrap();
        run_git(&local, &["add", "README.md"]);
        run_git(&local, &["commit", "-q", "-m", "init"]);
        run_git(&local, &["push", "-u", "origin", "main"]);
        run_git(&local, &["branch", "feature"]);
        run_git(&local, &["push", "-u", "origin", "feature"]);

        (tmp, remote, local)
    }

    #[test]
    fn delete_branch_removes_local_and_remote() {
        let (_tmp, _remote, local) = fixture_repo_with_origin();

        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        let project = Project {
            name: "fixture".into(),
            repo: local.to_string_lossy().into(),
            default_branch: "main".into(),
            machines: vec![Machine {
                name: "hub".into(),
                kind: MachineKind::Local,
                work_dir: local.clone(),
                host: None,
            }],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: Vec::new(),
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        };

        let wt = local.to_string_lossy().into_owned();
        assert!(local_branch_exists(&Host::Local, &wt, "feature").unwrap());
        assert!(remote_branch_exists(&Host::Local, &wt, "feature").unwrap());

        let out = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        assert!(matches!(out, DeleteOutcome::Deleted), "{out:?}");

        assert!(!local_branch_exists(&Host::Local, &wt, "feature").unwrap());
        assert!(!remote_branch_exists(&Host::Local, &wt, "feature").unwrap());

        // Idempotent — second call sees nothing to do.
        let again = delete_branch(&project, &task_on_branch("t", "feature")).unwrap();
        assert!(matches!(again, DeleteOutcome::NotPresent), "{again:?}");
    }
}
