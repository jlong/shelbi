//! `shelbi relay`'s v2 services end to end, through a [`RemoteLink`], with
//! only `ssh` stubbed.
//!
//! The link runs the real [`relay_command`] argv with a stub `ssh` in place of
//! the real one: the stub runs the far command locally (as `sshd` would hand
//! it to the user's shell), so the real `FIND_SHELBI` script finds the built
//! `shelbi`, which serves the relay against a sandboxed Shelbi home with one
//! project. Its `query` answers come from the CLI's real readers. No daemon
//! runs (and none is started); the far `hub.sock` is a test listener. Nothing
//! reaches a real host, the real `~/.shelbi` or the real hub.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::time::Duration;

use shelbi_app::launch::ssh::relay_command;
use shelbi_app::launch::RemoteLink;
use shelbi_app::source::wire::{Answer, Query};
use shelbi_client::relay::services::subscribe_line;
use shelbi_client::{ClientError, Transport};

const STUB_SSH: &str = r#"#!/bin/sh
while [ $# -gt 0 ]; do
  case "$1" in
    -o) shift 2 ;;
    --) shift; break ;;
    -*) shift ;;
    *) host=$1; shift ;;
  esac
done
exec /bin/sh -c "$*"
"#;

/// A throwaway far machine: a home, a Shelbi home with one project, the stub
/// `ssh` and a `shelbi` on its PATH.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // Short: a socket lives in here (macOS caps socket paths).
        let root = std::env::temp_dir().join(format!(
            "rs-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("bin")).unwrap();
        let root = root.canonicalize().unwrap();
        let bin = root.join("bin");
        std::fs::write(bin.join("ssh"), STUB_SSH).unwrap();
        std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_shelbi"), bin.join("shelbi")).unwrap();

        let shelbi_home = root.join("sh");
        let repo = root.join("repos").join("alpha");
        std::fs::create_dir_all(&repo).unwrap();
        let projects = shelbi_state::projects_dir_in(&shelbi_home);
        std::fs::create_dir_all(&projects).unwrap();
        let repo_s = repo.display();
        std::fs::write(
            projects.join("alpha.yaml"),
            format!(
                "name: alpha\nrepo: {repo_s}\nmachines:\n  - {{ name: hub, kind: local, work_dir: {repo_s} }}\n\
                 orchestrator: {{ runner: claude }}\nagent_runners:\n  claude: {{ command: claude, flags: [] }}\n\
                 workspaces: []\n"
            ),
        )
        .unwrap();
        Sandbox { root }
    }

    fn shelbi_home(&self) -> PathBuf {
        self.root.join("sh")
    }

    /// The link a remote window would open to this machine, over the stub.
    fn link(&self) -> RemoteLink {
        let mut argv = relay_command("fakehost");
        argv[0] = self.root.join("bin").join("ssh").display().to_string();
        let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.root.join("bin").display());
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..])
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("USER", "tester")
            .env("SHELL", "/bin/sh")
            .env("SHELBI_HOME", self.shelbi_home())
            .env("SHELBI_HUB_SOCK", self.shelbi_home().join("hub.sock"))
            .env("SHELBI_CONTROL_SOCK", self.shelbi_home().join("control.sock"))
            .env("SHELBI_ASKPASS", "never");
        RemoteLink::spawn("fakehost", cmd).expect("link to the sandbox relay")
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn queries_are_answered_by_the_far_readers() {
    let sandbox = Sandbox::new();
    let link = sandbox.link();
    assert!(link.supports_services(), "relay v{}", link.relay_version());

    let mut querier = link.querier().unwrap();
    assert_eq!(
        querier.ask(Query::ProjectExists { project: "alpha".into() }).unwrap(),
        Answer::ProjectExists(true)
    );
    assert_eq!(
        querier.ask(Query::ProjectExists { project: "nope".into() }).unwrap(),
        Answer::ProjectExists(false)
    );
    match querier.ask(Query::ConfigSkeleton { project: "alpha".into() }).unwrap() {
        Answer::ConfigSkeleton(Some(model)) => assert_eq!(model.project_label, "alpha"),
        other => panic!("expected alpha's skeleton, got {other:?}"),
    }
    assert!(matches!(
        querier.ask(Query::ConfigSkeleton { project: "nope".into() }).unwrap(),
        Answer::ConfigSkeleton(None)
    ));
    assert!(matches!(
        querier.ask(Query::OpenBoard { project: "alpha".into() }).unwrap(),
        Answer::OpenBoard(_)
    ));
    assert!(matches!(
        querier.ask(Query::Changelog { project: "alpha".into() }).unwrap(),
        Answer::Changelog(_)
    ));
    // Nothing the queries did started a daemon.
    assert!(!sandbox.shelbi_home().join("shelbi.pid").exists());
}

#[test]
fn control_needs_the_far_daemon_and_never_starts_it() {
    let sandbox = Sandbox::new();
    let link = sandbox.link();
    let err = link.open_control("alpha").err().expect("no daemon is running");
    assert!(
        matches!(&err, ClientError::Relay(why) if why.contains("isn't running")),
        "{err:?}"
    );
    assert!(link.channel().is_reachable(), "a failed open keeps the link");
    assert!(!sandbox.shelbi_home().join("shelbi.pid").exists());
    assert!(!sandbox.shelbi_home().join("control.sock").exists());
}

#[test]
fn changes_subscribes_on_the_far_hub_and_forwards_nothing_else() {
    let sandbox = Sandbox::new();
    std::fs::create_dir_all(sandbox.shelbi_home()).unwrap();
    let listener = UnixListener::bind(sandbox.shelbi_home().join("hub.sock")).unwrap();
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let (conn, _) = listener.accept().unwrap();
        let mut write = conn.try_clone().unwrap();
        let mut reader = BufReader::new(conn);
        let mut received = String::new();
        reader.read_line(&mut received).unwrap();
        write
            .write_all(b"{\"change\":\"board\",\"project\":\"alpha\"}\n")
            .unwrap();
        let _ = reader.read_to_string(&mut received);
        let _ = tx.send(received);
    });

    let link = sandbox.link();
    let stream = link.open_changes("alpha").expect("open changes");
    let (mut read, mut write, _shutdown) = Box::new(stream).split().unwrap();
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while !line.ends_with(b"\n") {
        assert_eq!(read.read(&mut byte).unwrap(), 1, "changes stream closed early");
        line.push(byte[0]);
    }
    assert_eq!(line, b"{\"change\":\"board\",\"project\":\"alpha\"}\n");

    write
        .write_all(b"{\"verb\":\"event\",\"project\":\"alpha\",\"line\":\"forged=1\"}\n")
        .unwrap();
    drop(write);
    let received = rx.recv_timeout(Duration::from_secs(20)).expect("hub connection closed");
    assert_eq!(received, subscribe_line("alpha"));
}
