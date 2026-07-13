//! CLI-side daemon version gate.
//!
//! Homebrew (and every other package manager) swaps the `shelbi` binary
//! on upgrade but never restarts the long-lived hub daemon, so the old
//! daemon keeps writing state in its old shape while new CLI one-shots
//! read the new layout — the shaft-project outage surfaced as bare
//! `io: No such file or directory` failures on every status transition.
//!
//! This module turns that silent skew into an actionable signal, driven
//! by the hello frame returned on a dedicated empty probe connection
//! (see `shelbi_state::probe_daemon_hello`):
//!
//! - **State-mutating commands** (`task move`, `task start`, `zen pr-*`,
//!   …) call [`ensure_daemon_matches_for_mutation`] and refuse to run on
//!   a mismatch, naming both versions and the fix. When it's safe (no
//!   in-progress workspaces) the CLI offers to run
//!   `shelbi daemon restart` right there — auto-accepted by the global
//!   `--yes` flag, skipped when non-interactive.
//! - **Read-only commands** (`task list`, `status`, `zen status`) call
//!   [`warn_on_mismatch`] — one stderr line, then proceed.
//! - No daemon listening at all is *not* a mismatch: the CLI's file
//!   fallbacks handle that case and always have.

use std::io::IsTerminal;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
pub use shelbi_state::DaemonVersionStatus;

/// This binary's version — what the daemon's hello must equal exactly
/// (daemon and CLI ship from the same workspace version; no range logic).
pub const CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Set to `1` by the global `--yes` flag: auto-accept the interactive
/// daemon-restart offer. Carried through the environment so the gate
/// doesn't need plumbing through every subcommand's argument struct.
pub const ASSUME_YES_ENV: &str = "SHELBI_YES";

/// How long to wait for the supervisor to bring the daemon back on the
/// new binary after `shelbi daemon restart`. launchd/systemd relaunch
/// with a ~1s throttle; 10s is generous headroom before we give up and
/// tell the user to re-run.
const RESTART_VERIFY_TIMEOUT: Duration = Duration::from_secs(10);

/// Probe the hub socket and classify the daemon's version against ours.
pub fn check() -> DaemonVersionStatus {
    shelbi_state::daemon_version_status()
}

/// Gate for state-mutating commands: on a version mismatch, either fix
/// it (the interactive/`--yes` restart path) or fail fast with an error
/// naming both versions and the remedy — never proceed into the
/// undiagnosable io-error swamp a mixed-version hub produces.
pub fn ensure_daemon_matches_for_mutation() -> Result<()> {
    let DaemonVersionStatus::Mismatch { daemon } = check() else {
        return Ok(());
    };
    let busy = busy_workspaces();
    if busy == 0 && restart_offer_accepted(&daemon)? {
        return restart_daemon_and_verify();
    }
    let busy_note = if busy > 0 {
        format!(
            " ({busy} workspace(s) are in progress — the supervisor relaunches the daemon \
             in about a second, so restarting is safe once you're ready)"
        )
    } else {
        String::new()
    };
    bail!(
        "hub daemon is {daemon}, CLI is {CLI_VERSION} — run `shelbi daemon restart` to put \
         the daemon on the current binary{busy_note}"
    )
}

/// Read-only companion to [`ensure_daemon_matches_for_mutation`]: one
/// stderr warning line on mismatch, then the command proceeds.
pub fn warn_on_mismatch() {
    if let DaemonVersionStatus::Mismatch { daemon } = check() {
        eprintln!(
            "warning: hub daemon is {daemon}, CLI is {CLI_VERSION} — run \
             `shelbi daemon restart` (state written by the old daemon may not match \
             what this CLI reads)"
        );
    }
}

/// One-line daemon-version summary for `shelbi status`.
pub fn status_line() -> String {
    match check() {
        DaemonVersionStatus::NotRunning => format!("not running; cli: {CLI_VERSION}"),
        DaemonVersionStatus::Match { version } => {
            format!("{version}; cli: {CLI_VERSION} (match)")
        }
        DaemonVersionStatus::Mismatch { daemon } => {
            format!("{daemon} — MISMATCH: cli is {CLI_VERSION}; run `shelbi daemon restart`")
        }
    }
}

/// Should we restart the daemon right now? `--yes` (via
/// [`ASSUME_YES_ENV`]) auto-accepts; an interactive terminal gets a
/// prompt; a non-interactive caller skips the offer (and the caller
/// falls through to the fail-fast error).
fn restart_offer_accepted(daemon: &str) -> Result<bool> {
    if std::env::var(ASSUME_YES_ENV).as_deref() == Ok("1") {
        eprintln!("hub daemon is {daemon}, CLI is {CLI_VERSION} — restarting the daemon (--yes)");
        return Ok(true);
    }
    if !std::io::stdin().is_terminal() || !std::io::stderr().is_terminal() {
        return Ok(false);
    }
    inquire::Confirm::new(&format!(
        "Hub daemon is {daemon} but this CLI is {CLI_VERSION}. Restart the daemon now?"
    ))
    .with_default(true)
    .prompt()
    .context("confirm prompt `Restart the daemon now?`")
}

/// Run `shelbi daemon restart` and wait for the relaunched daemon to
/// come back speaking this CLI's version.
fn restart_daemon_and_verify() -> Result<()> {
    super::daemon::run(Some(super::daemon::DaemonCmd::Restart))
        .context("restarting the hub daemon")?;
    let deadline = Instant::now() + RESTART_VERIFY_TIMEOUT;
    while Instant::now() < deadline {
        if let DaemonVersionStatus::Match { version } = check() {
            eprintln!("hub daemon restarted — now {version}");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    bail!(
        "restarted the hub daemon but it hasn't come back on {CLI_VERSION} within \
         {}s — check `shelbi daemon status` and re-run",
        RESTART_VERIFY_TIMEOUT.as_secs()
    )
}

/// Count of workspaces with in-progress tasks across every registered
/// project. Restarting the hub daemon affects the whole hub, so checking
/// only the command's project could bounce it while another project's
/// workers are active. Best-effort: an unreadable registry or board
/// counts as busy so we never auto-restart on missing information.
fn busy_workspaces() -> usize {
    let Ok(projects) = shelbi_state::list_projects() else {
        return 1;
    };
    let mut busy = 0;
    for project in projects {
        match super::status::workspace_idle_busy(&project.name) {
            Ok((_idle, project_busy)) => busy += project_busy,
            Err(_) => return busy.max(1),
        }
    }
    busy
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use std::io::{Read, Write};
    use std::os::unix::net::UnixListener;

    /// RAII: point `SHELBI_HUB_SOCK` at a path for the test's duration.
    fn hub_sock_guard(path: &std::path::Path) -> EnvGuard {
        let g = EnvGuard::new(&["SHELBI_HUB_SOCK", ASSUME_YES_ENV]);
        g.set("SHELBI_HUB_SOCK", path);
        g.remove(ASSUME_YES_ENV);
        g
    }

    fn short_sock(tag: &str) -> std::path::PathBuf {
        // macOS caps Unix-socket paths at ~104 bytes; keep it short.
        std::path::PathBuf::from(format!("/tmp/shb-{tag}-{}.sock", std::process::id()))
    }

    /// Serve exactly one empty probe connection, answering with `hello_line`
    /// (or closing without a response when `None`, like a pre-handshake daemon).
    fn serve_once(listener: UnixListener, hello_line: Option<String>) {
        std::thread::spawn(move || {
            if let Ok((mut s, _)) = listener.accept() {
                let mut request = Vec::new();
                let _ = s.read_to_end(&mut request);
                if request.is_empty() {
                    if let Some(line) = hello_line {
                        let _ = s.write_all(line.as_bytes());
                    }
                }
            }
        });
    }

    #[test]
    fn check_reports_not_running_without_a_listener() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("gone");
        let _ = std::fs::remove_file(&sock);
        let _g = hub_sock_guard(&sock);
        assert_eq!(check(), DaemonVersionStatus::NotRunning);
    }

    #[test]
    fn check_matches_a_daemon_on_our_version() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("match");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _g = hub_sock_guard(&sock);
        serve_once(
            listener,
            Some(shelbi_state::DaemonHello::new(CLI_VERSION).to_line()),
        );
        assert_eq!(
            check(),
            DaemonVersionStatus::Match {
                version: CLI_VERSION.into()
            }
        );
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn check_flags_a_daemon_on_a_different_version() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("mism");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _g = hub_sock_guard(&sock);
        serve_once(
            listener,
            Some(shelbi_state::DaemonHello::new("0.1.0").to_line()),
        );
        assert_eq!(
            check(),
            DaemonVersionStatus::Mismatch {
                daemon: "0.1.0".into()
            }
        );
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn check_flags_a_protocol_bump_even_on_same_semver() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("proto");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _g = hub_sock_guard(&sock);
        let mut hello = shelbi_state::DaemonHello::new(CLI_VERSION);
        hello.protocol = shelbi_state::HUB_PROTOCOL_VERSION + 1;
        serve_once(listener, Some(hello.to_line()));
        match check() {
            DaemonVersionStatus::Mismatch { daemon } => {
                assert!(daemon.contains("protocol"), "got: {daemon}");
            }
            other => panic!("expected protocol mismatch, got {other:?}"),
        }
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn check_treats_a_silent_pre_handshake_daemon_as_mismatch() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("old");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _g = hub_sock_guard(&sock);
        serve_once(listener, None);
        match check() {
            DaemonVersionStatus::Mismatch { daemon } => {
                assert!(daemon.contains("older"), "got: {daemon}");
            }
            other => panic!("expected pre-handshake mismatch, got {other:?}"),
        }
        let _ = std::fs::remove_file(&sock);
    }

    /// The acceptance-criterion error shape: a mutating command against a
    /// mismatched daemon fails fast, naming both versions and the fix —
    /// no bare io error, no restart attempt when non-interactive.
    #[test]
    fn mutation_gate_fails_fast_with_actionable_error_on_mismatch() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("gate");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).unwrap();
        let _g = hub_sock_guard(&sock);
        serve_once(
            listener,
            Some(shelbi_state::DaemonHello::new("0.1.0").to_line()),
        );
        // Cargo test is non-interactive, so the restart offer is skipped
        // and the command falls straight through to the fail-fast error.
        let err = ensure_daemon_matches_for_mutation().unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("0.1.0"), "names the daemon version: {msg}");
        assert!(msg.contains(CLI_VERSION), "names the CLI version: {msg}");
        assert!(
            msg.contains("shelbi daemon restart"),
            "names the fix: {msg}"
        );
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn mutation_gate_is_a_noop_without_a_daemon_or_on_match() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("noop");
        let _ = std::fs::remove_file(&sock);
        {
            let _g = hub_sock_guard(&sock);
            ensure_daemon_matches_for_mutation().expect("no daemon → proceed");
        }
        let listener = UnixListener::bind(&sock).unwrap();
        let _g = hub_sock_guard(&sock);
        serve_once(
            listener,
            Some(shelbi_state::DaemonHello::new(CLI_VERSION).to_line()),
        );
        ensure_daemon_matches_for_mutation().expect("matching daemon → proceed");
        let _ = std::fs::remove_file(&sock);
    }

    #[test]
    fn status_line_shapes() {
        let _l = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sock = short_sock("stat");
        let _ = std::fs::remove_file(&sock);
        let _g = hub_sock_guard(&sock);
        assert!(status_line().contains("not running"));
    }
}
