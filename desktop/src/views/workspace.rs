//! The Workspace view (`dt-workspace-view`): a dev workspace's agent session
//! (`<project>/ws/<name>`) in the terminal element, with the user's own shell
//! beside it on demand, and the workspace panel in the sidebar's column
//! (`dt-workspace-sidebar`).
//!
//! It follows the TUI's workspace interface (`crates/shelbi-tui/src/shell/workspace.rs`,
//! `workspace_panel.rs`) and its idle-workspace open (`rt-tui-idle-workspace-open`):
//!
//! - **The panel** takes the navigation sidebar's column while the workspace
//!   is open, as the review panel does: the view renders as the panel, at
//!   whatever width the window's column gives it, and the window puts
//!   [`WorkspaceView::render_main`] (the terminals and the description
//!   popover) in the main area beside it. It shows the back button and the
//!   live state, the workspace's name and agent role; the task's full title,
//!   id and a description preview with More (the full description in a
//!   popover); the branch, the worktree folder (click to reveal it in Finder)
//!   and the machine; then the view switch and the Shell toggle as rows. Its
//!   rows, selection and keys are the shared [`WorkspacePanelState`] (the
//!   TUI's panel navigates by the same functions). An idle workspace gets a
//!   reduced panel: name, state and where it lives, no task, switch or shell.
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

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, prelude::*, px, relative, AnyElement, AnyWindowHandle, Bounds, Context,
    CursorStyle, ElementId, Entity, EventEmitter, FocusHandle, Focusable, FontWeight,
    MouseButton, MouseMoveEvent, Pixels, SharedString, Subscription, Window,
};
use gpui_kit::component::text::TextView;
use shelbi_app::session::{SessionRef, WorkspaceContentRelauncher};
use shelbi_app::view::{WorkspaceBadge, WorkspaceDetailsInfo, WorkspaceHeader, WorkspaceRow};
use shelbi_app::workspace_panel::{WorkspacePanelState, WsEffect, WsPanelShape, WsRow};
use shelbi_app::WorkspaceSessionOp;

use super::session_view::{OnDeath, SessionDeps, SessionView, SessionViewEvent};
use super::sidebar::TRAFFIC_LIGHT_INSET;
use super::workspace_layout::{Split, DIVIDER_W};
use crate::assets::{heroicon, icons, Icon, IconSize};
use crate::keys::chord_from_keystroke;
use crate::refresh::SessionSink;
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
}

/// The Workspace view for one project window. It shows one workspace at a
/// time; selecting another retargets it.
pub struct WorkspaceView {
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
    split: Split,
    /// Where the panes were laid out last frame, for the divider drag.
    split_bounds: Option<Bounds<Pixels>>,
    dragging: bool,
    /// A failure or note at the foot of the panel (the TUI panel's status
    /// line).
    status_line: Option<String>,
    description_open: bool,
    /// Bumped whenever the shown workspace changes; a background result for
    /// an older one is dropped.
    generation: u64,
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
            split: Split::default(),
            split_bounds: None,
            dragging: false,
            status_line: None,
            description_open: false,
            generation: 0,
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
            self.description_open = false;
            self.row = None;
            self.panel = WorkspacePanelState::new(&WsPanelShape::for_workspace(false, false, true, true));
            self.focus = WsFocus::Panel;
        }
        self.panel.set_active(ContentView::Agent);
        self.shown = true;
        self.set_row(row, cx);
        let target = self.agent_target();
        self.content.update(cx, |c, cx| c.show_target(target, cx));
        cx.notify();
    }

    /// Hide the view: detach the panes and close the user shell.
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.description_open = false;
        self.dragging = false;
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

    /// Where keyboard focus goes when the view takes it: the agent pane once
    /// the user moved there, else the panel (as the review's).
    pub fn focus_target(&self, cx: &gpui::App) -> FocusHandle {
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
    #[cfg(feature = "visual-check")]
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
            WsEffect::ShowDescription => {
                self.description_open = true;
                cx.notify();
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

    /// A key while the panel has focus.
    fn panel_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.description_open && event.keystroke.key == "escape" {
            self.description_open = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
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
                    this.content.update(cx, |c, cx| c.show_target(target, cx));
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
        self.content.update(cx, |c, cx| c.show_target(target, cx));
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

    #[cfg(any(test, feature = "visual-check"))]
    pub fn shell_open(&self) -> bool {
        self.shell_open
    }

    /// Whether the description is open (the offscreen visual check).
    #[cfg(feature = "visual-check")]
    pub fn description_open(&self) -> bool {
        self.description_open
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

    fn toggle_description(&mut self, cx: &mut Context<Self>) {
        self.description_open = !self.description_open;
        cx.notify();
    }

    /// Open or close the description (the offscreen visual check).
    #[cfg(feature = "visual-check")]
    pub fn toggle_description_for_check(&mut self, cx: &mut Context<Self>) {
        self.toggle_description(cx);
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

    fn render_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let root = div()
            .id("workspace-panel")
            .debug_selector(|| "workspace-panel".into())
            .track_focus(&self.focus_handle)
            .key_context("WorkspacePanel")
            .on_key_down(cx.listener(Self::panel_key))
            .flex()
            .flex_col()
            .size_full()
            .gap_4()
            .p_3()
            // The window's leading column: clear of the traffic lights, as
            // the sidebar's header is.
            .pt(px(TRAFFIC_LIGHT_INSET))
            .overflow_y_scroll()
            .bg(theme.background)
            .text_sm()
            .text_color(theme.gray);
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
        let row_base = |id: &'static str, row: WsRow| {
            div()
                .id(id)
                .debug_selector(move || id.into())
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .rounded_md()
                .cursor_pointer()
                .when(selected(row), |d| d.bg(theme.selection))
                .when(!selected(row), |d| d.hover(|s| s.bg(theme.search)))
                .on_click(cx.listener(move |this, _, window, cx| this.click(row, window, cx)))
        };
        // A non-interactive line of the "where" block.
        let fact = |selector: &'static str, icon: Icon, text: String| {
            div()
                .debug_selector(move || selector.into())
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .px_2()
                .h(px(26.0))
                .min_w_0()
                .child(heroicon(icon, IconSize::Mini).text_color(theme.muted))
                .child(div().min_w_0().truncate().child(text))
        };

        // Status: back, the live state, the name and the agent's role.
        let dec = header.state_decoration();
        let status = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .child(
                row_base("ws-back", WsRow::Back)
                    .size(IconSize::Standard.px() + px(8.0))
                    .justify_center()
                    .px_0()
                    .child(heroicon(icons::ARROW_LEFT, IconSize::Standard).text_color(theme.gray)),
            )
            .child(
                div()
                    .debug_selector(|| "workspace-state".into())
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.decoration(dec.color))
                    .child(format!("{} {}", dec.glyph, header.state.label())),
            );
        let identity = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_2()
            .min_w_0()
            .child(
                div()
                    .debug_selector(|| "workspace-name".into())
                    .min_w_0()
                    .truncate()
                    .font_weight(FontWeight::BOLD)
                    .text_color(theme.white)
                    .child(header.name.clone()),
            )
            .when_some(header.agent.clone(), |d, role| {
                d.child(
                    div()
                        .debug_selector(|| "workspace-role".into())
                        .flex_none()
                        .text_color(theme.muted)
                        .child(format!("· {role}")),
                )
            });

        // Task: the full title (wrapped), its id, the description preview.
        let task = header.task_id.clone().map(|id| {
            let preview = details
                .map(|d| shelbi_app::review::description_text(&d.body))
                .unwrap_or_default();
            let more = panel.rows().contains(&WsRow::More).then(|| {
                row_base("ws-more", WsRow::More).child(div().text_color(theme.cyan).child("More"))
            });
            div()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .px_2()
                        .child(
                            div()
                                .debug_selector(|| "workspace-title".into())
                                .font_weight(FontWeight::BOLD)
                                .text_color(theme.white)
                                .child(header.title.clone().filter(|t| !t.is_empty()).unwrap_or_else(|| id.clone())),
                        )
                        .child(
                            div()
                                .debug_selector(|| "workspace-task-id".into())
                                .text_xs()
                                .text_color(theme.muted)
                                .child(id),
                        )
                        .when(!preview.is_empty(), |d| {
                            d.child(
                                div()
                                    .debug_selector(|| "workspace-description".into())
                                    .text_color(theme.gray)
                                    .text_ellipsis()
                                    .line_clamp(3)
                                    .child(preview),
                            )
                        }),
                )
                .children(more)
        });

        // Where: the branch, the worktree (click to reveal it when local),
        // the machine.
        let worktree = self.details.as_ref().map(|d| d.worktree.clone()).filter(|w| !w.is_empty());
        let folder = worktree.map(|path| {
            let text = div()
                .min_w_0()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis_start()
                .child(path);
            if panel.rows().contains(&WsRow::Folder) {
                row_base("ws-folder", WsRow::Folder)
                    .h(px(26.0))
                    .child(heroicon(icons::FOLDER_OPEN, IconSize::Mini).text_color(theme.muted))
                    .child(text)
                    .into_any_element()
            } else {
                div()
                    .debug_selector(|| "workspace-worktree".into())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_2()
                    .h(px(26.0))
                    .child(heroicon(icons::FOLDER_OPEN, IconSize::Mini).text_color(theme.muted))
                    .child(text)
                    .into_any_element()
            }
        });
        let machine = if header.is_remote {
            fact("workspace-machine", icons::GLOBE_ALT, format!("{} (remote)", header.machine))
        } else {
            fact("workspace-machine", icons::SERVER_STACK, header.machine.clone())
        };
        let place = div()
            .flex()
            .flex_col()
            .gap_0p5()
            .children(
                header
                    .branch
                    .clone()
                    .filter(|b| !b.is_empty())
                    .map(|b| fact("workspace-branch", icons::SHARE, b)),
            )
            .children(folder)
            .child(machine);

        // The view switch, then the shell toggle.
        let active = panel.active();
        let switches = panel.rows().iter().filter_map(|row| match row {
            WsRow::Switch(item) => Some(*item),
            _ => None,
        });
        let switches = switches.map(|item| {
            let (id, icon) = match item {
                ContentView::Agent => ("ws-switch-agent", icons::CHAT_BUBBLE_LEFT_RIGHT),
                ContentView::Diff => ("ws-switch-diff", icons::ARROWS_RIGHT_LEFT),
                ContentView::Editor => ("ws-switch-editor", icons::PENCIL_SQUARE),
            };
            let on = active == item;
            let color = if on { theme.white } else { theme.gray };
            row_base(id, WsRow::Switch(item))
                .h(px(30.0))
                .text_color(color)
                .when(on, |d| d.font_weight(FontWeight::BOLD).bg(theme.selection))
                .child(heroicon(icon, IconSize::Standard).text_color(color))
                .child(div().flex_1().min_w_0().truncate().child(item.label(&agent_name, &editor_name)))
                .when(on && item != ContentView::Agent, |d| {
                    d.child(
                        div()
                            .id("ws-close-content")
                            .debug_selector(|| "ws-close-content".into())
                            .flex_none()
                            .rounded_md()
                            .hover(|s| s.bg(theme.search))
                            .child(heroicon(icons::X_MARK, IconSize::Mini).text_color(theme.muted))
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.close_content(cx);
                            })),
                    )
                })
        });
        let shell = panel.rows().contains(&WsRow::Shell).then(|| {
            let (state, color) = match (self.shell_open, self.shell_starting) {
                (true, true) => ("starting", theme.muted),
                (true, false) => ("on", theme.green),
                (false, _) => ("off", theme.muted),
            };
            row_base("ws-shell-toggle", WsRow::Shell)
                .h(px(30.0))
                .child(heroicon(icons::COMMAND_LINE, IconSize::Standard).text_color(theme.gray))
                .child(div().flex_1().child("Shell"))
                .child(
                    div()
                        .debug_selector(|| "ws-shell-state".into())
                        .flex_none()
                        .text_xs()
                        .text_color(color)
                        .child(state),
                )
        });

        root.child(div().flex().flex_col().gap_1().child(status).child(identity))
            .children(task)
            .child(place)
            .child(div().flex().flex_col().gap_0p5().children(switches))
            .children(shell)
            .when_some(self.status_line.clone(), |d, line| {
                d.child(
                    div()
                        .debug_selector(|| "workspace-status".into())
                        .px_2()
                        .text_color(theme.yellow)
                        .child(line),
                )
            })
    }

    fn render_panes(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let this = cx.entity().downgrade();
        let widths = self
            .split_bounds
            .map(|b| self.split.widths(b.size.width.as_f32()));
        let content = div()
            .flex()
            .min_w_0()
            .min_h_0()
            .h_full()
            .map(|d| match (self.shell_open, widths) {
                (false, _) => d.flex_1(),
                (true, Some((agent, _))) => d.flex_none().w(px(agent)),
                (true, None) => d.flex_none().w(relative(self.split.ratio())),
            })
            .child(self.content.clone());
        let divider_color = if self.dragging {
            theme.divider_active
        } else {
            theme.divider
        };
        let shell_side = self.shell_open.then(|| {
            let divider = div()
                .id("ws-split-divider")
                .debug_selector(|| "ws-split-divider".into())
                .w(px(DIVIDER_W))
                .h_full()
                .flex_none()
                .flex()
                .justify_center()
                .cursor(CursorStyle::ResizeLeftRight)
                .child(div().w(px(1.0)).h_full().bg(divider_color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.dragging = true;
                        cx.stop_propagation();
                        cx.notify();
                    }),
                );
            let shell = div()
                .debug_selector(|| "ws-shell-pane".into())
                .flex()
                .min_w_0()
                .min_h_0()
                .h_full()
                .flex_1()
                .map(|d| {
                    if self.shell_starting {
                        d.items_center()
                            .justify_center()
                            .text_color(theme.muted)
                            .child("Starting a shell…")
                    } else {
                        d.child(self.shell.clone())
                    }
                });
            [divider.into_any_element(), shell.into_any_element()]
        });
        div()
            .id("ws-panes")
            .debug_selector(|| "ws-panes".into())
            .relative()
            .flex()
            .flex_row()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if let (true, Some(bounds)) = (this.dragging, this.split_bounds) {
                    let x = (event.position.x - bounds.origin.x).as_f32();
                    this.split.drag_to(x, bounds.size.width.as_f32());
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.dragging {
                        this.dragging = false;
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.dragging {
                        this.dragging = false;
                        cx.notify();
                    }
                }),
            )
            .child(content)
            .children(shell_side.into_iter().flatten())
            // Measures the panes' row for the divider math; repaints once
            // when the width first becomes known or changes.
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let _ = this.update(cx, |this, cx| {
                            if this.split_bounds != Some(bounds) {
                                this.split_bounds = Some(bounds);
                                cx.notify();
                            }
                        });
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
    }

    fn render_description(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if !self.description_open {
            return None;
        }
        let details = self.details.as_ref()?;
        let (id, title) = details.task.clone().unwrap_or_default();
        Some(
            div()
                .id("ws-description-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(theme.background.opacity(0.6))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_description(cx)))
                .child(
                    div()
                        .id("ws-description")
                        .debug_selector(|| "ws-description".into())
                        .flex()
                        .flex_col()
                        .w(relative(0.8))
                        .h(relative(0.85))
                        .overflow_hidden()
                        .rounded_lg()
                        .border_1()
                        .border_color(theme.divider)
                        .bg(theme.canvas)
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .justify_between()
                                .px_5()
                                .py_3()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .min_w_0()
                                        .child(div().text_color(theme.muted).child(id.clone()))
                                        .child(
                                            div()
                                                .truncate()
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(theme.white)
                                                .child(title),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("ws-description-close")
                                        .cursor_pointer()
                                        .child(heroicon(icons::X_MARK, IconSize::Standard).text_color(theme.muted))
                                        .on_click(cx.listener(|this, _, _, cx| this.toggle_description(cx))),
                                ),
                        )
                        .child(
                            div()
                                .id("ws-description-body")
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .px_5()
                                .pb_4()
                                .child(
                                    TextView::markdown(
                                        ElementId::Name(SharedString::from(format!("ws-body-{id}"))),
                                        details.body.clone(),
                                    )
                                    .w_full()
                                    .selectable(true),
                                ),
                        ),
                ),
        )
    }
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WorkspaceView {
    /// The main area beside the panel: the agent (or its diff or editor),
    /// the user shell when open, and the description popover over them.
    pub fn render_main(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        div()
            .id("workspace-view")
            .debug_selector(|| "workspace-view".into())
            .on_key_down(cx.listener(|this, ev: &gpui::KeyDownEvent, _, cx| {
                if this.description_open && ev.keystroke.key == "escape" {
                    this.toggle_description(cx);
                    cx.stop_propagation();
                }
            }))
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .size_full()
            .bg(theme.canvas)
            .child(self.render_panes(&theme, cx))
            .children(self.render_description(&theme, cx))
            .into_any_element()
    }
}

/// The view itself renders as the panel; the window sizes and places it.
impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        self.render_panel(&theme, cx)
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
        fn attached(&self) -> Vec<String> {
            self.attaches.lock().unwrap().iter().map(|(n, _)| n.clone()).collect()
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
        for selector in [
            "workspace-panel",
            "ws-back",
            "workspace-state",
            "workspace-name",
            "workspace-role",
            "workspace-title",
            "workspace-task-id",
            "workspace-description",
            "ws-more",
            "workspace-branch",
            "ws-folder",
            "workspace-machine",
            "ws-switch-agent",
            "ws-switch-diff",
            "ws-switch-editor",
            "ws-shell-toggle",
        ] {
            assert!(h.cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        assert!(h.cx.debug_bounds("ws-close-content").is_none(), "the agent has no ×");
        assert!(h.cx.debug_bounds("workspace-header").is_none(), "no header row");

        // The terminal starts at the top of the main area.
        let main = h.cx.debug_bounds("workspace-view").unwrap();
        let panes = h.cx.debug_bounds("ws-panes").unwrap();
        assert_eq!(panes.origin.y, main.origin.y, "nothing above the terminal");
        assert_eq!(panes.size.height, main.size.height);

        // The panel sits in its column, the panes beside it.
        let panel = h.cx.debug_bounds("workspace-panel").unwrap();
        assert!(panel.right() <= panes.origin.x);

        // The full title wraps rather than truncating, under the status.
        let state = h.cx.debug_bounds("workspace-state").unwrap();
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
        let name = h.cx.debug_bounds("workspace-name").unwrap();
        let title = h.cx.debug_bounds("workspace-title").unwrap();
        assert!(
            title.size.height > name.size.height * 1.5,
            "the title takes more than one line: {title:?} vs {name:?}"
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
        // The reduced panel: name, state and where it lives.
        for selector in ["ws-back", "workspace-state", "workspace-name", "ws-folder", "workspace-branch"] {
            assert!(h.cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
        for selector in [
            "workspace-title",
            "workspace-task-id",
            "ws-more",
            "workspace-role",
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
        let before = h.view.read_with(&h.cx, |v, _| v.split.ratio());

        // Drag the divider left.
        let start = divider.center();
        let to = gpui::point(start.x - px(120.0), start.y);
        h.cx.simulate_mouse_down(start, MouseButton::Left, gpui::Modifiers::none());
        h.cx.simulate_mouse_move(to, Some(MouseButton::Left), gpui::Modifiers::none());
        h.cx.simulate_mouse_up(to, MouseButton::Left, gpui::Modifiers::none());
        h.draw();
        let after = h.view.read_with(&h.cx, |v, _| v.split.ratio());
        assert!(after < before, "the drag moved the split ({before} -> {after})");
        assert!(!h.view.read_with(&h.cx, |v, _| v.dragging));
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
        assert!(h.cx.debug_bounds("workspace-machine").is_some());

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

        // `m` opens the description; Esc closes it before it leaves.
        h.key("m");
        assert!(h.view.read_with(&h.cx, |v, _| v.description_open));
        h.key("escape");
        assert!(!h.view.read_with(&h.cx, |v, _| v.description_open));
        assert!(h.events().is_empty());

        // Esc goes back.
        h.key("escape");
        assert_eq!(h.events(), vec![WorkspaceEvent::Back]);
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

        // More opens the description.
        h.click("ws-more");
        assert!(h.view.read_with(&h.cx, |v, _| v.description_open));
        h.draw();
        assert!(h.cx.debug_bounds("ws-description").is_some());
        h.update(|v, cx| v.toggle_description(cx));

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
        assert_eq!(h.events(), vec![WorkspaceEvent::Back]);
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
