//! The orchestrator handoff across real launches and attaches: `shelbi
//! orchestrate` (the bootstrap every client runs) against a genuine `shelbi
//! __session` with a stub runner, in an isolated `SHELBI_HOME`.
//!
//! The failure paths (a launch that doesn't come up restores the handoff) are
//! covered with a stub backend in `shelbi-orchestrator`, where the daemon's
//! supervisor can't race the assertion by claiming the file for its own relaunch.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use shelbi_core::Host;
use shelbi_orchestrator::session_backend::{backend, Liveness, SessionBackend, SessionTarget};

struct Sandbox {
    home: PathBuf,
}

impl Sandbox {
    /// A project `p` whose orchestrator runner is a stub that just stays alive.
    fn new() -> Self {
        // Short and under /tmp: session sockets live under the home, and macOS
        // caps a socket path at 104 bytes.
        let home = PathBuf::from(format!("/tmp/shb-oh-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let agent_dir = home.join("projects/p/agents/orchestrator");
        std::fs::create_dir_all(&agent_dir).unwrap();
        std::fs::write(agent_dir.join("instructions.md"), "# orchestrator\n").unwrap();
        std::fs::create_dir_all(home.join("repo")).unwrap();

        let stub = home.join("stub-runner.sh");
        shelbi_state::test_support::write_executable(&stub, "#!/bin/sh\nexec sleep 300\n");
        std::fs::write(
            home.join("projects/p.yaml"),
            format!(
                "name: p\nrepo: {repo}\ndefault_branch: main\n\
                 orchestrator:\n  runner: stub\n\
                 agent_runners:\n  stub:\n    command: {stub}\n    flags: []\n\
                 machines:\n  - name: local\n    kind: local\n    work_dir: {repo}\n\
                 workspaces: []\n",
                repo = home.join("repo").display(),
                stub = stub.display(),
            ),
        )
        .unwrap();
        // The in-process backend below addresses this home's sessions.
        std::env::set_var("SHELBI_HOME", &home);
        Self { home }
    }

    fn handoff(&self) -> PathBuf {
        self.home.join("projects/p/agents/orchestrator/handoff.md")
    }

    fn instructions(&self) -> String {
        std::fs::read_to_string(self.home.join("projects/p/.claude/agent-instructions.md"))
            .unwrap()
    }

    fn orchestrate(&self) -> String {
        let output: Output = Command::new(env!("CARGO_BIN_EXE_shelbi"))
            .args(["--project", "p", "orchestrate"])
            .env("SHELBI_HOME", &self.home)
            .env("SHELL", "/bin/sh")
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env("SHELBI_DAEMON_IDLE_GRACE_MS", "400")
            .env("SHELBI_DAEMON_IDLE_POLL_MS", "200")
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_HUB_SOCK")
            .env_remove("SHELBI_HUB_ADDR")
            .env_remove("SHELBI_CONTROL_SOCK")
            .output()
            .expect("run shelbi orchestrate");
        assert!(
            output.status.success(),
            "orchestrate failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn end_session(&self) {
        let _ = backend().kill(&Host::Local, &target());
        let deadline = Instant::now() + Duration::from_secs(10);
        while backend().probe(&Host::Local, &target(), None) != Liveness::Dead {
            assert!(Instant::now() < deadline, "the orchestrator session should exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let _ = backend().kill(&Host::Local, &target());
        // Closing the project lets this home's daemon idle out by itself.
        let _ = std::fs::write(self.home.join("projects/p/state.json"), br#"{"open":false}"#);
        wait_for_daemon_exit(&self.home.join("hub.sock"));
        let _ = std::fs::remove_dir_all(&self.home);
        std::env::remove_var("SHELBI_HOME");
    }
}

fn target() -> SessionTarget {
    SessionTarget::session("shelbi-p")
}

fn wait_for_daemon_exit(sock: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while sock.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn only_a_launch_consumes_the_handoff_and_an_attach_leaves_it_for_the_next_one() {
    let sandbox = Sandbox::new();

    // A cold launch ingests the waiting handoff and spends it once it is up.
    std::fs::write(sandbox.handoff(), "first handoff\n").unwrap();
    assert!(sandbox.orchestrate().contains("orchestrator started"));
    assert!(sandbox.instructions().contains("<system-reminder>\nfirst handoff\n</system-reminder>"));
    assert!(!sandbox.handoff().exists(), "a ready launch deletes the handoff");
    assert!(
        !sandbox.handoff().with_extension("md.claimed").exists(),
        "a ready launch leaves no claim behind"
    );

    // `shelbi reload` signals clients while the old orchestrator is still up: the
    // re-exec'd client's bootstrap attaches, and the handoff survives it.
    std::fs::write(sandbox.handoff(), "second handoff\n").unwrap();
    let deployed = sandbox.instructions();
    assert!(sandbox.orchestrate().contains("already running"));
    assert_eq!(std::fs::read_to_string(sandbox.handoff()).unwrap(), "second handoff\n");
    assert_eq!(sandbox.instructions(), deployed, "an attach deploys nothing");

    // The replacement launch then gets it.
    sandbox.end_session();
    assert!(sandbox.orchestrate().contains("orchestrator started"));
    assert!(sandbox.instructions().contains("second handoff"));
    assert!(!sandbox.handoff().exists());
}
