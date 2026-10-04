//! The client-side terminal emulator.
//!
//! Each terminal view owns one [`TermEmulator`], a headless
//! `alacritty_terminal` (the vendored fork chosen in the
//! `rt-spike-emulator-replay` Phase 0 spike) fed by the session's output
//! stream. The client locks its emulator to the session's size — a `resized`
//! frame travels in the output stream — so every emulator reflows at the same
//! point in the byte stream.
//!
//! ## Query replies are discarded
//!
//! A terminal program sends queries (cursor-position report `ESC[6n`, device
//! attributes, color and text-area queries, OSC clipboard loads) and expects a
//! reply written back to the PTY. **Only the session process answers them**,
//! because it is the one beside the real PTY and answers at once with no
//! clients attached (Codex quits if the cursor-position reply is late). The
//! client-side emulator here generates the same replies as a side effect of
//! parsing, and throws every one of them away: nothing this emulator produces
//! is ever written to the session. [`TermEmulator::discarded_query_replies`]
//! counts them so a test can prove a reply was generated *and* dropped.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Grid, Scroll};
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::Processor;

use crate::Size;

/// Lines of scrollback the client-side emulator retains, matching the session
/// process (plan, "History": 10,000 lines).
const SCROLLBACK_LINES: usize = 10_000;

/// A [`Dimensions`] adaptor for a plain [`Size`], used to build and resize the
/// terminal without depending on `alacritty_terminal`'s test-only `TermSize`.
#[derive(Clone, Copy)]
struct Dims {
    cols: usize,
    rows: usize,
}

impl Dimensions for Dims {
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

impl From<Size> for Dims {
    fn from(size: Size) -> Self {
        let size = size.non_zero();
        Dims { cols: size.cols as usize, rows: size.rows as usize }
    }
}

/// An [`EventListener`] that drops every event the emulator emits.
///
/// Events that carry a reply the program is waiting for (a written byte string
/// or a formatter closure the caller would invoke to produce one) are counted
/// before being dropped, so the emulator can report that it discarded a reply
/// rather than forwarding it. All other events (title changes, bells, wakeups)
/// are simply ignored here; the view layer learns about those from the
/// session's pushed events, not from this local emulator.
#[derive(Clone, Default)]
struct DiscardProxy {
    discarded_replies: Arc<AtomicUsize>,
}

impl EventListener for DiscardProxy {
    fn send_event(&self, event: Event) {
        let is_query_reply = matches!(
            event,
            Event::PtyWrite(_)
                | Event::ColorRequest(..)
                | Event::TextAreaSizeRequest(_)
                | Event::ClipboardLoad(..)
        );
        if is_query_reply {
            self.discarded_replies.fetch_add(1, Ordering::Relaxed);
        }
        // Every event is dropped here on purpose; see the module docs.
    }
}

/// The client-side emulator: feed it output, read its grid and mode to render.
pub struct TermEmulator {
    term: Term<DiscardProxy>,
    parser: Processor,
    discarded_replies: Arc<AtomicUsize>,
    size: Size,
}

impl TermEmulator {
    /// A fresh emulator locked to `size`, with the kitty keyboard protocol
    /// enabled (Claude Code's Shift+Enter depends on it) and the session's
    /// scrollback depth.
    pub fn new(size: Size) -> Self {
        let discarded_replies = Arc::new(AtomicUsize::new(0));
        let proxy = DiscardProxy { discarded_replies: Arc::clone(&discarded_replies) };
        let config = Config { scrolling_history: SCROLLBACK_LINES, kitty_keyboard: true, ..Config::default() };
        let term = Term::new(config, &Dims::from(size), proxy);
        Self { term, parser: Processor::new(), discarded_replies, size }
    }

    /// Feed a chunk of raw output bytes from the session into the emulator.
    ///
    /// The stream may be chunked anywhere: a single long-lived parser buffers
    /// partial escape sequences and multi-byte characters across calls. (Frame
    /// *edges* are split only at parser-rest boundaries by the session; that is
    /// a protocol concern, not this emulator's.)
    pub fn feed(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    /// The size the emulator is locked to.
    pub fn size(&self) -> Size {
        self.size
    }

    /// Lock the emulator to a new session size (driven by a `resized` frame in
    /// the output stream). A no-op if the size is unchanged.
    pub fn resize(&mut self, size: Size) {
        if size == self.size {
            return;
        }
        self.term.resize(Dims::from(size));
        self.size = size;
    }

    /// The active screen buffer (primary or alternate), for rendering.
    pub fn grid(&self) -> &Grid<Cell> {
        self.term.grid()
    }

    /// The current terminal mode flags.
    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// Whether the program is on the alternate screen (a full-screen program
    /// such as an editor or pager). Shelbi's scrollback, selection, and search
    /// are disabled here (plan, "Shared client crates").
    pub fn on_alt_screen(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    /// Whether the program enabled bracketed paste.
    pub fn bracketed_paste(&self) -> bool {
        self.term.mode().contains(TermMode::BRACKETED_PASTE)
    }

    /// Whether the program enabled focus reporting (`ESC[?1004h`).
    pub fn focus_reporting(&self) -> bool {
        self.term.mode().contains(TermMode::FOCUS_IN_OUT)
    }

    /// How many lines of scrollback history the active buffer holds. This is
    /// the maximum scrollback offset a viewer can reach.
    pub fn history_len(&self) -> usize {
        self.term.grid().history_size()
    }

    /// The current scrollback display offset (0 = pinned to the live bottom).
    pub fn display_offset(&self) -> usize {
        self.term.grid().display_offset()
    }

    /// Drive the scrollback viewport. Callers should prefer
    /// [`crate::view::TerminalView`], which gates this on the normal screen.
    pub fn scroll(&mut self, scroll: Scroll) {
        self.term.scroll_display(scroll);
    }

    /// The number of query replies this emulator generated and discarded (see
    /// the module docs). Non-zero after the program sends e.g. a cursor-position
    /// request; the bytes are never forwarded to the session.
    pub fn discarded_query_replies(&self) -> usize {
        self.discarded_replies.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn emu() -> TermEmulator {
        TermEmulator::new(Size::new(80, 24))
    }

    #[test]
    fn feeds_plain_text_into_the_grid() {
        let mut e = emu();
        e.feed(b"hello");
        let grid = e.grid();
        let row: String = (0..5).map(|c| grid[alacritty_terminal::index::Line(0)][alacritty_terminal::index::Column(c)].c).collect();
        assert_eq!(row, "hello");
    }

    #[test]
    fn query_replies_are_generated_then_discarded() {
        let mut e = emu();
        assert_eq!(e.discarded_query_replies(), 0);
        // Device Status Report (cursor position): the program expects a reply.
        e.feed(b"\x1b[6n");
        // Primary Device Attributes: another query.
        e.feed(b"\x1b[c");
        assert!(
            e.discarded_query_replies() >= 2,
            "the emulator should have generated replies to both queries and dropped them, \
             got {}",
            e.discarded_query_replies()
        );
    }

    #[test]
    fn tracks_the_alternate_screen() {
        let mut e = emu();
        assert!(!e.on_alt_screen());
        e.feed(b"\x1b[?1049h");
        assert!(e.on_alt_screen());
        e.feed(b"\x1b[?1049l");
        assert!(!e.on_alt_screen());
    }

    #[test]
    fn tracks_bracketed_paste_and_focus_modes() {
        let mut e = emu();
        assert!(!e.bracketed_paste());
        assert!(!e.focus_reporting());
        e.feed(b"\x1b[?2004h");
        e.feed(b"\x1b[?1004h");
        assert!(e.bracketed_paste());
        assert!(e.focus_reporting());
    }

    #[test]
    fn resize_relocks_the_size() {
        let mut e = emu();
        assert_eq!(e.size(), Size::new(80, 24));
        e.resize(Size::new(100, 40));
        assert_eq!(e.size(), Size::new(100, 40));
        assert_eq!(e.grid().columns(), 100);
        assert_eq!(e.grid().screen_lines(), 40);
    }
}
