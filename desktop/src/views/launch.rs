//! The launch window (`launch-desktop`, Figma "Startup · Projects and
//! remote sessions" 2539:10949 and "Startup · No projects yet" 2544:10318):
//! what the app opens when it starts with no daemon running
//! ([`shelbi_app::launch::route`]), and what File ▸ Show Launch Screen
//! brings up.
//!
//! A 900 × 620 window, traffic lights over its left panel: "Welcome to
//! Shelbi", the line under it, Add project and Connect to remote. The right
//! panel lists [`LaunchModel`]: Projects on this Mac (folder, name, path),
//! most recently opened first, then Recent remote sessions (name,
//! `user@host:path`, Connect), each section left out when it has no rows; or,
//! with nothing to list, "No projects yet", where Add project is the Primary
//! button.
//!
//! - A project opens through [`launch::open_project`]: off the UI thread, the
//!   app's bootstrap starts the daemon ([`LaunchDaemon`]), the project is
//!   stamped as opened, and its window opens; the launch window closes as
//!   soon as any project window opens ([`project_window_opened`]). The row
//!   shows a spinner meanwhile, and a failure shows under the lists.
//! - Add project opens the Add Project wizard; the project it creates opens
//!   in a window of its own (and this one closes).
//! - Connect to remote opens the Connect to Remote Session dialog over this
//!   window ([`ConnectDialog`]); a recent session's Connect reconnects to it
//!   ([`launch::connect_remote`]), moving it to the top.
//!
//! Keys: ↑ / ↓ move through the rows (the pointer over a row selects it too;
//! the first starts selected, as the frame draws it), Return opens the
//! selected row (Add project when there are none), ⌘W closes the window
//! (the last one quits the app, as any window's).

use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, prelude::*, px, AnyElement, App, ClickEvent, Context, Div, Entity, FocusHandle, Focusable, Global,
    KeyDownEvent, Role, SharedString, Stateful, Subscription, Window, WindowHandle,
};
use shelbi_app::add_project_wizard::Origin;
use shelbi_app::launch::{
    self, LaunchAction, LaunchModel, LaunchState, RecentRemotes, RemoteSession, ADD_PROJECT_LABEL,
    CONNECT_REMOTE_LABEL, EMPTY_BODY, EMPTY_TITLE, PROJECTS_HEADING, RECONNECT_LABEL, REMOTES_HEADING, WELCOME_INTRO,
    WELCOME_TITLE,
};
use shelbi_state::keymap::{Key as Code, KeyChord, Mods};

use super::connect_remote::{ConnectDialog, ConnectEvent};
use crate::app::CloseWindow;
use crate::assets::{heroicon, icons};
use crate::elements::spinner;
use crate::keys::chord_from_keystroke;
use crate::platform::launch::{services, Booted, BootstrapDaemon, LaunchServices};
use crate::theme::style::{self, ButtonContent, ButtonKind, ButtonSize, Type, Typography};
use crate::theme::{self, Theme};

/// The window's size (the frames').
pub const SIZE: (f32, f32) = (900.0, 620.0);
/// The window's title (the title bar hides it; the Window menu shows it).
pub const TITLE: &str = "Shelbi";
/// The left panel's width and the column its words and buttons sit in.
const LEFT_W: f32 = 320.0;
const LEFT_INNER_W: f32 = 256.0;
/// Both panels' inset; the right one starts lower, under the title bar.
const INSET: f32 = 32.0;
const RIGHT_TOP: f32 = 56.0;
/// A card's corners.
const CARD_RADIUS: f32 = 12.0;
/// The empty state's title (Desktop/Heading in this frame: 18 / 28
/// semibold) and its line's width.
const EMPTY_TITLE_SIZE: f32 = 18.0;
const EMPTY_BODY_W: f32 = 360.0;

/// The daemon start behind opening a project, and the bootstrap run it
/// leaves for the windows to share.
pub trait LaunchDaemon: launch::DaemonStarter {
    fn take(&self) -> Option<Booted> {
        None
    }
}

impl LaunchDaemon for BootstrapDaemon {
    fn take(&self) -> Option<Booted> {
        BootstrapDaemon::take(self)
    }
}

impl LaunchDaemon for launch::SharedDaemon {}

#[cfg(test)]
impl LaunchDaemon for launch::FakeDaemon {}

/// Reads the launch screen's rows (blocking: run off the UI thread).
pub type LoadFn = dyn Fn(&RecentRemotes) -> LaunchModel + Send + Sync;
/// The daemon start for one open.
pub type DaemonFn = dyn Fn(&mut App) -> Arc<dyn LaunchDaemon>;
/// Opens the project's window.
pub type OpenFn = dyn Fn(&str, &mut App);
/// Runs Add project.
pub type AddFn = dyn Fn(&mut App);

/// What the view runs; a test swaps in its own (a canned model, a fake
/// daemon, recorders for where it went).
#[derive(Clone)]
pub struct Deps {
    pub load: Arc<LoadFn>,
    pub daemon: Rc<DaemonFn>,
    pub open: Rc<OpenFn>,
    pub add: Rc<AddFn>,
}

impl Deps {
    /// The real machine.
    pub fn real() -> Self {
        Deps {
            load: Arc::new(|recents| {
                LaunchModel::load(recents).unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "couldn't list the projects");
                    LaunchModel {
                        projects: Vec::new(),
                        remotes: recents.load(),
                    }
                })
            }),
            daemon: Rc::new(|cx| match crate::app::running_cli(cx) {
                // A window has bootstrapped: the CLI's launcher is installed.
                Some(_) => Arc::new(launch::SharedDaemon),
                None => Arc::new(BootstrapDaemon::new(crate::app::client_id(cx))),
            }),
            open: Rc::new(crate::app::open_project_window),
            add: Rc::new(|cx| super::add_project::show(Origin::Workspace, None, cx)),
        }
    }
}

/// The open launch window, so asking again brings it up.
#[derive(Default)]
struct OpenLaunch(Option<WindowHandle<LaunchView>>);

impl Global for OpenLaunch {}

/// A test's [`Deps`] for every launch window it opens.
#[cfg(test)]
pub struct TestDeps(pub Deps);

#[cfg(test)]
impl Global for TestDeps {}

fn deps(cx: &App) -> Deps {
    #[cfg(test)]
    if let Some(d) = cx.try_global::<TestDeps>() {
        return d.0.clone();
    }
    let _ = cx;
    Deps::real()
}

/// The open launch window, if any.
pub fn open_window(cx: &App) -> Option<WindowHandle<LaunchView>> {
    cx.try_global::<OpenLaunch>().and_then(|o| o.0)
}

/// Open the launch window, or bring the open one up.
pub fn show(cx: &mut App) -> Option<WindowHandle<LaunchView>> {
    if let Some(open) = open_window(cx) {
        if open.update(cx, |_, window, _| window.activate_window()).is_ok() {
            return Some(open);
        }
    }
    let deps = deps(cx);
    let opened = cx.open_window(super::about::options(TITLE, SIZE, false, cx), move |window, cx| {
        cx.new(|cx| LaunchView::new(deps, window, cx))
    });
    match opened {
        Ok(handle) => {
            cx.set_global(OpenLaunch(Some(handle)));
            cx.activate(true);
            Some(handle)
        }
        Err(e) => {
            tracing::error!(error = %e, "could not open the launch window");
            None
        }
    }
}

/// Connect to Remote Session… with no project window to open it over (the
/// launch window frontmost, or no window at all): the launch window comes
/// up with the dialog over it.
pub fn connect_remote(cx: &mut App) {
    if let Some(handle) = show(cx) {
        let _ = handle.update(cx, |this, window, cx| this.open_connect(window, cx));
    }
}

/// A project window opened: the launch window has done its job and closes.
pub fn project_window_opened(cx: &mut App) {
    let Some(handle) = open_window(cx) else {
        return;
    };
    cx.set_global(OpenLaunch(None));
    // After this update: the project window is in the app's list by then,
    // so closing this one never reads as the last window closing.
    cx.defer(move |cx| {
        let _ = handle.update(cx, |_, window, _| window.remove_window());
    });
}

/// The registry's input while the launch window is frontmost: no project,
/// so only what needs none, the Connect dialog and this screen among them.
pub fn command_model() -> shelbi_app::command::CommandModel {
    shelbi_app::command::CommandModel {
        launch_screen: true,
        remote_connect: true,
        ..Default::default()
    }
}

/// A row on the launch screen or in the Connect dialog (Figma's Horizontal
/// Card, 2565:13541): the folder, the title over the line under it, and
/// `trailing` (a session's Connect, a spinner) at the right. `selected`
/// paints the hovered card's fill.
pub fn card(
    id: impl Into<SharedString>,
    title: String,
    subtitle: String,
    selected: bool,
    trailing: Option<AnyElement>,
    theme: &Theme,
) -> Stateful<Div> {
    let id: SharedString = id.into();
    let selector = id.clone();
    div()
        .id(id)
        .debug_selector(move || selector.to_string())
        .role(Role::ListItem)
        .aria_label(title.clone())
        .aria_selected(selected)
        .flex()
        .items_center()
        .gap(px(12.0))
        .w_full()
        .p(px(12.0))
        .rounded(px(CARD_RADIUS))
        .when(selected, |d| d.bg(theme.background))
        .child(heroicon(icons::FOLDER_LARGE).text_color(theme.icon))
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(
                    div()
                        .type_style(Type::BaseSemibold)
                        .text_color(theme.text_loud)
                        .truncate()
                        .child(title),
                )
                .child(
                    div()
                        .type_style(Type::Small)
                        .text_color(theme.muted)
                        .truncate()
                        .child(subtitle),
                ),
        )
        .children(trailing)
}

/// The launch window.
pub struct LaunchView {
    model: LaunchModel,
    /// The keyboard's row ([`LaunchModel::action_at`]).
    selected: usize,
    /// The row being opened or reconnected.
    busy: Option<usize>,
    error: Option<String>,
    dialog: Option<(Entity<ConnectDialog>, Subscription)>,
    /// Bumped per load: a load's late answer never replaces a newer one.
    load: u64,
    focus: FocusHandle,
    services: LaunchServices,
    deps: Deps,
    _subscriptions: Vec<Subscription>,
}

impl LaunchView {
    pub fn new(deps: Deps, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        focus.focus(window, cx);
        theme::window::apply_to(window, cx);
        let subscriptions = vec![
            theme::follow_system(window, cx),
            // Frontmost, the menu bar is this window's: no project, the
            // Connect dialog's item live.
            cx.observe_window_activation(window, |_, window, cx| {
                if window.is_window_active() {
                    install_menus(cx);
                }
            }),
        ];
        install_menus(cx);
        let mut this = LaunchView {
            model: LaunchModel::default(),
            selected: 0,
            busy: None,
            error: None,
            dialog: None,
            load: 0,
            focus,
            services: services(cx),
            deps,
            _subscriptions: subscriptions,
        };
        this.reload(cx);
        this
    }

    #[cfg(test)]
    pub fn model(&self) -> &LaunchModel {
        &self.model
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The open Connect dialog.
    pub fn dialog(&self) -> Option<Entity<ConnectDialog>> {
        self.dialog.as_ref().map(|(d, _)| d.clone())
    }

    /// Read the rows off the UI thread.
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.load += 1;
        let load = self.load;
        let (read, recents) = (self.deps.load.clone(), self.services.recents.clone());
        let work = cx.background_executor().spawn(async move { read(&recents) });
        cx.spawn(async move |this, cx| {
            let model = work.await;
            let _ = this.update(cx, |this, cx| {
                if load == this.load {
                    this.selected = this.selected.min(model.row_count().saturating_sub(1));
                    this.model = model;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Open the Connect to Remote Session dialog over the window (once).
    pub fn open_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let dialog = cx.new(|cx| ConnectDialog::new(window, cx));
        let sub = cx.subscribe_in(&dialog, window, |this, _, event: &ConnectEvent, window, cx| match event {
            ConnectEvent::Closed(session) => {
                this.dialog = None;
                this.focus.focus(window, cx);
                if let Some(session) = session {
                    this.connected(session.clone(), cx);
                }
                cx.notify();
            }
        });
        self.dialog = Some((dialog, sub));
        cx.notify();
    }

    /// Pick a row, a button or a key: what it does.
    pub fn perform(&mut self, action: LaunchAction, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match action {
            LaunchAction::OpenProject(slug) => self.open_project(slug, cx),
            LaunchAction::AddProject => {
                let add = self.deps.add.clone();
                // After this update, so the wizard's first frame isn't nested
                // in ours.
                cx.defer(move |cx| add(cx));
            }
            LaunchAction::ConnectToRemote => self.open_connect(window, cx),
            LaunchAction::Reconnect(session) => self.reconnect(session, cx),
        }
    }

    /// The row a slug or session is on.
    fn row_of(&self, action: &LaunchAction) -> Option<usize> {
        (0..self.model.row_count()).find(|i| self.model.action_at(*i).as_ref() == Some(action))
    }

    /// Start the daemon and open `slug` (off the UI thread), then its window.
    fn open_project(&mut self, slug: String, cx: &mut Context<Self>) {
        self.busy = self.row_of(&LaunchAction::OpenProject(slug.clone()));
        self.error = None;
        let daemon = (self.deps.daemon)(cx);
        let starter = daemon.clone();
        let work = cx
            .background_executor()
            .spawn(async move { launch::open_project(slug, starter.as_ref()) });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(launch::LaunchOutcome::OpenProject(slug)) => {
                        if let Some((ready, events)) = daemon.take() {
                            crate::app::share_ready(ready, cx);
                            listen(events, cx);
                        }
                        (this.deps.open.clone())(&slug, cx);
                    }
                    Ok(_) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "couldn't open the project");
                        this.error = Some(e.to_string());
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Reconnect to a recent remote session (off the UI thread).
    fn reconnect(&mut self, session: RemoteSession, cx: &mut Context<Self>) {
        self.busy = self.row_of(&LaunchAction::Reconnect(session.clone()));
        self.error = None;
        let LaunchServices { remote, recents } = self.services.clone();
        let work = cx
            .background_executor()
            .spawn(async move { launch::connect_remote(session, remote.as_ref(), &recents) });
        cx.spawn(async move |this, cx| {
            let outcome = work.await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match outcome {
                    Ok(session) => this.connected(session, cx),
                    Err(e) => this.error = Some(e.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    /// Connected to `session` (remembered): its window opens
    /// ([`crate::views::remote_session`]), and the list re-reads, so it is
    /// on top.
    fn connected(&mut self, session: RemoteSession, cx: &mut Context<Self>) {
        tracing::info!(host = %session.host, project = %session.project, "connected to a remote session");
        crate::views::remote_session::open_remote_session(session, cx);
        self.selected = self.model.projects.len();
        self.reload(cx);
    }

    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        if let Some(chord) = chord_from_keystroke(&event.keystroke) {
            if self.key(chord, window, cx) {
                cx.stop_propagation();
            }
        }
    }

    /// A key the window takes.
    pub(crate) fn key(&mut self, chord: KeyChord, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if chord.mods != Mods::NONE {
            return false;
        }
        let rows = self.model.row_count();
        match chord.code {
            Code::Up if rows > 0 => self.selected = self.selected.saturating_sub(1),
            Code::Down if rows > 0 => self.selected = (self.selected + 1).min(rows - 1),
            Code::Enter => {
                let action = self.model.action_at(self.selected).unwrap_or(LaunchAction::AddProject);
                self.perform(action, window, cx);
            }
            _ => return false,
        }
        cx.notify();
        true
    }

    fn select(&mut self, row: usize, cx: &mut Context<Self>) {
        if self.selected != row {
            self.selected = row;
            cx.notify();
        }
    }

    fn render_welcome(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let empty = self.model.state() == LaunchState::NoProjects;
        let add_kind = if empty { ButtonKind::Primary } else { ButtonKind::Secondary };
        let add = style::content_button(
            "launch-add-project",
            ButtonContent::label(ADD_PROJECT_LABEL).left(icons::PLUS),
            add_kind,
            ButtonSize::Base,
            false,
            theme,
        )
        .w_full()
        .role(Role::Button)
        .aria_label(ADD_PROJECT_LABEL)
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.perform(LaunchAction::AddProject, window, cx)));
        let connect = style::content_button(
            "launch-connect-remote",
            ButtonContent::label(CONNECT_REMOTE_LABEL).left(icons::SERVER_STACK),
            ButtonKind::Secondary,
            ButtonSize::Base,
            false,
            theme,
        )
        .w_full()
        .role(Role::Button)
        .aria_label(CONNECT_REMOTE_LABEL)
        .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
            this.perform(LaunchAction::ConnectToRemote, window, cx)
        }));
        div()
            .debug_selector(|| "launch-welcome".into())
            .flex()
            .flex_col()
            .flex_none()
            .justify_center()
            .gap(px(24.0))
            .w(px(LEFT_W))
            .h_full()
            .p(px(INSET))
            .bg(theme.sidebar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .w(px(LEFT_INNER_W))
                    .child(
                        div()
                            .debug_selector(|| "launch-title".into())
                            .type_style(Type::Title)
                            .text_color(theme.text_loud)
                            .child(WELCOME_TITLE),
                    )
                    .child(
                        div()
                            .type_style(Type::Base)
                            .text_color(theme.muted)
                            .child(WELCOME_INTRO),
                    ),
            )
            .child(div().flex().flex_col().gap(px(12.0)).w(px(LEFT_INNER_W)).child(add).child(connect))
    }

    fn render_lists(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let section = |id: &'static str, heading: &'static str, rows: Vec<Stateful<Div>>| {
            div()
                .debug_selector(move || id.into())
                .flex()
                .flex_col()
                .gap(px(8.0))
                .w_full()
                .child(
                    div()
                        .type_style(Type::BaseSemibold)
                        .text_color(theme.text_loud)
                        .child(heading),
                )
                .child(
                    div()
                        .id(id)
                        .role(Role::List)
                        .aria_label(heading)
                        .flex()
                        .flex_col()
                        .gap(px(4.0))
                        .w_full()
                        .children(rows),
                )
        };
        let spinner_at = |row: usize| {
            (self.busy == Some(row)).then(|| {
                spinner::spinner_sized(format!("launch-row-{row}-spinner"), theme.icon, 16.0).into_any_element()
            })
        };
        let projects: Vec<Stateful<Div>> = self
            .model
            .projects
            .iter()
            .enumerate()
            .map(|(row, p)| {
                let slug = p.slug.clone();
                card(
                    format!("launch-project-{row}"),
                    p.label.clone(),
                    p.display_path.clone(),
                    row == self.selected,
                    spinner_at(row),
                    theme,
                )
                .cursor_pointer()
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.select(row, cx);
                    }
                }))
                .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.perform(LaunchAction::OpenProject(slug.clone()), window, cx)
                }))
            })
            .collect();
        let offset = self.model.projects.len();
        let remotes: Vec<Stateful<Div>> = self
            .model
            .remotes
            .iter()
            .enumerate()
            .map(|(i, r)| {
                let row = offset + i;
                let session = r.session.clone();
                let trailing = spinner_at(row).unwrap_or_else(|| {
                    style::button(format!("launch-remote-{i}-connect"), RECONNECT_LABEL, ButtonKind::Secondary, ButtonSize::Small, false, theme)
                        .role(Role::Button)
                        .aria_label(format!("{RECONNECT_LABEL} {}", session.display_label()))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.perform(LaunchAction::Reconnect(session.clone()), window, cx)
                        }))
                        .into_any_element()
                });
                card(
                    format!("launch-remote-{i}"),
                    r.session.display_label().to_string(),
                    r.session.address(),
                    row == self.selected,
                    Some(trailing),
                    theme,
                )
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    if *hovered {
                        this.select(row, cx);
                    }
                }))
            })
            .collect();
        let inner = div()
            .flex()
            .flex_col()
            .justify_center()
            .gap(px(24.0))
            .min_h_full()
            .pt(px(RIGHT_TOP))
            .pb(px(INSET))
            .px(px(INSET))
            .when(!projects.is_empty(), |d| d.child(section("launch-projects", PROJECTS_HEADING, projects)))
            .when(!remotes.is_empty(), |d| d.child(section("launch-remotes", REMOTES_HEADING, remotes)))
            .children(self.error.as_ref().map(|e| {
                div()
                    .debug_selector(|| "launch-error".into())
                    .type_style(Type::Small)
                    .text_color(theme.error)
                    .child(e.clone())
            }));
        div()
            .id("launch-lists")
            .flex_1()
            .min_w_0()
            .h_full()
            .overflow_y_scroll()
            .child(inner)
    }

    fn render_empty(&self, theme: &Theme) -> Div {
        div()
            .debug_selector(|| "launch-empty".into())
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .items_center()
            .justify_center()
            .gap(px(16.0))
            .pt(px(INSET))
            .pb(px(INSET))
            .px(px(INSET))
            .child(heroicon(icons::FOLDER_LARGE_THIN).text_color(theme.icon))
            .child(
                div()
                    .type_style(Type::Heading)
                    .text_size(px(EMPTY_TITLE_SIZE))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .text_color(theme.text_loud)
                    .child(EMPTY_TITLE),
            )
            .child(
                div()
                    .w(px(EMPTY_BODY_W))
                    .type_style(Type::Base)
                    .text_color(theme.muted)
                    .text_center()
                    .child(EMPTY_BODY),
            )
            .children(self.error.as_ref().map(|e| {
                div()
                    .debug_selector(|| "launch-error".into())
                    .type_style(Type::Small)
                    .text_color(theme.error)
                    .child(e.clone())
            }))
    }
}

/// Install the launch window's menu bar.
fn install_menus(cx: &mut App) {
    let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
    let tree = crate::menus::tree::build(&command_model(), &keymaps, Default::default());
    crate::menus::install(tree, cx);
}

/// Keep listening to a bootstrap run the launch window finished: the
/// daemon's relaunch and detach pushes reach the app, as the first launch's
/// setup window's do.
fn listen(events: async_channel::Receiver<crate::platform::BootstrapEvent>, cx: &mut App) {
    use crate::platform::BootstrapEvent;
    cx.spawn(async move |cx| {
        while let Ok(event) = events.recv().await {
            match event {
                BootstrapEvent::State(_) => {}
                BootstrapEvent::RelaunchRequired(notice) => {
                    tracing::info!(reason = %notice.reason, "relaunch required");
                    cx.update(|cx| cx.refresh_windows());
                }
                BootstrapEvent::Detach(reason) => {
                    tracing::info!(%reason, "detached by the daemon");
                    cx.update(|cx| cx.quit());
                }
            }
        }
    })
    .detach();
}

impl Focusable for LaunchView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for LaunchView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let right = match self.model.state() {
            LaunchState::Projects => self.render_lists(&theme, cx).into_any_element(),
            LaunchState::NoProjects => self.render_empty(&theme).into_any_element(),
        };
        div()
            .id("launch")
            .debug_selector(|| "launch".into())
            .track_focus(&self.focus)
            .on_action(cx.listener(|this, _: &CloseWindow, window, cx| match this.dialog() {
                // ⌘W cancels the dialog first.
                Some(dialog) => dialog.update(cx, |d, cx| d.dismiss(cx)),
                None => window.remove_window(),
            }))
            .on_key_down(cx.listener(Self::key_down))
            .relative()
            .flex()
            .size_full()
            .bg(theme.canvas)
            .text_color(theme.foreground)
            .font_family(style::UI_FAMILY)
            .child(self.render_welcome(&theme, cx))
            .child(right)
            .children(self.dialog())
    }
}

#[cfg(test)]
#[path = "launch_tests.rs"]
mod tests;
