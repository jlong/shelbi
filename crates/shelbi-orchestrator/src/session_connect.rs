//! Attaching a client view to one of a project's sessions, on whichever
//! machine it runs.
//!
//! [`shelbi_app::session::connect`] finds a session on *this* machine (the
//! sessions directory) and attaches to it. A workspace on another machine has
//! its sessions there, reached through the machine's relay
//! ([`crate::remote_session::attach`]). [`connect`] is the one entry a client
//! connector calls for any target: it resolves where a workspace-scoped
//! target (the agent, its editor, diff or user shell) lives and takes the
//! matching path, classifying the outcome into the same
//! [`ConnectFailure`]s, plus [`ConnectFailure::Unreachable`] for a machine
//! that can't be reached (the client keeps reconnecting rather than counting a
//! death).
//!
//! It lives here, not in `shelbi-app`, because resolving the machine and the
//! relay are the orchestrator's.
//!
//! [`open_transport`] is the same resolution for a caller that speaks the
//! session protocol itself: the relay's `session` service ([`relay_opener`]),
//! which bridges the stream to a remote window on another machine. For a
//! workspace on another of this project's machines that is a stream through
//! this machine's own relay to it (its relay cache and its `-R` forward stay
//! here, on this side), so a window with no route to that machine still shows
//! the workspace: the second hop.
//!
//! A remote workspace's **agent** is not registered under the client's
//! discovery name (`<project>/ws/<ws>`): dispatch spawns it at
//! [`crate::workspace::workspace_target`] (`shelbi-w-<ws>`, slot `agent`),
//! which the session backend names `w-<ws>/ws/agent`. The remote lookup uses
//! that name ([`remote_session_name`]) so it finds the live agent. A remote
//! workspace's user shell is spawned under its discovery name
//! (`<project>/ws/<ws>/shell`), so content targets keep it.

use std::sync::Arc;

use shelbi_app::session::{self as app_session, ConnectFailure, Connected, GridSize, IdleInfo, SessionRef};
use shelbi_client::{LocalTransport, SessionOpener, Transport};
use shelbi_core::{Host, Project, WorkspaceSpec};
use shelbi_proto::{SessionOpenError, SessionTarget};

use crate::remote_session::{self, RemoteAttachError};

/// Attach to `target` in `project` with a full replay, locally or through the
/// workspace machine's relay. Blocking: run it off the UI thread.
pub fn connect(project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
    if let Some((host, want)) = remote_lookup(project, target) {
        return connect_remote(project, target, &host, &want);
    }
    app_session::connect(project, target, local_idle_info)
}

/// A raw transport to `target` in `project`, on whichever machine it runs:
/// its socket on this machine, or a stream through this machine's relay to
/// the workspace's own machine. Nothing is sent on it (the caller does its own
/// hello and attach). Classified as [`connect`] classifies: a machine that
/// can't be reached is [`SessionOpenError::Unreachable`] (keep reconnecting),
/// one that answered with no live session is [`SessionOpenError::NoSession`].
/// Blocking.
pub fn open_transport(project: &str, target: &SessionRef) -> Result<Box<dyn Transport>, SessionOpenError> {
    if let Some((host, want)) = remote_lookup(project, target) {
        return crate::remote_session::open_stream(&host, &want)
            .map(|stream| Box::new(stream) as Box<dyn Transport>)
            .map_err(|e| match e {
                RemoteAttachError::Unreachable(message) => SessionOpenError::Unreachable { message },
                RemoteAttachError::NoSession { exited } => SessionOpenError::NoSession { exited },
                RemoteAttachError::Failed(message) => SessionOpenError::Failed { message },
            });
    }
    let failed = |message: String| SessionOpenError::Failed { message };
    let root = shelbi_state::sessions_dir().map_err(|e| failed(e.to_string()))?;
    let want = app_session::discovery_name(project, target);
    let sessions = shelbi_client::list(&root).map_err(|e| failed(e.to_string()))?;
    let Some(session) = shelbi_client::choose_session(&sessions, &want) else {
        return Err(SessionOpenError::NoSession {
            exited: sessions.iter().any(|s| s.meta.name == want),
        });
    };
    let sock = std::os::unix::net::UnixStream::connect(&session.sock)
        .map_err(|e| failed(format!("connect {}: {e}", session.sock.display())))?;
    Ok(Box::new(LocalTransport(sock)))
}

/// The relay `session` service's opener: [`open_transport`] for the target
/// the stream names. `shelbi relay` injects it.
pub fn relay_opener() -> SessionOpener {
    Arc::new(|project: &str, target: &SessionTarget| {
        let target = SessionRef::from_target(target).ok_or_else(|| SessionOpenError::Failed {
            message: "unknown session target".into(),
        })?;
        open_transport(project, &target)
    })
}

/// For a target on a remote workspace: the SSH host it lives on and the name
/// its session registers under there. `None` for a local (or unknown)
/// workspace, which takes the local path.
fn remote_lookup(project: &str, target: &SessionRef) -> Option<(Host, String)> {
    let p = shelbi_state::load_project(project).ok()?;
    let spec = p.workspace(target.workspace()?)?;
    let host = p.machine(&spec.machine)?.host();
    if !host.is_ssh() {
        return None;
    }
    let name = remote_session_name(&p, spec, target)?;
    Some((host, name))
}

/// The name `target`, on the remote workspace `spec`, is registered under on
/// its machine. The agent is the name dispatch spawns it at
/// ([`crate::workspace::workspace_target`], named by the session backend:
/// `w-<ws>/ws/agent`); a content session (the user shell) is spawned under
/// its discovery name. `None` when the workspace's machine is unknown.
pub fn remote_session_name(project: &Project, spec: &WorkspaceSpec, target: &SessionRef) -> Option<String> {
    match target {
        SessionRef::Workspace(_) => {
            let addr = crate::workspace::workspace_target(project, spec).ok()?;
            Some(crate::session_process_backend::session_name(&addr))
        }
        _ => Some(app_session::discovery_name(&project.name, target)),
    }
}

/// The idle placeholder's identity for a local workspace with no session.
fn local_idle_info(project: &str, name: &str) -> IdleInfo {
    match crate::workspace::resolve_idle_workspace(project, name) {
        Some(id) => IdleInfo {
            name: name.to_string(),
            machine: id.machine,
            branch: id.branch,
        },
        None => IdleInfo::named(name),
    }
}

fn connect_remote(project: &str, target: &SessionRef, host: &Host, want: &str) -> Result<Connected, ConnectFailure> {
    classify_remote(target, want, remote_session::attach(host, want), |name| {
        remote_idle_info(project, name)
    })
}

/// Map a relay attach outcome onto the client's connect classification.
/// `idle` resolves an idle agent's placeholder identity (it reads config, so
/// it is injected and only runs on that path). Tested without a remote.
fn classify_remote(
    target: &SessionRef,
    want: &str,
    outcome: Result<remote_session::RemoteAttach, RemoteAttachError>,
    idle: impl FnOnce(&str) -> IdleInfo,
) -> Result<Connected, ConnectFailure> {
    match outcome {
        Ok(a) => Ok(Connected {
            conn: a.conn,
            events: a.events,
            size: GridSize::new(a.cols, a.rows),
        }),
        Err(RemoteAttachError::Unreachable(m)) => Err(ConnectFailure::Unreachable(m)),
        // The machine answered: no session ever ran under this name. For the
        // agent that is an idle workspace (the placeholder, as locally); for a
        // content session it was just ensured and hasn't registered yet.
        Err(RemoteAttachError::NoSession { exited: false }) => match target {
            SessionRef::Workspace(name) => Err(ConnectFailure::Idle(idle(name))),
            _ => Err(ConnectFailure::Starting(app_session::starting_error(want))),
        },
        Err(RemoteAttachError::NoSession { exited: true }) => {
            Err(ConnectFailure::Message(app_session::no_live_session_error(want, None)))
        }
        Err(RemoteAttachError::Failed(m)) => Err(ConnectFailure::Message(m)),
    }
}

/// A remote idle workspace's identity: its machine (the branch isn't read over
/// SSH, as the TUI's placeholder leaves it out for a remote workspace).
fn remote_idle_info(project: &str, name: &str) -> IdleInfo {
    let machine = shelbi_state::load_project(project)
        .ok()
        .and_then(|p| p.workspace(name).map(|w| w.machine.clone()))
        .unwrap_or_default();
    IdleInfo {
        name: name.to_string(),
        machine,
        branch: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> Project {
        let mut p: Project = serde_yaml::from_str(
            "name: demo\nrepo: /tmp/demo\ndefault_branch: main\n\
             orchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n  - name: hub\n    kind: local\n    work_dir: /tmp/demo\n\
             \x20 - name: devbox\n    kind: ssh\n    host: devbox\n    work_dir: /tmp/demo\n\
             workspaces:\n  - { name: alpha, machine: hub, runner: claude }\n  - { name: delta, machine: devbox, runner: claude }\n",
        )
        .unwrap();
        // The loader fills the name in from the file (`#[serde(skip)]`).
        p.name = "demo".into();
        p
    }

    /// The remote agent is looked up under the name dispatch spawns it at,
    /// not the local discovery name (which never matched a remote agent).
    #[test]
    fn a_remote_agent_is_looked_up_under_its_dispatch_name() {
        let p = project();
        let delta = p.workspace("delta").unwrap();
        let spawned = crate::session_process_backend::session_name(
            &crate::workspace::workspace_target(&p, delta).unwrap(),
        );
        assert_eq!(spawned, "w-delta/ws/agent");
        assert_eq!(
            remote_session_name(&p, delta, &SessionRef::Workspace("delta".into())).as_deref(),
            Some("w-delta/ws/agent")
        );
        // The user shell is spawned under its discovery name, so it keeps it.
        assert_eq!(
            remote_session_name(&p, delta, &SessionRef::workspace_shell("delta")).as_deref(),
            Some("demo/ws/delta/shell")
        );
    }

    fn classify(target: &SessionRef, outcome: RemoteAttachError) -> ConnectFailure {
        let idle = |name: &str| IdleInfo {
            name: name.to_string(),
            machine: "gpu".into(),
            branch: None,
        };
        match classify_remote(target, "p/ws/a", Err(outcome), idle) {
            Err(f) => f,
            Ok(_) => unreachable!(),
        }
    }

    #[test]
    fn an_unreachable_machine_is_unreachable_not_dead() {
        let f = classify(
            &SessionRef::Workspace("a".into()),
            RemoteAttachError::Unreachable("`gpu` is unreachable".into()),
        );
        assert!(matches!(f, ConnectFailure::Unreachable(m) if m == "`gpu` is unreachable"));
    }

    #[test]
    fn a_never_run_agent_is_idle_and_a_never_run_content_session_is_starting() {
        let agent = classify(
            &SessionRef::Workspace("a".into()),
            RemoteAttachError::NoSession { exited: false },
        );
        assert!(
            matches!(agent, ConnectFailure::Idle(info) if info.name == "a" && info.machine == "gpu")
        );
        let shell = classify(
            &SessionRef::workspace_shell("a"),
            RemoteAttachError::NoSession { exited: false },
        );
        assert!(matches!(shell, ConnectFailure::Starting(_)));
    }

    #[test]
    fn an_exited_session_is_a_message() {
        let f = classify(
            &SessionRef::Workspace("a".into()),
            RemoteAttachError::NoSession { exited: true },
        );
        assert!(matches!(f, ConnectFailure::Message(m) if m == "no live session `p/ws/a`"));
        let f = classify(&SessionRef::Workspace("a".into()), RemoteAttachError::Failed("boom".into()));
        assert!(matches!(f, ConnectFailure::Message(m) if m == "boom"));
    }
}
