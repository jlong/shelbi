use anyhow::{anyhow, bail, Result};
use clap::Args as ClapArgs;
use shelbi_core::TmuxAddr;

use super::require_project;

#[derive(Debug, ClapArgs)]
pub struct Args {
    /// tmux session to use. Defaults to `shelbi-<project>`.
    #[arg(long, env = "SHELBI_TMUX_SESSION")]
    pub session: Option<String>,
    /// Print attach instructions and exit even if the orchestrator is
    /// already running.
    #[arg(long)]
    pub status: bool,
}

pub fn run(project_opt: Option<String>, args: Args) -> Result<()> {
    let project_name = require_project(project_opt)?;
    let project = shelbi_state::load_project(&project_name).map_err(|e| anyhow!(e))?;

    // Orchestrator runs on the hub (the first `local` machine in the project).
    let hub = project
        .machines
        .iter()
        .find(|m| matches!(m.kind, shelbi_core::MachineKind::Local))
        .ok_or_else(|| anyhow!("project `{project_name}` has no local hub machine"))?;
    let host = hub.host();

    let runner_spec = project
        .runner(&project.orchestrator.runner)
        .ok_or_else(|| {
            anyhow!(
                "orchestrator runner `{}` not declared in project",
                project.orchestrator.runner
            )
        })?
        .clone();

    let session = args
        .session
        .clone()
        .unwrap_or_else(|| format!("shelbi-{}", project.name));
    let window_name = "orchestrator";
    let addr = TmuxAddr {
        session: session.clone(),
        window: window_name.into(),
    };

    // Ensure session exists.
    if !shelbi_tmux::has_session(&host, &session).map_err(|e| anyhow!(e))? {
        shelbi_tmux::new_session(&host, &session, "shelbi", None).map_err(|e| anyhow!(e))?;
    }

    // Materialize orchestrator workdir + CLAUDE.md prompt.
    let workdir = shelbi_state::project_dir(&project_name).map_err(|e| anyhow!(e))?;
    shelbi_state::ensure_dir(&workdir).map_err(|e| anyhow!(e))?;
    let prompt = shelbi_orchestrator::system_prompt(&project_name).map_err(|e| anyhow!(e))?;
    std::fs::write(workdir.join("CLAUDE.md"), &prompt)?;

    // Detect whether the orchestrator window already exists.
    let existing = shelbi_ssh::run_capture(
        &host,
        ["tmux", "list-windows", "-t", &session, "-F", "#W"],
    )
    .map_err(|e| anyhow!(e))?;
    let already = existing.lines().any(|w| w.trim() == window_name);

    if already {
        println!("orchestrator already running in {}:{window_name}", session);
        print_attach(&session, window_name);
        return Ok(());
    }
    if args.status {
        bail!("orchestrator is not running (use `shelbi orchestrate` to start it)");
    }

    // Launch.
    let launch = shelbi_agent::launch_command(&runner_spec);
    let env_prefix = format!(
        "SHELBI_PROJECT={} SHELBI_TMUX_SESSION={}",
        shelbi_agent::shell_escape(&project_name),
        shelbi_agent::shell_escape(&session),
    );
    let cd_cmd = format!(
        "cd {} && {} {}",
        shelbi_agent::shell_escape(&workdir.to_string_lossy()),
        env_prefix,
        launch,
    );
    shelbi_tmux::new_window(&host, &session, window_name, Some(&cd_cmd))
        .map_err(|e| anyhow!(e))?;

    println!(
        "✓ orchestrator `{}` started in {}",
        runner_spec.command,
        addr.target()
    );
    println!("  workdir: {}", workdir.display());
    println!("  CLAUDE.md ({} bytes) written", prompt.len());
    print_attach(&session, window_name);
    Ok(())
}

fn print_attach(session: &str, window: &str) {
    println!();
    println!("attach with:");
    if std::env::var("TMUX").is_ok() {
        println!("  tmux select-window -t {session}:{window}");
    } else {
        println!("  tmux attach -t {session} \\; select-window -t {window}");
    }
}
