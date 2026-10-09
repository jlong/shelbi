//! The **workspace interface** of the single-process TUI (the workspace-sidebar
//! task) — the dev-workspace twin of [`super::review`].
//!
//! When the user opens a dev workspace that has a running task, the main area
//! splits into a native **workspace panel** on the left (the sidebar's column)
//! and a **content terminal view** on the right. The panel is the shared
//! [`WorkspacePanel`](crate::workspace_panel::WorkspacePanel) widget; the content
//! view is a second [`SessionManager`] bound to one of the workspace's sessions:
//!
//! - **Agent** → the workspace's agent session, `SessionRef::Workspace(name)`
//!   (already live; no daemon call).
//! - **Diff** / **Editor** → the daemon-spawned `<project>/ws/<name>/<role>`
//!   sessions ([`shelbi_orchestrator::workspace_session`]). The shell asks the
//!   daemon to start these over the control socket and only attaches here.
//!
//! The panel's state machine is pure: a key/click yields a
//! [`WsEffect`](crate::workspace_panel::WsEffect) which this type maps to a
//! [`WorkspaceAction`] for the shell to carry out (switching the content
//! session, opening the description popover, returning to the nav sidebar, …).

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Frame;

use shelbi_app::ReviewRole;
use shelbi_state::IssueFile;
use shelbi_term::Size;

use crate::workspace_panel::{render_full, WorkspacePanel, WsEffect, WsStatus};

use super::session::{Connector, MainState, SessionManager, SessionRef};
use super::terminal_view::MouseOutcome;

/// Keyboard focus within the workspace interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WsFocus {
    /// Keys drive the panel (nav / activate).
    Panel,
    /// Keys forward to the content session (agent / editor).
    Content,
}

/// What the shell should do after a workspace-interface interaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceAction {
    /// Nothing (selection moved, key forwarded).
    None,
    /// Show a content view. `None` is the agent session (already live);
    /// `Some(role)` asks the daemon to ensure that session, then binds it.
    ShowContent(Option<ReviewRole>),
    /// Reveal the workspace worktree folder in the OS file manager.
    RevealFolder,
    /// Open the full task-description popover over the main area.
    ShowDescription,
    /// Back button / Esc: return to the nav sidebar, leaving the content
    /// sessions loaded.
    Back,
    /// Close (q): tear the content sessions down (the shell asks the daemon to
    /// end the editor/diff sessions) and return to the nav sidebar.
    Close,
}

/// The live workspace interface for one dev workspace.
pub struct WorkspaceInterface {
    panel: WorkspacePanel,
    /// The content terminal view (agent / diff / editor).
    content: SessionManager,
    workspace: String,
    focus: WsFocus,
    /// The in-progress task on this workspace, when resolvable — the source of
    /// the panel's title / description preview and the `More` popover's data.
    task: Option<IssueFile>,
}

impl WorkspaceInterface {
    /// Build the interface and start connecting the agent content view.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: &str,
        connector: Arc<dyn Connector>,
        workspace: impl Into<String>,
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        agent_name: impl Into<String>,
        status: WsStatus,
        task: Option<IssueFile>,
    ) -> Self {
        let workspace = workspace.into();
        let title = task.as_ref().map(|t| t.task.title.clone()).unwrap_or_default();
        let description = task.as_ref().map(|t| t.body.clone()).unwrap_or_default();
        let panel = WorkspacePanel::new(worktree, editor_name, agent_name, title, description, status);
        let mut content = SessionManager::new(project, connector).with_relauncher(Arc::new(
            super::relaunch::WorkspaceContentRelauncher {
                project: project.to_string(),
            },
        ));
        // Agent is the default view: bind to the workspace's agent session.
        content.show(SessionRef::Workspace(workspace.clone()));
        Self {
            panel,
            content,
            workspace,
            focus: WsFocus::Panel,
            task,
        }
    }

    pub fn workspace(&self) -> &str {
        &self.workspace
    }

    /// The in-progress task, for the shell to populate the description popover.
    pub fn task(&self) -> Option<&IssueFile> {
        self.task.as_ref()
    }

    /// Bind the content view to the agent session (the already-live workspace
    /// session). Re-selecting it while failed re-attaches.
    pub fn show_agent(&mut self) {
        self.content.show(SessionRef::Workspace(self.workspace.clone()));
    }

    /// Bind the content view to a daemon-spawned role session (the caller has
    /// already ensured it exists).
    pub fn show_role(&mut self, role: ReviewRole) {
        self.content.show(SessionRef::WorkspaceContent {
            workspace: self.workspace.clone(),
            role: role.as_str().to_string(),
        });
    }

    /// Put a message on the panel's status line (effect failures, notes).
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.panel.status_line = msg.into();
    }

    /// Mark the content session's teardown as deliberate, so the exit that
    /// follows a `Close` is not auto-restarted by the content supervisor.
    pub fn note_deliberate_close(&mut self) {
        self.content.note_deliberate_close();
    }

    // -- background plumbing, delegated to the content SessionManager --------

    pub fn poll(&mut self) -> bool {
        self.content.poll()
    }

    pub fn pump_output(&mut self, ring: &mut bool) -> bool {
        self.content.pump_output(ring)
    }

    /// Report the content `area` so the content view reflows to fill it (the
    /// panel occupies the sidebar column, so the content view fills the whole
    /// main area).
    pub fn resize(&mut self, content: Rect) {
        self.content.resize(content.width, content.height);
    }

    // -- input ---------------------------------------------------------------

    pub fn handle_key(&mut self, k: KeyEvent) -> WorkspaceAction {
        match self.focus {
            WsFocus::Content => self.handle_content_key(k),
            WsFocus::Panel => self.handle_panel_key(k),
        }
    }

    /// Move focus to the panel (the shell's `FocusSidebar` / Ctrl+H).
    pub fn focus_panel(&mut self) {
        self.focus = WsFocus::Panel;
    }

    /// Move focus to the content view (the shell's `FocusMain` / Ctrl+L), but
    /// only when a content session is live — so focus never lands on an
    /// empty/connecting content pane.
    pub fn focus_content(&mut self) {
        if matches!(self.content.state(), MainState::Live(_)) {
            self.focus = WsFocus::Content;
        }
    }

    #[cfg(test)]
    pub fn content_focused(&self) -> bool {
        matches!(self.focus, WsFocus::Content)
    }

    fn handle_content_key(&mut self, k: KeyEvent) -> WorkspaceAction {
        // Tab returns focus to the panel; everything else goes to the agent.
        if k.code == KeyCode::Tab && k.modifiers.is_empty() {
            self.focus = WsFocus::Panel;
            return WorkspaceAction::None;
        }
        if super::terminal_view::is_copy_key(&k) {
            if let Some(text) = self.content.live_pane_mut().and_then(|p| p.selection_copy()) {
                super::copy_to_clipboard(&text);
            }
            return WorkspaceAction::None;
        }
        if let Some(p) = self.content.live_pane_mut() {
            if let Some(bytes) = p.encode_key(&k) {
                self.content.send_input(&bytes);
            }
        }
        WorkspaceAction::None
    }

    fn handle_panel_key(&mut self, k: KeyEvent) -> WorkspaceAction {
        match k.code {
            // Tab moves focus to the content view (so the user can type in the
            // agent / editor) when one is live.
            KeyCode::Tab if k.modifiers.is_empty() => {
                if matches!(self.content.state(), MainState::Live(_)) {
                    self.focus = WsFocus::Content;
                }
                WorkspaceAction::None
            }
            // Esc returns to the nav sidebar (like the back arrow), leaving the
            // content sessions loaded.
            KeyCode::Esc => WorkspaceAction::Back,
            // `q` tears the editor/diff content sessions down and returns.
            KeyCode::Char('q') => WorkspaceAction::Close,
            KeyCode::Up | KeyCode::Char('k') => {
                self.panel.nav_up();
                WorkspaceAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.panel.nav_down();
                WorkspaceAction::None
            }
            KeyCode::Char('m') => map_effect(self.panel.request_description()),
            KeyCode::Enter | KeyCode::Char(' ') => map_effect(self.panel.activate()),
            _ => WorkspaceAction::None,
        }
    }

    /// Handle a mouse event. `panel_rect` is the sidebar column the panel
    /// occupies; `content_rect` is the main area the content view fills.
    pub fn handle_mouse(
        &mut self,
        m: MouseEvent,
        panel_rect: Rect,
        content_rect: Rect,
    ) -> WorkspaceAction {
        let in_panel = contains(panel_rect, m.column, m.row);
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.focus = if in_panel {
                WsFocus::Panel
            } else {
                WsFocus::Content
            };
        }
        if in_panel {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                return map_effect(self.panel.click(m.column, m.row));
            }
            return WorkspaceAction::None;
        }
        // Content area: forward to the content session like the main area does.
        let (pane_col, pane_row) = (
            m.column.saturating_sub(content_rect.x),
            m.row.saturating_sub(content_rect.y),
        );
        let viewer = Size::new(content_rect.width, content_rect.height);
        if let Some(p) = self.content.live_pane_mut() {
            match p.on_mouse(&m, pane_col, pane_row, viewer) {
                MouseOutcome::Forward(bytes) => self.content.send_input(&bytes),
                MouseOutcome::Copy(text) => super::copy_to_clipboard(&text),
                MouseOutcome::Handled | MouseOutcome::Ignored => {}
            }
        }
        WorkspaceAction::None
    }

    // -- render --------------------------------------------------------------

    /// Render the panel into `panel_rect` (the sidebar column) and the content
    /// view into `content_rect` (the main area). Returns the cursor position when
    /// the content view is focused and live.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        panel_rect: Rect,
        content_rect: Rect,
        truecolor: bool,
        focus_main: bool,
    ) -> Option<(u16, u16)> {
        render_full(frame, &mut self.panel, panel_rect);
        let buf = frame.buffer_mut();
        let mut cursor = None;
        match self.content.state() {
            MainState::Empty => {}
            MainState::Connecting(r) => super::render_placeholder(
                buf,
                content_rect,
                &format!("Connecting to {}…", r.display()),
            ),
            MainState::Failed(r, err) => super::render_placeholder(
                buf,
                content_rect,
                &format!(
                    "Couldn't attach to {}: {err} — select it again to retry.",
                    r.display()
                ),
            ),
            // The agent session should be live for a workspace with a task; if
            // it isn't up yet, treat it like "no session here" rather than the
            // full idle placeholder (which doesn't fit a content pane).
            MainState::Idle(info) => super::render_placeholder(
                buf,
                content_rect,
                &format!("No live session for {}", info.name),
            ),
            MainState::Restarting(attempt, max) => super::render_placeholder(
                buf,
                content_rect,
                &format!("Session exited — restarting ({attempt}/{max})…"),
            ),
            MainState::GaveUp(last_line) => super::render_placeholder(
                buf,
                content_rect,
                &super::session::gave_up_notice(last_line),
            ),
            MainState::Live(pane) => {
                let cur = pane.render(buf, content_rect, truecolor);
                if focus_main && self.focus == WsFocus::Content {
                    cursor = cur;
                }
            }
        }
        cursor
    }
}

/// Reveal the workspace worktree folder in the OS file manager (off the UI
/// thread).
pub fn reveal_folder(project: &str, workspace: &str) -> Result<(), String> {
    let info = shelbi_orchestrator::workspace_session::workspace_open_info(project, workspace)
        .map_err(|e| e.to_string())?;
    if info.worktree.is_empty() {
        return Err("no workspace worktree to reveal".into());
    }
    let (prog, args) = crate::panel::reveal_command(crate::panel::current_os(), &info.worktree);
    crate::panel::spawn_opener(&prog, &args)
}

/// Map a panel [`WsEffect`] to the shell-level [`WorkspaceAction`].
fn map_effect(effect: WsEffect) -> WorkspaceAction {
    match effect {
        WsEffect::None => WorkspaceAction::None,
        WsEffect::Back => WorkspaceAction::Back,
        WsEffect::RevealFolder => WorkspaceAction::RevealFolder,
        WsEffect::ShowAgent => WorkspaceAction::ShowContent(None),
        WsEffect::ShowDiff => WorkspaceAction::ShowContent(Some(ReviewRole::Diff)),
        WsEffect::ShowEditor => WorkspaceAction::ShowContent(Some(ReviewRole::Editor)),
        WsEffect::ShowDescription => WorkspaceAction::ShowDescription,
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::session::{ConnectFailure, Connected};
    use crate::theme::STATUS_YELLOW;
    use crossterm::event::KeyModifiers;

    /// A connector that never resolves — the content view stays Connecting, so
    /// these pure-routing tests touch no real session.
    struct NeverConnector;
    impl Connector for NeverConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            Err(ConnectFailure::Message("test: no session".into()))
        }
    }

    fn sample_task() -> IssueFile {
        let ts = chrono::Utc::now();
        IssueFile {
            task: shelbi_core::Issue {
                id: "cold-start".into(),
                title: "Cold-start cache".into(),
                column: shelbi_core::Column::in_progress(),
                priority: 1,
                assigned_to: Some("alpha".into()),
                workflow: None,
                branch: Some("shelbi/cold-start".into()),
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: ts,
                updated_at: ts,
                params: std::collections::BTreeMap::new(),
            },
            body: "## Summary\n\nWarm the application cache during startup.".into(),
            tracker_assignees: Vec::new(),
        }
    }

    fn status() -> WsStatus {
        WsStatus {
            text: "IN PROGRESS".into(),
            color: STATUS_YELLOW,
        }
    }

    fn iface() -> WorkspaceInterface {
        WorkspaceInterface::new(
            "proj",
            Arc::new(NeverConnector),
            "alpha",
            "/wt/alpha",
            "Vim",
            "Developer",
            status(),
            Some(sample_task()),
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn enter_on_a_switch_yields_show_content() {
        let mut it = iface();
        // Agent is selected by default → Enter shows the agent (content None).
        assert_eq!(it.handle_key(key(KeyCode::Enter)), WorkspaceAction::ShowContent(None));
        // Step to the Diff switch and activate it.
        it.panel.nav_down();
        assert_eq!(
            it.handle_key(key(KeyCode::Enter)),
            WorkspaceAction::ShowContent(Some(ReviewRole::Diff))
        );
    }

    #[test]
    fn esc_goes_back_and_q_closes() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Esc)), WorkspaceAction::Back);
        assert_eq!(it.handle_key(key(KeyCode::Char('q'))), WorkspaceAction::Close);
    }

    #[test]
    fn m_opens_the_description_popover() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Char('m'))), WorkspaceAction::ShowDescription);
        assert_eq!(it.task().map(|t| t.task.title.as_str()), Some("Cold-start cache"));
    }

    #[test]
    fn m_is_inert_for_a_bodyless_workspace() {
        let mut it = WorkspaceInterface::new(
            "proj",
            Arc::new(NeverConnector),
            "alpha",
            "/wt/alpha",
            "Vim",
            "Developer",
            status(),
            None,
        );
        assert_eq!(it.handle_key(key(KeyCode::Char('m'))), WorkspaceAction::None);
    }

    #[test]
    fn tab_stays_on_panel_until_content_is_live() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Tab)), WorkspaceAction::None);
        assert!(!it.content_focused());
        it.focus_content();
        assert!(!it.content_focused(), "focus does not move until content is live");
    }

    #[test]
    fn nav_moves_the_selection_without_an_action() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Down)), WorkspaceAction::None);
        assert_eq!(it.handle_key(key(KeyCode::Up)), WorkspaceAction::None);
    }
}
