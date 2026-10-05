//! Regression (`rt-login-env-capture-empty-path`): the login-shell environment
//! capture must succeed when the *caller* owns a controlling terminal — the
//! single-process TUI opening a project — not wedge until the deadline.
//!
//! The capture runs `$SHELL -l -i -c env`. An interactive (`-i`) shell enables
//! job control, so if it is merely placed in its own process group on a
//! controlling tty it becomes a *background* group and stops on SIGTTOU/SIGTTIN
//! the moment its rc touches terminal modes. The fix detaches the capture shell
//! into a brand-new session with no controlling tty (`setsid`), so the capture
//! returns the real environment promptly.
//!
//! This test is the only one in its binary on purpose: it `fork(2)`s to isolate
//! a child that acquires a pty as its controlling terminal before running the
//! capture, and keeping the binary single-test keeps that fork clear of other
//! test threads.

use std::time::{Duration, Instant};

/// A child that owns a controlling pty captures the login env (with a PATH) well
/// within the capture deadline, instead of stopping on job-control signals.
#[test]
fn capture_under_a_controlling_tty_gets_path_without_timeout() {
    // Where the child reports its verdict ("ok" / "fail:<why>").
    let out = tempfile::tempdir().expect("tempdir");
    let result_path = out.path().join("verdict");

    // A pty: the child will make the slave its controlling terminal.
    let (master, slave) = open_pty();

    // SAFETY: single-threaded fork isolation (this is the only test in the
    // binary). The child only calls async-signal-safe libc before handing off to
    // the capture, which it runs in its fresh session.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed: {}", std::io::Error::last_os_error());

    if pid == 0 {
        // ---- child ----
        unsafe {
            // New session (drops any inherited controlling tty), then claim the
            // pty slave as our controlling terminal and point stdio at it, so we
            // genuinely own a controlling tty like the TUI does.
            libc::setsid();
            libc::ioctl(slave, libc::TIOCSCTTY as _, 0);
            libc::dup2(slave, libc::STDIN_FILENO);
            libc::dup2(slave, libc::STDOUT_FILENO);
            libc::dup2(slave, libc::STDERR_FILENO);
            libc::close(master);
            if slave > libc::STDERR_FILENO {
                libc::close(slave);
            }
        }

        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let verdict = match shelbi_core::capture_env_bounded(&shell, Duration::from_secs(8)) {
            Some(env) => match env.get("PATH") {
                Some(p) if !p.is_empty() => "ok".to_string(),
                _ => "fail:no-path".to_string(),
            },
            None => "fail:timed-out".to_string(),
        };
        let _ = std::fs::write(&result_path, verdict);
        unsafe { libc::_exit(0) };
    }

    // ---- parent ----
    unsafe { libc::close(slave) };
    // Drain the master so the child never blocks writing to the pty.
    let drain = std::thread::spawn(move || {
        use std::io::Read;
        use std::os::unix::io::FromRawFd;
        let mut f = unsafe { std::fs::File::from_raw_fd(master) };
        let mut sink = Vec::new();
        let _ = f.read_to_end(&mut sink);
    });

    // Wait for the child with a hard ceiling so a wedged capture can't hang the
    // suite (the capture's own deadline is 8s; give it generous headroom).
    let exited = wait_for_child(pid, Duration::from_secs(25));
    if !exited {
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
        drop(drain);
        panic!("the capture never returned under a controlling tty (it wedged)");
    }

    let verdict = std::fs::read_to_string(&result_path).unwrap_or_else(|e| format!("fail:no-file:{e}"));
    assert_eq!(
        verdict, "ok",
        "capture under a controlling tty must yield a PATH without timing out (got {verdict:?})"
    );
}

/// `openpty(3)` → (master, slave) raw fds.
fn open_pty() -> (libc::c_int, libc::c_int) {
    let mut master: libc::c_int = -1;
    let mut slave: libc::c_int = -1;
    let rc = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, 0, "openpty failed: {}", std::io::Error::last_os_error());
    (master, slave)
}

/// Reap `pid` within `timeout`, returning whether it exited in time.
fn wait_for_child(pid: libc::pid_t, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        let mut status = 0;
        let rc = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
        if rc == pid {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
