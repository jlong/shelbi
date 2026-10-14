//! `shelbi-desktop`: the native macOS client for Shelbi, built with gpui.
//!
//! A sibling of the TUI at feature parity: both render over `shelbi-app`
//! (navigation, the command registry, view models, background refresh) and
//! talk to the same daemon and sessions. gpui types stay in this crate. See
//! README.md for the architecture map and which `dt-*` subtask fills each
//! module.

mod app;
mod assets;
mod commands;
mod elements;
mod platform;
mod theme;
mod views;

use gpui::{
    actions, point, prelude::*, px, size, App, Bounds, KeyBinding, Menu, MenuItem, SystemMenuType,
    TitlebarOptions, WindowBounds, WindowOptions,
};

use crate::app::ShelbiWindow;
use crate::theme::Theme;

/// The app name: the window title and the menu bar's app menu.
const APP_NAME: &str = "Shelbi";

// App-level actions. `Quit` closes this client only (the registry's
// `QuitShelbi`, which stops every agent, is a separate command).
actions!(shelbi_desktop, [Quit]);

fn main() {
    init_logging();

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(|cx: &mut App| {
            // gpui-kit's component layer (theme, inputs, lists, dialogs) for the
            // subtasks that adopt its widgets; our own tokens live in `Theme`.
            gpui_kit::init(cx);
            gpui_kit::component::Theme::change(gpui_kit::component::ThemeMode::Dark, None, cx);
            cx.set_global(Theme::dark());

            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.set_menus([Menu::new(APP_NAME).items([
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {APP_NAME}"), Quit),
            ])]);

            let bounds = Bounds::centered(None, size(px(1200.0), px(800.0)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    titlebar: Some(TitlebarOptions {
                        title: Some(APP_NAME.into()),
                        // The sidebar runs under a transparent title bar, the
                        // usual shape for a macOS sidebar app.
                        appears_transparent: true,
                        traffic_light_position: Some(point(px(12.0), px(12.0))),
                    }),
                    app_id: Some("co.32pixels.shelbi".into()),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| ShelbiWindow::new()),
            )
            .expect("failed to open the Shelbi window");
            cx.activate(true);
        });
}

/// Log to stderr, `warn` and up by default (`RUST_LOG` overrides), so a
/// not-implemented command's warning is visible when run from a terminal.
fn init_logging() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn"));
    fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();
}
