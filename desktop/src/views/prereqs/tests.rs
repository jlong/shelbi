use std::cell::RefCell;
use std::sync::Mutex;

use gpui::{Modifiers, TestAppContext, VisualTestContext};
use shelbi_app::prereqs::{FakeProbe, Phase, RowState};

use super::*;

const GIT: (&str, &str) = ("/usr/bin/git", "git version 2.39.5");
const GH: (&str, &str) = ("/opt/homebrew/bin/gh", "gh version 2.62.0");
const CLAUDE: (&str, &str) = ("/opt/homebrew/bin/claude", "2.1.4 (Claude Code)");
const BREW: (&str, &str) = ("/opt/homebrew/bin/brew", "Homebrew 4.4.6");

/// A machine the test can install tools on between checks.
struct Machine(Mutex<FakeProbe>);

impl Probe for Machine {
    fn path_dirs(&self) -> Vec<PathBuf> {
        self.0.lock().unwrap().path_dirs()
    }
    fn is_executable(&self, path: &std::path::Path) -> bool {
        self.0.lock().unwrap().is_executable(path)
    }
    fn version(&self, path: &std::path::Path) -> Result<String, String> {
        self.0.lock().unwrap().version(path)
    }
    fn command_line_tools(&self) -> bool {
        true
    }
}

struct Rig {
    cx: VisualTestContext,
    view: Entity<PrereqsView>,
    machine: Arc<Machine>,
    exits: Rc<RefCell<Vec<Exit>>>,
    saved: Arc<Mutex<Vec<PathBuf>>>,
}

fn rig(cx: &mut TestAppContext, exes: &[(&str, &str)]) -> Rig {
    rig_on(cx, FakeProbe::new(exes).dir("/opt/homebrew/bin"))
}

fn rig_on(cx: &mut TestAppContext, probe: FakeProbe) -> Rig {
    cx.update(|cx| {
        gpui_kit::init(cx);
        theme::init(cx);
    });
    let machine = Arc::new(Machine(Mutex::new(probe)));
    let exits: Rc<RefCell<Vec<Exit>>> = Rc::default();
    let saved: Arc<Mutex<Vec<PathBuf>>> = Arc::default();
    let deps = Deps {
        probe: machine.clone(),
        located: Arc::new(ToolPaths::default),
        save: Arc::new({
            let saved = saved.clone();
            move |found| {
                saved.lock().unwrap().push(found.path.clone());
                Ok(())
            }
        }),
        done: Rc::new({
            let exits = exits.clone();
            move |exit, _, _, _| exits.borrow_mut().push(exit)
        }),
        home: Some("/Users/me".into()),
        platform: Platform::Mac,
    };
    let (view, cx) = cx.add_window_view(|window, cx| PrereqsView::new(deps, window, cx));
    cx.simulate_resize(gpui::size(px(SIZE.0), px(SIZE.1)));
    let mut rig = Rig { cx: cx.clone(), view, machine, exits, saved };
    rig.settle();
    rig
}

impl Rig {
    fn settle(&mut self) {
        self.cx.run_until_parked();
        self.cx.update(|window, cx| window.draw(cx).clear(cx));
    }

    fn click(&mut self, selector: &'static str) {
        let bounds = self.cx.debug_bounds(selector).unwrap_or_else(|| panic!("{selector} isn't drawn"));
        self.cx.simulate_click(bounds.center(), Modifiers::none());
        self.settle();
    }

    fn drawn(&mut self, selector: &'static str) -> bool {
        self.cx.debug_bounds(selector).is_some()
    }

    fn phase(&mut self) -> Phase {
        self.view.read_with(&self.cx, |v, _| v.model().phase())
    }
}

#[gpui::test]
fn everything_found_goes_on_without_a_stop(cx: &mut TestAppContext) {
    let r = rig(cx, &[GIT, GH, CLAUDE, BREW]);
    assert_eq!(*r.exits.borrow(), [Exit::Continue]);
}

#[gpui::test]
fn copy_puts_the_command_on_the_clipboard_and_reads_copied_for_a_while(cx: &mut TestAppContext) {
    let mut r = rig(cx, &[GIT, BREW]);
    assert_eq!(r.phase(), Phase::Missing);
    assert!(r.exits.borrow().is_empty(), "a missing tool stops the launch");
    assert!(r.drawn("prereq-command-gh") && r.drawn("prereq-command-claude") && r.drawn("prereq-command-codex"));
    r.click("prereq-copy-gh");
    let clip = r.cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip.as_deref(), Some("brew install gh"));
    assert!(r.view.read_with(&r.cx, |v, _| v.copied.as_ref().map(|(t, _)| t.clone())).is_some());
    r.cx.executor().advance_clock(model::COPIED_FOR);
    r.settle();
    assert!(r.view.read_with(&r.cx, |v, _| v.copied.is_none()), "Copy again after two seconds");
}

#[gpui::test]
fn skip_asks_first_then_goes_on(cx: &mut TestAppContext) {
    let mut r = rig(cx, &[GIT, BREW, CLAUDE]);
    r.click("prereq-skip");
    assert!(r.drawn("prereq-skip-dialog"));
    let listed: Vec<Tool> = r.view.read_with(&r.cx, |v, _| v.model().consequences().into_iter().map(|(t, _)| t).collect());
    assert_eq!(listed, [Tool::GithubCli], "only what's missing");
    r.click("prereq-go-back");
    assert!(!r.view.read_with(&r.cx, |v, _| v.skip_dialog_open()));
    assert!(r.exits.borrow().is_empty());
    r.click("prereq-skip");
    r.click("prereq-skip-anyway");
    assert_eq!(*r.exits.borrow(), [Exit::Skip]);
}

#[gpui::test]
fn check_again_finds_a_new_install_and_continue_goes_on(cx: &mut TestAppContext) {
    let mut r = rig(cx, &[GIT, BREW, GH]);
    assert_eq!(r.view.read_with(&r.cx, |v, _| v.model().missing()), [Tool::Agent]);
    {
        let mut m = r.machine.0.lock().unwrap();
        *m = m.clone().with(CLAUDE.0, CLAUDE.1);
    }
    r.click("prereq-check-again");
    assert_eq!(r.phase(), Phase::Ready);
    assert!(r.exits.borrow().is_empty(), "after a miss, Ready waits for Continue");
    r.click("prereq-continue");
    assert_eq!(*r.exits.borrow(), [Exit::Continue]);
}

#[gpui::test]
fn locate_checks_the_path_and_remembers_it(cx: &mut TestAppContext) {
    // gh is installed, but not anywhere on the PATH.
    let mut probe = FakeProbe::new(&[GIT, BREW, CLAUDE]);
    probe.exes.insert("/Users/me/tools/gh".into(), Ok("gh version 2.62.0".into()));
    let mut r = rig_on(cx, probe);
    // The Mac app names the Terminal app, once, over the rows.
    assert_eq!(r.view.read_with(&r.cx, |v, _| v.model().body()), model::MISSING_ONE_BODY_MAC);
    r.click("prereq-locate-GithubCli");
    assert!(r.drawn("prereq-locate-path"));
    let input = r.view.read_with(&r.cx, |v, _| v.path_input.clone());
    r.cx.update(|window, cx| input.update(cx, |s, cx| s.set_value("~/nope/gh", window, cx)));
    r.view.update(&mut r.cx, |v, cx| v.validate(Tool::GithubCli, cx));
    r.settle();
    assert!(r.drawn("prereq-locate-error"));
    r.cx.update(|window, cx| input.update(cx, |s, cx| s.set_value("~/tools/gh", window, cx)));
    r.view.update(&mut r.cx, |v, cx| v.validate(Tool::GithubCli, cx));
    r.settle();
    assert!(r.drawn("prereq-located"));
    assert!(matches!(
        r.view.read_with(&r.cx, |v, _| v.model().state(Tool::GithubCli).clone()),
        RowState::Found(Found { located: true, .. })
    ));
    assert_eq!(*r.saved.lock().unwrap(), [PathBuf::from("/Users/me/tools/gh")]);
    assert_eq!(r.phase(), Phase::Ready);
    r.click("prereq-continue");
    assert_eq!(*r.exits.borrow(), [Exit::Continue]);
}

#[gpui::test]
fn a_long_list_scrolls_inside_the_fixed_window(cx: &mut TestAppContext) {
    // Nothing installed: four missing rows don't fit.
    let mut r = rig(cx, &[]);
    r.settle();
    assert!(r.drawn("prereq-fade") && r.drawn("prereq-scrollbar"), "the fade and the scrollbar show");
    let list = r.cx.debug_bounds("prereq-list").unwrap();
    let actions = r.cx.debug_bounds("prereq-check-again").unwrap();
    assert!(list.bottom() <= actions.top(), "the list stops above the actions");
}
