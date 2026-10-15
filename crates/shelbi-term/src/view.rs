//! The composed terminal view.
//!
//! [`TerminalView`] ties the client-side [`emulator`](crate::emulator) to
//! [`scrollback`](crate::scrollback), [`selection`](crate::selection), and
//! [`search`](crate::search), and is the single place the screen rules live:
//!
//! - **Scrollback is normal-screen only.** A full-screen program (an editor,
//!   a pager) on the alternate screen has no history; it scrolls itself, and
//!   a viewer's wheel reaches it as arrow keys (alternate-scroll mode, see
//!   [`crate::gesture`]).
//! - **Selection and search work on both screens.** On the alternate screen
//!   they cover what is on screen (there is no history to search), so text
//!   can be copied out of `less` or `vim`. Switching screens clears both,
//!   since their coordinates belong to the other buffer.
//!
//! A UI can use the lower-level pieces directly, but this is the intended
//! entry point.
//!
//! The view does not own input encoding; see [`crate::input`]. Nor does it do
//! clipping/letterboxing, which is a render-time concern over
//! [`crate::viewport`]; the view only exposes the helper that maps a viewer
//! cell to a grid point for selection.

use alacritty_terminal::grid::Scroll;
use alacritty_terminal::index::{Column, Line, Point as GridPoint, Side};
use alacritty_terminal::selection::{Selection as TermSelection, SelectionType};

use crate::emulator::TermEmulator;
use crate::links::{self, Link};
use crate::scrollback::Scrollback;
use crate::search::{Match, Search};
use crate::selection::{Point, Selection, SelectionMode};
use crate::viewport::{self, Placement};
use crate::Size;

/// A selection longer than this many lines isn't re-read after every batch of
/// output to see whether its text changed (it still moves with scrolling and
/// is dropped when the emulator erases text under it).
const SELECTION_WATCH_LINES: i32 = 2_000;

/// A session's terminal view: emulator plus normal-screen scrollback,
/// selection, and search.
pub struct TerminalView {
    emu: TermEmulator,
    scrollback: Scrollback,
    /// The live selection's mode and the text it covered when last set. The
    /// selection itself lives in the emulator (`Term::selection`), which moves
    /// it with its text as output scrolls into history and drops it when that
    /// text is erased; the text here catches an overwrite in place.
    selection: Option<(SelectionMode, String)>,
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
    /// than forcing the bottom (new output never yanks a scrolled-up view).
    /// A selection stays on its text as that text scrolls, and is dropped
    /// once the text under it changes. Leaving the alternate screen (quitting
    /// a full-screen program) clears any stale selection/search, since those
    /// only make sense on the normal screen. An open search's matches are not
    /// re-scanned here (that is a full-history scan); call
    /// [`TerminalView::refresh_search`] when convenient.
    pub fn feed(&mut self, bytes: &[u8]) {
        let was_alt = self.emu.on_alt_screen();
        self.emu.feed(bytes);
        self.scrollback = Scrollback::at(self.emu.display_offset());
        if self.emu.on_alt_screen() != was_alt {
            self.clear_selection();
            self.search = None;
        }
        self.drop_selection_if_changed();
    }

    /// Lock the view to a new session size.
    pub fn resize(&mut self, size: Size) {
        self.emu.resize(size);
        self.scrollback.clamp(self.emu.history_len());
        // A reflow invalidates fixed grid coordinates.
        self.clear_selection();
        self.search = None;
    }

    /// The viewport rows that changed since the last call (see
    /// [`TermEmulator::take_damage`]). Output, a resize, and a scrollback move
    /// all damage the screen; selection and search do not (a viewer repaints
    /// those itself when it changes them).
    pub fn take_damage(&mut self) -> crate::render::Damage {
        self.emu.take_damage()
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

    // ---- selection (either screen) ----

    /// Begin a cell selection at a grid point.
    pub fn begin_selection(&mut self, at: Point) {
        self.begin_selection_mode(at, SelectionMode::Cell);
    }

    /// Begin a selection in `mode` at a grid point: a word or line selection
    /// covers its word or line at once.
    pub fn begin_selection_mode(&mut self, at: Point, mode: SelectionMode) {
        let ty = match mode {
            SelectionMode::Cell => SelectionType::Simple,
            SelectionMode::Word => SelectionType::Semantic,
            SelectionMode::Line => SelectionType::Lines,
            SelectionMode::Block => SelectionType::Block,
        };
        self.emu.term_mut().selection = Some(TermSelection::new(ty, grid_point(at), Side::Left));
        self.selection = Some((mode, String::new()));
        self.remember_selection_text();
    }

    /// Extend the in-progress selection to a grid point. No-op if there is no
    /// selection.
    pub fn update_selection(&mut self, to: Point) {
        let Some((mode, _)) = self.selection else {
            return;
        };
        let Some(current) = self.emu.term().selection.as_ref() else {
            return;
        };
        // Both end cells are selected whichever way the drag runs: the side
        // of each end faces outward. A block decides by column.
        let anchor = current.anchor();
        let to = grid_point(to);
        let forward = match mode {
            SelectionMode::Block => to.column >= anchor.column,
            _ => to >= anchor,
        };
        let (anchor_side, focus_side) = if to == anchor {
            (Side::Left, Side::Left)
        } else if forward {
            (Side::Left, Side::Right)
        } else {
            (Side::Right, Side::Left)
        };
        let mut sel = TermSelection::new(current.ty, anchor, anchor_side);
        sel.update(to, focus_side);
        self.emu.term_mut().selection = Some(sel);
        self.remember_selection_text();
    }

    /// The current selection, resolved (a word or line selection already
    /// grown to its boundaries), or `None` with no selection or one that
    /// never left its starting cell.
    pub fn selection(&self) -> Option<Selection> {
        let (mode, _) = self.selection.as_ref()?;
        let range = self.emu.term().selection.as_ref()?.to_range(self.emu.term())?;
        let mut sel = Selection::with_mode(view_point(range.start), *mode);
        sel.update(view_point(range.end));
        Some(sel)
    }

    /// The selected text, or `None` if there is no selection.
    pub fn selection_text(&self) -> Option<String> {
        self.selection().map(|s| s.extract_text(self.emu.grid()))
    }

    /// Clear any selection.
    pub fn clear_selection(&mut self) {
        self.selection = None;
        self.emu.term_mut().selection = None;
    }

    fn remember_selection_text(&mut self) {
        let text = self.watched_selection_text().unwrap_or_default();
        if let Some((_, remembered)) = self.selection.as_mut() {
            *remembered = text;
        }
    }

    /// The selection's text, when it is short enough to watch.
    fn watched_selection_text(&self) -> Option<String> {
        let sel = self.selection()?;
        let (start, end) = sel.range();
        (end.line - start.line <= SELECTION_WATCH_LINES).then(|| sel.extract_text(self.emu.grid()))
    }

    /// After output: drop a selection the emulator already dropped (its text
    /// was erased) or whose text changed in place.
    fn drop_selection_if_changed(&mut self) {
        let Some((_, remembered)) = self.selection.as_ref() else {
            return;
        };
        if self.emu.term().selection.is_none() {
            self.selection = None;
            return;
        }
        if let Some(now) = self.watched_selection_text() {
            if &now != remembered {
                self.clear_selection();
            }
        }
    }

    /// How this view's `session`-sized screen sits in a `viewer`: letterboxed
    /// when the viewer is larger, clipped when it is smaller, with a clipped
    /// window that keeps the screen's content (and the cursor) in view rather
    /// than its top ([`viewport::fit_showing`]). Every client paints and maps
    /// the pointer through this one placement.
    pub fn placement(&self, session: Size, viewer: Size) -> Placement {
        if viewer.rows >= session.rows {
            // Nothing to anchor: skip the screen scan.
            return viewport::fit(session, viewer);
        }
        viewport::fit_showing(session, viewer, self.emu.content_bottom(), self.emu.cursor_screen_row())
    }

    /// The grid line (0 = top of the live screen, negative = history) shown on
    /// the first row of `placement`'s window at the current scroll position.
    /// Viewer row `r` of the window shows line `first_shown_line + r`.
    pub fn first_shown_line(&self, placement: &Placement) -> i32 {
        placement.rows.src_start as i32 - self.emu.display_offset() as i32
    }

    /// Translate a viewer cell to a grid [`Point`] for selection, accounting
    /// for the current scroll offset and the viewer's [`Placement`]. Returns
    /// `None` when the cell is in letterbox margin or outside a clipped window.
    pub fn viewer_point_to_grid(&self, placement: &Placement, col: u16, row: u16) -> Option<Point> {
        let (scol, srow) = placement.viewer_to_session(col, row)?;
        let line = srow as i32 - self.emu.display_offset() as i32;
        Some(Point::new(line, scol))
    }

    // ---- search (either screen) ----

    /// Start a search over the buffer (history and screen; on the alternate
    /// screen, just the screen). Returns the number of matches found.
    ///
    /// The current match is the newest one at or above the bottom of what is
    /// on screen (the match nearest where the user is looking, searching back
    /// through history), else the newest overall; it is scrolled into view.
    pub fn start_search(&mut self, query: &str, case_insensitive: bool) -> usize {
        let mut search = Search::new(query, case_insensitive);
        search.run(self.emu.grid());
        search.select_last_at_or_before(self.viewport_bottom_line());
        let n = search.len();
        if let Some(m) = search.current() {
            self.reveal(m);
        }
        self.search = Some(search);
        n
    }

    /// Re-scan an open search after output changed the buffer, keeping the
    /// current match when it is still there (else the one at the same place
    /// counting from the newest). Doesn't scroll. Returns the match count.
    pub fn refresh_search(&mut self) -> usize {
        let grid = self.emu.grid();
        match self.search.as_mut() {
            Some(search) => {
                search.rerun(grid);
                search.len()
            }
            None => 0,
        }
    }

    /// Advance to the next match in buffer order (toward the live bottom,
    /// wrapping to the oldest) and scroll it into view.
    pub fn search_next(&mut self) -> Option<Match> {
        let m = self.search.as_mut()?.next_match();
        if let Some(m) = m {
            self.reveal(m);
        }
        m
    }

    /// Step to the previous match in buffer order (back into history,
    /// wrapping to the newest) and scroll it into view.
    pub fn search_prev(&mut self) -> Option<Match> {
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

    /// The current match's place as `(n, total)`, 1-based (`(0, 0)` for an
    /// open search with no matches), or `None` with no search.
    pub fn search_position(&self) -> Option<(usize, usize)> {
        let s = self.search.as_ref()?;
        Some((s.current_index().map_or(0, |i| i + 1), s.len()))
    }

    /// Every match of the active search, in buffer order (empty with no
    /// search), for painting the highlights.
    pub fn search_matches(&self) -> &[Match] {
        self.search.as_ref().map_or(&[], |s| s.matches())
    }

    /// Clear any active search.
    pub fn clear_search(&mut self) {
        self.search = None;
    }

    /// The grid line at the bottom of the viewport.
    fn viewport_bottom_line(&self) -> i32 {
        self.emu.size().rows as i32 - 1 - self.emu.display_offset() as i32
    }

    /// Scroll so a match line is visible: a match already on screen stays
    /// put; one above the viewport is placed at its top; one below it drops
    /// toward the live bottom until it shows.
    fn reveal(&mut self, m: Match) {
        let rows = self.emu.size().rows as i32;
        let offset = self.emu.display_offset() as i32;
        let top = -offset;
        let bottom = top + rows - 1;
        if m.line >= top && m.line <= bottom {
            return;
        }
        let target = if m.line < top {
            -m.line
        } else {
            // Below the viewport: the smallest offset that shows it.
            (rows - 1 - m.line).max(0)
        };
        self.apply_scroll(target.max(0) as usize);
    }

    // ---- links and accessibility text ----

    /// The URL or file path under grid point `at` (see [`crate::links`]), or
    /// `None`. Works on either screen: a full-screen program's output links
    /// too.
    pub fn link_at(&self, at: Point) -> Option<Link> {
        links::link_at(self.emu.grid(), at)
    }

    /// The text on screen, as a screen reader reads it (see [`ScreenText`]).
    pub fn screen_text(&self) -> ScreenText {
        let grid = self.emu.grid();
        let size = self.emu.size();
        let offset = self.emu.display_offset() as i32;
        let rows = (0..size.rows)
            .map(|row| {
                let line = row as i32 - offset;
                let (text, _) = crate::selection::row_text(grid, line, 0, size.cols.saturating_sub(1), size.cols);
                text
            })
            .collect();
        let cursor = self.emu.cursor().map(|c| {
            let line = c.row as i32 - offset;
            (c.row, char_index_of_col(grid, line, c.col))
        });
        ScreenText { rows, cursor }
    }
}

/// What is on screen, as text: one string per viewport row (trailing blanks
/// trimmed, wide characters once), and the cursor as `(row, character index
/// within that row's string)` when it is shown. For a screen reader.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScreenText {
    /// Each viewport row's text, top to bottom.
    pub rows: Vec<String>,
    /// The cursor's row and character index in [`ScreenText::rows`].
    pub cursor: Option<(u16, usize)>,
}

/// The character index in a row's text (as [`crate::selection::row_text`]
/// builds it, wide-character spacers skipped) of grid column `col`.
fn char_index_of_col(
    grid: &alacritty_terminal::grid::Grid<alacritty_terminal::term::cell::Cell>,
    line: i32,
    col: u16,
) -> usize {
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::term::cell::Flags;
    let cols = grid.columns();
    let row = &grid[Line(line)];
    (0..(col as usize).min(cols))
        .filter(|&c| {
            !row[Column(c)]
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
        })
        .count()
}

/// A shelbi-term point as the emulator's.
fn grid_point(p: Point) -> GridPoint {
    GridPoint::new(Line(p.line), Column(p.col as usize))
}

/// The emulator's point as a shelbi-term one.
fn view_point(p: GridPoint) -> Point {
    Point::new(p.line.0, p.column.0 as u16)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::selection::SelectionMode;

    fn view() -> TerminalView {
        TerminalView::new(Size::new(20, 5))
    }

    /// dt-terminal-bottom-clipped: a session sized by a taller client (30
    /// rows) shown in a 10-row viewer, an agent's prompt and status line on
    /// its last rows. Scrolled to the bottom, the window's last row is the
    /// screen's last row; scrolling up walks back through the rows above it.
    #[test]
    fn a_shorter_viewer_shows_the_bottom_of_a_taller_session() {
        let session = Size::new(40, 30);
        let mut v = TerminalView::new(session);
        for i in 0..40 {
            v.feed(format!("output {i}\r\n").as_bytes());
        }
        v.feed(b"\x1b[29;1H\x1b[2K> prompt\r\n\x1b[2K-- status --\x1b[29;3H");
        let viewer = Size::new(40, 10);
        let p = v.placement(session, viewer);
        assert_eq!(p.rows.src_start, 20);
        assert_eq!(p.viewer_to_session(0, 9), Some((0, 29)));
        assert_eq!(v.first_shown_line(&p), 20);
        let grid = v.emulator().grid();
        let row_text = |line: i32| -> String {
            (0..12).map(|c| grid[Line(line)][Column(c)].c).collect::<String>().trim_end().to_string()
        };
        assert_eq!(row_text(v.first_shown_line(&p) + 9), "-- status --");
        assert_eq!(row_text(v.first_shown_line(&p) + 8), "> prompt");

        // Scrolled up three lines: the window moves up with it.
        v.scroll_up(3);
        let p = v.placement(session, viewer);
        assert_eq!(v.first_shown_line(&p), 17);
        // And back to the bottom: the status line is the last row again.
        v.scroll_to_bottom();
        let p = v.placement(session, viewer);
        assert_eq!(v.first_shown_line(&p) + viewer.rows as i32 - 1, 29);
        // A viewer at least as tall isn't anchored at all.
        assert_eq!(v.placement(session, Size::new(40, 30)).rows.src_start, 0);
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
    fn selection_works_on_the_alternate_screen() {
        // Copying out of `less` or `vim`.
        let mut v = view();
        v.feed(b"\x1b[?1049h");
        v.feed(b"hello world");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        assert_eq!(v.selection_text().as_deref(), Some("hello"));
    }

    #[test]
    fn search_works_on_the_normal_screen() {
        let mut v = view();
        v.feed(b"needle here\r\nand needle again");
        let n = v.start_search("needle", false);
        assert_eq!(n, 2);
        // The search starts at the newest match on screen and steps back.
        assert_eq!(v.search_current().unwrap().line, 1);
        assert_eq!(v.search_position(), Some((2, 2)));
        assert_eq!(v.search_prev().unwrap().line, 0);
        assert_eq!(v.search_position(), Some((1, 2)));
        assert_eq!(v.search_next().unwrap().line, 1);
    }

    #[test]
    fn search_matches_lists_every_hit() {
        let mut v = view();
        assert!(v.search_matches().is_empty());
        v.feed(b"needle here\r\nand needle again");
        v.start_search("needle", false);
        let lines: Vec<i32> = v.search_matches().iter().map(|m| m.line).collect();
        assert_eq!(lines, vec![0, 1]);
        v.clear_search();
        assert!(v.search_matches().is_empty());
    }

    #[test]
    fn search_covers_the_alternate_screen_without_scrolling() {
        let mut v = view();
        fill_history(&mut v, 30);
        v.feed(b"\x1b[?1049h\x1b[H");
        v.feed(b"needle here");
        assert_eq!(v.start_search("needle", false), 1, "the alt screen only, no history");
        assert_eq!(v.search_current().unwrap().line, 0);
        assert_eq!(v.scroll_offset(), 0);
    }

    #[test]
    fn switching_screens_clears_selection_and_search() {
        let mut v = view();
        v.feed(b"hello");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        v.start_search("hello", false);
        v.feed(b"\x1b[?1049h"); // enter alt
        assert!(v.selection().is_none());
        assert!(v.search_current().is_none());
    }

    #[test]
    fn a_selection_stays_on_its_text_as_output_scrolls() {
        let mut v = view();
        v.feed(b"keep me\r\n");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 6));
        // Push "keep me" up into history.
        fill_history(&mut v, 8);
        let sel = v.selection().expect("the selection survives output");
        assert!(sel.range().0.line < 0, "it moved into history with its text");
        assert_eq!(v.selection_text().as_deref(), Some("keep me"));
    }

    #[test]
    fn a_selection_is_dropped_when_its_text_changes() {
        let mut v = view();
        v.feed(b"hello world");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        // Output elsewhere leaves it alone.
        v.feed(b"\x1b[3;1Helsewhere");
        assert_eq!(v.selection_text().as_deref(), Some("hello"));
        // Overwriting the selected cells in place drops it.
        v.feed(b"\x1b[1;1HHELLO");
        assert!(v.selection().is_none());
    }

    #[test]
    fn a_selection_is_dropped_when_its_text_is_erased() {
        let mut v = view();
        v.feed(b"hello world");
        v.begin_selection(Point::new(0, 0));
        v.update_selection(Point::new(0, 4));
        v.feed(b"\x1b[2J");
        assert!(v.selection().is_none());
    }

    #[test]
    fn a_backward_drag_includes_both_end_cells() {
        let mut v = view();
        v.feed(b"hello world");
        v.begin_selection(Point::new(0, 10));
        v.update_selection(Point::new(0, 6));
        assert_eq!(v.selection_text().as_deref(), Some("world"));
        // Crossing back over the anchor flips direction cleanly.
        v.update_selection(Point::new(0, 10));
        assert!(v.selection().is_none(), "back on the anchor: nothing dragged");
    }

    #[test]
    fn word_boundaries_for_double_click() {
        let mut v = TerminalView::new(Size::new(60, 3));
        v.feed(b"cd ~/src/app; vim src/main.rs:42 (foo_bar-baz.txt) a|b");
        let word_at = |v: &mut TerminalView, col| {
            v.begin_selection_mode(Point::new(0, col), SelectionMode::Word);
            v.selection_text().unwrap()
        };
        assert_eq!(word_at(&mut v, 5), "~/src/app", "`;` ends a word");
        assert_eq!(word_at(&mut v, 20), "src/main.rs", "`:` ends a word");
        assert_eq!(word_at(&mut v, 30), "42");
        assert_eq!(word_at(&mut v, 37), "foo_bar-baz.txt", "`_ - .` are word characters");
        assert_eq!(word_at(&mut v, 51), "a", "`|` ends a word");
        assert!(crate::selection::is_word_separator(' '));
        assert!(!crate::selection::is_word_separator('/'));
    }

    #[test]
    fn word_selection_follows_a_wrapped_line() {
        let mut v = view(); // 20 columns
        v.feed(b"xx abcdefghijklmnopqrstuvwxyz yy");
        v.begin_selection_mode(Point::new(1, 2), SelectionMode::Word);
        assert_eq!(v.selection_text().as_deref(), Some("abcdefghijklmnopqrstuvwxyz"));
    }

    #[test]
    fn line_selection_takes_the_whole_wrapped_line() {
        let mut v = view();
        v.feed(b"short\r\nthis line is longer than twenty");
        v.begin_selection_mode(Point::new(1, 0), SelectionMode::Line);
        assert_eq!(v.selection_text().as_deref(), Some("this line is longer than twenty"));
    }

    #[test]
    fn block_selection_takes_a_rectangle() {
        let mut v = view();
        v.feed(b"abcdef\r\nghijkl\r\nmnopqr");
        v.begin_selection_mode(Point::new(2, 4), SelectionMode::Block);
        v.update_selection(Point::new(0, 1));
        let sel = v.selection().unwrap();
        assert_eq!(sel.range(), (Point::new(0, 1), Point::new(2, 4)));
        assert_eq!(sel.row_span(1, 20), Some((1, 4)));
        assert_eq!(v.selection_text().as_deref(), Some("bcde\nhijk\nnopq"));
    }

    #[test]
    fn search_reveals_a_match_only_when_it_is_off_screen() {
        let mut v = view();
        v.feed(b"needle old\r\n");
        fill_history(&mut v, 30);
        v.feed(b"needle new");
        v.start_search("needle", false);
        assert_eq!(v.scroll_offset(), 0, "the newest is on screen: no scroll");
        let old = v.search_prev().unwrap();
        assert_eq!(v.scroll_offset(), (-old.line) as usize, "the older one is scrolled to the top");
        v.search_next();
        assert_eq!(v.scroll_offset(), 0, "back down to the newest");
    }

    #[test]
    fn refresh_search_picks_up_new_output() {
        let mut v = view();
        v.feed(b"needle\r\n");
        assert_eq!(v.start_search("needle", false), 1);
        v.feed(b"another needle");
        assert_eq!(v.search_matches().len(), 1, "feed doesn't re-scan");
        assert_eq!(v.refresh_search(), 2);
        assert_eq!(v.search_position(), Some((1, 2)), "the current match is kept");
    }

    #[test]
    fn screen_text_reads_rows_and_the_cursor() {
        let mut v = view();
        v.feed("$ 漢字ok\r\nsecond".as_bytes());
        let text = v.screen_text();
        assert_eq!(text.rows.len(), 5);
        assert_eq!(text.rows[0], "$ 漢字ok");
        assert_eq!(text.rows[1], "second");
        assert_eq!(text.rows[4], "");
        assert_eq!(text.cursor, Some((1, 6)));
        v.feed(b"\x1b[1;7H"); // column 7: after the two wide characters
        assert_eq!(v.screen_text().cursor, Some((0, 4)), "a wide character is one character");
    }
}
