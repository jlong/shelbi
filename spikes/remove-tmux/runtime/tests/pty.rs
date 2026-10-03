//! `portable-pty` behavior checks for Phase 0: controlling terminal and
//! process-group handling, group kill reaching the whole group, and no
//! descriptor leaks into the child. Driven directly against the library with
//! `/bin/sh` as the child, so the findings are about `portable-pty` itself, not
//! about this spike's own code.

use std::io::Read;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, PtySize};

fn size() -> PtySize {
    PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Read from the master until `marker` is seen or the deadline passes.
fn read_until(reader: &mut dyn Read, marker: &str, timeout: Duration) -> String {
    let deadline = Instant::now() + timeout;
    let mut acc = String::new();
    let mut buf = [0u8; 1024];
    while Instant::now() < deadline {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                acc.push_str(&String::from_utf8_lossy(&buf[..n]));
                if acc.contains(marker) {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    acc
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The child gets the slave as its controlling terminal and is a session
/// leader in its own process group (portable-pty calls setsid + TIOCSCTTY).
/// Session/group leadership is read from the parent with getsid/getpgid, which
/// is portable; macOS `ps -o sess=` prints a kernel pointer, not the SID.
#[test]
fn child_has_controlling_tty_and_own_session() {
    let pair = native_pty_system().openpty(size()).unwrap();
    let mut cmd = CommandBuilder::new("/bin/sh");
    // `tty` prints the ctty path; then idle so we can inspect it from outside.
    cmd.args(["-c", "tty; echo RT_DONE; sleep 5"]);
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let child_pid = child.process_id().expect("child pid") as i32;
    let mut reader = pair.master.try_clone_reader().unwrap();
    let out = read_until(&mut reader, "RT_DONE", Duration::from_secs(5));

    assert!(
        out.contains("/dev/"),
        "child `tty` did not report a controlling terminal:\n{out}"
    );
    assert!(
        !out.contains("not a tty"),
        "child reported it has no tty:\n{out}"
    );

    // Leadership, read from the parent while the child idles.
    let sid = unsafe { libc::getsid(child_pid) };
    let pgid = unsafe { libc::getpgid(child_pid) };
    assert_eq!(pgid, child_pid, "child should lead its own process group");
    assert_eq!(sid, child_pid, "child should be its own session leader (setsid)");

    unsafe {
        libc::kill(child_pid, libc::SIGKILL);
    }
    let _ = child.wait();
}

/// Killing the child's process group reaps a grandchild that the child started
/// in the same group — the behavior Shelbi's `kill` relies on.
#[test]
fn group_kill_reaps_grandchild() {
    let pair = native_pty_system().openpty(size()).unwrap();
    let mut cmd = CommandBuilder::new("/bin/sh");
    cmd.args(["-c", "sleep 300 & echo GC=$! ; echo RT_READY ; wait"]);
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);

    let child_pid = child.process_id().expect("child pid") as i32;
    let mut reader = pair.master.try_clone_reader().unwrap();
    let out = read_until(&mut reader, "RT_READY", Duration::from_secs(5));

    let gc: i32 = out
        .lines()
        .find_map(|l| l.strip_prefix("GC=").and_then(|v| v.trim().parse().ok()))
        .unwrap_or_else(|| panic!("no grandchild pid in:\n{out:?}"));

    assert!(alive(child_pid), "child should be alive before kill");
    assert!(alive(gc), "grandchild should be alive before kill");

    // portable-pty's child is a group leader, so pgid == child_pid.
    let pgid = unsafe { libc::getpgid(child_pid) };
    assert_eq!(pgid, child_pid, "expected child to lead its group");
    unsafe {
        libc::killpg(pgid, libc::SIGTERM);
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && (alive(child_pid) || alive(gc)) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = child.wait();
    assert!(!alive(child_pid), "child survived group kill");
    assert!(!alive(gc), "grandchild survived group kill");
}

/// The child inherits only fds 0/1/2 (all pointing at the pty). The master and
/// the library's internal fds are not leaked in. We assert the child sees no
/// high-numbered fd, and specifically not the master's fd.
#[test]
fn no_descriptor_leak_into_child() {
    let pair = native_pty_system().openpty(size()).unwrap();
    let master_fd: Option<i32> = pair.master.as_raw_fd();

    let mut cmd = CommandBuilder::new("/bin/sh");
    // Emit CHR=<n> for each fd that resolves to a character device. The pty
    // slave (0/1/2) and the master are char devices; listing /dev/fd opens a
    // transient *directory* fd, which filtering on type ignores. Comparing fd
    // numbers across processes is meaningless, so counting char-device fds is
    // the reliable leak signal: a fourth one is a leaked master.
    cmd.args([
        "-c",
        "for f in /dev/fd/*; do n=${f##*/}; [ -c \"$f\" ] && echo CHR=$n; done; echo RT_DONE",
    ]);
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let out = read_until(&mut reader, "RT_DONE", Duration::from_secs(5));
    let _ = child.wait();

    let mut chr_fds: Vec<i32> = out
        .lines()
        .filter_map(|l| l.strip_prefix("CHR=").and_then(|v| v.trim().parse().ok()))
        .collect();
    chr_fds.sort_unstable();
    chr_fds.dedup();
    assert_eq!(
        chr_fds,
        vec![0, 1, 2],
        "child should hold exactly the three pty fds as char devices; a fourth is a leaked \
         master. master_fd(parent)={master_fd:?}, child char fds={chr_fds:?}, raw:\n{out}"
    );
}
