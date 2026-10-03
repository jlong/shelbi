//! Spike item 5: process survival.
//!
//! A detached session must outlive its launcher, lose its controlling
//! terminal, keep running, and take its child's whole process group down with
//! it on kill. These tests drive the `rt-runtime` binary and assert on the
//! status/marker files it writes. Unix-only (the whole effort targets macOS and
//! Linux).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rt-runtime");

fn tmpdir() -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!(
        "rt-spike-survival-{}-{}",
        std::process::id(),
        Instant::now().elapsed().as_nanos()
    ));
    fs::create_dir_all(&p).unwrap();
    p
}

fn read_kv(path: &Path, key: &str) -> Option<i32> {
    let body = fs::read_to_string(path).ok()?;
    for line in body.lines() {
        if let Some(v) = line.strip_prefix(&format!("{key}=")) {
            return v.trim().parse().ok();
        }
    }
    None
}

fn alive(pid: i32) -> bool {
    // signal 0 probes existence without delivering anything.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_for<F: Fn() -> bool>(timeout: Duration, f: F) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        sleep(Duration::from_millis(20));
    }
    f()
}

/// The launcher returns immediately; the session keeps running, reparented away
/// from the launcher, and advances its marker. A SIGTERM then reaps it and its
/// child's entire process group.
#[test]
fn survives_launcher_exit_and_group_kill() {
    let dir = tmpdir();
    let pidfile = dir.join("pid");
    let status = dir.join("status");
    let marker = dir.join("marker");

    let out = Command::new(BIN)
        .args(["detach-spawn", "--pidfile"])
        .arg(&pidfile)
        .arg("--")
        .args(["rt-runtime", "session", "--status"])
        .arg(&status)
        .arg("--marker")
        .arg(&marker)
        .output()
        .expect("spawn launcher");
    assert!(out.status.success(), "launcher should return 0");

    // The session wrote its status.
    assert!(
        wait_for(Duration::from_secs(5), || status.exists() && marker.exists()),
        "session never wrote status/marker"
    );

    let session_pid = read_kv(&status, "session_pid").expect("session_pid");
    let child_pgid = read_kv(&status, "child_pgid").expect("child_pgid");
    assert!(child_pgid > 1, "child_pgid should be a real group");

    // It is alive and reparented: PPID is no longer the launcher (which has
    // exited), so it is 1 (init) or a subreaper. We just assert it is not the
    // launcher pid, which is already gone.
    assert!(alive(session_pid), "session should be alive after launcher exit");

    // The marker advances while the launcher is gone — real liveness.
    let m0: u64 = fs::read_to_string(&marker).unwrap().trim().parse().unwrap_or(0);
    assert!(
        wait_for(Duration::from_secs(3), || {
            fs::read_to_string(&marker)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|m| m > m0)
                .unwrap_or(false)
        }),
        "marker did not advance after launcher exit"
    );

    // Kill the session; its child's whole process group must die with it.
    unsafe {
        libc::kill(session_pid, libc::SIGTERM);
    }
    assert!(
        wait_for(Duration::from_secs(5), || !alive(session_pid)),
        "session did not exit on SIGTERM"
    );
    assert!(
        wait_for(Duration::from_secs(3), || !group_has_members(child_pgid)),
        "child process group survived the session kill"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Are there any live processes left in `pgid`? Uses `kill(-pgid, 0)`, which
/// succeeds iff at least one process exists in the group.
fn group_has_members(pgid: i32) -> bool {
    unsafe { libc::kill(-pgid, 0) == 0 }
}

/// The launcher's own stdout is a pipe. Because the session redirects its stdio
/// to /dev/null, it does NOT inherit that pipe, so the read side reaches EOF as
/// soon as the launcher exits — even though the session runs on. This is the
/// property that keeps a launching `ssh host ...` from hanging: with the
/// channel fds closed, sshd sees EOF and returns.
#[test]
fn stdio_redirected_so_launcher_pipe_closes() {
    use std::io::Read;
    use std::process::Stdio;

    let dir = tmpdir();
    let status = dir.join("status");
    let marker = dir.join("marker");

    let mut child = Command::new(BIN)
        .args(["detach-spawn", "--"])
        .args(["rt-runtime", "session", "--status"])
        .arg(&status)
        .arg("--marker")
        .arg(&marker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn launcher");

    // Read the launcher's stdout to EOF. If the long-lived session had
    // inherited this pipe, this read would block until the session died.
    let mut buf = Vec::new();
    let start = Instant::now();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut buf)
        .expect("read launcher stdout");
    let elapsed = start.elapsed();
    let _ = child.wait();

    assert!(
        elapsed < Duration::from_secs(2),
        "launcher pipe stayed open {elapsed:?} — session inherited stdio (ssh would hang)"
    );

    // And the session really is still running (so the fast EOF was redirection,
    // not the session having already exited).
    assert!(
        wait_for(Duration::from_secs(5), || status.exists()),
        "session never started"
    );
    let session_pid = read_kv(&status, "session_pid").expect("session_pid");
    assert!(alive(session_pid), "session should outlive the launcher pipe EOF");

    // Cleanup.
    unsafe {
        libc::kill(session_pid, libc::SIGTERM);
    }
    wait_for(Duration::from_secs(5), || !alive(session_pid));
    let _ = fs::remove_dir_all(&dir);
}
