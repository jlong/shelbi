//! End-to-end tests for what `shelbi message` does when nothing would read the
//! message: it records `ack=undeliverable` and stops, so `message status` says so
//! and the question the message answered stays open. Each test runs the real
//! binary in an isolated home, with no daemon (the push treats its daemon notify
//! as best effort). The hook side of the channel is tested where the hooks live,
//! in `shelbi_orchestrator::workspace`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");
const T0: &str = "2026-01-01T00:00:00+00:00";

/// An isolated home with project `p`, its workspace's worktree, one task `t` and
/// one open question `q-1` on it.
struct Home {
    path: PathBuf,
}

impl Home {
    fn new(tag: &str, column: &str) -> Self {
        let pid = std::process::id();
        let path = PathBuf::from(format!("/tmp/shb-msg-{tag}-{pid}"));
        let _ = std::fs::remove_dir_all(&path);
        for dir in ["projects/p/tasks", "projects/p/questions/t", "repo/.shelbi/wt/dev", "tmp"] {
            std::fs::create_dir_all(path.join(dir)).unwrap();
        }
        let repo = path.join("repo");
        std::fs::write(
            path.join("projects/p.yaml"),
            format!(
                "name: p\nrepo: {repo}\ndefault_branch: main\n\
                 orchestrator:\n  runner: claude\n\
                 agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: {repo}\n\
                 workspaces:\n  - {{ name: dev, machine: local, runner: claude }}\n",
                repo = repo.display(),
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("projects/p/tasks/t.md"),
            format!(
                "---\nid: t\ntitle: Title\ncolumn: {column}\npriority: 0\nassigned_to: dev\n\
                 created_at: {T0}\nupdated_at: {T0}\n---\nbody\n"
            ),
        )
        .unwrap();
        std::fs::write(
            path.join("projects/p/questions/t/q-1.md"),
            format!(
                "---\nid: q-1\ntask: t\nproject: p\nkind: clarification\nasked_at: {T0}\n---\n\
                 Which one?\n"
            ),
        )
        .unwrap();
        Self { path }
    }

    fn shelbi(&self, args: &[&str]) -> Output {
        Command::new(BIN)
            .args(["--project", "p"])
            .args(args)
            .env("SHELBI_HOME", &self.path)
            .env("SHELBI_HUB_SOCK", self.path.join("no-hub.sock"))
            .env("SHELBI_CONTROL_SOCK", self.path.join("no-control.sock"))
            .env("TMPDIR", self.path.join("tmp"))
            .env("SHELBI_YES", "0")
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_ADDR")
            .stdin(Stdio::null())
            .output()
            .expect("run shelbi")
    }

    fn messages(&self) -> PathBuf {
        self.path.join("repo/.shelbi/wt/dev/.shelbi/messages")
    }

    /// Write the task's session beacon, as the SessionStart hook or dispatch does.
    fn beacon(&self, line: &str) {
        std::fs::create_dir_all(self.messages()).unwrap();
        std::fs::write(self.messages().join("t.session"), line).unwrap();
    }

    fn events(&self) -> String {
        std::fs::read_to_string(self.path.join("events.log")).unwrap_or_default()
    }

    /// Reply to `q-1`.
    fn reply(&self) -> Output {
        self.shelbi(&["message", "t", "reply", "the second", "--in-response-to", "q-1"])
    }

    fn question_is_open(&self) -> bool {
        let out = self.shelbi(&["question", "list", "--json"]);
        assert!(out.status.success(), "{}", stderr(&out));
        String::from_utf8_lossy(&out.stdout).contains("q-1")
    }

    /// The refusal path's whole contract, from the reply that gets refused.
    #[track_caller]
    fn assert_refused(&self, out: &Output, reason: &str) {
        let err = stderr(out);
        assert!(!out.status.success(), "should exit non-zero: {err}");
        assert!(err.contains("UNDELIVERABLE"), "{err}");
        assert!(err.contains(reason), "{err}");
        assert!(!err.contains("restart"), "must not promise a restart delivers: {err}");

        let msg_id = message_id(&err);
        let status = self.shelbi(&["message", "status", &msg_id]);
        let status_err = stderr(&status);
        assert!(!status.status.success(), "status gates on delivery: {status_err}");
        assert!(status_err.contains("undeliverable"), "{status_err}");
        assert!(status_err.contains(reason), "{status_err}");

        let events = self.events();
        assert!(
            events.contains(&format!("message={msg_id} task=t ack=undeliverable reason={reason}")),
            "{events}"
        );
        assert!(!events.contains("push=ok"), "a refusal is not a push: {events}");
        assert!(
            !self.messages().join("t.log").exists(),
            "a refused message must not reach the log"
        );
        assert!(self.question_is_open(), "the answer never arrived");
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The `m-<millis>-<pid>` id out of a CLI message.
fn message_id(text: &str) -> String {
    let at = text.find("m-1").or_else(|| text.find("m-2")).expect("a message id");
    text[at..].split_whitespace().next().unwrap().to_string()
}

/// A process to stand in for the worker session, and its beacon line.
fn live_session(worktree: &Path) -> (Child, String) {
    let child = Command::new("sleep")
        .arg("60")
        .current_dir(worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let out = Command::new("sh")
        .arg("-c")
        .arg(format!("echo {pid} $(LC_ALL=C TZ=UTC ps -o lstart= -p {pid})"))
        .output()
        .unwrap();
    (child, String::from_utf8(out.stdout).unwrap())
}

#[test]
fn a_done_task_is_undeliverable() {
    let home = Home::new("done", "done");
    assert!(home.question_is_open());
    let out = home.reply();
    home.assert_refused(&out, "done");
}

/// Done wins over everything that would otherwise fail first: a finished task's
/// worktree is often gone, and that shouldn't turn the answer into a config error.
#[test]
fn a_done_task_without_a_worktree_is_still_undeliverable() {
    let home = Home::new("done-gone", "done");
    std::fs::remove_dir_all(home.path.join("repo/.shelbi/wt/dev")).unwrap();
    let out = home.reply();
    home.assert_refused(&out, "done");
}

#[test]
fn a_session_that_never_started_is_undeliverable() {
    let home = Home::new("no-beacon", "in_progress");
    let out = home.reply();
    home.assert_refused(&out, "no_session");
}

#[test]
fn a_dead_session_is_undeliverable() {
    let home = Home::new("dead", "in_progress");
    let (mut session, line) = live_session(&home.path);
    home.beacon(&line);
    let _ = session.kill();
    let _ = session.wait();
    let out = home.reply();
    home.assert_refused(&out, "no_session");
}

/// A polling runner has no hooks, so its liveness comes from its pane. No pane
/// here, so it's refused just like a dead hooks session.
#[test]
fn a_polling_runner_without_a_pane_is_undeliverable() {
    let home = Home::new("polling", "in_progress");
    home.beacon("polling\n");
    let out = home.reply();
    home.assert_refused(&out, "no_session");
}

/// The other side of the gate: a live session's reply is queued, written to the
/// log and closes the question.
#[test]
fn a_live_session_queues_the_message_and_answers_the_question() {
    let home = Home::new("live", "in_progress");
    let (mut session, line) = live_session(&home.path);
    home.beacon(&line);

    let out = home.reply();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout.contains("queued"), "{stdout}");

    let msg_id = message_id(&stdout);
    let log = std::fs::read_to_string(home.messages().join("t.log")).unwrap();
    assert!(log.contains(&msg_id) && log.contains("the second"), "{log}");
    assert!(home.events().contains(&format!("message={msg_id} task=t push=ok")));
    assert!(!home.question_is_open(), "answered once queued");

    let status = home.shelbi(&["message", "status", &msg_id]);
    assert!(stderr(&status).contains("queued"), "{}", stderr(&status));

    let _ = session.kill();
    let _ = session.wait();
}

/// The whole channel in one test: the deployed hooks and the real CLI, with the
/// beacon the hook really writes. Send A, drain, send B, drain, then the session
/// dies and the next send is refused.
#[test]
fn messages_flow_through_the_real_hooks_until_the_session_dies() {
    let home = Home::new("full", "in_progress");
    let worktree = home.path.join("repo/.shelbi/wt/dev");
    shelbi_orchestrator::workspace::deploy_runner_hooks(&shelbi_core::Host::Local, &worktree)
        .unwrap();

    // The hook's session is its first non-shell ancestor, so a `perl` that runs
    // it and stays up plays the agent CLI.
    let mut session = Command::new("perl")
        .arg("-e")
        .arg(r#"system("sh", "-c", ".shelbi/hooks/session-start.sh"); sleep 60"#)
        .env("TASK_ID", "t")
        .current_dir(&worktree)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let beacon = home.messages().join("t.session");
    let start = std::time::Instant::now();
    while !beacon.exists() {
        assert!(start.elapsed().as_secs() < 30, "SessionStart never wrote the beacon");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    let send = |body: &str| home.shelbi(&["message", "t", "directive", body]);
    let drain = || {
        let out = Command::new("sh")
            .arg("-c")
            .arg(".shelbi/hooks/stop.sh")
            .env("TASK_ID", "t")
            .env("PROJECT", "p")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_HUB_SOCK")
            .current_dir(&worktree)
            .output()
            .unwrap();
        assert!(out.status.success());
        String::from_utf8(out.stdout).unwrap()
    };

    let a = send("message-a");
    assert!(a.status.success(), "{}", stderr(&a));
    let first = drain();
    assert!(first.contains("message-a"), "{first}");

    let b = send("message-b");
    assert!(b.status.success(), "{}", stderr(&b));
    let second = drain();
    assert!(second.contains("message-b"), "lost after the first drain: {second}");
    assert!(!second.contains("message-a"), "replayed: {second}");

    let _ = session.kill();
    let _ = session.wait();
    let refused = send("message-c");
    let err = stderr(&refused);
    assert!(!refused.status.success(), "{err}");
    assert!(err.contains("no_session"), "{err}");
    assert_eq!(drain(), "", "a refused message must not wait in the log");
}
