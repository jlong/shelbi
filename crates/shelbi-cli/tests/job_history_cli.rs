//! `shelbi job list --all` and `shelbi job log`: finished jobs stay listed
//! from the job history (`jobs/history.json`), newest first after the running
//! ones, and `job log` prints a command job's output (all of it, its last
//! `-n` lines, or followed to the end with `-f`).
//!
//! Each test runs the real binary under its own `SHELBI_ROOT`, with no daemon.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// An isolated root holding one registered global project, `demo`.
struct Root {
    path: PathBuf,
}

impl Root {
    fn new(tag: &str) -> Self {
        let path = std::env::temp_dir().join(format!("shelbi-job-hist-{tag}-{}", std::process::id()));
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
            .current_dir(&self.path)
            .env("SHELBI_ROOT", &self.path)
            .env_remove("SHELBI_HOME")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_CONTROL_SOCK")
            .env_remove("SHELBI_MANAGED_CONTEXT")
            // A worker pane's identity: a job started here would record it.
            .env_remove("SHELBI_WORKSPACE")
            .env_remove("SHELBI_AGENT")
            .env_remove("TASK_ID")
            .env_remove("PROJECT");
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn project_dir(&self) -> PathBuf {
        shelbi_state::projects_dir_in(&self.path).join("demo")
    }

    fn job_dir(&self, id: &str) -> PathBuf {
        self.project_dir().join("jobs").join(id)
    }

    fn write(&self, rel: &str, body: &str) {
        let path = self.project_dir().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }
}

impl Drop for Root {
    fn drop(&mut self) {
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
    String::from_utf8(out.stdout.clone()).unwrap()
}

fn rfc(mins_ago: i64) -> String {
    (chrono::Utc::now() - chrono::Duration::minutes(mins_ago)).to_rfc3339()
}

/// `j5` running; `j1` (failed command), `j2` (merged PR watch) and `j3` (ok
/// command) finished, in that order of finishing: j3 last.
fn seed(root: &Root) {
    root.write(
        "awareness.json",
        &format!(
            r#"{{"next_id": 5, "jobs": [
                {{"id": "j4", "kind": "pid", "target": "1", "started_at": "{}"}},
                {{"id": "j5", "kind": "command", "target": "cargo test", "label": "desktop tests", "started_at": "{}"}}
            ]}}"#,
            rfc(50),
            rfc(2)
        ),
    );
    let log = |id: &str| root.job_dir(id).join("output.log").display().to_string();
    root.write(
        "jobs/history.json",
        &format!(
            r#"{{"jobs": [
                {{"id": "j1", "kind": "command", "target": "cargo build", "started_at": "{}", "finished_at": "{}",
                  "status": "failed", "exit_code": 3, "detail": "line 30", "log": "{}"}},
                {{"id": "j3", "kind": "command", "target": "true", "started_at": "{}", "finished_at": "{}",
                  "status": "ok", "exit_code": 0, "log": "{}"}},
                {{"id": "j2", "kind": "pr", "target": "2265", "started_at": "{}", "finished_at": "{}", "status": "merged"}}
            ]}}"#,
            rfc(64),
            rfc(60),
            log("j1"),
            rfc(30),
            rfc(29),
            log("j3"),
            rfc(55),
            rfc(40),
        ),
    );
    let body: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    root.write("jobs/j1/output.log", &body);
    root.write("jobs/j3/output.log", "");
}

#[test]
fn list_shows_running_jobs_and_all_adds_finished_ones_newest_first() {
    let root = Root::new("list");
    seed(&root);

    let running = ok(&root.run(&["job", "list"]));
    assert!(running.contains("desktop tests") && !running.contains("cargo build"), "{running}");

    let all = ok(&root.run(&["job", "list", "--all"]));
    let pos = |s: &str| all.find(s).unwrap_or_else(|| panic!("{s} in\n{all}"));
    // Running (newest started first), then finished (newest finished first).
    assert!(pos("j5 ") < pos("j4 "), "{all}");
    assert!(pos("j4 ") < pos("j3 "), "{all}");
    assert!(pos("j3 ") < pos("j2 "), "{all}");
    assert!(pos("j2 ") < pos("j1 "), "{all}");
    let j1 = all.lines().find(|l| l.starts_with("j1 ")).unwrap();
    assert!(j1.contains("failed") && j1.contains("exit 3") && j1.contains("4m"), "{j1}");
    assert!(all.contains(&format!("log: {}", root.job_dir("j1").join("output.log").display())), "{all}");
    assert!(all.lines().find(|l| l.starts_with("j2 ")).unwrap().contains("merged"), "{all}");
    // The running rows' elapsed seconds may tick between the two runs: drop
    // the digits before comparing.
    let no_digits = |s: &str| s.chars().filter(|c| !c.is_ascii_digit()).collect::<String>();
    assert_eq!(no_digits(&all), no_digits(&ok(&root.run(&["job", "list", "--history"]))), "--history is --all");

    let json: serde_json::Value = serde_json::from_str(&ok(&root.run(&["job", "list", "--all", "--json"]))).unwrap();
    let rows: Vec<(String, String)> = json
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["id"].as_str().unwrap().into(), v["status"].as_str().unwrap().into()))
        .collect();
    let rows: Vec<(&str, &str)> = rows.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    assert_eq!(
        rows,
        [("j5", "running"), ("j4", "running"), ("j3", "ok"), ("j2", "merged"), ("j1", "failed")]
    );
    assert_eq!(json[4]["exit_code"], 3);
    assert_eq!(json[4]["duration_secs"], 240);
}

#[test]
fn log_prints_the_whole_log_or_its_last_lines() {
    let root = Root::new("log");
    seed(&root);
    let whole = ok(&root.run(&["job", "log", "j1"]));
    assert_eq!(whole.lines().count(), 30);
    assert!(whole.starts_with("line 1\n") && whole.ends_with("line 30\n"), "{whole}");

    let tail = ok(&root.run(&["job", "log", "j1", "-n", "20"]));
    let expected: String = (11..=30).map(|i| format!("line {i}\n")).collect();
    assert_eq!(tail, expected);
    assert_eq!(ok(&root.run(&["job", "log", "j1", "--lines", "1"])), "line 30\n");
    // A finished job with an empty log prints nothing, and -f on it returns.
    assert_eq!(ok(&root.run(&["job", "log", "j3", "-f"])), "");
}

#[test]
fn log_on_a_watch_prints_its_status_and_an_unknown_id_fails() {
    let root = Root::new("watch");
    seed(&root);
    let pr = ok(&root.run(&["job", "log", "j2"]));
    assert!(pr.contains("PR #2265") && pr.contains("merged") && pr.contains("no command output"), "{pr}");
    let pid = ok(&root.run(&["job", "log", "j4"]));
    assert!(pid.contains("pid 1") && pid.contains("still waiting"), "{pid}");

    let out = root.run(&["job", "log", "j99"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no job `j99`"));

    // A record whose log is gone says so.
    std::fs::remove_file(root.job_dir("j1").join("output.log")).unwrap();
    let out = root.run(&["job", "log", "j1"]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("no longer kept"), "{out:?}");
}

#[test]
fn follow_streams_a_running_job_until_it_writes_its_result() {
    let root = Root::new("follow");
    seed(&root);
    let log = root.job_dir("j5").join("output.log");
    root.write("jobs/j5/output.log", "first\n");
    let child = root
        .command(&["job", "log", "j5", "-f"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(600));
    append(&log, "second\n");
    std::thread::sleep(Duration::from_millis(600));
    append(&log, "last\n");
    root.write(
        "jobs/j5/result.json",
        &format!(r#"{{"exit_code": 2, "finished_at": "{}"}}"#, rfc(0)),
    );
    let out = wait_with_timeout(child, Duration::from_secs(20));
    assert_eq!(ok(&out), "first\nsecond\nlast\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("job j5 finished: failed, exit 2"), "{stderr}");
}

fn append(path: &Path, text: &str) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

fn wait_with_timeout(mut child: std::process::Child, limit: Duration) -> Output {
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().unwrap().is_some() {
            return child.wait_with_output().unwrap();
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!("`job log -f` never ended: {out:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `--estimate` on `job watch`, `job estimate` revising it, and `job list` /
/// `job log` showing elapsed against it.
#[test]
fn job_estimates_are_set_revised_and_listed() {
    let root = Root::new("estimate");
    root.write(
        "awareness.json",
        &format!(
            r#"{{"next_id": 1, "jobs": [
                {{"id": "j1", "kind": "command", "target": "cargo test", "label": "desktop tests",
                  "started_at": "{}", "estimate": "30m", "next_alert_at": "{}"}}
            ]}}"#,
            rfc(45),
            rfc(15)
        ),
    );
    root.write("jobs/j1/output.log", "step 1\nstep 2\n");
    root.write(
        "jobs/history.json",
        &format!(
            r#"{{"jobs": [{{"id": "j0", "kind": "pr", "target": "7", "started_at": "{}", "finished_at": "{}",
                 "status": "merged", "estimate": "10m"}}]}}"#,
            rfc(90),
            rfc(70)
        ),
    );
    let list = ok(&root.run(&["job", "list"]));
    assert!(list.contains(" 45m") && list.contains("/30m overdue") && list.contains("desktop tests"), "{list}");
    let all = ok(&root.run(&["job", "list", "--all"]));
    assert!(all.lines().next().unwrap().starts_with("j1    command  overdue"), "{all}");
    assert!(all.contains("20m/10m"), "the history keeps estimate against actual: {all}");

    // The log's header (stderr) says where it stands; the output stays clean.
    let out = root.run(&["job", "log", "j1"]);
    assert_eq!(ok(&out), "step 1\nstep 2\n");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("job j1 running: 30m estimate, elapsed 45m, overdue by 15m"), "{stderr}");

    // Revised up: no longer overdue, counted from the start.
    let revised = ok(&root.run(&["job", "estimate", "j1", "1h"]));
    assert!(revised.starts_with("✓ revised estimate for job j1: 1h (elapsed 45m, 1"), "{revised}");
    assert!(revised.trim_end().ends_with("m left)"), "{revised}");
    let list = ok(&root.run(&["job", "list"]));
    assert!(list.contains("/1h ") && !list.contains("overdue"), "{list}");
    let json: serde_json::Value = serde_json::from_str(&ok(&root.run(&["job", "list", "--json"]))).unwrap();
    assert_eq!(json[0]["estimate"], "1h");
    assert_eq!(json[0]["overdue"], false);

    // Finished or unknown jobs, and bad durations, are refused.
    for (args, want) in [
        (vec!["job", "estimate", "j0", "1h"], "has finished"),
        (vec!["job", "estimate", "j9", "1h"], "no running job `j9`"),
        (vec!["job", "estimate", "j1", "soon"], ""),
    ] {
        let out = root.run(&args);
        assert!(!out.status.success(), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(want), "{args:?}: {out:?}");
    }

    // A watch takes one too.
    let me = std::process::id().to_string();
    let watched = ok(&root.run(&["job", "watch", "--pid", &me, "--estimate", "1h30m"]));
    assert!(watched.contains("estimate: 1h30m"), "{watched}");
    let bad = root.run(&["job", "watch", "--pid", &me, "--estimate", "0m"]);
    assert!(!bad.status.success(), "{bad:?}");
}

#[test]
fn a_job_started_from_a_worker_session_records_the_worker() {
    let root = Root::new("starter");
    let pid = std::process::id().to_string();
    let worker = |args: &[&str]| {
        root.command(args)
            .env("SHELBI_WORKSPACE", "dev")
            .env("SHELBI_AGENT", "developer")
            .env("TASK_ID", "t1")
            .env("PROJECT", "demo")
            .env("SHELBI_MANAGED_CONTEXT", "1")
            .output()
            .unwrap()
    };
    let started = ok(&worker(&["job", "watch", "--pid", &pid, "--label", "mine"]));
    assert!(started.contains("reports to: dev"), "{started}");
    // The user's own shell: no starter, reports to the orchestrator as before.
    let plain = ok(&root.run(&["job", "watch", "--pid", &pid, "--label", "theirs"]));
    assert!(!plain.contains("reports to"), "{plain}");

    let list = ok(&root.run(&["job", "list"]));
    let line = |label: &str| list.lines().find(|l| l.contains(label)).unwrap_or_else(|| panic!("{label} in\n{list}")).to_string();
    assert!(line("mine").ends_with("mine  by dev"), "{list}");
    assert!(line("theirs").ends_with("theirs"), "{list}");

    let json: serde_json::Value = serde_json::from_str(&ok(&root.run(&["job", "list", "--json"]))).unwrap();
    let mine = json.as_array().unwrap().iter().find(|j| j["label"] == "mine").unwrap();
    assert_eq!(mine["started_by"], serde_json::json!({"workspace": "dev", "agent": "developer", "task": "t1"}));

    let all = ok(&root.run(&["job", "list", "--all"]));
    assert!(all.contains("      by: dev (developer, t1)"), "{all}");
    let id = mine["id"].as_str().unwrap();
    let log = root.run(&["job", "log", id]);
    assert!(String::from_utf8_lossy(&log.stderr).contains(&format!("job {id} started by dev (developer, t1)")), "{log:?}");

    // A session of another project has no worker in this one.
    let other = root
        .command(&["job", "watch", "--pid", &pid])
        .env("SHELBI_WORKSPACE", "dev")
        .env("PROJECT", "elsewhere")
        .output()
        .unwrap();
    assert!(!ok(&other).contains("reports to"));
}
