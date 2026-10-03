//! On-demand daemon lifecycle for `shelbi daemon restart|status`, plus the
//! upgrade step that retires a leftover launchd/systemd unit.
//!
//! The daemon is started on demand and exits when no project is open (see
//! `docs/removing-tmux/phase3-daemon.md`). There is no supervisor to install,
//! so this module does not write units: it only *removes* ones left by older
//! builds, and drives restart/status directly off the single-instance lock, the
//! socket, and the PID record (all in `shelbi-state`).
//!
//! [`retire_supervisor_units`] is called on daemon startup by [`super::serve`]
//! so an existing install self-heals: an installed unit's `KeepAlive` (launchd)
//! or `Restart=always` (systemd) loop would otherwise fight the on-demand
//! daemon, respawning the old binary after a restart and keeping a daemon alive
//! with no project open.

use std::fs;
use std::path::PathBuf;
#[cfg(any(target_os = "macos", target_os = "linux"))]
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};

/// launchd `Label` / plist basename for the retired supervisor, and its
/// pre-rename predecessor. Both are booted out and deleted so an upgrade from
/// any prior build leaves nothing respawning the old daemon.
#[cfg(target_os = "macos")]
const SERVICE_LABEL: &str = "dev.shelbi.daemon";
#[cfg(target_os = "macos")]
const LEGACY_SERVICE_LABEL: &str = "co.32pixels.shelbi";
/// systemd user unit name for the retired supervisor, and its pre-rename
/// predecessor.
#[cfg(target_os = "linux")]
const SYSTEMD_SERVICE_NAME: &str = "dev.shelbi.daemon.service";
#[cfg(target_os = "linux")]
const LEGACY_SYSTEMD_SERVICE_NAME: &str = "shelbi.service";

/// How long [`restart`] waits for the freshly started daemon to answer before
/// reporting success.
const RESTART_START_DEADLINE: Duration = Duration::from_secs(10);

// --------------------------------------------------------------------------
// Retire the supervisor units (upgrade step, run on daemon startup)
// --------------------------------------------------------------------------

/// Stop and remove any installed launchd/systemd supervisor unit, returning the
/// unit files removed (for disclosure). Idempotent: a host with no unit returns
/// an empty vec and makes no changes. Best-effort throughout — a hiccup must
/// never take the daemon down — so it swallows the expected "not loaded" noise
/// from `launchctl bootout` / `systemctl disable`.
pub(super) fn retire_supervisor_units() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        retire_launchd()
    }
    #[cfg(target_os = "linux")]
    {
        retire_systemd()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
fn retire_launchd() -> Vec<PathBuf> {
    let uid = current_uid();
    let mut removed = Vec::new();
    for label in [SERVICE_LABEL, LEGACY_SERVICE_LABEL] {
        // bootout stops the agent and unregisters it. Non-zero/"No such
        // process" is the expected no-op case; silence it.
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{uid}/{label}")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Some(path) = launch_agent_plist_path(label) {
            if path.exists() && fs::remove_file(&path).is_ok() {
                removed.push(path);
            }
        }
    }
    removed
}

#[cfg(target_os = "macos")]
fn current_uid() -> u32 {
    // SAFETY: getuid() is a thread-safe POSIX call with no inputs.
    unsafe { libc::getuid() }
}

#[cfg(target_os = "macos")]
fn launch_agent_plist_path(label: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join("Library/LaunchAgents").join(format!("{label}.plist")))
}

#[cfg(target_os = "linux")]
fn retire_systemd() -> Vec<PathBuf> {
    let mut removed = Vec::new();
    for unit in [SYSTEMD_SERVICE_NAME, LEGACY_SYSTEMD_SERVICE_NAME] {
        // disable --now stops the unit and drops its wants links. Tolerate the
        // "no such unit" error so retire stays idempotent after a manual rm.
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", unit])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        if let Some(path) = systemd_unit_path(unit) {
            if path.exists() && fs::remove_file(&path).is_ok() {
                removed.push(path);
            }
        }
    }
    if !removed.is_empty() {
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    removed
}

#[cfg(target_os = "linux")]
fn systemd_unit_path(unit: &str) -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/systemd/user").join(unit))
}

// --------------------------------------------------------------------------
// restart / status
// --------------------------------------------------------------------------

/// Stop the running daemon and start a fresh one on the current binary, with no
/// supervisor. Retires any leftover unit first so its respawn loop can't bring
/// the old binary back between the stop and the start.
pub(super) fn restart() -> Result<()> {
    let removed = retire_supervisor_units();
    for path in &removed {
        println!("✓ retired leftover supervisor unit {}", path.display());
    }

    let stopped = shelbi_state::stop_daemon().context("stopping the running hub daemon")?;
    if stopped {
        println!("✓ stopped the running hub daemon");
    }

    shelbi_state::ensure_daemon_running().context("starting the hub daemon")?;
    // ensure_daemon_running already waits for the socket; this re-check keeps
    // the success line honest under a slow host.
    let deadline = Instant::now() + RESTART_START_DEADLINE;
    while Instant::now() < deadline {
        if !matches!(
            shelbi_state::probe_daemon_hello(),
            shelbi_state::DaemonProbe::NotRunning
        ) {
            println!("✓ hub daemon restarted on the current binary");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    anyhow::bail!(
        "restarted the hub daemon but it did not come back within {}s — \
         check `shelbi daemon status`",
        RESTART_START_DEADLINE.as_secs()
    )
}

/// Report daemon status from the single-instance lock, the socket, and the PID
/// record — no supervisor to query.
pub(super) fn status() -> Result<()> {
    let lock_held = shelbi_state::daemon_lock_held();
    let probe = shelbi_state::probe_daemon_hello();
    let listening = !matches!(probe, shelbi_state::DaemonProbe::NotRunning);

    if !lock_held && !listening {
        println!("shelbi daemon: not running");
        println!("  (started on demand when you open a project)");
        return Ok(());
    }

    println!("shelbi daemon: running");
    println!("  lock held:  {}", if lock_held { "yes" } else { "no" });
    println!("  listening:  {}", if listening { "yes" } else { "no" });

    match &probe {
        shelbi_state::DaemonProbe::Hello(hello) => {
            println!("  version:    {}", hello.version);
            println!("  protocol:   {}", hello.protocol);
        }
        shelbi_state::DaemonProbe::NoHello => {
            println!("  version:    (older daemon, predates the version handshake)");
        }
        shelbi_state::DaemonProbe::NotRunning => {}
    }

    if let Ok(Some(record)) = shelbi_state::read_daemon_pid_record() {
        println!("  pid:        {}", record.pid);
        if let Some(v) = record.version {
            println!("  pid record: {v}");
        }
    }

    if let Ok(open) = shelbi_state::list_open_projects() {
        if open.is_empty() {
            println!("  open:       (no open projects — will exit when idle)");
        } else {
            println!("  open:       {}", open.join(", "));
        }
    }
    Ok(())
}
