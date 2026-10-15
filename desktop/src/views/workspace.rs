//! The Workspace view (`dt-workspace-view`): a dev workspace's agent session
//! (`<project>/ws/<name>`) in the terminal element, under a header showing the
//! task it is working (id and title), its branch, its machine and its live
//! state, with the user's own shell beside it on demand.
//!
//! It follows the TUI's workspace interface (`crates/shelbi-tui/src/shell/workspace.rs`,
//! `workspace_panel.rs`) and its idle-workspace open (`rt-tui-idle-workspace-open`):
//!
//! - **The agent.** Selecting a workspace attaches its agent session. An idle
//!   workspace (no session) shows the TUI's idle placeholder and re-attaches
//!   as soon as the sidebar row changes (a dispatch started a session), as the
//!   TUI retries the shown workspace on a change. Nothing is launched by
//!   selecting it: the daemon dispatches agents.
//! - **The panel's affordances**, in the header while the workspace has a
//!   task (the TUI shows its workspace panel only then): the task's full
//!   description (More), the worktree folder (Reveal), and the view switch:
//!   the agent, View Diff, Edit in <editor>. Diff and editor are the daemon's
//!   `<project>/ws/<name>/{diff,editor}` content sessions, ensured through
//!   `WorkspaceSessionOp::Ensure` and only attached here; Close (the TUI's `q`)
//!   ends them. Like the TUI's, they are local workspaces only.
//! - **The user shell.** A toggle opens a second terminal beside the agent,
//!   split with a draggable divider ([`super::workspace_layout`]), attached to
//!   the workspace's user shell (`<project>/ws/<name>/shell`, a login shell in
//!   the worktree on the workspace's machine) that the daemon starts
//!   (`WorkspaceSessionOp::EnsureShell`). Toggling it off, leaving the view,
//!   or exiting the shell closes it.
//! - **Remote workspaces.** A workspace on another machine attaches through
//!   that machine's relay (`shelbi_orchestrator::session_connect`), the header
//!   names the machine, and a dropped SSH connection reconnects
//!   ([`super::session_view`]).
//! - **Restart states** are the shared session view's (the Chat view's).
//! - **Drop to paste.** Files dropped on either terminal paste their paths.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    canvas, div, prelude::*, px, relative, AnyWindowHandle, Bounds, Context, CursorStyle,
    ElementId, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, MouseButton,
    MouseMoveEvent, Pixels, SharedString, Subscription, Window,
};
use gpui_kit::component::text::TextView;
use shelbi_app::session::{SessionRef, WorkspaceContentRelauncher};
use shelbi_app::view::{WorkspaceBadge, WorkspaceDetailsInfo, WorkspaceHeader, WorkspaceRow};
use shelbi_app::{ReviewRole, WorkspaceSessionOp};

use super::session_view::{OnDeath, SessionDeps, SessionView, SessionViewEvent};
use super::workspace_layout::{Split, DIVIDER_W};
use crate::assets::{heroicon, icons, Icon, IconSize};
use crate::refresh::SessionSink;
use crate::theme::Theme;

/// Which content the left pane shows. The TUI's `WsView`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentView {
    /// The workspace's agent session (the default).
    Agent,
    Diff,
    Editor,
}

impl ContentView {
    fn role(self) -> Option<ReviewRole> {
        match self {
            ContentView::Agent => None,
            ContentView::Diff => Some(ReviewRole::Diff),
            ContentView::Editor => Some(ReviewRole::Editor),
        }
    }
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
    /// The left pane: the agent, or its diff or editor.
    content: Entity<SessionView>,
    content_view: ContentView,
    /// The user shell beside it.
    shell: Entity<SessionView>,
    shell_open: bool,
    /// The daemon is starting the shell (the pane shows "Starting a shell…").
    shell_starting: bool,
    split: Split,
    /// Where the panes were laid out last frame, for the divider drag.
    split_bounds: Option<Bounds<Pixels>>,
    dragging: bool,
    /// A failure or note under the header (the TUI panel's status line).
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
            cx.subscribe(&content, |_, _, ev: &SessionViewEvent, cx| {
                if *ev == SessionViewEvent::Focused {
                    cx.emit(WorkspaceEvent::Focused);
                }
            }),
            cx.subscribe(&shell, |this, _, ev: &SessionViewEvent, cx| match ev {
                SessionViewEvent::Focused => cx.emit(WorkspaceEvent::Focused),
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
            content,
            content_view: ContentView::Agent,
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
            self.content_view = ContentView::Agent;
            self.row = None;
        }
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

    /// Where keyboard focus goes when the main area takes it: the agent pane.
    pub fn focus_target(&self, cx: &gpui::App) -> FocusHandle {
        if self.content.read(cx).is_shown() {
            self.content.read(cx).focus_target(cx)
        } else {
            self.focus_handle.clone()
        }
    }

    /// Whether keyboard focus is in one of the view's terminals.
    pub fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.content.read(cx).contains_focus(window, cx)
            || self.shell.read(cx).contains_focus(window, cx)
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

    /// Whether the workspace is working a task (the TUI opens its workspace
    /// panel, with the view switch, only then).
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
        if stale {
            self.request_details(cx);
        }
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
        self.content_view = view;
        cx.notify();
        self.session_op(WorkspaceSessionOp::Ensure { role }, cx, move |this, result, cx| {
            let workspace = this.workspace.clone().unwrap_or_default();
            match result {
                Ok(()) if this.content_view == view && this.shown => {
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
        self.content_view = ContentView::Agent;
        let target = self.agent_target();
        self.content.update(cx, |c, cx| c.show_target(target, cx));
        cx.notify();
    }

    /// End the diff and editor sessions and show the agent (the TUI panel's
    /// `q` Close).
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

    fn render_header(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let header = self.header();
        let has_task = self.has_task();
        let local = !self.is_remote();
        let editor_name = self
            .details
            .as_ref()
            .map(|d| d.editor_name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| "Editor".into());
        let has_body = self.details.as_ref().is_some_and(|d| !d.body.trim().is_empty());
        let content_view = self.content_view;

        let title_row = header.as_ref().map(|h| {
            let dec = h.state_decoration();
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .min_w_0()
                .child(
                    div()
                        .debug_selector(|| "workspace-state".into())
                        .flex_none()
                        .text_color(theme.decoration(dec.color))
                        .child(format!("{} {}", dec.glyph, h.state.label())),
                )
                .child(
                    div()
                        .debug_selector(|| "workspace-name".into())
                        .flex_none()
                        .font_weight(FontWeight::BOLD)
                        .text_color(theme.white)
                        .child(h.name.clone()),
                )
                .when_some(h.task_line(), |row, line| {
                    row.child(
                        div()
                            .debug_selector(|| "workspace-task".into())
                            .min_w_0()
                            .truncate()
                            .text_color(theme.gray)
                            .child(line),
                    )
                })
        });

        let location = header.as_ref().map(|h| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .min_w_0()
                .text_xs()
                .text_color(theme.muted)
                .when(h.is_remote, |d| {
                    d.child(heroicon(icons::GLOBE_ALT, IconSize::Micro).text_color(theme.muted))
                })
                .child(
                    div()
                        .debug_selector(|| "workspace-location".into())
                        .truncate()
                        .child(h.location_line()),
                )
                .when_some(h.agent.clone(), |d, agent| d.child(format!("· {agent}")))
        });

        let switch = |id: &'static str, label: String, view: ContentView| {
            let active = content_view == view;
            div()
                .id(id)
                .debug_selector(move || id.into())
                .px_2()
                .py_0p5()
                .rounded_md()
                .text_xs()
                .cursor_pointer()
                .when(active, |d| d.bg(theme.selection).text_color(theme.white))
                .when(!active, |d| d.text_color(theme.muted).hover(|s| s.bg(theme.search)))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| this.select_view(view, cx)))
        };
        let agent_label = header
            .as_ref()
            .and_then(|h| h.agent.clone())
            .unwrap_or_else(|| "Agent".into());
        let switcher = (has_task && local).then(|| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_0p5()
                .child(switch("ws-switch-agent", agent_label, ContentView::Agent))
                .child(switch("ws-switch-diff", "View Diff".into(), ContentView::Diff))
                .child(switch("ws-switch-editor", format!("Edit in {editor_name}"), ContentView::Editor))
                .when(content_view != ContentView::Agent, |d| {
                    d.child(
                        icon_button(theme, "ws-close-content", icons::X_MARK, "Close")
                            .on_click(cx.listener(|this, _, _, cx| this.close_content(cx))),
                    )
                })
        });

        let actions = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .when(has_task && has_body, |d| {
                d.child(
                    icon_button(theme, "ws-more", icons::DOCUMENT_TEXT, "More")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_description(cx))),
                )
            })
            .when(has_task && local, |d| {
                d.child(
                    icon_button(theme, "ws-reveal", icons::FOLDER_OPEN, "Reveal")
                        .on_click(cx.listener(|this, _, _, cx| this.reveal_folder(cx))),
                )
            })
            .child(
                icon_button(theme, "ws-shell-toggle", icons::COMMAND_LINE, "Shell")
                    .debug_selector(|| "ws-shell-toggle".into())
                    .when(self.shell_open, |d| d.bg(theme.selection).text_color(theme.white))
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_shell(window, cx))),
            );

        div()
            .debug_selector(|| "workspace-header".into())
            .flex()
            .flex_col()
            .flex_none()
            .gap_1()
            .px_4()
            .py_2()
            .border_b_1()
            .border_color(theme.divider)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .children(title_row)
                    .child(div().flex().flex_row().items_center().gap_3().flex_none().children(switcher).child(actions)),
            )
            .children(location)
            .when_some(self.status_line.clone(), |d, line| {
                d.child(
                    div()
                        .debug_selector(|| "workspace-status".into())
                        .text_xs()
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

fn icon_button(
    theme: &Theme,
    id: &'static str,
    icon: Icon,
    text: &str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap_1()
        .px_2()
        .py_0p5()
        .rounded_md()
        .text_xs()
        .text_color(theme.muted)
        .cursor_pointer()
        .hover(|s| s.bg(theme.search))
        .child(heroicon(icon, IconSize::Mini).text_color(theme.muted))
        .child(text.to_string())
}

impl Focusable for WorkspaceView {
    fn focus_handle(&self, _cx: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for WorkspaceView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        div()
            .id("workspace-view")
            .debug_selector(|| "workspace-view".into())
            .track_focus(&self.focus_handle)
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
            .child(self.render_header(&theme, cx))
            .child(self.render_panes(&theme, cx))
            .children(self.render_description(&theme, cx))
    }
}

#[cfg(test)]
mod tests {
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
    struct FakeSessions {
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
    struct FakeOps {
        details: Mutex<Option<WorkspaceDetailsInfo>>,
        ops: Mutex<Vec<WorkspaceSessionOp>>,
        fail_ops: Mutex<bool>,
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
        fn reveal(&self, _path: &str) -> Result<(), String> {
            Ok(())
        }
    }

    struct Harness {
        view: Entity<WorkspaceView>,
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

        fn advance(&mut self, by: Duration) {
            self.now.set(self.now.get() + by);
            self.cx.executor().advance_clock(by);
            self.cx.run_until_parked();
        }
    }

    fn row(name: &str, task: Option<&str>, badge: WorkspaceBadge, remote: bool) -> WorkspaceRow {
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
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                let handle = window.window_handle();
                cx.new(|cx| WorkspaceView::new("demo", handle, deps.take().unwrap(), None, cx))
            })
            .unwrap()
        });
        let mut vcx = VisualTestContext::from_window(window.into(), cx);
        let view = window.root(&mut vcx).unwrap();
        vcx.run_until_parked();
        Harness {
            view,
            cx: vcx,
            sessions,
            ops,
            now,
        }
    }

    #[gpui::test]
    fn a_working_workspace_shows_its_agent_live_under_the_header(cx: &mut TestAppContext) {
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
            "workspace-header",
            "workspace-state",
            "workspace-name",
            "workspace-task",
            "workspace-location",
            "ws-switch-agent",
            "ws-switch-diff",
            "ws-switch-editor",
            "ws-shell-toggle",
        ] {
            assert!(h.cx.debug_bounds(selector).is_some(), "{selector} is drawn");
        }
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
            ..WorkspaceDetailsInfo::default()
        });
        h.update(|v, cx| v.show("alpha", Some(row("alpha", None, WorkspaceBadge::Idle, false)), cx));
        assert_eq!(h.content_phase(), SessionPhase::Idle);
        assert!(h.ops().is_empty(), "selecting an idle workspace launches nothing");
        let header = h.view.read_with(&h.cx, |v, _| v.header()).unwrap();
        assert_eq!(header.task_line(), None);
        assert_eq!(header.location_line(), "hub · main");
        h.draw();
        assert!(h.cx.debug_bounds("ws-switch-diff").is_none(), "no view switch without a task");
        assert!(h.cx.debug_bounds("ws-shell-toggle").is_some(), "the shell is always offered");

        // An unrelated refresh (same row) doesn't retry.
        h.update(|v, cx| v.update_row(Some(row("alpha", None, WorkspaceBadge::Idle, false)), cx));
        assert!(h.sessions.attached().is_empty());

        // A dispatch started the agent: the row changes and the view attaches.
        h.sessions.idle.lock().unwrap().clear();
        h.update(|v, cx| v.update_row(Some(working()), cx));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        assert_eq!(h.sessions.attached(), vec!["demo/ws/alpha"]);
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

        h.update(|v, cx| v.close_content(cx));
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.content_view), ContentView::Agent);
        assert_eq!(h.ops().last(), Some(&WorkspaceSessionOp::Close));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/alpha"));
    }

    #[gpui::test]
    fn a_view_the_daemon_refuses_falls_back_to_the_agent(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show("alpha", Some(working()), cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.update(|v, cx| v.select_view(ContentView::Editor, cx));
        assert_eq!(h.view.read_with(&h.cx, |v, _| v.content_view), ContentView::Agent);
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
}
