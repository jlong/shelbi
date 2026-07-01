//! `shelbi open <name> --as-pane` — the lifecycle wrapper that
//! becomes a workspace pane's top-level process. It owns the agent
//! subprocess, installs signal handlers so tmux teardown / kill-window /
//! Ctrl-C all flow through the same exit path, and writes a
//! `workspace=<name> pane_alive=false reason=<short>` line to
//! `~/.shelbi/events.log` on every termination so the orchestrator's
//! reaction rules can fire on pane death.
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
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, Result};
use shelbi_core::{Column, Machine, Project, WorkspaceSpec};
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
/// hub, `/tmp/shelbi-hub.sock` on remote panes; here we default to the
/// hub path but honor an existing value so tests / remote panes can
/// override.
const ENV_HUB_SOCK: &str = "SHELBI_HUB_SOCK";

/// Build the `sh -c …`-suitable command string that re-enters this
/// binary under `--as-pane`. Exposed so `focus_or_create` and
/// `start_workspace_on_task` use the exact same string and can't drift.
pub fn wrapper_invocation(shelbi_bin: &str, project: &str, workspace: &str) -> String {
    format!(
        "{bin} --project {proj} open {ws} --as-pane",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project),
        ws = shelbi_agent::shell_escape(workspace),
    )
}

/// Run the wrapper: spawn the agent, wait, emit the lifecycle event,
/// optionally hold the pane open for a final keypress.
pub fn run(project: &Project, workspace: &WorkspaceSpec, machine: &Machine) -> Result<()> {
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
    let runner_with_mode =
        shelbi_agent::with_permission_mode(&runner, &project.workspace_permissions_mode);
    let launch = shelbi_agent::launch_command(&runner_with_mode);

    let worktree = orch_workspace::workspace_worktree(machine, workspace);
    let worktree_str = worktree.to_string_lossy().into_owned();

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
    let launch_full = if is_claude_runner(&runner_with_mode) && has_agent_instructions {
        format!(
            "{launch} --append-system-prompt \"$(cat {rel})\"",
            rel = shelbi_agent::shell_escape(orch_workspace::WORKTREE_AGENT_INSTRUCTIONS_REL),
        )
    } else {
        launch
    };

    // `cd` falls back to $HOME if the worktree doesn't exist — that keeps
    // a sidebar click on a never-used workspace from leaving the user in
    // an empty pane that immediately closes.
    let shell_cmd = format!(
        "cd {wd} 2>/dev/null || cd; LANG=C.UTF-8 {launch_full}",
        wd = shelbi_agent::shell_escape(&worktree_str),
    );

    // Look up the task currently assigned to this workspace so the
    // Phase 7 hooks (SessionStart tail + Stop message-inject) have a
    // concrete `$TASK_ID` to anchor their per-task paths on. Best-effort
    // — a workspace with no in-progress task assigned still gets a pane
    // (and the hooks no-op on empty TASK_ID), so the lookup never blocks
    // the spawn.
    let task_id = current_task_for_workspace(&project.name, &workspace.name).unwrap_or_default();

    // Signal handling: arrange for SIGHUP / SIGTERM / SIGINT to be
    // captured in a background thread that records which one fired and
    // proactively forwards it to the child. The wait() below returns
    // either way (Unix kernels propagate process-group signals to
    // children too); recording the signal lets us label the events.log
    // reason and skip the "press enter" prompt on a forced teardown.
    let received_signal: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    let signaled_flag = Arc::new(AtomicBool::new(false));

    // Phase 3: pin `SHELBI_HUB_SOCK` so the agent's `nc -U` / socat /
    // python socket-write one-liners resolve to the same path the daemon
    // is listening on. `hub_socket_path()` already honors the env var if
    // set (e.g. remote panes whose value points at the SSH reverse-forward
    // landing path), so the agent inherits the same resolution.
    //
    // Phase 7: also export `$PROJECT` and `$TASK_ID` so the Claude Code
    // SessionStart + Stop hooks can write to and tail the per-task
    // message log at `.shelbi/messages/$TASK_ID.log`.
    let hub_sock = shelbi_state::hub_socket_path()
        .map_err(|e| anyhow!("resolving hub socket path: {e}"))?;
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
    let child_pid = child.id() as libc::pid_t;

    let signal_handle = install_signal_listener(
        Arc::clone(&received_signal),
        Arc::clone(&signaled_flag),
        child_pid,
    )?;

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
    // `workspace=<name> pane_alive=false reason=signal:SIGHUP` right
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
        if let Err(e) = shelbi_state::append_workspace_pane_event(&workspace.name, false, &reason) {
            eprintln!(
                "shelbi: warning: couldn't write workspace pane-death event for `{}`: {e}",
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
fn install_signal_listener(
    received_signal: Arc<Mutex<Option<i32>>>,
    signaled_flag: Arc<AtomicBool>,
    child_pid: libc::pid_t,
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
            // Forward the signal to the child. Errors here are benign:
            // the child may have already exited (ESRCH) or the process
            // group may have delivered the signal already.
            unsafe {
                libc::kill(child_pid, sig);
            }
        }
    });
    Ok(handle)
}

/// Compose a short reason token for the events.log line. Signal-driven
/// exits get `signal:<name>`; natural exits get `exit:<code>`; the
/// no-info path collapses to `exit:unknown`.
fn exit_reason(status: &std::process::ExitStatus, received: Option<i32>) -> String {
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

/// Look up the in-progress task assigned to `workspace`, if any. Used
/// by the pane wrapper to seed `$TASK_ID` for the Phase 7 hooks. The
/// poller's invariant guarantees at most one in-progress task per
/// workspace (it dispatches sequentially), so `find` is correct — if
/// the invariant ever breaks we want the first match, not a silent
/// collapse to None.
///
/// Best-effort: returns `None` on read errors (missing project state,
/// permissions glitch, transient FS) because a missing `TASK_ID` makes
/// the hooks no-op but doesn't break the pane.
fn current_task_for_workspace(project: &str, workspace: &str) -> Option<String> {
    let in_progress = shelbi_state::list_column(project, Column::InProgress).ok()?;
    in_progress.into_iter().find_map(|tf| {
        if tf.task.assigned_to.as_deref() == Some(workspace) {
            Some(tf.task.id)
        } else {
            None
        }
    })
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
        // SAFETY: libc::kill is unsafe only because it takes a raw pid;
        // we're not dereferencing memory. ESRCH (process gone) is the
        // expected case when the tail has already been reaped.
        unsafe {
            libc::kill(pid, libc::SIGTERM);
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

fn is_claude_runner(spec: &shelbi_core::AgentRunnerSpec) -> bool {
    std::path::Path::new(&spec.command)
        .file_name()
        .and_then(|s| s.to_str())
        == Some("claude")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::ENV_LOCK;
    use shelbi_core::{
        AgentRunnerSpec, GitConfig, HeartbeatConfig, Machine, MachineKind, OrchestratorSpec,
        Project, WorkspaceSpec, ZenConfig,
    };
    use std::collections::BTreeMap;
    use std::os::unix::process::ExitStatusExt;
    use std::path::{Path, PathBuf};

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

    /// Acceptance criterion: "When the agent subprocess exits (any
    /// reason), the wrapper emits `workspace=<name> pane_alive=false
    /// reason=<short>` to the events log." Exercise the happy path
    /// (child exits 0) end-to-end with a stub runner so the wrapper's
    /// spawn-wait-emit dance is covered by tests, not just inspection.
    #[test]
    fn run_writes_pane_alive_false_event_when_agent_exits_cleanly() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("pane-clean-exit");
        std::env::set_var("SHELBI_HOME", &home);

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
        };
        // Runner is `/bin/sh -c 'exit 0'` — fast, deterministic, no
        // dependency on claude being installed for the test to run.
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), "exit 0".into()],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines.len(), 1, "expected exactly one event line, got: {log}");
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
        };
        // Runner writes the value the wrapper pinned into its env into a
        // file the test reads back below. `printenv VAR` exits 1 if the
        // var is absent, which would also show up in events.log.
        let cmd = format!(
            "printenv SHELBI_HUB_SOCK > {} ; exit 0",
            env_out.display()
        );
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), cmd],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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
        let home = fresh_test_home("pane-nonzero-exit");
        std::env::set_var("SHELBI_HOME", &home);

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "bravo".into(),
            machine: "hub".into(),
            runner: "stub".into(),
        };
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), "exit 42".into()],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
        }
    }

    /// `current_task_for_workspace` finds the in-progress task whose
    /// `assigned_to` matches the workspace. The wrapper uses this to
    /// seed `$TASK_ID` for the Phase 7 hooks.
    #[test]
    fn current_task_for_workspace_returns_in_progress_assignment() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("current-task-lookup");
        std::env::set_var("SHELBI_HOME", &home);

        // Task in InProgress assigned to alpha → that's what the
        // wrapper should pick up.
        let mut task = make_task("feat-x", shelbi_core::Column::InProgress);
        task.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &task, "").unwrap();
        // Decoy: a Todo task assigned to alpha must NOT be returned.
        let mut decoy = make_task("backlog-y", shelbi_core::Column::Todo);
        decoy.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &decoy, "").unwrap();

        assert_eq!(
            current_task_for_workspace("demo", "alpha").as_deref(),
            Some("feat-x"),
        );
        // Workspace with nothing assigned → None.
        assert_eq!(current_task_for_workspace("demo", "bravo"), None);

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
    /// the lock dir. Use a `sleep` child as a stand-in for the tail
    /// process — `kill` is observable via `wait()` returning a signaled
    /// status.
    #[test]
    fn kill_task_tail_kills_recorded_pid_and_removes_lock_dir() {
        let worktree = fresh_test_home("kill-tail-happy");
        let lock_dir = worktree
            .join(".shelbi")
            .join("messages")
            .join("feat-x.tail.d");
        std::fs::create_dir_all(&lock_dir).unwrap();

        // Long-sleeping child stands in for the tail. SIGTERM should
        // reap it well before the natural timeout fires.
        let mut child = std::process::Command::new("sleep")
            .arg("60")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        std::fs::write(lock_dir.join("pid"), child.id().to_string()).unwrap();

        kill_task_tail(&worktree, "feat-x").expect("kill must succeed");

        // Lock dir is gone.
        assert!(
            !lock_dir.exists(),
            "lock dir should be cleaned up: {}",
            lock_dir.display()
        );
        // Child got the signal — wait reaps it within a tiny window.
        let status = child.wait().expect("wait sleep");
        assert!(
            !status.success(),
            "killed sleep should not report success: {status:?}"
        );

        let _ = std::fs::remove_dir_all(&worktree);
    }

    /// End-to-end: the agent subprocess sees `$PROJECT`, `$TASK_ID`,
    /// and `$SHELBI_HUB_SOCK` in its env. Stub runner dumps the three
    /// vars to a file we then assert on, so we exercise the actual
    /// `Command::env(...)` plumbing, not just the lookup helper.
    #[test]
    fn run_exports_phase7_env_vars_to_agent_subprocess() {
        let _g = ENV_LOCK.lock().unwrap();
        let home = fresh_test_home("pane-env-exports");
        std::env::set_var("SHELBI_HOME", &home);
        // Honor explicit override — verifies the wrapper doesn't stomp
        // a remote-pane SHELBI_HUB_SOCK that was set by the SSH layer.
        let sock_override = home.join("custom-hub.sock");
        std::env::set_var("SHELBI_HUB_SOCK", &sock_override);

        // Seed an in-progress task assigned to the workspace so the
        // wrapper's task lookup returns a non-empty TASK_ID.
        let mut task = make_task("feat-env", shelbi_core::Column::InProgress);
        task.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &task, "").unwrap();

        let dump_path = home.join("agent-env.dump");
        let dump_str = dump_path.to_string_lossy().into_owned();
        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
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
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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

        std::env::remove_var("SHELBI_HOME");
        std::env::remove_var("SHELBI_HUB_SOCK");
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
        let home = fresh_test_home("pane-tail-cleanup");
        std::env::set_var("SHELBI_HOME", &home);

        let mut task = make_task("feat-tail", shelbi_core::Column::InProgress);
        task.assigned_to = Some("alpha".into());
        shelbi_state::save_task("demo", &task, "").unwrap();

        let machine = local_machine(&home);
        let workspace = WorkspaceSpec {
            name: "alpha".into(),
            machine: "hub".into(),
            runner: "stub".into(),
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
        // accidentally backgrounded along with the sleep.
        let pid_mirror = worktree.join("tail-pid-mirror");
        let pid_mirror_str = pid_mirror.to_string_lossy().into_owned();
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec![
                "-c".into(),
                format!(
                    "mkdir -p .shelbi/messages/$TASK_ID.tail.d; \
                     sleep 60 & \
                     echo $! > .shelbi/messages/$TASK_ID.tail.d/pid; \
                     cp .shelbi/messages/$TASK_ID.tail.d/pid {pid_mirror_str}; \
                     exit 0"
                ),
            ],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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
        };
        let runner = AgentRunnerSpec {
            command: "/bin/sh".into(),
            flags: vec!["-c".into(), "exit 0".into()],
        };
        let project = fixture_project("demo", machine.clone(), workspace.clone(), runner);

        run(&project, &workspace, &machine).unwrap();

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

    #[test]
    fn is_claude_runner_recognizes_basename() {
        let claude = shelbi_core::AgentRunnerSpec {
            command: "claude".into(),
            flags: vec![],
        };
        let claude_abs = shelbi_core::AgentRunnerSpec {
            command: "/opt/homebrew/bin/claude".into(),
            flags: vec![],
        };
        let codex = shelbi_core::AgentRunnerSpec {
            command: "codex".into(),
            flags: vec![],
        };
        assert!(is_claude_runner(&claude));
        assert!(is_claude_runner(&claude_abs));
        assert!(!is_claude_runner(&codex));
    }
}
