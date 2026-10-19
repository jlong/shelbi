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

use shelbi_app::session::{self as app_session, ConnectFailure, Connected, GridSize, IdleInfo, SessionRef};
use shelbi_core::Host;

use crate::remote_session::{self, RemoteAttachError};

/// Attach to `target` in `project` with a full replay, locally or through the
/// workspace machine's relay. Blocking: run it off the UI thread.
pub fn connect(project: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
    if let Some(host) = target.workspace().and_then(|ws| remote_host(project, ws)) {
        return connect_remote(project, target, &host);
    }
    app_session::connect(project, target, local_idle_info)
}

/// The SSH host a workspace lives on, or `None` for a local (or unknown)
/// workspace, which takes the local path.
fn remote_host(project: &str, workspace: &str) -> Option<Host> {
    let p = shelbi_state::load_project(project).ok()?;
    let spec = p.workspace(workspace)?;
    let host = p.machine(&spec.machine)?.host();
    host.is_ssh().then_some(host)
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

fn connect_remote(project: &str, target: &SessionRef, host: &Host) -> Result<Connected, ConnectFailure> {
    let want = app_session::discovery_name(project, target);
    classify_remote(target, &want, remote_session::attach(host, &want), |name| {
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
