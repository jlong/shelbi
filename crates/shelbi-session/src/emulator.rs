//! The session's headless terminal emulator.
//!
//! One per session process. It is fed every byte the child writes (so the
//! session owns authoritative screen state even with no client attached), and it
//! answers two questions the rest of the session asks:
//!
//! * **Where is the cursor?** — the [`responder`](crate::responder) needs a live
//!   cursor for DSR-6 cursor-position replies.
//! * **What does the screen say?** — `snapshot` and the `final.txt` written on
//!   exit render out of here.
//!
//! The crate is the vendored `alacritty_terminal` fork chosen in
//! `rt-spike-emulator-replay`, the only candidate exposing both screen buffers,
//! saved cursors, modes, and history (which attach-replay, owned by `rt-replay`,
//! serializes). This module uses only the headless subset: construct, feed,
//! read cursor, resize, render text. The **kitty keyboard protocol is enabled**
//! (Claude Code's Shift+Enter rides on it), which on this crate means setting
//! `Config.kitty_keyboard` at construction.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::responder::CursorSource;

/// Scrollback depth kept in memory, per the plan's "History" section.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A terminal size as the emulator's `Dimensions` trait wants it. `total_lines`
/// only sets the viewport; scrollback depth comes from `Config.scrolling_history`
/// (see [`Emulator::new`]), so this reports the visible rows for both.
#[derive(Clone, Copy)]
struct Size {
    cols: usize,
    rows: usize,
}

impl Dimensions for Size {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// The headless emulator: a `Term` plus the single long-lived parser that feeds
/// it (the parser buffers partial escape sequences across `feed` calls, so there
/// must be exactly one for the session's lifetime).
pub struct Emulator {
    term: Term<VoidListener>,
    parser: Processor,
    size: Size,
}

impl Emulator {
    /// Build an emulator for a `cols x rows` screen with the kitty keyboard
    /// protocol enabled and [`SCROLLBACK_LINES`] of history.
    pub fn new(cols: u16, rows: u16) -> Self {
        let size = Size {
            cols: cols.max(1) as usize,
            rows: rows.max(1) as usize,
        };
        let config = Config {
            scrolling_history: SCROLLBACK_LINES,
            kitty_keyboard: true,
            ..Config::default()
        };
        let term = Term::new(config, &size, VoidListener);
        Self {
            term,
            parser: Processor::new(),
            size,
        }
    }

    /// Feed a chunk of raw child output into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the screen. No-op if the dimensions are unchanged.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let size = Size {
            cols: cols.max(1) as usize,
            rows: rows.max(1) as usize,
        };
        self.size = size;
        self.term.resize(size);
    }

    /// Whether the kitty keyboard protocol's "disambiguate escape codes" mode is
    /// currently active (set by the child pushing the kitty flags). Used by tests
    /// to confirm the protocol is wired on.
    pub fn kitty_disambiguate_active(&self) -> bool {
        self.term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES)
    }

    /// The visible screen rendered as text, one line per row, trailing blank
    /// rows removed and trailing whitespace trimmed per row (the `capture-pane
    /// -p -J` shape the detectors expect).
    pub fn visible_text(&self) -> String {
        self.render(0)
    }

    /// The visible screen preceded by up to `history_lines` of scrollback,
    /// rendered as text. Used for `final.txt` and history-bearing snapshots.
    pub fn screen_with_history(&self, history_lines: usize) -> String {
        self.render(history_lines)
    }

    /// Render `history_lines` of scrollback (clamped to what's retained) plus the
    /// whole visible screen to text.
    fn render(&self, history_lines: usize) -> String {
        let grid = self.term.grid();
        let cols = grid.columns();
        let screen = grid.screen_lines() as i32;
        let hist = (grid.history_size() as i32).min(history_lines as i32);
        let mut out: Vec<String> = Vec::with_capacity((hist + screen) as usize);
        for line in (-hist)..screen {
            let row = &grid[Line(line)];
            let mut s = String::with_capacity(cols);
            for col in 0..cols {
                let cell = &row[Column(col)];
                // The trailing half of a wide glyph (and the padding before a
                // wide glyph that would overflow the line) is not a real
                // column of text; skip it so widths match a real terminal.
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                let c = cell.c;
                s.push(if c == '\0' { ' ' } else { c });
            }
            out.push(s.trim_end().to_string());
        }
        // Drop trailing blank rows so a mostly-empty screen doesn't render as a
        // wall of newlines.
        while out.last().map(|l| l.is_empty()).unwrap_or(false) {
            out.pop();
        }
        out.join("\n")
    }
}

impl CursorSource for Emulator {
    fn cursor_1based(&self) -> (u16, u16) {
        let p = self.term.grid().cursor.point;
        let row = (p.line.0 + 1).max(1) as u16;
        let col = (p.column.0 as i64 + 1).max(1) as u16;
        (row, col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_plain_text_written_to_the_screen() {
        let mut e = Emulator::new(80, 24);
        e.feed(b"hello world\r\n");
        assert_eq!(e.visible_text(), "hello world");
    }

    #[test]
    fn tracks_cursor_position_for_dsr_replies() {
        let mut e = Emulator::new(80, 24);
        // Move the cursor to row 5, col 10 (CUP is 1-based in the sequence).
        e.feed(b"\x1b[5;10H");
        assert_eq!(e.cursor_1based(), (5, 10));
    }

    #[test]
    fn kitty_protocol_is_enabled_and_responds_to_push() {
        let mut e = Emulator::new(80, 24);
        // Not active until the child pushes it.
        assert!(!e.kitty_disambiguate_active());
        // Push "disambiguate escape codes" (bit 0). With kitty_keyboard off this
        // would be a no-op; it taking effect proves the protocol is enabled.
        e.feed(b"\x1b[>1u");
        assert!(e.kitty_disambiguate_active());
    }

    #[test]
    fn history_is_rendered_above_the_visible_screen() {
        let mut e = Emulator::new(10, 2);
        // Three lines on a 2-row screen scroll the first into history.
        e.feed(b"one\r\ntwo\r\nthree");
        assert_eq!(e.visible_text(), "two\nthree");
        let with_hist = e.screen_with_history(10);
        assert!(with_hist.contains("one"), "history should carry `one`: {with_hist:?}");
        assert!(with_hist.ends_with("three"));
    }

    #[test]
    fn resize_changes_reported_width() {
        let mut e = Emulator::new(80, 24);
        e.resize(100, 40);
        assert_eq!(e.size.cols, 100);
        assert_eq!(e.size.rows, 40);
    }
}
