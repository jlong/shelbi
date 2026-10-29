//! The launch screen: what a client shows when it starts with no daemon
//! running (the desktop app always; the TUI outside a project), and what its
//! buttons do.
//!
//! - [`route`] decides between the launch screen and opening a project
//!   ([`route::decide`]), and reads whether the daemon is running without
//!   starting one.
//! - [`LaunchModel`] is the screen: the projects on this machine, most
//!   recently opened first, and the recent remote sessions, or the
//!   "No projects yet" state ([`LaunchState`]).
//! - [`recent`] keeps the recent remote sessions under the Shelbi home.
//! - [`remote`] is the Connect to Remote Session seam ([`RemoteConnector`]),
//!   its session type, and the dialog's words.
//! - [`connect`] is that dialog's two steps ([`ConnectFlow`]).
//! - [`ssh`] is the real connector ([`ssh::SshRemote`]): the host's own
//!   `shelbi remote list --json` over the user's `ssh`, and the terminal
//!   command that opens a remote session ([`ssh::terminal_command`]).
//! - [`terminal`] runs that terminal and reconnects it when `ssh` drops.
//! - [`perform`] carries out a [`LaunchAction`]: opening a local project
//!   starts the daemon first, through the same on-demand start the clients
//!   use today ([`DaemonStarter`]); Add project hands over to the add-project
//!   wizard; connecting remembers the session.

pub mod connect;
pub mod recent;
pub mod remote;
pub mod route;
pub mod ssh;
pub mod terminal;

use std::path::Path;

use shelbi_state::ProjectSummary;

pub use connect::ConnectFlow;
pub use recent::{RecentRemote, RecentRemotes, MAX_RECENT_REMOTES, RECENT_REMOTES_FILE};
pub use remote::{validate_host, RemoteConnector, RemoteError, RemoteSession};
pub use route::{decide, route, Client, DaemonState, LaunchRoute};
pub use ssh::{RemoteListing, SshRemote};

#[cfg(any(test, feature = "test-support"))]
pub use remote::FakeRemote;

/// The left panel's title.
pub const WELCOME_TITLE: &str = "Welcome to Shelbi";
/// The line under the title.
pub const WELCOME_INTRO: &str =
    "Open a project on this Mac or connect to Shelbi on another machine.";
/// The left panel's buttons.
pub const ADD_PROJECT_LABEL: &str = "Add project";
pub const CONNECT_REMOTE_LABEL: &str = "Connect to remote";
/// The right panel's section headings.
pub const PROJECTS_HEADING: &str = "Projects on this Mac";
pub const REMOTES_HEADING: &str = "Recent remote sessions";
/// Each recent remote session's button.
pub const RECONNECT_LABEL: &str = "Connect";
/// The right panel with nothing to list.
pub const EMPTY_TITLE: &str = "No projects yet";
pub const EMPTY_BODY: &str = "Add your first project, or connect to a remote Shelbi session.";

/// A project registered on this machine: a row under
/// [`PROJECTS_HEADING`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalProject {
    /// The project's id, what [`LaunchAction::OpenProject`] opens.
    pub slug: String,
    /// The row's title: the display name, else the id.
    pub label: String,
    /// The project's folder as registered.
    pub path: String,
    /// The row's second line: the folder, with the home folder as `~`.
    pub display_path: String,
}

/// Which right panel the launch screen shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchState {
    /// The project list and the recent remote sessions (a client leaves out
    /// a section with no rows).
    Projects,
    /// Nothing to list: no project here and no recent remote session
    /// ([`EMPTY_TITLE`], [`EMPTY_BODY`]).
    NoProjects,
}

/// The launch screen's contents.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchModel {
    /// Most recently opened first; never-opened projects after, by id.
    pub projects: Vec<LocalProject>,
    /// Newest first.
    pub remotes: Vec<RecentRemote>,
}

impl LaunchModel {
    /// The model over the registered projects (in [`shelbi_state::list_projects`]
    /// order) and the recent remote sessions. `home` is the folder shown as `~`.
    pub fn build(
        projects: Vec<ProjectSummary>,
        remotes: Vec<RecentRemote>,
        home: Option<&Path>,
    ) -> LaunchModel {
        let projects = projects
            .into_iter()
            .map(|p| LocalProject {
                label: p.display_label().to_string(),
                display_path: display_path(&p.repo_path, home),
                path: p.repo_path,
                slug: p.name,
            })
            .collect();
        LaunchModel { projects, remotes }
    }

    /// The model read from disk: this machine's registered projects and the
    /// recent remote sessions in `recents`.
    pub fn load(recents: &RecentRemotes) -> shelbi_core::Result<LaunchModel> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        Ok(LaunchModel::build(
            shelbi_state::list_projects()?,
            recents.load(),
            home.as_deref(),
        ))
    }

    /// The rows the keyboard moves through: the projects, then the recent
    /// remote sessions.
    pub fn row_count(&self) -> usize {
        self.projects.len() + self.remotes.len()
    }

    /// What row `index` does when picked (a click, Return on it): open the
    /// project, or reconnect to the remote session.
    pub fn action_at(&self, index: usize) -> Option<LaunchAction> {
        match self.projects.get(index) {
            Some(p) => Some(LaunchAction::OpenProject(p.slug.clone())),
            None => self
                .remotes
                .get(index - self.projects.len())
                .map(|r| LaunchAction::Reconnect(r.session.clone())),
        }
    }

    pub fn state(&self) -> LaunchState {
        if self.projects.is_empty() && self.remotes.is_empty() {
            LaunchState::NoProjects
        } else {
            LaunchState::Projects
        }
    }
}

/// `path` with a leading `home` shown as `~` (`~/Workspaces/shelbi`).
pub fn display_path(path: &str, home: Option<&Path>) -> String {
    let Some(home) = home.and_then(Path::to_str).map(|h| h.trim_end_matches('/')) else {
        return path.to_string();
    };
    if home.is_empty() {
        return path.to_string();
    }
    match path.strip_prefix(home) {
        Some("") => "~".to_string(),
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// Starting the hub daemon, the step before opening a project from the
/// launch screen.
pub trait DaemonStarter: Send + Sync {
    /// Ensure a daemon is running, starting one if none is. Blocks until it
    /// answers or the start fails, with the reason.
    fn ensure_running(&self) -> Result<(), String>;
}

/// The on-demand start every client uses today
/// ([`shelbi_state::ensure_daemon_running`]): `<exe> daemon` through the
/// process's installed launcher. The TUI runs itself; the desktop app's
/// bootstrap installs a launcher naming the CLI it found
/// ([`shelbi_state::set_daemon_launcher`]) before it gets here.
#[derive(Debug, Clone, Copy, Default)]
pub struct SharedDaemon;

impl DaemonStarter for SharedDaemon {
    fn ensure_running(&self) -> Result<(), String> {
        shelbi_state::ensure_daemon_running().map_err(|e| e.to_string())
    }
}

/// A [`DaemonStarter`] for tests: counts its starts and never runs anything.
#[cfg(any(test, feature = "test-support"))]
#[derive(Debug, Default)]
pub struct FakeDaemon {
    starts: std::sync::atomic::AtomicUsize,
    failure: Option<String>,
}

#[cfg(any(test, feature = "test-support"))]
impl FakeDaemon {
    pub fn new() -> FakeDaemon {
        FakeDaemon::default()
    }

    /// Every start fails with `reason`.
    pub fn failing(reason: &str) -> FakeDaemon {
        FakeDaemon {
            starts: Default::default(),
            failure: Some(reason.to_string()),
        }
    }

    pub fn starts(&self) -> usize {
        self.starts.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[cfg(any(test, feature = "test-support"))]
impl DaemonStarter for FakeDaemon {
    fn ensure_running(&self) -> Result<(), String> {
        self.starts.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.failure.clone().map_or(Ok(()), Err)
    }
}

/// What the user picked on the launch screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchAction {
    /// A row under [`PROJECTS_HEADING`].
    OpenProject(String),
    /// [`ADD_PROJECT_LABEL`].
    AddProject,
    /// [`CONNECT_REMOTE_LABEL`].
    ConnectToRemote,
    /// A recent remote session's [`RECONNECT_LABEL`].
    Reconnect(RemoteSession),
}

/// What the client does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchOutcome {
    /// The daemon is up: open this project as a client opens one today (the
    /// TUI's dashboard, the desktop's project window).
    OpenProject(String),
    /// Run the add-project wizard, the flow [`crate::Effect::AddProject`]
    /// opens (it opens the project it creates).
    AddProject,
    /// Show the Connect to Remote Session dialog.
    ConnectDialog,
    /// Connected to this remote session (now first in the recent list).
    Connected(RemoteSession),
}

/// Why a launch action failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LaunchError {
    /// The project isn't registered here any more.
    #[error("There's no project named {0} on this machine.")]
    UnknownProject(String),
    /// The daemon wouldn't start.
    #[error("Shelbi couldn't start: {0}")]
    Daemon(String),
    #[error(transparent)]
    Remote(#[from] RemoteError),
}

/// Carry out `action`. Opening a project and connecting block (a daemon
/// start, an `ssh`), so a client calls this off its UI thread.
pub fn perform(
    action: LaunchAction,
    daemon: &dyn DaemonStarter,
    remote: &dyn RemoteConnector,
    recents: &RecentRemotes,
) -> Result<LaunchOutcome, LaunchError> {
    match action {
        LaunchAction::OpenProject(slug) => open_project(slug, daemon),
        LaunchAction::AddProject => Ok(LaunchOutcome::AddProject),
        LaunchAction::ConnectToRemote => Ok(LaunchOutcome::ConnectDialog),
        LaunchAction::Reconnect(session) => {
            connect_remote(session, remote, recents).map(LaunchOutcome::Connected)
        }
    }
}

/// Start the daemon, then hand back `slug` to open. The project moves to the
/// top of the list for next time.
pub fn open_project(slug: String, daemon: &dyn DaemonStarter) -> Result<LaunchOutcome, LaunchError> {
    if !route::project_registered(&slug) {
        return Err(LaunchError::UnknownProject(slug));
    }
    daemon.ensure_running().map_err(LaunchError::Daemon)?;
    if let Err(error) = shelbi_state::touch_project_launched(&slug) {
        tracing::warn!(project = %slug, %error, "couldn't record the project as opened");
    }
    Ok(LaunchOutcome::OpenProject(slug))
}

/// Connect to `session` and remember it (the dialog's Connect and a recent
/// session's both land here). A session that connected but couldn't be
/// remembered still counts as connected.
pub fn connect_remote(
    session: RemoteSession,
    remote: &dyn RemoteConnector,
    recents: &RecentRemotes,
) -> Result<RemoteSession, LaunchError> {
    remote.connect(&session)?;
    if let Err(error) = recents.remember(session.clone(), chrono::Utc::now()) {
        tracing::warn!(host = %session.host, %error, "couldn't save the recent remote session");
    }
    Ok(session)
}

/// A throwaway Shelbi home for a test, with the hub socket inside it, so no
/// test reads the real `~/.shelbi` or reaches the real hub.
#[cfg(test)]
pub(crate) mod test_support {
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::MutexGuard;

    const VARS: [&str; 3] = ["SHELBI_HOME", "SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK"];

    pub(crate) struct Sandbox {
        home: PathBuf,
        saved: Vec<(&'static str, Option<OsString>)>,
        _lock: MutexGuard<'static, ()>,
    }

    impl Sandbox {
        pub(crate) fn new(tag: &str) -> Sandbox {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let lock = crate::test_env::ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // Short: the hub socket lives in here, and a Unix socket path is
            // capped near 100 bytes on macOS.
            let n = NEXT.fetch_add(1, Ordering::SeqCst);
            let home = std::env::temp_dir().join(format!("sl-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&home);
            std::fs::create_dir_all(&home).unwrap();
            let home = home.canonicalize().unwrap();
            std::fs::write(home.join("tag"), tag).unwrap();
            let saved = VARS.iter().map(|v| (*v, std::env::var_os(v))).collect();
            std::env::set_var("SHELBI_HOME", &home);
            std::env::set_var("SHELBI_HUB_SOCK", home.join("hub.sock"));
            std::env::set_var("SHELBI_CONTROL_SOCK", home.join("control.sock"));
            Sandbox {
                home,
                saved,
                _lock: lock,
            }
        }

        pub(crate) fn home(&self) -> &Path {
            &self.home
        }

        pub(crate) fn hub_sock(&self) -> PathBuf {
            self.home.join("hub.sock")
        }

        /// Register a global project `name` whose folder is a fresh
        /// directory in the sandbox; returns that folder.
        pub(crate) fn register_project(&self, name: &str) -> PathBuf {
            let repo = self.home.join("repos").join(name);
            std::fs::create_dir_all(&repo).unwrap();
            let projects = shelbi_state::projects_dir().unwrap();
            std::fs::create_dir_all(&projects).unwrap();
            let repo_s = repo.display();
            let yaml = format!(
                "name: {name}\n\
                 repo: {repo_s}\n\
                 machines:\n\
                 \x20\x20- {{ name: hub, kind: local, work_dir: {repo_s} }}\n\
                 orchestrator: {{ runner: claude }}\n\
                 agent_runners:\n\
                 \x20\x20claude: {{ command: claude, flags: [] }}\n\
                 workspaces: []\n"
            );
            std::fs::write(projects.join(format!("{name}.yaml")), yaml).unwrap();
            repo
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            for (var, value) in &self.saved {
                match value {
                    Some(v) => std::env::set_var(var, v),
                    None => std::env::remove_var(var),
                }
            }
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::Sandbox;
    use super::*;
    use chrono::{TimeZone, Utc};

    fn summary(name: &str, label: Option<&str>, repo: &str) -> ProjectSummary {
        ProjectSummary {
            name: name.into(),
            display_name: label.map(str::to_string),
            repo_path: repo.into(),
            machine_count: 1,
            workspace_count: 0,
            last_launched: None,
        }
    }

    fn remote_session(project: &str) -> RemoteSession {
        RemoteSession {
            host: "devbox".into(),
            user: "jlong".into(),
            project: project.into(),
            label: None,
            path: format!("~/Workspaces/{project}"),
        }
    }

    #[test]
    fn rows_carry_the_label_and_the_folder_under_home() {
        let home = Path::new("/Users/jlong");
        let model = LaunchModel::build(
            vec![
                summary("shelbi", Some("Shelbi"), "/Users/jlong/Workspaces/shelbi"),
                summary("ops", None, "/srv/ops"),
            ],
            Vec::new(),
            Some(home),
        );
        assert_eq!(
            model.projects[0],
            LocalProject {
                slug: "shelbi".into(),
                label: "Shelbi".into(),
                path: "/Users/jlong/Workspaces/shelbi".into(),
                display_path: "~/Workspaces/shelbi".into(),
            }
        );
        assert_eq!(model.projects[1].label, "ops");
        assert_eq!(model.projects[1].display_path, "/srv/ops");
    }

    #[test]
    fn rows_run_projects_then_remote_sessions() {
        let model = LaunchModel::build(
            vec![summary("a", None, "/a"), summary("b", None, "/b")],
            vec![RecentRemote {
                session: remote_session("shelbi"),
                last_connected: Utc.with_ymd_and_hms(2026, 10, 28, 9, 0, 0).unwrap(),
            }],
            None,
        );
        assert_eq!(model.row_count(), 3);
        assert_eq!(model.action_at(1), Some(LaunchAction::OpenProject("b".into())));
        assert_eq!(model.action_at(2), Some(LaunchAction::Reconnect(remote_session("shelbi"))));
        assert_eq!(model.action_at(3), None);
        assert_eq!(LaunchModel::default().action_at(0), None);
    }

    #[test]
    fn display_path_shortens_only_the_home_folder_itself() {
        let home = Some(Path::new("/Users/jlong/"));
        assert_eq!(display_path("/Users/jlong/code", home), "~/code");
        assert_eq!(display_path("/Users/jlong", home), "~");
        assert_eq!(display_path("/Users/jlongfellow/x", home), "/Users/jlongfellow/x");
        assert_eq!(display_path("/Users/jlong/code", None), "/Users/jlong/code");
        assert_eq!(display_path("/x", Some(Path::new(""))), "/x");
    }

    #[test]
    fn nothing_to_list_is_the_no_projects_state() {
        let empty = LaunchModel::default();
        assert_eq!(empty.state(), LaunchState::NoProjects);

        let local = LaunchModel::build(vec![summary("a", None, "/a")], Vec::new(), None);
        assert_eq!(local.state(), LaunchState::Projects);

        let remote_only = LaunchModel::build(
            Vec::new(),
            vec![RecentRemote {
                session: remote_session("shelbi"),
                last_connected: Utc.with_ymd_and_hms(2026, 10, 28, 9, 0, 0).unwrap(),
            }],
            None,
        );
        assert_eq!(remote_only.state(), LaunchState::Projects);
    }

    #[test]
    fn the_loaded_model_lists_projects_most_recently_opened_first() {
        let sandbox = Sandbox::new("launch-load");
        let recents = RecentRemotes::live();
        assert_eq!(LaunchModel::load(&recents).unwrap().state(), LaunchState::NoProjects);

        sandbox.register_project("alpha");
        sandbox.register_project("beta");
        sandbox.register_project("gamma");
        shelbi_state::touch_project_launched("gamma").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        shelbi_state::touch_project_launched("alpha").unwrap();
        recents.remember(remote_session("shelbi"), Utc::now()).unwrap();

        let model = LaunchModel::load(&recents).unwrap();
        let slugs: Vec<&str> = model.projects.iter().map(|p| p.slug.as_str()).collect();
        assert_eq!(slugs, ["alpha", "gamma", "beta"]);
        assert_eq!(model.remotes.len(), 1);
        assert_eq!(model.remotes[0].session.project, "shelbi");
    }

    #[test]
    fn opening_a_project_starts_the_daemon_then_opens_it() {
        let sandbox = Sandbox::new("launch-open");
        sandbox.register_project("alpha");
        sandbox.register_project("beta");
        shelbi_state::touch_project_launched("beta").unwrap();
        let daemon = FakeDaemon::new();
        let remote = FakeRemote::new();
        let recents = RecentRemotes::live();

        let outcome =
            perform(LaunchAction::OpenProject("alpha".into()), &daemon, &remote, &recents);
        assert_eq!(outcome, Ok(LaunchOutcome::OpenProject("alpha".into())));
        assert_eq!(daemon.starts(), 1);
        // Opened last, so listed first next time.
        let model = LaunchModel::load(&recents).unwrap();
        assert_eq!(model.projects[0].slug, "alpha");
    }

    #[test]
    fn a_failed_daemon_start_or_a_missing_project_opens_nothing() {
        let sandbox = Sandbox::new("launch-open-fail");
        sandbox.register_project("alpha");
        let remote = FakeRemote::new();
        let recents = RecentRemotes::live();

        let broken = FakeDaemon::failing("exited during startup");
        assert_eq!(
            perform(LaunchAction::OpenProject("alpha".into()), &broken, &remote, &recents),
            Err(LaunchError::Daemon("exited during startup".into()))
        );

        let daemon = FakeDaemon::new();
        assert_eq!(
            perform(LaunchAction::OpenProject("gone".into()), &daemon, &remote, &recents),
            Err(LaunchError::UnknownProject("gone".into()))
        );
        assert_eq!(daemon.starts(), 0, "no daemon for a project that isn't there");
    }

    #[test]
    fn add_project_and_connect_hand_over_without_starting_anything() {
        let _sandbox = Sandbox::new("launch-handover");
        let daemon = FakeDaemon::new();
        let remote = FakeRemote::new();
        let recents = RecentRemotes::live();
        assert_eq!(
            perform(LaunchAction::AddProject, &daemon, &remote, &recents),
            Ok(LaunchOutcome::AddProject)
        );
        assert_eq!(
            perform(LaunchAction::ConnectToRemote, &daemon, &remote, &recents),
            Ok(LaunchOutcome::ConnectDialog)
        );
        assert_eq!(daemon.starts(), 0);
        assert!(remote.searched().is_empty());
    }

    #[test]
    fn reconnecting_connects_and_moves_the_session_to_the_top() {
        let _sandbox = Sandbox::new("launch-reconnect");
        let daemon = FakeDaemon::new();
        let remote = FakeRemote::new();
        let recents = RecentRemotes::live();
        recents
            .remember(remote_session("shelbi"), Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap())
            .unwrap();
        recents
            .remember(remote_session("web"), Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap())
            .unwrap();

        let outcome = perform(
            LaunchAction::Reconnect(remote_session("shelbi")),
            &daemon,
            &remote,
            &recents,
        );
        assert_eq!(outcome, Ok(LaunchOutcome::Connected(remote_session("shelbi"))));
        assert_eq!(remote.connected(), vec![remote_session("shelbi")]);
        let order: Vec<String> =
            recents.load().into_iter().map(|r| r.session.project).collect();
        assert_eq!(order, ["shelbi", "web"]);
    }

    #[test]
    fn a_refused_connection_is_not_remembered() {
        let _sandbox = Sandbox::new("launch-refused");
        let daemon = FakeDaemon::new();
        let remote = FakeRemote::new().failing_connect(RemoteError::NoShelbi {
            host: "devbox".into(),
        });
        let recents = RecentRemotes::live();
        let outcome = perform(
            LaunchAction::Reconnect(remote_session("shelbi")),
            &daemon,
            &remote,
            &recents,
        );
        assert_eq!(
            outcome,
            Err(LaunchError::Remote(RemoteError::NoShelbi { host: "devbox".into() }))
        );
        assert!(recents.load().is_empty());
    }
}
