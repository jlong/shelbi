// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-merge/tests/fixture_corpus.rs), MIT in Shelbi.

//! Walks every directory under `fixtures/merge/` and runs the case through
//! the pipeline.
//!
//! Each case directory contains `base.md`, `ours.md`, `theirs.md`, and
//! exactly one of:
//!
//! - `expected.md` — assert the pipeline returns `Resolved(content)` byte-equal
//!   to the file's contents.
//! - `expected-conflict` (empty marker file) — assert the pipeline returns
//!   `Conflict`.
//!
//! Adding a new directory automatically adds a new case. Cases live in
//! `tests/fixtures/merge/` (at workspace level in ContextStore).

use std::fs;
use std::path::{Path, PathBuf};

use shelbi_wiki::merge::{resolve, MergeResult};

fn corpus_root() -> PathBuf {
    // CARGO_MANIFEST_DIR points at the crate root; the corpus lives in tests/fixtures.
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("merge")
}

fn list_cases(root: &Path) -> Vec<PathBuf> {
    let mut cases: Vec<PathBuf> = fs::read_dir(root)
        .unwrap_or_else(|e| panic!("read corpus root {root:?}: {e}"))
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    cases
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

#[test]
fn every_fixture_case_matches_pipeline_output() {
    let root = corpus_root();
    let cases = list_cases(&root);
    assert!(
        !cases.is_empty(),
        "no fixture cases found under {root:?} — corpus is empty"
    );

    let mut failures: Vec<String> = Vec::new();

    for case in &cases {
        let name = case
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("<unnamed>");
        let base = read(&case.join("base.md"));
        let ours = read(&case.join("ours.md"));
        let theirs = read(&case.join("theirs.md"));

        let expected_path = case.join("expected.md");
        let conflict_marker = case.join("expected-conflict");

        let outcome = resolve(&base, &ours, &theirs);

        match (expected_path.exists(), conflict_marker.exists()) {
            (true, true) => failures.push(format!(
                "{name}: fixture has both expected.md and expected-conflict"
            )),
            (false, false) => failures.push(format!(
                "{name}: fixture missing both expected.md and expected-conflict"
            )),
            (true, false) => {
                let expected = read(&expected_path);
                match outcome {
                    MergeResult::Resolved(actual) if actual == expected => {}
                    MergeResult::Resolved(actual) => failures.push(format!(
                        "{name}: resolved content differs from expected.md\n--- expected ---\n{expected}\n--- actual ---\n{actual}"
                    )),
                    MergeResult::Conflict => failures
                        .push(format!("{name}: pipeline returned Conflict; expected Resolved")),
                }
            }
            (false, true) => {
                if outcome != MergeResult::Conflict {
                    failures.push(format!(
                        "{name}: pipeline returned {outcome:?}; expected Conflict"
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} fixture(s) failed:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n\n")
    );
}
