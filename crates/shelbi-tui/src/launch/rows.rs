//! The launch screen's and the Connect dialog's lists: a project or a remote
//! session per row, its folder, its name over its folder path (or
//! `user@host:path`), and for a remote session a Connect chip at the right.
//!
//! ```text
//!   ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄
//!    📁 Shelbi                        Connect     <- the focused row
//!       jlong@devbox:~/Workspaces/shelbi
//!   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀
//!    📁 ContextStore                  Connect
//!       jlong@devbox:~/Workspaces/ContextStore
//! ```
//!
//! A lit row (the one with the keyboard, or the one under the pointer) is
//! the sidebar's half-block pill: the fill across both its lines, bled half
//! a row above and below ([`crate::sidebar::bleed_separator`]). The keyboard's
//! row is [`Theme::selection`], its name loud; the pointer's the lighter
//! [`Theme::row_hover`]. The chip is a Secondary button's fill, the focus
//! cyan while its row has the keyboard (Return presses it).
//!
//! [`Theme::selection`]: shelbi_app::theme::Theme::selection
//! [`Theme::row_hover`]: shelbi_app::theme::Theme::row_hover

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use crate::panel;
use crate::sidebar::bleed_separator;
use crate::theme::palette;

/// The lines a list of `n` rows takes: two a row, and a separator above,
/// between and below them.
#[cfg(test)]
pub(crate) fn height(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        3 * n + 1
    }
}

/// The folder before each row's name: the emoji where the terminal draws
/// one, else a box of the same width.
pub(crate) fn folder(emoji: bool) -> &'static str {
    if emoji {
        "📁"
    } else {
        "▭ "
    }
}

/// One row.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Item<'a> {
    pub title: &'a str,
    pub detail: &'a str,
    /// The chip's label (a remote session's Connect).
    pub action: Option<&'a str>,
}

/// Which rows are lit.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Lit {
    /// The row with the keyboard.
    pub focused: Option<usize>,
    /// The row under the pointer.
    pub hovered: Option<usize>,
    /// The chip under the pointer.
    pub action_hovered: Option<usize>,
}

/// Where a row landed in the lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Place {
    /// Its first line (the name's); the folder path is the next.
    pub line: usize,
    /// Its chip's column and width, when it has one.
    pub action: Option<(u16, u16)>,
}

/// The rows' lines, `width` wide, and where each row is.
pub(crate) fn lines(items: &[Item<'_>], lit: Lit, width: usize, emoji: bool) -> (Vec<Line<'static>>, Vec<Place>) {
    let mut out = Vec::new();
    let mut places = Vec::new();
    if items.is_empty() {
        return (out, places);
    }
    let fill_of = |i: usize| -> Option<Color> {
        if lit.focused == Some(i) {
            Some(palette().selection)
        } else if lit.hovered == Some(i) {
            Some(palette().row_hover)
        } else {
            None
        }
    };
    for (i, item) in items.iter().enumerate() {
        let above = i.checked_sub(1).and_then(fill_of);
        out.push(bleed_separator(above, fill_of(i), width));
        let (first, second, action) = row(item, i, fill_of(i), lit, width, emoji);
        places.push(Place { line: out.len(), action });
        out.push(first);
        out.push(second);
    }
    out.push(bleed_separator(fill_of(items.len() - 1), None, width));
    (out, places)
}

/// A row's two lines and its chip's place.
fn row(item: &Item<'_>, i: usize, fill: Option<Color>, lit: Lit, width: usize, emoji: bool) -> (Line<'static>, Line<'static>, Option<(u16, u16)>) {
    let p = palette();
    let on = |s: Style| match fill {
        Some(bg) => s.bg(bg),
        None => s,
    };
    let focused = lit.focused == Some(i);
    let title = if focused {
        Style::default().fg(p.white).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(p.foreground).add_modifier(Modifier::BOLD)
    };
    // Muted doesn't read on the selection's fill: gray there.
    let detail = Style::default().fg(if fill.is_some() { p.gray } else { p.muted });

    let icon = folder(emoji);
    let lead = 1 + panel::display_width(icon) + 1;
    let chip = item.action.map(|label| format!(" {label} "));
    let chip_w = chip.as_deref().map_or(0, panel::display_width);
    // The chip sits a column in from the right edge, a column of room
    // before it, when the name keeps a few columns beside it.
    let chip_room = if chip_w > 0 && width >= lead + 1 + 8 + chip_w + 2 { chip_w + 2 } else { 0 };
    let text_w = width.saturating_sub(lead + 1 + chip_room);

    let mut first = vec![Span::styled(" ", on(Style::default())), Span::styled(icon.to_string(), on(Style::default().fg(p.muted)))];
    first.push(Span::styled(" ", on(Style::default())));
    let name = panel::truncate_right(item.title, text_w);
    let used = lead + panel::display_width(&name);
    first.push(Span::styled(name, on(title)));
    let mut action = None;
    match chip {
        Some(chip) if chip_room > 0 => {
            first.push(Span::styled(" ".repeat(width - used - chip_room + 1), on(Style::default())));
            let x = width - chip_w - 1;
            let style = if focused {
                Style::default().bg(p.button_focus).fg(p.button_focus_text).add_modifier(Modifier::BOLD)
            } else if lit.action_hovered == Some(i) {
                Style::default().bg(p.button_lit).fg(p.white)
            } else {
                Style::default().bg(p.button).fg(p.white)
            };
            first.push(Span::styled(chip, style));
            first.push(Span::styled(" ", on(Style::default())));
            action = Some((x as u16, chip_w as u16));
        }
        _ => first.push(Span::styled(" ".repeat(width.saturating_sub(used)), on(Style::default()))),
    }

    let path = panel::truncate_left(item.detail, width.saturating_sub(lead + 1));
    let path_w = panel::display_width(&path);
    let second = vec![
        Span::styled(" ".repeat(lead), on(Style::default())),
        Span::styled(path, on(detail)),
        Span::styled(" ".repeat(width.saturating_sub(lead + path_w)), on(Style::default())),
    ];
    (Line::from(first), Line::from(second), action)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn rows_are_two_lines_between_separators_and_the_lit_one_bleeds() {
        let items = [
            Item { title: "Shelbi", detail: "~/Workspaces/shelbi", action: None },
            Item { title: "web", detail: "~/web", action: None },
        ];
        let lit = Lit { focused: Some(0), ..Lit::default() };
        let (lines, places) = lines(&items, lit, 30, false);
        assert_eq!(lines.len(), height(2));
        assert_eq!(places.iter().map(|p| p.line).collect::<Vec<_>>(), [1, 4]);
        assert_eq!(text(&lines[0]), "▄".repeat(30));
        assert_eq!(text(&lines[1]).trim_end(), " ▭  Shelbi");
        assert_eq!(text(&lines[2]).trim_end(), "    ~/Workspaces/shelbi");
        assert_eq!(text(&lines[3]), "▀".repeat(30), "the pill's bottom");
        assert_eq!(text(&lines[6]), "", "nothing lit below the last");
        assert_eq!(lines[1].spans[3].style.bg, Some(palette().selection));
        assert!(lines[1].spans[3].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn a_remote_row_has_its_chip_at_the_right() {
        let items = [Item { title: "Shelbi", detail: "jlong@devbox:~/Workspaces/shelbi", action: Some("Connect") }];
        let (lines, places) = lines(&items, Lit::default(), 40, false);
        let first = text(&lines[1]);
        assert_eq!(crate::panel::display_width(&first), 40);
        assert!(first.ends_with(" Connect  "), "{first:?}");
        assert_eq!(places[0].action, Some((30, 9)));
        // Too narrow for the chip: the row keeps its name.
        let (lines, places) = super::lines(&items, Lit::default(), 12, false);
        assert_eq!(places[0].action, None);
        assert!(text(&lines[1]).contains("Shel"));
    }
}
