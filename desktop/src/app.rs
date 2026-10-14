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
//! global chords (the focus moves and the palette chord, plus Ctrl+Space) in
//! the capture phase, so they never reach the session.
//!
//! The Issues nav row routes the main area to the native board
//! ([`IssuesView`], `dt-issues-board`), built for the window's project the
//! first time it is shown. A project switch opens (or focuses) the target's
//! own window, so every project gets its own board.

use std::collections::HashMap;

use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, svg, App, Bounds, Context, CursorStyle, Entity, FocusHandle,
    Global, KeyDownEvent, MouseButton, MouseMoveEvent, PromptLevel, SharedString, Subscription, Task,
    TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions,
};
use shelbi_app::nav::{ClientState, Focus, View};
use shelbi_app::view::{SidebarModel, SidebarTarget};
use shelbi_app::CommandKind;
use shelbi_proto::control::DetachSelector;

use crate::platform::version::APP_VERSION;
use crate::platform::{
    relaunch, Bootstrap, BootstrapEvent, BootstrapState, Failure, Ready, SystemPlatform,
};
use crate::views::bootstrap::{self as boot_view, ScreenAction};
use crate::views::chat::{ChatDeps, ChatEvent, ChatView};
use shelbi_state::keymap::{
    format_chord, DisplayStyle, GlobalAction, Key, Keymaps, SidebarAction,
};

use crate::assets::icons;
use crate::commands::{self, CommandHost};
use crate::keys::chord_from_keystroke;
use crate::projects::{self, Catalog};
use crate::refresh::{ControlWatcher, DaemonLink, FeedEvent, ProjectFeed, SessionNoticeKind};
use crate::theme::{self, Theme};
use crate::views::issues::{IssuesEvent, IssuesView};
use crate::views::settings::SettingsView;
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

/// Bind the window-level shortcuts.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        gpui::KeyBinding::new("ctrl-cmd-s", ToggleSidebar, None),
        gpui::KeyBinding::new("cmd-,", OpenSettings, None),
        gpui::KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
}

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
}

/// A command dispatch asked the window for, collected first and then carried
/// out with the window and its context in hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostCall {
    NotImplemented(String),
    ShowView(View),
    IssueCommand(CommandKind),
    SwitchProject(String),
    OpenSwitcher,
    Detach,
    DetachOthers,
    QuitProject,
    QuitShelbi,
}

impl CommandHost for Vec<HostCall> {
    fn not_implemented(&mut self, command_id: &str) {
        self.push(HostCall::NotImplemented(command_id.to_string()));
    }
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
    /// The "Not implemented yet" (or failure) banner.
    banner: Option<SharedString>,
    /// A transient status note (the orchestrator coming up, a failure).
    status: Option<SharedString>,
    focus_handle: FocusHandle,
    keymaps: Keymaps,
    palette_chord: String,
    /// The latest title each session reported.
    session_titles: HashMap<String, String>,
    window_state: WindowState,
    /// Bumped on every state change worth saving; a debounced save only
    /// writes if no newer change followed it.
    save_generation: u64,
    _feed: Option<ProjectFeed>,
    _feed_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl ShelbiWindow {
    pub fn new(
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

        let shared_ready = cx.try_global::<Shell>().and_then(|s| s.ready.clone());
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
            banner: None,
            status: None,
            focus_handle,
            keymaps,
            session_titles: HashMap::new(),
            window_state,
            save_generation: 0,
            _feed: None,
            _feed_task: None,
            _subscriptions: Vec::new(),
        };
        this.sync_selection();
        // The feed and keymaps only read local state: start them now, so the
        // shell has data the moment bootstrap is done.
        this.start_feed(cx);
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
        window.set_window_title(&this.model.project_label);
        this
    }

    /// Spawn the project's feed and pump its events into this view.
    fn start_feed(&mut self, cx: &mut Context<Self>) {
        let (feed, mut events) = ProjectFeed::spawn(&self.project);
        if let Some(control) = cx.try_global::<Shell>().and_then(|s| s.control.clone()) {
            control.watch(&self.project, &feed);
        }
        self._feed = Some(feed);
        self._feed_task = Some(cx.spawn(async move |this, cx| {
            while let Some(event) = events.next().await {
                if this.update(cx, |this, cx| this.apply_feed(event, cx)).is_err() {
                    break;
                }
            }
        }));
    }

    fn apply_feed(&mut self, event: FeedEvent, cx: &mut Context<Self>) {
        match event {
            FeedEvent::Snapshot(snapshot) => {
                if let Some(model) = snapshot.sidebar {
                    self.model = model;
                    self.sync_selection();
                }
            }
            FeedEvent::Link(link) => self.link = Some(link),
            FeedEvent::Session(notice) => match notice.kind {
                SessionNoticeKind::Title(title) => {
                    self.session_titles.insert(notice.session, title);
                }
                SessionNoticeKind::Exited => {
                    self.session_titles.remove(&notice.session);
                }
            },
        }
        cx.notify();
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
            HostCall::NotImplemented(id) => {
                self.banner = Some(commands::not_implemented_message(&id).into());
            }
            HostCall::ShowView(view) => {
                self.route = Route::View;
                let issues = view == View::Issues;
                let chat = is_chat(&view);
                self.client.set_view(view);
                self.client.focus_main();
                self.banner = None;
                if issues {
                    self.ensure_issues(window, cx);
                }
                if chat {
                    // Selecting it again while it is failed or given up
                    // re-opens it (a relaunch with a fresh budget).
                    let chat = self.ensure_chat(window, cx);
                    chat.update(cx, |chat, cx| chat.show(cx));
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
        }
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
            cx.new(|cx| ChatView::new(&project, handle, ChatDeps::live(&project), sink, cx));
        let sub = cx.subscribe(&chat, |this, _, ev: &ChatEvent, cx| match ev {
            // A click in the terminal moves the window's focus model to the
            // main area, so the sidebar stops taking keys.
            ChatEvent::Focused => {
                this.client.focus_main();
                cx.notify();
            }
        });
        self._subscriptions.push(sub);
        self.chat = Some(chat.clone());
        chat
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
    }

    /// Whether the chat's terminal holds keyboard focus.
    fn chat_focused(&self, window: &Window, cx: &App) -> bool {
        self.chat
            .as_ref()
            .is_some_and(|chat| chat.read(cx).contains_focus(window, cx))
    }

    /// Hand keyboard focus to what the main area shows: the board and the
    /// chat take it (their own keys), anything else leaves it on the window.
    fn focus_main_view(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match (&self.route, self.client.view(), &self.issues, &self.chat) {
            (Route::View, View::Issues, Some(issues), _) => {
                issues.update(cx, |v, _| v.request_focus());
            }
            (Route::View, view, _, Some(chat)) if is_chat(view) => {
                let target = chat.read(cx).focus_target(cx);
                window.focus(&target, cx);
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
            let view = cx.new(|cx| SettingsView::new(window, cx));
            self.route = Route::Settings(view);
        }
        self.sync_chat(window, cx);
        self.banner = None;
        self.client.focus_main();
        self.focus_main_view(window, cx);
        cx.notify();
    }

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.collapsed = !self.collapsed;
        if self.collapsed {
            self.client.focus_main();
        }
        self.window_state.sidebar_collapsed = self.collapsed;
        self.schedule_save(cx);
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

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let chord = chord_from_keystroke(&event.keystroke);
        tracing::trace!(keystroke = ?event.keystroke, ?chord, focus = ?self.client.focus(), "key down");
        let Some(chord) = chord else {
            return;
        };
        if chord.code == Key::Esc && self.switcher_open {
            self.switcher_open = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        match self.keymaps.global.dispatch(chord) {
            Some(GlobalAction::FocusSidebar) => {
                if !self.collapsed {
                    self.client.focus_sidebar();
                    // Take keys back from a main view that holds focus.
                    self.focus_handle.focus(window, cx);
                }
                cx.stop_propagation();
                cx.notify();
                return;
            }
            Some(GlobalAction::FocusMain) => {
                self.client.focus_main();
                self.focus_main_view(window, cx);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            // The palette also always opens on Ctrl+Space, the plan's reserved
            // key, as in the TUI.
            _ if is_reserved_palette_chord(chord) => {
                // dt-palette-overlays
                self.banner = Some(commands::not_implemented_message("action:palette").into());
                cx.stop_propagation();
                cx.notify();
                return;
            }
            Some(GlobalAction::OpenPalette) => {
                // dt-palette-overlays
                self.banner = Some(commands::not_implemented_message("action:palette").into());
                cx.stop_propagation();
                cx.notify();
                return;
            }
            _ => {}
        }
        if self.client.focus() != Focus::Sidebar || self.collapsed {
            // The main area's views take their own keys once they land.
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

    fn render_main(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.route {
            Route::Settings(view) => div().size_full().child(view.clone()).into_any_element(),
            Route::View => {
                let view = self.client.view();
                match (commands::view_stub_id(view), &self.issues) {
                    (Some(id), _) => div()
                        .debug_selector(|| "main-stub".into())
                        .flex()
                        .flex_col()
                        .flex_1()
                        .items_center()
                        .justify_center()
                        .gap_1()
                        .text_color(theme.muted)
                        .child(commands::not_implemented_message(&id))
                        .when_some(self.session_title(view), |d, title| {
                            d.child(div().text_xs().child(title))
                        })
                        .into_any_element(),
                    (None, _) if is_chat(view) && self.chat.is_some() => div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .children(self.chat.clone())
                        .into_any_element(),
                    (None, Some(issues)) if *view == View::Issues => div()
                        .flex()
                        .flex_1()
                        .min_h_0()
                        .min_w_0()
                        .child(issues.clone())
                        .into_any_element(),
                    (None, _) => div().into_any_element(),
                }
            }
        };
        let notice = |text: SharedString, color| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .justify_between()
                .px_4()
                .py_2()
                .bg(theme.selection)
                .text_color(color)
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
            .bg(theme.canvas)
            .when(self.collapsed, |main| {
                main.child(
                    div().h(px(sidebar::TRAFFIC_LIGHT_INSET)).pl(px(80.0)).pt(px(8.0)).child(
                        div()
                            .id("expand-sidebar")
                            .debug_selector(|| "expand-sidebar".into())
                            .p_1()
                            .w_auto()
                            .self_start()
                            .rounded_md()
                            .cursor_pointer()
                            .hover(|s| s.bg(theme.search))
                            .child(svg().path(icons::BARS_3).size(px(16.0)).text_color(theme.muted))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                    ),
                )
            })
            .when_some(self.model.session_lost.clone(), |main, lost| {
                main.child(notice(lost.into(), theme.red))
            })
            .when_some(self.status.clone(), |main, status| {
                main.child(notice(status, theme.muted))
            })
            .when_some(self.banner.clone(), |main, message| {
                main.child(
                    notice(message, theme.yellow).child(
                        div()
                            .id("dismiss-banner")
                            .cursor_pointer()
                            .child(svg().path(icons::X_MARK).size(px(16.0)).text_color(theme.muted))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.banner = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(body)
    }

    fn session_title(&self, view: &View) -> Option<String> {
        match view {
            View::Session(name) => self.session_titles.get(name).cloned(),
            _ => None,
        }
    }
}

/// Whether `view` is the orchestrator chat.
fn is_chat(view: &View) -> bool {
    matches!(view, View::Session(name) if name == "orch")
}

/// Whether the window takes `chord` away from a focused terminal: the focus
/// moves (Ctrl+H / Ctrl+L by default), the palette chord, and Ctrl+Space.
/// Everything else is the session's.
fn takes_from_terminal(keymaps: &Keymaps, chord: shelbi_state::keymap::KeyChord) -> bool {
    matches!(
        keymaps.global.dispatch(chord),
        Some(GlobalAction::FocusSidebar | GlobalAction::FocusMain | GlobalAction::OpenPalette)
    ) || is_reserved_palette_chord(chord)
}

/// Ctrl+Space, the plan's permanently reserved palette key: it opens the
/// palette from anywhere and is never forwarded to a session.
fn is_reserved_palette_chord(chord: shelbi_state::keymap::KeyChord) -> bool {
    chord == shelbi_state::keymap::KeyChord::new(Key::Char(' '), shelbi_state::keymap::Mods::CONTROL)
}

/// The palette-open chord as the search box shows it (`⌃P`).
fn palette_chord(keymaps: &Keymaps) -> String {
    keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .map(|c| format_chord(c, DisplayStyle::Mac))
        .unwrap_or_default()
}

impl SidebarHost for ShelbiWindow {
    fn sidebar_event(&mut self, event: SidebarEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            SidebarEvent::Activate(ordinal) => {
                let count = self.selectable_count();
                self.client.select(ordinal, count);
                self.activate_selection(window, cx);
            }
            // dt-palette-overlays
            SidebarEvent::OpenSearch => {
                self.banner = Some(commands::not_implemented_message("action:palette").into());
            }
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
        let theme = *theme;
        // The chat attaches the first time the shell paints with it routed
        // (bootstrap has no window to hand it); later changes go through
        // `perform`. Idempotent.
        self.sync_chat(window, cx);
        let window_width = window.viewport_size().width.as_f32();
        let catalog_projects = self
            .switcher_open
            .then(|| cx.try_global::<Catalog>().map(|c| c.projects()).unwrap_or_default());
        let sidebar = (!self.collapsed).then(|| {
            sidebar::render(
                SidebarProps {
                    model: &self.model,
                    selection: self.client.sidebar_selection(),
                    focused: self.client.focus() == Focus::Sidebar,
                    link: self.link,
                    palette_chord: self.palette_chord.clone(),
                    switcher: catalog_projects
                        .as_deref()
                        .map(|projects| (projects, self.project.as_str())),
                    width: sidebar::width_px(self.client.sidebar_width(), window_width),
                },
                &theme,
                cx,
            )
        });
        let divider_color = if self.dragging {
            theme.divider_active
        } else {
            theme.divider
        };
        let handle = (!self.collapsed).then(|| {
            // The right-edge divider, with a few pixels of grab room.
            div()
                .id("sidebar-resize")
                .debug_selector(|| "sidebar-resize".into())
                .w(px(5.0))
                .h_full()
                .flex_none()
                .flex()
                .justify_end()
                .bg(theme.background)
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
                    if width != this.client.sidebar_width() {
                        this.client.set_sidebar_width(width);
                        cx.notify();
                    }
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
            .bg(theme.canvas)
            .children(sidebar)
            .children(handle)
            .child(self.render_main(&theme, cx));

        let relaunch = self.ready().and_then(|r| r.control.relaunch_required());
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .size_full()
            .when_some(relaunch, |col, notice| {
                col.child(boot_view::render_relaunch_bar(&notice, &theme, move |_, cx| {
                    let _ = weak.update(cx, |this, cx| {
                        this.screen_action(ScreenAction::Relaunch, cx)
                    });
                }))
            })
            .child(shell.flex_1().min_h_0())
            .into_any_element()
    }
}

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
        // The main area draws the board for it, not the not-implemented stub.
        assert_eq!(commands::view_stub_id(&View::Issues), None);
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
    fn a_focused_terminal_keeps_every_key_but_the_global_chords() {
        let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
        let chord = |spec: &str| {
            let ks = gpui::Keystroke::parse(spec).unwrap();
            chord_from_keystroke(&ks).unwrap()
        };
        // Ctrl+H back to the sidebar, Ctrl+L to the main area, the palette.
        for spec in ["ctrl-h", "ctrl-l", "ctrl-space", "ctrl-p"] {
            assert!(takes_from_terminal(&keymaps, chord(spec)), "{spec}");
        }
        // Typing, Backspace (never mistaken for Ctrl+H), Shift+Enter, other
        // control keys reach the session.
        for spec in ["h", "backspace", "shift-enter", "enter", "ctrl-c", "ctrl-a", "alt-b"] {
            assert!(!takes_from_terminal(&keymaps, chord(spec)), "{spec}");
        }
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
