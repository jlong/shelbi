//! `shelbi daemon` — hub-side Unix-socket listener for worker → hub
//! messages plus the OS-supervisor install/uninstall/status/restart
//! plumbing that makes the daemon survive crashes and reboots. Phases 1,
//! 2, 4, and 9 of the Worker → Orchestrator Communication feature
//! (see `Plans/worker-orchestrator-communication.md` §5, §6, §8, §9, §13).
//!
//! ## Foreground (`shelbi daemon`, no subcommand)
//!
//! Binds `~/.shelbi/hub.sock` (overridable via `$SHELBI_HUB_SOCK`), reads
//! newline-delimited JSON messages from any number of concurrent clients,
//! and dispatches them by `verb`:
//!
//! - `event` (Phase 1) — body line is timestamped and appended to
//!   `~/.shelbi/events.log`.
//! - `request-clarification` (Phase 9) — emits a `question=… task=…
//!   kind=clarification text=…` event so the orchestrator's tail surfaces
//!   the question alongside every other transition. The reply travels back
//!   on the file-based `<worktree>/.shelbi/messages/<task-id>.log`
//!   channel via `shelbi message --in-response-to`.
//! - `message-pushed` (Phase 9, internal) — emitted by `shelbi message`
//!   after a successful file append; adds the (task, msg) pair to an
//!   in-memory pending map so the daemon can synthesize an `ack=timeout`
//!   event when the worker never confirms delivery.
//! - `message-ack` (Phase 9) — emitted by the worker after it processes a
//!   message; appends an `ack=worker` event and clears the pending entry.
//!
//! Unknown verbs and malformed payloads are logged to stderr and the
//! daemon keeps running — a single bad client must not be able to take
//! the listener down.
//!
//! ## Unacked-message reaper (Phase 9)
//!
//! Spawned at startup, the reaper wakes every second, scans the pending
//! map for `(task, msg)` pairs whose push timestamp is older than the
//! configurable threshold ([`ack_timeout_from_env`]; default 60s,
//! `$SHELBI_ACK_TIMEOUT_SECS` to override), and for each one emits a
//! single `message=… task=… ack=timeout` event before removing the
//! entry. Idempotent on shutdown: the reaper checks the same stop flag
//! the accept loop does, so SIGTERM stops both promptly.
//!
//! Phase 4 additions:
//!   - On startup, walks `~/.shelbi/ssh/` and removes orphaned SSH
//!     ControlMaster socket files (master process died, socket file
//!     leaked). Skips entirely when `~/.shelbi/shelbi.pid` names a
//!     still-running shelbi process — those sockets belong to that
//!     daemon and we don't touch them.
//!   - Records its own PID at `~/.shelbi/shelbi.pid` so the next
//!     start's cleanup can make the same decision.
//!
//! The daemon is stateless across *restarts* with respect to events.log
//! (the durable record). The pending map is in-memory by design — a
//! crash drops it; the orchestrator's view of which messages are
//! outstanding rebuilds organically as `push=ok` events stop pairing
//! with future `ack=worker` lines. We accept this tradeoff because the
//! safety net is "no silent loss", not "perfect delivery accounting".
//!
//! ## Supervision (`shelbi daemon install|uninstall|status|restart`)
//!
//! We don't write our own supervisor — auto-restart, run-at-login, and
//! reboot persistence ride on `launchd` (macOS) or `systemd --user`
//! (Linux). `install` writes the platform-appropriate unit file with
//! absolute paths baked in (so the supervisor doesn't need `$PATH`) and
//! loads it; `uninstall` unloads + removes; `status` summarizes; and
//! `restart` asks the supervisor to bounce the daemon so a freshly
//! installed binary takes effect without losing the auto-restart
//! guarantee.

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

/// launchd `Label` and systemd unit base name. We use a single string for
/// both so the user sees one stable identifier in `launchctl print`,
/// `systemctl --user status`, and any log lines that mention the unit.
const SERVICE_LABEL: &str = "co.32pixels.shelbi";
/// systemd needs the `.service` suffix on most commands. Centralized so a
/// future rename touches one place. `dead_code` allowed because the
/// constant is only referenced from `cfg(target_os = "linux")` arms;
/// macOS builds compile it but never read it.
#[allow(dead_code)]
const SYSTEMD_SERVICE_NAME: &str = "shelbi.service";

/// Environment variable that overrides the default unacked-message ack
/// timeout. Value is parsed as whole seconds; any non-positive or
/// unparseable value falls back to [`DEFAULT_ACK_TIMEOUT`] with a stderr
/// warning so a typo never silently disables the reaper.
const ACK_TIMEOUT_ENV: &str = "SHELBI_ACK_TIMEOUT_SECS";

/// Default time the daemon waits for a worker `message-ack` before
/// synthesizing an `ack=timeout` event. 60s matches the threshold called
/// out in `Plans/worker-orchestrator-communication.md` §9; long enough
/// to outwait a healthy hook/poll round-trip even under load, short
/// enough that the orchestrator notices a wedged worker within the
/// human reaction window.
const DEFAULT_ACK_TIMEOUT: Duration = Duration::from_secs(60);

/// How often the reaper wakes to scan the pending map. One second is
/// short enough that an `ack=timeout` lands within ~1s of crossing the
/// threshold, long enough that an idle daemon spends near-zero CPU.
const REAPER_TICK: Duration = Duration::from_secs(1);

/// In-memory map of pushed-but-unacked messages, keyed by `(task_id,
/// msg_id)` with the push time recorded against [`Instant::now`] at
/// arrival. Shared between the listener (insert on `message-pushed`,
/// remove on `message-ack`) and the reaper (drain on timeout). Wrapped
/// in `Arc<Mutex<…>>` so cheap clones move with each thread.
type PendingMap = HashMap<(String, String), Instant>;

/// Resolve the ack timeout once at daemon start. Env var wins so tests
/// and operators can dial it down without rebuilding; an unparseable
/// value warns and falls back to the default rather than silently
/// disabling the reaper.
fn ack_timeout_from_env() -> Duration {
    match std::env::var(ACK_TIMEOUT_ENV) {
        Ok(raw) => match raw.trim().parse::<u64>() {
            Ok(0) => {
                eprintln!(
                    "shelbi daemon: {ACK_TIMEOUT_ENV}=0 disabled — using default {}s",
                    DEFAULT_ACK_TIMEOUT.as_secs()
                );
                DEFAULT_ACK_TIMEOUT
            }
            Ok(n) => Duration::from_secs(n),
            Err(_) => {
                eprintln!(
                    "shelbi daemon: {ACK_TIMEOUT_ENV}=`{raw}` is not a non-negative integer; \
                     using default {}s",
                    DEFAULT_ACK_TIMEOUT.as_secs()
                );
                DEFAULT_ACK_TIMEOUT
            }
        },
        Err(_) => DEFAULT_ACK_TIMEOUT,
    }
}

/// Per-process daemon state shared across the accept loop, every client
/// handler thread, and the reaper. Holds the pending message map and the
/// resolved ack timeout; both are stamped at startup and never mutated.
/// Clones are cheap — the `Arc` is the only field that needs to move.
#[derive(Clone)]
struct Daemon {
    pending: Arc<Mutex<PendingMap>>,
    ack_timeout: Duration,
}

impl Daemon {
    fn new(ack_timeout: Duration) -> Self {
        Self {
            pending: Arc::new(Mutex::new(PendingMap::new())),
            ack_timeout,
        }
    }
}

/// `shelbi daemon <subcommand>`. The foreground entry runs when no
/// subcommand is supplied — launchd/systemd invoke us with bare
/// `shelbi daemon` and we serve until killed.
#[derive(Debug, clap::Subcommand)]
pub enum DaemonCmd {
    /// (default — also the form launchd/systemd invoke) Bind the hub
    /// socket and accept worker messages in the foreground until killed.
    Run,
    /// Install the platform supervisor unit (launchd plist on macOS,
    /// systemd user service on Linux) so the daemon auto-starts at login
    /// and is restarted on crash. Idempotent — re-running just refreshes
    /// the unit file and reloads it.
    Install,
    /// Stop the daemon and remove the platform supervisor unit.
    Uninstall,
    /// Print a short human-readable status by wrapping `launchctl print`
    /// or `systemctl --user status`.
    Status,
    /// Stop the daemon so the supervisor relaunches it — picks up a
    /// freshly installed binary without losing the auto-restart
    /// guarantee.
    Restart,
}

pub fn run(cmd: Option<DaemonCmd>) -> Result<()> {
    match cmd {
        None | Some(DaemonCmd::Run) => run_foreground(),
        Some(DaemonCmd::Install) => install(),
        Some(DaemonCmd::Uninstall) => uninstall(),
        Some(DaemonCmd::Status) => status(),
        Some(DaemonCmd::Restart) => restart(),
    }
}

/// Foreground entry point. Binds the socket, installs signal handlers, and
/// runs the accept loop until SIGTERM/SIGINT/SIGHUP. Errors from
/// individual clients are swallowed (logged to stderr) so the daemon
/// keeps serving the rest of the fleet.
fn run_foreground() -> Result<()> {
    let sock = shelbi_state::hub_socket_path().map_err(|e| anyhow!(e))?;
    prepare_socket(&sock)?;
    prune_stale_control_masters();

    let listener = UnixListener::bind(&sock)
        .with_context(|| format!("binding hub socket at {}", sock.display()))?;
    fs::set_permissions(&sock, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", sock.display()))?;

    // Record our PID for the next startup's cleanup decision. Written
    // AFTER prepare_socket succeeds and BEFORE the accept loop so a
    // crash mid-startup doesn't leave behind a PID file that points to
    // a process that never actually held the socket.
    let self_pid = std::process::id() as libc::pid_t;
    if let Err(e) = shelbi_state::write_daemon_pid(self_pid) {
        eprintln!("shelbi daemon: failed to write PID file: {e}");
    }

    let daemon = Daemon::new(ack_timeout_from_env());
    eprintln!(
        "shelbi daemon: listening at {} (ack timeout {}s)",
        sock.display(),
        daemon.ack_timeout.as_secs()
    );

    let stop = Arc::new(AtomicBool::new(false));
    install_shutdown_listener(stop.clone(), sock.clone())?;
    spawn_reaper(daemon.clone(), stop.clone());

    for incoming in listener.incoming() {
        // Shutdown wakes us via a self-connect; the resulting accept
        // returns Ok with a stream we never read. Check the flag first
        // and bail before spawning a handler that will see EOF anyway.
        if stop.load(Ordering::SeqCst) {
            break;
        }
        match incoming {
            Ok(stream) => {
                let daemon = daemon.clone();
                thread::spawn(move || handle_client(stream, &daemon));
            }
            Err(e) => {
                eprintln!("shelbi daemon: accept error: {e}");
            }
        }
    }

    let _ = fs::remove_file(&sock);
    // Best-effort: drop the PID file so the next start's cleanup
    // doesn't see us as a (now-dead) live daemon. The read path is
    // resilient to a stale PID anyway — this is just hygiene.
    if let Err(e) = shelbi_state::remove_daemon_pid_file() {
        eprintln!("shelbi daemon: failed to remove PID file: {e}");
    }
    eprintln!("shelbi daemon: stopped");
    Ok(())
}

/// Spawn the unacked-message reaper thread. Wakes every [`REAPER_TICK`],
/// drains pending entries past `ack_timeout`, emits one `ack=timeout`
/// event per drained pair, and exits when the shared stop flag is set
/// (same flag the accept loop watches, so SIGTERM stops both).
fn spawn_reaper(daemon: Daemon, stop: Arc<AtomicBool>) {
    thread::spawn(move || {
        while !stop.load(Ordering::SeqCst) {
            // Sleep in short slices so SIGTERM wakes us within a tick
            // rather than waiting up to REAPER_TICK for the next scan.
            let slice = Duration::from_millis(250);
            let mut waited = Duration::ZERO;
            while waited < REAPER_TICK && !stop.load(Ordering::SeqCst) {
                thread::sleep(slice);
                waited += slice;
            }
            if stop.load(Ordering::SeqCst) {
                break;
            }
            reap_expired(&daemon);
        }
    });
}

/// Drain all `(task, msg)` pairs in the pending map whose push time is
/// older than `daemon.ack_timeout`. For each drained pair emit one
/// `message=<id> task=<id> ack=timeout` event. Holds the map lock only
/// while collecting the expired keys — the event-append IO happens
/// after the lock is released so a slow events.log write never blocks
/// new pushes or acks on the listener side.
fn reap_expired(daemon: &Daemon) {
    let timeout = daemon.ack_timeout;
    let now = Instant::now();
    let expired: Vec<(String, String)> = {
        let mut map = match daemon.pending.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let keys: Vec<(String, String)> = map
            .iter()
            .filter_map(|(k, t)| {
                if now.saturating_duration_since(*t) >= timeout {
                    Some(k.clone())
                } else {
                    None
                }
            })
            .collect();
        for k in &keys {
            map.remove(k);
        }
        keys
    };
    for (task_id, msg_id) in expired {
        if let Err(e) = shelbi_state::append_message_ack_event(&msg_id, &task_id, "timeout") {
            eprintln!("shelbi daemon: failed to record ack=timeout for {msg_id}/{task_id}: {e}");
        }
    }
}

/// Walk `$SHELBI_HOME/ssh/` and unlink orphaned ControlMaster sockets
/// before the new daemon comes up. Skips entirely when the PID file
/// names a live shelbi process — that's another daemon's CMs and they
/// belong to it. Logs the outcome but never fails the daemon start:
/// the cleanup is best-effort hygiene, and a fresh `ssh` call will
/// still rebind a master even if a stale socket lingers.
fn prune_stale_control_masters() {
    let self_pid = std::process::id() as libc::pid_t;
    match shelbi_state::cleanup_stale_control_masters(self_pid) {
        Ok(shelbi_state::CmCleanupOutcome::SkippedAnotherDaemon { pid }) => {
            eprintln!(
                "shelbi daemon: another shelbi process (pid={pid}) holds the CMs; \
                 skipping ControlMaster cleanup"
            );
        }
        Ok(shelbi_state::CmCleanupOutcome::Scanned { removed, kept }) => {
            if removed > 0 || kept > 0 {
                eprintln!(
                    "shelbi daemon: ControlMaster cleanup — removed {removed} orphaned \
                     socket(s), kept {kept} live"
                );
            }
        }
        Err(e) => {
            eprintln!("shelbi daemon: ControlMaster cleanup failed: {e}");
        }
    }
}

/// Make sure the socket parent directory exists with `0700` perms and
/// the socket file itself is free for `bind()`. A leftover socket from a
/// previous run is reclaimed only if no one is currently listening on it;
/// a live daemon at the same path is a hard error so two of us never
/// race on the same file descriptor.
fn prepare_socket(sock: &Path) -> Result<()> {
    if let Some(parent) = sock.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating socket parent {}", parent.display()))?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                .with_context(|| format!("chmod 700 {}", parent.display()))?;
        }
    }

    if sock.exists() {
        // A live peer means another daemon owns this socket — refuse to
        // clobber it. A stale file (ECONNREFUSED / ENOENT on connect)
        // gets removed so we can rebind cleanly.
        match UnixStream::connect(sock) {
            Ok(_) => {
                return Err(anyhow!(
                    "another shelbi daemon is already listening at {} \
                     (delete the socket file if you're sure no daemon is running)",
                    sock.display()
                ));
            }
            Err(_) => {
                fs::remove_file(sock).with_context(|| {
                    format!("removing stale socket at {}", sock.display())
                })?;
            }
        }
    }
    Ok(())
}

/// Catch SIGTERM/SIGINT/SIGHUP on a background thread, flip the stop
/// flag, and wake the blocking `accept()` with a single self-connection
/// so the main loop notices and breaks out.
fn install_shutdown_listener(stop: Arc<AtomicBool>, sock: PathBuf) -> Result<()> {
    let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])
        .context("installing daemon signal handlers")?;
    thread::spawn(move || {
        if let Some(sig) = signals.forever().next() {
            eprintln!("shelbi daemon: received signal {sig}, shutting down");
            stop.store(true, Ordering::SeqCst);
            // Wake the accept loop. The connection itself is unused —
            // it's just a syscall poke so accept() returns instead of
            // blocking on the next client.
            let _ = UnixStream::connect(&sock);
        }
    });
    Ok(())
}

/// One client → one BufReader → newline-delimited JSON. Each line is
/// dispatched independently so a bad line in the middle of a batch
/// doesn't kill the rest. EOF closes the handler cleanly.
fn handle_client(stream: UnixStream, daemon: &Daemon) {
    let reader = BufReader::new(stream);
    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("shelbi daemon: client read error: {e}");
                return;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        if let Err(e) = dispatch(&line, daemon) {
            // Log + continue; one bad message must not take down a
            // multi-message client connection.
            eprintln!("shelbi daemon: rejected message: {e}: {line}");
        }
    }
}

#[derive(Debug, Deserialize)]
struct Message {
    verb: String,
    /// Project name the message belongs to. Currently audit-only on the
    /// daemon side — the per-project routing surface lives in the
    /// orchestrator, not here — but every verb carries it so future
    /// per-project consumers don't have to re-version the wire format.
    #[serde(default)]
    project: Option<String>,
    /// Body of an `event` message. Required for `event`; ignored for
    /// every other verb.
    #[serde(default)]
    line: Option<String>,
    /// Task this message references. Required for `request-clarification`,
    /// `message-ack`, and `message-pushed`; ignored for `event`.
    #[serde(default)]
    task_id: Option<String>,
    /// Opaque question id minted by the worker — echoed back in the
    /// orchestrator's reply (`shelbi message ... --in-response-to`) so
    /// multiple in-flight clarifications correlate cleanly. Required for
    /// `request-clarification`.
    #[serde(default)]
    question_id: Option<String>,
    /// Free-form question text the worker wants the user to answer.
    /// Truncated + folded before it lands in `events.log`. Required for
    /// `request-clarification`.
    #[serde(default)]
    question: Option<String>,
    /// Optional short excerpt the worker thinks is useful context for
    /// the question — currently emitted only as a debug-trace breadcrumb
    /// since the full question/context exchange lives in the message log,
    /// not the events stream.
    #[serde(default)]
    context: Option<String>,
    /// Opaque message id from `shelbi message`. Required for
    /// `message-ack` (worker referencing the message it processed) and
    /// `message-pushed` (the CLI announcing the push to the daemon).
    #[serde(default)]
    msg_id: Option<String>,
}

fn dispatch(raw: &str, daemon: &Daemon) -> Result<()> {
    let msg: Message = serde_json::from_str(raw).context("invalid JSON payload")?;
    match msg.verb.as_str() {
        "event" => handle_event(&msg),
        "request-clarification" => handle_request_clarification(&msg),
        "message-pushed" => handle_message_pushed(&msg, daemon),
        "message-ack" => handle_message_ack(&msg, daemon),
        other => Err(anyhow!("unknown verb `{other}`")),
    }
}

fn handle_event(msg: &Message) -> Result<()> {
    let body = msg
        .line
        .as_deref()
        .ok_or_else(|| anyhow!("event message missing `line` field"))?;
    if body.is_empty() {
        return Err(anyhow!("event message has empty `line` field"));
    }
    // One event = one line. Embedded newlines would tear the body across
    // multiple records (the second of which would be unparseable) so we
    // reject them outright rather than silently mangling the payload.
    if body.contains('\n') || body.contains('\r') {
        return Err(anyhow!("event `line` may not contain newlines"));
    }
    shelbi_state::append_external_event(body).map_err(|e| anyhow!(e))?;
    if let Some(project) = msg.project.as_deref() {
        tracing::debug!(project, "shelbi daemon: appended event");
    }
    Ok(())
}

/// Worker-side clarification request. Required fields are `task_id`,
/// `question_id`, and `question`; `context` is optional and currently
/// emitted only as a debug breadcrumb since the full body lives in the
/// per-task message log, not the events stream.
fn handle_request_clarification(msg: &Message) -> Result<()> {
    let task_id = required(msg.task_id.as_deref(), "request-clarification", "task_id")?;
    let question_id = required(
        msg.question_id.as_deref(),
        "request-clarification",
        "question_id",
    )?;
    let question = required(msg.question.as_deref(), "request-clarification", "question")?;
    shelbi_state::append_clarification_event(question_id, task_id, question)
        .map_err(|e| anyhow!(e))?;
    if msg.context.is_some() || msg.project.is_some() {
        tracing::debug!(
            project = msg.project.as_deref().unwrap_or("?"),
            task = task_id,
            question = question_id,
            has_context = msg.context.is_some(),
            "shelbi daemon: recorded clarification"
        );
    }
    Ok(())
}

/// Internal verb: `shelbi message` calls into the daemon after a
/// successful file append so the daemon can start the ack-timeout clock.
/// No event is emitted here — the CLI already wrote `push=ok` to
/// `events.log` via [`shelbi_state::append_message_event`]; we just
/// arm the in-memory timer that will fire `ack=timeout` if the worker
/// never confirms. A repeat `message-pushed` for the same (task, msg)
/// pair refreshes the timer rather than erroring; the CLI shouldn't
/// send duplicates but a manual retry under operator control should
/// reset the clock instead of failing.
fn handle_message_pushed(msg: &Message, daemon: &Daemon) -> Result<()> {
    let task_id = required(msg.task_id.as_deref(), "message-pushed", "task_id")?;
    let msg_id = required(msg.msg_id.as_deref(), "message-pushed", "msg_id")?;
    let mut map = daemon
        .pending
        .lock()
        .map_err(|_| anyhow!("pending map poisoned"))?;
    map.insert((task_id.to_string(), msg_id.to_string()), Instant::now());
    Ok(())
}

/// Worker-side delivery confirmation. Clears the matching pending entry
/// (no-op if the timer already expired and the reaper claimed it) and
/// emits a single `message=<id> task=<id> ack=worker` event so the
/// orchestrator's tail sees delivery on the same stream that carried
/// the original `push=ok` line.
fn handle_message_ack(msg: &Message, daemon: &Daemon) -> Result<()> {
    let task_id = required(msg.task_id.as_deref(), "message-ack", "task_id")?;
    let msg_id = required(msg.msg_id.as_deref(), "message-ack", "msg_id")?;
    {
        let mut map = daemon
            .pending
            .lock()
            .map_err(|_| anyhow!("pending map poisoned"))?;
        map.remove(&(task_id.to_string(), msg_id.to_string()));
    }
    shelbi_state::append_message_ack_event(msg_id, task_id, "worker").map_err(|e| anyhow!(e))?;
    Ok(())
}

/// Reject the request with a uniform "missing required field" error
/// when a verb-specific field arrives unset (or set to an empty
/// string). Centralized so the wire-protocol error messages stay
/// consistent across verbs — every line in the daemon log reads the
/// same way regardless of which verb tripped.
fn required<'a>(field: Option<&'a str>, verb: &str, name: &str) -> Result<&'a str> {
    match field {
        Some(s) if !s.is_empty() => Ok(s),
        _ => Err(anyhow!("{verb} message missing `{name}` field")),
    }
}

// ---------------------------------------------------------------------------
// Supervision: install / uninstall / status / restart
// ---------------------------------------------------------------------------
//
// Each verb has one entry point that dispatches by `cfg(target_os = …)`. A
// catch-all stub on unknown OSes prints a clear "not supported" line and
// returns Ok so scripts (notably `scripts/install.sh`) don't fail the
// install just because the host isn't macOS or Linux.

#[cfg(target_os = "macos")]
fn install() -> Result<()> {
    install_launchd()
}
#[cfg(target_os = "linux")]
fn install() -> Result<()> {
    install_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn install() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall() -> Result<()> {
    uninstall_launchd()
}
#[cfg(target_os = "linux")]
fn uninstall() -> Result<()> {
    uninstall_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn uninstall() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(target_os = "macos")]
fn status() -> Result<()> {
    status_launchd()
}
#[cfg(target_os = "linux")]
fn status() -> Result<()> {
    status_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn status() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(target_os = "macos")]
fn restart() -> Result<()> {
    restart_launchd()
}
#[cfg(target_os = "linux")]
fn restart() -> Result<()> {
    restart_systemd()
}
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn restart() -> Result<()> {
    unsupported_warning();
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn unsupported_warning() {
    eprintln!(
        "shelbi daemon: daemon supervision not supported on {}; \
         run `shelbi daemon` manually under your supervisor of choice",
        std::env::consts::OS
    );
}

/// Resolve the absolute path to the currently-running `shelbi` binary so
/// the supervisor doesn't depend on `$PATH` being set in the launchd /
/// systemd execution environment.
fn current_binary() -> Result<PathBuf> {
    std::env::current_exe().context("resolving current shelbi binary path")
}

/// Resolve the shelbi state root we want the supervised daemon to see.
/// Baking this into the unit file (as `SHELBI_ROOT=…`) means the user can
/// later move or rebuild the binary without the supervised daemon
/// silently flipping to a different home directory.
fn baked_state_root() -> Result<PathBuf> {
    shelbi_state::shelbi_home().map_err(|e| anyhow!(e))
}

/// `~/.shelbi/logs/`, the home for `daemon.out`, `daemon.err`, `daemon.log`.
fn ensure_log_dir() -> Result<PathBuf> {
    let dir = baked_state_root()?.join("logs");
    fs::create_dir_all(&dir)
        .with_context(|| format!("creating daemon log dir {}", dir.display()))?;
    Ok(dir)
}

// --------------------------- macOS / launchd ------------------------------

#[cfg(target_os = "macos")]
fn install_launchd() -> Result<()> {
    let plist_path = launch_agent_plist_path()?;
    if let Some(parent) = plist_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating LaunchAgents dir {}", parent.display()))?;
    }
    ensure_log_dir()?;

    let plist = render_launchd_plist(&LaunchdInputs::resolve()?);
    fs::write(&plist_path, plist)
        .with_context(|| format!("writing launchd plist {}", plist_path.display()))?;

    // Idempotent reinstall: bootout the old instance (if any) before
    // bootstrapping the freshly-written plist. bootout returns non-zero
    // when nothing is loaded; that's fine — swallow it.
    let uid = current_uid();
    let _ = Command::new("launchctl")
        .args(["bootout", &gui_target(uid)])
        .status();

    let status = Command::new("launchctl")
        .args(["bootstrap", &gui_domain(uid)])
        .arg(&plist_path)
        .status()
        .context("invoking launchctl bootstrap")?;
    if !status.success() {
        bail!(
            "launchctl bootstrap failed (exit {:?}) — see `launchctl print {}` for details",
            status.code(),
            gui_target(uid)
        );
    }

    println!("✓ installed launchd agent at {}", plist_path.display());
    println!("  label: {SERVICE_LABEL}");
    println!("  daemon should now be running — verify with `shelbi daemon status`");
    Ok(())
}

#[cfg(target_os = "macos")]
fn uninstall_launchd() -> Result<()> {
    let uid = current_uid();
    let _ = Command::new("launchctl")
        .args(["bootout", &gui_target(uid)])
        .status();

    let plist_path = launch_agent_plist_path()?;
    if plist_path.exists() {
        fs::remove_file(&plist_path)
            .with_context(|| format!("removing {}", plist_path.display()))?;
        println!("✓ removed {}", plist_path.display());
    } else {
        println!("(no plist at {})", plist_path.display());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn status_launchd() -> Result<()> {
    let uid = current_uid();
    let target = gui_target(uid);
    let out = Command::new("launchctl")
        .args(["print", &target])
        .output()
        .context("invoking launchctl print")?;
    if !out.status.success() {
        println!("shelbi daemon: not installed (or not loaded)");
        println!("  install with: shelbi daemon install");
        return Ok(());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let pid = parse_launchctl_field(&text, "pid").unwrap_or_else(|| "(none)".into());
    let state = parse_launchctl_field(&text, "state").unwrap_or_else(|| "unknown".into());
    let last_exit = parse_launchctl_field(&text, "last exit code");
    let runs = parse_launchctl_field(&text, "runs");

    println!("shelbi daemon ({SERVICE_LABEL})");
    println!("  state:           {state}");
    println!("  pid:             {pid}");
    if let Some(runs) = runs {
        println!("  total launches:  {runs}");
    }
    if let Some(le) = last_exit {
        println!("  last exit code:  {le}");
    }
    println!("  plist:           {}", launch_agent_plist_path()?.display());
    Ok(())
}

#[cfg(target_os = "macos")]
fn restart_launchd() -> Result<()> {
    let uid = current_uid();
    let target = gui_target(uid);
    let status = Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .status()
        .context("invoking launchctl kickstart")?;
    if !status.success() {
        bail!(
            "launchctl kickstart {} failed — is the agent installed? \
             (run `shelbi daemon install`)",
            target
        );
    }
    println!("✓ kickstarted {target}");
    Ok(())
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    // SAFETY: `getuid()` is a thread-safe POSIX call with no inputs.
    unsafe { libc::getuid() }
}

#[cfg(target_os = "macos")]
fn gui_domain(uid: u32) -> String {
    format!("gui/{uid}")
}

#[cfg(target_os = "macos")]
fn gui_target(uid: u32) -> String {
    format!("gui/{uid}/{SERVICE_LABEL}")
}

#[cfg(target_os = "macos")]
fn launch_agent_plist_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolving $HOME for LaunchAgents path")?;
    Ok(home
        .join("Library/LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

/// Inputs that go into the rendered plist. Bundled into a struct so the
/// renderer is pure (testable without touching the filesystem).
struct LaunchdInputs {
    binary: PathBuf,
    state_root: PathBuf,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

impl LaunchdInputs {
    fn resolve() -> Result<Self> {
        let state_root = baked_state_root()?;
        let log_dir = state_root.join("logs");
        Ok(Self {
            binary: current_binary()?,
            stdout_path: log_dir.join("daemon.out"),
            stderr_path: log_dir.join("daemon.err"),
            state_root,
        })
    }
}

fn render_launchd_plist(inputs: &LaunchdInputs) -> String {
    let exe = xml_escape(&inputs.binary.to_string_lossy());
    let root = xml_escape(&inputs.state_root.to_string_lossy());
    let out = xml_escape(&inputs.stdout_path.to_string_lossy());
    let err = xml_escape(&inputs.stderr_path.to_string_lossy());
    // ThrottleInterval=1 matches the systemd `RestartSec=1s` knob — both
    // platforms aim for a sub-second relaunch on crash so worker writes
    // resume promptly without hammering the OS in a crash loop.
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{SERVICE_LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>daemon</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>SHELBI_ROOT</key>
        <string>{root}</string>
    </dict>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ThrottleInterval</key>
    <integer>1</integer>
    <key>StandardOutPath</key>
    <string>{out}</string>
    <key>StandardErrorPath</key>
    <string>{err}</string>
</dict>
</plist>
"#
    )
}

/// Pull a `name = value` pair out of `launchctl print` output. The
/// command emits indented `key = value;` lines per state — we grab the
/// first occurrence of the requested key.
#[cfg(target_os = "macos")]
fn parse_launchctl_field(text: &str, key: &str) -> Option<String> {
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(&format!("{key} = ")) {
            return Some(rest.trim_end_matches(';').trim().to_string());
        }
    }
    None
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

// --------------------------- Linux / systemd ------------------------------

#[cfg(target_os = "linux")]
fn install_systemd() -> Result<()> {
    let unit_path = systemd_unit_path()?;
    if let Some(parent) = unit_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("creating systemd user unit dir {}", parent.display()))?;
    }
    ensure_log_dir()?;

    let unit = render_systemd_unit(&SystemdInputs::resolve()?);
    fs::write(&unit_path, unit)
        .with_context(|| format!("writing systemd unit {}", unit_path.display()))?;

    run_systemctl(&["daemon-reload"])?;
    run_systemctl(&["enable", "--now", SYSTEMD_SERVICE_NAME])?;

    println!("✓ installed systemd user unit at {}", unit_path.display());
    println!("  daemon should now be running — verify with `shelbi daemon status`");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_systemd() -> Result<()> {
    // disable --now stops the running unit and removes the wants/links.
    // Tolerate the error path (unit may not be loaded) so uninstall stays
    // idempotent even after a manual `rm`.
    let _ = run_systemctl(&["disable", "--now", SYSTEMD_SERVICE_NAME]);

    let unit_path = systemd_unit_path()?;
    if unit_path.exists() {
        fs::remove_file(&unit_path)
            .with_context(|| format!("removing {}", unit_path.display()))?;
        println!("✓ removed {}", unit_path.display());
    } else {
        println!("(no unit at {})", unit_path.display());
    }
    let _ = run_systemctl(&["daemon-reload"]);
    Ok(())
}

#[cfg(target_os = "linux")]
fn status_systemd() -> Result<()> {
    let out = Command::new("systemctl")
        .args(["--user", "status", SYSTEMD_SERVICE_NAME, "--no-pager"])
        .output()
        .context("invoking systemctl --user status")?;
    let text = String::from_utf8_lossy(&out.stdout);
    if text.trim().is_empty() {
        println!("shelbi daemon: not installed (or systemctl returned nothing)");
        println!("  install with: shelbi daemon install");
        return Ok(());
    }
    // systemctl's first ~6 lines are the header block (loaded/active/pid/
    // tasks/memory) — enough for a quick read without dumping the journal.
    for line in text.lines().take(6) {
        println!("{line}");
    }
    println!("  unit: {}", systemd_unit_path()?.display());
    Ok(())
}

#[cfg(target_os = "linux")]
fn restart_systemd() -> Result<()> {
    run_systemctl(&["restart", SYSTEMD_SERVICE_NAME])?;
    println!("✓ restarted {SYSTEMD_SERVICE_NAME}");
    Ok(())
}

#[cfg(target_os = "linux")]
fn systemd_unit_path() -> Result<PathBuf> {
    let home = dirs::home_dir().context("resolving $HOME for systemd unit path")?;
    Ok(home
        .join(".config/systemd/user")
        .join(SYSTEMD_SERVICE_NAME))
}

#[cfg(target_os = "linux")]
fn run_systemctl(args: &[&str]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("invoking systemctl --user")?;
    if !status.success() {
        bail!(
            "systemctl --user {} failed (exit {:?})",
            args.join(" "),
            status.code()
        );
    }
    Ok(())
}

/// Inputs that go into the rendered systemd unit. Bundled into a struct
/// so the renderer is pure (testable without touching the filesystem).
/// `dead_code` allowed because production callers live behind
/// `cfg(target_os = "linux")` — tests construct it directly on every
/// platform.
#[allow(dead_code)]
struct SystemdInputs {
    binary: PathBuf,
    state_root: PathBuf,
    log_path: PathBuf,
}

impl SystemdInputs {
    #[cfg(target_os = "linux")]
    fn resolve() -> Result<Self> {
        let state_root = baked_state_root()?;
        Ok(Self {
            binary: current_binary()?,
            log_path: state_root.join("logs/daemon.log"),
            state_root,
        })
    }
}

#[allow(dead_code)]
fn render_systemd_unit(inputs: &SystemdInputs) -> String {
    // Restart=always + RestartSec=1s mirrors launchd's KeepAlive +
    // ThrottleInterval=1: sub-second relaunch on any exit path. The
    // append: prefix on StandardOutput/Error keeps the log file alive
    // across restarts so the user can `tail -f` once and never re-open.
    format!(
        "[Unit]
Description=Shelbi hub daemon
Documentation=https://github.com/32pixelsco/shelbi
After=default.target

[Service]
Type=simple
ExecStart={exe} daemon
Environment=SHELBI_ROOT={root}
Restart=always
RestartSec=1s
StandardOutput=append:{log}
StandardError=append:{log}

[Install]
WantedBy=default.target
",
        exe = inputs.binary.display(),
        root = inputs.state_root.display(),
        log = inputs.log_path.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_daemon() -> Daemon {
        // Tests that exercise the timeout branch override `ack_timeout`
        // locally; everything else uses the production default so the
        // unit tests reflect real config.
        Daemon::new(DEFAULT_ACK_TIMEOUT)
    }

    #[test]
    fn dispatch_rejects_malformed_json() {
        let err = dispatch("not json", &test_daemon()).unwrap_err();
        assert!(err.to_string().contains("invalid JSON"), "{err}");
    }

    #[test]
    fn dispatch_rejects_unknown_verb() {
        let err = dispatch(
            r#"{"verb":"task-claim","project":"shelbi","line":"x=1"}"#,
            &test_daemon(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("unknown verb"), "{err}");
    }

    #[test]
    fn dispatch_event_requires_line_field() {
        let err = dispatch(r#"{"verb":"event","project":"shelbi"}"#, &test_daemon()).unwrap_err();
        assert!(err.to_string().contains("missing `line`"), "{err}");
    }

    #[test]
    fn dispatch_event_rejects_empty_line() {
        let err = dispatch(
            r#"{"verb":"event","project":"shelbi","line":""}"#,
            &test_daemon(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
    }

    #[test]
    fn dispatch_event_rejects_embedded_newline() {
        let err = dispatch(
            r#"{"verb":"event","project":"shelbi","line":"a\nb"}"#,
            &test_daemon(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("newlines"), "{err}");
    }

    #[test]
    fn dispatch_request_clarification_requires_core_fields() {
        let d = test_daemon();
        // Missing task_id
        let err = dispatch(
            r#"{"verb":"request-clarification","project":"shelbi","question_id":"q-1","question":"ok?"}"#,
            &d,
        )
        .unwrap_err();
        assert!(err.to_string().contains("task_id"), "{err}");

        // Missing question_id
        let err = dispatch(
            r#"{"verb":"request-clarification","project":"shelbi","task_id":"t-1","question":"ok?"}"#,
            &d,
        )
        .unwrap_err();
        assert!(err.to_string().contains("question_id"), "{err}");

        // Missing question
        let err = dispatch(
            r#"{"verb":"request-clarification","project":"shelbi","task_id":"t-1","question_id":"q-1"}"#,
            &d,
        )
        .unwrap_err();
        assert!(err.to_string().contains("question"), "{err}");
    }

    #[test]
    fn dispatch_message_ack_requires_core_fields() {
        let d = test_daemon();
        let err = dispatch(
            r#"{"verb":"message-ack","project":"shelbi","task_id":"t-1"}"#,
            &d,
        )
        .unwrap_err();
        assert!(err.to_string().contains("msg_id"), "{err}");

        let err = dispatch(
            r#"{"verb":"message-ack","project":"shelbi","msg_id":"m-1"}"#,
            &d,
        )
        .unwrap_err();
        assert!(err.to_string().contains("task_id"), "{err}");
    }

    #[test]
    fn dispatch_message_pushed_requires_core_fields() {
        let d = test_daemon();
        let err = dispatch(
            r#"{"verb":"message-pushed","project":"shelbi","task_id":"t-1"}"#,
            &d,
        )
        .unwrap_err();
        assert!(err.to_string().contains("msg_id"), "{err}");
    }

    #[test]
    fn message_pushed_then_ack_clears_pending_map() {
        let d = test_daemon();
        // Push: pending map gains the entry.
        dispatch(
            r#"{"verb":"message-pushed","project":"shelbi","task_id":"t-1","msg_id":"m-1"}"#,
            &d,
        )
        .unwrap();
        assert_eq!(d.pending.lock().unwrap().len(), 1);
        assert!(d
            .pending
            .lock()
            .unwrap()
            .contains_key(&("t-1".to_string(), "m-1".to_string())));

        // Ack: pending map is cleared.
        dispatch(
            r#"{"verb":"message-ack","project":"shelbi","task_id":"t-1","msg_id":"m-1"}"#,
            &d,
        )
        .unwrap();
        assert!(d.pending.lock().unwrap().is_empty());
    }

    #[test]
    fn message_pushed_is_idempotent_per_pair() {
        // A duplicate `message-pushed` for the same (task, msg) shouldn't
        // duplicate the entry — it should refresh the push time so a
        // legitimate operator retry doesn't trip the reaper while the
        // worker is still actively processing.
        let d = test_daemon();
        dispatch(
            r#"{"verb":"message-pushed","project":"shelbi","task_id":"t-1","msg_id":"m-1"}"#,
            &d,
        )
        .unwrap();
        let first = *d
            .pending
            .lock()
            .unwrap()
            .get(&("t-1".to_string(), "m-1".to_string()))
            .unwrap();
        std::thread::sleep(Duration::from_millis(5));
        dispatch(
            r#"{"verb":"message-pushed","project":"shelbi","task_id":"t-1","msg_id":"m-1"}"#,
            &d,
        )
        .unwrap();
        let second = *d
            .pending
            .lock()
            .unwrap()
            .get(&("t-1".to_string(), "m-1".to_string()))
            .unwrap();
        assert_eq!(d.pending.lock().unwrap().len(), 1);
        assert!(second > first, "expected timer refresh");
    }

    #[test]
    fn message_ack_for_unknown_pair_is_a_noop_not_an_error() {
        // The reaper may have claimed the entry first, or the daemon
        // restarted between push and ack — either way the worker's ack
        // is still meaningful for `events.log` and must not bounce off
        // a "no such pending message" error.
        let d = test_daemon();
        dispatch(
            r#"{"verb":"message-ack","project":"shelbi","task_id":"t-ghost","msg_id":"m-ghost"}"#,
            &d,
        )
        .expect("ack for unknown pair should be accepted");
    }

    #[test]
    fn ack_timeout_env_parses_valid_value() {
        let key = ACK_TIMEOUT_ENV;
        // Cooperate with parallel tests by saving/restoring the var.
        let saved = std::env::var(key).ok();
        std::env::set_var(key, "5");
        assert_eq!(ack_timeout_from_env(), Duration::from_secs(5));
        std::env::set_var(key, "bogus");
        assert_eq!(ack_timeout_from_env(), DEFAULT_ACK_TIMEOUT);
        std::env::set_var(key, "0");
        assert_eq!(ack_timeout_from_env(), DEFAULT_ACK_TIMEOUT);
        match saved {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn reap_expired_drains_entries_past_threshold_and_keeps_fresh_ones() {
        let mut d = test_daemon();
        d.ack_timeout = Duration::from_millis(50);
        // Backdate one entry so it's already expired, and leave the
        // other at "just now" so it survives the scan.
        {
            let mut map = d.pending.lock().unwrap();
            map.insert(
                ("t-old".into(), "m-old".into()),
                Instant::now()
                    .checked_sub(Duration::from_millis(200))
                    .unwrap(),
            );
            map.insert(("t-new".into(), "m-new".into()), Instant::now());
        }
        reap_expired(&d);
        let map = d.pending.lock().unwrap();
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&("t-new".to_string(), "m-new".to_string())));
        assert!(!map.contains_key(&("t-old".to_string(), "m-old".to_string())));
    }

    #[test]
    fn launchd_plist_contains_required_keys_and_paths() {
        let inputs = LaunchdInputs {
            binary: PathBuf::from("/Users/dev/bin/shelbi"),
            state_root: PathBuf::from("/Users/dev/.shelbi"),
            stdout_path: PathBuf::from("/Users/dev/.shelbi/logs/daemon.out"),
            stderr_path: PathBuf::from("/Users/dev/.shelbi/logs/daemon.err"),
        };
        let plist = render_launchd_plist(&inputs);
        assert!(plist.contains("<string>co.32pixels.shelbi</string>"), "{plist}");
        assert!(
            plist.contains("<string>/Users/dev/bin/shelbi</string>"),
            "absolute binary path: {plist}"
        );
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n    <true/>"));
        assert!(
            plist.contains("<string>/Users/dev/.shelbi/logs/daemon.out</string>"),
            "stdout path: {plist}"
        );
        assert!(
            plist.contains("<string>/Users/dev/.shelbi/logs/daemon.err</string>"),
            "stderr path: {plist}"
        );
        assert!(
            plist.contains("<string>/Users/dev/.shelbi</string>"),
            "SHELBI_ROOT env: {plist}"
        );
    }

    #[test]
    fn launchd_plist_xml_escapes_path_with_specials() {
        let inputs = LaunchdInputs {
            binary: PathBuf::from("/o&p<x>/shelbi"),
            state_root: PathBuf::from("/o&p<x>/.shelbi"),
            stdout_path: PathBuf::from("/o&p<x>/.shelbi/logs/daemon.out"),
            stderr_path: PathBuf::from("/o&p<x>/.shelbi/logs/daemon.err"),
        };
        let plist = render_launchd_plist(&inputs);
        assert!(plist.contains("/o&amp;p&lt;x&gt;/shelbi"), "escaped: {plist}");
        assert!(!plist.contains("/o&p<x>/shelbi"), "raw leaked: {plist}");
    }

    #[test]
    fn systemd_unit_contains_required_fields() {
        let inputs = SystemdInputs {
            binary: PathBuf::from("/home/dev/bin/shelbi"),
            state_root: PathBuf::from("/home/dev/.shelbi"),
            log_path: PathBuf::from("/home/dev/.shelbi/logs/daemon.log"),
        };
        let unit = render_systemd_unit(&inputs);
        assert!(
            unit.contains("ExecStart=/home/dev/bin/shelbi daemon"),
            "ExecStart: {unit}"
        );
        assert!(unit.contains("Restart=always"), "restart policy: {unit}");
        assert!(unit.contains("RestartSec=1s"), "restart delay: {unit}");
        assert!(
            unit.contains("Environment=SHELBI_ROOT=/home/dev/.shelbi"),
            "env: {unit}"
        );
        assert!(
            unit.contains("StandardOutput=append:/home/dev/.shelbi/logs/daemon.log"),
            "stdout: {unit}"
        );
        assert!(
            unit.contains("StandardError=append:/home/dev/.shelbi/logs/daemon.log"),
            "stderr: {unit}"
        );
        assert!(
            unit.contains("WantedBy=default.target"),
            "install target: {unit}"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parse_launchctl_field_extracts_value() {
        let sample = "\
            \tstate = running\n\
            \tpid = 12345\n\
            \tlast exit code = 0\n\
            \truns = 7\n";
        assert_eq!(
            parse_launchctl_field(sample, "state"),
            Some("running".into())
        );
        assert_eq!(parse_launchctl_field(sample, "pid"), Some("12345".into()));
        assert_eq!(
            parse_launchctl_field(sample, "last exit code"),
            Some("0".into())
        );
        assert_eq!(parse_launchctl_field(sample, "runs"), Some("7".into()));
        assert_eq!(parse_launchctl_field(sample, "missing"), None);
    }

    #[test]
    fn xml_escape_handles_all_specials() {
        assert_eq!(xml_escape("a&b<c>d\"e'f"), "a&amp;b&lt;c&gt;d&quot;e&apos;f");
        assert_eq!(xml_escape("plain/path/to/binary"), "plain/path/to/binary");
    }
}
