//! [`RemoteLink`]: one relay channel to a remote host, for a native remote
//! window.
//!
//! A remote window reaches its far project through the host's own `shelbi
//! relay` over a single `ssh` ([`super::ssh::relay_command`]): reads on the
//! `query` service, commands on `control`, change notifications on `changes`,
//! and session streams, all multiplexed on that one stdio channel. One link
//! per host is shared by every window and pane on it, which keeps under
//! sshd's default limit of ten sessions per connection.
//!
//! The link exposes nothing on either machine: no forwarded ports or socket
//! files, and this machine's `hub.sock` never reaches the far host. It never
//! starts the far daemon; a service that needs it fails to open while it is
//! stopped.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelbi_client::{ClientError, RelayChannel, RelayStream};
use shelbi_proto::{RelayService, RELAY_SERVICES_VERSION};

use super::remote::{validate_host, RemoteError};
use super::ssh::{relay_command, STEP_DEADLINE};
use crate::source::wire::Querier;

/// How much of `ssh`'s stderr a link keeps, for the reason a link failed.
const STDERR_KEEP: usize = 4096;

/// One `ssh <host> shelbi relay` and the [`RelayChannel`] over its stdio.
/// Dropping it ends the `ssh` child (which this link started).
pub struct RemoteLink {
    host: String,
    channel: RelayChannel,
    relay_version: u16,
    child: Child,
    stderr: Arc<Mutex<String>>,
}

impl RemoteLink {
    /// Open the link to `host` ([`relay_command`]) and wait for the far
    /// relay's hello. Fails when `ssh` can't sign in or reach the host, or
    /// the host has no Shelbi, with `ssh`'s own last line as the reason.
    pub fn open(host: &str) -> Result<RemoteLink, RemoteError> {
        let host = validate_host(host)?;
        let argv = relay_command(&host);
        let mut cmd = Command::new(&argv[0]);
        cmd.args(&argv[1..]);
        RemoteLink::spawn(&host, cmd)
    }

    /// Open a link over `cmd`, a command whose stdio speaks the relay
    /// protocol. [`RemoteLink::open`] passes the `ssh` from
    /// [`relay_command`]; tests pass a local `shelbi relay`.
    pub fn spawn(host: &str, mut cmd: Command) -> Result<RemoteLink, RemoteError> {
        let failed = |reason: String| RemoteError::Unreachable {
            host: host.to_string(),
            reason,
        };
        let mut child = cmd
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| failed(format!("couldn't run ssh: {e}")))?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = Arc::new(Mutex::new(String::new()));
        if let Some(mut pipe) = child.stderr.take() {
            let keep = stderr.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 1024];
                while let Ok(n) = pipe.read(&mut chunk) {
                    if n == 0 {
                        break;
                    }
                    let mut kept = keep.lock().unwrap();
                    kept.push_str(&String::from_utf8_lossy(&chunk[..n]));
                    if kept.len() > STDERR_KEEP {
                        let cut = kept.len() - STDERR_KEEP;
                        let cut = (cut..kept.len()).find(|i| kept.is_char_boundary(*i)).unwrap_or(0);
                        kept.drain(..cut);
                    }
                }
            });
        }
        let channel = match RelayChannel::new(Box::new(stdout), Box::new(stdin)) {
            Ok(channel) => channel,
            Err(e) => {
                end(&mut child);
                return Err(failed(e.to_string()));
            }
        };
        // The relay says hello first thing; no hello means ssh or the far
        // side failed, and ssh has said why on stderr.
        let relay_version = match channel.peer_version(STEP_DEADLINE) {
            Ok(version) => version,
            Err(_) => {
                end(&mut child);
                // Let the stderr reader catch ssh's last words.
                std::thread::sleep(Duration::from_millis(50));
                let said = stderr.lock().unwrap().clone();
                let reason = said
                    .lines()
                    .map(str::trim)
                    .rfind(|l| !l.is_empty())
                    .unwrap_or("the relay didn't start")
                    .to_string();
                return Err(failed(reason));
            }
        };
        Ok(RemoteLink {
            host: host.to_string(),
            channel,
            relay_version,
            child,
            stderr,
        })
    }

    /// The host as given.
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The far relay's protocol version, from its hello.
    pub fn relay_version(&self) -> u16 {
        self.relay_version
    }

    /// Whether the far relay serves `query`, `control` and `changes` (v2+).
    /// An older relay still bridges sessions.
    pub fn supports_services(&self) -> bool {
        self.relay_version >= RELAY_SERVICES_VERSION
    }

    /// Whether the channel is still up (keepalive answered, `ssh` running).
    pub fn is_reachable(&mut self) -> bool {
        self.channel.is_reachable() && matches!(self.child.try_wait(), Ok(None))
    }

    /// What `ssh` last printed on stderr, for a client explaining a drop.
    pub fn last_error(&self) -> Option<String> {
        let said = self.stderr.lock().unwrap();
        said.lines().map(str::trim).rfind(|l| !l.is_empty()).map(str::to_string)
    }

    /// The relay channel, for sessions (`list_sessions`, `open`).
    pub fn channel(&self) -> &RelayChannel {
        &self.channel
    }

    /// A querier on a fresh `query` stream.
    pub fn querier(&self) -> Result<Querier, ClientError> {
        Ok(Querier::new(self.channel.open_query()?))
    }

    /// A `control` stream for `project`'s window: the far daemon's control
    /// protocol, less what a remote window may not send.
    pub fn open_control(&self, project: &str) -> Result<RelayStream, ClientError> {
        self.channel.open_service(RelayService::Control {
            project: project.to_string(),
        })
    }

    /// A `changes` stream for `project`: the far hub's change notification
    /// lines, one way.
    pub fn open_changes(&self, project: &str) -> Result<RelayStream, ClientError> {
        self.channel.open_service(RelayService::Changes {
            project: project.to_string(),
        })
    }
}

impl Drop for RemoteLink {
    fn drop(&mut self) {
        end(&mut self.child);
    }
}

/// End a child this link started and reap it.
fn end(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_invalid_host_never_runs_ssh() {
        assert!(matches!(RemoteLink::open("-oProxyCommand=x"), Err(RemoteError::InvalidHost(_))));
        assert!(matches!(RemoteLink::open("  "), Err(RemoteError::InvalidHost(_))));
    }

    #[test]
    fn a_far_side_that_never_says_hello_fails_with_its_last_line() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "echo 'ssh: connect to host devbox port 22: Connection refused' >&2; exit 255"]);
        let err = RemoteLink::spawn("devbox", cmd).err().expect("no relay");
        match err {
            RemoteError::Unreachable { host, reason } => {
                assert_eq!(host, "devbox");
                assert!(reason.contains("Connection refused"), "{reason}");
            }
            other => panic!("expected unreachable, got {other:?}"),
        }
    }
}
