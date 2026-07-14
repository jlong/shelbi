//! `shelbi open <name> --as-pane` — the lifecycle wrapper that
//! becomes a workspace pane's top-level process. It owns the agent
//! subprocess, installs signal handlers so tmux teardown / kill-window /
//! Ctrl-C all flow through the same exit path, and writes a
//! `project=<name> workspace=<name> pane_alive=false reason=<short>` line to
//! `~/.shelbi/events.log` on every termination so the orchestrator's
//! reaction rules can fire on pane death. The leading `project=` scope keeps
//! a same-named workspace in another project (the log is hub-global) from
//! being read as *this* pane dying.
//!
//! Single-process model: this binary IS the pane. When it exits, the
//! pane closes. The default exit behavior prints
//! `[agent exited — press enter to close]` and waits on stdin so final
//! output doesn't vanish — that matches the "user wanted to look at this
//! workspace" mental model. Signal-driven exits skip the prompt because
//! the pane is being torn down externally and the user can't reach the
//! prompt anyway.

use std::io::{self, BufRead, IsTerminal, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, AtomicI32, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, Result};
use shelbi_core::{Machine, Project, StatusCategory, Task, WorkspaceSpec};
use shelbi_orchestrator::workspace as orch_workspace;

/// Env var the agent inherits naming the project the worker belongs to.
/// Read by the Phase 7 SessionStart / Stop hooks that emit `message-ack`
/// JSON over the hub socket — the ack carries the project so the daemon
/// can route to the right `events.log`.
const ENV_PROJECT: &str = "PROJECT";

/// Env var the agent inherits naming the task currently assigned to
/// this workspace, used by the Phase 7 hooks to scope per-task message
/// paths (`.shelbi/messages/$TASK_ID.log`). Empty when no task is
/// in-flight — the hooks `[ -n "$TASK_ID" ] || exit 0`-guard so a
/// "bare" pane (sidebar-clicked, no task running) is a clean no-op.
const ENV_TASK_ID: &str = "TASK_ID";

/// Env var pointing at the hub socket the worker writes JSON-line
/// messages to (events, clarification requests, message-acks). Phase 3
/// of the worker↔hub design pins this to `~/.shelbi/hub.sock` on the
/// hub, `/tmp/shelbi-hub-<uid>.sock` on remote panes; here we default to
/// the hub path but honor an existing value so tests / remote panes can
/// override.
const ENV_HUB_SOCK: &str = "SHELBI_HUB_SOCK";

/// Build the `sh -c …`-suitable command string that re-enters this
/// binary under `--as-pane`. Exposed so `focus_or_create` and
/// `start_workspace_on_task` use the exact same string and can't drift.
/// Delegates to [`shelbi_orchestrator::workspace_pane_cmd`] (the shared
/// builder the targeted `shelbi reload workspace` path also uses) with
/// `resume: false` — a fresh focus/create never resumes.
pub fn wrapper_invocation(shelbi_bin: &str, project: &str, workspace: &str) -> String {
    shelbi_orchestrator::workspace_pane_cmd(shelbi_bin, project, workspace, false)
}

/// Run the wrapper: spawn the agent, wait, emit the lifecycle event,
/// optionally hold the pane open for a final keypress.
///
/// `resume` is set when this pane was launched by `shelbi task resume`: for a
/// claude runner it adds `--continue` to the launch command so the pane reloads
/// its prior conversation instead of starting cold. It's a no-op for a normal
/// dispatch, a sidebar click, or a non-claude runner.
pub fn run(
    project: &Project,
    workspace: &WorkspaceSpec,
    machine: &Machine,
    resume: bool,
) -> Result<()> {
    let runner = project
        .runner(&workspace.runner)
        .ok_or_else(|| {
            anyhow!(
                "workspace `{}` references unknown runner `{}`",
                workspace.name,
                workspace.runner,
            )
        })?
        .clone();
    let worktree = orch_workspace::workspace_worktree(machine, workspace);
    let worktree_str = worktree.to_string_lossy().into_owned();

    // Resolve the task assigned to this workspace up front — used both to seed
    // `$TASK_ID` for the Phase 7 hooks (below) and to recover a missing
    // worktree (next). Covers in-progress AND in-review assignments: a review
    // workspace picking up a task that's already in Review is exactly the case
    // the missing-worktree recovery exists for. An inherited `TASK_ID` still
    // wins for the id itself (the dispatch path spawns this wrapper before it
    // writes the task's state, so a state lookup would race the save).
    let assigned_task = assigned_task_for_workspace(&project.name, &workspace.name);

    // An inherited non-empty `TASK_ID` means the dispatch path launched this
    // pane (deploy_and_spawn injects it via tmux `-e`) — a sidebar click
    // inherits nothing. Dispatch-originated launches hold a hard contract on
    // the worktree (bug-worker-commit-landed-on-hub-main-checkout): the agent
    // must start in it or not at all, never in a `$HOME` fallback where its
    // commits could land somewhere surprising.
    let inherited_task_id = std::env::var(ENV_TASK_ID)
        .ok()
        .filter(|s| !s.is_empty());
    let dispatched = inherited_task_id.is_some();

    // Missing-worktree recovery (bug-review-workspace-open-creates-missing-worktree):
    // when a task is assigned to this workspace but its worktree hasn't been
    // created yet, create + check out the worktree on the task's branch BEFORE
    // the agent launches so it starts in the worktree, not the `cd`-with-no-arg
    // home fallback. A bare (sidebar-click) launch only acts when the worktree
    // is entirely absent — an existing worktree (even mid-task, dirty, or on
    // another branch) is left untouched — and stays best-effort: a git failure
    // degrades to the `cd … || cd` fallback below rather than killing the
    // pane. A dispatched launch additionally heals a present-but-broken dir
    // (created but never got its `.git`) and hard-fails after this block if
    // no valid worktree came out of it.
    let worktree_valid = |wt: &Path| wt.join(".git").is_dir() || wt.join(".git").is_file();
    if let Some(task) = &assigned_task {
        if !worktree.exists() || (dispatched && !worktree_valid(&worktree)) {
            if let Some(branch) = resolve_task_branch(project, task) {
                if let Err(e) = orch_workspace::ensure_workspace_worktree(
                    &project.name,
                    machine,
                    workspace,
                    &branch,
                    project.base_branch(),
                ) {
                    eprintln!(
                        "shelbi: warning: couldn't create worktree for task `{}` assigned to \
                         workspace `{}`: {e}",
                        task.id, workspace.name,
                    );
                }
            }
        }
    }

    // Dispatch-originated launches must have a usable worktree by now —
    // whatever sync_worktree stood up, plus the recovery above. Refuse to
    // launch the agent anywhere else. The events.log line makes the stall
    // visible to the orchestrator and `shelbi events tail`; the dispatch
    // caller also notices via its readiness timeout.
    if dispatched && !worktree_valid(&worktree) {
        if let Err(e) = shelbi_state::append_workspace_pane_event(
            &project.name,
            &workspace.name,
            false,
            "worktree-missing",
        ) {
            eprintln!("shelbi: warning: couldn't write worktree-missing event: {e}");
        }
        return Err(anyhow!(
            "workspace `{}` was dispatched a task but its worktree at {} is \
             missing or broken — refusing to launch the agent outside it \
             (re-run `shelbi task start` after fixing the worktree)",
            workspace.name,
            worktree.display(),
        ));
    }

    // A crashed prior wrapper (mark_expected_teardown → tmux kill-window →
    // wrapper SIGKILLed before it could consume) can leave a stale
    // `.expected-teardown` marker for this workspace. Clear it up front so
    // it can't silently suppress the pane_alive event on our real, natural
    // exit later. `consume_expected_teardown` also enforces a max-age
    // freshness window as a belt-and-suspenders check, but clearing here
    // means the wrapper's lifetime is a hard boundary on the marker's
    // scope.
    let _ = shelbi_state::clear_expected_teardown(&workspace.name);

    // Conditional --append-system-prompt: only when the agent context has
    // been deployed (which task start does) and we're launching claude.
    // Bare `shelbi open` from sidebar click on a workspace that's
    // never run a task yet won't have the file — no flag in that case.
    let has_agent_instructions = worktree
        .join(orch_workspace::WORKTREE_AGENT_INSTRUCTIONS_REL)
        .exists();
    let startup_prompt_rel = worktree
        .join(orch_workspace::WORKTREE_STARTUP_PROMPT_REL)
        .exists()
        .then_some(orch_workspace::WORKTREE_STARTUP_PROMPT_REL);
    // Build the launch command through the shared constructor so this local
    // wrapper path and the remote dispatch path (`deploy_and_spawn`) apply the
    // same runner / permission-mode / startup-prompt logic and can't drift.
    let launch_full = orch_workspace::workspace_launch_command_with_startup_prompt(
        &runner,
        &project.workspace_permissions_mode,
        has_agent_instructions,
        resume,
        startup_prompt_rel,
    );

    // Bare launch: `cd` falls back to $HOME if the worktree doesn't exist —
    // that keeps a sidebar click on a never-used workspace from leaving the
    // user in an empty pane that immediately closes. Dispatched launch: no
    // fallback — the worktree was verified above, and if it vanishes in the
    // spawn window the agent must not start somewhere else, so the pane
    // exits instead (the wrapper's exit path still emits the pane event).
    let shell_cmd = if dispatched {
        format!(
            "cd {wd} || {{ echo 'shelbi: worktree {wd} disappeared before launch; aborting' >&2; exit 1; }}; LANG=C.UTF-8 {launch_full}",
            wd = shelbi_agent::shell_escape(&worktree_str),
        )
    } else {
        format!(
            "cd {wd} 2>/dev/null || cd; LANG=C.UTF-8 {launch_full}",
            wd = shelbi_agent::shell_escape(&worktree_str),
        )
    };

    // Seed `$TASK_ID` so the Phase 7 hooks (SessionStart tail + Stop
    // message-inject) have a concrete id to anchor their per-task paths on.
    // Prefer an inherited `TASK_ID` env var when the caller injected one via
    // tmux `-e`: the dispatch path spawns this wrapper BEFORE writing the
    // task's `assigned_to`/`column=in_progress` to disk, so the `assigned_task`
    // lookup above would race the state save and return None. Fall back to that
    // lookup for the sidebar-click path (no env → empty TASK_ID when nothing is
    // assigned, and the hooks no-op cleanly).
    let task_id = inherited_task_id
        .clone()
        .or_else(|| assigned_task.as_ref().map(|t| t.id.clone()))
        .unwrap_or_default();

    // Signal handling: arrange for SIGHUP / SIGTERM / SIGINT to be
    // captured in a background thread that records which one fired and
    // proactively forwards it to the child. The wait() below returns
    // either way (Unix kernels propagate process-group signals to
    // children too); recording the signal lets us label the events.log
    // reason and skip the "press enter" prompt on a forced teardown.
    //
    // The listener is installed BEFORE the child is spawned (Shelbi
    // ContextStore docs/planning:reviews/adversarial-2026-07/cli-session-ux.md
    // F11): a
    // signal arriving in the spawn window would otherwise hit the
    // wrapper's default disposition and kill it outright, orphaning a
    // half-started pane and dropping the lifecycle event. The child's
    // PID isn't known until spawn returns, so the listener reads it from
    // a shared cell that we populate the instant `spawn()` succeeds; a
    // signal caught before then is recorded and forwarded once the PID
    // lands.
    let received_signal: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    let signaled_flag = Arc::new(AtomicBool::new(false));
    let child_pid_cell = Arc::new(AtomicI32::new(0));

    let signal_handle = install_signal_listener(
        Arc::clone(&received_signal),
        Arc::clone(&signaled_flag),
        Arc::clone(&child_pid_cell),
    )?;

    // Phase 3: pin `SHELBI_HUB_SOCK` so the agent's `nc -U` / socat /
    // python socket-write one-liners resolve to the same path the daemon
    // is listening on. `hub_socket_path()` already honors the env var if
    // set (e.g. remote panes whose value points at the SSH reverse-forward
    // landing path), so the agent inherits the same resolution.
    //
    // Phase 7: also export `$PROJECT` and `$TASK_ID` so the Claude Code
    // SessionStart + Stop hooks can write to and tail the per-task
    // message log at `.shelbi/messages/$TASK_ID.log`.
    let hub_sock =
        shelbi_state::hub_socket_path().map_err(|e| anyhow!("resolving hub socket path: {e}"))?;
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&shell_cmd)
        .env(ENV_PROJECT, &project.name)
        .env(ENV_TASK_ID, &task_id)
        .env(ENV_HUB_SOCK, &hub_sock)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| anyhow!("spawning agent (`sh -c {shell_cmd}`): {e}"))?;
    // Publish the child PID so the already-running listener can forward
    // signals to it. Then handle the narrow window where a signal was
    // caught before the PID was published: forward it now so the child
    // still tears down instead of being left running.
    child_pid_cell.store(child.id() as i32, Ordering::SeqCst);
    if let Some(sig) = *received_signal.lock().unwrap() {
        // SAFETY: kill takes a raw pid; no memory is dereferenced. A
        // benign ESRCH here just means the child already exited.
        unsafe {
            libc::kill(child.id() as libc::pid_t, sig);
        }
    }

    let status = child
        .wait()
        .map_err(|e| anyhow!("waiting on agent subprocess: {e}"))?;

    // Tear down the signal listener thread so a stray late signal
    // doesn't leak into an unrelated shell.
    signal_handle.close();

    // Phase 7: when the worker pane is destroyed, kill any tail process
    // whose PID file is recorded in the worktree. Otherwise a tail spun
    // up by the SessionStart hook outlives the pane and accumulates
    // across worker restarts. Best-effort — failure to clean up is logged
    // but doesn't block the pane from closing.
    if !task_id.is_empty() {
        if let Err(e) = kill_task_tail(&worktree, &task_id) {
            eprintln!("shelbi: warning: couldn't clean up message-tail for task `{task_id}`: {e}",);
        }
    }

    let signaled = *received_signal.lock().unwrap();
    let reason = exit_reason(&status, signaled);

    // Suppress the `pane_alive=false` event when a shelbi-initiated caller
    // (dispatch, quit-project, quit-shelbi, `shelbi workspace stop`)
    // dropped the expected-teardown marker before killing our tmux window.
    // Otherwise every dispatch would emit
    // `project=<name> workspace=<name> pane_alive=false reason=signal:SIGHUP` right
    // before the replacement pane comes up — the orchestrator's reaction
    // rule ("don't auto-restart, flag it") assumes the event means real
    // pane death, so a spurious signal on every dispatch is genuinely
    // harmful. A manual `tmux kill-pane` from the user still fires the
    // event: nobody marked it as expected. See
    // bug-workspace-pane-alive-false-sighup-fires-spuriously-right-after-dispatch.
    let intentional_teardown =
        shelbi_state::consume_expected_teardown(&workspace.name).unwrap_or(false);
    if !intentional_teardown {
        // Best-effort: a failure here shouldn't keep the pane from closing.
        if let Err(e) = shelbi_state::append_workspace_pane_event(
            &project.name,
            &workspace.name,
            false,
            &reason,
        ) {
            eprintln!(
                "shelbi: warning: couldn't write workspace pane-death event for `{}`: {e}",
                workspace.name
            );
        }
    }

    // Tell the sidebar supervisor whether this death should be auto-restarted.
    // It can't see the exit reason (it only polls tmux liveness), so we hand
    // it a dedicated no-restart marker on the two deaths it must NOT relaunch:
    // a shelbi-initiated teardown (dispatch / `workspace stop` / project close),
    // and a clean `exit:0` (the user quit with Ctrl-D, or the agent finished
    // and exited on its own). A crash — a signal, or a non-zero exit — leaves
    // no marker, so the supervisor relaunches the pane and re-dispatches the
    // task. Separate key from the wrapper's own expected-teardown marker above
    // so the two consumers don't race over one file. Best-effort: on failure
    // the supervisor sees no marker and at worst relaunches a pane that exited
    // cleanly, which the next expected-teardown on its own teardown corrects.
    if intentional_teardown || reason == "exit:0" {
        if let Err(e) = shelbi_state::mark_expected_teardown(
            &shelbi_state::supervision_shutdown_key(&workspace.name),
        ) {
            eprintln!(
                "shelbi: warning: couldn't write supervision no-restart marker for `{}`: {e}",
                workspace.name
            );
        }
    }

    // Natural exit (clean code or agent-side signal that wasn't routed
    // to us): the user is here, give them a chance to read final output
    // before the pane vanishes. Skip the prompt on a forced teardown
    // (we caught a signal — the pane is closing anyway) and skip it
    // when stdin isn't a TTY (running under `cargo test`, an `sh -c`
    // wrapper without a controlling terminal, etc.) so the test
    // process doesn't hang waiting for input nobody can supply.
    let interactive = signaled.is_none() && io::stdin().is_terminal();
    if interactive {
        let _ = writeln!(io::stdout());
        let _ = writeln!(io::stdout(), "[agent exited — press enter to close]");
        let _ = io::stdout().flush();
        let mut line = String::new();
        let _ = io::stdin().lock().read_line(&mut line);
    }

    Ok(())
}

/// Spawn a background thread that blocks on `signal-hook`'s iterator,
/// records the first SIGHUP/SIGTERM/SIGINT it sees into `received_signal`,
/// and forwards the same signal to the child's PID (some tmux configs
/// only signal the wrapper, not the whole process group). Returns the
/// signal handle so the caller can close the listener once `wait()`
/// returns.
///
/// `child_pid_cell` is read at signal time rather than captured by value
/// so this can be installed BEFORE the child is spawned (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/cli-session-ux.md F11): the
/// caller stores the real PID into the cell the moment `spawn()`
/// succeeds. A signal that arrives while the cell is still `0` (the
/// pre-spawn window) is recorded but not forwarded here — the caller
/// forwards it once it publishes the PID.
pub(super) fn install_signal_listener(
    received_signal: Arc<Mutex<Option<i32>>>,
    signaled_flag: Arc<AtomicBool>,
    child_pid_cell: Arc<AtomicI32>,
) -> Result<signal_hook::iterator::Handle> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;

    let mut signals = Signals::new([SIGHUP, SIGINT, SIGTERM])
        .map_err(|e| anyhow!("installing signal handlers: {e}"))?;
    let handle = signals.handle();
    std::thread::spawn(move || {
        for sig in signals.forever() {
            if signaled_flag.swap(true, Ordering::SeqCst) {
                // Already recorded the first signal — ignore subsequent
                // ones. The wait() in the main thread will still return
                // once the child reaps.
                continue;
            }
            *received_signal.lock().unwrap() = Some(sig);
            // Forward the signal to the child. A `0` cell means the child
            // isn't spawned yet (pre-spawn signal window) — the caller
            // handles that case after publishing the PID, so skip here.
            // Errors from a real PID are benign: the child may have
            // already exited (ESRCH) or the process group may have
            // delivered the signal already.
            let child_pid = child_pid_cell.load(Ordering::SeqCst);
            if child_pid > 0 {
                unsafe {
                    libc::kill(child_pid as libc::pid_t, sig);
                }
            }
        }
    });
    Ok(handle)
}

/// Compose a short reason token for the events.log line. Signal-driven
/// exits get `signal:<name>`; natural exits get `exit:<code>`; the
/// no-info path collapses to `exit:unknown`.
pub(super) fn exit_reason(status: &std::process::ExitStatus, received: Option<i32>) -> String {
    if let Some(sig) = received {
        return format!("signal:{}", signal_name(sig));
    }
    // On Unix the child can also have died from a signal that wasn't
    // forwarded through us (e.g. SIGSEGV inside claude). Surface that
    // explicitly rather than collapsing it to a bare exit code.
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal:{}", signal_name(sig));
        }
    }
    match status.code() {
        Some(code) => format!("exit:{code}"),
        None => "exit:unknown".to_string(),
    }
}

/// Map the small set of signals we care about to short readable tokens.
/// Unknown signals fall back to their numeric value so the reason string
/// stays parseable.
fn signal_name(sig: i32) -> String {
    match sig {
        signal_hook::consts::SIGHUP => "SIGHUP".into(),
        signal_hook::consts::SIGINT => "SIGINT".into(),
        signal_hook::consts::SIGTERM => "SIGTERM".into(),
        signal_hook::consts::SIGKILL => "SIGKILL".into(),
        other => format!("{other}"),
    }
}

/// Look up the task currently assigned to `workspace`, if any. Used by the
/// pane wrapper to seed `$TASK_ID` for the Phase 7 hooks AND to recover a
/// missing worktree before launch — and by the parent module's
/// `focus_or_create` to decide whether a sidebar click relaunches the
/// worker (task assigned) or opens a plain user shell (idle).
/// Considers both the active (`in-progress`)
/// and handoff (`review`) categories: a review workspace's task sits in
/// Review, so an in-progress-only lookup would miss it and leave the
/// worktree uncreated — the very bug this recovery closes.
///
/// The poller's invariant guarantees at most one such task per workspace (it
/// dispatches sequentially), so `find` is correct — if the invariant ever
/// breaks we want the first match, not a silent collapse to None.
///
/// Best-effort: returns `None` on read errors (missing project state,
/// permissions glitch, transient FS) because a missing task just makes the
/// hooks no-op and skips the worktree recovery — neither breaks the pane.
pub(super) fn assigned_task_for_workspace(project: &str, workspace: &str) -> Option<Task> {
    let tasks = shelbi_state::list_tasks(project).ok()?;
    tasks.into_iter().find_map(|tf| {
        let anchors = matches!(
            tf.task.column.category(),
            StatusCategory::Active | StatusCategory::Handoff
        );
        if anchors && tf.task.assigned_to.as_deref() == Some(workspace) {
            Some(tf.task)
        } else {
            None
        }
    })
}

/// Resolve the branch a task's worktree must be checked out on, mirroring the
/// `shelbi task start` dispatch path: an explicit `task.branch` wins;
/// otherwise it's composed from the task's workflow + project config via
/// [`shelbi_orchestrator::branch::branch_name_for_task`]. An assigned
/// (in-progress or in-review) task almost always has `task.branch` already
/// written back, so the compose path is a rare fallback.
///
/// Best-effort: returns `None` (worktree recovery is skipped, and the pane
/// falls back to `$HOME`) when the branch can't be resolved — losing the
/// recovery is strictly better than aborting the pane.
fn resolve_task_branch(project: &Project, task: &Task) -> Option<String> {
    if let Some(branch) = &task.branch {
        return Some(branch.clone());
    }
    let workflow = shelbi_state::load_task_workflow(&project.name, project, task)
        .unwrap_or_else(|_| shelbi_core::default_workflow());
    shelbi_orchestrator::branch::branch_name_for_task(project, Some(&workflow), task).ok()
}

/// Reasoned cleanup of the SessionStart tail on pane exit. Reads the
/// pid file the hook recorded, sends SIGTERM to it, and removes the
/// lock directory. Idempotent: missing pid file or missing dir is a
/// no-op (the hook may never have run, or another pass already cleaned
/// up). Failure to kill (process already gone, EPERM) is benign.
fn kill_task_tail(worktree: &Path, task_id: &str) -> std::io::Result<()> {
    let lock_dir = worktree
        .join(".shelbi")
        .join("messages")
        .join(format!("{task_id}.tail.d"));
    let pid_file = lock_dir.join("pid");
    let pid_text = match std::fs::read_to_string(&pid_file) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    if let Ok(pid) = pid_text.trim().parse::<libc::pid_t>() {
        // Guard against PID reuse (F11): between the SessionStart hook
        // recording `$!` and this cleanup running, the tail may have died
        // and its PID been recycled to an unrelated process. Signaling
        // that innocent process is the bug. Same principle as
        // Shelbi ContextStore docs/planning:reviews/adversarial-2026-07/state-runtime.md
        // F5's identity check — verify the PID still names
        // *our* tail (a `tail` invocation referencing this task's message
        // log) before signaling. A recycled/unidentifiable PID is left
        // alone: leaking a stray tail is far less harmful than killing a
        // bystander.
        if pid_is_task_tail(pid, task_id) {
            // SAFETY: libc::kill is unsafe only because it takes a raw
            // pid; we're not dereferencing memory. ESRCH (process gone)
            // is the expected case when the tail has already been reaped.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }
    // rm -rf the lock dir so a stale pid file from a crashed tail
    // doesn't confuse the next SessionStart hook into trying to kill a
    // recycled pid.
    match std::fs::remove_dir_all(&lock_dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Identity guard for [`kill_task_tail`]: does the live process `pid`
/// look like the message-tail the SessionStart hook spawned for
/// `task_id`? The hook runs `tail -f -n 0 .shelbi/messages/<id>.log`, so
/// a genuine tail's argv contains both `tail` and this task's log name.
/// A PID recycled to an unrelated process won't. When the argv can't be
/// read at all (process gone, EPERM, unsupported platform) we report
/// `false` — refusing to signal a process we can't positively identify.
fn pid_is_task_tail(pid: libc::pid_t, task_id: &str) -> bool {
    let blob = match process_argv_blob(pid) {
        Some(b) => b,
        None => return false,
    };
    let contains = |needle: &[u8]| blob.windows(needle.len()).any(|w| w == needle);
    contains(b"tail") && contains(format!("{task_id}.log").as_bytes())
}

/// Best-effort read of a process's raw argument blob (NUL-separated
/// argv, plus the exec path on macOS). Used only to *identify* a process
/// before signaling it, so callers substring-scan the blob rather than
/// parse argv precisely. `None` means the argv couldn't be read (process
/// gone, permission denied, or an unsupported platform).
#[cfg(target_os = "linux")]
fn process_argv_blob(pid: libc::pid_t) -> Option<Vec<u8>> {
    if pid <= 0 {
        return None;
    }
    std::fs::read(format!("/proc/{pid}/cmdline"))
        .ok()
        .filter(|b| !b.is_empty())
}

/// macOS: `sysctl(KERN_PROCARGS2)` returns `[argc:i32][exec_path\0]
/// [pad\0…][argv0\0][argv1\0]…`. We don't parse the layout — the caller
/// only needs to know whether a distinguishing substring is present — so
/// we return the whole buffer.
#[cfg(target_os = "macos")]
fn process_argv_blob(pid: libc::pid_t) -> Option<Vec<u8>> {
    if pid <= 0 {
        return None;
    }
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // First call sizes the buffer; a failure (e.g. process gone, or we
    // lack permission to read its args) means "identity unknown".
    // SAFETY: sysctl writes only into `size` when the value pointer is
    // null; `mib` is a valid 3-element array.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || size == 0 {
        return None;
    }
    let mut buf = vec![0u8; size];
    // SAFETY: `buf` holds `size` writable bytes; sysctl writes at most
    // `size` and updates `size` to the count actually written.
    let rc = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            mib.len() as libc::c_uint,
            buf.as_mut_ptr() as *mut libc::c_void,
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    buf.truncate(size);
    Some(buf)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn process_argv_blob(_pid: libc::pid_t) -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use shelbi_core::{
        AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
        Project, WorkspaceSpec, ZenConfig,
    };
    use std::collections::BTreeMap;
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};
    use std::time::Duration;

    /// Poll for `child` to exit, up to `timeout`; return its status if it does,
    /// otherwise SIGKILL + reap it and return `None`. Lets a test that expects
    /// a child to have been signaled fail fast instead of blocking the whole
    /// test binary on an unbounded `wait()` if the signal was ever missed.
    fn wait_or_kill(
        child: &mut std::process::Child,
        timeout: Duration,
    ) -> Option<std::process::ExitStatus> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match child.try_wait().expect("try_wait on child") {
                Some(status) => return Some(status),
                None if std::time::Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                None => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    #[test]
    fn wrapper_invocation_quotes_each_segment() {
        // The string lands inside `sh -c '<…>'` so each segment has to
        // come out individually quoted; otherwise a project or workspace
        // name containing a space would split across the argv.
        let s = wrapper_invocation("/usr/local/bin/shelbi", "my project", "alpha");
        assert!(s.contains("--project 'my project'"), "got: {s}");
        assert!(s.contains("open alpha --as-pane"), "got: {s}");
    }

    #[test]
    fn wrapper_invocation_simple_names_skip_quoting() {
        // Conservative-quoting path: `shell_escape` returns alphanumeric
        // (plus `-_./:=`) tokens unquoted so the rendered command is
        // readable in `ps` / pane captures.
        let s = wrapper_invocation("shelbi", "demo", "bravo");
        assert_eq!(s, "shelbi --project demo open bravo --as-pane");
    }

    #[test]
    fn exit_reason_prefers_received_signal() {
        let status = std::process::ExitStatus::from_raw(0);
        assert_eq!(
            exit_reason(&status, Some(signal_hook::consts::SIGTERM)),
            "signal:SIGTERM"
        );
    }

    #[test]
    fn exit_reason_falls_through_to_child_signal_then_exit_code() {
        // Child died from a signal we didn't catch (e.g. SIGSEGV in
        // claude itself). The reason field still names the signal.
        // ExitStatus::from_raw(<signal_number>) on Unix encodes "died
        // from this signal" — see waitpid(2)'s status word format.
        let died_from_sigsegv = std::process::ExitStatus::from_raw(libc::SIGSEGV);
        let r = exit_reason(&died_from_sigsegv, None);
        assert!(r.starts_with("signal:"), "got: {r}");

        // Clean exit with a code.
        let clean = std::process::ExitStatus::from_raw(7 << 8); // exit code 7
        assert_eq!(exit_reason(&clean, None), "exit:7");
    }

    #[test]
    fn signal_name_covers_the_three_we_install() {
        assert_eq!(signal_name(signal_hook::consts::SIGHUP), "SIGHUP");
        assert_eq!(signal_name(signal_hook::consts::SIGINT), "SIGINT");
        assert_eq!(signal_name(signal_hook::consts::SIGTERM), "SIGTERM");
        // Unknown signals fall back to a numeric string so the reason
        // remains a single token.
        assert_eq!(signal_name(99), "99");
    }

    /// Shelbi ContextStore docs/planning:reviews/adversarial-2026-07/cli-session-ux.md
    /// F11 acceptance: the signal listener is installed BEFORE the child
    /// is spawned, so a signal delivered in the spawn window is *captured*
    /// (not fatal to the wrapper) and forwarded to the child once its PID
    /// is published. This mirrors `run()`'s ordering: install with a
    /// zero-initialized PID cell, spawn, publish the PID, then a signal
    /// arrives. `signal-hook` has already replaced SIGTERM's default
    /// disposition by the time we send it, so signaling our own process
    /// is safe — it lands on the listener thread, not the default killer.
    /// Serialized under `ENV_LOCK` so this process-wide SIGTERM can't leak
    /// into a concurrent `run()`-based test's listener.
    #[test]
    fn signal_listener_installed_before_spawn_captures_and_forwards() {
        use std::time::Duration;
        let _g = ENV_LOCK.lock().unwrap();

        let received_signal: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let signaled_flag = Arc::new(AtomicBool::new(false));
        let child_pid_cell = Arc::new(AtomicI32::new(0));

        // Install with no child yet — exactly the pre-spawn ordering.
        let handle = install_signal_listener(
            Arc::clone(&received_signal),
            Arc::clone(&signaled_flag),
            Arc::clone(&child_pid_cell),
        )
        .expect("install listener");

        // Spawn the stand-in child AFTER install, then publish its PID the
        // way `run()` does immediately after `spawn()` returns.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let child_pid = child.id() as libc::pid_t;
        child_pid_cell.store(child_pid as i32, Ordering::SeqCst);

        // A signal arriving "immediately after spawn" must be caught by
        // the listener, not kill this process.
        // SAFETY: kill only delivers a signal; no memory is touched.
        assert_eq!(
            unsafe { libc::kill(std::process::id() as libc::pid_t, libc::SIGTERM) },
            0,
            "sending SIGTERM to self should succeed"
        );

        // The listener records the signal…
        let mut recorded = None;
        for _ in 0..200 {
            if let Some(s) = *received_signal.lock().unwrap() {
                recorded = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            recorded,
            Some(libc::SIGTERM),
            "listener must capture the signal instead of the wrapper dying"
        );

        // …and forwards it to the child, which dies. Reap with
        // `try_wait` rather than a bare `kill(pid, 0)` probe: the child is
        // a direct child of this process, so after SIGTERM it lingers as a
        // zombie (kill(pid, 0) would still return 0) until we wait on it.
        let mut exited = None;
        for _ in 0..200 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    exited = Some(status);
                    break;
                }
                Ok(None) => {}
                Err(_) => break,
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        handle.close();
        let status = exited.expect("listener must forward the captured signal to the child");
        assert!(
            !status.success(),
            "child should have died from the forwarded signal: {status:?}"
        );

        // `try_wait` above already reaped it on success; the guarded
        // cleanup covers the unlikely no-exit path.
        let _ = child.kill();
        let _ = child.wait();
    }

    /// Acceptance criterion: "When the agent subprocess exits (any
    /// reason), the wrapper emits `project=<name> workspace=<name>
    /// pane_alive=false reason=<short>` to the events log." Exercise the happy path
    /// (child exits 0) end-to-end with a stub runner so the wrapper's
    /// spawn-wait-emit dance is covered by tests, not just inspection.
    #[test]
    fn run_writes_pane_alive_false_event_when_agent_exits_cleanly() {
        let _g = ENV_LOCK.lock().unwrap();
        // A leaked TASK_ID (running tests inside a worker pane) would flip
        // run() into dispatched mode and hard-fail on the missing worktree.
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        env.remove("TASK_ID");
        env.remove("PROJECT");
        env.remove("SHELBI_HUB_SOCK");
        let home = fresh_test_home("pane-clean-exit");
        std::env::set_var("SHELBI_HOME", &home);

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // Runner is `/bin/sh -c 'exit 0'` — fast, deterministic, no
        // dependency on claude being installed for the test to run.
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), "exit 0".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(
            lines.len(),
            1,
            "expected exactly one event line, got: {log}"
        );
        let line = lines[0];
        assert!(line.contains(" workspace=alpha "), "line: {line}");
        assert!(line.contains(" pane_alive=false "), "line: {line}");
        assert!(line.ends_with(" reason=exit:0"), "line: {line}");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Phase 3 of Worker → Orchestrator Communication: the pane wrapper
    /// must pass `SHELBI_HUB_SOCK` into the agent's environment so the
    /// agent's `nc -U $SHELBI_HUB_SOCK` (or socat / python) one-liner can
    /// resolve the socket without re-deriving the path. Exercise the
    /// spawn path with a stub runner that records its env into a file
    /// and verify the variable lands.
    #[test]
    fn run_passes_shelbi_hub_sock_into_agent_env() {
        let _g = ENV_LOCK.lock().unwrap();
        // See run_writes_pane_alive_false_event_when_agent_exits_cleanly —
        // a leaked TASK_ID would make this bare launch dispatched.
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        env.remove("TASK_ID");
        env.remove("PROJECT");
        env.remove("SHELBI_HUB_SOCK");
        let home = fresh_test_home("pane-env-hub-sock");
        std::env::set_var("SHELBI_HOME", &home);
        // Clear any caller-supplied override so we test the default
        // resolution path (`hub_socket_path()` falls back to
        // `<SHELBI_HOME>/hub.sock` when `SHELBI_HUB_SOCK` is unset).
        std::env::remove_var("SHELBI_HUB_SOCK");

        let env_out = home.join("captured-env");
        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // Runner writes the value the wrapper pinned into its env into a
        // file the test reads back below. `printenv VAR` exits 1 if the
        // var is absent, which would also show up in events.log.
        let cmd = format!("printenv SHELBI_HUB_SOCK > {} ; exit 0", env_out.display());
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), cmd],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        let observed = std::fs::read_to_string(&env_out)
            .expect("agent should have written its SHELBI_HUB_SOCK env to disk");
        let observed = observed.trim_end_matches('\n');
        // The pinned value matches `hub_socket_path()` under the test
        // home — that's the same resolution the daemon uses to pick its
        // listen path, so the agent's writes land at the right place.
        let expected = home.join("hub.sock");
        assert_eq!(observed, expected.to_string_lossy());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A non-zero exit code from the agent surfaces in the events.log
    /// `reason=` field so the orchestrator's reaction rules can tell a
    /// clean quit from a crash. (Signal-driven exits are covered by
    /// `exit_reason_falls_through_to_child_signal_then_exit_code` —
    /// that's a unit-level test on the same helper.)
    #[test]
    fn run_propagates_nonzero_exit_code_into_reason_field() {
        let _g = ENV_LOCK.lock().unwrap();
        // See run_writes_pane_alive_false_event_when_agent_exits_cleanly —
        // a leaked TASK_ID would make this bare launch dispatched.
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        env.remove("TASK_ID");
        env.remove("PROJECT");
        env.remove("SHELBI_HUB_SOCK");
        let home = fresh_test_home("pane-nonzero-exit");
        std::env::set_var("SHELBI_HOME", &home);

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "bravo".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), "exit 42".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1, "expected one event line, got: {log}");
        assert!(lines[0].ends_with(" reason=exit:42"), "line: {}", lines[0]);

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    fn fresh_test_home(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-pane-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn local_machine(home: &Path) -> Machine {
        Machine {
            name: "hub".into(),
            kind: MachineKind::Local,
            work_dir: home.to_path_buf(),
            host: None,
            tags: Vec::new(),
            forward: None,
        }
    }

    /// Minimal `Project` that exercises the pane-wrapper code without
    /// loading project YAML from disk. The runner spec is supplied
    /// directly so a test can stub in a fast-exiting binary instead of
    /// requiring claude on PATH.
    fn fixture_project(
        name: &str,
        machine: Machine,
        workspace: WorkspaceSpec,
        runner: AgentRunnerSpec,
    ) -> Project {
        let mut runners = BTreeMap::new();
        runners.insert(workspace.runner.clone(), runner);
        Project {
            name: name.into(),
            repo: "git@example:repo.git".into(),
            default_branch: "main".into(),
            default_workflow: None,
            config_mode: None,
            machines: vec![machine],
            orchestrator: OrchestratorSpec {
                runner: workspace.runner.clone(),
            },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![workspace],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "default".into(),
            workspace_settings_template: None,
            zen: ZenConfig::default(),
            heartbeat: HeartbeatConfig::default(),
            git: GitConfig::default(),
            detected_shapes: Vec::new(),
        }
    }

    /// `assigned_task_for_workspace` finds the in-progress task whose
    /// `assigned_to` matches the workspace. The wrapper uses this to
    /// seed `$TASK_ID` for the Phase 7 hooks.
    #[test]
    fn assigned_task_for_workspace_returns_in_progress_assignment() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("current-task-lookup");
        std::env::set_var("SHELBI_HOME", &home);

        // Task in InProgress assigned to alpha → that's what the
        // wrapper should pick up.
        let mut task = make_task("feat-x", shelbi_core::Column::in_progress());
        task.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &task, "").unwrap();
        // Decoy: a Todo task assigned to alpha must NOT be returned.
        let mut decoy = make_task("backlog-y", shelbi_core::Column::todo());
        decoy.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &decoy, "").unwrap();

        assert_eq!(
            assigned_task_for_workspace("demo", "alpha").map(|t| t.id),
            Some("feat-x".to_string()),
        );
        // Workspace with nothing assigned → None.
        assert!(assigned_task_for_workspace("demo", "bravo").is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// The review path the bug is about: a task assigned to a workspace
    /// while it sits in **Review** (the handoff category) must still be
    /// resolved, so its worktree gets recovered on open. An in-progress-only
    /// lookup would miss it and leave the review agent in `$HOME`.
    #[test]
    fn assigned_task_for_workspace_returns_review_assignment() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("review-task-lookup");
        std::env::set_var("SHELBI_HOME", &home);

        let mut task = make_task("bug-z", shelbi_core::Column::review());
        task.assigned_to = Some("review".into());
        shelbi_state::save_task("demo", &task, "").unwrap();

        assert_eq!(
            assigned_task_for_workspace("demo", "review").map(|t| t.id),
            Some("bug-z".to_string()),
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `kill_task_tail` is idempotent: no pid file (the SessionStart
    /// hook never ran) is a clean no-op, not an error.
    #[test]
    fn kill_task_tail_is_noop_when_pid_file_missing() {
        let worktree = fresh_test_home("kill-tail-noop");
        // No `.shelbi/messages/...` dirs exist.
        kill_task_tail(&worktree, "feat-x").expect("must be a no-op");
        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// `kill_task_tail` reads the pid file, sends SIGTERM, and removes
    /// the lock dir. Use a real `tail -f` on the task's message log — the
    /// same command the SessionStart hook spawns — so it passes the
    /// PID-identity guard the way a genuine tail does. `kill` is
    /// observable via `wait()` returning a signaled status.
    #[test]
    fn kill_task_tail_kills_recorded_pid_and_removes_lock_dir() {
        let worktree = fresh_test_home("kill-tail-happy");
        let msgs = worktree.join(".shelbi").join("messages");
        let lock_dir = msgs.join("feat-x.tail.d");
        std::fs::create_dir_all(&lock_dir).unwrap();
        // The tail follows the task's message log; its argv then contains
        // both `tail` and `feat-x.log`, which is what the identity guard
        // looks for.
        let log = msgs.join("feat-x.log");
        std::fs::write(&log, b"").unwrap();

        let mut child = std::process::Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg("0")
            .arg(&log)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tail");

        // Wait until the tail's argv is visible in /proc as *our* tail before
        // recording its pid and signaling it. `Command::spawn` can return in
        // the sliver between fork and the exec'd argv becoming readable in
        // `/proc/<pid>/cmdline`; signaling in that window makes the identity
        // guard (`pid_is_task_tail`) read an empty cmdline, skip the SIGTERM,
        // and leave the tail alive — which used to hang the unbounded
        // `child.wait()` below and, with it, the whole test binary. Production
        // never sees this window: the tail lives for the entire agent session
        // before cleanup runs.
        let pid = child.id() as libc::pid_t;
        let ready_deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !pid_is_task_tail(pid, "feat-x") {
            assert!(
                std::time::Instant::now() < ready_deadline,
                "tail never became identifiable in /proc within 5s",
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        std::fs::write(lock_dir.join("pid"), child.id().to_string()).unwrap();

        kill_task_tail(&worktree, "feat-x").expect("kill must succeed");

        // Lock dir is gone.
        assert!(
            !lock_dir.exists(),
            "lock dir should be cleaned up: {}",
            lock_dir.display()
        );
        // Child got the signal — reap it, but bound the wait so a missed kill
        // fails loudly instead of hanging the whole test binary.
        let status = wait_or_kill(&mut child, Duration::from_secs(5))
            .expect("kill_task_tail should have signaled the tail; it never exited");
        assert!(
            !status.success(),
            "killed tail should not report success: {status:?}"
        );

        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// PID-reuse guard (Shelbi ContextStore
    /// docs/planning:reviews/adversarial-2026-07/cli-session-ux.md F11): if the recorded PID no longer names *our*
    /// tail — because the tail died and its PID was recycled to an
    /// unrelated process — `kill_task_tail` must NOT signal it. Stand in
    /// for the recycled process with a bare `sleep` (argv doesn't look
    /// like a `tail` on our log), record its PID, and assert it survives.
    /// The lock dir is still cleaned up so the stale pid file can't
    /// mislead the next hook.
    #[test]
    fn kill_task_tail_refuses_to_signal_pid_with_mismatched_identity() {
        let worktree = fresh_test_home("kill-tail-identity");
        let lock_dir = worktree
            .join(".shelbi")
            .join("messages")
            .join("feat-x.tail.d");
        std::fs::create_dir_all(&lock_dir).unwrap();

        // Bystander that recycled the tail's PID: a plain `sleep`, whose
        // argv contains neither `tail` nor `feat-x.log`.
        let mut bystander = std::process::Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        std::fs::write(lock_dir.join("pid"), bystander.id().to_string()).unwrap();

        kill_task_tail(&worktree, "feat-x").expect("cleanup must succeed");

        // The bystander must be untouched: kill(pid, 0) still succeeds.
        let alive = unsafe { libc::kill(bystander.id() as libc::pid_t, 0) } == 0;
        assert!(
            alive,
            "kill_task_tail must not signal a PID whose identity isn't our tail"
        );
        // But the stale lock dir was still cleaned up.
        assert!(
            !lock_dir.exists(),
            "lock dir should be cleaned up even when the kill is skipped"
        );

        // Reap our bystander so it doesn't linger past the test.
        let _ = bystander.kill();
        let _ = bystander.wait();
        let _ = std::fs::remove_dir_all(&worktree);
    }

    #[test]
    fn background_message_tail_output_is_unread_until_drained() {
        let worktree = fresh_test_home("tail-drain-regression");
        let msgs = worktree.join(".shelbi").join("messages");
        std::fs::create_dir_all(&msgs).unwrap();
        let log = msgs.join("feat-x.log");
        std::fs::write(&log, b"").unwrap();

        let mut child = std::process::Command::new("tail")
            .arg("-f")
            .arg("-n")
            .arg("0")
            .arg(&log)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn tail");

        let mut stdout = child.stdout.take().expect("tail stdout should be piped");
        let fd = stdout.as_raw_fd();
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
        assert!(flags >= 0, "F_GETFL failed");
        let set = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
        assert_eq!(set, 0, "F_SETFL O_NONBLOCK failed");

        let mut buf = [0u8; 256];
        match stdout.read(&mut buf) {
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!("tail should have no output before append: {other:?}"),
        }

        std::thread::sleep(Duration::from_millis(100));
        {
            let mut f = std::fs::OpenOptions::new().append(true).open(&log).unwrap();
            f.write_all(b"{\"msg_id\":\"m-1\",\"body\":\"wake\"}\n")
                .unwrap();
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let mut drained = Vec::new();
        while std::time::Instant::now() < deadline {
            match stdout.read(&mut buf) {
                Ok(0) => std::thread::sleep(Duration::from_millis(20)),
                Ok(n) => {
                    drained.extend_from_slice(&buf[..n]);
                    if drained.ends_with(b"\n") {
                        break;
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("read tail stdout: {e}"),
            }
        }

        let drained = String::from_utf8_lossy(&drained);
        assert!(
            drained.contains("\"msg_id\":\"m-1\""),
            "tail had unread output only once explicitly drained; got {drained:?}"
        );

        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// End-to-end: the agent subprocess sees `$PROJECT`, `$TASK_ID`,
    /// and `$SHELBI_HUB_SOCK` in its env. Stub runner dumps the three
    /// vars to a file we then assert on, so we exercise the actual
    /// `Command::env(...)` plumbing, not just the lookup helper.
    #[test]
    fn run_exports_phase7_env_vars_to_agent_subprocess() {
        let _g = ENV_LOCK.lock().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        let home = fresh_test_home("pane-env-exports");
        env.set("SHELBI_HOME", &home);
        env.remove("TASK_ID");
        env.remove("PROJECT");
        // Honor explicit override — verifies the wrapper doesn't stomp
        // a remote-pane SHELBI_HUB_SOCK that was set by the SSH layer.
        let sock_override = home.join("custom-hub.sock");
        env.set("SHELBI_HUB_SOCK", &sock_override);

        // Seed an in-progress task assigned to the workspace so the
        // wrapper's task lookup returns a non-empty TASK_ID.
        let mut task = make_task("feat-env", shelbi_core::Column::in_progress());
        task.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &task, "").unwrap();

        let dump_path = home.join("agent-env.dump");
        let dump_str = dump_path.to_string_lossy().into_owned();
        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // Runner prints the three vars (newline-separated, stable
        // order) into a file we then read back.
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec![
                "-c".into(),
                format!(
                    "printf '%s\\n%s\\n%s\\n' \"$PROJECT\" \"$TASK_ID\" \"$SHELBI_HUB_SOCK\" > {}",
                    dump_str
                ),
            ],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        let body = std::fs::read_to_string(&dump_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "expected 3 env-var lines, got: {body:?}");
        assert_eq!(lines[0], "demo", "PROJECT line: {body:?}");
        assert_eq!(lines[1], "feat-env", "TASK_ID line: {body:?}");
        assert_eq!(
            lines[2],
            sock_override.to_string_lossy(),
            "SHELBI_HUB_SOCK line: {body:?}",
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// Dispatch-originated launches hard-fail on a missing worktree
    /// (bug-worker-commit-landed-on-hub-main-checkout): an inherited
    /// `TASK_ID` marks the pane as dispatched, and with no valid worktree
    /// the wrapper must refuse to spawn the agent (no `$HOME` fallback),
    /// returning an error and emitting a `worktree-missing` event.
    #[test]
    fn dispatched_run_hard_fails_when_worktree_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        env.remove("PROJECT");
        env.remove("SHELBI_HUB_SOCK");
        let home = fresh_test_home("pane-dispatched-missing-wt");
        env.set("SHELBI_HOME", &home);
        env.set("TASK_ID", "feat-hard-fail");

        let marker = home.join("agent-ran");
        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // If the wrapper ever spawns the agent anyway, this file appears
        // and the assertion below catches it.
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), format!("touch {}", marker.display())],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        let err = run(&project, &workspace, &machine, false)
            .expect_err("dispatched launch with no worktree must hard-fail");
        assert!(
            err.to_string().contains("missing or broken"),
            "error must explain the worktree state: {err}"
        );
        assert!(
            !marker.exists(),
            "the agent must never have been spawned outside its worktree"
        );

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(
            log.contains(" pane_alive=false ") && log.contains(" reason=worktree-missing"),
            "hard-fail must be visible in events.log, got: {log}"
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// The dispatched hard-fail only triggers on a missing/broken
    /// worktree: with a valid one in place, an inherited `TASK_ID` launch
    /// proceeds normally (and the strict no-fallback `cd` succeeds).
    #[test]
    fn dispatched_run_proceeds_with_valid_worktree() {
        let _g = ENV_LOCK.lock().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        env.remove("PROJECT");
        env.remove("SHELBI_HUB_SOCK");
        let home = fresh_test_home("pane-dispatched-valid-wt");
        env.set("SHELBI_HOME", &home);
        env.set("TASK_ID", "feat-valid");

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // A linked worktree's `.git` is a gitlink *file* — stand one up
        // so the validity check sees a real-shaped worktree without
        // needing a full git repo in the fixture.
        let worktree = orch_workspace::workspace_worktree(&machine, &workspace);
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();

        let marker = worktree.join("agent-ran");
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            // `touch` relative to cwd: also proves the agent started IN
            // the worktree, not in a fallback dir.
            flags: vec!["-c".into(), "touch agent-ran".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        assert!(
            marker.exists(),
            "agent should have run inside the worktree at {}",
            worktree.display()
        );

        let _ = std::fs::remove_dir_all(&home);
    }

    /// End-to-end: when the agent exits, any tail process the
    /// SessionStart hook spawned is reaped — no orphan tails leak
    /// across worker restarts. Stub runner spawns a `sleep` standing in
    /// for `tail -f`, records its pid in the lock file the way the hook
    /// would, then exits 0. After `run()` returns, the tail must be
    /// gone and the lock dir cleaned up.
    #[test]
    fn run_kills_session_tail_on_agent_exit() {
        let _g = ENV_LOCK.lock().unwrap();
        // See run_writes_pane_alive_false_event_when_agent_exits_cleanly —
        // a leaked TASK_ID would make this bare launch dispatched (and the
        // stood-up worktree here has no `.git`, so it would hard-fail).
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        env.remove("TASK_ID");
        env.remove("PROJECT");
        env.remove("SHELBI_HUB_SOCK");
        let home = fresh_test_home("pane-tail-cleanup");
        std::env::set_var("SHELBI_HOME", &home);

        let mut task = make_task("feat-tail", shelbi_core::Column::in_progress());
        task.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &task, "").unwrap();

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // worktree path the spawn path will compute for this workspace
        // — needs to exist so the stub runner's `cd` doesn't fall back
        // to $HOME (which would put the lock dir under the test's
        // SHELBI_HOME, not under the worktree the cleanup helper looks
        // at).
        let worktree = orch_workspace::workspace_worktree(&machine, &workspace);
        std::fs::create_dir_all(&worktree).unwrap();

        // Mirror of the file the SessionStart hook records so we can
        // verify the cleanup helper actually reaped the tail. Sequence
        // the runner script with `;` not `&&` so the mkdir doesn't get
        // accidentally backgrounded along with the tail. The stand-in is
        // a real `tail -f` on the task's message log (mirroring the
        // SessionStart hook) so it passes the PID-identity guard.
        let pid_mirror = worktree.join("tail-pid-mirror");
        let pid_mirror_str = pid_mirror.to_string_lossy().into_owned();
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec![
                "-c".into(),
                format!(
                    "mkdir -p .shelbi/messages/$TASK_ID.tail.d; \
                     touch .shelbi/messages/$TASK_ID.log; \
                     tail -f -n 0 .shelbi/messages/$TASK_ID.log > .shelbi/messages/$TASK_ID.unread.log 2>/dev/null & \
                     echo $! > .shelbi/messages/$TASK_ID.tail.d/pid; \
                     cp .shelbi/messages/$TASK_ID.tail.d/pid {pid_mirror_str}; \
                     exit 0"
                ),
            ],
            prompt_injection: None, dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        // The lock dir must be cleaned up by the wrapper.
        let lock_dir = worktree
            .join(".shelbi")
            .join("messages")
            .join("feat-tail.tail.d");
        assert!(
            !lock_dir.exists(),
            "lock dir should be removed on exit, but exists at {}",
            lock_dir.display()
        );

        // And the recorded tail pid must no longer be alive — kill(0)
        // returns ESRCH (so kill returns -1) when the process is gone.
        let mirrored =
            std::fs::read_to_string(&pid_mirror).expect("runner must have written the pid mirror");
        let pid: libc::pid_t = mirrored.trim().parse().expect("pid mirror parses");
        // Give the OS a tick to actually reap the SIGTERM'd child.
        for _ in 0..20 {
            // SAFETY: kill with sig=0 only checks existence.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if !alive {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        assert!(!alive, "tail pid {pid} should have been killed");

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Tiny Task fixture for in-test seeding. Mirrors the larger
    /// `make_task` in shelbi-state — we don't import it because that
    /// helper lives behind `#[cfg(test)]` and isn't reachable across
    /// crates.
    fn make_task(id: &str, column: shelbi_core::Column) -> shelbi_core::Task {
        let now = chrono::Utc::now();
        shelbi_core::Task {
            id: id.to_string(),
            title: id.replace('-', " "),
            column,
            priority: 0,
            assigned_to: None,
            workflow: None,
            branch: None,
            depends_on: Vec::new(),
            prefers_machine: None,
            zen: None,
            created_at: now,
            updated_at: now,
            params: std::collections::BTreeMap::new(),
        }
    }

    /// Bug regression: a shelbi-initiated pane teardown (dispatch,
    /// quit-project, quit-shelbi, `workspace stop`) marks the workspace's
    /// `.expected-teardown` file just before killing tmux. The wrapper's
    /// exit path must then suppress the `pane_alive=false` event —
    /// otherwise every dispatch fires a spurious pane-death line and
    /// trips the orchestrator's "flag it to the user" reaction rule for
    /// panes that are actually about to come back up.
    /// (bug-workspace-pane-alive-false-sighup-fires-spuriously-right-after-dispatch)
    #[test]
    fn run_suppresses_pane_event_when_expected_teardown_marker_is_set() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("pane-expected-teardown-suppress");
        std::env::set_var("SHELBI_HOME", &home);
        // A caller-set SHELBI_HUB_SOCK would route the wrapper's event
        // emit to a real daemon (writing to that daemon's events.log,
        // not this test's home) — clear it so we stay on the fallback
        // path that appends directly to `<SHELBI_HOME>/events.log`.
        std::env::remove_var("SHELBI_HUB_SOCK");

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // Runner writes the marker mid-run (simulates a concurrent
        // `shelbi task start` marking the teardown while the wrapper is
        // still alive), then exits 0. Real dispatch does the mark BEFORE
        // sending kill-window; from the wrapper's point of view the
        // ordering that matters is "marker present when the exit path
        // reads it", which either sequence satisfies.
        let mark_cmd = format!(
            "mkdir -p {home}/workspaces/alpha && \
             : > {home}/workspaces/alpha/.expected-teardown && \
             exit 0",
            home = home.display(),
        );
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), mark_cmd],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        // No pane_alive=false event landed — the intentional-teardown
        // marker consumed the emission.
        let log_path = shelbi_state::events_log_path().unwrap();
        let body = if log_path.exists() {
            std::fs::read_to_string(&log_path).unwrap()
        } else {
            String::new()
        };
        assert!(
            !body.contains(" pane_alive=false "),
            "expected teardown must not emit a pane-death event; log: {body:?}"
        );

        // And the marker was consumed (removed) so a subsequent unrelated
        // exit can't accidentally pick it up.
        let marker = shelbi_state::expected_teardown_marker_path("alpha").unwrap();
        assert!(
            !marker.exists(),
            "marker should be removed on consume, but exists at {}",
            marker.display()
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A stale `.expected-teardown` marker (older than the freshness
    /// window) must NOT suppress a real pane_alive event — it represents
    /// a shelbi kill that never actually completed, so this exit is
    /// unrelated to that intent. The consume path deletes the stale
    /// marker either way so it can't leak further forward.
    #[test]
    fn run_ignores_stale_expected_teardown_marker() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("pane-expected-teardown-stale");
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // The runner re-plants the marker AFTER the wrapper's startup
        // `clear_expected_teardown` has already run, then rewinds its
        // mtime past the freshness window. On exit the consume side sees
        // "marker present but stale" → doesn't suppress. `touch -t`'s
        // `[[CC]YY]MMDDhhmm` format is the intersection of BSD and GNU
        // touch, so this works on both macOS and Linux CI runners.
        let stale_plant = format!(
            "mkdir -p {home}/workspaces/alpha && \
             : > {home}/workspaces/alpha/.expected-teardown && \
             touch -t 202001010000 {home}/workspaces/alpha/.expected-teardown && \
             exit 0",
            home = home.display(),
        );
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), stale_plant],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        // A stale marker must not suppress: the real exit event fires.
        let log_path = shelbi_state::events_log_path().unwrap();
        let body = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            body.contains(" pane_alive=false "),
            "stale marker must not suppress pane-death event; log: {body:?}"
        );

        // Marker was still cleared so it can't linger.
        let marker = shelbi_state::expected_teardown_marker_path("alpha").unwrap();
        assert!(
            !marker.exists(),
            "stale marker should be consumed (deleted) even when not honored"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A stale marker left over from a mark-then-SIGKILL race must not
    /// survive across wrapper lifecycles: the wrapper clears the marker
    /// at startup so it can't accidentally suppress a later, unrelated
    /// exit that happens to fall inside the freshness window. Exercises
    /// the `clear_expected_teardown` call at the top of `run()`.
    #[test]
    fn run_clears_expected_teardown_at_startup_before_natural_exit() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("pane-expected-teardown-startup-clear");
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");

        // Plant a FRESH marker directly, then run the wrapper with a
        // runner that does NOT write a marker of its own. If the startup
        // clear works, this marker is gone by the time the exit path
        // runs, so the exit fires the pane_alive event normally.
        let ws_dir = home.join("workspaces").join("alpha");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(ws_dir.join(".expected-teardown"), b"").unwrap();

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), "exit 0".into()],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        // Startup cleared the marker → exit path saw no marker → event
        // fired. This is the belt in the belt-and-suspenders defense.
        let log_path = shelbi_state::events_log_path().unwrap();
        let body = std::fs::read_to_string(&log_path).unwrap();
        assert!(
            body.contains(" pane_alive=false "),
            "startup-clear must remove leftover marker so the natural exit still emits; log: {body:?}"
        );

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// Regression: the pane wrapper must honor an inherited `TASK_ID`
    /// env even when the on-disk state doesn't have the task in
    /// `in_progress` yet. This is the exact race the dispatch path
    /// hits: `start_workspace_on_task` fires the tmux window (which
    /// runs this wrapper) BEFORE writing the task's assignment/column,
    /// so a wrapper that only consulted the state store would see
    /// TASK_ID="" and the SessionStart hook would silently no-op.
    #[test]
    fn run_prefers_inherited_task_id_over_state_lookup() {
        let _g = ENV_LOCK.lock().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME", "TASK_ID", "PROJECT", "SHELBI_HUB_SOCK"]);
        let home = fresh_test_home("pane-inherited-task-id");
        env.set("SHELBI_HOME", &home);
        // No task in state → the state-lookup fallback would resolve
        // TASK_ID="". The tmux -e injection is simulated by setting
        // TASK_ID directly in the process env before `run()`.
        env.set("TASK_ID", "feat-race");
        // Same override treatment for PROJECT / SHELBI_HUB_SOCK, since
        // the wrapper pins all three on the child.
        env.set("PROJECT", "demo");
        let sock_override = home.join("custom-hub.sock");
        env.set("SHELBI_HUB_SOCK", &sock_override);

        let dump_path = home.join("agent-env.dump");
        let dump_str = dump_path.to_string_lossy().into_owned();
        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
            tags: Vec::new(),
            slot: None,
        };
        // An inherited TASK_ID marks the launch as dispatch-originated,
        // which now requires a valid worktree (in the real race,
        // sync_worktree stood it up before the pane spawned). A linked
        // worktree's `.git` is a gitlink file — fake one.
        let worktree = orch_workspace::workspace_worktree(&machine, &workspace);
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: elsewhere\n").unwrap();
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec![
                "-c".into(),
                format!(
                    "printf '%s\\n%s\\n%s\\n' \"$PROJECT\" \"$TASK_ID\" \"$SHELBI_HUB_SOCK\" > {}",
                    dump_str
                ),
            ],
            prompt_injection: None,
            dialog_signatures: vec![],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine, false).unwrap();

        let body = std::fs::read_to_string(&dump_path).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(
            lines[1], "feat-race",
            "inherited TASK_ID must win: {body:?}"
        );

        let _ = std::fs::remove_dir_all(&home);
    }
}
