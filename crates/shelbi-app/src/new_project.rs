//! The new-project flow every front end shares: its steps, in order, what a
//! chosen folder turned out to be, and the answers the creation engine takes.
//!
//! `shelbi init` (interactive and `-y`), bare `shelbi`'s first run,
//! `shelbi project add`, the TUI's Add project dialog and the desktop's Add
//! Project wizard all walk the same [`Step`]s and hand the same
//! [`NewProject`] to `shelbi_orchestrator::project_create::create_project`,
//! the one engine that writes a project. They differ only in widgets.
//!
//! 1. [`Step::Folder`]: the folder and the project's name. When the folder
//!    isn't a git repo the flow offers to initialize one
//!    ([`NewProject::git_init`]); when it carries a teammate's committed
//!    `.shelbi/project.yaml` it offers to pick that up
//!    ([`NewProject::pick_up`]), which skips the remaining steps (the
//!    committed config already answers them).
//! 2. [`Step::Agents`]: the default runner and model
//!    ([`crate::harness_picker::HarnessPicker`]), the detected install
//!    preselected.
//! 3. [`Step::Issues`]: where the board lives ([`IssuesChoice`]), GitHub
//!    preselected when the origin is a GitHub repo, and verified with the
//!    `gh` preflight before the flow moves on.
//! 4. [`Step::Settings`]: where the settings live ([`ConfigMode`]), the
//!    folder's [`Facts::recommended_mode`] preselected.
//!
//! Every default comes from one detection ([`FolderCheck`], run by
//! `project_create::check_folder`), and [`NewProject::detected`] is the
//! flow with every default accepted: what `shelbi init -y` creates.

use std::path::{Path, PathBuf};

use shelbi_core::{AgentDefaults, ConfigMode, GithubConnection, IssueTrackerBackend, IssueTrackerConfig, RunnerKind};

/// The flow's steps, in the order every front end shows them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Step {
    Folder,
    Agents,
    Issues,
    Settings,
}

/// How many steps the flow has ("Step 2 of 4").
pub const STEPS: usize = 4;

impl Step {
    pub const ALL: [Step; STEPS] = [Step::Folder, Step::Agents, Step::Issues, Step::Settings];

    /// Its 1-based position.
    pub fn number(self) -> usize {
        Step::ALL.iter().position(|s| *s == self).unwrap_or(0) + 1
    }

    /// "Step 2 of 4".
    pub fn label(self) -> String {
        format!("Step {} of {STEPS}", self.number())
    }

    /// The step after this one.
    pub fn next(self) -> Option<Step> {
        Step::ALL.get(self.number()).copied()
    }

    /// The step before this one.
    pub fn prev(self) -> Option<Step> {
        self.number().checked_sub(2).map(|i| Step::ALL[i])
    }

    /// The step's short name (the CLI's section heading).
    pub fn name(self) -> &'static str {
        match self {
            Step::Folder => "Folder",
            Step::Agents => "Agents",
            Step::Issues => "Issues",
            Step::Settings => "Settings",
        }
    }
}

/// The Issues step's title and the line under it.
pub const ISSUES_TITLE: &str = "Where should issues live?";
pub const ISSUES_INTRO: &str = "Choose where Shelbi keeps this project's tasks.";
/// The two choices: title and description.
pub const GITHUB_TITLE: &str = "GitHub Issues";
pub const GITHUB_BODY: &str =
    "Tasks are GitHub issues your team can see and file. Needs the GitHub CLI (gh), signed in.";
pub const FILE_SYSTEM_TITLE: &str = "File system";
pub const FILE_SYSTEM_BODY: &str = "Tasks are private markdown files on this machine. No GitHub needed.";
/// The repo field's label.
pub const GITHUB_REPO_LABEL: &str = "GitHub repo (owner/repo)";
/// What a GitHub board writes into the repo, shown once it's chosen.
pub const GITHUB_DISCLOSURE: &str = "Shelbi will create `shelbi:id/*` and `shelbi:status/*` labels \
     on first use and store a small metadata block in each issue's body.";

/// The Folder step's two offers.
pub const GIT_INIT_LABEL: &str = "Initialize a Git repository here";
pub const PICK_UP_LABEL: &str = "Use the project settings committed in this folder";

/// What the chosen folder is under version control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Repo {
    /// A Git repository (its own `.git` directory, or inside one).
    Git,
    /// A Git worktree (its `.git` is a file pointing at the main repo).
    Worktree,
    /// Not under Git: the flow offers to initialize a repository.
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

/// A teammate's committed `.shelbi/project.yaml` in the chosen folder that
/// this machine hasn't registered: the flow offers to pick it up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickUp {
    /// The committed `name:` (the project's id).
    pub name: String,
    /// The committed file.
    pub config: PathBuf,
}

/// What detection found in and around the chosen folder: the defaults every
/// step starts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    /// The default branch: origin's `HEAD`, else the checked-out branch,
    /// else `main` (also the branch a `git init` creates).
    pub default_branch: String,
    /// The origin remote, credentials removed.
    pub remote: Option<String>,
    /// The `owner/repo` the origin names on GitHub.
    pub github_repo: Option<String>,
    /// A committed project config to pick up.
    pub pick_up: Option<PickUp>,
    /// Where settings should live: in the repo when it looks shared (an
    /// origin and more than one committer), else outside it
    /// ([`recommend_mode`]).
    pub recommended_mode: ConfigMode,
    /// Why, as every front end shows it beside the recommendation
    /// (`origin remote + 3 committers`).
    pub mode_reason: String,
    /// The agent runners installed on this machine, in preference order.
    pub runners: Vec<RunnerKind>,
}

impl Default for Facts {
    fn default() -> Self {
        Facts {
            default_branch: DEFAULT_BRANCH.to_string(),
            remote: None,
            github_repo: None,
            pick_up: None,
            recommended_mode: ConfigMode::Global,
            mode_reason: NOT_GIT_REASON.to_string(),
            runners: Vec::new(),
        }
    }
}

/// What the settings-location recommendation reads: a repo's team signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TeamSignals {
    /// The repo has an origin remote: probably shared.
    pub has_remote_origin: bool,
    /// Distinct committer emails in the recent history.
    pub distinct_committer_emails: usize,
}

/// [`Facts::mode_reason`] for a folder that isn't a Git repo.
pub const NOT_GIT_REASON: &str = "not a Git repo yet";

/// Where a repo with `signals` should keep its settings, and why: in the
/// repo when it looks shared (an origin AND at least two committers), else
/// outside it.
pub fn recommend_mode(signals: &TeamSignals) -> (ConfigMode, String) {
    let n = signals.distinct_committer_emails;
    let committers = match n {
        0 => "no commits yet".to_string(),
        1 => "1 committer".to_string(),
        n => format!("{n} committers"),
    };
    match (signals.has_remote_origin, n >= 2) {
        (true, true) => (ConfigMode::InRepo, format!("origin remote + {committers}")),
        (true, false) => (ConfigMode::Global, format!("origin remote, but {committers}")),
        (false, _) => (ConfigMode::Global, format!("no origin remote, {committers}")),
    }
}

/// How a front end names a settings location in one word.
pub fn mode_name(mode: ConfigMode) -> &'static str {
    match mode {
        ConfigMode::InRepo => "In repo",
        ConfigMode::Global => "Global",
    }
}

impl Facts {
    /// The recommendation and why, as one line: `Recommended: In repo
    /// (origin remote + 3 committers)`.
    pub fn mode_recommendation(&self) -> String {
        format!("Recommended: {} ({})", mode_name(self.recommended_mode), self.mode_reason)
    }
}

/// The branch a folder with none detected (and a `git init`) gets.
pub const DEFAULT_BRANCH: &str = "main";

impl Facts {
    /// The runner the Agents step starts on: the first installed one, Claude
    /// Code when none is.
    pub fn recommended_runner(&self) -> RunnerKind {
        self.runners.first().copied().unwrap_or(RunnerKind::Claude)
    }

    /// The board the Issues step starts on: GitHub when the origin is a
    /// GitHub repo, else the file system.
    pub fn recommended_issues(&self) -> IssuesChoice {
        IssuesChoice::recommended(self.github_repo.as_deref())
    }
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
    /// The defaults detection found.
    pub facts: Facts,
}

/// How a status line reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Normal,
    Warning,
    Error,
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
            facts: Facts::default(),
        }
    }

    /// The folder isn't under Git, so the flow offers to initialize it.
    pub fn offers_git_init(&self) -> bool {
        self.problem.is_none() && self.repo == Some(Repo::NotGit)
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
            None => match (&self.facts.pick_up, self.repo) {
                (Some(pick_up), _) => format!(
                    "Found Shelbi settings for “{}” committed in this folder.",
                    pick_up.name
                ),
                (None, Some(Repo::Git)) => {
                    "Git repository detected. Shelbi will use this folder as your project.".into()
                }
                (None, Some(Repo::Worktree)) => {
                    "Git worktree detected. Shelbi will use this folder as your project.".into()
                }
                (None, Some(Repo::NotGit) | None) => {
                    "Not a Git repository. Shelbi works best in one.".into()
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
    pub fn wants_name(&self) -> bool {
        matches!(self.problem, Some(Problem::Taken { .. } | Problem::NoName))
    }
}

/// The two boards a new project can use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tracker {
    Github,
    FileSystem,
}

/// The Issues step's choice: the board, the GitHub repo, and whether that
/// repo passed the `gh` preflight. Checking takes `gh`, so the choice only
/// says when a check is due ([`Self::preflight_due`]); the host runs it and
/// answers with [`Self::preflight_passed`] or [`Self::preflight_failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssuesChoice {
    tracker: Tracker,
    /// The repo field's text.
    repo: String,
    /// The repo text that last passed the preflight.
    verified: Option<String>,
    /// Why the last preflight (or the repo's spelling) failed.
    error: Option<String>,
}

impl Default for IssuesChoice {
    fn default() -> Self {
        IssuesChoice::recommended(None)
    }
}

impl IssuesChoice {
    /// GitHub, on `github_repo`, when there is one; else the file system.
    pub fn recommended(github_repo: Option<&str>) -> Self {
        IssuesChoice {
            tracker: if github_repo.is_some() { Tracker::Github } else { Tracker::FileSystem },
            repo: github_repo.unwrap_or_default().to_string(),
            verified: None,
            error: None,
        }
    }

    pub fn tracker(&self) -> Tracker {
        self.tracker
    }

    pub fn repo(&self) -> &str {
        &self.repo
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The GitHub repo passed the preflight (as typed now).
    pub fn verified(&self) -> bool {
        self.verified.as_deref() == Some(self.repo.trim())
    }

    pub fn select(&mut self, tracker: Tracker) {
        if tracker != self.tracker {
            self.tracker = tracker;
            self.error = None;
        }
    }

    /// The other choice (an arrow key on the pickers).
    pub fn toggle(&mut self) {
        self.select(match self.tracker {
            Tracker::Github => Tracker::FileSystem,
            Tracker::FileSystem => Tracker::Github,
        });
    }

    pub fn set_repo(&mut self, text: impl Into<String>) {
        let text = text.into();
        if text != self.repo {
            self.repo = text;
            self.error = None;
        }
    }

    /// Continue can move on: the file system, or a GitHub repo that passed.
    pub fn ready(&self) -> bool {
        match self.tracker {
            Tracker::FileSystem => true,
            Tracker::Github => self.verified(),
        }
    }

    /// The repo a preflight is due for: GitHub is chosen and the repo as
    /// typed hasn't passed. A blank or misspelled repo fails here, without
    /// a check.
    pub fn preflight_due(&mut self) -> Option<String> {
        if self.tracker != Tracker::Github || self.verified() {
            return None;
        }
        match github_config(&self.repo) {
            Ok(_) => Some(self.repo.trim().to_string()),
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    /// `repo` passed the preflight.
    pub fn preflight_passed(&mut self, repo: &str) {
        if repo == self.repo.trim() {
            self.verified = Some(repo.to_string());
            self.error = None;
        }
    }

    /// `repo` failed the preflight with `message` (the fix to show).
    pub fn preflight_failed(&mut self, repo: &str, message: impl Into<String>) {
        if repo == self.repo.trim() {
            self.verified = None;
            self.error = Some(message.into());
        }
    }

    /// The board config this choice writes. A GitHub repo that doesn't
    /// spell `owner/repo` falls back to the file system (the step never
    /// moves on with one).
    pub fn config(&self) -> IssueTrackerConfig {
        match self.tracker {
            Tracker::FileSystem => IssueTrackerConfig::default(),
            Tracker::Github => github_config(&self.repo).unwrap_or_default(),
        }
    }
}

/// A `github` board on `repo`, validated the way the loader validates it.
pub fn github_config(repo: &str) -> Result<IssueTrackerConfig, String> {
    let repo = repo.trim();
    if repo.is_empty() {
        return Err("Enter the GitHub repo as owner/repo.".into());
    }
    let cfg = IssueTrackerConfig {
        backend: IssueTrackerBackend::Github,
        github: Some(GithubConnection { repo: repo.to_string() }),
        ..IssueTrackerConfig::default()
    };
    cfg.validate().map_err(|e| e.to_string())?;
    Ok(cfg)
}

/// Everything the creation engine needs to create one project: the folder
/// and the answer to every step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProject {
    /// The project's folder, resolved.
    pub root: PathBuf,
    /// The project's name as entered (the engine slugifies it into its id).
    pub name: String,
    /// Pick up the folder's committed config instead of creating one; every
    /// field below is then the committed config's business.
    pub pick_up: bool,
    /// Initialize a Git repository in a folder that isn't one.
    pub git_init: bool,
    pub defaults: AgentDefaults,
    pub issue_tracker: IssueTrackerConfig,
    pub mode: ConfigMode,
    /// The branch recorded as the project's default (and a `git init`'s).
    pub default_branch: String,
    /// The origin remote recorded as `github_url`.
    pub remote: Option<String>,
}

impl NewProject {
    /// The flow with every detected default accepted: the folder's name, a
    /// `git init` when it isn't a repo, a pick-up when one applies, the
    /// recommended runner on its default model, the recommended board and
    /// settings location, and the detected branch and remote. (A GitHub
    /// board still has to pass the preflight before it's created.)
    pub fn detected(check: &FolderCheck) -> NewProject {
        let facts = &check.facts;
        NewProject {
            root: check.path.clone(),
            name: check.name.clone().unwrap_or_default(),
            pick_up: facts.pick_up.is_some(),
            git_init: check.repo == Some(Repo::NotGit),
            defaults: AgentDefaults { runner: facts.recommended_runner(), model: None },
            issue_tracker: facts.recommended_issues().config(),
            mode: facts.recommended_mode,
            default_branch: facts.default_branch.clone(),
            remote: facts.remote.clone(),
        }
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

    #[test]
    fn the_steps_run_folder_agents_issues_settings() {
        assert_eq!(Step::ALL, [Step::Folder, Step::Agents, Step::Issues, Step::Settings]);
        assert_eq!(Step::Issues.label(), "Step 3 of 4");
        assert_eq!(Step::Folder.next(), Some(Step::Agents));
        assert_eq!(Step::Settings.next(), None);
        assert_eq!(Step::Folder.prev(), None);
        assert_eq!(Step::Settings.prev(), Some(Step::Issues));
    }

    #[test]
    fn github_is_recommended_only_for_a_github_origin_and_must_pass_preflight() {
        let mut none = IssuesChoice::recommended(None);
        assert_eq!(none.tracker(), Tracker::FileSystem);
        assert!(none.ready());
        assert_eq!(none.preflight_due(), None);
        assert_eq!(none.config(), IssueTrackerConfig::default());

        let mut gh = IssuesChoice::recommended(Some("acme/app"));
        assert_eq!(gh.tracker(), Tracker::Github);
        assert!(!gh.ready(), "GitHub waits for its preflight");
        assert_eq!(gh.preflight_due().as_deref(), Some("acme/app"));
        gh.preflight_failed("acme/app", "gh is not signed in");
        assert_eq!(gh.error(), Some("gh is not signed in"));
        assert!(!gh.ready());
        gh.preflight_passed("acme/app");
        assert!(gh.ready());
        assert_eq!(gh.preflight_due(), None);
        assert_eq!(gh.config().github.unwrap().repo, "acme/app");

        gh.set_repo("acme/other");
        assert!(!gh.ready(), "a different repo is checked again");
        gh.preflight_passed("acme/app");
        assert!(!gh.ready(), "an answer for old text is dropped");

        gh.set_repo("not a repo");
        assert_eq!(gh.preflight_due(), None);
        assert!(gh.error().is_some(), "a misspelled repo fails without a check");
        gh.toggle();
        assert_eq!(gh.tracker(), Tracker::FileSystem);
        assert!(gh.ready());
    }

    #[test]
    fn the_recommended_mode_needs_an_origin_and_two_committers_and_says_so() {
        let s = |has_remote_origin, distinct_committer_emails| {
            recommend_mode(&TeamSignals { has_remote_origin, distinct_committer_emails })
        };
        assert_eq!(s(true, 4), (ConfigMode::InRepo, "origin remote + 4 committers".into()));
        assert_eq!(s(true, 2).0, ConfigMode::InRepo);
        assert_eq!(s(true, 1), (ConfigMode::Global, "origin remote, but 1 committer".into()));
        assert_eq!(s(false, 5), (ConfigMode::Global, "no origin remote, 5 committers".into()));
        assert_eq!(s(false, 0), (ConfigMode::Global, "no origin remote, no commits yet".into()));
        assert_eq!(
            Facts::default().mode_recommendation(),
            "Recommended: Global (not a Git repo yet)"
        );
    }

    #[test]
    fn detected_accepts_every_default() {
        let path = PathBuf::from("/code/app");
        let check = FolderCheck {
            in_repo_dir: path.join(".shelbi"),
            path: path.clone(),
            name: Some("app".into()),
            slug: Some("app".into()),
            repo: Some(Repo::NotGit),
            problem: None,
            global_dir: None,
            facts: Facts {
                default_branch: "trunk".into(),
                remote: Some("git@github.com:acme/app.git".into()),
                github_repo: Some("acme/app".into()),
                pick_up: None,
                recommended_mode: ConfigMode::InRepo,
                mode_reason: "origin remote + 2 committers".into(),
                runners: vec![RunnerKind::Codex],
            },
        };
        let plan = NewProject::detected(&check);
        assert_eq!(plan.root, path);
        assert_eq!(plan.name, "app");
        assert!(plan.git_init && !plan.pick_up);
        assert_eq!(plan.defaults, AgentDefaults { runner: RunnerKind::Codex, model: None });
        assert_eq!(plan.issue_tracker.backend, IssueTrackerBackend::Github);
        assert_eq!(plan.mode, ConfigMode::InRepo);
        assert_eq!(plan.default_branch, "trunk");
        assert_eq!(plan.remote.as_deref(), Some("git@github.com:acme/app.git"));
        assert_eq!(Facts::default().recommended_runner(), RunnerKind::Claude);
    }
}
