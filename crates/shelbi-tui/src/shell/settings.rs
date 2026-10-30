//! Settings in the TUI (`tui-settings-shell`), after the desktop app's
//! (`desktop/src/views/settings_nav.rs`): the settings sidebar stands in the
//! nav sidebar's column and the shown page fills the main area, over the
//! shared `shelbi_app::settings` model.
//!
//! The sidebar, top to bottom: the panels' square back button with the
//! Settings title beside it, the Global section's page rows (each with its
//! number key), then a divider and the project switcher (the palette's
//! Projects column, the TUI's switcher) over the project's page rows. Only
//! the pages the TUI can show are listed ([`OFFERED`]); the follow-up pages
//! join the set as they land:
//!
//! - General (`tui-settings-general`): the appearance (Auto / Dark / Light)
//!   and the Editor and Diff tool pickers (`editor-difftool-tui`,
//!   [`tool_picker`]), over the shared
//!   [`GeneralForm`](shelbi_app::settings::general::GeneralForm), then a
//!   summary of where the settings live with the Open in editor paths. The
//!   controls save at once, as the desktop app's do: a theme when it is
//!   picked (it repaints every surface on the next frame once the write
//!   lands), a tool when it is picked from its list or browsed to, an
//!   "Other command…" when Enter commits what was typed. A save shows as
//!   pending until it lands, and a failed one puts the control back.
//! - Terminal: what the host terminal owns (the font, its ANSI palette,
//!   window effects), its sixteen colors as sessions draw them here, and a
//!   short session drawn through the sessions' own cell mapping. Nothing on
//!   it edits: the TUI never changes the host terminal's palette.
//! - Keybindings (`tui-settings-keybindings`, [`super::keybindings`]): the
//!   bindings editor over `keys.yaml`; a saved change applies to the whole
//!   TUI at once.
//! - Workflows (`tui-settings-workflows`, [`workflows`]): the project's
//!   workflows and status catalog, edited in place and saved as each edit is
//!   made, over the shared `shelbi_app::settings::workflows` state. Editing a
//!   workflow opens its editor (`tui-workflow-editor-form`,
//!   [`workflow_editor`]) over both Settings columns: one draft over the
//!   shared `shelbi_app::settings::workflow_editor` state, written at Save.
//! - Agents (`tui-settings-agents`, [`agents`]): the project's agents (New,
//!   Duplicate, a guarded Reset and Delete, Edit YAML) and an in-place
//!   editor for their shared instructions, over the shared
//!   `shelbi_app::settings::agents` state. Opening an agent opens its
//!   details editor (`tui-agent-editor`, [`agent_editor`]) over both
//!   columns: one draft over the shared
//!   `shelbi_app::settings::agent_editor` state, written at Save.
//! - Workspaces (`tui-settings-workspaces`, [`workspaces`]): the project's
//!   workspace pool and the machines that host it, with the New / Edit /
//!   Remove workspace forms over the shared pool service.
//!
//! Keys, all terminal-safe (no Command chords) and never forwarded to a
//! session while Settings shows:
//!
//! - the sidebar keymap's up / down / activate (`k` / `j` / Enter by
//!   default) and the arrows move through the focused list and open its row;
//! - Tab / Shift-Tab (and the global Ctrl+H / Ctrl+L) move focus between
//!   the sidebar and the page;
//! - a page's number (`1`) opens it from either;
//! - Esc closes the innermost thing first: the editor command being typed,
//!   the unsaved-edits prompt, then Settings itself (back to the view,
//!   selection and focus it was opened over; no session stops).
//!
//! Mouse: a click opens a row, runs a page action or answers the prompt;
//! the wheel moves the sidebar selection or scrolls the page.
//!
//! This module holds the state and the drawing; the shell routes events in
//! and carries out the [`SettingsAction`]s that need it (leaving, switching
//! project, running an effect, starting a load or a save off the UI thread).

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};
use shelbi_app::settings::general::{self, Change, GeneralForm, SaveStatus, ToolSetting};
use shelbi_app::settings::keybindings::Change as KeysChange;
use shelbi_app::settings::summary::{self, FileState, GeneralSummary, ProjectSummary};
use shelbi_app::settings::terminal_preview;
use shelbi_app::settings::{
    Header, Job, JobState, LeaveGuard, PageSet, SettingsNav, SettingsPage, Ticket,
};
use shelbi_app::settings::workflows::{Committed, Edit, Target};
use shelbi_app::{EditTarget, Effect};
use shelbi_state::keymap::{DisplayStyle, Key, Keymaps, SidebarAction};
use shelbi_state::{ThemePreference, UserConfig};
use shelbi_state::workflow_config::WorkflowConfig;
use shelbi_state::agents_config::AgentsConfig;

use super::keybindings::{KeysAction, KeysPage};
use crate::button::{self, Align, Button};
use crate::modal_form;
use crate::panel::{self, PAD};
use crate::table::{self, TableLine};
use crate::textbox::{self, Textbox};
use crate::theme::palette;

mod agent_editor;
mod agents;
mod machine_form;
mod tool_picker;
mod workflow_editor;
mod workflows;
pub(crate) mod workspaces;

pub(crate) use agent_editor::{run_work as run_agent_work, AgentDone, AgentWork};
use agent_editor::{AgentEditorPage, Back as AgentBack, EditorAction as AgentAction, Landed as AgentLanded};
pub(crate) use agents::CommitJob;
use agents::{AgentsHit, AgentsPage, Outcome};
use tool_picker::ToolLayer;
pub(crate) use workflow_editor::{run_work, EditorDone, EditorWork};
use workflow_editor::{EditorAction, Landed, WorkflowEditorPage};
pub(crate) use workflows::EditFile;
use workflows::{Nav, WorkflowsPage};
use shelbi_app::settings::workspace_form::{self, Outcome as PoolOutcome};
use shelbi_app::settings::workspaces::PoolIndex;
use shelbi_core::pool::PoolError;
use workspaces::{Ask, Landed as PoolLanded, WorkspacesPage};

#[cfg(test)]
pub(crate) use workflow_editor::{Item as EditorItem, Text as EditorText};
#[cfg(test)]
pub(crate) use agent_editor::{Item as AgentItem, Text as AgentText};

/// Workflow `name`'s row on the Workflows page (the shell's tests select it).
#[cfg(test)]
pub(crate) fn workflows_item_workflow(name: &str) -> workflows::Item {
    workflows::Item::Workflow(name.to_string())
}

/// Status `id`'s row on the Workflows page.
#[cfg(test)]
pub(crate) fn workflows_item_status(id: &str) -> workflows::Item {
    workflows::Item::Status(id.to_string())
}

/// The pages the TUI offers. A follow-up page joins here once it draws.
pub(crate) const OFFERED: PageSet = PageSet::NONE
    .with(SettingsPage::General)
    .with(SettingsPage::Keybindings)
    .with(SettingsPage::Terminal)
    .with(SettingsPage::Workspaces)
    .with(SettingsPage::Workflows)
    .with(SettingsPage::Agents);

/// A page's background read the shell starts ([`SettingsShell::begin_page_load`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Load {
    /// The General summary.
    General(Ticket),
    /// The Keybindings page: `keys.yaml`.
    Keys(Ticket),
    /// The Workflows page: this project's workflow config.
    Workflows(Ticket, String),
    /// The Agents page's model: page `id`, for `project`.
    Agents { id: u64, ticket: Ticket, project: String },
    /// The Workspaces page: this project's pool.
    Workspaces(Ticket, String),
}

/// Where keyboard focus sits inside Settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Region {
    Sidebar,
    Page,
}

/// A row the sidebar's keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Back,
    Page(SettingsPage),
    Switcher,
}

/// Where leaving the shown page goes (through the unsaved-edits prompt).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Leave {
    /// Out of Settings, back to what it was opened over.
    Back,
    /// Another page.
    Page(SettingsPage),
    /// Another project (the switcher, or the palette's switch).
    Project(String),
    /// Close the Agents page's shared-instructions editor, staying on the
    /// page.
    Editor,
}

/// What the shell does after a Settings key or click.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SettingsAction {
    /// Handled here; redraw.
    Stay,
    /// Leave Settings.
    Leave,
    /// Switch to this project (the prompt let it through).
    SwitchProject(String),
    /// Open the project switcher.
    OpenSwitcher,
    /// Run a registry effect (an Open in editor path, add project).
    Run(Effect),
    /// Load the shown page's data if it isn't loaded for this project.
    Load,
    /// Read the shown page's data again.
    Reload,
    /// The prompt's Save: start the page's save.
    Save,
    /// Write this preference to `config.yaml` (an immediate-save control).
    Write(Change),
    /// Open this file in the user's editor (`config.yaml`, an agent's Edit
    /// YAML); the page reads again when the editor returns.
    EditFile(PathBuf),
    /// The Workflows page queued a save: start it off the UI thread
    /// ([`SettingsShell::begin_commit`]).
    Commit,
    /// Open workflow `name`'s editor, on status `status`'s row when given
    /// (an agent's workflow-status assignment): [`SettingsShell::open_editor`].
    EditWorkflow { name: String, status: Option<String> },
    /// The workflow editor has work to run off the UI thread
    /// ([`SettingsShell::take_editor_work`]).
    EditorWork,
    /// The agent editor has work to run off the UI thread
    /// ([`SettingsShell::take_agent_work`]).
    AgentWork,
    /// Edit YAML on the Workflows page: open a workflow config file in the
    /// user's editor, scaffolding it first when it is missing.
    EditYaml(EditFile),
    /// Write this change to `keys.yaml` off the UI thread (the Keybindings
    /// page), then hand the result to [`SettingsShell::keys_saved`].
    SaveKeys(KeysChange),
    /// Open `keys.yaml` in the user's editor.
    OpenKeys,
    /// The bindings in effect changed ([`SettingsShell::keymaps`]): the
    /// shell, its views, the palette and the hints take them.
    ApplyKeymaps,
    /// Run an Agents page save off the UI thread.
    CommitAgents(Box<CommitJob>),
    /// Say this on the shell's status line (a save for a project no longer
    /// shown failed).
    Status(String),
    /// Run a workspace request off the UI thread
    /// ([`SettingsShell::pool_finished`] takes its result).
    RunPool(Ticket, Box<workspace_form::Job>),
    /// Run a machine save off the UI thread
    /// ([`SettingsShell::machine_finished`] takes its result).
    RunMachine(Ticket, Box<shelbi_app::settings::machine_form::Job>),
    /// Run a machine form's readiness request off the UI thread
    /// ([`SettingsShell::check_finished`] takes its outcome).
    RunCheck(Box<workspaces::CheckJob>),
}

/// A save the shell runs off the UI thread for the unsaved-edits prompt.
pub(crate) enum SaveStart {
    /// A blocking save (General's editor command).
    Work(Ticket, SaveWork),
    /// The Agents page's shared instructions, through its save queue.
    Commit(Box<CommitJob>),
}

/// What a page's focusable row does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Does {
    Run(Effect),
    Reload,
    /// The theme's segmented control (←/→, or Enter for the next one).
    Theme,
    /// A tool's picker (Editor, Diff tool): Enter opens its list.
    Tool(ToolSetting),
    /// Open a file in the editor.
    EditFile(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageAction {
    label: String,
    note: Option<String>,
    does: Does,
}

/// What a click at a recorded rect hits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Hit {
    Stop(Stop),
    Action(usize),
    /// A theme's segment.
    Theme(ThemePreference),
    PromptSave,
    PromptDiscard,
    PromptCancel,
    /// Something on the Agents page.
    Agents(AgentsHit),
}

/// A blocking save the shell runs off the UI thread: the config as written
/// when it wrote `config.yaml`.
pub(crate) type SaveWork = Box<dyn FnOnce() -> Result<Option<UserConfig>, String> + Send>;

/// Writes one preference change to `config.yaml` (blocking; run off the UI
/// thread): [`Change::write`] in production, a fixture in tests.
pub(crate) type ConfigWriter = Arc<dyn Fn(&Change) -> Result<UserConfig, String> + Send + Sync>;

/// The prompt's buttons, left to right.
const PROMPT_BUTTONS: [&str; 3] = ["Save", "Discard", "Cancel"];

/// The leave dialog's key hint.
pub(crate) const LEAVE_HINT: &str = "←→ choose · ⏎ press · s save · d discard · Esc cancel";

/// The leave dialog's buttons as drawn, Cancel, Discard and Save (Primary)
/// left to right, each with its index in [`PROMPT_BUTTONS`]; `choice` (an
/// index there) has the keyboard.
pub(crate) fn leave_buttons(choice: usize, enabled: bool) -> Vec<(Button<'static>, usize)> {
    [2usize, 1, 0]
        .into_iter()
        .map(|i| {
            let b = if i == 0 { Button::primary(PROMPT_BUTTONS[i]) } else { Button::secondary(PROMPT_BUTTONS[i]) };
            (b.focused(enabled && i == choice).enabled(enabled), i)
        })
        .collect()
}

/// The TUI's glyph for a page's row (the sidebar's emoji nav style).
fn page_glyph(page: SettingsPage) -> &'static str {
    match page {
        SettingsPage::General => "🔧",
        SettingsPage::Keybindings => "🔤",
        SettingsPage::Terminal => "💻",
        SettingsPage::Workspaces => "📁",
        SettingsPage::Workflows => "📑",
        SettingsPage::Agents => "🤖",
    }
}

/// What Settings draws that isn't its own state.
pub(crate) struct Chrome<'a> {
    /// The shown project's name, on the switcher.
    pub project_label: &'a str,
    /// The shell's status line, shown above the key hints.
    pub status: Option<&'a str>,
    /// The user's keymaps, for the hints.
    pub keymaps: &'a Keymaps,
    pub style: DisplayStyle,
    /// Whether a project is open (the project pages need one).
    pub has_project: bool,
    /// Where the pointer is (`None`: off the window, or under an overlay),
    /// for the tables' hover.
    pub pointer: Option<(u16, u16)>,
}

/// Settings' state. One per shell: it outlives a visit, so Settings reopens
/// on the page last shown.
pub(crate) struct SettingsShell {
    nav: SettingsNav,
    region: Region,
    stop: Stop,
    /// The General page's summary, loaded per project.
    general: Job<GeneralSummary>,
    /// The General page's appearance and tool controls.
    form: GeneralForm,
    /// The tool pickers' list or Browse prompt, over the General page.
    tools: ToolLayer,
    /// A theme loaded or saved that the shell hasn't applied yet.
    theme_out: Option<ThemePreference>,
    /// The Workflows page.
    workflows: WorkflowsPage,
    /// The Keybindings page. It holds the bindings in effect for the whole
    /// shell (seeded at launch), so it lives whether or not Settings shows.
    keys: KeysPage,
    /// The Agents page, for the shown project.
    agents: AgentsPage,
    /// The id the next project's Agents page gets.
    next_agents_id: u64,
    /// Agents pages a project switch replaced while a save ran: the shell
    /// finishes their queued saves (nothing shows them).
    retired: Vec<(u64, shelbi_app::settings::agents::AgentsState)>,
    /// The Workspaces page, loaded per project.
    workspaces: WorkspacesPage,
    /// The page's text start column at the last draw (editor clicks).
    page_x: u16,
    /// The workflow editor, while it shows (over both columns).
    editor: Option<WorkflowEditorPage>,
    /// The agent editor, while it shows (over both columns; never with the
    /// workflow editor).
    agent_editor: Option<AgentEditorPage>,
    /// The page's selected action row and scroll offset.
    action: usize,
    scroll: u16,
    /// The unsaved-edits prompt, and its focused button.
    guard: LeaveGuard<Leave>,
    prompt_choice: usize,
    /// The save the prompt started.
    save: Job<()>,
    /// Why the last save failed, shown on the page.
    save_error: Option<String>,
    /// Completions dropped as stale (a project or request moved on first).
    dropped: usize,
    /// Rects from the last draw, for clicks.
    hits: Vec<(Rect, Hit)>,
    /// Each drawn table line's rect and its row, from the last draw: what
    /// the pointer lights.
    hover_rects: Vec<(Rect, HoverKey)>,
    /// The table row drawn lit for the pointer at the last draw.
    hover_shown: Option<HoverKey>,
    /// The buttons the last draw left on screen, and the one it lit under
    /// the pointer.
    buttons: Vec<Rect>,
    button_lit: Option<Rect>,
    /// Whether a project was open at the last draw.
    has_project: bool,
    /// Unsaved edits a test injects (General edits nothing yet): `Some` is
    /// dirty, and its value is what the save returns.
    #[cfg(test)]
    pub(crate) test_edits: Option<Result<(), String>>,
}

impl Default for SettingsShell {
    fn default() -> Self {
        let nav = SettingsNav::new(OFFERED);
        SettingsShell {
            stop: Stop::Page(nav.page()),
            nav,
            region: Region::Sidebar,
            general: Job::default(),
            form: GeneralForm::default(),
            tools: ToolLayer::default(),
            theme_out: None,
            workflows: WorkflowsPage::default(),
            keys: KeysPage::new(None),
            agents: AgentsPage::new(0, None),
            next_agents_id: 1,
            retired: Vec::new(),
            workspaces: WorkspacesPage::default(),
            page_x: 0,
            editor: None,
            agent_editor: None,
            action: 0,
            scroll: 0,
            guard: LeaveGuard::default(),
            prompt_choice: 0,
            save: Job::default(),
            save_error: None,
            dropped: 0,
            hits: Vec::new(),
            hover_rects: Vec::new(),
            hover_shown: None,
            buttons: Vec::new(),
            button_lit: None,
            has_project: false,
            #[cfg(test)]
            test_edits: None,
        }
    }
}

impl SettingsShell {
    #[cfg(test)]
    pub(crate) fn page(&self) -> SettingsPage {
        self.nav.page()
    }

    #[cfg(test)]
    pub(crate) fn region(&self) -> Region {
        self.region
    }

    #[cfg(test)]
    pub(crate) fn stop(&self) -> Stop {
        self.stop
    }

    #[cfg(test)]
    pub(crate) fn dropped(&self) -> usize {
        self.dropped
    }

    #[cfg(test)]
    pub(crate) fn general(&self) -> &Job<GeneralSummary> {
        &self.general
    }

    #[cfg(test)]
    pub(crate) fn workflows(&self) -> &WorkflowsPage {
        &self.workflows
    }

    #[cfg(test)]
    pub(crate) fn workflows_mut(&mut self) -> &mut WorkflowsPage {
        &mut self.workflows
    }

    #[cfg(test)]
    pub(crate) fn form(&self) -> &GeneralForm {
        &self.form
    }

    #[cfg(test)]
    pub(crate) fn keys(&self) -> &KeysPage {
        &self.keys
    }

    #[cfg(test)]
    pub(crate) fn agents_page(&self) -> &AgentsPage {
        &self.agents
    }

    #[cfg(test)]
    pub(crate) fn workspaces(&self) -> &WorkspacesPage {
        &self.workspaces
    }

    #[cfg(test)]
    pub(crate) fn retired(&self) -> usize {
        self.retired.len()
    }

    pub(crate) fn keys_mut(&mut self) -> &mut KeysPage {
        &mut self.keys
    }

    /// The bindings in effect.
    pub(crate) fn keymaps(&self) -> &Keymaps {
        self.keys.keymaps()
    }

    /// Whether the Keybindings page shows with the keyboard.
    fn keys_focused(&self) -> bool {
        self.nav.page() == SettingsPage::Keybindings && self.region == Region::Page
    }

    /// Whether `k` should reach Settings ahead of the shell's own chords
    /// (the palette, the focus moves): a capture takes every key, the
    /// search plain typing.
    pub(crate) fn takes_key_first(&self, k: &KeyEvent) -> bool {
        self.guard.prompt().is_none() && self.keys_focused() && self.keys.takes_key_first(k)
    }

    #[cfg(test)]
    pub(crate) fn prompting(&self) -> bool {
        self.guard.prompt().is_some() || self.guard.leaving_after_save()
    }

    /// Settings opened: on the last page, the selection on its row, the keys
    /// on the sidebar.
    pub(crate) fn open(&mut self) {
        self.nav.open(None);
        self.stop = Stop::Page(self.nav.page());
        self.region = Region::Sidebar;
        self.guard.cancel();
        self.save_error = None;
        self.form.cancel_edit();
        self.form.clear_status();
        self.tools.close();
        self.workflows.close_modal();
        self.keys.blur();
        if self.nav.page() == SettingsPage::Workspaces {
            self.workspaces.shown();
        }
    }

    /// Settings closed: anything the Keybindings page held open ends, an
    /// open workspace form's draft goes (a request it sent still lands), and
    /// the Workspaces page stops probing.
    pub(crate) fn close(&mut self) {
        self.keys.blur();
        self.workspaces.hidden();
    }

    pub(crate) fn set_region(&mut self, region: Region) {
        if region != self.region {
            self.keys.blur();
        }
        self.region = region;
    }

    /// Whether the shown page holds unsaved edits.
    fn dirty(&self) -> bool {
        #[cfg(test)]
        if self.test_edits.is_some() {
            return true;
        }
        self.form.dirty()
            || self.agents.dirty()
            || self.editor.as_ref().is_some_and(WorkflowEditorPage::is_dirty)
            || self.agent_editor.as_ref().is_some_and(AgentEditorPage::is_dirty)
    }

    /// Whether an editor's save, duplicate or delete is running.
    fn editor_busy(&self) -> bool {
        self.editor.as_ref().is_some_and(WorkflowEditorPage::busy)
            || self.agent_editor.as_ref().is_some_and(AgentEditorPage::busy)
    }

    // --- the workflow editor -------------------------------------------------

    #[cfg(test)]
    pub(crate) fn editor(&self) -> Option<&WorkflowEditorPage> {
        self.editor.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn editor_mut(&mut self) -> Option<&mut WorkflowEditorPage> {
        self.editor.as_mut()
    }

    /// Whether Settings draws over both columns (the workflow editor, like
    /// the desktop's, hides the settings sidebar): the shell paints no
    /// divider and starts no sidebar drag then.
    pub(crate) fn covers_columns(&self) -> bool {
        self.editor.is_some() || self.agent_editor.is_some()
    }

    /// Open workflow `name` of `project` in the editor, on Settings ▸
    /// Workflows (Back returns there), with status `status`'s row selected
    /// when given. The draft shows at once when the Workflows page has the
    /// project's config; a read starts either way.
    pub(crate) fn open_editor(&mut self, project: Option<&str>, name: &str, status: Option<String>) -> SettingsAction {
        let Some(project) = project else {
            return SettingsAction::Stay;
        };
        if self.nav.show(SettingsPage::Workflows) {
            self.action = 0;
            self.scroll = 0;
        }
        self.stop = Stop::Page(SettingsPage::Workflows);
        self.region = Region::Page;
        self.workflows.close_modal();
        let config = self.workflows.state().filter(|s| s.project() == project).and_then(|s| s.config());
        self.agent_editor = None;
        self.editor = Some(WorkflowEditorPage::open(project, name, config, status));
        SettingsAction::EditorWork
    }

    // --- the agent editor ----------------------------------------------------

    #[cfg(test)]
    pub(crate) fn agent_editor(&self) -> Option<&AgentEditorPage> {
        self.agent_editor.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn agent_editor_mut(&mut self) -> Option<&mut AgentEditorPage> {
        self.agent_editor.as_mut()
    }

    /// Open agent `name` of `project` in the agent editor, on Settings ▸
    /// Agents (Back returns there, to its row). The draft shows at once
    /// when the Agents page has the project's agents; a read starts either
    /// way.
    pub(crate) fn open_agent_editor(&mut self, project: Option<&str>, name: &str) -> SettingsAction {
        let Some(project) = project else {
            return SettingsAction::Stay;
        };
        if self.nav.show(SettingsPage::Agents) {
            self.action = 0;
            self.scroll = 0;
        }
        self.stop = Stop::Page(SettingsPage::Agents);
        self.region = Region::Page;
        self.agents.close_menu();
        let config = (self.agents.project() == Some(project)).then(|| self.agents.state().config()).flatten();
        self.editor = None;
        self.agent_editor = Some(AgentEditorPage::open(project, name, config));
        SettingsAction::AgentWork
    }

    /// The agent editor's work to run now (the shell runs it off the UI
    /// thread and hands it back to [`Self::agent_landed`]).
    pub(crate) fn take_agent_work(&mut self) -> Option<(Ticket, AgentWork)> {
        self.agent_editor.as_mut()?.take_work()
    }

    /// The agent editor's work landed. Returns what to do next, and
    /// whether it wrote the files.
    pub(crate) fn agent_landed(&mut self, ticket: &Ticket, done: AgentDone) -> (SettingsAction, bool) {
        let Some(landed) = self.agent_editor.as_mut().and_then(|e| e.landed(ticket, done)) else {
            self.dropped += 1;
            return (SettingsAction::Stay, false);
        };
        let settings_save = self.guard.leaving_after_save();
        let busy = self.agent_editor.as_ref().is_some_and(AgentEditorPage::busy);
        match landed {
            AgentLanded::Stay => {
                if settings_save && !busy {
                    self.guard.saved(false);
                }
                (SettingsAction::Stay, false)
            }
            AgentLanded::Wrote => {
                // An agent saved, duplicated or renamed: the General
                // summary's count is read again when next shown.
                self.general.reset();
                if settings_save {
                    if let Some(leave) = self.guard.saved(true) {
                        return (self.go(leave), true);
                    }
                }
                // The Agents index under the editor reads again.
                (SettingsAction::Reload, true)
            }
            AgentLanded::Then(leave) => {
                self.general.reset();
                let action = match self.agent_editor.as_mut() {
                    Some(editor) => editor.then(leave),
                    None => AgentAction::Stay,
                };
                let next = self.route_agent(action);
                let next = if next == SettingsAction::Stay { SettingsAction::Reload } else { next };
                (next, true)
            }
            AgentLanded::Close { wrote, notice } => {
                if wrote {
                    self.general.reset();
                }
                self.agent_editor = None;
                if let Some(notice) = notice {
                    self.agents.set_notice(notice);
                }
                (SettingsAction::Reload, wrote)
            }
        }
    }

    /// Carry out what the agent editor asked.
    fn route_agent(&mut self, action: AgentAction) -> SettingsAction {
        match action {
            AgentAction::Stay => SettingsAction::Stay,
            AgentAction::Work => SettingsAction::AgentWork,
            AgentAction::Close(back) => {
                self.agent_editor = None;
                self.region = Region::Page;
                self.stop = Stop::Page(SettingsPage::Agents);
                self.agents.select_after_load(back);
                SettingsAction::Reload
            }
            AgentAction::OpenWorkflow { name, status } => {
                // The agent editor goes; closing the workflow editor lands
                // on Workflows (as on the desktop).
                self.agent_editor = None;
                SettingsAction::EditWorkflow { name, status }
            }
            AgentAction::ShowWorkflows => {
                self.agent_editor = None;
                self.go(Leave::Page(SettingsPage::Workflows))
            }
            AgentAction::EditFile(path) => SettingsAction::EditFile(path),
        }
    }

    /// The editor's work to run now (the shell runs it off the UI thread and
    /// hands it back to [`Self::editor_landed`]).
    pub(crate) fn take_editor_work(&mut self) -> Option<(Ticket, EditorWork)> {
        self.editor.as_mut()?.take_work()
    }

    /// Read the editor's files again (back from the user's editor, Settings
    /// reopened): only for the project it edits; the draft takes them only
    /// if it holds no edits when they land.
    pub(crate) fn reload_editor(&mut self, project: Option<&str>) {
        match self.editor.as_mut() {
            Some(editor) if Some(editor.project()) == project => editor.reload(),
            Some(_) => self.editor = None,
            None => {}
        }
        match self.agent_editor.as_mut() {
            Some(editor) if Some(editor.project()) == project => editor.reload(),
            Some(_) => self.agent_editor = None,
            None => {}
        }
    }

    /// Settings shows (again) for `project`: an editor left open for
    /// another project goes.
    pub(crate) fn project_shown(&mut self, project: Option<&str>) {
        if self.editor.as_ref().is_some_and(|e| Some(e.project()) != project) {
            self.editor = None;
        }
        if self.agent_editor.as_ref().is_some_and(|e| Some(e.project()) != project) {
            self.agent_editor = None;
        }
    }

    /// The editor's work landed. Returns what to do next, and whether it
    /// wrote the files (the shell refreshes the board then).
    pub(crate) fn editor_landed(&mut self, ticket: &Ticket, done: EditorDone) -> (SettingsAction, bool) {
        let Some(landed) = self.editor.as_mut().and_then(|e| e.landed(ticket, done)) else {
            self.dropped += 1;
            return (SettingsAction::Stay, false);
        };
        // A save Settings' own prompt started (a project switch) goes on
        // once it lands, or stays when it failed.
        let settings_save = self.guard.leaving_after_save();
        let busy = self.editor.as_ref().is_some_and(WorkflowEditorPage::busy);
        match landed {
            Landed::Stay => {
                if settings_save && !busy {
                    self.guard.saved(false);
                }
                (SettingsAction::Stay, false)
            }
            Landed::Wrote => {
                if settings_save {
                    if let Some(leave) = self.guard.saved(true) {
                        return (self.go(leave), true);
                    }
                }
                // The Workflows index under the editor reads again.
                (SettingsAction::Reload, true)
            }
            Landed::Then(leave) => {
                let action = match self.editor.as_mut() {
                    Some(editor) => editor.then(leave),
                    None => EditorAction::Stay,
                };
                let next = self.route_editor(action);
                let next = if next == SettingsAction::Stay { SettingsAction::Reload } else { next };
                (next, true)
            }
            Landed::Close { wrote, notice } => {
                self.close_editor(false);
                if let Some(notice) = notice {
                    self.workflows.set_error(Target::Page, notice);
                }
                (SettingsAction::Reload, wrote)
            }
        }
    }

    /// Carry out what the editor asked.
    fn route_editor(&mut self, action: EditorAction) -> SettingsAction {
        match action {
            EditorAction::Stay => SettingsAction::Stay,
            EditorAction::Work => SettingsAction::EditorWork,
            EditorAction::Close => {
                self.close_editor(false);
                SettingsAction::Reload
            }
            EditorAction::ManageStatuses => {
                self.close_editor(true);
                SettingsAction::Reload
            }
            EditorAction::EditYaml(file) => SettingsAction::EditYaml(file),
        }
    }

    /// Close the editor, back to the Workflows page: its workflow's row
    /// selected, or the first status (Manage project statuses).
    fn close_editor(&mut self, statuses: bool) {
        let Some(editor) = self.editor.take() else {
            return;
        };
        self.region = Region::Page;
        self.stop = Stop::Page(SettingsPage::Workflows);
        if statuses {
            self.workflows.select_after_load(workflows::Select::FirstStatus);
        } else {
            self.workflows.select_after_load(workflows::Select::Workflow(editor.name().to_string()));
        }
    }

    /// The shown page's save, if it has anything to save: the shared
    /// instructions' editor saves through the Agents page's queue.
    pub(crate) fn begin_save(&mut self, project: Option<&str>, writer: &ConfigWriter) -> Option<SaveStart> {
        #[cfg(test)]
        if let Some(result) = self.test_edits.clone() {
            let ticket = self.save.begin(project);
            return Some(SaveStart::Work(ticket, Box::new(move || result.map(|()| None))));
        }
        // General: the editor command typed but not committed.
        if self.form.dirty() {
            if let Some(change) = self.form.commit_edit() {
                let ticket = self.save.begin(project);
                let writer = writer.clone();
                return Some(SaveStart::Work(ticket, Box::new(move || writer(&change).map(Some))));
            }
        }
        if self.agents.dirty() {
            // A save queued behind one in flight starts when that lands.
            return self.agents.save_shared().map(|job| SaveStart::Commit(Box::new(job)));
        }
        self.guard.forget_save();
        None
    }

    /// Whether a save the prompt started is still running.
    fn prompt_saving(&self) -> bool {
        self.save.running()
            || (self.guard.leaving_after_save()
                && (self.agents.state().saving() || self.editor_busy()))
    }

    /// A save landed. Returns where to go when the prompt's Save started it
    /// and it worked.
    pub(crate) fn saved(&mut self, ticket: &Ticket, result: Result<Option<UserConfig>, String>) -> SettingsAction {
        let ok = result.is_ok();
        if !self.save.complete(ticket, result.clone().map(|_| ())) {
            self.dropped += 1;
            return SettingsAction::Stay;
        }
        match result {
            Ok(written) => {
                self.save_error = None;
                if let Some(cfg) = written {
                    self.config_written(Ok(cfg));
                }
                #[cfg(test)]
                {
                    self.test_edits = None;
                }
            }
            // The form says why its own save failed.
            Err(e) if self.form.saving() => self.config_written(Err(e)),
            Err(e) => self.save_error = Some(e),
        }
        match self.guard.saved(ok) {
            Some(leave) => self.go(leave),
            None => SettingsAction::Stay,
        }
    }

    /// Drop the shown page's edits (the prompt's Discard).
    fn discard_edits(&mut self) {
        #[cfg(test)]
        {
            self.test_edits = None;
        }
        self.form.cancel_edit();
        self.agents.close_editor();
        if let Some(editor) = self.editor.as_mut() {
            editor.discard_for_settings();
        }
        if let Some(editor) = self.agent_editor.as_mut() {
            editor.discard_for_settings();
        }
        self.save_error = None;
    }

    /// A preference write landed: the form takes it, and a theme it saved is
    /// handed to the shell to apply.
    pub(crate) fn config_written(&mut self, result: Result<UserConfig, String>) {
        if let Some(theme) = self.form.saved_result(result) {
            self.theme_out = Some(theme);
        }
    }

    /// The installed editors and diff tools were detected (off the UI
    /// thread): the pickers list them.
    pub(crate) fn tools_detected(&mut self, detected: shelbi_state::tools::DetectedTools) {
        self.form.set_detected(detected);
        self.tools.detected(&self.form);
    }

    /// The tool pickers' layer (the shell's tests read it).
    #[cfg(test)]
    pub(crate) fn tool_layer(&self) -> &ToolLayer {
        &self.tools
    }

    /// The theme `config.yaml` now holds, once, when a load or a save brought
    /// it in: the shell applies it to every surface.
    pub(crate) fn take_theme(&mut self) -> Option<ThemePreference> {
        self.theme_out.take()
    }

    /// A paste (or a dropped file's path) while Settings shows: into the
    /// Keybindings search, the editor command, the workflow name or the
    /// workspace form field being typed, else nowhere. Returns whether a field took it.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        if let Some(editor) = self.editor.as_mut() {
            return editor.paste(text);
        }
        if let Some(editor) = self.agent_editor.as_mut() {
            return editor.paste(text);
        }
        if self.nav.page() == SettingsPage::Keybindings {
            return self.keys_focused() && self.keys.paste(text);
        }
        if self.nav.page() == SettingsPage::Workflows {
            return self.workflows.paste(text);
        }
        if self.nav.page() == SettingsPage::Agents {
            return self.region == Region::Page && self.guard.prompt().is_none() && self.agents.paste(text);
        }
        if self.nav.page() == SettingsPage::Workspaces {
            return self.workspaces.paste(text);
        }
        if self.tools.paste(text) {
            return true;
        }
        match self.form.draft_mut() {
            Some(field) if self.region == Region::Page => {
                field.paste(text);
                true
            }
            _ => false,
        }
    }

    /// Ask to leave for `leave`: through the prompt when the page is dirty.
    pub(crate) fn request(&mut self, leave: Leave) -> SettingsAction {
        if self.editor_busy() {
            // The editor's save, duplicate or delete is running: wait for it.
            return SettingsAction::Stay;
        }
        if leave == Leave::Page(self.nav.page()) && self.editor.is_none() && self.agent_editor.is_none() {
            // Already shown: just put the selection back on its row.
            self.stop = Stop::Page(self.nav.page());
            return SettingsAction::Stay;
        }
        let dirty = self.dirty();
        match self.guard.leave(leave, dirty) {
            Some(leave) => self.go(leave),
            None => {
                self.prompt_choice = 0;
                SettingsAction::Stay
            }
        }
    }

    /// Carry out a leave the prompt let through.
    fn go(&mut self, leave: Leave) -> SettingsAction {
        if leave != Leave::Editor {
            // Leaving the page: an editor left open (with nothing unsaved,
            // or the prompt let it go) closes.
            self.agents.close_editor();
        }
        match leave {
            Leave::Back => SettingsAction::Leave,
            Leave::Page(page) => {
                self.editor = None;
                self.agent_editor = None;
                self.keys.blur();
                let from = self.nav.page();
                if self.nav.show(page) {
                    self.stop = Stop::Page(page);
                    self.action = 0;
                    self.scroll = 0;
                    if from == SettingsPage::Workspaces {
                        self.workspaces.hidden();
                    }
                    if page == SettingsPage::Workspaces {
                        self.workspaces.shown();
                    }
                }
                SettingsAction::Load
            }
            Leave::Project(project) => SettingsAction::SwitchProject(project),
            Leave::Editor => {
                self.agents.close_editor();
                SettingsAction::Stay
            }
        }
    }

    /// The shown project changed: the page stays, its project data goes, and
    /// anything still loading for the old project is dropped when it lands.
    pub(crate) fn project_changed(&mut self) {
        // The editor was the old project's: its work is dropped when it lands.
        self.editor = None;
        self.agent_editor = None;
        self.general.reset();
        self.workflows.project_changed();
        self.workspaces.project_changed();
        self.replace_agents(None);
        self.save.reset();
        self.action = 0;
        self.scroll = 0;
        self.save_error = None;
    }

    /// Start loading the shown page for `project`: a ticket the shell's
    /// reader carries, or `None` when it is already loaded (or loading) for
    /// it and `force` is off.
    pub(crate) fn begin_load(&mut self, project: Option<&str>, force: bool) -> Option<Ticket> {
        match self.nav.page() {
            SettingsPage::General => {
                let current = self.general.project() == project
                    && !matches!(self.general.state(), JobState::Idle);
                if current && !force {
                    return None;
                }
                Some(self.general.begin(project))
            }
            // The other pages read through `begin_page_load`; Terminal reads
            // nothing.
            SettingsPage::Keybindings
            | SettingsPage::Terminal
            | SettingsPage::Workspaces
            | SettingsPage::Workflows
            | SettingsPage::Agents => None,
        }
    }

    /// Start the shown page's read for `project` (see [`Self::begin_load`]
    /// for General's; Keybindings and Workflows read again on every visit,
    /// to show edits made in an editor or the desktop app).
    pub(crate) fn begin_page_load(&mut self, project: Option<&str>, force: bool) -> Option<Load> {
        match self.nav.page() {
            SettingsPage::General => self.begin_load(project, force).map(Load::General),
            SettingsPage::Keybindings => self.keys.begin_load().map(Load::Keys),
            SettingsPage::Workflows => {
                let ticket = self.workflows.begin_load(project, force)?;
                Some(Load::Workflows(ticket, project?.to_string()))
            }
            // The Agents page reads its files each time it's shown (as the
            // desktop's does), to pick up hand edits.
            SettingsPage::Agents => {
                if self.agents.project() != project {
                    self.replace_agents(project);
                }
                let ticket = self.agents.begin_load(force)?;
                Some(Load::Agents {
                    id: self.agents.id(),
                    ticket,
                    project: project?.to_string(),
                })
            }
            SettingsPage::Workspaces => {
                let ticket = self.workspaces.begin_load(project, force)?;
                Some(Load::Workspaces(ticket, project?.to_string()))
            }
            SettingsPage::Terminal => None,
        }
    }

    /// A Workspaces read landed. Returns whether the pool changed from an
    /// earlier read (the shell refreshes its sidebar too).
    pub(crate) fn pool_loaded(&mut self, ticket: &Ticket, read: Result<PoolIndex, String>) -> bool {
        let before = self.workspaces.dropped();
        let changed = self.workspaces.loaded(ticket, read);
        self.dropped += self.workspaces.dropped() - before;
        changed
    }

    /// A workspace request finished (see [`WorkspacesPage::finished`]).
    pub(crate) fn pool_finished(&mut self, ticket: &Ticket, result: Result<PoolOutcome, PoolError>) -> PoolLanded {
        let before = self.workspaces.dropped();
        let landed = self.workspaces.finished(ticket, result);
        self.dropped += self.workspaces.dropped() - before;
        landed
    }

    /// A machine save finished (see [`WorkspacesPage::machine_finished`]).
    pub(crate) fn machine_finished(
        &mut self,
        ticket: &Ticket,
        job: &shelbi_app::settings::machine_form::Job,
        result: Result<shelbi_core::pool::MachineChange, PoolError>,
    ) -> PoolLanded {
        let before = self.workspaces.dropped();
        let landed = self.workspaces.machine_finished(ticket, job, result);
        self.dropped += self.workspaces.dropped() - before;
        landed
    }

    /// A machine form's readiness request finished (see
    /// [`WorkspacesPage::check_finished`]): what to say on the status line,
    /// if the page can't say it.
    pub(crate) fn check_finished(
        &mut self,
        form: u64,
        request: &shelbi_app::settings::machine_check::ReadinessRequest,
        outcome: shelbi_app::settings::machine_check::CheckOutcome,
    ) -> SettingsAction {
        match self.workspaces.check_finished(form, request, outcome) {
            Some(status) => SettingsAction::Status(status),
            None => SettingsAction::Stay,
        }
    }

    /// A change was committed to the pool: the Workspaces page reads again
    /// when next shown.
    pub(crate) fn pool_stale(&mut self) {
        self.workspaces.mark_stale();
    }

    /// The run loop's tick: fold the prober's latest results in, and say
    /// whether the shown Workspaces page is due to read the config again.
    pub(crate) fn tick(&mut self, now: std::time::Instant) -> (bool, bool) {
        if self.nav.page() != SettingsPage::Workspaces {
            return (false, false);
        }
        // A machine form's running check animates its spinner and clock.
        let animate = self.workspaces.checking();
        (self.workspaces.take_probes() || animate, self.workspaces.reload_due(now))
    }

    /// What the Workspaces page's [`Ask`] means to the shell.
    fn ask(&self, ask: Ask) -> SettingsAction {
        match ask {
            Ask::Stay => SettingsAction::Stay,
            Ask::Reload => SettingsAction::Reload,
            Ask::Run(ticket, job) => SettingsAction::RunPool(ticket, job),
            Ask::RunMachine(ticket, job) => SettingsAction::RunMachine(ticket, job),
            Ask::RunCheck(job) => SettingsAction::RunCheck(job),
        }
    }

    /// A new Agents page for `project`. The old one's saves still in flight
    /// finish (its queue too), but nothing they hand back reaches the new
    /// page.
    fn replace_agents(&mut self, project: Option<&str>) {
        let id = self.next_agents_id;
        self.next_agents_id += 1;
        let old = std::mem::replace(&mut self.agents, AgentsPage::new(id, project));
        if old.state().saving() {
            let old_id = old.id();
            self.retired.push((old_id, old.into_state()));
        }
    }

    /// An Agents read landed for page `id`. Returns whether it was taken.
    pub(crate) fn agents_loaded(&mut self, id: u64, ticket: &Ticket, result: Result<AgentsConfig, String>) -> bool {
        let taken = id == self.agents.id() && self.agents.loaded(ticket, result);
        if !taken {
            self.dropped += 1;
        }
        taken
    }

    /// An Agents save landed for page `id`: the next queued save to run
    /// (that page's), and what the shell does next (leave, once the prompt's
    /// save landed; say why a retired page's save failed).
    pub(crate) fn agents_committed(&mut self, id: u64, done: shelbi_app::settings::agents::Committed) -> (Option<CommitJob>, SettingsAction) {
        if id != self.agents.id() {
            let Some(at) = self.retired.iter().position(|(r, _)| *r == id) else {
                self.dropped += 1;
                return (None, SettingsAction::Stay);
            };
            let error = done.error.clone();
            let state = &mut self.retired[at].1;
            let project = state.project().to_string();
            let next = state.committed(done).map(|(config, edit)| CommitJob { id, project: project.clone(), config, edit });
            if next.is_none() {
                self.retired.remove(at);
            }
            let action = match error {
                Some(e) => SettingsAction::Status(format!("Couldn't save {project}'s agents: {e}")),
                None => SettingsAction::Stay,
            };
            return (next, action);
        }
        let shared = matches!(done.edit, shelbi_app::settings::agents::Edit::SetSharedInstructions { .. });
        // An agent added, removed or reset: the General summary's count is
        // read again when next shown.
        if done.error.is_none() && !shared {
            self.general.reset();
        }
        let prompt_save = shared && self.guard.leaving_after_save();
        let ok = done.error.is_none();
        let next = self.agents.committed(done);
        let action = match prompt_save.then(|| self.guard.saved(ok)).flatten() {
            Some(leave) => self.go(leave),
            None => SettingsAction::Stay,
        };
        // A new or duplicated agent opens in the agent editor, as on the
        // desktop, unless the page moved on meanwhile.
        if let Some(name) = self.agents.take_opened() {
            let listed = self.agents.state().config().is_some_and(|c| c.agent(&name).is_some());
            if listed && action == SettingsAction::Stay && self.nav.page() == SettingsPage::Agents && self.agent_editor.is_none() {
                let project = self.agents.project().map(str::to_string);
                return (next, self.open_agent_editor(project.as_deref(), &name));
            }
        }
        (next, action)
    }

    /// The Workflows page's read landed. Returns whether it was taken.
    pub(crate) fn workflows_loaded(&mut self, ticket: &Ticket, result: Result<WorkflowConfig, String>) -> bool {
        let taken = self.workflows.loaded(ticket, result);
        if !taken {
            self.dropped += 1;
        }
        taken
    }

    /// The Workflows page's next save, if one waits: its ticket, project,
    /// model and edit.
    pub(crate) fn begin_commit(&mut self) -> Option<(Ticket, String, WorkflowConfig, Edit)> {
        self.workflows.begin_commit()
    }

    /// A Workflows save landed. Returns whether it saved for the shown
    /// project (the shell refreshes the board then).
    pub(crate) fn committed(&mut self, ticket: &Ticket, done: Committed) -> bool {
        self.workflows.committed(ticket, done)
    }

    /// Show `message` at `target` on the Workflows page (a file that
    /// couldn't be opened).
    pub(crate) fn workflows_error(&mut self, target: Target, message: String) {
        match self.editor.as_mut() {
            Some(editor) => editor.open_failed(message),
            None => self.workflows.set_error(target, message),
        }
    }

    /// A load landed. Returns whether it was taken (a stale one is dropped).
    pub(crate) fn loaded(&mut self, ticket: &Ticket, result: Result<GeneralSummary, String>) -> bool {
        let taken = self.general.complete(ticket, result);
        if taken {
            if let Some(summary) = self.general.done() {
                // What config.yaml holds now (it may have been edited outside
                // Shelbi): the form shows it, and the TUI follows its theme.
                let global = &summary.global;
                let waiting = self.form.saving();
                self.form.load(global.preferences.clone(), global.env_editor.clone());
                if let (Ok(prefs), false) = (&global.preferences, waiting) {
                    self.theme_out = Some(prefs.theme);
                }
            }
            let n = self.actions().len();
            self.action = self.action.min(n.saturating_sub(1));
        } else {
            self.dropped += 1;
        }
        taken
    }

    /// A read of `keys.yaml` landed.
    pub(crate) fn keys_loaded(&mut self, ticket: &Ticket, result: Result<String, String>) -> SettingsAction {
        if self.keys.loaded(ticket, result) {
            SettingsAction::ApplyKeymaps
        } else {
            SettingsAction::Stay
        }
    }

    /// A save of `keys.yaml` landed: what to do, and whether to read the
    /// file again after (a read was asked for while it was out).
    pub(crate) fn keys_saved(&mut self, result: Result<String, String>) -> (SettingsAction, bool) {
        let (changed, reload) = self.keys.saved(result);
        let action = if changed { SettingsAction::ApplyKeymaps } else { SettingsAction::Stay };
        (action, reload)
    }

    // --- the sidebar's stops ----------------------------------------------

    /// The stops, top to bottom.
    fn stops(&self) -> Vec<Stop> {
        let mut out = vec![Stop::Back];
        for section in self.nav.available().sections() {
            if section.header == Header::Project {
                out.push(Stop::Switcher);
            }
            out.extend(section.pages.iter().map(|p| Stop::Page(*p)));
        }
        out
    }

    fn step_stop(&mut self, down: bool) {
        let stops = self.stops();
        let at = stops.iter().position(|s| *s == self.stop).unwrap_or(0);
        let next = if down {
            (at + 1).min(stops.len() - 1)
        } else {
            at.saturating_sub(1)
        };
        self.stop = stops[next];
    }

    fn activate_stop(&mut self, stop: Stop) -> SettingsAction {
        self.stop = stop;
        match stop {
            Stop::Back => self.request(Leave::Back),
            Stop::Page(page) => self.request(Leave::Page(page)),
            Stop::Switcher => SettingsAction::OpenSwitcher,
        }
    }

    // --- the page's actions -----------------------------------------------

    /// The shown page's focusable rows, top to bottom.
    fn actions(&self) -> Vec<PageAction> {
        match self.nav.page() {
            SettingsPage::General => self.general_actions(),
            // Terminal only explains and shows: nothing on it acts.
            SettingsPage::Terminal
            | SettingsPage::Keybindings
            | SettingsPage::Workspaces
            | SettingsPage::Workflows
            | SettingsPage::Agents => Vec::new(),
        }
    }

    /// General's rows: the theme and the editor (once `config.yaml` reads),
    /// Edit config.yaml, the summary's actions, then About Shelbi (always:
    /// it reads nothing).
    fn general_actions(&self) -> Vec<PageAction> {
        let mut out = Vec::new();
        if let JobState::Done(summary) = self.general.state() {
            if self.form.saved().is_some() {
                out.push(PageAction { label: "Theme".into(), note: None, does: Does::Theme });
                for setting in [ToolSetting::Editor, ToolSetting::DiffTool] {
                    out.push(PageAction { label: setting.label().into(), note: None, does: Does::Tool(setting) });
                }
            }
            out.push(PageAction {
                label: "Edit config.yaml".into(),
                note: Some("in your editor".into()),
                does: Does::EditFile(summary.global.config.path().to_path_buf()),
            });
        }
        out.extend(self.summary_actions());
        out.push(PageAction {
            label: "About Shelbi".into(),
            note: Some(shelbi_app::about::version_line(crate::overlay::about::VERSION)),
            does: Does::Run(Effect::OpenAbout),
        });
        out
    }

    /// The General summary's actions.
    fn summary_actions(&self) -> Vec<PageAction> {
        let switch = PageAction {
            label: "Switch project…".into(),
            note: None,
            does: Does::Run(Effect::SwitchProject {
                project: String::new(),
            }),
        };
        let retry = PageAction {
            label: "Try again".into(),
            note: None,
            does: Does::Reload,
        };
        match self.general.state() {
            JobState::Idle | JobState::Running => Vec::new(),
            JobState::Failed(_) => vec![retry],
            JobState::Done(s) => match &s.project {
                None => vec![
                    switch,
                    PageAction {
                        label: "Add project…".into(),
                        note: None,
                        does: Does::Run(Effect::AddProject),
                    },
                ],
                Some(Err(_)) => vec![
                    retry,
                    PageAction {
                        label: "Edit Project Settings".into(),
                        note: Some("in your editor".into()),
                        does: Does::Run(Effect::OpenEditor {
                            target: EditTarget::Project,
                        }),
                    },
                    switch,
                ],
                Some(Ok(p)) => p
                    .edit_targets
                    .iter()
                    .map(|e| PageAction {
                        label: e.title.clone(),
                        note: Some(edit_note(&e.target)),
                        does: Does::Run(Effect::OpenEditor {
                            target: e.target.clone(),
                        }),
                    })
                    .collect(),
            },
        }
    }

    fn run_action(&mut self, index: usize) -> SettingsAction {
        let Some(action) = self.actions().into_iter().nth(index) else {
            return SettingsAction::Stay;
        };
        self.action = index;
        match action.does {
            Does::Run(effect) => SettingsAction::Run(effect),
            Does::Reload => SettingsAction::Reload,
            Does::Theme => write(self.form.step_theme(1)),
            Does::Tool(setting) => {
                self.form.cancel_edit();
                self.tools.open(&self.form, setting);
                SettingsAction::Stay
            }
            Does::EditFile(path) => SettingsAction::EditFile(path),
        }
    }

    /// Whether the editor command is being typed (it takes the page's keys).
    fn typing(&self) -> bool {
        self.region == Region::Page && self.nav.page() == SettingsPage::General && self.form.draft().is_some()
    }

    /// A key while the editor command is typed: Enter saves it, Esc puts it
    /// back, the rest edits the field. Nothing else on the page answers.
    fn field_key(&mut self, k: KeyEvent) -> SettingsAction {
        let Some(field) = self.form.draft_mut() else {
            return SettingsAction::Stay;
        };
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.form.cancel_edit(),
            KeyCode::Enter => return write(self.form.commit_edit()),
            KeyCode::Backspace => field.backspace(),
            KeyCode::Delete => field.delete(),
            KeyCode::Left => field.left(),
            KeyCode::Right => field.right(),
            KeyCode::Home => field.home(),
            KeyCode::End => field.end(),
            KeyCode::Char('a') if ctrl => field.home(),
            KeyCode::Char('e') if ctrl => field.end(),
            KeyCode::Char('u') if ctrl => field.clear(),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => field.insert(c),
            _ => {}
        }
        SettingsAction::Stay
    }

    // --- input ---------------------------------------------------------------

    /// A key while Settings shows (after the shell took the palette and focus
    /// chords). Every key is Settings': none reaches a session.
    pub(crate) fn handle_key(&mut self, k: KeyEvent, keymaps: &Keymaps) -> SettingsAction {
        if self.nav.page() == SettingsPage::Workspaces && self.workspaces.modal() {
            // The workspace form (or the removal's confirmation) is modal.
            self.region = Region::Page;
            let ask = self.workspaces.layer_key(k);
            return self.ask(ask);
        }
        if self.guard.prompt().is_some() {
            return self.prompt_key(k);
        }
        if self.prompt_saving() {
            // The prompt's save is running: wait for it.
            return SettingsAction::Stay;
        }
        if self.agent_editor.is_some() {
            // The agent editor holds every key (Esc goes Back through its
            // guard).
            let nav = nav_for(k, keymaps);
            let action = self.agent_editor.as_mut().map_or(AgentAction::Stay, |e| e.handle_key(k, nav));
            return self.route_agent(action);
        }
        let on_agents = self.region == Region::Page && self.nav.page() == SettingsPage::Agents;
        if on_agents && self.agents.captures() {
            // A menu, a confirm, the new row or the editor: every key is its.
            let outcome = self.agents.handle_key(k, keymaps);
            return self.outcome(outcome);
        }
        // A capture or the search holds every key (Esc, Tab, digits too).
        if self.keys_focused() && self.keys.holds_keys() {
            return self.keys_key(k, keymaps);
        }
        if self.nav.page() == SettingsPage::General && self.tools.is_open() {
            // A tool's list or the Browse prompt holds every key.
            self.region = Region::Page;
            let change = self.tools.key(&mut self.form, k, nav_for(k, keymaps));
            return write(change);
        }
        if self.typing() {
            return self.field_key(k);
        }
        let chord = crate::keymap::chord_from_event(k);
        let sidebar = chord.and_then(|c| keymaps.sidebar.dispatch(c));
        let up = k.code == KeyCode::Up || sidebar == Some(SidebarAction::NavUp);
        let down = k.code == KeyCode::Down || sidebar == Some(SidebarAction::NavDown);
        let activate = sidebar == Some(SidebarAction::Activate)
            || matches!(chord.map(|c| c.code), Some(Key::Enter));
        let nav = Nav { up, down, activate };
        if let Some(editor) = self.editor.as_mut() {
            // The editor holds every key (Esc goes Back through its guard).
            let action = editor.handle_key(k, nav);
            return self.route_editor(action);
        }
        let on_workflows = self.nav.page() == SettingsPage::Workflows;
        if on_workflows && self.workflows.modal_open() {
            // A menu, picker, confirm or name field holds every key (Esc
            // closes it, not Settings; digits and Tab type or pick).
            self.region = Region::Page;
            return self.workflows.handle_key(k, nav);
        }
        match k.code {
            KeyCode::Esc => return self.request(Leave::Back),
            KeyCode::Tab | KeyCode::BackTab => {
                self.region = match self.region {
                    Region::Sidebar => Region::Page,
                    Region::Page => Region::Sidebar,
                };
                return SettingsAction::Stay;
            }
            KeyCode::Char(c) if k.modifiers.difference(KeyModifiers::SHIFT).is_empty() => {
                if let Some(page) = c.to_digit(10).and_then(|n| OFFERED.for_shortcut(n as u8)) {
                    return self.request(Leave::Page(page));
                }
            }
            _ => {}
        }
        match self.region {
            Region::Sidebar => {
                if up || down {
                    self.step_stop(down);
                } else if activate {
                    return self.activate_stop(self.stop);
                }
                SettingsAction::Stay
            }
            Region::Page if on_workflows => self.workflows.handle_key(k, nav),
            Region::Page if self.nav.page() == SettingsPage::Keybindings => self.keys_key(k, keymaps),
            Region::Page if self.nav.page() == SettingsPage::Workspaces => {
                if up || down {
                    self.workspaces.step(down);
                } else if activate {
                    let ask = self.workspaces.activate(self.workspaces.cursor());
                    return self.ask(ask);
                } else if let Some(ask) = self.workspaces.page_key(k) {
                    return self.ask(ask);
                } else {
                    match k.code {
                        KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                        KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
                        KeyCode::Home => self.scroll = 0,
                        KeyCode::End => self.scroll = u16::MAX,
                        KeyCode::Char('r') if k.modifiers.is_empty() => return SettingsAction::Reload,
                        _ => {}
                    }
                }
                SettingsAction::Stay
            }
            Region::Page if on_agents => {
                match self.agents.handle_key(k, keymaps) {
                    Outcome::Pass => {}
                    outcome => return self.outcome(outcome),
                }
                match k.code {
                    KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                    KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
                    KeyCode::Home => self.scroll = 0,
                    KeyCode::End => self.scroll = u16::MAX,
                    KeyCode::Char('r') if k.modifiers.is_empty() => return SettingsAction::Reload,
                    _ => {}
                }
                SettingsAction::Stay
            }
            Region::Page => {
                let actions = self.actions();
                let n = actions.len();
                let on_theme = actions.get(self.action).is_some_and(|a| a.does == Does::Theme);
                if n == 0 && (up || down) {
                    // Nothing to choose: the arrows scroll.
                    self.scroll = if down { self.scroll.saturating_add(1) } else { self.scroll.saturating_sub(1) };
                } else if up {
                    self.action = self.action.saturating_sub(1);
                } else if down {
                    self.action = (self.action + 1).min(n.saturating_sub(1));
                } else if on_theme && matches!(k.code, KeyCode::Left | KeyCode::Right) {
                    return write(self.form.step_theme(if k.code == KeyCode::Left { -1 } else { 1 }));
                } else if activate {
                    return self.run_action(self.action);
                } else {
                    match k.code {
                        KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(8),
                        KeyCode::PageDown => self.scroll = self.scroll.saturating_add(8),
                        KeyCode::Home => self.scroll = 0,
                        KeyCode::End => self.scroll = u16::MAX,
                        KeyCode::Char('r') if k.modifiers.is_empty() => return SettingsAction::Reload,
                        _ => {}
                    }
                }
                SettingsAction::Stay
            }
        }
    }

    fn keys_key(&mut self, k: KeyEvent, keymaps: &Keymaps) -> SettingsAction {
        let action = self.keys.handle_key(k, keymaps);
        keys_action(action)
    }

    /// Carry out what the Agents page asked for.
    fn outcome(&mut self, outcome: Outcome) -> SettingsAction {
        match outcome {
            Outcome::Stay | Outcome::Pass => SettingsAction::Stay,
            Outcome::Commit(job) => SettingsAction::CommitAgents(Box::new(job)),
            Outcome::Run(effect) => SettingsAction::Run(effect),
            Outcome::EditFile(path) => SettingsAction::EditFile(path),
            Outcome::CloseEditor => self.request(Leave::Editor),
            Outcome::OpenAgent(name) => {
                let project = self.agents.project().map(str::to_string);
                self.open_agent_editor(project.as_deref(), &name)
            }
            Outcome::ShowPage(page) => self.request(Leave::Page(page)),
            Outcome::Reload => SettingsAction::Reload,
        }
    }

    /// A key while the unsaved-edits prompt asks: `s` / Save, `d` / Discard,
    /// Esc / `c` / Cancel; ←/→ and Tab move between the buttons, Enter
    /// presses the focused one.
    fn prompt_key(&mut self, k: KeyEvent) -> SettingsAction {
        match k.code {
            KeyCode::Esc | KeyCode::Char('c') => self.prompt_choose(2),
            KeyCode::Char('s') => self.prompt_choose(0),
            KeyCode::Char('d') => self.prompt_choose(1),
            // The buttons run Cancel, Discard, Save left to right.
            KeyCode::Right | KeyCode::Tab => {
                self.prompt_choice = self.prompt_choice.saturating_sub(1);
                SettingsAction::Stay
            }
            KeyCode::Left | KeyCode::BackTab => {
                self.prompt_choice = (self.prompt_choice + 1).min(PROMPT_BUTTONS.len() - 1);
                SettingsAction::Stay
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.prompt_choose(self.prompt_choice),
            _ => SettingsAction::Stay,
        }
    }

    fn prompt_choose(&mut self, choice: usize) -> SettingsAction {
        match choice {
            0 => {
                self.guard.save();
                if let Some(editor) = self.editor.as_mut() {
                    let action = editor.save_for_settings();
                    if action == EditorAction::Stay {
                        // A field refused its text: stay, its error showing.
                        self.guard.forget_save();
                    }
                    return self.route_editor(action);
                }
                if let Some(editor) = self.agent_editor.as_mut() {
                    let action = editor.save_for_settings();
                    if action == AgentAction::Stay {
                        self.guard.forget_save();
                    }
                    return self.route_agent(action);
                }
                SettingsAction::Save
            }
            1 => {
                self.discard_edits();
                match self.guard.discard() {
                    Some(leave) => self.go(leave),
                    None => SettingsAction::Stay,
                }
            }
            _ => {
                self.guard.cancel();
                SettingsAction::Stay
            }
        }
    }

    /// A mouse event over Settings (the sidebar column or the main area).
    pub(crate) fn handle_mouse(&mut self, m: MouseEvent, sidebar: Rect, main: Rect) -> SettingsAction {
        let hit = self
            .hits
            .iter()
            .find(|(r, _)| panel::in_rect(*r, m.column, m.row))
            .map(|(_, h)| *h);
        let over_sidebar = panel::in_rect(sidebar, m.column, m.row);
        let over_main = panel::in_rect(main, m.column, m.row);
        if self.nav.page() == SettingsPage::Workspaces && self.workspaces.modal() {
            let ask = self.workspaces.layer_mouse(m);
            return self.ask(ask);
        }
        if self.guard.prompt().is_none() {
            if let Some(editor) = self.editor.as_mut() {
                let action = editor.handle_mouse(m);
                return self.route_editor(action);
            }
            if let Some(editor) = self.agent_editor.as_mut() {
                let action = editor.handle_mouse(m);
                return self.route_agent(action);
            }
        }
        let on_workflows = self.nav.page() == SettingsPage::Workflows;
        if on_workflows && self.workflows.modal_open() && self.guard.prompt().is_none() {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                if let Some(action) = self.workflows.click_modal(m.column, m.row) {
                    return action;
                }
            } else if !matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown) {
                return SettingsAction::Stay;
            }
        }
        if self.nav.page() == SettingsPage::General && self.tools.is_open() && self.guard.prompt().is_none() {
            // A tool's list or the Browse prompt is modal.
            let change = self.tools.mouse(&mut self.form, m);
            return write(change);
        }
        if self.guard.prompt().is_some() {
            // The prompt is modal: only its buttons answer.
            return match (m.kind, hit) {
                (MouseEventKind::Down(MouseButton::Left), Some(Hit::PromptSave)) => self.prompt_choose(0),
                (MouseEventKind::Down(MouseButton::Left), Some(Hit::PromptDiscard)) => self.prompt_choose(1),
                (MouseEventKind::Down(MouseButton::Left), Some(Hit::PromptCancel)) => self.prompt_choose(2),
                _ => SettingsAction::Stay,
            };
        }
        // The Agents page's actions menu is modal too: a click outside it
        // closes it.
        if self.nav.page() == SettingsPage::Agents
            && self.agents.menu_open()
            && matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
            && !matches!(hit, Some(Hit::Agents(AgentsHit::MenuItem(_))))
        {
            self.agents.close_menu();
            return SettingsAction::Stay;
        }
        if self.typing() && over_main && hit != Some(Hit::Action(self.action)) {
            // The command being typed keeps the page: Enter saves it, Esc
            // puts it back. The sidebar still answers (through the prompt).
            return SettingsAction::Stay;
        }
        match m.kind {
            _ if over_main && self.nav.page() == SettingsPage::Keybindings => {
                if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                    self.region = Region::Page;
                }
                keys_action(self.keys.handle_mouse(m))
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let down = matches!(m.kind, MouseEventKind::ScrollDown);
                if over_sidebar {
                    self.region = Region::Sidebar;
                    self.step_stop(down);
                } else if over_main {
                    self.scroll = if down {
                        self.scroll.saturating_add(1)
                    } else {
                        self.scroll.saturating_sub(1)
                    };
                }
                SettingsAction::Stay
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if over_sidebar {
                    self.set_region(Region::Sidebar);
                } else if over_main {
                    self.region = Region::Page;
                }
                match hit {
                    Some(Hit::Stop(stop)) => self.activate_stop(stop),
                    // A click in the field being typed leaves it typing.
                    Some(Hit::Action(i)) if self.typing() && i == self.action => SettingsAction::Stay,
                    Some(Hit::Action(i)) if on_workflows => self.workflows.click_item(i),
                    Some(Hit::Action(i)) if self.nav.page() == SettingsPage::Workspaces => {
                        let ask = self.workspaces.activate(i);
                        self.ask(ask)
                    }
                    Some(Hit::Action(i)) => self.run_action(i),
                    Some(Hit::Agents(h)) => {
                        let column = m.column.saturating_sub(self.page_x);
                        let outcome = self.agents.click(h, column);
                        self.outcome(outcome)
                    }
                    Some(Hit::Theme(theme)) => {
                        if let Some(i) = self.actions().iter().position(|a| a.does == Does::Theme) {
                            self.action = i;
                        }
                        write(self.form.choose_theme(theme))
                    }
                    _ => SettingsAction::Stay,
                }
            }
            _ => SettingsAction::Stay,
        }
    }

    // --- drawing -------------------------------------------------------------

    /// Draw the settings sidebar into `sidebar` (the sidebar column's content
    /// rect) and the shown page into `main`.
    pub(crate) fn render(&mut self, f: &mut Frame, sidebar: Rect, main: Rect, chrome: &Chrome<'_>) {
        self.render_layers(f, sidebar, main, chrome);
        // The button under the pointer lifts, found in what was drawn last
        // (whatever layer is on top).
        let x = sidebar.x.min(main.x);
        let area = Rect { x, width: main.right().saturating_sub(x), ..main };
        self.buttons = button::drawn(f.buffer_mut(), area);
        self.button_lit = button::light_hovered(f.buffer_mut(), &self.buttons, chrome.pointer);
    }

    fn render_layers(&mut self, f: &mut Frame, sidebar: Rect, main: Rect, chrome: &Chrome<'_>) {
        self.hits.clear();
        self.hover_rects.clear();
        self.hover_shown = None;
        let moves = moves(chrome.keymaps, chrome.style);
        self.has_project = chrome.has_project;
        if let Some(editor) = self.editor.as_mut() {
            // Both columns, as the desktop's editor takes the window.
            let x = sidebar.x.min(main.x);
            let area = Rect { x, width: main.right().saturating_sub(x), ..main };
            editor.render(f, area, chrome.status, &moves);
            if self.guard.prompt().is_some() {
                self.render_prompt(f, area);
            }
            return;
        }
        if let Some(editor) = self.agent_editor.as_mut() {
            let x = sidebar.x.min(main.x);
            let area = Rect { x, width: main.right().saturating_sub(x), ..main };
            editor.render(f, area, chrome.status, &moves);
            if self.guard.prompt().is_some() {
                self.render_prompt(f, area);
            }
            return;
        }
        self.render_sidebar(f, sidebar, chrome.project_label);
        let on_workflows = self.nav.page() == SettingsPage::Workflows;
        let hints = if self.keys_focused() {
            self.keys.hints()
        } else if on_workflows && (self.region == Region::Page || self.workflows.modal_open()) {
            self.workflows.hints(&moves)
        } else if self.region == Region::Page && self.nav.page() == SettingsPage::Agents {
            self.agents.hints(&moves)
        } else if self.nav.page() == SettingsPage::General && self.tools.is_open() {
            self.tools.hints(&moves)
        } else {
            hints(self.region, self.nav.page(), self.typing(), &moves)
        };
        self.has_project = chrome.has_project;
        // What's over the page (a prompt, a menu, a form) takes the pointer.
        let pointer = chrome.pointer.filter(|_| !self.page_covered());
        self.render_page(f, main, chrome.status, &hints, pointer);
        if on_workflows {
            self.workflows.render_modal(f, main);
        }
        if self.nav.page() == SettingsPage::Agents {
            self.render_agents_menu(f, main);
        }
        if self.nav.page() == SettingsPage::General {
            self.tools.render(&self.form, f, main);
        }
        if self.nav.page() == SettingsPage::Workspaces {
            self.workspaces.render_layer(f, main);
        }
        if self.guard.prompt().is_some() || self.prompt_saving() {
            self.render_prompt(f, main);
        }
    }

    fn render_sidebar(&mut self, f: &mut Frame, area: Rect, project_label: &str) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        let focused = self.region == Region::Sidebar;
        let bottom = area.bottom();
        let mut y = area.y;
        let back = Rect {
            y,
            height: panel::BACK_BLOCK_H.min(area.height),
            ..area
        };
        panel::render_back_button(
            f,
            back,
            focused && self.stop == Stop::Back,
            "Settings",
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        );
        self.hits.push((back, Hit::Stop(Stop::Back)));
        y = (y + panel::BACK_BLOCK_H + 1).min(bottom);
        let inner_x = area.x + 1;
        let inner_w = area.width.saturating_sub(2);
        for section in self.nav.available().sections() {
            if y >= bottom {
                break;
            }
            match section.header {
                Header::Title(title) => {
                    let row = Rect { x: area.x + PAD, y, width: area.width.saturating_sub(PAD), height: 1 };
                    f.render_widget(
                        Paragraph::new(Span::styled(
                            title.to_string(),
                            Style::default().fg(palette().muted).add_modifier(Modifier::BOLD),
                        )),
                        row,
                    );
                    y += 1;
                }
                Header::Project => {
                    // A divider, then the switcher's row.
                    if y < bottom {
                        let rule = Rect { x: inner_x, y, width: inner_w, height: 1 };
                        f.render_widget(
                            Paragraph::new(Span::styled(
                                "─".repeat(inner_w as usize),
                                Style::default().fg(palette().ansi_dark_gray),
                            )),
                            rule,
                        );
                        y += 1;
                    }
                    if y < bottom {
                        let row = Rect { x: inner_x, y, width: inner_w, height: 1 };
                        let selected = self.stop == Stop::Switcher;
                        // The desktop's switcher: the project's name, its
                        // chevron at the right (a dropdown's `▾` here).
                        let w = inner_w as usize;
                        let name = panel::truncate_right(&format!(" {project_label}"), w.saturating_sub(3));
                        let pad = w.saturating_sub(panel::display_width(&name) + 2);
                        let text = format!("{name}{}▾ ", " ".repeat(pad));
                        let line = if selected {
                            Line::from(Span::styled(text, selected_style(focused)))
                        } else {
                            Line::from(vec![
                                Span::styled(name, Style::default().fg(palette().white).add_modifier(Modifier::BOLD)),
                                Span::raw(" ".repeat(pad)),
                                Span::styled("▾ ", Style::default().fg(palette().muted)),
                            ])
                        };
                        f.render_widget(Paragraph::new(line), row);
                        self.hits.push((Rect { x: area.x, width: area.width, ..row }, Hit::Stop(Stop::Switcher)));
                        y += 1;
                    }
                }
            }
            if section.pages.is_empty() {
                continue;
            }
            // The pages as a nav block (the sidebar's half-block selection).
            let selected = section.pages.iter().position(|p| self.stop == Stop::Page(*p));
            let width = inner_w as usize;
            let content: Vec<Line> = section
                .pages
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    page_line(*p, selected == Some(i), focused, *p == self.nav.page(), width)
                })
                .collect();
            let lines = crate::sidebar::bleed_block_lines(content, selected, width, palette().selection);
            let height = (lines.len() as u16).min(bottom - y);
            f.render_widget(
                Paragraph::new(lines),
                Rect { x: inner_x, y, width: inner_w, height },
            );
            for (i, p) in section.pages.iter().enumerate() {
                let row_y = y + 2 * i as u16 + 1;
                if row_y < bottom {
                    self.hits.push((
                        Rect { x: area.x, y: row_y, width: area.width, height: 1 },
                        Hit::Stop(Stop::Page(*p)),
                    ));
                }
            }
            y = (y + height).min(bottom);
        }
    }

    /// Whether something drawn over the page holds it: the unsaved-edits
    /// prompt, a page's menu, modal or form.
    fn page_covered(&self) -> bool {
        self.guard.prompt().is_some()
            || match self.nav.page() {
                SettingsPage::Workflows => self.workflows.modal_open(),
                SettingsPage::Agents => self.agents.menu_open(),
                SettingsPage::Workspaces => self.workspaces.modal(),
                SettingsPage::General => self.tools.is_open(),
                _ => false,
            }
    }

    /// Whether the pointer at `pointer` lights a different table row than
    /// the last draw did (the shell repaints when it does).
    pub(crate) fn hover_moved(&self, pointer: Option<(u16, u16)>) -> bool {
        let button = pointer.and_then(|(x, y)| self.buttons.iter().copied().find(|r| panel::in_rect(*r, x, y)));
        hovered(&self.hover_rects, pointer) != self.hover_shown || button != self.button_lit
    }

    fn render_page(&mut self, f: &mut Frame, area: Rect, status: Option<&str>, hints: &str, pointer: Option<(u16, u16)>) {
        if area.width == 0 || area.height == 0 {
            return;
        }
        // The bottom rows: the status line (when there is one) over the key
        // hints. A one-row area keeps only the hints.
        let mut body = area;
        let hint_row = Rect { y: area.bottom() - 1, height: 1, ..area };
        body.height -= 1;
        render_band(f, hint_row, hints, Style::default().fg(palette().muted).bg(palette().status_bar));
        if let Some(status) = status.filter(|s| !s.is_empty()) {
            if body.height > 1 {
                let row = Rect { y: body.bottom() - 1, height: 1, ..body };
                body.height -= 1;
                render_band(f, row, status, Style::default().fg(palette().yellow));
            }
        }
        let content = Rect {
            x: area.x + PAD,
            y: body.y + 1.min(body.height),
            width: area.width.saturating_sub(PAD * 2),
            height: body.height.saturating_sub(1),
        };
        if content.width == 0 || content.height == 0 {
            return;
        }
        if self.nav.page() == SettingsPage::Keybindings {
            let focused = self.region == Region::Page;
            self.hover_shown = self.keys.render(f, content, focused, pointer, &mut self.hover_rects);
            return;
        }
        self.page_x = content.x;
        let actions = self.actions();
        self.action = self.action.min(actions.len().saturating_sub(1));
        let rows = self.page_rows(content.width as usize, content.height as usize, &actions);
        let focus = match self.nav.page() {
            SettingsPage::Workflows => self.workflows.cursor(),
            SettingsPage::Workspaces => self.workspaces.cursor(),
            _ => self.action,
        };
        // Keep the focused rows in view: the last marked, then the first (an
        // editor marks its caret and its buttons), then clamp to the content.
        let max = (rows.len() as u16).saturating_sub(content.height);
        if self.region == Region::Page {
            let focused = |r: &Row| r.focus || (r.action.is_some() && r.action == Some(focus));
            let first = rows.iter().position(focused);
            let last = rows.iter().rposition(focused);
            for at in [last, first].into_iter().flatten() {
                // A row with more below it stays off the last line, which
                // says "↓ more".
                let room = if at + 1 < rows.len() { content.height.saturating_sub(1).max(1) } else { content.height };
                let at = at as u16;
                if at < self.scroll {
                    self.scroll = at;
                } else if at >= self.scroll + room {
                    self.scroll = at + 1 - room;
                }
            }
        }
        self.scroll = self.scroll.min(max);
        let clipped_below = self.scroll < max;
        let shown = rows.iter().skip(self.scroll as usize).take(content.height as usize);
        let shown_h = (content.height as usize).saturating_sub(usize::from(clipped_below));
        // Each table line's rect (out in the gutter), then the row the
        // pointer is on: all its lines draw lit.
        for (i, row) in shown.clone().take(shown_h).enumerate() {
            if let Some(key) = row.table_row {
                let rect = Rect { y: content.y + i as u16, height: 1, ..content };
                self.hover_rects.push((bleed_rect(rect, area), key));
            }
        }
        self.hover_shown = hovered(&self.hover_rects, pointer);
        for (i, row) in shown.enumerate() {
            let rect = Rect { y: content.y + i as u16, height: 1, ..content };
            let last = i as u16 + 1 == content.height;
            if last && clipped_below {
                // There's more below: say so on the last row.
                f.render_widget(
                    Paragraph::new(Span::styled("↓ more", Style::default().fg(palette().muted))),
                    rect,
                );
                continue;
            }
            if row.table_row.is_some() {
                let lit = row.table_row == self.hover_shown;
                let line = row.hover.as_ref().filter(|_| lit).unwrap_or(&row.line);
                f.render_widget(Paragraph::new(line.clone()), bleed_rect(rect, area));
            }
            // Every row comes drawn, its focus included; a table's line
            // draws out in the gutter, above.
            let line = row.table_row.is_none().then(|| row.line.clone());
            if let Some(line) = line {
                f.render_widget(Paragraph::new(line), rect);
            }
            if !row.targets.is_empty() {
                for &(x, w, hit) in &row.targets {
                    let r = Rect { x: content.x + x, width: w, ..rect }.intersection(content);
                    self.hits.push((r, hit));
                }
            } else if let Some(a) = row.action {
                self.hits.push((Rect { x: area.x, width: area.width, ..rect }, Hit::Action(a)));
            }
            if let Some(hit) = row.hit.filter(|_| row.targets.is_empty()) {
                let full = Rect { x: area.x, width: area.width, ..rect };
                match row.split {
                    // Two targets on one row: left of `at`, and from it on.
                    Some((at, right)) => {
                        let at = (content.x + at).min(full.right());
                        self.hits.push((Rect { width: at - full.x, ..full }, hit));
                        self.hits.push((Rect { x: at, width: full.right() - at, ..full }, right));
                    }
                    None => self.hits.push((full, hit)),
                }
            }
        }
    }

    /// The shown page's rows at `width` (`height` rows of content).
    fn page_rows(&mut self, width: usize, height: usize, actions: &[PageAction]) -> Vec<Row> {
        match self.nav.page() {
            SettingsPage::General => {
                let focus = (self.region == Region::Page).then_some(self.action);
                general_rows(&self.general, &self.form, self.save_error.as_deref(), width, actions, focus)
            }
            SettingsPage::Terminal => terminal_rows(width),
            SettingsPage::Workflows => self.workflows.rows(width, self.region == Region::Page, self.has_project),
            SettingsPage::Agents => {
                let focused = self.region == Region::Page;
                self.agents.rows(width, height, focused)
            }
            SettingsPage::Workspaces => self
                .workspaces
                .rows(width, self.region == Region::Page)
                .into_iter()
                .map(|r| Row {
                    action: r.item,
                    table_row: r.table_row,
                    hover: r.hover,
                    targets: r.targets.into_iter().map(|(x, w, i)| (x, w, Hit::Action(i))).collect(),
                    ..Row::text(r.line)
                })
                .collect(),
            // Keybindings draws itself.
            SettingsPage::Keybindings => Vec::new(),
        }
    }

    /// The Agents page's actions menu, over the page: the row's actions,
    /// each with its key; the selected one, when refused, with why.
    fn render_agents_menu(&mut self, f: &mut Frame, area: Rect) {
        let Some((name, items)) = self.agents.menu_lines() else {
            return;
        };
        let width = 46.min(area.width);
        let text_w = width.saturating_sub(4) as usize;
        let mut lines: Vec<(Line, Option<usize>)> = Vec::new();
        for (i, item) in items.iter().enumerate() {
            // The label and its key, as the other menus draw them.
            let (label, key) = match item.label.rsplit_once("  ") {
                Some((label, key)) => (label.to_string(), key.to_string()),
                None => (item.label.clone(), String::new()),
            };
            lines.push((menu_line(&label, &key, text_w + 2, item.selected, item.why.is_some()), Some(i)));
            if let (true, Some(why)) = (item.selected, &item.why) {
                for part in shelbi_app::overlay::error_log::wrap(why, text_w.saturating_sub(2).max(1)) {
                    lines.push((Line::from(Span::styled(format!("   {part}"), Style::default().fg(palette().yellow))), None));
                }
            }
        }
        let height = (lines.len() as u16 + 2).min(area.height);
        if width < 12 || height < 3 {
            return;
        }
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        let content = overlay_panel(f, rect, area, Some(&name));
        let inner = Rect { x: content.x - 1, width: content.width + 2, ..content };
        for (n, (line, item)) in lines.into_iter().take(inner.height as usize).enumerate() {
            let r = Rect { y: inner.y + n as u16, height: 1, ..inner };
            f.render_widget(Paragraph::new(line), r);
            if let Some(i) = item {
                self.hits.insert(0, (r, Hit::Agents(AgentsHit::MenuItem(i))));
            }
        }
    }

    fn render_prompt(&mut self, f: &mut Frame, area: Rect) {
        // The desktop's leave dialog: "Save changes to {name}?", what's
        // unsaved, then Cancel, Discard and Save at the right.
        let (name, what) = if self.guard.prompt() == Some(&Leave::Editor) {
            ("the shared instructions".to_string(), "the shared instructions")
        } else if let Some(editor) = self.editor.as_ref() {
            (editor.name().to_string(), "this workflow")
        } else if let Some(editor) = self.agent_editor.as_ref() {
            (editor.name().to_string(), "this agent")
        } else {
            (self.nav.page().label().to_string(), "this page")
        };
        let saving = self.prompt_saving();
        let body = if saving { "Saving…".to_string() } else { format!("Your edits to {what} haven’t been saved.") };
        let buttons = leave_buttons(self.prompt_choice, !saving);
        let hint = if saving { "" } else { LEAVE_HINT };
        let title = format!("Save changes to {name}?");
        let at = render_dialog(f, area, 60, &title, &[(body, Style::default().fg(palette().foreground))], &buttons, hint);
        let hits = [Hit::PromptSave, Hit::PromptDiscard, Hit::PromptCancel];
        for (r, i) in at.into_iter().filter(|_| !saving) {
            self.hits.push((r, hits[i]));
        }
    }
}

/// A preference change to save, if there is one.
fn write(change: Option<Change>) -> SettingsAction {
    change.map_or(SettingsAction::Stay, SettingsAction::Write)
}

/// What the shell does for a Keybindings page action.
fn keys_action(action: KeysAction) -> SettingsAction {
    match action {
        KeysAction::Stay => SettingsAction::Stay,
        KeysAction::Save(change) => SettingsAction::SaveKeys(change),
        KeysAction::OpenFile => SettingsAction::OpenKeys,
    }
}

/// One rendered page row: a line of text, or one of the page's actions
/// (drawn with its focus state at render time).
pub(crate) struct Row {
    line: Line<'static>,
    action: Option<usize>,
    /// What a click on it hits (a page that draws its own focus).
    hit: Option<Hit>,
    /// It holds the page's focus: scrolling keeps it in view.
    focus: bool,
    /// A second click target on the row: from this column of the page's
    /// text on, this hit.
    split: Option<(u16, Hit)>,
    /// A table's line: the table row it belongs to (the pointer lights all
    /// of a row's lines), drawn [`table::BLEED`] columns out into the
    /// gutter.
    table_row: Option<HoverKey>,
    /// The table line as drawn while the pointer is on its row.
    hover: Option<Line<'static>>,
    /// Buttons on the row (`x`, `w`, what a click on each hits) in place of
    /// the whole row's `action` or `hit` as click targets.
    targets: Vec<(u16, u16, Hit)>,
}

impl Row {
    fn text(line: Line<'static>) -> Row {
        Row { line, action: None, hit: None, focus: false, split: None, table_row: None, hover: None, targets: Vec::new() }
    }

    /// A line of a [`section_heading`], its buttons running `actions`
    /// (indexed as the heading's buttons were); the row holds the focus of
    /// the first one's action, if any.
    pub(crate) fn heading(h: HeadingLine, actions: &[Option<usize>]) -> Row {
        let action = h.targets.iter().find_map(|&(_, _, b)| actions.get(b).copied().flatten());
        let hits: Vec<Option<Hit>> = actions.iter().map(|a| a.map(Hit::Action)).collect();
        Row { action, ..Row::buttons(h, &hits, false) }
    }

    /// A line holding buttons (a [`section_heading`]'s, a row of
    /// [`button::row`]'s), each hitting `hits[i]`; `focus` when one of them
    /// holds the page's focus.
    pub(crate) fn buttons(h: HeadingLine, hits: &[Option<Hit>], focus: bool) -> Row {
        let targets: Vec<(u16, u16, Hit)> =
            h.targets.iter().filter_map(|&(x, w, b)| hits.get(b).copied().flatten().map(|hit| (x, w, hit))).collect();
        Row { targets, focus, ..Row::text(h.line) }
    }

    /// Table `table`'s line `l`: drawn out in the gutter, lit with its row.
    /// The header and its rule draw out too, but nothing lights them.
    pub(crate) fn table(l: TableLine, table: u8) -> Row {
        let key = (table, l.row.unwrap_or(usize::MAX));
        Row { table_row: Some(key), hover: l.hover, ..Row::text(l.line) }
    }

    /// A row a click hits, drawn focused (or not) by the page.
    fn hit(line: Line<'static>, hit: Hit, focus: bool) -> Row {
        Row { hit: Some(hit), focus, ..Row::text(line) }
    }

    fn blank() -> Row {
        Row::text(Line::raw(""))
    }
}

/// The General page: the appearance and the editor, then where this
/// computer's settings and the shown project's live, and the Open in editor
/// paths. Each focusable row sits at its index in `actions`.
fn general_rows(
    job: &Job<GeneralSummary>,
    form: &GeneralForm,
    save_error: Option<&str>,
    width: usize,
    actions: &[PageAction],
    focus: Option<usize>,
) -> Vec<Row> {
    let at = |does: &Does| actions.iter().position(|a| a.does == *does);
    let mut rows = vec![Row::text(Line::from(Span::styled(
        "General",
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
    )))];
    wrapped(&mut rows, "Your appearance, editor and diff tool, saved to config.yaml as you change them.", width, Style::default().fg(palette().muted));
    if let Some(e) = save_error {
        wrapped(&mut rows, &format!("Couldn't save: {e}"), width, Style::default().fg(palette().red));
    }
    if let Some(message) = form.status().message() {
        let color = match form.status() {
            SaveStatus::Failed(_) => palette().red,
            SaveStatus::Saved => palette().green,
            _ => palette().yellow,
        };
        wrapped(&mut rows, &message, width, Style::default().fg(color));
    }
    rows.push(Row::blank());
    let label_w = label_width(width);
    let summary = match job.state() {
        JobState::Idle | JobState::Running => {
            rows.push(Row::text(Line::from(Span::styled(
                "Loading…",
                Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
            ))));
            None
        }
        JobState::Failed(e) => {
            wrapped(&mut rows, &format!("Couldn't read the settings: {e}"), width, Style::default().fg(palette().red));
            wrapped(&mut rows, "Press r, or choose Try again.", width, Style::default().fg(palette().muted));
            None
        }
        JobState::Done(summary) => Some(summary),
    };
    if let Some(summary) = summary {
        if let Some(e) = form.unreadable() {
            heading(&mut rows, "Appearance and tools");
            wrapped(&mut rows, &format!("config.yaml doesn't read: {e}"), width, Style::default().fg(palette().red));
            wrapped(
                &mut rows,
                "Shelbi leaves it as it is. Fix it in your editor; Settings reads it again when you come back.",
                width,
                Style::default().fg(palette().muted),
            );
            rows.push(Row::blank());
        }
        if let Some(i) = at(&Does::Theme) {
            heading(&mut rows, "Appearance");
            // The label over its control, as the fields read: a segmented
            // control, each segment a click target down its three rows.
            rows.push(Row::text(textbox::label("Theme", focus == Some(i), false, width)));
            let labels: Vec<&str> = general::THEMES.iter().map(|t| general::theme_label(*t)).collect();
            let chosen = general::THEMES.iter().position(|t| *t == form.theme()).unwrap_or(0);
            let (lines, at) = button::segmented(&labels, chosen, focus == Some(i), width);
            let targets: Vec<(u16, u16, Hit)> =
                at.iter().zip(general::THEMES).map(|(&(x, w), t)| (x, w, Hit::Theme(t))).collect();
            for line in lines {
                rows.push(Row { action: Some(i), targets: targets.clone(), ..Row::text(line) });
            }
            wrapped(
                &mut rows,
                "Auto is dark here: a terminal doesn't report its appearance. The theme colors Shelbi's lists and panels, never your sessions.",
                width,
                Style::default().fg(palette().muted),
            );
            rows.push(Row::blank());
        }
        if at(&Does::Tool(ToolSetting::Editor)).is_some() {
            heading(&mut rows, "Editor and diff tool");
            for setting in [ToolSetting::Editor, ToolSetting::DiffTool] {
                if let Some(i) = at(&Does::Tool(setting)) {
                    tool_rows(&mut rows, form, setting, focus == Some(i), i, label_w, width);
                    rows.push(Row::blank());
                }
            }
        }
        heading(&mut rows, "This computer");
        let g = &summary.global;
        field(&mut rows, "Settings folder", &summary::display_path(&g.home), label_w, width, None);
        let (config, color) = match &g.config {
            FileState::Present(_) => ("Your settings".to_string(), None),
            FileState::Missing(_) => ("Not created yet: the defaults apply".to_string(), None),
            FileState::Invalid { error, .. } => (format!("Doesn't read: {error}"), Some(palette().red)),
        };
        field(&mut rows, "config.yaml", &config, label_w, width, color);
        let mut keys = match &g.keys {
            FileState::Present(_) => "Your overrides".to_string(),
            FileState::Missing(_) => "Built-in defaults".to_string(),
            FileState::Invalid { error, .. } => format!("Doesn't read: {error}"),
        };
        let mut keys_color = None;
        if g.keymap_problems > 0 {
            keys.push_str(&format!(" · {}", summary::count(g.keymap_problems, "problem")));
            keys_color = Some(palette().yellow);
        }
        field(&mut rows, "keys.yaml", &keys, label_w, width, keys_color);
        if let Some(i) = actions.iter().position(|a| matches!(a.does, Does::EditFile(_))) {
            rows.push(Row::blank());
            action_buttons(&mut rows, actions, i..i + 1, focus, width);
        }
        rows.push(Row::blank());
        match &summary.project {
            None => {
                heading(&mut rows, "Project");
                wrapped(&mut rows, "No project is open.", width, Style::default().fg(palette().foreground));
                wrapped(
                    &mut rows,
                    "Switch to a project, or add one, to see where its settings live.",
                    width,
                    Style::default().fg(palette().muted),
                );
            }
            Some(Err(e)) => {
                heading(&mut rows, "Project");
                wrapped(&mut rows, &format!("Couldn't read this project's settings: {e}"), width, Style::default().fg(palette().red));
                wrapped(
                    &mut rows,
                    "Try again, fix its project settings in your editor, or switch project.",
                    width,
                    Style::default().fg(palette().muted),
                );
            }
            Some(Ok(p)) => project_rows(&mut rows, p, label_w, width),
        }
    }
    // The summary's own actions, after everything above, then About.
    let about = at(&Does::Run(Effect::OpenAbout)).unwrap_or(actions.len());
    let first = actions[..about]
        .iter()
        .position(|a| matches!(a.does, Does::Run(_) | Does::Reload))
        .unwrap_or(about);
    if first < about {
        rows.push(Row::blank());
        let title = match job.done().and_then(|s| s.project.as_ref()) {
            Some(Ok(_)) => "Open in editor",
            _ => "What you can do",
        };
        heading(&mut rows, title);
        rows.push(Row::blank());
        action_buttons(&mut rows, actions, first..about, focus, width);
    }
    if about < actions.len() {
        rows.push(Row::blank());
        heading(&mut rows, "About");
        rows.push(Row::blank());
        action_buttons(&mut rows, actions, about..about + 1, focus, width);
    }
    rows
}

/// Page actions `range` as Secondary buttons flowing left to right, a new
/// band of three rows when the next won't fit; a lone one has its note
/// (what it opens, the version) beside it.
fn action_buttons(rows: &mut Vec<Row>, actions: &[PageAction], range: std::ops::Range<usize>, focus: Option<usize>, width: usize) {
    let lone = range.len() == 1;
    let mut band: Vec<(Button, Option<usize>)> = Vec::new();
    let mut used = 0;
    for i in range {
        let b = Button::secondary(&actions[i].label).focused(focus == Some(i));
        let w = b.width() + if band.is_empty() { 0 } else { button::GAP };
        if !band.is_empty() && used + w > width {
            button_rows(rows, &band, width, Align::Left);
            band.clear();
            used = 0;
        }
        used += b.width() + if band.is_empty() { 0 } else { button::GAP };
        band.push((b, Some(i)));
    }
    let at = rows.len();
    button_rows(rows, &band, width, Align::Left);
    if let (true, Some((b, Some(i)))) = (lone, band.first()) {
        if let (Some(note), Some(row)) = (&actions[*i].note, rows.get_mut(at + 1)) {
            if b.width() + 2 + panel::display_width(note) <= width {
                row.line.spans.push(Span::styled(format!("  {note}"), Style::default().fg(palette().muted)));
            }
        }
    }
}

/// The Terminal page: what the host terminal owns, its sixteen colors as
/// sessions draw them, and a short session drawn the way sessions are.
fn terminal_rows(width: usize) -> Vec<Row> {
    let muted = Style::default().fg(palette().muted);
    let mut rows = vec![Row::text(Line::from(Span::styled(
        "Terminal",
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
    )))];
    wrapped(
        &mut rows,
        "Your terminal app draws Shelbi, so it owns how sessions look: the font and its size, the ANSI colors, window opacity and blur, and keyboard options such as Option as Meta. Change them in your terminal's settings.",
        width,
        Style::default().fg(palette().foreground),
    );
    wrapped(
        &mut rows,
        "Shelbi never changes your terminal's colors. Its theme (General) colors only Shelbi's own lists and panels.",
        width,
        muted,
    );
    rows.push(Row::blank());
    // The desktop's ANSI colors section, read-only here: these are the
    // terminal's own.
    heading(&mut rows, "ANSI colors");
    wrapped(&mut rows, "Regular and bold colors used by terminal applications. These are your terminal's own.", width, muted);
    rows.push(Row::blank());
    swatch_rows(&mut rows, width);
    rows.push(Row::blank());
    heading(&mut rows, "Preview");
    wrapped(&mut rows, "A session as Shelbi draws it here, in those colors.", width, muted);
    rows.extend(preview_lines(width).into_iter().map(Row::text));
    rows.push(Row::blank());
    heading(&mut rows, "The desktop app");
    wrapped(
        &mut rows,
        "The desktop app has its own terminal font, colors and window effects (under desktop: in config.yaml). They don't apply here.",
        width,
        muted,
    );
    rows
}

/// The sixteen ANSI colors as the desktop's table lays them out: a row per
/// color, its name, then its regular and bold swatches, painted as a
/// session's cells are ([`ansi_swatches`](super::terminal_view::ansi_swatches)):
/// the host terminal's own palette, what a session's colors look like here.
fn swatch_rows(rows: &mut Vec<Row>, width: usize) {
    let colors = super::terminal_view::ansi_swatches();
    const NAMES: [&str; 8] = ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"];
    const NAME_W: usize = 10;
    if width < 3 {
        // No room for a pair: each swatch on its own row.
        for color in colors.iter() {
            rows.push(Row::text(Line::from(Span::styled(" ", Style::default().bg(*color)))));
        }
        return;
    }
    // A narrow page narrows the names, then the swatches.
    let name_w = NAME_W.min(width - 3);
    let room = width - name_w;
    let swatch = ((room - 1) / 2).clamp(1, 6);
    let gap = (room - 2 * swatch).clamp(1, 4);
    let head = Style::default().fg(palette().muted).add_modifier(Modifier::BOLD);
    let column = |text: &str, w: usize| format!("{:<w$}", panel::truncate_right(text, w));
    let header = format!("{}{}{}", column("Color", name_w), column("Regular", swatch + gap), "Bold");
    rows.push(Row::text(Line::from(Span::styled(panel::truncate_right(&header, width), head))));
    for (i, name) in NAMES.iter().enumerate() {
        rows.push(Row::text(Line::from(vec![
            Span::styled(column(name, name_w), Style::default().fg(palette().gray)),
            Span::styled(" ".repeat(swatch), Style::default().bg(colors[i])),
            Span::raw(" ".repeat(gap)),
            Span::styled(" ".repeat(swatch), Style::default().bg(colors[i + 8])),
        ])));
    }
}

/// The shared short preview session, run through a session's emulator and
/// drawn with the sessions' own cell mapping, one line per row.
fn preview_lines(width: usize) -> Vec<Line<'static>> {
    let cols = width.clamp(1, 80) as u16;
    let rows = terminal_preview::SHORT_ROWS;
    let pane = super::terminal_view::TerminalPane::canned(
        shelbi_term::Size::new(cols, rows),
        &terminal_preview::short(cols),
    );
    let area = Rect::new(0, 0, cols, rows);
    let mut buf = Buffer::empty(area);
    pane.render(&mut buf, area, true);
    (0..rows).map(|y| buffer_line(&buf, y)).collect()
}

/// Row `y` of `buf` as a line, runs of one style merged.
fn buffer_line(buf: &Buffer, y: u16) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut style = Style::default();
    for x in 0..buf.area.width {
        let cell = &buf[(x, y)];
        let cell_style = cell.style();
        if cell_style != style && !run.is_empty() {
            spans.push(Span::styled(std::mem::take(&mut run), style));
        }
        style = cell_style;
        run.push_str(cell.symbol());
    }
    let run = run.trim_end().to_string();
    if !run.is_empty() || spans.is_empty() {
        spans.push(Span::styled(run, style));
    }
    Line::from(spans)
}

fn project_rows(rows: &mut Vec<Row>, p: &ProjectSummary, label_w: usize, width: usize) {
    heading(rows, &p.label);
    let lives = if p.in_repo {
        "In the repository, shared through Git"
    } else {
        "On this computer, outside the repository"
    };
    field(rows, "Settings live", lives, label_w, width, None);
    field(rows, "Folder", &summary::display_path(&p.settings_dir), label_w, width, None);
    let (workflows, color) = match &p.workflows {
        Ok((w, s)) => (format!("{} · {}", summary::count(*w, "workflow"), summary::count(*s, "status")), None),
        Err(e) => (format!("Don't read: {e}"), Some(palette().red)),
    };
    // "statuss" never shows: statuses pluralize by hand.
    let workflows = workflows.replace("statuss", "statuses");
    field(rows, "Workflows", &workflows, label_w, width, color);
    let (agents, color) = match &p.agents {
        Ok(n) => (summary::count(*n, "agent"), None),
        Err(e) => (format!("Don't list: {e}"), Some(palette().red)),
    };
    field(rows, "Agents", &agents, label_w, width, color);
    field(rows, "Machines", &summary::count(p.machines, "machine"), label_w, width, None);
}

pub(crate) fn heading(rows: &mut Vec<Row>, text: &str) {
    // The desktop's section title: loud and bold.
    rows.push(Row::text(Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
    ))));
}

pub(crate) fn wrapped(rows: &mut Vec<Row>, text: &str, width: usize, style: Style) {
    for line in shelbi_app::overlay::error_log::wrap(text, width.max(1)) {
        rows.push(Row::text(Line::from(Span::styled(line, style))));
    }
}

/// A `label  value` row. At a narrow width (no room for both side by side)
/// the value drops to its own line under the label.
fn field(rows: &mut Vec<Row>, label: &str, value: &str, label_w: usize, width: usize, color: Option<ratatui::style::Color>) {
    let value_style = Style::default().fg(color.unwrap_or(palette().foreground));
    let label_style = Style::default().fg(palette().gray);
    let side_by_side = width >= 36 && label_w + 8 <= width;
    if side_by_side {
        let value_w = width - label_w;
        let mut first = true;
        for line in shelbi_app::overlay::error_log::wrap(value, value_w.max(1)) {
            let left = if first {
                format!("{:<label_w$}", panel::truncate_right(label, label_w.saturating_sub(1)))
            } else {
                " ".repeat(label_w)
            };
            first = false;
            rows.push(Row::text(Line::from(vec![
                Span::styled(left, label_style),
                Span::styled(line, value_style),
            ])));
        }
    } else {
        rows.push(Row::text(Line::from(Span::styled(panel::truncate_right(label, width), label_style))));
        for line in shelbi_app::overlay::error_log::wrap(value, width.saturating_sub(2).max(1)) {
            rows.push(Row::text(Line::from(Span::styled(format!("  {line}"), value_style))));
        }
    }
}

/// What an Open in editor row opens.
fn edit_note(target: &EditTarget) -> String {
    match target {
        EditTarget::Project => "project.yaml".into(),
        EditTarget::Agent(_) => "instructions.md".into(),
        EditTarget::ZenMode => "zenmode.md".into(),
        EditTarget::Workflows => "workflows/".into(),
        EditTarget::ChangelogTemplate => "changelog-template.md".into(),
    }
}

/// The label column's width on a page `width` wide.
fn label_width(width: usize) -> usize {
    17.min(width / 2)
}

/// A text field's rows on a page: its label, then `field`'s box (as wide as
/// the page, up to [`textbox::MAX_W`]), every row item `action`'s so a click
/// anywhere on it lands there. The box's text row holds the page's focus
/// while it's typed in.
pub(crate) fn field_rows(rows: &mut Vec<Row>, label: &str, field: Textbox<'_>, focused: bool, disabled: bool, action: Option<usize>, width: usize) {
    rows.push(Row { action, ..Row::text(textbox::label(label, focused, disabled, width)) });
    box_rows(rows, field, action, width);
}

/// [`field_rows`] without the label: a box under one label it shares (a
/// list of commands).
pub(crate) fn box_rows(rows: &mut Vec<Row>, field: Textbox<'_>, action: Option<usize>, width: usize) {
    for (n, line) in field.lines(width.min(textbox::MAX_W)).into_iter().enumerate() {
        rows.push(Row { action, focus: field.typing() && n == 1, ..Row::text(line) });
    }
}

/// One field of [`fields_row`].
pub(crate) struct RowField<'a> {
    pub label: &'a str,
    pub textbox: Textbox<'a>,
    pub focused: bool,
    pub disabled: bool,
    pub action: Option<usize>,
    /// Its width; 0 takes what the others leave.
    pub width: usize,
}

/// The narrowest a flexible field of [`fields_row`] gets before the row
/// stacks.
const ROW_FLEX_MIN: usize = 24;

/// Fields side by side, as the desktop's identity rows set them (Name,
/// Description, Initial status): each its label over its box, two columns
/// apart, each a click target in its own columns. Too narrow for them all,
/// they stack as [`field_rows`] do, a blank row between.
pub(crate) fn fields_row(rows: &mut Vec<Row>, fields: &[RowField<'_>], width: usize) {
    const GAP: usize = 2;
    let fixed: usize = fields.iter().map(|f| f.width).sum::<usize>() + GAP * fields.len().saturating_sub(1);
    let flex = fields.iter().filter(|f| f.width == 0).count();
    let total = width.min(textbox::MAX_W * 2);
    if total < fixed + flex * ROW_FLEX_MIN {
        for (i, f) in fields.iter().enumerate() {
            if i > 0 {
                rows.push(Row::blank());
            }
            field_rows(rows, f.label, f.textbox, f.focused, f.disabled, f.action, width);
        }
        return;
    }
    let share = (total - fixed) / flex.max(1);
    let widths: Vec<usize> = fields.iter().map(|f| if f.width == 0 { share } else { f.width }).collect();
    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new(); 1 + textbox::BOX_H];
    let mut targets = Vec::new();
    let mut x = 0;
    for (i, (f, w)) in fields.iter().zip(&widths).enumerate() {
        if i > 0 {
            for l in lines.iter_mut() {
                l.push(Span::raw(" ".repeat(GAP)));
            }
            x += GAP;
        }
        let label = textbox::label(f.label, f.focused, f.disabled, *w);
        let pad = w.saturating_sub(label.width());
        lines[0].extend(label.spans);
        lines[0].push(Span::raw(" ".repeat(pad)));
        for (l, b) in lines.iter_mut().skip(1).zip(f.textbox.lines(*w)) {
            l.extend(b.spans);
        }
        if let Some(a) = f.action {
            targets.push((x as u16, *w as u16, Hit::Action(a)));
        }
        x += w;
    }
    let action = fields.iter().find(|f| f.focused).or(fields.first()).and_then(|f| f.action);
    let typing = fields.iter().any(|f| f.textbox.typing());
    for (n, spans) in lines.into_iter().enumerate() {
        rows.push(Row { action, targets: targets.clone(), focus: typing && n == 2, ..Row::text(Line::from(spans)) });
    }
}

/// The scroll that keeps item `action`'s rows in `height` rows from
/// `scroll`: all of them (a field's label and box), with the lines said
/// under them when they fit, and at least one line below, since the last
/// line shows "↓ more" while rows are clipped below.
pub(crate) fn follow(rows: &[Row], action: usize, scroll: u16, height: u16) -> u16 {
    let Some(at) = rows.iter().position(|r| r.action == Some(action)) else {
        return scroll;
    };
    let own = rows.iter().skip(at).take_while(|r| r.action == Some(action)).count().max(1);
    let notes = rows.iter().skip(at + own).take_while(|r| r.action.is_none() && !r.focus).count();
    let end = (own - 1 + notes.clamp(1, 3)) as u16;
    let at = at as u16;
    if at < scroll {
        at
    } else if at + end >= scroll + height {
        (at + end + 1).saturating_sub(height).min(at)
    } else {
        scroll
    }
}

/// A picker's rows on a page: its label over a box holding the choice, a
/// `▾` at its right (a `dim` one can't be changed now).
pub(crate) fn picker_rows(rows: &mut Vec<Row>, label: &str, value: &str, focused: bool, dim: bool, action: Option<usize>, width: usize) {
    let field = Textbox::new(value).focused(focused).disabled(dim).trail("▾");
    field_rows(rows, label, field, focused, dim, action, width);
}

/// A tool setting's rows: its picker (the current choice), the command
/// field under it while "Other command…" is typed, what it is for, and
/// what Shelbi runs for it. Every row of the picker and the field is action
/// `i`'s, so a click anywhere on them lands there.
fn tool_rows(rows: &mut Vec<Row>, form: &GeneralForm, setting: ToolSetting, selected: bool, i: usize, label_w: usize, width: usize) {
    let picker = form.picker(setting);
    let typing = form.draft_setting() == Some(setting);
    let current = match picker.entries().get(picker.selected()) {
        Some(general::PickerEntry::Tool(t)) => tool_picker::tool_label(t),
        _ => picker.current_label(),
    };
    picker_rows(rows, setting.label(), &current, selected && !typing, !form.editable(), Some(i), width);
    let muted = Style::default().fg(palette().muted);
    if let (true, Some(draft)) = (typing, form.draft()) {
        let text = draft.text();
        let field = Textbox::new(&text).caret(Some(draft.caret())).placeholder("code --wait").focused(true);
        box_rows(rows, field, Some(i), width);
        wrapped(rows, "⏎ saves · Esc puts it back · blank clears it", width, Style::default().fg(palette().yellow));
    } else {
        let what = match setting {
            ToolSetting::Editor => "Used by Edit in … for workspaces and reviews.",
            ToolSetting::DiffTool => "Used by View Diff for workspaces and reviews.",
        };
        wrapped(rows, what, width, muted);
    }
    let runs = match setting {
        ToolSetting::Editor => {
            let (runs, source) = form.effective_editor();
            format!("{runs} ({})", general::source_label(source))
        }
        ToolSetting::DiffTool => form.effective_diff_tool(),
    };
    field(rows, "Shelbi runs", &runs, label_w, width, None);
}

/// A page's sidebar row: glyph, label, and its number key at the right.
fn page_line(page: SettingsPage, selected: bool, focused: bool, active: bool, width: usize) -> Line<'static> {
    let left = format!(" {} {}", page_glyph(page), page.label());
    let hint = format!("{} ", page.shortcut());
    let gap = width.saturating_sub(panel::display_width(&left) + hint.len());
    if selected {
        Line::from(vec![
            Span::styled(left, selected_style(focused)),
            Span::styled(" ".repeat(gap), selected_style(focused)),
            Span::styled(hint, Style::default().fg(palette().muted).bg(palette().selection)),
        ])
    } else {
        let style = if active {
            Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette().gray)
        };
        Line::from(vec![
            Span::styled(left, style),
            Span::raw(" ".repeat(gap)),
            Span::styled(hint, Style::default().fg(palette().muted)),
        ])
    }
}

/// The selection fill: white and bold while its list has focus, gray when
/// focus is elsewhere (the sidebar's convention).
pub(crate) fn selected_style(focused: bool) -> Style {
    let style = Style::default().bg(palette().selection);
    if focused {
        style.fg(palette().white).add_modifier(Modifier::BOLD)
    } else {
        style.fg(palette().gray)
    }
}

/// A table row's identity on a page, for the pointer's hover: the table
/// (a page numbers its tables) and the row in it. A table's header and
/// rule are row `usize::MAX`, which never has a hover look.
pub(crate) type HoverKey = (u8, usize);

/// The table row whose drawn line is under `pointer`.
pub(crate) fn hovered(drawn: &[(Rect, HoverKey)], pointer: Option<(u16, u16)>) -> Option<HoverKey> {
    let (x, y) = pointer?;
    drawn.iter().find(|(r, _)| panel::in_rect(*r, x, y)).map(|(_, k)| *k).filter(|k| k.1 != usize::MAX)
}

/// A content row's rect widened by [`table::BLEED`] each side, within
/// `area`: where a table line draws.
pub(crate) fn bleed_rect(rect: Rect, area: Rect) -> Rect {
    let bleed = table::BLEED as u16;
    let x = rect.x.saturating_sub(bleed).max(area.x);
    let right = (rect.right() + bleed).min(area.right());
    Rect { x, width: right.saturating_sub(x), ..rect }
}

/// Clear `rect` for a layer over the page, and [`table::BLEED`] columns
/// each side of it within `within`, so a lit table row's half-block caps
/// behind it don't peek out at its edges.
pub(crate) fn clear_layer(f: &mut Frame, rect: Rect, within: Rect) {
    let wide = bleed_rect(rect, within);
    f.render_widget(Clear, Rect { y: rect.y, height: rect.height, ..wide });
}

/// An overlay over a settings page (a dialog, a prompt, a menu), drawn as
/// the command palette is: cleared (with [`clear_layer`]'s gutter), the
/// [`Theme::search`] panel fill in the dialogs' shared muted border
/// ([`crate::overlay::dialog_frame`]). A menu's `title` sits in its top
/// border; a dialog says its own heading ([`modal_form::dialog_heading`]).
/// Returns the content: inset one column inside the border each side.
///
/// [`Theme::search`]: shelbi_app::theme::Theme::search
pub(crate) fn overlay_panel(f: &mut Frame, rect: Rect, within: Rect, title: Option<&str>) -> Rect {
    clear_layer(f, rect, within);
    crate::overlay::dialog_frame(f.buffer_mut(), rect, Some(palette().search), title)
}

/// A dialog centered over `area`, up to `width` wide, as the desktop's: its
/// heading, `body` (each paragraph wrapped in its style), a blank row, its
/// buttons at the right (in the order drawn, each with the index its owner
/// knows it by), and a muted key `hint` under them. Returns each shown
/// button's click target and index.
pub(crate) fn render_dialog(
    f: &mut Frame,
    area: Rect,
    width: u16,
    title: &str,
    body: &[(String, Style)],
    buttons: &[(Button<'_>, usize)],
    hint: &str,
) -> Vec<(Rect, usize)> {
    let width = width.min(area.width);
    if width < 16 || area.height < 6 {
        return Vec::new();
    }
    let text_w = width.saturating_sub(4) as usize;
    let mut lines = modal_form::dialog_heading(title, None, text_w);
    for (text, style) in body {
        lines.extend(
            shelbi_app::overlay::error_log::wrap(text, text_w.max(1))
                .into_iter()
                .map(|l| Line::from(Span::styled(l, *style))),
        );
    }
    lines.push(Line::raw(""));
    let shown: Vec<Button> = buttons.iter().map(|(b, _)| *b).collect();
    let mut at = Vec::new();
    for (band, band_at) in button::bands(&shown, text_w, Align::Right) {
        let y = lines.len() as u16;
        lines.extend(band);
        at.extend(band_at.into_iter().map(|(x, w, i)| (x, y, w, i)));
    }
    if !hint.is_empty() {
        lines.push(Line::from(Span::styled(panel::truncate_right(hint, text_w), Style::default().fg(palette().muted))));
    }
    let height = (lines.len() as u16 + 2).min(area.height);
    let rect = Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    };
    let content = overlay_panel(f, rect, area, None);
    f.render_widget(Paragraph::new(lines), content);
    at.into_iter()
        .filter_map(|(x, y, w, b)| {
            let r = Rect { x: content.x + x, y: content.y + y, width: w, height: 3 }.intersection(content);
            (!r.is_empty()).then_some((r, buttons[b].1))
        })
        .collect()
}

/// A menu's entry `width` wide on an overlay's panel: `label` at the left,
/// `note` (its key, `current`) muted at the right; the selected one a pill
/// on the selection fill between half-block caps (the table's lit row), an
/// inert one muted.
pub(crate) fn menu_line(label: &str, note: &str, width: usize, selected: bool, inert: bool) -> Line<'static> {
    let fill = palette().selection;
    let room = width.saturating_sub(2);
    let gap = room.saturating_sub(panel::display_width(label) + panel::display_width(note) + 2);
    let text = panel::truncate_right(&format!(" {label}{}{note} ", " ".repeat(gap)), room);
    let pad = room.saturating_sub(panel::display_width(&text));
    let text = format!("{text}{}", " ".repeat(pad));
    if selected {
        let style = if inert { Style::default().fg(palette().muted) } else { selected_style(true) };
        Line::from(vec![
            Span::styled("▐", Style::default().fg(fill)),
            Span::styled(text, style.bg(fill)),
            Span::styled("▌", Style::default().fg(fill)),
        ])
    } else {
        let fg = if inert { palette().muted } else { palette().foreground };
        Line::from(vec![Span::raw(" "), Span::styled(text, Style::default().fg(fg)), Span::raw(" ")])
    }
}

/// One line of a [`section_heading`]: the line, and the buttons on it
/// (`x`, `w`, the button's index in the heading's).
pub(crate) struct HeadingLine {
    pub line: Line<'static>,
    pub targets: Vec<(u16, u16, usize)>,
}

/// The narrowest a heading's text keeps beside its buttons; narrower, the
/// buttons go under it.
const HEADING_TEXT_MIN: usize = 24;

/// A section's heading as the desktop draws it: its title (loud and bold)
/// over its description (muted), the section's buttons at the right level
/// with them (under them when there's no room beside).
pub(crate) fn section_heading(title: &str, description: Option<&str>, buttons: &[Button<'_>], width: usize) -> Vec<HeadingLine> {
    let title_style = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(palette().muted);
    let buttons_w = button::row_width(buttons);
    let beside = !buttons.is_empty() && width >= buttons_w + 2 + HEADING_TEXT_MIN;
    let text_w = if beside { width - buttons_w - 2 } else { width };
    let mut text = vec![Line::from(Span::styled(panel::truncate_right(title, text_w), title_style))];
    if let Some(d) = description {
        // A wrap can carry the space it broke at to the next line.
        text.extend(
            shelbi_app::overlay::error_log::wrap(d, text_w.max(1))
                .into_iter()
                .map(|l| Line::from(Span::styled(l.trim_start().to_string(), desc_style))),
        );
    }
    let mut out: Vec<HeadingLine> = Vec::new();
    if beside {
        while text.len() < button::BUTTON_H {
            text.push(Line::raw(""));
        }
        let (row, at) = button::row(buttons, buttons_w, Align::Left);
        let x0 = (width - buttons_w) as u16;
        let left: [Line<'static>; 3] = [text[0].clone(), text[1].clone(), text[2].clone()];
        for line in button::beside(left, row, width, buttons_w) {
            let targets = at.iter().enumerate().map(|(b, &(x, w))| (x0 + x, w, b)).collect();
            out.push(HeadingLine { line, targets });
        }
        out.extend(text.into_iter().skip(button::BUTTON_H).map(|line| HeadingLine { line, targets: Vec::new() }));
    } else {
        out.extend(text.into_iter().map(|line| HeadingLine { line, targets: Vec::new() }));
        if !buttons.is_empty() {
            out.push(HeadingLine { line: Line::raw(""), targets: Vec::new() });
            out.extend(button_lines(buttons, width, Align::Left));
        }
    }
    out
}

/// A row of buttons set to `align` in `width` as three [`HeadingLine`]s.
pub(crate) fn button_lines(buttons: &[Button<'_>], width: usize, align: Align) -> Vec<HeadingLine> {
    button::bands(buttons, width, align)
        .into_iter()
        .flat_map(|(lines, at)| lines.into_iter().map(move |line| HeadingLine { line, targets: at.clone() }))
        .collect()
}

/// A row of buttons among a page's or an editor's rows, set to `align`:
/// each runs its item (a click lands on its own columns), and the rows
/// belong to the focused button's item (else the first's), so scrolling
/// keeps them in view.
pub(crate) fn button_rows(rows: &mut Vec<Row>, buttons: &[(Button<'_>, Option<usize>)], width: usize, align: Align) {
    let shown: Vec<Button> = buttons.iter().map(|(b, _)| *b).collect();
    // Too narrow for one row, they wrap to another.
    for (lines, at) in button::bands(&shown, width, align) {
        let band: Vec<&(Button, Option<usize>)> = at.iter().map(|&(_, _, i)| &buttons[i]).collect();
        let action = band.iter().find(|(b, _)| b.is_focused()).or(band.first()).and_then(|(_, a)| *a);
        let targets: Vec<(u16, u16, Hit)> =
            at.iter().filter_map(|&(x, w, i)| buttons[i].1.map(|a| (x, w, Hit::Action(a)))).collect();
        for line in lines {
            rows.push(Row { action, targets: targets.clone(), ..Row::text(line) });
        }
    }
}

/// A rule across the page between two sections (the desktop's full-width
/// divider), with a blank row each side.
pub(crate) fn section_rule(width: usize) -> [Line<'static>; 3] {
    [
        Line::raw(""),
        Line::from(Span::styled("─".repeat(width), Style::default().fg(palette().table_rule))),
        Line::raw(""),
    ]
}

/// A one-row band of `text`, padded across `area`.
fn render_band(f: &mut Frame, area: Rect, text: &str, style: Style) {
    let w = area.width as usize;
    let text = panel::truncate_right(&format!(" {text}"), w);
    let pad = w.saturating_sub(panel::display_width(&text));
    f.render_widget(Paragraph::new(Span::styled(format!("{text}{}", " ".repeat(pad)), style)), area);
}

/// What `k` means to a list under the user's sidebar keymap.
fn nav_for(k: KeyEvent, keymaps: &Keymaps) -> Nav {
    let chord = crate::keymap::chord_from_event(k);
    let sidebar = chord.and_then(|c| keymaps.sidebar.dispatch(c));
    Nav {
        up: k.code == KeyCode::Up || sidebar == Some(SidebarAction::NavUp),
        down: k.code == KeyCode::Down || sidebar == Some(SidebarAction::NavDown),
        activate: sidebar == Some(SidebarAction::Activate) || matches!(chord.map(|c| c.code), Some(Key::Enter)),
    }
}

/// The up / down hint in the user's sidebar chords.
pub(crate) fn moves(keymaps: &Keymaps, style: DisplayStyle) -> String {
    let letter = |a: SidebarAction| {
        keymaps
            .sidebar
            .chords_for(a)
            .into_iter()
            .find(|c| matches!(c.code, Key::Char(_)))
            .map(|c| crate::keymap::format_chord_or_unbound(Some(c), style))
    };
    match (letter(SidebarAction::NavUp), letter(SidebarAction::NavDown)) {
        (Some(u), Some(d)) => format!("↑↓/{u}{d}"),
        _ => "↑↓".to_string(),
    }
}

/// The key hints for the focused region.
fn hints(region: Region, page: SettingsPage, typing: bool, moves: &str) -> String {
    if typing {
        return "type the command · ⏎ save · Esc put back".to_string();
    }
    match region {
        Region::Sidebar => format!("{moves} move · ⏎ open · Tab page · Esc back"),
        Region::Page if page == SettingsPage::Workspaces => {
            format!("{moves} choose · ⏎ open · n new · e edit · d remove · r reload · Tab sidebar · Esc back")
        }
        Region::Page if page == SettingsPage::General => {
            format!("{moves} choose · ←→ theme · ⏎ run · r reload · Tab sidebar · Esc back")
        }
        // Nothing on the page acts: the arrows scroll it.
        Region::Page => format!("{moves} scroll · Tab sidebar · Esc back"),
    }
}

/// The installed tools the shell's tests detect in place of the disk.
#[cfg(test)]
pub(crate) fn fixture_detected() -> shelbi_state::tools::DetectedTools {
    use shelbi_state::tools::ToolOption;
    use shelbi_state::{ToolChoice, ToolKind};
    let cli = |name: &str, command: &str| ToolOption { name: name.into(), kind: ToolKind::Cli, choice: command.into(), app_path: None };
    let app = |name: &str, path: &str| ToolOption {
        name: name.into(),
        kind: ToolKind::App,
        choice: ToolChoice::app(path),
        app_path: Some(path.into()),
    };
    shelbi_state::tools::DetectedTools {
        editors: vec![
            cli("Neovim", "nvim"),
            cli("Helix", "hx"),
            app("Visual Studio Code", "/Applications/Visual Studio Code.app"),
            app("Zed", "/Applications/Zed.app"),
        ],
        diff_tools: vec![cli("Difftastic", "difft"), app("Kaleidoscope", "/Applications/Kaleidoscope.app")],
        git_difftool: Some("vimdiff".into()),
    }
}

/// A General summary for `project` with no disk read: what the shell's tests
/// read instead of `~/.shelbi`.
#[cfg(test)]
pub(crate) fn fixture_summary(project: Option<&str>) -> GeneralSummary {
    use shelbi_app::settings::summary::GlobalSummary;
    use std::path::PathBuf;
    GeneralSummary {
        global: GlobalSummary {
            home: PathBuf::from("/h/.shelbi"),
            config: FileState::Present(PathBuf::from("/h/.shelbi/config.yaml")),
            preferences: Ok(general::Preferences::default()),
            env_editor: None,
            keys: FileState::Missing(PathBuf::from("/h/.shelbi/keys.yaml")),
            keymap_problems: 0,
        },
        project: project.map(|p| {
            Ok(ProjectSummary {
                project: p.into(),
                label: p.to_uppercase(),
                settings_dir: PathBuf::from(format!("/h/.shelbi/projects/{p}")),
                in_repo: false,
                workflows: Ok((2, 9)),
                agents: Ok(6),
                machines: 1,
                edit_targets: vec![
                    shelbi_app::command::EditItem {
                        target: EditTarget::Project,
                        title: "Edit Project Settings".into(),
                    },
                    shelbi_app::command::EditItem {
                        target: EditTarget::ZenMode,
                        title: "Edit Zen Mode".into(),
                    },
                ],
            })
        }),
    }
}

/// The `config.yaml` a test shell reads and writes in place of
/// `~/.shelbi`: what it holds (`Err` for a file that doesn't read), a
/// failure to inject into the next writes, and how many writes landed.
#[cfg(test)]
pub(crate) struct FixtureConfig {
    pub config: Result<UserConfig, String>,
    pub fail_writes: Option<String>,
    pub writes: usize,
}

#[cfg(test)]
impl Default for FixtureConfig {
    fn default() -> Self {
        FixtureConfig {
            config: Ok(UserConfig::default()),
            fail_writes: None,
            writes: 0,
        }
    }
}

/// A test shell's reader and writer over one [`FixtureConfig`]: the reader
/// is [`fixture_summary`] with the store's preferences, the writer applies a
/// change to the store as `update_user_config` does to the file (refusing
/// one that doesn't read, keeping it as it was on an injected failure).
#[cfg(test)]
pub(crate) fn fixture_io(
    store: &Arc<std::sync::Mutex<FixtureConfig>>,
) -> (super::SettingsReader, ConfigWriter) {
    let read = store.clone();
    let reader: super::SettingsReader = Arc::new(move |project| {
        let mut summary = fixture_summary(project);
        summary.global.preferences = match &read.lock().unwrap().config {
            Ok(cfg) => Ok(general::Preferences::from_config(cfg)),
            Err(e) => Err(e.clone()),
        };
        summary
    });
    let write = store.clone();
    let writer: ConfigWriter = Arc::new(move |change| {
        let mut store = write.lock().unwrap();
        if let Some(e) = store.fail_writes.clone() {
            return Err(format!("Couldn't save config.yaml: {e}"));
        }
        let cfg = match &mut store.config {
            Ok(cfg) => cfg,
            Err(e) => return Err(format!("Couldn't save config.yaml: {e}")),
        };
        change.apply(cfg);
        let written = cfg.clone();
        store.writes += 1;
        Ok(written)
    });
    (reader, writer)
}

/// The Workspaces page's pool for `project` with no disk read: the shared
/// fixture's local and SSH machines and five workspaces.
#[cfg(test)]
pub(crate) fn fixture_pool(project: &str) -> Result<PoolIndex, String> {
    use shelbi_app::settings::workspaces::fixtures::{project as pool_project, POOL};
    let mut p = pool_project(POOL);
    p.name = project.to_string();
    let mut index = PoolIndex::build(&p, Some(std::path::Path::new("/Users/me")));
    index.file = Some(std::path::PathBuf::from(format!("/h/.shelbi/projects/{project}.yaml")));
    Ok(index)
}

/// A pool service that refuses everything: what a test that doesn't script
/// the service gets, so nothing reaches the hub or a real config.
#[cfg(test)]
pub(crate) fn unavailable_pool_service() -> workspace_form::PoolService {
    let no = || PoolError::Version("no pool service in this test".into());
    workspace_form::PoolService {
        add: std::sync::Arc::new(move |_, _| Err(no())),
        update: std::sync::Arc::new(move |_, _| Err(no())),
        remove: std::sync::Arc::new(move |_, _| Err(no())),
    }
}

/// A readiness service that reaches nothing (tests that don't script one):
/// every check answers unreachable and nothing is recorded.
#[cfg(test)]
pub(crate) fn unavailable_readiness_service() -> shelbi_app::settings::machine_check::ReadinessService {
    use shelbi_app::settings::machine_check::{CheckOutcome, Reach, ReadinessService, Report};
    ReadinessService {
        check: std::sync::Arc::new(|_, _| {
            CheckOutcome::Checked(Report::unreached(Reach::Unreachable, "Couldn’t connect", "no readiness service in this test"))
        }),
        record: std::sync::Arc::new(|_| {}),
    }
}

/// A machine service that refuses everything (tests that don't save).
#[cfg(test)]
pub(crate) fn unavailable_machine_service() -> shelbi_app::settings::machine_form::MachineService {
    let no = || PoolError::Version("no machine service in this test".into());
    shelbi_app::settings::machine_form::MachineService {
        add: std::sync::Arc::new(move |_, _| Err(no())),
        update: std::sync::Arc::new(move |_, _| Err(no())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::Color;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    /// A writer over a fresh fixture config.
    fn writer() -> ConfigWriter {
        fixture_io(&Arc::new(std::sync::Mutex::new(FixtureConfig::default()))).1
    }

    /// The built-in keymaps (read-only: no disk).
    fn keymaps() -> Keymaps {
        shelbi_state::keymap::validate_keymaps_yaml("", None).0
    }

    #[test]
    fn the_sidebar_stops_run_back_pages_then_switcher() {
        let s = SettingsShell::default();
        assert_eq!(
            s.stops(),
            [
                Stop::Back,
                Stop::Page(SettingsPage::General),
                Stop::Page(SettingsPage::Keybindings),
                Stop::Page(SettingsPage::Terminal),
                Stop::Switcher,
                Stop::Page(SettingsPage::Workspaces),
                Stop::Page(SettingsPage::Workflows),
                Stop::Page(SettingsPage::Agents)
            ]
        );
        assert_eq!(s.page(), SettingsPage::General);
    }

    #[test]
    fn arrows_and_jk_walk_the_stops_and_stop_at_the_ends() {
        let mut s = SettingsShell::default();
        s.open();
        let km = keymaps();
        s.handle_key(key(KeyCode::Char('j')), &km);
        assert_eq!(s.stop, Stop::Page(SettingsPage::Keybindings));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Page(SettingsPage::Terminal));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Switcher);
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Page(SettingsPage::Workspaces));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Page(SettingsPage::Workflows));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Page(SettingsPage::Agents));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Page(SettingsPage::Agents), "stops at the bottom");
        s.handle_key(key(KeyCode::Char('k')), &km);
        s.handle_key(key(KeyCode::Char('k')), &km);
        s.handle_key(key(KeyCode::Char('k')), &km);
        s.handle_key(key(KeyCode::Char('k')), &km);
        s.handle_key(key(KeyCode::Up), &km);
        s.handle_key(key(KeyCode::Up), &km);
        s.handle_key(key(KeyCode::Up), &km);
        assert_eq!(s.stop, Stop::Back);
        s.handle_key(key(KeyCode::Up), &km);
        assert_eq!(s.stop, Stop::Back, "stops at the top");
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Leave);
    }

    #[test]
    fn enter_on_the_switcher_opens_it_and_tab_moves_focus() {
        let mut s = SettingsShell::default();
        s.open();
        let km = keymaps();
        for _ in 0..3 {
            s.handle_key(key(KeyCode::Down), &km);
        }
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::OpenSwitcher);
        assert_eq!(s.handle_key(key(KeyCode::Tab), &km), SettingsAction::Stay);
        assert_eq!(s.region(), Region::Page);
        s.handle_key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT), &km);
        assert_eq!(s.region(), Region::Sidebar);
    }

    #[test]
    fn a_rebound_nav_key_walks_the_stops() {
        let (km, _) = shelbi_state::keymap::validate_keymaps_yaml(
            "defaults:\n  sidebar:\n    nav_down: w\n",
            None,
        );
        let mut s = SettingsShell::default();
        s.open();
        s.handle_key(key(KeyCode::Char('w')), &km);
        assert_eq!(
            s.stop,
            Stop::Page(SettingsPage::Keybindings),
            "the user's nav_down moves the selection"
        );
    }

    #[test]
    fn page_actions_follow_the_loaded_summary() {
        let mut s = SettingsShell::default();
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, ["About Shelbi"], "only About (it reads nothing) while loading");
        let t = s.begin_load(Some("proj"), false).expect("a first load");
        assert!(s.begin_load(Some("proj"), false).is_none(), "already loading for proj");
        assert!(s.loaded(&t, Ok(fixture_summary(Some("proj")))));
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(
            labels,
            ["Theme", "Editor", "Diff tool", "Edit config.yaml", "Edit Project Settings", "Edit Zen Mode", "About Shelbi"]
        );
        let km = keymaps();
        s.set_region(Region::Page);
        for _ in 0..5 {
            s.handle_key(key(KeyCode::Char('j')), &km);
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &km),
            SettingsAction::Run(Effect::OpenEditor {
                target: EditTarget::ZenMode
            })
        );
        assert_eq!(s.handle_key(key(KeyCode::Char('r')), &km), SettingsAction::Reload);
        // The last row opens About Shelbi.
        assert_eq!(s.run_action(s.actions().len() - 1), SettingsAction::Run(Effect::OpenAbout));
        let text = screen(&mut s, 100, 60);
        let about = text.lines().find(|l| l.contains("About Shelbi")).unwrap_or_default();
        assert!(about.contains("  Version "), "the button with its note:\n{text}");
        // No project: switch or add one.
        let t = s.begin_load(None, true).unwrap();
        s.loaded(&t, Ok(fixture_summary(None)));
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(
            labels,
            ["Theme", "Editor", "Diff tool", "Edit config.yaml", "Switch project…", "Add project…", "About Shelbi"]
        );
        // A failed read offers Try again.
        let t = s.begin_load(Some("proj"), true).unwrap();
        s.loaded(&t, Err("gone".into()));
        assert_eq!(s.actions()[0].does, Does::Reload);
    }

    #[test]
    fn a_dirty_page_asks_before_leaving_and_esc_cancels_first() {
        let mut s = SettingsShell::default();
        s.open();
        s.test_edits = Some(Ok(()));
        let km = keymaps();
        assert_eq!(s.handle_key(key(KeyCode::Esc), &km), SettingsAction::Stay);
        assert!(s.prompting());
        assert_eq!(s.handle_key(key(KeyCode::Char('j')), &km), SettingsAction::Stay, "the prompt holds the keys");
        assert_eq!(s.handle_key(key(KeyCode::Esc), &km), SettingsAction::Stay, "Esc closes the prompt first");
        assert!(!s.prompting());
        s.handle_key(key(KeyCode::Esc), &km);
        assert_eq!(s.handle_key(key(KeyCode::Char('d')), &km), SettingsAction::Leave, "discard goes");
        assert!(s.test_edits.is_none());
    }

    #[test]
    fn save_leaves_once_it_lands_and_a_failed_save_stays_with_its_error() {
        let mut s = SettingsShell::default();
        s.open();
        s.test_edits = Some(Err("disk full".into()));
        let km = keymaps();
        s.request(Leave::Project("beta".into()));
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Save, "Save is the default button");
        let Some(SaveStart::Work(ticket, work)) = s.begin_save(Some("proj"), &writer()) else { panic!("a dirty page saves") };
        assert_eq!(s.saved(&ticket, work()), SettingsAction::Stay);
        assert_eq!(s.save_error.as_deref(), Some("disk full"));
        s.test_edits = Some(Ok(()));
        s.request(Leave::Project("beta".into()));
        s.handle_key(key(KeyCode::Char('s')), &km);
        let Some(SaveStart::Work(ticket, work)) = s.begin_save(Some("proj"), &writer()) else { panic!("a dirty page saves") };
        assert_eq!(s.saved(&ticket, work()), SettingsAction::SwitchProject("beta".into()));
        assert!(s.save_error.is_none());
    }

    fn screen(s: &mut SettingsShell, w: u16, h: u16) -> String {
        let km = keymaps();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
        term.draw(|f| {
            let area = f.area();
            let side = Rect { width: 28, ..area };
            let main = Rect { x: 28, width: area.width - 28, ..area };
            let chrome = Chrome {
                project_label: "proj",
                status: None,
                keymaps: &km,
                style: DisplayStyle::Mac,
                has_project: true,
                pointer: None,
            };
            s.render(f, side, main, &chrome);
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

    #[test]
    fn no_project_and_a_failed_read_say_what_to_do() {
        let mut s = SettingsShell::default();
        let t = s.begin_load(None, false).unwrap();
        s.loaded(&t, Ok(fixture_summary(None)));
        let text = screen(&mut s, 100, 50);
        for want in ["No project is open.", "Switch project…", "Add project…", "This computer"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        let mut broken = fixture_summary(Some("proj"));
        broken.project = Some(Err("projects/proj.yaml: bad indent".into()));
        let t = s.begin_load(Some("proj"), true).unwrap();
        s.loaded(&t, Ok(broken));
        let text = screen(&mut s, 100, 70);
        for want in ["Couldn't read this project's settings", "proj.yaml: bad", "Try again", "Edit Project Settings"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        let t = s.begin_load(Some("proj"), true).unwrap();
        assert!(screen(&mut s, 100, 30).contains("Loading…"));
        s.loaded(&t, Err("the read stopped".into()));
        let text = screen(&mut s, 100, 30);
        assert!(text.contains("Couldn't read the settings: the read stopped"), "{text}");
        assert!(text.contains("Press r"), "{text}");
    }

    /// A shell's Settings on General's page, loaded from `summary`, its
    /// focus on the page's first row (the theme).
    fn on_general(summary: GeneralSummary) -> SettingsShell {
        let mut s = SettingsShell::default();
        s.open();
        let t = s.begin_load(Some("proj"), false).unwrap();
        assert!(s.loaded(&t, Ok(summary)));
        s.set_region(Region::Page);
        s
    }

    fn written(theme: ThemePreference, editor: Option<&str>) -> UserConfig {
        UserConfig {
            theme,
            editor: editor.map(shelbi_state::ToolChoice::from),
            ..UserConfig::default()
        }
    }

    #[test]
    fn the_theme_saves_at_once_shows_pending_and_a_failure_goes_back() {
        let mut s = on_general(fixture_summary(Some("proj")));
        assert_eq!(s.take_theme(), Some(ThemePreference::Auto), "a load hands the file's theme to the shell");
        let km = keymaps();
        assert_eq!(
            s.handle_key(key(KeyCode::Right), &km),
            SettingsAction::Write(Change::Theme(ThemePreference::Dark))
        );
        assert!(screen(&mut s, 100, 30).contains("Saving…"), "pending until the write lands");
        assert_eq!(s.handle_key(key(KeyCode::Right), &km), SettingsAction::Stay, "one save at a time");
        s.config_written(Ok(written(ThemePreference::Dark, None)));
        assert_eq!(s.take_theme(), Some(ThemePreference::Dark));
        assert!(screen(&mut s, 100, 30).contains("Saved to ~/.shelbi/config.yaml"));

        // ← wraps, Enter steps on; a failed write puts the control back.
        assert_eq!(
            s.handle_key(key(KeyCode::Left), &km),
            SettingsAction::Write(Change::Theme(ThemePreference::Auto))
        );
        s.config_written(Err("Couldn't save config.yaml: read-only file system".into()));
        assert_eq!(s.take_theme(), None, "nothing to apply");
        assert_eq!(s.form().theme(), ThemePreference::Dark);
        let text = screen(&mut s, 100, 30);
        assert!(text.contains("read-only file system"), "{text}");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &km),
            SettingsAction::Write(Change::Theme(ThemePreference::Light))
        );
    }

    #[test]
    fn a_click_on_a_segment_picks_that_theme() {
        let mut s = on_general(fixture_summary(Some("proj")));
        let text = screen(&mut s, 100, 30);
        let (x, y) = text
            .lines()
            .enumerate()
            .find_map(|(y, l)| l.find(" Light ").map(|x| (l[..x].chars().count() + 1, y)))
            .unwrap_or_else(|| panic!("{text}"));
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: x as u16,
            row: y as u16,
            modifiers: KeyModifiers::NONE,
        };
        let side = Rect::new(0, 0, 28, 30);
        let main = Rect::new(28, 0, 72, 30);
        assert_eq!(
            s.handle_mouse(click, side, main),
            SettingsAction::Write(Change::Theme(ThemePreference::Light))
        );
    }

    /// [`on_general`] with the fixture's installed tools detected.
    fn with_tools(summary: GeneralSummary) -> SettingsShell {
        let mut s = on_general(summary);
        s.tools_detected(fixture_detected());
        s
    }

    #[test]
    fn a_click_anywhere_on_a_picker_opens_its_list() {
        let mut s = with_tools(fixture_summary(Some("proj")));
        let km = keymaps();
        let side = Rect::new(0, 0, 28, 30);
        let main = Rect::new(28, 0, 72, 30);
        for (label, setting) in [("Editor", ToolSetting::Editor), ("Diff tool", ToolSetting::DiffTool)] {
            let text = screen(&mut s, 100, 30);
            let at = text.lines().position(|l| l.trim_end().ends_with(&format!("  {label}"))).unwrap_or_else(|| panic!("{text}"));
            let lines: Vec<&str> = text.lines().collect();
            assert!(lines[at + 1].contains("▄▄▄▄") && lines[at + 2].contains("▾") && lines[at + 3].contains("▀▀▀▀"), "{text}");
            for y in at + 1..=at + 3 {
                assert!(!s.tool_layer().is_open());
                let click = MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 60,
                    row: y as u16,
                    modifiers: KeyModifiers::NONE,
                };
                assert_eq!(s.handle_mouse(click, side, main), SettingsAction::Stay);
                assert_eq!(s.tool_layer().setting(), Some(setting), "a click on row {y} opens the list");
                assert_eq!(s.handle_key(key(KeyCode::Esc), &km), SettingsAction::Stay);
                assert_eq!(s.page(), SettingsPage::General, "Esc closed the list, not Settings");
                screen(&mut s, 100, 30);
            }
        }
    }

    #[test]
    fn the_list_draws_the_choices_and_a_click_picks_an_app() {
        let mut s = with_tools(fixture_summary(Some("proj")));
        let km = keymaps();
        s.handle_key(key(KeyCode::Down), &km);
        s.handle_key(key(KeyCode::Enter), &km);
        let text = screen(&mut s, 100, 40);
        for want in [
            " Editor ",
            "Default ($EDITOR or vim)",
            "Command line",
            "Neovim (nvim)",
            "Applications",
            "Visual Studio Code",
            "Zed",
            "Browse…",
            "Other command…",
            "No setting: runs vim, the default.",
            "⏎ pick · Esc close",
        ] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        let (x, y) = text
            .lines()
            .enumerate()
            .find_map(|(y, l)| l.find("  Zed ").map(|x| (l[..x].chars().count() + 3, y)))
            .unwrap_or_else(|| panic!("{text}"));
        let click = MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: x as u16, row: y as u16, modifiers: KeyModifiers::NONE };
        let zed = shelbi_state::ToolChoice::app("/Applications/Zed.app");
        assert_eq!(
            s.handle_mouse(click, Rect::new(0, 0, 28, 40), Rect::new(28, 0, 72, 40)),
            SettingsAction::Write(Change::Editor(Some(zed.clone())))
        );
        assert!(!s.tool_layer().is_open());
        // Pending until the write lands, then saved.
        assert!(screen(&mut s, 100, 40).contains("Saving…"));
        s.config_written(Ok(UserConfig { editor: Some(zed), ..UserConfig::default() }));
        let text = screen(&mut s, 100, 40);
        for want in ["Saved to ~/.shelbi/config.yaml", " Zed ", "Shelbi runs      Zed (your setting)"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
    }

    #[test]
    fn a_failed_pick_goes_back_and_says_why() {
        let mut s = with_tools(fixture_summary(Some("proj")));
        let km = keymaps();
        for _ in 0..2 {
            s.handle_key(key(KeyCode::Down), &km);
        }
        s.handle_key(key(KeyCode::Enter), &km);
        assert_eq!(s.tool_layer().setting(), Some(ToolSetting::DiffTool));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &km),
            SettingsAction::Write(Change::DiffTool(Some("difft".into())))
        );
        assert!(screen(&mut s, 100, 40).contains("Difftastic (difft)"), "pending shows the pick");
        s.config_written(Err("Couldn't save config.yaml: read-only".into()));
        let text = screen(&mut s, 100, 40);
        assert!(text.contains("read-only") && text.contains("Git's difftool"), "{text}");
        assert!(text.contains("Shelbi runs      Git's difftool (vimdiff)"), "{text}");
    }

    #[test]
    fn other_command_takes_every_key_while_typed_and_saves_only_on_enter() {
        let mut s = with_tools(fixture_summary(Some("proj")));
        let km = keymaps();
        s.handle_key(key(KeyCode::Down), &km);
        // The list's last entry: Other command…, which types under the picker.
        s.handle_key(key(KeyCode::Enter), &km);
        s.handle_key(key(KeyCode::End), &km);
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Stay);
        assert!(s.typing() && !s.tool_layer().is_open());
        // Letters, digits, `r`, `q`, the nav keys: all typed, nothing acts.
        for c in "code -w1rqjk".chars() {
            assert_eq!(s.handle_key(key(KeyCode::Char(c)), &km), SettingsAction::Stay);
        }
        assert_eq!(s.form().draft().unwrap().text(), "code -w1rqjk");
        assert_eq!(s.handle_key(key(KeyCode::Tab), &km), SettingsAction::Stay);
        assert_eq!(s.region(), Region::Page, "Tab doesn't leave the field");
        assert!(screen(&mut s, 100, 30).contains("⏎ save · Esc put back"));
        // Esc puts it back without a write.
        assert_eq!(s.handle_key(key(KeyCode::Esc), &km), SettingsAction::Stay);
        assert!(!s.typing() && s.form().editor().is_none());
        assert!(s.region() == Region::Page, "Esc closed the field, not Settings");

        s.handle_key(key(KeyCode::Enter), &km);
        s.handle_key(key(KeyCode::End), &km);
        s.handle_key(key(KeyCode::Enter), &km);
        assert!(s.paste("code --wait"), "a paste types into the field");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &km),
            SettingsAction::Write(Change::Editor(Some("code --wait".into())))
        );
        s.config_written(Ok(written(ThemePreference::Auto, Some("code --wait"))));
        let text = screen(&mut s, 100, 40);
        for want in ["Visual Studio Code (code --wait)", "Shelbi runs      code --wait (your setting)"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        // Default clears it: the fallback again.
        s.handle_key(key(KeyCode::Enter), &km);
        s.handle_key(key(KeyCode::Home), &km);
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Write(Change::Editor(None)));
        s.config_written(Ok(written(ThemePreference::Auto, None)));
        let text = screen(&mut s, 100, 40);
        assert!(text.contains("Default ($EDITOR or vim)") && text.contains("vim (the default)"), "{text}");
    }

    #[test]
    fn browse_opens_a_path_prompt_over_the_page() {
        let mut s = with_tools(fixture_summary(Some("proj")));
        let km = keymaps();
        s.handle_key(key(KeyCode::Down), &km);
        s.handle_key(key(KeyCode::Enter), &km);
        s.handle_key(key(KeyCode::End), &km);
        s.handle_key(key(KeyCode::Up), &km);
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Stay);
        assert!(s.tool_layer().browsing());
        let text = screen(&mut s, 100, 40);
        for want in [" Browse for an editor ", "/Applications/", "Tab completes", "Cancel", "Choose", "Tab complete"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        // Keys type into the prompt, never the page; a refused path says why.
        s.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL), &km);
        assert!(s.paste("/nonexistent/shelbi-tool"));
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Stay);
        let (text, error) = s.tool_layer().browse_text().unwrap();
        assert_eq!(text, "/nonexistent/shelbi-tool");
        assert!(error.unwrap().contains("doesn't exist"));
        assert!(screen(&mut s, 100, 40).contains("doesn't exist"));
        assert_eq!(s.handle_key(key(KeyCode::Esc), &km), SettingsAction::Stay);
        assert!(!s.tool_layer().is_open() && s.page() == SettingsPage::General);
    }

    #[test]
    fn the_fallback_names_editor_from_the_environment() {
        let mut summary = fixture_summary(Some("proj"));
        summary.global.env_editor = Some("nvim -p".into());
        let mut s = on_general(summary);
        assert!(screen(&mut s, 100, 30).contains("nvim -p ($EDITOR)"));
    }

    #[test]
    fn leaving_with_a_typed_command_asks_and_save_writes_it() {
        let mut s = on_general(fixture_summary(Some("proj")));
        let km = keymaps();
        s.handle_key(key(KeyCode::Down), &km);
        s.handle_key(key(KeyCode::Enter), &km);
        s.handle_key(key(KeyCode::End), &km);
        s.handle_key(key(KeyCode::Enter), &km);
        s.paste("hx");
        // Back from the sidebar (a click, or Ctrl+H then Enter).
        s.set_region(Region::Sidebar);
        assert_eq!(s.request(Leave::Back), SettingsAction::Stay);
        assert!(s.prompting(), "unsaved: the prompt asks");
        assert_eq!(s.handle_key(key(KeyCode::Char('s')), &km), SettingsAction::Save);
        let Some(SaveStart::Work(ticket, work)) = s.begin_save(Some("proj"), &writer()) else { panic!("the typed command saves") };
        let result = work();
        assert_eq!(result.as_ref().unwrap().as_ref().unwrap().editor.as_ref().and_then(shelbi_state::ToolChoice::command), Some("hx"));
        assert_eq!(s.saved(&ticket, result), SettingsAction::Leave);
        assert_eq!(s.form().editor(), Some("hx"));
    }

    #[test]
    fn a_config_that_doesnt_read_offers_no_controls_and_keeps_the_file() {
        let mut summary = fixture_summary(Some("proj"));
        summary.global.config = FileState::Invalid {
            path: PathBuf::from("/h/.shelbi/config.yaml"),
            error: "did not find expected key at line 2".into(),
        };
        summary.global.preferences = Err("did not find expected key at line 2".into());
        let mut s = on_general(summary);
        assert_eq!(s.take_theme(), None, "nothing to apply from a file that doesn't read");
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, ["Edit config.yaml", "Edit Project Settings", "Edit Zen Mode", "About Shelbi"]);
        let text = screen(&mut s, 100, 30);
        assert!(text.contains("config.yaml doesn't read: did not find expected key"), "{text}");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &keymaps()),
            SettingsAction::EditFile(PathBuf::from("/h/.shelbi/config.yaml"))
        );
    }

    #[test]
    fn general_is_readable_in_light_while_typing() {
        crate::theme::with_appearance(shelbi_app::theme::Appearance::Light, || {
            let mut summary = fixture_summary(Some("proj"));
            summary.global.preferences = Ok(general::Preferences {
                theme: ThemePreference::Light,
                editor: Some("code --wait".into()),
                diff_tool: None,
            });
            let mut s = on_general(summary);
            let km = keymaps();
            s.handle_key(key(KeyCode::Down), &km);
            s.handle_key(key(KeyCode::Enter), &km);
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
            term.draw(|f| {
                let chrome = Chrome { project_label: "proj", status: None, keymaps: &km, style: DisplayStyle::Mac, has_project: true, pointer: None };
                s.render(f, Rect::new(0, 0, 28, 40), Rect::new(28, 0, 72, 40), &chrome);
            })
            .unwrap();
            crate::theme::assert_light_readable("settings-general-typing", term.backend().buffer());
        });
    }

    /// The Terminal page's rows at `width`, as drawn text and their spans.
    fn terminal_page(width: usize) -> Vec<Line<'static>> {
        terminal_rows(width).into_iter().map(|r| r.line).collect()
    }

    #[test]
    fn terminal_explains_what_the_host_owns_and_offers_no_controls() {
        let mut s = SettingsShell::default();
        s.open();
        assert_eq!(s.handle_key(key(KeyCode::Char('3')), &keymaps()), SettingsAction::Load);
        assert_eq!(s.page(), SettingsPage::Terminal);
        assert!(s.actions().is_empty(), "nothing on it acts");
        let text = screen(&mut s, 110, 50);
        for want in ["Terminal", "owns how sessions look", "window opacity and blur", "never changes your terminal's colors", "ANSI colors", "Regular", "Bold", "Magenta", "Preview", "Claude Code", "They don't apply here"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        // No controls for the host's settings.
        for absent in ["Font", "Opacity", "Blur", "Option as Meta:", "Install"] {
            assert!(!text.contains(absent), "{absent:?} is the host's:\n{text}");
        }
    }

    #[test]
    fn the_swatches_paint_the_terminal_s_own_sixteen_colors() {
        let rows = terminal_page(80);
        let backgrounds = |label: &str| -> Vec<Color> {
            let line = rows
                .iter()
                .find(|l| l.spans.first().is_some_and(|s| s.content.starts_with(label)))
                .unwrap_or_else(|| panic!("{label}"));
            line.spans.iter().filter_map(|s| s.style.bg).filter(|c| *c != Color::Reset).collect()
        };
        // Each color's row: its regular swatch, then its bold one.
        let names = ["Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White"];
        let pairs: Vec<Vec<Color>> = names.iter().map(|n| backgrounds(n)).collect();
        let regular: Vec<Color> = pairs.iter().map(|p| p[0]).collect();
        let bold: Vec<Color> = pairs.iter().map(|p| p[1]).collect();
        assert_eq!(regular, [Color::Black, Color::Red, Color::Green, Color::Yellow, Color::Blue, Color::Magenta, Color::Cyan, Color::Gray]);
        assert_eq!(
            bold,
            [Color::DarkGray, Color::LightRed, Color::LightGreen, Color::LightYellow, Color::LightBlue, Color::LightMagenta, Color::LightCyan, Color::White]
        );
    }

    #[test]
    fn the_preview_draws_through_the_session_cell_mapping() {
        let lines = preview_lines(80);
        assert_eq!(lines.len(), terminal_preview::SHORT_ROWS as usize);
        let fg_of = |needle: &str| -> Option<Color> {
            lines.iter().find_map(|l| l.spans.iter().find(|s| s.content.contains(needle)).and_then(|s| s.style.fg))
        };
        assert_eq!(fg_of("▐▛"), Some(Color::LightRed), "the mascot is bright red");
        assert_eq!(fg_of("(crates/"), Some(Color::Cyan));
        assert_eq!(fg_of("(cargo test"), Some(Color::Yellow));
        assert_eq!(fg_of("187 -"), Some(Color::Red));
        assert_eq!(fg_of("187 +"), Some(Color::Green));
        assert_eq!(fg_of("⏵⏵"), Some(Color::LightMagenta));
        let text: Vec<String> = lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert!(text.last().unwrap().starts_with("? for shortcuts"), "{text:#?}");
    }

    #[test]
    fn the_terminal_page_fits_narrow_widths() {
        for width in [60, 40, 24, 12, 3, 1] {
            let rows = terminal_page(width);
            // Text wraps (a heading clips); the swatches and the preview are
            // sized to the page.
            let swatches = rows.iter().filter(|l| l.spans.iter().any(|s| s.style.bg.is_some_and(|c| c != Color::Reset)));
            for line in swatches.chain(preview_lines(width).iter()) {
                assert!(line.width() <= width, "{width}: {:?} is {} wide", line, line.width());
            }
            // Each color still shows, narrower.
            let painted = rows.iter().flat_map(|l| &l.spans).filter(|s| s.style.bg.is_some_and(|c| c != Color::Reset)).count();
            assert_eq!(painted, 16, "{width}: sixteen swatches");
        }
        let mut s = SettingsShell::default();
        s.open();
        s.handle_key(key(KeyCode::Char('3')), &keymaps());
        for (w, h) in [(40, 12), (30, 8), (12, 4)] {
            screen(&mut s, w.max(29), h);
        }
    }

    fn heading_text(lines: &[HeadingLine]) -> Vec<String> {
        lines.iter().map(|h| h.line.spans.iter().map(|s| s.content.as_ref()).collect::<String>()).collect()
    }

    #[test]
    fn a_section_heading_sets_its_buttons_beside_its_text_or_under_it() {
        let buttons = [Button::secondary("Edit YAML"), Button::secondary("New status")];
        let wide = section_heading("Statuses", Some("Work transitions through statuses in workflows."), &buttons, 100);
        let text = heading_text(&wide);
        assert!(text[0].starts_with("Statuses") && text[0].trim_end().ends_with('▄'), "{text:#?}");
        assert!(text[1].ends_with("  Edit YAML     New status  "), "{text:#?}");
        // Each button's three rows are its click target, at the right edge.
        assert_eq!(wide[1].targets, vec![(72, 13, 0), (86, 14, 1)]);
        let narrow = section_heading("Statuses", Some("Work transitions through statuses in workflows."), &buttons, 30);
        let text = heading_text(&narrow);
        assert_eq!(text[0], "Statuses");
        assert!(narrow.iter().filter(|h| !h.targets.is_empty()).count() >= 3, "the buttons under it: {text:#?}");
    }

    #[test]
    fn fields_share_a_row_when_they_fit_and_stack_when_they_dont() {
        let fields = |w: usize| {
            let mut rows = Vec::new();
            let f = |label, action, width| RowField { label, textbox: Textbox::new("x"), focused: false, disabled: false, action: Some(action), width };
            fields_row(&mut rows, &[f("Name", 0, 24), f("Description", 1, 0), f("Initial status", 2, 24)], w);
            rows
        };
        let wide = fields(120);
        assert_eq!(wide.len(), 4, "a label row and one box");
        let labels: String = wide[0].line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(labels.starts_with("Name") && labels.contains("Description") && labels.trim_end().ends_with("Initial status"), "{labels}");
        assert_eq!(wide[1].targets.iter().map(|t| (t.0, t.1)).collect::<Vec<_>>(), vec![(0, 24), (26, 68), (96, 24)]);
        // Narrow: label over box, each, a blank between.
        let narrow = fields(60);
        assert_eq!(narrow.len(), 3 * 4 + 2);
    }

    #[test]
    fn a_dialog_is_the_palettes_panel_and_keeps_its_buttons_when_narrow() {
        for width in [80u16, 30] {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, 24)).unwrap();
            let mut hits = Vec::new();
            term.draw(|f| {
                let buttons = [(Button::secondary("Cancel"), 1), (Button::danger("Remove workspace").focused(true), 0)];
                let body = [("Shelbi stops alpha's session.".to_string(), Style::default())];
                hits = render_dialog(f, f.area(), 56, "Remove alpha?", &body, &buttons, "Tab switch");
            })
            .unwrap();
            let buf = term.backend().buffer();
            assert_eq!(hits.len(), 2, "{width}: both buttons answer");
            // The muted border and the panel's fill, as the palette's.
            let (bx, by) = (0..24u16)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == "┌")
                .expect("a border");
            let (ex, ey) = (0..24u16)
                .flat_map(|y| (0..width).map(move |x| (x, y)))
                .find(|&(x, y)| buf[(x, y)].symbol() == "┘")
                .expect("its bottom corner");
            let rect = Rect::new(bx, by, ex + 1 - bx, ey + 1 - by);
            crate::overlay::assert_dialog_border(buf, rect).unwrap_or_else(|e| panic!("{width}: {e}"));
            assert_eq!(buf[(bx + 2, by + 1)].bg, palette().search);
            let (r, i) = hits.iter().find(|(_, i)| *i == 0).unwrap();
            assert_eq!(*i, 0);
            assert_eq!(buf[(r.x + 1, r.y + 1)].bg, palette().button_danger_focus, "{width}: the focused Danger button");
        }
    }

    #[test]
    fn renders_without_panicking_at_tiny_sizes() {
        let mut s = SettingsShell::default();
        let t = s.begin_load(Some("proj"), false).unwrap();
        s.loaded(&t, Ok(fixture_summary(Some("proj"))));
        s.test_edits = Some(Ok(()));
        s.request(Leave::Back);
        let km = keymaps();
        for (w, h) in [(1, 1), (10, 3), (24, 5), (40, 8), (80, 24)] {
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
            term.draw(|f| {
                let area = f.area();
                let side = Rect { width: area.width / 3, ..area };
                let main = Rect { x: side.right(), width: area.width - side.width, ..area };
                let chrome = Chrome {
                    project_label: "Proj",
                    status: Some("status"),
                    keymaps: &km,
                    style: DisplayStyle::Mac,
                    has_project: true,
                    pointer: None,
                };
                s.render(f, side, main, &chrome);
            })
            .unwrap();
        }
    }
}
