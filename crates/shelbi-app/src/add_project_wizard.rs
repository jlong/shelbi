//! The Add Project wizard's state machine (`dt-add-project-wizard`).
//!
//! The desktop app adds a project through a small wizard rather than one
//! form: screen 01 picks the project's folder (the name is the folder's), and
//! screen 02 picks where its settings live ([`ConfigMode`]). Create Project
//! then hands the host one [`CreateRequest`] for
//! `shelbi_orchestrator::project_create::add_project`, the engine `shelbi
//! init` and the TUI's form use, and screen 03 ([`Screen::Creating`]) shows
//! the engine's progress as a checklist of [`Stage`]s, from the stage events
//! the host forwards ([`Wizard::stage_started`], [`Wizard::stage_finished`],
//! [`Wizard::stage_failed`]). A failure keeps the checked rows and offers
//! Try Again, which asks for the same project as an [`Attempt::Retry`]. Once
//! it's created the wizard moves on to [`Screen::Created`] with the new
//! project's identity, and waits there for the user before the host opens it.
//!
//! The wizard never touches the filesystem. What the chosen folder is (a
//! repo, a worktree, missing, already a project) takes disk and git, so the
//! wizard only says which text a check is due for ([`Wizard::check_due`]);
//! the host runs it off the UI thread and hands the answer back with
//! [`Wizard::finish_check`], which drops an answer for text that has since
//! changed. Continue waits for an answer for the current text.
//!
//! Keys arrive as [`KeyChord`]s; the path and name fields' own editing
//! (caret, selection, paste) is the toolkit's text input, which reports the
//! new text with [`Wizard::set_path`] / [`Wizard::set_name`].

use std::path::{Path, PathBuf};

use shelbi_core::ConfigMode;
use shelbi_state::keymap::{Key, KeyChord, Mods};

/// The footer's step count ("Step 1 of 3"): the folder, the settings, the
/// creation.
pub const STEPS: usize = 3;

/// The title and the line under it, per screen.
pub const FOLDER_TITLE: &str = "Add project";
pub const FOLDER_INTRO: &str =
    "Select the folder that contains the project you want to work on with Shelbi.";
pub const SETTINGS_TITLE: &str = "Where should settings live?";
pub const SETTINGS_INTRO: &str =
    "Choose how Shelbi stores settings, agent instructions, and workflows for this project.";
pub const CREATING_TITLE: &str = "Setting your project up for Shelbi";
pub const CREATED_TITLE: &str = "Your project is ready";
/// Screen 03's button after a failure.
pub const RETRY_LABEL: &str = "Try Again";
/// The created screen's button.
pub const OPEN_LABEL: &str = "Open Project";

/// Screen 01's box before a folder is chosen.
pub const CHOOSE_PROMPT: &str = "Choose your local project folder.";

/// The two settings cards: title and description.
pub const IN_REPO_TITLE: &str = "In the project";
pub const IN_REPO_BODY: &str =
    "Keep settings with your code and share them with your team through Git.";
pub const GLOBAL_TITLE: &str = "Outside the project";
pub const GLOBAL_BODY: &str =
    "Keep settings personal to you on this Mac, separate from your repository.";

/// Where the wizard was opened from, which decides screen 01's Back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// No project exists yet: the app opened on the wizard. Nothing comes
    /// before screen 01, so its Back is disabled (closing the window quits).
    FirstLaunch,
    /// Opened from a project window: screen 01's Back closes the wizard,
    /// leaving that window as it was.
    Workspace,
}

/// The wizard's screens (the Figma frames 01, 02 and 03, then the created
/// project).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Folder,
    Settings,
    /// 03: the creation's checklist.
    Creating,
    /// 04: the project exists; the user moves on from here
    /// ([`Action::Open`]).
    Created,
}

impl Screen {
    /// The footer's "Step N of 3".
    pub fn step(self) -> usize {
        match self {
            Screen::Folder => 1,
            Screen::Settings => 2,
            Screen::Creating | Screen::Created => 3,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Screen::Folder => FOLDER_TITLE,
            Screen::Settings => SETTINGS_TITLE,
            Screen::Creating => CREATING_TITLE,
            Screen::Created => CREATED_TITLE,
        }
    }
}

/// Screen 03's rows: the creation engine's stages
/// (`project_create::ScaffoldStage`), in the order they run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Stage {
    /// The registration, config and workspace settings.
    Config,
    /// The default agents.
    Agents,
    /// The Task and Subtask workflows and statuses, and the rest of the
    /// scaffold.
    Workflows,
}

impl Stage {
    pub const ALL: [Stage; 3] = [Stage::Config, Stage::Agents, Stage::Workflows];

    /// The row's label.
    pub fn label(self) -> &'static str {
        match self {
            Stage::Config => "Writing Shelbi config",
            Stage::Agents => "Creating agents",
            Stage::Workflows => "Adding workflows",
        }
    }
}

/// How a checklist row shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowState {
    /// Not started: a muted circle and label.
    Pending,
    /// Running (one at most): the spinner and an emphasized label.
    Active,
    /// Succeeded: a green check.
    Done,
    /// Where the creation stopped.
    Failed,
}

/// The created project, as the creation engine reported it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Created {
    /// Its id (the settings folder's name).
    pub slug: String,
    /// The name it was created with.
    pub name: String,
    /// What the engine warned about while creating it (the commit guard's
    /// disclosure, a hook left alone), in order.
    pub warnings: Vec<String>,
}

/// The footer line: "Step 2 of 3".
pub fn step_label(screen: Screen) -> String {
    format!("Step {} of {STEPS}", screen.step())
}

/// What the chosen folder is under version control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repo {
    /// A Git repository (its own `.git` directory, or inside one).
    Git,
    /// A Git worktree (its `.git` is a file pointing at the main repo).
    Worktree,
    /// Not under Git: allowed, with the shared warning.
    NotGit,
}

/// Why the chosen folder can't be added (Continue stays disabled).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Problem {
    /// The path field is empty.
    Empty,
    NotFound,
    NotDirectory,
    /// The name has no letters or digits to build a project id from.
    NoName,
    /// A project with this id is already registered.
    Taken { slug: String },
    /// The check itself failed (the Shelbi home is unreadable).
    Unreadable(String),
}

/// What the chosen folder turned out to be: the host's answer to a check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderCheck {
    /// The folder, resolved (`~` expanded, made absolute).
    pub path: PathBuf,
    /// The project's name: the folder's, or the one typed after a clash.
    pub name: Option<String>,
    /// The id `name` slugifies to (the settings folder's name).
    pub slug: Option<String>,
    /// `None` while the folder doesn't exist as a directory.
    pub repo: Option<Repo>,
    pub problem: Option<Problem>,
    /// Where settings go in the project (`<folder>/.shelbi`).
    pub in_repo_dir: PathBuf,
    /// Where settings go outside it (`<Shelbi home>/projects/<id>`), when
    /// there is an id.
    pub global_dir: Option<PathBuf>,
}

impl FolderCheck {
    /// The folder can be added.
    pub fn usable(&self) -> bool {
        self.problem.is_none()
    }

    /// A check that didn't get as far as the folder: `problem` for `path`.
    pub fn failed(path: PathBuf, problem: Problem) -> Self {
        Self {
            in_repo_dir: path.join(".shelbi"),
            path,
            name: None,
            slug: None,
            repo: None,
            problem: Some(problem),
            global_dir: None,
        }
    }

    /// The box's line under the name: what was found, or what's wrong.
    pub fn message(&self) -> String {
        match &self.problem {
            Some(Problem::Empty) => "Enter the path to your project folder.".into(),
            Some(Problem::NotFound) => "This folder doesn't exist.".into(),
            Some(Problem::NotDirectory) => "This path is a file, not a folder.".into(),
            Some(Problem::NoName) => {
                "This name has no letters or digits to build a project id from. Enter a name for the project."
                    .into()
            }
            Some(Problem::Taken { slug }) => format!(
                "A project named “{slug}” already exists. Enter a different name for this one."
            ),
            Some(Problem::Unreadable(e)) => format!("Couldn't check this folder: {e}"),
            None => match self.repo {
                Some(Repo::Git) => {
                    "Git repository detected. Shelbi will use this folder as your project.".into()
                }
                Some(Repo::Worktree) => {
                    "Git worktree detected. Shelbi will use this folder as your project.".into()
                }
                // The shared warning (`Detected::NotGitRepo`): allowed, flagged.
                Some(Repo::NotGit) | None => {
                    "Not a Git repository. Shelbi expects Git, but will continue.".into()
                }
            },
        }
    }

    /// How [`Self::message`] reads: good news, a warning, or an error.
    pub fn tone(&self) -> Tone {
        match (&self.problem, self.repo) {
            (Some(_), _) => Tone::Error,
            (None, Some(Repo::Git | Repo::Worktree)) => Tone::Normal,
            (None, _) => Tone::Warning,
        }
    }

    /// A name typed into the clash field would fix this.
    fn wants_name(&self) -> bool {
        matches!(self.problem, Some(Problem::Taken { .. } | Problem::NoName))
    }
}

/// How a status line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Warning,
    Error,
}

/// The text a check is for: the path field, and the typed name if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckKey {
    pub path: String,
    pub name: Option<String>,
}

/// The focusable controls, each screen's in Tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Screen 01's path field (shown once a folder is chosen).
    Path,
    Browse,
    /// Screen 01's name field (shown when the folder's name clashes).
    Name,
    /// Screen 02's cards.
    Card(ConfigMode),
    Back,
    /// Continue, or Create Project.
    Primary,
}

/// What the host should do after a key or a click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Stay,
    /// Open the native folder picker; its answer goes to
    /// [`Wizard::folder_picked`].
    Browse,
    /// Close the wizard (before Create Project, nothing was written).
    Close,
    /// Run the creation engine for this request (once per press: a running
    /// creation takes no second one).
    Create(CreateRequest),
    /// Open the created project and close the wizard.
    Open(Created),
}

/// Which run of one request a [`CreateRequest`] is
/// (`project_create::Attempt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    New,
    /// Try Again after a failure: finish the same project.
    Retry,
}

/// The values Create Project hands the creation engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    pub name: String,
    /// The resolved folder.
    pub root: String,
    pub mode: ConfigMode,
    pub attempt: Attempt,
}

/// Screen 03's state: which stages finished, which is running, where it
/// stopped.
#[derive(Debug, Clone, Default)]
struct Progress {
    /// The request being (or last) created; Try Again repeats it.
    request: Option<CreateRequest>,
    /// The engine is working on it.
    running: bool,
    done: Vec<Stage>,
    active: Option<Stage>,
    /// The stage that failed and why.
    failed: Option<(Stage, String)>,
    warnings: Vec<String>,
}

/// The wizard's values and where it is.
#[derive(Debug, Clone)]
pub struct Wizard {
    origin: Origin,
    screen: Screen,
    focus: Focus,
    /// The path field's text, as shown (`~/…` under the home folder).
    path: String,
    /// A folder was chosen (screen 01 shows the field rather than the
    /// prompt from then on, even if the field is cleared).
    chosen: bool,
    /// The name typed after a clash; `None` uses the folder's name.
    name: Option<String>,
    mode: ConfigMode,
    /// The latest check, and the text it was for.
    check: Option<(CheckKey, FolderCheck)>,
    /// The text a check is running for.
    in_flight: Option<CheckKey>,
    progress: Progress,
    created: Option<Created>,
    home: Option<PathBuf>,
}

impl Wizard {
    /// A fresh wizard: no folder, settings outside the project (the form's
    /// default before it). `home` abbreviates a picked folder to `~/…`.
    pub fn new(origin: Origin, home: Option<PathBuf>) -> Self {
        Self {
            origin,
            screen: Screen::Folder,
            focus: Focus::Browse,
            path: String::new(),
            chosen: false,
            name: None,
            mode: ConfigMode::Global,
            check: None,
            in_flight: None,
            progress: Progress::default(),
            created: None,
            home,
        }
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }

    pub fn screen(&self) -> Screen {
        self.screen
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn chosen(&self) -> bool {
        self.chosen
    }

    /// The clash field's text, while it shows.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn mode(&self) -> ConfigMode {
        self.mode
    }

    /// Screen 03: how `stage`'s row shows.
    pub fn row(&self, stage: Stage) -> RowState {
        let p = &self.progress;
        if p.failed.as_ref().is_some_and(|(s, _)| *s == stage) {
            RowState::Failed
        } else if p.active == Some(stage) {
            RowState::Active
        } else if p.done.contains(&stage) {
            RowState::Done
        } else {
            RowState::Pending
        }
    }

    /// The creation engine is working.
    pub fn running(&self) -> bool {
        self.progress.running
    }

    /// Screen 03 after a failure: the stage it stopped in and why.
    pub fn failure(&self) -> Option<(Stage, &str)> {
        self.progress.failed.as_ref().map(|(s, m)| (*s, m.as_str()))
    }

    /// What the engine warned about during this creation.
    pub fn warnings(&self) -> &[String] {
        &self.progress.warnings
    }

    /// The project, once created.
    pub fn created(&self) -> Option<&Created> {
        self.created.as_ref()
    }

    /// The text the next check is for.
    pub fn key(&self) -> CheckKey {
        CheckKey {
            path: self.path.trim().to_string(),
            name: self.name.as_ref().map(|n| n.trim().to_string()),
        }
    }

    /// The check for the current text, once it has landed.
    pub fn check(&self) -> Option<&FolderCheck> {
        self.check.as_ref().filter(|(k, _)| *k == self.key()).map(|(_, c)| c)
    }

    /// A check is due or running for the current text.
    pub fn checking(&self) -> bool {
        self.chosen && self.check().is_none()
    }

    /// The text a check is due for: `Some` when a folder is chosen, the
    /// current text has no answer, and none is running for it. The host
    /// calls [`Self::begin_check`], runs it, then [`Self::finish_check`].
    pub fn check_due(&self) -> Option<CheckKey> {
        if !self.chosen || self.check().is_some() {
            return None;
        }
        let key = self.key();
        (self.in_flight.as_ref() != Some(&key)).then_some(key)
    }

    pub fn begin_check(&mut self, key: CheckKey) {
        self.in_flight = Some(key);
    }

    /// A check landed. An answer for text that has since changed is
    /// dropped (the host asks [`Self::check_due`] again). A clash opens the
    /// name field, seeded with the folder's name.
    pub fn finish_check(&mut self, key: CheckKey, check: FolderCheck) {
        if self.in_flight.as_ref() == Some(&key) {
            self.in_flight = None;
        }
        if key != self.key() {
            return;
        }
        if self.name.is_none() && check.wants_name() {
            let seed = check
                .name
                .clone()
                .or_else(|| folder_name(&check.path))
                .unwrap_or_default();
            self.name = Some(seed);
            // The seeded name is the same text the check was for, so this
            // answer stands for it.
            let key = self.key();
            self.check = Some((key, check));
            return;
        }
        self.check = Some((key, check));
    }

    /// Continue / Create Project can be pressed.
    pub fn primary_enabled(&self) -> bool {
        match self.screen {
            Screen::Folder => self.check().is_some_and(FolderCheck::usable),
            Screen::Settings => self.check().is_some_and(FolderCheck::usable),
            // Try Again, once a creation has failed.
            Screen::Creating => !self.progress.running && self.progress.failed.is_some(),
            Screen::Created => self.created.is_some(),
        }
    }

    /// Back can be pressed.
    pub fn back_enabled(&self) -> bool {
        match self.screen {
            Screen::Folder => self.origin == Origin::Workspace,
            Screen::Settings => true,
            Screen::Creating | Screen::Created => false,
        }
    }

    /// The path field's text changed (typed, pasted). A different folder
    /// drops a typed name.
    pub fn set_path(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text == self.path {
            return;
        }
        self.path = text;
        self.chosen = true;
        self.name = None;
    }

    /// The name field's text changed.
    pub fn set_name(&mut self, text: impl Into<String>) {
        if self.name.is_some() {
            self.name = Some(text.into());
        }
    }

    /// The folder picker closed: `Some` chose a folder, `None` cancelled
    /// (the previous choice stands).
    pub fn folder_picked(&mut self, path: Option<&Path>) {
        if let Some(path) = path {
            let shown = tilde(path, self.home.as_deref());
            self.set_path(shown);
        }
    }

    /// The controls Tab visits on this screen, in order (disabled buttons
    /// are skipped).
    pub fn tab_order(&self) -> Vec<Focus> {
        let mut order = Vec::new();
        match self.screen {
            Screen::Folder => {
                if self.chosen {
                    order.push(Focus::Path);
                }
                order.push(Focus::Browse);
                if self.name.is_some() {
                    order.push(Focus::Name);
                }
            }
            Screen::Settings => {
                order.push(Focus::Card(ConfigMode::InRepo));
                order.push(Focus::Card(ConfigMode::Global));
            }
            Screen::Creating | Screen::Created => {}
        }
        if self.back_enabled() {
            order.push(Focus::Back);
        }
        if self.primary_enabled() {
            order.push(Focus::Primary);
        }
        order
    }

    /// Move focus to `focus` (a click on a control).
    pub fn set_focus(&mut self, focus: Focus) {
        self.focus = focus;
    }

    /// Keep focus on a control this screen still offers: a button that just
    /// went disabled hands it to the screen's first control.
    pub fn settle_focus(&mut self) {
        let order = self.tab_order();
        if !order.contains(&self.focus) {
            if let Some(first) = order.first() {
                self.focus = *first;
            }
        }
    }

    fn step_focus(&mut self, forward: bool) {
        let order = self.tab_order();
        if order.is_empty() {
            return;
        }
        let at = order.iter().position(|f| *f == self.focus);
        let next = match (at, forward) {
            (Some(i), true) => (i + 1) % order.len(),
            (Some(i), false) => (i + order.len() - 1) % order.len(),
            (None, true) => 0,
            (None, false) => order.len() - 1,
        };
        self.focus = order[next];
    }

    /// Pick where settings live (a click on a card, or Space / an arrow on
    /// one).
    pub fn select_mode(&mut self, mode: ConfigMode) {
        if self.screen == Screen::Settings {
            self.mode = mode;
            self.focus = Focus::Card(mode);
        }
    }

    /// Back: screen 02 returns to the chosen folder; screen 01 closes the
    /// wizard when it was opened from a project window.
    pub fn back(&mut self) -> Action {
        if !self.back_enabled() {
            return Action::Stay;
        }
        match self.screen {
            Screen::Folder => Action::Close,
            Screen::Settings => {
                self.screen = Screen::Folder;
                self.focus = Focus::Primary;
                self.settle_focus();
                Action::Stay
            }
            Screen::Creating | Screen::Created => Action::Stay,
        }
    }

    /// Continue, Create Project, Try Again or Open Project: the screen's
    /// primary action, when enabled. Create Project moves to
    /// [`Screen::Creating`] and Try Again runs again, and neither can be
    /// pressed while the engine works, so a request is never sent twice.
    pub fn primary(&mut self) -> Action {
        if !self.primary_enabled() {
            return Action::Stay;
        }
        match self.screen {
            Screen::Folder => {
                self.screen = Screen::Settings;
                self.focus = Focus::Card(self.mode);
                Action::Stay
            }
            Screen::Settings => {
                let Some(check) = self.check() else {
                    return Action::Stay;
                };
                let request = CreateRequest {
                    name: check.name.clone().unwrap_or_default(),
                    root: check.path.display().to_string(),
                    mode: self.mode,
                    attempt: Attempt::New,
                };
                self.screen = Screen::Creating;
                self.focus = Focus::Primary;
                self.start(request)
            }
            Screen::Creating => self.retry(),
            Screen::Created => match &self.created {
                Some(created) => Action::Open(created.clone()),
                None => Action::Stay,
            },
        }
    }

    /// Run `request`: a fresh checklist.
    fn start(&mut self, request: CreateRequest) -> Action {
        self.progress = Progress {
            request: Some(request.clone()),
            running: true,
            ..Progress::default()
        };
        Action::Create(request)
    }

    /// Try Again: the failed request again, as a retry. The checklist starts
    /// over, and the engine checks each stage off again as it confirms it
    /// (stages that finished before go quickly: their files are there).
    pub fn retry(&mut self) -> Action {
        if self.screen != Screen::Creating || self.progress.running || self.progress.failed.is_none() {
            return Action::Stay;
        }
        let Some(mut request) = self.progress.request.clone() else {
            return Action::Stay;
        };
        request.attempt = Attempt::Retry;
        self.start(request)
    }

    /// The engine started `stage`: it's the one active row.
    pub fn stage_started(&mut self, stage: Stage) {
        if self.creating() {
            self.progress.done.retain(|s| *s != stage);
            self.progress.active = Some(stage);
        }
    }

    /// The engine finished `stage`: its row is checked.
    pub fn stage_finished(&mut self, stage: Stage) {
        if self.creating() {
            if self.progress.active == Some(stage) {
                self.progress.active = None;
            }
            if !self.progress.done.contains(&stage) {
                self.progress.done.push(stage);
            }
        }
    }

    /// The engine stopped in `stage` with `message`.
    pub fn stage_failed(&mut self, stage: Stage, message: impl Into<String>) {
        if self.creating() {
            self.progress.active = None;
            self.progress.failed = Some((stage, message.into()));
        }
    }

    /// The engine warned about something (shown with the created project),
    /// as [`prose`].
    pub fn warning(&mut self, line: &str) {
        if self.creating() {
            let line = prose(line);
            if !self.progress.warnings.contains(&line) {
                self.progress.warnings.push(line);
            }
        }
    }

    /// The engine is running on screen 03 (events for anything else are
    /// stale and dropped).
    fn creating(&self) -> bool {
        self.screen == Screen::Creating && self.progress.running
    }

    /// The engine created the project `slug`: every row is checked and the
    /// wizard moves on to [`Screen::Created`].
    pub fn creation_succeeded(&mut self, slug: impl Into<String>) {
        if !self.creating() {
            return;
        }
        let name = self.progress.request.as_ref().map(|r| r.name.clone()).unwrap_or_default();
        self.progress.running = false;
        self.progress.active = None;
        self.progress.done = Stage::ALL.to_vec();
        self.created = Some(Created {
            slug: slug.into(),
            name,
            warnings: self.progress.warnings.clone(),
        });
        self.screen = Screen::Created;
        self.focus = Focus::Primary;
    }

    /// Activate the focused control (Space, or a click).
    pub fn activate(&mut self, focus: Focus) -> Action {
        self.focus = focus;
        match focus {
            Focus::Browse if self.screen == Screen::Folder => Action::Browse,
            Focus::Card(mode) => {
                self.select_mode(mode);
                Action::Stay
            }
            Focus::Back => self.back(),
            Focus::Primary => self.primary(),
            _ => Action::Stay,
        }
    }

    /// Feed one key. `in_text` says a text field has the keyboard, so Space
    /// and the arrows are its (it already took them); Tab, Return and Esc are
    /// the wizard's everywhere.
    pub fn handle_key(&mut self, chord: KeyChord, in_text: bool) -> Action {
        let bare = chord.mods == Mods::NONE;
        match chord.code {
            Key::Tab if bare => {
                self.step_focus(true);
                Action::Stay
            }
            Key::BackTab => {
                self.step_focus(false);
                Action::Stay
            }
            Key::Tab if chord.mods == Mods::SHIFT => {
                self.step_focus(false);
                Action::Stay
            }
            // Return is the default button's, wherever focus is.
            Key::Enter if bare => self.primary(),
            // While the engine works it can't be stopped halfway; once
            // it's done or failed, Esc closes.
            Key::Esc if bare => {
                if self.progress.running {
                    Action::Stay
                } else {
                    Action::Close
                }
            }
            Key::Char(' ') if bare && !in_text => self.activate(self.focus),
            Key::Up | Key::Down | Key::Left | Key::Right if bare && !in_text => {
                if let Focus::Card(_) = self.focus {
                    let other = match self.mode {
                        ConfigMode::InRepo => ConfigMode::Global,
                        ConfigMode::Global => ConfigMode::InRepo,
                    };
                    self.select_mode(other);
                }
                Action::Stay
            }
            _ => Action::Stay,
        }
    }

    /// The creation failed with `message`: screen 03 keeps its checked rows
    /// and marks where it stopped, and offers Try Again. A stage the engine
    /// already reported failed keeps its own message; a failure before any
    /// stage (the request didn't validate) lands on the first unchecked row.
    pub fn creation_failed(&mut self, message: impl Into<String>) {
        if !self.creating() {
            return;
        }
        let p = &mut self.progress;
        p.running = false;
        if p.failed.is_none() {
            let stage = p
                .active
                .take()
                .or_else(|| Stage::ALL.into_iter().find(|s| !p.done.contains(s)))
                .unwrap_or(Stage::Workflows);
            p.failed = Some((stage, message.into()));
        }
        p.active = None;
        self.focus = Focus::Primary;
    }

    /// Screen 02's line under a card: where that mode keeps the settings,
    /// from the chosen folder and the real Shelbi home.
    pub fn storage_path(&self, mode: ConfigMode) -> Option<String> {
        let check = self.check()?;
        match mode {
            ConfigMode::InRepo => {
                let folder = folder_name(&check.path)?;
                Some(format!("{folder}/.shelbi"))
            }
            ConfigMode::Global => check
                .global_dir
                .as_deref()
                .map(|d| tilde(d, self.home.as_deref())),
        }
    }
}

/// `path` with `home` shortened to `~` (`~/Projects/acme`), as the path
/// field shows it; anywhere else as is.
pub fn tilde(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// An engine warning as one paragraph: the CLI's `shelbi:` prefixes and its
/// terminal line breaks taken out, the first letter capitalized.
pub fn prose(warning: &str) -> String {
    let text = warning
        .lines()
        .map(|l| l.trim().strip_prefix("shelbi:").unwrap_or(l.trim()).trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => text,
    }
}

/// A folder's own name (its last component), if it has one.
pub fn folder_name(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::test_keys::{ch, key};

    fn home() -> PathBuf {
        PathBuf::from("/Users/me")
    }

    fn wizard(origin: Origin) -> Wizard {
        Wizard::new(origin, Some(home()))
    }

    fn found(path: &str, name: &str, repo: Repo) -> FolderCheck {
        let path = PathBuf::from(path);
        FolderCheck {
            in_repo_dir: path.join(".shelbi"),
            path,
            name: Some(name.into()),
            slug: Some(name.to_lowercase().replace(' ', "-")),
            repo: Some(repo),
            problem: None,
            global_dir: Some(home().join(".shelbi/projects").join(name.to_lowercase())),
        }
    }

    /// Answer the due check with `check`.
    fn answer(w: &mut Wizard, check: FolderCheck) {
        let key = w.check_due().expect("a check is due");
        w.begin_check(key.clone());
        assert_eq!(w.check_due(), None, "one check at a time per text");
        w.finish_check(key, check);
    }

    fn chosen(origin: Origin) -> Wizard {
        let mut w = wizard(origin);
        w.folder_picked(Some(Path::new("/Users/me/Projects/acme")));
        answer(&mut w, found("/Users/me/Projects/acme", "acme", Repo::Git));
        w
    }

    #[test]
    fn it_starts_with_no_folder_and_continue_disabled() {
        let w = wizard(Origin::Workspace);
        assert_eq!(w.screen(), Screen::Folder);
        assert!(!w.chosen());
        assert_eq!(w.path(), "");
        assert_eq!(w.check_due(), None, "nothing to check before a folder is chosen");
        assert!(!w.primary_enabled());
        assert_eq!(step_label(w.screen()), "Step 1 of 3");
        assert_eq!(w.tab_order(), [Focus::Browse, Focus::Back]);
        assert_eq!(w.focus(), Focus::Browse);
    }

    #[test]
    fn a_picked_folder_shows_under_home_as_tilde_and_is_checked() {
        let mut w = wizard(Origin::Workspace);
        assert_eq!(w.activate(Focus::Browse), Action::Browse);
        w.folder_picked(Some(Path::new("/Users/me/My Projects/web app")));
        assert_eq!(w.path(), "~/My Projects/web app", "spaces kept, home shortened");
        assert!(w.checking());
        assert_eq!(
            w.check_due(),
            Some(CheckKey { path: "~/My Projects/web app".into(), name: None })
        );
        answer(&mut w, found("/Users/me/My Projects/web app", "web app", Repo::Git));
        assert!(w.primary_enabled());
        assert_eq!(w.tab_order(), [Focus::Path, Focus::Browse, Focus::Back, Focus::Primary]);
    }

    #[test]
    fn cancelling_the_picker_keeps_the_previous_folder() {
        let mut w = chosen(Origin::Workspace);
        w.folder_picked(None);
        assert_eq!(w.path(), "~/Projects/acme");
        assert!(w.primary_enabled(), "the earlier check still stands");
    }

    #[test]
    fn a_stale_check_is_dropped_and_the_new_text_rechecked() {
        let mut w = wizard(Origin::Workspace);
        w.set_path("~/old");
        let old = w.check_due().unwrap();
        w.begin_check(old.clone());
        // The user changes the path while the old check runs.
        w.set_path("~/new");
        assert_eq!(w.check_due().unwrap().path, "~/new", "the new text is due at once");
        w.finish_check(old, found("/Users/me/old", "old", Repo::Git));
        assert!(w.check().is_none(), "the old answer is dropped");
        assert!(!w.primary_enabled());
        answer(&mut w, FolderCheck::failed(PathBuf::from("/Users/me/new"), Problem::NotFound));
        assert_eq!(w.check().unwrap().message(), "This folder doesn't exist.");
        assert_eq!(w.check().unwrap().tone(), Tone::Error);
        assert!(!w.primary_enabled(), "a missing folder can't continue");
    }

    #[test]
    fn a_non_git_folder_warns_but_continues_and_a_worktree_is_named() {
        let mut w = wizard(Origin::Workspace);
        w.set_path("/tmp/scratch");
        answer(&mut w, found("/tmp/scratch", "scratch", Repo::NotGit));
        assert!(w.primary_enabled());
        assert_eq!(w.check().unwrap().tone(), Tone::Warning);
        assert!(w.check().unwrap().message().contains("Not a Git repository"));

        w.set_path("/tmp/wt/alpha");
        answer(&mut w, found("/tmp/wt/alpha", "alpha", Repo::Worktree));
        assert!(w.primary_enabled());
        assert!(w.check().unwrap().message().starts_with("Git worktree detected"));
    }

    #[test]
    fn a_name_clash_opens_the_name_field_and_a_new_name_is_rechecked() {
        let mut w = wizard(Origin::Workspace);
        w.set_path("~/src/acme");
        let mut taken = found("/Users/me/src/acme", "acme", Repo::Git);
        taken.problem = Some(Problem::Taken { slug: "acme".into() });
        answer(&mut w, taken);
        assert_eq!(w.name(), Some("acme"), "seeded with the folder's name");
        assert!(w.check().is_some(), "the clash stands for the seeded name");
        assert!(w.check().unwrap().message().contains("“acme” already exists"));
        assert!(!w.primary_enabled());
        assert!(w.tab_order().contains(&Focus::Name));

        w.set_name("acme two");
        assert_eq!(w.check_due().unwrap().name.as_deref(), Some("acme two"));
        answer(&mut w, found("/Users/me/src/acme", "acme two", Repo::Git));
        assert!(w.primary_enabled());

        // Another folder drops the typed name.
        w.set_path("~/src/other");
        assert_eq!(w.name(), None);
    }

    #[test]
    fn settings_cards_select_and_persist_through_back_and_continue() {
        let mut w = chosen(Origin::Workspace);
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Stay);
        assert_eq!(w.screen(), Screen::Settings);
        assert_eq!(step_label(w.screen()), "Step 2 of 3");
        assert_eq!(w.mode(), ConfigMode::Global);
        assert_eq!(w.focus(), Focus::Card(ConfigMode::Global));

        // An arrow on the cards moves the choice.
        w.handle_key(key(Key::Up), false);
        assert_eq!(w.mode(), ConfigMode::InRepo);
        assert_eq!(w.focus(), Focus::Card(ConfigMode::InRepo));

        assert_eq!(w.back(), Action::Stay);
        assert_eq!(w.screen(), Screen::Folder);
        assert_eq!(w.path(), "~/Projects/acme", "the folder survives Back");
        assert!(w.primary_enabled());

        w.primary();
        assert_eq!(w.screen(), Screen::Settings);
        assert_eq!(w.mode(), ConfigMode::InRepo, "the card survives Back / Continue");

        // A click on the other card.
        w.activate(Focus::Card(ConfigMode::Global));
        assert_eq!(w.mode(), ConfigMode::Global);
    }

    #[test]
    fn storage_paths_come_from_the_folder_and_the_shelbi_home() {
        let mut w = chosen(Origin::Workspace);
        w.primary();
        assert_eq!(w.storage_path(ConfigMode::InRepo).as_deref(), Some("acme/.shelbi"));
        assert_eq!(
            w.storage_path(ConfigMode::Global).as_deref(),
            Some("~/.shelbi/projects/acme")
        );
    }

    #[test]
    fn create_is_requested_once() {
        let mut w = chosen(Origin::Workspace);
        w.primary();
        w.select_mode(ConfigMode::InRepo);
        let request = CreateRequest {
            name: "acme".into(),
            root: "/Users/me/Projects/acme".into(),
            mode: ConfigMode::InRepo,
            attempt: Attempt::New,
        };
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Create(request));
        assert_eq!(w.screen(), Screen::Creating);
        assert!(w.running());
        assert_eq!(step_label(w.screen()), "Step 3 of 3");
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Stay, "no second submit");
        assert_eq!(w.activate(Focus::Primary), Action::Stay);
        assert_eq!(w.retry(), Action::Stay, "nothing to retry while it runs");
        assert_eq!(w.handle_key(key(Key::Esc), false), Action::Stay, "Esc waits for the creation");
        assert_eq!(w.back(), Action::Stay);
        assert!(w.tab_order().is_empty());
    }

    /// The wizard on screen 03, its engine running.
    fn creating() -> Wizard {
        let mut w = chosen(Origin::Workspace);
        w.primary();
        assert!(matches!(w.primary(), Action::Create(_)));
        w
    }

    fn rows(w: &Wizard) -> Vec<RowState> {
        Stage::ALL.iter().map(|s| w.row(*s)).collect()
    }

    #[test]
    fn the_checklist_follows_the_engines_stages_one_at_a_time() {
        use RowState::*;
        let mut w = creating();
        assert_eq!(rows(&w), [Pending, Pending, Pending], "nothing runs until the engine says so");
        w.stage_started(Stage::Config);
        assert_eq!(rows(&w), [Active, Pending, Pending]);
        w.stage_finished(Stage::Config);
        assert_eq!(rows(&w), [Done, Pending, Pending], "checked only when it finishes");
        w.stage_started(Stage::Agents);
        assert_eq!(rows(&w), [Done, Active, Pending]);
        w.warning("shelbi: installed a guard");
        w.warning("shelbi: installed a guard");
        w.stage_finished(Stage::Agents);
        w.stage_started(Stage::Workflows);
        assert_eq!(rows(&w), [Done, Done, Active]);
        w.stage_finished(Stage::Workflows);
        assert_eq!(rows(&w), [Done, Done, Done]);
        assert_eq!(w.screen(), Screen::Creating, "the engine's answer moves on, not the last check");

        w.creation_succeeded("acme");
        assert_eq!(w.screen(), Screen::Created);
        let created = Created {
            slug: "acme".into(),
            name: "acme".into(),
            warnings: vec!["Installed a guard".into()],
        };
        assert_eq!(w.created(), Some(&created));
        // The created screen waits for the user before the project opens.
        assert_eq!(w.tab_order(), [Focus::Primary]);
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Open(created));
        // A late event changes nothing.
        w.stage_failed(Stage::Config, "late");
        assert_eq!(w.failure(), None);
    }

    #[test]
    fn a_failure_keeps_the_checked_rows_and_try_again_retries_the_same_request() {
        use RowState::*;
        let mut w = creating();
        w.stage_started(Stage::Config);
        w.stage_finished(Stage::Config);
        w.stage_started(Stage::Agents);
        w.stage_failed(Stage::Agents, "agents is a file");
        w.creation_failed("Add project failed: agents is a file");
        assert_eq!(rows(&w), [Done, Failed, Pending]);
        assert_eq!(w.failure(), Some((Stage::Agents, "agents is a file")), "the stage's own reason");
        assert!(!w.running());
        assert_eq!(w.screen(), Screen::Creating, "it stays on the checklist");
        assert_eq!(w.path(), "~/Projects/acme", "the folder is kept");
        assert_eq!(w.mode(), ConfigMode::Global);
        assert_eq!(w.tab_order(), [Focus::Primary], "Try Again, and no Back");

        let again = CreateRequest {
            name: "acme".into(),
            root: "/Users/me/Projects/acme".into(),
            mode: ConfigMode::Global,
            attempt: Attempt::Retry,
        };
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Create(again));
        assert!(w.running());
        assert_eq!(rows(&w), [Pending, Pending, Pending], "the engine confirms each stage again");
        assert_eq!(w.failure(), None);
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Stay, "one retry at a time");
        w.stage_started(Stage::Config);
        w.stage_finished(Stage::Config);
        w.stage_started(Stage::Agents);
        w.stage_finished(Stage::Agents);
        w.stage_started(Stage::Workflows);
        w.stage_finished(Stage::Workflows);
        w.creation_succeeded("acme");
        assert_eq!(w.screen(), Screen::Created);
    }

    #[test]
    fn a_failure_before_any_stage_lands_on_the_first_unchecked_row() {
        let mut w = creating();
        w.creation_failed("a project already lives at `acme`");
        assert_eq!(w.failure(), Some((Stage::Config, "a project already lives at `acme`")));
        assert_eq!(w.row(Stage::Config), RowState::Failed);
        // Esc closes once nothing runs.
        assert_eq!(w.handle_key(key(Key::Esc), false), Action::Close);

        let mut w = creating();
        w.stage_started(Stage::Config);
        w.creation_failed("lost");
        assert_eq!(w.failure(), Some((Stage::Config, "lost")), "the running stage");
    }

    #[test]
    fn screen_one_back_closes_from_a_workspace_and_is_disabled_on_first_launch() {
        let mut w = wizard(Origin::Workspace);
        assert!(w.back_enabled());
        assert_eq!(w.back(), Action::Close);

        let mut w = wizard(Origin::FirstLaunch);
        assert!(!w.back_enabled());
        assert_eq!(w.back(), Action::Stay);
        assert_eq!(w.tab_order(), [Focus::Browse], "a disabled Back takes no focus");
        // Esc still closes (on first launch that quits the app).
        assert_eq!(w.handle_key(key(Key::Esc), false), Action::Close);
    }

    #[test]
    fn tab_cycles_the_screens_controls_and_space_activates() {
        let mut w = chosen(Origin::Workspace);
        w.set_focus(Focus::Path);
        w.handle_key(key(Key::Tab), true);
        assert_eq!(w.focus(), Focus::Browse);
        w.handle_key(key(Key::Tab), false);
        assert_eq!(w.focus(), Focus::Back);
        w.handle_key(key(Key::Tab), false);
        assert_eq!(w.focus(), Focus::Primary);
        w.handle_key(key(Key::Tab), false);
        assert_eq!(w.focus(), Focus::Path, "it wraps");
        w.handle_key(key(Key::BackTab), true);
        assert_eq!(w.focus(), Focus::Primary);

        // Space in the path field types a space (the field took it).
        w.set_focus(Focus::Path);
        assert_eq!(w.handle_key(ch(' '), true), Action::Stay);
        assert_eq!(w.screen(), Screen::Folder);
        // Space on Browse opens the picker.
        w.set_focus(Focus::Browse);
        assert_eq!(w.handle_key(ch(' '), false), Action::Browse);
    }

    #[test]
    fn focus_leaves_a_button_that_goes_disabled() {
        let mut w = chosen(Origin::FirstLaunch);
        w.set_focus(Focus::Primary);
        w.set_path("~/Projects/acme2");
        w.settle_focus();
        assert_eq!(w.focus(), Focus::Path);
    }

    #[test]
    fn a_warning_reads_as_one_paragraph() {
        assert_eq!(
            prose("shelbi: installed a hook at /r/.git/hooks/pre-commit.\n shelbi:   It blocks commits\n shelbi:   in agent panes."),
            "Installed a hook at /r/.git/hooks/pre-commit. It blocks commits in agent panes."
        );
        assert_eq!(prose("plain"), "Plain");
    }

    #[test]
    fn tilde_shortens_only_the_home_folder() {
        let home = Path::new("/Users/me");
        assert_eq!(tilde(Path::new("/Users/me"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/Users/me/a b"), Some(home)), "~/a b");
        assert_eq!(tilde(Path::new("/Users/meg/x"), Some(home)), "/Users/meg/x");
        assert_eq!(tilde(Path::new("/opt/x"), None), "/opt/x");
    }
}
