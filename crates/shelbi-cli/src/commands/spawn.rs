use std::path::PathBuf;

use anyhow::{anyhow, bail, Context, Result};
use chrono::Utc;
use clap::Args as ClapArgs;
use shelbi_core::{
    validate_agent_id, Agent, Host, Machine, Project, Status, TmuxAddr,
};

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// Kebab-case worker id, used as branch + worktree dir + tmux window name.
    pub id: String,
    /// Machine to run on (must be declared in the project).
    #[arg(long)]
    pub on: String,
    /// Agent runner name (must be declared in the project).
    #[arg(long)]
    pub runner: String,
    /// Initial prompt to send to the agent.
    pub prompt: String,
    /// Override the default branch name (`shelbi/<id>`).
    #[arg(long)]
    pub branch: Option<String>,
    /// tmux session to attach the worker window to. Defaults to
    /// `shelbi-<project>`.
    #[arg(long, env = "SHELBI_TMUX_SESSION")]
    pub session: Option<String>,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;
    validate_agent_id(&args.id).map_err(|e| anyhow!(e))?;

    let project = shelbi_state::load_project(&project_name)
        .with_context(|| format!("loading project `{project_name}`"))?;

    let machine = project
        .machine(&args.on)
        .ok_or_else(|| anyhow!("machine `{}` not in project `{project_name}`", args.on))?
        .clone();

    let runner_spec = project
        .runner(&args.runner)
        .ok_or_else(|| anyhow!("runner `{}` not declared in project", args.runner))?
        .clone();

    let host = machine.host();
    // For LOCAL workers we put them as a window inside `shelbi-<project>` so
    // they sit alongside the dashboard and orchestrator. For REMOTE workers
    // we give each worker its own tmux session named `shelbi-w-<id>` on the
    // remote — so the worker survives a hub disconnect, and re-attaching is
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
    create_worktree(&host, &machine, &branch, &worktree, &project)?;

    // 3. Spawn the worker tmux pane. We open it with an interactive shell
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
            .context("creating worker window")?
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
            .context("creating remote worker session")?;
        TmuxAddr {
            session: session.clone(),
            window: window_name.clone(),
        }
    };

    // 4. Launch the agent in the now-interactive shell. `exec` replaces the
    //    shell so the window closes naturally when the agent exits.
    let launch_cmd = shelbi_agent::launch_command(&runner_spec);
    let cd_launch = format!(
        "cd {} && exec {}",
        shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        launch_cmd
    );
    shelbi_tmux::send_line(&host, &addr, &cd_launch)
        .map_err(|e| anyhow!(e))
        .context("launching agent")?;

    // 5. Give the agent a moment to boot before piping the initial prompt
    //    in. claude/codex/etc. tend to print a banner + wait for the TTY
    //    to settle; sending too early can drop the first character.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    shelbi_tmux::send_line(&host, &addr, &args.prompt)
        .map_err(|e| anyhow!(e))
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

/// Add `.shelbi/` to the repo's `.gitignore` if it isn't already covered.
/// Writes to the file on the worker's filesystem via `sh -c`; never commits.
fn ensure_gitignored(host: &Host, machine: &Machine) -> Result<()> {
    let repo = machine.work_dir.to_string_lossy().into_owned();
    // `git check-ignore` exits 0 if the path is ignored, 1 if not, 128 on error.
    let probe = shelbi_ssh::run(
        host,
        ["git", "-C", &repo, "check-ignore", "-q", ".shelbi/"],
    )
    .map_err(|e| anyhow!(e))?;
    if probe.status.success() {
        return Ok(());
    }
    let gitignore = format!("{repo}/.gitignore");
    let snippet =
        "\n# shelbi worktrees + metadata (https://github.com/jlong/shelbi)\n.shelbi/\n";
    // Append via `sh -c` so the redirect works locally and over SSH.
    let cmd = format!(
        "printf '%s' {} >> {}",
        shelbi_agent::shell_escape(snippet),
        shelbi_agent::shell_escape(&gitignore),
    );
    shelbi_ssh::run_capture(host, ["sh", "-c", &cmd]).map_err(|e| anyhow!(e))?;
    Ok(())
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
    let parent_branch = project.default_branch.clone();

    // Check if branch already exists locally. If yes, attach the worktree to it;
    // if not, create it from the default branch.
    let branch_exists = shelbi_ssh::run(
        host,
        [
            "git", "-C", &repo_dir, "rev-parse", "--verify", branch,
        ],
    )
    .map_err(|e| anyhow!(e))?
    .status
    .success();

    let mut args: Vec<String> = vec!["git".into(), "-C".into(), repo_dir.clone(), "worktree".into(), "add".into()];
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
