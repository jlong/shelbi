//! Review-workspace server panes: a long-lived dev server running beside the
//! Review agent so a human can *run* the change under review, not just read
//! the diff.
//!
//! Everything else in shelbi is one-process-per-pane, one-pane-per-window
//! (see [`crate::workspace`]). A review workspace is the first place that
//! needs two live processes at once: the interactive agent pane and a
//! persistent server pane bound to a port. This module owns that second
//! pane's whole lifecycle — spawn, liveness, teardown, and the reaper that
//! sweeps a leaked server whose task has moved on.
//!
//! ## Model
//!
//! The server pane is a `tmux split-window` inside the workspace's *agent*
//! window (precedent: the dashboard's stash splits in `lib.rs`). We track it
//! by the stable pane id tmux hands back from `split-window -P -F
//! '#{pane_id}'`, persisted in a [`shelbi_state::ServerPaneRecord`] sidecar —
//! *not* by window/pane title, because `automatic-rename` rewrites titles out
//! from under us (the same reason [`crate::workspace::kill_workspace_pane`]
//! matches on session, not window name, for remote teardown).
//!
//! The pane's top-level process is the `shelbi open <ws> --as-server-pane`
//! lifecycle wrapper, mirroring the agent pane's `--as-pane` wrapper: it owns
//! the serve subprocess and emits a `workspace=<name> server_alive=false`
//! event on any exit so the orchestrator can react to a server death.
//!
//! ## Liveness without stable titles (spec §10 / §15)
//!
//! Two independent signals, matching the plan:
//!
//! 1. **pane liveness** — [`server_pane_alive`] asks tmux whether the tracked
//!    pane id still exists. The wrapper's `server_alive=false` event is the
//!    push-notification version of the same fact.
//! 2. **HTTP ready-probe** — [`wait_for_http_ready`] gates the "ready" signal
//!    on the server actually answering on its port, so "pane is up" is never
//!    mistaken for "URL is serving."
//!
//! ## Reaper (spec §10, load-bearing)
//!
//! A crashed or abandoned server that stays bound to its port blocks the next
//! dispatch onto that review workspace. [`reap_server_pane_if_leaked`] kills a
//! server pane whose workspace no longer has an active (in-progress / review)
//! task — covering crashes, leaked ports, and missed teardowns. It also
//! garbage-collects the record when the pane has already died on its own.
//!
//! ## Scope
//!
//! Local (hub) review workspaces are fully supported. Remote review
//! workspaces need the reachable-URL / tunnel story called out as an open
//! risk in the spec (§15) and the lifecycle wrapper isn't deployed to remote
//! machines, so [`spawn_server_pane`] returns a clear error there rather than
//! standing up a server we can't observe or reap.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, Instant};

use shelbi_core::{Column, Error, Host, Machine, Project, Result, WorkspaceSpec};
use shelbi_state::ServerPaneRecord;

use crate::workspace::workspace_tmux_addr;

/// Inputs for [`spawn_server_pane`]. The serve command and port come from the
/// caller (the review dispatch, once it lands) rather than from project
/// config, so this module stays independent of the review-config schema.
pub struct ServerPaneParams<'a> {
    pub project: &'a Project,
    pub workspace: &'a WorkspaceSpec,
    /// The task whose branch the server is serving — recorded so the reaper
    /// can tell a live review from a leaked one.
    pub task_id: &'a str,
    /// Port shelbi assigned to this workspace's server (exported as `$PORT`).
    pub port: u16,
    /// The shell command that starts the long-lived dev server. Runs under
    /// `sh -c` with `$PORT` in the environment (e.g. `npm run dev -- --port
    /// $PORT`). The caller is responsible for having it bind localhost by
    /// default where the framework allows (spec §15).
    pub serve: &'a str,
}

/// Spawn the server pane for a review workspace and persist its record.
///
/// Splits a new pane into the workspace's agent window running the
/// `--as-server-pane` wrapper, captures the stable pane id, writes a
/// [`ServerPaneRecord`] sidecar, and returns it. The agent pane is left
/// untouched and interactive.
///
/// Idempotency: any server pane already recorded for this workspace is torn
/// down first, so a re-spawn (e.g. refresh for a new branch) can never leak
/// the previous server or its port.
pub fn spawn_server_pane(params: ServerPaneParams<'_>) -> Result<ServerPaneRecord> {
    let machine = params
        .project
        .machine(&params.workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(params.workspace.machine.clone()))?;
    let host = machine.host();

    // Remote review workspaces: the lifecycle wrapper isn't on the remote
    // host and the reachable-URL story is an unresolved open risk (spec §15).
    // Fail loudly rather than stand up a server we can't observe or reap.
    if !matches!(host, Host::Local) {
        return Err(Error::Other(format!(
            "server panes are not yet supported for remote review workspaces \
             (workspace `{}` lives on machine `{}`)",
            params.workspace.name, params.workspace.machine,
        )));
    }

    // Never leak a prior server: tear down whatever this workspace already
    // owns before splitting a fresh one.
    kill_server_pane(&host, &params.workspace.name)?;

    let addr = workspace_tmux_addr(params.project, params.workspace)?;
    let shelbi_bin = current_exe_string()?;
    let wrapper = server_wrapper_invocation(
        &shelbi_bin,
        &params.project.name,
        &params.workspace.name,
    );

    // `-v` stacks the server under the agent (matches the dashboard stash
    // splits); `-d` keeps focus on the agent pane so an automated dispatch
    // doesn't yank the human's cursor into the server. `-P -F '#{pane_id}'`
    // hands back the stable pane id we track by. `-e` injects the port and
    // serve command the wrapper reads (same mechanism the agent pane's tmux
    // launch uses for TASK_ID / PROJECT).
    let port_env = format!("{}={}", ENV_PORT, params.port);
    let serve_env = format!("{}={}", ENV_SERVE_CMD, params.serve);
    let target = addr.target();
    let argv = vec![
        "tmux",
        "split-window",
        "-v",
        "-d",
        "-t",
        target.as_str(),
        "-P",
        "-F",
        "#{pane_id}",
        "-e",
        port_env.as_str(),
        "-e",
        serve_env.as_str(),
        "sh",
        "-c",
        wrapper.as_str(),
    ];
    let out = shelbi_ssh::run(&host, argv.iter().copied()).map_err(Error::Io)?;
    if !out.status.success() {
        return Err(Error::Other(format!(
            "split-window for server pane on `{}` failed: {}",
            params.workspace.name,
            String::from_utf8_lossy(&out.stderr).trim(),
        )));
    }
    let pane_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if pane_id.is_empty() {
        return Err(Error::Other(
            "tmux returned an empty pane id from split-window for the server pane".into(),
        ));
    }

    let record = ServerPaneRecord {
        pane_id,
        port: params.port,
        url: server_url(machine, params.port),
        task_id: params.task_id.to_string(),
    };
    shelbi_state::save_server_record(&params.workspace.name, &record)?;
    Ok(record)
}

/// The URL a human opens to reach the server. Localhost for a local machine
/// (spec §15: bind localhost by default); the machine host for a remote one
/// (used only for the advertised URL — remote spawn is unsupported for now).
fn server_url(machine: &Machine, port: u16) -> String {
    match machine.host() {
        Host::Local => format!("http://localhost:{port}"),
        Host::Ssh { host } => format!("http://{host}:{port}"),
    }
}

/// Is the tracked server pane still alive? Asks tmux whether a pane with the
/// recorded id currently exists anywhere on the server. A dead pane (the
/// serve process exited and tmux closed it) reports `false`.
pub fn server_pane_alive(host: &Host, pane_id: &str) -> Result<bool> {
    // `list-panes -a` enumerates every pane across all sessions; the pane id
    // is globally unique and rename-stable, so membership is an exact match.
    let out = shelbi_ssh::run(host, ["tmux", "list-panes", "-a", "-F", "#{pane_id}"])
        .map_err(Error::Io)?;
    if !out.status.success() {
        return Ok(false);
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    Ok(stdout.lines().any(|p| p.trim() == pane_id))
}

/// Kill a review workspace's server pane, if it has one, and clear its
/// record. Idempotent: a workspace with no recorded server pane is a clean
/// no-op, and a record whose pane is already gone is still cleared.
///
/// Marks a server-scoped expected-teardown before killing so the wrapper's
/// exit path suppresses the `server_alive=false` event — otherwise every
/// intentional teardown (re-dispatch, `workspace stop`, reap) would spam a
/// spurious server-death line, mirroring the agent pane's suppression
/// (bug-workspace-pane-alive-false-sighup-fires-spuriously-right-after-dispatch).
pub fn kill_server_pane(host: &Host, workspace: &str) -> Result<()> {
    let Some(record) = shelbi_state::load_server_record(workspace)? else {
        return Ok(());
    };

    // Suppress the wrapper's death event for this intentional kill.
    let _ = shelbi_state::mark_expected_teardown(&shelbi_state::server_teardown_key(workspace));
    let _ = shelbi_ssh::run(host, ["tmux", "kill-pane", "-t", record.pane_id.as_str()])
        .map_err(Error::Io);
    shelbi_state::clear_server_record(workspace)?;
    Ok(())
}

/// Outcome of a reaper pass over one workspace — returned so callers (the
/// poller, tests) can log or assert on what happened without re-deriving it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReapOutcome {
    /// No server record for this workspace — nothing to do.
    NoServer,
    /// The server's task is still active on the workspace; left running.
    StillActive { task_id: String },
    /// A leaked server (no active task) was killed and its record cleared.
    Reaped { task_id: String, port: u16 },
    /// The pane had already died on its own; the stale record was cleared.
    ClearedDeadRecord { task_id: String },
}

/// Reap a review workspace's server pane if it has leaked: a live server pane
/// with no active (in-progress / review) task still assigned to the workspace
/// is a leaked port blocking re-dispatch (spec §10). Also garbage-collects a
/// record whose pane already died so the sidecar doesn't linger.
///
/// "Active" is defined by task state, not by a `role: review` field, so this
/// works before the review-dispatch phase lands: any in-progress or review
/// task assigned to the workspace keeps its server alive.
pub fn reap_server_pane_if_leaked(project: &Project, workspace: &WorkspaceSpec) -> Result<ReapOutcome> {
    let Some(record) = shelbi_state::load_server_record(&workspace.name)?
    else {
        return Ok(ReapOutcome::NoServer);
    };
    let machine = project
        .machine(&workspace.machine)
        .ok_or_else(|| Error::UnknownMachine(workspace.machine.clone()))?;
    let host = machine.host();

    // Pane already dead (server crashed / exited): the wrapper has already
    // emitted its event; just clear the stale record so the workspace is
    // believed free again.
    if !server_pane_alive(&host, &record.pane_id)? {
        shelbi_state::clear_server_record(&workspace.name)?;
        return Ok(ReapOutcome::ClearedDeadRecord {
            task_id: record.task_id,
        });
    }

    if workspace_has_active_task(project, &workspace.name)? {
        return Ok(ReapOutcome::StillActive {
            task_id: record.task_id,
        });
    }

    // Leaked: live pane, no active task. Kill it and free the port.
    kill_server_pane(&host, &workspace.name)?;
    Ok(ReapOutcome::Reaped {
        task_id: record.task_id,
        port: record.port,
    })
}

/// Is any in-progress or review-column task still assigned to `workspace`?
/// We accept any active task on the workspace (not just the server's own
/// `task_id`), so a follow-up task loaded onto the same review slot doesn't
/// get its server reaped out from under it.
fn workspace_has_active_task(project: &Project, workspace: &str) -> Result<bool> {
    for column in [Column::InProgress, Column::Review] {
        let tasks = shelbi_state::list_column(&project.name, column)?;
        if tasks
            .iter()
            .any(|tf| tf.task.assigned_to.as_deref() == Some(workspace))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Default port for the ready-probe connect/read timeout knobs. A dev server
/// that's up answers in well under this; a port that isn't bound refuses
/// immediately (no wait). Kept modest so the probe loop stays responsive.
const PROBE_IO_TIMEOUT: Duration = Duration::from_secs(2);

/// How often [`wait_for_http_ready`] retries while waiting for the server to
/// come up. The serve command is booting (installing? compiling?) so early
/// connects refuse; a short poll keeps the "ready" latency low without busy-
/// spinning.
const PROBE_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Poll `http://localhost:<port><path>` until it answers with an accepted
/// status (2xx/3xx by default — the server is bound and routing) or `timeout`
/// elapses. Returns the first accepted status code, or `None` on timeout.
///
/// This is the ready-probe that gates the review agent's "ready" signal (spec
/// §10): a live pane means the process launched, but only an HTTP answer means
/// the URL actually serves. Implemented over a raw TCP + minimal HTTP/1.0 GET
/// so shelbi takes on no HTTP-client dependency for a one-line probe.
pub fn wait_for_http_ready(port: u16, path: &str, timeout: Duration) -> Option<u16> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(code) = probe_http_once(port, path) {
            if is_ready_status(code) {
                return Some(code);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(PROBE_POLL_INTERVAL);
    }
}

/// A status the ready-probe treats as "serving": any 2xx or 3xx. A 4xx/5xx
/// means the framework is up but the root path errored — still "bound," but we
/// don't want to declare ready on a broken app, so we keep polling until the
/// window closes (the caller then reports not-ready).
fn is_ready_status(code: u16) -> bool {
    (200..400).contains(&code)
}

/// One ready-probe attempt: TCP-connect to localhost, send a minimal HTTP/1.0
/// GET, parse the status code off the first response line. `None` on any
/// failure (connection refused because nothing's bound yet, timeout, or a
/// non-HTTP reply).
fn probe_http_once(port: u16, path: &str) -> Option<u16> {
    let addr = ("127.0.0.1", port)
        .to_socket_addrs()
        .ok()?
        .next()?;
    let mut stream = TcpStream::connect_timeout(&addr, PROBE_IO_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(PROBE_IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(PROBE_IO_TIMEOUT)).ok()?;
    let path = if path.is_empty() { "/" } else { path };
    let req = format!(
        "GET {path} HTTP/1.0\r\nHost: localhost\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).ok()?;
    let mut buf = Vec::with_capacity(256);
    // We only need the status line; cap the read so a chunked/streaming body
    // can't make us block for the whole IO timeout.
    let mut chunk = [0u8; 256];
    while buf.len() < 256 {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.windows(2).any(|w| w == b"\r\n") {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    parse_http_status(&buf)
}

/// Parse the numeric status code out of an HTTP status line
/// (`HTTP/1.1 200 OK`). Returns `None` if the bytes don't start with a valid
/// HTTP status line.
fn parse_http_status(bytes: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(bytes).ok()?;
    let first = text.lines().next()?;
    let mut parts = first.split_whitespace();
    let version = parts.next()?;
    if !version.starts_with("HTTP/") {
        return None;
    }
    parts.next()?.parse::<u16>().ok()
}

/// Environment variable naming the port the server pane binds. Read by the
/// `--as-server-pane` wrapper and exported to the serve subprocess.
pub const ENV_PORT: &str = "PORT";

/// Environment variable carrying the serve command the server pane runs.
pub const ENV_SERVE_CMD: &str = "SHELBI_SERVE_CMD";

/// Build the `sh -c`-suitable string that re-enters the shelbi binary as the
/// server pane's lifecycle wrapper. Kept here (not in the CLI crate) so the
/// spawn path and the wrapper agree on the exact invocation shape, mirroring
/// [`crate::workspace`]'s agent-pane wrapper.
pub fn server_wrapper_invocation(shelbi_bin: &str, project: &str, workspace: &str) -> String {
    format!(
        "{bin} --project {proj} open {ws} --as-server-pane",
        bin = shelbi_agent::shell_escape(shelbi_bin),
        proj = shelbi_agent::shell_escape(project),
        ws = shelbi_agent::shell_escape(workspace),
    )
}

/// Absolute path to the running `shelbi` binary, so the wrapper invocation is
/// anchored to this build. Mirrors the module-local helper in
/// [`crate::workspace`].
fn current_exe_string() -> Result<String> {
    Ok(std::env::current_exe()
        .map_err(Error::Io)?
        .to_string_lossy()
        .into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_http_status_reads_code_off_status_line() {
        assert_eq!(parse_http_status(b"HTTP/1.1 200 OK\r\n"), Some(200));
        assert_eq!(parse_http_status(b"HTTP/1.0 404 Not Found\r\n"), Some(404));
        assert_eq!(parse_http_status(b"HTTP/1.1 301 Moved\r\n"), Some(301));
    }

    #[test]
    fn parse_http_status_rejects_non_http() {
        assert_eq!(parse_http_status(b"garbage\r\n"), None);
        assert_eq!(parse_http_status(b""), None);
        assert_eq!(parse_http_status(b"HTTP/1.1 notanumber\r\n"), None);
    }

    #[test]
    fn is_ready_status_accepts_2xx_3xx_only() {
        assert!(is_ready_status(200));
        assert!(is_ready_status(204));
        assert!(is_ready_status(301));
        assert!(is_ready_status(399));
        assert!(!is_ready_status(404));
        assert!(!is_ready_status(500));
        assert!(!is_ready_status(199));
    }

    #[test]
    fn server_wrapper_invocation_quotes_each_segment() {
        let s = server_wrapper_invocation("/usr/local/bin/shelbi", "my project", "review-1");
        assert!(s.contains("--project 'my project'"), "got: {s}");
        assert!(s.ends_with("open review-1 --as-server-pane"), "got: {s}");
    }

    #[test]
    fn server_wrapper_invocation_simple_names_skip_quoting() {
        let s = server_wrapper_invocation("shelbi", "demo", "review-1");
        assert_eq!(s, "shelbi --project demo open review-1 --as-server-pane");
    }

    /// A real dev server returns 200 on its port. Stand up a one-shot TCP
    /// listener that speaks a minimal HTTP 200 and confirm the probe reads it.
    #[test]
    fn wait_for_http_ready_reads_200_from_a_live_listener() {
        use std::net::TcpListener;
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                // Drain the request line enough that the client's write
                // doesn't RST, then answer 200.
                let mut buf = [0u8; 128];
                let _ = stream.read(&mut buf);
                let _ = stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n");
            }
        });
        let code = wait_for_http_ready(port, "/", Duration::from_secs(3));
        assert_eq!(code, Some(200));
        let _ = server.join();
    }

    /// Nothing bound to the port → connect refuses → the probe times out and
    /// reports not-ready rather than hanging.
    #[test]
    fn wait_for_http_ready_times_out_when_nothing_listens() {
        // Bind then immediately drop to get a port nothing is listening on.
        let port = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            l.local_addr().unwrap().port()
        };
        let started = Instant::now();
        let code = wait_for_http_ready(port, "/", Duration::from_millis(300));
        assert_eq!(code, None);
        // The loop honored the deadline instead of spinning forever.
        assert!(started.elapsed() < Duration::from_secs(3), "probe overran its window");
    }

    #[test]
    fn server_url_is_localhost_for_a_local_machine() {
        let machine = local_machine(std::path::Path::new("/tmp/anywhere"));
        assert_eq!(server_url(&machine, 3000), "http://localhost:3000");
    }

    // --- Reaper decision + record lifecycle (no tmux needed) --------------

    /// A workspace with no server record is a clean no-op for both the
    /// reaper and the killer — the two idempotency guarantees teardown and
    /// re-dispatch lean on.
    #[test]
    fn reap_and_kill_are_noops_without_a_record() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home("noserver");
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project(&home, "review-1");
        let ws = &project.workspaces[0];
        assert_eq!(
            reap_server_pane_if_leaked(&project, ws).unwrap(),
            ReapOutcome::NoServer
        );
        // Killing a workspace that owns no server pane must not error.
        kill_server_pane(&Host::Local, &ws.name).unwrap();

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// A record whose pane has already died (the server crashed / exited on
    /// its own) is garbage-collected by the reaper: no active task keeps it,
    /// the pane isn't alive, so the stale sidecar is cleared. A bogus pane id
    /// stands in for a dead pane — `tmux list-panes` never contains it (and
    /// reports failure when there's no server at all), so liveness is false
    /// either way.
    #[test]
    fn reaper_clears_a_record_whose_pane_is_already_dead() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home("deadrec");
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project(&home, "review-1");
        let ws = &project.workspaces[0];
        shelbi_state::save_server_record(
            &ws.name,
            &ServerPaneRecord {
                pane_id: "%no-such-pane-999999".into(),
                port: 3000,
                url: "http://localhost:3000".into(),
                task_id: "gone-task".into(),
            },
        )
        .unwrap();

        let outcome = reap_server_pane_if_leaked(&project, ws).unwrap();
        assert_eq!(
            outcome,
            ReapOutcome::ClearedDeadRecord {
                task_id: "gone-task".into()
            }
        );
        // Record is gone → the hub no longer believes the port is bound.
        assert!(shelbi_state::load_server_record(&ws.name).unwrap().is_none());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    /// `workspace_has_active_task` — the reaper's "is this a leak?" predicate
    /// — is true for an in-progress OR review task assigned to the workspace,
    /// and false once the task has moved on (done / reassigned / gone).
    #[test]
    fn workspace_has_active_task_tracks_in_progress_and_review() {
        let _g = crate::test_lock::acquire();
        let home = fresh_home("activetask");
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project(&home, "review-1");

        // Nothing assigned yet → not active.
        assert!(!workspace_has_active_task(&project, "review-1").unwrap());

        // In-progress task assigned → active.
        let mut t = make_task("feat-x", Column::InProgress);
        t.assigned_to = Some("review-1".into());
        shelbi_state::save_task(&project.name, &t, "").unwrap();
        assert!(workspace_has_active_task(&project, "review-1").unwrap());

        // Move it to Review (still on the review workspace) → still active:
        // a loaded-for-review task must keep its server alive.
        t.column = Column::Review;
        shelbi_state::save_task(&project.name, &t, "").unwrap();
        assert!(workspace_has_active_task(&project, "review-1").unwrap());

        // Accepted → Done: no longer active, so its server may be reaped.
        t.column = Column::Done;
        shelbi_state::save_task(&project.name, &t, "").unwrap();
        assert!(!workspace_has_active_task(&project, "review-1").unwrap());

        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // --- tmux-gated integration: real pane liveness / kill / reap ---------

    fn tmux_available() -> bool {
        std::process::Command::new("tmux")
            .arg("-V")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    fn tmux(args: &[&str]) -> std::process::Output {
        std::process::Command::new("tmux").args(args).output().unwrap()
    }

    /// End-to-end over a live local tmux server: a manually-created split
    /// pane stands in for the server pane (decoupling this from the
    /// `--as-server-pane` wrapper binary). Exercises the load-bearing paths:
    /// liveness detection, the reaper killing a leaked pane (no active task),
    /// the reaper leaving a still-active pane alone, and explicit teardown.
    #[test]
    fn server_pane_liveness_reap_and_kill_over_real_tmux() {
        if !tmux_available() {
            eprintln!("skipping: tmux not on PATH");
            return;
        }
        let _g = crate::test_lock::acquire();
        let home = fresh_home("tmux-lifecycle");
        std::env::set_var("SHELBI_HOME", &home);

        let project = fixture_project(&home, "review-1");
        let ws = &project.workspaces[0];
        let addr = workspace_tmux_addr(&project, ws).unwrap();
        let session = addr.session.clone();

        // Fresh session hosting the workspace's agent window.
        let _ = tmux(&["kill-session", "-t", &session]);
        assert!(
            tmux(&["new-session", "-d", "-s", &session, "-n", &ws.name, "sh", "-c", "sleep 60"])
                .status
                .success(),
            "failed to create test session"
        );

        // Helper: split a stand-in server pane into the agent window and
        // record it, as spawn_server_pane would.
        let split_and_record = |task_id: &str| {
            let out = tmux(&[
                "split-window", "-v", "-d", "-t", &addr.target(),
                "-P", "-F", "#{pane_id}", "sh", "-c", "sleep 60",
            ]);
            assert!(out.status.success(), "split failed");
            let pane_id = String::from_utf8_lossy(&out.stdout).trim().to_string();
            shelbi_state::save_server_record(
                &ws.name,
                &ServerPaneRecord {
                    pane_id: pane_id.clone(),
                    port: 3000,
                    url: "http://localhost:3000".into(),
                    task_id: task_id.into(),
                },
            )
            .unwrap();
            pane_id
        };

        // 1. Liveness: a freshly split pane is alive; a bogus id isn't.
        let pane_id = split_and_record("leaked-task");
        assert!(server_pane_alive(&Host::Local, &pane_id).unwrap());
        assert!(!server_pane_alive(&Host::Local, "%bogus").unwrap());

        // 2. Reaper on a leaked pane (no active task) → Reaped + port freed.
        let outcome = reap_server_pane_if_leaked(&project, ws).unwrap();
        assert_eq!(
            outcome,
            ReapOutcome::Reaped { task_id: "leaked-task".into(), port: 3000 }
        );
        assert!(!server_pane_alive(&Host::Local, &pane_id).unwrap());
        assert!(shelbi_state::load_server_record(&ws.name).unwrap().is_none());

        // 3. Reaper leaves a still-active pane alone.
        let pane_id = split_and_record("live-task");
        let mut t = make_task("live-task", Column::InProgress);
        t.assigned_to = Some(ws.name.clone());
        shelbi_state::save_task(&project.name, &t, "").unwrap();
        assert_eq!(
            reap_server_pane_if_leaked(&project, ws).unwrap(),
            ReapOutcome::StillActive { task_id: "live-task".into() }
        );
        assert!(server_pane_alive(&Host::Local, &pane_id).unwrap());

        // 4. Explicit teardown kills the pane and clears the record even
        //    while the task is still active (the re-dispatch / stop path).
        kill_server_pane(&Host::Local, &ws.name).unwrap();
        assert!(!server_pane_alive(&Host::Local, &pane_id).unwrap());
        assert!(shelbi_state::load_server_record(&ws.name).unwrap().is_none());

        let _ = tmux(&["kill-session", "-t", &session]);
        std::env::remove_var("SHELBI_HOME");
        let _ = std::fs::remove_dir_all(&home);
    }

    // --- fixtures ---------------------------------------------------------

    fn fresh_home(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-server-pane-test-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn local_machine(work_dir: &std::path::Path) -> Machine {
        Machine {
            name: "hub".into(),
            kind: shelbi_core::MachineKind::Local,
            work_dir: work_dir.to_path_buf(),
            host: None,
        }
    }

    fn fixture_project(work_dir: &std::path::Path, workspace: &str) -> Project {
        use std::collections::BTreeMap;
        let mut runners = BTreeMap::new();
        runners.insert(
            "claude".to_string(),
            shelbi_core::AgentRunnerSpec {
                command: "claude".into(),
                flags: vec![],
                dialog_signatures: vec![],
            },
        );
        Project {
            name: "demo".into(),
            repo: "git@example:repo.git".into(),
            default_branch: "main".into(),
            config_mode: None,
            machines: vec![local_machine(work_dir)],
            orchestrator: shelbi_core::OrchestratorSpec { runner: "claude".into() },
            agent_runners: runners,
            editor: None,
            github_url: None,
            workspaces: vec![WorkspaceSpec {
                name: workspace.into(),
                machine: "hub".into(),
                runner: "claude".into(),
                // These fixtures exercise the review-server lifecycle, so the
                // slot is a review workspace (drives review_workspace_port).
                role: shelbi_core::model::WorkspaceRole::Review,
            }],
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            contextstore_sync: Vec::new(),
            detected_shapes: Vec::new(),
            git: shelbi_core::GitConfig::default(),
            review: shelbi_core::model::ReviewConfig::default(),
        }
    }

    fn make_task(id: &str, column: Column) -> shelbi_core::Task {
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
}
