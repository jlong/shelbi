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
//!
//! A window holds the host's [`HostLink`] ([`HostLink::shared`]), not a
//! [`RemoteLink`]: when the channel drops (the keepalive goes unanswered, or
//! `ssh` exits) every stream on it ends, and the next reconnect attempt
//! ([`HostLink::connect`]) checks the host first and then opens a fresh
//! channel, which every pane, read and subscription on that host then uses.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};

use shelbi_client::{ClientError, QueryStream, RelayChannel, RelayStream};
use shelbi_proto::{RelayService, RelaySession, RELAY_SERVICES_VERSION};

use super::remote::{validate_host, RemoteError};
use super::ssh::{relay_command, STEP_DEADLINE};
use crate::source::remote::RelayLink;
use crate::source::wire::Querier;

/// How much of `ssh`'s stderr a link keeps, for the reason a link failed.
const STDERR_KEEP: usize = 4096;

/// One `ssh <host> shelbi relay` and the [`RelayChannel`] over its stdio.
/// Dropping it ends the `ssh` child (which this link started).
pub struct RemoteLink {
    host: String,
    channel: RelayChannel,
    relay_version: u16,
    child: Mutex<Child>,
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
            child: Mutex::new(child),
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
    pub fn is_reachable(&self) -> bool {
        self.channel.is_reachable()
            && matches!(self.child.lock().unwrap_or_else(|p| p.into_inner()).try_wait(), Ok(None))
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
        end(self.child.get_mut().unwrap_or_else(|p| p.into_inner()));
    }
}

/// Opens a fresh channel to a host: [`RemoteLink::open`] for a real one.
pub type LinkOpener = dyn Fn(&str) -> Result<Arc<dyn RelayLink>, RemoteError> + Send + Sync;

/// After a reopen fails, how long [`HostLink::connect`] answers with that
/// failure before it tries again, so the panes of a window reconnecting at
/// once share one `ssh` rather than each starting their own.
pub const REOPEN_PAUSE: Duration = Duration::from_secs(1);

/// A host's relay channel as every window and pane on that host shares it:
/// the current channel while it is up, and a fresh one on the next
/// [`connect`](Self::connect) after it drops.
///
/// Reads, subscriptions and session attaches use the current channel through
/// [`RelayLink`], and fail with [`ClientError::RelayUnreachable`] while it is
/// down; only [`connect`](Self::connect) opens a new one, and it runs the
/// caller's check first (the session connector's is
/// [`super::RemoteConnector::connect`]: the host answers, it still has the
/// project, its daemon runs). So a reconnect never starts the far daemon: a
/// stopped one fails the check, and no channel is opened.
pub struct HostLink {
    host: String,
    open: Box<LinkOpener>,
    pause: Duration,
    current: Mutex<Option<Arc<dyn RelayLink>>>,
    /// Held while a channel is being opened; the last failure and when.
    opening: Mutex<Option<(Instant, RemoteError)>>,
    opens: AtomicU64,
}

impl HostLink {
    /// A link to `host` whose channels `open` opens. Nothing opens until the
    /// first [`connect`](Self::connect).
    pub fn new(
        host: impl Into<String>,
        open: impl Fn(&str) -> Result<Arc<dyn RelayLink>, RemoteError> + Send + Sync + 'static,
    ) -> HostLink {
        HostLink {
            host: host.into(),
            open: Box::new(open),
            pause: REOPEN_PAUSE,
            current: Mutex::new(None),
            opening: Mutex::new(None),
            opens: AtomicU64::new(0),
        }
    }

    /// A link to `host` over `ssh` ([`RemoteLink::open`]).
    pub fn over_ssh(host: &str) -> HostLink {
        HostLink::new(host, |host| RemoteLink::open(host).map(|link| Arc::new(link) as Arc<dyn RelayLink>))
    }

    /// The one link to `host` this process shares between its windows (over
    /// `ssh`), while any of them holds it.
    pub fn shared(host: &str) -> Arc<HostLink> {
        static LINKS: OnceLock<Mutex<HashMap<String, Weak<HostLink>>>> = OnceLock::new();
        let mut links = LINKS
            .get_or_init(Default::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        links.retain(|_, link| link.strong_count() > 0);
        if let Some(link) = links.get(host).and_then(Weak::upgrade) {
            return link;
        }
        let link = Arc::new(HostLink::over_ssh(host));
        links.insert(host.to_string(), Arc::downgrade(&link));
        link
    }

    /// How long a failed reopen is answered from memory ([`REOPEN_PAUSE`]).
    pub fn with_pause(mut self, pause: Duration) -> HostLink {
        self.pause = pause;
        self
    }

    /// Whether the current channel is up.
    pub fn is_up(&self) -> bool {
        self.live().is_some()
    }

    /// How many channels this link has opened.
    pub fn opens(&self) -> u64 {
        self.opens.load(Ordering::SeqCst)
    }

    /// The current channel while it is up. A channel found down is let go
    /// (ending its `ssh`).
    pub fn live(&self) -> Option<Arc<dyn RelayLink>> {
        let mut current = self.current.lock().unwrap_or_else(|p| p.into_inner());
        if current.as_ref().is_some_and(|link| !link.is_reachable()) {
            *current = None;
        }
        current.clone()
    }

    /// The channel, opening a fresh one when it is down: `check` first (an
    /// error from it is returned and nothing opens), then the opener. Callers
    /// arriving while another opens wait for it and share its channel; for
    /// [`REOPEN_PAUSE`] after a failure, they get that failure back.
    pub fn connect(&self, check: &dyn Fn() -> Result<(), RemoteError>) -> Result<Arc<dyn RelayLink>, RemoteError> {
        if let Some(link) = self.live() {
            return Ok(link);
        }
        let mut opening = self.opening.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(link) = self.live() {
            return Ok(link);
        }
        if let Some((at, error)) = opening.as_ref() {
            if at.elapsed() < self.pause {
                return Err(error.clone());
            }
        }
        match check().and_then(|()| (self.open)(&self.host)) {
            Ok(link) => {
                *opening = None;
                self.opens.fetch_add(1, Ordering::SeqCst);
                *self.current.lock().unwrap_or_else(|p| p.into_inner()) = Some(link.clone());
                Ok(link)
            }
            Err(error) => {
                *opening = Some((Instant::now(), error.clone()));
                Err(error)
            }
        }
    }

    fn channel(&self) -> Result<Arc<dyn RelayLink>, ClientError> {
        self.live().ok_or(ClientError::RelayUnreachable)
    }
}

impl RelayLink for HostLink {
    fn host(&self) -> &str {
        &self.host
    }
    fn is_reachable(&self) -> bool {
        self.is_up()
    }
    fn open_query(&self) -> Result<QueryStream, ClientError> {
        self.channel()?.open_query()
    }
    fn open_service(&self, service: RelayService) -> Result<RelayStream, ClientError> {
        self.channel()?.open_service(service)
    }
    fn list_sessions(&self) -> Result<Vec<RelaySession>, ClientError> {
        self.channel()?.list_sessions()
    }
    fn open_session(&self, short_id: &str) -> Result<RelayStream, ClientError> {
        self.channel()?.open_session(short_id)
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
    use std::sync::atomic::AtomicBool;

    /// A channel that is up until [`Fake::cut`] and serves nothing.
    struct Fake {
        up: AtomicBool,
    }

    impl RelayLink for Fake {
        fn host(&self) -> &str {
            "devbox"
        }
        fn is_reachable(&self) -> bool {
            self.up.load(Ordering::SeqCst)
        }
        fn open_query(&self) -> Result<QueryStream, ClientError> {
            Err(ClientError::Relay("fake".into()))
        }
        fn open_service(&self, _: RelayService) -> Result<RelayStream, ClientError> {
            Err(ClientError::Relay("fake".into()))
        }
        fn list_sessions(&self) -> Result<Vec<RelaySession>, ClientError> {
            Ok(Vec::new())
        }
        fn open_session(&self, _: &str) -> Result<RelayStream, ClientError> {
            Err(ClientError::Relay("fake".into()))
        }
    }

    /// A host link over [`Fake`] channels, and the channels it opened.
    fn fake_link() -> (HostLink, Arc<Mutex<Vec<Arc<Fake>>>>) {
        let opened: Arc<Mutex<Vec<Arc<Fake>>>> = Arc::default();
        let keep = opened.clone();
        let link = HostLink::new("devbox", move |_| {
            let fake = Arc::new(Fake { up: AtomicBool::new(true) });
            keep.lock().unwrap().push(fake.clone());
            Ok(fake as Arc<dyn RelayLink>)
        })
        .with_pause(Duration::ZERO);
        (link, opened)
    }

    #[test]
    fn a_host_link_opens_only_after_the_check_and_reuses_a_live_channel() {
        let (link, opened) = fake_link();
        assert!(!link.is_up());
        assert!(matches!(link.list_sessions(), Err(ClientError::RelayUnreachable)), "nothing opens on a read");

        let stopped = || Err(RemoteError::DaemonStopped { host: "devbox".into() });
        assert_eq!(link.connect(&stopped).err(), Some(RemoteError::DaemonStopped { host: "devbox".into() }));
        assert_eq!(link.opens(), 0, "a refused check opens no channel");

        let checks = AtomicU64::new(0);
        let check = || {
            checks.fetch_add(1, Ordering::SeqCst);
            Ok(())
        };
        link.connect(&check).unwrap();
        link.connect(&check).unwrap();
        assert_eq!((link.opens(), checks.load(Ordering::SeqCst)), (1, 1), "a live channel is shared, unchecked");
        assert!(link.list_sessions().unwrap().is_empty());

        // The channel drops: reads fail until a reconnect checks and reopens.
        opened.lock().unwrap()[0].up.store(false, Ordering::SeqCst);
        assert!(!link.is_up());
        assert!(matches!(link.list_sessions(), Err(ClientError::RelayUnreachable)));
        link.connect(&check).unwrap();
        assert_eq!((link.opens(), checks.load(Ordering::SeqCst)), (2, 2));
        assert!(link.is_up());
    }

    #[test]
    fn a_failed_reopen_is_answered_from_memory_for_the_pause() {
        let tries = Arc::new(AtomicU64::new(0));
        let count = tries.clone();
        let link = HostLink::new("devbox", move |host| {
            count.fetch_add(1, Ordering::SeqCst);
            Err(RemoteError::Unreachable {
                host: host.into(),
                reason: "Network is unreachable".into(),
            })
        })
        .with_pause(Duration::from_secs(60));
        let ok = || Ok(());
        for _ in 0..3 {
            assert!(matches!(link.connect(&ok), Err(RemoteError::Unreachable { .. })));
        }
        assert_eq!(tries.load(Ordering::SeqCst), 1, "panes reconnecting at once share one attempt");
    }

    #[test]
    fn one_host_link_per_host_while_held() {
        let a = HostLink::shared("rl-shared-a");
        let again = HostLink::shared("rl-shared-a");
        let b = HostLink::shared("rl-shared-b");
        assert!(Arc::ptr_eq(&a, &again));
        assert!(!Arc::ptr_eq(&a, &b));
        assert_eq!(a.opens(), 0, "sharing a link opens nothing");
    }

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
