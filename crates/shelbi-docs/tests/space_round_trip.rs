//! Reads and rewrites a real ContextStore space.
//!
//! `tests/fixtures/planning-space/` is a subset of this repo's `docs/planning`
//! branch (at 82605ebc), the ContextStore space Shelbi's plans live in, copied
//! byte for byte: its settings, `.csignore`, `.gitignore`, the blank template,
//! a per-folder settings file in the legacy flat form, and a handful of
//! documents. Each test works on a fresh copy in a temp dir, so the fixture
//! itself is never written.

use std::path::{Path, PathBuf};

use shelbi_docs::fs::folder::list_folders;
use shelbi_docs::fs::settings::{
    ensure_gitignore, load_space_settings, save_space_settings, space_settings_path,
};
use shelbi_docs::fs::tree::build_folder_tree;
use shelbi_docs::markdown::sections::{append_to_section, read_section, write_section};
use shelbi_docs::merge::{resolve, MergeResult};
use shelbi_docs::model::list_documents;
use shelbi_docs::search::{search, Query, QueryMode};
use yaml_serde::Value;

fn fixture() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("planning-space")
}

/// A scratch copy of the fixture space.
fn copy_of_fixture() -> tempfile::TempDir {
    let tmp = tempfile::tempdir().expect("tempdir");
    copy_dir(&fixture(), tmp.path());
    tmp
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let dest = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

fn yaml_file(path: &Path) -> Value {
    yaml_serde::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[test]
fn the_planning_space_settings_load() {
    let settings = load_space_settings(&fixture()).expect("load");
    assert_eq!(settings.name, "Shelbi");
    assert_eq!(settings.created_at, "2026-06-20T00:00:00Z");
    assert_eq!(settings.show_frontmatter, Some(false));
    let order = settings.folder_order.expect("folderOrder");
    assert_eq!(order[""], ["Product", "Plans", "Research", "reviews"]);
    assert!(settings.extra.is_empty(), "{:?}", settings.extra);
}

#[test]
fn rewriting_the_settings_keeps_every_key_and_value() {
    let space = copy_of_fixture();
    let path = space_settings_path(space.path());
    let before = yaml_file(&path);

    let settings = load_space_settings(space.path()).unwrap();
    save_space_settings(space.path(), &settings).unwrap();

    assert_eq!(yaml_file(&path), before, "rewritten:\n{}", read(&path));
}

#[test]
fn unknown_settings_keys_survive_a_load_modify_save() {
    let space = copy_of_fixture();
    let path = space_settings_path(space.path());
    // The real file plus keys this crate doesn't model, interleaved with the
    // ones it does, in shapes a newer ContextStore might write.
    std::fs::write(
        &path,
        "name: Shelbi\n\
         futureFlag: true\n\
         createdAt: '2026-06-20T00:00:00Z'\n\
         futureBlock:\n  \
           nested:\n  \
           - 1\n  \
           - two\n  \
           label: Hello\n\
         folderOrder:\n  \
           '':\n  \
           - Product\n  \
           - Plans\n\
         showFrontmatter: false\n\
         futureList: [a, b]\n",
    )
    .unwrap();
    let before = yaml_file(&path);

    let mut settings = load_space_settings(space.path()).unwrap();
    let keys: Vec<&str> = settings
        .extra
        .keys()
        .map(|k| k.as_str().unwrap())
        .collect();
    assert_eq!(keys, ["futureFlag", "futureBlock", "futureList"]);

    settings
        .folder_order
        .as_mut()
        .unwrap()
        .insert(String::new(), vec!["Plans".into(), "Product".into()]);
    save_space_settings(space.path(), &settings).unwrap();

    let after = yaml_file(&path);
    for key in ["futureFlag", "futureBlock", "futureList", "name", "createdAt"] {
        assert_eq!(after[key], before[key], "{key} changed:\n{}", read(&path));
    }
    let root_order = &after["folderOrder"][""];
    assert_eq!(root_order[0].as_str(), Some("Plans"));
    assert_eq!(root_order[1].as_str(), Some("Product"));
}

#[test]
fn folders_follow_folder_order_then_name() {
    let space = fixture();
    let tree = build_folder_tree(&space, Some(&space)).unwrap();
    let names: Vec<&str> = tree.iter().map(|n| n.name.as_str()).collect();
    // `reviews` is listed but absent; `Engineering` isn't listed, so it
    // follows the listed ones.
    assert_eq!(names, ["Product", "Plans", "Research", "Engineering"]);
}

#[test]
fn a_flat_folder_settings_file_names_the_folder() {
    // `Engineering/Code Review/.contextstore/settings.yml` is `name: Reviews`
    // with no trailing newline: the legacy flat form.
    let folders = list_folders(&fixture().join("Engineering")).unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].name, "Reviews");
    assert_eq!(folders[0].relative_path, "Code Review");
}

#[test]
fn documents_are_titled_by_their_h1() {
    let space = fixture();
    let titles: Vec<String> = list_documents(&space.join("Product"), &space, "Product")
        .unwrap()
        .into_iter()
        .map(|e| e.title)
        .collect();
    assert_eq!(titles, ["Issues", "Vision"]);
}

#[test]
fn writing_a_section_leaves_the_rest_of_a_real_doc_alone() {
    let original = read(&fixture().join("Product").join("vision.md"));
    let section = read_section(&original, "## What Shelbi is").unwrap();
    assert!(section.starts_with("## What Shelbi is\n"));

    let edited = write_section(&original, "## What Shelbi is", "Rewritten.\n").unwrap();
    assert_eq!(
        read_section(&edited, "## What Shelbi is").unwrap(),
        "## What Shelbi is\nRewritten."
    );
    // Everything before the section, and every later section, is unchanged.
    let head = &original[..original.find("## What Shelbi is").unwrap()];
    assert!(edited.starts_with(head));
    for heading in ["## What has landed since July", "## What Shelbi will not be"] {
        assert_eq!(
            read_section(&edited, heading).unwrap(),
            read_section(&original, heading).unwrap()
        );
    }
}

#[test]
fn additions_to_different_sections_of_a_real_doc_merge() {
    let base = read(&fixture().join("Product").join("vision.md"));
    let ours = append_to_section(&base, "## What Shelbi is", "Ours added this paragraph.").unwrap();
    let theirs =
        append_to_section(&base, "## What Shelbi will not be", "Theirs added this one.").unwrap();
    match resolve(&base, &ours, &theirs) {
        MergeResult::Resolved(merged) => {
            assert!(merged.contains("Ours added this paragraph."));
            assert!(merged.contains("Theirs added this one."));
            assert!(merged.starts_with("# Vision\n"));
        }
        MergeResult::Conflict => panic!("additions to different sections should merge"),
    }
}

#[test]
fn rewriting_a_whole_section_on_each_side_is_a_conflict() {
    // ContextStore's AST layer is conservative: a base block one side
    // dropped is a conflict, never a silent deletion. (Layer 1 panics inside
    // diff-match-patch-rs on these inputs; `resolve` catches that too.)
    let base = read(&fixture().join("Product").join("vision.md"));
    let ours = write_section(&base, "## What Shelbi is", "Ours rewrote this.\n").unwrap();
    let theirs = write_section(&base, "## What Shelbi will not be", "Theirs rewrote this.\n")
        .unwrap();
    assert_eq!(resolve(&base, &ours, &theirs), MergeResult::Conflict);
}

#[test]
fn search_finds_text_across_the_space() {
    let hits = search(
        &fixture(),
        &Query {
            pattern: "linear".into(),
            mode: QueryMode::Substring,
            ..Query::default()
        },
    )
    .unwrap();
    assert!(hits
        .iter()
        .any(|m| m.relative_path == Path::new("Plans/linear-integration.md")));
}

#[test]
fn an_existing_gitignore_is_left_alone() {
    let space = copy_of_fixture();
    let path = space.path().join(".gitignore");
    let before = read(&path);
    ensure_gitignore(space.path()).unwrap();
    assert_eq!(read(&path), before);
}
