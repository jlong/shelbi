// Copied from ContextStore (32pixelsCo/ContextStore@8c2fdc180c5670fae1165c5d8c939c247e5ddc53, ContextStoreCore/crates/cstore-fs/src/tree.rs), MIT in Shelbi.

//! Folder tree for sidebar display, built **lazily**.
//!
//! Mirrors the Swift `FileSystemManager.buildFolderTree` behavior — filters via
//! the space's `.csignore` and applies the custom `folderOrder` from
//! `settings.yml` — but exposes two entry points instead of one eager recursion:
//!
//! * [`build_children`] materializes a **single level** (the immediate child
//!   folders of one directory), marking each with whether it is itself
//!   expandable ([`FolderTreeNode::has_children`]) without descending into it.
//!   The sidebar calls this on first render (root level) and again each time a
//!   folder is expanded, so the cost of populating the tree scales with the
//!   folder being looked at rather than the whole space.
//! * [`build_folder_tree`] still walks the entire tree eagerly, filling every
//!   node's `children`. It backs the features that genuinely need the full set
//!   of folders at once (the "Move to…" destination pickers and the search
//!   palette's folder scoping), which run on deliberate user actions rather than
//!   on the space-switch hot path.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::fs::settings::load_space_settings;
use crate::fs::FsError;

/// One node in the folder tree. Matches the Swift `FolderNode` model, plus a
/// `has_children` flag so a lazily-built node can advertise whether it is
/// expandable without its children having been loaded yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTreeNode {
    /// Relative path from the content root (e.g. `"engineering/backend"`).
    pub relative_path: String,
    /// Display name (the on-disk folder name).
    pub name: String,
    /// Absolute filesystem path.
    pub path: PathBuf,
    /// Whether this folder contains at least one non-ignored subfolder — i.e.
    /// whether it can be expanded. Computed one level deep, so it is accurate
    /// even when [`Self::children`] has not been materialized.
    pub has_children: bool,
    /// Child folders (sorted per the space's `folderOrder`), or `None` when this
    /// node's level has not been loaded yet. [`build_children`] leaves this
    /// `None`; [`build_folder_tree`] fills it with `Some` at every level.
    pub children: Option<Vec<FolderTreeNode>>,
}

/// Build the immediate child folders of `parent_relative` (the empty string for
/// the content root), one level deep. Each returned node carries an accurate
/// [`FolderTreeNode::has_children`] flag but leaves `children` `None` — callers
/// materialize deeper levels on demand by calling this again with the child's
/// relative path.
///
/// Honors `.csignore` and the per-parent `folderOrder` exactly as
/// [`build_folder_tree`] does, so a lazily-assembled tree is indistinguishable
/// from an eagerly-built one.
pub fn build_children(
    content_root: &Path,
    settings_root: Option<&Path>,
    parent_relative: &str,
) -> Result<Vec<FolderTreeNode>, FsError> {
    let ignore = load_ignore(content_root)?;
    let folder_order = load_folder_order(content_root, settings_root);
    let dir = if parent_relative.is_empty() {
        content_root.to_path_buf()
    } else {
        content_root.join(parent_relative)
    };
    Ok(list_level(
        &dir,
        content_root,
        &ignore,
        &folder_order,
        parent_relative,
    ))
}

/// Build the recursive folder tree under `content_root`, honoring `.csignore`
/// and the per-parent `folderOrder`. Every node's `children` is `Some`. Prefer
/// [`build_children`] for the sidebar; this eager walk is for the features that
/// need every folder up front (move pickers, search scoping).
///
/// `settings_root` is the space root that owns `.contextstore/`. It may differ
/// from `content_root` when `settings.root` is set (orphan-branch deployment),
/// in which case the content lives in a subfolder while settings stay at the
/// repo root.
pub fn build_folder_tree(
    content_root: &Path,
    settings_root: Option<&Path>,
) -> Result<Vec<FolderTreeNode>, FsError> {
    let ignore = load_ignore(content_root)?;
    let folder_order = load_folder_order(content_root, settings_root);
    Ok(walk(content_root, content_root, &ignore, &folder_order))
}

fn load_ignore(content_root: &Path) -> Result<crate::ignore::CsIgnore, FsError> {
    crate::ignore::CsIgnore::load(content_root).map_err(|e| FsError::Io {
        path: content_root.to_string_lossy().into_owned(),
        source: std::io::Error::other(format!("csignore load: {e:?}")),
    })
}

fn load_folder_order(
    content_root: &Path,
    settings_root: Option<&Path>,
) -> BTreeMap<String, Vec<String>> {
    let settings_path = settings_root.unwrap_or(content_root);
    match load_space_settings(settings_path) {
        Ok(s) => s.folder_order.unwrap_or_default(),
        Err(_) => BTreeMap::new(),
    }
}

/// List the immediate non-ignored subfolders of `dir` (relative path
/// `parent_relative`), sorted per `folder_order`, leaving each node's children
/// unloaded. Shared by [`build_children`].
fn list_level(
    dir: &Path,
    space_root: &Path,
    ignore: &crate::ignore::CsIgnore,
    folder_order: &BTreeMap<String, Vec<String>>,
    parent_relative: &str,
) -> Vec<FolderTreeNode> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        let relative = match path.strip_prefix(space_root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => path.clone(),
        };
        if ignore.is_ignored(&relative, true) {
            continue;
        }
        nodes.push(FolderTreeNode {
            relative_path: relative.to_string_lossy().into_owned(),
            name: name.to_string(),
            path: path.clone(),
            has_children: has_child_dir(&path, space_root, ignore),
            children: None,
        });
    }
    sort_level(&mut nodes, folder_order, parent_relative);
    nodes
}

/// Whether `dir` has at least one non-ignored immediate subfolder — the
/// `has_children` flag, computed without recursing further. Stops at the first
/// match so a folder with many children costs the same as one with a single
/// child.
fn has_child_dir(dir: &Path, space_root: &Path, ignore: &crate::ignore::CsIgnore) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let relative = match path.strip_prefix(space_root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => path.clone(),
        };
        if ignore.is_ignored(&relative, true) {
            continue;
        }
        return true;
    }
    false
}

/// The eager, whole-tree walk backing [`build_folder_tree`]. Fills every node's
/// `children` with `Some`.
fn walk(
    dir: &Path,
    space_root: &Path,
    ignore: &crate::ignore::CsIgnore,
    folder_order: &BTreeMap<String, Vec<String>>,
) -> Vec<FolderTreeNode> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut nodes = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if !metadata.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };

        let relative = match path.strip_prefix(space_root) {
            Ok(p) => p.to_path_buf(),
            Err(_) => path.clone(),
        };
        if ignore.is_ignored(&relative, true) {
            continue;
        }

        let children = walk(&path, space_root, ignore, folder_order);
        nodes.push(FolderTreeNode {
            relative_path: relative.to_string_lossy().into_owned(),
            name: name.to_string(),
            path,
            has_children: !children.is_empty(),
            children: Some(children),
        });
    }

    let parent_relative = if dir == space_root {
        String::new()
    } else {
        dir.strip_prefix(space_root)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    sort_level(&mut nodes, folder_order, &parent_relative);
    nodes
}

/// Sort one level of sibling folders: honor the parent's explicit `folderOrder`
/// entry when present (unlisted names fall to the end, alphabetized), otherwise
/// sort case-insensitively by name. Shared by the lazy and eager builders so
/// both produce identical ordering.
fn sort_level(
    nodes: &mut [FolderTreeNode],
    folder_order: &BTreeMap<String, Vec<String>>,
    parent_relative: &str,
) {
    if let Some(order) = folder_order.get(parent_relative) {
        if !order.is_empty() {
            nodes.sort_by(|a, b| {
                let ai = order.iter().position(|n| n == &a.name);
                let bi = order.iter().position(|n| n == &b.name);
                match (ai, bi) {
                    (Some(ai), Some(bi)) => ai.cmp(&bi),
                    (Some(_), None) => std::cmp::Ordering::Less,
                    (None, Some(_)) => std::cmp::Ordering::Greater,
                    (None, None) => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                }
            });
            return;
        }
    }
    nodes.sort_by_key(|n| n.name.to_lowercase());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_space(root: &Path, name: &str) {
        let dir = root.join(".contextstore");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.yml"),
            format!("name: {name}\ncreatedAt: '2026-05-01T12:00:00Z'\n"),
        )
        .unwrap();
    }

    #[test]
    fn build_tree_recurses_and_sorts_alphabetically() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        std::fs::create_dir_all(tmp.path().join("Beta/Sub")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Alpha")).unwrap();

        let tree = build_folder_tree(tmp.path(), None).unwrap();
        let names: Vec<&str> = tree.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Beta"]);

        let beta = &tree[1];
        let beta_children = beta.children.as_ref().unwrap();
        assert_eq!(beta_children.len(), 1);
        assert_eq!(beta_children[0].name, "Sub");
        // Alpha has no subfolders, Beta has one — the flag reflects that.
        assert!(!tree[0].has_children);
        assert!(beta.has_children);
    }

    #[test]
    fn build_tree_skips_csignore_matches_and_dotfiles() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        std::fs::create_dir_all(tmp.path().join(".hidden")).unwrap();
        std::fs::create_dir_all(tmp.path().join("node_modules")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Engineering")).unwrap();
        std::fs::write(tmp.path().join(".csignore"), "node_modules/\n").unwrap();

        let tree = build_folder_tree(tmp.path(), None).unwrap();
        let names: Vec<&str> = tree.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Engineering"]);
    }

    #[test]
    fn build_tree_respects_folder_order_at_root() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        std::fs::create_dir_all(tmp.path().join("Alpha")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Beta")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Gamma")).unwrap();

        // Override settings to put Gamma first.
        let yml = "name: Space\ncreatedAt: '2026-05-01T12:00:00Z'\nfolderOrder:\n  \"\":\n    - Gamma\n    - Alpha\n    - Beta\n";
        std::fs::write(tmp.path().join(".contextstore/settings.yml"), yml).unwrap();

        let tree = build_folder_tree(tmp.path(), None).unwrap();
        let names: Vec<&str> = tree.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Gamma", "Alpha", "Beta"]);
    }

    // MARK: - Lazy single-level builds

    #[test]
    fn build_children_is_one_level_with_accurate_expandable_flags() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        std::fs::create_dir_all(tmp.path().join("Beta/Sub/Deep")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Alpha")).unwrap();

        // Root level: only immediate children, children left unloaded.
        let root = build_children(tmp.path(), None, "").unwrap();
        let names: Vec<&str> = root.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Beta"]);
        assert!(root.iter().all(|n| n.children.is_none()));
        // Alpha is a leaf; Beta is expandable (it holds Sub).
        assert!(!root[0].has_children);
        assert!(root[1].has_children);

        // Expanding Beta lists Sub, which is itself expandable (holds Deep).
        let beta = build_children(tmp.path(), None, "Beta").unwrap();
        assert_eq!(beta.len(), 1);
        assert_eq!(beta[0].name, "Sub");
        assert_eq!(beta[0].relative_path, "Beta/Sub");
        assert!(beta[0].has_children);
        assert!(beta[0].children.is_none());

        // Expanding the deepest level finds a leaf.
        let sub = build_children(tmp.path(), None, "Beta/Sub").unwrap();
        assert_eq!(sub.len(), 1);
        assert_eq!(sub[0].name, "Deep");
        assert!(!sub[0].has_children);
    }

    #[test]
    fn build_children_honors_ignore_for_expandable_flag() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        // Engineering's only subfolder is ignored, so it must read as a leaf.
        std::fs::create_dir_all(tmp.path().join("Engineering/node_modules")).unwrap();
        std::fs::write(tmp.path().join(".csignore"), "node_modules/\n").unwrap();

        let root = build_children(tmp.path(), None, "").unwrap();
        let eng = root.iter().find(|n| n.name == "Engineering").unwrap();
        assert!(!eng.has_children);
    }

    #[test]
    fn build_children_respects_folder_order() {
        let tmp = tempfile::tempdir().unwrap();
        seed_space(tmp.path(), "Space");
        std::fs::create_dir_all(tmp.path().join("Alpha")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Beta")).unwrap();
        std::fs::create_dir_all(tmp.path().join("Gamma")).unwrap();
        let yml = "name: Space\ncreatedAt: '2026-05-01T12:00:00Z'\nfolderOrder:\n  \"\":\n    - Gamma\n    - Alpha\n    - Beta\n";
        std::fs::write(tmp.path().join(".contextstore/settings.yml"), yml).unwrap();

        let root = build_children(tmp.path(), None, "").unwrap();
        let names: Vec<&str> = root.iter().map(|n| n.name.as_str()).collect();
        assert_eq!(names, vec!["Gamma", "Alpha", "Beta"]);
    }
}
