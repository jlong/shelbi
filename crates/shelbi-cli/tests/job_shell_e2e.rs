//! `shelbi job run` runs its command in a Shelbi session of its own
//! (`<project>/job/<id>`), under a PTY: the command sees a terminal, its
//! output still lands in `output.log` as plain text, `result.json` records
//! how it ended, and `job cancel` stops it through the session's runner.
//!
//! Each test runs the real binary under its own `SHELBI_ROOT`, with no
//! daemon, so the sessions it starts live in that root's `sessions/`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// An isolated root holding one registered global project, `demo`. Short,
/// so a session socket's path fits.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(tag: &str) -> Self {
        let path = PathBuf::from("/tmp").join(format!("sj-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        let projects = shelbi_state::projects_dir_in(&path);
        std::fs::create_dir_all(projects.join("demo/jobs")).unwrap();
        let work = path.join("work");
        std::fs::create_dir_all(&work).unwrap();
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

    fn command(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.arg("-p")
            .arg("demo")
            .args(args)
            .current_dir(self.path.join("work"))
            .env("SHELBI_ROOT", &self.path)
            // A plain login shell, so the job doesn't depend on the user's.
            .env("SHELL", "/bin/sh")
            .env_remove("SHELBI_HOME")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_CONTROL_SOCK")
            .env_remove("SHELBI_MANAGED_CONTEXT");
        cmd
    }

    fn job_dir(&self, id: &str) -> PathBuf {
        shelbi_state::projects_dir_in(&self.path).join("demo/jobs").join(id)
    }

    fn sessions(&self) -> Vec<shelbi_client::DiscoveredSession> {
        shelbi_client::list(&self.path.join("sessions")).unwrap_or_default()
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        // End any session still up (its pid comes from this root's own
        // session records), then remove the root.
        for s in self.sessions() {
            if s.alive && s.meta.pid > 1 {
                unsafe {
                    libc::kill(s.meta.pid as libc::pid_t, libc::SIGTERM);
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn ok(out: &Output) -> String {
    assert!(
        out.status.success(),
        "exit {:?}\nstdout: {}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn wait_for<T>(what: &str, limit: Duration, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn result(dir: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(dir.join("result.json")).ok()?;
    serde_json::from_str(&text).ok()
}

#[test]
fn a_job_runs_in_its_own_session_with_a_terminal_and_keeps_its_log() {
    let root = Root::new("run");
    let out = ok(&root
        .command(&[
            "job",
            "run",
            "--label",
            "e2e",
            "--",
            "sh",
            "-c",
            "printf '\\033[32mhello\\033[0m\\n'; [ -t 1 ] && echo tty; echo \"$JOB_E2E\"; exit 4",
        ])
        .env("JOB_E2E", "from-the-caller")
        .output()
        .unwrap());
    assert!(out.contains("✓ job j1 started: e2e"), "{out}");

    let dir = root.job_dir("j1");
    let result = wait_for("the job's result", Duration::from_secs(30), || result(&dir));
    assert_eq!(result["exit_code"], 4, "{result}");
    let log = std::fs::read_to_string(dir.join("output.log")).unwrap();
    assert_eq!(log, "hello\ntty\nfrom-the-caller\n", "plain text, the caller's environment");
    assert!(!dir.join("env.json").exists(), "the runner removed the caller's environment");

    // It ran in a job session, never a workspace's.
    let sessions = root.sessions();
    let names: Vec<&str> = sessions.iter().map(|s| s.meta.name.as_str()).collect();
    assert_eq!(names, ["demo/job/j1"], "{names:?}");
    wait_for("the session to exit", Duration::from_secs(10), || {
        root.sessions().iter().all(|s| !s.alive).then_some(())
    });
}

#[test]
fn cancel_stops_a_running_job_through_its_session() {
    let root = Root::new("cancel");
    ok(&root.run_job(&["sh", "-c", "echo started; sleep 60; echo never"]));
    let dir = root.job_dir("j1");
    wait_for("the job to start", Duration::from_secs(30), || {
        std::fs::read_to_string(dir.join("output.log"))
            .ok()
            .filter(|l| l.contains("started"))
    });
    let out = ok(&root.command(&["job", "cancel", "j1"]).output().unwrap());
    assert!(out.contains("canceled job j1"), "{out}");
    wait_for("the session to exit", Duration::from_secs(15), || {
        root.sessions().iter().all(|s| !s.alive).then_some(())
    });
    let log = std::fs::read_to_string(dir.join("output.log")).unwrap();
    assert!(!log.contains("never"), "the command was stopped: {log}");
    let listed = ok(&root.command(&["job", "list", "--all", "--json"]).output().unwrap());
    assert!(listed.contains("\"canceled\""), "{listed}");
}

impl Root {
    fn run_job(&self, cmd: &[&str]) -> Output {
        let mut args = vec!["job", "run", "--"];
        args.extend_from_slice(cmd);
        self.command(&args).output().unwrap()
    }
}
