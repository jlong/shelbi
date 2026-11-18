//! The Linear half of the Issues step (`shelbi init`, the TUI's Add project
//! dialog, the desktop's Add Project wizard) and of Settings' tracker page:
//! the API key, the team (required), and the optional project, label and
//! assignee filters, each picked from lists read from Linear.
//!
//! Like the rest of the new-project flow, [`LinearChoice`] never does IO.
//! It says which read is due ([`LinearChoice::due`], one
//! [`LinearRequest`] at a time; a host loops until it answers `None`), the
//! host runs it where the project lives
//! (`shelbi_orchestrator::project_create::linear::answer`, or the far
//! daemon's `AppRequest` for a project on another machine) off the UI
//! thread, and hands the [`LinearAnswer`] back
//! ([`LinearChoice::answer`]). An answer for a team or a connection the
//! user has since changed is dropped.
//!
//! The order a host sees:
//!
//! 1. [`LinearRequest::Teams`] as soon as Linear is chosen. It also proves
//!    the key: a missing key turns into [`KeyState::Missing`] (the view
//!    shows [`KEY_MISSING`] and a key field), a rejected one into
//!    [`KeyState::Rejected`]. A pasted key goes out as
//!    [`LinearRequest::Login`] ([`LinearChoice::submit_key`]), and the teams
//!    load again once it's stored.
//! 2. [`LinearRequest::TeamLists`] once a team is picked: its projects,
//!    labels and members for the three optional filters.
//! 3. [`LinearRequest::Preflight`] when Continue is pressed
//!    ([`LinearChoice::want_preflight`], which refuses a missing team with
//!    [`TEAM_REQUIRED`]): the key works, every filter resolves, and the key
//!    can create issues in the team. Continue moves on once the connection
//!    as chosen now has passed ([`LinearChoice::ready`]).
//! 4. [`LinearRequest::Viewer`] once nothing else is due and the key works
//!    but whose it is isn't known yet: the account row's name (Settings
//!    shows it beside "Change key…").

use serde::{Deserialize, Serialize};
use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig, LinearConnection};

/// The Linear card's title and description on the Issues step.
pub const LINEAR_TITLE: &str = "Linear";
pub const LINEAR_BODY: &str =
    "Tasks are Linear issues in one of your teams, optionally narrowed by project, label or assignee. Needs a Linear API key.";
/// The key field, its help, and what shows when this machine has no key.
pub const KEY_LABEL: &str = "Linear API key";
pub const KEY_HELP: &str = "Create one in Linear: Settings > Account > Security & access > Personal API keys.";
pub const KEY_MISSING: &str =
    "Shelbi has no Linear API key on this machine. Paste a personal API key, or run `shelbi tracker linear login`.";
/// The key field was submitted empty.
pub const KEY_EMPTY: &str = "Paste a Linear personal API key.";
/// The pickers' labels.
pub const TEAM_LABEL: &str = "Team";
pub const PROJECT_LABEL: &str = "Project";
pub const LABEL_LABEL: &str = "Label";
pub const ASSIGNEE_LABEL: &str = "Assignee";
/// The optional filters' help line.
pub const FILTERS_HELP: &str =
    "Optional. The board shows only the issues that match, and new issues get them too.";
/// A filter's "no filter" row.
pub const ANY: &str = "Any";
/// The assignee value that means the key's owner.
pub const ME: &str = "me";
/// Continue was pressed with no team.
pub const TEAM_REQUIRED: &str = "Choose the Linear team this project's issues live in.";
/// The account row's label, its button while a key works, and what it says
/// with no working key.
pub const ACCOUNT_LABEL: &str = "Linear account";
pub const CHANGE_KEY: &str = "Change key…";
pub const NOT_CONNECTED: &str = "Not connected. Paste a personal API key.";
/// The key field's button: check the pasted key with Linear and store it.
pub const VERIFY: &str = "Verify";
/// The key field's placeholder.
pub const KEY_PLACEHOLDER: &str = "lin_api_…";
/// A picker whose rows are being read.
pub const LOADING: &str = "Loading…";
/// The team picker before one is picked.
pub const CHOOSE_TEAM: &str = "Choose a team";

/// One of the optional filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Filter {
    Project,
    Label,
    Assignee,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::Project, Filter::Label, Filter::Assignee];

    /// The picker's label.
    pub fn label(self) -> &'static str {
        match self {
            Filter::Project => PROJECT_LABEL,
            Filter::Label => LABEL_LABEL,
            Filter::Assignee => ASSIGNEE_LABEL,
        }
    }

    /// The config key (`issue_tracker.linear.<key>`).
    pub fn key(self) -> &'static str {
        match self {
            Filter::Project => "project",
            Filter::Label => "label",
            Filter::Assignee => "assignee",
        }
    }
}

/// One row of a picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PickOption {
    /// The Linear id (empty for [`PickOption::any`] and [`PickOption::me`]).
    pub id: String,
    /// What the config records when it's picked: a team's key, a project's
    /// or label's name, a user's email, `me`. Empty: no filter.
    pub value: String,
    /// How the row reads.
    pub label: String,
    /// A second, quieter line (a team's name, a project's status, an email).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PickOption {
    /// A filter's first row: no filter.
    pub fn any() -> Self {
        PickOption { id: String::new(), value: String::new(), label: ANY.into(), detail: None }
    }

    /// The assignee picker's second row: the key's owner, whoever that is.
    pub fn me(viewer: Option<&str>) -> Self {
        PickOption {
            id: String::new(),
            value: ME.into(),
            label: "Me".into(),
            detail: viewer.map(str::to_string),
        }
    }

    /// `wanted` (a config value) names this row: its id, value, label or
    /// detail, ignoring case.
    pub fn matches(&self, wanted: &str) -> bool {
        let wanted = wanted.trim();
        !wanted.is_empty()
            && [Some(self.id.as_str()), Some(self.value.as_str()), Some(self.label.as_str()), self.detail.as_deref()]
                .into_iter()
                .flatten()
                .any(|k| !k.is_empty() && k.eq_ignore_ascii_case(wanted))
    }
}

/// A list read from Linear, and where its read is.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum Loadable<T> {
    /// Not asked for yet.
    #[default]
    Idle,
    Loading,
    Loaded(T),
    /// The read failed with this message ([`LinearChoice::retry`] asks
    /// again).
    Failed(String),
}

impl<T> Loadable<T> {
    pub fn loaded(&self) -> Option<&T> {
        match self {
            Loadable::Loaded(t) => Some(t),
            _ => None,
        }
    }

    pub fn is_loading(&self) -> bool {
        matches!(self, Loadable::Loading)
    }

    pub fn error(&self) -> Option<&str> {
        match self {
            Loadable::Failed(e) => Some(e),
            _ => None,
        }
    }

    fn from_result(result: Result<T, String>) -> Self {
        match result {
            Ok(t) => Loadable::Loaded(t),
            Err(e) => Loadable::Failed(e),
        }
    }
}

/// What's known about this machine's Linear API key.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum KeyState {
    /// Not checked yet (the first teams read checks it).
    #[default]
    Unknown,
    /// No key is configured: the view shows [`KEY_MISSING`] and the key
    /// field.
    Missing,
    /// Linear refused the key (or a pasted one), with why.
    Rejected(String),
    /// The key works; `viewer` is whose it is, once a login said so.
    Valid { viewer: Option<String> },
}

/// A pasted API key on its way to be checked and stored. Its `Debug` never
/// shows the key.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKey(String);

impl ApiKey {
    pub fn new(key: impl Into<String>) -> Self {
        ApiKey(key.into())
    }

    /// The key itself, for the one call that stores it.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApiKey(<redacted>)")
    }
}

/// A read (or the key's login) the host runs for a [`LinearChoice`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearRequest {
    /// The teams the key can see (and so, whether there is a working key).
    Teams,
    /// Check a pasted key against Linear and store it (the keychain).
    Login(ApiKey),
    /// A team's projects, labels and members, for the filters.
    TeamLists { team_id: String },
    /// The full check before a connection is written
    /// (`project_create::linear::linear_preflight`).
    Preflight(LinearConnection),
    /// Whose the working key is (the account row), once the teams proved
    /// it works.
    Viewer,
}

/// Why a read failed, as far as the key is concerned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearFailure {
    /// No key is configured on this machine.
    MissingKey(String),
    /// Linear refused the key.
    Rejected(String),
    /// Anything else (network, rate limit, a Linear error).
    Other(String),
}

impl LinearFailure {
    pub fn message(&self) -> &str {
        match self {
            LinearFailure::MissingKey(m) | LinearFailure::Rejected(m) | LinearFailure::Other(m) => m,
        }
    }
}

/// The host's answer to a [`LinearRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinearAnswer {
    Teams(Result<Vec<PickOption>, LinearFailure>),
    /// The stored key's owner, as a label.
    Login(Result<String, LinearFailure>),
    TeamLists {
        team_id: String,
        projects: Result<Vec<PickOption>, String>,
        labels: Result<Vec<PickOption>, String>,
        users: Result<Vec<PickOption>, String>,
    },
    /// `connection` passed (with a one-line summary of what it resolved to)
    /// or failed with the fix to show.
    Preflight { connection: LinearConnection, result: Result<String, String> },
    /// The key's owner, as a label (`Ada Lovelace <ada@example.com>`).
    Viewer(Result<String, LinearFailure>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Login {
    Wanted(ApiKey),
    Running,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Preflight {
    Wanted,
    Running(LinearConnection),
}

/// One team's filter lists.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Lists {
    team_id: String,
    projects: Loadable<Vec<PickOption>>,
    labels: Loadable<Vec<PickOption>>,
    users: Loadable<Vec<PickOption>>,
}

/// The Linear choice: the key's state, the connection as picked, the lists
/// the pickers show, and whether the connection passed the preflight.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LinearChoice {
    key: KeyState,
    key_input: String,
    login: Option<Login>,
    teams: Loadable<Vec<PickOption>>,
    /// The connection as picked (a team's key, filters as written).
    connection: LinearConnection,
    lists: Option<Lists>,
    preflight: Option<Preflight>,
    /// The connection that last passed the preflight, and its summary.
    verified: Option<(LinearConnection, String)>,
    /// Why Continue was refused, or the last preflight failed.
    error: Option<String>,
    /// The key's owner was asked for ([`LinearRequest::Viewer`]).
    viewer_asked: bool,
    /// "Change key…" opened the key field over a working key.
    changing_key: bool,
}

impl LinearChoice {
    /// A choice seeded with an existing connection (Settings, or a flag).
    pub fn with_connection(connection: LinearConnection) -> Self {
        LinearChoice { connection, ..LinearChoice::default() }
    }

    pub fn key_state(&self) -> &KeyState {
        &self.key
    }

    /// The key field shows: there's no working key yet, or "Change key…"
    /// opened it.
    pub fn needs_key(&self) -> bool {
        self.changing_key || matches!(self.key, KeyState::Missing | KeyState::Rejected(_))
    }

    /// The key field shows over a working key ("Change key…"), so it can be
    /// closed again ([`Self::cancel_change_key`]).
    pub fn changing_key(&self) -> bool {
        self.changing_key
    }

    /// "Change key…": show the key field to paste another key. The working
    /// key stays in use until a new one is stored.
    pub fn change_key(&mut self) {
        if matches!(self.key, KeyState::Valid { .. }) {
            self.changing_key = true;
            self.error = None;
        }
    }

    /// Close the key field "Change key…" opened, keeping the working key.
    pub fn cancel_change_key(&mut self) {
        if self.changing_key && self.login.is_none() {
            self.changing_key = false;
            self.key_input.clear();
            self.error = None;
        }
    }

    /// The account row: whose the working key is, `None` while there's no
    /// working key ([`NOT_CONNECTED`] shows) or it hasn't been read yet.
    pub fn account(&self) -> Option<&str> {
        match &self.key {
            KeyState::Valid { viewer } => viewer.as_deref(),
            _ => None,
        }
    }

    /// Why the key was refused, while the key field shows for it.
    pub fn key_error(&self) -> Option<&str> {
        match &self.key {
            KeyState::Rejected(why) => Some(why),
            _ => None,
        }
    }

    /// One sentence saying what the connection as picked shows and creates:
    /// `Shelbi shows and creates issues in ENG, project Shelbi, labeled
    /// shelbi, assigned to you.` `None` until a team is picked.
    pub fn scope_line(&self) -> Option<String> {
        let team = self.connection.team_ref()?;
        let team = self.team_option().map(|t| t.label.clone()).unwrap_or_else(|| team.to_string());
        let shown = |filter: Filter| -> Option<String> {
            let wanted = self.filter(filter)?;
            Some(match self.filter_option(filter) {
                Some(row) if !row.value.is_empty() => row.label,
                _ => wanted.to_string(),
            })
        };
        let mut parts = vec![team];
        if let Some(project) = shown(Filter::Project) {
            parts.push(format!("project {project}"));
        }
        if let Some(label) = shown(Filter::Label) {
            parts.push(format!("labeled {label}"));
        }
        match self.filter(Filter::Assignee) {
            Some(a) if a.eq_ignore_ascii_case(ME) => parts.push("assigned to you".into()),
            Some(_) => parts.push(format!("assigned to {}", shown(Filter::Assignee).unwrap_or_default())),
            None => {}
        }
        Some(format!("Shelbi shows and creates issues in {}.", parts.join(", ")))
    }

    /// The key field's text.
    pub fn key_input(&self) -> &str {
        &self.key_input
    }

    /// A pasted key is being checked.
    pub fn logging_in(&self) -> bool {
        self.login.is_some()
    }

    pub fn teams(&self) -> &Loadable<Vec<PickOption>> {
        &self.teams
    }

    /// The team as picked (a key, name or id); empty until one is.
    pub fn team(&self) -> &str {
        &self.connection.team
    }

    /// The loaded team the pick names.
    pub fn team_option(&self) -> Option<&PickOption> {
        let wanted = self.connection.team_ref()?;
        self.teams.loaded()?.iter().find(|t| t.matches(wanted))
    }

    /// A filter as picked; `None` is no filter.
    pub fn filter(&self, filter: Filter) -> Option<&str> {
        match filter {
            Filter::Project => self.connection.project_ref(),
            Filter::Label => self.connection.label_ref(),
            Filter::Assignee => self.connection.assignee_ref(),
        }
    }

    /// A filter picker's rows: [`PickOption::any`] first (and
    /// [`PickOption::me`] second for the assignee), then the team's.
    /// [`Loadable::Idle`] until a team is picked.
    pub fn filter_options(&self, filter: Filter) -> Loadable<Vec<PickOption>> {
        let Some(lists) = self.current_lists() else {
            return Loadable::Idle;
        };
        let list = match filter {
            Filter::Project => &lists.projects,
            Filter::Label => &lists.labels,
            Filter::Assignee => &lists.users,
        };
        match list {
            Loadable::Loaded(rows) => {
                let mut out = vec![PickOption::any()];
                if filter == Filter::Assignee {
                    let viewer = match &self.key {
                        KeyState::Valid { viewer } => viewer.as_deref(),
                        _ => None,
                    };
                    out.push(PickOption::me(viewer));
                }
                out.extend(rows.iter().cloned());
                Loadable::Loaded(out)
            }
            Loadable::Idle => Loadable::Idle,
            Loadable::Loading => Loadable::Loading,
            Loadable::Failed(e) => Loadable::Failed(e.clone()),
        }
    }

    /// The row a filter's pick names, among its loaded rows.
    pub fn filter_option(&self, filter: Filter) -> Option<PickOption> {
        let wanted = self.filter(filter).unwrap_or("");
        let rows = self.filter_options(filter);
        let rows = rows.loaded()?;
        if wanted.is_empty() {
            return rows.first().cloned();
        }
        rows.iter().skip(1).find(|r| r.matches(wanted)).cloned()
    }

    /// Why Continue was refused, or why the connection didn't pass.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The preflight is due or running.
    pub fn preflighting(&self) -> bool {
        self.preflight.is_some()
    }

    /// A login or a preflight is under way: Continue waits.
    pub fn busy(&self) -> bool {
        self.logging_in() || self.preflighting()
    }

    /// The connection as picked, blanks dropped: what the config records.
    pub fn connection(&self) -> LinearConnection {
        let keep = |v: Option<&str>| v.map(str::to_string);
        LinearConnection {
            team: self.connection.team.trim().to_string(),
            project: keep(self.connection.project_ref()),
            label: keep(self.connection.label_ref()),
            assignee: keep(self.connection.assignee_ref()),
        }
    }

    /// The connection as picked now passed the preflight.
    pub fn ready(&self) -> bool {
        !self.preflighting() && self.verified.as_ref().is_some_and(|(c, _)| *c == self.connection())
    }

    /// What the passed connection resolved to (`ENG (Engineering) · label
    /// shelbi`), once [`Self::ready`].
    pub fn summary(&self) -> Option<&str> {
        self.ready().then(|| self.verified.as_ref().map(|(_, s)| s.as_str())).flatten()
    }

    /// The board config this choice writes.
    pub fn config(&self) -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Linear,
            linear: Some(self.connection()),
            ..IssueTrackerConfig::default()
        }
    }

    /// Pick the team (a row's value, or a typed key). The filters belong to
    /// the old team, so they're dropped (an assignee of `me` stays: it's
    /// nobody's in particular).
    pub fn select_team(&mut self, team: &str) {
        let team = team.trim();
        if team == self.connection.team {
            return;
        }
        self.connection.team = team.to_string();
        self.connection.project = None;
        self.connection.label = None;
        if self.connection.assignee_ref().is_some_and(|a| !a.eq_ignore_ascii_case(ME)) {
            self.connection.assignee = None;
        }
        self.error = None;
    }

    /// Pick a filter (a row's value; `None` or blank: no filter).
    pub fn select_filter(&mut self, filter: Filter, value: Option<&str>) {
        let value = value.map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        let slot = match filter {
            Filter::Project => &mut self.connection.project,
            Filter::Label => &mut self.connection.label,
            Filter::Assignee => &mut self.connection.assignee,
        };
        if *slot != value {
            *slot = value;
            self.error = None;
        }
    }

    /// The next (or previous) loaded team: an arrow key on the team
    /// picker. From no team, forward picks the first.
    pub fn step_team(&mut self, forward: bool) {
        let Some(rows) = self.teams.loaded() else { return };
        let at = self.team_option().and_then(|t| rows.iter().position(|r| r == t));
        if let Some(next) = step_index(rows.len(), at, forward) {
            let team = rows[next].value.clone();
            self.select_team(&team);
        }
    }

    /// The next (or previous) row of a filter's picker (Any first):
    /// an arrow key on it.
    pub fn step_filter(&mut self, filter: Filter, forward: bool) {
        let rows = self.filter_options(filter);
        let Some(rows) = rows.loaded() else { return };
        let at = self.filter_option(filter).and_then(|o| rows.iter().position(|r| *r == o));
        if let Some(next) = step_index(rows.len(), at, forward) {
            let value = rows[next].value.clone();
            self.select_filter(filter, Some(&value));
        }
    }

    /// The key field's text changed.
    pub fn set_key_input(&mut self, text: impl Into<String>) {
        self.key_input = text.into();
    }

    /// Check and store the key field's key (the key field's Enter, or
    /// Continue while it shows). False when there's nothing to send.
    pub fn submit_key(&mut self) -> bool {
        if self.login.is_some() {
            return false;
        }
        let key = self.key_input.trim();
        if key.is_empty() {
            self.error = Some(KEY_EMPTY.into());
            return false;
        }
        self.login = Some(Login::Wanted(ApiKey::new(key)));
        self.error = None;
        true
    }

    /// Continue: run the preflight on the connection as picked. Refuses a
    /// missing team with [`TEAM_REQUIRED`]. False when nothing was started.
    pub fn want_preflight(&mut self) -> bool {
        if self.busy() {
            return false;
        }
        if self.connection.team_ref().is_none() {
            self.error = Some(TEAM_REQUIRED.into());
            return false;
        }
        self.error = None;
        self.preflight = Some(Preflight::Wanted);
        true
    }

    /// Ask again for every list whose read failed.
    pub fn retry(&mut self) {
        if matches!(self.teams, Loadable::Failed(_)) && !self.needs_key() {
            self.teams = Loadable::Idle;
        }
        if let Some(lists) = &self.lists {
            if [&lists.projects, &lists.labels, &lists.users].iter().any(|l| l.error().is_some()) {
                self.lists = None;
            }
        }
    }

    /// The loaded team's id, when the pick names one.
    fn team_id(&self) -> Option<String> {
        self.team_option().map(|t| t.id.clone()).filter(|id| !id.is_empty())
    }

    fn current_lists(&self) -> Option<&Lists> {
        let id = self.team_id()?;
        self.lists.as_ref().filter(|l| l.team_id == id)
    }

    /// The next read the host should run, marked as running. `None` when
    /// nothing is due (or a login is under way: everything waits on it).
    pub fn due(&mut self) -> Option<LinearRequest> {
        match self.login.take() {
            Some(Login::Wanted(key)) => {
                self.login = Some(Login::Running);
                return Some(LinearRequest::Login(key));
            }
            Some(Login::Running) => {
                self.login = Some(Login::Running);
                return None;
            }
            None => {}
        }
        if self.teams == Loadable::Idle && !matches!(self.key, KeyState::Missing | KeyState::Rejected(_)) {
            self.teams = Loadable::Loading;
            return Some(LinearRequest::Teams);
        }

        if let Some(team_id) = self.team_id() {
            if self.lists.as_ref().is_none_or(|l| l.team_id != team_id) {
                self.lists = Some(Lists {
                    team_id: team_id.clone(),
                    projects: Loadable::Loading,
                    labels: Loadable::Loading,
                    users: Loadable::Loading,
                });
                return Some(LinearRequest::TeamLists { team_id });
            }
        }
        if self.preflight == Some(Preflight::Wanted) {
            let connection = self.connection();
            self.preflight = Some(Preflight::Running(connection.clone()));
            return Some(LinearRequest::Preflight(connection));
        }
        // Last: whose the key is, for the account row (nothing waits on it).
        if matches!(self.key, KeyState::Valid { viewer: None }) && !self.viewer_asked {
            self.viewer_asked = true;
            return Some(LinearRequest::Viewer);
        }
        None
    }

    /// The host's answer to a request [`Self::due`] gave it.
    pub fn answer(&mut self, answer: LinearAnswer) {
        match answer {
            LinearAnswer::Teams(result) => {
                if !self.teams.is_loading() {
                    return;
                }
                match result {
                    Ok(teams) => {
                        if !matches!(self.key, KeyState::Valid { .. }) {
                            self.key = KeyState::Valid { viewer: None };
                        }
                        // A typed team that names a loaded one reads as its key.
                        if let Some(team) = teams.iter().find(|t| t.matches(&self.connection.team)) {
                            self.connection.team = team.value.clone();
                        }
                        self.teams = Loadable::Loaded(teams);
                    }
                    Err(failure) => {
                        self.note_key_failure(&failure);
                        self.teams = Loadable::Failed(failure.message().to_string());
                    }
                }
            }
            LinearAnswer::Login(result) => {
                if self.login != Some(Login::Running) {
                    return;
                }
                self.login = None;
                match result {
                    Ok(viewer) => {
                        self.key = KeyState::Valid { viewer: Some(viewer) };
                        self.changing_key = false;
                        self.key_input.clear();
                        self.teams = Loadable::Idle;
                        self.error = None;
                    }
                    Err(failure) if self.changing_key => {
                        // The working key stays; the field says why the new
                        // one wasn't taken.
                        self.error = Some(failure.message().to_string());
                    }
                    Err(failure) => {
                        self.key = KeyState::Rejected(failure.message().to_string());
                    }
                }
            }
            LinearAnswer::Viewer(result) => match result {
                Ok(viewer) => {
                    if let KeyState::Valid { viewer: shown @ None } = &mut self.key {
                        *shown = Some(viewer);
                    }
                }
                Err(failure) => self.note_key_failure(&failure),
            },
            LinearAnswer::TeamLists { team_id, projects, labels, users } => {
                if let Some(lists) = self.lists.as_mut().filter(|l| l.team_id == team_id) {
                    lists.projects = Loadable::from_result(projects);
                    lists.labels = Loadable::from_result(labels);
                    lists.users = Loadable::from_result(users);
                }
            }
            LinearAnswer::Preflight { connection, result } => {
                if !matches!(&self.preflight, Some(Preflight::Running(c)) if *c == connection) {
                    return;
                }
                self.preflight = None;
                if connection != self.connection() {
                    // Changed while it ran: Continue checks the new one.
                    return;
                }
                match result {
                    Ok(summary) => {
                        self.verified = Some((connection, summary));
                        self.error = None;
                    }
                    Err(why) => {
                        self.verified = None;
                        self.error = Some(why);
                    }
                }
            }
        }
    }

    fn note_key_failure(&mut self, failure: &LinearFailure) {
        match failure {
            LinearFailure::MissingKey(_) => self.key = KeyState::Missing,
            LinearFailure::Rejected(why) => self.key = KeyState::Rejected(why.clone()),
            LinearFailure::Other(_) => {}
        }
    }
}

/// Canned Linear answers for the clients' tests and the desktop's visual
/// check: teams ENG (Engineering) and DES (Design), ENG's projects, labels
/// and members, and Ada Lovelace's key. Nothing here reads Linear.
#[doc(hidden)]
pub mod fixture {
    use super::*;

    fn row(id: &str, value: &str, label: &str, detail: Option<&str>) -> PickOption {
        PickOption { id: id.into(), value: value.into(), label: label.into(), detail: detail.map(str::to_string) }
    }

    /// Whose the key is.
    pub const VIEWER: &str = "Ada Lovelace <ada@example.com>";

    /// The answer to `request` with a working key.
    pub fn answer(request: LinearRequest) -> LinearAnswer {
        match request {
            LinearRequest::Teams => LinearAnswer::Teams(Ok(vec![
                row("team-eng", "ENG", "ENG", Some("Engineering")),
                row("team-des", "DES", "DES", Some("Design")),
            ])),
            LinearRequest::Viewer => LinearAnswer::Viewer(Ok(VIEWER.into())),
            LinearRequest::Login(_) => LinearAnswer::Login(Ok(VIEWER.into())),
            LinearRequest::TeamLists { team_id } => LinearAnswer::TeamLists {
                team_id,
                projects: Ok(vec![
                    row("proj-shelbi", "Shelbi", "Shelbi", Some("In Progress")),
                    row("proj-site", "Website", "Website", Some("Planned")),
                ]),
                labels: Ok(vec![row("label-shelbi", "shelbi", "shelbi", None), row("label-bug", "Bug", "Bug", None)]),
                users: Ok(vec![
                    row("user-ada", "ada@example.com", "Ada Lovelace", Some("ada@example.com")),
                    row("user-alan", "alan@example.com", "Alan Turing", Some("alan@example.com")),
                ]),
            },
            LinearRequest::Preflight(connection) => {
                let team = connection.team.clone();
                LinearAnswer::Preflight { connection, result: Ok(format!("{team} (Engineering)")) }
            }
        }
    }

    /// The answer to `request` on a machine with no key yet (a pasted key
    /// logs in, after which [`answer`] applies).
    pub fn no_key(request: LinearRequest) -> LinearAnswer {
        match request {
            LinearRequest::Teams => LinearAnswer::Teams(Err(LinearFailure::MissingKey(KEY_MISSING.into()))),
            other => answer(other),
        }
    }
}

/// The row an arrow moves to among `len`, from `at` (none: the first),
/// wrapping. `None` for an empty list.
fn step_index(len: usize, at: Option<usize>, forward: bool) -> Option<usize> {
    if len == 0 {
        return None;
    }
    Some(match at {
        None => 0,
        Some(i) if forward => (i + 1) % len,
        Some(i) => (i + len - 1) % len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opt(id: &str, value: &str, label: &str, detail: Option<&str>) -> PickOption {
        PickOption {
            id: id.into(),
            value: value.into(),
            label: label.into(),
            detail: detail.map(str::to_string),
        }
    }

    fn teams() -> Vec<PickOption> {
        vec![
            opt("team-des", "DES", "DES", Some("Design")),
            opt("team-eng", "ENG", "ENG", Some("Engineering")),
        ]
    }

    fn lists(team_id: &str) -> LinearAnswer {
        LinearAnswer::TeamLists {
            team_id: team_id.into(),
            projects: Ok(vec![opt("proj-shelbi", "Shelbi", "Shelbi", Some("In Progress"))]),
            labels: Ok(vec![opt("label-bug", "Bug", "Bug", None)]),
            users: Err("Linear rate limit reached".into()),
        }
    }

    #[test]
    fn teams_load_first_then_the_picked_teams_lists() {
        let mut c = LinearChoice::default();
        assert_eq!(c.due(), Some(LinearRequest::Teams));
        assert!(c.teams().is_loading());
        assert_eq!(c.due(), None, "one teams read at a time");
        assert_eq!(c.filter_options(Filter::Project), Loadable::Idle, "no team, no lists");

        c.answer(LinearAnswer::Teams(Ok(teams())));
        assert_eq!(c.key_state(), &KeyState::Valid { viewer: None });
        assert_eq!(c.teams().loaded().unwrap().len(), 2);
        assert_eq!(c.due(), Some(LinearRequest::Viewer), "whose the key is, for the account row");
        assert_eq!(c.due(), None, "nothing more until a team is picked");
        c.answer(LinearAnswer::Viewer(Ok("Ada <ada@example.com>".into())));
        assert_eq!(c.account(), Some("Ada <ada@example.com>"));

        c.select_team("eng");
        assert_eq!(c.team_option().unwrap().id, "team-eng");
        assert_eq!(c.due(), Some(LinearRequest::TeamLists { team_id: "team-eng".into() }));
        assert!(c.filter_options(Filter::Label).is_loading());
        c.answer(lists("team-eng"));

        let projects = c.filter_options(Filter::Project);
        let rows = projects.loaded().unwrap();
        assert_eq!(rows[0], PickOption::any());
        assert_eq!(rows[1].value, "Shelbi");
        assert_eq!(c.filter_options(Filter::Assignee).error(), Some("Linear rate limit reached"));

        c.select_filter(Filter::Project, Some("Shelbi"));
        assert_eq!(c.filter_option(Filter::Project).unwrap().id, "proj-shelbi");
        assert_eq!(c.filter_option(Filter::Label), Some(PickOption::any()));

        // A retry asks for the team's lists again.
        c.retry();
        assert_eq!(c.due(), Some(LinearRequest::TeamLists { team_id: "team-eng".into() }));
    }

    #[test]
    fn an_answer_for_another_team_is_dropped_and_a_new_team_drops_its_filters() {
        let mut c = LinearChoice::default();
        c.due();
        c.answer(LinearAnswer::Teams(Ok(teams())));
        c.select_team("ENG");
        c.due();
        c.select_filter(Filter::Assignee, Some("me"));
        c.select_filter(Filter::Label, Some("Bug"));
        c.select_team("DES");
        assert_eq!(c.filter(Filter::Label), None, "labels are per team");
        assert_eq!(c.filter(Filter::Assignee), Some("me"), "me is anyone's");
        assert_eq!(c.due(), Some(LinearRequest::TeamLists { team_id: "team-des".into() }));
        c.answer(lists("team-eng"));
        assert!(c.filter_options(Filter::Project).is_loading(), "ENG's lists don't land on DES");
        c.answer(lists("team-des"));
        let users = c.filter_options(Filter::Assignee);
        assert!(users.error().is_some());
    }

    #[test]
    fn a_missing_key_shows_the_key_field_and_a_login_reloads_the_teams() {
        let mut c = LinearChoice::default();
        assert_eq!(c.due(), Some(LinearRequest::Teams));
        c.answer(LinearAnswer::Teams(Err(LinearFailure::MissingKey("no key".into()))));
        assert_eq!(c.key_state(), &KeyState::Missing);
        assert!(c.needs_key());
        assert_eq!(c.teams().error(), Some("no key"));
        assert_eq!(c.due(), None, "no teams read without a key");

        assert!(!c.submit_key(), "an empty field sends nothing");
        assert_eq!(c.error(), Some(KEY_EMPTY));
        c.set_key_input("  lin_api_x  ");
        assert!(c.submit_key());
        let Some(LinearRequest::Login(key)) = c.due() else { panic!("login due") };
        assert_eq!(key.expose(), "lin_api_x");
        assert_eq!(format!("{key:?}"), "ApiKey(<redacted>)");
        assert!(c.logging_in() && c.busy());
        assert_eq!(c.due(), None, "everything waits on the login");

        c.answer(LinearAnswer::Login(Err(LinearFailure::Rejected("bad key".into()))));
        assert_eq!(c.key_state(), &KeyState::Rejected("bad key".into()));
        assert!(c.needs_key());

        assert!(c.submit_key());
        c.due();
        c.answer(LinearAnswer::Login(Ok("ada <ada@example.com>".into())));
        assert_eq!(c.key_state(), &KeyState::Valid { viewer: Some("ada <ada@example.com>".into()) });
        assert_eq!(c.key_input(), "");
        assert_eq!(c.due(), Some(LinearRequest::Teams), "the teams load with the new key");
    }

    #[test]
    fn continue_needs_a_team_then_a_passed_preflight_of_the_current_pick() {
        let mut c = LinearChoice::default();
        c.due();
        c.answer(LinearAnswer::Teams(Ok(teams())));
        assert!(!c.want_preflight());
        assert_eq!(c.error(), Some(TEAM_REQUIRED));
        assert!(!c.ready());

        c.select_team("ENG");
        assert_eq!(c.error(), None);
        c.due(); // the lists
        c.select_filter(Filter::Label, Some(" Bug "));
        assert!(c.want_preflight());
        let want = LinearConnection {
            team: "ENG".into(),
            label: Some("Bug".into()),
            ..LinearConnection::default()
        };
        assert_eq!(c.due(), Some(LinearRequest::Preflight(want.clone())));
        assert!(!c.want_preflight(), "one preflight at a time");

        c.answer(LinearAnswer::Preflight { connection: want.clone(), result: Err("no Linear label `Bug`".into()) });
        assert!(!c.ready());
        assert_eq!(c.error(), Some("no Linear label `Bug`"));

        assert!(c.want_preflight());
        c.due();
        c.answer(LinearAnswer::Preflight { connection: want.clone(), result: Ok("ENG (Engineering)".into()) });
        assert!(c.ready());
        assert_eq!(c.summary(), Some("ENG (Engineering)"));
        assert_eq!(c.config().linear, Some(want.clone()));
        c.config().validate().unwrap();

        c.select_filter(Filter::Label, None);
        assert!(!c.ready(), "a changed pick is checked again");

        // An answer for a pick changed while the preflight ran is dropped.
        assert!(c.want_preflight());
        c.due();
        c.select_filter(Filter::Project, Some("Shelbi"));
        c.answer(LinearAnswer::Preflight {
            connection: LinearConnection::for_team("ENG"),
            result: Ok("ENG".into()),
        });
        assert!(!c.preflighting() && !c.ready());
    }

    #[test]
    fn change_key_keeps_the_working_key_until_a_new_one_is_stored() {
        let mut c = LinearChoice::default();
        c.due();
        c.answer(LinearAnswer::Teams(Ok(teams())));
        assert!(!c.needs_key());
        c.change_key();
        assert!(c.needs_key() && c.changing_key());
        c.set_key_input("lin_api_bad");
        assert!(c.submit_key());
        assert!(matches!(c.due(), Some(LinearRequest::Login(_))));
        c.answer(LinearAnswer::Login(Err(LinearFailure::Rejected("bad key".into()))));
        assert_eq!(c.key_state(), &KeyState::Valid { viewer: None }, "the old key still works");
        assert_eq!(c.error(), Some("bad key"));
        c.cancel_change_key();
        assert!(!c.needs_key() && c.key_input().is_empty() && c.error().is_none());
    }

    #[test]
    fn arrows_step_through_a_pickers_rows() {
        let mut c = LinearChoice::default();
        c.due();
        c.answer(LinearAnswer::Teams(Ok(teams())));
        c.step_team(true);
        assert_eq!(c.team(), "DES", "from no team, the first");
        c.step_team(false);
        assert_eq!(c.team(), "ENG", "wrapping");
        c.due();
        c.answer(lists("team-eng"));
        c.step_filter(Filter::Project, true);
        assert_eq!(c.filter(Filter::Project), Some("Shelbi"));
        c.step_filter(Filter::Project, true);
        assert_eq!(c.filter(Filter::Project), None, "back to Any");
    }

    #[test]
    fn the_scope_line_names_the_picks() {
        let mut c = LinearChoice::default();
        assert_eq!(c.scope_line(), None, "no team, no scope");
        c.due();
        c.answer(LinearAnswer::Teams(Ok(teams())));
        c.select_team("ENG");
        assert_eq!(c.scope_line().as_deref(), Some("Shelbi shows and creates issues in ENG."));
        c.due();
        c.answer(LinearAnswer::TeamLists {
            team_id: "team-eng".into(),
            projects: Ok(vec![]),
            labels: Ok(vec![opt("label-bug", "Bug", "Bug", None)]),
            users: Ok(vec![opt("u1", "ada@example.com", "Ada", Some("ada@example.com"))]),
        });
        c.select_filter(Filter::Label, Some("Bug"));
        c.select_filter(Filter::Assignee, Some("ada@example.com"));
        assert_eq!(
            c.scope_line().as_deref(),
            Some("Shelbi shows and creates issues in ENG, labeled Bug, assigned to Ada.")
        );
    }

    #[test]
    fn a_seeded_connection_is_preselected_by_any_of_its_names() {
        let mut c = LinearChoice::with_connection(LinearConnection {
            team: "engineering".into(),
            assignee: Some("me".into()),
            ..LinearConnection::default()
        });
        c.due();
        c.answer(LinearAnswer::Teams(Ok(teams())));
        assert_eq!(c.team(), "ENG", "a typed name reads as the team's key");
        assert_eq!(c.due(), Some(LinearRequest::TeamLists { team_id: "team-eng".into() }));
        c.answer(lists("team-eng"));
        assert_eq!(c.filter(Filter::Assignee), Some("me"));
        assert!(!PickOption::any().matches(""), "a blank names nothing");
    }
}
