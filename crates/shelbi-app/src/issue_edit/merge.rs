//! Text merging for [`super::IssueEdits`]: a line-level three-way merge that
//! combines two edits of one text only when they touch different lines, and
//! the single splice that turns one text into another (so an editor can
//! apply a rebase in place, keeping its caret).

/// Above this many base × other line pairs the diff isn't attempted and the
/// edits count as overlapping: the caller keeps both versions instead.
const DIFF_LIMIT: usize = 4_000_000;

/// Merge `mine` and `theirs`, two edits of `base`, when every change one
/// makes is apart from every change the other makes: not on the same lines,
/// and not on lines next to each other (an insertion at the same place
/// counts as touching). `None` when they touch, so the caller keeps both
/// texts rather than guess. Identical changes on both sides merge as one.
pub fn merge3(base: &str, mine: &str, theirs: &str) -> Option<String> {
    if mine == theirs || theirs == base {
        return Some(mine.to_string());
    }
    if mine == base {
        return Some(theirs.to_string());
    }
    let base_lines: Vec<&str> = base.split_inclusive('\n').collect();
    let mine_lines: Vec<&str> = mine.split_inclusive('\n').collect();
    let theirs_lines: Vec<&str> = theirs.split_inclusive('\n').collect();
    let ours = hunks(&base_lines, &mine_lines)?;
    let other = hunks(&base_lines, &theirs_lines)?;

    let mut out = String::with_capacity(mine.len().max(theirs.len()));
    let mut at = 0;
    let (mut i, mut j) = (0, 0);
    loop {
        let next = match (ours.get(i), other.get(j)) {
            (None, None) => break,
            (Some(a), None) => {
                i += 1;
                a
            }
            (None, Some(b)) => {
                j += 1;
                b
            }
            (Some(a), Some(b)) if a == b => {
                i += 1;
                j += 1;
                a
            }
            (Some(a), Some(b)) => {
                if a.start <= b.end && b.start <= a.end {
                    return None;
                }
                if a.start < b.start {
                    i += 1;
                    a
                } else {
                    j += 1;
                    b
                }
            }
        };
        for line in &base_lines[at..next.start] {
            out.push_str(line);
        }
        for line in &next.lines {
            out.push_str(line);
        }
        at = next.end;
    }
    for line in &base_lines[at..] {
        out.push_str(line);
    }
    Some(out)
}

/// One change from a base text: base lines `start..end` replaced by `lines`
/// (an insertion when the range is empty).
#[derive(Debug, PartialEq, Eq)]
struct Hunk<'a> {
    start: usize,
    end: usize,
    lines: Vec<&'a str>,
}

/// The changes that turn `base` into `other`, in base order, from a longest
/// common subsequence of their lines. `None` when the texts are too large to
/// compare.
fn hunks<'a>(base: &[&str], other: &[&'a str]) -> Option<Vec<Hunk<'a>>> {
    let prefix = base.iter().zip(other).take_while(|(a, b)| a == b).count();
    let suffix = base[prefix..]
        .iter()
        .rev()
        .zip(other[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let b = &base[prefix..base.len() - suffix];
    let o = &other[prefix..other.len() - suffix];
    let (n, m) = (b.len(), o.len());
    if n.checked_mul(m)? > DIFF_LIMIT {
        return None;
    }
    // lcs[i * w + j]: the common subsequence length of b[i..] and o[j..].
    let w = m + 1;
    let mut lcs = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i * w + j] = if b[i] == o[j] {
                lcs[(i + 1) * w + j + 1] + 1
            } else {
                lcs[(i + 1) * w + j].max(lcs[i * w + j + 1])
            };
        }
    }
    let mut out = Vec::new();
    let mut current: Option<Hunk<'a>> = None;
    let (mut i, mut j) = (0, 0);
    while i < n || j < m {
        if i < n && j < m && b[i] == o[j] {
            out.extend(current.take());
            i += 1;
            j += 1;
        } else if j < m && (i == n || lcs[i * w + j + 1] >= lcs[(i + 1) * w + j]) {
            let at = prefix + i;
            current
                .get_or_insert(Hunk { start: at, end: at, lines: Vec::new() })
                .lines
                .push(o[j]);
            j += 1;
        } else {
            let at = prefix + i;
            current
                .get_or_insert(Hunk { start: at, end: at, lines: Vec::new() })
                .end = at + 1;
            i += 1;
        }
    }
    out.extend(current);
    Some(out)
}

/// The one replacement that turns `old` into `new`: the byte range of `old`
/// that differs (after the longest common prefix and suffix, on character
/// boundaries) and what replaces it. An editor applies it as one edit, so
/// its caret and undo history outside the range stay put.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Splice {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

impl Splice {
    pub fn between(old: &str, new: &str) -> Splice {
        let mut start = old
            .bytes()
            .zip(new.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(start) || !new.is_char_boundary(start) {
            start -= 1;
        }
        let max_suffix = old.len().min(new.len()) - start;
        let mut suffix = old
            .bytes()
            .rev()
            .zip(new.bytes().rev())
            .take(max_suffix)
            .take_while(|(a, b)| a == b)
            .count();
        while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix)
        {
            suffix -= 1;
        }
        Splice {
            start,
            end: old.len() - suffix,
            text: new[start..new.len() - suffix].to_string(),
        }
    }

    /// Whether it changes nothing.
    pub fn is_empty(&self) -> bool {
        self.start == self.end && self.text.is_empty()
    }

    /// Where byte `offset` of the old text lands in the new one: unchanged
    /// before the splice, shifted after it, at the end of the replacement
    /// inside it.
    pub fn map(&self, offset: usize) -> usize {
        if offset <= self.start {
            offset
        } else if offset >= self.end {
            offset - self.end + self.start + self.text.len()
        } else {
            self.start + self.text.len()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disjoint_line_edits_merge() {
        let base = "# Title\n\nfirst\nsecond\nthird\n";
        let mine = "# Title\n\nFIRST\nsecond\nthird\n";
        let theirs = "# Title\n\nfirst\nsecond\nthird\n\n## Notes\nagent note\n";
        assert_eq!(
            merge3(base, mine, theirs).as_deref(),
            Some("# Title\n\nFIRST\nsecond\nthird\n\n## Notes\nagent note\n")
        );
        // Either side's edit wins alone; identical edits merge once.
        assert_eq!(merge3(base, base, theirs).as_deref(), Some(theirs));
        assert_eq!(merge3(base, mine, base).as_deref(), Some(mine));
        assert_eq!(merge3(base, mine, mine).as_deref(), Some(mine));
    }

    #[test]
    fn the_same_or_neighbouring_lines_do_not_merge() {
        let base = "a\nb\nc\nd\n";
        assert_eq!(merge3(base, "a\nB\nc\nd\n", "a\nb2\nc\nd\n"), None, "same line");
        assert_eq!(merge3(base, "a\nB\nc\nd\n", "a\nb\nC\nd\n"), None, "neighbouring lines");
        assert_eq!(merge3(base, "a\nb\nc\nd\nmine\n", "a\nb\nc\nd\ntheirs\n"), None, "same insertion point");
        assert_eq!(
            merge3(base, "A\nb\nc\nd\n", "a\nb\nc\nD\n").as_deref(),
            Some("A\nb\nc\nD\n"),
            "lines apart merge"
        );
    }

    #[test]
    fn a_shared_identical_change_plus_a_separate_one_merges() {
        let base = "a\nb\nc\nd\ne\n";
        let mine = "A\nb\nc\nd\nE\n";
        let theirs = "A\nb\nc\nd\ne\n";
        assert_eq!(merge3(base, mine, theirs).as_deref(), Some("A\nb\nc\nd\nE\n"));
    }

    #[test]
    fn splice_is_the_differing_middle_on_char_boundaries() {
        let s = Splice::between("hello world", "hello brave world");
        assert_eq!(s, Splice { start: 6, end: 6, text: "brave ".into() });
        assert_eq!(s.map(0), 0);
        assert_eq!(s.map(6), 6, "a caret at the splice stays before the insertion");
        assert_eq!(s.map(11), 17);
        let s = Splice::between("añb", "aéb");
        assert!(s.text == "é" && s.start == 1 && s.end == 3, "{s:?}");
        assert!(Splice::between("same", "same").is_empty());
        let s = Splice::between("aaa", "aa");
        assert_eq!((s.start, s.end, s.text.as_str()), (2, 3, ""));
    }
}
