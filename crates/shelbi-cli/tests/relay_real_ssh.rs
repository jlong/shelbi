//! A remote window's relay link against a real `sshd` and a real `ssh`, for
//! what only they can prove: a host that wants something typed asks once, and
//! the link really rides the connection the connect steps signed in.
//!
//! Off unless `SHELBI_TEST_REAL_SSHD=1`, since it needs an unprivileged `sshd`
//! (macOS and most Linux have one, CI's sandboxes may not). An unprivileged
//! sshd can't check a password, so the sign-in is the task's other case: a key
//! whose passphrase isn't in any agent, answered by a stand-in askpass that
//! counts how often it was asked. It runs a sandboxed `shelbi relay` through
//! `ForceCommand`, on its own port and in its own directory. Nothing reaches
//! the real `~/.shelbi`, the real hub or the user's `~/.ssh`.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use shelbi_app::launch::ssh::{relay_command, step_command, SshAuth};
use shelbi_app::launch::RemoteLink;
use shelbi_app::source::wire::{Answer, Query};
use shelbi_ssh::askpass::{run_until, Askpass};

const SSHD: &str = "/usr/sbin/sshd";
const HOST: &str = "lab";

fn enabled() -> bool {
    std::env::var("SHELBI_TEST_REAL_SSHD").as_deref() == Ok("1") && Path::new(SSHD).exists()
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// A throwaway far machine behind its own `sshd`.
struct Lab {
    root: PathBuf,
    sshd: Child,
    forward_port: u16,
    /// `ControlPersist`, in place of the real ten minutes.
    persist: Option<&'static str>,
}

impl Lab {
    fn new(max_sessions: u32) -> Lab {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        // Short and literal: the shared connection's socket lives in here, and
        // macOS's temp dir leaves too little of the socket path limit.
        let root = PathBuf::from(format!("/tmp/rsd-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (port, forward_port) = (free_port(), free_port());
        let at = {
            let root = root.clone();
            move |name: &str| root.join(name).display().to_string()
        };

        for (key, phrase) in [("hostkey", ""), ("userkey", "sesame")] {
            let made = Command::new("ssh-keygen")
                .args(["-q", "-t", "ed25519", "-N", phrase, "-f", &at(key)])
                .status()
                .unwrap();
            assert!(made.success());
        }
        std::fs::copy(at("userkey.pub"), at("authorized_keys")).unwrap();

        let home = root.join("sh");
        let repo = root.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let projects = shelbi_state::projects_dir_in(&home);
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

        // The far side: the relay's own script names `relay`, a step's doesn't.
        write_script(
            &root.join("far.sh"),
            &format!(
                "#!/bin/sh\ncase \"$SSH_ORIGINAL_COMMAND\" in\n\
                 *relay*) echo $$ > {pid}; exec env SHELBI_HOME={home} SHELBI_HUB_SOCK={home}/hub.sock \
                 SHELBI_CONTROL_SOCK={home}/control.sock {bin} relay ;;\n\
                 *) exec /bin/sh -c \"$SSH_ORIGINAL_COMMAND\" ;;\nesac\n",
                pid = at("relay.pid"),
                home = home.display(),
                bin = env!("CARGO_BIN_EXE_shelbi"),
            ),
        );
        write_script(
            &root.join("askpass"),
            &format!("#!/bin/sh\necho asked >> {}\necho sesame\n", at("asked")),
        );
        std::fs::write(
            at("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {host_key}\nPidFile {pid}\n\
                 AuthorizedKeysFile {keys}\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n\
                 StrictModes no\nMaxSessions {max_sessions}\nForceCommand {far}\n",
                host_key = at("hostkey"),
                pid = at("sshd.pid"),
                keys = at("authorized_keys"),
                far = at("far.sh"),
            ),
        )
        .unwrap();
        // As a user's own config might: a forward the relay must not set up.
        std::fs::write(
            at("ssh_config"),
            format!(
                "Host {HOST}\n  HostName 127.0.0.1\n  Port {port}\n  IdentityFile {key}\n  IdentitiesOnly yes\n  \
                 IdentityAgent none\n  StrictHostKeyChecking no\n  UserKnownHostsFile {known}\n  \
                 LocalForward 127.0.0.1:{forward_port} 127.0.0.1:{port}\n",
                key = at("userkey"),
                known = at("known_hosts"),
            ),
        )
        .unwrap();

        let sshd = Command::new(SSHD)
            .args(["-D", "-f", &at("sshd_config"), "-E", &at("sshd.log")])
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let lab = Lab { root, sshd, forward_port, persist: None };
        let until = Instant::now() + Duration::from_secs(5);
        while std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
            assert!(Instant::now() < until, "sshd didn't start: {:?}", std::fs::read_to_string(at("sshd.log")));
            std::thread::sleep(Duration::from_millis(20));
        }
        lab
    }

    fn path(&self, name: &str) -> String {
        self.root.join(name).display().to_string()
    }

    fn auth(&self) -> SshAuth {
        SshAuth::Ask(Askpass::new(self.path("askpass")).with_host(HOST))
    }

    fn control_path(&self) -> String {
        self.path("r-%C")
    }

    /// `argv` as a `Command`, reading the lab's config rather than the user's.
    fn ssh(&self, argv: &[String]) -> Command {
        let mut cmd = Command::new(&argv[0]);
        cmd.args(["-F", &self.path("ssh_config")]);
        for arg in &argv[1..] {
            match self.persist {
                Some(persist) if arg.starts_with("ControlPersist=") => cmd.arg(format!("ControlPersist={persist}")),
                _ => cmd.arg(arg),
            };
        }
        cmd
    }

    /// One connect step: what `OpenSsh` runs.
    fn step(&self, script: &str) -> String {
        let auth = self.auth();
        let argv = step_command(HOST, script, &auth, Some(&self.control_path()));
        let out = run_until(&mut self.ssh(&argv), auth.askpass(), Duration::from_secs(20)).unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }

    /// The link a native window would open now.
    fn link(&self) -> RemoteLink {
        let auth = self.auth();
        let argv = relay_command(HOST, &auth, Some(&self.control_path()));
        RemoteLink::spawn_asking(HOST, self.ssh(&argv), auth.askpass()).expect("the link opens")
    }

    /// How often the user has been asked.
    fn asked(&self) -> usize {
        std::fs::read_to_string(self.path("asked")).map_or(0, |text| text.lines().count())
    }

    fn master(&self, op: &str) -> bool {
        Command::new("ssh")
            .args(["-F", &self.path("ssh_config"), "-o", &format!("ControlPath={}", self.control_path())])
            .args(["-O", op, HOST])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    fn forward_is_up(&self) -> bool {
        std::net::TcpStream::connect(("127.0.0.1", self.forward_port)).is_ok()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        self.master("exit");
        let _ = self.sshd.kill();
        let _ = self.sshd.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn write_script(path: &Path, text: &str) {
    shelbi_state::test_support::write_executable(path, text);
}

fn settle(what: &str, mut done: impl FnMut() -> bool) {
    let until = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < until, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn serves_the_project(link: &RemoteLink) {
    let mut querier = link.querier().unwrap();
    assert!(matches!(
        querier.ask(Query::ProjectExists { project: "alpha".into() }).unwrap(),
        Answer::ProjectExists(true)
    ));
}

#[test]
fn a_link_rides_the_steps_sign_in_and_carries_no_forward() {
    if !enabled() {
        return;
    }
    let lab = Lab::new(10);
    assert_eq!(lab.step("echo signed-in").trim(), "signed-in");
    assert_eq!(lab.asked(), 1, "the first step asks");
    assert!(!lab.forward_is_up(), "a step's connection sets up no config forward");

    let link = lab.link();
    serves_the_project(&link);
    lab.step("true");
    assert_eq!(lab.asked(), 1, "nothing asks again");
    assert!(!lab.forward_is_up(), "the link sets up none either");
    assert!(link.is_reachable());
}

#[test]
fn a_link_with_no_connection_to_ride_asks_once_and_opens_it() {
    if !enabled() {
        return;
    }
    let lab = Lab::new(10);
    let link = lab.link();
    serves_the_project(&link);
    assert_eq!(lab.asked(), 1);
    assert!(lab.master("check"), "later steps have it to ride");
    lab.step("true");
    assert_eq!(lab.asked(), 1);
    assert!(!lab.forward_is_up());
}

#[test]
fn a_closed_connection_ends_the_link_at_once_rather_than_hanging_it() {
    if !enabled() {
        return;
    }
    let lab = Lab::new(10);
    lab.step("true");
    let link = lab.link();
    serves_the_project(&link);
    assert!(lab.master("exit"));
    settle("the link to read as down", || !link.is_reachable());
    assert_eq!(lab.asked(), 1);

    // The reconnect: its step signs in afresh, once, and the new link rides it.
    lab.step("true");
    assert_eq!(lab.asked(), 2);
    serves_the_project(&lab.link());
    assert_eq!(lab.asked(), 2);
}

#[test]
fn control_persist_never_ends_a_link_that_rides_the_connection() {
    if !enabled() {
        return;
    }
    let mut lab = Lab::new(10);
    lab.persist = Some("1");
    lab.step("true");
    let link = lab.link();
    std::thread::sleep(Duration::from_secs(4));
    assert!(link.is_reachable());
    serves_the_project(&link);
    assert!(lab.master("check"));
    assert_eq!(lab.asked(), 1);

    // The timer runs once nothing rides it: the connection then goes by itself.
    drop(link);
    settle("the idle connection to end", || !lab.master("check"));
}

#[test]
fn dropping_a_link_ends_the_far_relay_and_keeps_the_connection() {
    if !enabled() {
        return;
    }
    let lab = Lab::new(10);
    lab.step("true");
    let link = lab.link();
    serves_the_project(&link);
    let pid = std::fs::read_to_string(lab.path("relay.pid")).unwrap();
    let alive = || {
        Command::new("kill")
            .args(["-0", pid.trim()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive());
    drop(link);
    settle("the far relay to end", || !alive());
    assert!(lab.master("check"), "the connection stays for the next step");
}

#[test]
fn a_host_that_refuses_another_session_gets_a_link_of_its_own() {
    if !enabled() {
        return;
    }
    let lab = Lab::new(1);
    lab.step("true");
    // A terminal holding the one session sshd allows on that connection.
    let auth = lab.auth();
    let argv = step_command(HOST, "sleep 30", &auth, Some(&lab.control_path()));
    let mut held = lab.ssh(&argv);
    if let Some(askpass) = auth.askpass() {
        askpass.apply(&mut held);
    }
    let mut held = held.stdin(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    std::thread::sleep(Duration::from_millis(500));

    let link = lab.link();
    serves_the_project(&link);
    assert_eq!(lab.asked(), 2, "its own connection, its own one sign-in");
    drop(link);
    let _ = held.kill();
    let _ = held.wait();
}
