//! Settings ▸ Agents in the TUI (`tui-settings-agents`), after the desktop
//! app's page (`desktop/src/views/agents.rs`) over the same toolkit-free
//! state, `shelbi_app::settings::agents`. Every change goes through
//! `shelbi_state::agents_config`; this page never touches the files.
//!
//! Top to bottom: the Agents header and New agent, the table of the
//! project's agents (name with a Customized badge and the description, the
//! preferred runner with Recommended under it, the workflow statuses that
//! name the agent, the skill count, and a `⋮` for the row's actions), the
//! footnote, then Shared instructions: a preview of
//! `agents/_shared/preamble.md` with Edit instructions, which opens an
//! in-place multi-line editor with Save and Cancel. At a narrow width the
//! table stacks each agent's fields under its name; every action stays on a
//! key and in the row's menu.
//!
//! Keys on the page (the sidebar keymap's up / down / activate, as
//! elsewhere in Settings): Enter opens the focused row (an agent: its
//! editor; New agent, Edit instructions), `m` opens the agent's actions
//! (Edit, Edit YAML, Duplicate, Reset to default, Delete; the ones the model
//! refuses are listed with why), `y` is Edit YAML, `n` New agent. New agent
//! and Duplicate type the name into a row at the table's end (Enter creates,
//! Esc cancels). Reset and Delete ask under the row first, Cancel focused.
//! In the shared-instructions editor every key types (Enter a line break),
//! Ctrl+S saves, Tab steps to Save and Cancel, and Esc closes it, through
//! the unsaved-edits prompt when it holds changes.
//!
//! Saves run one at a time off the UI thread and queue behind each other
//! (the shell runs each [`CommitJob`]); loads too. Both carry the page's
//! [`AgentsPage::id`], new for each project, so a result that lands after a
//! project switch never reaches the new project's page.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use shelbi_app::settings::agents::{
    preview, runner_label, status_chips, unreadable_message, AgentsState, Committed, Confirm, Edit,
    RowAction, StatusChip, Target, AGENTS_HELPER, NOT_ASSIGNED, NO_SHARED, PREVIEW_LINES,
    SHARED_FOOTNOTE, SHARED_HELPER, SHARED_TITLE, UNSET_RUNNER,
};
use shelbi_app::settings::{Job, JobState, SettingsPage, Ticket};
use shelbi_app::text_area::TextArea;
use shelbi_app::Effect;
use shelbi_state::agents_config::{AgentSummary, AgentsConfig, Provenance};
use shelbi_state::keymap::{Key, Keymaps, SidebarAction};

use super::{heading, selected_style, wrapped, Hit, Row, OFFERED};
use crate::panel;
use crate::theme::palette;

/// Below this page width the table stacks each agent's fields.
const TABLE_MIN_W: usize = 72;
/// The table's fixed columns.
const MARK_W: usize = 2;
const RUNNER_W: usize = 18;
const SKILLS_W: usize = 7;
const MORE_W: usize = 2;
/// The editor's left rule.
const EDITOR_PREFIX: &str = "│ ";

/// A row the page's keys stop on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    New,
    /// An agent's row (one whose files don't read too).
    Agent(String),
    /// The footnote's Workflows link (once the TUI offers that page).
    Link,
    EditShared,
    /// The shared instructions' editor, while open.
    Editor,
    /// No project: switch to one or add one.
    SwitchProject,
    AddProject,
    /// The agents didn't read: read them again.
    Retry,
}

/// What a click on the page hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AgentsHit {
    Item(usize),
    /// A row's `⋮`.
    More(usize),
    MenuItem(usize),
    ConfirmCancel,
    ConfirmGo,
    /// A wrapped line of the editor (its index among them all).
    EditorLine(usize),
    EditorSave,
    EditorCancel,
}

/// Where focus sits inside the open editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorFocus {
    Text,
    Save,
    Cancel,
}

struct Editor {
    text: TextArea,
    focus: EditorFocus,
    /// The first wrapped line shown.
    top: usize,
    /// The wrapped lines from the last draw (`(line, start)`), for clicks.
    rows: Vec<(usize, usize)>,
    /// Lines shown at once (Page Up / Page Down move this far).
    height: usize,
}

/// One save for the shell to run off the UI thread: [`commit`] `edit` onto
/// `config` for `project`, then hand the result back under `id`.
///
/// [`commit`]: shelbi_app::settings::agents::commit
#[derive(Debug, Clone)]
pub(crate) struct CommitJob {
    pub id: u64,
    pub project: String,
    pub config: AgentsConfig,
    pub edit: Edit,
}

impl PartialEq for CommitJob {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.project == other.project && self.edit == other.edit
    }
}

impl Eq for CommitJob {}

/// One item of a row's actions menu, as drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MenuLine {
    /// The action and its key.
    pub label: String,
    /// Why the model refuses it (`None`: offered).
    pub why: Option<String>,
    pub selected: bool,
}

/// What a key or click on the page asks the Settings shell for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Outcome {
    Stay,
    /// Not the page's key: the shell's (Esc, Tab, a page digit, scrolling).
    Pass,
    Commit(CommitJob),
    Run(Effect),
    /// Open this file in the user's editor; the page reads again after.
    EditFile(PathBuf),
    /// Close the shared-instructions editor (through the unsaved-edits
    /// prompt).
    CloseEditor,
    ShowPage(SettingsPage),
    Reload,
    /// Open agent `.0` in the agent editor.
    OpenAgent(String),
}

/// The Agents page for one project. A project switch makes a new one.
pub(crate) struct AgentsPage {
    id: u64,
    project: Option<String>,
    state: AgentsState,
    load: Job<()>,
    focus: Item,
    /// The agent whose actions menu is open, and its selected item.
    menu: Option<(String, usize)>,
    /// The confirm strip's focused button: 0 Cancel, 1 the action.
    confirm_choice: usize,
    /// The new row's name.
    draft: TextArea,
    editor: Option<Editor>,
    /// Where the focus goes once the next read lands (the agent editor
    /// closed onto this page).
    pending_focus: Option<super::AgentBack>,
    /// An agent just created or duplicated, for Settings to open in the
    /// agent editor.
    opened: Option<String>,
    /// What the agent editor said as it closed (its agent gone from the
    /// files), until the next edit.
    note: Option<String>,
}

impl AgentsPage {
    pub(crate) fn new(id: u64, project: Option<&str>) -> Self {
        AgentsPage {
            id,
            project: project.map(str::to_string),
            state: AgentsState::new(project.unwrap_or_default()),
            load: Job::default(),
            focus: Item::New,
            menu: None,
            confirm_choice: 0,
            draft: TextArea::default(),
            editor: None,
            pending_focus: None,
            opened: None,
            note: None,
        }
    }

    /// Put the focus on `back` (an agent's row, or the shared
    /// instructions) now if it's listed, and again once the next read
    /// lands (a renamed or new agent shows then).
    pub(crate) fn select_after_load(&mut self, back: super::AgentBack) {
        self.apply_focus(&back);
        self.pending_focus = Some(back);
    }

    fn apply_focus(&mut self, back: &super::AgentBack) -> bool {
        let item = match back {
            super::AgentBack::Agent(name) => Item::Agent(name.clone()),
            super::AgentBack::Shared => Item::EditShared,
        };
        let listed = self.items().contains(&item);
        if listed {
            self.focus = item;
        }
        listed
    }

    /// The agent just created or duplicated, once.
    pub(crate) fn take_opened(&mut self) -> Option<String> {
        self.opened.take()
    }

    /// Say `note` over the page (until the next edit).
    pub(crate) fn set_notice(&mut self, note: String) {
        self.note = Some(note);
    }

    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    pub(crate) fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    pub(crate) fn state(&self) -> &AgentsState {
        &self.state
    }

    /// Take the state (a project switch while it saves: the shell finishes
    /// its queue).
    pub(crate) fn into_state(self) -> AgentsState {
        self.state
    }

    #[cfg(test)]
    pub(crate) fn focus(&self) -> &Item {
        &self.focus
    }

    pub(crate) fn menu_open(&self) -> bool {
        self.menu.is_some()
    }

    #[cfg(test)]
    pub(crate) fn editor_text(&self) -> Option<String> {
        self.editor.as_ref().map(|e| e.text.text())
    }


    // --- loads and saves ---------------------------------------------------

    /// Start a read of the agents (each time the page is shown, after an
    /// editor returns, `r`): its ticket, or `None` with no project or one
    /// already running and `force` off.
    pub(crate) fn begin_load(&mut self, force: bool) -> Option<Ticket> {
        let project = self.project.clone()?;
        if self.load.running() && !force {
            return None;
        }
        Some(self.load.begin(Some(&project)))
    }

    /// A read landed. Returns whether it was taken (a superseded one is
    /// dropped; a read landing while a save runs is dropped too, as the
    /// save hands back its own fresh model).
    pub(crate) fn loaded(&mut self, ticket: &Ticket, result: Result<AgentsConfig, String>) -> bool {
        if !self.load.is_current(ticket) {
            return false;
        }
        self.load.complete(ticket, result.as_ref().map(|_| ()).map_err(Clone::clone));
        if result.is_err() || !self.state.saving() {
            self.state.loaded(result);
        }
        if let Some(back) = self.pending_focus.take() {
            self.apply_focus(&back);
        }
        self.settle_focus();
        true
    }

    fn job(&self, (config, edit): (AgentsConfig, Edit)) -> Outcome {
        Outcome::Commit(CommitJob {
            id: self.id,
            project: self.state.project().to_string(),
            config,
            edit,
        })
    }

    fn request(&mut self, edit: Edit) -> Outcome {
        self.menu = None;
        self.note = None;
        match self.state.request(edit) {
            Some(job) => self.job(job),
            None => Outcome::Stay,
        }
    }

    /// A save landed: the next queued one to run, if any.
    pub(crate) fn committed(&mut self, done: Committed) -> Option<CommitJob> {
        let created = done.created().map(str::to_string);
        let next = self.state.committed(done);
        if !self.state.editing_shared {
            self.editor = None;
            if self.focus == Item::Editor {
                self.focus = Item::EditShared;
            }
        }
        if let Some(name) = created {
            // The new row takes the focus, and Settings opens it in the
            // agent editor (as the desktop's page does).
            self.draft = TextArea::default();
            self.focus = Item::Agent(name.clone());
            self.opened = Some(name);
        }
        self.settle_focus();
        next.map(|job| CommitJob {
            id: self.id,
            project: self.state.project().to_string(),
            config: job.0,
            edit: job.1,
        })
    }

    // --- the unsaved-edits prompt -------------------------------------------

    /// The shared-instructions editor holds text that isn't saved.
    pub(crate) fn dirty(&self) -> bool {
        self.editor
            .as_ref()
            .is_some_and(|e| e.text.text() != self.state.shared_instructions())
    }

    /// The prompt's Save: save the editor's text.
    pub(crate) fn save_shared(&mut self) -> Option<CommitJob> {
        let text = self.editor.as_ref()?.text.text();
        match self.request(Edit::SetSharedInstructions { text }) {
            Outcome::Commit(job) => Some(job),
            _ => None,
        }
    }

    /// Close the editor, dropping its text (Cancel, the prompt's Discard).
    pub(crate) fn close_editor(&mut self) {
        self.editor = None;
        self.state.editing_shared = false;
        self.state.clear_error(&Target::Shared);
        if self.focus == Item::Editor {
            self.focus = Item::EditShared;
        }
    }

    /// Whether the page takes every key (a menu, a confirm, the new row or
    /// the focused editor): Esc, Tab and digits are its own then.
    pub(crate) fn captures(&self) -> bool {
        self.menu.is_some()
            || self.state.confirm.is_some()
            || self.state.draft.is_some()
            || (self.editor.is_some() && self.focus == Item::Editor)
    }

    /// Text pasted (or a file dropped) over the page: into the focused
    /// editor or the new row's name. Returns whether one took it.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        if self.state.draft.is_some() {
            self.draft.insert_str(&text.replace(['\r', '\n'], ""));
        } else if let Some(editor) = self.editor.as_mut().filter(|_| self.focus == Item::Editor) {
            editor.focus = EditorFocus::Text;
            editor.text.insert_str(text);
        } else {
            return false;
        }
        true
    }

    // --- focus ----------------------------------------------------------------

    /// The rows the keys stop on, top to bottom.
    fn items(&self) -> Vec<Item> {
        if self.project.is_none() {
            return vec![Item::SwitchProject, Item::AddProject];
        }
        let Some(config) = self.state.config() else {
            return if matches!(self.load.state(), JobState::Failed(_)) { vec![Item::Retry] } else { Vec::new() };
        };
        let mut items = vec![Item::New];
        items.extend(config.agents().map(|a| Item::Agent(a.name().to_string())));
        items.extend(config.unreadable().iter().map(|u| Item::Agent(u.name.clone())));
        if OFFERED.contains(SettingsPage::Workflows) {
            items.push(Item::Link);
        }
        items.push(if self.editor.is_some() { Item::Editor } else { Item::EditShared });
        items
    }

    /// Keep the focus on a row that exists (a deleted agent's goes to the
    /// row now in its place).
    fn settle_focus(&mut self) {
        let items = self.items();
        if items.is_empty() || items.contains(&self.focus) {
            return;
        }
        self.focus = match &self.focus {
            Item::Agent(_) => items
                .iter()
                .find(|i| matches!(i, Item::Agent(_)))
                .cloned()
                .unwrap_or(Item::New),
            Item::Editor => Item::EditShared,
            Item::EditShared => Item::Editor,
            _ => items[0].clone(),
        };
        if !items.contains(&self.focus) {
            self.focus = items[0].clone();
        }
    }

    fn step(&mut self, down: bool) {
        let items = self.items();
        if items.is_empty() {
            return;
        }
        let at = items.iter().position(|i| *i == self.focus).unwrap_or(0);
        let next = if down { (at + 1).min(items.len() - 1) } else { at.saturating_sub(1) };
        self.focus = items[next].clone();
        if let (Item::Editor, Some(editor)) = (&self.focus, self.editor.as_mut()) {
            editor.focus = EditorFocus::Text;
        }
    }

    fn unreadable(&self, name: &str) -> Option<PathBuf> {
        self.state
            .config()?
            .unreadable()
            .iter()
            .find(|u| u.name == name)
            .map(|u| u.path.clone())
    }

    // --- actions --------------------------------------------------------------

    /// Open agent `name` for editing: the one hook a row's Edit goes
    /// through, into the agent editor (`tui-agent-editor`).
    fn open_agent(&self, name: &str) -> Outcome {
        Outcome::OpenAgent(name.to_string())
    }

    /// Open agent `name`'s `agent.yaml` in the user's editor: Edit YAML, the
    /// one action an agent whose files don't read offers (that's how it gets
    /// fixed). The page reads again when the editor returns.
    fn edit_yaml(&self, name: &str) -> Outcome {
        let dir = match (self.unreadable(name), self.state.config()) {
            (Some(dir), _) => dir,
            (None, Some(config)) => config.agent_dir(name),
            (None, None) => return Outcome::Stay,
        };
        Outcome::EditFile(dir.join("agent.yaml"))
    }

    fn open_draft(&mut self, from: Option<String>) {
        if self.state.config().is_none() {
            return;
        }
        self.menu = None;
        self.state.confirm = None;
        self.state.clear_error(&Target::NewAgent);
        self.draft = match &from {
            Some(from) => TextArea::from_text(&self.state.duplicate_name(from)),
            None => TextArea::default(),
        };
        self.draft.end();
        self.state.draft = Some(from);
    }

    fn open_editor(&mut self) {
        if self.state.config().is_none() {
            return;
        }
        self.state.clear_error(&Target::Shared);
        self.editor = Some(Editor {
            text: TextArea::from_text(self.state.shared_instructions()),
            focus: EditorFocus::Text,
            top: 0,
            rows: Vec::new(),
            height: 8,
        });
        self.state.editing_shared = true;
        self.focus = Item::Editor;
    }

    fn open_menu(&mut self, name: &str) {
        self.state.confirm = None;
        self.menu = Some((name.to_string(), 0));
        self.focus = Item::Agent(name.to_string());
    }

    fn run_row_action(&mut self, name: &str, action: RowAction) -> Outcome {
        let blocked = self
            .state
            .row_actions(name)
            .into_iter()
            .find(|(a, _)| *a == action)
            .map(|(_, why)| why);
        // Not offered for this row (Edit on one that doesn't read), or the
        // model refuses it: the menu says why; nothing runs.
        if !matches!(blocked, Some(None)) {
            return Outcome::Stay;
        }
        self.menu = None;
        match action {
            RowAction::Edit => self.open_agent(name),
            RowAction::EditYaml => self.edit_yaml(name),
            RowAction::Duplicate => {
                self.open_draft(Some(name.to_string()));
                Outcome::Stay
            }
            RowAction::Reset | RowAction::Delete => {
                let confirm = if action == RowAction::Reset {
                    Confirm::Reset(name.to_string())
                } else {
                    Confirm::Delete(name.to_string())
                };
                self.state.ask(confirm);
                self.confirm_choice = 0;
                Outcome::Stay
            }
        }
    }

    fn activate(&mut self) -> Outcome {
        match self.focus.clone() {
            Item::New => {
                self.open_draft(None);
                Outcome::Stay
            }
            Item::Agent(name) if self.unreadable(&name).is_some() => self.edit_yaml(&name),
            Item::Agent(name) => self.open_agent(&name),
            Item::Link => Outcome::ShowPage(SettingsPage::Workflows),
            Item::EditShared => {
                self.open_editor();
                Outcome::Stay
            }
            Item::Editor => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.focus = EditorFocus::Text;
                }
                Outcome::Stay
            }
            Item::SwitchProject => Outcome::Run(Effect::SwitchProject { project: String::new() }),
            Item::AddProject => Outcome::Run(Effect::AddProject),
            Item::Retry => Outcome::Reload,
        }
    }

    fn focused_agent(&self) -> Option<String> {
        match &self.focus {
            Item::Agent(name) => Some(name.clone()),
            _ => None,
        }
    }

    // --- keys -------------------------------------------------------------------

    /// A key while the page has focus.
    pub(crate) fn handle_key(&mut self, k: KeyEvent, keymaps: &Keymaps) -> Outcome {
        self.settle_focus();
        let chord = crate::keymap::chord_from_event(k);
        if self.menu.is_some() {
            return self.menu_key(k, chord.and_then(|c| keymaps.sidebar.dispatch(c)));
        }
        if self.state.confirm.is_some() {
            return self.confirm_key(k);
        }
        if self.state.draft.is_some() {
            return self.draft_key(k, chord);
        }
        if self.editor.is_some() && self.focus == Item::Editor {
            return self.editor_key(k, chord);
        }
        let sidebar = chord.and_then(|c| keymaps.sidebar.dispatch(c));
        let plain = k.modifiers.difference(KeyModifiers::SHIFT).is_empty();
        if k.code == KeyCode::Up || sidebar == Some(SidebarAction::NavUp) {
            self.step(false);
            return Outcome::Stay;
        }
        if k.code == KeyCode::Down || sidebar == Some(SidebarAction::NavDown) {
            self.step(true);
            return Outcome::Stay;
        }
        if sidebar == Some(SidebarAction::Activate) || matches!(chord.map(|c| c.code), Some(Key::Enter)) {
            return self.activate();
        }
        match k.code {
            KeyCode::Char('m') if plain => {
                if let Some(name) = self.focused_agent() {
                    self.open_menu(&name);
                }
                Outcome::Stay
            }
            KeyCode::Char('y') if plain => match self.focused_agent() {
                Some(name) => self.run_row_action(&name, RowAction::EditYaml),
                None => Outcome::Stay,
            },
            KeyCode::Char('n') if plain && self.project.is_some() => {
                self.open_draft(None);
                Outcome::Stay
            }
            _ => Outcome::Pass,
        }
    }

    fn menu_key(&mut self, k: KeyEvent, sidebar: Option<SidebarAction>) -> Outcome {
        let Some((name, at)) = self.menu.clone() else {
            return Outcome::Stay;
        };
        let actions = self.state.row_actions(&name);
        let pick = |a: RowAction| actions.iter().position(|(b, _)| *b == a);
        let chosen = match k.code {
            KeyCode::Esc => {
                self.menu = None;
                return Outcome::Stay;
            }
            KeyCode::Char(c @ ('e' | 'y' | 'd' | 'r' | 'x'))
                if k.modifiers.difference(KeyModifiers::SHIFT).is_empty() =>
            {
                pick(match c {
                    'e' => RowAction::Edit,
                    'y' => RowAction::EditYaml,
                    'd' => RowAction::Duplicate,
                    'r' => RowAction::Reset,
                    _ => RowAction::Delete,
                })
            }
            KeyCode::Up => self.move_menu(at.saturating_sub(1)),
            KeyCode::Down => self.move_menu((at + 1).min(actions.len().saturating_sub(1))),
            KeyCode::Enter => Some(at),
            _ => match sidebar {
                Some(SidebarAction::NavUp) => self.move_menu(at.saturating_sub(1)),
                Some(SidebarAction::NavDown) => self.move_menu((at + 1).min(actions.len().saturating_sub(1))),
                Some(SidebarAction::Activate) => Some(at),
                _ => None,
            },
        };
        match chosen.and_then(|i| actions.get(i).map(|(a, _)| *a)) {
            Some(action) => {
                if let Some((_, sel)) = self.menu.as_mut() {
                    *sel = actions.iter().position(|(a, _)| *a == action).unwrap_or(*sel);
                }
                self.run_row_action(&name, action)
            }
            None => Outcome::Stay,
        }
    }

    /// Select the menu's item `at`; nothing to run.
    fn move_menu(&mut self, at: usize) -> Option<usize> {
        if let Some((_, sel)) = self.menu.as_mut() {
            *sel = at;
        }
        None
    }

    fn confirm_key(&mut self, k: KeyEvent) -> Outcome {
        match k.code {
            KeyCode::Esc => {
                self.state.confirm = None;
                Outcome::Stay
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.confirm_choice = 1 - self.confirm_choice.min(1);
                Outcome::Stay
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.press_confirm(self.confirm_choice == 1),
            _ => Outcome::Stay,
        }
    }

    fn press_confirm(&mut self, go: bool) -> Outcome {
        let Some(confirm) = self.state.confirm.clone() else {
            return Outcome::Stay;
        };
        if !go {
            self.state.confirm = None;
            return Outcome::Stay;
        }
        self.request(confirm.edit())
    }

    fn draft_key(&mut self, k: KeyEvent, chord: Option<shelbi_state::keymap::KeyChord>) -> Outcome {
        match k.code {
            KeyCode::Esc => {
                self.state.draft = None;
                self.state.clear_error(&Target::NewAgent);
                Outcome::Stay
            }
            KeyCode::Enter => self.submit_draft(),
            _ => {
                if let Some(chord) = chord {
                    self.draft.edit(chord, true, 1);
                }
                Outcome::Stay
            }
        }
    }

    fn submit_draft(&mut self) -> Outcome {
        let name = self.draft.text().trim().to_string();
        let edit = match self.state.draft.clone() {
            None => return Outcome::Stay,
            Some(_) if name.is_empty() => return Outcome::Stay,
            Some(None) => Edit::CreateAgent { name },
            Some(Some(from)) => Edit::DuplicateAgent { from, name },
        };
        self.request(edit)
    }

    fn editor_key(&mut self, k: KeyEvent, chord: Option<shelbi_state::keymap::KeyChord>) -> Outcome {
        let Some(editor) = self.editor.as_mut() else {
            return Outcome::Stay;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('s') | KeyCode::Char('S') if ctrl => return self.save_editor(),
            KeyCode::Esc => return Outcome::CloseEditor,
            KeyCode::Tab => {
                editor.focus = match editor.focus {
                    EditorFocus::Text => EditorFocus::Save,
                    EditorFocus::Save => EditorFocus::Cancel,
                    EditorFocus::Cancel => EditorFocus::Text,
                };
                return Outcome::Stay;
            }
            KeyCode::BackTab => {
                editor.focus = match editor.focus {
                    EditorFocus::Text => EditorFocus::Cancel,
                    EditorFocus::Save => EditorFocus::Text,
                    EditorFocus::Cancel => EditorFocus::Save,
                };
                return Outcome::Stay;
            }
            _ => {}
        }
        match editor.focus {
            EditorFocus::Text => {
                if let Some(chord) = chord {
                    let page = editor.height;
                    editor.text.edit(chord, false, page);
                }
                Outcome::Stay
            }
            EditorFocus::Save | EditorFocus::Cancel => match k.code {
                KeyCode::Left | KeyCode::Right => {
                    editor.focus = if editor.focus == EditorFocus::Save { EditorFocus::Cancel } else { EditorFocus::Save };
                    Outcome::Stay
                }
                KeyCode::Up => {
                    editor.focus = EditorFocus::Text;
                    Outcome::Stay
                }
                KeyCode::Enter | KeyCode::Char(' ') if editor.focus == EditorFocus::Save => self.save_editor(),
                KeyCode::Enter | KeyCode::Char(' ') => Outcome::CloseEditor,
                _ => Outcome::Stay,
            },
        }
    }

    fn save_editor(&mut self) -> Outcome {
        let Some(editor) = self.editor.as_ref() else {
            return Outcome::Stay;
        };
        let text = editor.text.text();
        self.request(Edit::SetSharedInstructions { text })
    }

    // --- mouse ------------------------------------------------------------------

    /// A click on `hit`; `column` is the click's offset from the page's text
    /// start (placing the caret in the editor).
    pub(crate) fn click(&mut self, hit: AgentsHit, column: u16) -> Outcome {
        match hit {
            AgentsHit::Item(i) => {
                let Some(item) = self.items().get(i).cloned() else {
                    return Outcome::Stay;
                };
                // A first click selects, a click on the selected row opens it.
                if item == self.focus {
                    self.activate()
                } else {
                    self.focus = item;
                    Outcome::Stay
                }
            }
            AgentsHit::More(i) => {
                if let Some(Item::Agent(name)) = self.items().get(i).cloned() {
                    self.open_menu(&name);
                }
                Outcome::Stay
            }
            AgentsHit::MenuItem(i) => {
                let Some((name, _)) = self.menu.clone() else {
                    return Outcome::Stay;
                };
                match self.state.row_actions(&name).get(i).map(|(a, _)| *a) {
                    Some(action) => {
                        self.menu = Some((name.clone(), i));
                        self.run_row_action(&name, action)
                    }
                    None => Outcome::Stay,
                }
            }
            AgentsHit::ConfirmCancel => self.press_confirm(false),
            AgentsHit::ConfirmGo => self.press_confirm(true),
            AgentsHit::EditorLine(row) => {
                self.focus = Item::Editor;
                if let Some(editor) = self.editor.as_mut() {
                    editor.focus = EditorFocus::Text;
                    if let Some((line, start)) = editor.rows.get(row).copied() {
                        let col = (column as usize).saturating_sub(EDITOR_PREFIX.chars().count());
                        editor.text.move_to(line, start + col);
                    }
                }
                Outcome::Stay
            }
            AgentsHit::EditorSave => {
                self.focus = Item::Editor;
                self.save_editor()
            }
            AgentsHit::EditorCancel => {
                self.focus = Item::Editor;
                Outcome::CloseEditor
            }
        }
    }

    /// Close the menu (a click outside it).
    pub(crate) fn close_menu(&mut self) -> bool {
        self.menu.take().is_some()
    }

    // --- drawing ------------------------------------------------------------------

    /// The key hints while the page has focus.
    pub(crate) fn hints(&self, moves: &str) -> String {
        if self.menu.is_some() {
            return format!("{moves} choose · ⏎ run · e y d r x · Esc close");
        }
        if self.state.confirm.is_some() {
            return "←→ choose · ⏎ press · Esc cancel".into();
        }
        if self.state.draft.is_some() {
            return "Type a name · ⏎ create · Esc cancel".into();
        }
        if self.editor.is_some() && self.focus == Item::Editor {
            return "Ctrl+S save · Tab buttons · Esc cancel".into();
        }
        if self.project.is_none() {
            return format!("{moves} choose · ⏎ run · Tab sidebar · Esc back");
        }
        format!("{moves} choose · ⏎ open · m actions · y yaml · n new · r reload · Tab sidebar · Esc back")
    }

    /// The page's rows at `width` (the content's height `height`), focus
    /// drawn when `focused`.
    pub(crate) fn rows(&mut self, width: usize, height: usize, focused: bool) -> Vec<Row> {
        self.settle_focus();
        let mut rows = vec![Row::text(Line::from(Span::styled(
            "Agents",
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        )))];
        wrapped(&mut rows, AGENTS_HELPER, width, Style::default().fg(palette().muted));
        if let Some(e) = self.state.error_at(&Target::Page) {
            wrapped(&mut rows, e, width, Style::default().fg(palette().red));
        }
        if let Some(n) = self.state.notice().or(self.note.as_deref()) {
            wrapped(&mut rows, n, width, Style::default().fg(palette().yellow));
        }
        if self.state.saving() {
            rows.push(Row::text(Line::from(Span::styled(
                "Saving…",
                Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
            ))));
        }
        rows.push(Row::blank());
        let items = self.items();
        let index = |item: &Item| items.iter().position(|i| i == item);
        // The focused row draws as selected unless something under it (a
        // menu, a confirm, the new row) holds the keys.
        let quiet = focused && self.menu.is_none() && self.state.confirm.is_none() && self.state.draft.is_none();
        if self.project.is_none() {
            wrapped(&mut rows, "No project is open.", width, Style::default().fg(palette().foreground));
            wrapped(&mut rows, "Switch to a project, or add one, to see its agents.", width, Style::default().fg(palette().muted));
            rows.push(Row::blank());
            for (item, label) in [(Item::SwitchProject, "Switch project…"), (Item::AddProject, "Add project…")] {
                let i = index(&item);
                rows.push(self.button_row(label, None, i, quiet && self.focus == item, width));
            }
            return rows;
        }
        let Some(config) = self.state.config() else {
            match self.load.state() {
                JobState::Failed(_) => {
                    wrapped(&mut rows, "Press r, or choose Try again.", width, Style::default().fg(palette().muted));
                    rows.push(self.button_row("Try again", None, index(&Item::Retry), quiet && self.focus == Item::Retry, width));
                }
                _ => rows.push(Row::text(Line::from(Span::styled(
                    "Loading…",
                    Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
                )))),
            }
            return rows;
        };
        let summaries = config.summaries();
        let unreadable: Vec<(String, String)> =
            config.unreadable().iter().map(|u| (u.name.clone(), u.error.clone())).collect();
        rows.push(self.button_row("+ New agent", Some("n"), index(&Item::New), quiet && self.focus == Item::New, width));
        rows.push(Row::blank());
        let table = width >= TABLE_MIN_W;
        if table {
            rows.push(Row::text(table_header(width)));
        }
        rows.push(Row::text(rule(width)));
        for a in &summaries {
            let item = Item::Agent(a.name.clone());
            let i = index(&item);
            let selected = focused && self.focus == item && self.menu.is_none() && self.state.confirm.is_none();
            let lines = if table { table_row(a, width, selected) } else { stacked_row(a, width, selected) };
            push_item(&mut rows, lines, i, selected && self.state.draft.is_none(), width);
            self.under_row(&mut rows, &a.name, width);
        }
        for (name, error) in &unreadable {
            let item = Item::Agent(name.clone());
            let i = index(&item);
            let selected = focused && self.focus == item && self.menu.is_none() && self.state.confirm.is_none();
            let lines = unreadable_row(name, error, width, selected);
            push_item(&mut rows, lines, i, selected && self.state.draft.is_none(), width);
            self.under_row(&mut rows, name, width);
        }
        if summaries.is_empty() && unreadable.is_empty() {
            wrapped(&mut rows, "No agents yet.", width, Style::default().fg(palette().muted));
        }
        if let Some(from) = self.state.draft.clone() {
            self.draft_rows(&mut rows, from.as_deref(), width);
        }
        rows.push(Row::text(rule(width)));
        if OFFERED.contains(SettingsPage::Workflows) {
            wrapped(&mut rows, "Assign agents to workflow statuses on the Workflows page. Runner and model preferences can be changed for each role.", width, Style::default().fg(palette().muted));
            rows.push(self.button_row("Workflows", Some(&format!("page {}", SettingsPage::Workflows.shortcut())), index(&Item::Link), quiet && self.focus == Item::Link, width));
        } else {
            wrapped(&mut rows, "Agents are assigned to workflow statuses in each workflow's YAML. Runner and model preferences can be changed for each role.", width, Style::default().fg(palette().muted));
        }
        rows.push(Row::blank());
        heading(&mut rows, SHARED_TITLE);
        wrapped(&mut rows, SHARED_HELPER, width, Style::default().fg(palette().muted));
        rows.push(Row::blank());
        if self.editor.is_some() {
            self.editor_rows(&mut rows, width, height, focused);
        } else {
            preview_rows(&mut rows, self.state.shared_instructions(), width);
            rows.push(Row::blank());
            rows.push(self.button_row(
                "Edit instructions",
                Some("in place"),
                index(&Item::EditShared),
                quiet && self.focus == Item::EditShared,
                width,
            ));
        }
        if let Some(e) = self.state.error_at(&Target::Shared) {
            wrapped(&mut rows, e, width, Style::default().fg(palette().red));
        }
        wrapped(&mut rows, SHARED_FOOTNOTE, width, Style::default().fg(palette().muted));
        rows
    }

    /// A one-line action row: `› label   note`.
    fn button_row(&self, label: &str, note: Option<&str>, item: Option<usize>, selected: bool, width: usize) -> Row {
        let marker = if selected { "› " } else { "  " };
        let mut text = format!("{marker}{}", panel::truncate_right(label, width.saturating_sub(2)));
        if let Some(note) = note {
            if panel::display_width(&text) + 2 + note.chars().count() <= width {
                text.push_str("  ");
                text.push_str(note);
            }
        }
        let line = if selected {
            let pad = width.saturating_sub(panel::display_width(&text));
            Line::from(Span::styled(format!("{text}{}", " ".repeat(pad)), selected_style(true)))
        } else {
            Line::from(Span::styled(text, Style::default().fg(palette().cyan)))
        };
        match item {
            Some(i) => Row::hit(line, Hit::Agents(AgentsHit::Item(i)), selected),
            None => Row::text(line),
        }
    }

    /// Under agent `name`'s row: its confirm strip and its error.
    fn under_row(&self, rows: &mut Vec<Row>, name: &str, width: usize) {
        if let Some(confirm) = self.state.confirm.as_ref().filter(|c| c.agent() == name) {
            for line in shelbi_app::overlay::error_log::wrap(&confirm.message(), width.saturating_sub(4).max(1)) {
                rows.push(Row::text(Line::from(Span::styled(
                    format!("    {line}"),
                    Style::default().fg(palette().foreground),
                ))));
            }
            let cancel = " Cancel ";
            let go = format!(" {} ", confirm.label());
            let style = |on: bool, danger: bool| {
                if on && danger {
                    Style::default().fg(palette().on_color).bg(palette().error_button).add_modifier(Modifier::BOLD)
                } else if on {
                    selected_style(true)
                } else {
                    Style::default().fg(palette().gray).bg(palette().selection)
                }
            };
            let line = Line::from(vec![
                Span::raw("    "),
                Span::styled(cancel, style(self.confirm_choice == 0, false)),
                Span::raw(" "),
                Span::styled(go.clone(), style(self.confirm_choice == 1, true)),
            ]);
            // One row, two buttons: the click splits at the second's start.
            let mut row = Row::hit(line, Hit::Agents(AgentsHit::ConfirmCancel), true);
            row.split = Some(((4 + cancel.chars().count() + 1) as u16, Hit::Agents(AgentsHit::ConfirmGo)));
            rows.push(row);
        }
        if let Some(e) = self.state.error_at(&Target::Agent(name.to_string())) {
            for line in shelbi_app::overlay::error_log::wrap(e, width.saturating_sub(4).max(1)) {
                rows.push(Row::text(Line::from(Span::styled(format!("    {line}"), Style::default().fg(palette().red)))));
            }
        }
    }

    /// The new row: the name being typed, what Enter does, and a refusal.
    fn draft_rows(&self, rows: &mut Vec<Row>, from: Option<&str>, width: usize) {
        let label = "  Name: ";
        let room = width.saturating_sub(label.len() + 1).max(1);
        let text: String = self.draft.lines()[0].iter().collect();
        let (_, col) = self.draft.cursor();
        // Keep the caret in view when the name is longer than the room.
        let skip = (col + 1).saturating_sub(room);
        let shown: Vec<char> = text.chars().skip(skip).take(room).collect();
        let caret = col - skip;
        let mut spans = vec![Span::styled(label, Style::default().fg(palette().gray))];
        let before: String = shown.iter().take(caret).collect();
        let at: String = shown.get(caret).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
        let after: String = shown.iter().skip(caret + 1).collect();
        spans.push(Span::styled(before, Style::default().fg(palette().white)));
        spans.push(Span::styled(at, Style::default().fg(palette().canvas).bg(palette().white)));
        spans.push(Span::styled(after, Style::default().fg(palette().white)));
        let mut row = Row::text(Line::from(spans));
        row.focus = true;
        rows.push(row);
        let hint = match from {
            Some(from) => format!("Duplicate of {from}. Enter to create, Esc to cancel."),
            None => "Enter to create, Esc to cancel.".to_string(),
        };
        wrapped(rows, &format!("  {hint}"), width, Style::default().fg(palette().muted));
        if let Some(e) = self.state.error_at(&Target::NewAgent) {
            wrapped(rows, &format!("  {e}"), width, Style::default().fg(palette().red));
        }
    }

    /// The shared instructions' editor: its wrapped lines in a window that
    /// keeps the caret in view, then Save and Cancel.
    fn editor_rows(&mut self, rows: &mut Vec<Row>, width: usize, height: usize, focused: bool) {
        let active = focused && self.focus == Item::Editor;
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let text_w = width.saturating_sub(EDITOR_PREFIX.chars().count()).max(1);
        let (wrapped_rows, (caret_row, caret_col)) = editor.text.wrapped(text_w);
        // As tall as the text (and a line to type on), up to what leaves
        // the buttons in view.
        let most = height.saturating_sub(6).clamp(3, 20);
        let window = (wrapped_rows.len() + 1).max(5).min(most);
        editor.height = window;
        if caret_row < editor.top {
            editor.top = caret_row;
        } else if caret_row >= editor.top + window {
            editor.top = caret_row + 1 - window;
        }
        editor.top = editor.top.min(wrapped_rows.len().saturating_sub(1));
        let rule_style = Style::default().fg(if active { palette().cyan } else { palette().ansi_dark_gray });
        let text_style = Style::default().fg(palette().foreground);
        let lines = editor.text.lines();
        let caret_on = active && editor.focus == EditorFocus::Text;
        for v in editor.top..editor.top + window {
            let mut spans = vec![Span::styled(EDITOR_PREFIX, rule_style)];
            let mut focus = false;
            if let Some(&(line, start)) = wrapped_rows.get(v) {
                let chars: Vec<char> = lines[line]
                    .iter()
                    .skip(start)
                    .take(text_w)
                    .map(|c| if c.is_control() { ' ' } else { *c })
                    .collect();
                if caret_on && v == caret_row {
                    focus = true;
                    let before: String = chars.iter().take(caret_col).collect();
                    let at: String = chars.get(caret_col).map(|c| c.to_string()).unwrap_or_else(|| " ".into());
                    let after: String = chars.iter().skip(caret_col + 1).collect();
                    spans.push(Span::styled(before, text_style));
                    spans.push(Span::styled(at, Style::default().fg(palette().canvas).bg(palette().white)));
                    spans.push(Span::styled(after, text_style));
                } else {
                    spans.push(Span::styled(chars.into_iter().collect::<String>(), text_style));
                }
            }
            rows.push(Row::hit(Line::from(spans), Hit::Agents(AgentsHit::EditorLine(v)), focus));
        }
        editor.rows = wrapped_rows;
        let more = editor.rows.len() > editor.top + window;
        let button = |label: &str, on: bool| {
            if on {
                Span::styled(format!(" {label} "), selected_style(true))
            } else {
                Span::styled(format!(" {label} "), Style::default().fg(palette().gray).bg(palette().selection))
            }
        };
        let save = "Save ^S";
        let line = Line::from(vec![
            Span::raw("  "),
            button(save, active && editor.focus == EditorFocus::Save),
            Span::raw(" "),
            button("Cancel Esc", active && editor.focus == EditorFocus::Cancel),
            Span::styled(
                if more { "   ↓ more text" } else { "" },
                Style::default().fg(palette().muted),
            ),
        ]);
        let split = 2 + save.chars().count() + 2 + 1;
        let mut row = Row::hit(line, Hit::Agents(AgentsHit::EditorSave), active);
        row.split = Some((split as u16, Hit::Agents(AgentsHit::EditorCancel)));
        rows.push(row);
    }

    /// The actions menu over the page: the open row's actions, each with its
    /// key, a disabled one with why.
    pub(crate) fn menu_lines(&self) -> Option<(String, Vec<MenuLine>)> {
        let (name, at) = self.menu.as_ref()?;
        let keys = |a: RowAction| match a {
            RowAction::Edit => "e",
            RowAction::EditYaml => "y",
            RowAction::Duplicate => "d",
            RowAction::Reset => "r",
            RowAction::Delete => "x",
        };
        let items = self
            .state
            .row_actions(name)
            .into_iter()
            .enumerate()
            .map(|(i, (action, why))| {
                let label = match action {
                    RowAction::Edit => "Edit (instructions.md)".to_string(),
                    other => other.label().to_string(),
                };
                MenuLine { label: format!("{label}  {}", keys(action)), why, selected: i == *at }
            })
            .collect();
        Some((name.clone(), items))
    }
}

/// Push an item's `lines`, each hitting item `index` (the last column its
/// `⋮`), the first focused.
fn push_item(rows: &mut Vec<Row>, lines: Vec<Line<'static>>, index: Option<usize>, focus: bool, width: usize) {
    for (n, line) in lines.into_iter().enumerate() {
        let mut row = match index {
            Some(i) => Row::hit(line, Hit::Agents(AgentsHit::Item(i)), focus && n == 0),
            None => Row::text(line),
        };
        if let (0, Some(i)) = (n, index) {
            row.split = Some((width.saturating_sub(MORE_W) as u16, Hit::Agents(AgentsHit::More(i))));
        }
        rows.push(row);
    }
}

fn rule(width: usize) -> Line<'static> {
    Line::from(Span::styled("─".repeat(width), Style::default().fg(palette().ansi_dark_gray)))
}

/// `text` padded or cut to `width` columns.
fn cell(text: &str, width: usize) -> String {
    let text = panel::truncate_right(text, width);
    let pad = width.saturating_sub(panel::display_width(&text));
    format!("{text}{}", " ".repeat(pad))
}

/// The table's column widths at `width`: (name, statuses).
fn columns(width: usize) -> (usize, usize) {
    let rest = width.saturating_sub(MARK_W + RUNNER_W + SKILLS_W + MORE_W);
    let statuses = (rest * 2 / 5).clamp(16, 32);
    (rest.saturating_sub(statuses), statuses)
}

fn table_header(width: usize) -> Line<'static> {
    let (name_w, statuses_w) = columns(width);
    let style = Style::default().fg(palette().gray).add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::raw(" ".repeat(MARK_W)),
        Span::styled(cell("Agent", name_w), style),
        Span::styled(cell("Preferred runner", RUNNER_W), style),
        Span::styled(cell("Workflow statuses", statuses_w), style),
        Span::styled(cell("Skills", SKILLS_W), style),
    ])
}

/// The status chips laid into lines at most `width` wide: `● Name` each,
/// the dot in its category's color.
fn chip_lines(chips: &[StatusChip], width: usize) -> Vec<Vec<Span<'static>>> {
    if chips.is_empty() {
        return vec![vec![Span::styled(cell(NOT_ASSIGNED, width), Style::default().fg(palette().muted))]];
    }
    let mut lines: Vec<Vec<Span<'static>>> = Vec::new();
    let mut used = 0;
    let mut current: Vec<Span<'static>> = Vec::new();
    for chip in chips {
        let name = panel::truncate_right(&chip.name, width.saturating_sub(2).max(1));
        let w = 2 + panel::display_width(&name);
        let gap = usize::from(!current.is_empty());
        if !current.is_empty() && used + gap + w > width {
            lines.push(std::mem::take(&mut current));
            used = 0;
        }
        if !current.is_empty() {
            current.push(Span::raw(" "));
            used += 1;
        }
        let dot = chip.category.map(crate::kanban::category_color).unwrap_or(palette().muted);
        current.push(Span::styled("● ", Style::default().fg(dot)));
        current.push(Span::styled(name, Style::default().fg(palette().foreground)));
        used += w;
    }
    lines.push(current);
    // Pad each line to the column.
    for line in &mut lines {
        let w: usize = line.iter().map(|s| panel::display_width(&s.content)).sum();
        line.push(Span::raw(" ".repeat(width.saturating_sub(w))));
    }
    lines
}

/// An agent's name, bold, and its Customized badge when there's room.
fn name_spans(a: &AgentSummary, width: usize) -> Vec<Span<'static>> {
    const BADGE: &str = " Customized ";
    let name = panel::truncate_right(&a.name, width);
    let mut used = panel::display_width(&name);
    let mut spans = vec![Span::styled(name, Style::default().fg(palette().white).add_modifier(Modifier::BOLD))];
    if a.provenance == Provenance::Customized && used + 1 + BADGE.len() <= width {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            BADGE,
            Style::default().fg(palette().workflow_badge_fg).bg(palette().workflow_badge_bg),
        ));
        used += 1 + BADGE.len();
    }
    spans.push(Span::raw(" ".repeat(width.saturating_sub(used))));
    spans
}

/// Fill a selected row's spans with the selection background.
fn select(mut line: Line<'static>, selected: bool, width: usize) -> Line<'static> {
    if selected {
        let w: usize = line.spans.iter().map(|s| panel::display_width(&s.content)).sum();
        if w < width {
            line.spans.push(Span::raw(" ".repeat(width - w)));
        }
        line = line.patch_style(Style::default().bg(palette().selection));
    }
    line
}

/// An agent's table row: name and badge, runner, statuses, skills, `⋮`;
/// then its description, Recommended, and the statuses that didn't fit.
fn table_row(a: &AgentSummary, width: usize, selected: bool) -> Vec<Line<'static>> {
    let (name_w, statuses_w) = columns(width);
    let chips = chip_lines(&status_chips(a), statuses_w);
    let runner = a.preferred_runner.map_or(UNSET_RUNNER, runner_label);
    let marker = if selected { "› " } else { "  " };
    let mut first = vec![Span::styled(marker.to_string(), Style::default().fg(palette().cyan))];
    first.extend(name_spans(a, name_w.saturating_sub(1)));
    first.push(Span::raw(" "));
    first.push(Span::styled(cell(runner, RUNNER_W), Style::default().fg(palette().foreground)));
    first.extend(chips[0].clone());
    first.push(Span::styled(cell(&a.skills.to_string(), SKILLS_W), Style::default().fg(palette().gray)));
    first.push(Span::styled("⋮ ", Style::default().fg(palette().gray)));
    let mut out = vec![select(Line::from(first), selected, width)];
    let description = a.description.clone().unwrap_or_default();
    let lines = chips.len().max(2);
    for n in 1..lines {
        let mut spans = vec![Span::raw(" ".repeat(MARK_W))];
        let left = if n == 1 { cell(&description, name_w.saturating_sub(1)) } else { " ".repeat(name_w.saturating_sub(1)) };
        spans.push(Span::styled(left, Style::default().fg(palette().gray)));
        spans.push(Span::raw(" "));
        let rec = if n == 1 && a.runner_recommended { "Recommended" } else { "" };
        spans.push(Span::styled(cell(rec, RUNNER_W), Style::default().fg(palette().muted)));
        match chips.get(n) {
            Some(c) => spans.extend(c.clone()),
            None => spans.push(Span::raw(" ".repeat(statuses_w))),
        }
        out.push(select(Line::from(spans), selected, width));
    }
    out
}

/// An agent at a narrow width: its fields under its name.
fn stacked_row(a: &AgentSummary, width: usize, selected: bool) -> Vec<Line<'static>> {
    let marker = if selected { "› " } else { "  " };
    let inner = width.saturating_sub(MARK_W + MORE_W).max(1);
    let mut first = vec![Span::styled(marker.to_string(), Style::default().fg(palette().cyan))];
    first.extend(name_spans(a, inner));
    first.push(Span::styled("⋮ ", Style::default().fg(palette().gray)));
    let mut out = vec![select(Line::from(first), selected, width)];
    let indent = "    ";
    let text_w = width.saturating_sub(indent.len()).max(1);
    let mut push = |text: &str, style: Style| {
        for line in shelbi_app::overlay::error_log::wrap(text, text_w) {
            out.push(select(Line::from(vec![Span::raw(indent), Span::styled(line, style)]), selected, width));
        }
    };
    if let Some(d) = a.description.as_deref().filter(|d| !d.is_empty()) {
        push(d, Style::default().fg(palette().gray));
    }
    let mut runner = a.preferred_runner.map_or(UNSET_RUNNER, runner_label).to_string();
    if a.runner_recommended {
        runner.push_str(" (recommended)");
    }
    let skills = shelbi_app::settings::summary::count(a.skills, "skill");
    push(&format!("{runner} · {skills}"), Style::default().fg(palette().foreground));
    let chips = status_chips(a);
    let statuses = if chips.is_empty() {
        NOT_ASSIGNED.to_string()
    } else {
        format!("Statuses: {}", chips.iter().map(|c| c.name.as_str()).collect::<Vec<_>>().join(", "))
    };
    push(&statuses, Style::default().fg(palette().muted));
    out
}

/// An agent whose files don't read: its name, the error, and its `⋮` (Edit
/// YAML only).
fn unreadable_row(name: &str, error: &str, width: usize, selected: bool) -> Vec<Line<'static>> {
    let marker = if selected { "› " } else { "  " };
    let inner = width.saturating_sub(MARK_W + MORE_W).max(1);
    let first = Line::from(vec![
        Span::styled(marker.to_string(), Style::default().fg(palette().cyan)),
        Span::styled(cell(name, inner), Style::default().fg(palette().white).add_modifier(Modifier::BOLD)),
        Span::styled("⋮ ", Style::default().fg(palette().gray)),
    ]);
    let mut out = vec![select(first, selected, width)];
    for line in shelbi_app::overlay::error_log::wrap(&unreadable_message(error), width.saturating_sub(4).max(1)) {
        out.push(select(
            Line::from(vec![Span::raw("    "), Span::styled(line, Style::default().fg(palette().red))]),
            selected,
            width,
        ));
    }
    out
}

/// The shared instructions' preview: the first lines, lightly formatted.
fn preview_rows(rows: &mut Vec<Row>, markdown: &str, width: usize) {
    let (blocks, cut) = preview(markdown, PREVIEW_LINES);
    if blocks.is_empty() {
        wrapped(rows, &format!("  {NO_SHARED}"), width, Style::default().fg(palette().muted));
        return;
    }
    let text_w = width.saturating_sub(4).max(1);
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 {
            rows.push(Row::blank());
        }
        let style = if block.heading {
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().foreground)
        };
        for line in &block.lines {
            for part in shelbi_app::overlay::error_log::wrap(line, text_w) {
                rows.push(Row::text(Line::from(vec![
                    Span::styled("▏  ", Style::default().fg(palette().ansi_dark_gray)),
                    Span::styled(part, style),
                ])));
            }
        }
    }
    if cut {
        rows.push(Row::text(Line::from(vec![
            Span::styled("▏  ", Style::default().fg(palette().ansi_dark_gray)),
            Span::styled("…", Style::default().fg(palette().muted)),
        ])));
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Leave, Load, Region, SaveStart, SettingsAction, SettingsShell, Stop};
    use super::*;
    use ratatui::layout::Rect;
    use shelbi_state::keymap::DisplayStyle;
    use shelbi_app::settings::agents::fixture::Home;
    use shelbi_app::settings::agents::commit;

    const PROJECT: &str = "proj";

    fn home(tag: &str) -> Home {
        let lock = crate::test_support::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Home::new(&format!("tui-{tag}"), PROJECT, lock)
    }

    fn keymaps() -> Keymaps {
        shelbi_state::keymap::validate_keymaps_yaml("", None).0
    }

    /// A `config.yaml` writer over a fresh fixture (the Agents page never
    /// writes through it).
    fn writer() -> super::super::ConfigWriter {
        let store = std::sync::Arc::new(std::sync::Mutex::new(super::super::FixtureConfig::default()));
        super::super::fixture_io(&store).1
    }

    /// Settings on the Agents page for `project`, its loads and saves run
    /// in place of the shell's threads, what it hands the shell recorded.
    struct Driver {
        s: SettingsShell,
        km: Keymaps,
        project: Option<String>,
        edited: Vec<PathBuf>,
        ran: Vec<Effect>,
        status: Option<String>,
        left: Option<SettingsAction>,
    }

    impl Driver {
        fn new(project: Option<&str>) -> Driver {
            let mut d = Driver {
                s: SettingsShell::default(),
                km: keymaps(),
                project: project.map(str::to_string),
                edited: Vec::new(),
                ran: Vec::new(),
                status: None,
                left: None,
            };
            d.s.open();
            let action = d.s.request(Leave::Page(SettingsPage::Agents));
            d.apply(action);
            d.s.set_region(Region::Page);
            d
        }

        fn apply(&mut self, action: SettingsAction) {
            match action {
                SettingsAction::Load => self.load(false),
                SettingsAction::Reload => self.load(true),
                SettingsAction::CommitAgents(job) => self.commit(*job),
                SettingsAction::EditFile(path) => self.edited.push(path),
                SettingsAction::Run(effect) => self.ran.push(effect),
                SettingsAction::Status(text) => self.status = Some(text),
                SettingsAction::Save => match self.s.begin_save(self.project.as_deref(), &writer()) {
                    Some(SaveStart::Commit(job)) => self.commit(*job),
                    Some(SaveStart::Work(..)) => panic!("the Agents page saves through its queue"),
                    None => {}
                },
                SettingsAction::AgentWork => self.agent_work(),
                SettingsAction::Stay => {}
                other => self.left = Some(other),
            }
        }

        /// Run the agent editor's waiting work here, as the shell would off
        /// the UI thread.
        fn agent_work(&mut self) {
            while let Some((ticket, work)) = self.s.take_agent_work() {
                let done = super::super::run_agent_work(work, &|p: &str| AgentsConfig::load(p).map_err(|e| e.to_string()));
                let (action, _) = self.s.agent_landed(&ticket, done);
                self.apply(action);
            }
        }

        fn load(&mut self, force: bool) {
            match self.s.begin_page_load(self.project.as_deref(), force) {
                Some(Load::Agents { id, ticket, project }) => {
                    let config = AgentsConfig::load(&project).map_err(|e| e.to_string());
                    assert!(self.s.agents_loaded(id, &ticket, config));
                }
                Some(other) => panic!("not an Agents load: {other:?}"),
                None => {}
            }
        }

        fn commit(&mut self, job: CommitJob) {
            let done = commit(&job.project, &job.config, job.edit);
            let (next, action) = self.s.agents_committed(job.id, done);
            if let Some(next) = next {
                self.commit(next);
            }
            self.apply(action);
        }

        fn key_with(&mut self, code: KeyCode, mods: KeyModifiers) {
            let action = self.s.handle_key(KeyEvent::new(code, mods), &self.km);
            self.apply(action);
        }

        fn key(&mut self, code: KeyCode) {
            self.key_with(code, KeyModifiers::NONE);
        }

        fn keys(&mut self, text: &str) {
            for c in text.chars() {
                self.key(KeyCode::Char(c));
            }
        }

        /// Move the page's focus to `item` (from the top).
        fn focus(&mut self, item: Item) {
            for _ in 0..40 {
                self.key(KeyCode::Up);
            }
            for _ in 0..40 {
                if *self.s.agents.focus() == item {
                    return;
                }
                self.key(KeyCode::Down);
            }
            panic!("never reached {item:?}");
        }

        fn page(&self) -> &AgentsPage {
            &self.s.agents
        }

        fn screen(&mut self, w: u16, h: u16) -> String {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            let km = self.km.clone();
            term.draw(|f| {
                let area = f.area();
                let side = Rect { width: 28.min(area.width / 3), ..area };
                let main = Rect { x: side.right(), width: area.width - side.width, ..area };
                let chrome = super::super::Chrome { project_label: "proj", status: None, keymaps: &km, style: DisplayStyle::Mac, has_project: true };
                self.s.render(f, side, main, &chrome);
            })
            .unwrap();
            let buf = term.backend().buffer();
            let mut out = String::new();
            for y in 0..buf.area.height {
                for x in 0..buf.area.width {
                    out.push_str(buf[(x, y)].symbol());
                }
                out.push('\n');
            }
            out
        }
    }

    #[test]
    fn the_table_lists_agents_with_provenance_runner_assignments_and_skills() {
        let _home = home("rows");
        let mut d = Driver::new(Some(PROJECT));
        let text = d.screen(160, 60);
        for want in [
            "Agents",
            "+ New agent",
            "Preferred runner",
            "Workflow statuses",
            "orchestrator",
            "Customized",
            "Claude Code",
            "Recommended",
            "● Backlog",
            "● To Do",
            "● In Progress",
            "● Review",
            "Not assigned",
            "Shared instructions",
            "You are working on Shelbi",
            "Edit instructions",
            SHARED_FOOTNOTE,
        ] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        // The orchestrator and review rows carry the badge, the developer's doesn't.
        let line = |name: &str| text.lines().find(|l| l.contains(&format!(" {name} "))).unwrap().to_string();
        assert!(line("orchestrator").contains("Customized"));
        assert!(!line("developer").contains("Customized"));
        assert!(line("review").trim_end().ends_with("1      ⋮"), "the review agent's one skill:\n{}", line("review"));
        assert_eq!(text.matches("In Progress").count(), 1, "once, though task and subtask both assign it");
    }

    #[test]
    fn a_narrow_page_stacks_each_agents_fields_and_keeps_its_actions() {
        let _home = home("narrow");
        let mut d = Driver::new(Some(PROJECT));
        let text = d.screen(96, 60);
        assert!(!text.contains("Preferred runner"), "no table header:\n{text}");
        for want in ["orchestrator", "Statuses: Backlog, To Do", "Claude Code (recommended) · 0 skills", "Not assigned"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        // The actions are a key away all the same.
        d.focus(Item::Agent("developer".into()));
        d.key(KeyCode::Char('m'));
        let text = d.screen(64, 30);
        for want in ["Edit (instructions.md)", "Edit YAML", "Duplicate", "Reset to default", "Delete"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        crate::theme::with_appearance(shelbi_app::theme::Appearance::Light, || {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(64, 30)).unwrap();
            let km = d.km.clone();
            term.draw(|f| {
                let area = f.area();
                let side = Rect { width: 20, ..area };
                let main = Rect { x: 20, width: 44, ..area };
                let chrome = super::super::Chrome { project_label: "proj", status: None, keymaps: &km, style: DisplayStyle::Mac, has_project: true };
                d.s.render(f, side, main, &chrome);
            })
            .unwrap();
            crate::theme::assert_light_readable("agents-narrow", term.backend().buffer());
        });
    }

    #[test]
    fn the_page_reads_in_light_at_a_wide_size() {
        let _home = home("light");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::Agent("orchestrator".into()));
        crate::theme::with_appearance(shelbi_app::theme::Appearance::Light, || {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(160, 48)).unwrap();
            let km = d.km.clone();
            term.draw(|f| {
                let area = f.area();
                let side = Rect { width: 28, ..area };
                let main = Rect { x: 28, width: 132, ..area };
                let chrome = super::super::Chrome { project_label: "proj", status: None, keymaps: &km, style: DisplayStyle::Mac, has_project: true };
                d.s.render(f, side, main, &chrome);
            })
            .unwrap();
            crate::theme::assert_light_readable("agents-wide", term.backend().buffer());
        });
    }

    #[test]
    fn enter_opens_the_agent_and_y_its_yaml() {
        let home = home("open");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::Agent("developer".into()));
        d.key(KeyCode::Enter);
        assert_eq!(d.s.agent_editor().map(|e| e.name()), Some("developer"), "the agent editor opens");
        assert!(d.ran.is_empty());
        // Back (nothing unsaved) lands on the agent's row.
        d.key(KeyCode::Esc);
        assert!(d.s.agent_editor().is_none());
        assert_eq!(*d.page().focus(), Item::Agent("developer".into()));
        d.key(KeyCode::Char('y'));
        assert_eq!(d.edited, [home.agents().join("developer").join("agent.yaml")]);
    }

    #[test]
    fn new_agent_types_a_name_and_a_refusal_keeps_the_row() {
        let home = home("new");
        let mut d = Driver::new(Some(PROJECT));
        d.key(KeyCode::Char('n'));
        assert!(d.screen(120, 60).contains("Enter to create, Esc to cancel."));
        // Digits, `j` and Esc-free keys type into the name, not the page.
        d.keys("developer");
        d.key(KeyCode::Enter);
        assert_eq!(d.page().state().error_at(&Target::NewAgent), Some("An agent named `developer` already exists"));
        assert!(d.page().state().draft.is_some(), "the row stays open");
        for _ in 0.."developer".len() {
            d.key(KeyCode::Backspace);
        }
        d.keys("docs2");
        d.key(KeyCode::Enter);
        assert!(home.agents().join("docs2/agent.yaml").exists());
        assert!(d.page().state().draft.is_none());
        assert_eq!(d.s.agent_editor().map(|e| e.name()), Some("docs2"), "the new agent opens in the editor");
        assert!(d.ran.is_empty(), "no external editor opens on its own");
        d.key(KeyCode::Esc);
        assert_eq!(*d.page().focus(), Item::Agent("docs2".into()), "Back lands on the new row");
        // Esc drops a new row.
        d.key(KeyCode::Char('n'));
        d.keys("x");
        d.key(KeyCode::Esc);
        assert!(d.page().state().draft.is_none());
        assert_eq!(d.s.stop(), Stop::Page(SettingsPage::Agents), "Esc closed the row, not Settings");
        assert!(d.left.is_none());
    }

    #[test]
    fn duplicate_prefills_a_unique_name() {
        let home = home("dup");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::Agent("review".into()));
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('d'));
        assert!(d.screen(120, 60).contains("Duplicate of review."));
        d.key(KeyCode::Enter);
        let copy = home.agents().join("review-copy");
        assert!(std::fs::read_to_string(copy.join("instructions.md")).unwrap().contains("release notes"));
        assert_eq!(d.s.agent_editor().map(|e| e.name()), Some("review-copy"));
        d.key(KeyCode::Esc);
        assert_eq!(*d.page().focus(), Item::Agent("review-copy".into()));
    }

    #[test]
    fn reset_and_delete_are_guarded_and_ask_first() {
        let home = home("guards");
        let mut d = Driver::new(Some(PROJECT));
        // A shipped default: Reset and Delete are refused, with why.
        d.focus(Item::Agent("developer".into()));
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('r'));
        assert!(d.page().menu_open() && d.page().state().confirm.is_none(), "nothing ran");
        let text = d.screen(120, 40);
        assert!(text.contains("Already the shipped default."), "Reset says why:\n{text}");
        d.key(KeyCode::Char('x'));
        assert!(d.page().menu_open() && d.page().state().confirm.is_none(), "nothing ran");
        let text = d.screen(120, 40);
        assert!(text.contains("ships with Shelbi"), "Delete says why:\n{text}");
        d.key(KeyCode::Esc);
        assert!(!d.page().menu_open());

        // A customized one resets, once confirmed: Cancel is the default.
        d.focus(Item::Agent("orchestrator".into()));
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('r'));
        let text = d.screen(140, 60);
        assert!(text.contains("your changes to them are lost"), "{text}");
        d.key(KeyCode::Enter);
        assert!(d.page().state().confirm.is_none());
        let path = home.agents().join("orchestrator/instructions.md");
        assert!(std::fs::read_to_string(&path).unwrap().contains("release notes"), "Cancel kept it");
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('r'));
        d.key(KeyCode::Right);
        d.key(KeyCode::Enter);
        assert!(!std::fs::read_to_string(&path).unwrap().contains("release notes"), "reset");
        assert!(!d.screen(160, 60).lines().any(|l| l.contains(" orchestrator ") && l.contains("Customized")));

        // A user agent a workflow names can't be deleted; once free it can.
        d.key(KeyCode::Char('n'));
        d.keys("docs");
        d.key(KeyCode::Enter);
        // It opens in the agent editor; Back to the page.
        assert!(d.s.agent_editor().is_some());
        d.key(KeyCode::Esc);
        let task = home.workflows().join("task.yaml");
        let text = std::fs::read_to_string(&task).unwrap();
        std::fs::write(&task, text.replace("agent: review", "agent: docs")).unwrap();
        d.key(KeyCode::Char('r'));
        let rows = d.page().state().agents();
        let docs = rows.iter().find(|a| a.name == "docs").unwrap();
        assert_eq!(status_chips(docs)[0].name, "Review", "the assignment refreshed on reload");
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('x'));
        assert!(d.page().state().confirm.is_none(), "in use: refused");
        d.key(KeyCode::Esc);
        std::fs::write(&task, text).unwrap();
        d.key(KeyCode::Char('r'));
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('x'));
        assert!(d.screen(140, 60).contains("Delete docs? Its directory"));
        d.key(KeyCode::Tab);
        d.key(KeyCode::Enter);
        assert!(!home.agents().join("docs").exists());
        assert!(matches!(d.page().focus(), Item::Agent(_)), "the focus moved to a row still there");
    }

    #[test]
    fn shared_instructions_edit_in_place_and_save_markdown_as_text() {
        let home = home("shared");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::EditShared);
        d.key(KeyCode::Enter);
        assert_eq!(d.page().editor_text().as_deref(), Some(shelbi_app::settings::agents::fixture::PREAMBLE));
        let text = d.screen(140, 44);
        assert!(text.contains("Save ^S") && text.contains("Cancel Esc"), "the buttons show with the text:\n{text}");
        assert!(text.contains("│ You are working on Shelbi"), "{text}");
        // Select all by hand: to the end, then paste Markdown after it.
        d.key_with(KeyCode::PageDown, KeyModifiers::NONE);
        d.key(KeyCode::End);
        d.s.paste("## Notes\r\n\r\n- **always** run `cargo test`\r\n");
        d.keys("1j q");
        d.key_with(KeyCode::Char('s'), KeyModifiers::CONTROL);
        let path = home.agents().join("_shared/preamble.md");
        let want = format!(
            "{}## Notes\n\n- **always** run `cargo test`\n1j q",
            shelbi_app::settings::agents::fixture::PREAMBLE
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), want, "the Markdown as typed");
        assert!(d.page().editor_text().is_none(), "the editor closes once saved");
        assert_eq!(*d.page().focus(), Item::EditShared);
        assert!(d.screen(140, 60).contains("Notes"), "the preview shows the new text");
    }

    #[test]
    fn cancel_asks_before_dropping_changes() {
        let home = home("cancel");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::EditShared);
        d.key(KeyCode::Enter);
        // Clean: Esc just closes.
        d.key(KeyCode::Esc);
        assert!(d.page().editor_text().is_none());
        d.key(KeyCode::Enter);
        d.keys("Draft ");
        d.key(KeyCode::Esc);
        assert!(d.s.prompting(), "unsaved text: the prompt asks");
        assert!(d.screen(120, 40).contains("Save your changes to the shared"));
        d.key(KeyCode::Esc);
        assert!(!d.s.prompting());
        assert!(d.page().editor_text().unwrap().starts_with("Draft "), "Cancel kept the text");
        // Tab to the Cancel button, then the prompt's Discard.
        d.key(KeyCode::Tab);
        d.key(KeyCode::Tab);
        d.key(KeyCode::Enter);
        d.key(KeyCode::Char('d'));
        assert!(d.page().editor_text().is_none());
        let path = home.agents().join("_shared/preamble.md");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), shelbi_app::settings::agents::fixture::PREAMBLE);
        // Leaving Settings with unsaved text: the prompt's Save saves, then leaves.
        d.key(KeyCode::Enter);
        d.keys("Saved ");
        d.s.set_region(Region::Sidebar);
        d.key(KeyCode::Esc);
        assert!(d.s.prompting());
        d.key(KeyCode::Char('s'));
        assert_eq!(d.left, Some(SettingsAction::Leave), "left once the save landed");
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("Saved You are"));
    }

    #[test]
    fn a_failed_save_keeps_the_text_shows_why_and_reloads() {
        let home = home("conflict");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::EditShared);
        d.key(KeyCode::Enter);
        d.keys("Mine ");
        let path = home.agents().join("_shared/preamble.md");
        std::fs::write(&path, "By hand.\n").unwrap();
        d.key_with(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "By hand.\n", "nothing written");
        assert!(d.page().editor_text().unwrap().starts_with("Mine "), "the text stays");
        assert_eq!(d.page().state().shared_instructions(), "By hand.\n", "reloaded");
        let text = d.screen(140, 60);
        assert!(text.contains("Changed on disk"), "{text}");
        // A second Save is a deliberate overwrite.
        d.key_with(KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(std::fs::read_to_string(&path).unwrap().starts_with("Mine "));
    }

    #[test]
    fn an_unreadable_agent_shows_its_error_and_is_never_rewritten() {
        let home = home("unreadable");
        let mut d = Driver::new(Some(PROJECT));
        // A hand edit through Edit YAML breaks developer's manifest, and adds
        // a field and a file the model doesn't own to review's.
        d.focus(Item::Agent("developer".into()));
        d.key(KeyCode::Char('y'));
        let broken = home.agents().join("developer/agent.yaml");
        assert_eq!(d.edited, std::slice::from_ref(&broken));
        std::fs::write(&broken, "name: [unclosed\n").unwrap();
        let review = home.agents().join("review");
        let manifest = std::fs::read_to_string(review.join("agent.yaml")).unwrap() + "x-team: platform # ours\n";
        std::fs::write(review.join("agent.yaml"), &manifest).unwrap();
        std::fs::write(review.join("NOTES.txt"), "keep me\n").unwrap();
        let hooks = std::fs::read(review.join("settings.json")).ok();
        // The shell reloads when the editor returns.
        d.apply(SettingsAction::Reload);
        let text = d.screen(140, 60);
        assert!(text.contains("Can’t be edited here"), "{text}");
        d.focus(Item::Agent("developer".into()));
        d.key(KeyCode::Char('m'));
        assert_eq!(d.page().menu_lines().unwrap().1.len(), 1, "Edit YAML only");
        d.key(KeyCode::Esc);
        d.key(KeyCode::Enter);
        assert_eq!(d.edited.len(), 2, "Enter on it is Edit YAML too");

        // Page edits elsewhere leave both directories as they were.
        d.key(KeyCode::Char('n'));
        d.keys("docs");
        d.key(KeyCode::Enter);
        d.key(KeyCode::Esc);
        d.focus(Item::Agent("review".into()));
        d.key(KeyCode::Char('m'));
        d.key(KeyCode::Char('d'));
        d.key(KeyCode::Enter);
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "name: [unclosed\n");
        assert_eq!(std::fs::read_to_string(review.join("agent.yaml")).unwrap(), manifest);
        assert_eq!(std::fs::read_to_string(review.join("NOTES.txt")).unwrap(), "keep me\n");
        assert_eq!(std::fs::read(review.join("settings.json")).ok(), hooks, "hook wiring untouched");
        let copy = home.agents().join("review-copy");
        assert!(std::fs::read_to_string(copy.join("agent.yaml")).unwrap().contains("x-team: platform"), "the duplicate carries the field");
        assert!(copy.join("NOTES.txt").exists());
    }

    #[test]
    fn a_dropped_comment_notice_shows() {
        let _home = home("notice");
        let mut d = Driver::new(Some(PROJECT));
        let job = d.page().state().config().cloned().unwrap();
        let id = d.page().id();
        // A save whose agent.yaml lost a comment says so.
        let done = Committed {
            edit: Edit::CreateAgent { name: "docs".into() },
            config: Some(job),
            error: None,
            dropped_comments: vec!["agent.yaml: ## the runner".into()],
        };
        let action = d.s.request(Leave::Page(SettingsPage::Agents));
        d.apply(action);
        // Run it as a save in flight would land.
        let _ = d.s.agents.state.request(Edit::CreateAgent { name: "docs".into() });
        let (_, action) = d.s.agents_committed(id, done);
        d.apply(action);
        let text = d.screen(140, 60);
        assert!(text.contains("The save dropped these comments: agent.yaml: ## the runner"), "{text}");
    }

    #[test]
    fn a_project_switch_drops_the_old_projects_load_and_finishes_its_saves() {
        let home = home("switch");
        home.add_project("beta");
        let mut d = Driver::new(Some(PROJECT));
        // A save in flight, and a second queued behind it.
        d.key(KeyCode::Char('n'));
        d.keys("one");
        let SettingsAction::CommitAgents(first) = d.s.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &d.km) else {
            panic!("a save starts");
        };
        // Close its row and type another while it saves.
        d.key(KeyCode::Esc);
        d.key(KeyCode::Char('n'));
        d.keys("two");
        d.key(KeyCode::Enter);
        // A read for the old project still out too.
        let Some(Load::Agents { id: old_id, ticket: old_ticket, .. }) = d.s.begin_page_load(Some(PROJECT), true) else {
            panic!("a read");
        };
        // Switch.
        d.s.project_changed();
        d.project = Some("beta".into());
        d.load(false);
        assert_eq!(d.page().project(), Some("beta"));
        assert_eq!(d.s.retired(), 1, "the old page's saves finish");
        // The old read lands: dropped.
        assert!(!d.s.agents_loaded(old_id, &old_ticket, AgentsConfig::load(PROJECT).map_err(|e| e.to_string())));
        // The old save lands, and its queued one runs, for the old project.
        d.commit(*first);
        assert_eq!(d.s.retired(), 0);
        assert!(home.agents().join("one").exists());
        assert!(home.agents().join("two").exists());
        let beta = shelbi_state::agents_dir("beta").unwrap();
        assert!(beta.join("_shared").exists() && !beta.join("one").exists());
        assert!(d.page().state().agents().iter().all(|a| a.name != "one"), "never reaches beta's page");
        assert!(d.page().state().error_at(&Target::NewAgent).is_none());
    }

    #[test]
    fn no_project_offers_switch_and_add() {
        let mut d = Driver::new(None);
        let text = d.screen(120, 30);
        assert!(text.contains("No project is open."), "{text}");
        d.key(KeyCode::Enter);
        d.key(KeyCode::Down);
        d.key(KeyCode::Enter);
        assert_eq!(d.ran, [Effect::SwitchProject { project: String::new() }, Effect::AddProject]);
    }

    #[test]
    fn clicks_select_then_open_and_the_more_button_opens_the_menu() {
        let home = home("mouse");
        let mut d = Driver::new(Some(PROJECT));
        let text = d.screen(160, 60);
        let (row, line) = text.lines().enumerate().find(|(_, l)| l.contains(" developer ")).unwrap();
        let col = line.chars().position(|c| c == 'd').unwrap() as u16;
        let main = Rect { x: 28, y: 0, width: 132, height: 60 };
        let side = Rect { x: 0, y: 0, width: 28, height: 60 };
        let click = |x: u16, y: u16| crossterm::event::MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        use crossterm::event::MouseEventKind;
        let a = d.s.handle_mouse(click(col, row as u16), side, main);
        d.apply(a);
        assert_eq!(*d.page().focus(), Item::Agent("developer".into()));
        assert!(d.ran.is_empty(), "a first click selects");
        d.screen(160, 60);
        let a = d.s.handle_mouse(click(col, row as u16), side, main);
        d.apply(a);
        assert_eq!(d.s.agent_editor().map(|e| e.name()), Some("developer"), "a second opens");
        d.key(KeyCode::Esc);
        // The ⋮ at the row's end.
        let text = d.screen(160, 60);
        let line = text.lines().nth(row).unwrap();
        let more = line.chars().position(|c| c == '⋮').unwrap() as u16;
        let a = d.s.handle_mouse(click(more, row as u16), side, main);
        d.apply(a);
        assert!(d.page().menu_open());
        let text = d.screen(160, 60);
        let (y, line) = text.lines().enumerate().find(|(_, l)| l.contains("Edit YAML")).unwrap();
        let x = line.find("Edit YAML").map(|b| line[..b].chars().count()).unwrap() as u16;
        let a = d.s.handle_mouse(click(x, y as u16), side, main);
        d.apply(a);
        assert_eq!(d.edited, [home.agents().join("developer/agent.yaml")]);
    }

    #[test]
    fn renders_without_panicking_at_tiny_sizes() {
        let _home = home("tiny");
        let mut d = Driver::new(Some(PROJECT));
        d.focus(Item::EditShared);
        d.key(KeyCode::Enter);
        for (w, h) in [(1, 1), (10, 3), (24, 5), (40, 8), (64, 14), (80, 24)] {
            d.screen(w, h);
        }
        d.key(KeyCode::Esc);
        d.focus(Item::Agent("qa".into()));
        d.key(KeyCode::Char('m'));
        for (w, h) in [(1, 1), (10, 3), (24, 5), (40, 8)] {
            d.screen(w, h);
        }
    }
}
