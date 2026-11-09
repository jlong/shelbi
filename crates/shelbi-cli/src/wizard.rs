//! The CLI's new-project flow: `shelbi init` (interactive and `-y`), bare
//! `shelbi`'s first run, and `shelbi project add`.
//!
//! It walks the steps every front end shares
//! ([`shelbi_app::new_project::Step`]: Folder, Agents, Issues, Settings) on
//! the defaults the shared detection found
//! ([`shelbi_orchestrator::project_create::check_folder`]), then hands the
//! answers to the one engine that writes a project
//! ([`project_create::create_project`]). The TUI's dialog and the desktop's
//! wizard are the same steps with other widgets.
//!
//! A flag ([`Preset`]) pre-answers its step. A step with no flag is asked
//! through the [`Ask`] seam: [`Prompts`] asks at the terminal, and
//! [`Defaults`] (`-y`, or no terminal) takes the step's detected default,
//! which is exactly what the prompt pre-selects.

use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use inquire::{Confirm, Select, Text};
use shelbi_app::harness_picker::HarnessPicker;
use shelbi_app::new_project::{FolderCheck, NewProject, Problem, Step};
use shelbi_core::{ConfigMode, IssueTrackerBackend, IssueTrackerConfig, RunnerKind};
use shelbi_orchestrator::project_create::{
    self, detect::SUPPORTED_RUNNERS, installed_runners, AddProjectError, Attempt, ScaffoldReporter,
};

use crate::issue_tracker_setup::{self, GhProbe, GithubSuggestion, RealGhProbe};

pub const BANNER: &str = concat!(
    "   ▄▀▀▀▀▀▄   ▀▀    ▀▀  ▀▀▀▀▀▀▀   ▀▀   ▀▀▀▀▀▀▀▀▀▀▄   ▀▀▀▀▀\n",
    "  ▀▀        ▀▀    ▀▀  ▀▀        ▀▀        ▀▀    ▀▀   ▀▀\n",
    "  ▀▀▀▀▄    ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀    ▀▀      ▀▀▀▀▀▀▀▀▄    ▀▀\n",
    "▄     ▀▀  ▀▀    ▀▀  ▀▀        ▀▀        ▀▀     ▀▀  ▀▀\n",
    " ▀▀▀▀▀▀  ▀▀    ▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀▀▀▀  ▀▀▀▀▀",
);

pub const TAGLINE: &str = "an open-source agent orchestrator for the terminal";

pub fn print_banner() {
    println!("{BANNER}");
    println!("{TAGLINE}");
    println!();
}

pub fn text(label: &str, default: &str) -> Result<String> {
    Text::new(label)
        .with_default(default)
        .prompt()
        .with_context(|| format!("text prompt {label:?}"))
}

/// A runner a new project can default to (`--runner`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub(crate) enum Runner {
    Claude,
    Codex,
}

impl Runner {
    pub(crate) fn kind(self) -> RunnerKind {
        match self {
            Self::Claude => RunnerKind::Claude,
            Self::Codex => RunnerKind::Codex,
        }
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.kind().as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetupOutcome {
    Created(String),
    Quit,
}

/// The Issues step's flag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IssuesPreset {
    /// GitHub, on this repo or else the one the origin names.
    Github { repo: Option<String> },
    FileSystem,
}

/// Flags that pre-answer steps. `None` leaves the step to [`Ask`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Preset {
    /// The folder (`--root`); the current Git checkout's top otherwise.
    pub root: Option<PathBuf>,
    /// The project's name (`--project`); with a pick-up, its local alias.
    pub project: Option<String>,
    /// `--pick-up`.
    pub pick_up: bool,
    pub runner: Option<Runner>,
    pub model: Option<String>,
    pub issues: Option<IssuesPreset>,
    pub mode: Option<ConfigMode>,
    pub default_branch: Option<String>,
    /// `--github-url` (blank records none).
    pub remote: Option<String>,
}

/// How a step with no flag gets its answer. Every method is handed the
/// step's detected default.
pub(crate) trait Ask {
    /// The folder to set up.
    fn folder(&mut self, default: &Path, error: Option<&str>) -> Result<PathBuf>;
    /// The project's name.
    fn name(&mut self, default: &str, error: Option<&str>) -> Result<String>;
    /// A yes/no offer whose default is yes.
    fn confirm(&mut self, question: &str) -> Result<bool>;
    fn runner(&mut self, recommended: RunnerKind) -> Result<RunnerKind>;
    /// The default model for `picker`'s runner (`None`: its own default).
    fn model(&mut self, picker: &HarnessPicker) -> Result<Option<String>>;
    fn issues(&mut self, suggestion: &GithubSuggestion) -> Result<IssueTrackerConfig>;
    /// The settings location; `reason` says why `recommended` is.
    fn mode(&mut self, recommended: ConfigMode, reason: &str) -> Result<ConfigMode>;
    /// The flow is interactive (a person can fix a bad folder or name).
    fn interactive(&self) -> bool;
}

/// Run the flow: answer every step (flags first, then `ask`), then create
/// the project, printing the engine's progress.
pub(crate) fn setup(preset: Preset, ask: &mut dyn Ask) -> Result<SetupOutcome> {
    let cwd = std::env::current_dir().context("reading current directory")?;
    let Some(plan) = answer_steps(&cwd, &preset, ask, &mut RealGhProbe)? else {
        return Ok(SetupOutcome::Quit);
    };
    create(&plan)
}

/// Create `plan`'s project through the engine, printing its progress.
pub(crate) fn create(plan: &NewProject) -> Result<SetupOutcome> {
    println!();
    println!("Creating…");
    let slug = project_create::create_project(plan, Attempt::New, &mut StdoutReporter).map_err(|e| match e {
        AddProjectError::Invalid(msg) | AddProjectError::Failed(msg) => anyhow!(msg),
    })?;
    println!("✓ Project {slug} created.");
    Ok(SetupOutcome::Created(slug))
}

/// Prints the engine's progress: notes to stdout, warnings to stderr.
pub(crate) struct StdoutReporter;

impl ScaffoldReporter for StdoutReporter {
    fn note(&mut self, line: &str) {
        println!("{line}");
    }
    fn warn(&mut self, line: &str) {
        eprintln!("{line}");
    }
}

/// Answer every step for the folder `preset` or `ask` picks: the answers
/// [`create`] hands the engine, or `None` when the folder is already a
/// project (nothing to do). Write-free: errors here leave nothing behind.
pub(crate) fn answer_steps(
    cwd: &Path,
    preset: &Preset,
    ask: &mut dyn Ask,
    gh: &mut dyn GhProbe,
) -> Result<Option<NewProject>> {
    // Step 1: Folder.
    heading(Step::Folder, ask);
    let Some(check) = choose_folder(cwd, preset, ask)? else {
        return Ok(None);
    };
    let mut plan = NewProject::detected(&check);
    if let Some(pick_up) = &check.facts.pick_up {
        plan.pick_up = preset.pick_up
            || ask.confirm(&format!(
                "Found committed Shelbi settings for `{}` at {}. Use them?",
                pick_up.name,
                pick_up.config.display()
            ))?;
    } else {
        plan.pick_up = preset.pick_up;
    }
    if plan.pick_up {
        // The committed config answers every other step.
        return Ok(Some(plan));
    }
    if check.facts.pick_up.is_some() && preset.project.is_none() {
        // A declined pick-up: a new project, named for the folder unless the
        // user names it, which must be free.
        let default = shelbi_app::new_project::folder_name(&check.path).unwrap_or_default();
        let root = check.path.to_string_lossy();
        let mut error = None;
        plan.name = loop {
            let name = ask.name(&default, error.as_deref())?;
            match project_create::validate_add_project(&name, &root, &check.path) {
                Ok(_) => break name,
                Err(why) if ask.interactive() => error = Some(why),
                Err(why) => bail!(why),
            }
        };
    }
    if check.offers_git_init() {
        plan.git_init = ask.confirm(&format!(
            "{} is not a Git repo. Initialize one here with git init -b {}?",
            check.path.display(),
            preset.default_branch.as_deref().unwrap_or(&check.facts.default_branch)
        ))?;
    }

    // Step 2: Agents.
    heading(Step::Agents, ask);
    let installed = installed_runners();
    let runner = match preset.runner {
        Some(runner) => {
            if !ask.interactive() && !installed.iter().any(|r| r.kind == runner.kind()) {
                bail!(
                    "requested runner {runner} was not found on PATH; installed supported runners: {}",
                    installed.iter().map(|r| r.kind.as_str()).collect::<Vec<_>>().join(", ")
                );
            }
            runner.kind()
        }
        None if installed.is_empty() && !ask.interactive() => bail!("{}", missing_runner_guidance()),
        None => ask.runner(check.facts.recommended_runner())?,
    };
    let mut picker = HarnessPicker::new(runner, None);
    let model = match &preset.model {
        Some(model) => Some(model.clone()),
        None => ask.model(&picker)?,
    };
    picker.set_model(model.as_deref());
    plan.defaults = picker.defaults();

    // Step 3: Issues.
    heading(Step::Issues, ask);
    plan.issue_tracker = match &preset.issues {
        Some(IssuesPreset::FileSystem) => issue_tracker_setup::file_system_config(),
        Some(IssuesPreset::Github { repo }) => {
            let repo = repo
                .clone()
                .or_else(|| check.facts.github_repo.clone())
                .ok_or_else(|| {
                    anyhow!(
                        "no GitHub repo for `--issue-tracker github`: the origin remote isn't a \
                         GitHub repo. Pass `--github-repo owner/repo`."
                    )
                })?;
            let cfg = issue_tracker_setup::github_config(&repo)?;
            // Never write a GitHub config that won't load.
            if let Err(failure) = issue_tracker_setup::github_preflight(&repo, gh) {
                bail!("{}", failure.guidance());
            }
            cfg
        }
        None => ask.issues(&GithubSuggestion { repo: check.facts.github_repo.clone() })?,
    };
    if !ask.interactive() {
        print_issue_tracker_plan(&plan.issue_tracker);
    }

    // Step 4: Settings.
    heading(Step::Settings, ask);
    plan.mode = match preset.mode {
        Some(mode) => mode,
        None => ask.mode(check.facts.recommended_mode, &check.facts.mode_reason)?,
    };

    if let Some(branch) = &preset.default_branch {
        let branch = branch.trim();
        if branch.is_empty() {
            bail!("--default-branch must not be empty");
        }
        shelbi_core::validate_branch(branch).map_err(|error| anyhow!("invalid --default-branch {branch:?}: {error}"))?;
        plan.default_branch = branch.to_string();
    }
    if let Some(remote) = &preset.remote {
        plan.remote = project_create::remote_url_for_storage(remote);
    }
    Ok(Some(plan))
}

/// The Folder step: the folder and the project's name, re-asked (when
/// interactive) until the shared check finds no problem. `None` when, with
/// nobody to ask, the folder already is a project: nothing to do.
fn choose_folder(cwd: &Path, preset: &Preset, ask: &mut dyn Ask) -> Result<Option<FolderCheck>> {
    let mut folder = match &preset.root {
        Some(root) => project_create::absolutize(cwd, root),
        None => ask.folder(&default_folder(cwd), None)?,
    };
    if !ask.interactive() && !preset.pick_up {
        if let Ok(Some(project)) = shelbi_state::resolve_project_for_cwd(&folder) {
            println!("✓ Project {project} is already configured.");
            return Ok(None);
        }
    }
    let mut name = preset.project.clone();
    let mut asked_name = name.is_some();
    loop {
        let check = project_create::check_folder(cwd, &folder.to_string_lossy(), name.as_deref());
        let message = check.message();
        match &check.problem {
            None if asked_name || !ask.interactive() || check.facts.pick_up.is_some() => {
                println!("  {message}");
                return Ok(Some(check));
            }
            None => {
                // Offer the folder's name, which the user can change.
                let default = check.name.clone().unwrap_or_default();
                name = Some(ask.name(&default, None)?);
                asked_name = true;
            }
            Some(Problem::Taken { .. } | Problem::NoName) if ask.interactive() => {
                let default = check.name.clone().unwrap_or_default();
                name = Some(ask.name(&default, Some(&message))?);
                asked_name = true;
            }
            Some(Problem::Empty | Problem::NotFound | Problem::NotDirectory) if ask.interactive() && preset.root.is_none() => {
                folder = ask.folder(&check.path, Some(&message))?;
            }
            Some(Problem::Taken { slug }) => bail!(
                "a Shelbi project named {slug} already exists; no existing state was overwritten. \
                 Pass --project NAME to choose a different name."
            ),
            Some(Problem::NotFound) => bail!("project root {} does not exist", check.path.display()),
            Some(Problem::NotDirectory) => bail!("project root {} is not a directory", check.path.display()),
            Some(_) => bail!("{message}"),
        }
    }
}

/// The folder the Folder step starts on: the top of the Git checkout `cwd`
/// is in, else `cwd`.
fn default_folder(cwd: &Path) -> PathBuf {
    Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            let text = String::from_utf8(o.stdout).ok()?;
            let line = text.strip_suffix('\n').unwrap_or(&text);
            (!line.is_empty()).then(|| PathBuf::from(line))
        })
        .unwrap_or_else(|| cwd.to_path_buf())
}

fn heading(step: Step, ask: &dyn Ask) {
    if ask.interactive() {
        println!();
        println!("{} · {}", step.label(), step.name());
    }
}

fn missing_runner_guidance() -> String {
    concat!(
        "No supported agent runner was found on PATH. Install and authenticate Claude Code ",
        "(https://docs.claude.com/en/docs/claude-code) or Codex ",
        "(npm install -g @openai/codex), then start Shelbi again."
    )
    .to_string()
}

/// Print the resolved board in the non-interactive summary. For GitHub it
/// also discloses the labels and body metadata Shelbi will write.
fn print_issue_tracker_plan(tracker: &IssueTrackerConfig) {
    match tracker.backend {
        IssueTrackerBackend::Github => {
            let repo = tracker.github.as_ref().map(|g| g.repo.as_str()).unwrap_or("");
            println!("✓ issue tracker: GitHub ({repo})");
            println!("  {}", issue_tracker_setup::GITHUB_DISCLOSURE);
        }
        _ => println!("✓ issue tracker: file system (local markdown)"),
    }
}

/// [`Ask`] at the terminal, every prompt pre-selecting the detected default.
pub(crate) struct Prompts;

impl Ask for Prompts {
    fn folder(&mut self, default: &Path, error: Option<&str>) -> Result<PathBuf> {
        if let Some(error) = error {
            println!("  ✗ {error}");
        }
        let raw = text("Project folder:", &default.display().to_string())?;
        let cwd = std::env::current_dir().context("reading current directory")?;
        Ok(project_create::absolutize(&cwd, Path::new(raw.trim())))
    }

    fn name(&mut self, default: &str, error: Option<&str>) -> Result<String> {
        if let Some(error) = error {
            println!("  ✗ {error}");
        }
        text("Project name:", default)
    }

    fn confirm(&mut self, question: &str) -> Result<bool> {
        Confirm::new(question).with_default(true).prompt().context("confirmation prompt")
    }

    fn runner(&mut self, recommended: RunnerKind) -> Result<RunnerKind> {
        let installed = installed_runners();
        let options: Vec<Choice<RunnerKind>> = SUPPORTED_RUNNERS
            .into_iter()
            .map(|kind| {
                let found = installed.iter().find(|r| r.kind == kind);
                let label = shelbi_app::settings::agents::runner_label(kind);
                let detail = found.map_or_else(|| "not detected on PATH".to_string(), |r| r.version.clone());
                Choice(kind, format!("{label} ({detail})"))
            })
            .collect();
        let cursor = options.iter().position(|c| c.0 == recommended).unwrap_or(0);
        Select::new("Agent runner:", options)
            .with_starting_cursor(cursor)
            .prompt()
            .map(|c| c.0)
            .context("agent runner selection")
    }

    fn model(&mut self, picker: &HarnessPicker) -> Result<Option<String>> {
        let options: Vec<Choice<Option<String>>> =
            picker.models().into_iter().map(|m| Choice(m.model, m.label)).collect();
        if options.is_empty() {
            return Ok(None);
        }
        let cursor = options.iter().position(|c| c.0.as_deref() == picker.model()).unwrap_or(0);
        Select::new("Default model:", options)
            .with_starting_cursor(cursor)
            .prompt()
            .map(|c| c.0)
            .context("model selection")
    }

    fn issues(&mut self, suggestion: &GithubSuggestion) -> Result<IssueTrackerConfig> {
        issue_tracker_setup::choose_issue_tracker_interactive(suggestion, &mut RealGhProbe)
    }

    fn mode(&mut self, recommended: ConfigMode, reason: &str) -> Result<ConfigMode> {
        println!("Where should this project's settings live?");
        println!("  global  — per-user under ~/.shelbi/projects/<name>/, not committed.");
        println!("  in-repo — committed at <repo>/.shelbi/, shared with the team.");
        println!("  Recommended: {} ({reason})", shelbi_app::new_project::mode_name(recommended));
        let options = vec![
            Choice(ConfigMode::Global, mode_label(ConfigMode::Global, recommended)),
            Choice(ConfigMode::InRepo, mode_label(ConfigMode::InRepo, recommended)),
        ];
        let cursor = options.iter().position(|c| c.0 == recommended).unwrap_or(0);
        Select::new("Settings location:", options)
            .with_starting_cursor(cursor)
            .prompt()
            .map(|c| c.0)
            .context("settings location prompt")
    }

    fn interactive(&self) -> bool {
        true
    }
}

fn mode_label(mode: ConfigMode, recommended: ConfigMode) -> String {
    let name = match mode {
        ConfigMode::Global => "global",
        ConfigMode::InRepo => "in-repo",
    };
    if mode == recommended {
        format!("{name} (recommended)")
    } else {
        name.to_string()
    }
}

/// A prompt row: the value and how it reads.
#[derive(Clone)]
struct Choice<T>(T, String);

impl<T> Display for Choice<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.1)
    }
}

/// [`Ask`] with no terminal (`-y`): every step takes its detected default,
/// the same one [`Prompts`] pre-selects. A recommended GitHub board must
/// pass the preflight, or the file system is used instead.
pub(crate) struct Defaults<'a> {
    pub gh: &'a mut dyn GhProbe,
}

impl Ask for Defaults<'_> {
    fn folder(&mut self, default: &Path, _error: Option<&str>) -> Result<PathBuf> {
        Ok(default.to_path_buf())
    }

    fn name(&mut self, default: &str, _error: Option<&str>) -> Result<String> {
        Ok(default.to_string())
    }

    fn confirm(&mut self, _question: &str) -> Result<bool> {
        Ok(true)
    }

    fn runner(&mut self, recommended: RunnerKind) -> Result<RunnerKind> {
        Ok(recommended)
    }

    fn model(&mut self, _picker: &HarnessPicker) -> Result<Option<String>> {
        Ok(None)
    }

    fn issues(&mut self, suggestion: &GithubSuggestion) -> Result<IssueTrackerConfig> {
        let Some(repo) = &suggestion.repo else {
            return Ok(issue_tracker_setup::file_system_config());
        };
        match issue_tracker_setup::github_preflight(repo, self.gh) {
            Ok(()) => issue_tracker_setup::github_config(repo),
            Err(failure) => {
                println!("  GitHub Issues for {repo} isn't usable: {}", failure.guidance());
                println!("  → Using File system. Run `shelbi issue-store migrate --to github` later.");
                Ok(issue_tracker_setup::file_system_config())
            }
        }
    }

    fn mode(&mut self, recommended: ConfigMode, reason: &str) -> Result<ConfigMode> {
        println!("✓ settings: {} ({reason})", shelbi_app::new_project::mode_name(recommended));
        Ok(recommended)
    }

    fn interactive(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use shelbi_orchestrator::project_create::issues::GhCommandResult;
    use tempfile::TempDir;

    /// A `gh` that passes (or fails) the preflight.
    struct FakeGh(bool);

    impl GhProbe for FakeGh {
        fn gh_version(&mut self) -> Option<String> {
            self.0.then(|| "gh version 2".into())
        }
        fn gh_api_repo(&mut self, _repo: &str) -> GhCommandResult {
            GhCommandResult {
                success: true,
                stdout: r#"{"has_issues":true,"permissions":{"push":true}}"#.into(),
                stderr: String::new(),
            }
        }
    }

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git").args(args).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn defaults_answer_every_step_with_the_detected_default() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", temp.path().join("home"));
        let repo = temp.path().join("App");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "trunk"]);
        git(&repo, &["remote", "add", "origin", "https://u:p@github.com/acme/app.git"]);

        let preset = Preset { root: Some(repo.clone()), ..Preset::default() };
        let mut gh = FakeGh(true);
        let mut probe = FakeGh(true);
        let plan = answer_steps(temp.path(), &preset, &mut Defaults { gh: &mut gh }, &mut probe)
            .unwrap()
            .unwrap();
        let check = project_create::check_folder(temp.path(), &repo.to_string_lossy(), None);
        let mut expected = NewProject::detected(&check);
        // The recommended GitHub board passed its preflight.
        expected.issue_tracker = issue_tracker_setup::github_config("acme/app").unwrap();
        assert_eq!(plan, expected);
        assert_eq!(plan.default_branch, "trunk");
        assert_eq!(plan.remote.as_deref(), Some("https://github.com/acme/app.git"));
        assert_eq!(plan.name, "App");
        assert!(!plan.git_init);

        // A GitHub board that fails its preflight falls back to the file system.
        let mut gh = FakeGh(false);
        let plan = answer_steps(temp.path(), &preset, &mut Defaults { gh: &mut gh }, &mut probe)
            .unwrap()
            .unwrap();
        assert_eq!(plan.issue_tracker, IssueTrackerConfig::default());
    }

    #[test]
    fn flags_pre_answer_their_steps() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", temp.path().join("home"));
        let folder = temp.path().join("plain");
        std::fs::create_dir_all(&folder).unwrap();

        let preset = Preset {
            root: Some(folder.clone()),
            project: Some("Scripted".into()),
            runner: Some(Runner::Codex),
            model: Some("gpt-5".into()),
            issues: Some(IssuesPreset::FileSystem),
            mode: Some(ConfigMode::InRepo),
            default_branch: Some("develop".into()),
            remote: Some("https://user:secret@github.com/acme/s.git?token=x".into()),
            ..Preset::default()
        };
        let mut gh = FakeGh(false);
        let mut probe = FakeGh(false);
        let mut ask = Defaults { gh: &mut gh };
        // A test binary sees both runners installed.
        let plan = answer_steps(temp.path(), &preset, &mut ask, &mut probe).unwrap().unwrap();
        assert_eq!(plan.name, "Scripted");
        assert!(plan.git_init, "a plain folder is initialized");
        assert_eq!(plan.defaults.runner, RunnerKind::Codex);
        assert_eq!(plan.defaults.model.as_deref(), Some("gpt-5"));
        assert_eq!(plan.mode, ConfigMode::InRepo);
        assert_eq!(plan.default_branch, "develop");
        assert_eq!(plan.remote.as_deref(), Some("https://github.com/acme/s.git"));
    }

    #[test]
    fn a_taken_name_fails_without_a_terminal() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::write(home.join("projects/taken.yaml"), "name: taken\n").unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);
        let folder = temp.path().join("taken");
        std::fs::create_dir_all(&folder).unwrap();

        let preset = Preset { root: Some(folder), ..Preset::default() };
        let mut gh = FakeGh(false);
        let mut probe = FakeGh(false);
        let err = answer_steps(temp.path(), &preset, &mut Defaults { gh: &mut gh }, &mut probe).unwrap_err();
        assert!(err.to_string().contains("already exists"), "{err}");
    }

    #[test]
    fn a_committed_config_is_picked_up_and_skips_the_other_steps() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let temp = TempDir::new().unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", temp.path().join("home"));
        let repo = temp.path().join("clone");
        std::fs::create_dir_all(repo.join(".shelbi")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join(".shelbi/project.yaml"), "name: teamproj\n").unwrap();

        let preset = Preset { root: Some(repo.clone()), ..Preset::default() };
        let mut gh = FakeGh(false);
        let mut probe = FakeGh(false);
        let plan = answer_steps(temp.path(), &preset, &mut Defaults { gh: &mut gh }, &mut probe)
            .unwrap()
            .unwrap();
        assert!(plan.pick_up);
        assert_eq!(plan.name, "teamproj");
    }

    /// The plan the TUI's dialog hands over for `repo`, every step on its
    /// preselection: the folder's name typed in, Continue on each page, the
    /// GitHub preflight answered as passing.
    fn tui_plan(repo: &Path, mode: ConfigMode) -> NewProject {
        use shelbi_app::overlay::add_project::{Form, Page, Step as TuiStep};
        use shelbi_state::keymap::{Key, KeyChord, Mods};
        let enter = KeyChord::new(Key::Enter, Mods::NONE);
        let mut form = Form::new(repo);
        for c in shelbi_app::new_project::folder_name(repo).unwrap().chars() {
            form.handle_key(KeyChord::new(Key::Char(c), Mods::NONE));
        }
        assert_eq!(form.handle_key(enter), TuiStep::Validate);
        let check = project_create::check_folder(repo, form.root(), Some(form.name()));
        assert_eq!(form.folder_checked(check), TuiStep::Validate);
        project_create::validate_add_project(form.name(), form.root(), repo).unwrap();
        form.folder_valid();
        assert_eq!(form.handle_key(enter), TuiStep::Continue);
        if let TuiStep::Preflight(gh_repo) = form.handle_key(enter) {
            form.preflight_passed(&gh_repo);
        }
        assert_eq!(form.page(), Page::Settings);
        form.select_mode(mode);
        assert_eq!(form.handle_key(enter), TuiStep::Submit);
        form.plan().unwrap()
    }

    /// The plan the desktop wizard hands over for `repo`, every screen on
    /// its preselection, the GitHub preflight answered as passing.
    fn desktop_plan(repo: &Path, mode: ConfigMode) -> NewProject {
        use shelbi_app::add_project_wizard::{Action, Origin, Screen, Wizard};
        let mut w = Wizard::new(Origin::Workspace, None);
        w.folder_picked(Some(repo));
        let key = w.check_due().unwrap();
        w.begin_check(key.clone());
        let check = project_create::check_folder(repo, &key.path, key.name.as_deref());
        w.finish_check(key, check);
        assert_eq!(w.primary(), Action::Stay);
        assert_eq!(w.primary(), Action::Stay);
        if let Action::Preflight(gh_repo) = w.primary() {
            w.preflight_passed(&gh_repo);
        }
        assert_eq!(w.screen(), Screen::Settings);
        w.select_mode(mode);
        let Action::Create(request) = w.primary() else { panic!("Create Project") };
        request.plan
    }

    /// Every file under `dir`, its path relative to `base` and every
    /// mention of `base` in its text (and a task's timestamps) replaced, so
    /// two creations in two sandboxes compare byte for byte.
    fn snapshot(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if path.is_dir() {
                snapshot(base, &path, out);
            } else {
                let text = std::fs::read(&path).unwrap();
                let text = String::from_utf8_lossy(&text)
                    .replace(&*base.to_string_lossy(), "<sandbox>")
                    .lines()
                    // When it was created isn't an answer.
                    .map(|l| if l.starts_with("created_at: ") || l.starts_with("updated_at: ") { "<timestamp>" } else { l })
                    .collect::<Vec<_>>()
                    .join("\n");
                out.insert(path.strip_prefix(base).unwrap().to_path_buf(), text.into_bytes());
            }
        }
    }

    use std::collections::BTreeMap;

    /// The acceptance test for one engine: the same folder and answers,
    /// collected by `shelbi init -y` (the CLI path), the TUI's dialog and the
    /// desktop wizard, are the same plan, and the engine writes the same
    /// files for each, byte for byte (the registration, the workflows, the
    /// agents, the templates, the committed in-repo half).
    #[test]
    fn every_front_end_creates_byte_identical_projects() {
        let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        for mode in [ConfigMode::Global, ConfigMode::InRepo] {
            let mut files = Vec::new();
            let mut plans = Vec::new();
            for front_end in ["cli", "tui", "desktop"] {
                let temp = TempDir::new().unwrap();
                let base = temp.path().canonicalize().unwrap();
                env.set("SHELBI_HOME", base.join("home"));
                let repo = base.join("Web App");
                std::fs::create_dir_all(&repo).unwrap();
                git(&repo, &["init", "-q", "-b", "trunk"]);
                git(&repo, &["remote", "add", "origin", "https://tok@github.com/acme/web.git"]);

                let plan = match front_end {
                    "cli" => {
                        let preset = Preset { root: Some(repo.clone()), mode: Some(mode), ..Preset::default() };
                        let (mut gh, mut probe) = (FakeGh(true), FakeGh(true));
                        answer_steps(&base, &preset, &mut Defaults { gh: &mut gh }, &mut probe).unwrap().unwrap()
                    }
                    "tui" => tui_plan(&repo, mode),
                    _ => desktop_plan(&repo, mode),
                };
                // Folders differ per sandbox; everything else is an answer.
                plans.push(NewProject { root: PathBuf::new(), ..plan.clone() });
                let slug = project_create::create_project(
                    &plan,
                    Attempt::New,
                    &mut project_create::NullReporter,
                )
                .unwrap();
                assert_eq!(slug, "web-app");
                let mut snap = BTreeMap::new();
                snapshot(&base, &base, &mut snap);
                files.push((front_end, snap));
            }
            assert_eq!(plans[0], plans[1], "{mode:?}: the CLI and the TUI answer alike");
            assert_eq!(plans[0], plans[2], "{mode:?}: the CLI and the desktop answer alike");
            let plan = &plans[0];
            assert_eq!(plan.default_branch, "trunk");
            assert_eq!(plan.remote.as_deref(), Some("https://github.com/acme/web.git"));
            assert_eq!(plan.issue_tracker.github.as_ref().unwrap().repo, "acme/web");

            let (_, cli) = &files[0];
            let registration = match mode {
                ConfigMode::Global => PathBuf::from("home/projects/web-app.yaml"),
                ConfigMode::InRepo => PathBuf::from("Web App/.shelbi/project.yaml"),
            };
            let yaml = String::from_utf8(cli[&registration].clone()).unwrap();
            assert!(yaml.contains("default_branch: trunk"), "{yaml}");
            assert!(yaml.contains("github_url: https://github.com/acme/web.git"), "{yaml}");
            assert!(!yaml.contains("tok@"), "credentials never land on disk:\n{yaml}");
            for (front_end, snap) in &files[1..] {
                let paths = |s: &BTreeMap<PathBuf, Vec<u8>>| s.keys().cloned().collect::<Vec<_>>();
                assert_eq!(paths(cli), paths(snap), "{mode:?}: {front_end} wrote other files than the CLI");
                for (path, bytes) in cli {
                    assert!(
                        snap[path] == *bytes,
                        "{mode:?}: {front_end}'s {} differs from the CLI's:\n--- cli\n{}\n--- {front_end}\n{}",
                        path.display(),
                        String::from_utf8_lossy(bytes),
                        String::from_utf8_lossy(&snap[path]),
                    );
                }
            }
        }
    }
}
