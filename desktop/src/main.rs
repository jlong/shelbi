//! `shelbi-desktop`: the native macOS client for Shelbi, built with gpui.
//!
//! A sibling of the TUI at feature parity: both render over `shelbi-app`
//! (navigation, the command registry, view models, background refresh) and
//! talk to the same daemon and sessions. gpui types stay in this crate. See
//! README.md for the architecture map and which `dt-*` subtask fills each
//! module.

mod app;
mod assets;
#[cfg(feature = "bench")]
mod bench;
#[cfg(feature = "visual-check")]
mod bench_activity;
#[cfg(feature = "visual-check")]
mod bench_board;
#[cfg(feature = "visual-check")]
mod bench_builder;
#[cfg(feature = "visual-check")]
mod bench_push;
mod commands;
mod drops;
mod elements;
mod feeds;
mod keys;
mod licenses;
mod menus;
mod notify;
mod platform;
mod projects;
mod refresh;
mod spike;
#[cfg(test)]
mod test_env;
mod theme;
mod views;
#[cfg(feature = "visual-check")]
mod visual_check;
#[cfg(feature = "visual-check")]
mod visual_figma;
#[cfg(feature = "visual-check")]
mod visual_views;
#[cfg(feature = "visual-check")]
mod visual_review;
#[cfg(feature = "visual-check")]
mod visual_workspace;
mod window_state;

use gpui::App;

use crate::app::Shell;
use crate::feeds::FeedHub;
use crate::projects::Catalog;
use crate::refresh::ControlWatcher;
use crate::window_state::{launch_plan, WindowSession, WindowState};

/// What the command line asked for.
enum Mode {
    /// The app: the windows that were open, each on its project
    /// (`window_state::launch_plan`); an optional project argument takes the
    /// first one.
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
    /// `--visual-workspace [dir]`: render the Workspace view offscreen over
    /// fake sessions, one frame per state (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    VisualWorkspace(Option<String>),
    /// `--visual-review [dir]`: render the Review view offscreen over fake
    /// sessions, one frame per state (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    VisualReview(Option<String>),
    /// `--visual-figma [dir] [--2x]`: render every Figma mockup screen in
    /// the real shell over the mockups' own content, at the reference PNGs'
    /// size (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    VisualFigma(Option<String>, bool),
    /// `--visual-figma-fixtures <root>`: write `--visual-figma`'s mockup
    /// content into a Shelbi root and exit, for the real-window walk
    /// (`scripts/figma-window-walk.sh`; feature `visual-check`).
    #[cfg(feature = "visual-check")]
    VisualFigmaFixtures(String),
    /// `--bench-push [options]`: time the leading column's push in a real
    /// window over fake daemons (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    BenchPush(Vec<String>),
    /// `--bench-board [options]`: time the Issues board's frames on a large
    /// fixture board, offscreen (feature `visual-check`).
    #[cfg(feature = "visual-check")]
    BenchBoard(Vec<String>),
    /// `--bench-builder [options]`: time the workflow editor's Builder
    /// frames on a 20-status, 40-transition workflow, offscreen (feature
    /// `visual-check`).
    #[cfg(feature = "visual-check")]
    BenchBuilder(Vec<String>),
    /// `--bench-activity [options]`: time the Activity feed's first paint
    /// and frames on a large fixture events log, offscreen (feature
    /// `visual-check`).
    #[cfg(feature = "visual-check")]
    BenchActivity(Vec<String>),
    /// `--bench-term [options]`, `--bench-feed ...`, `--bench-record ...`:
    /// the terminal performance bench and its helpers (feature `bench`).
    #[cfg(feature = "bench")]
    Bench(String, Vec<String>),
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
        #[cfg(feature = "visual-check")]
        Some("--visual-workspace") => Mode::VisualWorkspace(args.next()),
        #[cfg(feature = "visual-check")]
        Some("--visual-review") => Mode::VisualReview(args.next()),
        #[cfg(feature = "visual-check")]
        Some("--visual-figma") => {
            let rest: Vec<String> = args.collect();
            let dir = rest.iter().find(|a| !a.starts_with("--")).cloned();
            Mode::VisualFigma(dir, rest.iter().any(|a| a == "--2x"))
        }
        #[cfg(feature = "visual-check")]
        Some("--visual-figma-fixtures") => match args.next() {
            Some(root) => Mode::VisualFigmaFixtures(root),
            None => usage("--visual-figma-fixtures needs a Shelbi root directory"),
        },
        #[cfg(feature = "visual-check")]
        Some("--bench-push") => Mode::BenchPush(args.collect()),
        #[cfg(feature = "visual-check")]
        Some("--bench-board") => Mode::BenchBoard(args.collect()),
        #[cfg(feature = "visual-check")]
        Some("--bench-builder") => Mode::BenchBuilder(args.collect()),
        #[cfg(feature = "visual-check")]
        Some("--bench-activity") => Mode::BenchActivity(args.collect()),
        #[cfg(feature = "bench")]
        Some(flag @ ("--bench-term" | "--bench-feed" | "--bench-record" | "--bench-client")) => {
            Mode::Bench(flag.to_string(), args.collect())
        }
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
    // Before any window (or offscreen render) draws text.
    platform::font_smoothing::apply();
    #[cfg(feature = "visual-check")]
    if let Mode::VisualCheck(dir) = mode {
        visual_check::run(visual_check::out_dir(dir));
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualViews(dir) = mode {
        visual_views::run(visual_check::out_dir(dir));
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualWorkspace(dir) = mode {
        visual_workspace::run(visual_check::out_dir(dir));
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualReview(dir) = mode {
        visual_review::run(visual_check::out_dir(dir));
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualFigma(dir, native) = mode {
        let (dir, theme) = visual_check::per_theme(dir);
        visual_figma::run(dir, theme, native);
    }
    #[cfg(feature = "visual-check")]
    if let Mode::VisualFigmaFixtures(root) = &mode {
        visual_figma::export_fixtures(root.into());
    }
    #[cfg(feature = "visual-check")]
    if let Mode::BenchPush(args) = &mode {
        bench_push::run(args);
    }
    #[cfg(feature = "visual-check")]
    if let Mode::BenchBoard(args) = &mode {
        bench_board::run(args);
    }
    #[cfg(feature = "visual-check")]
    if let Mode::BenchBuilder(args) = &mode {
        bench_builder::run(args);
    }
    #[cfg(feature = "visual-check")]
    if let Mode::BenchActivity(args) = &mode {
        bench_activity::run(args);
    }
    #[cfg(feature = "bench")]
    if let Mode::Bench(flag, args) = &mode {
        match flag.as_str() {
            "--bench-feed" => bench::feed::run(args),
            "--bench-record" => bench::record::run(args),
            "--bench-client" => bench::client::run(args),
            _ => bench::run(args),
        }
    }
    // The windows to open: local reads before any window exists (the spike
    // has none). The window slots, else (the first launch after the
    // upgrade) one window on the launch project with its old frame.
    let (session, plan) = match mode {
        Mode::App => {
            let (session, plan) = read_launch_plan();
            (session.unwrap_or_default(), plan)
        }
        _ => (WindowSession::default(), Vec::new()),
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
            theme::motion::sync_reduce_motion(cx);
            assets::load_fonts(cx);
            elements::terminal::TerminalSettings::init(cx);
            // System Settings' "Show scroll bars", which the terminals'
            // scrollbar follows (read now and on every change).
            elements::terminal::scrollbar::SystemScrollbars::follow(cx);
            // Image and file-promise drops onto a terminal pane (gpui's window
            // takes only Finder drops on its own).
            drops::install(cx);

            // The menu bar: the macOS-standard shortcuts now (⌘Q closes this
            // client only; the registry's Quit Shelbi, which stops every
            // agent, is its own confirmed item), then the active window's
            // commands once it has state (`ShelbiWindow::sync_menus`).
            menus::init(cx);
            menus::install(
                menus::tree::build(
                    &Default::default(),
                    &shelbi_state::keymap::validate_keymaps_yaml("", None).0,
                    Default::default(),
                ),
                cx,
            );
            // A clicked notification brings up the window showing its project
            // (else switches the frontmost one) and opens the item. (Notifications only post from the bundled app: an
            // unbundled `cargo run` has no notification center.)
            cx.on_system_notification_response(|response, cx| {
                app::open_notice_tag(&response.tag, cx);
            });

            // Closing the last window quits the app (its agents keep running).
            cx.on_window_closed(|cx, id| {
                app::window_closed(id, cx);
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
            let control = ControlWatcher::spawn();
            cx.set_global(FeedHub::live(Some(control.clone())));
            cx.set_global(Shell::new(
                shelbi_client::fresh_client_id(),
                Some(control),
                session,
            ));

            // Once a day, off-thread: is there a newer release? Every
            // project window shows the notice when there is.
            views::update::spawn_check(cx);

            // No project yet: the first launch checks for the CLI (installing
            // it when there is none), then becomes the Add Project wizard
            // (closing it, the last window, quits).
            if plan.is_empty() {
                views::cli_setup::show(cx);
                return;
            }
            app::open_launch_windows(plan, cx);
            cx.activate(true);
        });
}

/// The windows a launch opens now, and the saved window slots they came
/// from: local reads (the slots, the project list), no daemon.
pub(crate) fn read_launch_plan() -> (Option<WindowSession>, Vec<window_state::LaunchWindow>) {
    let session = WindowSession::load();
    let named = projects::named_project();
    let known = projects::project_names();
    let plan = launch_plan(
        session.as_ref(),
        named.as_deref(),
        projects::launch_project().as_deref(),
        &|p| known.contains(p),
        &WindowState::load_legacy,
    );
    (session, plan)
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
