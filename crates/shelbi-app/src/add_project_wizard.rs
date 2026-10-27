//! The Add Project wizard's state machine (`dt-add-project-wizard`).
//!
//! The desktop app adds a project through a small wizard rather than one
//! form: screen 01 picks the project's folder (the name is the folder's), and
//! screen 02 picks where its settings live ([`ConfigMode`]). Create Project
//! then hands the host one [`CreateRequest`] for
//! `shelbi_orchestrator::project_create::add_project`, the engine `shelbi
//! init` and the TUI's form use, and the wizard waits in [`Screen::Creating`]
//! until the host reports back.
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
pub const CREATING_TITLE: &str = "Creating project";

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

/// The wizard's screens (the Figma frames 01 and 02, then the creation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Folder,
    Settings,
    Creating,
}

impl Screen {
    /// The footer's "Step N of 3".
    pub fn step(self) -> usize {
        match self {
            Screen::Folder => 1,
            Screen::Settings => 2,
            Screen::Creating => 3,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Screen::Folder => FOLDER_TITLE,
            Screen::Settings => SETTINGS_TITLE,
            Screen::Creating => CREATING_TITLE,
        }
    }
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
    /// Close the wizard without creating anything.
    Close,
    /// Create the project (once).
    Create(CreateRequest),
}

/// The values Create Project hands the creation engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateRequest {
    pub name: String,
    /// The resolved folder.
    pub root: String,
    pub mode: ConfigMode,
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
    /// Why the last creation failed.
    error: Option<String>,
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
            error: None,
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

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
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
            Screen::Creating => false,
        }
    }

    /// Back can be pressed.
    pub fn back_enabled(&self) -> bool {
        match self.screen {
            Screen::Folder => self.origin == Origin::Workspace,
            Screen::Settings => true,
            Screen::Creating => false,
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
        self.error = None;
    }

    /// The name field's text changed.
    pub fn set_name(&mut self, text: impl Into<String>) {
        if self.name.is_some() {
            self.name = Some(text.into());
            self.error = None;
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
            Screen::Creating => {}
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
            self.error = None;
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
                self.error = None;
                self.settle_focus();
                Action::Stay
            }
            Screen::Creating => Action::Stay,
        }
    }

    /// Continue, or Create Project: the screen's primary action, when
    /// enabled. Create Project moves to [`Screen::Creating`], so a second
    /// press does nothing.
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
                };
                self.screen = Screen::Creating;
                self.error = None;
                Action::Create(request)
            }
            Screen::Creating => Action::Stay,
        }
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
            Key::Esc if bare => {
                if self.screen == Screen::Creating {
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

    /// The creation failed: back to screen 02 with the reason, so the user
    /// can go Back and fix it, or try again.
    pub fn creation_failed(&mut self, message: impl Into<String>) {
        if self.screen == Screen::Creating {
            self.screen = Screen::Settings;
            self.focus = Focus::Card(self.mode);
        }
        self.error = Some(message.into());
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
        };
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Create(request));
        assert_eq!(w.screen(), Screen::Creating);
        assert_eq!(step_label(w.screen()), "Step 3 of 3");
        assert_eq!(w.handle_key(key(Key::Enter), false), Action::Stay, "no second submit");
        assert_eq!(w.activate(Focus::Primary), Action::Stay);
        assert_eq!(w.handle_key(key(Key::Esc), false), Action::Stay, "Esc waits for the creation");
        assert_eq!(w.back(), Action::Stay);
        assert!(w.tab_order().is_empty());
    }

    #[test]
    fn a_failed_creation_returns_to_settings_with_the_reason() {
        let mut w = chosen(Origin::Workspace);
        w.primary();
        assert!(matches!(w.primary(), Action::Create(_)));
        w.creation_failed("a project already lives at `acme`");
        assert_eq!(w.screen(), Screen::Settings);
        assert_eq!(w.error(), Some("a project already lives at `acme`"));
        assert!(w.primary_enabled(), "it can be tried again");
        w.back();
        assert_eq!(w.error(), None);
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
    fn tilde_shortens_only_the_home_folder() {
        let home = Path::new("/Users/me");
        assert_eq!(tilde(Path::new("/Users/me"), Some(home)), "~");
        assert_eq!(tilde(Path::new("/Users/me/a b"), Some(home)), "~/a b");
        assert_eq!(tilde(Path::new("/Users/meg/x"), Some(home)), "/Users/meg/x");
        assert_eq!(tilde(Path::new("/opt/x"), None), "/opt/x");
    }
}
