//! The launch window and the Connect to Remote Session dialog over it: both
//! states, the keys, opening a project (the daemon started first, through a
//! fake), Add project, reconnecting, and the dialog's two screens with their
//! inline errors. Every test sets its own [`LaunchServices`] (the
//! foundation's `FakeRemote`, a throwaway recent list) and [`TestDeps`], so
//! nothing starts a daemon, runs `ssh` or touches the real Shelbi home.

use std::cell::RefCell;
use std::path::PathBuf;

use chrono::{TimeZone, Utc};
use gpui::{TestAppContext, VisualTestContext};
use shelbi_app::launch::connect::Stage;
use shelbi_app::launch::{FakeDaemon, FakeRemote, LocalProject, RemoteError};

use super::*;
use crate::views::connect_remote::test_services;

fn project(slug: &str) -> LocalProject {
    LocalProject {
        slug: slug.into(),
        label: slug.into(),
        path: format!("/Users/jlong/Workspaces/{slug}"),
        display_path: format!("~/Workspaces/{slug}"),
    }
}

fn session(project: &str) -> RemoteSession {
    RemoteSession {
        host: "devbox".into(),
        user: "jlong".into(),
        project: project.into(),
        label: None,
        path: format!("~/Workspaces/{project}"),
    }
}

/// A throwaway Shelbi home with `projects` registered, set as `SHELBI_HOME`
/// while the guard lives (the shared registration check reads it).
struct Home {
    dir: PathBuf,
    prev: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

impl Home {
    fn new(tag: &str, projects: &[&str]) -> Home {
        let lock = crate::test_env::lock();
        let dir = std::env::temp_dir().join(format!("shb-launch-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let prev = std::env::var_os("SHELBI_HOME");
        std::env::set_var("SHELBI_HOME", &dir);
        let registry = shelbi_state::projects_dir().unwrap();
        std::fs::create_dir_all(&registry).unwrap();
        for p in projects {
            let repo = dir.join("repos").join(p);
            std::fs::create_dir_all(&repo).unwrap();
            let yaml = format!(
                "name: {p}\nrepo: {r}\nmachines:\n  - {{ name: hub, kind: local, work_dir: {r} }}\norchestrator: {{ runner: claude }}\nagent_runners:\n  claude: {{ command: claude, flags: [] }}\nworkspaces: []\n",
                r = repo.display()
            );
            std::fs::write(registry.join(format!("{p}.yaml")), yaml).unwrap();
        }
        Home { dir, prev, _lock: lock }
    }

    fn recents(&self) -> RecentRemotes {
        RecentRemotes::at(self.dir.join(launch::RECENT_REMOTES_FILE))
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        match &self.prev {
            Some(v) => std::env::set_var("SHELBI_HOME", v),
            None => std::env::remove_var("SHELBI_HOME"),
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Rig {
    cx: VisualTestContext,
    view: Entity<LaunchView>,
    daemon: Arc<FakeDaemon>,
    opened: Rc<RefCell<Vec<String>>>,
    added: Rc<RefCell<usize>>,
}

/// A launch window over `model` (the recent sessions come from `recents`,
/// as the real load reads them), the dialog and reconnects run against
/// `remote`.
fn rig(cx: &mut TestAppContext, projects: Vec<LocalProject>, remote: FakeRemote, recents: RecentRemotes, daemon: FakeDaemon) -> Rig {
    super::super::about::tests::init(cx);
    let daemon = Arc::new(daemon);
    let opened = Rc::new(RefCell::new(Vec::new()));
    let added = Rc::new(RefCell::new(0));
    let deps = Deps {
        load: Arc::new(move |recents| LaunchModel {
            projects: projects.clone(),
            remotes: recents.load(),
        }),
        daemon: Rc::new({
            let daemon = daemon.clone();
            move |_| daemon.clone()
        }),
        open: Rc::new({
            let opened = opened.clone();
            move |slug, _| opened.borrow_mut().push(slug.to_string())
        }),
        add: Rc::new({
            let added = added.clone();
            move |_| *added.borrow_mut() += 1
        }),
    };
    cx.update(|cx| {
        cx.set_global(test_services(remote, recents));
        cx.set_global(TestDeps(deps));
    });
    let handle = cx.update(|cx| show(cx).expect("the launch window opens"));
    let mut vcx = VisualTestContext::from_window(handle.into(), cx);
    vcx.run_until_parked();
    let view = handle.root(&mut vcx).unwrap();
    Rig {
        cx: vcx,
        view,
        daemon,
        opened,
        added,
    }
}

impl Rig {
    fn read<R>(&mut self, f: impl FnOnce(&LaunchView) -> R) -> R {
        self.view.read_with(&self.cx, |v, _| f(v))
    }

    fn keys(&mut self, keys: &str) {
        self.cx.simulate_keystrokes(keys);
        self.cx.run_until_parked();
    }

    fn click(&mut self, selector: &'static str) {
        let bounds = self
            .cx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("{selector} isn't drawn"));
        self.cx.simulate_click(bounds.center(), gpui::Modifiers::none());
        self.cx.run_until_parked();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.cx.run_until_parked();
        self.cx.debug_bounds(selector).is_some()
    }

    fn dialog(&mut self) -> Entity<ConnectDialog> {
        self.read(|v| v.dialog()).expect("the Connect dialog is open")
    }

    fn step(&mut self) -> Stage {
        let dialog = self.dialog();
        dialog.read_with(&self.cx, |d, _| d.flow().stage())
    }

    fn type_host(&mut self, host: &str) {
        let dialog = self.dialog();
        dialog.update_in(&mut self.cx, |d, window, cx| d.type_host(host, window, cx));
        self.cx.run_until_parked();
    }
}

fn remembered(recents: &RecentRemotes, project: &str) {
    recents
        .remember(session(project), Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap())
        .unwrap();
}

#[gpui::test]
fn the_projects_state_lists_both_sections_with_the_first_row_selected(cx: &mut TestAppContext) {
    let home = Home::new("lists", &[]);
    let recents = home.recents();
    remembered(&recents, "shelbi");
    let mut r = rig(cx, vec![project("shelbi"), project("web")], FakeRemote::new(), recents, FakeDaemon::new());
    assert_eq!(r.read(|v| v.model().state()), LaunchState::Projects);
    assert!(r.drawn("launch-projects"));
    assert!(r.drawn("launch-remotes"));
    assert!(r.drawn("launch-remote-0-connect"));
    assert!(!r.drawn("launch-empty"));
    assert_eq!(r.read(|v| v.selected()), 0, "the first row starts selected, as the frame draws it");

    // ↓ walks from the projects into the remote sessions and stops there.
    r.keys("down down down");
    assert_eq!(r.read(|v| v.selected()), 2);
    r.keys("up");
    assert_eq!(r.read(|v| v.selected()), 1);
}

#[gpui::test]
fn no_projects_shows_the_empty_state_and_return_adds_one(cx: &mut TestAppContext) {
    let _home = Home::new("empty", &[]);
    let mut r = rig(cx, Vec::new(), FakeRemote::new(), RecentRemotes::none(), FakeDaemon::new());
    assert_eq!(r.read(|v| v.model().state()), LaunchState::NoProjects);
    assert!(r.drawn("launch-empty"));
    assert!(!r.drawn("launch-projects"));
    r.keys("enter");
    assert_eq!(*r.added.borrow(), 1, "Return is Add project, the Primary button here");
    r.click("launch-add-project");
    assert_eq!(*r.added.borrow(), 2);
    assert_eq!(r.daemon.starts(), 0);
}

#[gpui::test]
fn opening_a_project_starts_the_daemon_then_opens_its_window(cx: &mut TestAppContext) {
    let _home = Home::new("open", &["shelbi", "web"]);
    let mut r = rig(cx, vec![project("shelbi"), project("web")], FakeRemote::new(), RecentRemotes::none(), FakeDaemon::new());
    r.keys("down enter");
    assert_eq!(r.daemon.starts(), 1);
    assert_eq!(*r.opened.borrow(), ["web"]);
    assert_eq!(r.read(|v| v.error().map(str::to_string)), None);

    r.click("launch-project-0");
    assert_eq!(r.daemon.starts(), 2);
    assert_eq!(*r.opened.borrow(), ["web", "shelbi"]);
}

#[gpui::test]
fn a_daemon_that_wont_start_opens_nothing_and_says_why(cx: &mut TestAppContext) {
    let _home = Home::new("open-fail", &["shelbi"]);
    let daemon = FakeDaemon::failing("the daemon exited during startup");
    let mut r = rig(cx, vec![project("shelbi")], FakeRemote::new(), RecentRemotes::none(), daemon);
    r.keys("enter");
    assert!(r.opened.borrow().is_empty());
    assert_eq!(
        r.read(|v| v.error().map(str::to_string)).as_deref(),
        Some("Shelbi couldn't start: the daemon exited during startup")
    );
    assert!(r.drawn("launch-error"));
}

#[gpui::test]
fn a_project_gone_since_the_list_was_read_starts_no_daemon(cx: &mut TestAppContext) {
    let _home = Home::new("gone", &[]);
    let mut r = rig(cx, vec![project("gone")], FakeRemote::new(), RecentRemotes::none(), FakeDaemon::new());
    r.keys("enter");
    assert_eq!(r.daemon.starts(), 0);
    assert_eq!(
        r.read(|v| v.error().map(str::to_string)).as_deref(),
        Some("There's no project named gone on this machine.")
    );
    assert!(r.opened.borrow().is_empty());
}

#[gpui::test]
fn a_recent_sessions_connect_reconnects_and_moves_it_to_the_top(cx: &mut TestAppContext) {
    let home = Home::new("reconnect", &[]);
    let recents = home.recents();
    recents.remember(session("shelbi"), Utc.with_ymd_and_hms(2026, 10, 1, 9, 0, 0).unwrap()).unwrap();
    recents.remember(session("web"), Utc.with_ymd_and_hms(2026, 10, 2, 9, 0, 0).unwrap()).unwrap();
    let remote = FakeRemote::new();
    let mut r = rig(cx, Vec::new(), remote, recents.clone(), FakeDaemon::new());
    let order = |r: &mut Rig| r.read(|v| v.model().remotes.iter().map(|x| x.session.project.clone()).collect::<Vec<_>>());
    assert_eq!(order(&mut r), ["web", "shelbi"]);
    r.click("launch-remote-1-connect");
    assert_eq!(order(&mut r), ["shelbi", "web"], "connected, so first");
    assert_eq!(recents.load()[0].session, session("shelbi"));
    assert_eq!(r.daemon.starts(), 0, "a remote session starts no local daemon");
}

#[gpui::test]
fn a_refused_reconnect_shows_the_error(cx: &mut TestAppContext) {
    let home = Home::new("refused", &[]);
    let recents = home.recents();
    remembered(&recents, "shelbi");
    let remote = FakeRemote::new().failing_connect(RemoteError::Unreachable {
        host: "devbox".into(),
        reason: "Connection refused".into(),
    });
    let mut r = rig(cx, Vec::new(), remote, recents, FakeDaemon::new());
    r.keys("enter");
    assert_eq!(
        r.read(|v| v.error().map(str::to_string)).as_deref(),
        Some("Couldn't connect to devbox: Connection refused")
    );
}

#[gpui::test]
fn the_dialog_finds_sessions_then_connects_and_remembers(cx: &mut TestAppContext) {
    let home = Home::new("dialog", &[]);
    let recents = home.recents();
    let remote = FakeRemote::new().with_host(
        "build-server",
        "jlong",
        &[("shelbi", "~/Workspaces/shelbi"), ("ContextStore", "~/Workspaces/ContextStore")],
    );
    let mut r = rig(cx, vec![project("shelbi")], remote, recents.clone(), FakeDaemon::new());
    r.click("launch-connect-remote");
    assert!(r.drawn("connect-dialog"));
    assert_eq!(r.step(), Stage::Host);
    assert!(r.drawn("connect-host-hint"));

    r.type_host("build-server");
    r.keys("enter");
    assert_eq!(r.step(), Stage::Choose);
    assert!(r.drawn("connect-connected"));
    assert!(r.drawn("connect-session-1-connect"));

    // ↓ then Return connects the second session; the dialog closes and the
    // launch window lists it.
    r.keys("down enter");
    assert!(r.read(|v| v.dialog().is_none()), "connected, so closed");
    assert_eq!(recents.load()[0].session.project, "ContextStore");
    assert_eq!(r.read(|v| v.model().remotes.len()), 1);
}

#[gpui::test]
fn a_stopped_remote_daemon_starts_only_on_start_shelbi_then_its_window_opens(cx: &mut TestAppContext) {
    use crate::views::remote_session::tests::OPENED;
    OPENED.with(|o| o.borrow_mut().clear());
    let home = Home::new("stopped", &[]);
    let recents = home.recents();
    let remote = FakeRemote::new()
        .with_host("devbox", "jlong", &[("shelbi", "~/Workspaces/shelbi")])
        .with_stopped_daemon("devbox");
    let mut r = rig(cx, Vec::new(), remote, recents.clone(), FakeDaemon::new());
    r.click("launch-connect-remote");
    r.type_host("devbox");
    r.keys("enter");
    assert_eq!(r.step(), Stage::Choose);

    // Connect finds Shelbi stopped there: said, and Start Shelbi offered.
    r.click("connect-connect");
    let dialog = r.dialog();
    let (error, label) = dialog.read_with(&r.cx, |d, _| {
        (d.flow().error().map(str::to_string), d.flow().primary_label())
    });
    assert_eq!(error.as_deref(), Some("Shelbi isn't running on devbox."));
    assert_eq!(label, "Start Shelbi");
    assert!(recents.load().is_empty(), "not connected, so not remembered");
    assert!(OPENED.with(|o| o.borrow().is_empty()));

    // Start Shelbi starts it (the fake refuses to connect until then) and
    // connects: the dialog closes and the session's window opens.
    r.click("connect-connect");
    assert!(r.read(|v| v.dialog().is_none()), "connected, so closed");
    assert_eq!(recents.load()[0].session.project, "shelbi");
    let opened: Vec<String> = OPENED.with(|o| o.borrow().iter().map(|s| s.project.clone()).collect());
    assert_eq!(opened, ["shelbi"]);
}

#[gpui::test]
fn ssh_errors_show_under_the_host_field(cx: &mut TestAppContext) {
    let _home = Home::new("dialog-errors", &[]);
    let remote = FakeRemote::new().with_failing_host("down", RemoteError::NoShelbi { host: "down".into() });
    let mut r = rig(cx, Vec::new(), remote, RecentRemotes::none(), FakeDaemon::new());
    r.click("launch-connect-remote");
    r.type_host("down");
    r.click("connect-find");
    assert_eq!(r.step(), Stage::Host);
    assert!(r.drawn("connect-error"));
    let dialog = r.dialog();
    assert_eq!(
        dialog.read_with(&r.cx, |d, _| d.flow().error().map(str::to_string)).as_deref(),
        Some("Shelbi isn't installed on down.")
    );
    r.type_host("down2");
    assert!(!r.drawn("connect-error"), "editing the host clears the error");
}

#[gpui::test]
fn change_host_goes_back_and_esc_cancels(cx: &mut TestAppContext) {
    let _home = Home::new("dialog-back", &[]);
    let remote = FakeRemote::new().with_host("devbox", "jlong", &[("shelbi", "~/Workspaces/shelbi")]);
    let mut r = rig(cx, Vec::new(), remote, RecentRemotes::none(), FakeDaemon::new());
    r.click("launch-connect-remote");
    r.type_host("devbox");
    r.keys("enter");
    assert_eq!(r.step(), Stage::Choose);
    r.click("connect-change-host");
    assert_eq!(r.step(), Stage::Host);
    r.keys("escape");
    assert!(r.read(|v| v.dialog().is_none()));

    // ⌘W cancels the dialog before it closes the window.
    r.click("launch-connect-remote");
    r.cx.dispatch_action(CloseWindow);
    r.cx.run_until_parked();
    assert!(r.read(|v| v.dialog().is_none()));
    assert!(r.cx.update(|_, cx| open_window(cx).is_some()), "the window stays");
}

#[gpui::test]
fn the_launch_window_closes_once_a_project_window_opens(cx: &mut TestAppContext) {
    let _home = Home::new("closes", &[]);
    let _r = rig(cx, Vec::new(), FakeRemote::new(), RecentRemotes::none(), FakeDaemon::new());
    let before = cx.update(|cx| cx.windows().len());
    cx.update(project_window_opened);
    cx.run_until_parked();
    let after = cx.update(|cx| (cx.windows().len(), open_window(cx).is_some()));
    assert_eq!(after, (before - 1, false));
}

#[gpui::test]
fn its_menu_bar_offers_connect_with_no_project(cx: &mut TestAppContext) {
    let keymaps = shelbi_state::keymap::validate_keymaps_yaml("", None).0;
    let tree = crate::menus::tree::build(&command_model(), &keymaps, Default::default());
    let connect = tree
        .items()
        .into_iter()
        .find(|i| i.title == "Connect to Remote Session…")
        .expect("Shelbi ▸ Connect to Remote Session…");
    assert!(connect.enabled);
    let _ = cx;
}
