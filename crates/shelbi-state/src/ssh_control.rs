//! Hub-side SSH ControlMaster bookkeeping plus the daemon PID file.
//!
//! The hub maintains a long-lived outbound SSH ControlMaster per remote
//! host so each follow-up `ssh <host> ...` reuses a single TCP+TLS+auth
//! channel. We park those masters under `$SHELBI_HOME/ssh/` (default
//! `~/.shelbi/ssh/`) — distinct from `~/.ssh/` so the cleanup walk here
//! can't accidentally touch the user's hand-rolled CMs — and probe each
//! one at hub startup, removing any whose master process has died.
//!
//! The PID file at `$SHELBI_HOME/shelbi.pid` is the hub daemon's
//! aliveness beacon. The cleanup pass refuses to touch the directory
//! when the PID file points at a still-running shelbi process other
//! than us — that's another live daemon's CMs and they belong to it.
//! To survive PID reuse the file records a process *identity* token (the
//! recorded process's start-time) alongside the bare PID: a recycled PID
//! whose start-time no longer matches is recognized as NOT the daemon,
//! so cleanup isn't skipped forever (Shelbi ContextStore
//! docs/planning:reviews/adversarial-2026-07/state-runtime.md F5).

use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::OnceLock;

use shelbi_core::Result;

use crate::shelbi_home;

/// Directory holding the per-host SSH ControlMaster sockets:
/// `$SHELBI_HOME/ssh/`. Each file inside is named by OpenSSH's `%C`
/// connection hash (see [`ssh_control_path_template`]) — a fixed-length
/// digest that keeps the socket path under the `sun_path` cap even for
/// long home/user/host combinations.
///
/// Lives under SHELBI_HOME (not `~/.ssh/`) so the startup cleanup can
/// scan and prune without risking the user's own ControlMasters.
pub fn ssh_control_dir() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("ssh"))
}

/// Ensure [`ssh_control_dir`] exists with `0700` permissions. Idempotent —
/// creates the directory on first call and re-applies perms if they've
/// drifted.
///
/// Called from [`shelbi-ssh`](../../../shelbi-ssh/index.html) before every
/// outbound SSH invocation and from `ensure_root_subdirs` so a fresh
/// install ships with the directory in place. Without this, the first
/// hub→remote `ssh -o ControlPath=…` fails with
/// `unix_listener: cannot bind to path …: No such file or directory`
/// because OpenSSH won't create the ControlPath's parent for us.
pub fn ensure_ssh_control_dir() -> Result<()> {
    let dir = ssh_control_dir()?;
    fs::create_dir_all(&dir)?;
    // 0700: the sockets are only meaningful to the local user; opening
    // the directory up would let another local user hijack the master.
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// String passed to OpenSSH's `-o ControlPath=…`. Uses the `%C` token —
/// OpenSSH's hash of `%l%h%p%r` (local host, remote host, port, remote
/// user) — rather than the human-readable `%r@%h`, so the expanded path
/// is a fixed short length regardless of how long `$SHELBI_HOME`, the
/// username, or the FQDN are. `%r@%h` could push the path past macOS's
/// ~104-byte `sun_path` cap (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/process-boundaries.md F12), which fails every
/// SSH invocation with `ControlPath too long`. The directory matches
/// [`ssh_control_dir`] so this module's cleanup logic and ssh's runtime
/// agree on the path; the cleanup probes sockets by `connect()`, not by
/// filename, so the opaque `%C` digest doesn't affect it.
pub fn ssh_control_path_template() -> Result<String> {
    let dir = ssh_control_dir()?;
    Ok(format!("{}/%C", dir.display()))
}

/// Remote-side landing path for the hub socket's reverse forward.
/// `$SHELBI_REMOTE_HUB_SOCK` wins so remote workers (Phase 5) can be
/// pointed at the same value via env without rebuilding the binary.
///
/// Default `/tmp/shelbi-hub-<uid>-<pid>-<start>.sock`. The uid suffix keeps
/// different local users who both reverse-forward to the same shared remote
/// host from colliding, and the per-process token keeps stale sockets from a
/// crashed/reloaded hub from poisoning the next hub process. The uid is the
/// *local* caller's — computed the same way on every consumer (`spawn`, the
/// `-R` spec builder) so both ends of a given forward always agree.
pub fn remote_hub_socket_path() -> PathBuf {
    if let Some(p) = std::env::var_os("SHELBI_REMOTE_HUB_SOCK") {
        return PathBuf::from(p);
    }
    PathBuf::from(format!(
        "/tmp/shelbi-hub-{}.sock",
        remote_hub_socket_token()
    ))
}

fn remote_hub_socket_token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        let uid = unsafe { libc::getuid() };
        let pid = std::process::id();
        let start = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{uid}-{pid}-{start:x}")
    })
}

/// Daemon PID file: `$SHELBI_HOME/shelbi.pid`. Used by the cleanup
/// pass to detect an already-running shelbi daemon whose CMs should
/// be left alone.
pub fn daemon_pid_file_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("shelbi.pid"))
}

/// A parsed daemon PID file: the recorded PID plus the process
/// start-time token captured when the daemon wrote it.
///
/// `start_time` is `None` for a legacy single-field file (written before
/// this crate recorded identity) or when the writing daemon's start-time
/// couldn't be read. In that case identity can't be verified and callers
/// fall back to a bare liveness probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonPidRecord {
    pub pid: libc::pid_t,
    pub start_time: Option<u64>,
}

/// Read and parse `$SHELBI_HOME/shelbi.pid`. Returns `Ok(None)` when the
/// file is missing OR its first field is an unparseable PID — a torn or
/// empty file from a crashed write is treated the same as "no previous
/// daemon recorded itself."
///
/// File layout is `<pid>` or `<pid> <start_time>` (whitespace-separated,
/// one line). A missing or unparseable second field yields
/// `start_time: None`, keeping the reader compatible with legacy files.
pub fn read_daemon_pid_record() -> Result<Option<DaemonPidRecord>> {
    let path = daemon_pid_file_path()?;
    let text = match fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(shelbi_core::Error::Io(e)),
    };
    let mut fields = text.split_whitespace();
    let pid = match fields.next().and_then(|f| f.parse::<libc::pid_t>().ok()) {
        Some(p) => p,
        None => return Ok(None),
    };
    let start_time = fields.next().and_then(|f| f.parse::<u64>().ok());
    Ok(Some(DaemonPidRecord { pid, start_time }))
}

/// Read just the PID stored in `$SHELBI_HOME/shelbi.pid`. Thin wrapper
/// over [`read_daemon_pid_record`] for callers that don't need the
/// identity token. Returns `Ok(None)` on a missing or torn file.
pub fn read_daemon_pid() -> Result<Option<libc::pid_t>> {
    Ok(read_daemon_pid_record()?.map(|r| r.pid))
}

/// Atomically write our PID — and, when we can read it, our start-time
/// identity token — to `$SHELBI_HOME/shelbi.pid`. Recording the
/// start-time lets the next startup's cleanup tell a genuine live daemon
/// apart from an unrelated process that recycled the same PID.
pub fn write_daemon_pid(pid: libc::pid_t) -> Result<()> {
    let path = daemon_pid_file_path()?;
    let body = match process_start_time(pid) {
        Some(start) => format!("{pid} {start}\n"),
        None => format!("{pid}\n"),
    };
    crate::atomic_write(&path, body.as_bytes())
}

/// Best-effort unlink of the daemon PID file. Missing file is not an
/// error — clean shutdown after a never-fully-started daemon must not
/// surface a spurious failure.
pub fn remove_daemon_pid_file() -> Result<()> {
    let path = daemon_pid_file_path()?;
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(shelbi_core::Error::Io(e)),
    }
}

/// Does a process with `pid` currently exist? Implemented via
/// `kill(pid, 0)` — POSIX's standard "no-op signal, but still does the
/// permission/existence check." A `0` return means yes, the process is
/// alive (or at least known to the kernel). `ESRCH` means no such
/// process; `EPERM` means it exists but we can't signal it (treated as
/// alive — different user, but still a real process).
pub fn is_process_alive(pid: libc::pid_t) -> bool {
    if pid <= 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) is signal-safe and only reads kernel state.
    let rc = unsafe { libc::kill(pid, 0) };
    if rc == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// A per-process start-time token, used to distinguish a genuine live
/// process from an unrelated one that later recycled the same PID.
/// Encoded as start-seconds×10⁶ + start-microseconds on macOS and as the
/// kernel's `starttime` clock-tick field on Linux — the absolute unit
/// doesn't matter, only that the value is stable for a process's lifetime
/// and (practically) never collides across a PID's reuse.
///
/// Returns `None` when the process doesn't exist or its start-time can't
/// be read; callers treat that as "identity unknown" and fall back to a
/// bare liveness probe rather than guessing.
#[cfg(target_os = "macos")]
fn process_start_time(pid: libc::pid_t) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as libc::c_int;
    // SAFETY: proc_pidinfo writes at most `size` bytes into `info`, which
    // is exactly `size` bytes of owned, zeroed storage.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            &mut info as *mut _ as *mut libc::c_void,
            size,
        )
    };
    // proc_pidinfo returns the number of bytes filled; a short/zero/error
    // return means we couldn't read the record.
    if n != size {
        return None;
    }
    Some(
        info.pbi_start_tvsec
            .wrapping_mul(1_000_000)
            .wrapping_add(info.pbi_start_tvusec),
    )
}

/// Linux: field 22 (`starttime`) of `/proc/<pid>/stat`, in clock ticks
/// since boot. The `comm` field (2) is parenthesised and may itself
/// contain spaces or `)`, so we split on the *last* `)` before tokenising
/// the remaining, space-clean fields — `starttime` is the 20th of those.
#[cfg(target_os = "linux")]
fn process_start_time(pid: libc::pid_t) -> Option<u64> {
    if pid <= 0 {
        return None;
    }
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = &stat[stat.rfind(')')? + 1..];
    // After comm the fields are state(3) ppid(4) … starttime(22); the
    // slice starts at field 3, so starttime is index 22-3 = 19.
    after_comm.split_whitespace().nth(19)?.parse::<u64>().ok()
}

/// Fallback for platforms where we don't have a start-time probe: report
/// identity as unknown so callers degrade to a bare liveness check.
#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn process_start_time(_pid: libc::pid_t) -> Option<u64> {
    None
}

/// Is the daemon recorded in the PID file still the *same* running
/// process? A bare [`is_process_alive`] probe isn't enough: after PID
/// reuse the recorded PID may belong to an unrelated process, and
/// treating that as "another daemon" would skip ControlMaster cleanup
/// forever (Shelbi ContextStore
/// docs/planning:reviews/adversarial-2026-07/state-runtime.md F5).
///
/// When the record carries a start-time and we can read the current
/// process's start-time, they must match — a mismatch means the PID was
/// recycled to something that isn't our daemon. If either start-time is
/// unavailable (a legacy file, or a live process we can't introspect) we
/// conservatively fall back to liveness so we never nuke a real daemon's
/// ControlMasters on a probe we couldn't complete.
fn is_recorded_daemon_alive(rec: DaemonPidRecord) -> bool {
    if !is_process_alive(rec.pid) {
        return false;
    }
    match (rec.start_time, process_start_time(rec.pid)) {
        (Some(recorded), Some(current)) => recorded == current,
        _ => true,
    }
}

/// Outcome of [`cleanup_stale_control_masters`] — surfaced so the
/// daemon can log a one-liner instead of guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmCleanupOutcome {
    /// Another live shelbi process owns the CMs; we left them alone.
    SkippedAnotherDaemon { pid: libc::pid_t },
    /// Scanned the dir; `removed` is the count of orphaned socket
    /// files we unlinked, `kept` the count of sockets still answering
    /// a `connect()` (live masters).
    Scanned { removed: usize, kept: usize },
}

/// Walk `$SHELBI_HOME/ssh/` and remove socket files whose
/// ControlMaster is no longer alive (a `connect()` against the socket
/// fails with ENOENT, ECONNREFUSED, or similar).
///
/// `self_pid` is the calling process's PID — used to decide whether
/// the PID-file owner is "us" (cleanup proceeds) or another live
/// shelbi process (cleanup skipped). Pass `std::process::id() as i32`.
///
/// Live sockets (connect succeeded) are left in place. Non-socket
/// files in the directory are skipped silently — a stray text file
/// shouldn't keep the daemon from starting.
pub fn cleanup_stale_control_masters(self_pid: libc::pid_t) -> Result<CmCleanupOutcome> {
    if let Some(rec) = read_daemon_pid_record()? {
        if rec.pid != self_pid && is_recorded_daemon_alive(rec) {
            return Ok(CmCleanupOutcome::SkippedAnotherDaemon { pid: rec.pid });
        }
    }

    let dir = ssh_control_dir()?;
    if !dir.exists() {
        return Ok(CmCleanupOutcome::Scanned {
            removed: 0,
            kept: 0,
        });
    }

    let mut removed = 0;
    let mut kept = 0;
    for entry in fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let ft = match entry.file_type() {
            Ok(f) => f,
            Err(_) => continue,
        };
        // Skip subdirectories outright. Skip regular files that aren't
        // sockets — a stray text file (e.g. an editor swap) shouldn't
        // get unlinked just for being in the dir.
        if ft.is_dir() {
            continue;
        }
        if !is_socket(&path) {
            continue;
        }
        if is_socket_alive(&path) {
            kept += 1;
            continue;
        }
        match fs::remove_file(&path) {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // Vanished between stat and unlink — fine.
            }
            Err(e) => {
                tracing::warn!(
                    path = %path.display(),
                    error = %e,
                    "shelbi: failed to remove stale ControlMaster socket"
                );
            }
        }
    }
    Ok(CmCleanupOutcome::Scanned { removed, kept })
}

/// Best-effort: is this path a Unix-domain socket? Falls back to
/// `false` on any stat error so the cleanup loop treats "we don't
/// know" as "leave it alone."
fn is_socket(path: &std::path::Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_socket())
        .unwrap_or(false)
}

/// Probe the socket by opening a connection: a live ControlMaster has
/// a listener and `connect()` succeeds; an orphaned socket file
/// (master died, file leaked) gives ENOENT/ECONNREFUSED. Any other
/// error (permission denied, etc.) is conservatively treated as
/// "alive" so we don't unlink something we can't even probe.
fn is_socket_alive(path: &std::path::Path) -> bool {
    match UnixStream::connect(path) {
        Ok(_) => true,
        Err(e) => !matches!(
            e.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        ),
    }
}

/// Hint at the canonical bind argument for re-establishing the
/// reverse forward when shelling out. Returned as `OsString` so
/// callers can hand it straight to `Command::arg`.
///
/// Layout: `<remote_sock>:<local_hub_sock>` — exactly what ssh's `-R`
/// flag expects.
pub fn reverse_forward_spec() -> Result<OsString> {
    let remote = remote_hub_socket_path();
    let local = crate::hub_socket_path()?;
    let mut s = OsString::new();
    s.push(remote);
    s.push(":");
    s.push(local);
    Ok(s)
}

/// The `-R` argument for a **TCP loopback** reverse forward: OpenSSH binds
/// `127.0.0.1:<port>` on the remote and forwards it onto the hub's local
/// `hub.sock`. This is the fallback shape for hosts (Tailscale SSH) where the
/// Unix-socket landing path lands root-owned and unusable.
///
/// Layout: `127.0.0.1:<port>:<local_hub_sock>` — the remote-TCP → local-Unix
/// form of ssh's `-R`.
pub fn reverse_forward_spec_tcp(port: u16) -> Result<OsString> {
    let local = crate::hub_socket_path()?;
    let mut s = OsString::from(format!("{TCP_FORWARD_BIND_ADDR}:{port}:"));
    s.push(local);
    Ok(s)
}

/// Loopback address the TCP reverse forward binds on the remote. Loopback (not
/// `*`) keeps the forwarded listener reachable only from the remote host
/// itself — a worker in a pane there — never from the wider tailnet/LAN.
pub const TCP_FORWARD_BIND_ADDR: &str = "127.0.0.1";

/// First candidate port for a TCP loopback reverse forward, and the size of
/// the band we sweep on a bind collision. Ports are picked deterministically
/// from `[TCP_FORWARD_PORT_BASE, TCP_FORWARD_PORT_BASE + TCP_FORWARD_PORT_SPAN)`
/// so a re-established forward reuses the same port a worker was told about
/// whenever it's still free.
///
/// These are the *defaults*: [`tcp_forward_port_base`] and
/// [`tcp_forward_port_span`] read env overrides on top of them. The span is
/// deliberately roomy — a narrow band on a busy remote (many concurrent
/// workers, orphaned masters from a hard hub kill still holding ports until
/// their ControlPersist expires) is exactly the "no free loopback port in
/// band" exhaustion we widened it to avoid.
pub const TCP_FORWARD_PORT_BASE: u16 = 47100;
pub const TCP_FORWARD_PORT_SPAN: u16 = 64;

/// The first candidate loopback port, honoring the `SHELBI_TCP_FORWARD_PORT_BASE`
/// override. A malformed or zero value falls back to [`TCP_FORWARD_PORT_BASE`]
/// so a typo in the environment can never wedge the forward path.
pub fn tcp_forward_port_base() -> u16 {
    parse_env_u16("SHELBI_TCP_FORWARD_PORT_BASE")
        .filter(|&p| p > 0)
        .unwrap_or(TCP_FORWARD_PORT_BASE)
}

/// The width of the loopback band swept on a bind collision, honoring the
/// `SHELBI_TCP_FORWARD_PORT_SPAN` override. Falls back to
/// [`TCP_FORWARD_PORT_SPAN`] on a malformed/zero value, and is clamped so the
/// band `[base, base + span)` can never run past `u16::MAX` — otherwise the
/// sweep in `tcp_candidate_ports` would overflow when adding `base + i`.
pub fn tcp_forward_port_span() -> u16 {
    let base = tcp_forward_port_base();
    let span = parse_env_u16("SHELBI_TCP_FORWARD_PORT_SPAN")
        .filter(|&s| s > 0)
        .unwrap_or(TCP_FORWARD_PORT_SPAN);
    // Keep base + span - 1 <= u16::MAX so the deterministic sweep stays in range.
    let max_span = (u16::MAX - base).saturating_add(1);
    span.min(max_span).max(1)
}

/// Parse a `u16` from an environment variable, tolerating surrounding
/// whitespace. `None` on absent or unparseable values — callers substitute a
/// safe default rather than surfacing an error.
fn parse_env_u16(key: &str) -> Option<u16> {
    std::env::var(key).ok()?.trim().parse::<u16>().ok()
}

/// Persisted forward decision for a single remote host: which mode the hub
/// settled on, plus the loopback port a TCP forward bound (so the next
/// forward and the worker env agree on it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostForward {
    pub mode: shelbi_core::ForwardMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

/// State file recording the per-host forward decisions:
/// `$SHELBI_HOME/forward-modes.json`. A flat `{ "<host>": {mode, port} }` map,
/// small enough to read-modify-write atomically on each update.
pub fn forward_state_path() -> Result<PathBuf> {
    Ok(shelbi_home()?.join("forward-modes.json"))
}

/// Read the whole forward-mode map. Best-effort: a missing or unparseable file
/// yields an empty map so a torn write never wedges the forward path — the
/// worst case is re-running detection.
pub fn load_forward_state() -> std::collections::HashMap<String, HostForward> {
    let path = match forward_state_path() {
        Ok(p) => p,
        Err(_) => return std::collections::HashMap::new(),
    };
    match fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_default(),
        Err(_) => std::collections::HashMap::new(),
    }
}

/// The remembered forward decision for `host`, if any. `None` means detection
/// hasn't run (or was reset) and the caller should start from the Unix default.
pub fn load_host_forward(host: &str) -> Option<HostForward> {
    load_forward_state().get(host).copied()
}

/// Record (or clear) the forward decision for `host`. Read-modify-write against
/// the whole map so a concurrent update for a *different* host isn't lost.
/// Passing `None` forgets the host — used to reset back to auto-detection.
pub fn save_host_forward(host: &str, decision: Option<HostForward>) -> Result<()> {
    let mut map = load_forward_state();
    match decision {
        Some(hf) => {
            map.insert(host.to_string(), hf);
        }
        None => {
            map.remove(host);
        }
    }
    let body = serde_json::to_vec_pretty(&map).map_err(|e| {
        shelbi_core::Error::Other(format!("serializing forward-mode state: {e}"))
    })?;
    let path = forward_state_path()?;
    crate::atomic_write(&path, &body)
}

/// Where a *worker* on a remote host reaches the hub daemon — the endpoint the
/// hub's `-R` forward lands on. Resolved from the persisted per-host decision:
/// a host in TCP mode (with a bound port) yields [`HubEndpoint::Tcp`], everyone
/// else the default Unix landing socket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HubEndpoint {
    Unix(PathBuf),
    Tcp { addr: String, port: u16 },
}

impl HubEndpoint {
    /// The scheme-tagged value for the worker's `SHELBI_HUB_ADDR` env var, so
    /// the pane snippet can dispatch a Unix vs TCP connect without re-deriving
    /// anything: `unix:<path>` or `tcp:<addr>:<port>`.
    pub fn addr_env_value(&self) -> String {
        match self {
            HubEndpoint::Unix(p) => format!("unix:{}", p.display()),
            HubEndpoint::Tcp { addr, port } => format!("tcp:{addr}:{port}"),
        }
    }
}

/// Resolve the hub endpoint a worker on `host` should connect to, honoring the
/// persisted forward decision. Defaults to the Unix landing socket
/// ([`remote_hub_socket_path`]) unless the host has settled on TCP with a bound
/// port.
pub fn remote_hub_endpoint(host: &str) -> HubEndpoint {
    match load_host_forward(host) {
        Some(HostForward {
            mode: shelbi_core::ForwardMode::Tcp,
            port: Some(port),
        }) => HubEndpoint::Tcp {
            addr: TCP_FORWARD_BIND_ADDR.to_string(),
            port,
        },
        _ => HubEndpoint::Unix(remote_hub_socket_path()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::LOCK as TEST_LOCK;
    use std::os::unix::net::UnixListener;
    use std::sync::MutexGuard;

    /// Tolerate a poisoned mutex — a previous test panicked while
    /// holding the lock and we still want the rest of the suite to
    /// run instead of cascade-failing on PoisonError.
    fn lock_test() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Short-path temp dir. macOS caps unix-socket paths at ~104 bytes
    /// (SUN_LEN), and `std::env::temp_dir()` lands under
    /// `/var/folders/.../T/` which leaves no room for our `ssh/` +
    /// `user@host` suffix. `/tmp/` plus a brief slug keeps us well
    /// under the limit on every supported platform.
    fn fresh_home() -> PathBuf {
        let p = PathBuf::from(format!(
            "/tmp/shls-{}-{:x}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn control_dir_lives_under_shelbi_home() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert_eq!(ssh_control_dir().unwrap(), home.join("ssh"));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn ensure_ssh_control_dir_creates_missing_dir_with_0700() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = ssh_control_dir().unwrap();
        // Precondition: fresh home, no ssh/ subdir yet — mirrors the
        // fresh-install case that motivated this helper.
        assert!(!dir.exists(), "test setup: ssh/ dir should not exist yet");

        ensure_ssh_control_dir().unwrap();

        assert!(dir.is_dir(), "ssh/ dir should have been created");
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ssh/ dir should be 0700, got {mode:o}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn ensure_ssh_control_dir_is_idempotent_and_reasserts_perms() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let dir = ssh_control_dir().unwrap();

        // First call creates. Second call is a no-op on already-correct
        // state. Both must succeed — daemon startup and every SSH
        // invocation call this, so a spurious failure would wedge us.
        ensure_ssh_control_dir().unwrap();
        ensure_ssh_control_dir().unwrap();
        assert!(dir.is_dir());

        // Drift perms open (0755 — what create_dir_all defaults to) and
        // confirm the helper snaps them back to 0700. Guards against a
        // future refactor that removes the set_permissions call.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
        ensure_ssh_control_dir().unwrap();
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700, "ssh/ dir perms should have been reset to 0700");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn control_path_template_uses_connection_hash_token() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let tpl = ssh_control_path_template().unwrap();
        assert!(tpl.ends_with("/ssh/%C"), "got: {tpl}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn remote_hub_socket_defaults_to_per_session_tmp_and_env_overrides() {
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        let uid = unsafe { libc::getuid() };
        let path = remote_hub_socket_path();
        let s = path.to_string_lossy();
        assert!(
            s.starts_with(&format!("/tmp/shelbi-hub-{uid}-")),
            "got: {s}"
        );
        assert!(s.ends_with(".sock"), "got: {s}");
        assert_eq!(
            path,
            remote_hub_socket_path(),
            "path must be stable in-process"
        );
        std::env::set_var("SHELBI_REMOTE_HUB_SOCK", "/run/foo.sock");
        assert_eq!(remote_hub_socket_path(), PathBuf::from("/run/foo.sock"));
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
    }

    #[test]
    fn pid_file_round_trips_and_remove_is_idempotent() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        assert_eq!(read_daemon_pid().unwrap(), None);
        write_daemon_pid(12345).unwrap();
        assert_eq!(read_daemon_pid().unwrap(), Some(12345));
        remove_daemon_pid_file().unwrap();
        assert_eq!(read_daemon_pid().unwrap(), None);
        // Second remove on the now-missing file is still Ok — a clean
        // shutdown after a never-fully-started daemon would otherwise
        // leak a spurious error.
        remove_daemon_pid_file().unwrap();
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn read_daemon_pid_returns_none_for_garbage_contents() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        fs::write(home.join("shelbi.pid"), "not-a-pid\n").unwrap();
        // Garbage parses to None — treated as "no recorded daemon",
        // not a hard error. The torn-write recovery path needs this.
        assert_eq!(read_daemon_pid().unwrap(), None);
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn is_process_alive_recognizes_self_and_pid_one() {
        // Self is always alive — kill(getpid(), 0) succeeds.
        assert!(is_process_alive(std::process::id() as libc::pid_t));
        // pid 1 (init) is always present on a running system. On
        // macOS launchd / on Linux systemd. EPERM here is treated as
        // alive by design.
        assert!(is_process_alive(1));
        // Sentinel: 0 and negatives never refer to a real process.
        assert!(!is_process_alive(0));
        assert!(!is_process_alive(-1));
    }

    #[test]
    fn process_start_time_is_readable_and_stable_for_self() {
        // On the platforms we support (macOS/Linux) our own start-time
        // must be readable — the identity check depends on it. A second
        // read is stable (the value doesn't drift for a live process).
        let self_pid = std::process::id() as libc::pid_t;
        let a = process_start_time(self_pid);
        assert!(a.is_some(), "own start-time should be readable");
        assert_eq!(a, process_start_time(self_pid), "start-time must be stable");
        // Sentinels never name a real process.
        assert_eq!(process_start_time(0), None);
        assert_eq!(process_start_time(-1), None);
    }

    #[test]
    fn write_daemon_pid_records_start_time_for_a_live_process() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Writing OUR pid captures a start-time token (we're alive), so
        // the record round-trips with identity present.
        let self_pid = std::process::id() as libc::pid_t;
        write_daemon_pid(self_pid).unwrap();
        let rec = read_daemon_pid_record().unwrap().unwrap();
        assert_eq!(rec.pid, self_pid);
        assert_eq!(rec.start_time, process_start_time(self_pid));
        assert!(rec.start_time.is_some());
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn legacy_pid_only_file_parses_with_no_identity() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // A single-field file (the pre-identity format) still parses; the
        // identity token is simply absent.
        fs::write(home.join("shelbi.pid"), "4242\n").unwrap();
        let rec = read_daemon_pid_record().unwrap().unwrap();
        assert_eq!(rec.pid, 4242);
        assert_eq!(rec.start_time, None);
        // The pid-only accessor still works too.
        assert_eq!(read_daemon_pid().unwrap(), Some(4242));
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn recycled_pid_with_wrong_identity_is_treated_as_stale() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let ssh_dir = ssh_control_dir().unwrap();
        fs::create_dir_all(&ssh_dir).unwrap();

        // An orphan socket that a correct cleanup pass must unlink.
        let orphan = ssh_dir.join("user@orphan");
        {
            let _l = UnixListener::bind(&orphan).unwrap();
        }
        assert!(!is_socket_alive(&orphan));

        // Fixture: the PID file names a *live* process (this test), but
        // pairs it with a start-time that can't match — the hallmark of a
        // recycled PID that now belongs to an unrelated process. The old
        // bare kill-0 check would treat this as "another daemon" and skip
        // cleanup forever (Shelbi ContextStore
        // docs/planning:reviews/adversarial-2026-07/state-runtime.md F5).
        let self_pid = std::process::id() as libc::pid_t;
        let real = process_start_time(self_pid).expect("own start-time readable");
        let bogus = real ^ 0xDEAD_BEEF;
        assert_ne!(bogus, real);
        fs::write(home.join("shelbi.pid"), format!("{self_pid} {bogus}\n")).unwrap();

        // Pass a *different* self_pid so we exercise the identity path
        // rather than the "that PID is us" short-circuit. Identity
        // mismatch → recycled → cleanup proceeds and the orphan is gone.
        let outcome = cleanup_stale_control_masters(self_pid + 1).unwrap();
        assert_eq!(
            outcome,
            CmCleanupOutcome::Scanned {
                removed: 1,
                kept: 0
            }
        );
        assert!(!orphan.exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn genuine_live_daemon_with_matching_identity_is_detected() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let ssh_dir = ssh_control_dir().unwrap();
        fs::create_dir_all(&ssh_dir).unwrap();

        // Orphan that must be LEFT ALONE while another daemon owns the dir.
        let orphan = ssh_dir.join("user@orphan");
        {
            let _l = UnixListener::bind(&orphan).unwrap();
        }

        // Record this process with its true start-time — a genuine live
        // daemon. write_daemon_pid captures the matching identity token.
        let self_pid = std::process::id() as libc::pid_t;
        write_daemon_pid(self_pid).unwrap();

        // A different caller PID → identity check fires, start-times match,
        // so we recognise a live daemon and skip cleanup.
        let outcome = cleanup_stale_control_masters(self_pid + 1).unwrap();
        assert_eq!(
            outcome,
            CmCleanupOutcome::SkippedAnotherDaemon { pid: self_pid }
        );
        assert!(orphan.exists(), "another daemon's socket must be untouched");

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn cleanup_removes_orphan_keeps_live_and_skips_when_another_daemon() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let ssh_dir = ssh_control_dir().unwrap();
        fs::create_dir_all(&ssh_dir).unwrap();

        // Orphan: socket file with no listener (simulated by a regular
        // file). We need an actual socket node, not a plain file, so the
        // is_socket() check passes. Bind+drop a listener: the path now
        // refers to a closed socket file — a connect() returns
        // ECONNREFUSED.
        let orphan = ssh_dir.join("user@orphan");
        {
            let _l = UnixListener::bind(&orphan).unwrap();
        }
        // After drop the listener is gone but the file remains. Sanity:
        // confirm probe sees it as dead.
        assert!(orphan.exists());
        assert!(!is_socket_alive(&orphan));

        // Live: bind a listener and keep it. The handle stays in scope
        // for the duration of the test.
        let live = ssh_dir.join("user@live");
        let _live_listener = UnixListener::bind(&live).unwrap();
        assert!(is_socket_alive(&live));

        // Non-socket files in the directory are left alone.
        let stray = ssh_dir.join("README");
        fs::write(&stray, "not a socket").unwrap();

        // No PID file → cleanup proceeds.
        let outcome = cleanup_stale_control_masters(std::process::id() as libc::pid_t).unwrap();
        assert_eq!(
            outcome,
            CmCleanupOutcome::Scanned {
                removed: 1,
                kept: 1
            }
        );
        assert!(!orphan.exists());
        assert!(live.exists());
        assert!(stray.exists());

        // Re-stage the orphan and pin the directory with another live
        // daemon PID (pid 1 — always present). Cleanup must short-circuit.
        let orphan2 = ssh_dir.join("user@orphan2");
        {
            let _l = UnixListener::bind(&orphan2).unwrap();
        }
        write_daemon_pid(1).unwrap();
        let outcome = cleanup_stale_control_masters(std::process::id() as libc::pid_t).unwrap();
        assert_eq!(outcome, CmCleanupOutcome::SkippedAnotherDaemon { pid: 1 });
        // Orphan still present — we didn't touch the dir.
        assert!(orphan2.exists());

        // Stale PID for a process that no longer exists: cleanup
        // proceeds. PID 0 is never a real process, so passing it as the
        // recorded PID forces is_process_alive() to false; the existing
        // orphan is then unlinked.
        write_daemon_pid(0).unwrap();
        let outcome = cleanup_stale_control_masters(std::process::id() as libc::pid_t).unwrap();
        // orphan2 should be removed; live still alive.
        assert!(matches!(
            outcome,
            CmCleanupOutcome::Scanned {
                removed: 1,
                kept: 1
            }
        ));
        assert!(!orphan2.exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn reverse_forward_spec_joins_remote_and_local_with_colon() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        std::env::remove_var("SHELBI_HUB_SOCK");
        let spec = reverse_forward_spec().unwrap();
        let s = spec.to_string_lossy().into_owned();
        let uid = unsafe { libc::getuid() };
        assert!(
            s.starts_with(&format!("/tmp/shelbi-hub-{uid}-")),
            "got: {s}"
        );
        assert!(s.contains(".sock:"), "got: {s}");
        assert!(s.ends_with("/hub.sock"), "got: {s}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn reverse_forward_spec_tcp_binds_loopback_port_onto_hub_sock() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_HUB_SOCK");
        let spec = reverse_forward_spec_tcp(47105).unwrap();
        let s = spec.to_string_lossy().into_owned();
        // `127.0.0.1:<port>:<hub.sock>` — remote-TCP → local-Unix `-R` form.
        assert!(s.starts_with("127.0.0.1:47105:"), "got: {s}");
        assert!(s.ends_with("/hub.sock"), "got: {s}");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn host_forward_state_round_trips_and_clears() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        // Unknown host → no decision recorded.
        assert_eq!(load_host_forward("devbox"), None);

        // Persist a TCP decision with a bound port; it reads back verbatim.
        save_host_forward(
            "devbox",
            Some(HostForward {
                mode: shelbi_core::ForwardMode::Tcp,
                port: Some(47101),
            }),
        )
        .unwrap();
        assert_eq!(
            load_host_forward("devbox"),
            Some(HostForward {
                mode: shelbi_core::ForwardMode::Tcp,
                port: Some(47101),
            })
        );

        // A decision for a *different* host doesn't clobber the first
        // (read-modify-write against the whole map).
        save_host_forward(
            "mac",
            Some(HostForward {
                mode: shelbi_core::ForwardMode::Unix,
                port: None,
            }),
        )
        .unwrap();
        assert_eq!(
            load_host_forward("devbox").unwrap().mode,
            shelbi_core::ForwardMode::Tcp
        );

        // Clearing forgets just that host (back to auto-detection).
        save_host_forward("devbox", None).unwrap();
        assert_eq!(load_host_forward("devbox"), None);
        assert!(load_host_forward("mac").is_some());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn remote_hub_endpoint_reflects_persisted_mode() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");

        // Default (no decision) → Unix landing socket, `unix:` scheme.
        match remote_hub_endpoint("fresh") {
            HubEndpoint::Unix(p) => {
                assert!(p.to_string_lossy().starts_with("/tmp/shelbi-hub-"));
            }
            other => panic!("expected Unix endpoint, got {other:?}"),
        }
        assert!(remote_hub_endpoint("fresh")
            .addr_env_value()
            .starts_with("unix:/tmp/shelbi-hub-"));

        // Persisted TCP with a port → TCP loopback endpoint, `tcp:` scheme.
        save_host_forward(
            "tsbox",
            Some(HostForward {
                mode: shelbi_core::ForwardMode::Tcp,
                port: Some(47108),
            }),
        )
        .unwrap();
        assert_eq!(
            remote_hub_endpoint("tsbox"),
            HubEndpoint::Tcp {
                addr: "127.0.0.1".into(),
                port: 47108,
            }
        );
        assert_eq!(
            remote_hub_endpoint("tsbox").addr_env_value(),
            "tcp:127.0.0.1:47108"
        );

        // TCP recorded but no port yet → fall back to Unix until a port binds.
        save_host_forward(
            "halfway",
            Some(HostForward {
                mode: shelbi_core::ForwardMode::Tcp,
                port: None,
            }),
        )
        .unwrap();
        assert!(matches!(
            remote_hub_endpoint("halfway"),
            HubEndpoint::Unix(_)
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn tcp_forward_band_honors_env_overrides_and_clamps() {
        let _g = lock_test();
        // Clean slate: no overrides → defaults.
        std::env::remove_var("SHELBI_TCP_FORWARD_PORT_BASE");
        std::env::remove_var("SHELBI_TCP_FORWARD_PORT_SPAN");
        assert_eq!(tcp_forward_port_base(), TCP_FORWARD_PORT_BASE);
        assert_eq!(tcp_forward_port_span(), TCP_FORWARD_PORT_SPAN);

        // Valid overrides are honored (whitespace tolerated).
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_BASE", " 50000 ");
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_SPAN", "128");
        assert_eq!(tcp_forward_port_base(), 50000);
        assert_eq!(tcp_forward_port_span(), 128);

        // Malformed / zero values fall back to the compiled defaults rather
        // than wedging the forward path.
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_BASE", "not-a-port");
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_SPAN", "0");
        assert_eq!(tcp_forward_port_base(), TCP_FORWARD_PORT_BASE);
        assert_eq!(tcp_forward_port_span(), TCP_FORWARD_PORT_SPAN);

        // A span that would push the band past u16::MAX is clamped so the
        // deterministic sweep (base + i) can't overflow.
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_BASE", "65500");
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_SPAN", "1000");
        assert_eq!(tcp_forward_port_base(), 65500);
        let span = tcp_forward_port_span();
        assert_eq!(span, u16::MAX - 65500 + 1, "span clamped to fit the band");
        assert_eq!(
            65500u32 + span as u32 - 1,
            u16::MAX as u32,
            "top of band lands exactly on u16::MAX",
        );

        std::env::remove_var("SHELBI_TCP_FORWARD_PORT_BASE");
        std::env::remove_var("SHELBI_TCP_FORWARD_PORT_SPAN");
    }

    #[test]
    fn load_forward_state_tolerates_missing_and_torn_file() {
        let _g = lock_test();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        // Missing file → empty map, no error.
        assert!(load_forward_state().is_empty());
        // Torn/garbage file → empty map, not a panic.
        fs::write(forward_state_path().unwrap(), "{ not json").unwrap();
        assert!(load_forward_state().is_empty());
        std::env::remove_var("SHELBI_HOME");
    }
}
