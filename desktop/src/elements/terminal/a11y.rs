//! The screen as AccessKit text runs.
//!
//! The pane is a `Terminal` node; each visible row becomes one or more
//! `TextRun` children. AccessKit's conventions, which the macOS adapter turns
//! into the `AXTextArea` VoiceOver reads:
//!
//! - A run that ends a line carries its line break (`\n`) at the end of its
//!   value, counted as one character, so the rows read as lines rather than
//!   one run-together string.
//! - `character_lengths` gives each character's UTF-8 length and
//!   `word_starts` each word's first character, both as `u8`, so a run holds
//!   at most [`MAX_RUN_CHARS`] characters; a longer row is split.
//! - A word starts at a non-blank character after a blank (trailing blanks
//!   belong to the word before; a row's leading blanks are their own word).
//!
//! Plain data, unit-tested without a window.

/// The most characters in one run (`word_starts` indexes with a `u8`).
pub const MAX_RUN_CHARS: usize = 255;

/// One text run's content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Run {
    /// The text, with the line break when the run ends a line.
    pub value: String,
    /// Each character's UTF-8 length.
    pub character_lengths: Vec<u8>,
    /// The index of each word's first character.
    pub word_starts: Vec<u8>,
}

impl Run {
    /// Characters in the run (the line break counts as one).
    pub fn len(&self) -> usize {
        self.character_lengths.len()
    }
}

/// The runs for one row's text. `line_break` adds a `\n` to the last run
/// (every row but the screen's last ends a line).
pub fn row_runs(row: &str, line_break: bool) -> Vec<Run> {
    let mut chars: Vec<char> = row.chars().collect();
    if line_break {
        chars.push('\n');
    }
    if chars.is_empty() {
        return vec![Run {
            value: String::new(),
            character_lengths: Vec::new(),
            word_starts: Vec::new(),
        }];
    }
    chars
        .chunks(MAX_RUN_CHARS)
        .enumerate()
        .map(|(n, chunk)| {
            let before = (n > 0).then(|| chars[n * MAX_RUN_CHARS - 1]);
            let word_starts = chunk
                .iter()
                .enumerate()
                .filter(|&(i, c)| {
                    let prev = if i == 0 { before } else { Some(chunk[i - 1]) };
                    !c.is_whitespace() && prev.is_some_and(char::is_whitespace)
                })
                .map(|(i, _)| i as u8)
                .collect();
            Run {
                value: chunk.iter().collect(),
                character_lengths: chunk.iter().map(|c| c.len_utf8() as u8).collect(),
                word_starts,
            }
        })
        .collect()
}

/// Where a caret at character `index` of a row falls among its runs:
/// `(run, character within the run)`. Past the row's text (the cursor after
/// trailing blanks, which the text trims) it rests on the line break, or at
/// the end of the last run when there is none.
pub fn caret(runs: &[Run], index: usize) -> (usize, usize) {
    let mut start = 0;
    for (n, run) in runs.iter().enumerate() {
        let text_len = run.len() - usize::from(run.value.ends_with('\n'));
        if index < start + text_len || n + 1 == runs.len() {
            return (n, (index - start).min(text_len));
        }
        start += run.len();
    }
    (0, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_is_one_run_ending_in_its_line_break() {
        let runs = row_runs("$ ls -la", true);
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].value, "$ ls -la\n");
        assert_eq!(runs[0].len(), 9);
        assert_eq!(runs[0].word_starts, vec![2, 5]);
        // The screen's last row has no line break.
        assert_eq!(row_runs("$ ", false)[0].value, "$ ");
    }

    #[test]
    fn character_lengths_are_utf8() {
        let runs = row_runs("é漢👍", false);
        assert_eq!(runs[0].character_lengths, vec![2, 3, 4]);
        assert_eq!(runs[0].value.len(), 9);
    }

    #[test]
    fn leading_blanks_are_their_own_word() {
        assert_eq!(row_runs("   indented text", true)[0].word_starts, vec![3, 12]);
    }

    #[test]
    fn an_empty_row_is_a_bare_line_break() {
        let runs = row_runs("", true);
        assert_eq!(runs[0].value, "\n");
        assert_eq!(runs[0].character_lengths, vec![1]);
        assert_eq!(row_runs("", false)[0].len(), 0);
    }

    #[test]
    fn a_long_row_splits_and_keeps_word_starts_in_range() {
        let row = "ab ".repeat(100); // 300 characters
        let runs = row_runs(&row, true);
        assert_eq!(runs.len(), 2);
        assert_eq!(runs[0].len(), MAX_RUN_CHARS);
        assert_eq!(runs[1].len(), 301 - MAX_RUN_CHARS);
        assert!(runs[1].value.ends_with('\n') && !runs[0].value.ends_with('\n'));
        // 255 = 85 * 3, so the second run starts on a word.
        assert_eq!(runs[1].word_starts.first(), Some(&0));
        assert!(runs.iter().all(|r| r.word_starts.iter().all(|&w| (w as usize) < r.len())));
    }

    #[test]
    fn the_caret_lands_in_its_run_or_on_the_line_break() {
        let runs = row_runs("$", true); // "$\n"
        assert_eq!(caret(&runs, 0), (0, 0));
        assert_eq!(caret(&runs, 1), (0, 1), "after the text: on the line break");
        assert_eq!(caret(&runs, 2), (0, 1), "after trimmed blanks: still the line break");
        let long = row_runs(&"x".repeat(300), true);
        assert_eq!(caret(&long, 10), (0, 10));
        assert_eq!(caret(&long, 260), (1, 5));
        assert_eq!(caret(&long, 400), (1, 45), "clamped to the line break");
    }
}
