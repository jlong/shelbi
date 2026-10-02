//! End-to-end check of the `shelbi __review-serve` lifecycle wrapper: it must
//! start the serve command in its own session (so a single `kill(-pgid)` reaps
//! the whole tree) and persist that pgid to `$SHELBI_REVIEW_PGID_FILE` so any
//! teardown path can find it — the two properties the review-server leak fix
//! depends on.

#![cfg(unix)]

use std::process::Command;
use std::time::{Duration, Instant};

fn alive(pid: i32) -> bool {
    // Safety: signal 0 only probes existence/permission.
    unsafe { libc::kill(pid, 0) == 0 }
}

/// Session id of `pid` via `getsid(2)`, or -1 on error.
fn sid_of(pid: i32) -> i32 {
    // Safety: pure query, touches no memory.
    unsafe { libc::getsid(pid) }
}

#[test]
fn wrapper_records_pgid_and_runs_the_server_in_its_own_session() {
    let dir = std::env::temp_dir().join(format!(
        "shelbi-review-serve-test-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let pgid_file = dir.join("review-serve.pgid");

    // Launch the wrapper around a stub server (`sleep`) in the BACKGROUND — the
    // agent runs it as a managed background server, and the wrapper blocks in
    // wait() for as long as the server lives.
    let mut wrapper = Command::new(env!("CARGO_BIN_EXE_shelbi"))
        .args(["__review-serve", "--", "sleep", "30"])
        .env("SHELBI_REVIEW_PGID_FILE", &pgid_file)
        .spawn()
        .expect("spawn shelbi __review-serve");

    // The wrapper writes the pgid as soon as it has spawned the child.
    let deadline = Instant::now() + Duration::from_secs(10);
    let pgid = loop {
        if let Ok(s) = std::fs::read_to_string(&pgid_file) {
            if let Ok(p) = s.trim().parse::<i32>() {
                break p;
            }
        }
        assert!(
            Instant::now() < deadline,
            "wrapper never wrote the pgid file"
        );
        std::thread::sleep(Duration::from_millis(25));
    };

    assert!(pgid > 1, "recorded pgid must be a real group id, got {pgid}");
    assert!(alive(pgid), "the recorded group leader must be running");
    // setsid made the server a session leader, so its session id == its pid
    // (== the recorded pgid) and is NOT this test's session — i.e. it really
    // detached into its own session/group.
    assert_eq!(
        sid_of(pgid),
        pgid,
        "the server must be its own session leader (setsid)"
    );
    assert_ne!(
        sid_of(pgid),
        sid_of(std::process::id() as i32),
        "the server must not share the test's session"
    );

    // Reap the whole group the way teardown does, then clean up.
    // Safety: negative pid signals the process group.
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let _ = wrapper.wait();
    let _ = std::fs::remove_dir_all(&dir);
}
