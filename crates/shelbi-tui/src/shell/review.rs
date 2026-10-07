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
use shelbi_state::IssueFile;
use shelbi_term::Size;

use crate::review_panel::{render_full, PanelEffect, ReviewPanel};

use super::session::{Connector, MainState, SessionManager, SessionRef};
use super::terminal_view::MouseOutcome;

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
    /// Open the full task-description popover over the main area (the `More`
    /// link / `m` key). The shell opens the overlay; the content session keeps
    /// running underneath.
    ShowDescription,
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
    /// The reviewed task, when resolvable — the source of the panel's title and
    /// description preview, and the data the `More` description popover shows.
    /// `None` only if the issue store couldn't produce it (the panel then shows
    /// no task info and `More` is inert).
    task: Option<IssueFile>,
}

impl ReviewInterface {
    /// Build the interface and start connecting the Chat (agent) content view.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        project: &str,
        connector: Arc<dyn Connector>,
        task_id: impl Into<String>,
        slot: impl Into<String>,
        worktree: impl Into<String>,
        editor_name: impl Into<String>,
        has_review_url: bool,
        task: Option<IssueFile>,
    ) -> Self {
        let task_id = task_id.into();
        let slot = slot.into();
        // The panel shows the task's title and a preview of its body; the full
        // body opens in the description popover.
        let title = task.as_ref().map(|t| t.task.title.clone()).unwrap_or_default();
        let description = task.as_ref().map(|t| t.body.clone()).unwrap_or_default();
        let panel = ReviewPanel::new(worktree, editor_name, has_review_url, title, description);
        let mut content = SessionManager::new(project, connector);
        // Chat is the default view: bind to the review agent's workspace session.
        content.show(SessionRef::Workspace(slot.clone()));
        Self {
            panel,
            content,
            task_id,
            slot,
            focus: ReviewFocus::Panel,
            task,
        }
    }

    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// The reviewed task, for the shell to populate the description popover.
    pub fn task(&self) -> Option<&IssueFile> {
        self.task.as_ref()
    }

    /// Tighten the content view's connect-retry policy and re-arm the connect
    /// with it (test seam). The initial connect started in [`new`](Self::new)
    /// with the default 15 s deadline, so a test that wants to observe the
    /// give-up without waiting 15 s sets a short policy and reconnects.
    #[cfg(test)]
    pub(crate) fn set_content_retry(&mut self, retry: super::session::RetryPolicy) {
        self.content.set_retry(retry);
        self.content.reconnect();
    }

    /// The content view's current [`MainState`] (test accessor).
    #[cfg(test)]
    pub(crate) fn content_state(&self) -> MainState<'_> {
        self.content.state()
    }

    /// Bind the content view to Chat (the agent session). Re-selecting Chat
    /// while it is the current but *failed* target re-attaches — `SessionManager::show`
    /// re-attempts a failed/idle binding, so this is the retry path for a content
    /// connect that timed out (`rt-review-screen-hangs-on-connecting`).
    pub fn show_chat(&mut self) {
        self.content.show(SessionRef::Workspace(self.slot.clone()));
    }

    /// Bind the content view to a daemon-spawned role session. The caller has
    /// already ensured it exists (the daemon's half). Re-selecting the same role
    /// while it is the current but failed target re-attaches.
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

    /// Report the content `area` so the content view reflows to fill it. The
    /// panel now occupies the sidebar column, so the content view fills the whole
    /// main area (`rt-review-screen-hangs-on-connecting`: review panel replaces
    /// the sidebar, two columns). `&mut self` so the content `SessionManager`
    /// records the last requested size and re-applies it when the session goes
    /// live (`rt-review-content-session-edit-in-vi-doesn-t-fill-the-content-area`).
    pub fn resize(&mut self, content: Rect) {
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

    /// Move focus to the panel (the shell's `FocusSidebar` / Ctrl+H while a
    /// review owns the main area). Inert while a gated merge runs, so the
    /// focus can't change mid-merge — matching the Tab toggle (AC #4).
    pub fn focus_panel(&mut self) {
        if self.panel.merging {
            return;
        }
        self.focus = ReviewFocus::Panel;
    }

    /// Move focus to the content view (the shell's `FocusMain` / Ctrl+L),
    /// but only when a content session is live — the same guard Tab uses, so
    /// focus never lands on an empty/connecting content pane. Inert while a
    /// gated merge runs.
    pub fn focus_content(&mut self) {
        if self.panel.merging {
            return;
        }
        if matches!(self.content.state(), MainState::Live(_)) {
            self.focus = ReviewFocus::Content;
        }
    }

    /// Whether the content view currently holds focus. For the shell's focus
    /// routing and tests.
    #[cfg(test)]
    pub fn content_focused(&self) -> bool {
        matches!(self.focus, ReviewFocus::Content)
    }

    fn handle_content_key(&mut self, k: KeyEvent) -> ReviewAction {
        // Tab returns focus to the panel; everything else goes to the agent.
        if k.code == KeyCode::Tab && k.modifiers.is_empty() {
            self.focus = ReviewFocus::Panel;
            return ReviewAction::None;
        }
        // Cmd+C / Ctrl+Shift+C copies the content view's selection and is
        // consumed, never forwarded (no stray `c` / Ctrl+C to the editor). Same
        // rule as the main-area terminal (see the shell's `handle_main_key`).
        if super::terminal_view::is_copy_key(&k) {
            if let Some(text) = self.content.live_pane_mut().and_then(|p| p.selection_copy()) {
                super::copy_to_clipboard(&text);
            }
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
            // Esc leaves the review loaded and returns to the nav sidebar (like
            // the back arrow) — `rt-review-screen-hangs-on-connecting` AC.
            KeyCode::Esc => ReviewAction::Back,
            // `q` is the explicit teardown: end the editor/diff/server sessions
            // and free the slot's port.
            KeyCode::Char('q') => {
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
            // `m` opens the full task-description popover (the `More` link's
            // keyboard twin). Inert when the task has no body.
            KeyCode::Char('m') => map_effect(self.panel.request_description()),
            KeyCode::Enter | KeyCode::Char(' ') => map_effect(self.panel.activate()),
            _ => ReviewAction::None,
        }
    }

    /// Handle a mouse event. `panel_rect` is the sidebar column the panel now
    /// occupies; `content_rect` is the main area the content view fills.
    pub fn handle_mouse(
        &mut self,
        m: MouseEvent,
        panel_rect: Rect,
        content_rect: Rect,
    ) -> ReviewAction {
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

    /// Render the panel into `panel_rect` (the sidebar column) and the content
    /// view into `content_rect` (the main area). Returns the cursor position
    /// when the content view is focused and live.
    pub fn render(
        &mut self,
        frame: &mut Frame,
        panel_rect: Rect,
        content_rect: Rect,
        truecolor: bool,
        focus_main: bool,
    ) -> Option<(u16, u16)> {
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
                &format!(
                    "Couldn't attach to {}: {err} — select it again to retry.",
                    r.display()
                ),
            ),
            // The review agent's chat session should always be live; if it isn't
            // up yet, treat it like "no session here" rather than the dev-slot
            // idle placeholder (which doesn't fit a review content pane).
            MainState::Idle(info) => super::render_placeholder(
                buf,
                content_rect,
                &format!("No live session for {}", info.name),
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
    let (prog, args) = crate::panel::open_url_command(crate::panel::current_os(), &url);
    crate::panel::spawn_opener(&prog, &args)
}

/// Reveal the review worktree folder in the OS file manager (off the UI thread).
pub fn reveal_folder(project: &str, task: &str) -> Result<(), String> {
    let info = shelbi_orchestrator::review_session::review_open_info(project, task)
        .map_err(|e| e.to_string())?;
    if info.worktree.is_empty() {
        return Err("no review worktree to reveal".into());
    }
    let (prog, args) = crate::panel::reveal_command(crate::panel::current_os(), &info.worktree);
    crate::panel::spawn_opener(&prog, &args)
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
        PanelEffect::ShowDescription => ReviewAction::ShowDescription,
        PanelEffect::Approve => ReviewAction::Approve,
        PanelEffect::RejectPrompt => ReviewAction::Reject,
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::session::{ConnectFailure, Connected, RetryPolicy};
    use crossterm::event::KeyModifiers;
    use std::os::unix::net::UnixListener;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    /// A connector that never resolves — the content view stays Connecting, so
    /// these pure-routing tests touch no real session.
    struct NeverConnector;
    impl Connector for NeverConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            Err(ConnectFailure::Message("test: no session".into()))
        }
    }

    /// A connector that connects to a real Unix socket whose listener is gone, so
    /// every connect is **refused** — the "alive but not listening" zombie the
    /// review agent became. It maps the refused connect the way
    /// [`LiveConnector`](super::super::session::LiveConnector) does (a refused
    /// socket is "still starting"), so driving it exercises the real review
    /// content path's retry-then-give-up, not a hand-fed terminal error. Touches
    /// no real HOME / `~/.shelbi`: the socket lives in a tempdir.
    struct RefusingConnector {
        sock: PathBuf,
    }

    impl Connector for RefusingConnector {
        fn connect(&self, _p: &str, _t: &SessionRef) -> Result<Connected, ConnectFailure> {
            use shelbi_client::{ClientError, Connection};
            use shelbi_proto::capability;
            match Connection::open(&self.sock, None, capability::ALL) {
                Ok(_) => Err(ConnectFailure::Message("unexpected hello".into())),
                // A refused (or absent) socket is the transient "still starting"
                // signal the worker retries — exactly LiveConnector's mapping.
                Err(ClientError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                    ) =>
                {
                    Err(ConnectFailure::Starting(
                        "session `demo/ws/review-1` is starting (socket not up yet)".into(),
                    ))
                }
                Err(e) => Err(ConnectFailure::Message(e.to_string())),
            }
        }
    }

    /// Bind a Unix socket in a tempdir, then drop the listener so the path is
    /// left behind but every connect to it is refused — a stand-in for the
    /// zombie review agent (process up, socket refusing). Returns the tempdir
    /// (kept alive by the caller) and the socket path.
    fn refusing_socket() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let sock = dir.path().join("sock");
        let listener = UnixListener::bind(&sock).unwrap();
        drop(listener);
        (dir, sock)
    }

    /// A minimal task with a non-empty body so the panel shows the task-info
    /// section and the `More` link.
    fn sample_task() -> IssueFile {
        let ts = chrono::Utc::now();
        IssueFile {
            task: shelbi_core::Issue {
                id: "fix-login".into(),
                title: "Fix the login".into(),
                column: shelbi_core::Column::review(),
                priority: 1,
                assigned_to: Some("review-1".into()),
                workflow: None,
                branch: Some("jlong/fix-login".into()),
                depends_on: Vec::new(),
                prefers_machine: None,
                zen: None,
                launch: None,
                created_at: ts,
                updated_at: ts,
                params: std::collections::BTreeMap::new(),
            },
            body: "## Summary\n\nFix the broken login so sessions persist.".into(),
            tracker_assignees: Vec::new(),
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
            Some(sample_task()),
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
    fn m_key_opens_the_description_popover() {
        // `m` (the More link's keyboard twin) asks the shell to open the
        // task-description popover when the task has a body.
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Char('m'))), ReviewAction::ShowDescription);
        // The shell can reach the task to populate the popover.
        assert_eq!(it.task().map(|t| t.task.title.as_str()), Some("Fix the login"));
    }

    #[test]
    fn m_key_is_inert_for_a_bodyless_task() {
        let mut it = ReviewInterface::new(
            "proj",
            Arc::new(NeverConnector),
            "fix-login",
            "review-1",
            "/wt",
            "Vim",
            true,
            None,
        );
        assert_eq!(it.handle_key(key(KeyCode::Char('m'))), ReviewAction::None);
    }

    #[test]
    fn q_closes_when_not_merging() {
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Char('q'))), ReviewAction::Close);
    }

    #[test]
    fn esc_goes_back_and_leaves_the_review_loaded() {
        // Esc returns to the nav sidebar (Back) rather than tearing the review
        // down — it must not quit, and it is a distinct action from `q`'s Close.
        let mut it = iface();
        assert_eq!(it.handle_key(key(KeyCode::Esc)), ReviewAction::Back);
        assert!(!it.panel.should_quit, "Esc does not quit the review");
    }

    #[test]
    fn tab_toggles_focus_to_content_only_when_live() {
        let mut it = iface();
        // Content is still Connecting (NeverConnector), so Tab stays on panel.
        assert_eq!(it.handle_key(key(KeyCode::Tab)), ReviewAction::None);
        assert_eq!(it.focus, ReviewFocus::Panel);
    }

    #[test]
    fn focus_content_is_a_noop_until_the_content_is_live() {
        // `Ctrl+L` targets the content view, but — like Tab — it only lands
        // there when a content session is live. NeverConnector keeps it
        // Connecting, so focus stays on the panel.
        let mut it = iface();
        it.focus_content();
        assert_eq!(it.focus, ReviewFocus::Panel);
        it.focus_panel();
        assert_eq!(it.focus, ReviewFocus::Panel);
    }

    #[test]
    fn focus_moves_are_inert_while_merging() {
        // The review is inert to competing input during a gated merge, so the
        // focus-move methods can't change focus mid-merge (AC #4 parity).
        let mut it = iface();
        it.set_merging(true);
        it.focus_content();
        assert_eq!(it.focus, ReviewFocus::Panel);
        it.focus_panel();
        assert_eq!(it.focus, ReviewFocus::Panel);
    }

    /// Drive `it.poll()` until the content view leaves Connecting, or the budget
    /// runs out. Returns whether it settled.
    fn poll_content_until_settled(it: &mut ReviewInterface) -> bool {
        for _ in 0..400 {
            it.poll();
            if !matches!(it.content_state(), MainState::Connecting(_)) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }

    #[test]
    fn a_refusing_content_socket_ends_in_failed_not_connecting_forever() {
        // The review agent's socket refuses every connect (the zombie): the
        // content view must retry for the deadline and then settle in Failed with
        // a clear message — it must never sit on "Connecting to …" indefinitely
        // (`rt-review-session-alive-but-not-listening`). Driven through the real
        // ReviewInterface content path, not SessionManager alone.
        let (_dir, sock) = refusing_socket();
        let mut it = ReviewInterface::new(
            "demo",
            Arc::new(RefusingConnector { sock }),
            "fix-login",
            "review-1",
            "/wt",
            "Vim",
            true,
            Some(sample_task()),
        );
        // Tighten the retry so the give-up is observable in well under a second.
        it.set_content_retry(RetryPolicy {
            deadline: Duration::from_millis(120),
            backoff: Duration::from_millis(10),
        });

        let start = Instant::now();
        assert!(
            poll_content_until_settled(&mut it),
            "the refusing content connect must settle, not hang on Connecting",
        );
        let elapsed = start.elapsed();

        match it.content_state() {
            MainState::Failed(_, err) => {
                assert!(
                    err.contains("starting") && err.contains("not reachable"),
                    "the failure names the give-up, got {err:?}",
                );
            }
            other => panic!("expected the content view to fail, got {:?}", state_label(other)),
        }
        assert!(
            elapsed < Duration::from_secs(1),
            "the give-up happened near the bound, not forever (took {elapsed:?})",
        );

        // Re-selecting Chat re-attempts: it goes back to Connecting (one more
        // retry round) rather than staying stuck on the failure.
        it.show_chat();
        assert!(
            matches!(it.content_state(), MainState::Connecting(_)),
            "re-selecting Chat re-arms the connect",
        );
    }

    /// A readable label for a non-Failed `MainState`, for test panics.
    fn state_label(s: MainState<'_>) -> &'static str {
        match s {
            MainState::Empty => "Empty",
            MainState::Connecting(_) => "Connecting",
            MainState::Live(_) => "Live",
            MainState::Idle(_) => "Idle",
            MainState::Failed(_, _) => "Failed",
        }
    }
}
