//! Hub-side Unix-socket listener for worker → hub messages.
//!
//! This is the runtime half of `shelbi daemon` (the on-demand
//! restart/status plumbing lives in the sibling [`super::lifecycle`]
//! module). Phases 1, 2, and 9 of the Worker →
//! Orchestrator Communication feature (see
//! `Plans/worker-orchestrator-communication.md` §5, §6, §9, §13), and
//! Phase 3 of the remove-tmux effort (`docs/removing-tmux/phase3-daemon.md`).
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
//! - `subscribe` (Phase 3, remove-tmux) — hands the connection to a streaming
//!   loop that pushes change notifications (board and workspace changes) to the
//!   client until it disconnects, so a UI need not poll. One-way: no ack, and
//!   the existing one-shot verbs above are untouched.
//!
//! Unknown verbs and malformed payloads are logged to stderr
//! (debug-escaped, so client-controlled bytes can't smuggle ANSI
//! sequences into the operator's terminal) and the daemon keeps running
//! — a single bad client must not be able to take the listener down.
//!
//! ## Version handshake
//!
//! A CLI probe opens an empty connection and half-closes its write side.
//! EOF-before-any-frame is the hello request: the daemon replies with one
//! [`shelbi_state::DaemonHello`] JSON line carrying its semver and socket
//! protocol. Real event/message connections keep the legacy wire contract
//! exactly, with `ok\n` as their first response only after dispatch. That
//! client-first negotiation lets old pane wrappers survive a daemon restart
//! without mistaking the hello for a failed delivery and duplicating events.
//! The daemon's version is also recorded in the PID file next to the PID.
//!
//! ## Hardening
//!
//! - Frames are capped at [`MAX_FRAME_BYTES`]; an over-limit or
//!   newline-free stream is rejected and its connection closed, so no
//!   client can grow the read buffer without bound.
//! - Each successfully dispatched line is answered with
//!   [`shelbi_state::DAEMON_ACK`] (`ok\n`) on the same connection.
//!   Clients that wait for it before reporting success get a real
//!   delivery guarantee; no ack → their file fallback fires.
//! - Startup takes an exclusive `flock` on `hub.sock.lock` for the
//!   daemon's lifetime — only the holder ever unlinks/binds/removes the
//!   socket, so two racing daemons can't clobber each other.
//! - On the first SIGTERM/SIGINT/SIGHUP the accept loop drains the
//!   already-accepted backlog and waits (bounded) for in-flight
//!   handlers before exiting; a second signal force-exits.
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

use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
use signal_hook::iterator::Signals;

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

/// Hard cap on one newline-delimited frame from a client. A newline-free
/// stream would otherwise grow the read buffer without bound (OOM), and
/// a multi-megabyte `line` would blow past the ≤PIPE_BUF atomicity the
/// events.log append path relies on. 64KB is orders of magnitude above
/// any legitimate message; anything bigger is a bug or an abuse and the
/// connection is closed on the spot.
const MAX_FRAME_BYTES: u64 = 64 * 1024;

/// Cap on the `line` body of an `event` message. The append path's
/// tear-free guarantee only holds for writes ≤ PIPE_BUF (4096B); 4000
/// leaves headroom for the RFC3339 timestamp prefix and the trailing
/// newline the daemon prepends/appends.
const MAX_EVENT_BODY_BYTES: usize = 4000;

/// How long the shutdown path waits for in-flight client handlers to
/// finish before exiting anyway. Handlers process one tiny line each —
/// 3s is generous for a healthy box and short enough that a wedged
/// client can't hold up a supervisor-initiated restart indefinitely.
const SHUTDOWN_DRAIN_TIMEOUT: Duration = Duration::from_secs(3);

/// Upper bound on the number of outstanding (unacked) messages the daemon
/// tracks at once. Every entry eventually becomes either an `ack=worker`
/// or a synthesized `ack=timeout` line in `events.log`, so an unbounded
/// map lets a spammy or buggy worker amplify a burst of pushes into an
/// unbounded flood of log writes. Well above any healthy fleet's in-flight
/// count — hitting it means something is wrong, and we shed rather than
/// grow without limit. Refreshing an already-tracked pair is always
/// allowed; only *new* pairs past the cap are rejected.
const MAX_PENDING: usize = 10_000;

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
    /// Hub-owned board-index refresh machinery, shared with the manager thread
    /// so a `refresh-board` socket request and a scheduled tick single-flight
    /// through the same per-project lock. See [`super::board`].
    board: super::board::BoardRefresher,
    /// The shared shutdown flag. A `subscribe` handler blocks streaming change
    /// notifications and watches this so SIGTERM stops it within a poll slice
    /// instead of leaving it parked past the shutdown drain.
    stop: Arc<AtomicBool>,
}

impl Daemon {
    fn new(ack_timeout: Duration, stop: Arc<AtomicBool>) -> Self {
        Self {
            pending: Arc::new(Mutex::new(PendingMap::new())),
            ack_timeout,
            board: super::board::BoardRefresher::default(),
            stop,
        }
    }
}

/// Foreground entry point. Binds the socket, installs signal handlers, and
/// runs the accept loop until SIGTERM/SIGINT/SIGHUP. Errors from
/// individual clients are swallowed (logged to stderr) so the daemon
/// keeps serving the rest of the fleet.
pub(super) fn run_foreground() -> Result<()> {
    let sock = shelbi_state::hub_socket_path().map_err(|e| anyhow!(e))?;
    ensure_socket_dir(&sock)?;
    // Exclusive advisory lock held for the daemon's whole lifetime. Two
    // daemons racing through startup can't both hold it, so only the
    // winner ever probes/unlinks/binds/removes the socket — the loser
    // errors out here without touching the winner's live socket.
    let _bind_lock = acquire_bind_lock(&sock)?;
    prepare_socket(&sock)?;
    prune_stale_control_masters();
    reconcile_forward_modes();
    // Version-agnostic config validate-and-upgrade pass, run once before the
    // daemon starts serving — the natural "reconcile before serving" window,
    // alongside the housekeeping sweeps above. Best-effort: it emits findings
    // to events.log / the orchestrator handoff file and never blocks serving.
    let _ = crate::commands::config_upgrade::run_startup_pass();

    // Run with the user's interactive login-shell environment, not the minimal
    // one a detached on-demand spawn inherits. `.zshrc` is where nvm/fnm/Homebrew
    // PATH setup lives, so this is how the daemon's git/`gh`/ssh/workflow actions
    // find their tools — replacing the baked minimal PATH that used to need
    // healing to locate `gh`. Overlaid (not replaced) so anything the launcher
    // set intentionally (e.g. SHELBI_ROOT in a test) survives.
    apply_login_shell_env();

    // Retire any leftover launchd/systemd supervisor unit so its KeepAlive /
    // Restart=always loop can't fight the on-demand daemon. Idempotent; disclose
    // whatever it removed on events.log.
    retire_leftover_supervisor_units();

    // Tighten the umask around bind() so the socket inode is created
    // 0600 from the very start. Without this there is a window between
    // bind() and the chmod below where the socket carries the umask
    // default (typically world/group-connectable) and a local peer could
    // connect. Restore the previous umask immediately so nothing else the
    // daemon creates inherits the restrictive value.
    let prev_umask = unsafe { libc::umask(0o177) };
    let bind_result = UnixListener::bind(&sock);
    unsafe { libc::umask(prev_umask) };
    let listener =
        bind_result.with_context(|| format!("binding hub socket at {}", sock.display()))?;
    // Belt-and-suspenders in case a non-default umask still widened the
    // inode (umask can only clear bits, not set them).
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

    let stop = Arc::new(AtomicBool::new(false));
    let daemon = Daemon::new(ack_timeout_from_env(), stop.clone());
    eprintln!(
        "shelbi daemon: listening at {} (ack timeout {}s)",
        sock.display(),
        daemon.ack_timeout.as_secs()
    );

    install_shutdown_listener(stop.clone(), sock.clone())?;
    spawn_reaper(daemon.clone(), stop.clone());
    // The single board reader per hub: one refresh loop per open project,
    // publishing `board-index.json` on each project's configured cadence.
    super::board::spawn_refresh_manager(daemon.board.clone(), stop.clone());
    // The per-project workspace-poller manager: one poller per open project when
    // the hidden `SHELBI_DAEMON_POLLER` dev setting is on; a no-op otherwise (the
    // sidebar owns the poller by default). Phase 3, `rt-daemon-poller`.
    super::poller::spawn_poller_manager(stop.clone());
    // Exit when no project is open (the on-demand lifecycle: nothing to serve,
    // and the next open restarts us). A short minimum-lifetime debounce keeps a
    // just-started daemon alive long enough for the opener to record its open
    // flag and for a restart to verify, and avoids thrash on project switches.
    spawn_idle_monitor(stop.clone(), sock.clone());

    // The mutation control socket, bound and accepting BEFORE the hub serve loop
    // below starts answering hellos — so a client that waits on hub.sock and
    // then connects to control.sock never races the bind. Its accept loop runs
    // on its own thread (the hub serve below blocks this one) and watches the
    // same `stop` flag, so SIGTERM stops both.
    let control_sock = shelbi_state::control_socket_path().map_err(|e| anyhow!(e))?;
    let control_listener = super::control::bind(&control_sock)?;
    {
        let control_state = super::control::ControlState::new();
        let stop = stop.clone();
        thread::spawn(move || super::control::serve(control_listener, control_state, stop));
    }

    serve(&listener, &daemon, &stop);

    let _ = fs::remove_file(&control_sock);
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

/// Overlay the user's interactive login-shell environment onto this process's
/// environment. Captured once (`$SHELL -l -i -c env`, cached in `shelbi-core`)
/// and applied over the inherited environment so subprocesses (git, `gh`, ssh,
/// workflow actions) see the user's real PATH and config. An empty capture (the
/// login shell couldn't run) is a no-op, so a failure degrades to the inherited
/// environment rather than clobbering it.
fn apply_login_shell_env() {
    for (key, value) in shelbi_core::login_shell_env() {
        std::env::set_var(key, value);
    }
}

/// Stop and remove any leftover launchd/systemd supervisor unit (the retired
/// install path), disclosing each removal on `events.log`. Idempotent and
/// best-effort — a host with no unit does nothing, and a disclosure hiccup never
/// takes the daemon down.
fn retire_leftover_supervisor_units() {
    let removed = super::lifecycle::retire_supervisor_units();
    for path in removed {
        let body = format!("daemon-unit-retired unit={}", path.display());
        if let Err(e) = shelbi_state::append_external_event(&body) {
            tracing::debug!(error = %e, "shelbi daemon: failed to disclose daemon-unit-retired");
        }
        tracing::warn!(
            unit = %path.display(),
            "shelbi daemon: retired a leftover supervisor unit (daemon is now on-demand)",
        );
    }
}

/// Default minimum daemon lifetime before the idle monitor may trigger an exit.
/// Covers the window where a just-opened project hasn't recorded its flag yet
/// and the restart-verify window (which waits up to ~10s). Overridable with
/// [`IDLE_GRACE_ENV`] (milliseconds) so tests drive it fast.
const IDLE_GRACE: Duration = Duration::from_secs(15);
/// Default interval between idle checks once past the grace period. Overridable
/// with [`IDLE_POLL_ENV`] (milliseconds).
const IDLE_POLL: Duration = Duration::from_secs(5);
const IDLE_GRACE_ENV: &str = "SHELBI_DAEMON_IDLE_GRACE_MS";
const IDLE_POLL_ENV: &str = "SHELBI_DAEMON_IDLE_POLL_MS";

/// Parse a milliseconds env override, falling back to `default` on absence or a
/// non-positive/unparseable value.
fn duration_from_env_ms(key: &str, default: Duration) -> Duration {
    match std::env::var(key).ok().and_then(|v| v.parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => default,
    }
}

/// Spawn the idle-exit monitor: after a minimum-lifetime grace, poll the
/// open-project set and shut the daemon down once it is empty. Shutting down
/// reuses the signal path — flip the shared stop flag, then self-connect to wake
/// the blocking `accept()` so the main loop drains and exits.
fn spawn_idle_monitor(stop: Arc<AtomicBool>, sock: PathBuf) {
    let grace = duration_from_env_ms(IDLE_GRACE_ENV, IDLE_GRACE);
    let poll = duration_from_env_ms(IDLE_POLL_ENV, IDLE_POLL);
    thread::spawn(move || {
        if !sleep_unless_stopped(&stop, grace, poll) {
            return;
        }
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let empty = shelbi_state::list_open_projects()
                .map(|v| v.is_empty())
                .unwrap_or(false);
            if empty {
                eprintln!("shelbi daemon: no project open, shutting down");
                stop.store(true, Ordering::SeqCst);
                // Wake the accept loop so it notices the flag and drains.
                let _ = UnixStream::connect(&sock);
                return;
            }
            if !sleep_unless_stopped(&stop, poll, poll) {
                return;
            }
        }
    });
}

/// Sleep for `total` in `slice`-sized steps, returning early (`false`) if the
/// stop flag is set partway through so the monitor doesn't outlive a shutdown.
/// Returns `true` if the full duration elapsed without a stop.
fn sleep_unless_stopped(stop: &Arc<AtomicBool>, total: Duration, slice: Duration) -> bool {
    let slice = slice.min(total).max(Duration::from_millis(10));
    let mut waited = Duration::ZERO;
    while waited < total {
        if stop.load(Ordering::SeqCst) {
            return false;
        }
        thread::sleep(slice);
        waited += slice;
    }
    !stop.load(Ordering::SeqCst)
}

/// Decrements the live-connection counter when a handler thread exits —
/// Drop-based so a panicking handler still releases its slot and the
/// shutdown drain doesn't wait the full deadline for a thread that's
/// already gone.
struct LiveGuard(Arc<AtomicUsize>);
impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Accept loop + shutdown drain. Runs until the stop flag is set (the
/// signal listener flips it and wakes the blocking `accept()` with a
/// self-connect), then:
///
/// 1. keeps accepting in non-blocking mode until the listen backlog is
///    empty — a client whose `connect()` succeeded before the flag
///    flipped already considers its write in flight, so we must read it
///    rather than exit with it queued, and
/// 2. waits (up to [`SHUTDOWN_DRAIN_TIMEOUT`]) for every spawned handler
///    to finish, so an accepted connection is never dropped mid-dispatch
///    by process exit.
///
/// Combined with the ack byte `handle_client` writes per processed line,
/// a daemon restart can't silently eat an event: either the handler
/// finishes (event lands, client sees the ack) or the client never gets
/// the ack and its file fallback fires.
fn serve(listener: &UnixListener, daemon: &Daemon, stop: &Arc<AtomicBool>) {
    let live = Arc::new(AtomicUsize::new(0));
    for incoming in listener.incoming() {
        match incoming {
            Ok(stream) => {
                let daemon = daemon.clone();
                live.fetch_add(1, Ordering::SeqCst);
                let guard = LiveGuard(live.clone());
                thread::spawn(move || {
                    let _guard = guard;
                    handle_client(stream, &daemon);
                });
            }
            // Non-blocking mode (entered below once stop is set) reports
            // an empty backlog as WouldBlock — that's the drained-clean
            // exit path.
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(e) => {
                eprintln!("shelbi daemon: accept error: {e}");
            }
        }
        if stop.load(Ordering::SeqCst) {
            // Drain whatever the kernel already queued without blocking
            // for new clients. If the mode switch fails we can't drain
            // safely — bail and rely on the handler wait below.
            if listener.set_nonblocking(true).is_err() {
                break;
            }
        }
    }

    let deadline = Instant::now() + SHUTDOWN_DRAIN_TIMEOUT;
    while live.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let leftover = live.load(Ordering::SeqCst);
    if leftover > 0 {
        eprintln!("shelbi daemon: exiting with {leftover} client connection(s) still open");
    }
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
/// Lock the pending map, recovering from poison. A poisoned mutex means
/// some handler panicked while holding the lock — the map itself is
/// still a structurally sound `HashMap`, and bailing instead would wedge
/// every future push/ack into "timed out" forever. One policy for every
/// lock site: recover and keep serving.
fn lock_pending(pending: &Mutex<PendingMap>) -> MutexGuard<'_, PendingMap> {
    pending.lock().unwrap_or_else(PoisonError::into_inner)
}

fn reap_expired(daemon: &Daemon) {
    let timeout = daemon.ack_timeout;
    let now = Instant::now();
    let expired: Vec<(String, String)> = {
        let mut map = lock_pending(&daemon.pending);
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

/// Reconcile `forward-modes.json` against reality right after the stale
/// ControlMaster sweep, releasing any persisted forward decision whose master is
/// no longer alive. A dead master has no `-R` listener behind it, so a leftover
/// TCP entry both points workers at a dead port and — worse — could be miscounted
/// as an occupied port on the next allocation sweep (the false-exhaustion bug).
/// Runs once at startup, before any poller re-establishes forwards, so there is
/// no race with an in-flight ensure. Best-effort: never fails the daemon start.
fn reconcile_forward_modes() {
    let released = shelbi_state::reconcile_forward_state(|host, _hf| shelbi_ssh::master_alive(host));
    if !released.is_empty() {
        eprintln!(
            "shelbi daemon: forward-mode reconciliation — released {} stale entr{} ({})",
            released.len(),
            if released.len() == 1 { "y" } else { "ies" },
            released.join(", "),
        );
    }
}

/// Make sure the socket parent directory exists with `0700` perms.
/// Split out of [`prepare_socket`] because the bind lock file lives in
/// the same directory and must be acquirable *before* we touch the
/// socket itself.
fn ensure_socket_dir(sock: &Path) -> Result<()> {
    if let Some(parent) = sock.parent() {
        if !parent.as_os_str().is_empty() {
            let existed = parent.exists();
            fs::create_dir_all(parent)
                .with_context(|| format!("creating socket parent {}", parent.display()))?;
            // Only lock down a parent directory we just created (the
            // `~/.shelbi` home is ours to own). A pre-existing dir — most
            // importantly a shared one like `/tmp` when `SHELBI_HUB_SOCK`
            // points there — must keep its own permissions; chmod 700 on
            // `/tmp` would break every other user on the box. The socket
            // file itself gets chmod 600 after bind, which is what
            // actually restricts access to it.
            if !existed {
                fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
                    .with_context(|| format!("chmod 700 {}", parent.display()))?;
            }
        }
    }
    Ok(())
}

/// Take an exclusive advisory `flock` on `<sock>.lock` (e.g.
/// `hub.sock.lock`) and return the open file. The caller holds the file
/// — and therefore the lock — for the daemon's entire lifetime, which
/// makes the probe → unlink → bind sequence in [`prepare_socket`] and
/// the exit-path `remove_file` single-daemon by construction: the old
/// connect-probe alone was a TOCTOU where two racing daemons could each
/// see the other's socket as stale and unlink it.
///
/// The lock file itself is never removed — deleting it would let a
/// third daemon lock a *fresh* inode while the second still holds the
/// old one, recreating the race the lock exists to close. An orphaned
/// `hub.sock.lock` is inert: `flock` locks die with the holder.
fn acquire_bind_lock(sock: &Path) -> Result<fs::File> {
    let mut lock_os = sock.as_os_str().to_os_string();
    lock_os.push(".lock");
    let lock_path = PathBuf::from(lock_os);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("opening daemon lock file {}", lock_path.display()))?;
    // SAFETY: `flock` on a valid fd we own; no memory is dereferenced.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc != 0 {
        let err = std::io::Error::last_os_error();
        return Err(anyhow!(
            "another shelbi daemon holds {} ({err}) — refusing to start \
             (stop the other daemon, or check `shelbi daemon status`)",
            lock_path.display()
        ));
    }
    Ok(file)
}

/// Make sure the socket file itself is free for `bind()`. A leftover
/// socket from a previous run is reclaimed only if no one is currently
/// listening on it; a live daemon at the same path is a hard error.
/// Callers must already hold the bind lock ([`acquire_bind_lock`]) so
/// the probe-then-unlink below can't race another daemon's startup —
/// the connect probe is kept as defense in depth against a daemon
/// started by an older binary that predates the lock.
fn prepare_socket(sock: &Path) -> Result<()> {
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
                fs::remove_file(sock)
                    .with_context(|| format!("removing stale socket at {}", sock.display()))?;
            }
        }
    }
    Ok(())
}

/// Catch SIGTERM/SIGINT/SIGHUP on a background thread. The first signal
/// flips the stop flag and wakes the blocking `accept()` with a single
/// self-connection so the main loop notices and drains gracefully. Any
/// further signal force-exits: if the wake-up self-connect failed to
/// unblock the accept loop for whatever reason, the operator's second
/// Ctrl-C / SIGTERM must still work without resorting to SIGKILL (which
/// would skip socket/PID cleanup entirely).
fn install_shutdown_listener(stop: Arc<AtomicBool>, sock: PathBuf) -> Result<()> {
    let mut signals =
        Signals::new([SIGTERM, SIGINT, SIGHUP]).context("installing daemon signal handlers")?;
    thread::spawn(move || {
        let mut seen_first = false;
        for sig in signals.forever() {
            if !seen_first {
                seen_first = true;
                eprintln!("shelbi daemon: received signal {sig}, shutting down");
                stop.store(true, Ordering::SeqCst);
                // Wake the accept loop. The connection itself is unused —
                // it's just a syscall poke so accept() returns instead of
                // blocking on the next client.
                let _ = UnixStream::connect(&sock);
            } else {
                eprintln!("shelbi daemon: received signal {sig} during shutdown, forcing exit");
                std::process::exit(1);
            }
        }
    });
    Ok(())
}

/// One client → one BufReader → newline-delimited JSON. Each line is
/// dispatched independently so a bad line in the middle of a batch
/// doesn't kill the rest. EOF closes the handler cleanly.
///
/// **Version handshake:** a dedicated probe sends no frame and half-closes
/// its write side. When the first read returns EOF, the daemon replies with
/// one [`shelbi_state::DaemonHello`] line and closes. A connection that sends
/// any frame never receives that hello, preserving the pre-handshake client's
/// exact post-dispatch `ok\n` response. Write errors are ignored because the
/// shutdown self-poke and liveness probes also connect and immediately close.
///
/// Two hardening properties on top of the dispatch loop:
///
/// - **Frame cap.** Each line is read through a [`MAX_FRAME_BYTES`]
///   `take`, so a newline-free (or absurdly long) stream can't grow the
///   buffer without bound. An over-limit frame closes the connection —
///   no ack, so a well-behaved client falls back to its degraded path.
/// - **Ack per processed line.** After a successful dispatch the daemon
///   writes [`shelbi_state::DAEMON_ACK`] back on the stream. Clients
///   that read it before reporting success get a real delivery
///   guarantee: a daemon killed mid-dispatch never acks, so the
///   client-side file fallback fires instead of the event vanishing.
///   Write errors are ignored — fire-and-forget clients (`nc` scripts
///   that exit early) may close their read side first, and Rust ignores
///   SIGPIPE so the failed write is just an `Err` we drop.
///
/// Rejected lines are logged debug-escaped (`{:?}`) so ANSI/control
/// bytes from a hostile or confused client can't reach the operator's
/// terminal through `tail -f` on the daemon log.
fn handle_client(stream: UnixStream, daemon: &Daemon) {
    let mut reader = BufReader::new(&stream);
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    let mut saw_frame = false;
    loop {
        buf.clear();
        // `take` bounds this read: read_until returns at the newline,
        // at EOF, or after MAX_FRAME_BYTES + 1 bytes — whichever comes
        // first. The +1 lets us tell "exactly at the cap with a
        // newline" (fine) from "past the cap" (rejected).
        let n = match (&mut reader)
            .take(MAX_FRAME_BYTES + 1)
            .read_until(b'\n', &mut buf)
        {
            Ok(n) => n,
            Err(e) => {
                eprintln!("shelbi daemon: client read error: {e}");
                return;
            }
        };
        if n == 0 {
            if !saw_frame {
                // Empty, half-closed connection: explicit hello probe. This
                // reply happens only after EOF, never ahead of a legacy event
                // frame whose client expects `ok\n` as the first three bytes.
                let hello = shelbi_state::DaemonHello::new(env!("CARGO_PKG_VERSION"));
                let _ = (&stream).write_all(hello.to_line().as_bytes());
            }
            break; // EOF
        }
        saw_frame = true;
        let terminated = buf.last() == Some(&b'\n');
        if !terminated && n as u64 > MAX_FRAME_BYTES {
            eprintln!("shelbi daemon: frame exceeds {MAX_FRAME_BYTES} bytes; closing connection");
            return;
        }
        while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
            buf.pop();
        }
        let line = match std::str::from_utf8(&buf) {
            Ok(l) => l,
            Err(e) => {
                eprintln!("shelbi daemon: rejected non-UTF-8 frame: {e}");
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        // `subscribe` is not a one-shot request: it hands the connection over to
        // a streaming loop that pushes change notifications until the client
        // disconnects or the daemon shuts down. It takes over this connection, so
        // the per-line read loop ends here.
        if let Some(project_filter) = subscribe_project_filter(line) {
            stream_changes(&stream, &daemon.stop, project_filter.as_deref());
            return;
        }
        match dispatch(line, daemon) {
            // A verb with a custom reply (e.g. `refresh-board` → the new
            // `fetched_at`) writes that reply, newline-terminated, in place of
            // the bare `ok\n`. Every legacy verb returns `None` and keeps its
            // exact post-dispatch ack, so pre-handshake clients are unaffected.
            Ok(Some(reply)) => {
                let _ = (&stream).write_all(reply.as_bytes());
                let _ = (&stream).write_all(b"\n");
            }
            Ok(None) => {
                let _ = (&stream).write_all(shelbi_state::DAEMON_ACK);
            }
            Err(e) => {
                // Log + continue; one bad message must not take down a
                // multi-message client connection. No ack — the sender
                // must not mistake a rejection for delivery.
                eprintln!("shelbi daemon: rejected message: {e}: {line:?}");
            }
        }
    }
}

/// How long [`stream_changes`] blocks for the next notification before looping
/// to re-check the shutdown flag. Short enough that SIGTERM stops a parked
/// subscriber well within the shutdown drain.
const SUBSCRIBE_POLL_SLICE: Duration = Duration::from_millis(250);

/// If `line` is a `subscribe` frame, returns its optional project filter:
/// `Some(None)` subscribes to every project's changes, `Some(Some(p))` to only
/// project `p`'s. Returns `None` when the frame is not a subscribe, so it falls
/// through to `dispatch`, which reports the real parse error.
///
/// Parsed separately from the full [`Message`] so the streaming decision in
/// [`handle_client`] doesn't have to grow the one-shot dispatch path a sentinel
/// return. A per-project client (a sidebar watching one board) passes its
/// `project` so the daemon only wakes it for that project's churn; a client
/// omits it to see everything.
fn subscribe_project_filter(line: &str) -> Option<Option<String>> {
    #[derive(Deserialize)]
    struct SubFrame {
        verb: String,
        #[serde(default)]
        project: Option<String>,
    }
    serde_json::from_str::<SubFrame>(line)
        .ok()
        .filter(|m| m.verb == "subscribe")
        .map(|m| m.project)
}

/// Stream change notifications to a subscribed client until it disconnects or
/// the daemon shuts down (Phase 3 pushed change notifications,
/// `docs/removing-tmux/phase3-daemon.md`).
///
/// The client subscribes once and then only reads; the daemon pushes one NDJSON
/// [`shelbi_state::ChangeNotification`] line per change (board moves, workspace
/// status changes) that the board refresher and the poller publish to the
/// in-process change bus. A failed write means the client is gone, so the loop
/// exits and drops the subscription (pruned from the bus on the next publish).
/// The connection carries no `ok` ack — it is a one-way push channel, distinct
/// from the one-shot event/message verbs and from the mutation control socket
/// `rt-mutations-daemon` adds.
///
/// `project`, when set, filters the stream to that one project: the daemon is
/// hub-global and the bus carries every open project's changes, so a sidebar
/// watching one board subscribes with its `project` and is woken only by its
/// own churn, not a sibling project's.
fn stream_changes(mut stream: &UnixStream, stop: &Arc<AtomicBool>, project: Option<&str>) {
    let sub = shelbi_state::subscribe_changes();
    while !stop.load(Ordering::SeqCst) {
        if let Some(change) = sub.recv_timeout(SUBSCRIBE_POLL_SLICE) {
            if project.is_some_and(|p| change.project() != p) {
                continue; // another project's change — not this subscriber's
            }
            if stream.write_all(change.to_line().as_bytes()).is_err() {
                return; // client disconnected
            }
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
    /// Issue this message references. Required for `request-clarification`,
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

/// Dispatch one client frame. `Ok(None)` means "reply with the standard
/// `ok\n` ack"; `Ok(Some(reply))` carries a verb-specific reply line the caller
/// writes instead (only `refresh-board` uses this today). `Err` is logged and
/// gets no ack, so the sender never mistakes a rejection for delivery.
fn dispatch(raw: &str, daemon: &Daemon) -> Result<Option<String>> {
    let msg: Message = serde_json::from_str(raw).context("invalid JSON payload")?;
    match msg.verb.as_str() {
        "event" => handle_event(&msg).map(|()| None),
        "request-clarification" => handle_request_clarification(&msg).map(|()| None),
        "message-pushed" => handle_message_pushed(&msg, daemon).map(|()| None),
        "message-ack" => handle_message_ack(&msg, daemon).map(|()| None),
        "refresh-board" => handle_refresh_board(&msg, daemon).map(Some),
        // Debug-escaped so a control-byte-laden verb can't smuggle ANSI
        // sequences into the daemon log.
        other => Err(anyhow!("unknown verb {other:?}")),
    }
}

/// On-demand board refresh (`Plans/github-issue-caching-and-rate-limits.md`
/// §5): force a fresh read of `project`'s board now, publish `board-index.json`,
/// and reply with the new `fetched_at`. The caller (a consumer that needs a
/// board fresher than the file) waits at most two seconds for this reply, then
/// falls back to the file or the single-issue path — so a slow backend read
/// degrades to a timeout, never a hang.
fn handle_refresh_board(msg: &Message, daemon: &Daemon) -> Result<String> {
    let project = required(msg.project.as_deref(), "refresh-board", "project")?;
    daemon.board.refresh_now(project)
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
    // The O_APPEND write is only tear-free while the whole record stays
    // ≤ PIPE_BUF; an oversized body would silently forfeit that, so it's
    // rejected here rather than mangled downstream.
    if body.len() > MAX_EVENT_BODY_BYTES {
        return Err(anyhow!(
            "event `line` exceeds {MAX_EVENT_BODY_BYTES} bytes ({} bytes)",
            body.len()
        ));
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
    let mut map = lock_pending(&daemon.pending);
    let key = (task_id.to_string(), msg_id.to_string());
    if !map.contains_key(&key) && map.len() >= MAX_PENDING {
        return Err(anyhow!(
            "pending-ack map at capacity ({MAX_PENDING}); refusing message-pushed for {}/{} \
             (a worker is likely flooding pushes without acking)",
            key.0,
            key.1
        ));
    }
    map.insert(key, Instant::now());
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
        let mut map = lock_pending(&daemon.pending);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_daemon() -> Daemon {
        // Tests that exercise the timeout branch override `ack_timeout`
        // locally; everything else uses the production default so the
        // unit tests reflect real config.
        Daemon::new(DEFAULT_ACK_TIMEOUT, Arc::new(AtomicBool::new(false)))
    }

    /// RAII guard: point `$SHELBI_HOME` at a fresh temp dir for the
    /// duration of a test that calls `dispatch()` (or `reap_expired`,
    /// or any other daemon path that eventually writes to
    /// `events.log`). Historically these tests ran without isolation
    /// and their fixture ids (`t-1`, `m-1`, `t-ghost`, `m-ghost`,
    /// `t-old`, `m-old`) polluted the developer's real
    /// `~/.shelbi/events.log` on every `cargo test` — the exact
    /// "ghost keepalive ack" pattern the messaging-drop bug report
    /// called out. Any test that names test-shape ids MUST use this
    /// guard.
    ///
    /// Because `env::set_var` is process-global and Rust runs tests in
    /// parallel, the guard also holds the shared `ENV_LOCK` so two
    /// polluting tests can't race on `SHELBI_HOME` and clobber each
    /// other's isolated log.
    struct IsolatedShelbiHome {
        _lock: std::sync::MutexGuard<'static, ()>,
        prev: Option<String>,
        home: PathBuf,
    }
    impl IsolatedShelbiHome {
        fn new(tag: &str) -> Self {
            let lock = crate::commands::test_support::ENV_LOCK
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            let home = std::env::temp_dir().join(format!(
                "shelbi-daemon-test-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&home).unwrap();
            let prev = std::env::var("SHELBI_HOME").ok();
            std::env::set_var("SHELBI_HOME", &home);
            Self {
                _lock: lock,
                prev,
                home,
            }
        }
    }
    impl Drop for IsolatedShelbiHome {
        fn drop(&mut self) {
            match self.prev.take() {
                Some(v) => std::env::set_var("SHELBI_HOME", v),
                None => std::env::remove_var("SHELBI_HOME"),
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }

    fn assert_hello(bytes: &[u8]) {
        let hello = shelbi_state::DaemonHello::parse(std::str::from_utf8(bytes).unwrap())
            .expect("response must be the daemon hello");
        assert_eq!(hello.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(hello.protocol, shelbi_state::HUB_PROTOCOL_VERSION);
    }

    /// The version handshake uses an empty, half-closed connection as its
    /// request. No ordinary client frame is present, so the only response is
    /// the version/protocol hello.
    #[test]
    fn empty_half_closed_connection_returns_hello() {
        use std::net::Shutdown;
        let d = test_daemon();
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        client.shutdown(Shutdown::Write).unwrap(); // send nothing
        let mut bytes = Vec::new();
        (&client).read_to_end(&mut bytes).unwrap();
        handler.join().unwrap();

        assert_hello(&bytes);
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

    /// Minimal `file_system`-backend project YAML under the isolated home, so
    /// `refresh-board` can build a real (local, offline) store and read an
    /// empty board without a network backend.
    fn write_fs_project_yaml(home: &Path, name: &str) {
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(
            home.join(format!("projects/{name}.yaml")),
            format!(
                "name: {name}\nrepo: /tmp/{name}\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/{name}\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n"
            ),
        )
        .unwrap();
    }

    #[test]
    fn dispatch_refresh_board_requires_project_field() {
        // The verb is wired and validates its required field — a missing
        // project is a rejection (no ack), never a panic.
        let err = dispatch(r#"{"verb":"refresh-board"}"#, &test_daemon()).unwrap_err();
        assert!(err.to_string().contains("project"), "{err}");
    }

    #[test]
    fn dispatch_refresh_board_publishes_index_and_replies_with_fetched_at() {
        // End-to-end through the socket dispatch: a `refresh-board` for a real
        // (file_system) project reads the board, writes board-index.json, and
        // replies with the new fetched_at rather than the bare `ok`.
        let iso = IsolatedShelbiHome::new("refresh-board");
        write_fs_project_yaml(&iso.home, "p");
        let reply = dispatch(r#"{"verb":"refresh-board","project":"p"}"#, &test_daemon())
            .expect("refresh-board dispatch")
            .expect("refresh-board replies with a fetched_at, not the bare ack");
        assert!(
            chrono::DateTime::parse_from_rfc3339(reply.trim()).is_ok(),
            "reply is an RFC3339 fetched_at: {reply:?}"
        );
        let index = shelbi_state::read_board_index("p").expect("index published");
        assert_eq!(index.fetched_at, reply.trim());
        assert!(!index.stale);
    }

    #[test]
    fn handle_client_writes_the_refresh_board_reply_not_a_bare_ack() {
        // The wire path: a `refresh-board` frame gets the fetched_at line back,
        // distinguishable from the 3-byte `ok\n` a legacy verb returns.
        use std::net::Shutdown;
        let iso = IsolatedShelbiHome::new("refresh-board-wire");
        write_fs_project_yaml(&iso.home, "p");
        let d = test_daemon();
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        (&client)
            .write_all(b"{\"verb\":\"refresh-board\",\"project\":\"p\"}\n")
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut bytes = Vec::new();
        (&client).read_to_end(&mut bytes).unwrap();
        handler.join().unwrap();

        let reply = String::from_utf8(bytes).unwrap();
        assert_ne!(reply, "ok\n", "refresh-board must not reply with the bare ack");
        assert!(
            chrono::DateTime::parse_from_rfc3339(reply.trim()).is_ok(),
            "wire reply is an RFC3339 fetched_at: {reply:?}"
        );
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
        // The ack path calls `append_message_ack_event` which writes to
        // `~/.shelbi/events.log`. Isolate SHELBI_HOME so this test's
        // fixture ids (`t-1`, `m-1`) don't leak into the developer's
        // real events log as fake "ack=worker" lines on every `cargo
        // test` — the exact ghost-keepalive pattern the message-drop
        // bug report flagged.
        let _iso = IsolatedShelbiHome::new("push-then-ack");
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

    /// Happy path for the worker → hub `request-clarification` handshake:
    /// the daemon must persist the question as a
    /// `question=<q-id> task=<t-id> kind=clarification text=<snippet>`
    /// line so the orchestrator's `events tail` surfaces it, and the
    /// orchestrator can then answer with
    /// `shelbi message <task> reply --in-response-to <q-id> "…"`. This
    /// closes the loop the `--in-response-to` flag on `shelbi message`
    /// exists to serve — without a real e2e test, the flag drifts into
    /// dead code.
    #[test]
    fn request_clarification_dispatch_writes_events_log_line() {
        let _iso = IsolatedShelbiHome::new("clarify-happy");
        let d = test_daemon();
        let payload = r#"{"verb":"request-clarification","project":"shelbi","task_id":"feat-y","question_id":"q-42","question":"Should the dropdown use ARIA combobox roles?","context":"components/Menu.tsx line 88"}"#;
        dispatch(payload, &d).expect("clarification dispatch");

        // Confirm the line landed in the isolated events.log.
        let log = shelbi_state::events_log_path().unwrap();
        let body = std::fs::read_to_string(&log).expect("events log missing");
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 1, "expected one event line, got: {body}");
        let line = lines[0];
        assert!(
            line.contains(" question=q-42 "),
            "line missing question id: {line}"
        );
        assert!(line.contains(" task=feat-y "), "line missing task: {line}");
        assert!(
            line.contains(" kind=clarification "),
            "line missing kind marker: {line}"
        );
        // The question text must be represented (folded/truncated is
        // fine — the acceptance bar is that the operator sees a
        // human-readable snippet on the events stream).
        assert!(
            line.contains("dropdown") || line.contains("ARIA"),
            "line dropped the question text: {line}"
        );
    }

    #[test]
    fn message_ack_for_unknown_pair_is_a_noop_not_an_error() {
        // The reaper may have claimed the entry first, or the daemon
        // restarted between push and ack — either way the worker's ack
        // is still meaningful for `events.log` and must not bounce off
        // a "no such pending message" error.
        //
        // Isolate SHELBI_HOME so this test's `t-ghost` / `m-ghost`
        // fixture ids don't pollute the developer's real events log —
        // exactly the source of the "keepalive-shaped ghost acks" the
        // message-drop bug report saw drifting through events.log.
        let _iso = IsolatedShelbiHome::new("ack-ghost");
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
        // `reap_expired` synthesizes `message=<id> task=<id>
        // ack=timeout` lines; without isolation this test's `t-old` /
        // `m-old` fixtures land in the developer's real events log.
        let _iso = IsolatedShelbiHome::new("reap-expired");
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
    fn dispatch_event_rejects_over_length_line() {
        let body = "x".repeat(MAX_EVENT_BODY_BYTES + 1);
        let payload = serde_json::json!({"verb": "event", "line": body}).to_string();
        let err = dispatch(&payload, &test_daemon()).unwrap_err();
        assert!(err.to_string().contains("exceeds"), "{err}");
    }

    /// F12: a handler that panicked while holding the pending-map lock
    /// must not wedge every later push/ack into a permanent error (which
    /// the reaper would then surface as bogus `ack=timeout` lines for
    /// every message forever). All lock sites recover from poison.
    #[test]
    fn pending_map_poison_is_recovered_not_fatal() {
        let _iso = IsolatedShelbiHome::new("poison");
        let d = test_daemon();
        let pending = d.pending.clone();
        let _ = std::thread::spawn(move || {
            let _guard = pending.lock().unwrap();
            panic!("poison the pending map on purpose");
        })
        .join();
        dispatch(
            r#"{"verb":"message-pushed","project":"shelbi","task_id":"t-p","msg_id":"m-p"}"#,
            &d,
        )
        .expect("push must survive a poisoned lock");
        assert_eq!(lock_pending(&d.pending).len(), 1);
        dispatch(
            r#"{"verb":"message-ack","project":"shelbi","task_id":"t-p","msg_id":"m-p"}"#,
            &d,
        )
        .expect("ack must survive a poisoned lock");
        assert!(lock_pending(&d.pending).is_empty());
    }

    /// F4: the bind lock is exclusive for its lifetime and reacquirable
    /// after release — two daemons racing through startup can't both
    /// hold it, so only one ever unlinks/binds the socket.
    #[test]
    fn acquire_bind_lock_is_exclusive_and_releases_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "shelbi-daemon-lock-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("hub.sock");

        let first = acquire_bind_lock(&sock).expect("first lock");
        let second = acquire_bind_lock(&sock);
        assert!(second.is_err(), "second lock must be refused while held");
        assert!(
            second
                .unwrap_err()
                .to_string()
                .contains("another shelbi daemon"),
            "error should name the culprit"
        );
        drop(first);
        acquire_bind_lock(&sock).expect("relock after release");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Rolling-upgrade regression: a pre-handshake client reads exactly the
    /// first three response bytes and accepts only `ok\n`. A new daemon must
    /// preserve that response so the already-recorded event is not retried and
    /// then appended again through the degraded fallback.
    #[test]
    fn legacy_client_gets_bare_ack_and_pane_death_lands_once() {
        use std::net::Shutdown;
        let _iso = IsolatedShelbiHome::new("hc-ack");
        let d = test_daemon();
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        (&client)
            .write_all(
                b"{\"verb\":\"event\",\"project\":\"shelbi\",\"line\":\"project=shelbi workspace=bravo pane_alive=false reason=exit:0\"}\n",
            )
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut ack = [0u8; 3];
        (&client).read_exact(&mut ack).unwrap();
        assert_eq!(&ack, shelbi_state::DAEMON_ACK, "legacy ack: {ack:?}");
        let mut trailing = Vec::new();
        (&client).read_to_end(&mut trailing).unwrap();
        assert!(trailing.is_empty(), "legacy response grew: {trailing:?}");
        handler.join().unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(log.contains("workspace=bravo pane_alive=false"), "log: {log}");
        assert_eq!(
            log.lines()
                .filter(|line| line.contains("workspace=bravo pane_alive=false"))
                .count(),
            1,
            "pane-death event must be recorded exactly once: {log}"
        );
    }

    /// F2: an over-limit (newline-free) frame is rejected, the buffer
    /// never grows past the cap, and the connection is closed without an
    /// ack — so a well-behaved client falls back instead of assuming
    /// delivery.
    #[test]
    fn handle_client_rejects_oversized_frame_without_ack() {
        use std::net::Shutdown;
        let d = test_daemon();
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        let big = vec![b'x'; MAX_FRAME_BYTES as usize + 2]; // no newline anywhere
                                                            // The daemon may close mid-write once it sees the cap blown;
                                                            // a BrokenPipe here is part of the expected behavior.
        let _ = (&client).write_all(&big);
        let _ = client.shutdown(Shutdown::Write);
        let mut bytes = Vec::new();
        let _ = (&client).read_to_end(&mut bytes);
        assert!(bytes.is_empty(), "no response for oversized frame: {bytes:?}");
        handler.join().unwrap();
    }

    /// A rejected line gets no ack but keeps the connection alive; the
    /// next valid line on the same connection is processed and acked.
    #[test]
    fn handle_client_skips_bad_line_and_acks_next() {
        use std::net::Shutdown;
        let _iso = IsolatedShelbiHome::new("hc-mixed");
        let d = test_daemon();
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        (&client)
            .write_all(
                b"not json at all\n{\"verb\":\"event\",\"project\":\"shelbi\",\"line\":\"note=second\"}\n",
            )
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut bytes = Vec::new();
        (&client).read_to_end(&mut bytes).unwrap();
        assert_eq!(
            bytes,
            shelbi_state::DAEMON_ACK,
            "exactly one ack: {bytes:?}"
        );
        handler.join().unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(log.contains("note=second"), "log: {log}");
    }

    /// F3 (drain half): a client whose connection was accepted just as
    /// shutdown began still gets read, dispatched, and acked before
    /// `serve` returns — the restart window can't silently drop it.
    #[test]
    fn serve_drains_accepted_connection_on_shutdown() {
        use std::net::Shutdown;
        let _iso = IsolatedShelbiHome::new("drain");
        // macOS caps Unix-socket paths at ~104 bytes; keep it short.
        let sock = PathBuf::from(format!("/tmp/shb-drain-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let daemon = test_daemon();
        let stop = Arc::new(AtomicBool::new(false));
        let (stop2, daemon2) = (stop.clone(), daemon.clone());
        let server = thread::spawn(move || serve(&listener, &daemon2, &stop2));

        // Connect (so the accept happens) but hold the write back until
        // after shutdown starts — this is exactly the window the old
        // code lost: stream accepted, stop flag set, process exits with
        // the line unread.
        let client = UnixStream::connect(&sock).unwrap();
        stop.store(true, Ordering::SeqCst);
        let _ = UnixStream::connect(&sock); // the shutdown wake-up poke

        (&client)
            .write_all(b"{\"verb\":\"event\",\"project\":\"shelbi\",\"line\":\"note=drained\"}\n")
            .unwrap();
        client.shutdown(Shutdown::Write).unwrap();
        let mut ack = [0u8; 3];
        (&client).read_exact(&mut ack).unwrap();
        assert_eq!(&ack, shelbi_state::DAEMON_ACK, "legacy ack: {ack:?}");
        server.join().unwrap();

        let log = std::fs::read_to_string(shelbi_state::events_log_path().unwrap()).unwrap();
        assert!(log.contains("note=drained"), "log: {log}");
        assert_eq!(
            log.lines().filter(|line| line.contains("note=drained")).count(),
            1,
            "drained event must land exactly once: {log}"
        );
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn a_subscribed_client_receives_a_pushed_change_notification() {
        // Phase 3 pushed notifications: a `subscribe` frame hands the connection
        // to the streaming loop, and a change published to the in-process bus
        // (as the board refresher and poller do) is written to the client as one
        // NDJSON line. The client never polls.
        let stop = Arc::new(AtomicBool::new(false));
        let d = Daemon::new(DEFAULT_ACK_TIMEOUT, stop.clone());
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        // Subscribe to one project unique to this test. The bus is
        // process-global, so a concurrent test's publish (the board refresher,
        // `append_workspace_event`) also reaches this subscriber; the
        // server-side `project` filter drops those, so the only line that comes
        // back is the one this test publishes. No race on which lands first.
        let project = "serve-subscribe-test";
        (&client)
            .write_all(format!("{{\"verb\":\"subscribe\",\"project\":\"{project}\"}}\n").as_bytes())
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();

        // Publish on a short retry loop so the first publishes that race the
        // handler's subscribe aren't lost — once it has subscribed, the queued
        // notification reads back immediately. Deterministic within ~1s.
        let mut reader = BufReader::new(&client);
        let mut line = String::new();
        let mut got = false;
        for _ in 0..50 {
            shelbi_state::publish_change(shelbi_state::ChangeNotification::Board {
                project: project.into(),
            });
            match reader.read_line(&mut line) {
                Ok(n) if n > 0 => {
                    got = true;
                    break;
                }
                _ => thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(got, "subscriber received a change line");
        assert_eq!(
            line.trim(),
            format!(r#"{{"change":"board","project":"{project}"}}"#)
        );

        // Stop flag unblocks the streaming loop within a poll slice.
        stop.store(true, Ordering::SeqCst);
        handler.join().unwrap();
    }

    #[test]
    fn a_project_scoped_subscriber_ignores_other_projects_changes() {
        // A client that subscribes with a `project` is woken only by that
        // project's changes. A sibling project's churn is dropped server-side,
        // so a per-project sidebar isn't refreshed for a board it isn't showing.
        let stop = Arc::new(AtomicBool::new(false));
        let d = Daemon::new(DEFAULT_ACK_TIMEOUT, stop.clone());
        let (client, server) = UnixStream::pair().unwrap();
        let handler = thread::spawn(move || handle_client(server, &d));

        let mine = "serve-filter-mine";
        let other = "serve-filter-other";
        (&client)
            .write_all(format!("{{\"verb\":\"subscribe\",\"project\":\"{mine}\"}}\n").as_bytes())
            .unwrap();
        client
            .set_read_timeout(Some(Duration::from_millis(200)))
            .unwrap();

        let mut reader = BufReader::new(&client);
        let mut line = String::new();
        let mut got = false;
        for _ in 0..50 {
            // Publish the other project's change first every iteration: if the
            // filter leaked, it would arrive before mine and fail the assert.
            shelbi_state::publish_change(shelbi_state::ChangeNotification::Board {
                project: other.into(),
            });
            shelbi_state::publish_change(shelbi_state::ChangeNotification::Workspace {
                project: mine.into(),
                workspace: "alpha".into(),
            });
            match reader.read_line(&mut line) {
                Ok(n) if n > 0 => {
                    got = true;
                    break;
                }
                _ => thread::sleep(Duration::from_millis(20)),
            }
        }
        assert!(got, "project-scoped subscriber received its own change");
        assert_eq!(
            line.trim(),
            format!(r#"{{"change":"workspace","project":"{mine}","workspace":"alpha"}}"#),
            "only the subscriber's own project is streamed"
        );

        stop.store(true, Ordering::SeqCst);
        handler.join().unwrap();
    }

    #[test]
    fn subscribe_project_filter_parses_optional_project() {
        assert_eq!(
            subscribe_project_filter(r#"{"verb":"subscribe"}"#),
            Some(None),
            "subscribe with no project = all projects"
        );
        assert_eq!(
            subscribe_project_filter(r#"{"verb":"subscribe","project":"p"}"#),
            Some(Some("p".to_string())),
            "subscribe with a project = that project only"
        );
        assert_eq!(
            subscribe_project_filter(r#"{"verb":"event","project":"p"}"#),
            None,
            "a non-subscribe verb is not a subscribe frame"
        );
        assert_eq!(
            subscribe_project_filter("not json"),
            None,
            "a malformed frame is not a subscribe frame"
        );
    }
}
