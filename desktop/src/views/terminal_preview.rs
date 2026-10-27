//! Settings ▸ Terminal's preview: the shared preview session
//! ([`shelbi_app::settings::terminal_preview`], the Terminal frame's
//! (`2264:7445`) Claude Code session as a canned ANSI byte stream), which a
//! read-only terminal pane ([`TerminalPane::canned`]) draws with the real
//! terminal element, so it shows the font, the size and the ANSI colors as
//! every terminal would.
//!
//! [`TerminalPane::canned`]: crate::elements::terminal::TerminalPane::canned

use shelbi_term::Size;

/// The stream for a `size` grid, drawn from a clear screen.
pub fn stream(size: Size) -> Vec<u8> {
    shelbi_app::settings::terminal_preview::stream(size.cols, size.rows)
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    use shelbi_term::view::TerminalView;

    use super::*;

    fn screen(size: Size) -> TerminalView {
        let mut view = TerminalView::new(size);
        view.feed(&stream(size));
        view
    }

    fn rows(view: &TerminalView) -> Vec<String> {
        let grid = view.emulator().grid();
        (0..grid.screen_lines() as i32)
            .map(|row| {
                (0..grid.columns())
                    .map(|c| grid[Line(row)][Column(c)].c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    /// The row whose text starts with `prefix`, and the cell at `col` on it.
    fn cell(view: &TerminalView, prefix: &str, col: usize) -> (Color, Flags) {
        let row = rows(view).iter().position(|r| r.starts_with(prefix)).unwrap_or_else(|| panic!("{prefix:?}"));
        let c = &view.emulator().grid()[Line(row as i32)][Column(col)];
        (c.fg, c.flags)
    }

    #[test]
    fn the_frame_s_session_fills_the_grid_with_the_prompt_at_its_foot() {
        let size = Size::new(80, 48);
        let view = screen(size);
        let rows = rows(&view);
        assert_eq!(rows[0], " ▐▛███▜▌    Claude Code");
        assert_eq!(rows[1], "▝▜█████▛▘   Sonnet · Shelbi");
        assert_eq!(rows[2], "  ▘▘ ▝▝     ~/Workspaces/shelbi");
        assert_eq!(rows[4], "❯ Add Ctrl+K as another command palette shortcut.");
        assert!(rows.contains(&"     187 + GlobalAction::OpenPalette => &[\"ctrl-p\", \"ctrl-k\"],".to_string()));
        assert!(rows.contains(&"✻ Worked for 38s".to_string()));
        // The prompt block: the last five rows.
        let foot = &rows[43..];
        assert_eq!(foot[0], "─".repeat(80));
        assert_eq!((foot[1].as_str(), foot[2].as_str()), ("❯", "─".repeat(80).as_str()));
        assert_eq!(foot[3], "⏵⏵ accept edits on (shift+tab to cycle)");
        assert_eq!(foot[4], "? for shortcuts");
        let cursor = view.emulator().grid().cursor.point;
        assert_eq!((cursor.line.0, cursor.column.0), (44, 2), "the cursor waits in the prompt");
    }

    #[test]
    fn a_short_grid_puts_the_prompt_after_the_transcript() {
        let size = Size::new(80, 30);
        let mut view = TerminalView::new(size);
        view.feed(&stream(size));
        // Scrolled: the hint is the last row and nothing was cut off.
        let rows = rows(&view);
        assert_eq!(rows.last().map(String::as_str), Some("? for shortcuts"));
        let cursor = view.emulator().grid().cursor.point;
        assert_eq!((cursor.line.0, cursor.column.0), (26, 2), "the cursor waits in the prompt");
        assert_eq!(rows[rows.len() - 5], "─".repeat(80));
        assert_eq!(rows[rows.len() - 7], "✻ Worked for 38s");
    }

    #[test]
    fn each_line_takes_the_frame_s_ansi_color() {
        let view = screen(Size::new(80, 48));
        let named = |c: NamedColor| Color::Named(c);
        assert_eq!(cell(&view, " ▐▛", 1).0, named(NamedColor::BrightRed), "the mascot");
        let (fg, flags) = cell(&view, " ▐▛", 12);
        assert_eq!(fg, named(NamedColor::BrightWhite));
        assert!(flags.contains(Flags::BOLD), "Claude Code is bold");
        assert_eq!(cell(&view, "  ▘▘", 12).0, named(NamedColor::Cyan), "the directory");
        assert_eq!(cell(&view, "● Read", 0).0, named(NamedColor::BrightGreen));
        assert_eq!(cell(&view, "● Read", 6).0, named(NamedColor::Cyan));
        assert_eq!(cell(&view, "● Bash", 6).0, named(NamedColor::Yellow));
        assert_eq!(cell(&view, "     187 -", 5).0, named(NamedColor::Red), "the removed line");
        assert_eq!(cell(&view, "     187 +", 5).0, named(NamedColor::Green), "the added line");
        assert_eq!(cell(&view, "     185", 5).0, named(NamedColor::White));
        assert!(cell(&view, "  ⎿  Read", 2).1.contains(Flags::DIM), "tool output is dim");
        assert_eq!(cell(&view, "● I’ll", 0).0, named(NamedColor::Foreground), "the reply");
        assert_eq!(cell(&view, "⏵⏵", 0).0, named(NamedColor::BrightMagenta));
    }
}
