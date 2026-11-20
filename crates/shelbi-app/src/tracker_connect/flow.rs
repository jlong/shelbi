//! The "Connect to an issue tracker" flow (`tracker-connect`, Figma "Issue
//! tracker · Connection flow" 2753:11686), toolkit-free and without IO, like
//! [`crate::new_project`] and [`crate::linear_setup`]. The desktop app opens
//! it from the Issues header's ⋮ menu and the palette's Connect to Issue
//! Tracker; it paints [`TrackerFlow`] and runs what [`TrackerFlow::due`] asks
//! for through its [`crate::settings::tracker::TrackerService`] (this
//! machine's, or the far daemon's for a project on another one).
//!
//! ```text
//!  Chooser ──Connect──▶ Choose project ──▶ Agent rules ──▶ Review import ──▶ Importing
//!  (no counter)          Step 1 of 4        Step 2 of 4     Step 3 of 4       Step 4 of 4
//!                ◀──Back──            ◀──Back──        ◀──Back──
//! ```
//!
//! - **Chooser** (01-select-tracker / 02a-connected): one card per tracker,
//!   GitHub Issues and Linear (no Jira). A card's Connect picks its tracker
//!   and moves on. With a tracker connected, its card shows Connected and
//!   the others' Connect is disabled: **one external integration per
//!   project**.
//! - **Choose project** (03-github-choose / 03a-linear-choose): GitHub's
//!   organization and repository through [`GithubChoice`], or Linear's
//!   workspace (read-only), team (the only required field) and project
//!   through the shared [`LinearChoice`]. Continue is offered once the
//!   choice can be connected; it runs the connection's own preflight
//!   ([`FlowRequest::Github`] / [`FlowRequest::Linear`]) and moves on when
//!   that passes, or shows why it didn't.
//! - **Agent rules** (04-agent-rules): the project's [`AgentRules`]
//!   (`issue_tracker.agent_rules`), written with the connection; `tc-import`
//!   draws the toggles.
//! - **Review import** (06-review-import): a placeholder for `tc-import`'s
//!   preview. Its primary button starts the connection.
//! - **Importing** (07-importing): the connection runs
//!   ([`crate::tracker_connect::ConnectTracker`], answered with
//!   [`TrackerFlow::connected`]); `tc-import` adds the import's progress.
//!   A refusal (the local board has issues) offers Switch anyway, as
//!   Settings does.

use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig};

use super::{ConnectError, ConnectTracker, Connected, TrackerChoice};
use crate::github_setup::{GithubAnswer, GithubChoice, GithubRequest, REPO_REQUIRED};
use crate::linear_setup::{LinearAnswer, LinearChoice, LinearRequest, TEAM_REQUIRED};
use crate::new_project::{github_config, Tracker};

/// The chooser's heading, the line under it, and the note under the cards.
pub const CHOOSER_HEADING: &str = "Connect to an Issue tracker";
pub const CHOOSER_SUBHEADING: &str = "Bring external issues into Shelbi and keep work in sync.";
pub const ONE_AT_A_TIME: &str = "Only one issue tracker can be connected at a time.";
/// The cards' copy. (The Figma GitHub card's "a Linear team" is a slip.)
pub const GITHUB_TITLE: &str = "GitHub Issues";
pub const GITHUB_BODY: &str = "Sync issues from a GitHub repository.";
pub const LINEAR_TITLE: &str = "Linear";
pub const LINEAR_BODY: &str = "Sync issues from a Linear team.";
/// The Choose project step's note under its fields.
pub const ONLY_PROJECT: &str = "This will be the only issue tracker project connected to this Shelbi project.";
/// The Choose project step's account line (`✓ Account connected · Change
/// account`).
pub const ACCOUNT_CONNECTED: &str = "Account connected";
pub const CHANGE_ACCOUNT: &str = "Change account";
pub const RETRY: &str = "Retry";
/// Linear's Workspace field, and its Project picker's first row.
pub const WORKSPACE_LABEL: &str = "Workspace";
pub const ANY_PROJECT: &str = "Any project";
/// Where Linear's personal API keys are made.
pub const LINEAR_KEYS_URL: &str = "https://linear.app/settings/account/security";
pub const LINEAR_KEYS_LINK: &str = "Open Linear's API key settings";
/// The primary button while the choice's preflight runs.
pub const CHECKING: &str = "Checking…";
/// The buttons.
pub const CONNECT: &str = "Connect";
pub const CONNECTED: &str = "Connected";
pub const CONTINUE: &str = "Continue";
pub const BACK: &str = "Back";
pub const CLOSE: &str = "Close";
pub const IMPORT: &str = "Import issues";
pub const VIEW_BOARD: &str = "View board";
pub const SWITCH_ANYWAY: &str = "Switch anyway";
pub const TRY_AGAIN: &str = "Try again";
/// The Importing step's lines while the connection runs and once it's saved.
pub const CONNECTING: &str = "Saving the connection…";
pub const CONNECTION_SAVED: &str = "Connection saved";
pub const STATUSES_APPLIED: &str = "Status mappings applied";
pub const BACKGROUND_NOTE: &str = "You can close this window. Import and future sync continue in the background.";
/// The menu item and palette command that open the flow.
pub const MENU_TITLE: &str = "Connect to issue tracker…";

/// The trackers the chooser offers, in its order.
pub const TRACKERS: [Tracker; 2] = [Tracker::Github, Tracker::Linear];

/// The steps the counter counts (every step after the chooser).
pub const COUNTED_STEPS: usize = 4;

/// Where the flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Chooser,
    ChooseProject,
    AgentRules,
    ReviewImport,
    Importing,
}

impl Step {
    /// The step's number in "Step N of 4"; the chooser has none.
    pub fn number(self) -> Option<usize> {
        match self {
            Step::Chooser => None,
            Step::ChooseProject => Some(1),
            Step::AgentRules => Some(2),
            Step::ReviewImport => Some(3),
            Step::Importing => Some(4),
        }
    }
}

/// What a chooser card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    /// Its Connect button.
    Connect,
    /// The project is connected to it: the green badge, no button.
    Connected,
    /// Another tracker is connected: Connect, disabled.
    Disabled,
}

/// One chooser card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Card {
    pub tracker: Tracker,
    pub title: &'static str,
    pub body: &'static str,
    pub state: CardState,
}

/// The card copy for `tracker`.
pub fn card_copy(tracker: Tracker) -> (&'static str, &'static str) {
    match tracker {
        Tracker::Github => (GITHUB_TITLE, GITHUB_BODY),
        Tracker::Linear => (LINEAR_TITLE, LINEAR_BODY),
        Tracker::FileSystem => (crate::new_project::FILE_SYSTEM_TITLE, crate::new_project::FILE_SYSTEM_BODY),
    }
}

/// The tracker's short name in the steps' copy ("issues created in Linear").
pub fn tracker_name(tracker: Tracker) -> &'static str {
    match tracker {
        Tracker::Github => "GitHub",
        Tracker::Linear => "Linear",
        Tracker::FileSystem => "Shelbi",
    }
}

/// The agent rules for the tracker's issues (04-agent-rules): the project's
/// `issue_tracker.agent_rules`, written with the connection.
pub use shelbi_core::AgentRules;

/// The rules the step starts with on a project that has no tracker yet: the
/// frame's (both on). A project already on a tracker starts with its own.
pub const DEFAULT_RULES: AgentRules = AgentRules { require_review: true, assigned_only: true };

/// The connection's progress on the Importing step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Apply {
    /// Not started (any step before Importing).
    Idle,
    /// Asked for: [`TrackerFlow::due`] hands it to the host next (`force`
    /// switches even though the current board has issues).
    Wanted { force: bool },
    /// The host is running it.
    Running,
    /// Written.
    Done(Connected),
    /// Refused; nothing was written.
    Refused(ConnectError),
}

/// Blocking work the host runs off the UI thread, answering with
/// [`TrackerFlow::linear_answer`] or [`TrackerFlow::connected`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlowRequest {
    /// A Linear read for the Choose project step's pickers (or its
    /// preflight).
    Linear(LinearRequest),
    /// A GitHub read for the Choose project step's pickers (or its
    /// preflight).
    Github(GithubRequest),
    /// Write the connection (`apply_tracker_connection`, where the project
    /// lives).
    Connect(ConnectTracker),
}

/// The footer's left button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Secondary {
    Back,
    Close,
}

/// What the primary button asks of the host beyond the flow's own change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing (the flow moved, or couldn't).
    None,
    /// Close the flow and show the project's board.
    ViewBoard,
}

/// The flow's state.
#[derive(Debug, Clone)]
pub struct TrackerFlow {
    step: Step,
    /// The project's `issue_tracker:` as read; `None` until it is.
    current: Option<IssueTrackerConfig>,
    /// The tracker a card's Connect picked.
    tracker: Option<Tracker>,
    /// GitHub's organization and repository.
    github: GithubChoice,
    linear: LinearChoice,
    /// Continue was pressed on Choose project: the step moves on when the
    /// preflight it started passes.
    advancing: bool,
    rules: AgentRules,
    apply: Apply,
    /// Why Continue was refused.
    error: Option<String>,
}

impl Default for TrackerFlow {
    fn default() -> Self {
        TrackerFlow {
            step: Step::Chooser,
            current: None,
            tracker: None,
            github: GithubChoice::default(),
            linear: LinearChoice::default(),
            advancing: false,
            rules: DEFAULT_RULES,
            apply: Apply::Idle,
            error: None,
        }
    }
}

impl TrackerFlow {
    pub fn new() -> Self {
        Self::default()
    }

    /// The project's connection was read (or read again).
    pub fn load(&mut self, cfg: &IssueTrackerConfig) {
        // A project already on a tracker starts the rules step from its own
        // rules; until the user reaches the step, a reread follows them.
        if cfg.backend.is_remote() && self.step == Step::Chooser {
            self.rules = cfg.agent_rules;
        }
        self.current = Some(cfg.clone());
    }

    /// Whether the connection has been read: until then no card connects.
    pub fn loaded(&self) -> bool {
        self.current.is_some()
    }

    pub fn current(&self) -> Option<&IssueTrackerConfig> {
        self.current.as_ref()
    }

    /// The external tracker the project is connected to, as a chooser card.
    pub fn connected_tracker(&self) -> Option<Tracker> {
        match self.current.as_ref()?.backend {
            IssueTrackerBackend::Github => Some(Tracker::Github),
            IssueTrackerBackend::Linear => Some(Tracker::Linear),
            IssueTrackerBackend::FileSystem | IssueTrackerBackend::Jira => None,
        }
    }

    /// The project has an external tracker (any but the local board): the
    /// chooser connects nothing until it's disconnected (in Settings).
    pub fn has_connection(&self) -> bool {
        self.current.as_ref().is_some_and(|c| c.backend != IssueTrackerBackend::FileSystem)
    }

    pub fn step(&self) -> Step {
        self.step
    }

    /// The tracker picked on the chooser.
    pub fn tracker(&self) -> Option<Tracker> {
        self.tracker
    }

    /// The chooser's cards.
    pub fn cards(&self) -> Vec<Card> {
        let connected = self.connected_tracker();
        let any = self.has_connection();
        TRACKERS
            .into_iter()
            .map(|tracker| {
                let (title, body) = card_copy(tracker);
                let state = match () {
                    _ if connected == Some(tracker) => CardState::Connected,
                    _ if any || !self.loaded() => CardState::Disabled,
                    _ => CardState::Connect,
                };
                Card { tracker, title, body, state }
            })
            .collect()
    }

    /// A card's Connect: pick `tracker` and move to its Choose project step.
    /// False when the card can't connect (another tracker is connected, the
    /// connection isn't read yet, or the flow is past the chooser).
    pub fn connect(&mut self, tracker: Tracker) -> bool {
        let offered = self.cards().iter().any(|c| c.tracker == tracker && c.state == CardState::Connect);
        if self.step != Step::Chooser || !offered {
            return false;
        }
        self.tracker = Some(tracker);
        self.step = Step::ChooseProject;
        self.error = None;
        if tracker == Tracker::Linear {
            // The step shows the key's workspace.
            self.linear.want_workspace();
        }
        true
    }

    /// "Step N of 4", on every step after the chooser.
    pub fn counter(&self) -> Option<String> {
        self.step.number().map(|n| format!("Step {n} of {COUNTED_STEPS}"))
    }

    /// The step's heading.
    pub fn heading(&self) -> String {
        let name = tracker_name(self.tracker.unwrap_or(Tracker::Github));
        match self.step {
            Step::Chooser => CHOOSER_HEADING.into(),
            Step::ChooseProject => format!("Choose your {name} project"),
            Step::AgentRules => format!("How should agents handle issues created in {name}?"),
            Step::ReviewImport => "Review your first import".into(),
            Step::Importing => "Importing your issues".into(),
        }
    }

    /// The line under the heading.
    pub fn subheading(&self) -> String {
        let name = tracker_name(self.tracker.unwrap_or(Tracker::Github));
        match self.step {
            Step::Chooser => CHOOSER_SUBHEADING.into(),
            Step::ChooseProject => match self.tracker {
                Some(Tracker::Linear) => "Select the team whose issues you want to work on in Shelbi.".into(),
                _ => "Connect the project whose issues you want to work on in Shelbi.".into(),
            },
            Step::AgentRules => format!("These rules apply only to new issues imported from {name}."),
            Step::ReviewImport => {
                format!("Import from {}. Your existing Shelbi tasks stay on this board.", self.source_name())
            }
            Step::Importing => format!("Connecting {} and preparing your board.", self.source_name()),
        }
    }

    /// What the connection imports from, as the copy names it: the GitHub
    /// repository, or the Linear project (else its team).
    pub fn source_name(&self) -> String {
        match self.tracker {
            Some(Tracker::Github) => self.github.repo().to_string(),
            Some(Tracker::Linear) => {
                let conn = self.linear.connection();
                conn.project.clone().filter(|p| !p.trim().is_empty()).unwrap_or(conn.team)
            }
            _ => String::new(),
        }
    }

    // ---- Choose project -------------------------------------------------

    /// GitHub's repository (`owner/repo`).
    pub fn repo(&self) -> &str {
        self.github.repo()
    }

    pub fn set_repo(&mut self, repo: &str) {
        if self.step == Step::ChooseProject && repo.trim() != self.github.repo() {
            self.github.select_repo(repo);
            self.error = None;
        }
    }

    /// GitHub's choice (account, organization, repository).
    pub fn github(&self) -> &GithubChoice {
        &self.github
    }

    /// GitHub's choice, to pick an organization or repository, change the
    /// account or retry. Edits clear a refused Continue's reason.
    pub fn github_mut(&mut self) -> &mut GithubChoice {
        self.error = None;
        &mut self.github
    }

    /// Linear's choice (key, team, project).
    pub fn linear(&self) -> &LinearChoice {
        &self.linear
    }

    /// Linear's choice, to pick a team or project, or enter a key. Edits
    /// clear a refused Continue's reason.
    pub fn linear_mut(&mut self) -> &mut LinearChoice {
        self.error = None;
        &mut self.linear
    }

    /// The connection as picked, `None` while it can't be one (no tracker,
    /// an empty or malformed repo, a Linear choice with no team or key).
    pub fn wanted(&self) -> Option<TrackerChoice> {
        match self.tracker? {
            Tracker::Github => github_config(self.github.repo()).ok().map(|_| TrackerChoice::Github {
                repo: self.github.repo().to_string(),
            }),
            Tracker::Linear => {
                let conn = self.linear.connection();
                (!self.linear.needs_key() && conn.team_ref().is_some()).then_some(TrackerChoice::Linear(conn))
            }
            Tracker::FileSystem => None,
        }
    }

    // ---- Agent rules ------------------------------------------------------

    pub fn rules(&self) -> AgentRules {
        self.rules
    }

    pub fn set_rules(&mut self, rules: AgentRules) {
        if self.step == Step::AgentRules {
            self.rules = rules;
        }
    }

    /// The connected account the assigned-only rule names: the `gh` login
    /// for GitHub, the key's owner for Linear. `None` until known.
    pub fn account(&self) -> Option<&str> {
        match self.tracker? {
            Tracker::Github => self.github.login(),
            Tracker::Linear => self.linear.account(),
            Tracker::FileSystem => None,
        }
    }

    /// "Only work on issues assigned to me (<account>)".
    pub fn assigned_title(&self) -> String {
        crate::tracker_connect::rules_copy::assigned_title(self.account())
    }


    // ---- moving -----------------------------------------------------------

    /// Whether the step's Continue (its primary button) can be pressed.
    pub fn ready(&self) -> bool {
        match self.step {
            Step::Chooser => false,
            Step::ChooseProject => {
                let blocked = match self.tracker {
                    Some(Tracker::Linear) => self.linear.busy(),
                    Some(Tracker::Github) => self.github.needs_auth() || self.github.preflighting(),
                    _ => true,
                };
                !blocked && self.wanted().is_some()
            }
            Step::AgentRules | Step::ReviewImport => true,
            Step::Importing => match &self.apply {
                Apply::Running | Apply::Done(_) | Apply::Refused(_) => true,
                Apply::Idle | Apply::Wanted { .. } => false,
            },
        }
    }

    /// Why Continue was refused, or why the connection was.
    pub fn error(&self) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        if let Apply::Refused(e) = &self.apply {
            return Some(crate::view::title_case(e.message()));
        }
        match (self.step, self.tracker) {
            (Step::ChooseProject, Some(Tracker::Linear)) => self.linear.error().map(str::to_string),
            (Step::ChooseProject, Some(Tracker::Github)) => self.github.error().map(str::to_string),
            _ => None,
        }
    }

    /// Continue's preflight is due or running (the primary button says
    /// [`CHECKING`]).
    pub fn checking(&self) -> bool {
        self.step == Step::ChooseProject
            && match self.tracker {
                Some(Tracker::Linear) => self.linear.preflighting(),
                Some(Tracker::Github) => self.github.preflighting(),
                _ => false,
            }
    }

    /// The choice as picked passed its preflight.
    fn verified(&self) -> bool {
        match self.tracker {
            Some(Tracker::Linear) => self.linear.ready(),
            Some(Tracker::Github) => self.github.ready(),
            _ => false,
        }
    }

    /// Start the choice's preflight (Continue on Choose project).
    fn want_preflight(&mut self) -> bool {
        match self.tracker {
            Some(Tracker::Linear) => self.linear.want_preflight(),
            Some(Tracker::Github) => self.github.want_preflight(),
            _ => false,
        }
    }

    /// A preflight Continue started finished: move on if it passed (its
    /// failure shows as the step's error).
    fn finish_advance(&mut self) {
        if !self.advancing || self.checking() {
            return;
        }
        self.advancing = false;
        if self.step == Step::ChooseProject && self.verified() && self.wanted().is_some() {
            self.error = None;
            self.step = Step::AgentRules;
        }
    }

    /// Continue: the next step, once this one is ready. False (with
    /// [`Self::error`] saying why, where there's a why) when it isn't. On
    /// Choose project it moves on only once the choice has passed its
    /// preflight: until then it starts the preflight ([`Self::checking`])
    /// and the step moves on when that passes. From Review import it starts
    /// the connection.
    pub fn advance(&mut self) -> bool {
        if !self.ready() {
            if self.step == Step::ChooseProject && !self.checking() {
                self.error = match self.tracker {
                    Some(Tracker::Github) if self.github.needs_auth() => None,
                    Some(Tracker::Github) if self.github.repo().is_empty() => Some(REPO_REQUIRED.into()),
                    Some(Tracker::Github) => github_config(self.github.repo()).err(),
                    Some(Tracker::Linear) if !self.linear.needs_key() && !self.linear.busy() => {
                        Some(TEAM_REQUIRED.into())
                    }
                    _ => None,
                };
            }
            return false;
        }
        self.error = None;
        if self.step == Step::ChooseProject && !self.verified() {
            self.advancing = self.want_preflight();
            return false;
        }
        self.step = match self.step {
            Step::ChooseProject => Step::AgentRules,
            Step::AgentRules => Step::ReviewImport,
            Step::ReviewImport => {
                self.apply = Apply::Wanted { force: false };
                Step::Importing
            }
            Step::Chooser | Step::Importing => return false,
        };
        true
    }

    /// Whether Back can be pressed: on every counted step but Importing,
    /// which has it only after a refusal (to change the choice).
    pub fn can_back(&self) -> bool {
        match self.step {
            Step::Chooser => false,
            Step::ChooseProject | Step::AgentRules | Step::ReviewImport => true,
            Step::Importing => matches!(self.apply, Apply::Refused(_)),
        }
    }

    /// Back: the step before, its values kept.
    pub fn back(&mut self) -> bool {
        if !self.can_back() {
            return false;
        }
        self.error = None;
        // A preflight still running doesn't carry the step on.
        self.advancing = false;
        self.step = match self.step {
            Step::ChooseProject => Step::Chooser,
            Step::AgentRules => Step::ChooseProject,
            Step::ReviewImport => Step::AgentRules,
            Step::Importing => {
                self.apply = Apply::Idle;
                Step::ReviewImport
            }
            Step::Chooser => return false,
        };
        true
    }

    /// The footer's left button: Back on the counted steps, Close once the
    /// connection is under way (closing leaves it running).
    pub fn secondary(&self) -> Option<Secondary> {
        match self.step {
            Step::Chooser => None,
            Step::Importing if !self.can_back() => Some(Secondary::Close),
            _ => Some(Secondary::Back),
        }
    }

    /// The primary button's label; the chooser has none (its cards connect).
    pub fn primary_label(&self) -> Option<&'static str> {
        match self.step {
            Step::Chooser => None,
            Step::ChooseProject if self.checking() => Some(CHECKING),
            Step::ChooseProject | Step::AgentRules => Some(CONTINUE),
            Step::ReviewImport => Some(IMPORT),
            Step::Importing => Some(match &self.apply {
                Apply::Refused(_) if self.offers_force() => SWITCH_ANYWAY,
                Apply::Refused(_) => TRY_AGAIN,
                _ => VIEW_BOARD,
            }),
        }
    }

    /// The primary button: Continue on the steps before Importing; there,
    /// View board, or after a refusal Switch anyway / Try again.
    pub fn primary(&mut self) -> Action {
        if self.step != Step::Importing {
            self.advance();
            return Action::None;
        }
        match &self.apply {
            Apply::Running | Apply::Done(_) => Action::ViewBoard,
            Apply::Refused(_) => {
                self.apply = Apply::Wanted { force: self.offers_force() };
                Action::None
            }
            Apply::Idle | Apply::Wanted { .. } => Action::None,
        }
    }

    // ---- the work ---------------------------------------------------------

    /// The next blocking request, marked running: the host runs it off the
    /// UI thread and answers. Call until `None`.
    pub fn due(&mut self) -> Option<FlowRequest> {
        if let Apply::Wanted { force } = self.apply {
            let Some(choice) = self.wanted() else {
                // The choice can't be connected (it was changed under a
                // refusal): back to where it's picked.
                self.apply = Apply::Idle;
                self.step = Step::ChooseProject;
                return None;
            };
            self.apply = Apply::Running;
            return Some(FlowRequest::Connect(ConnectTracker::new(choice, force).with_rules(self.rules)));
        }
        if self.step == Step::ChooseProject {
            match self.tracker {
                Some(Tracker::Linear) => return self.linear.due().map(FlowRequest::Linear),
                Some(Tracker::Github) => return self.github.due().map(FlowRequest::Github),
                _ => {}
            }
        }
        None
    }

    /// The answer to a [`FlowRequest::Linear`].
    pub fn linear_answer(&mut self, answer: LinearAnswer) {
        self.linear.answer(answer);
        self.finish_advance();
    }

    /// The answer to a [`FlowRequest::Github`].
    pub fn github_answer(&mut self, answer: GithubAnswer) {
        self.github.answer(answer);
        self.finish_advance();
    }

    /// The answer to a [`FlowRequest::Connect`]. Written: the flow holds the
    /// new connection as the project's.
    pub fn connected(&mut self, result: Result<Connected, ConnectError>) {
        if self.apply != Apply::Running {
            return;
        }
        match result {
            Ok(done) => {
                if let (Some(current), Some(wanted)) = (&self.current, self.wanted()) {
                    self.current = Some(ConnectTracker::new(wanted, false).with_rules(self.rules).config(current));
                }
                self.apply = Apply::Done(done);
            }
            Err(e) => self.apply = Apply::Refused(e),
        }
    }

    /// The connection's progress.
    pub fn apply(&self) -> &Apply {
        &self.apply
    }

    /// The connection that was written.
    pub fn finished(&self) -> Option<&Connected> {
        match &self.apply {
            Apply::Done(done) => Some(done),
            _ => None,
        }
    }

    /// The refusal can be overridden: the current board has issues (or
    /// couldn't be counted), and Switch anyway leaves them behind.
    pub fn offers_force(&self) -> bool {
        matches!(self.apply, Apply::Refused(ConnectError::NotEmpty { .. } | ConnectError::Unverified { .. }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github_setup::{fixture as github_fixture, GithubFailure};
    use crate::linear_setup::fixture;
    use crate::tracker_connect::StatusSync;
    use shelbi_core::GithubConnection;

    fn local() -> IssueTrackerConfig {
        IssueTrackerConfig::default()
    }

    fn github(repo: &str) -> IssueTrackerConfig {
        IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(GithubConnection { repo: repo.into() }),
            ..IssueTrackerConfig::default()
        }
    }

    fn connected() -> Connected {
        Connected {
            backend: IssueTrackerBackend::Github,
            previous: IssueTrackerBackend::FileSystem,
            file: "app.yaml".into(),
            summary: "GitHub acme/app".into(),
            statuses: StatusSync::NotNeeded,
        }
    }

    /// Run the flow's reads with the fixtures' answers.
    fn answer_reads(flow: &mut TrackerFlow) {
        while let Some(req) = flow.due() {
            match req {
                FlowRequest::Linear(req) => flow.linear_answer(fixture::answer(req)),
                FlowRequest::Github(req) => flow.github_answer(github_fixture::answer(req)),
                FlowRequest::Connect(c) => panic!("no connection yet: {c:?}"),
            }
        }
    }

    /// Continue, with the preflight it starts answered.
    fn continue_(flow: &mut TrackerFlow) -> bool {
        let before = flow.step();
        flow.advance();
        if before == Step::ChooseProject {
            answer_reads(flow);
        }
        flow.step() != before
    }

    fn at_review(repo: &str) -> TrackerFlow {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert!(flow.connect(Tracker::Github));
        flow.set_repo(repo);
        assert!(continue_(&mut flow) && continue_(&mut flow));
        assert_eq!(flow.step(), Step::ReviewImport);
        flow
    }

    #[test]
    fn the_rules_step_starts_from_the_frame_or_the_projects_and_names_the_account() {
        // No tracker yet: the frame's defaults (both on).
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert_eq!(flow.rules(), DEFAULT_RULES);
        assert!(flow.connect(Tracker::Github));
        assert_eq!(flow.assigned_title(), "Only work on issues assigned to me");
        // The account comes from the GitHub picker's own account read.
        answer_reads(&mut flow);
        assert_eq!(flow.assigned_title(), "Only work on issues assigned to me (octocat)");
        // The step's toggles are what Import writes.
        flow.set_repo("acme/app");
        assert!(continue_(&mut flow));
        assert_eq!(flow.step(), Step::AgentRules);
        let off = AgentRules { require_review: false, assigned_only: false };
        flow.set_rules(off);
        assert!(continue_(&mut flow));
        flow.primary();
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("the connection") };
        assert_eq!(req.rules, Some(off));

        // A project already on a tracker starts from its own rules.
        let mut flow = TrackerFlow::new();
        let mut cfg = IssueTrackerConfig {
            backend: IssueTrackerBackend::Github,
            github: Some(shelbi_core::GithubConnection { repo: "acme/app".into() }),
            ..IssueTrackerConfig::default()
        };
        cfg.agent_rules = off;
        flow.load(&cfg);
        assert_eq!(flow.rules(), off);
    }

    #[test]
    fn the_steps_run_in_order_and_back_keeps_the_values() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert_eq!((flow.step(), flow.counter()), (Step::Chooser, None));
        assert_eq!((flow.primary_label(), flow.secondary()), (None, None), "the cards connect");
        assert!(!flow.advance() && !flow.back());

        assert!(flow.connect(Tracker::Github));
        assert_eq!(flow.step(), Step::ChooseProject);
        assert_eq!(flow.heading(), "Choose your GitHub project");
        flow.set_repo("acme/app");
        let mut seen = vec![(flow.step(), flow.counter())];
        while flow.step() != Step::Importing {
            assert_eq!(flow.secondary(), Some(Secondary::Back));
            assert!(continue_(&mut flow), "{:?} continues", flow.step());
            seen.push((flow.step(), flow.counter()));
        }
        let counter = |n: usize| Some(format!("Step {n} of 4"));
        assert_eq!(
            seen,
            vec![
                (Step::ChooseProject, counter(1)),
                (Step::AgentRules, counter(2)),
                (Step::ReviewImport, counter(3)),
                (Step::Importing, counter(4)),
            ]
        );
        assert_eq!(flow.secondary(), Some(Secondary::Close), "the connection runs: no Back");
        assert!(!flow.back());
    }

    #[test]
    fn back_walks_to_the_chooser_with_the_choice_kept() {
        let mut flow = at_review("acme/app");
        assert_eq!(flow.subheading(), "Import from acme/app. Your existing Shelbi tasks stay on this board.");
        assert!(flow.back());
        assert_eq!(flow.step(), Step::AgentRules);
        assert_eq!(flow.heading(), "How should agents handle issues created in GitHub?");
        assert!(flow.back() && flow.back());
        assert_eq!(flow.step(), Step::Chooser);
        assert!(flow.connect(Tracker::Github));
        assert_eq!(flow.repo(), "acme/app", "the repo is kept");
        assert!(flow.ready());
    }

    #[test]
    fn continue_waits_until_the_project_is_chosen() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Github);
        assert!(!flow.ready());
        assert!(!flow.advance());
        assert_eq!(flow.step(), Step::ChooseProject);
        assert_eq!(flow.error().as_deref(), Some(REPO_REQUIRED));
        flow.set_repo("not a repo");
        assert!(!flow.advance());
        assert!(flow.error().unwrap().contains("owner/repo"), "{:?}", flow.error());
        flow.set_repo("acme/app");
        assert_eq!(flow.error(), None, "an edit clears it");
        assert!(continue_(&mut flow));

        // Linear: a working key and a team (the only required field).
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Linear);
        assert_eq!(flow.heading(), "Choose your Linear project");
        assert!(matches!(flow.due(), Some(FlowRequest::Linear(LinearRequest::Teams))));
        assert!(!flow.ready(), "the teams are loading");
        flow.linear_answer(fixture::answer(LinearRequest::Teams));
        answer_reads(&mut flow);
        assert!(!flow.advance());
        assert_eq!(flow.error().as_deref(), Some(TEAM_REQUIRED));
        flow.linear_mut().select_team("ENG");
        answer_reads(&mut flow);
        assert!(flow.ready());
        assert!(!flow.advance(), "Continue runs the preflight first");
        assert!(flow.checking());
        assert_eq!(flow.primary_label(), Some(CHECKING));
        assert!(!flow.ready(), "one preflight at a time");
        let Some(FlowRequest::Linear(req @ LinearRequest::Preflight(_))) = flow.due() else { panic!("the preflight") };
        flow.linear_answer(fixture::answer(req));
        assert_eq!(flow.step(), Step::AgentRules, "it passed: on to the next step");
        assert_eq!(flow.heading(), "How should agents handle issues created in Linear?");
        assert_eq!(flow.linear().workspace().loaded().map(String::as_str), Some(fixture::WORKSPACE));
        assert_eq!(flow.source_name(), "ENG", "no project picked: the team");
    }

    #[test]
    fn github_continue_runs_the_preflight_and_a_failure_stays() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Github);
        answer_reads(&mut flow);
        assert_eq!(flow.github().owner(), github_fixture::LOGIN, "the login is the first organization");
        flow.github_mut().select_owner(github_fixture::ORG);
        answer_reads(&mut flow);
        assert_eq!(flow.github().repos().loaded().map(Vec::len), Some(2));
        flow.github_mut().select_repo("acme/app");
        assert!(flow.ready());
        assert!(!flow.advance());
        assert_eq!(flow.due(), Some(FlowRequest::Github(GithubRequest::Preflight { repo: "acme/app".into() })));
        flow.github_answer(GithubAnswer::Preflight { repo: "acme/app".into(), result: Err("Issues disabled".into()) });
        assert_eq!(flow.step(), Step::ChooseProject);
        assert_eq!(flow.error().as_deref(), Some("Issues disabled"));
        assert_eq!(flow.primary_label(), Some(CONTINUE));

        // Back while a preflight runs: its pass doesn't carry the step on.
        flow.advance();
        let _ = flow.due();
        assert!(flow.back());
        flow.github_answer(GithubAnswer::Preflight { repo: "acme/app".into(), result: Ok(()) });
        assert_eq!(flow.step(), Step::Chooser);
        // ...but the pass is kept: Continue moves straight on.
        assert!(flow.connect(Tracker::Github));
        assert!(flow.advance());
        assert_eq!(flow.step(), Step::AgentRules);
    }

    #[test]
    fn github_signed_out_shows_how_to_sign_in_and_waits() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Github);
        let Some(FlowRequest::Github(GithubRequest::Account)) = flow.due() else { panic!("the account first") };
        flow.github_answer(GithubAnswer::Account(Err(GithubFailure::NotSignedIn)));
        assert!(flow.github().needs_auth());
        flow.set_repo("acme/app");
        assert!(!flow.ready() && !flow.advance());
        assert_eq!(flow.error(), None, "the instructions say what's missing");
        flow.github_mut().retry();
        answer_reads(&mut flow);
        assert!(!flow.github().needs_auth());
        assert!(continue_(&mut flow));
    }

    #[test]
    fn linear_without_a_key_waits_for_one() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Linear);
        while let Some(FlowRequest::Linear(req)) = flow.due() {
            flow.linear_answer(fixture::no_key(req));
        }
        assert!(flow.linear().needs_key());
        assert!(!flow.ready() && !flow.advance());
        assert_eq!(flow.error(), None, "the key field says what's missing, not Team");
    }

    #[test]
    fn a_connected_tracker_shows_connected_and_disables_the_others() {
        let mut flow = TrackerFlow::new();
        let states = |f: &TrackerFlow| f.cards().iter().map(|c| (c.tracker, c.state)).collect::<Vec<_>>();
        assert_eq!(
            states(&flow),
            vec![(Tracker::Github, CardState::Disabled), (Tracker::Linear, CardState::Disabled)],
            "nothing connects before the connection is read"
        );
        assert!(!flow.connect(Tracker::Github));

        flow.load(&local());
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connect), (Tracker::Linear, CardState::Connect)]);
        let cards = flow.cards();
        assert_eq!((cards[0].title, cards[0].body), (GITHUB_TITLE, GITHUB_BODY));
        assert_eq!(cards[0].body, "Sync issues from a GitHub repository.");

        flow.load(&github("acme/app"));
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connected), (Tracker::Linear, CardState::Disabled)]);
        assert!(!flow.connect(Tracker::Linear) && !flow.connect(Tracker::Github));
        assert_eq!(flow.step(), Step::Chooser);
        assert!(!flow.connect(Tracker::FileSystem), "only the two trackers are cards");
    }

    #[test]
    fn import_starts_the_connection_and_view_board_follows_it() {
        let mut flow = at_review("acme/app");
        assert_eq!(flow.primary_label(), Some(IMPORT));
        assert_eq!(flow.primary(), Action::None);
        assert_eq!(flow.step(), Step::Importing);
        assert_eq!(
            flow.due(),
            Some(FlowRequest::Connect(
                ConnectTracker::new(TrackerChoice::Github { repo: "acme/app".into() }, false)
                    .with_rules(DEFAULT_RULES)
            ))
        );
        assert_eq!(flow.due(), None, "asked once");
        assert_eq!(flow.primary_label(), Some(VIEW_BOARD));
        flow.connected(Ok(connected()));
        assert_eq!(flow.finished().map(|c| c.summary.as_str()), Some("GitHub acme/app"));
        assert_eq!(flow.connected_tracker(), Some(Tracker::Github), "the flow holds the new connection");
        assert_eq!(flow.primary(), Action::ViewBoard);
        // A late or repeated answer changes nothing.
        flow.connected(Err(ConnectError::Failed("late".into())));
        assert!(flow.finished().is_some());
    }

    #[test]
    fn a_refused_switch_offers_to_force_it_or_go_back() {
        let mut flow = at_review("acme/app");
        flow.primary();
        let _ = flow.due();
        flow.connected(Err(ConnectError::NotEmpty {
            from: IssueTrackerBackend::FileSystem,
            to: IssueTrackerBackend::Github,
            issues: 3,
            message: "project `app` has 3 issues on the local file system".into(),
        }));
        assert!(flow.error().unwrap().starts_with("Project `app` has 3 issues"));
        assert_eq!(flow.primary_label(), Some(SWITCH_ANYWAY));
        assert_eq!(flow.secondary(), Some(Secondary::Back));
        assert_eq!(flow.primary(), Action::None);
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("a forced retry") };
        assert!(req.force);
        flow.connected(Err(ConnectError::Preflight("gh can't see acme/app".into())));
        assert_eq!(flow.primary_label(), Some(TRY_AGAIN), "a failed preflight can't be forced");
        assert!(flow.back());
        assert_eq!((flow.step(), flow.apply()), (Step::ReviewImport, &Apply::Idle));
        assert_eq!(flow.error(), None);
    }
}
