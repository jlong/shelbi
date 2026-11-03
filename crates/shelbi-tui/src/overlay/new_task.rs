//! New task in the TUI (`new-task-dialog`): the desktop's New task dialog
//! (Figma `2730:10845`) in the TUI's half-block language. The board's `n`,
//! its `+ New issue` chip and the palette's New Issue open it over the
//! current view.
//!
//! [`NewTaskForm`] (shared with the desktop, in `shelbi_app::new_task`)
//! holds the draft and decides everything, the words included; this paints
//! its model, keeps two [`TextArea`]s in step with it for typing, and hands
//! the shell a [`CreateRequest`] to run off the UI thread. The shell hands
//! the answer back ([`NewTaskDialog::finished`]).
//!
//! ```text
//!  ┌─────────────────────────────────────────────────────────────┐
//!  │ New task                                                    │
//!  │ Add work for your Orchestrator and agents.                  │
//!  │                                                             │
//!  │ Title                                                       │
//!  │ ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄ │
//!  │  Restore the last active project on launch▏                 │
//!  │ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀ │
//!  │                                                             │
//!  │ Workflow            Status              Priority            │
//!  │  Task           ▾    ◌ Backlog      ▾    End of queue   ▾   │
//!  │                                                             │
//!  │ Description                                                 │
//!  │  (multi-line, Markdown)                                     │
//!  │                                                             │
//!  │  Let Orchestrator write this up                     ( ○ )   │
//!  │  Expand this draft into a complete task with acceptance…    │
//!  │ ─────────────────────────────────────────────────────────── │
//!  │ Ctrl+Enter to create                 [ Cancel ] [Create Task]│
//!  └─────────────────────────────────────────────────────────────┘
//! ```
//!
//! Keys, as the desktop's: Tab / ⇧Tab walk the controls and never leave the
//! dialog; Ctrl+Return (or ⌘Return) creates from anywhere; Return creates
//! from Title, opens a picker, toggles the switch, or presses the focused
//! button, and in Description is a new line; Esc closes an open picker,
//! else cancels; ↑ / ↓ and Return pick in an open picker. A click works on
//! every control and every picker row; a paste goes into the text field
//! being typed in.

use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph},
    Frame,
};
use shelbi_app::new_task::{self as model, Action, CreateRequest, Field, Key, Menu, NewTaskForm};
use shelbi_app::overlay::error_log::wrap;
use shelbi_app::text_area::TextArea;
use shelbi_app::view::{NewTaskModel, NewTaskOption};
use shelbi_core::Workflow;
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::centered_rect;
use crate::button::{self, Button};
use crate::kanban::{category_color, category_glyph};
use crate::modal_form::FormLines;
use crate::panel;
use crate::textbox::{self, Textbox};
use crate::theme::palette;

/// The panel's width (the desktop's 680pt), less on a narrower terminal.
pub const PANEL_W: u16 = 76;
/// The columns between the panel's edge and its content: the border and
/// the dialogs' padding inside it.
const PAD_X: u16 = 1 + super::DIALOG_PAD_X;
/// The Description's height in lines: it grows from the first to the
/// second with its text, then scrolls.
const DESCRIPTION_LINES: (usize, usize) = (5, 9);
/// The columns between the three pickers.
const PICKER_GAP: usize = 2;
/// The narrowest content that sets the three pickers side by side.
const PICKERS_MIN_W: usize = 42;

/// What the shell does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Run this off the UI thread and hand the answer to
    /// [`NewTaskDialog::finished`].
    Create(CreateRequest),
    /// Close the dialog: cancelled.
    Close,
}

/// What a click lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Control(Field),
    /// Row `i` of the open picker.
    Choice(usize),
}

/// The open dialog.
pub struct NewTaskDialog {
    project: String,
    form: NewTaskForm,
    title: TextArea,
    description: TextArea,
    /// The first of Description's wrapped lines in view.
    description_top: usize,
    /// Where each control was drawn last, for clicks (the open picker's
    /// rows last, so they win).
    hits: Vec<(Rect, Hit)>,
}

impl NewTaskDialog {
    /// A blank draft for `project` over its loaded `workflows`, on
    /// `default_workflow`.
    pub fn new(project: &str, workflows: &[Workflow], default_workflow: &str) -> Self {
        NewTaskDialog {
            project: project.to_string(),
            form: NewTaskForm::new(workflows, default_workflow),
            title: TextArea::default(),
            description: TextArea::default(),
            description_top: 0,
            hits: Vec::new(),
        }
    }

    /// The project it adds to.
    pub fn project(&self) -> &str {
        &self.project
    }

    pub fn form(&self) -> &NewTaskForm {
        &self.form
    }

    /// The create came back: `Ok` closes the dialog, an error stays under
    /// the form with the draft kept.
    pub fn finished(&mut self, result: Result<(), String>) -> Step {
        act(self.form.finished(result))
    }

    /// A key press.
    pub fn handle_key(&mut self, ev: KeyEvent) -> Step {
        match crate::keymap::chord_from_event(ev) {
            Some(chord) => self.key(chord),
            None => Step::Stay,
        }
    }

    /// A chord: the form's keys first, the rest typed into the text field
    /// with the keyboard.
    pub fn key(&mut self, chord: KeyChord) -> Step {
        let bare = chord.mods.is_empty();
        let focus = self.form.focus();
        let menu_open = self.form.menu().is_some();
        let in_text = !menu_open && focus.is_text();
        let key = match chord.code {
            Code::Tab if bare => Some(Key::Tab),
            Code::Tab if chord.mods == Mods::SHIFT => Some(Key::BackTab),
            Code::BackTab => Some(Key::BackTab),
            // A terminal rarely delivers ⌘, so Ctrl+Return creates too.
            Code::Enter if chord.mods == Mods::SUPER || chord.mods == Mods::CONTROL => Some(Key::CmdEnter),
            // Return in the Description is its new line.
            Code::Enter if bare && in_text && focus == Field::Description => None,
            Code::Enter if bare => Some(Key::Enter),
            Code::Esc if bare => Some(Key::Esc),
            Code::Char(' ') if bare && !in_text => Some(Key::Space),
            Code::Up if bare && !in_text => Some(Key::Up),
            Code::Down if bare && !in_text => Some(Key::Down),
            _ => None,
        };
        match key {
            Some(key) => act(self.form.key(key)),
            None => {
                if in_text {
                    self.edit(chord);
                }
                Step::Stay
            }
        }
    }

    /// The text field with the keyboard, and whether it's one line.
    fn field(&mut self) -> Option<(&mut TextArea, bool)> {
        if self.form.creating() || self.form.menu().is_some() {
            return None;
        }
        match self.form.focus() {
            Field::Title => Some((&mut self.title, true)),
            Field::Description => Some((&mut self.description, false)),
            _ => None,
        }
    }

    fn edit(&mut self, chord: KeyChord) {
        let page = DESCRIPTION_LINES.0;
        if let Some((text, single)) = self.field() {
            if text.edit(chord, single, page) {
                self.sync();
            }
        }
    }

    /// Bracketed paste into the text field being typed in (Title takes it
    /// on one line). `false` when no field has the keyboard.
    pub fn paste(&mut self, pasted: &str) -> bool {
        let Some((text, single)) = self.field() else {
            return false;
        };
        if single {
            text.insert_str(&pasted.replace("\r\n", " ").replace(['\r', '\n'], " "));
        } else {
            text.insert_str(pasted);
        }
        self.sync();
        true
    }

    /// Hand the fields' text to the form.
    fn sync(&mut self) {
        self.form.set_title(&self.title.text());
        self.form.set_description(&self.description.text());
    }

    /// A left click at a screen cell.
    pub fn handle_click(&mut self, col: u16, row: u16) -> Step {
        let at = |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        let hit = self.hits.iter().rev().find(|(r, _)| at(r)).map(|(_, h)| *h);
        if let Some(Hit::Choice(i)) = hit {
            if let Some((menu, _)) = self.form.menu() {
                self.form.choose(menu, i);
            }
            return Step::Stay;
        }
        // A click anywhere else closes an open picker first (its own box
        // toggles it shut).
        let open = self.form.menu().map(|(m, _)| m);
        if open.is_some() && hit.and_then(|h| match h {
            Hit::Control(f) => Menu::of(f),
            _ => None,
        }) != open
        {
            self.form.close_menu();
        }
        match hit {
            Some(Hit::Control(Field::Description)) => {
                self.form.set_focus(Field::Description);
                self.click_description(col, row);
                Step::Stay
            }
            Some(Hit::Control(f @ (Field::Workflow | Field::Status | Field::Priority))) => {
                if let Some(menu) = Menu::of(f) {
                    self.form.toggle_menu(menu);
                }
                Step::Stay
            }
            Some(Hit::Control(f @ (Field::WriteUp | Field::Cancel | Field::Create))) => act(self.form.press(f)),
            Some(Hit::Control(f)) => {
                self.form.set_focus(f);
                Step::Stay
            }
            _ => Step::Stay,
        }
    }

    /// The pointer moved: over an open picker's row, its cursor follows.
    pub fn handle_hover(&mut self, col: u16, row: u16) {
        let at = |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        if let Some(&(_, Hit::Choice(i))) = self.hits.iter().rev().find(|(r, _)| at(r)) {
            self.form.hover_menu(i);
        }
    }

    /// A click on the Description's box: the caret to the wrapped row it
    /// landed on (the caps take the row beside them).
    fn click_description(&mut self, col: u16, row: u16) {
        let rects: Vec<Rect> = self
            .hits
            .iter()
            .filter(|(_, h)| *h == Hit::Control(Field::Description))
            .map(|(r, _)| *r)
            .collect();
        let (_, n) = self.description_window(self.description_room() + 2 * textbox::INSET);
        let [top, ..] = rects[..] else {
            return;
        };
        if rects.len() != n + 2 {
            return;
        }
        let v = (row.saturating_sub(top.y + 1) as usize).min(n - 1);
        let x = col.saturating_sub(top.x + textbox::INSET as u16);
        self.description.place(self.description_top + v, x as f32);
    }

    fn description_room(&self) -> usize {
        self.description.wrap().map_or(1, |w| w.width() as usize)
    }

    /// Where the last draw put the controls that aren't buttons: the
    /// fields, the pickers and the switch's card. In light their fill is a
    /// button's, so the shell's hover must not take them for buttons.
    pub fn not_buttons(&self) -> Vec<Rect> {
        self.hits
            .iter()
            .filter(|(_, hit)| !matches!(hit, Hit::Control(Field::Cancel | Field::Create)))
            .map(|(r, _)| *r)
            .collect()
    }

    /// The panel's rect in `area` (the whole window): centered, as tall as
    /// its content, never taller than `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let width = panel_width(area);
        let lines = self.lines(content_width(width) as usize).lines.len();
        centered_rect(area, width, panel_height(lines))
    }

    /// Draw the panel into `rect` ([`Self::rect`]'s).
    pub fn render(&mut self, f: &mut Frame, rect: Rect) {
        self.hits.clear();
        if rect.width < 8 || rect.height < 4 {
            return;
        }
        super::dialog_frame(f.buffer_mut(), rect, Some(palette().dialog), None);
        let body = Rect { y: rect.y + 1, height: rect.height - 2, ..rect };
        let pad = 1.min(body.height / 2);
        let content = Rect {
            x: body.x + PAD_X,
            y: body.y + pad,
            width: content_width(rect.width),
            height: body.height - 2 * pad,
        };
        // ↑ / ↓ in the Description move by the rows it's drawn on.
        self.description.set_wrap(Some(textbox::wrap(textbox::room(content.width as usize).max(1))));
        self.description_top = self.description_window(content.width as usize).0;
        let out = self.lines(content.width as usize);
        let focus = Hit::Control(self.form.focus());
        out.render(f, content, focus, &mut self.hits);
        self.render_menu(f, rect);
    }

    /// Description's wrapped lines in view: the first, and how many.
    fn description_window(&self, width: usize) -> (usize, usize) {
        let text_w = textbox::room(width).max(1);
        let (rows, (caret, _)) = self.description.wrapped(&textbox::wrap(text_w));
        let n = rows.len().clamp(DESCRIPTION_LINES.0, DESCRIPTION_LINES.1);
        let mut top = self.description_top.min(rows.len().saturating_sub(1));
        if self.form.focus() == Field::Description {
            if caret < top {
                top = caret;
            } else if caret >= top + n {
                top = caret + 1 - n;
            }
        }
        (top, n)
    }

    /// The content's lines `width` wide, with each control's place.
    fn lines(&self, width: usize) -> FormLines<Hit> {
        let m = self.form.model();
        let mut out = FormLines::new(width).with_rest(palette().dialog_control);
        out.heading(model::TITLE, model::SUBTITLE);
        self.title_field(&mut out, &m);
        out.blank();
        pickers(&mut out, &m);
        out.blank();
        self.description_field(&mut out, &m);
        out.blank();
        write_up(&mut out, &m);
        out.footer();
        if let Some(error) = &m.error {
            out.blank();
            out.text(error, Style::default().fg(palette().red));
        }
        out.blank();
        out.rule();
        footer(&mut out, &m);
        out
    }

    fn title_field(&self, out: &mut FormLines<Hit>, m: &NewTaskModel) {
        let on = m.focus == Field::Title && m.menu.is_none();
        let field = Textbox::new(&m.title).placeholder(model::TITLE_PLACEHOLDER).focused(on);
        let field = if on { field.caret(Some(self.title.cursor().1)) } else { field };
        out.field(model::TITLE_LABEL, field, on, m.creating, &[], Hit::Control(Field::Title));
    }

    /// The Description: its label, then its wrapped lines on the fill, the
    /// caret's kept in view while it's typed in, the placeholder while it's
    /// empty.
    fn description_field(&self, out: &mut FormLines<Hit>, m: &NewTaskModel) {
        let width = out.width;
        let focused = m.focus == Field::Description && m.menu.is_none();
        out.lines.push(textbox::label(model::DESCRIPTION_LABEL, focused, false, width));
        let fill = if focused { palette().input_focus } else { palette().dialog_control };
        let style = textbox::area_text(focused, false).bg(fill);
        let text_w = textbox::room(width).max(1);
        let (rows, (caret_row, caret_col)) = self.description.wrapped(&textbox::wrap(text_w));
        let (top, n) = self.description_window(width);
        let lines = self.description.lines();
        let mut body = Vec::new();
        for v in top..top + n {
            let mut spans = Vec::new();
            if let Some(&row) = rows.get(v) {
                let caret = (focused && v == caret_row).then_some(caret_col);
                let (chars, caret) = textbox::row_chars(lines, row, caret, text_w);
                let used = textbox::caret_spans(&mut spans, &chars, caret, text_w, style);
                if v == 0 && m.description.is_empty() {
                    let hint = panel::truncate_right(model::DESCRIPTION_PLACEHOLDER, text_w.saturating_sub(used));
                    spans.push(Span::styled(hint, Style::default().fg(palette().muted).bg(fill)));
                }
            }
            body.push(spans);
        }
        out.rows(textbox::area_in(body, fill, width), Hit::Control(Field::Description));
    }

    /// The open picker's rows, dropped below its box over what follows,
    /// the chosen one checked, the keyboard's lit.
    fn render_menu(&mut self, f: &mut Frame, panel: Rect) {
        let Some((menu, cursor)) = self.form.menu() else {
            return;
        };
        let field = menu.field();
        let boxes: Vec<Rect> = self.hits.iter().filter(|(_, h)| *h == Hit::Control(field)).map(|(r, _)| *r).collect();
        let Some(last) = boxes.iter().max_by_key(|r| r.y) else {
            return;
        };
        let m = self.form.model();
        let options: &[NewTaskOption] = match menu {
            Menu::Workflow => &m.workflows,
            Menu::Status => &m.statuses,
            Menu::Priority => &m.priorities,
        };
        let w = last.width as usize;
        let bottom = panel.bottom().saturating_sub(1);
        for (i, o) in options.iter().enumerate() {
            let y = last.y + 1 + i as u16;
            if y >= bottom {
                break;
            }
            let rect = Rect { y, height: 1, ..*last };
            let lit = i == cursor;
            let fill = if lit { palette().input_focus } else { palette().dialog_control };
            let mark = if o.selected { "✓ " } else { "  " };
            let fg = if lit { palette().white } else { palette().foreground };
            let mut spans = vec![Span::styled(format!(" {mark}"), Style::default().fg(fg).bg(fill))];
            let mut used = 3;
            if let Some(c) = o.category {
                spans.push(Span::styled(format!("{} ", category_glyph(c)), Style::default().fg(category_color(c)).bg(fill)));
                used += 2;
            }
            let label = panel::truncate_right(&o.label, w.saturating_sub(used));
            used += panel::display_width(&label);
            spans.push(Span::styled(label, Style::default().fg(fg).bg(fill)));
            spans.push(Span::styled(" ".repeat(w.saturating_sub(used)), Style::default().bg(fill)));
            f.render_widget(Clear, rect);
            f.render_widget(Paragraph::new(Line::from(spans)), rect);
            self.hits.push((rect, Hit::Choice(i)));
        }
    }
}

/// Workflow, Status and Priority: three pickers side by side (stacked on a
/// narrow terminal), each its label over a box with its value and `▾`.
fn pickers(out: &mut FormLines<Hit>, m: &NewTaskModel) {
    let width = out.width;
    let items = [
        (model::WORKFLOW_LABEL, m.workflow.as_str(), None, Field::Workflow),
        (model::STATUS_LABEL, m.status.as_str(), Some(m.status_category), Field::Status),
        (model::PRIORITY_LABEL, m.priority.as_str(), None, Field::Priority),
    ];
    let picker = |w: usize, (label, value, category, field): (&str, &str, Option<shelbi_core::StatusCategory>, Field)| {
        let focused = m.focus == field;
        let mut lines = vec![textbox::label(label, focused, m.creating, w)];
        lines.extend(select_lines(value, category, focused, m.menu.is_some_and(|(o, _)| o.field() == field), m.creating, w));
        lines
    };
    if width < PICKERS_MIN_W {
        for (i, item) in items.into_iter().enumerate() {
            if i > 0 {
                out.blank();
            }
            let first = out.lines.len();
            out.lines.extend(picker(width, item));
            out.controls.push(crate::modal_form::Control {
                first,
                end: out.lines.len(),
                line: first + 1,
                x: 0,
                w: width as u16,
                h: textbox::BOX_H as u16,
                hit: Hit::Control(item.3),
            });
        }
        return;
    }
    let each = (width - 2 * PICKER_GAP) / 3;
    let widths = [each, each, width - 2 * PICKER_GAP - 2 * each];
    let columns: Vec<Vec<Line<'static>>> = items.iter().zip(widths).map(|(item, w)| picker(w, *item)).collect();
    let first = out.lines.len();
    for row in 0..columns[0].len() {
        let mut spans: Vec<Span<'static>> = Vec::new();
        for (i, col) in columns.iter().enumerate() {
            if i > 0 {
                spans.push(Span::raw(" ".repeat(PICKER_GAP)));
            }
            let line = col[row].clone();
            let used = line.width();
            spans.extend(line.spans);
            spans.push(Span::raw(" ".repeat(widths[i].saturating_sub(used))));
        }
        out.lines.push(Line::from(spans));
    }
    let end = out.lines.len();
    let mut x = 0;
    for (i, item) in items.iter().enumerate() {
        out.controls.push(crate::modal_form::Control {
            first,
            end,
            line: first + 1,
            x: x as u16,
            w: widths[i] as u16,
            h: textbox::BOX_H as u16,
            hit: Hit::Control(item.3),
        });
        x += widths[i] + PICKER_GAP;
    }
}

/// A picker's box: `value` (after its status glyph, for Status) at the
/// left, `▾` at the right; lit while it has the keyboard or is open.
fn select_lines(
    value: &str,
    category: Option<shelbi_core::StatusCategory>,
    focused: bool,
    open: bool,
    disabled: bool,
    w: usize,
) -> [Line<'static>; 3] {
    let fill = textbox::fill(focused || open, disabled, false);
    let fill = if focused || open { fill } else { palette().dialog_control };
    let (above, below) = textbox::caps(w, fill);
    let on = Style::default().bg(fill);
    let fg = if disabled {
        palette().muted
    } else if focused {
        palette().white
    } else {
        palette().foreground
    };
    let mut spans = vec![Span::styled(" ".repeat(textbox::INSET), on)];
    let mut used = textbox::INSET;
    if let Some(c) = category {
        spans.push(Span::styled(format!("{} ", category_glyph(c)), on.fg(category_color(c))));
        used += 2;
    }
    let room = w.saturating_sub(used + 2 + textbox::INSET);
    let label = panel::truncate_right(value, room);
    used += panel::display_width(&label);
    spans.push(Span::styled(label, on.fg(fg)));
    spans.push(Span::styled(" ".repeat(w.saturating_sub(used + 1 + textbox::INSET)), on));
    spans.push(Span::styled("▾", on.fg(if disabled { palette().muted } else { palette().gray })));
    spans.push(Span::styled(" ".repeat(textbox::INSET), on));
    [above, Line::from(spans), below]
}

/// "Let Orchestrator write this up": a card in the fields' fill with the
/// label, the switch at its right, and the help wrapped under the label.
fn write_up(out: &mut FormLines<Hit>, m: &NewTaskModel) {
    let width = out.width;
    let focused = m.focus == Field::WriteUp;
    let fill = if focused { palette().input_focus } else { palette().dialog_control };
    let on = Style::default().bg(fill);
    let switch = if m.write_up { "( ● )" } else { "( ○ )" };
    let switch_style = if m.write_up {
        on.fg(palette().cyan).add_modifier(Modifier::BOLD)
    } else {
        on.fg(palette().gray)
    };
    let inset = textbox::INSET + 1;
    let text_w = width.saturating_sub(2 * inset + panel::display_width(switch) + 2).max(1);
    let label_style = if focused {
        on.fg(palette().white).add_modifier(Modifier::BOLD)
    } else {
        on.fg(palette().white)
    };
    let mut lines = Vec::new();
    let (above, below) = textbox::caps(width, fill);
    lines.push(above);
    let label = panel::truncate_right(model::WRITE_UP, text_w);
    let gap = width.saturating_sub(inset + panel::display_width(&label) + panel::display_width(switch) + inset);
    lines.push(Line::from(vec![
        Span::styled(" ".repeat(inset), on),
        Span::styled(label, label_style),
        Span::styled(" ".repeat(gap), on),
        Span::styled(switch, switch_style),
        Span::styled(" ".repeat(inset), on),
    ]));
    for part in wrap(model::WRITE_UP_HELP, text_w) {
        let used = inset + panel::display_width(&part);
        lines.push(Line::from(vec![
            Span::styled(" ".repeat(inset), on),
            Span::styled(part, on.fg(palette().muted)),
            Span::styled(" ".repeat(width.saturating_sub(used)), on),
        ]));
    }
    lines.push(below);
    out.rows(lines, Hit::Control(Field::WriteUp));
}

/// The footer: the create hint at the left, Cancel and Create Task (Primary,
/// disabled while there's no title; "Creating…" while it's out) at the right.
fn footer(out: &mut FormLines<Hit>, m: &NewTaskModel) {
    let width = out.width;
    let label = if m.creating { model::CREATING } else { model::CREATE };
    let cancel = Button::secondary(model::CANCEL).focused(m.focus == Field::Cancel && !m.creating).enabled(!m.creating);
    let create = Button::primary(label).focused(m.focus == Field::Create && m.can_create).enabled(m.can_create);
    let buttons = [cancel, create];
    let rw = button::row_width(&buttons);
    let hint = model::CREATE_HINT_TERMINAL;
    if rw + panel::display_width(hint) + 2 > width {
        // No room for the hint beside them: the buttons alone.
        return out.buttons(&[(cancel, Hit::Control(Field::Cancel)), (create, Hit::Control(Field::Create))], button::Align::Right);
    }
    let (right, at) = button::row(&buttons, rw, button::Align::Left);
    let muted = Style::default().fg(palette().muted);
    let left = [Line::raw(""), Line::from(Span::styled(hint, muted)), Line::raw("")];
    let first = out.lines.len();
    out.lines.extend(button::beside(left, right, width, rw));
    let offset = width - rw;
    for ((x, w), field) in at.into_iter().zip([Field::Cancel, Field::Create]) {
        out.controls.push(crate::modal_form::Control {
            first,
            end: first + button::BUTTON_H,
            line: first,
            x: x + offset as u16,
            w,
            h: button::BUTTON_H as u16,
            hit: Hit::Control(field),
        });
    }
}

fn act(action: Action) -> Step {
    match action {
        Action::Stay => Step::Stay,
        Action::Create(req) => Step::Create(req),
        Action::Close => Step::Close,
    }
}

fn panel_width(area: Rect) -> u16 {
    PANEL_W.min(area.width.saturating_sub(2)).max(1)
}

fn content_width(panel_w: u16) -> u16 {
    panel_w.saturating_sub(2 * PAD_X).max(1)
}

/// The border's rows, the padding rows, and `lines` of content.
fn panel_height(lines: usize) -> u16 {
    (lines as u16).saturating_add(4)
}

#[cfg(test)]
mod tests;
