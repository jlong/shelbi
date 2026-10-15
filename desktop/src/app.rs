//! The root view of a project window: the sidebar | main area split.
//!
//! One window per project ([`Shell`] keeps the registry). A window owns its
//! project's `shelbi_app::nav::ClientState`, the source of truth for what the
//! main area shows, where keyboard focus sits, the sidebar selection and the
//! sidebar width; its sidebar model, fed by the project's
//! [`ProjectFeed`](crate::refresh::ProjectFeed); and its persisted
//! [`WindowState`].
//!
//! The app's first window runs bootstrap (`crate::platform`: the login
//! environment, finding the CLI, starting the daemon, the version check) and
//! shows its progress and failure screens until it is `Ready`; the `Ready`
//! result is kept in [`Shell`], so every later window (a project switch) skips
//! straight to the shell. The shell's first frame paints the target project's
//! chrome from memory (its prefetched config skeleton, else the bare skeleton)
//! and never waits on disk, git, SSH or the daemon: the feed's first snapshot
//! replaces it, and bringing up the project's orchestrator runs on the
//! background executor through the CLI bootstrap found.
//!
//! The Orchestrator nav row (the view the window opens on) routes the main
//! area to the Chat view ([`ChatView`], `dt-chat-view`): the orchestrator's
//! session in a full-height terminal, attached while shown and detached while
//! another view is. While its terminal holds focus, the window takes the
//! global chords (the focus moves and the palette chords, Ctrl+Space and
//! Cmd+K included) in the capture phase, so they never reach the session.
//!
//! A workspace row routes the main area to the Workspace view
//! ([`WorkspaceView`], `dt-workspace-view`): its agent's terminal, full
//! height, with the user's shell beside it on demand. Its panel (status,
//! task, branch, worktree, machine, the view switch and the Shell toggle)
//! takes the sidebar's column with the same push as a review's
//! (`dt-workspace-sidebar`); its Back returns to the view the workspace was
//! opened over. Going from a workspace to a review, or back, passes through
//! the sidebar. Like the chat, its terminals give the window the global
//! chords.
//!
//! A review row (or the palette's `review:<id>`) opens the Review view
//! ([`ReviewView`], `dt-review-view`): the native review panel takes the
//! sidebar's column (`dt-review-replaces-sidebar`, as in the TUI) and the
//! main area shows the review agent's chat, or its diff or editor. Going in
//! and coming back out push between the two in that column
//! ([`column_swap`]: a slide and fade, a crossfade under Reduce Motion). A
//! queued review asks which free slot to load it onto first; the daemon
//! loads it, and its `ReviewOpened` opens the view. Leaving the review
//! returns to the view the main area showed before it, with the sidebar as
//! it was (selection, scroll, collapsed) and keys on it.
//!
//! The Issues nav row routes the main area to the native board
//! ([`IssuesView`], `dt-issues-board`), built for the window's project the
//! first time it is shown. A project switch opens (or focuses) the target's
//! own window, so every project gets its own board.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, App, Bounds, Context, CursorStyle, Entity, FocusHandle,
    Global, KeyDownEvent, MouseButton, MouseMoveEvent, PromptLevel, ScrollHandle, SharedString,
    Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions,
};
use shelbi_app::command::{EditItem, ProjectItem};
use shelbi_app::nav::{ClientState, Focus, View};
use shelbi_app::overlay::error_log::Viewer;
use shelbi_app::overlay::palette::ProjectEntry as PaletteProject;
use shelbi_app::overlay::review_confirm::Slot;
use shelbi_app::review::{LoadStep, ReviewInfo, ReviewLoader};
use shelbi_app::view::{SidebarModel, SidebarTarget};
use shelbi_app::{CommandKind, CommandRegistry, EditTarget, Mutation};
use shelbi_core::ConfigMode;
use shelbi_orchestrator::project_create::AddProjectError;
use shelbi_proto::control::DetachSelector;
use shelbi_state::DesktopNotifications;

use crate::platform::version::APP_VERSION;
use crate::platform::{
    relaunch, Bootstrap, BootstrapEvent, BootstrapState, Failure, Ready, SystemPlatform,
};
use crate::views::bootstrap::{self as boot_view, ScreenAction};
use crate::views::chat::{ChatDeps, ChatEvent, ChatView};
use crate::views::update::{self as update_view, UpdateState};
use shelbi_state::keymap::{
    format_chord, DisplayStyle, GlobalAction, Key, Keymaps, SidebarAction,
};

use crate::assets::icons;
use crate::commands::{self, CommandHost};
use crate::keys::{chord_from_keystroke, overlay_chord, paste_chords};
use crate::menus;
use crate::notify;
use crate::projects::{self, Catalog};
use crate::refresh::{ControlWatcher, DaemonLink, FeedEvent, ProjectFeed, SessionNoticeKind};
use crate::theme::style::{self, IconButtonSize};
use crate::theme::{self, Hue, Theme};
use crate::views::activity::{ActivityEvent, ActivityView};
use crate::views::column_swap::{self, ColumnSwap, Layer, Motion, Occupant};
use crate::views::issues::{IssuesEvent, IssuesView};
use crate::views::machines::{MachinesEvent, MachinesView};
use crate::views::review::{ReviewDeps, ReviewEvent, ReviewOps, ReviewView};
use crate::views::overlay::{self, ActiveOverlay, OverlayEvent, OverlayHost};
use crate::views::workspace::{WorkspaceDeps, WorkspaceEvent, WorkspaceView};
use crate::views::settings::{SettingsEvent, SettingsView};
use crate::views::sidebar::{self, SidebarEvent, SidebarHost, SidebarProps};
use crate::window_state::{Frame, WindowState};

actions!(
    shelbi_window,
    [
        /// Show or hide the sidebar (⌃⌘S, the macOS "Show Sidebar" shortcut).
        ToggleSidebar,
        /// Open the settings view (⌘,).
        OpenSettings,
        /// Close this project's window (⌘W). The project keeps running.
        CloseWindow,
    ]
);

const DEFAULT_WIDTH: f32 = 1200.0;
const DEFAULT_HEIGHT: f32 = 800.0;
/// Shown while the project's orchestrator comes up.
const STARTUP_STATUS: &str = "Starting the orchestrator…";

/// App-wide shell state: one window per project, the attached-client id, the
/// control-socket watcher feeds register with, and bootstrap's result once a
/// window has reached it.
#[derive(Default)]
pub struct Shell {
    windows: HashMap<String, WindowHandle<ShelbiWindow>>,
    pub client_id: String,
    pub control: Option<ControlWatcher>,
    ready: Option<Ready>,
}

impl Global for Shell {}

impl Shell {
    pub fn new(client_id: String, control: Option<ControlWatcher>) -> Self {
        Shell {
            windows: HashMap::new(),
            client_id,
            control,
            ready: None,
        }
    }
}

/// Open `project`'s window, or focus it if it already has one.
pub fn open_or_focus(project: &str, cx: &mut App) {
    let existing = cx
        .try_global::<Shell>()
        .and_then(|s| s.windows.get(project).copied());
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
    }
    open_window(project, cx);
}

/// A clicked notification: bring its project's window up and open the item.
pub fn open_notice_tag(tag: &str, cx: &mut App) {
    let Some((project, target)) = notify::parse_tag(tag) else {
        return;
    };
    open_or_focus(&project, cx);
    let handle = cx
        .try_global::<Shell>()
        .and_then(|s| s.windows.get(&project).copied());
    if let Some(handle) = handle {
        let _ = handle.update(cx, |this, window, cx| this.open_notice(target, window, cx));
    }
}

fn open_window(project: &str, cx: &mut App) {
    // A small local JSON read, before the window exists; it decides the frame.
    let state = WindowState::load(project);
    let bounds = match state.frame {
        Some(f) if f.width >= 400.0 && f.height >= 300.0 => Bounds {
            origin: gpui::point(px(f.x), px(f.y)),
            size: gpui::size(px(f.width), px(f.height)),
        },
        _ => {
            let mut b =
                Bounds::centered(None, gpui::size(px(DEFAULT_WIDTH), px(DEFAULT_HEIGHT)), cx);
            // Cascade a new window off the ones already open, so it doesn't
            // land exactly on top of them.
            let offset = px(28.0 * cx.windows().len() as f32);
            b.origin.x += offset;
            b.origin.y += offset;
            b
        }
    };
    let project_name = project.to_string();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(project.to_string().into()),
                // The sidebar runs under a transparent title bar, the usual
                // shape for a macOS sidebar app.
                appears_transparent: true,
                traffic_light_position: Some(gpui::point(px(12.0), px(12.0))),
            }),
            app_id: Some("co.32pixels.shelbi".into()),
            ..Default::default()
        },
        move |window, cx| cx.new(|cx| ShelbiWindow::new(&project_name, state, window, cx)),
    );
    match opened {
        Ok(handle) => {
            if !cx.has_global::<Shell>() {
                cx.set_global(Shell::default());
            }
            cx.global_mut::<Shell>()
                .windows
                .insert(project.to_string(), handle);
        }
        Err(e) => tracing::error!(project, error = %e, "could not open the project window"),
    }
}

/// Where the window is in reaching Shelbi.
enum Boot {
    /// Bootstrap is running; the state is one of the in-flight ones.
    InFlight(BootstrapState),
    Failed(Failure),
    Ready(Box<Ready>),
}

/// What the main area shows besides the routed [`View`].
enum Route {
    /// The `ClientState`'s view.
    View,
    /// The settings view (transient: not remembered as a view).
    Settings(Entity<SettingsView>),
    /// The Review view (transient too: the `ClientState`'s view stays the
    /// one the review was opened over, so leaving it returns there).
    Review,
}

/// A command dispatch asked the window for, collected first and then carried
/// out with the window and its context in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCall {
    ShowView(View),
    IssueCommand(CommandKind),
    SwitchProject(String),
    OpenSwitcher,
    Detach,
    DetachOthers,
    QuitProject,
    QuitShelbi,
    ToggleZen,
    OpenErrorLog,
    OpenAddProject,
    LoadReview(String),
    ApproveReview(String),
    AskRejectReason(String),
    RejectReview(String, String),
    OpenEditor(EditTarget),
}

impl CommandHost for Vec<HostCall> {
    fn show_view(&mut self, view: &View) {
        self.push(HostCall::ShowView(view.clone()));
    }
    fn issue_command(&mut self, kind: &CommandKind) {
        self.push(HostCall::IssueCommand(kind.clone()));
    }
    fn switch_project(&mut self, project: &str) {
        self.push(HostCall::SwitchProject(project.to_string()));
    }
    fn open_project_switcher(&mut self) {
        self.push(HostCall::OpenSwitcher);
    }
    fn detach(&mut self) {
        self.push(HostCall::Detach);
    }
    fn detach_other_clients(&mut self) {
        self.push(HostCall::DetachOthers);
    }
    fn quit_project(&mut self) {
        self.push(HostCall::QuitProject);
    }
    fn quit_shelbi(&mut self) {
        self.push(HostCall::QuitShelbi);
    }
    fn toggle_zen(&mut self) {
        self.push(HostCall::ToggleZen);
    }
    fn open_error_log(&mut self) {
        self.push(HostCall::OpenErrorLog);
    }
    fn open_add_project(&mut self) {
        self.push(HostCall::OpenAddProject);
    }
    fn load_review(&mut self, task_id: &str) {
        self.push(HostCall::LoadReview(task_id.to_string()));
    }
    fn approve_review(&mut self, id: &str) {
        self.push(HostCall::ApproveReview(id.to_string()));
    }
    fn ask_reject_reason(&mut self, id: &str) {
        self.push(HostCall::AskRejectReason(id.to_string()));
    }
    fn reject_review(&mut self, id: &str, reason: &str) {
        self.push(HostCall::RejectReview(id.to_string(), reason.to_string()));
    }
    fn open_editor(&mut self, target: &EditTarget) {
        self.push(HostCall::OpenEditor(target.clone()));
    }
}

/// What the palette's entries are built from besides the sidebar model: reads
/// of other projects, the edit targets on disk, and whether another client is
/// attached. Kept from the last read so the palette opens at once; re-read
/// (off the UI thread) each time it opens.
#[derive(Debug, Clone, Default)]
pub struct PaletteInputs {
    pub other_projects: Vec<ProjectItem>,
    pub edit_targets: Vec<EditItem>,
    pub projects: Vec<PaletteProject>,
    pub others_attached: bool,
}

impl PaletteInputs {
    /// Read them (blocking: small local files and one control-socket call).
    fn read(project: &str, control: Option<crate::platform::control::ControlHandle>, client_id: &str) -> Self {
        let summaries = shelbi_state::list_projects().unwrap_or_default();
        let open: std::collections::HashSet<String> = shelbi_state::list_open_projects()
            .unwrap_or_default()
            .into_iter()
            .collect();
        let others_attached = control
            .and_then(|c| c.connect().ok())
            .and_then(|mut c| c.list_clients().ok())
            .is_some_and(|clients| clients.iter().any(|c| c.client_id != client_id));
        PaletteInputs {
            other_projects: summaries
                .iter()
                .filter(|p| p.name != project)
                .map(|p| ProjectItem {
                    slug: p.name.clone(),
                    label: p.display_label().to_string(),
                })
                .collect(),
            edit_targets: shelbi_app::command_model::edit_targets(project),
            projects: shelbi_app::command_model::projects_column_from(
                project,
                summaries
                    .iter()
                    .map(|p| (p.name.clone(), p.display_label().to_string()))
                    .collect(),
                &open,
            ),
            others_attached,
        }
    }
}

/// A project window's root view.
pub struct ShelbiWindow {
    project: String,
    boot: Boot,
    /// Drains bootstrap's channel into this view; replaced on retry.
    _bootstrap: Option<Task<()>>,
    /// Re-resolves the theme when the system appearance changes.
    _appearance: Subscription,
    client: ClientState,
    model: SidebarModel,
    link: Option<DaemonLink>,
    collapsed: bool,
    dragging: bool,
    switcher_open: bool,
    route: Route,
    /// The Issues board for this window's project, created the first time it
    /// is shown.
    issues: Option<Entity<IssuesView>>,
    /// The orchestrator chat, created the first time it is shown.
    chat: Option<Entity<ChatView>>,
    /// The Activity feed and the Machines view, likewise created on first show.
    activity: Option<Entity<ActivityView>>,
    machines: Option<Entity<MachinesView>>,
    /// The open overlay (palette or dialog), mirrored in `client.overlay()`.
    overlay: Option<ActiveOverlay>,
    palette_inputs: PaletteInputs,
    /// The repo path an add-project detection is running for.
    detecting: Option<String>,
    /// Bumped on each transient status, so a flash only clears its own.
    flash_generation: u64,
    /// The Workspace view, created the first time a workspace is shown.
    workspace: Option<Entity<WorkspaceView>>,
    /// The view the shown workspace was opened over: its panel's Back
    /// returns there (the default view when it was the first one shown).
    workspace_return: Option<View>,
    /// The Review view, created the first time a review is opened.
    review: Option<Entity<ReviewView>>,
    /// The leading column's occupant (the sidebar, or the review or
    /// workspace panel in its place) and the push between them.
    column: ColumnSwap,
    /// The column's width while a panel (a review's or a workspace's) holds
    /// it with the sidebar collapsed, once the user drags it (else the
    /// default width).
    collapsed_panel_cols: Option<u16>,
    /// The sidebar's scroll, kept while a panel stands in for it.
    sidebar_scroll: ScrollHandle,
    /// The column transition's clock and the system's Reduce Motion setting
    /// (injectable for tests).
    clock: Rc<dyn Fn() -> Instant>,
    reduce_motion: Rc<dyn Fn() -> bool>,
    /// Opening a review: resolve, ask for a slot, load, open.
    review_loader: ReviewLoader,
    /// The blocking review work (resolve, load, the content sessions).
    review_ops: Arc<dyn ReviewOps>,
    /// The "Not implemented yet" (or failure) banner.
    banner: Option<SharedString>,
    /// A transient status note (the orchestrator coming up, a failure).
    status: Option<SharedString>,
    focus_handle: FocusHandle,
    keymaps: Keymaps,
    palette_chord: String,
    /// The window is the key window of the frontmost app: the menu bar shows
    /// its commands, and its notices are suppressed for what it shows.
    active: bool,
    /// Turns this project's refreshes into system notifications.
    notifier: notify::Tracker,
    /// The user's notification switches (`desktop.notifications`).
    notify_prefs: DesktopNotifications,
    window_state: WindowState,
    /// Bumped on every state change worth saving; a debounced save only
    /// writes if no newer change followed it.
    save_generation: u64,
    _feed: Option<ProjectFeed>,
    _feed_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    /// The shell has been drawn at least once (gpui has a frame to answer
    /// action-availability questions from).
    drawn_once: bool,
    /// Test seam: when set, [`Self::run`] records the host calls a command
    /// dispatches here instead of performing them, and the palette's inputs
    /// stay the ones the test set (no disk read).
    #[cfg(any(test, feature = "visual-check"))]
    tap: Option<std::rc::Rc<std::cell::RefCell<Vec<HostCall>>>>,
    /// Test seam: the Review view's deps (fake sessions and daemon ops).
    #[cfg(any(test, feature = "visual-check"))]
    review_deps: Option<ReviewDeps>,
    /// Test seam: the Workspace view's deps, likewise.
    #[cfg(any(test, feature = "visual-check"))]
    workspace_deps: Option<WorkspaceDeps>,
    /// `--bench-push`'s frame probe: each frame's render start, paint end
    /// and column position.
    #[cfg(feature = "visual-check")]
    probe: Option<Rc<std::cell::RefCell<crate::bench_push::Probe>>>,
}

impl ShelbiWindow {
    pub fn new(
        project: &str,
        window_state: WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shared_ready = cx.try_global::<Shell>().and_then(|s| s.ready.clone());
        let mut this = Self::build(project, window_state, window, cx);
        // The feed and keymaps only read local state: start them now, so the
        // shell has data the moment bootstrap is done.
        this.start_feed(window, cx);
        this.load_keymaps(cx);
        match shared_ready {
            Some(ready) => this.on_ready(ready, cx),
            None => this.start_bootstrap(cx),
        }
        this._subscriptions.push(cx.observe_window_bounds(window, |this, window, cx| {
            let b = window.window_bounds().get_bounds();
            this.window_state.frame = Some(Frame {
                x: b.origin.x.as_f32(),
                y: b.origin.y.as_f32(),
                width: b.size.width.as_f32(),
                height: b.size.height.as_f32(),
            });
            this.schedule_save(cx);
        }));
        this._subscriptions.push(cx.observe_window_activation(window, |this, window, cx| {
            this.active = window.is_window_active();
            if this.active {
                this.on_activated(cx);
            }
        }));
        this.load_notify_prefs(cx);
        window.set_window_title(&this.model.project_label);
        this
    }

    /// The window's state with nothing started: no feed, no bootstrap, the
    /// built-in keymaps. [`Self::new`] starts the live pieces on top; the
    /// smoke test drives a window built here.
    fn build(
        project: &str,
        window_state: WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut client = ClientState::new(project);
        client.set_sidebar_width(window_state.sidebar_width);
        // Land on the orchestrator chat, focused, as the TUI does.
        client.set_view(View::Session("orch".into()));
        client.focus_main();

        let model = cx
            .try_global::<Catalog>()
            .map(|c| c.first_frame_sidebar(project))
            .unwrap_or_else(|| SidebarModel::skeleton(project));
        // The built-in keymaps until the user's `keys.yaml` is read (below).
        let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);

        let mut this = ShelbiWindow {
            project: project.to_string(),
            boot: Boot::InFlight(BootstrapState::ResolvingEnv),
            _bootstrap: None,
            _appearance: theme::follow_system(window, cx),
            palette_chord: palette_chord(&keymaps),
            client,
            model,
            link: None,
            collapsed: window_state.sidebar_collapsed,
            dragging: false,
            switcher_open: false,
            route: Route::View,
            issues: None,
            chat: None,
            activity: None,
            machines: None,
            overlay: None,
            palette_inputs: PaletteInputs::default(),
            detecting: None,
            flash_generation: 0,
            workspace: None,
            workspace_return: None,
            review: None,
            column: ColumnSwap::new(),
            collapsed_panel_cols: None,
            sidebar_scroll: ScrollHandle::new(),
            clock: Rc::new(Instant::now),
            reduce_motion: Rc::new(crate::platform::motion::reduce_motion),
            review_loader: ReviewLoader::default(),
            review_ops: Arc::new(crate::views::review::LiveOps),
            banner: None,
            status: None,
            focus_handle,
            keymaps,
            active: false,
            notifier: notify::Tracker::new(project),
            notify_prefs: DesktopNotifications::default(),
            window_state,
            save_generation: 0,
            _feed: None,
            _feed_task: None,
            _subscriptions: Vec::new(),
            drawn_once: false,
            #[cfg(any(test, feature = "visual-check"))]
            tap: None,
            #[cfg(any(test, feature = "visual-check"))]
            review_deps: None,
            #[cfg(any(test, feature = "visual-check"))]
            workspace_deps: None,
            #[cfg(feature = "visual-check")]
            probe: None,
        };
        this.sync_selection();
        this
    }

    /// The window came to the front: the menu bar is now its, and the
    /// settings it reads (`keys.yaml` included) may have changed while it was
    /// behind.
    fn on_activated(&mut self, cx: &mut Context<Self>) {
        self.sync_menus(cx);
        self.load_keymaps(cx);
        self.load_notify_prefs(cx);
        self.refresh_palette_inputs(cx);
    }

    /// Rebuild the menu bar from this window's state, when it is the active
    /// window. Cheap when nothing changed (`menus::install` compares).
    fn sync_menus(&mut self, cx: &mut Context<Self>) {
        if !self.active {
            return;
        }
        let tree = menus::tree::build(
            &self.command_model(),
            &self.keymaps,
            menus::tree::WindowFacts {
                sidebar_collapsed: self.collapsed,
            },
        );
        menus::install(tree, cx);
    }

    /// The registry's input for this window: the live sidebar model plus the
    /// last [`PaletteInputs`].
    fn command_model(&self) -> shelbi_app::command::CommandModel {
        let inputs = &self.palette_inputs;
        shelbi_app::command_model::command_model_from(
            &self.project,
            &self.model,
            inputs.others_attached,
            inputs.other_projects.clone(),
            inputs.edit_targets.clone(),
        )
    }

    /// Read the notification switches off the UI thread.
    fn load_notify_prefs(&mut self, cx: &mut Context<Self>) {
        let read = cx
            .background_executor()
            .spawn(async { shelbi_state::load_user_config() });
        cx.spawn(async move |this, cx| {
            if let Ok(cfg) = read.await {
                this.update(cx, |this, _| this.notify_prefs = cfg.desktop.notifications)
                    .ok();
            }
        })
        .detach();
    }

    /// Re-read the palette's (and the menus') inputs off the UI thread: other
    /// projects, edit targets, whether another client is attached.
    fn refresh_palette_inputs(&mut self, cx: &mut Context<Self>) {
        #[cfg(any(test, feature = "visual-check"))]
        if self.tap.is_some() {
            return;
        }
        let project = self.project.clone();
        let control = self.ready().map(|r| r.control.clone());
        let client_id = cx
            .try_global::<Shell>()
            .map(|s| s.client_id.clone())
            .unwrap_or_default();
        let read = cx
            .background_executor()
            .spawn(async move { PaletteInputs::read(&project, control, &client_id) });
        cx.spawn(async move |this, cx| {
            let inputs = read.await;
            this.update(cx, |this, cx| {
                this.palette_inputs = inputs;
                let entries = this.palette_entries();
                let projects = this.palette_inputs.projects.clone();
                if let Some(ActiveOverlay::Palette { palette, .. }) = &mut this.overlay {
                    palette.set_entries(entries);
                    palette.set_projects(projects);
                }
                this.sync_menus(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Post (or clear) the notifications a refresh produced.
    fn apply_notices(&mut self, changes: notify::Changes, cx: &mut Context<Self>) {
        for tag in changes.cleared {
            cx.dismiss_system_notification(&tag);
        }
        if changes.notices.is_empty() {
            return;
        }
        let screen = notify::Screen {
            frontmost: self.active,
            showing: self.showing(),
        };
        let now = std::time::Instant::now();
        for notice in changes.notices {
            let tag = notice.tag();
            let decision = notify::decide(
                &notice,
                &self.notify_prefs,
                &screen,
                self.notifier.last_posted(&tag),
                now,
            );
            tracing::debug!(%tag, ?decision, "notification");
            if decision == notify::Decision::Post {
                cx.show_system_notification(gpui::SystemNotification {
                    tag: tag.clone().into(),
                    title: notice.title.into(),
                    body: notice.body.into(),
                    actions: Vec::new(),
                });
                self.notifier.record_posted(tag, now);
            }
        }
    }

    /// What the main area shows, as notices see it.
    fn showing(&self) -> notify::Showing {
        if !matches!(self.route, Route::View) {
            return notify::Showing::Other;
        }
        match self.client.view() {
            view if is_chat(view) => notify::Showing::Orchestrator,
            View::Session(name) => notify::Showing::Workspace(name.clone()),
            View::Issues => notify::Showing::Board,
            View::Activity | View::Machines => notify::Showing::Other,
        }
    }

    /// Open what a clicked notification is about.
    pub fn open_notice(&mut self, target: notify::Target, window: &mut Window, cx: &mut Context<Self>) {
        match target {
            notify::Target::Workspace(workspace) => {
                // Select its sidebar row too, so the window says where it went.
                if let Some(ordinal) = self.model.rows().iter().filter_map(|r| r.target()).position(
                    |t| matches!(&t, SidebarTarget::Workspace(w) if *w == workspace),
                ) {
                    let count = self.selectable_count();
                    self.client.select(ordinal, count);
                }
                self.client.focus_main();
                self.run(CommandKind::FocusWorkspace { workspace }, window, cx);
            }
            notify::Target::Review(id) => self.open_task(&id, window, cx),
            notify::Target::Orchestrator => {
                self.run(CommandKind::ShowView(View::Session("orch".into())), window, cx)
            }
        }
    }

    /// Spawn the project's feed and pump its events into this view.
    fn start_feed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (feed, mut events) = ProjectFeed::spawn(&self.project);
        if let Some(control) = cx.try_global::<Shell>().and_then(|s| s.control.clone()) {
            control.watch(&self.project, &feed);
        }
        self._feed = Some(feed);
        self._feed_task = Some(cx.spawn_in(window, async move |this, cx| {
            while let Some(event) = events.next().await {
                if this
                    .update_in(cx, |this, window, cx| this.apply_feed(event, window, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    /// Fold a feed event into the window, repainting (and re-deriving the
    /// menus) only when it changed something: the feed re-reads the project
    /// on a timer (every 5 s while the daemon pushes, 750 ms while polling)
    /// and most reads find nothing new, so an idle window must not repaint
    /// for them (`dt-terminal-perf-multiclient`). The notifier diffs against
    /// its last model, so an identical snapshot has nothing to tell it.
    fn apply_feed(&mut self, event: FeedEvent, window: &mut Window, cx: &mut Context<Self>) {
        let changed = match event {
            FeedEvent::Snapshot(snapshot) => match snapshot.sidebar {
                Some(model) if model != self.model => {
                    let changes = self.notifier.observe_sidebar(&model);
                    self.model = model;
                    self.apply_notices(changes, cx);
                    self.sync_selection();
                    // An open palette follows the live state, query kept.
                    let entries = self.palette_entries();
                    if let Some(ActiveOverlay::Palette { palette, .. }) = &mut self.overlay {
                        palette.set_entries(entries);
                    }
                    self.update_workspace_row(cx);
                    self.update_review_row(window, cx);
                    self.update_chat_supervisor_status(cx);
                    true
                }
                _ => false,
            },
            FeedEvent::Layout(event) => {
                self.apply_layout(event, window, cx);
                true
            }
            FeedEvent::Link(link) => self.link.replace(link) != Some(link),
            FeedEvent::Session(notice) => match notice.kind {
                // The chat and workspace views draw their own sessions' titles;
                // the window only turns the orchestrator's into notifications.
                SessionNoticeKind::Title(title) => {
                    if notice.session == "orch" {
                        let changes = self
                            .notifier
                            .observe_orchestrator(&title, &self.model.project_label);
                        self.apply_notices(changes, cx);
                    }
                    false
                }
                SessionNoticeKind::Exited => false,
            },
        };
        if changed {
            self.sync_menus(cx);
            cx.notify();
        }
    }

    /// Read the user's merged keymaps (`keys.yaml`) off the UI thread.
    fn load_keymaps(&mut self, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let read = cx
            .background_executor()
            .spawn(async move { shelbi_state::keymap::load_keymaps(Some(&project)).0 });
        cx.spawn(async move |this, cx| {
            let keymaps = read.await;
            this.update(cx, |this, cx| {
                this.palette_chord = palette_chord(&keymaps);
                this.keymaps = keymaps;
                if let Some(ws) = this.workspace.clone() {
                    let chord = this.palette_chord.clone();
                    ws.update(cx, |ws, cx| ws.set_palette_chord(&chord, cx));
                }
                this.sync_menus(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run bootstrap (again) and feed its events into this view.
    fn start_bootstrap(&mut self, cx: &mut Context<Self>) {
        self.boot = Boot::InFlight(BootstrapState::ResolvingEnv);
        let client_id = cx
            .try_global::<Shell>()
            .map(|s| s.client_id.clone())
            .unwrap_or_else(shelbi_client::fresh_client_id);
        let events =
            Bootstrap::spawn_as(SystemPlatform::default(), APP_VERSION.to_string(), client_id);
        self._bootstrap = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |this, cx| this.apply_boot(event, cx)).is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn apply_boot(&mut self, event: BootstrapEvent, cx: &mut Context<Self>) {
        match event {
            BootstrapEvent::State(BootstrapState::Ready(ready)) => {
                tracing::info!(
                    cli = %ready.cli.path.display(),
                    cli_version = ?ready.cli_version,
                    daemon = ready.control.daemon_version(),
                    "bootstrap ready"
                );
                // Later windows (project switches) reuse it and skip bootstrap.
                if cx.has_global::<Shell>() {
                    cx.global_mut::<Shell>().ready = Some(ready.clone());
                }
                self.on_ready(ready, cx);
            }
            BootstrapEvent::State(BootstrapState::Failed(failure)) => {
                tracing::warn!(?failure, "bootstrap failed");
                self.boot = Boot::Failed(failure);
            }
            BootstrapEvent::State(state) => self.boot = Boot::InFlight(state),
            // Every window reads the shared handle's relaunch state: repaint
            // them all so each shows the relaunch bar.
            BootstrapEvent::RelaunchRequired(notice) => {
                tracing::info!(reason = %notice.reason, "relaunch required");
                cx.refresh_windows();
            }
            // Leave, keeping every session and agent running.
            BootstrapEvent::Detach(reason) => {
                tracing::info!(%reason, "detached by the daemon");
                cx.quit();
            }
        }
        cx.notify();
    }

    /// Bootstrap reached Shelbi: show the shell and open the project.
    fn on_ready(&mut self, ready: Ready, cx: &mut Context<Self>) {
        self.boot = Boot::Ready(Box::new(ready));
        self.open_project(cx);
        self.refresh_palette_inputs(cx);
    }

    fn ready(&self) -> Option<&Ready> {
        match &self.boot {
            Boot::Ready(ready) => Some(ready),
            Boot::InFlight(_) | Boot::Failed(_) => None,
        }
    }

    fn relaunch_required(&self) -> bool {
        self.ready()
            .is_some_and(|r| r.control.relaunch_required().is_some())
    }

    fn screen_action(&mut self, action: ScreenAction, cx: &mut Context<Self>) {
        match action {
            ScreenAction::Retry => self.start_bootstrap(cx),
            ScreenAction::Relaunch => match relaunch::relaunch() {
                Ok(()) => cx.quit(),
                Err(e) => {
                    tracing::error!("relaunch failed: {e}");
                    self.banner = Some(
                        format!("Couldn't relaunch Shelbi: {e}. Quit and open it again.").into(),
                    );
                    cx.notify();
                }
            },
        }
    }

    /// Bring the project's orchestrator up (and record the project open) off
    /// the UI thread, through the CLI bootstrap found.
    fn open_project(&mut self, cx: &mut Context<Self>) {
        let Some(ready) = self.ready() else {
            return;
        };
        let cli = ready.cli.path.clone();
        let env = ready.env.clone();
        let project = self.project.clone();
        self.status = Some(STARTUP_STATUS.into());
        let open = cx
            .background_executor()
            .spawn(async move { projects::open_project(&project, &cli, &env) });
        cx.spawn(async move |this, cx| {
            let result = open.await;
            this.update(cx, |this, cx| {
                this.status = match result {
                    Ok(()) => None,
                    Err(message) => Some(message.into()),
                };
                // The orchestrator is up now: re-attach a chat that isn't
                // live, even mid-connect (that attach may have started before
                // the session existed and give up on it), as the TUI does
                // after its startup.
                if let Some(chat) = &this.chat {
                    chat.update(cx, |chat, cx| chat.reconnect(cx));
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn selectable_count(&self) -> usize {
        self.model.rows().iter().filter(|r| r.is_selectable()).count()
    }

    /// Keep the selection on a row that exists, and on the current view's nav
    /// row when the selection is still at its starting place.
    fn sync_selection(&mut self) {
        let count = self.selectable_count();
        self.client.clamp_selection(count);
    }

    fn target_at(&self, ordinal: usize) -> Option<SidebarTarget> {
        self.model
            .rows()
            .iter()
            .filter_map(|r| r.target())
            .nth(ordinal)
    }

    /// Open the selected sidebar row.
    fn activate_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.target_at(self.client.sidebar_selection()) else {
            return;
        };
        match target {
            SidebarTarget::View(view) => self.run(CommandKind::ShowView(view), window, cx),
            SidebarTarget::Workspace(workspace) => {
                self.client.focus_main();
                self.run(CommandKind::FocusWorkspace { workspace }, window, cx)
            }
            SidebarTarget::Review(task_id) => {
                self.client.focus_main();
                self.run(CommandKind::LoadReview { task_id }, window, cx)
            }
            SidebarTarget::Machine(name) => self.toggle_machine(name, cx),
        }
    }

    /// Collapse or expand a machine group: at once in the model, persisted
    /// off the UI thread (to `state.json`, shared with the TUI).
    fn toggle_machine(&mut self, name: String, cx: &mut Context<Self>) {
        if !self.model.collapsed_machines.remove(&name) {
            self.model.collapsed_machines.insert(name.clone());
        }
        self.sync_selection();
        let write = cx.background_executor().spawn(async move {
            shelbi_state::toggle_sidebar_machine_collapsed(&name).map_err(|e| e.to_string())
        });
        cx.spawn(async move |this, cx| {
            if let Err(e) = write.await {
                this.update(cx, |this, cx| {
                    this.banner = Some(format!("Couldn't save the collapse: {e}").into());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    /// Run a registry command. While a relaunch is required (the CLI was
    /// upgraded under the app), commands that reach the daemon are refused.
    fn run(&mut self, kind: CommandKind, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(any(test, feature = "visual-check"))]
        if let Some(tap) = &self.tap {
            commands::dispatch_command(&kind, &mut *tap.borrow_mut());
            return;
        }
        if self.relaunch_required() && commands::sends_to_daemon(&kind) {
            self.banner = Some("Shelbi was updated: relaunch before making changes.".into());
            cx.notify();
            return;
        }
        // Until bootstrap is ready no daemon launcher is installed, and
        // `shelbi_state::ensure_daemon_running` would fall back to
        // `current_exe()`: this app, not the CLI. Hold daemon-bound commands
        // until then.
        if self.ready().is_none() && commands::sends_to_daemon(&kind) {
            self.banner = Some("Still connecting to Shelbi: try again in a moment.".into());
            cx.notify();
            return;
        }
        let mut calls: Vec<HostCall> = Vec::new();
        commands::dispatch_command(&kind, &mut calls);
        for call in calls {
            self.perform(call, window, cx);
        }
        cx.notify();
    }

    fn perform(&mut self, call: HostCall, window: &mut Window, cx: &mut Context<Self>) {
        match call {
            HostCall::ShowView(view) => {
                // Opening a workspace from another view remembers that view
                // for the panel's Back; moving between workspaces (or back to
                // the one a review was opened over) keeps it.
                if workspace_name(&view).is_none() {
                    self.workspace_return = None;
                } else if workspace_name(self.client.view()).is_none() {
                    self.workspace_return = Some(self.client.view().clone());
                }
                self.route = Route::View;
                let chat = is_chat(&view);
                match view {
                    View::Issues => {
                        self.ensure_issues(window, cx);
                    }
                    View::Activity => {
                        self.ensure_activity(window, cx);
                    }
                    View::Machines => {
                        self.ensure_machines(window, cx);
                    }
                    View::Session(_) => {}
                }
                self.client.set_view(view);
                self.client.focus_main();
                self.banner = None;
                if chat {
                    // Selecting it again while it is failed or given up
                    // re-opens it (a relaunch with a fresh budget).
                    let chat = self.ensure_chat(window, cx);
                    chat.update(cx, |chat, cx| chat.show(cx));
                }
                if let Some(name) = workspace_name(self.client.view()).map(str::to_string) {
                    // Selecting it again re-selects its pane (re-opening a
                    // dead one), as the chat does.
                    let row = self.workspace_row(&name);
                    let ws = self.ensure_workspace(window, cx);
                    ws.update(cx, |ws, cx| ws.show(&name, row, cx));
                }
                self.sync_chat(window, cx);
                self.focus_main_view(window, cx);
            }
            HostCall::IssueCommand(kind) => {
                let issues = self.ensure_issues(window, cx);
                issues.update(cx, |v, cx| v.run_issue_command(&kind, window, cx));
            }
            HostCall::SwitchProject(project) => {
                self.switcher_open = false;
                if project != self.project {
                    let project = project.clone();
                    // Open the target's window after this update finishes,
                    // so the new window's first frame isn't nested in ours.
                    cx.defer(move |cx| open_or_focus(&project, cx));
                }
            }
            HostCall::OpenSwitcher => self.open_switcher(cx),
            // Leave, keeping every session and agent running.
            HostCall::Detach => cx.quit(),
            HostCall::DetachOthers => self.detach_others(cx),
            HostCall::QuitProject => self.confirm_quit_project(window, cx),
            HostCall::QuitShelbi => self.confirm_quit_shelbi(window, cx),
            HostCall::ToggleZen => self.toggle_zen(window, cx),
            HostCall::OpenErrorLog => self.open_error_log(window, cx),
            HostCall::OpenAddProject => self.open_add_project(window, cx),
            HostCall::LoadReview(task_id) => self.load_review(task_id, window, cx),
            HostCall::ApproveReview(id) => {
                let m = Mutation::ApproveReview {
                    project: self.project.clone(),
                    id: id.clone(),
                };
                self.run_review_mutation(id, m, Decision::APPROVE, window, cx);
            }
            HostCall::AskRejectReason(id) => {
                self.open_overlay(ActiveOverlay::reject_reason(id), window, cx)
            }
            HostCall::RejectReview(id, reason) => self.reject(id, reason, window, cx),
            HostCall::OpenEditor(target) => self.open_editor(target, cx),
        }
    }

    /// `edit:*`: open the target's file (the workflows directory for
    /// `edit:workflows`) in the user's editor, the path the TUI opens
    /// (`edit_target_path`). The lookup and the launch run off the UI thread;
    /// a terminal editor opens in Terminal.app, a GUI editor directly.
    fn open_editor(&mut self, target: EditTarget, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let launch = cx.background_executor().spawn(async move {
            let path = shelbi_app::command_model::edit_target_path(&project, &target)
                .filter(|p| p.exists())
                .ok_or_else(|| "Couldn't find that file to edit.".to_string())?;
            let editor = shelbi_state::resolve_editor();
            crate::views::external_edit::spawn(&crate::views::external_edit::launch_for(&editor, &path))?;
            Ok::<_, String>(format!(
                "Opened {} in {}.",
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                shelbi_state::editor_display_name(&editor)
            ))
        });
        cx.spawn(async move |this, cx| {
            let result = launch.await;
            this.update(cx, |this, cx| match result {
                Ok(message) => this.flash(message, cx),
                Err(e) => {
                    this.banner = Some(e.into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// The board for the window's project, created on first use. Its
    /// commands come back through [`Self::run`], so they take the same
    /// relaunch / bootstrap gates as the sidebar's.
    fn ensure_issues(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<IssuesView> {
        if let Some(issues) = &self.issues {
            return issues.clone();
        }
        let project = self.project.clone();
        let issues = cx.new(|cx| IssuesView::new(project, cx));
        let sub = cx.subscribe_in(&issues, window, |this, _, ev: &IssuesEvent, window, cx| {
            let IssuesEvent::Command(kind) = ev;
            this.run(kind.clone(), window, cx);
        });
        self._subscriptions.push(sub);
        self.issues = Some(issues.clone());
        issues
    }

    /// The orchestrator chat for the window's project, created on first use.
    fn ensure_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ChatView> {
        if let Some(chat) = &self.chat {
            return chat.clone();
        }
        let project = self.project.clone();
        let handle = window.window_handle();
        let sink = self._feed.as_ref().map(|f| f.session_sink());
        let chat =
            cx.new(|cx| ChatView::orchestrator(&project, handle, ChatDeps::live(&project), sink, cx));
        let sub = cx.subscribe(&chat, |this, _, ev: &ChatEvent, cx| match ev {
            // A click in the terminal moves the window's focus model to the
            // main area, so the sidebar stops taking keys.
            ChatEvent::Focused => {
                this.client.focus_main();
                cx.notify();
            }
            // Only a close-on-exit pane (the user shell) ends; the chat
            // restarts instead.
            ChatEvent::Ended => {}
        });
        self._subscriptions.push(sub);
        self.chat = Some(chat.clone());
        self.update_chat_supervisor_status(cx);
        chat
    }

    /// Hand the chat the supervisor's orchestrator failure, which its
    /// placeholder names while the orchestrator is down.
    fn update_chat_supervisor_status(&mut self, cx: &mut Context<Self>) {
        if let Some(chat) = self.chat.clone() {
            let status = self.model.orchestrator_status.clone();
            chat.update(cx, |chat, cx| chat.set_supervisor_status(status, cx));
        }
    }

    /// The Workspace view for the window's project, created on first use.
    fn ensure_workspace(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<WorkspaceView> {
        if let Some(ws) = &self.workspace {
            return ws.clone();
        }
        let project = self.project.clone();
        let handle = window.window_handle();
        let sink = self._feed.as_ref().map(|f| f.session_sink());
        let chord = self.palette_chord.clone();
        let deps = WorkspaceDeps::live(&project);
        #[cfg(any(test, feature = "visual-check"))]
        let deps = self.workspace_deps.take().unwrap_or(deps);
        let ws = cx.new(|cx| {
            let mut ws = WorkspaceView::new(&project, handle, deps, sink, cx);
            ws.set_palette_chord(&chord, cx);
            ws
        });
        let sub = cx.subscribe_in(&ws, window, |this, _, ev: &WorkspaceEvent, window, cx| match ev {
            WorkspaceEvent::Focused => {
                this.client.focus_main();
                cx.notify();
            }
            WorkspaceEvent::Back => this.leave_workspace(window, cx),
        });
        self._subscriptions.push(sub);
        self.workspace = Some(ws.clone());
        ws
    }

    /// The Review view for the window's project, created on first use.
    fn ensure_review(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ReviewView> {
        if let Some(review) = &self.review {
            return review.clone();
        }
        let project = self.project.clone();
        let handle = window.window_handle();
        let sink = self._feed.as_ref().map(|f| f.session_sink());
        let deps = ReviewDeps {
            ops: self.review_ops.clone(),
            ..ReviewDeps::live(&project)
        };
        #[cfg(any(test, feature = "visual-check"))]
        let deps = self.review_deps.take().unwrap_or(deps);
        let review = cx.new(|cx| ReviewView::new(&project, handle, deps, sink, cx));
        let sub = cx.subscribe_in(&review, window, |this, _, ev: &ReviewEvent, window, cx| match ev {
            ReviewEvent::Focused => {
                this.client.focus_main();
                cx.notify();
            }
            // Approve, or Reject (which asks for the reason): the registry's
            // commands, through the same gates as the palette's.
            ReviewEvent::Command(kind) => this.run(kind.clone(), window, cx),
            ReviewEvent::Back | ReviewEvent::Closed => this.leave_review(window, cx),
        });
        self._subscriptions.push(sub);
        self.review = Some(review.clone());
        review
    }

    /// The sidebar's row for review task `task`.
    fn review_row(&self, task: &str) -> Option<shelbi_app::view::ReviewRow> {
        self.model.reviews.iter().find(|r| r.task_id == task).cloned()
    }

    /// The review the main area shows, when it shows one.
    fn shown_review(&self, cx: &App) -> Option<String> {
        if !matches!(self.route, Route::Review) {
            return None;
        }
        let review = self.review.as_ref()?.read(cx);
        review.is_shown().then(|| review.task_id().map(str::to_string)).flatten()
    }

    /// Open the review `info` describes in the main area.
    fn open_review(&mut self, info: ReviewInfo, window: &mut Window, cx: &mut Context<Self>) {
        self.status = None;
        self.banner = None;
        let row = self.review_row(&info.task_id);
        let review = self.ensure_review(window, cx);
        self.route = Route::Review;
        review.update(cx, |r, cx| r.show(info, row, cx));
        self.client.focus_main();
        self.sync_chat(window, cx);
        self.focus_main_view(window, cx);
        cx.notify();
    }

    /// Leave the review for the view it was opened over: the sidebar comes
    /// back into its column (the push's reverse) and takes the keys again, on
    /// its selected row, as the TUI's back button does. Back leaves the
    /// review loaded; a close or a decision has ended it already. A
    /// collapsed sidebar stays collapsed, so keys go to the main area.
    fn leave_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Review) {
            return;
        }
        let view = self.client.view().clone();
        if !self.tapped_return(&view, window, cx) {
            self.perform(HostCall::ShowView(view), window, cx);
        }
        if !self.collapsed {
            self.client.focus_sidebar();
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Leave the workspace for the view it was opened over: the sidebar comes
    /// back into its column (the push's reverse) and takes the keys again, on
    /// its selected row, as leaving a review does. Showing the other view
    /// detaches the workspace's panes, as moving off a workspace always has;
    /// the daemon's sessions stay.
    fn leave_workspace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::View) || workspace_name(self.client.view()).is_none() {
            return;
        }
        let view = self.workspace_return.take().unwrap_or_else(View::default_for_project);
        if !self.tapped_return(&view, window, cx) {
            self.perform(HostCall::ShowView(view), window, cx);
        }
        if !self.collapsed {
            self.client.focus_sidebar();
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Under the test tap, leaving a review records the view it returns to
    /// instead of building it (building one reads the project's state) and
    /// only leaves the route. Returns whether it did.
    fn tapped_return(&mut self, view: &View, window: &mut Window, cx: &mut Context<Self>) -> bool {
        #[cfg(any(test, feature = "visual-check"))]
        if let Some(tap) = &self.tap {
            tap.borrow_mut().push(HostCall::ShowView(view.clone()));
            self.route = Route::View;
            if workspace_name(self.client.view()).is_some() {
                // Leaving a workspace: the main area (and so the column)
                // moves off it, though the view itself is only recorded.
                self.client.set_view(view.clone());
            }
            self.sync_chat(window, cx);
            return true;
        }
        let _ = (view, window, cx);
        false
    }

    /// Point the leading column at what the route wants (the review panel
    /// while a review is open, the workspace panel while a workspace is
    /// shown, else the sidebar), starting the push when that changes: a
    /// slide, or a crossfade under Reduce Motion.
    fn sync_column(&mut self, now: Instant) {
        let workspace_shown = matches!(self.route, Route::View)
            && workspace_name(self.client.view()).is_some()
            && self.workspace.is_some();
        let want = if matches!(self.route, Route::Review) && self.review.is_some() {
            Occupant::Review
        } else if workspace_shown {
            Occupant::Workspace
        } else {
            Occupant::Sidebar
        };
        if want != self.column.target() {
            let motion = if (self.reduce_motion)() { Motion::Fade } else { Motion::Slide };
            self.column.show(want, motion, now);
        }
    }

    /// Whether a panel (the review's or the workspace's) stands in the
    /// sidebar's column (or is on its way in).
    fn panel_in_column(&self) -> bool {
        self.column.target() != Occupant::Sidebar
    }

    /// Hand the review its refreshed sidebar row (the live server state), and
    /// drop a review whose task has left the review column: its close event
    /// may have been missed while the daemon was unreachable (the TUI's
    /// close-reconcile, `rt-tui-review`).
    fn update_review_row(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(review) = self.review.clone() else {
            return;
        };
        let Some(task) = review.read(cx).task_id().map(str::to_string) else {
            return;
        };
        let row = self.review_row(&task);
        let gone = row.is_none() && !self.model.board_loading && self.model.config_error.is_none();
        review.update(cx, |r, cx| r.update_row(row, cx));
        if gone && self.shown_review(cx).as_deref() == Some(task.as_str()) && !review.read(cx).is_merging() {
            self.review_ended(&task, window, cx);
        }
    }

    /// The shown review's task left review elsewhere (merged on GitHub, moved
    /// by another client): stop showing it.
    fn review_ended(&mut self, task: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(review) = self.review.clone() {
            review.update(cx, |r, cx| r.hide(cx));
        }
        self.flash(format!("Review {task} closed."), cx);
        self.leave_review(window, cx);
    }

    /// A layout event from the daemon: a slot this client loaded is serving
    /// (open it), or the shown review's slot was freed (close it).
    fn apply_layout(&mut self, event: shelbi_state::LayoutEvent, window: &mut Window, cx: &mut Context<Self>) {
        use shelbi_state::LayoutEvent;
        match event {
            LayoutEvent::ReviewOpened { task, .. } => {
                let step = self.review_loader.review_opened(&task);
                self.run_load_step(step, window, cx);
            }
            LayoutEvent::ReviewClosed { task, .. } => {
                let merging = self.review.as_ref().is_some_and(|r| r.read(cx).is_merging());
                if self.shown_review(cx).as_deref() == Some(task.as_str()) && !merging {
                    self.review_ended(&task, window, cx);
                }
            }
            // The content pane reconnects on its own; the chat likewise.
            LayoutEvent::ReviewAgentRecovered { .. } | LayoutEvent::OrchestratorRestarted => {}
        }
    }

    /// The sidebar's row for workspace `name`.
    fn workspace_row(&self, name: &str) -> Option<shelbi_app::view::WorkspaceRow> {
        self.model.workspaces.iter().find(|w| w.name == name).cloned()
    }

    /// Hand the shown workspace its refreshed sidebar row (its live state, and
    /// the change that re-attaches an idle one).
    fn update_workspace_row(&mut self, cx: &mut Context<Self>) {
        let Some(ws) = self.workspace.clone() else {
            return;
        };
        let Some(name) = ws.read(cx).workspace().map(str::to_string) else {
            return;
        };
        let row = self.workspace_row(&name);
        ws.update(cx, |ws, cx| ws.update_row(row, cx));
    }

    /// Whether the main area should show the chat right now.
    fn chat_wanted(&self) -> bool {
        matches!(self.boot, Boot::Ready(_))
            && matches!(self.route, Route::View)
            && is_chat(self.client.view())
    }

    /// Attach the chat when the main area shows it and detach it when it
    /// doesn't. Idempotent: it never re-shows a chat that is already shown,
    /// so a failed or given-up chat waits for the user to re-select it.
    fn sync_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let wanted = self.chat_wanted();
        if wanted {
            let chat = self.ensure_chat(window, cx);
            if !chat.read(cx).is_shown() {
                chat.update(cx, |chat, cx| chat.show(cx));
                if self.client.focus() == Focus::Main {
                    let target = chat.read(cx).focus_target(cx);
                    window.focus(&target, cx);
                }
            }
        } else if let Some(chat) = &self.chat {
            if chat.read(cx).is_shown() {
                chat.update(cx, |chat, cx| chat.hide(cx));
            }
        }
        // The Workspace view is shown by `ShowView` (it needs the row); here
        // it is only ever hidden, when the main area moves on.
        let workspace_wanted = matches!(self.boot, Boot::Ready(_))
            && matches!(self.route, Route::View)
            && workspace_name(self.client.view()).is_some();
        if !workspace_wanted {
            if let Some(ws) = &self.workspace {
                if ws.read(cx).is_shown() {
                    ws.update(cx, |ws, cx| ws.hide(cx));
                }
            }
        }
        // The Review view is shown by `open_review`; likewise only hidden
        // here. Hiding detaches its content; the daemon's sessions stay.
        if !matches!(self.route, Route::Review) {
            if let Some(review) = &self.review {
                if review.read(cx).is_shown() {
                    review.update(cx, |r, cx| r.hide(cx));
                }
            }
        }
    }

    /// Whether a main-area terminal (the chat's, or the Workspace view's
    /// agent or shell) holds keyboard focus.
    fn chat_focused(&self, window: &Window, cx: &App) -> bool {
        self.chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).contains_focus(window, cx))
            || self
                .workspace
                .as_ref()
                .is_some_and(|ws| ws.read(cx).is_shown() && ws.read(cx).contains_focus(window, cx))
            || self
                .review
                .as_ref()
                .is_some_and(|r| r.read(cx).is_shown() && r.read(cx).contains_focus(window, cx))
    }

    /// The Activity feed for the window's project, created on first use.
    /// A task row opens the task's detail on the board; a workspace row's
    /// `FocusWorkspace` goes through [`Self::run`] like the sidebar's.
    fn ensure_activity(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ActivityView> {
        if let Some(activity) = &self.activity {
            return activity.clone();
        }
        let project = self.project.clone();
        let activity = cx.new(|cx| ActivityView::new(project, cx));
        let sub = cx.subscribe_in(&activity, window, |this, _, ev: &ActivityEvent, window, cx| {
            match ev {
                ActivityEvent::OpenTask(id) => this.open_task(id, window, cx),
                ActivityEvent::Command(kind) => this.run(kind.clone(), window, cx),
            }
        });
        self._subscriptions.push(sub);
        self.activity = Some(activity.clone());
        activity
    }

    /// The Machines view for the window's project, created on first use.
    fn ensure_machines(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<MachinesView> {
        if let Some(machines) = &self.machines {
            return machines.clone();
        }
        let project = self.project.clone();
        let machines = cx.new(|cx| MachinesView::new(project, cx));
        let sub = cx.subscribe_in(&machines, window, |this, _, ev: &MachinesEvent, window, cx| {
            let MachinesEvent::Command(kind) = ev;
            this.run(kind.clone(), window, cx);
        });
        self._subscriptions.push(sub);
        self.machines = Some(machines.clone());
        machines
    }

    /// Show task `id`'s detail: route to the board and open it there.
    fn open_task(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.run(CommandKind::ShowView(View::Issues), window, cx);
        let issues = self.ensure_issues(window, cx);
        issues.update(cx, |v, cx| v.open_task(id, cx));
    }

    /// Hand keyboard focus to what the main area shows: a native view, the
    /// chat or the Workspace view takes it (their own keys), anything else
    /// leaves it on the window.
    fn focus_main_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Review) {
            match &self.review {
                Some(review) => {
                    let target = review.read(cx).focus_target(cx);
                    window.focus(&target, cx);
                }
                None => self.focus_handle.focus(window, cx),
            }
            return;
        }
        if !matches!(self.route, Route::View) {
            self.focus_handle.focus(window, cx);
            return;
        }
        match self.client.view() {
            View::Issues if self.issues.is_some() => {
                if let Some(issues) = &self.issues {
                    issues.update(cx, |v, _| v.request_focus());
                }
            }
            View::Activity if self.activity.is_some() => {
                if let Some(activity) = &self.activity {
                    activity.update(cx, |v, _| v.request_focus());
                }
            }
            View::Machines if self.machines.is_some() => {
                if let Some(machines) = &self.machines {
                    machines.update(cx, |v, _| v.request_focus());
                }
            }
            view if is_chat(view) && self.chat.is_some() => {
                if let Some(chat) = &self.chat {
                    let target = chat.read(cx).focus_target(cx);
                    window.focus(&target, cx);
                }
            }
            view if workspace_name(view).is_some() && self.workspace.is_some() => {
                if let Some(ws) = &self.workspace {
                    let target = ws.read(cx).focus_target(cx);
                    window.focus(&target, cx);
                }
            }
            _ => self.focus_handle.focus(window, cx),
        }
    }

    fn open_switcher(&mut self, cx: &mut Context<Self>) {
        self.switcher_open = true;
        // Re-read the project list off the UI thread; repaint when it lands.
        if let Some(catalog) = cx.try_global::<Catalog>().cloned() {
            let (tx, rx) = futures::channel::oneshot::channel();
            catalog.refresh(move || {
                let _ = tx.send(());
            });
            cx.spawn(async move |this, cx| {
                if rx.await.is_ok() {
                    this.update(cx, |_, cx| cx.notify()).ok();
                }
            })
            .detach();
        }
    }

    fn detach_others(&mut self, cx: &mut Context<Self>) {
        let client_id = cx
            .try_global::<Shell>()
            .map(|s| s.client_id.clone())
            .unwrap_or_default();
        let reason = format!("Detached by the desktop app on {}.", shelbi_client::hostname());
        let control = self.ready().map(|r| r.control.clone());
        let work = cx.background_executor().spawn(async move {
            let mut client = control
                .ok_or("Couldn't reach the daemon to detach other windows.")?
                .connect()
                .map_err(|e| format!("Couldn't reach the daemon: {e}"))?;
            client
                .detach_clients(DetachSelector::AllExcept { client_id }, &reason)
                .map_err(|e| format!("Couldn't detach other windows: {e}"))
        });
        cx.spawn(async move |this, cx| {
            let message = match work.await {
                Ok(0) => "No other windows are attached.".to_string(),
                Ok(n) => format!("Detached {n} other window{}.", if n == 1 { "" } else { "s" }),
                Err(e) => e.to_string(),
            };
            this.update(cx, |this, cx| {
                this.banner = Some(message.into());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn confirm_quit_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Quit {}?", self.model.project_label),
            Some("This stops the project's agents and closes its window. Other projects keep running."),
            &["Quit Project", "Cancel"],
            cx,
        );
        let project = self.project.clone();
        let control = self.ready().map(|r| r.control.clone());
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            // The orchestrator is about to be stopped on purpose: its exit is
            // not a crash to restart.
            this.update(cx, |this, cx| {
                if let Some(chat) = &this.chat {
                    chat.update(cx, |chat, _| chat.note_deliberate_close());
                }
                if let Some(ws) = &this.workspace {
                    ws.update(cx, |ws, cx| ws.note_deliberate_close(cx));
                }
                if let Some(review) = &this.review {
                    review.update(cx, |r, cx| r.note_deliberate_close(cx));
                }
            })
            .ok();
            let quit = cx
                .background_executor()
                .spawn(async move {
                    control
                        .ok_or_else(|| "Couldn't reach the daemon.".to_string())?
                        .connect()
                        .map_err(|e| e.to_string())?
                        .quit_project(&project)
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update_in(cx, |this, window, cx| match quit {
                Ok(()) => window.remove_window(),
                Err(e) => {
                    if let Some(chat) = &this.chat {
                        chat.update(cx, |chat, _| chat.clear_deliberate_close());
                    }
                    if let Some(ws) = &this.workspace {
                        ws.update(cx, |ws, cx| ws.clear_deliberate_close(cx));
                    }
                    if let Some(review) = &this.review {
                        review.update(cx, |r, cx| r.clear_deliberate_close(cx));
                    }
                    this.banner = Some(format!("Couldn't quit the project: {e}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn confirm_quit_shelbi(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Critical,
            "Quit Shelbi?",
            Some("This stops every agent in every project and shuts the daemon down."),
            &["Quit Shelbi", "Cancel"],
            cx,
        );
        let control = self.ready().map(|r| r.control.clone());
        cx.spawn_in(window, async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let quit = cx
                .background_executor()
                .spawn(async move {
                    control
                        .ok_or_else(|| "Couldn't reach the daemon.".to_string())?
                        .connect()
                        .map_err(|e| e.to_string())?
                        .quit_shelbi()
                        .map_err(|e| e.to_string())
                })
                .await;
            this.update(cx, |this, cx| match quit {
                Ok(()) => cx.quit(),
                Err(e) => {
                    this.banner = Some(format!("Couldn't quit Shelbi: {e}").into());
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Settings(_)) {
            let project = self.project.clone();
            let view = cx.new(|cx| SettingsView::new(&project, window, cx));
            // A saved switch applies to this window's next notice at once.
            let sub = cx.subscribe(&view, |this, _, event: &SettingsEvent, cx| match event {
                SettingsEvent::Saved => this.load_notify_prefs(cx),
            });
            self._subscriptions.push(sub);
            self.route = Route::Settings(view);
        }
        self.sync_chat(window, cx);
        self.banner = None;
        self.client.focus_main();
        self.focus_main_view(window, cx);
        cx.notify();
    }

    /// A divider drag asked for `width` columns: the user's sidebar width,
    /// and the column's while a panel holds it with the sidebar collapsed.
    fn drag_column(&mut self, width: u16, cx: &mut Context<Self>) {
        let mut changed = false;
        if self.collapsed && self.panel_in_column() && self.collapsed_panel_cols != Some(width) {
            self.collapsed_panel_cols = Some(width);
            changed = true;
        }
        if width != self.client.sidebar_width() {
            self.client.set_sidebar_width(width);
            changed = true;
        }
        if changed {
            cx.notify();
        }
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        if self.collapsed {
            self.client.focus_main();
        }
        self.window_state.sidebar_collapsed = self.collapsed;
        self.schedule_save(cx);
        self.sync_menus(cx);
        cx.notify();
    }

    /// Save the window state off the UI thread, debounced so a window drag
    /// (a bounds change per frame) writes once when it settles.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation += 1;
        let generation = self.save_generation;
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(400));
        cx.spawn(async move |this, cx| {
            timer.await;
            let Ok(Some((state, project))) = this.update(cx, |this, _| {
                (this.save_generation == generation)
                    .then(|| (this.window_state.clone(), this.project.clone()))
            }) else {
                return;
            };
            let saved = cx
                .background_executor()
                .spawn(async move { state.save(&project) })
                .await;
            if let Err(e) = saved {
                tracing::warn!(error = %e, "could not save the window state");
            }
        })
        .detach();
    }

    // --- overlays ----------------------------------------------------------

    /// Show `overlay`. Keys go to it until it closes; the routed view and the
    /// focus to return to are left as they were.
    fn open_overlay(&mut self, overlay: ActiveOverlay, window: &mut Window, cx: &mut Context<Self>) {
        self.client.open_overlay(overlay.nav());
        self.overlay = Some(overlay);
        self.switcher_open = false;
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Close the overlay and hand keys back to where focus was.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.overlay = None;
        self.detecting = None;
        self.client.close_overlay();
        match self.client.focus() {
            Focus::Main => self.focus_main_view(window, cx),
            Focus::Sidebar => self.focus_handle.focus(window, cx),
        }
        cx.notify();
    }

    /// The palette's entries for the current state: the registry over the
    /// live sidebar model and the last [`PaletteInputs`].
    fn palette_entries(&self) -> Vec<shelbi_palette::Entry> {
        CommandRegistry::new().entries(&self.command_model())
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.palette_entries();
        let projects = self.palette_inputs.projects.clone();
        self.open_overlay(ActiveOverlay::palette(entries, projects), window, cx);
        // Re-read the inputs off the UI thread; the open palette picks them up.
        self.refresh_palette_inputs(cx);
    }

    /// A key while an overlay is open: it takes every key except the app's
    /// own Cmd shortcuts (close the window, quit), plus Cmd+V to paste into
    /// its text and Cmd+C to copy the error log.
    fn overlay_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let cmd = keystroke.modifiers.platform && !keystroke.modifiers.control;
        if cmd && keystroke.key == "v" {
            self.overlay_paste(window, cx);
            cx.stop_propagation();
            return;
        }
        if cmd && keystroke.key == "c" {
            self.overlay_copy(cx);
            cx.stop_propagation();
            return;
        }
        // The app's other Cmd shortcuts (⌘W, ⌘Q, ⌘,) stay live; ⌘K is the
        // palette chord, which toggles it closed.
        if cmd && keystroke.key != "k" {
            return;
        }
        cx.stop_propagation();
        let Some(chord) = overlay_chord(keystroke) else {
            // A key with no chord still dismisses the "no slots" report.
            if let Some(ActiveOverlay::ReviewConfirm { task_id, dialog }) = &self.overlay {
                let event = overlay::confirm_event(task_id, dialog.dismiss_if_empty());
                self.apply_overlay_event(event, window, cx);
            }
            return;
        };
        let Some(o) = self.overlay.as_mut() else {
            return;
        };
        let event = o.handle_key(chord, &self.keymaps);
        self.apply_overlay_event(event, window, cx);
        cx.notify();
    }

    /// Act on what the open overlay resolved to.
    fn apply_overlay_event(&mut self, event: OverlayEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            OverlayEvent::Stay => self.detect_add_project(cx),
            OverlayEvent::Close => self.close_overlay(window, cx),
            OverlayEvent::FocusSidebar => {
                self.client.focus_sidebar();
                self.close_overlay(window, cx);
            }
            OverlayEvent::RunEntry(entry) => {
                // Close first: the command may open another overlay.
                self.close_overlay(window, cx);
                match CommandKind::from_id(&entry.id) {
                    Some(kind) => self.run(kind, window, cx),
                    None => self.banner = Some(format!("Unknown command: {}", entry.id).into()),
                }
            }
            OverlayEvent::LoadReview { task_id, slot } => {
                self.close_overlay(window, cx);
                let step = self.review_loader.slot_chosen(&task_id, &slot);
                self.run_load_step(step, window, cx);
            }
            OverlayEvent::Rejected { task_id, reason } => {
                self.close_overlay(window, cx);
                self.reject(task_id, reason, window, cx);
            }
            OverlayEvent::ZenResult {
                project,
                confirmed,
                dont_show_again,
            } => {
                self.close_overlay(window, cx);
                self.run_zen(project, Some((confirmed, dont_show_again)), cx);
            }
            OverlayEvent::AddProjectSubmit { name, root, mode } => {
                self.submit_add_project(name, root, mode, window, cx)
            }
            OverlayEvent::ClearErrors { project } => {
                if let Some(ActiveOverlay::ErrorLog {
                    viewer: Some(viewer),
                    ..
                }) = &mut self.overlay
                {
                    viewer.clear();
                }
                let clear = cx
                    .background_executor()
                    .spawn(async move { shelbi_state::clear_errors(&project).map_err(|e| e.to_string()) });
                cx.spawn(async move |this, cx| {
                    let result = clear.await;
                    this.update(cx, |this, cx| {
                        if let Err(e) = result {
                            this.banner = Some(format!("Couldn't clear the error log: {e}").into());
                        }
                        this.request_refresh();
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            OverlayEvent::Copy(text) => {
                overlay::error_log::copy(text, cx);
                self.flash("Copied the error log.", cx);
            }
        }
        cx.notify();
    }

    /// Ask the feed for a fresh sidebar (the Zen band, the unread count).
    fn request_refresh(&self) {
        if let Some(feed) = &self._feed {
            feed.request();
        }
    }

    /// Show `message` as the status note for a few seconds.
    fn flash(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.flash_generation += 1;
        let generation = self.flash_generation;
        self.status = Some(message.into());
        let timer = cx.background_executor().timer(std::time::Duration::from_secs(4));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                if this.flash_generation == generation {
                    this.status = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// `ToggleZen`: through the first-run intro on a first off-to-on toggle
    /// (both checks read small local files, off the UI thread).
    fn toggle_zen(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.clone();
        let check = cx
            .background_executor()
            .spawn(async move { shelbi_app::zen::should_show_intro(&project) });
        cx.spawn_in(window, async move |this, cx| {
            let intro = check.await;
            this.update_in(cx, |this, window, cx| {
                let project = this.project.clone();
                if intro {
                    this.open_overlay(ActiveOverlay::zen_intro(project), window, cx);
                } else {
                    this.run_zen(project, None, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Toggle Zen (or apply the intro's answer) off the UI thread, then
    /// refresh the sidebar so its Zen band follows.
    fn run_zen(&mut self, project: String, intro: Option<(bool, bool)>, cx: &mut Context<Self>) {
        let work = cx.background_executor().spawn(async move {
            match intro {
                Some((confirmed, dont_show_again)) => {
                    shelbi_app::zen::apply_intro(&project, confirmed, dont_show_again)
                }
                None => shelbi_app::zen::toggle(&project)
                    .unwrap_or_else(|e| format!("Zen toggle failed: {e}")),
            }
        });
        cx.spawn(async move |this, cx| {
            let message = work.await;
            this.update(cx, |this, cx| {
                this.flash(message, cx);
                this.request_refresh();
            })
            .ok();
        })
        .detach();
    }

    /// `OpenErrorLog`: open it at once, read and mark the log read off the
    /// UI thread (the sidebar's unread button clears on the next refresh).
    fn open_error_log(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let project = self.project.clone();
        self.open_overlay(ActiveOverlay::error_log(project.clone()), window, cx);
        let read = cx.background_executor().spawn({
            let project = project.clone();
            async move {
                let rows = shelbi_state::read_errors_with_unread(&project).unwrap_or_default();
                let _ = shelbi_state::mark_errors_read(&project);
                rows
            }
        });
        cx.spawn(async move |this, cx| {
            let rows = read.await;
            this.update(cx, |this, cx| {
                if let Some(ActiveOverlay::ErrorLog { project: p, viewer, .. }) = &mut this.overlay {
                    if *p == project {
                        *viewer = Some(Viewer::new(rows));
                    }
                }
                this.request_refresh();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// `AddProject`: the form, its repo path prefilled with the working
    /// directory (the home directory when the app was opened from Finder).
    fn open_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cwd = std::env::current_dir()
            .ok()
            .filter(|d| d.parent().is_some())
            .or_else(|| std::env::var_os("HOME").map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("/"));
        self.open_overlay(ActiveOverlay::add_project(&cwd), window, cx);
        self.detect_add_project(cx);
    }

    /// Run the form's "Detected:" check off the UI thread when its repo path
    /// changed (one at a time; a newer path is checked when the older lands).
    fn detect_add_project(&mut self, cx: &mut Context<Self>) {
        let Some(ActiveOverlay::AddProject { form, .. }) = &self.overlay else {
            return;
        };
        let Some(root) = form.detection_due() else {
            return;
        };
        if self.detecting.is_some() {
            return;
        }
        self.detecting = Some(root.clone());
        let cwd = form.cwd().to_path_buf();
        let detect = cx.background_executor().spawn({
            let root = root.clone();
            async move { shelbi_orchestrator::project_create::detect_root(&cwd, &root) }
        });
        cx.spawn(async move |this, cx| {
            let detected = detect.await;
            this.update(cx, |this, cx| {
                this.detecting = None;
                if let Some(ActiveOverlay::AddProject { form, .. }) = &mut this.overlay {
                    form.set_detected(&root, detected);
                }
                this.detect_add_project(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Run the form's `Mutation::AddProject` off the UI thread. A validation
    /// error goes back on the form; success opens the new project's window.
    fn submit_add_project(
        &mut self,
        name: String,
        root: String,
        mode: ConfigMode,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(ActiveOverlay::AddProject { form, busy }) = &mut self.overlay else {
            return;
        };
        *busy = true;
        let cwd = form.cwd().to_path_buf();
        let mutation = Mutation::AddProject { name, root, mode };
        let work = cx
            .background_executor()
            .spawn(async move { commands::run_add_project(&mutation, &cwd) });
        cx.spawn_in(_window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| match result {
                Ok(slug) => {
                    this.close_overlay(window, cx);
                    this.flash(format!("Created project {slug}."), cx);
                    if let Some(catalog) = cx.try_global::<Catalog>().cloned() {
                        catalog.refresh(|| {});
                    }
                    cx.defer(move |cx| open_or_focus(&slug, cx));
                }
                Err(AddProjectError::Invalid(msg)) | Err(AddProjectError::Failed(msg)) => {
                    if let Some(ActiveOverlay::AddProject { form, busy }) = &mut this.overlay {
                        *busy = false;
                        form.set_error(msg);
                    }
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    /// `LoadReview`: open a review task. Already shown, it just takes focus;
    /// otherwise the load flow ([`ReviewLoader`]) resolves it off the UI
    /// thread and opens it, or asks which free slot to load it onto.
    fn load_review(&mut self, task_id: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.shown_review(cx).as_deref() == Some(task_id.as_str()) {
            self.focus_main_view(window, cx);
            return;
        }
        let step = self.review_loader.open(&task_id);
        self.run_load_step(step, window, cx);
    }

    /// Carry out one step of the review load flow; the blocking ones run on
    /// the background executor and feed their result back into the flow.
    fn run_load_step(&mut self, step: LoadStep, window: &mut Window, cx: &mut Context<Self>) {
        match step {
            LoadStep::None => {}
            LoadStep::Resolve(task) => {
                let (ops, project, t) = (self.review_ops.clone(), self.project.clone(), task.clone());
                let resolve = cx.background_executor().spawn(async move { ops.resolve(&project, &t) });
                cx.spawn_in(window, async move |this, cx| {
                    let result = resolve.await;
                    this.update_in(cx, |this, window, cx| {
                        let step = this.review_loader.resolved(&task, result);
                        this.run_load_step(step, window, cx);
                    })
                    .ok();
                })
                .detach();
            }
            LoadStep::Open(info) => self.open_review(*info, window, cx),
            LoadStep::AskSlot {
                task,
                title,
                free_slots,
            } => {
                let total = free_slots.len();
                let slots = free_slots
                    .into_iter()
                    .map(|name| Slot { name, occupant: None })
                    .collect();
                self.open_overlay(ActiveOverlay::review_confirm(task, title, slots, total), window, cx);
            }
            // The overlay's "No review workspace is configured." report.
            LoadStep::NoSlots { task, title } => {
                self.open_overlay(ActiveOverlay::review_confirm(task, title, Vec::new(), 0), window, cx)
            }
            // Every slot is busy: the overlay's busy report, and nothing is
            // loaded. The task stays queued (pending-load) and the daemon's
            // auto-loader puts it on the next slot that frees.
            LoadStep::Busy { task, title } => {
                self.open_overlay(ActiveOverlay::review_confirm(task, title, Vec::new(), 1), window, cx)
            }
            LoadStep::Load { task, slot } => {
                self.status = Some(format!("Loading {task} onto {slot}…").into());
                let (ops, project) = (self.review_ops.clone(), self.project.clone());
                let (t, workspace) = (task.clone(), slot.clone());
                let load = cx.background_executor().spawn(async move {
                    ops.session_op(&project, &t, shelbi_app::ReviewSessionOp::Load { workspace })
                });
                cx.spawn_in(window, async move |this, cx| {
                    let result = load.await;
                    this.update_in(cx, |this, window, cx| {
                        let ok = result.is_ok();
                        let step = this.review_loader.loaded(&task, result);
                        // Loaded: the daemon's `ReviewOpened` opens the view,
                        // unless it already has.
                        this.status = (ok && this.review_loader.pending() == Some(task.as_str()))
                            .then(|| format!("Loaded {task}; opening the review…").into());
                        this.run_load_step(step, window, cx);
                        this.request_refresh();
                        cx.notify();
                    })
                    .ok();
                })
                .detach();
            }
            LoadStep::Failed(message) => {
                self.status = None;
                self.banner = Some(message.into());
            }
        }
        cx.notify();
    }

    fn reject(&mut self, id: String, reason: String, window: &mut Window, cx: &mut Context<Self>) {
        let m = Mutation::RejectReview {
            project: self.project.clone(),
            id: id.clone(),
            reason,
        };
        self.run_review_mutation(id, m, Decision::REJECT, window, cx);
    }

    /// Send a review decision to the daemon off the UI thread. The daemon runs
    /// it (the gated merge for an approve), so it completes even if this app
    /// quits mid-merge. While it runs, an open Review view of the task is
    /// inert; when it lands the view closes (success) or shows why not.
    fn run_review_mutation(
        &mut self,
        id: String,
        m: Mutation,
        decision: Decision,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let review = self
            .review
            .clone()
            .filter(|r| r.read(cx).task_id() == Some(id.as_str()));
        match &review {
            Some(r) => r.update(cx, |r, cx| r.set_merging(true, cx)),
            None => self.flash(format!("{} {id}…", decision.working), cx),
        }
        let work = cx.background_executor().spawn(async move {
            shelbi_app::execute_mutation(&m, &mut |_, _| {}).map_err(|e| e.to_string())
        });
        cx.spawn_in(window, async move |this, cx| {
            let result = work.await;
            this.update_in(cx, |this, window, cx| {
                // The view may have moved on to another review meanwhile.
                let review = review.filter(|r| r.read(cx).task_id() == Some(id.as_str()));
                let shown = this.shown_review(cx).as_deref() == Some(id.as_str());
                match result {
                    Ok(()) => {
                        if let Some(r) = &review {
                            r.update(cx, |r, cx| r.finish(cx));
                        }
                        this.flash(format!("{} {id}.", decision.done), cx);
                        if shown {
                            this.leave_review(window, cx);
                        }
                    }
                    Err(e) => match &review {
                        Some(r) => r.update(cx, |r, cx| {
                            r.set_merging(false, cx);
                            r.set_status(Some(format!("{} failed: {e}", decision.verb)), cx);
                        }),
                        None => {
                            this.status = None;
                            this.banner = Some(e.into());
                        }
                    },
                }
                this.request_refresh();
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // --- keyboard ----------------------------------------------------------

    /// The capture phase, ahead of the focused view: while the chat's terminal
    /// holds focus, the global chords are the window's, never the session's
    /// (the terminal would otherwise encode Ctrl+H as a backspace), as the TUI
    /// intercepts them before forwarding a key to a session.
    fn on_capture_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.chat_focused(window, cx) {
            return;
        }
        let Some(chord) = chord_from_keystroke(&event.keystroke) else {
            return;
        };
        if takes_from_terminal(&self.keymaps, chord) {
            self.on_key_down(event, window, cx);
        }
    }

    /// Move keyboard focus to the sidebar (the `focus_sidebar` chord).
    fn focus_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Review) {
            // The review panel holds the sidebar's column: it takes the keys
            // (the main area's, in the window's focus model).
            if let Some(review) = self.review.clone() {
                self.client.focus_main();
                review.update(cx, |r, cx| r.focus_panel(window, cx));
            }
        } else if self.column.target() == Occupant::Workspace {
            // Likewise the workspace panel.
            if let Some(ws) = self.workspace.clone() {
                self.client.focus_main();
                ws.update(cx, |w, cx| w.focus_panel(window, cx));
            }
        } else if !self.collapsed {
            self.client.focus_sidebar();
            // Take keys back from a main view that holds focus.
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Move keyboard focus to the main area (the `focus_main` chord).
    fn focus_main(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.client.focus_main();
        self.focus_main_view(window, cx);
        cx.notify();
    }

    // --- menus -------------------------------------------------------------

    /// A menu item: a registry command runs exactly as the palette runs it.
    fn menu_invoke(&mut self, invoke: &menus::MenuInvoke, window: &mut Window, cx: &mut Context<Self>) {
        use menus::tree::{AppCommand, MenuAction};
        match &invoke.target {
            MenuAction::Command(id) => {
                // The palette closes before running its entry; so does a menu
                // pick made with an overlay up.
                if self.overlay.is_some() {
                    self.close_overlay(window, cx);
                }
                match CommandKind::from_id(id) {
                    Some(kind) => self.run(kind, window, cx),
                    None => self.banner = Some(format!("Unknown command: {id}").into()),
                }
            }
            MenuAction::App(command) => match command {
                AppCommand::About => self.show_about(window, cx),
                AppCommand::Settings => self.open_settings(window, cx),
                AppCommand::CloseWindow => window.remove_window(),
                AppCommand::Minimize => window.minimize_window(),
                AppCommand::Zoom => window.zoom_window(),
                AppCommand::ToggleSidebar => self.toggle_sidebar(cx),
                AppCommand::CommandPalette => {
                    if self.overlay.is_none() {
                        self.open_palette(window, cx);
                    }
                }
                AppCommand::FocusSidebar => self.focus_sidebar(window, cx),
                AppCommand::FocusMain => self.focus_main(window, cx),
                other => {
                    menus::run_app_command(*other, cx);
                }
            },
        }
        cx.notify();
    }

    fn show_about(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let detail = match self.ready().and_then(|r| r.cli_version.clone()) {
            Some(cli) => format!("Desktop {APP_VERSION}, CLI {cli}\n{}", menus::tree::DOCS_URL),
            None => format!("Desktop {APP_VERSION}\n{}", menus::tree::DOCS_URL),
        };
        let answer = window.prompt(PromptLevel::Info, "Shelbi", Some(&detail), &["OK"], cx);
        cx.spawn(async move |_, _| {
            let _ = answer.await;
        })
        .detach();
    }

    /// Edit ▸ Paste with an overlay up: type the clipboard into its text.
    fn overlay_paste(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let multiline = self.overlay.as_ref().is_some_and(ActiveOverlay::takes_newlines);
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            for chord in paste_chords(&text, multiline) {
                if let Some(o) = self.overlay.as_mut() {
                    let event = o.handle_key(chord, &self.keymaps);
                    if event != OverlayEvent::Stay {
                        self.apply_overlay_event(event, window, cx);
                        break;
                    }
                }
            }
            self.detect_add_project(cx);
        }
        cx.notify();
    }

    /// Edit ▸ Copy with an overlay up: the error log's text, when it shows.
    fn overlay_copy(&mut self, cx: &mut Context<Self>) {
        if let Some(ActiveOverlay::ErrorLog {
            viewer: Some(viewer),
            ..
        }) = &self.overlay
        {
            overlay::error_log::copy(viewer.text(), cx);
            self.flash("Copied the error log.", cx);
        }
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let chord = chord_from_keystroke(&event.keystroke);
        tracing::trace!(keystroke = ?event.keystroke, ?chord, focus = ?self.client.focus(), "key down");
        if self.overlay.is_some() {
            self.overlay_key(event, window, cx);
            return;
        }
        let Some(chord) = chord else {
            return;
        };
        if shelbi_app::overlay::opens_palette(&self.keymaps, &chord) {
            self.open_palette(window, cx);
            cx.stop_propagation();
            return;
        }
        if chord.code == Key::Esc && self.switcher_open {
            self.switcher_open = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match self.keymaps.global.dispatch(chord) {
            Some(GlobalAction::FocusSidebar) => {
                self.focus_sidebar(window, cx);
                cx.stop_propagation();
                return;
            }
            Some(GlobalAction::FocusMain) => {
                self.focus_main(window, cx);
                cx.stop_propagation();
                return;
            }
            Some(GlobalAction::ZenToggle) => {
                self.run(CommandKind::ToggleZen, window, cx);
                cx.stop_propagation();
                return;
            }
            // Ctrl+C quits the TUI; here it's the terminal's (⌘Q quits).
            Some(GlobalAction::Quit | GlobalAction::OpenPalette) | None => {}
        }
        if self.client.focus() != Focus::Sidebar || self.collapsed || self.panel_in_column() {
            // The main area's views take their own keys once they land; the
            // sidebar takes none while a panel stands in for it.
            return;
        }
        let count = self.selectable_count();
        match (self.keymaps.sidebar.dispatch(chord), chord.code) {
            (Some(SidebarAction::NavUp), _) | (_, Key::BackTab) => self.client.select_up(),
            (Some(SidebarAction::NavDown), _) | (_, Key::Tab) => self.client.select_down(count),
            (Some(SidebarAction::Activate), _) => self.activate_selection(window, cx),
            (Some(SidebarAction::Refresh), _) => {
                if let Some(feed) = &self._feed {
                    feed.request();
                }
            }
            // `q` / Ctrl+C close the TUI; a window closes with ⌘W instead.
            (Some(SidebarAction::Quit), _) | (None, _) => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // --- rendering ---------------------------------------------------------

    /// The main area. `column_shown`: the leading column is up (the sidebar,
    /// or the review panel in its place); without it the main area offers
    /// the button that expands the sidebar.
    fn render_main(&self, theme: &Theme, column_shown: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.route {
            Route::Settings(view) => div().size_full().child(view.clone()).into_any_element(),
            Route::Review => div()
                .flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .children(self.review.clone().map(|r| r.update(cx, |r, cx| r.render_main(cx))))
                .into_any_element(),
            // Every view draws natively; a view whose entity isn't built yet
            // (the first frame after routing) draws an empty main area.
            Route::View => {
                let view = self.client.view();
                let content = match view {
                    View::Issues => self.issues.clone().map(|v| v.into_any_element()),
                    View::Activity => self.activity.clone().map(|v| v.into_any_element()),
                    View::Machines => self.machines.clone().map(|v| v.into_any_element()),
                    View::Session(_) if is_chat(view) => self.chat.clone().map(|v| v.into_any_element()),
                    View::Session(_) => self.workspace.clone().map(|w| w.update(cx, |w, cx| w.render_main(cx))),
                };
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .children(content)
                    .into_any_element()
            }
        };
        // A band across the main area, tinted by what it says.
        let notice = |text: SharedString, hue: Hue| {
            let (fg, bg) = theme.hue(hue);
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_4()
                .py_2()
                .bg(bg)
                .border_b_1()
                .border_color(theme.rule)
                .text_color(fg)
                .child(text)
        };
        div()
            .flex()
            .flex_col()
            .flex_1()
            // A flex item defaults to its content's width; the board's columns
            // must scroll inside the main area, not widen it.
            .min_w_0()
            .overflow_hidden()
            .h_full()
            .bg(theme.surface)
            .when(!column_shown, |main| {
                main.child(
                    div().h(px(sidebar::TRAFFIC_LIGHT_INSET)).pl(px(80.0)).pt(px(8.0)).child(
                        style::icon_button("expand-sidebar", icons::BARS_3, IconButtonSize::Default, false, theme)
                            .self_start()
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    ),
                )
            })
            .when_some(self.model.session_lost.clone(), |main, lost| {
                main.child(notice(lost.into(), Hue::Red))
            })
            // A newer release (platform::update), in the main column so it
            // clears the titlebar's traffic lights.
            .when_some(
                cx.try_global::<UpdateState>()
                    .and_then(|u| u.visible().cloned()),
                |main, update| {
                    main.child(update_view::render_update_bar(&update, theme, |_, cx| {
                        cx.global_mut::<UpdateState>().dismissed = true;
                        cx.refresh_windows();
                    }))
                },
            )
            .when_some(self.status.clone(), |main, status| {
                main.child(notice(status, Hue::Neutral))
            })
            .when_some(self.banner.clone(), |main, message| {
                main.child(
                    notice(message, Hue::Gold).child(
                        style::icon_button("dismiss-banner", icons::X_MARK, IconButtonSize::Small, false, theme)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.banner = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(body)
    }
}

/// Whether `view` is the orchestrator chat.
fn is_chat(view: &View) -> bool {
    matches!(view, View::Session(name) if name == "orch")
}

/// The workspace `view` shows, when it is a workspace's session.
fn workspace_name(view: &View) -> Option<&str> {
    match view {
        View::Session(name) if name != "orch" => Some(name),
        _ => None,
    }
}

/// Whether the window takes `chord` away from a focused terminal: the focus
/// moves (Ctrl+H / Ctrl+L by default), the Zen toggle (Option+Z), and every
/// palette chord (the configured one plus the reserved Ctrl+Space and Cmd+K).
/// Everything else is the session's.
fn takes_from_terminal(keymaps: &Keymaps, chord: shelbi_state::keymap::KeyChord) -> bool {
    matches!(
        keymaps.global.dispatch(chord),
        Some(GlobalAction::FocusSidebar | GlobalAction::FocusMain | GlobalAction::ZenToggle)
    ) || shelbi_app::overlay::opens_palette(keymaps, &chord)
}

/// The palette-open chord as the search box shows it (`⌃P`).
fn palette_chord(keymaps: &Keymaps) -> String {
    keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .map(|c| format_chord(c, DisplayStyle::Mac))
        .unwrap_or_default()
}

impl OverlayHost for ShelbiWindow {
    fn overlay_mut(&mut self) -> Option<&mut ActiveOverlay> {
        self.overlay.as_mut()
    }

    fn overlay_event(&mut self, event: OverlayEvent, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_overlay_event(event, window, cx);
    }
}

/// A review decision's wording: the status while it runs, the note when it
/// lands, and the panel's failure line (the TUI's `approve failed: …`).
#[derive(Debug, Clone, Copy)]
struct Decision {
    working: &'static str,
    done: &'static str,
    verb: &'static str,
}

impl Decision {
    const APPROVE: Decision = Decision {
        working: "Approving",
        done: "Approved",
        verb: "approve",
    };
    const REJECT: Decision = Decision {
        working: "Rejecting",
        done: "Rejected",
        verb: "reject",
    };
}

impl SidebarHost for ShelbiWindow {
    fn sidebar_event(&mut self, event: SidebarEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SidebarEvent::Activate(ordinal) => {
                let count = self.selectable_count();
                self.client.select(ordinal, count);
                self.activate_selection(window, cx);
            }
            SidebarEvent::OpenSearch => self.open_palette(window, cx),
            SidebarEvent::ToggleSwitcher => {
                if self.switcher_open {
                    self.switcher_open = false;
                } else {
                    self.run(
                        CommandKind::SwitchProject {
                            project: String::new(),
                        },
                        window,
                        cx,
                    );
                }
            }
            SidebarEvent::SwitchTo(project) => {
                self.run(CommandKind::SwitchProject { project }, window, cx)
            }
            SidebarEvent::OpenErrors => self.run(CommandKind::OpenErrorLog, window, cx),
            SidebarEvent::OpenSettings => self.open_settings(window, cx),
            SidebarEvent::Collapse => self.toggle_sidebar(cx),
        }
        cx.notify();
    }
}

impl Render for ShelbiWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        match &self.boot {
            Boot::Ready(_) => self.render_shell(&theme, window, cx),
            Boot::InFlight(state) => boot_view::render_progress(state, &theme),
            Boot::Failed(failure) => {
                let weak = cx.entity().downgrade();
                boot_view::render_failure(
                    &boot_view::failure_screen(failure),
                    &theme,
                    move |action, _, cx| {
                        let _ = weak.update(cx, |this, cx| this.screen_action(action, cx));
                    },
                )
            }
        }
    }
}

impl ShelbiWindow {
    fn render_shell(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        #[cfg(feature = "visual-check")]
        let render_start = Instant::now();
        let theme = *theme;
        // The chat attaches the first time the shell paints with it routed
        // (bootstrap has no window to hand it); later changes go through
        // `perform`. Idempotent.
        self.sync_chat(window, cx);
        let window_width = window.viewport_size().width.as_f32();
        let catalog_projects = self
            .switcher_open
            .then(|| cx.try_global::<Catalog>().map(|c| c.projects()).unwrap_or_default());
        // The leading column: the sidebar, or a review's or workspace's panel
        // in its place, both while the push between them runs (clipped to
        // the column).
        let now = (self.clock)();
        self.sync_column(now);
        let frame = self.column.frame(now);
        #[cfg(feature = "visual-check")]
        let probe = self.probe.clone().map(|probe| {
            probe.borrow_mut().begin(render_start, self.column.position(now), frame.animating);
            gpui::canvas(|_, _, _| {}, move |_, _, _, _| probe.borrow_mut().end(Instant::now()))
                .absolute()
                .size_0()
        });
        #[cfg(not(feature = "visual-check"))]
        let probe: Option<gpui::Empty> = None;
        if frame.animating {
            window.request_animation_frame();
        } else if self.column.target() == Occupant::Sidebar {
            self.collapsed_panel_cols = None;
        }
        let column = column_swap::column(
            &frame,
            self.collapsed,
            self.client.sidebar_width(),
            self.collapsed_panel_cols,
        );
        let column_shown = column.is_some();
        let column = column.map(|c| {
            let width = sidebar::width_px(c.width_cols, window_width);
            let layer = |selector: &'static str, l: Layer| {
                div()
                    .debug_selector(move || selector.into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(l.offset * width))
                    .w(px(width))
                    .opacity(l.opacity)
            };
            let sidebar = c.sidebar.map(|l| {
                layer("column-sidebar", l).child(sidebar::render(
                    SidebarProps {
                        model: &self.model,
                        selection: self.client.sidebar_selection(),
                        focused: self.client.focus() == Focus::Sidebar,
                        link: self.link,
                        palette_chord: self.palette_chord.clone(),
                        switcher: catalog_projects
                            .as_deref()
                            .map(|projects| (projects, self.project.as_str())),
                        width,
                        scroll: self.sidebar_scroll.clone(),
                    },
                    &theme,
                    cx,
                ))
            });
            let review = c
                .review
                .zip(self.review.clone())
                .map(|(l, review)| layer("column-review", l).child(review));
            let workspace = c
                .workspace
                .zip(self.workspace.clone())
                .map(|(l, ws)| layer("column-workspace", l).child(ws));
            div()
                .id("leading-column")
                .debug_selector(|| "leading-column".into())
                .relative()
                .flex_none()
                .w(px(width))
                .h_full()
                .overflow_hidden()
                .bg(theme.surface_recessed)
                .children(sidebar)
                .children(review)
                .children(workspace)
        });
        let divider_color = if self.dragging {
            theme.icon
        } else {
            theme.rule
        };
        let handle = column_shown.then(|| {
            // The column's right-edge divider, with a few pixels of grab
            // room. It resizes whichever panel the column shows.
            div()
                .id("sidebar-resize")
                .debug_selector(|| "sidebar-resize".into())
                .w(px(5.0))
                .h_full()
                .flex_none()
                .flex()
                .justify_end()
                .bg(theme.surface_recessed)
                .cursor(CursorStyle::ResizeLeftRight)
                .child(div().w(px(1.0)).h_full().bg(divider_color))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| {
                        this.dragging = true;
                        cx.stop_propagation();
                        cx.notify();
                    }),
                )
        });

        let shell = div()
            .id("shelbi-window")
            .key_context("ShelbiWindow")
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(Self::on_capture_key_down))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if this.dragging {
                    let window_width = window.viewport_size().width.as_f32();
                    let width = sidebar::drag_width_cols(event.position.x.as_f32(), window_width);
                    this.drag_column(width, cx);
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.dragging {
                        this.dragging = false;
                        this.window_state.sidebar_width = this.client.sidebar_width();
                        this.schedule_save(cx);
                        cx.notify();
                    }
                }),
            )
            .flex()
            .flex_row()
            .w_full()
            .font_family("Geist")
            .text_sm()
            .bg(theme.surface)
            .children(column)
            .children(handle)
            .child(self.render_main(&theme, column_shown, cx));

        let relaunch = self.ready().and_then(|r| r.control.relaunch_required());
        let weak = cx.entity().downgrade();
        let layer = self.overlay.as_ref().map(|o| {
            overlay::render_layer(o, window.viewport_size(), &self.keymaps, &theme, cx)
        });
        // Edit ▸ Cut / Copy / Paste / Select All: a focused terminal handles
        // them itself; an open overlay takes paste (and the error log's copy)
        // here; a focused gpui-kit text input gets them forwarded as its own
        // actions. Registered only when there is a target, so the menu items
        // are gray when there isn't.
        let overlay_open = self.overlay.is_some();
        // gpui answers this from the last drawn frame, and asserts (debug
        // builds) that there is one: a window that opens already bootstrapped
        // (a second project's, or the smoke test's) has none on its first.
        let input_focused = !overlay_open
            && self.drawn_once
            && window.is_action_available(&gpui_kit::base::input::Paste, cx);
        self.drawn_once = true;
        div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .on_action(cx.listener(Self::menu_invoke))
            .on_action(cx.listener(|_, _: &menus::Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &menus::Zoom, window, _| window.zoom_window()))
            .when(overlay_open, |d| {
                d.on_action(cx.listener(|this, _: &menus::Paste, window, cx| {
                    this.overlay_paste(window, cx)
                }))
                .on_action(cx.listener(|this, _: &menus::Copy, _, cx| this.overlay_copy(cx)))
            })
            .when(input_focused, |d| {
                use gpui_kit::base::input as kit;
                d.on_action(cx.listener(|_, _: &menus::Cut, window, cx| {
                    window.dispatch_action(Box::new(kit::Cut), cx)
                }))
                .on_action(cx.listener(|_, _: &menus::Copy, window, cx| {
                    window.dispatch_action(Box::new(kit::Copy), cx)
                }))
                .on_action(cx.listener(|_, _: &menus::Paste, window, cx| {
                    window.dispatch_action(Box::new(kit::Paste), cx)
                }))
                .on_action(cx.listener(|_, _: &menus::SelectAll, window, cx| {
                    window.dispatch_action(Box::new(kit::SelectAll), cx)
                }))
            })
            .when_some(relaunch, |col, notice| {
                col.child(boot_view::render_relaunch_bar(&notice, &theme, move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.screen_action(ScreenAction::Relaunch, cx)
                    });
                }))
            })
            .child(shell.flex_1().min_h_0())
            .children(layer)
            .children(probe)
            .into_any_element()
    }
}

/// A bootstrap result that points nowhere: the shell renders, and nothing
/// can reach a real CLI or daemon (the tap stops commands before they would).
#[cfg(any(test, feature = "visual-check"))]
fn inert_ready() -> Ready {
    use crate::platform::control::ControlHandle;
    use crate::platform::discovery::{Candidate, CliSource};
    Ready {
        cli: Candidate {
            path: "/nonexistent/shelbi".into(),
            source: CliSource::Setting,
        },
        cli_version: None,
        env: Arc::new(Default::default()),
        control: ControlHandle::new(
            "/nonexistent/control.sock".into(),
            APP_VERSION.into(),
            APP_VERSION.into(),
        ),
    }
}

/// `--visual-review`'s window: the real shell over a fixture sidebar, with
/// nothing live (no feed, bootstrap or daemon; commands and the view a review
/// returns to are only recorded), reviews over `deps`, and the column
/// transition on `clock`.
#[cfg(feature = "visual-check")]
impl ShelbiWindow {
    pub(crate) fn offscreen(
        project: &str,
        model: SidebarModel,
        deps: ReviewDeps,
        clock: Rc<dyn Fn() -> Instant>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut w = Self::build(project, WindowState::default(), window, cx);
        w.boot = Boot::Ready(Box::new(inert_ready()));
        w.model = model;
        w.tap = Some(Default::default());
        w.review_ops = deps.ops.clone();
        w.review_deps = Some(deps);
        w.clock = clock;
        w.reduce_motion = Rc::new(|| false);
        // A native view with no entity yet: nothing attaches or reads.
        w.client.set_view(View::Machines);
        w.sync_selection();
        w
    }

    pub(crate) fn offscreen_open_review(&mut self, info: ReviewInfo, window: &mut Window, cx: &mut Context<Self>) {
        self.open_review(info, window, cx);
    }

    /// Open `overlay` (the palette when `None`) over the shell.
    pub(crate) fn offscreen_open_overlay(
        &mut self,
        overlay: Option<ActiveOverlay>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match overlay {
            Some(overlay) => self.open_overlay(overlay, window, cx),
            None => self.open_palette(window, cx),
        }
    }

    pub(crate) fn offscreen_close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlay(window, cx);
    }

    pub(crate) fn offscreen_review(&self) -> Option<Entity<ReviewView>> {
        self.review.clone()
    }

    /// What the leading column draws now.
    pub(crate) fn offscreen_column(&mut self) -> column_swap::Frame {
        self.column.frame((self.clock)())
    }

    /// `--bench-push`'s window: [`Self::offscreen`]'s, on the real clock,
    /// with workspaces over `workspace` and every frame reported to `probe`.
    pub(crate) fn bench(
        project: &str,
        model: SidebarModel,
        review: ReviewDeps,
        workspace: WorkspaceDeps,
        probe: Rc<std::cell::RefCell<crate::bench_push::Probe>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut w = Self::offscreen(project, model, review, Rc::new(Instant::now), window, cx);
        w.workspace_deps = Some(workspace);
        w.probe = Some(probe);
        w
    }

    /// Carry out `call` as a command dispatch would.
    pub(crate) fn bench_perform(&mut self, call: HostCall, window: &mut Window, cx: &mut Context<Self>) {
        self.perform(call, window, cx);
    }

    /// The open panel's Back.
    pub(crate) fn bench_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(self.route, Route::Review) {
            self.leave_review(window, cx);
        } else {
            self.leave_workspace(window, cx);
        }
    }
}

#[cfg(test)]
#[path = "app_smoke.rs"]
mod smoke;

#[cfg(test)]
#[path = "app_column_tests.rs"]
mod column_tests;

#[cfg(test)]
mod tests {
    use super::*;

    fn calls(kind: CommandKind) -> Vec<HostCall> {
        let mut calls = Vec::new();
        commands::dispatch_command(&kind, &mut calls);
        calls
    }

    #[test]
    fn the_issues_row_routes_to_the_board_not_a_stub() {
        assert_eq!(
            calls(CommandKind::ShowView(View::Issues)),
            [HostCall::ShowView(View::Issues)]
        );
    }

    #[test]
    fn the_orchestrator_row_routes_to_the_chat() {
        assert_eq!(
            calls(CommandKind::ShowView(View::Session("orch".into()))),
            [HostCall::ShowView(View::Session("orch".into()))]
        );
        assert!(is_chat(&View::Session("orch".into())));
        assert!(!is_chat(&View::Session("alpha".into())));
        assert!(!is_chat(&View::Issues));
    }

    #[test]
    fn a_workspace_row_routes_to_the_workspace_view() {
        // Selecting a workspace (its row, or the palette's `workspace:<name>`)
        // shows its session view, which the window draws as the Workspace view.
        assert_eq!(
            calls(CommandKind::FocusWorkspace {
                workspace: "alpha".into()
            }),
            [HostCall::ShowView(View::Session("alpha".into()))]
        );
        assert_eq!(workspace_name(&View::Session("alpha".into())), Some("alpha"));
        assert_eq!(workspace_name(&View::Session("orch".into())), None);
        assert_eq!(workspace_name(&View::Issues), None);
    }

    #[test]
    fn a_focused_terminal_keeps_every_key_but_the_global_chords() {
        let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        let chord = |spec: &str| {
            let ks = gpui::Keystroke::parse(spec).unwrap();
            chord_from_keystroke(&ks).unwrap()
        };
        // Ctrl+H back to the sidebar, Ctrl+L to the main area, the palette,
        // the Zen toggle.
        for spec in ["ctrl-h", "ctrl-l", "ctrl-space", "ctrl-p", "cmd-k", "alt-z"] {
            assert!(takes_from_terminal(&keymaps, chord(spec)), "{spec}");
        }
        // Typing, Backspace (never mistaken for Ctrl+H), Shift+Enter, other
        // control keys reach the session.
        for spec in ["h", "backspace", "shift-enter", "enter", "ctrl-c", "ctrl-a", "alt-b"] {
            assert!(!takes_from_terminal(&keymaps, chord(spec)), "{spec}");
        }
    }

    #[test]
    fn the_review_panels_decisions_run_through_the_registry_and_the_daemon() {
        // The Review view emits these for Approve and Reject.
        assert_eq!(
            calls(CommandKind::ApproveReview { id: "t1".into() }),
            [HostCall::ApproveReview("t1".into())]
        );
        assert_eq!(
            calls(CommandKind::RejectReview {
                id: "t1".into(),
                reason: String::new()
            }),
            [HostCall::AskRejectReason("t1".into())],
            "Reject asks for the reason first"
        );
        assert_eq!(
            calls(CommandKind::RejectReview {
                id: "t1".into(),
                reason: "flaky test".into()
            }),
            [HostCall::RejectReview("t1".into(), "flaky test".into())]
        );
        // All three reach the daemon, so they wait for bootstrap and are
        // refused while a relaunch is pending.
        for kind in [
            CommandKind::ApproveReview { id: "t1".into() },
            CommandKind::RejectReview {
                id: "t1".into(),
                reason: String::new(),
            },
            CommandKind::LoadReview { task_id: "t1".into() },
        ] {
            assert!(commands::sends_to_daemon(&kind), "{kind:?}");
        }
        // A review row opens the review flow.
        assert_eq!(
            calls(CommandKind::LoadReview { task_id: "t1".into() }),
            [HostCall::LoadReview("t1".into())]
        );
    }

    #[test]
    fn board_mutations_reach_the_board() {
        for kind in [
            CommandKind::MoveIssue {
                id: "t1".into(),
                to_status: "done".into(),
            },
            CommandKind::StartIssue { id: "t1".into() },
            CommandKind::EditIssue { id: "t1".into() },
        ] {
            assert_eq!(calls(kind.clone()), [HostCall::IssueCommand(kind)]);
        }
    }
}
