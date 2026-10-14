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
mod spike;
mod theme;
mod views;
#[cfg(feature = "visual-check")]
mod visual_check;
#[cfg(feature = "visual-check")]
mod visual_views;
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

/// What the command line asked for.
enum Mode {
    /// The app, on the launch project (`projects::launch_project` reads the
    /// optional project argument itself).
    App,
    /// `--spike <session>`: one terminal element on one session, bypassing
    /// the project shell.
    Spike(String),
    /// `--print-version`: print the app's version and the CLI check, then
    /// exit (no window, no daemon). See `platform::print_version`.
    PrintVersion,
    /// `--visual-check [dir]`: render the terminal element offscreen and
    /// check its input handling (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    VisualCheck(Option<String>),
    /// `--visual-views [dir]`: render the Activity and Machines views
    /// offscreen against a sandboxed home (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    VisualViews(Option<String>),
}

fn parse_args() -> Mode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => Mode::App,
        Some("--spike") => match args.next() {
            Some(session) => Mode::Spike(session),
            None => usage("--spike needs a session (see `shelbi session ls`)"),
        },
        Some("--print-version") => Mode::PrintVersion,
        #[cfg(feature = "visual-check")]
        Some("--visual-check") => Mode::VisualCheck(args.next()),
        #[cfg(feature = "visual-check")]
        Some("--visual-views") => Mode::VisualViews(args.next()),
        Some(project) if !project.starts_with('-') => Mode::App,
        Some(other) => usage(&format!("unknown argument `{other}`")),
    }
}

fn usage(problem: &str) -> ! {
    eprintln!(
        "shelbi-desktop: {problem}\nusage: shelbi-desktop [<project>] | --spike <session> | --print-version"
    );
    std::process::exit(2);
}

fn main() {
    init_logging();
    let mode = parse_args();
    if let Mode::PrintVersion = mode {
        std::process::exit(platform::print_version::run());
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualCheck(dir) = mode {
        visual_check::run(visual_check::out_dir(dir));
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualViews(dir) = mode {
        visual_views::run(visual_check::out_dir(dir));
    }
    // The launch project: a local read before any window exists (the spike
    // has none).
    let launch = match mode {
        Mode::App => projects::launch_project(),
        _ => None,
    };

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

            // Closing the last window quits the app (its agents keep running).
            cx.on_window_closed(|cx, _| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();

            if let Mode::Spike(session) = mode {
                spike::open(session, cx);
                return;
            }

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

            // Once a day, off-thread: is there a newer release? Every
            // project window shows the notice when there is.
            views::update::spawn_check(cx);

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
