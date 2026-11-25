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
//! ```
//!
//! ↑/↓ move between the rows. Enter on a text field starts typing in it;
//! Enter saves what was typed and Esc puts the field back. The Default
//! workflow changes with ←/→ (Enter steps it too) and saves at once.
//! Enter (or ←/→) on Advanced opens or closes the intervals. A value the
//! form refuses keeps its text, shows its error in red under the field and
//! isn't written.
//!
//! The project's issue tracker has a page of its own, Settings ▸ Issues
//! ([`super::issues`]).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use shelbi_app::settings::general::LineInput;
use shelbi_app::settings::properties::{
    PropertiesForm, Text, ADVANCED_LABEL, DEFAULT_WORKFLOW_HELP, DEFAULT_WORKFLOW_LABEL, SECONDS, TEXTS, TITLE,
};
use shelbi_app::settings::{Job, Ticket};
use shelbi_state::workflow_config::WorkflowConfig;

use super::workflows::Nav;
use super::{field_rows, picker_rows, wrapped, Row};
use crate::textbox::Textbox;
use crate::theme::palette;

/// A row the page's keys stop on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Item {
    Text(Text),
    Workflow,
    Advanced,
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
}

impl PropertiesPage {
    #[cfg(test)]
    pub(crate) fn form(&self) -> &PropertiesForm {
        &self.form
    }

    pub(crate) fn cursor(&self) -> usize {
        self.cursor
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
        items
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
        true
    }

    /// Another project is shown: everything goes.
    pub(crate) fn project_changed(&mut self) {
        self.form.reset();
        self.load.reset();
        self.save.reset();
        self.cursor = 0;
        self.editing = None;
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
        self.editing.is_some()
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
    }

    /// Paste into the field being typed (an API key is usually pasted).
    pub(crate) fn paste(&mut self, text: &str) -> bool {
        match self.editing.as_mut() {
            Some((_, input)) => {
                input.paste(text);
                true
            }
            None => false,
        }
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
        if self.typing() {
            return Some(self.typing_key(k));
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
