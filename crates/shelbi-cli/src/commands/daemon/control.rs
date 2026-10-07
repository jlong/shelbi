//! The daemon's **mutation control socket** (`control.sock`), the one owner of
//! issue mutations. See the "Removing tmux" plan, "The daemon executes
//! mutations", and [`shelbi_proto::control`] for the wire protocol.
//!
//! A client connects, says hello, then either runs one mutation or subscribes
//! to change notifications. For a mutation the daemon:
//!
//! - runs **one mutation per issue at a time**, queued — a per-`(project, id)`
//!   mutex each job holds for its duration, so mutations on *different* issues
//!   run concurrently;
//! - takes an **expected state** (`status` + `updated_at`) and rejects the job
//!   if the issue has already moved on, and **rechecks** it immediately before
//!   the irreversible step (via the `recheck` closure `mutate::apply` invokes);
//! - **finishes even if the client disconnects** — the job runs on a detached
//!   thread holding its own output channel clone, so a closed socket only makes
//!   the output sends no-op; the merge/dispatch still completes;
//! - streams progress and the result to the requesting client and **announces
//!   the change to every other connected client**.
//!
//! All std threads, no async — matching the hub socket next door. The hub
//! socket's worker/event protocol is untouched.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use shelbi_orchestrator::mutate::{self, MutateError, OutputSink};
use shelbi_proto::control::{
    self, ChangeNote, ClientMsg, MutationError, MutationKind, MutationRequest, ReviewRole,
    ReviewSessionOp, ReviewSessionRequest, ServerMsg, Stream, WorkspaceSessionOp,
    WorkspaceSessionRequest, CONTROL_PROTOCOL_VERSION,
};
use shelbi_state::CLIENT_VERSION;

/// Shared control-socket state: per-issue serialization locks and the set of
/// connected subscribers to notify of changes. Cheap to clone (`Arc`).
#[derive(Clone)]
pub(super) struct ControlState {
    inner: Arc<Inner>,
}

/// Key into the per-issue lock table: `(project, issue id)`.
type IssueKey = (String, String);

/// The mutation executor the daemon runs under the per-issue lock. Defaults to
/// [`mutate::apply`]; a test substitutes a stub so no real git/`gh`/agent runs.
/// Signature mirrors [`mutate::apply`].
type ApplyFn = dyn Fn(
        &str,
        &str,
        &MutationKind,
        &mut dyn OutputSink,
        &mut dyn FnMut() -> Result<(), MutateError>,
    ) -> Result<ChangeNote, MutateError>
    + Send
    + Sync;

/// The project/shelbi quit and daemon-stop operations behind the control
/// socket's lifecycle commands (removing-tmux Phase 4f). Behind a trait so the
/// in-process control-socket tests can assert the wiring (which command ran,
/// whether the daemon was asked to stop) without ending real sessions or
/// killing the test's own process. The production impl composes
/// [`shelbi_orchestrator::quit`] and the daemon's shutdown.
pub(super) trait LifecycleOps: Send + Sync {
    /// Quit one project: end its sessions, drain the quit barrier, mark closed.
    fn quit_project(&self, project: &str);
    /// Quit Shelbi: close every project and end all sessions (but do NOT stop
    /// the daemon yet — the handler acks the client first, then calls
    /// [`stop_daemon`](Self::stop_daemon)).
    fn quit_shelbi(&self);
    /// Stop the daemon: flip the shared stop flag and wake the hub accept loop
    /// so the process drains and exits. Nothing restarts it (the projects are
    /// already closed).
    fn stop_daemon(&self);
    /// This daemon's version, for the out-of-date check against a subscriber's
    /// announced client version.
    fn daemon_version(&self) -> String;
}

/// The production [`LifecycleOps`]: real quit composition + daemon shutdown.
pub(super) struct DaemonLifecycle {
    /// The daemon's shared stop flag (shared with the hub + control accept
    /// loops and the idle monitor).
    pub stop: Arc<AtomicBool>,
    /// The hub socket path — a self-connect to it wakes the blocking hub accept
    /// loop so a stop takes effect promptly (the same wake the idle monitor and
    /// signal path use).
    pub hub_sock: std::path::PathBuf,
}

impl LifecycleOps for DaemonLifecycle {
    fn quit_project(&self, project: &str) {
        let _ = shelbi_orchestrator::quit::quit_project(project);
    }

    fn quit_shelbi(&self) {
        let _ = shelbi_orchestrator::quit::quit_shelbi();
    }

    fn stop_daemon(&self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking hub accept loop; the control accept loop polls the
        // flag on its own 50ms cadence so it needs no wake.
        let _ = UnixStream::connect(&self.hub_sock);
    }

    fn daemon_version(&self) -> String {
        CLIENT_VERSION.to_string()
    }
}

/// An inert [`LifecycleOps`] for the unit tests that never exercise the quit
/// path: quitting is a no-op and the daemon is never stopped, so a test that
/// only checks the mutation / broadcast behavior can't accidentally kill the
/// test process.
#[cfg(test)]
struct InertLifecycle;

#[cfg(test)]
impl LifecycleOps for InertLifecycle {
    fn quit_project(&self, _project: &str) {}
    fn quit_shelbi(&self) {}
    fn stop_daemon(&self) {}
    fn daemon_version(&self) -> String {
        CLIENT_VERSION.to_string()
    }
}

struct Inner {
    /// One mutex per `(project, id)`. A mutation job holds its issue's mutex for
    /// its whole duration; different issues use different mutexes and run
    /// concurrently. Entries are never removed — there are only as many as there
    /// are distinct issues touched in a daemon's lifetime.
    issue_locks: Mutex<HashMap<IssueKey, Arc<Mutex<()>>>>,
    /// Connected subscriber connections, keyed by connection id, each with the
    /// sender feeding its writer thread.
    subscribers: Mutex<HashMap<u64, Sender<ServerMsg>>>,
    next_conn_id: AtomicU64,
    /// The mutation executor (see [`ApplyFn`]).
    apply: Box<ApplyFn>,
    /// The quit/stop operations behind the lifecycle commands.
    lifecycle: Arc<dyn LifecycleOps>,
}

impl ControlState {
    /// Production state: the real `mutate::apply` executor and the given
    /// lifecycle ops (which capture the daemon's stop flag + hub socket).
    pub(super) fn production(lifecycle: Arc<dyn LifecycleOps>) -> Self {
        Self::with_apply_and_lifecycle(
            Box::new(|project, id, kind, sink, recheck| {
                mutate::apply(project, id, kind, sink, recheck)
            }),
            lifecycle,
        )
    }

    /// Test/default state: production executor, inert lifecycle ops (no project
    /// is quit and the daemon is never stopped). Used by the broadcast/lock
    /// unit tests that never exercise the lifecycle path.
    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self::with_apply(Box::new(|project, id, kind, sink, recheck| {
            mutate::apply(project, id, kind, sink, recheck)
        }))
    }

    #[cfg(test)]
    fn with_apply(apply: Box<ApplyFn>) -> Self {
        Self::with_apply_and_lifecycle(apply, Arc::new(InertLifecycle))
    }

    fn with_apply_and_lifecycle(apply: Box<ApplyFn>, lifecycle: Arc<dyn LifecycleOps>) -> Self {
        Self {
            inner: Arc::new(Inner {
                issue_locks: Mutex::new(HashMap::new()),
                subscribers: Mutex::new(HashMap::new()),
                next_conn_id: AtomicU64::new(1),
                apply,
                lifecycle,
            }),
        }
    }

    fn issue_lock(&self, project: &str, id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.inner.issue_locks.lock().unwrap();
        locks
            .entry((project.to_string(), id.to_string()))
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn register(&self, conn_id: u64, tx: Sender<ServerMsg>) {
        self.inner.subscribers.lock().unwrap().insert(conn_id, tx);
    }

    fn unregister(&self, conn_id: u64) {
        self.inner.subscribers.lock().unwrap().remove(&conn_id);
    }

    /// Announce `note` to every subscriber except `origin` (which gets the
    /// mutation's own `Done`). A dead subscriber's send just fails and is
    /// ignored; its handler unregisters it on disconnect.
    fn broadcast(&self, origin: u64, note: &ChangeNote) {
        let subs = self.inner.subscribers.lock().unwrap();
        for (&conn_id, tx) in subs.iter() {
            if conn_id == origin {
                continue;
            }
            let _ = tx.send(ServerMsg::Changed(note.clone()));
        }
    }

    /// Push `msg` to *every* subscriber (the reload re-exec signal, which must
    /// reach all attached clients including whoever asked for the reload if it
    /// happens to be subscribed). A dead subscriber's send just fails.
    fn broadcast_all(&self, msg: &ServerMsg) {
        let subs = self.inner.subscribers.lock().unwrap();
        for tx in subs.values() {
            let _ = tx.send(msg.clone());
        }
    }
}

/// Bind the control socket (0600, like the hub socket). Returns the listener for
/// [`serve`] to run on its own thread.
pub(super) fn bind(path: &std::path::Path) -> anyhow::Result<UnixListener> {
    use anyhow::Context;
    use std::os::unix::fs::PermissionsExt;
    // A stale socket from a crashed daemon blocks bind; the hub's single-instance
    // lock guarantees we are the only daemon, so removing it is safe.
    let _ = std::fs::remove_file(path);
    // Tighten the umask around bind() so the socket inode is created 0600 from
    // the start, closing the window between bind() and the chmod below where a
    // local peer could connect. Mask only the group/other bits (`0o077`), never
    // owner-execute: `umask` is process-global, and a value that cleared
    // owner-x (e.g. `0o177`) would strip the search bit from any *directory* a
    // concurrent thread creates in this same window, leaving it unusable
    // (EACCES). Harmless for a lone daemon process, but in the test binary many
    // threads create board/home dirs in parallel — see the rt-cli test env-lock
    // race. `0o077` yields the identical 0600 socket while leaving 0700 dirs.
    let prev_umask = unsafe { libc::umask(0o077) };
    let bind_result = UnixListener::bind(path);
    unsafe { libc::umask(prev_umask) };
    let listener =
        bind_result.with_context(|| format!("binding control socket at {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("chmod 600 {}", path.display()))?;
    Ok(listener)
}

/// Accept loop for the control socket. Polls `stop` between accepts (nonblocking
/// listener) so a SIGTERM stops it without a self-connect wake. Detached job
/// threads a connection spawned keep running to completion past this returning.
pub(super) fn serve(listener: UnixListener, state: ControlState, stop: Arc<AtomicBool>) {
    if listener.set_nonblocking(true).is_err() {
        eprintln!("shelbi daemon: control socket could not enter nonblocking mode; not serving");
        return;
    }
    while !stop.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                // The listener is nonblocking so we can poll `stop` between
                // accepts; on macOS/BSD an accepted socket *inherits* that flag,
                // which would make the per-connection blocking read loop treat a
                // momentary WouldBlock as EOF. Force each connection back to
                // blocking.
                let _ = stream.set_nonblocking(false);
                let state = state.clone();
                thread::spawn(move || handle_client(stream, state));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(e) => {
                eprintln!("shelbi daemon: control accept error: {e}");
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
}

/// Handle one control connection: hello, then a mix of mutate / subscribe
/// requests until the client goes away.
fn handle_client(stream: UnixStream, state: ControlState) {
    let conn_id = state.inner.next_conn_id.fetch_add(1, Ordering::SeqCst);
    let Ok(write_half) = stream.try_clone() else {
        return;
    };

    // One writer thread owns the socket write-half; job threads and the
    // broadcaster push whole `ServerMsg`s through this channel, so frames are
    // never interleaved mid-write.
    let (tx, rx): (Sender<ServerMsg>, Receiver<ServerMsg>) = std::sync::mpsc::channel();
    let writer = thread::spawn(move || writer_loop(write_half, rx));

    // The version the client announced in its hello, so a `Subscribe` can be
    // told straight away that it is out of date (Phase 4f).
    let mut client_version: Option<String> = None;

    let mut reader = FrameReader::new(stream);
    loop {
        let msg = match reader.read_frame::<ClientMsg>() {
            Ok(Some(m)) => m,
            Ok(None) => break,  // clean EOF
            Err(_) => break,    // framing/io error: drop the connection
        };
        match msg {
            ClientMsg::Hello {
                client_version: cv,
                ..
            } => {
                client_version = Some(cv);
                let _ = tx.send(ServerMsg::Hello {
                    protocol: CONTROL_PROTOCOL_VERSION,
                    daemon_version: state.inner.lifecycle.daemon_version(),
                });
            }
            ClientMsg::Subscribe => {
                state.register(conn_id, tx.clone());
                // If this subscriber announced a different version than ours it
                // is out of date: tell it to re-exec (a TUI) / prompt for
                // relaunch (the desktop app) right away, so it stops sending
                // commands before it does anything else.
                if let Some(cv) = &client_version {
                    if *cv != state.inner.lifecycle.daemon_version() {
                        let _ = tx.send(ServerMsg::Reexec {
                            reason: format!(
                                "client {cv} is out of date (daemon {})",
                                state.inner.lifecycle.daemon_version()
                            ),
                        });
                    }
                }
            }
            ClientMsg::Mutate(req) => {
                // Detached: the job outlives this connection so a client that
                // closes mid-merge doesn't abandon the work half-done.
                let state = state.clone();
                let tx = tx.clone();
                thread::spawn(move || run_job(conn_id, req, state, tx));
            }
            ClientMsg::QuitProject {
                request_id,
                project,
            } => {
                // Runs inline on this connection's handler thread (other clients
                // have their own threads). Ends the project's sessions, drains
                // its quit barrier, marks it closed, then acks.
                state.inner.lifecycle.quit_project(&project);
                let _ = tx.send(ServerMsg::Done { request_id });
            }
            ClientMsg::QuitShelbi { request_id } => {
                // Close every project and end all sessions, ACK the client, then
                // stop the daemon — acking before the stop so the reply isn't
                // lost to the shutdown race.
                state.inner.lifecycle.quit_shelbi();
                let _ = tx.send(ServerMsg::Done { request_id });
                state.inner.lifecycle.stop_daemon();
            }
            ClientMsg::ReloadClients { request_id } => {
                // Tell every attached client to re-exec, then ack the requester.
                state.broadcast_all(&ServerMsg::Reexec {
                    reason: "shelbi reload".to_string(),
                });
                let _ = tx.send(ServerMsg::Done { request_id });
            }
            ClientMsg::ReviewSession(req) => {
                // Detached like a mutation: the daemon owns the review sessions'
                // lifetime, so a client that detaches right after asking doesn't
                // abandon a half-spawned (or half-torn-down) interface.
                let tx = tx.clone();
                thread::spawn(move || run_review_session_job(req, tx));
            }
            ClientMsg::WorkspaceSession(req) => {
                // The dev-workspace twin of ReviewSession; detached for the same
                // reason (the daemon owns the editor/diff sessions' lifetime).
                let tx = tx.clone();
                thread::spawn(move || run_workspace_session_job(req, tx));
            }
        }
    }

    // The client is gone. Stop announcing to it and drop our sender so the
    // writer thread finishes once in-flight job threads release their clones.
    state.unregister(conn_id);
    drop(tx);
    let _ = writer.join();
}

/// Drain `rx`, encoding each message to a frame on the socket. Exits when the
/// channel closes (all senders dropped) or a write fails (client gone).
fn writer_loop(mut sock: UnixStream, rx: Receiver<ServerMsg>) {
    while let Ok(msg) = rx.recv() {
        let Ok(bytes) = control::encode(&msg) else {
            continue;
        };
        if sock.write_all(&bytes).is_err() || sock.flush().is_err() {
            // Client gone — keep draining so senders don't block, but stop
            // writing. (The channel is unbounded, so recv never blocks a sender.)
            break;
        }
    }
    // Drain any remaining queued messages so job-thread sends return promptly.
    while rx.recv().is_ok() {}
}

/// Run one mutation under its per-issue lock, with the expected-state gate and
/// the recheck closure, then report the result and broadcast the change.
fn run_job(origin: u64, req: MutationRequest, state: ControlState, tx: Sender<ServerMsg>) {
    let request_id = req.request_id;

    // Serialize per issue. `add` with no explicit id uses an empty key — those
    // can't collide meaningfully (the store's create is exclusive), so they
    // share one lock harmlessly.
    let lock = state.issue_lock(&req.project, &req.id);
    let _guard = lock.lock().unwrap_or_else(|p| p.into_inner());

    // Expected-state gate: refuse up front if the issue already moved on.
    if let Some(expected) = req.expected.clone() {
        match mutate::current_state(&req.project, &req.id) {
            Ok(actual) if actual != expected => {
                let _ = tx.send(ServerMsg::Failed {
                    request_id,
                    error: MutationError::Stale { expected, actual },
                });
                return;
            }
            Ok(_) => {}
            Err(e) => {
                let _ = tx.send(ServerMsg::Failed {
                    request_id,
                    error: e.into(),
                });
                return;
            }
        }
    }

    let mut sink = DaemonSink {
        tx: tx.clone(),
        request_id,
    };
    // Recheck closure: re-read current state and compare to `expected`,
    // immediately before the irreversible step `mutate::apply` performs.
    let project = req.project.clone();
    let id = req.id.clone();
    let expected = req.expected.clone();
    let mut recheck = move || -> Result<(), mutate::MutateError> {
        let Some(expected) = expected.clone() else {
            return Ok(());
        };
        let actual = mutate::current_state(&project, &id)?;
        if actual != expected {
            return Err(mutate::MutateError::Stale { expected, actual });
        }
        Ok(())
    };

    match (state.inner.apply)(&req.project, &req.id, &req.kind, &mut sink, &mut recheck) {
        Ok(note) => {
            let _ = tx.send(ServerMsg::Done { request_id });
            // Announce to the other connected clients.
            state.broadcast(origin, &note);
        }
        Err(e) => {
            let _ = tx.send(ServerMsg::Failed {
                request_id,
                error: e.into(),
            });
        }
    }
}

/// Carry out one review-session request (`rt-tui-review`): start/stop the
/// editor/diff/server sessions of a review slot through
/// [`shelbi_orchestrator::review_session`], then report the result keyed by
/// `request_id`. These are not issue mutations (no board change, no
/// expected-state gate), so they skip the per-issue lock and the recheck; the
/// orchestrator calls are themselves idempotent and best-effort.
fn run_review_session_job(req: ReviewSessionRequest, tx: Sender<ServerMsg>) {
    use shelbi_orchestrator::review_session::{self, ReviewContentRole};
    let request_id = req.request_id;
    let result = match req.op {
        ReviewSessionOp::Ensure { role } => {
            let role = match role {
                ReviewRole::Editor => ReviewContentRole::Editor,
                ReviewRole::Diff => ReviewContentRole::Diff,
            };
            review_session::ensure_content_session(&req.project, &req.task, role)
        }
        ReviewSessionOp::Close => review_session::close_review(&req.project, &req.task),
        ReviewSessionOp::Load { workspace } => {
            // Load a queued review task onto the chosen (free) slot — the same
            // path the tmux review-load Enter runs: check out the branch, run
            // the status's enter transition to boot and health-check the dev
            // server, and start the review agent. On success publish the
            // `ReviewOpened` layout event so subscribed clients open the native
            // review interface now the slot is serving
            // (`rt-tui-review-load-queued`). The load is self-locking (the
            // project-scoped review-load lock) and non-evicting, so a busy slot
            // is rejected rather than evicted.
            let loaded =
                shelbi_orchestrator::load::load_review_task(&req.project, &req.task, &workspace);
            if loaded.is_ok() {
                shelbi_state::publish_layout(
                    &req.project,
                    shelbi_state::LayoutEvent::ReviewOpened {
                        workspace: workspace.clone(),
                        task: req.task.clone(),
                    },
                );
            }
            loaded.map(|_| ())
        }
    };
    match result {
        Ok(()) => {
            let _ = tx.send(ServerMsg::Done { request_id });
        }
        Err(e) => {
            let _ = tx.send(ServerMsg::Failed {
                request_id,
                error: MutationError::Backend {
                    message: e.to_string(),
                },
            });
        }
    }
}

/// Carry out one workspace-session request (the workspace-sidebar task):
/// start/stop a dev workspace's editor/diff content sessions through
/// [`shelbi_orchestrator::workspace_session`], then report the result keyed by
/// `request_id`. The dev-workspace twin of [`run_review_session_job`]; like it,
/// these are not issue mutations, so they skip the per-issue lock and recheck —
/// the orchestrator calls are idempotent and best-effort.
fn run_workspace_session_job(req: WorkspaceSessionRequest, tx: Sender<ServerMsg>) {
    use shelbi_orchestrator::review_session::ReviewContentRole;
    use shelbi_orchestrator::workspace_session;
    let request_id = req.request_id;
    let result = match req.op {
        WorkspaceSessionOp::Ensure { role } => {
            let role = match role {
                ReviewRole::Editor => ReviewContentRole::Editor,
                ReviewRole::Diff => ReviewContentRole::Diff,
            };
            workspace_session::ensure_content_session(&req.project, &req.workspace, role)
        }
        WorkspaceSessionOp::Close => {
            workspace_session::close_content(&req.project, &req.workspace)
        }
    };
    match result {
        Ok(()) => {
            let _ = tx.send(ServerMsg::Done { request_id });
        }
        Err(e) => {
            let _ = tx.send(ServerMsg::Failed {
                request_id,
                error: MutationError::Backend {
                    message: e.to_string(),
                },
            });
        }
    }
}

/// An [`OutputSink`] that streams each line to the requesting client as a
/// [`ServerMsg::Line`]. A failed send (client gone) is ignored — the mutation
/// still runs to completion.
struct DaemonSink {
    tx: Sender<ServerMsg>,
    request_id: u64,
}

impl OutputSink for DaemonSink {
    fn emit(&mut self, stream: Stream, text: &str) {
        let _ = self.tx.send(ServerMsg::Line {
            request_id: self.request_id,
            stream,
            text: text.to_string(),
        });
    }
}

/// Reads length-prefixed control frames off a blocking socket, buffering partial
/// reads. (A sibling of the client's reader; kept local so the daemon doesn't
/// depend on `shelbi-client`.)
struct FrameReader {
    inner: UnixStream,
    buf: Vec<u8>,
    start: usize,
}

impl FrameReader {
    fn new(inner: UnixStream) -> Self {
        Self {
            inner,
            buf: Vec::with_capacity(4096),
            start: 0,
        }
    }

    fn read_frame<T: serde::de::DeserializeOwned>(
        &mut self,
    ) -> Result<Option<T>, shelbi_proto::ProtoError> {
        loop {
            match control::decode::<T>(&self.buf[self.start..]) {
                Ok((msg, consumed)) => {
                    self.start += consumed;
                    if self.start > 1 << 16 {
                        self.buf.drain(..self.start);
                        self.start = 0;
                    }
                    return Ok(Some(msg));
                }
                Err(shelbi_proto::ProtoError::Incomplete { .. }) => {
                    let mut chunk = [0u8; 4096];
                    // A read error (reset, timeout) is treated like EOF: the
                    // handler drops the connection either way.
                    let n = match self.inner.read(&mut chunk) {
                        Ok(n) => n,
                        Err(_) => return Ok(None),
                    };
                    if n == 0 {
                        // EOF, whole or partial frame — a clean close.
                        return Ok(None);
                    }
                    self.buf.extend_from_slice(&chunk[..n]);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_lock_is_shared_per_key_and_distinct_across_keys() {
        let state = ControlState::new();
        let a1 = state.issue_lock("p", "t1");
        let a2 = state.issue_lock("p", "t1");
        let b = state.issue_lock("p", "t2");
        // Same key → same mutex (so same-issue mutations serialize).
        assert!(Arc::ptr_eq(&a1, &a2));
        // Different key → different mutex (so different issues run concurrently).
        assert!(!Arc::ptr_eq(&a1, &b));
    }

    #[test]
    fn broadcast_reaches_other_subscribers_but_not_the_origin() {
        let state = ControlState::new();
        let (tx1, rx1) = std::sync::mpsc::channel();
        let (tx2, rx2) = std::sync::mpsc::channel();
        state.register(1, tx1);
        state.register(2, tx2);

        let note = ChangeNote {
            project: "p".into(),
            id: "t1".into(),
            verb: "move".into(),
            status: "todo".into(),
            updated_at: String::new(),
        };
        state.broadcast(1, &note);

        // Origin (conn 1) is not notified of its own change.
        assert!(rx1.try_recv().is_err());
        // The other subscriber is.
        match rx2.try_recv() {
            Ok(ServerMsg::Changed(got)) => assert_eq!(got, note),
            other => panic!("expected a Changed note, got {other:?}"),
        }

        // After unregister, a subscriber stops receiving.
        state.unregister(2);
        state.broadcast(1, &note);
        assert!(rx2.try_recv().is_err());
    }

    // --- in-process control-socket tests with a STUB executor ----------------
    //
    // These drive the real control server (ControlState + serve + run_job) over
    // a real socket, in-process, with `mutate::apply` replaced by a stub so no
    // git/`gh`/agent runs. The stub performs the status move through the real
    // filesystem store (so the expected-state gate sees the change) and records
    // whether it "merged"/"started an agent"; one stub can block on a barrier to
    // model a mutation still running when the client disconnects.

    use crate::commands::test_support::ENV_LOCK;
    use shelbi_client::ControlClient;
    use shelbi_core::Column;
    use shelbi_proto::control::{ExpectedState, MutationKind};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AOrd};
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    const T0: &str = "2026-01-01T00:00:00+00:00";

    fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
        let start = Instant::now();
        while start.elapsed() < deadline {
            if cond() {
                return true;
            }
            thread::sleep(Duration::from_millis(20));
        }
        cond()
    }

    /// An isolated `SHELBI_HOME` with a filesystem project `p`. Holds the env
    /// lock for the whole test (the daemon job threads read `SHELBI_HOME`).
    struct Home {
        path: PathBuf,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl Home {
        fn new(tag: &str) -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
            let path = std::env::temp_dir().join(format!(
                "shb-ctl-unit-{tag}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(path.join("projects/p/tasks")).unwrap();
            std::fs::write(
                path.join("projects/p.yaml"),
                "name: p\nrepo: /tmp/p\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/p\n\
                 workspaces:\n  - { name: dev, machine: local, runner: claude }\n",
            )
            .unwrap();
            std::env::set_var("SHELBI_HOME", &path);
            Self { path, _guard: guard }
        }

        fn write_issue(&self, id: &str, column: &str) {
            std::fs::write(
                self.path.join(format!("projects/p/tasks/{id}.md")),
                format!(
                    "---\nid: {id}\ntitle: {id}\ncolumn: {column}\npriority: 0\n\
                     created_at: {T0}\nupdated_at: {T0}\n---\nbody\n"
                ),
            )
            .unwrap();
        }

        fn column_of(&self, id: &str) -> String {
            std::fs::read_to_string(self.path.join(format!("projects/p/tasks/{id}.md")))
                .unwrap_or_default()
                .lines()
                .find_map(|l| l.strip_prefix("column:"))
                .map(|v| v.trim().to_string())
                .unwrap_or_default()
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            std::env::remove_var("SHELBI_HOME");
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    /// Records what the stub executor did, so a test can assert that the loser of
    /// a race never ran (no merge / no second agent) and that a job finished.
    #[derive(Default)]
    struct Recorder {
        calls: Mutex<Vec<(String, String)>>,
        merges: AtomicUsize,
        agent_starts: AtomicUsize,
        started: AtomicBool,
        finished: AtomicBool,
    }

    /// A [`ControlState`] whose executor is a stub: it honors the recheck, records
    /// the call, simulates the side effect (merge / agent start) without touching
    /// git/`gh`/tmux, optionally blocks on `gate`, then performs the real status
    /// move through the filesystem store so the next request's gate sees it.
    fn stub_state(rec: Arc<Recorder>, gate: Option<Arc<Barrier>>) -> ControlState {
        ControlState::with_apply(Box::new(move |project, id, kind, _sink, recheck| {
            recheck()?;
            rec.started.store(true, AOrd::SeqCst);
            rec.calls
                .lock()
                .unwrap()
                .push((id.to_string(), kind.verb().to_string()));
            match kind {
                MutationKind::Approve => {
                    rec.merges.fetch_add(1, AOrd::SeqCst);
                }
                MutationKind::Start { .. } => {
                    rec.agent_starts.fetch_add(1, AOrd::SeqCst);
                }
                _ => {}
            }
            if let Some(b) = &gate {
                b.wait();
            }
            let target_name = match kind {
                MutationKind::Move { to, .. } => to.as_str(),
                MutationKind::Approve => "done",
                MutationKind::Reject { .. } => "todo",
                MutationKind::Start { .. } => "in-progress",
                _ => "todo",
            };
            let target = Column::from_status_id(target_name);
            let store = shelbi_state::issue_store_for(project).map_err(MutateError::backend)?;
            store
                .move_status(id, &target, "test:stub")
                .map_err(MutateError::backend)?;
            rec.finished.store(true, AOrd::SeqCst);
            Ok(ChangeNote {
                project: project.to_string(),
                id: id.to_string(),
                verb: kind.verb().to_string(),
                status: target.as_str().to_string(),
                updated_at: String::new(),
            })
        }))
    }

    /// A control server running on a real socket in a background thread.
    struct Served {
        sock: PathBuf,
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
    }

    fn serve_bg(state: ControlState, tag: &str) -> Served {
        let sock = PathBuf::from(format!("/tmp/shb-ctlu-{}-{tag}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        let listener = bind(&sock).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let handle = {
            let stop = stop.clone();
            thread::spawn(move || serve(listener, state, stop))
        };
        assert!(
            wait_until(Duration::from_secs(5), || ControlClient::connect(
                &sock,
                "test"
            )
            .is_ok()),
            "control server never came up"
        );
        Served {
            sock,
            stop,
            handle: Some(handle),
        }
    }

    impl Drop for Served {
        fn drop(&mut self) {
            self.stop.store(true, AOrd::SeqCst);
            if let Some(h) = self.handle.take() {
                let _ = h.join();
            }
            let _ = std::fs::remove_file(&self.sock);
        }
    }

    fn req(request_id: u64, id: &str, kind: MutationKind, expected: ExpectedState) -> MutationRequest {
        MutationRequest {
            request_id,
            project: "p".into(),
            id: id.into(),
            expected: Some(expected),
            kind,
        }
    }

    fn at_review() -> ExpectedState {
        ExpectedState {
            status: "review".into(),
            updated_at: T0.into(),
        }
    }

    fn at_todo() -> ExpectedState {
        ExpectedState {
            status: "todo".into(),
            updated_at: T0.into(),
        }
    }

    fn is_stale(r: &Result<(), shelbi_client::ClientError>) -> bool {
        matches!(
            r,
            Err(shelbi_client::ClientError::Mutation(MutationError::Stale { .. }))
        )
    }

    /// Run `kind` against the server at `sock` on its own thread/connection.
    fn fire(
        sock: &std::path::Path,
        request_id: u64,
        kind: MutationKind,
        expected: ExpectedState,
    ) -> thread::JoinHandle<Result<(), shelbi_client::ClientError>> {
        let sock = sock.to_path_buf();
        thread::spawn(move || {
            let mut client = ControlClient::connect(&sock, "test").unwrap();
            client.mutate(&req(request_id, "t1", kind, expected), &mut |_s, _t| {})
        })
    }

    #[test]
    fn approve_against_reject_from_two_clients_one_wins_the_other_is_stale_with_no_merge() {
        let home = Home::new("appvrej");
        home.write_issue("t1", "review");
        let rec = Arc::new(Recorder::default());
        let served = serve_bg(stub_state(rec.clone(), None), "appvrej");

        let a = fire(&served.sock, 1, MutationKind::Approve, at_review());
        let r = fire(
            &served.sock,
            2,
            MutationKind::Reject {
                reason: "no".into(),
            },
            at_review(),
        );
        let ra = a.join().unwrap();
        let rr = r.join().unwrap();

        let oks = [&ra, &rr].iter().filter(|x| x.is_ok()).count();
        let stales = [&ra, &rr].iter().filter(|x| is_stale(x)).count();
        assert_eq!(oks, 1, "exactly one of approve/reject wins: {ra:?} {rr:?}");
        assert_eq!(stales, 1, "the loser is rejected as stale: {ra:?} {rr:?}");
        // The loser never reached the executor — so it ran no merge.
        assert_eq!(
            rec.calls.lock().unwrap().len(),
            1,
            "only the winner executed"
        );
        assert!(
            rec.merges.load(AOrd::SeqCst) <= 1,
            "the loser performed no merge"
        );
    }

    #[test]
    fn the_same_issue_started_twice_at_once_starts_exactly_one_agent() {
        let home = Home::new("dbldisp");
        home.write_issue("t1", "todo");
        let rec = Arc::new(Recorder::default());
        let served = serve_bg(stub_state(rec.clone(), None), "dbldisp");

        let start = || MutationKind::Start {
            workspace: Some("dev".into()),
            branch: None,
            reason: None,
            force: false,
        };
        let a = fire(&served.sock, 1, start(), at_todo());
        let b = fire(&served.sock, 2, start(), at_todo());
        let ra = a.join().unwrap();
        let rb = b.join().unwrap();

        assert_eq!(
            [&ra, &rb].iter().filter(|x| x.is_ok()).count(),
            1,
            "exactly one dispatch wins: {ra:?} {rb:?}"
        );
        assert_eq!([&ra, &rb].iter().filter(|x| is_stale(x)).count(), 1);
        assert_eq!(
            rec.agent_starts.load(AOrd::SeqCst),
            1,
            "exactly one agent is started"
        );
        // The canonical on-disk spelling of the active status.
        assert_eq!(home.column_of("t1"), "in_progress");
    }

    #[test]
    fn a_merge_crossing_mutation_finishes_after_the_client_disconnects() {
        let home = Home::new("midmerge");
        home.write_issue("t1", "review");
        let rec = Arc::new(Recorder::default());
        // The approve stub blocks at this barrier, so the "merge" is still in
        // flight when we close the client below.
        let gate = Arc::new(Barrier::new(2));
        let served = serve_bg(stub_state(rec.clone(), Some(gate.clone())), "midmerge");

        // Send approve on a raw connection, then close it WITHOUT reading the
        // reply, while the stub is blocked mid-merge.
        {
            let mut raw = UnixStream::connect(&served.sock).unwrap();
            raw.write_all(
                &control::encode(&ClientMsg::Hello {
                    protocol: CONTROL_PROTOCOL_VERSION,
                    client_version: "test".into(),
                })
                .unwrap(),
            )
            .unwrap();
            raw.write_all(
                &control::encode(&ClientMsg::Mutate(req(
                    1,
                    "t1",
                    MutationKind::Approve,
                    at_review(),
                )))
                .unwrap(),
            )
            .unwrap();
            raw.flush().unwrap();
            // Wait until the job is inside the (blocked) merge, then leave.
            assert!(
                wait_until(Duration::from_secs(5), || rec.started.load(AOrd::SeqCst)),
                "the job never started the merge"
            );
            raw.shutdown(Shutdown::Both).unwrap();
        }

        // The client is gone; now let the merge proceed. It must finish and the
        // status must be written even though no one is listening.
        gate.wait();
        assert!(
            wait_until(Duration::from_secs(5), || rec.finished.load(AOrd::SeqCst)),
            "the merge did not finish after the client left"
        );
        assert!(
            wait_until(Duration::from_secs(5), || home.column_of("t1") == "done"),
            "the status was not written (got `{}`)",
            home.column_of("t1")
        );
        assert_eq!(rec.merges.load(AOrd::SeqCst), 1);
    }

    // --- lifecycle commands (Phase 4f) ---------------------------------------
    //
    // These drive the real control server with a RECORDING LifecycleOps so the
    // wiring (which command ran, in what order, whether the daemon was asked to
    // stop) is asserted without ending real sessions or killing the test
    // process. The quit *composition* (ordering, scoping, barrier) is tested in
    // `shelbi_orchestrator::quit`.

    use shelbi_client::Notice;

    /// Records the lifecycle operations the control handler invokes.
    #[derive(Default)]
    struct RecordingLifecycle {
        calls: Mutex<Vec<String>>,
        version: String,
    }

    impl RecordingLifecycle {
        fn new(version: &str) -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(Vec::new()),
                version: version.to_string(),
            })
        }
        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    impl LifecycleOps for RecordingLifecycle {
        fn quit_project(&self, project: &str) {
            self.calls.lock().unwrap().push(format!("quit_project:{project}"));
        }
        fn quit_shelbi(&self) {
            self.calls.lock().unwrap().push("quit_shelbi".into());
        }
        fn stop_daemon(&self) {
            self.calls.lock().unwrap().push("stop_daemon".into());
        }
        fn daemon_version(&self) -> String {
            self.version.clone()
        }
    }

    /// A control server with a recording lifecycle and the production mutation
    /// executor (never reached by these tests).
    fn serve_lifecycle(life: Arc<RecordingLifecycle>, tag: &str) -> Served {
        let state = ControlState::with_apply_and_lifecycle(
            Box::new(|project, id, kind, sink, recheck| mutate::apply(project, id, kind, sink, recheck)),
            life,
        );
        serve_bg(state, tag)
    }

    #[test]
    fn quit_project_command_runs_quit_project_and_acks() {
        let life = RecordingLifecycle::new("v1");
        let served = serve_lifecycle(life.clone(), "qp-cmd");
        let mut client = ControlClient::connect(&served.sock, "v1").unwrap();
        client.quit_project("alpha").unwrap();
        assert!(
            wait_until(Duration::from_secs(5), || life.calls()
                == vec!["quit_project:alpha".to_string()]),
            "quit_project was invoked for the right project: {:?}",
            life.calls()
        );
    }

    #[test]
    fn quit_shelbi_command_acks_then_stops_the_daemon() {
        // AC4: quit shelbi ends everything and stops the daemon. The handler
        // must ack BEFORE stopping (so the reply isn't lost), so the recorded
        // order is quit_shelbi → stop_daemon and the client's call returns Ok.
        let life = RecordingLifecycle::new("v1");
        let served = serve_lifecycle(life.clone(), "qs-cmd");
        let mut client = ControlClient::connect(&served.sock, "v1").unwrap();
        client.quit_shelbi().unwrap();
        assert!(
            wait_until(Duration::from_secs(5), || life.calls()
                == vec!["quit_shelbi".to_string(), "stop_daemon".to_string()]),
            "quit_shelbi then stop_daemon, in that order: {:?}",
            life.calls()
        );
    }

    #[test]
    fn reload_broadcasts_reexec_to_a_subscriber() {
        // AC5: `shelbi reload` signals attached clients to re-exec. A subscribed
        // client receives a Reexec push when another client sends reload.
        let life = RecordingLifecycle::new("v1");
        let served = serve_lifecycle(life, "reload");

        // Subscriber A.
        let a = ControlClient::connect(&served.sock, "v1").unwrap();
        let mut sub = a.subscribe().unwrap();
        // Give the daemon a moment to register A before B triggers the reload.
        thread::sleep(Duration::from_millis(150));

        // Client B triggers the reload.
        let mut b = ControlClient::connect(&served.sock, "v1").unwrap();
        b.reload_clients().unwrap();

        match sub.recv() {
            Ok(Some(Notice::Reexec { reason })) => {
                assert!(reason.contains("reload"), "reason names the reload: {reason}")
            }
            other => panic!("subscriber A should receive a Reexec push, got {other:?}"),
        }
    }

    #[test]
    fn an_out_of_date_subscriber_is_told_to_reexec_immediately() {
        // AC6/AC7: a client whose version differs from the daemon's is out of
        // date. On subscribe the daemon pushes Reexec straight away so the
        // client re-execs / relaunches and sends no mutations.
        let life = RecordingLifecycle::new("daemon-NEW");
        let served = serve_lifecycle(life, "stale-sub");
        let a = ControlClient::connect(&served.sock, "client-OLD").unwrap();
        assert!(a.is_out_of_date("client-OLD"), "daemon reports a newer version");
        let mut sub = a.subscribe().unwrap();
        match sub.recv() {
            Ok(Some(Notice::Reexec { reason })) => {
                assert!(reason.contains("out of date"), "reason: {reason}")
            }
            other => panic!("an out-of-date subscriber must be told to re-exec, got {other:?}"),
        }
    }
}
