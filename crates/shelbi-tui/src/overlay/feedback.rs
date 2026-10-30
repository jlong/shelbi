//! Contact Us in the TUI (`tui-feedback-dialog`): the desktop's feedback
//! dialog, drawn in the TUI's half-block language. The sidebar footer's
//! `📣` and the palette's Send Feedback open it (the registry's
//! `action:send-feedback`).
//!
//! [`FeedbackForm`] (shared with the desktop, in `shelbi_app::feedback`)
//! holds the draft and decides everything, the words included; this paints
//! it, keeps three [`TextArea`]s in step with it for typing, and hands the
//! shell a [`Submission`] to send off the UI thread. The shell hands the
//! answer back ([`FeedbackDialog::finished`]).
//!
//! The panel is filled with [`Theme::dialog`] in the dialogs' muted
//! single-line border ([`super::dialog_frame`]). Inside, in the desktop's order: the
//! megaphone on its tinted badge and "Contact Us", centered; Name and Email
//! (placeholders, no labels, as the desktop's); the kind's segmented control
//! (Bug Report, Feature Request, Other); Message; the "Include system info"
//! checkbox; then Send Message and Cancel, full width. A send that worked
//! swaps in the thank-you: the heart, "Thank you!", its line, and Close.
//!
//! ```text
//!  ┌──────────────────────────────────────┐
//!  │                 ▄▄▄▄                 │
//!  │                  📣                  │
//!  │                 ▀▀▀▀                 │
//!  │              Contact Us              │
//!  │                                      │
//!  │ ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄ │
//!  │  Ada                                 │
//!  │ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀ │
//!  │ ...                                  │
//!  └──────────────────────────────────────┘
//! ```
//!
//! A short terminal drops the badge, then the panel's padding rows, and
//! past that scrolls the fields under the pinned error and buttons
//! ([`FormLines`]) so the focused control stays in view.
//!
//! Keys, as the desktop's: Tab / ⇧Tab walk the controls in visual order and
//! never leave the dialog; Return is Send Message (on Cancel, Cancel; in
//! Message, a new line, and ⌘Return or Ctrl+Return sends from there); Esc
//! cancels; Space presses the focused button or toggles the checkbox;
//! ← / → pick the kind on the segmented control. A click works on every
//! control; a paste goes into the text field being typed in.
//!
//! [`Theme::dialog`]: shelbi_app::theme::Theme::dialog

use crossterm::event::KeyEvent;
use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    Frame,
};
use shelbi_app::feedback::form::{self, Action, FeedbackForm, Focus, Key};
use shelbi_app::feedback::{FeedbackError, Kind, Remembered, Submission};
use shelbi_app::overlay::error_log::wrap;
use shelbi_app::text_area::TextArea;
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::centered_rect;
use crate::button::Button;
use crate::modal_form::{Control, FormLines};
use crate::panel;
use crate::sidebar::{BLEED_ABOVE, BLEED_BELOW};
use crate::textbox::{self, Textbox};
use crate::theme::palette;

/// The panel's width (the desktop's 440pt), less on a narrower terminal.
pub const PANEL_W: u16 = 56;
/// The columns between the panel's edge and its content: the border and
/// the dialogs' padding inside it.
const PAD_X: u16 = 1 + super::DIALOG_PAD_X;
/// The Message field's height in lines: it grows from the first to the
/// second with its text, then scrolls (the desktop's).
const MESSAGE_LINES: (usize, usize) = (5, 9);
/// The megaphone's and the heart's badge.
const MEGAPHONE: &str = "📣";
const HEART: &str = "♥";

/// What the shell does next.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    Stay,
    /// Send this off the UI thread and hand the answer to
    /// [`FeedbackDialog::finished`].
    Send(Submission),
    /// Close the dialog: cancelled, or after the thank-you.
    Close,
}

/// What a click lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hit {
    Control(Focus),
    Kind(Kind),
}

/// How much of the panel's chrome fits: the badge and the padding rows go
/// first on a short terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Fit {
    Full,
    NoBadge,
    Tight,
}

impl Fit {
    fn badge(self) -> bool {
        self == Fit::Full
    }

    fn pad_y(self) -> u16 {
        u16::from(self != Fit::Tight)
    }
}

/// The open dialog.
pub struct FeedbackDialog {
    form: FeedbackForm,
    name: TextArea,
    email: TextArea,
    message: TextArea,
    /// The first of Message's wrapped lines in view.
    message_top: usize,
    /// The terminal draws emoji: the megaphone's badge shows.
    emoji: bool,
    /// Where each control was drawn last, for clicks.
    hits: Vec<(Rect, Hit)>,
}

/// A single-line field's buffer holding `text`, the caret at its end.
fn line_buffer(text: &str) -> TextArea {
    let mut t = TextArea::from_text(text);
    t.end();
    t
}

impl FeedbackDialog {
    /// A new draft from the name and email remembered.
    pub fn new(remembered: Remembered, emoji: bool) -> Self {
        let form = FeedbackForm::new(remembered);
        FeedbackDialog {
            name: line_buffer(form.name()),
            email: line_buffer(form.email()),
            message: TextArea::default(),
            message_top: 0,
            emoji,
            hits: Vec::new(),
            form,
        }
    }

    pub fn form(&self) -> &FeedbackForm {
        &self.form
    }

    /// The send came back: the thank-you, or the error under the form with
    /// the draft kept.
    pub fn finished(&mut self, result: Result<(), FeedbackError>) {
        self.form.finished(result);
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
        let in_text = !self.form.sent() && focus.is_text();
        let key = match chord.code {
            Code::Tab if bare => Some(Key::Tab),
            Code::Tab if chord.mods == Mods::SHIFT => Some(Key::BackTab),
            Code::BackTab => Some(Key::BackTab),
            // A terminal rarely delivers ⌘, so Ctrl+Return sends too.
            Code::Enter if chord.mods == Mods::SUPER || chord.mods == Mods::CONTROL => Some(Key::CmdEnter),
            // Return in the message is its new line.
            Code::Enter if bare && in_text && focus == Focus::Message => None,
            Code::Enter if bare => Some(Key::Enter),
            Code::Esc if bare => Some(Key::Esc),
            Code::Char(' ') if bare && !in_text => Some(Key::Space),
            Code::Left if bare && !in_text => Some(Key::Left),
            Code::Right if bare && !in_text => Some(Key::Right),
            _ => None,
        };
        match key {
            Some(key) => {
                let action = self.form.key(key);
                act(action)
            }
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
        match self.form.focus() {
            _ if self.form.sent() => None,
            Focus::Name => Some((&mut self.name, true)),
            Focus::Email => Some((&mut self.email, true)),
            Focus::Message => Some((&mut self.message, false)),
            _ => None,
        }
    }

    fn edit(&mut self, chord: KeyChord) {
        let page = MESSAGE_LINES.0;
        if let Some((text, single)) = self.field() {
            if text.edit(chord, single, page) {
                self.sync();
            }
        }
    }

    /// Bracketed paste into the text field being typed in (a single-line
    /// field takes it on one line). `false` when no field has the keyboard.
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

    /// Hand the fields' text to the form (which clears an error it shows
    /// when one changed).
    fn sync(&mut self) {
        self.form.set_name(&self.name.text());
        self.form.set_email(&self.email.text());
        self.form.set_message(&self.message.text());
    }

    /// A left click at a screen cell.
    pub fn handle_click(&mut self, col: u16, row: u16) -> Step {
        let at = |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        // The last drawn wins: a segment over its track.
        let Some(&(_, hit)) = self.hits.iter().rev().find(|(r, _)| at(r)) else {
            return Step::Stay;
        };
        match hit {
            Hit::Kind(kind) => {
                self.form.pick(kind);
                Step::Stay
            }
            Hit::Control(Focus::SystemInfo) => {
                self.form.toggle_system_info();
                Step::Stay
            }
            Hit::Control(focus @ (Focus::Send | Focus::Cancel | Focus::Close)) => act(self.form.press(focus)),
            Hit::Control(focus) => {
                self.form.set_focus(focus);
                Step::Stay
            }
        }
    }

    /// Where the last draw put the controls that aren't buttons (the
    /// fields, the kind's segmented control). In light their fill is a
    /// button's, so the shell's hover must not take them for buttons.
    pub fn not_buttons(&self) -> Vec<Rect> {
        self.hits
            .iter()
            .filter(|(_, hit)| !matches!(hit, Hit::Control(Focus::Send | Focus::Cancel | Focus::Close)))
            .map(|(r, _)| *r)
            .collect()
    }

    /// The panel's rect in `area` (the whole window): centered, as tall as
    /// its content, never taller than `area`.
    pub fn rect(&self, area: Rect) -> Rect {
        let width = panel_width(area);
        let (fit, lines) = self.fit(width, area.height);
        centered_rect(area, width, panel_height(lines, fit))
    }

    /// The most chrome that fits `height` rows, and the content's lines.
    fn fit(&self, width: u16, height: u16) -> (Fit, usize) {
        let content_w = content_width(width) as usize;
        for fit in [Fit::Full, Fit::NoBadge] {
            let lines = self.lines(content_w, fit).lines.len();
            if panel_height(lines, fit) <= height {
                return (fit, lines);
            }
        }
        (Fit::Tight, self.lines(content_w, Fit::Tight).lines.len())
    }

    /// Draw the panel into `rect` ([`Self::rect`]'s).
    pub fn render(&mut self, f: &mut Frame, rect: Rect) {
        self.hits.clear();
        if rect.width < 8 || rect.height < 4 {
            return;
        }
        super::dialog_frame(f.buffer_mut(), rect, Some(palette().dialog), None);
        let body = Rect { y: rect.y + 1, height: rect.height - 2, ..rect };
        let (fit, _) = self.fit(rect.width, rect.height);
        let pad = fit.pad_y().min(body.height / 2);
        let content = Rect {
            x: body.x + PAD_X,
            y: body.y + pad,
            width: content_width(rect.width),
            height: body.height - 2 * pad,
        };
        let out = self.lines(content.width as usize, fit);
        self.message_top = self.message_window(content.width as usize).0;
        let focus = Hit::Control(self.form.focus());
        out.render(f, content, focus, &mut self.hits);
    }

    /// Message's wrapped lines in view: the first, and how many.
    fn message_window(&self, width: usize) -> (usize, usize) {
        let text_w = textbox::room(width).max(1);
        let (rows, (caret, _)) = self.message.wrapped(text_w);
        let n = rows.len().clamp(MESSAGE_LINES.0, MESSAGE_LINES.1);
        let mut top = self.message_top.min(rows.len().saturating_sub(1));
        if self.form.focus() == Focus::Message {
            if caret < top {
                top = caret;
            } else if caret >= top + n {
                top = caret + 1 - n;
            }
        }
        (top, n)
    }

    /// The content's lines `width` wide, with each control's place.
    fn lines(&self, width: usize, fit: Fit) -> FormLines<Hit> {
        let mut out = FormLines::new(width);
        if self.form.sent() {
            self.thanks(&mut out, fit);
        } else {
            self.draft(&mut out, fit);
        }
        out
    }

    fn draft(&self, out: &mut FormLines<Hit>, fit: Fit) {
        let width = out.width;
        let form = &self.form;
        let focus = form.focus();
        if fit.badge() && self.emoji {
            out.lines.extend(badge(MEGAPHONE, palette().primary, palette().primary_label, width));
        }
        out.lines.push(centered(form::TITLE, title_style(), width));
        out.blank();
        let rest = palette().dialog_control;
        // Name, then Email: their placeholders say what they are.
        for (hit, text, buffer, placeholder, invalid) in [
            (Focus::Name, form.name(), &self.name, form::NAME, false),
            (Focus::Email, form.email(), &self.email, form::EMAIL, form.error() == Some(form::NO_EMAIL)),
        ] {
            let on = focus == hit;
            let field = Textbox::new(text).placeholder(placeholder).rest(rest).focused(on).invalid(invalid);
            let field = if on { field.caret(Some(buffer.cursor().1)) } else { field };
            out.rows(field.lines(width), Hit::Control(hit));
        }
        self.kinds(out);
        self.message_box(out);
        self.system_info(out);
        // The error and the buttons stay in view under fields that scroll.
        out.footer();
        if let Some(error) = form.error() {
            out.text(error, Style::default().fg(palette().red));
        }
        out.blank();
        // The desktop's stacked, full-width Send Message (Primary) and
        // Cancel, as Settings' buttons draw (focus, hover, disabled alike).
        let sending = form.sending();
        let label = if sending { form::SENDING } else { form::SEND };
        let send = Button::primary(label).focused(focus == Focus::Send && !sending).enabled(!sending);
        out.rows(send.lines_in(width), Hit::Control(Focus::Send));
        let cancel = Button::secondary(form::CANCEL).focused(focus == Focus::Cancel && !sending).enabled(!sending);
        out.rows(cancel.lines_in(width), Hit::Control(Focus::Cancel));
    }

    /// Bug Report / Feature Request / Other: a track the width of the form
    /// (a textbox's caps and fill), the picked one a chip in it.
    fn kinds(&self, out: &mut FormLines<Hit>) {
        let width = out.width;
        let focused = self.form.focus() == Focus::Kind;
        let track = if focused { palette().input_focus } else { palette().dialog_control };
        let (above, below) = textbox::caps(width, track);
        let inner = width.saturating_sub(2 * textbox::INSET);
        let on_track = Style::default().bg(track);
        let mut spans = vec![Span::styled(" ".repeat(textbox::INSET.min(width)), on_track)];
        let mut segments = Vec::new();
        let mut x = textbox::INSET;
        for (i, kind) in Kind::ALL.into_iter().enumerate() {
            let n = Kind::ALL.len();
            let w = inner / n + usize::from(i < inner % n);
            let picked = kind == self.form.kind();
            let style = if picked {
                Style::default().bg(palette().segment).fg(palette().white).add_modifier(Modifier::BOLD)
            } else {
                on_track.fg(palette().gray)
            };
            spans.push(Span::styled(center(kind.label(), w), style));
            segments.push((x, w, kind));
            x += w;
        }
        let used: usize = spans.iter().map(|s| panel::display_width(&s.content)).sum();
        spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), on_track));
        let first = out.lines.len();
        out.rows([above, Line::from(spans), below], Hit::Control(Focus::Kind));
        // Each segment answers across the control's three rows.
        for (x, w, kind) in segments {
            out.controls.push(Control { first, end: first + 3, line: first, x: x as u16, w: w as u16, h: 3, hit: Hit::Kind(kind) });
        }
    }

    /// Message: its wrapped lines on the fill, the caret's kept in view
    /// while it's typed in, the placeholder while it's empty.
    fn message_box(&self, out: &mut FormLines<Hit>) {
        let width = out.width;
        let focused = self.form.focus() == Focus::Message;
        let fill = if focused { palette().input_focus } else { palette().dialog_control };
        let style = textbox::area_text(focused, false).bg(fill);
        let text_w = textbox::room(width).max(1);
        let (rows, (caret_row, caret_col)) = self.message.wrapped(text_w);
        let (top, n) = self.message_window(width);
        let lines = self.message.lines();
        let mut body = Vec::new();
        for v in top..top + n {
            let mut spans = Vec::new();
            if let Some(&(line, start)) = rows.get(v) {
                let chars: Vec<char> =
                    lines[line].iter().skip(start).take(text_w).map(|c| if c.is_control() { ' ' } else { *c }).collect();
                let caret = (focused && v == caret_row).then_some(caret_col);
                let used = textbox::caret_spans(&mut spans, &chars, caret, text_w, style);
                if v == 0 && self.form.message().is_empty() {
                    let hint = panel::truncate_right(form::MESSAGE, text_w.saturating_sub(used));
                    spans.push(Span::styled(hint, Style::default().fg(palette().muted).bg(fill)));
                }
            }
            body.push(spans);
        }
        out.rows(textbox::area_in(body, fill, width), Hit::Control(Focus::Message));
    }

    /// "Include system info": the box (checked, the accent with a `✓`),
    /// then its label, wrapped under itself.
    fn system_info(&self, out: &mut FormLines<Hit>) {
        let width = out.width;
        let on = self.form.include_system_info();
        let focused = self.form.focus() == Focus::SystemInfo;
        let check = if on {
            Span::styled(" ✓ ", Style::default().bg(palette().cyan).fg(palette().canvas).add_modifier(Modifier::BOLD))
        } else {
            let fill = if focused { palette().input_focus } else { palette().dialog_control };
            Span::styled("   ", Style::default().bg(fill))
        };
        let label = if focused {
            Style::default().fg(palette().cyan).add_modifier(Modifier::BOLD)
        } else if on {
            Style::default().fg(palette().white)
        } else {
            Style::default().fg(palette().foreground)
        };
        const INDENT: usize = 4;
        let mut lines = Vec::new();
        for (i, part) in wrap(form::system_info_label(), width.saturating_sub(INDENT).max(1)).into_iter().enumerate() {
            let lead = if i == 0 { check.clone() } else { Span::raw("   ") };
            lines.push(Line::from(vec![lead, Span::raw(" "), Span::styled(part, label)]));
        }
        out.blank();
        out.rows(lines, Hit::Control(Focus::SystemInfo));
    }

    fn thanks(&self, out: &mut FormLines<Hit>, fit: Fit) {
        let width = out.width;
        if fit.badge() {
            out.lines.extend(badge(HEART, palette().magenta_tint, palette().ansi_magenta, width));
        }
        out.lines.push(centered(form::THANKS_TITLE, title_style(), width));
        for line in form::THANKS.lines() {
            for part in wrap(line, width.max(1)) {
                out.lines.push(centered(&part, Style::default().fg(palette().muted), width));
            }
        }
        out.footer();
        out.blank();
        // Close has the keys (the only control): a focused Secondary button.
        out.rows(Button::secondary(form::CLOSE).focused(true).lines_in(width), Hit::Control(Focus::Close));
    }
}

fn act(action: Action) -> Step {
    match action {
        Action::Stay => Step::Stay,
        Action::Send(submission) => Step::Send(submission),
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
fn panel_height(lines: usize, fit: Fit) -> u16 {
    (lines as u16).saturating_add(2 + 2 * fit.pad_y())
}

/// The title's style: the strongest text, bold.
fn title_style() -> Style {
    Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
}

/// `text` centered in `width` columns (cut to fit).
fn center(text: &str, width: usize) -> String {
    let text = panel::truncate_right(text, width);
    let w = panel::display_width(&text);
    let left = width.saturating_sub(w) / 2;
    format!("{}{text}{}", " ".repeat(left), " ".repeat(width.saturating_sub(w + left)))
}

fn centered(text: &str, style: Style, width: usize) -> Line<'static> {
    let text = panel::truncate_right(text, width);
    let left = width.saturating_sub(panel::display_width(&text)) / 2;
    Line::from(vec![Span::raw(" ".repeat(left)), Span::styled(text, style)])
}

/// The header's icon on its tint, the caps above and below, centered.
fn badge(glyph: &str, fill: Color, color: Color, width: usize) -> [Line<'static>; 3] {
    // About as wide as it's tall: a two-cell emoji gets a column each side,
    // a one-cell glyph two.
    let pad = if panel::display_width(glyph) > 1 { " " } else { "  " };
    let face = format!("{pad}{glyph}{pad}");
    let w = panel::display_width(&face);
    let left = " ".repeat(width.saturating_sub(w) / 2);
    let cap = |g: &str| Line::from(vec![Span::raw(left.clone()), Span::styled(g.repeat(w), Style::default().fg(fill))]);
    [
        cap(BLEED_ABOVE),
        Line::from(vec![Span::raw(left.clone()), Span::styled(face, Style::default().bg(fill).fg(color))]),
        cap(BLEED_BELOW),
    ]
}

#[cfg(test)]
mod tests;
