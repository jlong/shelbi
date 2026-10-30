//! The root view of a Shelbi window: the sidebar | main area split.
//!
//! A window shows one project at a time and switches projects in place
//! (`dt-switch-project-in-window`): the sidebar's switcher, the palette, the
//! File menu and a notification click all retarget the current window rather
//! than opening another. File ▸ New Window (⌘⇧N) opens a second window, which
//! switches on its own; two windows may show the same project and share its
//! feed ([`crate::feeds`]). [`Shell`] keeps the open windows and their slots.
//!
//! A window owns its project's `shelbi_app::nav::ClientState`, the source of
//! truth for what the main area shows, where keyboard focus sits and the
//! sidebar width (the sidebar's selected row is derived from what the main
//! area shows, `dt-back-syncs-sidebar`); its sidebar model, fed by the project's
//! shared [`ProjectFeed`](crate::refresh::ProjectFeed); and its persisted
//! [`WindowState`] (per window slot, so a window keeps its frame across
//! switches).
//!
//! A switch tears the window's project-bound views down (chat, board,
//! activity, machines, review and workspace panels, their sessions, the feed
//! subscription), saving where the outgoing project was left
//! ([`ProjectViewState`]), and paints the target's prefetched sidebar skeleton
//! on the next frame with its own saved view state. The heavy part (building
//! the main area's view and attaching its sessions) waits for that frame to be
//! drawn ([`SwitchHold`]), the way a panel's content waits for its push
//! (`dt-instant-panel-push`). Quitting the project a window shows switches it
//! to another open project, or to the empty-state chooser.
//!
//! The app's first window runs bootstrap (`crate::platform`: the login
//! environment, finding the CLI, starting the daemon, the version check) and
//! shows its progress and failure screens until it is `Ready`; the `Ready`
//! result is kept in [`Shell`], so every later window (File ▸ New Window)
//! skips straight to the shell. The shell's first frame paints the target project's
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
//! (`dt-workspace-sidebar`) while the workspace works a task; its Back
//! returns to the view the workspace was opened over. An idle workspace (no
//! task) pushes nothing: the sidebar stays and the main area shows its idle
//! page (`dt-figma-terminal-screens`). Going from a workspace to a review, or back, passes through
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
//! it was (scroll, collapsed) and keys on it, its selection on that view's
//! row.
//!
//! The Issues nav row routes the main area to the native board
//! ([`IssuesView`], `dt-issues-board`), built for the window's project the
//! first time it is shown, and rebuilt for the new project after a switch.

use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use futures::StreamExt;
use gpui::{
    actions, div, prelude::*, px, AnyWindowHandle, App, Bounds, Context, CursorStyle, Entity,
    FocusHandle, Global, KeyDownEvent, MouseButton, MouseMoveEvent, PromptLevel, ScrollHandle,
    SharedString, Subscription, Task, TitlebarOptions, Window, WindowBounds, WindowHandle,
    WindowId, WindowOptions,
};
use shelbi_app::add_project_wizard::Origin;
use shelbi_app::command::{EditItem, ProjectItem};
use shelbi_app::nav::{ClientState, Focus, View};
use shelbi_app::overlay::error_log::Viewer;
use shelbi_app::changelog::ChangelogEntryView;
use shelbi_app::overlay::palette::ProjectEntry as PaletteProject;
use shelbi_app::overlay::palette_search::{issue_docs, IssueDoc, PaletteSearch, SearchCorpus};
use shelbi_app::overlay::review_confirm::Slot;
use shelbi_app::review::{LoadStep, ReviewInfo, ReviewLoader};
use shelbi_app::view::{SidebarModel, SidebarTarget};
use shelbi_app::{CommandKind, CommandRegistry, EditTarget, Mutation};
use shelbi_proto::control::DetachSelector;
use shelbi_state::DesktopNotifications;

use crate::platform::version::APP_VERSION;
use crate::platform::{
    relaunch, Bootstrap, BootstrapEvent, BootstrapState, Failure, Ready, SystemPlatform,
};
use crate::views::bootstrap::{self as boot_view, ScreenAction};
use crate::views::chat::{ChatDeps, ChatEvent, ChatView};
use crate::views::update::{self as update_view, UpdateState};
use crate::views::workspace_dialog::{Body as DialogBody, WorkspaceDialog, WorkspaceDialogEvent};
use crate::views::connect_remote::{ConnectDialog, ConnectEvent};
use crate::views::feedback::{FeedbackDialog, FeedbackEvent};
use shelbi_app::settings::workspaces::Intent;
use shelbi_state::keymap::{
    DisplayStyle, GlobalAction, Key, Keymaps, SidebarAction,
};

use crate::assets::{heroicon, icons};
use crate::commands::{self, CommandHost};
use crate::feeds::{FeedHub, FeedSubscription};
use crate::elements::terminal::{ResizeHold, TerminalSettings};
use crate::keys::{
    chord_from_keystroke, font_zoom, is_new_window, nav_chord, overlay_chord, paste_chords, NavChord,
};
use crate::menus;
use crate::notify;
use crate::projects::{self, Catalog, ProjectEntry};
use crate::refresh::{ControlWatcher, DaemonLink, FeedEvent, SessionNoticeKind};
use crate::theme::motion::{SharedTrack, SlideTrack};
use crate::theme::style::{self, IconButtonSize};
use crate::theme::{self, Hue, Theme};
use crate::views::activity::{ActivityEvent, ActivityView};
use crate::views::activity_reader::ActivityReader;
use crate::views::column_swap::{self, ColumnSwap, Layer, Motion, Occupant};
use crate::views::sidebar_reveal::{self, SidebarReveal};
use crate::views::issues::{IssuesEvent, IssuesView};
use crate::views::pane_session::MainShows;
use crate::views::session_view::SessionDeps;
use crate::views::machines::{MachinesEvent, MachinesView};
use crate::views::changelog::{ChangelogEvent, ChangelogView};
use crate::views::review::{ReviewDeps, ReviewEvent, ReviewOps, ReviewView};
use crate::views::overlay::{self, ActiveOverlay, OverlayEvent, OverlayHost};
use crate::views::workspace::{WorkspaceDeps, WorkspaceEvent, WorkspaceView};
use crate::views::settings::{SettingsEvent, SettingsView};
use crate::views::settings_nav::{self, PageLook, SettingsNav, SettingsPage};
use crate::views::sidebar::{self, SidebarEvent, SidebarHost, SidebarProps};
use crate::window_state::{self, Frame, LaunchWindow, ProjectViewState, WindowSession, WindowState};
use crate::theme::style::{Type, Typography};

actions!(
    shelbi_window,
    [
        /// Show or hide the sidebar (⌃⌘S, the macOS "Show Sidebar" shortcut).
        ToggleSidebar,
        /// Open the settings view (⌘,).
        OpenSettings,
        /// Close this window (⌘W). Its project keeps running.
        CloseWindow,
    ]
);

const DEFAULT_WIDTH: f32 = 1200.0;
const DEFAULT_HEIGHT: f32 = 800.0;
/// Shown while the project's orchestrator comes up.
const STARTUP_STATUS: &str = "Starting the orchestrator…";
/// How far a new window sits off the one it was opened from.
const CASCADE: f32 = 28.0;
/// How wide the column edge's grab room is: the column's last five points
/// (the Review rule's own point and the column's last four).
const RESIZE_GRAB: f32 = 5.0;
/// The title of a window with no project (the empty-state chooser).
const CHOOSER_TITLE: &str = "Shelbi";
/// How often an open palette takes the search hits that landed.
const PALETTE_POLL: std::time::Duration = std::time::Duration::from_millis(30);

/// App-wide shell state: the open windows (each in its slot), the
/// attached-client id, the control-socket watcher feeds register with,
/// bootstrap's result once a window has reached it, and the window slots
/// ([`WindowSession`]) a launch restores.
#[derive(Default)]
pub struct Shell {
    windows: Vec<OpenWindow>,
    pub client_id: String,
    pub control: Option<ControlWatcher>,
    ready: Option<Ready>,
    session: WindowSession,
    /// Bumped per session change; a save writes only the newest.
    session_generation: u64,
    /// Whether the session and view states are saved to disk (the app's
    /// shell; a test's default one saves nothing).
    persist: bool,
    /// Test seam: windows open inert (no bootstrap, feed or disk reads).
    #[cfg(test)]
    inert_windows: bool,
}

/// An open window and the slot its frame is saved in.
#[derive(Debug, Clone, Copy)]
struct OpenWindow {
    handle: WindowHandle<ShelbiWindow>,
    slot: usize,
}

impl Global for Shell {}

impl Shell {
    pub fn new(client_id: String, control: Option<ControlWatcher>, session: WindowSession) -> Self {
        Shell {
            windows: Vec::new(),
            client_id,
            control,
            ready: None,
            session,
            session_generation: 0,
            persist: true,
            #[cfg(test)]
            inert_windows: false,
        }
    }
}

/// The app's attached-client id (a fresh one before the shell exists).
pub fn client_id(cx: &App) -> String {
    cx.try_global::<Shell>()
        .map(|s| s.client_id.clone())
        .unwrap_or_else(shelbi_client::fresh_client_id)
}

/// Bootstrap reached Shelbi outside a project window (the first launch's
/// setup): windows opened from now on reuse it instead of bootstrapping.
pub fn share_ready(ready: Ready, cx: &mut App) {
    if cx.has_global::<Shell>() {
        cx.global_mut::<Shell>().ready = Some(ready);
    }
}

/// The CLI the app runs, once bootstrap found it, and whether a terminal
/// finds it by name (`Ready::on_login_path`).
pub fn running_cli(cx: &App) -> Option<(std::path::PathBuf, bool)> {
    let ready = cx.try_global::<Shell>()?.ready.as_ref()?;
    Some((ready.cli.path.clone(), ready.on_login_path))
}

/// The shared bootstrap result, once one is (tests).
#[cfg(test)]
pub fn shared_ready(cx: &App) -> Option<Ready> {
    cx.try_global::<Shell>().and_then(|s| s.ready.clone())
}

/// Open the windows a launch restores (`window_state::launch_plan`).
pub fn open_launch_windows(plan: Vec<LaunchWindow>, cx: &mut App) {
    for w in plan {
        open_window(&w.project, w.slot, w.state, None, cx);
    }
}

/// The open Shelbi windows, in the order they opened.
fn shelbi_windows(cx: &App) -> Vec<WindowHandle<ShelbiWindow>> {
    cx.try_global::<Shell>()
        .map(|s| s.windows.iter().map(|w| w.handle).collect())
        .unwrap_or_default()
}

/// The frontmost Shelbi window: the key one, else the top of the platform's
/// window stack, else the first opened.
fn frontmost(cx: &App) -> Option<WindowHandle<ShelbiWindow>> {
    let ours = shelbi_windows(cx);
    let find = |any: AnyWindowHandle| {
        ours.iter()
            .find(|h| h.window_id() == any.window_id())
            .copied()
    };
    cx.active_window()
        .and_then(find)
        .or_else(|| cx.window_stack()?.into_iter().find_map(find))
        .or_else(|| ours.first().copied())
}

/// The windows showing `project`, in the order they opened. `me`, the window
/// asking (mid-update, so it can't be read), counts as showing it.
fn windows_showing(project: &str, me: Option<WindowId>, cx: &App) -> Vec<WindowHandle<ShelbiWindow>> {
    shelbi_windows(cx)
        .into_iter()
        .filter(|h| Some(h.window_id()) == me || h.read(cx).is_ok_and(|w| w.shows(project)))
        .collect()
}

/// File ▸ New Window: another window on `project`, cascaded off the
/// frontmost one (whose sidebar width and collapse it takes), in the lowest
/// free slot. `None` when the window couldn't open (logged).
pub fn open_new_window(project: &str, cx: &mut App) -> Option<WindowHandle<ShelbiWindow>> {
    let mut state = WindowState::default();
    let mut cascade = None;
    if let Some(from) = frontmost(cx) {
        let _ = from.update(cx, |w, window, _| {
            state.sidebar_width = w.window_state.sidebar_width;
            state.sidebar_collapsed = w.window_state.sidebar_collapsed;
            cascade = Some(window.window_bounds().get_bounds());
        });
    }
    let taken: Vec<usize> = cx
        .try_global::<Shell>()
        .map(|s| s.windows.iter().map(|w| w.slot).collect())
        .unwrap_or_default();
    let slot = WindowSession::free_slot(&taken);
    let bounds = cascade.map(|mut b| {
        b.origin.x += px(CASCADE);
        b.origin.y += px(CASCADE);
        b
    });
    open_window(project, slot, state, bounds, cx)
}

/// A project picked on the launch window: the window already showing it
/// comes up, else a new window opens on it.
pub fn open_project_window(slug: &str, cx: &mut App) {
    if let Some(handle) = windows_showing(slug, None, cx).first().copied() {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
        crate::views::launch::project_window_opened(cx);
        return;
    }
    if open_new_window(slug, cx).is_some() {
        cx.activate(true);
    }
}

/// The Add Project wizard created `slug` and the user pressed Okay (the
/// wizard has re-read the catalog first): it opens on its Orchestrator. A
/// window already showing it (an earlier Okay got that far) comes up, else
/// the window the wizard was opened from switches to it, else (the first
/// launch, or that window has since closed) a new window opens on it. Where
/// a project with this id was left before doesn't come back. An error leaves
/// everything as it was, for the wizard to show and try again.
pub fn open_added_project(slug: &str, from: Option<WindowHandle<ShelbiWindow>>, cx: &mut App) -> Result<(), String> {
    let catalog = cx.try_global::<Catalog>().cloned();
    if let Some(catalog) = &catalog {
        if !catalog.lists(slug) {
            return Err(format!("Shelbi can’t find the project “{slug}” yet."));
        }
        catalog.forget_view(slug);
    }
    let shown = windows_showing(slug, None, cx).into_iter().next();
    for handle in shown.into_iter().chain(from) {
        if handle.update(cx, |w, window, cx| w.project_added(slug, window, cx)).is_ok() {
            return Ok(());
        }
    }
    let handle = open_new_window(slug, cx).ok_or_else(|| format!("Shelbi couldn’t open a window for “{slug}”."))?;
    handle
        .update(cx, |w, window, cx| w.project_added(slug, window, cx))
        .map_err(|e| e.to_string())
}

/// A clicked notification: the window already showing its project comes up
/// (else the frontmost one, switched to it) and opens the item.
pub fn open_notice_tag(tag: &str, cx: &mut App) {
    let Some((project, target)) = notify::parse_tag(tag) else {
        return;
    };
    let Some(handle) = windows_showing(&project, None, cx)
        .first()
        .copied()
        .or_else(|| frontmost(cx))
    else {
        return;
    };
    let _ = handle.update(cx, |this, window, cx| {
        window.activate_window();
        if !this.shows(&project) {
            this.switch_to(&project, window, cx);
        }
        this.open_notice(target, window, cx);
    });
}

/// Open a window on `project` in `slot`: at `bounds` when given (a cascade),
/// else at the slot's saved frame, else centered.
fn open_window(
    project: &str,
    slot: usize,
    state: WindowState,
    bounds: Option<Bounds<gpui::Pixels>>,
    cx: &mut App,
) -> Option<WindowHandle<ShelbiWindow>> {
    let bounds = bounds.unwrap_or_else(|| match state.frame {
        Some(f) if f.width >= 400.0 && f.height >= 300.0 => Bounds {
            origin: gpui::point(px(f.x), px(f.y)),
            size: gpui::size(px(f.width), px(f.height)),
        },
        _ => {
            let mut b =
                Bounds::centered(None, gpui::size(px(DEFAULT_WIDTH), px(DEFAULT_HEIGHT)), cx);
            // Cascade a new window off the ones already open, so it doesn't
            // land exactly on top of them.
            let offset = px(CASCADE * cx.windows().len() as f32);
            b.origin.x += offset;
            b.origin.y += offset;
            b
        }
    });
    if !cx.has_global::<Shell>() {
        cx.set_global(Shell::default());
    }
    #[cfg(test)]
    let inert = cx.global::<Shell>().inert_windows;
    let project_name = project.to_string();
    let opened = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(project.to_string().into()),
                // The sidebar runs under a transparent title bar, the usual
                // shape for a macOS sidebar app.
                appears_transparent: true,
                traffic_light_position: Some(sidebar::traffic_light_position()),
            }),
            app_id: Some("co.32pixels.shelbi".into()),
            ..Default::default()
        },
        move |window, cx| {
            cx.new(|cx| {
                #[cfg(test)]
                if inert {
                    return ShelbiWindow::inert(&project_name, slot, state, window, cx);
                }
                ShelbiWindow::new(&project_name, slot, state, window, cx)
            })
        },
    );
    match opened {
        Ok(handle) => {
            cx.global_mut::<Shell>().windows.push(OpenWindow { handle, slot });
            let _ = handle.update(cx, |w, _, cx| w.record_slot(cx));
            // A project is open: the launch window, if up, is done.
            crate::views::launch::project_window_opened(cx);
            Some(handle)
        }
        Err(e) => {
            tracing::error!(project, error = %e, "could not open a window");
            None
        }
    }
}

/// A window closed (`on_window_closed`): forget it. With others still open,
/// its slot isn't reopened at the next launch; the last window closing quits
/// the app with its slot still open, so the next launch brings it back.
pub fn window_closed(id: WindowId, cx: &mut App) {
    theme::window::forget(id);
    if !cx.has_global::<Shell>() {
        return;
    }
    let shell = cx.global_mut::<Shell>();
    let Some(at) = shell.windows.iter().position(|w| w.handle.window_id() == id) else {
        return;
    };
    let closed = shell.windows.remove(at);
    if !shell.windows.is_empty() {
        shell.session.close(closed.slot);
        save_session(cx);
    }
}

/// Save the window slots off the UI thread. Only the newest of overlapping
/// saves writes.
fn save_session(cx: &mut App) {
    if !persists(cx) {
        return;
    }
    let shell = cx.global_mut::<Shell>();
    shell.session_generation += 1;
    let generation = shell.session_generation;
    let session = shell.session.clone();
    cx.background_executor()
        .spawn(async move {
            static SAVED: std::sync::Mutex<u64> = std::sync::Mutex::new(0);
            let mut saved = SAVED.lock().unwrap_or_else(|p| p.into_inner());
            if generation <= *saved {
                return;
            }
            match session.save() {
                Ok(()) => *saved = generation,
                Err(e) => tracing::warn!(error = %e, "could not save the window slots"),
            }
        })
        .detach();
}

/// Whether the app saves its window and view state (see [`Shell::persist`]).
fn persists(cx: &App) -> bool {
    cx.try_global::<Shell>().is_some_and(|s| s.persist)
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
    NewWindow,
    OpenSwitcher,
    Detach,
    DetachOthers,
    QuitProject,
    QuitShelbi,
    ToggleZen,
    OpenErrorLog,
    OpenSettings,
    SendFeedback,
    OpenAbout,
    OpenAcknowledgements,
    OpenAddProject,
    ConnectRemote,
    ShowLaunchScreen,
    LoadReview(String),
    ApproveReview(String),
    AskRejectReason(String),
    RejectReview(String, String),
    OpenEditor(EditTarget),
    OpenIssue(String),
    OpenChangelogEntry(String),
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
    fn new_window(&mut self) {
        self.push(HostCall::NewWindow);
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
    fn open_settings(&mut self) {
        self.push(HostCall::OpenSettings);
    }
    fn send_feedback(&mut self) {
        self.push(HostCall::SendFeedback);
    }
    fn open_about(&mut self) {
        self.push(HostCall::OpenAbout);
    }
    fn open_acknowledgements(&mut self) {
        self.push(HostCall::OpenAcknowledgements);
    }
    fn open_add_project(&mut self) {
        self.push(HostCall::OpenAddProject);
    }
    fn connect_remote(&mut self) {
        self.push(HostCall::ConnectRemote);
    }
    fn show_launch_screen(&mut self) {
        self.push(HostCall::ShowLaunchScreen);
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
    fn open_issue(&mut self, id: &str) {
        self.push(HostCall::OpenIssue(id.to_string()));
    }
    fn open_changelog_entry(&mut self, id: &str) {
        self.push(HostCall::OpenChangelogEntry(id.to_string()));
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

/// A window switching projects waits for one frame before it builds the main
/// area's view and attaches its sessions: the frame shows the target's
/// sidebar (its prefetched skeleton, with its saved selection) at once.
#[derive(Debug, Clone, PartialEq)]
struct SwitchHold {
    /// A frame has rendered since the switch.
    rendered: bool,
    /// The review the project was left with open.
    review: Option<String>,
}

/// What the main area shows, as the sidebar's selection follows it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shown {
    View(View),
    Review(Option<String>),
    Settings,
}

/// A row the keys or a click put the selection on. It holds only while the
/// main area shows what it showed then (`over`): any change to the main area
/// hands the selection back to the row for what it shows.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cursor {
    row: SidebarTarget,
    over: Shown,
}

/// The sidebar scroll a switch restores once the row it was left on exists
/// (the skeleton may not have it yet; the feed's first snapshot will).
#[derive(Debug, Clone, PartialEq)]
struct PendingRows {
    selected: Option<String>,
    scroll_y: f32,
}

/// A Shelbi window's root view.
pub struct ShelbiWindow {
    /// The project the window shows (meaningless while [`Self::chooser`]).
    project: String,
    /// The window slot its frame is saved in.
    slot: usize,
    /// No project is shown: the empty-state chooser (the last open project
    /// was quit). Its keyboard row.
    chooser: bool,
    chooser_highlight: usize,
    /// This window's handle, once it has one (notices check it).
    handle: Option<AnyWindowHandle>,
    boot: Boot,
    /// Drains bootstrap's channel into this view; replaced on retry.
    _bootstrap: Option<Task<()>>,
    /// Re-resolves the theme when the system appearance changes.
    _appearance: Subscription,
    /// Re-reads Reduce Motion each time the window comes forward.
    _motion: Subscription,
    client: ClientState,
    model: SidebarModel,
    link: Option<DaemonLink>,
    collapsed: bool,
    /// The sidebar's hide and show, following `collapsed` on a toggle
    /// (`dt-sidebar-toggle-animation`); any other change to `collapsed`
    /// lands at once.
    reveal: SidebarReveal,
    dragging: bool,
    /// The project switcher's menu.
    switcher: sidebar::Switcher,
    /// When the open overlay opened, for its slide-and-fade in.
    overlay_opened: Option<Instant>,
    /// The sidebar's rows, measured for the selection chip's slide.
    nav_track: SharedTrack,
    /// Where the keys or a click moved the selection, until the main area
    /// changes ([`Self::selected`]).
    cursor: Option<Cursor>,
    /// The sidebar sections the user has collapsed (saved per project with
    /// the rest of where it was left).
    collapsed_sections: BTreeSet<sidebar::Section>,
    /// The sections and machine groups folding open or closed after a
    /// click on their header.
    folds: sidebar::SidebarFolds,
    route: Route,
    /// The settings sidebar's page and keyboard row (the page outlives a
    /// visit: Settings reopens on it), and its rows measured for the
    /// selection chip's slide.
    settings_nav: SettingsNav,
    settings_track: SharedTrack,
    /// Settings was opened over the Review view: leaving it reopens the
    /// review.
    settings_over_review: bool,
    /// The Issues board for this window's project, created the first time it
    /// is shown.
    issues: Option<Entity<IssuesView>>,
    /// The task detail is open over the main area, which shows this (the
    /// Review or Workspace panel's More, `dt-more-opens-detail`): the board
    /// draws it as a layer there until it closes or the main area moves on.
    detail_over: Option<Shown>,
    /// The workflow filter the project was left with, for its board.
    pending_workflow_filter: Option<String>,
    /// The orchestrator chat, created the first time it is shown.
    chat: Option<Entity<ChatView>>,
    /// The Activity feed and the Machines view, likewise created on first show.
    activity: Option<Entity<ActivityView>>,
    /// The Activity feed's reader, started when the project opens so its
    /// first read is waiting when the feed is first shown; the view takes
    /// it then (`dt-activity-perf`). Only a live window (`Self::new`)
    /// starts one.
    activity_reader: Option<ActivityReader>,
    prefetch_activity: bool,
    machines: Option<Entity<MachinesView>>,
    /// The Changelog page, created the first time it is shown.
    changelog: Option<Entity<ChangelogView>>,
    /// The open overlay (palette or dialog), mirrored in `client.overlay()`.
    overlay: Option<ActiveOverlay>,
    palette_inputs: PaletteInputs,
    /// Takes the open palette's issue and Changelog search hits as they
    /// land; ends when the palette closes.
    _palette_poll: Option<Task<()>>,
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
    /// The push was still running as of the last frame.
    column_moving: bool,
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
    /// A review a switch reopened (the project was left with it open): if
    /// it no longer opens, it backs out quietly, with no prompt or banner.
    quiet_review: Option<String>,
    /// The switch's first frame is still to come or to finish.
    switch_hold: Option<SwitchHold>,
    pending_rows: Option<PendingRows>,
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
    _feed: Option<FeedSubscription>,
    _feed_task: Option<Task<()>>,
    /// The window's own subscriptions (bounds, activation, quit).
    _subscriptions: Vec<Subscription>,
    /// The project-bound views' subscriptions, dropped with them on a switch.
    _view_subscriptions: Vec<Subscription>,
    /// The workflows a test's window was asked to open the editor of.
    #[cfg(test)]
    edited_workflows: Vec<String>,
    /// The agents a test's window was asked to open the details of.
    #[cfg(test)]
    edited_agents: Vec<String>,
    /// The forms Settings ▸ Workspaces asked for, in order.
    #[cfg(test)]
    pool_forms: Vec<Intent>,
    /// The New / Edit workspace dialog or a removal's confirmation, over the
    /// whole window while Settings shows.
    pool_dialog: Option<(Entity<WorkspaceDialog>, Subscription)>,
    /// Contact Us, over the whole window, with the focus to hand back when
    /// it closes. It belongs to no project, so a switch keeps it.
    feedback: Option<(Entity<FeedbackDialog>, Subscription, Option<FocusHandle>)>,
    /// Connect to Remote Session, over the whole window, with the focus to
    /// hand back when it closes. Like Contact Us, a switch keeps it.
    connect: Option<(Entity<ConnectDialog>, Subscription, Option<FocusHandle>)>,
    /// How often a pool change asked the feed for a refresh.
    #[cfg(test)]
    pool_refreshes: usize,
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
    /// Test seam: builds the chat's deps (each chat a switch builds).
    #[cfg(any(test, feature = "visual-check"))]
    chat_deps: Option<Rc<dyn Fn() -> ChatDeps>>,
    /// `--bench-push`'s frame probe: each frame's render start, paint end
    /// and column position.
    #[cfg(feature = "visual-check")]
    probe: Option<Rc<std::cell::RefCell<crate::bench_push::Probe>>>,
}

impl ShelbiWindow {
    pub fn new(
        project: &str,
        slot: usize,
        window_state: WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let shared_ready = cx.try_global::<Shell>().and_then(|s| s.ready.clone());
        let mut this = Self::build(project, window_state, window, cx);
        this.slot = slot;
        // Where the project was left (a small local read: a new window may
        // open before the catalog has prefetched it).
        let saved = if persists(cx) {
            ProjectViewState::load(project)
        } else {
            ProjectViewState::default()
        };
        this.adopt_view_state(saved);
        // The feed and keymaps only read local state: start them now, so the
        // shell has data the moment bootstrap is done.
        this.start_feed(window, cx);
        this.load_keymaps(cx);
        this.prefetch_activity = true;
        this.prefetch_activity();
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
        // Quitting (⌘Q, Detach) closes no window one by one: save where this
        // one's project was left now.
        this._subscriptions.push(cx.on_app_quit(|this, cx| {
            if let Some((project, state)) = this.view_state_to_save(cx) {
                if let Err(e) = state.save(&project) {
                    tracing::warn!(error = %e, "could not save the project's view state");
                }
            }
            async {}
        }));
        this.load_notify_prefs(cx);
        window.set_window_title(&this.model.project_label);
        this
    }

    /// A window that reaches nothing (a test's): the shell over the
    /// catalog's skeleton, commands and the palette's inputs recorded by a
    /// tap, no feed, bootstrap or disk.
    #[cfg(test)]
    pub(crate) fn inert(
        project: &str,
        slot: usize,
        window_state: WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut w = Self::build(project, window_state, window, cx);
        w.slot = slot;
        w.boot = Boot::Ready(Box::new(inert_ready()));
        w.tap = Some(Default::default());
        // A native view with no entity yet: nothing attaches or reads.
        w.client.set_view(View::Machines);
        window.set_window_title(&w.model.project_label);
        w
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
        // The window's translucency, before its first frame.
        theme::window::apply_to(window, cx);

        ShelbiWindow {
            project: project.to_string(),
            slot: 0,
            chooser: false,
            chooser_highlight: 0,
            handle: Some(window.window_handle()),
            boot: Boot::InFlight(BootstrapState::ResolvingEnv),
            _bootstrap: None,
            _appearance: theme::follow_system(window, cx),
            // The accessibility settings are read when a window comes to
            // the front (System Settings is in front while they change).
            _motion: cx.observe_window_activation(window, |_, _, cx| {
                theme::motion::sync_reduce_motion(cx);
                theme::window::sync_reduce_transparency(cx);
                crate::elements::terminal::scrollbar::SystemScrollbars::sync(cx);
            }),
            palette_chord: palette_chord(&keymaps),
            client,
            model,
            link: None,
            collapsed: window_state.sidebar_collapsed,
            reveal: SidebarReveal::new(!window_state.sidebar_collapsed),
            dragging: false,
            switcher: sidebar::Switcher::default(),
            overlay_opened: None,
            nav_track: SlideTrack::shared(),
            cursor: None,
            collapsed_sections: BTreeSet::new(),
            folds: Default::default(),
            route: Route::View,
            settings_nav: SettingsNav::default(),
            settings_track: SlideTrack::shared(),
            settings_over_review: false,
            issues: None,
            detail_over: None,
            pending_workflow_filter: None,
            chat: None,
            activity: None,
            activity_reader: None,
            prefetch_activity: false,
            machines: None,
            changelog: None,
            overlay: None,
            palette_inputs: PaletteInputs::default(),
            _palette_poll: None,
            flash_generation: 0,
            workspace: None,
            workspace_return: None,
            review: None,
            column: ColumnSwap::new(),
            column_moving: false,
            collapsed_panel_cols: None,
            sidebar_scroll: ScrollHandle::new(),
            clock: Rc::new(Instant::now),
            reduce_motion: Rc::new(crate::platform::motion::reduce_motion),
            review_loader: ReviewLoader::default(),
            quiet_review: None,
            switch_hold: None,
            pending_rows: None,
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
            _view_subscriptions: Vec::new(),
            #[cfg(test)]
            edited_workflows: Vec::new(),
            #[cfg(test)]
            edited_agents: Vec::new(),
            #[cfg(test)]
            pool_forms: Vec::new(),
            pool_dialog: None,
            feedback: None,
            connect: None,
            #[cfg(test)]
            pool_refreshes: 0,
            drawn_once: false,
            #[cfg(any(test, feature = "visual-check"))]
            tap: None,
            #[cfg(any(test, feature = "visual-check"))]
            review_deps: None,
            #[cfg(any(test, feature = "visual-check"))]
            workspace_deps: None,
            #[cfg(any(test, feature = "visual-check"))]
            chat_deps: None,
            #[cfg(feature = "visual-check")]
            probe: None,
        }
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
        let mut model = shelbi_app::command_model::command_model_from(
            &self.project,
            &self.model,
            inputs.others_attached,
            inputs.other_projects.clone(),
            inputs.edit_targets.clone(),
        );
        // This client opens windows: File ▸ New Window and the palette's.
        model.multi_window = true;
        // It has the launch screen and the Connect to Remote Session dialog.
        model.launch_screen = true;
        model.remote_connect = true;
        if self.chooser {
            // No project: only what needs none (switch, add, detach, quit
            // Shelbi).
            model.project = None;
            model.views.clear();
            model.workspaces.clear();
            model.reviews.clear();
            model.edit_targets.clear();
            model.multi_window = false;
        }
        model
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
        let reading = project.clone();
        let read = cx
            .background_executor()
            .spawn(async move { PaletteInputs::read(&project, control, &client_id) });
        cx.spawn(async move |this, cx| {
            let inputs = read.await;
            this.update(cx, |this, cx| {
                if !this.shows(&reading) {
                    return;
                }
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
        // Two windows on one project see the same changes: the first one
        // showing it posts, so a notice never comes twice.
        if changes.notices.is_empty() || !self.posts_notices(cx) {
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

    /// Whether this window posts its project's notices: it is the first open
    /// window showing the project.
    fn posts_notices(&self, cx: &App) -> bool {
        let Some(me) = self.handle.map(|h| h.window_id()) else {
            return true;
        };
        windows_showing(&self.project, Some(me), cx)
            .first()
            .is_none_or(|first| first.window_id() == me)
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
            View::Changelog | View::Activity | View::Machines => notify::Showing::Other,
        }
    }

    /// Open what a clicked notification is about.
    pub fn open_notice(&mut self, target: notify::Target, window: &mut Window, cx: &mut Context<Self>) {
        // The notice says where to go: a switch it came with restores nothing.
        self.switch_hold = None;
        self.pending_rows = None;
        match target {
            notify::Target::Workspace(workspace) => {
                self.client.focus_main();
                self.run(CommandKind::FocusWorkspace { workspace }, window, cx);
            }
            notify::Target::Review(id) => self.open_task(&id, window, cx),
            notify::Target::Orchestrator => {
                self.run(CommandKind::ShowView(View::Session("orch".into())), window, cx)
            }
        }
    }

    /// Subscribe to the project's feed (shared with any other window on the
    /// project) and pump its events into this view.
    fn start_feed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let hub = FeedHub::global(cx);
        let (feed, mut events) = hub.subscribe(&self.project, cx);
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
                    self.apply_pending_rows();
                    // An open palette follows the live state, query kept.
                    let entries = self.palette_entries();
                    if let Some(ActiveOverlay::Palette { palette, .. }) = &mut self.overlay {
                        palette.set_entries(entries);
                    }
                    self.update_workspace_row(cx);
                    self.update_review_row(window, cx);
                    self.update_task_rows(cx);
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
        let reading = project.clone();
        let read = cx
            .background_executor()
            .spawn(async move { shelbi_state::keymap::load_keymaps(Some(&project)).0 });
        cx.spawn(async move |this, cx| {
            let keymaps = read.await;
            this.update(cx, |this, cx| {
                if this.shows(&reading) {
                    this.adopt_keymaps(keymaps, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Take `keymaps` for the window's keys, its hints and the menu bar.
    fn adopt_keymaps(&mut self, keymaps: Keymaps, cx: &mut Context<Self>) {
        self.palette_chord = palette_chord(&keymaps);
        self.keymaps = keymaps;
        if let Some(ws) = self.workspace.clone() {
            let chord = self.palette_chord.clone();
            ws.update(cx, |ws, cx| ws.set_palette_chord(&chord, cx));
        }
        self.sync_menus(cx);
        cx.notify();
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
        let opening = project.clone();
        let open = cx
            .background_executor()
            .spawn(async move { projects::open_project(&project, &cli, &env) });
        cx.spawn(async move |this, cx| {
            let result = open.await;
            this.update(cx, |this, cx| {
                // The window switched away meanwhile: not its news.
                if !this.shows(&opening) {
                    return;
                }
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

    // --- switching projects ------------------------------------------------

    /// Whether the window shows `project` (not the chooser).
    pub fn shows(&self, project: &str) -> bool {
        !self.chooser && self.project == project
    }

    /// Switch this window to `project` in place. The outgoing project's
    /// view state is saved and its views torn down (their sessions detach;
    /// the daemon's stay); the target's prefetched sidebar paints on the next
    /// frame with its own saved selection, and its main view is built (and
    /// attached) once that frame is drawn. The window keeps its frame, size
    /// and sidebar width. Another window already showing `project` doesn't
    /// matter: two windows may show one project.
    pub fn switch_to(&mut self, project: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher.close((self.clock)());
        if self.shows(project) {
            return;
        }
        // Switched from Settings (its project section's switcher): it
        // reopens on the same page, for the new project.
        let settings = matches!(self.route, Route::Settings(_)).then(|| self.settings_nav.page());
        self.remember_view_state(cx);
        self.teardown_project(window, cx);
        self.chooser = false;
        self.project = project.to_string();
        self.client.switch_project(project);
        self.model = cx
            .try_global::<Catalog>()
            .map(|c| c.first_frame_sidebar(project))
            .unwrap_or_else(|| SidebarModel::skeleton(project));
        self.notifier = notify::Tracker::new(project);
        self.palette_inputs = PaletteInputs::default();
        window.set_window_title(&self.model.project_label);
        let saved = cx
            .try_global::<Catalog>()
            .map(|c| c.view_state(project))
            .unwrap_or_default();
        self.adopt_view_state(saved);
        self.start_feed(window, cx);
        self.prefetch_activity();
        if !self.offline() {
            self.load_keymaps(cx);
            self.open_project(cx);
            self.refresh_palette_inputs(cx);
        }
        self.record_slot(cx);
        self.sync_menus(cx);
        if let Some(page) = settings {
            self.open_settings(Some(page), window, cx);
        }
        cx.notify();
    }

    /// Show no project: the empty-state chooser (the project the window
    /// showed was quit and no other is open). The window stays open.
    fn show_chooser(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.chooser {
            return;
        }
        self.remember_view_state(cx);
        self.teardown_project(window, cx);
        self.chooser = true;
        self.chooser_highlight = 0;
        self.model = SidebarModel::skeleton("");
        self.palette_inputs = PaletteInputs::default();
        window.set_window_title(CHOOSER_TITLE);
        // The list to choose from, re-read off the UI thread.
        if let Some(catalog) = cx.try_global::<Catalog>().cloned().filter(|_| !self.offline()) {
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
        self.record_slot(cx);
        self.sync_menus(cx);
        cx.notify();
    }

    /// Drop everything bound to the shown project: its overlay, its views
    /// and their sessions and subscriptions, the review flow, the panel in
    /// the column, the feed subscription, its notes.
    fn teardown_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Whatever the review flow still has running opens nothing.
        if let Some(task) = self.review_loader.pending().map(str::to_string) {
            self.review_loader.cancel(&task);
        }
        self.review_loader = ReviewLoader::default();
        // A workspace dialog's draft is the old project's: it goes unsaved
        // (a job it already sent finishes against that project).
        self.pool_dialog = None;
        self.overlay = None;
        self.overlay_opened = None;
        self.client.close_overlay();
        // Detach the sessions now (the daemon's stay), then drop the views.
        if let Some(chat) = self.chat.take() {
            chat.update(cx, |chat, cx| chat.hide(cx));
        }
        if let Some(ws) = self.workspace.take() {
            ws.update(cx, |ws, cx| ws.hide(cx));
        }
        if let Some(review) = self.review.take() {
            review.update(cx, |r, cx| r.hide(cx));
        }
        // The native views' background reads stop without a join, so a slow
        // read in flight never holds the switch up.
        if let Some(issues) = self.issues.take() {
            issues.update(cx, |v, _| v.stop_workers());
        }
        self.detail_over = None;
        if let Some(activity) = self.activity.take() {
            activity.update(cx, |v, _| v.stop_workers());
        }
        // Dropping it stops it, without waiting on a read in flight.
        self.activity_reader = None;
        if let Some(machines) = self.machines.take() {
            machines.update(cx, |v, _| v.stop_workers());
        }
        if let Some(changelog) = self.changelog.take() {
            changelog.update(cx, |v, _| v.stop_workers());
        }
        self._view_subscriptions.clear();
        self.set_route(Route::View);
        self.workspace_return = None;
        self.column = ColumnSwap::new();
        self.column_moving = false;
        self.collapsed_panel_cols = None;
        self.sidebar_scroll = ScrollHandle::new();
        self.nav_track = SlideTrack::shared();
        self.folds.clear();
        self.cursor = None;
        self.switch_hold = None;
        self.pending_rows = None;
        self.pending_workflow_filter = None;
        self.quiet_review = None;
        self.banner = None;
        self.status = None;
        self.flash_generation += 1;
        self._feed_task = None;
        self._feed = None;
        self.link = None;
        self.focus_handle.focus(window, cx);
    }

    /// Where the shown project is left: its view, focus, sidebar selection
    /// and scroll, open review and board filter. `None` for the chooser.
    fn view_state_to_save(&self, cx: &App) -> Option<(String, ProjectViewState)> {
        if self.chooser {
            return None;
        }
        let state = ProjectViewState {
            view: Some(self.client.view().as_view_id()),
            sidebar_focused: self.client.focus() == Focus::Sidebar,
            selected: self.selected(cx).map(|t| row_key(&t)),
            scroll_y: self.sidebar_scroll.offset().y.as_f32(),
            review: self.shown_review(cx),
            workspace_return: self.workspace_return.as_ref().map(View::as_view_id),
            workflow_filter: self
                .issues
                .as_ref()
                .map(|i| i.read(cx).workflow_filter())
                .unwrap_or_else(|| self.pending_workflow_filter.clone()),
            collapsed_sections: self.collapsed_sections.iter().map(|s| s.key().to_string()).collect(),
        };
        Some((self.project.clone(), state))
    }

    /// Remember where the shown project is left: in the catalog at once (a
    /// switch back reads it from memory) and on disk off the UI thread.
    fn remember_view_state(&mut self, cx: &mut Context<Self>) {
        let Some((project, state)) = self.view_state_to_save(cx) else {
            return;
        };
        if let Some(catalog) = cx.try_global::<Catalog>() {
            catalog.remember_view(&project, state.clone());
        }
        if self.offline() || !persists(cx) {
            return;
        }
        cx.background_executor()
            .spawn(async move {
                if let Err(e) = state.save(&project) {
                    tracing::warn!(error = %e, "could not save the project's view state");
                }
            })
            .detach();
    }

    /// Take on where a project was left: its view and focus, the panel's
    /// Back, the sidebar position (once its rows exist), the board filter.
    /// The main view itself is built after the next frame ([`SwitchHold`]).
    fn adopt_view_state(&mut self, saved: ProjectViewState) {
        let view = saved
            .view
            .as_deref()
            .map(View::from_view_id)
            .unwrap_or_else(|| View::Session("orch".into()));
        self.client.set_view(view);
        if saved.sidebar_focused && !self.collapsed {
            self.client.focus_sidebar();
        } else {
            self.client.focus_main();
        }
        self.workspace_return = saved.workspace_return.as_deref().map(View::from_view_id);
        self.pending_workflow_filter = saved.workflow_filter;
        self.collapsed_sections = saved
            .collapsed_sections
            .iter()
            .filter_map(|key| sidebar::Section::from_key(key))
            .collect();
        // Restored, not toggled: drawn as left, with no fold.
        self.folds.clear();
        self.pending_rows = Some(PendingRows {
            selected: saved.selected,
            scroll_y: saved.scroll_y,
        });
        self.apply_pending_rows();
        self.switch_hold = Some(SwitchHold {
            rendered: false,
            review: saved.review,
        });
    }

    /// Scroll the sidebar back where the project was left once the row it
    /// was left on is there: on the skeleton already, or when a snapshot
    /// brings it. A row that is gone once the board has loaded is let go.
    /// The selection itself is the restored view's row ([`Self::selected`]).
    fn apply_pending_rows(&mut self) {
        let Some(pending) = self.pending_rows.clone() else {
            return;
        };
        let listed = pending.selected.as_deref().is_some_and(|key| {
            self.sidebar_entries()
                .iter()
                .filter_map(|e| e.row.target())
                .any(|t| row_key(&t) == key)
        });
        if !listed && pending.selected.is_some() && self.model.board_loading {
            return;
        }
        self.sidebar_scroll
            .set_offset(gpui::point(px(0.0), px(pending.scroll_y)));
        self.pending_rows = None;
    }

    /// The switch's frame is drawn: build the main area's view and attach its
    /// sessions, and reopen the review the project was left with.
    fn finish_switch(&mut self, hold: SwitchHold, window: &mut Window, cx: &mut Context<Self>) {
        let mut view = self.client.view().clone();
        // A workspace the project no longer declares (its rows are known, and
        // it isn't among them) gives way to the chat.
        if let Some(name) = workspace_name(&view) {
            if !self.model.workspaces.is_empty() && self.workspace_row(name).is_none() {
                view = View::Session("orch".into());
                self.workspace_return = None;
            }
        }
        let sidebar_focused = self.client.focus() == Focus::Sidebar;
        if workspace_name(&view).is_some_and(|name| WorkspaceView::wants_panel(self.workspace_row(name).as_ref())) {
            // The workspace panel is where it was: no push to it.
            self.column.show(Occupant::Workspace, Motion::Fade, (self.clock)());
            self.column.finish();
        }
        // Settings, reopened by a switch from its project section, stays in
        // front: the view is built behind it, for Back.
        let settings = matches!(self.route, Route::Settings(_)).then(|| std::mem::replace(&mut self.route, Route::View));
        self.perform(HostCall::ShowView(view), window, cx);
        if let Some(route) = settings {
            self.set_route(route);
            self.focus_main_view(window, cx);
            cx.notify();
            return;
        }
        if let Some(task) = hold.review {
            self.quiet_review = Some(task.clone());
            self.load_review(task, window, cx);
        }
        if sidebar_focused && !self.panel_in_column() {
            self.client.focus_sidebar();
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// Record this window's slot (its project and frame) and save the slots.
    fn record_slot(&mut self, cx: &mut Context<Self>) {
        let entry = window_state::Slot {
            project: (!self.chooser).then(|| self.project.clone()),
            open: true,
            state: self.window_state.clone(),
        };
        if cx.has_global::<Shell>() {
            cx.global_mut::<Shell>().session.set(self.slot, entry);
        }
        save_session(cx);
    }

    /// Close this window (⌘W), saving where its project was left. The save
    /// is written here, not off the thread: closing the last window quits the
    /// app, which wouldn't wait for it.
    fn close_window(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((project, state)) = self.view_state_to_save(cx) {
            if let Some(catalog) = cx.try_global::<Catalog>() {
                catalog.remember_view(&project, state.clone());
            }
            if !self.offline() && persists(cx) {
                if let Err(e) = state.save(&project) {
                    tracing::warn!(error = %e, "could not save the project's view state");
                }
            }
        }
        window.remove_window();
    }

    /// The project the shown one was quit: this window moves to another open
    /// project, or to the chooser when none is left.
    fn after_quit_project(&mut self, quit: String, window: &mut Window, cx: &mut Context<Self>) {
        let offline = self.offline();
        let next = cx.background_executor().spawn(async move {
            if offline {
                return None;
            }
            shelbi_state::list_open_projects()
                .unwrap_or_default()
                .into_iter()
                .find(|p| *p != quit)
        });
        cx.spawn_in(window, async move |this, cx| {
            let next = next.await;
            this.update_in(cx, |this, window, cx| this.leave_quit_project(next, window, cx))
                .ok();
        })
        .detach();
    }

    /// Move off a quit project: to `next`, else the chooser.
    fn leave_quit_project(&mut self, next: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        match next {
            Some(project) => self.switch_to(&project, window, cx),
            None => self.show_chooser(window, cx),
        }
    }

    /// Test seam: in a test window (the tap), nothing reaches disk, the CLI
    /// or the daemon.
    fn offline(&self) -> bool {
        #[cfg(any(test, feature = "visual-check"))]
        if self.tap.is_some() {
            return true;
        }
        false
    }

    /// The sidebar's rows as it draws them (the order and collapse the
    /// keyboard walks).
    fn sidebar_entries(&self) -> Vec<sidebar::Entry> {
        sidebar::layout(&self.model, &self.collapsed_sections)
    }

    fn selectable_count(&self) -> usize {
        self.sidebar_entries().iter().filter(|e| e.row.is_selectable()).count()
    }

    /// Route the main area. Whatever it shows next has its row selected:
    /// the keys' or a click's row is let go.
    fn set_route(&mut self, route: Route) {
        self.route = route;
        self.cursor = None;
        // A workspace dialog belongs to the settings view it was opened from.
        self.pool_dialog = None;
    }

    /// What the main area shows.
    fn shown(&self, cx: &App) -> Shown {
        match &self.route {
            Route::View => Shown::View(self.client.view().clone()),
            Route::Review => Shown::Review(self.review.as_ref().and_then(|r| r.read(cx).task_id().map(str::to_string))),
            Route::Settings(_) => Shown::Settings,
        }
    }

    /// The sidebar's selected row: the row for what the main area shows (the
    /// Orchestrator's for the chat, a workspace's for its page), unless the
    /// keys or a click have moved it since the main area last changed. None
    /// when what it shows has no row drawn (Machines, Settings, a collapsed
    /// section's row), so no stale row stays selected.
    fn selected(&self, cx: &App) -> Option<SidebarTarget> {
        let shown = self.shown(cx);
        let targets: Vec<SidebarTarget> = self.sidebar_entries().iter().filter_map(|e| e.row.target()).collect();
        if let Some(cursor) = self.cursor.as_ref().filter(|c| c.over == shown && targets.contains(&c.row)) {
            return Some(cursor.row.clone());
        }
        targets.into_iter().find(|t| match (&shown, t) {
            (Shown::View(view), SidebarTarget::View(row)) => view == row,
            (Shown::View(View::Session(name)), SidebarTarget::Workspace(row)) => name == row,
            (Shown::Review(Some(task)), SidebarTarget::Review(row)) => task == row,
            _ => false,
        })
    }

    /// [`Self::selected`] as a selectable-row ordinal.
    fn selected_ordinal(&self, cx: &App) -> Option<usize> {
        let selected = self.selected(cx)?;
        self.sidebar_entries()
            .iter()
            .filter_map(|e| e.row.target())
            .position(|t| t == selected)
    }

    /// Move the selection to `row` without opening it (the keys, a click
    /// before it opens).
    fn select_row(&mut self, row: SidebarTarget, cx: &App) {
        let over = self.shown(cx);
        self.cursor = Some(Cursor { row, over });
    }

    /// Move the selection one selectable row up or down. With nothing
    /// selected, Down starts at the top and Up at the bottom.
    fn step_selection(&mut self, down: bool, cx: &App) {
        let count = self.selectable_count();
        if count == 0 {
            return;
        }
        let ordinal = match (self.selected_ordinal(cx), down) {
            (Some(i), true) => (i + 1).min(count - 1),
            (Some(i), false) => i.saturating_sub(1),
            (None, true) => 0,
            (None, false) => count - 1,
        };
        if let Some(row) = self.target_at(ordinal) {
            self.select_row(row, cx);
        }
    }

    fn target_at(&self, ordinal: usize) -> Option<SidebarTarget> {
        self.sidebar_entries()
            .iter()
            .filter_map(|e| e.row.target())
            .nth(ordinal)
    }

    /// Collapse or expand a sidebar section, folding it closed or open, and
    /// remember it for the project. The selection stays on its row while
    /// that row is drawn (selection follows rows, not ordinals); the keys
    /// skip a section's rows from the moment it starts closing.
    fn toggle_section(&mut self, section: sidebar::Section, cx: &mut Context<Self>) {
        let collapsed = !self.collapsed_sections.remove(&section);
        if collapsed {
            self.collapsed_sections.insert(section);
        }
        self.folds.toggle(
            sidebar::Body::Section(section),
            collapsed,
            (self.clock)(),
            (self.reduce_motion)(),
        );
        self.remember_view_state(cx);
        cx.notify();
    }

    /// A sidebar chord (⌘O ⌘I ⌘L, ⌘1 to ⌘9): open what it names, selecting
    /// its row as a click would.
    fn nav_chord(&mut self, chord: NavChord, window: &mut Window, cx: &mut Context<Self>) {
        // While a task panel holds the column, ⌘1 to ⌘N are its views (the
        // hints it draws), not the hidden sidebar's rows.
        if let NavChord::Row(n) = chord {
            match self.column.target() {
                Occupant::Review => {
                    if let Some(review) = self.review.clone() {
                        review.update(cx, |r, cx| r.view_shortcut(n, window, cx));
                    }
                    return;
                }
                Occupant::Workspace => {
                    if let Some(ws) = self.workspace.clone() {
                        ws.update(cx, |w, cx| w.view_shortcut(n, window, cx));
                    }
                    return;
                }
                // Likewise the settings sidebar's pages.
                Occupant::Settings => {
                    if let Some(page) = SettingsPage::for_shortcut(n) {
                        self.show_settings_page(page, cx);
                    }
                    return;
                }
                Occupant::Sidebar => {}
            }
        }
        let target = match chord {
            NavChord::View(view) => Some(SidebarTarget::View(view)),
            NavChord::Row(n) => sidebar::layout(&self.model, &BTreeSet::new())
                .into_iter()
                .find(|e| e.shortcut == Some(n))
                .and_then(|e| e.row.target()),
        };
        // A collapsed section still answers its chords; the row just isn't
        // there to select.
        if let Some(target) = target {
            self.activate_target(target, window, cx);
        }
    }

    /// Open the selected sidebar row.
    fn activate_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.selected(cx) else {
            return;
        };
        self.activate_target(target, window, cx);
    }

    /// Open what a sidebar row opens, selecting it: what it opens takes the
    /// selection from there (a review still loading keeps it on its row).
    fn activate_target(&mut self, target: SidebarTarget, window: &mut Window, cx: &mut Context<Self>) {
        self.select_row(target.clone(), cx);
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
        let collapsed = !self.model.collapsed_machines.remove(&name);
        if collapsed {
            self.model.collapsed_machines.insert(name.clone());
        }
        self.folds.toggle(
            sidebar::Body::Machine(name.clone()),
            collapsed,
            (self.clock)(),
            (self.reduce_motion)(),
        );
        if self.offline() {
            cx.notify();
            return;
        }
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
                // The selection is the shown view's row again, even where
                // the keys had moved it off the view already shown.
                self.set_route(Route::View);
                let chat = is_chat(&view);
                match view {
                    View::Issues => {
                        self.ensure_issues(window, cx);
                    }
                    View::Changelog => {
                        self.ensure_changelog(window, cx);
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
                    // Its pane attaches once the push has settled. An idle
                    // workspace has no panel to push: its page shows at once.
                    let hold =
                        WorkspaceView::wants_panel(row.as_ref()) && self.push_ahead(Occupant::Workspace);
                    ws.update(cx, |ws, cx| {
                        ws.hold_content(hold, cx);
                        ws.show(&name, row, cx)
                    });
                }
                self.sync_chat(window, cx);
                self.focus_main_view(window, cx);
            }
            HostCall::IssueCommand(kind) => {
                let issues = self.ensure_issues(window, cx);
                issues.update(cx, |v, cx| v.run_issue_command(&kind, window, cx));
            }
            // In place: this window becomes the target's.
            HostCall::SwitchProject(project) => self.switch_to(&project, window, cx),
            HostCall::NewWindow => {
                if !self.chooser {
                    let project = self.project.clone();
                    // After this update, so the new window's first frame
                    // isn't nested in ours.
                    cx.defer(move |cx| {
                        open_new_window(&project, cx);
                    });
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
            HostCall::OpenSettings => self.open_settings(None, window, cx),
            HostCall::SendFeedback => self.open_feedback(window, cx),
            // After this update, so the window's first frame isn't nested in
            // ours (as New Window).
            HostCall::OpenAbout => cx.defer(crate::views::about::show_about),
            HostCall::OpenAcknowledgements => cx.defer(crate::views::about::show_bundled_software),
            HostCall::OpenAddProject => self.open_add_project(window, cx),
            HostCall::OpenIssue(id) => self.open_task(&id, window, cx),
            HostCall::OpenChangelogEntry(id) => self.open_changelog_entry(id, window, cx),
            HostCall::ConnectRemote => self.open_connect(window, cx),
            // Its own window, after this update (as New Window).
            HostCall::ShowLaunchScreen => cx.defer(|cx| {
                crate::views::launch::show(cx);
            }),
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
        self.open_in_editor(move || shelbi_app::command_model::edit_target_path(&project, &target), cx);
    }

    /// Open workflow `name`'s editor (Settings ▸ Workflows' edit button, a
    /// row's click, a workflow just created): it takes the whole window,
    /// the settings sidebar hidden, until it closes back onto the page.
    fn edit_workflow(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.edited_workflows.push(name.clone());
        if let Route::Settings(view) = &self.route {
            view.update(cx, |v, cx| v.open_editor(&name, window, cx));
        }
        cx.notify();
    }

    /// Open agent `name`'s details (Settings ▸ Agents' edit button, a row's
    /// click, an agent just created): like the workflow editor, it takes
    /// the whole window until it closes back onto the page.
    fn edit_agent(&mut self, name: String, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.edited_agents.push(name.clone());
        if let Route::Settings(view) = &self.route {
            view.update(cx, |v, cx| v.open_agent_editor(&name, window, cx));
        }
        cx.notify();
    }

    /// Open the form Settings ▸ Workspaces asks for: New workspace, a
    /// workspace's edit button, row or menu item, and Remove workspace… open
    /// a workspace dialog over the window; Add machine and Configure go to
    /// the settings view, which opens its machine form. One dialog at a time.
    fn open_pool_form(&mut self, intent: Intent, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        self.pool_forms.push(intent.clone());
        let Route::Settings(view) = &self.route else {
            return;
        };
        let view = view.clone();
        if self.pool_dialog.is_some() || view.read(cx).machine_form().is_some() {
            return;
        }
        let page = view.read(cx).workspaces();
        let dialog = match intent {
            // A dialog that can't open (the pool not read yet, a workspace
            // gone) leaves the page as it was.
            Intent::NewWorkspace | Intent::EditWorkspace(_) | Intent::RemoveWorkspace(_) => {
                DialogBody::for_intent(&intent, &page, cx)
                    .map(|body| cx.new(|cx| WorkspaceDialog::new(body, page, window, cx)))
            }
            Intent::AddMachine | Intent::ConfigureMachine(_) => {
                view.update(cx, |v, cx| v.open_pool_form(&intent, window, cx));
                None
            }
        };
        if let Some(dialog) = dialog {
            let sub = cx.subscribe_in(&dialog, window, |this, _, event: &WorkspaceDialogEvent, window, cx| match event {
                WorkspaceDialogEvent::Closed { done } => this.close_pool_dialog(done.clone(), window, cx),
            });
            self.pool_dialog = Some((dialog, sub));
        }
        cx.notify();
    }

    /// The workspace dialog closed: drop it, read the pool again after a
    /// change, and give the keyboard back to the button that opened it.
    fn close_pool_dialog(
        &mut self,
        done: Option<shelbi_app::settings::workspace_form::Outcome>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pool_dialog = None;
        if let Route::Settings(view) = &self.route {
            view.update(cx, |v, cx| v.pool_dialog_closed(done.as_ref(), window, cx));
        }
        cx.notify();
    }

    /// Open the dialog `intent` asks for, its Machine picker open when
    /// `picker` (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub(crate) fn offscreen_open_pool_form(&mut self, intent: Intent, picker: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.open_pool_form(intent, window, cx);
        if picker {
            if let Some((dialog, _)) = &self.pool_dialog {
                dialog.update(cx, |d, cx| d.offscreen_open_picker(window, cx));
            }
        }
    }

    /// The open workspace dialog (tests).
    #[cfg(test)]
    pub(crate) fn pool_dialog(&self) -> Option<Entity<WorkspaceDialog>> {
        self.pool_dialog.as_ref().map(|(d, _)| d.clone())
    }

    /// Open Contact Us (the sidebar footer's megaphone, Help ▸ Send
    /// Feedback…) over the window, closing an overlay first. It waits while
    /// another dialog is up, and opens once.
    pub(crate) fn open_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let machine_form = matches!(&self.route, Route::Settings(view) if view.read(cx).machine_form().is_some());
        if self.modal_open() || machine_form {
            return;
        }
        if self.overlay.is_some() {
            self.close_overlay(window, cx);
        }
        let restore = window.focused(cx);
        let dialog = cx.new(|cx| FeedbackDialog::new(window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, event: &FeedbackEvent, window, cx| match event {
            FeedbackEvent::Closed => this.close_feedback(window, cx),
        });
        self.feedback = Some((dialog, sub, restore));
        cx.notify();
    }

    /// Open Connect to Remote Session (Shelbi ▸ Connect to Remote Session…,
    /// the palette) over the window, closing an overlay first. It waits
    /// while another dialog is up, and opens once.
    pub(crate) fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let machine_form = matches!(&self.route, Route::Settings(view) if view.read(cx).machine_form().is_some());
        if self.modal_open() || machine_form {
            return;
        }
        if self.overlay.is_some() {
            self.close_overlay(window, cx);
        }
        let restore = window.focused(cx);
        let dialog = cx.new(|cx| ConnectDialog::new(window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, event: &ConnectEvent, window, cx| match event {
            ConnectEvent::Closed(session) => {
                if let Some((_, _, Some(restore))) = this.connect.take() {
                    window.focus(&restore, cx);
                }
                if let Some(s) = session {
                    this.flash(format!("Connected to {} on {}.", s.display_label(), s.host), cx);
                    crate::views::remote_session::open_remote_session(s.clone(), cx);
                }
                cx.notify();
            }
        });
        self.connect = Some((dialog, sub, restore));
        cx.notify();
    }

    /// ⌘W over Contact Us or Connect to Remote Session: cancel it.
    fn dismiss_dialog(&self, cx: &mut Context<Self>) {
        if let Some((d, _, _)) = &self.feedback {
            d.update(cx, |d, cx| d.dismiss(cx));
        } else if let Some((d, _, _)) = &self.connect {
            d.update(cx, |d, cx| d.dismiss(cx));
        }
    }

    /// A modal dialog is up over the window (Contact Us, Connect to Remote
    /// Session, a workspace dialog).
    fn modal_open(&self) -> bool {
        self.feedback.is_some() || self.connect.is_some() || self.pool_dialog.is_some()
    }

    /// The open Connect dialog (tests).
    #[cfg(test)]
    pub(crate) fn connect_dialog(&self) -> Option<Entity<ConnectDialog>> {
        self.connect.as_ref().map(|(d, _, _)| d.clone())
    }

    /// Contact Us closed: drop it and give the keyboard back to where it was.
    fn close_feedback(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, _, Some(restore))) = self.feedback.take() {
            window.focus(&restore, cx);
        }
        cx.notify();
    }

    /// The open Contact Us dialog (`--visual-figma`).
    #[cfg(feature = "visual-check")]
    pub(crate) fn offscreen_feedback(&self) -> Option<Entity<FeedbackDialog>> {
        self.feedback.as_ref().map(|(d, _, _)| d.clone())
    }

    /// The open Contact Us dialog (tests).
    #[cfg(test)]
    pub(crate) fn feedback(&self) -> Option<Entity<FeedbackDialog>> {
        self.feedback.as_ref().map(|(d, _, _)| d.clone())
    }

    /// The project's workspaces or machines changed on disk: read the feed
    /// now, so the sidebar's workspace tree (and the workspace shown, if it
    /// went) follow without waiting out its timer.
    fn pool_changed(&mut self, cx: &mut Context<Self>) {
        #[cfg(test)]
        {
            self.pool_refreshes += 1;
        }
        if let Some(feed) = &self._feed {
            feed.request();
        }
        cx.notify();
    }

    /// Whether the workflow editor (or the agent editor) holds the window.
    fn editing_workflow(&self, cx: &App) -> bool {
        matches!(&self.route, Route::Settings(view) if view.read(cx).editing())
    }

    /// Open the file `find` locates in the user's editor and say so. The
    /// lookup and the launch run off the UI thread; a terminal editor opens
    /// in Terminal.app, a GUI editor directly.
    fn open_in_editor(&mut self, find: impl FnOnce() -> Option<std::path::PathBuf> + Send + 'static, cx: &mut Context<Self>) {
        let launch = cx.background_executor().spawn(async move {
            let path = find()
                .filter(|p| p.exists())
                .ok_or_else(|| "Couldn't find that file to edit.".to_string())?;
            let editor = crate::views::external_edit::open_file(&path)?;
            Ok::<_, String>(format!(
                "Opened {} in {editor}.",
                path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
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
        let issues = cx.new(|cx| IssuesView::new(project.clone(), cx));
        // The task pane's live session: attach only, through the same
        // sessions the Workspace view reaches.
        let handle = window.window_handle();
        let sink = self._feed.as_ref().map(|f| f.session_sink());
        let deps = SessionDeps::live(&project);
        #[cfg(any(test, feature = "visual-check"))]
        let deps = self.workspace_deps.as_ref().map(|d| (d.sessions)()).unwrap_or(deps);
        issues.update(cx, |v, cx| v.host_sessions(handle, deps, sink, cx));
        let sub = cx.subscribe_in(&issues, window, |this, _, ev: &IssuesEvent, window, cx| match ev {
            IssuesEvent::Command(kind) => this.run(kind.clone(), window, cx),
            // A click in the pane's terminal moves the window's focus model
            // to the main area, so the sidebar stops taking keys.
            IssuesEvent::Focused => {
                this.client.focus_main();
                cx.notify();
            }
        });
        self._view_subscriptions.push(sub);
        let sub = cx.observe_in(&issues, window, |this, _, window, cx| this.sync_detail_over(window, cx));
        self._view_subscriptions.push(sub);
        if let Some(filter) = self.pending_workflow_filter.take() {
            issues.update(cx, |v, cx| v.restore_workflow_filter(Some(filter), cx));
        }
        self.issues = Some(issues.clone());
        self.update_task_rows(cx);
        issues
    }

    /// Hand the board the sidebar's workspace and review rows: which agent
    /// works the task its pane shows.
    fn update_task_rows(&mut self, cx: &mut Context<Self>) {
        if let Some(issues) = self.issues.clone() {
            let (workspaces, reviews) = (self.model.workspaces.clone(), self.model.reviews.clone());
            issues.update(cx, |v, cx| v.set_task_rows(workspaces, reviews, cx));
        }
    }

    /// Tell the board whether the window shows it (or its pane floating
    /// over another screen), and what that screen shows: a hidden pane
    /// detaches its session, and a pane over a screen already showing it
    /// leaves it out.
    fn sync_issues_shown(&mut self, cx: &mut Context<Self>) {
        let Some(issues) = self.issues.clone() else {
            return;
        };
        let ready = matches!(self.boot, Boot::Ready(_)) && !self.chooser;
        let board = ready && matches!(self.route, Route::View) && matches!(self.client.view(), View::Issues);
        let floating = ready && issues.read(cx).is_floating();
        let mut main_shows = MainShows::default();
        if floating {
            main_shows.workspace = self
                .workspace
                .as_ref()
                .filter(|w| w.read(cx).is_shown())
                .and_then(|w| w.read(cx).workspace().map(str::to_string));
            main_shows.review_task = self
                .review
                .as_ref()
                .filter(|r| matches!(self.route, Route::Review) && r.read(cx).is_shown())
                .and_then(|r| r.read(cx).task_id().map(str::to_string));
        }
        issues.update(cx, |v, cx| v.set_shown(board || floating, main_shows, cx));
    }

    /// The orchestrator chat for the window's project, created on first use.
    fn ensure_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<ChatView> {
        if let Some(chat) = &self.chat {
            return chat.clone();
        }
        let project = self.project.clone();
        let handle = window.window_handle();
        let sink = self._feed.as_ref().map(|f| f.session_sink());
        let deps = ChatDeps::live(&project);
        #[cfg(any(test, feature = "visual-check"))]
        let deps = self.chat_deps.as_ref().map(|make| make()).unwrap_or(deps);
        let chat = cx.new(|cx| ChatView::orchestrator(&project, handle, deps, sink, cx));
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
        self._view_subscriptions.push(sub);
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
        let deps = self.workspace_deps.clone().unwrap_or(deps);
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
            WorkspaceEvent::OpenTask(id) => this.open_task_over(id, window, cx),
        });
        self._view_subscriptions.push(sub);
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
            ReviewEvent::OpenTask(id) => this.open_task_over(id, window, cx),
        });
        self._view_subscriptions.push(sub);
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

    /// Open the review `info` describes in the main area. The review the
    /// main area already shows (its placeholder, put up the moment it was
    /// asked for) is filled in where it stands: the push it started runs on.
    fn open_review(&mut self, info: ReviewInfo, window: &mut Window, cx: &mut Context<Self>) {
        self.quiet_review = None;
        let row = self.review_row(&info.task_id);
        if self.shown_review(cx).as_deref() == Some(info.task_id.as_str()) {
            if let Some(review) = self.review.clone() {
                review.update(cx, |r, cx| r.show(info, row, cx));
            }
            cx.notify();
            return;
        }
        self.status = None;
        self.banner = None;
        let review = self.ensure_review(window, cx);
        let hold = self.push_ahead(Occupant::Review);
        self.set_route(Route::Review);
        review.update(cx, |r, cx| {
            r.hold_content(hold, cx);
            r.show(info, row, cx)
        });
        self.client.focus_main();
        self.sync_chat(window, cx);
        self.focus_main_view(window, cx);
        cx.notify();
    }

    /// Put review `task` up at once, before the load flow has resolved it
    /// (`dt-instant-panel-push`): the panel takes the column with what the
    /// sidebar row knows and placeholders for the rest, so the push starts on
    /// the next frame. The resolve fills it in ([`Self::open_review`]); a
    /// failure, or a slot prompt dismissed, takes it back out
    /// ([`Self::abandon_pending_review`]).
    fn open_review_pending(&mut self, task: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.status = None;
        self.banner = None;
        let row = self.review_row(task);
        let review = self.ensure_review(window, cx);
        let hold = self.push_ahead(Occupant::Review);
        self.set_route(Route::Review);
        review.update(cx, |r, cx| {
            r.hold_content(hold, cx);
            r.show_pending(task, row, cx)
        });
        self.client.focus_main();
        self.sync_chat(window, cx);
        self.focus_main_view(window, cx);
        cx.notify();
    }

    /// The review put up before it resolved won't open after all (the load
    /// failed, or the slot prompt was dismissed): the push reverses from
    /// wherever it stands, back to the view it was opened over.
    fn abandon_pending_review(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let pending = matches!(self.route, Route::Review)
            && self.review.as_ref().is_some_and(|r| r.read(cx).is_pending());
        if pending {
            self.leave_review(window, cx);
        }
    }

    /// Whether showing `panel` runs the column's push (it isn't there yet, or
    /// is still on its way): its content then waits for the push to settle.
    fn push_ahead(&self, panel: Occupant) -> bool {
        self.column.target() != panel || self.column_moving
    }

    /// The push has settled: the panel in the column attaches the content
    /// that waited for it.
    fn release_settled(&mut self, cx: &mut Context<Self>) {
        match self.column.target() {
            Occupant::Review => {
                if let Some(review) = self.review.clone().filter(|r| r.read(cx).is_holding()) {
                    review.update(cx, |r, cx| r.release_content(cx));
                }
            }
            Occupant::Workspace => {
                if let Some(ws) = self.workspace.clone().filter(|w| w.read(cx).is_holding()) {
                    ws.update(cx, |w, cx| w.release_content(cx));
                }
            }
            Occupant::Settings => {}
            Occupant::Sidebar => {
                // A chat that waited for the pop: one more frame attaches it.
                let chat_waits = self.chat_wanted() && self.chat.as_ref().is_none_or(|c| !c.read(cx).is_shown());
                if chat_waits {
                    cx.notify();
                }
            }
        }
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
        // Left before it opened: whatever the load flow still has running for
        // it opens nothing.
        if let Some(task) = self.review.as_ref().and_then(|r| r.read(cx).task_id().map(str::to_string)) {
            self.review_loader.cancel(&task);
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
            self.set_route(Route::View);
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
    /// while a review is open, the workspace panel while a workspace working
    /// a task is shown, else the sidebar), starting the push when that
    /// changes: a slide, or a crossfade under Reduce Motion.
    fn sync_column(&mut self, now: Instant, cx: &App) {
        let workspace_shown = matches!(self.route, Route::View)
            && workspace_name(self.client.view()).is_some()
            && self.workspace.as_ref().is_some_and(|w| w.read(cx).has_panel());
        let want = if matches!(self.route, Route::Settings(_)) {
            Occupant::Settings
        } else if matches!(self.route, Route::Review) && self.review.is_some() {
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
            && !self.chooser
            // A switch builds (and attaches) it after its first frame.
            && self.switch_hold.is_none()
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
            // Coming back from a panel: the chat attaches once the column's
            // pop has settled (the shell's next frame re-syncs it), not in
            // its frames.
            if !chat.read(cx).is_shown() && !self.push_ahead(Occupant::Sidebar) {
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
        // The board's pane attaches its session only while it is on screen.
        self.sync_issues_shown(cx);
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
            || self
                .issues
                .as_ref()
                .is_some_and(|v| v.read(cx).session_focused(window, cx))
    }

    /// Start the Activity feed's reader for the window's project, so its
    /// first read is done before the feed is first shown.
    fn prefetch_activity(&mut self) {
        if self.prefetch_activity && !self.chooser && self.activity.is_none() {
            self.activity_reader = Some(ActivityReader::spawn(self.project.clone()));
        }
    }

    /// Start the Activity reader as a live window does on open
    /// (`--bench-activity`).
    #[cfg(feature = "visual-check")]
    pub fn bench_prefetch_activity(&mut self) {
        self.prefetch_activity = true;
        self.prefetch_activity();
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
        let activity = match self.activity_reader.take() {
            Some(reader) => cx.new(|cx| ActivityView::with_reader(project, reader, cx)),
            None => cx.new(|cx| ActivityView::new(project, cx)),
        };
        let sub = cx.subscribe_in(&activity, window, |this, _, ev: &ActivityEvent, window, cx| {
            match ev {
                ActivityEvent::OpenTask(id) => this.open_task(id, window, cx),
                ActivityEvent::Command(kind) => this.run(kind.clone(), window, cx),
            }
        });
        self._view_subscriptions.push(sub);
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
        self._view_subscriptions.push(sub);
        self.machines = Some(machines.clone());
        machines
    }

    /// The Changelog page for the window's project, created on first use.
    /// An issue chip opens the issue's detail on the board.
    fn ensure_changelog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<ChangelogView> {
        if let Some(changelog) = &self.changelog {
            return changelog.clone();
        }
        let project = self.project.clone();
        let changelog = cx.new(|cx| ChangelogView::new(project, cx));
        let sub = cx.subscribe_in(&changelog, window, |this, _, ev: &ChangelogEvent, window, cx| {
            let ChangelogEvent::OpenIssue(id) = ev;
            this.open_task(id, window, cx);
        });
        self._view_subscriptions.push(sub);
        self.changelog = Some(changelog.clone());
        changelog
    }

    /// Show the Changelog with entry `id` in view (the palette's Changelog
    /// result).
    fn open_changelog_entry(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.run(CommandKind::ShowView(View::Changelog), window, cx);
        let changelog = self.ensure_changelog(window, cx);
        changelog.update(cx, |v, cx| v.reveal(&id, window, cx));
    }

    /// Show task `id`'s detail: route to the board and open it there.
    fn open_task(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.run(CommandKind::ShowView(View::Issues), window, cx);
        let issues = self.ensure_issues(window, cx);
        issues.update(cx, |v, cx| v.open_task(id, cx));
    }

    /// Show task `id`'s detail over the main area, which stays as it is
    /// under it (the Review and Workspace panels' More). Closing it hands
    /// the keys back to the main area ([`Self::sync_detail_over`]).
    fn open_task_over(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let shown = self.shown(cx);
        if matches!(shown, Shown::Settings | Shown::View(View::Issues)) {
            return self.open_task(id, window, cx);
        }
        let issues = self.ensure_issues(window, cx);
        issues.update(cx, |v, cx| v.open_task_over(id, cx));
        self.detail_over = Some(shown);
        cx.notify();
    }

    /// The board changed while its detail is up over the main area: the
    /// panel under it shows the task's title and description as the board
    /// reads them (an edit made in the detail), and once the detail closes
    /// the keys go back to what the main area shows.
    fn sync_detail_over(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.detail_over.is_none() {
            return;
        }
        let Some(issues) = self.issues.clone() else {
            self.detail_over = None;
            return;
        };
        let text = issues.update(cx, |v, _| v.detail_text());
        if let Some((task, title, body)) = text {
            if let Some(review) = self.review.clone().filter(|_| matches!(self.route, Route::Review)) {
                review.update(cx, |r, cx| r.refresh_task_text(&task, &title, &body, cx));
            } else if let Some(ws) = self.workspace.clone() {
                ws.update(cx, |w, cx| w.refresh_task_text(&task, &title, &body, cx));
            }
        }
        if issues.read(cx).overlay_open() {
            return;
        }
        self.detail_over = None;
        issues.update(cx, |v, cx| v.land(false, cx));
        self.focus_main_view(window, cx);
        cx.notify();
    }

    /// The main area moved on from what the detail opened over (a sidebar
    /// row, a command): the detail goes with it, or onto the board when the
    /// board is what shows now.
    fn settle_detail_over(&mut self, cx: &mut Context<Self>) {
        let Some(over) = self.detail_over.clone() else {
            return;
        };
        let shown = self.shown(cx);
        if shown == over {
            return;
        }
        self.detail_over = None;
        if let Some(issues) = self.issues.clone() {
            let onto_board = shown == Shown::View(View::Issues);
            issues.update(cx, |v, cx| v.land(!onto_board, cx));
        }
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
            View::Changelog if self.changelog.is_some() => {
                if let Some(changelog) = &self.changelog {
                    changelog.update(cx, |v, _| v.request_focus());
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
        let current = self.switcher_projects(cx).iter().position(|p| p.name == self.project);
        self.switcher.open(current.unwrap_or(0), (self.clock)());
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

    /// The projects the switcher lists (the catalog's, empty until it has
    /// loaded).
    fn switcher_projects(&self, cx: &App) -> Vec<ProjectEntry> {
        cx.try_global::<Catalog>().map(|c| c.projects()).unwrap_or_default()
    }

    /// [`Self::switcher_projects`] as the switcher and the chooser draw
    /// them: in the same order, each with its dot (the chooser has no
    /// current project).
    fn switcher_rows(&self, cx: &App) -> Vec<shelbi_app::overlay::palette::ProjectEntry> {
        let current = if self.chooser { "" } else { self.project.as_str() };
        cx.try_global::<Catalog>().map(|c| c.rows(current)).unwrap_or_default()
    }

    /// A key while the switcher is open: ↑↓ (and Tab) move its keyboard row,
    /// Enter picks it, Esc closes. `false` for any other key (it goes on as
    /// usual).
    fn switcher_key(&mut self, code: Key, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let projects = self.switcher_projects(cx);
        // The projects, then "Add project…".
        let count = projects.len() + 1;
        match code {
            Key::Up | Key::BackTab => self.switcher.step(-1, count),
            Key::Down | Key::Tab => self.switcher.step(1, count),
            Key::Esc => self.switcher.close((self.clock)()),
            Key::Enter => {
                let event = match projects.get(self.switcher.highlight) {
                    Some(p) => SidebarEvent::SwitchTo(p.name.clone()),
                    None => SidebarEvent::AddProject,
                };
                self.sidebar_event(event, window, cx);
            }
            _ => return false,
        }
        true
    }

    /// A key on the chooser: ↑↓ (and Tab) move its row, Enter opens it (the
    /// last row is Add Project). `false` for any other key.
    fn chooser_key(&mut self, code: Key, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let projects = self.switcher_projects(cx);
        let count = projects.len() + 1;
        match code {
            Key::Up | Key::BackTab => {
                self.chooser_highlight = (self.chooser_highlight + count - 1) % count
            }
            Key::Down | Key::Tab => self.chooser_highlight = (self.chooser_highlight + 1) % count,
            Key::Enter => match projects.get(self.chooser_highlight) {
                Some(p) => {
                    let project = p.name.clone();
                    self.run(CommandKind::SwitchProject { project }, window, cx)
                }
                None => self.run(CommandKind::AddProject, window, cx),
            },
            _ => return false,
        }
        true
    }

    /// The empty-state chooser: no project is open in this window. Every
    /// project (the catalog's), and Add Project.
    fn render_chooser(&self, theme: &Theme, cx: &mut Context<Self>) -> gpui::AnyElement {
        let projects = self.switcher_rows(cx);
        let add_ix = projects.len();
        let rows = projects.iter().enumerate().map(|(ix, p)| {
            let name = p.slug.clone();
            style::menu_row(format!("chooser-{}", p.slug), ix == self.chooser_highlight, theme)
                .child(crate::views::project_menu::dot(p.indicator, theme))
                .child(p.label.clone())
                .on_click(cx.listener(move |this, _, window, cx| {
                    let project = name.clone();
                    this.run(CommandKind::SwitchProject { project }, window, cx)
                }))
        });
        let add = style::menu_row("chooser-add", add_ix == self.chooser_highlight, theme)
            .child(heroicon(icons::PLUS).text_color(theme.icon))
            .child("Add Project…")
            .on_click(cx.listener(|this, _, window, cx| this.run(CommandKind::AddProject, window, cx)));
        div()
            .id("chooser")
            .debug_selector(|| "chooser".into())
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .size_full()
            .p_8()
            .overflow_y_scroll()
            .bg(theme.canvas)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .w(px(360.0))
                    .max_w_full()
                    .child(style::heading("No project open", theme))
                    .child(
                        div()
                            .text_color(theme.muted)
                            .child("Choose a project for this window."),
                    )
                    .child(div().flex().flex_col().gap_1().children(rows).child(add)),
            )
            .into_any_element()
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
            Some("This stops the project's agents. Other projects keep running."),
            &["Quit Project", "Cancel"],
            cx,
        );
        let project = self.project.clone();
        let quit_project = project.clone();
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
                if let Some(issues) = &this.issues {
                    issues.update(cx, |v, cx| v.note_deliberate_close(true, cx));
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
                // Every window that showed it moves on (this one included),
                // staying open.
                Ok(()) => {
                    let me = window.window_handle().window_id();
                    for other in windows_showing(&quit_project, Some(me), cx) {
                        if other.window_id() != me {
                            let quit = quit_project.clone();
                            cx.defer(move |cx| {
                                let _ = other.update(cx, |w, window, cx| {
                                    w.after_quit_project(quit, window, cx)
                                });
                            });
                        }
                    }
                    this.after_quit_project(quit_project.clone(), window, cx);
                }
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
                    if let Some(issues) = &this.issues {
                        issues.update(cx, |v, cx| v.note_deliberate_close(false, cx));
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

    /// Open Settings on `page`, or on the page last used. Its sidebar takes
    /// the column (the push the task panels use) and the keys.
    fn open_settings(&mut self, page: Option<SettingsPage>, window: &mut Window, cx: &mut Context<Self>) {
        self.settings_nav.open(page);
        let page = self.settings_nav.page();
        match &self.route {
            Route::Settings(view) => view.update(cx, |v, cx| v.show_page(page, cx)),
            route => {
                self.settings_over_review = matches!(route, Route::Review) && self.review.is_some();
                let project = self.project.clone();
                let view = cx.new(|cx| {
                    let mut view = SettingsView::new(&project, window, cx);
                    view.show_page(page, cx);
                    view
                });
                // A saved switch applies to this window's next notice at once,
                // and a rebind to its keys and menus.
                let sub = cx.subscribe_in(&view, window, |this, _, event: &SettingsEvent, window, cx| match event {
                    SettingsEvent::Saved => this.load_notify_prefs(cx),
                    SettingsEvent::KeysSaved(keymaps) => this.adopt_keymaps((**keymaps).clone(), cx),
                    SettingsEvent::Leave => {
                        this.focus_handle.focus(window, cx);
                        cx.notify();
                    }
                    SettingsEvent::EditWorkflow(name) => this.edit_workflow(name.clone(), window, cx),
                    SettingsEvent::EditAgent(name) => this.edit_agent(name.clone(), window, cx),
                    SettingsEvent::ShowPage(page) => this.show_settings_page(*page, cx),
                    SettingsEvent::OpenPoolForm(intent) => this.open_pool_form(intent.clone(), window, cx),
                    SettingsEvent::PoolChanged => this.pool_changed(cx),
                    SettingsEvent::EditorClosed => {
                        // The settings sidebar comes back on the page the
                        // editor closed onto (the agent editor's links go
                        // to Workflows).
                        if let Route::Settings(view) = &this.route {
                            let page = view.read(cx).page();
                            this.settings_nav.show(page);
                        }
                        this.focus_handle.focus(window, cx);
                        cx.notify();
                    }
                });
                self._view_subscriptions.push(sub);
                self.set_route(Route::Settings(view));
            }
        }
        self.sync_chat(window, cx);
        self.banner = None;
        self.client.focus_main();
        self.focus_main_view(window, cx);
        cx.notify();
    }

    /// Show a Settings page (its row, ⌘N, Enter on its row). Not while the
    /// workflow editor holds the window: it has no sidebar to pick from.
    fn show_settings_page(&mut self, page: SettingsPage, cx: &mut Context<Self>) {
        if self.editing_workflow(cx) {
            return;
        }
        self.settings_nav.show(page);
        if let Route::Settings(view) = &self.route {
            view.update(cx, |v, cx| v.show_page(page, cx));
        }
        cx.notify();
    }

    /// A key for the settings sidebar (the window holds focus, no field
    /// does): Esc leaves Settings, ↑ / ↓ (and the sidebar's other
    /// bindings) move the selection, Enter opens it. Returns whether it was
    /// one.
    fn settings_key(&mut self, chord: shelbi_state::keymap::KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        // The workflow editor: Esc is its Back; it has no sidebar keys.
        if let Route::Settings(view) = &self.route {
            if view.read(cx).editing() {
                if chord.code == Key::Esc && chord.mods.is_empty() {
                    view.update(cx, |v, cx| v.editor_back(cx));
                    return true;
                }
                return false;
            }
        }
        if chord.code == Key::Esc && chord.mods.is_empty() {
            self.leave_settings(window, cx);
            return true;
        }
        if self.collapsed {
            return false;
        }
        match (self.keymaps.sidebar.dispatch(chord), chord.code) {
            (Some(SidebarAction::NavUp), _) | (_, Key::BackTab) => self.settings_nav.step(false),
            (Some(SidebarAction::NavDown), _) | (_, Key::Tab) => self.settings_nav.step(true),
            (Some(SidebarAction::Activate), _) => {
                if self.settings_nav.activate() {
                    let page = self.settings_nav.page();
                    self.show_settings_page(page, cx);
                }
            }
            _ => return false,
        }
        true
    }

    /// Leave Settings for what it was opened over (the back button, Esc):
    /// the app sidebar comes back into its column (the push's reverse),
    /// selecting that view's row, and takes the keys again, as leaving a
    /// review does. Opened over a review, the review reopens.
    fn leave_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !matches!(self.route, Route::Settings(_)) {
            return;
        }
        let over_review = std::mem::take(&mut self.settings_over_review);
        if let Some(info) = self.review.as_ref().and_then(|r| r.read(cx).info()).filter(|_| over_review) {
            self.open_review(info, window, cx);
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
        self.reveal.set(!self.collapsed, (self.clock)(), (self.reduce_motion)());
        if self.collapsed {
            self.client.focus_main();
        }
        self.window_state.sidebar_collapsed = self.collapsed;
        self.schedule_save(cx);
        self.sync_menus(cx);
        cx.notify();
    }

    /// Save the window's slot off the UI thread, debounced so a window drag
    /// (a bounds change per frame) writes once when it settles.
    fn schedule_save(&mut self, cx: &mut Context<Self>) {
        self.save_generation += 1;
        let generation = self.save_generation;
        let timer = cx
            .background_executor()
            .timer(std::time::Duration::from_millis(400));
        cx.spawn(async move |this, cx| {
            timer.await;
            this.update(cx, |this, cx| {
                if this.save_generation == generation {
                    this.record_slot(cx);
                }
            })
            .ok();
        })
        .detach();
    }

    // --- overlays ----------------------------------------------------------

    /// Show `overlay`. Keys go to it until it closes; the routed view and the
    /// focus to return to are left as they were.
    fn open_overlay(&mut self, overlay: ActiveOverlay, window: &mut Window, cx: &mut Context<Self>) {
        self.client.open_overlay(overlay.nav());
        if self.overlay.is_none() {
            self.overlay_opened = Some((self.clock)());
        }
        self.overlay = Some(overlay);
        self.switcher.close((self.clock)());
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    /// Close the overlay and hand keys back to where focus was.
    fn close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let closed = self.overlay.take();
        self.client.close_overlay();
        match self.client.focus() {
            Focus::Main => self.focus_main_view(window, cx),
            Focus::Sidebar => self.focus_handle.focus(window, cx),
        }
        // The slot prompt (or its busy / no-slot report) closed without a
        // slot chosen (choosing one takes the overlay first): the review
        // waiting behind it doesn't open.
        if matches!(closed, Some(ActiveOverlay::ReviewConfirm { .. })) {
            self.abandon_pending_review(window, cx);
        }
        cx.notify();
    }

    /// The palette's entries for the current state: the registry over the
    /// live sidebar model and the last [`PaletteInputs`], in the desktop's
    /// copy and order.
    pub(crate) fn palette_entries(&self) -> Vec<shelbi_palette::Entry> {
        crate::views::palette::present(CommandRegistry::new().entries(&self.command_model()))
    }

    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.palette_entries();
        let projects = self.palette_inputs.projects.clone();
        // Typing searches the board and the Changelog too: what this window
        // has read, and (for a page it hasn't opened yet) the board index
        // and the changelog read off the UI thread.
        let (issues, changelog) = self.palette_corpus_held(cx);
        let complete = issues.is_some() && changelog.is_some();
        let corpus = SearchCorpus::new(issues.unwrap_or_default(), changelog.unwrap_or_default());
        let search = self.palette_search(corpus);
        self.open_overlay(ActiveOverlay::palette(entries, projects, search), window, cx);
        // Re-read the inputs off the UI thread; the open palette picks them up.
        self.refresh_palette_inputs(cx);
        if !complete {
            self.read_palette_corpus(cx);
        }
        self.poll_palette_search(cx);
    }

    /// Where the palette's searches run: their own thread (a test window
    /// searches inline, so what it draws doesn't wait on a thread).
    fn palette_search(&self, corpus: SearchCorpus) -> PaletteSearch {
        #[cfg(any(test, feature = "visual-check"))]
        if self.tap.is_some() {
            return PaletteSearch::inline(corpus);
        }
        PaletteSearch::background(corpus, None)
    }

    /// The issues and Changelog entries this window holds, each `None`
    /// until its page has read them.
    fn palette_corpus_held(&self, cx: &App) -> (Option<Vec<IssueDoc>>, Option<Vec<ChangelogEntryView>>) {
        let issues = self.issues.as_ref().and_then(|v| {
            let board = v.read(cx).board_input();
            (!board.loading).then(|| issue_docs(&board.tasks(), &board.project_statuses, board.rules()))
        });
        let changelog = self.changelog.as_ref().and_then(|v| v.read(cx).entries());
        (issues, changelog)
    }

    /// Read the palette's corpus off the UI thread (the board index and the
    /// changelog, never GitHub) and hand the open palette whatever part this
    /// window still doesn't hold.
    fn read_palette_corpus(&mut self, cx: &mut Context<Self>) {
        #[cfg(any(test, feature = "visual-check"))]
        if self.tap.is_some() {
            return;
        }
        let project = self.project.clone();
        let reading = project.clone();
        let read = cx
            .background_executor()
            .spawn(async move { shelbi_app::overlay::palette_search::read_corpus(&project) });
        cx.spawn(async move |this, cx| {
            let read = read.await;
            this.update(cx, |this, cx| {
                if !this.shows(&reading) {
                    return;
                }
                let (issues, changelog) = this.palette_corpus_held(cx);
                let corpus = SearchCorpus::new(
                    issues.unwrap_or_else(|| read.issues.to_vec()),
                    changelog.unwrap_or_else(|| read.changelog.to_vec()),
                );
                if let Some(ActiveOverlay::Palette { palette, .. }) = &mut this.overlay {
                    palette.set_corpus(corpus);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Take the open palette's search hits as they land (the search runs on
    /// its own thread), redrawing when they change; stops once the palette
    /// closes.
    fn poll_palette_search(&mut self, cx: &mut Context<Self>) {
        self._palette_poll = Some(cx.spawn(async move |this, cx| loop {
            cx.background_executor().timer(PALETTE_POLL).await;
            let open = this.update(cx, |this, cx| match &mut this.overlay {
                Some(ActiveOverlay::Palette { palette, .. }) => {
                    if palette.poll_search() {
                        cx.notify();
                    }
                    true
                }
                _ => false,
            });
            if !matches!(open, Ok(true)) {
                break;
            }
        }));
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
            OverlayEvent::Stay => {}
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
                // A slot was chosen: the review behind the prompt stays up
                // while it loads.
                self.overlay = None;
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
    /// refresh the sidebar so its Zen band (or, on a failure, the error
    /// badge) follows. Success flashes nothing: the band is the indicator.
    fn run_zen(&mut self, project: String, intro: Option<(bool, bool)>, cx: &mut Context<Self>) {
        let work = cx.background_executor().spawn(async move {
            let result = match intro {
                Some((confirmed, dont_show_again)) => {
                    shelbi_app::zen::apply_intro(&project, confirmed, dont_show_again)
                }
                None => shelbi_app::zen::toggle(&project),
            };
            shelbi_app::zen::report(&project, result)
        });
        cx.spawn(async move |this, cx| {
            let message = work.await;
            this.update(cx, |this, cx| {
                if let Some(message) = message {
                    this.flash(message, cx);
                }
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

    /// `AddProject`: the Add Project wizard's window (or the open one to
    /// the front). This window is where a created project opens.
    fn open_add_project(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let from = window.window_handle().downcast::<ShelbiWindow>();
        // After this update, so the wizard's first frame isn't nested in ours.
        cx.defer(move |cx| crate::views::add_project::show(Origin::Workspace, from, cx));
    }

    /// A project the Add Project wizard created: this window comes up on its
    /// Orchestrator. Settings or a review open over the old project don't
    /// follow the switch, and nothing saved for the id is restored (the
    /// caller has cleared the catalog's copy). Already showing it (a window
    /// just opened on it, or an earlier Okay), it lands on the Orchestrator
    /// all the same.
    pub(crate) fn project_added(&mut self, slug: &str, window: &mut Window, cx: &mut Context<Self>) {
        window.activate_window();
        self.flash(format!("Created project {slug}."), cx);
        self.settings_over_review = false;
        if !matches!(self.route, Route::View) {
            self.set_route(Route::View);
        }
        if self.shows(slug) {
            self.adopt_view_state(ProjectViewState::default());
            cx.notify();
        } else {
            self.switch_to(slug, window, cx);
        }
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
        self.open_review_pending(&task_id, window, cx);
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
            // A review a switch reopened that would need a prompt (it left
            // its slot meanwhile): back out quietly instead.
            LoadStep::AskSlot { task, .. } | LoadStep::NoSlots { task, .. } | LoadStep::Busy { task, .. }
                if self.quiet_review.as_deref() == Some(task.as_str()) =>
            {
                self.quiet_review = None;
                self.review_loader.cancel(&task);
                self.abandon_pending_review(window, cx);
            }
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
                // Back out first: returning to the view clears the banner.
                self.abandon_pending_review(window, cx);
                // A review a switch reopened that's gone says nothing.
                if self.quiet_review.take().is_none() {
                    self.banner = Some(message.into());
                }
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
        // Contact Us and Connect to Remote Session are modal like a
        // workspace dialog: ⌘W cancels them, the rest wait (their own menu
        // items included: they're already open).
        if self.feedback.is_some() || self.connect.is_some() {
            match &invoke.target {
                MenuAction::App(AppCommand::CloseWindow) => self.dismiss_dialog(cx),
                MenuAction::App(AppCommand::Minimize) => window.minimize_window(),
                MenuAction::App(AppCommand::Zoom) => window.zoom_window(),
                // About and Acknowledgements are their own windows: they open
                // over a modal too; every other registry command waits.
                MenuAction::Command(id) => {
                    menus::run_windowless_command(id, cx);
                }
                MenuAction::App(AppCommand::Settings | AppCommand::CommandPalette) => {}
                MenuAction::App(other) => {
                    menus::run_app_command(*other, cx);
                }
            }
            return;
        }
        // A workspace dialog is modal: ⌘W closes it (discarding its draft);
        // the menus' commands wait until it's gone.
        if let Some((dialog, _)) = &self.pool_dialog {
            match &invoke.target {
                MenuAction::App(AppCommand::CloseWindow) => dialog.update(cx, |d, cx| d.dismiss(cx)),
                MenuAction::App(AppCommand::Minimize) => window.minimize_window(),
                MenuAction::App(AppCommand::Zoom) => window.zoom_window(),
                // About and Acknowledgements are their own windows: they open
                // over a modal too; every other registry command waits.
                MenuAction::Command(id) => {
                    menus::run_windowless_command(id, cx);
                }
                MenuAction::App(AppCommand::Settings | AppCommand::CommandPalette) => {}
                MenuAction::App(other) => {
                    menus::run_app_command(*other, cx);
                }
            }
            return;
        }
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
                AppCommand::Settings => self.open_settings(None, window, cx),
                AppCommand::CloseWindow => self.close_window(window, cx),
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
        // The terminal font zoom (⌘+ ⌘− ⌘0) is app-wide and no overlay or
        // view binds the chords, so it applies wherever focus is.
        if let Some(step) = chord.as_ref().and_then(font_zoom) {
            TerminalSettings::zoom(step, cx);
            cx.stop_propagation();
            return;
        }
        if self.overlay.is_some() {
            self.overlay_key(event, window, cx);
            return;
        }
        // A workspace dialog is modal: it takes its keys ahead of everything
        // (`WorkspaceDialog::key`), and nothing behind it gets any.
        if self.modal_open() {
            return;
        }
        let Some(chord) = chord else {
            return;
        };
        // File ▸ New Window, ahead of a focused terminal or the chooser.
        if is_new_window(&chord) {
            self.run(CommandKind::NewWindow, window, cx);
            cx.stop_propagation();
            return;
        }
        if self.chooser {
            if self.chooser_key(chord.code, window, cx) {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        // The sidebar's own chords, from anywhere in the window.
        if let Some(nav) = nav_chord(&chord) {
            self.nav_chord(nav, window, cx);
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if shelbi_app::overlay::opens_palette(&self.keymaps, &chord) {
            self.open_palette(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.switcher.is_open() && self.switcher_key(chord.code, window, cx) {
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
                // Ctrl+L's redraw half: the TUI clears and repaints its
                // terminal; here a fresh frame is all it takes.
                self.focus_main(window, cx);
                window.refresh();
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
        if matches!(self.route, Route::Settings(_)) {
            // The settings sidebar's keys, while no field on the page has
            // focus (a typed `j` is the field's).
            if self.focus_handle.is_focused(window) && self.settings_key(chord, window, cx) {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        if self.client.focus() != Focus::Sidebar || self.collapsed || self.panel_in_column() {
            // The main area's views take their own keys once they land; the
            // sidebar takes none while a panel stands in for it.
            return;
        }
        match (self.keymaps.sidebar.dispatch(chord), chord.code) {
            (Some(SidebarAction::NavUp), _) | (_, Key::BackTab) => self.step_selection(false, cx),
            (Some(SidebarAction::NavDown), _) | (_, Key::Tab) => self.step_selection(true, cx),
            (Some(SidebarAction::Activate), _) => self.activate_selection(window, cx),
            (Some(SidebarAction::Refresh), _) => {
                if let Some(feed) = &self._feed {
                    feed.request();
                }
            }
            // The sidebar's `,`, the TUI's Settings key; ⌘, works anywhere.
            (Some(SidebarAction::OpenSettings), _) => self.open_settings(None, window, cx),
            // `q` / Ctrl+C close the TUI; a window closes with ⌘W instead.
            (Some(SidebarAction::Quit), _) | (None, _) => return,
        }
        cx.stop_propagation();
        cx.notify();
    }

    // --- rendering ---------------------------------------------------------

    /// The main area. `titlebar`: how far its titlebar row is open (the
    /// sidebar hidden or going, no panel holding the column), with the
    /// button that shows the sidebar.
    fn render_main(&self, theme: &Theme, titlebar: MainTitlebar, cx: &mut Context<Self>) -> impl IntoElement {
        let detail_over = self
            .detail_over
            .as_ref()
            .and_then(|_| self.issues.clone())
            .filter(|issues| issues.read(cx).is_floating());
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
                    View::Changelog => self.changelog.clone().map(|v| v.into_any_element()),
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
                .border_color(theme.divider)
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
            .bg(theme.canvas)
            // The collapsed sidebar's titlebar row, room for the traffic
            // lights (opening as the sidebar slides out).
            .when(titlebar.open > 0.0, |main| {
                main.child(div().flex_none().h(px(sidebar::TITLEBAR_H * titlebar.open)))
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
                    notice(message, Hue::Yellow).child(
                        style::icon_button("dismiss-banner", icons::X_MARK, IconButtonSize::Small, false, theme)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.banner = None;
                                cx.notify();
                            })),
                    ),
                )
            })
            .child(body)
            // Show the sidebar: beside the traffic lights and centered on
            // them, put in the window rather than the row, so it stays put
            // while the main area's edge slides past it, and fading in.
            .when(titlebar.open > 0.0, |main| {
                let size = IconButtonSize::Default.px();
                main.child(
                    div()
                        .absolute()
                        .top(px((sidebar::TITLEBAR_H - size) / 2.0))
                        .left(px(sidebar::BESIDE_TRAFFIC_LIGHTS - titlebar.left))
                        .opacity(titlebar.open)
                        .child(
                            style::icon_button("expand-sidebar", icons::SIDEBAR, IconButtonSize::Default, false, theme)
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                        ),
                )
            })
            // The task detail over the Review or Workspace screen: the board
            // draws it, sized to the main area as it is over the board.
            .when_some(detail_over, |main, issues| main.relative().child(issues))
    }
}

/// The main area's titlebar row while the sidebar is hidden (or going).
#[derive(Debug, Clone, Copy)]
struct MainTitlebar {
    /// How much of the row is open, 0 to 1.
    open: f32,
    /// The main area's left edge in the window.
    left: f32,
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

/// A sidebar row by what it opens (`view:tasks`, `workspace:alpha`,
/// `review:T-1`, `machine:hub`): how a project's saved selection names it.
fn row_key(target: &SidebarTarget) -> String {
    match target {
        SidebarTarget::View(view) => format!("view:{}", view.as_view_id()),
        SidebarTarget::Workspace(name) => format!("workspace:{name}"),
        SidebarTarget::Review(task) => format!("review:{task}"),
        SidebarTarget::Machine(name) => format!("machine:{name}"),
    }
}

/// Whether the window takes `chord` away from a focused terminal: the focus
/// moves (Ctrl+H / Ctrl+L by default), the Zen toggle (Option+Z), every
/// palette chord (the configured one plus the reserved Ctrl+Space and Cmd+K),
/// New Window (⌘⇧N), the terminal font zoom (⌘+ ⌘− ⌘0), and the sidebar's
/// chords (⌘O ⌘I ⌘L, ⌘1 to ⌘9).
/// Everything else is the session's.
fn takes_from_terminal(keymaps: &Keymaps, chord: shelbi_state::keymap::KeyChord) -> bool {
    matches!(
        keymaps.global.dispatch(chord),
        Some(GlobalAction::FocusSidebar | GlobalAction::FocusMain | GlobalAction::ZenToggle)
    ) || shelbi_app::overlay::opens_palette(keymaps, &chord)
        || is_new_window(&chord)
        || font_zoom(&chord).is_some()
        || nav_chord(&chord).is_some()
}

/// The palette-open chord as the search box shows it (`⌃P`).
fn palette_chord(keymaps: &Keymaps) -> String {
    keymaps
        .global
        .first_chord_for(GlobalAction::OpenPalette)
        .map(|c| crate::keys::chord_text(c, DisplayStyle::Mac))
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
                if let Some(target) = self.target_at(ordinal) {
                    self.activate_target(target, window, cx);
                }
            }
            SidebarEvent::OpenSearch => self.open_palette(window, cx),
            SidebarEvent::ToggleSwitcher => {
                if self.switcher.is_open() {
                    self.switcher.close((self.clock)());
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
            SidebarEvent::CloseSwitcher => self.switcher.close((self.clock)()),
            SidebarEvent::SwitchTo(project) => {
                self.run(CommandKind::SwitchProject { project }, window, cx)
            }
            SidebarEvent::AddProject => {
                self.switcher.close((self.clock)());
                self.run(CommandKind::AddProject, window, cx)
            }
            SidebarEvent::ToggleSection(section) => self.toggle_section(section, cx),
            SidebarEvent::ToggleZen => self.run(CommandKind::ToggleZen, window, cx),
            SidebarEvent::OpenErrors => self.run(CommandKind::OpenErrorLog, window, cx),
            SidebarEvent::OpenSettings => self.open_settings(None, window, cx),
            SidebarEvent::SendFeedback => self.run(CommandKind::SendFeedback, window, cx),
            SidebarEvent::Collapse => self.toggle_sidebar(cx),
            SidebarEvent::LeaveSettings => self.leave_settings(window, cx),
            SidebarEvent::ShowSettingsPage(page) => self.show_settings_page(page, cx),
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
        self.settle_detail_over(cx);
        // A switch (or a new window): this frame paints the project's
        // sidebar; the next builds its main view and attaches its sessions.
        if let Some(hold) = self.switch_hold.as_mut() {
            if hold.rendered {
                let hold = self.switch_hold.take().expect("held");
                cx.defer_in(window, move |this, window, cx| this.finish_switch(hold, window, cx));
            } else {
                hold.rendered = true;
                window.request_animation_frame();
            }
        }
        let window_width = window.viewport_size().width.as_f32();
        let now = (self.clock)();
        let reduce_motion = (self.reduce_motion)();
        let switcher_phase = self.switcher.phase(now, reduce_motion);
        if self.switcher.animating(now, reduce_motion) {
            window.request_animation_frame();
        }
        let catalog_projects = switcher_phase.map(|_| self.switcher_rows(cx));
        // The sidebar's sections and machine groups folding open or closed.
        self.folds.settle(now);
        if self.folds.animating(now) {
            window.request_animation_frame();
        }
        // The leading column: the sidebar, or a review's or workspace's panel
        // in its place, both while the push between them runs (clipped to
        // the column).
        self.sync_column(now, cx);
        self.column.pace(now);
        let frame = self.column.frame(now);
        self.column_moving = frame.animating;
        // The nav selection chip: slide it to the selected row when the
        // selection moves (keys, clicks, route changes alike) while the
        // sidebar is in its column or leaving it; a sidebar coming back (or
        // hidden) has it placed on its row directly.
        let selection = self.selected_ordinal(cx);
        let chip = {
            let mut track = self.nav_track.borrow_mut();
            let key = selection.and_then(|ordinal| sidebar::selected_key(&self.sidebar_entries(), ordinal));
            let reduce = self.reduce_motion.clone();
            let sidebar_target = self.column.target() == Occupant::Sidebar;
            let place = self.collapsed || sidebar_target == frame.animating;
            track.select(key, now, move || place || reduce());
            let chip = track.frame(now);
            if chip.is_some() {
                window.request_animation_frame();
            }
            chip
        };
        // The settings sidebar's chip, likewise: placed while its column
        // moves, sliding between its rows at rest.
        let settings_chip = matches!(self.route, Route::Settings(_)).then(|| {
            let mut track = self.settings_track.borrow_mut();
            let key = SharedString::from(self.settings_nav.selected().selector());
            let reduce = self.reduce_motion.clone();
            let place = frame.animating;
            track.select(Some(key), now, move || place || reduce());
            let chip = track.frame(now);
            if chip.is_some() {
                window.request_animation_frame();
            }
            chip
        });
        if !frame.animating {
            self.release_settled(cx);
        }
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
        // The sidebar's hide or show: the column at its width for this frame,
        // whatever it holds laid out at its full width and slid left (clipped,
        // never reflowed), the main area following its edge. The window's
        // terminals hold their resizes until it settles.
        if self.reveal.shown() == self.collapsed {
            // `collapsed` changed other than by a toggle: no motion.
            self.reveal = SidebarReveal::new(!self.collapsed);
        }
        self.reveal.pace(now);
        let reveal = self.reveal.frame(now);
        if reveal.animating {
            window.request_animation_frame();
        }
        ResizeHold::set(window.window_handle().window_id(), reveal.animating, cx);
        let column_px = |collapsed: bool| {
            column_swap::column(&frame, collapsed, self.client.sidebar_width(), self.collapsed_panel_cols)
                .map(|c| sidebar::width_px(c.width_cols, window_width))
        };
        let placement = column_px(false).and_then(|shown_px| {
            sidebar_reveal::place(shown_px, column_px(true), reveal.position, self.reveal.shown())
        });
        let column_shown = placement.is_some();
        let column = placement.map(|place| {
            let width = place.content;
            let layer = |selector: &'static str, l: Layer| {
                div()
                    .id(selector)
                    .debug_selector(move || selector.into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(px(l.offset * width))
                    .w(px(width))
                    .opacity(l.opacity)
            };
            let sidebar = frame.sidebar.filter(|_| reveal.position > 0.0).map(|l| {
                layer("column-sidebar", l).child(sidebar::render(
                    SidebarProps {
                        model: &self.model,
                        collapsed: &self.collapsed_sections,
                        folds: &self.folds,
                        now,
                        selection,
                        link: self.link,
                        palette_chord: self.palette_chord.clone(),
                        switcher: catalog_projects.as_deref().zip(switcher_phase).map(
                            |(projects, phase)| sidebar::SwitcherProps {
                                projects,
                                highlight: self.switcher.highlight,
                                phase,
                            },
                        ),
                        switcher_button: self.switcher.button.clone(),
                        switcher_open: self.switcher.is_open(),
                        width,
                        scroll: self.sidebar_scroll.clone(),
                        track: self.nav_track.clone(),
                        chip,
                    },
                    &theme,
                    cx,
                ))
            });
            // A task panel keeps the sidebar's titlebar and footer around it.
            let review = frame.review.zip(self.review.clone()).map(|(l, review)| {
                layer("column-review", l).child(sidebar::render_panel_column(
                    review,
                    &self.model,
                    self.link,
                    &self.palette_chord,
                    &theme,
                    cx,
                ))
            });
            let workspace = frame.workspace.zip(self.workspace.clone()).map(|(l, ws)| {
                layer("column-workspace", l).child(sidebar::render_panel_column(
                    ws,
                    &self.model,
                    self.link,
                    &self.palette_chord,
                    &theme,
                    cx,
                ))
            });
            // The settings sidebar is a sidebar: it hides with the app's.
            let settings = frame.settings.filter(|_| reveal.position > 0.0).map(|l| {
                layer("column-settings", l).child(settings_nav::render(
                    self.settings_nav,
                    settings_chip.flatten(),
                    &self.settings_track,
                    settings_nav::ProjectSwitcher {
                        label: self.model.project_label.clone().into(),
                        menu: catalog_projects.as_deref().zip(switcher_phase).map(|(projects, phase)| {
                            sidebar::SwitcherProps {
                                projects,
                                highlight: self.switcher.highlight,
                                phase,
                            }
                        }),
                        button: self.switcher.button.clone(),
                        open: self.switcher.is_open(),
                        width,
                    },
                    &self.palette_chord,
                    &theme,
                    cx,
                ))
            });
            div()
                .id("leading-column")
                .debug_selector(|| "leading-column".into())
                .relative()
                .flex_none()
                .w(px(place.visible))
                .h_full()
                .overflow_hidden()
                .bg(theme.sidebar)
                .child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(place.offset))
                        .w(px(width))
                        .children(sidebar)
                        .children(review)
                        .children(workspace)
                        .children(settings),
                )
        });
        let divider_color = if self.dragging {
            theme.icon
        } else {
            theme.divider
        };
        // Only the Review frames draw the column's 1pt rule (the reviewer
        // starts at x 281); every other frame puts the main area right
        // after the column at x 280, the fill change the only edge.
        let rule = self.column.target() == Occupant::Review;
        // The collapsed main area's titlebar row opens as the sidebar leaves,
        // its Show button fixed beside the traffic lights.
        // Where the main area starts: a machine dialog centers over it.
        let main_left = placement.map_or(0.0, |p| p.visible + if rule { 1.0 } else { 0.0 });
        let titlebar = MainTitlebar {
            open: placement.map_or(1.0, |p| p.titlebar),
            left: placement.map_or(0.0, |p| p.visible + if rule { 1.0 } else { 0.0 }),
        };
        let handle = column_shown.then(|| {
            // The column's right edge: the rule (in the layout, when shown)
            // or nothing, with the grab room laid over the column's edge.
            // It resizes whichever panel the column shows. A drag lights the
            // edge either way.
            div()
                .relative()
                .w(px(if rule { 1.0 } else { 0.0 }))
                .h_full()
                .flex_none()
                // The rule over the canvas (which the window no longer
                // paints under it while translucent).
                .when(rule, |d| {
                    d.bg(theme.canvas)
                        .child(div().absolute().top_0().bottom_0().left_0().w(px(1.0)).bg(divider_color))
                })
                .when(!rule && self.dragging, |d| {
                    d.child(div().absolute().top_0().bottom_0().left(px(-1.0)).w(px(1.0)).bg(divider_color))
                })
                .child(
                div()
                    .id("sidebar-resize")
                    .debug_selector(|| "sidebar-resize".into())
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .right_0()
                    .w(px(RESIZE_GRAB))
                    .cursor(CursorStyle::ResizeLeftRight)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            this.dragging = true;
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
        });

        // The workflow editor takes the whole window: no column, and its own
        // titlebar row in place of the main area's.
        let (column, handle, titlebar) = if self.editing_workflow(cx) {
            (None, None, MainTitlebar { open: 0.0, left: 0.0 })
        } else {
            (column, handle, titlebar)
        };

        let shell = div()
            .id("shelbi-window")
            .key_context("ShelbiWindow")
            .track_focus(&self.focus_handle)
            .capture_key_down(cx.listener(Self::on_capture_key_down))
            .on_key_down(cx.listener(Self::on_key_down))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(None, window, cx)))
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| this.close_window(window, cx)))
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
            .type_style(Type::Base)
            // Under the column and the main area, which paint the window's
            // two surfaces: translucent, it paints nothing.
            .bg(theme.nested(theme.canvas));
        let shell = if self.chooser {
            let chooser = self.render_chooser(&theme, cx);
            shell.child(chooser)
        } else {
            let main = self.render_main(&theme, titlebar, cx);
            shell.children(column).children(handle).child(main)
        };

        // Settings ▸ Workspaces' machine dialog, over the whole window (under
        // an overlay opened on top of it).
        let machine_form = match &self.route {
            Route::Settings(view) => view
                .read(cx)
                .machine_form()
                .map(|form| crate::views::machine_form::render_layer(form, px(main_left), &theme)),
            _ => None,
        };

        // The workspace dialog, over the whole window, its panel centered over
        // the main area.
        let pool_dialog = self.pool_dialog.as_ref().map(|(dialog, _)| {
            let inset = placement.map_or(0.0, |p| p.visible);
            dialog.update(cx, |d, _| d.set_inset(inset));
            dialog.clone()
        });
        // Contact Us, over the whole window.
        let feedback = self.feedback.as_ref().map(|(dialog, _, _)| dialog.clone());
        // Connect to Remote Session, over the whole window.
        let connect = self.connect.as_ref().map(|(dialog, _, _)| dialog.clone());
        let relaunch = self.ready().and_then(|r| r.control.relaunch_required());
        let weak = cx.entity().downgrade();
        let layer = self.overlay.as_ref().map(|o| {
            let entering = if reduce_motion {
                None
            } else {
                self.overlay_opened.and_then(|at| {
                    let t = now.saturating_duration_since(at).as_secs_f32()
                        / theme::motion::DIALOG_IN.as_secs_f32();
                    (t < 1.0).then(|| theme::motion::ease_out_expo(t))
                })
            };
            if entering.is_some() {
                window.request_animation_frame();
            }
            overlay::render_layer(o, window.viewport_size(), entering, &self.keymaps, &theme, cx)
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
            .children(machine_form)
            .children(pool_dialog)
            .children(feedback)
            .children(connect)
            .children(layer)
            .children(probe)
            .into_any_element()
    }
}

/// A bootstrap result that points nowhere: the shell renders, and nothing
/// can reach a real CLI or daemon (the tap stops commands before they would).
#[cfg(any(test, feature = "visual-check"))]
pub(crate) fn inert_ready() -> Ready {
    use crate::platform::control::ControlHandle;
    use crate::platform::discovery::{Candidate, CliSource};
    Ready {
        cli: Candidate {
            path: "/nonexistent/shelbi".into(),
            source: CliSource::Setting,
        },
        cli_version: None,
        env: Arc::new(Default::default()),
        on_login_path: true,
        control: ControlHandle::new(
            "/nonexistent/control.sock".into(),
            APP_VERSION.into(),
            APP_VERSION.into(),
        ),
    }
}

/// The board, the Activity feed and the Machines view, each once shown.
#[cfg(feature = "visual-check")]
pub(crate) type NativeViews = (
    Option<Entity<IssuesView>>,
    Option<Entity<ActivityView>>,
    Option<Entity<MachinesView>>,
);

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

    /// Stand in for the palette's Projects column read (an offscreen window
    /// doesn't read its inputs).
    pub(crate) fn offscreen_palette_projects(&mut self, projects: Vec<PaletteProject>) {
        self.palette_inputs.projects = projects;
    }

    /// Stand in for the board and Changelog reads the open palette searches.
    pub(crate) fn offscreen_palette_corpus(&mut self, corpus: SearchCorpus) {
        if let Some(ActiveOverlay::Palette { palette, .. }) = &mut self.overlay {
            palette.set_corpus(corpus);
        }
    }

    pub(crate) fn offscreen_close_overlay(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.close_overlay(window, cx);
    }

    /// Open the project switcher over the catalog as installed (no refresh
    /// from disk), or close it.
    pub(crate) fn offscreen_switcher(&mut self, open: bool, cx: &mut Context<Self>) {
        let now = (self.clock)();
        if open {
            let current = self.switcher_projects(cx).iter().position(|p| p.name == self.project);
            self.switcher.open(current.unwrap_or(0), now);
        } else {
            self.switcher.close(now);
        }
        cx.notify();
    }

    /// A key to the open switcher.
    pub(crate) fn offscreen_switcher_key(&mut self, code: Key, window: &mut Window, cx: &mut Context<Self>) {
        self.switcher_key(code, window, cx);
        cx.notify();
    }

    /// Move the sidebar's selection to the `ordinal`-th selectable row.
    pub(crate) fn offscreen_select(&mut self, ordinal: usize, cx: &mut Context<Self>) {
        if let Some(row) = self.target_at(ordinal) {
            self.select_row(row, cx);
        }
        cx.notify();
    }

    /// Collapse or expand a sidebar section, as a click on its header does.
    pub(crate) fn offscreen_toggle_section(&mut self, section: sidebar::Section, cx: &mut Context<Self>) {
        self.toggle_section(section, cx);
    }

    /// Hide or show the sidebar, as its Toggle sidebar button does.
    pub(crate) fn offscreen_toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.toggle_sidebar(cx);
    }

    /// Ask for review `task` before anything resolves it: its placeholder.
    pub(crate) fn offscreen_open_pending(&mut self, task: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_review_pending(task, window, cx);
    }

    pub(crate) fn offscreen_review(&self) -> Option<Entity<ReviewView>> {
        self.review.clone()
    }

    /// What the leading column draws now.
    pub(crate) fn offscreen_column(&mut self) -> column_swap::Frame {
        self.column.frame((self.clock)())
    }

    /// Where the sidebar's hide or show stands now.
    pub(crate) fn offscreen_reveal(&mut self) -> sidebar_reveal::RevealFrame {
        self.reveal.frame((self.clock)())
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

    /// `--bench-board`'s window: [`Self::offscreen`]'s, with workspaces
    /// over `workspace`.
    pub(crate) fn bench_workspace_deps(&mut self, workspace: WorkspaceDeps) {
        self.workspace_deps = Some(workspace);
    }

    /// `--visual-figma`'s window: [`Self::offscreen`]'s, with workspaces
    /// over `workspace` and the orchestrator chat over `chat`.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn figma(
        project: &str,
        model: SidebarModel,
        review: ReviewDeps,
        workspace: WorkspaceDeps,
        chat: Rc<dyn Fn() -> ChatDeps>,
        clock: Rc<dyn Fn() -> Instant>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut w = Self::offscreen(project, model, review, clock, window, cx);
        w.workspace_deps = Some(workspace);
        w.chat_deps = Some(chat);
        w
    }

    /// Move the sidebar's selection to the row that opens `target`, as a
    /// click on it would (a no-op when no row does).
    pub(crate) fn offscreen_select_target(&mut self, target: &SidebarTarget, cx: &mut Context<Self>) {
        let entries = self.sidebar_entries();
        if let Some(ordinal) = entries.iter().filter_map(|e| e.row.target()).position(|t| t == *target) {
            self.offscreen_select(ordinal, cx);
        }
    }

    /// Whether the task detail is up over the main area (the panels' More).
    pub(crate) fn offscreen_detail_over(&self, cx: &App) -> bool {
        self.detail_over.is_some() && self.issues.as_ref().is_some_and(|v| v.read(cx).is_floating())
    }

    pub(crate) fn offscreen_open_settings(&mut self, page: SettingsPage, window: &mut Window, cx: &mut Context<Self>) {
        self.open_settings(Some(page), window, cx);
    }

    /// Open workflow `name` in the editor (Settings ▸ Workflows open).
    pub(crate) fn offscreen_edit_workflow(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_workflow(name.to_string(), window, cx);
    }

    #[cfg(feature = "visual-check")]
    pub(crate) fn offscreen_edit_agent(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.edit_agent(name.to_string(), window, cx);
    }

    /// The settings view, while it is shown.
    pub(crate) fn offscreen_settings(&self) -> Option<Entity<SettingsView>> {
        match &self.route {
            Route::Settings(view) => Some(view.clone()),
            _ => None,
        }
    }

    /// Show task `id`'s detail on the board.
    pub(crate) fn offscreen_open_task(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.open_task(id, window, cx);
    }

    /// The orchestrator chat's terminal, once attached.
    pub(crate) fn offscreen_chat_pane(&self, cx: &App) -> Option<Entity<crate::elements::terminal::TerminalPane>> {
        self.chat.as_ref()?.read(cx).pane().cloned()
    }

    /// The Changelog page, once shown.
    pub(crate) fn offscreen_changelog(&self) -> Option<Entity<ChangelogView>> {
        self.changelog.clone()
    }

    /// The native views shown so far.
    pub(crate) fn offscreen_views(&self) -> NativeViews {
        (self.issues.clone(), self.activity.clone(), self.machines.clone())
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
#[path = "app_design_tests.rs"]
mod design_tests;

#[cfg(test)]
#[path = "app_window_tests.rs"]
mod window_tests;

#[cfg(test)]
#[path = "app_sidebar_tests.rs"]
mod sidebar_tests;

#[cfg(test)]
#[path = "app_settings_tests.rs"]
mod settings_tests;

#[cfg(test)]
#[path = "app_feedback_tests.rs"]
mod feedback_tests;

#[cfg(test)]
#[path = "app_connect_tests.rs"]
mod connect_tests;

#[cfg(test)]
#[path = "app_fold_tests.rs"]
mod fold_tests;

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
            CommandKind::SetIssueWorkflow {
                id: "t1".into(),
                workflow: "subtask".into(),
            },
        ] {
            assert_eq!(calls(kind.clone()), [HostCall::IssueCommand(kind)]);
        }
    }
}
