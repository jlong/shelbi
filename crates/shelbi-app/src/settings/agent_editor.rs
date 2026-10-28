//! The agent details editor's state, toolkit-free: one agent's draft over
//! `shelbi_state::agents_config`, the changes an editor makes to it, and
//! Save, Discard and the leave prompt. The desktop app's editor
//! (`dt-agent-details`) built it; it moved here (`tui-agent-editor`) so the
//! TUI's editor runs the same draft, mappings, file operations and guards.
//! It follows the workflow editor's ([`super::workflow_editor`]).
//!
//! The editor holds two models: the one loaded (the files as on disk) and
//! the draft. Every edit is a [`Change`] applied to the draft through the
//! model's API, so it is validated as it is made: a change the model refuses
//! leaves the draft as it was and shows the model's message at the field it
//! concerns ([`Field`]). Nothing is written until Save. Save first applies
//! what the text fields hold, then the typed name (a rename moves the
//! agent's directory, so it waits for Save), then writes the draft; Discard
//! puts the loaded model back. Back (or Esc), opening a file in the user's
//! editor, Duplicate, a workflow assignment and the footnotes' links ask to
//! save or discard unsaved edits first ([`Leave`]).
//!
//! The config the editor maps onto:
//!
//! - **Preferred runner** is `preferred_runner:`; Project default is no key
//!   (the project's `agents.<name>.runner` pin, else Claude Code, applies).
//! - **Model** and **Reasoning effort** are `runners.<runner>.model` and
//!   `.reasoning_effort` for the runner the agent runs on ([`runner`]: the
//!   project's pin, else the preferred one, else Claude Code, as dispatch
//!   resolves it). Use runner default is no key. A model is free text (a
//!   custom name is passed to the runner as typed); a generic runner takes
//!   neither.
//! - **Permissions** is `permissions_mode:`; Use project settings is no key.
//! - **Skills** are `skills/<name>/SKILL.md` (Add skill writes one with
//!   `name:` and `description:` frontmatter, or copies an existing skill
//!   folder or `.md` file whole); remove drops the skill's files.
//! - **Reset to default** puts the shipped files back into the draft, for
//!   Save or Discard like any other edit.
//!
//! What the project sets over these (a runner pin, a model or effort for
//! every agent on a runner, a permission ceiling) wins at dispatch; the
//! `*_in_effect` readers say what applies and where it comes from.
//!
//! [`runner`]: AgentEditorState::runner

use std::path::{Path, PathBuf};

use shelbi_core::{ReasoningEffort, RunnerKind};
use shelbi_state::agents_config::{
    runner_options, Agent, AgentError, AgentsConfig, Assignment, Provenance, Saved, Skill, SkillLayout,
};

use super::agents::{runner_label, sentence};
use super::LeaveGuard;

// ---------------------------------------------------------------------------
// Copy both clients show

pub const DESCRIPTION_PLACEHOLDER: &str = "What this agent does";
pub const INSTRUCTIONS_HELPER: &str = "Define how this agent approaches a task and when its work is complete.";
pub const SHARED_LINK: &str = "Shared project instructions";
pub const SKILLS_EMPTY: &str = "No skills added";
pub const SKILLS_EMPTY_HELP: &str = "Add reusable instructions for tasks this agent performs often.";
pub const SKILL_DESCRIPTION_PLACEHOLDER: &str = "When the agent should use this skill";
pub const IMPORT_HELP: &str = "Copy a skill folder (with its SKILL.md) or a .md file";
pub const RUNNER_HELPER: &str = "Choose the runner and defaults recommended for this role.";
pub const ASSIGNMENTS_HELPER: &str = "This agent runs when a task enters the assigned status.";
pub const NOT_ASSIGNED: &str = "Not assigned to any workflow status.";
pub const APPLY_NOTE: &str =
    "Changes apply to new agent sessions. Existing sessions keep their current instructions and settings.";
pub const PROJECT_DEFAULT: &str = "Project default";
pub const RUNNER_DEFAULT: &str = "Use runner default";
pub const PROJECT_SETTINGS: &str = "Use project settings";
pub const LEAVE_MESSAGE: &str = "Your edits to this agent haven’t been saved.";
pub const DELETE_MESSAGE: &str = "Its instructions and skills are deleted with it. This can’t be undone.";
pub const CONFLICT_MESSAGE: &str = "The agent’s files changed on disk since they were loaded, so the save was \
     refused. Reload them (your unsaved edits are discarded), or keep editing.";

/// The leave prompt's title.
pub fn leave_title(name: &str) -> String {
    format!("Save changes to {name}?")
}

/// The delete confirm's title.
pub fn delete_title(name: &str) -> String {
    format!("Delete {name}?")
}

/// The runners the Preferred runner picker lists after Project default.
pub fn runners() -> Vec<RunnerKind> {
    runner_options().into_iter().map(|r| r.kind).collect()
}

/// A reasoning effort's name, as its picker shows it.
pub fn effort_label(effort: ReasoningEffort) -> &'static str {
    match effort {
        ReasoningEffort::Low => "Low",
        ReasoningEffort::Medium => "Medium",
        ReasoningEffort::High => "High",
        ReasoningEffort::Max => "Max",
    }
}

/// A permission mode's name, as its picker shows it (the YAML token for one
/// Shelbi doesn't know).
pub fn permission_label(mode: &str) -> String {
    match mode {
        "plan" => "Plan".into(),
        "default" => "Default".into(),
        "auto" => "Auto".into(),
        "acceptEdits" => "Accept edits".into(),
        "bypassPermissions" => "Bypass permissions".into(),
        other => other.into(),
    }
}

/// The path typed into an Import field: `~/` is the home directory, and a
/// relative path is taken from `cwd` (a terminal client's working
/// directory).
pub fn import_path(text: &str, home: Option<&Path>, cwd: &Path) -> PathBuf {
    let text = text.trim();
    let expanded = match (text.strip_prefix("~/"), home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if text == "~" => home.map_or_else(|| PathBuf::from(text), Path::to_path_buf),
        _ => PathBuf::from(text),
    };
    if expanded.is_absolute() {
        expanded
    } else {
        cwd.join(expanded)
    }
}

/// Where an error shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field {
    Name,
    Description,
    Instructions,
    Runner,
    Model,
    Effort,
    Permissions,
    /// Skill `.0`'s row (its remove).
    Skill(String),
    /// The Add skill form (and Import).
    NewSkill,
    /// The titlebar: a save, reset, duplicate or delete that failed, or a
    /// file that couldn't open in the user's editor.
    Save,
}

/// One edit to the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Description(String),
    Instructions(String),
    PreferredRunner(Option<RunnerKind>),
    /// The model on the agent's runner (`None`: the runner's default). Any
    /// text is a model the runner is passed as typed.
    Model(Option<String>),
    Effort(Option<ReasoningEffort>),
    Permissions(Option<String>),
    AddSkill { name: String, description: String, body: String },
    /// Copy the skill folder (or `.md` file) at this path.
    ImportSkill(PathBuf),
    RemoveSkill(String),
    Reset,
}

impl Change {
    /// Where its error shows.
    pub fn field(&self) -> Field {
        match self {
            Change::Description(_) => Field::Description,
            Change::Instructions(_) => Field::Instructions,
            Change::PreferredRunner(_) => Field::Runner,
            Change::Model(_) => Field::Model,
            Change::Effort(_) => Field::Effort,
            Change::Permissions(_) => Field::Permissions,
            Change::AddSkill { .. } | Change::ImportSkill(_) => Field::NewSkill,
            Change::RemoveSkill(name) => Field::Skill(name.clone()),
            Change::Reset => Field::Save,
        }
    }
}

/// One of the agent's files to open in the user's editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentFile {
    /// `agent.yaml` (Edit YAML).
    Manifest,
    /// `instructions.md`.
    Instructions,
    /// Skill `.0`'s `SKILL.md` (or flat `.md`).
    Skill(String),
}

impl AgentFile {
    /// The file's name, for an error ("Couldn’t open agent.yaml").
    pub fn label(&self) -> String {
        match self {
            AgentFile::Manifest => "agent.yaml".into(),
            AgentFile::Instructions => "instructions.md".into(),
            AgentFile::Skill(name) => format!("{name}’s SKILL.md"),
        }
    }
}

/// What to do once unsaved edits are saved or discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    /// Back to Settings ▸ Agents.
    Back,
    /// Open one of the agent's files in the user's editor; the files are
    /// read again when it returns.
    Open(AgentFile),
    /// Duplicate the agent (as saved) and edit the copy.
    Duplicate,
    /// Open workflow `name` in the workflow editor (a Workflow assignments
    /// row), on status `status` when the client can.
    Workflow { name: String, status: Option<String> },
    /// Settings ▸ Workflows (Manage assignments in Workflows).
    Workflows,
    /// Back to Settings ▸ Agents, where the shared instructions are (the
    /// Instructions footnote's link).
    SharedInstructions,
}

/// Where a setting that applies comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// This agent's own `agent.yaml`.
    Agent,
    /// The project's settings, over the agent's.
    Project,
    /// Neither sets it: the default applies.
    Default,
}

/// One choice of a picker: its label, the change it makes, and whether
/// it's what the draft holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    /// A stable key (the YAML token, or `default`).
    pub key: String,
    pub label: String,
    pub change: Change,
    pub current: bool,
}

/// A skill as an Inspect view shows it: the draft's files, not the disk's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillView {
    pub name: String,
    pub description: Option<String>,
    pub layout: SkillLayout,
    /// Its `SKILL.md` (or flat `.md`), relative to the agent's directory.
    pub file: String,
    /// Every file it holds, relative to the agent's directory.
    pub files: Vec<String>,
    pub text: String,
    /// It isn't on disk yet (added since the load): Save writes it.
    pub unsaved: bool,
}

/// A save to run off the UI thread ([`run_save`]).
#[derive(Debug)]
pub struct SaveJob {
    config: AgentsConfig,
    name: String,
    rename: Option<String>,
}

/// What a save did: the model as saved and the agent's name, or the error
/// and where it shows.
pub type SaveOutcome = Result<(Saved, String), (Field, String)>;

/// Rename (when the name changed) and save, off the UI thread.
pub fn run_save(job: SaveJob) -> SaveOutcome {
    let mut config = job.config;
    let mut name = job.name;
    if let Some(new) = job.rename {
        config = config.rename_agent(&name, &new).map_err(|e| (Field::Name, message(&e)))?;
        name = new;
    }
    let saved = config.save().map_err(|e| (Field::Save, message(&e)))?;
    Ok((saved, name))
}

/// Copy agent `name` (as saved) as `copy` and save it, off the UI thread.
pub fn run_duplicate(base: AgentsConfig, name: &str, copy: &str) -> Result<AgentsConfig, String> {
    base.duplicate_agent(name, copy)
        .and_then(|c| c.save())
        .map(|s| s.config)
        .map_err(|e| message(&e))
}

/// Delete agent `name` and save, off the UI thread.
pub fn run_delete(base: AgentsConfig, name: &str) -> Result<(), String> {
    base.delete_agent(name).and_then(|c| c.save()).map(|_| ()).map_err(|e| message(&e))
}

fn message(e: &AgentError) -> String {
    match e {
        AgentError::Conflict(_) => format!("{} Discard to reload them.", sentence(&e.to_string())),
        e => sentence(&e.to_string()),
    }
}

/// The editor's state.
#[derive(Debug)]
pub struct AgentEditorState {
    /// The agent's name in the loaded model (its directory).
    name: String,
    /// What the Name field holds; applied at Save.
    typed_name: String,
    /// The files as loaded, and the draft over them.
    base: AgentsConfig,
    draft: AgentsConfig,
    /// Whether a save would write anything (kept, as working it out
    /// patches the files' text).
    draft_dirty: bool,
    errors: Vec<(Field, String)>,
    /// Comments the last save dropped.
    notice: Option<String>,
    /// The leave prompt while it asks, and where to go once the save it
    /// started lands.
    guard: LeaveGuard<Leave>,
    /// Delete is waiting to be confirmed.
    confirm_delete: bool,
    /// A save, a duplicate or a delete is running.
    busy: bool,
    /// The files changed on disk under the last save: Discard reloads.
    stale: bool,
}

impl AgentEditorState {
    /// Edit agent `name` of `config` (which has it).
    pub fn new(config: AgentsConfig, name: &str) -> AgentEditorState {
        AgentEditorState {
            name: name.to_string(),
            typed_name: name.to_string(),
            base: config.clone(),
            draft: config,
            draft_dirty: false,
            errors: Vec::new(),
            notice: None,
            guard: LeaveGuard::default(),
            confirm_delete: false,
            busy: false,
            stale: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn project(&self) -> &str {
        self.base.project()
    }

    /// The agent's `agent.yaml`, as saved.
    pub fn manifest_file(&self) -> PathBuf {
        self.file(&AgentFile::Manifest)
    }

    /// Where `file` is, as saved (a skill's under its saved layout, else
    /// where Add skill writes one).
    pub fn file(&self, file: &AgentFile) -> PathBuf {
        let dir = self.base.agent_dir(&self.name);
        match file {
            AgentFile::Manifest => dir.join("agent.yaml"),
            AgentFile::Instructions => dir.join("instructions.md"),
            AgentFile::Skill(skill) => {
                let saved = self.base.agent(&self.name).and_then(|a| a.skill_file(skill));
                dir.join(saved.unwrap_or_else(|| format!("skills/{skill}/SKILL.md")))
            }
        }
    }

    /// The draft of the agent.
    pub fn agent(&self) -> &Agent {
        self.draft.agent(&self.name).expect("the edited agent is in its model")
    }

    /// The titlebar's badge, as the Agents page shows it (the files as
    /// saved): Default for a shipped agent left as shipped, Customized for
    /// one edited, none for the user's own.
    pub fn provenance(&self) -> Provenance {
        self.base.provenance(&self.name)
    }

    /// The badge's text, if it has one.
    pub fn badge(&self) -> Option<&'static str> {
        match self.provenance() {
            Provenance::Shipped => Some("Default"),
            Provenance::Customized => Some("Customized"),
            Provenance::UserCreated => None,
        }
    }

    /// Whether there is anything to save.
    pub fn is_dirty(&self) -> bool {
        self.draft_dirty || self.typed_name.trim() != self.name
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// The last save was refused because the files changed on disk since
    /// they were loaded: Save stays refused until Discard reads them again.
    pub fn conflict(&self) -> bool {
        self.stale
    }

    pub fn error_at(&self, field: &Field) -> Option<&str> {
        self.errors.iter().find(|(f, _)| f == field).map(|(_, e)| e.as_str())
    }

    fn set_error(&mut self, field: Field, message: String) {
        self.errors.retain(|(f, _)| *f != field);
        self.errors.push((field, message));
    }

    fn clear_error(&mut self, field: &Field) {
        self.errors.retain(|(f, _)| f != field);
    }

    /// Drop the error showing at `field` (the Add skill form, cancelled).
    pub fn clear_error_at(&mut self, field: &Field) {
        self.clear_error(field);
    }

    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    pub fn typed_name(&self) -> &str {
        &self.typed_name
    }

    /// The Name field changed.
    pub fn type_name(&mut self, text: &str) {
        self.typed_name = text.to_string();
        self.clear_error(&Field::Name);
    }

    // -----------------------------------------------------------------------
    // Reading the draft

    /// The runner the agent runs on, and so the one Model and Reasoning
    /// effort apply to: the project's pin, else the preferred one, else
    /// Claude Code (dispatch's chain).
    pub fn runner(&self) -> RunnerKind {
        self.runner_in_effect().0
    }

    /// The runner the agent runs on and where that comes from.
    pub fn runner_in_effect(&self) -> (RunnerKind, Source) {
        if let Some(pin) = self.draft.project_runner_pin(&self.name) {
            return (pin, Source::Project);
        }
        match self.agent().preferred_runner() {
            Some(r) => (r, Source::Agent),
            None => (RunnerKind::Claude, Source::Default),
        }
    }

    /// The Model picker's choices after Use runner default: the runner's
    /// suggestions, and the draft's own model when it isn't one of them.
    pub fn models(&self) -> Vec<String> {
        let runner = self.runner();
        let mut models: Vec<String> = shelbi_core::model_suggestions(runner).iter().map(|m| m.to_string()).collect();
        if let Some(own) = self.model() {
            if !models.iter().any(|m| m == own) {
                models.push(own.to_string());
            }
        }
        models
    }

    pub fn model(&self) -> Option<&str> {
        self.agent().model(self.runner())
    }

    pub fn efforts(&self) -> &'static [ReasoningEffort] {
        shelbi_core::reasoning_efforts(self.runner())
    }

    pub fn effort(&self) -> Option<ReasoningEffort> {
        self.agent().reasoning_effort(self.runner())
    }

    /// The runner takes a model and an effort Shelbi can set (not generic).
    pub fn runner_takes_options(&self) -> bool {
        !shelbi_core::model_suggestions(self.runner()).is_empty()
    }

    /// Why Model and Reasoning effort can't be set: the runner takes
    /// neither.
    pub fn options_blocked(&self) -> Option<String> {
        (!self.runner_takes_options())
            .then(|| format!("{} runners take no model or reasoning effort.", runner_label(self.runner())))
    }

    /// The Permissions picker's choices after Use project settings: the
    /// known modes, and the draft's own when it isn't one.
    pub fn permission_modes(&self) -> Vec<String> {
        let mut modes: Vec<String> = shelbi_core::PERMISSION_MODES.iter().map(|m| m.to_string()).collect();
        if let Some(own) = self.agent().permissions_mode() {
            if !modes.iter().any(|m| m == own) {
                modes.push(own.to_string());
            }
        }
        modes
    }

    /// Preferred runner's choices: Project default, then every runner.
    pub fn runner_choices(&self) -> Vec<Choice> {
        let preferred = self.agent().preferred_runner();
        let mut out = vec![Choice {
            key: "default".into(),
            label: PROJECT_DEFAULT.into(),
            change: Change::PreferredRunner(None),
            current: preferred.is_none(),
        }];
        out.extend(runners().into_iter().map(|r| Choice {
            key: r.as_str().into(),
            label: runner_label(r).into(),
            change: Change::PreferredRunner(Some(r)),
            current: preferred == Some(r),
        }));
        out
    }

    /// Model's choices: Use runner default, then [`Self::models`]. Empty
    /// for a runner that takes none ([`Self::options_blocked`]).
    pub fn model_choices(&self) -> Vec<Choice> {
        if !self.runner_takes_options() {
            return Vec::new();
        }
        let model = self.model();
        let mut out = vec![Choice {
            key: "default".into(),
            label: RUNNER_DEFAULT.into(),
            change: Change::Model(None),
            current: model.is_none(),
        }];
        out.extend(self.models().into_iter().map(|m| Choice {
            key: m.clone(),
            label: m.clone(),
            current: model == Some(m.as_str()),
            change: Change::Model(Some(m)),
        }));
        out
    }

    /// Reasoning effort's choices: Use runner default, then the runner's.
    pub fn effort_choices(&self) -> Vec<Choice> {
        if !self.runner_takes_options() {
            return Vec::new();
        }
        let effort = self.effort();
        let mut out = vec![Choice {
            key: "default".into(),
            label: RUNNER_DEFAULT.into(),
            change: Change::Effort(None),
            current: effort.is_none(),
        }];
        out.extend(self.efforts().iter().map(|&e| Choice {
            key: e.as_str().into(),
            label: effort_label(e).into(),
            change: Change::Effort(Some(e)),
            current: effort == Some(e),
        }));
        out
    }

    /// Permissions' choices: Use project settings, then
    /// [`Self::permission_modes`].
    pub fn permission_choices(&self) -> Vec<Choice> {
        let mode = self.agent().permissions_mode();
        let mut out = vec![Choice {
            key: "default".into(),
            label: PROJECT_SETTINGS.into(),
            change: Change::Permissions(None),
            current: mode.is_none(),
        }];
        out.extend(self.permission_modes().into_iter().map(|m| Choice {
            key: m.clone(),
            label: permission_label(&m),
            current: mode == Some(m.as_str()),
            change: Change::Permissions(Some(m)),
        }));
        out
    }

    /// The model that applies on the agent's runner and where it comes
    /// from (the project's, for every agent on the runner, wins); `None`
    /// for the runner's own default, or a runner that takes none.
    pub fn model_in_effect(&self) -> Option<(String, Source)> {
        let runner = self.runner();
        if let Some(m) = self.draft.project_model(runner) {
            return Some((m.to_string(), Source::Project));
        }
        self.model().map(|m| (m.to_string(), Source::Agent))
    }

    pub fn effort_in_effect(&self) -> Option<(ReasoningEffort, Source)> {
        let runner = self.runner();
        if let Some(e) = self.draft.project_reasoning_effort(runner) {
            return Some((e, Source::Project));
        }
        self.effort().map(|e| (e, Source::Agent))
    }

    /// The permission mode a session launches with and where it comes
    /// from: the agent's request, lowered to the project's ceiling when it
    /// asks for more; the ceiling when it asks for nothing; `None` when
    /// neither is set (the user's own Claude settings apply).
    pub fn permissions_in_effect(&self) -> Option<(String, Source)> {
        let ceiling = self.draft.project_permissions_ceiling();
        let request = self.agent().permissions_mode();
        let mode = shelbi_core::clamp_permission_mode(ceiling, request)?;
        let lowered = request.is_some_and(|r| shelbi_core::clamp_permission_mode(None, Some(r)).as_deref() != Some(&mode));
        let source = if request.is_none() || lowered { Source::Project } else { Source::Agent };
        Some((mode, source))
    }

    /// The Preferred runner's line on what applies.
    pub fn runner_note(&self) -> String {
        let (runner, source) = self.runner_in_effect();
        let label = runner_label(runner);
        match source {
            Source::Project => format!(
                "The project runs {} on {label} (agents.{}.runner), over this preference.",
                self.name, self.name
            ),
            Source::Agent => format!("Runs on {label}."),
            Source::Default => format!("The project sets no runner for this agent: it runs on {label}."),
        }
    }

    /// Model's line on what applies.
    pub fn model_note(&self) -> String {
        if let Some(why) = self.options_blocked() {
            return why;
        }
        let runner = runner_label(self.runner());
        match self.model_in_effect() {
            Some((m, Source::Project)) => format!("The project sets {m} for every {runner} agent, over this one."),
            Some((m, _)) => format!("{runner} runs {m}."),
            None => format!("{runner} runs its own default model."),
        }
    }

    /// Reasoning effort's line on what applies.
    pub fn effort_note(&self) -> String {
        if let Some(why) = self.options_blocked() {
            return why;
        }
        let runner = runner_label(self.runner());
        match self.effort_in_effect() {
            Some((e, Source::Project)) => {
                format!("The project sets {} effort for every {runner} agent, over this one.", effort_label(e))
            }
            Some((e, _)) => format!("{runner} runs at {} effort.", effort_label(e)),
            None => format!("{runner} runs at its own default effort."),
        }
    }

    /// Permissions' line on what applies.
    pub fn permissions_note(&self) -> String {
        let requested = self.agent().permissions_mode().is_some();
        match self.permissions_in_effect() {
            Some((m, Source::Project)) if requested => format!(
                "The project allows at most {}, so sessions launch with {}.",
                permission_label(self.draft.project_permissions_ceiling().unwrap_or(&m)),
                permission_label(&m)
            ),
            Some((m, Source::Project)) => format!("Sessions launch with the project’s {}.", permission_label(&m)),
            Some((m, _)) => format!("Sessions launch with {}.", permission_label(&m)),
            None => "Neither the agent nor the project sets one: the runner’s own settings apply.".into(),
        }
    }

    /// The Runner preferences footnote (the desktop's, under the pickers).
    pub fn runner_footnote(&self) -> String {
        let runner = runner_label(self.runner());
        let lead = match self.runner_in_effect().1 {
            Source::Project => format!("The project pins this agent to {runner}. "),
            _ => String::new(),
        };
        if self.runner_takes_options() {
            format!("{lead}Model and effort apply to {runner}. Project settings can override these preferences.")
        } else {
            format!("{lead}{runner} runners take no model or reasoning effort. Project settings can override these preferences.")
        }
    }

    pub fn skills(&self) -> Vec<Skill> {
        self.agent().skills()
    }

    /// Skill `name` as the draft holds it.
    pub fn skill(&self, name: &str) -> Option<SkillView> {
        let agent = self.agent();
        let skill = agent.skills().into_iter().find(|s| s.name == name)?;
        let saved = self
            .base
            .agent(&self.name)
            .is_some_and(|a| a.skills().iter().any(|s| s.name == name));
        Some(SkillView {
            file: agent.skill_file(name)?,
            files: agent.skill_files(name),
            text: agent.skill_text(name).unwrap_or_default(),
            name: skill.name,
            description: skill.description,
            layout: skill.layout,
            unsaved: !saved,
        })
    }

    /// The workflow statuses that run the agent (as loaded: they follow
    /// the workflows, which this editor doesn't change).
    pub fn assignments(&self) -> Vec<Assignment> {
        self.base.assignments(&self.name)
    }

    /// Why the agent can't be renamed (shipped, or named by a workflow
    /// status or the project's runner pin).
    pub fn rename_blocked(&self) -> Option<String> {
        self.base.check_rename(&self.name).err().map(|e| sentence(&e.to_string()))
    }

    /// Why the agent can't be deleted.
    pub fn delete_blocked(&self) -> Option<String> {
        self.base.check_delete(&self.name).err().map(|e| sentence(&e.to_string()))
    }

    /// Why Reset to default is off: Shelbi doesn't ship the agent, or the
    /// draft already is the shipped default.
    pub fn reset_blocked(&self) -> Option<String> {
        if !self.draft.can_reset(&self.name) {
            Some("Only the agents Shelbi ships have a default to reset to.".into())
        } else if self.draft.provenance(&self.name) == Provenance::Shipped {
            Some("Already the shipped default.".into())
        } else {
            None
        }
    }

    /// The name a duplicate gets: `<name>-copy`, then `-copy-2`, … .
    pub fn duplicate_name(&self) -> String {
        let base = format!("{}-copy", self.name);
        let taken = |name: &str| self.base.agent(name).is_some() || self.base.unreadable().iter().any(|u| u.name == name);
        let mut name = base.clone();
        let mut n = 2;
        while taken(&name) {
            name = format!("{base}-{n}");
            n += 1;
        }
        name
    }

    // -----------------------------------------------------------------------
    // Editing

    /// Make `change` to the draft. A refusal leaves the draft as it was
    /// and shows at the change's field. Returns whether it was made.
    pub fn change(&mut self, change: Change) -> bool {
        let field = change.field();
        self.clear_error(&field);
        self.notice = None;
        match self.apply(&change) {
            Ok(next) => {
                self.draft = next;
                self.draft_dirty = self.draft.is_dirty();
                true
            }
            Err(e) => {
                self.set_error(field, e);
                false
            }
        }
    }

    fn apply(&self, change: &Change) -> Result<AgentsConfig, String> {
        let (c, a) = (&self.draft, self.name.as_str());
        let runner = self.runner();
        let made = match change {
            Change::Description(text) => c.set_description(a, Some(text)),
            Change::Instructions(text) => c.set_instructions(a, text),
            Change::PreferredRunner(r) => c.set_preferred_runner(a, *r),
            Change::Model(m) => c.set_model(a, runner, m.as_deref()),
            Change::Effort(e) => c.set_reasoning_effort(a, runner, *e),
            Change::Permissions(m) => c.set_permissions_mode(a, m.as_deref()),
            Change::AddSkill { name, description, body } => c.add_skill(a, name.trim(), description, body),
            Change::ImportSkill(path) => c.import_skill(a, path),
            Change::RemoveSkill(name) => c.remove_skill(a, name),
            Change::Reset => c.reset_to_default(a),
        };
        made.map_err(|e| message(&e))
    }

    /// Whether a text field's change would change the draft.
    pub fn differs(&self, change: &Change) -> bool {
        let a = self.agent();
        match change {
            Change::Description(text) => a.description().unwrap_or("") != text.trim(),
            Change::Instructions(text) => a.instructions() != text,
            _ => true,
        }
    }

    /// Make each text field's change that differs from the draft (a text
    /// field changed; none does when the client itself filled the field).
    pub fn type_texts(&mut self, texts: Vec<Change>) {
        for change in texts {
            if self.differs(&change) {
                self.change(change);
            }
        }
    }

    // -----------------------------------------------------------------------
    // Save, Discard, leaving

    /// Start a save: apply `texts` (what the text fields hold, each made
    /// only if it differs from the draft), then hand back the save to run.
    /// `None` when there's nothing to save, one is running, or a field's
    /// text was refused (its error shows).
    pub fn begin_save(&mut self, texts: Vec<Change>) -> Option<SaveJob> {
        if self.busy {
            return None;
        }
        self.clear_error(&Field::Save);
        let mut refused = false;
        for change in texts {
            if self.differs(&change) && !self.change(change) {
                refused = true;
            }
        }
        let typed = self.typed_name.trim().to_string();
        if refused || !self.is_dirty() {
            self.guard.forget_save();
            return None;
        }
        self.busy = true;
        Some(SaveJob {
            config: self.draft.clone(),
            name: self.name.clone(),
            rename: (typed != self.name).then_some(typed),
        })
    }

    /// A save landed: take its model (the draft and the loaded model are
    /// now the files), or show its error. Hands back what to do next, if
    /// the leave prompt's Save started it.
    pub fn saved(&mut self, outcome: SaveOutcome) -> Option<Leave> {
        self.busy = false;
        match outcome {
            Ok((saved, name)) => {
                let dropped: Vec<String> = saved
                    .dropped_comments
                    .iter()
                    .flat_map(|(path, lines)| {
                        let file = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
                        lines.iter().map(move |l| format!("{file}: {}", l.trim()))
                    })
                    .collect();
                let then = self.guard.saved(true);
                *self = AgentEditorState::new(saved.config, &name);
                if !dropped.is_empty() {
                    self.notice = Some(format!("The save dropped these comments: {}", dropped.join("; ")));
                }
                then
            }
            Err((field, e)) => {
                self.stale = e.starts_with("Changed on disk");
                self.set_error(field, e);
                self.guard.forget_save();
                None
            }
        }
    }

    /// Put the loaded model back. Returns whether the files must be read
    /// again (they changed under the last save): [`Self::reloaded`].
    pub fn discard(&mut self) -> bool {
        let stale = self.stale;
        *self = AgentEditorState::new(self.base.clone(), &self.name.clone());
        stale
    }

    /// The files, read again after a discard. `false` when the agent is
    /// gone from them.
    pub fn reloaded(&mut self, config: AgentsConfig) -> bool {
        if config.agent(&self.name).is_none() {
            return false;
        }
        *self = AgentEditorState::new(config, &self.name.clone());
        true
    }

    /// Ask to leave for `then`: `Some(then)` to go now, or `None` while the
    /// prompt asks to save or discard the unsaved edits first.
    pub fn leave(&mut self, then: Leave) -> Option<Leave> {
        let dirty = self.is_dirty();
        self.guard.leave(then, dirty)
    }

    pub fn prompt(&self) -> Option<&Leave> {
        self.guard.prompt()
    }

    pub fn cancel_prompt(&mut self) {
        self.guard.cancel();
    }

    /// The prompt's Discard: drop the edits and go.
    pub fn discard_and_leave(&mut self) -> Option<Leave> {
        let then = self.guard.discard();
        self.discard();
        then
    }

    /// The prompt's Save: save, then go once it lands ([`Self::saved`]).
    pub fn save_and_leave(&mut self, texts: Vec<Change>) -> Option<SaveJob> {
        self.guard.save();
        self.begin_save(texts)
    }

    /// Ask to confirm Delete, unless its guard refuses it.
    pub fn ask_delete(&mut self) {
        if self.delete_blocked().is_none() {
            self.confirm_delete = true;
        }
    }

    pub fn confirming_delete(&self) -> bool {
        self.confirm_delete
    }

    pub fn cancel_delete(&mut self) {
        self.confirm_delete = false;
    }

    /// Start a duplicate or a delete (on the model as saved). `None` while
    /// one is running.
    pub fn begin_job(&mut self) -> Option<AgentsConfig> {
        if self.busy {
            return None;
        }
        self.confirm_delete = false;
        self.clear_error(&Field::Save);
        self.busy = true;
        Some(self.base.clone())
    }

    /// A duplicate or a delete failed.
    pub fn job_failed(&mut self, e: String) {
        self.busy = false;
        self.set_error(Field::Save, e);
    }

    /// A file couldn't open in the user's editor.
    pub fn open_failed(&mut self, e: String) {
        self.set_error(Field::Save, e);
    }
}

#[cfg(test)]
mod tests;
