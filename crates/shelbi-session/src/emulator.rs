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

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Flags, LineLength};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::responder::CursorSource;

/// Scrollback depth kept in memory, per the plan's "History" section.
pub const SCROLLBACK_LINES: usize = 10_000;

/// A UI-level event the emulator surfaced while being fed, which the session
/// turns into a pushed protocol event (title changed, bell). The session drains
/// these after each [`Emulator::feed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmuEvent {
    /// The program set a new window title (empty string = reset to default).
    Title(String),
    /// The program rang the bell.
    Bell,
}

/// The sink the emulator's `Term` reports UI events to. `alacritty_terminal`
/// calls [`EventListener::send_event`] with `&self`, so the state lives behind an
/// `Arc<Mutex<_>>` and the session holds a clone to read the title and drain the
/// event queue. Everything the session does not surface (clipboard, color and
/// cursor queries, `PtyWrite` — all handled by the [`responder`](crate::responder)
/// instead) is dropped here.
#[derive(Clone, Default)]
pub struct EmuSink {
    inner: Arc<Mutex<SinkState>>,
}

#[derive(Default)]
struct SinkState {
    title: Option<String>,
    events: VecDeque<EmuEvent>,
}

impl EmuSink {
    /// The window title the program last set, if any.
    pub fn title(&self) -> Option<String> {
        self.inner.lock().unwrap().title.clone()
    }

    /// Take the queued events, oldest first, leaving the queue empty.
    pub fn drain(&self) -> Vec<EmuEvent> {
        self.inner.lock().unwrap().events.drain(..).collect()
    }
}

impl EventListener for EmuSink {
    fn send_event(&self, event: Event) {
        let mut st = self.inner.lock().unwrap();
        match event {
            Event::Title(title) => {
                st.title = Some(title.clone());
                st.events.push_back(EmuEvent::Title(title));
            }
            Event::ResetTitle => {
                st.title = None;
                st.events.push_back(EmuEvent::Title(String::new()));
            }
            Event::Bell => st.events.push_back(EmuEvent::Bell),
            // The responder answers cursor/DA/color queries off the PTY directly,
            // so the emulator's own replies and every other UI event are dropped.
            _ => {}
        }
    }
}

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
    term: Term<EmuSink>,
    parser: Processor,
    size: Size,
    sink: EmuSink,
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
        let sink = EmuSink::default();
        let term = Term::new(config, &size, sink.clone());
        Self {
            term,
            parser: Processor::new(),
            size,
            sink,
        }
    }

    /// Feed a chunk of raw child output into the emulator.
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// The window title the program last set, if any.
    pub fn title(&self) -> Option<String> {
        self.sink.title()
    }

    /// Take the UI events (title changes, bells) the program emitted since the
    /// last drain, oldest first. The session turns these into pushed protocol
    /// events.
    pub fn drain_events(&self) -> Vec<EmuEvent> {
        self.sink.drain()
    }

    /// Current `(cols, rows)` of the emulator screen.
    pub fn size(&self) -> (u16, u16) {
        (self.size.cols as u16, self.size.rows as u16)
    }

    /// Whether the program is on the alternate screen.
    pub fn alt_screen_active(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether bracketed-paste mode is enabled (so a paste is wrapped in
    /// `ESC [ 200 ~ … ESC [ 201 ~`).
    pub fn bracketed_paste_active(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
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
    /// whole visible screen to text, in the shape `tmux capture-pane -p -J`
    /// produces (see [`crate::emulator`] and the `capture_parity` integration
    /// test):
    ///
    /// * **Wrapped lines are joined.** A physical row whose last cell carries the
    ///   [`WRAPLINE`](Flags::WRAPLINE) flag flowed into the next row, so the two
    ///   render as one logical line with no break between them — exactly what
    ///   `-J` does. The join spans the history/screen boundary, so a line that
    ///   wrapped at the top of the visible screen still joins with its tail in
    ///   history.
    /// * **Trailing whitespace is trimmed per logical line.** Each row's content
    ///   stops at its [occupied length](LineLength::line_length) (trailing
    ///   default cells dropped), and the joined line is `trim_end`ed. tmux's `-J`
    ///   *preserves* trailing spaces, but the screen detectors this feeds
    ///   ([`crate`] clients, `ready.rs`/`submit.rs`) are trailing-whitespace
    ///   insensitive, and exact byte parity is unreachable anyway: this crate's
    ///   emulator and tmux track a line's "used" extent differently once a
    ///   program issues an erase-to-end-of-line. The parity test normalizes both
    ///   sides' trailing whitespace for that reason.
    /// * **Trailing blank rows are dropped**, so a mostly-empty screen does not
    ///   render as a wall of newlines, and there is no trailing newline.
    fn render(&self, history_lines: usize) -> String {
        let grid = self.term.grid();
        let cols = grid.columns();
        let screen = grid.screen_lines() as i32;
        let hist = (grid.history_size() as i32).min(history_lines as i32);
        let mut out: Vec<String> = Vec::with_capacity((hist + screen) as usize);
        // The logical line under construction, extended across every physical
        // row that wrapped into the next (so `-J`'s line joining is reproduced).
        let mut current = String::with_capacity(cols);
        let mut joining = false;
        for line in (-hist)..screen {
            let row = &grid[Line(line)];
            if !joining {
                current.clear();
            }
            // A wrapped row is full to the edge, so render every column; a
            // non-wrapped row stops at its occupied length so unused trailing
            // cells don't become spaces. `line_length()` returns the full width
            // for a wrapped row, so one bound covers both.
            let len = row.line_length().0;
            for col in 0..len {
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
                current.push(if c == '\0' { ' ' } else { c });
            }
            // Does this row flow into the next? The wrap flag lives on the row's
            // last cell.
            joining = cols > 0 && row[Column(cols - 1)].flags.contains(Flags::WRAPLINE);
            if !joining {
                out.push(current.trim_end().to_string());
            }
        }
        // A screen that ends mid-wrap (the bottom row wrapped) still has an
        // unflushed logical line.
        if joining {
            out.push(current.trim_end().to_string());
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
    fn wrapped_lines_are_joined_like_capture_pane_j() {
        // A line longer than the screen width wraps onto the next physical row.
        // `-J` (and so `visible_text`) joins the two back into one logical line
        // with no break, rather than emitting the wrap as a newline.
        let mut e = Emulator::new(10, 4);
        e.feed(b"abcdefghijXYZ");
        assert_eq!(e.visible_text(), "abcdefghijXYZ");
    }

    #[test]
    fn wrap_join_spans_the_history_boundary() {
        // A line that wraps right at the top of the visible screen still joins
        // with the tail that scrolled into history when history is requested.
        let mut e = Emulator::new(6, 2);
        // 10 chars on a 6-col, 2-row screen: wraps to row0 "abcdef" + row1
        // "ghij", then a newline pushes the wrapped pair up so "abcdef" lands in
        // history while "ghij" stays on the visible screen.
        e.feed(b"abcdefghij\r\nlast");
        let joined = e.screen_with_history(10);
        assert!(
            joined.contains("abcdefghij"),
            "wrapped line must rejoin across the history boundary: {joined:?}"
        );
        assert!(joined.ends_with("last"));
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
        assert_eq!(e.size(), (100, 40));
    }

    #[test]
    fn captures_title_changes_as_events() {
        let mut e = Emulator::new(80, 24);
        // OSC 0 sets both icon name and window title.
        e.feed(b"\x1b]0;my-agent\x07");
        assert_eq!(e.title().as_deref(), Some("my-agent"));
        let events = e.drain_events();
        assert!(events.contains(&EmuEvent::Title("my-agent".into())));
        // Draining twice yields nothing the second time.
        assert!(e.drain_events().is_empty());
    }

    #[test]
    fn captures_bell_as_an_event() {
        let mut e = Emulator::new(80, 24);
        e.feed(b"ding\x07");
        assert!(e.drain_events().contains(&EmuEvent::Bell));
    }

    #[test]
    fn tracks_bracketed_paste_and_alt_screen_modes() {
        let mut e = Emulator::new(80, 24);
        assert!(!e.bracketed_paste_active());
        assert!(!e.alt_screen_active());
        // Enable bracketed paste (DECSET 2004) and the alternate screen (1049).
        e.feed(b"\x1b[?2004h\x1b[?1049h");
        assert!(e.bracketed_paste_active());
        assert!(e.alt_screen_active());
        e.feed(b"\x1b[?2004l\x1b[?1049l");
        assert!(!e.bracketed_paste_active());
        assert!(!e.alt_screen_active());
    }
}
