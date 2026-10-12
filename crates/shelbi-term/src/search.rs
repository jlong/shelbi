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
}
