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
//!
//! Both of those create-arms only run for a workspace that's mid-task
//! (an active/handoff task assigned). An IDLE workspace gets a plain
//! interactive shell in its worktree instead — see [`open_idle_shell`] —
//! marked as user-occupied so dispatch skips the slot while it's open.

use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use shelbi_core::{Host, Machine, Project, WorkspaceSpec};
use shelbi_orchestrator::workspace as orch_workspace;

use super::require_project;

pub mod pane;

pub fn run(project_opt: Option<String>, name: String, as_pane: bool, resume: bool) -> Result<()> {
    let project = require_project(project_opt)?;
    open(&project, &name, as_pane, resume)
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
fn open(project: &str, name: &str, as_pane: bool, resume: bool) -> Result<()> {
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
        return pane::run(&p, &workspace, &machine, resume);
    }
    // `resume` only applies to the pane-wrapper path (it selects `--continue`);
    // a plain focus-or-create is the sidebar click and never resumes.
    focus_or_create(&p, &workspace, &machine, &host)
}

/// Existing pane → focus and exit. Missing pane → what gets created
/// depends on whether the workspace is mid-task:
///
/// - **Task assigned** (active or handoff category): relaunch the worker —
///   the lifecycle wrapper for local hosts, the legacy proxy-window for
///   remote hosts.
/// - **Idle** (nothing assigned): open a plain interactive shell in the
///   workspace's worktree instead ([`open_idle_shell`]), so a sidebar
///   click on an idle workspace lands the user "in" it rather than
///   booting a bare agent.
fn focus_or_create(
    project: &Project,
    workspace: &WorkspaceSpec,
    machine: &Machine,
    host: &Host,
) -> Result<()> {
    let project_session = format!("shelbi-{}", project.name);
    // `=` anchors the window-name half so tmux matches it EXACTLY rather
    // than by prefix: without it, `shelbi open web` would resolve (and
    // focus) an existing `web-api` window and never create `web`.
    let target = format!("{project_session}:={}", workspace.name);

    // A window in the project session — either a local workspace pane or
    // a remote proxy we created on an earlier open. Either way, focus
    // and exit. This is also what keeps the live-worker click behavior
    // unchanged: a running worker always has this window.
    if run_local_tmux(["select-window", "-t", &target]) {
        return Ok(());
    }

    if pane::assigned_task_for_workspace(&project.name, &workspace.name).is_none() {
        return open_idle_shell(project, workspace, machine, host);
    }

    match host {
        Host::Local => {
            let shelbi_bin = current_exe_string()?;
            let pane_cmd = pane::wrapper_invocation(&shelbi_bin, &project.name, &workspace.name);
            // `-S`: if a window with this name already raced into existence
            // between the select-window check above and here, select it
            // instead of creating a duplicate pane (and a duplicate agent)
            // on the same worktree. Closes the TOCTOU two rapid opens hit.
            if let Err(stderr) = run_local_tmux_checked([
                "new-window",
                "-S",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &workspace.name,
                "sh",
                "-c",
                &pane_cmd,
            ]) {
                bail!(
                    "couldn't create tmux window for workspace `{}`: {stderr}",
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
            // `-S`: same duplicate-guard as the local arm — select an
            // existing proxy window rather than stacking a second one.
            if let Err(stderr) = run_local_tmux_checked([
                "new-window",
                "-S",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &workspace.name,
                "sh",
                "-c",
                &cmd,
            ]) {
                bail!(
                    "couldn't open proxy window for remote workspace `{}` on `{ssh_host}`: {stderr}",
                    workspace.name
                );
            }
            let _ = run_local_tmux(["select-window", "-t", &target]);
            Ok(())
        }
    }
}

/// Open a plain interactive shell "in" an idle workspace.
///
/// Local: the workspace window's pane runs the user's default shell (no
/// pane command → tmux's `default-command`, a login shell) with its cwd
/// in the worktree — or the machine's `work_dir` when the worktree has
/// been detached/pruned (as happens after handoff). Remote: the usual
/// `shelbi-w-<name>` session is stood up on the workspace's machine (same
/// tmux/ssh plumbing as a worker launch), a `cd` line is sent into its
/// default shell, and the local window is the standard ssh proxy attach.
///
/// The slot is stamped with [`orch_workspace::USER_SHELL_OPTION`] so
/// dispatch refuses to clobber it and `shelbi workspace list` renders it
/// as user-occupied. Deliberately NOT a shelbi-managed agent pane: no
/// lifecycle wrapper, so exiting the shell closes the slot without a
/// `pane_alive=false` event, and supervision never restarts it (a
/// task-less pane death has nothing to relaunch).
fn open_idle_shell(
    project: &Project,
    workspace: &WorkspaceSpec,
    machine: &Machine,
    host: &Host,
) -> Result<()> {
    let project_session = format!("shelbi-{}", project.name);
    let target = format!("{project_session}:={}", workspace.name);
    let worktree = orch_workspace::workspace_worktree(machine, workspace);

    match host {
        Host::Local => {
            let dir = shell_window_dir(worktree, &machine.work_dir);
            // `-S`: same TOCTOU duplicate-guard as the worker arms — a
            // window that raced into existence is selected, not doubled.
            if let Err(stderr) = run_local_tmux_checked([
                "new-window",
                "-S",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &workspace.name,
                "-c",
                &dir.to_string_lossy(),
            ]) {
                bail!(
                    "couldn't open a shell window for workspace `{}`: {stderr}",
                    workspace.name
                );
            }
        }
        Host::Ssh { host: ssh_host } => {
            let addr = orch_workspace::workspace_tmux_addr(project, workspace)
                .map_err(|e| anyhow!(e))?;
            if !shelbi_tmux::has_session(host, &addr.session).map_err(|e| anyhow!(e))? {
                // No pane command → the remote session's default shell, so
                // the user's login rc files run — same reason the remote
                // worker launch creates an empty session first.
                shelbi_tmux::new_session(host, &addr.session, &addr.window, None)
                    .map_err(|e| anyhow!(e))?;
                shelbi_tmux::send_line(host, &addr, &shell_cd_line(&worktree, &machine.work_dir))
                    .map_err(|e| anyhow!(e))?;
            }
            // Same proxy-window mechanism as the remote worker arm — the
            // local window ssh-attaches to the workspace's own session.
            let cmd = format!(
                "ssh -t {host} tmux attach -t {remote_session}",
                host = shelbi_agent::shell_escape(ssh_host),
                remote_session = shelbi_agent::shell_escape(&addr.session),
            );
            if let Err(stderr) = run_local_tmux_checked([
                "new-window",
                "-S",
                "-t",
                &format!("{project_session}:"),
                "-n",
                &workspace.name,
                "sh",
                "-c",
                &cmd,
            ]) {
                bail!(
                    "couldn't open a shell for remote workspace `{}` on `{ssh_host}`: {stderr}",
                    workspace.name
                );
            }
        }
    }

    // Stamp the slot as a user shell so dispatch skips it while open.
    // Best-effort: on failure the slot degrades to reading as an orphaned
    // session (dispatch may then reset it), which is the pre-feature
    // behavior — warn rather than fail the open.
    let addr = orch_workspace::workspace_tmux_addr(project, workspace).map_err(|e| anyhow!(e))?;
    if let Err(e) = orch_workspace::mark_user_shell(host, &addr) {
        eprintln!(
            "shelbi: warning: couldn't mark workspace `{}` as user-occupied: {e}",
            workspace.name
        );
    }

    let _ = run_local_tmux(["select-window", "-t", &target]);
    Ok(())
}

/// Where a local idle-workspace shell starts: the worktree when it exists,
/// the machine's `work_dir` otherwise (the worktree is detached/pruned
/// after handoff, and a never-dispatched workspace has none at all).
fn shell_window_dir(worktree: PathBuf, work_dir: &Path) -> PathBuf {
    if worktree.is_dir() {
        worktree
    } else {
        work_dir.to_path_buf()
    }
}

/// The `cd` line sent into a remote idle-workspace shell: land in the
/// worktree, falling back to the machine's `work_dir` when the worktree
/// doesn't exist on the remote host (we can't cheaply stat it from here,
/// so the fallback runs in the remote shell itself).
fn shell_cd_line(worktree: &Path, work_dir: &Path) -> String {
    format!(
        "cd {wt} 2>/dev/null || cd {wd}",
        wt = shelbi_agent::shell_escape(&worktree.to_string_lossy()),
        wd = shelbi_agent::shell_escape(&work_dir.to_string_lossy()),
    )
}

/// Silent, best-effort tmux call for *probes* (`select-window` on a window
/// that may not exist yet) — a non-zero exit is the normal "not there,
/// create it" signal, so its stderr is intentionally nulled to keep the
/// terminal clean on the common create path.
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

/// Run a tmux command whose failure is a real error (not a probe),
/// returning `Ok(())` on success or `Err(<stderr>)` so the caller can fold
/// tmux's own reason into its message instead of collapsing to an opaque
/// `false` (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/cli-session-ux.md F12). `<stderr>` falls back to the exit status
/// (or the spawn error) when tmux printed nothing.
fn run_local_tmux_checked<I, S>(args: I) -> std::result::Result<(), String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    match std::process::Command::new("tmux").args(args).output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stderr = stderr.trim();
            if stderr.is_empty() {
                Err(format!("tmux exited {}", out.status))
            } else {
                Err(stderr.to_string())
            }
        }
        Err(e) => Err(format!("failed to run tmux: {e}")),
    }
}

fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()?.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The idle shell starts in the worktree when it exists on disk…
    #[test]
    fn shell_window_dir_prefers_existing_worktree() {
        let tmp = std::env::temp_dir().join(format!(
            "shelbi-open-shell-dir-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        let work_dir = Path::new("/tmp/project");
        assert_eq!(shell_window_dir(tmp.clone(), work_dir), tmp);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// …and falls back to the machine work_dir when the worktree has been
    /// detached/pruned (post-handoff) or never created.
    #[test]
    fn shell_window_dir_falls_back_to_work_dir_when_worktree_missing() {
        let missing = PathBuf::from("/definitely/not/a/real/worktree/path");
        let work_dir = Path::new("/tmp/project");
        assert_eq!(shell_window_dir(missing, work_dir), work_dir.to_path_buf());
    }

    /// The remote cd line carries the same fallback, evaluated in the
    /// remote shell (we can't stat the remote worktree from here), with
    /// both paths shell-escaped.
    #[test]
    fn shell_cd_line_falls_back_to_work_dir_remotely() {
        let line = shell_cd_line(
            Path::new("/work/my app/.shelbi/wt/delta"),
            Path::new("/work/my app"),
        );
        assert_eq!(
            line,
            "cd '/work/my app/.shelbi/wt/delta' 2>/dev/null || cd '/work/my app'"
        );
    }

    /// Simple paths ride unquoted (shell_escape's conservative-quoting
    /// path) so the line stays readable in pane captures.
    #[test]
    fn shell_cd_line_simple_paths_skip_quoting() {
        let line = shell_cd_line(
            Path::new("/work/app/.shelbi/wt/delta"),
            Path::new("/work/app"),
        );
        assert_eq!(line, "cd /work/app/.shelbi/wt/delta 2>/dev/null || cd /work/app");
    }
}
