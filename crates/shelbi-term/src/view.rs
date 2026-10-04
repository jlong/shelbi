//! The composed terminal view.
//!
//! [`TerminalView`] ties the client-side [`emulator`](crate::emulator) to
//! [`scrollback`](crate::scrollback), [`selection`](crate::selection), and
//! [`search`](crate::search), and is the single place the
//! **normal-screen-only** rule lives: scrollback, selection, and search are
//! disabled whenever the program is on the alternate screen (a full-screen
//! program such as an editor or pager owns the whole screen, so Shelbi offers
//! none of them). A UI can use the lower-level pieces directly, but this is the
//! intended entry point.
//!
//! The view does not own input encoding; see [`crate::input`]. Nor does it do
//! clipping/letterboxing, which is a render-time concern over
//! [`crate::viewport`]; the view only exposes the helper that maps a viewer
//! cell to a grid point for selection.

use alacritty_terminal::grid::Scroll;

use crate::emulator::TermEmulator;
use crate::scrollback::Scrollback;
use crate::search::{Match, Search};
use crate::selection::{Point, Selection};
use crate::viewport::Placement;
use crate::Size;

/// A session's terminal view: emulator plus normal-screen scrollback,
/// selection, and search.
pub struct TerminalView {
    emu: TermEmulator,
    scrollback: Scrollback,
    selection: Option<Selection>,
    search: Option<Search>,
}

impl TerminalView {
    /// A fresh view locked to `size`.
    pub fn new(size: Size) -> Self {
        Self {
            emu: TermEmulator::new(size),
            scrollback: Scrollback::new(),
            selection: None,
            search: None,
        }
    }

    /// The underlying emulator (for rendering: grid, mode, cursor).
    pub fn emulator(&self) -> &TermEmulator {
        &self.emu
    }

    /// Feed session output into the emulator.
    ///
    /// The emulator keeps the scroll position stable as output pushes lines
    /// into history, so the view mirrors the emulator's resulting offset rather
    /// than forcing the bottom. Leaving the alternate screen (quitting a
    /// full-screen program) clears any stale selection/search, since those
    /// only make sense on the normal screen.
    pub fn feed(&mut self, bytes: &[u8]) {
        let was_alt = self.emu.on_alt_screen();
        self.emu.feed(bytes);
        self.scrollback = Scrollback::at(self.emu.display_offset());
        if self.emu.on_alt_screen() != was_alt {
            self.selection = None;
            self.search = None;
        }
    }

    /// Lock the view to a new session size.
    pub fn resize(&mut self, size: Size) {
        self.emu.resize(size);
        self.scrollback.clamp(self.emu.history_len());
        // A reflow invalidates fixed grid coordinates.
        self.selection = None;
        self.search = None;
    }

    /// Note that the user sent input: the view follows the live bottom again.
    pub fn on_user_input(&mut self) {
        self.scroll_to_bottom();
    }

    // ---- scrollback (normal screen only) ----

    /// Whether Shelbi scrollback is available (i.e. the normal screen).
    pub fn scrollback_enabled(&self) -> bool {
        !self.emu.on_alt_screen()
    }

    /// The current scrollback offset above the live bottom (`0` at the bottom).
    /// Always `0` on the alternate screen.
    pub fn scroll_offset(&self) -> usize {
        self.emu.display_offset()
    }

    fn apply_scroll(&mut self, target: usize) {
        let target = target.min(self.emu.history_len());
        let delta = target as i32 - self.emu.display_offset() as i32;
        if delta != 0 {
            self.emu.scroll(Scroll::Delta(delta));
        }
        self.scrollback = Scrollback::at(self.emu.display_offset());
    }

    /// Scroll up `lines` into history. No-op on the alternate screen.
    pub fn scroll_up(&mut self, lines: usize) {
        if !self.scrollback_enabled() {
            return;
        }
        let mut sb = self.scrollback;
        let target = sb.scroll_up(lines, self.emu.history_len());
        self.apply_scroll(target);
    }

    /// Scroll down `lines` toward the live bottom. No-op on the alternate
    /// screen.
    pub fn scroll_down(&mut self, lines: usize) {
        if !self.scrollback_enabled() {
            return;
        }
        let mut sb = self.scrollback;
        let target = sb.scroll_down(lines);
        self.apply_scroll(target);
    }

    /// Scroll up a full page. No-op on the alternate screen.
    pub fn page_up(&mut self) {
        self.scroll_up(self.emu.size().rows as usize);
    }

    /// Scroll down a full page. No-op on the alternate screen.
    pub fn page_down(&mut self) {
        self.scroll_down(self.emu.size().rows as usize);
    }

    /// Jump to the oldest retained line. No-op on the alternate screen.
    pub fn scroll_to_top(&mut self) {
        if !self.scrollback_enabled() {
            return;
        }
        self.apply_scroll(self.emu.history_len());
    }

    /// Jump to the live bottom.
    pub fn scroll_to_bottom(&mut self) {
        self.apply_scroll(0);
    }

    // ---- selection (normal screen only) ----

    /// Begin a selection at a grid point. No-op on the alternate screen.
    pub fn begin_selection(&mut self, at: Point) {
        if !self.scrollback_enabled() {
            return;
        }
        self.selection = Some(Selection::start(at));
    }

    /// Extend the in-progress selection to a grid point. No-op if there is no
    /// selection or on the alternate screen.
    pub fn update_selection(&mut self, to: Point) {
        if !self.scrollback_enabled() {
            return;
        }
        if let Some(sel) = self.selection.as_mut() {
            sel.update(to);
        }
    }

    /// The current selection, if any.
    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// The selected text, or `None` if there is no selection (or on the
    /// alternate screen).
    pub fn selection_text(&self) -> Option<String> {
        if !self.scrollback_enabled() {
            return None;
        }
        self.selection.map(|s| s.extract_text(self.emu.grid()))
    }

    /// Clear any selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Translate a viewer cell to a grid [`Point`] for selection, accounting
    /// for the current scroll offset and the viewer's [`Placement`]. Returns
    /// `None` when the cell is in letterbox margin or outside a clipped window.
    pub fn viewer_point_to_grid(&self, placement: &Placement, col: u16, row: u16) -> Option<Point> {
        let (scol, srow) = placement.viewer_to_session(col, row)?;
        let line = srow as i32 - self.emu.display_offset() as i32;
        Some(Point::new(line, scol))
    }

    // ---- search (normal screen only) ----

    /// Start a search over the buffer. No-op on the alternate screen; returns
    /// the number of matches found (0 when disabled).
    pub fn start_search(&mut self, query: &str, case_insensitive: bool) -> usize {
        if !self.scrollback_enabled() {
            self.search = None;
            return 0;
        }
        let mut search = Search::new(query, case_insensitive);
        search.run(self.emu.grid());
        let n = search.len();
        if let Some(m) = search.current() {
            self.reveal(m);
        }
        self.search = Some(search);
        n
    }

    /// Advance to the next match and scroll it into view.
    pub fn search_next(&mut self) -> Option<Match> {
        if !self.scrollback_enabled() {
            return None;
        }
        let m = self.search.as_mut()?.next_match();
        if let Some(m) = m {
            self.reveal(m);
        }
        m
    }

    /// Step to the previous match and scroll it into view.
    pub fn search_prev(&mut self) -> Option<Match> {
        if !self.scrollback_enabled() {
            return None;
        }
        let m = self.search.as_mut()?.prev_match();
        if let Some(m) = m {
            self.reveal(m);
        }
        m
    }

    /// The current search match, if a search is active.
    pub fn search_current(&self) -> Option<Match> {
        self.search.as_ref().and_then(|s| s.current())
    }

    /// Clear any active search.
    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// Scroll so a match line is visible: a history line (negative) is placed
    /// at the top of the viewport; an on-screen line is revealed by dropping to
    /// the live bottom.
    fn reveal(&mut self, m: Match) {
        let target = if m.line < 0 { (-m.line) as usize } else { 0 };
        self.apply_scroll(target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn view() -> TerminalView {
        TerminalView::new(Size::new(20, 5))
    }

    /// Produce enough output to build scrollback history on the normal screen.
    fn fill_history(v: &mut TerminalView, lines: usize) {
        for i in 0..lines {
            v.feed(format!("line {i}\r\n").as_bytes());
        }
    }

    #[test]
    fn scrollback_works_on_the_normal_screen() {
        let mut v = view();
        fill_history(&mut v, 50);
        assert!(v.scrollback_enabled());
        assert!(v.emulator().history_len() > 0);
        v.scroll_up(10);
        assert_eq!(v.scroll_offset(), 10);
        v.scroll_down(4);
        assert_eq!(v.scroll_offset(), 6);
        v.scroll_to_bottom();
        assert_eq!(v.scroll_offset(), 0);
        v.scroll_to_top();
        assert_eq!(v.scroll_offset(), v.emulator().history_len());
    }

    #[test]
    fn scrollback_is_disabled_on_the_alternate_screen() {
        let mut v = view();
        fill_history(&mut v, 50);
        v.feed(b"\x1b[?1049h"); // enter alternate screen
        assert!(!v.scrollback_enabled());
        v.scroll_up(10);
        assert_eq!(v.scroll_offset(), 0, "scrollback must not move on the alt screen");
        v.scroll_to_top();
        assert_eq!(v.scroll_offset(), 0);
    }

    #[test]
    fn selection_works_on_the_normal_screen() {
        let mut v = view();
        v.feed(b"hello world");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        assert_eq!(v.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn selection_is_disabled_on_the_alternate_screen() {
        let mut v = view();
        v.feed(b"\x1b[?1049h");
        v.feed(b"hello world");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        assert!(v.selection().is_none(), "no selection should start on the alt screen");
        assert_eq!(v.selection_text(), None);
    }

    #[test]
    fn search_works_on_the_normal_screen() {
        let mut v = view();
        v.feed(b"needle here\r\nand needle again");
        let n = v.start_search("needle", false);
        assert_eq!(n, 2);
        assert_eq!(v.search_current().unwrap().line, 0);
        assert_eq!(v.search_next().unwrap().line, 1);
    }

    #[test]
    fn search_is_disabled_on_the_alternate_screen() {
        let mut v = view();
        v.feed(b"\x1b[?1049h");
        v.feed(b"needle here");
        assert_eq!(v.start_search("needle", false), 0);
        assert_eq!(v.search_current(), None);
        assert_eq!(v.search_next(), None);
    }

    #[test]
    fn leaving_the_alt_screen_clears_selection_and_search() {
        let mut v = view();
        v.feed(b"hello");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        v.start_search("hello", false);
        v.feed(b"\x1b[?1049h"); // enter alt
        assert!(v.selection().is_none());
        assert!(v.search_current().is_none());
    }
}
