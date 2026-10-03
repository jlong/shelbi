//! Navigation: the model of what a client is looking at.
//!
//! A client's view of Shelbi is: which project is selected, which main view
//! is showing (a native view such as the issues board, or a terminal view
//! bound to a session), where focus sits (the sidebar or the main area),
//! which sidebar row is selected, and whether an overlay is open.
//!
//! Per-client state ([`ClientState`]) is separate from global one-shot
//! flags ([`GlobalFlags`]): two attached clients each keep their own
//! current view, sidebar width, focus, and last-view-per-project, while a
//! flag like "the Zen intro has been seen" is global and shared.

use std::collections::HashMap;

/// What the main area is showing.
///
/// `Issues`, `Activity`, and `Machines` are native (in-process) views.
/// [`View::Session`] is a terminal view bound to a session by name — the
/// orchestrator chat, a workspace agent, a legacy spawned agent, or a
/// review editor/diff session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum View {
    Issues,
    Activity,
    Machines,
    /// A session rendered in a terminal view, identified by session name.
    Session(String),
}

impl View {
    /// The default view a client lands on for a project with no remembered
    /// view. The issues board is the most useful cold-start surface.
    pub fn default_for_project() -> View {
        View::Issues
    }
}

/// Where keyboard focus sits. With the main area focused on a terminal
/// view, every key goes to the agent except the palette chord; with the
/// sidebar focused, keys drive navigation. Overlays capture input
/// independently of this (see [`ClientState::overlay`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    Main,
}

/// An open overlay. These are the popups that used to be separate CLI
/// processes and become in-process overlays in the single-process TUI.
/// Only one is open at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    /// The command palette.
    Palette,
    /// Confirm approving (accepting) a review task.
    ReviewConfirm { task_id: String },
    /// Collect a reason for rejecting a review task.
    RejectReason { task_id: String },
    /// The per-project error log.
    ErrorLog,
    /// The first-run Zen Mode intro.
    ZenIntro,
}

/// One client's view of Shelbi. Each attached UI (and each desktop window)
/// owns one of these. Everything here is client-local: another client can
/// be on a different project, view, or focus at the same time.
#[derive(Debug, Clone)]
pub struct ClientState {
    /// The selected project slug, or `None` before any project is chosen.
    project: Option<String>,
    /// The current main view.
    view: View,
    /// Where keyboard focus sits.
    focus: Focus,
    /// The open overlay, if any.
    overlay: Option<Overlay>,
    /// Sidebar width in columns.
    sidebar_width: u16,
    /// The selected sidebar row index.
    sidebar_selection: usize,
    /// Per-project memory of the last view the client was on. Lets a client
    /// return to a project and land back where it left off rather than
    /// resetting to the default view.
    last_view: HashMap<String, View>,
}

/// The default sidebar width in columns. Matches the TUI's historical
/// sidebar width so the ported client looks identical.
pub const DEFAULT_SIDEBAR_WIDTH: u16 = 28;

impl Default for ClientState {
    fn default() -> Self {
        ClientState {
            project: None,
            view: View::default_for_project(),
            focus: Focus::Sidebar,
            overlay: None,
            sidebar_width: DEFAULT_SIDEBAR_WIDTH,
            sidebar_selection: 0,
            last_view: HashMap::new(),
        }
    }
}

impl ClientState {
    /// A fresh client already placed on `project`, on that project's
    /// default view.
    pub fn new(project: impl Into<String>) -> Self {
        let mut s = ClientState::default();
        s.switch_project(project);
        s
    }

    pub fn project(&self) -> Option<&str> {
        self.project.as_deref()
    }

    pub fn view(&self) -> &View {
        &self.view
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn overlay(&self) -> Option<&Overlay> {
        self.overlay.as_ref()
    }

    pub fn sidebar_width(&self) -> u16 {
        self.sidebar_width
    }

    pub fn sidebar_selection(&self) -> usize {
        self.sidebar_selection
    }

    /// Switch to `project`, remembering the view the client was on in the
    /// old project and restoring the view it last had in the new one (its
    /// default view the first time). Focus returns to the sidebar and any
    /// overlay closes — switching project is a fresh context.
    pub fn switch_project(&mut self, project: impl Into<String>) {
        let project = project.into();
        // Record where we were leaving the current project.
        if let Some(current) = self.project.take() {
            self.last_view.insert(current, self.view.clone());
        }
        // Restore (or default) the new project's view.
        self.view = self
            .last_view
            .get(&project)
            .cloned()
            .unwrap_or_else(View::default_for_project);
        self.project = Some(project);
        self.focus = Focus::Sidebar;
        self.overlay = None;
        self.sidebar_selection = 0;
    }

    /// Set the current main view, recording it as the current project's
    /// remembered view so a later [`switch_project`](Self::switch_project)
    /// back to it restores this view.
    pub fn set_view(&mut self, view: View) {
        if let Some(project) = &self.project {
            self.last_view.insert(project.clone(), view.clone());
        }
        self.view = view;
    }

    /// The view the client last had in `project`, if any — the value
    /// [`switch_project`](Self::switch_project) would restore.
    pub fn remembered_view(&self, project: &str) -> Option<&View> {
        self.last_view.get(project)
    }

    pub fn focus_sidebar(&mut self) {
        self.focus = Focus::Sidebar;
    }

    pub fn focus_main(&mut self) {
        self.focus = Focus::Main;
    }

    /// Open `overlay`. An open overlay replaces any currently open one
    /// (only one overlay shows at a time).
    pub fn open_overlay(&mut self, overlay: Overlay) {
        self.overlay = Some(overlay);
    }

    /// Close any open overlay. Returns the overlay that was open, if any.
    pub fn close_overlay(&mut self) -> Option<Overlay> {
        self.overlay.take()
    }

    pub fn set_sidebar_width(&mut self, width: u16) {
        self.sidebar_width = width;
    }

    /// Move the sidebar selection up one row (saturating at the top).
    pub fn select_up(&mut self) {
        self.sidebar_selection = self.sidebar_selection.saturating_sub(1);
    }

    /// Move the sidebar selection down one row, clamped so it never points
    /// past the last row. `row_count` is the number of selectable sidebar
    /// rows; a zero count leaves the selection at 0.
    pub fn select_down(&mut self, row_count: usize) {
        let max = row_count.saturating_sub(1);
        self.sidebar_selection = (self.sidebar_selection + 1).min(max);
    }

    /// Clamp the selection into range after the sidebar's row list changes
    /// (e.g. a workspace appeared or a review task cleared).
    pub fn clamp_selection(&mut self, row_count: usize) {
        let max = row_count.saturating_sub(1);
        if self.sidebar_selection > max {
            self.sidebar_selection = max;
        }
    }
}

/// Global one-shot flags that are NOT per-client. These persist across
/// clients and across a single client's project switches — a flag set by
/// one attached UI is seen by the others.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GlobalFlags {
    /// Whether the first-run Zen Mode intro has been shown. Once seen, the
    /// intro never shows again for any client.
    pub zen_intro_seen: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_client_starts_on_sidebar_and_issues() {
        let c = ClientState::default();
        assert_eq!(c.project(), None);
        assert_eq!(c.view(), &View::Issues);
        assert_eq!(c.focus(), Focus::Sidebar);
        assert!(c.overlay().is_none());
        assert_eq!(c.sidebar_width(), DEFAULT_SIDEBAR_WIDTH);
    }

    #[test]
    fn new_places_client_on_project_default_view() {
        let c = ClientState::new("alpha");
        assert_eq!(c.project(), Some("alpha"));
        assert_eq!(c.view(), &View::Issues);
    }

    #[test]
    fn last_view_remembered_per_project() {
        let mut c = ClientState::new("alpha");
        // On alpha, move to a session view.
        c.set_view(View::Session("orchestrator".into()));
        assert_eq!(c.view(), &View::Session("orchestrator".into()));

        // Switch to beta: lands on beta's default (Issues), not alpha's.
        c.switch_project("beta");
        assert_eq!(c.project(), Some("beta"));
        assert_eq!(c.view(), &View::Issues);

        // On beta, move to Activity.
        c.set_view(View::Activity);

        // Back to alpha: restores the session view we left it on.
        c.switch_project("alpha");
        assert_eq!(c.view(), &View::Session("orchestrator".into()));

        // Back to beta: restores Activity.
        c.switch_project("beta");
        assert_eq!(c.view(), &View::Activity);

        // The remembered views are queryable without switching.
        assert_eq!(
            c.remembered_view("alpha"),
            Some(&View::Session("orchestrator".into()))
        );
        assert_eq!(c.remembered_view("beta"), Some(&View::Activity));
        assert_eq!(c.remembered_view("gamma"), None);
    }

    #[test]
    fn first_visit_to_a_project_has_no_remembered_view() {
        let mut c = ClientState::new("alpha");
        assert_eq!(c.remembered_view("alpha"), None);
        c.set_view(View::Machines);
        assert_eq!(c.remembered_view("alpha"), Some(&View::Machines));
    }

    #[test]
    fn switching_project_resets_focus_selection_and_overlay() {
        let mut c = ClientState::new("alpha");
        c.focus_main();
        c.open_overlay(Overlay::Palette);
        c.select_down(10);
        c.select_down(10);
        assert_eq!(c.sidebar_selection(), 2);

        c.switch_project("beta");
        assert_eq!(c.focus(), Focus::Sidebar);
        assert!(c.overlay().is_none());
        assert_eq!(c.sidebar_selection(), 0);
    }

    #[test]
    fn overlay_open_close_is_single_slot() {
        let mut c = ClientState::new("alpha");
        assert!(c.overlay().is_none());
        c.open_overlay(Overlay::Palette);
        assert_eq!(c.overlay(), Some(&Overlay::Palette));
        // Opening a second overlay replaces the first.
        c.open_overlay(Overlay::ErrorLog);
        assert_eq!(c.overlay(), Some(&Overlay::ErrorLog));
        // Closing returns what was open and clears the slot.
        assert_eq!(c.close_overlay(), Some(Overlay::ErrorLog));
        assert!(c.overlay().is_none());
        assert_eq!(c.close_overlay(), None);
    }

    #[test]
    fn selection_clamps_to_row_count() {
        let mut c = ClientState::new("alpha");
        for _ in 0..100 {
            c.select_down(5);
        }
        assert_eq!(c.sidebar_selection(), 4);
        c.select_up();
        assert_eq!(c.sidebar_selection(), 3);

        // Row list shrinks (a workspace vanished): selection clamps in.
        c.clamp_selection(2);
        assert_eq!(c.sidebar_selection(), 1);

        // An empty row list parks the selection at 0.
        c.clamp_selection(0);
        assert_eq!(c.sidebar_selection(), 0);
        c.select_down(0);
        assert_eq!(c.sidebar_selection(), 0);
    }

    #[test]
    fn global_flags_are_independent_of_client_state() {
        let mut flags = GlobalFlags::default();
        assert!(!flags.zen_intro_seen);
        flags.zen_intro_seen = true;
        // A client switching projects does not touch global flags.
        let mut c = ClientState::new("alpha");
        c.switch_project("beta");
        assert!(flags.zen_intro_seen);
    }
}
