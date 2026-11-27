//! The Wiki sidebar's tree: which folders are open, which row is selected,
//! and what the filter keeps, over the hub's [`WikiNode`] tree.
//!
//! Toolkit-free, so the TUI and the desktop app walk the same tree with the
//! same rules (VS Code's): → opens a folder, or steps into an open one; ←
//! closes an open folder, or steps out to the parent. The selection is a
//! path, so a refresh (a `wiki_changed` event) that reorders or adds rows
//! keeps the user on the same document; a removed one falls back to the
//! row that took its place.
//!
//! The filter matches a document's title or file name, case-insensitively.
//! A match's folders stay shown, open, so it's reachable; a folder whose own
//! name matches shows too.

use std::collections::BTreeSet;

use crate::wiki::{NodeKind, WikiNode};

/// One visible row of the tree, top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Wiki-relative (`Plans/a.md`, `Plans`).
    pub path: String,
    /// What the row shows: a document's title, a folder's name.
    pub title: String,
    /// The file or folder name on disk (a tooltip, the rename draft).
    pub name: String,
    pub kind: NodeKind,
    /// 0 for the wiki's top level.
    pub depth: usize,
    /// A folder shown open (also when the filter opens it).
    pub expanded: bool,
    /// A folder with anything in it.
    pub has_children: bool,
}

impl TreeRow {
    pub fn is_folder(&self) -> bool {
        self.kind == NodeKind::Folder
    }
}

/// The tree's state. See the module docs.
#[derive(Debug, Clone, Default)]
pub struct WikiTree {
    nodes: Vec<WikiNode>,
    /// Folders the user opened, by path.
    expanded: BTreeSet<String>,
    /// The selected row's path.
    selected: Option<String>,
    filter: String,
    /// Whether the hub's tree has been read in yet.
    loaded: bool,
}

impl WikiTree {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a tree has been read in ([`Self::set_nodes`]) yet.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    pub fn nodes(&self) -> &[WikiNode] {
        &self.nodes
    }

    /// Take a freshly read tree. The selection stays on its path when it's
    /// still there, else on the row now where it was; open folders stay
    /// open.
    pub fn set_nodes(&mut self, nodes: Vec<WikiNode>) {
        let old_index = self.selected_index();
        self.nodes = nodes;
        self.loaded = true;
        let rows = self.rows();
        let still_there = self
            .selected
            .as_ref()
            .is_some_and(|p| rows.iter().any(|r| &r.path == p));
        if !still_there {
            self.selected = match old_index {
                Some(i) if !rows.is_empty() => Some(rows[i.min(rows.len() - 1)].path.clone()),
                _ => rows.first().map(|r| r.path.clone()),
            };
        }
    }

    /// The visible rows, top to bottom.
    pub fn rows(&self) -> Vec<TreeRow> {
        let mut out = Vec::new();
        let filter = self.filter.trim().to_lowercase();
        push_rows(&self.nodes, 0, &self.expanded, &filter, &mut out);
        out
    }

    /// The selected row's path.
    pub fn selected_path(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// The selected row's index in [`Self::rows`].
    pub fn selected_index(&self) -> Option<usize> {
        let path = self.selected.as_deref()?;
        self.rows().iter().position(|r| r.path == path)
    }

    /// The selected row.
    pub fn selected_row(&self) -> Option<TreeRow> {
        let path = self.selected.as_deref()?;
        self.rows().into_iter().find(|r| r.path == path)
    }

    /// The node at `path`, anywhere in the tree.
    pub fn node(&self, path: &str) -> Option<&WikiNode> {
        find(&self.nodes, path)
    }

    /// Select the row at `index` of [`Self::rows`] (clamped).
    pub fn select_index(&mut self, index: usize) {
        let rows = self.rows();
        if let Some(row) = rows.get(index.min(rows.len().saturating_sub(1))) {
            self.selected = Some(row.path.clone());
        }
    }

    /// Select `path`, opening its folders so it shows. `false` when the tree
    /// has no such row (the selection then stays where it was).
    pub fn select_path(&mut self, path: &str) -> bool {
        if self.node(path).is_none() {
            return false;
        }
        for folder in ancestors(path) {
            self.expanded.insert(folder.to_string());
        }
        self.selected = Some(path.to_string());
        true
    }

    pub fn move_up(&mut self) {
        match self.selected_index() {
            Some(i) => self.select_index(i.saturating_sub(1)),
            None => self.select_index(0),
        }
    }

    pub fn move_down(&mut self) {
        match self.selected_index() {
            Some(i) => self.select_index(i + 1),
            None => self.select_index(0),
        }
    }

    pub fn move_to_first(&mut self) {
        self.select_index(0);
    }

    pub fn move_to_last(&mut self) {
        self.select_index(usize::MAX);
    }

    /// → : open the selected folder, or step into it when it's open. A
    /// document stays as it is.
    pub fn expand(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if !row.is_folder() {
            return;
        }
        if !row.expanded {
            self.expanded.insert(row.path);
        } else if row.has_children {
            self.move_down();
        }
    }

    /// ← : close the selected folder when it's open, else step out to its
    /// folder.
    pub fn collapse(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_folder() && row.expanded && self.expanded.contains(&row.path) {
            self.expanded.remove(&row.path);
            return;
        }
        if let Some(parent) = parent_of(&row.path) {
            self.selected = Some(parent.to_string());
        }
    }

    /// Open the selected folder if it's closed, close it if it's open.
    pub fn toggle(&mut self) {
        let Some(row) = self.selected_row() else {
            return;
        };
        if row.is_folder() && !self.expanded.remove(&row.path) {
            self.expanded.insert(row.path);
        }
    }

    /// Whether folder `path` is open.
    pub fn is_expanded(&self, path: &str) -> bool {
        self.expanded.contains(path)
    }

    /// The open folders, for a client to keep across visits.
    pub fn expanded(&self) -> impl Iterator<Item = &str> {
        self.expanded.iter().map(String::as_str)
    }

    /// Open `paths` (a client's kept expansion state).
    pub fn set_expanded<I: IntoIterator<Item = String>>(&mut self, paths: I) {
        self.expanded = paths.into_iter().collect();
    }

    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Filter the rows. The selection moves to the first row that shows
    /// when it's filtered out; clearing the filter keeps it where it was.
    pub fn set_filter(&mut self, filter: impl Into<String>) {
        self.filter = filter.into();
        // A row the filter showed stays shown when it's cleared: its folders
        // open, as the filter had them.
        if self.filter.trim().is_empty() {
            if let Some(path) = self.selected.clone() {
                if self.node(&path).is_some() {
                    for folder in ancestors(&path) {
                        self.expanded.insert(folder.to_string());
                    }
                }
            }
        }
        if self.selected_index().is_none() {
            self.select_index(0);
        }
    }

    /// The folder a new document or folder goes in: the selected folder,
    /// the selected document's folder, or the top level (`""`).
    pub fn folder_for_new(&self) -> String {
        match self.selected_row() {
            Some(row) if row.is_folder() => row.path,
            Some(row) => parent_of(&row.path).unwrap_or_default().to_string(),
            None => String::new(),
        }
    }
}

/// `path`'s folder, or `None` at the top level.
pub fn parent_of(path: &str) -> Option<&str> {
    path.rsplit_once('/').map(|(parent, _)| parent)
}

/// `path`'s folders, outermost first (`a/b/c.md` → `a`, `a/b`).
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    path.match_indices('/').map(move |(i, _)| &path[..i])
}

/// Join a folder (`""` for the top level) and a name.
pub fn join(folder: &str, name: &str) -> String {
    if folder.is_empty() {
        name.to_string()
    } else {
        format!("{folder}/{name}")
    }
}

fn find<'a>(nodes: &'a [WikiNode], path: &str) -> Option<&'a WikiNode> {
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

fn matches(node: &WikiNode, filter: &str) -> bool {
    node.title.to_lowercase().contains(filter) || node.name.to_lowercase().contains(filter)
}

/// Whether `node` or anything under it matches.
fn shows(node: &WikiNode, filter: &str) -> bool {
    matches(node, filter) || node.children.iter().any(|c| shows(c, filter))
}

fn push_rows(nodes: &[WikiNode], depth: usize, expanded: &BTreeSet<String>, filter: &str, out: &mut Vec<TreeRow>) {
    for node in nodes {
        if !filter.is_empty() && !shows(node, filter) {
            continue;
        }
        let folder = node.kind == NodeKind::Folder;
        // Filtering, a folder opens to show the matches under it.
        let open = folder
            && if filter.is_empty() {
                expanded.contains(&node.path)
            } else {
                node.children.iter().any(|c| shows(c, filter))
            };
        out.push(TreeRow {
            path: node.path.clone(),
            title: node.title.clone(),
            name: node.name.clone(),
            kind: node.kind,
            depth,
            expanded: open,
            has_children: !node.children.is_empty(),
        });
        if open {
            push_rows(&node.children, depth + 1, expanded, filter, out);
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn doc(path: &str, title: &str) -> WikiNode {
        WikiNode {
            path: path.into(),
            name: path.rsplit('/').next().unwrap().into(),
            title: title.into(),
            kind: NodeKind::Document,
            children: Vec::new(),
        }
    }

    pub(crate) fn folder(path: &str, children: Vec<WikiNode>) -> WikiNode {
        let name: String = path.rsplit('/').next().unwrap().into();
        WikiNode {
            path: path.into(),
            title: name.clone(),
            name,
            kind: NodeKind::Folder,
            children,
        }
    }

    /// Plans/{Archive/{old}, awareness, opencode}, Research/{}, README.
    pub(crate) fn sample() -> Vec<WikiNode> {
        vec![
            folder(
                "Plans",
                vec![
                    folder("Plans/Archive", vec![doc("Plans/Archive/old.md", "Old plan")]),
                    doc("Plans/awareness.md", "Awareness v2"),
                    doc("Plans/opencode.md", "OpenCode integration"),
                ],
            ),
            folder("Research", vec![]),
            doc("README.md", "Shelbi docs"),
        ]
    }

    fn titles(t: &WikiTree) -> Vec<String> {
        t.rows().iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.title)).collect()
    }

    fn tree() -> WikiTree {
        let mut t = WikiTree::new();
        t.set_nodes(sample());
        t
    }

    #[test]
    fn folders_start_closed_and_the_first_row_is_selected() {
        let t = tree();
        assert!(t.is_loaded());
        assert_eq!(titles(&t), ["Plans", "Research", "Shelbi docs"]);
        assert_eq!(t.selected_path(), Some("Plans"));
    }

    #[test]
    fn right_opens_then_steps_in_and_left_closes_then_steps_out() {
        let mut t = tree();
        t.expand();
        assert_eq!(titles(&t), ["Plans", "  Archive", "  Awareness v2", "  OpenCode integration", "Research", "Shelbi docs"]);
        assert_eq!(t.selected_path(), Some("Plans"));
        t.expand();
        assert_eq!(t.selected_path(), Some("Plans/Archive"));
        t.move_down();
        assert_eq!(t.selected_path(), Some("Plans/awareness.md"));
        // ← on a document steps out to its folder; again closes it.
        t.collapse();
        assert_eq!(t.selected_path(), Some("Plans"));
        t.collapse();
        assert_eq!(titles(&t), ["Plans", "Research", "Shelbi docs"]);
        // An empty folder opens but has nothing to step into.
        t.select_path("Research");
        t.expand();
        t.expand();
        assert_eq!(t.selected_path(), Some("Research"));
        assert!(t.is_expanded("Research"));
    }

    #[test]
    fn up_and_down_stop_at_the_ends() {
        let mut t = tree();
        t.move_up();
        assert_eq!(t.selected_path(), Some("Plans"));
        t.move_to_last();
        assert_eq!(t.selected_path(), Some("README.md"));
        t.move_down();
        assert_eq!(t.selected_path(), Some("README.md"));
        t.move_to_first();
        assert_eq!(t.selected_index(), Some(0));
    }

    #[test]
    fn selecting_a_path_opens_its_folders() {
        let mut t = tree();
        assert!(t.select_path("Plans/Archive/old.md"));
        assert!(t.is_expanded("Plans") && t.is_expanded("Plans/Archive"));
        assert_eq!(t.selected_row().unwrap().depth, 2);
        assert!(!t.select_path("nope.md"));
        assert_eq!(t.selected_path(), Some("Plans/Archive/old.md"));
    }

    #[test]
    fn a_refresh_keeps_the_selection_or_takes_the_row_in_its_place() {
        let mut t = tree();
        t.select_path("Plans/opencode.md");
        let mut nodes = sample();
        nodes.push(doc("zeta.md", "Zeta"));
        t.set_nodes(nodes);
        assert_eq!(t.selected_path(), Some("Plans/opencode.md"));
        // Removed: the row now at its index.
        let mut nodes = sample();
        if let Some(plans) = nodes.first_mut() {
            plans.children.retain(|c| c.path != "Plans/opencode.md");
        }
        t.set_nodes(nodes);
        assert_eq!(t.selected_path(), Some("Research"));
        // Emptied: nothing to select.
        t.set_nodes(Vec::new());
        assert_eq!(t.selected_path(), None);
    }

    #[test]
    fn the_filter_keeps_matches_and_their_folders_open() {
        let mut t = tree();
        t.set_filter("OLD");
        assert_eq!(titles(&t), ["Plans", "  Archive", "    Old plan"]);
        // The selection (Plans) still shows.
        assert_eq!(t.selected_path(), Some("Plans"));
        // File names match too.
        t.set_filter("opencode.md");
        assert_eq!(titles(&t), ["Plans", "  OpenCode integration"]);
        t.set_filter("research");
        assert_eq!(titles(&t), ["Research"]);
        assert_eq!(t.selected_path(), Some("Research"));
        t.set_filter("");
        assert_eq!(titles(&t), ["Plans", "Research", "Shelbi docs"]);
        // A match selected while filtering stays selected, its folders open.
        t.set_filter("old");
        t.select_path("Plans/Archive/old.md");
        t.set_filter("");
        assert_eq!(t.selected_path(), Some("Plans/Archive/old.md"));
        assert!(t.is_expanded("Plans") && t.is_expanded("Plans/Archive"));
    }

    #[test]
    fn new_entries_go_in_the_selected_folder_or_the_documents_folder() {
        let mut t = tree();
        assert_eq!(t.folder_for_new(), "Plans");
        t.select_path("Plans/Archive/old.md");
        assert_eq!(t.folder_for_new(), "Plans/Archive");
        t.select_path("README.md");
        assert_eq!(t.folder_for_new(), "");
        assert_eq!(join("", "a.md"), "a.md");
        assert_eq!(join("Plans", "a.md"), "Plans/a.md");
        assert_eq!(parent_of("Plans/a.md"), Some("Plans"));
        assert_eq!(parent_of("a.md"), None);
    }

    #[test]
    fn toggle_opens_and_closes_a_folder() {
        let mut t = tree();
        t.toggle();
        assert!(t.is_expanded("Plans"));
        t.toggle();
        assert!(!t.is_expanded("Plans"));
        t.set_expanded(["Plans/Archive".to_string(), "Plans".to_string()]);
        assert_eq!(t.expanded().collect::<Vec<_>>(), ["Plans", "Plans/Archive"]);
        assert_eq!(t.rows().len(), 7);
    }
}
