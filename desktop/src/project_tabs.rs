//! A window's project tabs (`project-tabs`): one per open project, in the
//! order they were opened, saved with the window's state so the order
//! survives a restart.
//!
//! The toolkit-free part: which projects have tabs and in what order. The
//! tab bar (`crate::views::tab_bar`) draws them and the window
//! (`crate::app`) acts on them.
//!
//! The open set is `shelbi_state::list_open_projects` (as of the catalog's
//! last refresh), which knows nothing of order: a tab already in the saved
//! order keeps its place, and a project newly found open goes at the end,
//! unless another window already has a tab for it (each window keeps its own
//! tabs: one torn off into a new window, or dragged to another, stays there).
//! The window's own project always has a tab, open on disk yet or not
//! (bringing it up is still on its way, or it waits on onboarding).
//!
//! Dragging a tab moves it ([`ProjectTabs::move_to`]), or into another
//! window ([`ProjectTabs::insert_at`] there); the order is saved the same
//! way, so ⌘1 to ⌘9 and the next launch follow it.

/// The tabs, in order. Project ids (`projects/<name>.yaml` stems, or a remote
/// window's far project names).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectTabs {
    order: Vec<String>,
}

impl ProjectTabs {
    /// Tabs in a saved `order` (duplicates and empty names dropped).
    pub fn new(order: impl IntoIterator<Item = String>) -> ProjectTabs {
        let mut tabs = ProjectTabs::default();
        for name in order {
            tabs.insert(&name);
        }
        tabs
    }

    /// The tabs' projects, in order.
    pub fn order(&self) -> &[String] {
        &self.order
    }

    /// The `n`th tab's project, counting from 1 (the ⌘N it answers to).
    pub fn nth(&self, n: usize) -> Option<&str> {
        n.checked_sub(1).and_then(|i| self.order.get(i)).map(String::as_str)
    }

    pub fn contains(&self, project: &str) -> bool {
        self.order.iter().any(|p| p == project)
    }

    /// Give `project` a tab, at the end, unless it has one. Whether the tabs
    /// changed.
    pub fn insert(&mut self, project: &str) -> bool {
        if project.is_empty() || self.contains(project) {
            return false;
        }
        self.order.push(project.to_string());
        true
    }

    /// Give `project` a tab at `index` (past the end means last), unless it
    /// has one: a tab dropped in from another window. Whether the tabs
    /// changed.
    pub fn insert_at(&mut self, project: &str, index: usize) -> bool {
        if project.is_empty() || self.contains(project) {
            return false;
        }
        self.order.insert(index.min(self.order.len()), project.to_string());
        true
    }

    /// Take `project`'s tab away. Whether it had one.
    pub fn remove(&mut self, project: &str) -> bool {
        let before = self.order.len();
        self.order.retain(|p| p != project);
        self.order.len() != before
    }

    /// Move `project`'s tab to `index` in the order (past the end means
    /// last), the other tabs keeping their relative order. Whether the tabs
    /// changed: not when `project` has no tab or is there already.
    pub fn move_to(&mut self, project: &str, index: usize) -> bool {
        let Some(from) = self.order.iter().position(|p| p == project) else {
            return false;
        };
        let to = index.min(self.order.len() - 1);
        if to == from {
            return false;
        }
        let tab = self.order.remove(from);
        self.order.insert(to, tab);
        true
    }

    /// Where `project`'s tab lands when dropped in gap `slot` (0 before the
    /// first tab, `order().len()` after the last), as a [`Self::move_to`]
    /// index. `None` when the drop would leave it where it is (the gaps on
    /// either side of it), or it has no tab.
    pub fn landing(&self, project: &str, slot: usize) -> Option<usize> {
        let from = self.order.iter().position(|p| p == project)?;
        let slot = slot.min(self.order.len());
        let to = if slot > from { slot - 1 } else { slot };
        (to != from).then_some(to)
    }

    /// The tab that takes over when `project`'s closes: its right neighbor,
    /// or its left one when it was last. `None` when it is the only tab, or
    /// has none.
    pub fn neighbor(&self, project: &str) -> Option<&str> {
        let at = self.order.iter().position(|p| p == project)?;
        self.order
            .get(at + 1)
            .or_else(|| at.checked_sub(1).and_then(|i| self.order.get(i)))
            .map(String::as_str)
    }

    /// Follow the open set: a tab whose project is no longer `open` goes,
    /// a project newly open gets a tab at the end (in `open`'s order) unless
    /// another window has one for it (`elsewhere`: a tab torn off into its
    /// own window, or moved to another, stays where it went), and
    /// `current` (the window's own project, `None` on the chooser) keeps or
    /// gets one either way. With `open` unknown (`None`: a remote window,
    /// whose host's listing doesn't say what is open, or a catalog not yet
    /// read) nothing goes, and only `current` is added. Whether the tabs
    /// changed.
    pub fn reconcile(&mut self, open: Option<&[String]>, current: Option<&str>, elsewhere: &[String]) -> bool {
        let before = self.order.clone();
        if let Some(open) = open {
            self.order
                .retain(|p| current == Some(p.as_str()) || open.iter().any(|o| o == p));
            for project in open.iter().filter(|p| !elsewhere.contains(p)) {
                self.insert(project);
            }
        }
        if let Some(current) = current {
            self.insert(current);
        }
        self.order != before
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tabs(names: &[&str]) -> ProjectTabs {
        ProjectTabs::new(names.iter().map(|n| n.to_string()))
    }

    fn open(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    #[test]
    fn a_saved_order_loads_without_duplicates() {
        let t = ProjectTabs::new(["web", "", "api", "web"].map(String::from));
        assert_eq!(t.order(), ["web", "api"]);
    }

    #[test]
    fn nth_counts_from_one() {
        let t = tabs(&["web", "api", "docs"]);
        assert_eq!(t.nth(1), Some("web"));
        assert_eq!(t.nth(3), Some("docs"));
        assert_eq!(t.nth(4), None);
        assert_eq!(t.nth(0), None);
    }

    #[test]
    fn a_new_project_goes_at_the_end() {
        let mut t = tabs(&["web", "api"]);
        assert!(t.insert("docs"));
        assert!(!t.insert("web"), "already there");
        assert_eq!(t.order(), ["web", "api", "docs"]);
    }

    #[test]
    fn closing_selects_the_right_neighbor_else_the_left() {
        let t = tabs(&["web", "api", "docs"]);
        assert_eq!(t.neighbor("web"), Some("api"));
        assert_eq!(t.neighbor("api"), Some("docs"));
        assert_eq!(t.neighbor("docs"), Some("api"), "the last tab falls back left");
        assert_eq!(tabs(&["web"]).neighbor("web"), None, "the only tab");
        assert_eq!(t.neighbor("gone"), None);
    }

    #[test]
    fn remove_drops_the_tab() {
        let mut t = tabs(&["web", "api"]);
        assert!(t.remove("web"));
        assert!(!t.remove("web"));
        assert_eq!(t.order(), ["api"]);
    }

    #[test]
    fn move_to_places_a_tab_and_keeps_the_others_in_order() {
        let mut t = tabs(&["web", "api", "docs", "site"]);
        assert!(t.move_to("docs", 0), "to the start");
        assert_eq!(t.order(), ["docs", "web", "api", "site"]);
        assert!(t.move_to("docs", 3), "to the end");
        assert_eq!(t.order(), ["web", "api", "site", "docs"]);
        assert!(t.move_to("web", 99), "past the end is last");
        assert_eq!(t.order(), ["api", "site", "docs", "web"]);
        assert!(t.move_to("web", 1), "into the middle");
        assert_eq!(t.order(), ["api", "web", "site", "docs"]);
        assert!(!t.move_to("web", 1), "its own position");
        assert!(!t.move_to("docs", 9), "last already");
        assert!(!t.move_to("gone", 0), "an unknown project");
        assert_eq!(t.order(), ["api", "web", "site", "docs"]);
        assert!(!ProjectTabs::default().move_to("web", 0), "no tabs");
    }

    #[test]
    fn a_drop_gap_lands_before_the_tab_after_it() {
        let t = tabs(&["web", "api", "docs"]);
        // Gaps: 0 |web| 1 |api| 2 |docs| 3.
        assert_eq!(t.landing("docs", 0), Some(0), "before the first");
        assert_eq!(t.landing("docs", 1), Some(1), "between web and api");
        assert_eq!(t.landing("web", 2), Some(1), "past api, which steps left");
        assert_eq!(t.landing("web", 3), Some(2), "after the last");
        assert_eq!(t.landing("web", 9), Some(2), "past the end is the end");
        assert_eq!(t.landing("api", 1), None, "its own left gap");
        assert_eq!(t.landing("api", 2), None, "its own right gap");
        assert_eq!(t.landing("gone", 0), None);
        let mut moved = t.clone();
        assert!(moved.move_to("web", t.landing("web", 2).unwrap()));
        assert_eq!(moved.order(), ["api", "web", "docs"]);
    }

    #[test]
    fn reconcile_keeps_the_saved_order_and_appends_new_ones() {
        // `list_open_projects` is sorted by name; the tabs keep their own order.
        let mut t = tabs(&["web", "api"]);
        assert!(t.reconcile(Some(&open(&["api", "docs", "web"])), Some("web"), &[]));
        assert_eq!(t.order(), ["web", "api", "docs"]);
        assert!(!t.reconcile(Some(&open(&["api", "docs", "web"])), Some("web"), &[]), "settled");
    }

    #[test]
    fn reconcile_drops_closed_projects_but_never_the_current_one() {
        let mut t = tabs(&["web", "api", "docs"]);
        // `docs` was quit elsewhere; `web` (this window's) isn't recorded open
        // yet (its orchestrator is still coming up).
        assert!(t.reconcile(Some(&open(&["api"])), Some("web"), &[]));
        assert_eq!(t.order(), ["web", "api"]);
        // The chooser has no current project.
        assert!(t.reconcile(Some(&open(&[])), None, &[]));
        assert!(t.order().is_empty());
    }

    #[test]
    fn a_project_with_a_tab_in_another_window_gets_none_here() {
        // `docs` was torn off into its own window: a refresh doesn't bring
        // its tab back here, and a project newly open goes to one window.
        let mut t = tabs(&["web", "api"]);
        let elsewhere = open(&["docs", "site"]);
        assert!(!t.reconcile(Some(&open(&["api", "docs", "web"])), Some("web"), &elsewhere));
        assert_eq!(t.order(), ["web", "api"]);
        assert!(t.reconcile(Some(&open(&["api", "docs", "new", "web"])), Some("web"), &elsewhere));
        assert_eq!(t.order(), ["web", "api", "new"]);
        // A tab it already has stays, though another window has one too.
        let mut t = tabs(&["web", "docs"]);
        assert!(!t.reconcile(Some(&open(&["docs", "web"])), Some("web"), &elsewhere));
        assert_eq!(t.order(), ["web", "docs"]);
    }

    #[test]
    fn insert_at_places_a_dropped_in_tab() {
        let mut t = tabs(&["web", "api"]);
        assert!(t.insert_at("docs", 1));
        assert_eq!(t.order(), ["web", "docs", "api"]);
        assert!(t.insert_at("site", 9), "past the end is last");
        assert!(!t.insert_at("web", 0), "it has one");
        assert_eq!(t.order(), ["web", "docs", "api", "site"]);
    }

    #[test]
    fn an_unknown_open_set_only_adds_the_current_project() {
        let mut t = tabs(&["web", "api"]);
        assert!(t.reconcile(None, Some("docs"), &[]));
        assert_eq!(t.order(), ["web", "api", "docs"]);
        assert!(!t.reconcile(None, None, &[]));
    }
}
