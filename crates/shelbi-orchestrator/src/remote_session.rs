//! Remote session spawn and transport for the session backend (Phase 5,
//! `rt-remote-spawn`).
//!
//! On the session backend a *remote* workspace is the same `shelbi __session`
//! process a local one is — it just lives on another machine, started over SSH
//! and detached so it outlives the connection, and reached through one **relay
//! per machine** ([`shelbi_client::RelayChannel`], wired by `rt-relay`). This
//! module is the hub side of that: it spawns remote sessions, manages the
//! per-machine relay, and exposes the session operations the
//! [`SessionProcessBackend`](crate::session_process_backend) performs against a
//! remote target (probe, send, snapshot, title, kill, resize, enumerate).
//!
//! ## The SSH seam
//!
//! Every SSH interaction goes through the [`RemoteSsh`] seam so the whole remote
//! path is unit- and integration-testable without a real remote: the production
//! [`SshSeam`] shells out through [`shelbi_ssh`] (inheriting the reverse
//! `hub.sock` forward and the `SHELBI_HUB_ADDR` env prefix every Shelbi `ssh`
//! carries, so worker events still reach the hub unchanged), while a test
//! installs a fake seam via [`set_test_seam`] that spawns sessions locally and
//! bridges an in-process relay.
//!
//! ## Three states, never "dead" on a blip
//!
//! A remote session is dead, alive, or **unreachable**. A relay that cannot be
//! reached ([`shelbi_client::ClientError::RelayUnreachable`], or an SSH child
//! that will not start) yields [`Liveness::Unreachable`] — never
//! [`Liveness::Dead`] — so supervision does not redispatch a workspace whose
//! machine we merely failed to reach.
//!
//! ## Reconnect after an SSH drop
//!
//! The relay is cached per machine. When it dies mid-task the cached channel
//! flips unreachable; the next operation drops it and starts a fresh relay, and
//! the client reconnects to the still-running session by sequence number
//! (handled inside the session/relay stack — the agent keeps running throughout).

use std::collections::HashMap;
use std::io::Read as _;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use shelbi_client::{ClientError, Connection, RelayChannel};
use shelbi_core::{Error, Host, Result};
use shelbi_proto::capability;
use shelbi_session::SpawnSpec;

use crate::session_backend::{Liveness, SlotInfo};

/// Wall-clock bound for the remote `session new` launch. The remote launcher
/// detaches and returns at once (its child's stdio is redirected), so this is a
/// generous upper bound that only fires on a wedged connection.
const LAUNCH_DEADLINE: Duration = Duration::from_secs(60);

// ===========================================================================
// The SSH seam
// ===========================================================================

/// A relay channel plus whatever keeps its transport alive (the SSH child for
/// the production seam, a server thread for a test seam). Dropping it tears the
/// relay down.
pub struct RelayHandle {
    channel: RelayChannel,
    _guard: Box<dyn std::any::Any + Send + Sync>,
}

impl RelayHandle {
    /// Bundle a channel with the guard that owns its transport's lifetime.
    pub fn new(channel: RelayChannel, guard: Box<dyn std::any::Any + Send + Sync>) -> Self {
        Self {
            channel,
            _guard: guard,
        }
    }

    /// The relay channel for issuing requests.
    pub fn channel(&self) -> &RelayChannel {
        &self.channel
    }

    fn reachable(&self) -> bool {
        self.channel.is_reachable()
    }
}

/// Everything the remote session backend does over SSH, behind one seam.
///
/// `launch` starts a detached session on the remote; `open_relay` starts one
/// `shelbi relay` channel bridging every session on the machine. Both take the
/// resolved remote `shelbi` binary path (`bin`).
pub trait RemoteSsh: Send + Sync {
    /// Start a detached remote session running `spec` (via `shelbi session new`
    /// on the remote, which detaches and returns at once). An `Err` means the
    /// launch could not be performed (the host is unreachable or the launcher
    /// reported a failure).
    fn launch(&self, host: &Host, bin: &str, spec: &SpawnSpec) -> Result<()>;

    /// Open a relay channel to every session on `host`, started as
    /// `ssh <host> <bin> relay`.
    fn open_relay(&self, host: &Host, bin: &str) -> Result<RelayHandle>;
}

/// Production [`RemoteSsh`] over the host's `ssh`, via [`shelbi_ssh`].
pub struct SshSeam;

impl RemoteSsh for SshSeam {
    fn launch(&self, host: &Host, bin: &str, spec: &SpawnSpec) -> Result<()> {
        let argv = remote_session_new_argv(bin, spec);
        // `run_with_deadline` goes through `shelbi_ssh`, so the reverse
        // `hub.sock` forward and the `SHELBI_HUB_ADDR` env prefix ride along as
        // for any Shelbi ssh. The remote launcher detaches and returns promptly.
        let out = shelbi_ssh::run_with_deadline(host, &argv, LAUNCH_DEADLINE).map_err(|e| {
            Error::Other(format!(
                "ssh launch of remote session `{}` on `{}` failed: {e}",
                spec.name,
                host_label(host)
            ))
        })?;
        if !out.status.success() {
            return Err(Error::Other(format!(
                "remote session launch for `{}` on `{}` exited {}: {}",
                spec.name,
                host_label(host),
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    fn open_relay(&self, host: &Host, bin: &str) -> Result<RelayHandle> {
        let mut cmd: Command = shelbi_ssh::build_command(host, [bin, "relay"]);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().map_err(|e| {
            Error::Other(format!(
                "starting `ssh {} {bin} relay`: {e}",
                host_label(host)
            ))
        })?;
        let stdout = child.stdout.take().expect("relay stdout piped");
        let stdin = child.stdin.take().expect("relay stdin piped");
        // Drain stderr on a detached thread so a chatty ssh never blocks on a
        // full stderr pipe. Diagnostics are discarded; a dead relay surfaces as
        // an unreachable channel, which is what callers act on.
        if let Some(mut err) = child.stderr.take() {
            std::thread::spawn(move || {
                let mut sink = Vec::new();
                let _ = err.read_to_end(&mut sink);
            });
        }
        let channel = RelayChannel::new(Box::new(stdout), Box::new(stdin))
            .map_err(|e| Error::Other(format!("opening relay channel to `{}`: {e}", host_label(host))))?;
        Ok(RelayHandle::new(
            channel,
            Box::new(SshRelayGuard { child: Some(child) }),
        ))
    }
}

/// Keeps the `ssh … relay` child alive for the channel's lifetime and reaps it
/// on drop so a torn-down relay never leaves an ssh zombie.
struct SshRelayGuard {
    child: Option<Child>,
}

impl Drop for SshRelayGuard {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// The `shelbi session new …` argv the remote launcher runs. Mirrors the
/// [`shelbi session new`](crate) CLI; the trailing child argv comes after `--`.
fn remote_session_new_argv(bin: &str, spec: &SpawnSpec) -> Vec<String> {
    let mut argv = vec![
        bin.to_string(),
        "session".to_string(),
        "new".to_string(),
        "--name".to_string(),
        spec.name.clone(),
        "--cwd".to_string(),
        spec.cwd.to_string_lossy().into_owned(),
        "--cols".to_string(),
        spec.cols.to_string(),
        "--rows".to_string(),
        spec.rows.to_string(),
    ];
    if let Some(task) = &spec.task {
        argv.push("--task".to_string());
        argv.push(task.clone());
    }
    if spec.raw_output_log {
        argv.push("--raw-log".to_string());
    }
    argv.push("--".to_string());
    argv.extend(spec.child_argv.iter().cloned());
    argv
}

// ===========================================================================
// Seam + relay-cache globals (test-overridable)
// ===========================================================================

fn seam_slot() -> &'static Mutex<Option<Arc<dyn RemoteSsh>>> {
    static SEAM: OnceLock<Mutex<Option<Arc<dyn RemoteSsh>>>> = OnceLock::new();
    SEAM.get_or_init(|| Mutex::new(None))
}

/// The active SSH seam — the test override if one is installed, else the
/// production [`SshSeam`].
fn seam() -> Arc<dyn RemoteSsh> {
    seam_slot()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_else(|| Arc::new(SshSeam))
}

/// Install (or clear, with `None`) a fake SSH seam. Test-only: it also clears
/// the relay cache so a stale channel from a previous seam is never reused.
pub fn set_test_seam(seam: Option<Arc<dyn RemoteSsh>>) {
    *seam_slot().lock().unwrap_or_else(|p| p.into_inner()) = seam;
    reset_relays();
}

fn relay_cache() -> &'static Mutex<HashMap<String, Arc<RelayHandle>>> {
    static RELAYS: OnceLock<Mutex<HashMap<String, Arc<RelayHandle>>>> = OnceLock::new();
    RELAYS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop every cached relay (closing its SSH child). Used by tests and when
/// swapping the seam.
pub fn reset_relays() {
    relay_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
}

/// The per-machine cache key (the SSH hostname; `local` for the local host,
/// which never uses this path).
fn host_key(host: &Host) -> String {
    match host {
        Host::Ssh { host } => host.clone(),
        Host::Local => "local".to_string(),
    }
}

/// A human label for a host, for messages.
fn host_label(host: &Host) -> String {
    match host {
        Host::Ssh { host } => host.clone(),
        Host::Local => "local".to_string(),
    }
}

/// Get a reachable relay for `host`, creating it (or replacing a dead one) via
/// the seam. The channel is opened outside the cache lock so a slow SSH start
/// never serializes other machines.
fn get_relay(host: &Host, bin: &str) -> Result<Arc<RelayHandle>> {
    let key = host_key(host);
    {
        let cache = relay_cache().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(handle) = cache.get(&key) {
            if handle.reachable() {
                return Ok(handle.clone());
            }
        }
    }
    // Open a fresh relay without holding the lock.
    let handle = Arc::new(seam().open_relay(host, bin)?);
    let mut cache = relay_cache().lock().unwrap_or_else(|p| p.into_inner());
    // Another thread may have raced us to a reachable channel; prefer it and let
    // ours drop (closing the extra SSH child).
    if let Some(existing) = cache.get(&key) {
        if existing.reachable() {
            return Ok(existing.clone());
        }
    }
    cache.insert(key, handle.clone());
    Ok(handle)
}

/// Forget the cached relay for `host` (its SSH child is reaped on drop).
fn drop_relay(host: &Host) {
    relay_cache()
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .remove(&host_key(host));
}

fn is_unreachable(e: &ClientError) -> bool {
    matches!(e, ClientError::RelayUnreachable)
}

/// Run `f` against a reachable relay for `host`, reconnecting once if the
/// channel turns out to be dead — the SSH-drop recovery path. A relay that
/// cannot be opened at all is reported as [`ClientError::RelayUnreachable`].
fn with_channel<T>(
    host: &Host,
    bin: &str,
    f: impl Fn(&RelayChannel) -> std::result::Result<T, ClientError>,
) -> std::result::Result<T, ClientError> {
    let handle = get_relay(host, bin).map_err(|_| ClientError::RelayUnreachable)?;
    match f(handle.channel()) {
        Err(e) if is_unreachable(&e) => {
            // The relay died mid-request: start a fresh one and retry. The
            // session kept running; the client reconnects by sequence number.
            drop_relay(host);
            let handle = get_relay(host, bin).map_err(|_| ClientError::RelayUnreachable)?;
            f(handle.channel())
        }
        other => other,
    }
}

/// Open a client connection to the live remote session named `name` over the
/// relay `ch`.
fn open_connection(ch: &RelayChannel, name: &str) -> std::result::Result<Connection, ClientError> {
    let session = ch
        .list_sessions()?
        .into_iter()
        .find(|s| s.alive && s.name == name)
        .ok_or_else(|| ClientError::Relay(format!("no live remote session `{name}`")))?;
    let stream = ch.open(&session.short_id)?;
    let (conn, _events) =
        Connection::connect(Box::new(stream), None, &[capability::PASTE, capability::INFO])?;
    Ok(conn)
}

/// Map a relay/client error to the orchestrator [`Error`] with context.
fn to_err(op: &str, host: &Host, name: &str, e: ClientError) -> Error {
    Error::Other(format!(
        "remote {op} on session `{name}` ({}): {e}",
        host_label(host)
    ))
}

// ===========================================================================
// Remote binary resolution
// ===========================================================================

/// The remote `shelbi` binary recorded for `machine` by `shelbi machine setup`.
///
/// `Err` (naming `shelbi machine setup <machine>`) when nothing is recorded or
/// the recorded binary is incompatible with this hub — a dispatch that cannot
/// name a usable remote binary must fail loudly, not silently run the wrong one.
pub fn resolve_remote_bin(machine: &str) -> Result<String> {
    match shelbi_state::machine_state::load_machine_record(machine) {
        Some(rec) if rec.compatible => Ok(rec.path),
        Some(rec) => Err(Error::Other(format!(
            "the `shelbi` recorded for machine `{machine}` is version {} and is incompatible with this hub; \
             run `shelbi machine setup {machine}` to install a compatible build",
            rec.version
        ))),
        None => Err(Error::Other(format!(
            "no `shelbi` binary is recorded for machine `{machine}`; \
             run `shelbi machine setup {machine}` first",
        ))),
    }
}

/// Best-effort remote `shelbi` path for a host, for starting its relay.
///
/// The session backend addresses a target by [`Host`] alone, so it looks up the
/// machine record keyed by the SSH hostname (machine name and host coincide in
/// the common config) and uses its path; failing that it falls back to `shelbi`
/// on the remote PATH. The authoritative compatibility gate runs at dispatch
/// ([`resolve_remote_bin`], which has the machine name), so this only picks the
/// binary to drive once dispatch has cleared it.
pub(crate) fn relay_bin_for_host(host: &Host) -> String {
    match host {
        Host::Ssh { host } => shelbi_state::machine_state::load_machine_record(host)
            .map(|rec| rec.path)
            .unwrap_or_else(|| "shelbi".to_string()),
        Host::Local => "shelbi".to_string(),
    }
}

// ===========================================================================
// Spawn
// ===========================================================================

/// Spawn a detached session running `spec` on `host`, using remote binary
/// `bin`. The session outlives the SSH connection (the remote launcher detaches
/// it with stdio redirected).
pub fn spawn_remote_session(host: &Host, bin: &str, spec: &SpawnSpec) -> Result<()> {
    seam().launch(host, bin, spec)
}

// ===========================================================================
// Relay-backed session operations (what the backend's remote branch calls)
// ===========================================================================

/// Three-state liveness of the remote session named `name`.
///
/// A relay that answers (even "no such session") is definitive: present+alive →
/// [`Liveness::Alive`], otherwise [`Liveness::Dead`]. A relay we cannot reach →
/// [`Liveness::Unreachable`], never `Dead`.
pub fn probe(host: &Host, name: &str) -> Liveness {
    let bin = relay_bin_for_host(host);
    match with_channel(host, &bin, |ch| ch.list_sessions()) {
        Ok(sessions) => {
            if sessions.iter().any(|s| s.name == name && s.alive) {
                Liveness::Alive
            } else {
                Liveness::Dead
            }
        }
        Err(ClientError::RelayUnreachable) => Liveness::Unreachable {
            reason: format!("relay to `{}` is unreachable", host_label(host)),
        },
        Err(e) => Liveness::Unreachable {
            reason: format!("relay to `{}` failed: {e}", host_label(host)),
        },
    }
}

/// Send text (no trailing Enter) to the remote session's input.
pub fn send_text(host: &Host, name: &str, text: &str) -> Result<()> {
    let bin = relay_bin_for_host(host);
    with_channel(host, &bin, |ch| {
        let conn = open_connection(ch, name)?;
        conn.paste(text)
    })
    .map_err(|e| to_err("send-text", host, name, e))
}

/// Send a bare Enter keypress to the remote session.
pub fn send_enter(host: &Host, name: &str) -> Result<()> {
    let bin = relay_bin_for_host(host);
    with_channel(host, &bin, |ch| {
        let conn = open_connection(ch, name)?;
        conn.input(b"\r")
    })
    .map_err(|e| to_err("send-enter", host, name, e))
}

/// Snapshot the remote session's screen (`history_lines` of scrollback, `0` for
/// the visible screen only), in `capture-pane -p -J` shape.
pub fn snapshot(host: &Host, name: &str, history_lines: usize) -> Result<String> {
    let history = if history_lines == 0 {
        None
    } else {
        Some(history_lines as u32)
    };
    let bin = relay_bin_for_host(host);
    with_channel(host, &bin, |ch| {
        let conn = open_connection(ch, name)?;
        conn.snapshot(history).map(|snap| snap.text)
    })
    .map_err(|e| to_err("snapshot", host, name, e))
}

/// The remote session's title (carrying the `shelbi:<state>` worker marker), or
/// empty when the session is gone.
pub fn title(host: &Host, name: &str) -> Result<String> {
    let bin = relay_bin_for_host(host);
    let info = with_channel(host, &bin, |ch| {
        let conn = open_connection(ch, name)?;
        conn.info()
    });
    match info {
        Ok(info) => Ok(info.title.unwrap_or_default().trim_end().to_string()),
        // A dead session has no live title (no running emulator to emit it);
        // report empty, which `parse_pane_title_marker` reads as "no marker".
        Err(ClientError::Relay(_)) => Ok(String::new()),
        Err(e) => Err(to_err("title", host, name, e)),
    }
}

/// Kill the remote session's child process group (best-effort: an already-gone
/// session is fine).
pub fn kill(host: &Host, name: &str) -> Result<()> {
    let bin = relay_bin_for_host(host);
    let result = with_channel(host, &bin, |ch| {
        let conn = open_connection(ch, name)?;
        conn.kill(None)
    });
    match result {
        Ok(()) => Ok(()),
        // Nothing live to kill — already gone.
        Err(ClientError::Relay(_)) => Ok(()),
        Err(e) => Err(to_err("kill", host, name, e)),
    }
}

/// Resize the remote session.
pub fn resize(host: &Host, name: &str, cols: u16, rows: u16) -> Result<()> {
    let bin = relay_bin_for_host(host);
    let result = with_channel(host, &bin, |ch| {
        let conn = open_connection(ch, name)?;
        conn.resize(cols, rows)
    });
    match result {
        Ok(()) => Ok(()),
        Err(ClientError::Relay(_)) => Ok(()),
        Err(e) => Err(to_err("resize", host, name, e)),
    }
}

/// Enumerate the live workspace slots on `host` under `project` — the remote
/// analogue of listing a project session's windows. `None` when the machine
/// could not be asked (distinct from "no sessions").
pub fn enumerate(host: &Host, project: &str) -> Option<Vec<SlotInfo>> {
    let bin = relay_bin_for_host(host);
    let prefix = format!("{project}/ws/");
    match with_channel(host, &bin, |ch| ch.list_sessions()) {
        Ok(sessions) => Some(
            sessions
                .into_iter()
                .filter(|s| s.alive)
                .filter_map(|s| {
                    s.name.strip_prefix(&prefix).map(|workspace| SlotInfo {
                        id: s.name.clone(),
                        name: workspace.to_string(),
                    })
                })
                .collect(),
        ),
        Err(_) => None,
    }
}

/// The logical names of every live session on `host`. `Err` when the machine
/// could not be reached.
pub fn live_session_names(host: &Host) -> Result<Vec<String>> {
    let bin = relay_bin_for_host(host);
    with_channel(host, &bin, |ch| ch.list_sessions())
        .map(|sessions| {
            sessions
                .into_iter()
                .filter(|s| s.alive)
                .map(|s| s.name)
                .collect()
        })
        .map_err(|e| Error::Other(format!("listing remote sessions on `{}`: {e}", host_label(host))))
}

/// Build the default-size `SpawnSpec` for a remote dispatch launch line. The
/// launch already `cd`s into the worktree and sets the hub env before `exec`, so
/// it is run under the login shell (`$SHELL -lc`).
pub fn remote_launch_spec(name: String, worktree: PathBuf, task: Option<String>, launch_line: String) -> SpawnSpec {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
    let (cols, rows) = crate::session_process_backend::SessionProcessBackend::default_size();
    SpawnSpec {
        name,
        cwd: worktree,
        cols,
        rows,
        task,
        raw_output_log: false,
        child_argv: vec![shell, "-lc".to_string(), launch_line],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, version: &str, compatible: bool) -> shelbi_state::machine_state::MachineRecord {
        shelbi_state::machine_state::MachineRecord {
            path: path.to_string(),
            version: version.to_string(),
            compatible,
            source: shelbi_state::machine_state::SOURCE_PATH.to_string(),
            checked_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn remote_session_new_argv_is_the_session_new_cli() {
        let spec = SpawnSpec {
            name: "demo/ws/alice".into(),
            cwd: PathBuf::from("/home/u/wt/alice"),
            cols: 120,
            rows: 40,
            task: Some("t-7".into()),
            raw_output_log: false,
            child_argv: vec!["/bin/sh".into(), "-lc".into(), "exec claude".into()],
        };
        let argv = remote_session_new_argv("/home/u/.shelbi/bin/shelbi", &spec);
        assert_eq!(
            argv,
            vec![
                "/home/u/.shelbi/bin/shelbi",
                "session",
                "new",
                "--name",
                "demo/ws/alice",
                "--cwd",
                "/home/u/wt/alice",
                "--cols",
                "120",
                "--rows",
                "40",
                "--task",
                "t-7",
                "--",
                "/bin/sh",
                "-lc",
                "exec claude",
            ]
        );
    }

    #[test]
    fn remote_session_new_argv_omits_optional_flags() {
        let spec = SpawnSpec {
            name: "demo/orch".into(),
            cwd: PathBuf::from("/tmp"),
            cols: 80,
            rows: 24,
            task: None,
            raw_output_log: false,
            child_argv: vec!["codex".into()],
        };
        let argv = remote_session_new_argv("shelbi", &spec);
        assert!(!argv.iter().any(|a| a == "--task"));
        assert!(!argv.iter().any(|a| a == "--raw-log"));
        assert_eq!(argv.last().unwrap(), "codex");
    }

    #[test]
    fn resolve_remote_bin_requires_a_compatible_record() {
        let _g = crate::test_lock::acquire();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", home.path());

        // Nothing recorded → error names `shelbi machine setup <machine>`.
        let err = resolve_remote_bin("devbox").unwrap_err().to_string();
        assert!(err.contains("shelbi machine setup devbox"), "{err}");

        // Incompatible → same remedy, naming the version.
        shelbi_state::machine_state::save_machine_record(
            "devbox",
            Some(rec("/usr/bin/shelbi", "0.1.0", false)),
        )
        .unwrap();
        let err = resolve_remote_bin("devbox").unwrap_err().to_string();
        assert!(err.contains("shelbi machine setup devbox"), "{err}");
        assert!(err.contains("0.1.0"), "{err}");

        // Compatible → the recorded path.
        shelbi_state::machine_state::save_machine_record(
            "devbox",
            Some(rec("/home/u/.shelbi/bin/shelbi", "0.9.0", true)),
        )
        .unwrap();
        assert_eq!(
            resolve_remote_bin("devbox").unwrap(),
            "/home/u/.shelbi/bin/shelbi"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn relay_bin_falls_back_to_path_shelbi_without_a_record() {
        let _g = crate::test_lock::acquire();
        let home = tempfile::tempdir().unwrap();
        std::env::set_var("SHELBI_HOME", home.path());
        assert_eq!(
            relay_bin_for_host(&Host::Ssh { host: "unknown".into() }),
            "shelbi"
        );
        shelbi_state::machine_state::save_machine_record(
            "box",
            Some(rec("/opt/shelbi", "0.9.0", true)),
        )
        .unwrap();
        assert_eq!(
            relay_bin_for_host(&Host::Ssh { host: "box".into() }),
            "/opt/shelbi"
        );
        std::env::remove_var("SHELBI_HOME");
    }
}
