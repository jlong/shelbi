// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-merge/src/ast.rs), MIT in Shelbi.

//! Layer 2 — markdown AST structural merge.
//!
//! Parses each version with comrak, takes the top-level block sequence,
//! and runs a content-hash longest-common-subsequence over the blocks.
//! Anchors (blocks present in both LCSes) are merged recursively; blocks
//! that appear on one side only are interleaved as insertions.
//!
//! Modified-in-both anchor blocks defer to Layer 1 ([`crate::merge::resolve_dmp`])
//! at the block level, so paragraph-level inline edits still flow through
//! DiffMatchPatch.
//!
//! Today's contract is conservative: if a base block is missing from
//! either side's LCS, the layer reports a conflict rather than implement
//! Swift's "deletion wins" rule. Per-list-item structural merging is
//! similarly deferred — lists are treated as opaque blocks for now.
//! Both are tracked DoD work for follow-up iterations.

use std::collections::{HashMap, HashSet};

use comrak::Arena;
use crate::markdown::comrak_options;

use crate::merge::{resolve_dmp, MergeResult};

/// A single top-level markdown block as both its rendered CommonMark
/// source and the trimmed identity hash used for matching.
struct Block {
    /// CommonMark source rendered by comrak. Used as-is in merge output
    /// when the block isn't modified.
    source: String,

    /// `source` trimmed of surrounding whitespace — the identity key for
    /// LCS matching and insertion deduplication.
    hash: String,
}

/// Parse `content` and return its top-level block sequence.
///
/// Whitespace-only blocks are skipped because they would otherwise
/// contribute spurious identities to the LCS.
fn parse_blocks(content: &str) -> Vec<Block> {
    let arena = Arena::new();
    let options = comrak_options();
    let root = comrak::parse_document(&arena, content, &options);
    let mut blocks = Vec::new();
    for child in root.children() {
        let mut source = String::new();
        if comrak::format_commonmark(child, &options, &mut source).is_err() {
            continue;
        }
        let hash = source.trim().to_string();
        if hash.is_empty() {
            continue;
        }
        blocks.push(Block { source, hash });
    }
    blocks
}

/// Longest common subsequence by block hash.
///
/// Returns `(base_index, side_index)` pairs in ascending order — these
/// identify matched anchor blocks for the merge walker.
fn lcs_by_hash(base: &[Block], side: &[Block]) -> Vec<(usize, usize)> {
    let m = base.len();
    let n = side.len();
    if m == 0 || n == 0 {
        return Vec::new();
    }
    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 1..=m {
        for j in 1..=n {
            if base[i - 1].hash == side[j - 1].hash {
                dp[i][j] = dp[i - 1][j - 1] + 1;
            } else {
                dp[i][j] = dp[i - 1][j].max(dp[i][j - 1]);
            }
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (m, n);
    while i > 0 && j > 0 {
        if base[i - 1].hash == side[j - 1].hash {
            pairs.push((i - 1, j - 1));
            i -= 1;
            j -= 1;
        } else if dp[i - 1][j] >= dp[i][j - 1] {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

/// Merge a block that appears as an LCS anchor on both sides.
///
/// Layer-0 shortcuts come first, then a DMP fallback at the block level
/// so inline paragraph edits resolve through the same machinery as the
/// document-level Layer 1.
fn merge_block(base: &Block, ours: &Block, theirs: &Block) -> Option<String> {
    if ours.hash == theirs.hash {
        return Some(ours.source.clone());
    }
    if ours.hash == base.hash {
        return Some(theirs.source.clone());
    }
    if theirs.hash == base.hash {
        return Some(ours.source.clone());
    }
    match resolve_dmp(&base.source, &ours.source, &theirs.source) {
        MergeResult::Resolved(s) => Some(s),
        MergeResult::Conflict => None,
    }
}

/// Run the Layer 2 (AST) merge.
///
/// Returns [`MergeResult::Conflict`] whenever the conservative contract
/// is not satisfied (a base block missing from either side, or a modified
/// anchor block that even DMP can't reconcile).
pub fn resolve_ast(base: &str, ours: &str, theirs: &str) -> MergeResult {
    let base_blocks = parse_blocks(base);
    let ours_blocks = parse_blocks(ours);
    let theirs_blocks = parse_blocks(theirs);

    let ours_lcs = lcs_by_hash(&base_blocks, &ours_blocks);
    let theirs_lcs = lcs_by_hash(&base_blocks, &theirs_blocks);
    let base_to_ours: HashMap<usize, usize> = ours_lcs.into_iter().collect();
    let base_to_theirs: HashMap<usize, usize> = theirs_lcs.into_iter().collect();

    for base_idx in 0..base_blocks.len() {
        if !base_to_ours.contains_key(&base_idx) || !base_to_theirs.contains_key(&base_idx) {
            return MergeResult::Conflict;
        }
    }

    let mut result: Vec<String> = Vec::new();
    let mut prev_ours: usize = 0;
    let mut prev_theirs: usize = 0;

    for base_idx in 0..base_blocks.len() {
        let o_idx = base_to_ours[&base_idx];
        let t_idx = base_to_theirs[&base_idx];

        let mut window_hashes: HashSet<String> = HashSet::new();
        for block in &ours_blocks[prev_ours..o_idx] {
            result.push(block.source.clone());
            window_hashes.insert(block.hash.clone());
        }
        for block in &theirs_blocks[prev_theirs..t_idx] {
            if !window_hashes.contains(&block.hash) {
                result.push(block.source.clone());
            }
        }

        let Some(merged) = merge_block(
            &base_blocks[base_idx],
            &ours_blocks[o_idx],
            &theirs_blocks[t_idx],
        ) else {
            return MergeResult::Conflict;
        };
        result.push(merged);

        prev_ours = o_idx + 1;
        prev_theirs = t_idx + 1;
    }

    let mut tail_hashes: HashSet<String> = HashSet::new();
    for block in &ours_blocks[prev_ours..] {
        result.push(block.source.clone());
        tail_hashes.insert(block.hash.clone());
    }
    for block in &theirs_blocks[prev_theirs..] {
        if !tail_hashes.contains(&block.hash) {
            result.push(block.source.clone());
        }
    }

    MergeResult::Resolved(serialize_blocks(&result))
}

/// Concatenate merged block sources back into a single document.
///
/// comrak's renderer terminates each block source with a newline but not
/// the blank-line separator markdown convention expects between blocks.
/// We insert one when consecutive blocks would otherwise touch.
fn serialize_blocks(blocks: &[String]) -> String {
    let mut out = String::new();
    for (i, block) in blocks.iter().enumerate() {
        if i > 0 && !out.ends_with("\n\n") {
            if !out.ends_with('\n') {
                out.push('\n');
            }
            out.push('\n');
        }
        out.push_str(block);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_blocks_skips_empty_input() {
        assert!(parse_blocks("").is_empty());
        assert!(parse_blocks("\n\n").is_empty());
    }

    #[test]
    fn parse_blocks_emits_top_level_blocks() {
        let blocks = parse_blocks("# Title\n\nParagraph.\n\n- item\n- item two\n");
        assert_eq!(blocks.len(), 3, "expected heading + paragraph + list");
    }

    #[test]
    fn lcs_returns_aligned_indices() {
        let a = vec![
            Block {
                source: String::new(),
                hash: "a".into(),
            },
            Block {
                source: String::new(),
                hash: "b".into(),
            },
            Block {
                source: String::new(),
                hash: "c".into(),
            },
        ];
        let b = vec![
            Block {
                source: String::new(),
                hash: "a".into(),
            },
            Block {
                source: String::new(),
                hash: "x".into(),
            },
            Block {
                source: String::new(),
                hash: "c".into(),
            },
        ];
        let pairs = lcs_by_hash(&a, &b);
        assert_eq!(pairs, vec![(0, 0), (2, 2)]);
    }

    #[test]
    fn deletion_on_either_side_returns_conflict() {
        let base = "First.\n\nSecond.\n\nThird.\n";
        let ours = "First.\n\nThird.\n";
        let theirs = "First.\n\nSecond modified.\n\nThird.\n";
        assert_eq!(resolve_ast(base, ours, theirs), MergeResult::Conflict);
    }

    #[test]
    fn empty_base_interleaves_both_additions() {
        let base = "";
        let ours = "Our paragraph.\n";
        let theirs = "Their paragraph.\n";
        let MergeResult::Resolved(merged) = resolve_ast(base, ours, theirs) else {
            panic!("expected Resolved");
        };
        assert!(merged.contains("Our paragraph."));
        assert!(merged.contains("Their paragraph."));
    }

    #[test]
    fn appends_after_shared_section_interleave() {
        // ours and theirs each append a new section after the same shared
        // base section; the AST walker emits ours' addition then theirs'.
        let base = "## Shared\n\nShared body.\n";
        let ours = "## Shared\n\nShared body.\n\n## Ours\n\nOur addition.\n";
        let theirs = "## Shared\n\nShared body.\n\n## Theirs\n\nTheir addition.\n";
        let MergeResult::Resolved(merged) = resolve_ast(base, ours, theirs) else {
            panic!("expected Resolved");
        };
        let ours_at = merged.find("## Ours").expect("missing ours section");
        let theirs_at = merged.find("## Theirs").expect("missing theirs section");
        assert!(
            ours_at < theirs_at,
            "ours section should precede theirs in interleave order"
        );
    }
}
