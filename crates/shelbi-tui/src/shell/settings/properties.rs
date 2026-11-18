//! Settings ▸ Properties (`settings-properties-page`): the project YAML's own
//! settings, over the shared
//! [`PropertiesForm`](shelbi_app::settings::properties::PropertiesForm) the
//! desktop app's page draws too.
//!
//! ```text
//!  Properties
//!  Settings for this project, stored in ~/.shelbi/projects/shelbi.yaml.
//!
//!  Name
//!  ▐ Shelbi                              ▌
//!  Repository
//!  ▐ git@github.com:jlong/shelbi.git     ▌
//!  Default branch
//!  ▐ main                                ▌
//!  Default workflow
//!  ▐ app                               ▾ ▌
//!  ▸ Advanced
//!
//!  Issue tracker
//!  (•) Local board
//!  ( ) GitHub Issues
//!  ( ) Linear
//!  …the chosen board's fields (the GitHub repo, or `crate::linear_rows`)…
//!  [ Cancel ]  [ Connect ]
//! ```
//!
//! ↑/↓ move between the rows. Enter on a text field starts typing in it;
//! Enter saves what was typed and Esc puts the field back. The Default
//! workflow changes with ←/→ (Enter steps it too) and saves at once.
//! Enter (or ←/→) on Advanced opens or closes the intervals. A value the
//! form refuses keeps its text, shows its error in red under the field and
//! isn't written.
//!
//! The Issue tracker section is the shared
//! [`TrackerForm`](shelbi_app::settings::tracker::TrackerForm): Enter or
//! ←/→ on a board picks it; Enter on the repo or the API key field types in
//! it (a pasted key shows as dots, Enter verifies and stores it); the Linear
//! pickers step with ←/→ (Enter steps too), `r` reads a list that failed
//! again; nothing is written until Connect (or Save). Its Linear reads and
//! the connection run off the UI thread ([`PropertiesPage::take_tracker_work`]).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use shelbi_app::settings::general::LineInput;
use shelbi_app::linear_setup::{Filter, LinearAnswer, LinearRequest};
use shelbi_app::new_project::{self as np, Tracker};
use shelbi_app::settings::properties::{
    PropertiesForm, Text, ADVANCED_LABEL, DEFAULT_WORKFLOW_HELP, DEFAULT_WORKFLOW_LABEL, SECONDS, TEXTS, TITLE,
};
use shelbi_app::settings::tracker::{self as tr, TrackerForm};
use shelbi_app::tracker_connect::{ConnectError, ConnectTracker, Connected};
use shelbi_app::settings::{Job, Ticket};
use shelbi_state::workflow_config::WorkflowConfig;

use super::workflows::Nav;
use super::{field_rows, picker_rows, wrapped, Hit, Row};
use crate::linear_rows::{self, Part};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// A row the page's keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Text(Text),
    Workflow,
    Advanced,
    Tracker(TrackerItem),
}

/// A stop in the Issue tracker section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackerItem {
    Choice(Tracker),
    Repo,
    Key,
    ChangeKey,
    Team,
    Filter(Filter),
    Cancel,
    Force,
    Primary,
}

/// Work the Issue tracker section needs run off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrackerWork {
    Linear(LinearRequest),
    Connect(ConnectTracker),
}

/// Its outcome, handed back by [`PropertiesPage::tracker_done`].
#[derive(Debug, Clone)]
pub(crate) enum TrackerDone {
    Linear(LinearAnswer),
    Connected(Result<Connected, ConnectError>),
}

impl TrackerWork {
    /// Run it with `service` (blocking).
    pub(crate) fn run(self, service: &tr::TrackerService) -> TrackerDone {
        match self {
            TrackerWork::Linear(req) => TrackerDone::Linear((service.linear)(req)),
            TrackerWork::Connect(req) => TrackerDone::Connected((service.connect)(req)),
        }
    }
}

/// The Properties page: the shared form, its read and save, the row the
/// keys are on and the field being typed.
#[derive(Debug, Default)]
pub(crate) struct PropertiesPage {
    form: PropertiesForm,
    load: Job<()>,
    save: Job<()>,
    cursor: usize,
    /// The field being typed, and what's typed.
    editing: Option<(Text, LineInput)>,
    /// The Issue tracker section.
    tracker: TrackerForm,
    /// The tracker field being typed (the repo, the API key).
    tracker_typing: Option<(TrackerItem, LineInput)>,
    /// Work for the shell to run.
    tracker_work: Vec<TrackerWork>,
}

impl PropertiesPage {
    #[cfg(test)]
    pub(crate) fn form(&self) -> &PropertiesForm {
        &self.form
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub(crate) fn tracker(&self) -> &TrackerForm {
        &self.tracker
    }

    /// The rows the keys stop on, top to bottom.
    pub(crate) fn items(&self) -> Vec<Item> {
        let mut items = vec![
            Item::Text(Text::Label),
            Item::Text(Text::Repository),
            Item::Text(Text::DefaultBranch),
            Item::Workflow,
            Item::Advanced,
        ];
        if self.form.advanced_open() {
            items.extend(TEXTS.into_iter().filter(|t| t.advanced()).map(Item::Text));
        }
        items.extend(self.tracker_items().into_iter().map(Item::Tracker));
        items
    }

    /// The Issue tracker section's stops.
    fn tracker_items(&self) -> Vec<TrackerItem> {
        let t = &self.tracker;
        if !t.loaded() {
            return Vec::new();
        }
        let mut items: Vec<TrackerItem> = tr::CHOICES.into_iter().map(TrackerItem::Choice).collect();
        match t.tracker() {
            Tracker::Github => items.push(TrackerItem::Repo),
            Tracker::Linear => {
                let linear = t.linear();
                if linear.needs_key() {
                    items.push(TrackerItem::Key);
                } else if matches!(linear.key_state(), shelbi_app::linear_setup::KeyState::Valid { .. }) {
                    items.push(TrackerItem::ChangeKey);
                }
                items.push(TrackerItem::Team);
                if linear.team_option().is_some() {
                    items.extend(Filter::ALL.map(TrackerItem::Filter));
                }
            }
            Tracker::FileSystem => {}
        }
        items.push(TrackerItem::Cancel);
        if t.offers_force() {
            items.push(TrackerItem::Force);
        }
        items.push(TrackerItem::Primary);
        items
    }

    // --- the tracker's work ------------------------------------------------------

    /// Queue the Linear reads the section has due.
    fn queue_linear(&mut self) {
        while let Some(req) = self.tracker.linear_due() {
            self.tracker_work.push(TrackerWork::Linear(req));
        }
    }

    /// Whether the section has work for the shell.
    pub(crate) fn tracker_work_due(&self) -> bool {
        !self.tracker_work.is_empty()
    }

    /// The section's work to run now, for the project shown.
    pub(crate) fn take_tracker_work(&mut self) -> Option<(String, Vec<TrackerWork>)> {
        let project = self.load.project()?.to_string();
        if self.tracker_work.is_empty() {
            return None;
        }
        Some((project, std::mem::take(&mut self.tracker_work)))
    }

    /// A piece of work landed for `project`. Returns whether a connection
    /// was written (the page reads the project again).
    pub(crate) fn tracker_done(&mut self, project: &str, done: TrackerDone) -> bool {
        if self.load.project() != Some(project) {
            return false;
        }
        let wrote = match done {
            TrackerDone::Linear(answer) => {
                self.tracker.linear_answer(answer);
                false
            }
            TrackerDone::Connected(result) => {
                let ok = result.is_ok();
                self.tracker.connected(result);
                ok
            }
        };
        self.queue_linear();
        self.clamp_cursor();
        wrote
    }

    fn clamp_cursor(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    // --- loading and saving ----------------------------------------------------

    /// Start a read for `project` (every visit, to show a hand edit): a
    /// ticket the shell's reader carries, or `None` with no project open.
    pub(crate) fn begin_load(&mut self, project: Option<&str>) -> Option<Ticket> {
        let Some(project) = project else {
            self.project_changed();
            return None;
        };
        if self.load.project() != Some(project) {
            self.project_changed();
        }
        self.form.clear_status();
        Some(self.load.begin(Some(project)))
    }

    /// The read landed. Returns whether it was taken (a stale one is
    /// dropped).
    pub(crate) fn loaded(&mut self, ticket: &Ticket, read: Result<WorkflowConfig, String>) -> bool {
        if !self.load.complete(ticket, Ok(())) {
            return false;
        }
        self.form.load(read);
        if let Some(cfg) = self.form.issue_tracker_config().cloned() {
            self.tracker.load(&cfg);
            self.queue_linear();
        }
        true
    }

    /// Another project is shown: everything goes.
    pub(crate) fn project_changed(&mut self) {
        self.form.reset();
        self.load.reset();
        self.save.reset();
        self.cursor = 0;
        self.editing = None;
        self.tracker.reset();
        self.tracker_typing = None;
        self.tracker_work.clear();
    }

    /// Start saving `config` (from the form): the ticket and project for
    /// the shell's writer.
    fn begin_save(&mut self, config: Option<WorkflowConfig>) -> Option<(Ticket, String, WorkflowConfig)> {
        let config = config?;
        let project = self.load.project()?.to_string();
        let ticket = self.save.begin(Some(&project));
        Some((ticket, project, config))
    }

    /// A save landed. Returns `Some(taken)`, and the next save when one was
    /// queued behind it; `None` for a stale ticket.
    pub(crate) fn saved(
        &mut self,
        ticket: &Ticket,
        result: Result<WorkflowConfig, String>,
    ) -> Option<Option<(Ticket, String, WorkflowConfig)>> {
        if !self.save.complete(ticket, Ok(())) {
            return None;
        }
        let next = self.form.saved_result(result);
        Some(self.begin_save(next))
    }

    // --- typing ----------------------------------------------------------------

    /// Whether a field is being typed (it holds every key).
    pub(crate) fn typing(&self) -> bool {
        self.editing.is_some() || self.tracker_typing.is_some()
    }

    /// Whether what's typed differs from the field (leaving asks first).
    pub(crate) fn dirty(&self) -> bool {
        self.editing.as_ref().is_some_and(|(t, input)| input.text() != self.form.text(*t))
    }

    /// Start typing in `t`.
    fn begin_typing(&mut self, t: Text) {
        if !self.form.editable() {
            return;
        }
        self.editing = Some((t, LineInput::new(self.form.text(t))));
    }

    /// Save what's typed (Enter, and the leave prompt's Save).
    pub(crate) fn commit_typing(&mut self) -> Option<(Ticket, String, WorkflowConfig)> {
        let (t, input) = self.editing.take()?;
        self.form.set_text(t, &input.text());
        let next = self.form.commit();
        self.begin_save(next)
    }

    /// The leave prompt's Save: what's typed, as the config its save writes
    /// (the shell runs it as the prompt's save, [`Self::prompt_saved`]).
    pub(crate) fn commit_for_prompt(&mut self) -> Option<WorkflowConfig> {
        let (t, input) = self.editing.take()?;
        self.form.set_text(t, &input.text());
        self.form.commit()
    }

    /// The prompt's save landed.
    pub(crate) fn prompt_saved(&mut self, result: Result<(), String>) {
        self.form.forget_save(result);
    }

    /// Drop what's typed (Esc, and the leave prompt's Discard).
    pub(crate) fn cancel_typing(&mut self) {
        if let Some((t, _)) = self.editing.take() {
            self.form.revert(t);
        }
        if let Some((TrackerItem::Key, _)) = self.tracker_typing.take() {
            self.tracker.cancel_change_key();
        }
    }

    /// Paste into the field being typed (an API key is usually pasted).
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        if let Some((_, input)) = self.tracker_typing.as_mut() {
            input.paste(text.trim());
            return true;
        }
        match self.editing.as_mut() {
            Some((_, input)) => {
                input.paste(text);
                true
            }
            None => false,
        }
    }

    /// Enter in a tracker field: the repo takes the text; the key is
    /// verified and stored.
    fn commit_tracker_typing(&mut self) {
        let Some((item, input)) = self.tracker_typing.take() else { return };
        match item {
            TrackerItem::Repo => self.tracker.set_repo(&input.text()),
            TrackerItem::Key => {
                self.tracker.set_key_input(&input.text());
                self.tracker.submit_key();
            }
            _ => {}
        }
        self.queue_linear();
        self.clamp_cursor();
    }

    /// A key while a tracker field is typed.
    fn tracker_typing_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some((_, input)) = self.tracker_typing.as_mut() else { return };
        match k.code {
            KeyCode::Enter => self.commit_tracker_typing(),
            KeyCode::Esc => self.cancel_typing(),
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Left => input.left(),
            KeyCode::Right => input.right(),
            KeyCode::Home => input.home(),
            KeyCode::End => input.end(),
            KeyCode::Char('u') if ctrl => input.clear(),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => input.insert(c),
            _ => {}
        }
    }

    /// Run tracker stop `item` (Enter, a click).
    fn activate_tracker(&mut self, item: TrackerItem) {
        let t = &mut self.tracker;
        match item {
            TrackerItem::Choice(choice) => t.select(choice),
            TrackerItem::Repo => {
                if t.editable() {
                    self.tracker_typing = Some((item, LineInput::new(t.issues().repo())));
                }
            }
            TrackerItem::Key => {
                if t.editable() && !t.linear().logging_in() {
                    self.tracker_typing = Some((item, LineInput::new(t.linear().key_input())));
                }
            }
            TrackerItem::ChangeKey => {
                t.change_key();
                if let Some(at) = self.items().iter().position(|i| *i == Item::Tracker(TrackerItem::Key)) {
                    self.cursor = at;
                    self.tracker_typing = Some((TrackerItem::Key, LineInput::new("")));
                }
            }
            TrackerItem::Team => t.step_team(true),
            TrackerItem::Filter(f) => t.step_filter(f, true),
            TrackerItem::Cancel => t.cancel(),
            TrackerItem::Force => {
                if let Some(req) = t.submit(true) {
                    self.tracker_work.push(TrackerWork::Connect(req));
                }
            }
            TrackerItem::Primary => {
                if let Some(req) = t.submit(false) {
                    self.tracker_work.push(TrackerWork::Connect(req));
                }
            }
        }
        self.queue_linear();
        self.clamp_cursor();
    }

    /// ←/→ on tracker stop `item`.
    fn step_tracker(&mut self, item: TrackerItem, forward: bool) {
        match item {
            TrackerItem::Choice(_) => self.tracker.step(forward),
            TrackerItem::Team => self.tracker.step_team(forward),
            TrackerItem::Filter(f) => self.tracker.step_filter(f, forward),
            _ => return,
        }
        // The cursor follows the board it picked.
        if let TrackerItem::Choice(_) = item {
            let now = Item::Tracker(TrackerItem::Choice(self.tracker.tracker()));
            if let Some(at) = self.items().iter().position(|i| *i == now) {
                self.cursor = at;
            }
        }
        self.queue_linear();
        self.clamp_cursor();
    }

    /// A key while a field is typed.
    fn typing_key(&mut self, k: KeyEvent) -> Option<(Ticket, String, WorkflowConfig)> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let (_, input) = self.editing.as_mut()?;
        match k.code {
            KeyCode::Enter => return self.commit_typing(),
            KeyCode::Esc => self.cancel_typing(),
            KeyCode::Backspace => input.backspace(),
            KeyCode::Delete => input.delete(),
            KeyCode::Left => input.left(),
            KeyCode::Right => input.right(),
            KeyCode::Home => input.home(),
            KeyCode::End => input.end(),
            KeyCode::Char('a') if ctrl => input.home(),
            KeyCode::Char('e') if ctrl => input.end(),
            KeyCode::Char('u') if ctrl => input.clear(),
            KeyCode::Char(c) if !ctrl && !k.modifiers.contains(KeyModifiers::ALT) => input.insert(c),
            _ => {}
        }
        None
    }

    // --- keys ------------------------------------------------------------------

    /// Move the selection a row down or up.
    pub(crate) fn step(&mut self, down: bool) {
        let n = self.items().len();
        self.cursor = if down { (self.cursor + 1).min(n.saturating_sub(1)) } else { self.cursor.saturating_sub(1) };
    }

    /// A key on the page: `Some` for one it took (and the save it started,
    /// if any), `None` for one it leaves to Settings (Esc, Tab, a digit).
    pub(crate) fn key(&mut self, k: KeyEvent, nav: Nav) -> Option<Option<(Ticket, String, WorkflowConfig)>> {
        if self.tracker_typing.is_some() {
            self.tracker_typing_key(k);
            return Some(None);
        }
        if self.typing() {
            return Some(self.typing_key(k));
        }
        if let (KeyCode::Char('r'), Some(Item::Tracker(TrackerItem::Team | TrackerItem::Filter(_)))) =
            (k.code, self.items().get(self.cursor).copied())
        {
            if k.modifiers.is_empty() {
                self.tracker.retry_linear();
                self.queue_linear();
                return Some(None);
            }
        }
        if nav.up || nav.down {
            self.step(nav.down);
            return Some(None);
        }
        if nav.activate {
            return Some(self.activate(self.cursor));
        }
        let step = match k.code {
            KeyCode::Left => -1,
            KeyCode::Right => 1,
            _ => return None,
        };
        let next = match self.items().get(self.cursor).copied() {
            Some(Item::Workflow) => self.form.step_workflow(step),
            Some(Item::Advanced) => {
                self.form.toggle_advanced();
                None
            }
            Some(Item::Tracker(item)) => {
                self.step_tracker(item, step > 0);
                None
            }
            _ => return Some(None),
        };
        Some(self.begin_save(next))
    }

    /// Run row `i` (Enter, a click).
    pub(crate) fn activate(&mut self, i: usize) -> Option<(Ticket, String, WorkflowConfig)> {
        let item = self.items().get(i).copied()?;
        self.cursor = i;
        match item {
            Item::Text(t) => self.begin_typing(t),
            Item::Workflow => {
                let next = self.form.step_workflow(1);
                return self.begin_save(next);
            }
            Item::Advanced => {
                self.form.toggle_advanced();
                let n = self.items().len();
                self.cursor = self.cursor.min(n - 1);
            }
            Item::Tracker(item) => self.activate_tracker(item),
        }
        None
    }

    // --- drawing ---------------------------------------------------------------

    /// The page's rows at `width`, each focusable row's lines tagged with its
    /// item's index.
    pub(crate) fn rows(&self, width: usize, focused: bool, has_project: bool) -> Vec<Row> {
        let muted = Style::default().fg(palette().muted);
        let mut rows = vec![Row::text(Line::from(Span::styled(
            TITLE,
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        )))];
        if !has_project {
            wrapped(&mut rows, "Open a project to edit its properties.", width, muted);
            return rows;
        }
        wrapped(&mut rows, &self.form.subtitle(), width, muted);
        if let Some(message) = self.form.status_message() {
            let color = match self.form.status() {
                shelbi_app::settings::orchestrator::SaveStatus::Failed(_) => palette().red,
                shelbi_app::settings::orchestrator::SaveStatus::Saved => palette().green,
                _ => palette().yellow,
            };
            wrapped(&mut rows, &message, width, Style::default().fg(color));
        }
        rows.push(Row::blank());
        if let Some(e) = self.form.unreadable() {
            let red = Style::default().fg(palette().red);
            wrapped(&mut rows, &format!("The project's settings don't read: {e}"), width, red);
            wrapped(&mut rows, "Press r to read them again.", width, muted);
            return rows;
        }
        if !self.form.editable() {
            rows.push(Row::text(Line::from(Span::styled(
                "Loading…",
                Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
            ))));
            return rows;
        }
        for (i, item) in self.items().into_iter().enumerate() {
            let on = focused && self.cursor == i;
            match item {
                Item::Text(t) => self.text_rows(&mut rows, t, on, i, width),
                Item::Workflow => {
                    picker_rows(&mut rows, DEFAULT_WORKFLOW_LABEL, self.form.default_workflow(), on, false, Some(i), width);
                    wrapped(&mut rows, DEFAULT_WORKFLOW_HELP, width, muted);
                }
                // The section draws all its stops at its first.
                Item::Tracker(TrackerItem::Choice(Tracker::FileSystem)) => {
                    self.tracker_rows(&mut rows, focused, i, width);
                    continue;
                }
                Item::Tracker(_) => continue,
                Item::Advanced => {
                    let open = self.form.advanced_open();
                    let mut style = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
                    if on {
                        style = style.add_modifier(Modifier::UNDERLINED);
                    }
                    rows.push(Row::blank());
                    rows.push(Row {
                        action: Some(i),
                        ..Row::text(Line::from(vec![
                            Span::styled(if open { "▾ " } else { "▸ " }, Style::default().fg(palette().cyan)),
                            Span::styled(ADVANCED_LABEL, style),
                        ]))
                    });
                }
            }
            rows.push(Row::blank());
        }
        rows
    }

    /// The Issue tracker section's rows: its heading, the boards, the chosen
    /// board's fields, what's wrong or what was written, and the buttons.
    /// Its stops start at item `first`.
    fn tracker_rows(&self, rows: &mut Vec<Row>, focused: bool, first: usize, width: usize) {
        let t = &self.tracker;
        let items = self.items();
        let at = |item: TrackerItem| items.iter().position(|i| *i == Item::Tracker(item));
        let on = |item: TrackerItem| focused && at(item) == Some(self.cursor);
        let muted = Style::default().fg(palette().muted);
        let _ = first;
        rows.push(Row::blank());
        rows.push(Row::text(Line::from(Span::styled(
            tr::TITLE,
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        ))));
        wrapped(rows, tr::HELP, width, muted);
        for choice in tr::CHOICES {
            let item = TrackerItem::Choice(choice);
            let picked = t.tracker() == choice;
            let mut style = Style::default().fg(if picked { palette().white } else { palette().gray });
            if on(item) {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            rows.push(Row {
                action: at(item),
                focus: on(item),
                ..Row::text(Line::from(vec![
                    Span::styled(if picked { "(•) " } else { "( ) " }, Style::default().fg(if picked { palette().cyan } else { palette().gray })),
                    Span::styled(tr::choice_title(choice), style),
                ]))
            });
        }
        rows.push(Row::blank());
        match t.tracker() {
            Tracker::FileSystem => wrapped(rows, np::FILE_SYSTEM_BODY, width, muted),
            Tracker::Github => {
                let typed = match &self.tracker_typing {
                    Some((TrackerItem::Repo, input)) => Some(input),
                    _ => None,
                };
                let shown = typed.map(LineInput::text).unwrap_or_else(|| t.issues().repo().to_string());
                let mut field = Textbox::new(&shown).placeholder("owner/repo").focused(on(TrackerItem::Repo));
                if let Some(input) = typed {
                    field = field.caret(Some(input.caret()));
                }
                field_rows(rows, np::GITHUB_REPO_LABEL, field, on(TrackerItem::Repo), !t.editable(), at(TrackerItem::Repo), width);
                wrapped(rows, np::GITHUB_BODY, width, muted);
            }
            Tracker::Linear => {
                let focus = match items.get(self.cursor) {
                    Some(Item::Tracker(TrackerItem::Key)) if focused => Some(Part::Key),
                    Some(Item::Tracker(TrackerItem::ChangeKey)) if focused => Some(Part::ChangeKey),
                    Some(Item::Tracker(TrackerItem::Team)) if focused => Some(Part::Team),
                    Some(Item::Tracker(TrackerItem::Filter(f))) if focused => Some(Part::Filter(*f)),
                    _ => None,
                };
                // The key field shows what's typed (as dots) while it's typed.
                let mut choice = t.linear().clone();
                if let Some((TrackerItem::Key, input)) = &self.tracker_typing {
                    choice.set_key_input(input.text());
                }
                let drawn = linear_rows::rows(&choice, focus, t.editable(), true, width);
                for (line, part) in drawn.lines {
                    let item = part.map(|p| match p {
                        Part::Key => TrackerItem::Key,
                        Part::ChangeKey => TrackerItem::ChangeKey,
                        Part::Team => TrackerItem::Team,
                        Part::Filter(f) => TrackerItem::Filter(f),
                    });
                    let action = item.and_then(at);
                    rows.push(Row { action, focus: item.is_some_and(on), ..Row::text(line) });
                }
                if matches!(self.tracker_typing, Some((TrackerItem::Key, _))) {
                    wrapped(rows, "Enter verifies and stores the key; Esc puts it back.", width, muted);
                }
            }
        }
        if let Some(e) = t.error() {
            rows.push(Row::blank());
            wrapped(rows, &format!("✗ {e}"), width, Style::default().fg(palette().red));
        }
        if let Some(line) = t.status_line() {
            rows.push(Row::blank());
            wrapped(rows, &line, width, Style::default().fg(palette().green));
        }
        rows.push(Row::blank());
        // The buttons on one line, each its own stop.
        let mut spans = Vec::new();
        let mut targets = Vec::new();
        let mut x = 0u16;
        let mut buttons = vec![(TrackerItem::Cancel, tr::CANCEL, t.cancel_enabled())];
        if t.offers_force() {
            buttons.push((TrackerItem::Force, tr::SWITCH_ANYWAY, t.editable()));
        }
        buttons.push((TrackerItem::Primary, t.primary_label(), t.primary_enabled()));
        let mut focus = false;
        for (item, label, enabled) in buttons {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
                x += 2;
            }
            let text = format!("[ {label} ]");
            let w = panel::display_width(&text) as u16;
            let mut style = Style::default().fg(if enabled { palette().foreground } else { palette().muted });
            if item == TrackerItem::Primary && enabled {
                style = style.fg(palette().cyan);
            }
            if item == TrackerItem::Force {
                style = style.fg(palette().red);
            }
            if on(item) {
                style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
                focus = true;
            }
            if let Some(i) = at(item) {
                targets.push((x, w, Hit::Action(i)));
            }
            spans.push(Span::styled(text, style));
            x += w;
        }
        rows.push(Row { targets, focus, ..Row::text(Line::from(spans)) });
    }

    /// A text field's rows: its label over its box (the typed text while
    /// it's typed), its help, and its error.
    fn text_rows(&self, rows: &mut Vec<Row>, t: Text, on: bool, i: usize, width: usize) {
        let typed = match &self.editing {
            Some((e, input)) if *e == t => Some(input),
            _ => None,
        };
        let shown = typed.map(LineInput::text).unwrap_or_else(|| self.form.text(t).to_string());
        let placeholder = self.form.placeholder(t);
        let mut field = Textbox::new(&shown)
            .placeholder(&placeholder)
            .focused(on)
            .invalid(self.form.error(t).is_some());
        if let Some(input) = typed {
            field = field.caret(Some(input.caret()));
        }
        let label = if t.numeric() { format!("{} ({SECONDS})", t.label()) } else { t.label().to_string() };
        field_rows(rows, &label, field, on, false, Some(i), width);
        if let Some(e) = self.form.error(t) {
            wrapped(rows, e, width, Style::default().fg(palette().red));
        }
        let muted = Style::default().fg(palette().muted);
        if typed.is_some() {
            wrapped(rows, "Enter saves; Esc puts it back.", width, muted);
        } else {
            wrapped(rows, &self.form.help(t), width, muted);
        }
    }
}
