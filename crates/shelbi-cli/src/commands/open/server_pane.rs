//! `shelbi open <name> --as-server-pane` — the lifecycle wrapper that owns a
//! review workspace's long-lived **dev server** process.
//!
//! It's the server-side twin of [`super::pane`]: where that wrapper owns the
//! interactive agent subprocess and emits `pane_alive=false` on exit, this one
//! owns the `$SHELBI_SERVE_CMD` server subprocess on `$PORT` and emits
//! `project=<name> workspace=<name> server_alive=false reason=<short>` when
//! the server dies (the `project=` scope keeps a same-named review workspace
//! in another project from being mistaken for this one on the hub-global log).
//! The distinct verb lets the orchestrator tell "the agent stopped" from "the
//! served URL went down / the port freed."
//!
//! The port and serve command arrive as environment variables (`PORT`,
//! `SHELBI_SERVE_CMD`) injected by
//! [`shelbi_orchestrator::server_pane::spawn_server_pane`] on the `tmux
//! split-window -e` that created this pane — the same mechanism the agent
//! pane's dispatch uses for `TASK_ID` / `PROJECT`.
//!
//! Teardown suppression mirrors the agent pane exactly, but keyed on the
//! server-scoped marker ([`shelbi_state::server_teardown_key`]) so an
//! intentional kill of the server pane (reap, re-dispatch, `workspace stop`)
//! doesn't fire a spurious `server_alive=false` while a real crash still does.

use std::io::{self, BufRead, IsTerminal, Write};
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, bail, Result};
use shelbi_core::{Machine, Project, WorkspaceSpec};
use shelbi_orchestrator::server_pane::{ENV_PORT, ENV_SERVE_CMD};
use shelbi_orchestrator::workspace as orch_workspace;

use super::pane;

/// Run the server-pane wrapper: launch the dev server, wait, emit the
/// lifecycle event, optionally hold the pane open for a final keypress.
pub fn run(project: &Project, workspace: &WorkspaceSpec, machine: &Machine) -> Result<()> {
    let port = std::env::var(ENV_PORT).unwrap_or_default();
    let serve = std::env::var(ENV_SERVE_CMD).unwrap_or_default();
    if serve.trim().is_empty() {
        // Nothing to serve — the spawn path always sets SHELBI_SERVE_CMD, so
        // an empty value means this wrapper was invoked by hand or the env
        // didn't propagate. Fail loudly rather than sit on a dead pane.
        bail!(
            "no serve command: `${ENV_SERVE_CMD}` is unset or empty \
             (this wrapper is spawned by shelbi's review dispatch, not run directly)"
        );
    }

    let worktree = orch_workspace::workspace_worktree(machine, workspace);
    let worktree_str = worktree.to_string_lossy().into_owned();

    // Clear any stale server-teardown marker up front (mirrors the agent
    // pane): a mark-then-SIGKILL race could otherwise leave a marker that
    // suppresses this pane's real, natural exit later.
    let teardown_key = shelbi_state::server_teardown_key(&workspace.name);
    let _ = shelbi_state::clear_expected_teardown(&teardown_key);

    // `cd` falls back to $HOME if the worktree is missing so the pane doesn't
    // instantly close on a misconfiguration; the serve command references
    // `$PORT`, which we also pin explicitly on the child's env below.
    let shell_cmd = format!(
        "cd {wd} 2>/dev/null || cd; LANG=C.UTF-8 {serve}",
        wd = shelbi_agent::shell_escape(&worktree_str),
    );

    // Install the signal listener BEFORE spawning the child (F11): a
    // SIGHUP/SIGTERM/SIGINT arriving in the spawn window would otherwise
    // kill this wrapper outright and drop the lifecycle event. The child
    // PID isn't known until spawn returns, so the listener reads it from a
    // shared cell we populate the instant `spawn()` succeeds.
    let received_signal: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    let signaled_flag = Arc::new(AtomicBool::new(false));
    let child_pid_cell = Arc::new(AtomicI32::new(0));

    let signal_handle = pane::install_signal_listener(
        Arc::clone(&received_signal),
        Arc::clone(&signaled_flag),
        Arc::clone(&child_pid_cell),
    )?;

    let hub_sock = shelbi_state::hub_socket_path()
        .map_err(|e| anyhow!("resolving hub socket path: {e}"))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&shell_cmd)
        .env(ENV_PORT, &port)
        .env("PROJECT", &project.name)
        .env("SHELBI_HUB_SOCK", &hub_sock)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow!("spawning dev server (`sh -c {shell_cmd}`): {e}"))?;
    // Publish the child PID, then forward any signal caught in the narrow
    // pre-publish window so the server still tears down.
    child_pid_cell.store(child.id() as i32, Ordering::SeqCst);
    if let Some(sig) = *received_signal.lock().unwrap() {
        // SAFETY: kill takes a raw pid; no memory is dereferenced.
        unsafe {
            libc::kill(child.id() as libc::pid_t, sig);
        }
    }

    let status = child
        .wait()
        .map_err(|e| anyhow!("waiting on dev-server subprocess: {e}"))?;
    signal_handle.close();

    let signaled = *received_signal.lock().unwrap();
    let reason = pane::exit_reason(&status, signaled);

    // Suppress the death event when a shelbi-initiated caller (reap,
    // re-dispatch, `workspace stop`) marked the server teardown before
    // killing this pane. A manual `tmux kill-pane` from the user still fires
    // it — nobody marked it as expected.
    let intentional_teardown =
        shelbi_state::consume_expected_teardown(&teardown_key).unwrap_or(false);
    if !intentional_teardown {
        if let Err(e) = shelbi_state::append_workspace_server_event(
            &project.name,
            &workspace.name,
            false,
            &reason,
        ) {
            eprintln!(
                "shelbi: warning: couldn't write server pane-death event for `{}`: {e}",
                workspace.name
            );
        }
    }

    // Natural exit: give the human a chance to read the server's final output
    // (a crash backtrace, a port-in-use error) before the pane vanishes. Skip
    // on a forced teardown (pane's closing anyway) and when stdin isn't a TTY
    // (under tests / a non-controlling-terminal wrapper).
    let interactive = signaled.is_none() && io::stdin().is_terminal();
    if interactive {
        let _ = writeln!(io::stdout());
        let _ = writeln!(io::stdout(), "[dev server exited — press enter to close]");
        let _ = io::stdout().flush();
        let mut line = String::new();
        let _ = io::stdin().lock().read_line(&mut line);
    }

    Ok(())
}
