//! The agent details editor in the TUI (`tui-agent-editor`), after the
//! desktop app's (`desktop/src/views/agent_editor.rs`) over the same shared
//! draft (`shelbi_app::settings::agent_editor`). Settings ▸ Agents opens it
//! (Enter on an agent, Edit in its menu, an agent just created or
//! duplicated). Like the desktop's, it takes the settings sidebar's column
//! too while it shows.
//!
//! **The header** (never scrolls): Back, the agent's name with its badge
//! (Default, Customized, none for the user's own) and a `●` while there are
//! unsaved edits, then Save changes, Discard and the `⋮ Actions` menu (Edit
//! YAML, Edit instructions.md, Duplicate, Reset to default, Delete).
//!
//! **The page** scrolls under it, one focusable row per control: Agent
//! (Name, read-only with the guard's reason where a rename is refused;
//! Description); Instructions (`instructions.md` in an in-place multi-line
//! editor, Edit instructions.md in the user's editor, the shared
//! instructions' link); Skills (each with its description: Inspect, Open
//! SKILL.md, Remove; Add skill's form; Import from a path); Runner
//! preferences (Preferred runner, Model with a custom name, Reasoning
//! effort, Permissions, each with what applies and where it comes from);
//! Workflow assignments (a row per status, opening that workflow's editor
//! on it; Manage assignments in Workflows); Reset to default and Delete.
//!
//! Keys (no mouse needed): the user's up / down (and the arrows) walk the
//! rows, PgUp / PgDn / Home / End jump, Tab / Shift+Tab go to the next or
//! previous section. Enter does a row's main thing: a text field starts
//! typing (each key goes to the draft at once; Enter, Esc or ↑↓ stop), the
//! instructions or a skill body open for editing (every key types, Enter a
//! line break; Esc stops), a picker opens, a skill opens its menu. `m` opens
//! the row's menu (else the agent's actions), `i` inspects a skill, `e`
//! opens a file in the user's editor, `x` / Delete removes a skill (after a
//! confirm), `n` adds a skill, `y` is Edit YAML, Ctrl+S saves, Esc goes
//! Back.
//!
//! Every control edits the shared draft through `shelbi_state`'s
//! `AgentsConfig`; nothing is written before Save. A change the model
//! refuses shows its message under its row. Save runs off the input loop
//! ([`AgentWork`]); a failed save keeps the draft and says why. Back, Esc,
//! opening a file in the user's editor, Duplicate and an assignment ask
//! Save / Discard / Cancel first when there are unsaved edits (the shared
//! `Leave` guard); a project switch asks the same through Settings' prompt.
//! A save refused because the files changed on disk asks Reload or Keep
//! editing, never overwriting.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use shelbi_app::settings::agent_editor::{
    delete_title, import_path, leave_title, run_delete, run_duplicate, run_save, AgentEditorState, AgentFile,
    Change, Choice, Field, Leave, SaveJob, SaveOutcome, Source, APPLY_NOTE, ASSIGNMENTS_HELPER, CONFLICT_MESSAGE,
    DELETE_MESSAGE, DESCRIPTION_PLACEHOLDER, IMPORT_HELP, INSTRUCTIONS_HELPER, LEAVE_MESSAGE, NOT_ASSIGNED,
    RUNNER_HELPER, SKILLS_EMPTY, SKILLS_EMPTY_HELP, SKILL_DESCRIPTION_PLACEHOLDER,
};
use shelbi_app::settings::{Job, Ticket};
use shelbi_app::text_area::TextArea;
use shelbi_state::agents_config::{AgentsConfig, SkillLayout};

use super::workflow_editor::picker_line;
use super::workflows::{add_line, cell, error_rows, plain_line, LineEdit, Nav, WorkflowsPage};
use super::{heading, render_band, selected_style, wrapped, Row, PROMPT_BUTTONS};
use crate::panel::{self, PAD};
use crate::theme::palette;

/// A multi-line field's left rule.
const AREA_PREFIX: &str = "│ ";
/// The instructions' preview, in lines, while not being edited.
const PREVIEW_LINES: usize = 10;

/// A one-line text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Text {
    Name,
    Description,
    /// The Add skill form's fields.
    SkillName,
    SkillDescription,
    /// The path Import copies a skill from.
    Import,
    /// A model name typed in (Model ▸ Custom model…).
    CustomModel,
}

/// A multi-line field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Area {
    Instructions,
    /// The Add skill form's SKILL.md body.
    SkillBody,
}

/// A row the keys stop on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Back,
    Save,
    Discard,
    Actions,
    Text(Text),
    Area(Area),
    EditInstructions,
    SharedLink,
    Skill(String),
    AddSkill,
    SkillAdd,
    SkillCancel,
    Runner,
    Model,
    Effort,
    Permissions,
    Assignment(usize),
    ManageAssignments,
    Reset,
    Delete,
}

/// The sections Tab steps through, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    Agent,
    Instructions,
    Skills,
    Runner,
    Assignments,
    Manage,
}

impl Item {
    fn section(&self) -> Section {
        match self {
            Item::Back | Item::Save | Item::Discard | Item::Actions => Section::Header,
            Item::Text(Text::Name | Text::Description) => Section::Agent,
            Item::Area(Area::Instructions) | Item::EditInstructions | Item::SharedLink => Section::Instructions,
            Item::Skill(_)
            | Item::AddSkill
            | Item::Text(Text::SkillName | Text::SkillDescription | Text::Import)
            | Item::Area(Area::SkillBody)
            | Item::SkillAdd
            | Item::SkillCancel => Section::Skills,
            Item::Runner | Item::Model | Item::Text(Text::CustomModel) | Item::Effort | Item::Permissions => {
                Section::Runner
            }
            Item::Assignment(_) | Item::ManageAssignments => Section::Assignments,
            Item::Reset | Item::Delete => Section::Manage,
        }
    }
}

/// A picker the editor opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pick {
    Runner,
    Model,
    Effort,
    Permissions,
}

/// One thing a row, a menu entry or a picker's choice does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    Change(Change),
    Type(Text),
    Edit(Area),
    Pick(Pick),
    SkillMenu(String),
    Inspect(String),
    AskRemove(String),
    OpenSkillForm,
    CloseSkillForm,
    AddSkill,
    Actions,
    Leave(Leave),
    Save,
    Discard,
    Delete,
    /// Close the menu (an entry that only explains).
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    label: String,
    /// Its key in a menu, or `current` in a picker.
    note: String,
    act: Act,
    /// Dimmed: it can't run (choosing it closes the menu).
    inert: bool,
    help: Option<String>,
}

/// What asks over the editor.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Confirm {
    /// Delete the agent? Delete / Cancel.
    Delete,
    /// Remove skill `.0` from the draft? Remove / Cancel.
    RemoveSkill(String),
    /// The save found the files changed on disk: Reload / Keep editing.
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Modal {
    List { title: String, entries: Vec<Entry>, selected: usize },
    Confirm { kind: Confirm, choice: usize },
    /// Skill `name` as the draft holds it, scrolled `top` lines.
    Inspect { name: String, top: usize },
}

/// What a click hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Item(usize),
    Entry(usize),
    Button(usize),
}

/// Where closing the editor lands on the Agents page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Back {
    /// The agent's row.
    Agent(String),
    /// The shared instructions.
    Shared,
}

/// What the editor asks of Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Stay,
    /// Start the work waiting in [`AgentEditorPage::take_work`].
    Work,
    /// Back to Settings ▸ Agents.
    Close(Back),
    /// Open workflow `name`'s editor, on status `status`.
    OpenWorkflow { name: String, status: Option<String> },
    /// Settings ▸ Workflows.
    ShowWorkflows,
    /// Open this file in the user's editor (the editor reads the files
    /// again when it returns).
    EditFile(PathBuf),
}

/// Background work the shell runs off the input loop ([`run_work`]), and
/// hands back under the editor's ticket.
pub(crate) enum AgentWork {
    /// Read `project`'s agents.
    Load { project: String },
    Save(SaveJob),
    Duplicate { base: AgentsConfig, name: String, copy: String },
    Delete { base: AgentsConfig, name: String },
}

/// What [`AgentWork`] did.
pub(crate) enum AgentDone {
    Loaded(Result<AgentsConfig, String>),
    Saved(SaveOutcome),
    Duplicated(Result<AgentsConfig, String>, String),
    Deleted(Result<(), String>),
}

/// Run `work` (blocking: off the input loop). `read` reads a project's
/// agents (the shell's Agents reader, which refuses the real `~/.shelbi`
/// in tests).
pub(crate) fn run_work(work: AgentWork, read: &dyn Fn(&str) -> Result<AgentsConfig, String>) -> AgentDone {
    match work {
        AgentWork::Load { project } => AgentDone::Loaded(read(&project)),
        AgentWork::Save(job) => AgentDone::Saved(run_save(job)),
        AgentWork::Duplicate { base, name, copy } => {
            let done = run_duplicate(base, &name, &copy);
            AgentDone::Duplicated(done, copy)
        }
        AgentWork::Delete { base, name } => AgentDone::Deleted(run_delete(base, &name)),
    }
}

/// What landing work asks of Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Landed {
    /// Stay in the editor.
    Stay,
    /// Files were written: the Agents index reads again.
    Wrote,
    /// Close the editor (a delete, the agent gone from the files).
    Close { wrote: bool, notice: Option<String> },
    /// Carry on to this leave (the leave prompt's Save landed).
    Then(Leave),
}

/// The Add skill form, while open.
#[derive(Debug, Clone, Default)]
struct SkillForm {
    name: String,
    description: String,
    body: TextArea,
}

/// A multi-line field being edited: its first wrapped line shown, the
/// lines it shows at once, and its wrapped lines at the last draw (clicks).
#[derive(Debug, Clone, Copy, Default)]
struct AreaView {
    top: usize,
    height: usize,
}

/// The editor's state: the shared draft, plus the TUI's rows, cursor, text
/// buffers and modal, and its background work's ticket.
pub(crate) struct AgentEditorPage {
    project: String,
    /// The agent being opened or edited.
    name: String,
    state: Option<AgentEditorState>,
    load_error: Option<String>,
    load: Job<()>,
    job: Job<()>,
    pending: Option<(Ticket, AgentWork)>,
    /// What the Description field holds.
    description: String,
    instructions: TextArea,
    skill_form: Option<SkillForm>,
    import: String,
    /// The custom model being typed (its row shows while `Some`).
    custom_model: Option<String>,
    /// The one-line field being typed.
    typing: Option<(Text, LineEdit)>,
    /// The multi-line field being edited, and its view.
    editing: Option<Area>,
    area: AreaView,
    modal: Option<Modal>,
    prompt_choice: usize,
    cursor: usize,
    scroll: u16,
    follow_cursor: bool,
    hits: Vec<(Rect, Hit)>,
}

impl AgentEditorPage {
    /// Edit agent `name` of `project`: at once from `config` when the
    /// Agents page has it loaded. Either way a read starts (the shell takes
    /// it, [`Self::take_work`]), bringing the files as they are now.
    pub(crate) fn open(project: &str, name: &str, config: Option<&AgentsConfig>) -> Self {
        let mut page = AgentEditorPage {
            project: project.to_string(),
            name: name.to_string(),
            state: None,
            load_error: None,
            load: Job::default(),
            job: Job::default(),
            pending: None,
            description: String::new(),
            instructions: TextArea::default(),
            skill_form: None,
            import: String::new(),
            custom_model: None,
            typing: None,
            editing: None,
            area: AreaView::default(),
            modal: None,
            prompt_choice: 0,
            cursor: 0,
            scroll: 0,
            follow_cursor: true,
            hits: Vec::new(),
        };
        if let Some(config) = config.filter(|c| c.agent(name).is_some()) {
            page.show(AgentEditorState::new(config.clone(), name));
        }
        page.reload();
        page
    }

    pub(crate) fn project(&self) -> &str {
        &self.project
    }

    pub(crate) fn name(&self) -> &str {
        self.state.as_ref().map_or(&self.name, |s| s.name())
    }

    #[cfg(test)]
    pub(crate) fn state(&self) -> Option<&AgentEditorState> {
        self.state.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn cursor_item(&self) -> Option<Item> {
        self.current()
    }

    #[cfg(test)]
    pub(crate) fn select(&mut self, item: &Item) -> bool {
        self.select_item(item)
    }

    #[cfg(test)]
    pub(crate) fn modal_title(&self) -> Option<String> {
        match &self.modal {
            Some(Modal::List { title, .. }) => Some(title.clone()),
            Some(Modal::Confirm { kind: Confirm::Delete, .. }) => Some("Delete agent".into()),
            Some(Modal::Confirm { kind: Confirm::RemoveSkill(_), .. }) => Some("Remove skill".into()),
            Some(Modal::Confirm { kind: Confirm::Conflict, .. }) => Some("Changed on disk".into()),
            Some(Modal::Inspect { name, .. }) => Some(format!("Skill {name}")),
            None => None,
        }
    }

    #[cfg(test)]
    pub(crate) fn editing(&self) -> Option<Area> {
        self.editing
    }

    #[cfg(test)]
    pub(crate) fn typing(&self) -> Option<Text> {
        self.typing.as_ref().map(|(t, _)| t.clone())
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.state.as_ref().is_some_and(AgentEditorState::is_dirty)
    }

    pub(crate) fn busy(&self) -> bool {
        self.state.as_ref().is_some_and(AgentEditorState::busy)
    }

    /// The work the shell should start now, if any.
    pub(crate) fn take_work(&mut self) -> Option<(Ticket, AgentWork)> {
        self.pending.take()
    }

    /// Read the files again (back from the user's editor, Settings
    /// reopened). The draft takes them only if it holds no edits when they
    /// land.
    pub(crate) fn reload(&mut self) {
        let ticket = self.load.begin(Some(&self.project));
        self.pending = Some((ticket, AgentWork::Load { project: self.project.clone() }));
    }

    /// Edit `state` from here on: every field shows it.
    fn show(&mut self, state: AgentEditorState) {
        self.name = state.name().to_string();
        self.state = Some(state);
        self.modal = None;
        self.skill_form = None;
        self.custom_model = None;
        self.import.clear();
        self.load_error = None;
        self.sync();
        self.clamp();
    }

    /// Put the draft's text in the fields (and stop typing in them).
    fn sync(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let a = state.agent();
        self.description = a.description().unwrap_or_default().to_string();
        let caret = self.instructions.cursor();
        self.instructions = TextArea::from_text(a.instructions());
        self.instructions.move_to(caret.0, caret.1);
        self.typing = None;
        self.editing = None;
    }

    /// What the text fields hold, as changes (Save applies those that
    /// differ from the draft).
    fn texts(&self) -> Vec<Change> {
        vec![Change::Description(self.description.clone()), Change::Instructions(self.instructions.text())]
    }

    /// A text field changed: the draft takes each that differs.
    fn typed(&mut self) {
        let texts = self.texts();
        if let Some(state) = self.state.as_mut() {
            state.type_texts(texts);
        }
    }

    fn text(&self, text: &Text) -> String {
        match text {
            Text::Name => self.state.as_ref().map(|s| s.typed_name().to_string()).unwrap_or_default(),
            Text::Description => self.description.clone(),
            Text::SkillName => self.skill_form.as_ref().map(|f| f.name.clone()).unwrap_or_default(),
            Text::SkillDescription => self.skill_form.as_ref().map(|f| f.description.clone()).unwrap_or_default(),
            Text::Import => self.import.clone(),
            Text::CustomModel => self.custom_model.clone().unwrap_or_default(),
        }
    }

    /// Field `text` now holds `value`: into its buffer, and the draft
    /// (the name waits for Save; the skill form, Import and a custom model
    /// wait for their own Enter).
    fn set_text(&mut self, text: &Text, value: String) {
        match text {
            Text::Name => {
                if let Some(state) = self.state.as_mut() {
                    state.type_name(&value);
                }
            }
            Text::Description => {
                self.description = value;
                self.typed();
            }
            Text::SkillName => {
                if let Some(f) = self.skill_form.as_mut() {
                    f.name = value;
                }
            }
            Text::SkillDescription => {
                if let Some(f) = self.skill_form.as_mut() {
                    f.description = value;
                }
            }
            Text::Import => self.import = value,
            Text::CustomModel => self.custom_model = Some(value),
        }
    }

    fn area_text(&mut self, area: Area) -> Option<&mut TextArea> {
        match area {
            Area::Instructions => Some(&mut self.instructions),
            Area::SkillBody => self.skill_form.as_mut().map(|f| &mut f.body),
        }
    }

    // --- rows -----------------------------------------------------------------

    /// The rows the keys stop on, top to bottom (the header's first).
    pub(crate) fn items(&self) -> Vec<Item> {
        let mut out = vec![Item::Back, Item::Save, Item::Discard, Item::Actions];
        let Some(state) = self.state.as_ref() else {
            return out;
        };
        out.extend([
            Item::Text(Text::Name),
            Item::Text(Text::Description),
            Item::Area(Area::Instructions),
            Item::EditInstructions,
            Item::SharedLink,
        ]);
        out.extend(state.skills().into_iter().map(|s| Item::Skill(s.name)));
        out.push(Item::AddSkill);
        if self.skill_form.is_some() {
            out.extend([
                Item::Text(Text::SkillName),
                Item::Text(Text::SkillDescription),
                Item::Area(Area::SkillBody),
                Item::SkillAdd,
                Item::SkillCancel,
            ]);
        }
        out.push(Item::Text(Text::Import));
        out.extend([Item::Runner, Item::Model]);
        if self.custom_model.is_some() {
            out.push(Item::Text(Text::CustomModel));
        }
        out.extend([Item::Effort, Item::Permissions]);
        out.extend((0..state.assignments().len()).map(Item::Assignment));
        out.extend([Item::ManageAssignments, Item::Reset, Item::Delete]);
        out
    }

    fn clamp(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn current(&self) -> Option<Item> {
        self.items().into_iter().nth(self.cursor)
    }

    fn select_item(&mut self, item: &Item) -> bool {
        match self.items().iter().position(|it| it == item) {
            Some(i) => {
                self.cursor = i;
                true
            }
            None => false,
        }
    }

    // --- acting -----------------------------------------------------------------

    /// What Enter does on `item`.
    fn primary(&self, item: &Item) -> Act {
        match item {
            Item::Back => Act::Leave(Leave::Back),
            Item::Save => Act::Save,
            Item::Discard => Act::Discard,
            Item::Actions => Act::Actions,
            // A locked name only explains (its reason shows under it).
            Item::Text(Text::Name) if self.state.as_ref().and_then(AgentEditorState::rename_blocked).is_some() => {
                Act::Nothing
            }
            Item::Text(t) => Act::Type(t.clone()),
            Item::Area(a) => Act::Edit(*a),
            Item::EditInstructions => Act::Leave(Leave::Open(AgentFile::Instructions)),
            Item::SharedLink => Act::Leave(Leave::SharedInstructions),
            Item::Skill(name) => Act::SkillMenu(name.clone()),
            Item::AddSkill => Act::OpenSkillForm,
            Item::SkillAdd => Act::AddSkill,
            Item::SkillCancel => Act::CloseSkillForm,
            Item::Runner => Act::Pick(Pick::Runner),
            Item::Model => Act::Pick(Pick::Model),
            Item::Effort => Act::Pick(Pick::Effort),
            Item::Permissions => Act::Pick(Pick::Permissions),
            Item::Assignment(i) => match self.state.as_ref().and_then(|s| s.assignments().get(*i).cloned()) {
                Some(a) => Act::Leave(Leave::Workflow { name: a.workflow, status: Some(a.status) }),
                None => Act::Nothing,
            },
            Item::ManageAssignments => Act::Leave(Leave::Workflows),
            Item::Reset => Act::Change(Change::Reset),
            Item::Delete => Act::Delete,
        }
    }

    fn run(&mut self, act: Act) -> EditorAction {
        self.modal = None;
        if self.state.is_none() {
            return match act {
                Act::Leave(Leave::Back) => EditorAction::Close(Back::Agent(self.name.clone())),
                _ => EditorAction::Stay,
            };
        }
        match act {
            Act::Change(change) => self.change(change),
            Act::Type(text) => {
                let value = self.text(&text);
                self.select_item(&Item::Text(text.clone()));
                self.typing = Some((text, LineEdit::new(&value)));
            }
            Act::Edit(area) => {
                self.select_item(&Item::Area(area));
                self.editing = Some(area);
                self.area = AreaView::default();
            }
            Act::Pick(pick) => self.open_pick(pick),
            Act::SkillMenu(name) => self.skill_menu(&name),
            Act::Inspect(name) => self.modal = Some(Modal::Inspect { name, top: 0 }),
            Act::AskRemove(name) => {
                // Cancel is focused: Enter alone never removes.
                self.modal = Some(Modal::Confirm { kind: Confirm::RemoveSkill(name), choice: 1 });
            }
            Act::OpenSkillForm => {
                if self.skill_form.is_none() {
                    self.skill_form = Some(SkillForm::default());
                }
                self.select_item(&Item::Text(Text::SkillName));
                self.typing = Some((Text::SkillName, LineEdit::new(&self.text(&Text::SkillName))));
            }
            Act::CloseSkillForm => self.close_skill_form(),
            Act::AddSkill => self.add_skill(),
            Act::Actions => self.actions_menu(),
            Act::Leave(leave) => return self.leave(leave),
            Act::Save => return self.save(),
            Act::Discard => return self.discard(),
            Act::Delete => {
                if self.state.as_ref().and_then(AgentEditorState::delete_blocked).is_none() && !self.busy() {
                    if let Some(state) = self.state.as_mut() {
                        state.ask_delete();
                    }
                    // Cancel is focused: Enter alone never deletes.
                    self.modal = Some(Modal::Confirm { kind: Confirm::Delete, choice: 1 });
                }
            }
            Act::Nothing => {}
        }
        self.clamp();
        EditorAction::Stay
    }

    /// Make `change` to the draft (a refusal shows at its row).
    fn change(&mut self, change: Change) {
        let reset = change == Change::Reset;
        let removed = match &change {
            Change::RemoveSkill(name) => Some(name.clone()),
            _ => None,
        };
        // A text field's typing goes into the draft first, so the change
        // lands on it (and a reset replaces it).
        self.typed();
        let made = self.state.as_mut().is_some_and(|s| s.change(change));
        if made && reset {
            self.sync();
        }
        if made && removed.is_some() {
            self.clamp();
        }
    }

    fn close_skill_form(&mut self) {
        self.skill_form = None;
        if matches!(self.editing, Some(Area::SkillBody)) {
            self.editing = None;
        }
        if matches!(&self.typing, Some((Text::SkillName | Text::SkillDescription, _))) {
            self.typing = None;
        }
        if let Some(state) = self.state.as_mut() {
            state.clear_error_at(&Field::NewSkill);
        }
        self.select_item(&Item::AddSkill);
    }

    /// The form's Add: the skill goes into the draft and the form closes,
    /// or the model's refusal shows under it (the form keeps its text).
    fn add_skill(&mut self) {
        self.typing = None;
        self.editing = None;
        let Some(form) = self.skill_form.as_ref() else {
            return;
        };
        let name = form.name.trim().to_string();
        let change = Change::AddSkill { name: name.clone(), description: form.description.clone(), body: form.body.text() };
        if self.state.as_mut().is_some_and(|s| s.change(change)) {
            self.skill_form = None;
            self.select_item(&Item::Skill(name));
        }
    }

    /// Import: copy the skill at the typed path into the draft, or show
    /// why not under the field (a missing path, a manifest without a
    /// description).
    fn import_skill(&mut self) {
        self.typing = None;
        let text = self.import.trim().to_string();
        if text.is_empty() {
            return;
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let path = import_path(&text, home.as_deref(), &cwd);
        let before: Vec<String> = self.state.as_ref().map(|s| s.skills().into_iter().map(|s| s.name).collect()).unwrap_or_default();
        if self.state.as_mut().is_some_and(|s| s.change(Change::ImportSkill(path))) {
            self.import.clear();
            let added = self.state.as_ref().and_then(|s| s.skills().into_iter().find(|s| !before.contains(&s.name)));
            if let Some(skill) = added {
                self.select_item(&Item::Skill(skill.name));
            }
        }
    }

    fn entry(label: impl Into<String>, note: impl Into<String>, act: Act) -> Entry {
        Entry { label: label.into(), note: note.into(), act, inert: false, help: None }
    }

    fn list(&mut self, title: impl Into<String>, entries: Vec<Entry>) {
        let selected = entries.iter().position(|c| c.note == "current").unwrap_or(0);
        self.modal = Some(Modal::List { title: title.into(), entries, selected });
    }

    fn choices(choices: Vec<Choice>) -> Vec<Entry> {
        choices
            .into_iter()
            .map(|c| Self::entry(c.label, if c.current { "current" } else { "" }, Act::Change(c.change)))
            .collect()
    }

    fn open_pick(&mut self, pick: Pick) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let blocked = state.options_blocked();
        let (title, entries) = match pick {
            Pick::Runner => ("Preferred runner", Self::choices(state.runner_choices())),
            Pick::Model | Pick::Effort if blocked.is_some() => {
                let title = if pick == Pick::Model { "Model" } else { "Reasoning effort" };
                let explain = Entry { inert: true, help: blocked, ..Self::entry("Nothing to choose", "", Act::Nothing) };
                (title, vec![explain])
            }
            Pick::Model => {
                let mut entries = Self::choices(state.model_choices());
                entries.push(Entry {
                    help: Some("Type any model name the runner accepts; it's passed to the runner as typed.".into()),
                    ..Self::entry("Custom model…", "", Act::Type(Text::CustomModel))
                });
                ("Model", entries)
            }
            Pick::Effort => ("Reasoning effort", Self::choices(state.effort_choices())),
            Pick::Permissions => ("Permissions", Self::choices(state.permission_choices())),
        };
        self.list(title, entries);
    }

    fn skill_menu(&mut self, name: &str) {
        let entries = vec![
            Self::entry("Inspect", "i", Act::Inspect(name.into())),
            Self::entry("Open SKILL.md in your editor", "e", Act::Leave(Leave::Open(AgentFile::Skill(name.into())))),
            Self::entry("Remove skill…", "x", Act::AskRemove(name.into())),
            Self::entry("Add skill…", "n", Act::OpenSkillForm),
        ];
        self.modal = Some(Modal::List { title: name.to_string(), entries, selected: 0 });
    }

    /// The header's ⋮: Save, Discard, Edit YAML, Edit instructions.md,
    /// Duplicate, Reset to default, Delete (dimmed with its reason when the
    /// guards refuse it up front).
    fn actions_menu(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let dirty = state.is_dirty();
        let inert = |mut c: Entry, why: Option<String>| {
            if why.is_some() {
                c.inert = true;
                c.help = why;
            }
            c
        };
        let entries = vec![
            inert(Self::entry("Save changes", "^S", Act::Save), (!dirty).then(|| "There’s nothing to save.".into())),
            inert(Self::entry("Discard changes", "", Act::Discard), (!dirty).then(|| "There’s nothing to discard.".into())),
            Self::entry("Edit YAML", "y", Act::Leave(Leave::Open(AgentFile::Manifest))),
            Self::entry("Edit instructions.md", "e", Act::Leave(Leave::Open(AgentFile::Instructions))),
            Self::entry("Duplicate", "c", Act::Leave(Leave::Duplicate)),
            inert(Self::entry("Reset to default", "", Act::Change(Change::Reset)), state.reset_blocked()),
            inert(Self::entry("Delete agent…", "", Act::Delete), state.delete_blocked()),
        ];
        let title = state.name().to_string();
        self.modal = Some(Modal::List { title, entries, selected: 0 });
    }

    // --- save, discard, leaving -----------------------------------------------

    fn save(&mut self) -> EditorAction {
        self.typing = None;
        let texts = self.texts();
        let job = self.state.as_mut().and_then(|s| s.begin_save(texts));
        self.start_save(job)
    }

    fn start_save(&mut self, job: Option<SaveJob>) -> EditorAction {
        match job {
            Some(job) => {
                let ticket = self.job.begin(Some(&self.project));
                self.pending = Some((ticket, AgentWork::Save(job)));
                EditorAction::Work
            }
            None => EditorAction::Stay,
        }
    }

    /// Save for Settings' own prompt (a project switch): the prompt goes
    /// on once it lands. `Stay` when nothing starts.
    pub(crate) fn save_for_settings(&mut self) -> EditorAction {
        self.save()
    }

    /// Discard: the draft goes back to the files, read again when they
    /// changed under the last save.
    fn discard(&mut self) -> EditorAction {
        let reload = self.state.as_mut().is_some_and(AgentEditorState::discard);
        self.after_discard();
        if reload {
            self.reload();
            return EditorAction::Work;
        }
        EditorAction::Stay
    }

    fn after_discard(&mut self) {
        self.skill_form = None;
        self.custom_model = None;
        self.import.clear();
        self.sync();
        self.clamp();
    }

    /// Settings' prompt's Discard (leaving anyway).
    pub(crate) fn discard_for_settings(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.discard();
        }
        self.after_discard();
    }

    fn leave(&mut self, leave: Leave) -> EditorAction {
        self.typing = None;
        self.editing = None;
        // What's typed counts as an edit to leave with.
        self.typed();
        let go = match self.state.as_mut() {
            Some(state) => state.leave(leave),
            None => Some(leave),
        };
        match go {
            Some(leave) => self.go(leave),
            None => {
                self.prompt_choice = 0;
                EditorAction::Stay
            }
        }
    }

    /// Carry out a leave the guard let through.
    fn go(&mut self, leave: Leave) -> EditorAction {
        match leave {
            Leave::Back => EditorAction::Close(Back::Agent(self.name().to_string())),
            Leave::SharedInstructions => EditorAction::Close(Back::Shared),
            Leave::Workflow { name, status } => EditorAction::OpenWorkflow { name, status },
            Leave::Workflows => EditorAction::ShowWorkflows,
            Leave::Duplicate => {
                let Some(state) = self.state.as_mut() else {
                    return EditorAction::Stay;
                };
                let (name, copy) = (state.name().to_string(), state.duplicate_name());
                let Some(base) = state.begin_job() else {
                    return EditorAction::Stay;
                };
                let ticket = self.job.begin(Some(&self.project));
                self.pending = Some((ticket, AgentWork::Duplicate { base, name, copy }));
                EditorAction::Work
            }
            Leave::Open(file) => {
                let Some(state) = self.state.as_mut() else {
                    return EditorAction::Stay;
                };
                let path = state.file(&file);
                if !path.exists() {
                    state.open_failed(format!("Couldn’t find {}.", file.label()));
                    return EditorAction::Stay;
                }
                EditorAction::EditFile(path)
            }
        }
    }

    fn delete(&mut self) -> EditorAction {
        self.modal = None;
        let Some(state) = self.state.as_mut() else {
            return EditorAction::Stay;
        };
        if state.delete_blocked().is_some() {
            state.cancel_delete();
            return EditorAction::Stay;
        }
        let name = state.name().to_string();
        let Some(base) = state.begin_job() else {
            return EditorAction::Stay;
        };
        let ticket = self.job.begin(Some(&self.project));
        self.pending = Some((ticket, AgentWork::Delete { base, name }));
        EditorAction::Work
    }

    /// Work landed. A result for an older request (or another project) is
    /// dropped: `None`.
    pub(crate) fn landed(&mut self, ticket: &Ticket, done: AgentDone) -> Option<Landed> {
        match done {
            AgentDone::Loaded(config) => {
                if !self.load.complete(ticket, Ok(())) {
                    return None;
                }
                let Some(state) = self.state.as_mut() else {
                    match config {
                        Ok(config) if config.agent(&self.name).is_some() => {
                            let name = self.name.clone();
                            self.show(AgentEditorState::new(config, &name));
                        }
                        Ok(_) => self.load_error = Some(format!("There is no agent named {}.", self.name)),
                        Err(e) => self.load_error = Some(format!("Couldn’t read the agents: {e}")),
                    }
                    return Some(Landed::Stay);
                };
                // A draft with edits (or a save running) keeps them; its
                // save will be refused should it overwrite a hand edit.
                if state.is_dirty() || state.busy() {
                    return Some(Landed::Stay);
                }
                match config {
                    Ok(config) => {
                        if state.reloaded(config) {
                            self.sync();
                            self.clamp();
                            Some(Landed::Stay)
                        } else {
                            Some(Landed::Close {
                                wrote: false,
                                notice: Some(format!("Agent {} is no longer in the files.", self.name)),
                            })
                        }
                    }
                    Err(e) => {
                        state.job_failed(format!("Couldn’t read the agents again: {e}"));
                        Some(Landed::Stay)
                    }
                }
            }
            AgentDone::Saved(outcome) => {
                if !self.job.complete(ticket, Ok(())) {
                    return None;
                }
                let state = self.state.as_mut()?;
                let ok = outcome.is_ok();
                let then = state.saved(outcome);
                if ok {
                    self.name = self.state.as_ref().map(|s| s.name().to_string()).unwrap_or_default();
                    self.skill_form = None;
                    self.custom_model = None;
                    self.sync();
                    self.clamp();
                } else if self.state.as_ref().is_some_and(AgentEditorState::conflict) {
                    // Keep editing is focused: Enter alone never drops edits.
                    self.modal = Some(Modal::Confirm { kind: Confirm::Conflict, choice: 1 });
                }
                Some(match (then, ok) {
                    (Some(then), _) => Landed::Then(then),
                    (None, true) => Landed::Wrote,
                    (None, false) => Landed::Stay,
                })
            }
            AgentDone::Duplicated(result, copy) => {
                if !self.job.complete(ticket, Ok(())) {
                    return None;
                }
                match result {
                    Ok(config) => {
                        self.cursor = 0;
                        self.scroll = 0;
                        self.show(AgentEditorState::new(config, &copy));
                        Some(Landed::Wrote)
                    }
                    Err(e) => {
                        if let Some(state) = self.state.as_mut() {
                            state.job_failed(e);
                        }
                        Some(Landed::Stay)
                    }
                }
            }
            AgentDone::Deleted(result) => {
                if !self.job.complete(ticket, Ok(())) {
                    return None;
                }
                match result {
                    Ok(()) => Some(Landed::Close { wrote: true, notice: None }),
                    Err(e) => {
                        if let Some(state) = self.state.as_mut() {
                            state.job_failed(e);
                        }
                        Some(Landed::Stay)
                    }
                }
            }
        }
    }

    /// Carry on to `leave` after the leave prompt's save landed.
    pub(crate) fn then(&mut self, leave: Leave) -> EditorAction {
        self.go(leave)
    }

    // --- keys -----------------------------------------------------------------

    /// Paste into the field being typed or edited; false when none is.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        if let Some(area) = self.editing {
            if let Some(t) = self.area_text(area) {
                t.insert_str(text);
            }
            if area == Area::Instructions {
                self.typed();
            }
            return true;
        }
        let Some((field, mut edit)) = self.typing.take() else {
            return false;
        };
        for c in text.chars().filter(|c| !c.is_control()) {
            edit.insert(c);
        }
        let value = edit.text();
        self.typing = Some((field.clone(), edit));
        self.set_text(&field, value);
        true
    }

    /// Every key while the editor shows.
    pub(crate) fn handle_key(&mut self, k: KeyEvent, nav: Nav) -> EditorAction {
        self.follow_cursor = true;
        if self.state.as_ref().is_some_and(|s| s.prompt().is_some()) {
            return self.prompt_key(k);
        }
        if let Some(modal) = self.modal.clone() {
            return self.modal_key(modal, k, nav);
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl && k.code == KeyCode::Char('s') {
            return self.save();
        }
        if let Some(area) = self.editing {
            return self.area_key(area, k);
        }
        if self.typing.is_some() {
            return self.typing_key(k);
        }
        let items = self.items();
        let n = items.len();
        let current = items.get(self.cursor).cloned();
        match k.code {
            KeyCode::Esc => {
                if self.skill_form.is_some() && current.as_ref().is_some_and(|i| i.section() == Section::Skills) {
                    self.close_skill_form();
                    return EditorAction::Stay;
                }
                return self.leave(Leave::Back);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.step_section(k.code == KeyCode::Tab);
                return EditorAction::Stay;
            }
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(8),
            KeyCode::PageDown => self.cursor = (self.cursor + 8).min(n.saturating_sub(1)),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = n.saturating_sub(1),
            _ if nav.up => self.cursor = self.cursor.saturating_sub(1),
            _ if nav.down => self.cursor = (self.cursor + 1).min(n.saturating_sub(1)),
            _ => {}
        }
        let Some(item) = current else {
            return EditorAction::Stay;
        };
        if nav.activate {
            let act = self.primary(&item);
            return self.run(act);
        }
        if nav.up || nav.down || !k.modifiers.difference(KeyModifiers::SHIFT).is_empty() {
            return EditorAction::Stay;
        }
        let act = match (k.code, &item) {
            (KeyCode::Char('m') | KeyCode::Char('.'), Item::Skill(name)) => Act::SkillMenu(name.clone()),
            (KeyCode::Char('m') | KeyCode::Char('.'), _) => Act::Actions,
            (KeyCode::Char('i'), Item::Skill(name)) => Act::Inspect(name.clone()),
            (KeyCode::Char('e'), Item::Skill(name)) => Act::Leave(Leave::Open(AgentFile::Skill(name.clone()))),
            (KeyCode::Char('e'), _) => Act::Leave(Leave::Open(AgentFile::Instructions)),
            (KeyCode::Char('x') | KeyCode::Delete, Item::Skill(name)) => Act::AskRemove(name.clone()),
            (KeyCode::Char('n'), _) => Act::OpenSkillForm,
            (KeyCode::Char('y'), _) => Act::Leave(Leave::Open(AgentFile::Manifest)),
            (KeyCode::Char('c'), Item::Back | Item::Save | Item::Discard | Item::Actions) => Act::Leave(Leave::Duplicate),
            _ => return EditorAction::Stay,
        };
        self.run(act)
    }

    fn step_section(&mut self, forward: bool) {
        let items = self.items();
        let Some(here) = items.get(self.cursor).map(Item::section) else {
            return;
        };
        let starts: Vec<usize> =
            (0..items.len()).filter(|&i| i == 0 || items[i].section() != items[i - 1].section()).collect();
        let at = starts.iter().rposition(|&s| s <= self.cursor).unwrap_or(0);
        let next = if forward {
            starts.get(at + 1).copied()
        } else if items[starts[at]].section() == here && self.cursor > starts[at] {
            Some(starts[at])
        } else {
            at.checked_sub(1).map(|a| starts[a])
        };
        if let Some(i) = next {
            self.cursor = i;
        }
    }

    /// A key while a one-line field is typed: Enter, Esc, ↑↓ and Tab stop
    /// typing; the rest edit the field. Enter in the skill form's fields
    /// moves to the next one; in Import, imports; in Custom model, sets it.
    fn typing_key(&mut self, k: KeyEvent) -> EditorAction {
        let Some((field, mut edit)) = self.typing.take() else {
            return EditorAction::Stay;
        };
        match k.code {
            KeyCode::Enter => match field {
                Text::SkillName => return self.run(Act::Type(Text::SkillDescription)),
                Text::SkillDescription => return self.run(Act::Edit(Area::SkillBody)),
                Text::Import => self.import_skill(),
                Text::CustomModel => {
                    let model = self.custom_model.take().unwrap_or_default();
                    let model = model.trim().to_string();
                    let change = Change::Model((!model.is_empty()).then_some(model.clone()));
                    let made = self.state.as_mut().is_some_and(|s| s.change(change));
                    if !made {
                        // Keep the row and what was typed under the error.
                        self.custom_model = Some(model);
                    }
                    self.select_item(&Item::Model);
                }
                _ => {}
            },
            KeyCode::Esc => {
                if field == Text::CustomModel {
                    self.custom_model = None;
                    self.select_item(&Item::Model);
                }
            }
            KeyCode::Up => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Down => self.cursor = (self.cursor + 1).min(self.items().len().saturating_sub(1)),
            KeyCode::Tab | KeyCode::BackTab => self.step_section(k.code == KeyCode::Tab),
            _ => {
                edit.key(k);
                let value = edit.text();
                self.typing = Some((field.clone(), edit));
                self.set_text(&field, value);
            }
        }
        self.clamp();
        EditorAction::Stay
    }

    /// A key while a multi-line field is edited: every key types (Enter a
    /// line break); Esc stops, Tab stops and goes to the next section.
    fn area_key(&mut self, area: Area, k: KeyEvent) -> EditorAction {
        match k.code {
            KeyCode::Esc => {
                self.editing = None;
                return EditorAction::Stay;
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.editing = None;
                self.step_section(k.code == KeyCode::Tab);
                return EditorAction::Stay;
            }
            _ => {}
        }
        let page = self.area.height.max(1);
        if let (Some(chord), Some(text)) = (crate::keymap::chord_from_event(k), self.area_text(area)) {
            text.edit(chord, false, page);
        }
        if area == Area::Instructions {
            self.typed();
        }
        EditorAction::Stay
    }

    fn modal_key(&mut self, modal: Modal, k: KeyEvent, nav: Nav) -> EditorAction {
        match modal {
            Modal::List { title, entries, mut selected } => {
                let up = k.code == KeyCode::Up || (nav.up && !nav.activate);
                let down = k.code == KeyCode::Down || (nav.down && !nav.activate);
                match k.code {
                    KeyCode::Esc => {
                        self.modal = None;
                        return EditorAction::Stay;
                    }
                    KeyCode::Enter => return self.choose(&entries, selected),
                    _ if up => selected = selected.saturating_sub(1),
                    _ if down => selected = (selected + 1).min(entries.len().saturating_sub(1)),
                    _ if nav.activate => return self.choose(&entries, selected),
                    KeyCode::Char(c) => {
                        // An entry's own key runs it from the menu too.
                        let typed = c.to_string();
                        if let Some(i) = entries.iter().position(|e| e.note.split(' ').any(|k| k == typed)) {
                            return self.choose(&entries, i);
                        }
                    }
                    KeyCode::Delete => {
                        if let Some(i) = entries.iter().position(|e| e.note == "x") {
                            return self.choose(&entries, i);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::List { title, entries, selected });
                EditorAction::Stay
            }
            Modal::Confirm { kind, mut choice } => {
                let (yes, no) = match kind {
                    Confirm::Delete | Confirm::RemoveSkill(_) => (KeyCode::Char('y'), KeyCode::Char('n')),
                    Confirm::Conflict => (KeyCode::Char('r'), KeyCode::Char('k')),
                };
                match k.code {
                    KeyCode::Esc => return self.confirm(kind, false),
                    c if c == no => return self.confirm(kind, false),
                    c if c == yes => return self.confirm(kind, true),
                    KeyCode::Left | KeyCode::BackTab => choice = 0,
                    KeyCode::Right | KeyCode::Tab => choice = 1,
                    KeyCode::Enter | KeyCode::Char(' ') => return self.confirm(kind, choice == 0),
                    _ => {}
                }
                self.modal = Some(Modal::Confirm { kind, choice });
                EditorAction::Stay
            }
            Modal::Inspect { name, mut top } => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => {
                        self.modal = None;
                        return EditorAction::Stay;
                    }
                    KeyCode::Char('e') => {
                        self.modal = None;
                        return self.leave(Leave::Open(AgentFile::Skill(name)));
                    }
                    KeyCode::Char('x') | KeyCode::Delete => return self.run(Act::AskRemove(name)),
                    KeyCode::PageUp => top = top.saturating_sub(8),
                    KeyCode::PageDown => top += 8,
                    KeyCode::Home => top = 0,
                    _ if k.code == KeyCode::Up || nav.up => top = top.saturating_sub(1),
                    _ if k.code == KeyCode::Down || nav.down => top += 1,
                    _ => {}
                }
                self.modal = Some(Modal::Inspect { name, top });
                EditorAction::Stay
            }
        }
    }

    fn choose(&mut self, entries: &[Entry], index: usize) -> EditorAction {
        match entries.get(index) {
            Some(c) if !c.inert => {
                let act = c.act.clone();
                if act == Act::Type(Text::CustomModel) {
                    let seed = self.state.as_ref().and_then(|s| s.model()).unwrap_or_default().to_string();
                    self.custom_model = Some(seed);
                }
                self.run(act)
            }
            _ => {
                self.modal = None;
                EditorAction::Stay
            }
        }
    }

    fn confirm(&mut self, kind: Confirm, yes: bool) -> EditorAction {
        self.modal = None;
        match (kind, yes) {
            (Confirm::Delete, true) => self.delete(),
            (Confirm::RemoveSkill(name), true) => {
                self.change(Change::RemoveSkill(name));
                EditorAction::Stay
            }
            // Reload: the draft goes, the files are read again.
            (Confirm::Conflict, true) => self.discard(),
            (Confirm::Delete, false) => {
                if let Some(state) = self.state.as_mut() {
                    state.cancel_delete();
                }
                EditorAction::Stay
            }
            (_, false) => EditorAction::Stay,
        }
    }

    /// A key while the leave prompt asks: `s` / Save, `d` / Discard, Esc /
    /// `c` / Cancel; ←/→ and Tab move between the buttons, Enter presses.
    fn prompt_key(&mut self, k: KeyEvent) -> EditorAction {
        match k.code {
            KeyCode::Esc | KeyCode::Char('c') => self.prompt_choose(2),
            KeyCode::Char('s') => self.prompt_choose(0),
            KeyCode::Char('d') => self.prompt_choose(1),
            KeyCode::Left | KeyCode::BackTab => {
                self.prompt_choice = self.prompt_choice.saturating_sub(1);
                EditorAction::Stay
            }
            KeyCode::Right | KeyCode::Tab => {
                self.prompt_choice = (self.prompt_choice + 1).min(PROMPT_BUTTONS.len() - 1);
                EditorAction::Stay
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.prompt_choose(self.prompt_choice),
            _ => EditorAction::Stay,
        }
    }

    fn prompt_choose(&mut self, choice: usize) -> EditorAction {
        match choice {
            0 => {
                let texts = self.texts();
                let job = self.state.as_mut().and_then(|s| s.save_and_leave(texts));
                self.start_save(job)
            }
            1 => {
                let then = self.state.as_mut().and_then(AgentEditorState::discard_and_leave);
                self.after_discard();
                match then {
                    Some(leave) => self.go(leave),
                    None => EditorAction::Stay,
                }
            }
            _ => {
                if let Some(state) = self.state.as_mut() {
                    state.cancel_prompt();
                }
                EditorAction::Stay
            }
        }
    }

    /// A mouse event over the editor: a click selects a row (a second click
    /// does its Enter), answers a menu or a prompt; the wheel scrolls.
    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) -> EditorAction {
        let hit = self.hits.iter().find(|(r, _)| panel::in_rect(*r, m.column, m.row)).map(|(_, h)| *h);
        match m.kind {
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown if self.modal.is_none() => {
                let down = matches!(m.kind, MouseEventKind::ScrollDown);
                self.scroll = if down { self.scroll.saturating_add(1) } else { self.scroll.saturating_sub(1) };
                self.follow_cursor = false;
                EditorAction::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.state.as_ref().is_some_and(|s| s.prompt().is_some()) {
                    return match hit {
                        Some(Hit::Button(b)) => self.prompt_choose(b),
                        _ => EditorAction::Stay,
                    };
                }
                if let Some(modal) = self.modal.clone() {
                    return match (modal, hit) {
                        (Modal::List { entries, .. }, Some(Hit::Entry(i))) => self.choose(&entries, i),
                        (Modal::Confirm { kind, .. }, Some(Hit::Button(b))) => self.confirm(kind, b == 0),
                        (Modal::List { .. } | Modal::Inspect { .. }, None) => {
                            self.modal = None;
                            EditorAction::Stay
                        }
                        _ => EditorAction::Stay,
                    };
                }
                let Some(Hit::Item(i)) = hit else {
                    return EditorAction::Stay;
                };
                self.typing = None;
                self.editing = None;
                let again = self.cursor == i;
                self.cursor = i;
                let header = matches!(self.current(), Some(Item::Back | Item::Save | Item::Discard | Item::Actions));
                match self.current() {
                    Some(item) if again || header || matches!(item, Item::Text(_) | Item::Area(_)) => {
                        let act = self.primary(&item);
                        self.run(act)
                    }
                    _ => EditorAction::Stay,
                }
            }
            _ => EditorAction::Stay,
        }
    }

    // --- drawing --------------------------------------------------------------

    /// The hint band's keys for what has the keys.
    pub(crate) fn hints(&self, moves: &str) -> String {
        if self.state.as_ref().is_some_and(|s| s.prompt().is_some()) {
            return "s save · d discard · c/Esc cancel · ←→ choose · ⏎ press".into();
        }
        match &self.modal {
            Some(Modal::List { .. }) => return format!("{moves} choose · ⏎ pick · a key runs its entry · Esc close"),
            Some(Modal::Confirm { kind: Confirm::Conflict, .. }) => {
                return "r reload · k/Esc keep editing · ←→ choose · ⏎ press".into()
            }
            Some(Modal::Confirm { .. }) => return "y confirm · n/Esc cancel · ←→ choose · ⏎ press".into(),
            Some(Modal::Inspect { .. }) => return "↑↓ PgUp/PgDn scroll · e open in editor · x remove · Esc close".into(),
            None => {}
        }
        if self.editing.is_some() {
            return "Type · ⏎ new line · Esc done · Tab next section · ^S save".into();
        }
        if let Some((text, _)) = &self.typing {
            return match text {
                Text::Import => "Type a path (~/ for home) · ⏎ import · Esc done".into(),
                Text::CustomModel => "Type a model name · ⏎ set · Esc cancel".into(),
                Text::SkillName | Text::SkillDescription => "Type · ⏎ next field · Esc done · ^S save".into(),
                _ => "Type · ⏎/Esc done · ↑↓ done and move · ^S save".into(),
            };
        }
        let rest = match self.current() {
            Some(Item::Skill(_)) => "⏎ menu · i inspect · e open in editor · x remove · n add",
            Some(Item::Area(_)) => "⏎ edit here · e open in your editor",
            Some(Item::Text(Text::Import)) => "⏎ type a path, then ⏎ import",
            Some(Item::Text(Text::Name)) if self.state.as_ref().and_then(AgentEditorState::rename_blocked).is_some() => {
                "the name is locked"
            }
            Some(Item::Text(_)) => "⏎ edit",
            Some(Item::Assignment(_)) => "⏎ open the workflow",
            Some(Item::Back) => "⏎ back",
            _ => "⏎ choose",
        };
        format!("{moves} · {rest} · Tab section · m menu · ^S save · Esc back")
    }

    /// Draw the editor over `area` (both Settings columns).
    pub(crate) fn render(&mut self, f: &mut Frame, area: Rect, status: Option<&str>, moves: &str) {
        self.hits.clear();
        if area.width == 0 || area.height == 0 {
            return;
        }
        let hints = self.hints(moves);
        let mut body = area;
        let hint_row = Rect { y: area.bottom() - 1, height: 1, ..area };
        body.height -= 1;
        render_band(f, hint_row, &hints, Style::default().fg(palette().muted).bg(palette().status_bar));
        if let Some(status) = status.filter(|s| !s.is_empty()) {
            if body.height > 1 {
                let row = Rect { y: body.bottom() - 1, height: 1, ..body };
                body.height -= 1;
                render_band(f, row, status, Style::default().fg(palette().yellow));
            }
        }
        if body.height == 0 {
            return;
        }
        let inner = Rect { x: area.x + PAD, width: area.width.saturating_sub(PAD * 2), ..body };
        if inner.width == 0 {
            return;
        }
        let items = self.items();
        self.clamp();
        let header = Rect { height: 1, ..inner };
        self.render_header(f, header, &items);
        let content = Rect { y: inner.y + 2.min(inner.height), height: inner.height.saturating_sub(2), ..inner };
        if content.height == 0 {
            return;
        }
        let rows = self.rows(content.width as usize, content.height as usize);
        // Keep the focused row in view (the caret's line while editing),
        // then clamp to the content.
        let max = (rows.len() as u16).saturating_sub(content.height);
        if self.follow_cursor {
            let caret = rows.iter().position(|r| r.focus);
            let at = caret.or_else(|| rows.iter().position(|r| r.action == Some(self.cursor)));
            if let Some(at) = at {
                // The row's notes and error lines too, when they fit.
                // At least one line under it: the last line shows "↓ more"
                // while rows are clipped below.
                let end = if caret.is_some() {
                    1
                } else {
                    rows.iter().skip(at + 1).take_while(|r| r.action.is_none() && !r.focus).count().clamp(1, 3) as u16
                };
                let at = at as u16;
                if at < self.scroll {
                    self.scroll = at;
                } else if at + end >= self.scroll + content.height {
                    self.scroll = (at + end + 1).saturating_sub(content.height).min(at);
                }
            }
        }
        self.scroll = self.scroll.min(max);
        let clipped_below = self.scroll < max;
        for (i, row) in rows.iter().skip(self.scroll as usize).take(content.height as usize).enumerate() {
            let rect = Rect { y: content.y + i as u16, height: 1, ..content };
            if i as u16 + 1 == content.height && clipped_below {
                f.render_widget(Paragraph::new(Span::styled("↓ more", Style::default().fg(palette().muted))), rect);
                continue;
            }
            f.render_widget(Paragraph::new(row.line.clone()), rect);
            if let Some(a) = row.action {
                self.hits.push((Rect { x: area.x, width: area.width, ..rect }, Hit::Item(a)));
            }
        }
        if self.state.as_ref().is_some_and(|s| s.prompt().is_some()) {
            self.render_prompt(f, area);
        } else {
            self.render_modal(f, area);
        }
    }

    fn render_header(&mut self, f: &mut Frame, rect: Rect, items: &[Item]) {
        let state = self.state.as_ref();
        let dirty = state.is_some_and(AgentEditorState::is_dirty);
        let busy = state.is_some_and(AgentEditorState::busy);
        let selected = |item: &Item| items.get(self.cursor) == Some(item);
        let button = |label: &str, item: &Item, enabled: bool| {
            let style = if selected(item) {
                selected_style(true)
            } else if enabled {
                Style::default().fg(palette().white).bg(palette().selection)
            } else {
                Style::default().fg(palette().muted)
            };
            Span::styled(format!(" {label} "), style)
        };
        let bold = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
        let mut left = vec![button("← Back", &Item::Back, true), Span::raw("  "), Span::styled(self.name().to_string(), bold)];
        if let Some(badge) = state.and_then(AgentEditorState::badge) {
            left.push(Span::styled(format!(" {badge}"), Style::default().fg(palette().cyan)));
        }
        if dirty {
            left.push(Span::styled(" ●", Style::default().fg(palette().yellow)));
        }
        let save = button(if busy { "Saving…" } else { "Save changes" }, &Item::Save, dirty && !busy);
        let discard = button("Discard", &Item::Discard, dirty && !busy);
        let actions = button("⋮ Actions", &Item::Actions, state.is_some());
        let right = [save, Span::raw(" "), discard, Span::raw(" "), actions];
        let left_w: usize = left.iter().map(|s| panel::display_width(&s.content)).sum();
        let right_w: usize = right.iter().map(|s| panel::display_width(&s.content)).sum();
        let width = rect.width as usize;
        let gap = width.saturating_sub(left_w + right_w);
        let mut spans = left;
        if left_w + right_w > width {
            // Narrow: the name gives way first, then the buttons are cut.
            let room = width.saturating_sub(right_w + 10);
            spans.truncate(2);
            spans.push(Span::styled(panel::truncate_right(self.name(), room.max(1)), bold));
        }
        spans.push(Span::raw(" ".repeat(gap.max(1))));
        spans.extend(right);
        f.render_widget(Paragraph::new(Line::from(spans.clone())), rect);
        let mut x = rect.x;
        let targets = [
            (" ← Back ", Item::Back),
            (" Save changes ", Item::Save),
            (" Saving… ", Item::Save),
            (" Discard ", Item::Discard),
            (" ⋮ Actions ", Item::Actions),
        ];
        for span in &spans {
            let w = panel::display_width(&span.content) as u16;
            if let Some((_, item)) = targets.iter().find(|(t, _)| *t == span.content) {
                if let Some(i) = items.iter().position(|it| it == item) {
                    if x + w <= rect.right() {
                        self.hits.push((Rect { x, width: w, ..rect }, Hit::Item(i)));
                    }
                }
            }
            x = x.saturating_add(w);
        }
    }

    /// The page's rows at `width`, for a content `height` (an edited
    /// field's window fits it).
    fn rows(&mut self, width: usize, height: usize) -> Vec<Row> {
        let mut rows = Vec::new();
        let muted = Style::default().fg(palette().muted);
        let red = Style::default().fg(palette().red);
        let Some(state) = self.state.as_ref() else {
            match &self.load_error {
                Some(e) => wrapped(&mut rows, e, width, red),
                None => rows.push(Row::text(Line::from(Span::styled("Loading…", muted.add_modifier(Modifier::ITALIC))))),
            }
            return rows;
        };
        if let Some(e) = state.error_at(&Field::Save) {
            wrapped(&mut rows, e, width, red);
        } else if let Some(n) = state.notice() {
            wrapped(&mut rows, n, width, Style::default().fg(palette().yellow));
        }
        let items = self.items();
        let index = |item: &Item| items.iter().position(|it| it == item);
        // `Some(true)` on the cursor's row only (the rows draw no
        // unfocused selection).
        let selected = |item: &Item| (index(item) == Some(self.cursor)).then_some(true);
        let label_w = 18.min(width / 3).max(6);
        let note = |rows: &mut Vec<Row>, text: &str| note_rows(rows, text, width);
        let errors = |rows: &mut Vec<Row>, f: Field| error_rows(rows, state.error_at(&f), width);

        // Agent.
        heading(&mut rows, "Agent");
        let blocked = state.rename_blocked();
        let line = self.field_line("Name", &Text::Name, "", selected(&Item::Text(Text::Name)) == Some(true), label_w, width, blocked.is_some());
        rows.push(Row { action: index(&Item::Text(Text::Name)), ..Row::text(line) });
        if let Some(why) = &blocked {
            note(&mut rows, &format!("Locked: {why}"));
        }
        errors(&mut rows, Field::Name);
        let line = self.field_line(
            "Description",
            &Text::Description,
            DESCRIPTION_PLACEHOLDER,
            selected(&Item::Text(Text::Description)) == Some(true),
            label_w,
            width,
            false,
        );
        rows.push(Row { action: index(&Item::Text(Text::Description)), ..Row::text(line) });
        errors(&mut rows, Field::Description);

        // Instructions.
        rows.push(Row::blank());
        rows.push(Row::text(Line::from(vec![
            Span::styled("Instructions", Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD)),
            Span::styled("  Markdown", muted),
        ])));
        wrapped(&mut rows, INSTRUCTIONS_HELPER, width, muted);
        let editing = self.editing == Some(Area::Instructions);
        let at = index(&Item::Area(Area::Instructions));
        let on = selected(&Item::Area(Area::Instructions)) == Some(true);
        let window = if editing { height.saturating_sub(4).clamp(3, 30) } else { PREVIEW_LINES };
        area_rows(&mut rows, &self.instructions, &mut self.area, editing, on, at, window, width);
        let errors = |rows: &mut Vec<Row>, f: Field| error_rows(rows, self.state.as_ref().and_then(|s| s.error_at(&f)), width);
        errors(&mut rows, Field::Instructions);
        let state = self.state.as_ref().expect("loaded");
        rows.push(Row {
            action: index(&Item::EditInstructions),
            ..Row::text(add_line("Edit instructions.md in your editor", selected(&Item::EditInstructions), width))
        });
        rows.push(Row {
            action: index(&Item::SharedLink),
            ..Row::text(add_line(
                "Shared project instructions are included before these instructions ›",
                selected(&Item::SharedLink),
                width,
            ))
        });

        // Skills.
        rows.push(Row::blank());
        let skills = state.skills();
        heading(&mut rows, &format!("Skills ({})", skills.len()));
        if skills.is_empty() {
            rows.push(Row::text(Line::from(Span::styled(format!("  {SKILLS_EMPTY}"), Style::default().fg(palette().white)))));
            note(&mut rows, SKILLS_EMPTY_HELP);
        }
        for skill in &skills {
            let item = Item::Skill(skill.name.clone());
            let layout = match skill.layout {
                SkillLayout::Directory => "",
                SkillLayout::File => "  (flat .md)",
            };
            let text = format!("{}{layout}", skill.name);
            let line = plain_line(&text, selected(&item), width, Style::default().fg(palette().white));
            rows.push(Row { action: index(&item), ..Row::text(line) });
            match &skill.description {
                Some(d) => note(&mut rows, d),
                None => note(&mut rows, "No description: the runner may not load it."),
            }
            errors(&mut rows, Field::Skill(skill.name.clone()));
        }
        rows.push(Row { action: index(&Item::AddSkill), ..Row::text(add_line("+ Add skill", selected(&Item::AddSkill), width)) });
        if self.skill_form.is_some() {
            for (label, text, placeholder) in [
                ("  Skill name", Text::SkillName, "lowercase-with-dashes"),
                ("  Description", Text::SkillDescription, SKILL_DESCRIPTION_PLACEHOLDER),
            ] {
                let item = Item::Text(text.clone());
                let line = self.field_line(label, &text, placeholder, selected(&item) == Some(true), label_w, width, false);
                rows.push(Row { action: index(&item), ..Row::text(line) });
            }
            rows.push(Row::text(Line::from(Span::styled("    Instructions (SKILL.md body)", Style::default().fg(palette().gray)))));
            let editing = self.editing == Some(Area::SkillBody);
            let at = index(&Item::Area(Area::SkillBody));
            let on = selected(&Item::Area(Area::SkillBody)) == Some(true);
            let window = if editing { height.saturating_sub(6).clamp(3, 16) } else { 4 };
            if let Some(form) = self.skill_form.as_ref() {
                let mut view = if editing { self.area } else { AreaView::default() };
                area_rows(&mut rows, &form.body, &mut view, editing, on, at, window, width);
                if editing {
                    self.area = view;
                }
            }
            let button = |label: &str, item: &Item| {
                let style = if selected(item) == Some(true) {
                    selected_style(true)
                } else {
                    Style::default().fg(palette().gray).bg(palette().selection)
                };
                Span::styled(format!(" {label} "), style)
            };
            rows.push(Row {
                action: index(&Item::SkillAdd),
                ..Row::text(Line::from(vec![Span::raw("    "), button("Add skill", &Item::SkillAdd)]))
            });
            rows.push(Row {
                action: index(&Item::SkillCancel),
                ..Row::text(Line::from(vec![Span::raw("    "), button("Cancel", &Item::SkillCancel)]))
            });
        }
        let state = self.state.as_ref().expect("loaded");
        let item = Item::Text(Text::Import);
        let line = self.field_line("Import skill", &Text::Import, "a path: ~/skills/lint or lint.md", selected(&item) == Some(true), label_w, width, false);
        rows.push(Row { action: index(&item), ..Row::text(line) });
        note(&mut rows, IMPORT_HELP);
        error_rows(&mut rows, state.error_at(&Field::NewSkill), width);

        // Runner preferences.
        rows.push(Row::blank());
        heading(&mut rows, "Runner preferences");
        wrapped(&mut rows, RUNNER_HELPER, width, muted);
        let current = |choices: Vec<Choice>, fallback: &str| {
            choices.into_iter().find(|c| c.current).map_or_else(|| fallback.to_string(), |c| c.label)
        };
        let blocked = state.options_blocked();
        let picker = |rows: &mut Vec<Row>, label: &str, value: &str, item: Item, dim: bool| {
            let on = selected(&item) == Some(true);
            let mut line = picker_line(label, value, on, label_w, width);
            if dim && !on {
                line = Line::from(Span::styled(
                    format!("  {} {}", cell(label, label_w), panel::truncate_right(value, width.saturating_sub(label_w + 3))),
                    muted,
                ));
            }
            rows.push(Row { action: index(&item), ..Row::text(line) });
        };
        picker(&mut rows, "Preferred runner", &current(state.runner_choices(), "Project default"), Item::Runner, false);
        note(&mut rows, &state.runner_note());
        errors(&mut rows, Field::Runner);
        let model = current(state.model_choices(), state.model().unwrap_or("Use runner default"));
        picker(&mut rows, "Model", &model, Item::Model, blocked.is_some());
        if self.custom_model.is_some() {
            let item = Item::Text(Text::CustomModel);
            let line = self.field_line("  Custom model", &Text::CustomModel, "a model name", selected(&item) == Some(true), label_w, width, false);
            rows.push(Row { action: index(&item), ..Row::text(line) });
        }
        note(&mut rows, &with_source(&state.model_note(), state.model_in_effect().map(|(_, s)| s)));
        errors(&mut rows, Field::Model);
        picker(&mut rows, "Reasoning effort", &current(state.effort_choices(), "Use runner default"), Item::Effort, blocked.is_some());
        note(&mut rows, &state.effort_note());
        errors(&mut rows, Field::Effort);
        picker(&mut rows, "Permissions", &current(state.permission_choices(), "Use project settings"), Item::Permissions, false);
        note(&mut rows, &state.permissions_note());
        errors(&mut rows, Field::Permissions);

        // Workflow assignments.
        rows.push(Row::blank());
        heading(&mut rows, "Workflow assignments");
        wrapped(&mut rows, ASSIGNMENTS_HELPER, width, muted);
        let assignments = state.assignments();
        if assignments.is_empty() {
            note(&mut rows, NOT_ASSIGNED);
        }
        for (i, a) in assignments.iter().enumerate() {
            let item = Item::Assignment(i);
            let sel = selected(&item);
            let color = a.category.map_or(palette().foreground, crate::kanban::category_color);
            let marker = if sel.is_some() { "› " } else { "  " };
            let base = sel.map_or(Style::default(), selected_style);
            let text = format!("{} / {}", a.workflow, a.status_name);
            let room = width.saturating_sub(6);
            let mut spans = vec![
                Span::styled(marker, base),
                Span::styled("● ", base.fg(color)),
                Span::styled(panel::truncate_right(&text, room), if sel.is_some() { base } else { Style::default().fg(palette().white) }),
                Span::styled(" ›", if sel.is_some() { base } else { Style::default().fg(palette().cyan) }),
            ];
            super::workflows::pad_selected(&mut spans, sel, width);
            rows.push(Row { action: index(&item), ..Row::text(Line::from(spans)) });
        }
        rows.push(Row {
            action: index(&Item::ManageAssignments),
            ..Row::text(add_line("Manage assignments in Workflows ›", selected(&Item::ManageAssignments), width))
        });

        // Reset and Delete.
        rows.push(Row::blank());
        let reset = state.reset_blocked();
        let style = if reset.is_some() { muted } else { Style::default().fg(palette().cyan) };
        rows.push(Row { action: index(&Item::Reset), ..Row::text(plain_line("Reset to default", selected(&Item::Reset), width, style)) });
        if let Some(why) = reset {
            note(&mut rows, &why);
        }
        let delete = state.delete_blocked();
        let style = if delete.is_some() { muted } else { red };
        rows.push(Row { action: index(&Item::Delete), ..Row::text(plain_line("Delete agent…", selected(&Item::Delete), width, style)) });
        if let Some(why) = delete {
            note(&mut rows, &why);
        }
        rows.push(Row::blank());
        wrapped(&mut rows, APPLY_NOTE, width, muted);
        rows
    }

    /// A text field's row: its label, then the text (the line editor while
    /// typed), the placeholder when empty. A `locked` field's value is
    /// muted.
    #[allow(clippy::too_many_arguments)]
    fn field_line(
        &self,
        label: &str,
        text: &Text,
        placeholder: &str,
        selected: bool,
        label_w: usize,
        width: usize,
        locked: bool,
    ) -> Line<'static> {
        let marker = if selected { "› " } else { "  " };
        let head = format!("{marker}{}", cell(label, label_w));
        let head_style = if selected { selected_style(true) } else { Style::default().fg(palette().gray) };
        let mut spans = vec![Span::styled(head, head_style), Span::raw(" ")];
        let room = width.saturating_sub(label_w + 3);
        match &self.typing {
            Some((t, edit)) if t == text => spans.extend(edit.spans(room)),
            _ => {
                let value = self.text(text);
                if value.is_empty() {
                    spans.push(Span::styled(panel::truncate_right(placeholder, room), Style::default().fg(palette().muted)));
                } else {
                    let style = Style::default().fg(if locked { palette().gray } else { palette().white });
                    spans.push(Span::styled(panel::truncate_right(&value, room), style));
                }
            }
        }
        Line::from(spans)
    }

    fn render_modal(&mut self, f: &mut Frame, area: Rect) {
        let Some(modal) = self.modal.clone() else {
            return;
        };
        match modal {
            Modal::List { title, entries, selected } => {
                let help = entries.get(selected).and_then(|c| c.help.clone());
                let help_lines = help.map(|h| shelbi_app::overlay::error_log::wrap(&h, 40)).unwrap_or_default();
                let extra = if help_lines.is_empty() { 0 } else { help_lines.len() as u16 + 1 };
                let Some(rect) = WorkflowsPage::popup(area, 44, entries.len() as u16 + 2 + extra) else {
                    return;
                };
                let inner = WorkflowsPage::frame(f, rect, &format!(" {title} "));
                let w = inner.width as usize;
                let room = inner.height.saturating_sub(extra) as usize;
                let skip = (selected + 1).saturating_sub(room.max(1));
                for (row, (i, c)) in entries.iter().enumerate().skip(skip).take(room).enumerate() {
                    let r = Rect { y: inner.y + row as u16, height: 1, ..inner };
                    let left = format!(" {}", c.label);
                    let right = format!("{} ", c.note);
                    let gap = w.saturating_sub(panel::display_width(&left) + panel::display_width(&right));
                    let text = panel::truncate_right(&format!("{left}{}{right}", " ".repeat(gap)), w);
                    let style = if i == selected {
                        selected_style(true)
                    } else if c.inert {
                        Style::default().fg(palette().muted).bg(palette().background)
                    } else {
                        Style::default().fg(palette().foreground).bg(palette().background)
                    };
                    f.render_widget(Paragraph::new(Span::styled(text, style)), r);
                    self.hits.push((r, Hit::Entry(i)));
                }
                let first = inner.y + room as u16 + 1;
                for (y, line) in (first..inner.bottom()).zip(help_lines) {
                    f.render_widget(
                        Paragraph::new(Span::styled(format!(" {line}"), Style::default().fg(palette().muted))),
                        Rect { y, height: 1, ..inner },
                    );
                }
            }
            Modal::Confirm { kind, choice } => {
                let (title, message, buttons) = match &kind {
                    Confirm::Delete => (" Delete agent ", format!("{} {DELETE_MESSAGE}", delete_title(self.name())), [" Delete ", " Cancel "]),
                    Confirm::RemoveSkill(name) => (
                        " Remove skill ",
                        format!("Remove skill {name}? Its files are deleted when you save; Discard brings it back."),
                        [" Remove ", " Cancel "],
                    ),
                    Confirm::Conflict => (" Changed on disk ", CONFLICT_MESSAGE.to_string(), [" Reload ", " Keep editing "]),
                };
                self.render_buttons(f, area, title, &message, &buttons, choice);
            }
            Modal::Inspect { name, top } => self.render_inspect(f, area, &name, top),
        }
    }

    /// A skill as the draft holds it: its description, layout and files,
    /// then its SKILL.md, scrolling.
    fn render_inspect(&mut self, f: &mut Frame, area: Rect, name: &str, top: usize) {
        let Some(skill) = self.state.as_ref().and_then(|s| s.skill(name)) else {
            self.modal = None;
            return;
        };
        let width = area.width.saturating_sub(4).clamp(12, 90);
        let Some(rect) = WorkflowsPage::popup(area, width, area.height.saturating_sub(2)) else {
            return;
        };
        let inner = WorkflowsPage::frame(f, rect, &format!(" Skill {name} "));
        let w = inner.width.saturating_sub(2) as usize;
        let muted = Style::default().fg(palette().muted);
        let mut lines: Vec<Line> = Vec::new();
        let mut push = |text: &str, style: Style| {
            for l in shelbi_app::overlay::error_log::wrap(text, w.max(1)) {
                lines.push(Line::from(Span::styled(l, style)));
            }
        };
        push(skill.description.as_deref().unwrap_or("No description."), Style::default().fg(palette().white));
        let layout = match skill.layout {
            SkillLayout::Directory => "a folder",
            SkillLayout::File => "a flat .md file",
        };
        let saved = if skill.unsaved { " · not saved yet" } else { "" };
        push(&format!("{} as {layout}{saved}", skill.file), muted);
        for file in skill.files.iter().filter(|p| **p != skill.file) {
            push(&format!("  {file}"), muted);
        }
        push("", muted);
        for line in skill.text.lines() {
            let line: String = line.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
            push(&line, Style::default().fg(palette().foreground));
        }
        let room = inner.height as usize;
        let top = top.min(lines.len().saturating_sub(room));
        if let Some(Modal::Inspect { top: t, .. }) = self.modal.as_mut() {
            *t = top;
        }
        let shown: Vec<Line> = lines.into_iter().skip(top).take(room).collect();
        f.render_widget(Paragraph::new(shown), Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), ..inner });
    }

    fn render_prompt(&mut self, f: &mut Frame, area: Rect) {
        let message = format!("{} {LEAVE_MESSAGE}", leave_title(self.name()));
        let buttons: Vec<String> = PROMPT_BUTTONS.iter().map(|b| format!(" {b} ")).collect();
        let buttons: Vec<&str> = buttons.iter().map(String::as_str).collect();
        self.render_buttons(f, area, " Unsaved changes ", &message, &buttons, self.prompt_choice);
    }

    fn render_buttons(&mut self, f: &mut Frame, area: Rect, title: &str, message: &str, buttons: &[&str], choice: usize) {
        let lines = shelbi_app::overlay::error_log::wrap(message, 44);
        let Some(rect) = WorkflowsPage::popup(area, 50, lines.len() as u16 + 4) else {
            return;
        };
        let inner = WorkflowsPage::frame(f, rect, title);
        for (i, line) in lines.iter().enumerate() {
            let y = inner.y + i as u16;
            if y + 1 >= inner.bottom() {
                break;
            }
            f.render_widget(
                Paragraph::new(Span::styled(format!(" {line}"), Style::default().fg(palette().foreground))),
                Rect { y, height: 1, ..inner },
            );
        }
        let y = inner.bottom().saturating_sub(1);
        let mut x = inner.x + 1;
        for (i, label) in buttons.iter().enumerate() {
            let w = panel::display_width(label) as u16;
            if x + w > inner.right() {
                break;
            }
            let style = if i == choice { selected_style(true) } else { Style::default().fg(palette().gray).bg(palette().selection) };
            let r = Rect { x, y, width: w, height: 1 };
            f.render_widget(Paragraph::new(Span::styled(label.to_string(), style)), r);
            self.hits.push((r, Hit::Button(i)));
            x += w + 1;
        }
    }
}

/// A muted note under a row, each wrapped line indented.
fn note_rows(rows: &mut Vec<Row>, text: &str, width: usize) {
    for line in shelbi_app::overlay::error_log::wrap(text, width.saturating_sub(4).max(1)) {
        rows.push(Row::text(Line::from(Span::styled(format!("    {line}"), Style::default().fg(palette().muted)))));
    }
}

/// A Model note with where the model comes from, when it's set.
fn with_source(note: &str, source: Option<Source>) -> String {
    match source {
        Some(Source::Agent) => format!("{} (this agent’s setting).", note.trim_end_matches('.')),
        _ => note.to_string(),
    }
}

/// A multi-line field's rows: while `editing`, a `window` of its wrapped
/// lines kept around the caret (the caret's row marked as the page's
/// focus); otherwise a preview of its first `window` lines and how many
/// more there are. Every row is item `at`'s, so a click on it selects it.
#[allow(clippy::too_many_arguments)]
fn area_rows(
    rows: &mut Vec<Row>,
    text: &TextArea,
    view: &mut AreaView,
    editing: bool,
    selected: bool,
    at: Option<usize>,
    window: usize,
    width: usize,
) {
    let text_w = width.saturating_sub(2 + AREA_PREFIX.chars().count()).max(1);
    let (wrapped_rows, (caret_row, caret_col)) = text.wrapped(text_w);
    let rule = Style::default().fg(if editing || selected { palette().cyan } else { palette().ansi_dark_gray });
    let style = Style::default().fg(palette().foreground);
    let lines = text.lines();
    let window = if editing { window.min(wrapped_rows.len() + 1).max(3.min(window)) } else { window.min(wrapped_rows.len()) };
    if editing {
        view.height = window;
        if caret_row < view.top {
            view.top = caret_row;
        } else if caret_row >= view.top + window {
            view.top = caret_row + 1 - window;
        }
        view.top = view.top.min(wrapped_rows.len().saturating_sub(1));
    }
    let top = if editing { view.top } else { 0 };
    let marker = if selected && !editing { "› " } else { "  " };
    for v in top..top + window.max(1) {
        let mut spans = vec![Span::styled(marker, Style::default().fg(palette().cyan)), Span::styled(AREA_PREFIX, rule)];
        let mut focus = false;
        if let Some(&(line, start)) = wrapped_rows.get(v) {
            let chars: Vec<char> =
                lines[line].iter().skip(start).take(text_w).map(|c| if c.is_control() { ' ' } else { *c }).collect();
            if editing && v == caret_row {
                focus = true;
                let before: String = chars.iter().take(caret_col).collect();
                let caret: String = chars.get(caret_col).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
                let after: String = chars.iter().skip(caret_col + 1).collect();
                spans.push(Span::styled(before, style));
                spans.push(Span::styled(caret, Style::default().fg(palette().canvas).bg(palette().white)));
                spans.push(Span::styled(after, style));
            } else {
                spans.push(Span::styled(chars.into_iter().collect::<String>(), style));
            }
        }
        rows.push(Row { action: at, focus, ..Row::text(Line::from(spans)) });
    }
    let below = wrapped_rows.len().saturating_sub(top + window);
    let more = if editing {
        let above = top;
        match (above, below) {
            (0, 0) => None,
            (a, b) => Some(format!("{a} lines above · {b} below")),
        }
    } else if below > 0 {
        Some(format!("… {below} more lines · ⏎ edit"))
    } else if wrapped_rows.len() <= 1 && text.is_blank() {
        Some("Empty · ⏎ edit".into())
    } else {
        None
    };
    if let Some(more) = more {
        rows.push(Row { action: at, ..Row::text(Line::from(Span::styled(format!("    {more}"), Style::default().fg(palette().muted)))) });
    }
}

#[cfg(test)]
mod tests;
