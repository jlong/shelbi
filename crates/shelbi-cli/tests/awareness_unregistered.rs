//! `shelbi remind`, `shelbi job` and `shelbi issue estimate` refuse a project
//! that isn't registered before touching disk: a mistyped `-p` (or a stale
//! `SHELBI_PROJECT`) must error out instead of creating
//! `projects/<typo>/awareness.json`, whose entries would fire into an event log
//! no orchestrator reads.
//!
//! Each test runs the real binary under its own `SHELBI_ROOT`, with no daemon.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// An isolated root holding one registered global project, `demo`, whose
/// work dir is `<root>/work`.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "shelbi-aware-unreg-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        let projects = shelbi_state::projects_dir_in(&path);
        std::fs::create_dir_all(projects.join("demo")).unwrap();
        std::fs::create_dir_all(path.join("work")).unwrap();
        let work = path.join("work");
        std::fs::write(
            projects.join("demo.yaml"),
            format!(
                "name: demo\nrepo: {w}\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: {w}\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n",
                w = work.display()
            ),
        )
        .unwrap();
        Self { path }
    }

    /// Run `shelbi <args>` from `cwd`, with `project_env` as `SHELBI_PROJECT`.
    fn run_in(&self, cwd: &Path, project_env: Option<&str>, args: &[&str]) -> Output {
        let mut cmd = Command::new(BIN);
        cmd.args(args)
            .current_dir(cwd)
            .env("SHELBI_ROOT", &self.path)
            .env_remove("SHELBI_HOME")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_CONTROL_SOCK")
            .env_remove("SHELBI_MANAGED_CONTEXT");
        if let Some(p) = project_env {
            cmd.env("SHELBI_PROJECT", p);
        }
        cmd.output().unwrap()
    }

    /// Run from the root itself (outside any project's work dir).
    fn run(&self, args: &[&str]) -> Output {
        self.run_in(&self.path, None, args)
    }

    fn projects_dir(&self) -> PathBuf {
        shelbi_state::projects_dir_in(&self.path)
    }

    fn projects(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.projects_dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// The command failed with the registration error and wrote nothing.
    #[track_caller]
    fn assert_refused(&self, out: &Output) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(!out.status.success(), "expected failure; stderr: {stderr}");
        assert!(
            stderr.contains("project `livetest` is not registered"),
            "stderr: {stderr}"
        );
        assert!(stderr.contains("livetest.yaml"), "stderr: {stderr}");
        assert_eq!(self.projects(), ["demo", "demo.yaml"]);
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn stdout(out: &Output) -> String {
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn remind_refuses_an_unregistered_project() {
    let root = Root::new("remind");
    root.assert_refused(&root.run(&["-p", "livetest", "remind", "in", "1h", "check", "it"]));
    root.assert_refused(&root.run(&["-p", "livetest", "remind", "at", "23:59", "x"]));
    root.assert_refused(&root.run(&["-p", "livetest", "remind", "list"]));
    root.assert_refused(&root.run(&["-p", "livetest", "remind", "cancel", "r1"]));
    // A stale `SHELBI_PROJECT` is refused the same way.
    root.assert_refused(&root.run_in(&root.path, Some("livetest"), &["remind", "in", "1h", "x"]));
}

#[test]
fn job_refuses_an_unregistered_project() {
    let root = Root::new("job");
    root.assert_refused(&root.run(&["-p", "livetest", "job", "run", "--", "true"]));
    let pid = std::process::id().to_string();
    root.assert_refused(&root.run(&["-p", "livetest", "job", "watch", "--pid", &pid]));
    root.assert_refused(&root.run(&["-p", "livetest", "job", "list"]));
    root.assert_refused(&root.run(&["-p", "livetest", "job", "cancel", "j1"]));
    root.assert_refused(&root.run_in(&root.path, Some("livetest"), &["job", "run", "--", "true"]));
}

#[test]
fn issue_estimate_refuses_an_unregistered_project() {
    let root = Root::new("estimate");
    root.assert_refused(&root.run(&["-p", "livetest", "issue", "estimate", "t1", "30m"]));
}

#[test]
fn a_registered_project_still_takes_reminders_and_jobs() {
    let root = Root::new("ok");
    let store = root.projects_dir().join("demo/awareness.json");

    // By `-p`.
    let out = stdout(&root.run(&["-p", "demo", "remind", "in", "1h", "check", "it"]));
    assert!(out.contains("reminder r1"), "{out}");
    // By `$SHELBI_PROJECT`.
    let out = stdout(&root.run_in(&root.path, Some("demo"), &["remind", "in", "2h", "again"]));
    assert!(out.contains("reminder r2"), "{out}");
    // By the current directory.
    let work = root.path.join("work");
    let out = stdout(&root.run_in(&work, None, &["remind", "list"]));
    assert!(out.contains("check it") && out.contains("again"), "{out}");
    let out = stdout(&root.run_in(&work, None, &["remind", "cancel", "r2"]));
    assert!(out.contains("r2"), "{out}");

    let pid = std::process::id().to_string();
    let out = stdout(&root.run(&["-p", "demo", "job", "watch", "--pid", &pid, "--label", "me"]));
    assert!(out.contains("job j3"), "{out}");
    let out = stdout(&root.run(&["-p", "demo", "job", "list"]));
    assert!(out.contains("me"), "{out}");
    let out = stdout(&root.run(&["-p", "demo", "job", "cancel", "j3"]));
    assert!(out.contains("j3"), "{out}");

    assert!(store.is_file());
    assert_eq!(root.projects(), ["demo", "demo.yaml"]);
}
