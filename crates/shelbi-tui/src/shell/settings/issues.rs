//! Settings ▸ Issues (`settings-issues-page`): where the project's issues
//! live, over the shared
//! [`TrackerForm`](shelbi_app::settings::tracker::TrackerForm) the desktop
//! app's Issues page draws too.
//!
//! ```text
//!  Issues
//!  Where this project's issues live. A project uses one external tracker
//!  at most: GitHub or Linear.
//!
//!  (•) Local board
//!  ( ) GitHub Issues
//!  ( ) Linear
//!  …the chosen board's fields (the GitHub repo, or `crate::linear_rows`)…
//!  [ ] Always require human review before beginning work
//!  [ ] Only work on issues assigned to me
//!  [ Cancel ]  [ Connect ]
//! ```
//!
//! ↑/↓ move between the rows. Enter or ←/→ on a board picks it; Enter on
//! the repo or the API key field types in it (a pasted key shows as dots,
//! Enter verifies and stores it); the Linear pickers step with ←/→ (Enter
//! steps too), `r` reads a list that failed again; nothing is written until
//! Connect (or Save). The project's config is read on every visit, and its
//! Linear reads and the connection run off the UI thread
//! ([`IssuesPage::take_tracker_work`]).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use shelbi_app::github_setup::{GithubAnswer, GithubRequest};
use shelbi_app::linear_setup::{Filter, LinearAnswer, LinearRequest};
use shelbi_app::new_project::{self as np, Tracker};
use shelbi_app::settings::general::LineInput;
use shelbi_app::settings::tracker::{self as tr, TrackerForm};
use shelbi_app::settings::{Job, SettingsPage, Ticket};
use shelbi_app::tracker_connect::{rules_copy, ConnectError, ConnectTracker, Connected};
use shelbi_state::workflow_config::WorkflowConfig;

use super::workflows::Nav;
use super::{field_rows, wrapped, Hit, Row};
use crate::linear_rows::{self, Part};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// A row the page's keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Choice(Tracker),
    Repo,
    Key,
    ChangeKey,
    Team,
    Filter(Filter),
    /// The agent rules' toggles (an external tracker only).
    RequireReview,
    AssignedOnly,
    Cancel,
    Force,
    Primary,
}

/// Work the page needs run off the UI thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrackerWork {
    Linear(LinearRequest),
    Connect(ConnectTracker),
    /// A GitHub read (whose account `gh` is, for the assigned-only rule's
    /// title).
    Github(GithubRequest),
}

/// Its outcome, handed back by [`IssuesPage::tracker_done`].
#[derive(Debug, Clone)]
pub(crate) enum TrackerDone {
    Linear(LinearAnswer),
    Connected(Result<Connected, ConnectError>),
    Github(GithubAnswer),
}

impl TrackerWork {
    /// Run it with `service` (blocking).
    pub(crate) fn run(self, service: &tr::TrackerService) -> TrackerDone {
        match self {
            TrackerWork::Linear(req) => TrackerDone::Linear((service.linear)(req)),
            TrackerWork::Connect(req) => TrackerDone::Connected((service.connect)(req)),
            TrackerWork::Github(req) => TrackerDone::Github((service.github)(req)),
        }
    }
}

/// The Issues page: the shared form, the project read that seeds it, the
/// row the keys are on and the field being typed.
#[derive(Debug, Default)]
pub(crate) struct IssuesPage {
    load: Job<()>,
    /// Why the project's config didn't read, if it didn't.
    unreadable: Option<String>,
    cursor: usize,
    tracker: TrackerForm,
    /// The field being typed (the repo, the API key).
    typing: Option<(Item, LineInput)>,
    /// Work for the shell to run.
    work: Vec<TrackerWork>,
}

impl IssuesPage {
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub(crate) fn tracker(&self) -> &TrackerForm {
        &self.tracker
    }

    /// The rows the keys stop on, top to bottom.
    pub(crate) fn items(&self) -> Vec<Item> {
        let t = &self.tracker;
        if !t.loaded() {
            return Vec::new();
        }
        let mut items: Vec<Item> = tr::CHOICES.into_iter().map(Item::Choice).collect();
        match t.tracker() {
            Tracker::Github => items.push(Item::Repo),
            Tracker::Linear => {
                let linear = t.linear();
                if linear.needs_key() {
                    items.push(Item::Key);
                } else if matches!(linear.key_state(), shelbi_app::linear_setup::KeyState::Valid { .. }) {
                    items.push(Item::ChangeKey);
                }
                items.push(Item::Team);
                if linear.team_option().is_some() {
                    items.extend(Filter::ALL.map(Item::Filter));
                }
            }
            Tracker::FileSystem => {}
        }
        if t.shows_rules() {
            items.extend([Item::RequireReview, Item::AssignedOnly]);
        }
        items.push(Item::Cancel);
        if t.offers_force() {
            items.push(Item::Force);
        }
        items.push(Item::Primary);
        items
    }

    // --- the work ----------------------------------------------------------------

    /// Queue the reads the form has due: Linear's, and GitHub's login.
    fn queue_reads(&mut self) {
        while let Some(req) = self.tracker.linear_due() {
            self.work.push(TrackerWork::Linear(req));
        }
        if let Some(req) = self.tracker.github_due() {
            self.work.push(TrackerWork::Github(req));
        }
    }

    /// Whether the page has work for the shell.
    pub(crate) fn work_due(&self) -> bool {
        !self.work.is_empty()
    }

    /// The page's work to run now, for the project shown.
    pub(crate) fn take_tracker_work(&mut self) -> Option<(String, Vec<TrackerWork>)> {
        let project = self.load.project()?.to_string();
        if self.work.is_empty() {
            return None;
        }
        Some((project, std::mem::take(&mut self.work)))
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
            TrackerDone::Github(answer) => {
                self.tracker.github_answer(answer);
                false
            }
        };
        self.queue_reads();
        self.clamp_cursor();
        wrote
    }

    fn clamp_cursor(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    // --- loading -----------------------------------------------------------------

    /// Start a read for `project` (every visit, to show a hand edit or a
    /// connection made elsewhere): a ticket the shell's reader carries, or
    /// `None` with no project open.
    pub(crate) fn begin_load(&mut self, project: Option<&str>) -> Option<Ticket> {
        let Some(project) = project else {
            self.project_changed();
            return None;
        };
        if self.load.project() != Some(project) {
            self.project_changed();
        }
        Some(self.load.begin(Some(project)))
    }

    /// The read landed. Returns whether it was taken (a stale one is
    /// dropped).
    pub(crate) fn loaded(&mut self, ticket: &Ticket, read: Result<WorkflowConfig, String>) -> bool {
        if !self.load.complete(ticket, Ok(())) {
            return false;
        }
        match read {
            Ok(config) => {
                self.unreadable = None;
                self.tracker.load(&config.issue_tracker().config);
                self.queue_reads();
                self.clamp_cursor();
            }
            Err(e) => self.unreadable = Some(e),
        }
        true
    }

    /// Another project is shown: everything goes.
    pub(crate) fn project_changed(&mut self) {
        self.load.reset();
        self.unreadable = None;
        self.cursor = 0;
        self.tracker.reset();
        self.typing = None;
        self.work.clear();
    }

    // --- typing ------------------------------------------------------------------

    /// Whether a field is being typed (it holds every key).
    pub(crate) fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// Drop what's typed (Esc, and leaving the page).
    pub(crate) fn cancel_typing(&mut self) {
        if let Some((Item::Key, _)) = self.typing.take() {
            self.tracker.cancel_change_key();
        }
    }

    /// Paste into the field being typed (an API key is usually pasted).
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match self.typing.as_mut() {
            Some((_, input)) => {
                input.paste(text.trim());
                true
            }
            None => false,
        }
    }

    /// Enter in a field: the repo takes the text; the key is verified and
    /// stored.
    pub(crate) fn commit_typing(&mut self) {
        let Some((item, input)) = self.typing.take() else { return };
        match item {
            Item::Repo => self.tracker.set_repo(&input.text()),
            Item::Key => {
                self.tracker.set_key_input(&input.text());
                self.tracker.submit_key();
            }
            _ => {}
        }
        self.queue_reads();
        self.clamp_cursor();
    }

    /// A key while a field is typed.
    fn typing_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some((_, input)) = self.typing.as_mut() else { return };
        match k.code {
            KeyCode::Enter => self.commit_typing(),
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

    /// ←/→ on `item`.
    fn step_item(&mut self, item: Item, forward: bool) {
        match item {
            Item::Choice(_) => self.tracker.step(forward),
            Item::Team => self.tracker.step_team(forward),
            Item::Filter(f) => self.tracker.step_filter(f, forward),
            _ => return,
        }
        // The cursor follows the board it picked.
        if let Item::Choice(_) = item {
            let now = Item::Choice(self.tracker.tracker());
            if let Some(at) = self.items().iter().position(|i| *i == now) {
                self.cursor = at;
            }
        }
        self.queue_reads();
        self.clamp_cursor();
    }

    // --- keys --------------------------------------------------------------------

    /// Move the selection a row down or up.
    pub(crate) fn step(&mut self, down: bool) {
        let n = self.items().len();
        self.cursor = if down { (self.cursor + 1).min(n.saturating_sub(1)) } else { self.cursor.saturating_sub(1) };
    }

    /// A key on the page: whether it took it (`false` leaves it to Settings:
    /// Esc, Tab, a digit, the scroll keys).
    pub(crate) fn key(&mut self, k: KeyEvent, nav: Nav) -> bool {
        if self.typing() {
            self.typing_key(k);
            return true;
        }
        if let (KeyCode::Char('r'), Some(Item::Team | Item::Filter(_))) =
            (k.code, self.items().get(self.cursor).copied())
        {
            if k.modifiers.is_empty() {
                self.tracker.retry_linear();
                self.queue_reads();
                return true;
            }
        }
        if nav.up || nav.down {
            self.step(nav.down);
            return true;
        }
        if nav.activate {
            self.activate(self.cursor);
            return true;
        }
        let forward = match k.code {
            KeyCode::Left => false,
            KeyCode::Right => true,
            _ => return false,
        };
        if let Some(item) = self.items().get(self.cursor).copied() {
            self.step_item(item, forward);
        }
        true
    }

    /// Run row `i` (Enter, a click).
    pub(crate) fn activate(&mut self, i: usize) {
        let Some(item) = self.items().get(i).copied() else { return };
        self.cursor = i;
        let t = &mut self.tracker;
        match item {
            Item::Choice(choice) => t.select(choice),
            Item::Repo => {
                if t.editable() {
                    self.typing = Some((item, LineInput::new(t.issues().repo())));
                }
            }
            Item::Key => {
                if t.editable() && !t.linear().logging_in() {
                    self.typing = Some((item, LineInput::new(t.linear().key_input())));
                }
            }
            Item::ChangeKey => {
                t.change_key();
                if let Some(at) = self.items().iter().position(|i| *i == Item::Key) {
                    self.cursor = at;
                    self.typing = Some((Item::Key, LineInput::new("")));
                }
            }
            Item::Team => t.step_team(true),
            Item::Filter(f) => t.step_filter(f, true),
            Item::RequireReview => t.toggle_require_review(),
            Item::AssignedOnly => t.toggle_assigned_only(),
            Item::Cancel => t.cancel(),
            Item::Force => {
                if let Some(req) = t.submit(true) {
                    self.work.push(TrackerWork::Connect(req));
                }
            }
            Item::Primary => {
                if let Some(req) = t.submit(false) {
                    self.work.push(TrackerWork::Connect(req));
                }
            }
        }
        self.queue_reads();
        self.clamp_cursor();
    }

    // --- drawing -----------------------------------------------------------------

    /// The page's rows at `width`: its title and help, the boards, the
    /// chosen board's fields, the agent rules, what's wrong or what was
    /// written, and the buttons. Each focusable row is tagged with its
    /// item's index.
    pub(crate) fn rows(&self, width: usize, focused: bool, has_project: bool) -> Vec<Row> {
        let muted = Style::default().fg(palette().muted);
        let mut rows = vec![Row::text(Line::from(Span::styled(
            SettingsPage::Issues.label(),
            Style::default().fg(palette().white).add_modifier(Modifier::BOLD),
        )))];
        if !has_project {
            wrapped(&mut rows, "Open a project to choose where its issues live.", width, muted);
            return rows;
        }
        wrapped(&mut rows, tr::HELP, width, muted);
        rows.push(Row::blank());
        if let Some(e) = &self.unreadable {
            let red = Style::default().fg(palette().red);
            wrapped(&mut rows, &format!("The project's settings don't read: {e}"), width, red);
            wrapped(&mut rows, "Press r to read them again.", width, muted);
            return rows;
        }
        let t = &self.tracker;
        if !t.loaded() {
            rows.push(Row::text(Line::from(Span::styled(
                "Loading…",
                Style::default().fg(palette().muted).add_modifier(Modifier::ITALIC),
            ))));
            return rows;
        }
        let items = self.items();
        let at = |item: Item| items.iter().position(|i| *i == item);
        let on = |item: Item| focused && at(item) == Some(self.cursor);
        for choice in tr::CHOICES {
            let item = Item::Choice(choice);
            let picked = t.tracker() == choice;
            let mut style = Style::default().fg(if picked { palette().white } else { palette().gray });
            if on(item) {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            rows.push(Row {
                action: at(item),
                focus: on(item),
                ..Row::text(Line::from(vec![
                    Span::styled(
                        if picked { "(•) " } else { "( ) " },
                        Style::default().fg(if picked { palette().cyan } else { palette().gray }),
                    ),
                    Span::styled(tr::choice_title(choice), style),
                ]))
            });
        }
        rows.push(Row::blank());
        match t.tracker() {
            Tracker::FileSystem => wrapped(&mut rows, np::FILE_SYSTEM_BODY, width, muted),
            Tracker::Github => {
                let typed = match &self.typing {
                    Some((Item::Repo, input)) => Some(input),
                    _ => None,
                };
                let shown = typed.map(LineInput::text).unwrap_or_else(|| t.issues().repo().to_string());
                let mut field = Textbox::new(&shown).placeholder("owner/repo").focused(on(Item::Repo));
                if let Some(input) = typed {
                    field = field.caret(Some(input.caret()));
                }
                field_rows(&mut rows, np::GITHUB_REPO_LABEL, field, on(Item::Repo), !t.editable(), at(Item::Repo), width);
                wrapped(&mut rows, np::GITHUB_BODY, width, muted);
            }
            Tracker::Linear => {
                let focus = match items.get(self.cursor) {
                    Some(Item::Key) if focused => Some(Part::Key),
                    Some(Item::ChangeKey) if focused => Some(Part::ChangeKey),
                    Some(Item::Team) if focused => Some(Part::Team),
                    Some(Item::Filter(f)) if focused => Some(Part::Filter(*f)),
                    _ => None,
                };
                // The key field shows what's typed (as dots) while it's typed.
                let mut choice = t.linear().clone();
                if let Some((Item::Key, input)) = &self.typing {
                    choice.set_key_input(input.text());
                }
                let drawn = linear_rows::rows(&choice, focus, t.editable(), true, width);
                for (line, part) in drawn.lines {
                    let item = part.map(|p| match p {
                        Part::Key => Item::Key,
                        Part::ChangeKey => Item::ChangeKey,
                        Part::Team => Item::Team,
                        Part::Filter(f) => Item::Filter(f),
                    });
                    let action = item.and_then(at);
                    rows.push(Row { action, focus: item.is_some_and(on), ..Row::text(line) });
                }
                if matches!(self.typing, Some((Item::Key, _))) {
                    wrapped(&mut rows, "Enter verifies and stores the key; Esc puts it back.", width, muted);
                }
            }
        }
        if t.shows_rules() {
            // The agent rules: a checkbox each, its help under it.
            rows.push(Row::blank());
            let rules = t.rules();
            let assigned = t.assigned_title();
            for (item, checked, title, help) in [
                (Item::RequireReview, rules.require_review, rules_copy::REVIEW_TITLE, rules_copy::REVIEW_HELP),
                (Item::AssignedOnly, rules.assigned_only, assigned.as_str(), rules_copy::ASSIGNED_HELP),
            ] {
                let mut style = Style::default().fg(if t.editable() { palette().white } else { palette().muted });
                if on(item) {
                    style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
                }
                rows.push(Row {
                    action: at(item),
                    focus: on(item),
                    ..Row::text(Line::from(vec![
                        Span::styled(if checked { "[x] " } else { "[ ] " }, Style::default().fg(palette().cyan)),
                        Span::styled(title.to_string(), style),
                    ]))
                });
                wrapped(&mut rows, &format!("    {help}"), width, muted);
            }
        }
        if let Some(e) = t.error() {
            rows.push(Row::blank());
            wrapped(&mut rows, &format!("✗ {e}"), width, Style::default().fg(palette().red));
        }
        if let Some(line) = t.status_line() {
            rows.push(Row::blank());
            wrapped(&mut rows, &line, width, Style::default().fg(palette().green));
        }
        rows.push(Row::blank());
        // The buttons on one line, each its own stop.
        let mut spans = Vec::new();
        let mut targets = Vec::new();
        let mut x = 0u16;
        let mut buttons = vec![(Item::Cancel, tr::CANCEL, t.cancel_enabled())];
        if t.offers_force() {
            buttons.push((Item::Force, tr::SWITCH_ANYWAY, t.editable()));
        }
        buttons.push((Item::Primary, t.primary_label(), t.primary_enabled()));
        let mut focus = false;
        for (item, label, enabled) in buttons {
            if !spans.is_empty() {
                spans.push(Span::raw("  "));
                x += 2;
            }
            let text = format!("[ {label} ]");
            let w = panel::display_width(&text) as u16;
            let mut style = Style::default().fg(if enabled { palette().foreground } else { palette().muted });
            if item == Item::Primary && enabled {
                style = style.fg(palette().cyan);
            }
            if item == Item::Force {
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
        rows
    }
}
