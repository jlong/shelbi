//! `shelbi open <name> [--as-pane]` — focus or create a workspace's tmux
//! pane. Single entry point for both the sidebar click-to-focus path and
//! the dispatch path — the "exists?" check lives here so callers don't
//! have to branch on it.
//!
//! For LOCAL workspaces, an empty pane is created with this same command
//! re-entered under `--as-pane` (the wrapper that owns the agent
//! subprocess and emits a `pane_alive=false` event on exit). The
//! lifecycle wrapper lives in the [`pane`] submodule.
//!
//! For REMOTE workspaces, the pane is a proxy window that
//! `ssh -t … tmux attach -t shelbi-w-<name>` into the workspace's own
//! remote tmux session — the lifecycle wrapper isn't deployed to remote
//! machines.

use anyhow::{anyhow, bail, Result};
use shelbi_core::{Host, Project, WorkspaceSpec};

use super::require_project;

pub mod pane;

pub fn run(project_opt: Option<String>, name: String, as_pane: bool) -> Result<()> {
    let project = require_project(project_opt)?;
    open(&project, &name, as_pane)
}

/// Top-level dispatcher for `shelbi open <name> [--as-pane]`.
///
/// Without `--as-pane`: focus the existing tmux pane if any, otherwise
/// create one. The created pane re-enters under `--as-pane` so the agent
/// is owned by a wrapper process that emits a `pane_alive=false` event
/// on exit.
///
/// With `--as-pane`: act as the pane wrapper — spawn the agent,
/// install signal handlers, and stay alive until the agent exits or a
/// signal arrives, then write the lifecycle event and (on clean exit)
/// prompt the user before tearing down so final output stays visible.
fn open(project: &str, name: &str, as_pane: bool) -> Result<()> {
    let p = shelbi_state::load_project(project).map_err(|e| anyhow!(e))?;
    let workspace = p
        .workspace(name)
        .ok_or_else(|| {
            anyhow!(
                "workspace `{name}` not declared in project `{project}` (known: {})",
                p.workspaces
                    .iter()
                    .map(|w| w.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?
        .clone();
    let machine = p
        .machine(&workspace.machine)
        .ok_or_else(|| {
            anyhow!(
                "workspace `{name}` references unknown machine `{}`",
                workspace.machine
            )
        })?
        .clone();
    let host = machine.host();

    if as_pane {
        // The wrapper is local-only: remote workspaces don't run shelbi
        // on the workspace host, so --as-pane has no meaning there.
        if !host.is_local() {
            bail!(
                "--as-pane is only valid for local workspaces \
                 (workspace `{name}` lives on a remote machine)"
            );
        }
        return pane::run(&p, &workspace, &machine);
    }
    focus_or_create(&p, &workspace, &host)
}

/// Existing pane → focus and exit. Missing pane → create one (with the
/// lifecycle wrapper for local hosts; with the legacy proxy-window for
/// remote hosts) and select it.
fn focus_or_create(
    project: &Project,
    workspace: &WorkspaceSpec,
    host: &Host,
) -> Result<()> {
    let project_session = format!("shelbi-{}", project.name);
    let target = format!("{project_session}:{}", workspace.name);

    // A window in the project session — either a local workspace pane or
    // a remote proxy we created on an earlier open. Either way, focus
    // and exit.
    if run_local_tmux(["select-window", "-t", &target]) {
        return Ok(());
    }

    match host {
        Host::Local => {
            let shelbi_bin = current_exe_string()?;
            let pane_cmd = pane::wrapper_invocation(&shelbi_bin, &project.name, &workspace.name);
            if !run_local_tmux([
                "new-window",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &workspace.name,
                "sh",
                "-c",
                &pane_cmd,
            ]) {
                bail!(
                    "couldn't create tmux window for workspace `{}`",
                    workspace.name
                );
            }
            let _ = run_local_tmux(["select-window", "-t", &target]);
            Ok(())
        }
        Host::Ssh { host: ssh_host } => {
            // Preserved verbatim from the pre-refactor focus_workspace
            // remote arm — the proxy-window mechanism is what makes a
            // devbox workspace clickable from the local sidebar. We do NOT
            // run the lifecycle wrapper here: there's no shelbi on the
            // remote, and the workspace's own tmux session is what holds
            // claude (this proxy only attaches to it).
            let remote_session = format!("shelbi-w-{}", workspace.name);
            let cmd = format!(
                "ssh -t {host} tmux attach -t {remote_session}",
                host = shelbi_agent::shell_escape(ssh_host),
                remote_session = shelbi_agent::shell_escape(&remote_session),
            );
            if !run_local_tmux([
                "new-window",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &workspace.name,
                "sh",
                "-c",
                &cmd,
            ]) {
                bail!(
                    "couldn't open proxy window for remote workspace `{}` on `{ssh_host}`",
                    workspace.name
                );
            }
            let _ = run_local_tmux(["select-window", "-t", &target]);
            Ok(())
        }
    }
}

fn run_local_tmux<I, S>(args: I) -> bool
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    std::process::Command::new("tmux")
        .args(args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()?
        .to_string_lossy()
        .into_owned())
}
