//! `shelbi __review-serve -- <cmd...>` — the thin launch wrapper the Review
//! agent runs in place of the raw serve command from a workflow `review:`
//! recipe (the orchestrator renders the serve line wrapped in this; see
//! `render_review_recipe_section`).
//!
//! Why it exists: the review dev server is started by the *agent* inside its
//! tmux pane and promptly detaches itself into the background, so when the pane
//! is torn down the server's whole process tree (`next-server`, `esbuild`,
//! `contentlayer2`, …) is orphaned to launchd, keeps running, and holds the
//! review port — leftovers pile up across loads. Shelbi never spawned it, so it
//! had nothing to kill.
//!
//! This wrapper fixes that by owning the launch topology without owning the
//! command (the `review:` recipe stays project-customizable):
//!
//! - It starts the serve command in its **own session** via `setsid`, so the
//!   new session's leader is the serve process and its process-group id equals
//!   that pid. Every grandchild the server forks inherits that group.
//! - It records the pgid to `$SHELBI_REVIEW_PGID_FILE` (hub-side, set by the
//!   orchestrator) so any teardown path can `kill(-pgid, …)` the whole tree
//!   later — even after a hub/daemon restart, since the record is on disk.
//! - On a clean child exit it removes the record. If the wrapper is itself
//!   killed first (the pane's SIGHUP), the record is deliberately left in place:
//!   that is exactly the orphaned-server case teardown must still reap.

/// Env var naming the file this wrapper writes the launched server's
/// process-group id into. The orchestrator injects it into the review pane
/// (see `local_pane_tmux_argv`); its value is
/// `shelbi_state::review_serve_pgid_path(workspace)`. Absent on a manual
/// invocation, in which case the server still runs but isn't orchestrator
/// -tracked. Spelled once in `shelbi_state` so producer and consumer can't
/// drift.
use shelbi_state::REVIEW_SERVE_PGID_FILE_ENV as PGID_FILE_ENV;

/// Run `cmd` (the serve command and its args, everything after `--`) in its own
/// session, record its pgid, wait for it, and propagate its exit status.
pub fn run(cmd: Vec<String>) -> anyhow::Result<()> {
    let mut parts = cmd.into_iter();
    let program = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("shelbi __review-serve: no command given after `--`"))?;
    let args: Vec<String> = parts.collect();

    let mut command = std::process::Command::new(&program);
    command.args(&args);

    // Start the server as a session leader: a new session implies a new process
    // group whose id is the child's pid, so a single `kill(-pgid)` from any
    // teardown path reaps the server and every grandchild at once. setsid also
    // drops the controlling tty; inherited stdio (the pane) is kept, so the
    // agent still sees the server's output while it polls the ready probe.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Safety: the closure runs in the forked child before exec and only
        // calls async-signal-safe `setsid`/`errno` — no allocation, no shared
        // state. Returning the error aborts the exec (surfaced as spawn error).
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    let mut child = command
        .spawn()
        .map_err(|e| anyhow::anyhow!("shelbi __review-serve: failed to start `{program}`: {e}"))?;

    // setsid made the child a session + group leader, so pgid == pid.
    record_pgid(child.id() as i32);

    let status = child.wait()?;
    // Clean exit: drop the record so a later teardown doesn't chase a dead
    // (possibly recycled) pgid.
    clear_pgid_record();
    std::process::exit(status.code().unwrap_or(1));
}

/// Persist the launched server's pgid to `$SHELBI_REVIEW_PGID_FILE`.
/// Best-effort: a write failure just means this server won't be auto-reaped on
/// teardown (the pre-fix behavior), so it must never abort the launch.
fn record_pgid(pgid: i32) {
    let Some(path) = std::env::var_os(PGID_FILE_ENV) else {
        return;
    };
    let path = std::path::PathBuf::from(path);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(&path, format!("{pgid}\n"));
}

/// Remove the pgid record on a clean exit. Best-effort and idempotent.
fn clear_pgid_record() {
    if let Some(path) = std::env::var_os(PGID_FILE_ENV) {
        let _ = std::fs::remove_file(path);
    }
}
