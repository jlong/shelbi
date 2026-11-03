//! A native window for a project on another machine (remote-window P6),
//! behind a developer setting.
//!
//! Connect to Remote Session and a recent remote session open, today, the
//! far machine's TUI in a terminal over `ssh -t`. With the dev flag on
//! ([`native_remote_enabled`]: `desktop.native_remote: true` in the viewer's
//! `config.yaml`, or `SHELBI_NATIVE_REMOTE=1`), a host running exactly this
//! build opens the full main window instead: sidebar, Orchestrator, Issues,
//! the task pane, Workspaces, Activity, Machines and Settings, read through a
//! [`RemoteSource`] and attached through a [`RemoteSessionConnector`], both
//! over the host's one shared relay link ([`HostLink::shared`]).
//!
//! This module is the toolkit-free half a client builds that window from:
//!
//! - [`choose`]: native or terminal, and the terminal view's one-line notice
//!   when the versions differ ([`RemoteListing::window_decision`]). With the
//!   flag off it asks the host nothing, so the terminal window opens exactly
//!   as it did.
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

use std::sync::Arc;

use crate::command::CommandKind;
use crate::launch::link::HostLink;
use crate::launch::remote::{RemoteConnector, RemoteError, RemoteSession};
use crate::launch::RemoteListing;
use crate::session::remote::RemoteSessionConnector;
use crate::source::remote::{RelayLink, RemoteSource};
use crate::source::{ProjectHandle, ProjectLink};

/// The environment variable that turns the native remote window on for one
/// launch (`SHELBI_NATIVE_REMOTE=1`).
pub const NATIVE_REMOTE_ENV: &str = "SHELBI_NATIVE_REMOTE";

/// Whether the native remote window is on, from the viewer's setting and the
/// environment override: the variable set to `1`, `true` or `yes` turns it
/// on, `0`, `false` or `no` off; unset (or anything else), the setting
/// decides.
pub fn native_remote_from(setting: Option<bool>, env: Option<&str>) -> bool {
    match env.map(|v| v.trim().to_ascii_lowercase()) {
        Some(v) if matches!(v.as_str(), "1" | "true" | "yes") => true,
        Some(v) if matches!(v.as_str(), "0" | "false" | "no") => false,
        _ => setting.unwrap_or(false),
    }
}

/// Whether the native remote window is on for this process: the viewer's
/// `desktop.native_remote` and [`NATIVE_REMOTE_ENV`]. Reads `config.yaml`;
/// call it off the UI thread.
pub fn native_remote_enabled() -> bool {
    let setting = shelbi_state::load_user_config()
        .ok()
        .and_then(|cfg| cfg.desktop.native_remote);
    native_remote_from(setting, std::env::var(NATIVE_REMOTE_ENV).ok().as_deref())
}

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

/// Decide where `host`'s project opens: with the flag off, the terminal view
/// with no notice, and `listing` is never asked (the host is probed exactly
/// as before). With it on, `listing` is read: a host on this exact version
/// speaking this build's protocols opens natively; any other opens the
/// terminal view with the notice naming both versions. A listing that can't
/// be read opens the terminal view, which says what's wrong on its own.
/// Blocking when the flag is on (`listing` runs `ssh`).
pub fn choose(
    flag: bool,
    host: &str,
    listing: impl FnOnce() -> Result<RemoteListing, RemoteError>,
) -> RemoteOpen {
    if !flag {
        return RemoteOpen::Terminal { notice: None };
    }
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
    fn the_flag_is_the_setting_unless_the_environment_says() {
        assert!(!native_remote_from(None, None));
        assert!(native_remote_from(Some(true), None));
        assert!(!native_remote_from(Some(false), None));
        assert!(native_remote_from(None, Some("1")));
        assert!(native_remote_from(Some(false), Some(" TRUE ")));
        assert!(!native_remote_from(Some(true), Some("0")));
        assert!(native_remote_from(Some(true), Some("maybe")), "an unknown value leaves the setting");
    }

    #[test]
    fn with_the_flag_off_the_host_is_never_asked() {
        let asked = std::cell::Cell::new(false);
        let open = choose(false, "devbox", || {
            asked.set(true);
            Err(RemoteError::Failed("never".into()))
        });
        assert_eq!(open, RemoteOpen::Terminal { notice: None });
        assert!(!asked.get(), "the flag off opens the terminal view exactly as before");
    }

    #[test]
    fn the_same_version_opens_natively_and_another_one_in_the_terminal_with_a_notice() {
        let fake = FakeRemote::new().with_host("devbox", "jlong", &[("shelbi", "~/shelbi")]);
        match choose(true, "devbox", || fake.listing("devbox")) {
            RemoteOpen::Native(listing) => {
                assert_eq!(listing.user, "jlong");
                assert_eq!(listing.projects[0].name, "shelbi");
            }
            other => panic!("expected native, got {other:?}"),
        }

        let older = FakeRemote::new()
            .with_host("devbox", "jlong", &[("shelbi", "~/shelbi")])
            .with_far_version("devbox", "0.0.1", Some(NativeCapability::current()));
        let RemoteOpen::Terminal { notice: Some(notice) } = choose(true, "devbox", || older.listing("devbox")) else {
            panic!("a different version opens the terminal view with a notice");
        };
        assert!(notice.starts_with("Shelbi on devbox is 0.0.1 and "), "{notice}");
        assert!(notice.contains(env!("CARGO_PKG_VERSION")), "{notice}");
        assert!(notice.ends_with("so it opens in its terminal view. Update both to get the full window."), "{notice}");

        let unreachable = FakeRemote::new();
        assert_eq!(
            choose(true, "nowhere", || unreachable.listing("nowhere")),
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
