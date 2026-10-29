//! Connecting to Shelbi on another machine: the session types, the
//! [`RemoteConnector`] seam the launch screen and the Connect to Remote
//! Session dialog call, and the dialog's words.
//!
//! The real connector (over SSH, with the user's own keys and config) is the
//! `launch-remote-connect` work; [`FakeRemote`] stands in for it in tests.

use serde::{Deserialize, Serialize};

/// The dialog's name: the command and the menu item (the TUI's heading).
pub const DIALOG_TITLE: &str = "Connect to Remote Session";
/// The heading the Host and Choose session frames draw (Figma 2541:10275):
/// the desktop dialog's.
pub const DIALOG_HEADING: &str = "Connect to remote";
/// The line under the title on the Host screen.
pub const HOST_INTRO: &str = "Connect to Shelbi running on another machine.";
/// The host field's label.
pub const HOST_LABEL: &str = "SSH host";
/// The note under the host field.
pub const HOST_HINT: &str =
    "Use an SSH alias or user@hostname. Your existing SSH keys and configuration are used.";
/// The Host screen's primary button.
pub const FIND_SESSIONS_LABEL: &str = "Find Sessions";
/// Both screens' secondary button.
pub const CANCEL_LABEL: &str = "Cancel";
/// The Choose session screen's button back to the Host screen.
pub const CHANGE_HOST_LABEL: &str = "Change host";
/// The Choose session screen's primary button, and each row's.
pub const CONNECT_LABEL: &str = "Connect";

/// The line under the title on the Choose session screen.
pub fn choose_intro(host: &str) -> String {
    format!("Choose a session on {host}.")
}

/// The Choose session screen's line beside the check.
pub fn connected_as(user: &str) -> String {
    format!("Connected as {user}")
}

/// A Shelbi project on another machine: what a session row shows, what
/// [`RemoteConnector::connect`] opens, and what the recent remote sessions
/// remember.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteSession {
    /// The host as the user gave it: an SSH alias or `user@hostname`.
    pub host: String,
    /// The user Shelbi runs as on the host.
    pub user: String,
    /// The project's id on the host.
    pub project: String,
    /// The project's display name, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The project's folder on the host, as the host reports it
    /// (`~/Workspaces/shelbi`).
    pub path: String,
}

impl RemoteSession {
    /// The row's title: the display name, else the id.
    pub fn display_label(&self) -> &str {
        self.label.as_deref().unwrap_or(&self.project)
    }

    /// The row's second line, `user@host:path`. A host given as
    /// `user@hostname` isn't doubled up.
    pub fn address(&self) -> String {
        let host = self.host.rsplit_once('@').map_or(self.host.as_str(), |(_, h)| h);
        format!("{}@{host}:{}", self.user, self.path)
    }

    /// Whether `other` is the same session: the same project of the same
    /// user's Shelbi on the same host. Its folder and name may have changed.
    pub fn same_session(&self, other: &RemoteSession) -> bool {
        self.host == other.host && self.user == other.user && self.project == other.project
    }
}

/// Why a remote step failed. Each carries the line the dialog shows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RemoteError {
    /// The host field holds something that can't be an SSH destination.
    #[error("{0}")]
    InvalidHost(String),
    /// SSH couldn't reach or log in to the host.
    #[error("Couldn't connect to {host}: {reason}")]
    Unreachable { host: String, reason: String },
    /// The host answered, but has no Shelbi to connect to.
    #[error("Shelbi isn't installed on {host}.")]
    NoShelbi { host: String },
    /// Anything else, already worded for the dialog.
    #[error("{0}")]
    Failed(String),
}

/// The host field's text as an SSH destination: trimmed, and refused when
/// it is empty, holds whitespace, or starts with `-` (which `ssh` would read
/// as an option).
pub fn validate_host(host: &str) -> Result<String, RemoteError> {
    let host = host.trim();
    if host.is_empty() {
        return Err(RemoteError::InvalidHost("Enter an SSH host.".into()));
    }
    if host.starts_with('-') || host.chars().any(char::is_whitespace) {
        return Err(RemoteError::InvalidHost(format!(
            "“{host}” isn't an SSH alias or user@hostname."
        )));
    }
    Ok(host.to_string())
}

/// How a client reaches Shelbi on another machine. Both calls block (they
/// run `ssh`), so a client makes them off its UI thread.
pub trait RemoteConnector: Send + Sync {
    /// Log in to `host` and list its Shelbi sessions: the user Shelbi runs as
    /// there, and its projects.
    fn find_sessions(&self, host: &str) -> Result<(String, Vec<RemoteSession>), RemoteError>;

    /// Connect to `session`.
    fn connect(&self, session: &RemoteSession) -> Result<(), RemoteError>;
}

/// A scripted [`RemoteConnector`] for tests: hosts answer what they were
/// given, and every search and connection is recorded.
#[cfg(any(test, feature = "test-support"))]
#[derive(Default)]
pub struct FakeRemote {
    hosts: std::sync::Mutex<std::collections::HashMap<String, FakeHost>>,
    connect_error: std::sync::Mutex<Option<RemoteError>>,
    searched: std::sync::Mutex<Vec<String>>,
    connected: std::sync::Mutex<Vec<RemoteSession>>,
}

#[cfg(any(test, feature = "test-support"))]
type FakeHost = Result<(String, Vec<RemoteSession>), RemoteError>;

#[cfg(any(test, feature = "test-support"))]
impl FakeRemote {
    pub fn new() -> FakeRemote {
        FakeRemote::default()
    }

    /// `host` logs in as `user` and lists `projects` (id, folder).
    pub fn with_host(self, host: &str, user: &str, projects: &[(&str, &str)]) -> FakeRemote {
        let sessions = projects
            .iter()
            .map(|(project, path)| RemoteSession {
                host: host.to_string(),
                user: user.to_string(),
                project: project.to_string(),
                label: None,
                path: path.to_string(),
            })
            .collect();
        self.lock_hosts().insert(host.to_string(), Ok((user.to_string(), sessions)));
        self
    }

    /// Searching `host` fails with `error`.
    pub fn with_failing_host(self, host: &str, error: RemoteError) -> FakeRemote {
        self.lock_hosts().insert(host.to_string(), Err(error));
        self
    }

    /// Every connect fails with `error`.
    pub fn failing_connect(self, error: RemoteError) -> FakeRemote {
        *self.connect_error.lock().unwrap_or_else(|p| p.into_inner()) = Some(error);
        self
    }

    /// The hosts searched, in order.
    pub fn searched(&self) -> Vec<String> {
        self.searched.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// The sessions connected to, in order.
    pub fn connected(&self) -> Vec<RemoteSession> {
        self.connected.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    fn lock_hosts(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, FakeHost>> {
        self.hosts.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(any(test, feature = "test-support"))]
impl RemoteConnector for FakeRemote {
    fn find_sessions(&self, host: &str) -> Result<(String, Vec<RemoteSession>), RemoteError> {
        let host = validate_host(host)?;
        self.searched
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(host.clone());
        self.lock_hosts().get(&host).cloned().unwrap_or_else(|| {
            Err(RemoteError::Unreachable {
                host: host.clone(),
                reason: "Could not resolve hostname".into(),
            })
        })
    }

    fn connect(&self, session: &RemoteSession) -> Result<(), RemoteError> {
        if let Some(error) = self.connect_error.lock().unwrap_or_else(|p| p.into_inner()).clone() {
            return Err(error);
        }
        self.connected
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .push(session.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(host: &str, user: &str, project: &str) -> RemoteSession {
        RemoteSession {
            host: host.into(),
            user: user.into(),
            project: project.into(),
            label: None,
            path: format!("~/Workspaces/{project}"),
        }
    }

    #[test]
    fn a_row_reads_user_at_host_colon_path() {
        let mut s = session("devbox", "jlong", "shelbi");
        assert_eq!(s.address(), "jlong@devbox:~/Workspaces/shelbi");
        assert_eq!(s.display_label(), "shelbi");
        s.label = Some("Shelbi".into());
        assert_eq!(s.display_label(), "Shelbi");
        // A host typed as user@hostname keeps one user in the address.
        let s = session("jlong@build-server", "jlong", "web");
        assert_eq!(s.address(), "jlong@build-server:~/Workspaces/web");
    }

    #[test]
    fn same_session_ignores_folder_and_name() {
        let a = session("devbox", "jlong", "shelbi");
        let mut moved = a.clone();
        moved.path = "/srv/shelbi".into();
        moved.label = Some("Shelbi".into());
        assert!(a.same_session(&moved));
        assert!(!a.same_session(&session("devbox", "root", "shelbi")));
        assert!(!a.same_session(&session("build", "jlong", "shelbi")));
        assert!(!a.same_session(&session("devbox", "jlong", "web")));
    }

    #[test]
    fn hosts_that_could_be_ssh_options_are_refused() {
        assert_eq!(validate_host("  devbox \n").unwrap(), "devbox");
        assert_eq!(validate_host("jlong@10.0.0.4").unwrap(), "jlong@10.0.0.4");
        assert!(matches!(validate_host("   "), Err(RemoteError::InvalidHost(_))));
        assert!(matches!(validate_host("-oProxyCommand=x"), Err(RemoteError::InvalidHost(_))));
        assert!(matches!(validate_host("dev box"), Err(RemoteError::InvalidHost(_))));
    }

    #[test]
    fn the_fake_answers_scripted_hosts_and_records_calls() {
        let fake = FakeRemote::new()
            .with_host("devbox", "jlong", &[("shelbi", "~/Workspaces/shelbi")])
            .with_failing_host("down", RemoteError::NoShelbi { host: "down".into() });

        let (user, sessions) = fake.find_sessions(" devbox ").unwrap();
        assert_eq!(user, "jlong");
        assert_eq!(sessions, vec![session("devbox", "jlong", "shelbi")]);
        assert_eq!(
            fake.find_sessions("down").unwrap_err().to_string(),
            "Shelbi isn't installed on down."
        );
        assert!(matches!(
            fake.find_sessions("nowhere"),
            Err(RemoteError::Unreachable { .. })
        ));
        assert_eq!(fake.searched(), vec!["devbox", "down", "nowhere"]);

        fake.connect(&sessions[0]).unwrap();
        assert_eq!(fake.connected(), sessions);

        let refusing = FakeRemote::new().failing_connect(RemoteError::Failed("no".into()));
        assert!(refusing.connect(&sessions[0]).is_err());
        assert!(refusing.connected().is_empty());
    }

    #[test]
    fn the_dialog_lines_name_the_host_and_user() {
        assert_eq!(choose_intro("build-server"), "Choose a session on build-server.");
        assert_eq!(connected_as("jlong"), "Connected as jlong");
    }
}
