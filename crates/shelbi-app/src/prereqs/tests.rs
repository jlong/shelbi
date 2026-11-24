use std::collections::{BTreeMap, BTreeSet};
use std::process::Command;
use std::sync::Mutex;

use super::*;

/// A machine made of a set of executables, `--version` answers and whether
/// the Command Line Tools are installed. Records each `--version` it runs.
#[derive(Default)]
struct Fake {
    dirs: Vec<PathBuf>,
    exes: BTreeSet<PathBuf>,
    versions: BTreeMap<PathBuf, Result<String, String>>,
    clt: bool,
    ran: Mutex<Vec<PathBuf>>,
}

impl Fake {
    fn new(dirs: &[&str]) -> Self {
        Fake {
            dirs: dirs.iter().map(PathBuf::from).collect(),
            clt: true,
            ..Fake::default()
        }
    }

    fn exe(mut self, path: &str, version: &str) -> Self {
        self.exes.insert(path.into());
        self.versions.insert(path.into(), Ok(version.to_string()));
        self
    }

    fn broken(mut self, path: &str, why: &str) -> Self {
        self.exes.insert(path.into());
        self.versions.insert(path.into(), Err(why.to_string()));
        self
    }

    fn ran(&self) -> Vec<PathBuf> {
        self.ran.lock().unwrap().clone()
    }
}

impl Probe for Fake {
    fn path_dirs(&self) -> Vec<PathBuf> {
        self.dirs.clone()
    }
    fn is_executable(&self, path: &Path) -> bool {
        self.exes.contains(path)
    }
    fn version(&self, path: &Path) -> Result<String, String> {
        self.ran.lock().unwrap().push(path.to_path_buf());
        self.versions.get(path).cloned().unwrap_or_else(|| Err("no such file".into()))
    }
    fn command_line_tools(&self) -> bool {
        self.clt
    }
}

/// Everything installed, the way a Homebrew Mac has it.
fn full_mac() -> Fake {
    Fake::new(&["/usr/bin", "/opt/homebrew/bin", "/Users/me/.local/bin"])
        .exe("/usr/bin/git", "git version 2.39.5 (Apple Git-154)")
        .exe("/opt/homebrew/bin/gh", "gh version 2.62.0 (2024-11-14)\nhttps://github.com/cli/cli/releases/tag/v2.62.0")
        .exe("/Users/me/.local/bin/claude", "2.1.4 (Claude Code)")
        .exe("/opt/homebrew/bin/brew", "Homebrew 4.4.6")
}

fn checked(platform: Platform, probe: &dyn Probe, located: &ToolPaths) -> Prereqs {
    let mut p = Prereqs::new(platform, Client::Desktop);
    let run = p.begin_check();
    run_check(run, located, platform, probe, |e| {
        p.apply(e);
    });
    p
}

fn found(p: &Prereqs, tool: Tool) -> Found {
    match p.state(tool) {
        RowState::Found(f) => f.clone(),
        other => panic!("{tool:?} is {other:?}"),
    }
}

#[test]
fn everything_installed_is_ready_with_versions() {
    let p = checked(Platform::Mac, &full_mac(), &ToolPaths::default());
    assert_eq!(p.phase(), Phase::Ready);
    assert!(p.all_found());
    assert!(p.missing().is_empty());
    assert_eq!(p.title(), READY_TITLE);
    assert_eq!(p.body(), READY_BODY_MAC);
    let home = Path::new("/Users/me");
    assert_eq!(found(&p, Tool::Git).detail(Some(home)), "2.39.5 · /usr/bin/git");
    assert_eq!(found(&p, Tool::GithubCli).detail(Some(home)), "2.62.0 · /opt/homebrew/bin/gh");
    assert_eq!(found(&p, Tool::Agent).detail(Some(home)), "2.1.4 · ~/.local/bin/claude");
    assert_eq!(found(&p, Tool::Homebrew).detail(Some(home)), "4.4.6 · /opt/homebrew/bin/brew");
    assert_eq!(p.tools(), [Tool::Git, Tool::GithubCli, Tool::Agent, Tool::Homebrew]);
}

#[test]
fn a_failed_version_shows_the_path_alone() {
    let probe = full_mac().broken("/opt/homebrew/bin/gh", "boom");
    let p = checked(Platform::Mac, &probe, &ToolPaths::default());
    let gh = found(&p, Tool::GithubCli);
    assert_eq!(gh.version, None);
    assert_eq!(gh.detail(None), "/opt/homebrew/bin/gh");
}

#[test]
fn either_agent_satisfies_the_coding_agent_row() {
    let codex_only = Fake::new(&["/opt/homebrew/bin"]).exe("/opt/homebrew/bin/codex", "codex-cli 0.46.0");
    let state = check_tool(Tool::Agent, &ToolPaths::default(), Platform::Mac, &codex_only, true);
    let RowState::Found(f) = state else { panic!("{state:?}") };
    assert_eq!((f.binary, f.version.as_deref()), (Binary::Codex, Some("0.46.0")));

    let claude_only = Fake::new(&["/x"]).exe("/x/claude", "2.1.4 (Claude Code)");
    let state = check_tool(Tool::Agent, &ToolPaths::default(), Platform::Mac, &claude_only, true);
    assert!(matches!(state, RowState::Found(Found { binary: Binary::Claude, .. })));

    let neither = Fake::new(&["/x"]);
    assert_eq!(check_tool(Tool::Agent, &ToolPaths::default(), Platform::Mac, &neither, true), RowState::Missing);
}

#[test]
fn missing_rows_are_listed_in_display_order_with_consequences() {
    let probe = Fake::new(&["/usr/bin", "/opt/homebrew/bin"])
        .exe("/usr/bin/git", "git version 2.39.5")
        .exe("/opt/homebrew/bin/brew", "Homebrew 4.4.6");
    let p = checked(Platform::Mac, &probe, &ToolPaths::default());
    assert_eq!(p.phase(), Phase::Missing);
    assert_eq!(p.missing(), [Tool::GithubCli, Tool::Agent]);
    assert_eq!((p.title(), p.body()), (MISSING_TITLE, MISSING_BODY_MAC));
    assert_eq!(
        p.consequences(),
        [
            (Tool::GithubCli, "Shelbi can't open pull requests, sync GitHub issues, or follow CI."),
            (Tool::Agent, "Workspaces have no agent to run, so no task can start."),
        ]
    );
    assert_eq!(p.skip_copy().0, SKIP_TITLE);
    // Nothing a tool depends on is missing: Homebrew stays last.
    assert_eq!(p.tools(), [Tool::Git, Tool::GithubCli, Tool::Agent, Tool::Homebrew]);
    assert_eq!(p.missing_detail(Tool::GithubCli), "Not found on your PATH. Install it with Homebrew, then check again.");
}

#[test]
fn one_missing_tool_reads_singular() {
    let probe = full_mac();
    let mut p = Prereqs::new(Platform::Mac, Client::Desktop);
    let run = p.begin_check();
    run_check(run, &ToolPaths::default(), Platform::Mac, &probe, |mut e| {
        if e.tool == Tool::GithubCli && e.state != RowState::Checking {
            e.state = RowState::Missing;
        }
        p.apply(e);
    });
    assert_eq!((p.title(), p.body()), (MISSING_ONE_TITLE, MISSING_ONE_BODY_MAC));
    assert_eq!(p.skip_copy(), (SKIP_TITLE_ONE, SKIP_BODY_ONE, SKIP_FOOTER_ONE));
}

#[test]
fn a_fresh_mac_lists_rows_in_dependency_order() {
    // No Command Line Tools: /usr/bin/git is the installer's stub.
    let mut probe = Fake::new(&["/usr/bin"]).exe("/usr/bin/git", "git version 2.39.5");
    probe.clt = false;
    let p = checked(Platform::Mac, &probe, &ToolPaths::default());
    assert_eq!(p.missing(), [Tool::Git, Tool::Homebrew, Tool::GithubCli, Tool::Agent]);
    assert_eq!(p.tools(), [Tool::Git, Tool::Homebrew, Tool::GithubCli, Tool::Agent]);
    assert!(probe.ran().is_empty(), "the stub is never run (it opens the installer)");
    assert_eq!(p.install_commands(Tool::Git)[0].text, "xcode-select --install");
    assert_eq!(p.install_commands(Tool::Homebrew)[0].text, BREW_INSTALL);
    assert_eq!(
        BREW_INSTALL,
        r#"/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)""#
    );
    assert_eq!(p.missing_detail(Tool::GithubCli), "Needs Homebrew. Install Homebrew above, then run:");
}

#[test]
fn only_missing_homebrew_also_moves_it_up() {
    let probe = Fake::new(&["/usr/bin", "/x"])
        .exe("/usr/bin/git", "git version 2.39.5")
        .exe("/x/gh", "gh version 2.62.0")
        .exe("/x/claude", "2.1.4");
    let p = checked(Platform::Mac, &probe, &ToolPaths::default());
    assert_eq!(p.missing(), [Tool::Homebrew]);
    assert_eq!(p.tools(), [Tool::Git, Tool::Homebrew, Tool::GithubCli, Tool::Agent]);
}

#[test]
fn a_real_git_later_on_the_path_wins_over_the_stub() {
    let mut probe = Fake::new(&["/usr/bin", "/opt/homebrew/bin"])
        .exe("/usr/bin/git", "x")
        .exe("/opt/homebrew/bin/git", "git version 2.47.0");
    probe.clt = false;
    let state = check_tool(Tool::Git, &ToolPaths::default(), Platform::Mac, &probe, true);
    let RowState::Found(f) = state else { panic!() };
    assert_eq!(f.path, PathBuf::from("/opt/homebrew/bin/git"));
    // Linux has no stub.
    let state = check_tool(Tool::Git, &ToolPaths::default(), Platform::Linux, &probe, false);
    assert!(matches!(state, RowState::Found(Found { ref path, .. }) if path == Path::new("/usr/bin/git")));
}

#[test]
fn linux_has_no_homebrew_row_and_its_own_hints() {
    let probe = Fake::new(&["/usr/bin"]).exe("/usr/bin/git", "git version 2.43.0");
    let p = checked(Platform::Linux, &probe, &ToolPaths::default());
    assert_eq!(p.tools(), [Tool::Git, Tool::GithubCli, Tool::Agent]);
    assert_eq!(p.missing(), [Tool::GithubCli, Tool::Agent]);
    assert!(p.install_commands(Tool::Git).is_empty());
    assert_eq!(p.install_commands(Tool::GithubCli)[0].text, "https://github.com/cli/cli#installation");
    let agents: Vec<_> = p.install_commands(Tool::Agent).into_iter().map(|c| (c.label, c.text)).collect();
    assert_eq!(
        agents,
        [
            (Some("Claude Code"), "npm install -g @anthropic-ai/claude-code".to_string()),
            (Some("Codex"), "npm install -g @openai/codex".to_string()),
            (Some("OpenCode"), "npm install -g opencode-ai".to_string()),
        ]
    );
    let ready = checked(
        Platform::Linux,
        &Fake::new(&["/b"]).exe("/b/git", "1.0").exe("/b/gh", "1.0").exe("/b/codex", "1.0"),
        &ToolPaths::default(),
    );
    assert_eq!((ready.phase(), ready.body()), (Phase::Ready, READY_BODY_LINUX));
}

#[test]
fn mac_agent_commands_offer_both() {
    let cmds = Tool::Agent.install_commands(Platform::Mac);
    assert_eq!(cmds[0].text, "brew install --cask claude-code");
    assert_eq!(cmds[1].text, "brew install --cask codex");
    assert_eq!(Tool::GithubCli.install_commands(Platform::Mac)[0].text, "brew install gh");
}

#[test]
fn checks_report_each_row_starting_then_finishing() {
    let mut events = Vec::new();
    run_check(7, &ToolPaths::default(), Platform::Mac, &full_mac(), |e| events.push((e.tool, matches!(e.state, RowState::Checking))));
    assert_eq!(
        events,
        [
            (Tool::Git, true),
            (Tool::Git, false),
            (Tool::GithubCli, true),
            (Tool::GithubCli, false),
            (Tool::Agent, true),
            (Tool::Agent, false),
            (Tool::Homebrew, true),
            (Tool::Homebrew, false),
        ]
    );
}

#[test]
fn a_new_run_drops_the_old_runs_stragglers() {
    let mut p = Prereqs::new(Platform::Mac, Client::Desktop);
    let old = p.begin_check();
    assert_eq!(p.phase(), Phase::Checking);
    let new = p.begin_check();
    assert!(!p.apply(CheckEvent { run: old, tool: Tool::Git, state: RowState::Missing }));
    assert_eq!(p.state(Tool::Git), &RowState::Pending);
    assert!(p.apply(CheckEvent { run: new, tool: Tool::Git, state: RowState::Checking }));
    assert_eq!(p.state(Tool::Git), &RowState::Checking);
}

#[test]
fn the_quick_check_runs_nothing() {
    let probe = full_mac();
    assert!(quick_check(&ToolPaths::default(), Platform::Mac, &probe));
    assert!(probe.ran().is_empty());
    let mut missing = Fake::new(&["/usr/bin"]).exe("/usr/bin/git", "x");
    missing.clt = true;
    assert!(!quick_check(&ToolPaths::default(), Platform::Mac, &missing));
}

#[test]
fn version_parsing() {
    for (out, want) in [
        ("git version 2.39.5 (Apple Git-154)", Some("2.39.5")),
        ("gh version 2.62.0 (2024-11-14)", Some("2.62.0")),
        ("2.1.4 (Claude Code)", Some("2.1.4")),
        ("codex-cli 0.46.0", Some("0.46.0")),
        ("Homebrew 4.4.6\nHomebrew/homebrew-core (git revision 1)", Some("4.4.6")),
        ("tool v1.2.3-beta.1", Some("1.2.3-beta.1")),
        ("version: (1.10)", Some("1.10")),
        ("no digits here", None),
        ("build 42", None),
        ("", None),
    ] {
        assert_eq!(parse_version(out).as_deref(), want, "{out:?}");
    }
}

#[test]
fn a_located_binary_counts_and_is_preferred() {
    let probe = Fake::new(&["/opt/homebrew/bin"])
        .exe("/opt/homebrew/bin/gh", "gh version 2.0.0")
        .exe("/Users/me/tools/gh", "gh version 2.62.0");
    let mut located = ToolPaths::default();
    located.set("gh", Some("/Users/me/tools/gh".into()));
    let state = check_tool(Tool::GithubCli, &located, Platform::Mac, &probe, true);
    let RowState::Found(f) = state else { panic!() };
    assert_eq!(f.path, PathBuf::from("/Users/me/tools/gh"));
    assert!(f.located);
    // A located path that's gone falls back to the PATH.
    located.set("gh", Some("/gone/gh".into()));
    let RowState::Found(f) = check_tool(Tool::GithubCli, &located, Platform::Mac, &probe, false) else { panic!() };
    assert_eq!((f.path, f.located), (PathBuf::from("/opt/homebrew/bin/gh"), false));
    // With nothing on the PATH, the located one alone satisfies the check.
    let only = Fake::new(&[]).exe("/Users/me/tools/claude", "2.1.4");
    let mut located = ToolPaths::default();
    located.set("claude", Some("/Users/me/tools/claude".into()));
    assert!(matches!(
        check_tool(Tool::Agent, &located, Platform::Mac, &only, false),
        RowState::Found(Found { located: true, .. })
    ));
}

#[test]
fn validating_a_located_path() {
    let home = Path::new("/Users/me");
    let probe = Fake::new(&[])
        .exe("/Users/me/.local/bin/gh", "gh version 2.62.0")
        .broken("/Users/me/bad/gh", "dyld: Library not loaded")
        .exe("/opt/codex-1/codex-aarch64", "codex-cli 0.46.0");
    let ok = validate_located(Tool::GithubCli, "~/.local/bin/gh", Some(home), Platform::Mac, &probe).unwrap();
    assert_eq!(ok.path, PathBuf::from("/Users/me/.local/bin/gh"));
    assert_eq!(ok.located_message(), "gh version 2.62.0");
    assert!(ok.located);

    let err = |input: &str| validate_located(Tool::GithubCli, input, Some(home), Platform::Mac, &probe).unwrap_err();
    assert_eq!(err("  "), "Enter the path to the gh executable.");
    assert_eq!(err("bin/gh"), "Enter a full path, starting with / or ~.");
    assert_eq!(err("/nope/gh"), "/nope/gh isn't an executable file.");
    assert_eq!(err("~/bad/gh"), "`~/bad/gh --version` failed: dyld: Library not loaded");

    // The agent row tells Codex from Claude Code by the file's name.
    let codex = validate_located(Tool::Agent, "/opt/codex-1/codex-aarch64", Some(home), Platform::Mac, &probe).unwrap();
    assert_eq!((codex.binary, codex.located_message()), (Binary::Codex, "codex version 0.46.0".to_string()));
}

#[test]
fn locating_marks_the_row_found_and_keeps_the_panel() {
    let mut p = checked(Platform::Mac, &Fake::new(&["/usr/bin"]).exe("/usr/bin/git", "2.0"), &ToolPaths::default());
    assert_eq!(p.state(Tool::GithubCli), &RowState::Missing);
    p.start_locate(Tool::GithubCli);
    assert_eq!(p.locating(), Some(Tool::GithubCli));
    p.located(Tool::GithubCli, Err("nope".into()));
    assert_eq!(p.locate_result(), Some(&Err("nope".to_string())));
    assert_eq!(p.state(Tool::GithubCli), &RowState::Missing);
    let gh = Found { binary: Binary::Gh, path: "/x/gh".into(), version: Some("2.62.0".into()), located: true };
    p.located(Tool::GithubCli, Ok(gh.clone()));
    assert_eq!(p.state(Tool::GithubCli), &RowState::Found(gh));
    assert_eq!(p.locating(), Some(Tool::GithubCli));
    // A late answer for a panel that moved is dropped.
    p.start_locate(Tool::Agent);
    p.located(Tool::GithubCli, Err("late".into()));
    assert_eq!(p.locate_result(), None);
    // Check again closes it.
    p.begin_check();
    assert_eq!(p.locating(), None);
}

#[test]
fn paths_show_home_as_tilde() {
    let home = Some(Path::new("/Users/me"));
    assert_eq!(display_path(Path::new("/Users/me/.local/bin/claude"), home), "~/.local/bin/claude");
    assert_eq!(display_path(Path::new("/Users/meg/x"), home), "/Users/meg/x");
    assert_eq!(display_path(Path::new("/usr/bin/git"), None), "/usr/bin/git");
    assert_eq!(expand_tilde(Path::new("~/x"), home), PathBuf::from("/Users/me/x"));
}

#[test]
fn the_missing_line_says_where_to_run_the_commands() {
    // The Mac desktop app names the Terminal app; nothing else claims one.
    assert_eq!(missing_body(Platform::Mac, Client::Desktop, 2), "Open the Terminal app, run the commands below, then check again.");
    assert_eq!(missing_body(Platform::Mac, Client::Desktop, 1), "Open the Terminal app, run the command below, then check again.");
    assert_eq!(missing_body(Platform::Mac, Client::Tui, 2), "Run the commands below in another terminal, then check again.");
    assert_eq!(missing_body(Platform::Linux, Client::Desktop, 1), "Install it from a terminal as shown below, then check again.");
    assert_eq!(missing_body(Platform::Linux, Client::Tui, 3), "Install them from another terminal as shown below, then check again.");
    for platform in [Platform::Mac, Platform::Linux] {
        for client in [Client::Desktop, Client::Tui] {
            for count in [1, 2] {
                let body = missing_body(platform, client, count);
                let app = platform == Platform::Mac && client == Client::Desktop;
                assert_eq!(body.contains("Terminal app"), app, "{body}");
                assert!(body.contains("erminal"), "every line says where: {body}");
                assert!(!body.contains('—'), "{body}");
            }
        }
    }
    // The rows never repeat where (the line above them says it once).
    for platform in [Platform::Mac, Platform::Linux] {
        for tool in Tool::CHECK_ORDER {
            for brew_missing in [false, true] {
                let detail = tool.missing_detail(platform, brew_missing);
                assert!(!detail.to_lowercase().contains("terminal"), "{detail}");
            }
        }
    }
    assert_eq!(
        Tool::Homebrew.missing_detail(Platform::Mac, false),
        "Shelbi installs the other tools with Homebrew. Install Git first, then run:"
    );
}

#[test]
fn the_tui_line_points_at_another_terminal() {
    let mut p = Prereqs::new(Platform::Mac, Client::Tui);
    let run = p.begin_check();
    run_check(run, &ToolPaths::default(), Platform::Mac, &full_mac(), |mut e| {
        if e.tool == Tool::GithubCli && e.state != RowState::Checking {
            e.state = RowState::Missing;
        }
        p.apply(e);
    });
    assert_eq!(p.body(), MISSING_ONE_BODY_TUI_MAC);
}

#[test]
fn locate_copy() {
    // Every row's button reads Browse…, the agent's too: the panel names
    // the executable.
    for tool in Tool::CHECK_ORDER {
        assert_eq!(tool.locate_label(), "Browse…");
    }
    assert_eq!(Tool::GithubCli.locate_body(), "Choose the gh executable. Shelbi remembers this location.");
}

#[test]
fn the_bounded_runner_reads_output_and_failures() {
    let mut ok = Command::new("/bin/sh");
    ok.args(["-c", "echo 'tool 1.2.3'"]);
    assert_eq!(run_bounded_for_test(ok, Duration::from_secs(5)).unwrap(), "tool 1.2.3");
    let mut stderr_only = Command::new("/bin/sh");
    stderr_only.args(["-c", "echo 'v 9.9' >&2"]);
    assert_eq!(run_bounded_for_test(stderr_only, Duration::from_secs(5)).unwrap(), "v 9.9");
    let mut fails = Command::new("/bin/sh");
    fails.args(["-c", "echo 'bad thing' >&2; exit 3"]);
    assert_eq!(run_bounded_for_test(fails, Duration::from_secs(5)).unwrap_err(), "bad thing");
    let mut quiet = Command::new("/bin/sh");
    quiet.args(["-c", "exit 4"]);
    assert_eq!(run_bounded_for_test(quiet, Duration::from_secs(5)).unwrap_err(), "exited with status 4");
    let mut slow = Command::new("/bin/sh");
    slow.args(["-c", "sleep 5"]);
    assert!(run_bounded_for_test(slow, Duration::from_millis(100)).unwrap_err().starts_with("no answer"));
}
