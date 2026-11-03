//! Project-source gate: the desktop's views, its feed and its window read a
//! project only through the window's `ProjectSource`
//! (`shelbi_app::source`), never by calling a `shelbi_state` /
//! `shelbi_orchestrator` reader for it directly.
//!
//! Why: a window for a project on another machine draws the same views over
//! a remote source. A view that reads the local disk itself would quietly
//! show this machine's state (or nothing) in that window. This test fails the
//! build when such a call sneaks back in.
//!
//! Like the board-seam gate (`crates/shelbi-cli/tests/board_seam_gate.rs`) it
//! parses each file with `syn` and walks the AST, so doc comments and string
//! literals that name a reader don't count, and `#[cfg(test)]` / `#[test]`
//! items are skipped (fixtures seed state with the readers). It flags a
//! qualified path (a call, or a function passed as a value) and a `use` whose
//! trailing segments match a forbidden reader; a bare name is a local or one
//! of those imports. Method calls are the source's own and are never
//! flagged.
//!
//! What stays the viewer's own (and so isn't listed): the user config,
//! window state, recent remotes, the update check, feedback, bootstrap and
//! discovery, the project list, and the writes later phases move behind the
//! daemon.

use std::path::{Path, PathBuf};

use syn::visit::{self, Visit};

/// The readers (and the link's local plumbing) a scanned file must reach
/// through the window's source or link instead. Each entry is a path suffix:
/// `["load_project"]` matches `shelbi_state::load_project`, and
/// `["WorkflowConfig", "load"]` matches `WorkflowConfig::load` but not every
/// `load`.
const FORBIDDEN: &[&[&str]] = &[
    // Sidebar and project.
    &["read_sidebar_model"],
    &["read_config_skeleton"],
    &["load_project"],
    &["project_dir"],
    &["config_project_dir"],
    &["command_model", "edit_targets"],
    &["agents_dir"],
    &["zenmode_path"],
    &["workflows_dir"],
    &["changelog_template_path"],
    // Board, task detail, search, palette.
    &["read_open_board"],
    &["read_closed_page"],
    &["load_more_closed"],
    &["issue_store_for"],
    &["resolve_issue_store"],
    &["read_board_report"],
    &["read_board_report_with_cfg"],
    &["list_workflows"],
    &["load_project_statuses"],
    &["search_issues"],
    &["GithubSearch"],
    &["SearchWorker", "github"],
    &["read_corpus"],
    &["load_keymaps"],
    &["execute_mutation"],
    &["read_expected"],
    // Activity.
    &["read_activity_window"],
    &["read_activity_window_staged"],
    &["events_log_path"],
    &["ProjectInfo", "read"],
    // Workspaces, review, machines.
    &["workspace_details"],
    &["review_open_target"],
    &["review_open_info"],
    &["review_slots"],
    &["read_machines_model"],
    &["probe_host_reachability"],
    // Errors, Zen, changelog.
    &["read_errors_with_unread"],
    &["read_errors"],
    &["read_state"],
    &["should_show_intro"],
    &["read_changelog"],
    &["changelog", "read_entries"],
    &["changelog_stamp"],
    &["ChangelogWatch"],
    // Settings.
    &["summary", "read"],
    &["summary", "read_project"],
    &["WorkflowConfig", "load"],
    &["AgentsConfig", "load"],
    &["ProjectGit", "load"],
    &["workspaces", "read"],
    &["workflows", "load"],
    &["workflows_state", "load"],
    &["list_agents"],
    // The link's local plumbing: sessions, change and layout subscriptions.
    &["session_connect", "connect"],
    &["changes", "spawn"],
    &["changes", "spawn_observed"],
    &["layout_sub", "spawn"],
];

/// Where the views, the feed and the window live, under `desktop/src`.
const SCAN: &[&str] = &["views", "refresh.rs", "feeds.rs", "projects.rs", "app.rs"];

#[derive(Debug, PartialEq)]
struct Finding {
    file: PathBuf,
    line: usize,
    path: String,
}

struct Scan<'a> {
    file: &'a Path,
    findings: Vec<Finding>,
}

/// True when `attrs` gate the item to test builds: `#[test]` (or
/// `#[gpui::test]`), or a `cfg` that mentions `test` at any depth
/// (`cfg(test)`, `cfg(any(test, feature = "visual-check"))`).
fn is_test_gated(attrs: &[syn::Attribute]) -> bool {
    fn mentions_test(tokens: proc_macro2::TokenStream) -> bool {
        tokens.into_iter().any(|tt| match tt {
            proc_macro2::TokenTree::Ident(i) => i == "test",
            proc_macro2::TokenTree::Group(g) => mentions_test(g.stream()),
            _ => false,
        })
    }
    attrs.iter().any(|attr| {
        if attr.path().segments.last().is_some_and(|s| s.ident == "test") {
            return true;
        }
        match &attr.meta {
            syn::Meta::List(list) if list.path.is_ident("cfg") => mentions_test(list.tokens.clone()),
            _ => false,
        }
    })
}

/// The forbidden entry `segments` ends with, if any.
fn forbidden(segments: &[String]) -> Option<&'static [&'static str]> {
    FORBIDDEN.iter().copied().find(|f| {
        segments.len() >= f.len() && segments[segments.len() - f.len()..].iter().zip(f.iter()).all(|(a, b)| a == b)
    })
}

impl Scan<'_> {
    fn check(&mut self, segments: Vec<String>, line: usize) {
        if forbidden(&segments).is_some() {
            self.findings.push(Finding {
                file: self.file.to_path_buf(),
                line,
                path: segments.join("::"),
            });
        }
    }

    /// Every full path a `use` tree names (`a::{b, c::d}` → `a::b`,
    /// `a::c::d`; a rename checks the original name).
    fn use_paths(tree: &syn::UseTree, prefix: &mut Vec<String>, out: &mut Vec<(Vec<String>, usize)>) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                Self::use_paths(&p.tree, prefix, out);
                prefix.pop();
            }
            syn::UseTree::Name(n) => {
                let mut path = prefix.clone();
                path.push(n.ident.to_string());
                out.push((path, n.ident.span().start().line));
            }
            syn::UseTree::Rename(r) => {
                let mut path = prefix.clone();
                path.push(r.ident.to_string());
                out.push((path, r.ident.span().start().line));
            }
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    Self::use_paths(t, prefix, out);
                }
            }
            syn::UseTree::Glob(_) => {}
        }
    }
}

impl<'a> Visit<'a> for Scan<'a> {
    fn visit_item_mod(&mut self, node: &'a syn::ItemMod) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'a syn::ItemFn) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_fn(self, node);
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'a syn::ImplItemFn) {
        if !is_test_gated(&node.attrs) {
            visit::visit_impl_item_fn(self, node);
        }
    }

    fn visit_item_impl(&mut self, node: &'a syn::ItemImpl) {
        if !is_test_gated(&node.attrs) {
            visit::visit_item_impl(self, node);
        }
    }

    fn visit_stmt(&mut self, node: &'a syn::Stmt) {
        let attrs = match node {
            syn::Stmt::Local(l) => &l.attrs,
            syn::Stmt::Item(_) | syn::Stmt::Expr(..) | syn::Stmt::Macro(_) => {
                return visit::visit_stmt(self, node);
            }
        };
        if !is_test_gated(attrs) {
            visit::visit_stmt(self, node);
        }
    }

    fn visit_item_use(&mut self, node: &'a syn::ItemUse) {
        if is_test_gated(&node.attrs) {
            return;
        }
        let mut paths = Vec::new();
        Self::use_paths(&node.tree, &mut Vec::new(), &mut paths);
        for (path, line) in paths {
            self.check(path, line);
        }
    }

    fn visit_expr_path(&mut self, node: &'a syn::ExprPath) {
        // A bare name is a local, or an import the `use` check has seen.
        if node.path.segments.len() > 1 {
            let segments: Vec<String> = node.path.segments.iter().map(|s| s.ident.to_string()).collect();
            let line = node.path.segments.last().map_or(0, |s| s.ident.span().start().line);
            self.check(segments, line);
        }
        visit::visit_expr_path(self, node);
    }
}

fn rust_files(path: &Path, out: &mut Vec<PathBuf>) {
    if path.is_dir() {
        for entry in std::fs::read_dir(path).into_iter().flatten().flatten() {
            rust_files(&entry.path(), out);
        }
    } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
        out.push(path.to_path_buf());
    }
}

fn scan_source(file: &Path, src: &str) -> Vec<Finding> {
    let ast = syn::parse_file(src).unwrap_or_else(|e| panic!("parse {}: {e}", file.display()));
    let mut scan = Scan {
        file,
        findings: Vec::new(),
    };
    scan.visit_file(&ast);
    scan.findings
}

#[test]
fn views_and_the_feed_read_a_project_only_through_its_source() {
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    for root in SCAN {
        let root = src.join(root);
        assert!(root.exists(), "the source gate can't find {}: update SCAN", root.display());
        rust_files(&root, &mut files);
    }
    assert!(files.len() > 20, "the gate scanned too little: {files:?}");
    let mut findings = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file).unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        findings.extend(scan_source(file, &text));
    }
    if !findings.is_empty() {
        let mut message = String::from(
            "views, the feed and the window must read a project through the window's \
             `ProjectSource` (and reach its sessions and subscriptions through its \
             `ProjectLink`), so a remote project's window draws the same views. \
             Route these through `shelbi_app::source`:\n",
        );
        for f in &findings {
            let rel = f.file.strip_prefix(&src).unwrap_or(&f.file);
            message.push_str(&format!("  desktop/src/{}:{}: `{}`\n", rel.display(), f.line, f.path));
        }
        panic!("{message}");
    }
}

#[test]
fn the_gate_flags_a_direct_read_and_skips_tests_docs_and_methods() {
    let src = r#"
        //! Mentions shelbi_state::load_project in a doc: fine.
        use shelbi_orchestrator::machines_model::read_machines_model;
        use shelbi_app::settings::{workspaces::read as read_pool, general};
        fn view(project: &str, source: &dyn Source, project_dir: &Path) {
            let _ = project_dir.join("x");
            let _ = shelbi_state::load_project(project);
            let _ = WorkflowConfig::load(project);
            let _ = WorkflowConfig::default();
            let reader = shelbi_app::board::read_open_board;
            let _ = source.open_board();
            let _ = "read_changelog";
            let _ = store.load(project);
        }
        #[cfg(test)]
        mod tests {
            fn fixture() { shelbi_state::load_project("x"); }
        }
        #[cfg(any(test, feature = "visual-check"))]
        mod fixture {
            fn write() { shelbi_state::project_dir("x"); }
        }
    "#;
    let found: Vec<String> = scan_source(Path::new("x.rs"), src).into_iter().map(|f| f.path).collect();
    assert_eq!(
        found,
        [
            "shelbi_orchestrator::machines_model::read_machines_model",
            "shelbi_app::settings::workspaces::read",
            "shelbi_state::load_project",
            "WorkflowConfig::load",
            "shelbi_app::board::read_open_board",
        ]
    );
}
