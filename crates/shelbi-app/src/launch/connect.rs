//! The Connect to Remote Session dialog's state: the same two steps in every
//! client (the SSH host, then the host's sessions), whichever way it was
//! opened (the launch screen's Connect to remote, the palette, the Shelbi
//! menu).
//!
//! [`ConnectFlow`] decides what each step shows and what a press does; the
//! client draws it, keeps its focus, and runs the two blocking calls
//! ([`RemoteConnector::find_sessions`], [`super::connect_remote`]) off its
//! UI thread, handing each answer back ([`ConnectFlow::found`],
//! [`ConnectFlow::connected`]). While a call runs the flow is busy: it
//! starts nothing else, and the field and buttons wait.
//!
//! A host whose Shelbi isn't running ([`RemoteError::DaemonStopped`]) is
//! said under the list, and the primary button offers to start it
//! ([`super::remote::START_DAEMON_LABEL`]): [`ConnectFlow::start`] hands
//! back the host for the client to run [`RemoteConnector::start_daemon`] on,
//! and [`ConnectFlow::started`] the session to connect to once it's up.
//! Nothing starts the far daemon without that press.
//!
//! [`RemoteConnector::find_sessions`]: super::RemoteConnector::find_sessions
//! [`RemoteConnector::start_daemon`]: super::RemoteConnector::start_daemon

use super::remote::{validate_host, RemoteError, RemoteSession};
use super::LaunchError;

/// The Host step's primary button while the search runs.
pub const FINDING_LABEL: &str = "Finding Sessions…";
/// The Choose session step's primary button while the connection is made.
pub const CONNECTING_LABEL: &str = "Connecting…";
/// The primary button while the host's Shelbi starts.
pub const STARTING_LABEL: &str = "Starting Shelbi…";

/// What the dialog says when the host answered with no sessions.
pub fn no_sessions(host: &str) -> String {
    format!("There are no Shelbi sessions on {host}.")
}

/// Which of the two screens shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// The SSH host field, Cancel, Find Sessions.
    Host,
    /// "Connected as <user>", Change Host, the host's sessions, Cancel,
    /// Connect.
    Choose,
}

/// The blocking call in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Finding,
    Connecting,
    /// Starting the host's Shelbi ([`ConnectFlow::start`]).
    Starting,
}

/// The host's answer to a search.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    host: String,
    user: String,
    sessions: Vec<RemoteSession>,
}

/// The dialog's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectFlow {
    host: String,
    stage: Stage,
    found: Option<Found>,
    selected: usize,
    busy: Option<Busy>,
    error: Option<String>,
    /// The session a connect found the host's daemon stopped for: the
    /// primary button offers to start it.
    stopped: Option<RemoteSession>,
}

impl Default for ConnectFlow {
    fn default() -> Self {
        ConnectFlow::new()
    }
}

impl ConnectFlow {
    /// The Host step with an empty field.
    pub fn new() -> ConnectFlow {
        ConnectFlow {
            host: String::new(),
            stage: Stage::Host,
            found: None,
            selected: 0,
            busy: None,
            error: None,
            stopped: None,
        }
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    /// The host field's text.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The host field changed: a refusal it showed goes.
    pub fn set_host(&mut self, text: &str) {
        if text != self.host {
            self.host = text.to_string();
            self.error = None;
        }
    }

    pub fn busy(&self) -> Option<Busy> {
        self.busy
    }

    /// The line under the field (Host) or the list (Choose), in red.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The line under the title: what to do on this step.
    pub fn intro(&self) -> String {
        match (&self.stage, &self.found) {
            (Stage::Choose, Some(found)) => super::remote::choose_intro(&found.host),
            _ => super::remote::HOST_INTRO.to_string(),
        }
    }

    /// The Choose step's "Connected as <user>".
    pub fn connected_as(&self) -> Option<String> {
        let found = self.found.as_ref().filter(|_| self.stage == Stage::Choose)?;
        Some(super::remote::connected_as(&found.user))
    }

    /// The host's sessions (empty on the Host step).
    pub fn sessions(&self) -> &[RemoteSession] {
        match (&self.stage, &self.found) {
            (Stage::Choose, Some(found)) => &found.sessions,
            _ => &[],
        }
    }

    /// The session the dialog's Connect connects to.
    pub fn selected(&self) -> usize {
        self.selected
    }

    pub fn select(&mut self, index: usize) {
        if index < self.sessions().len() {
            self.selected = index;
        }
    }

    /// The primary button's label, its busy form while its call runs.
    pub fn primary_label(&self) -> &'static str {
        match (self.stage, self.busy) {
            (Stage::Host, Some(Busy::Finding)) => FINDING_LABEL,
            (Stage::Host, _) => super::remote::FIND_SESSIONS_LABEL,
            (_, Some(Busy::Starting)) => STARTING_LABEL,
            (Stage::Choose, Some(Busy::Connecting)) => CONNECTING_LABEL,
            (Stage::Choose, _) if self.stopped.is_some() => super::remote::START_DAEMON_LABEL,
            (Stage::Choose, _) => super::remote::CONNECT_LABEL,
        }
    }

    /// Find Sessions: the host to search, or `None` (a refusal shows under
    /// the field, or a call is already running).
    pub fn find(&mut self) -> Option<String> {
        if self.busy.is_some() || self.stage != Stage::Host {
            return None;
        }
        match validate_host(&self.host) {
            Ok(host) => {
                self.error = None;
                self.busy = Some(Busy::Finding);
                Some(host)
            }
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }

    /// The search for `host` came back: its sessions on the Choose step, or
    /// the reason under the field.
    pub fn found(&mut self, host: &str, result: Result<(String, Vec<RemoteSession>), RemoteError>) {
        if self.busy != Some(Busy::Finding) {
            return;
        }
        self.busy = None;
        match result {
            Ok((_, sessions)) if sessions.is_empty() => self.error = Some(no_sessions(host)),
            Ok((user, sessions)) => {
                self.found = Some(Found {
                    host: host.to_string(),
                    user,
                    sessions,
                });
                self.stage = Stage::Choose;
                self.selected = 0;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    /// Change Host: back to the field, its text kept.
    pub fn change_host(&mut self) {
        if self.busy.is_some() {
            return;
        }
        self.stage = Stage::Host;
        self.error = None;
        self.stopped = None;
    }

    /// Connect to session `index` (a row's Connect), or the selected one
    /// (the dialog's): the session to connect to, or `None` while a call
    /// runs.
    pub fn connect(&mut self, index: Option<usize>) -> Option<RemoteSession> {
        if self.busy.is_some() {
            return None;
        }
        let index = index.unwrap_or(self.selected);
        let session = self.sessions().get(index)?.clone();
        self.selected = index;
        self.error = None;
        self.busy = Some(Busy::Connecting);
        Some(session)
    }

    /// The connection came back: `true` when it was made (the client closes
    /// the dialog), else the reason under the list.
    pub fn connected(&mut self, result: Result<RemoteSession, LaunchError>) -> bool {
        if self.busy != Some(Busy::Connecting) {
            return false;
        }
        self.busy = None;
        match result {
            Ok(_) => {
                self.stopped = None;
                true
            }
            Err(e) => {
                if let LaunchError::Remote(RemoteError::DaemonStopped { .. }) = &e {
                    self.stopped = self.sessions().get(self.selected).cloned();
                }
                self.error = Some(e.to_string());
                false
            }
        }
    }

    /// Whether the primary button starts the host's Shelbi (a connect found
    /// it stopped) rather than connecting.
    pub fn offers_start(&self) -> bool {
        self.stage == Stage::Choose && self.stopped.is_some()
    }

    /// Start Shelbi: the host to start it on, or `None` (nothing to start, or
    /// a call is running).
    pub fn start(&mut self) -> Option<String> {
        if self.busy.is_some() || !self.offers_start() {
            return None;
        }
        self.error = None;
        self.busy = Some(Busy::Starting);
        self.stopped.as_ref().map(|s| s.host.clone())
    }

    /// The start came back: the session to connect to now (the flow is
    /// connecting; hand its answer to [`Self::connected`]), or `None` with
    /// the reason under the list.
    pub fn started(&mut self, result: Result<(), RemoteError>) -> Option<RemoteSession> {
        if self.busy != Some(Busy::Starting) {
            return None;
        }
        self.busy = None;
        match result {
            Ok(()) => {
                let session = self.stopped.take()?;
                self.busy = Some(Busy::Connecting);
                Some(session)
            }
            Err(e) => {
                self.error = Some(e.to_string());
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::remote::{choose_intro, HOST_INTRO, START_DAEMON_LABEL};

    #[test]
    fn a_stopped_daemon_offers_to_start_it_then_connects() {
        let mut flow = on_choose();
        flow.select(1);
        let web = flow.connect(None).unwrap();
        assert!(flow.start().is_none(), "nothing to start yet");
        flow.connected(Err(LaunchError::Remote(RemoteError::DaemonStopped { host: "devbox".into() })));
        assert_eq!(flow.error(), Some("Shelbi isn't running on devbox."));
        assert!(flow.offers_start());
        assert_eq!(flow.primary_label(), START_DAEMON_LABEL);

        assert_eq!(flow.start().as_deref(), Some("devbox"));
        assert_eq!(flow.primary_label(), STARTING_LABEL);
        assert!(flow.connect(None).is_none(), "busy while it starts");
        assert_eq!(flow.started(Ok(())), Some(web.clone()));
        assert_eq!(flow.busy(), Some(Busy::Connecting));
        assert!(flow.connected(Ok(web)));
        assert!(!flow.offers_start());
    }

    #[test]
    fn a_failed_start_says_why_and_keeps_the_offer_off() {
        let mut flow = on_choose();
        flow.connect(None).unwrap();
        flow.connected(Err(LaunchError::Remote(RemoteError::DaemonStopped { host: "devbox".into() })));
        flow.start().unwrap();
        assert_eq!(flow.started(Err(RemoteError::Failed("Shelbi couldn't start on devbox: boom".into()))), None);
        assert_eq!(flow.error(), Some("Shelbi couldn't start on devbox: boom"));
        // Change Host drops the offer.
        flow.change_host();
        assert!(!flow.offers_start());
    }

    fn session(project: &str) -> RemoteSession {
        RemoteSession {
            host: "devbox".into(),
            user: "jlong".into(),
            project: project.into(),
            label: None,
            path: format!("~/Workspaces/{project}"),
        }
    }

    fn on_choose() -> ConnectFlow {
        let mut flow = ConnectFlow::new();
        flow.set_host(" devbox ");
        let host = flow.find().unwrap();
        flow.found(&host, Ok(("jlong".into(), vec![session("shelbi"), session("web")])));
        flow
    }

    #[test]
    fn find_validates_the_host_and_waits_for_the_answer() {
        let mut flow = ConnectFlow::new();
        assert_eq!(flow.intro(), HOST_INTRO);
        assert_eq!(flow.find(), None);
        assert_eq!(flow.error(), Some("Enter an SSH host."));
        flow.set_host("-oProxyCommand=x");
        assert!(flow.error().is_none(), "typing clears the refusal");
        assert_eq!(flow.find(), None);
        assert!(flow.error().is_some());

        flow.set_host(" devbox ");
        assert_eq!(flow.find().as_deref(), Some("devbox"));
        assert_eq!(flow.busy(), Some(Busy::Finding));
        assert_eq!(flow.primary_label(), FINDING_LABEL);
        assert_eq!(flow.find(), None, "one search at a time");
    }

    #[test]
    fn a_failed_or_empty_search_stays_on_the_host_step() {
        let mut flow = ConnectFlow::new();
        flow.set_host("down");
        flow.find();
        flow.found("down", Err(RemoteError::NoShelbi { host: "down".into() }));
        assert_eq!(flow.stage(), Stage::Host);
        assert_eq!(flow.error(), Some("Shelbi isn't installed on down."));
        assert_eq!(flow.busy(), None);

        flow.find();
        flow.found("down", Ok(("jlong".into(), Vec::new())));
        assert_eq!(flow.stage(), Stage::Host);
        assert_eq!(flow.error(), Some(no_sessions("down").as_str()));
    }

    #[test]
    fn found_sessions_show_on_the_choose_step() {
        let flow = on_choose();
        assert_eq!(flow.stage(), Stage::Choose);
        assert_eq!(flow.intro(), choose_intro("devbox"));
        assert_eq!(flow.connected_as().as_deref(), Some("Connected as jlong"));
        assert_eq!(flow.sessions().len(), 2);
        assert_eq!(flow.primary_label(), "Connect");
    }

    #[test]
    fn connect_takes_a_row_or_the_selection_and_reports_back() {
        let mut flow = on_choose();
        flow.select(1);
        flow.select(9);
        assert_eq!(flow.selected(), 1, "out of range is ignored");
        assert_eq!(flow.connect(None), Some(session("web")));
        assert_eq!(flow.primary_label(), CONNECTING_LABEL);
        assert_eq!(flow.connect(Some(0)), None, "busy");
        flow.change_host();
        assert_eq!(flow.stage(), Stage::Choose, "Change Host waits too");

        let refused = Err(LaunchError::Remote(RemoteError::Failed("Permission denied".into())));
        assert!(!flow.connected(refused));
        assert_eq!(flow.error(), Some("Permission denied"));
        assert_eq!(flow.connect(Some(0)), Some(session("shelbi")));
        assert_eq!(flow.selected(), 0);
        assert!(flow.connected(Ok(session("shelbi"))));
    }

    #[test]
    fn change_host_goes_back_with_the_text_kept() {
        let mut flow = on_choose();
        flow.change_host();
        assert_eq!(flow.stage(), Stage::Host);
        assert_eq!(flow.host(), " devbox ");
        assert!(flow.sessions().is_empty());
        assert!(flow.connected_as().is_none());
    }
}
