//! The real [`RemoteConnector`]: Shelbi on another machine, over the user's
//! own `ssh`.
//!
//! Every step is one `ssh <host>` running a short `sh` script on the far
//! side. The script finds the host's `shelbi` the way `shelbi machine setup`
//! does (the interactive login shell's PATH, then `~/.shelbi/bin/shelbi`) and
//! asks it `shelbi remote list --json` ([`RemoteListing`]): the user it runs
//! as, its version, whether its daemon is running, and its projects.
//!
//! - **Credentials.** `ssh` runs with `BatchMode=yes`, so it signs in with the
//!   user's keys, agent and `~/.ssh/config` and never prompts; a host that
//!   wants a password fails with [`RemoteError::AuthFailed`]. Shelbi stores
//!   nothing but the host as typed (in the recent remote sessions).
//! - **No hub forward.** Unlike the hub's own machine connections
//!   (`shelbi_ssh`), these never carry the `-R` forward of this machine's hub
//!   socket: the far side has its own Shelbi, and its `hub.sock` must stay its
//!   own.
//! - **The daemon.** Finding and connecting only read; the far daemon is
//!   started only by [`RemoteConnector::start_daemon`], which a client calls
//!   when the user asks ([`super::remote::START_DAEMON_LABEL`]).
//! - **Opening a session** runs the host's own TUI for the project in a
//!   terminal ([`terminal_command`]); the far side keeps every session
//!   running when the connection drops, so a client reconnects by running it
//!   again ([`exit_meaning`]).
//!
//! The transport is a seam ([`SshTransport`]): tests drive [`SshRemote`]
//! through a scripted fake and never reach a real host.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use super::remote::{validate_host, RemoteConnector, RemoteError, RemoteSession};

/// The `remote list --json` shape this Shelbi reads. A host speaking another
/// is refused as a version mismatch.
pub const REMOTE_PROTOCOL: u32 = 1;

/// How long one `ssh` step may take before it is abandoned as unreachable.
/// Generous, since the far side starts an interactive login shell.
pub const STEP_DEADLINE: Duration = Duration::from_secs(25);

/// How long starting the far daemon may take (it waits for the socket).
pub const START_DEADLINE: Duration = Duration::from_secs(45);

/// What `ssh` exits with when the connection itself failed (or dropped).
pub const SSH_CONNECTION_LOST: i32 = 255;

/// The options every remote-session `ssh` runs with. `BatchMode` turns any
/// password or passphrase prompt into a failure; the keepalives turn a dead
/// link into exit [`SSH_CONNECTION_LOST`] within about a minute instead of a
/// hang. The user's `~/.ssh/config` supplies everything else.
pub const SSH_OPTIONS: &[&str] = &[
    "-o",
    "BatchMode=yes",
    "-o",
    "ConnectTimeout=10",
    "-o",
    "ServerAliveInterval=15",
    "-o",
    "ServerAliveCountMax=4",
    "-o",
    "LogLevel=ERROR",
];

/// One project in a [`RemoteListing`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteProject {
    /// The project's id.
    pub name: String,
    /// Its display name, when it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// Its folder, with the host's home shown as `~`.
    pub path: String,
}

/// What `shelbi remote list --json` prints: this machine's Shelbi, as another
/// machine's Connect to Remote Session dialog sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteListing {
    /// The listing's shape ([`REMOTE_PROTOCOL`]).
    pub protocol: u32,
    /// The `shelbi` version.
    pub version: String,
    /// The user Shelbi runs as.
    pub user: String,
    /// Whether the hub daemon is running (read without starting it).
    pub daemon_running: bool,
    /// The registered projects, most recently opened first.
    pub projects: Vec<RemoteProject>,
}

impl RemoteListing {
    /// This machine's listing, read from the Shelbi home. Starts nothing.
    pub fn local() -> shelbi_core::Result<RemoteListing> {
        let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
        let projects = shelbi_state::list_projects()?
            .into_iter()
            .map(|p| RemoteProject {
                path: super::display_path(&p.repo_path, home.as_deref()),
                label: p.display_name,
                name: p.name,
            })
            .collect();
        Ok(RemoteListing {
            protocol: REMOTE_PROTOCOL,
            version: env!("CARGO_PKG_VERSION").to_string(),
            user: local_user(),
            daemon_running: super::route::daemon_state() == super::DaemonState::Running,
            projects,
        })
    }

    /// The listing's projects as sessions on `host`.
    pub fn sessions(&self, host: &str) -> Vec<RemoteSession> {
        self.projects
            .iter()
            .map(|p| RemoteSession {
                host: host.to_string(),
                user: self.user.clone(),
                project: p.name.clone(),
                label: p.label.clone(),
                path: p.path.clone(),
            })
            .collect()
    }
}

/// The user this process runs as: `$USER`, else `$LOGNAME`, else `id -un`.
fn local_user() -> String {
    ["USER", "LOGNAME"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .map(|u| u.trim().to_string())
        .find(|u| !u.is_empty())
        .or_else(|| {
            let out = Command::new("id").arg("-un").output().ok()?;
            let user = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!user.is_empty()).then_some(user)
        })
        .unwrap_or_default()
}

/// What one `ssh` step returned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SshOutput {
    /// The exit code; `None` when `ssh` died on a signal.
    pub code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
}

/// Running a script on a host. The seam [`SshRemote`] drives, so tests can
/// answer for a host without one.
pub trait SshTransport: Send + Sync {
    /// Run the `sh` script `script` on `host`, giving up after `deadline`.
    /// `Err` only when `ssh` couldn't be run or timed out.
    fn run(&self, host: &str, script: &str, deadline: Duration) -> std::io::Result<SshOutput>;
}

/// The host's `ssh` with [`SSH_OPTIONS`].
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenSsh;

impl SshTransport for OpenSsh {
    fn run(&self, host: &str, script: &str, deadline: Duration) -> std::io::Result<SshOutput> {
        let mut child = Command::new("ssh")
            .args(SSH_OPTIONS)
            .arg(host)
            .arg("--")
            .arg(remote_sh(script))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let started = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait()? {
                break status;
            }
            if started.elapsed() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("no answer after {}s", deadline.as_secs()),
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        Ok(SshOutput {
            code: status.code(),
            stdout: stdout.join().unwrap_or_default(),
            stderr: stderr.join().unwrap_or_default(),
        })
    }
}

/// Read a child's pipe to the end on its own thread, so a chatty one never
/// blocks on a full pipe.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut text = String::new();
        if let Some(mut pipe) = pipe {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            text = String::from_utf8_lossy(&bytes).into_owned();
        }
        text
    })
}

/// `script` as the one remote command word `ssh` hands the far login shell:
/// `sh -c '<script>'`, so the far side runs it under `sh` whatever the
/// user's shell is.
fn remote_sh(script: &str) -> String {
    format!("sh -c {}", shelbi_core::shell_escape(script))
}

// The script's answers are framed in markers, so login-shell noise around
// them is ignored.
const M_MISSING: &str = "@@shelbi-missing@@";
const M_BIN: (&str, &str) = ("@@shelbi-bin@@", "@@end-bin@@");
const M_VERSION: (&str, &str) = ("@@shelbi-version@@", "@@end-version@@");
const M_LISTING: (&str, &str) = ("@@shelbi-listing@@", "@@end-listing@@");
const M_STATUS: (&str, &str) = ("@@shelbi-status@@", "@@end-status@@");

/// The `sh` that sets `P` to the host's `shelbi`: what its interactive login
/// shell finds on PATH (a plain `ssh` command doesn't load the user's PATH),
/// else the managed `~/.shelbi/bin/shelbi`, else empty.
const FIND_SHELBI: &str = r#"SH=${SHELL:-/bin/sh}
P=$("$SH" -l -i -c 'command -v shelbi' 2>/dev/null </dev/null | tail -n 1)
case "$P" in /*) ;; *) P= ;; esac
if [ -z "$P" ] && [ -x "$HOME/.shelbi/bin/shelbi" ]; then P="$HOME/.shelbi/bin/shelbi"; fi
"#;

/// The script [`SshRemote::probe`] runs: find `shelbi`, then print its
/// version and its `remote list --json`, each framed in markers.
pub fn probe_script() -> String {
    format!(
        "{FIND_SHELBI}if [ -z \"$P\" ]; then printf '\\n{M_MISSING}\\n'; exit 0; fi
printf '\\n{}%s{}\\n' \"$P\"
printf '{}%s{}\\n' \"$(\"$P\" --version 2>/dev/null)\"
printf '{}\\n'
\"$P\" remote list --json 2>&1
S=$?
printf '\\n{}\\n{}%s{}\\n' \"$S\"
",
        M_BIN.0, M_BIN.1, M_VERSION.0, M_VERSION.1, M_LISTING.0, M_LISTING.1, M_STATUS.0, M_STATUS.1,
    )
}

/// The script [`RemoteConnector::start_daemon`] runs: find `shelbi` and start
/// its daemon the way its own clients do (`shelbi __ensure-daemon`, which
/// returns once the daemon answers).
pub fn start_script() -> String {
    format!(
        "{FIND_SHELBI}if [ -z \"$P\" ]; then printf '\\n{M_MISSING}\\n'; exit 0; fi
exec \"$P\" __ensure-daemon
"
    )
}

/// The command a client runs, in a terminal, to open `session`: the host's
/// own TUI for the project, over `ssh -t`. The far TUI attaches to the
/// project's running sessions, so running this again after a drop picks up
/// where the user left off.
pub fn terminal_command(session: &RemoteSession) -> Vec<String> {
    let script = format!(
        "{FIND_SHELBI}if [ -z \"$P\" ]; then echo 'Shelbi isn'\\''t installed on this machine.' >&2; exit 127; fi
exec \"$P\" --project {}
",
        shelbi_core::shell_escape(&session.project)
    );
    let mut argv = vec!["ssh".to_string(), "-t".to_string()];
    argv.extend(SSH_OPTIONS.iter().map(|o| o.to_string()));
    argv.push(session.host.clone());
    argv.push("--".to_string());
    argv.push(remote_sh(&script));
    argv
}

/// What a remote session's terminal ending means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalExit {
    /// The user quit the far TUI: close the session.
    Closed,
    /// The connection dropped (or never came up): reconnect, on the shared
    /// [`crate::session::ReconnectController`] backoff.
    ConnectionLost,
    /// The far side ran and failed: show its last line and stop.
    Failed,
}

/// How a client reads the exit code of [`terminal_command`].
pub fn exit_meaning(code: Option<i32>) -> TerminalExit {
    match code {
        Some(0) => TerminalExit::Closed,
        // A signal (ssh killed under us) is a lost link, like 255.
        Some(SSH_CONNECTION_LOST) | None => TerminalExit::ConnectionLost,
        Some(_) => TerminalExit::Failed,
    }
}

/// What a host said about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteHost {
    /// The host as given.
    pub host: String,
    /// The far `shelbi`'s path.
    pub binary: String,
    pub listing: RemoteListing,
}

impl RemoteHost {
    pub fn sessions(&self) -> Vec<RemoteSession> {
        self.listing.sessions(&self.host)
    }
}

/// The real [`RemoteConnector`]. See the module docs.
pub struct SshRemote {
    transport: Box<dyn SshTransport>,
    /// This machine's Shelbi version, which the host's must share
    /// `major.minor` with.
    local_version: String,
}

impl Default for SshRemote {
    fn default() -> Self {
        SshRemote::new()
    }
}

impl SshRemote {
    /// Over the host's `ssh`, as this build's version.
    pub fn new() -> SshRemote {
        SshRemote::with_transport(Box::new(OpenSsh), env!("CARGO_PKG_VERSION"))
    }

    pub fn with_transport(transport: Box<dyn SshTransport>, local_version: &str) -> SshRemote {
        SshRemote {
            transport,
            local_version: local_version.to_string(),
        }
    }

    /// Sign in to `host` and read its Shelbi. Every failure is one of the
    /// dialog's errors.
    pub fn probe(&self, host: &str) -> Result<RemoteHost, RemoteError> {
        let host = validate_host(host)?;
        let out = self
            .transport
            .run(&host, &probe_script(), STEP_DEADLINE)
            .map_err(|e| unreachable(&host, &e.to_string()))?;
        parse_probe(&host, &out, &self.local_version)
    }
}

impl RemoteConnector for SshRemote {
    fn find_sessions(&self, host: &str) -> Result<(String, Vec<RemoteSession>), RemoteError> {
        let found = self.probe(host)?;
        Ok((found.listing.user.clone(), found.sessions()))
    }

    fn connect(&self, session: &RemoteSession) -> Result<(), RemoteError> {
        let found = self.probe(&session.host)?;
        if !found.listing.projects.iter().any(|p| p.name == session.project) {
            return Err(RemoteError::Failed(format!(
                "{} has no Shelbi project named {}.",
                found.host, session.project
            )));
        }
        if !found.listing.daemon_running {
            return Err(RemoteError::DaemonStopped { host: found.host });
        }
        Ok(())
    }

    fn start_daemon(&self, host: &str) -> Result<(), RemoteError> {
        let host = validate_host(host)?;
        let out = self
            .transport
            .run(&host, &start_script(), START_DEADLINE)
            .map_err(|e| unreachable(&host, &e.to_string()))?;
        if let Some(error) = classify_ssh_failure(&host, &out) {
            return Err(error);
        }
        if out.stdout.contains(M_MISSING) {
            return Err(RemoteError::NoShelbi { host });
        }
        if out.code != Some(0) {
            return Err(RemoteError::Failed(format!(
                "Shelbi couldn't start on {host}: {}",
                last_line(&out.stderr).unwrap_or("it exited without saying why")
            )));
        }
        Ok(())
    }
}

/// Read the probe script's answer from `host`.
pub fn parse_probe(host: &str, out: &SshOutput, local_version: &str) -> Result<RemoteHost, RemoteError> {
    if let Some(error) = classify_ssh_failure(host, out) {
        return Err(error);
    }
    let no_shelbi = || RemoteError::NoShelbi {
        host: host.to_string(),
    };
    if out.stdout.contains(M_MISSING) {
        return Err(no_shelbi());
    }
    let Some(binary) = between(&out.stdout, M_BIN).map(str::trim).filter(|b| !b.is_empty()) else {
        // The script never got as far as finding `shelbi` (a remote shell
        // that isn't `sh`-compatible, a failed exec): say what it said.
        return Err(RemoteError::Failed(format!(
            "{host} didn't answer as expected: {}",
            last_line(&out.stderr).unwrap_or("no output")
        )));
    };
    let remote_version = between(&out.stdout, M_VERSION)
        .and_then(version_of)
        .unwrap_or_else(|| "an unknown version".to_string());
    let mismatch = || RemoteError::VersionMismatch {
        host: host.to_string(),
        local: local_version.to_string(),
        remote: remote_version.clone(),
    };
    let listing = between(&out.stdout, M_LISTING).unwrap_or("");
    let status = between(&out.stdout, M_STATUS).map(str::trim);
    if status != Some("0") {
        // A Shelbi from before `remote list` rejects the subcommand: too old
        // to connect to.
        if listing.contains("unrecognized subcommand") || listing.contains("unexpected argument") {
            return Err(mismatch());
        }
        return Err(RemoteError::Failed(format!(
            "Shelbi on {host} couldn't list its projects: {}",
            last_line(listing).unwrap_or("it exited without saying why")
        )));
    }
    let Some(listing) = json_object(listing).and_then(|j| serde_json::from_str::<RemoteListing>(j).ok())
    else {
        return Err(mismatch());
    };
    if listing.protocol != REMOTE_PROTOCOL || !same_release_line(local_version, &listing.version) {
        return Err(RemoteError::VersionMismatch {
            host: host.to_string(),
            local: local_version.to_string(),
            remote: listing.version,
        });
    }
    Ok(RemoteHost {
        host: host.to_string(),
        binary: binary.to_string(),
        listing,
    })
}

/// `ssh`'s own failures: the connection, the sign-in, or the host key. `None`
/// when the script ran.
pub fn classify_ssh_failure(host: &str, out: &SshOutput) -> Option<RemoteError> {
    if out.code == Some(0) {
        return None;
    }
    let text = out.stderr.to_ascii_lowercase();
    let reason = || last_line(&out.stderr).unwrap_or("ssh exited without saying why").to_string();
    let host_s = host.to_string();
    if HOST_KEY_MARKERS.iter().any(|m| text.contains(m)) {
        return Some(RemoteError::HostKey { host: host_s });
    }
    if AUTH_MARKERS.iter().any(|m| text.contains(m)) {
        return Some(RemoteError::AuthFailed {
            host: host_s,
            reason: reason(),
        });
    }
    if out.code == Some(SSH_CONNECTION_LOST) || UNREACHABLE_MARKERS.iter().any(|m| text.contains(m)) {
        return Some(unreachable(host, &reason()));
    }
    None
}

fn unreachable(host: &str, reason: &str) -> RemoteError {
    RemoteError::Unreachable {
        host: host.to_string(),
        reason: reason.trim_start_matches("ssh: ").trim_end_matches('.').to_string(),
    }
}

const HOST_KEY_MARKERS: &[&str] = &[
    "host key verification failed",
    "remote host identification has changed",
    "no matching host key",
];

const AUTH_MARKERS: &[&str] = &[
    "permission denied",
    "authentication failed",
    "too many authentication failures",
    "no supported authentication methods",
];

const UNREACHABLE_MARKERS: &[&str] = &[
    "could not resolve",
    "connection refused",
    "connection timed out",
    "operation timed out",
    "no route to host",
    "network is unreachable",
    "host is down",
    "connection closed",
];

/// The text between `markers`, or `None` when either is missing.
fn between<'a>(text: &'a str, markers: (&str, &str)) -> Option<&'a str> {
    let start = text.find(markers.0)? + markers.0.len();
    let rest = &text[start..];
    Some(&rest[..rest.find(markers.1)?])
}

/// The first `{` to the last `}`: the JSON in a listing a login shell may
/// have wrapped in noise.
fn json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    (end > start).then(|| &text[start..=end])
}

/// The version in `shelbi --version`'s line (`shelbi 0.42.0`).
fn version_of(line: &str) -> Option<String> {
    line.split_whitespace()
        .last()
        .map(|v| v.trim_start_matches('v').to_string())
        .filter(|v| v.chars().next().is_some_and(|c| c.is_ascii_digit()))
}

/// Whether two versions share `major.minor`, the rule Shelbi already holds
/// remote machines to: patch releases talk to each other.
pub fn same_release_line(a: &str, b: &str) -> bool {
    fn line(v: &str) -> Option<(u64, u64)> {
        let mut parts = v.trim().trim_start_matches('v').split(['.', '-', '+']);
        Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
    }
    matches!((line(a), line(b)), (Some(x), Some(y)) if x == y)
}

fn last_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).rfind(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A scripted transport: each host answers a fixed [`SshOutput`] (or an
    /// I/O error), and every run is recorded.
    #[derive(Default)]
    struct FakeSsh {
        answers: Vec<(String, Result<SshOutput, String>)>,
        runs: Mutex<Vec<(String, String)>>,
    }

    impl FakeSsh {
        fn answer(mut self, host: &str, out: SshOutput) -> FakeSsh {
            self.answers.push((host.into(), Ok(out)));
            self
        }

        fn timing_out(mut self, host: &str) -> FakeSsh {
            self.answers.push((host.into(), Err("no answer after 25s".into())));
            self
        }
    }

    impl SshTransport for std::sync::Arc<FakeSsh> {
        fn run(&self, host: &str, script: &str, _: Duration) -> std::io::Result<SshOutput> {
            self.runs.lock().unwrap().push((host.into(), script.into()));
            match self.answers.iter().find(|(h, _)| h == host) {
                Some((_, Ok(out))) => Ok(out.clone()),
                Some((_, Err(e))) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, e.clone())),
                None => Ok(failed(255, "ssh: Could not resolve hostname nowhere: nodename nor servname provided, or not known")),
            }
        }
    }

    fn failed(code: i32, stderr: &str) -> SshOutput {
        SshOutput {
            code: Some(code),
            stdout: String::new(),
            stderr: stderr.into(),
        }
    }

    fn listing(version: &str, daemon_running: bool) -> RemoteListing {
        RemoteListing {
            protocol: REMOTE_PROTOCOL,
            version: version.into(),
            user: "jlong".into(),
            daemon_running,
            projects: vec![
                RemoteProject {
                    name: "shelbi".into(),
                    label: Some("Shelbi".into()),
                    path: "~/Workspaces/shelbi".into(),
                },
                RemoteProject {
                    name: "web".into(),
                    label: None,
                    path: "/srv/web".into(),
                },
            ],
        }
    }

    /// What the probe script prints on a host whose login shell is chatty.
    fn answered(version_line: &str, listing_text: &str, status: i32) -> SshOutput {
        SshOutput {
            code: Some(0),
            stdout: format!(
                "Welcome to devbox!\n\n{}/home/jlong/.local/bin/shelbi{}\n{}{version_line}{}\n{}\n{listing_text}\n{}\n{}{status}{}\n",
                M_BIN.0, M_BIN.1, M_VERSION.0, M_VERSION.1, M_LISTING.0, M_LISTING.1, M_STATUS.0, M_STATUS.1
            ),
            stderr: "zsh: no job control in this shell\n".into(),
        }
    }

    fn ok_host(version: &str, daemon_running: bool) -> SshOutput {
        answered(
            &format!("shelbi {version}"),
            &serde_json::to_string_pretty(&listing(version, daemon_running)).unwrap(),
            0,
        )
    }

    fn remote(fake: FakeSsh) -> (SshRemote, std::sync::Arc<FakeSsh>) {
        let fake = std::sync::Arc::new(fake);
        (SshRemote::with_transport(Box::new(fake.clone()), "0.42.0"), fake)
    }

    #[test]
    fn finding_sessions_lists_the_hosts_projects_and_user() {
        let (remote, fake) = remote(FakeSsh::default().answer("devbox", ok_host("0.42.3", true)));
        let (user, sessions) = remote.find_sessions(" devbox ").unwrap();
        assert_eq!(user, "jlong");
        assert_eq!(
            sessions,
            vec![
                RemoteSession {
                    host: "devbox".into(),
                    user: "jlong".into(),
                    project: "shelbi".into(),
                    label: Some("Shelbi".into()),
                    path: "~/Workspaces/shelbi".into(),
                },
                RemoteSession {
                    host: "devbox".into(),
                    user: "jlong".into(),
                    project: "web".into(),
                    label: None,
                    path: "/srv/web".into(),
                },
            ]
        );
        assert_eq!(sessions[0].address(), "jlong@devbox:~/Workspaces/shelbi");
        let runs = fake.runs.lock().unwrap();
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].0, "devbox", "the host is trimmed before ssh sees it");
        assert!(runs[0].1.contains("remote list --json"));
    }

    #[test]
    fn an_invalid_host_never_reaches_ssh() {
        let (remote, fake) = remote(FakeSsh::default());
        assert!(matches!(
            remote.find_sessions("-oProxyCommand=evil"),
            Err(RemoteError::InvalidHost(_))
        ));
        assert!(fake.runs.lock().unwrap().is_empty());
    }

    #[test]
    fn each_failure_mode_has_its_own_error() {
        let (remote, _) = remote(
            FakeSsh::default()
                .answer(
                    "locked",
                    failed(255, "jlong@locked: Permission denied (publickey,password)."),
                )
                .answer("newkey", failed(255, "Host key verification failed."))
                .answer(
                    "down",
                    failed(255, "ssh: connect to host down port 22: Connection refused"),
                )
                .timing_out("wedged")
                .answer(
                    "bare",
                    SshOutput {
                        code: Some(0),
                        stdout: format!("motd\n{M_MISSING}\n"),
                        stderr: String::new(),
                    },
                )
                .answer("old", answered("shelbi 0.41.9", "error: unrecognized subcommand 'remote'", 2))
                .answer("newer", ok_host("0.43.0", true)),
        );
        let err = |host: &str| remote.find_sessions(host).unwrap_err();

        assert_eq!(
            err("locked"),
            RemoteError::AuthFailed {
                host: "locked".into(),
                reason: "jlong@locked: Permission denied (publickey,password).".into()
            }
        );
        assert_eq!(err("newkey"), RemoteError::HostKey { host: "newkey".into() });
        assert_eq!(
            err("down"),
            RemoteError::Unreachable {
                host: "down".into(),
                reason: "connect to host down port 22: Connection refused".into()
            }
        );
        assert_eq!(
            err("nowhere").to_string(),
            "Couldn't connect to nowhere: Could not resolve hostname nowhere: nodename nor servname provided, or not known"
        );
        assert_eq!(
            err("wedged"),
            RemoteError::Unreachable {
                host: "wedged".into(),
                reason: "no answer after 25s".into()
            }
        );
        assert_eq!(err("bare"), RemoteError::NoShelbi { host: "bare".into() });
        assert_eq!(
            err("old"),
            RemoteError::VersionMismatch {
                host: "old".into(),
                local: "0.42.0".into(),
                remote: "0.41.9".into()
            }
        );
        assert_eq!(
            err("newer").to_string(),
            "Shelbi on newer is 0.43.0, and this machine has 0.42.0. Update them to the same version."
        );
    }

    #[test]
    fn a_listing_in_another_shape_is_a_version_mismatch() {
        let mut other = listing("0.42.0", true);
        other.protocol = REMOTE_PROTOCOL + 1;
        let out = answered(
            "shelbi 0.42.0",
            &serde_json::to_string(&other).unwrap(),
            0,
        );
        assert!(matches!(
            parse_probe("devbox", &out, "0.42.0"),
            Err(RemoteError::VersionMismatch { .. })
        ));
        let garbled = answered("shelbi 0.42.0", "not json at all", 0);
        assert!(matches!(
            parse_probe("devbox", &garbled, "0.42.0"),
            Err(RemoteError::VersionMismatch { .. })
        ));
        let broken = answered("shelbi 0.42.0", "error: io: Permission denied", 1);
        assert_eq!(
            parse_probe("devbox", &broken, "0.42.0").unwrap_err().to_string(),
            "Shelbi on devbox couldn't list its projects: error: io: Permission denied"
        );
    }

    #[test]
    fn connecting_checks_the_project_and_the_daemon_and_starts_nothing() {
        let (remote, fake) = remote(
            FakeSsh::default()
                .answer("devbox", ok_host("0.42.0", true))
                .answer("idle", ok_host("0.42.0", false)),
        );
        let (_, sessions) = remote.find_sessions("devbox").unwrap();
        remote.connect(&sessions[0]).unwrap();

        let mut gone = sessions[0].clone();
        gone.project = "gone".into();
        assert_eq!(
            remote.connect(&gone).unwrap_err().to_string(),
            "devbox has no Shelbi project named gone."
        );

        let (_, idle) = remote.find_sessions("idle").unwrap();
        assert_eq!(
            remote.connect(&idle[0]),
            Err(RemoteError::DaemonStopped { host: "idle".into() })
        );
        assert!(
            fake.runs.lock().unwrap().iter().all(|(_, s)| !s.contains("__ensure-daemon")),
            "nothing but start_daemon starts the far daemon"
        );
    }

    #[test]
    fn starting_the_daemon_runs_the_hosts_own_start() {
        let (remote, fake) = remote(
            FakeSsh::default()
                .answer("idle", SshOutput { code: Some(0), ..Default::default() })
                .answer("broken", failed(1, "shelbi daemon exited during startup"))
                .answer("locked", failed(255, "Permission denied (publickey).")),
        );
        remote.start_daemon("idle").unwrap();
        assert!(fake.runs.lock().unwrap()[0].1.contains("__ensure-daemon"));
        assert_eq!(
            remote.start_daemon("broken").unwrap_err().to_string(),
            "Shelbi couldn't start on broken: shelbi daemon exited during startup"
        );
        assert!(matches!(remote.start_daemon("locked"), Err(RemoteError::AuthFailed { .. })));
    }

    #[test]
    fn the_terminal_runs_the_hosts_tui_for_the_project_without_prompting() {
        let session = RemoteSession {
            host: "jlong@devbox".into(),
            user: "jlong".into(),
            project: "shelbi".into(),
            label: None,
            path: "~/Workspaces/shelbi".into(),
        };
        let argv = terminal_command(&session);
        assert_eq!(&argv[..2], ["ssh", "-t"]);
        assert!(argv.windows(2).any(|w| w == ["-o", "BatchMode=yes"]));
        assert!(!argv.iter().any(|a| a == "-R"), "never forwards this hub's socket");
        let host_at = argv.iter().position(|a| a == "jlong@devbox").unwrap();
        assert_eq!(argv[host_at + 1], "--");
        let remote_cmd = &argv[host_at + 2];
        assert!(remote_cmd.starts_with("sh -c '"));
        assert!(remote_cmd.contains("--project shelbi"));
        assert_eq!(argv.len(), host_at + 3);
    }

    #[test]
    fn exit_codes_say_whether_to_reconnect() {
        assert_eq!(exit_meaning(Some(0)), TerminalExit::Closed);
        assert_eq!(exit_meaning(Some(255)), TerminalExit::ConnectionLost);
        assert_eq!(exit_meaning(None), TerminalExit::ConnectionLost);
        assert_eq!(exit_meaning(Some(127)), TerminalExit::Failed);
    }

    #[test]
    fn release_lines_match_on_major_minor() {
        assert!(same_release_line("0.42.0", "0.42.7"));
        assert!(same_release_line("v1.2.3", "1.2.0-rc.1"));
        assert!(!same_release_line("0.42.0", "0.43.0"));
        assert!(!same_release_line("0.42.0", "nonsense"));
        assert_eq!(version_of("shelbi 0.42.1"), Some("0.42.1".into()));
        assert_eq!(version_of(""), None);
    }

    /// The real `ssh` against this machine, for a developer with key-based
    /// `ssh localhost` set up and Shelbi on its PATH. Off unless
    /// `SHELBI_TEST_SSH_LOCALHOST=1`; it only reads (never starts a daemon).
    #[test]
    fn localhost_over_real_ssh_when_enabled() {
        if std::env::var("SHELBI_TEST_SSH_LOCALHOST").as_deref() != Ok("1") {
            return;
        }
        let remote = SshRemote::new();
        match remote.find_sessions("localhost") {
            Ok((user, _)) => assert!(!user.is_empty()),
            // A clear dialog error is a pass too: the point is that ssh ran
            // without prompting and its answer was classified.
            Err(error) => assert!(!error.to_string().is_empty(), "{error:?}"),
        }
    }
}
