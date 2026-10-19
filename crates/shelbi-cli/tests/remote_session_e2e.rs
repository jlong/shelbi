//! A remote dispatch-to-handoff cycle driven through the **session backend**
//! over a **stubbed SSH seam** (`rt-remote-spawn`).
//!
//! The acceptance for Phase 5: with the session-backend flag on, a `Host::Ssh`
//! workspace is a real `shelbi __session` process started "remotely" and reached
//! through a relay — probe / snapshot / send / title / kill all ride that relay,
//! a dropped relay reconnects to the still-running agent, and an unreachable
//! machine is reported Unreachable (never Dead).
//!
//! Only SSH itself is stubbed, as the acceptance criteria permit. The stub seam
//! ([`remote_session::RemoteSsh`]):
//!
//! - `launch` spawns the session **locally** with the real binary
//!   (`CARGO_BIN_EXE_shelbi` via `spawn_with_exe`), so a genuine `shelbi
//!   __session` owns the PTY and emulator exactly as a remote launch would
//!   produce; and
//! - `open_relay` runs the real [`shelbi_client::serve_relay`] in-process over a
//!   `UnixStream` pair (the pattern `relay_e2e` uses), standing in for `ssh
//!   <host> shelbi relay`.
//!
//! Every operation under test goes through `session_backend::backend()` with a
//! `Host::Ssh` target, so the production `SessionProcessBackend` remote branch +
//! `remote_session` relay stack are exercised end to end.

#![cfg(unix)]

use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use shelbi_client::{serve_relay, RelayChannel};
use shelbi_core::{Error, Host, Result};
use shelbi_orchestrator::remote_session::{self, RelayHandle, RemoteSsh};
use shelbi_orchestrator::session_backend::{backend, Liveness, SessionBackend, SessionTarget};
use shelbi_session::SpawnSpec;

/// These tests mutate process-global env + the remote-session seam, so they
/// serialize against each other through this lock.
static ENV_LOCK: Mutex<()> = Mutex::new(());

fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A stub SSH seam: `launch` spawns the session locally with the real binary,
/// `open_relay` bridges an in-process [`serve_relay`] over a `UnixStream` pair.
struct LocalRelaySeam;

impl RemoteSsh for LocalRelaySeam {
    fn launch(&self, _host: &Host, _bin: &str, spec: &SpawnSpec) -> Result<()> {
        shelbi_client::spawn_with_exe(Path::new(env!("CARGO_BIN_EXE_shelbi")), spec)
            .map(|_| ())
            .map_err(|e| Error::Other(format!("stub launch: {e}")))
    }

    fn open_relay(&self, _host: &Host, _bin: &str) -> Result<RelayHandle> {
        let root = shelbi_state::sessions_dir().map_err(|e| Error::Other(e.to_string()))?;
        let (hub, relay) = UnixStream::pair().map_err(Error::Io)?;
        let relay_read = relay.try_clone().map_err(Error::Io)?;
        // The relay server runs until its channel's socket closes (i.e. until the
        // RelayHandle — and the hub-side channel — is dropped, our "SSH drop").
        let handle = std::thread::spawn(move || {
            let _ = serve_relay(Box::new(relay_read), Box::new(relay), &root);
        });
        let hub_read = hub.try_clone().map_err(Error::Io)?;
        let channel = RelayChannel::new(Box::new(hub_read), Box::new(hub))
            .map_err(|e| Error::Other(format!("relay channel: {e}")))?;
        Ok(RelayHandle::new(channel, Box::new(handle)))
    }
}

/// A seam whose machine can never be reached: every open fails.
struct UnreachableSeam;

impl RemoteSsh for UnreachableSeam {
    fn launch(&self, _host: &Host, _bin: &str, _spec: &SpawnSpec) -> Result<()> {
        Err(Error::Other("unreachable (test)".into()))
    }
    fn open_relay(&self, _host: &Host, _bin: &str) -> Result<RelayHandle> {
        Err(Error::Other("unreachable (test)".into()))
    }
}

#[test]
fn remote_dispatch_to_handoff_reconnect_and_unreachable() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    let home = tempfile::tempdir().unwrap();
    let marker = home.path().join("review-ready.marker");

    std::env::set_var("SHELBI_HOME", home.path());
    remote_session::set_test_seam(Some(Arc::new(LocalRelaySeam)));

    // The remote workspace, addressed as the orchestrator addresses one: a
    // standalone session for the slot. The backend derives the logical name
    // `demo/ws/alice` from this, which the relay then discovers by.
    let host = Host::Ssh {
        host: "box".into(),
    };
    let target = SessionTarget::slot("shelbi-demo", "alice");

    struct Guard<'a> {
        host: &'a Host,
        target: &'a SessionTarget,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = backend().kill(self.host, self.target);
            remote_session::set_test_seam(None);
            std::env::remove_var("SHELBI_HOME");
        }
    }
    let _guard = Guard {
        host: &host,
        target: &target,
    };

    // Before dispatch: the relay answers "no such session" → definitively Dead
    // (the machine is reachable, just empty).
    assert_eq!(backend().probe(&host, &target, None), Liveness::Dead);

    // A stub agent: draws a ready input box + `shelbi:idle` title, blocks for a
    // line, then flips to `shelbi:review` and writes the review-ready marker.
    //
    // The marker is written tmp+rename (the atomic pattern real workers use): a
    // bare `> {marker}` creates and truncates the file before `printf` writes, so
    // the `marker.exists()` handoff gate below can fire before the contents land
    // and the following `read_to_string` reads it empty. The rename makes the
    // marker appear only once its full contents are there.
    let stub = format!(
        "printf '? for shortcuts\\n'; \
         printf '\\033]2;shelbi:idle\\007'; \
         IFS= read -r line; \
         printf '\\033]2;shelbi:review\\007'; \
         printf '%s\\n' \"$line\" > {marker}.tmp && mv {marker}.tmp {marker}; \
         printf 'REVIEW-READY\\n'; \
         exec cat",
        marker = marker.display(),
    );

    // Dispatch: the session backend's `spawn` builds the SpawnSpec and the stub
    // seam launches it (locally, with the real binary) — the remote spawn path.
    backend()
        .spawn(&host, &target, Some(&stub))
        .expect("remote spawn");

    // AC1: the agent starts in a (remote) session process, reached over the
    // relay — probe reports it Alive.
    let alive = wait_for(Duration::from_secs(10), || {
        matches!(backend().probe(&host, &target, None), Liveness::Alive).then_some(())
    });
    assert!(alive.is_some(), "remote session never came up Alive over the relay");

    // Its ready screen is readable over the relay.
    let ready = wait_for(Duration::from_secs(10), || {
        let snap = backend().snapshot(&host, &target).ok()?;
        snap.contains("? for shortcuts").then_some(snap)
    });
    assert!(ready.is_some(), "ready snapshot not visible over the relay");

    // AC2: kill the relay mid-task (simulating an SSH drop). The session keeps
    // running; the next op opens a fresh relay and reconnects to it.
    remote_session::reset_relays();

    // Deliver the message through the same seam `submit` uses — over a *fresh*
    // relay (the old one is gone). This is the reconnect path.
    let delivered = wait_for(Duration::from_secs(10), || {
        backend().send_line(&host, &target, "do the thing").ok()
    });
    assert!(
        delivered.is_some(),
        "could not deliver over a reconnected relay after the drop"
    );

    // The handoff: the agent wrote its review marker and flipped its title — read
    // back over the (reconnected) relay.
    let handoff = wait_for(Duration::from_secs(10), || {
        let title = backend().title(&host, &target).ok()?;
        (title.contains("shelbi:review") && marker.exists()).then_some(())
    });
    assert!(
        handoff.is_some(),
        "remote agent never reached handoff (title={:?}, marker={})",
        backend().title(&host, &target),
        marker.exists()
    );
    assert_eq!(
        std::fs::read_to_string(&marker).unwrap().trim(),
        "do the thing",
        "the delivered line reached the remote agent"
    );

    // AC3: an unreachable machine is reported Unreachable, never Dead, so
    // supervision will not redispatch it.
    remote_session::set_test_seam(Some(Arc::new(UnreachableSeam)));
    assert!(matches!(
        backend().probe(&host, &target, None),
        Liveness::Unreachable { .. }
    ));
    // `into_exists()` turns Unreachable into an Err (not `Ok(false)`), which is
    // what gates supervision's kill/redispatch — so the agent is left alone.
    assert!(backend().probe(&host, &target, None).into_exists().is_err());
    // Restore the working seam so the Guard can clean up the real session.
    remote_session::set_test_seam(Some(Arc::new(LocalRelaySeam)));
}

/// Like [`LocalRelaySeam`], but it keeps the hub end of every relay socket so
/// the test can kill the "SSH connection" under a live relay
/// ([`DroppableRelaySeam::drop_ssh`]): the relay channel then reads EOF, as it
/// does when `ssh` dies, and every stream on it ends.
#[derive(Default)]
struct DroppableRelaySeam {
    hub_ends: Mutex<Vec<UnixStream>>,
}

impl DroppableRelaySeam {
    fn drop_ssh(&self) {
        for end in self.hub_ends.lock().unwrap().drain(..) {
            let _ = end.shutdown(std::net::Shutdown::Both);
        }
    }
}

impl RemoteSsh for DroppableRelaySeam {
    fn launch(&self, host: &Host, bin: &str, spec: &SpawnSpec) -> Result<()> {
        LocalRelaySeam.launch(host, bin, spec)
    }

    fn open_relay(&self, _host: &Host, _bin: &str) -> Result<RelayHandle> {
        let root = shelbi_state::sessions_dir().map_err(|e| Error::Other(e.to_string()))?;
        let (hub, relay) = UnixStream::pair().map_err(Error::Io)?;
        let relay_read = relay.try_clone().map_err(Error::Io)?;
        let handle = std::thread::spawn(move || {
            let _ = serve_relay(Box::new(relay_read), Box::new(relay), &root);
        });
        self.hub_ends
            .lock()
            .unwrap()
            .push(hub.try_clone().map_err(Error::Io)?);
        let hub_read = hub.try_clone().map_err(Error::Io)?;
        let channel = RelayChannel::new(Box::new(hub_read), Box::new(hub))
            .map_err(|e| Error::Other(format!("relay channel: {e}")))?;
        Ok(RelayHandle::new(channel, Box::new(handle)))
    }
}

/// Collect a session's output until it contains `want` (or the stream ends /
/// the deadline passes). Returns whether it was seen.
fn read_until(rx: &std::sync::mpsc::Receiver<shelbi_client::SessionEvent>, want: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut seen = Vec::new();
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(shelbi_client::SessionEvent::Output { data, .. })
            | Ok(shelbi_client::SessionEvent::Resync { replay: data, .. }) => {
                seen.extend_from_slice(&data);
                if String::from_utf8_lossy(&seen).contains(want) {
                    return true;
                }
            }
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return false,
        }
    }
    false
}

/// The client side of a remote workspace (`dt-workspace-view`): a terminal
/// view attaches to the remote agent through the machine's relay with the
/// shared `session_connect::connect` (the desktop's connector), streams its
/// output and sends input; when the SSH connection under the relay dies the
/// stream ends while the agent keeps running; an unreachable machine reads as
/// `Unreachable` (reconnect, not a death); and once the machine answers again a
/// fresh attach replays the screen, the earlier exchange included.
#[test]
fn a_client_attach_through_the_relay_recovers_after_an_ssh_drop() {
    use shelbi_app::session::{ConnectFailure, SessionRef};
    use shelbi_orchestrator::session_connect;

    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join("projects/demo/tasks")).unwrap();
    std::fs::write(
        home.path().join("projects/demo.yaml"),
        "name: demo\nrepo: /tmp/demo\ndefault_branch: main\n\
         orchestrator:\n  runner: claude\n\
         agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
         machines:\n  - name: box\n    kind: ssh\n    host: box\n    work_dir: /tmp/demo\n\
         workspaces:\n  - { name: alice, machine: box, runner: claude }\n",
    )
    .unwrap();
    std::env::set_var("SHELBI_HOME", home.path());
    let seam = Arc::new(DroppableRelaySeam::default());
    remote_session::set_test_seam(Some(seam.clone()));

    let host = Host::Ssh {
        host: "box".into(),
    };
    let target = SessionTarget::slot("shelbi-demo", "alice");
    struct Guard<'a> {
        host: &'a Host,
        target: &'a SessionTarget,
    }
    impl Drop for Guard<'_> {
        fn drop(&mut self) {
            let _ = backend().kill(self.host, self.target);
            remote_session::set_test_seam(None);
            std::env::remove_var("SHELBI_HOME");
        }
    }
    let _guard = Guard {
        host: &host,
        target: &target,
    };

    let agent = SessionRef::Workspace("alice".into());
    // Before anything runs there: the machine answers, no session → idle.
    assert!(matches!(
        session_connect::connect("demo", &agent),
        Err(ConnectFailure::Idle(info)) if info.machine == "box"
    ));

    backend()
        .spawn(&host, &target, Some("printf 'hello from box\\n'; exec cat"))
        .expect("remote spawn");
    assert!(
        wait_for(Duration::from_secs(10), || {
            matches!(backend().probe(&host, &target, None), Liveness::Alive).then_some(())
        })
        .is_some(),
        "the remote session never came up"
    );

    // Attach through the relay, as the desktop's Workspace view does.
    let c = wait_for(Duration::from_secs(10), || session_connect::connect("demo", &agent).ok())
        .expect("attach through the relay");
    let rx = c.events.into_inner();
    assert!(read_until(&rx, "hello from box"), "the attach replays the remote screen");
    c.conn.input(b"ping\r").unwrap();
    assert!(read_until(&rx, "ping"), "input reaches the remote session");

    // The SSH connection dies: the stream ends (no exit, nothing left to read).
    seam.drop_ssh();
    let ended = wait_for(Duration::from_secs(10), || match rx.recv_timeout(Duration::from_millis(100)) {
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => Some(()),
        _ => None,
    });
    assert!(ended.is_some(), "the client's stream ends when the relay's SSH dies");
    drop(c.conn);

    // While the machine can't be reached the client is told Unreachable (it
    // keeps reconnecting), never that the session is gone.
    remote_session::set_test_seam(Some(Arc::new(UnreachableSeam)));
    assert!(matches!(
        session_connect::connect("demo", &agent),
        Err(ConnectFailure::Unreachable(_))
    ));

    // The machine is back: a fresh relay, and the agent kept running all along.
    remote_session::set_test_seam(Some(seam.clone()));
    let again = session_connect::connect("demo", &agent).expect("reattach after the drop");
    let rx = again.events.into_inner();
    assert!(read_until(&rx, "ping"), "the replay carries the exchange from before the drop");
    again.conn.input(b"pong\r").unwrap();
    assert!(read_until(&rx, "pong"), "the reattached session takes input");
}
