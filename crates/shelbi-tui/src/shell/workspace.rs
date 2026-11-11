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
//!   On any machine: a remote workspace's editor and diff run there, and
//!   they (like its agent) attach through the machine's relay.
//!
//! With `diff_tool: builtin` (the default) View Diff is Shelbi's own live diff
//! ([`DiffView`]) instead of a session; it's kept while the panel is open, so
//! going to the agent and back keeps the cursor and folds.
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

use super::diff_view::{DiffOpener, DiffOut, DiffSetup, DiffView};
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
    /// Open the worktree (the editor) or its diff in the app chosen for the
    /// role; the content view stays as it is.
    OpenApp(ReviewRole),
    /// Reveal the workspace worktree folder in the OS file manager.
    RevealFolder,
    /// Copy the task's branch name to the clipboard.
    CopyBranch,
    /// Open the task's pull request in the system browser.
    OpenPr,
    /// Open the full task-description popover over the main area.
    ShowDescription,
    /// Back button / Esc: return to the nav sidebar, leaving the content
    /// sessions loaded.
    Back,
    /// Close (q): tear the content sessions down (the shell asks the daemon to
    /// end the editor/diff sessions) and return to the nav sidebar.
    Close,
    /// The diff's `Ctrl-g`: open this file in the user's editor at this line.
    OpenFile { path: std::path::PathBuf, line: Option<u32> },
}

/// The live workspace interface for one dev workspace.
pub struct WorkspaceInterface {
    panel: WorkspacePanel,
    /// The content terminal view (agent / diff / editor).
    content: SessionManager,
    workspace: String,
    focus: WsFocus,
    /// A click asked for the content (its sidebar row, a panel view): it
    /// takes the keys once it is live, unless they move first.
    content_wanted: bool,
    /// The in-progress task on this workspace, when resolvable — the source of
    /// the panel's title / description preview and the `More` popover's data.
    task: Option<IssueFile>,
    /// A remote worktree's `host:path` (what the folder row copies).
    remote_address: Option<String>,
    project: String,
    /// Shelbi's own diff, once View Diff has shown it.
    diff: Option<DiffView>,
    /// The task the workspace is on now, which [`Self::diff`] is about. Unlike
    /// [`Self::task`] this follows the board while the view stays open.
    diff_task: Option<String>,
    /// The main area shows [`Self::diff`].
    diff_showing: bool,
    diff_setup: DiffSetup,
    /// A test's session in place of the worktree's.
    diff_opener: Option<DiffOpener>,
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
        let diff_task = task.as_ref().map(|t| t.task.id.clone());
        let mut panel = WorkspacePanel::new(worktree, editor_name, agent_name, title, description, status);
        panel.set_branch_and_pr(
            task.as_ref().and_then(|t| t.task.branch.as_deref()),
            task.as_ref().and_then(|t| shelbi_app::board::task_pr(&t.task)),
        );
        let mut content = SessionManager::new(project, connector).with_relauncher(Arc::new(
            shelbi_app::session::WorkspaceContentRelauncher {
                project: project.to_string(),
                link: None,
            },
        ));
        // Agent is the default view: bind to the workspace's agent session.
        content.show(SessionRef::Workspace(workspace.clone()));
        Self {
            panel,
            content,
            workspace,
            focus: WsFocus::Panel,
            content_wanted: false,
            task,
            remote_address: None,
            project: project.to_string(),
            diff: None,
            diff_task,
            diff_showing: false,
            diff_setup: DiffSetup::default(),
            diff_opener: None,
        }
    }

    /// The keys Shelbi's own diff reads.
    pub fn with_diff_setup(mut self, setup: DiffSetup) -> Self {
        self.diff_setup = setup;
        self
    }

    /// The keymap changed in Settings.
    pub fn set_diff_setup(&mut self, setup: DiffSetup) {
        if let Some(diff) = self.diff.as_mut() {
            diff.set_setup(setup.clone());
        }
        self.diff_setup = setup;
    }

    /// Whether View Diff is Shelbi's own view, not a session.
    pub fn builtin_diff(&self) -> bool {
        self.panel.apps.builtin_diff
    }

    /// Show Shelbi's own diff, opening it the first time, with the keyboard on
    /// it: selecting it means reading it.
    pub fn show_builtin_diff(&mut self) {
        if self.diff.is_none() {
            let task = self.diff_task.clone();
            let opener = self.diff_opener.clone().unwrap_or_else(|| DiffView::workspace_opener(&self.project, &self.workspace, task));
            let worktree = self.remote_address.is_none().then(|| self.panel.worktree.clone());
            self.diff = Some(DiffView::new(opener, self.diff_setup.clone(), worktree, "No changes yet"));
        }
        if let Some(diff) = self.diff.as_mut() {
            diff.set_showing(true);
        }
        self.diff_showing = true;
        self.focus = WsFocus::Content;
    }

    /// The workspace is on `task` now. A diff left by another task has that
    /// task's comments and viewed state, so it's replaced: at once when on show,
    /// otherwise the next time View Diff is selected.
    pub fn follow_task(&mut self, task: Option<&str>) {
        if self.diff_task.as_deref() == task {
            return;
        }
        self.diff_task = task.map(str::to_string);
        self.diff = None;
        if self.diff_showing {
            let focus = self.focus;
            self.show_builtin_diff();
            self.focus = focus;
        }
    }

    /// The main area is going to a session.
    fn leave_diff(&mut self) {
        self.diff_showing = false;
        if let Some(diff) = self.diff.as_mut() {
            diff.set_showing(false);
        }
    }

    /// The diff on show, when it's the one on show.
    fn showing_diff(&mut self) -> Option<&mut DiffView> {
        self.diff.as_mut().filter(|_| self.diff_showing)
    }

    /// Whether the diff has the keyboard and takes `k` ahead of the shell's own
    /// keys (`Ctrl-p`, the focus moves it rebinds).
    pub fn diff_claims(&self, k: &KeyEvent) -> bool {
        self.focus == WsFocus::Content
            && self.diff_showing
            && self.diff.as_ref().is_some_and(|diff| diff.claims(k))
    }

    /// Edit and View Diff open these in their apps (`editor:` /
    /// `diff_tool:` apps) instead of a pane.
    pub fn with_apps(mut self, apps: shelbi_state::AppTools) -> Self {
        self.panel.apps = apps;
        self
    }

    /// Open `opener`'s session instead of the worktree's (test seam).
    #[cfg(test)]
    pub(crate) fn set_diff_opener(&mut self, opener: DiffOpener) {
        self.diff_opener = Some(opener);
    }

    /// The workspace is on a remote machine at `address` (`host:path`): its
    /// folder row copies that, as there's no local folder to reveal.
    pub fn with_remote_address(mut self, address: Option<String>) -> Self {
        self.remote_address = address;
        self
    }

    /// The branch the branch row shows when the task names none (the
    /// worktree's): `workspace_details` resolves the task's, else the
    /// worktree's.
    pub fn with_branch(mut self, branch: Option<String>) -> Self {
        if self.panel.branch.is_none() {
            let pr = self.panel.pr.clone();
            self.panel.set_branch_and_pr(branch.as_deref(), pr);
        }
        self
    }

    /// The task's branch, as the panel's branch row shows it.
    pub fn branch(&self) -> Option<&str> {
        self.panel.branch.as_deref()
    }

    /// Where the panel's pull request opens, when it has one.
    pub fn pr_url(&self) -> Option<&str> {
        self.panel.pr_url()
    }

    /// The editor's display name, as "Edit in <name>" shows it.
    pub fn editor_name(&self) -> &str {
        &self.panel.editor_name
    }

    /// A remote worktree's `host:path`, `None` for a local workspace.
    pub fn remote_address(&self) -> Option<&str> {
        self.remote_address.as_deref()
    }

    /// The panel's Edit entry, as drawn.
    #[cfg(test)]
    pub fn editor_label(&self) -> String {
        format!("Edit in {}", self.panel.editor_name)
    }

    /// Which of Edit / View Diff open an app.
    #[cfg(test)]
    pub fn apps(&self) -> shelbi_state::AppTools {
        self.panel.apps
    }

    /// The editor and diff settings changed: "Edit in <name>" and which of
    /// Edit / View Diff open an app follow them.
    pub fn set_tools(&mut self, editor_name: String, apps: shelbi_state::AppTools) {
        self.panel.editor_name = editor_name;
        self.panel.apps = apps;
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
        self.leave_diff();
        self.content.show(SessionRef::Workspace(self.workspace.clone()));
    }

    /// Bind the content view to a daemon-spawned role session (the caller has
    /// already ensured it exists).
    pub fn show_role(&mut self, role: ReviewRole) {
        self.leave_diff();
        self.content.show(SessionRef::WorkspaceContent {
            workspace: self.workspace.clone(),
            role: role.as_str().to_string(),
        });
    }

    /// Put a message on the panel's status line (effect failures, notes).
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.panel.status_line = msg.into();
    }

    /// Replace the panel's header status (the badge beside the back button).
    /// Called on each board refresh so the header tracks the live workspace
    /// state the sidebar shows, rather than freezing at the value it held when
    /// the view opened.
    pub fn set_header_status(&mut self, status: WsStatus) {
        self.panel.status = status;
    }

    /// The current header status text (test accessor).
    #[cfg(test)]
    pub(crate) fn header_status_text(&self) -> &str {
        &self.panel.status.text
    }

    /// Mark the content session's teardown as deliberate, so the exit that
    /// follows a `Close` is not auto-restarted by the content supervisor.
    pub fn note_deliberate_close(&mut self) {
        self.content.note_deliberate_close();
    }

    // -- background plumbing, delegated to the content SessionManager --------

    pub fn poll(&mut self) -> bool {
        let changed = self.content.poll();
        let diff = self.diff.as_mut().is_some_and(DiffView::poll);
        self.settle_wanted_content() || diff || changed
    }

    pub fn pump_output(&mut self, ring: &mut bool) -> bool {
        self.content.pump_output(ring)
    }

    /// The content view's current [`MainState`] (test accessor).
    #[cfg(test)]
    pub(crate) fn content_state(&self) -> MainState<'_> {
        self.content.state()
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
        self.keys_to_panel();
    }

    /// The panel takes the keys, and a click's wait for the content is over.
    fn keys_to_panel(&mut self) {
        self.focus = WsFocus::Panel;
        self.content_wanted = false;
    }

    /// Whether the content can take the keys: the diff, or a live session.
    fn content_focusable(&self) -> bool {
        self.diff_showing || matches!(self.content.state(), MainState::Live(_))
    }

    /// Move focus to the content view (the shell's `FocusMain` / Ctrl+L), but
    /// only when a content session is live — so focus never lands on an
    /// empty/connecting content pane.
    pub fn focus_content(&mut self) {
        if self.content_focusable() {
            self.focus = WsFocus::Content;
        }
    }

    /// A click opened this workspace or one of its views: the content takes
    /// the keys as soon as it is live ([`Self::poll`] lands them), unless they
    /// move first. The keyboard's Enter leaves them on the panel.
    pub fn want_content(&mut self) {
        self.content_wanted = true;
    }

    /// Land a click's wanted focus once the content can take it. Whether focus
    /// moved.
    fn settle_wanted_content(&mut self) -> bool {
        if !self.content_wanted || !self.content_focusable() {
            return false;
        }
        self.content_wanted = false;
        let moved = self.focus != WsFocus::Content;
        self.focus = WsFocus::Content;
        moved
    }

    #[cfg(test)]
    pub fn content_focused(&self) -> bool {
        matches!(self.focus, WsFocus::Content)
    }

    /// Deliver a bracketed paste (a drag-and-drop lands as a paste of the
    /// dropped path) to the content session. Focus moves to the content view
    /// when it is live so the user can keep typing in the agent/editor after
    /// the drop; the paste is best-effort and reaches only a live session.
    pub fn paste_into_content(&mut self, text: &str) {
        self.focus_content();
        if let Some(diff) = self.showing_diff() {
            diff.paste(text);
            return;
        }
        self.content.send_paste(text);
    }

    fn handle_content_key(&mut self, k: KeyEvent) -> WorkspaceAction {
        if let Some(diff) = self.showing_diff() {
            return match diff.key(k) {
                DiffOut::None => WorkspaceAction::None,
                DiffOut::FocusPanel => {
                    self.keys_to_panel();
                    WorkspaceAction::None
                }
                DiffOut::Edit { path, line } => WorkspaceAction::OpenFile { path, line },
                // A workspace's `gY` goes to its agent: only a review rejects.
                DiffOut::Reject { .. } => WorkspaceAction::None,
            };
        }
        // Tab returns focus to the panel; everything else goes to the agent.
        if k.code == KeyCode::Tab && k.modifiers.is_empty() {
            self.keys_to_panel();
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
                self.focus_content();
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
            if in_panel {
                self.keys_to_panel();
            } else {
                self.focus = WsFocus::Content;
                self.content_wanted = false;
            }
        }
        if in_panel {
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                let effect = self.panel.click(m.column, m.row);
                // A view picked with the mouse is ready for the keyboard: its
                // content takes the keys once it is live.
                if matches!(effect, WsEffect::ShowAgent | WsEffect::ShowDiff | WsEffect::ShowEditor) {
                    self.want_content();
                }
                return map_effect(effect);
            }
            return WorkspaceAction::None;
        }
        if let Some(diff) = self.showing_diff() {
            diff.mouse(m);
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
        let keyboard = focus_main && self.focus == WsFocus::Content;
        if let Some(diff) = self.showing_diff() {
            return diff.render(buf, content_rect, truecolor, keyboard);
        }
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
                &shelbi_app::session::restarting_notice(attempt, max),
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
    let info = shelbi_orchestrator::workspace_session::workspace_details(project, workspace)
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
        WsEffect::CopyBranch => WorkspaceAction::CopyBranch,
        WsEffect::OpenPr => WorkspaceAction::OpenPr,
        WsEffect::ShowAgent => WorkspaceAction::ShowContent(None),
        WsEffect::ShowDiff => WorkspaceAction::ShowContent(Some(ReviewRole::Diff)),
        WsEffect::ShowEditor => WorkspaceAction::ShowContent(Some(ReviewRole::Editor)),
        WsEffect::OpenApp(role) => WorkspaceAction::OpenApp(role),
        WsEffect::ShowDescription => WorkspaceAction::ShowDescription,
        WsEffect::Close => WorkspaceAction::Close,
        // The TUI's panel has no user-shell row (the desktop's), so this
        // never comes out of it.
        WsEffect::ToggleShell => WorkspaceAction::None,
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::session::{ConnectFailure, Connected};
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
                pr: None,
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
            tone: crate::workspace_panel::StatusTone::Yellow,
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

    /// Attaches every target to one real session running `cat`, so the content
    /// goes live.
    struct CatAll {
        sock: std::path::PathBuf,
    }

    impl Connector for CatAll {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            use shelbi_client::Connection;
            let (conn, events) = Connection::open(&self.sock, None, shelbi_proto::capability::ALL)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            conn.attach(None).map_err(|e| ConnectFailure::Message(e.to_string()))?;
            Ok(Connected { conn, events, size: shelbi_app::session::GridSize::new(80, 24) })
        }
    }

    /// Poll (what the shell's loop does each tick) until `done`.
    fn poll_until(it: &mut WorkspaceInterface, done: impl Fn(&WorkspaceInterface) -> bool) -> bool {
        let mut ring = false;
        for _ in 0..1500 {
            it.poll();
            it.pump_output(&mut ring);
            if done(it) {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_click_on_a_view_lands_the_keys_on_its_content_once_live() {
        // nav-click-focuses-content: a mouse click on a panel view hands the
        // keys to its content (Enter keeps them on the panel).
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = crate::shell::session::tests::spawn_cat_session(home.path(), "proj/ws/alpha");
        let mut it = WorkspaceInterface::new(
            "proj",
            Arc::new(CatAll { sock }),
            "alpha",
            "/wt/alpha",
            "Vim",
            "Builder",
            status(),
            Some(sample_task()),
        );
        let (panel, content) = (Rect::new(0, 0, 40, 30), Rect::new(40, 0, 60, 30));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| { it.render(f, panel, content, false, true); }).unwrap();
        let buf = terminal.backend().buffer().clone();
        let rows: Vec<String> = (0..30).map(|y| (0..40).map(|x| buf[(x, y)].symbol()).collect()).collect();
        let diff_y = rows.iter().position(|r| r.contains("View Diff")).expect("View Diff drawn");
        let agent_y = (0..diff_y).rev().find(|&y| rows[y].contains("Builder")).expect("the agent's row") as u16;
        let col = rows[agent_y as usize].find("Builder").unwrap() as u16;

        // The click (the agent's view, already shown): the keys wait on the
        // panel until the agent's terminal is live, then move there.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row: agent_y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(it.handle_mouse(click, panel, content), WorkspaceAction::ShowContent(None));
        assert!(poll_until(&mut it, |i| i.content_focused()), "the agent's terminal took the keys");

        // Back to the panel by key before the content is up again: the wish is
        // over, the keys stay where the user put them.
        it.focus_panel();
        it.want_content();
        it.focus_panel();
        for _ in 0..20 {
            it.poll();
        }
        assert!(!it.content_focused(), "a move to the panel cancels the click's wish");

        // Enter on a view keeps the keys on the panel.
        assert_eq!(it.handle_key(key(KeyCode::Enter)), WorkspaceAction::ShowContent(None));
        for _ in 0..20 {
            it.poll();
        }
        assert!(!it.content_focused(), "the keyboard moves on through the panel");
    }

    #[test]
    fn nav_moves_the_selection_without_an_action() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Down)), WorkspaceAction::None);
        assert_eq!(it.handle_key(key(KeyCode::Up)), WorkspaceAction::None);
    }
}
