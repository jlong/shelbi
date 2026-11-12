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
//! order keeps its place, and a project newly found open goes at the end.
//! The window's own project always has a tab, open on disk yet or not
//! (bringing it up is still on its way, or it waits on onboarding).

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

    /// Take `project`'s tab away. Whether it had one.
    pub fn remove(&mut self, project: &str) -> bool {
        let before = self.order.len();
        self.order.retain(|p| p != project);
        self.order.len() != before
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
    /// a project newly open gets a tab at the end (in `open`'s order), and
    /// `current` (the window's own project, `None` on the chooser) keeps or
    /// gets one either way. With `open` unknown (`None`: a remote window,
    /// whose host's listing doesn't say what is open, or a catalog not yet
    /// read) nothing goes, and only `current` is added. Whether the tabs
    /// changed.
    pub fn reconcile(&mut self, open: Option<&[String]>, current: Option<&str>) -> bool {
        let before = self.order.clone();
        if let Some(open) = open {
            self.order
                .retain(|p| current == Some(p.as_str()) || open.iter().any(|o| o == p));
            for project in open {
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
    fn reconcile_keeps_the_saved_order_and_appends_new_ones() {
        // `list_open_projects` is sorted by name; the tabs keep their own order.
        let mut t = tabs(&["web", "api"]);
        assert!(t.reconcile(Some(&open(&["api", "docs", "web"])), Some("web")));
        assert_eq!(t.order(), ["web", "api", "docs"]);
        assert!(!t.reconcile(Some(&open(&["api", "docs", "web"])), Some("web")), "settled");
    }

    #[test]
    fn reconcile_drops_closed_projects_but_never_the_current_one() {
        let mut t = tabs(&["web", "api", "docs"]);
        // `docs` was quit elsewhere; `web` (this window's) isn't recorded open
        // yet (its orchestrator is still coming up).
        assert!(t.reconcile(Some(&open(&["api"])), Some("web")));
        assert_eq!(t.order(), ["web", "api"]);
        // The chooser has no current project.
        assert!(t.reconcile(Some(&open(&[])), None));
        assert!(t.order().is_empty());
    }

    #[test]
    fn an_unknown_open_set_only_adds_the_current_project() {
        let mut t = tabs(&["web", "api"]);
        assert!(t.reconcile(None, Some("docs")));
        assert_eq!(t.order(), ["web", "api", "docs"]);
        assert!(!t.reconcile(None, None));
    }
}
