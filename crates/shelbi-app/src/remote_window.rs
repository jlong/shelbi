//! A native window for a project on another machine (remote-window P6,
//! the default since P10).
//!
//! Connect to Remote Session and a recent remote session open the full main
//! window for a host running exactly this build: sidebar, Orchestrator,
//! Issues, the task pane, Workspaces, Activity, Machines and Settings, read
//! through a [`RemoteSource`] and attached through a
//! [`RemoteSessionConnector`], both over the host's one shared relay link
//! ([`HostLink::shared`]). A host on any other version (a patch release
//! apart, or one that predates the native window) opens the far machine's
//! TUI in a terminal over `ssh -t`, with a line naming both versions.
//!
//! This module is the toolkit-free half a client builds that window from:
//!
//! - [`choose`]: native or terminal, and the terminal view's one-line notice
//!   when the versions differ ([`RemoteListing::window_decision`]).
//! - [`reconnect_check`]: what every reconnect of a native window checks
//!   first. The far side may have been upgraded while the window was away, so
//!   the decision is made again; a host that no longer matches refuses the
//!   reconnect ([`RemoteError::NoLongerNative`]) and the window closes its
//!   panes and offers Reopen (which decides again) rather than send commands
//!   to a daemon that may reject them.
//! - [`remote_handle`]: the window's [`ProjectHandle`] for a project on the
//!   host (its source, link and session connector).
//! - [`window_title`]: `jlong@devbox · Shelbi`.
//! - What a remote window can't do yet, each with the sentence a client shows
//!   instead of doing it: [`command_unavailable`] (the command registry
//!   carries it, [`crate::command::CommandModel::remote_host`], so menus draw
//!   the item disabled and the palette lists the reason). The project-state
//!   writes (the board's workspace filter and column overrides, Zen Mode, the
//!   error log and its read marks, a pane's reopen marker) go to the far
//!   daemon like issue mutations ([`crate::state_write`]).
//! - Open in Terminal View ([`crate::CommandKind::OpenTerminalView`]) keeps
//!   the terminal window reachable from a native one.
//!
//! What a remote window does on the far side instead of here (P8): Settings
//! saves its config through the far daemon (`SaveConfig`,
//! [`crate::settings::store`]); workspace and machine changes and machine
//! checks run from the far hub ([`crate::far`]); Edit opens the far file in
//! the far `$EDITOR`, in a terminal session there ([`crate::far::FarFile`]);
//! and a file dropped on a far terminal is uploaded there and its far path
//! pasted ([`crate::far::upload`]).
//!
//! The far daemon is never started implicitly: the window's link only
//! reconnects after [`RemoteConnector::connect`] says the daemon runs, and a
//! stopped one is offered to the user to start ([`RemoteConnector::start_daemon`]).
//!
//! A workspace the far project runs on its own other machine (a second hop)
//! isn't reachable over the link yet (P9): its pane shows a placeholder
//! naming that machine ([`two_hop_placeholder`]).

use std::sync::Arc;

use crate::command::CommandKind;
use crate::launch::link::HostLink;
use crate::launch::remote::{RemoteConnector, RemoteError, RemoteSession};
use crate::launch::{RemoteListing, WindowDecision};
use crate::session::remote::RemoteSessionConnector;
use crate::source::remote::{RelayLink, RemoteSource};
use crate::source::{ProjectHandle, ProjectLink};

/// Which window a remote session opens in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteOpen {
    /// The full main window, for a host running exactly this build. The
    /// listing it was decided on lists the host's projects (the window's
    /// project switcher) and their display names.
    Native(RemoteListing),
    /// The far TUI in a terminal (today's window), with the line saying why
    /// when the native one was wanted but the host can't have it.
    Terminal { notice: Option<String> },
}

/// Decide where `host`'s project opens: `listing` is read, and a host on
/// this exact version speaking this build's protocols opens natively; any
/// other opens the terminal view with the notice naming both versions. A
/// listing that can't be read opens the terminal view, which says what's
/// wrong on its own. Blocking (`listing` runs `ssh`).
pub fn choose(host: &str, listing: impl FnOnce() -> Result<RemoteListing, RemoteError>) -> RemoteOpen {
    match listing() {
        Ok(listing) => {
            let decision = listing.window_decision();
            if decision.is_native() {
                RemoteOpen::Native(listing)
            } else {
                RemoteOpen::Terminal {
                    notice: decision.notice(host),
                }
            }
        }
        Err(error) => {
            tracing::info!(host, %error, "no listing: the remote session opens in its terminal view");
            RemoteOpen::Terminal { notice: None }
        }
    }
}

/// What a native window checks before its link to `session`'s host comes
/// back up (a pane's reconnect, the window's own watch, Start Shelbi): the
/// host answers, still has the project and its daemon runs
/// ([`RemoteConnector::connect`], which never starts it), and then the
/// decision again, from a fresh listing, since the far Shelbi may have been
/// upgraded meanwhile. A host that no longer opens natively refuses with
/// [`RemoteError::NoLongerNative`] carrying the line to show. Blocking
/// (`ssh`).
pub fn reconnect_check(remote: &dyn RemoteConnector, session: &RemoteSession) -> Result<(), RemoteError> {
    remote.connect(session)?;
    let listing = remote.listing(&session.host)?;
    match listing.window_decision() {
        WindowDecision::Native => Ok(()),
        WindowDecision::Fallback { local, remote, .. } => Err(RemoteError::NoLongerNative {
            message: changed_notice(bare_host(&session.host), &local, &remote),
            host: session.host.clone(),
        }),
    }
}

/// The line a native window shows once its host stops matching: the far
/// version when it moved, and what Reopen does.
pub fn changed_notice(host: &str, local: &str, remote: &str) -> String {
    let here = if cfg!(target_os = "macos") { "this Mac" } else { "this computer" };
    if local == remote {
        format!("Shelbi on {host} changed since this window opened. Reopen to continue in its terminal view.")
    } else {
        format!("Shelbi on {host} is now {remote} and {here} has {local}. Reopen to continue in its terminal view.")
    }
}

/// What a pane for workspace `workspace` shows when the far project runs it
/// on `machine`, another of its own machines: the window can't reach a
/// second hop yet (P9).
pub fn two_hop_placeholder(workspace: &str, machine: &str) -> String {
    format!("{workspace} runs on {machine}. A remote window can't show a workspace on another of its machines yet.")
}

/// `host` without a `user@` the user typed.
pub fn bare_host(host: &str) -> &str {
    host.rsplit_once('@').map_or(host, |(_, h)| h)
}

/// A native remote window's title: who and where, and the project
/// (`jlong@devbox · Shelbi`).
pub fn window_title(user: &str, host: &str, project_label: &str) -> String {
    format!("{user}@{} · {project_label}", bare_host(host))
}

/// The handle a native window holds for `name` on `session`'s host: a
/// [`RemoteSource`] and a [`ProjectLink::remote`] over `link` (the host's
/// shared relay link), whose panes attach through the returned
/// [`RemoteSessionConnector`] (its [`RemoteSessionConnector::refusal`] says
/// when the far daemon was found stopped). A reconnect checks the host with
/// `remote` first, so it never starts the far daemon.
pub fn remote_handle(
    session: &RemoteSession,
    name: &str,
    link: Arc<HostLink>,
    remote: Arc<dyn RemoteConnector>,
) -> (ProjectHandle, Arc<RemoteSessionConnector>) {
    let project = RemoteSession {
        project: name.to_string(),
        ..session.clone()
    };
    let relay: Arc<dyn RelayLink> = link.clone();
    let sessions = Arc::new(RemoteSessionConnector::new(link, project, remote));
    let source = Arc::new(RemoteSource::new(name, relay.clone()));
    let handle = ProjectHandle::new(source, ProjectLink::remote(name, relay, sessions.clone()));
    (handle, sessions)
}

/// Why `kind` can't run in a window on a project on `host`, or `None` when it
/// can. These are the actions that still write this machine's files or
/// command this machine's daemon, and the ones a remote window never sends
/// (Quit Shelbi, denylisted by the relay). Issue mutations, project-state
/// writes (Zen Mode among them), reviews, Quit Project and Edit (an editor
/// session on the far machine) go to the far side.
pub fn command_unavailable(kind: &CommandKind, host: &str) -> Option<String> {
    let host = bare_host(host);
    let reason = match kind {
        CommandKind::AddProject => format!("Add a project on {host} from Shelbi there."),
        CommandKind::QuitShelbi => format!("Quit Shelbi on {host} from Shelbi there."),
        CommandKind::DetachOtherClients => {
            format!("Detaching {host}'s other clients isn't available in a remote window.")
        }
        CommandKind::NewWindow => "Another window on a remote project isn't available yet.".to_string(),
        CommandKind::OpenEditor { .. }
        | CommandKind::ShowView(_)
        | CommandKind::ToggleZen
        | CommandKind::FocusWorkspace { .. }
        | CommandKind::LoadReview { .. }
        | CommandKind::OpenErrorLog
        | CommandKind::OpenSettings
        | CommandKind::SendFeedback
        | CommandKind::OpenAbout
        | CommandKind::OpenAcknowledgements
        | CommandKind::SwitchProject { .. }
        | CommandKind::ConnectRemote
        | CommandKind::ShowLaunchScreen
        | CommandKind::OpenTerminalView
        | CommandKind::Detach
        | CommandKind::QuitProject
        | CommandKind::OpenIssue { .. }
        | CommandKind::OpenChangelogEntry { .. }
        | CommandKind::MoveIssue { .. }
        | CommandKind::StartIssue { .. }
        | CommandKind::AssignIssue { .. }
        | CommandKind::ApproveReview { .. }
        | CommandKind::RejectReview { .. }
        | CommandKind::EditIssue { .. }
        | CommandKind::AddIssue { .. }
        | CommandKind::NewIssue
        | CommandKind::PrioritizeIssue { .. }
        | CommandKind::SetIssueWorkflow { .. } => return None,
    };
    Some(reason)
}

/// Why a panel's open-in-app or reveal-folder does nothing in a remote
/// window: the worktree is on `host`, not this machine.
pub fn local_open_unavailable(host: &str) -> String {
    format!(
        "That folder is on {}: opening it from a remote window isn't available yet.",
        bare_host(host)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::{FakeRemote, NativeCapability};

    #[test]
    fn the_same_version_opens_natively_and_another_one_in_the_terminal_with_a_notice() {
        let fake = FakeRemote::new().with_host("devbox", "jlong", &[("shelbi", "~/shelbi")]);
        match choose("devbox", || fake.listing("devbox")) {
            RemoteOpen::Native(listing) => {
                assert_eq!(listing.user, "jlong");
                assert_eq!(listing.projects[0].name, "shelbi");
            }
            other => panic!("expected native, got {other:?}"),
        }

        let older = FakeRemote::new()
            .with_host("devbox", "jlong", &[("shelbi", "~/shelbi")])
            .with_far_version("devbox", "0.0.1", Some(NativeCapability::current()));
        let RemoteOpen::Terminal { notice: Some(notice) } = choose("devbox", || older.listing("devbox")) else {
            panic!("a different version opens the terminal view with a notice");
        };
        assert!(notice.starts_with("Shelbi on devbox is 0.0.1 and "), "{notice}");
        assert!(notice.contains(env!("CARGO_PKG_VERSION")), "{notice}");
        assert!(notice.ends_with("so it opens in its terminal view. Update both to get the full window."), "{notice}");

        let unreachable = FakeRemote::new();
        assert_eq!(
            choose("nowhere", || unreachable.listing("nowhere")),
            RemoteOpen::Terminal { notice: None }
        );
    }

    #[test]
    fn the_title_names_who_where_and_the_project() {
        assert_eq!(window_title("jlong", "devbox", "Shelbi"), "jlong@devbox · Shelbi");
        assert_eq!(window_title("jlong", "root@devbox", "web"), "jlong@devbox · web");
    }

    #[test]
    fn far_writes_and_local_only_actions_are_unavailable_with_a_reason() {
        let unavailable = [
            CommandKind::AddProject,
            CommandKind::QuitShelbi,
            CommandKind::DetachOtherClients,
            CommandKind::NewWindow,
        ];
        for kind in unavailable {
            let reason = command_unavailable(&kind, "jlong@devbox").unwrap_or_else(|| panic!("{kind:?}"));
            assert!(reason.ends_with('.'), "{reason}");
            assert!(!reason.contains("not implemented"), "{reason}");
        }
        for kind in [
            CommandKind::MoveIssue {
                id: "1".into(),
                to_status: "todo".into(),
            },
            CommandKind::QuitProject,
            // A far state write, through the far daemon (P7).
            CommandKind::ToggleZen,
            CommandKind::ShowView(crate::nav::View::Issues),
            CommandKind::OpenSettings,
            // An editor session on the far machine.
            CommandKind::OpenEditor {
                target: crate::exec::EditTarget::ZenMode,
            },
        ] {
            assert_eq!(command_unavailable(&kind, "devbox"), None, "{kind:?}");
        }
        assert!(command_unavailable(&CommandKind::QuitShelbi, "jlong@devbox")
            .unwrap()
            .contains("on devbox"));
    }
}
