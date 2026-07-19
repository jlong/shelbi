use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use shelbi_core::Project;
use tempfile::TempDir;

const PROCESS_DEADLINE: Duration = Duration::from_secs(10);

struct CompletedProcess {
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Copy)]
struct FakePath {
    git: bool,
    claude: bool,
    codex: bool,
    tmux: bool,
}

impl FakePath {
    fn one_runner() -> Self {
        Self {
            git: true,
            claude: true,
            codex: false,
            tmux: true,
        }
    }
}

fn real_program(name: &str) -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").expect("test PATH"))
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .unwrap_or_else(|| panic!("{name} must be installed to run this test"))
}

fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    let mut permissions = fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).unwrap();
}

fn fake_path(root: &Path, tools: FakePath) -> PathBuf {
    let bin = root.join("bin");
    fs::create_dir_all(&bin).unwrap();
    if tools.git {
        symlink(real_program("git"), bin.join("git")).unwrap();
    }
    if tools.claude {
        executable(&bin.join("claude"), "#!/bin/sh\nprintf 'claude 2.1.0\\n'\n");
    }
    if tools.codex {
        executable(&bin.join("codex"), "#!/bin/sh\nprintf 'codex 0.101.0\\n'\n");
    }
    if tools.tmux {
        executable(&bin.join("tmux"), "#!/bin/sh\nprintf 'tmux 3.5a\\n'\n");
    }
    bin
}

fn git_init(repo: &Path) {
    fs::create_dir_all(repo).unwrap();
    let status = Command::new(real_program("git"))
        .args(["init", "-q", "-b", "main"])
        .current_dir(repo)
        .status()
        .unwrap();
    assert!(status.success());
}

/// Spawn with an open stdin pipe and keep the parent writer alive until the
/// process exits. A prompt or terminal read therefore blocks and hits this
/// deadline instead of receiving EOF and looking like a clean noninteractive
/// failure.
fn run_init(cwd: &Path, home: &Path, path: &Path, args: &[&str]) -> CompletedProcess {
    let mut command = init_command(cwd, home, path, args);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let open_stdin = child.stdin.take().expect("piped stdin");
    wait_for_init(child, open_stdin)
}

/// Attach stdin to a real PTY and leave its master open without
/// sending input. This catches prompts hidden behind `stdin.is_terminal()`.
fn run_init_with_tty(cwd: &Path, home: &Path, path: &Path, args: &[&str]) -> CompletedProcess {
    let mut master_fd = -1;
    let mut slave_fd = -1;
    let result = unsafe {
        libc::openpty(
            &mut master_fd,
            &mut slave_fd,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(
        result,
        0,
        "openpty failed: {}",
        std::io::Error::last_os_error()
    );

    let master = unsafe { File::from_raw_fd(master_fd) };
    let slave = unsafe { File::from_raw_fd(slave_fd) };
    assert_eq!(unsafe { libc::isatty(slave.as_raw_fd()) }, 1);

    let mut command = init_command(cwd, home, path, args);
    let child = command
        .stdin(Stdio::from(slave))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    wait_for_init(child, master)
}

fn init_command(cwd: &Path, home: &Path, path: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_shelbi"));
    command
        .args(args)
        .current_dir(cwd)
        .env("SHELBI_HOME", home)
        .env("HOME", home.parent().unwrap_or(home))
        .env("PATH", path)
        .env_remove("SHELBI_ROOT")
        .env_remove("SHELBI_PROJECT")
        .stdin(Stdio::null());
    command
}

fn wait_for_init<T>(mut child: Child, input_guard: T) -> CompletedProcess {
    let deadline = Instant::now() + PROCESS_DEADLINE;

    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            drop(input_guard);
            let _ = child.wait();
            let mut stdout = String::new();
            let mut stderr = String::new();
            child
                .stdout
                .take()
                .unwrap()
                .read_to_string(&mut stdout)
                .unwrap();
            child
                .stderr
                .take()
                .unwrap()
                .read_to_string(&mut stderr)
                .unwrap();
            panic!(
                "shelbi did not exit while stdin remained open\nstdout:\n{stdout}\nstderr:\n{stderr}"
            );
        }
        thread::sleep(Duration::from_millis(20));
    };

    drop(input_guard);
    let mut stdout = String::new();
    let mut stderr = String::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut stdout)
        .unwrap();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    CompletedProcess {
        status,
        stdout,
        stderr,
    }
}

fn load_project(home: &Path, name: &str) -> Project {
    // The id is the config filename stem now, not a YAML key — match on the
    // file `<name>.yaml` and stamp the id the way the real loader does.
    snapshot_files(home)
        .into_iter()
        .filter(|(path, _)| path.file_stem().and_then(|s| s.to_str()) == Some(name))
        .find_map(|(_, contents)| {
            serde_yaml::from_slice::<Project>(&contents).ok().map(|mut project| {
                project.name = name.to_string();
                project
            })
        })
        .unwrap_or_else(|| panic!("project registration for {name} was not generated"))
}

fn snapshot_files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(base: &Path, path: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(path)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect::<Vec<_>>();
        entries.sort();
        for entry in entries {
            if entry.is_dir() {
                visit(base, &entry, files);
            } else {
                files.insert(
                    entry.strip_prefix(base).unwrap().to_path_buf(),
                    fs::read(&entry).unwrap(),
                );
            }
        }
    }

    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

#[test]
fn yes_mode_with_one_runner_initializes_git_while_stdin_is_open() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("demo");
    let home = temp.path().join("home");
    fs::create_dir_all(&repo).unwrap();
    let bin = fake_path(&temp.path().join("path"), FakePath::one_runner());

    let result = run_init(
        &repo,
        &home,
        &bin,
        &["init", "-y", "--default-branch", "develop"],
    );
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr
    );
    assert!(result.stdout.contains("Project demo created"));
    assert!(repo.join(".git").is_dir());
    let project = load_project(&home, "demo");
    assert_eq!(project.default_branch, "develop");
    assert_eq!(project.orchestrator.runner, "claude");
    // The pool is provisioned by the orchestrator's first-boot interview.
    assert!(project.workspaces.is_empty());
    let branch = Command::new(real_program("git"))
        .args(["branch", "--show-current"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert!(branch.status.success());
    assert_eq!(String::from_utf8(branch.stdout).unwrap().trim(), "develop");
}

#[test]
fn yes_mode_with_one_runner_never_reads_from_a_tty() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("tty-demo");
    let home = temp.path().join("home");
    git_init(&repo);
    let bin = fake_path(&temp.path().join("path"), FakePath::one_runner());

    let result = run_init_with_tty(&repo, &home, &bin, &["init", "-y"]);
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr
    );
    assert!(result.stdout.contains("Project tty-demo created"));
    assert_eq!(
        load_project(&home, "tty-demo").orchestrator.runner,
        "claude"
    );
}

#[test]
fn root_before_init_selects_state_while_project_defaults_to_cwd() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("project");
    let env_home = temp.path().join("state-from-env");
    let flag_home = temp.path().join("state-from-flag");
    git_init(&repo);
    let bin = fake_path(&temp.path().join("path"), FakePath::one_runner());
    let flag_home_arg = flag_home.to_str().unwrap();

    let result = run_init(
        &repo,
        &env_home,
        &bin,
        &["--root", flag_home_arg, "init", "-y"],
    );
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr
    );
    let project = load_project(&flag_home, "project");
    assert_eq!(
        project.repo,
        fs::canonicalize(&repo).unwrap().display().to_string()
    );
    assert!(
        !env_home.exists(),
        "the global --root flag must beat SHELBI_HOME"
    );
}

#[test]
fn ambiguous_runners_fail_without_state_until_runner_flag_resolves_them() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("ambiguous");
    git_init(&repo);
    let bin = fake_path(
        &temp.path().join("path"),
        FakePath {
            git: true,
            claude: true,
            codex: true,
            tmux: true,
        },
    );

    let ambiguous_home = temp.path().join("ambiguous-home");
    let ambiguous = run_init(&repo, &ambiguous_home, &bin, &["init", "-y"]);
    assert!(!ambiguous.status.success());
    assert!(ambiguous.stderr.contains("both claude and codex"));
    assert!(ambiguous.stderr.contains("--runner claude"));
    assert!(!ambiguous_home.exists(), "ambiguity must be write-free");

    let resolved_home = temp.path().join("resolved-home");
    let resolved = run_init(
        &repo,
        &resolved_home,
        &bin,
        &["init", "-y", "--runner", "codex"],
    );
    assert!(
        resolved.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        resolved.stdout,
        resolved.stderr
    );
    let project = load_project(&resolved_home, "ambiguous");
    assert_eq!(project.orchestrator.runner, "codex");
    // The pool is provisioned by the orchestrator's first-boot interview.
    assert!(project.workspaces.is_empty());
}

#[test]
fn missing_prerequisites_do_not_initialize_git_or_scaffold_state() {
    let temp = TempDir::new().unwrap();

    let no_runner_repo = temp.path().join("no-runner");
    fs::create_dir_all(&no_runner_repo).unwrap();
    let no_runner_home = temp.path().join("no-runner-home");
    let no_runner_bin = fake_path(
        &temp.path().join("no-runner-path"),
        FakePath {
            git: true,
            claude: false,
            codex: false,
            tmux: true,
        },
    );
    let no_runner = run_init(
        &no_runner_repo,
        &no_runner_home,
        &no_runner_bin,
        &["init", "-y"],
    );
    assert!(!no_runner.status.success());
    assert!(no_runner.stderr.contains("No supported agent runner"));
    assert!(!no_runner_home.exists());
    assert!(!no_runner_repo.join(".git").exists());

    let no_tmux_repo = temp.path().join("no-tmux");
    fs::create_dir_all(&no_tmux_repo).unwrap();
    let no_tmux_home = temp.path().join("no-tmux-home");
    let no_tmux_bin = fake_path(
        &temp.path().join("no-tmux-path"),
        FakePath {
            git: true,
            claude: true,
            codex: false,
            tmux: false,
        },
    );
    let no_tmux = run_init(&no_tmux_repo, &no_tmux_home, &no_tmux_bin, &["init", "-y"]);
    assert!(!no_tmux.status.success());
    assert!(no_tmux.stderr.contains("tmux was not found"));
    assert!(!no_tmux_home.exists());
    assert!(!no_tmux_repo.join(".git").exists());

    let no_git_repo = temp.path().join("no-git");
    fs::create_dir_all(&no_git_repo).unwrap();
    let no_git_home = temp.path().join("no-git-home");
    let no_git_bin = fake_path(
        &temp.path().join("no-git-path"),
        FakePath {
            git: false,
            claude: true,
            codex: false,
            tmux: true,
        },
    );
    let no_git = run_init(&no_git_repo, &no_git_home, &no_git_bin, &["init", "-y"]);
    assert!(!no_git.status.success());
    assert!(no_git.stderr.contains("Git was not found on PATH"));
    assert!(!no_git_home.exists());
    assert!(!no_git_repo.join(".git").exists());
}

#[test]
fn invalid_state_root_fails_before_deferred_git_initialization() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let state_root = temp.path().join("state-is-a-file");
    fs::write(&state_root, "keep me\n").unwrap();
    let bin = fake_path(&temp.path().join("path"), FakePath::one_runner());

    let result = run_init(&repo, &state_root, &bin, &["init", "-y"]);
    assert!(!result.status.success());
    assert!(
        result.stderr.contains("not a directory"),
        "stderr: {}",
        result.stderr
    );
    assert_eq!(fs::read_to_string(&state_root).unwrap(), "keep me\n");
    assert!(!repo.join(".git").exists());
}

#[test]
fn blocked_scaffold_directory_fails_before_git_or_state_writes() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("blocked");
    let home = temp.path().join("home");
    fs::create_dir_all(&repo).unwrap();
    let project_dir = home.join("projects/blocked");
    fs::create_dir_all(&project_dir).unwrap();
    fs::write(project_dir.join("agents"), "keep me\n").unwrap();
    let before = snapshot_files(&home);
    let bin = fake_path(&temp.path().join("path"), FakePath::one_runner());

    let result = run_init(&repo, &home, &bin, &["init", "-y"]);
    assert!(!result.status.success());
    assert!(
        result.stderr.contains("agents") && result.stderr.contains("not a directory"),
        "stderr: {}",
        result.stderr
    );
    assert_eq!(snapshot_files(&home), before);
    assert!(!repo.join(".git").exists());
}

#[test]
fn all_explicit_plan_flags_override_detected_defaults() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("detected-name");
    let home = temp.path().join("home");
    git_init(&repo);
    let bin = fake_path(
        &temp.path().join("path"),
        FakePath {
            git: true,
            claude: true,
            codex: true,
            tmux: true,
        },
    );
    let repo_arg = repo.to_str().unwrap();

    let result = run_init(
        temp.path(),
        &home,
        &bin,
        &[
            "init",
            "-y",
            "--root",
            repo_arg,
            "--project",
            "scripted",
            "--runner",
            "codex",
            "--default-branch",
            "develop",
            "--github-url",
            "https://user:secret@github.com/example/scripted.git?token=hidden",
            "--orchestrator-runner",
            "claude",
        ],
    );
    assert!(
        result.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        result.stdout,
        result.stderr
    );

    let project = load_project(&home, "scripted");
    assert_eq!(project.name, "scripted");
    assert_eq!(project.default_branch, "develop");
    assert_eq!(
        project.github_url.as_deref(),
        Some("https://github.com/example/scripted.git")
    );
    assert_eq!(project.orchestrator.runner, "claude");
    // Workspace provisioning moved to the orchestrator's first-boot interview,
    // so a freshly-init'd project ships with an empty pool.
    assert!(project.workspaces.is_empty());
}

#[test]
fn configured_repository_is_a_write_free_success_even_without_prerequisites() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("configured");
    let home = temp.path().join("home");
    git_init(&repo);
    let ready_bin = fake_path(&temp.path().join("ready-path"), FakePath::one_runner());

    let first = run_init(&repo, &home, &ready_bin, &["init", "-y"]);
    assert!(
        first.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        first.stdout,
        first.stderr
    );
    let before = snapshot_files(&home);

    let empty_bin = fake_path(
        &temp.path().join("empty-path"),
        FakePath {
            git: false,
            claude: false,
            codex: false,
            tmux: false,
        },
    );
    let second = run_init(&repo, &home, &empty_bin, &["init", "-y"]);
    assert!(
        second.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        second.stdout,
        second.stderr
    );
    assert!(second.stdout.contains("already configured"));
    assert_eq!(before, snapshot_files(&home));
}

#[test]
fn yes_mode_rejects_legacy_prompting_flows_while_stdin_is_open() {
    let temp = TempDir::new().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let bin = fake_path(
        &temp.path().join("empty-path"),
        FakePath {
            git: false,
            claude: false,
            codex: false,
            tmux: false,
        },
    );

    for (flag, value) in [("--pick-up", None), ("--mode", Some("global"))] {
        let home = temp.path().join(flag.trim_start_matches('-'));
        let mut args = vec!["init", "-y", flag];
        if let Some(value) = value {
            args.push(value);
        }
        let result = run_init(&repo, &home, &bin, &args);
        assert!(!result.status.success());
        assert!(
            result.stderr.contains("cannot be combined"),
            "stderr for {flag}: {}",
            result.stderr
        );
        assert!(!home.exists());
    }
}
