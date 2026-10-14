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
mod keys;
mod platform;
mod projects;
mod refresh;
mod theme;
mod views;
mod window_state;

use gpui::{actions, App, KeyBinding, Menu, MenuItem, SystemMenuType};

use crate::app::Shell;
use crate::projects::Catalog;
use crate::refresh::ControlWatcher;

/// The app name: the menu bar's app menu.
const APP_NAME: &str = "Shelbi";

// App-level actions. `Quit` closes this client only (the registry's
// `QuitShelbi`, which stops every agent, is a separate command).
actions!(shelbi_desktop, [Quit]);

fn main() {
    init_logging();
    // The launch project: a local read before any window exists.
    let launch = projects::launch_project();

    gpui_platform::application()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            // gpui-kit's component layer (theme, inputs, lists, dialogs) for the
            // subtasks that adopt its widgets; our own tokens live in `Theme`,
            // resolved from `config.yaml::theme` (gpui-kit's light/dark mode
            // follows it).
            gpui_kit::init(cx);
            theme::init(cx);
            assets::load_fonts(cx);

            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            app::bind_keys(cx);
            cx.set_menus([Menu::new(APP_NAME).items([
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action(format!("Quit {APP_NAME}"), Quit),
            ])]);

            // The project list and each project's config skeleton, read in
            // the background so a switch paints the target's chrome at once.
            let catalog = Catalog::default();
            catalog.refresh(|| {});
            cx.set_global(catalog);

            // Change notes from the daemon's control socket, routed to each
            // project's feed. The app's attached-client registration (and its
            // detach / relaunch pushes) is bootstrap's control watch, under
            // this id.
            cx.set_global(Shell::new(
                shelbi_client::fresh_client_id(),
                Some(ControlWatcher::spawn()),
            ));

            // Closing the last window quits the app (its agents keep running).
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            match launch {
                Some(project) => app::open_or_focus(&project, cx),
                None => {
                    tracing::error!("no Shelbi project found: run `shelbi init` in a repository");
                    cx.quit();
                    return;
                }
            }
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
