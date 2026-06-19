//! Worker lifecycle: the pre-declared agent slots that pick up Kanban
//! tasks. See [`crate::ensure_dashboard`] for the project's overall tmux
//! layout; this module is concerned only with the per-worker slot.
//!
//! Each worker owns a stable worktree at
//! `<machine.work_dir>/.shelbi/wt/<worker-name>`. The worktree persists
//! across tasks; the worker switches branches between assignments. The
//! worker's tmux pane (window for local hub workers, session for remote
//! workers) is killed and re-created on every assignment to clear the
//! agent's context — that's the user-specified semantics.
//!
//! Reviewer hint: this module does no state writes to task files; the
//! caller (CLI) is responsible for updating `assigned_to` / `branch` /
//! `column`. We just stand up the worktree + tmux pane + claude.

use std::path::PathBuf;

use shelbi_core::{Error, Host, Machine, Project, Result, TmuxAddr, WorkerSpec};

/// Where a worker's pane lives in tmux. Local workers get a window in the
/// project session; remote workers get their own session (so they survive
/// SSH drops).
pub fn worker_tmux_addr(project: &Project, worker: &WorkerSpec) -> Result<TmuxAddr> {
    let machine = project
        .machine(&worker.machine)
        .ok_or_else(|| Error::UnknownMachine(worker.machine.clone()))?;
    Ok(match machine.host() {
        Host::Local => TmuxAddr {
            session: format!("shelbi-{}", project.name),
            window: worker.name.clone(),
        },
        Host::Ssh { .. } => TmuxAddr {
            session: format!("shelbi-w-{}", worker.name),
            window: "agent".into(),
        },
    })
}

/// `<machine.work_dir>/.shelbi/wt/<worker-name>` — the worker's persistent
/// worktree path on its machine.
pub fn worker_worktree(machine: &Machine, worker: &WorkerSpec) -> PathBuf {
    machine.work_dir.join(".shelbi").join("wt").join(&worker.name)
}

/// Does the worker have a live tmux pane right now?
pub fn worker_pane_alive(host: &Host, addr: &TmuxAddr) -> Result<bool> {
    // Local: check `session:window` exists. Remote: it's a whole session.
    // `tmux list-windows -t session -F #W | grep -w window` does both.
    let out = shelbi_ssh::run(
        host,
        ["tmux", "list-windows", "-t", &addr.session, "-F", "#W"],
    )
    .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|w| w.trim() == addr.window))
}

/// Kill the worker's pane (idempotent — silently OK if already gone).
pub fn kill_worker_pane(host: &Host, addr: &TmuxAddr) -> Result<()> {
    // Local: `kill-window -t session:window`. Remote: `kill-session -t
    // session` (the session IS the worker). Use list to figure out which.
    if !worker_pane_alive(host, addr)? {
        return Ok(());
    }
    // Count windows in the session — if 1, killing the window would also
    // kill the session (true for remote workers). Either way, the right
    // verb on the session itself is fine for remote, and `kill-window`
    // for a worker window in the multi-window project session is fine for
    // local. Differentiate by host kind.
    match host {
        Host::Local => {
            let _ = shelbi_ssh::run(host, ["tmux", "kill-window", "-t", &addr.target()])
                .map_err(Error::Io)?;
        }
        Host::Ssh { .. } => {
            let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &addr.session])
                .map_err(Error::Io)?;
        }
    }
    Ok(())
}

/// Spec for `start_worker_on_task`. We don't take a `&Task` because the
/// caller may have a fresh task id without a frontmatter file yet.
pub struct StartSpec<'a> {
    pub project: &'a Project,
    pub worker: &'a WorkerSpec,
    pub task_id: &'a str,
    pub branch: &'a str,
    /// Body of the task markdown — appended to the prompt as context.
    pub task_body: &'a str,
}

/// Tear down the worker's pane, switch its worktree to `branch` (creating
/// the worktree off `default_branch` and the branch off `default_branch` if
/// needed), and start the runner with an initial prompt. Bails on a dirty
/// worktree so the user doesn't silently lose work.
pub fn start_worker_on_task(spec: StartSpec<'_>) -> Result<TmuxAddr> {
    let machine = spec
        .project
        .machine(&spec.worker.machine)
        .ok_or_else(|| Error::UnknownMachine(spec.worker.machine.clone()))?
        .clone();
    let runner = spec
        .project
        .runner(&spec.worker.runner)
        .ok_or_else(|| Error::UnknownRunner(spec.worker.runner.clone()))?
        .clone();

    let host = machine.host();
    let worktree = worker_worktree(&machine, spec.worker);
    let addr = worker_tmux_addr(spec.project, spec.worker)?;

    // 1. Make sure the worktree exists + is on the right branch, clean.
    sync_worktree(
        &host,
        &machine,
        &worktree,
        spec.branch,
        &spec.project.default_branch,
    )?;

    // 2. Reset the tmux pane — that's how we clear context. If it doesn't
    //    exist yet, this is a no-op; otherwise the next step recreates it.
    kill_worker_pane(&host, &addr)?;

    // 3. Create the pane. Start with an interactive shell (no `-c <cmd>`)
    //    so the user's rc files run and the pane outlives the agent
    //    process. Local = window in the project session; remote = its own
    //    session so the worker survives an SSH drop.
    match &host {
        Host::Local => {
            if !shelbi_tmux::has_session(&host, &addr.session)? {
                shelbi_tmux::new_session(&host, &addr.session, &addr.window, None)?;
            } else {
                shelbi_tmux::new_window(&host, &addr.session, &addr.window, None)?;
            }
        }
        Host::Ssh { .. } => {
            shelbi_tmux::new_session(&host, &addr.session, &addr.window, None)?;
        }
    }

    // 4. cd into the worktree and launch the agent. No `exec` — when the
    //    agent exits, the shell stays so the worker pane is reusable.
    let launch = shelbi_agent::launch_command(&runner);
    let cd_launch = format!(
        "cd {wd} && {launch}",
        wd = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
    );
    shelbi_tmux::send_line(&host, &addr, &cd_launch)?;

    // 5. Let the agent's TTY settle before we type into it (same reason as
    //    spawn — banners + prompt redraws can swallow the first chars).
    std::thread::sleep(std::time::Duration::from_millis(1500));
    let prompt = compose_prompt(spec.task_id, spec.branch, spec.task_body);
    shelbi_tmux::send_line(&host, &addr, &prompt)?;

    Ok(addr)
}

/// Build the initial prompt: the task body + the loop-closing instruction
/// that tells the worker how to mark itself done.
fn compose_prompt(task_id: &str, branch: &str, body: &str) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}\n")
    } else {
        trimmed.to_string()
    };
    format!(
        "{body_section}\n\n\
         ---\n\
         You are working on task `{task_id}` on branch `{branch}`. When \
         the work is complete and committed, run:\n\
         \n\
         shelbi task move {task_id} --to review\n\
         \n\
         to hand off for review."
    )
}

/// Ensure the worktree exists and is checked out on `branch`. Creates the
/// worktree off the project's default branch if absent, creates the branch
/// off the default if it doesn't exist yet, and bails if the worktree has
/// uncommitted changes (otherwise switching branches would lose work).
fn sync_worktree(
    host: &Host,
    machine: &Machine,
    worktree: &std::path::Path,
    branch: &str,
    default_branch: &str,
) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();

    let worktree_exists = shelbi_ssh::run(
        host,
        ["test", "-d", &format!("{wt_str}/.git")],
    )
    .map_err(Error::Io)?
    .status
    .success()
        || shelbi_ssh::run(host, ["test", "-f", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();

    let branch_exists = shelbi_ssh::run(
        host,
        ["git", "-C", &repo, "rev-parse", "--verify", branch],
    )
    .map_err(Error::Io)?
    .status
    .success();

    if !worktree_exists {
        // Fresh worktree off the requested branch (or off the default if
        // the branch is also new).
        let mut argv: Vec<String> = vec![
            "git".into(),
            "-C".into(),
            repo.clone(),
            "worktree".into(),
            "add".into(),
        ];
        if branch_exists {
            argv.push(wt_str.clone());
            argv.push(branch.into());
        } else {
            argv.push("-b".into());
            argv.push(branch.into());
            argv.push(wt_str.clone());
            argv.push(default_branch.into());
        }
        let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: argv.join(" "),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
        return Ok(());
    }

    // Already exists — make sure it's clean and on the right branch.
    let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "status", "--porcelain"])?;
    if !dirty.trim().is_empty() {
        return Err(Error::Other(format!(
            "worker worktree at {wt_str} has uncommitted changes — \
             commit, stash, or discard before assigning a new task:\n{dirty}"
        )));
    }

    let current = shelbi_ssh::run_capture(
        host,
        ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
    )?;
    if current.trim() == branch {
        return Ok(());
    }

    // Switch (and create the branch off default if it doesn't exist).
    let mut argv: Vec<String> = vec!["git".into(), "-C".into(), wt_str.clone(), "checkout".into()];
    if !branch_exists {
        argv.push("-b".into());
        argv.push(branch.into());
        argv.push(default_branch.into());
    } else {
        argv.push(branch.into());
    }
    let out = shelbi_ssh::run(host, &argv).map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: argv.join(" "),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec};
    use std::collections::BTreeMap;

    fn fixture_project() -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
            },
        );
        Project {
            name: "myapp".into(),
            repo: "git@example:repo.git".into(),
            default_branch: "main".into(),
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/myapp".into(),
                    host: None,
                },
                Machine {
                    name: "m2".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/myapp".into(),
                    host: Some("m2.local".into()),
                },
            ],
            orchestrator: OrchestratorSpec {
                runner: "claude".into(),
            },
            agent_runners: runners,
            editor: None,
            workers: vec![
                WorkerSpec {
                    name: "alice".into(),
                    machine: "hub".into(),
                    runner: "claude".into(),
                },
                WorkerSpec {
                    name: "bob".into(),
                    machine: "m2".into(),
                    runner: "claude".into(),
                },
            ],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
        }
    }

    #[test]
    fn local_worker_lives_in_project_session_window() {
        let p = fixture_project();
        let addr = worker_tmux_addr(&p, &p.workers[0]).unwrap();
        assert_eq!(addr.session, "shelbi-myapp");
        assert_eq!(addr.window, "alice");
    }

    #[test]
    fn remote_worker_gets_its_own_session() {
        let p = fixture_project();
        let addr = worker_tmux_addr(&p, &p.workers[1]).unwrap();
        assert_eq!(addr.session, "shelbi-w-bob");
        assert_eq!(addr.window, "agent");
    }

    #[test]
    fn worktree_path_under_machine_workdir() {
        let p = fixture_project();
        let wt = worker_worktree(&p.machines[0], &p.workers[0]);
        assert_eq!(wt, PathBuf::from("/tmp/myapp/.shelbi/wt/alice"));
    }

    #[test]
    fn prompt_includes_task_id_branch_and_done_instruction() {
        let prompt = compose_prompt("fix-login", "shelbi/fix-login", "Fix the Safari SSO bug.");
        assert!(prompt.contains("Fix the Safari SSO bug."));
        assert!(prompt.contains("fix-login"));
        assert!(prompt.contains("shelbi/fix-login"));
        assert!(prompt.contains("shelbi task move fix-login --to review"));
    }
}
