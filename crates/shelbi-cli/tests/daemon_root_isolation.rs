//! Real-binary tests for the "a worker pane took down the hub" incident
//! (`worker-kill-took-down-hub`).
//!
//! * A daemon stop is **root-scoped**: `shelbi daemon stop` under a sandbox
//!   `SHELBI_ROOT` never signals the daemon another root's `shelbi.pid` records.
//! * Stopping the daemon is **not fatal**: SIGTERM to the daemon leaves every
//!   session running, and a fresh on-demand daemon sees them live.
//! * A session host that *is* signalled says who did it in `exit.json`.
//!
//! Every process here runs under its own short `/tmp` root with a scrubbed
//! environment (`env_clear` + HOME/PATH/USER/SHELBI_ROOT), so neither the
//! developer's live `~/.shelbi`, its hub socket, nor its daemon lock is ever in
//! reach — even when this suite runs from inside a Shelbi worker pane that
//! exports `SHELBI_HUB_SOCK`. The only pids signalled are ones these tests
//! spawned (or read from their own sandbox's pid file).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// A sandbox root: a short `/tmp` dir (Unix socket paths cap at ~104 bytes on
/// macOS) holding one open filesystem project `p`, so a daemon started here
/// doesn't idle-exit. Removed, and its daemon reaped, on drop.
struct Root {
    path: PathBuf,
    daemon: Option<Child>,
}

impl Root {
    fn new(tag: &str) -> Self {
        let path = PathBuf::from(format!("/tmp/shb-iso-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(path.join("projects/p/tasks")).unwrap();
        std::fs::write(
            path.join("projects/p.yaml"),
            "name: p\nrepo: /tmp/p\ndefault_branch: main\n\
             orchestrator:\n  runner: fake\n\
             agent_runners:\n  fake:\n    command: sleep\n    flags: [\"100000\"]\n\
             machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/p\n\
             workspaces:\n  - { name: dev, machine: local }\n",
        )
        .unwrap();
        std::fs::write(path.join("projects/p/state.json"), br#"{"open":true}"#).unwrap();
        Self { path, daemon: None }
    }

    /// A `shelbi` invocation confined to this root: a scrubbed environment, so
    /// no hub-socket / control-socket / project env from the caller leaks in.
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(BIN);
        cmd.env_clear()
            .env("HOME", std::env::var_os("HOME").unwrap_or_default())
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("USER", std::env::var_os("USER").unwrap_or_default())
            .env("SHELBI_ROOT", &self.path)
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env("SHELBI_YES", "0")
            .stdin(Stdio::null());
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().expect("run shelbi")
    }

    fn hub_sock(&self) -> PathBuf {
        self.path.join("hub.sock")
    }

    /// Start `shelbi daemon` for this root as our child and wait until it has
    /// recorded its pid and answers on its socket.
    fn start_daemon(&mut self) -> i32 {
        let child = self
            .cmd()
            .arg("daemon")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn shelbi daemon");
        let pid = child.id() as i32;
        self.daemon = Some(child);
        assert!(
            wait_until(Duration::from_secs(30), || self.recorded_pid() == Some(pid)
                && UnixStream::connect(self.hub_sock()).is_ok()),
            "daemon for {} never came up",
            self.path.display()
        );
        pid
    }

    /// The pid this root's `shelbi.pid` records.
    fn recorded_pid(&self) -> Option<i32> {
        let text = std::fs::read_to_string(self.path.join("shelbi.pid")).ok()?;
        text.split_whitespace().next()?.parse().ok()
    }

    /// Wait for our daemon child to exit (reaping it), returning whether it did.
    fn wait_daemon_exit(&mut self, timeout: Duration) -> bool {
        let Some(child) = self.daemon.as_mut() else {
            return true;
        };
        let exited = wait_until(timeout, || matches!(child.try_wait(), Ok(Some(_))));
        if exited {
            self.daemon = None;
        }
        exited
    }

    fn daemon_alive(&mut self) -> bool {
        match self.daemon.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        // A daemon this root started on demand (not our child) is reaped by pid
        // from this root's own pid file — never anything else.
        if let Some(pid) = self.recorded_pid() {
            if pid > 1 {
                unsafe { libc::kill(pid, libc::SIGKILL) };
            }
        }
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    loop {
        if cond() {
            return true;
        }
        if start.elapsed() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn alive(pid: i32) -> bool {
    pid > 1 && unsafe { libc::kill(pid, 0) == 0 }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Spawn a session named `name` under `root` running `sleep`, returning its dir.
fn new_session(root: &Root, name: &str) -> PathBuf {
    let out = root.run(&["session", "new", "--name", name, "--", "/bin/sleep", "300"]);
    assert!(out.status.success(), "session new failed: {}", stderr(&out));
    let sessions = root.path.join("sessions");
    let mut found = None;
    assert!(
        wait_until(Duration::from_secs(10), || {
            found = std::fs::read_dir(&sessions).ok().and_then(|dirs| {
                dirs.flatten().map(|d| d.path()).find(|d| {
                    std::fs::read_to_string(d.join("meta.json"))
                        .ok()
                        .and_then(|m| serde_json::from_str::<serde_json::Value>(&m).ok())
                        .is_some_and(|m| m["name"] == name)
                        && d.join("sock").exists()
                })
            });
            found.is_some()
        }),
        "session {name} never came up"
    );
    found.unwrap()
}

fn session_pid(dir: &Path) -> i32 {
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("meta.json")).unwrap()).unwrap();
    meta["pid"].as_i64().expect("meta.json pid") as i32
}

/// Whether the session at `dir` answers the protocol hello — i.e. is attachable.
fn session_answers_hello(dir: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(dir.join("sock")) else {
        return false;
    };
    stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let hello = shelbi_proto::Frame::Hello(shelbi_proto::Hello {
        protocol_version: shelbi_proto::PROTOCOL_VERSION,
        colors: None,
        capabilities: vec![],
    })
    .encode()
    .unwrap();
    if stream.write_all(&hello).is_err() {
        return false;
    }
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        if let Ok((shelbi_proto::Frame::Hello(_), _)) = shelbi_proto::Frame::decode(&buf) {
            return true;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return false,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
    false
}

/// SIGKILL a session host we spawned (by the pid its own meta.json records).
fn reap_session(dir: &Path) {
    let pid = session_pid(dir);
    if pid > 1 {
        unsafe { libc::kill(pid, libc::SIGKILL) };
    }
}

#[test]
fn sandboxed_daemon_stop_never_signals_another_roots_daemon() {
    // "Real" root: a live daemon recorded in its own shelbi.pid.
    let mut real = Root::new("real");
    let real_pid = real.start_daemon();

    // Sandbox root, nothing running. `daemon stop` there is a no-op.
    let sandbox = Root::new("sbx");
    let out = sandbox.run(&["daemon", "stop"]);
    assert!(out.status.success(), "sandbox stop failed: {}", stderr(&out));
    assert!(stdout(&out).contains("not running"), "{}", stdout(&out));
    assert!(real.daemon_alive(), "sandbox stop must leave the real daemon alone");

    // Even a sandbox whose shelbi.pid names the real daemon (a copied or stale
    // record) never signals it: this root's lock is free and its socket silent,
    // so stop never acts on a pid at all.
    std::fs::copy(real.path.join("shelbi.pid"), sandbox.path.join("shelbi.pid")).unwrap();
    let out = sandbox.run(&["daemon", "stop"]);
    assert!(out.status.success(), "sandbox stop failed: {}", stderr(&out));
    assert!(stdout(&out).contains("not running"), "{}", stdout(&out));
    std::thread::sleep(Duration::from_millis(300));
    assert!(real.daemon_alive(), "a foreign pid in the sandbox record must not be signalled");
    assert!(alive(real_pid));

    // A leaked hub-socket env (sandbox root, but SHELBI_HUB_SOCK still pointing
    // at the real hub) with the sandbox recording some other process: the
    // socket answers as the real daemon, the record disagrees, so stop refuses
    // and signals neither.
    let mut decoy = Command::new("/bin/sleep").arg("60").spawn().unwrap();
    std::fs::write(sandbox.path.join("shelbi.pid"), format!("{}\n", decoy.id())).unwrap();
    let out = sandbox
        .cmd()
        .args(["daemon", "stop"])
        .env("SHELBI_HUB_SOCK", real.hub_sock())
        .output()
        .unwrap();
    assert!(!out.status.success(), "a socket/root disagreement must refuse");
    assert!(stderr(&out).contains("refusing"), "{}", stderr(&out));
    assert!(real.daemon_alive(), "refused stop must not touch the real daemon");
    assert!(matches!(decoy.try_wait(), Ok(None)), "refused stop must not touch the decoy");
    let _ = decoy.kill();
    let _ = decoy.wait();

    // The real root's own stop does stop its own daemon.
    let out = real.run(&["daemon", "stop"]);
    assert!(out.status.success(), "real stop failed: {}", stderr(&out));
    assert!(stdout(&out).contains(&format!("pid {real_pid}")), "{}", stdout(&out));
    assert!(real.wait_daemon_exit(Duration::from_secs(10)), "daemon did not exit");
}

#[test]
fn daemon_sigterm_leaves_sessions_alive_and_a_fresh_daemon_finds_them() {
    let mut root = Root::new("survive");
    let daemon_pid = root.start_daemon();

    // A session for a project that is NOT open (`q`), so its daemon watchdog
    // never starts a daemon of its own behind the test's back.
    let session = new_session(&root, "q/ws/dev");
    let host_pid = session_pid(&session);
    assert!(session_answers_hello(&session));

    // SIGTERM the daemon — the pid this root recorded, which is our child.
    assert_eq!(root.recorded_pid(), Some(daemon_pid));
    unsafe { libc::kill(daemon_pid, libc::SIGTERM) };
    assert!(root.wait_daemon_exit(Duration::from_secs(15)), "daemon ignored SIGTERM");

    // The session outlived it: still running, still attachable, no exit record.
    std::thread::sleep(Duration::from_millis(300));
    assert!(alive(host_pid), "session host died with the daemon");
    assert!(!session.join("exit.json").exists(), "session wrote an exit record");
    assert!(session_answers_hello(&session), "session no longer attachable");

    // The on-demand restart brings up a fresh daemon that lists it live.
    let out = root.run(&["__ensure-daemon"]);
    assert!(out.status.success(), "ensure-daemon failed: {}", stderr(&out));
    let new_pid = root.recorded_pid().expect("restarted daemon recorded its pid");
    assert_ne!(new_pid, daemon_pid, "a fresh daemon should be running");
    let ls = root.run(&["session", "ls"]);
    let listing = stdout(&ls);
    let line = listing
        .lines()
        .find(|l| l.starts_with("q/ws/dev"))
        .unwrap_or_else(|| panic!("session missing from ls:\n{listing}"));
    assert!(line.contains("live"), "session not live under the new daemon: {line}");
    assert!(session_answers_hello(&session));

    reap_session(&session);
    let out = root.run(&["daemon", "stop"]);
    assert!(out.status.success(), "stop failed: {}", stderr(&out));
}

#[test]
fn a_signalled_session_records_the_signal_and_its_sender() {
    let root = Root::new("sender");
    let session = new_session(&root, "q/ws/sig");
    let host_pid = session_pid(&session);

    // A sender that stays alive past the signal, so its command is resolvable.
    let mut sender = Command::new("/bin/sh")
        .arg("-c")
        .arg(format!("kill -TERM {host_pid}; sleep 5"))
        .spawn()
        .unwrap();
    let sender_pid = sender.id() as i64;

    let exit = session.join("exit.json");
    assert!(
        wait_until(Duration::from_secs(10), || exit.exists()
            && std::fs::read_to_string(&exit).map(|t| t.ends_with('}')).unwrap_or(false)),
        "session never wrote exit.json"
    );
    let rec: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&exit).unwrap()).unwrap();
    let _ = sender.kill();
    let _ = sender.wait();

    assert_eq!(rec["signal"], libc::SIGTERM, "{rec}");
    assert_eq!(rec["sender_pid"].as_i64(), Some(sender_pid), "{rec}");
    let reason = rec["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("SIGTERM") && reason.contains(&format!("pid {sender_pid}")),
        "{reason}"
    );
    assert!(
        rec["sender"].as_str().is_some_and(|s| s.contains("sh")),
        "sender command not resolved: {rec}"
    );
}
