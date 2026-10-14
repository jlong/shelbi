//! The root view: the bootstrap screens until the app reaches Shelbi, then
//! the sidebar | main area split.
//!
//! Bootstrap (`crate::platform`) runs off the UI thread; this view renders
//! its progress, a full-window screen for a failure, and once `Ready`, the
//! shell, with the relaunch bar on top if the CLI is upgraded underneath.
//!
//! Both shell halves are placeholders in the foundation. The sidebar lists one nav
//! row per [`View`] so each `ShowView` reaches the command dispatch (and its
//! not-implemented banner) through a real click. `dt-shell-sidebar` replaces
//! the sidebar with the real one (projects, nav, workspaces, Zen) and the
//! per-view subtasks fill the main area.

use gpui::{
    div, prelude::*, px, svg, AnyElement, Context, FontWeight, SharedString, Subscription, Task,
    Window,
};
use shelbi_app::{CommandKind, View};

use crate::assets::icons;
use crate::commands::{self, CommandHost};
use crate::platform::version::APP_VERSION;
use crate::platform::{
    relaunch, Bootstrap, BootstrapEvent, BootstrapState, Failure, Ready, RelaunchNotice,
    SystemPlatform,
};
use crate::theme::{self, Theme};
use crate::views::bootstrap::{self as boot_view, ScreenAction};

/// Sidebar width in the foundation. `dt-shell-sidebar` makes it resizable.
const SIDEBAR_WIDTH: f32 = 240.0;

/// Nav and banner icon size.
const ICON_SIZE: f32 = 16.0;

/// The nav rows: the TUI sidebar's views, in its order, each with its
/// Heroicon.
fn nav_rows() -> [(&'static str, &'static str, View); 4] {
    [
        (
            "Orchestrator",
            icons::CHAT_BUBBLE_LEFT_RIGHT,
            View::Session("orch".into()),
        ),
        ("Issues", icons::VIEW_COLUMNS, View::Issues),
        ("Activity", icons::BOLT, View::Activity),
        ("Machines", icons::SERVER_STACK, View::Machines),
    ]
}

/// Where the window is in reaching Shelbi.
enum Boot {
    /// Bootstrap is running; the state is one of the in-flight ones.
    InFlight(BootstrapState),
    Failed(Failure),
    Ready(Box<Ready>),
}

/// The window's root view.
pub struct ShelbiWindow {
    boot: Boot,
    /// Set when the CLI was upgraded under the running app. Commands that
    /// reach the daemon are refused until relaunch; views keep working.
    relaunch: Option<RelaunchNotice>,
    /// Drains bootstrap's channel into this view; replaced on retry.
    _bootstrap: Option<Task<()>>,
    /// The nav row last clicked, painted with the selection fill.
    selected: Option<View>,
    /// The "Not implemented yet" banner, when a stub command ran.
    banner: Option<SharedString>,
    /// Re-resolves the theme when the system appearance changes.
    _appearance: Subscription,
}

impl ShelbiWindow {
    /// The window, with bootstrap started.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            boot: Boot::InFlight(BootstrapState::ResolvingEnv),
            relaunch: None,
            _bootstrap: None,
            selected: None,
            banner: None,
            _appearance: theme::follow_system(window, cx),
        };
        this.start_bootstrap(cx);
        this
    }

    /// Run bootstrap (again) and feed its events into this view.
    fn start_bootstrap(&mut self, cx: &mut Context<Self>) {
        self.boot = Boot::InFlight(BootstrapState::ResolvingEnv);
        let events = Bootstrap::spawn(SystemPlatform::default(), APP_VERSION.to_string());
        self._bootstrap = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                if this.update(cx, |this, cx| this.apply(event, cx)).is_err() {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn apply(&mut self, event: BootstrapEvent, cx: &mut Context<Self>) {
        match event {
            BootstrapEvent::State(BootstrapState::Ready(ready)) => {
                tracing::info!(
                    cli = %ready.cli.path.display(),
                    daemon = ready.control.daemon_version(),
                    "bootstrap ready"
                );
                self.boot = Boot::Ready(Box::new(ready));
            }
            BootstrapEvent::State(BootstrapState::Failed(failure)) => {
                tracing::warn!(?failure, "bootstrap failed");
                self.boot = Boot::Failed(failure);
            }
            BootstrapEvent::State(state) => self.boot = Boot::InFlight(state),
            BootstrapEvent::RelaunchRequired(notice) => self.relaunch = Some(notice),
        }
        cx.notify();
    }

    /// One line on what bootstrap connected to, for the placeholder main area.
    fn connection_summary(&self) -> Option<String> {
        let Boot::Ready(ready) = &self.boot else {
            return None;
        };
        let cli_version = ready
            .cli_version
            .as_deref()
            .map(|v| format!(" {v}"))
            .unwrap_or_default();
        Some(format!(
            "Daemon {}, shelbi{cli_version} at {} (from {})",
            ready.control.daemon_version(),
            ready.cli.path.display(),
            ready.cli.source.label()
        ))
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

    /// Run a command from the UI and repaint.
    fn run(&mut self, kind: CommandKind, cx: &mut Context<Self>) {
        if self.relaunch.is_some() && commands::sends_to_daemon(&kind) {
            self.banner = Some("Shelbi was updated: relaunch before making changes.".into());
            cx.notify();
            return;
        }
        if let CommandKind::ShowView(view) = &kind {
            self.selected = Some(view.clone());
        }
        commands::dispatch_command(&kind, self);
        cx.notify();
    }

    fn render_sidebar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = nav_rows()
            .into_iter()
            .enumerate()
            .map(|(ix, (label, icon, view))| {
                let selected = self.selected.as_ref() == Some(&view);
                let color = if selected {
                    theme.white
                } else {
                    theme.gray
                };
                div()
                    .id(("nav", ix))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_1()
                    .rounded_md()
                    .cursor_pointer()
                    .when(selected, |row| {
                        row.bg(theme.selection)
                            .text_color(theme.white)
                            .font_weight(FontWeight::BOLD)
                    })
                    .when(!selected, |row| row.text_color(theme.gray))
                    .child(
                        svg()
                            .path(icon)
                            .size(px(ICON_SIZE))
                            .flex_none()
                            .text_color(color),
                    )
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.run(CommandKind::ShowView(view.clone()), cx)
                    }))
            });

        div()
            .flex()
            .flex_col()
            .gap_1()
            .w(px(SIDEBAR_WIDTH))
            .h_full()
            .flex_none()
            .p_3()
            // Clear the macOS traffic lights, which sit over the sidebar.
            .pt(px(36.0))
            .bg(theme.background)
            .child(
                div()
                    .px_3()
                    .pb_2()
                    .text_color(theme.cyan)
                    .font_weight(FontWeight::BOLD)
                    .child("Shelbi"),
            )
            .children(rows)
    }

    fn render_main(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .flex_1()
            .h_full()
            .bg(theme.canvas)
            .when_some(self.banner.clone(), |main, message| {
                main.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .px_4()
                        .py_2()
                        .bg(theme.selection)
                        .text_color(theme.yellow)
                        .child(message)
                        .child(
                            div()
                                .id("dismiss-banner")
                                .cursor_pointer()
                                .child(
                                    svg()
                                        .path(icons::X_MARK)
                                        .size(px(ICON_SIZE))
                                        .text_color(theme.muted),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.banner = None;
                                    cx.notify();
                                })),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .flex_col()
                    .gap_1()
                    .text_color(theme.muted)
                    .child("Shelbi desktop foundation: pick a view in the sidebar.")
                    .when_some(self.connection_summary(), |col, summary| {
                        col.child(div().text_xs().child(summary))
                    }),
            )
    }
}

impl CommandHost for ShelbiWindow {
    fn not_implemented(&mut self, command_id: &str) {
        self.banner = Some(commands::not_implemented_message(command_id).into());
    }
}

impl ShelbiWindow {
    fn render_shell(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let weak = cx.entity().downgrade();
        div()
            .flex()
            .flex_col()
            .size_full()
            .text_sm()
            .when_some(self.relaunch.as_ref(), |col, notice| {
                col.child(boot_view::render_relaunch_bar(
                    notice,
                    theme,
                    move |_, cx| {
                        let _ = weak.update(cx, |this, cx| {
                            this.screen_action(ScreenAction::Relaunch, cx)
                        });
                    },
                ))
            })
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_1()
                    .min_h_0()
                    .child(self.render_sidebar(theme, cx))
                    // The sidebar's right-edge divider (`divider`).
                    .child(div().w(px(1.0)).h_full().flex_none().bg(theme.divider))
                    .child(self.render_main(theme, cx)),
            )
            .into_any_element()
    }
}

impl Render for ShelbiWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        match &self.boot {
            Boot::Ready(_) => self.render_shell(&theme, cx),
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
