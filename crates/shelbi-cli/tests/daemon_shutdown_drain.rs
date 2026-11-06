//! End-to-end tests for the daemon's shutdown drain: a real `shelbi daemon`
//! process in an isolated home, signalled the way `shelbi daemon restart` does,
//! with a slow mutation running in it.
//!
//! The slow mutation is a real `shelbi issue edit` on a GitHub board: its write
//! is a `gh` PATCH, and the stub `gh` on `PATH` holds that PATCH until the test
//! creates a `release` file. So each test decides exactly when the mutation
//! finishes, rather than racing a sleep, and waits on what the daemon and the
//! clients say on stderr rather than on how long something should have taken.

#![cfg(unix)]

use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use shelbi_client::{ClientError, ControlClient};
use shelbi_proto::control::{MutationError, MutationKind, MutationRequest};

const BIN: &str = env!("CARGO_BIN_EXE_shelbi");

/// How long anything here may take to happen. Generous, for a loaded machine: a
/// test only waits this long when it is about to fail.
const SETTLE: Duration = Duration::from_secs(60);

/// Where the started daemon's stderr goes, under the home.
const DAEMON_STDERR: &str = "daemon.stderr";

/// Where the stderr of the edit that sets param `name` goes, under the home.
fn edit_stderr(name: &str) -> String {
    format!("edit-{name}.stderr")
}

/// Answers the reads an issue edit makes. A PATCH announces itself in
/// `patch-started`, waits for `release`, then stores the body it was given and
/// counts itself in `patches`.
const GH_STUB: &str = r#"#!/bin/sh
D="$FAKE_GH_DIR"
escaped() { sed -e 's/\\/\\\\/g' -e 's/"/\\"/g' "$1" | awk '{printf "%s\\n", $0}'; }
labels='{"name":"shelbi:id/t"},{"name":"shelbi:status/todo"}'
node() {
  printf '{"number":7,"title":"Title","state":"OPEN","stateReason":null,"createdAt":"2026-08-01T00:00:00Z","updatedAt":"2026-08-02T00:00:00Z","body":"%s","assignees":{"nodes":[]},"labels":{"nodes":[%s]}}' \
    "$(escaped "$D/body")" "$labels"
}
rest() {
  printf '{"number":7,"title":"Title","state":"open","created_at":"2026-08-01T00:00:00Z","updated_at":"2026-08-02T00:00:00Z","body":"%s","assignees":[],"labels":[%s]}' \
    "$(escaped "$D/body")" "$labels"
}
limit='"rateLimit":{"remaining":4999,"resetAt":"2030-01-01T00:00:00Z"}'
case "$*" in
  *graphql*IdSearch*)
    printf '{"data":{%s,"search":{"nodes":[{"number":7}]}}}\n' "$limit"; exit 0 ;;
  *graphql*IssuesByNumber*)
    printf '{"data":{%s,"repository":{"i0":%s}}}\n' "$limit" "$(node)"; exit 0 ;;
  *graphql*)
    printf '{"data":{%s,"repository":{"issue":%s}}}\n' "$limit" "$(node)"; exit 0 ;;
esac
patch=false; include=false
for a in "$@"; do
  case "$a" in PATCH) patch=true ;; --include|-i) include=true ;; esac
done
$include && printf 'HTTP/2.0 200 OK\r\n\r\n'
if $patch; then
  echo started >> "$D/patch-started"
  n=0
  while [ ! -e "$D/release" ] && [ "$n" -lt 600 ]; do sleep 0.1; n=$((n + 1)); done
  while [ $# -gt 0 ]; do
    if [ "$1" = "-f" ]; then
      case "$2" in
        body=*) printf '%s' "${2#body=}" > "$D/body" ;;
      esac
      shift
    fi
    shift
  done
  echo patch >> "$D/patches"
  rest; echo; exit 0
fi
case "$*" in
  */labels*|*/comments*|*rate_limit*) exit 0 ;;
esac
rest; echo
"#;

/// An isolated home holding a GitHub-board project and a daemon started on it.
struct Home {
    path: PathBuf,
    hub: PathBuf,
    control: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new(tag: &str) -> Self {
        let pid = std::process::id();
        let path = PathBuf::from(format!("/tmp/shb-drain-{tag}-{pid}"));
        let _ = std::fs::remove_dir_all(&path);
        for dir in ["projects/p/tasks", "bin", "gh", "tmp"] {
            std::fs::create_dir_all(path.join(dir)).unwrap();
        }
        std::fs::write(
            path.join("projects/p.yaml"),
            "name: p\nrepo: /tmp/p\ndefault_branch: main\n\
             issue_tracker:\n  backend: github\n  github:\n    repo: owner/repo\n\
             orchestrator:\n  runner: claude\n\
             agent_runners:\n  claude:\n    command: claude\n    flags: []\n\
             machines:\n  - name: local\n    kind: local\n    work_dir: /tmp/p\n\
             workspaces:\n  - { name: dev, machine: local, runner: claude }\n",
        )
        .unwrap();
        // Keep the daemon alive (the idle monitor exits when no project is open).
        std::fs::write(path.join("projects/p/state.json"), br#"{"open":true}"#).unwrap();
        write_script(&path.join("bin/gh"), GH_STUB);
        // The daemon overlays the login shell's environment onto its own, which
        // would put the real `gh` and token back. This shell reports this home's.
        write_script(&path.join("bin/login-shell"), "#!/bin/sh\nexec env\n");
        std::fs::write(path.join("gh/body"), "Prose.\n").unwrap();

        let hub = PathBuf::from(format!("/tmp/shb-drain-{tag}-{pid}-h.sock"));
        let control = PathBuf::from(format!("/tmp/shb-drain-{tag}-{pid}-c.sock"));
        let _ = std::fs::remove_file(&hub);
        let _ = std::fs::remove_file(&control);
        Self {
            path,
            hub,
            control,
            daemon: None,
        }
    }

    /// `shelbi <args>` with this home's environment, and no pane's leaking in.
    fn shelbi(&self, args: &[&str]) -> Command {
        let mut cmd = Command::new(BIN);
        let path = format!(
            "{}:{}",
            self.path.join("bin").display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.args(args)
            .env("PATH", path)
            .env("SHELBI_HOME", &self.path)
            .env("SHELBI_HUB_SOCK", &self.hub)
            .env("SHELBI_CONTROL_SOCK", &self.control)
            .env("FAKE_GH_DIR", self.path.join("gh"))
            .env("SHELL", self.path.join("bin/login-shell"))
            .env("GH_TOKEN", "test-token")
            .env("TMPDIR", self.path.join("tmp"))
            .env("SHELBI_NO_RETIRE_UNITS", "1")
            .env("SHELBI_YES", "0")
            .env_remove("SHELBI_ROOT")
            .env_remove("SHELBI_PROJECT")
            .env_remove("SHELBI_HUB_ADDR")
            .stdin(Stdio::null());
        cmd
    }

    /// A file for a child's stderr, to read while the child still runs.
    fn stderr_file(&self, name: &str) -> std::fs::File {
        std::fs::File::create(self.path.join(name)).unwrap()
    }

    fn stderr_of(&self, name: &str) -> String {
        std::fs::read_to_string(self.path.join(name)).unwrap_or_default()
    }

    fn start_daemon(&mut self) {
        let child = self
            .shelbi(&["daemon"])
            .stdout(Stdio::null())
            .stderr(self.stderr_file(DAEMON_STDERR))
            .spawn()
            .expect("spawn shelbi daemon");
        self.daemon = Some(child);
        assert!(
            wait_until(SETTLE, || ControlClient::connect(&self.control, "test").is_ok()),
            "the daemon's control socket never came up"
        );
    }

    /// Start a `shelbi issue edit` that sets param `name`, its output captured.
    fn spawn_edit(&self, name: &str) -> Child {
        self.shelbi(&["--project", "p", "issue", "edit", "t", "--param", &format!("{name}=yes")])
            .stdout(Stdio::piped())
            .stderr(self.stderr_file(&edit_stderr(name)))
            .spawn()
            .expect("run shelbi issue edit")
    }

    fn gh_file(&self, name: &str) -> String {
        std::fs::read_to_string(self.path.join("gh").join(name)).unwrap_or_default()
    }

    fn patches(&self) -> usize {
        self.gh_file("patches").lines().count()
    }

    /// Wait for a slow edit to reach its `gh` PATCH, inside the daemon.
    fn wait_for_patch_to_start(&self) {
        assert!(
            wait_until(SETTLE, || !self.gh_file("patch-started").is_empty()),
            "the edit never reached its gh PATCH"
        );
    }

    /// Let every held PATCH, and every later one, finish.
    fn release_patches(&self) {
        std::fs::write(self.path.join("gh/release"), "").unwrap();
    }

    /// SIGTERM the daemon this test started, what `shelbi daemon restart` sends,
    /// and wait until it is draining its one running job. A signal lands in its
    /// own time: a request sent before this is admitted, correctly, and queues
    /// behind the held PATCH on the issue's lock.
    fn signal_daemon_and_wait_for_the_drain(&self) {
        let pid = self.daemon.as_ref().expect("a started daemon").id() as libc::pid_t;
        // SAFETY: kill with a real signal to a child this test spawned and hasn't
        // reaped, so the pid is still that child's.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGTERM) }, 0);
        assert!(
            wait_until(SETTLE, || self.stderr_of(DAEMON_STDERR).contains("waiting for 1 job(s)")),
            "the daemon never began draining: {}",
            self.stderr_of(DAEMON_STDERR)
        );
    }

    fn daemon_exited(&mut self) -> bool {
        let daemon = self.daemon.as_mut().expect("a started daemon");
        daemon.try_wait().expect("poll the daemon").is_some()
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Never strand a held PATCH (and the job waiting on it) past the test.
        let _ = std::fs::write(self.path.join("gh/release"), "");
        if let Some(mut d) = self.daemon.take() {
            let _ = d.kill();
            let _ = d.wait();
        }
        // A client may have started the next daemon, detached. This home's own
        // pid record names it; this stops nothing else.
        let _ = self.shelbi(&["daemon", "stop"]).stdout(Stdio::null()).stderr(Stdio::null()).status();
        let _ = std::fs::remove_dir_all(&self.path);
        let _ = std::fs::remove_file(&self.hub);
        let _ = std::fs::remove_file(&self.control);
    }
}

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

fn wait_until(deadline: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

fn refused(result: Result<Option<shelbi_proto::control::ExpectedState>, ClientError>) -> bool {
    matches!(result, Err(ClientError::Mutation(MutationError::ShuttingDown)))
}

fn unassign(request_id: u64) -> MutationRequest {
    MutationRequest {
        request_id,
        project: "p".into(),
        id: "t".into(),
        expected: None,
        kind: MutationKind::Unassign,
    }
}

/// The reported bug: a restart during a merge or board move left it half-applied,
/// because the daemon exited while the job's thread was still running.
#[test]
fn a_slow_mutation_finishes_before_the_daemon_exits() {
    let mut home = Home::new("slow");
    home.start_daemon();
    let edit = home.spawn_edit("task");
    home.wait_for_patch_to_start();

    home.signal_daemon_and_wait_for_the_drain();
    // Well past the old 3s drain, still held mid-PATCH. A daemon that exited
    // here took the job with it.
    std::thread::sleep(Duration::from_millis(4000));
    assert!(!home.daemon_exited(), "the daemon must wait for its running mutation");
    assert_eq!(home.patches(), 0, "the PATCH is still held");

    home.release_patches();
    assert!(
        wait_until(SETTLE, || home.daemon_exited()),
        "the daemon never exited once its job was done"
    );
    assert_eq!(home.patches(), 1, "the mutation completed before the process exited");
    assert!(home.gh_file("body").contains("task: yes"), "{}", home.gh_file("body"));

    // And the client heard the result, rather than losing it to the exit.
    let out = edit.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("t edited (params)"),
        "stdout:\n{stdout}\nstderr:\n{}",
        home.stderr_of(&edit_stderr("task"))
    );
    assert!(
        !home.control.exists() && !home.hub.exists(),
        "a clean exit removes its sockets"
    );
}

#[test]
fn a_new_mutation_during_the_drain_gets_a_retryable_refusal() {
    let mut home = Home::new("refuse");
    home.start_daemon();
    let mut connected_before = ControlClient::connect(&home.control, "test").unwrap();
    let edit = home.spawn_edit("task");
    home.wait_for_patch_to_start();
    home.signal_daemon_and_wait_for_the_drain();

    // Both an open connection and a new one are told to retry.
    assert!(refused(connected_before.mutate(&unassign(2), &mut |_, _| {})));
    let mut connected_during = ControlClient::connect(&home.control, "test").unwrap();
    assert!(refused(connected_during.mutate(&unassign(3), &mut |_, _| {})));

    home.release_patches();
    assert!(wait_until(SETTLE, || home.daemon_exited()));
    assert_eq!(home.patches(), 1, "only the mutation from before the shutdown ran");
    assert!(edit.wait_with_output().unwrap().status.success());
}

/// The refusal's point: a client that gets it waits, and its request lands on
/// the daemon that replaces this one.
#[test]
fn a_client_refused_during_the_drain_lands_on_the_next_daemon() {
    let mut home = Home::new("retry");
    home.start_daemon();
    let first = home.spawn_edit("task");
    home.wait_for_patch_to_start();
    home.signal_daemon_and_wait_for_the_drain();

    let second = home.spawn_edit("other");
    // Seen waiting, not failing, while the first job holds the drain.
    assert!(
        wait_until(SETTLE, || home.stderr_of(&edit_stderr("other")).contains("restarting")),
        "the second edit was never refused: {}",
        home.stderr_of(&edit_stderr("other"))
    );
    home.release_patches();
    assert!(wait_until(SETTLE, || home.daemon_exited()));

    let first = first.wait_with_output().unwrap();
    assert!(first.status.success(), "{}", home.stderr_of(&edit_stderr("task")));
    let second = second.wait_with_output().unwrap();
    assert!(second.status.success(), "stderr:\n{}", home.stderr_of(&edit_stderr("other")));

    assert_eq!(home.patches(), 2, "both landed, each exactly once");
    assert!(home.gh_file("body").contains("other: yes"), "{}", home.gh_file("body"));
}

/// Every client's liveness probe takes the daemon's real bind lock for a moment.
/// A daemon starting in that moment used to exit, "another daemon holds it", and
/// the desktop app showed its failed-to-start screen.
///
/// Probes keep coming here, from before the spawn until the daemon is up, so the
/// test doesn't depend on how long the daemon takes to reach the lock. Without a
/// retry it often finds the lock busy and exits, so this catches a regression
/// most runs rather than every run (`acquire_bind_lock`'s own unit test is the
/// deterministic check); with a retry it always gets through a gap.
#[test]
fn probes_taking_the_bind_lock_do_not_make_a_starting_daemon_exit() {
    let mut home = Home::new("probe");
    let mut lock_path = home.hub.clone().into_os_string();
    lock_path.push(".lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .unwrap();
    let done = Arc::new(AtomicBool::new(false));
    let prober = {
        let done = done.clone();
        std::thread::spawn(move || {
            while !done.load(Ordering::SeqCst) {
                // SAFETY: flock on a valid fd this closure owns. Once the daemon
                // holds the lock this simply fails, as any prober's would.
                let held = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
                std::thread::sleep(Duration::from_millis(30));
                if held {
                    // SAFETY: same fd, still open and owned.
                    unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        })
    };
    let child = home
        .shelbi(&["daemon"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn shelbi daemon");
    home.daemon = Some(child);

    let settled = wait_until(SETTLE, || {
        ControlClient::connect(&home.control, "test").is_ok() || home.daemon_exited()
    });
    done.store(true, Ordering::SeqCst);
    prober.join().unwrap();
    assert!(settled, "the daemon neither came up nor exited");
    assert!(!home.daemon_exited(), "a momentary lock holder must not make the daemon exit");
    let _ = std::fs::remove_file(PathBuf::from(lock_path));
}
