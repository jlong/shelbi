use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;
use std::rc::Rc;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;

use super::*;

/// A machine with these executables and `--version` answers.
struct Fake {
    dirs: Vec<PathBuf>,
    exes: BTreeMap<PathBuf, String>,
}

impl Probe for Fake {
    fn path_dirs(&self) -> Vec<PathBuf> {
        self.dirs.clone()
    }
    fn is_executable(&self, path: &Path) -> bool {
        self.exes.contains_key(path)
    }
    fn version(&self, path: &Path) -> Result<String, String> {
        self.exes.get(path).cloned().ok_or_else(|| "no such file".into())
    }
    fn command_line_tools(&self) -> bool {
        true
    }
}

fn fake(exes: &[(&str, &str)]) -> Arc<Fake> {
    Arc::new(Fake {
        dirs: vec!["/usr/bin".into(), "/opt/homebrew/bin".into()],
        exes: exes.iter().map(|(p, v)| (PathBuf::from(p), v.to_string())).collect(),
    })
}

const GIT: (&str, &str) = ("/usr/bin/git", "git version 2.39.5");
const BREW: (&str, &str) = ("/opt/homebrew/bin/brew", "Homebrew 4.4.6");
const GH: (&str, &str) = ("/opt/homebrew/bin/gh", "gh version 2.62.0");
const CLAUDE: (&str, &str) = ("/opt/homebrew/bin/claude", "2.1.4 (Claude Code)");

/// The app with a recording clipboard.
fn app(probe: Arc<Fake>) -> (PrereqApp, Rc<RefCell<Vec<String>>>) {
    let copied: Rc<RefCell<Vec<String>>> = Rc::default();
    let sink = copied.clone();
    let app = PrereqApp::new(Platform::Mac, probe, ToolPaths::default(), Some("/Users/me".into()))
        .with_copier(Box::new(move |t| sink.borrow_mut().push(t.to_string())));
    (app, copied)
}

/// Tick until the check run finishes (or the screen exits).
fn settle(app: &mut PrereqApp) -> Option<PrereqExit> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let (_, exit) = app.tick();
        if exit.is_some() {
            return exit;
        }
        if app.model().phase() != Phase::Checking && app.validating.is_none() {
            return None;
        }
        assert!(Instant::now() < deadline, "the check never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn key(app: &mut PrereqApp, code: KeyCode) -> Option<PrereqExit> {
    app.handle_key(KeyEvent::new(code, KeyModifiers::NONE))
}

fn screen(app: &mut PrereqApp, w: u16, h: u16) -> String {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| app.draw(f)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect::<String>().trim_end().to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn everything_found_on_the_first_check_continues_without_a_stop() {
    let (mut app, _) = app(fake(&[GIT, BREW, GH, CLAUDE]));
    assert_eq!(settle(&mut app), Some(PrereqExit::Continue));
}

#[test]
fn missing_tools_show_rows_commands_and_keys() {
    let (mut app, _) = app(fake(&[GIT, BREW]));
    assert_eq!(settle(&mut app), None);
    let s = screen(&mut app, 100, 40);
    assert!(s.contains(model::MISSING_TITLE), "{s}");
    assert!(s.contains("✓ Git") && s.contains("2.39.5 · /usr/bin/git"), "{s}");
    assert!(s.contains("✕ GitHub CLI"), "{s}");
    assert!(s.contains("brew install gh"), "{s}");
    assert!(s.contains("brew install --cask claude-code") && s.contains("brew install --cask codex"), "{s}");
    assert!(s.contains("Locate gh…"), "{s}");
    assert!(s.contains("Skip for now") && s.contains("Check again"), "{s}");
    assert!(s.contains("c Copy") && s.contains("s Skip"), "{s}");
}

#[test]
fn copy_puts_exactly_the_command_on_the_clipboard_then_says_copied() {
    let (mut app, copied) = app(fake(&[GIT, BREW]));
    settle(&mut app);
    // The first stop is GitHub CLI's command (Git is found and first).
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(*copied.borrow(), ["brew install gh"]);
    assert!(screen(&mut app, 100, 40).contains("✓ Copied"));
    // The agent row's two commands are stops of their own.
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(copied.borrow().last().unwrap(), "brew install --cask codex");
}

#[test]
fn a_fresh_mac_lists_git_and_homebrew_first() {
    let (mut app, copied) = app(fake(&[]));
    settle(&mut app);
    let s = screen(&mut app, 110, 60);
    let at = |t: &str| s.find(t).unwrap_or_else(|| panic!("{t} missing:\n{s}"));
    assert!(at("✕ Git") < at("✕ Homebrew") && at("✕ Homebrew") < at("✕ GitHub CLI"));
    assert!(s.contains("xcode-select --install"), "{s}");
    assert!(s.contains("Needs Homebrew. Install Homebrew above, then run:"), "{s}");
    key(&mut app, KeyCode::Char('c'));
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('c'));
    assert_eq!(*copied.borrow(), ["xcode-select --install", model::BREW_INSTALL]);
}

#[test]
fn skip_asks_first_listing_only_whats_missing() {
    let (mut app, _) = app(fake(&[GIT, BREW, CLAUDE]));
    settle(&mut app);
    key(&mut app, KeyCode::Char('s'));
    assert!(app.skip_dialog_open());
    let s = screen(&mut app, 100, 40);
    assert!(s.contains(model::SKIP_TITLE_ONE), "{s}");
    assert!(s.contains("Shelbi can't open pull requests"), "{s}");
    assert!(!s.contains("no task can start"), "only the missing tools:\n{s}");
    assert_eq!(key(&mut app, KeyCode::Esc), None);
    assert!(!app.skip_dialog_open(), "Go back");
    key(&mut app, KeyCode::Char('s'));
    assert_eq!(key(&mut app, KeyCode::Char('y')), Some(PrereqExit::Skip));
}

#[test]
fn check_again_reruns_and_ready_shows_continue() {
    let probe = fake(&[GIT, BREW, GH]);
    let (mut app, _) = app(probe);
    settle(&mut app);
    assert_eq!(app.model().missing(), [Tool::Agent]);
    // Install the agent, check again.
    let installed = fake(&[GIT, BREW, GH, CLAUDE]);
    app.probe = installed;
    key(&mut app, KeyCode::Char('r'));
    assert_eq!(app.model().phase(), Phase::Checking);
    assert_eq!(settle(&mut app), None, "Check again after a miss waits for Continue");
    let s = screen(&mut app, 100, 40);
    assert!(s.contains(model::READY_TITLE) && s.contains("Continue"), "{s}");
    assert_eq!(key(&mut app, KeyCode::Enter), Some(PrereqExit::Continue));
}

#[test]
fn locate_validates_the_path_and_shows_the_version() {
    let probe = Arc::new(Fake {
        dirs: vec!["/usr/bin".into(), "/opt/homebrew/bin".into()],
        exes: [GIT, BREW, CLAUDE, ("/Users/me/tools/gh", "gh version 2.62.0")]
            .iter()
            .map(|(p, v)| (PathBuf::from(p), v.to_string()))
            .collect(),
    });
    let home = std::env::temp_dir().join(format!("shelbi-prereqs-tui-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let _env = crate::test_support::ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let saved = std::env::var_os("SHELBI_HOME");
    std::env::set_var("SHELBI_HOME", &home);
    let (mut app, _) = app(probe);
    settle(&mut app);
    key(&mut app, KeyCode::Down);
    key(&mut app, KeyCode::Char('l'));
    assert_eq!(app.model().locating(), Some(Tool::GithubCli));
    for c in "/nope".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    assert!(screen(&mut app, 100, 40).contains("/nope isn't an executable file."));
    for _ in 0..5 {
        key(&mut app, KeyCode::Backspace);
    }
    for c in "~/tools/gh".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    key(&mut app, KeyCode::Enter);
    settle(&mut app);
    let s = screen(&mut app, 100, 40);
    assert!(s.contains("✓ gh version 2.62.0"), "{s}");
    assert_eq!(app.model().phase(), Phase::Ready);
    // Remembered for the next launch.
    assert_eq!(shelbi_state::tool_paths::located().gh, Some(PathBuf::from("/Users/me/tools/gh")));
    assert_eq!(key(&mut app, KeyCode::Enter), Some(PrereqExit::Continue));
    match saved {
        Some(saved) => std::env::set_var("SHELBI_HOME", saved),
        None => std::env::remove_var("SHELBI_HOME"),
    }
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_short_terminal_scrolls_with_a_scrollbar() {
    let (mut app, _) = app(fake(&[]));
    settle(&mut app);
    let s = screen(&mut app, 80, 20);
    assert!(s.contains('┃'), "a scrollbar:\n{s}");
    // Moving down to the last stop keeps it in view.
    for _ in 0..10 {
        key(&mut app, KeyCode::Down);
    }
    let s = screen(&mut app, 80, 20);
    assert!(s.contains("brew install --cask codex"), "{s}");
    assert!(!s.contains("xcode-select"), "scrolled past the top:\n{s}");
}

#[test]
fn esc_quits() {
    let (mut app, _) = app(fake(&[GIT]));
    settle(&mut app);
    assert_eq!(key(&mut app, KeyCode::Esc), Some(PrereqExit::Quit));
}
