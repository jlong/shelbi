//! The one project-creation engine. Every front end that creates a project
//! calls it: `shelbi init` (interactive and `-y`), bare `shelbi`'s first run,
//! `shelbi project add`, the TUI's Add project dialog and the desktop's Add
//! Project wizard. They walk the same steps (`shelbi_app::new_project::Step`),
//! start every step on the same detected defaults ([`check_folder`]), and hand
//! the answers ([`NewProject`]) to [`create_project`], which validates them and
//! writes the project. Nothing else writes one, so the same answers produce the
//! same files from every front end.
//!
//! The engine owns detection ([`detect`]), the issue-tracker checks
//! ([`issues`]), validation ([`validate_add_project`]), the starter YAML
//! ([`render_project_yaml`]), the atomic registration
//! ([`write_new_project_registration`]), the agents/workflows/statuses/Zen
//! scaffold, the commit-guard install, and picking up a teammate's committed
//! config ([`pick_up`]).
//!
//! Human-readable progress goes through the [`ScaffoldReporter`] seam rather
//! than straight to stdout/stderr, so the TUI (inside a ratatui alt-screen)
//! drops it, the desktop draws its checklist from the [`ScaffoldStage`]
//! callbacks, and the CLI prints every line.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{anyhow, bail, Context, Result};

pub use shelbi_app::new_project::NewProject;
use shelbi_core::{AgentDefaults, ConfigMode, IssueTrackerConfig, RunnerKind};
use shelbi_state::AgentMaterializeOutcome;

pub mod detect;
pub mod issues;
mod pick_up;

pub use detect::{check_folder, installed_runners, remote_url_for_storage, InstalledRunner};
pub use pick_up::{pick_up, PickedUp};

/// Filename of the committed, in-repo project config, written under
/// `<repo>/.shelbi/project.yaml` for `mode: in-repo`.
pub const IN_REPO_CONFIG_REL: &str = ".shelbi/project.yaml";

// ---------------------------------------------------------------------------
// Progress reporting
// ---------------------------------------------------------------------------

/// Sink for the human-readable progress lines the scaffold engine emits.
///
/// `note` is an ordinary stdout-style status line (`✓ wrote project: …`);
/// `warn` is a stderr-style caution (the commit-guard disclosure, a foreign
/// hook left untouched). Each line carries no trailing newline — the sink adds
/// one if it prints. The CLI's implementation prints `note`→stdout and
/// `warn`→stderr verbatim, preserving the exact output `shelbi init` had before
/// this engine moved out of `shelbi-cli`.
///
/// The stage callbacks are the structured half: [`scaffold_project`] says
/// when each [`ScaffoldStage`] starts, finishes, or fails, so a caller that
/// draws progress (the desktop wizard's checklist) never has to parse the
/// lines. They default to nothing, so a line-only reporter (the CLI's) is
/// unchanged.
pub trait ScaffoldReporter {
    fn note(&mut self, line: &str);
    fn warn(&mut self, line: &str);
    /// `stage`'s work begins. Exactly one stage runs at a time.
    fn stage_started(&mut self, _stage: ScaffoldStage) {}
    /// `stage`'s work all succeeded.
    fn stage_finished(&mut self, _stage: ScaffoldStage) {}
    /// `stage` failed with `error`; nothing after it ran.
    fn stage_failed(&mut self, _stage: ScaffoldStage, _error: &str) {}
}

/// The parts of [`scaffold_project`], in the order they run. Every write the
/// scaffold makes belongs to one, so the last one finishing means the project
/// is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScaffoldStage {
    /// The registration (`projects/<id>.yaml`, or `<id>/local.yaml` plus the
    /// committed `<repo>/.shelbi/project.yaml` in-repo), the onboarding seeds
    /// published with it, and the workspace-settings template.
    Config,
    /// The default agents' workspaces (`agents/<name>/`).
    Agents,
    /// The Task and Subtask workflows and the status catalogue, then the rest
    /// of the project's scaffold: the Zen policy, the PR template, the shared
    /// agent preamble and the commit guard.
    Workflows,
}

impl ScaffoldStage {
    /// Every stage, in order.
    pub const ALL: [ScaffoldStage; 3] =
        [ScaffoldStage::Config, ScaffoldStage::Agents, ScaffoldStage::Workflows];
}

impl From<ScaffoldStage> for shelbi_app::add_project_wizard::Stage {
    fn from(stage: ScaffoldStage) -> Self {
        use shelbi_app::add_project_wizard::Stage;
        match stage {
            ScaffoldStage::Config => Stage::Config,
            ScaffoldStage::Agents => Stage::Agents,
            ScaffoldStage::Workflows => Stage::Workflows,
        }
    }
}

/// One stage callback, as [`CollectReporter`] records it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StageEvent {
    Started(ScaffoldStage),
    Finished(ScaffoldStage),
    Failed(ScaffoldStage, String),
}

/// A reporter that drops every line — the TUI path, which surfaces a single
/// status-line summary on completion rather than the scrolling scaffold log.
pub struct NullReporter;

impl ScaffoldReporter for NullReporter {
    fn note(&mut self, _line: &str) {}
    fn warn(&mut self, _line: &str) {}
}

/// A reporter that collects every line, for tests (asserting the exact output)
/// and for any caller that wants to render the scaffold log itself.
#[derive(Debug, Default)]
pub struct CollectReporter {
    pub notes: Vec<String>,
    pub warnings: Vec<String>,
    /// The stage callbacks, in order.
    pub stages: Vec<StageEvent>,
}

impl ScaffoldReporter for CollectReporter {
    fn note(&mut self, line: &str) {
        self.notes.push(line.to_string());
    }
    fn warn(&mut self, line: &str) {
        self.warnings.push(line.to_string());
    }
    fn stage_started(&mut self, stage: ScaffoldStage) {
        self.stages.push(StageEvent::Started(stage));
    }
    fn stage_finished(&mut self, stage: ScaffoldStage) {
        self.stages.push(StageEvent::Finished(stage));
    }
    fn stage_failed(&mut self, stage: ScaffoldStage, error: &str) {
        self.stages.push(StageEvent::Failed(stage, error.to_string()));
    }
}

// ---------------------------------------------------------------------------
// Project-root validation
// ---------------------------------------------------------------------------

/// Outcome of validating a candidate project-root path. Non-OK variants carry
/// only the discriminant; the caller renders the user-facing message because it
/// differs between the interactive prompt (re-prompt with `✗`), the scripted
/// `--root` path (hard error), and the Add-project form (inline error).
#[derive(Debug, PartialEq, Eq)]
pub enum RootValidation {
    Ok,
    NotExists,
    NotDirectory,
    /// Directory exists but doesn't look like a git repo. A warning, not an
    /// error — Shelbi's workflow assumes git, but nothing in the scaffold
    /// actively rejects a non-git dir.
    NotGitRepo,
}

/// Pure validator: no prompting, no global state. Checks (in order):
/// 1. path exists
/// 2. path is a directory
/// 3. path is a git repo (has `.git`, OR `git rev-parse --git-dir` succeeds
///    inside it — the latter catches working trees whose `.git` is a regular
///    file pointing at the gitdir).
pub fn validate_root(path: &Path) -> RootValidation {
    if !path.exists() {
        return RootValidation::NotExists;
    }
    let is_dir = std::fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false);
    if !is_dir {
        return RootValidation::NotDirectory;
    }
    if !is_git_repo(path) {
        return RootValidation::NotGitRepo;
    }
    RootValidation::Ok
}

fn is_git_repo(path: &Path) -> bool {
    if path.join(".git").exists() {
        return true;
    }
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--git-dir"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Validate a project name before it's used as a filesystem path component
/// (`~/.shelbi/projects/<name>.yaml`, `~/.shelbi/projects/<name>/`) and
/// interpolated into on-disk config. Delegates to
/// [`shelbi_core::validate_project_name`] — the storage-layer chokepoint — so
/// the pre-check and the on-disk invariant can't drift: a name must be a single
/// path component of lowercase `[a-z0-9_-]` starting with a letter or digit.
pub fn validate_project_name(name: &str) -> Result<()> {
    shelbi_core::validate_project_name(name).map_err(|_| {
        anyhow!(
            "project name `{name}` is invalid — it must be a single path component of \
             lowercase `[a-z0-9_-]` starting with a letter or digit (no `/`, `..`, spaces, \
             uppercase, or leading `.`). Pass --project NAME with a name like `my-app`."
        )
    })
}

/// Project name derived from a chosen root: the basename of the path,
/// unchanged. `None` when the path has no usable file component (e.g. `/`).
pub fn project_name_from_root(path: &Path) -> Option<String> {
    path.file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
}

/// Whether either supported local registration for `name` already exists. The
/// scaffolder uses this to refuse to silently overwrite or shadow a
/// pre-existing flat or split project registration.
pub fn project_name_collides(name: &str) -> Result<bool> {
    shelbi_state::has_project_registration(name).map_err(|e| anyhow!(e))
}

/// Expand `~` / `~/…` against `$HOME` and resolve relative paths against `cwd`.
/// Stops short of `canonicalize` because that fails on non-existent paths — we
/// want validation to report the user's typed path verbatim, not a canonical
/// form they didn't type.
pub fn absolutize(cwd: &Path, path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    let expanded: PathBuf = if raw == "~" {
        shelbi_core::home_dir().unwrap_or_else(|| path.to_path_buf())
    } else if let Some(rest) = raw.strip_prefix("~/") {
        match shelbi_core::home_dir() {
            Some(h) => h.join(rest),
            None => path.to_path_buf(),
        }
    } else {
        path.to_path_buf()
    };

    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

/// Resolved + validated project root, plus the derived project name.
#[derive(Debug, Clone)]
pub struct ResolvedProjectRoot {
    pub path: PathBuf,
    /// The slug/id — a valid `[a-z0-9_-]` path component. Keys the on-disk
    /// folder, settings file, tmux session, and every state entry.
    pub name: String,
    /// The human-readable label the name was derived from, recorded only when
    /// slugifying actually changed it (e.g. `ContextStore` → `contextstore`).
    /// `None` when the entered name already equalled its slug, so a project
    /// whose name needs no massaging stays free of a redundant `display_name`.
    pub display_name: Option<String>,
}

// ---------------------------------------------------------------------------
// Add-project form validation
// ---------------------------------------------------------------------------

/// The add-project form's "Detected:" line for the repo path `root` (as
/// typed, resolved against `cwd`): the git-repo check [`validate_root`] does.
/// Runs git, so callers keep it off any UI thread they can.
pub fn detect_root(cwd: &Path, root: &str) -> shelbi_app::overlay::add_project::Detected {
    use shelbi_app::overlay::add_project::Detected;
    let trimmed = root.trim();
    if trimmed.is_empty() {
        return Detected::NotFound;
    }
    match validate_root(&absolutize(cwd, Path::new(trimmed))) {
        RootValidation::Ok => Detected::GitRepo,
        RootValidation::NotGitRepo => Detected::NotGitRepo,
        RootValidation::NotExists | RootValidation::NotDirectory => Detected::NotFound,
    }
}

/// Why [`create_project`] didn't create the project.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddProjectError {
    /// The form's input doesn't validate: show it on the form and keep it open.
    Invalid(String),
    /// Validation passed but scaffolding failed.
    Failed(String),
}

/// Which [`add_project`] call this is for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Attempt {
    /// The first: an id that's already registered is a clash.
    New,
    /// Finishing the same request after a failed attempt: a registration this
    /// request's earlier attempt published (same id, same folder, same config
    /// mode) is picked up and the scaffold completed around it, keeping every
    /// file already there. Any other registration under the id is still a
    /// clash.
    Retry,
}

/// Create the project `plan` describes: the one path every front end
/// takes. Validates the answers (the name and folder with
/// [`validate_add_project`], or under [`Attempt::Retry`] its retry form; the
/// agent defaults; the branch; that a GitHub board won't strand a local one),
/// then runs the [`ScaffoldStage`]s, reporting to `out`: initialize Git when
/// the plan approved it, make sure the Shelbi home is laid out, register the
/// project and write its config (the detected branch and remote recorded, an
/// empty workspace pool the orchestrator fills on first boot), its agents
/// (recommending the chosen runner), and its workflows, Zen policy and
/// templates, then install the commit guard. A plan that picks up a committed
/// config runs [`pick_up`] instead. Returns the project's id. Blocking (file
/// IO and git); run it off any UI thread.
pub fn create_project(
    plan: &NewProject,
    attempt: Attempt,
    out: &mut dyn ScaffoldReporter,
) -> std::result::Result<String, AddProjectError> {
    if plan.pick_up {
        // A name other than the committed one is the local alias to use.
        let committed = detect::read_in_repo_name(&plan.root.join(IN_REPO_CONFIG_REL));
        let name = plan.name.trim();
        let alias = (!name.is_empty() && committed.as_deref() != Some(name)).then_some(name);
        return pick_up(&plan.root, alias, out)
            .map(|picked| picked.alias)
            .map_err(|e| AddProjectError::Failed(format!("Add project failed: {e:#}")));
    }
    check_agent_defaults(&plan.defaults).map_err(|e| AddProjectError::Invalid(e.to_string()))?;
    let retry = (attempt == Attempt::Retry).then_some(plan.mode);
    let root = plan.root.to_string_lossy();
    let resolved = validate_request(&plan.name, &root, &plan.root, retry).map_err(AddProjectError::Invalid)?;
    let branch = plan.default_branch.trim();
    shelbi_core::validate_branch(branch)
        .map_err(|e| AddProjectError::Invalid(format!("`{branch}` isn't a usable default branch: {e}")))?;
    issues::ensure_board_not_stranded(&resolved.name, &plan.issue_tracker)
        .map_err(|e| AddProjectError::Invalid(e.to_string()))?;
    if plan.git_init && validate_root(&resolved.path) == RootValidation::NotGitRepo && !git_available() {
        return Err(AddProjectError::Invalid(
            "Git was not found on PATH. Install Git, then try again.".into(),
        ));
    }
    // Everything that could stop the scaffold halfway is checked before the
    // first write (a `git init` included).
    preflight_persistence(&resolved.name).map_err(|e| AddProjectError::Failed(format!("Add project failed: {e:#}")))?;
    scaffold_project(&resolved, plan, out)
        .map_err(|e| AddProjectError::Failed(format!("Add project failed: {e}")))?;
    Ok(resolved.name)
}

/// `git` runs on this machine.
fn git_available() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Verify every state destination the scaffold writes is a usable, writable
/// directory (or can become one) before anything is written, so a blocked
/// destination fails the creation with nothing changed, not halfway through.
/// Write probes are sibling temp files removed at once.
fn preflight_persistence(project: &str) -> Result<()> {
    let err = |e| anyhow!("{e}");
    let home = shelbi_state::shelbi_home().map_err(err)?;
    let projects = shelbi_state::projects_dir().map_err(err)?;
    let sessions = shelbi_state::sessions_dir().map_err(err)?;
    let project_dir = shelbi_state::project_dir(project).map_err(err)?;
    let agents = shelbi_state::agents_dir(project).map_err(err)?;
    let workflows = shelbi_state::workflows_dir(project).map_err(err)?;
    let tasks = shelbi_state::tasks_dir(project).map_err(err)?;
    let ssh = shelbi_state::ssh_control_dir().map_err(err)?;
    for directory in [&home, &projects, &sessions, &project_dir, &agents, &workflows, &tasks] {
        ensure_directory_or_missing(directory)?;
        probe_nearest_writable_directory(directory)?;
    }
    for directory in shelbi_state::STANDARD_SUBDIRS.iter().map(|name| home.join(name)).chain(std::iter::once(ssh)) {
        ensure_directory_or_missing(&directory)?;
    }
    let default_session = sessions.join("default.yaml");
    if default_session.exists() {
        let text = std::fs::read_to_string(&default_session)
            .with_context(|| format!("reading {}", default_session.display()))?;
        yaml_serde::from_str::<shelbi_core::Session>(&text).with_context(|| {
            format!(
                "{} is not a valid Shelbi session; repair or remove it, then try again",
                default_session.display()
            )
        })?;
    }
    Ok(())
}

fn ensure_directory_or_missing(path: &Path) -> Result<()> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => bail!("{} exists but is not a directory", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("inspecting {}", path.display())),
    }
}

fn probe_nearest_writable_directory(path: &Path) -> Result<()> {
    let mut ancestor = path;
    loop {
        match std::fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => break,
            Ok(_) => bail!("{} exists but is not a directory", ancestor.display()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                ancestor = ancestor
                    .parent()
                    .ok_or_else(|| anyhow!("no existing parent directory for {}", path.display()))?;
            }
            Err(error) => return Err(error).with_context(|| format!("inspecting {}", ancestor.display())),
        }
    }
    let (temp, file) = create_sibling_temp(&ancestor.join(".shelbi-init-write-probe"))
        .with_context(|| format!("Shelbi state destination {} is not writable", path.display()))?;
    drop(file);
    std::fs::remove_file(&temp).with_context(|| format!("removing state write probe {}", temp.display()))?;
    Ok(())
}

/// Whether a new project can be created with `defaults`: a generic runner
/// has no command Shelbi could scaffold, so a new project (which declares
/// only Claude Code and Codex) can't default to one. The same refusal the
/// orchestrator onboarding gives an undeclared generic runner.
pub fn check_agent_defaults(
    defaults: &AgentDefaults,
) -> std::result::Result<(), shelbi_state::orchestrator_setup::RunnerSetupError> {
    match defaults.runner {
        RunnerKind::Generic => {
            Err(shelbi_state::orchestrator_setup::RunnerSetupError::NotDeclared(RunnerKind::Generic))
        }
        RunnerKind::Claude | RunnerKind::Codex => Ok(()),
    }
}

/// Validate the "Add project" form (a name + a repo path, resolved against
/// `cwd`) into a ready-to-scaffold [`ResolvedProjectRoot`], or a user-facing
/// inline error string. Any human-readable name is accepted — it's slugified
/// into the on-disk id, and the collision / path guards run against that slug,
/// mirroring what `shelbi init` does before anything is written.
///
/// Shared by the CLI palette's Add-project dialog and the in-process TUI
/// overlay so both entry points validate identically.
pub fn validate_add_project(
    name: &str,
    root: &str,
    cwd: &Path,
) -> std::result::Result<ResolvedProjectRoot, String> {
    validate_request(name, root, cwd, None)
}

/// [`validate_add_project`], or with `retry` (the request's config mode) its
/// [`Attempt::Retry`] form: an existing registration passes only when
/// [`registration_is_for`] this folder and mode.
fn validate_request(
    name: &str,
    root: &str,
    cwd: &Path,
    retry: Option<ConfigMode>,
) -> std::result::Result<ResolvedProjectRoot, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Enter a project name.".to_string());
    }
    // Slugify for storage. Only an empty / all-punctuation name has nothing to
    // build an id from — every other human-readable name is accepted.
    let slug = shelbi_core::normalize_project_name(name).ok().ok_or_else(|| {
        format!("`{name}` has no letters or digits to build an id from — try a name like `my-app`.")
    })?;
    let taken = || format!("a project already lives at `{slug}` — pick a different name.");
    // Collision is checked on the SLUG (the folder / settings-file id), not the
    // display name, so two different labels that slugify the same still clash.
    // A retry decides once the folder is known.
    let collides = project_name_collides(&slug).map_err(|e| e.to_string())?;
    if collides && retry.is_none() {
        return Err(taken());
    }

    let root_raw = root.trim();
    if root_raw.is_empty() {
        return Err("Enter the project's repo path.".to_string());
    }
    let path = absolutize(cwd, Path::new(root_raw));
    match validate_root(&path) {
        // A non-git dir is allowed (Shelbi expects git but doesn't hard-reject),
        // matching `shelbi init --root`'s warn-and-continue behavior.
        RootValidation::Ok | RootValidation::NotGitRepo => {}
        RootValidation::NotExists => return Err(format!("{} does not exist.", path.display())),
        RootValidation::NotDirectory => {
            return Err(format!("{} is not a directory.", path.display()))
        }
    }
    if let (true, Some(mode)) = (collides, retry) {
        if !registration_is_for(&slug, &path, mode) {
            return Err(taken());
        }
    }

    // The display label is the raw human-readable name, kept only when
    // slugifying changed it (so a clean slug never grows a redundant label).
    let display_name = (slug != name).then(|| name.to_string());
    Ok(ResolvedProjectRoot {
        path,
        name: slug,
        display_name,
    })
}

/// Whether `slug`'s registration is the one creating a project at `root` in
/// `mode` publishes: the registration file for that mode (the flat
/// `projects/<id>.yaml` for global, the split `projects/<id>/local.yaml`
/// in-repo, never both), whose hub machine works in `root`. Reads the file
/// itself rather than loading the project, so a registration whose scaffold
/// stopped early (no committed in-repo half yet) still answers.
fn registration_is_for(slug: &str, root: &Path, mode: ConfigMode) -> bool {
    let Ok(projects) = shelbi_state::projects_dir() else {
        return false;
    };
    let flat = projects.join(format!("{slug}.yaml"));
    let split = projects.join(slug).join("local.yaml");
    let file = match (mode, flat.is_file(), split.is_file()) {
        (ConfigMode::Global, true, false) => flat,
        (ConfigMode::InRepo, false, true) => split,
        _ => return false,
    };
    let Ok(text) = std::fs::read_to_string(&file) else {
        return false;
    };
    let Ok(doc) = yaml_serde::from_str::<yaml_serde::Value>(&text) else {
        return false;
    };
    let same = |dir: &str| {
        let dir = Path::new(dir);
        dir == root
            || matches!(
                (dir.canonicalize(), root.canonicalize()),
                (Ok(a), Ok(b)) if a == b
            )
    };
    doc.get("machines")
        .and_then(|m| m.as_sequence())
        .into_iter()
        .flatten()
        .filter(|m| m.get("name").and_then(|n| n.as_str()) == Some("hub"))
        .filter_map(|m| m.get("work_dir").and_then(|w| w.as_str()))
        .any(same)
}

// ---------------------------------------------------------------------------
// Starter YAML rendering
// ---------------------------------------------------------------------------

/// The small, stable starter surface `shelbi init` emits. Built with
/// `yaml_serde` (never `format!`) so a `work_dir` containing ` #` or a name
/// containing `: ` is quoted/escaped rather than silently corrupting the file.
#[derive(serde::Serialize)]
struct ProjectYaml<'a> {
    /// Free-form human display **label**. The project **id** is the config
    /// file's basename, so this key carries the id only when the entered name
    /// was slugified into a different id.
    #[serde(rename = "name", skip_serializing_if = "Option::is_none")]
    label: Option<&'a str>,
    repo: &'a str,
    default_branch: &'a str,
    /// Emitted only for in-repo projects. Global projects omit the key.
    #[serde(skip_serializing_if = "Option::is_none")]
    config_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_workflow: Option<&'a str>,
    /// The origin remote, credentials removed.
    #[serde(skip_serializing_if = "Option::is_none")]
    github_url: Option<&'a str>,
    machines: Vec<MachineYaml<'a>>,
    orchestrator: OrchestratorYaml<'a>,
    /// The runner fleet's house model, written only when the new-project
    /// flow chose a model ([`AgentDefaults::model`]). Before `workspaces:`
    /// so the decorator's workspace and runner hints still land where they
    /// belong.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    runners: BTreeMap<&'a str, FleetRunnerYaml<'a>>,
    agent_runners: BTreeMap<&'a str, RunnerYaml<'a>>,
    #[serde(skip_serializing_if = "IssueTrackerConfig::is_default")]
    issue_tracker: &'a IssueTrackerConfig,
}

#[derive(serde::Serialize)]
struct MachineYaml<'a> {
    name: &'a str,
    kind: &'a str,
    work_dir: &'a str,
}

#[derive(serde::Serialize)]
struct OrchestratorYaml<'a> {
    runner: &'a str,
}

#[derive(serde::Serialize)]
struct RunnerYaml<'a> {
    command: &'a str,
    flags: Vec<&'a str>,
}

/// A `runners.<kind>` fleet entry: the launcher (the same command as its
/// `agent_runners` entry) and the model every agent of that kind runs.
#[derive(serde::Serialize)]
struct FleetRunnerYaml<'a> {
    command: &'a str,
    model: &'a str,
}

/// What a new project's registration records: the answers that land in its
/// YAML.
#[derive(Debug, Clone, Copy)]
pub struct Starter<'a> {
    /// The id.
    pub name: &'a str,
    /// The human label, when slugifying changed the entered name.
    pub display_name: Option<&'a str>,
    /// The folder: the `repo` and the hub machine's `work_dir`.
    pub work_dir: &'a Path,
    pub default_branch: &'a str,
    /// The origin remote (credentials already removed).
    pub github_url: Option<&'a str>,
    pub issue_tracker: &'a IssueTrackerConfig,
    pub defaults: &'a AgentDefaults,
}

/// Render the starter project YAML via serde (never string interpolation),
/// so a `work_dir` containing ` #` or a name containing `: ` is quoted
/// rather than corrupting the file. Validates the name and the agent
/// defaults first. `orchestrator.runner` names the chosen kind's
/// `agent_runners` entry, a chosen model is the fleet's house model
/// (`runners.<kind>.model`), and the workspace pool starts empty: the
/// orchestrator's first boot provisions it, on the chosen runner.
pub fn render_project_yaml(starter: &Starter, config_mode: Option<&str>) -> Result<String> {
    validate_project_name(starter.name)?;
    check_agent_defaults(starter.defaults).map_err(|e| anyhow!(e))?;
    let runner = starter.defaults.runner.as_str();
    let work_dir = starter.work_dir.to_string_lossy();
    let mut agent_runners = BTreeMap::new();
    for kind in detect::SUPPORTED_RUNNERS {
        let command = kind.as_str();
        agent_runners.insert(command, RunnerYaml { command, flags: vec![] });
    }
    let doc = ProjectYaml {
        label: starter.display_name,
        repo: &work_dir,
        default_branch: starter.default_branch,
        config_mode,
        default_workflow: Some(shelbi_core::TASK_WORKFLOW_NAME),
        github_url: starter.github_url,
        machines: vec![MachineYaml {
            name: "hub",
            kind: "local",
            work_dir: &work_dir,
        }],
        orchestrator: OrchestratorYaml { runner },
        runners: starter
            .defaults
            .model
            .as_deref()
            .map(|model| (runner, FleetRunnerYaml { command: runner, model }))
            .into_iter()
            .collect(),
        agent_runners,
        issue_tracker: starter.issue_tracker,
    };
    let active = yaml_serde::to_string(&doc).context("serializing project YAML")?;
    Ok(shelbi_core::scaffold::decorate_project_yaml(&active))
}

/// Render the two halves of an in-repo project's config: the committed shared
/// half (`<repo>/.shelbi/project.yaml`) and the user-local half
/// (`~/.shelbi/projects/<id>/local.yaml`). Returns `(shared_body, local_body)`.
pub fn render_split_project_yaml(starter: &Starter) -> Result<(String, String)> {
    let flat = render_project_yaml(&Starter { display_name: None, ..*starter }, Some("in-repo"))?;
    let mut project = shelbi_core::Project::from_yaml_str(&flat).map_err(|e| anyhow!(e))?;
    // The id lives in the filename/registry dir, never a YAML key, but the
    // committed file's `name:` is pick-up's anchor — force it to the id.
    project.name = starter.name.to_string();
    project.label = Some(starter.name.to_string());
    project.display_name = starter.display_name.map(str::to_string);
    project.config_mode = Some(ConfigMode::InRepo);
    let shared = project.to_shared_yaml_string().map_err(|e| anyhow!(e))?;
    let local = project.to_local_yaml_string().map_err(|e| anyhow!(e))?;
    Ok((shared, local))
}

// ---------------------------------------------------------------------------
// Atomic registration + on-disk scaffold
// ---------------------------------------------------------------------------

/// Create a uniquely-named sibling temp file next to `path`, for a
/// write-then-publish. Mirrors the CLI wizard's helper.
fn create_sibling_temp(path: &Path) -> Result<(PathBuf, std::fs::File)> {
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);
    let parent = path
        .parent()
        .ok_or_else(|| anyhow!("{} has no parent directory", path.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("shelbi-file");
    for _ in 0..100 {
        let nonce = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{file_name}.tmp-{}-{nonce}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(file) => return Ok((candidate, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("creating {}", candidate.display()));
            }
        }
    }
    bail!(
        "could not reserve a temporary file next to {}",
        path.display()
    )
}

/// Publish a new local registry entry while atomically arming its first-launch
/// greeting with respect to dashboard bootstrap. Returns `true` if this call
/// published the registration, `false` if one already existed.
pub fn write_new_project_registration(
    project: &str,
    registration_path: &Path,
    body: &str,
) -> Result<bool> {
    use std::io::Write;

    if let Some(parent) = registration_path.parent() {
        shelbi_state::ensure_dir(parent).map_err(|e| anyhow!(e))?;
    }

    // Build the complete registration under a sibling temp name first. A
    // process crash before the final hard link therefore leaves no
    // discoverable partial or fully written-but-unarmed project.
    let (temp_path, mut temp_file) = create_sibling_temp(registration_path)?;
    if let Err(error) = temp_file.write_all(body.as_bytes()) {
        drop(temp_file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("writing {}", temp_path.display()));
    }
    drop(temp_file);

    let _dashboard_lock = shelbi_state::lock_dashboard(project).map_err(|e| anyhow!(e))?;
    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let flat_registration = projects_dir.join(format!("{project}.yaml"));
    let split_registration = projects_dir.join(project).join("local.yaml");
    if flat_registration.exists() || split_registration.exists() {
        let _ = std::fs::remove_file(&temp_path);
        return Ok(false);
    }
    shelbi_state::arm_contextual_greeting(project).map_err(|e| anyhow!(e))?;
    let publish = std::fs::hard_link(&temp_path, registration_path);
    let _ = std::fs::remove_file(&temp_path);
    match publish {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            shelbi_state::claim_contextual_greeting(project).map_err(|e| anyhow!(e))?;
            Ok(false)
        }
        Err(error) => Err(error).with_context(|| {
            format!(
                "publishing {} as {}",
                temp_path.display(),
                registration_path.display()
            )
        }),
    }
}

/// Write the committed shared half to `<repo>/.shelbi/project.yaml`. Idempotent
/// — a pre-existing file is left alone (a previous run, or a teammate committed
/// it): the committed config is a git-tracked contract with every future clone,
/// so init never clobbers one it finds.
fn write_in_repo_shared(
    root: &Path,
    shared_body: &str,
    out: &mut dyn ScaffoldReporter,
) -> Result<()> {
    let dir = root.join(".shelbi");
    let path = dir.join("project.yaml");
    if path.exists() {
        out.note(&format!("(in-repo config already exists at {})", path.display()));
        return Ok(());
    }
    shelbi_state::ensure_dir(&dir).map_err(|e| anyhow!(e))?;
    std::fs::write(&path, shared_body)?;
    out.note(&format!("✓ wrote in-repo config: {}", path.display()));
    Ok(())
}

/// Write the per-project `workspace-settings.json.template`. Mode-aware: the
/// project YAML has already been written by the time this runs, so
/// `config_project_dir` can read the mode back off disk.
pub fn write_workspace_settings_template(
    project: &str,
    out: &mut dyn ScaffoldReporter,
) -> Result<()> {
    let template_path = shelbi_state::config_project_dir(project)
        .map_err(|e| anyhow!(e))?
        .join("workspace-settings.json.template");
    if template_path.exists() {
        out.note(&format!(
            "(workspace settings template already exists at {})",
            template_path.display()
        ));
        return Ok(());
    }
    shelbi_state::ensure_dir(template_path.parent().unwrap()).map_err(|e| anyhow!(e))?;
    std::fs::write(&template_path, shelbi_state::DEFAULT_WORKSPACE_SETTINGS_TEMPLATE)?;
    out.note(&format!(
        "✓ wrote workspace settings template: {}",
        template_path.display()
    ));
    Ok(())
}

/// Stringify a [`shelbi_state::AgentMaterializeOutcome`] for the init / reload
/// report. One renderer so the user sees the same wording for the same outcome
/// regardless of which path (init scaffold here, or CLI reload) touched the
/// agent workspace.
pub fn render_agent_materialize_outcome(outcome: &AgentMaterializeOutcome) -> String {
    match outcome {
        AgentMaterializeOutcome::Created { agent } => {
            format!("✓ created agent workspace: agents/{agent}/")
        }
        AgentMaterializeOutcome::Unchanged { agent } => {
            format!("(agent workspace already exists: agents/{agent}/)")
        }
        AgentMaterializeOutcome::Upgraded { agent } => format!(
            "✓ upgraded agents/{agent}/instructions.md to the new bundled default \
             (was the previous default, untouched — nothing to preserve)"
        ),
        AgentMaterializeOutcome::Preserved {
            agent,
            first_notice,
        } => {
            if *first_notice {
                format!(
                    "(preserved your custom agents/{agent}/instructions.md — \
                     differs from the bundled default; the project owns the override)"
                )
            } else {
                format!("(preserved your custom agents/{agent}/instructions.md)")
            }
        }
        AgentMaterializeOutcome::MigratedZenCommands { agent, .. } => format!(
            "✓ pinned legacy Zen PR commands in custom agents/{agent}/instructions.md \
             (all other prose preserved)"
        ),
        AgentMaterializeOutcome::RepairedRequiredSections { agent, sections, .. } => format!(
            "✓ repaired custom agents/{agent}/instructions.md — added required section(s): {}",
            sections.join(", ")
        ),
    }
}

/// Install the context-scoped default-branch commit guard from the scaffold
/// path: disclose on a fresh install (`warn`, i.e. stderr in the CLI), caution
/// when a foreign hook blocks it, and stay silent on the "already there" refresh
/// and on best-effort failures (e.g. a not-yet-git root — `shelbi guard install`
/// can add it later). Never fails the scaffold: a hook is a convenience.
fn install_guard_at_scaffold(
    project: &shelbi_core::Project,
    work_dir: &Path,
    out: &mut dyn ScaffoldReporter,
) {
    use crate::githook::{self, HookInstall, InstallMode};
    let protected = crate::protected_default_branches(project);
    let refs: Vec<&str> = protected.iter().map(String::as_str).collect();
    match githook::install_hub_branch_guard(work_dir, &refs, InstallMode::CreateIfMissing) {
        Ok(HookInstall::Installed) => out.warn(&githook::hub_branch_guard_disclosure(work_dir)),
        Ok(HookInstall::SkippedForeignHook) => out.warn(&format!(
            "shelbi: {}/.git/hooks/pre-commit is user-authored — left untouched. \
             The default-branch commit guard was NOT installed.",
            work_dir.display()
        )),
        // Refreshed (already installed) and any best-effort error stay quiet.
        _ => {}
    }
}

/// Write the project [`create_project`] validated: the [`ScaffoldStage`]s, in
/// order, each reported started then finished (or failed, which stops the
/// scaffold there). Every step is individually idempotent, so a run
/// interrupted after registration finishes on retry. In-repo mode also
/// writes the committed `<repo>/.shelbi/project.yaml` a teammate's
/// [`pick_up`] consumes. For a runner other than Claude Code the shipped
/// agents recommend it ([`shelbi_state::recommend_runner_for_new_agents`],
/// which leaves any `agent.yaml` that isn't the shipped text alone).
fn scaffold_project(resolved: &ResolvedProjectRoot, plan: &NewProject, out: &mut dyn ScaffoldReporter) -> Result<()> {
    // Held from the collision check through the last write.
    let _scaffold_lock = run_stage(out, ScaffoldStage::Config, |out| scaffold_config(resolved, plan, out))?;
    run_stage(out, ScaffoldStage::Agents, |out| scaffold_agents(&resolved.name, plan.defaults.runner, out))?;
    run_stage(out, ScaffoldStage::Workflows, |out| scaffold_workflows(&resolved.name, out))?;
    // Disclose and install the context-scoped default-branch commit guard
    // now, with the user's knowledge: this is the consented install. Project
    // open only *refreshes* an already-installed hook, so it's never created
    // silently. Best-effort: a non-git root or a foreign hook degrades quietly.
    if let Ok(project) = shelbi_state::load_project(&resolved.name) {
        install_guard_at_scaffold(&project, &resolved.path, out);
    }
    Ok(())
}

/// [`ScaffoldStage::Agents`]: the default agents, recommending `runner`.
fn scaffold_agents(project: &str, runner: RunnerKind, out: &mut dyn ScaffoldReporter) -> Result<()> {
    let outcomes = shelbi_state::self_heal_default_agents(project).map_err(|e| anyhow!(e))?;
    for outcome in outcomes {
        out.note(&render_agent_materialize_outcome(&outcome));
    }
    if runner != RunnerKind::Claude {
        shelbi_state::recommend_runner_for_new_agents(project, runner).map_err(|e| anyhow!(e))?;
    }
    Ok(())
}

/// Lay out the Shelbi home every project needs: the standard directories,
/// the default session (`sessions/default.yaml`) and the self-documenting
/// hub-wide `config.yaml`. Idempotent: an existing file is never touched.
pub fn ensure_home(out: &mut dyn ScaffoldReporter) -> Result<()> {
    let home = shelbi_state::ensure_root_subdirs().map_err(|e| anyhow!(e))?;
    let default_session = shelbi_state::sessions_dir().map_err(|e| anyhow!(e))?.join("default.yaml");
    if !default_session.exists() {
        std::fs::write(&default_session, "name: default\nprojects: []\nstartup: []\n")
            .with_context(|| format!("writing {}", default_session.display()))?;
    }
    if shelbi_state::scaffold_user_config_if_missing().map_err(|e| anyhow!(e))? {
        let path = shelbi_state::user_config_path().map_err(|e| anyhow!(e))?;
        out.note(&format!("✓ wrote global config: {}", path.display()));
    }
    out.note(&format!("✓ scaffolded {}", home.display()));
    Ok(())
}

/// Initialize a Git repository at `root` on `branch`, unless it already is
/// one.
fn git_init(root: &Path, branch: &str, out: &mut dyn ScaffoldReporter) -> Result<()> {
    if validate_root(root) != RootValidation::NotGitRepo {
        return Ok(());
    }
    let output = Command::new("git")
        .args(["init", "-b", branch])
        .current_dir(root)
        .output()
        .with_context(|| format!("running git init in {}", root.display()))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("git init failed in {}: {detail}", root.display());
    }
    out.note(&format!("✓ initialized a Git repository in {} on {branch}", root.display()));
    Ok(())
}

/// Run `work` as `stage`: report it started, then finished or failed.
fn run_stage<T>(
    out: &mut dyn ScaffoldReporter,
    stage: ScaffoldStage,
    work: impl FnOnce(&mut dyn ScaffoldReporter) -> Result<T>,
) -> Result<T> {
    out.stage_started(stage);
    match work(out) {
        Ok(value) => {
            out.stage_finished(stage);
            Ok(value)
        }
        Err(error) => {
            out.stage_failed(stage, &error.to_string());
            Err(error)
        }
    }
}

/// [`ScaffoldStage::Config`]: initialize Git (when approved) and lay out the
/// home, take the scaffold lock (returned, for the caller to hold through the
/// remaining stages), seed onboarding, publish the registration, and write
/// the committed in-repo half and the workspace-settings template.
fn scaffold_config(
    resolved: &ResolvedProjectRoot,
    plan: &NewProject,
    out: &mut dyn ScaffoldReporter,
) -> Result<shelbi_state::ProjectScaffoldLock> {
    if plan.git_init {
        git_init(&resolved.path, plan.default_branch.trim(), out)?;
    }
    ensure_home(out)?;
    let mode = plan.mode;
    let github_url = plan.remote.as_deref().and_then(remote_url_for_storage);
    let starter = Starter {
        name: &resolved.name,
        display_name: resolved.display_name.as_deref(),
        work_dir: &resolved.path,
        default_branch: plan.default_branch.trim(),
        github_url: github_url.as_deref(),
        issue_tracker: &plan.issue_tracker,
        defaults: &plan.defaults,
    };
    let projects_dir = shelbi_state::projects_dir().map_err(|e| anyhow!(e))?;
    let scaffold_lock = shelbi_state::lock_project_scaffold().map_err(|e| anyhow!(e))?;
    let yaml_path = projects_dir.join(format!("{}.yaml", resolved.name));
    let registration_exists =
        shelbi_state::has_project_registration(&resolved.name).map_err(|e| anyhow!(e))?;
    let is_first_registration =
        !shelbi_state::has_any_project_registration().map_err(|e| anyhow!(e))?;

    // Seed runtime onboarding before publishing the registration. If this
    // process stops between these steps, the project is still undiscoverable
    // and a retry idempotently reuses the same fixed Welcome task. A
    // registration that already existed at entry is user-owned and must never
    // gain or resurrect onboarding state.
    if !registration_exists {
        let _ = shelbi_state::scaffold_welcome_task(&resolved.name).map_err(|e| anyhow!(e))?;
        if is_first_registration {
            shelbi_state::arm_first_run_hint().map_err(|e| anyhow!(e))?;
        }
    }

    let in_repo_split = if mode == ConfigMode::InRepo {
        Some(render_split_project_yaml(&starter)?)
    } else {
        None
    };

    if registration_exists {
        let existing_path = if yaml_path.is_file() {
            yaml_path.clone()
        } else {
            projects_dir.join(&resolved.name).join("local.yaml")
        };
        out.note(&format!(
            "(project registration already exists at {})",
            existing_path.display()
        ));
    } else {
        match mode {
            ConfigMode::InRepo => {
                let (_shared, local) =
                    in_repo_split.as_ref().expect("in-repo split rendered above");
                let local_path = projects_dir.join(&resolved.name).join("local.yaml");
                if write_new_project_registration(&resolved.name, &local_path, local)? {
                    out.note(&format!(
                        "✓ registered project (in-repo): {}",
                        local_path.display()
                    ));
                } else {
                    out.note(&format!(
                        "(project registration already exists at {})",
                        local_path.display()
                    ));
                }
            }
            ConfigMode::Global => {
                let yaml = render_project_yaml(&starter, None)?;
                if write_new_project_registration(&resolved.name, &yaml_path, &yaml)? {
                    out.note(&format!("✓ wrote project: {}", yaml_path.display()));
                } else {
                    out.note(&format!("(project YAML already exists at {})", yaml_path.display()));
                }
            }
        }
    }

    if let Some((shared, _local)) = in_repo_split.as_ref() {
        write_in_repo_shared(&resolved.path, shared, out)?;
    }

    write_workspace_settings_template(&resolved.name, out)?;
    Ok(scaffold_lock)
}

/// [`ScaffoldStage::Workflows`]: the workflows and statuses, then the rest of
/// the project-wide scaffold (Zen policy, PR template, changelog template,
/// shared preamble).
fn scaffold_workflows(project: &str, out: &mut dyn ScaffoldReporter) -> Result<()> {
    for path in shelbi_state::scaffold_project_workflow(project).map_err(|e| anyhow!(e))? {
        out.note(&format!("✓ wrote project workflow: {}", path.display()));
    }
    let statuses_path = shelbi_state::statuses_path(project).map_err(|e| anyhow!(e))?;
    if !statuses_path.exists() {
        shelbi_state::scaffold_project_statuses(project).map_err(|e| anyhow!(e))?;
        out.note(&format!("✓ wrote project statuses: {}", statuses_path.display()));
    }
    match shelbi_state::scaffold_zenmode(project).map_err(|e| anyhow!(e))? {
        shelbi_state::ZenmodeOutcome::Created => {
            let path = shelbi_state::zenmode_path(project).map_err(|e| anyhow!(e))?;
            out.note(&format!("✓ wrote Zen policy: {}", path.display()));
        }
        shelbi_state::ZenmodeOutcome::Migrated => {
            let path = shelbi_state::zenmode_path(project).map_err(|e| anyhow!(e))?;
            out.note(&format!(
                "✓ pinned legacy Zen PR commands in {} (custom prose preserved)",
                path.display()
            ));
        }
        shelbi_state::ZenmodeOutcome::Unchanged => {}
    }

    if let shelbi_state::PrTemplateOutcome::Created =
        shelbi_state::scaffold_pr_template(project).map_err(|e| anyhow!(e))?
    {
        let path = shelbi_state::pr_template_path(project).map_err(|e| anyhow!(e))?;
        out.note(&format!("✓ wrote PR template: {}", path.display()));
    }
    if let shelbi_state::ChangelogTemplateOutcome::Created =
        shelbi_state::scaffold_changelog_template(project).map_err(|e| anyhow!(e))?
    {
        let path = shelbi_state::changelog_template_path(project).map_err(|e| anyhow!(e))?;
        out.note(&format!("✓ wrote changelog template: {}", path.display()));
    }
    if let shelbi_state::SharedPreambleOutcome::Created =
        shelbi_state::scaffold_shared_preamble(project).map_err(|e| anyhow!(e))?
    {
        let path =
            shelbi_state::agent_shared_preamble_path(project).map_err(|e| anyhow!(e))?;
        out.note(&format!("✓ wrote shared agent preamble (hub safety): {}", path.display()));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Any test here that sets/removes SHELBI_HOME takes the *crate-wide*
    // `test_lock::acquire()` and holds it until the env is restored. A
    // module-local mutex only serialized these tests against each other; it
    // left them racing the other SHELBI_HOME-mutating tests in the crate
    // (`poller::tests`, `reload_target_tmux_tests`), which leaked a foreign
    // home in and produced spurious cross-module failures.
    fn lock_env() -> std::sync::MutexGuard<'static, ()> {
        crate::test_lock::acquire()
    }

    fn fresh_home() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "shelbi-project-create-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(p.join("projects")).unwrap();
        p
    }

    /// The answers a front end would hand the engine for `name` in `root`
    /// (resolved against `cwd`), with the stock defaults.
    fn plan(name: &str, root: &str, mode: ConfigMode, defaults: &AgentDefaults, cwd: &Path) -> NewProject {
        NewProject {
            root: absolutize(cwd, Path::new(root.trim())),
            name: name.to_string(),
            pick_up: false,
            git_init: false,
            defaults: defaults.clone(),
            issue_tracker: IssueTrackerConfig::default(),
            mode,
            default_branch: "main".into(),
            remote: None,
        }
    }

    /// [`create_project`] for the plan [`plan`] builds.
    fn add_project(
        name: &str,
        root: &str,
        mode: ConfigMode,
        defaults: &AgentDefaults,
        cwd: &Path,
        attempt: Attempt,
        out: &mut dyn ScaffoldReporter,
    ) -> std::result::Result<String, AddProjectError> {
        if root.trim().is_empty() {
            return Err(AddProjectError::Invalid("Enter the project's repo path.".into()));
        }
        create_project(&plan(name, root, mode, defaults, cwd), attempt, out)
    }

    /// Scaffold the already-validated `resolved` with the stock defaults.
    fn scaffold(resolved: &ResolvedProjectRoot, mode: ConfigMode, tracker: &IssueTrackerConfig, out: &mut dyn ScaffoldReporter) -> Result<()> {
        let mut p = plan(&resolved.name, &resolved.path.to_string_lossy(), mode, &AgentDefaults::default(), &resolved.path);
        p.issue_tracker = tracker.clone();
        scaffold_project(resolved, &p, out)
    }

    #[test]
    fn validate_root_reports_each_state() {
        let tmp = fresh_home();
        assert_eq!(validate_root(&tmp.join("nope")), RootValidation::NotExists);
        let file = tmp.join("f.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(validate_root(&file), RootValidation::NotDirectory);
        let dir = tmp.join("plain");
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(validate_root(&dir), RootValidation::NotGitRepo);
        let repo = tmp.join("repo");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(validate_root(&repo), RootValidation::Ok);
        // A worktree's `.git` is a file pointing at the real gitdir.
        let wt = tmp.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /tmp/elsewhere\n").unwrap();
        assert_eq!(validate_root(&wt), RootValidation::Ok);
    }

    #[test]
    fn absolutize_resolves_relative_and_passes_absolute() {
        let cwd = PathBuf::from("/tmp/cwd");
        assert_eq!(absolutize(&cwd, Path::new("sub")), PathBuf::from("/tmp/cwd/sub"));
        assert_eq!(absolutize(&cwd, Path::new("/abs")), PathBuf::from("/abs"));
    }

    fn starter<'a>(name: &'a str, work_dir: &'a Path, tracker: &'a IssueTrackerConfig, defaults: &'a AgentDefaults) -> Starter<'a> {
        Starter {
            name,
            display_name: None,
            work_dir,
            default_branch: "main",
            github_url: None,
            issue_tracker: tracker,
            defaults,
        }
    }

    #[test]
    fn render_project_yaml_round_trips_and_elides_defaults() {
        let tracker = IssueTrackerConfig::default();
        let defaults = AgentDefaults::default();
        let yaml = render_project_yaml(&starter("my-app", Path::new("/tmp/my-app"), &tracker, &defaults), None).unwrap();
        let project: shelbi_core::Project = yaml_serde::from_str(&yaml).unwrap();
        // A file_system board is the default → no `issue_tracker:` block.
        assert!(!yaml.contains("issue_tracker"));
        assert_eq!(project.label, None);
        assert_eq!(project.default_branch, "main");
        assert_eq!(project.repo, "/tmp/my-app");
        assert_eq!(project.github_url, None);
        assert!(project.workspaces.is_empty(), "the orchestrator provisions the pool on first boot");
        assert_eq!(project.machines.len(), 1);
        assert_eq!(project.machines[0].work_dir, PathBuf::from("/tmp/my-app"));
        // A hostile name can never reach disk.
        let tmp = Path::new("/tmp");
        assert!(render_project_yaml(&starter("../../evil", tmp, &tracker, &defaults), None).is_err());
        assert!(render_project_yaml(&starter("has\nnewline", tmp, &tracker, &defaults), None).is_err());
    }

    #[test]
    fn render_project_yaml_records_the_detected_branch_remote_and_runner() {
        let tracker = IssueTrackerConfig::default();
        let defaults = AgentDefaults { runner: RunnerKind::Codex, model: Some("gpt-5".into()) };
        let yaml = render_project_yaml(
            &Starter {
                default_branch: "trunk",
                github_url: Some("git@github.com:acme/app.git"),
                ..starter("app", Path::new("/code/app"), &tracker, &defaults)
            },
            None,
        )
        .unwrap();
        let project: shelbi_core::Project = yaml_serde::from_str(&yaml).unwrap();
        assert_eq!(project.default_branch, "trunk");
        assert_eq!(project.github_url.as_deref(), Some("git@github.com:acme/app.git"));
        assert_eq!(project.orchestrator.runner, "codex");
        assert!(project.workspaces.is_empty());
        assert!(!yaml.contains("runner: claude"), "nothing is pinned to Claude:\n{yaml}");
        // The commented github_url example stays out once the key is set.
        assert_eq!(yaml.matches("github_url:").count(), 1, "{yaml}");
    }

    #[test]
    fn validate_add_project_rejects_empty_and_collisions_and_bad_root() {
        let _g = lock_env();
        let home = fresh_home();
        std::fs::write(home.join("projects/taken.yaml"), "name: taken\n").unwrap();
        std::env::set_var("SHELBI_HOME", &home);

        // `/tmp` exists (non-git allowed) so only the name path is exercised.
        let cwd = Path::new("/tmp");
        assert_eq!(
            validate_add_project("", "/tmp", cwd).unwrap_err(),
            "Enter a project name."
        );
        assert!(validate_add_project("!!!", "/tmp", cwd)
            .unwrap_err()
            .contains("my-app"));
        assert!(validate_add_project("Taken", "/tmp", cwd)
            .unwrap_err()
            .contains("taken"));
        assert!(validate_add_project("fresh", "   ", cwd)
            .unwrap_err()
            .contains("repo path"));
        assert!(validate_add_project("fresh", "/definitely/not/here/xyzzy", cwd)
            .unwrap_err()
            .contains("does not exist"));

        // A mixed-case name is accepted: slugified id + raw display label.
        let ok = validate_add_project("My App", "/tmp", cwd).unwrap();
        assert_eq!(ok.name, "my-app");
        assert_eq!(ok.display_name.as_deref(), Some("My App"));
        assert_eq!(ok.path, PathBuf::from("/tmp"));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn add_project_runs_the_forms_mutation_and_reports_bad_input_as_invalid() {
        use shelbi_app::overlay::add_project::Detected;
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let repo = home.join("web");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        // The form's "Detected:" line, for a relative path against `cwd`.
        assert_eq!(detect_root(&home, "web"), Detected::GitRepo);
        assert_eq!(detect_root(&home, "projects"), Detected::NotGitRepo);
        assert_eq!(detect_root(&home, "nope"), Detected::NotFound);
        assert_eq!(detect_root(&home, "  "), Detected::NotFound);

        // Bad input stays on the form; nothing is written.
        assert_eq!(
            add_project("", "web", ConfigMode::Global, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter),
            Err(AddProjectError::Invalid("Enter a project name.".into()))
        );
        assert!(!home.join("projects/web.yaml").exists());

        // Good input scaffolds and names the new project.
        assert_eq!(
            add_project("Web", "web", ConfigMode::Global, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter),
            Ok("web".to_string())
        );
        assert!(home.join("projects/web.yaml").exists());
        // The second time the name is taken.
        assert!(matches!(
            add_project("web", "web", ConfigMode::Global, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter),
            Err(AddProjectError::Invalid(msg)) if msg.contains("already")
        ));

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn check_folder_reports_the_folder_its_name_and_clashes() {
        use shelbi_app::add_project_wizard::{Problem, Repo};
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let projects = home.join("projects");

        // A repo whose path has spaces; its name keeps its case, its id
        // slugifies, and outside-the-project settings live under the home.
        let repo = home.join("My Code/Web App");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let c = check_folder(&home, "My Code/Web App", None);
        assert_eq!(c.path, repo);
        assert_eq!(c.repo, Some(Repo::Git));
        assert_eq!(c.name.as_deref(), Some("Web App"));
        assert_eq!(c.slug.as_deref(), Some("web-app"));
        assert_eq!(c.problem, None);
        assert_eq!(c.in_repo_dir, repo.join(".shelbi"));
        assert_eq!(c.global_dir, Some(projects.join("web-app")));

        // A worktree, a plain folder (allowed), a file, a missing path.
        let wt = home.join("wt/alpha");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /tmp/elsewhere\n").unwrap();
        assert_eq!(check_folder(&home, "wt/alpha", None).repo, Some(Repo::Worktree));
        let plain = check_folder(&home, "wt", None);
        assert_eq!((plain.repo, plain.problem), (Some(Repo::NotGit), None));
        std::fs::write(home.join("file.txt"), "x").unwrap();
        assert_eq!(check_folder(&home, "file.txt", None).problem, Some(Problem::NotDirectory));
        assert_eq!(check_folder(&home, "nope", None).problem, Some(Problem::NotFound));
        assert_eq!(check_folder(&home, "  ", None).problem, Some(Problem::Empty));

        // A registered id clashes, whatever the case or spacing of the name
        // that slugifies to it; a typed name gets past it.
        std::fs::write(projects.join("web-app.yaml"), "name: web-app\n").unwrap();
        assert_eq!(
            check_folder(&home, "My Code/Web App", None).problem,
            Some(Problem::Taken { slug: "web-app".into() })
        );
        assert_eq!(
            check_folder(&home, "My Code/Web App", Some("WEB app")).problem,
            Some(Problem::Taken { slug: "web-app".into() })
        );
        let renamed = check_folder(&home, "My Code/Web App", Some("Web App 2"));
        assert_eq!((renamed.slug.as_deref(), renamed.problem), (Some("web-app-2"), None));
        assert_eq!(check_folder(&home, "My Code/Web App", Some("!!")).problem, Some(Problem::NoName));

        // Checking wrote nothing.
        assert!(!repo.join(".shelbi").exists());
        assert!(!projects.join("web-app-2.yaml").exists());

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn the_wizards_request_creates_an_in_repo_project_in_a_worktree() {
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let wt = home.join("src/alpha wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), "gitdir: /tmp/elsewhere\n").unwrap();

        let c = check_folder(&home, &wt.display().to_string(), None);
        let (name, root) = (c.name.unwrap(), c.path.display().to_string());
        assert_eq!(add_project(&name, &root, ConfigMode::InRepo, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter), Ok("alpha-wt".to_string()));
        assert!(wt.join(IN_REPO_CONFIG_REL).is_file());
        assert!(shelbi_state::has_project_registration("alpha-wt").unwrap());

        std::env::remove_var("SHELBI_HOME");
    }

    /// Every file a created project has, by path relative to `home` (and
    /// `repo`, under `repo/`), with its text.
    fn project_files(home: &Path, repo: &Path) -> std::collections::BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, tag: &str, out: &mut std::collections::BTreeMap<String, String>) {
            for entry in std::fs::read_dir(dir).unwrap().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, tag, out);
                } else if let Ok(text) = std::fs::read_to_string(&path) {
                    let rel = path.strip_prefix(root).unwrap().display().to_string();
                    out.insert(format!("{tag}{rel}"), text);
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(home, &home.join("projects"), "", &mut out);
        if repo.join(".shelbi").is_dir() {
            walk(repo, &repo.join(".shelbi"), "repo/", &mut out);
        }
        out
    }

    /// `files` with `home` (as given and as macOS resolves it) written `<home>`.
    fn normalized(
        files: std::collections::BTreeMap<String, String>,
        home: &Path,
    ) -> std::collections::BTreeMap<String, String> {
        let real = home.canonicalize().unwrap().display().to_string();
        let given = home.display().to_string();
        files.into_iter().map(|(k, v)| (k, v.replace(&real, "<home>").replace(&given, "<home>"))).collect()
    }

    /// Create `acme` in a fresh home with `defaults` (in `mode`): the home,
    /// the repo, and the loaded project.
    fn create_with(mode: ConfigMode, defaults: &AgentDefaults) -> (PathBuf, PathBuf, shelbi_core::Project) {
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        let repo = home.join("acme");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let slug = add_project("acme", "acme", mode, defaults, &home, Attempt::New, &mut NullReporter).unwrap();
        let project = shelbi_state::load_project(&slug).unwrap();
        (home, repo, project)
    }

    fn manifest(home: &Path, agent: &str) -> String {
        std::fs::read_to_string(home.join(format!("projects/acme/agents/{agent}/agent.yaml"))).unwrap()
    }

    #[test]
    fn claude_on_its_default_model_creates_exactly_todays_project() {
        let _g = lock_env();
        for mode in [ConfigMode::Global, ConfigMode::InRepo] {
            // Today's path: the scaffold `shelbi init` runs, no defaults.
            let home = fresh_home();
            std::env::set_var("SHELBI_HOME", &home);
            let repo = home.join("acme");
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            let resolved = validate_add_project("acme", "acme", &home).unwrap();
            scaffold(&resolved, mode, &IssueTrackerConfig::default(), &mut NullReporter).unwrap();
            let today = normalized(project_files(&home, &repo), &home);

            let (home2, repo2, project) = create_with(mode, &AgentDefaults::default());
            let chosen = normalized(project_files(&home2, &repo2), &home2);
            // `state.json` and the Welcome task carry the time they were
            // written; everything else is byte-identical.
            let strip = |m: &std::collections::BTreeMap<String, String>| {
                m.iter()
                    .filter(|(k, _)| !k.ends_with("state.json"))
                    .map(|(k, v)| {
                        let v: Vec<&str> = v.lines().filter(|l| !l.starts_with("created_at: ") && !l.starts_with("updated_at: ")).collect();
                        (k.clone(), v.join("\n"))
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(strip(&chosen), strip(&today), "{mode:?}");
            assert_eq!(project.orchestrator.runner, "claude");
            assert!(project.runners.is_empty());
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn the_default_runner_becomes_the_orchestrators_runner() {
        let _g = lock_env();
        for mode in [ConfigMode::Global, ConfigMode::InRepo] {
            let codex = AgentDefaults { runner: RunnerKind::Codex, model: None };
            let (_home, _repo, project) = create_with(mode, &codex);
            assert_eq!(project.orchestrator.runner, "codex", "{mode:?}");
            assert_eq!(project.default_runner_spec().map(|r| r.command.as_str()), Some("codex"));
            assert!(project.runners.is_empty(), "the runner's default model writes no fleet entry");
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_chosen_model_is_the_fleets_house_model() {
        let _g = lock_env();
        for mode in [ConfigMode::Global, ConfigMode::InRepo] {
            let chosen = AgentDefaults { runner: RunnerKind::Codex, model: Some("gpt-5-codex".into()) };
            let (home, repo, project) = create_with(mode, &chosen);
            let fleet = project.runners.get(&RunnerKind::Codex).expect("runners.codex");
            assert_eq!(fleet.model.as_deref(), Some("gpt-5-codex"), "{mode:?}");
            assert_eq!(fleet.spec.command, "codex");
            assert_eq!(project.runners.len(), 1);
            // The orchestrator launches on it, and the agents config reads it.
            assert_eq!(shelbi_core::orchestrator_model(&project, None).as_deref(), Some("gpt-5-codex"));
            let config = shelbi_state::agents_config::AgentsConfig::load("acme").unwrap();
            assert_eq!(config.project_model(RunnerKind::Codex), Some("gpt-5-codex"));
            // In-repo, it's committed with the rest of the shared settings.
            if mode == ConfigMode::InRepo {
                let shared = std::fs::read_to_string(repo.join(IN_REPO_CONFIG_REL)).unwrap();
                assert!(shared.contains("gpt-5-codex"), "{shared}");
            } else {
                let yaml = std::fs::read_to_string(home.join("projects/acme.yaml")).unwrap();
                assert!(yaml.contains("runners:\n  codex:\n    command: codex\n    model: gpt-5-codex\n"), "{yaml}");
            }

            // Claude with a model: the same, on its own kind.
            let claude = AgentDefaults { runner: RunnerKind::Claude, model: Some("opus".into()) };
            let (_h, _r, project) = create_with(mode, &claude);
            assert_eq!(project.orchestrator.runner, "claude");
            assert_eq!(project.runners.get(&RunnerKind::Claude).and_then(|r| r.model.as_deref()), Some("opus"));
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_non_claude_default_is_every_shipped_agents_recommendation() {
        let _g = lock_env();
        let codex = AgentDefaults { runner: RunnerKind::Codex, model: None };
        let (home, _repo, project) = create_with(ConfigMode::Global, &codex);
        for agent in shelbi_state::DEFAULT_AGENTS {
            let text = manifest(&home, agent.name);
            assert!(text.contains("\npreferred_runner: codex\n"), "{}: {text}", agent.name);
            assert!(!text.contains("preferred_runner: claude"), "{}", agent.name);
            // So an agent the project doesn't pin launches on it.
            let parsed = shelbi_core::AgentManifest::from_yaml_str(&text).unwrap();
            let launch = shelbi_core::resolve_agent_launch(&project, agent.name, Some(&parsed), None);
            assert_eq!(launch.kind, RunnerKind::Codex, "{}", agent.name);
        }

        // An `agent.yaml` that isn't the shipped text is never rewritten.
        let path = home.join("projects/acme/agents/developer/agent.yaml");
        std::fs::write(&path, "name: developer\npreferred_runner: claude\n").unwrap();
        assert_eq!(shelbi_state::recommend_runner_for_new_agents("acme", RunnerKind::Codex).unwrap(), Vec::<String>::new());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "name: developer\npreferred_runner: claude\n");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn the_orchestrator_onboarding_starts_on_the_chosen_defaults() {
        let _g = lock_env();
        let chosen = AgentDefaults { runner: RunnerKind::Codex, model: Some("gpt-5-codex".into()) };
        let (_home, _repo, _project) = create_with(ConfigMode::Global, &chosen);
        let mut onboarding = shelbi_app::onboarding::OnboardingState::new("acme");
        onboarding.loaded(shelbi_app::onboarding::load("acme"));
        assert_eq!(onboarding.runner(), RunnerKind::Codex);
        assert_eq!(onboarding.model_choice(), Some("gpt-5-codex"));
        assert_eq!(onboarding.model().model_note, None, "the choice is the project's own");
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_generic_default_is_refused_before_anything_is_written() {
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        std::fs::create_dir_all(home.join("acme/.git")).unwrap();
        let generic = AgentDefaults { runner: RunnerKind::Generic, model: None };
        assert!(matches!(
            add_project("acme", "acme", ConfigMode::Global, &generic, &home, Attempt::New, &mut NullReporter),
            Err(AddProjectError::Invalid(msg)) if msg.contains("generic")
        ));
        assert!(!home.join("projects/acme.yaml").exists());
        assert!(!home.join("projects/acme").exists());
        std::env::remove_var("SHELBI_HOME");
    }

    /// Byte-identical `shelbi init` output guard: a fresh global project emits
    /// exactly these progress lines (in this order) from the scaffold engine,
    /// which the CLI prints verbatim. If the wording or order changes here,
    /// `shelbi init`'s terminal output changed too — update both deliberately.
    #[test]
    fn scaffold_project_emits_the_expected_init_output_for_a_fresh_global_project() {
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let repo = home.join("acme");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let resolved = ResolvedProjectRoot {
            path: repo.clone(),
            name: "acme".to_string(),
            display_name: None,
        };

        let mut out = CollectReporter::default();
        scaffold(&resolved, ConfigMode::Global, &IssueTrackerConfig::default(), &mut out)
            .unwrap();

        let yaml = home.join("projects/acme.yaml");
        let workflow = shelbi_state::workflow_path("acme", shelbi_core::TASK_WORKFLOW_NAME).unwrap();
        let subtask =
            shelbi_state::workflow_path("acme", shelbi_core::SUBTASK_WORKFLOW_NAME).unwrap();
        let statuses = shelbi_state::statuses_path("acme").unwrap();
        let settings = shelbi_state::config_project_dir("acme")
            .unwrap()
            .join("workspace-settings.json.template");
        let zen = shelbi_state::zenmode_path("acme").unwrap();
        let pr = shelbi_state::pr_template_path("acme").unwrap();

        // The project YAML line, the workspace-settings line, the agent-create
        // lines (one per shipped wired agent, order from self_heal), then the
        // workflow/statuses/Zen/PR-template lines.
        // The home's layout first (a fresh home gets the hub-wide config),
        // then the project YAML line, the workspace-settings line, …
        let config = shelbi_state::user_config_path().unwrap();
        assert_eq!(
            out.notes[..2],
            [format!("✓ wrote global config: {}", config.display()), format!("✓ scaffolded {}", home.display())]
        );
        let notes = &out.notes[2..];
        assert_eq!(notes[0], format!("✓ wrote project: {}", yaml.display()));
        assert_eq!(
            notes[1],
            format!("✓ wrote workspace settings template: {}", settings.display())
        );
        // Agent-workspace lines: every shipped wired agent is Created on a
        // fresh project, each rendered as `✓ created agent workspace: …`.
        let created: Vec<&String> = out
            .notes
            .iter()
            .filter(|l| l.starts_with("✓ created agent workspace: agents/"))
            .collect();
        assert!(
            !created.is_empty(),
            "expected at least one created-agent line, got: {:?}",
            out.notes
        );
        // The tail lines, in order, after the agent block.
        let tail: Vec<&String> = out
            .notes
            .iter()
            .filter(|l| !l.starts_with("✓ created agent workspace: agents/"))
            .collect();
        assert!(tail.contains(&&format!("✓ wrote project workflow: {}", workflow.display())));
        assert!(tail.contains(&&format!("✓ wrote project workflow: {}", subtask.display())));
        assert!(tail.contains(&&format!("✓ wrote project statuses: {}", statuses.display())));
        assert!(tail.contains(&&format!("✓ wrote Zen policy: {}", zen.display())));
        assert!(tail.contains(&&format!("✓ wrote PR template: {}", pr.display())));
        let changelog = shelbi_state::changelog_template_path("acme").unwrap();
        assert!(tail.contains(&&format!("✓ wrote changelog template: {}", changelog.display())));
        assert_eq!(
            std::fs::read_to_string(&changelog).unwrap(),
            shelbi_state::DEFAULT_CHANGELOG_TEMPLATE,
            "a new project ships the changelog template"
        );
        let preamble = shelbi_state::agent_shared_preamble_path("acme").unwrap();
        assert!(tail.contains(&&format!(
            "✓ wrote shared agent preamble (hub safety): {}",
            preamble.display()
        )));
        assert_eq!(
            std::fs::read_to_string(&preamble).unwrap(),
            shelbi_state::DEFAULT_SHARED_PREAMBLE,
            "a new project ships the hub-safety preamble"
        );

        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn scaffold_project_is_idempotent_on_rerun() {
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);

        let repo = home.join("idem");
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        let resolved = ResolvedProjectRoot {
            path: repo,
            name: "idem".to_string(),
            display_name: None,
        };

        scaffold(&resolved, ConfigMode::Global, &IssueTrackerConfig::default(), &mut NullReporter)
            .unwrap();
        // A second run must not error and must report the registration as
        // already existing rather than double-writing it.
        let mut out = CollectReporter::default();
        scaffold(&resolved, ConfigMode::Global, &IssueTrackerConfig::default(), &mut out)
            .unwrap();
        assert!(
            out.notes.iter().any(|l| l.contains("already exists")),
            "re-run should report an existing registration, got: {:?}",
            out.notes
        );

        std::env::remove_var("SHELBI_HOME");
    }

    /// The stage events of a run that got all the way through.
    fn all_stages() -> Vec<StageEvent> {
        ScaffoldStage::ALL
            .iter()
            .flat_map(|s| [StageEvent::Started(*s), StageEvent::Finished(*s)])
            .collect()
    }

    /// Where `slug`'s registration lives, asserting there is exactly one.
    fn only_registration(home: &Path, slug: &str) -> PathBuf {
        let flat = home.join(format!("projects/{slug}.yaml"));
        let split = home.join(format!("projects/{slug}/local.yaml"));
        match (flat.is_file(), split.is_file()) {
            (true, false) => flat,
            (false, true) => split,
            other => panic!("expected exactly one registration for {slug}, got {other:?}"),
        }
    }

    /// Checks, as each stage reports, that its writes are already on disk
    /// (and the next stage's aren't yet).
    struct DiskChecker {
        slug: String,
        events: Vec<StageEvent>,
    }

    impl ScaffoldReporter for DiskChecker {
        fn note(&mut self, _line: &str) {}
        fn warn(&mut self, _line: &str) {}
        fn stage_started(&mut self, stage: ScaffoldStage) {
            let agents = shelbi_state::agents_dir(&self.slug).unwrap();
            let statuses = shelbi_state::statuses_path(&self.slug).unwrap();
            match stage {
                ScaffoldStage::Config => {
                    assert!(!shelbi_state::has_project_registration(&self.slug).unwrap())
                }
                ScaffoldStage::Agents => assert!(!agents.exists(), "agents before their stage"),
                ScaffoldStage::Workflows => assert!(!statuses.exists(), "statuses before their stage"),
            }
            self.events.push(StageEvent::Started(stage));
        }
        fn stage_finished(&mut self, stage: ScaffoldStage) {
            let slug = self.slug.as_str();
            match stage {
                ScaffoldStage::Config => {
                    assert!(shelbi_state::has_project_registration(slug).unwrap());
                    let settings = shelbi_state::config_project_dir(slug)
                        .unwrap()
                        .join("workspace-settings.json.template");
                    assert!(settings.is_file(), "workspace settings before Config is checked");
                }
                ScaffoldStage::Agents => {
                    let agents = shelbi_state::agents_dir(slug).unwrap();
                    assert!(agents.join("developer").is_dir(), "agents before Agents is checked");
                }
                ScaffoldStage::Workflows => {
                    for wf in [shelbi_core::TASK_WORKFLOW_NAME, shelbi_core::SUBTASK_WORKFLOW_NAME] {
                        assert!(shelbi_state::workflow_path(slug, wf).unwrap().is_file());
                    }
                    assert!(shelbi_state::statuses_path(slug).unwrap().is_file());
                    assert!(shelbi_state::zenmode_path(slug).unwrap().is_file());
                    assert!(shelbi_state::pr_template_path(slug).unwrap().is_file());
                    assert!(shelbi_state::agent_shared_preamble_path(slug).unwrap().is_file());
                }
            }
            self.events.push(StageEvent::Finished(stage));
        }
        fn stage_failed(&mut self, stage: ScaffoldStage, error: &str) {
            self.events.push(StageEvent::Failed(stage, error.to_string()));
        }
    }

    #[test]
    fn each_stage_is_reported_in_order_once_its_writes_are_on_disk() {
        let _g = lock_env();
        for mode in [ConfigMode::Global, ConfigMode::InRepo] {
            let home = fresh_home();
            std::env::set_var("SHELBI_HOME", &home);
            let repo = home.join("acme");
            std::fs::create_dir_all(repo.join(".git")).unwrap();
            let mut out = DiskChecker { slug: "acme".into(), events: Vec::new() };
            let slug = add_project("acme", "acme", mode, &AgentDefaults::default(), &home, Attempt::New, &mut out).unwrap();
            assert_eq!(slug, "acme");
            assert_eq!(out.events, all_stages(), "{mode:?}");
            only_registration(&home, "acme");
        }
        std::env::remove_var("SHELBI_HOME");
    }

    /// What blocks a stage: a plain file where it needs a directory.
    fn obstruct(home: &Path, repo: &Path, mode: ConfigMode, stage: ScaffoldStage) -> PathBuf {
        let config = match mode {
            ConfigMode::Global => home.join("projects/acme"),
            ConfigMode::InRepo => repo.join(".shelbi"),
        };
        let at = match stage {
            // Global: the project's own folder (onboarding is seeded there
            // before the registration). In-repo: the committed config's
            // folder (written after the registration).
            ScaffoldStage::Config => config,
            ScaffoldStage::Agents => config.join("agents"),
            ScaffoldStage::Workflows => config.join("workflows"),
        };
        std::fs::create_dir_all(at.parent().unwrap()).unwrap();
        std::fs::write(&at, "in the way\n").unwrap();
        at
    }

    #[test]
    fn a_failure_at_each_stage_is_reported_and_a_retry_finishes_the_same_project() {
        let _g = lock_env();
        for mode in [ConfigMode::Global, ConfigMode::InRepo] {
            for (i, stage) in ScaffoldStage::ALL.into_iter().enumerate() {
                let home = fresh_home();
                std::env::set_var("SHELBI_HOME", &home);
                let repo = home.join("acme");
                std::fs::create_dir_all(repo.join(".git")).unwrap();
                let blocker = obstruct(&home, &repo, mode, stage);
                let case = format!("{mode:?} {stage:?}");

                let mut out = CollectReporter::default();
                let failed = add_project("acme", "acme", mode, &AgentDefaults::default(), &home, Attempt::New, &mut out);
                assert!(matches!(failed, Err(AddProjectError::Failed(_))), "{case}: {failed:?}");
                if mode == ConfigMode::Global {
                    // Every global destination is checked before the first
                    // write: the creation stops before any stage, nothing
                    // registered, and once it's clear a new attempt works.
                    assert!(out.stages.is_empty(), "{case}: {:?}", out.stages);
                    assert!(!shelbi_state::has_project_registration("acme").unwrap(), "{case}");
                    std::fs::remove_file(&blocker).unwrap();
                    let slug = add_project("acme", "acme", mode, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter);
                    assert_eq!(slug, Ok("acme".into()), "{case}");
                    std::env::remove_var("SHELBI_HOME");
                    continue;
                }
                let mut expected = all_stages()[..i * 2].to_vec();
                expected.push(StageEvent::Started(stage));
                assert_eq!(out.stages[..expected.len()], expected[..], "{case}");
                assert_eq!(out.stages.len(), expected.len() + 1, "{case}: nothing after the failure");
                assert!(
                    matches!(out.stages.last(), Some(StageEvent::Failed(s, _)) if *s == stage),
                    "{case}: {:?}",
                    out.stages
                );

                // Customize a file the earlier stages wrote: the retry keeps it.
                let custom = (i > 0).then(|| {
                    let settings = shelbi_state::config_project_dir("acme")
                        .unwrap()
                        .join("workspace-settings.json.template");
                    std::fs::write(&settings, "{\"custom\": true}\n").unwrap();
                    settings
                });
                let instructions = (i > 1).then(|| {
                    let path = shelbi_state::agents_dir("acme")
                        .unwrap()
                        .join("developer/instructions.md");
                    let mine = format!("{}\n## My notes\n", std::fs::read_to_string(&path).unwrap());
                    std::fs::write(&path, &mine).unwrap();
                    (path, mine)
                });

                std::fs::remove_file(&blocker).unwrap();
                let registered = shelbi_state::has_project_registration("acme").unwrap();
                if registered {
                    // A plain second request clashes with the partial project...
                    assert!(
                        matches!(
                            add_project("acme", "acme", mode, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter),
                            Err(AddProjectError::Invalid(m)) if m.contains("already")
                        ),
                        "{case}"
                    );
                }
                // ...and a retry of this one finishes it.
                let mut out = CollectReporter::default();
                assert_eq!(
                    add_project("acme", "acme", mode, &AgentDefaults::default(), &home, Attempt::Retry, &mut out),
                    Ok("acme".to_string()),
                    "{case}"
                );
                assert_eq!(out.stages, all_stages(), "{case}");
                let registration = only_registration(&home, "acme");
                assert_eq!(
                    registration.ends_with("local.yaml"),
                    mode == ConfigMode::InRepo,
                    "{case}: the registration for its mode"
                );
                if let Some(settings) = custom {
                    assert_eq!(std::fs::read_to_string(settings).unwrap(), "{\"custom\": true}\n", "{case}");
                }
                if let Some((path, mine)) = instructions {
                    assert_eq!(std::fs::read_to_string(path).unwrap(), mine, "{case}");
                }
                assert!(shelbi_state::statuses_path("acme").unwrap().is_file(), "{case}");
            }
        }
        std::env::remove_var("SHELBI_HOME");
    }

    #[test]
    fn a_retry_never_takes_an_unrelated_registration() {
        let _g = lock_env();
        let home = fresh_home();
        std::env::set_var("SHELBI_HOME", &home);
        for dir in ["mine/acme", "theirs/acme"] {
            std::fs::create_dir_all(home.join(dir).join(".git")).unwrap();
        }
        // Someone else's project already has the id.
        add_project("acme", "theirs/acme", ConfigMode::Global, &AgentDefaults::default(), &home, Attempt::New, &mut NullReporter)
            .unwrap();
        let theirs = std::fs::read_to_string(home.join("projects/acme.yaml")).unwrap();

        let mut out = CollectReporter::default();
        let other_folder =
            add_project("acme", "mine/acme", ConfigMode::Global, &AgentDefaults::default(), &home, Attempt::Retry, &mut out);
        assert!(matches!(other_folder, Err(AddProjectError::Invalid(m)) if m.contains("already")));
        assert!(out.stages.is_empty(), "nothing ran");
        // The same folder in the other config mode isn't this request either.
        assert!(matches!(
            add_project("acme", "theirs/acme", ConfigMode::InRepo, &AgentDefaults::default(), &home, Attempt::Retry, &mut NullReporter),
            Err(AddProjectError::Invalid(_))
        ));
        assert!(!home.join("theirs/acme/.shelbi/project.yaml").exists());
        assert_eq!(std::fs::read_to_string(home.join("projects/acme.yaml")).unwrap(), theirs);
        only_registration(&home, "acme");

        // A retry of the request that made it is welcome, and changes nothing.
        assert_eq!(
            add_project("acme", "theirs/acme", ConfigMode::Global, &AgentDefaults::default(), &home, Attempt::Retry, &mut NullReporter),
            Ok("acme".to_string())
        );
        assert_eq!(std::fs::read_to_string(home.join("projects/acme.yaml")).unwrap(), theirs);

        std::env::remove_var("SHELBI_HOME");
    }
}
