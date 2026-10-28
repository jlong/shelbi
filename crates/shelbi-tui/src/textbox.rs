//! The settings screens' text field, drawn the way the sidebar's search box
//! is: a `▄` row above, the text on a solid fill, a `▀` row below. The caps'
//! glyphs carry the fill as their foreground over the terminal's default
//! background, so the box reads as a filled bar with half-cell padding above
//! and below, and no border.
//!
//! ```text
//!   Name
//!   ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
//!    alpha█
//!   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
//!   Workspace names are unique per project.
//! ```
//!
//! A field is its label over the box ([`label`]), the box's three rows
//! ([`Textbox::lines`]), and what's said under it (help, or an error in
//! red). The state shows in the fill: [`Theme::input`] at rest,
//! [`Theme::input_focus`] while it has the keyboard, [`Theme::input_invalid`]
//! while its value is refused, [`Theme::input_disabled`] (text muted) when
//! it can't be changed. A multi-line field ([`area`]) wraps the same fill
//! and caps around its whole text.
//!
//! The pages lay their rows out as lines, so the widget hands back lines;
//! each page marks all three of a box's rows as the field's click target.
//!
//! [`Theme::input`]: shelbi_app::theme::Theme::input
//! [`Theme::input_focus`]: shelbi_app::theme::Theme::input_focus
//! [`Theme::input_invalid`]: shelbi_app::theme::Theme::input_invalid
//! [`Theme::input_disabled`]: shelbi_app::theme::Theme::input_disabled

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};
use unicode_width::UnicodeWidthChar;

use crate::panel;
use crate::sidebar::{BLEED_ABOVE, BLEED_BELOW};
use crate::theme::palette;

/// The widest a page's field grows: a wide terminal doesn't stretch a name
/// across the screen. A modal's fields fill the modal.
pub(crate) const MAX_W: usize = 72;

/// Rows a box takes: the cap above, the text, the cap below.
pub(crate) const BOX_H: usize = 3;

/// The columns of fill each side of the text.
pub(crate) const INSET: usize = 1;

/// One text field's box.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Textbox<'a> {
    text: &'a str,
    /// The caret's position (in chars) while the field is typed in: the box
    /// draws it and scrolls to keep it in view.
    caret: Option<usize>,
    placeholder: &'a str,
    focused: bool,
    disabled: bool,
    invalid: bool,
    /// The whole text is selected (a suggestion typing replaces).
    selected: bool,
    /// Keep the text's end in view when it doesn't fit (a path).
    clip_left: bool,
    /// Drawn at the box's right edge (a picker's `▾`, a `read-only` note).
    trail: Option<&'a str>,
    /// The fill at rest in place of [`Theme::input`] (a field on a dialog's
    /// panel).
    ///
    /// [`Theme::input`]: shelbi_app::theme::Theme::input
    rest: Option<Color>,
}

impl<'a> Textbox<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        Textbox { text, ..Textbox::default() }
    }

    /// Typed in, the caret at char `at` (`None`: not typed in).
    pub(crate) fn caret(mut self, at: Option<usize>) -> Self {
        self.caret = at.map(|at| at.min(self.text.chars().count()));
        self
    }

    /// Typed in, the caret after the last char.
    pub(crate) fn caret_at_end(self) -> Self {
        let end = self.text.chars().count();
        self.caret(Some(end))
    }

    pub(crate) fn placeholder(mut self, text: &'a str) -> Self {
        self.placeholder = text;
        self
    }

    pub(crate) fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    pub(crate) fn disabled(mut self, on: bool) -> Self {
        self.disabled = on;
        self
    }

    pub(crate) fn invalid(mut self, on: bool) -> Self {
        self.invalid = on;
        self
    }

    pub(crate) fn selected(mut self, on: bool) -> Self {
        self.selected = on;
        self
    }

    pub(crate) fn clip_left(mut self, on: bool) -> Self {
        self.clip_left = on;
        self
    }

    pub(crate) fn trail(mut self, text: &'a str) -> Self {
        self.trail = Some(text);
        self
    }

    pub(crate) fn rest(mut self, fill: Color) -> Self {
        self.rest = Some(fill);
        self
    }

    /// Whether it's typed in (it has a caret).
    pub(crate) fn typing(&self) -> bool {
        self.caret.is_some()
    }

    /// The box's fill for its state.
    pub(crate) fn fill(&self) -> Color {
        let focused = self.focused || self.caret.is_some();
        match self.rest {
            Some(rest) if !focused && !self.disabled && !self.invalid => rest,
            _ => fill(focused, self.disabled, self.invalid),
        }
    }

    /// The box's three rows, `width` columns each.
    pub(crate) fn lines(&self, width: usize) -> [Line<'static>; 3] {
        let fill = self.fill();
        let (above, below) = caps(width, fill);
        [above, self.text_line(width, fill), below]
    }

    fn text_line(&self, width: usize, fill: Color) -> Line<'static> {
        let on_fill = Style::default().bg(fill);
        let text_style = on_fill.fg(if self.disabled {
            palette().muted
        } else if self.focused || self.caret.is_some() {
            palette().white
        } else {
            palette().foreground
        });
        let trail = self.trail.unwrap_or("");
        let trail_w = panel::display_width(trail);
        // The text's room: the box less its insets and the trail (with a
        // column's gap before it).
        let gap = usize::from(trail_w > 0);
        let room = width.saturating_sub(2 * INSET + trail_w + gap);
        let mut spans = vec![Span::styled(" ".repeat(INSET.min(width)), on_fill)];
        let mut used = 0usize;
        let placeholder = on_fill.fg(palette().muted);
        if room > 0 {
            match self.caret {
                Some(at) => {
                    let chars: Vec<char> = self.text.chars().collect();
                    used = caret_spans(&mut spans, &chars, Some(at), room, text_style);
                    if chars.is_empty() && used < room {
                        let hint = panel::truncate_right(self.placeholder, room - used);
                        used += panel::display_width(&hint);
                        spans.push(Span::styled(hint, placeholder));
                    }
                }
                None if self.text.is_empty() => {
                    let hint = panel::truncate_right(self.placeholder, room);
                    used = panel::display_width(&hint);
                    spans.push(Span::styled(hint, placeholder));
                }
                None => {
                    let shown = if self.clip_left {
                        panel::truncate_left(self.text, room)
                    } else {
                        panel::truncate_right(self.text, room)
                    };
                    used = panel::display_width(&shown);
                    let style = if self.selected {
                        Style::default().fg(palette().canvas).bg(palette().cyan)
                    } else {
                        text_style
                    };
                    spans.push(Span::styled(shown, style));
                }
            }
        }
        let inner = width.saturating_sub(INSET);
        let pad = inner.saturating_sub(INSET + used + trail_w);
        spans.push(Span::styled(" ".repeat(pad), on_fill));
        if trail_w > 0 && INSET + trail_w < width {
            spans.push(Span::styled(trail.to_string(), on_fill.fg(palette().muted)));
        }
        let drawn: usize = spans.iter().map(|s| panel::display_width(&s.content)).sum();
        if drawn < width {
            spans.push(Span::styled(" ".repeat(width - drawn), on_fill));
        }
        Line::from(spans)
    }
}

/// A text field's fill: a refusal tints it red, focus cyan (a read-only
/// field the keyboard is on too, so focus always shows); at rest a disabled
/// field is quieter.
pub(crate) fn fill(focused: bool, disabled: bool, invalid: bool) -> Color {
    if invalid {
        palette().input_invalid
    } else if focused {
        palette().input_focus
    } else if disabled {
        palette().input_disabled
    } else {
        palette().input
    }
}

/// The half-block caps above and below a box `width` wide in `fill`.
pub(crate) fn caps(width: usize, fill: Color) -> (Line<'static>, Line<'static>) {
    let style = Style::default().fg(fill);
    (
        Line::from(Span::styled(BLEED_ABOVE.repeat(width), style)),
        Line::from(Span::styled(BLEED_BELOW.repeat(width), style)),
    )
}

/// A field's label, over its box: gray at rest, white and bold while the
/// field has the keyboard (read-only or not), muted when it can't be
/// changed.
pub(crate) fn label(text: &str, focused: bool, disabled: bool, width: usize) -> Line<'static> {
    let style = if focused {
        Style::default().fg(palette().white).add_modifier(Modifier::BOLD)
    } else if disabled {
        Style::default().fg(palette().muted)
    } else {
        Style::default().fg(palette().gray)
    };
    Line::from(Span::styled(panel::truncate_right(text, width), style))
}

/// `chars` in `room` columns with the caret (when there is one) at char
/// `caret`, scrolled so the caret's cell shows; pushes the spans and returns
/// the columns used. The caret is a block in the strong text color, the char
/// under it drawn in the fill.
pub(crate) fn caret_spans(spans: &mut Vec<Span<'static>>, chars: &[char], caret: Option<usize>, room: usize, style: Style) -> usize {
    let w = |c: &char| c.width().unwrap_or(0).max(1);
    let Some(at) = caret.map(|c| c.min(chars.len())) else {
        let mut used = 0;
        let shown: String = chars
            .iter()
            .take_while(|c| {
                used += w(c);
                used <= room
            })
            .collect();
        let used = panel::display_width(&shown);
        spans.push(Span::styled(shown, style));
        return used;
    };
    // Start as far left as keeps the caret's cell in the room.
    let caret_w = chars.get(at).map_or(1, w);
    let mut start = 0;
    let mut before: usize = chars[..at].iter().map(w).sum();
    while start < at && before + caret_w > room {
        before -= w(&chars[start]);
        start += 1;
    }
    let mut used = 0;
    let head: String = chars[start..at].iter().collect();
    used += panel::display_width(&head);
    spans.push(Span::styled(head, style));
    let under = chars.get(at).map_or(" ".to_string(), |c| c.to_string());
    let fill = style.bg.unwrap_or(Color::Reset);
    spans.push(Span::styled(under, Style::default().fg(fill).bg(palette().white)));
    used += caret_w;
    let mut tail = String::new();
    for c in chars.iter().skip(at + 1) {
        if used + w(c) > room {
            break;
        }
        used += w(c);
        tail.push(*c);
    }
    spans.push(Span::styled(tail, style));
    used
}

/// A multi-line field: `rows` (each already cut to the box's text room,
/// `width - 2` columns) on the fill, with the caps above and below the
/// whole area. A span without a background of its own takes the fill.
pub(crate) fn area(rows: Vec<Vec<Span<'static>>>, focused: bool, disabled: bool, invalid: bool, width: usize) -> Vec<Line<'static>> {
    area_in(rows, fill(focused, disabled, invalid), width)
}

/// [`area`] in `fill`, whatever its state.
pub(crate) fn area_in(rows: Vec<Vec<Span<'static>>>, fill: Color, width: usize) -> Vec<Line<'static>> {
    let on_fill = Style::default().bg(fill);
    let (above, below) = caps(width, fill);
    let mut lines = vec![above];
    for row in rows {
        let mut spans = vec![Span::styled(" ".repeat(INSET.min(width)), on_fill)];
        for mut span in row {
            if span.style.bg.is_none() {
                span.style.bg = Some(fill);
            }
            spans.push(span);
        }
        let used: usize = spans.iter().map(|s| panel::display_width(&s.content)).sum();
        spans.push(Span::styled(" ".repeat(width.saturating_sub(used)), on_fill));
        lines.push(Line::from(spans));
    }
    lines.push(below);
    lines
}

/// The text room inside a box `width` wide.
pub(crate) fn room(width: usize) -> usize {
    width.saturating_sub(2 * INSET)
}

/// The text style on a multi-line field's fill.
pub(crate) fn area_text(focused: bool, disabled: bool) -> Style {
    Style::default().fg(if disabled {
        palette().muted
    } else if focused {
        palette().white
    } else {
        palette().foreground
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{assert_light_readable, with_appearance};
    use ratatui::{buffer::Buffer, layout::Rect};
    use shelbi_app::theme::Appearance;

    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Draw `lines` from row `y` of `buf`.
    fn draw(buf: &mut Buffer, y: u16, lines: &[Line]) {
        for (i, line) in lines.iter().enumerate() {
            buf.set_line(0, y + i as u16, line, buf.area.width);
        }
    }

    fn render(tb: Textbox, width: u16) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, width, 3));
        draw(&mut buf, 0, &tb.lines(width as usize));
        buf
    }

    fn row(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn a_box_is_a_cap_the_text_on_its_fill_and_a_cap() {
        let buf = render(Textbox::new("alpha"), 12);
        assert_eq!(row(&buf, 0), "▄".repeat(12));
        assert_eq!(row(&buf, 1), " alpha      ");
        assert_eq!(row(&buf, 2), "▀".repeat(12));
        // The caps carry the fill as their glyph's color on the terminal's
        // own background; the text row is the solid fill, every cell.
        assert_eq!(buf[(0, 0)].fg, palette().input);
        assert_eq!(buf[(0, 0)].bg, Color::Reset);
        assert_eq!(buf[(0, 2)].fg, palette().input);
        for x in 0..12 {
            assert_eq!(buf[(x, 1)].bg, palette().input, "column {x}");
        }
        assert_eq!(buf[(1, 1)].fg, palette().foreground);
    }

    #[test]
    fn each_state_has_its_own_fill() {
        let fill_of = |tb: Textbox| render(tb, 10)[(5, 1)].bg;
        let idle = fill_of(Textbox::new("x"));
        let focused = fill_of(Textbox::new("x").focused(true));
        let typing = fill_of(Textbox::new("x").caret_at_end());
        let invalid = fill_of(Textbox::new("x").invalid(true));
        let disabled = fill_of(Textbox::new("x").disabled(true));
        assert_eq!(idle, palette().input);
        assert_eq!(focused, palette().input_focus);
        assert_eq!(typing, palette().input_focus, "typing in it is focus");
        assert_eq!(invalid, palette().input_invalid);
        assert_eq!(disabled, palette().input_disabled);
        let mut all = vec![idle, focused, invalid, disabled];
        all.dedup();
        assert_eq!(all.len(), 4, "four distinct fills");
        // A refused field keeps its red tint while it's typed in; a
        // read-only one the keyboard is on shows focus, its text muted.
        assert_eq!(fill_of(Textbox::new("x").invalid(true).caret_at_end()), palette().input_invalid);
        assert_eq!(fill_of(Textbox::new("x").disabled(true).focused(true)), palette().input_focus);
        assert_eq!(render(Textbox::new("x").disabled(true).focused(true), 10)[(1, 1)].fg, palette().muted);
        // Disabled text is muted; a focused value is the strong color.
        assert_eq!(render(Textbox::new("x").disabled(true), 10)[(1, 1)].fg, palette().muted);
        assert_eq!(render(Textbox::new("x").focused(true), 10)[(1, 1)].fg, palette().white);
    }

    #[test]
    fn an_empty_field_shows_its_placeholder_muted() {
        let buf = render(Textbox::new("").placeholder("Optional tags"), 20);
        assert_eq!(row(&buf, 1), " Optional tags      ");
        assert_eq!(buf[(1, 1)].fg, palette().muted);
        // Typed in, the caret sits on the first cell, the placeholder after.
        let buf = render(Textbox::new("").placeholder("Optional tags").caret_at_end(), 20);
        assert_eq!(row(&buf, 1), "  Optional tags     ");
        assert_eq!(buf[(1, 1)].bg, palette().white, "the caret");
        assert_eq!(buf[(2, 1)].fg, palette().muted);
    }

    #[test]
    fn the_caret_is_a_block_on_its_char() {
        let buf = render(Textbox::new("alpha").caret(Some(2)), 12);
        assert_eq!(row(&buf, 1), " alpha      ");
        assert_eq!(buf[(3, 1)].symbol(), "p");
        assert_eq!(buf[(3, 1)].bg, palette().white);
        assert_eq!(buf[(3, 1)].fg, palette().input_focus);
        assert_eq!(buf[(2, 1)].bg, palette().input_focus);
        // At the end, a blank block after the text.
        let buf = render(Textbox::new("alpha").caret_at_end(), 12);
        assert_eq!(buf[(6, 1)].symbol(), " ");
        assert_eq!(buf[(6, 1)].bg, palette().white);
    }

    #[test]
    fn a_long_value_scrolls_to_keep_the_caret_in_view() {
        let long = "abcdefghijklmnopqrstuvwxyz";
        // Room for 8 chars: at the end, the last 7 and the caret show.
        let buf = render(Textbox::new(long).caret_at_end(), 10);
        assert_eq!(row(&buf, 1), " tuvwxyz  ");
        assert_eq!(buf[(8, 1)].bg, palette().white);
        // At the start, the first 8 show and the caret is on `a`.
        let buf = render(Textbox::new(long).caret(Some(0)), 10);
        assert_eq!(row(&buf, 1), " abcdefgh ");
        assert_eq!(buf[(1, 1)].bg, palette().white);
        // In the middle, the caret's char is the last shown.
        let buf = render(Textbox::new(long).caret(Some(12)), 10);
        assert_eq!(row(&buf, 1), " fghijklm ");
        assert_eq!(buf[(8, 1)].symbol(), "m");
        assert_eq!(buf[(8, 1)].bg, palette().white);
        // Not typed in, a long value is cut with an ellipsis (or keeps its
        // end, for a path).
        let buf = render(Textbox::new(long), 10);
        assert_eq!(row(&buf, 1), " abcdefg… ");
        let buf = render(Textbox::new("/home/me/src/shelbi").clip_left(true), 12);
        assert_eq!(row(&buf, 1), " .../shelbi ");
    }

    #[test]
    fn wide_chars_scroll_by_their_cells() {
        // Two-cell chars: room 6 holds the caret block and two of them.
        let buf = render(Textbox::new("日本語です").caret_at_end(), 8);
        let shown = row(&buf, 1).replace(' ', "");
        assert!(shown.contains("です"), "{shown:?}");
        assert!(!shown.contains("語"), "{shown:?}");
    }

    #[test]
    fn a_trail_sits_at_the_right_edge() {
        let buf = render(Textbox::new("devbox").trail("▾"), 14);
        assert_eq!(row(&buf, 1), " devbox     ▾ ");
        let buf = render(Textbox::new("a very long machine name").trail("▾"), 14);
        assert_eq!(row(&buf, 1), " a very lo… ▾ ");
    }

    #[test]
    fn a_whole_selected_value_is_highlighted() {
        let buf = render(Textbox::new("alpha").focused(true).selected(true), 12);
        assert_eq!(buf[(1, 1)].bg, palette().cyan);
        assert_eq!(buf[(6, 1)].bg, palette().input_focus);
    }

    #[test]
    fn narrow_boxes_never_overflow() {
        for width in 0..6u16 {
            let tb = Textbox::new("alpha").placeholder("p").caret_at_end().trail("▾");
            for line in tb.lines(width as usize) {
                assert!(panel::display_width(&text(&line)) <= width as usize, "width {width}: {:?}", text(&line));
            }
        }
    }

    #[test]
    fn a_multi_line_area_has_the_caps_around_it() {
        let style = area_text(false, false);
        let lines = area(
            vec![vec![Span::styled("one", style)], vec![Span::styled("two", style)]],
            false,
            false,
            false,
            8,
        );
        let shown: Vec<String> = lines.iter().map(text).collect();
        assert_eq!(shown, ["▄".repeat(8), " one    ".into(), " two    ".into(), "▀".repeat(8)]);
        for line in &lines[1..3] {
            assert!(line.spans.iter().all(|s| s.style.bg == Some(palette().input)));
        }
    }

    #[test]
    fn every_state_reads_in_the_light_theme() {
        with_appearance(Appearance::Light, || {
            let states = [
                Textbox::new("alpha"),
                Textbox::new("alpha").focused(true),
                Textbox::new("alpha").caret(Some(1)),
                Textbox::new("alpha").invalid(true),
                Textbox::new("alpha").disabled(true),
                Textbox::new("").placeholder("Optional tags"),
                Textbox::new("").placeholder("Optional tags").caret_at_end(),
                Textbox::new("alpha").focused(true).selected(true),
                Textbox::new("devbox").trail("▾"),
            ];
            let area = Rect::new(0, 0, 20, 3 * states.len() as u16);
            let mut buf = Buffer::empty(area);
            for (i, tb) in states.iter().enumerate() {
                draw(&mut buf, 3 * i as u16, &tb.lines(20));
            }
            assert_light_readable("textbox_states", &buf);
        });
    }
}
