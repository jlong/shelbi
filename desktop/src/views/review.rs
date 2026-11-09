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
//! - **View Diff waits for its diff** (`review-diff-loading-state`): the shared
//!   [`DiffLoad`] retries an `Ensure` the daemon refuses because the worktree
//!   is mid-load, and holds "Loading diff…" over the pane until the tool has
//!   drawn, so it never falls back to the Reviewer. After 15s it says why it
//!   couldn't, in the view, with a Retry. The pane stays attached and sized
//!   under the words, so the tool draws at the size it will be shown at.
//! - **Shelbi's own diff.** With `diff_tool: builtin` (the default), View Diff
//!   is a [`DiffView`] in the content's place, not a pane: nothing to ensure
//!   and no wait, on a remote slot too. An external tool keeps the paragraph
//!   above.
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

use shelbi_app::source::ProjectHandle;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, prelude::*, px, AnyElement, AnyWindowHandle, Bounds, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Pixels, SharedString, Subscription, Task, Window,
};
use shelbi_app::diff::{DiffTarget, TargetKind};
use shelbi_app::review::diff_load::{
    DiffLoad, DiffLoadView, DiffSeen, DIFF_FAILED_TITLE, DIFF_RETRY_LABEL, LOADING_DIFF,
};
use shelbi_app::review::{
    sub_state_detail, sub_state_label, ActiveView, PanelEffect, PanelRow, ReviewInfo,
    ReviewPanelModel, ReviewPanelState, ReviewSwitch, ReviewTarget,
};
use shelbi_app::session::{ReviewRelauncher, SessionRef};
use shelbi_app::toast::Toast;
use shelbi_app::view::{ReviewRow, ReviewState};
use shelbi_app::{CommandKind, ReviewRole, ReviewSessionOp, WorkspaceSessionOp};
use shelbi_core::{Column, StatusCategory};

use super::diff_view::{DiffDeps, DiffView, DiffViewEvent, FilePlace};
use super::session_view::{
    OnDeath, SessionDeps, SessionPhase, SessionView, SessionViewEvent, REVIEW_TERMINAL_INSET_X,
};
use super::task_panel;
use super::workspace_layout::{render_split, ShellSplit};
use crate::assets::{icons, Icon};
use crate::keys::chord_from_keystroke;
use crate::refresh::SessionSink;
use crate::theme::motion::{self, SharedTrack, SlideTrack};
use crate::theme::style::{self, ButtonKind, ButtonSize, Type, Typography, NAV_RADIUS};
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
    /// Open `task`'s review worktree (the editor) or its diff in the app the
    /// settings chose for `role`.
    fn open_in_app(&self, _project: &str, _task: &str, _role: ReviewRole) -> Result<(), String> {
        Err("apps can't be opened here".into())
    }
}

/// The real thing: the review state read through the window's project
/// source, the daemon's session ops through its link, and `open`. Answers
/// for the window's project only (the `project` its methods are handed).
pub struct LiveOps {
    project: ProjectHandle,
}

impl LiveOps {
    pub fn new(project: ProjectHandle) -> Self {
        LiveOps { project }
    }
}

impl ReviewOps for LiveOps {
    fn resolve(&self, project: &str, task: &str) -> Result<ReviewTarget, String> {
        use shelbi_app::source::ReviewOpenTarget;
        debug_assert_eq!(project, self.project.name());
        match self.project.source.review_open_target(task)? {
            ReviewOpenTarget::Serving(info) => {
                let (title, body) = info
                    .task
                    .as_ref()
                    .map(|t| (t.task.title.clone(), t.body.clone()))
                    .unwrap_or_default();
                let pr = info.task.as_ref().and_then(|t| shelbi_app::board::task_pr(&t.task));
                Ok(ReviewTarget::Serving(ReviewInfo {
                    task_id: task.to_string(),
                    slot: info.slot,
                    title,
                    body,
                    worktree: info.worktree,
                    editor_name: info.editor_name,
                    apps: info.apps,
                    review_url: info.review_url,
                    has_health_check: info.has_health_check,
                    remote_machine: info.remote_machine,
                    pr,
                }))
            }
            ReviewOpenTarget::Queued { title } => {
                // The free slots only: a busy slot is left to its occupant,
                // never evicted (the TUI's load).
                let all = self.project.source.review_slots()?;
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
        debug_assert_eq!(project, self.project.name());
        self.project
            .link
            .review_session(task, op, &mut |_, _| {})
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn open_in_app(&self, project: &str, task: &str, role: ReviewRole) -> Result<(), String> {
        if let Some(host) = self.project.key().host() {
            return Err(shelbi_app::remote_window::local_open_unavailable(host));
        }
        shelbi_orchestrator::review_session::open_in_app(project, task, role.into())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn open_url(&self, url: &str) -> Result<(), String> {
        run_open(url)
    }

    fn reveal(&self, path: &str) -> Result<(), String> {
        if let Some(host) = self.project.key().host() {
            return Err(shelbi_app::remote_window::local_open_unavailable(host));
        }
        run_open(path)
    }

    fn shell_op(&self, project: &str, slot: &str, op: WorkspaceSessionOp) -> Result<(), String> {
        debug_assert_eq!(project, self.project.name());
        self.project
            .link
            .workspace_session(slot, op, &mut |_, _| {})
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
    /// Opens Shelbi's own diff view.
    pub diff: DiffDeps,
}

impl ReviewDeps {
    /// The real daemon and sessions for `project`.
    pub fn live(project: &ProjectHandle) -> Self {
        let handle = project.clone();
        Self {
            ops: Arc::new(LiveOps::new(project.clone())),
            sessions: Rc::new(move |task| {
                SessionDeps::live_with(
                    &handle.link,
                    Arc::new(ReviewRelauncher {
                        project: handle.name().to_string(),
                        task: task.to_string(),
                        link: Some(handle.link.clone()),
                    }),
                )
            }),
            diff: DiffDeps::live(project),
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
    /// `gY` in the diff: Reject, its reason starting from the comments.
    RejectWith { id: String, reason: String },
    /// Leave the review loaded and go back to the previous view.
    Back,
    /// More: open this task's detail over the screen.
    OpenTask(String),
    /// The review was torn down (the daemon was asked to end its sessions):
    /// go back to the previous view.
    Closed,
    /// Put this toast up over the main area (`Launching Zed…`), or take the
    /// one showing down (`None`: a terminal editor takes the main area).
    Toast(Option<Toast>),
}

/// Where keyboard focus sits inside the review.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReviewFocus {
    Panel,
    Content,
}

/// How often a running [`DiffLoad`] looks at the pane and its clock.
const DIFF_TICK: Duration = Duration::from_millis(100);

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
    /// View Diff's wait for its diff, once it was asked for.
    diff_load: Option<DiffLoad>,
    /// What the wait last drew, so the window redraws when that changes.
    diff_drawn: Option<DiffLoadView>,
    /// The content pane's clock.
    clock: Rc<dyn Fn() -> Instant>,
    /// Shelbi's own diff, once View Diff was shown. It keeps its place while
    /// another view is up; a review's checkout doesn't move, so it's idle.
    diff: Option<ShownDiff>,
}

struct ShownDiff {
    view: Entity<DiffView>,
    _events: Subscription,
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
    diff_deps: DiffDeps,
    /// The keyboard moves to the content at the next frame: a view switch
    /// swapped what it's in.
    refocus: bool,
    /// A click asked for the content (the review's sidebar row, a panel
    /// view): the next frame that finds the review resolved hands it the keys
    /// ([`Self::land_wanted_content`]), unless they've left the view since.
    content_wanted: bool,
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
    /// The next look at a running [`DiffLoad`].
    _diff_tick: Option<Task<()>>,
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
            diff_deps: deps.diff,
            refocus: false,
            content_wanted: false,
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
            _diff_tick: None,
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
            apps: Default::default(),
            review_url: None,
            has_health_check: false,
            remote_machine: None,
            pr: None,
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
        let clock = deps.clock.clone();
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
            diff_load: None,
            diff_drawn: None,
            clock,
            diff: None,
        }
    }

    /// Hide the view: detach the content pane (the daemon's sessions stay).
    pub fn hide(&mut self, cx: &mut Context<Self>) {
        if !self.shown {
            return;
        }
        self.shown = false;
        self.held = None;
        self.content_wanted = false;
        self.split.dragging = false;
        self.close_shell(cx);
        if let Some(r) = self.review.as_mut() {
            if let Some(load) = r.diff_load.as_mut() {
                load.left();
            }
            r.content.update(cx, |c, cx| c.hide(cx));
            if let Some(diff) = &r.diff {
                diff.view.update(cx, |d, _| d.set_visible(false));
            }
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

    /// `editor:` or `diff_tool:` was saved: re-read the review's Edit in …
    /// name and which of Edit / View Diff open in an app, and show them.
    pub fn tools_changed(&mut self, cx: &mut Context<Self>) {
        let Some(task) = self.review.as_ref().filter(|r| r.resolved).map(|r| r.info.task_id.clone()) else {
            return;
        };
        let ops = self.ops.clone();
        let project = self.project.clone();
        let read = cx.background_executor().spawn(async move { ops.resolve(&project, &task) });
        cx.spawn(async move |this, cx| {
            let Ok(ReviewTarget::Serving(fresh)) = read.await else {
                return;
            };
            this.update(cx, |this, cx| {
                let Some(r) = this.review.as_mut().filter(|r| r.resolved && r.info.task_id == fresh.task_id) else {
                    return;
                };
                if r.info.editor_name == fresh.editor_name && r.info.apps == fresh.apps {
                    return;
                }
                let diff_changed = r.info.apps.builtin_diff != fresh.apps.builtin_diff;
                r.info.editor_name = fresh.editor_name;
                r.info.apps = fresh.apps;
                r.model = ReviewPanelModel::build(&r.info, r.row.as_ref());
                r.panel.sync(&r.model);
                // View Diff is up, and now means the other thing.
                if diff_changed {
                    r.diff = None;
                    if r.panel.active() == ActiveView::Diff {
                        this.select_view(ActiveView::Diff, cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        if let (Some(diff), ReviewFocus::Content) = (self.diff_on_show(), self.focus) {
            return diff.read(cx).focus_handle(cx);
        }
        match (&self.review, self.focus) {
            (Some(r), ReviewFocus::Content) if r.content.read(cx).pane().is_some() => {
                r.content.read(cx).focus_target(cx)
            }
            _ => self.focus_handle.clone(),
        }
    }

    /// Where the keyboard goes when the diff went off screen holding it (a view
    /// chord, a click on another view): the view that's up now, which takes it
    /// from its own handle once its pane attaches. Left on a handle that isn't
    /// drawn, no key would reach anything, the window's chords included.
    fn focus_off_hidden_diff(&self, window: &Window, cx: &gpui::App) -> Option<FocusHandle> {
        let r = self.review.as_ref()?;
        let diff = r.diff.as_ref().filter(|d| d.view.read(cx).contains_focus(window))?;
        if self.diff_on_show().is_some_and(|shown| shown == &diff.view) {
            return None;
        }
        Some(match self.focus {
            ReviewFocus::Content => r.content.read(cx).focus_target(cx),
            ReviewFocus::Panel => self.focus_handle.clone(),
        })
    }

    /// Give the panel keyboard focus (the sidebar chord while the panel holds
    /// the sidebar's column).
    pub fn focus_panel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus = ReviewFocus::Panel;
        self.content_wanted = false;
        window.focus(&self.focus_handle, cx);
        cx.notify();
    }

    /// Whether the content's terminal has the keys, for the window's tests.
    #[cfg(test)]
    pub(crate) fn content_has_keys(&self, window: &Window, cx: &gpui::App) -> bool {
        self.review
            .as_ref()
            .and_then(|r| r.content.read(cx).pane().map(|p| p.read(cx).focus_handle(cx)))
            .is_some_and(|h| h.is_focused(window))
    }

    /// A click opened this review (its sidebar row) or one of its views: the
    /// content takes the keys, its terminal once the pane attaches. The
    /// keyboard's Enter leaves them on the panel. Lands at the next frame.
    pub fn want_content(&mut self, cx: &mut Context<Self>) {
        self.content_wanted = true;
        cx.notify();
    }

    /// Hand the keys to the content a click asked for. Waits while the review
    /// is a placeholder (its content is rebuilt when it resolves); gives up
    /// when the keys have left the view (a dialog, a field, the palette took
    /// them) or a merge holds the review still. The terminal takes them when
    /// its pane is the shown view's; otherwise its session view does, which
    /// hands them on when the pane attaches.
    fn land_wanted_content(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.content_wanted {
            return;
        }
        if !self.shown || !self.contains_focus(window, cx) {
            self.content_wanted = false;
            return;
        }
        let Some((resolved, merging)) = self.review.as_ref().map(|r| (r.resolved, r.panel.is_merging())) else {
            return;
        };
        if !resolved {
            return;
        }
        self.content_wanted = false;
        let Some(r) = self.review.as_ref().filter(|_| !merging) else {
            return;
        };
        let target = match self.diff_on_show() {
            Some(diff) => diff.read(cx).focus_handle(cx),
            None => {
                let content = r.content.read(cx);
                let shown = self.content_target(r.panel.active());
                match content.pane() {
                    Some(pane) if shown.as_ref() == Some(content.target()) => pane.read(cx).focus_handle(cx),
                    _ => Focusable::focus_handle(content, cx),
                }
            }
        };
        self.focus = ReviewFocus::Content;
        window.focus(&target, cx);
        cx.emit(ReviewEvent::Focused);
    }

    /// Whether keyboard focus is in the view (its panel or its terminal).
    pub fn contains_focus(&self, window: &Window, cx: &gpui::App) -> bool {
        self.focus_handle.is_focused(window)
            || self.shell.read(cx).contains_focus(window, cx)
            || self
                .review
                .as_ref()
                .is_some_and(|r| r.content.read(cx).contains_focus(window, cx))
            || self.diff_with_focus(window, cx).is_some()
    }

    /// The built-in diff, while it's what the main area shows.
    pub fn diff_on_show(&self) -> Option<&Entity<DiffView>> {
        let r = self.review.as_ref()?;
        (r.panel.active() == ActiveView::Diff).then_some(r.diff.as_ref()).flatten().map(|d| &d.view)
    }

    /// The built-in diff, while it has the keyboard.
    pub fn diff_with_focus(&self, window: &Window, cx: &gpui::App) -> Option<&Entity<DiffView>> {
        self.diff_on_show().filter(|d| d.read(cx).contains_focus(window))
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

    /// Show the agent, the diff or the editor. The editor is ensured through
    /// the daemon first, a failure falling back to the agent with the error on
    /// the status line, as the TUI does. The diff waits for itself in its own
    /// view ([`Self::select_diff`]).
    pub fn select_view(&mut self, view: ActiveView, cx: &mut Context<Self>) {
        let Some(target) = self.content_target(view) else {
            return;
        };
        if let Some(r) = self.review.as_mut() {
            r.panel.set_active(view);
            if view != ActiveView::Diff {
                if let Some(load) = r.diff_load.as_mut() {
                    load.left();
                }
            }
        }
        if view == ActiveView::Diff {
            if self.review.as_ref().is_some_and(|r| r.info.apps.builtin_diff) {
                self.show_builtin_diff(cx);
            } else {
                self.select_diff(cx);
            }
            return;
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

    /// View Diff is Shelbi's own: show the view, opening it the first time.
    /// The pane behind it detaches; the sessions stay with the daemon.
    fn show_builtin_diff(&mut self, cx: &mut Context<Self>) {
        let far = self.diff_deps.far.clone();
        let deps = self.diff_deps.clone();
        let project = self.project.clone();
        let focus = self.focus;
        // A pane waiting for the push no longer has anything to attach to.
        self.held = None;
        let Some(r) = self.review.as_mut() else {
            return;
        };
        r.content.update(cx, |c, cx| c.hide(cx));
        let info = &r.info;
        let opened = r.diff.get_or_insert_with(|| {
            let target = DiffTarget {
                project,
                workspace: info.slot.clone(),
                kind: TargetKind::Review,
                task: Some(info.task_id.clone()),
            };
            let place = match (far, &info.remote_machine) {
                (Some(why), _) => FilePlace::Elsewhere(why),
                (None, Some(machine)) => FilePlace::Elsewhere(format!("The file is on {machine}: there's no worktree here to open.")),
                (None, None) => FilePlace::Local(info.worktree.clone()),
            };
            let view = cx.new(|cx| DiffView::new(target, place, &deps, cx));
            let events = cx.subscribe(&view, |this, _, ev: &DiffViewEvent, cx| match ev {
                DiffViewEvent::Focused => {
                    this.focus = ReviewFocus::Content;
                    cx.emit(ReviewEvent::Focused);
                }
                DiffViewEvent::ReleaseFocus => {
                    this.focus = ReviewFocus::Panel;
                    this.content_wanted = false;
                    this.refocus = true;
                    cx.notify();
                }
                DiffViewEvent::Reject(reason) => {
                    if let Some(id) = this.task_id().map(str::to_string) {
                        cx.emit(ReviewEvent::RejectWith { id, reason: reason.clone() });
                    }
                }
            });
            ShownDiff { view, _events: events }
        });
        opened.view.update(cx, |d, cx| {
            d.set_visible(true);
            if focus == ReviewFocus::Content {
                d.request_focus(cx);
            }
        });
        cx.notify();
    }

    /// View Diff was selected: start or resume the wait for the diff.
    fn select_diff(&mut self, cx: &mut Context<Self>) {
        let seen = self.diff_seen(cx);
        let Some(r) = self.review.as_mut() else {
            return;
        };
        let now = (r.clock)();
        if DiffLoad::select(&mut r.diff_load, &seen, now) {
            self.send_diff_ensure(cx);
        }
        cx.notify();
        self.drive_diff(cx);
    }

    /// Show View Diff with this wait, as it stands (the visual check's stand-in
    /// for a daemon that keeps refusing).
    #[cfg(feature = "visual-check")]
    pub fn offscreen_diff_wait(&mut self, load: DiffLoad, cx: &mut Context<Self>) {
        if let Some(r) = self.review.as_mut() {
            r.panel.set_active(ActiveView::Diff);
            r.diff_load = Some(load);
        }
        cx.notify();
    }

    /// Retry: start the wait for the diff over.
    fn retry_diff(&mut self, cx: &mut Context<Self>) {
        let Some(r) = self.review.as_mut() else {
            return;
        };
        let now = (r.clock)();
        if r.diff_load.as_mut().is_some_and(|load| load.retry(now)) {
            self.send_diff_ensure(cx);
        }
        self.drive_diff(cx);
    }

    /// Ask the daemon for the diff session; the answer lands in the wait.
    fn send_diff_ensure(&mut self, cx: &mut Context<Self>) {
        let op = ReviewSessionOp::Ensure { role: ReviewRole::Diff };
        self.run_op(op, cx, |this, result, cx| {
            if let Some(r) = this.review.as_mut() {
                let now = (r.clock)();
                if let Some(load) = r.diff_load.as_mut() {
                    load.ensure_answered(result, now);
                }
            }
            this.drive_diff(cx);
        });
    }

    /// What the pane shows of the diff session.
    fn diff_seen(&self, cx: &gpui::App) -> DiffSeen {
        let (Some(r), Some(target)) = (&self.review, self.content_target(ActiveView::Diff)) else {
            return DiffSeen::NotYet;
        };
        let content = r.content.read(cx);
        if *content.target() != target {
            return DiffSeen::NotYet;
        }
        match content.phase() {
            SessionPhase::Live if content.pane().is_some_and(|p| p.read(cx).view().emulator().has_content()) => {
                DiffSeen::Drawn
            }
            SessionPhase::Failed(why) | SessionPhase::GaveUp(why) => DiffSeen::Ended(why),
            SessionPhase::Idle => DiffSeen::Ended("the diff session isn't running".to_string()),
            _ => DiffSeen::NotYet,
        }
    }

    /// One step of the wait: send a retry that's due, attach a session that's
    /// up, note what the pane shows, and look again soon if it isn't over.
    fn drive_diff(&mut self, cx: &mut Context<Self>) {
        let active = self.shown && self.panel().is_some_and(|p| p.active() == ActiveView::Diff);
        let target = self.content_target(ActiveView::Diff);
        let Some(r) = self.review.as_mut() else {
            return;
        };
        let now = (r.clock)();
        let Some(load) = r.diff_load.as_mut() else {
            return;
        };
        let send = load.take_due_ensure(now);
        let attach = active && load.needs_attach();
        if attach {
            load.mark_attached();
        }
        if send {
            self.send_diff_ensure(cx);
        }
        if let (true, Some(target)) = (attach, target) {
            self.attach(target, cx);
        }
        let seen = if active { self.diff_seen(cx) } else { DiffSeen::NotYet };
        let Some(r) = self.review.as_mut() else {
            return;
        };
        let Some(load) = r.diff_load.as_mut() else {
            return;
        };
        load.observe(&seen, now);
        let drawn = load.view(now);
        let tick = load.needs_tick(active);
        if r.diff_drawn.as_ref() != Some(&drawn) {
            r.diff_drawn = Some(drawn);
            cx.notify();
        }
        if tick && self._diff_tick.is_none() {
            let timer = cx.background_executor().timer(DIFF_TICK);
            self._diff_tick = Some(cx.spawn(async move |this, cx| {
                timer.await;
                let _ = this.update(cx, |this, cx| {
                    this._diff_tick = None;
                    this.drive_diff(cx);
                });
            }));
        }
    }

    /// What View Diff's wait draws over the pane now, if anything. Before
    /// "Loading" is due (a ready diff is up by then) a pane already bound to
    /// the session is covered bare, so it reads as nothing rather than
    /// "Connecting…"; one not bound yet still shows the view it came from.
    fn diff_cover(&self, cx: &gpui::App) -> Option<DiffLoadView> {
        let r = self.review.as_ref()?;
        if r.panel.active() != ActiveView::Diff {
            return None;
        }
        let load = r.diff_load.as_ref()?;
        // Asked of the pane, not the wait's last look: a drawn diff shows now.
        if self.diff_seen(cx) == DiffSeen::Drawn {
            return None;
        }
        match load.view((r.clock)()) {
            DiffLoadView::Ready => None,
            DiffLoadView::Settling if !load.is_attached() => None,
            view => Some(view),
        }
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
                // Shelbi's own diff is driven from the keyboard: asking for it
                // hands it the keys, with Esc to come back.
                if self.review.as_ref().is_some_and(|r| r.info.apps.builtin_diff) {
                    self.focus = ReviewFocus::Content;
                    cx.emit(ReviewEvent::Focused);
                }
                self.select_view(ActiveView::Diff, cx);
            }
            PanelEffect::ShowEditor => {
                self.set_status(None, cx);
                // A terminal editor: never under a toast.
                cx.emit(ReviewEvent::Toast(None));
                self.select_view(ActiveView::Editor, cx);
            }
            // The editor app takes a moment to show: a toast says it's
            // coming, and its failure replaces it. The content view stays.
            PanelEffect::OpenApp(ReviewRole::Editor) => {
                self.set_status(None, cx);
                let name = self.model().map(|m| m.editor_name.clone()).unwrap_or_default();
                cx.emit(ReviewEvent::Toast(Some(Toast::launching_editor(&name))));
                let project = self.project.clone();
                let ops = self.ops.clone();
                let work = cx
                    .background_executor()
                    .spawn(async move { ops.open_in_app(&project, &task, ReviewRole::Editor) });
                // The toast is the window's, so the failure shows even after
                // the review moved on: it names the editor, not the task.
                cx.spawn(async move |this, cx| {
                    if let Err(e) = work.await {
                        this.update(cx, |_, cx| {
                            cx.emit(ReviewEvent::Toast(Some(Toast::editor_launch_failed(&name, &e))));
                        })
                        .ok();
                    }
                })
                .detach();
            }
            // The app opens; the content view stays as it is.
            PanelEffect::OpenApp(role) => {
                self.set_status(None, cx);
                let project = self.project.clone();
                self.run_opener("open the app", move |ops| ops.open_in_app(&project, &task, role), cx);
            }
            PanelEffect::OpenPr => {
                if let Some(url) = self.model().and_then(|m| m.pr_url().map(str::to_string)) {
                    self.run_opener("open the pull request", move |ops| ops.open_url(&url), cx);
                }
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
        self.content_wanted = false;
        window.focus(&self.focus_handle, cx);
        let Some(effect) = self.review.as_mut().map(|r| r.panel.click(row)) else {
            return;
        };
        // A view picked with the mouse is ready for the keyboard.
        let view = matches!(effect, PanelEffect::ShowChat | PanelEffect::ShowDiff | PanelEffect::ShowEditor);
        self.apply(effect, cx);
        if view && !self.is_pending() {
            self.want_content(cx);
        }
        cx.notify();
    }

    /// A key while the panel has focus.
    fn panel_key(&mut self, event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(chord) = chord_from_keystroke(&event.keystroke) else {
            return;
        };
        // Tab moves to the content terminal once it is live (the TUI's).
        if chord.code == shelbi_state::keymap::Key::Tab && chord.mods.is_empty() {
            if let Some(target) = self.diff_on_show().map(|d| d.read(cx).focus_handle(cx)) {
                self.focus = ReviewFocus::Content;
                window.focus(&target, cx);
            } else if let Some(r) = &self.review {
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

        // The pull request, under the summary it belongs to.
        let pr = model.pr.as_ref().map(|pr| {
            let mut link = task_panel::pr_link("review-pr", pr, selected(PanelRow::Pr), theme);
            if pr.url.is_some() {
                link = link.on_click(listen(PanelRow::Pr));
            }
            div()
                .debug_selector(|| "review-pr-row".into())
                .flex()
                .flex_row()
                .flex_none()
                .items_center()
                .gap(px(8.0))
                .px(px(18.0))
                .pb(px(8.0))
                .min_w_0()
                .child(
                    div()
                        .flex_none()
                        .type_style(Type::Small)
                        .text_color(theme.muted)
                        .child("PR"),
                )
                .child(task_panel::pr_value(link, pr, theme))
        });

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
                        ButtonKind::Success,
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
            .children(pr)
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
        // The pane stays laid out under the wait, sized for when it shows.
        let cover = self.diff_cover(cx);
        let builtin = self.diff_on_show().cloned();
        let content = match builtin {
            Some(diff) => div()
                .debug_selector(|| "review-content".into())
                .relative()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .h_full()
                .child(diff)
                .into_any_element(),
            None => div()
            .debug_selector(|| "review-content".into())
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .when(cover.is_some(), |d| d.opacity(0.0))
                    .child(shown.content.clone()),
            )
            .children(cover.map(|view| self.render_diff_cover(view, &theme, cx)))
            .into_any_element(),
        };
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

impl ReviewView {
    /// "Loading diff…", or why it couldn't, over the pane.
    fn render_diff_cover(&self, view: DiffLoadView, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let cover = div()
            .id("review-diff-cover")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_3()
            .px_4()
            .text_center();
        match view {
            DiffLoadView::Settling | DiffLoadView::Ready => cover.into_any_element(),
            DiffLoadView::Loading => cover
                .debug_selector(|| "review-diff-loading".into())
                .type_style(Type::Base)
                .text_color(theme.muted)
                .child(LOADING_DIFF)
                .into_any_element(),
            DiffLoadView::Failed { cause } => cover
                .debug_selector(|| "review-diff-failed".into())
                .child(
                    div()
                        .type_style(Type::BaseMedium)
                        .text_color(theme.text_loud)
                        .child(DIFF_FAILED_TITLE),
                )
                .child(
                    div()
                        .debug_selector(|| "review-diff-cause".into())
                        .max_w(px(520.0))
                        .type_style(Type::Base)
                        .text_color(theme.muted)
                        .child(cause),
                )
                .child(
                    style::button("review-diff-retry", DIFF_RETRY_LABEL, ButtonKind::Secondary, ButtonSize::Base, false, theme)
                        .debug_selector(|| "review-diff-retry".into())
                        .on_click(cx.listener(|this, _, _, cx| this.retry_diff(cx))),
                )
                .into_any_element(),
        }
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
        if let Some(target) = self.focus_off_hidden_diff(window, cx) {
            window.focus(&target, cx);
        }
        if std::mem::take(&mut self.refocus) {
            let target = self.focus_target(cx);
            window.focus(&target, cx);
        }
        self.land_wanted_content(window, cx);
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
        /// How far the fake clock has moved on from where the deps started it.
        elapsed: Mutex<Duration>,
    }

    impl FakeSessions {
        /// Send output to the latest attach of the session `name`.
        fn output(&self, name: &str, data: &[u8]) {
            let attaches = self.attaches.lock().unwrap();
            let (_, tx) = attaches
                .iter()
                .rev()
                .find(|(n, _)| n == name)
                .unwrap_or_else(|| panic!("{name} is attached"));
            tx.unbounded_send(SessionEvent::Output { seq: 0, data: data.to_vec() }).unwrap();
        }

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
        pub(crate) ops: Mutex<Vec<(String, ReviewSessionOp)>>,
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

        /// Move the fake clock and the timers on by `by`, a tick at a time.
        fn advance(&mut self, by: Duration) {
            let mut left = by;
            while !left.is_zero() {
                let step = left.min(DIFF_TICK);
                *self.sessions.elapsed.lock().unwrap() += step;
                self.cx.executor().advance_clock(step);
                self.cx.run_until_parked();
                left -= step;
            }
        }

        /// Whether `selector` is drawn now.
        fn drawn(&mut self, selector: &'static str) -> bool {
            self.draw();
            self.cx.debug_bounds(selector).is_some()
        }

        fn cover(&self) -> Option<DiffLoadView> {
            self.view.read_with(&self.cx, |v, cx| v.diff_cover(cx))
        }

        fn diff_ensures(&self) -> usize {
            self.ops().iter().filter(|op| **op == ReviewSessionOp::Ensure { role: ReviewRole::Diff }).count()
        }

        fn active(&self) -> ActiveView {
            self.view.read_with(&self.cx, |v, _| v.panel().unwrap().active())
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
            apps: Default::default(),
            review_url: Some("http://localhost:4101".into()),
            has_health_check: true,
            remote_machine: None,
            pr: None,
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
            pr: None,
        }
    }

    /// Review deps over fake sessions and daemon ops: nothing reaches a
    /// daemon or a session (the window's tests use them too).
    pub(crate) fn fake_deps() -> (ReviewDeps, Arc<FakeSessions>, Arc<FakeOps>) {
        let sessions = Arc::new(FakeSessions::default());
        let ops = Arc::new(FakeOps::default());
        let start = Instant::now();
        let deps = {
            let sessions = sessions.clone();
            ReviewDeps {
                ops: ops.clone(),
                sessions: Rc::new(move |_task| {
                    let clock = sessions.clone();
                    SessionDeps {
                        connector: sessions.clone(),
                        relauncher: sessions.clone(),
                        retry: RetryPolicy::default(),
                        reconnect: ReconnectPolicy {
                            first_backoff: Duration::from_secs(1),
                            max_backoff: Duration::from_secs(4),
                        },
                        clock: Rc::new(move || start + *clock.elapsed.lock().unwrap()),
                    }
                }),
                diff: DiffDeps::canned(crate::views::diff_view::canned::SAMPLE_PATCH, Arc::default()),
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
    fn a_tool_saved_in_settings_reaches_the_open_review(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        let mut fresh = info();
        fresh.editor_name = "Zed".into();
        fresh.apps = shelbi_state::AppTools { editor: true, diff: true, ..Default::default() };
        *h.ops.resolve_with.lock().unwrap() = Some(Ok(ReviewTarget::Serving(fresh)));
        h.update(|v, cx| v.tools_changed(cx));
        let model = h.view.read_with(&h.cx, |v, _| v.model().cloned()).unwrap();
        assert_eq!(model.editor_name, "Zed");
        assert_eq!(model.apps, shelbi_state::AppTools { editor: true, diff: true, ..Default::default() });
        assert_eq!(model.title, info().title, "the rest of the review is kept");
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
    fn an_editor_the_daemon_refuses_falls_back_to_chat(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.click("review-switch-editor");
        assert_eq!(h.active(), ActiveView::Chat);
        assert_eq!(h.status_line().as_deref(), Some("couldn't open view: the daemon said no"));
        assert_eq!(h.content_phase(), SessionPhase::Live);
    }

    const DIFF_SESSION: &str = "demo/review/review-1/diff";

    fn builtin_info() -> ReviewInfo {
        let mut i = info();
        i.apps.builtin_diff = true;
        i
    }

    fn diff_up(h: &Harness) -> bool {
        h.view.read_with(&h.cx, |v, _| v.diff_on_show().is_some())
    }

    /// Let the canned diff's worker land.
    fn load_diff(h: &mut Harness) {
        let diff = h.view.read_with(&h.cx, |v, _| v.diff_on_show().cloned()).expect("the diff is up");
        let start = Instant::now();
        loop {
            h.cx.update(|_, cx| diff.update(cx, |d, cx| d.poll_now(cx)));
            h.cx.run_until_parked();
            if !h.cx.update(|_, cx| diff.read(cx).state().loading()) || start.elapsed() > Duration::from_secs(10) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        h.draw();
    }

    #[gpui::test]
    fn the_builtin_diff_replaces_the_pane_with_no_session(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(builtin_info(), None, cx));
        h.click("review-switch-diff");
        h.draw();
        assert_eq!(h.active(), ActiveView::Diff);
        assert!(diff_up(&h));
        assert!(h.drawn("diff-view"));
        assert_eq!(h.diff_ensures(), 0, "nothing asked of the daemon");
        assert!(!h.sessions.attached().iter().any(|n| n == DIFF_SESSION));
        let target = h.view.read_with(&h.cx, |v, cx| v.diff_on_show().unwrap().read(cx).target().clone());
        assert_eq!(target.kind, shelbi_app::diff::TargetKind::Review);
        assert_eq!(target.workspace, "review-1");
        assert_eq!(target.task.as_deref(), Some("t-1"));
        // It takes the keys, and Esc hands them to the panel.
        let in_diff = h.cx.update(|window, cx| h.view.read(cx).diff_with_focus(window, cx).is_some());
        assert!(in_diff, "asking for it hands it the keyboard");
        load_diff(&mut h);
        h.key("j");
        let moved = h.view.read_with(&h.cx, |v, cx| v.diff_on_show().unwrap().read(cx).state().cursor());
        assert_eq!(moved, 1);
        h.key("escape");
        h.draw();
        let in_panel = h.cx.update(|window, cx| h.view.read(cx).focus_handle.is_focused(window));
        assert!(in_panel);
        // Reviewer again: the pane reattaches and the diff keeps its place.
        h.click("review-switch-chat");
        h.draw();
        assert!(!h.drawn("diff-view"));
        assert_eq!(h.content_phase(), SessionPhase::Live);
        h.click("review-switch-diff");
        h.draw();
        let kept = h.view.read_with(&h.cx, |v, cx| v.diff_on_show().unwrap().read(cx).state().cursor());
        assert_eq!(kept, 1);
    }

    #[gpui::test]
    fn an_external_tool_keeps_its_pane(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.click("review-switch-diff");
        h.draw();
        assert!(!diff_up(&h));
        assert!(!h.drawn("diff-view"));
        assert_eq!(h.diff_ensures(), 1);
    }

    #[gpui::test]
    fn a_remote_slot_offers_the_builtin_diff(cx: &mut TestAppContext) {
        let mut h = open(cx);
        let mut remote = builtin_info();
        remote.remote_machine = Some("gpu".into());
        h.update(|v, cx| v.show(remote, None, cx));
        h.draw();
        assert!(h.drawn("review-switch-diff"));
        assert!(!h.drawn("review-switch-editor"));
        h.click("review-switch-diff");
        assert!(diff_up(&h));
    }

    #[gpui::test]
    fn the_tool_changing_swaps_what_view_diff_shows(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(builtin_info(), None, cx));
        h.click("review-switch-diff");
        assert!(diff_up(&h));
        *h.ops.resolve_with.lock().unwrap() = Some(Ok(ReviewTarget::Serving(info())));
        h.update(|v, cx| v.tools_changed(cx));
        h.draw();
        assert!(!diff_up(&h), "the external tool now");
        assert_eq!(h.active(), ActiveView::Diff);
        assert_eq!(h.diff_ensures(), 1);
    }

    #[gpui::test]
    fn a_ready_diff_opens_with_no_loading(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.click("review-switch-diff");
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some(DIFF_SESSION));
        // Inside the grace the pane is only covered bare, and the replay ends it.
        assert_eq!(h.cover(), Some(DiffLoadView::Settling));
        assert!(!h.drawn("review-diff-loading"));
        h.sessions.output(DIFF_SESSION, b"diff --git a/x b/x");
        h.advance(DIFF_TICK);
        assert_eq!(h.cover(), None, "the terminal shows");
        assert!(!h.drawn("review-diff-loading"));
        assert_eq!(h.diff_ensures(), 1);

        // A long while on, with nothing to wait for, there's still no error.
        h.advance(Duration::from_secs(20));
        assert_eq!(h.cover(), None);
    }

    #[gpui::test]
    fn a_worktree_not_ready_loads_then_retries_into_the_diff(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.click("review-switch-diff");

        // No fallback, no error: the view stays on the diff and says Loading.
        assert_eq!(h.active(), ActiveView::Diff);
        assert_eq!(h.status_line(), None);
        h.advance(Duration::from_secs(2));
        assert!(h.drawn("review-diff-loading"));
        assert_eq!(h.active(), ActiveView::Diff);
        let tries = h.diff_ensures();
        assert!(tries > 1, "the refused ensure is retried, {tries} so far");
        assert!(!h.sessions.attached().contains(&DIFF_SESSION.to_string()));

        // The worktree comes ready; the next retry attaches.
        *h.ops.fail_ops.lock().unwrap() = false;
        h.advance(Duration::from_secs(2));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some(DIFF_SESSION));
        assert!(h.drawn("review-diff-loading"), "the tool hasn't drawn yet");

        // The tool draws: no click, the diff replaces Loading.
        h.sessions.output(DIFF_SESSION, b"diff --git a/x b/x");
        h.advance(DIFF_TICK);
        assert!(!h.drawn("review-diff-loading"));
        assert_eq!(h.cover(), None);
    }

    #[gpui::test]
    fn a_persistent_failure_times_out_with_the_cause_and_a_retry(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.click("review-switch-diff");

        h.advance(Duration::from_millis(14_800));
        assert!(h.drawn("review-diff-loading"), "no error before 15s");
        assert!(!h.drawn("review-diff-failed"));
        h.advance(Duration::from_millis(300));
        assert!(h.drawn("review-diff-failed"));
        assert_eq!(h.cover(), Some(DiffLoadView::Failed { cause: "the daemon said no".into() }));
        assert_eq!(h.active(), ActiveView::Diff, "still no fallback");

        // It stops asking until told to.
        let tries = h.diff_ensures();
        h.advance(Duration::from_secs(10));
        assert_eq!(h.diff_ensures(), tries);

        // Retry starts it over, and this time it works.
        *h.ops.fail_ops.lock().unwrap() = false;
        h.click("review-diff-retry");
        assert_eq!(h.diff_ensures(), tries + 1);
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some(DIFF_SESSION));
        h.sessions.output(DIFF_SESSION, b"diff --git a/x b/x");
        h.advance(DIFF_TICK);
        assert_eq!(h.cover(), None);
    }

    #[gpui::test]
    fn a_session_that_never_draws_times_out_and_a_late_draw_replaces_the_error(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.click("review-switch-diff");
        h.advance(Duration::from_millis(15_100));
        assert!(matches!(h.cover(), Some(DiffLoadView::Failed { .. })));
        assert!(h.drawn("review-diff-failed"));

        h.sessions.output(DIFF_SESSION, b"finally");
        h.advance(DIFF_TICK);
        assert_eq!(h.cover(), None);
    }

    #[gpui::test]
    fn switching_away_and_back_mid_load_adds_no_ensure(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        *h.ops.fail_ops.lock().unwrap() = true;
        h.click("review-switch-diff");
        assert_eq!(h.diff_ensures(), 1);

        // Away and straight back, inside the first backoff: nothing new is sent.
        h.click("review-switch-chat");
        assert_eq!(h.cover(), None, "the Reviewer shows, uncovered");
        h.click("review-switch-diff");
        assert_eq!(h.diff_ensures(), 1);
        assert_eq!(h.active(), ActiveView::Diff);

        // Comes ready while away: nothing attaches until it's back.
        h.click("review-switch-chat");
        *h.ops.fail_ops.lock().unwrap() = false;
        h.advance(Duration::from_secs(2));
        let tries = h.diff_ensures();
        assert!(!h.sessions.attached().contains(&DIFF_SESSION.to_string()));
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some("demo/ws/review-1"));
        h.click("review-switch-diff");
        assert_eq!(h.diff_ensures(), tries, "the wait already has its session");
        assert_eq!(h.sessions.attached().last().map(String::as_str), Some(DIFF_SESSION));
    }

    #[gpui::test]
    fn coming_back_to_a_drawn_diff_doesnt_flash_loading(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.click("review-switch-diff");
        h.sessions.output(DIFF_SESSION, b"diff --git a/x b/x");
        h.advance(DIFF_TICK);
        assert_eq!(h.cover(), None);

        // Clicked again with the pane showing it: nothing to do.
        let ensures = h.diff_ensures();
        h.click("review-switch-diff");
        assert_eq!(h.diff_ensures(), ensures);
        assert_eq!(h.cover(), None);

        // Via the Reviewer: re-ensured (the session may have gone), attached at
        // once, its replay in before Loading is due.
        h.click("review-switch-chat");
        h.click("review-switch-diff");
        assert_eq!(h.diff_ensures(), ensures + 1);
        h.sessions.output(DIFF_SESSION, b"diff --git a/x b/x");
        h.advance(DIFF_TICK);
        assert_eq!(h.cover(), None);
        assert!(!h.drawn("review-diff-loading"));
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
    fn the_panel_shows_the_pr_and_a_click_opens_it(cx: &mut TestAppContext) {
        use shelbi_core::{IssuePr, PrCi, PrState};
        let url = "https://github.com/o/r/pull/2137";
        let mut h = open(cx);
        let with_pr = ReviewInfo {
            pr: Some(IssuePr {
                state: Some(PrState::Open),
                ci: Some(PrCi::Failing),
                ..IssuePr::new(2137, Some(url.into()))
            }),
            ..info()
        };
        h.update(|v, cx| v.show(with_pr.clone(), Some(row(ReviewState::Serving)), cx));
        h.draw();
        assert!(h.cx.debug_bounds("review-pr-row").is_some());
        assert!(h.cx.debug_bounds("pr-state").is_some());
        assert!(h.cx.debug_bounds("pr-ci").is_some());

        // It sits under the summary, above the worktree.
        let summary = h.cx.debug_bounds("review-title").unwrap();
        let pr_row = h.cx.debug_bounds("review-pr-row").unwrap();
        let worktree = h.cx.debug_bounds("review-worktree").unwrap();
        assert!(summary.bottom() <= pr_row.top() && pr_row.bottom() <= worktree.top());

        h.click("review-pr");
        assert_eq!(h.ops.opened.lock().unwrap().as_slice(), [url.to_string()]);

        // The row keeps up with the board while the panel stays open.
        let fresh = ReviewRow {
            pr: Some(IssuePr {
                state: Some(PrState::Merged),
                ..IssuePr::new(2137, Some(url.into()))
            }),
            ..row(ReviewState::Serving)
        };
        h.update(|v, cx| v.update_row(Some(fresh), cx));
        let pr = h.view.read_with(&h.cx, |v, _| v.model().and_then(|m| m.pr.clone()));
        assert_eq!(pr.and_then(|p| p.state), Some(PrState::Merged));

        // A task with no PR has no row.
        h.update(|v, cx| v.show(info(), Some(row(ReviewState::Serving)), cx));
        h.draw();
        assert!(h.cx.debug_bounds("review-pr-row").is_none());
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
        h.click("review-switch-browser");
        h.click("review-shell-toggle");
        h.focus_panel();
        h.key("q");
        assert!(h.events().is_empty(), "no Close or decision: {:?}", h.events());
        assert!(h.ops().is_empty(), "no view switch reached the daemon");
        assert!(h.ops.opened.lock().unwrap().is_empty());
        assert!(h.ops.shell_ops.lock().unwrap().is_empty(), "no shell either");
        assert_eq!(h.status_line().as_deref(), Some(shelbi_app::review::MERGE_IN_PROGRESS));

        // Back still leaves: the merge doesn't hold the window here.
        h.click("review-back");
        h.key("escape");
        assert_eq!(h.events(), vec![ReviewEvent::Back, ReviewEvent::Back]);
        h.events.borrow_mut().clear();

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
    fn edit_in_an_app_toasts_and_a_failed_launch_replaces_it(cx: &mut TestAppContext) {
        let mut h = open(cx);
        let mut info = info();
        info.editor_name = "Zed".into();
        info.apps = shelbi_state::AppTools { editor: true, ..Default::default() };
        h.update(|v, cx| v.show(info, None, cx));
        h.update(|v, cx| v.apply(PanelEffect::OpenApp(ReviewRole::Editor), cx));
        // The fake ops can't open apps: the launch fails with that reason.
        assert_eq!(
            h.events(),
            vec![
                ReviewEvent::Toast(Some(Toast::launching_editor("Zed"))),
                ReviewEvent::Toast(Some(Toast::editor_launch_failed("Zed", "apps can't be opened here"))),
            ]
        );
        assert_eq!(h.status_line(), None, "the toast says it, not the status line");
        assert!(h.ops().is_empty(), "no editor pane was asked for");
    }

    #[gpui::test]
    fn a_pane_editor_takes_the_toast_down(cx: &mut TestAppContext) {
        let mut h = open(cx);
        h.update(|v, cx| v.show(info(), None, cx));
        h.update(|v, cx| v.apply(PanelEffect::ShowEditor, cx));
        assert_eq!(h.events(), vec![ReviewEvent::Toast(None)]);
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
        assert!(!h.view.read_with(&h.cx, |v, _| v.panel().is_some_and(ReviewPanelState::is_merging)));
        assert!(h.events().is_empty(), "the window is already moving on");
    }
}
