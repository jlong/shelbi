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
//!   and the external editor command, over the shared
//!   [`GeneralForm`](shelbi_app::settings::general::GeneralForm), then a
//!   summary of where the settings live with the Open in editor paths. Both
//!   controls save at once, as the desktop app's do: a theme when it is
//!   picked (it repaints every surface on the next frame once the write
//!   lands), the editor when Enter commits what was typed. A save shows as
//!   pending until it lands, and a failed one puts the control back.
//! - Terminal: what the host terminal owns (the font, its ANSI palette,
//!   window effects), its sixteen colors as sessions draw them here, and a
//!   short session drawn through the sessions' own cell mapping. Nothing on
//!   it edits: the TUI never changes the host terminal's palette.
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
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use shelbi_app::settings::general::{self, Change, GeneralForm, SaveStatus};
use shelbi_app::settings::summary::{self, FileState, GeneralSummary, ProjectSummary};
use shelbi_app::settings::terminal_preview;
use shelbi_app::settings::{
    Header, Job, JobState, LeaveGuard, PageSet, SettingsNav, SettingsPage, Ticket,
};
use shelbi_app::{EditTarget, Effect};
use shelbi_state::keymap::{DisplayStyle, Key, Keymaps, SidebarAction};
use shelbi_state::{EditorSource, ThemePreference, UserConfig};

use crate::panel::{self, PAD};
use crate::theme::palette;

/// The pages the TUI offers. A follow-up page joins here once it draws.
pub(crate) const OFFERED: PageSet = PageSet::NONE
    .with(SettingsPage::General)
    .with(SettingsPage::Terminal);

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
    /// Open this file in the user's editor (`config.yaml`).
    EditFile(PathBuf),
}

/// What a page's focusable row does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Does {
    Run(Effect),
    Reload,
    /// The theme's segmented control (←/→, or Enter for the next one).
    Theme,
    /// The editor command's field (Enter types, Enter again saves).
    Editor,
    /// Clear the editor override, back to the fallback.
    ClearEditor,
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
enum Hit {
    Stop(Stop),
    Action(usize),
    /// A theme's segment.
    Theme(ThemePreference),
    PromptSave,
    PromptDiscard,
    PromptCancel,
}

/// A blocking save the shell runs off the UI thread: the config as written
/// when it wrote `config.yaml`.
pub(crate) type SaveWork = Box<dyn FnOnce() -> Result<Option<UserConfig>, String> + Send>;

/// Writes one preference change to `config.yaml` (blocking; run off the UI
/// thread): [`Change::write`] in production, a fixture in tests.
pub(crate) type ConfigWriter = Arc<dyn Fn(&Change) -> Result<UserConfig, String> + Send + Sync>;

/// The prompt's buttons, left to right.
const PROMPT_BUTTONS: [&str; 3] = ["Save", "Discard", "Cancel"];

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
}

/// Settings' state. One per shell: it outlives a visit, so Settings reopens
/// on the page last shown.
pub(crate) struct SettingsShell {
    nav: SettingsNav,
    region: Region,
    stop: Stop,
    /// The General page's summary, loaded per project.
    general: Job<GeneralSummary>,
    /// The General page's appearance and editor controls.
    form: GeneralForm,
    /// A theme loaded or saved that the shell hasn't applied yet.
    theme_out: Option<ThemePreference>,
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
            theme_out: None,
            action: 0,
            scroll: 0,
            guard: LeaveGuard::default(),
            prompt_choice: 0,
            save: Job::default(),
            save_error: None,
            dropped: 0,
            hits: Vec::new(),
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
    pub(crate) fn form(&self) -> &GeneralForm {
        &self.form
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
    }

    pub(crate) fn set_region(&mut self, region: Region) {
        self.region = region;
    }

    /// Whether the shown page holds unsaved edits.
    fn dirty(&self) -> bool {
        #[cfg(test)]
        if self.test_edits.is_some() {
            return true;
        }
        self.form.dirty()
    }

    /// The shown page's save, if it has anything to save.
    pub(crate) fn begin_save(&mut self, project: Option<&str>, writer: &ConfigWriter) -> Option<(Ticket, SaveWork)> {
        #[cfg(test)]
        if let Some(result) = self.test_edits.clone() {
            let ticket = self.save.begin(project);
            return Some((ticket, Box::new(move || result.map(|()| None))));
        }
        // General: the editor command typed but not committed.
        if self.form.dirty() {
            if let Some(change) = self.form.commit_edit() {
                let ticket = self.save.begin(project);
                let writer = writer.clone();
                return Some((ticket, Box::new(move || writer(&change).map(Some))));
            }
        }
        self.guard.forget_save();
        None
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
        self.save_error = None;
    }

    /// A preference write landed: the form takes it, and a theme it saved is
    /// handed to the shell to apply.
    pub(crate) fn config_written(&mut self, result: Result<UserConfig, String>) {
        if let Some(theme) = self.form.saved_result(result) {
            self.theme_out = Some(theme);
        }
    }

    /// The theme `config.yaml` now holds, once, when a load or a save brought
    /// it in: the shell applies it to every surface.
    pub(crate) fn take_theme(&mut self) -> Option<ThemePreference> {
        self.theme_out.take()
    }

    /// A paste (or a dropped file's path) while Settings shows: into the
    /// editor command being typed, else nowhere.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
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
        if leave == Leave::Page(self.nav.page()) {
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
        match leave {
            Leave::Back => SettingsAction::Leave,
            Leave::Page(page) => {
                if self.nav.show(page) {
                    self.stop = Stop::Page(page);
                    self.action = 0;
                    self.scroll = 0;
                }
                SettingsAction::Load
            }
            Leave::Project(project) => SettingsAction::SwitchProject(project),
        }
    }

    /// The shown project changed: the page stays, its project data goes, and
    /// anything still loading for the old project is dropped when it lands.
    pub(crate) fn project_changed(&mut self) {
        self.general.reset();
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
            // Not offered yet: nothing to load.
            SettingsPage::Keybindings
            | SettingsPage::Terminal
            | SettingsPage::Workspaces
            | SettingsPage::Workflows
            | SettingsPage::Agents => None,
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
    /// Edit config.yaml, then the summary's actions.
    fn general_actions(&self) -> Vec<PageAction> {
        let mut out = Vec::new();
        if let JobState::Done(summary) = self.general.state() {
            if self.form.saved().is_some() {
                out.push(PageAction { label: "Theme".into(), note: None, does: Does::Theme });
                out.push(PageAction { label: "Command".into(), note: None, does: Does::Editor });
                if self.form.editor().is_some() {
                    out.push(PageAction {
                        label: "Clear the editor setting".into(),
                        note: Some(format!("use {}", self.form.fallback_editor().0)),
                        does: Does::ClearEditor,
                    });
                }
            }
            out.push(PageAction {
                label: "Edit config.yaml".into(),
                note: Some("in your editor".into()),
                does: Does::EditFile(summary.global.config.path().to_path_buf()),
            });
        }
        out.extend(self.summary_actions());
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
            Does::Editor => {
                self.form.begin_edit();
                SettingsAction::Stay
            }
            Does::ClearEditor => write(self.form.clear_editor()),
            Does::EditFile(path) => SettingsAction::EditFile(path),
        }
    }

    /// Whether the editor command is being typed (it takes the page's keys).
    fn typing(&self) -> bool {
        self.region == Region::Page && self.form.draft().is_some()
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
        if self.guard.prompt().is_some() {
            return self.prompt_key(k);
        }
        if self.save.running() {
            // The prompt's save is running: wait for it.
            return SettingsAction::Stay;
        }
        if self.typing() {
            return self.field_key(k);
        }
        let chord = crate::keymap::chord_from_event(k);
        let sidebar = chord.and_then(|c| keymaps.sidebar.dispatch(c));
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
        let up = k.code == KeyCode::Up || sidebar == Some(SidebarAction::NavUp);
        let down = k.code == KeyCode::Down || sidebar == Some(SidebarAction::NavDown);
        let activate = sidebar == Some(SidebarAction::Activate)
            || matches!(chord.map(|c| c.code), Some(Key::Enter));
        match self.region {
            Region::Sidebar => {
                if up || down {
                    self.step_stop(down);
                } else if activate {
                    return self.activate_stop(self.stop);
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

    /// A key while the unsaved-edits prompt asks: `s` / Save, `d` / Discard,
    /// Esc / `c` / Cancel; ←/→ and Tab move between the buttons, Enter
    /// presses the focused one.
    fn prompt_key(&mut self, k: KeyEvent) -> SettingsAction {
        match k.code {
            KeyCode::Esc | KeyCode::Char('c') => self.prompt_choose(2),
            KeyCode::Char('s') => self.prompt_choose(0),
            KeyCode::Char('d') => self.prompt_choose(1),
            KeyCode::Left | KeyCode::BackTab => {
                self.prompt_choice = self.prompt_choice.saturating_sub(1);
                SettingsAction::Stay
            }
            KeyCode::Right | KeyCode::Tab => {
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
        if self.guard.prompt().is_some() {
            // The prompt is modal: only its buttons answer.
            return match (m.kind, hit) {
                (MouseEventKind::Down(MouseButton::Left), Some(Hit::PromptSave)) => self.prompt_choose(0),
                (MouseEventKind::Down(MouseButton::Left), Some(Hit::PromptDiscard)) => self.prompt_choose(1),
                (MouseEventKind::Down(MouseButton::Left), Some(Hit::PromptCancel)) => self.prompt_choose(2),
                _ => SettingsAction::Stay,
            };
        }
        if self.typing() && over_main && hit != Some(Hit::Action(self.action)) {
            // The command being typed keeps the page: Enter saves it, Esc
            // puts it back. The sidebar still answers (through the prompt).
            return SettingsAction::Stay;
        }
        match m.kind {
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
                    self.region = Region::Sidebar;
                } else if over_main {
                    self.region = Region::Page;
                }
                match hit {
                    Some(Hit::Stop(stop)) => self.activate_stop(stop),
                    // A click in the field being typed leaves it typing.
                    Some(Hit::Action(i)) if self.typing() && i == self.action => SettingsAction::Stay,
                    Some(Hit::Action(i)) => self.run_action(i),
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
        self.hits.clear();
        self.render_sidebar(f, sidebar, chrome.project_label);
        let hints = hints(self.region, self.nav.page(), self.typing(), chrome.keymaps, chrome.style);
        self.render_page(f, main, chrome.status, &hints);
        if self.guard.prompt().is_some() || self.save.running() {
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
                        let text = format!(" ▾ {project_label}");
                        let text = panel::truncate_right(&text, inner_w as usize);
                        let line = if selected {
                            let pad = (inner_w as usize).saturating_sub(panel::display_width(&text));
                            Line::from(Span::styled(
                                format!("{text}{}", " ".repeat(pad)),
                                selected_style(focused),
                            ))
                        } else {
                            Line::from(Span::styled(text, Style::default().fg(palette().cyan)))
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

    fn render_page(&mut self, f: &mut Frame, area: Rect, status: Option<&str>, hints: &str) {
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
        let actions = self.actions();
        self.action = self.action.min(actions.len().saturating_sub(1));
        let rows = self.page_rows(content.width as usize, &actions);
        // Keep the focused action in view, then clamp to the content.
        let max = (rows.len() as u16).saturating_sub(content.height);
        if self.region == Region::Page {
            if let Some(at) = rows.iter().position(|r| r.action == Some(self.action)) {
                let at = at as u16;
                if at < self.scroll {
                    self.scroll = at;
                } else if at >= self.scroll + content.height {
                    self.scroll = at + 1 - content.height;
                }
            }
        }
        self.scroll = self.scroll.min(max);
        let clipped_below = self.scroll < max;
        for (i, row) in rows.iter().skip(self.scroll as usize).take(content.height as usize).enumerate() {
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
            let line = match row.action {
                Some(a) => {
                    let selected = self.region == Region::Page && a == self.action;
                    let width = rect.width as usize;
                    match &actions[a].does {
                        Does::Theme => {
                            let (line, segments) = theme_line(self.form.theme(), selected, label_width(width), width);
                            for (x, w, theme) in segments {
                                if x + w <= rect.width {
                                    self.hits.push((Rect { x: rect.x + x, width: w, ..rect }, Hit::Theme(theme)));
                                }
                            }
                            line
                        }
                        Does::Editor => editor_line(&self.form, selected, label_width(width), width),
                        _ => action_line(&actions[a], selected, width),
                    }
                }
                None => row.line.clone(),
            };
            f.render_widget(Paragraph::new(line), rect);
            if let Some(a) = row.action {
                self.hits.push((Rect { x: area.x, width: area.width, ..rect }, Hit::Action(a)));
            }
        }
    }

    /// The shown page's rows at `width`.
    fn page_rows(&self, width: usize, actions: &[PageAction]) -> Vec<Row> {
        match self.nav.page() {
            SettingsPage::General => general_rows(&self.general, &self.form, self.save_error.as_deref(), width, actions),
            SettingsPage::Terminal => terminal_rows(width),
            SettingsPage::Keybindings
            | SettingsPage::Workspaces
            | SettingsPage::Workflows
            | SettingsPage::Agents => Vec::new(),
        }
    }

    fn render_prompt(&mut self, f: &mut Frame, area: Rect) {
        let width = 44.min(area.width);
        let height = 7.min(area.height);
        if width < 12 || height < 4 {
            return;
        }
        let rect = Rect {
            x: area.x + (area.width - width) / 2,
            y: area.y + (area.height - height) / 2,
            width,
            height,
        };
        f.render_widget(Clear, rect);
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(palette().divider_active))
            .title(Span::styled(
                " Unsaved changes ",
                Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
            ));
        let inner = block.inner(rect);
        f.render_widget(block, rect);
        let text_w = inner.width.saturating_sub(2) as usize;
        let message = if self.save.running() {
            "Saving…".to_string()
        } else {
            format!("Save your changes to {} before leaving?", self.nav.page().label())
        };
        let mut lines: Vec<Line> = shelbi_app::overlay::error_log::wrap(&message, text_w.max(1))
            .into_iter()
            .map(|l| Line::from(Span::styled(l, Style::default().fg(palette().foreground))))
            .collect();
        lines.truncate(inner.height.saturating_sub(2) as usize);
        f.render_widget(
            Paragraph::new(lines),
            Rect { x: inner.x + 1, width: inner.width.saturating_sub(2), height: inner.height.saturating_sub(1), ..inner },
        );
        if self.save.running() {
            return;
        }
        // The buttons on the last row, left to right.
        let y = inner.bottom().saturating_sub(1);
        let mut x = inner.x + 1;
        let hits = [Hit::PromptSave, Hit::PromptDiscard, Hit::PromptCancel];
        for (i, label) in PROMPT_BUTTONS.iter().enumerate() {
            let text = format!(" {label} ");
            let w = text.chars().count() as u16;
            if x + w > inner.right() {
                break;
            }
            let style = if i == self.prompt_choice {
                selected_style(true)
            } else {
                Style::default().fg(palette().gray).bg(palette().selection)
            };
            let r = Rect { x, y, width: w, height: 1 };
            f.render_widget(Paragraph::new(Span::styled(text, style)), r);
            self.hits.push((r, hits[i]));
            x += w + 1;
        }
    }
}

/// A preference change to save, if there is one.
fn write(change: Option<Change>) -> SettingsAction {
    change.map_or(SettingsAction::Stay, SettingsAction::Write)
}

/// One rendered page row: a line of text, or one of the page's actions
/// (drawn with its focus state at render time).
struct Row {
    line: Line<'static>,
    action: Option<usize>,
}

impl Row {
    fn text(line: Line<'static>) -> Row {
        Row { line, action: None }
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
) -> Vec<Row> {
    let at = |does: &Does| actions.iter().position(|a| a.does == *does);
    let action_row = |i: usize| Row { line: Line::raw(""), action: Some(i) };
    let mut rows = vec![Row::text(Line::from(Span::styled(
        "General",
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
    )))];
    wrapped(&mut rows, "Your appearance and editor, saved to config.yaml as you change them.", width, Style::default().fg(palette().muted));
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
            heading(&mut rows, "Appearance and editor");
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
            rows.push(action_row(i));
            wrapped(
                &mut rows,
                "Auto is dark here: a terminal doesn't report its appearance. The theme colors Shelbi's lists and panels, never your sessions.",
                width,
                Style::default().fg(palette().muted),
            );
            rows.push(Row::blank());
        }
        if let Some(i) = at(&Does::Editor) {
            heading(&mut rows, "Editor");
            rows.push(action_row(i));
            if form.draft().is_some() {
                wrapped(&mut rows, "⏎ saves · Esc puts it back · blank clears it", width, Style::default().fg(palette().yellow));
            } else {
                wrapped(
                    &mut rows,
                    "What Open in editor and a review's Edit view run. Arguments are kept (code --wait).",
                    width,
                    Style::default().fg(palette().muted),
                );
            }
            let (runs, source) = form.effective_editor();
            field(&mut rows, "Shelbi runs", &format!("{runs} ({})", general::source_label(source)), label_w, width, None);
            if source == EditorSource::Config {
                let (fallback, source) = form.fallback_editor();
                field(&mut rows, "Without it", &format!("{fallback} ({})", general::source_label(source)), label_w, width, None);
            }
            if let Some(i) = at(&Does::ClearEditor) {
                rows.push(action_row(i));
            }
            rows.push(Row::blank());
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
            rows.push(action_row(i));
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
    // The summary's own actions, after everything above.
    let first = actions
        .iter()
        .position(|a| matches!(a.does, Does::Run(_) | Does::Reload))
        .unwrap_or(actions.len());
    if first < actions.len() {
        rows.push(Row::blank());
        let title = match job.done().and_then(|s| s.project.as_ref()) {
            Some(Ok(_)) => "Open in editor",
            _ => "What you can do",
        };
        heading(&mut rows, title);
        for i in first..actions.len() {
            rows.push(action_row(i));
        }
    }
    rows
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
    heading(&mut rows, "Your terminal's colors");
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

/// The sixteen ANSI colors, regular over bright, painted as a session's
/// cells are ([`ansi_swatches`](super::terminal_view::ansi_swatches)): the
/// host terminal's own palette, what a session's colors look like here. A narrow page
/// puts each row's label above its swatches and narrows them.
fn swatch_rows(rows: &mut Vec<Row>, width: usize) {
    let colors = super::terminal_view::ansi_swatches();
    const NAMES: [&str; 8] = ["blk", "red", "grn", "yel", "blu", "mag", "cyn", "wht"];
    const LABEL: usize = 9;
    let label_style = Style::default().fg(palette().gray);
    let inline = width >= LABEL + 8 * 4;
    let room = if inline { width - LABEL } else { width };
    // Each swatch and the gap after it.
    let (swatch, gap) = match room / 8 {
        n if n >= 4 => (3, 1),
        n if n >= 2 => (n - 1, 1),
        _ => (1, 0),
    };
    let indent = if inline { " ".repeat(LABEL) } else { String::new() };
    // Swatches a row holds: all eight unless the page is narrower than that.
    let per_row = (room / (swatch + gap).max(1)).clamp(1, 8);
    for (label, colors) in [("Regular", &colors[..8]), ("Bright", &colors[8..])] {
        if !inline {
            rows.push(Row::text(Line::from(Span::styled(panel::truncate_right(label, width), label_style))));
        }
        for (i, chunk) in colors.chunks(per_row).enumerate() {
            let mut spans = Vec::new();
            if inline && i == 0 {
                spans.push(Span::styled(format!("{label:<LABEL$}"), label_style));
            }
            for (j, color) in chunk.iter().enumerate() {
                spans.push(Span::styled(" ".repeat(swatch), Style::default().bg(*color)));
                if j + 1 < chunk.len() {
                    spans.push(Span::raw(" ".repeat(gap)));
                }
            }
            rows.push(Row::text(Line::from(spans)));
        }
    }
    if swatch == 3 {
        let names: String = NAMES.iter().map(|n| format!("{n} ")).collect();
        rows.push(Row::text(Line::from(Span::styled(format!("{indent}{}", names.trim_end()), Style::default().fg(palette().muted)))));
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

fn heading(rows: &mut Vec<Row>, text: &str) {
    rows.push(Row::text(Line::from(Span::styled(
        text.to_string(),
        Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD),
    ))));
}

fn wrapped(rows: &mut Vec<Row>, text: &str, width: usize, style: Style) {
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
    }
}

/// The label column's width on a page `width` wide.
fn label_width(width: usize) -> usize {
    17.min(width / 2)
}

/// The focus marker and, when there is room for it beside the control, the
/// row's label in its column.
fn item_label(label: &str, selected: bool, label_w: usize, width: usize) -> String {
    let marker = if selected { "› " } else { "  " };
    if width >= 36 {
        let w = label_w.saturating_sub(2);
        format!("{marker}{:<w$}", panel::truncate_right(label, w.saturating_sub(1)))
    } else {
        marker.to_string()
    }
}

/// The theme's segmented control: `› Theme   Auto  Dark  Light`, the chosen
/// one filled. Returns the line and each segment's column, width and theme.
fn theme_line(chosen: ThemePreference, selected: bool, label_w: usize, width: usize) -> (Line<'static>, Vec<(u16, u16, ThemePreference)>) {
    let left = item_label("Theme", selected, label_w, width);
    let label_style = if selected {
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().gray)
    };
    let mut x = panel::display_width(&left);
    let mut spans = vec![Span::styled(left, label_style)];
    let mut segments = Vec::new();
    for theme in general::THEMES {
        let text = format!(" {} ", general::theme_label(theme));
        let w = text.chars().count();
        if x + w > width {
            break;
        }
        let style = if theme == chosen {
            selected_style(selected)
        } else {
            Style::default().fg(palette().gray)
        };
        segments.push((x as u16, w as u16, theme));
        spans.push(Span::styled(text, style));
        spans.push(Span::raw(" "));
        x += w + 1;
    }
    (Line::from(spans), segments)
}

/// The editor command's row: its value (or "Not set"), or, while it is
/// typed, the field with its caret, scrolled to keep the caret in view.
fn editor_line(form: &GeneralForm, selected: bool, label_w: usize, width: usize) -> Line<'static> {
    let left = item_label("Command", selected, label_w, width);
    let room = width.saturating_sub(panel::display_width(&left)).max(1);
    let label_style = if selected {
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(palette().gray)
    };
    let mut spans = vec![Span::styled(left, label_style)];
    match form.draft() {
        Some(field) => {
            let chars: Vec<char> = field.text().chars().collect();
            let caret = field.caret();
            let start = (caret + 1).saturating_sub(room);
            let shown: String = chars.iter().skip(start).take(room).collect();
            let at = caret - start;
            let before: String = shown.chars().take(at).collect();
            let under: String = shown.chars().nth(at).map_or(" ".to_string(), String::from);
            let after: String = shown.chars().skip(at + 1).collect();
            let fill = Style::default().fg(palette().white).bg(palette().selection);
            spans.push(Span::styled(before, fill));
            spans.push(Span::styled(under, fill.add_modifier(Modifier::REVERSED)));
            let used = at + 1 + after.chars().count();
            spans.push(Span::styled(format!("{after}{}", " ".repeat(room.saturating_sub(used))), fill));
        }
        None => match form.editor() {
            Some(command) => spans.push(Span::styled(
                panel::truncate_right(command, room),
                Style::default().fg(if selected { palette().white } else { palette().foreground }),
            )),
            None => spans.push(Span::styled(
                panel::truncate_right("Not set", room),
                Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
            )),
        },
    }
    Line::from(spans)
}

/// An action row: `› label   note`, the selection fill when focused.
fn action_line(action: &PageAction, selected: bool, width: usize) -> Line<'static> {
    let marker = if selected { "› " } else { "  " };
    let label = panel::truncate_right(&action.label, width.saturating_sub(2));
    let mut text = format!("{marker}{label}");
    if let Some(note) = &action.note {
        let room = width.saturating_sub(panel::display_width(&text) + 2);
        if room >= note.chars().count() {
            text.push_str("  ");
            text.push_str(note);
        }
    }
    if selected {
        let pad = width.saturating_sub(panel::display_width(&text));
        Line::from(Span::styled(format!("{text}{}", " ".repeat(pad)), selected_style(true)))
    } else {
        Line::from(Span::styled(text, Style::default().fg(palette().cyan)))
    }
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
fn selected_style(focused: bool) -> Style {
    let style = Style::default().bg(palette().selection);
    if focused {
        style.fg(palette().white).add_modifier(Modifier::BOLD)
    } else {
        style.fg(palette().gray)
    }
}

/// A one-row band of `text`, padded across `area`.
fn render_band(f: &mut Frame, area: Rect, text: &str, style: Style) {
    let w = area.width as usize;
    let text = panel::truncate_right(&format!(" {text}"), w);
    let pad = w.saturating_sub(panel::display_width(&text));
    f.render_widget(Paragraph::new(Span::styled(format!("{text}{}", " ".repeat(pad)), style)), area);
}

/// The key hints for the focused region, in the user's sidebar chords.
fn hints(region: Region, page: SettingsPage, typing: bool, keymaps: &Keymaps, style: DisplayStyle) -> String {
    if typing {
        return "type the command · ⏎ save · Esc put back".to_string();
    }
    let letter = |a: SidebarAction| {
        keymaps
            .sidebar
            .chords_for(a)
            .into_iter()
            .find(|c| matches!(c.code, Key::Char(_)))
            .map(|c| crate::keymap::format_chord_or_unbound(Some(c), style))
    };
    let moves = match (letter(SidebarAction::NavUp), letter(SidebarAction::NavDown)) {
        (Some(u), Some(d)) => format!("↑↓/{u}{d}"),
        _ => "↑↓".to_string(),
    };
    match region {
        Region::Sidebar => format!("{moves} move · ⏎ open · Tab page · Esc back"),
        Region::Page if page == SettingsPage::General => {
            format!("{moves} choose · ←→ theme · ⏎ run · r reload · Tab sidebar · Esc back")
        }
        // Nothing on the page acts: the arrows scroll it.
        Region::Page => format!("{moves} scroll · Tab sidebar · Esc back"),
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
                Stop::Page(SettingsPage::Terminal),
                Stop::Switcher
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
        assert_eq!(s.stop, Stop::Page(SettingsPage::Terminal));
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Switcher);
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Switcher, "stops at the bottom");
        s.handle_key(key(KeyCode::Char('k')), &km);
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
        s.handle_key(key(KeyCode::Down), &km);
        s.handle_key(key(KeyCode::Down), &km);
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
        assert_eq!(s.stop, Stop::Page(SettingsPage::Terminal), "the user's nav_down moves the selection");
    }

    #[test]
    fn page_actions_follow_the_loaded_summary() {
        let mut s = SettingsShell::default();
        assert!(s.actions().is_empty(), "nothing to do while loading");
        let t = s.begin_load(Some("proj"), false).expect("a first load");
        assert!(s.begin_load(Some("proj"), false).is_none(), "already loading for proj");
        assert!(s.loaded(&t, Ok(fixture_summary(Some("proj")))));
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, ["Theme", "Command", "Edit config.yaml", "Edit Project Settings", "Edit Zen Mode"]);
        let km = keymaps();
        s.set_region(Region::Page);
        for _ in 0..4 {
            s.handle_key(key(KeyCode::Char('j')), &km);
        }
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &km),
            SettingsAction::Run(Effect::OpenEditor {
                target: EditTarget::ZenMode
            })
        );
        assert_eq!(s.handle_key(key(KeyCode::Char('r')), &km), SettingsAction::Reload);
        // No project: switch or add one.
        let t = s.begin_load(None, true).unwrap();
        s.loaded(&t, Ok(fixture_summary(None)));
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, ["Theme", "Command", "Edit config.yaml", "Switch project…", "Add project…"]);
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
        let (ticket, work) = s.begin_save(Some("proj"), &writer()).expect("a dirty page saves");
        assert_eq!(s.saved(&ticket, work()), SettingsAction::Stay);
        assert_eq!(s.save_error.as_deref(), Some("disk full"));
        s.test_edits = Some(Ok(()));
        s.request(Leave::Project("beta".into()));
        s.handle_key(key(KeyCode::Char('s')), &km);
        let (ticket, work) = s.begin_save(Some("proj"), &writer()).unwrap();
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
        let text = screen(&mut s, 100, 40);
        for want in ["No project is open.", "Switch project…", "Add project…", "This computer"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        let mut broken = fixture_summary(Some("proj"));
        broken.project = Some(Err("projects/proj.yaml: bad indent".into()));
        let t = s.begin_load(Some("proj"), true).unwrap();
        s.loaded(&t, Ok(broken));
        let text = screen(&mut s, 100, 40);
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
            editor: editor.map(str::to_string),
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

    #[test]
    fn the_editor_takes_every_key_while_typed_and_saves_only_on_enter() {
        let mut s = on_general(fixture_summary(Some("proj")));
        let km = keymaps();
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Stay, "Enter starts typing");
        assert!(s.typing());
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
        assert!(s.paste("code --wait"), "a paste types into the field");
        assert_eq!(
            s.handle_key(key(KeyCode::Enter), &km),
            SettingsAction::Write(Change::Editor(Some("code --wait".into())))
        );
        s.config_written(Ok(written(ThemePreference::Auto, Some("code --wait"))));
        let text = screen(&mut s, 100, 40);
        for want in ["code --wait", "Shelbi runs      code --wait (your setting)", "Without it       vim (the default)", "Clear the editor setting"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        // Clearing restores the fallback.
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.handle_key(key(KeyCode::Enter), &km), SettingsAction::Write(Change::Editor(None)));
        s.config_written(Ok(written(ThemePreference::Auto, None)));
        let text = screen(&mut s, 100, 40);
        assert!(text.contains("Not set") && text.contains("vim (the default)"), "{text}");
        assert!(!text.contains("Clear the editor setting"), "{text}");
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
        s.paste("hx");
        // Back from the sidebar (a click, or Ctrl+H then Enter).
        s.set_region(Region::Sidebar);
        assert_eq!(s.request(Leave::Back), SettingsAction::Stay);
        assert!(s.prompting(), "unsaved: the prompt asks");
        assert_eq!(s.handle_key(key(KeyCode::Char('s')), &km), SettingsAction::Save);
        let (ticket, work) = s.begin_save(Some("proj"), &writer()).expect("the typed command saves");
        let result = work();
        assert_eq!(result.as_ref().unwrap().as_ref().unwrap().editor.as_deref(), Some("hx"));
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
        assert_eq!(labels, ["Edit config.yaml", "Edit Project Settings", "Edit Zen Mode"]);
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
            });
            let mut s = on_general(summary);
            let km = keymaps();
            s.handle_key(key(KeyCode::Down), &km);
            s.handle_key(key(KeyCode::Enter), &km);
            let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
            term.draw(|f| {
                let chrome = Chrome { project_label: "proj", status: None, keymaps: &km, style: DisplayStyle::Mac };
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
        for want in ["Terminal", "owns how sessions look", "window opacity and blur", "never changes your terminal's colors", "Regular", "Bright", "Preview", "Claude Code", "They don't apply here"] {
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
        assert_eq!(
            backgrounds("Regular"),
            [Color::Black, Color::Red, Color::Green, Color::Yellow, Color::Blue, Color::Magenta, Color::Cyan, Color::Gray]
        );
        assert_eq!(
            backgrounds("Bright"),
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
                };
                s.render(f, side, main, &chrome);
            })
            .unwrap();
        }
    }
}
