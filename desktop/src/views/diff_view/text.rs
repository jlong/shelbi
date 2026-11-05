//! A diff line as the body shapes it: tabs expanded and control characters
//! blanked (the shaper draws neither), a very long line cut, and the model's
//! byte ranges moved to match. No gpui, so it's tested on its own.

use std::ops::Range;

/// A tab's stop.
pub const TAB_WIDTH: usize = 4;

/// The most of a line that's shaped: a minified bundle's one line shouldn't
/// cost a frame. The highlighter stops at the same length.
pub const MAX_SHOWN: usize = shelbi_app::diff::highlight::MAX_LINE_LEN;

/// What's shaped for a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub text: String,
    /// `(source byte of a tab, what expanding it and the tabs before it added)`.
    tabs: Vec<(u32, u32)>,
}

impl Shown {
    pub fn of(line: &str) -> Shown {
        let source = &line[..floor_char(line, MAX_SHOWN.min(line.len()))];
        if !source.bytes().any(|b| b.is_ascii_control()) {
            return Shown { text: source.to_string(), tabs: Vec::new() };
        }
        let mut text = String::with_capacity(source.len() + 8);
        let mut tabs = Vec::new();
        let mut column = 0usize;
        let mut added = 0u32;
        for (at, c) in source.char_indices() {
            match c {
                '\t' => {
                    let width = TAB_WIDTH - column % TAB_WIDTH;
                    text.extend(std::iter::repeat_n(' ', width));
                    column += width;
                    added += width as u32 - 1;
                    tabs.push((at as u32, added));
                }
                c if c.is_ascii_control() => {
                    text.push(' ');
                    column += 1;
                }
                c => {
                    text.push(c);
                    column += 1;
                }
            }
        }
        Shown { text, tabs }
    }

    /// Where the source's byte `at` is in [`Self::text`], snapped back to a
    /// character.
    pub fn at(&self, at: usize) -> usize {
        let past = self.tabs.partition_point(|(tab, _)| (*tab as usize) < at);
        let added = past.checked_sub(1).map_or(0, |i| self.tabs[i].1 as usize);
        floor_char(&self.text, (at + added).min(self.text.len()))
    }

    /// A source byte range in [`Self::text`]; empty when it's past the cut.
    pub fn range(&self, range: &Range<u32>) -> Range<usize> {
        self.at(range.start as usize)..self.at(range.end as usize)
    }
}

fn floor_char(text: &str, mut at: usize) -> usize {
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The stretches of a `len`-byte text that `spans` color, in order and covering
/// all of it. Spans past the end, overlapping an earlier one, or empty are
/// dropped: a shaped line's runs have to add up exactly.
pub fn stretches<T: Copy>(len: usize, spans: impl Iterator<Item = (Range<usize>, Option<T>)>) -> Vec<(usize, Option<T>)> {
    let mut out = Vec::new();
    let mut at = 0;
    for (range, value) in spans {
        let start = range.start.min(len);
        let end = range.end.min(len);
        if start < at || start >= end {
            continue;
        }
        if start > at {
            out.push((start - at, None));
        }
        out.push((end - start, value));
        at = end;
    }
    if at < len {
        out.push((len - at, None));
    }
    out
}

/// How long ago `time` was, as the blame gutter says it.
pub fn age(time: i64, now: i64) -> String {
    let seconds = (now - time).max(0);
    match seconds {
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s if s < 86_400 * 60 => format!("{}d", s / 86_400),
        s if s < 86_400 * 730 => format!("{}mo", s / (86_400 * 30)),
        s => format!("{}y", s / (86_400 * 365)),
    }
}

/// The digits a line number column needs, at least four so it doesn't shift
/// as files scroll by.
pub fn digits(max: u32) -> usize {
    (max.max(1).ilog10() as usize + 1).max(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_passes_through() {
        let s = Shown::of("let x = 1;");
        assert_eq!(s.text, "let x = 1;");
        assert_eq!(s.at(4), 4);
    }

    #[test]
    fn tabs_expand_to_their_stops_and_ranges_follow() {
        let s = Shown::of("\tfn a\tb");
        assert_eq!(s.text, "    fn a    b");
        assert_eq!(s.at(0), 0, "the tab itself");
        assert_eq!(s.at(1), 4, "after it");
        assert_eq!(s.range(&(1..3)), 4..6);
        assert_eq!(s.at(6), 12, "after the second tab");
        // A tab partway to a stop only pads to it.
        assert_eq!(Shown::of("ab\tc").text, "ab  c");
    }

    #[test]
    fn control_characters_blank_without_moving_anything() {
        let s = Shown::of("a\u{1b}[0mb");
        assert_eq!(s.text, "a [0mb");
        assert_eq!(s.at(5), 5);
    }

    #[test]
    fn a_long_line_is_cut_on_a_character_and_ranges_clamp() {
        let line = "é".repeat(MAX_SHOWN);
        let s = Shown::of(&line);
        assert!(s.text.len() <= MAX_SHOWN);
        assert!(s.text.is_char_boundary(s.text.len()));
        let past = s.range(&((line.len() as u32 - 2)..(line.len() as u32)));
        assert!(past.is_empty());
    }

    #[test]
    fn stretches_cover_the_text_and_drop_what_cant_be_shaped() {
        let spans = [(2..4, Some('a')), (3..5, Some('b')), (5..5, Some('c')), (6..9, Some('d')), (20..30, Some('e'))];
        assert_eq!(
            stretches(8, spans.into_iter()),
            [(2, None), (2, Some('a')), (2, None), (2, Some('d'))],
        );
        assert_eq!(stretches::<char>(3, std::iter::empty()), [(3, None)]);
        assert_eq!(stretches::<char>(0, std::iter::empty()), []);
    }

    #[test]
    fn ages_read_at_a_glance() {
        assert_eq!(age(0, 90), "1m");
        assert_eq!(age(0, 7_200), "2h");
        assert_eq!(age(0, 86_400 * 3), "3d");
        assert_eq!(age(0, 86_400 * 90), "3mo");
        assert_eq!(age(0, 86_400 * 800), "2y");
        assert_eq!(age(100, 0), "0m", "a clock behind the commit");
    }

    #[test]
    fn the_number_column_holds_the_largest_line() {
        assert_eq!(digits(0), 4);
        assert_eq!(digits(999), 4);
        assert_eq!(digits(12_345), 5);
    }
}
