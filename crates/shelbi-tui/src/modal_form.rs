//! The modal forms' layout (Settings' workspace and machine forms, and
//! Contact Us): each field its label over its textbox with what's said
//! under it, a blank row between fields, laid out as lines with each
//! control's click target and block recorded, so a short terminal scrolls
//! to keep the focused control (and what's said under it) in view.

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::button::{self, Align, Button};
use crate::panel;
use crate::textbox::{self, Textbox};

/// The narrowest form that sets a [`FormLines::pair`] side by side.
pub(crate) const PAIR_MIN_W: usize = 56;

/// The columns between a [`FormLines::pair`]'s fields.
pub(crate) const PAIR_GAP: usize = 2;

/// Where a [`FormLines::pair`]'s right field starts in a form `width` wide,
/// and its width (`None`: the pair stacks).
pub(crate) fn pair_right(width: usize) -> Option<(usize, usize)> {
    (width >= PAIR_MIN_W).then(|| {
        let left_w = (width - PAIR_GAP) / 2;
        (left_w + PAIR_GAP, width - PAIR_GAP - left_w)
    })
}

/// One field of a [`FormLines::pair`].
pub(crate) struct Field<'a, H> {
    pub label: &'a str,
    pub textbox: Textbox<'a>,
    pub focused: bool,
    pub disabled: bool,
    pub under: Vec<(&'a str, Style)>,
    pub hit: H,
}

/// A dialog's heading as its first lines: the title bold and loud, what it
/// does wrapped under it in gray, then a blank row (the desktop dialog's
/// header).
pub(crate) fn dialog_heading(title: &str, description: Option<&str>, width: usize) -> Vec<Line<'static>> {
    let palette = crate::theme::palette();
    let mut lines = vec![Line::from(Span::styled(
        panel::truncate_right(title, width),
        Style::default().fg(palette.white).add_modifier(Modifier::BOLD),
    ))];
    if let Some(d) = description {
        lines.extend(
            shelbi_app::overlay::error_log::wrap(d, width.max(1))
                .into_iter()
                .map(|l| Line::from(Span::styled(l, Style::default().fg(palette.gray)))),
        );
    }
    lines.push(Line::raw(""));
    lines
}

/// A field's lines `width` wide: its label, its box (at rest in `rest`,
/// when the form gives one), then `under` wrapped.
fn field_lines(label: &str, field: Textbox<'_>, focused: bool, disabled: bool, under: &[(&str, Style)], width: usize, rest: Option<Color>) -> Vec<Line<'static>> {
    let mut lines = vec![textbox::label(label, focused, disabled, width)];
    let field = match rest {
        Some(fill) => field.rest(fill),
        None => field,
    };
    lines.extend(field.lines(width));
    for (text, style) in under {
        for l in shelbi_app::overlay::error_log::wrap(text, width.max(1)) {
            lines.push(Line::from(Span::styled(l, *style)));
        }
    }
    lines
}

/// A control's place in a form's lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Control<H> {
    /// The lines it and what's said under it span: `first..end`.
    pub first: usize,
    pub end: usize,
    /// Its click target: `h` lines from `line`, `w` columns from `x`.
    pub line: usize,
    pub x: u16,
    pub w: u16,
    pub h: u16,
    pub hit: H,
}

/// A form's content as lines `width` columns wide, and its controls.
pub(crate) struct FormLines<H> {
    pub lines: Vec<Line<'static>>,
    pub controls: Vec<Control<H>>,
    pub width: usize,
    /// Where the footer (the buttons and the key hint) starts: a form
    /// taller than its room keeps it pinned under the scrolling fields.
    footer: Option<usize>,
    /// Its fields' fill at rest (a Settings dialog's wells), when not the
    /// text field's own.
    rest: Option<Color>,
}

impl<H: Copy + PartialEq> FormLines<H> {
    pub(crate) fn new(width: usize) -> Self {
        FormLines { lines: Vec::new(), controls: Vec::new(), width, footer: None, rest: None }
    }

    /// Its fields at rest in `fill` (a Settings dialog's wells on the
    /// overlay's panel).
    pub(crate) fn with_rest(mut self, fill: Color) -> Self {
        self.rest = Some(fill);
        self
    }

    pub(crate) fn blank(&mut self) {
        self.lines.push(Line::raw(""));
    }

    /// The dialog's heading: its title, what it's for, a blank row.
    pub(crate) fn heading(&mut self, title: &str, description: &str) {
        let desc = (!description.is_empty()).then_some(description);
        self.lines.extend(dialog_heading(title, desc, self.width));
    }

    /// A rule across the form (above a dialog's buttons).
    pub(crate) fn rule(&mut self) {
        let style = Style::default().fg(crate::theme::palette().table_rule);
        self.lines.push(Line::from(Span::styled("─".repeat(self.width), style)));
    }

    /// `text` wrapped to the form's width.
    pub(crate) fn text(&mut self, text: &str, style: Style) {
        for l in shelbi_app::overlay::error_log::wrap(text, self.width.max(1)) {
            self.lines.push(Line::from(Span::styled(l, style)));
        }
    }

    /// A field: `label` over `field`'s box, then each of `under` wrapped.
    /// The box (all three of its rows) is `hit`'s click target.
    pub(crate) fn field(&mut self, label: &str, field: Textbox<'_>, focused: bool, disabled: bool, under: &[(&str, Style)], hit: H) {
        let first = self.lines.len();
        self.lines.extend(field_lines(label, field, focused, disabled, under, self.width, self.rest));
        self.controls.push(Control {
            first,
            end: self.lines.len(),
            line: first + 1,
            x: 0,
            w: self.width as u16,
            h: textbox::BOX_H as u16,
            hit,
        });
    }

    /// Two fields side by side, each half the width (a narrow form stacks
    /// them, a blank row between).
    pub(crate) fn pair(&mut self, left: Field<'_, H>, right: Field<'_, H>) {
        const GAP: usize = PAIR_GAP;
        if self.width < PAIR_MIN_W {
            self.field(left.label, left.textbox, left.focused, left.disabled, &left.under, left.hit);
            self.blank();
            self.field(right.label, right.textbox, right.focused, right.disabled, &right.under, right.hit);
            return;
        }
        let left_w = (self.width - GAP) / 2;
        let right_w = self.width - GAP - left_w;
        let a = field_lines(left.label, left.textbox, left.focused, left.disabled, &left.under, left_w, self.rest);
        let b = field_lines(right.label, right.textbox, right.focused, right.disabled, &right.under, right_w, self.rest);
        let first = self.lines.len();
        for i in 0..a.len().max(b.len()) {
            let mut spans: Vec<Span<'static>> = Vec::new();
            let left_line = a.get(i).cloned().unwrap_or_default();
            let used: usize = left_line.spans.iter().map(|s| panel::display_width(&s.content)).sum();
            spans.extend(left_line.spans);
            spans.push(Span::raw(" ".repeat(left_w.saturating_sub(used) + GAP)));
            spans.extend(b.get(i).cloned().unwrap_or_default().spans);
            self.lines.push(Line::from(spans));
        }
        let end = self.lines.len();
        let h = textbox::BOX_H as u16;
        self.controls.push(Control { first, end, line: first + 1, x: 0, w: left_w as u16, h, hit: left.hit });
        self.controls.push(Control { first, end, line: first + 1, x: (left_w + GAP) as u16, w: right_w as u16, h, hit: right.hit });
    }

    /// A one-line control at `x`, `w` wide, on the line about to be pushed
    /// (or pushed last, with `line`); its block runs from `first`.
    pub(crate) fn control(&mut self, first: usize, line: usize, x: usize, w: usize, hit: H) {
        self.controls.push(Control { first, end: line + 1, line, x: x as u16, w: w as u16, h: 1, hit });
    }

    /// `lines` (a box, a button, a checkbox), all of them `hit`'s click
    /// target across the form's width.
    pub(crate) fn rows(&mut self, lines: impl IntoIterator<Item = Line<'static>>, hit: H) {
        let first = self.lines.len();
        self.lines.extend(lines);
        let end = self.lines.len();
        let h = (end - first) as u16;
        self.controls.push(Control { first, end, line: first, x: 0, w: self.width as u16, h, hit });
    }

    /// A row of buttons (each with its click target) set to `align`.
    pub(crate) fn buttons(&mut self, buttons: &[(Button<'_>, H)], align: Align) {
        let shown: Vec<Button> = buttons.iter().map(|(b, _)| *b).collect();
        // Too narrow for one row, they wrap to another.
        for (lines, at) in button::bands(&shown, self.width, align) {
            let first = self.lines.len();
            self.lines.extend(lines);
            for (x, w, i) in at {
                let h = button::BUTTON_H;
                self.controls.push(Control { first, end: first + h, line: first, x, w, h: h as u16, hit: buttons[i].1 });
            }
        }
    }

    /// Buttons at the left and at the right of one row (a dialog's
    /// destructive action apart from its Cancel and confirm).
    pub(crate) fn buttons_apart(&mut self, left: &[(Button<'_>, H)], right: &[(Button<'_>, H)]) {
        if left.is_empty() {
            return self.buttons(right, Align::Right);
        }
        let l: Vec<Button> = left.iter().map(|(b, _)| *b).collect();
        let r: Vec<Button> = right.iter().map(|(b, _)| *b).collect();
        let (lw, rw) = (button::row_width(&l), button::row_width(&r));
        if lw + rw + 2 > self.width {
            // No room for both on one row: the right ones first.
            self.buttons(right, Align::Right);
            self.blank();
            return self.buttons(left, Align::Left);
        }
        let (left_lines, left_at) = button::row(&l, lw, Align::Left);
        let (right_lines, right_at) = button::row(&r, rw, Align::Left);
        let first = self.lines.len();
        self.lines.extend(button::beside(left_lines, right_lines, self.width, rw));
        let offset = (self.width - rw) as u16;
        let at = left_at.into_iter().chain(right_at.into_iter().map(|(x, w)| (x + offset, w)));
        for ((x, w), (_, hit)) in at.zip(left.iter().chain(right)) {
            self.controls.push(Control { first, end: first + button::BUTTON_H, line: first, x, w, h: button::BUTTON_H as u16, hit: *hit });
        }
    }

    /// The lines from here on are the footer.
    pub(crate) fn footer(&mut self) {
        self.footer = Some(self.lines.len());
    }

    /// The lines to skip so `focus`'s block shows in `height` rows: its end
    /// when it fits, else its start. Focus in a pinned footer shows the
    /// fields' end.
    pub(crate) fn scroll(&self, focus: H, height: usize) -> usize {
        let Some(c) = self.controls.iter().find(|c| c.hit == focus) else {
            return 0;
        };
        if self.footer.is_some_and(|at| c.line >= at) {
            return self.footer.unwrap_or(0).saturating_sub(height);
        }
        if c.end <= height {
            0
        } else if c.end - c.first <= height {
            c.end - height
        } else {
            c.first
        }
    }

    /// Draw the lines into `content`, scrolled to keep `focus` in view (the
    /// footer pinned to the bottom when they don't all fit), and record
    /// each control's (visible) click target in `hits`.
    pub(crate) fn render(mut self, f: &mut Frame, content: Rect, focus: H, hits: &mut Vec<(Rect, H)>) {
        let height = content.height as usize;
        // The footer pins only when the fields keep a few rows of their own.
        let at = self.footer.filter(|at| self.lines.len() > height && self.lines.len() - at + 4 <= height);
        let (body_h, footer) = match at {
            Some(at) => {
                let footer: Vec<Line<'static>> = self.lines.split_off(at);
                (height - footer.len(), footer)
            }
            None => {
                self.footer = None;
                (height, Vec::new())
            }
        };
        let skip = self.scroll(focus, body_h);
        let body = Rect { height: body_h as u16, ..content };
        f.render_widget(Paragraph::new(self.lines.into_iter().skip(skip).collect::<Vec<_>>()), body);
        let footer_y = content.y + body_h as u16;
        f.render_widget(Paragraph::new(footer), Rect { y: footer_y, height: content.height - body_h as u16, ..content });
        for c in self.controls {
            for row in c.line..c.line + c.h as usize {
                let y = match at {
                    Some(at) if row >= at => footer_y + (row - at) as u16,
                    _ => match row.checked_sub(skip) {
                        Some(y) if y < body_h => content.y + y as u16,
                        _ => continue,
                    },
                };
                hits.push((Rect { x: content.x + c.x, y, width: c.w.max(1), height: 1 }, c.hit));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrolling_keeps_the_focused_block_in_view() {
        let mut form: FormLines<u8> = FormLines::new(20);
        let control = |first, end, hit| Control { first, end, line: first, x: 0, w: 1, h: 1, hit };
        form.controls = vec![control(0, 3, 1), control(3, 9, 2), control(12, 13, 3)];
        assert_eq!(form.scroll(1, 5), 0);
        assert_eq!(form.scroll(2, 6), 3, "the block's end shows");
        assert_eq!(form.scroll(2, 4), 3, "too tall: its start");
        assert_eq!(form.scroll(3, 5), 8);
        // With the footer pinned from line 11, its button shows the fields'
        // end.
        form.footer = Some(11);
        assert_eq!(form.scroll(3, 5), 6);
        assert_eq!(form.scroll(2, 6), 3, "a field scrolls as before");
    }

    #[test]
    fn a_tall_form_pins_its_footer_under_the_scrolling_fields() {
        let mut form: FormLines<u8> = FormLines::new(10);
        for i in 0..12 {
            form.text(&format!("field {i}"), Style::default());
        }
        form.controls.push(Control { first: 0, end: 1, line: 0, x: 0, w: 5, h: 1, hit: 1 });
        form.footer();
        form.blank();
        form.text("[Save]", Style::default());
        form.control(13, 13, 0, 6, 2);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(10, 8)).unwrap();
        let mut hits = Vec::new();
        term.draw(|f| form.render(f, f.area(), 1, &mut hits)).unwrap();
        let buf = term.backend().buffer();
        let row = |y: u16| (0..10).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>();
        assert_eq!(row(0).trim_end(), "field 0");
        assert_eq!(row(5).trim_end(), "field 5", "six rows of fields");
        assert_eq!(row(6).trim_end(), "", "the footer's blank");
        assert_eq!(row(7).trim_end(), "[Save]", "the button at the bottom");
        assert!(hits.contains(&(Rect::new(0, 7, 6, 1), 2)), "{hits:?}");
    }

    #[test]
    fn a_field_is_its_label_its_box_and_what_is_said_under_it() {
        let mut form: FormLines<u8> = FormLines::new(16);
        form.field("Name", Textbox::new("alpha"), false, false, &[("Unique.", Style::default())], 7);
        let text: Vec<String> =
            form.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(text, ["Name", &"▄".repeat(16), " alpha          ", &"▀".repeat(16), "Unique."]);
        assert_eq!(form.controls, [Control { first: 0, end: 5, line: 1, x: 0, w: 16, h: 3, hit: 7 }]);
    }

    fn field<'a>(label: &'a str, text: &'a str, under: Vec<(&'a str, Style)>, hit: u8) -> Field<'a, u8> {
        Field { label, textbox: Textbox::new(text), focused: false, disabled: false, under, hit }
    }

    #[test]
    fn a_pair_sits_side_by_side_when_it_fits_and_stacks_when_it_does_not() {
        let mut form: FormLines<u8> = FormLines::new(60);
        form.pair(field("Name", "devbox", vec![], 1), field("SSH host", "", vec![("An alias.", Style::default())], 2));
        let text: Vec<String> =
            form.lines.iter().map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect()).collect();
        assert_eq!(text.len(), 5, "{text:#?}");
        assert!(text[0].starts_with("Name ") && text[0].contains("  SSH host"), "{text:?}");
        assert_eq!(panel::display_width(&text[2]), 60);
        assert!(text[4].starts_with(&" ".repeat(31)) && text[4].ends_with("An alias."), "{:?}", text[4]);
        assert_eq!(form.controls[0].w, 29);
        assert_eq!((form.controls[1].x, form.controls[1].w), (31, 29));
        let mut form: FormLines<u8> = FormLines::new(40);
        form.pair(field("Name", "devbox", vec![], 1), field("SSH host", "", vec![], 2));
        assert_eq!(form.lines.len(), 9, "stacked, a blank between");
        assert_eq!(form.controls[1].line, 6);
    }
}
