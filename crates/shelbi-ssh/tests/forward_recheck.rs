//! The reverse-forward recheck against a fake `ssh` on `PATH`, so these assert
//! what actually reaches the ssh boundary: above all, that a healthy master never
//! sees `-O exit`. In its own test binary because `PATH` and `SHELBI_HOME` are
//! process-wide. The probe scripts themselves run against real sockets in the
//! crate's unit tests; here the fake just answers with their exit codes.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use shelbi_core::{ForwardMode, Host};
use shelbi_ssh::{build_command, ensure_reverse_forward, recheck_reverse_forward};
use shelbi_state::{load_host_forward, save_host_forward, HostForward};

/// Plays the remote host and its ControlMaster, driven by files in
/// `$FAKE_SSH_STATE`: `master` exists while a master is up and holds the `-R` it
/// opened with, `probe_exit` is what the forward probe returns, `socket_exit` is
/// what `test -S`/`test -w` return after a repair, and `tcp_stderr` makes a TCP
/// master open fail.
const FAKE_SSH: &str = r#"#!/bin/sh
state=$FAKE_SSH_STATE
printf '%s\n' "$*" >> "$state/log"
read_or() { if [ -e "$state/$1" ]; then cat "$state/$1"; else echo "$2"; fi; }
case " $* " in
  *" -O check "*) [ -e "$state/master" ]; exit $? ;;
  *" -O exit "*) rm -f "$state/master"; exit 0 ;;
  *shelbi-forward-probe*) exit "$(read_or probe_exit 0)" ;;
  *" rm -f "*) exit 0 ;;
  *" test -S "*|*" test -w "*) exit "$(read_or socket_exit 0)" ;;
esac
spec=
prev=
for arg in "$@"; do
  [ "$prev" = "-R" ] && spec=$arg
  prev=$arg
done
if [ -n "$spec" ]; then
  case "$spec" in
    127.0.0.1:*) if [ -e "$state/tcp_stderr" ]; then cat "$state/tcp_stderr" >&2; exit 255; fi ;;
  esac
  printf '%s\n' "$spec" > "$state/master"
fi
exit 0
"#;

/// One test's isolated world. Holding it keeps other tests off the process env.
struct World {
    state: PathBuf,
    home: PathBuf,
    _guard: MutexGuard<'static, ()>,
}

fn world(tag: &str) -> World {
    static LOCK: Mutex<()> = Mutex::new(());
    static PATH_ONCE: OnceLock<()> = OnceLock::new();
    let guard = LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let root = PathBuf::from(format!("/tmp/shfr-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let (state, home) = (root.join("state"), root.join("home"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(home.join("ssh")).unwrap();

    PATH_ONCE.get_or_init(|| {
        let bin = PathBuf::from(format!("/tmp/shfr-{}-bin", std::process::id()));
        std::fs::create_dir_all(&bin).unwrap();
        let ssh = bin.join("ssh");
        shelbi_state::test_support::write_executable(&ssh, FAKE_SSH);
        let path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{path}", bin.display()));
        std::env::set_var("SHELBI_FORWARD_RETRY_ATTEMPTS", "1");
        std::env::set_var("SHELBI_FORWARD_RETRY_BACKOFF_MS", "0");
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_BASE", "47100");
        std::env::set_var("SHELBI_TCP_FORWARD_PORT_SPAN", "2");
        std::env::remove_var("SHELBI_REMOTE_HUB_SOCK");
        // Inside a worker pane these point at the live hub. Events must land in
        // this test's SHELBI_HOME instead.
        for var in ["SHELBI_HUB_SOCK", "SHELBI_HUB_ADDR", "SHELBI_CONTROL_SOCK"] {
            std::env::remove_var(var);
        }
    });
    std::env::set_var("FAKE_SSH_STATE", &state);
    std::env::set_var("SHELBI_HOME", &home);
    World {
        state,
        home,
        _guard: guard,
    }
}

impl World {
    fn set(&self, file: &str, value: &str) {
        std::fs::write(self.state.join(file), value).unwrap();
    }
    fn master_up_with(&self, spec: &str) {
        self.set("master", spec);
    }
    fn master_spec(&self) -> Option<String> {
        read(&self.state.join("master")).map(|s| s.trim().to_string())
    }
    fn log(&self) -> Vec<String> {
        read(&self.state.join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
    fn count(&self, needle: &str) -> usize {
        self.log().iter().filter(|l| l.contains(needle)).count()
    }
    fn drops(&self) -> usize {
        self.count(" -O exit ")
    }
    fn events(&self) -> String {
        read(&self.home.join("events.log")).unwrap_or_default()
    }
}

impl Drop for World {
    fn drop(&mut self) {
        if let Some(root) = self.state.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn ssh(host: &str) -> Host {
    Host::Ssh { host: host.into() }
}

fn tcp_record(port: u16) -> Option<HostForward> {
    Some(HostForward {
        mode: ForwardMode::Tcp,
        port: Some(port),
    })
}

/// The `-R` spec a default Unix forward carries.
fn unix_spec() -> String {
    shelbi_state::reverse_forward_spec()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[test]
fn a_healthy_unix_master_is_left_alone() {
    let w = world("healthy-unix");
    w.master_up_with(&unix_spec());

    ensure_reverse_forward(&ssh("healthy-unix"), None).unwrap();

    assert_eq!(w.drops(), 0, "a healthy master must not be dropped: {:?}", w.log());
    assert_eq!(w.count(" rm -f "), 0, "nothing to clean up: {:?}", w.log());
    assert_eq!(w.count("StreamLocalBindUnlink"), 0, "nothing to reopen: {:?}", w.log());
    assert_eq!(w.master_spec(), Some(unix_spec()));
}

#[test]
fn a_healthy_tcp_master_is_left_alone_and_the_record_is_not_rewritten() {
    let w = world("healthy-tcp");
    let host = "healthy-tcp";
    save_host_forward(host, tcp_record(47100)).unwrap();
    w.master_up_with("127.0.0.1:47100:hub.sock");
    let state_path = shelbi_state::forward_state_path().unwrap();
    let before = std::fs::metadata(&state_path).unwrap().modified().unwrap();

    ensure_reverse_forward(&ssh(host), None).unwrap();

    assert_eq!(w.drops(), 0, "{:?}", w.log());
    assert_eq!(w.count("ExitOnForwardFailure=yes"), 0, "no sweep: {:?}", w.log());
    assert_eq!(std::fs::metadata(&state_path).unwrap().modified().unwrap(), before);
    assert!(
        w.log().iter().any(|l| l.contains("shelbi-forward-probe") && l.contains("47100")),
        "the probe targets the recorded port: {:?}",
        w.log()
    );
}

#[test]
fn a_failed_probe_with_no_answer_does_not_drop_a_live_master() {
    let w = world("inconclusive");
    w.master_up_with(&unix_spec());
    w.set("probe_exit", "255");

    ensure_reverse_forward(&ssh("inconclusive"), None).unwrap();

    assert_eq!(w.drops(), 0, "{:?}", w.log());
}

#[test]
fn a_stale_landing_socket_is_repaired() {
    let w = world("stale-socket");
    w.master_up_with(&unix_spec());
    w.set("probe_exit", "12");

    ensure_reverse_forward(&ssh("stale-socket"), None).unwrap();

    assert!(w.drops() >= 1, "must drop the master: {:?}", w.log());
    assert_eq!(w.count(" rm -f "), 1, "{:?}", w.log());
    assert_eq!(w.count("StreamLocalBindUnlink=yes"), 1, "{:?}", w.log());
    assert_eq!(w.master_spec(), Some(unix_spec()));
}

#[test]
fn a_dead_master_is_reopened_without_probing() {
    let w = world("dead-master");

    ensure_reverse_forward(&ssh("dead-master"), None).unwrap();

    assert_eq!(w.count("shelbi-forward-probe"), 0, "{:?}", w.log());
    assert_eq!(w.master_spec(), Some(unix_spec()));
}

#[test]
fn an_unusable_landing_socket_still_falls_back_to_tcp_under_auto() {
    let w = world("wedge-auto");
    let host = "wedge-auto";
    w.master_up_with(&unix_spec());
    w.set("probe_exit", "11");
    w.set("socket_exit", "1");

    ensure_reverse_forward(&ssh(host), None).unwrap();

    assert_eq!(load_host_forward(host), tcp_record(47100));
    assert!(w.master_spec().unwrap().starts_with("127.0.0.1:47100:"));
    assert!(w.events().contains("action=falling_back_to_tcp"), "{}", w.events());
}

#[test]
fn a_broken_tcp_forward_is_rebound() {
    let w = world("broken-tcp");
    let host = "broken-tcp";
    save_host_forward(host, tcp_record(47101)).unwrap();
    w.master_up_with("127.0.0.1:47101:hub.sock");
    w.set("probe_exit", "12");

    ensure_reverse_forward(&ssh(host), None).unwrap();

    assert!(w.drops() >= 1, "{:?}", w.log());
    assert_eq!(load_host_forward(host), tcp_record(47101));
    assert!(w.master_spec().unwrap().starts_with("127.0.0.1:47101:"));
}

#[test]
fn a_tcp_pin_with_no_port_yet_sweeps_for_one() {
    let w = world("tcp-pin");
    let host = "tcp-pin";
    w.master_up_with(&unix_spec());

    ensure_reverse_forward(&ssh(host), Some(ForwardMode::Tcp)).unwrap();

    assert_eq!(w.count("shelbi-forward-probe"), 0, "no port to probe: {:?}", w.log());
    assert_eq!(load_host_forward(host), tcp_record(47100));
}

#[test]
fn a_unix_pin_overrides_an_earlier_tcp_fallback() {
    let w = world("unix-pin");
    let host = "unix-pin";
    save_host_forward(host, tcp_record(47100)).unwrap();
    w.master_up_with("127.0.0.1:47100:hub.sock");
    w.set("probe_exit", "10");

    // The master still carries the old TCP forward, and no landing socket exists.
    ensure_reverse_forward(&ssh(host), Some(ForwardMode::Unix)).unwrap();

    assert_eq!(load_host_forward(host), None, "the pin clears the record");
    assert_eq!(w.master_spec(), Some(unix_spec()), "reopened on the unix forward");
    assert!(w.events().contains("detail=pin_cleared_tcp_record"), "{}", w.events());

    // Every other reader now agrees, with no pin in hand.
    let cmd = build_command(&ssh(host), ["true"]);
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().into_owned()).collect();
    let r = args.iter().position(|a| a == "-R").expect("argv carries -R");
    assert_eq!(args[r + 1], unix_spec());
    assert!(matches!(
        shelbi_state::remote_hub_endpoint(host, None),
        shelbi_state::HubEndpoint::Unix(_)
    ));
}

#[test]
fn a_unix_pin_that_is_wedged_fails_without_falling_back_to_tcp() {
    let w = world("unix-pin-wedged");
    let host = "unix-pin-wedged";
    w.master_up_with(&unix_spec());
    w.set("probe_exit", "11");
    w.set("socket_exit", "1");

    let err = ensure_reverse_forward(&ssh(host), Some(ForwardMode::Unix));

    assert!(err.is_err(), "a pin is respected even when it's failing");
    assert_eq!(load_host_forward(host), None);
    assert_eq!(w.count("ExitOnForwardFailure=yes"), 0, "no sweep: {:?}", w.log());
}

#[test]
fn many_workspaces_on_one_host_share_one_check_per_interval() {
    let w = world("gate");
    let host = "gate";
    w.master_up_with(&unix_spec());
    let interval = Duration::from_secs(600);

    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                recheck_reverse_forward(&ssh(host), None, interval).unwrap();
            });
        }
    });
    // The next poll tick, still inside the interval.
    recheck_reverse_forward(&ssh(host), None, interval).unwrap();

    assert_eq!(w.count(" -O check "), 1, "{:?}", w.log());
    assert_eq!(w.count("shelbi-forward-probe"), 1, "{:?}", w.log());
    assert_eq!(w.drops(), 0, "{:?}", w.log());

    // A different host has its own stamp.
    w.set("log", "");
    recheck_reverse_forward(&ssh("gate-other"), None, interval).unwrap();
    assert_eq!(w.count(" -O check "), 1, "{:?}", w.log());

    // Once the interval has passed the check runs again.
    w.set("log", "");
    recheck_reverse_forward(&ssh(host), None, Duration::ZERO).unwrap();
    assert_eq!(w.count(" -O check "), 1, "{:?}", w.log());
}

#[test]
fn a_skipped_recheck_still_applies_the_pin() {
    let w = world("gate-pin");
    let host = "gate-pin";
    w.master_up_with(&unix_spec());
    let interval = Duration::from_secs(600);
    recheck_reverse_forward(&ssh(host), None, interval).unwrap();
    save_host_forward(host, tcp_record(47100)).unwrap();
    w.set("log", "");

    recheck_reverse_forward(&ssh(host), Some(ForwardMode::Unix), interval).unwrap();

    assert_eq!(w.log(), Vec::<String>::new(), "the check itself was skipped");
    assert_eq!(load_host_forward(host), None);
}
