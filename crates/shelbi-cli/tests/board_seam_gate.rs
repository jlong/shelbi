//! Board-seam gate: production code in the consumer crates
//! (`shelbi-orchestrator`, `shelbi-tui`, `shelbi-cli`) must reach a project's
//! issue board **only** through the [`shelbi_state::IssueStore`] trait — never
//! by calling the low-level markdown-board functions directly.
//!
//! Why this matters: a project configured with `issue_tracker.backend: github`
//! resolves an `IssueStore` whose reads/writes go to GitHub. Any direct call to
//! `save_task` / `move_task` / `list_tasks` / … bypasses that resolution and
//! silently reads/writes the local `~/.shelbi/projects/<name>/tasks/` board
//! instead — the exact "phantom local writes a github backend never sees" bug
//! the pluggable-issue-trackers migration closes. This test fails the build if
//! a new direct call sneaks back in, so the seam can't quietly re-open.
//!
//! The low-level functions still exist (they are the *mechanism* the
//! `FileSystemStore` backend delegates to, and test fixtures use them to seed
//! boards) — they live in `shelbi-state`, which this gate does **not** scan.
//! Only production (non-`#[cfg(test)]`) code in the three consumer crates is
//! checked; `#[test]` / `#[cfg(test)]` items are skipped so fixtures that call
//! `save_task` to build a board remain fine.
//!
//! Robustness note: the scan parses each file with `syn` and walks the AST
//! rather than grepping text, so it is not fooled by the many `#[cfg(test)]`
//! modules interleaved with production code, by doc comments that mention a
//! function name, or by string literals. Method calls (`store.get()`,
//! `store.list()`, …) are trait calls and are intentionally *not* flagged;
//! only free-function calls whose final path segment is a forbidden name are.

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

/// The low-level filesystem board functions no production call-site outside
/// `FileSystemStore` may invoke directly. Keep in sync with the delegating
/// methods on [`shelbi_state::IssueStore`].
const FORBIDDEN: &[&str] = &[
    // reads
    "list_tasks",
    "list_column",
    "load_task",
    // writes
    "save_task",
    "save_task_unlocked",
    "move_task",
    "move_task_and_unassign",
    "delete_task",
    "renumber_column",
    "set_task_branch",
    "set_task_priority",
    "set_task_parked",
    "clear_task_parked",
    "park_review_task",
    "release_task_to_todo",
    "reject_review_task",
];

struct Finding {
    file: PathBuf,
    line: usize,
    name: String,
}

/// AST walker that records forbidden free-function calls in production code.
/// It never descends into `#[cfg(test)]` / `#[test]` items, so board fixtures
/// in test modules are ignored.
struct Scan<'a> {
    file: &'a Path,
    findings: Vec<Finding>,
}

/// True when `attrs` gate the item to test builds (`#[test]`, `#[cfg(test)]`,
/// `#[cfg(all(test, …))]`, `#[cfg(any(test, …))]`). Anything whose `cfg(...)`
/// token stream mentions `test` is treated as test-only — deliberately broad,
/// since the goal is to skip test code, not to police it.
fn is_test_gated(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if attr.path().is_ident("test") {
            return true;
        }
        if let syn::Meta::List(list) = &attr.meta {
            if list.path.is_ident("cfg") {
                // e.g. "test", "all(test , feature = \"x\")", "any(test , foo)".
                return list
                    .tokens
                    .clone()
                    .into_iter()
                    .any(|tt| tt.to_string() == "test");
            }
        }
        false
    })
}

impl<'a> Visit<'a> for Scan<'a> {
    fn visit_item_mod(&mut self, node: &'a syn::ItemMod) {
        if is_test_gated(&node.attrs) {
            return;
        }
        visit::visit_item_mod(self, node);
    }

    fn visit_item_fn(&mut self, node: &'a syn::ItemFn) {
        if is_test_gated(&node.attrs) {
            return;
        }
        visit::visit_item_fn(self, node);
    }

    fn visit_impl_item_fn(&mut self, node: &'a syn::ImplItemFn) {
        if is_test_gated(&node.attrs) {
            return;
        }
        visit::visit_impl_item_fn(self, node);
    }

    fn visit_item_impl(&mut self, node: &'a syn::ItemImpl) {
        if is_test_gated(&node.attrs) {
            return;
        }
        visit::visit_item_impl(self, node);
    }

    fn visit_expr_call(&mut self, node: &'a syn::ExprCall) {
        if let syn::Expr::Path(path) = node.func.as_ref() {
            if let Some(last) = path.path.segments.last() {
                let name = last.ident.to_string();
                if FORBIDDEN.contains(&name.as_str()) {
                    self.findings.push(Finding {
                        file: self.file.to_path_buf(),
                        line: last.ident.span().start().line,
                        name,
                    });
                }
            }
        }
        visit::visit_expr_call(self, node);
    }
}

/// Recursively collect every `*.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_direct_low_level_board_calls_in_production_code() {
    // `crates/shelbi-cli`; the sibling consumer crates live one level up.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let crates = manifest.parent().expect("crates/ dir");
    let scan_roots = [
        crates.join("shelbi-orchestrator/src"),
        crates.join("shelbi-tui/src"),
        crates.join("shelbi-cli/src"),
    ];

    let mut files = Vec::new();
    for root in &scan_roots {
        assert!(
            root.exists(),
            "board-seam gate can't find {} — did the crate layout move? \
             Update the scan roots in this test.",
            root.display()
        );
        rust_files(root, &mut files);
    }

    let mut findings = Vec::new();
    for file in &files {
        let src = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        let ast = syn::parse_file(&src)
            .unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
        let mut scan = Scan {
            file,
            findings: Vec::new(),
        };
        scan.visit_file(&ast);
        findings.extend(scan.findings);
    }

    if !findings.is_empty() {
        let mut lines = String::from(
            "production code must reach the board through the `IssueStore` trait, \
             not the low-level board functions directly.\n\
             Route these through a store resolved with \
             `shelbi_state::resolve_issue_store` / `issue_store_for` (or an \
             existing `&Project`'s `issue_tracker`) instead:\n",
        );
        for f in &findings {
            lines.push_str(&format!(
                "  {}:{} — direct call to `{}`\n",
                f.file.display(),
                f.line,
                f.name
            ));
        }
        panic!("{lines}");
    }
}
