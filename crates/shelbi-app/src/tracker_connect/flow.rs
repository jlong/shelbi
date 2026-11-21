//! The "Connect to an issue tracker" flow (`tracker-connect`, Figma "Issue
//! tracker · Connection flow" 2753:11686), toolkit-free and without IO, like
//! [`crate::new_project`] and [`crate::linear_setup`]. The desktop app opens
//! it from the Issues header's ⋮ menu and the palette's Connect to Issue
//! Tracker; it paints [`TrackerFlow`] and runs what [`TrackerFlow::due`] asks
//! for through its [`crate::settings::tracker::TrackerService`] (this
//! machine's, or the far daemon's for a project on another one).
//!
//! ```text
//!  Chooser ──Connect──▶ (authorized?) ──yes──▶ Chooser, Connected ──beat──▶ Choose project
//!     ▲                      │                    ▲      ▲                     │
//!     │                      no                   │      └───────Back──────────┘
//!     │                      ▼                    │
//!     └───────Back────── Authorize ──Continue─────┘
//!                     (Add API key /
//!                      Sign in to GitHub)
//!
//!  Choose project ──▶ Agent rules ──▶ Review import ──▶ Importing
//!                ◀──Back──       ◀──Back──
//! ```
//!
//! No step shows a counter: every footer is Back and the primary button,
//! right-aligned.
//!
//! - **Chooser** (01-select-tracker / 02a-github-connected): one card per
//!   tracker, GitHub Issues and Linear (no Jira). A card's Connect runs its
//!   tracker's authorization: the GitHub CLI's sign-in, or a working Linear
//!   API key. While it runs the card says [`CHECKING`]. Once authorized the
//!   card shows Connected and the others' Connect is disabled, and after a
//!   short beat ([`CONNECTED_BEAT_MS`], [`TrackerFlow::beat_due`]) the flow
//!   moves on to Choose project. Back from there returns to this Connected
//!   chooser, whose footer's Continue moves on again; there the other
//!   cards' Connect works again, to switch (nothing is written until
//!   Import), and the Connected badge stays until another tracker is
//!   authorized. A project already on a tracker shows its card Connected
//!   and the others disabled: **one external integration per project**.
//! - **Authorize** (02b-linear-api-key): when the tracker isn't authorized
//!   yet. Linear's Add API key (the key field with Verify; Continue waits for
//!   a verified key) and GitHub's sign-in help (`gh auth login`, with Retry).
//!   Continue returns to the chooser, Connected. Choose your Linear project's
//!   Change account opens it too, and then Back and Continue return there.
//! - **Choose project** (03-github-choose / 03a-linear-choose): GitHub's
//!   organization and repository through [`GithubChoice`], or Linear's
//!   workspace (read-only), team (the only required field) and project
//!   through the shared [`LinearChoice`]. Continue is offered once the
//!   choice can be connected; it runs the connection's own preflight
//!   ([`FlowRequest::Github`] / [`FlowRequest::Linear`]) and moves on when
//!   that passes, or shows why it didn't.
//! - **Agent rules** (04-agent-rules): the project's [`AgentRules`]
//!   (`issue_tracker.agent_rules`), two switches (both on for a project
//!   with no tracker yet), written with the connection.
//! - **Review import** (06-review-import): what the import would bring in,
//!   from one read of the chosen connection where the project lives
//!   ([`FlowRequest::Preview`], answered with [`TrackerFlow::preview_answer`]):
//!   the open count, who agents may work for under the rules, the first few
//!   issues and the closed ones left out. A failed read offers Retry. When
//!   the local board has issues ([`FlowRequest::LocalIssues`], answered with
//!   [`TrackerFlow::local_answer`]) a card offers to move them to the
//!   tracker first, on by default ([`TrackerFlow::move_on`]); off, the switch
//!   leaves them on the local file system (a forced switch). Its primary
//!   button (Import N issues) starts the move, then the connection.
//! - **Importing** (07-importing): the move runs first when it's on
//!   ([`FlowRequest::Move`], answered with [`TrackerFlow::moved`]: the
//!   project's `shelbi issue-store migrate --apply`), then the connection
//!   ([`crate::tracker_connect::ConnectTracker`], answered with
//!   [`TrackerFlow::connected`]); then the project's board reads the new
//!   tracker, and the step completes when its first read lands
//!   ([`TrackerFlow::imported`]). A move that stops says what moved and
//!   what didn't, with Try again and Back. A refusal because the local
//!   board still has issues (added after Review) offers the move again, or
//!   Switch anyway.

use shelbi_core::{IssueTrackerBackend, IssueTrackerConfig};

use super::{
    import_copy, move_copy, ConnectError, ConnectTracker, Connected, ImportPreview, MoveFailed, Moved, TrackerChoice,
};
use crate::github_setup::{GithubAnswer, GithubAuth, GithubChoice, GithubRequest, REPO_REQUIRED};
use crate::linear_setup::{KeyState, LinearAnswer, LinearChoice, LinearRequest, Loadable, TEAM_REQUIRED};
use crate::new_project::{github_config, Tracker};

/// The chooser's heading and the line under it. (No note under the cards:
/// the Figma dropped "Only one issue tracker can be connected at a time.")
pub const CHOOSER_HEADING: &str = "Connect to an Issue tracker";
pub const CHOOSER_SUBHEADING: &str = "Bring external issues into Shelbi and keep work in sync.";
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
/// Where Linear's personal API keys are made, and the Add API key step's
/// link there.
pub const LINEAR_KEYS_URL: &str = "https://linear.app/settings/account/security";
pub const LINEAR_KEYS_LINK: &str = "Open Linear";
/// The Add API key step (02b-linear-api-key).
pub const API_KEY_HEADING: &str = "Add API key";
pub const API_KEY_SUBHEADING: &str = "Shelbi needs access to the Linear API. Add a personal API key below.";
pub const API_KEY_LABEL: &str = "Linear account";
pub const API_KEY_PLACEHOLDER: &str = "lin_api…";
pub const API_KEY_HELP: &str = "Create one in Linear: Settings → Account → Security & access → Personal API keys.";
/// GitHub's authorization step: the GitHub CLI's sign-in help.
pub const GITHUB_AUTH_HEADING: &str = "Sign in to GitHub";
pub const GITHUB_AUTH_SUBHEADING: &str = "Shelbi needs access to GitHub through the GitHub CLI (gh).";
/// How long the chooser shows a card Connected before Choose project.
pub const CONNECTED_BEAT_MS: u64 = 700;
/// The primary button while the choice's preflight runs, and a card's while
/// its authorization is checked.
pub const CHECKING: &str = "Checking…";
/// The buttons.
pub const CONNECT: &str = "Connect";
/// The File system row's button in the new-project wizard's chooser.
pub const USE: &str = "Use";
pub const CONNECTED: &str = "Connected";
pub const CONTINUE: &str = "Continue";
pub const BACK: &str = "Back";
pub const CLOSE: &str = "Close";
pub const IMPORT: &str = "Import issues";
pub const VIEW_BOARD: &str = "View board";
pub const SWITCH_ANYWAY: &str = "Switch anyway";
/// The Importing step's primary button after a refusal with the move on.
pub const MOVE_AND_SWITCH: &str = "Move and switch";
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

/// The rows the new-project wizard's chooser offers: the trackers, then the
/// local file system (a new project has no board to keep yet).
pub const NEW_PROJECT_TRACKERS: [Tracker; 3] = [Tracker::Github, Tracker::Linear, Tracker::FileSystem];

/// The agent rules the new-project wizard's rules step starts on: the ones a
/// project created without the step gets (`shelbi init -y`, the TUI), so
/// accepting every preselection creates the same project everywhere.
pub const NEW_PROJECT_RULES: AgentRules = AgentRules { require_review: false, assigned_only: true };

/// Where the flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Chooser,
    /// The picked tracker isn't authorized yet: Linear's Add API key, or how
    /// to sign the GitHub CLI in.
    Authorize,
    ChooseProject,
    AgentRules,
    ReviewImport,
    Importing,
}

/// What a chooser card offers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardState {
    /// Its Connect button.
    Connect,
    /// The project is connected to it: the green badge, no button.
    Connected,
    /// Another tracker is connected (or being connected): Connect, disabled.
    Disabled,
    /// Its Connect was pressed and its authorization is being checked: the
    /// button says [`CHECKING`], disabled.
    Checking,
    /// The new-project wizard's File system row: its [`USE`] button, which
    /// needs no sub-screens.
    Use,
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

/// The Review import step's read of what the import would bring in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Preview {
    /// Not asked for (any step before Review import).
    Idle,
    /// Asked for: [`TrackerFlow::due`] hands it to the host next.
    Wanted,
    /// The host is reading it.
    Loading,
    Loaded(ImportPreview),
    /// The read failed: why (the step offers Retry).
    Failed(String),
}

/// The Review import step's count of the local board's issues, for its
/// move card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalIssues {
    /// Not asked for (any step before Review import).
    Idle,
    /// Asked for: [`TrackerFlow::due`] hands it to the host next.
    Wanted,
    /// The host is counting.
    Loading,
    Counted(usize),
    /// The count failed: the card still shows ("your local issues").
    Unknown(String),
}

/// Moving the local issues to the tracker, on Importing before the switch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Move {
    /// Not asked for (the move is off, or there's nothing to move).
    Idle,
    /// Asked for: [`TrackerFlow::due`] hands it to the host next.
    Wanted,
    /// The host is running it.
    Running,
    Done(Moved),
    /// It stopped: what moved and what didn't (Try again resumes).
    Failed(MoveFailed),
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
    /// Read what a first import of this connection would bring in, where the
    /// project lives (answered with [`TrackerFlow::preview_answer`]).
    Preview(TrackerChoice),
    /// Count the project's local issues, where the project lives (answered
    /// with [`TrackerFlow::local_answer`]).
    LocalIssues,
    /// Move the local issues into this connection, where the project lives
    /// (answered with [`TrackerFlow::moved`]).
    Move(TrackerChoice),
}

/// The Connected chooser's beat before it moves on to Choose project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Beat {
    Idle,
    /// [`TrackerFlow::beat_due`] hands it to the host next.
    Due,
    /// The host's timer runs; [`TrackerFlow::beat_done`] ends it.
    Running(u32),
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
    /// A card's Connect was pressed: its tracker's authorization is being
    /// checked (the card says Checking…).
    authorizing: Option<Tracker>,
    /// The tracker whose authorization passed: the chooser shows its card
    /// Connected.
    authorized: Option<Tracker>,
    /// The Connected chooser's beat before Choose project.
    beat: Beat,
    /// How many beats have started: a stale timer's end is dropped.
    beats: u32,
    /// The Authorize step was opened from Choose project (Change account):
    /// Back and Continue return there.
    rekey: bool,
    /// GitHub's organization and repository.
    github: GithubChoice,
    linear: LinearChoice,
    /// Continue was pressed on Choose project: the step moves on when the
    /// preflight it started passes.
    advancing: bool,
    rules: AgentRules,
    /// The Review import step's read, and the connection it was read for.
    preview: Preview,
    preview_for: Option<TrackerChoice>,
    /// The local board's issues, and whether Import moves them first.
    local: LocalIssues,
    move_on: bool,
    moving: Move,
    /// The connection is a forced switch (the move was turned off): a
    /// retry of a refusal that can't be overridden keeps it.
    force: bool,
    apply: Apply,
    /// How many issues the board's first read of the new tracker brought in.
    imported: Option<usize>,
    /// Why Continue was refused.
    error: Option<String>,
    /// The flow is the new-project wizard's Issues step
    /// ([`TrackerFlow::for_new_project`]).
    new_project: bool,
    /// The new-project chooser's choice: what its Continue goes on with
    /// (the recommended row until a row's button is pressed).
    choice: Tracker,
    /// The new-project flow just finished a choice ([`Self::take_complete`]).
    complete: bool,
    /// The `issue_tracker:` block the new-project flow last finished with.
    answer: Option<IssueTrackerConfig>,
}

impl Default for TrackerFlow {
    fn default() -> Self {
        TrackerFlow {
            step: Step::Chooser,
            current: None,
            tracker: None,
            authorizing: None,
            authorized: None,
            beat: Beat::Idle,
            beats: 0,
            rekey: false,
            github: GithubChoice::default(),
            linear: LinearChoice::default(),
            advancing: false,
            rules: DEFAULT_RULES,
            preview: Preview::Idle,
            preview_for: None,
            local: LocalIssues::Idle,
            move_on: true,
            moving: Move::Idle,
            force: false,
            apply: Apply::Idle,
            imported: None,
            error: None,
            new_project: false,
            choice: Tracker::FileSystem,
            complete: false,
            answer: None,
        }
    }
}

impl TrackerFlow {
    pub fn new() -> Self {
        Self::default()
    }

    /// The new-project wizard's Issues step: the same screens for a project
    /// that doesn't exist yet. The chooser adds the File system row and
    /// keeps a Continue (on the recommended row: GitHub on `github_repo`,
    /// the origin's, else the file system), Choose your GitHub project
    /// starts on `github_repo`, and the agent rules start on `rules`. The
    /// rules' Continue (or File system's Use) finishes the step
    /// ([`Self::take_complete`], [`Self::answer`]): a new project has
    /// nothing to review, move or import.
    pub fn for_new_project(github_repo: Option<&str>, rules: AgentRules) -> Self {
        TrackerFlow {
            current: Some(IssueTrackerConfig::default()),
            github: github_repo.map(GithubChoice::with_repo).unwrap_or_default(),
            rules,
            new_project: true,
            choice: if github_repo.is_some() { Tracker::Github } else { Tracker::FileSystem },
            ..TrackerFlow::default()
        }
    }

    /// The flow is the new-project wizard's.
    pub fn is_new_project(&self) -> bool {
        self.new_project
    }

    /// What the new-project chooser's Continue goes on with.
    pub fn choice(&self) -> Tracker {
        self.choice
    }

    /// The new-project flow finished a choice since the last ask (the
    /// wizard moves on to its next step).
    pub fn take_complete(&mut self) -> bool {
        std::mem::take(&mut self.complete)
    }

    /// The `issue_tracker:` block the new-project flow last finished with:
    /// the connection as picked with the agent rules (what Connect to an
    /// issue tracker writes over a fresh project), or the file system's.
    pub fn answer(&self) -> Option<&IssueTrackerConfig> {
        self.answer.as_ref()
    }

    /// Finish the new-project flow with `tracker`'s choice.
    fn finish(&mut self, tracker: Tracker) -> bool {
        let cfg = match tracker {
            Tracker::FileSystem => IssueTrackerConfig::default(),
            _ => match self.wanted() {
                Some(choice) => ConnectTracker::new(choice, false).with_rules(self.rules).config(&IssueTrackerConfig::default()),
                None => return false,
            },
        };
        self.error = None;
        self.choice = tracker;
        self.answer = Some(cfg);
        self.complete = true;
        true
    }

    /// The chooser's rows, in order.
    pub fn trackers(&self) -> &'static [Tracker] {
        if self.new_project {
            &NEW_PROJECT_TRACKERS
        } else {
            &TRACKERS
        }
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
        self.trackers()
            .iter()
            .copied()
            .map(|tracker| {
                let (title, body) = card_copy(tracker);
                let state = match () {
                    _ if connected == Some(tracker) => CardState::Connected,
                    _ if tracker == Tracker::FileSystem => {
                        if self.authorizing.is_some() || self.in_beat() {
                            CardState::Disabled
                        } else {
                            CardState::Use
                        }
                    }
                    _ if any || !self.loaded() => CardState::Disabled,
                    _ if self.authorizing == Some(tracker) => CardState::Checking,
                    _ if self.authorized == Some(tracker) => CardState::Connected,
                    // While a check or the beat runs nothing else connects;
                    // after Back another card can (it replaces the choice).
                    _ if self.authorizing.is_some() || self.in_beat() => CardState::Disabled,
                    _ => CardState::Connect,
                };
                Card { tracker, title, body, state }
            })
            .collect()
    }

    /// The tracker the chooser shows Connected for this flow (its
    /// authorization passed), before Choose project.
    pub fn authorized(&self) -> Option<Tracker> {
        self.authorized
    }

    /// A card's Connect: pick `tracker` and check its authorization (the
    /// GitHub CLI's sign-in, or a working Linear key). Authorized, the
    /// chooser shows it Connected and the beat moves on to Choose project;
    /// not, the Authorize step asks for it. False when the card can't
    /// connect (another tracker is connected, the connection isn't read yet,
    /// or the flow is past the chooser).
    pub fn connect(&mut self, tracker: Tracker) -> bool {
        let offered = self
            .cards()
            .iter()
            .any(|c| c.tracker == tracker && matches!(c.state, CardState::Connect | CardState::Use));
        if self.step != Step::Chooser || !offered {
            return false;
        }
        if tracker == Tracker::FileSystem {
            return self.finish(Tracker::FileSystem);
        }
        self.choice = tracker;
        self.tracker = Some(tracker);
        self.authorizing = Some(tracker);
        self.error = None;
        match tracker {
            Tracker::Linear => {
                // Choose project shows the key's workspace.
                self.linear.want_workspace();
                // A failed teams read is asked again.
                self.linear.retry();
            }
            Tracker::Github => {
                // Signed out before: check again (it may be signed in now).
                if self.github.needs_auth() || self.github.account().error().is_some() {
                    self.github.retry();
                }
            }
            Tracker::FileSystem => {}
        }
        self.settle();
        true
    }

    /// Whether the picked tracker is authorized: `Some(true)` yes,
    /// `Some(false)` no (the Authorize step asks), `None` not known yet.
    fn authorization(&self, tracker: Tracker) -> Option<bool> {
        match tracker {
            Tracker::Github => match self.github.auth() {
                GithubAuth::Valid => Some(!self.github.needs_auth()),
                GithubAuth::Missing { .. } => Some(false),
                GithubAuth::Unknown => None,
            },
            Tracker::Linear => match self.linear.key_state() {
                KeyState::Valid { .. } => Some(!self.linear.needs_key()),
                KeyState::Missing | KeyState::Rejected(_) => Some(false),
                KeyState::Unknown => None,
            },
            Tracker::FileSystem => None,
        }
    }

    /// A card's check finished: show it Connected, or ask for the
    /// authorization on its own step. A read that failed for another reason
    /// (the network) shows on the chooser; Connect asks again.
    fn settle(&mut self) {
        let Some(tracker) = self.authorizing else { return };
        match self.authorization(tracker) {
            Some(true) => {
                self.authorizing = None;
                self.show_connected(tracker);
            }
            Some(false) => {
                self.authorizing = None;
                self.rekey = false;
                self.step = Step::Authorize;
            }
            None => {
                if let (Tracker::Linear, Loadable::Failed(why)) = (tracker, self.linear.teams()) {
                    self.error = Some(format!("Couldn't reach Linear: {why}"));
                    self.authorizing = None;
                    // A tracker authorized before stays the choice.
                    self.tracker = self.authorized.or(self.tracker);
                }
            }
        }
    }

    /// The chooser with `tracker` Connected, and its beat due.
    fn show_connected(&mut self, tracker: Tracker) {
        self.step = Step::Chooser;
        self.authorized = Some(tracker);
        self.error = None;
        self.beat = Beat::Due;
    }

    /// The Connected chooser's beat, once: the host waits
    /// [`CONNECTED_BEAT_MS`] and hands the token back to [`Self::beat_done`].
    pub fn beat_due(&mut self) -> Option<u32> {
        if self.beat != Beat::Due {
            return None;
        }
        self.beats += 1;
        self.beat = Beat::Running(self.beats);
        Some(self.beats)
    }

    /// The beat ended: on to Choose project, unless the flow moved since.
    pub fn beat_done(&mut self, token: u32) {
        if self.beat == Beat::Running(token) && self.step == Step::Chooser && self.authorized.is_some() {
            self.beat = Beat::Idle;
            self.step = Step::ChooseProject;
        }
    }

    /// The Connected chooser is moving on by itself (no footer yet).
    pub fn in_beat(&self) -> bool {
        self.step == Step::Chooser && self.beat != Beat::Idle
    }

    /// Choose your Linear project's Change account: the Add API key step,
    /// to verify another key (Back keeps the working one).
    pub fn change_account(&mut self) {
        if self.step != Step::ChooseProject {
            return;
        }
        match self.tracker {
            Some(Tracker::Linear) => {
                self.linear.change_key();
                if self.linear.changing_key() {
                    self.advancing = false;
                    self.error = None;
                    self.rekey = true;
                    self.step = Step::Authorize;
                }
            }
            Some(Tracker::Github) => {
                self.error = None;
                self.github.change_account();
            }
            _ => {}
        }
    }

    /// Verify on the Authorize step: check the pasted Linear key (and store
    /// it), or read the GitHub CLI's account again.
    pub fn verify(&mut self) -> bool {
        if self.step != Step::Authorize {
            return false;
        }
        match self.tracker {
            Some(Tracker::Linear) => self.linear.submit_key(),
            Some(Tracker::Github) => {
                self.github.retry();
                true
            }
            _ => false,
        }
    }

    /// The Authorize step's tracker is authorized: Continue is offered.
    fn verified_auth(&self) -> bool {
        match self.tracker {
            Some(Tracker::Linear) => self.authorization(Tracker::Linear) == Some(true) && !self.linear.logging_in(),
            Some(Tracker::Github) => {
                self.authorization(Tracker::Github) == Some(true) && !self.github.account().is_loading()
            }
            _ => false,
        }
    }

    /// The Authorize step's line once Verify passed (`Verified · Ada
    /// Lovelace <ada@example.com>`, `Signed in as octocat`).
    pub fn verified_line(&self) -> Option<String> {
        if self.step != Step::Authorize || !self.verified_auth() {
            return None;
        }
        Some(match (self.tracker?, self.account()) {
            (Tracker::Github, Some(login)) => format!("Signed in as {login}"),
            (Tracker::Github, None) => "Signed in".to_string(),
            (_, Some(account)) => format!("Verified · {account}"),
            (_, None) => "Verified".to_string(),
        })
    }

    /// Why the Authorize step's key (or the key Verify sent) was refused,
    /// shown under the field.
    pub fn key_error(&self) -> Option<String> {
        if self.step != Step::Authorize || self.tracker != Some(Tracker::Linear) {
            return None;
        }
        self.linear.key_error().or(self.linear.error()).map(str::to_string)
    }

    /// The step's heading.
    pub fn heading(&self) -> String {
        let name = tracker_name(self.tracker.unwrap_or(Tracker::Github));
        match self.step {
            Step::Chooser if self.new_project => crate::new_project::ISSUES_TITLE.into(),
            Step::Chooser => CHOOSER_HEADING.into(),
            Step::Authorize => match self.tracker {
                Some(Tracker::Linear) => API_KEY_HEADING.into(),
                _ => GITHUB_AUTH_HEADING.into(),
            },
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
            Step::Chooser if self.new_project => crate::new_project::ISSUES_INTRO.into(),
            Step::Chooser => CHOOSER_SUBHEADING.into(),
            Step::Authorize => match self.tracker {
                Some(Tracker::Linear) => API_KEY_SUBHEADING.into(),
                _ => GITHUB_AUTH_SUBHEADING.into(),
            },
            Step::ChooseProject => match self.tracker {
                Some(Tracker::Linear) => "Select the team whose issues you want to work on in Shelbi.".into(),
                _ => "Connect the project whose issues you want to work on in Shelbi.".into(),
            },
            Step::AgentRules => format!("These rules apply only to new issues imported from {name}."),
            // With local issues the move card says what becomes of them.
            Step::ReviewImport if self.offers_move() => format!("Import from {}.", self.source_name()),
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

    // ---- Review import ----------------------------------------------------

    /// The Review import step's read.
    pub fn preview(&self) -> &Preview {
        &self.preview
    }

    /// The preview, once read.
    pub fn previewed(&self) -> Option<&ImportPreview> {
        match &self.preview {
            Preview::Loaded(p) => Some(p),
            _ => None,
        }
    }

    /// Ask for the preview of the current choice unless it's read (or being
    /// read) already: on reaching Review import, and on Retry.
    fn want_preview(&mut self) {
        let choice = self.wanted();
        let fresh = choice.is_some() && self.preview_for == choice;
        match self.preview {
            Preview::Loaded(_) | Preview::Loading | Preview::Wanted if fresh => {}
            _ => {
                self.preview = Preview::Wanted;
                self.preview_for = choice;
            }
        }
    }

    /// The failed read's Retry.
    pub fn retry_preview(&mut self) {
        if self.step == Step::ReviewImport && matches!(self.preview, Preview::Failed(_)) {
            self.preview = Preview::Idle;
            self.want_preview();
        }
    }

    /// The answer to a [`FlowRequest::Preview`]. An answer for a choice that
    /// has since changed is dropped.
    pub fn preview_answer(&mut self, choice: &TrackerChoice, result: Result<ImportPreview, String>) {
        if self.preview != Preview::Loading || self.preview_for.as_ref() != Some(choice) {
            return;
        }
        self.preview = match result {
            Ok(p) => Preview::Loaded(p),
            Err(why) => Preview::Failed(why),
        };
    }

    // ---- the move card -----------------------------------------------------

    /// The local board's count.
    pub fn local(&self) -> &LocalIssues {
        &self.local
    }

    /// Count the local board on reaching Review import (each time: issues
    /// may have been added since). A project not on its local board has
    /// nothing local to move.
    fn want_local(&mut self) {
        let local = self.current.as_ref().is_none_or(|c| c.backend == IssueTrackerBackend::FileSystem);
        if !local {
            self.local = LocalIssues::Counted(0);
        } else if self.local != LocalIssues::Loading {
            self.local = LocalIssues::Wanted;
        }
    }

    /// The answer to a [`FlowRequest::LocalIssues`].
    pub fn local_answer(&mut self, result: Result<usize, String>) {
        if self.local != LocalIssues::Loading {
            return;
        }
        self.local = match result {
            Ok(n) => LocalIssues::Counted(n),
            Err(why) => LocalIssues::Unknown(why),
        };
    }

    /// The step shows the move card: Review import with issues on the
    /// local board (or a count that failed), or Importing after a refusal
    /// because the local board still has issues.
    pub fn offers_move(&self) -> bool {
        match self.step {
            Step::ReviewImport => {
                matches!(self.local, LocalIssues::Counted(1..) | LocalIssues::Unknown(_))
            }
            Step::Importing => matches!(self.moving, Move::Idle | Move::Done(_)) && self.offers_force(),
            _ => false,
        }
    }

    /// How many local issues the card names, `None` when it can't say.
    pub fn move_count(&self) -> Option<usize> {
        match (&self.apply, &self.local) {
            (Apply::Refused(ConnectError::NotEmpty { issues, .. }), _) if self.step == Step::Importing => Some(*issues),
            (Apply::Refused(ConnectError::Unverified { .. }), _) if self.step == Step::Importing => None,
            (_, LocalIssues::Counted(n)) => Some(*n),
            _ => None,
        }
    }

    /// Whether Import moves the local issues first (on by default).
    pub fn move_on(&self) -> bool {
        self.move_on
    }

    /// The card's switch.
    pub fn set_move(&mut self, on: bool) {
        if self.offers_move() {
            self.move_on = on;
        }
    }

    /// Where the moved issues are created: the GitHub repository, or the
    /// Linear team.
    pub fn move_target(&self) -> String {
        match self.tracker {
            Some(Tracker::Github) => self.github.repo().to_string(),
            Some(Tracker::Linear) => {
                let conn = self.linear.connection();
                conn.team_ref().map(str::to_string).unwrap_or(conn.team)
            }
            _ => String::new(),
        }
    }

    /// "Move your 3 local issues to Linear".
    pub fn move_title(&self) -> String {
        move_copy::card_title(self.move_count(), self.tracker_title())
    }

    /// The card's help: what happens to the local issues, on or off.
    pub fn move_help(&self) -> String {
        if self.move_on {
            move_copy::card_on(self.move_count(), &self.move_target())
        } else {
            move_copy::card_off(self.move_count())
        }
    }

    fn tracker_title(&self) -> &'static str {
        tracker_name(self.tracker.unwrap_or(Tracker::Github))
    }

    /// The move's progress.
    pub fn moving(&self) -> &Move {
        &self.moving
    }

    /// The Importing step's move line: "Moving 3 local issues…" (not done)
    /// or "Moved 3 local issues to Linear" (done). `None` without a move
    /// (or when it stopped: that shows as the step's error).
    pub fn move_line(&self) -> Option<(bool, String)> {
        match &self.moving {
            Move::Wanted | Move::Running => Some((false, move_copy::moving(self.move_count()))),
            Move::Done(m) => Some((true, move_copy::moved(m.total(), self.tracker_title()))),
            Move::Idle | Move::Failed(_) => None,
        }
    }

    /// The answer to a [`FlowRequest::Move`]: moved, the switch follows
    /// (unforced: the tracker has every local issue now).
    pub fn moved(&mut self, result: Result<Moved, MoveFailed>) {
        if self.moving != Move::Running {
            return;
        }
        match result {
            Ok(done) => {
                self.moving = Move::Done(done);
                self.force = false;
                self.apply = Apply::Wanted { force: false };
            }
            Err(failed) => self.moving = Move::Failed(failed),
        }
    }

    /// Import: the move first when the card offers it and it's on, else the
    /// connection (forced when the card's move is off).
    fn start_import(&mut self) {
        self.imported = None;
        self.apply = Apply::Idle;
        self.moving = Move::Idle;
        let offered = self.offers_move();
        self.force = offered && !self.move_on;
        if offered && self.move_on {
            self.moving = Move::Wanted;
        } else {
            self.apply = Apply::Wanted { force: self.force };
        }
    }

    // ---- Importing --------------------------------------------------------

    /// How many open issues the import is bringing in, as the preview read
    /// them.
    pub fn expected(&self) -> Option<usize> {
        self.previewed().map(|p| p.open)
    }

    /// The project's board read the new tracker: the import is done, with
    /// `open` issues.
    pub fn imported(&mut self, open: usize) {
        if matches!(self.apply, Apply::Done(_)) && self.imported.is_none() {
            self.imported = Some(open);
        }
    }

    /// How many issues the import brought in, once the board has them.
    pub fn landed(&self) -> Option<usize> {
        self.imported
    }

    /// The Importing step's import line: "Importing 64 issues…" until the
    /// board has them, then "Imported 64 issues".
    pub fn import_line(&self) -> String {
        match self.imported {
            Some(n) => import_copy::imported(n),
            None => import_copy::importing(self.expected()),
        }
    }


    // ---- moving -----------------------------------------------------------

    /// Whether the step's Continue (its primary button) can be pressed.
    pub fn ready(&self) -> bool {
        match self.step {
            // The new-project chooser always goes on with its choice
            // (once no check runs).
            Step::Chooser if self.new_project => self.authorizing.is_none(),
            // The Connected chooser's Continue (after Back from Choose
            // project, or Return during the beat).
            Step::Chooser => self.authorized.is_some() && self.authorizing.is_none(),
            Step::Authorize => self.verified_auth(),
            Step::ChooseProject => {
                let blocked = match self.tracker {
                    Some(Tracker::Linear) => self.linear.busy(),
                    Some(Tracker::Github) => self.github.needs_auth() || self.github.preflighting(),
                    _ => true,
                };
                !blocked && self.wanted().is_some()
            }
            Step::AgentRules => true,
            // The count is in the button: it waits for the read (a failed
            // read can still import, or Retry), and for the local count (so
            // the move card shows before Import can be pressed).
            Step::ReviewImport => {
                matches!(self.preview, Preview::Loaded(_) | Preview::Failed(_))
                    && !matches!(self.local, LocalIssues::Wanted | LocalIssues::Loading)
            }
            Step::Importing => {
                matches!(self.moving, Move::Failed(_))
                    || match &self.apply {
                        Apply::Running | Apply::Done(_) | Apply::Refused(_) => true,
                        Apply::Idle | Apply::Wanted { .. } => false,
                    }
            }
        }
    }

    /// Why Continue was refused, or why the connection was.
    pub fn error(&self) -> Option<String> {
        if let Some(e) = &self.error {
            return Some(e.clone());
        }
        if let Move::Failed(f) = &self.moving {
            return Some(move_copy::failed(f, self.tracker_title()));
        }
        if let Apply::Refused(e) = &self.apply {
            return Some(match e {
                // The local board still has issues: readable, not the CLI's
                // migrate instructions (the move card offers the move).
                ConnectError::NotEmpty { issues, .. } => move_copy::still_local(Some(*issues), self.tracker_title()),
                ConnectError::Unverified { .. } => move_copy::still_local(None, self.tracker_title()),
                e => crate::view::title_case(e.message()),
            });
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
    /// On the Authorize step, Continue before the key verifies (Return)
    /// runs Verify.
    pub fn advance(&mut self) -> bool {
        if !self.ready() {
            if self.step == Step::Authorize {
                self.verify();
                return false;
            }
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
        if self.new_project {
            match self.step {
                // The chooser's Continue presses its choice's button, unless
                // that tracker is Connected already (then on to Choose
                // project, as below).
                Step::Chooser if self.authorized != Some(self.choice) => {
                    let choice = self.choice;
                    return self.connect(choice);
                }
                // The rules are the last screen: the step is done.
                Step::AgentRules => {
                    let Some(tracker) = self.tracker else { return false };
                    return self.finish(tracker);
                }
                _ => {}
            }
        }
        self.step = match self.step {
            Step::Chooser => {
                self.beat = Beat::Idle;
                self.tracker = self.authorized;
                Step::ChooseProject
            }
            Step::Authorize if self.rekey => {
                self.rekey = false;
                Step::ChooseProject
            }
            Step::Authorize => {
                let Some(tracker) = self.tracker else { return false };
                self.show_connected(tracker);
                Step::Chooser
            }
            Step::ChooseProject => Step::AgentRules,
            Step::AgentRules => {
                self.want_preview();
                self.want_local();
                Step::ReviewImport
            }
            Step::ReviewImport => {
                self.start_import();
                Step::Importing
            }
            Step::Importing => return false,
        };
        true
    }

    /// Whether Back can be pressed: on every step after the chooser but
    /// Importing, which has it only after a refusal or a move that stopped
    /// (to change the choice).
    pub fn can_back(&self) -> bool {
        match self.step {
            Step::Chooser => false,
            // A key being verified finishes first (its answer decides
            // where Change account's Back lands).
            Step::Authorize => !self.linear.logging_in(),
            Step::ChooseProject | Step::AgentRules | Step::ReviewImport => true,
            Step::Importing => matches!(self.apply, Apply::Refused(_)) || matches!(self.moving, Move::Failed(_)),
        }
    }

    /// Back: the step before, its values kept. From Choose project that's
    /// the chooser with the tracker Connected; from the Authorize step, the
    /// fresh chooser (or Choose project, for Change account).
    pub fn back(&mut self) -> bool {
        if !self.can_back() {
            return false;
        }
        self.error = None;
        // A preflight still running doesn't carry the step on.
        self.advancing = false;
        self.step = match self.step {
            Step::Authorize if self.rekey => {
                self.rekey = false;
                self.linear.cancel_change_key();
                Step::ChooseProject
            }
            Step::Authorize => {
                // A tracker authorized before (Back, then another card's
                // Connect) stays Connected and the choice.
                if self.authorized.is_some() && self.authorized != self.tracker {
                    self.tracker = self.authorized;
                } else {
                    self.authorized = None;
                }
                if let Some(tracker) = self.authorized {
                    self.choice = tracker;
                }
                Step::Chooser
            }
            Step::ChooseProject => {
                self.beat = Beat::Idle;
                Step::Chooser
            }
            Step::AgentRules => Step::ChooseProject,
            Step::ReviewImport => Step::AgentRules,
            Step::Importing => {
                self.apply = Apply::Idle;
                self.moving = Move::Idle;
                // Counted again: the board may have changed.
                self.want_local();
                Step::ReviewImport
            }
            Step::Chooser => return false,
        };
        true
    }

    /// The footer's left button: Back on the steps after the chooser, Close
    /// once the connection is under way (closing leaves it running).
    pub fn secondary(&self) -> Option<Secondary> {
        match self.step {
            Step::Chooser => None,
            Step::Importing if !self.can_back() => Some(Secondary::Close),
            _ => Some(Secondary::Back),
        }
    }

    /// The primary button's label. The chooser has none (its cards connect)
    /// but Continue once a card is Connected and its beat is over (Back from
    /// Choose project).
    pub fn primary_label(&self) -> Option<String> {
        let label = match self.step {
            Step::Chooser if self.new_project => CONTINUE,
            Step::Chooser if self.ready() && !self.in_beat() => CONTINUE,
            Step::Chooser => return None,
            Step::ChooseProject if self.checking() => CHECKING,
            Step::Authorize | Step::ChooseProject | Step::AgentRules => CONTINUE,
            Step::ReviewImport => {
                return Some(match self.previewed() {
                    Some(p) => import_copy::import_button(p.open),
                    None => IMPORT.to_string(),
                })
            }
            Step::Importing if matches!(self.moving, Move::Failed(_)) => TRY_AGAIN,
            Step::Importing => match &self.apply {
                Apply::Refused(_) if self.offers_force() && self.move_on => MOVE_AND_SWITCH,
                Apply::Refused(_) if self.offers_force() => SWITCH_ANYWAY,
                Apply::Refused(_) => TRY_AGAIN,
                _ => VIEW_BOARD,
            },
        };
        Some(label.to_string())
    }

    /// The primary button: Continue on the steps before Importing; there,
    /// View board; after a move that stopped, Try again; after a refusal
    /// because the local board has issues, Move and switch (the card's move
    /// on) or Switch anyway (off); after another refusal, Try again.
    pub fn primary(&mut self) -> Action {
        if self.step != Step::Importing {
            self.advance();
            return Action::None;
        }
        if matches!(self.moving, Move::Failed(_)) {
            // Moving again skips what made it (matched by shelbi id).
            self.moving = Move::Wanted;
            return Action::None;
        }
        match &self.apply {
            Apply::Running | Apply::Done(_) => Action::ViewBoard,
            Apply::Refused(_) if self.offers_force() && self.move_on => {
                self.apply = Apply::Idle;
                self.moving = Move::Wanted;
                Action::None
            }
            Apply::Refused(_) if self.offers_force() => {
                self.force = true;
                self.apply = Apply::Wanted { force: true };
                Action::None
            }
            Apply::Refused(_) => {
                self.apply = Apply::Wanted { force: self.force };
                Action::None
            }
            Apply::Idle | Apply::Wanted { .. } => Action::None,
        }
    }

    // ---- the work ---------------------------------------------------------

    /// The next blocking request, marked running: the host runs it off the
    /// UI thread and answers. Call until `None`.
    pub fn due(&mut self) -> Option<FlowRequest> {
        if self.moving == Move::Wanted {
            let Some(choice) = self.wanted() else {
                self.moving = Move::Idle;
                self.step = Step::ChooseProject;
                return None;
            };
            self.moving = Move::Running;
            return Some(FlowRequest::Move(choice));
        }
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
        if self.preview == Preview::Wanted {
            match self.preview_for.clone() {
                Some(choice) => {
                    self.preview = Preview::Loading;
                    return Some(FlowRequest::Preview(choice));
                }
                None => self.preview = Preview::Idle,
            }
        }
        if self.local == LocalIssues::Wanted {
            self.local = LocalIssues::Loading;
            return Some(FlowRequest::LocalIssues);
        }
        // The picked tracker's reads: its authorization check (from the
        // chooser, or the Authorize step), and Choose project's pickers
        // (from the Connected chooser on, read ahead during the beat).
        let reads = match self.step {
            Step::Chooser => self.authorizing.is_some() || self.authorized.is_some(),
            Step::Authorize | Step::ChooseProject => true,
            _ => false,
        };
        if reads {
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
        self.settle();
        // Choose project never shows the key field: a key that stopped
        // working there asks for another on the Add API key step.
        if self.step == Step::ChooseProject && self.tracker == Some(Tracker::Linear) && self.linear.needs_key() {
            self.advancing = false;
            self.rekey = false;
            self.authorized = None;
            self.step = Step::Authorize;
        }
        self.finish_advance();
    }

    /// The answer to a [`FlowRequest::Github`].
    pub fn github_answer(&mut self, answer: GithubAnswer) {
        self.github.answer(answer);
        self.settle();
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
        answer_with(flow, fixture::answer);
    }

    /// Run the flow's reads, Linear's answered by `linear`.
    fn answer_with(flow: &mut TrackerFlow, linear: fn(LinearRequest) -> LinearAnswer) {
        while let Some(req) = flow.due() {
            match req {
                FlowRequest::Linear(req) => flow.linear_answer(linear(req)),
                FlowRequest::Github(req) => flow.github_answer(github_fixture::answer(req)),
                FlowRequest::Preview(choice) => {
                    let answer = crate::tracker_connect::fixture::answer(choice.clone());
                    flow.preview_answer(&choice, answer)
                }
                // An empty local board unless a test says otherwise.
                FlowRequest::LocalIssues => flow.local_answer(Ok(0)),
                FlowRequest::Connect(c) => panic!("no connection yet: {c:?}"),
                FlowRequest::Move(c) => panic!("no move yet: {c:?}"),
            }
        }
    }

    /// Run out the Connected chooser's beat.
    fn beat(flow: &mut TrackerFlow) {
        let token = flow.beat_due().expect("the Connected chooser's beat");
        assert_eq!(flow.beat_due(), None, "one beat");
        flow.beat_done(token);
    }

    /// A card's Connect with the tracker authorized (the fixtures), through
    /// the Connected chooser to Choose project.
    fn connect(flow: &mut TrackerFlow, tracker: Tracker) {
        assert!(flow.connect(tracker));
        answer_reads(flow);
        assert_eq!((flow.step(), flow.authorized()), (Step::Chooser, Some(tracker)));
        beat(flow);
        assert_eq!(flow.step(), Step::ChooseProject);
    }

    fn states(flow: &TrackerFlow) -> Vec<(Tracker, CardState)> {
        flow.cards().iter().map(|c| (c.tracker, c.state)).collect()
    }

    /// Continue, with the preflight it starts answered.
    fn continue_(flow: &mut TrackerFlow) -> bool {
        let before = flow.step();
        flow.advance();
        if before != Step::ReviewImport {
            answer_reads(flow);
        }
        flow.step() != before
    }

    fn at_review(repo: &str) -> TrackerFlow {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        connect(&mut flow, Tracker::Github);
        flow.set_repo(repo);
        assert!(continue_(&mut flow) && continue_(&mut flow));
        assert_eq!(flow.step(), Step::ReviewImport);
        flow
    }

    /// Review your first import with `local` answering the local board's
    /// count (the preview read too).
    fn at_review_with(tracker: Tracker, local: Result<usize, String>) -> TrackerFlow {
        let mut flow = TrackerFlow::new();
        flow.load(&self::local());
        connect(&mut flow, tracker);
        match tracker {
            Tracker::Linear => {
                flow.linear_mut().select_team("ENG");
            }
            _ => flow.set_repo("acme/app"),
        }
        assert!(continue_(&mut flow));
        assert_eq!(flow.step(), Step::AgentRules);
        flow.advance();
        assert_eq!(flow.step(), Step::ReviewImport);
        let mut local = Some(local);
        while let Some(req) = flow.due() {
            match req {
                FlowRequest::Preview(choice) => {
                    let answer = crate::tracker_connect::fixture::answer(choice.clone());
                    flow.preview_answer(&choice, answer)
                }
                FlowRequest::LocalIssues => {
                    assert!(!flow.ready(), "Import waits for the local count");
                    flow.local_answer(local.take().expect("counted once"))
                }
                other => panic!("{other:?}"),
            }
        }
        flow
    }

    fn issue(id: &str, title: &str) -> crate::tracker_connect::MovedIssue {
        crate::tracker_connect::MovedIssue { id: id.into(), title: title.into() }
    }

    #[test]
    fn the_rules_step_starts_from_the_frame_or_the_projects_and_names_the_account() {
        // No tracker yet: the frame's defaults (both on).
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert_eq!(flow.rules(), DEFAULT_RULES);
        assert!(flow.connect(Tracker::Github));
        assert_eq!(flow.assigned_title(), "Only work on issues assigned to me");
        // The account comes from the GitHub check's own account read.
        answer_reads(&mut flow);
        assert_eq!(flow.assigned_title(), "Only work on issues assigned to me (octocat)");
        beat(&mut flow);
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
    fn the_steps_run_in_order_with_back_and_continue_and_no_counter() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert_eq!(flow.step(), Step::Chooser);
        assert_eq!((flow.primary_label().as_deref(), flow.secondary()), (None, None), "the cards connect");
        assert!(!flow.advance() && !flow.back());

        connect(&mut flow, Tracker::Github);
        assert_eq!(flow.heading(), "Choose your GitHub project");
        flow.set_repo("acme/app");
        let mut seen = vec![flow.step()];
        while flow.step() != Step::Importing {
            assert_eq!(flow.secondary(), Some(Secondary::Back));
            assert!(continue_(&mut flow), "{:?} continues", flow.step());
            seen.push(flow.step());
        }
        assert_eq!(seen, vec![Step::ChooseProject, Step::AgentRules, Step::ReviewImport, Step::Importing]);
        assert_eq!(flow.secondary(), Some(Secondary::Close), "the connection runs: no Back");
        assert!(!flow.back());
    }

    #[test]
    fn connect_shows_the_card_connected_then_moves_on_after_the_beat() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert!(flow.connect(Tracker::Github));
        // The check runs: the card says so, the other can't connect.
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Checking), (Tracker::Linear, CardState::Disabled)]);
        assert!(!flow.connect(Tracker::Linear));
        assert_eq!(flow.due(), Some(FlowRequest::Github(GithubRequest::Account)));
        flow.github_answer(github_fixture::answer(GithubRequest::Account));

        // Authorized: GitHub Connected, Linear disabled (02a), no footer
        // while the beat runs.
        assert_eq!(flow.step(), Step::Chooser);
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connected), (Tracker::Linear, CardState::Disabled)]);
        assert!(flow.in_beat());
        assert_eq!((flow.primary_label(), flow.secondary()), (None, None));
        let token = flow.beat_due().expect("the beat");
        flow.beat_done(token + 1);
        assert_eq!(flow.step(), Step::Chooser, "another beat's end is dropped");
        flow.beat_done(token);
        assert_eq!(flow.step(), Step::ChooseProject);
        assert_eq!(flow.heading(), "Choose your GitHub project");

        // Back: the chooser, still Connected, with Continue to move on
        // again, and Linear's Connect working again to switch.
        assert!(flow.back());
        assert_eq!(flow.step(), Step::Chooser);
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connected), (Tracker::Linear, CardState::Connect)]);
        assert!(!flow.in_beat() && flow.beat_due().is_none(), "no beat after Back");
        assert_eq!((flow.primary_label().as_deref(), flow.secondary()), (Some(CONTINUE), None));
        assert!(flow.ready() && flow.advance());
        assert_eq!(flow.step(), Step::ChooseProject);

        // Return during the beat moves on at once; the beat's end is then
        // dropped.
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Github);
        answer_reads(&mut flow);
        let token = flow.beat_due().unwrap();
        assert!(flow.advance());
        assert_eq!(flow.step(), Step::ChooseProject);
        flow.back();
        flow.beat_done(token);
        assert_eq!(flow.step(), Step::Chooser, "the old beat doesn't carry the chooser on");
    }

    #[test]
    fn after_back_another_card_switches_and_the_badge_stays_until_it_connects() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        connect(&mut flow, Tracker::Github);
        flow.set_repo("acme/app");
        assert!(flow.back());

        // Linear without a key: GitHub keeps its badge while Linear is
        // checked and while its key is asked for; Back keeps GitHub the
        // choice.
        assert!(flow.connect(Tracker::Linear));
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connected), (Tracker::Linear, CardState::Checking)]);
        assert!(!flow.ready() && flow.primary_label().is_none(), "no Continue while Linear is checked");
        answer_with(&mut flow, fixture::no_key);
        assert_eq!((flow.step(), flow.heading()), (Step::Authorize, API_KEY_HEADING.to_string()));
        assert!(flow.back());
        assert_eq!((flow.step(), flow.tracker(), flow.authorized()), (Step::Chooser, Some(Tracker::Github), Some(Tracker::Github)));
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connected), (Tracker::Linear, CardState::Connect)]);
        assert!(flow.advance());
        assert_eq!((flow.heading(), flow.repo()), ("Choose your GitHub project".to_string(), "acme/app"));

        // A key verified this time: Linear replaces GitHub.
        assert!(flow.back());
        flow.connect(Tracker::Linear);
        flow.linear_mut().set_key_input("lin_api_good");
        flow.verify();
        answer_reads(&mut flow);
        assert!(flow.advance());
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Disabled), (Tracker::Linear, CardState::Connected)]);
        beat(&mut flow);
        assert_eq!((flow.step(), flow.heading()), (Step::ChooseProject, "Choose your Linear project".to_string()));
        assert!(flow.back());
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connect), (Tracker::Linear, CardState::Connected)]);
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
        assert_eq!(flow.authorized(), Some(Tracker::Github));
        assert!(flow.advance());
        assert_eq!(flow.repo(), "acme/app", "the repo is kept");
        assert!(flow.ready());
    }

    #[test]
    fn continue_waits_until_the_project_is_chosen() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        connect(&mut flow, Tracker::Github);
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
        assert!(flow.connect(Tracker::Linear));
        assert!(matches!(flow.due(), Some(FlowRequest::Linear(LinearRequest::Teams))), "the check is the teams read");
        assert_eq!(flow.step(), Step::Chooser, "until it answers");
        flow.linear_answer(fixture::answer(LinearRequest::Teams));
        assert_eq!(flow.authorized(), Some(Tracker::Linear), "a working key skips Add API key");
        beat(&mut flow);
        assert_eq!(flow.heading(), "Choose your Linear project");
        answer_reads(&mut flow);
        assert!(!flow.advance());
        assert_eq!(flow.error().as_deref(), Some(TEAM_REQUIRED));
        flow.linear_mut().select_team("ENG");
        answer_reads(&mut flow);
        assert!(flow.ready());
        assert!(!flow.advance(), "Continue runs the preflight first");
        assert!(flow.checking());
        assert_eq!(flow.primary_label().as_deref(), Some(CHECKING));
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
        connect(&mut flow, Tracker::Github);
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
        assert_eq!(flow.primary_label().as_deref(), Some(CONTINUE));

        // Back while a preflight runs: its pass doesn't carry the step on.
        flow.advance();
        let _ = flow.due();
        assert!(flow.back());
        flow.github_answer(GithubAnswer::Preflight { repo: "acme/app".into(), result: Ok(()) });
        assert_eq!(flow.step(), Step::Chooser);
        // ...but the pass is kept: Continue moves straight on.
        assert!(flow.advance());
        assert!(flow.advance());
        assert_eq!(flow.step(), Step::AgentRules);
    }

    #[test]
    fn github_signed_out_asks_to_sign_in_then_shows_it_connected() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Github);
        let Some(FlowRequest::Github(GithubRequest::Account)) = flow.due() else { panic!("the account first") };
        flow.github_answer(GithubAnswer::Account(Err(GithubFailure::NotSignedIn)));
        assert_eq!(flow.step(), Step::Authorize);
        assert_eq!((flow.heading(), flow.subheading()), (GITHUB_AUTH_HEADING.to_string(), GITHUB_AUTH_SUBHEADING.to_string()));
        assert!(flow.github().needs_auth());
        assert_eq!((flow.primary_label().as_deref(), flow.secondary()), (Some(CONTINUE), Some(Secondary::Back)));
        assert!(!flow.ready(), "Continue waits for the sign-in");
        assert_eq!(flow.error(), None, "the instructions say what's missing");
        // Return (Continue before it's signed in) checks again: still out.
        assert!(!flow.advance());
        assert_eq!(flow.due(), Some(FlowRequest::Github(GithubRequest::Account)));
        flow.github_answer(GithubAnswer::Account(Err(GithubFailure::NotSignedIn)));
        assert!(!flow.ready() && flow.verified_line().is_none());
        // Signed in now: Retry passes, and Continue shows it Connected.
        assert!(flow.verify());
        answer_reads(&mut flow);
        assert_eq!(flow.step(), Step::Authorize, "Continue is the user's");
        assert_eq!(flow.verified_line().as_deref(), Some("Signed in as octocat"));
        assert!(flow.ready() && flow.advance());
        assert_eq!((flow.step(), flow.authorized()), (Step::Chooser, Some(Tracker::Github)));
        beat(&mut flow);
        assert_eq!(flow.step(), Step::ChooseProject);

        // Back from the sign-in step: the fresh chooser, and Connect checks
        // again.
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Github);
        let _ = flow.due();
        flow.github_answer(GithubAnswer::Account(Err(GithubFailure::NotSignedIn)));
        assert!(flow.back());
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connect), (Tracker::Linear, CardState::Connect)]);
        assert!(flow.connect(Tracker::Github));
        assert_eq!(flow.due(), Some(FlowRequest::Github(GithubRequest::Account)), "checked again");
    }

    #[test]
    fn linear_without_a_key_asks_for_one_on_its_own_step() {
        use crate::linear_setup::{LinearFailure, KEY_EMPTY};
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Linear);
        answer_with(&mut flow, fixture::no_key);
        assert_eq!(flow.step(), Step::Authorize);
        assert_eq!((flow.heading(), flow.subheading()), (API_KEY_HEADING.to_string(), API_KEY_SUBHEADING.to_string()));
        assert_eq!(flow.authorized(), None);
        assert!(!flow.ready(), "Continue waits for a verified key");
        assert_eq!((flow.error(), flow.key_error()), (None, None), "the field says what's missing");

        // Return verifies: an empty field says so under it.
        assert!(!flow.advance());
        assert_eq!(flow.key_error().as_deref(), Some(KEY_EMPTY));
        // A key Linear refuses: why, under the field.
        flow.linear_mut().set_key_input("lin_api_bad");
        assert!(!flow.advance());
        let Some(FlowRequest::Linear(LinearRequest::Login(_))) = flow.due() else { panic!("the login") };
        assert!(!flow.can_back(), "the key's check finishes first");
        flow.linear_answer(LinearAnswer::Login(Err(LinearFailure::Rejected("Linear refused this key.".into()))));
        assert_eq!((flow.step(), flow.key_error().as_deref()), (Step::Authorize, Some("Linear refused this key.")));
        assert!(!flow.ready());
        // A good one: verified, and Continue shows Linear Connected.
        flow.linear_mut().set_key_input("lin_api_good");
        assert!(flow.verify());
        answer_reads(&mut flow);
        assert_eq!((flow.key_error(), flow.verified_line()), (None, Some(format!("Verified · {}", fixture::VIEWER))));
        assert!(flow.ready() && flow.advance());
        assert_eq!(flow.step(), Step::Chooser);
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Disabled), (Tracker::Linear, CardState::Connected)]);
        answer_reads(&mut flow);
        beat(&mut flow);
        assert_eq!(flow.heading(), "Choose your Linear project");
        assert!(!flow.linear().needs_key());

        // Change account opens Add API key; Back keeps the working key.
        flow.change_account();
        assert_eq!((flow.step(), flow.heading()), (Step::Authorize, API_KEY_HEADING.to_string()));
        assert!(!flow.ready(), "a new key verifies first");
        assert!(flow.back());
        assert_eq!(flow.step(), Step::ChooseProject);
        assert!(!flow.linear().needs_key());
        // ...and a new key's Continue goes straight back to Choose project.
        flow.change_account();
        flow.linear_mut().set_key_input("lin_api_other");
        assert!(!flow.advance(), "Return verifies first");
        answer_reads(&mut flow);
        assert!(flow.advance());
        assert_eq!(flow.step(), Step::ChooseProject);
    }

    #[test]
    fn a_key_that_stops_working_on_choose_project_asks_for_another() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        connect(&mut flow, Tracker::Linear);
        flow.linear_answer(LinearAnswer::Viewer(Err(crate::linear_setup::LinearFailure::Rejected("revoked".into()))));
        assert_eq!(flow.step(), Step::Authorize, "Choose project never shows the key field");
        assert_eq!(flow.key_error().as_deref(), Some("revoked"));
        assert!(flow.back());
        assert_eq!((flow.step(), flow.authorized()), (Step::Chooser, None), "the fresh chooser");
    }

    #[test]
    fn a_failed_check_shows_on_the_chooser_and_connect_asks_again() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        flow.connect(Tracker::Linear);
        let _ = flow.due();
        flow.linear_answer(LinearAnswer::Teams(Err(crate::linear_setup::LinearFailure::Other("offline".into()))));
        assert_eq!(flow.step(), Step::Chooser);
        assert_eq!(flow.error().as_deref(), Some("Couldn't reach Linear: offline"));
        assert_eq!(states(&flow), vec![(Tracker::Github, CardState::Connect), (Tracker::Linear, CardState::Connect)]);
        assert!(flow.connect(Tracker::Linear));
        assert_eq!(flow.error(), None);
        assert_eq!(flow.due(), Some(FlowRequest::Linear(LinearRequest::Teams)), "asked again");
    }

    #[test]
    fn a_connected_tracker_shows_connected_and_disables_the_others() {
        let mut flow = TrackerFlow::new();
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
        assert_eq!(flow.primary_label(), None, "nothing to continue to");
        assert!(!flow.connect(Tracker::FileSystem), "only the two trackers are cards");
    }

    #[test]
    fn import_starts_the_connection_and_view_board_follows_it() {
        let mut flow = at_review("acme/app");
        assert_eq!(flow.primary_label().as_deref(), Some("Import 64 issues"));
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
        assert_eq!(flow.primary_label().as_deref(), Some(VIEW_BOARD));
        flow.connected(Ok(connected()));
        assert_eq!(flow.finished().map(|c| c.summary.as_str()), Some("GitHub acme/app"));
        assert_eq!(flow.connected_tracker(), Some(Tracker::Github), "the flow holds the new connection");
        assert_eq!(flow.primary(), Action::ViewBoard);
        // A late or repeated answer changes nothing.
        flow.connected(Err(ConnectError::Failed("late".into())));
        assert!(flow.finished().is_some());

        // The board's first read of the new tracker completes the import.
        assert_eq!((flow.landed(), flow.import_line()), (None, "Importing 64 issues…".to_string()));
        flow.imported(61);
        assert_eq!((flow.landed(), flow.import_line()), (Some(61), "Imported 61 issues".to_string()));
        flow.imported(70);
        assert_eq!(flow.landed(), Some(61), "the first read is the import");
    }

    #[test]
    fn review_reads_the_preview_once_and_a_failure_retries() {
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        connect(&mut flow, Tracker::Github);
        flow.set_repo("acme/app");
        assert!(continue_(&mut flow));
        assert_eq!(flow.step(), Step::AgentRules);
        assert_eq!(flow.preview(), &Preview::Idle, "not read before the step");
        flow.advance();
        assert_eq!(flow.step(), Step::ReviewImport);
        let choice = TrackerChoice::Github { repo: "acme/app".into() };
        assert_eq!(flow.due(), Some(FlowRequest::Preview(choice.clone())));
        assert_eq!(flow.due(), Some(FlowRequest::LocalIssues), "and the local board's count");
        flow.local_answer(Ok(0));
        assert_eq!(flow.due(), None, "asked once");
        assert!(!flow.ready(), "Import waits for the count");
        assert_eq!(flow.primary_label().as_deref(), Some(IMPORT));
        // A stale answer (another repo) is dropped.
        flow.preview_answer(&TrackerChoice::Github { repo: "acme/web".into() }, Err("x".into()));
        assert_eq!(flow.preview(), &Preview::Loading);
        flow.preview_answer(&choice, Err("gh: rate limited".into()));
        assert_eq!(flow.preview(), &Preview::Failed("gh: rate limited".into()));
        assert!(flow.ready(), "a failed read can still import");
        flow.retry_preview();
        assert_eq!(flow.due(), Some(FlowRequest::Preview(choice.clone())));
        flow.preview_answer(&choice, crate::tracker_connect::fixture::answer(choice.clone()));
        assert_eq!(flow.expected(), Some(64));

        // Back and on again keeps the read; another repo reads again.
        flow.back();
        flow.advance();
        assert_eq!(flow.due(), Some(FlowRequest::LocalIssues), "only the local board is counted again");
        flow.local_answer(Ok(0));
        assert_eq!(flow.due(), None);
        assert_eq!(flow.expected(), Some(64));
        flow.back();
        flow.back();
        flow.set_repo("acme/web");
        answer_reads(&mut flow);
        assert!(continue_(&mut flow) && continue_(&mut flow));
        assert_eq!(flow.step(), Step::ReviewImport);
        assert_eq!(flow.expected(), Some(64), "acme/web's read (the fixture's counts)");
        assert_eq!(flow.subheading(), "Import from acme/web. Your existing Shelbi tasks stay on this board.");
    }

    #[test]
    fn the_review_copy_follows_the_assignment_rule() {
        use crate::tracker_connect::{import_copy as c, ImportPreview};
        let p = ImportPreview { open: 64, mine: 48, unassigned: 4, closed: 12, ..Default::default() };
        let on = AgentRules { require_review: true, assigned_only: true };
        let off = AgentRules { require_review: true, assigned_only: false };
        assert_eq!(c::will_import(64), "64 open issues will be imported");
        assert_eq!(
            c::eligibility(&p, on),
            vec![
                "48 assigned to you are eligible for agents.".to_string(),
                "16 others stay visible, but agents will not pick them up.".to_string(),
            ]
        );
        assert_eq!(
            c::eligibility(&p, off),
            vec![
                "48 assigned to you and 4 unassigned are eligible for agents.".to_string(),
                "12 assigned to others stay visible, but agents will not pick them up.".to_string(),
            ]
        );
        assert_eq!((p.eligible(on), p.eligible(off)), (48, 52));
        assert_eq!(c::closed_note(&p), "12 closed issues are excluded. Imported tasks will follow your review and assignment rules.");
        let capped = ImportPreview { closed: 1000, closed_capped: true, ..p.clone() };
        assert!(c::closed_note(&capped).starts_with("1000+ closed issues are excluded."));
        let one = ImportPreview { open: 1, mine: 1, ..Default::default() };
        assert_eq!(c::will_import(1), "1 open issue will be imported");
        assert_eq!(c::eligibility(&one, on), vec!["1 assigned to you is eligible for agents.".to_string()]);
        assert_eq!(c::import_button(1), "Import 1 issue");
        assert_eq!((c::toast_title(64), c::toast_detail("acme/app")), ("64 issues imported".into(), "acme/app · Synced just now".into()));
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
        assert!(flow.error().unwrap().starts_with("3 local issues aren't on GitHub yet"));
        assert_eq!(flow.primary_label().as_deref(), Some(MOVE_AND_SWITCH), "the move is offered again");
        flow.set_move(false);
        assert_eq!(flow.primary_label().as_deref(), Some(SWITCH_ANYWAY));
        assert_eq!(flow.secondary(), Some(Secondary::Back));
        assert_eq!(flow.primary(), Action::None);
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("a forced retry") };
        assert!(req.force);
        flow.connected(Err(ConnectError::Preflight("gh can't see acme/app".into())));
        assert_eq!(flow.primary_label().as_deref(), Some(TRY_AGAIN), "a failed preflight can't be forced");
        assert!(flow.back());
        assert_eq!((flow.step(), flow.apply()), (Step::ReviewImport, &Apply::Idle));
        assert_eq!(flow.error(), None);
    }

    #[test]
    fn with_local_issues_review_offers_the_move_on_by_default_and_import_moves_first() {
        let mut flow = at_review_with(Tracker::Linear, Ok(3));
        assert!(flow.ready());
        assert!(flow.offers_move() && flow.move_on());
        assert_eq!(flow.move_title(), "Move your 3 local issues to Linear");
        assert_eq!(flow.move_help(), "They're created in ENG first, keeping their status, so nothing is left behind.");
        assert_eq!(flow.subheading(), "Import from ENG.", "the card says what becomes of the local issues");

        // Import: the move first, then the (unforced) switch.
        flow.primary();
        assert_eq!(flow.step(), Step::Importing);
        let choice = flow.wanted().unwrap();
        assert_eq!(flow.due(), Some(FlowRequest::Move(choice.clone())));
        assert_eq!(flow.due(), None, "the switch waits for the move");
        assert_eq!(flow.move_line(), Some((false, "Moving 3 local issues…".to_string())));
        assert!(!flow.ready() && !flow.can_back(), "nothing to press while it moves");
        assert_eq!(flow.secondary(), Some(Secondary::Close));
        flow.moved(Ok(crate::tracker_connect::Moved {
            moved: vec![issue("a", "A"), issue("b", "B"), issue("c", "C")],
            already: 0,
            comments: 2,
        }));
        assert_eq!(flow.move_line(), Some((true, "Moved 3 local issues to Linear".to_string())));
        assert_eq!(
            flow.due(),
            Some(FlowRequest::Connect(ConnectTracker::new(choice, false).with_rules(DEFAULT_RULES))),
            "no --force: nothing is left behind"
        );
        flow.connected(Ok(connected()));
        assert_eq!(flow.error(), None);
        assert_eq!(flow.primary(), Action::ViewBoard);
    }

    #[test]
    fn with_the_move_off_the_switch_is_forced_and_nothing_moves() {
        let mut flow = at_review_with(Tracker::Github, Ok(1));
        assert_eq!(flow.move_title(), "Move your 1 local issue to GitHub");
        assert_eq!(flow.move_help(), "It's created in acme/app first, keeping its status, so nothing is left behind.");
        flow.set_move(false);
        assert!(!flow.move_on());
        assert_eq!(flow.move_help(), "It stays on the local file system and won't be visible to this project.");
        flow.primary();
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("the connection, no move") };
        assert!(req.force, "a forced switch, without a refusal round-trip");
        assert_eq!(flow.move_line(), None);
        // A refusal that can't be overridden retries as forced still.
        flow.connected(Err(ConnectError::Preflight("gh can't see acme/app".into())));
        assert_eq!(flow.primary_label().as_deref(), Some(TRY_AGAIN));
        flow.primary();
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("the retry") };
        assert!(req.force);
    }

    #[test]
    fn without_local_issues_there_is_no_card_and_the_flow_is_unchanged() {
        let mut flow = at_review_with(Tracker::Github, Ok(0));
        assert!(!flow.offers_move());
        assert_eq!(flow.subheading(), "Import from acme/app. Your existing Shelbi tasks stay on this board.");
        flow.primary();
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("the connection") };
        assert!(!req.force);
        assert_eq!(flow.moving(), &Move::Idle);

        // A project not on its local board isn't counted at all.
        let mut flow = TrackerFlow::new();
        flow.load(&github("acme/app"));
        flow.want_local();
        assert_eq!(flow.local(), &LocalIssues::Counted(0));
    }

    #[test]
    fn a_count_that_fails_still_shows_the_card_with_the_move_on() {
        let flow = at_review_with(Tracker::Linear, Err("permission denied".into()));
        assert!(flow.offers_move() && flow.move_on());
        assert_eq!(flow.move_title(), "Move your local issues to Linear");
        assert_eq!(flow.move_help(), "They're created in ENG first, keeping their status, so nothing is left behind.");
    }

    #[test]
    fn a_move_that_stops_says_what_moved_and_try_again_resumes() {
        let mut flow = at_review_with(Tracker::Linear, Ok(3));
        flow.primary();
        let Some(FlowRequest::Move(choice)) = flow.due() else { panic!("the move") };
        flow.moved(Err(crate::tracker_connect::MoveFailed {
            moved: vec![issue("fix-login", "Fix login")],
            already: 0,
            left: vec![issue("add-tests", "Add tests"), issue("rename", "")],
            reason: "Linear refused the write: rate limited".into(),
        }));
        let error = flow.error().unwrap();
        assert_eq!(
            error,
            "Moved 1 of 3 local issues to Linear, then it stopped. Linear refused the write: rate limited. \
             Moved: Fix login. Not moved yet: Add tests, rename. The project wasn't switched. Try again picks up \
             where it stopped."
        );
        assert!(!error.contains("shelbi ") && !error.contains("--"), "no CLI text: {error}");
        assert_eq!(flow.due(), None, "nothing is switched");
        assert_eq!(flow.move_line(), None, "the failure is the step's error");
        assert_eq!((flow.primary_label().as_deref(), flow.secondary()), (Some(TRY_AGAIN), Some(Secondary::Back)));
        assert!(flow.ready());

        // Try again moves again (the move skips what made it).
        flow.primary();
        assert_eq!(flow.due(), Some(FlowRequest::Move(choice.clone())));
        flow.moved(Err(crate::tracker_connect::MoveFailed::before_start("couldn't reach Linear")));
        assert_eq!(
            flow.error().unwrap(),
            "Couldn't move your local issues to Linear. Couldn't reach Linear. The project wasn't switched. Try \
             again picks up where it stopped."
        );
        // Back: Review again, the local board counted again.
        assert!(flow.back());
        assert_eq!((flow.step(), flow.moving(), flow.error()), (Step::ReviewImport, &Move::Idle, None));
        assert_eq!(flow.due(), Some(FlowRequest::LocalIssues));
    }

    #[test]
    fn issues_added_after_review_are_refused_readably_with_the_move_offered_again() {
        // No local issues at Review, three by the switch.
        let mut flow = at_review_with(Tracker::Linear, Ok(0));
        flow.primary();
        let _ = flow.due();
        flow.connected(Err(ConnectError::NotEmpty {
            from: IssueTrackerBackend::FileSystem,
            to: IssueTrackerBackend::Linear,
            issues: 3,
            message: "project `app` has 3 issues on the local file system; ... shelbi issue-store migrate".into(),
        }));
        let error = flow.error().unwrap();
        assert!(error.starts_with("3 local issues aren't on Linear yet"), "{error}");
        assert!(!error.contains("shelbi"), "no CLI text: {error}");
        assert!(flow.offers_move() && flow.move_on());
        assert_eq!(flow.move_title(), "Move your 3 local issues to Linear");
        assert_eq!(flow.primary_label().as_deref(), Some(MOVE_AND_SWITCH));
        flow.primary();
        let choice = flow.wanted().unwrap();
        assert_eq!(flow.due(), Some(FlowRequest::Move(choice.clone())));
        flow.moved(Ok(crate::tracker_connect::Moved { moved: vec![issue("a", "A")], already: 2, comments: 0 }));
        assert_eq!(flow.move_line(), Some((true, "Moved 3 local issues to Linear".to_string())));
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("the switch") };
        assert!(!req.force);

        // Off, it's Switch anyway.
        flow.connected(Err(ConnectError::Unverified { message: "couldn't check".into() }));
        assert!(flow.error().unwrap().starts_with("Some local issues may not be on Linear yet"));
        flow.set_move(false);
        assert_eq!(flow.primary_label().as_deref(), Some(SWITCH_ANYWAY));
        flow.primary();
        let Some(FlowRequest::Connect(req)) = flow.due() else { panic!("the forced switch") };
        assert!(req.force);
    }

    #[test]
    fn the_new_project_flow_adds_file_system_and_ends_at_the_rules() {
        // Connect to an issue tracker keeps its two cards and no Continue.
        let mut flow = TrackerFlow::new();
        flow.load(&local());
        assert_eq!(flow.trackers(), &TRACKERS);
        assert_eq!(flow.primary_label(), None);
        assert!(!flow.is_new_project());

        let mut flow = TrackerFlow::for_new_project(Some("acme/app"), NEW_PROJECT_RULES);
        assert_eq!(flow.trackers(), &NEW_PROJECT_TRACKERS);
        assert_eq!(
            states(&flow),
            [(Tracker::Github, CardState::Connect), (Tracker::Linear, CardState::Connect), (Tracker::FileSystem, CardState::Use)]
        );
        assert_eq!((flow.choice(), flow.primary_label().as_deref()), (Tracker::Github, Some(CONTINUE)));
        assert_eq!(flow.heading(), crate::new_project::ISSUES_TITLE);
        assert_eq!(flow.rules(), NEW_PROJECT_RULES);

        // Continue presses the choice's Connect; while it checks, nothing
        // else can be pressed.
        assert!(flow.advance());
        assert_eq!(states(&flow)[0].1, CardState::Checking);
        assert_eq!(states(&flow)[2].1, CardState::Disabled);
        answer_reads(&mut flow);
        beat(&mut flow);
        assert_eq!((flow.step(), flow.repo()), (Step::ChooseProject, "acme/app"));
        assert!(continue_(&mut flow));
        assert_eq!(flow.step(), Step::AgentRules);
        assert!(flow.advance());
        assert_eq!(flow.step(), Step::AgentRules, "the rules are the last screen");
        assert!(flow.take_complete() && !flow.take_complete());
        assert_eq!(flow.answer(), Some(&github("acme/app")));
        assert_eq!(flow.due(), None, "nothing to preview or connect");

        // File system's Use finishes at once, over the tracker picked before.
        while flow.back() {}
        assert_eq!(flow.step(), Step::Chooser);
        assert!(flow.connect(Tracker::FileSystem));
        assert!(flow.take_complete());
        assert_eq!((flow.answer(), flow.choice()), (Some(&local()), Tracker::FileSystem));
        assert_eq!(flow.authorized(), Some(Tracker::Github), "GitHub stays Connected to switch back");
    }
}
