//! An integration test links `shelbi-state` without `cfg(test)`, the way
//! every other crate's tests do, so only the harness-path check can tell it
//! is a test. When cargo moves its harnesses again (as 1.100's build-dir
//! layout did), this fails here instead of every test silently running as if
//! it were a real install: the live-home guard off, and the agent-runner
//! probe reading the machine's PATH.

#[test]
fn this_harness_is_recognized_as_a_test_binary() {
    assert!(
        shelbi_state::in_test_binary(),
        "a cargo test harness at {:?} isn't recognized as one",
        std::env::current_exe().unwrap(),
    );
}
