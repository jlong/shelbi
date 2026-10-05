//! The review interface of the single-process TUI (`rt-tui-review`,
//! removing-tmux Phase 4e).
//!
//! The main area splits into a **native review panel** on the left and a
//! **content terminal view** on the right. The panel is the existing shared
//! [`ReviewPanel`](crate::review_panel::ReviewPanel) widget — the same items,
//! labels, and keys the tmux `__review-panel` process renders — embedded and
//! drawn with [`render_full`](crate::review_panel::render_full). The content
//! view is a second [`SessionManager`] bound to one of the review slot's
//! sessions:
//!
//! - **Chat** → the review agent, the workspace session `<project>/ws/<slot>`.
//! - **Diff** / **Editor** → the daemon-spawned `<project>/review/<slot>/<role>`
//!   sessions ([`shelbi_orchestrator::review_session`]). The shell asks the
//!   daemon to start these over the control socket and only attaches here, so
//!   they survive a client detach and the daemon frees the slot's port on
//!   close.
//!
//! The panel's state machine is pure: a key/click yields a
//! [`PanelEffect`](crate::review_panel::PanelEffect) which this type maps to a
//! [`ReviewAction`] for the shell to carry out (switching the content session,
//! running the gated merge off-thread, opening the reject overlay, …). While a
//! gated merge is in flight the whole review is inert to competing input
//! (the panel declines Approve/Reject/quit; this also drops view switches and
//! Back) so a multi-second `gh` merge can never be raced — plan AC #4.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use ratatui::Frame;

use shelbi_app::ReviewRole;
use shelbi_term::Size;

use crate::review_panel::{render_full, PanelEffect, ReviewPanel};

use super::session::{Connector, MainState, SessionManager, SessionRef};
use super::terminal_view::MouseOutcome;

/// The panel column's width, clamped to leave room for the content view.
const PANEL_WIDTH: u16 = 32;

/// Keyboard focus within the review interface.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFocus {
    /// Keys drive the panel (nav / activate).
    Panel,
    /// Keys forward to the content session (chat / editor).
    Content,
}

/// What the shell should do after a review-interface interaction. The shell
/// performs the blocking parts off the UI thread (one event loop).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewAction {
    /// Nothing (selection moved, key forwarded, inert while merging).
    None,
    /// Show a content view. `None` is Chat (the agent session, already live);
    /// `Some(role)` asks the daemon to ensure that session, then binds it.
    ShowContent(Option<ReviewRole>),
    /// Run the gated review→done merge off the UI thread.
    Approve,
    /// Open the reject-reason overlay.
    Reject,
    /// Open the configured review URL in the system browser.
    OpenBrowser,
    /// Reveal the review worktree folder in the OS file manager.
    RevealFolder,
    /// Back button: return to the orchestrator chat, leaving the review loaded.
    Back,
    /// Close the review (q / Esc): tear the interface down (the shell asks the
    /// daemon to end the editor/diff/server sessions and free the port).
    Close,
}

/// The live review interface for one task.
pub struct ReviewInterface {
    panel: ReviewPanel,
    /// The content terminal view (chat / diff / editor), its own connection so
    /// it coexists with the panel. Reuses all the main-area session machinery.
    content: SessionManager,
    task_id: String,
    slot: String,
    focus: ReviewFocus,
}

impl ReviewInterface {
    /// Build the interface and start connecting the Chat (agent) content view.
    pub fn new(
        project: &str,
        connector: Arc<dyn Connector>,
        task_id: impl Into<String>,
        slot: impl Into<String>,
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        has_review_url: bool,
    ) -> Self {
        let task_id = task_id.into();
        let slot = slot.into();
        let panel = ReviewPanel::new(worktree, editor_name, has_review_url);
        let mut content = SessionManager::new(project, connector);
        // Chat is the default view: bind to the review agent's workspace session.
        content.show(SessionRef::Workspace(slot.clone()));
        Self {
            panel,
            content,
            task_id,
            slot,
            focus: ReviewFocus::Panel,
        }
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Bind the content view to Chat (the agent session).
    pub fn show_chat(&mut self) {
        self.content.show(SessionRef::Workspace(self.slot.clone()));
    }

    /// Bind the content view to a daemon-spawned role session. The caller has
    /// already ensured it exists (the daemon's half).
    pub fn show_role(&mut self, role: ReviewRole) {
        self.content.show(SessionRef::Review {
            slot: self.slot.clone(),
            role: role.as_str().to_string(),
        });
    }

    /// Drive the busy spinner while a merge is in flight / set the state.
    pub fn set_merging(&mut self, on: bool) {
        self.panel.merging = on;
        if on {
            self.panel.spinner = 0;
            self.panel.status_line.clear();
        }
    }

    pub fn is_merging(&self) -> bool {
        self.panel.merging
    }

    pub fn tick_spinner(&mut self) {
        if self.panel.merging {
            self.panel.spinner = self.panel.spinner.wrapping_add(1);
        }
    }

    /// Put a message on the panel's status line (effect failures, notes).
    pub fn set_status(&mut self, msg: impl Into<String>) {
        self.panel.status_line = msg.into();
    }

    // -- background plumbing, delegated to the content SessionManager --------

    pub fn poll(&mut self) -> bool {
        self.content.poll()
    }

    pub fn pump_output(&mut self, ring: &mut bool) -> bool {
        self.content.pump_output(ring)
    }

    /// Report the review `area` (the whole main area) so the content view
    /// reflows to fill just its sub-rect.
    pub fn resize(&self, area: Rect) {
        let (_panel, content) = split(area);
        self.content.resize(content.width, content.height);
    }

    // -- input ---------------------------------------------------------------

    /// Handle a key. `is_palette` is true when the shell already recognized the
    /// palette-open chord (handled by the shell), so it never reaches here.
    pub fn handle_key(&mut self, k: KeyEvent) -> ReviewAction {
        match self.focus {
            ReviewFocus::Content => self.handle_content_key(k),
            ReviewFocus::Panel => self.handle_panel_key(k),
        }
    }

    fn handle_content_key(&mut self, k: KeyEvent) -> ReviewAction {
        // Tab returns focus to the panel; everything else goes to the agent.
        if k.code == KeyCode::Tab && k.modifiers.is_empty() {
            self.focus = ReviewFocus::Panel;
            return ReviewAction::None;
        }
        if let Some(p) = self.content.live_pane_mut() {
            if let Some(bytes) = p.encode_key(&k) {
                self.content.send_input(&bytes);
            }
        }
        ReviewAction::None
    }

    fn handle_panel_key(&mut self, k: KeyEvent) -> ReviewAction {
        // While the gated merge runs the review is inert to competing input:
        // the panel itself declines Approve/Reject/quit, and here we also drop
        // view switches, Back, and the focus toggle so nothing re-enters
        // mid-merge (AC #4). The spinner still advances on the loop tick.
        if self.panel.merging {
            return ReviewAction::None;
        }
        match k.code {
            // Tab moves focus to the content view (so the reviewer can type in
            // the agent / editor) when one is live.
            KeyCode::Tab if k.modifiers.is_empty() => {
                if matches!(self.content.state(), MainState::Live(_)) {
                    self.focus = ReviewFocus::Content;
                }
                ReviewAction::None
            }
            KeyCode::Char('q') | KeyCode::Esc => {
                self.panel.request_quit();
                if self.panel.should_quit {
                    ReviewAction::Close
                } else {
                    ReviewAction::None
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.panel.nav_up();
                ReviewAction::None
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.panel.nav_down();
                ReviewAction::None
            }
            KeyCode::Enter | KeyCode::Char(' ') => map_effect(self.panel.activate()),
            _ => ReviewAction::None,
        }
    }

    /// Handle a mouse event within the review `area` (the whole main area).
    pub fn handle_mouse(&mut self, m: MouseEvent, area: Rect) -> ReviewAction {
        let (panel_rect, content_rect) = split(area);
        let in_panel = contains(panel_rect, m.column, m.row);
        if matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.focus = if in_panel {
                ReviewFocus::Panel
            } else {
                ReviewFocus::Content
            };
        }
        if in_panel {
            if self.panel.merging {
                return ReviewAction::None;
            }
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                return map_effect(self.panel.click(m.column, m.row));
            }
            return ReviewAction::None;
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
        ReviewAction::None
    }

    // -- render --------------------------------------------------------------

    /// Render the panel beside the content view. Returns the cursor position
    /// when the content view is focused and live.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        area: Rect,
        truecolor: bool,
        focus_main: bool,
    ) -> Option<(u16, u16)> {
        let (panel_rect, content_rect) = split(area);
        // Panel first (render_full borrows the frame's buffer internally).
        render_full(frame, &mut self.panel, panel_rect);
        // Then the content view into the remaining buffer.
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
                &format!("Couldn't attach to {}: {err}", r.display()),
            ),
            MainState::Live(pane) => {
                let cur = pane.render(buf, content_rect, truecolor);
                if focus_main && self.focus == ReviewFocus::Content {
                    cursor = cur;
                }
            }
        }
        cursor
    }
}

/// Open the configured review URL for `task` in the system browser (off the UI
/// thread). Reuses the shared opener used by the tmux review panel.
pub fn open_browser(project: &str, task: &str) -> Result<(), String> {
    let url = crate::review_panel::review_url(project, task)
        .ok_or_else(|| "no review URL configured".to_string())?;
    let (prog, args) = crate::review_panel::open_url_command(crate::review_panel::current_os(), &url);
    crate::review_panel::spawn_opener(&prog, &args)
}

/// Reveal the review worktree folder in the OS file manager (off the UI thread).
pub fn reveal_folder(project: &str, task: &str) -> Result<(), String> {
    let info = shelbi_orchestrator::review_session::review_open_info(project, task)
        .map_err(|e| e.to_string())?;
    if info.worktree.is_empty() {
        return Err("no review worktree to reveal".into());
    }
    let (prog, args) =
        crate::review_panel::reveal_command(crate::review_panel::current_os(), &info.worktree);
    crate::review_panel::spawn_opener(&prog, &args)
}

/// Map a panel [`PanelEffect`] to the shell-level [`ReviewAction`].
fn map_effect(effect: PanelEffect) -> ReviewAction {
    match effect {
        PanelEffect::None => ReviewAction::None,
        PanelEffect::FocusDashboard => ReviewAction::Back,
        PanelEffect::ShowChat => ReviewAction::ShowContent(None),
        PanelEffect::ShowDiff => ReviewAction::ShowContent(Some(ReviewRole::Diff)),
        PanelEffect::ShowVim => ReviewAction::ShowContent(Some(ReviewRole::Editor)),
        PanelEffect::OpenBrowser => ReviewAction::OpenBrowser,
        PanelEffect::RevealFolder => ReviewAction::RevealFolder,
        PanelEffect::Approve => ReviewAction::Approve,
        PanelEffect::RejectPrompt => ReviewAction::Reject,
    }
}

/// Split the review `area` into (panel, content).
fn split(area: Rect) -> (Rect, Rect) {
    let w = PANEL_WIDTH.min(area.width.saturating_sub(1).max(1));
    let panel = Rect::new(area.x, area.y, w, area.height);
    let content = Rect::new(
        area.x + w,
        area.y,
        area.width.saturating_sub(w),
        area.height,
    );
    (panel, content)
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::session::Connected;
    use crossterm::event::KeyModifiers;

    /// A connector that never resolves — the content view stays Connecting, so
    /// these pure-routing tests touch no real session.
    struct NeverConnector;
    impl Connector for NeverConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, String> {
            Err("test: no session".into())
        }
    }

    fn iface() -> ReviewInterface {
        ReviewInterface::new(
            "proj",
            Arc::new(NeverConnector),
            "fix-login",
            "review-1",
            "/wt",
            "Vim",
            true,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    #[test]
    fn enter_on_approve_row_yields_approve() {
        let mut it = iface();
        // Navigate to the Approve row and activate it.
        for _ in 0..24 {
            if map_effect(it.panel.activate()) == ReviewAction::Approve {
                return;
            }
            it.panel.nav_down();
        }
        panic!("never reached an Approve activation");
    }

    #[test]
    fn competing_input_is_blocked_while_merging() {
        let mut it = iface();
        it.set_merging(true);
        // Every panel key is inert while the gated merge runs: no second
        // approve, no reject, no close, no view switch (AC #4).
        for code in [
            KeyCode::Enter,
            KeyCode::Char(' '),
            KeyCode::Char('q'),
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::Down,
            KeyCode::Up,
        ] {
            assert_eq!(
                it.handle_key(key(code)),
                ReviewAction::None,
                "key {code:?} must be inert while merging"
            );
        }
        // And it never quit.
        assert!(!it.panel.should_quit, "q/Esc cannot close during a merge");
    }

    #[test]
    fn q_closes_when_not_merging() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Char('q'))), ReviewAction::Close);
    }

    #[test]
    fn tab_toggles_focus_to_content_only_when_live() {
        let mut it = iface();
        // Content is still Connecting (NeverConnector), so Tab stays on panel.
        assert_eq!(it.handle_key(key(KeyCode::Tab)), ReviewAction::None);
        assert_eq!(it.focus, ReviewFocus::Panel);
    }
}
