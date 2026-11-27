//! What the Wiki tree's edits ask the hub for: a new document's or folder's
//! path, a rename's, a duplicate's, a drag's moves and folder orders, a
//! document's breadcrumb. Toolkit-free and shared by both clients: the
//! desktop's tree (`desktop/src/views/wiki_tree.rs`) uses all of it, the
//! TUI's what its keys reach. The tree itself (rows, expansion, selection,
//! filter) is [`crate::wiki_tree`].
//!
//! Paths are wiki-relative with `/` separators, `""` the root, as the hub
//! answers them ([`crate::wiki::WikiNode::path`]).

use crate::wiki::{NodeKind, WikiNode};

/// The name a new document starts with (the title a rename then replaces).
pub const NEW_DOCUMENT: &str = "Untitled";
/// The name a new folder starts with.
pub const NEW_FOLDER: &str = "New Folder";

/// The folder `path` is in (`""` for the root's children).
pub fn parent_of(path: &str) -> &str {
    path.trim_matches('/').rsplit_once('/').map_or("", |(parent, _)| parent)
}

/// The last part of `path`: its file or folder name.
pub fn name_of(path: &str) -> &str {
    let path = path.trim_matches('/');
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// `name` inside `folder`.
pub fn join(folder: &str, name: &str) -> String {
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

/// The nodes directly in `folder` (the root's when `""`), or `None` when no
/// such folder is in `nodes`.
pub fn children_of<'a>(nodes: &'a [WikiNode], folder: &str) -> Option<&'a [WikiNode]> {
    let folder = folder.trim_matches('/');
    if folder.is_empty() {
        return Some(nodes);
    }
    find(nodes, folder).filter(|n| n.kind == NodeKind::Folder).map(|n| n.children.as_slice())
}

/// The node at `path`, anywhere in `nodes`.
pub fn find<'a>(nodes: &'a [WikiNode], path: &str) -> Option<&'a WikiNode> {
    let path = path.trim_matches('/');
    for node in nodes {
        if node.path == path {
            return Some(node);
        }
        if node.kind == NodeKind::Folder && path.starts_with(&format!("{}/", node.path)) {
            return find(&node.children, path);
        }
    }
    None
}

/// The folder a new document or folder goes in when `selected` is the
/// selected row: the folder itself, a document's folder, the root with
/// nothing selected.
pub fn new_parent(nodes: &[WikiNode], selected: Option<&str>) -> String {
    match selected.and_then(|p| find(nodes, p)) {
        Some(node) if node.kind == NodeKind::Folder => node.path.clone(),
        Some(node) => parent_of(&node.path).to_string(),
        None => String::new(),
    }
}

/// A free path for a new document (`Untitled.md`, `Untitled 2.md`, …) or
/// folder (`New Folder`, `New Folder 2`, …) in `folder`, given what `nodes`
/// has there. Names compare case-insensitively, as macOS's file system does.
pub fn new_path(nodes: &[WikiNode], folder: &str, kind: NodeKind) -> String {
    let taken = taken_names(nodes, folder);
    let (stem, ext) = match kind {
        NodeKind::Document => (NEW_DOCUMENT, ".md"),
        NodeKind::Folder => (NEW_FOLDER, ""),
    };
    join(folder, &free_name(&taken, stem, ext))
}

/// A free path for a copy of `path` beside it: `plan copy.md`, then
/// `plan copy 2.md`, … (a folder's `Plans copy`, …), as Finder names one.
pub fn duplicate_path(nodes: &[WikiNode], path: &str) -> String {
    let folder = parent_of(path);
    let taken = taken_names(nodes, folder);
    let (stem, ext) = split_ext(name_of(path));
    join(folder, &free_name(&taken, &format!("{stem} copy"), ext))
}

fn taken_names(nodes: &[WikiNode], folder: &str) -> Vec<String> {
    children_of(nodes, folder)
        .unwrap_or_default()
        .iter()
        .map(|n| n.name.to_lowercase())
        .collect()
}

fn free_name(taken: &[String], stem: &str, ext: &str) -> String {
    (1..)
        .map(|n| if n == 1 { format!("{stem}{ext}") } else { format!("{stem} {n}{ext}") })
        .find(|name| !taken.contains(&name.to_lowercase()))
        .unwrap_or_default()
}

/// `name` split before its `.md` (a folder's whole name, `""`).
fn split_ext(name: &str) -> (&str, &str) {
    match name.len().checked_sub(3).filter(|&at| name[at..].eq_ignore_ascii_case(".md")) {
        Some(at) if at > 0 => (&name[..at], &name[at..]),
        _ => (name, ""),
    }
}

/// What a rename starts with in its field: a document's file name without
/// `.md` (the extension isn't the user's to type), a folder's name.
pub fn rename_draft(node: &WikiNode) -> String {
    match node.kind {
        NodeKind::Document => split_ext(&node.name).0.to_string(),
        NodeKind::Folder => node.name.clone(),
    }
}

/// The path renaming `node` to `typed` moves it to, or why it can't:
/// `None` when nothing changes. A document keeps (or gets) its `.md`.
pub fn rename_target(node: &WikiNode, typed: &str) -> Result<Option<String>, String> {
    let typed = typed.trim();
    if typed.is_empty() {
        return Err("A name can't be empty.".into());
    }
    if typed.contains('/') || typed.contains('\\') {
        return Err("A name can't contain a slash.".into());
    }
    if typed == "." || typed == ".." || typed.starts_with('.') {
        return Err("A name can't start with a dot.".into());
    }
    let name = match node.kind {
        NodeKind::Document if split_ext(typed).1.is_empty() => format!("{typed}.md"),
        _ => typed.to_string(),
    };
    if name == node.name {
        return Ok(None);
    }
    Ok(Some(join(parent_of(&node.path), &name)))
}

/// Where a drop lands relative to the row under the pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropAt {
    /// Into the folder under the pointer (or, on a document, into its
    /// folder).
    Into,
    /// Above the row: among its siblings, before it.
    Before,
    /// Below the row: among its siblings, after it.
    After,
}

/// A drop's requests, in order: a move (when it changes folders), then an
/// order for the folder it lands in (a folder dropped before or after a
/// sibling folder). Both empty: the drop changes nothing.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DropPlan {
    pub moves: Option<(String, String)>,
    pub order: Option<(String, Vec<String>)>,
}

impl DropPlan {
    pub fn is_empty(&self) -> bool {
        self.moves.is_none() && self.order.is_none()
    }
}

/// The plan for dropping `dragged` on `target` at `at`, or why it can't
/// land there (into itself, a name taken in the folder it lands in).
/// Documents have no order of their own (they sort pinned, then by title),
/// so a document dropped before or after a row only changes folders.
pub fn drop_plan(nodes: &[WikiNode], dragged: &str, target: &str, at: DropAt) -> Result<DropPlan, String> {
    let dragged_node = find(nodes, dragged).ok_or("It's no longer in the wiki.")?;
    let target_node = find(nodes, target).ok_or("Its destination is no longer in the wiki.")?;
    let folder = match (at, target_node.kind) {
        (DropAt::Into, NodeKind::Folder) => target_node.path.clone(),
        _ => parent_of(&target_node.path).to_string(),
    };
    if dragged_node.kind == NodeKind::Folder
        && (folder == dragged_node.path || folder.starts_with(&format!("{}/", dragged_node.path)))
    {
        return Err("A folder can't go inside itself.".into());
    }
    let name = name_of(dragged).to_string();
    let mut plan = DropPlan::default();
    if parent_of(dragged) != folder {
        let taken = taken_names(nodes, &folder);
        if taken.contains(&name.to_lowercase()) {
            return Err(format!("“{name}” is already there."));
        }
        plan.moves = Some((dragged.to_string(), join(&folder, &name)));
    }
    let reorders = dragged_node.kind == NodeKind::Folder
        && target_node.kind == NodeKind::Folder
        && matches!(at, DropAt::Before | DropAt::After)
        && target_node.path != dragged_node.path;
    if reorders {
        let mut order: Vec<String> = children_of(nodes, &folder)
            .unwrap_or_default()
            .iter()
            .filter(|n| n.kind == NodeKind::Folder && n.name != name)
            .map(|n| n.name.clone())
            .collect();
        let at_target = order.iter().position(|n| *n == target_node.name).unwrap_or(order.len());
        let index = if at == DropAt::After { at_target + 1 } else { at_target };
        order.insert(index.min(order.len()), name);
        let current: Vec<String> = children_of(nodes, &folder)
            .unwrap_or_default()
            .iter()
            .filter(|n| n.kind == NodeKind::Folder)
            .map(|n| n.name.clone())
            .collect();
        if plan.moves.is_some() || order != current {
            plan.order = Some((folder, order));
        }
    }
    Ok(plan)
}

/// The titles of the folders `path` is in, then its own: what the
/// document view's breadcrumb shows (`Plans › OpenCode integration`). A
/// path the tree doesn't have falls back to its parts.
pub fn breadcrumb(nodes: &[WikiNode], path: &str) -> Vec<String> {
    let parts: Vec<&str> = path.trim_matches('/').split('/').filter(|p| !p.is_empty()).collect();
    (1..=parts.len())
        .map(|n| {
            let at = parts[..n].join("/");
            find(nodes, &at).map_or_else(|| split_ext(parts[n - 1]).0.to_string(), |node| node.title.clone())
        })
        .collect()
}

/// The paths of `nodes`' folders that hold documents, in tree order: a
/// fresh tree's expansion (its first level open, so a first visit shows
/// what's there).
pub fn first_level_folders(nodes: &[WikiNode]) -> Vec<String> {
    nodes.iter().filter(|n| n.kind == NodeKind::Folder).map(|n| n.path.clone()).collect()
}

/// `path` after a move of `from` to `to`: itself when it's elsewhere, its
/// new place when it is `from` or inside it. Keeps a selection and an
/// expansion on what moved.
pub fn moved(path: &str, from: &str, to: &str) -> String {
    if path == from {
        to.to_string()
    } else if let Some(rest) = path.strip_prefix(&format!("{from}/")) {
        format!("{to}/{rest}")
    } else {
        path.to_string()
    }
}

/// How many documents `nodes` hold, folders' included.
pub fn document_count(nodes: &[WikiNode]) -> usize {
    nodes
        .iter()
        .map(|n| match n.kind {
            NodeKind::Document => 1,
            NodeKind::Folder => document_count(&n.children),
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str, title: &str) -> WikiNode {
        WikiNode {
            path: path.into(),
            name: name_of(path).into(),
            title: title.into(),
            kind: NodeKind::Document,
            icon: None,
            children: Vec::new(),
        }
    }

    fn folder(path: &str, children: Vec<WikiNode>) -> WikiNode {
        WikiNode {
            path: path.into(),
            name: name_of(path).into(),
            title: name_of(path).into(),
            kind: NodeKind::Folder,
            icon: None,
            children,
        }
    }

    fn wiki() -> Vec<WikiNode> {
        vec![
            folder(
                "Plans",
                vec![
                    folder("Plans/Archive", vec![doc("Plans/Archive/old.md", "Old")]),
                    folder("Plans/Drafts", vec![]),
                    doc("Plans/awareness-v2.md", "Awareness v2"),
                    doc("Plans/Untitled.md", "Untitled"),
                ],
            ),
            folder("Research", vec![]),
            doc("README.md", "Read me"),
        ]
    }

    #[test]
    fn paths_split_into_folder_and_name() {
        assert_eq!(parent_of("Plans/Archive/old.md"), "Plans/Archive");
        assert_eq!(parent_of("README.md"), "");
        assert_eq!(name_of("Plans/Archive"), "Archive");
        assert_eq!(join("", "a.md"), "a.md");
        assert_eq!(join("Plans", "a.md"), "Plans/a.md");
        assert_eq!(find(&wiki(), "Plans/Archive/old.md").unwrap().title, "Old");
        assert!(find(&wiki(), "Plans/nope.md").is_none());
    }

    #[test]
    fn a_new_item_goes_in_the_selected_folder_under_a_free_name() {
        let w = wiki();
        assert_eq!(new_parent(&w, Some("Plans")), "Plans");
        assert_eq!(new_parent(&w, Some("Plans/awareness-v2.md")), "Plans");
        assert_eq!(new_parent(&w, None), "");
        assert_eq!(new_path(&w, "Plans", NodeKind::Document), "Plans/Untitled 2.md");
        assert_eq!(new_path(&w, "Research", NodeKind::Document), "Research/Untitled.md");
        assert_eq!(new_path(&w, "", NodeKind::Folder), "New Folder");
    }

    #[test]
    fn a_duplicate_is_named_as_finder_names_a_copy() {
        let mut w = wiki();
        assert_eq!(duplicate_path(&w, "Plans/awareness-v2.md"), "Plans/awareness-v2 copy.md");
        w[0].children.push(doc("Plans/awareness-v2 copy.md", "Awareness v2"));
        assert_eq!(duplicate_path(&w, "Plans/awareness-v2.md"), "Plans/awareness-v2 copy 2.md");
        assert_eq!(duplicate_path(&w, "Research"), "Research copy");
    }

    #[test]
    fn a_rename_keeps_a_documents_extension_and_refuses_bad_names() {
        let d = doc("Plans/awareness-v2.md", "Awareness v2");
        assert_eq!(rename_draft(&d), "awareness-v2");
        assert_eq!(rename_target(&d, "awareness").unwrap(), Some("Plans/awareness.md".into()));
        assert_eq!(rename_target(&d, "awareness-v2").unwrap(), None);
        assert_eq!(rename_target(&d, "notes.MD").unwrap(), Some("Plans/notes.MD".into()));
        assert!(rename_target(&d, " ").is_err());
        assert!(rename_target(&d, "a/b").is_err());
        assert!(rename_target(&d, ".hidden").is_err());
        let f = folder("Plans/Archive", vec![]);
        assert_eq!(rename_draft(&f), "Archive");
        assert_eq!(rename_target(&f, "Old plans").unwrap(), Some("Plans/Old plans".into()));
    }

    #[test]
    fn dropping_into_a_folder_moves_and_before_a_folder_also_orders() {
        let w = wiki();
        let plan = drop_plan(&w, "README.md", "Plans", DropAt::Into).unwrap();
        assert_eq!(plan.moves, Some(("README.md".into(), "Plans/README.md".into())));
        assert_eq!(plan.order, None);
        // A document dropped on a document lands in its folder.
        let plan = drop_plan(&w, "README.md", "Plans/awareness-v2.md", DropAt::Before).unwrap();
        assert_eq!(plan.moves, Some(("README.md".into(), "Plans/README.md".into())));
        // Folders reorder among their siblings.
        let plan = drop_plan(&w, "Plans/Drafts", "Plans/Archive", DropAt::Before).unwrap();
        assert_eq!(plan.moves, None);
        assert_eq!(plan.order, Some(("Plans".into(), vec!["Drafts".into(), "Archive".into()])));
        // Already there: nothing to do.
        assert!(drop_plan(&w, "Plans/Drafts", "Plans/Archive", DropAt::After).unwrap().is_empty());
        // From another folder: a move, then the order.
        let plan = drop_plan(&w, "Research", "Plans/Archive", DropAt::After).unwrap();
        assert_eq!(plan.moves, Some(("Research".into(), "Plans/Research".into())));
        assert_eq!(plan.order, Some(("Plans".into(), vec!["Archive".into(), "Research".into(), "Drafts".into()])));
    }

    #[test]
    fn a_drop_into_itself_or_onto_a_taken_name_is_refused() {
        let w = wiki();
        assert!(drop_plan(&w, "Plans", "Plans/Archive", DropAt::Into).is_err());
        assert!(drop_plan(&w, "Plans", "Plans", DropAt::Into).is_err());
        let mut w = w;
        w[1].children.push(doc("Research/README.md", "Other"));
        assert!(drop_plan(&w, "README.md", "Research", DropAt::Into).is_err());
    }

    #[test]
    fn a_breadcrumb_reads_titles_and_falls_back_to_names() {
        let w = wiki();
        assert_eq!(breadcrumb(&w, "Plans/Archive/old.md"), vec!["Plans", "Archive", "Old"]);
        assert_eq!(breadcrumb(&w, "Gone/thing.md"), vec!["Gone", "thing"]);
    }

    #[test]
    fn a_move_carries_paths_inside_what_moved() {
        assert_eq!(moved("Plans/Archive/old.md", "Plans", "Docs/Plans"), "Docs/Plans/Archive/old.md");
        assert_eq!(moved("Plans", "Plans", "Docs/Plans"), "Docs/Plans");
        assert_eq!(moved("Plansx/a.md", "Plans", "Docs"), "Plansx/a.md");
        assert_eq!(document_count(&wiki()), 4);
        assert_eq!(first_level_folders(&wiki()), vec!["Plans", "Research"]);
    }
}
