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
//! With `diff_tool: builtin` (the default) View Diff is Shelbi's own diff
//! ([`DiffView`]) instead of a session, and an external tool keeps the session
//! below.
//!
//! The panel's state machine is pure: a key/click yields a
//! [`PanelEffect`](crate::review_panel::PanelEffect) which this type maps to a
//! [`ReviewAction`] for the shell to carry out (switching the content session,
//! running the gated merge off-thread, opening the reject overlay, …). While a
//! gated merge is in flight the whole review is inert to competing input
//! (the panel declines Approve/Reject/quit; this also drops view switches and
//! Back) so a multi-second `gh` merge can never be raced — plan AC #4.
//!
//! **View Diff waits for its diff** (`review-diff-loading-state`): the shared
//! [`DiffLoad`] retries an `Ensure` the daemon refuses because the worktree is
//! mid-load, and the view reads "Loading diff…" until the tool has drawn,
//! rather than falling back to the Reviewer. After 15s it says why it couldn't,
//! with a Retry (`r`, a click, or selecting View Diff again).

use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::Frame;

use shelbi_app::review::diff_load::{
    DiffLoad, DiffLoadView, DiffSeen, DIFF_FAILED_TITLE, DIFF_RETRY_LABEL, LOADING_DIFF,
};
use shelbi_app::{ReviewRole, ReviewSessionOp};
use shelbi_state::IssueFile;
use shelbi_term::Size;

use crate::review_panel::{render_full, PanelEffect, ReviewPanel};

use super::diff_view::{DiffOpener, DiffOut, DiffSetup, DiffView};
use super::session::{Connector, MainState, SessionManager, SessionRef};
use crate::theme::palette;
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
    /// `gY` in the diff: Reject, its reason starting from the comments.
    RejectWith(String),
    /// Open the review worktree (the editor) or its diff in the app chosen
    /// for the role; the content view stays as it is.
    OpenApp(ReviewRole),
    /// Open the configured review URL in the system browser.
    OpenBrowser,
    /// Reveal the review worktree folder in the OS file manager.
    RevealFolder,
    /// Open the task's pull request in the system browser.
    OpenPr,
    /// Open the full task-description popover over the main area (the `More`
    /// link / `m` key). The shell opens the overlay; the content session keeps
    /// running underneath.
    ShowDescription,
    /// Back button: return to the orchestrator chat, leaving the review loaded.
    Back,
    /// Close the review (q / Esc): tear the interface down (the shell asks the
    /// daemon to end the editor/diff/server sessions and free the port).
    Close,
    /// The diff's `Ctrl-g`: open this file in the user's editor at this line.
    OpenFile { path: std::path::PathBuf, line: Option<u32> },
}

/// Asks the daemon for the diff session. Runs off the UI thread.
type DiffEnsure = Arc<dyn Fn() -> Result<(), String> + Send + Sync>;

/// The live review interface for one task.
pub struct ReviewInterface {
    panel: ReviewPanel,
    /// The content terminal view (chat / diff / editor), its own connection so
    /// it coexists with the panel. Reuses all the main-area session machinery.
    content: SessionManager,
    task_id: String,
    slot: String,
    focus: ReviewFocus,
    /// A click asked for the content (its sidebar row, a panel view): it
    /// takes the keys once it is live, unless they move first.
    content_wanted: bool,
    /// The reviewed task, when resolvable — the source of the panel's title and
    /// description preview, and the data the `More` description popover shows.
    /// `None` only if the issue store couldn't produce it (the panel then shows
    /// no task info and `More` is inert).
    task: Option<IssueFile>,
    /// The main area shows View Diff (its wait, or its terminal).
    diff_active: bool,
    /// View Diff's wait for its diff, once it was asked for.
    diff_load: Option<DiffLoad>,
    /// The answer to the ensure in flight. Its own channel, not the shell's
    /// one job slot, so another view's job can't lose an answer the wait is
    /// counting on.
    diff_answer: Option<Receiver<Result<(), String>>>,
    diff_ensure: DiffEnsure,
    /// What the wait last drew, so the shell redraws when that changes.
    diff_drawn: Option<DiffLoadView>,
    /// Where the failure's Retry was last drawn.
    retry_rect: Option<Rect>,
    project: String,
    /// Shelbi's own diff, once View Diff has shown it. While `diff_active` and
    /// this is set, it's what the main area shows: no wait, no session.
    builtin: Option<DiffView>,
    diff_setup: DiffSetup,
    /// A test's session in place of the worktree's.
    diff_opener: Option<DiffOpener>,
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
        let mut panel = ReviewPanel::new(worktree, editor_name, has_review_url, title, description);
        panel.pr = task.as_ref().and_then(|t| shelbi_app::board::task_pr(&t.task));
        let mut content = SessionManager::new(project, connector).with_relauncher(Arc::new(
            shelbi_app::session::ReviewRelauncher {
                project: project.to_string(),
                task: task_id.clone(),
                link: None,
            },
        ));
        // Chat is the default view: bind to the review agent's workspace session.
        content.show(SessionRef::Workspace(slot.clone()));
        let diff_ensure: DiffEnsure = {
            let (project, task) = (project.to_string(), task_id.clone());
            Arc::new(move || {
                let op = ReviewSessionOp::Ensure { role: ReviewRole::Diff };
                shelbi_app::review_session(&project, &task, op, &mut |_, _| {})
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
        };
        Self {
            panel,
            content,
            task_id,
            slot,
            focus: ReviewFocus::Panel,
            content_wanted: false,
            task,
            diff_active: false,
            diff_load: None,
            diff_answer: None,
            diff_ensure,
            diff_drawn: None,
            retry_rect: None,
            project: project.to_string(),
            builtin: None,
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
        if let Some(diff) = self.builtin.as_mut() {
            diff.set_setup(setup.clone());
        }
        self.diff_setup = setup;
    }

    /// Answer View Diff's ensures here instead of asking the daemon (test seam).
    #[cfg(test)]
    pub(crate) fn set_diff_ensure(&mut self, ensure: DiffEnsure) {
        self.diff_ensure = ensure;
    }

    /// Open `opener`'s session instead of the worktree's (test seam).
    #[cfg(test)]
    pub(crate) fn set_diff_opener(&mut self, opener: DiffOpener) {
        self.diff_opener = Some(opener);
    }

    /// Show Shelbi's own diff, opening it the first time, with the keyboard on
    /// it: selecting it means reading it.
    fn show_builtin_diff(&mut self) {
        if self.builtin.is_none() {
            let opener = self.diff_opener.clone().unwrap_or_else(|| DiffView::review_opener(&self.project, &self.slot, &self.task_id));
            let worktree = Some(self.panel.worktree.clone());
            self.builtin = Some(DiffView::new(opener, self.diff_setup.clone(), worktree, "No changes on this branch"));
        }
        // The setting moved from an external tool: its wait mustn't go on to
        // bind a session behind this.
        self.diff_load = None;
        self.diff_answer = None;
        self.diff_active = true;
        self.focus = ReviewFocus::Content;
    }

    /// The diff on show, when it's the one on show.
    fn showing_builtin(&mut self) -> Option<&mut DiffView> {
        self.builtin.as_mut().filter(|_| self.diff_active)
    }

    /// Whether the diff has the keyboard and takes `k` ahead of the shell's own
    /// keys (`Ctrl-p`, the focus moves it rebinds).
    pub fn diff_claims(&self, k: &KeyEvent) -> bool {
        self.focus == ReviewFocus::Content
            && self.diff_active
            && self.builtin.as_ref().is_some_and(|diff| diff.claims(k))
    }

    /// The review slot the task is loaded on.
    pub fn slot(&self) -> &str {
        &self.slot
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

    /// Pump the content view (test accessor): the shell's loop, without the wait.
    #[cfg(test)]
    pub(crate) fn poll_content_for_test(&mut self, ring: &mut bool) {
        self.content.poll();
        self.content.pump_output(ring);
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
        self.leave_diff();
        self.content.show(SessionRef::Workspace(self.slot.clone()));
    }

    /// Bind the content view to a daemon-spawned role session. The caller has
    /// already ensured it exists (the daemon's half). Re-selecting the same role
    /// while it is the current but failed target re-attaches.
    pub fn show_role(&mut self, role: ReviewRole) {
        self.leave_diff();
        self.content.show(Self::role_target(&self.slot, role));
    }

    /// The editor's ensure answered, for a switch to it that may be stale:
    /// View Diff selected since it was asked for keeps the main area.
    pub fn editor_ready(&mut self, result: Result<(), String>) {
        match result {
            Ok(()) if !self.diff_active => self.show_role(ReviewRole::Editor),
            Ok(()) => {}
            Err(e) => {
                self.set_status(format!("open view failed: {e}"));
                if !self.diff_active {
                    self.show_chat();
                }
            }
        }
    }

    fn role_target(slot: &str, role: ReviewRole) -> SessionRef {
        SessionRef::Review {
            slot: slot.to_string(),
            role: role.as_str().to_string(),
        }
    }

    /// Show View Diff: start or resume the wait for the diff.
    pub fn show_diff(&mut self) {
        self.show_diff_at(Instant::now());
    }

    fn show_diff_at(&mut self, now: Instant) {
        if self.panel.apps.builtin_diff {
            self.show_builtin_diff();
            return;
        }
        // The setting moved to an external tool: that's what shows from now on.
        self.builtin = None;
        self.diff_active = true;
        let seen = self.diff_seen();
        if DiffLoad::select(&mut self.diff_load, &seen, now) {
            self.send_diff_ensure();
        }
        self.drive_diff(now);
    }

    /// Retry: start the wait for the diff over.
    fn retry_diff(&mut self, now: Instant) {
        if self.diff_load.as_mut().is_some_and(|load| load.retry(now)) {
            self.send_diff_ensure();
        }
        self.drive_diff(now);
    }

    /// The pane is going to another view; the wait goes on behind it.
    fn leave_diff(&mut self) {
        self.diff_active = false;
        if let Some(load) = self.diff_load.as_mut() {
            load.left();
        }
    }

    fn send_diff_ensure(&mut self) {
        let (tx, rx) = mpsc::channel();
        self.diff_answer = Some(rx);
        let ensure = self.diff_ensure.clone();
        let spawned = std::thread::Builder::new()
            .name("shelbi-review-ensure".into())
            .spawn(move || {
                let _ = tx.send(ensure());
            });
        if let (Err(e), Some(load)) = (spawned, self.diff_load.as_mut()) {
            self.diff_answer = None;
            load.ensure_answered(Err(e.to_string()), Instant::now());
        }
    }

    /// What the pane shows of the diff session.
    fn diff_seen(&self) -> DiffSeen {
        let diff = Self::role_target(&self.slot, ReviewRole::Diff);
        if self.content.current_target() != Some(&diff) {
            return DiffSeen::NotYet;
        }
        match self.content.state() {
            MainState::Live(pane) if pane.has_drawn() => DiffSeen::Drawn,
            MainState::Failed(_, why) | MainState::GaveUp(why) => DiffSeen::Ended(why.to_string()),
            MainState::Idle(_) => DiffSeen::Ended("the diff session isn't running".to_string()),
            _ => DiffSeen::NotYet,
        }
    }

    /// One step of the wait: take the ensure's answer, send a retry that's
    /// due, attach a session that's up, and note what the pane shows. Returns
    /// whether what the view draws changed.
    fn drive_diff(&mut self, now: Instant) -> bool {
        let Some(load) = self.diff_load.as_mut() else {
            return false;
        };
        if let Some(rx) = &self.diff_answer {
            match rx.try_recv() {
                Ok(result) => {
                    load.ensure_answered(result, now);
                    self.diff_answer = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    load.ensure_answered(Err("the daemon request ended without an answer".into()), now);
                    self.diff_answer = None;
                }
            }
        }
        let send = load.take_due_ensure(now);
        let attach = self.diff_active && load.needs_attach();
        if attach {
            load.mark_attached();
        }
        if send {
            self.send_diff_ensure();
        }
        if attach {
            self.content.show(Self::role_target(&self.slot, ReviewRole::Diff));
        }
        let seen = if self.diff_active { self.diff_seen() } else { DiffSeen::NotYet };
        let Some(load) = self.diff_load.as_mut() else {
            return false;
        };
        load.observe(&seen, now);
        let drawn = load.view(now);
        if self.diff_drawn.as_ref() == Some(&drawn) {
            return false;
        }
        self.diff_drawn = Some(drawn);
        true
    }

    /// What View Diff's wait draws in place of the terminal now, if anything.
    /// Before "Loading" is due (a ready diff is up by then) a pane already bound
    /// to the session is blank, not "Connecting…"; one not bound yet still shows
    /// the view it came from.
    fn diff_cover(&self, now: Instant) -> Option<DiffLoadView> {
        if !self.diff_active {
            return None;
        }
        let load = self.diff_load.as_ref()?;
        // Asked of the pane, not the wait's last look: a drawn diff shows now.
        if self.diff_seen() == DiffSeen::Drawn {
            return None;
        }
        match load.view(now) {
            DiffLoadView::Ready => None,
            DiffLoadView::Settling if !load.is_attached() => None,
            view => Some(view),
        }
    }

    /// Whether the terminal is hidden behind the wait: input isn't for it yet.
    fn diff_covered(&self) -> bool {
        self.diff_cover(Instant::now()).is_some()
    }

    /// Drive the busy spinner while a merge is in flight / set the state.
    pub fn set_merging(&mut self, on: bool) {
        self.panel.merging = on;
        if on {
            // The merge holds the review still: no focus lands mid-merge.
            self.content_wanted = false;
            self.panel.spinner = 0;
            self.panel.status_line.clear();
        }
    }

    /// The panel's status line, for the shell's tests.
    #[cfg(test)]
    pub fn status_line(&self) -> &str {
        &self.panel.status_line
    }

    pub fn is_merging(&self) -> bool {
        self.panel.merging
    }

    pub fn tick_spinner(&mut self) {
        if self.panel.merging {
            self.panel.spinner = self.panel.spinner.wrapping_add(1);
        }
    }

    /// Follow the task's sidebar row: the panel's server line shows its
    /// sub-state and location. Returns whether anything changed.
    pub fn set_server(&mut self, row: Option<&shelbi_app::view::ReviewRow>) -> bool {
        let server = row.map(|r| (r.state, r.location.clone()));
        let pr = row
            .and_then(|r| r.pr.clone())
            .or_else(|| self.task.as_ref().and_then(|t| shelbi_app::board::task_pr(&t.task)));
        if self.panel.server == server && self.panel.pr == pr {
            return false;
        }
        self.panel.server = server;
        // The description popover shows the same PR.
        if let Some(task) = self.task.as_mut() {
            task.task.pr = pr.clone();
        }
        self.panel.set_pr(pr);
        true
    }

    /// Where the panel's pull request opens, when it has one.
    pub fn pr_url(&self) -> Option<&str> {
        self.panel.pr_url()
    }

    /// Edit and View Diff open these in their apps (`editor:` /
    /// `diff_tool:` apps) instead of a pane.
    pub fn with_apps(mut self, apps: shelbi_state::AppTools) -> Self {
        self.panel.apps = apps;
        self
    }

    /// The editor and diff settings changed: "Edit in <name>" and which of
    /// Edit / View Diff open an app follow them.
    pub fn set_tools(&mut self, editor_name: String, apps: shelbi_state::AppTools) {
        self.panel.editor_name = editor_name;
        self.panel.apps = apps;
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
        let changed = self.content.poll();
        let builtin = self.builtin.as_mut().is_some_and(DiffView::poll);
        let diff = self.drive_diff(Instant::now());
        self.settle_wanted_content() || diff || changed || builtin
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
        // `r` on the failure is its Retry, wherever focus sits.
        if k.code == KeyCode::Char('r')
            && k.modifiers.is_empty()
            && !self.panel.merging
            && matches!(self.diff_cover(Instant::now()), Some(DiffLoadView::Failed { .. }))
        {
            self.retry_diff(Instant::now());
            return ReviewAction::None;
        }
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
        self.keys_to_panel();
    }

    /// The panel takes the keys, and a click's wait for the content is over.
    fn keys_to_panel(&mut self) {
        self.focus = ReviewFocus::Panel;
        self.content_wanted = false;
    }

    /// Whether the content can take the keys: Shelbi's diff, or a live session
    /// no wait covers.
    fn content_focusable(&mut self) -> bool {
        self.showing_builtin().is_some()
            || (matches!(self.content.state(), MainState::Live(_)) && !self.diff_covered())
    }

    /// A click opened this review or one of its views: the content takes the
    /// keys as soon as it is live ([`Self::poll`] lands them), unless they move
    /// first. The keyboard's Enter leaves them on the panel. Inert mid-merge.
    pub fn want_content(&mut self) {
        if !self.panel.merging {
            self.content_wanted = true;
        }
    }

    /// Whether a click's wish for the content is waiting, for the shell's tests.
    #[cfg(test)]
    pub fn wants_content(&self) -> bool {
        self.content_wanted
    }

    /// Land a click's wanted focus once the content can take it. Whether focus
    /// moved.
    fn settle_wanted_content(&mut self) -> bool {
        if !self.content_wanted || self.panel.merging || !self.content_focusable() {
            return false;
        }
        self.content_wanted = false;
        let moved = self.focus != ReviewFocus::Content;
        self.focus = ReviewFocus::Content;
        moved
    }

    /// Move focus to the content view (the shell's `FocusMain` / Ctrl+L),
    /// but only when a content session is live — the same guard Tab uses, so
    /// focus never lands on an empty/connecting content pane. Inert while a
    /// gated merge runs.
    pub fn focus_content(&mut self) {
        if self.panel.merging {
            return;
        }
        if self.content_focusable() {
            self.focus = ReviewFocus::Content;
        }
    }

    /// Whether the content view currently holds focus. For the shell's focus
    /// routing and tests.
    #[cfg(test)]
    pub fn content_focused(&self) -> bool {
        matches!(self.focus, ReviewFocus::Content)
    }

    /// Deliver a bracketed paste (a drag-and-drop lands as a paste of the
    /// dropped path) to the content session. Focus moves to the content view
    /// when it is live so the reviewer can keep typing in the agent/editor
    /// after the drop; the paste is best-effort and reaches only a live session.
    /// Inert while a gated merge runs — the review takes no competing input then
    /// (AC #4 parity with the focus-move methods).
    pub fn paste_into_content(&mut self, text: &str) {
        if self.panel.merging || self.diff_covered() {
            return;
        }
        self.focus_content();
        if let Some(diff) = self.showing_builtin() {
            diff.paste(text);
            return;
        }
        self.content.send_paste(text);
    }

    fn handle_content_key(&mut self, k: KeyEvent) -> ReviewAction {
        if let Some(diff) = self.showing_builtin() {
            return match diff.key(k) {
                DiffOut::None => ReviewAction::None,
                DiffOut::FocusPanel => {
                    self.keys_to_panel();
                    ReviewAction::None
                }
                DiffOut::Edit { path, line } => ReviewAction::OpenFile { path, line },
                DiffOut::Reject { reason } => ReviewAction::RejectWith(reason),
            };
        }
        // Tab returns focus to the panel; everything else goes to the agent.
        if k.code == KeyCode::Tab && k.modifiers.is_empty() {
            self.keys_to_panel();
            return ReviewAction::None;
        }
        if self.diff_covered() {
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
        // view switches and the focus toggle so nothing re-enters mid-merge
        // (AC #4). Esc still leaves: a review reopened mid-merge never holds
        // the user. The spinner still advances on the loop tick.
        if self.panel.merging {
            return if k.code == KeyCode::Esc {
                ReviewAction::Back
            } else {
                ReviewAction::None
            };
        }
        match k.code {
            // Tab moves focus to the content view (so the reviewer can type in
            // the agent / editor) when one is live.
            KeyCode::Tab if k.modifiers.is_empty() => {
                self.focus_content();
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
            if in_panel {
                self.keys_to_panel();
            } else {
                self.focus = ReviewFocus::Content;
                self.content_wanted = false;
            }
        }
        if in_panel {
            if self.panel.merging {
                let back = matches!(m.kind, MouseEventKind::Down(MouseButton::Left))
                    && self.panel.back_hit(m.column, m.row);
                return if back { ReviewAction::Back } else { ReviewAction::None };
            }
            if let MouseEventKind::Down(MouseButton::Left) = m.kind {
                let effect = self.panel.click(m.column, m.row);
                // A view picked with the mouse is ready for the keyboard: its
                // content takes the keys once it is live.
                if matches!(effect, PanelEffect::ShowChat | PanelEffect::ShowDiff | PanelEffect::ShowEditor) {
                    self.want_content();
                }
                return map_effect(effect);
            }
            return ReviewAction::None;
        }
        if let Some(diff) = self.showing_builtin() {
            diff.mouse(m);
            return ReviewAction::None;
        }
        // The wait's words are over the content: its Retry takes the click, and
        // nothing reaches a terminal that isn't showing.
        if self.diff_covered() {
            let on_retry = self.retry_rect.is_some_and(|r| contains(r, m.column, m.row));
            if on_retry && matches!(m.kind, MouseEventKind::Down(MouseButton::Left)) {
                self.retry_diff(Instant::now());
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
        self.retry_rect = None;
        let keyboard = focus_main && self.focus == ReviewFocus::Content;
        if let Some(diff) = self.showing_builtin() {
            return diff.render(buf, content_rect, truecolor, keyboard);
        }
        match self.diff_cover(Instant::now()) {
            Some(DiffLoadView::Loading) => {
                super::render_placeholder(buf, content_rect, LOADING_DIFF);
                return None;
            }
            Some(DiffLoadView::Failed { cause }) => {
                self.retry_rect = render_diff_failed(buf, content_rect, &cause);
                return None;
            }
            Some(DiffLoadView::Settling | DiffLoadView::Ready) => return None,
            None => {}
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
            // The review agent's chat session should always be live; if it isn't
            // up yet, treat it like "no session here" rather than the dev-slot
            // idle placeholder (which doesn't fit a review content pane).
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
                if focus_main && self.focus == ReviewFocus::Content {
                    cursor = cur;
                }
            }
        }
        cursor
    }
}

/// The failed diff, centered in `area`: the heading, why, and the Retry. Returns
/// where the Retry landed, when there was room for it.
fn render_diff_failed(buf: &mut ratatui::buffer::Buffer, area: Rect, cause: &str) -> Option<Rect> {
    let width = area.width.saturating_sub(4);
    if width == 0 || area.height == 0 {
        return None;
    }
    let cause = wrap_words(cause, usize::from(width));
    let height = (cause.len() as u16).saturating_add(4).min(area.height);
    let top = area.y + (area.height - height) / 2;
    let mut lines = vec![Line::styled(
        DIFF_FAILED_TITLE,
        Style::default().add_modifier(Modifier::BOLD),
    )];
    let muted = Style::default().fg(palette().ansi_dark_gray);
    lines.extend(cause.into_iter().map(|l| Line::styled(l, muted)));
    let retry_row = top + lines.len() as u16 + 1;
    Paragraph::new(lines)
        .alignment(Alignment::Center)
        .render(Rect::new(area.x + 2, top, width, height), buf);

    if retry_row >= area.bottom() {
        return None;
    }
    let label = format!(" {DIFF_RETRY_LABEL} (r) ");
    let label_width = (label.chars().count() as u16).min(width);
    let rect = Rect::new(area.x + (area.width - label_width) / 2, retry_row, label_width, 1);
    Paragraph::new(label)
        .style(Style::default().add_modifier(Modifier::REVERSED))
        .render(rect, buf);
    Some(rect)
}

/// `text` broken at spaces to fit `width` columns (a longer word is cut).
fn wrap_words(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut word: Vec<char> = word.chars().collect();
        if !line.is_empty() && line.chars().count() + 1 + word.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        while word.len() > width {
            lines.push(word.drain(..width).collect());
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.extend(word);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
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
        PanelEffect::Back => ReviewAction::Back,
        PanelEffect::ShowChat => ReviewAction::ShowContent(None),
        PanelEffect::ShowDiff => ReviewAction::ShowContent(Some(ReviewRole::Diff)),
        PanelEffect::ShowEditor => ReviewAction::ShowContent(Some(ReviewRole::Editor)),
        PanelEffect::OpenApp(role) => ReviewAction::OpenApp(role),
        PanelEffect::OpenBrowser => ReviewAction::OpenBrowser,
        PanelEffect::OpenPr => ReviewAction::OpenPr,
        PanelEffect::RevealFolder => ReviewAction::RevealFolder,
        PanelEffect::ShowDescription => ReviewAction::ShowDescription,
        PanelEffect::Approve => ReviewAction::Approve,
        PanelEffect::RejectPrompt => ReviewAction::Reject,
        // The TUI handles `q` itself (`request_quit`); the shared table's Close
        // means the same teardown.
        PanelEffect::Close => ReviewAction::Close,
        // The TUI's panel has no user-shell row (the desktop's), so this
        // never comes out of it.
        PanelEffect::ToggleShell => ReviewAction::None,
    }
}

fn contains(area: Rect, x: u16, y: u16) -> bool {
    x >= area.left() && x < area.right() && y >= area.top() && y < area.bottom()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shell::session::{ConnectFailure, Connected, RetryPolicy};
    use shelbi_app::review::diff_load::DIFF_LOAD_TIMEOUT;
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
                pr: None,
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

    /// Attaches the diff to a real session running `cat`: blank until something
    /// is typed at it, so a test decides when the "tool" draws. Everything else
    /// (the Reviewer) has no session.
    struct CatDiff {
        sock: PathBuf,
    }

    impl Connector for CatDiff {
        fn connect(&self, _p: &str, target: &SessionRef) -> Result<Connected, ConnectFailure> {
            use shelbi_client::Connection;
            if !matches!(target, SessionRef::Review { .. }) {
                return Err(ConnectFailure::Message("no session".into()));
            }
            let (conn, events) = Connection::open(&self.sock, None, shelbi_proto::capability::ALL)
                .map_err(|e| ConnectFailure::Message(e.to_string()))?;
            conn.attach(None).map_err(|e| ConnectFailure::Message(e.to_string()))?;
            let size = shelbi_app::session::GridSize::new(80, 24);
            Ok(Connected { conn, events, size })
        }
    }

    /// The daemon's half, scripted: counts the asks and answers `fail` while set.
    #[derive(Default)]
    struct FakeEnsure {
        asks: std::sync::atomic::AtomicUsize,
        refusal: std::sync::Mutex<Option<String>>,
    }

    impl FakeEnsure {
        fn asks(&self) -> usize {
            self.asks.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn refuse(&self, why: Option<&str>) {
            *self.refusal.lock().unwrap() = why.map(str::to_string);
        }
    }

    fn with_ensure(mut it: ReviewInterface) -> (ReviewInterface, Arc<FakeEnsure>) {
        let fake = Arc::new(FakeEnsure::default());
        let f = fake.clone();
        it.diff_ensure = Arc::new(move || {
            f.asks.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match f.refusal.lock().unwrap().clone() {
                Some(why) => Err(why),
                None => Ok(()),
            }
        });
        (it, fake)
    }

    /// Wait for the outstanding ensure thread's answer and take it, at `now`.
    fn settle_ensure(it: &mut ReviewInterface, now: Instant) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while it.diff_answer.is_some() && Instant::now() < deadline {
            it.drive_diff(now);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(it.diff_answer.is_none(), "the ensure answered");
    }

    /// Pump the content and the wait until `done`, at the wait's time `now`.
    fn pump_until(it: &mut ReviewInterface, now: Instant, done: impl Fn(&ReviewInterface) -> bool) -> bool {
        let mut ring = false;
        for _ in 0..1500 {
            it.poll_content_for_test(&mut ring);
            it.drive_diff(now);
            if done(it) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    const SECOND: Duration = Duration::from_secs(1);

    fn render_main(it: &mut ReviewInterface) -> String {
        let backend = ratatui::backend::TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                it.render(f, Rect::new(0, 0, 28, 20), Rect::new(28, 0, 72, 20), false, true);
            })
            .unwrap();
        let buf = terminal.backend().buffer();
        (0..20)
            .map(|y| (28..100).map(|x| buf[(x, y)].symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_ready_diff_opens_with_no_loading_flash() {
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = crate::shell::session::tests::spawn_cat_session(home.path(), "proj/review/review-1/diff");
        let (mut it, ensure) = with_ensure(ReviewInterface::new(
            "proj", Arc::new(CatDiff { sock }), "fix-login", "review-1", "/wt", "Vim", true, None,
        ));
        let t0 = Instant::now();
        it.show_diff_at(t0);
        settle_ensure(&mut it, t0);
        assert!(pump_until(&mut it, t0, |i| matches!(i.content.state(), MainState::Live(_))));
        // Attached inside the grace, and the tool draws: Loading is never drawn.
        assert_eq!(it.diff_load.as_ref().unwrap().view(t0), DiffLoadView::Settling);
        assert!(!render_main(&mut it).contains(LOADING_DIFF));
        it.content.send_input(b"diff --git a/x b/x");
        assert!(pump_until(&mut it, t0, |i| i.diff_seen() == DiffSeen::Drawn));
        assert_eq!(it.diff_load.as_ref().unwrap().view(t0), DiffLoadView::Ready);
        assert!(!it.diff_covered());
        assert_eq!(ensure.asks(), 1);

        // Selected again with the pane showing it: nothing to do, no ensure.
        it.show_diff_at(t0 + SECOND);
        settle_ensure(&mut it, t0 + SECOND);
        assert_eq!(ensure.asks(), 1);
        assert!(!it.diff_covered());
    }

    #[test]
    fn a_worktree_not_ready_loads_then_retries_into_the_diff() {
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = crate::shell::session::tests::spawn_cat_session(home.path(), "proj/review/review-1/diff");
        let (mut it, ensure) = with_ensure(ReviewInterface::new(
            "proj", Arc::new(CatDiff { sock }), "fix-login", "review-1", "/wt", "Vim", true, None,
        ));
        ensure.refuse(Some("the review worktree isn't checked out yet"));
        let t0 = Instant::now();
        it.show_diff_at(t0);
        settle_ensure(&mut it, t0);

        // The refusal is no error: no status, no fallback, Loading.
        let t1 = t0 + SECOND;
        it.drive_diff(t1);
        assert_eq!(it.panel.status_line, "");
        assert!(it.diff_active);
        assert_eq!(it.diff_load.as_ref().unwrap().view(t1), DiffLoadView::Loading);
        assert!(ensure.asks() >= 1);
        settle_ensure(&mut it, t1);

        // The worktree comes ready; the retry attaches the session.
        ensure.refuse(None);
        let t2 = t0 + 3 * SECOND;
        it.drive_diff(t2);
        settle_ensure(&mut it, t2);
        assert!(pump_until(&mut it, t2, |i| matches!(i.content.state(), MainState::Live(_))));
        assert_eq!(it.diff_load.as_ref().unwrap().view(t2), DiffLoadView::Loading, "attached, not drawn");

        // The tool draws: the terminal replaces Loading, with no key.
        it.content.send_input(b"diff --git a/x b/x");
        assert!(pump_until(&mut it, t2, |i| i.diff_seen() == DiffSeen::Drawn));
        assert!(!it.diff_covered());
    }

    #[test]
    fn a_persistent_failure_times_out_with_the_cause_and_retries_on_r() {
        let (mut it, ensure) = with_ensure(iface());
        ensure.refuse(Some("git merge-base failed"));
        let t0 = Instant::now();
        it.show_diff_at(t0);
        settle_ensure(&mut it, t0);

        let load = |it: &ReviewInterface, at| it.diff_load.as_ref().unwrap().view(at);
        let almost = t0 + DIFF_LOAD_TIMEOUT - Duration::from_millis(1);
        it.drive_diff(almost);
        settle_ensure(&mut it, almost);
        assert_eq!(load(&it, almost), DiffLoadView::Loading, "no error before the timeout");

        let up = t0 + DIFF_LOAD_TIMEOUT;
        it.drive_diff(up);
        assert_eq!(load(&it, up), DiffLoadView::Failed { cause: "git merge-base failed".into() });
        assert_eq!(it.panel.status_line, "", "the cause is in the view");
        assert!(it.diff_active, "and it didn't fall back");
        let asks = ensure.asks();

        // `r` starts it over. (On the real clock, so it reads the view that way.)
        ensure.refuse(None);
        it.diff_load = Some({
            let mut l = DiffLoad::start(Instant::now() - DIFF_LOAD_TIMEOUT);
            l.ensure_answered(Err("git merge-base failed".into()), Instant::now());
            l
        });
        it.diff_answer = None;
        let drawn = render_main(&mut it);
        assert!(drawn.contains(DIFF_FAILED_TITLE), "{drawn}");
        assert!(drawn.contains("git merge-base failed"), "{drawn}");
        assert!(drawn.contains(DIFF_RETRY_LABEL), "{drawn}");
        assert_eq!(it.handle_key(key(KeyCode::Char('r'))), ReviewAction::None);
        settle_ensure(&mut it, Instant::now());
        assert_eq!(ensure.asks(), asks + 1);
        assert!(it.diff_load.as_ref().unwrap().needs_attach() || it.diff_load.as_ref().unwrap().is_attached());
    }

    #[test]
    fn clicking_retry_starts_the_wait_over() {
        let (mut it, ensure) = with_ensure(iface());
        it.diff_active = true;
        it.diff_load = Some({
            let mut l = DiffLoad::start(Instant::now() - DIFF_LOAD_TIMEOUT);
            l.ensure_answered(Err("nope".into()), Instant::now());
            l
        });
        let _ = render_main(&mut it);
        let at = it.retry_rect.expect("the Retry is drawn");
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: at.x + 1,
            row: at.y,
            modifiers: KeyModifiers::empty(),
        };
        it.handle_mouse(click, Rect::new(0, 0, 28, 20), Rect::new(28, 0, 72, 20));
        settle_ensure(&mut it, Instant::now());
        assert_eq!(ensure.asks(), 1);
    }

    #[test]
    fn switching_away_and_back_mid_load_adds_no_ensure() {
        let (mut it, ensure) = with_ensure(iface());
        ensure.refuse(Some("not yet"));
        let t0 = Instant::now();
        it.show_diff_at(t0);
        it.show_chat();
        it.show_diff_at(t0);
        settle_ensure(&mut it, t0);
        assert_eq!(ensure.asks(), 1, "the one in flight serves the wait");

        // During the backoff too.
        it.show_chat();
        it.show_diff_at(t0);
        settle_ensure(&mut it, t0);
        assert_eq!(ensure.asks(), 1);
    }

    #[test]
    fn a_stale_editor_answer_doesnt_take_the_pane_from_view_diff() {
        let (mut it, _ensure) = with_ensure(iface());
        it.show_diff_at(Instant::now());
        it.editor_ready(Ok(()));
        assert!(it.diff_active);
        it.editor_ready(Err("no".into()));
        assert!(it.diff_active);
        assert_eq!(it.panel.status_line, "open view failed: no");
    }

    #[test]
    fn keys_and_focus_dont_reach_a_terminal_behind_the_wait() {
        let (mut it, ensure) = with_ensure(iface());
        ensure.refuse(Some("not yet"));
        it.diff_active = true;
        it.diff_load = Some(DiffLoad::start(Instant::now() - SECOND));
        assert!(it.diff_covered());
        it.focus_content();
        assert!(!it.content_focused());
    }

    #[test]
    fn wrap_words_breaks_at_spaces_and_cuts_a_long_word() {
        assert_eq!(wrap_words("one two three", 7), vec!["one two", "three"]);
        assert_eq!(wrap_words("abcdefghij kl", 4), vec!["abcd", "efgh", "ij", "kl"]);
        assert!(wrap_words("   ", 4).is_empty());
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
    fn the_panel_shows_the_tasks_pr_and_its_row_opens_it() {
        use shelbi_core::{IssuePr, PrCi, PrState};
        let url = "https://github.com/o/r/pull/2137";
        let mut task = sample_task();
        task.task.pr = Some(IssuePr::new(2137, Some(url.into())));
        let mut it = ReviewInterface::new(
            "proj",
            Arc::new(NeverConnector),
            "fix-login",
            "review-1",
            "/wt",
            "Vim",
            true,
            Some(task),
        );
        assert_eq!(it.pr_url(), Some(url));

        // The sidebar row keeps it live: the panel and its popover follow.
        let row = shelbi_app::view::ReviewRow {
            task_id: "fix-login".into(),
            pr: Some(IssuePr {
                state: Some(PrState::Open),
                ci: Some(PrCi::Failing),
                ..IssuePr::new(2137, Some(url.into()))
            }),
            ..Default::default()
        };
        assert!(it.set_server(Some(&row)));
        assert!(!it.set_server(Some(&row)), "nothing new the second time");
        assert_eq!(it.panel.pr.as_ref().and_then(|p| p.ci), Some(PrCi::Failing));
        assert_eq!(it.task.as_ref().unwrap().task.pr.as_ref().and_then(|p| p.ci), Some(PrCi::Failing));

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 30)).unwrap();
        let area = Rect::new(0, 0, 60, 30);
        terminal.draw(|f| crate::review_panel::render_full(f, &mut it.panel, area)).unwrap();
        let buf = terminal.backend().buffer().clone();
        let lines: Vec<String> = (0..buf.area.height)
            .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol().to_string()).collect())
            .collect();
        let y = lines.iter().position(|l| l.contains("PR #2137 ↗")).expect("the PR line is drawn");
        assert!(lines[y].contains("open") && lines[y].contains("● checks failing"), "{}", lines[y]);

        // A click on it, and Enter once the keys reach it, both open it.
        assert_eq!(map_effect(it.panel.click(6, y as u16)), ReviewAction::OpenPr);
        it.panel.nav_up();
        it.panel.nav_down();
        assert_eq!(map_effect(it.panel.activate()), ReviewAction::OpenPr);
    }

    #[test]
    fn a_pr_arriving_while_the_panel_is_open_keeps_the_selection() {
        let mut it = iface();
        let before = map_effect(it.panel.activate());
        assert_eq!(before, ReviewAction::ShowContent(None), "opens on Chat");
        let row = shelbi_app::view::ReviewRow {
            task_id: "fix-login".into(),
            pr: Some(shelbi_core::IssuePr::new(7, Some("https://h/pull/7".into()))),
            ..Default::default()
        };
        assert!(it.set_server(Some(&row)));
        assert_eq!(map_effect(it.panel.activate()), before, "still on Chat, not the row above it");
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
        assert!(!it.panel.should_quit, "q cannot close during a merge");
        // Esc leaves it loaded: the merge doesn't hold the user here.
        assert_eq!(it.handle_key(key(KeyCode::Esc)), ReviewAction::Back);
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

    /// Attaches every target to one real session running `cat`, so the content
    /// goes live.
    struct CatAll {
        sock: PathBuf,
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
    fn poll_until(it: &mut ReviewInterface, done: impl Fn(&ReviewInterface) -> bool) -> bool {
        let mut ring = false;
        for _ in 0..1500 {
            it.poll();
            it.pump_output(&mut ring);
            if done(it) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn a_click_on_a_view_lands_the_keys_on_its_content_once_live() {
        // nav-click-focuses-content: a mouse click on a panel view hands the
        // keys to its content (Enter keeps them on the panel); a merge holds
        // them still.
        let _lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::tempdir().unwrap();
        let (sock, _cleanup) = crate::shell::session::tests::spawn_cat_session(home.path(), "proj/ws/review-1");
        let mut it = ReviewInterface::new(
            "proj", Arc::new(CatAll { sock }), "fix-login", "review-1", "/wt", "Vim", true, Some(sample_task()),
        );
        let (panel, content) = (Rect::new(0, 0, 40, 30), Rect::new(40, 0, 60, 30));
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        terminal.draw(|f| { it.render(f, panel, content, false, true); }).unwrap();
        let buf = terminal.backend().buffer().clone();
        let rows: Vec<String> = (0..30).map(|y| (0..40).map(|x| buf[(x, y)].symbol()).collect()).collect();
        let chat_y = rows.iter().position(|r| r.contains("Reviewer")).expect("the chat's row") as u16;
        let col = rows[chat_y as usize].find("Reviewer").unwrap() as u16;

        // A merge holds the review still: the click's wish doesn't land.
        it.set_merging(true);
        it.want_content();
        assert!(poll_until(&mut it, |i| matches!(i.content_state(), MainState::Live(_))));
        it.poll();
        assert!(!it.content_focused(), "no focus lands mid-merge");
        it.set_merging(false);

        // The click on Chat (already shown): the keys move to its terminal.
        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row: chat_y,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(it.handle_mouse(click, panel, content), ReviewAction::ShowContent(None));
        assert!(poll_until(&mut it, |i| i.content_focused()), "the chat's terminal took the keys");

        // Enter on a view keeps the keys on the panel.
        it.focus_panel();
        it.handle_key(key(KeyCode::Up));
        it.handle_key(key(KeyCode::Enter));
        for _ in 0..20 {
            it.poll();
        }
        assert!(!it.content_focused(), "the keyboard moves on through the panel");
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
            MainState::Restarting(..) => "Restarting",
            MainState::GaveUp(..) => "GaveUp",
        }
    }
}
