// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-merge/src/lib.rs), MIT in Shelbi.

//! Merge conflict resolution pipeline.
//!
//! Given a three-way merge (`base`, `ours`, `theirs`), [`resolve`] runs the
//! pipeline described in the `cstore-merge` sub-plan:
//!
//! 1. **Layer 0 — identity short cuts.** Trivially-resolvable cases handled
//!    in this crate without invoking the heavier merge machinery.
//! 2. **Layer 1 — DiffMatchPatch.** Word-level patch application via
//!    `diff-match-patch-rs` with a reverse-direction sanity check.
//! 3. **Layer 2 — Markdown AST merge.** Comrak parse + block-level LCS
//!    structural merge. Conservative today: handles top-level block
//!    insertions / modifications and routes modified-in-both blocks
//!    through Layer 1. Per-list-item dive-in and "deletion wins"
//!    semantics land in follow-up iterations.
//!
//! The pipeline returns [`MergeResult::Resolved`] with merged content, or
//! [`MergeResult::Conflict`] when no layer can reconcile the inputs.

#![deny(missing_docs)]

mod ast;

use diff_match_patch_rs::{DiffMatchPatch, Efficient, PatchInput};

/// The outcome of an attempted three-way merge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeResult {
    /// The merge was resolved automatically; carries the merged content.
    Resolved(String),

    /// The inputs conflict and no layer of the pipeline could reconcile them.
    /// Callers fall back to manual resolution (conflict markers, UI prompt).
    Conflict,
}

/// Run the full merge pipeline (Layer 0 → Layer 1 → Layer 2).
///
/// Layer 0 ordering matches the Swift `MergeResolver`:
///
/// 1. `ours == base` → return `theirs`
/// 2. `theirs == base` → return `ours`
/// 3. `ours == theirs` → return `ours`
///
/// Inputs that survive Layer 0 fall through to Layer 1 (DiffMatchPatch).
/// Layer 2 (markdown AST) is reserved for a future iteration; until it
/// lands, anything Layer 1 cannot resolve is reported as a conflict.
pub fn resolve(base: &str, ours: &str, theirs: &str) -> MergeResult {
    if ours == base {
        return MergeResult::Resolved(theirs.to_string());
    }
    if theirs == base {
        return MergeResult::Resolved(ours.to_string());
    }
    if ours == theirs {
        return MergeResult::Resolved(ours.to_string());
    }

    // diff-match-patch-rs can panic on certain inputs in debug builds
    // (overflow on edge cases). A panic during merge isn't actionable
    // information for the caller; treat it as `Conflict` so manual
    // resolution kicks in.
    let dmp_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resolve_dmp(base, ours, theirs)
    }))
    .unwrap_or(MergeResult::Conflict);

    if let MergeResult::Resolved(merged) = dmp_result {
        return MergeResult::Resolved(merged);
    }

    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        resolve_ast(base, ours, theirs)
    }))
    .unwrap_or(MergeResult::Conflict)
}

/// Run only the DiffMatchPatch layer.
///
/// Algorithm (matching `MergeResolver.swift`):
///
/// 1. Build `base → theirs` patches and apply them to `ours` (`merged_a`).
///    Any failed patch → conflict.
/// 2. Build `base → ours` patches and apply them to `theirs` (`merged_b`).
///    Any failed patch → conflict.
/// 3. If `merged_a != merged_b` → conflict.
/// 4. Otherwise return `Resolved(merged_a)`.
///
/// The reverse-direction check (step 2) catches cases where patches happen
/// to apply but produce semantic nonsense.
pub fn resolve_dmp(base: &str, ours: &str, theirs: &str) -> MergeResult {
    let dmp = DiffMatchPatch::new();

    let Ok(their_patches) = dmp.patch_make::<Efficient>(PatchInput::new_text_text(base, theirs))
    else {
        return MergeResult::Conflict;
    };
    let Ok((merged_a, flags_a)) = dmp.patch_apply::<Efficient>(&their_patches, ours) else {
        return MergeResult::Conflict;
    };
    if !flags_a.iter().all(|f| *f) {
        return MergeResult::Conflict;
    }

    let Ok(our_patches) = dmp.patch_make::<Efficient>(PatchInput::new_text_text(base, ours)) else {
        return MergeResult::Conflict;
    };
    let Ok((merged_b, flags_b)) = dmp.patch_apply::<Efficient>(&our_patches, theirs) else {
        return MergeResult::Conflict;
    };
    if !flags_b.iter().all(|f| *f) {
        return MergeResult::Conflict;
    }

    if merged_a != merged_b {
        return MergeResult::Conflict;
    }

    MergeResult::Resolved(merged_a)
}

/// Run only the markdown AST layer.
///
/// Entry point used for testing the AST layer in isolation and for
/// callers that want to choose pipeline layers explicitly. Behavior is
/// described in [`resolve`]'s Layer 2 section.
pub fn resolve_ast(base: &str, ours: &str, theirs: &str) -> MergeResult {
    ast::resolve_ast(base, ours, theirs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use static_assertions::assert_impl_all;

    assert_impl_all!(MergeResult: Send, Sync, Clone);

    // ─── Layer 0 ────────────────────────────────────────────────────

    #[test]
    fn layer_0_ours_equals_base_returns_theirs() {
        let base = "line one\nline two\n";
        let ours = base;
        let theirs = "line one\nline two changed\n";
        assert_eq!(
            resolve(base, ours, theirs),
            MergeResult::Resolved(theirs.to_string())
        );
    }

    #[test]
    fn layer_0_theirs_equals_base_returns_ours() {
        let base = "line one\nline two\n";
        let ours = "line one changed\nline two\n";
        let theirs = base;
        assert_eq!(
            resolve(base, ours, theirs),
            MergeResult::Resolved(ours.to_string())
        );
    }

    #[test]
    fn layer_0_ours_equals_theirs_returns_ours() {
        let base = "line one\nline two\n";
        let ours = "line one\nline two\nline three\n";
        let theirs = ours;
        assert_eq!(
            resolve(base, ours, theirs),
            MergeResult::Resolved(ours.to_string())
        );
    }

    #[test]
    fn layer_0_ordering_ours_eq_base_beats_ours_eq_theirs() {
        // When ours == base AND ours == theirs (all three equal), the first
        // rule fires and we return `theirs`. Same string, but the rule order
        // matters for the Swift parity contract.
        let same = "same\n";
        assert_eq!(
            resolve(same, same, same),
            MergeResult::Resolved(same.to_string())
        );
    }

    #[test]
    fn layer_0_empty_inputs() {
        assert_eq!(resolve("", "", ""), MergeResult::Resolved(String::new()));
        assert_eq!(
            resolve("", "added\n", ""),
            MergeResult::Resolved("added\n".to_string())
        );
    }

    // ─── Layer 1 (DMP) — ported from Swift MergeResolverTests ───────

    #[test]
    fn layer_1_non_overlapping_paragraphs_auto_resolve() {
        let base = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph.";
        let ours = "First paragraph modified.\n\nSecond paragraph.\n\nThird paragraph.";
        let theirs = "First paragraph.\n\nSecond paragraph.\n\nThird paragraph modified.";

        let MergeResult::Resolved(merged) = resolve(base, ours, theirs) else {
            panic!("expected resolved");
        };
        assert!(merged.contains("First paragraph modified"));
        assert!(merged.contains("Third paragraph modified"));
    }

    #[test]
    fn layer_1_adjacent_line_edits_auto_resolve() {
        let base = "Line 1\nLine 2\nLine 3";
        let ours = "Line 1 modified\nLine 2\nLine 3";
        let theirs = "Line 1\nLine 2 modified\nLine 3";

        let MergeResult::Resolved(merged) = resolve(base, ours, theirs) else {
            panic!("expected resolved");
        };
        assert!(merged.contains("Line 1 modified"));
        assert!(merged.contains("Line 2 modified"));
    }

    #[test]
    fn layer_1_different_words_on_same_line_auto_resolve() {
        let base = "The quick brown fox jumps over the lazy dog";
        let ours = "The QUICK brown fox jumps over the lazy dog";
        let theirs = "The quick brown fox jumps over the LAZY dog";

        let MergeResult::Resolved(merged) = resolve(base, ours, theirs) else {
            panic!("expected resolved");
        };
        assert!(merged.contains("QUICK"));
        assert!(merged.contains("LAZY"));
    }

    /// One side making a superset of the other's edit is not a conflict: the
    /// side that changed less has its change present in the result.
    ///
    /// Worth pinning because it *looks* like a discarded side at a glance —
    /// "theirs" appears nowhere in the output — when in fact the only edit
    /// theirs made is there.
    #[test]
    fn layer_1_a_superset_edit_keeps_both_sides_changes() {
        let base = "REMOTE v2.";
        let ours = "LOCAL v3."; // renamed *and* bumped
        let theirs = "REMOTE v3."; // bumped only
        assert_eq!(
            resolve(base, ours, theirs),
            MergeResult::Resolved("LOCAL v3.".to_string())
        );
    }

    #[test]
    fn layer_1_same_word_edited_differently_conflicts() {
        let base = "Hello world";
        let ours = "Hello WORLD";
        let theirs = "Hello World";
        assert_eq!(resolve(base, ours, theirs), MergeResult::Conflict);
    }

    #[test]
    fn layer_1_overlapping_paragraph_rewrites_conflict() {
        let base = "Introduction paragraph.\n\nMain content here.\n\nConclusion paragraph.";
        let ours = "Introduction paragraph completely rewritten.\n\nMain content here.\n\nConclusion paragraph.";
        let theirs = "A different introduction.\n\nMain content here.\n\nConclusion paragraph.";
        assert_eq!(resolve(base, ours, theirs), MergeResult::Conflict);
    }

    #[test]
    fn layer_1_markdown_header_and_body_change_auto_resolve() {
        let base = "# Document Title\n\nThis is the introduction.\n\n## Section One\n\nContent for section one.";
        let ours = "# Updated Document Title\n\nThis is the introduction.\n\n## Section One\n\nContent for section one.";
        let theirs = "# Document Title\n\nThis is the introduction.\n\n## Section One\n\nUpdated content for section one.";

        let MergeResult::Resolved(merged) = resolve(base, ours, theirs) else {
            panic!("expected resolved");
        };
        assert!(merged.contains("# Updated Document Title"));
        assert!(merged.contains("Updated content for section one"));
    }

    #[test]
    fn layer_1_empty_base_both_added_different_conflicts_at_dmp() {
        // Direct DMP layer call: empty base + two different additions
        // cannot be reconciled by patch application, so Layer 1 alone
        // reports a conflict. The full pipeline now falls through to
        // Layer 2 (AST) for this case.
        let base = "";
        let ours = "Our new content";
        let theirs = "Their new content";
        assert_eq!(resolve_dmp(base, ours, theirs), MergeResult::Conflict);
    }

    #[test]
    fn layer_2_empty_base_both_added_different_interleaves() {
        // Mirrors StructuralMergeResolver.swift: with no base blocks,
        // the AST walker emits both additions in order (deduped).
        let base = "";
        let ours = "Our new content\n";
        let theirs = "Their new content\n";
        let MergeResult::Resolved(merged) = resolve(base, ours, theirs) else {
            panic!("expected Resolved via AST layer");
        };
        assert!(merged.contains("Our new content"));
        assert!(merged.contains("Their new content"));
    }

    #[test]
    fn layer_1_whitespace_only_changes_conflict() {
        // Both sides modified the same whitespace differently.
        let base = "Hello  world";
        let ours = "Hello world";
        let theirs = "Hello   world";
        assert_eq!(resolve(base, ours, theirs), MergeResult::Conflict);
    }

    // ─── Direct entry-point checks ──────────────────────────────────

    #[test]
    fn resolve_dmp_resolves_adjacent_lines_directly() {
        // Bypassing Layer 0, call resolve_dmp directly.
        let base = "Line 1\nLine 2\nLine 3";
        let ours = "Line 1 modified\nLine 2\nLine 3";
        let theirs = "Line 1\nLine 2 modified\nLine 3";
        let MergeResult::Resolved(merged) = resolve_dmp(base, ours, theirs) else {
            panic!("expected resolved");
        };
        assert!(merged.contains("Line 1 modified"));
        assert!(merged.contains("Line 2 modified"));
    }

    #[test]
    fn resolve_ast_single_paragraph_modified_both_sides_conflicts() {
        // Single-block document with no LCS anchors — conservative AST
        // contract reports a conflict.
        assert_eq!(resolve_ast("a", "b", "c"), MergeResult::Conflict);
    }
}
