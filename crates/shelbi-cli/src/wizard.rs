//! First-run project setup.
//!
//! The default path detects a complete setup plan, renders a live preflight
//! followed by one confirmation card, and writes nothing until the user
//! chooses Launch. The longer questionnaire remains available behind the
//! Customize key, with every detected value used as its default.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use clap::ValueEnum;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use inquire::{Confirm, Select, Text};
use shelbi_core::{
    AgentRunnerSpec, Machine, MachineKind, OrchestratorSpec, Project,
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, ValueEnum)]
pub(crate) enum Runner {
    Claude,
    Codex,
}

impl Runner {
    const ALL: [Self; 2] = [Self::Claude, Self::Codex];

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }

    #[allow(dead_code)]
    pub(crate) fn from_id(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            _ => None,
        }
    }
}

impl Display for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.id())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedRunner {
    pub(crate) runner: Runner,
    pub(crate) version: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DetectedSetupPlan {
    pub(crate) project_name: String,
    /// Human-readable label recorded when the entered/detected name was
    /// slugified into a different `project_name` (e.g. `My Demo` →
    /// `my-demo`). `None` when the name was already a clean slug, so the
    /// project YAML stays free of a redundant `display_name`.
    pub(crate) display_name: Option<String>,
    pub(crate) repo_root: PathBuf,
    pub(crate) default_branch: String,
    pub(crate) remote_url: Option<String>,
    pub(crate) selected_runner: Runner,
    pub(crate) orchestrator_runner: Runner,
    pub(crate) detected_runners: Vec<DetectedRunner>,
    pub(crate) tmux_version: String,
    pub(crate) cpu_count: usize,
    /// Repository path approved for deferred `git init`. `None` means the
    /// detected root was already a repository. Keeping the path (rather than
    /// only a bool) lets Customize safely distinguish a newly chosen path.
    pub(crate) git_init_root: Option<PathBuf>,
}

/// Script-provided replacements for the editable fields on the detected setup
/// card. The repository root is intentionally absent: callers choose it before
/// detection so deferred Git initialization remains approved for that exact
/// path rather than accidentally prompting after an override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SetupPlanOverrides {
    pub(crate) project_name: Option<String>,
    pub(crate) default_branch: Option<String>,
    pub(crate) remote_url: Option<String>,
    pub(crate) orchestrator_runner: Option<Runner>,
}

impl DetectedSetupPlan {
    /// Apply explicit scripted values after detection. Every error here is a
    /// prerequisite failure and therefore happens before Git initialization or
    /// project scaffolding begins.
    pub(crate) fn apply_overrides(&mut self, overrides: SetupPlanOverrides) -> Result<()> {
        if let Some(project_name) = overrides.project_name {
            let (slug, display_name) = crate::project_root::slug_and_display(&project_name)?;
            self.project_name = slug;
            self.display_name = display_name;
        }
        if let Some(default_branch) = overrides.default_branch {
            let default_branch = default_branch.trim();
            if default_branch.is_empty() {
                bail!("--default-branch must not be empty");
            }
            shelbi_core::validate_branch(default_branch)
                .map_err(|error| anyhow!("invalid --default-branch {default_branch:?}: {error}"))?;
            self.default_branch = default_branch.to_string();
        }
        if let Some(remote_url) = overrides.remote_url {
            self.remote_url = remote_url_for_storage(&remote_url);
        }
        if let Some(orchestrator_runner) = overrides.orchestrator_runner {
            if !self
                .detected_runners
                .iter()
                .any(|detected| detected.runner == orchestrator_runner)
            {
                bail!(
                    "requested orchestrator runner {} was not found on PATH; installed supported runners: {}",
                    orchestrator_runner,
                    self.detected_runners
                        .iter()
                        .map(|detected| detected.runner.id())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            self.orchestrator_runner = orchestrator_runner;
        }

        // Keep schema validation ahead of every write. The shared commit
        // boundary repeats this check for interactive Customize callers.
        let _ = self.to_project()?;
        Ok(())
    }

    pub(crate) fn to_project(&self) -> Result<Project> {
        let hub = Machine {
            name: "hub".to_string(),
            kind: MachineKind::Local,
            work_dir: self.repo_root.clone(),
            host: None,
            tags: Vec::new(),
            forward: None,
        };
        // A freshly-init'd project ships with an empty workspace pool. The
        // orchestrator provisions the pool on first boot via its questions
        // tool (see the "Bootstrap on session start" first-boot step in the
        // orchestrator instructions), creating each slot with
        // `shelbi workspace add`.
        let workspaces = Vec::new();

        // Selection is intentionally singular, but both built-in runner
        // declarations stay available so installing or switching later is a
        // settings edit rather than a hand-authored YAML repair.
        let agent_runners = Runner::ALL
            .into_iter()
            .map(|runner| {
                (
                    runner.id().to_string(),
                    AgentRunnerSpec {
                        command: runner.id().to_string(),
                        flags: vec![],
                        prompt_injection: None,
                        dialog_signatures: vec![],
                        integration: None,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        let project = Project {
            name: self.project_name.clone(),
            display_name: self.display_name.clone(),
            repo: self.repo_root.display().to_string(),
            default_branch: self.default_branch.clone(),
            default_workflow: Some(shelbi_core::TASK_WORKFLOW_NAME.to_string()),
            config_mode: None,
            machines: vec![hub],
            orchestrator: OrchestratorSpec {
                runner: self.orchestrator_runner.id().to_string(),
            },
            agent_runners,
            editor: None,
            // A Git remote can embed HTTP credentials in its userinfo. The
            // card deliberately redacts those credentials, and the persisted
            // project must not silently put them back on disk.
            github_url: self.remote_url.as_deref().and_then(remote_url_for_storage),
            workspaces,
            workspace_poll_interval_secs: 5,
            workspace_permissions_mode: "auto".into(),
            workspace_settings_template: None,
            zen: shelbi_core::ZenConfig::default(),
            heartbeat: shelbi_core::HeartbeatConfig::default(),
            git: shelbi_core::GitConfig::default(),
            detected_shapes: Vec::new(),
        };
        project
            .validate_workspaces()
            .map_err(|error| anyhow!(error))?;
        Ok(project)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SetupOutcome {
    Created(String),
    Quit,
}

pub(crate) fn setup_one_project() -> Result<SetupOutcome> {
    let root = std::env::current_dir().context("reading current directory")?;
    preserve_in_repo_pickup_contract(&root)?;
    let mut probe = RealSetupProbe;
    let mut ui = RealSetupUi::default();
    setup_one_project_with(&root, &mut probe, &mut ui)
}

fn preserve_in_repo_pickup_contract(root: &Path) -> Result<()> {
    // Preserve the in-repo config pickup contract. A clone that already has
    // committed Shelbi config needs its local.yaml registered by `init
    // --pick-up`; creating a parallel global project here would leave the cwd
    // permanently resolving to the still-unpicked shared config.
    let _ = shelbi_state::resolve_project_for_cwd(root).map_err(|error| anyhow!(error))?;
    Ok(())
}

/// Reusable detection entry point for scripted onboarding. An explicit runner
/// resolves the two-runner case; omitting it is accepted only when exactly one
/// supported runner is installed. This function performs no writes.
#[allow(dead_code)]
pub(crate) fn detect_setup_plan(
    root: &Path,
    explicit_runner: Option<Runner>,
) -> Result<DetectedSetupPlan> {
    preserve_in_repo_pickup_contract(root)?;
    let mut probe = RealSetupProbe;
    let mut sink = SilentPreflight;
    detect_setup_plan_with(root, explicit_runner, &mut probe, &mut sink)
}

/// Commit a fully resolved setup plan without reading the terminal. This uses
/// the same creation function as Enter on the interactive card; the UI shim
/// deliberately errors if a future change accidentally reaches any prompt.
pub(crate) fn accept_setup_plan(plan: DetectedSetupPlan) -> Result<SetupOutcome> {
    let mut probe = RealSetupProbe;
    let mut ui = NonInteractiveSetupUi::default();
    create_project_from_plan(plan, &mut probe, &mut ui)
}

fn detect_setup_plan_with<P, S>(
    root: &Path,
    explicit_runner: Option<Runner>,
    probe: &mut P,
    sink: &mut S,
) -> Result<DetectedSetupPlan>
where
    P: SetupProbe,
    S: PreflightSink,
{
    let git = probe.git_defaults(root);
    if let Some(failure) = &git.probe_failure {
        bail!(failure.clone());
    }
    let initialize_git = !git.inside_git;
    let git = git_defaults_for_plan(root, git);
    let snapshot = detect_snapshot(git, probe, sink)?;
    let selected = resolve_runner(&snapshot.runners, explicit_runner)?;
    Ok(assemble_plan(snapshot, selected, initialize_git))
}

fn setup_one_project_with<P, U>(root: &Path, probe: &mut P, ui: &mut U) -> Result<SetupOutcome>
where
    P: SetupProbe,
    U: SetupUi,
{
    let detected_git = probe.git_defaults(root);
    if let Some(failure) = &detected_git.probe_failure {
        bail!(failure.clone());
    }
    let initialize_git = !detected_git.inside_git;
    if initialize_git && !ui.confirm_git_init(root)? {
        ui.message("No files were written. Run git init -b main and try Shelbi again.")?;
        return Ok(SetupOutcome::Quit);
    }

    let git = git_defaults_for_plan(root, detected_git);
    let snapshot = detect_snapshot(git, probe, ui)?;
    let selected_runner = match snapshot.runners.as_slice() {
        [only] => only.runner,
        many => ui.select_runner(many)?,
    };
    let plan = assemble_plan(snapshot, selected_runner, initialize_git);

    match ui.plan_action(&plan)? {
        PlanAction::Launch => create_project_from_plan(plan, probe, ui),
        PlanAction::Customize => {
            let plan = ui.customize(&plan)?;
            create_project_from_plan(plan, probe, ui)
        }
        PlanAction::Quit => Ok(SetupOutcome::Quit),
    }
}

fn git_defaults_for_plan(root: &Path, detected: GitDefaults) -> GitDefaults {
    if detected.inside_git {
        return detected;
    }
    GitDefaults {
        inside_git: true,
        repo_root: Some(root.to_path_buf()),
        default_branch: Some("main".to_string()),
        remote_url: None,
        probe_failure: None,
    }
}

fn create_project_from_plan<P, U>(
    plan: DetectedSetupPlan,
    probe: &mut P,
    ui: &mut U,
) -> Result<SetupOutcome>
where
    P: SetupProbe,
    U: SetupUi,
{
    // Interactive customization can change every editable field. Validate the
    // complete resolved plan before Git init so bad input cannot leave a
    // repository or partial project behind.
    let _ = plan.to_project()?;
    ensure_project_registration_available(&plan.project_name)?;
    preflight_persistence(&plan)?;
    let current_git = probe.git_defaults(&plan.repo_root);
    if let Some(failure) = &current_git.probe_failure {
        bail!(failure.clone());
    }
    if !current_git.inside_git {
        let already_approved = plan.git_init_root.as_deref() == Some(plan.repo_root.as_path());
        if !already_approved && !ui.confirm_git_init(&plan.repo_root)? {
            ui.message("No files were written. Choose a Git repository and try Shelbi again.")?;
            return Ok(SetupOutcome::Quit);
        }
        initialize_git_if_needed(&plan.repo_root, &plan.default_branch, probe)?;
    }
    persist_plan(&plan)?;
    ui.message(&format!("✓ Project {} created.", plan.project_name))?;

    // Disclose and install the context-scoped default-branch commit guard as
    // part of this consented setup — never silently on a later project open.
    // Best-effort: failure to write a hook must not fail project creation.
    if let Ok(project) = plan.to_project() {
        crate::commands::guard::install_at_init(&project, &plan.repo_root);
    }
    Ok(SetupOutcome::Created(plan.project_name))
}

fn initialize_git_if_needed<P>(repo_root: &Path, default_branch: &str, probe: &mut P) -> Result<()>
where
    P: SetupProbe,
{
    probe.init_git(repo_root, default_branch)?;
    if !probe.git_defaults(repo_root).inside_git {
        bail!(
            "git init reported success, but {} is still not a Git repository",
            repo_root.display()
        );
    }
    Ok(())
}

/// Verify that all state destinations needed by the scaffold are structurally
/// usable and writable before deferred `git init` changes the repository.
/// Write probes are complete sibling temp files removed immediately; no Shelbi
/// directory or discoverable project registration is created here.
fn preflight_persistence(plan: &DetectedSetupPlan) -> Result<()> {
    let home = shelbi_state::shelbi_home().map_err(|error| anyhow!(error))?;
    let projects = shelbi_state::projects_dir().map_err(|error| anyhow!(error))?;
    let sessions = shelbi_state::sessions_dir().map_err(|error| anyhow!(error))?;
    let project_dir =
        shelbi_state::project_dir(&plan.project_name).map_err(|error| anyhow!(error))?;
    let agents = shelbi_state::agents_dir(&plan.project_name).map_err(|error| anyhow!(error))?;
    let workflows =
        shelbi_state::workflows_dir(&plan.project_name).map_err(|error| anyhow!(error))?;
    let tasks = shelbi_state::tasks_dir(&plan.project_name).map_err(|error| anyhow!(error))?;
    let ssh = shelbi_state::ssh_control_dir().map_err(|error| anyhow!(error))?;

    for directory in [
        home.as_path(),
        projects.as_path(),
        sessions.as_path(),
        project_dir.as_path(),
        agents.as_path(),
        workflows.as_path(),
        tasks.as_path(),
    ] {
        ensure_directory_or_missing(directory)?;
        probe_nearest_writable_directory(directory)?;
    }
    for required_directory in shelbi_state::STANDARD_SUBDIRS
        .iter()
        .map(|name| home.join(name))
        .chain(std::iter::once(ssh))
    {
        ensure_directory_or_missing(&required_directory)?;
    }

    let default_session = sessions.join("default.yaml");
    if default_session.exists() {
        let contents = std::fs::read_to_string(&default_session)
            .with_context(|| format!("reading {}", default_session.display()))?;
        serde_yaml::from_str::<shelbi_core::Session>(&contents).with_context(|| {
            format!(
                "{} is not a valid Shelbi session; repair or remove it, then run Shelbi again",
                default_session.display()
            )
        })?;
    }

    let template = project_dir.join("workspace-settings.json.template");
    if template.exists() {
        let _ = std::fs::read_to_string(&template)
            .with_context(|| format!("reading {}", template.display()))?;
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
                ancestor = ancestor.parent().ok_or_else(|| {
                    anyhow!("no existing parent directory for {}", path.display())
                })?;
            }
            Err(error) => {
                return Err(error).with_context(|| format!("inspecting {}", ancestor.display()));
            }
        }
    }

    let target = ancestor.join(".shelbi-init-write-probe");
    let (temp_path, file) = create_sibling_temp(&target).with_context(|| {
        format!(
            "Shelbi state destination {} is not writable",
            path.display()
        )
    })?;
    drop(file);
    std::fs::remove_file(&temp_path)
        .with_context(|| format!("removing state write probe {}", temp_path.display()))?;
    Ok(())
}

fn persist_plan(plan: &DetectedSetupPlan) -> Result<()> {
    let project = plan.to_project()?;
    let registration_yaml =
        serde_yaml::to_string(&project).context("serializing detected project registration")?;

    // Registration is the discoverable commit point for the multi-file
    // scaffold. Serialize every creation path (including `init --pick-up`)
    // from its collision check through publication so a losing attempt can
    // never seed onboarding into somebody else's registration.
    let _scaffold_lock =
        shelbi_state::lock_project_scaffold().map_err(|error| anyhow!(error))?;

    let (flat_registration, split_registration) = registration_paths(&project.name)?;
    if let Some(existing) = [&flat_registration, &split_registration]
        .into_iter()
        .find(|path| path.exists())
    {
        return Err(project_collision_error(&project.name, existing));
    }
    let is_first_registration =
        !shelbi_state::has_any_project_registration().map_err(|error| anyhow!(error))?;

    shelbi_state::ensure_root_subdirs().map_err(|error| anyhow!(error))?;
    let sessions_dir = shelbi_state::sessions_dir().map_err(|error| anyhow!(error))?;
    let default_session = sessions_dir.join("default.yaml");
    ensure_default_session(&default_session)?;
    let _ = shelbi_state::scaffold_user_config_if_missing().map_err(|error| anyhow!(error))?;

    write_workspace_settings_template(&project.name)?;
    let _ =
        shelbi_state::materialize_default_agents(&project.name).map_err(|error| anyhow!(error))?;
    // A retry after an interrupted scaffold may find an agent directory that
    // exists but is incomplete. The normal self-heal pass fills those missing
    // bundled files before the registration becomes discoverable.
    let _ =
        shelbi_state::self_heal_default_agents(&project.name).map_err(|error| anyhow!(error))?;
    let _ =
        shelbi_state::scaffold_project_workflow(&project.name).map_err(|error| anyhow!(error))?;
    let statuses_path =
        shelbi_state::statuses_path(&project.name).map_err(|error| anyhow!(error))?;
    if !statuses_path.exists() {
        shelbi_state::scaffold_project_statuses(&project.name).map_err(|error| anyhow!(error))?;
    }
    let _ = shelbi_state::scaffold_zenmode(&project.name).map_err(|error| anyhow!(error))?;
    let _ = shelbi_state::scaffold_welcome_task(&project.name).map_err(|error| anyhow!(error))?;
    // The registration is the commit point. Until it exists, a retry
    // re-enters onboarding and the scaffold helpers can finish any missing
    // files. Publish a complete temporary file with an atomic no-clobber link
    // so neither an interruption nor a concurrent setup can leave a partial
    // or replaced registration at the discoverable path.
    // Serialize the registration commit with dashboard bootstrap. This keeps
    // a concurrent setup loser from re-arming the greeting after the winner's
    // first dashboard already consumed it, and prevents a dashboard from
    // observing the registration between publication and arming.
    let _dashboard_lock = shelbi_state::lock_dashboard(&project.name)
        .map_err(|error| anyhow!(error))?;
    if let Some(existing) = [&flat_registration, &split_registration]
        .into_iter()
        .find(|path| path.exists())
    {
        return Err(project_collision_error(&project.name, existing));
    }
    // Missing first_run_seen means "already seen" for upgrade safety. Arm
    // the hint explicitly only while publishing this machine's first project,
    // before the registration commit point so a persistence failure cannot
    // leave a discoverable project that setup reported as failed.
    if is_first_registration {
        shelbi_state::arm_first_run_hint().map_err(|error| anyhow!(error))?;
    }
    shelbi_state::arm_contextual_greeting(&project.name).map_err(|error| anyhow!(error))?;
    write_new_project_registration(
        &flat_registration,
        registration_yaml.as_bytes(),
        &project.name,
    )?;
    Ok(())
}

fn registration_paths(project: &str) -> Result<(PathBuf, PathBuf)> {
    shelbi_core::validate_project_name(project).map_err(|error| anyhow!(error))?;
    let projects_dir = shelbi_state::projects_dir().map_err(|error| anyhow!(error))?;
    Ok((
        projects_dir.join(format!("{project}.yaml")),
        projects_dir.join(project).join("local.yaml"),
    ))
}

fn ensure_project_registration_available(project: &str) -> Result<()> {
    let (flat, split) = registration_paths(project)?;
    if let Some(existing) = [&flat, &split].into_iter().find(|path| path.exists()) {
        return Err(project_collision_error(project, existing));
    }
    Ok(())
}

fn project_collision_error(project: &str, existing: &Path) -> anyhow::Error {
    anyhow!(
        "a Shelbi project named {} already exists at {}; no existing state was overwritten. \
         Run Shelbi again, press c on the setup card, and choose a different project name.",
        project,
        existing.display()
    )
}

fn write_new_project_registration(path: &Path, contents: &[u8], project: &str) -> Result<()> {
    if publish_complete_file_if_absent(path, contents)? {
        Ok(())
    } else {
        Err(project_collision_error(project, path))
    }
}

fn ensure_default_session(path: &Path) -> Result<()> {
    const DEFAULT_SESSION: &str = "name: default\nprojects: []\nstartup: []\n";
    let _ = publish_complete_file_if_absent(path, DEFAULT_SESSION.as_bytes())?;
    let contents =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    serde_yaml::from_str::<shelbi_core::Session>(&contents).with_context(|| {
        format!(
            "{} is not a valid Shelbi session; repair or remove it, then run Shelbi again",
            path.display()
        )
    })?;
    Ok(())
}

/// Write a complete sibling temp, then atomically publish it without replacing
/// an existing destination. Returns true when this call created `path`.
fn publish_complete_file_if_absent(path: &Path, contents: &[u8]) -> Result<bool> {
    if path.exists() {
        return Ok(false);
    }
    let (temp_path, mut file) = create_sibling_temp(path)?;
    if let Err(error) = file.write_all(contents) {
        drop(file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("writing {}", temp_path.display()));
    }
    drop(file);
    let publish = std::fs::hard_link(&temp_path, path);
    let _ = std::fs::remove_file(&temp_path);
    match publish {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error).with_context(|| format!("publishing {}", path.display())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDefaults {
    inside_git: bool,
    repo_root: Option<PathBuf>,
    default_branch: Option<String>,
    remote_url: Option<String>,
    probe_failure: Option<String>,
}

impl GitDefaults {
    fn probe(cwd: &Path) -> Self {
        let repo_root = match probe_git_repo_root(cwd) {
            Ok(Some(root)) => root,
            Ok(None) => {
                return Self {
                    inside_git: false,
                    repo_root: None,
                    default_branch: None,
                    remote_url: None,
                    probe_failure: None,
                };
            }
            Err(failure) => {
                return Self {
                    inside_git: false,
                    repo_root: None,
                    default_branch: None,
                    remote_url: None,
                    probe_failure: Some(failure),
                };
            }
        };
        Self {
            inside_git: true,
            repo_root: Some(repo_root),
            default_branch: probe_default_branch(cwd),
            remote_url: git_text(cwd, &["remote", "get-url", "origin"])
                .as_deref()
                .and_then(remote_url_for_storage),
            probe_failure: None,
        }
    }
}

fn probe_git_repo_root(cwd: &Path) -> std::result::Result<Option<PathBuf>, String> {
    let output = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .env("LC_ALL", "C")
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                "Git was not found on PATH. Install Git, then start Shelbi again.".to_string()
            } else {
                format!("could not run Git in {}: {error}", cwd.display())
            }
        })?;
    if output.status.success() {
        let root = git_repo_root_from_stdout(&output.stdout).map_err(|detail| {
            format!(
                "Git reported a repository in {}, but its root path {detail}",
                cwd.display()
            )
        })?;
        return Ok(Some(root));
    }
    let detail = first_output_line(&output).unwrap_or_else(|| output.status.to_string());
    if detail.to_ascii_lowercase().contains("not a git repository") {
        Ok(None)
    } else {
        Err(format!(
            "could not inspect the Git repository at {}: {detail}",
            cwd.display()
        ))
    }
}

fn git_repo_root_from_stdout(stdout: &[u8]) -> std::result::Result<PathBuf, &'static str> {
    let mut bytes = stdout.to_vec();
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
    }
    if bytes.is_empty() {
        return Err("was empty");
    }
    let root = String::from_utf8(bytes).map_err(|_| "was not valid UTF-8")?;
    Ok(PathBuf::from(root))
}

fn probe_default_branch(cwd: &Path) -> Option<String> {
    git_text(
        cwd,
        &[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ],
    )
    .and_then(|reference| {
        let branch = reference.strip_prefix("origin/").unwrap_or(&reference);
        (!branch.is_empty()).then(|| branch.to_string())
    })
    .or_else(|| git_text(cwd, &["symbolic-ref", "--quiet", "--short", "HEAD"]))
}

fn git_text(cwd: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    first_output_line(&output)
}

trait SetupProbe {
    fn git_defaults(&mut self, root: &Path) -> GitDefaults;
    fn init_git(&mut self, root: &Path, default_branch: &str) -> Result<()>;
    fn runner_version(&mut self, runner: Runner) -> Option<String>;
    fn tmux_version(&mut self) -> Option<String>;
    fn cpu_count(&mut self) -> usize;
}

struct RealSetupProbe;

impl SetupProbe for RealSetupProbe {
    fn git_defaults(&mut self, root: &Path) -> GitDefaults {
        GitDefaults::probe(root)
    }

    fn init_git(&mut self, root: &Path, default_branch: &str) -> Result<()> {
        let output = Command::new("git")
            .args(["init", "-b", default_branch])
            .current_dir(root)
            .output()
            .with_context(|| format!("running git init in {}", root.display()))?;
        if output.status.success() {
            return Ok(());
        }
        let detail = first_output_line(&output).unwrap_or_else(|| output.status.to_string());
        bail!("git init failed in {}: {detail}", root.display())
    }

    fn runner_version(&mut self, runner: Runner) -> Option<String> {
        command_version(runner.id(), &["--version"])
            .map(|line| normalize_version(runner.id(), &line))
    }

    fn tmux_version(&mut self) -> Option<String> {
        command_version("tmux", &["-V"]).map(|line| normalize_version("tmux", &line))
    }

    fn cpu_count(&mut self) -> usize {
        std::thread::available_parallelism()
            .map(usize::from)
            .unwrap_or(1)
    }
}

fn command_version(command: &str, args: &[&str]) -> Option<String> {
    command_version_with_deadline(command, args, VERSION_PROBE_TIMEOUT)
}

const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

fn command_version_with_deadline(
    command: &str,
    args: &[&str],
    deadline: Duration,
) -> Option<String> {
    let argv = std::iter::once(command).chain(args.iter().copied());
    let output = shelbi_ssh::run_with_deadline(&shelbi_core::Host::Local, argv, deadline).ok()?;
    if !output.status.success() {
        return None;
    }
    Some(first_output_line(&output).unwrap_or_else(|| "version unknown".to_string()))
}

fn first_output_line(output: &Output) -> Option<String> {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .chain(String::from_utf8_lossy(&output.stderr).lines())
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(sanitize_terminal_text)
}

fn normalize_version(command: &str, line: &str) -> String {
    let sanitized = sanitize_terminal_text(line);
    let trimmed = sanitized.trim();
    let prefixes: &[&str] = match command {
        "claude" => &["claude code ", "claude "],
        "codex" => &["codex-cli ", "codex "],
        "tmux" => &["tmux "],
        _ => &[],
    };
    let lower = trimmed.to_ascii_lowercase();
    for prefix in prefixes {
        if lower.starts_with(prefix) {
            return trimmed[prefix.len()..].trim().to_string();
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Clone)]
struct DetectionSnapshot {
    git: GitDefaults,
    runners: Vec<DetectedRunner>,
    tmux_version: String,
    cpu_count: usize,
}

fn detect_snapshot<P, S>(git: GitDefaults, probe: &mut P, sink: &mut S) -> Result<DetectionSnapshot>
where
    P: SetupProbe,
    S: PreflightSink,
{
    let repo_root = git
        .repo_root
        .as_deref()
        .map(display_path)
        .unwrap_or_else(|| "unknown".to_string());
    sink.emit(PreflightItem::ok("git repo", repo_root))?;

    let default_branch = git
        .default_branch
        .clone()
        .unwrap_or_else(|| "main".to_string());
    sink.emit(PreflightItem::ok("default branch", default_branch.clone()))?;

    let remote_display = git
        .remote_url
        .as_deref()
        .map(display_remote)
        .unwrap_or_else(|| "not configured".to_string());
    sink.emit(PreflightItem::ok("remote", remote_display))?;

    let runners = Runner::ALL
        .into_iter()
        .filter_map(|runner| {
            probe
                .runner_version(runner)
                .map(|version| DetectedRunner { runner, version })
        })
        .collect::<Vec<_>>();
    if runners.is_empty() {
        sink.emit(PreflightItem::failed(
            "agent",
            "claude and codex not found on PATH",
        ))?;
    } else {
        let value = format!(
            "{} on PATH",
            runners
                .iter()
                .map(|runner| format!("{} {}", runner.runner, runner.version))
                .collect::<Vec<_>>()
                .join(" + ")
        );
        sink.emit(PreflightItem::ok("agent", value))?;
    }

    let tmux_version = probe.tmux_version();
    match tmux_version.as_deref() {
        Some(version) => sink.emit(PreflightItem::ok("tmux", version))?,
        None => sink.emit(PreflightItem::failed("tmux", "not found on PATH"))?,
    }

    let cpu_count = probe.cpu_count().max(1);
    sink.emit(PreflightItem::ok(
        "machine",
        format!("{cpu_count} core{}", if cpu_count == 1 { "" } else { "s" }),
    ))?;

    if runners.is_empty() {
        let mut guidance = missing_runner_guidance();
        if tmux_version.is_none() {
            guidance.push_str("\n\n");
            guidance.push_str(&missing_tmux_guidance(current_platform()));
        }
        bail!("{guidance}");
    }
    let Some(tmux_version) = tmux_version else {
        bail!("{}", missing_tmux_guidance(current_platform()));
    };

    Ok(DetectionSnapshot {
        git: GitDefaults {
            default_branch: Some(default_branch),
            ..git
        },
        runners,
        tmux_version,
        cpu_count,
    })
}

fn assemble_plan(
    snapshot: DetectionSnapshot,
    selected_runner: Runner,
    initialize_git: bool,
) -> DetectedSetupPlan {
    let repo_root = snapshot.git.repo_root.unwrap_or_else(|| PathBuf::from("."));
    let git_init_root = initialize_git.then(|| repo_root.clone());
    let (project_name, display_name) = wizard_default_project_name(&repo_root);
    DetectedSetupPlan {
        project_name,
        display_name,
        repo_root,
        default_branch: snapshot
            .git
            .default_branch
            .unwrap_or_else(|| "main".to_string()),
        remote_url: snapshot
            .git
            .remote_url
            .as_deref()
            .and_then(remote_url_for_storage),
        selected_runner,
        orchestrator_runner: selected_runner,
        detected_runners: snapshot.runners,
        tmux_version: snapshot.tmux_version,
        cpu_count: snapshot.cpu_count,
        git_init_root,
    }
}

fn resolve_runner(runners: &[DetectedRunner], explicit_runner: Option<Runner>) -> Result<Runner> {
    if runners.is_empty() {
        bail!("{}", missing_runner_guidance());
    }
    if let Some(explicit) = explicit_runner {
        if runners.iter().any(|runner| runner.runner == explicit) {
            return Ok(explicit);
        }
        bail!(
            "requested runner {} was not found on PATH; installed supported runners: {}",
            explicit,
            runners
                .iter()
                .map(|runner| runner.runner.id())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    match runners {
        [only] => Ok(only.runner),
        _ => {
            bail!("both claude and codex are on PATH; rerun with --runner claude or --runner codex")
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Platform {
    MacOs,
    DebianLike,
    Other,
}

fn current_platform() -> Platform {
    if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "linux") {
        Platform::DebianLike
    } else {
        Platform::Other
    }
}

fn missing_tmux_guidance(platform: Platform) -> String {
    let install = match platform {
        Platform::MacOs => "brew install tmux",
        Platform::DebianLike => {
            "sudo apt install tmux (Debian/Ubuntu) or sudo dnf install tmux (Fedora)"
        }
        Platform::Other => "install tmux 3.2 or later with your package manager",
    };
    format!("tmux was not found on PATH. Run {install}, then start Shelbi again.")
}

fn missing_runner_guidance() -> String {
    concat!(
        "No supported agent runner was found on PATH. Install and authenticate Claude Code ",
        "(https://docs.claude.com/en/docs/claude-code) or Codex ",
        "(npm install -g @openai/codex), then start Shelbi again."
    )
    .to_string()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PreflightItem {
    success: bool,
    label: &'static str,
    value: String,
}

impl PreflightItem {
    fn ok(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            success: true,
            label,
            value: value.into(),
        }
    }

    fn failed(label: &'static str, value: impl Into<String>) -> Self {
        Self {
            success: false,
            label,
            value: value.into(),
        }
    }
}

trait PreflightSink {
    fn emit(&mut self, item: PreflightItem) -> Result<()>;
}

struct SilentPreflight;

impl PreflightSink for SilentPreflight {
    fn emit(&mut self, _item: PreflightItem) -> Result<()> {
        Ok(())
    }
}

trait SetupUi: PreflightSink {
    fn confirm_git_init(&mut self, root: &Path) -> Result<bool>;
    fn select_runner(&mut self, runners: &[DetectedRunner]) -> Result<Runner>;
    fn plan_action(&mut self, plan: &DetectedSetupPlan) -> Result<PlanAction>;
    fn customize(&mut self, plan: &DetectedSetupPlan) -> Result<DetectedSetupPlan>;
    fn message(&mut self, message: &str) -> Result<()>;
}

struct RealSetupUi {
    stdout: io::Stdout,
}

struct NonInteractiveSetupUi {
    stdout: io::Stdout,
}

impl Default for NonInteractiveSetupUi {
    fn default() -> Self {
        Self {
            stdout: io::stdout(),
        }
    }
}

impl PreflightSink for NonInteractiveSetupUi {
    fn emit(&mut self, _item: PreflightItem) -> Result<()> {
        Ok(())
    }
}

impl SetupUi for NonInteractiveSetupUi {
    fn confirm_git_init(&mut self, _root: &Path) -> Result<bool> {
        bail!("non-interactive setup reached an unexpected Git confirmation")
    }

    fn select_runner(&mut self, _runners: &[DetectedRunner]) -> Result<Runner> {
        bail!("non-interactive setup reached an unexpected runner prompt")
    }

    fn plan_action(&mut self, _plan: &DetectedSetupPlan) -> Result<PlanAction> {
        bail!("non-interactive setup reached an unexpected plan-card prompt")
    }

    fn customize(&mut self, _plan: &DetectedSetupPlan) -> Result<DetectedSetupPlan> {
        bail!("non-interactive setup reached an unexpected customization prompt")
    }

    fn message(&mut self, message: &str) -> Result<()> {
        writeln!(self.stdout, "{message}")?;
        self.stdout.flush()?;
        Ok(())
    }
}

impl Default for RealSetupUi {
    fn default() -> Self {
        Self {
            stdout: io::stdout(),
        }
    }
}

impl PreflightSink for RealSetupUi {
    fn emit(&mut self, item: PreflightItem) -> Result<()> {
        render_preflight_item(&mut self.stdout, &item)
    }
}

impl SetupUi for RealSetupUi {
    fn confirm_git_init(&mut self, root: &Path) -> Result<bool> {
        Confirm::new(&format!(
            "{} is not a Git repo. Initialize one here with git init -b main?",
            display_path(root)
        ))
        .with_default(true)
        .prompt()
        .context("Git init confirmation")
    }

    fn select_runner(&mut self, runners: &[DetectedRunner]) -> Result<Runner> {
        let options = runners
            .iter()
            .cloned()
            .map(RunnerChoice)
            .collect::<Vec<_>>();
        Select::new("Which agent?", options)
            .prompt()
            .map(|choice| choice.0.runner)
            .context("agent runner selection")
    }

    fn plan_action(&mut self, plan: &DetectedSetupPlan) -> Result<PlanAction> {
        writeln!(self.stdout)?;
        render_plan_card(&mut self.stdout, plan)?;
        writeln!(self.stdout, "\n  Enter launch    c customize    q quit")?;
        self.stdout.flush()?;
        let action = read_plan_action()?;
        writeln!(self.stdout)?;
        self.stdout.flush()?;
        Ok(action)
    }

    fn customize(&mut self, plan: &DetectedSetupPlan) -> Result<DetectedSetupPlan> {
        customize_from(plan)
    }

    fn message(&mut self, message: &str) -> Result<()> {
        writeln!(self.stdout, "{message}")?;
        self.stdout.flush()?;
        Ok(())
    }
}

#[derive(Clone)]
struct RunnerChoice(DetectedRunner);

impl Display for RunnerChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.0.runner, self.0.version)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlanAction {
    Launch,
    Customize,
    Quit,
}

fn plan_action_for_key(key: KeyEvent) -> Option<PlanAction> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
        return Some(PlanAction::Quit);
    }
    let plain = key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT;
    match key.code {
        KeyCode::Enter => Some(PlanAction::Launch),
        KeyCode::Char('c' | 'C') if plain => Some(PlanAction::Customize),
        KeyCode::Char('q' | 'Q') if plain => Some(PlanAction::Quit),
        KeyCode::Esc => Some(PlanAction::Quit),
        _ => None,
    }
}

fn read_plan_action() -> Result<PlanAction> {
    let _guard = RawModeGuard::enter()?;
    loop {
        if let Event::Key(key) = event::read().context("reading setup card input")? {
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if let Some(action) = plan_action_for_key(key) {
                return Ok(action);
            }
        }
    }
}

struct RawModeGuard {
    active: bool,
}

impl RawModeGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enabling raw mode for setup card")?;
        Ok(Self { active: true })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = disable_raw_mode();
            self.active = false;
        }
    }
}

fn customize_from(plan: &DetectedSetupPlan) -> Result<DetectedSetupPlan> {
    println!();
    println!("Customize setup. Press Enter to keep each detected value.");
    println!();

    let (project_name, display_name) =
        prompt_project_name(plan.display_name.as_deref().unwrap_or(&plan.project_name))?;
    let repo_raw = text("Path to the repo:", &plan.repo_root.display().to_string())?;
    let repo_root = PathBuf::from(repo_raw.trim());
    let default_branch = text("Default branch:", &plan.default_branch)?
        .trim()
        .to_string();
    let remote_raw = text(
        "GitHub repo URL (optional):",
        plan.remote_url.as_deref().unwrap_or(""),
    )?;
    let remote_url = remote_url_for_storage(&remote_raw);

    let selected_runner = prompt_runner(
        "Agent runner (used by every workspace):",
        &plan.detected_runners,
        plan.selected_runner,
    )?;
    let orchestrator_runner = prompt_runner(
        "Orchestrator runner:",
        &plan.detected_runners,
        plan.orchestrator_runner,
    )?;

    Ok(DetectedSetupPlan {
        project_name,
        display_name,
        repo_root,
        default_branch,
        remote_url,
        selected_runner,
        orchestrator_runner,
        detected_runners: plan.detected_runners.clone(),
        tmux_version: plan.tmux_version.clone(),
        cpu_count: plan.cpu_count,
        git_init_root: plan.git_init_root.clone(),
    })
}

fn prompt_runner(label: &str, runners: &[DetectedRunner], default: Runner) -> Result<Runner> {
    // Customize is the detailed escape hatch, so retain the old wizard's
    // ability to choose either supported runner. The detected runner remains
    // preselected, and an unavailable alternative is labeled honestly.
    let options = customization_runner_choices(runners);
    let cursor = options
        .iter()
        .position(|choice| choice.0.runner == default)
        .unwrap_or(0);
    Select::new(label, options)
        .with_starting_cursor(cursor)
        .prompt()
        .map(|choice| choice.0.runner)
        .with_context(|| format!("runner prompt {label:?}"))
}

fn customization_runner_choices(runners: &[DetectedRunner]) -> Vec<RunnerChoice> {
    Runner::ALL
        .into_iter()
        .map(|runner| {
            runners
                .iter()
                .find(|detected| detected.runner == runner)
                .cloned()
                .unwrap_or_else(|| DetectedRunner {
                    runner,
                    version: "not detected on PATH".to_string(),
                })
        })
        .map(RunnerChoice)
        .collect()
}

/// The default project name derived from the repo basename: the slug plus the
/// human-readable label to keep when slugifying changed it. Falls back to
/// `my-project` (no label) when the basename has nothing to slugify.
fn wizard_default_project_name(cwd: &Path) -> (String, Option<String>) {
    cwd.file_name()
        .and_then(|name| name.to_str())
        .and_then(|raw| {
            shelbi_core::normalize_project_name(raw)
                .ok()
                .map(|slug| {
                    let display = (slug != raw).then(|| raw.to_string());
                    (slug, display)
                })
        })
        .unwrap_or_else(|| ("my-project".to_string(), None))
}

fn prompt_project_name(default: &str) -> Result<(String, Option<String>)> {
    use inquire::validator::{ErrorMessage, Validation};

    let validator = |value: &str| -> std::result::Result<Validation, inquire::CustomUserError> {
        if shelbi_core::normalize_project_name(value.trim()).is_ok() {
            Ok(Validation::Valid)
        } else {
            Ok(Validation::Invalid(ErrorMessage::Custom(
                "project name needs at least one letter or digit (for example, my-app)".into(),
            )))
        }
    };
    let raw = Text::new("Project name:")
        .with_default(default)
        .with_validator(validator)
        .prompt()
        .context("project name prompt")?;
    crate::project_root::slug_and_display(&raw)
}

fn write_workspace_settings_template(project: &str) -> Result<()> {
    let path = shelbi_state::config_project_dir(project)
        .map_err(|error| anyhow!(error))?
        .join("workspace-settings.json.template");
    if path.exists() {
        let existing = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        if serde_json::from_str::<serde_json::Value>(&existing).is_ok() {
            return Ok(());
        }
    }
    shelbi_state::ensure_dir(path.parent().expect("template path has a parent"))
        .map_err(|error| anyhow!(error))?;
    atomic_replace_file(
        &path,
        shelbi_state::DEFAULT_WORKSPACE_SETTINGS_TEMPLATE.as_bytes(),
    )?;
    Ok(())
}

fn atomic_replace_file(path: &Path, contents: &[u8]) -> Result<()> {
    let (temp_path, mut temp_file) = create_sibling_temp(path)?;
    if let Err(error) = temp_file.write_all(contents) {
        drop(temp_file);
        let _ = std::fs::remove_file(&temp_path);
        return Err(error).with_context(|| format!("writing {}", temp_path.display()));
    }
    drop(temp_file);
    if let Err(error) = std::fs::rename(&temp_path, path) {
        let _ = std::fs::remove_file(&temp_path);
        return Err(error)
            .with_context(|| format!("publishing {} as {}", temp_path.display(), path.display()));
    }
    Ok(())
}

pub(crate) fn create_sibling_temp(path: &Path) -> Result<(PathBuf, std::fs::File)> {
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

fn render_preflight_item(writer: &mut impl Write, item: &PreflightItem) -> Result<()> {
    writeln!(
        writer,
        "  {} {:<19} {}",
        if item.success { "✓" } else { "✗" },
        item.label,
        item.value
    )?;
    writer.flush()?;
    Ok(())
}

const MIN_CARD_INNER_WIDTH: usize = 58;
const MAX_CARD_INNER_WIDTH: usize = 76;
const CARD_LABEL_WIDTH: usize = 12;

fn render_plan_card(writer: &mut impl Write, plan: &DetectedSetupPlan) -> Result<()> {
    let title_name = truncate_card_text(&plan.project_name, MAX_CARD_INNER_WIDTH - 3);
    let title = format!("─ {title_name} ");
    let mut lines = Vec::new();
    lines.extend(card_rows(
        "repo",
        &format!(
            "{} ({})",
            display_path(&plan.repo_root),
            plan.default_branch
        ),
    ));
    lines.extend(card_rows(
        "github",
        &plan
            .remote_url
            .as_deref()
            .map(display_remote)
            .unwrap_or_else(|| "not configured".to_string()),
    ));
    lines.extend(card_rows("agent", plan.selected_runner.id()));
    lines.extend(card_rows(
        "workspaces",
        "created at first boot (orchestrator interview)",
    ));
    lines.extend(card_rows(
        "workflows",
        "task (branch → PR → review) · subtask",
    ));
    lines.extend(card_rows("agents", "orchestrator · developer · review"));
    lines.extend(card_rows("", "(+ qa, security, adversarial, opt-in)"));
    let footer = "  Everything above is editable later: Ctrl+Space → \"Edit\"";
    let card_width = lines
        .iter()
        .map(|line| line.width())
        .chain(std::iter::once(footer.width()))
        .chain(std::iter::once(title.width()))
        .max()
        .unwrap_or(MIN_CARD_INNER_WIDTH)
        .clamp(MIN_CARD_INNER_WIDTH, MAX_CARD_INNER_WIDTH);

    let title_len = title.width();
    writeln!(
        writer,
        "  ┌{}{}┐",
        title,
        "─".repeat(card_width.saturating_sub(title_len))
    )?;
    render_card_line(writer, card_width, "")?;
    for line in &lines {
        render_card_line(writer, card_width, line)?;
    }
    render_card_line(writer, card_width, "")?;
    render_card_line(writer, card_width, footer)?;
    writeln!(writer, "  └{}┘", "─".repeat(card_width))?;
    writer.flush()?;
    Ok(())
}

fn card_rows(label: &str, value: &str) -> Vec<String> {
    let value_width = MAX_CARD_INNER_WIDTH - 2 - CARD_LABEL_WIDTH;
    wrap_card_value(value, value_width)
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            format!(
                "  {:<width$}{value}",
                if index == 0 { label } else { "" },
                width = CARD_LABEL_WIDTH,
            )
        })
        .collect()
}

fn wrap_card_value(value: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in value.split_whitespace() {
        for chunk in split_word_by_width(word, width) {
            if current.is_empty() {
                current = chunk;
            } else if current.width() + 1 + chunk.width() <= width {
                current.push(' ');
                current.push_str(&chunk);
            } else {
                lines.push(std::mem::take(&mut current));
                current = chunk;
            }
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn split_word_by_width(word: &str, width: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;
    for character in word.chars() {
        let character_width = character.width().unwrap_or(0);
        if !current.is_empty() && current_width + character_width > width {
            chunks.push(std::mem::take(&mut current));
            current_width = 0;
        }
        current.push(character);
        current_width += character_width;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

fn truncate_card_text(value: &str, width: usize) -> String {
    if value.width() <= width {
        return value.to_string();
    }
    let ellipsis_width = '…'.width().unwrap_or(1);
    let available = width.saturating_sub(ellipsis_width);
    let mut rendered = String::new();
    let mut rendered_width = 0;
    for character in value.chars() {
        let character_width = character.width().unwrap_or(0);
        if rendered_width + character_width > available {
            break;
        }
        rendered.push(character);
        rendered_width += character_width;
    }
    rendered.push('…');
    rendered
}

fn render_card_line(writer: &mut impl Write, width: usize, content: &str) -> Result<()> {
    let padding = width.saturating_sub(content.width());
    writeln!(writer, "  │{content}{}│", " ".repeat(padding))?;
    Ok(())
}

fn display_path(path: &Path) -> String {
    let rendered = if let Some(home) = dirs::home_dir() {
        if path == home {
            "~".to_string()
        } else if let Ok(relative) = path.strip_prefix(&home) {
            format!("~/{}", relative.display())
        } else {
            path.display().to_string()
        }
    } else {
        path.display().to_string()
    };
    sanitize_terminal_text(&rendered)
}

fn display_remote(remote: &str) -> String {
    let sanitized = sanitize_terminal_text(remote);
    let remote = sanitized.trim().trim_end_matches('/');
    let display = if let Some(rest) = remote.strip_prefix("git@") {
        rest.split_once(':')
            .map(|(host, path)| format!("{host}/{path}"))
            .unwrap_or_else(|| rest.to_string())
    } else if let Some((_, rest)) = remote.split_once("://") {
        let rest = rest
            .rsplit_once('@')
            .map(|(_, value)| value)
            .unwrap_or(rest);
        rest.to_string()
    } else {
        remote.to_string()
    };
    display.trim_end_matches(".git").to_string()
}

/// Return a terminal-safe remote suitable for project YAML, dropping HTTP(S)
/// userinfo and URL query/fragment data where access tokens are commonly
/// embedded. SSH's conventional `git@host:path` form is preserved.
fn remote_url_for_storage(remote: &str) -> Option<String> {
    let sanitized = sanitize_terminal_text(remote);
    let remote = sanitized.trim();
    if remote.is_empty() {
        return None;
    }

    let Some((scheme, rest)) = remote.split_once("://") else {
        return Some(remote.to_string());
    };
    let rest = rest.split(['?', '#']).next().unwrap_or(rest);
    let scheme_lower = scheme.to_ascii_lowercase();
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let safe_authority = match authority.rsplit_once('@') {
        Some((userinfo, host)) if matches!(scheme_lower.as_str(), "ssh" | "git+ssh") => {
            let username = userinfo
                .split_once(':')
                .map(|(user, _)| user)
                .unwrap_or(userinfo);
            if username.is_empty() {
                host.to_string()
            } else {
                format!("{username}@{host}")
            }
        }
        Some((_, host)) => host.to_string(),
        None => authority.to_string(),
    };
    if safe_authority.is_empty() {
        return None;
    }
    let path = &rest[authority_end..];
    Some(format!("{scheme}://{safe_authority}{path}"))
}

fn sanitize_terminal_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::test_support::{EnvGuard, ENV_LOCK};
    use crossterm::event::KeyEventKind;
    use tempfile::TempDir;

    fn fixture_git(root: &Path) -> GitDefaults {
        GitDefaults {
            inside_git: true,
            repo_root: Some(root.to_path_buf()),
            default_branch: Some("main".to_string()),
            remote_url: Some("git@github.com:jlong/shaft.git".to_string()),
            probe_failure: None,
        }
    }

    fn fixture_runner(runner: Runner) -> DetectedRunner {
        DetectedRunner {
            runner,
            version: match runner {
                Runner::Claude => "2.1.0".to_string(),
                Runner::Codex => "0.101.0".to_string(),
            },
        }
    }

    fn fixture_plan(root: &Path, runner: Runner) -> DetectedSetupPlan {
        DetectedSetupPlan {
            project_name: "shaft".to_string(),
            display_name: None,
            repo_root: root.to_path_buf(),
            default_branch: "main".to_string(),
            remote_url: Some("git@github.com:jlong/shaft.git".to_string()),
            selected_runner: runner,
            orchestrator_runner: runner,
            detected_runners: vec![fixture_runner(runner)],
            tmux_version: "3.5a".to_string(),
            cpu_count: 10,
            git_init_root: None,
        }
    }

    struct FakeProbe {
        git_before_init: GitDefaults,
        git_after_init: Option<GitDefaults>,
        runners: Vec<DetectedRunner>,
        tmux_version: Option<String>,
        cpu_count: usize,
        init_calls: usize,
        init_error: Option<String>,
    }

    impl FakeProbe {
        fn ready(root: &Path, runners: Vec<DetectedRunner>) -> Self {
            Self {
                git_before_init: fixture_git(root),
                git_after_init: None,
                runners,
                tmux_version: Some("3.5a".to_string()),
                cpu_count: 10,
                init_calls: 0,
                init_error: None,
            }
        }
    }

    impl SetupProbe for FakeProbe {
        fn git_defaults(&mut self, _root: &Path) -> GitDefaults {
            if self.init_calls > 0 {
                self.git_after_init
                    .clone()
                    .unwrap_or_else(|| self.git_before_init.clone())
            } else {
                self.git_before_init.clone()
            }
        }

        fn init_git(&mut self, _root: &Path, _default_branch: &str) -> Result<()> {
            self.init_calls += 1;
            if let Some(error) = &self.init_error {
                bail!(error.clone());
            }
            Ok(())
        }

        fn runner_version(&mut self, runner: Runner) -> Option<String> {
            self.runners
                .iter()
                .find(|candidate| candidate.runner == runner)
                .map(|candidate| candidate.version.clone())
        }

        fn tmux_version(&mut self) -> Option<String> {
            self.tmux_version.clone()
        }

        fn cpu_count(&mut self) -> usize {
            self.cpu_count
        }
    }

    struct MockUi {
        confirm_git: bool,
        selected_runner: Runner,
        action: PlanAction,
        customized: Option<DetectedSetupPlan>,
        preflight: Vec<PreflightItem>,
        messages: Vec<String>,
        confirm_calls: usize,
        select_calls: usize,
        action_calls: usize,
        action_input: Option<DetectedSetupPlan>,
        customize_input: Option<DetectedSetupPlan>,
    }

    impl MockUi {
        fn new(action: PlanAction) -> Self {
            Self {
                confirm_git: true,
                selected_runner: Runner::Claude,
                action,
                customized: None,
                preflight: Vec::new(),
                messages: Vec::new(),
                confirm_calls: 0,
                select_calls: 0,
                action_calls: 0,
                action_input: None,
                customize_input: None,
            }
        }
    }

    impl PreflightSink for MockUi {
        fn emit(&mut self, item: PreflightItem) -> Result<()> {
            self.preflight.push(item);
            Ok(())
        }
    }

    impl SetupUi for MockUi {
        fn confirm_git_init(&mut self, _root: &Path) -> Result<bool> {
            self.confirm_calls += 1;
            Ok(self.confirm_git)
        }

        fn select_runner(&mut self, _runners: &[DetectedRunner]) -> Result<Runner> {
            self.select_calls += 1;
            Ok(self.selected_runner)
        }

        fn plan_action(&mut self, plan: &DetectedSetupPlan) -> Result<PlanAction> {
            self.action_calls += 1;
            self.action_input = Some(plan.clone());
            Ok(self.action)
        }

        fn customize(&mut self, plan: &DetectedSetupPlan) -> Result<DetectedSetupPlan> {
            self.customize_input = Some(plan.clone());
            Ok(self.customized.clone().unwrap_or_else(|| plan.clone()))
        }

        fn message(&mut self, message: &str) -> Result<()> {
            self.messages.push(message.to_string());
            Ok(())
        }
    }

    #[derive(Default)]
    struct RecordingWriter {
        bytes: Vec<u8>,
        flushes: usize,
    }

    impl Write for RecordingWriter {
        fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes += 1;
            Ok(())
        }
    }

    impl RecordingWriter {
        fn text(&self) -> String {
            String::from_utf8(self.bytes.clone()).unwrap()
        }
    }

    fn git(root: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(root)
            .status()
            .unwrap();
        assert!(
            status.success(),
            "git {args:?} failed in {}",
            root.display()
        );
    }

    #[test]
    fn runner_resolution_covers_one_both_neither_and_explicit() {
        let claude = fixture_runner(Runner::Claude);
        let codex = fixture_runner(Runner::Codex);

        assert_eq!(
            resolve_runner(std::slice::from_ref(&claude), None).unwrap(),
            Runner::Claude
        );
        assert_eq!(
            resolve_runner(std::slice::from_ref(&codex), None).unwrap(),
            Runner::Codex
        );
        assert!(resolve_runner(&[], None)
            .unwrap_err()
            .to_string()
            .contains("No supported agent runner"));
        assert!(resolve_runner(&[claude.clone(), codex.clone()], None)
            .unwrap_err()
            .to_string()
            .contains("--runner claude"));
        assert_eq!(
            resolve_runner(&[claude.clone(), codex], Some(Runner::Codex)).unwrap(),
            Runner::Codex
        );
        assert!(resolve_runner(&[claude], Some(Runner::Codex))
            .unwrap_err()
            .to_string()
            .contains("was not found on PATH"));
    }

    #[test]
    fn scripted_overrides_win_deterministically_and_validate_before_writes() {
        let root = TempDir::new().unwrap();
        let mut plan = fixture_plan(root.path(), Runner::Codex);
        plan.detected_runners = vec![
            fixture_runner(Runner::Claude),
            fixture_runner(Runner::Codex),
        ];

        plan.apply_overrides(SetupPlanOverrides {
            project_name: Some("My Demo".to_string()),
            default_branch: Some(" develop ".to_string()),
            remote_url: Some(
                "https://user:secret@github.com/example/demo.git?token=hidden".to_string(),
            ),
            orchestrator_runner: Some(Runner::Claude),
        })
        .unwrap();

        assert_eq!(plan.project_name, "my-demo");
        assert_eq!(plan.default_branch, "develop");
        assert_eq!(
            plan.remote_url.as_deref(),
            Some("https://github.com/example/demo.git")
        );
        assert_eq!(plan.selected_runner, Runner::Codex);
        assert_eq!(plan.orchestrator_runner, Runner::Claude);

        let project = plan.to_project().unwrap();
        assert_eq!(project.orchestrator.runner, "claude");
        // Workspace provisioning is deferred to the orchestrator's first-boot
        // interview, so a scripted init produces an empty pool.
        assert!(project.workspaces.is_empty());

        let mut invalid_branch = fixture_plan(root.path(), Runner::Claude);
        assert!(invalid_branch
            .apply_overrides(SetupPlanOverrides {
                default_branch: Some("bad..branch".to_string()),
                ..SetupPlanOverrides::default()
            })
            .unwrap_err()
            .to_string()
            .contains("invalid --default-branch"));

        let mut unavailable = fixture_plan(root.path(), Runner::Claude);
        assert!(unavailable
            .apply_overrides(SetupPlanOverrides {
                orchestrator_runner: Some(Runner::Codex),
                ..SetupPlanOverrides::default()
            })
            .unwrap_err()
            .to_string()
            .contains("orchestrator runner codex was not found"));
    }

    #[test]
    fn customization_keeps_both_runner_choices_and_labels_missing_alternative() {
        let choices = customization_runner_choices(&[fixture_runner(Runner::Claude)]);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].0.runner, Runner::Claude);
        assert_eq!(choices[0].0.version, "2.1.0");
        assert_eq!(choices[1].0.runner, Runner::Codex);
        assert_eq!(choices[1].0.version, "not detected on PATH");
    }

    #[test]
    fn one_runner_is_silent_and_both_prompt_exactly_once() {
        let root = TempDir::new().unwrap();

        let mut one_probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        let mut one_ui = MockUi::new(PlanAction::Quit);
        assert_eq!(
            setup_one_project_with(root.path(), &mut one_probe, &mut one_ui).unwrap(),
            SetupOutcome::Quit
        );
        assert_eq!(one_ui.select_calls, 0);
        assert_eq!(one_ui.action_calls, 1);

        let mut both_probe = FakeProbe::ready(
            root.path(),
            vec![
                fixture_runner(Runner::Claude),
                fixture_runner(Runner::Codex),
            ],
        );
        let mut both_ui = MockUi::new(PlanAction::Quit);
        both_ui.selected_runner = Runner::Codex;
        assert_eq!(
            setup_one_project_with(root.path(), &mut both_probe, &mut both_ui).unwrap(),
            SetupOutcome::Quit
        );
        assert_eq!(both_ui.select_calls, 1);
        assert_eq!(both_ui.action_calls, 1);
    }

    #[test]
    fn neither_runner_and_missing_tmux_stop_before_selector_or_card() {
        let root = TempDir::new().unwrap();

        let mut no_runner_probe = FakeProbe::ready(root.path(), Vec::new());
        let mut no_runner_ui = MockUi::new(PlanAction::Launch);
        let error = setup_one_project_with(root.path(), &mut no_runner_probe, &mut no_runner_ui)
            .unwrap_err();
        assert!(error.to_string().contains("No supported agent runner"));
        assert_eq!(no_runner_ui.select_calls, 0);
        assert_eq!(no_runner_ui.action_calls, 0);

        let mut no_tmux_probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        no_tmux_probe.tmux_version = None;
        let mut no_tmux_ui = MockUi::new(PlanAction::Launch);
        let error =
            setup_one_project_with(root.path(), &mut no_tmux_probe, &mut no_tmux_ui).unwrap_err();
        assert!(error.to_string().contains("tmux was not found"));
        assert_eq!(no_tmux_ui.select_calls, 0);
        assert_eq!(no_tmux_ui.action_calls, 0);

        let mut neither_probe = FakeProbe::ready(root.path(), Vec::new());
        neither_probe.tmux_version = None;
        let mut neither_ui = MockUi::new(PlanAction::Launch);
        let error =
            setup_one_project_with(root.path(), &mut neither_probe, &mut neither_ui).unwrap_err();
        let guidance = error.to_string();
        assert!(guidance.contains("No supported agent runner"));
        assert!(guidance.contains("tmux was not found"));
        assert_eq!(neither_ui.select_calls, 0);
        assert_eq!(neither_ui.action_calls, 0);
    }

    #[test]
    fn preflight_is_emitted_in_stable_progressive_order() {
        let root = TempDir::new().unwrap();
        let mut probe = FakeProbe::ready(
            root.path(),
            vec![
                fixture_runner(Runner::Claude),
                fixture_runner(Runner::Codex),
            ],
        );
        let mut ui = MockUi::new(PlanAction::Quit);
        setup_one_project_with(root.path(), &mut probe, &mut ui).unwrap();

        let labels = ui
            .preflight
            .iter()
            .map(|item| item.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            vec![
                "git repo",
                "default branch",
                "remote",
                "agent",
                "tmux",
                "machine"
            ]
        );
        assert!(ui.preflight.iter().all(|item| item.success));
        assert!(ui.preflight[3].value.contains("claude 2.1.0"));
        assert!(ui.preflight[3].value.contains("codex 0.101.0"));
        assert_eq!(ui.preflight[5].value, "10 cores");
    }

    #[test]
    fn missing_git_branch_and_remote_use_stable_non_panicking_defaults() {
        let root = TempDir::new().unwrap();
        let mut probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        probe.git_before_init.default_branch = None;
        probe.git_before_init.remote_url = None;
        let mut sink = MockUi::new(PlanAction::Quit);

        let plan = detect_setup_plan_with(root.path(), Some(Runner::Claude), &mut probe, &mut sink)
            .unwrap();
        assert_eq!(plan.default_branch, "main");
        assert!(plan.remote_url.is_none());
        assert_eq!(sink.preflight[1].value, "main");
        assert_eq!(sink.preflight[2].value, "not configured");

        let mut writer = RecordingWriter::default();
        render_plan_card(&mut writer, &plan).unwrap();
        assert!(writer.text().contains("github      not configured"));
    }

    #[test]
    fn non_git_decline_and_q_are_write_free_then_launch_initializes_once() {
        let _lock = ENV_LOCK.lock().unwrap();
        let root = TempDir::new().unwrap();
        let home = root.path().join("home");
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);
        let non_git = GitDefaults {
            inside_git: false,
            repo_root: None,
            default_branch: None,
            remote_url: None,
            probe_failure: None,
        };

        let mut decline_probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        decline_probe.git_before_init = non_git.clone();
        let mut decline_ui = MockUi::new(PlanAction::Launch);
        decline_ui.confirm_git = false;
        assert_eq!(
            setup_one_project_with(root.path(), &mut decline_probe, &mut decline_ui).unwrap(),
            SetupOutcome::Quit
        );
        assert_eq!(decline_probe.init_calls, 0);
        assert_eq!(decline_ui.confirm_calls, 1);
        assert!(decline_ui.preflight.is_empty());
        assert_eq!(decline_ui.action_calls, 0);

        let mut accept_probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        accept_probe.git_before_init = non_git.clone();
        accept_probe.git_after_init = Some(fixture_git(root.path()));
        let mut accept_ui = MockUi::new(PlanAction::Quit);
        assert_eq!(
            setup_one_project_with(root.path(), &mut accept_probe, &mut accept_ui).unwrap(),
            SetupOutcome::Quit
        );
        assert_eq!(accept_probe.init_calls, 0);
        assert_eq!(accept_ui.confirm_calls, 1);
        assert_eq!(accept_ui.action_calls, 1);

        let mut launch_probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        launch_probe.git_before_init = non_git.clone();
        launch_probe.git_after_init = Some(fixture_git(root.path()));
        initialize_git_if_needed(root.path(), "main", &mut launch_probe).unwrap();
        assert_eq!(launch_probe.init_calls, 1);

        let mut failed_probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        failed_probe.git_before_init = non_git.clone();
        failed_probe.init_error = Some("git init failed: permission denied".to_string());
        let mut failed_ui = MockUi::new(PlanAction::Launch);
        let error =
            setup_one_project_with(root.path(), &mut failed_probe, &mut failed_ui).unwrap_err();
        assert!(error.to_string().contains("permission denied"));
        assert_eq!(failed_ui.action_calls, 1);

        let mut unverified_probe =
            FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        unverified_probe.git_before_init = non_git.clone();
        unverified_probe.git_after_init = Some(non_git);
        let mut unverified_ui = MockUi::new(PlanAction::Launch);
        let error = setup_one_project_with(root.path(), &mut unverified_probe, &mut unverified_ui)
            .unwrap_err();
        assert!(error.to_string().contains("reported success"));
        assert_eq!(unverified_ui.action_calls, 1);

        let mut detected_probe =
            FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        detected_probe.git_before_init = GitDefaults {
            inside_git: false,
            repo_root: None,
            default_branch: None,
            remote_url: None,
            probe_failure: None,
        };
        let mut sink = SilentPreflight;
        let detected = detect_setup_plan_with(
            root.path(),
            Some(Runner::Claude),
            &mut detected_probe,
            &mut sink,
        )
        .unwrap();
        assert_eq!(detected.git_init_root.as_deref(), Some(root.path()));
        assert_eq!(detected.repo_root, root.path());
        assert_eq!(detected_probe.init_calls, 0);
    }

    #[test]
    fn git_probe_failures_stop_instead_of_offering_destructive_init() {
        let root = TempDir::new().unwrap();
        let mut probe = FakeProbe::ready(root.path(), vec![fixture_runner(Runner::Claude)]);
        probe.git_before_init = GitDefaults {
            inside_git: false,
            repo_root: None,
            default_branch: None,
            remote_url: None,
            probe_failure: Some("dubious ownership; configure safe.directory".to_string()),
        };
        let mut ui = MockUi::new(PlanAction::Launch);

        let error = setup_one_project_with(root.path(), &mut probe, &mut ui).unwrap_err();
        assert!(error.to_string().contains("safe.directory"));
        assert_eq!(ui.confirm_calls, 0);
        assert!(ui.preflight.is_empty());
        assert_eq!(probe.init_calls, 0);
    }

    #[test]
    fn direct_setup_preserves_fresh_clone_pickup_guidance() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let repo = temp.path().join("fresh-clone");
        std::fs::create_dir_all(repo.join(".shelbi")).unwrap();
        std::fs::write(repo.join(".shelbi/project.yaml"), "name: shared\n").unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        let error = preserve_in_repo_pickup_contract(&repo).unwrap_err();
        assert!(error.to_string().contains("shelbi init --pick-up"));
        assert!(!home.exists(), "pickup detection must remain read-only");
    }

    #[test]
    fn non_git_enter_initializes_then_persists_after_one_confirmation() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("new-repo");
        std::fs::create_dir_all(&root).unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        let mut probe = FakeProbe::ready(&root, vec![fixture_runner(Runner::Claude)]);
        probe.git_before_init = GitDefaults {
            inside_git: false,
            repo_root: None,
            default_branch: None,
            remote_url: None,
            probe_failure: None,
        };
        probe.git_after_init = Some(fixture_git(&root));
        let mut ui = MockUi::new(PlanAction::Launch);

        assert_eq!(
            setup_one_project_with(&root, &mut probe, &mut ui).unwrap(),
            SetupOutcome::Created("new-repo".to_string())
        );
        assert_eq!(ui.confirm_calls, 1);
        assert_eq!(probe.init_calls, 1);
        assert!(home.join("projects/new-repo.yaml").is_file());
    }

    #[test]
    fn action_parser_covers_enter_customize_quit_and_modifiers() {
        let key = |code, modifiers| KeyEvent::new(code, modifiers);
        assert_eq!(
            plan_action_for_key(key(KeyCode::Enter, KeyModifiers::NONE)),
            Some(PlanAction::Launch)
        );
        for code in [KeyCode::Char('c'), KeyCode::Char('C')] {
            assert_eq!(
                plan_action_for_key(key(code, KeyModifiers::NONE)),
                Some(PlanAction::Customize)
            );
        }
        for code in [KeyCode::Char('q'), KeyCode::Char('Q'), KeyCode::Esc] {
            assert_eq!(
                plan_action_for_key(key(code, KeyModifiers::NONE)),
                Some(PlanAction::Quit)
            );
        }
        assert_eq!(
            plan_action_for_key(key(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Some(PlanAction::Quit)
        );
        assert_eq!(
            plan_action_for_key(key(KeyCode::Char('c'), KeyModifiers::ALT)),
            None
        );
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(plan_action_for_key(release), None);
        assert_eq!(
            plan_action_for_key(key(KeyCode::Char('x'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn preflight_renderer_flushes_each_resolved_row() {
        let mut writer = RecordingWriter::default();
        render_preflight_item(
            &mut writer,
            &PreflightItem::ok("git repo", "~/Workspaces/shaft"),
        )
        .unwrap();
        render_preflight_item(
            &mut writer,
            &PreflightItem::failed("tmux", "not found on PATH"),
        )
        .unwrap();
        assert_eq!(writer.flushes, 2);
        let text = writer.text();
        assert!(text.contains("✓ git repo"));
        assert!(text.contains("✗ tmux"));
    }

    #[test]
    fn plan_card_renders_complete_plan_without_truncation() {
        let root = Path::new("/Users/example/Workspaces/shaft");
        let plan = fixture_plan(root, Runner::Claude);
        let mut writer = RecordingWriter::default();
        render_plan_card(&mut writer, &plan).unwrap();
        let card = writer.text();
        for expected in [
            "shaft",
            "github.com/jlong/shaft",
            "claude",
            // Workspaces are no longer provisioned at init — the card points
            // at the orchestrator's first-boot interview instead of listing
            // slot names.
            "created at first boot (orchestrator interview)",
            "task (branch → PR → review) · subtask",
            "orchestrator · developer · review",
            "(+ qa, security, adversarial, opt-in)",
            "Ctrl+Space → \"Edit\"",
        ] {
            assert!(
                card.contains(expected),
                "missing {expected:?} from:\n{card}"
            );
        }
        assert!(
            !card.contains('…'),
            "card discarded detected fields:\n{card}"
        );
        assert!(
            card.contains("workspaces  created at first boot"),
            "card labels ran together:\n{card}"
        );
    }

    #[test]
    fn plan_card_wraps_long_values_to_terminal_friendly_width() {
        let mut plan = fixture_plan(Path::new("/tmp/shaft"), Runner::Claude);
        plan.repo_root = PathBuf::from(format!("/tmp/{}", "專案".repeat(24)));
        let mut writer = RecordingWriter::default();
        render_plan_card(&mut writer, &plan).unwrap();
        let card = writer.text();

        for line in card.lines() {
            assert!(
                line.width() <= MAX_CARD_INNER_WIDTH + 4,
                "card line is too wide ({} columns): {line:?}",
                line.width()
            );
        }
    }

    #[test]
    fn remote_display_normalizes_common_forms_and_redacts_userinfo() {
        assert_eq!(
            display_remote("git@github.com:jlong/shaft.git"),
            "github.com/jlong/shaft"
        );
        assert_eq!(
            display_remote("https://github.com/jlong/shaft.git"),
            "github.com/jlong/shaft"
        );
        assert_eq!(
            display_remote("https://secret-token@github.com/jlong/shaft.git"),
            "github.com/jlong/shaft"
        );
        assert_eq!(
            display_remote("ssh://git@github.com/jlong/shaft.git"),
            "github.com/jlong/shaft"
        );
        assert_eq!(
            remote_url_for_storage(
                "https://oauth-user:secret-token@github.com/jlong/shaft.git?access_token=also-secret#fragment"
            )
            .as_deref(),
            Some("https://github.com/jlong/shaft.git")
        );
        assert_eq!(
            remote_url_for_storage("git@github.com:jlong/shaft.git").as_deref(),
            Some("git@github.com:jlong/shaft.git")
        );
        assert_eq!(
            remote_url_for_storage("ssh://git:secret@github.com/jlong/shaft.git").as_deref(),
            Some("ssh://git@github.com/jlong/shaft.git")
        );
        assert_eq!(
            remote_url_for_storage("ftp://user:secret@example.com/repo?token=hidden").as_deref(),
            Some("ftp://example.com/repo")
        );
    }

    #[test]
    fn version_normalization_is_non_panicking_and_strips_terminal_controls() {
        assert_eq!(normalize_version("tmux", "tmux 3.5a"), "3.5a");
        assert_eq!(normalize_version("claude", "Claude Code 2.1.0"), "2.1.0");
        assert_eq!(normalize_version("codex", "codex-cli 0.101.0"), "0.101.0");
        assert_eq!(normalize_version("claude", "2.1\u{1b}[31m"), "2.1[31m");
        assert_eq!(normalize_version("claude", ""), "");
    }

    #[cfg(unix)]
    #[test]
    fn version_probe_stops_a_hung_runner_at_its_deadline() {
        let start = std::time::Instant::now();
        assert!(
            command_version_with_deadline("/bin/sleep", &["5"], Duration::from_millis(50))
                .is_none()
        );
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn git_probe_handles_fresh_nested_missing_remote_and_slash_branch() {
        let temp = TempDir::new().unwrap();
        let repo = temp.path().join("Shaft");
        std::fs::create_dir_all(repo.join("src/nested")).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);

        let fresh = GitDefaults::probe(&repo.join("src/nested"));
        assert!(fresh.inside_git);
        assert_eq!(
            std::fs::canonicalize(fresh.repo_root.unwrap()).unwrap(),
            std::fs::canonicalize(&repo).unwrap()
        );
        assert_eq!(fresh.default_branch.as_deref(), Some("main"));
        assert!(fresh.remote_url.is_none());

        git(
            &repo,
            &["remote", "add", "origin", "git@github.com:jlong/shaft.git"],
        );
        git(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/release/2026",
            ],
        );
        let with_origin = GitDefaults::probe(&repo);
        assert_eq!(with_origin.default_branch.as_deref(), Some("release/2026"));
        assert_eq!(
            with_origin.remote_url.as_deref(),
            Some("git@github.com:jlong/shaft.git")
        );

        let non_repo = temp.path().join("plain");
        std::fs::create_dir_all(&non_repo).unwrap();
        let none = GitDefaults::probe(&non_repo);
        assert!(!none.inside_git);
        assert!(none.repo_root.is_none());
        assert!(none.default_branch.is_none());
        assert!(none.remote_url.is_none());
    }

    #[test]
    fn git_root_parser_preserves_path_whitespace_and_rejects_lossy_paths() {
        assert_eq!(
            git_repo_root_from_stdout(b"/tmp/repo with space \n").unwrap(),
            PathBuf::from("/tmp/repo with space ")
        );
        assert_eq!(
            git_repo_root_from_stdout(b"/tmp/repo-with-newline\n\n").unwrap(),
            PathBuf::from("/tmp/repo-with-newline\n")
        );
        assert!(git_repo_root_from_stdout(b"\xff\n").is_err());
    }

    #[test]
    fn default_plan_sets_orchestrator_runner_and_empty_pool() {
        let root = TempDir::new().unwrap();
        let plan = fixture_plan(root.path(), Runner::Codex);
        let project = plan.to_project().unwrap();
        assert_eq!(project.orchestrator.runner, "codex");
        // Workspaces are provisioned by the orchestrator's first-boot
        // interview, not at init — a fresh project has an empty pool.
        assert!(project.workspaces.is_empty());
        assert_eq!(project.default_workflow.as_deref(), Some("task"));
        assert_eq!(
            project
                .agent_runners
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["claude", "codex"]
        );
    }

    #[test]
    fn launch_persists_complete_scaffold_but_quit_writes_no_state() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("shaft");
        std::fs::create_dir_all(&root).unwrap();

        let quit_home = temp.path().join("quit-home");
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &quit_home);
        let mut quit_probe = FakeProbe::ready(&root, vec![fixture_runner(Runner::Claude)]);
        let mut quit_ui = MockUi::new(PlanAction::Quit);
        assert_eq!(
            setup_one_project_with(&root, &mut quit_probe, &mut quit_ui).unwrap(),
            SetupOutcome::Quit
        );
        assert!(
            !quit_home.exists(),
            "q must not create even the Shelbi root directory"
        );

        let launch_home = temp.path().join("launch-home");
        env.set("SHELBI_HOME", &launch_home);
        let mut launch_probe = FakeProbe::ready(&root, vec![fixture_runner(Runner::Codex)]);
        let mut launch_ui = MockUi::new(PlanAction::Launch);
        assert_eq!(
            setup_one_project_with(&root, &mut launch_probe, &mut launch_ui).unwrap(),
            SetupOutcome::Created("shaft".to_string())
        );

        let project = shelbi_state::load_project("shaft").unwrap();
        assert_eq!(project.orchestrator.runner, "codex");
        assert!(project
            .workspaces
            .iter()
            .all(|workspace| workspace.runner == "codex"));
        assert!(launch_home.join("sessions/default.yaml").is_file());
        shelbi_state::load_session("default").unwrap();
        assert!(launch_home.join("config.yaml").is_file());
        assert!(launch_home
            .join("projects/shaft/workspace-settings.json.template")
            .is_file());
        assert!(launch_home
            .join("projects/shaft/agents/developer/instructions.md")
            .is_file());
        assert!(launch_home
            .join("projects/shaft/workflows/task.yaml")
            .is_file());
        assert!(launch_home
            .join("projects/shaft/workflows/subtask.yaml")
            .is_file());
        assert!(launch_home
            .join("projects/shaft/workflows/statuses.yaml")
            .is_file());
        assert!(shelbi_state::zenmode_path("shaft").unwrap().is_file());
        let tasks = shelbi_state::list_tasks("shaft").unwrap();
        assert_eq!(tasks.len(), 1);
        let welcome = &tasks[0];
        assert_eq!(welcome.task.title, shelbi_state::WELCOME_TASK_TITLE);
        assert_eq!(welcome.task.column, shelbi_core::Column::backlog());
        for concept in [
            "Backlog to Todo",
            "automatically dispatches",
            "Ctrl+P",
            "safe to delete",
        ] {
            assert!(welcome.body.contains(concept), "missing `{concept}`");
        }
        assert!(
            !shelbi_state::read_global_state().unwrap().first_run_seen,
            "the machine's first project must arm the launch hint"
        );
        assert!(
            shelbi_state::read_state("shaft")
                .unwrap()
                .contextual_greeting_pending,
            "new project must be armed for its one-shot contextual greeting"
        );
    }

    #[test]
    fn interactive_enter_and_scripted_acceptance_persist_identical_state() {
        fn snapshot_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
            fn visit(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
                let mut entries = std::fs::read_dir(path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path())
                    .collect::<Vec<_>>();
                entries.sort();
                for entry in entries {
                    if entry.is_dir() {
                        visit(base, &entry, files);
                    } else {
                        files.insert(
                            entry.strip_prefix(base).unwrap().to_path_buf(),
                            std::fs::read(&entry).unwrap(),
                        );
                    }
                }
            }

            let mut files = BTreeMap::new();
            visit(root, root, &mut files);
            files
        }

        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("shaft");
        let interactive_home = temp.path().join("interactive-home");
        let scripted_home = temp.path().join("scripted-home");
        std::fs::create_dir_all(&root).unwrap();
        git(&root, &["init", "-q", "-b", "main"]);
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        let plan = fixture_plan(&root, Runner::Codex);

        env.set("SHELBI_HOME", &interactive_home);
        let mut interactive_probe = FakeProbe::ready(&root, vec![fixture_runner(Runner::Codex)]);
        let mut interactive_ui = MockUi::new(PlanAction::Launch);
        assert_eq!(
            setup_one_project_with(&root, &mut interactive_probe, &mut interactive_ui).unwrap(),
            SetupOutcome::Created("shaft".to_string())
        );
        assert_eq!(interactive_ui.confirm_calls, 0);
        assert_eq!(interactive_ui.select_calls, 0);
        assert_eq!(interactive_ui.action_calls, 1);
        assert_eq!(interactive_ui.action_input.as_ref(), Some(&plan));

        env.set("SHELBI_HOME", &scripted_home);
        assert_eq!(
            accept_setup_plan(plan).unwrap(),
            SetupOutcome::Created("shaft".to_string())
        );

        let mut interactive_files = snapshot_files(&interactive_home);
        let mut scripted_files = snapshot_files(&scripted_home);
        let welcome_path = PathBuf::from("projects")
            .join("shaft")
            .join("tasks")
            .join(format!("{}.md", shelbi_state::WELCOME_TASK_ID));
        let parse_welcome = |files: &mut BTreeMap<PathBuf, Vec<u8>>| {
            let contents = files
                .remove(&welcome_path)
                .expect("Welcome task must be present in the scaffold snapshot");
            let text = std::str::from_utf8(&contents).expect("Welcome task must be UTF-8");
            shelbi_state::parse_task_file(text).expect("Welcome task must parse")
        };
        let mut interactive_welcome = parse_welcome(&mut interactive_files);
        let scripted_welcome = parse_welcome(&mut scripted_files);

        assert_eq!(interactive_files, scripted_files);
        assert_eq!(interactive_welcome.body, scripted_welcome.body);

        // Welcome timestamps are generated when each scaffold is committed.
        // Normalize only those volatile fields before comparing all remaining
        // frontmatter semantically.
        interactive_welcome.task.created_at = scripted_welcome.task.created_at;
        interactive_welcome.task.updated_at = scripted_welcome.task.updated_at;
        assert_eq!(
            serde_yaml::to_value(&interactive_welcome.task).unwrap(),
            serde_yaml::to_value(&scripted_welcome.task).unwrap()
        );
    }

    #[test]
    fn persistence_redacts_remote_credentials_and_repairs_interrupted_scaffolds() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("shaft");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(home.join("projects/shaft/agents/developer")).unwrap();
        std::fs::write(
            home.join("projects/shaft/workspace-settings.json.template"),
            "{ interrupted",
        )
        .unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        let mut plan = fixture_plan(&root, Runner::Claude);
        plan.remote_url = Some(
            "https://oauth-user:secret-token@github.com/jlong/shaft.git?access_token=also-secret"
                .to_string(),
        );
        persist_plan(&plan).unwrap();

        let registration = std::fs::read_to_string(home.join("projects/shaft.yaml")).unwrap();
        assert!(!registration.contains("secret-token"));
        assert!(!registration.contains("also-secret"));
        assert!(registration.contains("https://github.com/jlong/shaft.git"));
        let template =
            std::fs::read_to_string(home.join("projects/shaft/workspace-settings.json.template"))
                .unwrap();
        serde_json::from_str::<serde_json::Value>(&template).unwrap();
        assert!(home
            .join("projects/shaft/agents/developer/instructions.md")
            .is_file());
    }

    #[test]
    fn reopen_and_later_project_do_not_reseed_or_rearm_onboarding() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("shaft");
        std::fs::create_dir_all(&root).unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        persist_plan(&fixture_plan(&root, Runner::Claude)).unwrap();
        let welcome_path = shelbi_state::task_path("shaft", shelbi_state::WELCOME_TASK_ID).unwrap();
        assert!(welcome_path.is_file());

        // Deleting the guide is permanent. An ordinary configured-project
        // load (including its compatibility migrations) must remain read-only
        // with respect to the Welcome seed.
        std::fs::remove_file(&welcome_path).unwrap();
        shelbi_state::load_project("shaft").unwrap();
        assert!(!welcome_path.exists());

        // Simulate the first sidebar claim, then add another project through
        // the same detected scaffold used by `shelbi project add`.
        assert!(shelbi_state::claim_first_run_hint().unwrap());
        let mut later = fixture_plan(&root, Runner::Codex);
        later.project_name = "later-project".to_string();
        persist_plan(&later).unwrap();
        assert!(shelbi_state::read_global_state().unwrap().first_run_seen);
        assert_eq!(shelbi_state::list_tasks("later-project").unwrap().len(), 1);
        assert!(
            !welcome_path.exists(),
            "scaffolding another project must not resurrect a deleted guide"
        );
    }

    #[test]
    fn customize_receives_detected_defaults_and_persists_its_result() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("shaft");
        std::fs::create_dir_all(&root).unwrap();
        let home = temp.path().join("home");
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        let mut probe = FakeProbe::ready(
            &root,
            vec![
                fixture_runner(Runner::Claude),
                fixture_runner(Runner::Codex),
            ],
        );
        let mut ui = MockUi::new(PlanAction::Customize);
        ui.selected_runner = Runner::Claude;
        let mut customized = fixture_plan(&root, Runner::Codex);
        customized.project_name = "custom-shaft".to_string();
        customized.default_branch = "develop".to_string();
        customized.orchestrator_runner = Runner::Claude;
        customized.detected_runners = vec![
            fixture_runner(Runner::Claude),
            fixture_runner(Runner::Codex),
        ];
        ui.customized = Some(customized);

        assert_eq!(
            setup_one_project_with(&root, &mut probe, &mut ui).unwrap(),
            SetupOutcome::Created("custom-shaft".to_string())
        );
        let detected = ui.customize_input.expect("customize received plan");
        assert_eq!(detected.project_name, "shaft");
        assert_eq!(detected.repo_root, root);
        assert_eq!(detected.default_branch, "main");
        assert_eq!(
            detected.remote_url.as_deref(),
            Some("git@github.com:jlong/shaft.git")
        );
        assert_eq!(detected.selected_runner, Runner::Claude);
        assert_eq!(detected.orchestrator_runner, Runner::Claude);
        assert_eq!(detected.detected_runners.len(), 2);
        assert_eq!(detected.tmux_version, "3.5a");
        assert_eq!(detected.cpu_count, 10);

        let saved = shelbi_state::load_project("custom-shaft").unwrap();
        assert_eq!(saved.default_branch, "develop");
        assert_eq!(saved.orchestrator.runner, "claude");
        // The pool is provisioned later by the orchestrator's first-boot
        // interview, so the saved project starts empty.
        assert!(saved.workspaces.is_empty());
    }

    #[test]
    fn flat_and_split_collisions_are_rejected_before_any_scaffolding_writes() {
        let _lock = ENV_LOCK.lock().unwrap();
        let temp = TempDir::new().unwrap();
        let home = temp.path().join("home");
        let root = temp.path().join("shaft");
        std::fs::create_dir_all(home.join("projects")).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let original = "name: shaft\nrepo: keep-me\n";
        std::fs::write(home.join("projects/shaft.yaml"), original).unwrap();
        let env = EnvGuard::new(&["SHELBI_HOME"]);
        env.set("SHELBI_HOME", &home);

        let mut pending_init = fixture_plan(&root, Runner::Claude);
        pending_init.git_init_root = Some(root.clone());
        let mut collision_probe = FakeProbe::ready(&root, vec![fixture_runner(Runner::Claude)]);
        collision_probe.git_before_init = GitDefaults {
            inside_git: false,
            repo_root: None,
            default_branch: None,
            remote_url: None,
            probe_failure: None,
        };
        let mut collision_ui = MockUi::new(PlanAction::Launch);
        let error = create_project_from_plan(pending_init, &mut collision_probe, &mut collision_ui)
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("no existing state was overwritten"));
        assert_eq!(collision_probe.init_calls, 0);

        let error = persist_plan(&fixture_plan(&root, Runner::Claude)).unwrap_err();
        assert!(error
            .to_string()
            .contains("no existing state was overwritten"));
        assert_eq!(
            std::fs::read_to_string(home.join("projects/shaft.yaml")).unwrap(),
            original
        );
        assert!(
            !shelbi_state::task_path("shaft", shelbi_state::WELCOME_TASK_ID)
                .unwrap()
                .exists(),
            "an existing registration must not gain the Welcome card"
        );
        assert!(!home.join("sessions").exists());
        assert!(!home.join("config.yaml").exists());

        std::fs::remove_file(home.join("projects/shaft.yaml")).unwrap();
        std::fs::create_dir_all(home.join("projects/shaft")).unwrap();
        let split_original = "repo: keep-split-me\n";
        std::fs::write(home.join("projects/shaft/local.yaml"), split_original).unwrap();

        let error = persist_plan(&fixture_plan(&root, Runner::Claude)).unwrap_err();
        assert!(error.to_string().contains("projects/shaft/local.yaml"));
        assert_eq!(
            std::fs::read_to_string(home.join("projects/shaft/local.yaml")).unwrap(),
            split_original
        );
        assert!(!home.join("sessions").exists());
        assert!(!home.join("config.yaml").exists());
    }

    #[test]
    fn failure_guidance_is_platform_appropriate() {
        assert!(missing_tmux_guidance(Platform::MacOs).contains("brew install tmux"));
        let linux = missing_tmux_guidance(Platform::DebianLike);
        assert!(linux.contains("apt install tmux"));
        assert!(linux.contains("dnf install tmux"));
        assert!(missing_tmux_guidance(Platform::Other).contains("package manager"));
        let runners = missing_runner_guidance();
        assert!(runners.contains("Claude Code"));
        assert!(runners.contains("@openai/codex"));
        assert!(!runners.contains("+    "));
    }

    #[test]
    fn project_name_helper_retains_existing_behavior() {
        assert_eq!(
            wizard_default_project_name(Path::new("/tmp/Shaft")),
            ("shaft".to_string(), Some("Shaft".to_string()))
        );
        assert_eq!(
            wizard_default_project_name(Path::new("/tmp/My App")),
            ("my-app".to_string(), Some("My App".to_string()))
        );
        assert_eq!(
            wizard_default_project_name(Path::new("/")),
            ("my-project".to_string(), None)
        );
    }
}
