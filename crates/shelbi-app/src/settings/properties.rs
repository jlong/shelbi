//! Settings ▸ Properties' shared form (`settings-properties-page`): the
//! project YAML's own settings, the ones no other project page covers.
//!
//! ```text
//!  Properties
//!  Settings for this project, stored in ~/.shelbi/projects/shelbi.yaml.
//!
//!  Name             [ Shelbi                          ]
//!  Repository       [ git@github.com:jlong/shelbi.git ]
//!  Default branch   [ main                            ]
//!  Default workflow [ app                           ⌄ ]
//!  Editor           [ Default ($EDITOR or vim)      ⌄ ]
//!  Issue tracker    GitHub · jlong/shelbi
//!                   Change it in the project YAML.
//!  ▸ Advanced
//!    Workspace poll interval    [ 5   ] seconds
//!    GitHub reconcile interval  [ 900 ] seconds
//! ```
//!
//! The values live beside `default_workflow:` and `orchestrator.awareness:`,
//! so the form loads and saves them through the project's
//! [`WorkflowConfig`] ([`WorkflowConfig::properties`],
//! [`WorkflowConfig::set_properties`]): one hash-checked, comment-preserving
//! plan sent to the daemon that owns the project, here or on the far
//! machine, which writes an in-repo project's `editor:` to its `local.yaml`.
//!
//! Saving works as the General and Workflows pages' do: there is no Save
//! button. A text field saves when it's committed (Enter, or focus leaving
//! it); a picker saves when a choice is made. Each value is checked first
//! ([`project_properties::check`]); one that's refused keeps its text, shows
//! its error under the field, and isn't written, while the other fields'
//! changes still save. The project file is checked as `shelbi config lint`
//! would before it's written (the state layer refuses a file the loader
//! wouldn't take), so nothing invalid lands on disk. Saves run one at a
//! time; a commit made while one is in flight is saved when it lands
//! ([`PropertiesForm::saved_result`] hands it back).
//!
//! Toolkit-free: the desktop app's and the TUI's Properties pages draw
//! [`PropertiesForm`] with the labels here.

use std::collections::HashMap;

use shelbi_state::project_properties::{self, Field, Properties};
use shelbi_state::tools::{DetectedTools, ToolOption};
use shelbi_state::workflow_config::WorkflowConfig;
use shelbi_state::{ToolChoice, ToolKind};

use crate::settings::general::{PickerEntry, ToolPicker, ToolSetting, DEFAULT_EDITOR_LABEL};
use crate::settings::orchestrator::SaveStatus;
use crate::settings::store::{self, ConfigStore};
use crate::settings::summary::display_path;

/// The page's title.
pub const TITLE: &str = "Properties";
/// The Issue tracker row's label.
pub const ISSUE_TRACKER_LABEL: &str = "Issue tracker";
/// The note under the issue tracker's summary.
pub const ISSUE_TRACKER_NOTE: &str = "Change it in the project YAML.";
/// The Advanced group's disclosure.
pub const ADVANCED_LABEL: &str = "Advanced";
/// The unit after an interval field.
pub const SECONDS: &str = "seconds";

/// A field the page edits as text. The Default workflow is a picker; the
/// Editor is a picker whose Other command… types into [`Text::Editor`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Text {
    Label,
    Repository,
    DefaultBranch,
    Editor,
    PollInterval,
    ReconcileInterval,
}

/// The text fields, top to bottom: the main group, then Advanced's.
pub const TEXTS: [Text; 6] = [
    Text::Label,
    Text::Repository,
    Text::DefaultBranch,
    Text::Editor,
    Text::PollInterval,
    Text::ReconcileInterval,
];

impl Text {
    pub fn label(self) -> &'static str {
        match self {
            Text::Label => "Name",
            Text::Repository => "Repository",
            Text::DefaultBranch => "Default branch",
            Text::Editor => "Editor",
            Text::PollInterval => "Workspace poll interval",
            Text::ReconcileInterval => "GitHub reconcile interval",
        }
    }

    /// The line under the field.
    pub fn help(self) -> &'static str {
        match self {
            Text::Label => "Shown on the project’s tab and in the sidebar. Its ID stays the same.",
            Text::Repository => "The project’s remote, as recorded when it was set up.",
            Text::DefaultBranch => "Where workspace branches start, unless a workflow says otherwise.",
            Text::Editor => {
                "Saved as the project’s editor:. Edit in … opens Settings ▸ General’s editor; Shelbi doesn’t read this one yet."
            }
            Text::PollInterval => "How often the orchestrator checks each workspace.",
            Text::ReconcileInterval => {
                "How often Shelbi checks GitHub for pull requests merged there. 0 turns it off."
            }
        }
    }

    /// Whether it sits under Advanced.
    pub fn advanced(self) -> bool {
        matches!(self, Text::PollInterval | Text::ReconcileInterval)
    }

    /// Whether it holds a number of seconds.
    pub fn numeric(self) -> bool {
        self.advanced()
    }

    fn field(self) -> Field {
        match self {
            Text::Label => Field::Label,
            Text::Repository => Field::GithubUrl,
            Text::DefaultBranch => Field::DefaultBranch,
            Text::Editor => Field::Editor,
            Text::PollInterval => Field::PollInterval,
            Text::ReconcileInterval => Field::ReconcileInterval,
        }
    }

    fn of(field: Field) -> Option<Text> {
        TEXTS.into_iter().find(|t| t.field() == field)
    }

    /// The field's text for `p`.
    fn shown(self, p: &Properties) -> String {
        match self {
            Text::Label => p.label.clone().unwrap_or_default(),
            Text::Repository => p.github_url.clone().unwrap_or_default(),
            Text::DefaultBranch => p.default_branch.clone(),
            Text::Editor => p.editor.clone().unwrap_or_default(),
            Text::PollInterval => p.workspace_poll_interval_secs.to_string(),
            Text::ReconcileInterval => p.github_reconcile_interval_secs.to_string(),
        }
    }

    /// `p` with this field set from `text`, or why `text` can't be one.
    fn set(self, p: &mut Properties, text: &str) -> Result<(), String> {
        let text = text.trim();
        let optional = || (!text.is_empty()).then(|| text.to_string());
        let secs = || {
            text.parse::<u64>()
                .map_err(|_| "Enter a whole number of seconds.".to_string())
        };
        match self {
            Text::Label => p.label = optional(),
            Text::Repository => p.github_url = optional(),
            Text::DefaultBranch => p.default_branch = text.to_string(),
            Text::Editor => p.editor = optional(),
            Text::PollInterval => p.workspace_poll_interval_secs = secs()?,
            Text::ReconcileInterval => p.github_reconcile_interval_secs = secs()?,
        }
        Ok(())
    }
}

/// The Default workflow picker's label.
pub const DEFAULT_WORKFLOW_LABEL: &str = "Default workflow";
/// Its help line.
pub const DEFAULT_WORKFLOW_HELP: &str = "The workflow a new issue gets when it doesn’t name one.";

/// The Properties page's state: the project's config as read, the fields as
/// typed, what each field's last check said, and the save in flight.
#[derive(Debug, Clone, Default)]
pub struct PropertiesForm {
    /// `None` until it's read, or while it doesn't read.
    config: Option<WorkflowConfig>,
    unreadable: Option<String>,
    drafts: HashMap<Text, String>,
    /// The Default workflow as picked, until it's saved.
    workflow: Option<String>,
    errors: HashMap<Text, String>,
    saving: bool,
    /// A commit came in while a save was in flight.
    queued: bool,
    status: SaveStatus,
    advanced: bool,
}

impl PropertiesForm {
    /// The config was read: `Ok` with it, or why it doesn't read. Ignored
    /// while a save is in flight (its result is newer). A field the user has
    /// changed keeps its text; the rest show the file's.
    pub fn load(&mut self, read: Result<WorkflowConfig, String>) {
        if self.saving {
            return;
        }
        match read {
            Ok(config) => {
                for t in TEXTS {
                    if !self.dirty(t) {
                        self.drafts.insert(t, t.shown(config.properties()));
                        self.errors.remove(&t);
                    }
                }
                if self.workflow.as_deref() == Some(config.default_workflow()) {
                    self.workflow = None;
                }
                self.config = Some(config);
                self.unreadable = None;
            }
            Err(e) => {
                self.config = None;
                self.unreadable = Some(e);
            }
        }
    }

    /// Forget everything (another project is shown). A save in flight is
    /// the client's to drop by its ticket.
    pub fn reset(&mut self) {
        *self = PropertiesForm::default();
    }

    /// Whether the config has been read (or failed to).
    pub fn loaded(&self) -> bool {
        self.config.is_some() || self.unreadable.is_some()
    }

    /// Why the config doesn't read, if it doesn't.
    pub fn unreadable(&self) -> Option<&str> {
        self.unreadable.as_deref()
    }

    /// Whether the fields take edits: the config read.
    pub fn editable(&self) -> bool {
        self.config.is_some()
    }

    /// Whether a save is in flight.
    pub fn saving(&self) -> bool {
        self.saving
    }

    /// The config as last read or saved.
    pub fn config(&self) -> Option<&WorkflowConfig> {
        self.config.as_ref()
    }

    /// The line under the title: the file the page edits.
    pub fn subtitle(&self) -> String {
        match &self.config {
            Some(c) => format!("Settings for this project, stored in {}.", display_path(c.project_path())),
            None => "Settings for this project.".into(),
        }
    }

    /// `t`'s text as typed (the file's until it's edited).
    pub fn text(&self, t: Text) -> &str {
        self.drafts.get(&t).map(String::as_str).unwrap_or("")
    }

    /// `t`'s text as saved.
    fn saved_text(&self, t: Text) -> String {
        self.config.as_ref().map(|c| t.shown(c.properties())).unwrap_or_default()
    }

    /// What an empty `t` stands for.
    pub fn placeholder(&self, t: Text) -> String {
        match t {
            Text::Label => self.config.as_ref().map(|c| c.project().to_string()).unwrap_or_default(),
            Text::Repository => "git@github.com:owner/repo.git".into(),
            Text::DefaultBranch => project_properties::DEFAULT_BRANCH.into(),
            Text::Editor => DEFAULT_EDITOR_LABEL.into(),
            Text::PollInterval => project_properties::DEFAULT_POLL_SECS.to_string(),
            Text::ReconcileInterval => project_properties::DEFAULT_RECONCILE_SECS.to_string(),
        }
    }

    /// The help under `t`: its own, and for an in-repo project's editor,
    /// the file it's kept in.
    pub fn help(&self, t: Text) -> String {
        match (t, &self.config) {
            (Text::Editor, Some(c)) if c.layout() == shelbi_core::ConfigMode::InRepo => {
                format!("{} Kept in {}, not in the repo.", t.help(), display_path(c.editor_path()))
            }
            _ => t.help().to_string(),
        }
    }

    /// Type into `t`. Its last error goes until it's checked again.
    pub fn set_text(&mut self, t: Text, text: &str) {
        if !self.editable() || (t == Text::Editor && !self.editor_editable()) {
            return;
        }
        self.drafts.insert(t, text.to_string());
        self.errors.remove(&t);
    }

    /// Put `t` back on the file's value (Esc).
    pub fn revert(&mut self, t: Text) {
        let saved = self.saved_text(t);
        self.drafts.insert(t, saved);
        self.errors.remove(&t);
    }

    /// Whether `t` differs from the file.
    pub fn dirty(&self, t: Text) -> bool {
        self.config.is_some() && self.drafts.get(&t).is_some_and(|d| *d != self.saved_text(t))
    }

    /// Whether anything differs from the file.
    pub fn any_dirty(&self) -> bool {
        TEXTS.into_iter().any(|t| self.dirty(t)) || self.workflow.is_some()
    }

    /// `t`'s last error.
    pub fn error(&self, t: Text) -> Option<&str> {
        self.errors.get(&t).map(String::as_str)
    }

    /// Whether the editor can be saved (an in-repo project's `local.yaml`
    /// was read).
    pub fn editor_editable(&self) -> bool {
        self.config.as_ref().is_some_and(WorkflowConfig::editor_editable)
    }

    /// The workflows to pick the default from, in name order.
    pub fn workflows(&self) -> Vec<String> {
        self.config
            .as_ref()
            .map(|c| c.workflows().map(|w| w.name.clone()).collect())
            .unwrap_or_default()
    }

    /// The default workflow shown: the one being saved, else the file's.
    pub fn default_workflow(&self) -> &str {
        self.workflow
            .as_deref()
            .or(self.config.as_ref().map(WorkflowConfig::default_workflow))
            .unwrap_or("")
    }

    /// The issue tracker's summary (read-only on this page).
    pub fn issue_tracker(&self) -> Option<String> {
        self.config.as_ref().map(|c| c.issue_tracker().summary())
    }

    /// Whether Advanced is open (it opens itself for an error inside it).
    pub fn advanced_open(&self) -> bool {
        self.advanced || TEXTS.into_iter().any(|t| t.advanced() && self.errors.contains_key(&t))
    }

    pub fn toggle_advanced(&mut self) {
        self.advanced = !self.advanced_open();
    }

    /// Pick the default workflow: saves it.
    pub fn choose_workflow(&mut self, name: &str) -> Option<WorkflowConfig> {
        if !self.editable() {
            return None;
        }
        self.workflow = Some(name.to_string());
        self.commit()
    }

    /// The default workflow `steps` places from the shown one, wrapping (a
    /// keyboard's ←/→).
    pub fn step_workflow(&mut self, steps: isize) -> Option<WorkflowConfig> {
        let all = self.workflows();
        if all.is_empty() {
            return None;
        }
        let at = all.iter().position(|w| w == self.default_workflow()).unwrap_or(0) as isize;
        let next = all[(at + steps).rem_euclid(all.len() as isize) as usize].clone();
        self.choose_workflow(&next)
    }

    /// The Editor's picker: the default, this machine's command-line editors
    /// (the project's `editor:` is a command line), the current command when
    /// it is none of those, Browse… and Other command….
    pub fn editor_picker(&self, detected: &DetectedTools) -> ToolPicker {
        let cli: Vec<ToolOption> = detected.editors.iter().filter(|t| t.kind == ToolKind::Cli).cloned().collect();
        let current = self
            .config
            .as_ref()
            .and_then(|c| c.properties().editor.clone())
            .map(ToolChoice::Command);
        let custom = current.as_ref().filter(|c| !cli.iter().any(|t| &t.choice == *c)).map(ToolOption::custom);
        ToolPicker {
            setting: ToolSetting::Editor,
            default_detail: None,
            cli,
            apps: Vec::new(),
            custom,
            current,
        }
    }

    /// Pick an editor entry: Default and a tool save at once; Browse and
    /// Other command need more from the user, so they change nothing here.
    pub fn choose_editor_entry(&mut self, entry: &PickerEntry) -> Option<WorkflowConfig> {
        let command = match entry {
            PickerEntry::Default => String::new(),
            PickerEntry::Tool(t) => t.choice.command()?.to_string(),
            PickerEntry::Browse | PickerEntry::OtherCommand => return None,
        };
        self.choose_editor(&command)
    }

    /// Set the editor's command (empty: the default) and save it.
    pub fn choose_editor(&mut self, command: &str) -> Option<WorkflowConfig> {
        if !self.editor_editable() {
            return None;
        }
        self.set_text(Text::Editor, command);
        self.commit()
    }

    /// Browse picked `path` for the editor: a program's path is its command;
    /// an app is refused (the project's editor is a command line).
    pub fn browse_editor(&mut self, path: &std::path::Path) -> Result<Option<WorkflowConfig>, String> {
        match shelbi_state::tools::choice_from_path(path)? {
            ToolChoice::Command(command) => Ok(self.choose_editor(&command)),
            ToolChoice::App { .. } => Err("Pick a program: a project’s editor is a command line.".into()),
        }
    }

    /// Check every field and save what changed: the config to write
    /// ([`save`]), or `None` when nothing valid changed, the form can't
    /// save, or a save is in flight (this one follows it,
    /// [`Self::saved_result`]). A refused field keeps its text and error and
    /// is left as the file has it.
    pub fn commit(&mut self) -> Option<WorkflowConfig> {
        let config = self.config.as_ref()?;
        if self.saving {
            self.queued = true;
            return None;
        }
        let saved = config.properties().clone();
        let mut next = saved.clone();
        let mut errors = HashMap::new();
        for t in TEXTS {
            if !self.dirty(t) {
                continue;
            }
            let mut candidate = next.clone();
            match t.set(&mut candidate, self.text(t)) {
                Ok(()) => match project_properties::check(&candidate).into_iter().find(|p| Text::of(p.field) == Some(t)) {
                    Some(problem) => {
                        errors.insert(t, problem.message);
                    }
                    None => next = candidate,
                },
                Err(e) => {
                    errors.insert(t, e);
                }
            }
        }
        self.errors = errors;
        let mut out = config.clone();
        if let Some(workflow) = self.workflow.clone() {
            if workflow != config.default_workflow() {
                match out.set_default_workflow(&workflow) {
                    Ok(c) => out = c,
                    Err(e) => {
                        self.workflow = None;
                        self.status = SaveStatus::Failed(sentence(&e.to_string()));
                        return None;
                    }
                }
            }
        }
        if next != saved {
            match out.set_properties(next) {
                Ok(c) => out = c,
                Err(e) => {
                    self.status = SaveStatus::Failed(sentence(&e.to_string()));
                    return None;
                }
            }
        }
        if !out.is_dirty() {
            self.workflow = None;
            return None;
        }
        self.saving = true;
        self.status = SaveStatus::Saving;
        Some(out)
    }

    /// The save landed. On success the form holds the config as now on
    /// disk; on failure the fields keep what was typed and the status says
    /// why. Returns the next save to start: a commit made while this one was
    /// in flight.
    pub fn saved_result(&mut self, result: Result<WorkflowConfig, String>) -> Option<WorkflowConfig> {
        if !std::mem::take(&mut self.saving) {
            return None;
        }
        match result {
            Ok(config) => {
                for t in TEXTS {
                    // A field saved as typed reads back the file's spelling
                    // (trimmed); one typed into since keeps its text.
                    let typed = self.text(t).trim().to_string();
                    if typed == t.shown(config.properties()) {
                        self.drafts.insert(t, t.shown(config.properties()));
                    }
                }
                if self.workflow.as_deref() == Some(config.default_workflow()) {
                    self.workflow = None;
                }
                self.config = Some(config);
                self.status = SaveStatus::Saved;
            }
            Err(e) => {
                self.workflow = None;
                self.status = SaveStatus::Failed(e);
                self.queued = false;
                return None;
            }
        }
        if std::mem::take(&mut self.queued) {
            return self.commit();
        }
        None
    }

    /// A save the form handed out was carried out by someone else (a
    /// client's leave prompt) and landed: stop waiting for it. The page reads
    /// the file again when it's next shown.
    pub fn forget_save(&mut self, result: Result<(), String>) {
        if !std::mem::take(&mut self.saving) {
            return;
        }
        self.queued = false;
        self.status = match result {
            Ok(()) => SaveStatus::Saved,
            Err(e) => SaveStatus::Failed(e),
        };
    }

    pub fn status(&self) -> &SaveStatus {
        &self.status
    }

    /// The save's line on the page, if any.
    pub fn status_message(&self) -> Option<String> {
        match &self.status {
            SaveStatus::Idle => None,
            SaveStatus::Saving => Some("Saving…".into()),
            SaveStatus::Saved => Some(match &self.config {
                Some(c) => format!("Saved to {}", display_path(c.project_path())),
                None => "Saved".into(),
            }),
            SaveStatus::Failed(e) => Some(e.clone()),
        }
    }

    /// Forget the save line (a new visit to the page).
    pub fn clear_status(&mut self) {
        if !self.saving {
            self.status = SaveStatus::Idle;
        }
    }
}

/// `text` as a sentence: its first letter capitalized.
fn sentence(text: &str) -> String {
    crate::settings::workspace_form::sentence(text)
}

/// Write `config` (from [`PropertiesForm::commit`]) through `store` and read
/// it back. Blocking: run it off the UI thread.
pub fn save(store: &dyn ConfigStore, config: &WorkflowConfig) -> Result<WorkflowConfig, String> {
    store::save_workflows(store, config)
        .map(|saved| saved.config)
        .map_err(|e| sentence(&e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp home with project `props`, whose YAML carries comments.
    fn home(tag: &str) -> (crate::settings::agents::fixture::Home, crate::settings::store::Store) {
        let lock = crate::test_env::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = crate::settings::agents::fixture::Home::new("props", tag, lock);
        let store = store::Disk::store(tag);
        (home, store)
    }

    fn project_yaml(tag: &str) -> String {
        let path = shelbi_state::projects_dir().unwrap().join(format!("{tag}.yaml"));
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn nothing_saves_before_the_config_reads() {
        let mut form = PropertiesForm::default();
        assert!(!form.loaded() && !form.editable());
        form.set_text(Text::Label, "x");
        assert_eq!(form.text(Text::Label), "", "no edits before the load");
        assert!(form.commit().is_none());
        form.load(Err("no project YAML".into()));
        assert_eq!(form.unreadable(), Some("no project YAML"));
        assert!(form.choose_workflow("task").is_none());
        assert_eq!(form.status(), &SaveStatus::Idle);
    }

    #[test]
    fn load_edit_validate_and_save_round_trip_keeping_comments() {
        let (home, store) = home("props-page");
        let path = shelbi_state::projects_dir().unwrap().join("props-page.yaml");
        let original = format!("# The project's own header.\n{}", project_yaml("props-page"));
        std::fs::write(&path, &original).unwrap();

        let mut form = PropertiesForm::default();
        form.load(store.workflows());
        assert!(form.editable());
        assert!(form.subtitle().starts_with("Settings for this project, stored in "));
        assert!(form.subtitle().ends_with("props-page.yaml."));
        assert_eq!(form.text(Text::DefaultBranch), "main");
        assert_eq!(form.text(Text::PollInterval), "5");
        assert_eq!(form.placeholder(Text::Label), "props-page", "an empty name shows the id");
        assert!(form.commit().is_none(), "nothing changed");

        // A bad value shows under its field and isn't written; the good
        // ones save.
        form.set_text(Text::PollInterval, "0");
        form.set_text(Text::ReconcileInterval, "soon");
        form.set_text(Text::Label, "  Props Page ");
        assert!(!form.advanced_open() || form.error(Text::PollInterval).is_some());
        let next = form.commit().expect("the name saves");
        assert_eq!(form.error(Text::PollInterval), Some("Use 1 to 3600 seconds."));
        assert_eq!(form.error(Text::ReconcileInterval), Some("Enter a whole number of seconds."));
        assert!(form.advanced_open(), "Advanced opens on its fields' errors");
        assert!(form.saving() && form.status() == &SaveStatus::Saving);
        form.set_text(Text::Repository, "git@github.com:a/props.git");
        assert!(form.commit().is_none(), "one save at a time");
        let follow = form.saved_result(save(store.as_ref(), &next));
        assert_eq!(form.text(Text::Label), "Props Page", "it reads back trimmed");
        let followed = follow.expect("the commit made mid-save follows");
        assert_eq!(form.status(), &SaveStatus::Saving, "and starts at once");
        assert!(form.saved_result(save(store.as_ref(), &followed)).is_none());
        assert_eq!(form.status(), &SaveStatus::Saved);
        assert_eq!(shelbi_state::load_project("props-page").unwrap().github_url.as_deref(), Some("git@github.com:a/props.git"));
        assert_eq!(form.text(Text::PollInterval), "0", "the refused value keeps its text");

        let text = project_yaml("props-page");
        assert!(text.starts_with("# The project's own header.\n"), "comments stay: {text}");
        assert!(text.contains("name: Props Page"));
        assert!(!text.contains("workspace_poll_interval_secs"), "the refused value wasn't written");
        let project = shelbi_state::load_project("props-page").unwrap();
        assert_eq!(project.display_label(), "Props Page");

        // Fixing it saves it; Esc puts a field back.
        form.set_text(Text::PollInterval, "10");
        form.set_text(Text::ReconcileInterval, "0");
        let next = form.commit().unwrap();
        assert_eq!(form.error(Text::PollInterval), None);
        form.saved_result(save(store.as_ref(), &next));
        let project = shelbi_state::load_project("props-page").unwrap();
        assert_eq!(project.workspace_poll_interval_secs, 10);
        assert_eq!(project.github_reconcile_interval_secs, 0);
        form.set_text(Text::DefaultBranch, "bad branch");
        assert!(form.commit().is_none(), "nothing valid changed");
        assert!(form.error(Text::DefaultBranch).unwrap().contains("isn't a valid branch name"));
        form.revert(Text::DefaultBranch);
        assert_eq!((form.text(Text::DefaultBranch), form.error(Text::DefaultBranch)), ("main", None));
        drop(home);
    }

    #[test]
    fn the_pickers_save_at_once() {
        let (home, store) = home("props-pick");
        let mut form = PropertiesForm::default();
        form.load(store.workflows());
        let all = form.workflows();
        assert!(all.len() > 1, "the fixture ships more than one workflow: {all:?}");
        let other = all.iter().find(|w| *w != form.default_workflow()).unwrap().clone();
        let next = form.choose_workflow(&other).unwrap();
        assert_eq!(form.default_workflow(), other, "the pick shows while it saves");
        form.saved_result(save(store.as_ref(), &next));
        assert_eq!(form.default_workflow(), other);
        assert_eq!(store.workflows().unwrap().default_workflow(), other);

        let picker = form.editor_picker(&DetectedTools::default());
        assert_eq!(picker.current_label(), DEFAULT_EDITOR_LABEL);
        let next = form.choose_editor("hx --vsplit").unwrap();
        form.saved_result(save(store.as_ref(), &next));
        assert_eq!(shelbi_state::load_project("props-pick").unwrap().editor.as_deref(), Some("hx --vsplit"));
        let picker = form.editor_picker(&DetectedTools::default());
        assert_eq!(picker.custom.as_ref().map(|c| c.choice.clone()), Some(ToolChoice::Command("hx --vsplit".into())));
        let next = form.choose_editor_entry(&PickerEntry::Default).unwrap();
        form.saved_result(save(store.as_ref(), &next));
        assert_eq!(shelbi_state::load_project("props-pick").unwrap().editor, None);
        assert_eq!(form.issue_tracker().as_deref(), Some("Files on disk"));

        // A failed write keeps what was typed and says why.
        form.set_text(Text::Label, "Mine");
        let _ = form.commit().unwrap();
        assert!(form.saved_result(Err("Changed since it was loaded".into())).is_none());
        assert_eq!(form.status_message().as_deref(), Some("Changed since it was loaded"));
        assert_eq!(form.text(Text::Label), "Mine");
        assert!(form.dirty(Text::Label));
        drop(home);
    }
}
