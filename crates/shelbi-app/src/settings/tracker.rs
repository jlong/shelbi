//! Settings ▸ Properties' Issue tracker section (`linear-settings-ui`),
//! toolkit-free: where the project's issues live, and switching or editing
//! that connection. The desktop app's and the TUI's Properties pages draw
//! [`TrackerForm`].
//!
//! ```text
//!  Issue tracker
//!    ( ) Local board      ( ) GitHub Issues      (•) Linear
//!
//!    Linear account   Ada Lovelace <ada@…>               [Change key…]
//!    Team *           [ ENG · Engineering        ▾ ]
//!    Project          [ Any                      ▾ ]
//!    Label            [ Any                      ▾ ]
//!    Assignee         [ Me                       ▾ ]
//!
//!    Shelbi shows and creates issues in ENG, project Shelbi, assigned to you.
//!                                                     [Cancel] [Connect]
//! ```
//!
//! The choice is the new-project flow's ([`IssuesChoice`], with Linear's
//! key, team and filter pickers in [`crate::linear_setup::LinearChoice`]),
//! seeded with the project's connection. Unlike the other Properties fields
//! nothing saves until Connect (or Save, for the same board's new filters):
//! a connection is one operation
//! (`shelbi_orchestrator::tracker_connect::apply_tracker_connection`), run
//! where the project lives through a [`TrackerService`]. It preflights the
//! new tracker, writes exactly one tracker block (**one external
//! integration**: GitHub or Linear, never both), and, for Linear, syncs the
//! status catalog. It refuses to switch boards while the current one has
//! issues; the refusal says how to migrate them, and the section offers
//! [`SWITCH_ANYWAY`] (the CLI's `--force`).

use std::sync::Arc;

use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig};

use crate::linear_setup::{Filter, LinearAnswer, LinearChoice, LinearRequest, TEAM_REQUIRED};
use crate::new_project::{github_config, IssuesChoice, Tracker};
use crate::source::ProjectLink;
use crate::tracker_connect::{ConnectError, ConnectTracker, Connected, StatusSync, TrackerChoice};

/// The section's title and the line under it.
pub const TITLE: &str = "Issue tracker";
pub const HELP: &str = "Where this project's issues live. A project uses one external tracker at most: \
     GitHub or Linear.";
/// The local board's choice, as Settings names it.
pub const LOCAL_TITLE: &str = "Local board";
/// The buttons.
pub const CONNECT: &str = "Connect";
pub const SAVE: &str = "Save";
pub const USE_LOCAL: &str = "Use local board";
pub const CANCEL: &str = "Cancel";
pub const SWITCH_ANYWAY: &str = "Switch anyway";
/// While a connection runs.
pub const CONNECTING: &str = "Connecting…";

/// A choice's name in Settings (the local board reads as a board here, not
/// as "File system").
pub fn choice_title(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::FileSystem => LOCAL_TITLE,
        other => other.title(),
    }
}

/// The choices in the order Settings lists them: the local board first.
pub const CHOICES: [Tracker; 3] = [Tracker::FileSystem, Tracker::Github, Tracker::Linear];

/// The board `cfg` connects to.
fn tracker_of(cfg: &IssueTrackerConfig) -> Tracker {
    match cfg.backend {
        IssueTrackerBackend::Github => Tracker::Github,
        IssueTrackerBackend::Linear => Tracker::Linear,
        IssueTrackerBackend::FileSystem | IssueTrackerBackend::Jira => Tracker::FileSystem,
    }
}

/// The blocking work the section needs, run where the project lives: this
/// machine (`shelbi_orchestrator::tracker_connect::local_service`) or the
/// far daemon ([`TrackerService::far`]).
#[derive(Clone)]
pub struct TrackerService {
    /// Connect, switch or disconnect.
    pub connect: Arc<dyn Fn(ConnectTracker) -> Result<Connected, ConnectError> + Send + Sync>,
    /// One Linear read for the pickers (or a pasted key's login).
    pub linear: Arc<dyn Fn(LinearRequest) -> LinearAnswer + Send + Sync>,
    /// Sync the status catalog from the tracker now (Workflows' Sync now).
    pub sync_statuses: Arc<dyn Fn() -> StatusSync + Send + Sync>,
}

impl std::fmt::Debug for TrackerService {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TrackerService")
    }
}

impl TrackerService {
    /// The far daemon's, over `link`.
    pub fn far(link: &ProjectLink) -> Self {
        let (a, b, c) = (link.clone(), link.clone(), link.clone());
        TrackerService {
            connect: Arc::new(move |req| crate::far::connect_tracker(&a, req)),
            linear: Arc::new(move |req| crate::far::linear(&b, req)),
            sync_statuses: Arc::new(move || crate::far::sync_statuses(&c)),
        }
    }
}

/// The section's state: the connection as read, the choice as picked, and
/// the connection in flight or its outcome.
#[derive(Debug, Clone)]
pub struct TrackerForm {
    /// The project's `issue_tracker:` as last read; `None` until it is.
    current: Option<IssueTrackerConfig>,
    choice: IssuesChoice,
    /// The user changed the choice since it was seeded: a reload keeps it.
    edited: bool,
    connecting: bool,
    /// Why Connect was refused before it ran (no team, a misspelled repo).
    error: Option<String>,
    /// The last connection's refusal.
    refused: Option<ConnectError>,
    /// The last connection that was written.
    connected: Option<Connected>,
}

impl Default for TrackerForm {
    fn default() -> Self {
        TrackerForm {
            current: None,
            choice: IssuesChoice::recommended(None),
            edited: false,
            connecting: false,
            error: None,
            refused: None,
            connected: None,
        }
    }
}

impl TrackerForm {
    /// The project's config was read. The choice follows it unless the user
    /// has changed it (or a connection is running, whose outcome is newer).
    pub fn load(&mut self, cfg: &IssueTrackerConfig) {
        if self.connecting {
            return;
        }
        let changed = self.current.as_ref() != Some(cfg);
        self.current = Some(cfg.clone());
        if changed && !self.edited {
            self.seed();
        }
    }

    /// Forget everything (another project is shown).
    pub fn reset(&mut self) {
        *self = TrackerForm::default();
    }

    fn seed(&mut self) {
        let Some(cfg) = &self.current else { return };
        self.choice = match TrackerChoice::from_config(cfg) {
            TrackerChoice::FileSystem => {
                let mut c = IssuesChoice::recommended(None);
                c.select(Tracker::FileSystem);
                c
            }
            TrackerChoice::Github { repo } => IssuesChoice::recommended(Some(&repo)),
            TrackerChoice::Linear(conn) => IssuesChoice::linear_with(conn),
        };
        self.edited = false;
        self.error = None;
    }

    /// Whether the connection has been read.
    pub fn loaded(&self) -> bool {
        self.current.is_some()
    }

    /// The connection as read.
    pub fn current(&self) -> Option<&IssueTrackerConfig> {
        self.current.as_ref()
    }

    /// The board the project uses now.
    pub fn current_tracker(&self) -> Option<Tracker> {
        self.current.as_ref().map(tracker_of)
    }

    /// The board picked.
    pub fn tracker(&self) -> Tracker {
        self.choice.tracker()
    }

    pub fn issues(&self) -> &IssuesChoice {
        &self.choice
    }

    pub fn linear(&self) -> &LinearChoice {
        self.choice.linear()
    }

    /// Whether the section takes edits: the connection was read and none is
    /// running.
    pub fn editable(&self) -> bool {
        self.loaded() && !self.connecting
    }

    fn edit(&mut self) {
        self.edited = true;
        self.error = None;
        self.refused = None;
        self.connected = None;
    }

    /// Pick a board.
    pub fn select(&mut self, tracker: Tracker) {
        if self.editable() && tracker != self.tracker() {
            self.choice.select(tracker);
            self.edit();
        }
    }

    /// The next (or previous) board, in [`CHOICES`] order, wrapping.
    pub fn step(&mut self, forward: bool) {
        let n = CHOICES.len();
        let at = CHOICES.iter().position(|t| *t == self.tracker()).unwrap_or(0);
        self.select(CHOICES[if forward { (at + 1) % n } else { (at + n - 1) % n }]);
    }

    /// The GitHub repo field's text.
    pub fn set_repo(&mut self, text: &str) {
        if self.editable() && text != self.choice.repo() {
            self.choice.set_repo(text);
            self.edit();
        }
    }

    /// Pick the Linear team (a team row's value).
    pub fn select_team(&mut self, team: &str) {
        if self.editable() && team.trim() != self.linear().team() {
            self.choice.linear_mut().select_team(team);
            self.edit();
        }
    }

    /// Pick a Linear filter (a row's value; `None` or blank: none).
    pub fn select_filter(&mut self, filter: Filter, value: Option<&str>) {
        if self.editable() {
            let before = self.linear().filter(filter).map(str::to_string);
            self.choice.linear_mut().select_filter(filter, value);
            if self.linear().filter(filter).map(str::to_string) != before {
                self.edit();
            }
        }
    }

    /// An arrow key on the team picker.
    pub fn step_team(&mut self, forward: bool) {
        if self.editable() {
            let before = self.linear().connection();
            self.choice.linear_mut().step_team(forward);
            if self.linear().connection() != before {
                self.edit();
            }
        }
    }

    /// An arrow key on a filter's picker.
    pub fn step_filter(&mut self, filter: Filter, forward: bool) {
        if self.editable() {
            let before = self.linear().connection();
            self.choice.linear_mut().step_filter(filter, forward);
            if self.linear().connection() != before {
                self.edit();
            }
        }
    }

    /// The Linear key field's text.
    pub fn set_key_input(&mut self, text: &str) {
        self.choice.linear_mut().set_key_input(text);
    }

    /// Verify: check the pasted key with Linear and store it (the host runs
    /// the login from [`Self::linear_due`]). False when nothing was sent.
    pub fn submit_key(&mut self) -> bool {
        self.choice.linear_mut().submit_key()
    }

    /// "Change key…" / its Cancel.
    pub fn change_key(&mut self) {
        self.choice.linear_mut().change_key();
    }

    pub fn cancel_change_key(&mut self) {
        self.choice.linear_mut().cancel_change_key();
    }

    /// Ask Linear again for the lists whose read failed.
    pub fn retry_linear(&mut self) {
        self.choice.linear_mut().retry();
    }

    /// The Linear read due now (Linear picked), marked running: the host
    /// runs it off the UI thread with its [`TrackerService::linear`] and
    /// answers with [`Self::linear_answer`]. Call until `None`.
    pub fn linear_due(&mut self) -> Option<LinearRequest> {
        self.choice.linear_due()
    }

    pub fn linear_answer(&mut self, answer: LinearAnswer) {
        self.choice.linear_answer(answer);
    }

    /// The connection as picked, `None` while it can't be one (a Linear
    /// choice with no team, an empty repo).
    pub fn wanted(&self) -> Option<TrackerChoice> {
        match self.tracker() {
            Tracker::FileSystem => Some(TrackerChoice::FileSystem),
            Tracker::Github => {
                let repo = self.choice.repo().trim();
                (!repo.is_empty()).then(|| TrackerChoice::Github { repo: repo.to_string() })
            }
            Tracker::Linear => {
                let conn = self.linear().connection();
                conn.team_ref().is_some().then_some(TrackerChoice::Linear(conn))
            }
        }
    }

    /// The choice differs from the project's connection.
    pub fn dirty(&self) -> bool {
        let Some(current) = &self.current else { return false };
        Some(self.tracker()) != self.current_tracker() || self.wanted() != Some(TrackerChoice::from_config(current))
    }

    /// The primary button's label: Connect a new board, Save the same
    /// board's changes, or go back to the local board.
    pub fn primary_label(&self) -> &'static str {
        match self.tracker() {
            _ if Some(self.tracker()) == self.current_tracker() => SAVE,
            Tracker::FileSystem => USE_LOCAL,
            _ => CONNECT,
        }
    }

    /// The primary button can be pressed.
    pub fn primary_enabled(&self) -> bool {
        if !self.editable() || self.linear_busy() {
            return false;
        }
        if self.tracker() == Tracker::Linear && self.linear().needs_key() {
            // Verify the key first (the key field's own button).
            return false;
        }
        self.dirty()
    }

    /// Cancel can be pressed: there's a change to throw away.
    pub fn cancel_enabled(&self) -> bool {
        self.editable() && (self.dirty() || self.edited || self.refused.is_some())
    }

    fn linear_busy(&self) -> bool {
        self.tracker() == Tracker::Linear && self.linear().busy()
    }

    /// Connect (or Save): the request the host runs with its
    /// [`TrackerService::connect`], answering [`Self::connected`]. `None`
    /// when there's nothing to send, or the choice was refused up front
    /// ([`Self::error`] says why). `force` switches even though the
    /// current board has issues ([`SWITCH_ANYWAY`]).
    pub fn submit(&mut self, force: bool) -> Option<ConnectTracker> {
        if !self.editable() || self.linear_busy() || !self.dirty() {
            return None;
        }
        let choice = match self.tracker() {
            Tracker::FileSystem => TrackerChoice::FileSystem,
            Tracker::Github => match github_config(self.choice.repo()) {
                Ok(_) => TrackerChoice::Github { repo: self.choice.repo().trim().to_string() },
                Err(why) => {
                    self.error = Some(why);
                    return None;
                }
            },
            Tracker::Linear => match self.wanted() {
                _ if self.linear().needs_key() => return None,
                Some(choice) => choice,
                None => {
                    self.error = Some(TEAM_REQUIRED.into());
                    return None;
                }
            },
        };
        self.error = None;
        self.refused = None;
        self.connected = None;
        self.connecting = true;
        Some(ConnectTracker { choice, force })
    }

    /// The connection's outcome. Written: the section shows the new
    /// connection (the host reads the project again, and the Workflows page
    /// picks up a synced catalog). Refused: the choice stays, with why.
    pub fn connected(&mut self, result: Result<Connected, ConnectError>) {
        if !std::mem::take(&mut self.connecting) {
            return;
        }
        match result {
            Ok(done) => {
                if let (Some(current), Some(wanted)) = (&self.current, self.wanted()) {
                    self.current = Some(wanted.config(current));
                }
                self.edited = false;
                self.connected = Some(done);
            }
            Err(e) => self.refused = Some(e),
        }
    }

    /// Cancel: back to the project's connection.
    pub fn cancel(&mut self) {
        if self.editable() {
            self.seed();
            self.refused = None;
        }
    }

    /// A connection is running.
    pub fn connecting(&self) -> bool {
        self.connecting
    }

    /// Why the choice can't be connected, or why the last connection was
    /// refused, as a sentence.
    pub fn error(&self) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        if let Some(e) = &self.refused {
            return Some(crate::view::title_case(e.message()));
        }
        match self.tracker() {
            Tracker::Linear => self.linear().error().map(str::to_string),
            _ => None,
        }
    }

    /// The last refusal can be overridden: the current board has issues
    /// (or couldn't be counted), and [`SWITCH_ANYWAY`] leaves them behind.
    pub fn offers_force(&self) -> bool {
        matches!(self.refused, Some(ConnectError::NotEmpty { .. } | ConnectError::Unverified { .. }))
    }

    /// The line under the buttons: the connection running, or the one just
    /// written (and what became of the statuses).
    pub fn status_line(&self) -> Option<String> {
        if self.connecting {
            return Some(CONNECTING.into());
        }
        let done = self.connected.as_ref()?;
        let mut line = format!("Connected: {}.", done.summary.trim_end_matches('.'));
        match &done.statuses {
            StatusSync::NotNeeded => {}
            StatusSync::Synced(what) => line.push_str(&format!(" Statuses synced: {what}.")),
            StatusSync::Failed(why) => line.push_str(&format!(" The statuses didn't sync: {why}.")),
        }
        Some(line)
    }

    /// The last connection written, until the next edit.
    pub fn last_connected(&self) -> Option<&Connected> {
        self.connected.as_ref()
    }

    /// The sentence saying what the Linear choice shows and creates.
    pub fn scope_line(&self) -> Option<String> {
        (self.tracker() == Tracker::Linear).then(|| self.linear().scope_line()).flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linear_setup::{LinearFailure, PickOption, KEY_EMPTY};
    use shelbi_core::{GithubConnection, LinearConnection};

    fn opt(id: &str, value: &str, label: &str) -> PickOption {
        PickOption { id: id.into(), value: value.into(), label: label.into(), detail: None }
    }

    fn github(repo: &str) -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: repo.into() }),
            ..IssueTrackerConfig::default()
        }
    }

    fn linear_cfg(conn: LinearConnection) -> IssueTrackerConfig {
        IssueTrackerConfig { backend: IssueTrackerBackend::Linear, linear: Some(conn), ..IssueTrackerConfig::default() }
    }

    fn connected(backend: IssueTrackerBackend) -> Connected {
        Connected {
            backend,
            previous: IssueTrackerBackend::Github,
            file: "app.yaml".into(),
            summary: "Linear: ENG (Engineering)".into(),
            statuses: StatusSync::Synced("6 statuses from the Linear team".into()),
        }
    }

    /// Run the form's Linear reads with canned answers: one team, its lists.
    fn answer_reads(form: &mut TrackerForm) -> Vec<LinearRequest> {
        let mut asked = Vec::new();
        while let Some(req) = form.linear_due() {
            asked.push(req.clone());
            let answer = match req {
                LinearRequest::Teams => LinearAnswer::Teams(Ok(vec![opt("team-eng", "ENG", "ENG")])),
                LinearRequest::Viewer => LinearAnswer::Viewer(Ok("Ada <ada@example.com>".into())),
                LinearRequest::TeamLists { team_id } => LinearAnswer::TeamLists {
                    team_id,
                    projects: Ok(vec![opt("p1", "Shelbi", "Shelbi")]),
                    labels: Ok(vec![opt("l1", "shelbi", "shelbi")]),
                    users: Ok(vec![]),
                },
                LinearRequest::Login(_) => LinearAnswer::Login(Ok("Ada <ada@example.com>".into())),
                LinearRequest::Preflight(connection) => LinearAnswer::Preflight { connection, result: Ok("ENG".into()) },
            };
            form.linear_answer(answer);
        }
        asked
    }

    #[test]
    fn it_seeds_from_the_project_and_saves_nothing_until_connect() {
        let mut form = TrackerForm::default();
        assert!(!form.editable() && form.submit(false).is_none());
        form.load(&github("acme/app"));
        assert_eq!((form.tracker(), form.issues().repo()), (Tracker::Github, "acme/app"));
        assert!(!form.dirty() && !form.primary_enabled());
        assert_eq!(form.primary_label(), SAVE);

        form.select(Tracker::FileSystem);
        assert!(form.dirty());
        assert_eq!(form.primary_label(), USE_LOCAL);
        // A reload keeps the user's pick.
        form.load(&github("acme/other"));
        assert_eq!(form.tracker(), Tracker::FileSystem);
        form.cancel();
        assert_eq!((form.tracker(), form.issues().repo()), (Tracker::Github, "acme/other"));

        form.set_repo("not a repo");
        assert!(form.submit(false).is_none());
        assert!(form.error().unwrap().contains("owner/repo"), "{:?}", form.error());
        form.set_repo("acme/new");
        let req = form.submit(false).unwrap();
        assert_eq!(req.choice, TrackerChoice::Github { repo: "acme/new".into() });
        assert!(form.connecting() && !form.editable());
        assert_eq!(form.status_line().as_deref(), Some(CONNECTING));
    }

    #[test]
    fn linear_needs_a_working_key_and_a_team_and_shows_its_scope() {
        let mut form = TrackerForm::default();
        form.load(&IssueTrackerConfig::default());
        assert_eq!(form.tracker(), Tracker::FileSystem);
        assert_eq!(form.linear_due(), None, "nothing to read for the local board");
        form.select(Tracker::Linear);
        assert_eq!(form.primary_label(), CONNECT);

        // No key: the key field shows and Connect waits for Verify.
        assert_eq!(form.linear_due(), Some(LinearRequest::Teams));
        form.linear_answer(LinearAnswer::Teams(Err(LinearFailure::MissingKey("no key".into()))));
        assert!(form.linear().needs_key() && !form.primary_enabled());
        assert!(!form.submit_key());
        assert_eq!(form.error().as_deref(), Some(KEY_EMPTY));
        form.set_key_input("lin_api_x");
        assert!(form.submit_key());
        let asked = answer_reads(&mut form);
        assert!(matches!(asked[0], LinearRequest::Login(_)));
        assert_eq!(asked[1], LinearRequest::Teams, "the teams load with the stored key");
        assert_eq!(form.linear().account(), Some("Ada <ada@example.com>"));

        assert!(form.submit(false).is_none());
        assert_eq!(form.error().as_deref(), Some(TEAM_REQUIRED));
        form.select_team("ENG");
        answer_reads(&mut form);
        form.select_filter(Filter::Project, Some("Shelbi"));
        form.select_filter(Filter::Label, Some("shelbi"));
        form.select_filter(Filter::Assignee, Some("me"));
        assert_eq!(
            form.scope_line().as_deref(),
            Some("Shelbi shows and creates issues in ENG, project Shelbi, labeled shelbi, assigned to you.")
        );
        let req = form.submit(false).unwrap();
        let TrackerChoice::Linear(conn) = &req.choice else { panic!("{req:?}") };
        assert_eq!((conn.team.as_str(), conn.assignee.as_deref()), ("ENG", Some("me")));

        form.connected(Ok(connected(IssueTrackerBackend::Linear)));
        assert!(!form.dirty(), "the section shows the new connection");
        assert_eq!(form.primary_label(), SAVE);
        assert_eq!(
            form.status_line().as_deref(),
            Some("Connected: Linear: ENG (Engineering). Statuses synced: 6 statuses from the Linear team.")
        );
    }

    #[test]
    fn a_refused_switch_says_how_to_migrate_and_offers_to_force_it() {
        let mut form = TrackerForm::default();
        form.load(&github("acme/app"));
        form.select(Tracker::Linear);
        answer_reads(&mut form);
        form.select_team("ENG");
        let req = form.submit(false).unwrap();
        assert!(!req.force);
        form.connected(Err(ConnectError::NotEmpty {
            from: IssueTrackerBackend::Github,
            to: IssueTrackerBackend::Linear,
            issues: 3,
            message: "project `app` has 3 issues on GitHub (acme/app); run `shelbi issue-store migrate --to linear`"
                .into(),
        }));
        assert!(form.error().unwrap().starts_with("Project `app` has 3 issues"));
        assert!(form.offers_force());
        assert_eq!(form.tracker(), Tracker::Linear, "the pick stays");
        let forced = form.submit(true).unwrap();
        assert!(forced.force);
        form.connected(Err(ConnectError::Preflight("no team ENG".into())));
        assert!(!form.offers_force(), "a failed preflight can't be forced");
        form.cancel();
        assert_eq!((form.tracker(), form.error()), (Tracker::Github, None));
    }

    #[test]
    fn a_linear_project_seeds_its_filters_and_a_filter_change_is_a_save() {
        let mut form = TrackerForm::default();
        let conn = LinearConnection { label: Some("shelbi".into()), ..LinearConnection::for_team("ENG") };
        form.load(&linear_cfg(conn));
        answer_reads(&mut form);
        assert_eq!(form.linear().filter(Filter::Label), Some("shelbi"));
        assert!(!form.dirty());
        form.select_filter(Filter::Label, None);
        assert!(form.dirty() && form.primary_enabled());
        assert_eq!(form.primary_label(), SAVE);
        form.select_filter(Filter::Label, Some("shelbi"));
        assert!(!form.dirty(), "back as it was");
    }
}
