//! `shelbi detach` — detach clients from the one running session.
//!
//! Shelbi runs a single long-lived session owned by the daemon; multiple clients
//! (TUI windows today, the Desktop app / a remote client later) can attach to it
//! at once. Detaching a client stops *its* rendering and drops *its* connections
//! while every session and agent keeps running — the client-local stop `q` (and
//! the palette's "Detach") performs, made reachable from outside the client:
//!
//! - `shelbi detach` detaches **every** attached client and prints how many.
//! - `shelbi detach <client-id>` detaches just that one.
//! - `shelbi detach --list` lists the attached clients without detaching.
//!
//! There is no `shelbi attach`: plain `shelbi` reattaches to the running session
//! (or starts one). This command is a thin client over the daemon's control
//! socket, mirroring [`super::quit`]; the IO sits behind the [`DetachOps`] seam
//! so the routing (daemon-running gate, list vs detach, the selector, the
//! output) is unit-testable without a daemon.

use anyhow::{anyhow, Result};
use shelbi_proto::control::{AttachedClient, DetachSelector};

/// The side effects a detach performs, behind a trait so the routing is testable
/// with a stubbed daemon.
pub(crate) trait DetachOps {
    /// Whether a hub daemon is currently running (its single-instance lock is
    /// held). When it is not, nothing is open — there are no clients to detach.
    fn daemon_running(&self) -> bool;
    /// List the attached clients through the control socket.
    fn list_clients(&self) -> Result<Vec<AttachedClient>>;
    /// Detach the clients matching `selector`, carrying `reason` (printed by each
    /// detached client on exit). Returns how many were detached.
    fn detach(&self, selector: DetachSelector, reason: &str) -> Result<u32>;
}

/// What a detach did, for reporting / assertions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DetachReport {
    /// No daemon was running — nothing is attached.
    NothingOpen,
    /// `--list`: the attached clients (possibly empty).
    Listed(Vec<AttachedClient>),
    /// `n` clients were detached (`0` means none were attached).
    Detached(u32),
    /// `shelbi detach <id>` named a client that is not attached.
    NoSuchClient(String),
}

/// `shelbi detach [CLIENT_ID] [--list]`.
pub fn run(list: bool, client_id: Option<String>) -> Result<()> {
    let reason = format!("Detached by 'shelbi detach' from {}.", shelbi_client::hostname());
    let report = run_with(&LiveDetach, list, client_id, &reason)?;
    match report {
        DetachReport::NothingOpen => {
            println!("No clients attached.");
        }
        DetachReport::Listed(clients) => print_client_list(&clients),
        DetachReport::Detached(0) => {
            println!("No clients attached.");
        }
        DetachReport::Detached(n) => {
            println!("Detached {n} client{}.", if n == 1 { "" } else { "s" });
        }
        DetachReport::NoSuchClient(id) => {
            println!("No client '{id}' attached.");
        }
    }
    Ok(())
}

/// The detach routing, exercised by both production and the tests.
pub(crate) fn run_with(
    ops: &dyn DetachOps,
    list: bool,
    client_id: Option<String>,
    reason: &str,
) -> Result<DetachReport> {
    // No daemon → nothing is open. Report and exit cleanly (exit 0) for all forms.
    if !ops.daemon_running() {
        return Ok(DetachReport::NothingOpen);
    }

    if list {
        return Ok(DetachReport::Listed(ops.list_clients()?));
    }

    match client_id {
        Some(id) => {
            let n = ops.detach(
                DetachSelector::One {
                    client_id: id.clone(),
                },
                reason,
            )?;
            if n == 0 {
                Ok(DetachReport::NoSuchClient(id))
            } else {
                Ok(DetachReport::Detached(n))
            }
        }
        None => Ok(DetachReport::Detached(ops.detach(DetachSelector::All, reason)?)),
    }
}

/// Print the attached-clients table for `shelbi detach --list`.
fn print_client_list(clients: &[AttachedClient]) {
    if clients.is_empty() {
        println!("No clients attached.");
        return;
    }
    println!("{:<10}  {:<7}  {:<16}  {:<12}  ATTACHED", "ID", "KIND", "HOST", "PROJECT");
    for c in clients {
        let pid = c.pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
        println!(
            "{:<10}  {:<7}  {:<16}  {:<12}  {}{}",
            c.client_id,
            c.kind.as_str(),
            c.host,
            c.project.as_deref().unwrap_or("-"),
            c.attached_at,
            pid,
        );
    }
}

/// The production [`DetachOps`]: the daemon-running gate is the single-instance
/// lock, and each operation is a control-socket round-trip.
struct LiveDetach;

impl DetachOps for LiveDetach {
    fn daemon_running(&self) -> bool {
        shelbi_state::daemon_lock_held()
    }

    fn list_clients(&self) -> Result<Vec<AttachedClient>> {
        let mut client = connect()?;
        client
            .list_clients()
            .map_err(|e| anyhow!("listing attached clients: {e}"))
    }

    fn detach(&self, selector: DetachSelector, reason: &str) -> Result<u32> {
        let mut client = connect()?;
        client
            .detach_clients(selector, reason)
            .map_err(|e| anyhow!("detaching clients: {e}"))
    }
}

/// Connect to the daemon's control socket, retrying briefly (the daemon may hold
/// the single-instance lock before its control socket answers). Mirrors
/// [`super::quit`]'s connect.
fn connect() -> Result<shelbi_client::ControlClient> {
    use std::time::{Duration, Instant};
    let sock = shelbi_state::control_socket_path().map_err(|e| anyhow!(e))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match shelbi_client::ControlClient::connect(&sock, shelbi_state::CLIENT_VERSION) {
            Ok(c) => return Ok(c),
            Err(e) => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "could not reach the hub daemon's control socket at {}: {e}",
                        sock.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use shelbi_proto::control::ClientKind;
    use std::sync::Mutex;

    /// A stubbed daemon recording the detach calls and returning a fixed client
    /// list / detach count.
    struct StubDaemon {
        running: bool,
        clients: Vec<AttachedClient>,
        calls: Mutex<Vec<String>>,
    }
    impl StubDaemon {
        fn new(running: bool, clients: Vec<AttachedClient>) -> Self {
            Self {
                running,
                clients,
                calls: Mutex::new(Vec::new()),
            }
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }
    impl DetachOps for StubDaemon {
        fn daemon_running(&self) -> bool {
            self.running
        }
        fn list_clients(&self) -> Result<Vec<AttachedClient>> {
            self.calls.lock().unwrap().push("list".into());
            Ok(self.clients.clone())
        }
        fn detach(&self, selector: DetachSelector, _reason: &str) -> Result<u32> {
            // Count the clients the selector matches, like the daemon would.
            let count = self
                .clients
                .iter()
                .filter(|c| match &selector {
                    DetachSelector::All => true,
                    DetachSelector::AllExcept { client_id } => c.client_id != *client_id,
                    DetachSelector::One { client_id } => c.client_id == *client_id,
                })
                .count() as u32;
            self.calls
                .lock()
                .unwrap()
                .push(format!("detach:{selector:?}"));
            Ok(count)
        }
    }

    fn client(id: &str) -> AttachedClient {
        AttachedClient {
            client_id: id.into(),
            kind: ClientKind::Tui,
            host: "studio".into(),
            pid: Some(42),
            project: Some("alpha".into()),
            attached_at: "2026-10-07T00:00:00Z".into(),
        }
    }

    #[test]
    fn detach_all_reports_the_count() {
        let daemon = StubDaemon::new(true, vec![client("a"), client("b")]);
        let report = run_with(&daemon, false, None, "r").unwrap();
        assert_eq!(report, DetachReport::Detached(2));
        assert_eq!(daemon.calls(), vec!["detach:All"]);
    }

    #[test]
    fn detach_all_with_no_clients_reports_zero() {
        let daemon = StubDaemon::new(true, vec![]);
        let report = run_with(&daemon, false, None, "r").unwrap();
        assert_eq!(report, DetachReport::Detached(0));
    }

    #[test]
    fn detach_one_targets_just_that_client() {
        let daemon = StubDaemon::new(true, vec![client("a"), client("b")]);
        let report = run_with(&daemon, false, Some("a".into()), "r").unwrap();
        assert_eq!(report, DetachReport::Detached(1));
        assert_eq!(
            daemon.calls(),
            vec![r#"detach:One { client_id: "a" }"#.to_string()]
        );
    }

    #[test]
    fn detach_one_unknown_reports_no_such_client() {
        let daemon = StubDaemon::new(true, vec![client("a")]);
        let report = run_with(&daemon, false, Some("zzz".into()), "r").unwrap();
        assert_eq!(report, DetachReport::NoSuchClient("zzz".into()));
    }

    #[test]
    fn list_returns_the_clients_without_detaching() {
        let daemon = StubDaemon::new(true, vec![client("a"), client("b")]);
        let report = run_with(&daemon, true, None, "r").unwrap();
        assert_eq!(report, DetachReport::Listed(vec![client("a"), client("b")]));
        assert_eq!(daemon.calls(), vec!["list"]);
    }

    #[test]
    fn no_daemon_reports_nothing_open_without_connecting() {
        let daemon = StubDaemon::new(false, vec![client("a")]);
        // Detach, list, and detach-one all short-circuit.
        assert_eq!(
            run_with(&daemon, false, None, "r").unwrap(),
            DetachReport::NothingOpen
        );
        assert_eq!(
            run_with(&daemon, true, None, "r").unwrap(),
            DetachReport::NothingOpen
        );
        assert_eq!(
            run_with(&daemon, false, Some("a".into()), "r").unwrap(),
            DetachReport::NothingOpen
        );
        assert!(daemon.calls().is_empty(), "no control-socket call when idle");
    }

    #[test]
    fn a_detach_error_surfaces() {
        struct FailDaemon;
        impl DetachOps for FailDaemon {
            fn daemon_running(&self) -> bool {
                true
            }
            fn list_clients(&self) -> Result<Vec<AttachedClient>> {
                unreachable!()
            }
            fn detach(&self, _selector: DetachSelector, _reason: &str) -> Result<u32> {
                anyhow::bail!("control socket went away")
            }
        }
        let err = run_with(&FailDaemon, false, None, "r").unwrap_err();
        assert!(err.to_string().contains("went away"), "err: {err}");
    }
}
