//! The CLI side of issue mutations: build a [`MutationKind`], then either run
//! it in-process (the library, setting off — byte-identical to the old CLI) or
//! send it to the daemon's control socket (setting on). The daemon path exists
//! so several clients can mutate the board without interleaving stale work; see
//! the "Removing tmux" plan, "The daemon executes mutations".
//!
//! `shelbi issue move|start|assign|unassign|edit|add` call [`run_mutation`].
//! Output and exit codes are the same on both paths: the library emits every
//! user-facing line through an [`OutputSink`](shelbi_orchestrator::mutate::OutputSink),
//! and the daemon streams those same lines back verbatim.

use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use shelbi_orchestrator::mutate;
use shelbi_proto::control::{MutationKind, MutationRequest, Stream};

/// Run a mutation against issue `id` in `project`, routed through the daemon's
/// control socket (the one runtime).
pub fn run_mutation(project: &str, id: &str, kind: MutationKind) -> Result<()> {
    run_via_daemon(project, id, kind)
}

/// The daemon path: ensure the daemon is up, connect to the control socket, and
/// stream the mutation's output back, printing it exactly as the in-process
/// path would.
fn run_via_daemon(project: &str, id: &str, kind: MutationKind) -> Result<()> {
    // Capture the `(status, revision)` we are looking at so the daemon can
    // reject a command whose issue has moved on. `add` has no prior state.
    let expected = match &kind {
        MutationKind::Add(_) => None,
        _ => mutate::current_state(project, id).ok(),
    };

    shelbi_state::ensure_daemon_running().map_err(|e| anyhow!(e))?;
    let sock = shelbi_state::control_socket_path().map_err(|e| anyhow!(e))?;
    let mut client = connect_with_retry(&sock)?;

    let req = MutationRequest {
        request_id: 1,
        project: project.to_string(),
        id: id.to_string(),
        expected,
        kind,
    };
    let mut on_line = |stream: Stream, text: &str| match stream {
        Stream::Stdout => println!("{text}"),
        Stream::Stderr => eprintln!("{text}"),
    };
    client.mutate(&req, &mut on_line).map_err(map_client_err)
}

/// Connect to the control socket, retrying briefly: `ensure_daemon_running`
/// waits on `hub.sock`, and the control socket is bound just before the hub
/// starts answering, so in practice it is already up — but a cold start can
/// still race, so retry for a short window.
fn connect_with_retry(sock: &std::path::Path) -> Result<shelbi_client::ControlClient> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match shelbi_client::ControlClient::connect(sock, shelbi_state::CLIENT_VERSION) {
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

/// Map a control-client error to an `anyhow` error. A daemon-run mutation that
/// failed carries its typed [`MutationError`](shelbi_proto::control::MutationError),
/// whose `Display` is the operator-facing message.
fn map_client_err(e: shelbi_client::ClientError) -> anyhow::Error {
    anyhow!("{e}")
}
