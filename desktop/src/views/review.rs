//! The Review view (`dt-review-view`): a ready-for-review task's review
//! interface, the native review panel beside a content terminal.
//!
//! The panel takes the navigation sidebar's column while a review is open
//! (`dt-review-replaces-sidebar`, as the TUI's panel replaces its sidebar):
//! the view renders as the panel, at whatever width the window's column
//! gives it, and the window puts [`ReviewView::render_main`] (the content,
//! and the user shell beside it when open) in the main area beside it.
//!
//! It follows the TUI's review interface (`crates/shelbi-tui/src/shell/review.rs`,
//! `review_panel.rs`):
//!
//! - **The panel** is drawn from [`ReviewPanelModel`] and driven by the shared
//!   [`ReviewPanelState`] (the TUI panel's rows, keys and merge gate), as the
//!   Navigation component's State=Review (`dt-figma-task-panels`, the pieces
//!   in [`super::task_panel`]; the window keeps the sidebar's titlebar and
//!   footer around it): the back button and the task's column (REVIEW), the
//!   title and the description clamped to five lines with `More` opening
//!   the task detail over the screen (the window draws it,
//!   `dt-more-opens-detail`), the worktree with Open (reveal it in Finder), the views
//!   numbered ⌘1 to ⌘N (Reviewer / View Diff / Edit in <editor> / Web
//!   Browser when a URL exists / Shell), then Actions: Approve and Reject.
//!   The review slot's **server** has no block of its own: until it serves,
//!   the Web Browser row is inert and says "loading…" where its hint sits,
//!   its tooltip carrying the sub-state and the health check; serving, the
//!   tooltip is the URL.
//! - **The user shell** (Shell) opens beside the content in the slot's
//!   worktree, as the Workspace view's does (`WorkspaceSessionOp::EnsureShell`
//!   on the slot, a workspace too; the split is
//!   [`super::workspace_layout::render_split`]). Leaving the review closes it.
//! - **The content** is one [`SessionView`]: Reviewer is the review agent's
//!   session (`<project>/ws/<slot>`); View Diff and Edit in <editor> are the
//!   daemon's `<project>/review/<slot>/<role>` sessions, ensured through
//!   `ReviewSessionOp::Ensure` and only attached here, so the daemon owns
//!   their spawn and teardown (`rt-tui-review`). The attach is the shared
//!   session view's: the retried connect that never picks a zombie sibling
//!   (the ENOTCONN re-entry fix), the bounded handshake (the 5s and wedged
//!   session fixes), give-up to a failed notice, and re-selecting a failed
//!   view re-attaches.
//! - **Approve and Reject** go back to the window as registry commands
//!   (`ApproveReview` / `RejectReview`), which run the daemon's gated merge
//!   or open the reject-reason overlay. While a decision is in flight the
//!   panel is inert (the window sets the merge gate).
//! - **Back** (the back button, Esc) leaves the review loaded and returns to
//!   the previous view; **Close** (`q`) asks the daemon to end the editor,
//!   diff and server sessions and free the slot's port, then returns too.
//!
//! The window opens the panel the moment the review is asked for, before the
//! load flow has resolved it (`dt-instant-panel-push`): [`ReviewView::show_pending`]
//! shows what the sidebar row knows (the title) with placeholders for the
//! rest, and [`ReviewView::show`] fills it in when the resolve answers. While
//! the column's push runs the window holds the content's attach
//! ([`ReviewView::hold_content`]), so the session's connect, replay and
//! first layout land after the push has settled, not in its frames.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    div, prelude::*, px, AnyElement, AnyWindowHandle, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Pixels, SharedString, Subscription, Window,
};
use shelbi_app::review::{
    sub_state_detail, sub_state_label, ActiveView, PanelEffect, PanelRow, ReviewInfo,
    ReviewPanelModel, ReviewPanelState, ReviewSwitch, ReviewTarget,
};
use shelbi_app::session::{ReviewRelauncher, SessionRef};
use shelbi_app::view::{ReviewRow, ReviewState};
use shelbi_app::{CommandKind, ReviewSessionOp, WorkspaceSessionOp};
use shelbi_core::{Column, StatusCategory};

use super::session_view::{OnDeath, SessionDeps, SessionView, SessionViewEvent, REVIEW_TERMINAL_INSET_X};
use super::task_panel;
use super::workspace_layout::{render_split, ShellSplit};
use crate::assets::{icons, Icon};
use crate::keys::chord_from_keystroke;
use crate::refresh::SessionSink;
use crate::theme::motion::{self, SharedTrack, SlideTrack};
use crate::theme::style::{self, ButtonKind, Type, Typography, NAV_RADIUS};
use crate::theme::Theme;

/// The blocking work behind a review, run on the background executor:
/// resolving and loading a review task, the daemon's content sessions, and the
/// openers. Injectable so the view and the window's flow are tested without a
/// daemon.
pub trait ReviewOps: Send + Sync {
    /// Whether `task` is serving on a review slot (and its panel inputs) or
    /// still queued (and the free slots).
    fn resolve(&self, project: &str, task: &str) -> Result<ReviewTarget, String>;
    /// Ask the daemon to load, start or stop one of the review's sessions.
    fn session_op(&self, project: &str, task: &str, op: ReviewSessionOp) -> Result<(), String>;
    /// Open `url` in the default browser.
    fn open_url(&self, url: &str) -> Result<(), String>;
    /// Show `path` in Finder.
    fn reveal(&self, path: &str) -> Result<(), String>;
    /// Start or stop the user shell in review slot `slot`'s worktree (the
    /// workspace's `EnsureShell` / `CloseShell`: a slot is a workspace).
    fn shell_op(&self, project: &str, slot: &str, op: WorkspaceSessionOp) -> Result<(), String>;
}

/// The real thing: the orchestrator's review state, the daemon's control
/// socket, and `open`.
pub struct LiveOps;

impl ReviewOps for LiveOps {
    fn resolve(&self, project: &str, task: &str) -> Result<ReviewTarget, String> {
        use shelbi_orchestrator::review_session::{review_open_target, ReviewOpenTarget};
        match review_open_target(project, task).map_err(|e| e.to_string())? {
            ReviewOpenTarget::Serving(info) => {
                let (title, body) = info
                    .task
                    .as_ref()
                    .map(|t| (t.task.title.clone(), t.body.clone()))
                    .unwrap_or_default();
                Ok(ReviewTarget::Serving(ReviewInfo {
                    task_id: task.to_string(),
                    slot: info.slot,
                    title,
                    body,
                    worktree: info.worktree,
                    editor_name: info.editor_name,
                    review_url: info.review_url,
                    has_health_check: info.has_health_check,
                    remote_machine: info.remote_machine,
                }))
            }
            ReviewOpenTarget::Queued { title } => {
                // The free slots only: a busy slot is left to its occupant,
                // never evicted (the TUI's load).
                let all = shelbi_orchestrator::load::review_slots(project).map_err(|e| e.to_string())?;
                let total_slots = all.len();
                let free_slots = all
                    .into_iter()
                    .filter(|s| s.occupant.is_none())
                    .map(|s| s.name)
                    .collect();
                Ok(ReviewTarget::Queued {
                    title,
                    free_slots,
                    total_slots,
                })
            }
        }
    }

    fn session_op(&self, project: &str, task: &str, op: ReviewSessionOp) -> Result<(), String> {
        shelbi_app::review_session(project, task, op, &mut |_, _| {})
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        run_open(url)
    }

    fn reveal(&self, path: &str) -> Result<(), String> {
        run_open(path)
    }

    fn shell_op(&self, project: &str, slot: &str, op: WorkspaceSessionOp) -> Result<(), String> {
        shelbi_app::workspace_session(project, slot, op, &mut |_, _| {})
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn run_open(target: &str) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(target)
        .status()
        .map_err(|e| format!("couldn't run `open`: {e}"))
        .and_then(|s| {
            if s.success() {
                Ok(())
            } else {
                Err(format!("`open {target}` exited {s}"))
            }
        })
}

/// What the view needs from the outside world.
pub struct ReviewDeps {
    pub ops: Arc<dyn ReviewOps>,
    /// Builds the content pane's session deps for a review task (its
    /// relauncher re-ensures that task's editor and diff).
    pub sessions: Rc<dyn Fn(&str) -> SessionDeps>,
}

impl ReviewDeps {
    /// The real daemon and sessions for `project`.
    pub fn live(project: &str) -> Self {
        let project = project.to_string();
        Self {
            ops: Arc::new(LiveOps),
            sessions: Rc::new(move |task| {
                SessionDeps::live_with(Arc::new(ReviewRelauncher {
                    project: project.clone(),
                    task: task.to_string(),
                }))
            }),
        }
    }
}

/// What the view tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewEvent {
    /// The content terminal took keyboard focus.
    Focused,
    /// Run a registry command: `ApproveReview`, or `RejectReview` with no
    /// reason (which asks for one).
    Command(CommandKind),
    /// Leave the review loaded and go back to the previous view.
    Back,
    /// More: open this task's detail over the screen.
    OpenTask(String),
    /// The review was torn down (the daemon was asked to end its sessions):
    /// go back to the previous view.
    Closed,
}

/// Where keyboard focus sits inside the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFocus {
    Panel,
    Content,
}

/// One review shown: its inputs, the panel, and the content pane.
struct Shown {
    info: ReviewInfo,
    row: Option<ReviewRow>,
    model: ReviewPanelModel,
    panel: ReviewPanelState,
    content: Entity<SessionView>,
    _content_events: Subscription,
    /// The load flow has resolved the review: `info` is the daemon's, not
    /// the placeholder built from the sidebar row.
    resolved: bool,
}

/// The Review view for one project window. It shows one review at a time;
/// opening another retargets it.
pub struct ReviewView {
    /// The view switches, measured for the current one's sliding chip.
    switch_track: SharedTrack,
    project: String,
    window: AnyWindowHandle,
    ops: Arc<dyn ReviewOps>,
    sessions: Rc<dyn Fn(&str) -> SessionDeps>,
    sink: Option<SessionSink>,
    review: Option<Shown>,
    shown: bool,
    focus: ReviewFocus,
    /// The user shell beside the content (Shell), in the slot's worktree.
    shell: Entity<SessionView>,
    shell_open: bool,
    /// The daemon is starting the shell (the pane shows "Starting a shell…").
    shell_starting: bool,
    /// The content | shell split, and its divider's drag.
    split: ShellSplit,
    _shell_events: Subscription,
    /// Bumped whenever the shown review changes; a background result for an
    /// older one is dropped.
    generation: u64,
    /// The window's push is running: the content waits to attach.
    hold: bool,
    /// The attach that waited for the push.
    held: Option<SessionRef>,
    focus_handle: FocusHandle,
}

impl EventEmitter<ReviewEvent> for ReviewView {}

impl ReviewView {
    pub fn new(
        project: &str,
        window: AnyWindowHandle,
        deps: ReviewDeps,
        sink: Option<SessionSink>,
        cx: &mut Context<Self>,
    ) -> Self {
        let shell = cx.new(|cx| {
            let mut v = SessionView::new(project, SessionRef::workspace_shell(""), "", window, (deps.sessions)(""), None, cx);
            v.set_on_death(OnDeath::Close);
            v
        });
        let shell_events = cx.subscribe(&shell, |this, _, ev: &SessionViewEvent, cx| match ev {
            SessionViewEvent::Focused => {
                this.focus = ReviewFocus::Content;
                cx.emit(ReviewEvent::Focused);
            }
            // The user exited the shell: close the split.
            SessionViewEvent::Ended => {
                this.shell_open = false;
                this.shell_starting = false;
                cx.notify();
            }
        });
        Self {
            project: project.to_string(),
            window,
            ops: deps.ops,
            sessions: deps.sessions,
            shell,
            shell_open: false,
            shell_starting: false,
            split: ShellSplit::default(),
            _shell_events: shell_events,
            sink,
            review: None,
            switch_track: SlideTrack::shared(),
            shown: false,
            focus: ReviewFocus::Panel,
            generation: 0,
            hold: false,
            held: None,
            focus_handle: cx.focus_handle(),
        }
    }

    /// The task shown (or last shown).
    pub fn task_id(&self) -> Option<&str> {
        self.review.as_ref().map(|r| r.info.task_id.as_str())
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// The shown review is only the placeholder: the load flow hasn't
    /// resolved it yet.
    pub fn is_pending(&self) -> bool {
        self.shown && self.review.as_ref().is_some_and(|r| !r.resolved)
    }

    /// Hold the content's attach until [`Self::release_content`] (the window,
    /// while its push runs), or stop holding (attaching what waited).
    pub fn hold_content(&mut self, hold: bool, cx: &mut Context<Self>) {
        if hold {
            self.hold = true;
        } else {
            self.release_content(cx);
        }
    }

    /// The push has settled: attach the content that waited for it.
    pub fn release_content(&mut self, cx: &mut Context<Self>) {
        self.hold = false;
        if let Some(target) = self.held.take() {
            if self.shown {
                self.attach(target, cx);
            }
        }
    }

    /// An attach is waiting for the push to settle.
    pub fn is_holding(&self) -> bool {
        self.hold
    }

    /// Show `task` at once, before the load flow has resolved it: the panel
    /// with what its sidebar `row` knows (the title, the slot, the server's
    /// state) and placeholders for the rest, the content still empty.
    /// [`Self::show`] fills it in. The review already shown (re-entering it)
    /// keeps what it had until then, its content re-attaching.
    pub fn show_pending(&mut self, task: &str, row: Option<ReviewRow>, cx: &mut Context<Self>) {
        if self.task_id() == Some(task) {
            if let Some(r) = self.review.as_mut() {
                if row.is_some() {
                    r.row = row;
                    r.model = ReviewPanelModel::build(&r.info, r.row.as_ref());
                    r.panel.sync(&r.model);
                }
            }
            self.shown = true;
            if self.review.as_ref().is_some_and(|r| r.resolved) {
                self.show_active(cx);
            }
            cx.notify();
            return;
        }
        self.close_shell(cx);
        if let Some(old) = self.review.take() {
            old.content.update(cx, |c, cx| c.hide(cx));
        }
        self.generation += 1;
        self.focus = ReviewFocus::Panel;
        self.held = None;
        let info = ReviewInfo {
            task_id: task.to_string(),
            slot: row.as_ref().and_then(|r| r.workspace.clone()).unwrap_or_default(),
            title: row.as_ref().map(|r| r.title.clone()).unwrap_or_default(),
            body: String::new(),
            worktree: String::new(),
            editor_name: String::new(),
            review_url: None,
            has_health_check: false,
            remote_machine: None,
        };
        let mut shown = self.build(info, row, cx);
        shown.resolved = false;
        self.review = Some(shown);
        self.shown = true;
        cx.notify();
    }

    /// The open review's inputs, once it has resolved (a placeholder's are
    /// the sidebar row's guess).
    pub fn info(&self) -> Option<ReviewInfo> {
        self.review.as_ref().filter(|r| r.resolved).map(|r| r.info.clone())
    }

    /// The panel as shown now.
    pub fn model(&self) -> Option<&ReviewPanelModel> {
        self.review.as_ref().map(|r| &r.model)
    }

    /// The panel's interactive state.
    pub fn panel(&self) -> Option<&ReviewPanelState> {
        self.review.as_ref().map(|r| &r.panel)
    }

    /// Show the review `info` describes, its live state from `row`. The same
    /// task again re-attaches its content (re-entering a review; a failed
    /// view re-opens); another task starts fresh on Reviewer.
    pub fn show(&mut self, info: ReviewInfo, row: Option<ReviewRow>, cx: &mut Context<Self>) {
        // A placeholder is replaced outright: its content was built for the
        // slot the sidebar row guessed, and never attached.
        let same = self
            .review
            .as_ref()
            .is_some_and(|r| r.resolved && r.info.task_id == info.task_id);
        if !same {
            self.close_shell(cx);
            if let Some(old) = self.review.take() {
                old.content.update(cx, |c, cx| c.hide(cx));
            }
            self.generation += 1;
            self.focus = ReviewFocus::Panel;
            self.held = None;
            self.review = Some(self.build(info, row, cx));
        } else if let Some(r) = self.review.as_mut() {
            // Fresh inputs (the description, the URL) for the same review,
            // or the resolve filling in its placeholder.
            r.info = info;
            r.row = row;
            r.resolved = true;
            r.model = ReviewPanelModel::build(&r.info, r.row.as_ref());
            r.panel.sync(&r.model);
        }
        self.shown = true;
        self.show_active(cx);
        cx.notify();
    }

    fn build(&self, info: ReviewInfo, row: Option<ReviewRow>, cx: &mut Context<Self>) -> Shown {
        let model = ReviewPanelModel::build(&info, row.as_ref());
        // The desktop offers the user shell beside the content (Shell ⌘5).
        let panel = ReviewPanelState::with_shell(&model, true);
        let deps = (self.sessions)(&info.task_id);
        let (window, sink, project) = (self.window, self.sink.clone(), self.project.clone());
        let slot = info.slot.clone();
        let remote = info.remote_machine.clone();
        let content = cx.new(|cx| {
            let mut v = SessionView::new(&project, SessionRef::Workspace(slot.clone()), &slot, window, deps, sink, cx);
            v.set_remote_machine(remote);
            // A review pane with no session says so; the dev slot's idle
            // placeholder doesn't fit it (the TUI's review interface).
            v.set_idle_placeholder(false);
            v.set_inset_x(REVIEW_TERMINAL_INSET_X);
            v
        });
        let events = cx.subscribe(&content, |this, _, ev: &SessionViewEvent, cx| {
            if *ev == SessionViewEvent::Focused {
                this.focus = ReviewFocus::Content;
                cx.emit(ReviewEvent::Focused);
            }
        });
        Shown {
            info,
            row,
            model,
            panel,
            content,
            _content_events: events,
            resolved: true,
        }
    }

    /// Hide the view: detach the content pane (the daemon's sessions stay).
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.held = None;
        self.split.dragging = false;
        self.close_shell(cx);
        if let Some(r) = &self.review {
            r.content.update(cx, |c, cx| c.hide(cx));
        }
        cx.notify();
    }

    /// The task's title and description as the board reads them now (the
    /// detail opened over the screen edited them): the panel shows them
    /// when they're its review's.
    pub fn refresh_task_text(&mut self, task: &str, title: &str, body: &str, cx: &mut Context<Self>) {
        let Some(r) = self.review.as_mut().filter(|r| r.resolved && r.info.task_id == task) else {
            return;
        };
        if r.info.title == title && r.info.body == body {
            return;
        }
        r.info.title = title.to_string();
        r.info.body = body.to_string();
        r.model = ReviewPanelModel::build(&r.info, r.row.as_ref());
        r.panel.sync(&r.model);
        cx.notify();
    }

    /// The task's sidebar row refreshed: the live server state.
    pub fn update_row(&mut self, row: Option<ReviewRow>, cx: &mut Context<Self>) {
        let Some(r) = self.review.as_mut() else {
            return;
        };
        if row.is_none() || r.row == row {
            return;
        }
        r.row = row;
        r.model = ReviewPanelModel::build(&r.info, r.row.as_ref());
        r.panel.sync(&r.model);
        cx.notify();
    }

    /// Start or end the merge gate (the window does, while a decision on this
    /// review is in flight).
    pub fn set_merging(&mut self, on: bool, cx: &mut Context<Self>) {
        if let Some(r) = self.review.as_mut() {
            r.panel.set_merging(on);
            cx.notify();
        }
    }

    pub fn is_merging(&self) -> bool {
        self.panel().is_some_and(ReviewPanelState::is_merging)
    }

    /// A failure or note on the panel's status line.
    pub fn set_status(&mut self, line: Option<String>, cx: &mut Context<Self>) {
        if let Some(r) = self.review.as_mut() {
            r.panel.set_status(line);
            cx.notify();
        }
    }

    /// The content's sessions are about to end on purpose: don't restart them.
    pub fn note_deliberate_close(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.review {
            r.content.update(cx, |c, _| c.note_deliberate_close());
        }
    }

    /// The deliberate teardown didn't happen after all.
    pub fn clear_deliberate_close(&mut self, cx: &mut Context<Self>) {
        if let Some(r) = &self.review {
            r.content.update(cx, |c, _| c.clear_deliberate_close());
        }
    }

    /// The decision on this review went through (approved, rejected): the
    /// task has left review. Ask the daemon to end its content sessions, as
    /// the TUI does after a merge, and stop showing it.
    pub fn finish(&mut self, cx: &mut Context<Self>) {
        self.note_deliberate_close(cx);
        self.run_op(ReviewSessionOp::Close, cx, |_, result, _| {
            if let Err(e) = result {
                tracing::warn!(error = %e, "couldn't close the review's sessions");
            }
        });
        self.set_merging(false, cx);
        self.hide(cx);
    }

    /// Where keyboard focus goes when the main area takes it: the content
    /// terminal once the user moved there, else the panel.
    pub fn focus_target(&self, cx: &gpui::App) -> FocusHandle {
        match (&self.review, self.focus) {
            (Some(r), ReviewFocus::Content) if r.content.read(cx).pane().is_some() => {
                r.content.read(cx).focus_target(cx)
            }
            _ => self.focus_handle.clone(),
        }
    }

    /// Give the panel keyboard focus (the sidebar chord while the panel holds
    /// the sidebar's column).
    pub fn focus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus = ReviewFocus::Panel;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Whether keyboard focus is in the view (its panel or its terminal).
    pub fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus_handle.is_focused(window)
            || self.shell.read(cx).contains_focus(window, cx)
            || self
                .review
                .as_ref()
                .is_some_and(|r| r.content.read(cx).contains_focus(window, cx))
    }

    fn show_active(&mut self, cx: &mut Context<Self>) {
        let Some(view) = self.panel().map(ReviewPanelState::active) else {
            return;
        };
        self.select_view(view, cx);
    }

    fn content_target(&self, view: ActiveView) -> Option<SessionRef> {
        let slot = self.review.as_ref()?.info.slot.clone();
        Some(match view.role() {
            None => SessionRef::Workspace(slot),
            Some(role) => SessionRef::Review {
                slot,
                role: role.as_str().to_string(),
            },
        })
    }

    /// Show the agent, the diff or the editor. Diff and editor are ensured
    /// through the daemon first; a failure falls back to the agent with the
    /// error on the status line, as the TUI does.
    pub fn select_view(&mut self, view: ActiveView, cx: &mut Context<Self>) {
        let Some(target) = self.content_target(view) else {
            return;
        };
        if let Some(r) = self.review.as_mut() {
            r.panel.set_active(view);
        }
        let Some(role) = view.role() else {
            self.attach(target, cx);
            return;
        };
        cx.notify();
        self.run_op(ReviewSessionOp::Ensure { role }, cx, move |this, result, cx| {
            let still = this.panel().is_some_and(|p| p.active() == view) && this.shown;
            match result {
                Ok(()) if still => this.attach(target, cx),
                Ok(()) => {}
                Err(e) => {
                    if let Some(r) = this.review.as_mut() {
                        r.panel.set_status(Some(format!("couldn't open view: {e}")));
                    }
                    this.select_view(ActiveView::Chat, cx);
                }
            }
        });
    }

    fn attach(&mut self, target: SessionRef, cx: &mut Context<Self>) {
        if self.hold {
            self.held = Some(target);
            return;
        }
        if let Some(r) = &self.review {
            r.content.update(cx, |c, cx| c.show_target(target, cx));
        }
        cx.notify();
    }

    /// Run a daemon op for the shown review off the UI thread, then `done`
    /// with its result (dropped if the view has moved to another review).
    fn run_op(
        &mut self,
        op: ReviewSessionOp,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static,
    ) {
        let Some(task) = self.task_id().map(str::to_string) else {
            return;
        };
        let ops = self.ops.clone();
        let project = self.project.clone();
        let generation = self.generation;
        let work = cx
            .background_executor()
            .spawn(async move { ops.session_op(&project, &task, op) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    done(this, result, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// Run an opener off the UI thread; a failure lands on the status line.
    fn run_opener(
        &mut self,
        what: &'static str,
        open: impl FnOnce(&dyn ReviewOps) -> Result<(), String> + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        let ops = self.ops.clone();
        let generation = self.generation;
        let work = cx.background_executor().spawn(async move { open(ops.as_ref()) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = work.await {
                this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.set_status(Some(format!("couldn't {what}: {e}")), cx);
                    }
                })
                .ok();
            }
        })
        .detach();
    }

    /// Carry out what a panel interaction asked for.
    pub fn apply(&mut self, effect: PanelEffect, cx: &mut Context<Self>) {
        let Some(task) = self.task_id().map(str::to_string) else {
            return;
        };
        // A placeholder offers Back alone until the resolve fills it in.
        if self.is_pending() && effect != PanelEffect::Back {
            return;
        }
        match effect {
            PanelEffect::None => {}
            PanelEffect::Back => cx.emit(ReviewEvent::Back),
            PanelEffect::ShowChat => {
                self.set_status(None, cx);
                self.select_view(ActiveView::Chat, cx);
            }
            PanelEffect::ShowDiff => {
                self.set_status(None, cx);
                self.select_view(ActiveView::Diff, cx);
            }
            PanelEffect::ShowEditor => {
                self.set_status(None, cx);
                self.select_view(ActiveView::Editor, cx);
            }
            // Inert until the slot's server is serving (the row says so).
            PanelEffect::OpenBrowser => {
                if self.model().is_some_and(|m| m.server.state == ReviewState::Serving) {
                    self.open_url(cx);
                }
            }
            PanelEffect::RevealFolder => {
                let Some(path) = self
                    .review
                    .as_ref()
                    .map(|r| r.info.worktree.clone())
                    .filter(|p| !p.is_empty())
                else {
                    self.set_status(Some("no review worktree to reveal".into()), cx);
                    return;
                };
                self.run_opener("reveal the folder", move |ops| ops.reveal(&path), cx);
            }
            // More opens the task's detail over the screen.
            PanelEffect::ShowDescription => cx.emit(ReviewEvent::OpenTask(task)),
            PanelEffect::ToggleShell => self.toggle_shell(cx),
            PanelEffect::Approve => cx.emit(ReviewEvent::Command(CommandKind::ApproveReview { id: task })),
            PanelEffect::RejectPrompt => cx.emit(ReviewEvent::Command(CommandKind::RejectReview {
                id: task,
                reason: String::new(),
            })),
            PanelEffect::Close => self.close(cx),
        }
    }

    fn open_url(&mut self, cx: &mut Context<Self>) {
        match self.model().and_then(|m| m.review_url.clone()) {
            Some(url) => self.run_opener("open the browser", move |ops| ops.open_url(&url), cx),
            None => self.set_status(Some("no review URL configured".into()), cx),
        }
    }

    /// Tear the review down (the TUI's `q`): the daemon ends the editor,
    /// diff and server sessions and frees the port; the window goes back.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.note_deliberate_close(cx);
        self.run_op(ReviewSessionOp::Close, cx, |_, result, _| {
            if let Err(e) = result {
                tracing::warn!(error = %e, "couldn't close the review");
            }
        });
        self.hide(cx);
        cx.emit(ReviewEvent::Closed);
    }

    /// ⌘N (the window's chord while the panel holds the column): the
    /// panel's N-th view, as its hints number them. Focus stays where it is.
    pub fn view_shortcut(&mut self, n: u8, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(r) = self.review.as_mut() else {
            return;
        };
        let row = (n as usize)
            .checked_sub(1)
            .and_then(|i| r.panel.rows().iter().copied().filter(|row| is_view_row(*row)).nth(i));
        if let Some(row) = row {
            let effect = r.panel.click(row);
            self.apply(effect, cx);
            cx.notify();
        }
    }

    // --- the user shell ----------------------------------------------------

    /// Open or close the user shell beside the content.
    pub fn toggle_shell(&mut self, cx: &mut Context<Self>) {
        if self.shell_open {
            self.close_shell(cx);
        } else {
            self.open_shell(cx);
        }
        cx.notify();
    }

    #[cfg(test)]
    pub fn shell_open(&self) -> bool {
        self.shell_open
    }

    fn open_shell(&mut self, cx: &mut Context<Self>) {
        let Some(slot) = self.review.as_ref().map(|r| r.info.slot.clone()).filter(|s| !s.is_empty()) else {
            return;
        };
        self.shell_open = true;
        self.shell_starting = true;
        self.set_status(None, cx);
        self.shell_op(WorkspaceSessionOp::EnsureShell, cx, move |this, result, cx| {
            if !this.shell_open {
                return;
            }
            this.shell_starting = false;
            match result {
                Ok(()) => {
                    let target = SessionRef::workspace_shell(&slot);
                    this.shell.update(cx, |s, cx| s.show_target(target, cx));
                }
                Err(e) => {
                    this.shell_open = false;
                    this.set_status(Some(format!("couldn't open a shell: {e}")), cx);
                }
            }
        });
    }

    fn close_shell(&mut self, cx: &mut Context<Self>) {
        if !self.shell_open {
            return;
        }
        self.shell_open = false;
        self.shell_starting = false;
        self.shell.update(cx, |s, cx| s.hide(cx));
        self.shell_op(WorkspaceSessionOp::CloseShell, cx, |_, result, _| {
            if let Err(e) = result {
                tracing::warn!(error = %e, "couldn't close the review's user shell");
            }
        });
    }

    /// Run a shell op for the shown review's slot off the UI thread, then
    /// `done` with its result (dropped if the view has moved on).
    fn shell_op(
        &mut self,
        op: WorkspaceSessionOp,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static,
    ) {
        let Some(slot) = self.review.as_ref().map(|r| r.info.slot.clone()).filter(|s| !s.is_empty()) else {
            return;
        };
        let ops = self.ops.clone();
        let project = self.project.clone();
        let generation = self.generation;
        let work = cx
            .background_executor()
            .spawn(async move { ops.shell_op(&project, &slot, op) });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                if this.generation == generation {
                    done(this, result, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// A click on a panel row.
    fn click(&mut self, row: PanelRow, window: &mut Window, cx: &mut Context<Self>) {
        self.focus = ReviewFocus::Panel;
        window.focus(&self.focus_handle, cx);
        let Some(effect) = self.review.as_mut().map(|r| r.panel.click(row)) else {
            return;
        };
        self.apply(effect, cx);
        cx.notify();
    }

    /// A key while the panel has focus.
    fn panel_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_keystroke(&event.keystroke) else {
            return;
        };
        // Tab moves to the content terminal once it is live (the TUI's).
        if chord.code == shelbi_state::keymap::Key::Tab && chord.mods.is_empty() {
            if let Some(r) = &self.review {
                if r.content.read(cx).pane().is_some() && !r.panel.is_merging() {
                    self.focus = ReviewFocus::Content;
                    let target = r.content.read(cx).focus_target(cx);
                    window.focus(&target, cx);
                }
            }
            cx.stop_propagation();
            return;
        }
        let Some(r) = self.review.as_mut() else {
            return;
        };
        let before = (r.panel.selected(), r.panel.status_line().map(str::to_string));
        let effect = r.panel.handle_key(chord);
        let after = (r.panel.selected(), r.panel.status_line().map(str::to_string));
        if effect == PanelEffect::None && before == after {
            // Not the panel's key: let the window have it.
            return;
        }
        cx.stop_propagation();
        self.apply(effect, cx);
        cx.notify();
    }

    // --- rendering ---------------------------------------------------------

    fn render_panel(
        &self,
        shown: &Shown,
        chip: Option<Bounds<Pixels>>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let model = &shown.model;
        let panel = &shown.panel;
        let focused = self.focus == ReviewFocus::Panel;
        let selected = |row: PanelRow| focused && panel.selected_row() == Some(row);
        let listen = |row: PanelRow| {
            cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| this.click(row, window, cx))
        };

        // The back button and the task's status (its column: Review).
        let back = task_panel::back_button("review-back", selected(PanelRow::Back), theme)
            .on_click(listen(PanelRow::Back));
        let status_label = Column::review().default_status_name().to_string();
        let status = task_panel::status_row(back, Some((StatusCategory::Handoff, status_label.as_str())), theme);

        // The issue summary: the title, five lines of description, More.
        let title = if model.title.is_empty() {
            model.task_id.clone()
        } else {
            model.title.clone()
        };
        let more = (shown.resolved && model.has_body()).then(|| {
            task_panel::more_link("review-more", selected(PanelRow::More), theme)
                .on_click(listen(PanelRow::More))
        });
        let summary = task_panel::summary(
            ("review-title", "review-description"),
            title,
            model.description.clone(),
            more,
            theme,
        );

        if !shown.resolved {
            // The load flow hasn't answered yet: what the sidebar row gave,
            // and a placeholder where the rest will land.
            return self
                .panel_root(theme, cx)
                .child(status)
                .child(summary)
                .child(
                    div()
                        .debug_selector(|| "review-loading".into())
                        .px(px(18.0))
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child("Loading…"),
                );
        }

        // The worktree, with Open when it can be revealed (a local slot).
        let folder = (!model.worktree.is_empty()).then(|| {
            let open = (!model.is_remote()).then(|| {
                task_panel::open_button("review-folder", theme).on_click(listen(PanelRow::Folder))
            });
            task_panel::worktree_row("review-worktree", &model.worktree, open, selected(PanelRow::Folder), 0.0, theme)
        });

        // The views: Reviewer, its diff and editor, the browser, the shell,
        // ⌘1…⌘N.
        let serving = model.server.state == ReviewState::Serving;
        let mut shortcut = 0u8;
        let mut views = Vec::new();
        for row in panel.rows().iter().copied() {
            let n = if is_view_row(row) {
                shortcut += 1;
                (shortcut <= 9).then_some(shortcut)
            } else {
                None
            };
            match row {
                PanelRow::Switch(item) => {
                    let (id, icon) = switch_id_icon(item);
                    let current = match item {
                        ReviewSwitch::Chat => panel.active() == ActiveView::Chat,
                        ReviewSwitch::Diff => panel.active() == ActiveView::Diff,
                        ReviewSwitch::Editor => panel.active() == ActiveView::Editor,
                        ReviewSwitch::Browser => false,
                    };
                    let label = switch_label(item, &model.editor_name);
                    if item == ReviewSwitch::Browser && !serving {
                        views.push(self.render_waiting_browser(model, selected(row), theme).into_any_element());
                        continue;
                    }
                    let painted = style::row_state_rounded(
                        div().id(id).debug_selector(move || id.into()),
                        id,
                        current,
                        selected(row) && chip.is_none(),
                        chip.is_none(),
                        px(NAV_RADIUS),
                        theme,
                    )
                    .child(motion::probe(&self.switch_track, id))
                    .when_some(
                        (item == ReviewSwitch::Browser).then(|| model.review_url.clone()).flatten(),
                        |d, url| d.tooltip(style::tooltip(url)),
                    );
                    views.push(
                        task_panel::view_row(painted, icon, label, n, None, current, theme)
                            .on_click(listen(row))
                            .into_any_element(),
                    );
                }
                PanelRow::Shell => {
                    let open = self.shell_open;
                    let painted = style::row_state_rounded(
                        div().id("review-shell-toggle").debug_selector(|| "review-shell-toggle".into()),
                        "review-shell-toggle",
                        false,
                        selected(row),
                        true,
                        px(NAV_RADIUS),
                        theme,
                    )
                    // Open, the toggle holds the overlay fill and a loud label.
                    .when(open, |d| d.bg(theme.background).text_color(theme.text_loud))
                    .tooltip(style::tooltip(if open { "Close the shell" } else { "Open a shell in the worktree" }));
                    let starting = self.shell_starting.then(|| {
                        div()
                            .debug_selector(|| "review-shell-state".into())
                            .flex_none()
                            .px(px(4.0))
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child("starting…")
                            .into_any_element()
                    });
                    views.push(
                        task_panel::view_row(painted, icons::COMMAND_LINE, "Shell", n, starting, false, theme)
                            .on_click(listen(row))
                            .into_any_element(),
                    );
                }
                _ => {}
            }
        }
        let views = task_panel::views_block()
            .child(motion::origin_probe(&self.switch_track))
            .children(chip.map(|b| {
                motion::floating_chip(b, |d| {
                    style::nav_chip(d.rounded(px(NAV_RADIUS)), theme)
                })
            }))
            .children(views);

        // Actions: Approve and Reject, or the merge in flight.
        let actions = if panel.is_merging() {
            div()
                .debug_selector(|| "review-merging".into())
                .px(px(20.0))
                .py(px(8.0))
                .type_style(Type::SmallMedium)
                .text_color(theme.warning)
                .child("Merging the PR…")
        } else {
            div()
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(12.0))
                .px(px(12.0))
                .py(px(8.0))
                .child(
                    style::small_icon_button(
                        "review-approve",
                        icons::CHECK,
                        "Approve",
                        ButtonKind::Primary,
                        selected(PanelRow::Approve),
                        theme,
                    )
                    .on_click(listen(PanelRow::Approve)),
                )
                .child(
                    style::small_icon_button(
                        "review-reject",
                        icons::X_MARK,
                        "Reject",
                        ButtonKind::Danger,
                        selected(PanelRow::Reject),
                        theme,
                    )
                    .on_click(listen(PanelRow::Reject)),
                )
        };

        self.panel_root(theme, cx)
            .child(status)
            .child(summary)
            .children(folder)
            .child(views)
            .child(task_panel::section_header("Actions", theme))
            .child(actions)
            .when_some(panel.status_line().map(str::to_string), |d, line| {
                d.child(task_panel::note("review-status-line", line, theme.error))
            })
    }

    /// The panel's frame: focus, keys, the scroll.
    fn panel_root(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        div()
            .id("review-panel")
            .debug_selector(|| "review-panel".into())
            .track_focus(&self.focus_handle)
            .key_context("ReviewPanel")
            .on_key_down(cx.listener(Self::panel_key))
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .type_style(Type::Base)
            .text_color(theme.foreground)
    }

    /// The Web Browser row while the slot's server isn't serving yet: muted
    /// and inert, "loading…" where its ⌘N hint sits, the server's sub-state
    /// and health check in its tooltip (the Server block Figma dropped).
    fn render_waiting_browser(
        &self,
        model: &ReviewPanelModel,
        keyboard: bool,
        theme: &Theme,
    ) -> gpui::Stateful<gpui::Div> {
        let server = &model.server;
        let mut tip = format!("{}: {}", sub_state_label(server.state), sub_state_detail(server.state));
        tip.push_str(&format!(" · {}", server.health.label()));
        if let Some(url) = &model.review_url {
            tip.push_str(&format!(" · {url}"));
        }
        let id = "review-switch-browser";
        let d = div()
            .id(id)
            .debug_selector(move || id.into())
            .relative()
            .cursor_default()
            .text_color(theme.muted)
            .when(keyboard, |d| d.bg(theme.background))
            .tooltip(style::tooltip(tip));
        let waiting = div()
            .debug_selector(|| "review-server-state".into())
            .flex_none()
            .px(px(4.0))
            .type_style(Type::XSmall)
            .text_color(theme.muted)
            .child("loading…")
            .into_any_element();
        task_panel::view_row(d, icons::GLOBE_ALT, switch_label(ReviewSwitch::Browser, ""), None, Some(waiting), false, theme)
            .cursor_default()
    }
}

impl Focusable for ReviewView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ReviewView {
    /// The main area beside the panel: the content terminal, and the user
    /// shell beside it when open. Empty when nothing was shown.
    pub fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let root = div()
            .id("review-view")
            .debug_selector(|| "review-view".into())
            .relative()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .size_full()
            .bg(theme.nested(theme.canvas));
        let Some(shown) = &self.review else {
            return root.into_any_element();
        };
        // Not resolved yet, or waiting for the push to settle: an empty
        // canvas, not the content's "no session" notice.
        let waiting = !shown.resolved || self.held.is_some();
        let content = div()
            .debug_selector(|| "review-content".into())
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(shown.content.clone())
            .into_any_element();
        let shell = self.shell_open.then(|| {
            if self.shell_starting {
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .text_color(theme.muted)
                    .child("Starting a shell…")
                    .into_any_element()
            } else {
                self.shell.clone().into_any_element()
            }
        });
        let panes = (!waiting).then(|| {
            render_split("review", &self.split, |this: &mut Self| &mut this.split, content, shell, &theme, cx)
        });
        root.children(panes).into_any_element()
    }
}

/// Whether `row` is one of the views (numbered ⌘1…⌘N): the view switch and
/// the shell.
fn is_view_row(row: PanelRow) -> bool {
    matches!(row, PanelRow::Switch(_) | PanelRow::Shell)
}

/// A view switch entry's element id and icon (Reviewer is the sidebar's
/// chat bubble, as its Orchestrator row).
fn switch_id_icon(item: ReviewSwitch) -> (&'static str, Icon) {
    match item {
        ReviewSwitch::Chat => ("review-switch-chat", icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS),
        ReviewSwitch::Diff => ("review-switch-diff", icons::ARROWS_RIGHT_LEFT),
        ReviewSwitch::Editor => ("review-switch-editor", icons::PENCIL_SQUARE),
        ReviewSwitch::Browser => ("review-switch-browser", icons::GLOBE_ALT),
    }
}

/// A view switch entry's label: the shared one, except the browser reads
/// "Web Browser" here (the component's; the TUI keeps "Open Browser").
fn switch_label(item: ReviewSwitch, editor_name: &str) -> String {
    match item {
        ReviewSwitch::Browser => "Web Browser".to_string(),
        other => other.label(editor_name),
    }
}

/// The id of the view switch for what the main area shows, if one is.
fn active_switch_id(panel: &ReviewPanelState) -> Option<&'static str> {
    Some(match panel.active() {
        ActiveView::Chat => "review-switch-chat",
        ActiveView::Diff => "review-switch-diff",
        ActiveView::Editor => "review-switch-editor",
    })
}

impl Render for ReviewView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // The current view switch's chip slides when the main area changes.
        let chip = {
            let key = self.review.as_ref().and_then(|r| active_switch_id(&r.panel));
            let mut track = self.switch_track.borrow_mut();
            track.select(key.map(SharedString::from), Instant::now(), || cx.reduce_motion());
            let chip = track.frame(Instant::now());
            if chip.is_some() {
                window.request_animation_frame();
            }
            chip
        };
        match &self.review {
            Some(shown) => self.render_panel(shown, chip, &theme, cx).into_any_element(),
            None => div()
                .debug_selector(|| "review-panel-empty".into())
                .size_full()
                .bg(theme.nested(theme.sidebar))
                .into_any_element(),
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use gpui::{TestAppContext, VisualTestContext, WindowAppearance};
    use shelbi_app::review::HealthCheck;
    use shelbi_app::session::{
        self as app_session, ConnectFailure, ReconnectPolicy, Relauncher, RetryPolicy,
    };
    use shelbi_app::theme::ThemePreference;
    use shelbi_app::view::ReviewState;
    use shelbi_app::ReviewRole;
    use shelbi_client::SessionEvent;
    use shelbi_term::Size;

    use crate::elements::terminal::session::{Connected, SessionIo};
    use crate::views::session_view::{SessionConnector, SessionPhase};

    struct NullIo;
    impl SessionIo for NullIo {
        fn input(&self, _bytes: Vec<u8>) {}
        fn paste(&self, _text: String, _bracketed: bool) {}
        fn resize(&self, _size: Size) {}
    }

    /// Sessions by name: a name in `refusing` fails to attach (a zombie, a
    /// session not up yet), anything else attaches.
    #[derive(Default)]
    pub(crate) struct FakeSessions {
        refusing: Mutex<Vec<String>>,
        attaches: Mutex<Vec<(String, UnboundedSender<SessionEvent>)>>,
    }

    impl FakeSessions {
        pub(crate) fn attached(&self) -> Vec<String> {
            self.attaches.lock().unwrap().iter().map(|(n, _)| n.clone()).collect()
        }
    }

    impl SessionConnector for FakeSessions {
        fn connect(
            &self,
            project: &str,
            target: &SessionRef,
            _retry: RetryPolicy,
        ) -> Result<Connected, ConnectFailure> {
            let name = app_session::discovery_name(project, target);
            if self.refusing.lock().unwrap().contains(&name) {
                return Err(ConnectFailure::Message(format!("{name}: Socket is not connected (os error 57)")));
            }
            let (tx, rx) = unbounded();
            self.attaches.lock().unwrap().push((name.clone(), tx));
            Ok(Connected {
                link: Box::new(NullIo),
                events: rx,
                size: Size::new(80, 24),
                name,
                cwd: None,
            })
        }
    }

    impl Relauncher for FakeSessions {
        fn relaunch(&self, _target: &SessionRef, _reopen: bool) -> Result<(), String> {
            Ok(())
        }
    }

    #[derive(Default)]
    pub(crate) struct FakeOps {
        ops: Mutex<Vec<(String, ReviewSessionOp)>>,
        fail_ops: Mutex<bool>,
        opened: Mutex<Vec<String>>,
        /// Every shell op: the slot and the op.
        shell_ops: Mutex<Vec<(String, WorkspaceSessionOp)>>,
        /// What a resolve answers instead of serving [`info`].
        pub(crate) resolve_with: Mutex<Option<Result<ReviewTarget, String>>>,
    }

    impl ReviewOps for FakeOps {
        fn resolve(&self, _project: &str, _task: &str) -> Result<ReviewTarget, String> {
            match &*self.resolve_with.lock().unwrap() {
                Some(answer) => answer.clone(),
                None => Ok(ReviewTarget::Serving(info())),
            }
        }
        fn session_op(&self, _project: &str, task: &str, op: ReviewSessionOp) -> Result<(), String> {
            self.ops.lock().unwrap().push((task.to_string(), op));
            if *self.fail_ops.lock().unwrap() {
                Err("the daemon said no".into())
            } else {
                Ok(())
            }
        }
        fn open_url(&self, url: &str) -> Result<(), String> {
            self.opened.lock().unwrap().push(url.to_string());
            Ok(())
        }
        fn reveal(&self, path: &str) -> Result<(), String> {
            self.opened.lock().unwrap().push(path.to_string());
            Ok(())
        }
        fn shell_op(&self, _project: &str, slot: &str, op: WorkspaceSessionOp) -> Result<(), String> {
            self.shell_ops.lock().unwrap().push((slot.to_string(), op));
            Ok(())
        }
    }

    /// The window's composition in miniature: the panel in a leading column,
    /// the main area beside it.
    struct Host {
        view: Entity<ReviewView>,
    }

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let main = self.view.update(cx, |v, cx| v.render_main(cx));
            div()
                .flex()
                .flex_row()
                .size_full()
                .child(div().w(px(252.0)).h_full().flex_none().child(self.view.clone()))
                .child(main)
        }
    }

    struct Harness {
        view: Entity<ReviewView>,
        cx: VisualTestContext,
        sessions: Arc<FakeSessions>,
        ops: Arc<FakeOps>,
        events: Rc<std::cell::RefCell<Vec<ReviewEvent>>>,
    }

    impl Harness {
        fn update<R>(&mut self, f: impl FnOnce(&mut ReviewView, &mut Context<ReviewView>) -> R) -> R {
            let r = self.view.update(&mut self.cx, f);
            self.cx.run_until_parked();
            r
        }

        fn content_phase(&mut self) -> SessionPhase {
            self.cx.update(|_, cx| {
                let content = self.view.read(cx).review.as_ref().unwrap().content.clone();
                content.read(cx).phase()
            })
        }

        fn ops(&self) -> Vec<ReviewSessionOp> {
            self.ops.ops.lock().unwrap().iter().map(|(_, op)| op.clone()).collect()
        }

        fn draw(&mut self) {
            self.cx.update(|window, cx| window.draw(cx).clear(cx));
            self.cx.run_until_parked();
        }

        fn click(&mut self, selector: &'static str) {
            self.draw();
            let at = self
                .cx
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} is drawn"))
                .center();
            self.cx.simulate_click(at, gpui::Modifiers::none());
            self.cx.run_until_parked();
        }

        fn key(&mut self, keystroke: &str) {
            self.cx.simulate_keystrokes(keystroke);
            self.cx.run_until_parked();
        }

        fn focus_panel(&mut self) {
            self.cx.update(|window, cx| {
                let handle = self.view.read(cx).focus_handle.clone();
                window.focus(&handle, cx);
            });
            self.cx.run_until_parked();
        }

        fn events(&self) -> Vec<ReviewEvent> {
            self.events.borrow().clone()
        }

        fn status_line(&self) -> Option<String> {
            self.view
                .read_with(&self.cx, |v, _| v.panel().and_then(|p| p.status_line().map(str::to_string)))
        }
    }

    pub(crate) fn info() -> ReviewInfo {
        ReviewInfo {
            task_id: "t-1".into(),
            slot: "review-1".into(),
            title: "Cold-start cache".into(),
            body: "## Summary\n\nWarm the cache at boot.".into(),
            worktree: "/w/review-1".into(),
            editor_name: "Vim".into(),
            review_url: Some("http://localhost:4101".into()),
            has_health_check: true,
            remote_machine: None,
        }
    }

    pub(crate) fn row(state: ReviewState) -> ReviewRow {
        ReviewRow {
            task_id: "t-1".into(),
            title: "Cold-start cache".into(),
            branch: "jlong/t-1".into(),
            location: (state == ReviewState::Serving).then(|| "hub:4101".to_string()),
            workspace: Some("review-1".into()),
            state,
            number: None,
        }
    }

    /// Review deps over fake sessions and daemon ops: nothing reaches a
    /// daemon or a session (the window's tests use them too).
    pub(crate) fn fake_deps() -> (ReviewDeps, Arc<FakeSessions>, Arc<FakeOps>) {
        let sessions = Arc::new(FakeSessions::default());
        let ops = Arc::new(FakeOps::default());
        let now = Rc::new(Cell::new(Instant::now()));
        let deps = {
            let sessions = sessions.clone();
            ReviewDeps {
                ops: ops.clone(),
                sessions: Rc::new(move |_task| {
                    let now = now.clone();
                    SessionDeps {
                        connector: sessions.clone(),
                        relauncher: sessions.clone(),
                        retry: RetryPolicy::default(),
                        reconnect: ReconnectPolicy {
                            first_backoff: Duration::from_secs(1),
                            max_backoff: Duration::from_secs(4),
                        },
                        clock: Rc::new(move || now.get()),
                    }
                }),
            }
        };
        (deps, sessions, ops)
    }

    fn open(cx: &mut TestAppContext) -> Harness {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let (deps, sessions, ops) = fake_deps();
        let events = Rc::new(std::cell::RefCell::new(Vec::new()));
        let deps = Cell::new(Some(deps));
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let handle = window.window_handle();
                let view = cx.new(|cx| ReviewView::new("demo", handle, deps.take().unwrap(), None, cx));
                cx.new(|_| Host { view })
            })
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut vcx).unwrap().read_with(&vcx, |h, _| h.view.clone());
        let sink = events.clone();
        vcx.update(|_, cx| {
            cx.subscribe(&view, move |_, ev: &ReviewEvent, _| sink.borrow_mut().push(ev.clone()))
                .detach();
        });
        vcx.run_until_parked();
        Harness {
            view,
            cx: vcx,
            sessions,
            ops,
            events,
        }
    }

    #[gpui::test]
    fn the_panel_renders_from_the_model_beside_the_review_agents_chat(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        assert_eq!(h.sessions.attached(), vec!["demo/ws/review-1"], "Chat is the review agent");
        assert!(h.ops().is_empty(), "the chat needs nothing from the daemon");

        let model = h.view.read_with(&h.cx, |v, _| v.model().cloned()).unwrap();
        assert_eq!(model.status_label, "Ready for review");
        assert_eq!(model.server.health, HealthCheck::Passed);

        h.draw();
        // State=Review: back and the status, the summary, the worktree with
        // Open, the five views, Actions.
        for selector in [
            "review-panel",
            "review-back",
            "task-panel-status",
            "review-title",
            "review-description",
            "review-more",
            "review-worktree",
            "review-folder",
            "review-switch-chat",
            "review-switch-diff",
            "review-switch-editor",
            "review-switch-browser",
            "review-shell-toggle",
            "review-approve",
            "review-reject",
            "review-content",
        ] {
            assert!(h.cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        // Figma dropped the Server block: serving, the browser row shows its
        // ⌘4 hint, not a state.
        for selector in ["review-server", "review-health", "review-url", "review-server-state"] {
            assert!(h.cx.debug_bounds(selector).is_none(), "{selector} is not drawn");
        }
        let panel = h.cx.debug_bounds("review-panel").unwrap();
        let content = h.cx.debug_bounds("review-content").unwrap();
        assert!(panel.origin.x < content.origin.x, "the panel sits left of the content");
    }

    #[gpui::test]
    fn the_server_state_follows_the_sidebar_row_live(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Loading)), cx));
        let server = |h: &Harness| h.view.read_with(&h.cx, |v, _| v.model().unwrap().server.clone());
        assert_eq!(server(&h).state, ReviewState::Loading);
        assert_eq!(server(&h).health, HealthCheck::Waiting);
        assert_eq!(server(&h).location, None);

        h.update(|v, cx| v.update_row(Some(row(ReviewState::Serving)), cx));
        assert_eq!(server(&h).state, ReviewState::Serving);
        assert_eq!(server(&h).health, HealthCheck::Passed);
        assert_eq!(server(&h).location.as_deref(), Some("hub:4101"));
        assert_eq!(h.sessions.attached().len(), 1, "a refresh doesn't re-attach");

        // A refresh without the row (a cold read) keeps what it knew.
        h.update(|v, cx| v.update_row(None, cx));
        assert_eq!(server(&h).state, ReviewState::Serving);
    }

    #[gpui::test]
    fn diff_and_editor_attach_to_daemon_spawned_sessions(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.click("review-switch-diff");
        assert_eq!(h.ops(), vec![ReviewSessionOp::Ensure { role: ReviewRole::Diff }]);
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/review/review-1/diff"));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        let active = |h: &Harness| h.view.read_with(&h.cx, |v, _| v.panel().unwrap().active());
        assert_eq!(active(&h), ActiveView::Diff);

        h.click("review-switch-editor");
        assert_eq!(h.ops().last(), Some(&ReviewSessionOp::Ensure { role: ReviewRole::Editor }));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/review/review-1/editor"));
        assert_eq!(active(&h), ActiveView::Editor);

        h.click("review-switch-chat");
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/review-1"));
        assert_eq!(h.ops().len(), 2, "Chat ensures nothing");
    }

    #[gpui::test]
    fn a_view_the_daemon_refuses_falls_back_to_chat(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.click("review-switch-diff");
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.panel().unwrap().active()), ActiveView::Chat);
        assert_eq!(h.status_line().as_deref(), Some("couldn't open view: the daemon said no"));
        assert_eq!(h.content_phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn browser_opens_the_url_and_the_folder_reveals(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.click("review-switch-browser");
        h.click("review-folder");
        assert_eq!(*h.ops.opened.lock().unwrap(), vec!["http://localhost:4101", "/w/review-1"]);
        assert_eq!(
            h.view.read_with(&h.cx, |v, _| v.panel().unwrap().active()),
            ActiveView::Chat,
            "the browser is not a content view"
        );
        assert_eq!(h.sessions.attached(), vec!["demo/ws/review-1"]);
    }

    #[gpui::test]
    fn the_browser_waits_for_the_server_to_serve(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Loading)), cx));
        h.draw();
        assert!(h.cx.debug_bounds("review-server-state").is_some(), "loading… where ⌘4 sits");
        h.click("review-switch-browser");
        assert!(h.ops.opened.lock().unwrap().is_empty(), "inert until serving");
        // The server came up: the row opens the URL.
        h.update(|v, cx| v.update_row(Some(row(ReviewState::Serving)), cx));
        h.draw();
        assert!(h.cx.debug_bounds("review-server-state").is_none());
        h.click("review-switch-browser");
        assert_eq!(*h.ops.opened.lock().unwrap(), vec!["http://localhost:4101"]);
    }

    #[gpui::test]
    fn the_shell_opens_beside_the_reviewer_in_the_slot(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.click("review-shell-toggle");
        assert!(h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(
            *h.ops.shell_ops.lock().unwrap(),
            vec![("review-1".to_string(), WorkspaceSessionOp::EnsureShell)]
        );
        assert!(h.sessions.attached().contains(&"demo/ws/review-1/shell".to_string()));
        h.draw();
        assert!(h.cx.debug_bounds("review-shell-pane").is_some());
        assert!(h.cx.debug_bounds("review-split-divider").is_some());
        assert_eq!(
            h.view.read_with(&h.cx, |v, _| v.panel().unwrap().active()),
            ActiveView::Chat,
            "the reviewer stays in the content pane"
        );
        // ⌘5 is the shell: it closes.
        h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.view_shortcut(5, window, cx)));
        h.cx.run_until_parked();
        assert!(!h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(
            h.ops.shell_ops.lock().unwrap().last(),
            Some(&("review-1".to_string(), WorkspaceSessionOp::CloseShell))
        );
        // Leaving the review closes an open shell.
        h.click("review-shell-toggle");
        h.update(|v, cx| v.hide(cx));
        assert_eq!(h.ops.shell_ops.lock().unwrap().len(), 4);
        assert_eq!(
            h.ops.shell_ops.lock().unwrap().last().map(|(_, op)| op.clone()),
            Some(WorkspaceSessionOp::CloseShell)
        );
    }

    #[gpui::test]
    fn view_shortcuts_pick_the_numbered_views(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        let shortcut = |h: &mut Harness, n: u8| {
            h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.view_shortcut(n, window, cx)));
            h.cx.run_until_parked();
        };
        shortcut(&mut h, 2);
        assert_eq!(h.ops(), vec![ReviewSessionOp::Ensure { role: ReviewRole::Diff }], "⌘2 is View Diff");
        shortcut(&mut h, 4);
        assert_eq!(*h.ops.opened.lock().unwrap(), vec!["http://localhost:4101"], "⌘4 is Web Browser");
        shortcut(&mut h, 1);
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.panel().unwrap().active()), ActiveView::Chat);
        shortcut(&mut h, 6);
        assert!(h.events().is_empty(), "no sixth view, and never Approve: {:?}", h.events());
    }

    #[gpui::test]
    fn without_a_url_there_is_no_browser(cx: &mut TestAppContext) {
        let mut h = open(cx);
        let no_url = ReviewInfo {
            review_url: None,
            has_health_check: false,
            ..info()
        };
        h.update(|v, cx| v.show(no_url, Some(row(ReviewState::Serving)), cx));
        h.draw();
        assert!(h.cx.debug_bounds("review-switch-browser").is_none());
        let health = h.view.read_with(&h.cx, |v, _| v.model().unwrap().server.health);
        assert_eq!(health, HealthCheck::NotDeclared);
    }

    #[gpui::test]
    fn approve_and_reject_go_to_the_registry(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.click("review-approve");
        h.click("review-reject");
        assert_eq!(
            h.events(),
            vec![
                ReviewEvent::Command(CommandKind::ApproveReview { id: "t-1".into() }),
                ReviewEvent::Command(CommandKind::RejectReview {
                    id: "t-1".into(),
                    reason: String::new()
                }),
            ]
        );
        assert!(h.ops().is_empty(), "a decision is the window's (the daemon's), not a session op");
    }

    #[gpui::test]
    fn while_merging_the_panel_takes_no_competing_input(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.update(|v, cx| v.set_merging(true, cx));
        h.draw();
        assert!(h.cx.debug_bounds("review-merging").is_some(), "the actions become a busy line");
        assert!(h.cx.debug_bounds("review-approve").is_none());
        h.click("review-switch-diff");
        h.click("review-back");
        h.click("review-switch-browser");
        h.click("review-shell-toggle");
        h.focus_panel();
        h.key("q");
        h.key("escape");
        assert!(h.events().is_empty(), "no Back, Close or decision: {:?}", h.events());
        assert!(h.ops().is_empty(), "no view switch reached the daemon");
        assert!(h.ops.opened.lock().unwrap().is_empty());
        assert!(h.ops.shell_ops.lock().unwrap().is_empty(), "no shell either");
        assert_eq!(h.status_line().as_deref(), Some(shelbi_app::review::MERGE_IN_PROGRESS));

        // The decision failed: the panel is live again.
        h.update(|v, cx| {
            v.set_merging(false, cx);
            v.set_status(Some("approve failed: checks pending".into()), cx);
        });
        h.click("review-approve");
        assert_eq!(h.events(), vec![ReviewEvent::Command(CommandKind::ApproveReview { id: "t-1".into() })]);
    }

    #[gpui::test]
    fn panel_keys_navigate_and_switch(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.focus_panel();
        // Reviewer is selected; one down is View Diff.
        h.key("j");
        h.key("enter");
        assert_eq!(h.ops(), vec![ReviewSessionOp::Ensure { role: ReviewRole::Diff }]);
        // `m` opens the task's detail.
        h.key("m");
        assert_eq!(h.events(), vec![ReviewEvent::OpenTask("t-1".into())]);
        // So does Enter on the More row (up from View Diff; the rows wrap).
        let selected = |h: &Harness| h.view.read_with(&h.cx, |v, _| v.panel().and_then(|p| p.selected_row()));
        for _ in 0..12 {
            if selected(&h) == Some(PanelRow::More) {
                break;
            }
            h.key("k");
        }
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.panel().and_then(|p| p.selected_row())), Some(PanelRow::More));
        h.key("enter");
        assert_eq!(h.events().len(), 2);
        assert_eq!(h.events()[1], ReviewEvent::OpenTask("t-1".into()));
        h.key("escape");
        assert_eq!(h.events().last(), Some(&ReviewEvent::Back));
    }

    #[gpui::test]
    fn more_opens_the_detail_and_the_board_s_text_refreshes_the_summary(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.click("review-more");
        assert_eq!(h.events(), vec![ReviewEvent::OpenTask("t-1".into())]);
        h.click("review-more");
        assert_eq!(h.events().len(), 2, "More opens it every time, it doesn't toggle");

        h.update(|v, cx| v.refresh_task_text("t-1", "Edited title", "Edited body.", cx));
        let model = h.view.read_with(&h.cx, |v, _| v.model().cloned()).unwrap();
        assert_eq!(model.title, "Edited title");
        assert_eq!(model.description, "Edited body.");
        h.update(|v, cx| v.refresh_task_text("t-9", "Other", "Other", cx));
        let model = h.view.read_with(&h.cx, |v, _| v.model().cloned()).unwrap();
        assert_eq!(model.title, "Edited title");
    }

    #[gpui::test]
    fn close_asks_the_daemon_to_tear_the_review_down(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.focus_panel();
        h.key("q");
        assert_eq!(h.ops(), vec![ReviewSessionOp::Close]);
        assert_eq!(h.events(), vec![ReviewEvent::Closed]);
        assert!(!h.view.read_with(&h.cx, |v, _| v.is_shown()));
        assert_eq!(h.content_phase(), SessionPhase::Empty, "the content detached");
    }

    #[gpui::test]
    fn back_leaves_the_review_loaded(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.click("review-back");
        assert_eq!(h.events(), vec![ReviewEvent::Back]);
        assert!(h.ops().is_empty(), "Back ends nothing");
    }

    #[gpui::test]
    fn re_entering_a_review_reattaches_every_time(cx: &mut TestAppContext) {
        let mut h = open(cx);
        for round in 1..=4 {
            h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
            assert_eq!(h.content_phase(), SessionPhase::Live, "round {round}");
            h.update(|v, cx| v.hide(cx));
            assert_eq!(h.content_phase(), SessionPhase::Empty, "round {round}");
        }
        assert_eq!(h.sessions.attached(), vec!["demo/ws/review-1"; 4]);

        // A diff left open is re-attached on the way back in (its session is
        // the daemon's, still running).
        h.update(|v, cx| v.show(info(), None, cx));
        h.click("review-switch-diff");
        h.update(|v, cx| v.hide(cx));
        h.update(|v, cx| v.show(info(), None, cx));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/review/review-1/diff"));
        assert_eq!(h.content_phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn a_failed_attach_is_retried_by_selecting_it_again(cx: &mut TestAppContext) {
        let mut h = open(cx);
        // An old session under the same name refuses (the ENOTCONN re-entry).
        h.sessions.refusing.lock().unwrap().push("demo/ws/review-1".into());
        h.update(|v, cx| v.show(info(), None, cx));
        assert!(matches!(h.content_phase(), SessionPhase::Failed(_)));
        h.sessions.refusing.lock().unwrap().clear();
        h.click("review-switch-chat");
        assert_eq!(h.content_phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn another_review_starts_fresh_on_reviewer(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.click("review-switch-diff");
        let other = ReviewInfo {
            task_id: "t-2".into(),
            slot: "review-2".into(),
            title: "Other".into(),
            ..info()
        };
        h.update(|v, cx| v.show(other, None, cx));
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.task_id().map(str::to_string)).as_deref(), Some("t-2"));
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.panel().unwrap().active()), ActiveView::Chat);
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/review-2"));
    }

    #[gpui::test]
    fn a_remote_slot_offers_no_diff_editor_or_folder(cx: &mut TestAppContext) {
        let mut h = open(cx);
        let remote = ReviewInfo {
            remote_machine: Some("gpu".into()),
            ..info()
        };
        h.update(|v, cx| v.show(remote, None, cx));
        h.draw();
        for gone in ["review-switch-diff", "review-switch-editor", "review-folder"] {
            assert!(h.cx.debug_bounds(gone).is_none(), "{gone} is hidden off the hub");
        }
        assert!(h.cx.debug_bounds("review-switch-chat").is_some());
    }

    #[gpui::test]
    fn finishing_a_decision_closes_the_sessions_and_hides(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.update(|v, cx| v.set_merging(true, cx));
        h.update(|v, cx| v.finish(cx));
        assert_eq!(h.ops(), vec![ReviewSessionOp::Close]);
        assert!(!h.view.read_with(&h.cx, |v, _| v.is_shown()));
        assert!(!h.view.read_with(&h.cx, |v, _| v.is_merging()));
        assert!(h.events().is_empty(), "the window is already moving on");
    }
}
