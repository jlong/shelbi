// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-search/tests/perf.rs), MIT in Shelbi.

//! Performance smoke test for [`shelbi_wiki::search::search`].
//!
//! Sub-plan DoD: "Large fixture (1000 documents) returns in <1s on
//! CI." This test materialises 1000 small markdown documents in a
//! tempdir, runs a substring search across all of them, and asserts
//! the wall clock comes back well under one second.
//!
//! It runs on every workspace test invocation; if a refactor ever
//! regresses live-mode search past the budget, this fails before CI
//! merges it.

use std::fs;
use std::time::{Duration, Instant};

use shelbi_wiki::search::{search, Query, QueryMode};
use tempfile::TempDir;

#[test]
fn search_one_thousand_docs_under_one_second() {
    let dir = TempDir::new().expect("tempdir");
    // 1000 docs of ~10 lines each, half containing the needle.
    for i in 0..1000 {
        let path = dir.path().join(format!("doc-{i:04}.md"));
        let mut body = String::with_capacity(256);
        for line in 0..10 {
            if line == 5 && i % 2 == 0 {
                body.push_str("PIX-perf needle line for the matcher\n");
            } else {
                body.push_str("filler line with no match\n");
            }
        }
        fs::write(&path, body).expect("write fixture");
    }

    let query = Query {
        pattern: "PIX-perf".to_string(),
        mode: QueryMode::SubstringExact,
        max_total: 0,
        ..Query::default()
    };

    let start = Instant::now();
    let hits = search(dir.path(), &query).expect("search");
    let elapsed = start.elapsed();

    assert_eq!(hits.len(), 500, "expected 500 matches across the corpus");
    assert!(
        elapsed < Duration::from_secs(1),
        "1000-doc search took {elapsed:?}, exceeds 1s budget"
    );

    eprintln!("cstore-search 1000-doc smoke: {elapsed:?}");
}
