//! Settings ▸ Documentation (`docs-settings`): where the project's docs live and
//! where its sync stands, over the shared
//! [`DocsForm`](shelbi_app::settings::docs::DocsForm) the desktop app's Documentation
//! page draws too.
//!
//! ```text
//!  Documentation
//!  Where this project's docs live (its plans, decisions and docs), and
//!  whether Shelbi syncs it on its own.
//!
//!  Source
//!  (•) Orphan branch
//!  ( ) Repo folder
//!  Branch
//!  ┌──────────────────────────────┐
//!  │ docs/planning                │
//!  └──────────────────────────────┘
//!  [x] Auto-sync
//!  Status  ● Synced · 2m ago   [ Sync now ]
//! ```
//!
//! ↑/↓ move between the rows. Enter or ←/→ on a source picks it; Enter on
//! the Branch / Folder field types in it (Enter saves it, Esc puts it
//! back); Enter or Space toggles Auto-sync; Enter on the button runs Sync
//! now (or Set up docs); `r` reads the page again. The page's reads, saves,
//! Sync now and Set up run off the UI thread
//! ([`DocsPage::take_docs_work`]).

use chrono::Utc;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use shelbi_app::settings::general::LineInput;
use shelbi_app::settings::orchestrator::SaveStatus;
use shelbi_app::settings::docs::{self as wk, Tone, DocsDone, DocsForm, DocsWork};
use shelbi_app::settings::SettingsPage;
use shelbi_core::DocsSource;

use super::workflows::Nav;
use super::{field_rows, wrapped, Hit, Row};
use crate::panel;
use crate::textbox::Textbox;
use crate::theme::palette;

/// A row the page's keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Source(DocsSource),
    Location,
    AutoSync,
    Button,
}

/// The Documentation page: the shared form, the project it's for, the row the keys
/// are on and the field being typed.
#[derive(Debug, Default)]
pub(crate) struct DocsPage {
    project: Option<String>,
    form: DocsForm,
    cursor: usize,
    typing: Option<LineInput>,
}

impl DocsPage {
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    #[cfg(test)]
    pub(crate) fn form(&self) -> &DocsForm {
        &self.form
    }

    /// The rows the keys stop on, top to bottom.
    pub(crate) fn items(&self) -> Vec<Item> {
        if self.form.config().is_none() {
            return Vec::new();
        }
        let mut items: Vec<Item> = wk::SOURCES.into_iter().map(Item::Source).collect();
        items.extend([Item::Location, Item::AutoSync, Item::Button]);
        items
    }

    fn clamp_cursor(&mut self) {
        let n = self.items().len();
        self.cursor = self.cursor.min(n.saturating_sub(1));
    }

    // --- the work ----------------------------------------------------------------

    /// A visit to the page for `project` (`None`: no project open): its
    /// config and the hub's status are read again.
    pub(crate) fn visit(&mut self, project: Option<&str>) {
        if self.project.as_deref() != project {
            self.project_changed();
            self.project = project.map(str::to_string);
        }
        if self.project.is_some() {
            self.form.visit();
        }
    }

    /// Read the hub's status again (`r`, a hub event).
    pub(crate) fn refresh(&mut self) {
        if self.project.is_some() {
            self.form.visit();
        }
    }

    /// Whether the page has work for the shell.
    pub(crate) fn work_due(&self) -> bool {
        self.project.is_some() && self.form.work_due()
    }

    /// The page's work to run now: its project, the form's generation (to
    /// hand back with each result) and the work, in order.
    pub(crate) fn take_docs_work(&mut self) -> Option<(String, u64, Vec<DocsWork>)> {
        let project = self.project.clone()?;
        if !self.form.work_due() {
            return None;
        }
        Some((project, self.form.generation(), self.form.take_work()))
    }

    /// A piece of work landed for `project`. Returns whether it was taken.
    pub(crate) fn docs_done(&mut self, project: &str, generation: u64, done: DocsDone) -> bool {
        if self.project.as_deref() != Some(project) {
            return false;
        }
        let taken = self.form.done(generation, done);
        self.clamp_cursor();
        taken
    }

    /// Another project is shown: everything goes.
    pub(crate) fn project_changed(&mut self) {
        self.project = None;
        self.form.reset();
        self.cursor = 0;
        self.typing = None;
    }

    // --- typing ------------------------------------------------------------------

    /// Whether the field is being typed (it holds every key).
    pub(crate) fn typing(&self) -> bool {
        self.typing.is_some()
    }

    /// Drop what's typed (Esc, and leaving the page).
    pub(crate) fn cancel_typing(&mut self) {
        self.typing = None;
        self.form.revert_text();
    }

    /// Paste into the field being typed.
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match self.typing.as_mut() {
            Some(input) => {
                input.paste(text.trim());
                true
            }
            None => false,
        }
    }

    /// Enter in the field (or a click elsewhere): the text is checked and
    /// saved.
    pub(crate) fn commit_typing(&mut self) {
        let Some(input) = self.typing.take() else { return };
        self.form.set_text(&input.text());
        self.form.commit_text();
    }

    fn typing_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let Some(input) = self.typing.as_mut() else { return };
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

    // --- keys --------------------------------------------------------------------

    /// Move the selection a row down or up.
    pub(crate) fn step(&mut self, down: bool) {
        let n = self.items().len();
        self.cursor = if down { (self.cursor + 1).min(n.saturating_sub(1)) } else { self.cursor.saturating_sub(1) };
    }

    /// A key on the page: whether it took it (`false` leaves it to Settings:
    /// Esc, Tab, a digit, the scroll keys, `r`).
    pub(crate) fn key(&mut self, k: KeyEvent, nav: Nav) -> bool {
        if self.typing() {
            self.typing_key(k);
            return true;
        }
        if nav.up || nav.down {
            self.step(nav.down);
            return true;
        }
        if nav.activate {
            self.activate(self.cursor);
            return true;
        }
        let item = self.items().get(self.cursor).copied();
        match (k.code, item) {
            (KeyCode::Left | KeyCode::Right, Some(Item::Source(_))) => {
                self.form.step_source(if k.code == KeyCode::Right { 1 } else { -1 });
                // The cursor follows the source it picked.
                let now = Item::Source(self.form.source());
                if let Some(at) = self.items().iter().position(|i| *i == now) {
                    self.cursor = at;
                }
                true
            }
            (KeyCode::Char(' '), Some(Item::AutoSync)) if k.modifiers.is_empty() => {
                self.form.toggle_auto();
                true
            }
            _ => false,
        }
    }

    /// Run row `i` (Enter, a click).
    pub(crate) fn activate(&mut self, i: usize) {
        let Some(item) = self.items().get(i).copied() else { return };
        self.cursor = i;
        match item {
            Item::Source(source) => self.form.choose_source(source),
            Item::Location => {
                if self.form.editable() {
                    self.typing = Some(LineInput::new(&self.form.text()));
                }
            }
            Item::AutoSync => self.form.toggle_auto(),
            Item::Button => self.form.press(),
        }
        self.clamp_cursor();
    }

    // --- drawing -----------------------------------------------------------------

    /// The page's rows at `width`. Each focusable row is tagged with its
    /// item's index.
    pub(crate) fn rows(&self, width: usize, focused: bool, has_project: bool) -> Vec<Row> {
        let muted = Style::default().fg(palette().muted);
        let bold = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
        let mut rows = vec![Row::text(Line::from(Span::styled(SettingsPage::Docs.label(), bold)))];
        if !has_project {
            wrapped(&mut rows, "Open a project to set up its docs.", width, muted);
            return rows;
        }
        wrapped(&mut rows, wk::HELP, width, muted);
        rows.push(Row::blank());
        let f = &self.form;
        let red = Style::default().fg(palette().red);
        if let Some(e) = f.unreadable() {
            wrapped(&mut rows, &format!("The project's settings don't read: {e}"), width, red);
            wrapped(&mut rows, "Press r to read them again.", width, muted);
            return rows;
        }
        if f.config().is_none() {
            rows.push(Row::text(Line::from(Span::styled(
                "Loading…",
                muted.add_modifier(Modifier::ITALIC),
            ))));
            return rows;
        }
        if let Some(e) = f.block_unreadable() {
            wrapped(&mut rows, &format!("The project's docs: block doesn't read ({e}). A change here replaces it."), width, red);
            rows.push(Row::blank());
        }
        let items = self.items();
        let at = |item: Item| items.iter().position(|i| *i == item);
        let on = |item: Item| focused && at(item) == Some(self.cursor);
        let label_style = |item: Item, enabled: bool| {
            let mut style = Style::default().fg(if enabled { palette().white } else { palette().muted });
            if on(item) {
                style = style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);
            }
            style
        };
        let editable = f.editable();

        // Source.
        rows.push(Row::text(Line::from(Span::styled(wk::SOURCE_LABEL, bold))));
        for source in wk::SOURCES {
            let item = Item::Source(source);
            let picked = f.source() == source;
            rows.push(Row {
                action: at(item),
                focus: on(item),
                ..Row::text(Line::from(vec![
                    Span::styled(
                        if picked { "(•) " } else { "( ) " },
                        Style::default().fg(if picked { palette().cyan } else { palette().gray }),
                    ),
                    Span::styled(wk::source_label(source), label_style(item, editable)),
                ]))
            });
            wrapped(&mut rows, &format!("    {}", wk::source_help(source)), width, muted);
        }
        rows.push(Row::blank());

        // The branch or the folder.
        let source = f.source();
        let shown = self.typing.as_ref().map(LineInput::text).unwrap_or_else(|| f.text());
        let mut field = Textbox::new(&shown).placeholder(wk::placeholder(source)).focused(on(Item::Location));
        if let Some(input) = &self.typing {
            field = field.caret(Some(input.caret()));
        }
        field_rows(
            &mut rows,
            wk::location_label(source),
            field,
            on(Item::Location),
            !editable,
            at(Item::Location),
            width,
        );
        match f.error() {
            Some(e) => wrapped(&mut rows, &format!("✗ {e}"), width, red),
            None => wrapped(&mut rows, wk::location_help(source), width, muted),
        }
        if self.typing.is_some() {
            wrapped(&mut rows, "Enter saves it; Esc puts it back.", width, muted);
        }
        rows.push(Row::blank());

        // Auto-sync.
        rows.push(Row {
            action: at(Item::AutoSync),
            focus: on(Item::AutoSync),
            ..Row::text(Line::from(vec![
                Span::styled(if f.auto() { "[x] " } else { "[ ] " }, Style::default().fg(palette().cyan)),
                Span::styled(wk::AUTO_SYNC_LABEL, label_style(Item::AutoSync, editable)),
            ]))
        });
        wrapped(&mut rows, &format!("    {}", wk::AUTO_SYNC_HELP), width, muted);
        rows.push(Row::blank());

        // Status, and its button on the same line.
        let line = f.status_line(Utc::now());
        let tone = match line.tone {
            Tone::Good => palette().green,
            Tone::Busy => palette().yellow,
            Tone::Muted => palette().muted,
            Tone::Bad => palette().red,
        };
        let (button, enabled) = f.button();
        let lead = format!("{}  ", wk::STATUS_LABEL);
        let status = format!("● {}   ", line.text);
        let text = format!("[ {} ]", button.label());
        let x = (panel::display_width(&lead) + panel::display_width(&status)) as u16;
        let w = panel::display_width(&text) as u16;
        let mut style = Style::default().fg(if enabled { palette().cyan } else { palette().muted });
        if on(Item::Button) {
            style = style.add_modifier(Modifier::BOLD | Modifier::REVERSED);
        }
        let targets = at(Item::Button).map(|i| vec![(x, w, Hit::Action(i))]).unwrap_or_default();
        rows.push(Row {
            targets,
            focus: on(Item::Button),
            ..Row::text(Line::from(vec![
                Span::styled(lead, bold),
                Span::styled(status, Style::default().fg(tone)),
                Span::styled(text, style),
            ]))
        });
        if let Some(root) = f.root() {
            wrapped(&mut rows, &format!("At {root}"), width, muted);
        }
        if let Some(pr) = f.pull_request() {
            wrapped(&mut rows, &format!("Pull request: {pr}"), width, muted);
        }
        if let Some(e) = f.sync_error() {
            wrapped(&mut rows, &format!("✗ {e}"), width, red);
        }
        if let Some(msg) = f.status_message() {
            let style = match f.status() {
                SaveStatus::Failed(_) => red,
                _ => Style::default().fg(palette().green),
            };
            rows.push(Row::blank());
            wrapped(&mut rows, &msg, width, style);
        }
        rows
    }
}
