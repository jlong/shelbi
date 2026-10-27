//! Settings ▸ Terminal's preview session: the desktop Terminal frame's
//! (`2264:7445`) Claude Code session as a canned ANSI byte stream. Each
//! client feeds it to its own terminal emulator and draws the result with
//! the same code that draws its sessions: the desktop app with its font,
//! size and ANSI colors, the TUI in the host terminal's own palette.
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

/// The stream for a `cols` x `rows` grid, drawn from a clear screen.
pub fn stream(cols: u16, rows: u16) -> Vec<u8> {
    let lines: Vec<&[Span]> = TRANSCRIPT.to_vec();
    session(&lines, cols, rows)
}

/// The transcript lines the short session keeps: the request, the diff
/// (cyan, dim, red, green) and the test run (yellow, bright green), so it
/// shows every color the full one does.
const SHORT: [usize; 8] = [0, 1, 9, 10, 13, 14, 17, 23];

/// Rows the short session takes: the banner, [`SHORT`], a blank row and the
/// prompt block.
pub const SHORT_ROWS: u16 = (MASCOT.len() + SHORT.len()) as u16 + 1 + PROMPT_ROWS;

/// A short version of the session, for a page with little room (the TUI's),
/// for a `cols`-wide grid [`SHORT_ROWS`] tall.
pub fn short(cols: u16) -> Vec<u8> {
    let lines: Vec<&[Span]> = SHORT.iter().map(|i| TRANSCRIPT[*i]).collect();
    session(&lines, cols, SHORT_ROWS)
}

/// The banner, `transcript`, then the prompt block, for a `cols` x `rows`
/// grid.
fn session(transcript: &[&[Span]], cols: u16, rows: u16) -> Vec<u8> {
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
    for line in transcript {
        out.push_str("\r\n");
        for s in *line {
            span(&mut out, *s);
        }
    }
    let used = (MASCOT.len() + transcript.len()) as u16;
    // The prompt block at the foot of the grid, or after a blank row when
    // the transcript leaves no room.
    if used + 1 + PROMPT_ROWS <= rows {
        out.push_str(&format!("\x1b[{};1H", rows - PROMPT_ROWS + 1));
    } else {
        out.push_str("\r\n\r\n");
    }
    let divider = "─".repeat(cols as usize);
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

/// Rows the whole session takes, prompt block included, with no blank rows
/// to spare: a grid this tall shows all of it.
pub const ROWS: u16 = (MASCOT.len() + TRANSCRIPT.len()) as u16 + 1 + PROMPT_ROWS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_short_session_keeps_every_color_and_fits_its_rows() {
        let text = String::from_utf8(short(50)).unwrap();
        for sgr in ["\x1b[91m", "\x1b[1;97m", "\x1b[36m", "\x1b[2m", "\x1b[31m", "\x1b[32m", "\x1b[33m", "\x1b[1;92m", "\x1b[95m"] {
            assert!(text.contains(sgr), "{sgr:?} in {text:?}");
        }
        assert!(text.contains(&format!("\x1b[{};1H", SHORT_ROWS - PROMPT_ROWS + 1)), "the prompt at its foot");
        assert_eq!(SHORT_ROWS, 17);
    }

    #[test]
    fn a_grid_rows_tall_holds_the_session_with_the_prompt_at_its_foot() {
        let text = String::from_utf8(stream(60, ROWS)).unwrap();
        // The prompt block moves to the grid's last five rows.
        assert!(text.contains(&format!("\x1b[{};1H", ROWS - PROMPT_ROWS + 1)), "{text:?}");
        assert!(text.contains(&"─".repeat(60)));
        // A shorter grid runs the prompt on after the transcript instead.
        let short = String::from_utf8(stream(20, 12)).unwrap();
        assert!(!short.contains(";1H"), "{short:?}");
        assert!(short.contains("? for shortcuts"));
    }
}
