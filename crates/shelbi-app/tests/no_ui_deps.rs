//! Guards the crate's defining invariant: `shelbi-app` is the
//! toolkit-independent app model, so neither it nor `shelbi-state` may
//! depend on a UI toolkit (ratatui, crossterm, gpui) — directly or
//! transitively. The TUI and the desktop app are renderers *over* this
//! crate, not dependencies of it.
//!
//! The check runs `cargo tree` against the resolved dependency graph (so it
//! catches a transitive pull-in, not just a direct one) and asserts the
//! forbidden crates are absent. It skips gracefully when `cargo` isn't on
//! PATH or the metadata command can't run, the same way the workspace's
//! tmux-backed tests skip without `tmux` — CI has cargo, so the assertion
//! is enforced there.

use std::process::Command;

const FORBIDDEN: &[&str] = &["ratatui", "crossterm", "gpui"];

fn tree_for(package: &str) -> Option<String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--package",
            package,
            "--edges",
            "normal,build",
            "--prefix",
            "none",
            "--no-dedupe",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn assert_clean(package: &str) {
    let Some(tree) = tree_for(package) else {
        eprintln!("skipping no_ui_deps for {package}: `cargo tree` unavailable");
        return;
    };
    for crate_name in FORBIDDEN {
        // Match the crate at the start of a `cargo tree` line (name then a
        // space and a version), so a substring in some other crate's name
        // can't trip a false positive.
        let needle = format!("{crate_name} v");
        let hit = tree
            .lines()
            .any(|line| line.trim_start().starts_with(&needle));
        assert!(
            !hit,
            "{package} must not depend on `{crate_name}` (directly or \
             transitively); found it in the resolved dependency tree.\n\
             Full tree:\n{tree}"
        );
    }
}

#[test]
fn shelbi_app_has_no_ui_toolkit_dependency() {
    assert_clean("shelbi-app");
}

#[test]
fn shelbi_state_has_no_ui_toolkit_dependency() {
    // The chord-type migration moved shelbi-state off crossterm; this keeps
    // it that way (a regression here would also re-pull crossterm into
    // shelbi-app via shelbi-state).
    assert_clean("shelbi-state");
}
