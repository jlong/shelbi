//! `shelbi remote connect` end to end, with only `ssh` stubbed.
//!
//! A stub `ssh` on `PATH` stands in for the network: it logs its argv, and
//! runs the remote command *locally* (the way `sshd` would hand it to the
//! user's shell), against a sandboxed Shelbi home with its own hub socket.
//! So the real `OpenSsh` transport, the real probe script, the real
//! `shelbi remote list --json` and the real parsing all run. The terminal
//! step (`ssh -t`) exits 0 at once, as if the user quit the far Shelbi, so no
//! TUI ever starts. Nothing reaches a real host, the real `~/.shelbi` or the
//! real hub.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};

const STUB_SSH: &str = r#"#!/bin/sh
printf '%s' "$*" | tr '\n' ' ' >> "$STUB_SSH_LOG"
echo >> "$STUB_SSH_LOG"
tty=0
while [ $# -gt 0 ]; do
  case "$1" in
    -t) tty=1; shift ;;
    -o) shift 2 ;;
    --) shift; break ;;
    -*) shift ;;
    *) host=$1; shift ;;
  esac
done
if [ "$host" = down ]; then
  echo "ssh: connect to host down port 22: Connection refused" >&2
  exit 255
fi
if [ "$tty" = 1 ]; then exit 0; fi
exec /bin/sh -c "$*"
"#;

/// A throwaway machine: a home, a Shelbi home with one project, the stub
/// `ssh` and a `shelbi` on its PATH.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new() -> Sandbox {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // Short: the hub socket lives in here (macOS caps socket paths).
        let root = std::env::temp_dir().join(format!(
            "rc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&root);
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let root = root.canonicalize().unwrap();
        let bin = root.join("bin");
        std::fs::write(bin.join("ssh"), STUB_SSH).unwrap();
        std::fs::set_permissions(bin.join("ssh"), std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_shelbi"), bin.join("shelbi")).unwrap();

        let shelbi_home = root.join("sh");
        let repo = root.join("repos").join("alpha");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(shelbi_home.join("projects")).unwrap();
        let repo_s = repo.display();
        std::fs::write(
            shelbi_home.join("projects").join("alpha.yaml"),
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

    fn hub_sock(&self) -> PathBuf {
        self.shelbi_home().join("hub.sock")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.root.join("ssh.log")).unwrap_or_default()
    }

    /// Answer every hub probe, as a running daemon does.
    fn run_daemon(&self) {
        let listener = UnixListener::bind(self.hub_sock()).unwrap();
        std::thread::spawn(move || {
            use std::io::Write;
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let _ = stream.write_all(b"ok\n");
            }
        });
    }

    /// `shelbi <args>` on this machine, with nothing from the real one.
    fn shelbi(&self, args: &[&str]) -> Output {
        let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", self.root.join("bin").display());
        Command::new(env!("CARGO_BIN_EXE_shelbi"))
            .args(args)
            .env_clear()
            .env("PATH", path)
            .env("HOME", &self.root)
            .env("USER", "tester")
            .env("SHELL", "/bin/sh")
            .env("SHELBI_HOME", self.shelbi_home())
            .env("SHELBI_HUB_SOCK", self.hub_sock())
            .env("SHELBI_CONTROL_SOCK", self.shelbi_home().join("control.sock"))
            .env("STUB_SSH_LOG", self.root.join("ssh.log"))
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn recents(home: &Path) -> String {
    std::fs::read_to_string(home.join("recent-remotes.json")).unwrap_or_default()
}

#[test]
fn connecting_opens_the_hosts_shelbi_and_remembers_the_session() {
    let sandbox = Sandbox::new();
    sandbox.run_daemon();

    let out = sandbox.shelbi(&["remote", "connect", "fakehost"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stderr(&out).contains("Disconnected from fakehost"), "{}", stderr(&out));

    let log = sandbox.log();
    let runs: Vec<&str> = log.lines().collect();
    // Find, connect (each a probe), then the terminal.
    assert_eq!(runs.len(), 3, "{log}");
    for run in &runs {
        assert!(run.contains("BatchMode=yes"), "never prompts: {run}");
        assert!(!run.contains(" -R "), "never forwards this hub: {run}");
    }
    assert!(runs[0].contains("remote list --json"));
    assert!(runs[2].starts_with("-t "), "the terminal gets a tty: {}", runs[2]);
    assert!(runs[2].contains("--project alpha"), "{}", runs[2]);

    let saved = recents(&sandbox.shelbi_home());
    assert!(saved.contains("\"host\": \"fakehost\""), "{saved}");
    assert!(saved.contains("\"user\": \"tester\""), "{saved}");
    assert!(saved.contains("\"project\": \"alpha\""), "{saved}");
    assert!(!saved.to_lowercase().contains("password"), "{saved}");
}

#[test]
fn a_stopped_daemon_is_reported_and_never_started_unasked() {
    let sandbox = Sandbox::new();

    let out = sandbox.shelbi(&["remote", "connect", "fakehost", "alpha"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("Shelbi isn't running on fakehost. Run again with --yes to start it."),
        "{}",
        stderr(&out)
    );
    assert!(!sandbox.log().contains("__ensure-daemon"));
    assert!(!sandbox.log().contains("-t "), "no terminal without a daemon");
    assert!(!sandbox.shelbi_home().join("shelbi.pid").exists());
    assert_eq!(recents(&sandbox.shelbi_home()), "", "a refused connection isn't remembered");
}

#[test]
fn an_unreachable_host_says_why() {
    let sandbox = Sandbox::new();
    let out = sandbox.shelbi(&["remote", "connect", "down"]);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("Couldn't connect to down: connect to host down port 22: Connection refused"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn the_listing_starts_nothing() {
    let sandbox = Sandbox::new();
    let out = sandbox.shelbi(&["remote", "list", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(json["user"], "tester");
    assert_eq!(json["daemon_running"], false);
    assert_eq!(json["projects"][0]["name"], "alpha");
    assert!(!sandbox.shelbi_home().join("shelbi.pid").exists());
    assert!(!sandbox.hub_sock().exists());
}
