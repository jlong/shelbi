//! End-to-end tests of the `shelbi session` debug subcommands against **real**
//! detached session processes.
//!
//! Each case drives the actual `shelbi` binary (`CARGO_BIN_EXE_shelbi`) as a
//! subprocess with an isolated `SHELBI_HOME`, so `new` spawns a genuine
//! `shelbi __session` owning a PTY and a `/bin/sh` + `cat` child, and `ls`,
//! `snapshot`, `send`, and `kill` drive it over its Unix socket — the same path
//! a user takes. `SHELBI_HOME` is passed per-subprocess (never set on the test
//! process), so these need no serial lock and never touch the real `~/.shelbi`.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn shelbi(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_shelbi"))
        .args(args)
        .env("SHELBI_HOME", home)
        .env_remove("SHELBI_PROJECT")
        .env_remove("TASK_ID")
        .output()
        .expect("run shelbi")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Poll `f` until it returns `Some`, or the deadline passes.
fn wait_for<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Best-effort cleanup: kill the session even if an assertion panics, before
/// the temp home is removed.
struct KillGuard {
    home: PathBuf,
    id: String,
}

impl Drop for KillGuard {
    fn drop(&mut self) {
        let _ = shelbi(&self.home, &["session", "kill", &self.id]);
    }
}

#[test]
fn session_subcommands_drive_a_real_session_end_to_end() {
    let home = tempfile::tempdir().unwrap();
    let cwd = tempfile::tempdir().unwrap();

    // new: spawn a detached session running a shell that prints a marker then
    // execs `cat` (a PTY echoes typed input, so `send` is observable).
    let new = shelbi(
        home.path(),
        &[
            "session",
            "new",
            "--name",
            "demo/ws/alpha",
            "--cwd",
            cwd.path().to_str().unwrap(),
            "--cols",
            "80",
            "--rows",
            "24",
            "--task",
            "t-7",
            "--",
            "/bin/sh",
            "-c",
            "printf 'READYMARK\\n'; exec cat",
        ],
    );
    assert!(new.status.success(), "session new failed: {}", String::from_utf8_lossy(&new.stderr));
    let out = stdout(&new);
    // "started session <id> (demo/ws/alpha)"
    let id = out
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(2))
        .expect("new should print the session id")
        .to_string();
    assert!(!id.is_empty());
    let _guard = KillGuard {
        home: home.path().to_path_buf(),
        id: id.clone(),
    };

    // ls: the session shows up live and fully serving — size is queried over
    // the socket, so waiting for "80x24" also confirms it is accepting
    // connections (the lifetime lock is taken a touch before the listener).
    let listed = wait_for(Duration::from_secs(10), || {
        let out = stdout(&shelbi(home.path(), &["session", "ls"]));
        if out.contains("demo/ws/alpha") && out.contains("live") && out.contains("80x24") {
            Some(out)
        } else {
            None
        }
    })
    .expect("ls should list the live session with its size");
    assert!(listed.contains("t-7"), "ls should show the task:\n{listed}");
    assert!(listed.contains(&id), "ls should show the short id:\n{listed}");

    // snapshot: the child's startup marker is on screen.
    let snap = wait_for(Duration::from_secs(10), || {
        let out = stdout(&shelbi(home.path(), &["session", "snapshot", &id]));
        out.contains("READYMARK").then_some(out)
    })
    .expect("snapshot should show the child's marker");
    assert!(snap.contains("READYMARK"));

    // send: deliver text with a trailing Enter; the PTY echoes it and `cat`
    // prints it, so a later snapshot contains it.
    let sent = shelbi(home.path(), &["session", "send", &id, "hello-there", "--enter"]);
    assert!(sent.status.success(), "session send failed: {}", String::from_utf8_lossy(&sent.stderr));
    let after = wait_for(Duration::from_secs(10), || {
        let out = stdout(&shelbi(home.path(), &["session", "snapshot", &id]));
        out.contains("hello-there").then_some(out)
    })
    .expect("snapshot should show the sent text");
    assert!(after.contains("hello-there"));

    // kill: the session dies; `ls` reports it dead (list does not reap).
    let killed = shelbi(home.path(), &["session", "kill", &id]);
    assert!(killed.status.success(), "session kill failed: {}", String::from_utf8_lossy(&killed.stderr));
    let dead = wait_for(Duration::from_secs(10), || {
        let out = stdout(&shelbi(home.path(), &["session", "ls"]));
        // The session line is no longer "live"; it reads "dead" (optionally
        // with an exit code/signal).
        (out.contains(&id) && out.contains("dead")).then_some(out)
    });
    assert!(dead.is_some(), "ls should report the killed session as dead");
}

#[test]
fn ls_on_an_empty_home_reports_no_sessions() {
    let home = tempfile::tempdir().unwrap();
    let out = shelbi(home.path(), &["session", "ls"]);
    assert!(out.status.success());
    assert!(stdout(&out).contains("no sessions"));
}

#[test]
fn selector_with_no_match_errors() {
    let home = tempfile::tempdir().unwrap();
    let out = shelbi(home.path(), &["session", "snapshot", "does-not-exist"]);
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("no session") || err.contains("no sessions"), "stderr was: {err}");
}
