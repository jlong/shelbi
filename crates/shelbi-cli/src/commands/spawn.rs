use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::Args as ClapArgs;
use shelbi_core::{validate_agent_id, Agent, Host, Machine, Project, Status, TmuxAddr};

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Kebab-case workspace id, used as branch + worktree dir + tmux window name.
    pub id: String,
    /// Machine to run on (must be declared in the project).
    #[arg(long)]
    pub on: String,
    /// Agent runner name (must be declared in the project).
    #[arg(long)]
    pub runner: String,
    /// Initial prompt to send to the agent.
    pub prompt: String,
    /// Override the generated branch name.
    #[arg(long)]
    pub branch: Option<String>,
    /// tmux session to attach the workspace window to. Defaults to
    /// `shelbi-<project>`.
    #[arg(long, env = "SHELBI_TMUX_SESSION")]
    pub session: Option<String>,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;
    validate_agent_id(&args.id).map_err(|e| anyhow!(e))?;

    let project = shelbi_state::load_project(&project_name)
        .with_context(|| format!("loading project `{project_name}`"))?;

    // Share the workspace injection lock with `shelbi send` and task
    // dispatch. Besides serializing duplicate legacy spawns, this keeps the
    // initial text -> settle -> Enter sequence from interleaving with a send
    // that begins as soon as the agent record becomes visible.
    let _pane_injection_lock =
        shelbi_state::lock_workspace(&project_name, &args.id).map_err(|e| anyhow!(e))?;

    let machine = project
        .machine(&args.on)
        .ok_or_else(|| anyhow!("machine `{}` not in project `{project_name}`", args.on))?
        .clone();

    let runner_spec = project
        .runner(&args.runner)
        .ok_or_else(|| anyhow!("runner `{}` not declared in project", args.runner))?
        .clone();

    let host = machine.host();
    // For LOCAL workspaces we put them as a window inside `shelbi-<project>` so
    // they sit alongside the dashboard and orchestrator. For REMOTE workspaces
    // we give each workspace its own tmux session named `shelbi-w-<id>` on the
    // remote — so the workspace survives a hub disconnect, and re-attaching is
    // just `ssh host -t tmux attach -t shelbi-w-<id>`.
    let (session, window_name) = if host.is_local() {
        (
            args.session
                .clone()
                .unwrap_or_else(|| format!("shelbi-{}", project.name)),
            args.id.clone(),
        )
    } else {
        (format!("shelbi-w-{}", args.id), "agent".to_string())
    };
    let branch = args
        .branch
        .clone()
        .unwrap_or_else(|| format!("shelbi/{}", args.id));

    let worktree = worktree_path(&machine, &args.id);
    let work_dir_display = expand_tilde(&worktree);

    println!(
        "spawning agent {} on {} ({host:?})\n  branch: {}\n  worktree: {}\n  session/window: {}:{}",
        args.id,
        machine.name,
        branch,
        work_dir_display.display(),
        session,
        window_name,
    );

    // 1. Make sure the repo's .gitignore covers .shelbi/ so the parent
    //    worktree doesn't get marked dirty by our metadata.
    ensure_gitignored(&host, &machine)?;

    // 2. Create the worktree (git worktree add -b <branch> <path>).
    // Lock order is workspace -> Git worktrees/refs. Keep the inner lock
    // scoped to named checkout; pane startup must not hold it.
    let git_worktree_lock = shelbi_state::lock_git_worktrees(&project_name)
        .map_err(|e| anyhow!(e))?;
    create_worktree(&host, &machine, &branch, &worktree, &project)?;
    drop(git_worktree_lock);

    // 3. Spawn the workspace tmux pane. We open it with an interactive shell
    //    (no inline command) so the user's rc files run and pick up tools
    //    installed in shell-specific PATHs (npm-global, asdf, pyenv, nvm).
    //    Then we send-keys the cd+launch and the initial prompt.
    let addr = if host.is_local() {
        if !shelbi_tmux::has_session(&host, &session).map_err(|e| anyhow!(e))? {
            shelbi_tmux::new_session(&host, &session, "shelbi", None)
                .map_err(|e| anyhow!(e))
                .context("creating tmux session")?;
        }
        shelbi_tmux::new_window(&host, &session, &window_name, None)
            .map_err(|e| anyhow!(e))
            .context("creating workspace window")?
    } else {
        if shelbi_tmux::has_session(&host, &session).map_err(|e| anyhow!(e))? {
            bail!(
                "remote tmux session `{session}` already exists on {} — pick a new task id, \
                 or kill it with `ssh {} tmux kill-session -t {session}`",
                machine.name,
                machine.name
            );
        }
        shelbi_tmux::new_session(&host, &session, &window_name, None)
            .map_err(|e| anyhow!(e))
            .context("creating remote workspace session")?;
        TmuxAddr {
            session: session.clone(),
            window: window_name.clone(),
        }
    };

    // 4. Launch the agent in the now-interactive shell. `exec` replaces the
    //    shell so the window closes naturally when the agent exits.
    //
    //    Local: tmux server inherits the user's already-set-up login env
    //    (since they ran shelbi from a terminal), so a plain `exec` finds
    //    everything the user has on PATH.
    //
    //    Remote: tmux server was started by `ssh host -- tmux new-session …`,
    //    which runs through a NON-login non-interactive shell — so tmux
    //    (and every pane it spawns) inherits a stripped-down PATH that's
    //    missing Homebrew, asdf, nvm, etc. Re-exec through `$SHELL -lc`
    //    so the login rc files (~/.zprofile, ~/.bash_profile) run and
    //    we pick up the same PATH the user has in their own terminal.
    let launch_cmd = shelbi_agent::launch_command(&runner_spec);
    let cd_launch = if host.is_local() {
        format!(
            "cd {} && exec {}",
            shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            launch_cmd
        )
    } else {
        // Prefix with the hub-endpoint env (`SHELBI_HUB_ADDR`, plus the legacy
        // `SHELBI_HUB_SOCK` on Unix-forward hosts) so the agent's socket-write
        // paragraph (see agents/developer/instructions.md) can reach the hub
        // over whichever transport the reverse forward settled on — Unix socket
        // or the TCP loopback fallback used for Tailscale-SSH hosts. Without it
        // the agent's instructions short-circuit and worker→hub events are
        // dropped (per Phase 5's accepted residual risk).
        let hub_env = shelbi_orchestrator::workspace::remote_hub_env_prefix(&host);
        format!(
            "cd {} && {}exec \"${{SHELL:-/bin/bash}}\" -lc {}",
            shelbi_agent::shell_escape(&worktree.to_string_lossy()),
            hub_env,
            shelbi_agent::shell_escape(&launch_cmd),
        )
    };
    shelbi_tmux::send_line(&host, &addr, &cd_launch)
        .map_err(|e| anyhow!(e))
        .context("launching agent")?;

    // 5. Claude must draw its structural input box before we type. A fixed
    //    delay can land the prompt in a slow startup screen or trust dialog;
    //    once the later empty box appears, that lost text can look falsely
    //    submitted. Non-Claude runners have no supported pane parser, so they
    //    retain the conservative startup settle and explicit unverified
    //    delivery verdict.
    let submit_profile = shelbi_orchestrator::submit::SubmitProfile::for_runner(&runner_spec);
    if submit_profile.uses_claude_ui() {
        let ready = match shelbi_orchestrator::ready::wait_for_claude_ready(
            &host,
            &addr,
            shelbi_orchestrator::ready::READY_TIMEOUT,
        ) {
            Ok(ready) => ready,
            Err(probe_error) => {
                shelbi_state::append_send_event(
                    &project.name,
                    &args.id,
                    "stuck",
                    "readiness_probe_error",
                )
                .map_err(|log_error| {
                    anyhow!(
                        "waiting for Claude input readiness failed ({probe_error}); recording the stuck delivery also failed: {log_error}"
                    )
                })?;
                return Err(anyhow!(
                    "waiting for Claude input readiness failed: {probe_error}; prompt was not sent and the failure was recorded in events.log"
                ));
            }
        };
        if !ready {
            shelbi_state::append_send_event(
                &project.name,
                &args.id,
                "stuck",
                "readiness_timeout",
            )
            .map_err(|e| {
                anyhow!(
                    "Claude input readiness timed out; recording the stuck delivery also failed: {e}"
                )
            })?;
            bail!(
                "Claude input readiness timed out after {}s on {}; prompt was not sent and the failure was recorded in events.log",
                shelbi_orchestrator::ready::READY_TIMEOUT.as_secs(),
                addr.target(),
            );
        }
    } else {
        std::thread::sleep(std::time::Duration::from_millis(1500));
    }
    super::send::send_verified(
        &project.name,
        &args.id,
        &runner_spec,
        &host,
        &addr,
        &args.prompt,
    )
    .context("sending initial prompt")?;

    // 5. Write the agent state file.
    let now = Utc::now();
    let agent = Agent {
        id: args.id.clone(),
        project: project.name.clone(),
        machine: machine.name.clone(),
        runner: args.runner.clone(),
        branch: branch.clone(),
        worktree: worktree.clone(),
        status: Status::Running,
        created: now,
        updated: now,
        tmux: TmuxAddr {
            session: session.clone(),
            window: window_name.clone(),
        },
    };
    let body = format!(
        "# Task\n\n{}\n\n## Progress\n\n- spawned at {}\n",
        args.prompt,
        now.to_rfc3339()
    );
    shelbi_state::save_agent(&project.name, &agent, &body).map_err(|e| anyhow!(e))?;
    shelbi_state::append_log(&project.name, &args.id, &format!("spawn: {}", args.prompt))
        .map_err(|e| anyhow!(e))?;

    println!("✓ agent {} spawned at {}", args.id, addr.target());
    Ok(())
}

fn worktree_path(machine: &Machine, id: &str) -> PathBuf {
    machine.work_dir.join(".shelbi").join("wt").join(id)
}

fn expand_tilde(p: &std::path::Path) -> PathBuf {
    if let Some(stripped) = p.to_str().and_then(|s| s.strip_prefix("~/")) {
        if let Some(home) = dirs::home_dir() {
            return home.join(stripped);
        }
    }
    p.to_path_buf()
}

/// Add shelbi's footprint to the repo's `.gitignore` if it isn't already
/// covered: `.shelbi/` (metadata) plus the `.claude/` files shelbi deploys
/// into the worktree on every dispatch (settings.json, agent-instructions.md,
/// skills/, the ready marker). Without the `.claude/` entries a repo that
/// doesn't already ignore that dir shows those files as untracked and the
/// worktree reads as dirty. Writes to the file on the workspace's filesystem
/// via `sh -c`; never commits.
fn ensure_gitignored(host: &Host, machine: &Machine) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    // Probe each footprint independently so a repo that already ignores
    // `.shelbi/` (but not the shelbi-written `.claude/` files) still gets the
    // `.claude/` block appended. `git check-ignore` exits 0 if the path is
    // ignored, 1 if not, 128 on error.
    let mut snippet = String::new();
    if !check_ignored(host, &repo, ".shelbi/")? {
        snippet.push_str(
            "\n# shelbi worktrees + metadata (https://github.com/jlong/shelbi)\n.shelbi/\n",
        );
    }
    // Representative probe: if the ready marker isn't ignored, none of the
    // shelbi-written `.claude/` files are, so append the whole block.
    if !check_ignored(host, &repo, ".claude/shelbi-ready")? {
        snippet.push_str(
            "\n# shelbi deploy footprint written into the worktree on dispatch\n\
             .claude/settings.json\n\
             .claude/agent-instructions.md\n\
             .claude/skills/\n\
             .claude/shelbi-ready\n",
        );
    }
    if snippet.is_empty() {
        return Ok(());
    }
    let gitignore = format!("{repo}/.gitignore");
    // Append via `sh -c` so the redirect works locally and over SSH.
    let cmd = format!(
        "printf '%s' {} >> {}",
        shelbi_agent::shell_escape(&snippet),
        shelbi_agent::shell_escape(&gitignore),
    );
    shelbi_ssh::run_capture(host, ["sh", "-c", &cmd]).map_err(|e| anyhow!(e))?;
    Ok(())
}

/// `git check-ignore -q <path>` → true when the path is ignored. Exit 0 =
/// ignored, 1 = not ignored, 128 = error (treated as "not ignored" so a
/// probe failure just appends the snippet rather than blocking the spawn).
fn check_ignored(host: &Host, repo: &str, path: &str) -> Result<bool> {
    let probe = shelbi_ssh::run(host, ["git", "-C", repo, "check-ignore", "-q", path])
        .map_err(|e| anyhow!(e))?;
    Ok(probe.status.success())
}

fn create_worktree(
    host: &Host,
    machine: &Machine,
    branch: &str,
    worktree: &std::path::Path,
    project: &Project,
) -> Result<()> {
    let repo_dir = machine.work_dir.to_string_lossy().into_owned();
    let wt_str = worktree.to_string_lossy().into_owned();
    let parent_branch = project.base_branch().to_string();

    // Check if branch already exists locally. If yes, attach the worktree to it;
    // if not, create it from the default branch.
    let branch_exists = shelbi_ssh::run(
        host,
        ["git", "-C", &repo_dir, "rev-parse", "--verify", branch],
    )
    .map_err(|e| anyhow!(e))?
    .status
    .success();

    let mut args: Vec<String> = vec![
        "git".into(),
        "-C".into(),
        repo_dir.clone(),
        "worktree".into(),
        "add".into(),
    ];
    if branch_exists {
        args.push(wt_str.clone());
        args.push(branch.into());
    } else {
        args.push("-b".into());
        args.push(branch.into());
        args.push(wt_str.clone());
        args.push(parent_branch.clone());
    }

    let output = shelbi_ssh::run(host, &args).map_err(|e| anyhow!(e))?;
    if !output.status.success() {
        bail!(
            "git worktree add failed: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}
