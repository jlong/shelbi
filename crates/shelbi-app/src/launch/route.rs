//! Launch routing: whether a client starts on the launch screen or opens a
//! project.
//!
//! Both clients ask [`decide`] once at startup. The decision itself is the
//! pure [`route`], over three inputs:
//!
//! - which client is starting ([`Client`]);
//! - whether the hub daemon is running ([`DaemonState`], read by
//!   [`daemon_state`] without starting one);
//! - the registered project the TUI was started in, if any (the desktop app
//!   has no meaningful working directory, so it never asks).
//!
//! | client  | daemon      | in a project | route                   |
//! |---------|-------------|--------------|-------------------------|
//! | desktop | not running | either       | launch screen           |
//! | desktop | running     | either       | the usual startup       |
//! | TUI     | either      | yes          | open that project       |
//! | TUI     | not running | no           | launch screen           |
//! | TUI     | running     | no           | the usual startup       |

use std::path::Path;

use shelbi_state::DaemonProbe;

/// The client that is starting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    Desktop,
    Tui,
}

/// Whether the hub daemon is up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DaemonState {
    /// A daemon answers the hub socket, or holds its lock while it starts.
    Running,
    NotRunning,
}

/// Where a client starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchRoute {
    /// Show the launch screen: the local projects and the recent remote
    /// sessions ([`super::LaunchModel`]).
    LaunchScreen,
    /// Open this registered project, as today.
    OpenProject(String),
    /// The client's startup from before the launch screen existed: the
    /// desktop's bootstrap, which connects to the running daemon and opens
    /// its project; the TUI's onboarding for a directory that isn't a
    /// project.
    Usual,
}

/// The launch decision (see the module's table). `project` is the
/// registered project the client was started in: the TUI's `--project` /
/// `SHELBI_PROJECT`, or its working directory's project ([`cwd_project`]).
/// The desktop ignores it.
pub fn route(client: Client, daemon: DaemonState, project: Option<&str>) -> LaunchRoute {
    match (client, daemon, project) {
        (Client::Desktop, DaemonState::NotRunning, _) => LaunchRoute::LaunchScreen,
        (Client::Desktop, DaemonState::Running, _) => LaunchRoute::Usual,
        (Client::Tui, _, Some(project)) => LaunchRoute::OpenProject(project.to_string()),
        (Client::Tui, DaemonState::NotRunning, None) => LaunchRoute::LaunchScreen,
        (Client::Tui, DaemonState::Running, None) => LaunchRoute::Usual,
    }
}

/// [`route`] over the live state: probes the daemon (never starting one)
/// and, for the TUI, resolves `explicit` or `cwd` to a registered project.
///
/// An error is the TUI's project resolution failing in a way the user has
/// to act on (an in-repo project this machine hasn't picked up yet); the
/// TUI reports it as it does today rather than showing the launch screen.
pub fn decide(
    client: Client,
    explicit: Option<&str>,
    cwd: &Path,
) -> shelbi_core::Result<LaunchRoute> {
    let project = match client {
        Client::Desktop => None,
        Client::Tui => match explicit {
            Some(name) => Some(name.to_string()).filter(|n| project_registered(n)),
            None => cwd_project(cwd)?,
        },
    };
    Ok(route(client, daemon_state(), project.as_deref()))
}

/// Whether a hub daemon is running, without starting one: something answers
/// the hub socket, or a daemon holds the single-instance lock while it
/// starts up (so a client launched a moment after another doesn't show the
/// launch screen over a daemon that is about to answer).
pub fn daemon_state() -> DaemonState {
    let answers = !matches!(shelbi_state::probe_daemon_hello(), DaemonProbe::NotRunning);
    if answers || shelbi_state::daemon_lock_held() {
        DaemonState::Running
    } else {
        DaemonState::NotRunning
    }
}

/// The registered project `cwd` is inside, if any.
pub fn cwd_project(cwd: &Path) -> shelbi_core::Result<Option<String>> {
    Ok(shelbi_state::resolve_project_for_cwd(cwd)?.filter(|n| project_registered(n)))
}

/// Whether `name` has a local registration on this machine: the flat global
/// `projects/<name>.yaml` or an in-repo project's `projects/<name>/local.yaml`.
/// A path-only check, so nothing is loaded or migrated.
pub fn project_registered(name: &str) -> bool {
    shelbi_state::has_project_registration(name).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::test_support::Sandbox;

    use Client::{Desktop, Tui};
    use DaemonState::{NotRunning, Running};

    #[test]
    fn every_client_daemon_and_directory_combination_routes() {
        let open = |p: &str| LaunchRoute::OpenProject(p.to_string());
        let cases = [
            (Desktop, NotRunning, None, LaunchRoute::LaunchScreen),
            (Desktop, NotRunning, Some("web"), LaunchRoute::LaunchScreen),
            (Desktop, Running, None, LaunchRoute::Usual),
            (Desktop, Running, Some("web"), LaunchRoute::Usual),
            (Tui, NotRunning, None, LaunchRoute::LaunchScreen),
            (Tui, NotRunning, Some("web"), open("web")),
            (Tui, Running, None, LaunchRoute::Usual),
            (Tui, Running, Some("web"), open("web")),
        ];
        for (client, daemon, project, want) in cases {
            assert_eq!(
                route(client, daemon, project),
                want,
                "{client:?} / daemon {daemon:?} / project {project:?}"
            );
        }
    }

    #[test]
    fn no_daemon_reads_as_not_running_and_starts_nothing() {
        let sandbox = Sandbox::new("route-idle");
        assert_eq!(daemon_state(), NotRunning);
        // Asking twice still finds nothing: the probe never spawned a daemon.
        assert_eq!(daemon_state(), NotRunning);
        assert!(!sandbox.home().join("hub.sock").exists());
    }

    #[test]
    fn a_listening_hub_socket_reads_as_running() {
        let sandbox = Sandbox::new("route-listening");
        let listener = std::os::unix::net::UnixListener::bind(sandbox.hub_sock()).unwrap();
        // Answer one probe with a line that isn't a hello (an older daemon):
        // still a daemon, so still running.
        let answer = std::thread::spawn(move || {
            use std::io::Write;
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.write_all(b"ok\n");
        });
        assert_eq!(daemon_state(), Running);
        answer.join().unwrap();
    }

    #[test]
    fn decide_reads_the_working_directory_only_for_the_tui() {
        let sandbox = Sandbox::new("route-decide");
        let repo = sandbox.register_project("web");
        let elsewhere = sandbox.home().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();

        assert_eq!(
            decide(Tui, None, &repo).unwrap(),
            LaunchRoute::OpenProject("web".into())
        );
        assert_eq!(decide(Tui, None, &elsewhere).unwrap(), LaunchRoute::LaunchScreen);
        assert_eq!(decide(Desktop, None, &repo).unwrap(), LaunchRoute::LaunchScreen);
        // An explicit project counts only when it is registered here.
        assert_eq!(
            decide(Tui, Some("web"), &elsewhere).unwrap(),
            LaunchRoute::OpenProject("web".into())
        );
        assert_eq!(
            decide(Tui, Some("gone"), &elsewhere).unwrap(),
            LaunchRoute::LaunchScreen
        );
    }

    #[test]
    fn registration_counts_both_config_layouts() {
        let sandbox = Sandbox::new("route-registered");
        sandbox.register_project("flat");
        let split = shelbi_state::project_dir("split").unwrap();
        std::fs::create_dir_all(&split).unwrap();
        std::fs::write(split.join("local.yaml"), "repo: /tmp/split\n").unwrap();
        std::fs::create_dir_all(shelbi_state::project_dir("bare").unwrap()).unwrap();

        assert!(project_registered("flat"));
        assert!(project_registered("split"));
        assert!(!project_registered("bare"), "a directory without local.yaml");
        assert!(!project_registered("missing"));
        assert!(!project_registered("../flat"), "names never escape the registry");
    }
}
