//! Word-level highlights within a changed line, and which deleted line pairs
//! with which added one. The split view lines its rows up on the same pairs.

use std::ops::Range;

use similar::{capture_diff_slices, Algorithm, DiffOp};

use super::model::{DiffLine, Hunk, LineKind};

/// A change block larger than this pairs lines in order: scoring every line
/// against every other is quadratic, and a block this large is a rewrite.
const SCORED_BLOCK: usize = 32;
/// A line longer than this is left whole (minified code, data).
const MAX_LINE: usize = 2_000;
/// Two lines sharing less than this share of their words aren't an edit of
/// one another, so neither gets highlights.
const MIN_SIMILARITY: f32 = 0.3;

/// A run of deleted lines followed by added ones, by index in `lines`.
fn change_blocks(lines: &[DiffLine]) -> Vec<(Range<usize>, Range<usize>)> {
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].kind == LineKind::Context {
            i += 1;
            continue;
        }
        let del_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Deleted {
            i += 1;
        }
        let add_start = i;
        while i < lines.len() && lines[i].kind == LineKind::Added {
            i += 1;
        }
        blocks.push((del_start..add_start, add_start..i));
    }
    blocks
}

/// The word tokens of `text`: runs of word characters, each other character
/// alone. Byte ranges.
fn tokens(text: &str) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = None;
    for (at, c) in text.char_indices() {
        if c.is_alphanumeric() || c == '_' {
            start.get_or_insert(at);
            continue;
        }
        if let Some(s) = start.take() {
            out.push(s..at);
        }
        out.push(at..at + c.len_utf8());
    }
    if let Some(s) = start {
        out.push(s..text.len());
    }
    out
}

/// How alike two lines are, 0 to 1: the share of word tokens in common.
fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    fn words(t: &str) -> Vec<&str> {
        let mut w: Vec<&str> = tokens(t)
            .into_iter()
            .map(|r| &t[r])
            .filter(|w| w.chars().any(|c| !c.is_whitespace()))
            .collect();
        w.sort_unstable();
        w
    }
    let (a, b) = (words(a), words(b));
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut shared) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(b[j]) {
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
        }
    }
    2.0 * shared as f32 / (a.len() + b.len()) as f32
}

/// Which deleted line pairs with which added one: indexes into the two runs,
/// both ascending. A small block takes the order-preserving pairing with the
/// most in common; a large one pairs in order.
pub fn pairs(deleted: &[DiffLine], added: &[DiffLine]) -> Vec<(usize, usize)> {
    let (n, m) = (deleted.len(), added.len());
    if n == 0 || m == 0 {
        return Vec::new();
    }
    if n > SCORED_BLOCK || m > SCORED_BLOCK {
        return (0..n.min(m)).collect::<Vec<_>>().into_iter().map(|i| (i, i)).collect();
    }
    let score: Vec<Vec<f32>> = deleted
        .iter()
        .map(|d| added.iter().map(|a| similarity(&d.text, &a.text)).collect())
        .collect();
    // Longest-common-subsequence shape: best[i][j] is the best total for
    // deleted[i..] and added[j..].
    let mut best = vec![vec![0f32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let take = if score[i][j] >= MIN_SIMILARITY { score[i][j] + best[i + 1][j + 1] } else { 0.0 };
            best[i][j] = take.max(best[i + 1][j]).max(best[i][j + 1]);
        }
    }
    let (mut i, mut j) = (0, 0);
    let mut out = Vec::new();
    while i < n && j < m {
        let take = score[i][j] >= MIN_SIMILARITY && (best[i][j] - (score[i][j] + best[i + 1][j + 1])).abs() < 1e-5;
        if take {
            out.push((i, j));
            i += 1;
            j += 1;
        } else if best[i + 1][j] >= best[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    out
}

/// Every deleted-to-added pairing in `hunk`, as indexes into its lines.
pub fn hunk_pairs(hunk: &Hunk) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    for (del, add) in change_blocks(&hunk.lines) {
        for (a, b) in pairs(&hunk.lines[del.clone()], &hunk.lines[add.clone()]) {
            out.push((del.start + a, add.start + b));
        }
    }
    out
}

/// The changed byte ranges of a pair of lines, merged where adjacent.
pub fn highlight(old: &str, new: &str) -> (Vec<Range<u32>>, Vec<Range<u32>>) {
    let (old_ranges, new_ranges) = (tokens(old), tokens(new));
    let old_tokens: Vec<&str> = old_ranges.iter().map(|r| &old[r.clone()]).collect();
    let new_tokens: Vec<&str> = new_ranges.iter().map(|r| &new[r.clone()]).collect();
    let mut old_out: Vec<Range<u32>> = Vec::new();
    let mut new_out: Vec<Range<u32>> = Vec::new();
    let push = |out: &mut Vec<Range<u32>>, ranges: &[Range<usize>], from: usize, to: usize| {
        if from >= to {
            return;
        }
        let range = ranges[from].start as u32..ranges[to - 1].end as u32;
        match out.last_mut() {
            Some(last) if last.end == range.start => last.end = range.end,
            _ => out.push(range),
        }
    };
    for op in capture_diff_slices(Algorithm::Myers, &old_tokens, &new_tokens) {
        match op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete { old_index, old_len, .. } => {
                push(&mut old_out, &old_ranges, old_index, old_index + old_len);
            }
            DiffOp::Insert { new_index, new_len, .. } => {
                push(&mut new_out, &new_ranges, new_index, new_index + new_len);
            }
            DiffOp::Replace { old_index, old_len, new_index, new_len } => {
                push(&mut old_out, &old_ranges, old_index, old_index + old_len);
                push(&mut new_out, &new_ranges, new_index, new_index + new_len);
            }
        }
    }
    (old_out, new_out)
}

/// Fill in `hunk`'s intraline highlights.
pub fn annotate(hunk: &mut Hunk) {
    for (del, add) in hunk_pairs(hunk) {
        let (old, new) = (&hunk.lines[del].text, &hunk.lines[add].text);
        if old.len() > MAX_LINE || new.len() > MAX_LINE {
            continue;
        }
        let (old_ranges, new_ranges) = highlight(old, new);
        hunk.lines[del].intraline = old_ranges;
        hunk.lines[add].intraline = new_ranges;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse::parse_patch;

    fn slices(text: &str, ranges: &[Range<u32>]) -> Vec<String> {
        ranges.iter().map(|r| text[r.start as usize..r.end as usize].to_string()).collect()
    }

    #[test]
    fn a_changed_word_is_highlighted_alone() {
        let (old, new) = ("let total = count + 1;", "let total = amount + 1;");
        let (a, b) = highlight(old, new);
        assert_eq!(slices(old, &a), ["count"]);
        assert_eq!(slices(new, &b), ["amount"]);
    }

    #[test]
    fn multibyte_text_stays_on_char_boundaries() {
        let (old, new) = ("naïve café", "naïve thé");
        let (a, b) = highlight(old, new);
        assert_eq!(slices(old, &a), ["café"]);
        assert_eq!(slices(new, &b), ["thé"]);
    }

    fn hunk(body: &str) -> Hunk {
        let patch = format!("diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1,99 +1,99 @@\n{body}");
        parse_patch(&patch).remove(0).hunks.remove(0)
    }

    #[test]
    fn unrelated_lines_get_no_highlights() {
        let h = hunk("-fn alpha(one: u32) -> bool {\n+let mut zed = Vec::new();\n");
        assert!(h.lines.iter().all(|l| l.intraline.is_empty()));
    }

    #[test]
    fn lines_pair_with_the_one_they_resemble_not_the_one_at_their_index() {
        let h = hunk("-let total = count + 1;\n+// a new comment here\n+let total = amount + 1;\n");
        assert_eq!(hunk_pairs(&h), [(0, 2)]);
        assert!(h.lines[1].intraline.is_empty());
        assert_eq!(slices(&h.lines[2].text, &h.lines[2].intraline), ["amount"]);
    }

    #[test]
    fn a_very_long_line_is_left_whole() {
        let long = "x ".repeat(MAX_LINE);
        let h = hunk(&format!("-{long}a\n+{long}b\n"));
        assert!(h.lines.iter().all(|l| l.intraline.is_empty()));
    }
}
