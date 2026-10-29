//! The launch screen and Connect to Remote Session, driven as the user
//! drives them (keys, clicks) over a `TestBackend`, at the three sizes the
//! task names (100x30, 160x45, 60x20).
//!
//! With `SHELBI_LAUNCH_CAPTURE=<dir>` set, every rendered state is also
//! written as `<dir>/<state>-<w>x<h>.html`, for checking it by eye.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;
use std::time::{Duration, Instant};

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::{backend::TestBackend, buffer::Buffer, layout::Rect, Terminal};
use shelbi_app::launch::connect::Stage;
use shelbi_app::launch::{self, remote, FakeDaemon, FakeRemote, LocalProject, RecentRemote};

use super::*;
use crate::theme::palette;

const SIZES: [(u16, u16); 3] = [(100, 30), (160, 45), (60, 20)];

/// A throwaway Shelbi home (the launch model, the recent sessions and a
/// project's registration live in it), with the hub sockets inside it so
/// nothing reaches the real hub.
struct Sandbox {
    home: PathBuf,
    saved: Vec<(&'static str, Option<OsString>)>,
    _lock: MutexGuard<'static, ()>,
}

impl Sandbox {
    fn new() -> Sandbox {
        let lock = crate::test_support::ENV_LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let home = tempfile::Builder::new().prefix("sl").tempdir_in("/tmp").unwrap().keep();
        let vars = ["SHELBI_HOME", "SHELBI_HUB_SOCK", "SHELBI_CONTROL_SOCK"];
        let saved = vars.iter().map(|v| (*v, std::env::var_os(v))).collect();
        std::env::set_var("SHELBI_HOME", &home);
        std::env::set_var("SHELBI_HUB_SOCK", home.join("hub.sock"));
        std::env::set_var("SHELBI_CONTROL_SOCK", home.join("control.sock"));
        Sandbox { home, saved, _lock: lock }
    }

    /// Register a global project `name` (the routing's path-only check).
    fn register(&self, name: &str) {
        let repo = self.home.join("repos").join(name);
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(self.home.join("projects")).unwrap();
        let yaml = format!(
            "name: {name}\nrepo: {repo}\nmachines:\n  - {{ name: hub, kind: local, work_dir: {repo} }}\n\
             orchestrator: {{ runner: claude }}\nagent_runners:\n  claude: {{ command: claude, flags: [] }}\n\
             workspaces: []\n",
            repo = repo.display()
        );
        std::fs::write(self.home.join("projects").join(format!("{name}.yaml")), yaml).unwrap();
    }

    fn recents(&self) -> RecentRemotes {
        RecentRemotes::at(self.home.join("recent-remotes.json"))
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        for (var, value) in &self.saved {
            match value {
                Some(v) => std::env::set_var(var, v),
                None => std::env::remove_var(var),
            }
        }
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn project(slug: &str, label: &str) -> LocalProject {
    LocalProject {
        slug: slug.into(),
        label: label.into(),
        path: format!("/Users/jlong/Workspaces/{label}"),
        display_path: format!("~/Workspaces/{label}"),
    }
}

fn session(project: &str, label: &str) -> RemoteSession {
    RemoteSession {
        host: "devbox".into(),
        user: "jlong".into(),
        project: project.into(),
        label: Some(label.into()),
        path: format!("~/Workspaces/{project}"),
    }
}

/// The design's lists: three projects here, two recent sessions.
fn figma_model() -> LaunchModel {
    let at = Utc.with_ymd_and_hms(2026, 10, 28, 9, 0, 0).unwrap();
    LaunchModel {
        projects: vec![
            project("shelbi", "Shelbi"),
            project("contextstore", "ContextStore"),
            project("32pixels", "32pixels"),
        ],
        remotes: vec![
            RecentRemote { session: session("shelbi", "Shelbi"), last_connected: at },
            RecentRemote { session: session("ContextStore", "ContextStore"), last_connected: at },
        ],
    }
}

fn devbox() -> FakeRemote {
    FakeRemote::new().with_host("devbox", "jlong", &[("shelbi", "~/Workspaces/shelbi"), ("ContextStore", "~/Workspaces/ContextStore")])
}

fn app_with(model: LaunchModel, remote: FakeRemote, recents: RecentRemotes) -> (LaunchApp, Arc<FakeDaemon>, Arc<FakeRemote>) {
    let daemon = Arc::new(FakeDaemon::new());
    let remote = Arc::new(remote);
    let connector: Arc<dyn RemoteConnector> = remote.clone();
    let app = LaunchApp::new(model, daemon.clone(), Some(connector), recents, Path::new("/tmp"), false);
    (app, daemon, remote)
}

/// An app whose recent sessions go nowhere a test reads back.
fn app(model: LaunchModel) -> LaunchApp {
    let recents = RecentRemotes::at(std::env::temp_dir().join("shelbi-launch-tests-unused.json"));
    app_with(model, devbox(), recents).0
}

fn draw(app: &mut LaunchApp, w: u16, h: u16) -> Buffer {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    term.backend().buffer().clone()
}

fn rows(buf: &Buffer) -> Vec<String> {
    (0..buf.area.height)
        .map(|y| (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect::<String>())
        .collect()
}

fn text(buf: &Buffer) -> String {
    rows(buf).join("\n")
}

/// Where `needle` first shows: its column and row.
fn find(buf: &Buffer, needle: &str) -> Option<(u16, u16)> {
    rows(buf).iter().enumerate().find_map(|(y, row)| {
        row.find(needle).map(|at| (row[..at].chars().count() as u16, y as u16))
    })
}

/// The Connect chip on the row named `name`: a cell inside it.
fn chip(buf: &Buffer, name: &str) -> (u16, u16) {
    let rows = rows(buf);
    let (y, line) = rows
        .iter()
        .enumerate()
        .find(|(_, r)| r.to_lowercase().contains(&format!(" {} ", name.to_lowercase())) && r.contains(" Connect "))
        .unwrap_or_else(|| panic!("no Connect on {name}'s row:\n{}", rows.join("\n")));
    let x = line[..line.rfind(" Connect ").unwrap()].chars().count() as u16 + 2;
    (x, y as u16)
}

fn capture(state: &str, buf: &Buffer) {
    if let Ok(dir) = std::env::var("SHELBI_LAUNCH_CAPTURE") {
        let name = format!("{state}-{}x{}", buf.area.width, buf.area.height);
        crate::theme::dump_dark_html(Path::new(&dir), &name, buf);
    }
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn press(app: &mut LaunchApp, code: KeyCode) -> Option<LaunchExit> {
    app.handle_event(key(code))
}

fn type_text(app: &mut LaunchApp, text: &str) {
    for c in text.chars() {
        app.handle_event(key(KeyCode::Char(c)));
    }
}

fn mouse(kind: MouseEventKind, (column, row): (u16, u16)) -> Event {
    Event::Mouse(MouseEvent { kind, column, row, modifiers: KeyModifiers::NONE })
}

fn click(app: &mut LaunchApp, at: (u16, u16)) -> Option<LaunchExit> {
    app.handle_event(mouse(MouseEventKind::Down(MouseButton::Left), at))
}

/// Tick until `done` or two seconds pass (the calls run on threads).
fn settle(app: &mut LaunchApp, mut done: impl FnMut(&LaunchApp) -> bool) -> Option<LaunchExit> {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        let (_, exit) = app.tick();
        if exit.is_some() {
            return exit;
        }
        if done(app) {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the background call didn't come back");
}

fn stage(app: &LaunchApp) -> Option<Stage> {
    app.connect_dialog().map(|d| d.flow().stage())
}

// --- Rendering ---------------------------------------------------------------

#[test]
fn the_projects_state_renders_at_every_size() {
    for (w, h) in SIZES {
        let mut app = app(figma_model());
        let buf = draw(&mut app, w, h);
        capture("projects", &buf);
        let screen = text(&buf);
        for needle in [launch::WELCOME_TITLE, "+ Add project", "≡ Connect to remote", launch::PROJECTS_HEADING, "Shelbi", "~/Workspaces/Shelbi"] {
            assert!(screen.contains(needle), "{needle:?} at {w}x{h}:\n{screen}");
        }
        // The keyboard starts on the first project: its pill.
        let (x, y) = find(&buf, "~/Workspaces/Shelbi").unwrap();
        assert_eq!(buf[(x, y)].bg, palette().selection, "{w}x{h}");
        assert!(rows(&buf).last().unwrap().contains("↑↓ Move"), "the keys at the bottom at {w}x{h}");
        if w >= screen::STACK_BELOW {
            // Side by side: the welcome in the filled left column, the lists right of it.
            let (tx, ty) = find(&buf, launch::WELCOME_TITLE).unwrap();
            let (px, _) = find(&buf, launch::PROJECTS_HEADING).unwrap();
            assert!(px > tx + 20, "{w}x{h}");
            assert_eq!(buf[(0, ty)].bg, palette().dialog);
            for needle in [launch::REMOTES_HEADING, "jlong@devbox:~/Workspaces/ContextStore", " Connect "] {
                assert!(screen.contains(needle), "{needle:?} at {w}x{h}:\n{screen}");
            }
        } else {
            // Stacked: the welcome in a band across the top, the buttons on one row.
            let (_, ty) = find(&buf, launch::WELCOME_TITLE).unwrap();
            let (_, py) = find(&buf, launch::PROJECTS_HEADING).unwrap();
            assert!(ty < 3 && py > ty + 5, "{w}x{h}:\n{screen}");
            assert_eq!(find(&buf, "+ Add project").unwrap().1, find(&buf, "≡ Connect to remote").unwrap().1);
            assert_eq!(buf[(w - 1, ty)].bg, palette().dialog, "the band spans the width");
        }
    }
}

#[test]
fn the_no_projects_state_renders_at_every_size() {
    for (w, h) in SIZES {
        let mut app = app(LaunchModel::default());
        let buf = draw(&mut app, w, h);
        capture("no-projects", &buf);
        let screen = text(&buf);
        for needle in [launch::WELCOME_TITLE, launch::EMPTY_TITLE, "Add your first project"] {
            assert!(screen.contains(needle), "{needle:?} at {w}x{h}:\n{screen}");
        }
        assert!(!screen.contains(launch::PROJECTS_HEADING));
        // Add project is the Primary with the keyboard.
        assert_eq!(app.screen().focus(), Target::AddProject);
        let (x, y) = find(&buf, "+ Add project").unwrap();
        assert_eq!(buf[(x, y)].bg, palette().button_focus, "{w}x{h}");
    }
}

#[test]
fn a_long_list_scrolls_to_the_keyboard_on_a_short_terminal() {
    let mut model = figma_model();
    model.projects = (0..12).map(|i| project(&format!("p{i:02}"), &format!("Project {i:02}"))).collect();
    let mut app = app(model);
    for _ in 0..11 {
        press(&mut app, KeyCode::Down);
    }
    assert_eq!(app.screen().focus(), Target::Project(11));
    let buf = draw(&mut app, 60, 20);
    let screen = text(&buf);
    assert!(screen.contains("Project 11") && !screen.contains("Project 00"), "{screen}");
    // The wheel scrolls back up.
    for _ in 0..40 {
        app.handle_event(mouse(MouseEventKind::ScrollUp, (30, 15)));
    }
    press(&mut app, KeyCode::Left);
    let buf = draw(&mut app, 60, 20);
    assert!(text(&buf).contains(launch::PROJECTS_HEADING));
}

#[test]
fn the_connect_dialog_renders_both_steps_at_every_size() {
    for (w, h) in SIZES {
        let mut app = app(figma_model());
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        let buf = draw(&mut app, w, h);
        capture("connect-host-empty", &buf);
        let screen = text(&buf);
        for needle in [remote::DIALOG_TITLE, remote::HOST_INTRO, remote::HOST_LABEL, "Use an SSH alias", "Cancel", "Find Sessions"] {
            assert!(screen.contains(needle), "{needle:?} at {w}x{h}:\n{screen}");
        }
        let dialog = app.connect_dialog().unwrap().rect(Rect::new(0, 0, w, h));
        crate::overlay::assert_dialog_border(&buf, dialog).unwrap();

        type_text(&mut app, "devbox");
        capture("connect-host", &draw(&mut app, w, h));

        press(&mut app, KeyCode::Enter);
        settle(&mut app, |a| stage(a) == Some(Stage::Choose));
        let buf = draw(&mut app, w, h);
        capture("connect-choose", &buf);
        let screen = text(&buf);
        for needle in ["Choose a session on devbox.", "✓ Connected as jlong", remote::CHANGE_HOST_LABEL, "jlong@devbox:~/Workspaces/shelbi", "Connect"] {
            assert!(screen.contains(needle), "{needle:?} at {w}x{h}:\n{screen}");
        }
    }
}

#[test]
fn the_connect_dialog_says_what_went_wrong_under_the_field() {
    for (w, h) in SIZES {
        let remote = FakeRemote::new().with_failing_host("build", launch::RemoteError::NoShelbi { host: "build".into() });
        let recents = RecentRemotes::at(std::env::temp_dir().join("shelbi-launch-tests-unused.json"));
        let (mut app, _, _) = app_with(figma_model(), remote, recents);
        app.handle_event(key(KeyCode::Left));
        app.handle_event(key(KeyCode::Down));
        app.handle_event(key(KeyCode::Enter));
        // Nothing typed: refused at once.
        press(&mut app, KeyCode::Enter);
        assert!(text(&draw(&mut app, w, h)).contains("Enter an SSH host."));
        type_text(&mut app, "build");
        press(&mut app, KeyCode::Enter);
        settle(&mut app, |a| a.connect_dialog().is_some_and(|d| d.flow().busy().is_none()));
        let buf = draw(&mut app, w, h);
        capture("connect-host-error", &buf);
        let screen = text(&buf);
        assert!(screen.contains("Shelbi isn't installed on build."), "{w}x{h}:\n{screen}");
        let (x, y) = find(&buf, "Shelbi isn't installed").unwrap();
        assert_eq!(buf[(x, y)].fg, palette().red);
        assert_eq!(stage(&app), Some(Stage::Host));
    }
}

#[test]
fn every_state_reads_in_the_light_theme() {
    use shelbi_app::theme::Appearance;
    crate::theme::with_appearance(Appearance::Light, || {
        let mut projects = app(figma_model());
        crate::theme::assert_light_readable("launch-projects", &draw(&mut projects, 100, 30));
        let mut empty = app(LaunchModel::default());
        crate::theme::assert_light_readable("launch-empty", &draw(&mut empty, 60, 20));
        press(&mut projects, KeyCode::Left);
        press(&mut projects, KeyCode::Down);
        press(&mut projects, KeyCode::Enter);
        type_text(&mut projects, "devbox");
        crate::theme::assert_light_readable("launch-connect-host", &draw(&mut projects, 100, 30));
        press(&mut projects, KeyCode::Enter);
        settle(&mut projects, |a| stage(a) == Some(Stage::Choose));
        crate::theme::assert_light_readable("launch-connect-choose", &draw(&mut projects, 100, 30));
    });
}

// --- Keyboard ----------------------------------------------------------------

#[test]
fn arrows_and_tab_walk_the_buttons_and_the_rows() {
    let mut app = app(figma_model());
    assert_eq!(app.screen().focus(), Target::Project(0));
    for want in [Target::Project(1), Target::Project(2), Target::Remote(0), Target::Remote(1), Target::Remote(1)] {
        press(&mut app, KeyCode::Down);
        assert_eq!(app.screen().focus(), want);
    }
    press(&mut app, KeyCode::Up);
    assert_eq!(app.screen().focus(), Target::Remote(0));
    press(&mut app, KeyCode::Left);
    assert_eq!(app.screen().focus(), Target::AddProject);
    press(&mut app, KeyCode::Down);
    assert_eq!(app.screen().focus(), Target::ConnectRemote);
    press(&mut app, KeyCode::Right);
    assert_eq!(app.screen().focus(), Target::Project(0));
    // Tab goes through everything and wraps; ⇧Tab back.
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.screen().focus(), Target::ConnectRemote);
    for _ in 0..7 {
        press(&mut app, KeyCode::Tab);
    }
    assert_eq!(app.screen().focus(), Target::ConnectRemote, "seven stops, around once");
}

#[test]
fn return_on_a_project_starts_the_daemon_then_opens_it() {
    let sandbox = Sandbox::new();
    sandbox.register("contextstore");
    let (mut app, daemon, _) = app_with(figma_model(), devbox(), sandbox.recents());
    press(&mut app, KeyCode::Down);
    assert_eq!(press(&mut app, KeyCode::Enter), None);
    assert!(app.screen().busy());
    assert!(text(&draw(&mut app, 100, 30)).contains("Opening ContextStore…"));
    let exit = settle(&mut app, |_| false);
    assert_eq!(exit, Some(LaunchExit::Open("contextstore".into())));
    assert_eq!(daemon.starts(), 1);
}

#[test]
fn a_project_that_wont_open_is_said_at_the_bottom() {
    let sandbox = Sandbox::new();
    let (mut app, daemon, _) = app_with(figma_model(), devbox(), sandbox.recents());
    press(&mut app, KeyCode::Enter);
    settle(&mut app, |a| !a.screen().busy());
    assert_eq!(daemon.starts(), 0, "not registered here: no daemon");
    let buf = draw(&mut app, 100, 30);
    capture("open-failed", &buf);
    let (x, y) = find(&buf, "There's no project named shelbi").expect("the reason");
    assert_eq!(y, 29);
    assert_eq!(buf[(x, y)].fg, palette().red);
}

#[test]
fn add_project_opens_the_form_and_esc_or_q_leaves() {
    let mut app = app(figma_model());
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Enter);
    assert!(app.adding_project());
    let buf = draw(&mut app, 100, 30);
    capture("add-project", &buf);
    assert!(text(&buf).contains("Add a project"));
    // q types into the form; Esc cancels it.
    press(&mut app, KeyCode::Char('q'));
    assert!(app.adding_project());
    press(&mut app, KeyCode::Esc);
    assert!(!app.adding_project());
    assert_eq!(press(&mut app, KeyCode::Char('q')), Some(LaunchExit::Quit));
    assert_eq!(press(&mut app, KeyCode::Esc), Some(LaunchExit::Quit));
    let ctrl_c = Event::Key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
    assert_eq!(app.handle_event(ctrl_c), Some(LaunchExit::Quit));
}

#[test]
fn add_project_creates_the_project_and_opens_it() {
    let sandbox = Sandbox::new();
    let repo = sandbox.home.join("acme");
    std::fs::create_dir_all(&repo).unwrap();
    let daemon = Arc::new(FakeDaemon::new());
    let mut app = LaunchApp::new(LaunchModel::default(), daemon, None, sandbox.recents(), &repo, false);
    press(&mut app, KeyCode::Enter);
    assert!(app.adding_project());
    type_text(&mut app, "acme");
    press(&mut app, KeyCode::Enter);
    let exit = settle(&mut app, |_| false);
    assert_eq!(exit, Some(LaunchExit::Open("acme".into())));
    assert!(shelbi_app::launch::route::project_registered("acme"));
}

#[test]
fn the_connect_flow_runs_by_keyboard_and_remembers_the_session() {
    let sandbox = Sandbox::new();
    let (mut app, _, remote) = app_with(LaunchModel::default(), devbox(), sandbox.recents());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert_eq!(stage(&app), Some(Stage::Host));
    type_text(&mut app, "devbox");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, |a| stage(a) == Some(Stage::Choose));
    assert_eq!(remote.searched(), ["devbox"]);
    assert_eq!(app.connect_dialog().unwrap().focus(), connect::Focus::Session(0));

    // Change Host goes back with the host kept; Find again.
    press(&mut app, KeyCode::BackTab);
    assert_eq!(app.connect_dialog().unwrap().focus(), connect::Focus::ChangeHost);
    press(&mut app, KeyCode::Enter);
    assert_eq!(stage(&app), Some(Stage::Host));
    assert_eq!(app.connect_dialog().unwrap().flow().host(), "devbox");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, |a| stage(a) == Some(Stage::Choose));

    // The second session, then the dialog's Connect: it opens, as a local
    // project does, and is remembered.
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Tab);
    press(&mut app, KeyCode::Tab);
    assert_eq!(app.connect_dialog().unwrap().focus(), connect::Focus::Primary);
    press(&mut app, KeyCode::Enter);
    capture("connect-connecting", &draw(&mut app, 100, 30));
    let exit = settle(&mut app, |_| false);
    let connected = remote.connected();
    assert_eq!(connected.iter().map(|s| s.project.as_str()).collect::<Vec<_>>(), ["ContextStore"]);
    assert_eq!(exit, Some(LaunchExit::OpenRemote(connected[0].clone())));
    assert_eq!(sandbox.recents().load()[0].session.project, "ContextStore");
}

#[test]
fn a_stopped_remote_daemon_is_started_only_when_the_user_asks() {
    let sandbox = Sandbox::new();
    let (mut app, _, remote) = app_with(LaunchModel::default(), devbox().with_stopped_daemon("devbox"), sandbox.recents());
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    type_text(&mut app, "devbox");
    press(&mut app, KeyCode::Enter);
    settle(&mut app, |a| stage(a) == Some(Stage::Choose));

    // Connect finds Shelbi stopped: said under the list, Start Shelbi offered.
    press(&mut app, KeyCode::Enter);
    settle(&mut app, |a| a.connect_dialog().is_some_and(|d| d.flow().busy().is_none()));
    let flow = app.connect_dialog().unwrap().flow();
    assert_eq!(flow.error(), Some("Shelbi isn't running on devbox."));
    assert_eq!(flow.primary_label(), "Start Shelbi");
    assert!(remote.started().is_empty(), "nothing started unasked");
    let buf = draw(&mut app, 100, 30);
    assert!(find(&buf, "Start Shelbi").is_some(), "{}", text(&buf));
    capture("connect-daemon-stopped", &buf);

    // Start Shelbi starts it, then connects.
    for _ in 0..6 {
        if app.connect_dialog().unwrap().focus() == connect::Focus::Primary {
            break;
        }
        press(&mut app, KeyCode::Tab);
    }
    assert_eq!(app.connect_dialog().unwrap().focus(), connect::Focus::Primary);
    press(&mut app, KeyCode::Enter);
    let exit = settle(&mut app, |_| false);
    assert_eq!(remote.started(), ["devbox"]);
    assert_eq!(exit, Some(LaunchExit::OpenRemote(remote.connected()[0].clone())));
}

#[test]
fn without_a_connector_connecting_is_not_offered() {
    for (w, h) in SIZES {
        let recents = RecentRemotes::at(std::env::temp_dir().join("shelbi-launch-tests-unused.json"));
        let mut app = LaunchApp::new(figma_model(), Arc::new(FakeDaemon::new()), None, recents, Path::new("/tmp"), false);
        let buf = draw(&mut app, w, h);
        capture("projects-no-connector", &buf);
        let screen = text(&buf);
        assert!(screen.contains("+ Add project") && !screen.contains("Connect to remote"), "{w}x{h}:\n{screen}");
        assert!(!screen.contains(" Connect "), "no Connect on the recent sessions at {w}x{h}");
        // The recent rows don't take the keyboard or a click.
        for _ in 0..6 {
            press(&mut app, KeyCode::Down);
        }
        assert_eq!(app.screen().focus(), Target::Project(2));
        press(&mut app, KeyCode::Left);
        press(&mut app, KeyCode::Down);
        assert_eq!(app.screen().focus(), Target::AddProject);
        for _ in 0..4 {
            press(&mut app, KeyCode::Tab);
        }
        assert_eq!(app.screen().focus(), Target::AddProject, "four stops, around once");
        if let Some(at) = find(&buf, "jlong@devbox:~/Workspaces/shelbi") {
            click(&mut app, at);
            assert!(!app.screen().busy() && app.screen().focus() == Target::AddProject);
        }
    }
}

#[test]
fn esc_cancels_the_connect_dialog() {
    let mut app = app(figma_model());
    press(&mut app, KeyCode::Left);
    press(&mut app, KeyCode::Down);
    press(&mut app, KeyCode::Enter);
    assert!(app.connect_dialog().is_some());
    assert_eq!(press(&mut app, KeyCode::Esc), None, "Esc closes the dialog, not the screen");
    assert!(app.connect_dialog().is_none());
}

// --- Mouse -------------------------------------------------------------------

#[test]
fn clicks_press_buttons_open_projects_and_connect_recent_sessions() {
    let sandbox = Sandbox::new();
    sandbox.register("32pixels");
    let (mut app, _, remote) = app_with(figma_model(), devbox(), sandbox.recents());
    let buf = draw(&mut app, 160, 45);

    // Add project, then the form's Esc.
    click(&mut app, find(&buf, "+ Add project").unwrap());
    assert!(app.adding_project());
    press(&mut app, KeyCode::Esc);

    // Connect to remote, then Cancel by click.
    click(&mut app, find(&buf, "≡ Connect to remote").unwrap());
    assert!(app.connect_dialog().is_some());
    let dialog = draw(&mut app, 160, 45);
    click(&mut app, find(&dialog, "Cancel").unwrap());
    assert!(app.connect_dialog().is_none());

    // A recent session's body takes the keyboard; its Connect connects.
    let body = find(&buf, "jlong@devbox:~/Workspaces/ContextStore").unwrap();
    click(&mut app, body);
    assert_eq!(app.screen().focus(), Target::Remote(1));
    assert!(remote.connected().is_empty());
    let buf = draw(&mut app, 160, 45);
    click(&mut app, chip(&buf, "ContextStore"));
    let exit = settle(&mut app, |_| false);
    assert_eq!(exit, Some(LaunchExit::OpenRemote(session("ContextStore", "ContextStore"))));
    assert_eq!(remote.connected().len(), 1);
    assert_eq!(sandbox.recents().load()[0].session.project, "ContextStore");

    // A project row opens it.
    let (mut app, daemon, _) = app_with(figma_model(), devbox(), sandbox.recents());
    let buf = draw(&mut app, 160, 45);
    click(&mut app, find(&buf, "~/Workspaces/32pixels").unwrap());
    assert_eq!(settle(&mut app, |_| false), Some(LaunchExit::Open("32pixels".into())));
    assert_eq!(daemon.starts(), 1);
}

#[test]
fn the_pointer_lights_the_row_under_it() {
    let mut app = app(figma_model());
    let buf = draw(&mut app, 100, 30);
    let at = find(&buf, "~/Workspaces/32pixels").unwrap();
    app.handle_event(mouse(MouseEventKind::Moved, at));
    let buf = draw(&mut app, 100, 30);
    capture("projects-hover", &buf);
    assert_eq!(buf[at].bg, palette().row_hover);
    assert_eq!(app.screen().focus(), Target::Project(0), "hovering doesn't move the keyboard");
}

#[test]
fn the_connect_dialog_works_by_mouse() {
    let sandbox = Sandbox::new();
    let (mut app, _, remote) = app_with(LaunchModel::default(), devbox(), sandbox.recents());
    let buf = draw(&mut app, 100, 30);
    click(&mut app, find(&buf, "≡ Connect to remote").unwrap());
    type_text(&mut app, "devbox");
    let buf = draw(&mut app, 100, 30);
    click(&mut app, find(&buf, "Find Sessions").unwrap());
    settle(&mut app, |a| stage(a) == Some(Stage::Choose));
    let buf = draw(&mut app, 100, 30);
    // The first session's own Connect.
    click(&mut app, chip(&buf, "shelbi"));
    let exit = settle(&mut app, |_| false);
    assert!(matches!(exit, Some(LaunchExit::OpenRemote(s)) if s.project == "shelbi"));
    assert_eq!(remote.connected()[0].project, "shelbi");
}
