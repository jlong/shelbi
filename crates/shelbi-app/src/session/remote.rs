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
//! Sessions on the far project's own remote machines (a second hop) are not
//! on the far host's session list; until the relay can reach them (P9) such a
//! workspace reads as idle on the host.

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
        let opened = self.link.connect(&|| self.remote.connect(&self.session));
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
        attach_over(&*link, project, target)
    }
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
    let stream = link
        .open_session(&session.short_id)
        .map_err(|e| relay_failure(host, &want, e))?;
    let (conn, events) =
        Connection::connect(Box::new(stream), None, capability::ALL).map_err(|e| relay_failure(host, &want, e))?;
    let size = match conn.info() {
        Ok(info) => GridSize::new(info.cols.max(1), info.rows.max(1)),
        Err(_) => GridSize::new(80, 24),
    };
    conn.attach(None).map_err(|e| relay_failure(host, &want, e))?;
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
