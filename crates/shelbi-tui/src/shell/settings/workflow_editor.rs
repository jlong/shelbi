//! The workflow editor in the TUI (`tui-workflow-editor-form`), after the
//! desktop app's (`desktop/src/views/workflow_editor.rs`) over the same
//! shared draft (`shelbi_app::settings::workflow_editor`). Settings ▸
//! Workflows opens it (Enter or `e` on a workflow, Edit in its menu), and an
//! agent's workflow-status assignment opens it on that status's row. Like
//! the desktop's, it takes the settings sidebar's column too while it shows.
//!
//! **The header** (never scrolls): Back, the workflow's name with its
//! `default` badge, then Save changes (dimmed while there's nothing to save,
//! `Saving…` while a save runs), Discard (dimmed without unsaved edits) and
//! the `⋮ Actions` menu (Edit YAML, Duplicate, Set as default, Manage project
//! statuses, Delete). The desktop's Builder | Form control joins it once the
//! TUI has a Builder ([`VIEWS`]); both would edit this one draft.
//!
//! **The Form** scrolls under it, one focusable row per control, in the
//! desktop's sections: Workflow (Name, Description, Initial status);
//! Statuses (each status with its owner, agent and workspace tags; Add
//! status; Manage project statuses); Transitions (each `from → to`, its
//! actions in order, Add action, Details and, open, the Details fields:
//! Target branch, the Run commands one per row, Ready check, Ready timeout,
//! URL; Add transition); Git (Base branch, Branch name, Merge strategy);
//! Review (Review mode, and the recipe's fields when it runs the branch);
//! Automation (Zen settings, Required task fields); Delete workflow.
//!
//! Keys (no mouse needed): the user's up / down (and the arrows) walk the
//! rows, PgUp / PgDn / Home / End jump, Tab / Shift+Tab go to the next or
//! previous section. Enter does a row's main thing: a text field starts
//! typing (each key goes to the draft at once, as the desktop's fields do;
//! Enter, Esc or ↑↓ stop typing), a picker opens, a status or a transition
//! opens its menu. `m` opens the row's menu (else the workflow's actions),
//! every menu entry has its own key: `o` owner, `a` agent, `t` tags (a
//! status), `f` / `t` from / to, `d` details (a transition), `n` adds in the
//! section (a status, an action or a command, a transition), `x` / Delete
//! removes, Shift/Alt+↑↓ reorders a status, an action or a command. Ctrl+S
//! saves. Esc goes Back.
//!
//! Every control edits the shared draft through `shelbi_state`'s
//! `WorkflowConfig`; nothing is written before Save. A change the model
//! refuses shows its message under its row, and a text field keeps what was
//! typed (refused text included) until it's corrected, a save, a discard or
//! a reload. Save runs off the input loop ([`EditorWork`]); a failed save
//! keeps the draft and says why. Back, Esc, Manage project statuses,
//! Duplicate and Edit YAML ask Save / Discard / Cancel first when there are
//! unsaved edits (the shared `Leave` guard); a project switch asks the
//! same through Settings' own prompt. A save refused because the files
//! changed on disk asks Reload (the draft goes) or Keep editing, never
//! overwriting. Delete asks first; the model refuses it (and a rename) while
//! open issues use the workflow.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};
use shelbi_app::settings::workflow_editor::{
    action_label, owner_label, run_delete, run_duplicate, run_save, strategy_label, zen_custom_help, Change,
    EditorState, Field, GitForm, Leave, Mode, ProjectGit, ReviewMode, SaveJob, SaveOutcome, TextForm,
    ACTIONS, ALL_STATUSES_ADDED, DESCRIPTION_PLACEHOLDER, DETAILS_READY, DETAILS_TARGET, DETAILS_TIMEOUT,
    DETAILS_URL, GIT_HELP, PARAMS_HELP, REVIEW_FIELDS, RUN_HELP, STRATEGIES, TAGS_PLACEHOLDER, TRANSITIONS_HELP,
    ZEN_PROJECT_HELP,
};
use shelbi_app::settings::workflows::Target;
use shelbi_app::settings::{Job, Ticket};
use shelbi_core::{Owner, StatusCategory};
use shelbi_state::workflow_config::WorkflowConfig;

use super::workflows::{add_line, cell, error_rows, pad_selected, plain_line, EditFile, LineEdit, Nav, WorkflowsPage};
use super::{heading, render_band, selected_style, wrapped, Row, PROMPT_BUTTONS};
use crate::panel::{self, PAD};
use crate::theme::palette;

/// The views the TUI's editor offers over the draft. The Builder (its own
/// task) joins here; with two, the header shows the Builder | Form control.
pub(crate) const VIEWS: &[Mode] = &[Mode::Form];

/// A text field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Text {
    Name,
    Description,
    /// Status `id`'s Workspace tags.
    Tags(String),
    BaseBranch,
    Branch,
    /// A Run the branch field, in `REVIEW_FIELDS`' order.
    Review(usize),
    Params,
    /// The open Details panel's fields.
    Target,
    /// Its Run command at this line.
    Run(usize),
    Ready,
    Timeout,
    Url,
}

/// A row the keys stop on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Back,
    Save,
    Discard,
    Actions,
    Text(Text),
    InitialStatus,
    Status(String),
    AddStatus,
    ManageStatuses,
    Transition(usize),
    /// Transition `.0`'s action at slot `.1`.
    Action(usize, usize),
    AddAction(usize),
    Details(usize),
    /// Add a Run command to the open Details.
    AddRun,
    AddTransition,
    MergeStrategy,
    ReviewMode,
    Zen,
    Delete,
}

/// The sections Tab steps through, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Header,
    Workflow,
    Statuses,
    Transitions,
    Git,
    Review,
    Automation,
    Manage,
}

impl Item {
    fn section(&self) -> Section {
        match self {
            Item::Back | Item::Save | Item::Discard | Item::Actions => Section::Header,
            Item::Text(Text::Name | Text::Description) | Item::InitialStatus => Section::Workflow,
            Item::Text(Text::Tags(_)) | Item::Status(_) | Item::AddStatus | Item::ManageStatuses => Section::Statuses,
            Item::Transition(_)
            | Item::Action(..)
            | Item::AddAction(_)
            | Item::Details(_)
            | Item::AddRun
            | Item::AddTransition
            | Item::Text(Text::Target | Text::Run(_) | Text::Ready | Text::Timeout | Text::Url) => Section::Transitions,
            Item::Text(Text::BaseBranch | Text::Branch) | Item::MergeStrategy => Section::Git,
            Item::ReviewMode | Item::Text(Text::Review(_)) => Section::Review,
            Item::Zen | Item::Text(Text::Params) => Section::Automation,
            Item::Delete => Section::Manage,
        }
    }
}

/// A picker the editor opens.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Pick {
    InitialStatus,
    AddStatus,
    Owner(String),
    Agent(String),
    From(usize),
    To(usize),
    SetAction(usize, usize),
    AddAction(usize),
    MergeStrategy,
    ReviewMode,
    Zen,
}

/// One thing a row, a menu entry or a picker's choice does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    Change(Change),
    Type(Text),
    Pick(Pick),
    StatusMenu(String),
    TransitionMenu(usize),
    Actions,
    ToggleDetails(usize),
    AddRun,
    RemoveRun(usize),
    MoveRun(usize, bool),
    MoveStatus(String, bool),
    MoveAction(usize, usize, bool),
    Leave(Leave),
    Save,
    Discard,
    Delete,
    /// Close the menu (an entry that only explains).
    Nothing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    label: String,
    /// Its key in a menu, or `current` in a picker.
    note: String,
    act: Act,
    /// Dimmed: it can't run (choosing it closes the menu).
    inert: bool,
    help: Option<String>,
}

/// What asks over the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Confirm {
    /// Delete the workflow? Delete / Cancel.
    Delete,
    /// The save found the files changed on disk: Reload / Keep editing.
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Modal {
    List { title: String, choices: Vec<Choice>, selected: usize },
    Confirm { kind: Confirm, choice: usize },
}

/// What a click hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Item(usize),
    Choice(usize),
    Button(usize),
}

/// What the editor asks of Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EditorAction {
    Stay,
    /// Start the work waiting in [`WorkflowEditorPage::take_work`].
    Work,
    /// Back to Settings ▸ Workflows.
    Close,
    /// Back to the Workflows page's Statuses.
    ManageStatuses,
    /// Open the workflow's file in the user's editor.
    EditYaml(EditFile),
}

/// Background work the shell runs off the input loop
/// ([`run_work`]), and hands back under the editor's ticket.
pub(crate) enum EditorWork {
    /// Read `project`'s workflow config (and its `git:` defaults).
    Load { project: String },
    Save(SaveJob),
    Duplicate { base: WorkflowConfig, name: String, copy: String },
    Delete { base: WorkflowConfig, name: String },
}

/// What [`EditorWork`] did.
pub(crate) enum EditorDone {
    Loaded(Result<WorkflowConfig, String>, ProjectGit),
    Saved(SaveOutcome),
    Duplicated(Result<WorkflowConfig, String>, String),
    Deleted(Result<(), String>),
}

/// Run `work` (blocking: off the input loop). `read` reads a project's
/// workflow config (the shell's Workflows reader, which tests replace).
pub(crate) fn run_work(work: EditorWork, read: &dyn Fn(&str) -> Result<WorkflowConfig, String>) -> EditorDone {
    match work {
        EditorWork::Load { project } => {
            let config = read(&project);
            // A test's shell has no SHELBI_HOME unless it set one: the
            // defaults, rather than the real ~/.shelbi.
            let git = if cfg!(test) && std::env::var_os("SHELBI_HOME").is_none() {
                ProjectGit::default()
            } else {
                ProjectGit::load(&project)
            };
            EditorDone::Loaded(config, git)
        }
        EditorWork::Save(job) => EditorDone::Saved(run_save(job)),
        EditorWork::Duplicate { base, name, copy } => {
            let done = run_duplicate(base, &name, &copy);
            EditorDone::Duplicated(done, copy)
        }
        EditorWork::Delete { base, name } => EditorDone::Deleted(run_delete(base, &name)),
    }
}

/// What landing work asks of Settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Landed {
    /// Stay in the editor.
    Stay,
    /// Files were written: refresh the board and the Workflows index.
    Wrote,
    /// Close the editor (Back, a delete, the workflow gone from the files).
    Close { wrote: bool, notice: Option<String> },
    /// Carry on to this leave (the leave prompt's Save landed).
    Then(Leave),
}

/// The editor's state: the shared draft, plus the TUI's rows, cursor, text
/// buffers and modal, and its background work's ticket.
pub(crate) struct WorkflowEditorPage {
    project: String,
    /// The workflow being opened or edited.
    name: String,
    state: Option<EditorState>,
    load_error: Option<String>,
    /// The editor's reads, and its save / duplicate / delete.
    load: Job<()>,
    job: Job<()>,
    pending: Option<(Ticket, EditorWork)>,
    /// What the text fields hold (possibly refused text) and the open
    /// Details' Run commands, one per line.
    texts: TextForm,
    run: Vec<String>,
    details: Option<usize>,
    /// The field being typed.
    typing: Option<(Text, LineEdit)>,
    modal: Option<Modal>,
    prompt_choice: usize,
    cursor: usize,
    scroll: u16,
    /// Scroll the cursor's row into view at the next draw (the wheel turns
    /// it off until a key moves the cursor).
    follow_cursor: bool,
    /// Put the cursor on this status once the draft loads (an Agents link).
    focus_status: Option<String>,
    hits: Vec<(Rect, Hit)>,
}

impl WorkflowEditorPage {
    /// Edit workflow `name` of `project`: at once from `config` when the
    /// Workflows page has it loaded. Either way a read starts (the shell
    /// takes it, [`Self::take_work`]): it brings the project's `git:`
    /// defaults, and the files as they are now.
    pub(crate) fn open(project: &str, name: &str, config: Option<&WorkflowConfig>, focus_status: Option<String>) -> Self {
        let mut page = WorkflowEditorPage {
            project: project.to_string(),
            name: name.to_string(),
            state: None,
            load_error: None,
            load: Job::default(),
            job: Job::default(),
            pending: None,
            texts: TextForm::default(),
            run: Vec::new(),
            details: None,
            typing: None,
            modal: None,
            prompt_choice: 0,
            cursor: 0,
            scroll: 0,
            follow_cursor: true,
            focus_status,
            hits: Vec::new(),
        };
        if let Some(config) = config.filter(|c| c.workflow(name).is_some()) {
            page.show(EditorState::new(config.clone(), name));
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
    pub(crate) fn state(&self) -> Option<&EditorState> {
        self.state.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn cursor_item(&self) -> Option<Item> {
        self.items().into_iter().nth(self.cursor)
    }

    #[cfg(test)]
    pub(crate) fn select(&mut self, item: &Item) -> bool {
        match self.items().iter().position(|it| it == item) {
            Some(i) => {
                self.cursor = i;
                true
            }
            None => false,
        }
    }

    #[cfg(test)]
    pub(crate) fn typed_text(&self, text: &Text) -> String {
        self.text(text)
    }

    #[cfg(test)]
    pub(crate) fn modal_title(&self) -> Option<String> {
        match &self.modal {
            Some(Modal::List { title, .. }) => Some(title.clone()),
            Some(Modal::Confirm { kind: Confirm::Delete, .. }) => Some("Delete workflow".into()),
            Some(Modal::Confirm { kind: Confirm::Conflict, .. }) => Some("Changed on disk".into()),
            None => None,
        }
    }

    pub(crate) fn is_dirty(&self) -> bool {
        self.state.as_ref().is_some_and(EditorState::is_dirty)
    }

    pub(crate) fn busy(&self) -> bool {
        self.state.as_ref().is_some_and(EditorState::busy)
    }

    /// The work the shell should start now, if any.
    pub(crate) fn take_work(&mut self) -> Option<(Ticket, EditorWork)> {
        self.pending.take()
    }

    /// Read the files again (the editor came back from the user's editor,
    /// Settings was reopened). The draft takes them only if it holds no
    /// edits when they land.
    pub(crate) fn reload(&mut self) {
        let ticket = self.load.begin(Some(&self.project));
        self.pending = Some((ticket, EditorWork::Load { project: self.project.clone() }));
    }

    /// Edit `state` from here on: every field shows it.
    fn show(&mut self, mut state: EditorState) {
        state.mode = VIEWS[0];
        if let Some(git) = self.state.as_ref().map(|s| s.project_git().clone()) {
            state.set_project_git(git);
        }
        self.name = state.name().to_string();
        self.state = Some(state);
        self.details = None;
        self.typing = None;
        self.modal = None;
        self.load_error = None;
        self.sync(true);
        if let Some(id) = self.focus_status.take() {
            if !self.select_item(&Item::Status(id.clone())) {
                self.focus_status = Some(id);
            }
        }
        self.clamp();
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

    /// Put the draft's text in the fields: all of them (`force`), or only
    /// a field for a status that has none yet (and none for one gone).
    /// Refused text a field holds stays until a forced sync.
    fn sync(&mut self, force: bool) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let form = state.text_form(self.details);
        if force {
            self.run = form.details.as_ref().map(|(_, d)| d.run.lines().map(str::to_string).collect()).unwrap_or_default();
            self.texts = form;
            self.typing = None;
            return;
        }
        let tags = form
            .tags
            .into_iter()
            .map(|(id, text)| match self.texts.tags.iter().find(|(t, _)| *t == id) {
                Some(kept) => kept.clone(),
                None => (id, text),
            })
            .collect();
        self.texts.tags = tags;
        if self.details.is_none() {
            self.texts.details = None;
            self.run.clear();
        }
    }

    /// The fields as a form: the open Details' Run commands joined.
    fn form(&self) -> TextForm {
        let mut form = self.texts.clone();
        if let Some((_, details)) = form.details.as_mut() {
            details.run = self.run.join("\n");
        }
        form
    }

    fn text(&self, text: &Text) -> String {
        let t = &self.texts;
        let details = t.details.as_ref().map(|(_, d)| d);
        match text {
            Text::Name => self.state.as_ref().map(|s| s.typed_name().to_string()).unwrap_or_default(),
            Text::Description => t.description.clone(),
            Text::Tags(id) => t.tags.iter().find(|(t, _)| t == id).map(|(_, s)| s.clone()).unwrap_or_default(),
            Text::BaseBranch => t.base_branch.clone(),
            Text::Branch => t.branch.clone(),
            Text::Review(i) => t.review.field(*i).to_string(),
            Text::Params => t.params.clone(),
            Text::Target => details.map(|d| d.target.clone()).unwrap_or_default(),
            Text::Run(line) => self.run.get(*line).cloned().unwrap_or_default(),
            Text::Ready => details.map(|d| d.ready.clone()).unwrap_or_default(),
            Text::Timeout => details.map(|d| d.ready_timeout.clone()).unwrap_or_default(),
            Text::Url => details.map(|d| d.url.clone()).unwrap_or_default(),
        }
    }

    /// Field `text` now holds `value`: into the buffer, then the draft
    /// (the name waits for Save).
    fn set_text(&mut self, text: &Text, value: String) {
        let t = &mut self.texts;
        match text {
            Text::Name => {
                if let Some(state) = self.state.as_mut() {
                    state.type_name(&value);
                }
                return;
            }
            Text::Description => t.description = value,
            Text::Tags(id) => match t.tags.iter_mut().find(|(t, _)| t == id) {
                Some((_, s)) => *s = value,
                None => t.tags.push((id.clone(), value)),
            },
            Text::BaseBranch => t.base_branch = value,
            Text::Branch => t.branch = value,
            Text::Review(i) => *t.review.field_mut(*i) = value,
            Text::Params => t.params = value,
            Text::Run(line) => {
                if let Some(l) = self.run.get_mut(*line) {
                    *l = value;
                }
            }
            Text::Target | Text::Ready | Text::Timeout | Text::Url => {
                if let Some((_, d)) = t.details.as_mut() {
                    let slot = match text {
                        Text::Target => &mut d.target,
                        Text::Ready => &mut d.ready,
                        Text::Timeout => &mut d.ready_timeout,
                        _ => &mut d.url,
                    };
                    *slot = value;
                }
            }
        }
        self.typed();
    }

    /// A text field changed: the draft takes each field that differs.
    fn typed(&mut self) {
        let form = self.form();
        if let Some(state) = self.state.as_mut() {
            state.type_texts(&form);
        }
    }

    // --- rows -----------------------------------------------------------------

    /// The rows the keys stop on, top to bottom (the header's first).
    pub(crate) fn items(&self) -> Vec<Item> {
        let mut out = vec![Item::Back, Item::Save, Item::Discard, Item::Actions];
        let Some(state) = self.state.as_ref() else {
            return out;
        };
        out.extend([Item::Text(Text::Name), Item::Text(Text::Description), Item::InitialStatus]);
        for s in &state.workflow().statuses {
            out.push(Item::Status(s.id.clone()));
        }
        out.extend([Item::AddStatus, Item::ManageStatuses]);
        for (i, t) in state.transitions().iter().enumerate() {
            out.push(Item::Transition(i));
            out.extend((0..t.actions.len()).map(|slot| Item::Action(i, slot)));
            if t.actions.len() < ACTIONS.len() {
                out.push(Item::AddAction(i));
            }
            out.push(Item::Details(i));
            if self.details == Some(i) {
                out.push(Item::Text(Text::Target));
                out.extend((0..self.run.len()).map(|l| Item::Text(Text::Run(l))));
                out.extend([Item::AddRun, Item::Text(Text::Ready), Item::Text(Text::Timeout), Item::Text(Text::Url)]);
            }
        }
        out.push(Item::AddTransition);
        out.extend([Item::Text(Text::BaseBranch), Item::Text(Text::Branch), Item::MergeStrategy, Item::ReviewMode]);
        if state.review_mode() == ReviewMode::Serve {
            out.extend((0..REVIEW_FIELDS.len()).map(|i| Item::Text(Text::Review(i))));
        }
        out.extend([Item::Zen, Item::Text(Text::Params), Item::Delete]);
        out
    }

    fn clamp(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn current(&self) -> Option<Item> {
        self.items().into_iter().nth(self.cursor)
    }

    // --- acting -----------------------------------------------------------------

    /// What Enter does on `item`.
    fn primary(&self, item: &Item) -> Act {
        match item {
            Item::Back => Act::Leave(Leave::Back),
            Item::Save => Act::Save,
            Item::Discard => Act::Discard,
            Item::Actions => Act::Actions,
            Item::Text(t) => Act::Type(t.clone()),
            Item::InitialStatus => Act::Pick(Pick::InitialStatus),
            Item::Status(id) => Act::StatusMenu(id.clone()),
            Item::AddStatus => Act::Pick(Pick::AddStatus),
            Item::ManageStatuses => Act::Leave(Leave::ManageStatuses),
            Item::Transition(i) => Act::TransitionMenu(*i),
            Item::Action(i, slot) => Act::Pick(Pick::SetAction(*i, *slot)),
            Item::AddAction(i) => Act::Pick(Pick::AddAction(*i)),
            Item::Details(i) => Act::ToggleDetails(*i),
            Item::AddRun => Act::AddRun,
            Item::AddTransition => Act::Change(Change::AddTransition),
            Item::MergeStrategy => Act::Pick(Pick::MergeStrategy),
            Item::ReviewMode => Act::Pick(Pick::ReviewMode),
            Item::Zen => Act::Pick(Pick::Zen),
            Item::Delete => Act::Delete,
        }
    }

    fn run(&mut self, act: Act) -> EditorAction {
        self.modal = None;
        if self.state.is_none() {
            return match act {
                Act::Leave(Leave::Back) => EditorAction::Close,
                _ => EditorAction::Stay,
            };
        }
        match act {
            Act::Change(change) => self.change(change),
            Act::Type(text) => {
                let value = self.text(&text);
                self.typing = Some((text, LineEdit::new(&value)));
            }
            Act::Pick(pick) => self.open_pick(pick),
            Act::StatusMenu(id) => self.status_menu(&id),
            Act::TransitionMenu(i) => self.transition_menu(i),
            Act::Actions => self.actions_menu(),
            Act::ToggleDetails(i) => {
                self.details = if self.details == Some(i) { None } else { Some(i) };
                self.texts.details = None;
                let form = self.state.as_ref().map(|s| s.text_form(self.details));
                if let Some(form) = form {
                    self.run = form.details.as_ref().map(|(_, d)| d.run.lines().map(str::to_string).collect()).unwrap_or_default();
                    self.texts.details = form.details;
                }
                self.select_item(&Item::Details(i));
            }
            Act::AddRun => {
                let at = match self.current() {
                    Some(Item::Text(Text::Run(l))) => l + 1,
                    _ => self.run.len(),
                };
                self.run.insert(at, String::new());
                self.select_item(&Item::Text(Text::Run(at)));
                self.typing = Some((Text::Run(at), LineEdit::new("")));
            }
            Act::RemoveRun(line) => {
                if line < self.run.len() {
                    self.run.remove(line);
                    self.typed();
                    self.clamp();
                }
            }
            Act::MoveRun(line, down) => {
                let to = if down { line + 1 } else { line.wrapping_sub(1) };
                if to < self.run.len() && line < self.run.len() {
                    self.run.swap(line, to);
                    self.typed();
                    self.select_item(&Item::Text(Text::Run(to)));
                }
            }
            Act::MoveStatus(id, down) => {
                let at = self.state.as_ref().and_then(|s| s.workflow().statuses.iter().position(|st| st.id == id));
                if let Some(at) = at {
                    let n = self.state.as_ref().map_or(0, |s| s.workflow().statuses.len());
                    let to = if down { at + 1 } else { at.wrapping_sub(1) };
                    if to < n {
                        self.change(Change::MoveStatus { id: id.clone(), index: to });
                        self.select_item(&Item::Status(id));
                    }
                }
            }
            Act::MoveAction(i, slot, down) => {
                let n = self.state.as_ref().and_then(|s| s.transitions().get(i)).map_or(0, |t| t.actions.len());
                let to = if down { slot + 1 } else { slot.wrapping_sub(1) };
                if to < n {
                    self.change(Change::MoveAction { index: i, slot, to });
                    self.select_item(&Item::Action(i, to));
                }
            }
            Act::Leave(leave) => return self.leave(leave),
            Act::Save => return self.save(),
            Act::Discard => return self.discard(),
            Act::Delete => {
                if self.state.as_ref().and_then(EditorState::delete_blocked).is_none() && !self.busy() {
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
        // Removing a transition (or a status, with its transitions)
        // renumbers the rest: the open Details would be another's.
        if matches!(change, Change::RemoveTransition(_) | Change::RemoveStatus(_)) {
            self.details = None;
        }
        let structural = matches!(change, Change::AddStatus(_) | Change::RemoveStatus(_) | Change::RemoveTransition(_));
        let added = match &change {
            Change::AddStatus(id) => Some(Item::Status(id.clone())),
            Change::AddTransition => Some(Item::Transition(self.state.as_ref().map_or(0, |s| s.transitions().len()))),
            Change::AddAction { index, .. } => {
                let slot = self.state.as_ref().and_then(|s| s.transitions().get(*index)).map_or(0, |t| t.actions.len());
                Some(Item::Action(*index, slot))
            }
            _ => None,
        };
        let made = self.state.as_mut().is_some_and(|s| s.change(change));
        if structural {
            self.sync(false);
        }
        if let (true, Some(item)) = (made, added) {
            self.select_item(&item);
        }
    }

    fn catalog_name(&self, id: &str) -> String {
        self.state
            .as_ref()
            .and_then(|s| s.catalog().iter().find(|c| c.id == id))
            .map_or_else(|| id.to_string(), |c| c.name.clone())
    }

    fn category(&self, id: &str) -> Option<StatusCategory> {
        self.state.as_ref().and_then(|s| s.catalog().iter().find(|c| c.id == id)).map(|c| c.category)
    }

    fn choice(label: impl Into<String>, note: impl Into<String>, act: Act) -> Choice {
        Choice { label: label.into(), note: note.into(), act, inert: false, help: None }
    }

    fn list(&mut self, title: impl Into<String>, choices: Vec<Choice>) {
        let selected = choices.iter().position(|c| c.note == "current").unwrap_or(0);
        self.modal = Some(Modal::List { title: title.into(), choices, selected });
    }

    fn open_pick(&mut self, pick: Pick) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let current = |yes: bool| if yes { "current" } else { "" };
        let statuses: Vec<(String, String)> =
            state.workflow().statuses.iter().map(|s| (s.id.clone(), self.catalog_name(&s.id))).collect();
        let (title, choices): (String, Vec<Choice>) = match &pick {
            Pick::InitialStatus => {
                let at = state.initial_status().map(|s| s.id.clone());
                let choices = statuses
                    .iter()
                    .map(|(id, name)| Self::choice(name, current(at.as_deref() == Some(id)), Act::Change(Change::InitialStatus(id.clone()))))
                    .collect();
                ("Initial status".into(), choices)
            }
            Pick::AddStatus => {
                let addable = state.addable_statuses();
                let choices = if addable.is_empty() {
                    vec![Choice { inert: true, help: Some(ALL_STATUSES_ADDED.into()), ..Self::choice("Nothing to add", "", Act::Nothing) }]
                } else {
                    addable.iter().map(|s| Self::choice(&s.name, &s.id, Act::Change(Change::AddStatus(s.id.clone())))).collect()
                };
                ("Add status".into(), choices)
            }
            Pick::Owner(id) => {
                let owner = state.status(id).map(|s| s.owner);
                let choices = [Owner::User, Owner::Agent]
                    .into_iter()
                    .map(|o| Self::choice(owner_label(o), current(owner == Some(o)), Act::Change(Change::Owner { id: id.clone(), owner: o })))
                    .collect();
                (format!("{} owner", self.catalog_name(id)), choices)
            }
            Pick::Agent(id) => {
                let agent = state.status(id).and_then(|s| s.agent.clone());
                let mut choices = vec![Self::choice("None", current(agent.is_none()), Act::Change(Change::Agent { id: id.clone(), agent: None }))];
                choices.extend(state.agents().into_iter().map(|a| {
                    let note = current(agent.as_deref() == Some(a.as_str()));
                    Self::choice(&a, note, Act::Change(Change::Agent { id: id.clone(), agent: Some(a.clone()) }))
                }));
                (format!("{} agent", self.catalog_name(id)), choices)
            }
            Pick::From(i) | Pick::To(i) => {
                let t = state.transitions().get(*i).cloned();
                let from = matches!(pick, Pick::From(_));
                let at = t.as_ref().map(|t| if from { t.from.clone() } else { t.to.clone() });
                let choices = statuses
                    .iter()
                    .map(|(id, name)| {
                        let change = if from {
                            Change::From { index: *i, status: id.clone() }
                        } else {
                            Change::To { index: *i, status: id.clone() }
                        };
                        Self::choice(name, current(at.as_deref() == Some(id)), Act::Change(change))
                    })
                    .collect();
                ((if from { "From" } else { "To" }).into(), choices)
            }
            Pick::SetAction(i, slot) => {
                let t = state.transitions().get(*i);
                let at = t.and_then(|t| t.actions.get(*slot)).copied();
                let choices = ACTIONS
                    .iter()
                    .filter(|a| Some(**a) == at || !t.is_some_and(|t| t.actions.contains(a)))
                    .map(|a| Self::choice(action_label(*a), current(Some(*a) == at), Act::Change(Change::SetAction { index: *i, slot: *slot, action: *a })))
                    .collect();
                ("Action".into(), choices)
            }
            Pick::AddAction(i) => {
                let t = state.transitions().get(*i);
                let choices = ACTIONS
                    .iter()
                    .filter(|a| !t.is_some_and(|t| t.actions.contains(a)))
                    .map(|a| Self::choice(action_label(*a), "", Act::Change(Change::AddAction { index: *i, action: *a })))
                    .collect();
                ("Add action".into(), choices)
            }
            Pick::MergeStrategy => {
                let at = state.git_form().merge_strategy;
                let choices = STRATEGIES
                    .iter()
                    .map(|s| {
                        let form = GitForm {
                            base_branch: self.texts.base_branch.clone(),
                            branch: self.texts.branch.clone(),
                            merge_strategy: *s,
                        };
                        Self::choice(strategy_label(*s), current(*s == at), Act::Change(Change::Git(form)))
                    })
                    .collect();
                ("Merge strategy".into(), choices)
            }
            Pick::ReviewMode => {
                let at = state.review_mode();
                let choices = [ReviewMode::DiffOnly, ReviewMode::Serve]
                    .into_iter()
                    .map(|m| Choice { help: Some(m.help().into()), ..Self::choice(m.label(), current(m == at), Act::Change(Change::ReviewMode(m))) })
                    .collect();
                ("Review mode".into(), choices)
            }
            Pick::Zen => {
                let custom = state.zen_override();
                let mut choices = vec![Choice {
                    help: Some(ZEN_PROJECT_HELP.into()),
                    ..Self::choice("Use project settings", current(custom.is_none()), Act::Change(Change::InheritZen))
                }];
                if let Some(what) = custom {
                    choices.push(Choice { help: Some(zen_custom_help(&what)), ..Self::choice("Custom", "current", Act::Nothing) });
                }
                ("Zen settings".into(), choices)
            }
        };
        if choices.is_empty() {
            return;
        }
        self.list(title, choices);
    }

    fn status_menu(&mut self, id: &str) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let ids: Vec<&str> = state.workflow().statuses.iter().map(|s| s.id.as_str()).collect();
        let at = ids.iter().position(|s| *s == id).unwrap_or(0);
        let mut choices = vec![
            Self::choice("Owner…", "o", Act::Pick(Pick::Owner(id.into()))),
            Self::choice("Agent…", "a", Act::Pick(Pick::Agent(id.into()))),
            Self::choice("Workspace tags", "t", Act::Type(Text::Tags(id.into()))),
        ];
        if at > 0 {
            choices.push(Self::choice("Move up", "⇧↑", Act::MoveStatus(id.into(), false)));
        }
        if at + 1 < ids.len() {
            choices.push(Self::choice("Move down", "⇧↓", Act::MoveStatus(id.into(), true)));
        }
        choices.push(Self::choice("Remove from workflow", "x", Act::Change(Change::RemoveStatus(id.into()))));
        choices.push(Self::choice("Add status…", "n", Act::Pick(Pick::AddStatus)));
        let title = self.catalog_name(id);
        self.modal = Some(Modal::List { title, choices, selected: 0 });
    }

    fn transition_menu(&mut self, i: usize) {
        let Some(t) = self.state.as_ref().and_then(|s| s.transitions().get(i)).cloned() else {
            return;
        };
        let details = if self.details == Some(i) { "Close details" } else { "Details" };
        let mut choices = vec![
            Self::choice("From…", "f", Act::Pick(Pick::From(i))),
            Self::choice("To…", "t", Act::Pick(Pick::To(i))),
        ];
        if t.actions.len() < ACTIONS.len() {
            choices.push(Self::choice("Add action…", "n", Act::Pick(Pick::AddAction(i))));
        }
        choices.push(Self::choice(details, "d", Act::ToggleDetails(i)));
        choices.push(Self::choice("Remove transition", "x", Act::Change(Change::RemoveTransition(i))));
        choices.push(Self::choice("Add transition", "", Act::Change(Change::AddTransition)));
        let title = format!("{} → {}", self.catalog_name(&t.from), self.catalog_name(&t.to));
        self.modal = Some(Modal::List { title, choices, selected: 0 });
    }

    /// The header's ⋮: Save, Discard, Edit YAML, Duplicate, Set as default,
    /// Manage project statuses, Delete (dimmed with its reason when the
    /// guards refuse it up front).
    fn actions_menu(&mut self) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let dirty = state.is_dirty();
        let inert = |mut c: Choice, why: Option<String>| {
            if why.is_some() {
                c.inert = true;
                c.help = why;
            }
            c
        };
        let choices = vec![
            inert(Self::choice("Save changes", "^S", Act::Save), (!dirty).then(|| "There’s nothing to save.".into())),
            inert(Self::choice("Discard changes", "", Act::Discard), (!dirty).then(|| "There’s nothing to discard.".into())),
            Self::choice("Edit YAML", "y", Act::Leave(Leave::EditYaml)),
            Self::choice("Duplicate", "c", Act::Leave(Leave::Duplicate)),
            inert(
                Self::choice("Set as default", "", Act::Change(Change::SetDefault)),
                state.is_default().then(|| "Already the default.".into()),
            ),
            Self::choice("Manage project statuses", "", Act::Leave(Leave::ManageStatuses)),
            inert(Self::choice("Delete workflow…", "", Act::Delete), state.delete_blocked()),
        ];
        let title = state.name().to_string();
        self.modal = Some(Modal::List { title, choices, selected: 0 });
    }

    // --- save, discard, leaving -----------------------------------------------

    fn save(&mut self) -> EditorAction {
        self.typing = None;
        let changes = match self.state.as_ref() {
            Some(state) => state.text_changes(&self.form()),
            None => return EditorAction::Stay,
        };
        let job = self.state.as_mut().and_then(|s| s.begin_save(changes));
        self.start_save(job)
    }

    fn start_save(&mut self, job: Option<SaveJob>) -> EditorAction {
        match job {
            Some(job) => {
                let ticket = self.job.begin(Some(&self.project));
                self.pending = Some((ticket, EditorWork::Save(job)));
                EditorAction::Work
            }
            None => EditorAction::Stay,
        }
    }

    /// Save for Settings' own prompt (a project switch): the prompt goes
    /// on once it lands. `Stay` when nothing starts (a field refused its
    /// text: its error shows).
    pub(crate) fn save_for_settings(&mut self) -> EditorAction {
        self.save()
    }

    /// Discard: the draft goes back to the files, read again when they
    /// changed under the last save.
    fn discard(&mut self) -> EditorAction {
        let reload = self.state.as_mut().is_some_and(EditorState::discard);
        self.sync(true);
        self.clamp();
        if reload {
            self.reload();
            return EditorAction::Work;
        }
        EditorAction::Stay
    }

    /// Settings' prompt's Discard (leaving anyway).
    pub(crate) fn discard_for_settings(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.discard();
        }
        self.sync(true);
    }

    fn leave(&mut self, leave: Leave) -> EditorAction {
        self.typing = None;
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
            Leave::Back => EditorAction::Close,
            Leave::ManageStatuses => EditorAction::ManageStatuses,
            Leave::Duplicate => {
                let Some(state) = self.state.as_mut() else {
                    return EditorAction::Stay;
                };
                let (name, copy) = (state.name().to_string(), state.duplicate_name());
                let Some(base) = state.begin_job() else {
                    return EditorAction::Stay;
                };
                let ticket = self.job.begin(Some(&self.project));
                self.pending = Some((ticket, EditorWork::Duplicate { base, name, copy }));
                EditorAction::Work
            }
            Leave::EditYaml => match self.state.as_ref() {
                Some(state) => EditorAction::EditYaml(EditFile {
                    path: state.file(),
                    target: Target::Workflow(state.name().to_string()),
                    statuses_of: None,
                }),
                None => EditorAction::Stay,
            },
        }
    }

    fn delete(&mut self) -> EditorAction {
        self.modal = None;
        let Some(state) = self.state.as_mut() else {
            return EditorAction::Stay;
        };
        if state.delete_blocked().is_some() {
            return EditorAction::Stay;
        }
        let name = state.name().to_string();
        let Some(base) = state.begin_job() else {
            return EditorAction::Stay;
        };
        let ticket = self.job.begin(Some(&self.project));
        self.pending = Some((ticket, EditorWork::Delete { base, name }));
        EditorAction::Work
    }

    /// Edit YAML couldn't open the file.
    pub(crate) fn open_failed(&mut self, message: String) {
        if let Some(state) = self.state.as_mut() {
            state.open_failed(message);
        }
    }

    /// Work landed. A result for an older request (or another project) is
    /// dropped: `None`.
    pub(crate) fn landed(&mut self, ticket: &Ticket, done: EditorDone) -> Option<Landed> {
        match done {
            EditorDone::Loaded(config, git) => {
                if !self.load.complete(ticket, Ok(())) {
                    return None;
                }
                let Some(state) = self.state.as_mut() else {
                    match config {
                        Ok(config) if config.workflow(&self.name).is_some() => {
                            let name = self.name.clone();
                            let mut state = EditorState::new(config, &name);
                            state.set_project_git(git);
                            self.show(state);
                        }
                        Ok(_) => self.load_error = Some(format!("There is no workflow named {}.", self.name)),
                        Err(e) => self.load_error = Some(format!("Couldn’t read the workflows: {e}")),
                    }
                    return Some(Landed::Stay);
                };
                state.set_project_git(git);
                // A draft with edits (or a save running) keeps them; its
                // save will be refused should it overwrite a hand edit.
                if state.is_dirty() || state.busy() {
                    return Some(Landed::Stay);
                }
                match config {
                    Ok(config) => {
                        if state.reloaded(config) {
                            self.sync(true);
                            self.clamp();
                            Some(Landed::Stay)
                        } else {
                            Some(Landed::Close {
                                wrote: false,
                                notice: Some(format!("Workflow {} is no longer in the files.", self.name)),
                            })
                        }
                    }
                    Err(e) => {
                        state.job_failed(format!("Couldn’t read the workflows again: {e}"));
                        Some(Landed::Stay)
                    }
                }
            }
            EditorDone::Saved(outcome) => {
                if !self.job.complete(ticket, Ok(())) {
                    return None;
                }
                let state = self.state.as_mut()?;
                let ok = outcome.is_ok();
                let then = state.saved(outcome);
                if ok {
                    self.sync(true);
                    self.clamp();
                } else if self.state.as_ref().is_some_and(EditorState::conflict) {
                    // Keep editing is focused: Enter alone never drops edits.
                    self.modal = Some(Modal::Confirm { kind: Confirm::Conflict, choice: 1 });
                }
                Some(match (then, ok) {
                    (Some(then), _) => Landed::Then(then),
                    (None, true) => Landed::Wrote,
                    (None, false) => Landed::Stay,
                })
            }
            EditorDone::Duplicated(result, copy) => {
                if !self.job.complete(ticket, Ok(())) {
                    return None;
                }
                match result {
                    Ok(config) => {
                        self.cursor = 0;
                        self.show(EditorState::new(config, &copy));
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
            EditorDone::Deleted(result) => {
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

    /// Paste into the field being typed (newlines dropped); false when none
    /// is.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
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
        if self.typing.is_some() {
            return self.typing_key(k);
        }
        let items = self.items();
        let n = items.len();
        let current = items.get(self.cursor).cloned();
        let shifted = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        if shifted && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            let down = k.code == KeyCode::Down;
            let act = match current {
                Some(Item::Status(id)) => Act::MoveStatus(id, down),
                Some(Item::Action(i, slot)) => Act::MoveAction(i, slot, down),
                Some(Item::Text(Text::Run(line))) => Act::MoveRun(line, down),
                _ => return EditorAction::Stay,
            };
            return self.run(act);
        }
        match k.code {
            KeyCode::Esc => return self.leave(Leave::Back),
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
            (KeyCode::Char('m') | KeyCode::Char('.'), Item::Status(id)) => Act::StatusMenu(id.clone()),
            (KeyCode::Char('m') | KeyCode::Char('.'), Item::Transition(i)) => Act::TransitionMenu(*i),
            (KeyCode::Char('m') | KeyCode::Char('.'), _) => Act::Actions,
            (KeyCode::Char('o'), Item::Status(id)) => Act::Pick(Pick::Owner(id.clone())),
            (KeyCode::Char('a'), Item::Status(id)) => Act::Pick(Pick::Agent(id.clone())),
            (KeyCode::Char('t'), Item::Status(id)) => Act::Type(Text::Tags(id.clone())),
            (KeyCode::Char('f'), Item::Transition(i)) => Act::Pick(Pick::From(*i)),
            (KeyCode::Char('t'), Item::Transition(i)) => Act::Pick(Pick::To(*i)),
            (KeyCode::Char('d'), Item::Transition(i) | Item::Action(i, _) | Item::AddAction(i) | Item::Details(i)) => {
                Act::ToggleDetails(*i)
            }
            (KeyCode::Char('n'), it) => match it.section() {
                Section::Statuses => Act::Pick(Pick::AddStatus),
                Section::Transitions => match it {
                    Item::Transition(i) | Item::Action(i, _) | Item::AddAction(i) => Act::Pick(Pick::AddAction(*i)),
                    Item::Text(Text::Run(_)) | Item::AddRun => Act::AddRun,
                    _ => Act::Change(Change::AddTransition),
                },
                _ => return EditorAction::Stay,
            },
            (KeyCode::Char('x') | KeyCode::Delete, Item::Status(id)) => Act::Change(Change::RemoveStatus(id.clone())),
            (KeyCode::Char('x') | KeyCode::Delete, Item::Transition(i)) => Act::Change(Change::RemoveTransition(*i)),
            (KeyCode::Char('x') | KeyCode::Delete, Item::Action(i, slot)) => {
                Act::Change(Change::RemoveAction { index: *i, slot: *slot })
            }
            (KeyCode::Char('x') | KeyCode::Delete, Item::Text(Text::Run(line))) => Act::RemoveRun(*line),
            (KeyCode::Char('y'), _) => Act::Leave(Leave::EditYaml),
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
        let starts: Vec<usize> = (0..items.len())
            .filter(|&i| i == 0 || items[i].section() != items[i - 1].section())
            .collect();
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

    /// A key while a field is typed: Enter, Esc, ↑↓ and Tab stop typing (the
    /// text is already in the draft); the rest edit the field.
    fn typing_key(&mut self, k: KeyEvent) -> EditorAction {
        let Some((field, mut edit)) = self.typing.take() else {
            return EditorAction::Stay;
        };
        match k.code {
            KeyCode::Enter | KeyCode::Esc => {}
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
        EditorAction::Stay
    }

    fn modal_key(&mut self, modal: Modal, k: KeyEvent, nav: Nav) -> EditorAction {
        match modal {
            Modal::List { title, choices, mut selected } => {
                let up = k.code == KeyCode::Up || (nav.up && !nav.activate);
                let down = k.code == KeyCode::Down || (nav.down && !nav.activate);
                match k.code {
                    KeyCode::Esc => {
                        self.modal = None;
                        return EditorAction::Stay;
                    }
                    KeyCode::Enter => return self.choose(&choices, selected),
                    _ if up => selected = selected.saturating_sub(1),
                    _ if down => selected = (selected + 1).min(choices.len().saturating_sub(1)),
                    _ if nav.activate => return self.choose(&choices, selected),
                    KeyCode::Char(c) => {
                        // An entry's own key runs it from the menu too.
                        let typed = c.to_string();
                        if let Some(i) = choices.iter().position(|e| e.note.split(' ').any(|k| k == typed)) {
                            return self.choose(&choices, i);
                        }
                    }
                    KeyCode::Delete => {
                        if let Some(i) = choices.iter().position(|e| e.note == "x") {
                            return self.choose(&choices, i);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::List { title, choices, selected });
                EditorAction::Stay
            }
            Modal::Confirm { kind, mut choice } => {
                let (yes, no) = match kind {
                    Confirm::Delete => (KeyCode::Char('y'), KeyCode::Char('n')),
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
        }
    }

    fn choose(&mut self, choices: &[Choice], index: usize) -> EditorAction {
        match choices.get(index) {
            Some(c) if !c.inert => self.run(c.act.clone()),
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
            // Reload: the draft goes, the files are read again.
            (Confirm::Conflict, true) => self.discard(),
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
                let changes = match self.state.as_ref() {
                    Some(state) => state.text_changes(&self.form()),
                    None => return EditorAction::Stay,
                };
                let job = self.state.as_mut().and_then(|s| s.save_and_leave(changes));
                self.start_save(job)
            }
            1 => {
                let then = self.state.as_mut().and_then(EditorState::discard_and_leave);
                self.sync(true);
                self.clamp();
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
                        (Modal::List { choices, .. }, Some(Hit::Choice(i))) => self.choose(&choices, i),
                        (Modal::Confirm { kind, .. }, Some(Hit::Button(b))) => self.confirm(kind, b == 0),
                        (Modal::List { .. }, None) => {
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
                let again = self.cursor == i;
                self.cursor = i;
                let header = matches!(self.current(), Some(Item::Back | Item::Save | Item::Discard | Item::Actions));
                match self.current() {
                    Some(item) if again || header || matches!(item, Item::Text(_)) => {
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
            Some(Modal::Confirm { kind: Confirm::Delete, .. }) => return "y delete · n/Esc cancel · ←→ choose · ⏎ press".into(),
            Some(Modal::Confirm { kind: Confirm::Conflict, .. }) => return "r reload · k/Esc keep editing · ←→ choose · ⏎ press".into(),
            None => {}
        }
        if self.typing.is_some() {
            return "Type · ⏎/Esc done · ↑↓ done and move · ^S save".into();
        }
        let rest = match self.current() {
            Some(Item::Status(_)) => "⏎ menu · o owner · a agent · t tags · ⇧↑↓ move · x remove · n add",
            Some(Item::Transition(_)) => "⏎ menu · f from · t to · n action · d details · x remove",
            Some(Item::Action(..)) => "⏎ change · ⇧↑↓ move · x remove · n add",
            Some(Item::Text(Text::Run(_))) => "⏎ edit · ⇧↑↓ move · x remove · n add",
            Some(Item::Text(_)) => "⏎ edit",
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
        // The header: never scrolls.
        let header = Rect { height: 1, ..inner };
        self.render_header(f, header, &items);
        let content = Rect {
            y: inner.y + 2.min(inner.height),
            height: inner.height.saturating_sub(2),
            ..inner
        };
        if content.height == 0 {
            return;
        }
        let rows = self.rows(content.width as usize);
        // Keep the focused row in view, then clamp to the content.
        let max = (rows.len() as u16).saturating_sub(content.height);
        if self.follow_cursor {
            if let Some(at) = rows.iter().position(|r| r.action == Some(self.cursor)) {
                // The row's error lines too, when they fit.
                // At least one line under it: the last line shows "↓ more"
                // while rows are clipped below.
                let end = rows.iter().skip(at + 1).take_while(|r| r.action.is_none()).count().clamp(1, 3) as u16;
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
        let dirty = state.is_some_and(EditorState::is_dirty);
        let busy = state.is_some_and(EditorState::busy);
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
        let back = button("← Back", &Item::Back, true);
        let mut left = vec![back, Span::raw("  ")];
        left.push(Span::styled(
            self.name().to_string(),
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        ));
        if state.is_some_and(EditorState::is_default) {
            left.push(Span::styled(" default", Style::default().fg(palette().cyan)));
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
        // Narrow: the name gives way first, then the buttons are cut.
        let gap = width.saturating_sub(left_w + right_w);
        let mut spans = left;
        if left_w + right_w > width {
            let room = width.saturating_sub(right_w + 10);
            spans.truncate(2);
            let name = panel::truncate_right(self.name(), room.max(1));
            spans.push(Span::styled(name, Style::default().fg(palette().white).add_modifier(Modifier::BOLD)));
        }
        spans.push(Span::raw(" ".repeat(gap.max(1))));
        spans.extend(right);
        f.render_widget(Paragraph::new(Line::from(spans.clone())), rect);
        // Click targets: walk the spans' widths.
        let mut x = rect.x;
        let targets = [(" ← Back ", Item::Back), (" Save changes ", Item::Save), (" Saving… ", Item::Save), (" Discard ", Item::Discard), (" ⋮ Actions ", Item::Actions)];
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

    /// The Form's rows at `width`.
    fn rows(&self, width: usize) -> Vec<Row> {
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
        // `Some(true)` on the cursor's row only: `Some(false)` would draw
        // every other row with the unfocused selection fill.
        let selected = |item: &Item| (index(item) == Some(self.cursor)).then_some(true);
        let label_w = 18.min(width / 3).max(6);
        let field = |rows: &mut Vec<Row>, label: &str, text: Text, placeholder: &str| {
            let item = Item::Text(text.clone());
            let line = self.field_line(label, &text, placeholder, selected(&item) == Some(true), label_w, width);
            rows.push(Row { action: index(&item), ..Row::text(line) });
        };
        let picker = |rows: &mut Vec<Row>, label: &str, value: &str, item: Item| {
            let line = picker_line(label, value, selected(&item) == Some(true), label_w, width);
            rows.push(Row { action: index(&item), ..Row::text(line) });
        };
        let errors = |rows: &mut Vec<Row>, f: Field| error_rows(rows, state.error_at(&f), width);

        // Workflow.
        heading(&mut rows, "Workflow");
        field(&mut rows, "Name", Text::Name, "");
        errors(&mut rows, Field::Name);
        field(&mut rows, "Description", Text::Description, DESCRIPTION_PLACEHOLDER);
        errors(&mut rows, Field::Description);
        let initial = state.initial_status().map(|s| self.catalog_name(&s.id)).unwrap_or_else(|| "—".into());
        picker(&mut rows, "Initial status", &initial, Item::InitialStatus);
        errors(&mut rows, Field::InitialStatus);

        // Statuses.
        rows.push(Row::blank());
        heading(&mut rows, &format!("Statuses ({})", state.workflow().statuses.len()));
        errors(&mut rows, Field::Statuses);
        for s in &state.workflow().statuses {
            let item = Item::Status(s.id.clone());
            let line = self.status_line(s.id.as_str(), selected(&item), width);
            rows.push(Row { action: index(&item), ..Row::text(line) });
            errors(&mut rows, Field::Status(s.id.clone()));
        }
        rows.push(Row { action: index(&Item::AddStatus), ..Row::text(add_line("+ Add status", selected(&Item::AddStatus), width)) });
        rows.push(Row { action: index(&Item::ManageStatuses), ..Row::text(add_line("Manage project statuses", selected(&Item::ManageStatuses), width)) });

        // Transitions.
        rows.push(Row::blank());
        heading(&mut rows, &format!("Transitions ({})", state.transitions().len()));
        wrapped(&mut rows, TRANSITIONS_HELP, width, muted);
        errors(&mut rows, Field::Transitions);
        for (i, t) in state.transitions().iter().enumerate() {
            let item = Item::Transition(i);
            let text = format!("{} → {}", self.catalog_name(&t.from), self.catalog_name(&t.to));
            let style = Style::default().fg(palette().white);
            rows.push(Row { action: index(&item), ..Row::text(plain_line(&text, selected(&item), width, style)) });
            errors(&mut rows, Field::Transition(i));
            for (slot, a) in t.actions.iter().enumerate() {
                let item = Item::Action(i, slot);
                let text = format!("    {}. {}", slot + 1, action_label(*a));
                let line = plain_line(&text, selected(&item), width, Style::default().fg(palette().foreground));
                rows.push(Row { action: index(&item), ..Row::text(line) });
            }
            if t.actions.is_empty() {
                wrapped(&mut rows, "      No actions", width, muted);
            }
            if index(&Item::AddAction(i)).is_some() {
                rows.push(Row { action: index(&Item::AddAction(i)), ..Row::text(add_line("    + Add action", selected(&Item::AddAction(i)), width)) });
            }
            let open = self.details == Some(i);
            let set = if state.has_details(i) { " (set)" } else { "" };
            let text = format!("    {} Details{set}", if open { "▾" } else { "▸" });
            let line = plain_line(&text, selected(&Item::Details(i)), width, Style::default().fg(palette().cyan));
            rows.push(Row { action: index(&Item::Details(i)), ..Row::text(line) });
            if open {
                field(&mut rows, &format!("  {}", DETAILS_TARGET.0), Text::Target, DETAILS_TARGET.1);
                wrapped(&mut rows, &format!("      Run: {RUN_HELP}"), width, muted);
                for line in 0..self.run.len() {
                    field(&mut rows, "    $", Text::Run(line), "");
                }
                rows.push(Row { action: index(&Item::AddRun), ..Row::text(add_line("      + Add command", selected(&Item::AddRun), width)) });
                field(&mut rows, &format!("  {}", DETAILS_READY.0), Text::Ready, DETAILS_READY.1);
                field(&mut rows, &format!("  {}", DETAILS_TIMEOUT.0), Text::Timeout, DETAILS_TIMEOUT.1);
                field(&mut rows, &format!("  {}", DETAILS_URL.0), Text::Url, DETAILS_URL.1);
            }
        }
        rows.push(Row { action: index(&Item::AddTransition), ..Row::text(add_line("+ Add transition", selected(&Item::AddTransition), width)) });

        // Git.
        rows.push(Row::blank());
        heading(&mut rows, "Git");
        wrapped(&mut rows, GIT_HELP, width, muted);
        let project = state.project_git();
        field(&mut rows, "Base branch", Text::BaseBranch, &project.base_branch);
        field(&mut rows, "Branch name", Text::Branch, &project.branch);
        picker(&mut rows, "Merge strategy", strategy_label(state.git_form().merge_strategy), Item::MergeStrategy);
        errors(&mut rows, Field::Git);

        // Review.
        rows.push(Row::blank());
        heading(&mut rows, "Review");
        let mode = state.review_mode();
        picker(&mut rows, "Review mode", mode.label(), Item::ReviewMode);
        wrapped(&mut rows, mode.help(), width, muted);
        if mode == ReviewMode::Serve {
            for (i, (label, placeholder)) in REVIEW_FIELDS.iter().enumerate() {
                let label = if i == 0 { format!("{label} *") } else { label.to_string() };
                field(&mut rows, &label, Text::Review(i), placeholder);
            }
        }
        errors(&mut rows, Field::Review);

        // Automation.
        rows.push(Row::blank());
        heading(&mut rows, "Automation");
        let custom = state.zen_override();
        let (zen, help) = match &custom {
            None => ("Use project settings".to_string(), ZEN_PROJECT_HELP.to_string()),
            Some(what) => ("Custom".to_string(), zen_custom_help(what)),
        };
        picker(&mut rows, "Zen settings", &zen, Item::Zen);
        wrapped(&mut rows, &help, width, muted);
        errors(&mut rows, Field::Zen);
        field(&mut rows, "Required fields", Text::Params, "None");
        wrapped(&mut rows, PARAMS_HELP, width, muted);
        errors(&mut rows, Field::RequiredParams);

        // Delete.
        rows.push(Row::blank());
        let blocked = state.delete_blocked();
        let style = if blocked.is_some() { muted } else { red };
        rows.push(Row { action: index(&Item::Delete), ..Row::text(plain_line("Delete workflow…", selected(&Item::Delete), width, style)) });
        if let Some(why) = blocked {
            wrapped(&mut rows, &format!("    {why}"), width, muted);
        }
        rows
    }

    /// A text field's row: its label, then the text (the line editor while
    /// typed), the placeholder when empty; red label when its field errs.
    fn field_line(&self, label: &str, text: &Text, placeholder: &str, selected: bool, label_w: usize, width: usize) -> Line<'static> {
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
                    spans.push(Span::styled(panel::truncate_right(&value, room), Style::default().fg(palette().white)));
                }
            }
        }
        Line::from(spans)
    }

    /// A status's row: the handle, its name in its category's color, then
    /// its owner, agent and workspace tags.
    fn status_line(&self, id: &str, selected: Option<bool>, width: usize) -> Line<'static> {
        let Some(s) = self.state.as_ref().and_then(|st| st.status(id)) else {
            return Line::default();
        };
        let marker = if selected.is_some() { "› " } else { "  " };
        let color = self.category(id).map_or(palette().foreground, crate::kanban::category_color);
        let name_w = 16.min(width / 3);
        let tags = self.text(&Text::Tags(id.to_string()));
        let typing_tags = matches!(&self.typing, Some((Text::Tags(t), _)) if t == id);
        let agent = s.agent.clone().unwrap_or_else(|| "—".into());
        let tail = format!("{:<6} {:<14} ", owner_label(s.owner), panel::truncate_right(&agent, 14));
        let (base, glyph, name, muted) = match selected {
            Some(focused) => {
                let st = selected_style(focused);
                (st, st.fg(color), st, st)
            }
            None => (
                Style::default(),
                Style::default().fg(color),
                Style::default().fg(palette().white),
                Style::default().fg(palette().foreground),
            ),
        };
        let mut spans = vec![
            Span::styled(marker.to_string(), base),
            Span::styled("● ", glyph),
            Span::styled(cell(&self.catalog_name(id), name_w), name),
            Span::styled(" ", base),
        ];
        let used = 2 + 2 + name_w + 1;
        if width >= used + 22 {
            spans.push(Span::styled(tail, muted));
        }
        let used: usize = spans.iter().map(|s| panel::display_width(&s.content)).sum();
        let room = width.saturating_sub(used);
        match &self.typing {
            Some((_, edit)) if typing_tags => spans.extend(edit.spans(room)),
            _ if tags.is_empty() => spans.push(Span::styled(panel::truncate_right(TAGS_PLACEHOLDER, room), base.fg(palette().muted))),
            _ => spans.push(Span::styled(panel::truncate_right(&format!("[{tags}]"), room), muted)),
        }
        pad_selected(&mut spans, selected, width);
        Line::from(spans)
    }

    fn render_modal(&mut self, f: &mut Frame, area: Rect) {
        let Some(modal) = self.modal.clone() else {
            return;
        };
        match modal {
            Modal::List { title, choices, selected } => {
                let help = choices.get(selected).and_then(|c| c.help.clone());
                let help_lines = help.map(|h| shelbi_app::overlay::error_log::wrap(&h, 40)).unwrap_or_default();
                let extra = if help_lines.is_empty() { 0 } else { help_lines.len() as u16 + 1 };
                let Some(rect) = WorkflowsPage::popup(area, 44, choices.len() as u16 + 2 + extra) else {
                    return;
                };
                let inner = WorkflowsPage::frame(f, rect, &format!(" {title} "));
                let w = inner.width as usize;
                // Long lists scroll to keep the selection in view.
                let room = inner.height.saturating_sub(extra) as usize;
                let skip = (selected + 1).saturating_sub(room.max(1));
                for (row, (i, c)) in choices.iter().enumerate().skip(skip).take(room).enumerate() {
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
                    self.hits.push((r, Hit::Choice(i)));
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
                let (title, message, buttons) = match kind {
                    Confirm::Delete => (
                        " Delete workflow ",
                        format!(
                            "Delete workflow {} and its file? Open issues on it keep it from going.",
                            self.name()
                        ),
                        [" Delete ", " Cancel "],
                    ),
                    Confirm::Conflict => (
                        " Changed on disk ",
                        "The workflow files changed on disk since they were loaded, so the save was refused. \
                         Reload them (your unsaved edits are discarded), or keep editing."
                            .to_string(),
                        [" Reload ", " Keep editing "],
                    ),
                };
                self.render_buttons(f, area, title, &message, &buttons, choice);
            }
        }
    }

    fn render_prompt(&mut self, f: &mut Frame, area: Rect) {
        let message = format!("Save your changes to {} before leaving?", self.name());
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
            let style = if i == choice {
                selected_style(true)
            } else {
                Style::default().fg(palette().gray).bg(palette().selection)
            };
            let r = Rect { x, y, width: w, height: 1 };
            f.render_widget(Paragraph::new(Span::styled(label.to_string(), style)), r);
            self.hits.push((r, Hit::Button(i)));
            x += w + 1;
        }
    }
}

/// A picker's row: its label, then the value with a `▾`.
pub(super) fn picker_line(label: &str, value: &str, selected: bool, label_w: usize, width: usize) -> Line<'static> {
    let marker = if selected { "› " } else { "  " };
    let head_style = if selected { selected_style(true) } else { Style::default().fg(palette().gray) };
    let room = width.saturating_sub(label_w + 5);
    Line::from(vec![
        Span::styled(format!("{marker}{}", cell(label, label_w)), head_style),
        Span::raw(" "),
        Span::styled(panel::truncate_right(value, room), Style::default().fg(palette().white)),
        Span::styled(" ▾", Style::default().fg(palette().cyan)),
    ])
}

#[cfg(test)]
mod tests;
