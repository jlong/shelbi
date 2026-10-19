//! The view-model field checklist stays complete.
//!
//! `desktop/docs/parity.md` has a table per `shelbi_app::view` model (the
//! structs declared in `src/view.rs`, plus the ones `view.rs` re-exports from
//! `src/review.rs`) naming where the TUI and the desktop app display each
//! public field. This test reads the sources and the doc and fails when a
//! model gains a field the table doesn't list, when the table keeps a row for
//! a field that is gone, or when a row leaves a client's cell empty. So a new
//! field fails here until the table, and both renderers, account for it.
//!
//! It lives in the root workspace (not `desktop/`) so the CI that runs on
//! every `shelbi-app` change enforces it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn crate_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// `pub struct Name {` blocks at the top level of `source`, with their
/// `pub field:` names in order. Only structs named in `only` when given.
fn pub_structs(source: &str, only: Option<&[String]>) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut current: Option<(String, Vec<String>)> = None;
    for line in source.lines() {
        if let Some(rest) = line.strip_prefix("pub struct ") {
            if let Some(name) = rest.strip_suffix(" {") {
                current = Some((name.trim().to_string(), Vec::new()));
            }
            continue;
        }
        if line.starts_with('}') {
            if let Some((name, fields)) = current.take() {
                if only.is_none_or(|names| names.contains(&name)) {
                    out.insert(name, fields);
                }
            }
            continue;
        }
        if let Some((_, fields)) = current.as_mut() {
            if let Some(rest) = line.strip_prefix("    pub ") {
                if let Some((field, _)) = rest.split_once(':') {
                    fields.push(field.trim().to_string());
                }
            }
        }
    }
    out
}

/// The structs `view.rs` re-exports from `review.rs`
/// (`pub use crate::review::{A, B};`).
fn reexported_from_review(view: &str) -> Vec<String> {
    view.lines()
        .filter_map(|l| l.trim().strip_prefix("pub use crate::review::{"))
        .flat_map(|l| l.trim_end_matches("};").split(','))
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .collect()
}

/// Every view model and its public fields.
fn models() -> BTreeMap<String, Vec<String>> {
    let src = crate_dir().join("src");
    let view = read(&src.join("view.rs"));
    let mut models = pub_structs(&view, None);
    let names = reexported_from_review(&view);
    models.extend(pub_structs(&read(&src.join("review.rs")), Some(&names)));
    models
}

/// The doc's tables: `### \`Struct\`` headings, then `| \`field\` | TUI |
/// Desktop |` rows. Returns struct -> [(field, tui cell, desktop cell)].
fn doc_tables(doc: &str) -> BTreeMap<String, Vec<(String, String, String)>> {
    let mut out: BTreeMap<String, Vec<(String, String, String)>> = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in doc.lines() {
        if let Some(h) = line.strip_prefix("### ") {
            current = h
                .trim()
                .strip_prefix('`')
                .and_then(|h| h.strip_suffix('`'))
                .map(str::to_string);
            if let Some(name) = &current {
                out.entry(name.clone()).or_default();
            }
            continue;
        }
        if line.starts_with("## ") {
            current = None;
            continue;
        }
        let Some(name) = &current else { continue };
        let cells: Vec<&str> = line.trim().trim_matches('|').split('|').map(str::trim).collect();
        if cells.len() < 3 {
            continue;
        }
        let Some(field) = cells[0].strip_prefix('`').and_then(|c| c.strip_suffix('`')) else {
            continue;
        };
        out.get_mut(name).unwrap().push((
            field.to_string(),
            cells[1].to_string(),
            cells[2].to_string(),
        ));
    }
    out
}

fn parity_doc() -> String {
    read(&crate_dir().join("../../desktop/docs/parity.md"))
}

#[test]
fn the_parser_sees_the_models() {
    let models = models();
    // A sanity floor so a parser regression can't pass the real check by
    // finding nothing.
    for name in ["SidebarModel", "IssueCard", "MachineRow", "ErrorLogModel", "ReviewPanelModel"] {
        assert!(models.contains_key(name), "found {name}");
    }
    assert!(models["SidebarModel"].contains(&"project_label".to_string()));
    assert!(models.values().map(Vec::len).sum::<usize>() > 100);
}

#[test]
fn the_parity_table_lists_every_view_model_field() {
    let models = models();
    let tables = doc_tables(&parity_doc());
    let mut problems = Vec::new();
    for (model, fields) in &models {
        let Some(rows) = tables.get(model) else {
            problems.push(format!("no table for `{model}`"));
            continue;
        };
        for field in fields {
            match rows.iter().find(|(f, _, _)| f == field) {
                None => problems.push(format!("`{model}.{field}` has no row")),
                Some((_, tui, desktop)) => {
                    if tui.is_empty() {
                        problems.push(format!("`{model}.{field}` has no TUI cell"));
                    }
                    if desktop.is_empty() {
                        problems.push(format!("`{model}.{field}` has no Desktop cell"));
                    }
                }
            }
        }
        for (field, _, _) in rows {
            if !fields.contains(field) {
                problems.push(format!("`{model}.{field}` is listed but no longer exists"));
            }
        }
    }
    for model in tables.keys() {
        if !models.contains_key(model) {
            problems.push(format!("table for `{model}`, which is not a view model"));
        }
    }
    assert!(
        problems.is_empty(),
        "desktop/docs/parity.md is out of date with shelbi_app::view:\n  {}",
        problems.join("\n  ")
    );
}
