//! Settings in the TUI (`tui-settings-shell`), after the desktop app's
//! (`desktop/src/views/settings_nav.rs`): the settings sidebar stands in the
//! nav sidebar's column and the shown page fills the main area, over the
//! shared `shelbi_app::settings` model.
//!
//! The sidebar, top to bottom: the panels' square back button with the
//! Settings title beside it, the Global section's page rows (each with its
//! number key), then a divider and the project switcher (the palette's
//! Projects column, the TUI's switcher) over the project's page rows. Only
//! the pages the TUI can show are listed ([`OFFERED`]): General for now, a
//! summary of where the settings live with the Open in editor paths; the
//! follow-up pages join the set as they land.
//!
//! Keys, all terminal-safe (no Command chords) and never forwarded to a
//! session while Settings shows:
//!
//! - the sidebar keymap's up / down / activate (`k` / `j` / Enter by
//!   default) and the arrows move through the focused list and open its row;
//! - Tab / Shift-Tab (and the global Ctrl+H / Ctrl+L) move focus between
//!   the sidebar and the page;
//! - a page's number (`1`) opens it from either;
//! - Esc closes the innermost thing first: the unsaved-edits prompt, then
//!   Settings itself (back to the view, selection and focus it was opened
//!   over; no session stops).
//!
//! Mouse: a click opens a row, runs a page action or answers the prompt;
//! the wheel moves the sidebar selection or scrolls the page.
//!
//! This module holds the state and the drawing; the shell routes events in
//! and carries out the [`SettingsAction`]s that need it (leaving, switching
//! project, running an effect, starting a load or a save off the UI thread).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};
use shelbi_app::settings::summary::{self, FileState, GeneralSummary, ProjectSummary};
use shelbi_app::settings::{
    Header, Job, JobState, LeaveGuard, PageSet, SettingsNav, SettingsPage, Ticket,
};
use shelbi_app::{EditTarget, Effect};
use shelbi_state::keymap::{DisplayStyle, Key, Keymaps, SidebarAction};

use crate::panel::{self, PAD};
use crate::theme::palette;

/// The pages the TUI offers. A follow-up page joins here once it draws.
pub(crate) const OFFERED: PageSet = PageSet::NONE.with(SettingsPage::General);

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
}

/// What a page's action row does.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Does {
    Run(Effect),
    Reload,
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
    PromptSave,
    PromptDiscard,
    PromptCancel,
}

/// A blocking save the shell runs off the UI thread.
pub(crate) type SaveWork = Box<dyn FnOnce() -> Result<(), String> + Send>;

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
        false
    }

    /// The shown page's save, if it has anything to save.
    pub(crate) fn begin_save(&mut self, project: Option<&str>) -> Option<(Ticket, SaveWork)> {
        #[cfg(test)]
        if let Some(result) = self.test_edits.clone() {
            let ticket = self.save.begin(project);
            return Some((ticket, Box::new(move || result)));
        }
        let _ = project;
        self.guard.forget_save();
        None
    }

    /// A save landed. Returns where to go when the prompt's Save started it
    /// and it worked.
    pub(crate) fn saved(&mut self, ticket: &Ticket, result: Result<(), String>) -> SettingsAction {
        let ok = result.is_ok();
        if !self.save.complete(ticket, result.clone()) {
            self.dropped += 1;
            return SettingsAction::Stay;
        }
        match result {
            Ok(()) => {
                self.save_error = None;
                #[cfg(test)]
                {
                    self.test_edits = None;
                }
            }
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
        self.save_error = None;
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

    /// The shown page's action rows.
    fn actions(&self) -> Vec<PageAction> {
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
        }
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
                let n = self.actions().len();
                if up {
                    self.action = self.action.saturating_sub(1);
                } else if down {
                    self.action = (self.action + 1).min(n.saturating_sub(1));
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
                    Some(Hit::Action(i)) => self.run_action(i),
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
        let hints = hints(self.region, chrome.keymaps, chrome.style);
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
                Some(a) => action_line(&actions[a], self.region == Region::Page && a == self.action, rect.width as usize),
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
            SettingsPage::General => general_rows(&self.general, self.save_error.as_deref(), width, actions),
            SettingsPage::Keybindings
            | SettingsPage::Terminal
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

/// The General page: where this computer's settings and the shown
/// project's live, then the Open in editor paths.
fn general_rows(job: &Job<GeneralSummary>, save_error: Option<&str>, width: usize, actions: &[PageAction]) -> Vec<Row> {
    let mut rows = vec![Row::text(Line::from(Span::styled(
        "General",
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
    )))];
    wrapped(&mut rows, "Where Shelbi keeps your settings, and the files behind them.", width, Style::default().fg(palette().muted));
    if let Some(e) = save_error {
        wrapped(&mut rows, &format!("Couldn't save: {e}"), width, Style::default().fg(palette().red));
    }
    rows.push(Row::blank());
    let label_w = 17.min(width / 2);
    match job.state() {
        JobState::Idle | JobState::Running => {
            rows.push(Row::text(Line::from(Span::styled(
                "Loading…",
                Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
            ))));
        }
        JobState::Failed(e) => {
            wrapped(&mut rows, &format!("Couldn't read the settings: {e}"), width, Style::default().fg(palette().red));
            wrapped(&mut rows, "Press r, or choose Try again.", width, Style::default().fg(palette().muted));
        }
        JobState::Done(summary) => {
            heading(&mut rows, "This computer");
            let g = &summary.global;
            field(&mut rows, "Settings folder", &summary::display_path(&g.home), label_w, width, None);
            let (config, color) = match &g.config {
                FileState::Present(_) => (format!("Theme: {}", summary::theme_label(g.theme)), None),
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
            field(&mut rows, "Editor", &g.editor, label_w, width, None);
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
    }
    if !actions.is_empty() {
        rows.push(Row::blank());
        let title = match job.done().and_then(|s| s.project.as_ref()) {
            Some(Ok(_)) => "Open in editor",
            _ => "What you can do",
        };
        heading(&mut rows, title);
        for i in 0..actions.len() {
            rows.push(Row { line: Line::raw(""), action: Some(i) });
        }
    }
    rows
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
fn hints(region: Region, keymaps: &Keymaps, style: DisplayStyle) -> String {
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
        Region::Page => format!("{moves} choose · ⏎ run · r reload · Tab sidebar · Esc back"),
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
            theme: shelbi_state::ThemePreference::Auto,
            editor: "vim".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
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
            [Stop::Back, Stop::Page(SettingsPage::General), Stop::Switcher]
        );
        assert_eq!(s.page(), SettingsPage::General);
    }

    #[test]
    fn arrows_and_jk_walk_the_stops_and_stop_at_the_ends() {
        let mut s = SettingsShell::default();
        s.open();
        let km = keymaps();
        s.handle_key(key(KeyCode::Char('j')), &km);
        assert_eq!(s.stop, Stop::Switcher);
        s.handle_key(key(KeyCode::Down), &km);
        assert_eq!(s.stop, Stop::Switcher, "stops at the bottom");
        s.handle_key(key(KeyCode::Char('k')), &km);
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
        assert_eq!(s.stop, Stop::Switcher, "the user's nav_down moves the selection");
    }

    #[test]
    fn page_actions_follow_the_loaded_summary() {
        let mut s = SettingsShell::default();
        assert!(s.actions().is_empty(), "nothing to do while loading");
        let t = s.begin_load(Some("proj"), false).expect("a first load");
        assert!(s.begin_load(Some("proj"), false).is_none(), "already loading for proj");
        assert!(s.loaded(&t, Ok(fixture_summary(Some("proj")))));
        let labels: Vec<String> = s.actions().into_iter().map(|a| a.label).collect();
        assert_eq!(labels, ["Edit Project Settings", "Edit Zen Mode"]);
        let km = keymaps();
        s.set_region(Region::Page);
        s.handle_key(key(KeyCode::Char('j')), &km);
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
        assert_eq!(labels, ["Switch project…", "Add project…"]);
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
        let (ticket, work) = s.begin_save(Some("proj")).expect("a dirty page saves");
        assert_eq!(s.saved(&ticket, work()), SettingsAction::Stay);
        assert_eq!(s.save_error.as_deref(), Some("disk full"));
        s.test_edits = Some(Ok(()));
        s.request(Leave::Project("beta".into()));
        s.handle_key(key(KeyCode::Char('s')), &km);
        let (ticket, work) = s.begin_save(Some("proj")).unwrap();
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
        let text = screen(&mut s, 100, 30);
        for want in ["No project is open.", "Switch project…", "Add project…", "This computer"] {
            assert!(text.contains(want), "{want:?}:\n{text}");
        }
        let mut broken = fixture_summary(Some("proj"));
        broken.project = Some(Err("projects/proj.yaml: bad indent".into()));
        let t = s.begin_load(Some("proj"), true).unwrap();
        s.loaded(&t, Ok(broken));
        let text = screen(&mut s, 100, 30);
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
