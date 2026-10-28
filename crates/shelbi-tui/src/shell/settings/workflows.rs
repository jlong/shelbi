//! Settings ▸ Workflows in the TUI (`tui-settings-workflows`), after the
//! desktop app's page (`desktop/src/views/workflows.rs`) over the same
//! shared state (`shelbi_app::settings::workflows`): the project's workflows
//! over its status catalog, edited in place and saved as each edit is made.
//!
//! Top to bottom: the Workflows table (the name with a `default` badge, the
//! description, the status and transition counts; a file the model can't
//! read is listed with its error), its `+ New workflow` row; the Statuses
//! table (the status in its board color, its id, its category as text, the
//! workflows it's used in), its `+ New status` and `Edit statuses.yaml`
//! rows, the rename note; then the Status categories legend.
//!
//! The page's keys (with the page focused) walk every row with the user's
//! up / down; Enter does the row's main thing (edit a workflow, rename a
//! status, open a new row); `m` opens the row's actions menu, which lists
//! every action with its key: `n` new, `c` duplicate (a workflow) or change
//! category (a status), `s` set as default, `e` edit or rename, `y` Edit
//! YAML, `x` / Delete delete, Shift+↑ / Shift+↓ (or Alt) move a status, `r`
//! reload. A name is typed inline (Enter saves, Esc cancels; a new status's
//! category cycles with Tab). Delete asks first, and a delete the model's
//! guards refuse up front (the default or only workflow, a status a workflow
//! uses) says why at the row instead.
//!
//! Every edit goes to the shared state's queue; the shell runs each save off
//! the input and render loop ([`WorkflowsPage::begin_commit`]) and hands the
//! result back ([`WorkflowsPage::committed`]) under a ticket, so a result for
//! a project no longer shown is dropped.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use shelbi_app::settings::workflows::{
    category_help, category_label, Committed, Draft, Edit, Target, WorkflowsState, CATEGORIES,
    RENAME_NOTE, STATUSES_HELP, WORKFLOWS_HELP,
};
use shelbi_app::settings::{Job, Ticket};
use shelbi_core::StatusCategory;
use shelbi_state::workflow_config::{StatusSummary, WorkflowConfig, WorkflowSummary};

use super::{box_rows, heading, selected_style, wrapped, Row, SettingsAction};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// A row the page's keys stop on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    Workflow(String),
    /// A workflow file the model can't read.
    Unreadable(String),
    /// `+ New workflow`, or the new workflow's name being typed.
    NewWorkflow,
    Status(String),
    /// `+ New status`, or the new status's name being typed.
    NewStatus,
    /// Open `statuses.yaml` in the user's editor.
    EditStatuses,
}

impl Item {
    /// Whether the row sits in the Workflows table (`n` makes a workflow
    /// there, a status below).
    fn in_workflows(&self) -> bool {
        matches!(self, Item::Workflow(_) | Item::Unreadable(_) | Item::NewWorkflow)
    }
}

/// One thing a row can do (its menu lists them with their keys).
#[derive(Debug, Clone, PartialEq, Eq)]
enum Act {
    EditWorkflow(String),
    WorkflowYaml(String),
    NewWorkflow,
    Duplicate(String),
    SetDefault(String),
    DeleteWorkflow(String),
    Rename(String),
    Category(String),
    MoveStatus(String, bool),
    DeleteStatus(String),
    NewStatus,
    StatusesYaml,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MenuEntry {
    label: &'static str,
    key: &'static str,
    act: Act,
    /// Why it can't run (shown dimmed; choosing it says why at the row).
    blocked: bool,
}

/// What holds the keys over the page.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Modal {
    /// A row's actions.
    Menu { title: String, entries: Vec<MenuEntry>, selected: usize },
    /// Status `id`'s category picker.
    Category { id: String, selected: usize },
    /// Delete `edit`? Button 0 deletes, 1 cancels.
    Confirm { edit: Edit, message: String, choice: usize },
    /// Status `id`'s name being typed.
    Rename { id: String, text: LineEdit },
    /// The new row's name being typed (the kind is the state's `draft`).
    Draft { text: LineEdit },
}

/// What a click on the modal hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ModalHit {
    Entry(usize),
    Delete,
    Cancel,
}

/// Where the cursor goes once the page's next read lands (the workflow
/// editor closed over it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Select {
    Workflow(String),
    FirstStatus,
}

/// The keys the shell resolved from the user's sidebar keymap.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Nav {
    pub up: bool,
    pub down: bool,
    pub activate: bool,
}

/// A file the page asks the shell to open in the user's editor, and where a
/// failure to open it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EditFile {
    pub path: std::path::PathBuf,
    pub target: Target,
    /// `Some(project)` for `statuses.yaml`: written from the shipped catalog
    /// first when the project has none yet.
    pub statuses_of: Option<String>,
}

/// The page's state: the shared state for the shown project, plus the
/// TUI's cursor, modal and the tickets of its background work.
#[derive(Default)]
pub(crate) struct WorkflowsPage {
    state: Option<WorkflowsState>,
    load: Job<()>,
    save: Job<()>,
    /// The save to start next (the shell takes it).
    pending: Option<(WorkflowConfig, Edit)>,
    cursor: usize,
    modal: Option<Modal>,
    /// Where the cursor goes once the save in flight lands (a created
    /// workflow or status, a moved status).
    select_after: Option<Item>,
    /// Saves dropped as stale (their project moved on).
    dropped: usize,
    /// Where the cursor goes when the next read lands.
    select_on_load: Option<Select>,
    /// The modal's click targets, from the last draw.
    hits: Vec<(Rect, ModalHit)>,
}

impl WorkflowsPage {
    pub(crate) fn state(&self) -> Option<&WorkflowsState> {
        self.state.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn cursor_item(&self) -> Option<Item> {
        self.items().into_iter().nth(self.cursor)
    }

    #[cfg(test)]
    pub(crate) fn dropped(&self) -> usize {
        self.dropped
    }

    #[cfg(test)]
    pub(crate) fn select(&mut self, item: &Item) {
        if let Some(i) = self.items().iter().position(|it| it == item) {
            self.cursor = i;
        }
    }

    /// Whether a modal (a menu, a picker, the confirm, a name being typed)
    /// holds the keys.
    pub(crate) fn modal_open(&self) -> bool {
        self.modal.is_some()
    }

    /// Close any modal (Settings reopened); a new row being typed closes too.
    pub(crate) fn close_modal(&mut self) {
        if matches!(self.modal, Some(Modal::Draft { .. })) {
            self.close_draft();
        }
        self.modal = None;
    }

    /// Put the cursor on `select` now, and again when the next read lands
    /// (the editor may have renamed or added rows).
    pub(crate) fn select_after_load(&mut self, select: Select) {
        self.select_on_load = Some(select);
        self.apply_select();
    }

    fn apply_select(&mut self) {
        let Some(select) = self.select_on_load.as_ref() else {
            return;
        };
        let items = self.items();
        let at = match select {
            Select::Workflow(name) => items.iter().position(|i| *i == Item::Workflow(name.clone())),
            Select::FirstStatus => items.iter().position(|i| matches!(i, Item::Status(_))),
        };
        if let Some(i) = at {
            self.cursor = i;
        }
    }

    /// Show `message` at `target`.
    pub(crate) fn set_error(&mut self, target: Target, message: String) {
        if let Some(state) = self.state.as_mut() {
            state.set_error(target, message);
        }
    }

    /// The shown project changed (or closed): its state goes, and anything
    /// in flight for it is dropped when it lands.
    pub(crate) fn project_changed(&mut self) {
        self.state = None;
        self.load.reset();
        self.save.reset();
        self.pending = None;
        self.modal = None;
        self.select_after = None;
        self.select_on_load = None;
        self.cursor = 0;
    }

    /// Start reading `project`'s workflows: on every visit, so the page
    /// shows the files as they are now (hand edits included). `None` with no
    /// project, or while a read for it is already running and `force` is
    /// off.
    pub(crate) fn begin_load(&mut self, project: Option<&str>, force: bool) -> Option<Ticket> {
        let Some(project) = project else {
            self.project_changed();
            return None;
        };
        if self.state.as_ref().is_none_or(|s| s.project() != project) {
            self.project_changed();
            self.state = Some(WorkflowsState::new(project));
        }
        if self.load.running() && !force {
            return None;
        }
        Some(self.load.begin(Some(project)))
    }

    /// A read landed. Returns whether it was taken (a stale one is dropped;
    /// one landing during a save is left to the save's own fresh model).
    pub(crate) fn loaded(&mut self, ticket: &Ticket, result: Result<WorkflowConfig, String>) -> bool {
        let outcome = result.as_ref().map(|_| ()).map_err(Clone::clone);
        if !self.load.complete(ticket, outcome) {
            return false;
        }
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let taken = state.loaded(result);
        if taken {
            self.apply_select();
            self.select_on_load = None;
        }
        self.clamp_cursor();
        taken
    }

    /// The save to run now, if one is waiting: its ticket, the project, the
    /// model to apply it to, and the edit.
    pub(crate) fn begin_commit(&mut self) -> Option<(Ticket, String, WorkflowConfig, Edit)> {
        let (config, edit) = self.pending.take()?;
        let project = self.state.as_ref()?.project().to_string();
        let ticket = self.save.begin(Some(&project));
        Some((ticket, project, config, edit))
    }

    /// A save landed. Returns whether it saved, for the shown project (the
    /// shell refreshes the board then); a stale one is dropped. The next
    /// queued save, if any, waits in [`Self::begin_commit`].
    pub(crate) fn committed(&mut self, ticket: &Ticket, done: Committed) -> bool {
        if !self.save.complete(ticket, Ok(())) {
            self.dropped += 1;
            return false;
        }
        let Some(state) = self.state.as_mut() else {
            return false;
        };
        let ok = done.error.is_none();
        let edit = done.edit.clone();
        let next = state.committed(done);
        self.pending = next;
        match &self.modal {
            // The new row closes once its item exists (the state cleared
            // its draft).
            Some(Modal::Draft { .. }) if state.draft.is_none() => self.modal = None,
            _ => {}
        }
        if !ok {
            // The cursor stays where the error shows.
            if let Edit::AddStatus { .. } | Edit::CreateWorkflow { .. } | Edit::DuplicateWorkflow { .. } = edit {
                self.select_after = None;
            }
        }
        if let Some(item) = self.select_after.take() {
            if let Some(i) = self.items().iter().position(|it| *it == item) {
                self.cursor = i;
            } else if !ok {
                self.select_after = Some(item);
            }
        }
        self.clamp_cursor();
        ok
    }

    /// Make `edit` through the shared queue.
    fn request(&mut self, edit: Edit) -> SettingsAction {
        let Some(state) = self.state.as_mut() else {
            return SettingsAction::Stay;
        };
        match state.request(edit) {
            Some(job) => {
                self.pending = Some(job);
                SettingsAction::Commit
            }
            None => SettingsAction::Stay,
        }
    }

    // --- rows -----------------------------------------------------------------

    /// The rows the keys stop on, top to bottom.
    pub(crate) fn items(&self) -> Vec<Item> {
        let Some(state) = self.state.as_ref().filter(|s| s.config().is_some()) else {
            return Vec::new();
        };
        let mut out: Vec<Item> = state.workflows().into_iter().map(|w| Item::Workflow(w.name)).collect();
        out.extend(state.unreadable().iter().map(|u| Item::Unreadable(u.name.clone())));
        out.push(Item::NewWorkflow);
        out.extend(state.statuses().into_iter().map(|s| Item::Status(s.id)));
        out.push(Item::NewStatus);
        out.push(Item::EditStatuses);
        out
    }

    fn clamp_cursor(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    fn current(&self) -> Option<Item> {
        self.items().into_iter().nth(self.cursor)
    }

    // --- actions ----------------------------------------------------------------

    /// The selected row's actions, in menu order.
    fn entries(&self, item: &Item) -> Vec<MenuEntry> {
        let Some(state) = self.state.as_ref() else {
            return Vec::new();
        };
        let entry = |label, key, act, blocked| MenuEntry { label, key, act, blocked };
        match item {
            Item::Workflow(name) => {
                let is_default = state.config().is_some_and(|c| c.default_workflow() == name);
                let mut out = vec![
                    entry("Edit", "⏎ e", Act::EditWorkflow(name.clone()), false),
                    entry("Edit YAML", "y", Act::WorkflowYaml(name.clone()), false),
                    entry("Duplicate", "c", Act::Duplicate(name.clone()), false),
                ];
                if !is_default {
                    out.push(entry("Set as default", "s", Act::SetDefault(name.clone()), false));
                }
                let blocked = state.delete_workflow_blocked(name).is_some();
                out.push(entry("Delete…", "x", Act::DeleteWorkflow(name.clone()), blocked));
                out.push(entry("New workflow", "n", Act::NewWorkflow, false));
                out
            }
            Item::Unreadable(name) => vec![entry("Edit YAML", "⏎ y", Act::WorkflowYaml(name.clone()), false)],
            Item::NewWorkflow => vec![entry("New workflow", "⏎ n", Act::NewWorkflow, false)],
            Item::Status(id) => {
                let statuses = state.statuses();
                let at = statuses.iter().position(|s| &s.id == id).unwrap_or(0);
                let mut out = vec![
                    entry("Rename", "⏎ e", Act::Rename(id.clone()), false),
                    entry("Change category", "c", Act::Category(id.clone()), false),
                ];
                if at > 0 {
                    out.push(entry("Move up", "⇧↑", Act::MoveStatus(id.clone(), false), false));
                }
                if at + 1 < statuses.len() {
                    out.push(entry("Move down", "⇧↓", Act::MoveStatus(id.clone(), true), false));
                }
                let blocked = state.delete_status_blocked(id).is_some();
                out.push(entry("Delete…", "x", Act::DeleteStatus(id.clone()), blocked));
                out.push(entry("New status", "n", Act::NewStatus, false));
                out.push(entry("Edit statuses.yaml", "y", Act::StatusesYaml, false));
                out
            }
            Item::NewStatus => vec![entry("New status", "⏎ n", Act::NewStatus, false)],
            Item::EditStatuses => vec![entry("Edit statuses.yaml", "⏎ y", Act::StatusesYaml, false)],
        }
    }

    /// The row's main thing (Enter).
    fn primary(item: &Item) -> Act {
        match item {
            Item::Workflow(name) => Act::EditWorkflow(name.clone()),
            Item::Unreadable(name) => Act::WorkflowYaml(name.clone()),
            Item::NewWorkflow => Act::NewWorkflow,
            Item::Status(id) => Act::Rename(id.clone()),
            Item::NewStatus => Act::NewStatus,
            Item::EditStatuses => Act::StatusesYaml,
        }
    }

    fn run(&mut self, act: Act) -> SettingsAction {
        self.modal = None;
        let Some(state) = self.state.as_ref() else {
            return SettingsAction::Stay;
        };
        let Some(config) = state.config() else {
            return SettingsAction::Stay;
        };
        match act {
            Act::EditWorkflow(name) => SettingsAction::EditWorkflow { name, status: None },
            Act::WorkflowYaml(name) => SettingsAction::EditYaml(EditFile {
                path: config.workflow_path(&name),
                target: Target::Workflow(name),
                statuses_of: None,
            }),
            Act::StatusesYaml => SettingsAction::EditYaml(EditFile {
                path: config.statuses_path().to_path_buf(),
                target: Target::Page,
                statuses_of: Some(state.project().to_string()),
            }),
            Act::NewWorkflow => {
                self.open_draft(Draft::Workflow { from: None }, String::new());
                SettingsAction::Stay
            }
            Act::Duplicate(from) => {
                let name = state.duplicate_name(&from);
                self.open_draft(Draft::Workflow { from: Some(from) }, name);
                SettingsAction::Stay
            }
            Act::NewStatus => {
                self.open_draft(Draft::Status { category: StatusCategory::Backlog }, String::new());
                SettingsAction::Stay
            }
            Act::SetDefault(name) => {
                self.select_after = Some(Item::Workflow(name.clone()));
                self.request(Edit::SetDefault { name })
            }
            Act::Rename(id) => {
                let name = config.status(&id).map(|s| s.name.clone()).unwrap_or_default();
                self.modal = Some(Modal::Rename { id, text: LineEdit::new(&name) });
                SettingsAction::Stay
            }
            Act::Category(id) => {
                let current = config.status(&id).map(|s| s.category).unwrap_or(StatusCategory::Backlog);
                let selected = CATEGORIES.iter().position(|c| *c == current).unwrap_or(0);
                self.modal = Some(Modal::Category { id, selected });
                SettingsAction::Stay
            }
            Act::MoveStatus(id, down) => match state.move_status_by(&id, down) {
                Some(edit) => {
                    self.select_after = Some(Item::Status(id));
                    self.request(edit)
                }
                None => SettingsAction::Stay,
            },
            // A guard known up front says why at the row instead.
            Act::DeleteWorkflow(name) => match state.delete_workflow_blocked(&name) {
                Some(why) => {
                    self.set_error(Target::Workflow(name), why);
                    SettingsAction::Stay
                }
                None => self.confirm(Edit::DeleteWorkflow { name }),
            },
            Act::DeleteStatus(id) => match state.delete_status_blocked(&id) {
                Some(why) => {
                    self.set_error(Target::Status(id), why);
                    SettingsAction::Stay
                }
                None => self.confirm(Edit::DeleteStatus { id }),
            },
        }
    }

    fn confirm(&mut self, edit: Edit) -> SettingsAction {
        let message = self
            .state
            .as_ref()
            .and_then(|s| s.confirmation(&edit))
            .unwrap_or_else(|| "Delete it?".into());
        // Cancel is focused: Enter alone never deletes.
        self.modal = Some(Modal::Confirm { edit, message, choice: 1 });
        SettingsAction::Stay
    }

    fn open_draft(&mut self, draft: Draft, text: String) {
        let item = match draft {
            Draft::Workflow { .. } => Item::NewWorkflow,
            Draft::Status { .. } => Item::NewStatus,
        };
        if let Some(state) = self.state.as_mut() {
            state.clear_error(match item {
                Item::NewWorkflow => &Target::NewWorkflow,
                _ => &Target::NewStatus,
            });
            state.draft = Some(draft);
        }
        if let Some(i) = self.items().iter().position(|it| *it == item) {
            self.cursor = i;
        }
        self.modal = Some(Modal::Draft { text: LineEdit::new(&text) });
    }

    fn close_draft(&mut self) {
        if let Some(state) = self.state.as_mut() {
            state.draft = None;
        }
        self.modal = None;
    }

    // --- keys -----------------------------------------------------------------

    /// A key on the page (focused), or any key while a modal is open.
    pub(crate) fn handle_key(&mut self, k: KeyEvent, nav: Nav) -> SettingsAction {
        if let Some(modal) = self.modal.clone() {
            return self.modal_key(modal, k, nav);
        }
        let items = self.items();
        let n = items.len();
        let shifted = k.modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT);
        let current = items.get(self.cursor).cloned();
        // Shift/Alt + ↑↓ move a status.
        if shifted && matches!(k.code, KeyCode::Up | KeyCode::Down) {
            if let Some(Item::Status(id)) = current {
                return self.run(Act::MoveStatus(id, k.code == KeyCode::Down));
            }
            return SettingsAction::Stay;
        }
        if nav.up {
            self.cursor = self.cursor.saturating_sub(1);
            return SettingsAction::Stay;
        }
        if nav.down {
            self.cursor = (self.cursor + 1).min(n.saturating_sub(1));
            return SettingsAction::Stay;
        }
        if k.code == KeyCode::Char('r') && k.modifiers.is_empty() {
            return SettingsAction::Reload;
        }
        match k.code {
            KeyCode::PageUp => self.cursor = self.cursor.saturating_sub(8),
            KeyCode::PageDown => self.cursor = (self.cursor + 8).min(n.saturating_sub(1)),
            KeyCode::Home => self.cursor = 0,
            KeyCode::End => self.cursor = n.saturating_sub(1),
            _ => {}
        }
        let Some(item) = current else {
            return SettingsAction::Stay;
        };
        if nav.activate {
            return self.run(Self::primary(&item));
        }
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        let act = match (k.code, &item) {
            (KeyCode::Char('m') | KeyCode::Char('.'), _) if plain => {
                self.open_menu(&item);
                return SettingsAction::Stay;
            }
            (KeyCode::Char('n'), it) if plain => {
                if it.in_workflows() {
                    Act::NewWorkflow
                } else {
                    Act::NewStatus
                }
            }
            (KeyCode::Char('y'), Item::Workflow(n) | Item::Unreadable(n)) if plain => Act::WorkflowYaml(n.clone()),
            (KeyCode::Char('y'), Item::Status(_) | Item::NewStatus | Item::EditStatuses) if plain => Act::StatusesYaml,
            (KeyCode::Char('e'), Item::Workflow(n)) if plain => Act::EditWorkflow(n.clone()),
            (KeyCode::Char('e'), Item::Status(id)) if plain => Act::Rename(id.clone()),
            (KeyCode::Char('c'), Item::Workflow(n)) if plain => Act::Duplicate(n.clone()),
            (KeyCode::Char('c'), Item::Status(id)) if plain => Act::Category(id.clone()),
            (KeyCode::Char('s'), Item::Workflow(n)) if plain => Act::SetDefault(n.clone()),
            (KeyCode::Char('x') | KeyCode::Delete, Item::Workflow(n)) => Act::DeleteWorkflow(n.clone()),
            (KeyCode::Char('x') | KeyCode::Delete, Item::Status(id)) => Act::DeleteStatus(id.clone()),
            _ => return SettingsAction::Stay,
        };
        self.run(act)
    }

    fn open_menu(&mut self, item: &Item) {
        let entries = self.entries(item);
        if entries.is_empty() {
            return;
        }
        let title = match item {
            Item::Workflow(n) | Item::Unreadable(n) => n.clone(),
            Item::Status(id) => self
                .state
                .as_ref()
                .and_then(|s| s.config())
                .and_then(|c| c.status(id))
                .map(|s| s.name.clone())
                .unwrap_or_else(|| id.clone()),
            Item::NewWorkflow => "Workflows".into(),
            Item::NewStatus | Item::EditStatuses => "Statuses".into(),
        };
        self.modal = Some(Modal::Menu { title, entries, selected: 0 });
    }

    fn modal_key(&mut self, modal: Modal, k: KeyEvent, nav: Nav) -> SettingsAction {
        match modal {
            Modal::Menu { title, entries, mut selected } => {
                let up = k.code == KeyCode::Up || (nav.up && !nav.activate);
                let down = k.code == KeyCode::Down || (nav.down && !nav.activate);
                match k.code {
                    KeyCode::Esc => {
                        self.modal = None;
                        return SettingsAction::Stay;
                    }
                    KeyCode::Enter => return self.choose(&entries, selected),
                    _ if up => selected = selected.saturating_sub(1),
                    _ if down => selected = (selected + 1).min(entries.len().saturating_sub(1)),
                    _ if nav.activate => return self.choose(&entries, selected),
                    KeyCode::Char(c) => {
                        // An entry's own key runs it from the menu too.
                        let typed = c.to_string();
                        if let Some(i) = entries.iter().position(|e| e.key.split(' ').any(|k| k == typed)) {
                            return self.choose(&entries, i);
                        }
                    }
                    KeyCode::Delete => {
                        if let Some(i) = entries.iter().position(|e| e.key == "x") {
                            return self.choose(&entries, i);
                        }
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Menu { title, entries, selected });
                SettingsAction::Stay
            }
            Modal::Category { id, mut selected } => {
                match k.code {
                    KeyCode::Esc => {
                        self.modal = None;
                        return SettingsAction::Stay;
                    }
                    KeyCode::Enter => return self.pick_category(id, selected),
                    KeyCode::Up => selected = selected.saturating_sub(1),
                    KeyCode::Down => selected = (selected + 1).min(CATEGORIES.len() - 1),
                    _ if nav.up => selected = selected.saturating_sub(1),
                    _ if nav.down => selected = (selected + 1).min(CATEGORIES.len() - 1),
                    _ if nav.activate => return self.pick_category(id, selected),
                    _ => {}
                }
                self.modal = Some(Modal::Category { id, selected });
                SettingsAction::Stay
            }
            Modal::Confirm { edit, message, mut choice } => {
                match k.code {
                    KeyCode::Esc | KeyCode::Char('n') => {
                        self.modal = None;
                        return SettingsAction::Stay;
                    }
                    KeyCode::Char('y') => return self.confirmed(edit),
                    KeyCode::Left | KeyCode::BackTab => choice = 0,
                    KeyCode::Right | KeyCode::Tab => choice = 1,
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        if choice == 0 {
                            return self.confirmed(edit);
                        }
                        self.modal = None;
                        return SettingsAction::Stay;
                    }
                    _ => {}
                }
                self.modal = Some(Modal::Confirm { edit, message, choice });
                SettingsAction::Stay
            }
            Modal::Rename { id, mut text } => {
                match k.code {
                    KeyCode::Esc => {
                        self.modal = None;
                        return SettingsAction::Stay;
                    }
                    KeyCode::Enter => {
                        self.modal = None;
                        let typed = text.text().trim().to_string();
                        let current = self
                            .state
                            .as_ref()
                            .and_then(|s| s.config())
                            .and_then(|c| c.status(&id))
                            .map(|s| s.name.clone());
                        if current.is_some_and(|name| name != typed) {
                            return self.request(Edit::RenameStatus { id, name: typed });
                        }
                        return SettingsAction::Stay;
                    }
                    _ => text.key(k),
                }
                self.modal = Some(Modal::Rename { id, text });
                SettingsAction::Stay
            }
            Modal::Draft { mut text } => {
                let draft = self.state.as_ref().and_then(|s| s.draft.clone());
                match (k.code, draft) {
                    (KeyCode::Esc, _) | (_, None) => {
                        self.close_draft();
                        return SettingsAction::Stay;
                    }
                    (KeyCode::Enter, Some(draft)) => {
                        let name = text.text().trim().to_string();
                        if name.is_empty() {
                            return SettingsAction::Stay;
                        }
                        let edit = match draft {
                            Draft::Workflow { from: None } => Edit::CreateWorkflow { name: name.clone() },
                            Draft::Workflow { from: Some(from) } => Edit::DuplicateWorkflow { from, name: name.clone() },
                            Draft::Status { category } => {
                                let id = self.state.as_ref().and_then(|s| s.new_status_id(&name));
                                self.select_after = id.map(Item::Status);
                                return self.request(Edit::AddStatus { name, category });
                            }
                        };
                        self.select_after = Some(Item::Workflow(name));
                        return self.request(edit);
                    }
                    (KeyCode::Tab | KeyCode::BackTab, Some(Draft::Status { category })) => {
                        let at = CATEGORIES.iter().position(|c| *c == category).unwrap_or(0);
                        let n = CATEGORIES.len();
                        let next = if k.code == KeyCode::Tab { (at + 1) % n } else { (at + n - 1) % n };
                        if let Some(state) = self.state.as_mut() {
                            state.draft = Some(Draft::Status { category: CATEGORIES[next] });
                        }
                    }
                    _ => text.key(k),
                }
                self.modal = Some(Modal::Draft { text });
                SettingsAction::Stay
            }
        }
    }

    fn choose(&mut self, entries: &[MenuEntry], index: usize) -> SettingsAction {
        match entries.get(index) {
            Some(entry) => self.run(entry.act.clone()),
            None => {
                self.modal = None;
                SettingsAction::Stay
            }
        }
    }

    fn pick_category(&mut self, id: String, selected: usize) -> SettingsAction {
        self.modal = None;
        let category = CATEGORIES[selected.min(CATEGORIES.len() - 1)];
        let same = self
            .state
            .as_ref()
            .and_then(|s| s.config())
            .and_then(|c| c.status(&id))
            .is_some_and(|s| s.category == category);
        if same {
            return SettingsAction::Stay;
        }
        self.request(Edit::SetCategory { id, category })
    }

    fn confirmed(&mut self, edit: Edit) -> SettingsAction {
        self.modal = None;
        self.request(edit)
    }

    /// Paste into the name being typed (newlines dropped); false when no
    /// field is open.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        let Some(Modal::Rename { text: field, .. } | Modal::Draft { text: field }) = self.modal.as_mut() else {
            return false;
        };
        for c in text.chars().filter(|c| !c.is_control()) {
            field.insert(c);
        }
        true
    }

    /// A click on a row: select it; a click on the selected row opens its
    /// menu (a `+` row or an Edit YAML row runs at once).
    pub(crate) fn click_item(&mut self, index: usize) -> SettingsAction {
        let Some(item) = self.items().into_iter().nth(index) else {
            return SettingsAction::Stay;
        };
        if self.modal.is_some() {
            return SettingsAction::Stay;
        }
        let again = self.cursor == index;
        self.cursor = index;
        match item {
            Item::NewWorkflow | Item::NewStatus | Item::EditStatuses | Item::Unreadable(_) => {
                self.run(Self::primary(&item))
            }
            _ if again => {
                self.open_menu(&item);
                SettingsAction::Stay
            }
            _ => SettingsAction::Stay,
        }
    }

    /// A click while a modal shows: `Some` when the modal took it (a click
    /// outside a menu or picker closes it).
    pub(crate) fn click_modal(&mut self, column: u16, row: u16) -> Option<SettingsAction> {
        let modal = self.modal.clone()?;
        if matches!(modal, Modal::Rename { .. } | Modal::Draft { .. }) {
            return None;
        }
        let hit = self.hits.iter().find(|(r, _)| panel::in_rect(*r, column, row)).map(|(_, h)| *h);
        Some(match (modal, hit) {
            (Modal::Menu { entries, .. }, Some(ModalHit::Entry(i))) => self.choose(&entries, i),
            (Modal::Category { id, .. }, Some(ModalHit::Entry(i))) => self.pick_category(id, i),
            (Modal::Confirm { edit, .. }, Some(ModalHit::Delete)) => self.confirmed(edit),
            (Modal::Confirm { .. }, Some(ModalHit::Cancel)) | (Modal::Menu { .. } | Modal::Category { .. }, None) => {
                self.modal = None;
                SettingsAction::Stay
            }
            _ => SettingsAction::Stay,
        })
    }

    /// The hint band's keys for what has focus on the page.
    pub(crate) fn hints(&self, moves: &str) -> String {
        match &self.modal {
            Some(Modal::Menu { .. }) => format!("{moves} choose · ⏎ run · a key runs its action · Esc close"),
            Some(Modal::Category { .. }) => format!("{moves} choose · ⏎ set category · Esc cancel"),
            Some(Modal::Confirm { .. }) => "y delete · n/Esc cancel · ←→ choose · ⏎ press".into(),
            Some(Modal::Rename { .. }) => "Type the name · ⏎ save · Esc cancel".into(),
            Some(Modal::Draft { .. }) => match self.state.as_ref().and_then(|s| s.draft.as_ref()) {
                Some(Draft::Status { .. }) => "Type the name · Tab category · ⏎ add · Esc cancel".into(),
                _ => "Type the name · ⏎ create · Esc cancel".into(),
            },
            None => {
                let rest = match self.current() {
                    Some(Item::Workflow(_)) => "⏎ edit · m actions · n new · c duplicate · s default · x delete · y YAML",
                    Some(Item::Unreadable(_)) => "⏎ edit YAML · m actions",
                    Some(Item::Status(_)) => {
                        "⏎ rename · m actions · c category · ⇧↑↓ move · x delete · n new · y YAML"
                    }
                    Some(Item::NewWorkflow) => "⏎ new workflow",
                    Some(Item::NewStatus) => "⏎ new status",
                    Some(Item::EditStatuses) => "⏎ open statuses.yaml",
                    None => "r reload",
                };
                format!("{moves} · {rest} · Tab sidebar · Esc back")
            }
        }
    }

    // --- drawing --------------------------------------------------------------

    /// The row the cursor is on, for the shell's scroll (`Row::action`).
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// The page's rows at `width`; `focused` when the page has the keys.
    pub(super) fn rows(&self, width: usize, focused: bool, has_project: bool) -> Vec<Row> {
        let mut rows = vec![Row::text(Line::from(vec![
            Span::styled("Workflows", Style::default().fg(palette().white).add_modifier(Modifier::BOLD)),
            Span::styled(self.saving_note(), Style::default().fg(palette().muted)),
        ]))];
        let muted = Style::default().fg(palette().muted);
        let red = Style::default().fg(palette().red);
        let Some(state) = self.state.as_ref().filter(|_| has_project) else {
            rows.push(Row::blank());
            wrapped(&mut rows, "No project is open. Switch to a project to edit its workflows.", width, muted);
            return rows;
        };
        wrapped(&mut rows, WORKFLOWS_HELP, width, muted);
        if let Some(notice) = state.notice() {
            wrapped(&mut rows, notice, width, Style::default().fg(palette().yellow));
        }
        if let Some(e) = state.error_at(&Target::Page) {
            wrapped(&mut rows, e, width, red);
        }
        let Some(config) = state.config() else {
            rows.push(Row::blank());
            if state.error_at(&Target::Page).is_some() {
                wrapped(&mut rows, "Press r to try again.", width, muted);
            } else {
                rows.push(Row::text(Line::from(Span::styled(
                    "Loading…",
                    muted.add_modifier(Modifier::ITALIC),
                ))));
            }
            return rows;
        };
        // Errors whose row is gone (a status deleted by hand meanwhile).
        let items = self.items();
        for (target, e) in state.errors() {
            let shown = match target {
                Target::Page => true,
                Target::Workflow(n) => items.iter().any(|i| matches!(i, Item::Workflow(m) | Item::Unreadable(m) if m == n)),
                Target::Status(id) => items.contains(&Item::Status(id.clone())),
                Target::NewWorkflow | Target::NewStatus => state.draft.is_some(),
            };
            if !shown {
                wrapped(&mut rows, e, width, red);
            }
        }
        let selected = |i: usize| (i == self.cursor).then_some(focused);
        let mut index = 0usize;

        // Workflows.
        rows.push(Row::blank());
        heading(&mut rows, "Workflows");
        let workflows = state.workflows();
        let cols = WorkflowCols::fit(&workflows, width);
        rows.push(Row::text(cols.heading()));
        for w in &workflows {
            rows.push(Row::item(cols.line(w, selected(index), width), index));
            error_rows(&mut rows, state.error_at(&Target::Workflow(w.name.clone())), width);
            index += 1;
        }
        for u in state.unreadable() {
            let text = format!("⚠ {}  Can’t be edited here: {}", u.name, u.error);
            rows.push(Row::item(plain_line(&text, selected(index), width, Style::default().fg(palette().red)), index));
            error_rows(&mut rows, state.error_at(&Target::Workflow(u.name.clone())), width);
            index += 1;
        }
        match (&self.modal, &state.draft) {
            (Some(Modal::Draft { text }), Some(Draft::Workflow { from })) => {
                let refused = state.error_at(&Target::NewWorkflow);
                draft_rows(&mut rows, text, "Workflow name", refused, index, width);
                let help = match from {
                    Some(from) => format!("Duplicate of {from}. Enter to create, Esc to cancel."),
                    None => "Enter to create, Esc to cancel.".to_string(),
                };
                wrapped(&mut rows, &help, width, muted);
            }
            _ => {
                rows.push(Row::item(add_line("+ New workflow", selected(index), width), index));
                error_rows(&mut rows, state.error_at(&Target::NewWorkflow), width);
            }
        }
        index += 1;

        // Statuses.
        rows.push(Row::blank());
        heading(&mut rows, "Statuses");
        wrapped(&mut rows, STATUSES_HELP, width, muted);
        let statuses = state.statuses();
        let cols = StatusCols::fit(&statuses, width);
        rows.push(Row::text(cols.heading()));
        for s in &statuses {
            let refused = state.error_at(&Target::Status(s.id.clone()));
            match &self.modal {
                // Renamed in a field under its row.
                Some(Modal::Rename { id, text }) if *id == s.id => {
                    rows.push(Row::item(cols.line(s, Some(true), width), index));
                    draft_rows(&mut rows, text, "Status name", refused, index, width);
                    wrapped(&mut rows, "Enter to rename, Esc to cancel.", width, muted);
                }
                _ => {
                    rows.push(Row::item(cols.line(s, selected(index), width), index));
                    error_rows(&mut rows, refused, width);
                }
            }
            index += 1;
        }
        match (&self.modal, &state.draft) {
            (Some(Modal::Draft { text }), Some(Draft::Status { category })) => {
                let refused = state.error_at(&Target::NewStatus);
                draft_rows(&mut rows, text, "Status name", refused, index, width);
                let id = state.new_status_id(&text.text()).unwrap_or_else(|| "…".into());
                rows.push(Row::text(Line::from(vec![
                    Span::styled("id ", muted),
                    Span::styled(id, Style::default().fg(palette().foreground)),
                    Span::styled("  category ", muted),
                    Span::styled(category_label(*category), Style::default().fg(crate::kanban::category_color(*category))),
                    Span::styled("  (Tab changes)", muted),
                ])));
                wrapped(&mut rows, "Enter to add, Esc to cancel.", width, muted);
            }
            _ => {
                rows.push(Row::item(add_line("+ New status", selected(index), width), index));
                error_rows(&mut rows, state.error_at(&Target::NewStatus), width);
            }
        }
        index += 1;
        rows.push(Row::item(add_line("Edit statuses.yaml in your editor", selected(index), width), index));
        wrapped(&mut rows, RENAME_NOTE, width, muted);

        // The legend.
        rows.push(Row::blank());
        heading(&mut rows, "Status categories");
        let label_w = 10.min(width / 3);
        for category in CATEGORIES {
            let label = format!("{:<label_w$}", category_label(category));
            let help_w = width.saturating_sub(label_w + 2).max(1);
            for (i, line) in shelbi_app::overlay::error_log::wrap(category_help(category), help_w).into_iter().enumerate() {
                let left = if i == 0 { label.clone() } else { " ".repeat(label_w) };
                rows.push(Row::text(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(left, Style::default().fg(crate::kanban::category_color(category))),
                    Span::styled(line, Style::default().fg(palette().foreground)),
                ])));
            }
        }
        let _ = config;
        rows
    }

    fn saving_note(&self) -> String {
        match self.state.as_ref() {
            Some(s) if s.saving() && s.queued() > 0 => format!("  saving… ({} more queued)", s.queued()),
            Some(s) if s.saving() => "  saving…".into(),
            _ => String::new(),
        }
    }

    /// Draw the open menu, picker or confirm over `area` (the main area).
    pub(crate) fn render_modal(&mut self, f: &mut Frame, area: Rect) {
        self.hits.clear();
        let Some(modal) = self.modal.clone() else {
            return;
        };
        match modal {
            Modal::Menu { title, entries, selected } => {
                let lines: Vec<(String, String, bool)> = entries
                    .iter()
                    .map(|e| (e.label.to_string(), e.key.to_string(), e.blocked))
                    .collect();
                self.render_list(f, area, &format!(" {title} "), &lines, selected, None);
            }
            Modal::Category { id, selected } => {
                let current = self
                    .state
                    .as_ref()
                    .and_then(|s| s.config())
                    .and_then(|c| c.status(&id))
                    .map(|s| s.category);
                let lines: Vec<(String, String, bool)> = CATEGORIES
                    .iter()
                    .map(|c| {
                        let mark = if Some(*c) == current { "current" } else { "" };
                        (category_label(*c).to_string(), mark.to_string(), false)
                    })
                    .collect();
                let help = category_help(CATEGORIES[selected.min(CATEGORIES.len() - 1)]);
                self.render_list(f, area, " Category ", &lines, selected, Some(help));
            }
            Modal::Confirm { edit, message, choice } => {
                let title = match edit {
                    Edit::DeleteWorkflow { .. } => " Delete workflow ",
                    _ => " Delete status ",
                };
                self.render_confirm(f, area, title, &message, choice);
            }
            Modal::Rename { .. } | Modal::Draft { .. } => {}
        }
    }

    pub(super) fn popup(area: Rect, width: u16, height: u16) -> Option<Rect> {
        let width = width.min(area.width);
        let height = height.min(area.height);
        (width >= 12 && height >= 3).then(|| Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        })
    }

    pub(super) fn frame(f: &mut Frame, rect: Rect, title: &str) -> Rect {
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette().divider_active))
            .style(Style::default().bg(palette().background))
            .title(Span::styled(
                title.to_string(),
                Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        inner
    }

    /// A list popup: each entry's label and its note (a key, or `current`);
    /// a blocked entry dimmed; `help` under the list.
    fn render_list(
        &mut self,
        f: &mut Frame,
        area: Rect,
        title: &str,
        entries: &[(String, String, bool)],
        selected: usize,
        help: Option<&str>,
    ) {
        let help_lines = help.map(|h| shelbi_app::overlay::error_log::wrap(h, 36)).unwrap_or_default();
        let extra = if help_lines.is_empty() { 0 } else { help_lines.len() as u16 + 1 };
        let Some(rect) = Self::popup(area, 40, entries.len() as u16 + 2 + extra) else {
            return;
        };
        let inner = Self::frame(f, rect, title);
        let w = inner.width as usize;
        for (i, (label, note, blocked)) in entries.iter().enumerate() {
            let y = inner.y + i as u16;
            if y >= inner.bottom() {
                break;
            }
            let row = Rect { y, height: 1, ..inner };
            let left = format!(" {label}");
            let right = format!("{note} ");
            let gap = w.saturating_sub(panel::display_width(&left) + panel::display_width(&right));
            let text = panel::truncate_right(&format!("{left}{}{right}", " ".repeat(gap)), w);
            let style = if i == selected {
                selected_style(true)
            } else if *blocked {
                Style::default().fg(palette().muted).bg(palette().background)
            } else {
                Style::default().fg(palette().foreground).bg(palette().background)
            };
            f.render_widget(Paragraph::new(Span::styled(text, style)), row);
            self.hits.push((row, ModalHit::Entry(i)));
        }
        let first = inner.y + entries.len() as u16 + 1;
        for (y, line) in (first..inner.bottom()).zip(help_lines) {
            f.render_widget(
                Paragraph::new(Span::styled(format!(" {line}"), Style::default().fg(palette().muted))),
                Rect { y, height: 1, ..inner },
            );
        }
    }

    fn render_confirm(&mut self, f: &mut Frame, area: Rect, title: &str, message: &str, choice: usize) {
        let lines = shelbi_app::overlay::error_log::wrap(message, 44);
        let Some(rect) = Self::popup(area, 50, lines.len() as u16 + 4) else {
            return;
        };
        let inner = Self::frame(f, rect, title);
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
        for (i, (label, hit)) in [(" Delete ", ModalHit::Delete), (" Cancel ", ModalHit::Cancel)].into_iter().enumerate() {
            let w = label.chars().count() as u16;
            if x + w > inner.right() {
                break;
            }
            let style = if i == choice {
                selected_style(true)
            } else {
                Style::default().fg(palette().gray).bg(palette().selection)
            };
            let r = Rect { x, y, width: w, height: 1 };
            f.render_widget(Paragraph::new(Span::styled(label, style)), r);
            self.hits.push((r, hit));
            x += w + 1;
        }
    }
}

/// Error lines under a row, indented and red.
pub(super) fn error_rows(rows: &mut Vec<Row>, error: Option<&str>, width: usize) {
    if let Some(e) = error {
        for line in shelbi_app::overlay::error_log::wrap(e, width.saturating_sub(4).max(1)) {
            rows.push(Row::text(Line::from(Span::styled(
                format!("    {line}"),
                Style::default().fg(palette().red),
            ))));
        }
    }
}

/// A focusable row of plain text in `style`, with the selection fill when
/// `selected` (`Some(focused)`).
pub(super) fn plain_line(text: &str, selected: Option<bool>, width: usize, style: Style) -> Line<'static> {
    let marker = if selected.is_some() { "› " } else { "  " };
    let text = panel::truncate_right(&format!("{marker}{text}"), width);
    match selected {
        Some(focused) => {
            let pad = width.saturating_sub(panel::display_width(&text));
            Line::from(Span::styled(format!("{text}{}", " ".repeat(pad)), selected_style(focused)))
        }
        None => Line::from(Span::styled(text, style)),
    }
}

/// A `+ New …` or Edit YAML row, in the action color.
pub(super) fn add_line(text: &str, selected: Option<bool>, width: usize) -> Line<'static> {
    plain_line(text, selected, width, Style::default().fg(palette().cyan))
}

/// A name being typed (a new workflow or status, a rename): a field
/// (`placeholder` while empty) with the caret, then its refusal; every row
/// is item `index`'s, and the text's row holds the page's focus.
fn draft_rows(rows: &mut Vec<Row>, text: &LineEdit, placeholder: &str, refused: Option<&str>, index: usize, width: usize) {
    let typed = text.text();
    let field = Textbox::new(&typed).placeholder(placeholder).caret(Some(text.caret())).invalid(refused.is_some());
    rows.push(Row::blank());
    box_rows(rows, field, Some(index), width.min(DRAFT_W));
    if let Some(e) = refused {
        wrapped(rows, e, width, Style::default().fg(palette().red));
    }
}

/// A name field's widest (the desktop's draft field).
const DRAFT_W: usize = 40;

/// Pad or cut `text` to exactly `w` columns.
pub(super) fn cell(text: &str, w: usize) -> String {
    let t = panel::truncate_right(text, w);
    let pad = w.saturating_sub(panel::display_width(&t));
    format!("{t}{}", " ".repeat(pad))
}

/// The workflow table's columns at a width.
struct WorkflowCols {
    name: usize,
    /// 0 when there's no room for descriptions.
    description: usize,
    counts: bool,
}

const STATUSES_COL: usize = 10;
const TRANSITIONS_COL: usize = 12;

impl WorkflowCols {
    fn fit(rows: &[WorkflowSummary], width: usize) -> WorkflowCols {
        let longest = rows.iter().map(|r| r.name.chars().count() + if r.is_default { 9 } else { 0 }).max().unwrap_or(4);
        let counts = width >= 44;
        let room = width.saturating_sub(2 + if counts { STATUSES_COL + TRANSITIONS_COL } else { 0 });
        let name = (longest + 2).clamp(8, 30).min(room);
        let description = room.saturating_sub(name);
        WorkflowCols { name, description: if description >= 12 { description } else { 0 }, counts }
    }

    fn heading(&self) -> Line<'static> {
        let mut text = format!("  {}", cell("Name", self.name));
        if self.description > 0 {
            text.push_str(&cell("Description", self.description));
        }
        if self.counts {
            text.push_str(&format!("{:>STATUSES_COL$}{:>TRANSITIONS_COL$}", "Statuses", "Transitions"));
        }
        Line::from(Span::styled(text, Style::default().fg(palette().muted)))
    }

    fn line(&self, w: &WorkflowSummary, selected: Option<bool>, width: usize) -> Line<'static> {
        let marker = if selected.is_some() { "› " } else { "  " };
        let badge = if w.is_default { " default" } else { "" };
        let name_room = self.name.saturating_sub(panel::display_width(badge) + 1);
        let name = panel::truncate_right(&w.name, name_room.max(1));
        let name_pad = self.name.saturating_sub(panel::display_width(&name) + panel::display_width(badge));
        let description = if self.description > 0 {
            cell(w.description.as_deref().unwrap_or(""), self.description.saturating_sub(1)) + " "
        } else {
            String::new()
        };
        let counts = if self.counts {
            format!("{:>STATUSES_COL$}{:>TRANSITIONS_COL$}", w.statuses, w.transitions)
        } else {
            String::new()
        };
        let (base, badge_style, name_style, muted) = match selected {
            Some(focused) => {
                let s = selected_style(focused);
                (s, s.fg(palette().cyan), s, s)
            }
            None => (
                Style::default(),
                Style::default().fg(palette().cyan),
                Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
                Style::default().fg(palette().foreground),
            ),
        };
        let mut spans = vec![
            Span::styled(marker.to_string(), base),
            Span::styled(name, name_style),
            Span::styled(badge.to_string(), badge_style),
            Span::styled(" ".repeat(name_pad), base),
            Span::styled(description, muted),
            Span::styled(counts, muted),
        ];
        pad_selected(&mut spans, selected, width);
        Line::from(spans)
    }
}

/// The status table's columns at a width.
struct StatusCols {
    name: usize,
    /// 0 when there's no room.
    id: usize,
    category: usize,
    used_in: usize,
}

impl StatusCols {
    fn fit(rows: &[StatusSummary], width: usize) -> StatusCols {
        let longest_name = rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(6);
        let longest_id = rows.iter().map(|r| r.id.chars().count()).max().unwrap_or(2);
        let room = width.saturating_sub(4);
        let name = (longest_name + 2).clamp(8, 24).min(room);
        let category = 10.min(room.saturating_sub(name));
        let mut left = room.saturating_sub(name + category);
        let id = if width >= 60 { (longest_id + 2).clamp(6, 22).min(left) } else { 0 };
        left = left.saturating_sub(id);
        let used_in = if left >= 8 { left } else { 0 };
        StatusCols { name, id, category, used_in }
    }

    fn heading(&self) -> Line<'static> {
        let mut text = format!("    {}", cell("Status", self.name));
        if self.id > 0 {
            text.push_str(&cell("ID", self.id));
        }
        text.push_str(&cell("Category", self.category));
        if self.used_in > 0 {
            text.push_str(&cell("Used in", self.used_in));
        }
        Line::from(Span::styled(text, Style::default().fg(palette().muted)))
    }

    fn tail(&self, s: &StatusSummary) -> (String, String, String) {
        let id = if self.id > 0 { cell(&s.id, self.id) } else { String::new() };
        let category = cell(category_label(s.category), self.category);
        let used = if self.used_in == 0 {
            String::new()
        } else if s.used_in.is_empty() {
            cell("—", self.used_in)
        } else {
            cell(&s.used_in.join(", "), self.used_in)
        };
        (id, category, used)
    }

    fn line(&self, s: &StatusSummary, selected: Option<bool>, width: usize) -> Line<'static> {
        let marker = if selected.is_some() { "› " } else { "  " };
        let color = crate::kanban::category_color(s.category);
        let (id, category, used) = self.tail(s);
        let (base, glyph, name, muted, cat) = match selected {
            Some(focused) => {
                let st = selected_style(focused);
                (st, st.fg(color), st, st, st)
            }
            None => (
                Style::default(),
                Style::default().fg(color),
                Style::default().fg(palette().white),
                Style::default().fg(palette().foreground),
                Style::default().fg(color),
            ),
        };
        let mut spans = vec![
            Span::styled(marker.to_string(), base),
            Span::styled("● ", glyph),
            Span::styled(cell(&s.name, self.name), name),
            Span::styled(id, muted),
            Span::styled(category, cat),
            Span::styled(used, muted),
        ];
        pad_selected(&mut spans, selected, width);
        Line::from(spans)
    }
}

/// Fill a selected row's line to `width` with the selection color.
pub(super) fn pad_selected(spans: &mut Vec<Span<'static>>, selected: Option<bool>, width: usize) {
    let used: usize = spans.iter().map(|s| panel::display_width(&s.content)).sum();
    if let Some(focused) = selected {
        spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), selected_style(focused)));
    }
}

/// A one-line text field: its text and the caret's position (in chars).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct LineEdit {
    chars: Vec<char>,
    caret: usize,
}

impl LineEdit {
    pub(super) fn new(text: &str) -> LineEdit {
        let chars: Vec<char> = text.chars().collect();
        LineEdit { caret: chars.len(), chars }
    }

    pub(crate) fn text(&self) -> String {
        self.chars.iter().collect()
    }

    /// The caret's position, in chars.
    pub(crate) fn caret(&self) -> usize {
        self.caret
    }

    pub(super) fn insert(&mut self, c: char) {
        self.chars.insert(self.caret, c);
        self.caret += 1;
    }

    /// Edit with `k`: printable characters type; Backspace, Delete, ←/→,
    /// Home/End (and Ctrl+A/E), Ctrl+U clears to the start.
    pub(super) fn key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('a') if ctrl => self.caret = 0,
            KeyCode::Char('e') if ctrl => self.caret = self.chars.len(),
            KeyCode::Char('u') if ctrl => {
                self.chars.drain(..self.caret);
                self.caret = 0;
            }
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => self.insert(c),
            KeyCode::Backspace if self.caret > 0 => {
                self.caret -= 1;
                self.chars.remove(self.caret);
            }
            KeyCode::Delete if self.caret < self.chars.len() => {
                self.chars.remove(self.caret);
            }
            KeyCode::Left => self.caret = self.caret.saturating_sub(1),
            KeyCode::Right => self.caret = (self.caret + 1).min(self.chars.len()),
            KeyCode::Home => self.caret = 0,
            KeyCode::End => self.caret = self.chars.len(),
            _ => {}
        }
    }
}

#[cfg(test)]
pub(super) mod tests;
