//! The agent details editor's state, toolkit-free (`dt-agent-details`): one
//! agent's draft over `shelbi_state::agents_config`, the changes the editor
//! makes to it, and Save, Discard and the leave prompt. It follows the
//! workflow editor's ([`super::workflow_editor_state`]).
//!
//! The editor holds two models: the one loaded (the files as on disk) and
//! the draft. Every edit is a [`Change`] applied to the draft through the
//! model's API, so it is validated as it is made: a change the model refuses
//! leaves the draft as it was and shows the model's message at the field it
//! concerns ([`Field`]). Nothing is written until Save. Save first applies
//! what the text fields hold, then the typed name (a rename moves the
//! agent's directory, so it waits for Save), then writes the draft; Discard
//! puts the loaded model back. Back (or Esc), Edit YAML, Duplicate, a
//! workflow assignment and the footnotes' links ask to save or discard
//! unsaved edits first ([`Leave`]).
//!
//! The config the editor maps onto:
//!
//! - **Preferred runner** is `preferred_runner:`; Project default is no key
//!   (the project's `agents.<name>.runner` pin, else Claude Code, applies).
//! - **Model** and **Reasoning effort** are `runners.<runner>.model` and
//!   `.reasoning_effort` for the runner the agent runs on ([`runner`]: the
//!   preferred one, else Claude Code, as dispatch resolves it). Use runner
//!   default is no key. A generic runner takes neither.
//! - **Permissions** is `permissions_mode:`; Use project settings is no key.
//! - **Skills** are `skills/<name>/SKILL.md` (Add skill writes one with
//!   `name:` and `description:` frontmatter, or copies an existing skill
//!   folder or `.md` file); remove drops the skill's files.
//! - **Reset to default** puts the shipped files back into the draft, for
//!   Save or Discard like any other edit.
//!
//! [`runner`]: AgentEditorState::runner

use std::path::PathBuf;

use shelbi_core::{ReasoningEffort, RunnerKind};
use shelbi_state::agents_config::{
    runner_options, AgentError, Agent, AgentsConfig, Assignment, Provenance, Saved, Skill,
};

use super::workflows_state::sentence;

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
    /// The Add skill form.
    NewSkill,
    /// The titlebar: a save, reset, duplicate or delete that failed, or an
    /// Edit YAML that couldn't open.
    Save,
}

/// One edit to the draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Description(String),
    Instructions(String),
    PreferredRunner(Option<RunnerKind>),
    /// The model on the agent's runner (`None`: the runner's default).
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

/// What to do once unsaved edits are saved or discarded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    /// Back to Settings ▸ Agents.
    Back,
    /// Open the agent's `agent.yaml` in the user's editor.
    EditYaml,
    /// Duplicate the agent (as saved) and edit the copy.
    Duplicate,
    /// Open workflow `.0` in the workflow editor (a Workflow assignments
    /// row).
    Workflow(String),
    /// Settings ▸ Workflows (Manage assignments in Workflows).
    Workflows,
    /// Back to Settings ▸ Agents, where the shared instructions are (the
    /// Instructions footnote's link).
    SharedInstructions,
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
    /// The leave prompt, while it asks.
    prompt: Option<Leave>,
    /// What to do once the save in flight lands.
    after_save: Option<Leave>,
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
            prompt: None,
            after_save: None,
            confirm_delete: false,
            busy: false,
            stale: false,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The agent's `agent.yaml`, as saved.
    pub fn manifest_file(&self) -> PathBuf {
        self.base.agent_dir(&self.name).join("agent.yaml")
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

    /// Whether there is anything to save.
    pub fn is_dirty(&self) -> bool {
        self.draft_dirty || self.typed_name.trim() != self.name
    }

    pub fn busy(&self) -> bool {
        self.busy
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

    /// The runner Model and Reasoning effort apply to: the preferred one,
    /// else Claude Code (dispatch's default).
    pub fn runner(&self) -> RunnerKind {
        self.agent().preferred_runner().unwrap_or(RunnerKind::Claude)
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

    pub fn skills(&self) -> Vec<Skill> {
        self.agent().skills()
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
            self.after_save = None;
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
                let then = self.after_save.take();
                *self = AgentEditorState::new(saved.config, &name);
                if !dropped.is_empty() {
                    self.notice = Some(format!("The save dropped these comments: {}", dropped.join("; ")));
                }
                then
            }
            Err((field, e)) => {
                self.stale = e.starts_with("Changed on disk");
                self.set_error(field, e);
                self.after_save = None;
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
        if self.is_dirty() {
            self.prompt = Some(then);
            None
        } else {
            Some(then)
        }
    }

    pub fn prompt(&self) -> Option<&Leave> {
        self.prompt.as_ref()
    }

    pub fn cancel_prompt(&mut self) {
        self.prompt = None;
    }

    /// The prompt's Discard: drop the edits and go.
    pub fn discard_and_leave(&mut self) -> Option<Leave> {
        let then = self.prompt.take();
        self.discard();
        then
    }

    /// The prompt's Save: save, then go once it lands ([`Self::saved`]).
    pub fn save_and_leave(&mut self, texts: Vec<Change>) -> Option<SaveJob> {
        self.after_save = self.prompt.take();
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

    /// Edit YAML couldn't open the file.
    pub fn open_failed(&mut self, e: String) {
        self.set_error(Field::Save, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::agents_state::tests::home;
    use crate::views::workflows_state::tests::PROJECT;

    fn editor(name: &str) -> AgentEditorState {
        AgentEditorState::new(AgentsConfig::load(PROJECT).unwrap(), name)
    }

    /// Save as the view does, here rather than off the UI thread.
    fn save(state: &mut AgentEditorState, texts: Vec<Change>) -> Option<Leave> {
        let job = state.begin_save(texts)?;
        state.saved(run_save(job))
    }

    fn on_disk(name: &str) -> Agent {
        AgentsConfig::load(PROJECT).unwrap().agent(name).unwrap().clone()
    }

    fn agent_file(home: &crate::views::workflows_state::tests::Home, agent: &str, file: &str) -> PathBuf {
        home.root.join("projects").join(PROJECT).join("agents").join(agent).join(file)
    }

    #[test]
    fn edits_stay_in_the_draft_until_save_and_discard_reverts_them() {
        let home = home("ed-draft");
        let yaml = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
        let mut state = editor("developer");
        assert_eq!(state.provenance(), Provenance::Shipped);
        assert!(!state.is_dirty());

        assert!(state.change(Change::Description("Ships the feature.".into())));
        assert!(state.is_dirty());
        assert_eq!(std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap(), yaml);

        assert!(!state.discard());
        assert!(!state.is_dirty());
        assert_eq!(state.agent().description(), on_disk("developer").description());

        // Typed into the field, then saved: written, comments kept.
        save(&mut state, vec![Change::Description("Ships the feature.".into())]);
        assert!(!state.is_dirty());
        assert_eq!(on_disk("developer").description(), Some("Ships the feature."));
        assert_eq!(state.provenance(), Provenance::Customized, "the badge follows the save");
        let text = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
        let comments = |t: &str| t.lines().filter(|l| l.trim_start().starts_with('#')).count();
        assert_eq!(comments(&text), comments(&yaml), "{text}");
    }

    #[test]
    fn instructions_save_as_markdown() {
        let home = home("ed-instructions");
        let mut state = editor("developer");
        let text = "# Developer\n\nShip it.\n";
        save(&mut state, vec![Change::Instructions(text.into())]);
        assert_eq!(std::fs::read_to_string(agent_file(&home, "developer", "instructions.md")).unwrap(), text);
        // The same text again is nothing to save.
        assert!(state.begin_save(vec![Change::Instructions(text.into())]).is_none());
    }

    #[test]
    fn runner_preferences_map_default_choices_to_absent_keys() {
        let home = home("ed-runner");
        let mut state = editor("developer");
        assert_eq!(state.runner(), RunnerKind::Claude);
        assert!(state.models().iter().any(|m| m == "opus"));

        assert!(state.change(Change::Model(Some("opus".into()))));
        assert!(state.change(Change::Effort(Some(ReasoningEffort::High))));
        assert!(state.change(Change::Permissions(Some("acceptEdits".into()))));
        save(&mut state, vec![]);
        let a = on_disk("developer");
        assert_eq!(a.model(RunnerKind::Claude), Some("opus"));
        assert_eq!(a.reasoning_effort(RunnerKind::Claude), Some(ReasoningEffort::High));
        assert_eq!(a.permissions_mode(), Some("acceptEdits"));

        // The defaults remove the keys.
        assert!(state.change(Change::Model(None)));
        assert!(state.change(Change::Effort(None)));
        assert!(state.change(Change::Permissions(None)));
        assert!(state.change(Change::PreferredRunner(None)));
        save(&mut state, vec![]);
        let text = std::fs::read_to_string(agent_file(&home, "developer", "agent.yaml")).unwrap();
        for key in ["model:", "reasoning_effort:", "permissions_mode:", "preferred_runner:"] {
            assert!(!text.lines().any(|l| l.trim_start().starts_with(key)), "{key} in {text}");
        }
        assert_eq!(state.runner(), RunnerKind::Claude, "Claude Code applies without one");

        // Codex: its own model list; generic takes neither, so it's refused.
        assert!(state.change(Change::PreferredRunner(Some(RunnerKind::Codex))));
        assert_eq!(state.runner(), RunnerKind::Codex);
        assert!(state.models().iter().any(|m| m.starts_with("gpt")));
        assert!(state.change(Change::PreferredRunner(Some(RunnerKind::Generic))));
        assert!(!state.runner_takes_options());
        assert!(!state.change(Change::Model(Some("opus".into()))));
        assert!(state.error_at(&Field::Model).is_some());
    }

    #[test]
    fn an_unknown_model_or_mode_on_disk_stays_a_choice() {
        let home = home("ed-own-model");
        let path = agent_file(&home, "developer", "agent.yaml");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}permissions_mode: someday\nrunners:\n  claude:\n    model: claude-x-1\n")).unwrap();
        let state = editor("developer");
        assert_eq!(state.model(), Some("claude-x-1"));
        assert_eq!(state.models().last().map(String::as_str), Some("claude-x-1"));
        assert_eq!(state.permission_modes().last().map(String::as_str), Some("someday"));
    }

    #[test]
    fn skills_add_import_and_remove() {
        let home = home("ed-skills");
        // A user agent to add skills to.
        let config = AgentsConfig::load(PROJECT).unwrap().create_agent("docs", None).unwrap().save().unwrap().config;
        let mut state = AgentEditorState::new(config, "docs");
        assert!(state.skills().is_empty());

        // A skill needs a description.
        assert!(!state.change(Change::AddSkill { name: "release".into(), description: " ".into(), body: String::new() }));
        assert!(state.error_at(&Field::NewSkill).is_some());
        assert!(state.change(Change::AddSkill {
            name: "release".into(),
            description: "Cut a release.".into(),
            body: "Run the release script.".into(),
        }));
        assert!(state.error_at(&Field::NewSkill).is_none());

        // An existing skill folder imports whole.
        let src = home.root.join("import").join("triage");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("SKILL.md"), "---\nname: triage\ndescription: Sort issues.\n---\n").unwrap();
        assert!(state.change(Change::ImportSkill(src)));
        let names: Vec<String> = state.skills().into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["release", "triage"]);
        save(&mut state, vec![]);
        let skill = agent_file(&home, "docs", "skills/release/SKILL.md");
        let text = std::fs::read_to_string(skill).unwrap();
        assert!(text.contains("description: Cut a release.") && text.contains("Run the release script."), "{text}");

        // Remove drops the files at Save.
        assert!(state.change(Change::RemoveSkill("triage".into())));
        assert!(agent_file(&home, "docs", "skills/triage/SKILL.md").exists());
        save(&mut state, vec![]);
        assert!(!agent_file(&home, "docs", "skills/triage/SKILL.md").exists());
        assert!(!state.change(Change::RemoveSkill("triage".into())), "already gone");
    }

    #[test]
    fn reset_puts_the_shipped_files_in_the_draft() {
        let _home = home("ed-reset");
        let mut state = editor("orchestrator");
        assert_eq!(state.provenance(), Provenance::Customized);
        assert!(state.reset_blocked().is_none());
        assert!(state.change(Change::Reset));
        assert!(state.is_dirty());
        assert_eq!(state.reset_blocked().as_deref(), Some("Already the shipped default."));
        save(&mut state, vec![]);
        assert_eq!(state.provenance(), Provenance::Shipped);

        // A user's own agent has nothing to reset to.
        let config = AgentsConfig::load(PROJECT).unwrap().create_agent("docs", None).unwrap();
        let state = AgentEditorState::new(config, "docs");
        assert!(state.reset_blocked().is_some());
        assert_eq!(state.provenance(), Provenance::UserCreated);
    }

    #[test]
    fn a_rename_waits_for_save_and_shipped_or_assigned_agents_refuse_it() {
        let home = home("ed-rename");
        // Shipped: refused at Save, at the field; nothing moved.
        let mut state = editor("qa");
        assert!(state.rename_blocked().is_some());
        state.type_name("tester");
        assert!(state.is_dirty());
        assert!(save(&mut state, vec![]).is_none());
        assert!(state.error_at(&Field::Name).is_some());
        assert!(agent_file(&home, "qa", "agent.yaml").exists());

        // The user's own: renamed at Save.
        let config = AgentsConfig::load(PROJECT).unwrap().create_agent("docs", None).unwrap().save().unwrap().config;
        let mut state = AgentEditorState::new(config, "docs");
        assert!(state.rename_blocked().is_none());
        state.type_name("writer");
        assert!(!agent_file(&home, "writer", "agent.yaml").exists(), "not before Save");
        save(&mut state, vec![]);
        assert_eq!(state.name(), "writer");
        assert!(agent_file(&home, "writer", "agent.yaml").exists() && !agent_file(&home, "docs", "agent.yaml").exists());
    }

    #[test]
    fn leaving_with_edits_asks_first() {
        let _home = home("ed-leave");
        let mut state = editor("developer");
        assert_eq!(state.leave(Leave::Back), Some(Leave::Back), "nothing unsaved");
        state.change(Change::Permissions(Some("plan".into())));
        assert_eq!(state.leave(Leave::Workflow("task".into())), None);
        assert_eq!(state.prompt(), Some(&Leave::Workflow("task".into())));
        state.cancel_prompt();
        assert!(state.prompt().is_none() && state.is_dirty());

        state.leave(Leave::Back);
        assert_eq!(state.discard_and_leave(), Some(Leave::Back));
        assert!(!state.is_dirty());

        state.change(Change::Permissions(Some("plan".into())));
        state.leave(Leave::Workflows);
        let job = state.save_and_leave(vec![]).unwrap();
        assert_eq!(state.saved(run_save(job)), Some(Leave::Workflows));
        assert_eq!(on_disk("developer").permissions_mode(), Some("plan"));
    }

    #[test]
    fn a_file_changed_under_the_save_is_kept_and_discard_reloads() {
        let home = home("ed-stale");
        let mut state = editor("developer");
        state.change(Change::Description("Mine.".into()));
        let path = agent_file(&home, "developer", "instructions.md");
        std::fs::write(&path, "# By hand\n").unwrap();
        assert!(save(&mut state, vec![]).is_none());
        let error = state.error_at(&Field::Save).unwrap().to_string();
        assert!(error.contains("Discard to reload"), "{error}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# By hand\n", "the hand edit stays");
        assert!(state.discard(), "the files must be read again");
        assert!(state.reloaded(AgentsConfig::load(PROJECT).unwrap()));
        assert_eq!(state.agent().instructions(), "# By hand\n");
    }

    #[test]
    fn duplicate_and_delete_run_on_the_saved_model() {
        let home = home("ed-dup");
        let mut state = editor("developer");
        assert_eq!(state.duplicate_name(), "developer-copy");
        assert!(state.delete_blocked().is_some(), "shipped");
        state.ask_delete();
        assert!(!state.confirming_delete(), "a blocked delete asks nothing");

        let base = state.begin_job().unwrap();
        assert!(state.begin_job().is_none(), "one at a time");
        let config = run_duplicate(base, "developer", "developer-copy").unwrap();
        assert!(agent_file(&home, "developer-copy", "instructions.md").exists());

        let mut copy = AgentEditorState::new(config, "developer-copy");
        assert_eq!(copy.duplicate_name(), "developer-copy-copy");
        assert!(copy.delete_blocked().is_none());
        copy.ask_delete();
        assert!(copy.confirming_delete());
        let base = copy.begin_job().unwrap();
        assert!(!copy.confirming_delete());
        run_delete(base, "developer-copy").unwrap();
        assert!(!agent_file(&home, "developer-copy", "agent.yaml").exists());
    }

    #[test]
    fn assignments_list_the_statuses_that_run_the_agent() {
        let _home = home("ed-assign");
        let state = editor("developer");
        let rows: Vec<(String, String)> =
            state.assignments().into_iter().map(|a| (a.workflow, a.status_name)).collect();
        assert!(rows.contains(&("task".to_string(), "In Progress".to_string())), "{rows:?}");
        assert!(state.rename_blocked().is_some());
        assert!(editor("security").assignments().is_empty());
    }
}
