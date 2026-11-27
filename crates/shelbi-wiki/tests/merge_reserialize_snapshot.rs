// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-merge/tests/reserialize_snapshot.rs), MIT in Shelbi.

//! Snapshot test for the parse → render round trip used by Layer 2.
//!
//! The merge pipeline reads three markdown documents, parses them with
//! comrak, and re-renders the merged result via `format_commonmark`. The
//! renderer is not byte-identical to its input — it normalizes
//! whitespace, list markers, and heading style. We pin those
//! normalizations with a snapshot so a future comrak bump or option
//! change doesn't silently shift the merge pipeline's output shape.
//!
//! Fixture layout: `fixtures/reserialize/golden.md` is the input;
//! `fixtures/reserialize/golden.expected.md` is the pinned output. Run
//! with `UPDATE_SNAPSHOT=1` to regenerate the snapshot file when an
//! intentional change is made.

use std::fs;
use std::path::PathBuf;

use comrak::Arena;
use shelbi_wiki::markdown::comrak_options;

fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("reserialize")
}

fn parse_then_render(content: &str) -> String {
    let arena = Arena::new();
    let options = comrak_options();
    let root = comrak::parse_document(&arena, content, &options);
    let mut out = String::new();
    comrak::format_commonmark(root, &options, &mut out).expect("format_commonmark");
    out
}

#[test]
fn golden_round_trip_matches_snapshot() {
    let root = fixtures_root();
    let input_path = root.join("golden.md");
    let expected_path = root.join("golden.expected.md");
    let input = fs::read_to_string(&input_path).expect("read golden.md");
    let actual = parse_then_render(&input);

    if std::env::var_os("UPDATE_SNAPSHOT").is_some() {
        fs::write(&expected_path, &actual).expect("write snapshot");
        return;
    }

    let expected = fs::read_to_string(&expected_path).unwrap_or_else(|e| {
        panic!(
            "missing snapshot {expected_path:?}: {e}\n\
             rerun with UPDATE_SNAPSHOT=1 to generate it"
        )
    });
    assert_eq!(
        actual, expected,
        "reserialize output diverged from golden.expected.md \
         — review changes and rerun with UPDATE_SNAPSHOT=1 if intentional"
    );
}
