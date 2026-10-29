//! The task pane's live session (`dt-task-pane-session`): the board's one
//! [`SessionView`], attached to the agent working the pane's task, and the
//! details | session split over it.
//!
//! - **Which session.** The pane's task resolves through the sidebar's rows
//!   ([`shelbi_app::task_session::resolve`]): the workspace working it, or
//!   the review slot it is loaded onto. Nothing else attaches, so a task
//!   assigned to a workspace busy with another task, an idle task, and a
//!   task no longer on the board show no session.
//! - **Attach only.** The view's relauncher is [`NoopRelauncher`]: opening,
//!   switching or closing the pane attaches and detaches this one client and
//!   never relaunches, restarts or dispatches an agent. A session that exits
//!   is re-attached once the daemon brings it back (the shared restart
//!   states), and a remote one reconnects over its relay.
//! - **One at a time.** Another task, or the pane closing, drops the old
//!   attach before the next one starts, so the old task's output is never
//!   shown under the new task.
//! - **Not twice.** Over a screen that already shows the session (the pane
//!   opened from that workspace's or review's More) the pane leaves it out.

use std::sync::Arc;

use gpui::{AnyWindowHandle, AppContext, Bounds, Context, Entity, FocusHandle, Pixels, Subscription, Window};
use shelbi_app::session::NoopRelauncher;
use shelbi_app::task_session::{self, TaskSession};
use shelbi_app::view::{ReviewRow, WorkspaceRow};

use super::issues::{IssuesEvent, IssuesView};
use super::pane_split::PaneSplit;
use super::session_view::{SessionDeps, SessionView, SessionViewEvent};
use crate::refresh::SessionSink;

/// The terminal grid's inset in the pane's session area: the frame's 20pt
/// across, 16pt over the output and 8pt under the input (2570:10612,
/// 2570:10623).
pub const INSET_X: f32 = 20.0;
pub const INSET_TOP: f32 = 16.0;
pub const INSET_BOTTOM: f32 = 8.0;

/// What the main area already shows under a floating pane: the pane leaves
/// out a session that is already on screen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MainShows {
    /// The workspace whose agent the Workspace view shows.
    pub workspace: Option<String>,
    /// The task the Review view shows.
    pub review_task: Option<String>,
}

/// The board's pane session.
pub struct PaneSession {
    view: Entity<SessionView>,
    /// The session attached now (or connecting), `None` when detached.
    attached: Option<TaskSession>,
    workspaces: Vec<WorkspaceRow>,
    reviews: Vec<ReviewRow>,
    /// Whether the window shows the board, or the pane floating over
    /// another screen.
    shown: bool,
    main_shows: MainShows,
    pub(super) split: PaneSplit,
    /// The details | session column as last laid out (the drag's math).
    pub(super) bounds: Option<Bounds<Pixels>>,
    pub(super) dragging: bool,
    /// The resize handle, focusable for its keyboard resize.
    pub(super) handle_focus: FocusHandle,
    _subscription: Subscription,
}

impl PaneSession {
    /// The pane's session view in `window`, on `deps` with its relauncher
    /// replaced by one that never launches anything.
    pub fn new(window: AnyWindowHandle, project: &str, deps: SessionDeps, sink: Option<SessionSink>, cx: &mut Context<IssuesView>) -> Self {
        let deps = SessionDeps {
            relauncher: Arc::new(NoopRelauncher),
            ..deps
        };
        let view = cx.new(|cx| {
            let mut v = SessionView::new(project, shelbi_app::session::SessionRef::Workspace(String::new()), "", window, deps, sink, cx);
            v.set_idle_placeholder(false);
            v.set_inset_x(INSET_X);
            v.set_inset_y(INSET_TOP, INSET_BOTTOM);
            v
        });
        let sub = cx.subscribe(&view, |_, _, ev: &SessionViewEvent, cx| {
            if *ev == SessionViewEvent::Focused {
                cx.emit(IssuesEvent::Focused);
            }
        });
        Self {
            view,
            attached: None,
            workspaces: Vec::new(),
            reviews: Vec::new(),
            shown: false,
            main_shows: MainShows::default(),
            split: PaneSplit::default(),
            bounds: None,
            dragging: false,
            handle_focus: cx.focus_handle(),
            _subscription: sub,
        }
    }

    /// The session view.
    pub fn view(&self) -> &Entity<SessionView> {
        &self.view
    }

    /// The session the pane shows, when it shows one.
    pub fn attached(&self) -> Option<&TaskSession> {
        self.attached.as_ref()
    }

    /// The sidebar's refreshed rows. Returns whether the association changed
    /// enough to repaint.
    pub fn set_rows(&mut self, workspaces: Vec<WorkspaceRow>, reviews: Vec<ReviewRow>) -> bool {
        if self.workspaces == workspaces && self.reviews == reviews {
            return false;
        }
        self.workspaces = workspaces;
        self.reviews = reviews;
        true
    }

    /// Whether the window shows the pane, and what the main area under it
    /// shows. Returns whether that changed.
    pub fn set_shown(&mut self, shown: bool, main_shows: MainShows) -> bool {
        if self.shown == shown && self.main_shows == main_shows {
            return false;
        }
        self.shown = shown;
        self.main_shows = main_shows;
        true
    }

    /// Whether keyboard focus is in the session (its terminal or notice).
    pub fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.attached.is_some() && self.view.read(cx).contains_focus(window, cx)
    }

    /// Bring the attach in line with the pane's task (`None` when the pane
    /// is closed, or its task isn't on the board): attach the session working
    /// it, detach what no longer belongs, and re-attach a session that wasn't
    /// there when a row changed.
    pub fn sync(&mut self, task: Option<&str>, rows_changed: bool, cx: &mut Context<IssuesView>) {
        let want = task
            .filter(|_| self.shown)
            .and_then(|t| task_session::resolve(t, &self.workspaces, &self.reviews))
            .filter(|s| !self.on_screen(s));
        match (want, self.attached.take()) {
            (None, None) => {}
            (None, Some(_)) => self.view.update(cx, |v, cx| v.hide(cx)),
            (Some(want), Some(was)) if want.task == was.task && want.workspace == was.workspace => {
                let remote = want.remote_machine.clone();
                self.view.update(cx, |v, cx| {
                    v.set_remote_machine(remote);
                    // A row change may have brought up a session that wasn't
                    // there (a dispatch), as the Workspace view retries.
                    if rows_changed {
                        v.retry_if_idle(cx);
                    }
                });
                self.attached = Some(want);
            }
            (Some(want), was) => {
                let target = want.target();
                let remote = want.remote_machine.clone();
                self.view.update(cx, |v, cx| {
                    // Drop the old attach first, so nothing of the last
                    // task's session shows under this one.
                    if was.is_some() {
                        v.hide(cx);
                    }
                    v.set_remote_machine(remote);
                    v.show_target(target, cx);
                });
                self.attached = Some(want);
            }
        }
    }

    /// Whether the main area under the pane already shows `session`.
    fn on_screen(&self, session: &TaskSession) -> bool {
        if session.is_review() {
            self.main_shows.review_task.as_deref() == Some(session.task.as_str())
        } else {
            self.main_shows.workspace.as_deref() == Some(session.workspace.as_str())
        }
    }
}

#[cfg(test)]
#[path = "pane_session_tests.rs"]
mod tests;
