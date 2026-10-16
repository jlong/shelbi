//! Search within scrollback and the visible screen.
//!
//! Search, like scrollback, is for sessions on the **normal** screen;
//! [`crate::view::TerminalView`] enforces that. It scans the retained history
//! and the visible screen top-to-bottom for a literal substring and lets the
//! caller step through the matches, driving the scroll offset to reveal the
//! current one.
//!
//! Matches are reported per grid row and do not span a wrapped-row boundary;
//! this is a deliberate simplification over the emulator's logical-line model
//! and is adequate for locating output on screen.
//!
//! ## Stepping
//!
//! Matches are kept in buffer order (oldest history line first).
//! [`Search::next_match`] steps toward the live bottom and
//! [`Search::prev_match`] back into history, both wrapping. A search usually
//! starts from the newest match a viewer can see
//! ([`Search::select_last_at_or_before`]) and steps back from there; a viewer
//! binds Enter to the older match and Shift+Enter to the newer one. After the
//! buffer changes, [`Search::rerun`] re-scans and keeps the current match.

use alacritty_terminal::grid::{Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Cell;

/// A single match: a half-open column range `[start_col, end_col)` on one grid
/// line (grid-line coordinates, as in [`crate::selection`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    /// Grid line the match is on.
    pub line: i32,
    /// First column of the match.
    pub start_col: u16,
    /// One past the last column of the match.
    pub end_col: u16,
}

/// A search over a session's buffer. Rebuild [`Search::run`] after output
/// changes the grid; step through results with [`Search::next_match`] /
/// [`Search::prev_match`].
#[derive(Debug, Clone, Default)]
pub struct Search {
    query: Vec<char>,
    case_insensitive: bool,
    matches: Vec<Match>,
    current: Option<usize>,
}

impl Search {
    /// A search for `query`. An empty query matches nothing.
    pub fn new(query: &str, case_insensitive: bool) -> Self {
        let query: Vec<char> = if case_insensitive {
            query.chars().flat_map(|c| c.to_lowercase()).collect()
        } else {
            query.chars().collect()
        };
        Self { query, case_insensitive, matches: Vec::new(), current: None }
    }

    /// (Re)scan `grid` for the query, from the oldest history line to the
    /// bottom of the screen. The first match becomes current.
    pub fn run(&mut self, grid: &Grid<Cell>) {
        self.matches.clear();
        self.current = None;
        if self.query.is_empty() {
            return;
        }

        let cols = grid.columns() as u16;
        let top = -(grid.history_size() as i32);
        let bottom = grid.screen_lines() as i32 - 1;

        for line in top..=bottom {
            let row = &grid[Line(line)];
            let chars: Vec<char> = (0..cols)
                .map(|c| {
                    let ch = row[Column(c as usize)].c;
                    if self.case_insensitive {
                        ch.to_lowercase().next().unwrap_or(ch)
                    } else {
                        ch
                    }
                })
                .collect();
            self.find_in_row(line, &chars);
        }

        if !self.matches.is_empty() {
            self.current = Some(0);
        }
    }

    fn find_in_row(&mut self, line: i32, chars: &[char]) {
        let qlen = self.query.len();
        if qlen == 0 || chars.len() < qlen {
            return;
        }
        let mut start = 0;
        while start + qlen <= chars.len() {
            if chars[start..start + qlen] == self.query[..] {
                self.matches.push(Match {
                    line,
                    start_col: start as u16,
                    end_col: (start + qlen) as u16,
                });
                start += qlen; // non-overlapping matches
            } else {
                start += 1;
            }
        }
    }

    /// Re-scan `grid` after its content changed, keeping the current match
    /// if a match is still at the same place, else the match at the same
    /// position counted from the newest (new output adds matches at the
    /// bottom, so that keeps the user near what they were looking at).
    pub fn rerun(&mut self, grid: &Grid<Cell>) {
        let previous = self.current();
        let from_newest = self.current.map(|i| self.matches.len() - 1 - i);
        self.run(grid);
        if self.matches.is_empty() {
            return;
        }
        self.current = match previous.and_then(|p| self.matches.iter().position(|m| *m == p)) {
            Some(i) => Some(i),
            None => match from_newest {
                Some(back) => Some(self.matches.len().saturating_sub(1 + back)),
                None => Some(0),
            },
        };
    }

    /// Make the newest match on or above grid line `line` current (the
    /// newest match overall when none is), and return it.
    pub fn select_last_at_or_before(&mut self, line: i32) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        let i = self
            .matches
            .iter()
            .rposition(|m| m.line <= line)
            .unwrap_or(self.matches.len() - 1);
        self.current = Some(i);
        self.current()
    }

    /// The current match's index in [`Search::matches`].
    pub fn current_index(&self) -> Option<usize> {
        self.current
    }

    /// All matches, in top-to-bottom order.
    pub fn matches(&self) -> &[Match] {
        &self.matches
    }

    /// Whether the search found nothing.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// The number of matches.
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// The current match, if any.
    pub fn current(&self) -> Option<Match> {
        self.current.map(|i| self.matches[i])
    }

    /// Advance to the next match (wrapping to the first), returning it.
    pub fn next_match(&mut self) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = Some(match self.current {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        });
        self.current()
    }

    /// Step back to the previous match (wrapping to the last), returning it.
    pub fn prev_match(&mut self) -> Option<Match> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = Some(match self.current {
            Some(0) | None => self.matches.len() - 1,
            Some(i) => i - 1,
        });
        self.current()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::term::test::mock_term;

    #[test]
    fn empty_query_matches_nothing() {
        let term = mock_term("anything here");
        let mut s = Search::new("", false);
        s.run(term.grid());
        assert!(s.is_empty());
        assert_eq!(s.current(), None);
    }

    #[test]
    fn finds_matches_across_lines() {
        let term = mock_term("foo bar\r\nbar foo");
        let mut s = Search::new("foo", false);
        s.run(term.grid());
        assert_eq!(s.len(), 2);
        assert_eq!(s.matches()[0], Match { line: 0, start_col: 0, end_col: 3 });
        assert_eq!(s.matches()[1], Match { line: 1, start_col: 4, end_col: 7 });
    }

    #[test]
    fn current_starts_at_first_and_cycles() {
        let term = mock_term("a-a-a");
        let mut s = Search::new("a", false);
        s.run(term.grid());
        assert_eq!(s.len(), 3);
        assert_eq!(s.current().unwrap().start_col, 0);
        assert_eq!(s.next_match().unwrap().start_col, 2);
        assert_eq!(s.next_match().unwrap().start_col, 4);
        assert_eq!(s.next_match().unwrap().start_col, 0); // wraps
        assert_eq!(s.prev_match().unwrap().start_col, 4); // wraps back
    }

    #[test]
    fn case_insensitive_matches() {
        let term = mock_term("Hello HELLO hello");
        let mut s = Search::new("hello", true);
        s.run(term.grid());
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn case_sensitive_is_exact() {
        let term = mock_term("Hello HELLO hello");
        let mut s = Search::new("hello", false);
        s.run(term.grid());
        assert_eq!(s.len(), 1);
        assert_eq!(s.matches()[0].start_col, 12);
    }

    #[test]
    fn select_last_at_or_before_picks_the_newest_visible_match() {
        let term = mock_term("x\r\nfoo\r\nx\r\nfoo\r\nfoo");
        let mut s = Search::new("foo", false);
        s.run(term.grid());
        assert_eq!(s.select_last_at_or_before(3).unwrap().line, 3);
        assert_eq!(s.current_index(), Some(1));
        assert_eq!(s.select_last_at_or_before(10).unwrap().line, 4);
        // Nothing at or above line 0: fall back to the newest overall.
        assert_eq!(s.select_last_at_or_before(0).unwrap().line, 4);
    }

    #[test]
    fn stepping_back_from_the_newest_walks_into_history_and_wraps() {
        let term = mock_term("foo 1\r\nfoo 2\r\nfoo 3");
        let mut s = Search::new("foo", false);
        s.run(term.grid());
        s.select_last_at_or_before(i32::MAX);
        let back: Vec<i32> = (0..4).map(|_| s.prev_match().unwrap().line).collect();
        assert_eq!(back, vec![1, 0, 2, 1], "older, older, wrap to newest, older");
        assert_eq!(s.next_match().unwrap().line, 2, "Shift+Enter steps newer");
    }

    #[test]
    fn rerun_keeps_the_current_match() {
        let term = mock_term("foo a\r\nfoo b\r\nfoo c");
        let mut s = Search::new("foo", false);
        s.run(term.grid());
        s.next_match();
        let before = s.current();
        s.rerun(term.grid());
        assert_eq!(s.current(), before);
    }

    #[test]
    fn rerun_falls_back_to_the_same_place_from_the_newest() {
        let mut s = Search::new("foo", false);
        s.run(mock_term("foo a\r\nfoo b\r\nfoo c").grid());
        s.select_last_at_or_before(i32::MAX);
        s.prev_match(); // the second newest (line 1)
        // The buffer changed under it: matches now at lines 1, 2, 3.
        s.rerun(mock_term("bar\r\nxfoo\r\nxfoo\r\nxfoo").grid());
        assert_eq!(s.current().unwrap().line, 2, "still the second newest");
        assert_eq!(s.current_index(), Some(1));
    }

    #[test]
    fn rerun_with_no_matches_left_clears_the_current() {
        let mut s = Search::new("foo", false);
        s.run(mock_term("foo").grid());
        s.rerun(mock_term("bar").grid());
        assert!(s.is_empty());
        assert_eq!(s.current(), None);
    }
}
