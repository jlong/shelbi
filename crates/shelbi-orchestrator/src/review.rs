//! Review flow: check a worker's branch out into the machine's main
//! work_dir and launch a fresh claude pane there for the user to inspect
//! the changes, make small tweaks, and decide accept / send-back.
//!
//! One review per project per machine — same machine.work_dir, same tmux
//! window. Invoking review on a second task swaps the checkout and
//! restarts the pane (clearing claude context, same semantics as workers).
//!
//! Worktree conflict: git refuses to check out a branch that's already
//! checked out in another worktree. So before the review checkout we
//! release any worker worktree currently sitting on the task's branch,
//! switching it back to `default_branch` — but only if that worktree is
//! clean. A dirty worker worktree bails the review with a clear message.

use std::path::PathBuf;

use shelbi_core::{Error, Host, Machine, Project, Result, Task, TmuxAddr};

use crate::worker::worker_worktree;

/// Where the review pane lives. Local = window in the project's session;
/// remote = its own session (so an SSH drop doesn't kill the review).
pub fn review_tmux_addr(project: &Project, machine: &Machine) -> TmuxAddr {
    match machine.host() {
        Host::Local => TmuxAddr {
            session: format!("shelbi-{}", project.name),
            window: "review".into(),
        },
        Host::Ssh { .. } => TmuxAddr {
            session: format!("shelbi-r-{}", machine.name),
            window: "review".into(),
        },
    }
}

/// Resolve which machine to review on. Order of preference: explicit
/// override, the worker the task is assigned to, the first local machine
/// in the project.
pub fn resolve_review_machine<'a>(
    project: &'a Project,
    task: &Task,
    explicit: Option<&str>,
) -> Result<&'a Machine> {
    if let Some(name) = explicit {
        return project
            .machine(name)
            .ok_or_else(|| Error::UnknownMachine(name.to_string()));
    }
    if let Some(worker_name) = &task.assigned_to {
        if let Some(worker) = project.worker(worker_name) {
            if let Some(m) = project.machine(&worker.machine) {
                return Ok(m);
            }
        }
    }
    project
        .machines
        .iter()
        .find(|m| matches!(m.kind, shelbi_core::MachineKind::Local))
        .ok_or_else(|| Error::Other("project has no local machine to review on".into()))
}

/// Idempotent teardown. OK if the pane was never created. On the SSH path
/// the whole session is dedicated to review, so we key liveness off
/// `has_session` rather than a window-name match — tmux's
/// `automatic-rename` retitles the window once claude takes over the
/// pane, so a name-based check would miss live sessions and let the next
/// `new_session` collide. (Same reasoning as `kill_worker_pane`.) After
/// killing we poll until tmux confirms the session is gone, so a flaky
/// SSH round-trip surfaces as a clear error instead of a silent skip
/// followed by a `duplicate session` failure on `new_session`.
pub fn kill_review_pane(host: &Host, addr: &TmuxAddr) -> Result<()> {
    match host {
        Host::Local => {
            // Local: the review window is one of many in the shared
            // project session. We still gate the kill on a window
            // probe — `kill-window -t session:review` would otherwise
            // return non-zero if the window was auto-renamed away, and
            // that's not actionable.
            let probe = shelbi_ssh::run(
                host,
                ["tmux", "list-windows", "-t", &addr.session, "-F", "#W"],
            )
            .map_err(Error::Io)?;
            if !probe.status.success() {
                return Ok(());
            }
            let stdout = String::from_utf8_lossy(&probe.stdout);
            if !stdout.lines().any(|w| w.trim() == addr.window) {
                return Ok(());
            }
            let _ = shelbi_ssh::run(host, ["tmux", "kill-window", "-t", &addr.target()])
                .map_err(Error::Io)?;
        }
        Host::Ssh { .. } => {
            if !shelbi_tmux::has_session(host, &addr.session)? {
                return Ok(());
            }
            let _ = shelbi_ssh::run(host, ["tmux", "kill-session", "-t", &addr.session])
                .map_err(Error::Io)?;
            // tmux normally tears the session down synchronously, but if
            // the kill races (or the SSH round-trip swallowed an error)
            // we must NOT return Ok with a live session — start_review's
            // next step is `new_session` and the names would collide.
            for _ in 0..20 {
                if !shelbi_tmux::has_session(host, &addr.session)? {
                    return Ok(());
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            return Err(Error::Other(format!(
                "tmux session `{}` still present after kill-session — \
                 review cannot start; try `tmux kill-session -t {}` on \
                 the remote and retry",
                addr.session, addr.session
            )));
        }
    }
    Ok(())
}

/// Look up the project + task on disk and kick off the review for it.
/// Returns the tmux target string (`session:window`) of the review pane
/// the caller should focus. Used by the TUI's sidebar and the Ctrl+P
/// palette so they share one code path.
pub fn start_review_by_id(project_name: &str, task_id: &str) -> Result<String> {
    let project = shelbi_state::load_project(project_name)?;
    let tf = shelbi_state::load_task(project_name, task_id)?;
    let machine = resolve_review_machine(&project, &tf.task, None)?;
    let addr = start_review(ReviewSpec {
        project: &project,
        machine,
        task: &tf.task,
        task_body: &tf.body,
    })?;
    Ok(addr.target())
}

/// Spec passed to `start_review`. The body is the task's markdown body
/// (the prompt context the user gave the worker), included in the
/// reviewer's opening prompt so it knows what the work was for.
pub struct ReviewSpec<'a> {
    pub project: &'a Project,
    pub machine: &'a Machine,
    pub task: &'a Task,
    pub task_body: &'a str,
}

/// Preflight → checkout → restart review pane → send prompt.
pub fn start_review(spec: ReviewSpec<'_>) -> Result<TmuxAddr> {
    let host = spec.machine.host();
    let branch = spec
        .task
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{}", spec.task.id));

    preflight_workdir(&host, spec.machine)?;
    release_branch_from_worker_worktrees(&host, spec.project, spec.machine, &branch)?;
    checkout(&host, spec.machine, &branch)?;

    let addr = review_tmux_addr(spec.project, spec.machine);
    kill_review_pane(&host, &addr)?;

    let runner_name = &spec.project.orchestrator.runner;
    let runner = spec
        .project
        .runner(runner_name)
        .ok_or_else(|| Error::UnknownRunner(runner_name.clone()))?
        .clone();

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

    // Local: tmux server inherits the user's already-set-up login env, so
    // a plain invocation finds everything on PATH. Remote: tmux was
    // started over SSH through a non-login non-interactive shell and
    // inherits a stripped-down PATH that's missing Homebrew, asdf, nvm,
    // etc. Re-exec through `$SHELL -lc` so the login rc files run and we
    // pick up the same PATH the user has in their own terminal — otherwise
    // claude launches without its expected env and dies with "Input must
    // be provided either through stdin or as a prompt argument when using
    // --print".
    let launch = shelbi_agent::launch_command(&runner);
    let cd_launch = if host.is_local() {
        format!(
            "cd {wd} && {launch}",
            wd = shelbi_agent::shell_escape(&spec.machine.work_dir.to_string_lossy()),
        )
    } else {
        format!(
            "cd {wd} && exec \"${{SHELL:-/bin/bash}}\" -lc {launch}",
            wd = shelbi_agent::shell_escape(&spec.machine.work_dir.to_string_lossy()),
            launch = shelbi_agent::shell_escape(&launch),
        )
    };
    shelbi_tmux::send_line(&host, &addr, &cd_launch)?;

    std::thread::sleep(std::time::Duration::from_millis(1500));
    let prompt = compose_review_prompt(&spec.task.id, &branch, spec.task_body);
    shelbi_tmux::send_line(&host, &addr, &prompt)?;

    Ok(addr)
}

fn compose_review_prompt(task_id: &str, branch: &str, body: &str) -> String {
    let trimmed = body.trim();
    let body_section = if trimmed.is_empty() {
        format!("# Task {task_id}")
    } else {
        trimmed.to_string()
    };
    format!(
        "You are reviewing task `{task_id}` on branch `{branch}`. The \
         changes are checked out in this working directory — the user is \
         about to inspect them and run the app.\n\n\
         If the user asks for small tweaks, make them. If the work needs \
         substantial rework, advise them to run:\n\n\
         shelbi task move {task_id} --to todo\n\n\
         to send it back. If everything looks good and the user accepts, \
         they'll move it to done.\n\n\
         Task context:\n\n\
         {body_section}"
    )
}

fn preflight_workdir(host: &Host, machine: &Machine) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &repo, "status", "--porcelain"])?;
    // .shelbi/ is shelbi's metadata — ignore it from the cleanliness check
    // even if the user hasn't gitignored it yet. (Same carve-out merge.rs
    // applies for the same reason.)
    let user_dirty: Vec<&str> = dirty
        .lines()
        .filter(|l| {
            let path = l.get(3..).unwrap_or("");
            !(path.starts_with(".shelbi/") || path == ".shelbi" || path == ".gitignore")
        })
        .collect();
    if !user_dirty.is_empty() {
        return Err(Error::Other(format!(
            "review work_dir at {repo} has uncommitted changes — commit or \
             stash before reviewing another branch:\n{}",
            user_dirty.join("\n")
        )));
    }
    Ok(())
}

/// If a worker worktree on this machine is currently on `branch`, switch
/// it to `default_branch` so the main work_dir is free to check out
/// `branch`. Bails on a dirty worker worktree (we'd silently lose work).
fn release_branch_from_worker_worktrees(
    host: &Host,
    project: &Project,
    machine: &Machine,
    branch: &str,
) -> Result<()> {
    for worker in &project.workers {
        if worker.machine != machine.name {
            continue;
        }
        let wt: PathBuf = worker_worktree(machine, worker);
        let wt_str = wt.to_string_lossy().into_owned();
        // Skip workers without an actual worktree yet.
        let exists = shelbi_ssh::run(host, ["test", "-e", &format!("{wt_str}/.git")])
            .map_err(Error::Io)?
            .status
            .success();
        if !exists {
            continue;
        }
        let head = shelbi_ssh::run_capture(
            host,
            ["git", "-C", &wt_str, "rev-parse", "--abbrev-ref", "HEAD"],
        )?;
        if head.trim() != branch {
            continue;
        }
        let dirty = shelbi_ssh::run_capture(host, ["git", "-C", &wt_str, "status", "--porcelain"])?;
        if !dirty.trim().is_empty() {
            return Err(Error::Other(format!(
                "worker `{}`'s worktree is on `{branch}` with uncommitted \
                 changes — commit, stash, or discard before reviewing",
                worker.name
            )));
        }
        // Detach HEAD on the worker's worktree — frees the branch ref so
        // the main clone can claim it. We avoid switching to a named
        // branch here because the natural choice (`default_branch`) is
        // typically checked out in the main clone, and git refuses to
        // double-claim a branch across worktrees. sync_worktree will
        // re-attach to the right branch the next time the worker gets a
        // task.
        let out = shelbi_ssh::run(host, ["git", "-C", &wt_str, "checkout", "--detach"])
            .map_err(Error::Io)?;
        if !out.status.success() {
            return Err(Error::Command {
                cmd: format!("git -C {wt_str} checkout --detach"),
                status: out.status.to_string(),
                stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
            });
        }
    }
    Ok(())
}

fn checkout(host: &Host, machine: &Machine, branch: &str) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    let out = shelbi_ssh::run(host, ["git", "-C", &repo, "checkout", branch])
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Command {
            cmd: format!("git -C {repo} checkout {branch}"),
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_core::{AgentRunnerSpec, MachineKind, OrchestratorSpec, WorkerSpec};
    use std::collections::BTreeMap;

    fn fixture() -> (Project, Task) {
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            AgentRunnerSpec { command: "claude".into(), flags: vec![] },
        );
        let p = Project {
            name: "p".into(),
            repo: "r".into(),
            default_branch: "main".into(),
            machines: vec![
                Machine {
                    name: "hub".into(),
                    kind: MachineKind::Local,
                    work_dir: "/tmp/p".into(),
                    host: None,
                },
                Machine {
                    name: "m2".into(),
                    kind: MachineKind::Ssh,
                    work_dir: "/work/p".into(),
                    host: Some("m2.local".into()),
                },
            ],
            orchestrator: OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workers: vec![
                WorkerSpec { name: "alice".into(), machine: "hub".into(), runner: "claude".into() },
                WorkerSpec { name: "bob".into(), machine: "m2".into(), runner: "claude".into() },
            ],
            worker_poll_interval_secs: 5,
            worker_permissions_mode: "auto".into(),
            worker_settings_template: None,
        };
        let now = chrono::Utc::now();
        let t = Task {
            id: "fix-thing".into(),
            title: "Fix the thing".into(),
            column: shelbi_core::Column::Review,
            priority: 0,
            assigned_to: Some("alice".into()),
            branch: Some("shelbi/fix-thing".into()),
            depends_on: Vec::new(),
            prefers_machine: None,
            created_at: now,
            updated_at: now,
        };
        (p, t)
    }

    #[test]
    fn local_review_window_lives_in_project_session() {
        let (p, _) = fixture();
        let addr = review_tmux_addr(&p, &p.machines[0]);
        assert_eq!(addr.session, "shelbi-p");
        assert_eq!(addr.window, "review");
    }

    #[test]
    fn remote_review_gets_per_machine_session() {
        let (p, _) = fixture();
        let addr = review_tmux_addr(&p, &p.machines[1]);
        assert_eq!(addr.session, "shelbi-r-m2");
        assert_eq!(addr.window, "review");
    }

    #[test]
    fn machine_resolution_prefers_assigned_worker() {
        let (p, t) = fixture();
        let m = resolve_review_machine(&p, &t, None).unwrap();
        assert_eq!(m.name, "hub"); // alice is on hub
    }

    #[test]
    fn machine_resolution_falls_back_to_first_local() {
        let (p, mut t) = fixture();
        t.assigned_to = None;
        let m = resolve_review_machine(&p, &t, None).unwrap();
        assert_eq!(m.name, "hub");
    }

    #[test]
    fn machine_resolution_honors_explicit_override() {
        let (p, t) = fixture();
        let m = resolve_review_machine(&p, &t, Some("m2")).unwrap();
        assert_eq!(m.name, "m2");
    }

    #[test]
    fn review_prompt_includes_task_id_branch_and_send_back_instruction() {
        let prompt = compose_review_prompt("fix-thing", "shelbi/fix-thing", "Some context.");
        assert!(prompt.contains("fix-thing"));
        assert!(prompt.contains("shelbi/fix-thing"));
        assert!(prompt.contains("shelbi task move fix-thing --to todo"));
        assert!(prompt.contains("Some context."));
    }
}
