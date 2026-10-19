//! The Workspace view (`dt-workspace-view`): a dev workspace's agent session
//! (`<project>/ws/<name>`) in the terminal element, with the user's own shell
//! beside it on demand, and the workspace panel in the sidebar's column
//! (`dt-workspace-sidebar`).
//!
//! It follows the TUI's workspace interface (`crates/shelbi-tui/src/shell/workspace.rs`,
//! `workspace_panel.rs`) and its idle-workspace open (`rt-tui-idle-workspace-open`):
//!
//! - **The panel** takes the navigation sidebar's column while the workspace
//!   works a task, as the review panel does: the view renders as the panel, at
//!   whatever width the window's column gives it, and the window puts
//!   [`WorkspaceView::render_main`] (the terminals) in the main area beside
//!   it, with the sidebar's titlebar and footer around the panel. It is the
//!   Navigation component's State=Workspace (`dt-figma-task-panels`, the
//!   pieces in [`super::task_panel`]): the back button and the task's
//!   column (IN PROGRESS); the title and the description clamped to five
//!   lines, More opening the task detail over the screen (the window
//!   draws it, `dt-more-opens-detail`); the worktree with Open (reveal it in
//!   Finder); then the views, numbered ⌘1 to ⌘N: the agent, View Diff, Edit
//!   in <editor>, and the Shell toggle. Its rows, selection and keys are the
//!   shared [`WorkspacePanelState`] (the TUI's panel navigates by the same
//!   functions).
//! - **An idle workspace** (no task) leaves the navigation sidebar where it
//!   is (Figma "Workspace idle", 2174:851): the main area shows its page, a
//!   toolbar with the state and name, where it lives and a Shell button, a
//!   divider, then the agent pane (the idle placeholder until a dispatch
//!   starts a session). When a task arrives the panel pushes in; when it
//!   ends the panel leaves (its reduced, task-less rows show while it goes).
//! - **The agent.** Selecting a workspace attaches its agent session. An idle
//!   workspace (no session) shows the TUI's idle placeholder and re-attaches
//!   as soon as the sidebar row changes (a dispatch started a session), as the
//!   TUI retries the shown workspace on a change. Nothing is launched by
//!   selecting it: the daemon dispatches agents.
//! - **The view switch**: the agent, View Diff, Edit in <editor>. Diff and
//!   editor are the daemon's `<project>/ws/<name>/{diff,editor}` content
//!   sessions, ensured through `WorkspaceSessionOp::Ensure` and only attached
//!   here; Close (the × on the active one) ends them, and `q` (the TUI's
//!   Close) ends them and goes back. Like the TUI's, they are local
//!   workspaces only.
//! - **The user shell.** The Shell row opens a second terminal beside the
//!   agent, split with a draggable divider ([`super::workspace_layout`]),
//!   attached to the workspace's user shell (`<project>/ws/<name>/shell`, a
//!   login shell in the worktree on the workspace's machine) that the daemon
//!   starts (`WorkspaceSessionOp::EnsureShell`). Toggling it off, leaving the
//!   view, or exiting the shell closes it.
//! - **Remote workspaces.** A workspace on another machine attaches through
//!   that machine's relay (`shelbi_orchestrator::session_connect`), the panel
//!   names the machine, and a dropped SSH connection reconnects
//!   ([`super::session_view`]).
//! - **Back** (the back button, Esc) asks the window to return to the view
//!   the workspace was opened over, leaving its sessions as they are.
//! - **Restart states** are the shared session view's (the Chat view's).
//! - **Drop to paste.** Files dropped on either terminal paste their paths.
//!
//! While the window's column push runs, the window holds the agent pane's
//! attach ([`WorkspaceView::hold_content`], `dt-instant-panel-push`): the
//! panel draws at once, and the session's connect, replay and first layout
//! land after the push has settled.

use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    div, prelude::*, px, AnyElement, AnyWindowHandle, Bounds, Context, Entity, EventEmitter,
    FocusHandle, Focusable, Pixels, SharedString, Subscription, Window,
};
use shelbi_app::session::{SessionRef, WorkspaceContentRelauncher};
use shelbi_app::view::{WorkspaceBadge, WorkspaceDetailsInfo, WorkspaceHeader, WorkspaceRow};
use shelbi_app::workspace_panel::{WorkspacePanelState, WsEffect, WsPanelShape, WsRow};
use shelbi_app::WorkspaceSessionOp;
use shelbi_core::{Column, StatusCategory};

use super::session_view::{OnDeath, SessionDeps, SessionView, SessionViewEvent};
use super::task_panel;
use super::workspace_layout::{render_split, ShellSplit};
use crate::assets::{heroicon, icons, Icon};
use crate::keys::chord_from_keystroke;
use crate::refresh::SessionSink;
use crate::theme::motion::{self, SharedTrack, SlideTrack};
use crate::theme::style::{self, IconButtonSize, Type, Typography, NAV_RADIUS};
use crate::theme::Theme;

/// Which content the left pane shows: the panel's view switch (the TUI's
/// `WsView`).
pub use shelbi_app::workspace_panel::WsSwitch as ContentView;

/// Where keyboard focus sits inside the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WsFocus {
    Panel,
    Content,
}

/// The blocking work the view hands the background executor: reading a
/// workspace's details, asking the daemon for its sessions, revealing its
/// folder. Injectable so the view is tested without a daemon.
pub trait WorkspaceOps: Send + Sync {
    /// The workspace's task, branch and worktree.
    fn details(&self, project: &str, workspace: &str) -> Result<WorkspaceDetailsInfo, String>;
    /// Ask the daemon to start or stop one of the workspace's sessions.
    fn session_op(&self, project: &str, workspace: &str, op: WorkspaceSessionOp) -> Result<(), String>;
    /// Show `path` in Finder.
    fn reveal(&self, path: &str) -> Result<(), String>;
}

/// The real thing: the orchestrator's details reader, the daemon's control
/// socket, and `open`.
pub struct LiveOps;

impl WorkspaceOps for LiveOps {
    fn details(&self, project: &str, workspace: &str) -> Result<WorkspaceDetailsInfo, String> {
        let d = shelbi_orchestrator::workspace_session::workspace_details(project, workspace)
            .map_err(|e| e.to_string())?;
        Ok(WorkspaceDetailsInfo {
            task: d.task.as_ref().map(|t| (t.task.id.clone(), t.task.title.clone())),
            body: d.task.map(|t| t.body).unwrap_or_default(),
            branch: d.branch,
            worktree: d.worktree,
            editor_name: d.editor_name,
        })
    }

    fn session_op(&self, project: &str, workspace: &str, op: WorkspaceSessionOp) -> Result<(), String> {
        shelbi_app::workspace_session(project, workspace, op, &mut |_, _| {})
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn reveal(&self, path: &str) -> Result<(), String> {
        std::process::Command::new("open")
            .arg(path)
            .status()
            .map_err(|e| format!("couldn't run `open`: {e}"))
            .and_then(|s| {
                if s.success() {
                    Ok(())
                } else {
                    Err(format!("`open {path}` exited {s}"))
                }
            })
    }
}

/// What the view needs from the outside world.
#[derive(Clone)]
pub struct WorkspaceDeps {
    pub ops: Arc<dyn WorkspaceOps>,
    /// Builds the deps of each of the view's two session panes.
    pub sessions: Rc<dyn Fn() -> SessionDeps>,
}

impl WorkspaceDeps {
    /// The real daemon and sessions for `project`: agents re-open through the
    /// daemon's relaunch markers, content sessions and the shell are
    /// re-ensured.
    pub fn live(project: &str) -> Self {
        let project = project.to_string();
        Self {
            ops: Arc::new(LiveOps),
            sessions: Rc::new(move || {
                SessionDeps::live_with(Arc::new(WorkspaceContentRelauncher {
                    project: project.clone(),
                }))
            }),
        }
    }
}

/// What the view tells its window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceEvent {
    /// One of its terminals took keyboard focus: the window's focus model
    /// moves to the main area.
    Focused,
    /// Back (the back button, Esc, or `q` after closing the content): return
    /// to the view the workspace was opened over.
    Back,
    /// More: open this task's detail over the screen.
    OpenTask(String),
}

/// The Workspace view for one project window. It shows one workspace at a
/// time; selecting another retargets it.
pub struct WorkspaceView {
    /// The view switches, measured for the current one's sliding chip.
    switch_track: SharedTrack,
    project: String,
    ops: Arc<dyn WorkspaceOps>,
    /// The workspace shown (kept while hidden, so showing it again re-attaches).
    workspace: Option<String>,
    shown: bool,
    /// Its sidebar row: the live state and current task.
    row: Option<WorkspaceRow>,
    details: Option<WorkspaceDetailsInfo>,
    details_pending: bool,
    /// The panel's rows, selection and active view switch entry.
    panel: WorkspacePanelState,
    focus: WsFocus,
    /// The left pane: the agent, or its diff or editor.
    content: Entity<SessionView>,
    /// The user shell beside it.
    shell: Entity<SessionView>,
    shell_open: bool,
    /// The daemon is starting the shell (the pane shows "Starting a shell…").
    shell_starting: bool,
    /// The agent | shell split, and its divider's drag.
    split: ShellSplit,
    /// A failure or note at the foot of the panel (the TUI panel's status
    /// line).
    status_line: Option<String>,
    /// Bumped whenever the shown workspace changes; a background result for
    /// an older one is dropped.
    generation: u64,
    /// The window's push is running: the left pane waits to attach.
    hold: bool,
    /// The attach that waited for the push.
    held: Option<SessionRef>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<WorkspaceEvent> for WorkspaceView {}

impl WorkspaceView {
    pub fn new(
        project: &str,
        window: AnyWindowHandle,
        deps: WorkspaceDeps,
        sink: Option<SessionSink>,
        cx: &mut Context<Self>,
    ) -> Self {
        let make = deps.sessions.clone();
        let content = cx.new(|cx| {
            SessionView::new(
                project,
                SessionRef::Workspace(String::new()),
                "",
                window,
                make(),
                sink.clone(),
                cx,
            )
        });
        let shell = cx.new(|cx| {
            let mut v = SessionView::new(project, SessionRef::workspace_shell(""), "", window, make(), None, cx);
            v.set_on_death(OnDeath::Close);
            v
        });
        let subscriptions = vec![
            cx.subscribe(&content, |this, _, ev: &SessionViewEvent, cx| {
                if *ev == SessionViewEvent::Focused {
                    this.focus = WsFocus::Content;
                    cx.emit(WorkspaceEvent::Focused);
                }
            }),
            cx.subscribe(&shell, |this, _, ev: &SessionViewEvent, cx| match ev {
                SessionViewEvent::Focused => {
                    this.focus = WsFocus::Content;
                    cx.emit(WorkspaceEvent::Focused);
                }
                // The user exited the shell: close the split.
                SessionViewEvent::Ended => {
                    this.shell_open = false;
                    this.shell_starting = false;
                    cx.notify();
                }
            }),
        ];
        Self {
            project: project.to_string(),
            ops: deps.ops,
            workspace: None,
            switch_track: SlideTrack::shared(),
            shown: false,
            row: None,
            details: None,
            details_pending: false,
            panel: WorkspacePanelState::new(&WsPanelShape::for_workspace(false, false, true, true)),
            focus: WsFocus::Panel,
            content,
            shell,
            shell_open: false,
            shell_starting: false,
            split: ShellSplit::default(),
            status_line: None,
            generation: 0,
            hold: false,
            held: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    /// The workspace shown (or last shown).
    pub fn workspace(&self) -> Option<&str> {
        self.workspace.as_deref()
    }

    pub fn is_shown(&self) -> bool {
        self.shown
    }

    /// Show `workspace`, whose sidebar row is `row`. Selecting the one already
    /// shown re-selects its pane (re-opening a dead one, as in the TUI).
    pub fn show(&mut self, workspace: &str, row: Option<WorkspaceRow>, cx: &mut Context<Self>) {
        if self.workspace.as_deref() != Some(workspace) {
            self.close_panes(cx);
            self.generation += 1;
            self.workspace = Some(workspace.to_string());
            self.details = None;
            self.details_pending = false;
            self.status_line = None;
            self.row = None;
            self.panel = WorkspacePanelState::new(&WsPanelShape::for_workspace(false, false, true, true));
            self.focus = WsFocus::Panel;
        }
        self.panel.set_active(ContentView::Agent);
        self.shown = true;
        self.set_row(row, cx);
        let target = self.agent_target();
        self.attach(target, cx);
        cx.notify();
    }

    /// Hold the left pane's attach until [`Self::release_content`] (the
    /// window, while its push runs), or stop holding (attaching what waited).
    pub fn hold_content(&mut self, hold: bool, cx: &mut Context<Self>) {
        if hold {
            self.hold = true;
        } else {
            self.release_content(cx);
        }
    }

    /// The push has settled: attach the pane that waited for it.
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

    /// Point the left pane at `target`, or keep it for after the push.
    fn attach(&mut self, target: SessionRef, cx: &mut Context<Self>) {
        if self.hold {
            self.held = Some(target);
            return;
        }
        self.content.update(cx, |c, cx| c.show_target(target, cx));
    }

    /// Hide the view: detach the panes and close the user shell.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.split.dragging = false;
        self.held = None;
        self.close_panes(cx);
        cx.notify();
    }

    /// The sidebar refreshed: track the row's live state, re-attach an idle
    /// agent when its row changed, and re-read the details when the task did.
    pub fn update_row(&mut self, row: Option<WorkspaceRow>, cx: &mut Context<Self>) {
        if !self.shown || row == self.row {
            return;
        }
        let changed = self.row.is_some();
        self.set_row(row, cx);
        if changed {
            self.content.update(cx, |c, cx| {
                c.retry_if_idle(cx);
            });
        }
        cx.notify();
    }

    /// The palette chord the idle placeholder names.
    pub fn set_palette_chord(&mut self, chord: &str, cx: &mut Context<Self>) {
        self.content.update(cx, |c, _| c.set_palette_chord(chord));
    }

    /// The project is being quit: the exits that follow are not restarted.
    pub fn note_deliberate_close(&mut self, cx: &mut Context<Self>) {
        self.content.update(cx, |c, _| c.note_deliberate_close());
    }

    /// The deliberate teardown didn't happen after all.
    pub fn clear_deliberate_close(&mut self, cx: &mut Context<Self>) {
        self.content.update(cx, |c, _| c.clear_deliberate_close());
    }

    /// Whether a workspace whose sidebar row is `row` shows the panel in
    /// the sidebar's column: only while it works a task. An idle one keeps
    /// the sidebar and shows its page in the main area.
    pub fn wants_panel(row: Option<&WorkspaceRow>) -> bool {
        row.is_some_and(|r| r.current_task.is_some())
    }

    /// Whether the shown workspace has the panel (see [`Self::wants_panel`]).
    pub fn has_panel(&self) -> bool {
        Self::wants_panel(self.row.as_ref())
    }

    /// Where keyboard focus goes when the view takes it: the agent pane once
    /// the user moved there, else the panel (as the review's). Without the
    /// panel (an idle workspace's page) it is the agent pane, as the chat's.
    pub fn focus_target(&self, cx: &gpui::App) -> FocusHandle {
        if !self.has_panel() {
            return self.content.read(cx).focus_target(cx);
        }
        if self.focus == WsFocus::Content && self.content.read(cx).pane().is_some() {
            self.content.read(cx).focus_target(cx)
        } else {
            self.focus_handle.clone()
        }
    }

    /// Give the panel keyboard focus (the sidebar chord while the panel holds
    /// the sidebar's column).
    pub fn focus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus = WsFocus::Panel;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Whether keyboard focus is in the view: its panel or one of its
    /// terminals.
    pub fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus_handle.is_focused(window)
            || self.content.read(cx).contains_focus(window, cx)
            || self.shell.read(cx).contains_focus(window, cx)
    }

    /// The panel's navigation state.
    #[cfg(any(test, feature = "visual-check"))]
    pub fn panel(&self) -> &WorkspacePanelState {
        &self.panel
    }

    /// The header as shown now.
    pub fn header(&self) -> Option<WorkspaceHeader> {
        let row = self.row.clone().or_else(|| {
            self.workspace.as_ref().map(|name| WorkspaceRow {
                name: name.clone(),
                machine: String::new(),
                is_remote: false,
                current_task: None,
                agent: None,
                badge: WorkspaceBadge::Loading,
            })
        })?;
        Some(WorkspaceHeader::build(&row, self.details.as_ref()))
    }

    /// Whether the workspace is working a task (the panel shows the task,
    /// the view switch and the shell only then).
    fn has_task(&self) -> bool {
        self.row.as_ref().is_some_and(|r| r.current_task.is_some())
    }

    fn is_remote(&self) -> bool {
        self.row.as_ref().is_some_and(|r| r.is_remote)
    }

    fn agent_target(&self) -> SessionRef {
        SessionRef::Workspace(self.workspace.clone().unwrap_or_default())
    }

    fn set_row(&mut self, row: Option<WorkspaceRow>, cx: &mut Context<Self>) {
        let remote = row.as_ref().filter(|r| r.is_remote).map(|r| r.machine.clone());
        self.content.update(cx, |c, _| c.set_remote_machine(remote.clone()));
        self.shell.update(cx, |c, _| c.set_remote_machine(remote));
        let stale = match &row {
            Some(r) => WorkspaceHeader::needs_details(r, self.details.as_ref()),
            None => self.details.is_none(),
        };
        self.row = row;
        self.sync_panel();
        if stale {
            self.request_details(cx);
        }
    }

    /// The details, when they describe what the row shows now (its task, or
    /// the idle worktree).
    fn current_details(&self) -> Option<&WorkspaceDetailsInfo> {
        let details = self.details.as_ref()?;
        let row = self.row.as_ref()?;
        let resolved = details.task.as_ref().map(|(id, _)| id.as_str());
        (row.current_task.as_deref() == resolved).then_some(details)
    }

    /// The panel's rows for what the workspace is doing now.
    fn panel_shape(&self) -> WsPanelShape {
        let has_body = self.current_details().is_some_and(|d| !d.body.trim().is_empty());
        WsPanelShape::for_workspace(self.has_task(), has_body, !self.is_remote(), true)
    }

    fn sync_panel(&mut self) {
        let shape = self.panel_shape();
        self.panel.sync(&shape);
    }

    /// Read the workspace's details off the UI thread (one read at a time; a
    /// row change while one is in flight is caught by the next refresh).
    fn request_details(&mut self, cx: &mut Context<Self>) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        if self.details_pending {
            return;
        }
        self.details_pending = true;
        let ops = self.ops.clone();
        let project = self.project.clone();
        let generation = self.generation;
        let read = cx
            .background_executor()
            .spawn(async move { ops.details(&project, &workspace) });
        cx.spawn(async move |this, cx| {
            let result = read.await;
            this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.details_pending = false;
                match result {
                    Ok(details) => this.details = Some(details),
                    Err(e) => tracing::warn!(error = %e, "couldn't read the workspace's details"),
                }
                this.sync_panel();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run a daemon op for the shown workspace off the UI thread, then `done`
    /// with its result (dropped if the view has moved to another workspace).
    fn session_op(
        &mut self,
        op: WorkspaceSessionOp,
        cx: &mut Context<Self>,
        done: impl FnOnce(&mut Self, Result<(), String>, &mut Context<Self>) + 'static,
    ) {
        let Some(workspace) = self.workspace.clone() else {
            return;
        };
        let ops = self.ops.clone();
        let project = self.project.clone();
        let generation = self.generation;
        let work = cx
            .background_executor()
            .spawn(async move { ops.session_op(&project, &workspace, op) });
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

    // --- the panel ---------------------------------------------------------

    /// Carry out a panel effect.
    fn apply(&mut self, effect: WsEffect, window: &mut Window, cx: &mut Context<Self>) {
        match effect {
            WsEffect::None => {}
            WsEffect::Back => cx.emit(WorkspaceEvent::Back),
            WsEffect::Close => {
                self.close_content(cx);
                cx.emit(WorkspaceEvent::Back);
            }
            WsEffect::RevealFolder => self.reveal_folder(cx),
            WsEffect::ShowAgent => self.select_view(ContentView::Agent, cx),
            WsEffect::ShowDiff => self.select_view(ContentView::Diff, cx),
            WsEffect::ShowEditor => self.select_view(ContentView::Editor, cx),
            // More opens the task's detail over the screen.
            WsEffect::ShowDescription => {
                if let Some(task) = self.row.as_ref().and_then(|r| r.current_task.clone()) {
                    cx.emit(WorkspaceEvent::OpenTask(task));
                }
            }
            WsEffect::ToggleShell => self.toggle_shell(window, cx),
        }
    }

    /// A click on a panel row.
    fn click(&mut self, row: WsRow, window: &mut Window, cx: &mut Context<Self>) {
        self.focus = WsFocus::Panel;
        window.focus(&self.focus_handle, cx);
        let effect = self.panel.click(row);
        self.apply(effect, window, cx);
        cx.notify();
    }

    /// ⌘N (the window's chord while the panel holds the column): the
    /// panel's N-th view, as its hints number them. Focus stays where it is.
    pub fn view_shortcut(&mut self, n: u8, window: &mut Window, cx: &mut Context<Self>) {
        let row = (n as usize)
            .checked_sub(1)
            .and_then(|i| self.panel.rows().iter().copied().filter(|r| is_view_row(*r)).nth(i));
        if let Some(row) = row {
            let effect = self.panel.click(row);
            self.apply(effect, window, cx);
            cx.notify();
        }
    }

    /// A key while the panel has focus.
    fn panel_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_keystroke(&event.keystroke) else {
            return;
        };
        // Tab moves to the agent's terminal once it is live (the TUI's).
        if chord.code == shelbi_state::keymap::Key::Tab && chord.mods.is_empty() {
            if self.content.read(cx).pane().is_some() {
                self.focus = WsFocus::Content;
                let target = self.content.read(cx).focus_target(cx);
                window.focus(&target, cx);
            }
            cx.stop_propagation();
            return;
        }
        let before = self.panel.selected();
        let effect = self.panel.handle_key(chord);
        if effect == WsEffect::None && before == self.panel.selected() {
            // Not the panel's key: let the window have it.
            return;
        }
        cx.stop_propagation();
        self.apply(effect, window, cx);
        cx.notify();
    }

    // --- the view switch (the TUI panel's Agent / View Diff / Edit) --------

    /// Show the agent, its diff, or its editor in the left pane. Diff and
    /// editor are ensured through the daemon first; a failure falls back to
    /// the agent with the error on the status line, as the TUI does.
    pub fn select_view(&mut self, view: ContentView, cx: &mut Context<Self>) {
        self.status_line = None;
        let Some(role) = view.role() else {
            self.show_agent(cx);
            return;
        };
        self.panel.set_active(view);
        cx.notify();
        self.session_op(WorkspaceSessionOp::Ensure { role }, cx, move |this, result, cx| {
            let workspace = this.workspace.clone().unwrap_or_default();
            match result {
                Ok(()) if this.panel.active() == view && this.shown => {
                    let target = SessionRef::WorkspaceContent {
                        workspace,
                        role: role.as_str().to_string(),
                    };
                    this.attach(target, cx);
                }
                Ok(()) => {}
                Err(e) => {
                    this.status_line = Some(format!("couldn't open view: {e}"));
                    this.show_agent(cx);
                }
            }
        });
    }

    /// Show the agent in the left pane (leaving the status line alone).
    fn show_agent(&mut self, cx: &mut Context<Self>) {
        self.panel.set_active(ContentView::Agent);
        let target = self.agent_target();
        self.attach(target, cx);
        cx.notify();
    }

    /// End the diff and editor sessions and show the agent (the × on the
    /// active entry; `q` also goes back, as the TUI panel's Close).
    pub fn close_content(&mut self, cx: &mut Context<Self>) {
        self.select_view(ContentView::Agent, cx);
        self.session_op(WorkspaceSessionOp::Close, cx, |this, result, _| {
            if let Err(e) = result {
                this.status_line = Some(format!("couldn't close the views: {e}"));
            }
        });
    }

    // --- the user shell ----------------------------------------------------

    /// Open or close the user shell beside the agent.
    pub fn toggle_shell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.shell_open {
            self.close_shell(cx);
            let target = self.focus_target(cx);
            window.focus(&target, cx);
        } else {
            self.open_shell(cx);
        }
        cx.notify();
    }

    /// The panel row the keys are on (the window's tests).
    #[cfg(test)]
    pub fn panel_selected_row(&self) -> Option<WsRow> {
        self.panel.selected_row()
    }

    #[cfg(any(test, feature = "visual-check"))]
    pub fn shell_open(&self) -> bool {
        self.shell_open
    }

    /// What the agent pane says instead of a terminal (the offscreen check).
    #[cfg(feature = "visual-check")]
    pub fn content_notice(&self, cx: &gpui::App) -> Option<String> {
        self.content.read(cx).notice()
    }

    fn open_shell(&mut self, cx: &mut Context<Self>) {
        if self.workspace.is_none() {
            return;
        }
        self.shell_open = true;
        self.shell_starting = true;
        self.status_line = None;
        self.session_op(WorkspaceSessionOp::EnsureShell, cx, |this, result, cx| {
            if !this.shell_open {
                return;
            }
            this.shell_starting = false;
            match result {
                Ok(()) => {
                    let target = SessionRef::workspace_shell(this.workspace.as_deref().unwrap_or_default());
                    this.shell.update(cx, |s, cx| s.show_target(target, cx));
                }
                Err(e) => {
                    this.shell_open = false;
                    this.status_line = Some(format!("couldn't open a shell: {e}"));
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
        self.session_op(WorkspaceSessionOp::CloseShell, cx, |_, result, _| {
            if let Err(e) = result {
                tracing::warn!(error = %e, "couldn't close the user shell");
            }
        });
    }

    /// Detach both panes and close the shell (leaving the view or the
    /// workspace).
    fn close_panes(&mut self, cx: &mut Context<Self>) {
        self.close_shell(cx);
        self.content.update(cx, |c, cx| c.hide(cx));
    }

    // --- the panel's other affordances ---------------------------------------

    /// The task's title and description as the board reads them now (the
    /// detail opened over the screen edited them): the panel shows them
    /// when they're its task's.
    pub fn refresh_task_text(&mut self, task: &str, title: &str, body: &str, cx: &mut Context<Self>) {
        let Some(details) = self.details.as_mut() else {
            return;
        };
        let Some((id, shown_title)) = details.task.as_mut() else {
            return;
        };
        if id != task || (shown_title == title && details.body == body) {
            return;
        }
        *shown_title = title.to_string();
        details.body = body.to_string();
        self.sync_panel();
        cx.notify();
    }

    fn reveal_folder(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.details.as_ref().map(|d| d.worktree.clone()).filter(|p| !p.is_empty())
        else {
            self.status_line = Some("no workspace worktree to reveal".into());
            cx.notify();
            return;
        };
        let ops = self.ops.clone();
        let work = cx.background_executor().spawn(async move { ops.reveal(&path) });
        cx.spawn(async move |this, cx| {
            if let Err(e) = work.await {
                this.update(cx, |this, cx| {
                    this.status_line = Some(format!("couldn't reveal the folder: {e}"));
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    // --- rendering ---------------------------------------------------------

    fn render_panel(&self, chip: Option<Bounds<Pixels>>, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("workspace-panel")
            .debug_selector(|| "workspace-panel".into())
            .track_focus(&self.focus_handle)
            .key_context("WorkspacePanel")
            .on_key_down(cx.listener(Self::panel_key))
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .type_style(Type::Base)
            .text_color(theme.foreground);
        let Some(header) = self.header() else {
            return root;
        };
        let details = self.current_details();
        let editor_name = details
            .map(|d| d.editor_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Editor".into());
        let agent_name = header.agent.clone().unwrap_or_else(|| "Agent".into());
        let panel = &self.panel;
        let focused = self.focus == WsFocus::Panel;
        let selected = |row: WsRow| focused && panel.selected_row() == Some(row);
        let listen = |row: WsRow| cx.listener(move |this: &mut Self, _: &gpui::ClickEvent, window, cx| this.click(row, window, cx));

        // The back button and the task's status (its column: In Progress).
        let back = task_panel::back_button("ws-back", selected(WsRow::Back), theme).on_click(listen(WsRow::Back));
        let status_label = Column::in_progress().default_status_name().to_string();
        let status = task_panel::status_row(
            back,
            header.task_id.is_some().then_some((StatusCategory::Active, status_label.as_str())),
            theme,
        );

        // The issue summary: the title, five lines of description, More.
        let summary = header.task_id.clone().map(|id| {
            let preview = details
                .map(|d| shelbi_app::review::description_text(&d.body))
                .unwrap_or_default();
            let more = panel.rows().contains(&WsRow::More).then(|| {
                task_panel::more_link("ws-more", selected(WsRow::More), theme)
                    .on_click(listen(WsRow::More))
            });
            let title = header.title.clone().filter(|t| !t.is_empty()).unwrap_or(id);
            task_panel::summary(
                ("workspace-title", "workspace-description"),
                title,
                preview,
                more,
                theme,
            )
        });

        // The worktree, with Open when it can be revealed (a local one); a
        // remote one names its machine in the path's tooltip.
        let worktree = self.details.as_ref().map(|d| d.worktree.clone()).filter(|w| !w.is_empty());
        let folder = worktree.map(|path| {
            let local = panel.rows().contains(&WsRow::Folder);
            let open = local.then(|| task_panel::open_button("ws-folder", theme).on_click(listen(WsRow::Folder)));
            let row = task_panel::worktree_row("workspace-worktree", &path, open, selected(WsRow::Folder), 8.0, theme);
            let tip = if header.is_remote {
                format!("{path} on {}", header.machine)
            } else {
                path
            };
            div()
                .id("workspace-worktree-tip")
                .flex_none()
                .tooltip(style::tooltip(tip))
                .child(row)
        });

        // The views: the agent, its diff and editor, then the shell, ⌘1…⌘N.
        let active = panel.active();
        let mut shortcut = 0u8;
        let mut next = || {
            shortcut += 1;
            (shortcut <= 9).then_some(shortcut)
        };
        let mut views = Vec::new();
        for row in panel.rows().iter().copied() {
            match row {
                WsRow::Switch(item) => {
                    let (id, icon) = switch_id_icon(item);
                    let on = active == item;
                    let painted = style::row_state_rounded(
                        div().id(id).debug_selector(move || id.into()),
                        id,
                        on,
                        selected(row) && chip.is_none(),
                        chip.is_none(),
                        px(NAV_RADIUS),
                        theme,
                    )
                    .child(motion::probe(&self.switch_track, id));
                    let close = (on && item != ContentView::Agent).then(|| {
                        style::icon_button("ws-close-content", icons::X_MARK, IconButtonSize::Small, false, theme)
                            .tooltip(style::tooltip("Close"))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_content(cx);
                            }))
                    });
                    let n = next();
                    views.push(
                        task_panel::view_row(painted, icon, item.label(&agent_name, &editor_name), n, None, on, theme)
                            .children(close)
                            .on_click(listen(row))
                            .into_any_element(),
                    );
                }
                WsRow::Shell => {
                    let open = self.shell_open;
                    let painted = style::row_state_rounded(
                        div().id("ws-shell-toggle").debug_selector(|| "ws-shell-toggle".into()),
                        "ws-shell-toggle",
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
                            .debug_selector(|| "ws-shell-state".into())
                            .flex_none()
                            .px(px(4.0))
                            .type_style(Type::XSmall)
                            .text_color(theme.muted)
                            .child("starting…")
                            .into_any_element()
                    });
                    let n = next();
                    views.push(
                        task_panel::view_row(painted, icons::COMMAND_LINE, "Shell", n, starting, false, theme)
                            .on_click(listen(row))
                            .into_any_element(),
                    );
                }
                _ => {}
            }
        }
        let views = (!views.is_empty()).then(|| {
            task_panel::views_block()
                .child(motion::origin_probe(&self.switch_track))
                .children(chip.map(|b| {
                    motion::floating_chip(b, |d| {
                        style::nav_chip(d.rounded(px(NAV_RADIUS)), theme)
                    })
                }))
                .children(views)
        });

        root.child(status)
            .children(summary)
            .children(folder)
            .children(views)
            .when_some(self.status_line.clone(), |d, line| {
                d.child(task_panel::note("workspace-status", line, theme.warning))
            })
    }

    fn render_panes(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
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
        render_split(
            "ws",
            &self.split,
            |this: &mut Self| &mut this.split,
            self.content.clone().into_any_element(),
            shell,
            theme,
            cx,
        )
    }
}

/// Whether `row` is one of the views (numbered ⌘1…⌘N): the view switch and
/// the shell.
fn is_view_row(row: WsRow) -> bool {
    matches!(row, WsRow::Switch(_) | WsRow::Shell)
}

/// A view switch entry's element id and icon (the agent is the sidebar's
/// chat bubble, as its Orchestrator row).
fn switch_id_icon(item: ContentView) -> (&'static str, Icon) {
    match item {
        ContentView::Agent => ("ws-switch-agent", icons::CHAT_BUBBLE_OVAL_LEFT_ELLIPSIS),
        ContentView::Diff => ("ws-switch-diff", icons::ARROWS_RIGHT_LEFT),
        ContentView::Editor => ("ws-switch-editor", icons::PENCIL_SQUARE),
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WorkspaceView {
    /// An idle workspace's toolbar (Figma 2174:996): the state and the name,
    /// the Shell button at the trailing edge, then where it lives (`machine
    /// · worktree`) in X-Small muted.
    fn render_toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        let header = self.header()?;
        let dec = header.state_decoration();
        let mut machine = header.machine.clone();
        if header.is_remote {
            machine.push_str(" (remote)");
        }
        let worktree = self.details.as_ref().map(|d| home_relative(&d.worktree)).filter(|w| !w.is_empty());
        let place = match worktree {
            Some(path) if machine.is_empty() => path,
            Some(path) => format!("{machine} · {path}"),
            None => header.location_line(),
        };
        let open = self.shell_open;
        // The frame's Shell: 8 across, 4 down, 8 between a 14pt icon and the
        // X-Small label (24pt tall).
        let shell = style::button_base("ws-shell-button", style::ButtonKind::Quiet, false, theme)
            .h(px(24.0))
            .px_2()
            .gap_2()
            .when(open, |d| d.bg(theme.background))
            .type_style(Type::XSmall)
            .text_color(if open { theme.text_loud } else { theme.muted })
            .tooltip(style::tooltip(if open { "Close the shell" } else { "Open a shell in the worktree" }))
            .on_click(cx.listener(|this, _, window, cx| this.toggle_shell(window, cx)))
            .child(heroicon(icons::COMMAND_LINE_SMALL).text_color(if open { theme.text_loud } else { theme.muted }))
            .child("Shell");
        Some(
            div()
                .debug_selector(|| "workspace-toolbar".into())
                .flex()
                .flex_col()
                .flex_none()
                .gap_1()
                .px_4()
                .py_2()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap_2()
                        .h(px(28.0))
                        .min_w_0()
                        .type_style(Type::Base)
                        .child(
                            div()
                                .debug_selector(|| "workspace-state".into())
                                .flex_none()
                                .text_color(theme.muted)
                                .child(format!("{} {}", dec.glyph, header.state.label().to_lowercase())),
                        )
                        .child(
                            div()
                                .debug_selector(|| "workspace-name".into())
                                .min_w_0()
                                .truncate()
                                .type_style(Type::BaseMedium)
                                .text_color(theme.text_loud)
                                .child(header.name.clone()),
                        )
                        .child(div().flex_1())
                        .child(shell),
                )
                .child(
                    div()
                        .debug_selector(|| "workspace-place".into())
                        .min_w_0()
                        .truncate()
                        .type_style(Type::XSmall)
                        .text_color(theme.muted)
                        .child(place),
                )
                .when_some(self.status_line.clone(), |d, line| {
                    d.child(
                        div()
                            .debug_selector(|| "workspace-status".into())
                            .type_style(Type::XSmall)
                            .text_color(theme.warning)
                            .child(line),
                    )
                }),
        )
    }

    /// The main area: the agent (or its diff or editor) and the user shell
    /// when open, under a divider (the
    /// Workspace frames' 1px rule at the top, 2167:653). Without the panel,
    /// the idle page's toolbar heads it.
    pub fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let toolbar = if self.has_panel() { None } else { self.render_toolbar(&theme, cx) };
        div()
            .id("workspace-view")
            .debug_selector(|| "workspace-view".into())
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .size_full()
            .bg(theme.canvas)
            .children(toolbar)
            .child(div().flex_none().h(px(1.0)).bg(theme.divider))
            // Waiting for the push to settle: an empty canvas, not the
            // pane's "no session" notice.
            .children((self.held.is_none()).then(|| self.render_panes(&theme, cx)))
            .into_any_element()
    }
}

/// `path` with the home directory written `~`, as the idle toolbar shows a
/// worktree.
fn home_relative(path: &str) -> String {
    let home = std::env::var("HOME").unwrap_or_default();
    match path.strip_prefix(home.as_str()) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => format!("~{rest}"),
        _ => path.to_string(),
    }
}

/// The view itself renders as the panel; the window sizes and places it.
impl Render for WorkspaceView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // The current view switch's chip slides when the main area changes.
        let chip = {
            let key = self.header().map(|_| match self.panel.active() {
                ContentView::Agent => "ws-switch-agent",
                ContentView::Diff => "ws-switch-diff",
                ContentView::Editor => "ws-switch-editor",
            });
            let mut track = self.switch_track.borrow_mut();
            track.select(key.map(SharedString::from), Instant::now(), || cx.reduce_motion());
            let chip = track.frame(Instant::now());
            if chip.is_some() {
                window.request_animation_frame();
            }
            chip
        };
        self.render_panel(chip, &theme, cx)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::cell::Cell;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use futures::channel::mpsc::{unbounded, UnboundedSender};
    use gpui::{MouseButton, TestAppContext, VisualTestContext, WindowAppearance};
    use shelbi_app::session::{
        self as app_session, ConnectFailure, IdleInfo, ReconnectPolicy, Relauncher, RetryPolicy,
    };
    use shelbi_app::theme::ThemePreference;
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

    /// Sessions by name: a name in `idle` (until removed) or `unreachable`
    /// answers that way, anything else attaches.
    #[derive(Default)]
    pub(crate) struct FakeSessions {
        idle: Mutex<Vec<String>>,
        unreachable: Mutex<Vec<String>>,
        /// Every attach: the session name and its event sender.
        attaches: Mutex<Vec<(String, UnboundedSender<SessionEvent>)>>,
    }

    impl FakeSessions {
        pub(crate) fn attached(&self) -> Vec<String> {
            self.attaches.lock().unwrap().iter().map(|(n, _)| n.clone()).collect()
        }

        /// The attaches still connected (their pane hasn't dropped them).
        pub(crate) fn live(&self) -> Vec<String> {
            self.attaches
                .lock()
                .unwrap()
                .iter()
                .filter(|(_, tx)| !tx.is_closed())
                .map(|(n, _)| n.clone())
                .collect()
        }

        /// The newest attach of `name`.
        fn stream(&self, name: &str) -> UnboundedSender<SessionEvent> {
            let attaches = self.attaches.lock().unwrap();
            attaches.iter().rev().find(|(n, _)| n == name).unwrap().1.clone()
        }

        /// Drop every stream of `name`: their connections close with no exit.
        fn drop_streams(&self, name: &str) {
            self.attaches.lock().unwrap().retain(|(n, _)| n != name);
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
            if self.unreachable.lock().unwrap().contains(&name) {
                return Err(ConnectFailure::Unreachable("`gpu` is unreachable".into()));
            }
            if self.idle.lock().unwrap().contains(&name) {
                return Err(ConnectFailure::Idle(IdleInfo {
                    name: target.workspace().unwrap_or_default().to_string(),
                    machine: "hub".into(),
                    branch: Some("main".into()),
                }));
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
        details: Mutex<Option<WorkspaceDetailsInfo>>,
        ops: Mutex<Vec<WorkspaceSessionOp>>,
        fail_ops: Mutex<bool>,
        revealed: Mutex<Vec<String>>,
    }

    impl WorkspaceOps for FakeOps {
        fn details(&self, _project: &str, _workspace: &str) -> Result<WorkspaceDetailsInfo, String> {
            self.details.lock().unwrap().clone().ok_or_else(|| "no details".to_string())
        }
        fn session_op(&self, _project: &str, _workspace: &str, op: WorkspaceSessionOp) -> Result<(), String> {
            self.ops.lock().unwrap().push(op);
            if *self.fail_ops.lock().unwrap() {
                Err("the daemon said no".into())
            } else {
                Ok(())
            }
        }
        fn reveal(&self, path: &str) -> Result<(), String> {
            self.revealed.lock().unwrap().push(path.to_string());
            Ok(())
        }
    }

    /// The window's layout in small: the panel in a sidebar-width column and
    /// the main area beside it.
    struct Host {
        view: Entity<WorkspaceView>,
    }

    impl Render for Host {
        fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let main = self.view.update(cx, |v, cx| v.render_main(cx));
            div()
                .flex()
                .flex_row()
                .size_full()
                .child(div().flex_none().w(px(280.0)).h_full().child(self.view.clone()))
                .child(div().flex().flex_1().min_w_0().h_full().child(main))
        }
    }

    struct Harness {
        view: Entity<WorkspaceView>,
        events: Rc<std::cell::RefCell<Vec<WorkspaceEvent>>>,
        cx: VisualTestContext,
        sessions: Arc<FakeSessions>,
        ops: Arc<FakeOps>,
        now: Rc<Cell<Instant>>,
    }

    impl Harness {
        fn update<R>(&mut self, f: impl FnOnce(&mut WorkspaceView, &mut Context<WorkspaceView>) -> R) -> R {
            let r = self.view.update(&mut self.cx, f);
            self.cx.run_until_parked();
            r
        }

        fn content_phase(&mut self) -> SessionPhase {
            self.cx.update(|_, cx| {
                let content = self.view.read(cx).content.clone();
                content.read(cx).phase()
            })
        }

        fn shell_phase(&mut self) -> SessionPhase {
            self.cx.update(|_, cx| {
                let shell = self.view.read(cx).shell.clone();
                shell.read(cx).phase()
            })
        }

        fn ops(&self) -> Vec<WorkspaceSessionOp> {
            self.ops.ops.lock().unwrap().clone()
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
            self.draw();
            self.cx.update(|window, cx| self.view.update(cx, |v, cx| v.focus_panel(window, cx)));
            self.cx.run_until_parked();
        }

        fn events(&self) -> Vec<WorkspaceEvent> {
            self.events.borrow().clone()
        }

        fn active(&self) -> ContentView {
            self.view.read_with(&self.cx, |v, _| v.panel.active())
        }

        fn advance(&mut self, by: Duration) {
            self.now.set(self.now.get() + by);
            self.cx.executor().advance_clock(by);
            self.cx.run_until_parked();
        }
    }

    /// Deps over fake sessions (every attach succeeds) and fake daemon ops
    /// that answer with [`details_for_t1`], for the window's tests.
    pub(crate) fn fake_deps() -> (WorkspaceDeps, Arc<FakeSessions>, Arc<FakeOps>) {
        let sessions = Arc::new(FakeSessions::default());
        let ops = Arc::new(FakeOps::default());
        *ops.details.lock().unwrap() = Some(details_for_t1());
        let s = sessions.clone();
        let deps = WorkspaceDeps {
            ops: ops.clone(),
            sessions: Rc::new(move || SessionDeps {
                connector: s.clone(),
                relauncher: s.clone(),
                retry: RetryPolicy::default(),
                reconnect: ReconnectPolicy::default(),
                clock: Rc::new(Instant::now),
            }),
        };
        (deps, sessions, ops)
    }

    pub(crate) fn row(name: &str, task: Option<&str>, badge: WorkspaceBadge, remote: bool) -> WorkspaceRow {
        WorkspaceRow {
            name: name.into(),
            machine: if remote { "gpu" } else { "hub" }.into(),
            is_remote: remote,
            current_task: task.map(str::to_string),
            agent: task.map(|_| "developer".to_string()),
            badge,
        }
    }

    fn working() -> WorkspaceRow {
        row("alpha", Some("t-1"), WorkspaceBadge::Working, false)
    }

    fn details_for_t1() -> WorkspaceDetailsInfo {
        WorkspaceDetailsInfo {
            task: Some(("t-1".into(), "Cold-start cache".into())),
            body: "## Summary\n\nWarm the cache.".into(),
            branch: Some("jlong/t-1".into()),
            worktree: "/w/alpha".into(),
            editor_name: "Vim".into(),
        }
    }

    fn open(cx: &mut TestAppContext) -> Harness {
        cx.update(|cx| {
            cx.set_global(Theme::resolve(ThemePreference::Dark, WindowAppearance::Dark));
            gpui_kit::init(cx);
        });
        let sessions = Arc::new(FakeSessions::default());
        let ops = Arc::new(FakeOps::default());
        *ops.details.lock().unwrap() = Some(details_for_t1());
        let now = Rc::new(Cell::new(Instant::now()));
        let deps = {
            let sessions = sessions.clone();
            let now = now.clone();
            WorkspaceDeps {
                ops: ops.clone(),
                sessions: Rc::new(move || {
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
        let deps = Cell::new(Some(deps));
        let events = Rc::new(std::cell::RefCell::new(Vec::new()));
        let sink = events.clone();
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let handle = window.window_handle();
                let view = cx.new(|cx| WorkspaceView::new("demo", handle, deps.take().unwrap(), None, cx));
                cx.subscribe(&view, move |_, ev: &WorkspaceEvent, _| sink.borrow_mut().push(ev.clone()))
                    .detach();
                cx.new(|_| Host { view })
            })
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let host = window.root(&mut vcx).unwrap();
        let view = host.read_with(&vcx, |h, _| h.view.clone());
        vcx.run_until_parked();
        Harness {
            view,
            events,
            cx: vcx,
            sessions,
            ops,
            now,
        }
    }

    #[gpui::test]
    fn a_working_workspace_shows_its_agent_live_beside_its_panel(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        assert_eq!(h.sessions.attached(), vec!["demo/ws/alpha"]);
        assert!(h.ops().is_empty(), "attaching the agent asks the daemon for nothing");

        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.task_line().as_deref(), Some("t-1  Cold-start cache"));
        assert_eq!(header.location_line(), "hub · jlong/t-1");
        assert_eq!(header.state.label(), "Working");

        h.draw();
        // State=Workspace: back and the status, the summary, the worktree
        // with Open, the views.
        for selector in [
            "workspace-panel",
            "ws-back",
            "task-panel-status",
            "workspace-title",
            "workspace-description",
            "ws-more",
            "workspace-worktree",
            "ws-folder",
            "ws-switch-agent",
            "ws-switch-diff",
            "ws-switch-editor",
            "ws-shell-toggle",
        ] {
            assert!(h.cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        // The component draws no workspace name, role, issue id, branch or
        // machine.
        for selector in [
            "workspace-name",
            "workspace-role",
            "workspace-task-id",
            "workspace-branch",
            "workspace-machine",
        ] {
            assert!(h.cx.debug_bounds(selector).is_none(), "{selector} is not drawn");
        }
        assert!(h.cx.debug_bounds("ws-close-content").is_none(), "the agent has no ×");
        // The back button is the component's 28pt square, the views its 34pt
        // nav items.
        let back = h.cx.debug_bounds("ws-back").unwrap();
        assert_eq!((back.size.width, back.size.height), (px(28.0), px(28.0)));
        assert_eq!(h.cx.debug_bounds("ws-switch-diff").unwrap().size.height, px(34.0));
        assert!(h.cx.debug_bounds("workspace-header").is_none(), "no header row");

        // The terminal starts at the top of the main area, under the
        // Workspace frame's 1px rule (Figma 2167:653).
        let main = h.cx.debug_bounds("workspace-view").unwrap();
        let panes = h.cx.debug_bounds("ws-panes").unwrap();
        assert_eq!(panes.origin.y, main.origin.y + px(1.0), "only the rule above the terminal");
        assert_eq!(panes.size.height, main.size.height - px(1.0));

        // The panel sits in its column, the panes beside it.
        let panel = h.cx.debug_bounds("workspace-panel").unwrap();
        assert!(panel.right() <= panes.origin.x);

        // The full title wraps rather than truncating, under the status.
        let state = h.cx.debug_bounds("task-panel-status").unwrap();
        let title = h.cx.debug_bounds("workspace-title").unwrap();
        assert!(title.origin.y > state.origin.y);
        let row = h.view.read_with(&h.cx, |v, _| v.panel.rows().to_vec());
        assert_eq!(
            row,
            [
                WsRow::Back,
                WsRow::More,
                WsRow::Folder,
                WsRow::Switch(ContentView::Agent),
                WsRow::Switch(ContentView::Diff),
                WsRow::Switch(ContentView::Editor),
                WsRow::Shell,
            ]
        );
    }

    #[gpui::test]
    fn the_description_clamps_to_five_lines_and_more_opens_the_detail(cx: &mut TestAppContext) {
        let mut h = open(cx);
        *h.ops.details.lock().unwrap() = Some(WorkspaceDetailsInfo {
            body: "Build a native desktop client for Shelbi. ".repeat(20),
            ..details_for_t1()
        });
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.draw();
        let clamped = h.cx.debug_bounds("workspace-description").unwrap();
        assert_eq!(clamped.size.height, px(18.0 * 5.0), "five 18pt lines");
        // More asks the window for the task's detail; the preview stays
        // clamped and the link stays More.
        h.click("ws-more");
        assert_eq!(h.events(), vec![WorkspaceEvent::OpenTask("t-1".into())]);
        h.draw();
        assert_eq!(h.cx.debug_bounds("workspace-description").unwrap().size.height, px(90.0));
        h.click("ws-more");
        assert_eq!(h.events().len(), 2, "More opens it every time, it doesn't toggle");
    }

    #[gpui::test]
    fn the_board_s_text_refreshes_the_summary(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.update(|v, cx| v.refresh_task_text("t-1", "Warm-start cache", "Edited in the detail.", cx));
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.title.as_deref(), Some("Warm-start cache"));
        assert_eq!(
            h.view.read_with(&h.cx, |v, _| v.details.as_ref().map(|d| d.body.clone())),
            Some("Edited in the detail.".into())
        );
        // Another task's text leaves it alone.
        h.update(|v, cx| v.refresh_task_text("t-2", "Other", "Other body", cx));
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.title.as_deref(), Some("Warm-start cache"));
    }

    #[gpui::test]
    fn view_shortcuts_pick_the_numbered_views(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        let shortcut = |h: &mut Harness, n: u8| {
            h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.view_shortcut(n, window, cx)));
            h.cx.run_until_parked();
        };
        shortcut(&mut h, 2);
        assert_eq!(h.active(), ContentView::Diff, "⌘2 is View Diff");
        shortcut(&mut h, 3);
        assert_eq!(h.active(), ContentView::Editor, "⌘3 is Edit in Vim");
        shortcut(&mut h, 4);
        assert!(h.view.read_with(&h.cx, |v, _| v.shell_open()), "⌘4 toggles the shell");
        shortcut(&mut h, 1);
        assert_eq!(h.active(), ContentView::Agent, "⌘1 is the agent");
        shortcut(&mut h, 5);
        assert_eq!(h.active(), ContentView::Agent, "no fifth view");
        assert!(h.events().is_empty());
    }

    #[gpui::test]
    fn a_long_title_wraps_in_the_panel(cx: &mut TestAppContext) {
        let mut h = open(cx);
        *h.ops.details.lock().unwrap() = Some(WorkspaceDetailsInfo {
            task: Some((
                "t-1".into(),
                "Desktop: a terminal pane sometimes can't scroll to the live bottom after a resize".into(),
            )),
            ..details_for_t1()
        });
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.draw();
        let title = h.cx.debug_bounds("workspace-title").unwrap();
        assert!(
            title.size.height > px(18.0 * 1.5),
            "the title takes more than one 18pt line: {title:?}"
        );
        assert!(title.size.width <= px(280.0));
    }

    #[gpui::test]
    fn the_header_follows_the_sidebar_row(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.update(|v, cx| {
            v.update_row(Some(row("alpha", Some("t-1"), WorkspaceBadge::AwaitingPermission, false)), cx)
        });
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.state.label(), "Needs permission");
        assert_eq!(header.task_line().as_deref(), Some("t-1  Cold-start cache"));
        assert_eq!(h.sessions.attached().len(), 1, "a live agent is left alone");
    }

    #[gpui::test]
    fn an_idle_workspace_waits_for_a_session_and_launches_nothing(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.sessions.idle.lock().unwrap().push("demo/ws/alpha".into());
        *h.ops.details.lock().unwrap() = Some(WorkspaceDetailsInfo {
            branch: Some("main".into()),
            worktree: "/w/alpha".into(),
            ..WorkspaceDetailsInfo::default()
        });
        h.update(|v, cx| v.show("alpha", Some(row("alpha", None, WorkspaceBadge::Idle, false)), cx));
        assert_eq!(h.content_phase(), SessionPhase::Idle);
        assert!(h.ops().is_empty(), "selecting an idle workspace launches nothing");
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.task_line(), None);
        assert_eq!(header.location_line(), "hub · main");
        h.draw();
        // The reduced panel: back and where it lives.
        for selector in ["ws-back", "workspace-worktree", "ws-folder"] {
            assert!(h.cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        for selector in [
            "task-panel-status",
            "workspace-title",
            "ws-more",
            "ws-switch-agent",
            "ws-switch-diff",
            "ws-shell-toggle",
        ] {
            assert!(h.cx.debug_bounds(selector).is_none(), "{selector} is not drawn while idle");
        }
        let rows = h.view.read_with(&h.cx, |v, _| v.panel.rows().to_vec());
        assert_eq!(rows, [WsRow::Back, WsRow::Folder]);
        // The main area shows the idle placeholder.
        assert!(h.cx.debug_bounds("ws-panes").is_some());

        // An unrelated refresh (same row) doesn't retry.
        h.update(|v, cx| v.update_row(Some(row("alpha", None, WorkspaceBadge::Idle, false)), cx));
        assert!(h.sessions.attached().is_empty());

        // A dispatch started the agent: the row changes and the view attaches.
        h.sessions.idle.lock().unwrap().clear();
        h.update(|v, cx| v.update_row(Some(working()), cx));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        assert_eq!(h.sessions.attached(), vec!["demo/ws/alpha"]);
        // And the panel grows the task, the switch and the shell.
        h.draw();
        assert!(h.cx.debug_bounds("ws-switch-agent").is_some());
        assert!(h.cx.debug_bounds("ws-shell-toggle").is_some());
    }

    #[gpui::test]
    fn an_idle_workspace_shows_its_page_without_the_panel(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.sessions.idle.lock().unwrap().push("demo/ws/alpha".into());
        *h.ops.details.lock().unwrap() = Some(WorkspaceDetailsInfo {
            branch: Some("main".into()),
            worktree: "/w/alpha".into(),
            ..WorkspaceDetailsInfo::default()
        });
        h.update(|v, cx| v.show("alpha", Some(row("alpha", None, WorkspaceBadge::Idle, false)), cx));
        assert!(!h.view.read_with(&h.cx, |v, _| v.has_panel()), "no task, no panel");
        h.draw();
        // The page (Figma 2174:851): the toolbar, a rule, the placeholder.
        let main = h.cx.debug_bounds("workspace-view").unwrap();
        let toolbar = h.cx.debug_bounds("workspace-toolbar").expect("the toolbar is drawn");
        let panes = h.cx.debug_bounds("ws-panes").unwrap();
        assert_eq!(toolbar.origin.y, main.origin.y);
        assert_eq!(toolbar.size.height, px(64.0), "8 + 28 + 4 + 16 + 8, as drawn");
        assert_eq!(panes.origin.y, toolbar.bottom() + px(1.0), "under the 1px rule");
        assert!(h.cx.debug_bounds("workspace-place").is_some());
        // Focus goes to the agent pane's view, as the chat's does.
        let target = h.view.read_with(&h.cx, |v, cx| v.focus_target(cx));
        let content = h.view.read_with(&h.cx, |v, cx| v.content.read(cx).focus_target(cx));
        assert_eq!(target, content);

        // The Shell button opens the user shell beside the placeholder.
        h.click("ws-shell-button");
        h.cx.run_until_parked();
        assert_eq!(h.ops(), vec![WorkspaceSessionOp::EnsureShell]);
        assert_eq!(h.shell_phase(), SessionPhase::Live);
        h.click("ws-shell-button");
        h.cx.run_until_parked();
        assert_eq!(h.ops(), vec![WorkspaceSessionOp::EnsureShell, WorkspaceSessionOp::CloseShell]);

        // A dispatch gives it a task: the panel, and no toolbar.
        h.sessions.idle.lock().unwrap().clear();
        h.update(|v, cx| v.update_row(Some(working()), cx));
        assert!(h.view.read_with(&h.cx, |v, _| v.has_panel()));
        h.draw();
        assert!(h.cx.debug_bounds("workspace-toolbar").is_none());
    }

    #[test]
    fn the_idle_toolbar_writes_home_as_a_tilde() {
        let home = std::env::var("HOME").unwrap();
        assert_eq!(home_relative(&format!("{home}/Workspaces/wt/charlie")), "~/Workspaces/wt/charlie");
        assert_eq!(home_relative("/srv/wt/charlie"), "/srv/wt/charlie");
        assert_eq!(home_relative(&format!("{home}x/wt")), format!("{home}x/wt"), "only the whole directory");
    }

    #[gpui::test]
    fn the_user_shell_opens_beside_the_agent_resizes_and_closes_with_the_view(
        cx: &mut TestAppContext,
    ) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.toggle_shell(window, cx)));
        h.cx.run_until_parked();
        assert_eq!(h.ops(), vec![WorkspaceSessionOp::EnsureShell]);
        assert_eq!(h.shell_phase(), SessionPhase::Live);
        assert_eq!(h.sessions.attached(), vec!["demo/ws/alpha", "demo/ws/alpha/shell"]);
        assert_eq!(h.content_phase(), SessionPhase::Live, "the agent stays attached");

        // Both panes are laid out side by side, split by the divider.
        h.draw();
        h.draw();
        let shell = h.cx.debug_bounds("ws-shell-pane").expect("the shell pane is drawn");
        let divider = h.cx.debug_bounds("ws-split-divider").expect("the divider is drawn");
        assert!(divider.origin.x < shell.origin.x);
        let before = h.view.read_with(&h.cx, |v, _| v.split.split.ratio());

        // Drag the divider left.
        let start = divider.center();
        let to = gpui::point(start.x - px(120.0), start.y);
        h.cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::none());
        h.cx.simulate_mouse_move(to, Some(MouseButton::Left), gpui::Modifiers::none());
        h.cx.simulate_mouse_up(to, MouseButton::Left, gpui::Modifiers::none());
        h.draw();
        let after = h.view.read_with(&h.cx, |v, _| v.split.split.ratio());
        assert!(after < before, "the drag moved the split ({before} -> {after})");
        assert!(!h.view.read_with(&h.cx, |v, _| v.split.dragging));
        let moved = h.cx.debug_bounds("ws-split-divider").unwrap();
        assert!(moved.origin.x < divider.origin.x, "the divider follows the drag");

        // Leaving the view closes the shell.
        h.update(|v, cx| v.hide(cx));
        assert_eq!(h.ops(), vec![WorkspaceSessionOp::EnsureShell, WorkspaceSessionOp::CloseShell]);
        assert_eq!(h.shell_phase(), SessionPhase::Empty);
        assert!(!h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(h.content_phase(), SessionPhase::Empty, "the agent detaches too");
    }

    #[gpui::test]
    fn exiting_the_shell_closes_the_split(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.toggle_shell(window, cx)));
        h.cx.run_until_parked();
        h.sessions
            .stream("demo/ws/alpha/shell")
            .unbounded_send(SessionEvent::Exited(shelbi_proto::Exited {
                code: Some(0),
                signal: None,
                reason: None,
            }))
            .unwrap();
        h.cx.run_until_parked();
        assert!(!h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(h.shell_phase(), SessionPhase::Empty);
        assert_eq!(h.sessions.attached().len(), 2, "the shell isn't restarted");

        // Toggling it again starts a fresh one.
        h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.toggle_shell(window, cx)));
        h.cx.run_until_parked();
        assert_eq!(h.shell_phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn a_shell_the_daemon_refuses_reports_why(cx: &mut TestAppContext) {
        let mut h = open(cx);
        *h.ops.fail_ops.lock().unwrap() = true;
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.toggle_shell(window, cx)));
        h.cx.run_until_parked();
        assert!(!h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(
            h.view.read_with(&h.cx, |v, _| v.status_line.clone()).as_deref(),
            Some("couldn't open a shell: the daemon said no")
        );
    }

    #[gpui::test]
    fn the_diff_switch_ensures_then_attaches_and_close_returns_to_the_agent(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.update(|v, cx| v.select_view(ContentView::Diff, cx));
        assert_eq!(h.ops(), vec![WorkspaceSessionOp::Ensure { role: ReviewRole::Diff }]);
        assert_eq!(h.sessions.attached(), vec!["demo/ws/alpha", "demo/ws/alpha/diff"]);
        assert_eq!(h.content_phase(), SessionPhase::Live);

        h.draw();
        assert!(h.cx.debug_bounds("ws-close-content").is_some(), "the active diff has a ×");
        h.click("ws-close-content");
        assert_eq!(h.active(), ContentView::Agent);
        assert!(h.events().is_empty(), "the × stays in the workspace");
        assert_eq!(h.ops().last(), Some(&WorkspaceSessionOp::Close));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/alpha"));
    }

    #[gpui::test]
    fn a_view_the_daemon_refuses_falls_back_to_the_agent(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.update(|v, cx| v.select_view(ContentView::Editor, cx));
        assert_eq!(h.active(), ContentView::Agent);
        assert_eq!(
            h.view.read_with(&h.cx, |v, _| v.status_line.clone()).as_deref(),
            Some("couldn't open view: the daemon said no")
        );
        assert_eq!(h.content_phase(), SessionPhase::Live);
    }

    #[gpui::test]
    fn switching_workspaces_closes_the_shell_and_retargets(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.cx.update(|window, cx| h.view.update(cx, |v, cx| v.toggle_shell(window, cx)));
        h.cx.run_until_parked();
        h.update(|v, cx| v.show("bravo", Some(row("bravo", None, WorkspaceBadge::Idle, false)), cx));
        assert_eq!(h.ops().last(), Some(&WorkspaceSessionOp::CloseShell));
        assert!(!h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/bravo"));
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.name, "bravo");
        assert_eq!(header.task_line(), None, "alpha's task never labels bravo");
    }

    #[gpui::test]
    fn a_remote_workspace_names_its_machine_and_recovers_from_an_ssh_drop(cx: &mut TestAppContext) {
        let mut h = open(cx);
        let remote = row("alpha", Some("t-1"), WorkspaceBadge::Working, true);
        h.update(|v, cx| v.show("alpha", Some(remote), cx));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.location_line(), "gpu (remote) · jlong/t-1");
        h.draw();
        assert!(h.cx.debug_bounds("ws-switch-diff").is_none(), "no diff/editor off the hub");
        assert!(h.cx.debug_bounds("ws-switch-editor").is_none());
        assert!(h.cx.debug_bounds("ws-folder").is_none(), "nothing to reveal off the hub");
        assert!(h.cx.debug_bounds("workspace-worktree").is_some(), "the path is still shown");
        assert!(h.cx.debug_bounds("ws-switch-agent").is_some());
        assert!(h.cx.debug_bounds("ws-shell-toggle").is_some());

        // The SSH connection under the relay dies: the stream ends, the
        // machine is unreachable for a while, then answers again.
        h.sessions.unreachable.lock().unwrap().push("demo/ws/alpha".into());
        h.sessions.drop_streams("demo/ws/alpha");
        h.cx.run_until_parked();
        assert_eq!(h.content_phase(), SessionPhase::Reconnecting(2));
        h.sessions.unreachable.lock().unwrap().clear();
        h.advance(Duration::from_secs(1));
        assert_eq!(h.content_phase(), SessionPhase::Live, "recovered on its own");
        assert_eq!(h.sessions.attached(), vec!["demo/ws/alpha"]);
    }

    #[gpui::test]
    fn panel_keys_navigate_switch_and_go_back(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.focus_panel();
        assert_eq!(
            h.view.read_with(&h.cx, |v, _| v.panel.selected_row()),
            Some(WsRow::Switch(ContentView::Agent)),
            "the agent is selected first"
        );
        h.key("down");
        h.key("enter");
        assert_eq!(h.active(), ContentView::Diff);
        assert_eq!(h.ops(), vec![WorkspaceSessionOp::Ensure { role: ReviewRole::Diff }]);
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/alpha/diff"));

        // Down twice more to the shell, and toggle it.
        h.key("j");
        h.key("j");
        h.key("space");
        assert!(h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(h.ops().last(), Some(&WorkspaceSessionOp::EnsureShell));

        // `m` opens the task's detail.
        h.key("m");
        assert_eq!(h.events(), vec![WorkspaceEvent::OpenTask("t-1".into())]);

        // So does Enter on the More row, up past the worktree.
        h.key("k");
        h.key("k");
        h.key("k");
        h.key("k");
        h.key("k");
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.panel.selected_row()), Some(WsRow::More));
        h.key("enter");
        assert_eq!(
            h.events(),
            vec![WorkspaceEvent::OpenTask("t-1".into()), WorkspaceEvent::OpenTask("t-1".into())]
        );

        // Esc goes back.
        h.key("escape");
        assert_eq!(h.events().last(), Some(&WorkspaceEvent::Back));
    }

    #[gpui::test]
    fn tab_moves_to_the_agent_and_the_sidebar_chord_comes_back(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.focus_panel();
        h.key("tab");
        let in_terminal = h.cx.update(|window, cx| {
            let v = h.view.read(cx);
            v.content.read(cx).contains_focus(window, cx)
        });
        assert!(in_terminal, "Tab hands the keys to the agent");
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.focus), WsFocus::Content);
        h.focus_panel();
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.focus), WsFocus::Panel);
    }

    #[gpui::test]
    fn q_closes_the_content_and_goes_back(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        h.update(|v, cx| v.select_view(ContentView::Editor, cx));
        h.focus_panel();
        h.key("q");
        assert_eq!(h.ops().last(), Some(&WorkspaceSessionOp::Close));
        assert_eq!(h.active(), ContentView::Agent);
        assert_eq!(h.events(), vec![WorkspaceEvent::Back]);
    }

    #[gpui::test]
    fn panel_rows_click_like_the_header_buttons_did(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));

        // The worktree reveals in Finder.
        h.click("ws-folder");
        assert_eq!(*h.ops.revealed.lock().unwrap(), vec!["/w/alpha".to_string()]);

        // More opens the task's detail (the window draws it).
        h.click("ws-more");
        assert_eq!(h.events(), vec![WorkspaceEvent::OpenTask("t-1".into())]);

        // The editor switch, then the agent.
        h.click("ws-switch-editor");
        assert_eq!(h.active(), ContentView::Editor);
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/alpha/editor"));
        h.click("ws-switch-agent");
        assert_eq!(h.active(), ContentView::Agent);
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/alpha"));

        // The shell toggles on and off.
        h.click("ws-shell-toggle");
        assert!(h.view.read_with(&h.cx, |v, _| v.shell_open()));
        h.draw();
        assert!(h.cx.debug_bounds("ws-shell-pane").is_some());
        h.click("ws-shell-toggle");
        assert!(!h.view.read_with(&h.cx, |v, _| v.shell_open()));
        assert_eq!(h.ops().last(), Some(&WorkspaceSessionOp::CloseShell));

        // Back.
        h.click("ws-back");
        assert_eq!(h.events(), vec![WorkspaceEvent::OpenTask("t-1".into()), WorkspaceEvent::Back]);
    }

    #[gpui::test]
    fn the_editor_row_names_the_resolved_editor(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        let (agent, editor) = h.view.read_with(&h.cx, |v, _| {
            let header = v.header().unwrap();
            let editor = v.current_details().map(|d| d.editor_name.clone()).unwrap();
            (header.agent.unwrap(), editor)
        });
        assert_eq!(ContentView::Agent.label(&agent, &editor), "Developer");
        assert_eq!(ContentView::Editor.label(&agent, &editor), "Edit in Vim");
    }
}
