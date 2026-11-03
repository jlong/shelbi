//! The Orchestrator view's onboarding, toolkit-free: the three steps a
//! project whose orchestrator was never set up shows in place of the
//! orchestrator's terminal, and the saves they make. Both clients drive this
//! one state and only render it ([`OnboardingState::model`]).
//!
//! 1. **Intro** ("Your project’s orchestrator"): Get started.
//! 2. **Harness** ("Choose your Orchestrator"): the runner the orchestrator
//!    launches on and its default model. Continue saves them: the project
//!    YAML's `orchestrator.runner` points at a runner of the chosen kind
//!    ([`shelbi_state::orchestrator_setup::set_orchestrator_runner`],
//!    comment-preserving, a custom runner of that kind kept as it is), and
//!    the orchestrator's `agent.yaml` gets `preferred_runner` and
//!    `runners.<kind>.model` through the agent editor's comment-preserving
//!    [`AgentsConfig`] save. The orchestrator's launch reads the model from
//!    there ([`shelbi_core::orchestrator_model`]).
//! 3. **Instructions** ("Make it yours"): `agents/orchestrator/instructions.md`
//!    in a Markdown editor. Back returns to step 2 keeping both steps' edits.
//!    Launch Orchestrator saves the file (only when it changed), marks the
//!    project onboarded ([`shelbi_state::mark_orchestrator_onboarded`]) and
//!    hands back to the client, which starts the orchestrator through its
//!    usual launch path and shows the terminal.
//!
//! Whether a project shows this at all is [`needs_onboarding`] (the skip
//! rule is [`shelbi_state::orchestrator_onboarded`]'s).
//!
//! The reads and saves block, so each is a job a client runs off its UI
//! thread: [`load`], [`run_continue`] and [`run_launch`], each handed back
//! with the matching `*ed` method.

use shelbi_core::RunnerKind;
use shelbi_state::agents_config::{runner_options, AgentError, AgentsConfig};
use shelbi_state::orchestrator_setup::{self, RunnerSetupError};

use crate::settings::agent_editor::RUNNER_DEFAULT;
use crate::settings::agents::{runner_label, sentence};
use crate::view::{HarnessOption, ModelOption, OnboardingModel, OnboardingStep};

// ---------------------------------------------------------------------------
// Copy both clients show (the Figma frames 2709:10719, 2709:11068, 2709:11492)

pub const INTRO_TITLE: &str = "Your project’s orchestrator";
pub const INTRO_BODY: &str = "Every project in Shelbi has an orchestrator agent to manage the work. \
     Tell the Orchestrator what you want, it will take care of creating issues and coordinating work \
     with other agents.";
pub const GET_STARTED: &str = "Get started";

pub const HARNESS_TITLE: &str = "Choose your Orchestrator";
pub const HARNESS_BODY: &str = "Choose the provider and model that will plan and coordinate work in this project.";
pub const HARNESS_LABEL: &str = "Harness";
pub const MODEL_LABEL: &str = "Default model";
/// The helper under both pickers.
pub const FIELD_HELP: &str = "Used for new Orchestrator sessions in Shelbi.";
pub const CONTINUE: &str = "Continue";

pub const INSTRUCTIONS_TITLE: &str = "Make it yours";
pub const INSTRUCTIONS_BODY: &str = "Edit the agent instructions below to match how you like to work.";
pub const INSTRUCTIONS_LABEL: &str = "Orchestrator instructions";
pub const INSTRUCTIONS_FORMAT: &str = "Markdown";
pub const BACK: &str = "Back";
pub const LAUNCH: &str = "Launch Orchestrator";

/// Shown in step 2 and 3 while the agents are still being read.
pub const LOADING: &str = "Loading…";

/// The agent the onboarding sets up.
pub const AGENT: &str = shelbi_state::ORCHESTRATOR_AGENT;

/// Whether `project` shows the onboarding in its Orchestrator view (and so
/// doesn't start its orchestrator on its own). Reads `state.json` and two
/// paths: cheap, but disk.
pub fn needs_onboarding(project: &str) -> bool {
    !shelbi_state::orchestrator_onboarded(project)
}

/// The company behind a runner, shown beside its name.
pub fn provider(kind: RunnerKind) -> Option<&'static str> {
    match kind {
        RunnerKind::Claude => Some("Anthropic"),
        RunnerKind::Codex => Some("OpenAI"),
        RunnerKind::Generic => None,
    }
}

/// A model's name as the picker shows it: the suggested aliases by their
/// product names, anything else as typed.
pub fn model_label(model: &str) -> String {
    match model {
        "opus" => "Opus".into(),
        "sonnet" => "Sonnet".into(),
        "haiku" => "Haiku".into(),
        "gpt-5" => "GPT-5".into(),
        "gpt-5-codex" => "GPT-5 Codex".into(),
        other => other.into(),
    }
}

/// What [`load`] reads: the project's agents and the runner the
/// orchestrator launches on.
#[derive(Debug, Clone)]
pub struct Setup {
    config: AgentsConfig,
    /// The kind of the runner `orchestrator.runner` names.
    runner: RunnerKind,
    /// The kinds some `agent_runners` entry runs.
    declared: Vec<RunnerKind>,
}

impl Setup {
    /// A setup over `config` (tests and callers that have it loaded).
    pub fn new(config: AgentsConfig, runner: RunnerKind, declared: Vec<RunnerKind>) -> Setup {
        Setup { config, runner, declared }
    }
}

/// Read what the onboarding edits. Blocking: run it off the UI thread.
pub fn load(project: &str) -> Result<Setup, String> {
    let config = AgentsConfig::load(project).map_err(|e| sentence(&e.to_string()))?;
    if config.agent(AGENT).is_none() {
        return Err(format!("This project has no {AGENT} agent."));
    }
    let loaded = shelbi_state::load_project(project).map_err(|e| sentence(&e.to_string()))?;
    let declared = [RunnerKind::Claude, RunnerKind::Codex, RunnerKind::Generic]
        .into_iter()
        .filter(|k| orchestrator_setup::runner_declared(&loaded, *k))
        .collect();
    Ok(Setup::new(config, orchestrator_setup::orchestrator_runner_kind(&loaded), declared))
}

/// Step 2's save, to run off the UI thread ([`run_continue`]).
#[derive(Debug)]
pub struct ContinueJob {
    project: String,
    /// The agents with the choices made; saved when dirty.
    config: AgentsConfig,
    /// The runner `orchestrator.runner` should run on.
    runner: RunnerKind,
}

/// What step 2's save did: the agents as now on disk, or the error.
pub type ContinueOutcome = Result<AgentsConfig, String>;

/// Point `orchestrator.runner` at the chosen kind, then save the
/// orchestrator's `agent.yaml`. Blocking.
pub fn run_continue(job: ContinueJob) -> ContinueOutcome {
    orchestrator_setup::set_orchestrator_runner(&job.project, job.runner).map_err(|e| match e {
        RunnerSetupError::NotDeclared(_) => e.to_string(),
        RunnerSetupError::File { .. } => format!("Couldn’t save the project settings: {e}"),
    })?;
    if job.config.is_dirty() {
        job.config.save().map(|s| s.config).map_err(|e| agent_error(&e))
    } else {
        Ok(job.config)
    }
}

/// Step 3's save, to run off the UI thread ([`run_launch`]).
#[derive(Debug)]
pub struct LaunchJob {
    project: String,
    /// The agents with the instructions draft, when it changed.
    config: Option<AgentsConfig>,
}

/// What Launch Orchestrator's save did.
pub type LaunchOutcome = Result<(), String>;

/// Save the instructions (when they changed) and mark the project
/// onboarded. Blocking. Starting the orchestrator is the client's.
pub fn run_launch(job: LaunchJob) -> LaunchOutcome {
    if let Some(config) = job.config {
        config.save().map_err(|e| agent_error(&e))?;
    }
    shelbi_state::mark_orchestrator_onboarded(&job.project)
        .map_err(|e| format!("Couldn’t record the setup: {}", sentence(&e.to_string())))
}

fn agent_error(e: &AgentError) -> String {
    match e {
        AgentError::Conflict(_) => {
            "The orchestrator’s files changed on disk since they were loaded, so nothing was saved.".into()
        }
        e => sentence(&e.to_string()),
    }
}

/// The onboarding's state.
#[derive(Debug)]
pub struct OnboardingState {
    project: String,
    step: OnboardingStep,
    setup: Option<Setup>,
    load_error: Option<String>,
    /// Step 2's choices.
    runner: RunnerKind,
    model: Option<String>,
    /// Step 3's draft, and the text as loaded (or last saved).
    instructions: String,
    saved_instructions: String,
    error: Option<String>,
    busy: bool,
    finished: bool,
}

impl OnboardingState {
    /// `project`'s onboarding, on step 1, its agents not read yet
    /// ([`Self::loaded`]).
    pub fn new(project: &str) -> OnboardingState {
        OnboardingState {
            project: project.to_string(),
            step: OnboardingStep::Intro,
            setup: None,
            load_error: None,
            runner: RunnerKind::Claude,
            model: None,
            instructions: String::new(),
            saved_instructions: String::new(),
            error: None,
            busy: false,
            finished: false,
        }
    }

    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn step(&self) -> OnboardingStep {
        self.step
    }

    pub fn is_loaded(&self) -> bool {
        self.setup.is_some()
    }

    /// A save is running.
    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Launch Orchestrator's save landed: the client starts the
    /// orchestrator and drops this.
    pub fn finished(&self) -> bool {
        self.finished
    }

    /// [`load`]'s result. Step 2 starts on the runner the orchestrator
    /// launches on and its model; step 3 on the instructions as on disk.
    pub fn loaded(&mut self, setup: Result<Setup, String>) {
        match setup {
            Ok(setup) => {
                let text = setup.config.agent(AGENT).map(|a| a.instructions().to_string()).unwrap_or_default();
                self.runner = setup.runner;
                self.saved_instructions = text.clone();
                self.instructions = text;
                self.setup = Some(setup);
                self.model = self.effective_model(self.runner);
                self.load_error = None;
            }
            Err(e) => self.load_error = Some(format!("Couldn’t read the orchestrator’s settings: {e}")),
        }
    }

    // -----------------------------------------------------------------------
    // Steps

    /// Get started: on to step 2.
    pub fn get_started(&mut self) {
        if self.step == OnboardingStep::Intro {
            self.step = OnboardingStep::Harness;
            self.error = None;
        }
    }

    /// Back a step, keeping every edit (Esc on step 2 too, back to the
    /// intro). Not while a save runs.
    pub fn back(&mut self) {
        if self.busy {
            return;
        }
        self.step = match self.step {
            OnboardingStep::Intro | OnboardingStep::Harness => OnboardingStep::Intro,
            OnboardingStep::Instructions => OnboardingStep::Harness,
        };
        self.error = None;
    }

    // -----------------------------------------------------------------------
    // Step 2

    /// The runner chosen.
    pub fn runner(&self) -> RunnerKind {
        self.runner
    }

    /// The model chosen (`None`: the runner's default).
    pub fn model_choice(&self) -> Option<&str> {
        self.model.as_deref()
    }

    /// Choose the runner. Its model starts as the one it already uses.
    pub fn select_runner(&mut self, kind: RunnerKind) {
        if self.busy || kind == self.runner {
            return;
        }
        self.runner = kind;
        self.model = self.effective_model(kind);
        self.error = None;
    }

    /// Choose the model: a suggestion, any other text (trimmed; blank is
    /// the runner's default), or `None`.
    pub fn select_model(&mut self, model: Option<&str>) {
        if self.busy || !self.takes_model() {
            return;
        }
        self.model = model.map(str::trim).filter(|m| !m.is_empty()).map(str::to_string);
        self.error = None;
    }

    /// The chosen runner takes a model Shelbi can set.
    pub fn takes_model(&self) -> bool {
        !shelbi_core::model_suggestions(self.runner).is_empty()
    }

    /// The model `kind` launches with now: the project's
    /// `runners.<kind>.model`, else the orchestrator's own.
    fn effective_model(&self, kind: RunnerKind) -> Option<String> {
        let setup = self.setup.as_ref()?;
        setup
            .config
            .project_model(kind)
            .or_else(|| setup.config.agent(AGENT)?.model(kind))
            .map(str::to_string)
    }

    /// Continue: the save to run ([`run_continue`]), or `None` (already
    /// saving, not loaded, or the choice is refused, with the reason in the
    /// step's error).
    pub fn begin_continue(&mut self) -> Option<ContinueJob> {
        if self.busy || self.step != OnboardingStep::Harness {
            return None;
        }
        let setup = self.setup.as_ref()?;
        if !setup.declared.contains(&self.runner) && self.runner == RunnerKind::Generic {
            self.error = Some(RunnerSetupError::NotDeclared(self.runner).to_string());
            return None;
        }
        let mut config = setup.config.clone();
        let agent = config.agent(AGENT)?;
        if agent.preferred_runner() != Some(self.runner) {
            config = match config.set_preferred_runner(AGENT, Some(self.runner)) {
                Ok(c) => c,
                Err(e) => return self.refuse(&e),
            };
        }
        // Only a model that differs from what already applies is written: a
        // kept default writes nothing.
        if self.takes_model() && self.model != self.effective_model(self.runner) {
            config = match config.set_model(AGENT, self.runner, self.model.as_deref()) {
                Ok(c) => c,
                Err(e) => return self.refuse(&e),
            };
        }
        self.busy = true;
        self.error = None;
        Some(ContinueJob { project: self.project.clone(), config, runner: self.runner })
    }

    fn refuse<T>(&mut self, e: &AgentError) -> Option<T> {
        self.error = Some(agent_error(e));
        None
    }

    /// [`run_continue`]'s result: on to step 3, or the error.
    pub fn continued(&mut self, outcome: ContinueOutcome) {
        self.busy = false;
        match outcome {
            Ok(config) => {
                if let Some(setup) = self.setup.as_mut() {
                    setup.config = config;
                    setup.runner = self.runner;
                    if !setup.declared.contains(&self.runner) {
                        setup.declared.push(self.runner);
                    }
                }
                self.step = OnboardingStep::Instructions;
                self.error = None;
            }
            Err(e) => self.error = Some(e),
        }
    }

    // -----------------------------------------------------------------------
    // Step 3

    pub fn instructions(&self) -> &str {
        &self.instructions
    }

    /// The instructions editor changed.
    pub fn type_instructions(&mut self, text: &str) {
        if self.busy {
            return;
        }
        self.instructions = text.to_string();
        self.error = None;
    }

    /// The draft differs from the file.
    pub fn instructions_changed(&self) -> bool {
        self.instructions != self.saved_instructions
    }

    /// Launch Orchestrator: the save to run ([`run_launch`]), or `None`.
    pub fn begin_launch(&mut self) -> Option<LaunchJob> {
        if self.busy || self.step != OnboardingStep::Instructions {
            return None;
        }
        let setup = self.setup.as_ref()?;
        let config = if self.instructions_changed() {
            match setup.config.set_instructions(AGENT, &self.instructions) {
                Ok(c) => Some(c),
                Err(e) => return self.refuse(&e),
            }
        } else {
            None
        };
        self.busy = true;
        self.error = None;
        Some(LaunchJob { project: self.project.clone(), config })
    }

    /// [`run_launch`]'s result. `true`: saved and marked, so the client
    /// starts the orchestrator now.
    pub fn launched(&mut self, outcome: LaunchOutcome) -> bool {
        self.busy = false;
        match outcome {
            Ok(()) => {
                self.saved_instructions = self.instructions.clone();
                self.finished = true;
                true
            }
            Err(e) => {
                self.error = Some(e);
                false
            }
        }
    }

    // -----------------------------------------------------------------------
    // What a client draws

    /// The view model for the current state.
    pub fn model(&self) -> OnboardingModel {
        let harnesses = runner_options()
            .into_iter()
            .map(|r| HarnessOption {
                kind: r.kind,
                label: runner_label(r.kind).into(),
                provider: provider(r.kind).map(str::to_string),
                selected: r.kind == self.runner,
            })
            .collect();
        let takes_model = self.takes_model();
        let models = if takes_model { self.model_options() } else { Vec::new() };
        let model_label = self.model.as_deref().map_or_else(|| RUNNER_DEFAULT.to_string(), model_label);
        OnboardingModel {
            step: self.step,
            loading: self.setup.is_none(),
            harnesses,
            models,
            model_label,
            model_enabled: takes_model && self.setup.is_some(),
            model_note: self.model_note(),
            instructions: self.instructions.clone(),
            error: self.load_error.clone().or_else(|| self.error.clone()),
            busy: self.busy,
        }
    }

    /// Default model's choices: the runner's default, its suggestions, and
    /// the chosen model when it's none of them.
    fn model_options(&self) -> Vec<ModelOption> {
        let mut out = vec![ModelOption {
            model: None,
            label: RUNNER_DEFAULT.into(),
            selected: self.model.is_none(),
        }];
        let mut models: Vec<String> =
            shelbi_core::model_suggestions(self.runner).iter().map(|m| m.to_string()).collect();
        if let Some(own) = &self.model {
            if !models.contains(own) {
                models.push(own.clone());
            }
        }
        out.extend(models.into_iter().map(|m| ModelOption {
            label: model_label(&m),
            selected: self.model.as_deref() == Some(m.as_str()),
            model: Some(m),
        }));
        out
    }

    fn model_note(&self) -> Option<String> {
        if !self.takes_model() {
            return Some(format!("{} runners take no model.", runner_label(self.runner)));
        }
        let project = self.setup.as_ref()?.config.project_model(self.runner)?;
        (self.model.as_deref() != Some(project)).then(|| {
            format!(
                "The project’s settings set {} for every {} agent, over this choice.",
                model_label(project),
                runner_label(self.runner)
            )
        })
    }
}

#[cfg(test)]
mod tests;
