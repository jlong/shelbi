//! A remote window's terminal panes: sessions on the far host itself (its
//! orchestrator, its local workspaces' agents, review and workspace content,
//! user shells), attached through the host's relay link.
//!
//! [`RemoteSessionConnector`] is the [`Connector`] a remote window's
//! [`crate::source::ProjectLink`] carries. An attach asks the far relay for
//! its sessions (`ListSessions`), finds the one [`discovery_name`] names,
//! opens a stream to it, and connects over that stream exactly as a local
//! pane connects over its socket (`Connection::connect`, the one code path
//! both share). The outcomes are classified as the hub's own remote
//! workspace attach classifies them (`session_connect::classify_remote`):
//!
//! - the channel is down, or drops mid-attach: [`ConnectFailure::Unreachable`],
//!   so the pane keeps reconnecting (the session may well still be running);
//! - a workspace agent with no session: [`ConnectFailure::Idle`];
//! - a content session (editor, diff, shell) not registered yet:
//!   [`ConnectFailure::Starting`], since the client just asked the far daemon
//!   to ensure it (through the link's `control` service, as locally);
//! - the orchestrator missing: [`ConnectFailure::Awaiting`], since the far
//!   project declares one (a relaunch in progress);
//! - a session that ran and exited: [`ConnectFailure::Message`].
//!
//! **Reconnect.** The link is the host's shared [`HostLink`]. While its
//! channel is down, each attempt first runs the [`RemoteConnector::connect`]
//! check (the host answers, still has the project, its daemon runs) and only
//! then opens a fresh channel. A reconnect therefore never starts the far
//! daemon: a stopped one ends the pane's loop with the
//! [`RemoteError::DaemonStopped`] message, kept as
//! [`RemoteSessionConnector::refusal`] for the window to offer Start. Sessions
//! resume by attach and resync (the replay); the agents never stopped.
//!
//! **Two hops.** A workspace the far project runs on another of its own
//! machines is not on the far host's session list, and this machine has no
//! route (and maybe no credentials) to that machine. When a workspace-scoped
//! target misses on the far list and the far project says the workspace is
//! remote ([`crate::source::WorkspaceDetails::is_remote`]), the pane opens the
//! relay's `session` service instead: the far `shelbi relay` resolves the
//! target as the far hub does, through the far side's own relay to that
//! machine, and bridges the session's frames, so the pane speaks the session
//! protocol end to end ([`attach_through`]). This machine's `ssh` gains no
//! `-R` or `-L`; the far side's relay and forward stay there. Either hop
//! dropping ends the stream, and the pane reconnects as for any drop: the
//! far machine being unreachable is [`ConnectFailure::Unreachable`] (the
//! shared notice), and so is the third machine, named in the reason.
//!
//! **Version.** Every reconnect checks the host with
//! [`crate::remote_window::reconnect_check`]: the far Shelbi may have been
//! upgraded while the channel was down, and a host that no longer opens
//! natively refuses ([`RemoteError::NoLongerNative`], kept as the refusal
//! for the window to offer Reopen) rather than have commands sent to it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelbi_client::{ClientError, Connection};
use shelbi_proto::capability;

use super::{
    discovery_name, no_live_session_error, starting_error, ConnectFailure, Connected, Connector,
    GridSize, IdleInfo, SessionRef,
};
use crate::launch::link::HostLink;
use crate::launch::remote::{RemoteConnector, RemoteError, RemoteSession};
use crate::source::remote::RelayLink;

/// The reason a pane shows while its host's channel is down.
pub const CONNECTION_LOST: &str = "The connection was lost.";

/// How long the title probe waits for the orchestrator's answer.
const TITLE_DEADLINE: Duration = Duration::from_millis(1500);

/// The [`Connector`] for a project on another machine. See the module docs.
pub struct RemoteSessionConnector {
    link: Arc<HostLink>,
    session: RemoteSession,
    remote: Arc<dyn RemoteConnector>,
    refusal: Mutex<Option<RemoteError>>,
}

impl RemoteSessionConnector {
    /// Panes of `session`'s project, attached over `link` (the host's shared
    /// link), whose reconnects check the host with `remote` first.
    pub fn new(link: Arc<HostLink>, session: RemoteSession, remote: Arc<dyn RemoteConnector>) -> Self {
        RemoteSessionConnector {
            link,
            session,
            remote,
            refusal: Mutex::new(None),
        }
    }

    /// Why the last reconnect was refused, when the host answered but
    /// wouldn't have it ([`RemoteError::DaemonStopped`]: the window offers to
    /// start Shelbi there). Cleared by the next attempt that gets through.
    pub fn refusal(&self) -> Option<RemoteError> {
        self.refusal.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The host's channel: the current one, or a fresh one once the host
    /// checks out.
    fn channel(&self) -> Result<Arc<dyn RelayLink>, ConnectFailure> {
        let opened = self
            .link
            .connect(&|| crate::remote_window::reconnect_check(self.remote.as_ref(), &self.session));
        let mut refusal = self.refusal.lock().unwrap_or_else(|p| p.into_inner());
        match opened {
            Ok(link) => {
                *refusal = None;
                Ok(link)
            }
            Err(RemoteError::Unreachable { reason, .. }) => Err(ConnectFailure::Unreachable(reason)),
            Err(error) => {
                let message = error.to_string();
                *refusal = Some(error);
                Err(ConnectFailure::Message(message))
            }
        }
    }
}

impl Connector for RemoteSessionConnector {
    fn connect(&self, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
        let link = self.channel()?;
        match attach_over(&*link, project, target) {
            Err(miss @ (ConnectFailure::Idle(_) | ConnectFailure::Starting(_))) => {
                second_hop(link, project, target, miss)
            }
            attached => attached,
        }
    }
}

/// A workspace-scoped target with no session on the far host: idle (or not
/// started yet) there, or, when the far project runs the workspace on another
/// of its machines, attached through the far side ([`attach_through`]).
/// `miss` is the far-host outcome, kept for a target that isn't remote.
fn second_hop(
    link: Arc<dyn RelayLink>,
    project: &str,
    target: &SessionRef,
    miss: ConnectFailure,
) -> Result<Connected, ConnectFailure> {
    use crate::source::ProjectSource;
    let Some(workspace) = target.workspace() else {
        return Err(miss);
    };
    let source = crate::source::remote::RemoteSource::new(project, link.clone());
    match source.workspace_details(workspace) {
        Ok(details) if details.is_remote => attach_through(&*link, project, target, &details.machine),
        _ => Err(miss),
    }
}

/// Attach to `target` in `project` through the far side: the relay's
/// `session` service resolves it where the far project runs it (on
/// `machine`, another of its machines, through the far side's own relay
/// there) and bridges its frames. A full replay, as [`attach_over`].
/// Blocking.
///
/// Classified as a far-local attach is: either hop unreachable is
/// [`ConnectFailure::Unreachable`] (the reason names the far side's view of
/// it), a workspace agent with no session [`ConnectFailure::Idle`] on
/// `machine`, a content session not registered yet
/// [`ConnectFailure::Starting`], one that exited a message.
pub fn attach_through(
    link: &dyn RelayLink,
    project: &str,
    target: &SessionRef,
    machine: &str,
) -> Result<Connected, ConnectFailure> {
    use shelbi_proto::{RelayService, SessionOpenError};
    let host = link.host();
    let want = discovery_name(project, target);
    let service = RelayService::Session {
        project: project.to_string(),
        target: target.to_target(),
    };
    let stream = match link.open_service(service) {
        Ok(stream) => stream,
        Err(ClientError::Relay(error)) => {
            return Err(match SessionOpenError::from_wire(&error) {
                SessionOpenError::Unreachable { message } => ConnectFailure::Unreachable(message),
                SessionOpenError::NoSession { exited: true } => {
                    ConnectFailure::Message(no_live_session_error(&want, None))
                }
                SessionOpenError::NoSession { exited: false } => match target {
                    SessionRef::Workspace(name) => ConnectFailure::Idle(IdleInfo {
                        name: name.clone(),
                        machine: machine.to_string(),
                        branch: None,
                    }),
                    _ => ConnectFailure::Starting(starting_error(&want)),
                },
                SessionOpenError::Failed { message } => {
                    ConnectFailure::Message(format!("attaching to `{want}` on `{machine}` through `{host}`: {message}"))
                }
            })
        }
        Err(other) => return Err(relay_failure(host, &want, other)),
    };
    connect_stream(stream, host, &want)
}

/// `error` from the relay as a connect failure: the channel going away is
/// [`ConnectFailure::Unreachable`] (keep reconnecting), anything else a
/// message naming what was being attached.
fn relay_failure(host: &str, want: &str, error: ClientError) -> ConnectFailure {
    match error {
        ClientError::RelayUnreachable => ConnectFailure::Unreachable(CONNECTION_LOST.to_string()),
        other => ConnectFailure::Message(format!("attaching to `{want}` on `{host}`: {other}")),
    }
}

/// Attach to `target` in `project` on `link`'s host with a full replay.
/// Blocking: run it off the UI thread.
pub fn attach_over(link: &dyn RelayLink, project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
    let host = link.host();
    let want = discovery_name(project, target);
    let sessions = link.list_sessions().map_err(|e| relay_failure(host, &want, e))?;
    let Some(session) = sessions.iter().find(|s| s.alive && s.name == want) else {
        let exited = sessions.iter().any(|s| s.name == want);
        return Err(match target {
            _ if exited => ConnectFailure::Message(no_live_session_error(&want, None)),
            SessionRef::Workspace(name) => ConnectFailure::Idle(IdleInfo {
                name: name.clone(),
                machine: host.to_string(),
                branch: None,
            }),
            SessionRef::Orchestrator => ConnectFailure::Awaiting(no_live_session_error(&want, None)),
            SessionRef::Review { .. } | SessionRef::WorkspaceContent { .. } => {
                ConnectFailure::Starting(starting_error(&want))
            }
        });
    };
    attach_stream(link, &session.short_id, &want)
}

/// Attach to the far session named `name` (one with no [`SessionRef`]: an
/// editor session a remote window asked the far daemon for) with a full
/// replay. A name not live yet is [`ConnectFailure::Starting`]; one that
/// exited, a message. Blocking.
pub fn attach_named_over(link: &dyn RelayLink, name: &str) -> Result<Connected, ConnectFailure> {
    let sessions = link.list_sessions().map_err(|e| relay_failure(link.host(), name, e))?;
    let Some(session) = sessions.iter().find(|s| s.alive && s.name == name) else {
        return Err(if sessions.iter().any(|s| s.name == name) {
            ConnectFailure::Message(no_live_session_error(name, None))
        } else {
            ConnectFailure::Starting(starting_error(name))
        });
    };
    attach_stream(link, &session.short_id, name)
}

/// Open far session `short_id` (named `want`) and attach with a replay.
fn attach_stream(link: &dyn RelayLink, short_id: &str, want: &str) -> Result<Connected, ConnectFailure> {
    let host = link.host();
    let stream = link
        .open_session(short_id)
        .map_err(|e| relay_failure(host, want, e))?;
    connect_stream(stream, host, want)
}

/// Connect over an open relay `stream` to `want` and attach with a replay.
fn connect_stream(stream: shelbi_client::RelayStream, host: &str, want: &str) -> Result<Connected, ConnectFailure> {
    let (conn, events) =
        Connection::connect(Box::new(stream), None, capability::ALL).map_err(|e| relay_failure(host, want, e))?;
    let size = match conn.info() {
        Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
        Err(_) => GridSize::new(80, 24),
    };
    conn.attach(None).map_err(|e| relay_failure(host, want, e))?;
    Ok(Connected { conn, events, size })
}

/// The far orchestrator's window title, asked over a short-lived,
/// never-attached stream (the session protocol's `info`), as the local probe
/// asks its socket. `None` when the channel is down, there is no live
/// orchestrator, it doesn't answer in time, or it has set no title.
pub fn orchestrator_title_over(link: &dyn RelayLink, project: &str) -> Option<String> {
    if !link.is_reachable() {
        return None;
    }
    let want = discovery_name(project, &SessionRef::Orchestrator);
    let sessions = link.list_sessions().ok()?;
    let session = sessions.iter().find(|s| s.alive && s.name == want)?;
    let stream = link.open_session(&session.short_id).ok()?;
    // `info` has no timeout of its own: answer on a thread, and give up on
    // it (the stream closes when the connection drops) after the deadline.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("shelbi-remote-title".into())
        .spawn(move || {
            let title = Connection::connect(Box::new(stream), None, &[capability::INFO])
                .ok()
                .and_then(|(conn, _events)| conn.info().ok())
                .and_then(|info| info.title);
            let _ = tx.send(title);
        })
        .ok()?;
    rx.recv_timeout(TITLE_DEADLINE).ok().flatten()
}

/// Type `text` into `project`'s orchestrator on `link`'s host (New task's
/// write-up hand-off), over a stream the relay opens to its session.
pub fn message_orchestrator_over(
    link: &dyn RelayLink,
    project: &str,
    text: &str,
) -> Result<(), crate::new_task::HandoffError> {
    use crate::new_task::HandoffError;
    let want = discovery_name(project, &SessionRef::Orchestrator);
    let sessions = link
        .list_sessions()
        .map_err(|e| HandoffError::Failed(format!("{}: {e}", link.host())))?;
    let Some(session) = sessions.iter().find(|s| s.alive && s.name == want) else {
        return Err(HandoffError::NotRunning);
    };
    let stream = link
        .open_session(&session.short_id)
        .map_err(|e| HandoffError::Failed(format!("{}: {e}", link.host())))?;
    let (conn, _events) = Connection::connect(Box::new(stream), None, capability::ALL)
        .map_err(|e| HandoffError::Failed(format!("{}: {e}", link.host())))?;
    crate::new_task::paste_then_return(&conn, text)
}

/// This machine's orchestrator's window title, from its session socket.
/// `None` when there is no live session, it doesn't answer within half a
/// second, or it has set no title.
pub fn local_orchestrator_title(project: &str) -> Option<String> {
    let root = shelbi_state::sessions_dir().ok()?;
    let sessions = shelbi_client::list(&root).ok()?;
    let session = shelbi_client::choose_session(&sessions, &discovery_name(project, &SessionRef::Orchestrator))?;
    let (conn, _events) = Connection::open_with_timeout(
        &session.sock,
        None,
        &[capability::INFO],
        Duration::from_millis(500),
    )
    .ok()?;
    conn.info().ok()?.title
}
