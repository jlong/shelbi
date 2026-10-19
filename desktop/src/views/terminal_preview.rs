//! Settings ▸ Terminal's preview: the Terminal frame's (`2264:7445`) Claude
//! Code session as a canned ANSI byte stream, which a read-only terminal pane
//! ([`TerminalPane::canned`]) draws with the real terminal element, so it
//! shows the font, the size and the ANSI colors as every terminal would.
//!
//! Where the frame binds a line to an ANSI variable, the stream sets the
//! same color by SGR: the mascot bold (bright) red, "Claude Code" and the
//! input prompt bold white, the tool's argument cyan or yellow, tool output
//! dim, the diff's removed line red and its added line green, the mode line
//! bright magenta (the frame's bold magenta). What the frame paints a
//! literal white (the request, the tool names) is the default foreground,
//! bold for the tool names, so it stays legible in light, where ANSI white
//! is a gray. The frame's 16px gaps between turns are a blank row; its 4px
//! and 8px gaps have no row of their own. The prompt block (dividers, the
//! prompt with the cursor, the mode line, the hint) sits at the bottom of
//! the grid, as in the frame, when the grid is tall enough, and right after
//! the transcript when it isn't.
//!
//! [`TerminalPane::canned`]: crate::elements::terminal::TerminalPane::canned

use shelbi_term::Size;

/// One styled run: its SGR parameters (empty for the defaults) and its
/// text.
type Span = (&'static str, &'static str);

const DEFAULT: &str = "";
const BOLD: &str = "1";
const BOLD_WHITE: &str = "1;97";
const WHITE: &str = "37";
const CYAN: &str = "36";
const YELLOW: &str = "33";
const RED: &str = "31";
const GREEN: &str = "32";
const BRIGHT_RED: &str = "91";
const BRIGHT_GREEN: &str = "92";
const BOLD_BRIGHT_GREEN: &str = "1;92";
const BRIGHT_MAGENTA: &str = "95";
const DIM: &str = "2";
const DIVIDER: &str = "2;90";

/// The mascot's three rows (block elements, as Claude Code prints them),
/// padded to one width so the identity lines up beside it.
const MASCOT: [&str; 3] = [" ▐▛███▜▌  ", "▝▜█████▛▘ ", "  ▘▘ ▝▝   "];

/// The transcript under the banner, a line per entry (an empty line is the
/// frame's gap between turns).
const TRANSCRIPT: &[&[Span]] = &[
    &[],
    &[(BOLD, "❯ "), (DEFAULT, "Add Ctrl+K as another command palette shortcut.")],
    &[],
    &[(DEFAULT, "● I’ll add Ctrl+K while keeping the existing Ctrl+P binding.")],
    &[(WHITE, "  First, I’ll check how the shortcuts are defined.")],
    &[],
    &[(BRIGHT_GREEN, "● "), (BOLD, "Read"), (CYAN, "(crates/shelbi-state/src/keymap/actions.rs)")],
    &[(DIM, "  ⎿  Read 80 lines")],
    &[],
    &[(BRIGHT_GREEN, "● "), (BOLD, "Update"), (CYAN, "(crates/shelbi-state/src/keymap/actions.rs)")],
    &[(DIM, "  ⎿  Added 1 line, removed 1 line")],
    &[(WHITE, "     185   GlobalAction::Quit => &[\"ctrl-c\"],")],
    &[(WHITE, "     186   GlobalAction::ZenToggle => &[\"alt-z\"],")],
    &[(RED, "     187 - GlobalAction::OpenPalette => &[\"ctrl-p\"],")],
    &[(GREEN, "     187 + GlobalAction::OpenPalette => &[\"ctrl-p\", \"ctrl-k\"],")],
    &[(WHITE, "     188   GlobalAction::FocusSidebar => &[\"ctrl-h\"],")],
    &[],
    &[(BRIGHT_GREEN, "● "), (BOLD, "Bash"), (YELLOW, "(cargo test -p shelbi-state keymap)")],
    &[(DIM, "  ⎿  Running unittests src/lib.rs")],
    &[(WHITE, "     running 46 tests")],
    &[(WHITE, "     test keymap::tests::defaults ... "), (GREEN, "ok")],
    &[(WHITE, "     test keymap::tests::parse_chord ... "), (GREEN, "ok")],
    &[(DIM, "     …")],
    &[(WHITE, "     test result: "), (BOLD_BRIGHT_GREEN, "ok. 46 passed; 0 failed")],
    &[],
    &[(DEFAULT, "● Done. Both shortcuts now open the command palette.")],
    &[(WHITE, "  • Ctrl+K — added")],
    &[(WHITE, "  • Ctrl+P — unchanged")],
    &[(WHITE, "  • All 46 keymap tests pass")],
    &[],
    &[(DIM, "✻ Worked for 38s")],
];

/// Rows the prompt block takes: a divider, the prompt, a divider, the mode
/// line and the hint.
const PROMPT_ROWS: u16 = 5;

/// The stream for a `size` grid, drawn from a clear screen.
pub fn stream(size: Size) -> Vec<u8> {
    let mut out = String::new();
    // The banner: the mascot with the session's identity beside it.
    let identity: [Span; 3] = [
        (BOLD_WHITE, "Claude Code"),
        (WHITE, "Sonnet · Shelbi"),
        (CYAN, "~/Workspaces/shelbi"),
    ];
    for (row, (mascot, id)) in MASCOT.iter().zip(identity).enumerate() {
        if row > 0 {
            out.push_str("\r\n");
        }
        span(&mut out, (BRIGHT_RED, mascot));
        out.push_str("  ");
        span(&mut out, id);
    }
    for line in TRANSCRIPT {
        out.push_str("\r\n");
        for s in *line {
            span(&mut out, *s);
        }
    }
    let used = (MASCOT.len() + TRANSCRIPT.len()) as u16;
    // The prompt block at the foot of the grid, or after a blank row when
    // the transcript leaves no room.
    if used + 1 + PROMPT_ROWS <= size.rows {
        out.push_str(&format!("\x1b[{};1H", size.rows - PROMPT_ROWS + 1));
    } else {
        out.push_str("\r\n\r\n");
    }
    let divider = "─".repeat(size.cols as usize);
    span(&mut out, (DIVIDER, &divider));
    out.push_str("\r\n");
    span(&mut out, (BOLD_WHITE, "❯ "));
    out.push_str("\r\n");
    span(&mut out, (DIVIDER, &divider));
    out.push_str("\r\n");
    span(&mut out, (BRIGHT_MAGENTA, "⏵⏵ accept edits on "));
    span(&mut out, (DIM, "(shift+tab to cycle)"));
    out.push_str("\r\n");
    span(&mut out, (DIM, "? for shortcuts"));
    // The cursor waits in the prompt, after `❯ `: three rows up.
    out.push_str("\x1b[3A\x1b[3G");
    out.into_bytes()
}

/// Write `text` in `sgr`'s style, then reset.
fn span(out: &mut String, (sgr, text): (&str, &str)) {
    if sgr.is_empty() {
        out.push_str(text);
    } else {
        out.push_str(&format!("\x1b[{sgr}m{text}\x1b[0m"));
    }
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
