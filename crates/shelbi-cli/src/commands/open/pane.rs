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
use std::process::{Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use anyhow::{anyhow, Result};
use shelbi_core::{Machine, Project, WorkspaceSpec};
use shelbi_orchestrator::workspace as orch_workspace;

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

    // Signal handling: arrange for SIGHUP / SIGTERM / SIGINT to be
    // captured in a background thread that records which one fired and
    // proactively forwards it to the child. The wait() below returns
    // either way (Unix kernels propagate process-group signals to
    // children too); recording the signal lets us label the events.log
    // reason and skip the "press enter" prompt on a forced teardown.
    let received_signal: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
    let signaled_flag = Arc::new(AtomicBool::new(false));

    let mut child = Command::new("sh")
        .arg("-c")
        .arg(&shell_cmd)
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

    let signaled = *received_signal.lock().unwrap();
    let reason = exit_reason(&status, signaled);

    // Best-effort: a failure here shouldn't keep the pane from closing.
    if let Err(e) = shelbi_state::append_workspace_pane_event(&workspace.name, false, &reason) {
        eprintln!(
            "shelbi: warning: couldn't write workspace pane-death event for `{}`: {e}",
            workspace.name
        );
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
