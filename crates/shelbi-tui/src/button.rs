//! The settings screens' button and segmented control, drawn the way a
//! field's box is ([`crate::textbox`]): a `▄` row above, the label on a
//! solid fill, a `▀` row below, the caps' glyphs carrying the fill as their
//! foreground. A button sits level with a field beside it.
//!
//! ```text
//!   ▄▄▄▄▄▄▄▄▄▄ ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
//!     Cancel     Create workspace
//!   ▀▀▀▀▀▀▀▀▀▀ ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
//! ```
//!
//! The kinds are the desktop's ([`Kind`]): Secondary on a gray fill, Primary
//! (a dialog's confirming action) on a cyan tint, Danger (a destructive one)
//! on a red tint. With the keyboard a button goes solid cyan (red for
//! Danger), the desktop's focus ring color; under the pointer its fill
//! lifts ([`light_hovered`], at draw time from the drawn cells); a button
//! that can't run now is a read-only field's muted look (focus included).
//!
//! An editor's title bar lays buttons, its title and a segmented control
//! out on one three-row [`bar`].
//!
//! The segmented control ([`segmented`]) is a field's box holding its
//! choices, the chosen one a chip in [`Theme::segment`] (the desktop's
//! canvas chip, as Contact Us's), the others lifting under the pointer.
//!
//! [`Theme::segment`]: shelbi_app::theme::Theme::segment

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::panel;
use crate::sidebar::{BLEED_ABOVE, BLEED_BELOW};
use crate::textbox;
use crate::theme::palette;

/// Rows a button takes: the cap above, the label, the cap below.
pub(crate) const BUTTON_H: usize = 3;

/// Columns of fill each side of a label.
const INSET: usize = 2;

/// Columns between two buttons in a row.
pub(crate) const GAP: usize = 1;

/// A button's kind, as the desktop's `ButtonKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A dialog's confirming action (Save, Create workspace).
    Primary,
    /// Every other action.
    Secondary,
    /// A destructive action (Delete, Remove workspace).
    Danger,
}

/// One button.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Button<'a> {
    label: &'a str,
    kind: Kind,
    focused: bool,
    enabled: bool,
    /// Lifted as under the pointer, for a view that tracks its own hover
    /// (the rest find theirs in the drawn frame, [`light_hovered`]).
    hovered: bool,
}

impl<'a> Button<'a> {
    pub(crate) fn new(label: &'a str, kind: Kind) -> Self {
        Button { label, kind, focused: false, enabled: true, hovered: false }
    }

    pub(crate) fn secondary(label: &'a str) -> Self {
        Button::new(label, Kind::Secondary)
    }

    pub(crate) fn primary(label: &'a str) -> Self {
        Button::new(label, Kind::Primary)
    }

    pub(crate) fn danger(label: &'a str) -> Self {
        Button::new(label, Kind::Danger)
    }

    pub(crate) fn focused(mut self, on: bool) -> Self {
        self.focused = on;
        self
    }

    pub(crate) fn enabled(mut self, on: bool) -> Self {
        self.enabled = on;
        self
    }

    pub(crate) fn hovered(mut self, on: bool) -> Self {
        self.hovered = on;
        self
    }

    pub(crate) fn is_focused(&self) -> bool {
        self.focused
    }

    /// Its width: the label and its insets.
    pub(crate) fn width(&self) -> usize {
        panel::display_width(self.label) + 2 * INSET
    }

    /// Its fill and its label's style.
    pub(crate) fn look(&self) -> (Color, Style) {
        let p = palette();
        if !self.enabled {
            // The keyboard can still land on it (to hear why it can't run):
            // a read-only field's focus, as the textbox shows it.
            let fill = if self.focused { p.input_focus } else { p.input_disabled };
            return (fill, Style::default().fg(p.muted));
        }
        let strong = Style::default().add_modifier(Modifier::BOLD);
        let lit = self.hovered;
        match (self.kind, self.focused) {
            (Kind::Danger, true) => (p.button_danger_focus, strong.fg(p.button_focus_text)),
            (_, true) => (p.button_focus, strong.fg(p.button_focus_text)),
            (Kind::Primary, false) => {
                (if lit { p.button_primary_lit } else { p.button_primary }, strong.fg(p.button_primary_text))
            }
            (Kind::Secondary, false) => (if lit { p.button_lit } else { p.button }, Style::default().fg(p.white)),
            (Kind::Danger, false) => {
                (if lit { p.button_danger_lit } else { p.button_danger }, Style::default().fg(p.button_danger_text))
            }
        }
    }

    /// Its three rows' spans, `width()` columns each.
    pub(crate) fn spans(&self) -> [Span<'static>; 3] {
        let (fill, style) = self.look();
        let w = self.width();
        let cap = Style::default().fg(fill);
        [
            Span::styled(BLEED_ABOVE.repeat(w), cap),
            Span::styled(format!("{}{}{}", " ".repeat(INSET), self.label, " ".repeat(INSET)), style.bg(fill)),
            Span::styled(BLEED_BELOW.repeat(w), cap),
        ]
    }

    /// Its three rows `width` wide (at least its own), the label centered:
    /// a full-width button (a dialog's stacked Send Message and Cancel).
    pub(crate) fn lines_in(&self, width: usize) -> [Line<'static>; 3] {
        let (fill, style) = self.look();
        let w = width.max(self.width());
        let label = panel::display_width(self.label);
        let left = (w - label) / 2;
        let cap = Style::default().fg(fill);
        [
            Line::from(Span::styled(BLEED_ABOVE.repeat(w), cap)),
            Line::from(Span::styled(format!("{}{}{}", " ".repeat(left), self.label, " ".repeat(w - label - left)), style.bg(fill))),
            Line::from(Span::styled(BLEED_BELOW.repeat(w), cap)),
        ]
    }

    /// Draw it into `rect` of `buf` (its three rows, the label centered),
    /// over whatever fill is there.
    pub(crate) fn draw(&self, buf: &mut Buffer, rect: Rect) {
        for (i, line) in self.lines_in(rect.width as usize).iter().enumerate().take(rect.height as usize) {
            buf.set_line(rect.x, rect.y + i as u16, line, rect.width);
        }
    }

    /// Its three rows.
    #[cfg(test)]
    pub(crate) fn lines(&self) -> [Line<'static>; 3] {
        self.spans().map(Line::from)
    }
}

/// Where a row of buttons sits in its width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Align {
    Left,
    Right,
}

/// A row of buttons [`GAP`] apart in `width` columns: its three lines and
/// each button's column and width. Buttons that don't fit are left off the
/// end (so a right-aligned row keeps its first ones).
pub(crate) fn row(buttons: &[Button<'_>], width: usize, align: Align) -> ([Line<'static>; 3], Vec<(u16, u16)>) {
    let mut shown = Vec::new();
    let mut used = 0;
    for b in buttons {
        let w = b.width() + if shown.is_empty() { 0 } else { GAP };
        if used + w > width {
            break;
        }
        used += w;
        shown.push(*b);
    }
    let start = match align {
        Align::Left => 0,
        Align::Right => width.saturating_sub(used),
    };
    let mut lines: [Vec<Span<'static>>; 3] = [vec![], vec![], vec![]];
    let mut at = Vec::new();
    let mut x = start;
    if start > 0 {
        for l in lines.iter_mut() {
            l.push(Span::raw(" ".repeat(start)));
        }
    }
    for (i, b) in shown.iter().enumerate() {
        if i > 0 {
            for l in lines.iter_mut() {
                l.push(Span::raw(" ".repeat(GAP)));
            }
            x += GAP;
        }
        for (l, s) in lines.iter_mut().zip(b.spans()) {
            l.push(s);
        }
        at.push((x as u16, b.width() as u16));
        x += b.width();
    }
    (lines.map(Line::from), at)
}

/// One band of [`bands`]: its three lines, and each button's column,
/// width and index.
pub(crate) type Band = ([Line<'static>; 3], Vec<(u16, u16, usize)>);

/// `buttons` in bands of three rows, as many to a band as fit `width`
/// (one too wide alone still gets its own band, clipped), each band set to
/// `align`: its lines, and each button's column, width and index in
/// `buttons`. Nothing is left off, so a narrow dialog keeps its confirming
/// action.
pub(crate) fn bands(buttons: &[Button<'_>], width: usize, align: Align) -> Vec<Band> {
    let mut out = Vec::new();
    let mut start = 0;
    while start < buttons.len() {
        let mut end = start + 1;
        while end < buttons.len() && row_width(&buttons[start..=end]) <= width {
            end += 1;
        }
        let band = &buttons[start..end];
        let (lines, at) = if row_width(band) > width {
            // One alone, too wide: drawn from the left, cut by the edge.
            row(band, usize::MAX / 2, Align::Left)
        } else {
            row(band, width, align)
        };
        out.push((lines, at.into_iter().enumerate().map(|(i, (x, w))| (x, w, start + i)).collect()));
        start = end;
    }
    out
}

/// The width a row of `buttons` takes.
pub(crate) fn row_width(buttons: &[Button<'_>]) -> usize {
    let sum: usize = buttons.iter().map(Button::width).sum();
    sum + GAP * buttons.len().saturating_sub(1)
}

/// `left` (three lines), padded to `width - right_w`, then `right` after it:
/// a heading with its buttons at the right.
pub(crate) fn beside(left: [Line<'static>; 3], right: [Line<'static>; 3], width: usize, right_w: usize) -> [Line<'static>; 3] {
    let room = width.saturating_sub(right_w);
    let mut out = left;
    for (line, r) in out.iter_mut().zip(right) {
        let w = line.width();
        if w < room {
            line.spans.push(Span::raw(" ".repeat(room - w)));
        }
        line.spans.extend(r.spans);
    }
    out
}

/// A segmented control in `width` columns: a field's box holding `labels`,
/// `chosen` a chip in the segment fill (bold and strong while the control has
/// the keyboard), `hovered` lifting. Returns its three lines and each
/// segment's column and width.
pub(crate) fn segmented(
    labels: &[&str],
    chosen: usize,
    focused: bool,
    hovered: Option<usize>,
    width: usize,
) -> ([Line<'static>; 3], Vec<(u16, u16)>) {
    let p = palette();
    let track = textbox::fill(focused, false, false);
    let on_track = Style::default().bg(track);
    let mut spans = vec![Span::styled(" ", on_track)];
    let mut at = Vec::new();
    let mut x = 1;
    for (i, label) in labels.iter().enumerate() {
        let text = format!(" {label} ");
        let w = panel::display_width(&text);
        if x + w + 1 > width {
            break;
        }
        let style = if i == chosen {
            // The desktop's canvas chip on its track, as Contact Us draws it.
            let s = Style::default().bg(p.segment).fg(p.white);
            if focused {
                s.add_modifier(Modifier::BOLD)
            } else {
                s
            }
        } else if hovered == Some(i) {
            Style::default().bg(p.button).fg(p.white)
        } else {
            on_track.fg(p.foreground)
        };
        spans.push(Span::styled(text, style));
        at.push((x as u16, w as u16));
        x += w;
    }
    spans.push(Span::styled(" ", on_track));
    x += 1;
    let (above, below) = textbox::caps(x, track);
    ([above, Line::from(spans), below], at)
}

/// A badge beside a title (Default, Customized): the desktop's
/// `workflow_badge` chip.
pub(crate) fn badge(text: &str) -> Span<'static> {
    let p = palette();
    Span::styled(format!(" {text} "), Style::default().fg(p.workflow_badge_fg).bg(p.workflow_badge_bg))
}

/// One piece of a [`bar`]: its three rows, its width, where clicks on it
/// land (`x` within it, `w`, a tag its owner knows), and how soon it goes
/// when the bar is too narrow (0: never; the highest goes first).
pub(crate) struct Piece {
    rows: [Vec<Span<'static>>; 3],
    width: usize,
    hits: Vec<(u16, u16, usize)>,
    drop: u8,
    /// Text that gives way (cut with `…`) before anything drops.
    shrink: Option<(String, Style)>,
}

impl Piece {
    /// A button, tagged `tag`.
    pub(crate) fn button(b: Button<'_>, tag: usize, drop: u8) -> Piece {
        let w = b.width();
        Piece { rows: b.spans().map(|s| vec![s]), width: w, hits: vec![(0, w as u16, tag)], drop, shrink: None }
    }

    /// A title on the middle row (bold and loud), `after` it (a badge).
    pub(crate) fn title(text: &str, after: Vec<Span<'static>>) -> Piece {
        let style = Style::default().fg(palette().white).add_modifier(Modifier::BOLD);
        let after_w: usize = after.iter().map(|s| panel::display_width(&s.content)).sum();
        let mut middle = vec![Span::styled(text.to_string(), style)];
        middle.extend(after);
        Piece {
            rows: [vec![], middle, vec![]],
            width: panel::display_width(text) + after_w,
            hits: Vec::new(),
            drop: 0,
            shrink: Some((text.to_string(), style)),
        }
    }

    /// A segmented control, each segment tagged `tags[i]`.
    pub(crate) fn segmented(labels: &[&str], chosen: usize, focused: bool, tags: &[usize], drop: u8) -> Piece {
        let (lines, at) = segmented(labels, chosen, focused, None, usize::MAX);
        let width = lines[0].width();
        let hits = at.into_iter().zip(tags).map(|((x, w), t)| (x, w, *t)).collect();
        Piece { rows: lines.map(|l| l.spans), width, hits, drop, shrink: None }
    }
}

/// A one-bar row of pieces three rows tall (an editor's title bar): `left`
/// from the left edge, `right` against the right one, a column between
/// neighbors. Too narrow, the pieces with the highest drop rank go first,
/// then a title gives way. Returns the lines and each click target
/// (`x`, `w`, tag).
pub(crate) fn bar(mut left: Vec<Piece>, mut right: Vec<Piece>, width: usize) -> ([Line<'static>; 3], Vec<(u16, u16, usize)>) {
    let total = |l: &[Piece], r: &[Piece]| {
        let n = l.len() + r.len();
        l.iter().chain(r).map(|p| p.width).sum::<usize>() + 2 * n.saturating_sub(1)
    };
    // The pieces that can go, highest rank first, then the title gives way
    // (down to a few columns).
    while total(&left, &right) > width {
        let worst = left.iter().chain(&right).map(|p| p.drop).max().unwrap_or(0);
        if worst == 0 {
            break;
        }
        if let Some(at) = right.iter().rposition(|p| p.drop == worst) {
            right.remove(at);
        } else if let Some(at) = left.iter().rposition(|p| p.drop == worst) {
            left.remove(at);
        }
    }
    let over = total(&left, &right).saturating_sub(width);
    if over > 0 {
        if let Some(p) = left.iter_mut().find(|p| p.shrink.is_some()) {
            let (text, style) = p.shrink.clone().unwrap_or_default();
            let keep = panel::display_width(&text).saturating_sub(over).max(6);
            let cut = panel::truncate_right(&text, keep);
            p.width = p.width - panel::display_width(&text) + panel::display_width(&cut);
            p.rows[1][0] = Span::styled(cut, style);
        }
    }
    let mut rows: [Vec<Span<'static>>; 3] = [vec![], vec![], vec![]];
    let mut hits = Vec::new();
    let mut x = 0usize;
    let place = |rows: &mut [Vec<Span<'static>>; 3], hits: &mut Vec<(u16, u16, usize)>, p: Piece, x: &mut usize| {
        for (row, spans) in rows.iter_mut().zip(p.rows) {
            let used: usize = row.iter().map(|s| panel::display_width(&s.content)).sum();
            if used < *x {
                row.push(Span::raw(" ".repeat(*x - used)));
            }
            row.extend(spans);
        }
        hits.extend(p.hits.iter().map(|&(hx, w, t)| (*x as u16 + hx, w, t)));
        *x += p.width;
    };
    for p in left {
        place(&mut rows, &mut hits, p, &mut x);
        x += 2;
    }
    let right_w = if right.is_empty() { 0 } else { total(&[], &right) };
    x = x.max(width.saturating_sub(right_w));
    for p in right {
        place(&mut rows, &mut hits, p, &mut x);
        x += 2;
    }
    (rows.map(Line::from), hits)
}

/// A button's rest fill and the fill it lifts to under the pointer.
fn lifts() -> [(Color, Color); 3] {
    let p = palette();
    [
        (p.button, p.button_lit),
        (p.button_primary, p.button_primary_lit),
        (p.button_danger, p.button_danger_lit),
    ]
}

/// Every button drawn in `area` of `buf`: its three rows, found from the
/// drawn cells — a run of a button's rest fill with its caps above and
/// below.
pub(crate) fn drawn(buf: &Buffer, area: Rect) -> Vec<Rect> {
    let area = area.intersection(buf.area);
    let fills: Vec<Color> = lifts().iter().map(|(rest, _)| *rest).collect();
    let mut out = Vec::new();
    for y in area.top()..area.bottom() {
        let mut x = area.left();
        while x < area.right() {
            let bg = buf[(x, y)].bg;
            if !fills.contains(&bg) {
                x += 1;
                continue;
            }
            let start = x;
            while x < area.right() && buf[(x, y)].bg == bg {
                x += 1;
            }
            let cap = |cx: u16, cy: u16, glyph: &str| {
                let c = &buf[(cx, cy)];
                c.symbol() == glyph && c.fg == bg
            };
            let above = y > area.top() && cap(start, y - 1, BLEED_ABOVE);
            let below = y + 1 < area.bottom() && cap(start, y + 1, BLEED_BELOW);
            if above && below {
                out.push(Rect { x: start, y: y - 1, width: x - start, height: 3 });
            }
        }
    }
    out
}

/// Lift the button under `pointer` (one of [`drawn`]'s) to its hover fill.
/// Returns the lit button's rect.
pub(crate) fn light_hovered(buf: &mut Buffer, buttons: &[Rect], pointer: Option<(u16, u16)>) -> Option<Rect> {
    let (px, py) = pointer?;
    let rect = *buttons.iter().find(|r| panel::in_rect(**r, px, py))?;
    for y in rect.top()..rect.bottom() {
        for x in rect.left()..rect.right() {
            let cell = &mut buf[(x, y)];
            for (rest, lit) in lifts() {
                if cell.bg == rest {
                    cell.bg = lit;
                }
                if cell.fg == rest {
                    cell.fg = lit;
                }
            }
        }
    }
    Some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::widgets::{Paragraph, Widget};

    fn draw(lines: Vec<Line<'static>>, w: u16) -> Buffer {
        let area = Rect::new(0, 0, w, lines.len() as u16);
        let mut buf = Buffer::empty(area);
        Paragraph::new(lines).render(area, &mut buf);
        buf
    }

    fn text(buf: &Buffer, y: u16) -> String {
        (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>().trim_end().to_string()
    }

    #[test]
    fn a_button_is_its_label_on_a_fill_between_half_block_caps() {
        let b = Button::secondary("Cancel");
        assert_eq!(b.width(), 10);
        let buf = draw(b.lines().to_vec(), 10);
        assert_eq!(text(&buf, 0), "▄".repeat(10));
        assert_eq!(text(&buf, 1), "  Cancel");
        assert_eq!(text(&buf, 2), "▀".repeat(10));
        let (fill, _) = b.look();
        assert_eq!(fill, palette().button);
        assert_eq!(buf[(0, 1)].bg, fill);
        assert_eq!(buf[(0, 0)].fg, fill, "the cap carries the fill");
    }

    #[test]
    fn each_kind_and_state_has_its_own_look() {
        let p = palette();
        let fill = |b: Button| b.look().0;
        assert_eq!(fill(Button::primary("Save")), p.button_primary);
        assert_eq!(fill(Button::danger("Delete")), p.button_danger);
        assert_eq!(fill(Button::secondary("Cancel").focused(true)), p.button_focus);
        assert_eq!(fill(Button::primary("Save").focused(true)), p.button_focus);
        assert_eq!(fill(Button::danger("Delete").focused(true)), p.button_danger_focus);
        let off = Button::primary("Save").enabled(false);
        assert_eq!(off.look(), (p.input_disabled, Style::default().fg(p.muted)), "disabled wins");
        let off = off.focused(true);
        assert_eq!(off.look(), (p.input_focus, Style::default().fg(p.muted)), "a read-only field's focus");
    }

    #[test]
    fn a_row_aligns_right_and_drops_what_does_not_fit() {
        let buttons = [Button::secondary("Cancel"), Button::primary("Create workspace")];
        let (lines, at) = row(&buttons, 40, Align::Right);
        assert_eq!(at, vec![(9, 10), (20, 20)]);
        let buf = draw(lines.to_vec(), 40);
        assert_eq!(text(&buf, 1), "           Cancel     Create workspace");
        let (_, at) = row(&buttons, 25, Align::Left);
        assert_eq!(at, vec![(0, 10)], "the second doesn't fit");
    }

    #[test]
    fn the_button_under_the_pointer_lifts_and_others_do_not() {
        let buttons = [Button::secondary("Cancel"), Button::primary("Save"), Button::danger("Delete")];
        let (lines, at) = row(&buttons, 40, Align::Left);
        let mut buf = draw(lines.to_vec(), 40);
        let found = drawn(&buf, buf.area);
        assert_eq!(found.len(), 3, "{found:?}");
        assert_eq!(found[1], Rect { x: at[1].0, y: 0, width: at[1].1, height: 3 });
        // On its top cap, the Primary lifts whole.
        let lit = light_hovered(&mut buf, &found, Some((at[1].0 + 2, 0)));
        assert_eq!(lit, Some(found[1]));
        let p = palette();
        assert_eq!(buf[(at[1].0, 1)].bg, p.button_primary_lit);
        assert_eq!(buf[(at[1].0, 2)].fg, p.button_primary_lit);
        assert_eq!(buf[(0, 1)].bg, p.button, "Cancel stays");
        assert_eq!(light_hovered(&mut buf, &found, Some((39, 1))), None, "nothing under the pointer");
    }

    #[test]
    fn focused_and_disabled_buttons_are_not_lit() {
        let buttons = [Button::secondary("A").focused(true), Button::secondary("B").enabled(false)];
        let (lines, _) = row(&buttons, 20, Align::Left);
        let buf = draw(lines.to_vec(), 20);
        assert!(drawn(&buf, buf.area).is_empty());
    }

    #[test]
    fn a_segmented_control_fills_the_chosen_segment() {
        let (lines, at) = segmented(&["Auto", "Dark", "Light"], 1, false, Some(2), 40);
        let buf = draw(lines.to_vec(), 40);
        assert_eq!(text(&buf, 1), "  Auto  Dark  Light");
        assert_eq!(at, vec![(1, 6), (7, 6), (13, 7)]);
        let p = palette();
        assert_eq!(buf[(8, 1)].bg, p.segment, "the chosen");
        assert_eq!(buf[(2, 1)].bg, p.input, "the track");
        assert_eq!(buf[(14, 1)].bg, p.button, "the hovered");
        assert_eq!(text(&buf, 0), "▄".repeat(21));
        let (lines, _) = segmented(&["Auto", "Dark", "Light"], 0, true, None, 40);
        let buf = draw(lines.to_vec(), 40);
        assert_eq!(buf[(0, 1)].bg, p.input_focus, "focus tints the track");
        // Too narrow for every segment: the ones that fit.
        let (_, at) = segmented(&["Auto", "Dark", "Light"], 0, false, None, 14);
        assert_eq!(at.len(), 2);
    }

    #[test]
    fn bands_wrap_what_does_not_fit_and_keep_every_button() {
        let buttons = [Button::secondary("Cancel"), Button::primary("Create workspace")];
        let one = bands(&buttons, 40, Align::Right);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].1, vec![(9, 10, 0), (20, 20, 1)]);
        // Too narrow for both: the confirming action gets its own band.
        let two = bands(&buttons, 26, Align::Right);
        assert_eq!(two.len(), 2);
        assert_eq!(two[0].1, vec![(16, 10, 0)]);
        assert_eq!(two[1].1, vec![(6, 20, 1)]);
        // Narrower than a button: it's still there, from the left.
        let cut = bands(&buttons, 12, Align::Right);
        assert_eq!(cut.iter().map(|b| b.1.len()).sum::<usize>(), 2);
        assert_eq!(cut[1].1, vec![(0, 20, 1)]);
    }

    #[test]
    fn a_bar_sets_its_pieces_at_both_ends_and_drops_the_least_needed_first() {
        let pieces = |w: usize| {
            let left = vec![Piece::button(Button::secondary("←"), 0, 0), Piece::title("orchestrator", vec![Span::raw("  "), badge("Default")])];
            let right = vec![
                Piece::button(Button::secondary("Discard changes"), 1, 2),
                Piece::button(Button::primary("Save changes"), 2, 0),
                Piece::button(Button::secondary("⋮"), 3, 0),
            ];
            let (lines, hits) = bar(left, right, w);
            let buf = draw(lines.to_vec(), w as u16);
            (text(&buf, 1), hits)
        };
        let (wide, hits) = pieces(100);
        assert!(wide.contains("orchestrator   Default") && wide.contains("Discard changes"), "{wide}");
        assert_eq!(hits.last().map(|h| h.0 + h.1), Some(100), "the ⋮ ends at the right edge");
        // Narrow: Discard goes before the title is cut.
        let (narrow, hits) = pieces(60);
        assert!(!narrow.contains("Discard") && narrow.contains("orchestrator"), "{narrow}");
        assert_eq!(hits.iter().map(|h| h.2).collect::<Vec<_>>(), vec![0, 2, 3]);
        // Narrower still: the title gives way, the buttons stay.
        let (tight, _) = pieces(44);
        assert!(tight.contains('…') && tight.contains("Save changes"), "{tight}");
    }

}
