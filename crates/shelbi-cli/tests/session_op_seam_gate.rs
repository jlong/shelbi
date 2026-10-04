//! Session-seam gate: production code in the consumer crates
//! (`shelbi-orchestrator`, `shelbi-tui`, `shelbi-cli`) must perform *session
//! operations* through the [`shelbi_orchestrator::session_backend::SessionBackend`]
//! trait — never by calling the low-level `shelbi_tmux::` session-operation
//! functions directly.
//!
//! Why this matters: the remove-tmux effort routes every session operation
//! (spawn, kill, probe, send, snapshot/history, title, metadata, enumerate,
//! respawn, resize, injection lock) behind the `SessionBackend` seam so a
//! non-tmux backend can be dropped in later (`rt-backend-sessions`). A direct
//! `shelbi_tmux::capture` / `has_session` / `send_line` / … call bypasses that
//! seam and nails the call site to tmux. This gate fails the build if a new
//! direct session-op call sneaks into a file that has already been migrated (or
//! into brand-new code), so the seam can't quietly re-open.
//!
//! **Scope.** Only the `shelbi_tmux::` *session-operation* functions are gated
//! (see [`FORBIDDEN`]). The address helpers (`session_target`,
//! `command_target`) and the layout/topology functions (`new_window`,
//! `kill_window`) are deliberately *not* gated: layout is not abstracted behind
//! the seam — the ~200 raw layout calls are deleted outright when the
//! single-process TUI replaces what they do (Phase 4), not moved behind a
//! trait. This gate also does not police raw `["tmux", …]` argv; that is the
//! layout surface, tracked in `docs/removing-tmux/README.md`.
//!
//! **Allowlist.** A handful of files are still permitted to make direct
//! session-op calls; each is listed in [`ALLOWED`] with the reason it is exempt
//! and when it loses the exemption. As those files are migrated or deleted in
//! later phases, their entries come off the list and the gate tightens
//! automatically. Keep [`ALLOWED`] in sync with
//! `docs/removing-tmux/README.md`.
//!
//! Robustness note: like `board_seam_gate.rs`, the scan parses each file with
//! `syn` and walks the AST rather than grepping text, so it is not fooled by
//! `#[cfg(test)]` modules (which legitimately drive a real tmux server and are
//! deleted at cutover), by doc comments that mention a function name, or by
//! string literals.

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

/// The `shelbi_tmux::` session-operation functions no production call-site
/// outside the tmux backend impl (and the [`ALLOWED`] files) may invoke
/// directly. Keep in sync with the methods on
/// [`shelbi_orchestrator::session_backend::SessionBackend`].
///
/// Deliberately excluded: `session_target` / `command_target` (address
/// helpers, not session ops) and `new_window` / `kill_window` (tmux layout /
/// topology, deleted in Phase 4 rather than moved behind the seam).
const FORBIDDEN: &[&str] = &[
    "new_session",
    "has_session",
    "has_session_with_deadline",
    "send_text",
    "send_enter",
    "send_line",
    "capture",
    "capture_history",
    "pane_title",
];

/// Files still permitted to make direct `shelbi_tmux::` session-op calls, as
/// path suffixes. Each entry is either the backend implementation itself or
/// code slated for migration/deletion in a later remove-tmux phase.
///
/// Keep this in sync with the allowlist table in
/// `docs/removing-tmux/README.md`.
const ALLOWED: &[&str] = &[
    // The tmux backend *is* the seam: these calls are the delegation every
    // other caller now routes through.
    "shelbi-orchestrator/src/session_backend.rs",
    // The Phase 6 cutover migration pass (`rt-cutover-migration`) must query the
    // *legacy tmux* runtime directly: after the flip, the `SessionBackend` seam
    // resolves to the session-process backend, which knows nothing about the old
    // `shelbi-<p>` / `shelbi-w-<ws>` tmux sessions it is the pass's whole job to
    // find and confirm gone. Removed with the module at `rt-cutover-delete`.
    "shelbi-orchestrator/src/migration.rs",
    // Orchestrator bootstrap + stash-session probes. Not in the Phase 2
    // caller-migration scope; moves behind the seam when the poller and
    // supervision move to the daemon (Phase 3).
    "shelbi-orchestrator/src/lib.rs",
    // Legacy agent commands, deleted wholesale at the Phase 6 cutover
    // (`shelbi spawn`, `tail`, and all of `merge`). Not worth migrating onto
    // the seam only to delete.
    "shelbi-cli/src/commands/spawn.rs",
    "shelbi-cli/src/commands/tail.rs",
    "shelbi-cli/src/commands/merge.rs",
];

struct Finding {
    file: PathBuf,
    line: usize,
    name: String,
}

/// AST walker that records direct `shelbi_tmux::<session-op>` calls in
/// production code. It never descends into `#[cfg(test)]` / `#[test]` items, so
/// the integration tests that drive a real tmux server (and are deleted at
/// cutover) are ignored.
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

/// True when `path` names a `shelbi_tmux::<forbidden>` free function: some
/// segment is `shelbi_tmux` and the final segment is a forbidden session op.
/// Callers always fully-qualify (`shelbi_tmux::capture(…)`); no file imports a
/// session-op function by bare name, so matching the qualified path is both
/// precise (a local helper named `capture` is never flagged) and complete.
fn is_forbidden_session_op(path: &syn::Path) -> Option<String> {
    let mentions_tmux = path
        .segments
        .iter()
        .any(|seg| seg.ident == "shelbi_tmux");
    if !mentions_tmux {
        return None;
    }
    let last = path.segments.last()?;
    let name = last.ident.to_string();
    FORBIDDEN.contains(&name.as_str()).then_some(name)
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
            if let Some(name) = is_forbidden_session_op(&path.path) {
                let line = path
                    .path
                    .segments
                    .last()
                    .map(|seg| seg.ident.span().start().line)
                    .unwrap_or(0);
                self.findings.push(Finding {
                    file: self.file.to_path_buf(),
                    line,
                    name,
                });
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

/// True when `file` matches an [`ALLOWED`] path suffix.
fn is_allowed(file: &Path) -> bool {
    let normalized = file.to_string_lossy().replace('\\', "/");
    ALLOWED.iter().any(|suffix| normalized.ends_with(suffix))
}

#[test]
fn no_direct_session_op_calls_in_production_code() {
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
            "session-seam gate can't find {} — did the crate layout move? \
             Update the scan roots in this test.",
            root.display()
        );
        rust_files(root, &mut files);
    }

    let mut findings = Vec::new();
    for file in &files {
        if is_allowed(file) {
            continue;
        }
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
            "production code must perform session operations through the \
             `SessionBackend` trait, not the low-level `shelbi_tmux::` \
             functions directly.\n\
             Route these through `shelbi_orchestrator::session_backend::backend()` \
             with a `SessionTarget` instead (see that module's docs):\n",
        );
        for f in &findings {
            lines.push_str(&format!(
                "  {}:{} — direct call to `shelbi_tmux::{}`\n",
                f.file.display(),
                f.line,
                f.name
            ));
        }
        panic!("{lines}");
    }
}
